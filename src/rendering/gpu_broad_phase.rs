//! Runs `broad_phase.comp`, the broad phase shared by every GPU solver,
//! without waiting, like [`super::gpu_cloth::GpuClothRunner`]: each
//! submission signals a fence, and [`GpuBroadPhase::take_completed`]
//! returns only finished ones, tagged with the fixed tick the caller gave.
//! Packing, unpacking and the CPU reference live in
//! [`crate::runtime::broad_phase`].

use std::sync::Arc;
use std::time::Duration;

use vulkano::buffer::{Buffer, BufferCreateInfo, BufferUsage, Subbuffer};
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
use vulkano::sync::future::FenceSignalFuture;
use vulkano::sync::GpuFuture;

use crate::runtime::broad_phase::{pack, unpack, BroadPairs, BroadProxy};

mod shader {
    vulkano_shaders::shader! {
        ty: "compute",
        path: "src/shaders/compute/broad_phase.comp",
    }
}

struct Pending {
    fence: FenceSignalFuture<Box<dyn GpuFuture>>,
    buffer: Subbuffer<[u32]>,
    tick: u64,
}

pub struct GpuBroadPhase {
    device: Arc<Device>,
    queue: Arc<Queue>,
    pipeline: Arc<ComputePipeline>,
    memory: Arc<StandardMemoryAllocator>,
    commands: Arc<StandardCommandBufferAllocator>,
    descriptors: Arc<StandardDescriptorSetAllocator>,
    pending: Vec<Pending>,
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
            pending: Vec::new(),
        })
    }

    /// Submits a search for the overlapping pairs among `proxies`, rows
    /// of `capacity` partners, tagged with `tick`. Returns at once; `false`
    /// when [`pack`] refuses the proxies and nothing was submitted.
    pub fn submit(
        &mut self,
        tick: u64,
        proxies: &[BroadProxy],
        capacity: u32,
    ) -> Result<bool, String> {
        let Some(words) = pack(proxies, capacity) else {
            return Ok(false);
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
        let fence = vulkano::sync::now(self.device.clone())
            .then_execute(self.queue.clone(), command_buffer)
            .map_err(|e| error(&e))?
            .boxed()
            .then_signal_fence_and_flush()
            .map_err(|e| format!("{e:?}"))?;
        self.pending.push(Pending {
            fence,
            buffer,
            tick,
        });
        Ok(true)
    }

    /// Submissions not yet returned by [`Self::take_completed`].
    #[must_use]
    pub fn in_flight(&self) -> usize {
        self.pending.len()
    }

    /// The pairs of every finished submission with its tick, in submission
    /// order. Never waits for unfinished work.
    pub fn take_completed(&mut self) -> Vec<(u64, BroadPairs)> {
        let mut done = Vec::new();
        let mut index = 0;
        while index < self.pending.len() {
            if !self.pending[index].fence.is_signaled().unwrap_or(false) {
                index += 1;
                continue;
            }
            let pending = self.pending.remove(index);
            // The zero timeout releases the buffer's GPU lock for the read.
            if pending.fence.wait(Some(Duration::ZERO)).is_err() {
                continue;
            }
            // A failed read (lost device) drops the submission.
            if let Ok(words) = pending.buffer.read() {
                done.push((pending.tick, unpack(&words)));
            };
        }
        done
    }

    /// Blocks until every submission has finished. **Slow:** for tests and
    /// tools only, never the per-frame path.
    pub fn block_until_complete(&mut self) {
        for pending in &self.pending {
            let _ = pending.fence.wait(None);
        }
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
        let mut broad =
            GpuBroadPhase::new(base.device.clone(), base.queue.clone())
                .unwrap();
        // Blocks for one result; the frame path polls `take_completed`.
        let mut pairs = |proxies: &[BroadProxy], capacity| {
            if !broad.submit(7, proxies, capacity).unwrap() {
                return None;
            }
            broad.block_until_complete();
            let mut done = broad.take_completed();
            assert_eq!(done.len(), 1);
            let (tick, pairs) = done.pop().unwrap();
            assert_eq!(tick, 7);
            Some(pairs)
        };
        // 3000 boxes of four sizes and three layers: more proxies than
        // lanes, many cells per bucket, boxes across cell borders.
        let proxies = scattered(3000);
        let expected = pairs_on_cpu(&proxies, 256);
        assert!(expected.pairs.len() > 1000, "{}", expected.pairs.len());
        assert!(expected.overflowed.is_empty());
        let first = pairs(&proxies, 256).unwrap();
        assert_eq!(first, expected);
        // Small rows overflow and keep the lowest partners, the same way
        // on every run.
        let tight = pairs_on_cpu(&proxies, 2);
        assert!(!tight.overflowed.is_empty());
        for _ in 0..3 {
            assert_eq!(pairs(&proxies, 2).unwrap(), tight);
        }
        // All points (no extent) and an empty set work too.
        let points: Vec<_> = proxies
            .iter()
            .map(|p| BroadProxy { max: p.min, ..*p })
            .collect();
        assert_eq!(pairs(&points, 4).unwrap(), pairs_on_cpu(&points, 4));
        assert_eq!(pairs(&[], 4).unwrap(), BroadPairs::default());
        let bad = [BroadProxy {
            min: [1.0; 3],
            max: [0.0; 3],
            layer: 1,
            mask: 1,
        }];
        assert_eq!(pairs(&bad, 4), None);
        // Two searches in flight at once come back in order with their
        // ticks, each once.
        assert!(broad.submit(1, &proxies, 256).unwrap());
        assert!(broad.submit(2, &points, 4).unwrap());
        assert_eq!(broad.in_flight(), 2);
        broad.block_until_complete();
        let done = broad.take_completed();
        assert_eq!(broad.in_flight(), 0);
        assert_eq!(done, vec![(1, expected), (2, pairs_on_cpu(&points, 4))]);
        assert!(broad.take_completed().is_empty());
    }
}
