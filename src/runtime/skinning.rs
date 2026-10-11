//! Deformed meshes. `rusting.skin` lists the joints that bend an entity's
//! mesh, and `rusting.morph` weighs its blend shapes. Before each frame is
//! extracted, the mesh is morphed, then skinned, on the CPU into a private
//! copy that the renderer draws instead; `MeshRenderer` keeps the cooked
//! mesh, so saving is unchanged.
//!
//! With the [`GpuSkinning`] resource, the copy stays undeformed and the
//! renderer deforms it on the GPU each frame from the [`SkinJob`]s that
//! extraction lists; the CPU path stays the reference.

use std::collections::HashMap;
use std::sync::Arc;

use bevy_ecs::change_detection::DetectChangesMut;
use bevy_ecs::entity::Entity;
use bevy_ecs::prelude::{Component, Resource, World};
use nalgebra::{Matrix3, Matrix4, Vector3};
use serde::{Deserialize, Serialize};

use super::{GlobalTransform, MeshRenderer, Parent};
use crate::assets::{
    AssetServer, Handle, MeshAsset, MorphTargets, SkinWeights,
};

/// Joints that bend this entity's mesh, with weights from the `.rskin`
/// file cooked next to the mesh. glTF import fills it in.
#[derive(
    Component, Clone, Debug, Default, PartialEq, Serialize, Deserialize,
)]
#[serde(default)]
pub struct Skin {
    /// Joint entities by path from this entity: child names joined by `/`,
    /// `..` for the parent (`../Armature/Hip`).
    pub joints: Vec<String>,
    /// One inverse bind matrix per joint, column-major.
    pub inverse_bind: Vec<[[f32; 4]; 4]>,
}

/// Blend shape weights for this entity's mesh, with shape offsets from the
/// `.rmorph` file cooked next to the mesh. glTF import fills it in. Child
/// mesh parts without their own `Morph` use their parent's weights.
#[derive(
    Component, Clone, Debug, Default, PartialEq, Serialize, Deserialize,
)]
#[serde(default)]
pub struct Morph {
    /// One weight per blend shape, usually 0 to 1.
    pub weights: Vec<f32>,
}

/// Runtime copy of a skinned or morphed mesh, rebuilt from the cooked mesh
/// whenever a joint or weight changes. Never saved.
#[derive(Component, Clone, Debug)]
pub struct SkinnedMesh {
    base: Handle<MeshAsset>,
    /// The deformed mesh the renderer draws.
    pub mesh: Handle<MeshAsset>,
    matrices: Vec<Matrix4<f32>>,
    morph_weights: Vec<f32>,
}

/// Cooked skin weights and blend shapes of one mesh.
type Cooked = Arc<(Option<SkinWeights>, MorphTargets)>;

/// Present: skinned and morphed meshes deform on the GPU instead of the
/// CPU. Windowed runners insert it; headless tools keep the CPU path.
#[derive(Resource, Clone, Copy, Debug, Default)]
pub struct GpuSkinning;

/// One mesh for the renderer to deform this frame: the copy it draws, the
/// cooked mesh it starts from, and the joint matrices and blend shape
/// weights, as `update_skins` would apply them on the CPU.
#[derive(Clone, Debug, PartialEq)]
pub struct SkinJob {
    pub mesh: Handle<MeshAsset>,
    pub base: Handle<MeshAsset>,
    /// Column-major; empty leaves the mesh unskinned.
    pub matrices: Vec<[[f32; 4]; 4]>,
    pub morph_weights: Vec<f32>,
}

impl Deformers {
    /// The cooked skin weights and blend shapes `update_skins` read for
    /// `base`.
    #[must_use]
    pub fn cooked(
        &self,
        base: Handle<MeshAsset>,
    ) -> Option<&(Option<SkinWeights>, MorphTargets)> {
        self.cooked.get(&base.key()).map(|cooked| &**cooked)
    }
}

/// The deformations of this frame with [`GpuSkinning`], in entity order;
/// empty without it.
pub fn skin_jobs(world: &mut World) -> Vec<SkinJob> {
    if !world.contains_resource::<GpuSkinning>() {
        return Vec::new();
    }
    let mut query = world.query::<(Entity, &SkinnedMesh)>();
    let mut jobs = query
        .iter(world)
        .map(|(entity, state)| {
            (
                entity,
                SkinJob {
                    mesh: state.mesh,
                    base: state.base,
                    matrices: state
                        .matrices
                        .iter()
                        .map(|&matrix| matrix.into())
                        .collect(),
                    morph_weights: state.morph_weights.clone(),
                },
            )
        })
        .collect::<Vec<_>>();
    jobs.sort_by_key(|(entity, _)| *entity);
    jobs.into_iter().map(|(_, job)| job).collect()
}

/// Every deformed copy in the mesh list and the entity that draws it, so
/// copies of despawned or undeformed entities are freed, plus the cooked
/// data read for each base mesh.
#[derive(Debug, Default)]
pub struct Deformers {
    copies: Vec<(Entity, Handle<MeshAsset>)>,
    pub(crate) cooked: HashMap<u64, Cooked>,
}

/// The blend shape weights `entity` draws with: its own, else its parent's.
fn morph_weights(world: &World, entity: Entity) -> Option<&Morph> {
    world.get::<Morph>(entity).or_else(|| {
        world
            .get::<Parent>(entity)
            .and_then(|parent| world.get::<Morph>(parent.0))
    })
}

/// Drops `SkinnedMesh` where the skin, weights or mesh went away, and frees
/// every copy no entity draws any more.
fn free_stale_copies(world: &mut World) {
    let copies = std::mem::take(
        &mut world.resource_mut::<AssetServer>().deformers.copies,
    );
    let mut query = world.query::<(
        Entity,
        &SkinnedMesh,
        Option<&Skin>,
        Option<&MeshRenderer>,
    )>();
    // A snapshot restore can bring back a state whose copy was freed.
    let stale = query
        .iter(world)
        .filter(|&(entity, state, skin, renderer)| {
            (skin.is_none() && morph_weights(world, entity).is_none())
                || renderer.is_none_or(|r| r.mesh != state.base)
                || !copies.contains(&(entity, state.mesh))
        })
        .map(|(entity, ..)| entity)
        .collect::<Vec<_>>();
    for entity in stale {
        world.entity_mut(entity).remove::<SkinnedMesh>();
    }
    let (keep, free): (Vec<_>, Vec<_>) =
        copies.into_iter().partition(|(entity, mesh)| {
            world
                .get::<SkinnedMesh>(*entity)
                .is_some_and(|state| state.mesh == *mesh)
        });
    let mut assets = world.resource_mut::<AssetServer>();
    for (_, mesh) in free {
        let _ = assets.meshes.remove(mesh);
    }
    assets.deformers.copies = keep;
}

/// Joint matrix per joint: the joint's motion from its bind pose, in the
/// skinned entity's space. Missing joints keep the bind pose.
#[must_use]
pub fn joint_matrices(world: &World, entity: Entity) -> Vec<Matrix4<f32>> {
    let (Some(skin), Some(global)) = (
        world.get::<Skin>(entity),
        world.get::<GlobalTransform>(entity),
    ) else {
        return Vec::new();
    };
    let to_local = Matrix4::from(global.matrix)
        .try_inverse()
        .unwrap_or_else(Matrix4::identity);
    skin.joints
        .iter()
        .enumerate()
        .map(|(index, path)| {
            let joint = super::find_target(world, entity, path)
                .and_then(|joint| world.get::<GlobalTransform>(joint));
            let inverse_bind = skin
                .inverse_bind
                .get(index)
                .map_or_else(Matrix4::identity, |matrix| {
                    Matrix4::from(*matrix)
                });
            joint.map_or_else(Matrix4::identity, |joint| {
                to_local * Matrix4::from(joint.matrix) * inverse_bind
            })
        })
        .collect()
}

/// Linear blend skinning of `base` by four weighted joints per vertex.
/// Weights are normalized; a vertex with no weight stays put.
#[must_use]
pub fn skin_mesh(
    base: &MeshAsset,
    weights: &SkinWeights,
    matrices: &[Matrix4<f32>],
) -> MeshAsset {
    let mut mesh = base.clone();
    for (index, vertex) in mesh.vertices.iter_mut().enumerate() {
        let (Some(joints), Some(amounts)) =
            (weights.joints.get(index), weights.weights.get(index))
        else {
            continue;
        };
        let total: f32 = amounts.iter().sum();
        if total <= f32::EPSILON {
            continue;
        }
        let blend = joints.iter().zip(amounts).fold(
            Matrix4::zeros(),
            |sum, (&joint, &amount)| {
                matrices
                    .get(usize::from(joint))
                    .map_or(sum, |matrix| sum + matrix * (amount / total))
            },
        );
        let linear: Matrix3<f32> = blend.fixed_view::<3, 3>(0, 0).into();
        let normals = linear
            .try_inverse()
            .map_or(linear, |inverse| inverse.transpose());
        vertex.position = blend
            .transform_point(&Vector3::from(vertex.position).into())
            .into();
        let turn = |value: Vector3<f32>, by: &Matrix3<f32>| {
            (by * value).try_normalize(f32::EPSILON).unwrap_or(value)
        };
        vertex.normal = turn(Vector3::from(vertex.normal), &normals).into();
        let [x, y, z, w] = vertex.tangent;
        let [x, y, z] = turn(Vector3::new(x, y, z), &linear).into();
        vertex.tangent = [x, y, z, w];
    }
    mesh
}

/// Adds each blend shape's offsets, scaled by its weight, to `mesh`.
/// Normals are renormalized; a mesh without shape normals keeps its own.
#[must_use]
pub fn morph_mesh(
    base: &MeshAsset,
    targets: &MorphTargets,
    weights: &[f32],
) -> MeshAsset {
    let mut mesh = base.clone();
    for (index, &weight) in weights.iter().enumerate() {
        if weight == 0.0 {
            continue;
        }
        let add = |value: &mut [f32; 3], offset: [f32; 3]| {
            for (value, offset) in value.iter_mut().zip(offset) {
                *value += offset * weight;
            }
        };
        let positions = targets.positions.get(index).map_or(&[][..], |v| v);
        let normals = targets.normals.get(index).map_or(&[][..], |v| v);
        for (vertex, &offset) in mesh.vertices.iter_mut().zip(positions) {
            add(&mut vertex.position, offset);
        }
        for (vertex, &offset) in mesh.vertices.iter_mut().zip(normals) {
            add(&mut vertex.normal, offset);
        }
    }
    if weights.iter().any(|&weight| weight != 0.0) {
        for vertex in &mut mesh.vertices {
            vertex.normal = Vector3::from(vertex.normal)
                .try_normalize(f32::EPSILON)
                .map_or(vertex.normal, Into::into);
        }
    }
    mesh
}

/// Re-deforms every `rusting.skin` or `rusting.morph` mesh whose joints or
/// weights changed since last frame. Runs at the start of render
/// extraction, after transforms propagate.
pub fn update_skins(world: &mut World) {
    if !world.contains_resource::<AssetServer>() {
        return;
    }
    free_stale_copies(world);
    let mut query = world.query::<(Entity, &MeshRenderer)>();
    let deformed = query
        .iter(world)
        .filter(|&(entity, _)| {
            world.get::<Skin>(entity).is_some()
                || morph_weights(world, entity).is_some()
        })
        .map(|(entity, renderer)| (entity, renderer.mesh))
        .collect::<Vec<_>>();
    for (entity, base) in deformed {
        let matrices = joint_matrices(world, entity);
        let weights = morph_weights(world, entity)
            .map_or_else(Vec::new, |morph| morph.weights.clone());
        let state = world.get::<SkinnedMesh>(entity);
        if state.is_some_and(|state| {
            state.matrices == matrices && state.morph_weights == weights
        }) {
            continue;
        }
        let copy = state.map(|state| state.mesh);
        let cooked = cooked(world, base);
        let (skin, morph) = &*cooked;
        // A mesh without its `.rskin` or `.rmorph` file draws as cooked.
        if copy.is_none() && skin.is_none() && morph.positions.is_empty() {
            continue;
        }
        // On the GPU the copy keeps the cooked vertices and the renderer
        // deforms it from the job, so only the first frame copies it.
        let gpu = world.contains_resource::<GpuSkinning>();
        if !(gpu && copy.is_some()) {
            let mut assets = world.resource_mut::<AssetServer>();
            let Some(cooked_mesh) = assets.meshes.get(base) else {
                continue;
            };
            let mut mesh = if gpu {
                cooked_mesh.clone()
            } else {
                morph_mesh(cooked_mesh, morph, &weights)
            };
            if let Some(skin) =
                skin.as_ref().filter(|_| !gpu && !matrices.is_empty())
            {
                mesh = skin_mesh(&mesh, skin, &matrices);
            }
            if copy.is_none() {
                let mesh = assets.meshes.insert(mesh);
                assets.deformers.copies.push((entity, mesh));
                world.entity_mut(entity).insert(SkinnedMesh {
                    base,
                    mesh,
                    matrices,
                    morph_weights: weights,
                });
                continue;
            }
            if let Some(slot) =
                copy.and_then(|copy| assets.meshes.get_mut(copy))
            {
                *slot = mesh;
            }
        }
        // The drawn handle is the same, so extraction need not collect
        // renderables again.
        if let Some(mut state) = world.get_mut::<SkinnedMesh>(entity) {
            let state = state.bypass_change_detection();
            state.matrices = matrices;
            state.morph_weights = weights;
        }
    }
}

/// The cooked skin weights and blend shapes of `base`, read once.
// ponytail: never re-read, so a hot-reloaded `.rskin` or `.rmorph` needs a
// restart; key the cache by mesh revision if that matters.
fn cooked(world: &mut World, base: Handle<MeshAsset>) -> Cooked {
    let assets = world.resource::<AssetServer>();
    if let Some(cooked) = assets.deformers.cooked.get(&base.key()) {
        return cooked.clone();
    }
    let cooked = Arc::new((
        assets.skin_weights(base).ok(),
        assets.morph_targets(base).unwrap_or_default(),
    ));
    world
        .resource_mut::<AssetServer>()
        .deformers
        .cooked
        .insert(base.key(), cooked.clone());
    cooked
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::assets::MeshVertex;
    use crate::runtime::{hierarchy::set_parent, Name};

    #[test]
    fn joints_bend_vertices_and_normals_in_the_mesh_space() {
        let mut world = World::new();
        let global = |matrix: Matrix4<f32>| GlobalTransform {
            matrix: matrix.into(),
        };
        let rig = world.spawn(global(Matrix4::identity())).id();
        let mesh = world
            .spawn((
                Skin {
                    joints: vec!["../Bone".into()],
                    inverse_bind: vec![Matrix4::identity().into()],
                },
                global(Matrix4::new_translation(&Vector3::x())),
            ))
            .id();
        let turn = Matrix4::from_axis_angle(
            &Vector3::z_axis(),
            std::f32::consts::FRAC_PI_2,
        );
        let bone = world.spawn((Name("Bone".into()), global(turn))).id();
        set_parent(&mut world, mesh, rig).unwrap();
        set_parent(&mut world, bone, rig).unwrap();

        let matrices = joint_matrices(&world, mesh);
        let base = MeshAsset {
            vertices: vec![
                MeshVertex {
                    position: [1.0, 0.0, 0.0],
                    normal: [1.0, 0.0, 0.0],
                    ..MeshVertex::default()
                };
                2
            ],
            indices: Vec::new(),
        };
        // Weights are normalized; the second vertex has none and stays.
        let weights = SkinWeights {
            joints: vec![[0; 4]; 2],
            weights: vec![[2.0, 0.0, 0.0, 0.0], [0.0; 4]],
        };
        let bent = skin_mesh(&base, &weights, &matrices);
        let near = |a: [f32; 3], b: [f32; 3]| {
            a.iter().zip(b).all(|(a, b)| (a - b).abs() < 1e-5)
        };
        // As in glTF, the bind pose places vertices by the joints alone:
        // (1, 0, 0) turns to world (0, 1, 0), which is (-1, 1, 0) from the
        // mesh entity.
        assert!(near(bent.vertices[0].position, [-1.0, 1.0, 0.0]));
        assert!(near(bent.vertices[0].normal, [0.0, 1.0, 0.0]));
        assert_eq!(bent.vertices[1], base.vertices[1]);
    }

    #[test]
    fn copies_are_freed_when_the_skin_goes_or_the_entity_despawns() {
        let mut world = World::new();
        world.insert_resource(AssetServer::default());
        let mut assets = world.resource_mut::<AssetServer>();
        let base = assets.meshes.insert(MeshAsset::default());
        let material = assets.fallback_material;
        let skinned = |world: &mut World| {
            let renderer = MeshRenderer {
                mesh: base,
                material,
                cast_shadows: true,
                receive_shadows: true,
            };
            world
                .spawn((Skin::default(), renderer, GlobalTransform::default()))
                .id()
        };
        // Stands in for a cooked `.rskin` file.
        world.resource_mut::<AssetServer>().deformers.cooked.insert(
            base.key(),
            Arc::new((Some(SkinWeights::default()), MorphTargets::default())),
        );
        let meshes =
            |world: &World| world.resource::<AssetServer>().meshes.len();
        let before = meshes(&world);
        let first = skinned(&mut world);
        let second = skinned(&mut world);
        update_skins(&mut world);
        assert_eq!(meshes(&world), before + 2);

        world.entity_mut(first).remove::<Skin>();
        world.despawn(second);
        update_skins(&mut world);
        assert!(world.get::<SkinnedMesh>(first).is_none());
        assert_eq!(meshes(&world), before);
    }

    #[test]
    fn gpu_skinning_keeps_the_copy_cooked_and_lists_a_job() {
        let mut world = World::new();
        world.insert_resource(AssetServer::default());
        world.insert_resource(GpuSkinning);
        let mut assets = world.resource_mut::<AssetServer>();
        let base = assets.meshes.insert(MeshAsset {
            vertices: vec![MeshVertex::default()],
            indices: Vec::new(),
        });
        let material = assets.fallback_material;
        assets.deformers.cooked.insert(
            base.key(),
            Arc::new((
                None,
                MorphTargets {
                    positions: vec![vec![[1.0, 0.0, 0.0]]],
                    normals: Vec::new(),
                },
            )),
        );
        let entity = world
            .spawn((
                Morph { weights: vec![1.0] },
                MeshRenderer {
                    mesh: base,
                    material,
                    cast_shadows: true,
                    receive_shadows: true,
                },
                GlobalTransform::default(),
            ))
            .id();
        let copy_of = |world: &World| {
            let copy = world.get::<SkinnedMesh>(entity).unwrap().mesh;
            let assets = world.resource::<AssetServer>();
            (
                copy,
                assets.meshes.get(copy).unwrap().vertices[0].position,
                assets.meshes.revision(copy),
            )
        };
        update_skins(&mut world);
        let (copy, position, revision) = copy_of(&world);
        assert_eq!(position, [0.0; 3]);
        world.get_mut::<Morph>(entity).unwrap().weights[0] = 0.5;
        update_skins(&mut world);
        assert_eq!(copy_of(&world), (copy, position, revision));
        assert_eq!(
            skin_jobs(&mut world),
            vec![SkinJob {
                mesh: copy,
                base,
                matrices: Vec::new(),
                morph_weights: vec![0.5],
            }]
        );
        // Without the switch, the CPU deforms the copy and lists no job.
        world.remove_resource::<GpuSkinning>();
        world.get_mut::<Morph>(entity).unwrap().weights[0] = 0.25;
        update_skins(&mut world);
        assert_eq!(copy_of(&world).1, [0.25, 0.0, 0.0]);
        assert!(skin_jobs(&mut world).is_empty());
    }
}
