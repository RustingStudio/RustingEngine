//! Deterministic particle fluid (position based fluids, Macklin and Müller
//! 2013) on the CPU.
//!
//! The solver follows the physics determinism rules: particles and their
//! neighbors are visited in ascending index order, every pass writes to its
//! own buffer (Jacobi style), nothing is accumulated through atomics and no
//! randomness is used. The same particles and settings give the same bits.
//! It is independent of the ECS; a component and rendering come later.

// The per-axis loops index several small arrays at once; iterators read worse.
#![allow(clippy::needless_range_loop)]

use nalgebra::Vector3;
use std::collections::HashMap;
use std::f32::consts::PI;

/// Artificial pressure is off: with these units it dominated the density
/// correction and blew the fluid apart. Revisit with surface tension.
const SCORR: f32 = 0.0;

/// Most density iterations one step runs, whatever the settings ask.
pub const MAX_FLUID_ITERATIONS: u32 = 64;

/// Solver settings. Lengths are meters and times seconds.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FluidSettings {
    /// Rest spacing of particles. The smoothing radius is `2.0 * spacing`.
    pub spacing: f32,
    /// Rest density in particles per cubic meter is derived from `spacing`;
    /// this is the mass of one particle in kilograms.
    pub particle_mass: f32,
    pub gravity: [f32; 3],
    /// Density-constraint iterations per step, up to
    /// [`MAX_FLUID_ITERATIONS`]. More is stiffer and slower.
    pub iterations: u32,
    /// Constraint relaxation. Larger values soften the fluid.
    pub relaxation: f32,
    /// XSPH viscosity in 0..1.
    pub viscosity: f32,
    /// Axis-aligned container as (min, max) corners.
    pub bounds: ([f32; 3], [f32; 3]),
    /// Which solver steps the particles.
    pub solver: FluidSolver,
    /// Coulomb friction between grains and against the container, for
    /// [`FluidSolver::Granular`]; the others ignore it.
    pub friction: f32,
}

/// The solver a fluid runs on.
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
pub enum FluidSolver {
    /// Position based fluid: stiff and splashy, for small volumes of a few
    /// thousand particles.
    #[default]
    Particles,
    /// FLIP on a grid over the container: no neighbor search, so it carries
    /// large water volumes; softer, and it slowly loses volume.
    Grid,
    /// Grains with friction (sand, gravel, rubble): they pile into heaps
    /// whose slope grows with `friction`.
    Granular,
}

impl Default for FluidSettings {
    fn default() -> Self {
        Self {
            spacing: 0.1,
            particle_mass: 1.0,
            gravity: [0.0, -9.81, 0.0],
            iterations: 4,
            relaxation: 10.0,
            viscosity: 0.01,
            bounds: ([-1.0; 3], [1.0; 3]),
            solver: FluidSolver::Particles,
            friction: 0.6,
        }
    }
}

impl FluidSettings {
    /// Smoothing radius.
    pub fn radius(&self) -> f32 {
        2.0 * self.spacing
    }

    /// Density of a settled fluid, in kilograms per cubic meter.
    pub fn rest_density(&self) -> f32 {
        self.particle_mass / self.spacing.powi(3)
    }
}

/// Particle state and the solver's scratch buffers.
#[derive(Clone, Debug, Default)]
pub struct Fluid {
    pub positions: Vec<[f32; 3]>,
    pub velocities: Vec<[f32; 3]>,
    predicted: Vec<[f32; 3]>,
    lambda: Vec<f32>,
    delta: Vec<[f32; 3]>,
    neighbors: Vec<Vec<u32>>,
}

fn sub(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn dot(a: [f32; 3]) -> f32 {
    a[0] * a[0] + a[1] * a[1] + a[2] * a[2]
}

fn poly6(r2: f32, h: f32) -> f32 {
    let h2 = h * h;
    if r2 >= h2 {
        return 0.0;
    }
    315.0 / (64.0 * PI * h.powi(9)) * (h2 - r2).powi(3)
}

/// Gradient of the spiky kernel with respect to the first point.
fn spiky_gradient(d: [f32; 3], h: f32) -> [f32; 3] {
    let r = dot(d).sqrt();
    if r >= h || r < 1e-6 {
        return [0.0; 3];
    }
    let scale = -45.0 / (PI * h.powi(6)) * (h - r).powi(2) / r;
    [d[0] * scale, d[1] * scale, d[2] * scale]
}

/// Neighbor lists of `points` within `h`. A hash grid with cell size `h` holds the
/// indices in ascending order, so each list comes out sorted.
pub(super) fn neighbors(points: &[[f32; 3]], h: f32) -> Vec<Vec<u32>> {
    let cell = |p: [f32; 3]| {
        [
            (p[0] / h).floor() as i32,
            (p[1] / h).floor() as i32,
            (p[2] / h).floor() as i32,
        ]
    };
    let mut grid: HashMap<[i32; 3], Vec<u32>> = HashMap::new();
    for (i, p) in points.iter().enumerate() {
        grid.entry(cell(*p)).or_default().push(i as u32);
    }
    let h2 = h * h;
    let count = points.len();
    let mut lists = Vec::with_capacity(count);
    for i in 0..count {
        let c = cell(points[i]);
        let mut found = Vec::new();
        for dz in -1..=1 {
            for dy in -1..=1 {
                for dx in -1..=1 {
                    let key = [c[0] + dx, c[1] + dy, c[2] + dz];
                    for &j in grid.get(&key).into_iter().flatten() {
                        let d = sub(points[i], points[j as usize]);
                        if j as usize != i && dot(d) < h2 {
                            found.push(j);
                        }
                    }
                }
            }
        }
        // Cells are visited in a fixed order, but the merged list is
        // only sorted per cell. Sort so the sum order never depends on
        // the grid layout.
        found.sort_unstable();
        lists.push(found);
    }
    lists
}

impl Fluid {
    /// Fills a box of particles on the rest lattice, x fastest.
    pub fn block(min: [f32; 3], counts: [u32; 3], spacing: f32) -> Self {
        let mut positions = Vec::new();
        for z in 0..counts[2] {
            for y in 0..counts[1] {
                for x in 0..counts[0] {
                    positions.push([
                        min[0] + x as f32 * spacing,
                        min[1] + y as f32 * spacing,
                        min[2] + z as f32 * spacing,
                    ]);
                }
            }
        }
        let count = positions.len();
        Self {
            velocities: vec![[0.0; 3]; count],
            positions,
            ..Self::default()
        }
    }

    /// Density at particle `i` from the predicted positions.
    fn density(&self, i: usize, settings: &FluidSettings) -> f32 {
        let h = settings.radius();
        let mut sum = poly6(0.0, h);
        for &j in &self.neighbors[i] {
            let d = sub(self.predicted[i], self.predicted[j as usize]);
            sum += poly6(dot(d), h);
        }
        sum * settings.particle_mass
    }

    fn find_neighbors(&mut self, h: f32) {
        self.neighbors = neighbors(&self.predicted, h);
    }

    fn clamp_to_bounds(p: &mut [f32; 3], bounds: ([f32; 3], [f32; 3])) {
        for axis in 0..3 {
            p[axis] = p[axis].clamp(bounds.0[axis], bounds.1[axis]);
        }
    }

    /// Advances the fluid by `dt` seconds.
    pub fn step(&mut self, settings: &FluidSettings, dt: f32) {
        if dt <= 0.0 {
            return;
        }
        match settings.solver {
            FluidSolver::Particles => {}
            FluidSolver::Grid => return super::flip::step(self, settings, dt),
            FluidSolver::Granular => {
                return super::granular::step(self, settings, dt);
            }
        }
        let count = self.positions.len();
        let h = settings.radius();
        let rest = settings.rest_density();
        self.predicted.clear();
        for i in 0..count {
            let v = &mut self.velocities[i];
            for axis in 0..3 {
                v[axis] += settings.gravity[axis] * dt;
            }
            let mut p = [
                self.positions[i][0] + v[0] * dt,
                self.positions[i][1] + v[1] * dt,
                self.positions[i][2] + v[2] * dt,
            ];
            Self::clamp_to_bounds(&mut p, settings.bounds);
            self.predicted.push(p);
        }
        self.find_neighbors(h);
        self.lambda.resize(count, 0.0);
        self.delta.resize(count, [0.0; 3]);
        // Artificial pressure that keeps particles from clumping.
        let w_ref = poly6((0.2 * h) * (0.2 * h), h);
        // Scene data sets the count; past a few dozen it only stalls the step.
        for _ in 0..settings.iterations.min(MAX_FLUID_ITERATIONS) {
            for i in 0..count {
                let constraint = self.density(i, settings) / rest - 1.0;
                let mut grad_i = [0.0f32; 3];
                let mut sum = 0.0;
                for &j in &self.neighbors[i] {
                    let d = sub(self.predicted[i], self.predicted[j as usize]);
                    let g = spiky_gradient(d, h);
                    let g = [
                        g[0] * settings.particle_mass / rest,
                        g[1] * settings.particle_mass / rest,
                        g[2] * settings.particle_mass / rest,
                    ];
                    sum += dot(g);
                    for axis in 0..3 {
                        grad_i[axis] += g[axis];
                    }
                }
                sum += dot(grad_i);
                // Only push apart: a lone particle is not pulled together.
                self.lambda[i] = if constraint > 0.0 {
                    -constraint / (sum + settings.relaxation)
                } else {
                    0.0
                };
            }
            for i in 0..count {
                let mut total = [0.0f32; 3];
                for &j in &self.neighbors[i] {
                    let d = sub(self.predicted[i], self.predicted[j as usize]);
                    let ratio = poly6(dot(d), h) / w_ref;
                    let scorr = -SCORR * ratio.powi(4);
                    let factor =
                        self.lambda[i] + self.lambda[j as usize] + scorr;
                    let g = spiky_gradient(d, h);
                    for axis in 0..3 {
                        total[axis] += factor * g[axis];
                    }
                }
                let scale = settings.particle_mass / rest;
                self.delta[i] =
                    [total[0] * scale, total[1] * scale, total[2] * scale];
            }
            for i in 0..count {
                for axis in 0..3 {
                    self.predicted[i][axis] += self.delta[i][axis];
                }
                Self::clamp_to_bounds(&mut self.predicted[i], settings.bounds);
            }
        }
        for i in 0..count {
            for axis in 0..3 {
                self.velocities[i][axis] =
                    (self.predicted[i][axis] - self.positions[i][axis]) / dt;
            }
        }
        // XSPH viscosity, from a copy so the order of particles is
        // irrelevant.
        if settings.viscosity > 0.0 {
            let old = self.velocities.clone();
            for i in 0..count {
                let mut correction = [0.0f32; 3];
                for &j in &self.neighbors[i] {
                    let d = sub(self.predicted[i], self.predicted[j as usize]);
                    let w = poly6(dot(d), h) / poly6(0.0, h);
                    for axis in 0..3 {
                        correction[axis] +=
                            (old[j as usize][axis] - old[i][axis]) * w;
                    }
                }
                for axis in 0..3 {
                    self.velocities[i][axis] +=
                        settings.viscosity * correction[axis];
                }
            }
        }
        std::mem::swap(&mut self.positions, &mut self.predicted);
    }

    /// Hash of every position and velocity bit, in particle order.
    pub fn state_hash(&self) -> u64 {
        let mut hash = 0xcbf2_9ce4_8422_2325u64;
        for value in self.positions.iter().chain(&self.velocities).flatten() {
            hash = (hash ^ u64::from(value.to_bits()))
                .wrapping_mul(0x0000_0100_0000_01b3);
        }
        hash
    }
}

/// Marks the entity that draws one particle of the [`FluidVolume`] on the
/// given entity. The editor hides these from the Hierarchy and Scene View
/// picking, and `sync_fluid_visuals` despawns them with their volume.
#[derive(bevy_ecs::prelude::Component, Clone, Copy, Debug, PartialEq, Eq)]
pub struct FluidParticle(pub bevy_ecs::prelude::Entity);

/// Marks a surface entity ([`FluidSurface`] or [`super::WaterMesh`]) and the
/// assets it owns, so `reap_surfaces` can free them when the surface goes.
#[derive(bevy_ecs::prelude::Component, Clone, Copy, Debug)]
pub struct OwnedSurface {
    pub mesh: crate::assets::Handle<crate::assets::MeshAsset>,
    /// A material only this surface uses; shared materials stay `None`.
    pub material: Option<crate::assets::Handle<crate::assets::MaterialAsset>>,
}

/// A fluid on an entity. It steps once per fixed tick; volumes do not
/// interact, so their order does not matter. It is runtime state made by
/// game code: it is not saved in scene files or reflected yet.
#[derive(bevy_ecs::prelude::Component, Clone, Debug, Default)]
pub struct FluidVolume {
    pub settings: FluidSettings,
    pub fluid: Fluid,
    /// Draws each particle as this mesh, scaled to the particle spacing (a
    /// unit-diameter mesh such as the built-in sphere fits). Without it the
    /// fluid is simulated but not drawn.
    pub visual: Option<super::MeshRenderer>,
    /// Draws the fluid as one smooth surface mesh that `sync_fluid_surfaces`
    /// rebuilds every fixed tick.
    pub surface: Option<FluidSurface>,
    /// One entity per particle, kept by [`sync_fluid_visuals`].
    pub(super) particles: Vec<bevy_ecs::prelude::Entity>,
}

/// How a [`FluidVolume`] draws its surface.
#[derive(Clone, Copy, Debug)]
pub struct FluidSurface {
    pub material: crate::assets::Handle<crate::assets::MaterialAsset>,
    mesh: Option<crate::assets::Handle<crate::assets::MeshAsset>>,
    entity: Option<bevy_ecs::prelude::Entity>,
    /// Hash of the particles the mesh was built from; a fluid at rest keeps
    /// its mesh.
    built_from: u64,
}

impl FluidSurface {
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

impl FluidVolume {
    pub fn new(settings: FluidSettings, fluid: Fluid) -> Self {
        Self {
            settings,
            fluid,
            ..Self::default()
        }
    }
}

/// Authoring form of a fluid: a block of particles resting in a box. A
/// fixed step turns it into a [`FluidVolume`] on the same entity. The box is
/// centered on the entity's position and the particles fill its floor up to
/// `count_y` layers.
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
pub struct FluidBlock {
    /// Rest spacing of particles in meters.
    pub spacing: f32,
    pub count_x: u32,
    pub count_y: u32,
    pub count_z: u32,
    /// Half the size of the container box, in meters.
    pub container_half_extents: [f32; 3],
    /// Density-constraint iterations per step.
    pub iterations: u32,
    /// XSPH viscosity in 0..1.
    pub viscosity: f32,
    /// Particles for small splashy volumes, Grid for large water volumes,
    /// Granular for sand and gravel.
    pub solver: FluidSolver,
    /// Grain friction for the Granular solver; higher piles steeper.
    pub friction: f32,
    /// Draw the fluid as a water surface.
    pub visible: bool,
    /// Also draw each particle as a sphere (for debugging the solver).
    pub show_particles: bool,
}

impl Default for FluidBlock {
    fn default() -> Self {
        Self {
            spacing: 0.1,
            count_x: 6,
            count_y: 6,
            count_z: 6,
            container_half_extents: [0.5, 0.5, 0.5],
            iterations: 4,
            viscosity: 0.01,
            solver: FluidSolver::Particles,
            friction: 0.6,
            visible: true,
            show_particles: false,
        }
    }
}

/// Per fixed step, before [`couple_fluids`]: gives every [`FluidBlock`]
/// without a volume its [`FluidVolume`].
pub(super) fn spawn_fluid_volumes(
    mut commands: bevy_ecs::prelude::Commands,
    mut assets: Option<bevy_ecs::prelude::ResMut<crate::assets::AssetServer>>,
    blocks: bevy_ecs::prelude::Query<
        (bevy_ecs::prelude::Entity, &FluidBlock, &crate::Transform),
        bevy_ecs::prelude::Without<FluidVolume>,
    >,
) {
    for (entity, block, transform) in &blocks {
        let volume = volume_for_block(block, transform, assets.as_deref_mut());
        commands.entity(entity).insert(volume);
    }
}

/// The volume a [`FluidBlock`] starts as.
fn volume_for_block(
    block: &FluidBlock,
    transform: &crate::Transform,
    assets: Option<&mut crate::assets::AssetServer>,
) -> FluidVolume {
    let center = transform.position;
    let half = block.container_half_extents;
    let settings = FluidSettings {
        spacing: block.spacing.max(0.01),
        iterations: block.iterations,
        viscosity: block.viscosity,
        solver: block.solver,
        friction: block.friction,
        bounds: (
            std::array::from_fn(|axis| center[axis] - half[axis]),
            std::array::from_fn(|axis| center[axis] + half[axis]),
        ),
        ..FluidSettings::default()
    };
    let counts = capped_counts([block.count_x, block.count_y, block.count_z]);
    let start = [
        center[0] - counts[0] as f32 * settings.spacing / 2.0,
        center[1] - half[1] + settings.spacing / 2.0,
        center[2] - counts[2] as f32 * settings.spacing / 2.0,
    ];
    let mut volume = FluidVolume::new(
        settings,
        Fluid::block(start, counts, settings.spacing),
    );
    if let Some(assets) = assets {
        if block.show_particles {
            volume.visual = Some(super::MeshRenderer {
                mesh: assets.builtin_primitives
                    [&crate::assets::PrimitiveShape::Sphere],
                material: assets.fallback_material,
                cast_shadows: true,
                receive_shadows: true,
            });
        }
        if block.visible {
            volume.surface = Some(FluidSurface::new(water_material(assets)));
        }
    }
    volume
}

/// The particle state of every fluid, by scene object, for
/// [`restore_fluids`].
pub fn capture_fluids(
    world: &mut bevy_ecs::prelude::World,
) -> Vec<(uuid::Uuid, Fluid)> {
    let mut query = world.query::<(&super::SceneId, &FluidVolume)>();
    query
        .iter(world)
        .map(|(id, volume)| (id.0, volume.fluid.clone()))
        .collect()
}

/// Gives each fluid block in `saved` its captured particles, replacing the
/// fresh block the scene load would start it as.
pub fn restore_fluids(
    world: &mut bevy_ecs::prelude::World,
    saved: &[(uuid::Uuid, Fluid)],
) {
    let mut query = world.query::<(
        bevy_ecs::prelude::Entity,
        &super::SceneId,
        &FluidBlock,
        &crate::Transform,
    )>();
    let blocks: Vec<_> = query
        .iter(world)
        .filter_map(|(entity, id, block, transform)| {
            let (_, fluid) = saved.iter().find(|(saved, _)| *saved == id.0)?;
            Some((entity, *block, *transform, fluid.clone()))
        })
        .collect();
    for (entity, block, transform, fluid) in blocks {
        let mut assets = world.remove_resource::<crate::assets::AssetServer>();
        let mut volume = volume_for_block(&block, &transform, assets.as_mut());
        if let Some(assets) = assets {
            world.insert_resource(assets);
        }
        volume.fluid = fluid;
        world.entity_mut(entity).insert(volume);
    }
}

/// Most particles one block spawns; a larger block would freeze the engine.
pub const MAX_BLOCK_PARTICLES: u64 = 50_000;

/// Shrinks the largest axis until the block fits [`MAX_BLOCK_PARTICLES`].
fn capped_counts(mut counts: [u32; 3]) -> [u32; 3] {
    while counts
        .iter()
        .map(|&count| u64::from(count))
        .product::<u64>()
        > MAX_BLOCK_PARTICLES
    {
        let axis = (0..3).max_by_key(|&axis| counts[axis]).unwrap_or(0);
        counts[axis] /= 2;
    }
    counts
}

/// The shared translucent blue material of fluid blocks.
fn water_material(
    assets: &mut crate::assets::AssetServer,
) -> crate::assets::Handle<crate::assets::MaterialAsset> {
    if let Some((handle, _)) = assets
        .materials
        .iter()
        .find(|(_, material)| material.name == "Fluid Block Water")
    {
        return handle;
    }
    assets.materials.insert(crate::assets::MaterialAsset {
        name: "Fluid Block Water".into(),
        alpha_mode: crate::assets::AlphaMode::Blend,
        base_color: [0.15, 0.45, 0.85, 0.65],
        roughness: 0.08,
        ..crate::assets::MaterialAsset::default()
    })
}

/// Per fixed step: despawns surface entities whose fluid or water no longer
/// points at them (component removed, surface switched off, owner gone) and
/// frees their mesh and own material.
pub(super) fn reap_surfaces(
    mut commands: bevy_ecs::prelude::Commands,
    assets: Option<bevy_ecs::prelude::ResMut<crate::assets::AssetServer>>,
    surfaces: bevy_ecs::prelude::Query<(
        bevy_ecs::prelude::Entity,
        &FluidParticle,
        &OwnedSurface,
    )>,
    volumes: bevy_ecs::prelude::Query<&FluidVolume>,
    waters: bevy_ecs::prelude::Query<&super::WaterMesh>,
    soft: bevy_ecs::prelude::Query<&super::SoftBodyVolume>,
    cloths: bevy_ecs::prelude::Query<&super::ClothVolume>,
) {
    let Some(mut assets) = assets else {
        return;
    };
    for (entity, owner, owned) in &surfaces {
        let kept = volumes.get(owner.0).is_ok_and(|volume| {
            volume
                .surface
                .as_ref()
                .is_some_and(|surface| surface.entity == Some(entity))
        }) || waters
            .get(owner.0)
            .is_ok_and(|water| water.entity() == entity)
            || soft.get(owner.0).is_ok_and(|volume| {
                volume
                    .skin
                    .as_ref()
                    .is_some_and(|skin| skin.entity == Some(entity))
            })
            || cloths.get(owner.0).is_ok_and(|cloth| {
                cloth
                    .skin
                    .as_ref()
                    .is_some_and(|skin| skin.entity == Some(entity))
            });
        if kept {
            continue;
        }
        commands.entity(entity).despawn();
        let _ = assets.meshes.remove(owned.mesh);
        if let Some(material) = owned.material {
            let _ = assets.materials.remove(material);
        }
    }
}

/// Per fixed step: advances every `FluidVolume`, then lets volumes push on
/// each other (see [`separate_volumes`]), in `SpawnOrder`.
pub(super) fn step_fluids(
    time: bevy_ecs::prelude::Res<super::FrameTime>,
    mut volumes: bevy_ecs::prelude::Query<(
        bevy_ecs::prelude::Entity,
        Option<&super::SpawnOrder>,
        &mut FluidVolume,
    )>,
) {
    let dt = time.fixed_delta.as_secs_f32();
    let mut volumes: Vec<_> = volumes.iter_mut().collect();
    volumes.sort_by_key(|(entity, order, _)| visit_key(*order, *entity));
    let mut volumes: Vec<&mut FluidVolume> = volumes
        .iter_mut()
        .map(|(.., volume)| &mut **volume)
        .collect();
    for volume in &mut volumes {
        let FluidVolume {
            settings, fluid, ..
        } = &mut **volume;
        fluid.step(settings, dt);
    }
    separate_volumes(&mut volumes, dt);
}

/// Passes of [`separate_volumes`] per step.
const SEPARATION_PASSES: usize = 4;

/// Pushes apart particles of different volumes closer than the mean of
/// their spacings, shared by particle mass so momentum is kept, and gives the
/// push to their velocities: so water and sand mix, a heavy fluid sinks
/// through a light one, and both stay in their own containers. Pairs of
/// volumes and particles go in index order.
// ponytail: a few Gauss-Seidel passes after the volumes step, so the
// volumes feel each other's weight but not each other's pressure; solve
// them in one density solve if layered fluids must hold rest density.
fn separate_volumes(volumes: &mut [&mut FluidVolume], dt: f32) {
    for _ in 0..SEPARATION_PASSES {
        for b in 1..volumes.len() {
            let (head, tail) = volumes.split_at_mut(b);
            let other = &mut *tail[0];
            for volume in head.iter_mut() {
                separate(volume, other, dt);
            }
        }
    }
}

/// One pass of [`separate_volumes`] over the particles of `a` and `b`.
fn separate(a: &mut FluidVolume, b: &mut FluidVolume, dt: f32) {
    let (mass_a, mass_b) = (a.settings.particle_mass, b.settings.particle_mass);
    if mass_a <= 0.0 || mass_b <= 0.0 {
        return;
    }
    let reach = (a.settings.spacing + b.settings.spacing) / 2.0;
    let cell = |p: [f32; 3]| p.map(|x| (x / reach).floor() as i32);
    let mut grid: HashMap<[i32; 3], Vec<usize>> = HashMap::new();
    for (j, &p) in b.fluid.positions.iter().enumerate() {
        grid.entry(cell(p)).or_default().push(j);
    }
    let share_a = mass_b / (mass_a + mass_b);
    let (bounds_a, bounds_b) = (a.settings.bounds, b.settings.bounds);
    let (fa, fb) = (&mut a.fluid, &mut b.fluid);
    for i in 0..fa.positions.len() {
        let [x, y, z] = cell(fa.positions[i]);
        for offset in 0..27 {
            let key = [
                x + offset % 3 - 1,
                y + offset / 3 % 3 - 1,
                z + offset / 9 - 1,
            ];
            let Some(near) = grid.get(&key) else {
                continue;
            };
            for &j in near {
                let delta = Vector3::from(fa.positions[i])
                    - Vector3::from(fb.positions[j]);
                let length = delta.norm();
                if length >= reach || length < 1e-6 {
                    continue;
                }
                let push = delta * ((reach - length) / length);
                let (move_a, move_b) = (push * share_a, push * (1.0 - share_a));
                for (p, v, d, (low, high)) in [
                    (
                        &mut fa.positions[i],
                        &mut fa.velocities[i],
                        move_a,
                        bounds_a,
                    ),
                    (
                        &mut fb.positions[j],
                        &mut fb.velocities[j],
                        -move_b,
                        bounds_b,
                    ),
                ] {
                    // A push into a wall stays in the container.
                    let to = (Vector3::from(*p) + d)
                        .sup(&Vector3::from(low))
                        .inf(&Vector3::from(high));
                    *v = (Vector3::from(*v) + (to - Vector3::from(*p)) / dt)
                        .into();
                    *p = to.into();
                }
            }
        }
    }
}

/// Stable order for anything that visits several objects: [`SpawnOrder`]
/// first (it survives reloads and restores), then entity order for objects
/// without one.
pub(super) fn visit_key(
    order: Option<&super::SpawnOrder>,
    entity: bevy_ecs::prelude::Entity,
) -> (bool, u64, bevy_ecs::prelude::Entity) {
    (order.is_none(), order.map_or(0, |order| order.0), entity)
}

/// Per fixed step, after [`step_fluids`]: gives each particle of a volume
/// with a `visual` an entity and moves it to the particle. Extra entities
/// are despawned when the particle count drops or the visual is removed, and
/// so are the particles of a volume that no longer exists. A volume that
/// holds particles owned by another volume (a copy) makes its own.
pub(super) fn sync_fluid_visuals(
    mut commands: bevy_ecs::prelude::Commands,
    mut volumes: bevy_ecs::prelude::Query<(
        bevy_ecs::prelude::Entity,
        &mut FluidVolume,
    )>,
    particles: bevy_ecs::prelude::Query<
        (bevy_ecs::prelude::Entity, &FluidParticle),
        bevy_ecs::prelude::Without<OwnedSurface>,
    >,
    mut transforms: bevy_ecs::prelude::Query<&mut crate::Transform>,
    waters: bevy_ecs::prelude::Query<
        (),
        bevy_ecs::prelude::With<super::WaterBody>,
    >,
) {
    for (particle, owner) in &particles {
        if !volumes.contains(owner.0) && !waters.contains(owner.0) {
            commands.entity(particle).despawn();
        }
    }
    for (owner, mut volume) in &mut volumes {
        let volume = &mut *volume;
        let owns = |entity| {
            particles
                .get(entity)
                .is_ok_and(|(_, marker)| marker.0 == owner)
        };
        let wanted = if volume.visual.is_some() {
            volume.fluid.positions.len()
        } else {
            0
        };
        for entity in
            volume.particles.drain(wanted.min(volume.particles.len())..)
        {
            if owns(entity) {
                commands.entity(entity).despawn();
            }
        }
        let Some(visual) = volume.visual else {
            continue;
        };
        let scale = [volume.settings.spacing; 3];
        for (index, position) in volume.fluid.positions.iter().enumerate() {
            match volume.particles.get(index) {
                Some(&entity) if owns(entity) => {
                    if let Ok(mut transform) = transforms.get_mut(entity) {
                        transform.position = *position;
                    }
                }
                slot => {
                    let transform = crate::Transform {
                        position: *position,
                        scale,
                        ..crate::Transform::default()
                    };
                    let id = commands
                        .spawn((transform, visual, FluidParticle(owner)))
                        .id();
                    if slot.is_some() {
                        volume.particles[index] = id;
                    } else {
                        volume.particles.push(id);
                    }
                }
            }
        }
    }
}

/// Trades momentum between a particle pushed out of a body along `outward`
/// and the body: a particle moving into the body leaves with the body's
/// speed along `outward`, and the body takes the momentum it lost.
/// `share` is the particle's mass over the body's.
fn trade(
    velocity: &mut [f32; 3],
    outward: [f32; 3],
    body: &mut [f32; 3],
    share: f32,
) {
    let closing: f32 = (0..3)
        .map(|axis| (velocity[axis] - body[axis]) * outward[axis])
        .sum();
    if closing >= 0.0 {
        return;
    }
    // Split the closing speed so total momentum is kept: the particle and
    // the body end with the same speed along `outward`.
    let particle = -closing / (1.0 + share);
    for axis in 0..3 {
        velocity[axis] += particle * outward[axis];
        body[axis] -= particle * share * outward[axis];
    }
}

/// Per fixed step, before [`step_fluids`]: couples `FluidVolume`s with
/// dynamic bodies that have a sphere collider. A body gets buoyancy from the
/// volume of sphere its overlapping particles fill, and drag; particles
/// inside the sphere are pushed out to its surface, which displaces the
/// fluid, and trade momentum with the body along the way out (see
/// [`trade`]), so a stream pushes a body and a moving body drags the fluid.
/// Bodies and volumes are visited in `SpawnOrder` (entity order for those
/// without one), and each body reads the particles as the previous body left
/// them.
///
/// A GPU body takes part when it reads its state back (a [`super::GpuStateMirror`]
/// at most [`MAX_COUPLED_STATE_AGE`] ticks old): the fluid meets it at the
/// mirrored pose carried forward by its velocity over the state's age, and
/// what the fluid does to its velocity goes back as an impulse that applies
/// before the next GPU tick (see [`super::GpuPhysicsCommands`]).
/// ponytail: sphere, box and capsule colliders only (a box at a rest pose has no
/// torque from the fluid), and every body checks every particle.
/// Add other shapes and use the solver's grid when this shows in profiles.
#[allow(clippy::type_complexity)]
pub(super) fn couple_fluids(
    time: bevy_ecs::prelude::Res<super::FrameTime>,
    physics: bevy_ecs::prelude::Res<super::PhysicsSettings>,
    gpu_commands: Option<bevy_ecs::prelude::ResMut<super::GpuPhysicsCommands>>,
    mut volumes: bevy_ecs::prelude::Query<(
        bevy_ecs::prelude::Entity,
        Option<&super::SpawnOrder>,
        &mut FluidVolume,
    )>,
    // A child body is placed by its parent, so fluids skip it.
    mut bodies: bevy_ecs::prelude::Query<
        (
            bevy_ecs::prelude::Entity,
            Option<&super::SpawnOrder>,
            &crate::Transform,
            &mut super::RigidBody,
            &super::Collider,
            Option<&super::PhysicsBody>,
            Option<&super::PhysicsId>,
            Option<&super::GpuStateMirror>,
        ),
        bevy_ecs::prelude::Without<super::Parent>,
    >,
) {
    use super::{ColliderShape, RigidBodyKind, SimulationClass};
    let dt = time.fixed_delta.as_secs_f32();
    let gravity = physics.gravity;
    let gravity_size = dot(gravity).sqrt();
    if volumes.is_empty() {
        return;
    }
    // Copies of the coupled bodies, with the velocity they started at and
    // the GPU id of a GPU body.
    let mut coupled = Vec::new();
    for (entity, order, transform, body, collider, physics, id, mirror) in
        &bodies
    {
        if body.kind != RigidBodyKind::Dynamic || body.mass <= 0.0 {
            continue;
        }
        match (physics.map(|p| p.simulation), id, mirror) {
            (Some(SimulationClass::Cpu), ..) => coupled.push((
                visit_key(order, entity),
                *transform,
                *body,
                *collider,
                body.linear_velocity,
                None,
            )),
            (Some(SimulationClass::Gpu), Some(id), Some(mirror))
                if gpu_commands.is_some()
                    && mirror.age_ticks(time.fixed_tick)
                        <= MAX_COUPLED_STATE_AGE =>
            {
                let age = mirror.age_ticks(time.fixed_tick) as f32 * dt;
                let mut pose = mirror.transform;
                for axis in 0..3 {
                    pose.position[axis] += mirror.linear_velocity[axis] * age;
                }
                let mut body = *body;
                body.linear_velocity = mirror.linear_velocity;
                coupled.push((
                    visit_key(order, entity),
                    pose,
                    body,
                    *collider,
                    mirror.linear_velocity,
                    Some(*id),
                ));
            }
            _ => {}
        }
    }
    coupled.sort_by_key(|(key, ..)| *key);
    let mut volumes: Vec<_> = volumes.iter_mut().collect();
    volumes.sort_by_key(|(entity, order, _)| visit_key(*order, *entity));
    for (_, _, volume) in &mut volumes {
        let FluidVolume {
            settings, fluid, ..
        } = &mut **volume;
        let particle_volume = settings.spacing.powi(3);
        for (_, transform, body, collider, ..) in &mut coupled {
            let center = transform.position;
            // Particles never leave `bounds`, so a body clear of that box
            // touches none of them.
            let scale = transform.scale.into_iter().fold(0.0, f32::max);
            let reach = scale
                * match collider.shape {
                    ColliderShape::Sphere { radius } => radius,
                    ColliderShape::Box { half_extents } => {
                        dot(half_extents).sqrt()
                    }
                    ColliderShape::Capsule {
                        half_height,
                        radius,
                    } => half_height + radius,
                    _ => continue,
                };
            let (low, high) = settings.bounds;
            let gap: [f32; 3] = std::array::from_fn(|axis| {
                (low[axis] - center[axis])
                    .max(center[axis] - high[axis])
                    .max(0.0)
            });
            if dot(gap) > reach * reach {
                continue;
            }
            let mut inside = 0u32;
            let share = settings.particle_mass / body.mass;
            let displaced = match collider.shape {
                ColliderShape::Sphere { radius } => {
                    let radius = radius
                        * transform.scale.into_iter().fold(0.0, f32::max);
                    for (position, velocity) in
                        fluid.positions.iter_mut().zip(&mut fluid.velocities)
                    {
                        let offset = sub(*position, center);
                        let distance = dot(offset).sqrt();
                        if distance >= radius {
                            continue;
                        }
                        inside += 1;
                        // Straight up is the way out of a sphere that is
                        // exactly on a particle.
                        let outward = if distance > 1e-6 {
                            offset.map(|axis| axis / distance)
                        } else {
                            [0.0, 1.0, 0.0]
                        };
                        *position = std::array::from_fn(|axis| {
                            center[axis] + outward[axis] * radius
                        });
                        trade(
                            velocity,
                            outward,
                            &mut body.linear_velocity,
                            share,
                        );
                    }
                    4.0 / 3.0 * PI * radius.powi(3)
                }
                ColliderShape::Box { half_extents } => {
                    let half: [f32; 3] = std::array::from_fn(|axis| {
                        half_extents[axis] * transform.scale[axis]
                    });
                    let rotation = super::sim_math::rotation_from_euler(
                        transform.rotation[0],
                        transform.rotation[1],
                        transform.rotation[2],
                    );
                    for (position, velocity) in
                        fluid.positions.iter_mut().zip(&mut fluid.velocities)
                    {
                        let offset = sub(*position, center);
                        let mut local =
                            rotation.inverse() * Vector3::from(offset);
                        // Leave through the face with the least penetration.
                        let Some((axis, depth)) = (0..3)
                            .map(|axis| (axis, half[axis] - local[axis].abs()))
                            .min_by(|a, b| a.1.total_cmp(&b.1))
                            .filter(|(_, depth)| *depth > 0.0)
                            .filter(|_| {
                                (0..3)
                                    .all(|axis| local[axis].abs() < half[axis])
                            })
                        else {
                            continue;
                        };
                        inside += 1;
                        let sign = if local[axis] < 0.0 { -1.0 } else { 1.0 };
                        local[axis] += sign * depth;
                        let world = rotation * local;
                        *position =
                            std::array::from_fn(|i| center[i] + world[i]);
                        let mut face = Vector3::zeros();
                        face[axis] = sign;
                        let outward = rotation * face;
                        trade(
                            velocity,
                            outward.into(),
                            &mut body.linear_velocity,
                            share,
                        );
                    }
                    8.0 * half[0] * half[1] * half[2]
                }
                ColliderShape::Capsule {
                    half_height,
                    radius,
                } => {
                    let radius =
                        radius * transform.scale[0].max(transform.scale[2]);
                    let half = half_height * transform.scale[1];
                    let rotation = super::sim_math::rotation_from_euler(
                        transform.rotation[0],
                        transform.rotation[1],
                        transform.rotation[2],
                    );
                    let axis = rotation * Vector3::y();
                    for (position, velocity) in
                        fluid.positions.iter_mut().zip(&mut fluid.velocities)
                    {
                        let offset = Vector3::from(sub(*position, center));
                        // The nearest point of the core segment, then the
                        // same push as a sphere from there.
                        let along = offset.dot(&axis).clamp(-half, half);
                        let from_core = offset - axis * along;
                        let distance = from_core.norm();
                        if distance >= radius {
                            continue;
                        }
                        inside += 1;
                        let outward = if distance > 1e-6 {
                            from_core / distance
                        } else {
                            Vector3::y()
                        };
                        let pushed = axis * along + outward * radius;
                        *position =
                            std::array::from_fn(|i| center[i] + pushed[i]);
                        trade(
                            velocity,
                            outward.into(),
                            &mut body.linear_velocity,
                            share,
                        );
                    }
                    PI * radius * radius * 2.0 * half
                        + 4.0 / 3.0 * PI * radius.powi(3)
                }
                _ => continue,
            };
            let sphere = displaced;
            let filled = (inside as f32 * particle_volume / sphere).min(1.0);
            // Grains hold a body up by contact (the push and trade above),
            // not by buoyancy.
            if filled <= 0.0
                || gravity_size <= 0.0
                || settings.solver == FluidSolver::Granular
            {
                continue;
            }
            let lift = settings.rest_density() * filled * sphere * gravity_size
                / body.mass;
            for axis in 0..3 {
                body.linear_velocity[axis] -=
                    gravity[axis] / gravity_size * lift * dt;
                body.linear_velocity[axis] *=
                    1.0 - (2.0 * filled * dt).min(1.0);
            }
        }
    }
    let mut gpu_commands = gpu_commands;
    for ((_, _, entity), _, body, _, start, id) in coupled {
        if body.linear_velocity == start {
            continue;
        }
        match (id, gpu_commands.as_mut()) {
            (Some(id), Some(commands)) => commands.push(
                id,
                super::GpuBodyCommand::Impulse(std::array::from_fn(|axis| {
                    body.mass * (body.linear_velocity[axis] - start[axis])
                })),
            ),
            _ => {
                if let Ok((.., mut rigid, _, _, _, _)) = bodies.get_mut(entity)
                {
                    rigid.linear_velocity = body.linear_velocity;
                }
            }
        }
    }
}

/// Oldest read-back GPU state, in fixed ticks, that fluids still couple
/// with. Readback normally runs one to three frames behind; a body whose
/// state is older (paused readback, a stalled GPU) is left out rather than
/// pushed from a pose that may be far from where it is.
pub const MAX_COUPLED_STATE_AGE: u64 = 8;

/// Per fixed step, after [`step_fluids`]: keeps one entity and one mesh per
/// volume with a `surface` and rebuilds the mesh from the particles. A
/// removed `surface` takes its entity and mesh away. A copied volume makes
/// its own entity and mesh. The mesh of a despawned volume stays in the
/// asset server (the entity goes with [`sync_fluid_visuals`]).
/// ponytail: the mesh uploads again each tick; a GPU surface pass is the
/// upgrade if this shows in profiles.
pub(super) fn sync_fluid_surfaces(
    mut commands: bevy_ecs::prelude::Commands,
    assets: Option<bevy_ecs::prelude::ResMut<crate::assets::AssetServer>>,
    mut volumes: bevy_ecs::prelude::Query<(
        bevy_ecs::prelude::Entity,
        &mut FluidVolume,
    )>,
    markers: bevy_ecs::prelude::Query<&FluidParticle>,
) {
    let Some(mut assets) = assets else {
        return;
    };
    for (owner, mut volume) in &mut volumes {
        let volume = &mut *volume;
        let Some(surface) = volume.surface.as_mut() else {
            continue;
        };
        let owned = surface.entity.is_some_and(|entity| {
            markers.get(entity).is_ok_and(|marker| marker.0 == owner)
        });
        if !owned {
            surface.mesh = None;
            surface.entity = None;
        }
        let hash = volume.fluid.positions.iter().flatten().fold(
            0xcbf2_9ce4_8422_2325_u64,
            |hash, axis| {
                (hash ^ u64::from(axis.to_bits()))
                    .wrapping_mul(0x0100_0000_01b3)
            },
        );
        // A freed mesh (asset reset, reload) rebuilds even if nothing moved.
        let live = surface
            .mesh
            .is_some_and(|handle| assets.meshes.get(handle).is_some());
        if live && surface.built_from == hash {
            continue;
        }
        surface.built_from = hash;
        let mesh = super::fluid_surface::surface_mesh(
            &volume.fluid.positions,
            volume.settings.spacing,
        );
        match surface
            .mesh
            .and_then(|handle| assets.meshes.get_mut(handle))
        {
            Some(slot) => *slot = mesh,
            None => {
                let handle = assets.meshes.insert(mesh);
                surface.mesh = Some(handle);
                surface.entity = Some(
                    commands
                        .spawn((
                            crate::Transform::default(),
                            super::MeshRenderer {
                                mesh: handle,
                                material: surface.material,
                                cast_shadows: false,
                                receive_shadows: true,
                            },
                            FluidParticle(owner),
                            OwnedSurface {
                                mesh: handle,
                                material: None,
                            },
                        ))
                        .id(),
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_trade_keeps_momentum_and_only_acts_on_closing_particles() {
        let (mut particle, mut body) = ([0.0, -2.0, 1.0], [0.0, 1.0, 0.0]);
        super::trade(&mut particle, [0.0, 1.0, 0.0], &mut body, 0.25);
        // 0.25 * -2 + 1 = 0.5 before; both move at 0.4 along y after.
        assert_eq!([particle[0], particle[2]], [0.0, 1.0]);
        assert!((particle[1] - 0.4).abs() < 1e-6, "{particle:?}");
        assert!((body[1] - 0.4).abs() < 1e-6, "{body:?}");
        assert!((0.25 * particle[1] + body[1] - 0.5).abs() < 1e-6);
        let leaving = [0.0, 3.0, 0.0];
        let mut moved = leaving;
        super::trade(&mut moved, [0.0, 1.0, 0.0], &mut body, 0.25);
        assert_eq!(moved, leaving);
    }

    use super::*;

    #[test]
    fn huge_blocks_are_shrunk_to_the_cap() {
        let counts = capped_counts([1000, 1000, 1000]);
        let total: u64 = counts.iter().map(|&count| u64::from(count)).product();
        assert!(total > 0 && total <= MAX_BLOCK_PARTICLES);
        assert_eq!(capped_counts([6, 6, 6]), [6, 6, 6]);
    }

    fn settings() -> FluidSettings {
        FluidSettings {
            bounds: ([-0.5, 0.0, -0.5], [0.5, 2.0, 0.5]),
            ..FluidSettings::default()
        }
    }

    fn column() -> Fluid {
        Fluid::block([-0.4, 0.0, -0.4], [8, 12, 8], 0.1)
    }

    #[test]
    fn the_same_start_gives_the_same_bits() {
        let (mut a, mut b) = (column(), column());
        for _ in 0..30 {
            a.step(&settings(), 1.0 / 60.0);
            b.step(&settings(), 1.0 / 60.0);
        }
        assert_eq!(a.state_hash(), b.state_hash());
    }

    #[test]
    fn a_column_falls_settles_and_stays_inside_the_container() {
        let s = settings();
        let mut fluid = column();
        for _ in 0..240 {
            fluid.step(&s, 1.0 / 60.0);
        }
        let top = fluid
            .positions
            .iter()
            .map(|p| p[1])
            .fold(f32::MIN, f32::max);
        let speed = fluid
            .velocities
            .iter()
            .map(|v| dot(*v).sqrt())
            .fold(0.0, f32::max);
        for p in &fluid.positions {
            for axis in 0..3 {
                assert!(p[axis] >= s.bounds.0[axis] - 1e-4);
                assert!(p[axis] <= s.bounds.1[axis] + 1e-4);
                assert!(p[axis].is_finite());
            }
        }
        // 768 particles of 0.1 m spacing fill 0.8 m * 0.8 m * 1.2 m; the
        // 1 m * 1 m floor spreads them to well under the starting height.
        assert!(top < 1.0, "column did not collapse: top {top}");
        assert!(speed < 3.0, "fluid did not settle: speed {speed}");
    }

    #[test]
    fn a_settled_pool_keeps_its_density_near_rest() {
        let s = settings();
        let mut fluid = column();
        for _ in 0..240 {
            fluid.step(&s, 1.0 / 60.0);
        }
        fluid.predicted = fluid.positions.clone();
        fluid.find_neighbors(s.radius());
        let mean = (0..fluid.positions.len())
            .map(|i| fluid.density(i, &s))
            .sum::<f32>()
            / fluid.positions.len() as f32;
        let ratio = mean / s.rest_density();
        assert!((0.5..1.6).contains(&ratio), "density ratio {ratio}");
    }

    fn run_app(app: &mut crate::runtime::App, ticks: u32) {
        for _ in 0..ticks {
            app.update(std::time::Duration::from_secs_f64(1.0 / 60.0))
                .unwrap();
        }
    }

    /// Impulses one tick of a water block sends a 1 kg GPU ball whose
    /// read-back state is `age` ticks old, at `position` moving at
    /// `velocity`.
    fn gpu_ball_impulses(
        position: [f32; 3],
        velocity: [f32; 3],
        age: u64,
    ) -> Vec<[f32; 3]> {
        use crate::runtime::{
            Collider, ColliderShape, FrameTime, GpuBodyCommand,
            GpuPhysicsCommands, GpuStateMirror, PhysicsBody, PhysicsId,
            RigidBody, SimulationClass,
        };
        let mut app = crate::runtime::App::new();
        app.world_mut()
            .insert_resource(GpuPhysicsCommands::default());
        app.world_mut().resource_mut::<FrameTime>().fixed_tick = 20;
        let settings = FluidSettings {
            bounds: ([-0.4, 0.0, -0.4], [0.4, 1.0, 0.4]),
            ..FluidSettings::default()
        };
        let water = Fluid::block([-0.3, 0.0, -0.3], [7, 4, 7], 0.1);
        app.world_mut().spawn(FluidVolume::new(settings, water));
        let transform = crate::Transform {
            position,
            ..crate::Transform::default()
        };
        let id = PhysicsId {
            slot: 7,
            generation: 1,
        };
        app.world_mut().spawn((
            transform,
            RigidBody {
                mass: 1.0,
                ..RigidBody::default()
            },
            Collider {
                shape: ColliderShape::Sphere { radius: 0.12 },
                ..Collider::default()
            },
            PhysicsBody {
                simulation: SimulationClass::Gpu,
                ..PhysicsBody::default()
            },
            id,
            GpuStateMirror {
                tick: 20 - age,
                transform,
                linear_velocity: velocity,
                angular_velocity: [0.0; 3],
                custom_values: None,
            },
        ));
        run_app(&mut app, 1);
        app.world()
            .resource::<GpuPhysicsCommands>()
            .commands
            .iter()
            .map(|&(body, command)| match (body == id, command) {
                (true, GpuBodyCommand::Impulse(impulse)) => impulse,
                other => panic!("unexpected command {other:?}"),
            })
            .collect()
    }

    #[test]
    fn water_pushes_a_gpu_body_through_impulses_from_its_read_back_state() {
        // Under water with fresh state: one impulse, up (buoyancy).
        let fresh = gpu_ball_impulses([0.0, 0.15, 0.0], [0.0; 3], 0);
        assert_eq!(fresh.len(), 1);
        assert!(fresh[0][1] > 0.0, "{fresh:?}");
        // Out of the water two ticks ago, but moving into it: met where it
        // is now.
        let late = gpu_ball_impulses([-0.6, 0.15, 0.0], [18.0, 0.0, 0.0], 2);
        assert_eq!(late.len(), 1);
        assert!(late[0][1] > 0.0 && late[0][0] < 0.0, "{late:?}");
        // State older than the limit is not trusted.
        let stale = MAX_COUPLED_STATE_AGE + 1;
        assert!(gpu_ball_impulses([0.0, 0.15, 0.0], [0.0; 3], stale).is_empty());
    }

    type Block = ([f32; 3], [u32; 3], f32, FluidSolver, [f32; 3]);

    /// The particles of each volume after 300 ticks in one 0.6 m
    /// container, for blocks `(min, counts, particle mass, solver,
    /// velocity)`.
    fn mixed(blocks: &[Block]) -> Vec<Vec<[f32; 3]>> {
        let mut app = crate::runtime::App::new();
        let volumes: Vec<_> = blocks
            .iter()
            .map(|&(min, counts, particle_mass, solver, velocity)| {
                let settings = FluidSettings {
                    bounds: ([-0.3, 0.0, -0.3], [0.3, 2.0, 0.3]),
                    particle_mass,
                    solver,
                    ..FluidSettings::default()
                };
                let mut fluid = Fluid::block(min, counts, 0.1);
                fluid.velocities.fill(velocity);
                app.world_mut()
                    .spawn(FluidVolume::new(settings, fluid))
                    .id()
            })
            .collect();
        run_app(&mut app, 300);
        volumes
            .into_iter()
            .map(|entity| {
                app.world()
                    .get::<FluidVolume>(entity)
                    .unwrap()
                    .fluid
                    .positions
                    .clone()
            })
            .collect()
    }

    #[test]
    fn heavy_grains_sink_through_water_light_ones_float_and_a_wave_pushes_a_pile(
    ) {
        use FluidSolver::{Granular, Particles};
        let lowest = |points: &[[f32; 3]]| {
            points.iter().map(|p| p[1]).fold(f32::INFINITY, f32::min)
        };
        let mean_x = |points: &[[f32; 3]]| {
            points.iter().map(|p| p[0]).sum::<f32>() / points.len() as f32
        };
        let pool = ([-0.25, 0.0, -0.25], [6, 3, 6], 1.0, Particles, [0.0; 3]);
        let grains =
            |mass| ([-0.15, 0.5, -0.15], [4, 2, 4], mass, Granular, [0.0; 3]);
        let [water, sand] = &mixed(&[pool, grains(3.0)])[..] else {
            unreachable!()
        };
        assert!(lowest(sand) < 0.02, "sand bottom at {}", lowest(sand));
        for grain in sand {
            for drop in water {
                let gap = (Vector3::from(*grain) - Vector3::from(*drop)).norm();
                assert!(gap > 0.05, "a grain {gap} m from a drop");
            }
        }
        assert!(lowest(&water[..]) >= 0.0);
        let [_, chips] = &mixed(&[pool, grains(0.3)])[..] else {
            unreachable!()
        };
        assert!(lowest(chips) > 0.04, "chips bottom at {}", lowest(chips));
        let pile = ([0.0, 0.0, -0.1], [3, 2, 3], 1.0, Granular, [0.0; 3]);
        let wave = (
            [-0.3, 0.0, -0.25],
            [2, 3, 6],
            1.0,
            Particles,
            [4.0, 0.0, 0.0],
        );
        let still = mean_x(&mixed(&[pile])[0]);
        let pushed = mean_x(&mixed(&[pile, wave])[0]);
        assert!(pushed > still + 0.04, "pile at {pushed}, alone {still}");
    }
}
