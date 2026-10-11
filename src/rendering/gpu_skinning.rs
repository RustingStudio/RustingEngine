//! GPU skinning (`runtime::GpuSkinning`): a compute shader writes each
//! skinned or morphed copy's vertices straight into its prepared vertex
//! buffer, so every pass that draws the copy sees the deformed mesh. The
//! cooked vertices, joints, weights and blend shapes upload once per copy.

use vulkano::buffer::{Buffer, BufferCreateInfo, BufferUsage, Subbuffer};
use vulkano::memory::allocator::{
    AllocationCreateInfo, MemoryTypeFilter, StandardMemoryAllocator,
};

use std::sync::Arc;

use super::scene_renderer::SceneRenderError;
use crate::assets::{MeshAsset, MorphTargets, SkinWeights};

pub(super) mod shader {
    vulkano_shaders::shader! {
        ty: "compute",
        include: ["src/shaders"],
        path: "src/shaders/compute/skinning.comp",
    }
}

/// The inputs of one copy that do not change from frame to frame.
pub(super) struct SkinInputs {
    /// Revision of the cooked mesh these were read from.
    pub revision: u64,
    pub vertex_count: u32,
    pub target_count: u32,
    /// 16 floats per vertex, laid out like `SceneVertex`.
    pub base: Subbuffer<[f32]>,
    pub joints: Subbuffer<[[u32; 4]]>,
    pub weights: Subbuffer<[[f32; 4]]>,
    /// Per blend shape and vertex: position offset, then normal offset.
    pub morph: Subbuffer<[f32]>,
}

impl SkinInputs {
    pub fn new(
        allocator: &Arc<StandardMemoryAllocator>,
        revision: u64,
        mesh: &MeshAsset,
        skin: Option<&SkinWeights>,
        morph: &MorphTargets,
    ) -> Result<Self, SceneRenderError> {
        fn upload<T: vulkano::buffer::BufferContents + Copy>(
            allocator: &Arc<StandardMemoryAllocator>,
            mut data: Vec<T>,
            empty: T,
        ) -> Result<Subbuffer<[T]>, SceneRenderError> {
            if data.is_empty() {
                data.push(empty);
            }
            Buffer::from_iter(
                allocator.clone(),
                BufferCreateInfo {
                    usage: BufferUsage::STORAGE_BUFFER,
                    ..Default::default()
                },
                AllocationCreateInfo {
                    memory_type_filter: MemoryTypeFilter::PREFER_DEVICE
                        | MemoryTypeFilter::HOST_SEQUENTIAL_WRITE,
                    ..Default::default()
                },
                data,
            )
            .map_err(|error| SceneRenderError(error.to_string()))
        }
        let count = mesh.vertices.len();
        let base = mesh
            .vertices
            .iter()
            .flat_map(|v| {
                [
                    v.position.as_slice(),
                    &v.normal,
                    &v.uv,
                    &v.tangent,
                    &v.color,
                ]
                .concat()
            })
            .collect();
        // A vertex without skin weights has none, so it stays put.
        let (joints, weights) = (0..count)
            .map(|index| {
                skin.and_then(|skin| {
                    Some((
                        skin.joints.get(index)?.map(u32::from),
                        *skin.weights.get(index)?,
                    ))
                })
                .unwrap_or_default()
            })
            .unzip();
        let offsets = |targets: &[Vec<[f32; 3]>], target: usize, index| {
            targets
                .get(target)
                .and_then(|offsets| offsets.get(index))
                .copied()
                .unwrap_or_default()
        };
        let morph_rows = (0..morph.positions.len())
            .flat_map(|target| {
                (0..count).flat_map(move |index| {
                    [
                        offsets(&morph.positions, target, index),
                        offsets(&morph.normals, target, index),
                    ]
                    .concat()
                })
            })
            .collect();
        Ok(Self {
            revision,
            vertex_count: count as u32,
            target_count: morph.positions.len() as u32,
            base: upload(allocator, base, 0.0)?,
            joints: upload(allocator, joints, [0; 4])?,
            weights: upload(allocator, weights, [0.0; 4])?,
            morph: upload(allocator, morph_rows, 0.0)?,
        })
    }
}
