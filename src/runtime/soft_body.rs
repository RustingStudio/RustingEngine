//! Deterministic XPBD soft bodies from tetrahedral meshes (Macklin, Müller
//! and Chentanez 2016) on the CPU.
//!
//! Each tetrahedron edge is a distance constraint and each tetrahedron a
//! volume constraint, both with XPBD compliance. Steps are split into small
//! substeps with one constraint pass each, so no Lagrange multipliers are
//! kept between passes. Constraints are solved Gauss-Seidel in ascending
//! index order, nothing is accumulated through atomics and no randomness is
//! used: the same mesh and settings give the same bits. It is independent of
//! the ECS; [`SoftBodyVolume`] puts it on an entity and attaches it to rigid
//! bodies and keeps it out of their sphere, box and capsule colliders.
//! Overstretched edges tear by removing the tetrahedra around them.

// The particle loops index several parallel arrays at once.
#![allow(clippy::needless_range_loop)]

use nalgebra::Vector3;

/// Most substeps one step runs, whatever the settings ask.
pub const MAX_SOFT_BODY_SUBSTEPS: u32 = 64;

/// Solver settings. Lengths are meters and times seconds.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SoftBodySettings {
    pub gravity: [f32; 3],
    /// Substeps per step, up to [`MAX_SOFT_BODY_SUBSTEPS`]. More is stiffer
    /// and slower.
    pub substeps: u32,
    /// Edge compliance (inverse stiffness). 0 keeps edges rigid.
    pub edge_compliance: f32,
    /// Volume compliance. 0 keeps every tetrahedron at its rest volume.
    pub volume_compliance: f32,
    /// Height of a ground plane, if any. Particles below it are lifted onto
    /// it and lose their sideways motion (static friction).
    pub floor: Option<f32>,
    /// Fraction of velocity lost per second, so wobbles die out.
    pub damping: f32,
    /// Strain past which an edge tears: an edge stretched beyond
    /// `rest * (1 + tear_strain)` at the end of a step removes every
    /// tetrahedron that holds it. `None` never tears.
    pub tear_strain: Option<f32>,
}

impl Default for SoftBodySettings {
    fn default() -> Self {
        Self {
            gravity: [0.0, -9.81, 0.0],
            substeps: 10,
            edge_compliance: 0.0,
            volume_compliance: 0.0,
            floor: Some(0.0),
            damping: 1.0,
            tear_strain: None,
        }
    }
}

/// A rigid body that soft-body particles hang on, for
/// [`SoftBody::step_coupled`]. The step moves and turns it with the pull of
/// its particles; inverse mass and inertia 0 make it immovable (kinematic).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AnchorBody {
    pub position: [f32; 3],
    pub rotation: nalgebra::Rotation3<f32>,
    pub velocity: [f32; 3],
    pub angular_velocity: [f32; 3],
    pub inverse_mass: f32,
    /// World-space inverse inertia, held fixed through the step.
    pub inverse_inertia: nalgebra::Matrix3<f32>,
}

/// Holds one particle on a point fixed to an [`AnchorBody`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Anchor {
    pub particle: usize,
    /// Index into the bodies slice.
    pub body: usize,
    /// The point in the body's frame.
    pub local: [f32; 3],
}

/// A collider shape particles cannot enter, in its body's frame and already
/// scaled.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ObstacleShape {
    Sphere(f32),
    Box([f32; 3]),
    /// Along the body's Y axis, like capsule colliders.
    Capsule {
        half_height: f32,
        radius: f32,
    },
}

impl ObstacleShape {
    /// The shape of a sphere, box or capsule collider scaled like rigid
    /// physics scales it; `None` for mesh, heightfield and 2D colliders.
    pub fn from_collider(
        shape: super::ColliderShape,
        scale: [f32; 3],
    ) -> Option<Self> {
        let scale = vector(scale).abs();
        match shape {
            super::ColliderShape::Sphere { radius } => {
                Some(Self::Sphere(radius * scale.max()))
            }
            super::ColliderShape::Box { half_extents } => Some(Self::Box(
                vector(half_extents).component_mul(&scale).into(),
            )),
            super::ColliderShape::Capsule {
                half_height,
                radius,
            } => Some(Self::Capsule {
                half_height: half_height * scale.y,
                radius: radius * scale.x.max(scale.z),
            }),
            _ => None,
        }
    }

    /// Radius of a sphere around the body's center that holds the shape.
    pub fn bounding_radius(&self) -> f32 {
        match *self {
            Self::Sphere(radius) => radius,
            Self::Box(half) => vector(half).norm(),
            Self::Capsule {
                half_height,
                radius,
            } => half_height + radius,
        }
    }

    /// For a point inside the shape (body frame), the outward direction to
    /// the nearest surface and how far it is.
    fn push_out(&self, point: Vector3<f32>) -> Option<(Vector3<f32>, f32)> {
        let (from, radius) = match *self {
            Self::Box(half) => {
                let depths = vector(half) - point.abs();
                if depths.min() <= 0.0 {
                    return None;
                }
                let axis = depths.imin();
                let mut normal = Vector3::zeros();
                normal[axis] = if point[axis] < 0.0 { -1.0 } else { 1.0 };
                return Some((normal, depths[axis]));
            }
            Self::Sphere(radius) => (Vector3::zeros(), radius),
            Self::Capsule {
                half_height,
                radius,
            } => (
                Vector3::new(
                    0.0,
                    point.y.clamp(-half_height, half_height),
                    0.0,
                ),
                radius,
            ),
        };
        let offset = point - from;
        let distance = offset.norm();
        if distance >= radius {
            return None;
        }
        // A point right on the core goes up.
        let normal = if distance > 1e-9 {
            offset / distance
        } else {
            Vector3::y()
        };
        Some((normal, radius - distance))
    }
}

/// An [`ObstacleShape`] carried by one of the bodies of
/// [`SoftBody::step_coupled`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Obstacle {
    /// Index into the bodies slice.
    pub body: usize,
    pub shape: ObstacleShape,
}

/// A tetrahedral soft body: particles, their tetrahedra and the edges
/// between them, with rest lengths and volumes taken at creation.
#[derive(Clone, Debug, PartialEq)]
pub struct SoftBody {
    pub positions: Vec<[f32; 3]>,
    pub velocities: Vec<[f32; 3]>,
    /// 0 for pinned particles, which never move.
    pub inverse_masses: Vec<f32>,
    pub tetrahedra: Vec<[u32; 4]>,
    pub edges: Vec<[u32; 2]>,
    rest_lengths: Vec<f32>,
    rest_volumes: Vec<f32>,
}

pub(super) fn vector(value: [f32; 3]) -> Vector3<f32> {
    Vector3::from(value)
}

fn tetrahedron_volume(positions: &[[f32; 3]], tet: [u32; 4]) -> f32 {
    let [a, b, c, d] = tet.map(|index| vector(positions[index as usize]));
    (b - a).cross(&(c - a)).dot(&(d - a)) / 6.0
}

/// The six edges of a tetrahedron, each with its smaller index first.
fn tet_edges(tet: [u32; 4]) -> [[u32; 2]; 6] {
    [(0, 1), (0, 2), (0, 3), (1, 2), (1, 3), (2, 3)]
        .map(|(a, b)| [tet[a].min(tet[b]), tet[a].max(tet[b])])
}

impl SoftBody {
    /// Shortest rest edge, 0 with no edges: the lattice spacing of a block.
    #[must_use]
    pub fn min_rest_length(&self) -> f32 {
        self.rest_lengths
            .iter()
            .copied()
            .reduce(f32::min)
            .unwrap_or(0.0)
    }

    /// Builds a body from particle positions and tetrahedra. Each
    /// tetrahedron's mass (`density` times its volume) is shared equally by
    /// its four particles. Inside-out tetrahedra are turned the right way.
    /// Refuses indices out of range and flat tetrahedra.
    pub fn new(
        positions: Vec<[f32; 3]>,
        mut tetrahedra: Vec<[u32; 4]>,
        density: f32,
    ) -> Result<Self, String> {
        let mut masses = vec![0.0_f32; positions.len()];
        let mut rest_volumes = Vec::with_capacity(tetrahedra.len());
        let mut edges = Vec::new();
        for (index, tet) in tetrahedra.iter_mut().enumerate() {
            if tet.iter().any(|&p| p as usize >= positions.len()) {
                return Err(format!(
                    "tetrahedron {index} has no such particle"
                ));
            }
            let mut volume = tetrahedron_volume(&positions, *tet);
            if volume < 0.0 {
                tet.swap(2, 3);
                volume = -volume;
            }
            if volume <= 1e-12 {
                return Err(format!("tetrahedron {index} is flat"));
            }
            rest_volumes.push(volume);
            for &particle in tet.iter() {
                masses[particle as usize] += density * volume / 4.0;
            }
            edges.extend(tet_edges(*tet));
        }
        // Shared edges appear once, in a fixed (sorted) order.
        edges.sort_unstable();
        edges.dedup();
        let rest_lengths = edges
            .iter()
            .map(|&[a, b]| {
                (vector(positions[a as usize]) - vector(positions[b as usize]))
                    .norm()
            })
            .collect();
        let inverse_masses = masses
            .iter()
            .map(|&mass| if mass > 0.0 { 1.0 / mass } else { 0.0 })
            .collect();
        Ok(Self {
            velocities: vec![[0.0; 3]; positions.len()],
            positions,
            inverse_masses,
            tetrahedra,
            edges,
            rest_lengths,
            rest_volumes,
        })
    }

    /// A box of `cells` cubes of side `spacing` with its minimum corner at
    /// `min`, each cube cut into six tetrahedra around its main diagonal
    /// (the Kuhn split, so neighbor cubes share faces exactly).
    pub fn block(
        min: [f32; 3],
        cells: [u32; 3],
        spacing: f32,
        density: f32,
    ) -> Result<Self, String> {
        let [nx, ny, nz] = cells.map(|count| count + 1);
        let mut positions = Vec::with_capacity((nx * ny * nz) as usize);
        for z in 0..nz {
            for y in 0..ny {
                for x in 0..nx {
                    positions.push([
                        min[0] + x as f32 * spacing,
                        min[1] + y as f32 * spacing,
                        min[2] + z as f32 * spacing,
                    ]);
                }
            }
        }
        let index = |x: u32, y: u32, z: u32| x + nx * (y + ny * z);
        let mut tetrahedra = Vec::new();
        for z in 0..cells[2] {
            for y in 0..cells[1] {
                for x in 0..cells[0] {
                    let corner =
                        |c: [u32; 3]| index(x + c[0], y + c[1], z + c[2]);
                    for order in [
                        [0, 1, 2],
                        [0, 2, 1],
                        [1, 0, 2],
                        [1, 2, 0],
                        [2, 0, 1],
                        [2, 1, 0],
                    ] {
                        let mut step = [0u32; 3];
                        let mut tet = [corner(step); 4];
                        for (slot, axis) in order.into_iter().enumerate() {
                            step[axis] = 1;
                            tet[slot + 1] = corner(step);
                        }
                        tetrahedra.push(tet);
                    }
                }
            }
        }
        Self::new(positions, tetrahedra, density)
    }

    /// Pins a particle where it is: it keeps its place whatever pulls on it.
    pub fn pin(&mut self, particle: usize) {
        self.inverse_masses[particle] = 0.0;
        self.velocities[particle] = [0.0; 3];
    }

    /// Total volume of all tetrahedra.
    pub fn volume(&self) -> f32 {
        self.tetrahedra
            .iter()
            .map(|&tet| tetrahedron_volume(&self.positions, tet))
            .sum()
    }

    /// Total volume at rest.
    pub fn rest_volume(&self) -> f32 {
        self.rest_volumes.iter().sum()
    }

    /// Removes every tetrahedron holding an edge stretched past
    /// `rest * (1 + strain)`, then the edges no tetrahedron holds any more.
    /// Particles keep their mass, so loose ones fly on as free points.
    /// Returns how many tetrahedra were removed.
    pub fn tear(&mut self, strain: f32) -> usize {
        let torn: Vec<[u32; 2]> = self
            .edges
            .iter()
            .zip(&self.rest_lengths)
            .filter(|&(&[a, b], &rest)| {
                let length = (vector(self.positions[a as usize])
                    - vector(self.positions[b as usize]))
                .norm();
                length > rest * (1.0 + strain)
            })
            .map(|(&edge, _)| edge)
            .collect();
        if torn.is_empty() {
            return 0;
        }
        let before = self.tetrahedra.len();
        let mut kept = 0;
        for index in 0..before {
            let tet = self.tetrahedra[index];
            let holds_torn = tet_edges(tet)
                .iter()
                .any(|edge| torn.binary_search(edge).is_ok());
            if !holds_torn {
                self.tetrahedra[kept] = tet;
                self.rest_volumes[kept] = self.rest_volumes[index];
                kept += 1;
            }
        }
        self.tetrahedra.truncate(kept);
        self.rest_volumes.truncate(kept);
        let mut held: Vec<[u32; 2]> = self
            .tetrahedra
            .iter()
            .flat_map(|&tet| tet_edges(tet))
            .collect();
        held.sort_unstable();
        held.dedup();
        let mut kept = 0;
        for index in 0..self.edges.len() {
            if held.binary_search(&self.edges[index]).is_ok() {
                self.edges[kept] = self.edges[index];
                self.rest_lengths[kept] = self.rest_lengths[index];
                kept += 1;
            }
        }
        self.edges.truncate(kept);
        self.rest_lengths.truncate(kept);
        before - self.tetrahedra.len()
    }

    /// Advances the body by `dt` seconds.
    pub fn step(&mut self, settings: &SoftBodySettings, dt: f32) {
        self.step_coupled(settings, dt, &mut [], &[], &[]);
    }

    /// Advances the body by `dt` seconds with each anchor's particle held on
    /// its point of a rigid body. Each substep solves the anchors as
    /// zero-length XPBD constraints between particle and body (Müller et
    /// al. 2020, "Detailed rigid body simulation with extended position
    /// based dynamics"), so the body is pulled and turned by the soft body
    /// as well as carrying it. Bodies move with their velocities through
    /// the step and come back with the pose and velocities they end it
    /// with. Anchors on pinned particles or missing bodies hold nothing.
    /// Particles inside an obstacle's shape are pushed out to its surface
    /// along the nearest way out, and its body is pushed back the same way
    /// (no friction).
    /// With [`SoftBodySettings::tear_strain`] set, the step ends with
    /// [`SoftBody::tear`].
    pub fn step_coupled(
        &mut self,
        settings: &SoftBodySettings,
        dt: f32,
        bodies: &mut [AnchorBody],
        anchors: &[Anchor],
        obstacles: &[Obstacle],
    ) {
        let substeps = settings.substeps.clamp(1, MAX_SOFT_BODY_SUBSTEPS);
        let h = dt / substeps as f32;
        if h <= 0.0 {
            return;
        }
        let mut previous = self.positions.clone();
        for _ in 0..substeps {
            previous.copy_from_slice(&self.positions);
            predict(
                &mut self.positions,
                &mut self.velocities,
                &self.inverse_masses,
                settings.gravity,
                settings.damping,
                h,
            );
            advance_bodies(bodies, h);
            solve_distances(
                &mut self.positions,
                &self.inverse_masses,
                &self.edges,
                &self.rest_lengths,
                settings.edge_compliance / (h * h),
            );
            self.solve_volumes(settings.volume_compliance / (h * h));
            solve_holds(
                &mut self.positions,
                &self.inverse_masses,
                bodies,
                anchors,
                obstacles,
                h,
            );
            finish_substep(
                &mut self.positions,
                &mut self.velocities,
                &self.inverse_masses,
                &previous,
                settings.floor,
                h,
            );
        }
        if let Some(strain) = settings.tear_strain {
            self.tear(strain);
        }
    }

    fn solve_volumes(&mut self, alpha: f32) {
        // For each corner, the two other corners whose cross product (from
        // a third) gives the outward volume gradient of that corner.
        const OPPOSITE: [[usize; 3]; 4] =
            [[1, 3, 2], [0, 2, 3], [0, 3, 1], [0, 1, 2]];
        for (tet, &rest) in self.tetrahedra.iter().zip(&self.rest_volumes) {
            let corners = tet.map(|index| index as usize);
            let points = corners.map(|index| vector(self.positions[index]));
            let mut gradients = [Vector3::zeros(); 4];
            let mut weight = 0.0;
            for (j, [p, q, r]) in OPPOSITE.into_iter().enumerate() {
                gradients[j] = (points[q] - points[p])
                    .cross(&(points[r] - points[p]))
                    / 6.0;
                weight += self.inverse_masses[corners[j]]
                    * gradients[j].norm_squared();
            }
            if weight + alpha == 0.0 {
                continue;
            }
            let volume = (points[1] - points[0])
                .cross(&(points[2] - points[0]))
                .dot(&(points[3] - points[0]))
                / 6.0;
            let lambda = -(volume - rest) / (weight + alpha);
            for j in 0..4 {
                let index = corners[j];
                self.positions[index] = (vector(self.positions[index])
                    + gradients[j] * (lambda * self.inverse_masses[index]))
                    .into();
            }
        }
    }

    /// The rest length of each edge and the rest volume of each
    /// tetrahedron, in the order of [`Self::edges`] and
    /// [`Self::tetrahedra`].
    pub(super) fn rests(&self) -> (&[f32], &[f32]) {
        (&self.rest_lengths, &self.rest_volumes)
    }

    /// FNV-1a over every position and velocity bit, for determinism checks.
    pub fn state_hash(&self) -> u64 {
        let mut hash = 0xcbf2_9ce4_8422_2325u64;
        for value in self.positions.iter().chain(&self.velocities).flatten() {
            hash = (hash ^ u64::from(value.to_bits()))
                .wrapping_mul(0x0000_0100_0000_01b3);
        }
        hash
    }
}

/// Starts a substep: damps every free particle's velocity, adds gravity
/// and moves the particle by it.
pub(super) fn predict(
    positions: &mut [[f32; 3]],
    velocities: &mut [[f32; 3]],
    inverse_masses: &[f32],
    gravity: [f32; 3],
    damping: f32,
    h: f32,
) {
    let keep = (1.0 - damping * h).clamp(0.0, 1.0);
    for i in 0..positions.len() {
        if inverse_masses[i] == 0.0 {
            continue;
        }
        let velocity = vector(velocities[i]) * keep + vector(gravity) * h;
        velocities[i] = velocity.into();
        positions[i] = (vector(positions[i]) + velocity * h).into();
    }
}

/// Moves and turns every body by its velocities for one substep.
pub(super) fn advance_bodies(bodies: &mut [AnchorBody], h: f32) {
    for body in bodies {
        body.position =
            (vector(body.position) + vector(body.velocity) * h).into();
        body.rotation =
            nalgebra::Rotation3::new(vector(body.angular_velocity) * h)
                * body.rotation;
    }
}

/// One Gauss-Seidel pass over distance constraints between particle pairs,
/// in slice order, with XPBD compliance `alpha` (already divided by h²).
pub(super) fn solve_distances(
    positions: &mut [[f32; 3]],
    inverse_masses: &[f32],
    pairs: &[[u32; 2]],
    rests: &[f32],
    alpha: f32,
) {
    for (pair, &rest) in pairs.iter().zip(rests) {
        let [a, b] = pair.map(|index| index as usize);
        let weight = inverse_masses[a] + inverse_masses[b];
        let delta = vector(positions[a]) - vector(positions[b]);
        let length = delta.norm();
        if weight + alpha == 0.0 || length < 1e-9 {
            continue;
        }
        let normal = delta / length;
        let lambda = -(length - rest) / (weight + alpha);
        positions[a] = (vector(positions[a])
            + normal * (lambda * inverse_masses[a]))
            .into();
        positions[b] = (vector(positions[b])
            - normal * (lambda * inverse_masses[b]))
            .into();
    }
}

/// Holds anchored particles on their bodies and pushes particles out of
/// obstacles, moving the bodies back (see [`SoftBody::step_coupled`]).
pub(super) fn solve_holds(
    positions: &mut [[f32; 3]],
    inverse_masses: &[f32],
    bodies: &mut [AnchorBody],
    anchors: &[Anchor],
    obstacles: &[Obstacle],
    h: f32,
) {
    for anchor in anchors {
        let Some(body) = bodies.get_mut(anchor.body) else {
            continue;
        };
        if anchor.particle >= positions.len() {
            continue;
        }
        let arm = body.rotation * vector(anchor.local);
        let gap =
            vector(body.position) + arm - vector(positions[anchor.particle]);
        close_gap(
            positions,
            inverse_masses,
            anchor.particle,
            body,
            arm,
            gap,
            h,
        );
    }
    for obstacle in obstacles {
        let Some(body) = bodies.get_mut(obstacle.body) else {
            continue;
        };
        // Pinned particles push too: they move the body alone.
        for i in 0..positions.len() {
            let arm = vector(positions[i]) - vector(body.position);
            let local = body.rotation.inverse() * arm;
            if let Some((normal, depth)) = obstacle.shape.push_out(local) {
                let gap = body.rotation * normal * depth;
                close_gap(positions, inverse_masses, i, body, arm, gap, h);
            }
        }
    }
}

/// Moves `particle` by `gap` (to where it should be), shared with `body` as
/// one XPBD position constraint: the body is pushed and turned the other
/// way at `arm` from its center, as much as its inverse mass and inertia
/// allow.
fn close_gap(
    positions: &mut [[f32; 3]],
    inverse_masses: &[f32],
    particle: usize,
    body: &mut AnchorBody,
    arm: Vector3<f32>,
    gap: Vector3<f32>,
    h: f32,
) {
    let weight = inverse_masses[particle];
    let length = gap.norm();
    if length < 1e-9 {
        return;
    }
    let normal = gap / length;
    let turn = arm.cross(&normal);
    let body_weight =
        body.inverse_mass + turn.dot(&(body.inverse_inertia * turn));
    if weight + body_weight == 0.0 {
        return;
    }
    let lambda = length / (weight + body_weight);
    positions[particle] =
        (vector(positions[particle]) + normal * (weight * lambda)).into();
    let push = -normal * lambda;
    body.position = (vector(body.position) + push * body.inverse_mass).into();
    body.velocity =
        (vector(body.velocity) + push * (body.inverse_mass / h)).into();
    let spin = body.inverse_inertia * arm.cross(&push);
    body.rotation = nalgebra::Rotation3::new(spin) * body.rotation;
    body.angular_velocity = (vector(body.angular_velocity) + spin / h).into();
}

/// Ends a substep: lifts particles below the floor onto it without
/// sideways motion (static friction) and takes velocities from how far
/// each particle moved.
pub(super) fn finish_substep(
    positions: &mut [[f32; 3]],
    velocities: &mut [[f32; 3]],
    inverse_masses: &[f32],
    previous: &[[f32; 3]],
    floor: Option<f32>,
    h: f32,
) {
    for i in 0..positions.len() {
        if inverse_masses[i] == 0.0 {
            continue;
        }
        if let Some(floor) = floor {
            if positions[i][1] < floor {
                positions[i] = [previous[i][0], floor, previous[i][2]];
            }
        }
        velocities[i] =
            ((vector(positions[i]) - vector(previous[i])) / h).into();
    }
}

/// Holds a particle of a [`SoftBodyVolume`] on a point fixed to a rigid
/// body. The body carries the particle along, and a dynamic body feels the
/// soft body pull back.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SoftAttachment {
    pub particle: usize,
    pub body: bevy_ecs::prelude::Entity,
    /// The point in the body's frame, in meters (scale is not applied).
    pub local: [f32; 3],
}

/// A soft body on an entity, stepped once per fixed tick after rigid
/// physics. It is runtime state, kept in snapshots but not in scene files;
/// a scene saves a [`SoftBlock`], which makes one on its first tick.
#[derive(bevy_ecs::prelude::Component, Clone, Debug)]
pub struct SoftBodyVolume {
    pub settings: SoftBodySettings,
    pub body: SoftBody,
    pub attachments: Vec<SoftAttachment>,
    /// Draws the outer faces of the tetrahedra; `None` draws nothing.
    pub skin: Option<SoftSkin>,
}

/// The drawn surface of a [`SoftBodyVolume`]: one entity whose mesh is
/// rebuilt from the particles whenever they move.
#[derive(Clone, Debug)]
pub struct SoftSkin {
    pub material: crate::assets::Handle<crate::assets::MaterialAsset>,
    mesh: Option<crate::assets::Handle<crate::assets::MeshAsset>>,
    pub(super) entity: Option<bevy_ecs::prelude::Entity>,
    /// State hash the mesh was built from; a body at rest keeps its mesh.
    built_from: u64,
}

impl SoftSkin {
    pub fn new(
        material: crate::assets::Handle<crate::assets::MaterialAsset>,
    ) -> Self {
        Self {
            material,
            mesh: None,
            entity: None,
            built_from: 0,
        }
    }
}

impl SoftBodyVolume {
    pub fn new(settings: SoftBodySettings, body: SoftBody) -> Self {
        Self {
            settings,
            body,
            attachments: Vec::new(),
            skin: None,
        }
    }

    /// Attaches every particle within `radius` of `point` (world space) to
    /// `body`, whose transform is `transform`, where the particles are now.
    /// Returns how many were attached.
    pub fn attach_near(
        &mut self,
        point: [f32; 3],
        radius: f32,
        body: bevy_ecs::prelude::Entity,
        transform: &crate::Transform,
    ) -> usize {
        let rotation = body_rotation(transform);
        let origin = vector(transform.position);
        let before = self.attachments.len();
        for (particle, &position) in self.body.positions.iter().enumerate() {
            if (vector(position) - vector(point)).norm() <= radius {
                self.attachments.push(SoftAttachment {
                    particle,
                    body,
                    local: (rotation.inverse() * (vector(position) - origin))
                        .into(),
                });
            }
        }
        self.attachments.len() - before
    }
}

pub(super) fn body_rotation(
    transform: &crate::Transform,
) -> nalgebra::Rotation3<f32> {
    let [roll, pitch, yaw] = transform.rotation;
    super::sim_math::rotation_from_euler(roll, pitch, yaw)
}

/// Most particles one [`SoftBlock`] spawns; more would stall the tick.
pub const MAX_SOFT_BLOCK_PARTICLES: u64 = 4_096;

/// A scene's soft body: a box of `count_x` by `count_y` by `count_z` cubes
/// of `spacing` meters centered on the entity, each cut into six
/// tetrahedra. The first fixed tick gives the entity its
/// [`SoftBodyVolume`]. It falls, keeps out of sphere, box and capsule
/// colliders and pushes dynamic bodies back.
#[derive(
    bevy_ecs::prelude::Component,
    Clone,
    Copy,
    Debug,
    PartialEq,
    serde::Serialize,
    serde::Deserialize,
)]
#[serde(default)]
pub struct SoftBlock {
    pub spacing: f32,
    pub count_x: u32,
    pub count_y: u32,
    pub count_z: u32,
    /// kg/m³.
    pub density: f32,
    /// Edge compliance; 0 is rigid, 1e-3 is a wobbly jelly.
    pub softness: f32,
    /// Volume compliance; 0 keeps every tetrahedron's volume.
    pub squash: f32,
    /// Fraction of velocity lost per second.
    pub damping: f32,
    /// Strain past which edges tear; 0 never tears.
    pub tear_strain: f32,
    pub substeps: u32,
    /// Draw the outer faces.
    pub visible: bool,
}

impl Default for SoftBlock {
    fn default() -> Self {
        Self {
            spacing: 0.1,
            count_x: 4,
            count_y: 4,
            count_z: 4,
            density: 1000.0,
            softness: 1e-4,
            squash: 0.0,
            damping: 1.0,
            tear_strain: 0.0,
            substeps: 10,
            visible: true,
        }
    }
}

impl SoftBlock {
    /// The volume this block starts as, centered on `center`.
    pub fn volume(&self, center: [f32; 3]) -> Result<SoftBodyVolume, String> {
        let mut counts = [self.count_x, self.count_y, self.count_z]
            .map(|count| count.max(1));
        while counts.iter().map(|&c| u64::from(c) + 1).product::<u64>()
            > MAX_SOFT_BLOCK_PARTICLES
        {
            let axis = (0..3).max_by_key(|&axis| counts[axis]).unwrap_or(0);
            counts[axis] = (counts[axis] / 2).max(1);
        }
        let spacing = self.spacing.max(0.01);
        let min = std::array::from_fn(|axis| {
            center[axis] - counts[axis] as f32 * spacing / 2.0
        });
        let body =
            SoftBody::block(min, counts, spacing, self.density.max(1.0))?;
        let settings = SoftBodySettings {
            substeps: self.substeps,
            edge_compliance: self.softness.max(0.0),
            volume_compliance: self.squash.max(0.0),
            floor: None,
            damping: self.damping.max(0.0),
            tear_strain: (self.tear_strain > 0.0).then_some(self.tear_strain),
            ..SoftBodySettings::default()
        };
        Ok(SoftBodyVolume::new(settings, body))
    }
}

/// Per fixed step, before [`step_soft_bodies`]: gives every [`SoftBlock`]
/// without a volume its [`SoftBodyVolume`].
pub(super) fn spawn_soft_bodies(
    mut commands: bevy_ecs::prelude::Commands,
    mut assets: Option<bevy_ecs::prelude::ResMut<crate::assets::AssetServer>>,
    blocks: bevy_ecs::prelude::Query<
        (bevy_ecs::prelude::Entity, &SoftBlock, &crate::Transform),
        bevy_ecs::prelude::Without<SoftBodyVolume>,
    >,
) {
    for (entity, block, transform) in &blocks {
        match block.volume(transform.position) {
            Ok(mut volume) => {
                if let Some(assets) =
                    assets.as_deref_mut().filter(|_| block.visible)
                {
                    volume.skin = Some(SoftSkin::new(jelly_material(assets)));
                }
                commands.entity(entity).insert(volume);
            }
            Err(error) => eprintln!("soft block {entity}: {error}"),
        }
    }
}

pub(super) fn named_material(
    assets: &mut crate::assets::AssetServer,
    name: &str,
    base_color: [f32; 4],
    roughness: f32,
) -> crate::assets::Handle<crate::assets::MaterialAsset> {
    if let Some((handle, _)) = assets
        .materials
        .iter()
        .find(|(_, material)| material.name == name)
    {
        return handle;
    }
    assets.materials.insert(crate::assets::MaterialAsset {
        name: name.into(),
        base_color,
        roughness,
        ..crate::assets::MaterialAsset::default()
    })
}

fn jelly_material(
    assets: &mut crate::assets::AssetServer,
) -> crate::assets::Handle<crate::assets::MaterialAsset> {
    named_material(assets, "Soft Block Jelly", [0.35, 0.8, 0.3, 1.0], 0.25)
}

/// The faces of a body's tetrahedra that no other tetrahedron shares, each
/// wound counterclockwise seen from outside. Torn bodies show their cuts.
pub fn skin_mesh(body: &SoftBody) -> crate::assets::MeshAsset {
    let mut faces: Vec<([u32; 3], [u32; 3])> = Vec::new();
    for &[a, b, c, d] in &body.tetrahedra {
        for face in [[a, c, b], [a, b, d], [a, d, c], [b, c, d]] {
            let mut key = face;
            key.sort_unstable();
            faces.push((key, face));
        }
    }
    faces.sort_unstable();
    let mut indices = Vec::new();
    let mut index = 0;
    while index < faces.len() {
        let mut end = index + 1;
        while end < faces.len() && faces[end].0 == faces[index].0 {
            end += 1;
        }
        if end - index == 1 {
            indices.extend(faces[index].1);
        }
        index = end;
    }
    let mut normals = vec![Vector3::zeros(); body.positions.len()];
    for triangle in indices.chunks_exact(3) {
        let [a, b, c] =
            [0, 1, 2].map(|i| vector(body.positions[triangle[i] as usize]));
        let face = (b - a).cross(&(c - a));
        for &corner in triangle {
            normals[corner as usize] += face;
        }
    }
    let vertices = body
        .positions
        .iter()
        .zip(&normals)
        .map(|(&position, normal)| crate::assets::MeshVertex {
            position,
            normal: normal.try_normalize(1e-12).unwrap_or(Vector3::y()).into(),
            ..crate::assets::MeshVertex::default()
        })
        .collect();
    crate::assets::MeshAsset { vertices, indices }
}

/// Per fixed step, after [`step_soft_bodies`]: rebuilds the skin mesh of
/// every visible soft body and cloth that moved, spawning its entity the
/// first time. The entity carries the fluid surface markers, so the editor
/// hides it and [`super::fluid::reap_surfaces`] frees it once the volume is
/// gone.
pub(super) fn sync_soft_skins(
    mut commands: bevy_ecs::prelude::Commands,
    assets: Option<bevy_ecs::prelude::ResMut<crate::assets::AssetServer>>,
    mut volumes: bevy_ecs::prelude::Query<(
        bevy_ecs::prelude::Entity,
        &mut SoftBodyVolume,
    )>,
    mut cloths: bevy_ecs::prelude::Query<(
        bevy_ecs::prelude::Entity,
        &mut super::ClothVolume,
    )>,
    markers: bevy_ecs::prelude::Query<&super::FluidParticle>,
) {
    let Some(mut assets) = assets else {
        return;
    };
    for (owner, mut volume) in &mut volumes {
        let volume = &mut *volume;
        let Some(skin) = volume.skin.as_mut() else {
            continue;
        };
        let hash =
            volume.body.state_hash() ^ volume.body.tetrahedra.len() as u64;
        sync_skin(
            &mut commands,
            &mut assets,
            &markers,
            owner,
            skin,
            hash,
            || skin_mesh(&volume.body),
        );
    }
    for (owner, mut cloth) in &mut cloths {
        let cloth = &mut *cloth;
        let Some(skin) = cloth.skin.as_mut() else {
            continue;
        };
        let hash =
            cloth.cloth.state_hash() ^ cloth.cloth.triangles.len() as u64;
        sync_skin(
            &mut commands,
            &mut assets,
            &markers,
            owner,
            skin,
            hash,
            || super::cloth::cloth_mesh(&cloth.cloth),
        );
    }
}

/// Rebuilds one skin's mesh with `build` unless it was built from `hash`.
fn sync_skin(
    commands: &mut bevy_ecs::prelude::Commands,
    assets: &mut crate::assets::AssetServer,
    markers: &bevy_ecs::prelude::Query<&super::FluidParticle>,
    owner: bevy_ecs::prelude::Entity,
    skin: &mut SoftSkin,
    hash: u64,
    build: impl FnOnce() -> crate::assets::MeshAsset,
) {
    let owned = skin.entity.is_some_and(|entity| {
        markers.get(entity).is_ok_and(|marker| marker.0 == owner)
    });
    if !owned {
        skin.mesh = None;
        skin.entity = None;
    }
    let live = skin
        .mesh
        .is_some_and(|handle| assets.meshes.get(handle).is_some());
    if live && skin.built_from == hash {
        return;
    }
    skin.built_from = hash;
    let mesh = build();
    match skin.mesh.and_then(|handle| assets.meshes.get_mut(handle)) {
        Some(slot) => *slot = mesh,
        None => {
            let handle = assets.meshes.insert(mesh);
            skin.mesh = Some(handle);
            skin.entity = Some(
                commands
                    .spawn((
                        crate::Transform::default(),
                        super::MeshRenderer {
                            mesh: handle,
                            material: skin.material,
                            cast_shadows: true,
                            receive_shadows: true,
                        },
                        super::FluidParticle(owner),
                        super::fluid::OwnedSurface {
                            mesh: handle,
                            material: None,
                        },
                    ))
                    .id(),
            );
        }
    }
}

/// The bodies soft bodies and cloth hang on and collide with.
pub(super) type CoupledBodies<'w, 's> = bevy_ecs::prelude::Query<
    'w,
    's,
    (
        bevy_ecs::prelude::Entity,
        Option<&'static super::SpawnOrder>,
        &'static mut crate::Transform,
        Option<&'static mut super::RigidBody>,
        Option<&'static super::Collider>,
        Option<&'static super::PhysicsBody>,
        Option<&'static super::PhysicsId>,
        Option<&'static super::GpuStateMirror>,
    ),
    bevy_ecs::prelude::Without<super::Parent>,
>;

/// Fixed tick and command queue for GPU bodies that soft bodies and cloth
/// meet, when a GPU backend takes commands.
type GpuLink<'a> = Option<(u64, &'a mut super::GpuPhysicsCommands)>;

/// A collider soft bodies and cloth keep out of, with its visit key.
type ColliderEntry = (
    (bool, u64, bevy_ecs::prelude::Entity),
    bevy_ecs::prelude::Entity,
    [f32; 3],
    ObstacleShape,
);

/// Per fixed step, after rigid physics: steps every [`SoftBodyVolume`],
/// then every [`super::ClothVolume`], each in spawn order and skipping
/// those marked [`super::gpu_cloth::GpuCloth`], with attached
/// particles held on their bodies and particles kept out of nearby sphere,
/// box and capsule colliders, then writes back the pose and velocities of
/// the dynamic bodies it moved. Attachments to a missing or parented body
/// hold nothing; a volume does not collide with the bodies it is attached
/// to. Particles of every [`super::FluidVolume`] (fluid or granular) near
/// a volume join its step as small dynamic spheres (see [`grain_bodies`]),
/// so the two push each other with their masses: a light jelly floats, a
/// heavy one sinks, and a sheet holds water and sags under it. A dynamic
/// GPU body takes part through its read-back state, and what the step does
/// to its velocity goes back as an impulse (see the `hybrid_physics` module
/// docs on latency); without fresh state it is left out.
// ponytail: a GPU body gets no angular impulse back (the command queue has
// none), so a deformable cannot spin it. Add a torque impulse command if
// that matters.
// ponytail: every collider is checked against every volume each tick (a
// bounding-sphere test); use the broad phase if scenes get many of both.
#[allow(clippy::type_complexity)]
pub(super) fn step_soft_bodies(
    time: bevy_ecs::prelude::Res<super::FrameTime>,
    mut gpu_commands: Option<
        bevy_ecs::prelude::ResMut<super::GpuPhysicsCommands>,
    >,
    mut volumes: bevy_ecs::prelude::Query<
        (
            bevy_ecs::prelude::Entity,
            Option<&super::SpawnOrder>,
            &mut SoftBodyVolume,
        ),
        bevy_ecs::prelude::Without<super::gpu_cloth::GpuCloth>,
    >,
    mut cloths: bevy_ecs::prelude::Query<
        (
            bevy_ecs::prelude::Entity,
            Option<&super::SpawnOrder>,
            &mut super::ClothVolume,
        ),
        bevy_ecs::prelude::Without<super::gpu_cloth::GpuCloth>,
    >,
    mut bodies: CoupledBodies,
    mut fluids: bevy_ecs::prelude::Query<(
        bevy_ecs::prelude::Entity,
        Option<&super::SpawnOrder>,
        &mut super::FluidVolume,
    )>,
) {
    let dt = time.fixed_delta.as_secs_f32();
    let mut fluids: Vec<_> = fluids.iter_mut().collect();
    fluids.sort_by_key(|(entity, order, _)| {
        super::fluid::visit_key(*order, *entity)
    });
    let mut fluids: Vec<_> = fluids
        .iter_mut()
        .map(|(_, _, fluid)| &mut **fluid)
        .collect();
    let mut colliders: Vec<ColliderEntry> = bodies
        .iter()
        .filter_map(|(entity, order, transform, _, collider, physics, ..)| {
            let shape = ObstacleShape::from_collider(
                collider.filter(|_| physics.is_some())?.shape,
                transform.scale,
            )?;
            Some((
                super::fluid::visit_key(order, entity),
                entity,
                transform.position,
                shape,
            ))
        })
        .collect();
    colliders.sort_by_key(|&(key, ..)| key);
    let mut volumes: Vec<_> = volumes.iter_mut().collect();
    volumes.sort_by_key(|(entity, order, _)| {
        super::fluid::visit_key(*order, *entity)
    });
    for (_, _, mut volume) in volumes {
        let volume = &mut *volume;
        let bounds = bounds(&volume.body.positions);
        let particles = volume.body.positions.len();
        couple(
            &mut bodies,
            &colliders,
            &volume.attachments,
            particles,
            bounds,
            &mut fluids,
            volume.body.min_rest_length() / 2.0,
            dt,
            gpu_commands
                .as_deref_mut()
                .map(|commands| (time.fixed_tick, commands)),
            |held, anchors, obstacles| {
                volume.body.step_coupled(
                    &volume.settings,
                    dt,
                    held,
                    anchors,
                    obstacles,
                )
            },
        );
    }
    let mut cloths: Vec<_> = cloths.iter_mut().collect();
    cloths.sort_by_key(|(entity, order, _)| {
        super::fluid::visit_key(*order, *entity)
    });
    for (_, _, mut cloth) in cloths {
        let cloth = &mut *cloth;
        let bounds = bounds(&cloth.cloth.positions);
        let particles = cloth.cloth.positions.len();
        couple(
            &mut bodies,
            &colliders,
            &cloth.attachments,
            particles,
            bounds,
            &mut fluids,
            cloth.cloth.min_rest_length() / 2.0,
            dt,
            gpu_commands
                .as_deref_mut()
                .map(|commands| (time.fixed_tick, commands)),
            |held, anchors, obstacles| {
                cloth.cloth.step_coupled(
                    &cloth.settings,
                    dt,
                    held,
                    anchors,
                    obstacles,
                )
            },
        );
    }
}

/// Gathers the bodies one volume hangs on, the colliders near its
/// `bounds` and the grains of `fluids` near it (its particles are balls of
/// `radius`), runs `step` with them and writes back the dynamic ones.
#[allow(clippy::too_many_arguments)]
fn couple(
    bodies: &mut CoupledBodies,
    colliders: &[ColliderEntry],
    attachments: &[SoftAttachment],
    particles: usize,
    (low, high): (Vector3<f32>, Vector3<f32>),
    fluids: &mut [&mut super::FluidVolume],
    radius: f32,
    dt: f32,
    mut gpu: GpuLink,
    step: impl FnOnce(&mut [AnchorBody], &[Anchor], &[Obstacle]),
) {
    // Entity and, for a GPU body, its id and starting velocity.
    let mut entities = Vec::new();
    let mut held = Vec::new();
    let tick = gpu.as_ref().map(|(tick, _)| *tick);
    let mut slot = |entity| {
        if let Some(index) = entities.iter().position(|&(e, _)| e == entity) {
            return Some(index);
        }
        let (_, _, transform, rigid, collider, physics, id, mirror) =
            bodies.get(entity).ok()?;
        let dynamic_gpu = physics
            .is_some_and(|p| p.simulation == super::SimulationClass::Gpu)
            && rigid.is_some_and(|rigid| {
                rigid.kind == super::RigidBodyKind::Dynamic
            });
        if !dynamic_gpu {
            entities.push((entity, None));
            held.push(anchor_body(transform, rigid, collider, dt));
            return Some(held.len() - 1);
        }
        let (pose, velocity) = super::coupled_gpu_state(mirror?, tick?, dt)?;
        let mirror = mirror?;
        // `anchor_body` takes a pose at the end of the tick.
        let mut pose = pose;
        for axis in 0..3 {
            pose.position[axis] += velocity[axis] * dt;
        }
        let rigid = super::RigidBody {
            linear_velocity: velocity,
            angular_velocity: mirror.angular_velocity,
            ..*rigid?
        };
        entities.push((entity, Some((*id?, velocity))));
        held.push(anchor_body(&pose, Some(&rigid), collider, dt));
        Some(held.len() - 1)
    };
    let mut anchors = Vec::new();
    for attachment in attachments {
        if attachment.particle >= particles {
            continue;
        }
        if let Some(body) = slot(attachment.body) {
            anchors.push(Anchor {
                particle: attachment.particle,
                body,
                local: attachment.local,
            });
        }
    }
    let center = (low + high) / 2.0;
    let reach = (high - low).norm() / 2.0;
    let mut obstacles = Vec::new();
    for &(_, entity, position, shape) in colliders {
        if attachments.iter().any(|a| a.body == entity) {
            continue;
        }
        // Room for a tick of travel at up to 60 m/s either way.
        let margin = 2.0 * 60.0 * dt;
        if (vector(position) - center).norm()
            > reach + shape.bounding_radius() + margin
        {
            continue;
        }
        if let Some(body) = slot(entity) {
            obstacles.push(Obstacle { body, shape });
        }
    }
    let rigid = held.len();
    let grains = grain_bodies(fluids, (low, high), radius, dt);
    for (index, &(_, _, shape, body)) in grains.iter().enumerate() {
        held.push(body);
        obstacles.push(Obstacle {
            body: rigid + index,
            shape,
        });
    }
    step(&mut held, &anchors, &obstacles);
    for (&(fluid, grain, ..), state) in grains.iter().zip(&held[rigid..]) {
        let fluid = &mut fluids[fluid].fluid;
        fluid.positions[grain] = state.position;
        fluid.velocities[grain] = state.velocity;
    }
    held.truncate(rigid);
    for ((entity, gpu_body), state) in entities.into_iter().zip(held) {
        if state.inverse_mass == 0.0 {
            continue;
        }
        if let Some((id, start)) = gpu_body {
            let mass = state.inverse_mass.recip();
            if let Some((_, commands)) =
                gpu.as_mut().filter(|_| state.velocity != start)
            {
                commands.push(
                    id,
                    super::GpuBodyCommand::Impulse(std::array::from_fn(
                        |axis| mass * (state.velocity[axis] - start[axis]),
                    )),
                );
            }
            continue;
        }
        let Ok((_, _, mut transform, Some(mut rigid), ..)) =
            bodies.get_mut(entity)
        else {
            continue;
        };
        let (roll, pitch, yaw) = state.rotation.euler_angles();
        transform.position = state.position;
        transform.rotation = [roll, pitch, yaw];
        rigid.linear_velocity = state.velocity;
        rigid.angular_velocity = state.angular_velocity;
    }
}

/// Every particle of `fluids` that can reach the box `bounds` this tick,
/// as `(fluid index, particle index, shape, body)`: a dynamic sphere of
/// the particle's mass, `radius` plus half the fluid spacing across (so a
/// soft-body particle, a point in the step, stops at the sum of both
/// radii), with no spin. The fluid has already stepped this tick, so like a
/// rigid body the grain starts where it was a tick ago and moves through
/// the tick again. Fluids and particles go in index order.
// ponytail: grains finer than about 3/4 of a deformable's spacing can slip
// between its particles, and a grain over about 30 times the mass of the
// particles it rests on overpowers their constraints and tears through
// (1 kg water grains need a sheet of 3 kg/m² or more at 0.1 m spacing).
// Push grains out of tetrahedra and triangles, or scale contact masses,
// if fine or heavy fluids meet coarse or light deformables.
fn grain_bodies(
    fluids: &[&mut super::FluidVolume],
    (low, high): (Vector3<f32>, Vector3<f32>),
    radius: f32,
    dt: f32,
) -> Vec<(usize, usize, ObstacleShape, AnchorBody)> {
    let mut grains = Vec::new();
    for (index, volume) in fluids.iter().enumerate() {
        let settings = &volume.settings;
        if settings.particle_mass <= 0.0 {
            continue;
        }
        let reach = radius + settings.spacing / 2.0;
        let fluid = &volume.fluid;
        for (grain, (&position, &velocity)) in
            fluid.positions.iter().zip(&fluid.velocities).enumerate()
        {
            let velocity = vector(velocity);
            let start = vector(position) - velocity * dt;
            // Distance from the box to the start and to the end of the
            // grain's path through the tick.
            let gap = |p: Vector3<f32>| {
                (low - p).sup(&(p - high)).sup(&Vector3::zeros()).norm()
            };
            if gap(start).min(gap(vector(position))) > reach {
                continue;
            }
            grains.push((
                index,
                grain,
                ObstacleShape::Sphere(reach),
                AnchorBody {
                    position: start.into(),
                    rotation: nalgebra::Rotation3::identity(),
                    velocity: velocity.into(),
                    angular_velocity: [0.0; 3],
                    inverse_mass: 1.0 / settings.particle_mass,
                    inverse_inertia: nalgebra::Matrix3::zeros(),
                },
            ));
        }
    }
    grains
}

/// The smallest box around some points.
fn bounds(points: &[[f32; 3]]) -> (Vector3<f32>, Vector3<f32>) {
    let mut low = Vector3::repeat(f32::MAX);
    let mut high = Vector3::repeat(f32::MIN);
    for &point in points {
        low = low.inf(&vector(point));
        high = high.sup(&vector(point));
    }
    (low, high)
}

/// The start-of-tick state of a body for [`SoftBody::step_coupled`]. Only a
/// dynamic body with mass can be moved by the soft body.
fn anchor_body(
    transform: &crate::Transform,
    rigid: Option<&super::RigidBody>,
    collider: Option<&super::Collider>,
    dt: f32,
) -> AnchorBody {
    let rotation = body_rotation(transform);
    let (velocity, spin) =
        rigid.map_or((Vector3::zeros(), Vector3::zeros()), |rigid| {
            (
                vector(rigid.linear_velocity),
                vector(rigid.angular_velocity),
            )
        });
    let dynamic = rigid.is_some_and(|rigid| {
        rigid.kind == super::RigidBodyKind::Dynamic && rigid.mass > 0.0
    });
    let (inverse_mass, inverse_inertia) = match (rigid, collider) {
        (Some(rigid), Some(collider)) if dynamic => (
            1.0 / rigid.mass,
            super::cpu_physics::world_inverse_inertia(
                collider,
                transform.scale,
                &rotation,
                rigid.mass,
            ),
        ),
        (Some(rigid), None) if dynamic => {
            (1.0 / rigid.mass, nalgebra::Matrix3::zeros())
        }
        _ => (0.0, nalgebra::Matrix3::zeros()),
    };
    // Rigid physics already moved the body through this tick; the soft step
    // moves it through the tick again from where it was.
    AnchorBody {
        position: (vector(transform.position) - velocity * dt).into(),
        rotation: nalgebra::Rotation3::new(-spin * dt) * rotation,
        velocity: velocity.into(),
        angular_velocity: spin.into(),
        inverse_mass,
        inverse_inertia,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DT: f32 = 1.0 / 60.0;

    fn run(body: &mut SoftBody, settings: &SoftBodySettings, steps: u32) {
        for _ in 0..steps {
            body.step(settings, DT);
        }
    }

    #[test]
    fn a_block_is_six_tetrahedra_per_cube_and_fills_its_box() {
        let body = SoftBody::block([0.0; 3], [2, 3, 4], 0.5, 1000.0).unwrap();
        assert_eq!(body.positions.len(), 3 * 4 * 5);
        assert_eq!(body.tetrahedra.len(), 6 * 24);
        let expected = 1.0 * 1.5 * 2.0;
        assert!((body.volume() - expected).abs() < 1e-4, "{}", body.volume());
        let mass: f32 = body.inverse_masses.iter().map(|w| 1.0 / w).sum();
        assert!((mass - expected * 1000.0).abs() < 0.5, "{mass}");
        assert!(
            SoftBody::new(vec![[0.0; 3]; 4], vec![[0, 1, 2, 3]], 1.0).is_err()
        );
        assert!(
            SoftBody::new(vec![[0.0; 3]; 3], vec![[0, 1, 2, 3]], 1.0).is_err()
        );
    }

    #[test]
    fn a_dropped_jelly_lands_on_the_floor_and_keeps_its_volume() {
        let settings = SoftBodySettings {
            edge_compliance: 1e-4,
            ..SoftBodySettings::default()
        };
        let mut body =
            SoftBody::block([-0.5, 1.0, -0.5], [4, 4, 4], 0.25, 1000.0)
                .unwrap();
        let mut lowest_volume = f32::MAX;
        for _ in 0..300 {
            body.step(&settings, DT);
            lowest_volume = lowest_volume.min(body.volume());
        }
        let bottom =
            body.positions.iter().map(|p| p[1]).fold(f32::MAX, f32::min);
        let top = body.positions.iter().map(|p| p[1]).fold(f32::MIN, f32::max);
        assert!((0.0..0.01).contains(&bottom), "bottom {bottom}");
        assert!(top > 0.8, "collapsed to {top}");
        assert!(
            lowest_volume > 0.95 * body.rest_volume(),
            "volume fell to {lowest_volume}"
        );
        let speed = body
            .velocities
            .iter()
            .map(|v| vector(*v).norm())
            .fold(0.0, f32::max);
        assert!(speed < 0.2, "still moving at {speed}");
    }

    #[test]
    fn a_pinned_beam_holds_its_end_and_softer_beams_sag_more() {
        let sag = |edge_compliance: f32| {
            let settings = SoftBodySettings {
                edge_compliance,
                volume_compliance: edge_compliance,
                floor: None,
                ..SoftBodySettings::default()
            };
            let mut beam =
                SoftBody::block([0.0, 1.0, 0.0], [8, 1, 1], 0.1, 500.0)
                    .unwrap();
            let pinned: Vec<usize> = (0..beam.positions.len())
                .filter(|&i| beam.positions[i][0] == 0.0)
                .collect();
            for &i in &pinned {
                beam.pin(i);
            }
            let start = beam.positions.clone();
            run(&mut beam, &settings, 240);
            for &i in &pinned {
                assert_eq!(beam.positions[i], start[i]);
            }
            beam.positions
                .iter()
                .map(|p| 1.0 - p[1])
                .fold(0.0, f32::max)
        };
        let stiff = sag(1e-7);
        let soft = sag(1e-4);
        assert!(stiff < 0.1, "a stiff beam sagged {stiff}");
        assert!(soft > stiff * 4.0, "soft {soft} against stiff {stiff}");
    }

    #[test]
    fn the_same_start_gives_the_same_bits() {
        let settings = SoftBodySettings {
            edge_compliance: 1e-4,
            volume_compliance: 1e-5,
            ..SoftBodySettings::default()
        };
        let make = || {
            let mut body =
                SoftBody::block([0.0, 0.5, 0.0], [3, 2, 3], 0.2, 800.0)
                    .unwrap();
            body.velocities[5] = [1.0, 2.0, -0.5];
            body
        };
        let (mut a, mut b) = (make(), make());
        run(&mut a, &settings, 60);
        run(&mut b, &settings, 60);
        assert_eq!(a.state_hash(), b.state_hash());
        assert!(a.positions.iter().flatten().all(|v| v.is_finite()));
    }

    #[test]
    fn an_overstretched_column_tears_and_drops_its_lower_half() {
        let pulled = |tear_strain: Option<f32>| {
            let settings = SoftBodySettings {
                edge_compliance: 1e-3,
                floor: None,
                tear_strain,
                ..SoftBodySettings::default()
            };
            let mut column =
                SoftBody::block([0.0, 0.0, 0.0], [1, 2, 1], 0.1, 500.0)
                    .unwrap();
            for i in 0..column.positions.len() {
                if column.positions[i][1] > 0.15 {
                    column.pin(i);
                } else if column.positions[i][1] < 0.05 {
                    column.velocities[i] = [0.0, -20.0, 0.0];
                }
            }
            let tetrahedra = column.tetrahedra.len();
            run(&mut column, &settings, 60);
            let lowest = column
                .positions
                .iter()
                .map(|p| p[1])
                .fold(f32::MAX, f32::min);
            (tetrahedra - column.tetrahedra.len(), lowest)
        };
        let (kept, held_at) = pulled(None);
        assert_eq!(kept, 0);
        assert!(held_at > -0.5, "the untearable column fell to {held_at}");
        let (removed, fell_to) = pulled(Some(0.5));
        assert!(removed >= 6, "only {removed} tetrahedra tore");
        assert!(fell_to < -2.0, "the torn half only fell to {fell_to}");
    }

    #[test]
    fn tearing_keeps_only_edges_of_remaining_tetrahedra() {
        let mut body =
            SoftBody::block([0.0, 0.0, 0.0], [2, 1, 1], 1.0, 1.0).unwrap();
        body.positions[0][0] -= 5.0;
        let removed = body.tear(0.5);
        assert!(removed > 0 && removed < 12, "removed {removed}");
        assert_eq!(body.tetrahedra.len() + removed, 12);
        assert!(body.tetrahedra.iter().all(|t| !t.contains(&0)));
        for edge in &body.edges {
            assert!(body
                .tetrahedra
                .iter()
                .any(|&t| tet_edges(t).contains(edge)));
        }
        assert_eq!(body.edges.len(), body.rest_lengths.len());
        assert_eq!(body.tear(0.5), 0);
    }

    fn still_body(position: [f32; 3], inverse_mass: f32) -> AnchorBody {
        AnchorBody {
            position,
            rotation: nalgebra::Rotation3::identity(),
            velocity: [0.0; 3],
            angular_velocity: [0.0; 3],
            inverse_mass,
            inverse_inertia: nalgebra::Matrix3::identity() * inverse_mass,
        }
    }

    #[test]
    fn a_jelly_dropped_on_a_fixed_box_rests_on_its_top() {
        let settings = SoftBodySettings {
            edge_compliance: 1e-4,
            floor: None,
            ..SoftBodySettings::default()
        };
        let mut body =
            SoftBody::block([-0.2, 1.0, -0.2], [2, 2, 2], 0.2, 1000.0).unwrap();
        let mut bodies = [still_body([0.0, 0.0, 0.0], 0.0)];
        let obstacles = [Obstacle {
            body: 0,
            shape: ObstacleShape::Box([1.0, 0.5, 1.0]),
        }];
        for _ in 0..180 {
            body.step_coupled(&settings, DT, &mut bodies, &[], &obstacles);
        }
        let bottom =
            body.positions.iter().map(|p| p[1]).fold(f32::MAX, f32::min);
        assert!((0.49..0.52).contains(&bottom), "bottom {bottom}");
        assert_eq!(bodies[0], still_body([0.0, 0.0, 0.0], 0.0));
    }

    #[test]
    fn a_ball_thrown_into_a_floating_jelly_shares_its_momentum() {
        let settings = SoftBodySettings {
            gravity: [0.0; 3],
            edge_compliance: 1e-4,
            floor: None,
            damping: 0.0,
            ..SoftBodySettings::default()
        };
        let mut jelly =
            SoftBody::block([0.0, -0.2, -0.2], [2, 2, 2], 0.2, 250.0).unwrap();
        let jelly_mass: f32 =
            jelly.inverse_masses.iter().map(|w| 1.0 / w).sum();
        let ball_mass = 2.0;
        let mut bodies = [still_body([-0.5, 0.0, 0.0], 1.0 / ball_mass)];
        bodies[0].inverse_inertia =
            nalgebra::Matrix3::identity() / (0.4 * ball_mass * 0.15 * 0.15);
        bodies[0].velocity = [3.0, 0.0, 0.0];
        let obstacles = [Obstacle {
            body: 0,
            shape: ObstacleShape::Sphere(0.15),
        }];
        for _ in 0..60 {
            jelly.step_coupled(&settings, DT, &mut bodies, &[], &obstacles);
        }
        let jelly_momentum: f32 = jelly
            .velocities
            .iter()
            .zip(&jelly.inverse_masses)
            .map(|(v, w)| v[0] / w)
            .sum();
        let total = jelly_momentum + bodies[0].velocity[0] * ball_mass;
        assert!(
            (total - 6.0).abs() < 0.3,
            "momentum {total} (jelly {jelly_momentum} of {jelly_mass} kg)"
        );
        assert!(bodies[0].velocity[0] < 2.0, "ball kept {:?}", bodies[0]);
        assert!(jelly_momentum > 1.0, "jelly got {jelly_momentum}");
        let deepest = jelly
            .positions
            .iter()
            .map(|&p| (vector(p) - vector(bodies[0].position)).norm())
            .fold(f32::MAX, f32::min);
        assert!(deepest > 0.14, "a particle sits {deepest} inside the ball");
    }

    #[test]
    fn the_skin_is_the_outer_faces_wound_outward() {
        for (cells, triangles) in [([1, 1, 1], 12), ([2, 2, 2], 48)] {
            let body = SoftBody::block([0.0; 3], cells, 0.5, 1000.0).unwrap();
            let mesh = skin_mesh(&body);
            assert_eq!(mesh.indices.len(), triangles * 3, "{cells:?}");
            let center = Vector3::repeat(cells[0] as f32 * 0.25);
            for triangle in mesh.indices.chunks_exact(3) {
                let [a, b, c] = [0, 1, 2]
                    .map(|i| vector(body.positions[triangle[i] as usize]));
                let out = (b - a).cross(&(c - a));
                assert!(out.dot(&((a + b + c) / 3.0 - center)) > 0.0);
            }
            for &index in &mesh.indices {
                let vertex = &mesh.vertices[index as usize];
                let outward = vector(vertex.position) - center;
                assert!(vector(vertex.normal).dot(&outward) > 0.0);
            }
        }
    }

    use crate::runtime::{
        App, Cloth, ClothSettings, ClothVolume, Fluid, FluidSettings,
        FluidSolver, FluidVolume,
    };

    fn run_app(app: &mut App, ticks: u32) {
        for _ in 0..ticks {
            app.update(std::time::Duration::from_secs_f64(1.0 / 60.0))
                .unwrap();
        }
    }

    /// Lowest point of a 0.3 m jelly of `density` dropped into a 0.4 m
    /// deep pool of water or sand after 4 s.
    fn jelly_rest(density: f32, solver: FluidSolver) -> f32 {
        let mut app = App::new();
        let settings = FluidSettings {
            bounds: ([-0.4, 0.0, -0.4], [0.4, 2.0, 0.4]),
            particle_mass: 1.0,
            solver,
            ..FluidSettings::default()
        };
        let fluid = Fluid::block([-0.35, 0.0, -0.35], [8, 4, 8], 0.1);
        app.world_mut().spawn(FluidVolume::new(settings, fluid));
        let body =
            SoftBody::block([-0.15, 0.6, -0.15], [3; 3], 0.1, density).unwrap();
        let jelly = app
            .world_mut()
            .spawn(SoftBodyVolume::new(SoftBodySettings::default(), body))
            .id();
        run_app(&mut app, 240);
        let volume = app.world().get::<SoftBodyVolume>(jelly).unwrap();
        volume
            .body
            .positions
            .iter()
            .map(|p| p[1])
            .fold(f32::INFINITY, f32::min)
    }

    #[test]
    fn a_light_jelly_floats_on_water_and_sand_and_a_heavy_one_sinks() {
        for solver in [FluidSolver::Particles, FluidSolver::Granular] {
            let light = jelly_rest(300.0, solver);
            let heavy = jelly_rest(5000.0, solver);
            assert!(light > 0.12, "{solver:?}: light jelly bottom at {light}");
            assert!(
                light > heavy + 0.08,
                "{solver:?}: light {light} vs heavy {heavy}"
            );
        }
        assert!(jelly_rest(5000.0, FluidSolver::Particles) < 0.1);
    }

    /// One tick of a 1 kg GPU ball falling at 2 m/s onto a pinned sheet,
    /// with read-back state `age` ticks old: the impulses sent back, the
    /// sheet's middle height, and whether the ball's authored pose stayed.
    fn gpu_ball_on_sheet(age: u64) -> (Vec<[f32; 3]>, f32, bool) {
        use crate::runtime::{
            Collider, ColliderShape, FrameTime, GpuBodyCommand,
            GpuPhysicsCommands, GpuStateMirror, PhysicsBody, PhysicsId,
            RigidBody, SimulationClass,
        };
        let mut app = App::new();
        app.world_mut()
            .insert_resource(GpuPhysicsCommands::default());
        app.world_mut().resource_mut::<FrameTime>().fixed_tick = 20;
        let mut cloth = Cloth::grid(
            [-0.5, 1.0, -0.5],
            [1.0, 0.0, 0.0],
            [0.0, 0.0, 1.0],
            [10, 10],
            0.1,
            10.0,
        )
        .unwrap();
        for (index, mass) in cloth.inverse_masses.iter_mut().enumerate() {
            let (x, z) = (index % 11, index / 11);
            if x == 0 || x == 10 || z == 0 || z == 10 {
                *mass = 0.0;
            }
        }
        let sheet = app
            .world_mut()
            .spawn(ClothVolume::new(ClothSettings::default(), cloth))
            .id();
        // Writing a GPU body's `Transform` would restart it from there.
        let authored = crate::Transform {
            position: [0.0, 1.14, 0.0],
            ..crate::Transform::default()
        };
        let id = PhysicsId {
            slot: 5,
            generation: 1,
        };
        let ball = app
            .world_mut()
            .spawn((
                authored,
                RigidBody {
                    mass: 1.0,
                    ..RigidBody::default()
                },
                Collider {
                    shape: ColliderShape::Sphere { radius: 0.15 },
                    ..Collider::default()
                },
                PhysicsBody {
                    simulation: SimulationClass::Gpu,
                    ..PhysicsBody::default()
                },
                id,
                GpuStateMirror {
                    tick: 20 - age,
                    transform: authored,
                    linear_velocity: [0.0, -2.0, 0.0],
                    angular_velocity: [0.0; 3],
                    custom_values: None,
                },
            ))
            .id();
        run_app(&mut app, 1);
        let impulses = app
            .world()
            .resource::<GpuPhysicsCommands>()
            .commands
            .iter()
            .map(|&(body, command)| match (body == id, command) {
                (true, GpuBodyCommand::Impulse(impulse)) => impulse,
                other => panic!("unexpected command {other:?}"),
            })
            .collect();
        let middle = app
            .world()
            .get::<ClothVolume>(sheet)
            .unwrap()
            .cloth
            .positions[5 * 11 + 5][1];
        let kept =
            *app.world().get::<crate::Transform>(ball).unwrap() == authored;
        (impulses, middle, kept)
    }

    #[test]
    fn a_gpu_ball_and_a_sheet_push_each_other_through_read_back_state() {
        let (fresh, pressed, kept) = gpu_ball_on_sheet(0);
        assert!(kept, "the GPU body's authored pose was overwritten");
        assert_eq!(fresh.len(), 1);
        assert!(fresh[0][1] > 0.0, "{fresh:?}");
        let stale = crate::runtime::MAX_COUPLED_STATE_AGE + 1;
        let (none, untouched, kept) = gpu_ball_on_sheet(stale);
        assert!(kept && none.is_empty());
        assert!(pressed < untouched - 0.005, "{pressed} vs {untouched}");
    }

    #[test]
    fn water_poured_on_a_pinned_sheet_stays_on_it_and_sags_it() {
        let mut app = App::new();
        let settings = FluidSettings {
            bounds: ([-0.5, 0.0, -0.5], [0.5, 3.0, 0.5]),
            particle_mass: 1.0,
            ..FluidSettings::default()
        };
        let fluid = Fluid::block([-0.15, 1.3, -0.15], [4, 3, 4], 0.1);
        let count = fluid.positions.len();
        app.world_mut().spawn(FluidVolume::new(settings, fluid));
        let mut cloth = Cloth::grid(
            [-0.5, 1.0, -0.5],
            [1.0, 0.0, 0.0],
            [0.0, 0.0, 1.0],
            [10, 10],
            0.1,
            10.0,
        )
        .unwrap();
        for (index, mass) in cloth.inverse_masses.iter_mut().enumerate() {
            let (x, z) = (index % 11, index / 11);
            if x == 0 || x == 10 || z == 0 || z == 10 {
                *mass = 0.0;
            }
        }
        let sheet = app
            .world_mut()
            .spawn(ClothVolume::new(ClothSettings::default(), cloth))
            .id();
        run_app(&mut app, 180);
        let fluid = &app
            .world_mut()
            .query::<&FluidVolume>()
            .single(app.world())
            .unwrap()
            .fluid;
        let lowest = fluid.positions.iter().map(|p| p[1]).fold(9.0, f32::min);
        assert_eq!(fluid.positions.len(), count);
        let cloth = &app.world().get::<ClothVolume>(sheet).unwrap().cloth;
        let middle = cloth.positions[5 * 11 + 5][1];
        assert!(lowest > 0.5, "water fell through to {lowest}");
        // 48 kg of water on a 10 kg sheet pulls its middle down from the
        // 0.97 it hangs at alone.
        assert!(middle < 0.95, "sheet middle at {middle}");
        assert!(middle > 0.3, "sheet middle at {middle}");
    }

    /// Water, sand, a jelly, a pinned sheet with a ball on it and a hinged
    /// arm with a ball paddle, all in one 1.2 m tank. Returns every particle
    /// and body position after `ticks` (over 60), the paddle's highest point
    /// after its first second, and the jelly's lowest point.
    fn mixed_scene(water: bool, ticks: u32) -> (Vec<[f32; 3]>, f32, f32) {
        use crate::runtime::{
            Articulation, Collider, ColliderShape, Joint, JointKind,
            PhysicsBody, RigidBody, RigidBodyKind,
        };
        use crate::Transform;
        let mut app = App::new();
        let world = app.world_mut();
        let tank = ([-0.6, 0.0, -0.6], [0.6, 3.0, 0.6]);
        if water {
            let settings = FluidSettings {
                bounds: tank,
                ..FluidSettings::default()
            };
            let pool = Fluid::block([-0.55, 0.0, -0.55], [12, 3, 12], 0.1);
            world.spawn(FluidVolume::new(settings, pool));
        }
        let settings = FluidSettings {
            bounds: tank,
            particle_mass: 3.0,
            solver: FluidSolver::Granular,
            ..FluidSettings::default()
        };
        let sand = Fluid::block([-0.5, 0.6, 0.2], [3, 2, 3], 0.1);
        world.spawn(FluidVolume::new(settings, sand));
        let body =
            SoftBody::block([-0.1, 0.6, -0.5], [3; 3], 0.1, 300.0).unwrap();
        let jelly = world
            .spawn(SoftBodyVolume::new(SoftBodySettings::default(), body))
            .id();
        let mut cloth = Cloth::grid(
            [0.1, 1.0, 0.1],
            [1.0, 0.0, 0.0],
            [0.0, 0.0, 1.0],
            [4, 4],
            0.1,
            10.0,
        )
        .unwrap();
        for (index, mass) in cloth.inverse_masses.iter_mut().enumerate() {
            if index % 5 == 0 || index % 5 == 4 {
                *mass = 0.0;
            }
        }
        world.spawn(ClothVolume::new(ClothSettings::default(), cloth));
        let body = |world: &mut bevy_ecs::world::World, at, radius, kind| {
            world
                .spawn((
                    Transform::new(at),
                    PhysicsBody::default(),
                    RigidBody {
                        kind,
                        ..RigidBody::default()
                    },
                    Collider {
                        shape: ColliderShape::Sphere { radius },
                        ..Collider::default()
                    },
                ))
                .id()
        };
        // The arm swings down from level through the pool.
        let base = body(world, [0.0, 0.55, -0.3], 0.05, RigidBodyKind::Fixed);
        world.entity_mut(base).insert(Articulation {});
        let paddle =
            body(world, [0.45, 0.55, -0.3], 0.12, RigidBodyKind::Dynamic);
        world.entity_mut(paddle).insert(Joint {
            frame: [0.0, -std::f32::consts::FRAC_PI_2, 0.0],
            target_frame: [0.0, -std::f32::consts::FRAC_PI_2, 0.0],
            ..Joint::new(
                JointKind::Hinge {
                    limit: None,
                    spring: None,
                    motor: None,
                },
                base,
                [-0.45, 0.0, 0.0],
                [0.0; 3],
            )
        });
        // A ball dropped on the sheet.
        let ball = body(world, [0.3, 1.6, 0.3], 0.08, RigidBodyKind::Dynamic);
        // How high the paddle swings back after its first pass.
        run_app(&mut app, 60);
        let mut swing = f32::MIN;
        for _ in 60..ticks {
            run_app(&mut app, 1);
            swing = swing
                .max(app.world().get::<Transform>(paddle).unwrap().position[1]);
        }
        let world = app.world_mut();
        let mut points = Vec::new();
        for volume in world.query::<&FluidVolume>().iter(world) {
            points.extend(&volume.fluid.positions);
        }
        for volume in world.query::<&SoftBodyVolume>().iter(world) {
            points.extend(&volume.body.positions);
        }
        for volume in world.query::<&ClothVolume>().iter(world) {
            points.extend(&volume.cloth.positions);
        }
        for entity in [paddle, ball] {
            points.push(world.get::<Transform>(entity).unwrap().position);
        }
        let bottom = world
            .get::<SoftBodyVolume>(jelly)
            .unwrap()
            .body
            .positions
            .iter()
            .map(|p| p[1])
            .fold(f32::INFINITY, f32::min);
        (points, swing, bottom)
    }

    #[test]
    fn every_kind_of_body_shares_one_tank_and_runs_the_same_twice() {
        let (points, swing, bottom) = mixed_scene(true, 240);
        let (_, dry_swing, dry_bottom) = mixed_scene(false, 240);
        assert!(points.iter().flatten().all(|x| x.is_finite()));
        // The water and the floating jelly stop the arm; dry, it swings on.
        assert!(swing < 0.4, "wet paddle swung to {swing}");
        assert!(dry_swing > 0.45, "dry paddle swung to {dry_swing}");
        assert!(bottom > 0.04, "jelly bottom at {bottom}");
        assert!(dry_bottom < 0.01, "dry jelly bottom at {dry_bottom}");
        // The ball rests on the sheet.
        let ball = points[points.len() - 1];
        assert!(ball[1] > 0.9, "ball at {ball:?}");
        assert!(
            points == mixed_scene(true, 240).0,
            "the scene ran differently"
        );
    }
}
