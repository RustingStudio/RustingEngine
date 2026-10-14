//! Baked lightmaps: six square faces of texels wrapped around each
//! lightmapped object's mesh box, filled by a compute shader that traces
//! rays against every lightmapped triangle.
//!
//! One storage buffer holds everything the main pass reads. Row 0 holds the
//! slot count; slot `s` (an entity's place in `RenderWorld::lightmaps`) has
//! four header rows from `1 + 4 * s`: three rows of the affine map from
//! world space into the unit mesh box, then resolution, first texel row,
//! first triangle and triangle count. Texels follow the headers, face by
//! face (+X, -X, +Y, -Y, +Z, -Z), each face `resolution` rows of
//! `resolution` texels along the next two axes in turn.

use std::collections::HashMap;

use nalgebra::{Matrix4, Point3, Vector3};

use crate::assets::AssetServer;
use crate::runtime::RenderWorld;

/// Most lightmap slots; later lightmapped entities get none.
pub(super) const MAX_LIGHTMAPS: usize = 256;
/// Most texels across all lightmaps; a slot that would pass it gets none.
const MAX_LIGHTMAP_TEXELS: usize = 1 << 20;
/// Most triangles the bake traces against; a slot that would pass it gets
/// no lightmap and casts no bounce light.
// ponytail: every ray tests every triangle; add a BVH before raising this.
const MAX_LIGHTMAP_TRIANGLES: usize = 1 << 14;

/// The rows the bake fills and the triangles it traces against.
pub(super) struct LightmapLayout {
    /// Header rows, then zeroed texel rows.
    pub rows: Vec<[f32; 4]>,
    /// Four rows per world-space triangle: each corner with one albedo
    /// channel in w, then the emissive color.
    pub triangles: Vec<[f32; 4]>,
    /// Slot and texel count of each slot to bake.
    pub slots: Vec<(u32, u32)>,
}

/// Lays out a lightmap for each entity in `render_world.lightmaps` that
/// renders a loaded mesh, within the slot, texel and triangle limits.
pub(super) fn lightmap_layout(
    render_world: &RenderWorld,
    assets: &AssetServer,
) -> LightmapLayout {
    let renderables = render_world
        .renderables
        .iter()
        .map(|renderable| (renderable.entity, renderable))
        .collect::<HashMap<_, _>>();
    let count = render_world.lightmaps.len().min(MAX_LIGHTMAPS);
    let mut rows = vec![[0.0; 4]; 1 + 4 * count];
    rows[0][0] = count as f32;
    let mut triangles = Vec::new();
    let mut slots = Vec::new();
    for (slot, (entity, lightmap)) in
        render_world.lightmaps[..count].iter().enumerate()
    {
        let Some(renderable) = renderables.get(entity) else {
            continue;
        };
        let Some(mesh) = assets.meshes.get(renderable.mesh) else {
            continue;
        };
        let resolution = lightmap.resolution.clamp(2, 64) as usize;
        let texels = 6 * resolution * resolution;
        let first_triangle = triangles.len() / 4;
        let triangle_count = mesh.indices.len() / 3;
        if mesh.vertices.is_empty()
            || rows.len() + texels > 1 + 4 * count + MAX_LIGHTMAP_TEXELS
            || first_triangle + triangle_count > MAX_LIGHTMAP_TRIANGLES
        {
            continue;
        }
        let model = Matrix4::from(renderable.transform.matrix);
        let mut low = Vector3::repeat(f32::INFINITY);
        let mut high = Vector3::repeat(f32::NEG_INFINITY);
        for vertex in &mesh.vertices {
            let position = Vector3::from(vertex.position);
            low = low.inf(&position);
            high = high.sup(&position);
        }
        let size = (high - low).map(|side| side.max(1e-3));
        let to_world = model
            * Matrix4::new_translation(&low)
            * Matrix4::new_nonuniform_scaling(&size);
        let Some(to_box) = to_world.try_inverse() else {
            continue;
        };
        let header = 1 + 4 * slot;
        for row in 0..3 {
            rows[header + row] = [
                to_box[(row, 0)],
                to_box[(row, 1)],
                to_box[(row, 2)],
                to_box[(row, 3)],
            ];
        }
        rows[header + 3] = [
            resolution as f32,
            rows.len() as f32,
            first_triangle as f32,
            triangle_count as f32,
        ];
        rows.resize(rows.len() + texels, [0.0; 4]);
        slots.push((slot as u32, texels as u32));
        let material = assets.materials.get(renderable.material);
        let albedo = material.map_or([1.0; 4], |material| material.base_color);
        let emissive = material.map_or([0.0; 3], |material| material.emissive);
        for corners in mesh.indices.as_chunks::<3>().0 {
            for (channel, &index) in corners.iter().enumerate() {
                let position = mesh
                    .vertices
                    .get(index as usize)
                    .map_or([0.0; 3], |vertex| vertex.position);
                let world = model.transform_point(&Point3::from(position));
                triangles.push([world.x, world.y, world.z, albedo[channel]]);
            }
            triangles.push([emissive[0], emissive[1], emissive[2], 0.0]);
        }
    }
    LightmapLayout {
        rows,
        triangles,
        slots,
    }
}

pub(super) mod bake_shader {
    vulkano_shaders::shader! {
        ty: "compute",
        path: "src/shaders/compute/lightmap_bake.comp",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::assets::{MeshAsset, MeshVertex};
    use crate::runtime::{ExtractedRenderable, GlobalTransform, Lightmap};
    use bevy_ecs::entity::Entity;

    fn renderable(
        entity: Entity,
        mesh: crate::assets::Handle<MeshAsset>,
        matrix: [[f32; 4]; 4],
    ) -> ExtractedRenderable {
        ExtractedRenderable {
            entity,
            transform: GlobalTransform { matrix },
            mesh,
            material: crate::assets::Handle::from_key(0),
            cast_shadows: true,
            receive_shadows: true,
            bounds: None,
        }
    }

    #[test]
    fn lightmaps_map_the_mesh_box_onto_the_unit_cube_and_skip_the_rest() {
        let mut assets = AssetServer::default();
        let corner = |position: [f32; 3]| MeshVertex {
            position,
            ..MeshVertex::default()
        };
        let mesh = assets.meshes.insert(MeshAsset {
            vertices: vec![
                corner([-1.0, -1.0, 0.0]),
                corner([1.0, -1.0, 0.0]),
                corner([1.0, 1.0, 0.5]),
            ],
            indices: vec![0, 1, 2],
        });
        let mut world = RenderWorld::default();
        let placed = Entity::from_raw_u32(1).unwrap();
        let bare = Entity::from_raw_u32(2).unwrap();
        // Scaled by 2 and moved up 3 along y.
        world.renderables = vec![renderable(
            placed,
            mesh,
            [
                [2.0, 0.0, 0.0, 0.0],
                [0.0, 2.0, 0.0, 0.0],
                [0.0, 0.0, 2.0, 0.0],
                [0.0, 3.0, 0.0, 1.0],
            ],
        )];
        world.lightmaps = vec![
            (placed, Lightmap { resolution: 1 }),
            (bare, Lightmap::default()),
        ];
        let layout = lightmap_layout(&world, &assets);
        assert_eq!(layout.rows[0][0], 2.0);
        // Resolution clamps to 2: 24 texels after the nine header rows.
        assert_eq!(layout.slots, vec![(0, 24)]);
        assert_eq!(layout.rows[4], [2.0, 9.0, 0.0, 1.0]);
        assert_eq!(layout.rows.len(), 9 + 24);
        // The bare entity renders nothing, so its slot stays empty.
        assert_eq!(layout.rows[8], [0.0; 4]);
        let to_box = |point: [f32; 3]| {
            let row = |r: [f32; 4]| {
                r[0] * point[0] + r[1] * point[1] + r[2] * point[2] + r[3]
            };
            [
                row(layout.rows[1]),
                row(layout.rows[2]),
                row(layout.rows[3]),
            ]
        };
        assert_eq!(to_box([-2.0, 1.0, 0.0]), [0.0, 0.0, 0.0]);
        assert_eq!(to_box([2.0, 5.0, 1.0]), [1.0, 1.0, 1.0]);
        assert_eq!(layout.triangles.len(), 4);
        assert_eq!(layout.triangles[2][..3], [2.0, 5.0, 1.0]);
    }
}
