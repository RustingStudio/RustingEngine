//! GPU particle emitters (`ParticleSimulation::Gpu`): the fixed step only
//! counts spawns; each frame a compute shader respawns the ring slots the
//! new spawns land in, moves every live particle and writes its billboard
//! straight into a vertex buffer the particle pipelines draw. Nothing is
//! read back.
//!
//! Each emitter keeps a ring of `max_particles` slots of three rows
//! (position and age, velocity and lifetime, size); spawn `n` lands in slot
//! `n % max_particles`, overwriting the oldest particle.

use vulkano::buffer::{Buffer, BufferCreateInfo, BufferUsage, Subbuffer};
use vulkano::memory::allocator::{
    AllocationCreateInfo, MemoryTypeFilter, StandardMemoryAllocator,
};

use std::sync::Arc;

use super::scene_renderer::{ParticleVertex, SceneRenderError};

pub(super) mod shader {
    vulkano_shaders::shader! {
        ty: "compute",
        path: "src/shaders/compute/gpu_particles.comp",
    }
}

/// One GPU emitter's buffers on the device.
pub(super) struct GpuParticleState {
    /// Three rows per slot.
    pub particles: Subbuffer<[[f32; 4]]>,
    pub billboards: Subbuffer<[ParticleVertex]>,
    /// Spawns already placed in `particles`.
    pub spawned: u64,
}

impl GpuParticleState {
    /// Zeroed slots (age 0 of lifetime 0, so none is alive).
    pub fn new(
        allocator: &Arc<StandardMemoryAllocator>,
        slots: u32,
    ) -> Result<Self, SceneRenderError> {
        let error =
            |error: &dyn std::fmt::Display| SceneRenderError(error.to_string());
        let device_buffer = |usage| {
            (
                BufferCreateInfo {
                    usage,
                    ..Default::default()
                },
                AllocationCreateInfo {
                    memory_type_filter: MemoryTypeFilter::PREFER_DEVICE,
                    ..Default::default()
                },
            )
        };
        let (info, allocation) = device_buffer(
            BufferUsage::STORAGE_BUFFER | BufferUsage::TRANSFER_DST,
        );
        let particles = Buffer::new_slice::<[f32; 4]>(
            allocator.clone(),
            info,
            allocation,
            3 * u64::from(slots),
        )
        .map_err(|e| error(&e))?;
        let (info, allocation) = device_buffer(
            BufferUsage::STORAGE_BUFFER | BufferUsage::VERTEX_BUFFER,
        );
        let billboards = Buffer::new_slice::<ParticleVertex>(
            allocator.clone(),
            info,
            allocation,
            u64::from(slots),
        )
        .map_err(|e| error(&e))?;
        Ok(Self {
            particles,
            billboards,
            spawned: 0,
        })
    }

    pub fn slots(&self) -> u32 {
        self.billboards.len() as u32
    }
}

/// First spawn number and count to place this frame: the spawns after
/// `placed` up to `spawned`, at most one ring's worth. A count that went
/// backwards (a restart) starts over.
pub(super) fn new_spawns(placed: u64, spawned: u64, slots: u32) -> (u64, u32) {
    let fresh = if spawned < placed {
        spawned
    } else {
        spawned - placed
    };
    let count = fresh.min(u64::from(slots));
    (spawned - count, count as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_spawns_cap_at_one_ring_and_restart_from_zero() {
        assert_eq!(new_spawns(0, 5, 100), (0, 5));
        assert_eq!(new_spawns(5, 7, 100), (5, 2));
        // Only the newest ring's worth survives a long gap.
        assert_eq!(new_spawns(5, 1_000, 100), (900, 100));
        // A restart counts from zero again.
        assert_eq!(new_spawns(50, 3, 100), (0, 3));
        assert_eq!(new_spawns(9, 9, 100), (9, 0));
    }
}
