//! Runs `broad_phase.comp`, the broad phase shared by every GPU solver.
//! Packing, unpacking and the CPU reference live in
//! [`crate::runtime::broad_phase`].

use std::sync::Arc;

use vulkano::buffer::{Buffer, BufferCreateInfo, BufferUsage};
use vulkano::command_buffer::allocator::StandardCommandBufferAllocator;
use vulkano::command_buffer::{AutoCommandBufferBuilder, CommandBufferUsage};
use vulkano::descriptor_set::allocator::StandardDescriptorSetAllocator;
use vulkano::descriptor_set::{DescriptorSet, WriteDescriptorSet};
use vulkano::device::{Device, Queue};
use vulkano::memory::allocator::{
    AllocationCreateInfo, MemoryTypeFilter, StandardMemoryAllocator,
};
use vulkano::pipeline::compute::ComputePipelineCreateInfo;
use vulkano::pipeline::layout::PipelineDescriptorSetLayoutCreateInfo;
use vulkano::pipeline::{
    ComputePipeline, Pipeline, PipelineBindPoint, PipelineLayout,
    PipelineShaderStageCreateInfo,
};
use vulkano::sync::GpuFuture;

use crate::runtime::broad_phase::{pack, unpack, BroadPairs, BroadProxy};

mod shader {
    vulkano_shaders::shader! {
        ty: "compute",
        path: "src/shaders/compute/broad_phase.comp",
    }
}

pub struct GpuBroadPhase {
    device: Arc<Device>,
    queue: Arc<Queue>,
    pipeline: Arc<ComputePipeline>,
    memory: Arc<StandardMemoryAllocator>,
    commands: Arc<StandardCommandBufferAllocator>,
    descriptors: Arc<StandardDescriptorSetAllocator>,
}

impl GpuBroadPhase {
    pub fn new(device: Arc<Device>, queue: Arc<Queue>) -> Result<Self, String> {
        let error = |error: &dyn std::fmt::Display| error.to_string();
        let entry_point = shader::load(device.clone())
            .map_err(|e| error(&e))?
            .entry_point("main")
            .ok_or("broad_phase.comp has no main")?;
        let stage = PipelineShaderStageCreateInfo::new(entry_point);
        let layout = PipelineLayout::new(
            device.clone(),
            PipelineDescriptorSetLayoutCreateInfo::from_stages([&stage])
                .into_pipeline_layout_create_info(device.clone())
                .map_err(|e| format!("{e:?}"))?,
        )
        .map_err(|e| error(&e))?;
        let pipeline = ComputePipeline::new(
            device.clone(),
            None,
            ComputePipelineCreateInfo::stage_layout(stage, layout),
        )
        .map_err(|e| error(&e))?;
        Ok(Self {
            memory: Arc::new(StandardMemoryAllocator::new_default(
                device.clone(),
            )),
            commands: Arc::new(StandardCommandBufferAllocator::new(
                device.clone(),
                Default::default(),
            )),
            descriptors: Arc::new(StandardDescriptorSetAllocator::new(
                device.clone(),
                Default::default(),
            )),
            device,
            queue,
            pipeline,
        })
    }

    /// The overlapping pairs among `proxies`, rows of `capacity` partners;
    /// `None` when [`pack`] refuses them. Waits for the GPU.
    // ponytail: blocking; give it a fence and take results a frame later,
    // as `GpuClothRunner` does, when a solver calls it every tick.
    pub fn pairs(
        &self,
        proxies: &[BroadProxy],
        capacity: u32,
    ) -> Result<Option<BroadPairs>, String> {
        let Some(words) = pack(proxies, capacity) else {
            return Ok(None);
        };
        let error = |error: &dyn std::fmt::Display| error.to_string();
        let buffer = Buffer::from_iter(
            self.memory.clone(),
            BufferCreateInfo {
                usage: BufferUsage::STORAGE_BUFFER,
                ..Default::default()
            },
            AllocationCreateInfo {
                memory_type_filter: MemoryTypeFilter::PREFER_DEVICE
                    | MemoryTypeFilter::HOST_RANDOM_ACCESS,
                ..Default::default()
            },
            words,
        )
        .map_err(|e| format!("{e:?}"))?;
        let set = DescriptorSet::new(
            self.descriptors.clone(),
            self.pipeline.layout().set_layouts()[0].clone(),
            [WriteDescriptorSet::buffer(0, buffer.clone())],
            [],
        )
        .map_err(|e| format!("{e:?}"))?;
        let mut builder = AutoCommandBufferBuilder::primary(
            self.commands.clone(),
            self.queue.queue_family_index(),
            CommandBufferUsage::OneTimeSubmit,
        )
        .map_err(|e| format!("{e:?}"))?;
        builder
            .bind_pipeline_compute(self.pipeline.clone())
            .map_err(|e| format!("{e:?}"))?
            .bind_descriptor_sets(
                PipelineBindPoint::Compute,
                self.pipeline.layout().clone(),
                0,
                set,
            )
            .map_err(|e| format!("{e:?}"))?;
        // One workgroup runs every pass, with barriers between them.
        unsafe { builder.dispatch([1, 1, 1]) }.map_err(|e| format!("{e:?}"))?;
        let command_buffer = builder.build().map_err(|e| format!("{e:?}"))?;
        vulkano::sync::now(self.device.clone())
            .then_execute(self.queue.clone(), command_buffer)
            .map_err(|e| error(&e))?
            .then_signal_fence_and_flush()
            .map_err(|e| format!("{e:?}"))?
            .wait(None)
            .map_err(|e| format!("{e:?}"))?;
        let words = buffer.read().map_err(|e| format!("{e:?}"))?;
        Ok(Some(unpack(&words)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::broad_phase::pairs_on_cpu;
    use crate::runtime::broad_phase::tests::scattered;

    #[test]
    #[cfg_attr(
        not(feature = "gpu-tests"),
        ignore = "run with `--features gpu-tests` on a machine with a Vulkan driver"
    )]
    fn the_gpu_broad_phase_finds_the_reference_pairs() {
        use crate::rendering::test_support::headless_device;

        if vulkano::VulkanLibrary::new().is_err() {
            eprintln!("skipping: no Vulkan driver present");
            return;
        }
        let base = headless_device();
        let broad = GpuBroadPhase::new(base.device.clone(), base.queue.clone())
            .unwrap();
        // 3000 boxes of four sizes and three layers: more proxies than
        // lanes, many cells per bucket, boxes across cell borders.
        let proxies = scattered(3000);
        let expected = pairs_on_cpu(&proxies, 256);
        assert!(expected.pairs.len() > 1000, "{}", expected.pairs.len());
        assert!(expected.overflowed.is_empty());
        let first = broad.pairs(&proxies, 256).unwrap().unwrap();
        assert_eq!(first, expected);
        // Small rows overflow and keep the lowest partners, the same way
        // on every run.
        let tight = pairs_on_cpu(&proxies, 2);
        assert!(!tight.overflowed.is_empty());
        for _ in 0..3 {
            assert_eq!(broad.pairs(&proxies, 2).unwrap().unwrap(), tight);
        }
        // All points (no extent) and an empty set work too.
        let points: Vec<_> = proxies
            .iter()
            .map(|p| BroadProxy { max: p.min, ..*p })
            .collect();
        assert_eq!(
            broad.pairs(&points, 4).unwrap().unwrap(),
            pairs_on_cpu(&points, 4)
        );
        assert_eq!(
            broad.pairs(&[], 4).unwrap().unwrap(),
            BroadPairs::default()
        );
        let bad = [BroadProxy {
            min: [1.0; 3],
            max: [0.0; 3],
            layer: 1,
            mask: 1,
        }];
        assert_eq!(broad.pairs(&bad, 4).unwrap(), None);
    }
}
