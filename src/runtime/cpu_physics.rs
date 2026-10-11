//! Engine-owned CPU physics for `SimulationClass::Cpu` bodies.
//!
//! Every `FixedUpdate` step, [`step_cpu_physics`] gathers CPU bodies and
//! static colliders into the [`PhysicsWorld`] resource, integrates dynamic
//! and kinematic bodies, resolves contacts with sequential impulses, and
//! writes the result back to `Transform` and `RigidBody`. The ECS stays the
//! newest state for these bodies, so gameplay reads it on the same tick.
//! Queries ([`PhysicsWorld::raycast`], [`PhysicsWorld::overlap_sphere`])
//! answer immediately against the poses of the last step.
//!
//! Colliders scale with the entity's `Transform::scale`. Sensors report a
//! [`CollisionEvent`] but never push bodies apart. Two colliders interact
//! only when each one's `CollisionLayers::memberships` shares a bit with the
//! other's `filters`; a collider without `CollisionLayers` is on every layer.
//!
//! A dynamic body that stays nearly still for [`SLEEP_STEPS`] steps falls
//! asleep: it gets the [`Sleeping`] marker, its velocity is zeroed, and it
//! stops integrating until an awake moving body touches it, gameplay moves
//! it or sets its velocity, or gameplay removes the marker. Two sleeping
//! bodies (or a sleeping and a fixed one) are not tested against each
//! other, so resting sleepers send no `CollisionEvent`; they keep the
//! contacts they fell asleep with in [`PhysicsWorld::contacts`].
//!
//! A [`Joint`] ties a body to another body or to the world; see `joints.rs`.
//! Joint rows are solved with the contacts, and a moving or motor-driven
//! body wakes the other side of its joints.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, OnceLock};

use bevy_ecs::change_detection::DetectChanges;
use bevy_ecs::entity::Entity;
use bevy_ecs::prelude::{Component, Has, Resource, World};
use nalgebra::{
    Matrix3, Matrix4, Quaternion, Rotation3, UnitQuaternion, Vector3,
};

use crate::assets::{AssetServer, MeshAsset};
use crate::runtime::sim_math;
use crate::runtime::{
    AxisLock, Collider, ColliderShape, CollisionLayers, EventQueue, FrameTime,
    GlobalTransform, GpuProxyOf, MeshRenderer, Name, Parent, PhysicsBody,
    PhysicsSettings, PhysicsSolver, RigidBody, RigidBodyKind, SimulationClass,
};
use crate::Transform;

#[path = "articulation.rs"]
mod articulation;
#[path = "joints.rs"]
mod joints;
pub use articulation::Articulation;
pub use joints::{
    AxisMotion, Joint, JointAxis, JointBroken, JointKind, JointMotor,
    JointSpring,
};

/// Replaces the scene gravity for dynamic CPU bodies that overlap this
/// object's sensor collider, like a Godot `Area3D` gravity override: zero-g
/// rooms, sideways wind tunnels, or a small planet. The object needs a CPU
/// physics body with a sensor collider. Where volumes overlap, the highest
/// `priority` wins, then the first in body order. `RigidBody::gravity_scale`
/// still applies.
#[derive(
    Component,
    Clone,
    Copy,
    Debug,
    PartialEq,
    serde::Serialize,
    serde::Deserialize,
)]
#[serde(default)]
pub struct GravityVolume {
    /// Acceleration inside the volume in m/s²; `[0, 0, 0]` is zero-g.
    pub gravity: [f32; 3],
    /// Above 0, pulls toward the volume's centre at this many m/s² in place
    /// of `gravity`, for a planet.
    pub toward_center: f32,
    pub priority: i32,
}

impl Default for GravityVolume {
    fn default() -> Self {
        Self {
            gravity: [0.0; 3],
            toward_center: 0.0,
            priority: 0,
        }
    }
}

/// A grid of ground heights for a `Heightfield` collider: `heights[row]
/// [column]`, rows along +Z and columns along +X, `spacing` metres apart and
/// centred on the entity. [`Heightfield::mesh`] gives the same surface to
/// draw.
#[derive(
    Component, Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize,
)]
#[serde(default)]
pub struct Heightfield {
    pub heights: Vec<Vec<f32>>,
    pub spacing: f32,
    /// Surface of each cell, `cells[row][column]` (one row and column fewer
    /// than `heights`), as an index into `surfaces`. A missing cell, or an
    /// index past the end, uses the collider's own friction, restitution
    /// and [`PhysicsMaterial`].
    pub cells: Vec<Vec<u8>>,
    pub surfaces: Vec<GroundSurface>,
}

impl Default for Heightfield {
    fn default() -> Self {
        Self {
            heights: vec![vec![0.0; 2]; 2],
            spacing: 1.0,
            cells: Vec::new(),
            surfaces: Vec::new(),
        }
    }
}

/// A patch of [`Heightfield`] ground (ice, mud) with its own friction,
/// restitution and material name for `SoundCue::with_material`. The
/// collider's [`PhysicsMaterial`] combine modes still apply.
#[derive(
    Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize,
)]
#[serde(default)]
pub struct GroundSurface {
    pub name: String,
    pub friction: f32,
    pub restitution: f32,
}

impl GroundSurface {
    /// Friction and restitution of each surface, clamped to their ranges.
    fn values(surfaces: &[Self]) -> Vec<[f32; 2]> {
        surfaces
            .iter()
            .map(|surface| {
                [
                    surface.friction.max(0.0),
                    surface.restitution.clamp(0.0, 1.0),
                ]
            })
            .collect()
    }
}

/// Per-triangle surfaces for a `TriangleMesh` collider on the same entity:
/// `triangles[i]` indexes `surfaces` for the mesh's triangle `i`, in index
/// buffer order. A missing entry, or an index past the end, uses the
/// collider's own friction, restitution and [`PhysicsMaterial`].
#[derive(
    Component,
    Clone,
    Debug,
    Default,
    PartialEq,
    serde::Serialize,
    serde::Deserialize,
)]
#[serde(default)]
pub struct MeshSurfaces {
    pub triangles: Vec<u8>,
    pub surfaces: Vec<GroundSurface>,
}

impl MeshSurfaces {
    fn triangle_surfaces(&self, count: usize) -> Vec<Option<u8>> {
        (0..count)
            .map(|index| {
                self.triangles.get(index).copied().filter(|&surface| {
                    usize::from(surface) < self.surfaces.len()
                })
            })
            .collect()
    }
}

impl Heightfield {
    /// Surface of each triangle of [`Self::mesh`], in its order.
    fn triangle_surfaces(&self) -> Vec<Option<u8>> {
        let rows = self.heights.len();
        let columns = self.heights.iter().map(Vec::len).max().unwrap_or(0);
        let mut surfaces = Vec::new();
        for row in 0..rows.saturating_sub(1) {
            for column in 0..columns.saturating_sub(1) {
                let surface = self
                    .cells
                    .get(row)
                    .and_then(|cells| cells.get(column))
                    .copied()
                    .filter(|&index| usize::from(index) < self.surfaces.len());
                surfaces.extend([surface; 2]);
            }
        }
        surfaces
    }

    /// Two triangles per grid cell, facing up, with smooth normals and UVs
    /// spanning the grid. Short rows count as 0 past their end.
    pub fn mesh(&self) -> crate::assets::MeshAsset {
        let rows = self.heights.len();
        let columns = self.heights.iter().map(Vec::len).max().unwrap_or(0);
        let height = |row: usize, column: usize| {
            self.heights[row.min(rows - 1)]
                .get(column.min(columns - 1))
                .copied()
                .unwrap_or(0.0)
        };
        if rows < 2 || columns < 2 {
            return crate::assets::MeshAsset::default();
        }
        let origin = [
            -0.5 * (columns - 1) as f32 * self.spacing,
            -0.5 * (rows - 1) as f32 * self.spacing,
        ];
        let mut vertices = Vec::with_capacity(rows * columns);
        for row in 0..rows {
            for column in 0..columns {
                let dx = height(row, column + 1)
                    - height(row, column.saturating_sub(1));
                let dz = height(row + 1, column)
                    - height(row.saturating_sub(1), column);
                let normal =
                    Vector3::new(-dx, 2.0 * self.spacing, -dz).normalize();
                vertices.push(crate::assets::MeshVertex {
                    position: [
                        origin[0] + column as f32 * self.spacing,
                        height(row, column),
                        origin[1] + row as f32 * self.spacing,
                    ],
                    normal: normal.into(),
                    uv: [
                        column as f32 / (columns - 1) as f32,
                        row as f32 / (rows - 1) as f32,
                    ],
                    tangent: [1.0, 0.0, 0.0, 1.0],
                    color: [1.0; 4],
                });
            }
        }
        let mut indices = Vec::with_capacity((rows - 1) * (columns - 1) * 6);
        for row in 0..rows - 1 {
            for column in 0..columns - 1 {
                let at = |r: usize, c: usize| (r * columns + c) as u32;
                let [a, b, c, d] = [
                    at(row, column),
                    at(row, column + 1),
                    at(row + 1, column),
                    at(row + 1, column + 1),
                ];
                // Counterclockwise seen from above.
                indices.extend([a, c, b, b, c, d]);
            }
        }
        crate::assets::MeshAsset { vertices, indices }
    }
}

/// A 2D outline for a `Polygon` or `Chain` collider: `points` in the
/// entity's XY plane, extruded `depth` metres along Z and centred on the
/// entity. [`Polygon2d::mesh`] gives the same shape to draw.
#[derive(
    Component, Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize,
)]
#[serde(default)]
pub struct Polygon2d {
    pub points: Vec<[f32; 2]>,
    pub depth: f32,
    /// The chain joins its last point back to the first, and the drawn
    /// mesh gets front and back caps.
    pub closed: bool,
}

impl Default for Polygon2d {
    fn default() -> Self {
        Self {
            points: vec![[-0.5, -0.5], [0.5, -0.5], [0.0, 0.5]],
            depth: 1.0,
            closed: true,
        }
    }
}

impl Polygon2d {
    /// One quad per segment, plus caps fanned from the first point when
    /// `closed` (so a closed outline should be convex). Flat normals point
    /// outward for counterclockwise points.
    pub fn mesh(&self) -> crate::assets::MeshAsset {
        self.mesh_with(self.closed)
    }

    fn mesh_with(&self, closed: bool) -> crate::assets::MeshAsset {
        let mut mesh = crate::assets::MeshAsset::default();
        let half = 0.5 * self.depth;
        let mut face = |corners: &[[f32; 3]], normal: [f32; 3]| {
            let first = mesh.vertices.len() as u32;
            mesh.vertices.extend(corners.iter().map(|&position| {
                crate::assets::MeshVertex {
                    position,
                    normal,
                    uv: [position[0], position[1]],
                    tangent: [1.0, 0.0, 0.0, 1.0],
                    color: [1.0; 4],
                }
            }));
            for k in 1..corners.len().saturating_sub(1) as u32 {
                mesh.indices.extend([first, first + k, first + k + 1]);
            }
        };
        let count = self.points.len();
        let segments = if closed && count > 2 {
            count
        } else {
            count.saturating_sub(1)
        };
        for i in 0..segments {
            let [ax, ay] = self.points[i];
            let [bx, by] = self.points[(i + 1) % count];
            let normal = Vector3::new(by - ay, ax - bx, 0.0)
                .try_normalize(1e-12)
                .unwrap_or_default();
            face(
                &[
                    [ax, ay, -half],
                    [bx, by, -half],
                    [bx, by, half],
                    [ax, ay, half],
                ],
                normal.into(),
            );
        }
        if closed && count > 2 {
            let cap = |z: f32| self.points.iter().map(move |&[x, y]| [x, y, z]);
            face(&cap(half).collect::<Vec<_>>(), [0.0, 0.0, 1.0]);
            face(&cap(-half).rev().collect::<Vec<_>>(), [0.0, 0.0, -1.0]);
        }
        mesh
    }
}

/// Shape of a [`ForceField`]'s push.
#[derive(
    Clone,
    Copy,
    Debug,
    Default,
    PartialEq,
    Eq,
    serde::Serialize,
    serde::Deserialize,
)]
pub enum FieldKind {
    /// Along `direction`.
    #[default]
    Directional,
    /// Away from the field's centre; a negative strength pulls in.
    Radial,
    /// Around `direction` through the field's centre, counterclockwise
    /// seen from where `direction` points.
    Vortex,
    /// Along `direction` in gusts that vary with time and place by
    /// `turbulence`.
    Wind,
    /// The [`ForceFieldFunctions`] entry named by `function`, scaled by
    /// `strength`. CPU bodies only.
    Custom,
}

/// What a custom [`ForceField`] function sees of one body.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FieldSample {
    /// Body centre minus the field's centre, in world axes.
    pub offset: [f32; 3],
    pub velocity: [f32; 3],
    /// Fixed time in seconds.
    pub seconds: f32,
    /// The field's `direction`.
    pub direction: [f32; 3],
}

/// A custom field's push per unit `strength`. A plain function, so it
/// keeps no state and replays the same.
pub type FieldFunction = fn(FieldSample) -> [f32; 3];

/// Named functions for [`FieldKind::Custom`] fields; register them with
/// [`crate::runtime::App::add_force_field_function`]. A field naming no
/// entry pushes nothing.
#[derive(bevy_ecs::prelude::Resource, Clone, Debug, Default)]
pub struct ForceFieldFunctions(
    pub std::collections::BTreeMap<String, FieldFunction>,
);

/// Pushes dynamic CPU bodies overlapping this sensor collider. Fields add
/// up where they overlap, on top of gravity.
#[derive(
    Component, Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize,
)]
#[serde(default)]
pub struct ForceField {
    pub kind: FieldKind,
    /// Acceleration in m/s², the same for light and heavy bodies.
    pub strength: f32,
    /// World direction of a directional field or wind; the axis of a
    /// vortex.
    pub direction: [f32; 3],
    /// Above 0, the push fades to nothing this far from the centre.
    pub falloff_distance: f32,
    /// Wind gust size as a share of `strength`, 0 for steady wind.
    pub turbulence: f32,
    /// [`ForceFieldFunctions`] entry of a `Custom` field.
    pub function: String,
}

impl Default for ForceField {
    fn default() -> Self {
        Self {
            kind: FieldKind::Directional,
            strength: 10.0,
            direction: [0.0, 1.0, 0.0],
            falloff_distance: 0.0,
            turbulence: 0.0,
            function: String::new(),
        }
    }
}

impl ForceField {
    /// Acceleration on a body at `offset` from the field's centre, moving
    /// at `velocity`, at `seconds` of fixed time.
    fn push(
        &self,
        offset: Vector3<f32>,
        velocity: Vector3<f32>,
        seconds: f32,
        functions: Option<&ForceFieldFunctions>,
    ) -> Vector3<f32> {
        let direction = Vector3::from(self.direction)
            .try_normalize(1e-6)
            .unwrap_or_default();
        let distance = offset.norm();
        let fade = if self.falloff_distance > 0.0 {
            (1.0 - distance / self.falloff_distance).max(0.0)
        } else {
            1.0
        };
        let toward = match self.kind {
            FieldKind::Directional => direction,
            FieldKind::Radial => offset.try_normalize(1e-6).unwrap_or_default(),
            FieldKind::Vortex => direction
                .cross(&offset)
                .try_normalize(1e-6)
                .unwrap_or_default(),
            FieldKind::Wind => {
                // Two slow sine gusts that drift through space; no RNG, so
                // every run sees the same wind.
                let (a, _) = sim_math::sin_cos(1.7 * seconds + 0.31 * offset.x);
                let (b, _) =
                    sim_math::sin_cos(0.6 * seconds + 0.23 * offset.z + 1.3);
                direction * (1.0 + self.turbulence * 0.5 * (a + b))
            }
            FieldKind::Custom => functions
                .and_then(|found| found.0.get(&self.function))
                .map_or_else(Vector3::zeros, |function| {
                    function(FieldSample {
                        offset: offset.into(),
                        velocity: velocity.into(),
                        seconds,
                        direction: self.direction,
                    })
                    .into()
                }),
        };
        toward * self.strength * fade
    }
}

/// Zeroes the locked components of `vector`.
fn clear_locked(mask: [bool; 3], vector: &mut Vector3<f32>) {
    for (value, locked) in vector.iter_mut().zip(mask) {
        if locked {
            *value = 0.0;
        }
    }
}

/// How two touching colliders' friction or restitution become the pair's.
/// Where the two sides differ, the later variant in this list wins.
#[derive(
    Clone,
    Copy,
    Debug,
    Default,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    serde::Serialize,
    serde::Deserialize,
)]
pub enum CombineMode {
    /// Friction: the geometric mean. Restitution: the larger.
    #[default]
    Default,
    Average,
    Min,
    Multiply,
    Max,
}

impl CombineMode {
    fn combine(
        self,
        other: Self,
        a: f32,
        b: f32,
        default: fn(f32, f32) -> f32,
    ) -> f32 {
        match self.max(other) {
            Self::Default => default(a, b),
            Self::Average => 0.5 * (a + b),
            Self::Min => a.min(b),
            Self::Multiply => a * b,
            Self::Max => a.max(b),
        }
    }
}

/// A named surface on a CPU collider: how its `Collider::friction` and
/// `restitution` combine with the other side's, and a `name` that sound
/// cues can match (`SoundCue::with_material`), such as "metal" or "ice".
#[derive(
    Component,
    Clone,
    Debug,
    Default,
    PartialEq,
    serde::Serialize,
    serde::Deserialize,
)]
#[serde(default)]
pub struct PhysicsMaterial {
    pub name: String,
    pub friction_combine: CombineMode,
    pub restitution_combine: CombineMode,
}

/// Fired once per touching pair of CPU colliders in each `FixedUpdate` step.
/// `sensor` is true when either collider is a sensor.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CollisionEvent {
    pub a: Entity,
    pub b: Entity,
    pub sensor: bool,
}

/// One touching pair found by the last step. `normal` points from `a` to
/// `b`; `depth` is how far they overlap along it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Contact {
    pub a: Entity,
    pub b: Entity,
    pub normal: [f32; 3],
    pub depth: f32,
    pub point: [f32; 3],
    pub sensor: bool,
    /// How fast the bodies were closing along `normal` before the solve,
    /// in m/s; 0 when they were not.
    pub speed: f32,
    /// Index into the touched [`Heightfield::surfaces`] where that cell has
    /// one; `None` otherwise.
    pub surface: Option<u8>,
}

/// First collider a ray reaches.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RayHit {
    pub entity: Entity,
    pub distance: f32,
    pub point: [f32; 3],
    pub normal: [f32; 3],
}

/// Result of [`PhysicsWorld::move_character`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CharacterMove {
    pub position: [f32; 3],
    /// True when the move touched a surface facing up (a floor).
    pub grounded: bool,
    /// The body of the last floor touched, for riding moving platforms.
    pub floor: Option<Entity>,
    /// True when the move touched a surface facing down (a ceiling).
    pub ceiling: bool,
    /// The last wall the move ran into (neither floor nor ceiling) and the
    /// wall's normal, so a walking body can push it.
    pub wall: Option<(Entity, [f32; 3])>,
}

/// Where an object is in the world: the propagated pose for a child, the
/// local position otherwise (a root's own position is never a frame stale).
pub(crate) fn world_position(
    transform: &Transform,
    parent: Option<&Parent>,
    global: Option<&GlobalTransform>,
) -> [f32; 3] {
    match (parent, global) {
        (Some(_), Some(global)) => {
            let column = global.matrix[3];
            [column[0], column[1], column[2]]
        }
        _ => transform.position,
    }
}

/// Where an object is turned in the world, as quaternion `[x, y, z, w]`:
/// the propagated pose for a child, the local rotation otherwise.
pub(crate) fn world_rotation(
    transform: &Transform,
    parent: Option<&Parent>,
    global: Option<&GlobalTransform>,
) -> [f32; 4] {
    let rotation = match (parent, global) {
        (Some(_), Some(global)) => {
            let m = global.matrix;
            let column =
                |i: usize| Vector3::new(m[i][0], m[i][1], m[i][2]).normalize();
            Rotation3::from_matrix_unchecked(Matrix3::from_columns(&[
                column(0),
                column(1),
                column(2),
            ]))
        }
        _ => sim_math::rotation_from_euler(
            transform.rotation[0],
            transform.rotation[1],
            transform.rotation[2],
        ),
    };
    UnitQuaternion::from_rotation_matrix(&rotation)
        .coords
        .into()
}

/// The identity of [`world_rotation`].
pub(crate) const NO_ROTATION: [f32; 4] = [0.0, 0.0, 0.0, 1.0];

impl CharacterMove {
    /// Moves a character standing on `floor` (that body and its position
    /// and [`world_rotation`] after the previous step) the way the floor
    /// has moved since, so it rides moving and turning platforms. Returns
    /// the new position and how far the floor turned around +Y, in
    /// radians, for the rider to turn its heading by. A separate move keeps
    /// the character's own downward probe short enough to stay grounded.
    pub(crate) fn ride(
        physics: &PhysicsWorld,
        shape: ColliderShape,
        position: [f32; 3],
        floor: Option<(Entity, [f32; 3], [f32; 4])>,
        layer_mask: u32,
        rider: Entity,
        pose_of: impl Fn(Entity) -> Option<([f32; 3], [f32; 4])>,
    ) -> ([f32; 3], f32) {
        let Some((floor, was, was_turn)) = floor else {
            return (position, 0.0);
        };
        let Some((now, now_turn)) = pose_of(floor) else {
            return (position, 0.0);
        };
        let (carried, yaw) = if now_turn == was_turn {
            (std::array::from_fn(|axis| now[axis] - was[axis]), 0.0)
        } else {
            let quaternion = |[x, y, z, w]: [f32; 4]| {
                UnitQuaternion::new_unchecked(Quaternion::new(w, x, y, z))
            };
            let turn = quaternion(now_turn) * quaternion(was_turn).inverse();
            // The rider's offset from the floor turns with the floor.
            let offset = turn
                * Vector3::from(std::array::from_fn::<f32, 3, _>(|axis| {
                    position[axis] - was[axis]
                }));
            // +X turned by a yaw of `a` around +Y is (cos a, 0, -sin a).
            let x = turn * Vector3::x();
            (
                std::array::from_fn(|axis| {
                    now[axis] + offset[axis] - position[axis]
                }),
                sim_math::atan2(-x.z, x.x),
            )
        };
        if carried == [0.0; 3] {
            return (position, yaw);
        }
        let moved = physics
            .move_character(shape, position, carried, layer_mask, Some(rider))
            .position;
        (moved, yaw)
    }
}

#[derive(Clone, Debug)]
enum Shape {
    Sphere(f32),
    Box(Vector3<f32>),
    /// Segment along local Y from `-half_height` to `half_height`, plus radius.
    Capsule {
        half_height: f32,
        radius: f32,
    },
    /// Convex hull of the mesh points (the support function never needs the
    /// hull itself).
    Hull(Arc<MeshData>),
    /// Static triangle soup, collided one triangle at a time.
    Triangles(Arc<MeshData>),
    /// A dynamic body's own collider plus its collider children, moving as
    /// one body; pairs collide part by part.
    Compound(Arc<[Part]>),
}

/// One collider of a [`Shape::Compound`], placed in the body's frame.
#[derive(Clone, Debug)]
struct Part {
    offset: Vector3<f32>,
    rotation: Rotation3<f32>,
    shape: Shape,
}

/// A mesh collider in the body's local frame, already scaled.
#[derive(Debug)]
struct MeshData {
    points: Vec<Vector3<f32>>,
    /// Hull triangles face outward; triangle-mesh ones keep their winding.
    triangles: Vec<[Vector3<f32>; 3]>,
    bounding_radius: f32,
    /// Distance from the local origin to the nearest face (hulls only).
    inner_radius: f32,
    /// Largest absolute coordinate on each axis.
    half_extents: Vector3<f32>,
    /// Surface index of each triangle (heightfields only).
    surfaces: Vec<Option<u8>>,
    /// Friction and restitution of each surface.
    surface_values: Vec<[f32; 2]>,
    /// The triangles' boxes, for ray casts.
    ray_tree: RayTree,
}

impl MeshData {
    fn new(mesh: &MeshAsset, scale: [f32; 3], convex: bool) -> Self {
        let scale = Vector3::from(scale);
        let points = mesh
            .vertices
            .iter()
            .map(|vertex| Vector3::from(vertex.position).component_mul(&scale))
            .collect::<Vec<_>>();
        let indices = if mesh.indices.is_empty() {
            (0..points.len() as u32).collect()
        } else {
            mesh.indices.clone()
        };
        let center =
            points.iter().sum::<Vector3<f32>>() / points.len().max(1) as f32;
        let mut inner_radius = f32::INFINITY;
        let triangles: Vec<_> = indices
            .chunks_exact(3)
            .filter(|chunk| chunk.iter().all(|&i| (i as usize) < points.len()))
            .map(|chunk| [0, 1, 2].map(|k| points[chunk[k] as usize]))
            .map(|[a, b, c]| {
                let normal = (b - a).cross(&(c - a));
                if !convex {
                    return [a, b, c];
                }
                let outward = if normal.dot(&(a - center)) < 0.0 {
                    [a, c, b]
                } else {
                    [a, b, c]
                };
                if let Some(normal) = normal.try_normalize(1e-12) {
                    inner_radius = inner_radius.min(normal.dot(&a).abs());
                }
                outward
            })
            .collect();
        let half_extents = points
            .iter()
            .fold(Vector3::zeros(), |most: Vector3<f32>, point| {
                most.sup(&point.abs())
            });
        let ray_tree = RayTree::new(
            &triangles
                .iter()
                .map(|[a, b, c]: &[Vector3<f32>; 3]| {
                    [a.inf(&b.inf(c)), a.sup(&b.sup(c))]
                })
                .collect::<Vec<_>>(),
        );
        Self {
            ray_tree,
            bounding_radius: points
                .iter()
                .map(|point| point.norm())
                .fold(0.0, f32::max),
            inner_radius: if convex && inner_radius.is_finite() {
                inner_radius.max(1e-3)
            } else {
                0.0
            },
            points,
            triangles,
            half_extents,
            surfaces: Vec::new(),
            surface_values: Vec::new(),
        }
    }
}

/// Mesh colliders built by the last step, keyed by mesh, revision, scale
/// and kind, so unchanged meshes are not rebuilt every step.
type MeshCache = HashMap<(u64, u64, [u32; 3], bool), Arc<MeshData>>;

#[derive(Clone, Debug)]
struct Body {
    entity: Entity,
    kind: RigidBodyKind,
    /// Children are placed by their parent, so the solver never moves them.
    movable: bool,
    position: Vector3<f32>,
    rotation: Rotation3<f32>,
    shape: Shape,
    sensor: bool,
    /// Stands in for a GPU body ([`GpuProxyOf`]); GPU bodies skip it.
    proxy: bool,
    layers: CollisionLayers,
    friction: f32,
    restitution: f32,
    /// Friction and restitution [`CombineMode`]s.
    combine: [CombineMode; 2],
    inverse_mass: f32,
    /// Inverse principal moments of inertia in the body's local axes; zero
    /// for bodies the solver does not move.
    inverse_inertia: Vector3<f32>,
    asleep: bool,
    /// Moved by its articulation, never by the rigid-body integrator.
    articulated: bool,
    velocity: Vector3<f32>,
    angular_velocity: Vector3<f32>,
    locks: AxisLock,
}

impl Shape {
    /// A primitive collider scaled by `scale`; `None` for mesh colliders,
    /// which need their mesh (see [`gather_bodies`]).
    fn scaled(shape: ColliderShape, scale: [f32; 3]) -> Option<Self> {
        let scale = Vector3::from(scale).abs();
        Some(match shape {
            ColliderShape::Sphere { radius } => {
                Self::Sphere(radius * scale.max())
            }
            ColliderShape::Box { half_extents } => {
                Self::Box(Vector3::from(half_extents).component_mul(&scale))
            }
            ColliderShape::Capsule {
                half_height,
                radius,
            } => Self::Capsule {
                half_height: half_height * scale.y,
                radius: radius * scale.x.max(scale.z),
            },
            ColliderShape::ConvexMesh
            | ColliderShape::TriangleMesh
            | ColliderShape::Heightfield
            | ColliderShape::Polygon
            | ColliderShape::Chain => {
                return None;
            }
        })
    }

    /// Inverse principal moments of a solid shape of `mass`.
    // ponytail: a capsule counts as its bounding box; close enough for
    // tumbling, exact formula if capsule spin ever looks wrong.
    // ponytail: hulls count as their bounding box too.
    fn inverse_inertia(&self, mass: f32) -> Vector3<f32> {
        let half = match *self {
            Self::Sphere(radius) => {
                return Vector3::repeat(1.0 / (0.4 * mass * radius * radius));
            }
            Self::Box(half) => half,
            Self::Hull(ref mesh) | Self::Triangles(ref mesh) => {
                mesh.half_extents
            }
            Self::Capsule {
                half_height,
                radius,
            } => Vector3::new(radius, half_height + radius, radius),
            Self::Compound(ref parts) => compound_half_extents(parts),
        };
        let squared = half.component_mul(&half);
        Vector3::new(
            squared.y + squared.z,
            squared.x + squared.z,
            squared.x + squared.y,
        )
        .map(|sum| 3.0 / (mass * sum).max(1e-9))
    }

    /// GPU encoding: x = kind (0 box, 1 sphere, 2 capsule), then the box
    /// half extents, the sphere radius, or the capsule half height and
    /// radius.
    // ponytail: GPU bodies see a mesh collider as its bounding box; send
    // triangles to the shader if GPU bodies must land on uneven meshes.
    fn gpu_words(&self) -> [f32; 4] {
        match *self {
            Self::Box(half) => [0.0, half.x, half.y, half.z],
            Self::Hull(ref mesh) | Self::Triangles(ref mesh) => {
                let half = mesh.half_extents;
                [0.0, half.x, half.y, half.z]
            }
            Self::Compound(ref parts) => {
                let half = compound_half_extents(parts);
                [0.0, half.x, half.y, half.z]
            }
            Self::Sphere(radius) => [1.0, radius, 0.0, 0.0],
            Self::Capsule {
                half_height,
                radius,
            } => [2.0, half_height, radius, 0.0],
        }
    }

    fn bounding_radius(&self) -> f32 {
        match *self {
            Self::Sphere(radius) => radius,
            Self::Hull(ref mesh) | Self::Triangles(ref mesh) => {
                mesh.bounding_radius
            }
            Self::Box(half) => half.norm(),
            Self::Capsule {
                half_height,
                radius,
            } => half_height + radius,
            Self::Compound(ref parts) => parts
                .iter()
                .map(|part| part.offset.norm() + part.shape.bounding_radius())
                .fold(0.0, f32::max),
        }
    }
}

/// Half extents of a box around every part's bounding sphere.
fn compound_half_extents(parts: &[Part]) -> Vector3<f32> {
    parts.iter().fold(Vector3::zeros(), |half, part| {
        half.sup(&part.offset.abs().add_scalar(part.shape.bounding_radius()))
    })
}

/// World-space inverse inertia of a solid `collider` of `mass`, the same
/// the solver uses. A mesh collider counts as its unscaled-cube box.
pub(super) fn world_inverse_inertia(
    collider: &Collider,
    scale: [f32; 3],
    rotation: &Rotation3<f32>,
    mass: f32,
) -> Matrix3<f32> {
    let shape = Shape::scaled(collider.shape, scale)
        .unwrap_or(Shape::Box(Vector3::from(scale).abs() * 0.5));
    let rotation = rotation.matrix();
    rotation
        * Matrix3::from_diagonal(&shape.inverse_inertia(mass))
        * rotation.transpose()
}

/// [`GpuCollider::shape`] kind of a GPU body that has no collider.
pub(crate) const GPU_NO_SHAPE: [f32; 4] = [3.0, 0.0, 0.0, 0.0];

/// A GPU body's collider scaled by its transform, in the encoding of
/// [`GpuCollider::shape`], or [`GPU_NO_SHAPE`].
pub(crate) fn gpu_shape_words(
    collider: Option<&Collider>,
    scale: [f32; 3],
) -> [f32; 4] {
    // ponytail: a GPU body's own mesh is not at hand here, so a mesh
    // collider on a GPU body is its unit cube scaled by the transform.
    collider.map_or(GPU_NO_SHAPE, |collider| {
        Shape::scaled(collider.shape, scale)
            .unwrap_or(Shape::Box(Vector3::from(scale).abs() * 0.5))
            .gpu_words()
    })
}

/// A solid CPU or static collider that GPU bodies collide against, as of
/// the last CPU step.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GpuCollider {
    /// Rotation and position, without scale (scale is in `shape`).
    pub model: [[f32; 4]; 4],
    /// x = kind (0 box, 1 sphere, 2 capsule along local Y); then the box
    /// half extents, the sphere radius, or the capsule half height and
    /// radius. Already scaled.
    pub shape: [f32; 4],
    /// Linear velocity of a kinematic or dynamic CPU body.
    pub velocity: [f32; 3],
    pub friction: f32,
    pub restitution: f32,
    pub layers: CollisionLayers,
}

/// A non-`Custom` [`ForceField`] sensor that pushes GPU bodies whose centre comes
/// within their bounding radius of its collider, as of the last CPU step.
#[derive(Clone, Debug, PartialEq)]
pub struct GpuForceField {
    /// Rotation and position, like [`GpuCollider::model`].
    pub model: [[f32; 4]; 4],
    /// The sensor's shape, encoded like [`GpuCollider::shape`].
    pub shape: [f32; 4],
    pub field: ForceField,
}

impl Body {
    fn gpu_model(&self) -> [[f32; 4]; 4] {
        let mut model = self.rotation.to_homogeneous();
        model.fixed_view_mut::<3, 1>(0, 3).copy_from(&self.position);
        model.into()
    }

    /// Query-only sensor body.
    fn probe(shape: Shape, position: Vector3<f32>) -> Self {
        Self {
            entity: Entity::PLACEHOLDER,
            kind: RigidBodyKind::Fixed,
            movable: false,
            position,
            rotation: Rotation3::identity(),
            shape,
            sensor: true,
            proxy: false,
            layers: CollisionLayers::default(),
            friction: 0.0,
            restitution: 0.0,
            combine: [CombineMode::Default; 2],
            inverse_mass: 0.0,
            inverse_inertia: Vector3::zeros(),
            asleep: false,
            articulated: false,
            locks: AxisLock::default(),
            velocity: Vector3::zeros(),
            angular_velocity: Vector3::zeros(),
        }
    }

    fn world_inverse_inertia(&self) -> Matrix3<f32> {
        let rotation = self.rotation.matrix();
        let mut inertia = rotation
            * Matrix3::from_diagonal(&self.inverse_inertia)
            * rotation.transpose();
        // A locked axis takes no turn from any torque.
        for (axis, locked) in self.locks.angular.into_iter().enumerate() {
            if locked {
                inertia.row_mut(axis).fill(0.0);
                inertia.column_mut(axis).fill(0.0);
            }
        }
        inertia
    }

    fn segment(&self) -> (Vector3<f32>, Vector3<f32>, f32) {
        let &Shape::Capsule {
            half_height,
            radius,
        } = &self.shape
        else {
            unreachable!("only capsules have a segment");
        };
        let axis = self.rotation * Vector3::y() * half_height;
        (self.position - axis, self.position + axis, radius)
    }

    /// Radius of the largest sphere around the center inside the shape.
    fn inner_radius(&self) -> f32 {
        match self.shape {
            Shape::Sphere(radius) | Shape::Capsule { radius, .. } => radius,
            Shape::Box(half) => half.min(),
            Shape::Hull(ref mesh) | Shape::Triangles(ref mesh) => {
                mesh.inner_radius
            }
            Shape::Compound(_) => self
                .parts()
                .iter()
                .map(Self::inner_radius)
                .fold(f32::INFINITY, f32::min),
        }
    }

    /// Each part of a compound as a body of its own (same entity and
    /// motion); any other body as itself.
    fn parts(&self) -> Vec<Self> {
        let Shape::Compound(ref parts) = self.shape else {
            return vec![self.clone()];
        };
        parts
            .iter()
            .map(|part| Self {
                position: self.position + self.rotation * part.offset,
                rotation: self.rotation * part.rotation,
                shape: part.shape.clone(),
                ..self.clone()
            })
            .collect()
    }

    /// Radius around the core shape: a sphere is a rounded point and a
    /// capsule a rounded segment; the rest have no rounding.
    fn core_radius(&self) -> f32 {
        match self.shape {
            Shape::Sphere(radius) | Shape::Capsule { radius, .. } => radius,
            _ => 0.0,
        }
    }

    /// Farthest point of the core shape along `direction`, in world space.
    fn support(&self, direction: Vector3<f32>) -> Vector3<f32> {
        let local = self.rotation.inverse() * direction;
        let farthest = |points: &[Vector3<f32>]| {
            points
                .iter()
                .copied()
                .max_by(|a, b| a.dot(&local).total_cmp(&b.dot(&local)))
                .unwrap_or_default()
        };
        let point = match self.shape {
            Shape::Sphere(_) => Vector3::zeros(),
            Shape::Capsule { half_height, .. } => {
                Vector3::y() * half_height.copysign(local.y)
            }
            Shape::Box(half) => local.zip_map(&half, |d, h| h.copysign(d)),
            Shape::Hull(ref mesh) | Shape::Triangles(ref mesh) => {
                farthest(&mesh.points)
            }
            Shape::Compound(_) => {
                unreachable!("compound pairs collide part by part")
            }
        };
        self.position + self.rotation * point
    }

    /// Corners of a box or points of a hull in world space; empty for
    /// rounded shapes and triangle meshes.
    fn vertices(&self) -> Vec<Vector3<f32>> {
        let local = match self.shape {
            Shape::Box(half) => (0..8)
                .map(|corner| {
                    Vector3::from_fn(|axis, _| {
                        if corner >> axis & 1 == 1 {
                            half[axis]
                        } else {
                            -half[axis]
                        }
                    })
                })
                .collect(),
            Shape::Hull(ref mesh) => mesh.points.clone(),
            _ => Vec::new(),
        };
        local
            .into_iter()
            .map(|point| self.position + self.rotation * point)
            .collect()
    }

    fn interacts_with(&self, other: &Self) -> bool {
        self.layers.memberships & other.layers.filters != 0
            && other.layers.memberships & self.layers.filters != 0
    }

    fn bounding_radius(&self) -> f32 {
        self.shape.bounding_radius()
    }
}

/// Marks a CPU body that fell asleep; see the module docs. Remove it to
/// wake the body.
#[derive(Component, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Sleeping;

/// Order in which the CPU solver visits a body. Scene loading numbers the
/// scene's objects by `SceneId`, and game spawns continue the count, so a
/// reloaded scene solves in the same order as the first load did. Bodies
/// without one follow, in entity order.
#[derive(Component, Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct SpawnOrder(pub u64);

/// The next [`SpawnOrder`] to hand out. Replacing the scene resets it.
#[derive(Resource, Clone, Copy, Debug, Default)]
pub struct NextSpawnOrder(pub u64);

/// Takes the next [`SpawnOrder`] for an object spawned during play.
pub(crate) fn next_spawn_order(world: &mut World) -> SpawnOrder {
    let mut next = world.get_resource_or_insert_with(NextSpawnOrder::default);
    next.0 += 1;
    SpawnOrder(next.0 - 1)
}

/// CPU physics state after the last fixed step, with immediate queries.
#[derive(Resource, Clone, Debug, Default)]
pub struct PhysicsWorld {
    bodies: Vec<Body>,
    contacts: Vec<Contact>,
    /// Fast bodies the last step stopped at a fixed or kinematic surface.
    impacts: Vec<Contact>,
    /// Consecutive still steps of each awake dynamic body, and the position
    /// each sleeping body fell asleep at (to notice gameplay moving it).
    rest: HashMap<Entity, (u32, [f32; 3])>,
    /// Last step's impulses per contact pair, to warm-start the solver.
    warm: WarmStart,
    /// Last step's impulse of each joint row, by row key.
    joint_warm: JointWarm,
    meshes: MeshCache,
    /// The bodies' bounding boxes for ray casts, built by the first cast
    /// after the bodies change.
    ray_tree: OnceLock<RayTree>,
}

/// Contact points of each pair in the first body's local frame, with the
/// normal and two tangent impulses they ended the step with.
type WarmStart = HashMap<(Entity, Entity), Vec<(Vector3<f32>, [f32; 3])>>;
/// Impulse of each joint's rows at the end of the step, by row key.
type JointWarm = HashMap<Entity, Vec<(u8, f32)>>;
/// Local distance within which a new manifold point inherits an old impulse.
const WARM_MATCH: f32 = 0.05;

/// Still steps before a body sleeps (half a second at 60 Hz).
pub const SLEEP_STEPS: u32 = 30;
/// Speeds below which a body counts as still, in m/s and rad/s.
const SLEEP_LINEAR: f32 = 0.05;
const SLEEP_ANGULAR: f32 = 0.05;

const SOLVER_ITERATIONS: usize = 8;
/// Joint passes per solver iteration. A long lever on a light body couples
/// a joint's rows tightly, and one pass leaves its anchor drifting.
// ponytail: extra passes, not a block solve of the anchor rows; switch to
// a 3x3 anchor block if joint-heavy scenes make this cost show.
const JOINT_PASSES: usize = 4;
/// Overlap left alone so resting contacts do not jitter.
const PENETRATION_SLOP: f32 = 0.005;
/// Share of the remaining overlap removed each step.
const POSITION_CORRECTION: f32 = 0.8;
/// Share of an articulation's contact overlap removed per step by velocity.
const CONTACT_BIAS: f32 = 0.2;
/// Closing speed below which contacts do not bounce, in m/s.
const RESTITUTION_THRESHOLD: f32 = 1.0;

impl PhysicsWorld {
    /// Feeds the state carried into the next step (sleep counters and
    /// warm-start impulses) in entity order.
    pub(crate) fn hash_state(&self, hasher: &mut super::StateHasher) {
        let mut rest: Vec<_> = self.rest.iter().collect();
        rest.sort_unstable_by_key(|(entity, _)| **entity);
        for (entity, (steps, position)) in rest {
            hasher.entity(*entity);
            hasher.word(u64::from(*steps));
            hasher.floats(position);
        }
        let mut warm: Vec<_> = self.warm.iter().collect();
        warm.sort_unstable_by_key(|(pair, _)| **pair);
        for ((first, second), points) in warm {
            hasher.entity(*first);
            hasher.entity(*second);
            for (point, impulses) in points {
                hasher.floats(point.as_slice());
                hasher.floats(impulses);
            }
        }
        let mut joints: Vec<_> = self.joint_warm.iter().collect();
        joints.sort_unstable_by_key(|(entity, _)| **entity);
        for (entity, rows) in joints {
            hasher.entity(*entity);
            for (key, impulse) in rows {
                hasher.word(u64::from(*key));
                hasher.floats(&[*impulse]);
            }
        }
    }

    /// Touching pairs found by the last step, sensors included.
    #[must_use]
    pub fn contacts(&self) -> &[Contact] {
        &self.contacts
    }

    /// Fast bodies (`a`) the last step's continuous collision stopped at a
    /// fixed or kinematic collider (`b`) before they touched, with the
    /// speed they hit at. Such a hit never shows in [`Self::contacts`] with
    /// its speed, because the body arrives already stopped.
    #[must_use]
    pub fn impacts(&self) -> &[Contact] {
        &self.impacts
    }

    /// Nearest collider hit by a ray within `max_distance`, among colliders
    /// whose `CollisionLayers::memberships` share a bit with `layer_mask`
    /// (`u32::MAX` for all). Sensors are included; a ray starting inside a
    /// collider does not hit it.
    #[must_use]
    pub fn raycast(
        &self,
        origin: [f32; 3],
        direction: [f32; 3],
        max_distance: f32,
        layer_mask: u32,
    ) -> Option<RayHit> {
        self.raycast_where(origin, direction, max_distance, layer_mask, |_| {
            true
        })
    }

    /// [`Self::raycast`] that skips colliders whose entity `keep` rejects.
    #[must_use]
    pub fn raycast_where(
        &self,
        origin: [f32; 3],
        direction: [f32; 3],
        max_distance: f32,
        layer_mask: u32,
        keep: impl Fn(Entity) -> bool,
    ) -> Option<RayHit> {
        let origin = Vector3::from(origin);
        let direction = Vector3::from(direction).try_normalize(1e-6)?;
        let tree = self.ray_tree.get_or_init(|| {
            RayTree::new(
                &self
                    .bodies
                    .iter()
                    .map(|body| {
                        let reach = Vector3::repeat(body.bounding_radius());
                        [body.position - reach, body.position + reach]
                    })
                    .collect::<Vec<_>>(),
            )
        });
        let (index, distance, normal) =
            tree.cast(origin, direction, max_distance, |index| {
                let body = &self.bodies[index];
                if body.layers.memberships & layer_mask == 0 {
                    return None;
                }
                let hit = ray_body(origin, direction, body)?;
                keep(body.entity).then_some(hit)
            })?;
        Some(RayHit {
            entity: self.bodies[index].entity,
            distance,
            point: (origin + direction * distance).into(),
            normal: normal.into(),
        })
    }

    /// Replaces the bodies, dropping the ray tree built over the old ones.
    fn set_bodies(&mut self, bodies: Vec<Body>) {
        self.bodies = bodies;
        self.ray_tree = OnceLock::new();
    }

    /// Solid colliders of the last step, for GPU bodies to collide against.
    #[must_use]
    pub fn gpu_colliders(&self) -> Vec<GpuCollider> {
        self.bodies
            .iter()
            .filter(|body| !body.sensor && !body.proxy)
            .map(|body| GpuCollider {
                model: body.gpu_model(),
                shape: body.shape.gpu_words(),
                velocity: body.velocity.into(),
                friction: body.friction,
                restitution: body.restitution,
                layers: body.layers,
            })
            .collect()
    }

    /// Force-field sensors of the last step, for GPU bodies to feel.
    #[must_use]
    pub fn gpu_force_fields(&self, world: &World) -> Vec<GpuForceField> {
        self.bodies
            .iter()
            .filter(|body| body.sensor)
            .filter_map(|body| {
                let field = world.get::<ForceField>(body.entity)?;
                // ponytail: custom Rust functions cannot run in the shader.
                (field.kind != FieldKind::Custom).then(|| GpuForceField {
                    model: body.gpu_model(),
                    shape: body.shape.gpu_words(),
                    field: field.clone(),
                })
            })
            .collect()
    }

    /// Every collider on a layer in `layer_mask` that overlaps a sphere, in
    /// stable entity order.
    #[must_use]
    pub fn overlap_sphere(
        &self,
        center: [f32; 3],
        radius: f32,
        layer_mask: u32,
    ) -> Vec<Entity> {
        let probe = Body::probe(Shape::Sphere(radius), center.into());
        self.bodies
            .iter()
            .filter(|body| body.layers.memberships & layer_mask != 0)
            .filter(|body| collide(&probe, body).is_some())
            .map(|body| body.entity)
            .collect()
    }

    /// Nearest solid collider hit when `shape` (unrotated) moves from
    /// `origin` along `direction` up to `max_distance`. Uses the same layer
    /// mask as [`Self::raycast`]; sensors and `exclude` are skipped. A
    /// collider the shape already overlaps is hit at distance 0 only when
    /// the motion goes deeper into it, so a shape can always move out.
    /// `distance` is where the shape stops just before touching; `normal`
    /// points from the collider toward the shape. Only Box, Sphere and
    /// Capsule cast; shapes that read a mesh or a component give `None`.
    // ponytail: marches in steps of the shape's inner radius, then bisects;
    // a collider thinner than that gap can be skipped, and cost grows with
    // `max_distance`. Use exact swept tests if casts get long or hot.
    #[must_use]
    pub fn shape_cast(
        &self,
        shape: ColliderShape,
        origin: [f32; 3],
        direction: [f32; 3],
        max_distance: f32,
        layer_mask: u32,
        exclude: Option<Entity>,
    ) -> Option<RayHit> {
        let shape = Shape::scaled(shape, [1.0; 3])?;
        let origin = Vector3::from(origin);
        let direction = Vector3::from(direction).try_normalize(1e-6)?;
        let probe = |distance: f32| {
            Body::probe(shape.clone(), origin + direction * distance)
        };
        let reach = probe(0.0).bounding_radius();
        let step = probe(0.0).inner_radius().max(1e-3);
        self.bodies
            .iter()
            .filter(|body| body.layers.memberships & layer_mask != 0)
            .filter(|body| !body.sensor && Some(body.entity) != exclude)
            .filter(|body| {
                // Skip colliders whose bounding sphere is far from the path.
                let along = (body.position - origin)
                    .dot(&direction)
                    .clamp(0.0, max_distance);
                let gap = (origin + direction * along - body.position).norm();
                gap <= reach + body.bounding_radius()
            })
            .filter_map(|body| {
                if let Some(start) = collide(&probe(0.0), body) {
                    let normal = -Vector3::from(start.normal);
                    return (normal.dot(&direction) < 0.0).then(|| RayHit {
                        entity: body.entity,
                        distance: 0.0,
                        point: start.point,
                        normal: normal.into(),
                    });
                }
                let (mut free, mut blocked) = (0.0_f32, None);
                while free < max_distance {
                    let next = (free + step).min(max_distance);
                    if collide(&probe(next), body).is_some() {
                        blocked = Some(next);
                        break;
                    }
                    free = next;
                }
                let mut blocked = blocked?;
                for _ in 0..16 {
                    let middle = 0.5 * (free + blocked);
                    if collide(&probe(middle), body).is_some() {
                        blocked = middle;
                    } else {
                        free = middle;
                    }
                }
                let contact = collide(&probe(blocked), body)?;
                Some(RayHit {
                    entity: body.entity,
                    distance: free,
                    point: contact.point,
                    normal: (-Vector3::from(contact.normal)).into(),
                })
            })
            .min_by(|a, b| a.distance.total_cmp(&b.distance))
    }

    /// Moves an unrotated character `shape` (a box, sphere or capsule; a
    /// mesh shape does not move) from `position` by `motion`,
    /// sliding along solid colliders instead of passing through them (see
    /// [`Self::shape_cast`]). The caller writes the returned position to the
    /// character's `Transform`; pass the character's own entity as
    /// `exclude`. Uses the poses of the last fixed step.
    #[must_use]
    pub fn move_character(
        &self,
        shape: ColliderShape,
        position: [f32; 3],
        motion: [f32; 3],
        layer_mask: u32,
        exclude: Option<Entity>,
    ) -> CharacterMove {
        self.slide(shape, position, motion, layer_mask, exclude, 0.7, None)
    }

    /// Like [`Self::move_character`], for a walking character: ground
    /// steeper than `max_slope` radians does not count as ground, and a
    /// steep surface or edge lifts the body only where it touches no higher
    /// than `max_step_height` above the body's bottom, so it walks over low
    /// ledges and cannot climb anything taller.
    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub fn move_character_on_foot(
        &self,
        shape: ColliderShape,
        position: [f32; 3],
        motion: [f32; 3],
        layer_mask: u32,
        exclude: Option<Entity>,
        max_slope: f32,
        max_step_height: f32,
    ) -> CharacterMove {
        self.slide(
            shape,
            position,
            motion,
            layer_mask,
            exclude,
            max_slope.cos(),
            Some(max_step_height),
        )
    }

    /// Where a walking character on the ground at `position` lands when it
    /// snaps down at most `distance` onto a floor no steeper than
    /// `max_slope` radians: its new position and the floor, or `None` with
    /// no floor in reach. Past a ledge, the ledge's edge holds the shape
    /// while a floor below its center is in reach, so it steps down instead
    /// of falling.
    #[must_use]
    pub fn snap_to_floor(
        &self,
        shape: ColliderShape,
        position: [f32; 3],
        distance: f32,
        layer_mask: u32,
        exclude: Option<Entity>,
        max_slope: f32,
    ) -> Option<([f32; 3], Entity)> {
        let floor_y = max_slope.cos();
        let down = [0.0, -1.0, 0.0];
        let hit = self
            .shape_cast(shape, position, down, distance, layer_mask, exclude)?;
        if hit.normal[1] < floor_y {
            let reach = shape_bottom(shape) + distance;
            let below = self.raycast_where(
                position,
                down,
                reach,
                layer_mask,
                |entity| Some(entity) != exclude,
            )?;
            if below.normal[1] < floor_y {
                return None;
            }
        }
        let mut position = position;
        position[1] -= (hit.distance - 0.01).max(0.0);
        Some((position, hit.entity))
    }

    #[allow(clippy::too_many_arguments)]
    fn slide(
        &self,
        shape: ColliderShape,
        position: [f32; 3],
        motion: [f32; 3],
        layer_mask: u32,
        exclude: Option<Entity>,
        floor_y: f32,
        max_step_height: Option<f32>,
    ) -> CharacterMove {
        /// Gap kept to surfaces so the next move does not start inside them.
        const SKIN: f32 = 0.01;
        let mut position = Vector3::from(position);
        let mut remaining = Vector3::from(motion);
        let mut grounded = false;
        let mut floor = None;
        let mut ceiling = false;
        let mut wall = None;
        let bottom = shape_bottom(shape);
        if bottom == 0.0 {
            remaining = Vector3::zeros();
        }
        // A cast passes through a collider it starts inside, so a body
        // teleported or risen into a floor would fall through it. Cast down
        // from above to lift a body sunk less than its `bottom` onto the top.
        if bottom > 0.0 {
            let above = position + Vector3::y() * bottom;
            if let Some(hit) = self.shape_cast(
                shape,
                above.into(),
                [0.0, -1.0, 0.0],
                bottom,
                layer_mask,
                exclude,
            ) {
                if hit.normal[1] > floor_y {
                    position.y = above.y - hit.distance + SKIN;
                }
            }
        }
        // Each slide removes motion into one surface; three covers a corner.
        for _ in 0..4 {
            let length = remaining.norm();
            if length < 1e-6 {
                break;
            }
            let direction = remaining / length;
            let Some(hit) = self.shape_cast(
                shape,
                position.into(),
                direction.into(),
                length + SKIN,
                layer_mask,
                exclude,
            ) else {
                position += remaining;
                break;
            };
            let normal = Vector3::from(hit.normal);
            let above_bottom = hit.point[1]
                - (position.y + direction.y * hit.distance - bottom);
            // On foot, an edge low enough to step onto holds the body up
            // like ground while it climbs; a steeper surface above that
            // height must not lift it.
            let step = max_step_height.map(|step| above_bottom <= step);
            if normal.y > floor_y || (step == Some(true) && normal.y > 0.0) {
                grounded = true;
                floor = Some(hit.entity);
            }
            ceiling |= normal.y < -0.7;
            if normal.y.abs() <= floor_y {
                wall = Some((hit.entity, hit.normal));
            }
            let travel = (hit.distance - SKIN).max(0.0);
            position += direction * travel;
            remaining = direction * (length - travel);
            let rise = remaining.y;
            remaining -= normal * remaining.dot(&normal).min(0.0);
            if normal.y <= floor_y && step == Some(false) {
                remaining.y = remaining.y.min(rise.max(0.0));
            }
        }
        CharacterMove {
            position: position.into(),
            grounded,
            floor,
            ceiling,
            wall,
        }
    }
}

/// Distance from a character shape's center down to its bottom; 0 for
/// shapes a character cannot be.
fn shape_bottom(shape: ColliderShape) -> f32 {
    match Shape::scaled(shape, [1.0; 3]) {
        Some(Shape::Sphere(radius)) => radius,
        Some(Shape::Box(half)) => half.y,
        Some(Shape::Capsule {
            half_height,
            radius,
        }) => half_height + radius,
        _ => 0.0,
    }
}

/// Runs one fixed CPU physics step as [`PhysicsSettings::substeps`] equal
/// substeps; see the module docs. A pair touching in several substeps
/// sends one [`CollisionEvent`], and bodies fall asleep only on the last.
pub(super) fn step_cpu_physics(world: &mut World) {
    let dt = world.resource::<FrameTime>().fixed_delta.as_secs_f32();
    let substeps = world.resource::<PhysicsSettings>().substeps.max(1);
    let mut events = Vec::new();
    let mut impacts = Vec::new();
    for index in 0..substeps {
        let last = index + 1 == substeps;
        substep(world, dt / substeps as f32, last, &mut events, &mut impacts);
    }
    let mut seen = HashSet::new();
    let mut queue = world.resource_mut::<EventQueue<CollisionEvent>>();
    for event in events {
        if seen.insert((event.a, event.b)) {
            queue.send(event);
        }
    }
    world.resource_mut::<PhysicsWorld>().impacts = impacts;
}

fn substep(
    world: &mut World,
    dt: f32,
    last: bool,
    sent: &mut Vec<CollisionEvent>,
    all_impacts: &mut Vec<Contact>,
) {
    let settings = world.resource::<PhysicsSettings>().clone();
    let mut rest =
        std::mem::take(&mut world.resource_mut::<PhysicsWorld>().rest);
    let mut meshes =
        std::mem::take(&mut world.resource_mut::<PhysicsWorld>().meshes);
    let mut bodies = gather_bodies(world, &mut meshes);
    world.resource_mut::<PhysicsWorld>().meshes = meshes;
    let alive: HashSet<Entity> =
        bodies.iter().map(|body| body.entity).collect();
    rest.retain(|entity, _| alive.contains(entity));
    // Gameplay woke it: moved it, gave it velocity, or removed the marker.
    for body in bodies.iter_mut().filter(|body| body.asleep) {
        let position: [f32; 3] = body.position.into();
        if body.velocity != Vector3::zeros()
            || rest.get(&body.entity).is_none_or(|(_, at)| *at != position)
        {
            body.asleep = false;
        }
    }
    wake_on_lost_support(world, &mut bodies);
    let links = joints::gather_joints(world, &bodies);
    let (mut articulations, taken) =
        articulation::build(world, &mut bodies, &links);
    let mut contacts = find_contacts(&bodies);
    // GPU bodies already collide with each other on the GPU.
    contacts.retain(|(a, b, _)| {
        let both_proxies = bodies[*a].proxy && bodies[*b].proxy;
        !joints::excluded(&links, *a, *b) && !both_proxies
    });
    for (a, b, contact) in &mut contacts {
        let closing = bodies[*a].velocity - bodies[*b].velocity;
        contact.speed = closing.dot(&Vector3::from(contact.normal)).max(0.0);
    }
    // Players with `push_bodies` off stay in the contacts, so dynamic bodies
    // still rest on them, but the solver sees them at rest and they shove
    // nothing. Their own velocity is put back after the solve.
    let no_push: HashSet<Entity> = world
        .query::<(Entity, &super::PlayerController)>()
        .iter(world)
        .filter(|(_, player)| !player.push_bodies)
        .map(|(entity, _)| entity)
        .collect();
    let links: Vec<_> = links
        .into_iter()
        .zip(taken)
        .filter_map(|(link, taken)| (!taken).then_some(link))
        .collect();
    // ponytail: one pass, so a push wakes only direct neighbours this step
    // and a chain wakes over the next steps; add islands if that shows.
    for (a, b, contact) in &contacts {
        let moving = |body: &Body| {
            !body.asleep
                && body.kind != RigidBodyKind::Fixed
                && body.velocity.norm() > SLEEP_LINEAR
        };
        if contact.sensor {
            continue;
        }
        if moving(&bodies[*a]) {
            bodies[*b].asleep = false;
        }
        if moving(&bodies[*b]) {
            bodies[*a].asleep = false;
        }
    }
    // A moving or motor-driven side wakes the other side of its joint.
    for link in &links {
        let Some(a) = link.a else {
            bodies[link.b].asleep &= !joints::has_motor(&link.joint);
            continue;
        };
        let moving = |body: &Body| {
            !body.asleep
                && body.kind != RigidBodyKind::Fixed
                && body.velocity.norm() > SLEEP_LINEAR
        };
        let wake = joints::has_motor(&link.joint)
            || moving(&bodies[a])
            || moving(&bodies[link.b]);
        if wake {
            bodies[a].asleep = false;
            bodies[link.b].asleep = false;
        }
    }
    for body in &mut bodies {
        if body.asleep {
            body.inverse_mass = 0.0;
            body.inverse_inertia = Vector3::zeros();
            body.velocity = Vector3::zeros();
            body.angular_velocity = Vector3::zeros();
        } else if world.get::<Sleeping>(body.entity).is_some() {
            world.entity_mut(body.entity).remove::<Sleeping>();
            rest.remove(&body.entity);
        }
    }
    let gravity = Vector3::from(settings.gravity);
    let mut impacts = Vec::new();
    if settings.enabled {
        let gravities = body_gravity(world, &bodies, &contacts, gravity);
        let time = world.resource::<FrameTime>();
        let seconds =
            (time.fixed_tick as f64 * time.fixed_delta.as_secs_f64()) as f32;
        let pushes = field_pushes(world, &bodies, &contacts, seconds);
        for (index, body) in bodies
            .iter_mut()
            .enumerate()
            .filter(|(_, body)| body.inverse_mass > 0.0 && !body.articulated)
        {
            let scale = world
                .get::<RigidBody>(body.entity)
                .map_or(1.0, |rigid| rigid.gravity_scale);
            body.velocity += (gravities[index] * scale + pushes[index]) * dt;
        }
    }
    if settings.enabled {
        let mut physics = world.resource_mut::<PhysicsWorld>();
        let mut warm = std::mem::take(&mut physics.warm);
        let mut joint_warm = std::mem::take(&mut physics.joint_warm);
        articulations.free_motion(&mut bodies, gravity, dt);
        let locked_at: Vec<_> = bodies
            .iter()
            .enumerate()
            .filter(|(_, body)| body.locks != AxisLock::default())
            .map(|(index, body)| (index, body.position))
            .collect();
        let held: Vec<_> = bodies
            .iter_mut()
            .enumerate()
            .filter(|(_, body)| no_push.contains(&body.entity))
            .map(|(index, body)| (index, std::mem::take(&mut body.velocity)))
            .collect();
        let broken = solve_velocities(
            &mut bodies,
            &mut articulations,
            &contacts,
            &mut warm,
            &links,
            &mut joint_warm,
            dt,
        );
        for (index, velocity) in held {
            bodies[index].velocity = velocity;
        }
        for &(index, _) in &locked_at {
            let body = &mut bodies[index];
            clear_locked(body.locks.linear, &mut body.velocity);
            clear_locked(body.locks.angular, &mut body.angular_velocity);
        }
        let mut physics = world.resource_mut::<PhysicsWorld>();
        physics.warm = warm;
        physics.joint_warm = joint_warm;
        for (link, force, torque) in broken {
            let link = &links[link];
            world.entity_mut(link.entity).remove::<Joint>();
            world
                .resource_mut::<EventQueue<JointBroken>>()
                .send(JointBroken {
                    joint: link.entity,
                    target: link.joint.target,
                    force,
                    torque,
                });
        }
        impacts = sweep_fast_bodies(&mut bodies, dt);
        for body in bodies.iter_mut().filter(|body| {
            body.movable
                && body.kind != RigidBodyKind::Fixed
                && !body.asleep
                && !body.articulated
        }) {
            body.position += body.velocity * dt;
            let spin = body.angular_velocity * dt;
            if spin != Vector3::zeros() {
                body.rotation =
                    sim_math::rotation_from_scaled_axis(spin) * body.rotation;
            }
        }
        articulations.integrate(&mut bodies, dt);
        // A player that pushes nothing does not push bodies out of overlap.
        let pushed_out: Vec<_> = contacts
            .iter()
            .filter(|(a, b, _)| {
                !no_push.contains(&bodies[*a].entity)
                    && !no_push.contains(&bodies[*b].entity)
            })
            .cloned()
            .collect();
        correct_positions(&mut bodies, &pushed_out, dt);
        // Overlap push-out and CCD move along contact normals; put locked
        // coordinates back.
        for (index, start) in locked_at {
            let body = &mut bodies[index];
            for axis in 0..3 {
                if body.locks.linear[axis] {
                    body.position[axis] = start[axis];
                }
            }
        }
        write_back(world, &bodies);
        if last {
            fall_asleep(world, &bodies, &mut rest);
        }
    }

    for (a, b, contact) in &contacts {
        sent.push(CollisionEvent {
            a: bodies[*a].entity,
            b: bodies[*b].entity,
            sensor: contact.sensor,
        });
    }
    // Pairs of inert bodies are not tested, so a sleeper keeps the contacts
    // it fell asleep with: a ball resting on the floor still touches it.
    let inert: HashSet<Entity> = bodies
        .iter()
        .filter(|body| body.asleep || body.kind == RigidBodyKind::Fixed)
        .map(|body| body.entity)
        .collect();
    let mut physics = world.resource_mut::<PhysicsWorld>();
    let resting: Vec<Contact> = physics
        .contacts
        .iter()
        .filter(|contact| {
            inert.contains(&contact.a) && inert.contains(&contact.b)
        })
        .map(|contact| Contact {
            speed: 0.0,
            ..*contact
        })
        .collect();
    physics.contacts = contacts
        .into_iter()
        .map(|(.., contact)| contact)
        .chain(resting)
        .collect();
    all_impacts.extend(impacts);
    physics.set_bodies(bodies);
    physics.rest = rest;
    if let Some(kill_y) = settings.kill_y {
        kill_fallen(world, kill_y);
    }
}

/// Despawns the loose dynamic bodies below `kill_y` with their children and
/// sends [`FellOut`] for each, in body order.
fn kill_fallen(world: &mut World, kill_y: f32) {
    let fallen: Vec<Entity> = world
        .resource::<PhysicsWorld>()
        .bodies
        .iter()
        .filter(|body| {
            body.movable
                && !body.proxy
                && body.kind == RigidBodyKind::Dynamic
                && body.position.y < kill_y
        })
        .map(|body| body.entity)
        .collect();
    for entity in fallen {
        let mut physics = world.resource_mut::<PhysicsWorld>();
        let physics = &mut *physics;
        physics.forget_body(entity);
        let mut bodies = std::mem::take(&mut physics.bodies);
        bodies.retain(|body| body.entity != entity);
        physics.set_bodies(bodies);
        for contacts in [&mut physics.contacts, &mut physics.impacts] {
            contacts
                .retain(|contact| contact.a != entity && contact.b != entity);
        }
        let name = world.get::<Name>(entity).map(|name| name.0.clone());
        world
            .resource_mut::<EventQueue<FellOut>>()
            .send(FellOut { entity, name });
        crate::runtime::despawn_tree(world, entity);
    }
}

/// Sent in the fixed step a loose dynamic CPU body fell below
/// [`PhysicsSettings::kill_y`], after it and its children were despawned.
#[derive(Clone, Debug, PartialEq)]
pub struct FellOut {
    pub entity: Entity,
    pub name: Option<String>,
}

impl PhysicsWorld {
    /// Forgets the contact impulses and sleep count kept for `entity`, so
    /// its next step starts as if it had just been created.
    pub(crate) fn forget_body(&mut self, entity: Entity) {
        self.rest.remove(&entity);
        self.warm.retain(|(a, b), _| *a != entity && *b != entity);
        self.joint_warm.remove(&entity);
    }

    /// Restarts `entity`'s still-step count, so a body that gameplay drives
    /// every step never falls asleep.
    pub(crate) fn keep_awake(&mut self, entity: Entity) {
        self.rest.remove(&entity);
    }

    /// Moves the kept solver state from old entities to new ones, after a
    /// snapshot was loaded into new entities. State of unlisted entities is
    /// dropped.
    pub(crate) fn rename_bodies(&mut self, renamed: &HashMap<Entity, Entity>) {
        let new = |entity: &Entity| renamed.get(entity).copied();
        let mut bodies = std::mem::take(&mut self.bodies);
        bodies.retain_mut(|body| {
            new(&body.entity)
                .map(|entity| body.entity = entity)
                .is_some()
        });
        self.set_bodies(bodies);
        for contacts in [&mut self.contacts, &mut self.impacts] {
            contacts.retain_mut(|contact| {
                match (new(&contact.a), new(&contact.b)) {
                    (Some(a), Some(b)) => {
                        (contact.a, contact.b) = (a, b);
                        true
                    }
                    _ => false,
                }
            });
        }
        self.rest = self
            .rest
            .drain()
            .filter_map(|(entity, rest)| Some((new(&entity)?, rest)))
            .collect();
        self.warm = self
            .warm
            .drain()
            .filter_map(|((a, b), points)| Some(((new(&a)?, new(&b)?), points)))
            .collect();
        self.joint_warm = self
            .joint_warm
            .drain()
            .filter_map(|(entity, rows)| Some((new(&entity)?, rows)))
            .collect();
    }
}

/// Wakes each sleeping body near where a body was removed, or was moved,
/// turned or changed in kind by gameplay since the last step, so a pile
/// falls when its floor goes away. Contacts between a sleeper and a fixed
/// body are never recorded, so this checks bounding spheres instead.
// ponytail: every changed body against every sleeper; a moving kinematic
// platform beside a large sleeping pile pays this each step. Use the broad
// phase if that shows.
fn wake_on_lost_support(world: &World, bodies: &mut [Body]) {
    const MARGIN: f32 = 0.05;
    let sleepers: Vec<usize> = (0..bodies.len())
        .filter(|&index| bodies[index].asleep)
        .collect();
    if sleepers.is_empty() {
        return;
    }
    let index: HashMap<Entity, usize> = bodies
        .iter()
        .enumerate()
        .map(|(index, body)| (body.entity, index))
        .collect();
    let physics = world.resource::<PhysicsWorld>();
    let changed = physics.bodies.iter().filter(|old| {
        let Some(&now) = index.get(&old.entity) else {
            return true;
        };
        let now = &bodies[now];
        // The solver moves awake dynamic bodies, and the speed rule wakes
        // what they push. Anything else moved because gameplay moved it.
        let solver_moved = old.kind == RigidBodyKind::Dynamic
            && now.kind == RigidBodyKind::Dynamic
            && !old.asleep;
        !solver_moved
            && (old.position != now.position
                || old.rotation != now.rotation
                || old.kind != now.kind)
    });
    let mut wake = Vec::new();
    for other in changed.filter(|other| !other.sensor) {
        for &sleeper in &sleepers {
            let body = &bodies[sleeper];
            let reach =
                other.bounding_radius() + body.bounding_radius() + MARGIN;
            if body.entity != other.entity
                && (body.position - other.position).norm_squared()
                    <= reach * reach
            {
                wake.push(sleeper);
            }
        }
    }
    for index in wake {
        bodies[index].asleep = false;
    }
}

/// Counts still steps for awake dynamic bodies and puts the ones that stayed
/// still long enough to sleep.
fn fall_asleep(
    world: &mut World,
    bodies: &[Body],
    rest: &mut HashMap<Entity, (u32, [f32; 3])>,
) {
    for body in bodies.iter().filter(|body| {
        !body.asleep
            && !body.articulated
            && body.inverse_mass > 0.0
            && body.kind == RigidBodyKind::Dynamic
    }) {
        let still = body.velocity.norm() < SLEEP_LINEAR
            && body.angular_velocity.norm() < SLEEP_ANGULAR;
        let steps = rest.entry(body.entity).or_default();
        steps.0 = if still { steps.0 + 1 } else { 0 };
        if steps.0 < SLEEP_STEPS {
            continue;
        }
        steps.1 = body.position.into();
        let mut entity = world.entity_mut(body.entity);
        entity.insert(Sleeping);
        if let Some(mut rigid) = entity.get_mut::<RigidBody>() {
            rigid.linear_velocity = [0.0; 3];
            rigid.angular_velocity = [0.0; 3];
        }
    }
}

/// Collision bodies of every CPU and static collider, in [`SpawnOrder`]. A
/// mesh collider whose entity has no loaded mesh is left out.
fn gather_bodies(world: &mut World, meshes: &mut MeshCache) -> Vec<Body> {
    let mut query = world.query::<(
        Entity,
        &Transform,
        &Collider,
        &PhysicsBody,
        Option<&RigidBody>,
        Option<&Parent>,
        Option<&GlobalTransform>,
        Option<&CollisionLayers>,
        Option<&Sleeping>,
        Option<&MeshRenderer>,
        Option<&GpuProxyOf>,
        Option<&SpawnOrder>,
        Has<super::PlayerController>,
        (
            Option<&PhysicsMaterial>,
            Option<&AxisLock>,
            Option<&super::GpuStateMirror>,
        ),
    )>();
    let time = world.resource::<FrameTime>();
    let (tick, dt) = (time.fixed_tick, time.fixed_delta.as_secs_f32());
    let assets = world.get_resource::<AssetServer>();
    let mut used = MeshCache::new();
    let mut bodies = query
        .iter(world)
        .filter(|(_, _, _, physics, rigid, .., (_, _, mirror))| {
            // A fixed GPU body never moves, so CPU queries and the player
            // can stand on it as a static collider. A dynamic one with fresh
            // read-back state is met where that state puts it.
            match physics.simulation {
                SimulationClass::Cpu | SimulationClass::Static => true,
                SimulationClass::Gpu => {
                    matches!(
                        physics.solver,
                        PhysicsSolver::Full | PhysicsSolver::Simplified
                    ) && rigid.is_some_and(|rigid| match rigid.kind {
                        RigidBodyKind::Fixed => true,
                        RigidBodyKind::Dynamic => mirror.is_some_and(|m| {
                            super::coupled_gpu_state(m, tick, dt).is_some()
                        }),
                        _ => false,
                    })
                }
                _ => false,
            }
        })
        .filter_map(
            |(
                entity,
                transform,
                collider,
                physics,
                rigid,
                parent,
                global,
                layers,
                sleeping,
                renderer,
                proxy,
                order,
                player,
                (material, locks, mirror),
            )| {
                let movable = parent.is_none();
                // ponytail: children read the pose propagated last frame, and
                // are treated as fixed; simulate them once joints exist.
                let pose = match (parent, global) {
                    (Some(_), Some(global)) => {
                        Transform::from_matrix(Matrix4::from(global.matrix))
                    }
                    _ => *transform,
                };
                let mut rigid = rigid.copied().unwrap_or(RigidBody {
                    kind: RigidBodyKind::Fixed,
                    ..RigidBody::default()
                });
                // A dynamic GPU body moves CPU bodies like a kinematic one
                // and is never written back; the GPU feels them through
                // `gpu_colliders`.
                // ponytail: each side answers the other's last known state,
                // so momentum is not exactly kept between CPU and GPU, and
                // with substeps the GPU pose does not advance between them.
                let gpu_state = mirror
                    .filter(|_| {
                        physics.simulation == SimulationClass::Gpu
                            && rigid.kind == RigidBodyKind::Dynamic
                    })
                    .and_then(|mirror| {
                        super::coupled_gpu_state(mirror, tick, dt)
                    });
                let pose = match gpu_state {
                    Some((pose, velocity)) => {
                        rigid.linear_velocity = velocity;
                        rigid.angular_velocity = mirror?.angular_velocity;
                        pose
                    }
                    None => pose,
                };
                let kind = match (physics.simulation, gpu_state) {
                    (SimulationClass::Cpu, _) => rigid.kind,
                    (_, Some(_)) => RigidBodyKind::Kinematic,
                    _ => RigidBodyKind::Fixed,
                };
                let shape = match Shape::scaled(collider.shape, pose.scale) {
                    Some(shape) => shape,
                    None if collider.shape == ColliderShape::Heightfield => {
                        let field =
                            world.entity(entity).get_ref::<Heightfield>()?;
                        // Keyed by entity and last change, so an unchanged
                        // grid is not rebuilt every step.
                        let key = (
                            entity.to_bits(),
                            u64::from(field.last_changed().get()),
                            pose.scale.map(f32::to_bits),
                            false,
                        );
                        let mesh = match meshes.get(&key) {
                            Some(mesh) => mesh.clone(),
                            None => Arc::new(MeshData {
                                surfaces: field.triangle_surfaces(),
                                surface_values: GroundSurface::values(
                                    &field.surfaces,
                                ),
                                ..MeshData::new(
                                    &field.mesh(),
                                    pose.scale,
                                    false,
                                )
                            }),
                        };
                        used.insert(key, mesh.clone());
                        Shape::Triangles(mesh)
                    }
                    None if matches!(
                        collider.shape,
                        ColliderShape::Polygon | ColliderShape::Chain
                    ) =>
                    {
                        let outline =
                            world.entity(entity).get_ref::<Polygon2d>()?;
                        let convex = collider.shape == ColliderShape::Polygon;
                        let key = (
                            entity.to_bits(),
                            u64::from(outline.last_changed().get()),
                            pose.scale.map(f32::to_bits),
                            convex,
                        );
                        let mesh = match meshes.get(&key) {
                            Some(mesh) => mesh.clone(),
                            None => Arc::new(MeshData::new(
                                &outline.mesh_with(convex),
                                pose.scale,
                                convex,
                            )),
                        };
                        used.insert(key, mesh.clone());
                        if convex {
                            Shape::Hull(mesh)
                        } else {
                            Shape::Triangles(mesh)
                        }
                    }
                    None => {
                        let handle = renderer?.mesh;
                        let assets = assets?;
                        let convex =
                            collider.shape == ColliderShape::ConvexMesh;
                        let revision = assets.meshes.revision(handle)?;
                        let tags = world
                            .entity(entity)
                            .get_ref::<MeshSurfaces>()
                            .filter(|_| !convex);
                        // Tagged meshes are cached per entity, since two
                        // entities can tag one mesh differently.
                        let key = match &tags {
                            Some(tags) => (
                                entity.to_bits(),
                                revision << 32
                                    | u64::from(tags.last_changed().get()),
                                pose.scale.map(f32::to_bits),
                                false,
                            ),
                            None => (
                                handle.key(),
                                revision,
                                pose.scale.map(f32::to_bits),
                                convex,
                            ),
                        };
                        let mesh = match meshes.get(&key) {
                            Some(mesh) => mesh.clone(),
                            None => {
                                let mut mesh = MeshData::new(
                                    assets.meshes.get(handle)?,
                                    pose.scale,
                                    convex,
                                );
                                if let Some(tags) = &tags {
                                    mesh.surfaces = tags.triangle_surfaces(
                                        mesh.triangles.len(),
                                    );
                                    mesh.surface_values =
                                        GroundSurface::values(&tags.surfaces);
                                }
                                Arc::new(mesh)
                            }
                        };
                        used.insert(key, mesh.clone());
                        if convex {
                            Shape::Hull(mesh)
                        } else {
                            Shape::Triangles(mesh)
                        }
                    }
                };
                let inverse_mass = if kind == RigidBodyKind::Dynamic
                    && movable
                    && rigid.mass > 0.0
                    && !matches!(shape, Shape::Triangles(_))
                {
                    1.0 / rigid.mass
                } else {
                    0.0
                };
                let moving = kind != RigidBodyKind::Fixed;
                let key = (order.map_or(u64::MAX, |order| order.0), entity);
                Some((
                    key,
                    parent.map(|parent| parent.0),
                    Body {
                        entity,
                        kind,
                        movable,
                        position: pose.position.into(),
                        rotation: sim_math::rotation_from_euler(
                            pose.rotation[0],
                            pose.rotation[1],
                            pose.rotation[2],
                        ),
                        sensor: collider.sensor,
                        // GPU bodies already collide with each other on the GPU.
                        proxy: proxy.is_some()
                            || physics.simulation == SimulationClass::Gpu,
                        layers: layers.copied().unwrap_or_default(),
                        friction: collider.friction.max(0.0),
                        restitution: collider.restitution.clamp(0.0, 1.0),
                        combine: material
                            .map_or([CombineMode::Default; 2], |m| {
                                [m.friction_combine, m.restitution_combine]
                            }),
                        inverse_mass,
                        // A dynamic player never tips over.
                        inverse_inertia: if inverse_mass > 0.0 && !player {
                            shape.inverse_inertia(rigid.mass)
                        } else {
                            Vector3::zeros()
                        },
                        shape,
                        asleep: sleeping.is_some() && inverse_mass > 0.0,
                        articulated: false,
                        locks: locks.copied().unwrap_or_default(),
                        velocity: if moving {
                            rigid.linear_velocity.into()
                        } else {
                            Vector3::zeros()
                        },
                        angular_velocity: if moving && !player {
                            rigid.angular_velocity.into()
                        } else {
                            Vector3::zeros()
                        },
                    },
                ))
            },
        )
        .collect::<Vec<_>>();
    *meshes = used;
    // Query order follows archetypes and entity ids change on a reload;
    // sort so results depend on neither.
    bodies.sort_by_key(|(key, ..)| *key);
    merge_compounds(
        world,
        bodies
            .into_iter()
            .map(|(_, parent, body)| (parent, body))
            .collect(),
    )
}

/// Folds the solid collider children of each dynamic CPU body into that
/// body as a [`Shape::Compound`], so they move with it (Godot's compound
/// rigid body). Contacts and ray hits on a child report the parent.
// ponytail: the parent's own mass, origin and friction stand for the whole
// compound, and its inertia is the box around the parts. Sum the parts'
// mass and inertia if uneven compounds tumble wrongly.
fn merge_compounds(
    world: &World,
    bodies: Vec<(Option<Entity>, Body)>,
) -> Vec<Body> {
    let hosts: HashMap<Entity, usize> = bodies
        .iter()
        .enumerate()
        .filter(|(_, (parent, body))| {
            parent.is_none()
                && body.kind == RigidBodyKind::Dynamic
                && body.inverse_mass > 0.0
        })
        .map(|(index, (_, body))| (body.entity, index))
        .collect();
    let mut parts: HashMap<usize, Vec<Part>> = HashMap::new();
    let mut merged = vec![false; bodies.len()];
    for (index, (parent, body)) in bodies.iter().enumerate() {
        let Some(&host) = parent.and_then(|parent| hosts.get(&parent)) else {
            continue;
        };
        if body.sensor || body.proxy {
            continue;
        }
        let (Some(local), Some(host_pose)) = (
            world.get::<Transform>(body.entity),
            world.get::<Transform>(bodies[host].1.entity),
        ) else {
            continue;
        };
        merged[index] = true;
        parts.entry(host).or_default().push(Part {
            offset: Vector3::from(local.position)
                .component_mul(&Vector3::from(host_pose.scale)),
            rotation: sim_math::rotation_from_euler(
                local.rotation[0],
                local.rotation[1],
                local.rotation[2],
            ),
            shape: body.shape.clone(),
        });
    }
    bodies
        .into_iter()
        .enumerate()
        .filter(|(index, _)| !merged[*index])
        .map(|(index, (_, mut body))| {
            if let Some(children) = parts.remove(&index) {
                let own = Part {
                    offset: Vector3::zeros(),
                    rotation: Rotation3::identity(),
                    shape: body.shape.clone(),
                };
                body.shape = Shape::Compound(
                    std::iter::once(own).chain(children).collect(),
                );
                if body.inverse_inertia != Vector3::zeros() {
                    body.inverse_inertia =
                        body.shape.inverse_inertia(1.0 / body.inverse_mass);
                }
            }
            body
        })
        .collect()
}

/// Candidates from a three-dimensional bounding-sphere broad phase. Sweep
/// the axis with the greatest center spread, then reject pairs separated on
/// either remaining axis before running the shape-specific collision test.
/// Sort pairs back into body order so the solver sees a stable constraint
/// order regardless of the selected axis.
fn broad_phase_candidates(bodies: &[Body]) -> Vec<(usize, usize)> {
    if bodies.len() < 2 {
        return Vec::new();
    }
    let radii = bodies.iter().map(Body::bounding_radius).collect::<Vec<_>>();
    let mut low = bodies[0].position;
    let mut high = low;
    for body in &bodies[1..] {
        low = low.inf(&body.position);
        high = high.sup(&body.position);
    }
    let spread = high - low;
    let axis = (0..3)
        .max_by(|&a, &b| {
            spread[a].total_cmp(&spread[b]).then_with(|| b.cmp(&a))
        })
        .unwrap();
    let mut order = (0..bodies.len()).collect::<Vec<_>>();
    order.sort_by(|&a, &b| {
        (bodies[a].position[axis] - radii[a])
            .total_cmp(&(bodies[b].position[axis] - radii[b]))
            .then_with(|| a.cmp(&b))
    });
    let mut candidates = Vec::new();
    for (rank, &a) in order.iter().enumerate() {
        let end = bodies[a].position[axis] + radii[a];
        for &b in &order[rank + 1..] {
            if bodies[b].position[axis] - radii[b] > end {
                break;
            }
            let reach = radii[a] + radii[b];
            if (0..3).any(|other| {
                other != axis
                    && (bodies[a].position[other] - bodies[b].position[other])
                        .abs()
                        > reach
            }) {
                continue;
            }
            candidates.push((a.min(b), a.max(b)));
        }
    }
    candidates.sort_unstable();
    candidates
}

/// Each body's gravity: the scene's, or that of the [`GravityVolume`]
/// sensor it overlaps with the highest priority (ties go to the first in
/// body order, so the result does not depend on contact order).
// ponytail: articulated bodies and sleepers keep the scene gravity; a volume
// moved onto a sleeping body does not wake it.
fn body_gravity(
    world: &World,
    bodies: &[Body],
    contacts: &[(usize, usize, Contact)],
    scene: Vector3<f32>,
) -> Vec<Vector3<f32>> {
    let mut best: Vec<Option<(i32, usize, GravityVolume)>> =
        vec![None; bodies.len()];
    for (a, b, _) in contacts.iter().filter(|(.., c)| c.sensor) {
        for (volume, body) in [(*a, *b), (*b, *a)] {
            let Some(found) = world.get::<GravityVolume>(bodies[volume].entity)
            else {
                continue;
            };
            let slot = &mut best[body];
            if slot.is_none_or(|(priority, index, _)| {
                (found.priority, std::cmp::Reverse(volume))
                    > (priority, std::cmp::Reverse(index))
            }) {
                *slot = Some((found.priority, volume, *found));
            }
        }
    }
    best.iter()
        .zip(bodies)
        .map(|(slot, body)| match slot {
            None => scene,
            Some((_, volume, found)) if found.toward_center > 0.0 => {
                (bodies[*volume].position - body.position)
                    .try_normalize(1e-6)
                    .unwrap_or_default()
                    * found.toward_center
            }
            Some((.., found)) => Vector3::from(found.gravity),
        })
        .collect()
}

/// Each body's summed [`ForceField`] acceleration, from the sensors it
/// overlaps in contact order (stable, so the sum is too).
fn field_pushes(
    world: &World,
    bodies: &[Body],
    contacts: &[(usize, usize, Contact)],
    seconds: f32,
) -> Vec<Vector3<f32>> {
    let functions = world.get_resource::<ForceFieldFunctions>();
    let mut pushes = vec![Vector3::zeros(); bodies.len()];
    for (a, b, _) in contacts.iter().filter(|(.., c)| c.sensor) {
        for (field, body) in [(*a, *b), (*b, *a)] {
            if let Some(found) = world.get::<ForceField>(bodies[field].entity) {
                pushes[body] += found.push(
                    bodies[body].position - bodies[field].position,
                    bodies[body].velocity,
                    seconds,
                    functions,
                );
            }
        }
    }
    pushes
}

/// Narrow phase over the broad-phase pairs, split across rayon workers.
/// An indexed collect keeps pair order, so any worker count gives the same
/// contacts in the same order.
fn find_contacts(bodies: &[Body]) -> Vec<(usize, usize, Contact)> {
    use rayon::prelude::*;
    broad_phase_candidates(bodies)
        .par_iter()
        .with_min_len(64)
        .filter_map(|&(a, b)| {
            narrow_phase(&bodies[a], &bodies[b]).map(|contact| (a, b, contact))
        })
        .collect()
}

fn narrow_phase(first: &Body, second: &Body) -> Option<Contact> {
    let inert = |body: &Body| body.kind == RigidBodyKind::Fixed || body.asleep;
    if inert(first) && inert(second) || !first.interacts_with(second) {
        return None;
    }
    let reach = first.bounding_radius() + second.bounding_radius();
    if (first.position - second.position).norm_squared() > reach * reach {
        return None;
    }
    collide(first, second)
}

/// One manifold point of a solid contact, prepared for the solver.
#[derive(Clone)]
struct SolverPoint {
    a: usize,
    b: usize,
    normal: Vector3<f32>,
    tangents: [Vector3<f32>; 2],
    /// Contact point relative to each body's center.
    ra: Vector3<f32>,
    rb: Vector3<f32>,
    /// Inverse effective masses along the normal and the two tangents.
    masses: [f32; 3],
    /// Normal speed the contact aims for (restitution bounce).
    target: f32,
    friction: f32,
    /// Accumulated normal and tangent impulses.
    impulses: [f32; 3],
}

/// Sequential impulses over every manifold point, with angular response and
/// accumulated friction clamped by each point's normal impulse. Joint rows
/// are solved before the contacts in each iteration. Returns the joints
/// that broke, from [`joints::broken`].
fn solve_velocities(
    bodies: &mut [Body],
    articulations: &mut articulation::Articulations,
    contacts: &[(usize, usize, Contact)],
    warm: &mut WarmStart,
    links: &[joints::JointLink],
    joint_warm: &mut JointWarm,
    dt: f32,
) -> Vec<(usize, f32, f32)> {
    let inertia = bodies
        .iter()
        .map(Body::world_inverse_inertia)
        .collect::<Vec<_>>();
    let mut rows =
        joints::joint_rows(bodies, &inertia, articulations, links, dt);
    for (link, rows) in &mut rows {
        if let Some(old) = joint_warm.get(&links[*link].entity) {
            for row in rows {
                if let Some((_, impulse)) =
                    old.iter().find(|(key, _)| *key == row.key)
                {
                    row.impulse = *impulse;
                }
            }
        }
    }
    let point_velocity = |bodies: &[Body], a: usize, b: usize, ra, rb| {
        let (first, second) = (&bodies[a], &bodies[b]);
        second.velocity + second.angular_velocity.cross(&rb)
            - first.velocity
            - first.angular_velocity.cross(&ra)
    };
    let mut points = Vec::new();
    for (a, b, contact) in contacts {
        let (a, b) = (*a, *b);
        let (first, second) = (&bodies[a], &bodies[b]);
        if contact.sensor || first.inverse_mass + second.inverse_mass == 0.0 {
            continue;
        }
        let normal = Vector3::from(contact.normal);
        let tangent = normal
            .cross(&Vector3::x())
            .try_normalize(1e-3)
            .unwrap_or_else(|| normal.cross(&Vector3::z()).normalize());
        let tangents = [tangent, normal.cross(&tangent)];
        // A heightfield cell with its own surface replaces that side's
        // friction and restitution.
        let [first_friction, first_restitution] =
            surface_values(first, contact)
                .unwrap_or([first.friction, first.restitution]);
        let [second_friction, second_restitution] =
            surface_values(second, contact)
                .unwrap_or([second.friction, second.restitution]);
        for point in manifold(first, second, contact) {
            let (ra, rb) = (point - first.position, point - second.position);
            let articulated = first.articulated || second.articulated;
            let mass = |direction: &Vector3<f32>| {
                let (ca, cb) = (ra.cross(direction), rb.cross(direction));
                let k = if articulated {
                    articulations.inverse_mass(
                        bodies,
                        &inertia,
                        &[(a, -ca, -direction), (b, cb, *direction)],
                    )
                } else {
                    first.inverse_mass
                        + second.inverse_mass
                        + ca.dot(&(inertia[a] * ca))
                        + cb.dot(&(inertia[b] * cb))
                };
                if k > 0.0 {
                    1.0 / k
                } else {
                    0.0
                }
            };
            // Bounce targets use the closing speed before any impulse.
            let closing = point_velocity(bodies, a, b, ra, rb).dot(&normal);
            let restitution = first.combine[1].combine(
                second.combine[1],
                first_restitution,
                second_restitution,
                f32::max,
            );
            // correct_positions leaves articulations alone, so their
            // contacts push out of overlap through the velocity target.
            let push_out = if articulated {
                CONTACT_BIAS / dt * (contact.depth - PENETRATION_SLOP).max(0.0)
            } else {
                0.0
            };
            points.push(SolverPoint {
                a,
                b,
                normal,
                tangents,
                ra,
                rb,
                masses: [mass(&normal), mass(&tangents[0]), mass(&tangents[1])],
                target: if closing < -RESTITUTION_THRESHOLD {
                    -restitution * closing
                } else {
                    0.0
                }
                .max(push_out),
                friction: first.combine[0].combine(
                    second.combine[0],
                    first_friction,
                    second_friction,
                    |a, b| (a * b).sqrt(),
                ),
                impulses: warm
                    .get(&(first.entity, second.entity))
                    .and_then(|old| {
                        let local = first.rotation.inverse() * ra;
                        old.iter()
                            .find(|(at, _)| (at - local).norm() < WARM_MATCH)
                    })
                    .map_or([0.0; 3], |(_, impulses)| *impulses),
            });
        }
    }
    let apply = |bodies: &mut [Body],
                 articulations: &mut articulation::Articulations,
                 point: &SolverPoint,
                 impulse: Vector3<f32>| {
        let (a, b) = (point.a, point.b);
        if bodies[a].articulated || bodies[b].articulated {
            let length = impulse.norm();
            if length > 0.0 {
                let direction = impulse / length;
                let sides = [
                    (a, -point.ra.cross(&direction), -direction),
                    (b, point.rb.cross(&direction), direction),
                ];
                articulations.apply(bodies, &inertia, &sides, length);
            }
            return;
        }
        bodies[a].velocity -= impulse * bodies[a].inverse_mass;
        bodies[a].angular_velocity -= inertia[a] * point.ra.cross(&impulse);
        bodies[b].velocity += impulse * bodies[b].inverse_mass;
        bodies[b].angular_velocity += inertia[b] * point.rb.cross(&impulse);
    };
    joints::warm_start(bodies, &inertia, articulations, &rows);
    for point in &points {
        let [normal, first, second] = point.impulses;
        let impulse = point.normal * normal
            + point.tangents[0] * first
            + point.tangents[1] * second;
        apply(bodies, articulations, point, impulse);
    }
    let serial = solve_free_islands(bodies, &inertia, links, &mut points);
    for _ in 0..SOLVER_ITERATIONS {
        for _ in 0..JOINT_PASSES {
            joints::solve(bodies, &inertia, articulations, &mut rows);
            articulations.solve_rows(bodies);
        }
        for &index in &serial {
            solve_point(
                &mut (&mut *bodies, &mut *articulations),
                &mut points[index],
                |(bodies, _), point| {
                    point_velocity(bodies, point.a, point.b, point.ra, point.rb)
                },
                |(bodies, articulations), point, impulse| {
                    apply(bodies, articulations, point, impulse);
                },
            );
        }
    }
    warm.clear();
    for point in &points {
        let first = &bodies[point.a];
        warm.entry((first.entity, bodies[point.b].entity))
            .or_default()
            .push((first.rotation.inverse() * point.ra, point.impulses));
    }
    let broken = joints::broken(links, &rows, dt);
    joint_warm.clear();
    for (link, rows) in rows {
        joint_warm.insert(
            links[link].entity,
            rows.iter().map(|row| (row.key, row.impulse)).collect(),
        );
    }
    broken
}

/// One sequential-impulse pass over `point`: the normal impulse, then
/// Coulomb friction on each tangent, limited by the normal impulse.
fn solve_point<S>(
    state: &mut S,
    point: &mut SolverPoint,
    velocity: impl Fn(&S, &SolverPoint) -> Vector3<f32>,
    apply: impl Fn(&mut S, &SolverPoint, Vector3<f32>),
) {
    let change = (point.target - velocity(state, point).dot(&point.normal))
        * point.masses[0];
    let total = (point.impulses[0] + change).max(0.0);
    let applied = total - point.impulses[0];
    point.impulses[0] = total;
    apply(state, point, point.normal * applied);
    let limit = point.friction * point.impulses[0];
    for axis in 0..2 {
        let tangent = point.tangents[axis];
        let change =
            -velocity(state, point).dot(&tangent) * point.masses[axis + 1];
        let total = (point.impulses[axis + 1] + change).clamp(-limit, limit);
        let applied = total - point.impulses[axis + 1];
        point.impulses[axis + 1] = total;
        apply(state, point, tangent * applied);
    }
}

/// Velocities of one body, copied out of `bodies` so an island can be
/// solved on its own worker.
#[derive(Clone, Copy)]
struct Motion {
    velocity: Vector3<f32>,
    angular_velocity: Vector3<f32>,
    inverse_mass: f32,
    inertia: Matrix3<f32>,
}

/// Solves every contact island that has no joint and no articulation on
/// rayon workers, all [`SOLVER_ITERATIONS`] at once. Islands share no
/// moving body, and each runs its points in contact order, so the result
/// is the same for any worker count. Returns the indices of the remaining
/// points, which the caller solves in turn with the joints.
fn solve_free_islands(
    bodies: &mut [Body],
    inertia: &[Matrix3<f32>],
    links: &[joints::JointLink],
    points: &mut [SolverPoint],
) -> Vec<usize> {
    use rayon::prelude::*;
    let moving = |index: usize| {
        bodies[index].inverse_mass > 0.0 || bodies[index].articulated
    };
    // Union-find over moving bodies; fixed and kinematic ones do not join
    // islands, because no impulse changes them.
    let mut parent = (0..bodies.len()).collect::<Vec<_>>();
    fn root(parent: &mut [usize], mut index: usize) -> usize {
        while parent[index] != index {
            parent[index] = parent[parent[index]];
            index = parent[index];
        }
        index
    }
    for point in points.iter() {
        if moving(point.a) && moving(point.b) {
            let (a, b) =
                (root(&mut parent, point.a), root(&mut parent, point.b));
            parent[a.max(b)] = a.min(b);
        }
    }
    let mut tied = vec![false; bodies.len()];
    for link in links {
        tied[link.b] = true;
        if let Some(a) = link.a {
            tied[a] = true;
        }
    }
    for index in 0..bodies.len() {
        if tied[index] || bodies[index].articulated {
            let top = root(&mut parent, index);
            tied[top] = true;
        }
    }
    // Islands in order of their first point.
    let mut island_of = HashMap::new();
    let mut islands: Vec<Vec<usize>> = Vec::new();
    let mut serial = Vec::new();
    for (index, point) in points.iter().enumerate() {
        let body = if moving(point.a) { point.a } else { point.b };
        let top = root(&mut parent, body);
        if !moving(body) || tied[top] {
            serial.push(index);
            continue;
        }
        let island = *island_of.entry(top).or_insert_with(|| {
            islands.push(Vec::new());
            islands.len() - 1
        });
        islands[island].push(index);
    }
    let bodies_ref = &*bodies;
    let points_ref = &*points;
    let solved = islands
        .par_iter()
        .map(|indices| {
            let mut slots = HashMap::new();
            let mut members = Vec::new();
            let mut motions = Vec::new();
            let mut slot = |body: usize| {
                *slots.entry(body).or_insert_with(|| {
                    members.push(body);
                    motions.push(Motion {
                        velocity: bodies_ref[body].velocity,
                        angular_velocity: bodies_ref[body].angular_velocity,
                        inverse_mass: bodies_ref[body].inverse_mass,
                        inertia: inertia[body],
                    });
                    motions.len() - 1
                })
            };
            let mut local = indices
                .iter()
                .map(|&index| {
                    let mut point = points_ref[index].clone();
                    point.a = slot(point.a);
                    point.b = slot(point.b);
                    point
                })
                .collect::<Vec<_>>();
            for _ in 0..SOLVER_ITERATIONS {
                for point in &mut local {
                    solve_point(
                        &mut motions,
                        point,
                        |motions, point| {
                            let (first, second) =
                                (&motions[point.a], &motions[point.b]);
                            second.velocity
                                + second.angular_velocity.cross(&point.rb)
                                - first.velocity
                                - first.angular_velocity.cross(&point.ra)
                        },
                        |motions, point, impulse| {
                            let first = &mut motions[point.a];
                            first.velocity -= impulse * first.inverse_mass;
                            first.angular_velocity -=
                                first.inertia * point.ra.cross(&impulse);
                            let second = &mut motions[point.b];
                            second.velocity += impulse * second.inverse_mass;
                            second.angular_velocity +=
                                second.inertia * point.rb.cross(&impulse);
                        },
                    );
                }
            }
            let impulses =
                local.iter().map(|point| point.impulses).collect::<Vec<_>>();
            (members, motions, impulses)
        })
        .collect::<Vec<_>>();
    for (indices, (members, motions, impulses)) in islands.iter().zip(solved) {
        for (body, motion) in members.into_iter().zip(motions) {
            if motion.inverse_mass > 0.0 {
                bodies[body].velocity = motion.velocity;
                bodies[body].angular_velocity = motion.angular_velocity;
            }
        }
        for (&index, impulses) in indices.iter().zip(impulses) {
            points[index].impulses = impulses;
        }
    }
    serial
}

/// Friction and restitution of the heightfield cell or tagged triangle
/// `contact` touched on `body`, when it has a surface of its own.
fn surface_values(body: &Body, contact: &Contact) -> Option<[f32; 2]> {
    let Shape::Triangles(ref mesh) = body.shape else {
        return None;
    };
    mesh.surface_values
        .get(usize::from(contact.surface?))
        .copied()
}

/// Contact points of a pair in world space. Two boxes touching face to face
/// get up to four points: the incident face clipped to the reference face.
/// Every other pair, and edge contacts, use the single contact point.
fn manifold(a: &Body, b: &Body, contact: &Contact) -> Vec<Vector3<f32>> {
    let single = vec![Vector3::from(contact.point)];
    if matches!(a.shape, Shape::Compound(_))
        || matches!(b.shape, Shape::Compound(_))
    {
        // Points of every touching pair of parts that pushes the same way
        // as the deepest one: a table stands on all its legs.
        let normal = Vector3::from(contact.normal);
        let others = b.parts();
        let mut points = Vec::new();
        for part in a.parts() {
            for other in &others {
                let Some(touch) = collide(&part, other) else {
                    continue;
                };
                if Vector3::from(touch.normal).dot(&normal) > 0.9 {
                    points.extend(manifold(&part, other, &touch));
                }
            }
        }
        return if points.is_empty() { single } else { points };
    }
    let (&Shape::Box(half_a), &Shape::Box(half_b)) = (&a.shape, &b.shape)
    else {
        return mesh_manifold(a, b, contact).unwrap_or(single);
    };
    let normal = Vector3::from(contact.normal);
    // The body axis most parallel to `direction`: (axis, alignment, sign).
    let facing = |body: &Body, direction: Vector3<f32>| {
        (0..3)
            .map(|axis| {
                let dot =
                    (body.rotation * Vector3::ith(axis, 1.0)).dot(&direction);
                (axis, dot.abs(), dot.signum())
            })
            .max_by(|x, y| x.1.total_cmp(&y.1))
            .unwrap()
    };
    let (face_a, face_b) = (facing(a, normal), facing(b, -normal));
    // The reference face is the one most parallel to the contact normal.
    let (reference, half, (axis, alignment, sign), incident, incident_half) =
        if face_a.1 >= face_b.1 {
            (a, half_a, face_a, b, half_b)
        } else {
            (b, half_b, face_b, a, half_a)
        };
    if alignment < 0.95 {
        return single;
    }
    let outward = reference.rotation * Vector3::ith(axis, sign);
    let (incident_axis, _, incident_sign) = facing(incident, -outward);
    let (u, v) = ((incident_axis + 1) % 3, (incident_axis + 2) % 3);
    let to_reference = reference.rotation.inverse();
    let mut polygon = [(1.0, 1.0), (1.0, -1.0), (-1.0, -1.0), (-1.0, 1.0)]
        .map(|(su, sv)| {
            let mut corner = Vector3::zeros();
            corner[incident_axis] =
                incident_sign * incident_half[incident_axis];
            corner[u] = su * incident_half[u];
            corner[v] = sv * incident_half[v];
            let world = incident.position + incident.rotation * corner;
            to_reference * (world - reference.position)
        })
        .to_vec();
    let sides = [(axis + 1) % 3, (axis + 2) % 3];
    for side in sides {
        for direction in [1.0, -1.0] {
            polygon = clip(&polygon, side, direction, half[side]);
        }
    }
    // Keep the clipped points that are below the reference face.
    polygon.retain(|point| half[axis] - sign * point[axis] >= 0.0);
    if polygon.is_empty() {
        return single;
    }
    if polygon.len() > 4 {
        // The extremes along both face diagonals are the corners of the
        // contact area; a same-size face clips to duplicated corners, where
        // per-axis extremes would keep only three.
        let (x, y) = (sides[0], sides[1]);
        let mut keep = [(1.0, 1.0), (1.0, -1.0), (-1.0, -1.0), (-1.0, 1.0)]
            .map(|(sx, sy)| {
                (0..polygon.len())
                    .max_by(|&i, &j| {
                        let score = |p: &Vector3<f32>| sx * p[x] + sy * p[y];
                        score(&polygon[i]).total_cmp(&score(&polygon[j]))
                    })
                    .unwrap()
            })
            .to_vec();
        keep.dedup();
        polygon = keep.into_iter().map(|index| polygon[index]).collect();
    }
    polygon
        .into_iter()
        .map(|point| reference.position + reference.rotation * point)
        .collect()
}

/// Contact points for a pair with a mesh collider: the vertices of the
/// polyhedral side that reach deepest along the normal, up to four.
// ponytail: those vertices are not clipped to the other collider, so a big
// box on one small triangle is supported at all four corners. Clip them as
// box pairs do if that shows.
fn mesh_manifold(
    a: &Body,
    b: &Body,
    contact: &Contact,
) -> Option<Vec<Vector3<f32>>> {
    /// Vertices within this distance of the deepest one count as touching.
    const TOLERANCE: f32 = 0.02;
    let is_mesh = |body: &Body| {
        matches!(body.shape, Shape::Hull(_) | Shape::Triangles(_))
    };
    if !is_mesh(a) && !is_mesh(b) {
        return None;
    }
    let normal = Vector3::from(contact.normal);
    // The smaller polyhedral body lies on the other; a triangle mesh has no
    // vertices here, so its partner is used.
    let size = |body: &Body| match body.shape {
        Shape::Box(half) => half.norm(),
        Shape::Hull(ref mesh) => mesh.half_extents.norm(),
        _ => f32::INFINITY,
    };
    let (vertices, toward, _) = [
        (a.vertices(), normal, size(a)),
        (b.vertices(), -normal, size(b)),
    ]
    .into_iter()
    .filter(|(vertices, ..)| !vertices.is_empty())
    .min_by(|x, y| x.2.total_cmp(&y.2))?;
    let deepest = vertices
        .iter()
        .map(|vertex| vertex.dot(&toward))
        .fold(f32::NEG_INFINITY, f32::max);
    let touching = vertices
        .into_iter()
        .filter(|vertex| vertex.dot(&toward) >= deepest - TOLERANCE)
        .collect::<Vec<_>>();
    if touching.len() < 2 {
        return None;
    }
    let tangent = normal
        .cross(&Vector3::x())
        .try_normalize(1e-3)
        .unwrap_or_else(|| normal.cross(&Vector3::z()).normalize());
    let bitangent = normal.cross(&tangent);
    let mut keep = [(1.0, 1.0), (1.0, -1.0), (-1.0, -1.0), (-1.0, 1.0)]
        .map(|(sx, sy)| {
            (0..touching.len())
                .max_by(|&i, &j| {
                    let score = |p: &Vector3<f32>| {
                        sx * p.dot(&tangent) + sy * p.dot(&bitangent)
                    };
                    score(&touching[i]).total_cmp(&score(&touching[j]))
                })
                .unwrap()
        })
        .to_vec();
    keep.sort_unstable();
    keep.dedup();
    Some(keep.into_iter().map(|index| touching[index]).collect())
}

/// Sutherland-Hodgman: keeps the part of a convex polygon where
/// `direction * point[side] <= limit`.
fn clip(
    polygon: &[Vector3<f32>],
    side: usize,
    direction: f32,
    limit: f32,
) -> Vec<Vector3<f32>> {
    let mut kept = Vec::with_capacity(polygon.len() + 1);
    for (index, &current) in polygon.iter().enumerate() {
        let next = polygon[(index + 1) % polygon.len()];
        let (here, there) = (
            direction * current[side] - limit,
            direction * next[side] - limit,
        );
        if here <= 0.0 {
            kept.push(current);
        }
        if (here <= 0.0) != (there <= 0.0) {
            kept.push(current + (next - current) * (here / (here - there)));
        }
    }
    kept
}

/// Continuous collision for bodies that would move farther than their inner
/// radius this step. A ray from the center along the motion, against targets
/// grown by the body's rounding radius, finds the first solid collider. A
/// fixed or kinematic target stops the body at its surface and removes the
/// velocity into it. A dynamic target stops the body just inside contact with
/// its velocity kept, so next step's contact solve hands over the momentum.
// ponytail: grown boxes keep sharp corners and hulls are not grown, so a
// fast body stops a little early at box corners and can still clip a hull
// edge it only grazes. Add a swept-shape test if gameplay needs it.
fn sweep_fast_bodies(bodies: &mut [Body], dt: f32) -> Vec<Contact> {
    let mut impacts = Vec::new();
    for index in 0..bodies.len() {
        let body = &bodies[index];
        let travel = body.velocity.norm() * dt;
        let reach = body.inner_radius();
        if body.inverse_mass == 0.0
            || body.articulated
            || body.sensor
            || travel <= reach
            // ponytail: a compound is not swept; a center ray would let its
            // parts pass through. Sweep each part if thin compounds tunnel.
            || matches!(body.shape, Shape::Compound(_))
        {
            continue;
        }
        let direction = body.velocity / body.velocity.norm();
        let pad = body.core_radius();
        let hit = bodies
            .iter()
            .enumerate()
            .filter(|(other, target)| {
                *other != index && !target.sensor && body.interacts_with(target)
            })
            .filter_map(|(_, target)| {
                // Most targets are nowhere near the ray: rule them out with
                // a bounding sphere before building a grown copy.
                let offset = target.position - body.position;
                let along = offset.dot(&direction);
                let size = target.bounding_radius() + pad;
                if along < -size
                    || along > travel + reach + size
                    || (offset - direction * along).norm() > size
                {
                    return None;
                }
                let target_grown = grown(target, pad);
                let (distance, normal) =
                    ray_body(body.position, direction, &target_grown)?;
                // Hulls and meshes come back ungrown, so `pad` is not in
                // their distance.
                let added =
                    if matches!(target_grown, std::borrow::Cow::Owned(_)) {
                        pad
                    } else {
                        0.0
                    };
                Some((
                    distance + added - reach,
                    normal,
                    target.kind,
                    target.entity,
                ))
            })
            .filter(|(distance, ..)| *distance < travel)
            .min_by(|a, b| a.0.total_cmp(&b.0));
        let Some((stop, normal, kind, target)) = hit else {
            continue;
        };
        let body = &mut bodies[index];
        if kind == RigidBodyKind::Dynamic {
            // Integration adds `travel` next, landing just inside contact.
            body.position += direction * (stop + PENETRATION_SLOP - travel);
            continue;
        }
        body.position += direction * stop.max(0.0);
        let into = body.velocity.dot(&normal);
        if into < 0.0 {
            body.velocity -= normal * into;
            impacts.push(Contact {
                a: body.entity,
                b: target,
                normal: (-normal).into(),
                depth: 0.0,
                point: body.position.into(),
                sensor: false,
                speed: -into,
                surface: None,
            });
        }
    }
    impacts
}

/// `body` with its sphere, box or capsule grown by `pad`, for casting a
/// rounded body's center against it.
fn grown(body: &Body, pad: f32) -> std::borrow::Cow<'_, Body> {
    let shape = match body.shape {
        _ if pad == 0.0 => return std::borrow::Cow::Borrowed(body),
        Shape::Sphere(radius) => Shape::Sphere(radius + pad),
        Shape::Box(half) => Shape::Box(half.add_scalar(pad)),
        Shape::Capsule {
            half_height,
            radius,
        } => Shape::Capsule {
            half_height,
            radius: radius + pad,
        },
        Shape::Hull(_) | Shape::Triangles(_) | Shape::Compound(_) => {
            return std::borrow::Cow::Borrowed(body);
        }
    };
    std::borrow::Cow::Owned(Body {
        shape,
        ..body.clone()
    })
}

/// Pushes overlapping bodies apart after integration. The contact depth is
/// corrected by how far the bodies already moved along the normal.
fn correct_positions(
    bodies: &mut [Body],
    contacts: &[(usize, usize, Contact)],
    dt: f32,
) {
    for (a, b, contact) in contacts {
        // Articulations resolve their overlap through the contact bias.
        let movable = |body: &Body| {
            if body.articulated {
                0.0
            } else {
                body.inverse_mass
            }
        };
        let (ima, imb) = (movable(&bodies[*a]), movable(&bodies[*b]));
        if contact.sensor || ima + imb == 0.0 {
            continue;
        }
        let normal = Vector3::from(contact.normal);
        let separation =
            (bodies[*b].velocity - bodies[*a].velocity).dot(&normal) * dt;
        let depth = contact.depth - separation - PENETRATION_SLOP;
        if depth <= 0.0 {
            continue;
        }
        let push = normal * (depth * POSITION_CORRECTION / (ima + imb));
        bodies[*a].position -= push * ima;
        bodies[*b].position += push * imb;
    }
}

fn write_back(world: &mut World, bodies: &[Body]) {
    // A proxy stands in for a body another solver owns.
    for body in bodies.iter().filter(|body| {
        body.movable && body.kind != RigidBodyKind::Fixed && !body.proxy
    }) {
        let position: [f32; 3] = body.position.into();
        let (x, y, z) = sim_math::euler_from_rotation(&body.rotation);
        let mut entity = world.entity_mut(body.entity);
        if let Some(mut transform) = entity.get_mut::<Transform>() {
            // Only touch changed fields so resting bodies do not trigger
            // change detection (and GPU or render re-extraction) every tick.
            if transform.position != position {
                transform.position = position;
            }
            if body.angular_velocity != Vector3::zeros() {
                transform.rotation = [x, y, z];
            }
        }
        if let Some(mut rigid) = entity.get_mut::<RigidBody>() {
            let velocity: [f32; 3] = body.velocity.into();
            if rigid.linear_velocity != velocity {
                rigid.linear_velocity = velocity;
            }
            let spin: [f32; 3] = body.angular_velocity.into();
            if rigid.angular_velocity != spin {
                rigid.angular_velocity = spin;
            }
        }
    }
}

/// Contact from `a` to `b`, or `None` when they do not touch.
fn collide(a: &Body, b: &Body) -> Option<Contact> {
    let sensor = a.sensor || b.sensor;
    let mut surface = None;
    let (normal, depth, point) = match (&a.shape, &b.shape) {
        (Shape::Compound(_), _) | (_, Shape::Compound(_)) => {
            // The deepest touching pair of parts.
            let others = b.parts();
            return a
                .parts()
                .iter()
                .flat_map(|part| {
                    others.iter().filter_map(|other| collide(part, other))
                })
                .max_by(|x, y| x.depth.total_cmp(&y.depth));
        }
        (Shape::Triangles(_), Shape::Triangles(_)) => return None,
        (_, Shape::Triangles(mesh)) => {
            let hit;
            (hit, surface) = triangles(a, b, mesh)?;
            hit
        }
        (Shape::Triangles(mesh), _) => {
            let hit;
            (hit, surface) = triangles(b, a, mesh)?;
            flip(hit)
        }
        (Shape::Hull(_), _) | (_, Shape::Hull(_)) => convex(
            |direction| a.support(direction),
            |direction| b.support(direction),
            a.core_radius(),
            b.core_radius(),
            b.position - a.position,
        )?,
        (&Shape::Sphere(ra), &Shape::Sphere(rb)) => {
            spheres(a.position, ra, b.position, rb)?
        }
        (&Shape::Sphere(radius), &Shape::Box(half)) => {
            sphere_box(a.position, radius, b, half)?
        }
        (&Shape::Box(half), &Shape::Sphere(radius)) => {
            flip(sphere_box(b.position, radius, a, half)?)
        }
        (&Shape::Box(ha), &Shape::Box(hb)) => boxes(a, ha, b, hb)?,
        (Shape::Capsule { .. }, _) => {
            let (center, radius) = capsule_proxy(a, b);
            let proxy = Body {
                position: center,
                shape: Shape::Sphere(radius),
                ..a.clone()
            };
            return collide(&proxy, b).map(|contact| Contact {
                a: a.entity,
                sensor,
                ..contact
            });
        }
        (_, Shape::Capsule { .. }) => {
            let contact = collide(b, a)?;
            surface = contact.surface;
            flip((contact.normal.into(), contact.depth, contact.point.into()))
        }
    };
    Some(Contact {
        a: a.entity,
        b: b.entity,
        normal: normal.into(),
        depth,
        point: point.into(),
        sensor,
        speed: 0.0,
        surface,
    })
}

type Hit = (Vector3<f32>, f32, Vector3<f32>);

fn flip((normal, depth, point): Hit) -> Hit {
    (-normal, depth, point)
}

/// The sphere on the capsule's segment nearest to `other`, which stands in
/// for the capsule in the pair test.
// ponytail: against a box the nearest segment point is refined twice, not
// solved exactly; a box edge crossing a long capsule can report shallow depth.
fn capsule_proxy(capsule: &Body, other: &Body) -> (Vector3<f32>, f32) {
    let (start, end, radius) = capsule.segment();
    let center = match other.shape {
        Shape::Hull(_) | Shape::Triangles(_) | Shape::Compound(_) => {
            unreachable!("mesh and compound pairs use their own tests")
        }
        Shape::Capsule { .. } => {
            let (other_start, other_end, _) = other.segment();
            segments_closest(start, end, other_start, other_end).0
        }
        Shape::Box(half) => {
            let mut point = closest_on_segment(start, end, other.position);
            for _ in 0..2 {
                point = closest_on_segment(
                    start,
                    end,
                    closest_on_box(point, other, half),
                );
            }
            point
        }
        Shape::Sphere(_) => closest_on_segment(start, end, other.position),
    };
    (center, radius)
}

fn spheres(a: Vector3<f32>, ra: f32, b: Vector3<f32>, rb: f32) -> Option<Hit> {
    let offset = b - a;
    let distance = offset.norm();
    if distance > ra + rb {
        return None;
    }
    let normal = offset.try_normalize(1e-6).unwrap_or_else(Vector3::y);
    Some((normal, ra + rb - distance, a + normal * ra))
}

fn closest_on_box(
    point: Vector3<f32>,
    body: &Body,
    half: Vector3<f32>,
) -> Vector3<f32> {
    let local = body.rotation.inverse() * (point - body.position);
    let clamped = local
        .zip_zip_map(&-half, &half, |value, low, high| value.clamp(low, high));
    body.position + body.rotation * clamped
}

fn sphere_box(
    center: Vector3<f32>,
    radius: f32,
    body: &Body,
    half: Vector3<f32>,
) -> Option<Hit> {
    let local = body.rotation.inverse() * (center - body.position);
    let inside = local.iter().zip(half.iter()).all(|(v, h)| v.abs() <= *h);
    if inside {
        // Push out through the nearest face.
        let (axis, gap) = (0..3)
            .map(|axis| (axis, half[axis] - local[axis].abs()))
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .unwrap();
        let mut face = Vector3::zeros();
        face[axis] = local[axis].signum();
        // Normal from the sphere toward the box.
        let normal = -(body.rotation * face);
        return Some((normal, gap + radius, center));
    }
    let closest = closest_on_box(center, body, half);
    let offset = closest - center;
    let distance = offset.norm();
    if distance > radius {
        return None;
    }
    Some((offset / distance.max(1e-6), radius - distance, closest))
}

fn boxes(
    a: &Body,
    ha: Vector3<f32>,
    b: &Body,
    hb: Vector3<f32>,
) -> Option<Hit> {
    let axes_a = [0, 1, 2].map(|i| a.rotation * Vector3::ith(i, 1.0));
    let axes_b = [0, 1, 2].map(|i| b.rotation * Vector3::ith(i, 1.0));
    fn project(
        axes: &[Vector3<f32>; 3],
        half: Vector3<f32>,
        axis: &Vector3<f32>,
    ) -> f32 {
        (0..3).map(|i| half[i] * axes[i].dot(axis).abs()).sum()
    }
    let offset = b.position - a.position;
    let mut best: Option<(f32, Vector3<f32>)> = None;
    let candidates = axes_a.iter().chain(&axes_b).copied().chain(
        axes_a
            .iter()
            .flat_map(|x| axes_b.iter().map(move |y| x.cross(y))),
    );
    for axis in candidates {
        let Some(axis) = axis.try_normalize(1e-5) else {
            continue; // Parallel edges add nothing.
        };
        let overlap = project(&axes_a, ha, &axis) + project(&axes_b, hb, &axis)
            - offset.dot(&axis).abs();
        if overlap < 0.0 {
            return None;
        }
        // Face axes come first, so an equal edge axis never replaces one.
        if best.is_none_or(|(depth, _)| overlap < depth - 1e-5) {
            let normal = if offset.dot(&axis) < 0.0 { -axis } else { axis };
            best = Some((overlap, normal));
        }
    }
    let (depth, normal) = best?;
    // A representative point; the solver clips a full face manifold.
    let point = closest_on_box(b.position, a, ha)
        .lerp(&closest_on_box(a.position, b, hb), 0.5);
    Some((normal, depth, point))
}

/// `other` against each triangle of a static mesh `body`; the deepest
/// triangle contact wins. The normal points from `other` to the mesh.
// ponytail: every triangle is tested against a bounding sphere; add a BVH
// when meshes get large.
fn triangles(
    other: &Body,
    body: &Body,
    mesh: &MeshData,
) -> Option<(Hit, Option<u8>)> {
    let reach = other.bounding_radius();
    let (hit, index) = mesh
        .triangles
        .iter()
        .map(|triangle| triangle.map(|v| body.position + body.rotation * v))
        .enumerate()
        .filter(|(_, [a, b, c])| {
            let center = (a + b + c) / 3.0;
            let radius = [a, b, c]
                .iter()
                .map(|v| (*v - center).norm())
                .fold(0.0, f32::max);
            (other.position - center).norm() <= reach + radius
        })
        .filter_map(|(index, triangle)| {
            let hit = convex(
                |direction| other.support(direction),
                |direction| {
                    triangle
                        .iter()
                        .copied()
                        .max_by(|x, y| {
                            x.dot(&direction).total_cmp(&y.dot(&direction))
                        })
                        .unwrap()
                },
                other.core_radius(),
                0.0,
                triangle[0] - other.position,
            )?;
            Some((hit, index))
        })
        .max_by(|x, y| x.0 .1.total_cmp(&y.0 .1))?;
    Some((hit, mesh.surfaces.get(index).copied().flatten()))
}

/// A point of the Minkowski difference `A - B` with the point of `A` it
/// came from (the point of `B` is `from_a - point`).
#[derive(Clone, Copy, Debug)]
struct SupportPoint {
    point: Vector3<f32>,
    from_a: Vector3<f32>,
}

/// Contact between two convex cores given by support functions, each
/// rounded by a radius (a sphere is a rounded point, a capsule a rounded
/// segment). GJK finds the core distance; when the cores overlap, EPA finds
/// the penetration. `guess` is any direction from A toward B.
fn convex(
    support_a: impl Fn(Vector3<f32>) -> Vector3<f32>,
    support_b: impl Fn(Vector3<f32>) -> Vector3<f32>,
    radius_a: f32,
    radius_b: f32,
    guess: Vector3<f32>,
) -> Option<Hit> {
    let support = |direction: Vector3<f32>| {
        let from_a = support_a(direction);
        SupportPoint {
            point: from_a - support_b(-direction),
            from_a,
        }
    };
    let radii = radius_a + radius_b;
    let start = guess.try_normalize(1e-9).unwrap_or_else(Vector3::x);
    let mut simplex = vec![support(-start)];
    let mut closest = simplex[0].point;
    let mut from_a = simplex[0].from_a;
    let mut overlap = false;
    for _ in 0..64 {
        let length = closest.norm_squared();
        if length < 1e-10 {
            overlap = true;
            break;
        }
        let next = support(-closest);
        if length - closest.dot(&next.point) <= 1e-6 * length.max(1e-4) {
            break;
        }
        simplex.push(next);
        let Some(kept) = closest_on_simplex(&simplex) else {
            overlap = true;
            break;
        };
        closest = kept
            .iter()
            .fold(Vector3::zeros(), |sum, (v, w)| sum + v.point * *w);
        from_a = kept
            .iter()
            .fold(Vector3::zeros(), |sum, (v, w)| sum + v.from_a * *w);
        simplex = kept.into_iter().map(|(vertex, _)| vertex).collect();
    }
    if !overlap {
        let distance = closest.norm();
        if distance > radii {
            return None;
        }
        if distance > 1e-4 {
            let normal = -closest / distance;
            let surface_a = from_a + normal * radius_a;
            let surface_b = from_a - closest - normal * radius_b;
            return Some((
                normal,
                radii - distance,
                (surface_a + surface_b) * 0.5,
            ));
        }
    }
    let (normal, distance, from_a) = expand(&support, simplex).unwrap_or((
        start,
        0.0,
        support(start).from_a,
    ));
    let surface_a = from_a + normal * radius_a;
    let surface_b = from_a - normal * (distance + radius_b);
    Some((normal, distance + radii, (surface_a + surface_b) * 0.5))
}

/// The point of a simplex (1 to 4 support points) nearest the origin, as
/// the vertices it lies on with their barycentric weights. `None` when a
/// tetrahedron contains the origin.
fn closest_on_simplex(
    simplex: &[SupportPoint],
) -> Option<Vec<(SupportPoint, f32)>> {
    let weighted = |indices: &[usize], weights: &[f32]| {
        indices
            .iter()
            .zip(weights)
            .filter(|(_, weight)| **weight > 0.0)
            .map(|(&index, &weight)| (simplex[index], weight))
            .collect::<Vec<_>>()
    };
    match simplex.len() {
        1 => Some(vec![(simplex[0], 1.0)]),
        2 => {
            let (a, b) = (simplex[0].point, simplex[1].point);
            let t = (-a.dot(&(b - a)) / (b - a).norm_squared().max(1e-12))
                .clamp(0.0, 1.0);
            Some(weighted(&[0, 1], &[1.0 - t, t]))
        }
        3 => {
            let [a, b, c] = [0, 1, 2].map(|i| simplex[i].point);
            Some(weighted(&[0, 1, 2], &triangle_weights(a, b, c)))
        }
        _ => {
            // Faces the origin sees from outside; a flat tetrahedron counts
            // every face, since none of them bounds a volume.
            let faces =
                [[0, 1, 2, 3], [0, 1, 3, 2], [0, 2, 3, 1], [1, 2, 3, 0]];
            let points = [0, 1, 2, 3].map(|i| simplex[i].point);
            let mut best: Option<(f32, Vec<(SupportPoint, f32)>)> = None;
            for [i, j, k, opposite] in faces {
                let normal =
                    (points[j] - points[i]).cross(&(points[k] - points[i]));
                let origin_side = -points[i].dot(&normal);
                let inside_side = (points[opposite] - points[i]).dot(&normal);
                if origin_side * inside_side > 0.0 && inside_side.abs() > 1e-9 {
                    continue;
                }
                let weights = triangle_weights(points[i], points[j], points[k]);
                let kept = weighted(&[i, j, k], &weights);
                let distance = kept
                    .iter()
                    .fold(Vector3::zeros(), |sum, (v, w)| sum + v.point * *w)
                    .norm_squared();
                if best.as_ref().is_none_or(|(least, _)| distance < *least) {
                    best = Some((distance, kept));
                }
            }
            best.map(|(_, kept)| kept)
        }
    }
}

/// Barycentric weights of the point of triangle `abc` nearest the origin
/// (Ericson, Real-Time Collision Detection 5.1.5).
fn triangle_weights(
    a: Vector3<f32>,
    b: Vector3<f32>,
    c: Vector3<f32>,
) -> [f32; 3] {
    let (ab, ac) = (b - a, c - a);
    let (d1, d2) = (-ab.dot(&a), -ac.dot(&a));
    if d1 <= 0.0 && d2 <= 0.0 {
        return [1.0, 0.0, 0.0];
    }
    let (d3, d4) = (-ab.dot(&b), -ac.dot(&b));
    if d3 >= 0.0 && d4 <= d3 {
        return [0.0, 1.0, 0.0];
    }
    let vc = d1 * d4 - d3 * d2;
    if vc <= 0.0 && d1 >= 0.0 && d3 <= 0.0 {
        let v = d1 / (d1 - d3);
        return [1.0 - v, v, 0.0];
    }
    let (d5, d6) = (-ab.dot(&c), -ac.dot(&c));
    if d6 >= 0.0 && d5 <= d6 {
        return [0.0, 0.0, 1.0];
    }
    let vb = d5 * d2 - d1 * d6;
    if vb <= 0.0 && d2 >= 0.0 && d6 <= 0.0 {
        let w = d2 / (d2 - d6);
        return [1.0 - w, 0.0, w];
    }
    let va = d3 * d6 - d5 * d4;
    if va <= 0.0 && d4 - d3 >= 0.0 && d5 - d6 >= 0.0 {
        let w = (d4 - d3) / ((d4 - d3) + (d5 - d6));
        return [0.0, 1.0 - w, w];
    }
    let denominator = 1.0 / (va + vb + vc);
    let (v, w) = (vb * denominator, vc * denominator);
    [1.0 - v - w, v, w]
}

/// EPA: grows the simplex of overlapping cores into a polytope until its
/// face nearest the origin lies on the Minkowski difference's surface.
/// Returns that face's outward normal (from A toward B), its distance (the
/// core penetration) and the matching deepest point of A. `None` when the
/// cores are flat against each other and no volume can be built.
fn expand(
    support: &impl Fn(Vector3<f32>) -> SupportPoint,
    mut vertices: Vec<SupportPoint>,
) -> Option<(Vector3<f32>, f32, Vector3<f32>)> {
    // Grow a touching simplex into a tetrahedron around the origin.
    let directions = [
        Vector3::x(),
        Vector3::y(),
        Vector3::z(),
        -Vector3::x(),
        -Vector3::y(),
        -Vector3::z(),
    ];
    while vertices.len() < 4 {
        let points = vertices.iter().map(|v| v.point).collect::<Vec<_>>();
        let candidates: Vec<Vector3<f32>> = match points.len() {
            1 => directions.to_vec(),
            2 => directions
                .iter()
                .map(|axis| (points[1] - points[0]).cross(axis))
                .collect(),
            _ => {
                let normal =
                    (points[1] - points[0]).cross(&(points[2] - points[0]));
                vec![normal, -normal]
            }
        };
        let grows = |candidate: &SupportPoint| match points.len() {
            1 => (candidate.point - points[0]).norm() > 1e-5,
            2 => {
                (points[1] - points[0])
                    .cross(&(candidate.point - points[0]))
                    .norm()
                    > 1e-5
            }
            _ => {
                let normal =
                    (points[1] - points[0]).cross(&(points[2] - points[0]));
                (candidate.point - points[0]).dot(&normal).abs() > 1e-6
            }
        };
        let found = candidates
            .into_iter()
            .filter_map(|direction| direction.try_normalize(1e-9))
            .map(support)
            .find(grows)?;
        vertices.push(found);
    }
    vertices.truncate(4);
    let mut faces: Vec<[usize; 3]> = Vec::new();
    for [i, j, k, opposite] in
        [[0, 1, 2, 3], [0, 1, 3, 2], [0, 2, 3, 1], [1, 2, 3, 0]]
    {
        let [a, b, c, d] = [i, j, k, opposite].map(|n| vertices[n].point);
        faces.push(if (b - a).cross(&(c - a)).dot(&(d - a)) > 0.0 {
            [i, k, j]
        } else {
            [i, j, k]
        });
    }
    let plane = |vertices: &[SupportPoint], [i, j, k]: [usize; 3]| {
        let [a, b, c] = [i, j, k].map(|n| vertices[n].point);
        let normal = (b - a).cross(&(c - a)).try_normalize(1e-12)?;
        Some((normal, normal.dot(&a)))
    };
    for _ in 0..64 {
        let (index, normal, distance) = faces
            .iter()
            .enumerate()
            .filter_map(|(index, face)| {
                plane(&vertices, *face).map(|(n, d)| (index, n, d))
            })
            .min_by(|x, y| x.2.total_cmp(&y.2))?;
        let next = support(normal);
        if next.point.dot(&normal) - distance < 1e-4 {
            let [i, j, k] = faces[index];
            let [a, b, c] = [i, j, k].map(|n| vertices[n].point);
            let weights = triangle_weights(a, b, c);
            let from_a = [i, j, k]
                .iter()
                .zip(weights)
                .fold(Vector3::zeros(), |sum, (n, w)| {
                    sum + vertices[*n].from_a * w
                });
            return Some((normal, distance.max(0.0), from_a));
        }
        // Remove every face the new point sees and stitch the hole's rim
        // to it; rim edges are the ones only one removed face has.
        vertices.push(next);
        let new = vertices.len() - 1;
        let mut rim: Vec<[usize; 2]> = Vec::new();
        faces.retain(|&face| {
            let [a, ..] = face.map(|n| vertices[n].point);
            let visible = plane(&vertices, face)
                .is_some_and(|(n, _)| n.dot(&(next.point - a)) > 0.0);
            if visible {
                for edge in
                    [[face[0], face[1]], [face[1], face[2]], [face[2], face[0]]]
                {
                    if let Some(shared) =
                        rim.iter().position(|e| *e == [edge[1], edge[0]])
                    {
                        rim.swap_remove(shared);
                    } else {
                        rim.push(edge);
                    }
                }
            }
            !visible
        });
        faces.extend(rim.into_iter().map(|[a, b]| [a, b, new]));
    }
    None
}

fn closest_on_segment(
    start: Vector3<f32>,
    end: Vector3<f32>,
    point: Vector3<f32>,
) -> Vector3<f32> {
    let segment = end - start;
    let length = segment.norm_squared();
    if length < 1e-12 {
        return start;
    }
    let t = ((point - start).dot(&segment) / length).clamp(0.0, 1.0);
    start + segment * t
}

/// Closest points between two segments (Ericson, Real-Time Collision
/// Detection 5.1.9).
fn segments_closest(
    p1: Vector3<f32>,
    q1: Vector3<f32>,
    p2: Vector3<f32>,
    q2: Vector3<f32>,
) -> (Vector3<f32>, Vector3<f32>) {
    let (d1, d2, r) = (q1 - p1, q2 - p2, p1 - p2);
    let (a, e, f) = (d1.norm_squared(), d2.norm_squared(), d2.dot(&r));
    let (s, t) = if a < 1e-12 && e < 1e-12 {
        (0.0, 0.0)
    } else if a < 1e-12 {
        (0.0, (f / e).clamp(0.0, 1.0))
    } else {
        let c = d1.dot(&r);
        if e < 1e-12 {
            ((-c / a).clamp(0.0, 1.0), 0.0)
        } else {
            let b = d1.dot(&d2);
            let denominator = a * e - b * b;
            let mut s = if denominator > 1e-12 {
                ((b * f - c * e) / denominator).clamp(0.0, 1.0)
            } else {
                0.0
            };
            let mut t = (b * s + f) / e;
            if t < 0.0 {
                t = 0.0;
                s = (-c / a).clamp(0.0, 1.0);
            } else if t > 1.0 {
                t = 1.0;
                s = ((b - c) / a).clamp(0.0, 1.0);
            }
            (s, t)
        }
    };
    (p1 + d1 * s, p2 + d2 * t)
}

/// Distance along a unit ray and the surface normal there.
/// Box tree over items (bodies, or a mesh's triangles) for ray casts, so a
/// ray visits only the boxes it passes through instead of every item.
#[derive(Clone, Debug, Default)]
struct RayTree {
    nodes: Vec<RayNode>,
    /// Item indices, leaf after leaf.
    items: Vec<u32>,
}

#[derive(Clone, Copy, Debug)]
struct RayNode {
    min: Vector3<f32>,
    max: Vector3<f32>,
    /// A leaf's first entry in `items`, or an inner node's first child (the
    /// second follows it).
    first: u32,
    /// Items in a leaf; 0 for an inner node.
    count: u32,
}

/// Most items in a tree leaf.
const RAY_LEAF: usize = 4;

impl RayTree {
    /// A tree over items with these `[min, max]` boxes.
    fn new(bounds: &[[Vector3<f32>; 2]]) -> Self {
        let mut tree = Self {
            nodes: Vec::with_capacity(2 * bounds.len() / RAY_LEAF + 1),
            items: (0..bounds.len() as u32).collect(),
        };
        if !bounds.is_empty() {
            tree.nodes.push(RayNode {
                min: Vector3::zeros(),
                max: Vector3::zeros(),
                first: 0,
                count: 0,
            });
            tree.build(0, 0, bounds.len(), bounds);
        }
        tree
    }

    fn build(
        &mut self,
        node: usize,
        start: usize,
        end: usize,
        bounds: &[[Vector3<f32>; 2]],
    ) {
        // Padded so a hit on a box face never falls outside by rounding.
        let pad = Vector3::repeat(1e-4);
        let (min, max) = self.items[start..end].iter().fold(
            (
                Vector3::repeat(f32::INFINITY),
                Vector3::repeat(f32::NEG_INFINITY),
            ),
            |(min, max), &item| {
                let [low, high] = bounds[item as usize];
                (min.inf(&(low - pad)), max.sup(&(high + pad)))
            },
        );
        if end - start <= RAY_LEAF {
            self.nodes[node] = RayNode {
                min,
                max,
                first: start as u32,
                count: (end - start) as u32,
            };
            return;
        }
        // Halve the items at the median center on the longest axis.
        let axis = (max - min).imax();
        let middle = (start + end) / 2;
        let center = |item: u32| {
            let [low, high] = bounds[item as usize];
            low[axis] + high[axis]
        };
        self.items[start..end]
            .select_nth_unstable_by(middle - start, |a, b| {
                center(*a).total_cmp(&center(*b)).then(a.cmp(b))
            });
        let child = self.nodes.len();
        self.nodes.extend([self.nodes[node]; 2]);
        self.nodes[node] = RayNode {
            min,
            max,
            first: child as u32,
            count: 0,
        };
        self.build(child, start, middle, bounds);
        self.build(child + 1, middle, end, bounds);
    }

    /// The nearest hit within `max_distance` that `test` reports for an
    /// item whose box the ray (unit `direction`) passes through, as item,
    /// distance and `test`'s value. Ties go to the lowest item, the same
    /// answer a scan in item order gives.
    fn cast<T>(
        &self,
        origin: Vector3<f32>,
        direction: Vector3<f32>,
        max_distance: f32,
        mut test: impl FnMut(usize) -> Option<(f32, T)>,
    ) -> Option<(usize, f32, T)> {
        let inverse = direction.map(|d| 1.0 / d);
        let mut best: Option<(usize, f32, T)> = None;
        let mut stack = Vec::with_capacity(64);
        if !self.nodes.is_empty() {
            stack.push(0);
        }
        while let Some(index) = stack.pop() {
            let node = self.nodes[index];
            let limit = best.as_ref().map_or(max_distance, |hit| hit.1);
            if !ray_box(origin, inverse, node.min, node.max, limit) {
                continue;
            }
            let first = node.first as usize;
            if node.count == 0 {
                stack.extend([first + 1, first]);
                continue;
            }
            for &item in &self.items[first..first + node.count as usize] {
                let item = item as usize;
                let Some((distance, value)) = test(item) else {
                    continue;
                };
                let better = match &best {
                    None => distance <= max_distance,
                    Some((other, nearest, _)) => {
                        distance < *nearest
                            || (distance == *nearest && item < *other)
                    }
                };
                if better {
                    best = Some((item, distance, value));
                }
            }
        }
        best
    }
}

/// Whether a ray enters the box `[min, max]` between 0 and `limit`. A ray
/// lying in a box face counts as entering it.
fn ray_box(
    origin: Vector3<f32>,
    inverse: Vector3<f32>,
    min: Vector3<f32>,
    max: Vector3<f32>,
    limit: f32,
) -> bool {
    let (mut near, mut far) = (0.0_f32, limit);
    for axis in 0..3 {
        // A ray parallel to and on a slab gives NaN, which max and min
        // skip, keeping the box.
        let t1 = (min[axis] - origin[axis]) * inverse[axis];
        let t2 = (max[axis] - origin[axis]) * inverse[axis];
        near = near.max(t1.min(t2));
        far = far.min(t1.max(t2));
    }
    near <= far
}

fn ray_body(
    origin: Vector3<f32>,
    direction: Vector3<f32>,
    body: &Body,
) -> Option<(f32, Vector3<f32>)> {
    match body.shape {
        Shape::Sphere(radius) => {
            let t = ray_sphere(origin, direction, body.position, radius)?;
            let point = origin + direction * t;
            Some((t, (point - body.position) / radius))
        }
        Shape::Box(half) => {
            let inverse = body.rotation.inverse();
            let local_origin = inverse * (origin - body.position);
            let local_direction = inverse * direction;
            let (mut near, mut far) = (f32::NEG_INFINITY, f32::INFINITY);
            let mut normal = Vector3::zeros();
            for axis in 0..3 {
                let (o, d) = (local_origin[axis], local_direction[axis]);
                if d.abs() < 1e-8 {
                    if o.abs() > half[axis] {
                        return None;
                    }
                    continue;
                }
                let t1 = (-half[axis] - o) / d;
                let t2 = (half[axis] - o) / d;
                let (low, high) = if t1 < t2 { (t1, t2) } else { (t2, t1) };
                if low > near {
                    near = low;
                    normal = Vector3::zeros();
                    normal[axis] = -d.signum();
                }
                far = far.min(high);
            }
            (near >= 0.0 && near <= far).then(|| (near, body.rotation * normal))
        }
        Shape::Hull(ref mesh) | Shape::Triangles(ref mesh) => {
            let inverse = body.rotation.inverse();
            let local_origin = inverse * (origin - body.position);
            let local_direction = inverse * direction;
            // A hull is solid: only faces seen from outside stop the ray.
            let one_sided = matches!(body.shape, Shape::Hull(_));
            let (_, t, normal) = mesh.ray_tree.cast(
                local_origin,
                local_direction,
                f32::INFINITY,
                |index| {
                    ray_triangle(
                        local_origin,
                        local_direction,
                        &mesh.triangles[index],
                    )
                    .filter(|(_, normal)| {
                        !one_sided || normal.dot(&local_direction) < 0.0
                    })
                },
            )?;
            let normal = if normal.dot(&local_direction) > 0.0 {
                -normal
            } else {
                normal
            };
            Some((t, body.rotation * normal))
        }
        Shape::Compound(_) => body
            .parts()
            .iter()
            .filter_map(|part| ray_body(origin, direction, part))
            .min_by(|a, b| a.0.total_cmp(&b.0)),
        Shape::Capsule { .. } => {
            let (start, end, radius) = body.segment();
            let t = ray_capsule(origin, direction, start, end, radius)?;
            let point = origin + direction * t;
            let axis_point = closest_on_segment(start, end, point);
            Some((t, (point - axis_point) / radius))
        }
    }
}

/// Moller-Trumbore: distance along the ray and the triangle's unit normal
/// (by winding).
fn ray_triangle(
    origin: Vector3<f32>,
    direction: Vector3<f32>,
    [a, b, c]: &[Vector3<f32>; 3],
) -> Option<(f32, Vector3<f32>)> {
    let (ab, ac) = (b - a, c - a);
    let p = direction.cross(&ac);
    let determinant = ab.dot(&p);
    if determinant.abs() < 1e-12 {
        return None;
    }
    let offset = origin - a;
    let u = offset.dot(&p) / determinant;
    let q = offset.cross(&ab);
    let v = direction.dot(&q) / determinant;
    let t = ac.dot(&q) / determinant;
    (u >= 0.0 && v >= 0.0 && u + v <= 1.0 && t >= 0.0)
        .then(|| (t, ab.cross(&ac).normalize()))
}

fn ray_sphere(
    origin: Vector3<f32>,
    direction: Vector3<f32>,
    center: Vector3<f32>,
    radius: f32,
) -> Option<f32> {
    let offset = origin - center;
    let b = offset.dot(&direction);
    let h = b * b - (offset.norm_squared() - radius * radius);
    if h < 0.0 {
        return None;
    }
    let t = -b - h.sqrt();
    (t >= 0.0).then_some(t)
}

/// Ray against a capsule (after Inigo Quilez's `capIntersect`).
fn ray_capsule(
    origin: Vector3<f32>,
    direction: Vector3<f32>,
    start: Vector3<f32>,
    end: Vector3<f32>,
    radius: f32,
) -> Option<f32> {
    let (ba, oa) = (end - start, origin - start);
    let (baba, bard, baoa) =
        (ba.norm_squared(), ba.dot(&direction), ba.dot(&oa));
    let (rdoa, oaoa) = (direction.dot(&oa), oa.norm_squared());
    let a = baba - bard * bard;
    let b = baba * rdoa - baoa * bard;
    let c = baba * oaoa - baoa * baoa - radius * radius * baba;
    let h = b * b - a * c;
    if a > 1e-12 && h >= 0.0 {
        let t = (-b - h.sqrt()) / a;
        let y = baoa + t * bard;
        if y > 0.0 && y < baba {
            return (t >= 0.0).then_some(t);
        }
    }
    // Otherwise the ray can only enter through a cap.
    [start, end]
        .into_iter()
        .filter_map(|cap| ray_sphere(origin, direction, cap, radius))
        .min_by(f32::total_cmp)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn body(shape: Shape, position: [f32; 3]) -> Body {
        Body {
            entity: Entity::PLACEHOLDER,
            kind: RigidBodyKind::Dynamic,
            movable: true,
            position: position.into(),
            rotation: Rotation3::identity(),
            shape,
            sensor: false,
            proxy: false,
            layers: CollisionLayers::default(),
            friction: 0.5,
            restitution: 0.0,
            combine: [CombineMode::Default; 2],
            inverse_mass: 1.0,
            inverse_inertia: Vector3::repeat(1.0),
            asleep: false,
            articulated: false,
            locks: AxisLock::default(),
            velocity: Vector3::zeros(),
            angular_velocity: Vector3::zeros(),
        }
    }

    fn assert_near(actual: f32, expected: f32) {
        assert!((actual - expected).abs() < 1e-4, "{actual} != {expected}");
    }

    /// Points in [-1, 1) from a fixed seed, for test layouts.
    fn noise(seed: &mut u32) -> f32 {
        *seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        (*seed >> 8) as f32 / (1 << 23) as f32 - 1.0
    }

    /// Nearest hit by scanning every item, the way rays were cast before
    /// the ray trees.
    fn scan_ray(
        origin: Vector3<f32>,
        direction: Vector3<f32>,
        bodies: &[Body],
    ) -> Option<(usize, f32, Vector3<f32>)> {
        bodies
            .iter()
            .enumerate()
            .filter_map(|(index, body)| {
                let (distance, normal) = match &body.shape {
                    Shape::Triangles(mesh) => {
                        let (t, normal) = mesh
                            .triangles
                            .iter()
                            .filter_map(|triangle| {
                                ray_triangle(origin, direction, triangle)
                            })
                            .min_by(|a, b| a.0.total_cmp(&b.0))?;
                        let flip = normal.dot(&direction) > 0.0;
                        (t, if flip { -normal } else { normal })
                    }
                    _ => ray_body(origin, direction, body)?,
                };
                Some((index, distance, normal))
            })
            .min_by(|a, b| a.1.total_cmp(&b.1))
    }

    #[test]
    fn ray_trees_find_the_same_hits_as_scanning_every_item() {
        let mut seed = 7;
        // A bumpy 64 x 64 heightfield: 8,192 triangles.
        let heights = (0..65)
            .map(|_| (0..65).map(|_| noise(&mut seed) * 2.0).collect())
            .collect();
        let field = Heightfield {
            heights,
            spacing: 1.0,
            ..Heightfield::default()
        };
        let island = Shape::Triangles(Arc::new(MeshData::new(
            &field.mesh(),
            [1.0; 3],
            false,
        )));
        let mut bodies = vec![body(island, [0.0; 3])];
        // 500 spheres and boxes above it.
        for index in 0..500 {
            let at = [
                noise(&mut seed) * 30.0,
                3.0 + noise(&mut seed).abs() * 10.0,
                noise(&mut seed) * 30.0,
            ];
            let shape = if index % 2 == 0 {
                Shape::Sphere(0.5)
            } else {
                Shape::Box(Vector3::new(0.4, 0.3, 0.6))
            };
            bodies.push(body(shape, at));
        }
        for (index, body) in bodies.iter_mut().enumerate() {
            body.entity = Entity::from_raw_u32(index as u32).unwrap();
        }
        let mut world = PhysicsWorld::default();
        world.set_bodies(bodies.clone());
        let mut hits = 0;
        for _ in 0..400 {
            let origin = Vector3::new(
                noise(&mut seed) * 40.0,
                20.0,
                noise(&mut seed) * 40.0,
            );
            let direction = Vector3::new(
                noise(&mut seed),
                -1.0 - noise(&mut seed).abs(),
                noise(&mut seed),
            )
            .normalize();
            let actual =
                world.raycast(origin.into(), direction.into(), 60.0, u32::MAX);
            // `raycast` normalizes again, which can move the last bit.
            let direction = direction.try_normalize(1e-6).unwrap();
            let expected = scan_ray(origin, direction, &bodies)
                .filter(|hit| hit.1 <= 60.0);
            match (expected, actual) {
                (None, None) => {}
                (Some((index, distance, normal)), Some(hit)) => {
                    hits += 1;
                    assert_eq!(hit.entity, bodies[index].entity);
                    assert_eq!(hit.distance, distance);
                    assert_eq!(hit.normal, <[f32; 3]>::from(normal));
                }
                (expected, actual) => panic!("{expected:?} vs {actual:?}"),
            }
        }
        assert!(hits > 200, "{hits}");
    }

    #[test]
    fn pair_tests_report_normal_from_a_to_b_and_depth() {
        let unit_box = Shape::Box(Vector3::repeat(0.5));
        let cases = [
            (
                Shape::Sphere(1.0),
                [0.0; 3],
                Shape::Sphere(1.0),
                [1.5, 0.0, 0.0],
                [1.0, 0.0, 0.0],
                0.5,
            ),
            (
                Shape::Sphere(0.5),
                [0.0, 0.9, 0.0],
                unit_box.clone(),
                [0.0; 3],
                [0.0, -1.0, 0.0],
                0.1,
            ),
            (
                unit_box.clone(),
                [0.0; 3],
                unit_box.clone(),
                [0.0, 0.0, 0.8],
                [0.0, 0.0, 1.0],
                0.2,
            ),
            (
                Shape::Capsule {
                    half_height: 1.0,
                    radius: 0.5,
                },
                [0.0; 3],
                Shape::Sphere(0.5),
                [0.9, 0.8, 0.0],
                [1.0, 0.0, 0.0],
                0.1,
            ),
            (
                unit_box.clone(),
                [0.0; 3],
                Shape::Capsule {
                    half_height: 1.0,
                    radius: 0.25,
                },
                [0.0, 1.5, 0.0],
                [0.0, 1.0, 0.0],
                0.25,
            ),
        ];
        for (shape_a, at_a, shape_b, at_b, normal, depth) in cases {
            let contact = collide(
                &body(shape_a.clone(), at_a),
                &body(shape_b.clone(), at_b),
            )
            .unwrap_or_else(|| panic!("{shape_a:?} and {shape_b:?} touch"));
            for (got, want) in contact.normal.iter().zip(normal) {
                assert_near(*got, want);
            }
            assert_near(contact.depth, depth);
        }
        assert!(collide(
            &body(unit_box.clone(), [0.0; 3]),
            &body(unit_box.clone(), [1.1, 0.0, 0.0])
        )
        .is_none());
    }

    #[test]
    fn broad_phase_finds_the_same_pairs_as_testing_every_pair() {
        // Deterministic scatter; some bodies overlap, most do not.
        let mut seed = 7_u32;
        let mut next = || {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (seed >> 8) as f32 / (1 << 24) as f32 * 20.0 - 10.0
        };
        let bodies = (0..200)
            .map(|index| {
                let shape = match index % 3 {
                    0 => Shape::Sphere(0.6),
                    1 => Shape::Box(Vector3::new(0.5, 0.7, 0.4)),
                    _ => Shape::Capsule {
                        half_height: 0.5,
                        radius: 0.3,
                    },
                };
                body(shape, [next(), next() * 0.2, next() * 0.2])
            })
            .collect::<Vec<_>>();
        let mut brute = Vec::new();
        for a in 0..bodies.len() {
            for b in a + 1..bodies.len() {
                if narrow_phase(&bodies[a], &bodies[b]).is_some() {
                    brute.push((a, b));
                }
            }
        }
        let swept = find_contacts(&bodies)
            .into_iter()
            .map(|(a, b, _)| (a, b))
            .collect::<Vec<_>>();
        assert!(brute.len() > 10, "scatter too sparse: {}", brute.len());
        assert_eq!(swept, brute);
    }

    #[test]
    fn broad_phase_prunes_a_tall_column_without_changing_pair_order() {
        let mut bodies = (0..200)
            .map(|index| {
                body(Shape::Sphere(0.6), [0.0, index as f32 * 4.0, 0.0])
            })
            .collect::<Vec<_>>();
        bodies[80].position.y = bodies[79].position.y + 0.8;

        assert_eq!(broad_phase_candidates(&bodies), vec![(79, 80)]);
        let contacts = find_contacts(&bodies);
        assert_eq!(contacts.len(), 1);
        assert_eq!((contacts[0].0, contacts[0].1), (79, 80));
    }

    #[test]
    fn rotated_boxes_separate_on_an_edge_axis() {
        let unit_box = Shape::Box(Vector3::repeat(0.5));
        let mut turned = body(unit_box.clone(), [1.2, 0.0, 0.0]);
        turned.rotation =
            Rotation3::from_euler_angles(0.0, 0.0, std::f32::consts::FRAC_PI_4);
        // The turned box reaches sqrt(0.5) = 0.707 along X: 0.5 + 0.707 > 1.2.
        let contact =
            collide(&body(unit_box.clone(), [0.0; 3]), &turned).unwrap();
        assert_near(contact.normal[0], 1.0);
        assert_near(contact.depth, 0.5 + 0.5_f32.sqrt() - 1.2);
        turned.position.x = 1.25;
        assert!(collide(&body(unit_box.clone(), [0.0; 3]), &turned).is_none());
    }

    #[test]
    fn rays_hit_each_shape_on_its_surface() {
        let world = PhysicsWorld {
            bodies: vec![
                body(Shape::Sphere(1.0), [0.0, 0.0, -5.0]),
                body(Shape::Box(Vector3::repeat(0.5)), [0.0, 0.0, -2.0]),
                body(
                    Shape::Capsule {
                        half_height: 1.0,
                        radius: 0.5,
                    },
                    [3.0, 0.0, 0.0],
                ),
            ],
            contacts: Vec::new(),
            impacts: Vec::new(),
            rest: HashMap::new(),
            warm: HashMap::new(),
            joint_warm: HashMap::new(),
            meshes: HashMap::new(),
            ray_tree: OnceLock::new(),
        };
        let hit = world
            .raycast([0.0; 3], [0.0, 0.0, -1.0], 100.0, u32::MAX)
            .unwrap();
        assert_near(hit.distance, 1.5);
        assert_eq!(hit.normal, [0.0, 0.0, 1.0]);
        let hit = world
            .raycast([3.0, 5.0, 0.0], [0.0, -1.0, 0.0], 100.0, u32::MAX)
            .unwrap();
        assert_near(hit.distance, 3.5);
        assert_near(hit.normal[1], 1.0);
        let hit = world
            .raycast([0.0; 3], [1.0, 0.0, 0.0], 100.0, u32::MAX)
            .unwrap();
        assert_near(hit.distance, 2.5);
        assert!(world
            .raycast([0.0; 3], [1.0, 0.0, 0.0], 2.0, u32::MAX)
            .is_none());
        assert!(world
            .raycast([0.0; 3], [0.0, 1.0, 0.0], 100.0, u32::MAX)
            .is_none());
        assert_eq!(
            world.overlap_sphere([0.0, 0.0, -2.7], 0.3, u32::MAX).len(),
            1
        );
    }

    /// A closed cube mesh of half size `half`, with mixed winding (the
    /// hull must orient its faces itself).
    fn cube_mesh(half: f32) -> MeshAsset {
        let vertices = (0..8)
            .map(|corner| crate::assets::MeshVertex {
                position: [0, 1, 2].map(|axis| {
                    if corner >> axis & 1 == 1 {
                        half
                    } else {
                        -half
                    }
                }),
                ..Default::default()
            })
            .collect();
        let indices = vec![
            0, 1, 3, 0, 3, 2, 4, 5, 7, 4, 7, 6, 0, 1, 5, 0, 5, 4, 2, 3, 7, 2,
            7, 6, 0, 2, 6, 0, 6, 4, 1, 3, 7, 1, 7, 5,
        ];
        MeshAsset { vertices, indices }
    }

    /// A 20 x 20 floor at y = 0 made of two triangles.
    fn floor_mesh() -> MeshAsset {
        let vertices =
            [[-10.0, -10.0], [10.0, -10.0], [10.0, 10.0], [-10.0, 10.0]]
                .map(|[x, z]| crate::assets::MeshVertex {
                    position: [x, 0.0, z],
                    ..Default::default()
                })
                .to_vec();
        MeshAsset {
            vertices,
            indices: vec![0, 2, 1, 0, 3, 2],
        }
    }

    #[test]
    fn mesh_colliders_match_primitives_and_report_normals_from_a_to_b() {
        let hull = || {
            Shape::Hull(Arc::new(MeshData::new(
                &cube_mesh(0.5),
                [1.0; 3],
                true,
            )))
        };
        let floor = Shape::Triangles(Arc::new(MeshData::new(
            &floor_mesh(),
            [1.0; 3],
            false,
        )));
        let unit_box = Shape::Box(Vector3::repeat(0.5));
        let capsule = Shape::Capsule {
            half_height: 0.5,
            radius: 0.25,
        };
        // (a, b, normal a to b, depth); each hull case matches its box case.
        let cases = [
            (
                hull(),
                Shape::Sphere(0.5),
                [0.0, 0.9, 0.0],
                [0.0, 1.0, 0.0],
                0.1,
            ),
            (
                unit_box.clone(),
                Shape::Sphere(0.5),
                [0.0, 0.9, 0.0],
                [0.0, 1.0, 0.0],
                0.1,
            ),
            (hull(), hull(), [0.8, 0.0, 0.0], [1.0, 0.0, 0.0], 0.2),
            (
                hull(),
                unit_box.clone(),
                [0.0, 0.0, -0.7],
                [0.0, 0.0, -1.0],
                0.3,
            ),
            (
                hull(),
                Shape::Sphere(0.5),
                [0.0, 0.3, 0.0],
                [0.0, 1.0, 0.0],
                0.7,
            ),
            (
                hull(),
                capsule.clone(),
                [0.0, 1.2, 0.0],
                [0.0, 1.0, 0.0],
                0.05,
            ),
            (
                Shape::Sphere(0.5),
                floor.clone(),
                [0.0, -0.4, 0.0],
                [0.0, -1.0, 0.0],
                0.1,
            ),
            (
                floor.clone(),
                hull(),
                [3.0, 0.45, 2.0],
                [0.0, 1.0, 0.0],
                0.05,
            ),
        ];
        for (a, b, offset, normal, depth) in cases {
            let label = format!("{a:?} vs {b:?}");
            let contact = collide(&body(a, [0.0; 3]), &body(b, offset))
                .unwrap_or_else(|| panic!("{label} misses"));
            assert!(
                (Vector3::from(contact.normal) - Vector3::from(normal)).norm()
                    < 1e-3,
                "{label}: normal {:?}",
                contact.normal
            );
            assert!(
                (contact.depth - depth).abs() < 1e-3,
                "{label}: depth {}",
                contact.depth
            );
        }
        assert!(collide(
            &body(hull(), [0.0; 3]),
            &body(hull(), [1.1, 0.0, 0.0])
        )
        .is_none());
        assert!(collide(
            &body(Shape::Sphere(0.5), [0.0, 0.6, 0.0]),
            &body(floor.clone(), [0.0; 3])
        )
        .is_none());
        assert!(collide(
            &body(floor.clone(), [0.0; 3]),
            &body(floor, [0.0; 3])
        )
        .is_none());
    }

    #[test]
    fn rays_hit_hulls_from_outside_and_triangle_meshes_from_either_side() {
        let world = PhysicsWorld {
            bodies: vec![
                body(
                    Shape::Hull(Arc::new(MeshData::new(
                        &cube_mesh(0.5),
                        [1.0; 3],
                        true,
                    ))),
                    [0.0, 0.0, -3.0],
                ),
                body(
                    Shape::Triangles(Arc::new(MeshData::new(
                        &floor_mesh(),
                        [1.0; 3],
                        false,
                    ))),
                    [0.0, -1.0, 0.0],
                ),
            ],
            ..PhysicsWorld::default()
        };
        let hit = world
            .raycast([0.0; 3], [0.0, 0.0, -1.0], 100.0, u32::MAX)
            .unwrap();
        assert_near(hit.distance, 2.5);
        assert_eq!(hit.normal, [0.0, 0.0, 1.0]);
        // A ray starting inside the hull does not hit it.
        assert!(world
            .raycast([0.0, 0.0, -3.0], [1.0, 0.0, 0.0], 100.0, u32::MAX)
            .is_none());
        for (origin, direction, normal) in [(3.0, -1.0, 1.0), (-3.0, 1.0, -1.0)]
        {
            let hit = world
                .raycast(
                    [1.0, origin, 1.0],
                    [0.0, direction, 0.0],
                    100.0,
                    u32::MAX,
                )
                .unwrap();
            assert_near(hit.distance, (origin + 1.0).abs());
            assert_near(hit.normal[1], normal);
        }
    }

    #[test]
    fn segment_closest_points_handle_crossing_and_parallel_segments() {
        let (a, b) = segments_closest(
            Vector3::new(-1.0, 0.0, 0.0),
            Vector3::new(1.0, 0.0, 0.0),
            Vector3::new(0.0, -1.0, 1.0),
            Vector3::new(0.0, 1.0, 1.0),
        );
        assert_eq!((a, b), (Vector3::zeros(), Vector3::new(0.0, 0.0, 1.0)));
        let (a, b) = segments_closest(
            Vector3::zeros(),
            Vector3::new(1.0, 0.0, 0.0),
            Vector3::new(0.0, 2.0, 0.0),
            Vector3::new(1.0, 2.0, 0.0),
        );
        assert_near((a - b).norm(), 2.0);
    }

    #[test]
    fn foot_ik_stands_on_the_ground_and_skips_its_own_colliders() {
        use crate::runtime::GlobalTransform;
        use crate::runtime::{
            propagate_transforms, Animation, Children, Ik, IkKind, Parent,
        };
        use crate::Transform;
        let mut world = World::new();
        let mut chain = vec![world
            .spawn((Transform::default(), Animation::default()))
            .id()];
        for position in [[0.3, 1.0, 0.0], [0.0, -0.5, 0.05], [0.0, -0.5, -0.05]]
        {
            let parent = *chain.last().unwrap();
            let child = world
                .spawn((
                    Transform {
                        position,
                        ..Transform::default()
                    },
                    Parent(parent),
                ))
                .id();
            world.entity_mut(parent).insert(Children(vec![child]));
            chain.push(child);
        }
        let foot = chain[3];
        world.entity_mut(foot).insert(Ik {
            kind: IkKind::Foot,
            ..Ik::default()
        });
        // A collider on the foot sits between the ray start and the step,
        // whose top is at 0.2.
        let mut shoe =
            body(Shape::Box(Vector3::repeat(0.05)), [0.3, 0.35, 0.0]);
        shoe.entity = foot;
        let mut step = body(Shape::Box(Vector3::repeat(0.5)), [0.3, -0.3, 0.0]);
        step.entity = world.spawn_empty().id();
        world.insert_resource(PhysicsWorld {
            bodies: vec![shoe, step],
            contacts: Vec::new(),
            impacts: Vec::new(),
            rest: HashMap::new(),
            warm: HashMap::new(),
            joint_warm: HashMap::new(),
            meshes: HashMap::new(),
            ray_tree: OnceLock::new(),
        });
        crate::runtime::solve_ik(&mut world);
        propagate_transforms(&mut world);
        let at = world.get::<GlobalTransform>(foot).unwrap().matrix[3];
        assert_near(at[1], 0.2);
        assert!((at[0] - 0.3).abs() < 1e-3 && at[2].abs() < 1e-3, "{at:?}");
    }

    #[test]
    fn foot_ik_lowers_the_hips_tilts_to_the_slope_and_does_not_pile_up() {
        use crate::runtime::{
            propagate_transforms, Animation, Children, GlobalTransform, Ik,
            IkKind, Parent,
        };
        use crate::Transform;
        let mut world = World::new();
        let mut chain = vec![world
            .spawn((Transform::default(), Animation::default()))
            .id()];
        for position in [
            [0.0, 1.0, 0.0],
            [0.3, 0.0, 0.0],
            [0.0, -0.5, 0.05],
            [0.0, -0.5, -0.05],
        ] {
            let parent = *chain.last().unwrap();
            let child = world
                .spawn((
                    Transform {
                        position,
                        ..Transform::default()
                    },
                    Parent(parent),
                ))
                .id();
            world.entity_mut(parent).insert(Children(vec![child]));
            chain.push(child);
        }
        let (hips, foot) = (chain[1], chain[4]);
        world.entity_mut(foot).insert(Ik {
            kind: IkKind::Foot,
            ..Ik::default()
        });
        // A slope tilted 0.3 rad about Z, 0.28 m below the floor under the
        // foot: out of the straight leg's reach without lowering the hips.
        let mut slope =
            body(Shape::Box(Vector3::repeat(0.5)), [0.3, -0.8, 0.0]);
        slope.rotation = Rotation3::from_axis_angle(&Vector3::z_axis(), 0.3);
        slope.entity = world.spawn_empty().id();
        let normal = slope.rotation * Vector3::y();
        world.insert_resource(PhysicsWorld {
            bodies: vec![slope],
            contacts: Vec::new(),
            impacts: Vec::new(),
            rest: HashMap::new(),
            warm: HashMap::new(),
            joint_warm: HashMap::new(),
            meshes: HashMap::new(),
            ray_tree: OnceLock::new(),
        });
        let ground = world
            .resource::<PhysicsWorld>()
            .raycast([0.3, 0.5, 0.0], [0.0, -1.0, 0.0], 1.0, u32::MAX)
            .unwrap()
            .point[1];
        let mut poses = Vec::new();
        for _ in 0..3 {
            crate::runtime::solve_ik(&mut world);
            propagate_transforms(&mut world);
            let model = world.get::<GlobalTransform>(foot).unwrap().matrix;
            assert_near(model[3][1], ground);
            let up = Vector3::new(model[1][0], model[1][1], model[1][2]);
            assert!(up.normalize().dot(&normal) > 0.9999, "{up:?}");
            poses.push((
                *world.get::<Transform>(hips).unwrap(),
                *world.get::<Transform>(foot).unwrap(),
            ));
        }
        assert!(poses[0].0.position[1] < 0.75, "hips lowered");
        assert_eq!(poses[0], poses[2], "no pile-up across ticks");
    }

    /// A CPU ball at rest after one zero-gravity tick next to a GPU ball
    /// whose read-back state, `age` ticks old, has it moving into the CPU
    /// ball: the CPU ball's velocity, and whether the GPU ball's authored
    /// pose and velocity stayed.
    fn hit_by_gpu_ball(age: u64) -> ([f32; 3], bool) {
        use crate::runtime::{App, GpuStateMirror, PhysicsId};
        let mut app = App::new();
        app.world_mut().resource_mut::<PhysicsSettings>().gravity = [0.0; 3];
        app.world_mut().resource_mut::<FrameTime>().fixed_tick = 20;
        let ball = |position: [f32; 3], simulation| {
            (
                Transform {
                    position,
                    ..Transform::default()
                },
                Collider {
                    shape: ColliderShape::Sphere { radius: 0.5 },
                    ..Collider::default()
                },
                RigidBody::default(),
                PhysicsBody {
                    simulation,
                    ..PhysicsBody::default()
                },
            )
        };
        let cpu = app
            .world_mut()
            .spawn(ball([0.0; 3], SimulationClass::Cpu))
            .id();
        // Authored far off; the GPU has since moved it against the CPU ball.
        let authored = ball([-5.0, 0.0, 0.0], SimulationClass::Gpu);
        let gpu = app
            .world_mut()
            .spawn((
                authored.clone(),
                PhysicsId {
                    slot: 1,
                    generation: 1,
                },
                GpuStateMirror {
                    tick: 20 - age,
                    transform: Transform {
                        position: [-0.95, 0.0, 0.0],
                        ..Transform::default()
                    },
                    linear_velocity: [3.0, 0.0, 0.0],
                    angular_velocity: [0.0; 3],
                    custom_values: None,
                },
            ))
            .id();
        app.update(std::time::Duration::from_secs_f64(1.0 / 60.0))
            .unwrap();
        let world = app.world();
        let kept = world.get::<Transform>(gpu) == Some(&authored.0)
            && world.get::<RigidBody>(gpu) == Some(&authored.2);
        (world.get::<RigidBody>(cpu).unwrap().linear_velocity, kept)
    }

    #[test]
    fn a_cpu_body_is_pushed_by_a_gpu_body_at_its_read_back_state() {
        let (pushed, kept) = hit_by_gpu_ball(1);
        assert!(kept, "the GPU body's authored state was overwritten");
        assert!(pushed[0] > 0.5, "{pushed:?}");
        let stale = crate::runtime::MAX_COUPLED_STATE_AGE + 1;
        let (still, kept) = hit_by_gpu_ball(stale);
        assert!(kept);
        assert_eq!(still, [0.0; 3]);
    }
}
