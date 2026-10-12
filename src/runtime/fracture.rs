//! Pre-fractured meshes: a [`Fracture`] on an object says how its mesh
//! breaks, by Voronoi cells around seeded random points, by slicing planes,
//! or both, and [`Fracture::fracture`] turns the mesh into convex pieces
//! plus the connectivity graph of which pieces touch.
//!
//! The source mesh is treated as convex: pieces fill its convex shape, so
//! a non-convex mesh loses its dents. Every piece is convex, so it can use
//! a `ConvexMesh` collider as it is.
//!
//! At play time [`break_fractures`] breaks an object whose contacts in one
//! fixed tick bring at least `break_energy` joules of impact energy (the
//! stress of every hit that tick, summed): the object is replaced by one
//! dynamic `ConvexMesh` body per piece, moving as the object moved, with
//! the energy beyond `break_energy` scattering the pieces away from the
//! hardest hit.
//!
//! Each break sends one [`Fractured`] event naming the object, its pieces,
//! where it was hit, how hard, and the world box it filled, so audio,
//! particles, navigation and gameplay can react without polling.
//!
//! The pieces are [`Debris`], and [`manage_debris`] keeps them cheap. Only
//! sleeping debris is ever removed or merged, so nothing vanishes in
//! flight: a piece asleep for its fracture's `debris_lifetime` despawns,
//! and with `merge_debris` the pieces of one break, once all asleep, become
//! one static triangle-mesh body. At the [`DebrisBudget`] a new break first
//! removes the longest-sleeping debris to make room, oldest sleep tick
//! first, then lowest [`SpawnOrder`]; if too little debris sleeps, the
//! object stays whole this tick rather than dropping moving pieces. GPU
//! debris counts as asleep once its read-back [`GpuStateMirror`] shows the
//! GPU solver stopped it, from the tick of that state.
//!
//! Determinism: Voronoi seeds come from [`RandomSeed`] streams of the
//! fracture's own seed, cells are cut in index order, objects break in
//! `SceneId` order (then `SpawnOrder`, never entity id), and the scatter is drawn from the scene's
//! [`RandomSeed`] at the fixed tick, so the same scene and inputs always
//! give the same bits.

use std::collections::BTreeMap;
use std::time::Duration;

use bevy_ecs::entity::Entity;
use bevy_ecs::prelude::{Component, Has, Resource, World};
use nalgebra::Vector3;

use super::cpu_physics::next_spawn_order;
use crate::assets::{AssetServer, MeshAsset, MeshVertex};
use crate::runtime::{
    sim_math, Collider, ColliderShape, CollisionLayers, EventQueue, FrameTime,
    GpuStateMirror, Heightfield, MeshRenderer, Name, PhysicsBody,
    PhysicsSyncMode, PhysicsWorld, RandomSeed, RigidBody, RigidBodyKind,
    SceneId, SimulationClass, Sleeping, SpawnOrder,
};
use crate::Transform;

/// Most pieces one fracture makes, Voronoi cells and slice halves
/// together. Fracture settings come from scene files, and cutting costs
/// grow with the square of the piece count.
pub const MAX_FRACTURE_PIECES: usize = 256;

/// How an object's mesh breaks into pieces.
#[derive(
    Component, Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize,
)]
#[serde(default)]
pub struct Fracture {
    /// Voronoi cells to cut the mesh into; 0 or 1 makes no Voronoi cuts.
    pub pieces: u32,
    /// Picks where the Voronoi cells lie.
    pub seed: u64,
    /// Planes `[x, y, z, d]` in the object's local frame, each cutting every
    /// piece along the points `p` with `[x, y, z] · p = d`.
    pub slices: Vec<[f32; 4]>,
    /// Impact energy in joules that breaks the object during play within
    /// one fixed tick; 0 never breaks it.
    pub break_energy: f32,
    /// Seconds a piece stays once it sleeps; 0 keeps pieces.
    pub debris_lifetime: f32,
    /// Once every piece of a break sleeps, they merge into one static body.
    pub merge_debris: bool,
}

/// Most [`Debris`] bodies alive at once; 0 is no cap. At the cap a break
/// removes the longest-sleeping debris (by sleep tick, then [`SpawnOrder`])
/// or, when too little sleeps, waits; moving debris is never dropped.
#[derive(Resource, Clone, Copy, Debug, PartialEq, Eq)]
pub struct DebrisBudget(pub u32);

impl Default for DebrisBudget {
    fn default() -> Self {
        Self(512)
    }
}

/// Merges small sleeping [`Debris`] into one fixed triangle-mesh body per
/// grid cell, so a long match leaves few bodies however much breaks. Each
/// tick, the sleeping pieces no wider than `max_size` metres, and earlier
/// aggregates, are grouped by the `cell`-metre cube their box centre is in;
/// a cube with two or more becomes one body. Pieces merge in
/// [`SpawnOrder`], and the body takes the first one's look and spawn
/// order. A `cell` of 0 turns this off.
#[derive(Resource, Clone, Copy, Debug, PartialEq)]
pub struct DebrisAggregation {
    pub cell: f32,
    pub max_size: f32,
}

impl Default for DebrisAggregation {
    fn default() -> Self {
        Self {
            cell: 0.0,
            max_size: 0.5,
        }
    }
}

/// Sent in the fixed step an object with a [`Fracture`] broke, after it
/// was despawned and its pieces spawned. Breaks send in `SceneId` order.
#[derive(Clone, Debug, PartialEq)]
pub struct Fractured {
    /// The despawned object, or the terrain a chunk was dug out of.
    pub entity: Entity,
    pub name: Option<String>,
    /// The new [`Debris`] bodies, in spawn order.
    pub pieces: Vec<Entity>,
    /// World point of the hardest hit that tick.
    pub point: [f32; 3],
    /// Impact energy in joules summed over the tick.
    pub energy: f32,
    pub mass: f32,
    /// World box `[min, max]` the object filled before it broke, for
    /// navigation and other spatial caches to rebuild.
    pub bounds: [[f32; 3]; 2],
}

/// Sent in the fixed step sleeping [`Debris`] froze back into a terrain
/// ([`TerrainFracture::refreeze`]), one per terrain and tick.
#[derive(Clone, Debug, PartialEq)]
pub struct Refrozen {
    pub terrain: Entity,
    /// World box `[min, max]` of the frozen pieces, for navigation and other
    /// spatial caches to rebuild.
    pub bounds: [[f32; 3]; 2],
}

/// A body made by a break.
#[derive(Component, Clone, Copy, Debug, PartialEq)]
pub struct Debris {
    /// The [`SpawnOrder`] of the break's first piece, shared by all its
    /// pieces.
    pub group: u64,
    pub lifetime: f32,
    pub merge: bool,
    /// The fixed tick it fell asleep on; `None` while it moves.
    pub asleep_since: Option<u64>,
}

impl Default for Fracture {
    fn default() -> Self {
        Self {
            pieces: 8,
            seed: 1,
            slices: Vec::new(),
            break_energy: 0.0,
            debris_lifetime: 0.0,
            merge_debris: false,
        }
    }
}

/// The pieces of a fractured mesh, in the source mesh's frame.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FracturedMesh {
    pub pieces: Vec<MeshAsset>,
    /// Pairs of pieces (lower index first, sorted) that share a face of
    /// some area; touching only at an edge or a corner is no link.
    pub links: Vec<[u32; 2]>,
}

type V = Vector3<f32>;

/// The cut a face came from; the source surface is no cut.
const SURFACE: u32 = u32::MAX;
/// Cut numbers of slicing planes start here, above the Voronoi pairs.
const SLICE_CUTS: u32 = 1 << 20;

#[derive(Clone, Debug)]
struct Face {
    /// Counterclockwise seen from outside the cell.
    points: Vec<V>,
    cut: u32,
}

type Cell = Vec<Face>;

impl Fracture {
    /// Cuts `mesh` into convex pieces and finds which ones touch.
    pub fn fracture(&self, mesh: &MeshAsset) -> FracturedMesh {
        let points: Vec<V> =
            mesh.vertices.iter().map(|v| V::from(v.position)).collect();
        let Some(base) = hull_cell(mesh, &points) else {
            return FracturedMesh::default();
        };
        let low = points.iter().fold(points[0], |m, p| m.inf(p));
        let high = points.iter().fold(points[0], |m, p| m.sup(p));
        let size = (high - low).norm();
        let eps = 1e-5 * size;

        let count = (self.pieces as usize).min(MAX_FRACTURE_PIECES);
        let mut cells = if count > 1 {
            let seeds = seeds(&base, low, high, count, self.seed);
            voronoi(&base, &seeds, eps)
        } else {
            vec![base]
        };
        for (k, slice) in self.slices.iter().enumerate() {
            let normal = V::new(slice[0], slice[1], slice[2]);
            let Some(length) = Some(normal.norm()).filter(|l| *l > 1e-12)
            else {
                continue;
            };
            let (normal, offset) = (normal / length, slice[3] / length);
            let cut = SLICE_CUTS + k as u32;
            let mut halves = Vec::new();
            for (i, cell) in cells.iter().enumerate() {
                if halves.len() + cells.len() - i >= MAX_FRACTURE_PIECES {
                    halves.push(cell.clone());
                    continue;
                }
                halves.extend(
                    [(normal, offset), (-normal, -offset)]
                        .into_iter()
                        .map(|(n, d)| clip(cell, n, d, cut, eps))
                        .filter(|half| !half.is_empty()),
                );
            }
            cells = halves;
        }
        FracturedMesh {
            links: links(&cells, size * size * 1e-6),
            pieces: cells.iter().map(piece_mesh).collect(),
        }
    }
}

/// Per fixed step, after physics: breaks every object with a
/// [`Fracture`] whose contacts this tick brought `break_energy` or more.
pub(super) fn break_fractures(world: &mut World) {
    let mut struck = BTreeMap::<Entity, Impact>::new();
    {
        let mass = |entity| {
            world
                .get::<RigidBody>(entity)
                .filter(|body| body.kind == RigidBodyKind::Dynamic)
                .map(|body| body.mass)
        };
        let Some(physics) = world.get_resource::<PhysicsWorld>() else {
            return;
        };
        for contact in physics.contacts() {
            if contact.sensor || contact.speed <= 0.0 {
                continue;
            }
            // Energy the hit takes out of the closing motion: half the
            // reduced mass times the closing speed squared.
            let reduced = match (mass(contact.a), mass(contact.b)) {
                (Some(a), Some(b)) => a * b / (a + b).max(f32::MIN_POSITIVE),
                (Some(m), None) | (None, Some(m)) => m,
                (None, None) => continue,
            };
            let energy = 0.5 * reduced * contact.speed * contact.speed;
            for body in [contact.a, contact.b] {
                let impact = struck.entry(body).or_default();
                impact.energy += energy;
                if energy > impact.hardest {
                    impact.hardest = energy;
                    impact.point = contact.point.into();
                }
            }
        }
    }
    let mut breaking: Vec<_> = struck
        .into_iter()
        .filter_map(|(entity, impact)| {
            let fracture = world.get::<Fracture>(entity)?;
            (fracture.break_energy > 0.0
                && impact.energy >= fracture.break_energy)
                .then(|| {
                    // Entity ids shift with unrelated spawns, so the scene
                    // id or spawn order picks the order and the stream.
                    let id =
                        world.get::<SceneId>(entity).map(|id| id.0.as_u128());
                    let order = world.get::<SpawnOrder>(entity).map(|o| o.0);
                    (id, order, entity, impact)
                })
        })
        .collect();
    breaking.sort_unstable_by_key(|(id, order, entity, _)| {
        (*id, *order, entity.to_bits())
    });
    let tick = world
        .get_resource::<FrameTime>()
        .map_or(0, |t| t.fixed_tick);
    let seed = world
        .get_resource::<RandomSeed>()
        .copied()
        .unwrap_or_default();
    for (id, order, entity, impact) in breaking {
        let key = match (id, order) {
            (Some(id), _) => id as u64 ^ (id >> 64) as u64,
            (None, Some(order)) => order,
            (None, None) => entity.to_bits(),
        };
        let stream = RandomSeed::stream("fracture", key);
        shatter(world, entity, impact, |k| seed.unit(tick, stream ^ k));
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct Impact {
    energy: f32,
    hardest: f32,
    point: V,
}

/// Replaces `entity` by its pieces; `draw(k)` is the k-th random value in
/// [0, 1) of this break.
fn shatter(
    world: &mut World,
    entity: Entity,
    impact: Impact,
    draw: impl Fn(u64) -> f32,
) {
    let (Some(fracture), Some(transform), Some(renderer)) = (
        world.get::<Fracture>(entity).cloned(),
        world.get::<Transform>(entity).copied(),
        world.get::<MeshRenderer>(entity).copied(),
    ) else {
        return;
    };
    let Some(mesh) = world
        .get_resource::<AssetServer>()
        .and_then(|assets| assets.meshes.get(renderer.mesh))
        .cloned()
    else {
        return;
    };
    let broken = fracture.fracture(&mesh);
    if broken.pieces.is_empty() {
        return;
    }
    let body = world.get::<RigidBody>(entity).copied().unwrap_or_default();
    let collider = world.get::<Collider>(entity).copied().unwrap_or_default();
    let layers = world.get::<CollisionLayers>(entity).copied();
    let [roll, pitch, yaw] = transform.rotation;
    let turn = sim_math::rotation_from_euler(roll, pitch, yaw);
    let scale = V::from(transform.scale);
    let moving = body.kind == RigidBodyKind::Dynamic;
    let (velocity, spin) = if moving {
        (
            V::from(body.linear_velocity),
            V::from(body.angular_velocity),
        )
    } else {
        (V::zeros(), V::zeros())
    };
    let pieces: Vec<(MeshAsset, V, f32)> =
        broken.pieces.into_iter().map(centred).collect();
    let total: f32 = pieces.iter().map(|(.., volume)| volume).sum();
    if !make_room(world, pieces.len()) {
        return;
    }
    let excess = (impact.energy - fracture.break_energy).max(0.0);
    let scatter = (excess / body.mass.max(1e-6)).sqrt();
    let mut bounds = [[f32::INFINITY; 3], [f32::NEG_INFINITY; 3]];
    for vertex in &mesh.vertices {
        let p = V::from(transform.position)
            + turn * V::from(vertex.position).component_mul(&scale);
        for axis in 0..3 {
            bounds[0][axis] = bounds[0][axis].min(p[axis]);
            bounds[1][axis] = bounds[1][axis].max(p[axis]);
        }
    }
    let name = world.get::<Name>(entity).map(|name| name.0.clone());
    world.despawn(entity);
    let mut group = None;
    let mut spawned = Vec::with_capacity(pieces.len());
    for (k, (mesh, center, volume)) in pieces.into_iter().enumerate() {
        let offset = turn * center.component_mul(&scale);
        let position = V::from(transform.position) + offset;
        let away = (position - impact.point)
            .try_normalize(1e-6)
            .unwrap_or_default();
        let kick = scatter * (0.5 + 0.5 * draw(k as u64));
        let mesh = match world.get_resource_mut::<AssetServer>() {
            Some(mut assets) => assets.meshes.insert(mesh),
            None => return,
        };
        let order = next_spawn_order(world);
        let group = *group.get_or_insert(order.0);
        let mut piece = world.spawn((
            Transform {
                position: position.into(),
                ..transform
            },
            MeshRenderer { mesh, ..renderer },
            Collider {
                shape: ColliderShape::ConvexMesh,
                ..collider
            },
            PhysicsBody::default(),
            RigidBody {
                kind: RigidBodyKind::Dynamic,
                mass: body.mass * volume / total.max(f32::MIN_POSITIVE),
                linear_velocity: (velocity + spin.cross(&offset) + away * kick)
                    .into(),
                angular_velocity: spin.into(),
                gravity_scale: body.gravity_scale,
            },
            order,
            Debris {
                group,
                lifetime: fracture.debris_lifetime,
                merge: fracture.merge_debris,
                asleep_since: None,
            },
        ));
        if let Some(layers) = layers {
            piece.insert(layers);
        }
        spawned.push(piece.id());
    }
    if let Some(mut events) = world.get_resource_mut::<EventQueue<Fractured>>()
    {
        events.send(Fractured {
            entity,
            name,
            pieces: spawned,
            point: impact.point.into(),
            energy: impact.energy,
            mass: body.mass,
            bounds,
        });
    }
}

/// Removes the longest-sleeping debris until `count` more fit the budget.
/// False, removing nothing, when too little debris sleeps.
fn make_room(world: &mut World, count: usize) -> bool {
    let budget = world
        .get_resource::<DebrisBudget>()
        .copied()
        .unwrap_or_default()
        .0 as usize;
    if budget == 0 {
        return true;
    }
    let mut debris: Vec<_> = world
        .query::<(Entity, &Debris, Option<&SpawnOrder>)>()
        .iter(world)
        .map(|(entity, debris, order)| {
            (debris.asleep_since, order.map(|o| o.0), entity)
        })
        .collect();
    let over = (debris.len() + count).saturating_sub(budget);
    if over == 0 {
        return true;
    }
    debris.retain(|(asleep, ..)| asleep.is_some());
    if debris.len() < over || count > budget {
        return false;
    }
    debris.sort_unstable_by_key(|&(asleep, order, entity)| {
        (asleep, order, entity.to_bits())
    });
    for &(.., entity) in &debris[..over] {
        remove_debris(world, entity);
    }
    true
}

/// Despawns a debris body and frees its own mesh.
fn remove_debris(world: &mut World, entity: Entity) {
    if let Some(renderer) = world.get::<MeshRenderer>(entity).copied() {
        if let Some(mut assets) = world.get_resource_mut::<AssetServer>() {
            let _ = assets.meshes.remove(renderer.mesh);
        }
    }
    world.despawn(entity);
}

/// Per fixed step, after breaks: notes when debris falls asleep, removes
/// debris asleep past its lifetime, and merges settled breaks.
pub(super) fn manage_debris(world: &mut World) {
    let (tick, step) = world
        .get_resource::<FrameTime>()
        .map_or((0, Duration::ZERO), |time| {
            (time.fixed_tick, time.fixed_delta)
        });
    let mut pieces: Vec<_> = world
        .query::<(
            Entity,
            &mut Debris,
            Has<Sleeping>,
            Option<&RigidBody>,
            Option<&SpawnOrder>,
            Option<&PhysicsBody>,
            Option<&GpuStateMirror>,
        )>()
        .iter_mut(world)
        .map(
            |(entity, mut debris, sleeping, body, order, physics, mirror)| {
                // The GPU solver zeroes a body's velocities when it puts it to
                // sleep, and a falling body never reads back exactly zero.
                let gpu_asleep = physics
                    .is_some_and(|p| p.simulation == SimulationClass::Gpu)
                    .then_some(mirror)
                    .flatten()
                    .filter(|m| {
                        m.linear_velocity == [0.0; 3]
                            && m.angular_velocity == [0.0; 3]
                    })
                    .map(|m| m.tick);
                let still = sleeping
                    || gpu_asleep.is_some()
                    || body.is_some_and(|b| b.kind != RigidBodyKind::Dynamic);
                if !still {
                    debris.asleep_since = None;
                } else if debris.asleep_since.is_none() {
                    debris.asleep_since = Some(gpu_asleep.unwrap_or(tick));
                }
                (order.map(|o| o.0), entity.to_bits(), entity, *debris)
            },
        )
        .collect();
    pieces.sort_unstable_by_key(|&(order, bits, ..)| (order, bits));
    let step = step.as_secs_f32();
    // Each break's still pieces, or `None` once one of them moves.
    let mut groups = BTreeMap::<u64, Option<Vec<Entity>>>::new();
    for &(.., entity, debris) in &pieces {
        let Some(since) = debris.asleep_since else {
            groups.insert(debris.group, None);
            continue;
        };
        let asleep = tick.saturating_sub(since) as f32 * step;
        if debris.lifetime > 0.0 && asleep >= debris.lifetime {
            remove_debris(world, entity);
            continue;
        }
        if debris.merge {
            if let Some(members) =
                groups.entry(debris.group).or_insert(Some(Vec::new()))
            {
                members.push(entity);
            }
        }
    }
    for members in groups.into_values().flatten() {
        if members.len() > 1 {
            merge(world, &members);
        }
    }
    let aggregation = world
        .get_resource::<DebrisAggregation>()
        .copied()
        .unwrap_or_default();
    if aggregation.cell > 0.0 {
        aggregate(world, aggregation);
    }
}

/// Merges sleeping debris per [`DebrisAggregation`] cell.
fn aggregate(world: &mut World, settings: DebrisAggregation) {
    let mut pieces: Vec<_> = world
        .query::<(
            Entity,
            &Debris,
            Option<&SpawnOrder>,
            Option<&Collider>,
            Option<&RigidBody>,
        )>()
        .iter(world)
        .filter(|(_, debris, ..)| debris.asleep_since.is_some())
        .map(|(entity, _, order, collider, body)| {
            // What `merge` makes: a fixed triangle mesh.
            let merged = collider
                .is_some_and(|c| c.shape == ColliderShape::TriangleMesh)
                && body.is_some_and(|b| b.kind == RigidBodyKind::Fixed);
            (order.map(|o| o.0), entity, merged)
        })
        .collect();
    pieces
        .sort_unstable_by_key(|&(order, entity, _)| (order, entity.to_bits()));
    let mut cells = BTreeMap::<[i64; 3], Vec<Entity>>::new();
    for (_, entity, merged) in pieces {
        let Some(points) = world_points(world, entity) else {
            continue;
        };
        let low = points.iter().fold(points[0], |m, p| m.inf(p));
        let high = points.iter().fold(points[0], |m, p| m.sup(p));
        if !merged && (high - low).max() > settings.max_size {
            continue;
        }
        let centre = 0.5 * (low + high) / settings.cell;
        let key = [0, 1, 2].map(|axis| centre[axis].floor() as i64);
        cells.entry(key).or_default().push(entity);
    }
    for members in cells.into_values() {
        if members.len() > 1 {
            merge(world, &members);
        }
    }
}

/// Replaces sleeping pieces with one fixed triangle-mesh body in world
/// space, taking the first piece's look, collider and spawn order.
fn merge(world: &mut World, members: &[Entity]) {
    let mut mesh = MeshAsset::default();
    let mut mass = 0.0;
    for &entity in members {
        let (Some(transform), Some(renderer)) = (
            pose(world, entity),
            world.get::<MeshRenderer>(entity).copied(),
        ) else {
            continue;
        };
        let Some(piece) = world
            .get_resource::<AssetServer>()
            .and_then(|assets| assets.meshes.get(renderer.mesh))
        else {
            continue;
        };
        let [roll, pitch, yaw] = transform.rotation;
        let turn = sim_math::rotation_from_euler(roll, pitch, yaw);
        let scale = V::from(transform.scale);
        let base = mesh.vertices.len() as u32;
        mesh.vertices.extend(piece.vertices.iter().map(|vertex| {
            let normal = turn * V::from(vertex.normal).component_div(&scale);
            MeshVertex {
                position: (V::from(transform.position)
                    + turn * V::from(vertex.position).component_mul(&scale))
                .into(),
                normal: normal.try_normalize(1e-12).unwrap_or(normal).into(),
                ..*vertex
            }
        }));
        mesh.indices.extend(piece.indices.iter().map(|&i| base + i));
        mass += world.get::<RigidBody>(entity).map_or(0.0, |body| body.mass);
    }
    let first = members[0];
    let (Some(renderer), Some(order), Some(debris)) = (
        world.get::<MeshRenderer>(first).copied(),
        world.get::<SpawnOrder>(first).copied(),
        world.get::<Debris>(first).copied(),
    ) else {
        return;
    };
    let collider = world.get::<Collider>(first).copied().unwrap_or_default();
    let layers = world.get::<CollisionLayers>(first).copied();
    let Some(mesh) = world
        .get_resource_mut::<AssetServer>()
        .map(|mut assets| assets.meshes.insert(mesh))
    else {
        return;
    };
    for &entity in members {
        remove_debris(world, entity);
    }
    let mut merged = world.spawn((
        Transform::default(),
        MeshRenderer { mesh, ..renderer },
        Collider {
            shape: ColliderShape::TriangleMesh,
            ..collider
        },
        PhysicsBody::default(),
        RigidBody {
            kind: RigidBodyKind::Fixed,
            mass,
            ..RigidBody::default()
        },
        order,
        debris,
    ));
    if let Some(layers) = layers {
        merged.insert(layers);
    }
}

/// `mesh` moved so its centre of volume is the origin, that centre, and
/// its volume.
fn centred(mut mesh: MeshAsset) -> (MeshAsset, V, f32) {
    let mut volume = 0.0;
    let mut moment = V::zeros();
    for triangle in mesh.indices.chunks_exact(3) {
        let [a, b, c] = [0, 1, 2]
            .map(|k| V::from(mesh.vertices[triangle[k] as usize].position));
        // The tetrahedron from the origin to the triangle.
        let part = a.dot(&b.cross(&c)) / 6.0;
        volume += part;
        moment += part * (a + b + c) / 4.0;
    }
    let center = moment / if volume.abs() > 0.0 { volume } else { 1.0 };
    for vertex in &mut mesh.vertices {
        vertex.position = (V::from(vertex.position) - center).into();
    }
    (mesh, center, volume)
}

/// The convex shape of `mesh`: its box cut by every triangle's plane.
fn hull_cell(mesh: &MeshAsset, points: &[V]) -> Option<Cell> {
    let first = *points.first()?;
    let low = points.iter().fold(first, |m, p| m.inf(p));
    let high = points.iter().fold(first, |m, p| m.sup(p));
    let eps = 1e-5 * (high - low).norm();
    let center = points.iter().sum::<V>() / points.len() as f32;
    let indices: Vec<usize> = if mesh.indices.is_empty() {
        (0..points.len()).collect()
    } else {
        mesh.indices.iter().map(|&i| i as usize).collect()
    };
    let mut cell = box_cell(low, high);
    for triangle in indices.chunks_exact(3) {
        let Some([a, b, c]) = triangle
            .iter()
            .map(|&i| points.get(i).copied())
            .collect::<Option<Vec<V>>>()
            .and_then(|corners| <[V; 3]>::try_from(corners).ok())
        else {
            continue;
        };
        let Some(mut normal) = (b - a).cross(&(c - a)).try_normalize(1e-12)
        else {
            continue;
        };
        if normal.dot(&(a - center)) < 0.0 {
            normal = -normal;
        }
        cell = clip(&cell, normal, normal.dot(&a), SURFACE, eps);
    }
    (cell.len() >= 4).then_some(cell)
}

/// The box from `low` to `high`.
fn box_cell(low: V, high: V) -> Cell {
    let mut cell = Vec::new();
    for axis in 0..3 {
        let (b, c) = ((axis + 1) % 3, (axis + 2) % 3);
        for (at, outward) in [(high[axis], true), (low[axis], false)] {
            let corner = |u: f32, v: f32| {
                let mut p = V::zeros();
                p[axis] = at;
                p[b] = u;
                p[c] = v;
                p
            };
            // Counterclockwise around +axis in (b, c) order.
            let mut points = vec![
                corner(low[b], low[c]),
                corner(high[b], low[c]),
                corner(high[b], high[c]),
                corner(low[b], high[c]),
            ];
            if !outward {
                points.reverse();
            }
            cell.push(Face {
                points,
                cut: SURFACE,
            });
        }
    }
    cell
}

/// `count` points inside `cell`, drawn from the seed's stream.
fn seeds(cell: &Cell, low: V, high: V, count: usize, seed: u64) -> Vec<V> {
    let stream = RandomSeed(seed);
    let inside = |p: &V| {
        cell.iter().all(|face| {
            let normal = face_normal(&face.points);
            normal.dot(&(p - face.points[0])) < 0.0
        })
    };
    let mut seeds = Vec::with_capacity(count);
    let mut draw = 0u64;
    // A thin shape in a big box rejects most draws; give up rather than
    // loop forever, and make fewer pieces.
    while seeds.len() < count && draw < 64 * count as u64 {
        let p = V::from_fn(|axis, _| {
            let t = stream.unit(0, draw * 3 + axis as u64);
            low[axis] + (high[axis] - low[axis]) * t
        });
        draw += 1;
        if inside(&p) {
            seeds.push(p);
        }
    }
    seeds
}

/// The Voronoi cell of each seed within `base`.
fn voronoi(base: &Cell, seeds: &[V], eps: f32) -> Vec<Cell> {
    let n = seeds.len();
    let mut cells = Vec::with_capacity(n);
    for i in 0..n {
        let mut cell = base.clone();
        for j in 0..n {
            let Some(normal) = (seeds[j] - seeds[i]).try_normalize(1e-12)
            else {
                continue;
            };
            if cell.is_empty() {
                break;
            }
            let middle = 0.5 * (seeds[i] + seeds[j]);
            let cut = (i.min(j) * n + i.max(j)) as u32;
            cell = clip(&cell, normal, normal.dot(&middle), cut, eps);
        }
        if !cell.is_empty() {
            cells.push(cell);
        }
    }
    cells
}

/// The part of `points` (a convex polygon) with `normal · p <= offset`.
fn clip_polygon(points: &[V], normal: V, offset: f32) -> Vec<V> {
    let mut out = Vec::with_capacity(points.len() + 1);
    for (k, &a) in points.iter().enumerate() {
        let b = points[(k + 1) % points.len()];
        let (da, db) = (normal.dot(&a) - offset, normal.dot(&b) - offset);
        if da <= 0.0 {
            out.push(a);
        }
        if (da < 0.0 && db > 0.0) || (da > 0.0 && db < 0.0) {
            out.push(a + (b - a) * (da / (da - db)));
        }
    }
    out
}

/// The part of `cell` with `normal · p <= offset`, closed by a face of
/// `cut`. Empty when nothing of the cell is on that side.
fn clip(cell: &Cell, normal: V, offset: f32, cut: u32, eps: f32) -> Cell {
    let side = |p: &V| normal.dot(p) - offset;
    let all = || cell.iter().flat_map(|face| &face.points);
    if all().all(|p| side(p) <= eps) {
        // Nothing (or only a sliver within rounding) is cut off.
        return cell.clone();
    }
    if all().all(|p| side(p) >= -eps) {
        return Vec::new();
    }
    let mut out = Vec::with_capacity(cell.len() + 1);
    let mut rim: Vec<V> = Vec::new();
    for face in cell {
        let points = clip_polygon(&face.points, normal, offset);
        for p in &points {
            if side(p).abs() <= eps
                && !rim.iter().any(|q| (q - p).norm() <= eps)
            {
                rim.push(*p);
            }
        }
        if points.len() >= 3 {
            out.push(Face {
                points,
                cut: face.cut,
            });
        }
    }
    if rim.len() >= 3 {
        let center = rim.iter().sum::<V>() / rim.len() as f32;
        let u = (rim[0] - center).normalize();
        let v = normal.cross(&u);
        // Counterclockwise around `normal`, which points out of the kept
        // side. A pseudo-angle keeps the order free of platform trig.
        let angle = |p: &V| {
            let (x, y) = ((p - center).dot(&u), (p - center).dot(&v));
            let t = y / (x.abs() + y.abs()).max(f32::MIN_POSITIVE);
            if x < 0.0 {
                2.0 - t
            } else if y < 0.0 {
                4.0 + t
            } else {
                t
            }
        };
        rim.sort_by(|a, b| angle(a).total_cmp(&angle(b)));
        out.push(Face { points: rim, cut });
    }
    if out.len() < 4 {
        return Vec::new();
    }
    out
}

/// Newell's normal of a polygon, unit length (zero when degenerate).
fn face_normal(points: &[V]) -> V {
    let mut normal = V::zeros();
    for (k, a) in points.iter().enumerate() {
        normal += a.cross(&points[(k + 1) % points.len()]);
    }
    normal.try_normalize(1e-20).unwrap_or_default()
}

fn area(points: &[V]) -> f32 {
    let mut sum = V::zeros();
    for (k, a) in points.iter().enumerate() {
        sum += a.cross(&points[(k + 1) % points.len()]);
    }
    0.5 * sum.norm()
}

/// Pairs of cells with faces of the same cut that overlap by more than
/// `min_area`.
fn links(cells: &[Cell], min_area: f32) -> Vec<[u32; 2]> {
    let mut links = Vec::new();
    for a in 0..cells.len() {
        for b in a + 1..cells.len() {
            let touching = cells[a].iter().any(|fa| {
                fa.cut != SURFACE
                    && cells[b].iter().any(|fb| {
                        fb.cut == fa.cut
                            && overlap(&fa.points, &fb.points) > min_area
                    })
            });
            if touching {
                links.push([a as u32, b as u32]);
            }
        }
    }
    links
}

/// Area two coplanar convex polygons share.
fn overlap(a: &[V], b: &[V]) -> f32 {
    let normal = face_normal(b);
    let mut shared = a.to_vec();
    for (k, p) in b.iter().enumerate() {
        if shared.len() < 3 {
            return 0.0;
        }
        let edge = b[(k + 1) % b.len()] - p;
        // Inside is left of each edge seen along the outward normal.
        let out = edge.cross(&normal);
        shared = clip_polygon(&shared, out, out.dot(p));
    }
    if shared.len() < 3 {
        return 0.0;
    }
    area(&shared)
}

/// Flat-shaded triangles of a cell's faces.
fn piece_mesh(cell: &Cell) -> MeshAsset {
    let mut mesh = MeshAsset::default();
    for face in cell {
        let normal = face_normal(&face.points);
        let u = (face.points[1] - face.points[0])
            .try_normalize(1e-12)
            .unwrap_or_else(V::x);
        let v = normal.cross(&u);
        let first = mesh.vertices.len() as u32;
        mesh.vertices.extend(face.points.iter().map(|p| MeshVertex {
            position: (*p).into(),
            normal: normal.into(),
            uv: [p.dot(&u), p.dot(&v)],
            tangent: [u.x, u.y, u.z, 1.0],
            color: [1.0; 4],
        }));
        for k in 1..face.points.len() as u32 - 1 {
            mesh.indices.extend([first, first + k, first + k + 1]);
        }
    }
    mesh
}

/// Makes a [`Heightfield`] break in square chunks of `chunk_cells` cells.
/// A chunk whose contacts in one fixed tick bring `break_energy` joules is
/// promoted: its height samples drop `depth` metres and the dug-out slab
/// becomes one dynamic GPU box body ([`Debris`]), resting in the hole and
/// thrown up by the energy beyond `break_energy`.
#[derive(
    Component, Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize,
)]
#[serde(default)]
pub struct TerrainFracture {
    pub chunk_cells: u32,
    /// Voronoi pieces a dug-out slab breaks into, around points drawn from
    /// the scene's [`RandomSeed`] at the tick; 0 or 1 keeps one box.
    pub pieces: u32,
    /// Joules within one fixed tick; 0 never breaks.
    pub break_energy: f32,
    /// Metres the chunk's ground drops; the slab's thickness.
    pub depth: f32,
    /// kg per cubic metre of the slab.
    pub density: f32,
    /// Seconds a slab stays once it sleeps; 0 keeps it.
    pub debris_lifetime: f32,
    /// Seconds any [`Debris`] resting on this terrain sleeps before it
    /// freezes back into the heights; 0 never refreezes.
    pub refreeze: f32,
    /// Chunks already promoted, as `row * chunk_columns + column`, sorted.
    /// A chunk is promoted once.
    pub broken: Vec<u32>,
}

impl Default for TerrainFracture {
    fn default() -> Self {
        Self {
            chunk_cells: 4,
            pieces: 1,
            break_energy: 0.0,
            depth: 0.5,
            density: 2000.0,
            debris_lifetime: 0.0,
            refreeze: 0.0,
            broken: Vec::new(),
        }
    }
}

/// Promotes struck [`TerrainFracture`] chunks, after [`break_fractures`].
/// Terrains go in `SceneId` order (then `SpawnOrder`), chunks in index
/// order, and the throw is drawn from the scene's [`RandomSeed`] at the
/// fixed tick. At the [`DebrisBudget`] a chunk stays whole that tick, as an
/// object does.
// ponytail: the GPU solver sees a heightfield as its bounding box, so a slab
// lifted inside that box is pushed onto its top; give the GPU heightfield
// contacts if slabs must settle back into their holes.
pub(super) fn break_terrain(world: &mut World) {
    let mut struck = BTreeMap::<(Entity, u32), Impact>::new();
    {
        let Some(physics) = world.get_resource::<PhysicsWorld>() else {
            return;
        };
        for contact in physics.contacts() {
            if contact.sensor || contact.speed <= 0.0 {
                continue;
            }
            for (terrain, other) in
                [(contact.a, contact.b), (contact.b, contact.a)]
            {
                let (Some(fracture), Some(field), Some(transform)) = (
                    world.get::<TerrainFracture>(terrain),
                    world.get::<Heightfield>(terrain),
                    world.get::<Transform>(terrain),
                ) else {
                    continue;
                };
                let Some(mass) = world
                    .get::<RigidBody>(other)
                    .filter(|body| body.kind == RigidBodyKind::Dynamic)
                    .map(|body| body.mass)
                else {
                    continue;
                };
                let point = V::from(contact.point);
                let Some(chunk) = chunk_at(fracture, field, transform, point)
                else {
                    continue;
                };
                let energy = 0.5 * mass * contact.speed * contact.speed;
                let impact = struck.entry((terrain, chunk)).or_default();
                impact.energy += energy;
                if energy > impact.hardest {
                    impact.hardest = energy;
                    impact.point = point;
                }
            }
        }
    }
    let mut breaking: Vec<_> = struck
        .into_iter()
        .filter(|((terrain, chunk), impact)| {
            world.get::<TerrainFracture>(*terrain).is_some_and(|f| {
                f.break_energy > 0.0
                    && impact.energy >= f.break_energy
                    && f.broken.binary_search(chunk).is_err()
            })
        })
        .map(|((terrain, chunk), impact)| {
            let id = world.get::<SceneId>(terrain).map(|id| id.0.as_u128());
            let order = world.get::<SpawnOrder>(terrain).map(|o| o.0);
            (id, order, terrain, chunk, impact)
        })
        .collect();
    breaking.sort_unstable_by_key(|(id, order, terrain, chunk, _)| {
        (*id, *order, terrain.to_bits(), *chunk)
    });
    let tick = world
        .get_resource::<FrameTime>()
        .map_or(0, |t| t.fixed_tick);
    let seed = world
        .get_resource::<RandomSeed>()
        .copied()
        .unwrap_or_default();
    for (id, order, terrain, chunk, impact) in breaking {
        let key = match (id, order) {
            (Some(id), _) => id as u64 ^ (id >> 64) as u64,
            (None, Some(order)) => order,
            // Never the entity id, which shifts with unrelated spawns.
            (None, None) => 0,
        };
        let stream =
            RandomSeed::stream("terrain_fracture", key) ^ u64::from(chunk);
        // The Voronoi points and the throws are new each tick, so two
        // chunks never crack alike, yet a replay cracks them the same.
        let pattern = seed.value(tick, stream ^ u64::MAX);
        promote(world, terrain, chunk, impact, pattern, |k| {
            seed.unit(tick, stream ^ k)
        });
    }
}

/// Columns and rows of chunks over `field`'s cells.
fn chunk_grid(fracture: &TerrainFracture, field: &Heightfield) -> [usize; 2] {
    let cells = fracture.chunk_cells.max(1) as usize;
    let rows = field.heights.len().saturating_sub(1);
    let columns = field
        .heights
        .iter()
        .map(Vec::len)
        .max()
        .unwrap_or(0)
        .saturating_sub(1);
    [columns.div_ceil(cells), rows.div_ceil(cells)]
}

/// The chunk under the world point `point`, if it is over the field.
fn chunk_at(
    fracture: &TerrainFracture,
    field: &Heightfield,
    transform: &Transform,
    point: V,
) -> Option<u32> {
    let [roll, pitch, yaw] = transform.rotation;
    let turn = sim_math::rotation_from_euler(roll, pitch, yaw);
    let local = (turn.inverse() * (point - V::from(transform.position)))
        .component_div(&V::from(transform.scale));
    let [columns, rows] = chunk_grid(fracture, field);
    let size = fracture.chunk_cells.max(1) as f32 * field.spacing;
    let column = (local.x + 0.5 * cells_x(field) as f32 * field.spacing) / size;
    let row = (local.z + 0.5 * cells_z(field) as f32 * field.spacing) / size;
    let inside = |at: f32, count: usize| at >= 0.0 && at < count as f32;
    (inside(column, columns) && inside(row, rows))
        .then(|| (row as usize * columns + column as usize) as u32)
}

fn cells_x(field: &Heightfield) -> usize {
    field
        .heights
        .iter()
        .map(Vec::len)
        .max()
        .unwrap_or(0)
        .saturating_sub(1)
}

fn cells_z(field: &Heightfield) -> usize {
    field.heights.len().saturating_sub(1)
}

/// Digs `chunk` out of `terrain` and spawns it as dynamic GPU debris, cut
/// into `pieces` Voronoi cells around points picked by `pattern`; `draw(k)`
/// in [0, 1) scales piece k's throw.
fn promote(
    world: &mut World,
    terrain: Entity,
    chunk: u32,
    impact: Impact,
    pattern: u64,
    draw: impl Fn(u64) -> f32,
) {
    let (Some(fracture), Some(mut field), Some(transform)) = (
        world.get::<TerrainFracture>(terrain).cloned(),
        world.get::<Heightfield>(terrain).cloned(),
        world.get::<Transform>(terrain).copied(),
    ) else {
        return;
    };
    let [columns, _] = chunk_grid(&fracture, &field);
    let cells = fracture.chunk_cells.max(1) as usize;
    let (row, column) = (chunk as usize / columns, chunk as usize % columns);
    let first = [column * cells, row * cells];
    let last = [
        ((column + 1) * cells).min(cells_x(&field)),
        ((row + 1) * cells).min(cells_z(&field)),
    ];
    // Every sample of the chunk, edges included, drops by `depth`; short
    // rows count as 0 past their end, as in `Heightfield::mesh`.
    let mut top = 0.0;
    let mut samples = 0.0;
    for z in first[1]..=last[1] {
        let heights = &mut field.heights[z];
        if heights.len() <= last[0] {
            heights.resize(last[0] + 1, 0.0);
        }
        for height in &mut heights[first[0]..=last[0]] {
            top += *height;
            samples += 1.0;
            *height -= fracture.depth;
        }
    }
    let top = top / samples;
    let spacing = field.spacing;
    let local = V::new(
        (0.5 * (first[0] + last[0]) as f32 - 0.5 * cells_x(&field) as f32)
            * spacing,
        top - 0.5 * fracture.depth,
        (0.5 * (first[1] + last[1]) as f32 - 0.5 * cells_z(&field) as f32)
            * spacing,
    );
    let size = V::new(
        (last[0] - first[0]) as f32 * spacing,
        fracture.depth,
        (last[1] - first[1]) as f32 * spacing,
    )
    .component_mul(&V::from(transform.scale));
    let [roll, pitch, yaw] = transform.rotation;
    let turn = sim_math::rotation_from_euler(roll, pitch, yaw);
    let position = V::from(transform.position)
        + turn * local.component_mul(&V::from(transform.scale));
    let half = 0.5 * size;
    // (mesh, offset from the slab centre, scale, shape, volume) per piece.
    let pieces: Vec<(Option<MeshAsset>, V, V, ColliderShape, f32)> =
        if fracture.pieces > 1 {
            let cut = Fracture {
                pieces: fracture.pieces,
                seed: pattern,
                ..Fracture::default()
            };
            cut.fracture(&piece_mesh(&box_cell(-half, half)))
                .pieces
                .into_iter()
                .map(centred)
                .map(|(mesh, center, volume)| {
                    let shape = ColliderShape::ConvexMesh;
                    (Some(mesh), center, V::repeat(1.0), shape, volume)
                })
                .collect()
        } else {
            let shape = ColliderShape::Box {
                half_extents: [0.5; 3],
            };
            vec![(None, V::zeros(), size, shape, size.x * size.y * size.z)]
        };
    if pieces.is_empty() || !make_room(world, pieces.len()) {
        return;
    }
    let mass = fracture.density * size.x * size.y * size.z;
    let excess = (impact.energy - fracture.break_energy).max(0.0);
    let throw = (excess / mass.max(1e-6)).sqrt();
    let up = turn * V::y();
    let mut broken = fracture.broken.clone();
    if let Err(at) = broken.binary_search(&chunk) {
        broken.insert(at, chunk);
    }
    let layers = world.get::<CollisionLayers>(terrain).copied();
    let name = world.get::<Name>(terrain).map(|name| name.0.clone());
    world.entity_mut(terrain).insert((
        field,
        TerrainFracture {
            broken,
            ..fracture.clone()
        },
    ));
    let mut group = None;
    let mut spawned = Vec::with_capacity(pieces.len());
    for (k, (mesh, center, scale, shape, volume)) in
        pieces.into_iter().enumerate()
    {
        let Some((mesh, material)) =
            world.get_resource_mut::<AssetServer>().map(|mut assets| {
                // Its own mesh, so removing the debris frees only that.
                let mesh = mesh.or_else(|| {
                    assets.meshes.get(assets.fallback_mesh).cloned()
                });
                (
                    assets.meshes.insert(mesh.unwrap_or_default()),
                    assets.fallback_material,
                )
            })
        else {
            return;
        };
        let order = next_spawn_order(world);
        let group = *group.get_or_insert(order.0);
        let mut piece = world.spawn((
            Transform {
                position: (position + turn * center).into(),
                rotation: transform.rotation,
                scale: scale.into(),
            },
            MeshRenderer {
                mesh,
                material,
                cast_shadows: true,
                receive_shadows: true,
            },
            Collider {
                shape,
                ..Collider::default()
            },
            PhysicsBody {
                simulation: SimulationClass::Gpu,
                ..PhysicsBody::default()
            },
            // Reads state back so the budget can tell when the slab sleeps.
            PhysicsSyncMode::SelectedState,
            RigidBody {
                kind: RigidBodyKind::Dynamic,
                mass: fracture.density * volume,
                linear_velocity: (up * throw * (0.5 + 0.5 * draw(k as u64)))
                    .into(),
                ..RigidBody::default()
            },
            order,
            Debris {
                group,
                lifetime: fracture.debris_lifetime,
                merge: false,
                asleep_since: None,
            },
        ));
        if let Some(layers) = layers {
            piece.insert(layers);
        }
        spawned.push(piece.id());
    }
    let mut bounds = [[f32::INFINITY; 3], [f32::NEG_INFINITY; 3]];
    for corner in 0..8 {
        let sign = |bit: u32| if corner & bit == 0 { -1.0 } else { 1.0 };
        let p = position
            + turn
                * V::new(sign(1) * half.x, sign(2) * half.y, sign(4) * half.z);
        for axis in 0..3 {
            bounds[0][axis] = bounds[0][axis].min(p[axis]);
            bounds[1][axis] = bounds[1][axis].max(p[axis]);
        }
    }
    if let Some(mut events) = world.get_resource_mut::<EventQueue<Fractured>>()
    {
        events.send(Fractured {
            entity: terrain,
            name,
            pieces: spawned,
            point: impact.point.into(),
            energy: impact.energy,
            mass,
            bounds,
        });
    }
}

/// Per fixed step, after [`manage_debris`]: [`Debris`] asleep for a
/// terrain's [`TerrainFracture::refreeze`] seconds, with its bottom within
/// one sample spacing of the ground under it, is despawned and its top
/// stamped into the heightfield. Every sample nearest the piece's footprint
/// rises to the piece's top, so the ground restores the piece's collision
/// (a little more volume than the piece had, never less). Pieces go in
/// [`SpawnOrder`], terrains in `SceneId` then `SpawnOrder` order, and a
/// piece freezes into the first terrain that takes it.
pub(super) fn refreeze_debris(world: &mut World) {
    let (tick, step) =
        world.get_resource::<FrameTime>().map_or((0, 0.0), |time| {
            (time.fixed_tick, time.fixed_delta.as_secs_f32())
        });
    let mut terrains: Vec<_> = world
        .query::<(
            Entity,
            &TerrainFracture,
            &Heightfield,
            &Transform,
            Option<&SceneId>,
            Option<&SpawnOrder>,
        )>()
        .iter(world)
        .filter(|(_, fracture, ..)| fracture.refreeze > 0.0)
        .map(|(entity, fracture, field, transform, scene, order)| {
            let key = (scene.map(|s| s.0), order.map(|o| o.0));
            (key, entity, fracture.refreeze, field.clone(), *transform)
        })
        .collect();
    if terrains.is_empty() {
        return;
    }
    terrains.sort_by(|a, b| (&a.0, a.1.to_bits()).cmp(&(&b.0, b.1.to_bits())));
    let mut pieces: Vec<_> = world
        .query::<(Entity, &Debris, Option<&SpawnOrder>)>()
        .iter(world)
        .filter_map(|(entity, debris, order)| {
            let asleep =
                tick.saturating_sub(debris.asleep_since?) as f32 * step;
            Some((order.map(|o| o.0), entity, asleep))
        })
        .collect();
    pieces
        .sort_unstable_by_key(|&(order, entity, ..)| (order, entity.to_bits()));
    let mut changed = vec![None::<[[f32; 3]; 2]>; terrains.len()];
    for (_, entity, asleep) in pieces {
        let Some(points) = world_points(world, entity) else {
            continue;
        };
        if points.is_empty() {
            continue;
        }
        for (index, (_, _, refreeze, field, transform)) in
            terrains.iter_mut().enumerate()
        {
            if asleep < *refreeze || !stamp(field, transform, &points) {
                continue;
            }
            let bounds = changed[index]
                .get_or_insert([[f32::INFINITY; 3], [f32::NEG_INFINITY; 3]]);
            for p in &points {
                for axis in 0..3 {
                    bounds[0][axis] = bounds[0][axis].min(p[axis]);
                    bounds[1][axis] = bounds[1][axis].max(p[axis]);
                }
            }
            remove_debris(world, entity);
            break;
        }
    }
    for ((_, terrain, _, field, _), bounds) in terrains.into_iter().zip(changed)
    {
        let Some(bounds) = bounds else {
            continue;
        };
        if let Some(mut heights) = world.get_mut::<Heightfield>(terrain) {
            *heights = field;
        }
        if let Some(mut events) =
            world.get_resource_mut::<EventQueue<Refrozen>>()
        {
            events.send(Refrozen { terrain, bounds });
        }
    }
}

/// Where `entity` is: a GPU body's own `Transform` is its spawn pose, so
/// its read-back [`GpuStateMirror`] wins.
fn pose(world: &World, entity: Entity) -> Option<Transform> {
    world
        .get::<GpuStateMirror>(entity)
        .map(|mirror| mirror.transform)
        .or_else(|| world.get::<Transform>(entity).copied())
}

/// The world-space vertices of `entity`'s mesh at its [`pose`]; `None`
/// without a mesh or with an empty one.
fn world_points(world: &World, entity: Entity) -> Option<Vec<V>> {
    let pose = pose(world, entity)?;
    let renderer = world.get::<MeshRenderer>(entity)?;
    let mesh = world
        .get_resource::<AssetServer>()?
        .meshes
        .get(renderer.mesh)?;
    let [roll, pitch, yaw] = pose.rotation;
    let turn = sim_math::rotation_from_euler(roll, pitch, yaw);
    let points: Vec<_> = mesh
        .vertices
        .iter()
        .map(|v| {
            V::from(pose.position)
                + turn * V::from(v.position).component_mul(&V::from(pose.scale))
        })
        .collect();
    (!points.is_empty()).then_some(points)
}

/// Raises the samples nearest the footprint of `points` (world space) to
/// their top, if their bottom rests within one spacing of that ground.
fn stamp(field: &mut Heightfield, transform: &Transform, points: &[V]) -> bool {
    let spacing = field.spacing;
    let (columns, rows) = (cells_x(field), cells_z(field));
    if spacing <= 0.0 || columns == 0 || rows == 0 {
        return false;
    }
    let [roll, pitch, yaw] = transform.rotation;
    let back = sim_math::rotation_from_euler(roll, pitch, yaw).inverse();
    let mut low = V::repeat(f32::INFINITY);
    let mut high = V::repeat(f32::NEG_INFINITY);
    for p in points {
        let local = (back * (p - V::from(transform.position)))
            .component_div(&V::from(transform.scale));
        low = low.inf(&local);
        high = high.sup(&local);
    }
    // Nearest sample index along an axis of `cells` cells.
    let index =
        |x: f32, cells: usize| (x / spacing + 0.5 * cells as f32).round();
    let range = |low: f32, high: f32, cells: usize| {
        let first = index(low, cells).max(0.0);
        let last = index(high, cells).min(cells as f32);
        (first <= last).then_some(first as usize..=last as usize)
    };
    let (Some(xs), Some(zs)) =
        (range(low.x, high.x, columns), range(low.z, high.z, rows))
    else {
        return false;
    };
    let height = |row: &Vec<f32>, x: usize| row.get(x).copied().unwrap_or(0.0);
    let ground = zs
        .clone()
        .flat_map(|z| xs.clone().map(move |x| (z, x)))
        .map(|(z, x)| height(&field.heights[z], x))
        .fold(f32::NEG_INFINITY, f32::max);
    if low.y > ground + spacing {
        return false;
    }
    for z in zs {
        let row = &mut field.heights[z];
        if row.len() <= *xs.end() {
            row.resize(xs.end() + 1, 0.0);
        }
        for h in &mut row[xs.clone()] {
            *h = h.max(high.y);
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy_ecs::prelude::With;

    fn box_mesh(half: [f32; 3]) -> MeshAsset {
        piece_mesh(&box_cell(-V::from(half), V::from(half)))
    }

    fn volume(mesh: &MeshAsset) -> f32 {
        mesh.indices
            .chunks_exact(3)
            .map(|t| {
                let [a, b, c] = [0, 1, 2]
                    .map(|k| V::from(mesh.vertices[t[k] as usize].position));
                a.dot(&b.cross(&c)) / 6.0
            })
            .sum()
    }

    fn connected(count: usize, links: &[[u32; 2]]) -> bool {
        let mut reached = vec![false; count];
        let mut stack = vec![0];
        reached[0] = true;
        while let Some(at) = stack.pop() {
            for &[a, b] in links {
                let (a, b) = (a as usize, b as usize);
                for (from, to) in [(a, b), (b, a)] {
                    if from == at && !reached[to] {
                        reached[to] = true;
                        stack.push(to);
                    }
                }
            }
        }
        reached.iter().all(|&r| r)
    }

    #[test]
    fn two_slices_quarter_a_box_and_link_only_face_neighbours() {
        let fracture = Fracture {
            pieces: 0,
            slices: vec![[1.0, 0.0, 0.0, 0.0], [0.0, 2.0, 0.0, 0.0]],
            ..Fracture::default()
        };
        let broken = fracture.fracture(&box_mesh([1.0; 3]));
        assert_eq!(broken.pieces.len(), 4);
        for piece in &broken.pieces {
            assert!((volume(piece) - 2.0).abs() < 1e-4, "{}", volume(piece));
        }
        // Order: x halves first, each split in y. Diagonal quarters only
        // share an edge.
        assert_eq!(broken.links, vec![[0, 1], [0, 2], [1, 3], [2, 3]]);
    }

    #[test]
    fn voronoi_cells_fill_a_prism_and_form_one_connected_graph() {
        // A triangular prism, so the hull clipping matters.
        let prism = crate::runtime::Polygon2d::default().mesh();
        let fracture = Fracture {
            pieces: 12,
            seed: 7,
            slices: vec![[0.0, 0.0, 1.0, 0.1]],
            ..Fracture::default()
        };
        let broken = fracture.fracture(&prism);
        assert!(broken.pieces.len() > 12, "{}", broken.pieces.len());
        let total: f32 = broken.pieces.iter().map(volume).sum();
        assert!((total - 0.5).abs() < 1e-3, "pieces fill {total} m³ of 0.5");
        for piece in &broken.pieces {
            assert!(volume(piece) > 0.0);
        }
        assert!(connected(broken.pieces.len(), &broken.links));
        // A cell touches a handful of neighbours, not every other cell.
        assert!(broken.links.len() < 6 * broken.pieces.len());
    }

    #[test]
    fn the_seed_alone_picks_the_cells() {
        let mesh = box_mesh([1.0, 0.5, 0.25]);
        let at = |seed| {
            Fracture {
                pieces: 10,
                seed,
                ..Fracture::default()
            }
            .fracture(&mesh)
        };
        assert_eq!(at(3), at(3));
        assert_ne!(at(3), at(4));
    }

    /// A fixed 10 m ground slab, top at y = 0.
    fn ground() -> crate::runtime::App {
        let mut app = crate::runtime::App::new();
        app.add_plugin(crate::assets::AssetPlugin).unwrap();
        app.world_mut().spawn((
            Transform::new([0.0, -0.5, 0.0]),
            PhysicsBody::default(),
            RigidBody {
                kind: RigidBodyKind::Fixed,
                ..RigidBody::default()
            },
            Collider {
                shape: ColliderShape::Box {
                    half_extents: [5.0, 0.5, 5.0],
                },
                ..Collider::default()
            },
        ));
        app
    }

    /// A 10 kg crate breaking into 6 pieces above 100 J, its bottom
    /// `height` above the ground at `x`.
    fn add_crate(
        app: &mut crate::runtime::App,
        x: f32,
        height: f32,
        fracture: Fracture,
    ) -> Entity {
        let world = app.world_mut();
        let order = next_spawn_order(world);
        let (mesh, material) = {
            let mut assets = world.resource_mut::<AssetServer>();
            let material = assets.fallback_material;
            (assets.meshes.insert(box_mesh([0.5; 3])), material)
        };
        world
            .spawn((
                Transform::new([x, 0.5 + height, 0.0]),
                PhysicsBody::default(),
                RigidBody {
                    mass: 10.0,
                    ..RigidBody::default()
                },
                Collider {
                    shape: ColliderShape::ConvexMesh,
                    ..Collider::default()
                },
                MeshRenderer {
                    mesh,
                    material,
                    cast_shadows: true,
                    receive_shadows: true,
                },
                Fracture {
                    pieces: 6,
                    break_energy: 100.0,
                    ..fracture
                },
                order,
            ))
            .id()
    }

    fn run(app: &mut crate::runtime::App, ticks: u32) {
        for _ in 0..ticks {
            app.update(std::time::Duration::from_secs_f64(1.0 / 60.0))
                .unwrap();
        }
    }

    /// Positions and masses of the dynamic bodies, by x.
    fn dynamic(app: &mut crate::runtime::App) -> Vec<([f32; 3], f32)> {
        let world = app.world_mut();
        let mut bodies: Vec<_> = world
            .query::<(&Transform, &RigidBody)>()
            .iter(world)
            .filter(|(_, body)| body.kind == RigidBodyKind::Dynamic)
            .map(|(transform, body)| (transform.position, body.mass))
            .collect();
        bodies.sort_by(|a, b| a.0[0].total_cmp(&b.0[0]));
        bodies
    }

    /// Drops a 10 kg crate that breaks at 100 J from `height` metres onto
    /// fixed ground for 1 s; returns its bodies' positions and masses.
    fn drop_crate(height: f32) -> Vec<([f32; 3], f32)> {
        let mut app = ground();
        add_crate(&mut app, 0.0, height, Fracture::default());
        run(&mut app, 60);
        dynamic(&mut app)
    }

    fn debris_count(app: &mut crate::runtime::App) -> usize {
        let world = app.world_mut();
        world.query::<&Debris>().iter(world).count()
    }

    #[test]
    fn a_break_sends_one_fractured_event_and_a_soft_landing_none() {
        let mut app = ground();
        let soft = add_crate(&mut app, -2.0, 0.2, Fracture::default());
        let hard = add_crate(&mut app, 2.0, 3.0, Fracture::default());
        app.world_mut()
            .entity_mut(hard)
            .insert(Name("crate".into()));
        let mut seen = Vec::new();
        for _ in 0..60 {
            run(&mut app, 1);
            let events = app.world_mut().resource::<EventQueue<Fractured>>();
            seen.extend(events.iter().cloned());
        }
        assert_eq!(seen.len(), 1, "{seen:?}");
        let event = &seen[0];
        assert_ne!(event.entity, soft);
        assert_eq!(event.entity, hard);
        assert_eq!(event.name.as_deref(), Some("crate"));
        assert_eq!(event.mass, 10.0);
        // About 290 J from 3 m; the hit is on the ground under the crate.
        assert!(event.energy > 100.0 && event.energy < 400.0, "{event:?}");
        assert!((event.point[0] - 2.0).abs() < 0.6, "{event:?}");
        assert!(event.point[1].abs() < 0.1, "{event:?}");
        let [min, max] = event.bounds;
        for (axis, centre) in [(0, 2.0), (2, 0.0)] {
            assert!((min[axis] - (centre - 0.5)).abs() < 0.05, "{event:?}");
            assert!((max[axis] - (centre + 0.5)).abs() < 0.05, "{event:?}");
        }
        assert!(min[1].abs() < 0.1 && (max[1] - 1.0).abs() < 0.1);
        // The pieces it names are the debris now in the world.
        let world = app.world_mut();
        assert_eq!(event.pieces.len(), 6);
        assert!(event
            .pieces
            .iter()
            .all(|&e| world.get::<Debris>(e).is_some()));
        assert!(world.get_entity(hard).is_err());
    }

    #[test]
    fn breaks_do_not_depend_on_entity_ids() {
        // Unrelated entities shift every id; the pieces land the same.
        let landed = |padding: usize| {
            let mut app = ground();
            for _ in 0..padding {
                app.world_mut().spawn_empty();
            }
            add_crate(&mut app, 0.0, 3.0, Fracture::default());
            run(&mut app, 60);
            dynamic(&mut app)
        };
        let base = landed(0);
        assert_eq!(base.len(), 6);
        assert_eq!(landed(7), base);
    }

    #[test]
    fn sleeping_debris_leaves_after_its_lifetime() {
        let mut app = ground();
        add_crate(
            &mut app,
            0.0,
            3.0,
            Fracture {
                debris_lifetime: 1.0,
                ..Fracture::default()
            },
        );
        let meshes = app.world_mut().resource::<AssetServer>().meshes.len();
        run(&mut app, 60);
        assert_eq!(debris_count(&mut app), 6);
        run(&mut app, 600);
        assert_eq!(debris_count(&mut app), 0);
        assert!(dynamic(&mut app).is_empty());
        // Every piece mesh was freed with its body.
        let world = app.world_mut();
        assert_eq!(world.resource::<AssetServer>().meshes.len(), meshes);
    }

    #[test]
    fn a_settled_break_merges_into_one_static_body() {
        let mut app = ground();
        add_crate(
            &mut app,
            0.0,
            3.0,
            Fracture {
                merge_debris: true,
                ..Fracture::default()
            },
        );
        run(&mut app, 600);
        assert!(dynamic(&mut app).is_empty());
        let world = app.world_mut();
        let merged: Vec<_> = world
            .query::<(&Debris, &RigidBody, &Collider, &MeshRenderer)>()
            .iter(world)
            .map(|(_, body, collider, renderer)| (*body, *collider, *renderer))
            .collect();
        assert_eq!(merged.len(), 1);
        let (body, collider, renderer) = merged[0];
        assert_eq!(body.kind, RigidBodyKind::Fixed);
        assert_eq!(collider.shape, ColliderShape::TriangleMesh);
        assert!((body.mass - 10.0).abs() < 1e-3, "{} kg", body.mass);
        let assets = world.resource::<AssetServer>();
        let mesh = assets.meshes.get(renderer.mesh).unwrap();
        // The pieces kept their volume and stayed on the ground.
        assert!((volume(mesh) - 1.0).abs() < 1e-2, "{}", volume(mesh));
        assert!(mesh.vertices.iter().all(|v| v.position[1] > -0.1));
    }

    #[test]
    fn at_the_budget_a_break_waits_for_sleeping_debris() {
        // Two crates land on one tick, but only one break fits.
        let mut app = ground();
        app.world_mut().insert_resource(DebrisBudget(8));
        add_crate(&mut app, -2.0, 3.0, Fracture::default());
        add_crate(&mut app, 2.0, 3.0, Fracture::default());
        run(&mut app, 60);
        assert_eq!(debris_count(&mut app), 6);
        assert_eq!(dynamic(&mut app).len(), 7, "the second crate stays whole");

        // Once the pieces sleep, a later break replaces the oldest of them.
        let mut app = ground();
        app.world_mut().insert_resource(DebrisBudget(8));
        add_crate(&mut app, -2.0, 3.0, Fracture::default());
        run(&mut app, 300);
        let world = app.world_mut();
        let asleep = world
            .query::<&Debris>()
            .iter(world)
            .filter(|debris| debris.asleep_since.is_some())
            .count();
        assert_eq!(asleep, 6);
        add_crate(&mut app, 2.0, 3.0, Fracture::default());
        run(&mut app, 60);
        assert_eq!(debris_count(&mut app), 8);
        assert!(!dynamic(&mut app).iter().any(|(_, mass)| *mass == 10.0));
    }

    #[test]
    fn a_hard_landing_breaks_a_crate_and_a_soft_one_does_not() {
        // 0.2 m: about 20 J at landing.
        assert_eq!(drop_crate(0.2).len(), 1);
        // 3 m: about 290 J.
        let pieces = drop_crate(3.0);
        assert_eq!(pieces.len(), 6);
        let mass: f32 = pieces.iter().map(|(_, mass)| mass).sum();
        assert!((mass - 10.0).abs() < 1e-3, "pieces weigh {mass} kg");
        for (position, _) in &pieces {
            assert!(position.iter().all(|x| x.is_finite()));
            // Resting on or near the ground, not flung far or fallen through.
            assert!(position[1] > -0.1 && position[1] < 1.5, "{position:?}");
            assert!(position[0].hypot(position[2]) < 4.0, "{position:?}");
        }
        assert_eq!(pieces, drop_crate(3.0));
    }

    #[test]
    fn a_huge_piece_count_is_capped() {
        let broken = Fracture {
            pieces: u32::MAX,
            slices: vec![[1.0, 0.0, 0.0, 0.0]; 20],
            ..Fracture::default()
        }
        .fracture(&box_mesh([1.0; 3]));
        assert!(broken.pieces.len() <= MAX_FRACTURE_PIECES);
        assert!(Fracture::default()
            .fracture(&MeshAsset::default())
            .pieces
            .is_empty());
    }

    /// An 8 by 8 m flat heightfield at the origin, in 4-cell chunks that
    /// break at 200 J, and a 10 kg crate dropped from `height` at `x, z`.
    fn terrain_and_crate(
        padding: usize,
        x: f32,
        z: f32,
        height: f32,
    ) -> (crate::runtime::App, Entity) {
        let mut app = crate::runtime::App::new();
        app.add_plugin(crate::assets::AssetPlugin).unwrap();
        for _ in 0..padding {
            app.world_mut().spawn_empty();
        }
        let terrain = app
            .world_mut()
            .spawn((
                Transform::default(),
                PhysicsBody::default(),
                RigidBody {
                    kind: RigidBodyKind::Fixed,
                    ..RigidBody::default()
                },
                Collider {
                    shape: ColliderShape::Heightfield,
                    ..Collider::default()
                },
                Heightfield {
                    heights: vec![vec![0.0; 9]; 9],
                    ..Heightfield::default()
                },
                TerrainFracture {
                    break_energy: 200.0,
                    ..TerrainFracture::default()
                },
            ))
            .id();
        let world = app.world_mut();
        let order = next_spawn_order(world);
        world.spawn((
            Transform::new([x, 0.5 + height, z]),
            PhysicsBody::default(),
            RigidBody {
                mass: 10.0,
                ..RigidBody::default()
            },
            Collider::default(),
            order,
        ));
        (app, terrain)
    }

    #[test]
    fn a_hard_hit_digs_its_terrain_chunk_out_as_a_gpu_slab() {
        let (mut app, terrain) = terrain_and_crate(0, 2.0, 2.0, 3.0);
        // A soft landing on another chunk breaks nothing.
        let world = app.world_mut();
        world.spawn((
            Transform::new([-2.0, 0.7, -2.0]),
            PhysicsBody::default(),
            RigidBody {
                mass: 10.0,
                ..RigidBody::default()
            },
            Collider::default(),
        ));
        let mut seen = Vec::new();
        for _ in 0..60 {
            run(&mut app, 1);
            let events = app.world_mut().resource::<EventQueue<Fractured>>();
            seen.extend(events.iter().cloned());
        }
        assert_eq!(seen.len(), 1, "{seen:?}");
        let event = &seen[0];
        assert_eq!(event.entity, terrain);
        assert!(event.energy > 200.0 && event.energy < 400.0, "{event:?}");
        assert_eq!(event.mass, 2000.0 * 4.0 * 0.5 * 4.0);
        assert_eq!(event.bounds, [[0.0, -0.5, 0.0], [4.0, 0.0, 4.0]]);
        let world = app.world();
        let fracture = world.get::<TerrainFracture>(terrain).unwrap();
        // Chunk row 1, column 1 of a 2 by 2 grid.
        assert_eq!(fracture.broken, [3]);
        let field = world.get::<Heightfield>(terrain).unwrap();
        for (row, heights) in field.heights.iter().enumerate() {
            for (column, &height) in heights.iter().enumerate() {
                let dug = row >= 4 && column >= 4;
                assert_eq!(
                    height,
                    if dug { -0.5 } else { 0.0 },
                    "{row} {column}"
                );
            }
        }
        let slab = event.pieces[0];
        let transform = world.get::<Transform>(slab).unwrap();
        assert_eq!(transform.position, [2.0, -0.25, 2.0]);
        assert_eq!(transform.scale, [4.0, 0.5, 4.0]);
        let body = world.get::<RigidBody>(slab).unwrap();
        assert_eq!(body.kind, RigidBodyKind::Dynamic);
        assert_eq!(body.mass, event.mass);
        let [vx, vy, vz] = body.linear_velocity;
        assert!(vx == 0.0 && vy > 0.0 && vz == 0.0, "{body:?}");
        let physics = world.get::<PhysicsBody>(slab).unwrap();
        assert_eq!(physics.simulation, SimulationClass::Gpu);
        assert!(world.get::<Debris>(slab).is_some());
    }

    #[test]
    fn terrain_breaks_do_not_depend_on_entity_ids_and_hold_at_the_budget() {
        let slab = |padding: usize| {
            let (mut app, terrain) = terrain_and_crate(padding, -2.0, 2.0, 3.0);
            run(&mut app, 60);
            let world = app.world_mut();
            let broken = world
                .get::<TerrainFracture>(terrain)
                .unwrap()
                .broken
                .clone();
            let mut slabs = world
                .query_filtered::<(&Transform, &RigidBody), With<Debris>>();
            let slabs: Vec<_> = slabs
                .iter(world)
                .map(|(t, b)| (t.position, b.linear_velocity.map(f32::to_bits)))
                .collect();
            (broken, slabs)
        };
        let base = slab(0);
        // Column 0, row 1.
        assert_eq!(base.0, [2]);
        assert_eq!(base.1.len(), 1);
        assert_eq!(slab(7), base);
        // With no room for debris the chunk stays whole.
        let (mut app, terrain) = terrain_and_crate(0, -2.0, 2.0, 3.0);
        app.world_mut().insert_resource(DebrisBudget(1));
        app.world_mut().spawn((Debris {
            group: 0,
            lifetime: 0.0,
            merge: false,
            asleep_since: None,
        },));
        run(&mut app, 60);
        let world = app.world();
        assert!(world
            .get::<TerrainFracture>(terrain)
            .unwrap()
            .broken
            .is_empty());
        assert!(world
            .get::<Heightfield>(terrain)
            .unwrap()
            .heights
            .iter()
            .flatten()
            .all(|&h| h == 0.0));
    }

    #[test]
    fn gpu_debris_stopped_by_the_gpu_solver_makes_room_at_the_budget() {
        // One GPU slab fills the budget; a dig waits until it has slept.
        let dig = |velocity: [f32; 3]| {
            let (mut app, terrain) = terrain_and_crate(0, -2.0, 2.0, 3.0);
            app.world_mut().insert_resource(DebrisBudget(1));
            let old = app
                .world_mut()
                .spawn((
                    PhysicsBody {
                        simulation: SimulationClass::Gpu,
                        ..PhysicsBody::default()
                    },
                    RigidBody::default(),
                    GpuStateMirror {
                        tick: 3,
                        transform: Transform::new([9.0, 0.0, 9.0]),
                        linear_velocity: velocity,
                        angular_velocity: [0.0; 3],
                        custom_values: None,
                    },
                    Debris {
                        group: 0,
                        lifetime: 0.0,
                        merge: false,
                        asleep_since: None,
                    },
                ))
                .id();
            run(&mut app, 60);
            let world = app.world();
            let broken = world.get::<TerrainFracture>(terrain).unwrap();
            (world.get_entity(old).is_ok(), broken.broken.clone())
        };
        assert_eq!(dig([0.0; 3]), (false, vec![2]));
        assert_eq!(dig([0.0, -0.01, 0.0]), (true, vec![]));
    }

    #[test]
    fn resting_debris_refreezes_into_the_terrain_it_sleeps_on() {
        let (mut app, terrain) = terrain_and_crate(0, -2.0, 2.0, 100.0);
        app.world_mut()
            .get_mut::<TerrainFracture>(terrain)
            .unwrap()
            .refreeze = 1.0;
        let piece = |app: &mut crate::runtime::App, at: [f32; 3], gpu: bool| {
            let world = app.world_mut();
            let order = next_spawn_order(world);
            let (mesh, material) = {
                let mut assets = world.resource_mut::<AssetServer>();
                let material = assets.fallback_material;
                (assets.meshes.insert(box_mesh([0.5; 3])), material)
            };
            let mut entity = world.spawn((
                Transform::new(at),
                PhysicsBody::default(),
                RigidBody::default(),
                Collider {
                    shape: ColliderShape::ConvexMesh,
                    ..Collider::default()
                },
                MeshRenderer {
                    mesh,
                    material,
                    cast_shadows: true,
                    receive_shadows: true,
                },
                order,
                Debris {
                    group: order.0,
                    lifetime: 0.0,
                    merge: false,
                    asleep_since: None,
                },
            ));
            if gpu {
                // Asleep on the GPU at x -1, z -1, its spawn pose far above.
                entity.insert((
                    PhysicsBody {
                        simulation: SimulationClass::Gpu,
                        ..PhysicsBody::default()
                    },
                    GpuStateMirror {
                        tick: 1,
                        transform: Transform::new([-1.0, 0.5, -1.0]),
                        linear_velocity: [0.0; 3],
                        angular_velocity: [0.0; 3],
                        custom_values: None,
                    },
                ));
            }
            entity.id()
        };
        // Falls onto the terrain, sleeps, then freezes into it.
        let falling = piece(&mut app, [1.0, 1.0, 1.0], false);
        let gpu = piece(&mut app, [0.0, 50.0, 0.0], true);
        // Sleeps on nothing far above the ground, so never freezes.
        let floating = piece(&mut app, [3.0, 5.0, 3.0], false);
        app.world_mut().get_mut::<RigidBody>(floating).unwrap().kind =
            RigidBodyKind::Fixed;
        let mut frozen = Vec::new();
        for _ in 0..300 {
            run(&mut app, 1);
            let events = app.world().resource::<EventQueue<Refrozen>>();
            frozen.extend(events.iter().cloned());
        }
        let world = app.world();
        assert!(world.get_entity(falling).is_err());
        assert!(world.get_entity(gpu).is_err());
        assert!(world.get_entity(floating).is_ok());
        let heights = &world.get::<Heightfield>(terrain).unwrap().heights;
        // Sample 5 is at 1 m, sample 3 at -1 m; the pieces are 1 m tall.
        for [z, x] in [[5, 5], [3, 3]] {
            assert!((heights[z][x] - 1.0).abs() < 0.05, "{:?}", heights[z]);
        }
        // Far from both, and under the floating piece at 3 m.
        for [z, x] in [[0, 0], [8, 8], [7, 7], [0, 8]] {
            assert_eq!(heights[z][x], 0.0);
        }
        assert!(!frozen.is_empty());
        assert!(frozen.iter().all(|e| e.terrain == terrain));
    }

    #[test]
    fn small_sleeping_debris_merges_into_one_static_body_per_cell() {
        let mut app = ground();
        app.world_mut().insert_resource(DebrisAggregation {
            cell: 4.0,
            max_size: 0.5,
        });
        let piece = |app: &mut crate::runtime::App, x: f32, half: f32| {
            let world = app.world_mut();
            let order = next_spawn_order(world);
            let (mesh, material) = {
                let mut assets = world.resource_mut::<AssetServer>();
                let material = assets.fallback_material;
                (assets.meshes.insert(box_mesh([half; 3])), material)
            };
            world.spawn((
                Transform::new([x, half + 0.05, 0.5]),
                PhysicsBody::default(),
                RigidBody {
                    mass: 1.0,
                    ..RigidBody::default()
                },
                Collider {
                    shape: ColliderShape::ConvexMesh,
                    ..Collider::default()
                },
                MeshRenderer {
                    mesh,
                    material,
                    cast_shadows: true,
                    receive_shadows: true,
                },
                order,
                Debris {
                    group: order.0,
                    lifetime: 0.0,
                    merge: false,
                    asleep_since: None,
                },
            ));
        };
        // Cell x -4..0 holds two small pieces and one too big to merge;
        // cell 0..4 one small piece alone.
        piece(&mut app, -1.0, 0.2);
        piece(&mut app, -3.0, 0.2);
        piece(&mut app, -2.0, 0.5);
        piece(&mut app, 1.0, 0.2);
        run(&mut app, 300);
        let bodies = |app: &mut crate::runtime::App| {
            let world = app.world_mut();
            let mut bodies: Vec<_> = world
                .query_filtered::<(&RigidBody, &Collider), With<Debris>>()
                .iter(world)
                .map(|(body, collider)| {
                    (
                        body.kind == RigidBodyKind::Fixed,
                        collider.shape,
                        body.mass,
                    )
                })
                .collect();
            bodies.sort_by(|a, b| a.2.total_cmp(&b.2));
            bodies
        };
        let merged = (true, ColliderShape::TriangleMesh, 2.0);
        let piece_body = |mass| (false, ColliderShape::ConvexMesh, mass);
        assert_eq!(
            bodies(&mut app),
            [piece_body(1.0), piece_body(1.0), merged]
        );
        // A later piece joins the cell's aggregate.
        piece(&mut app, -0.5, 0.2);
        run(&mut app, 300);
        assert_eq!(
            bodies(&mut app),
            [
                piece_body(1.0),
                piece_body(1.0),
                (true, ColliderShape::TriangleMesh, 3.0)
            ]
        );
    }

    /// Pieces of each dig, as (offset from the dug chunk's centre, mass
    /// bits), with the crates at `xs` (z 2) and the scene seeded `seed`.
    fn cracked(
        padding: usize,
        seed: u64,
        xs: &[f32],
    ) -> Vec<Vec<([u32; 3], u32)>> {
        let (mut app, terrain) = terrain_and_crate(padding, xs[0], 2.0, 3.0);
        let world = app.world_mut();
        world.insert_resource(RandomSeed(seed));
        world.get_mut::<TerrainFracture>(terrain).unwrap().pieces = 6;
        for &x in &xs[1..] {
            let order = next_spawn_order(world);
            world.spawn((
                Transform::new([x, 3.5, 2.0]),
                PhysicsBody::default(),
                RigidBody {
                    mass: 10.0,
                    ..RigidBody::default()
                },
                Collider::default(),
                order,
            ));
        }
        let mut digs = Vec::new();
        for _ in 0..60 {
            run(&mut app, 1);
            let events = app.world_mut().resource::<EventQueue<Fractured>>();
            digs.extend(events.iter().cloned());
        }
        let world = app.world();
        digs.iter()
            .map(|dig| {
                let [low, high] = dig.bounds;
                let centre = [0, 1, 2].map(|a| 0.5 * (low[a] + high[a]));
                let mut pieces: Vec<_> = dig
                    .pieces
                    .iter()
                    .map(|&piece| {
                        let at =
                            world.get::<Transform>(piece).unwrap().position;
                        let body = world.get::<RigidBody>(piece).unwrap();
                        assert_eq!(body.kind, RigidBodyKind::Dynamic);
                        let physics = world.get::<PhysicsBody>(piece).unwrap();
                        assert_eq!(physics.simulation, SimulationClass::Gpu);
                        let shape = world.get::<Collider>(piece).unwrap().shape;
                        assert_eq!(shape, ColliderShape::ConvexMesh);
                        (
                            [0, 1, 2].map(|a| (at[a] - centre[a]).to_bits()),
                            body.mass.to_bits(),
                        )
                    })
                    .collect();
                pieces.sort_unstable();
                let mass: f32 =
                    pieces.iter().map(|&(_, m)| f32::from_bits(m)).sum();
                assert!(
                    (mass - dig.mass).abs() < 1e-3 * dig.mass,
                    "{mass} {dig:?}"
                );
                pieces
            })
            .collect()
    }

    #[test]
    fn dug_slabs_crack_in_seeded_patterns_that_differ_per_chunk() {
        let base = cracked(0, 1, &[-2.0, 2.0]);
        assert_eq!(base.len(), 2);
        assert!(base.iter().all(|dig| dig.len() == 6), "{base:?}");
        // Two chunks dug in the same tick crack differently.
        assert_ne!(base[0], base[1]);
        // Unrelated entities change nothing; another scene seed does.
        assert_eq!(cracked(7, 1, &[-2.0, 2.0]), base);
        assert_ne!(cracked(0, 2, &[-2.0, 2.0]), base);
    }
}
