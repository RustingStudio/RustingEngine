//! Particle emitter v2: many short-lived sprites per emitter, simulated on
//! the fixed step and drawn in one instanced draw.
//!
//! A [`ParticleEmitter`] is the authored, saved settings. The engine adds a
//! [`ParticleSystem`] next to it that holds the live particles, so no
//! particle is an entity. The renderer reads every system through
//! [`RenderWorld::particles`](super::RenderWorld) and draws each emitter's
//! particles with one instanced call.
//!
//! Every random value comes from [`RandomSeed`] (the scene seed), keyed by
//! the emitter's `SceneId` and the particle's spawn number, and indexed by
//! the fixed tick, so a run repeats exactly (see `AGENTS.md`).
//!
//! `rusting.burst_emitter` still works and is unchanged.

use std::f32::consts::{PI, TAU};
use std::time::Duration;

use bevy_ecs::entity::Entity;
use bevy_ecs::prelude::{
    Commands, Component, Query, Res, Resource, With, Without, World,
};
use bevy_ecs::query::QueryItem;
use serde::{Deserialize, Serialize};

use super::sim_math;
use super::{FrameTime, GlobalTransform, RandomSeed, SceneId};

/// Where new particles start, around the emitter. `shape_size` gives the
/// extent: box half extents, or `x` as the radius of the round shapes.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize,
)]
pub enum EmitterShape {
    /// All at the emitter's position.
    #[default]
    Point,
    /// Anywhere inside a box with half extents `shape_size`.
    Box,
    /// Anywhere inside a sphere of radius `shape_size[0]`.
    Sphere,
    /// On a disc of radius `shape_size[0]` across the emitter's XZ plane;
    /// directions lean outward with distance from the center, by `spread`.
    Cone,
    /// On a ring of radius `shape_size[0]` in the emitter's XZ plane.
    Circle,
}

/// Which space live particles move in.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize,
)]
pub enum ParticleSpace {
    /// Particles stay where they were emitted when the emitter moves.
    #[default]
    World,
    /// Particles move with the emitter.
    Local,
}

/// How each particle faces the camera.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize,
)]
pub enum ParticleFacing {
    /// A square that always faces the camera.
    #[default]
    Billboard,
    /// Stretched along its velocity by `stretch`, for rain and sparks.
    Velocity,
}

/// Where an emitter's particles are simulated.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize,
)]
pub enum ParticleSimulation {
    /// On the CPU each fixed step: every setting works.
    #[default]
    Cpu,
    /// In a compute shader each frame, for up to `MAX_GPU_PARTICLES`.
    /// Turbulence, start colors, rotation, spin, `Local` space, `Velocity`
    /// facing and collision restarts are ignored, and alpha blended
    /// particles are not sorted. GPU force fields push the particles, and
    /// `collide` bounces them off physics colliders.
    Gpu,
}

/// Most particles one GPU emitter keeps alive.
pub const MAX_GPU_PARTICLES: u32 = 1 << 20;

/// How a particle's color mixes with what is behind it.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize,
)]
pub enum ParticleBlend {
    /// Covers what is behind by its alpha: smoke, leaves, confetti.
    #[default]
    Alpha,
    /// Adds light: fire, sparks, magic.
    Additive,
}

/// The sprite drawn for each particle.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize,
)]
pub enum ParticleSprite {
    /// Round, soft-edged glow.
    #[default]
    Soft,
    /// Round with a crisp edge.
    Disc,
    /// Full square: confetti, leaves.
    Square,
}

/// A burst of `count` particles at `time` seconds into each cycle.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ParticleBurst {
    pub time: f32,
    pub count: u32,
}

impl Default for ParticleBurst {
    fn default() -> Self {
        Self {
            time: 0.0,
            count: 10,
        }
    }
}

/// A value at a point of a particle's life, 0 at birth and 1 at death.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CurveKey {
    pub t: f32,
    pub value: f32,
}

impl Default for CurveKey {
    fn default() -> Self {
        Self { t: 0.0, value: 1.0 }
    }
}

/// A linear RGBA color at a point of a particle's life.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ColorKey {
    pub t: f32,
    pub color: [f32; 4],
}

impl Default for ColorKey {
    fn default() -> Self {
        Self {
            t: 0.0,
            color: [1.0; 4],
        }
    }
}

/// Live state change asked for by game code or the editor; the next fixed
/// step applies it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ParticleCommand {
    #[default]
    None,
    Play,
    Pause,
    Stop,
    Restart,
}

/// Emits and simulates particles. See the module docs. Ranges are
/// `[min, max]`; each particle draws its own value between them.
#[derive(Component, Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ParticleEmitter {
    /// Start emitting when the scene starts. Off waits for `play()`.
    pub autoplay: bool,
    /// Particles per second while playing.
    pub rate: f32,
    /// Bursts in each cycle.
    pub bursts: Vec<ParticleBurst>,
    /// Particles alive at once; extra spawns are skipped.
    pub max_particles: u32,
    /// Start as if one full cycle had already played.
    pub prewarm: bool,
    /// Seconds in one cycle.
    pub duration: f32,
    /// Start a new cycle after `duration`; off stops emitting.
    pub looping: bool,
    /// Restart when this body's collider starts touching another one.
    pub on_collision: bool,
    pub shape: EmitterShape,
    pub shape_size: [f32; 3],
    /// Seconds each particle lives.
    pub lifetime: [f32; 2],
    /// Start speed in metres per second.
    pub speed: [f32; 2],
    /// Start size in metres.
    pub size: [f32; 2],
    /// Start rotation in radians.
    pub rotation: [f32; 2],
    /// Spin in radians per second.
    pub spin: [f32; 2],
    /// Launch direction in the emitter's space.
    pub direction: [f32; 3],
    /// Half angle in radians around `direction`; PI sends particles every
    /// way.
    pub spread: f32,
    /// Downward acceleration in metres per second squared.
    pub gravity: f32,
    /// Fraction of velocity lost per second.
    pub drag: f32,
    /// Constant world acceleration, metres per second squared.
    pub wind: [f32; 3],
    /// Strength of a swirling push, metres per second squared.
    pub turbulence: f32,
    /// Size of the swirls: higher makes smaller, faster eddies.
    pub turbulence_frequency: f32,
    /// Size multiplier over life. Empty keeps the start size.
    pub size_over_life: Vec<CurveKey>,
    /// Linear RGBA over life. Empty is white.
    pub color_over_life: Vec<ColorKey>,
    /// Each particle picks one of these at spawn and multiplies its color
    /// over life by it, for confetti or mixed leaves. Empty is white.
    pub start_colors: Vec<[f32; 4]>,
    /// Brightness multiplier; above 1 glows with bloom.
    pub emissive: f32,
    /// Fraction of life spent fading in from transparent.
    pub fade_in: f32,
    /// Fraction of life spent fading out at the end.
    pub fade_out: f32,
    pub space: ParticleSpace,
    pub facing: ParticleFacing,
    /// Length factor along the velocity for `Velocity` facing, in seconds
    /// of travel.
    pub stretch: f32,
    pub blend: ParticleBlend,
    pub sprite: ParticleSprite,
    pub simulation: ParticleSimulation,
    /// `Gpu` simulation: particles bounce off physics colliders by the
    /// collider's restitution.
    pub collide: bool,
    /// `Gpu` simulation: seconds of path each particle trails behind it,
    /// 0 for none.
    pub trail: f32,
    /// `Gpu` simulation under a parent `Gpu` emitter: a sub-emitter that
    /// spawns this many particles where each parent particle dies, instead
    /// of by rate and bursts. 0 for a normal emitter.
    pub on_parent_death: u32,
    /// Set by `play`, `pause`, `stop`, and `restart`.
    #[serde(skip)]
    pub command: ParticleCommand,
}

impl Default for ParticleEmitter {
    fn default() -> Self {
        Self {
            autoplay: true,
            rate: 20.0,
            bursts: Vec::new(),
            max_particles: 1000,
            prewarm: false,
            duration: 5.0,
            looping: true,
            on_collision: false,
            shape: EmitterShape::Point,
            shape_size: [0.5; 3],
            lifetime: [1.0, 2.0],
            speed: [1.0, 2.0],
            size: [0.1, 0.2],
            rotation: [0.0, 0.0],
            spin: [0.0, 0.0],
            direction: [0.0, 1.0, 0.0],
            spread: 0.4,
            gravity: 0.0,
            drag: 0.0,
            wind: [0.0; 3],
            turbulence: 0.0,
            turbulence_frequency: 1.0,
            size_over_life: Vec::new(),
            color_over_life: Vec::new(),
            start_colors: Vec::new(),
            emissive: 1.0,
            fade_in: 0.1,
            fade_out: 0.3,
            space: ParticleSpace::World,
            facing: ParticleFacing::Billboard,
            stretch: 0.1,
            blend: ParticleBlend::Alpha,
            sprite: ParticleSprite::Soft,
            simulation: ParticleSimulation::Cpu,
            collide: false,
            trail: 0.0,
            on_parent_death: 0,
            command: ParticleCommand::None,
        }
    }
}

impl ParticleEmitter {
    pub fn play(&mut self) {
        self.command = ParticleCommand::Play;
    }

    /// Freezes particles where they are.
    pub fn pause(&mut self) {
        self.command = ParticleCommand::Pause;
    }

    /// Stops emitting; live particles finish their lives.
    pub fn stop(&mut self) {
        self.command = ParticleCommand::Stop;
    }

    /// Clears live particles and starts a new cycle.
    pub fn restart(&mut self) {
        self.command = ParticleCommand::Restart;
    }

    /// Size multiplier at `life` (0..1).
    #[must_use]
    pub fn size_at(&self, life: f32) -> f32 {
        sample_keys(&self.size_over_life, life, |key| (key.t, key.value), 1.0)
    }

    /// Linear RGBA at `life` (0..1), with fades applied to alpha.
    #[must_use]
    pub fn color_at(&self, life: f32) -> [f32; 4] {
        let mut color = [0, 1, 2, 3].map(|channel| {
            sample_keys(
                &self.color_over_life,
                life,
                |key| (key.t, key.color[channel]),
                1.0,
            )
        });
        let fade_in = if self.fade_in > 0.0 {
            (life / self.fade_in).min(1.0)
        } else {
            1.0
        };
        let fade_out = if self.fade_out > 0.0 {
            ((1.0 - life) / self.fade_out).min(1.0)
        } else {
            1.0
        };
        color[3] *= fade_in.max(0.0) * fade_out.max(0.0);
        color
    }
}

/// Piecewise-linear value of keys sorted by `t`; `empty` with no keys.
fn sample_keys<K>(
    keys: &[K],
    t: f32,
    get: impl Fn(&K) -> (f32, f32),
    empty: f32,
) -> f32 {
    let Some(first) = keys.first() else {
        return empty;
    };
    let mut previous = get(first);
    if t <= previous.0 {
        return previous.1;
    }
    for key in &keys[1..] {
        let next = get(key);
        if t <= next.0 {
            let span = next.0 - previous.0;
            if span <= 0.0 {
                return next.1;
            }
            let f = (t - previous.0) / span;
            return previous.1 + (next.1 - previous.1) * f;
        }
        previous = next;
    }
    previous.1
}

/// One live particle. Position and velocity are in world space, or in the
/// emitter's space for [`ParticleSpace::Local`].
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Particle {
    pub position: [f32; 3],
    pub velocity: [f32; 3],
    pub age: f32,
    pub lifetime: f32,
    pub size: f32,
    pub rotation: f32,
    pub spin: f32,
    /// One of `start_colors`, or white.
    pub tint: [f32; 4],
}

/// Live particles of a [`ParticleEmitter`]. Added by the engine; never
/// saved.
#[derive(Component, Clone, Debug, Default, PartialEq)]
pub struct ParticleSystem {
    pub particles: Vec<Particle>,
    /// Seconds since the current cycle sequence started.
    pub time: f32,
    pub playing: bool,
    pub paused: bool,
    /// Fraction of a particle `rate` has not emitted yet.
    pub pending: f32,
    /// Particles spawned so far; numbers each spawn's random values.
    pub spawned: u64,
    /// False until the first fixed step has seen the emitter.
    pub started: bool,
}

impl ParticleSystem {
    #[must_use]
    pub fn alive(&self) -> usize {
        self.particles.len()
    }
}

/// Emitter frame: world matrix columns 0..3 and its position.
#[derive(Clone, Copy)]
struct Frame {
    matrix: [[f32; 4]; 4],
}

impl Frame {
    fn point(&self, p: [f32; 3]) -> [f32; 3] {
        let m = &self.matrix;
        std::array::from_fn(|r| {
            m[0][r] * p[0] + m[1][r] * p[1] + m[2][r] * p[2] + m[3][r]
        })
    }

    /// Rotates `v` by the frame without its scale.
    fn direction(&self, v: [f32; 3]) -> [f32; 3] {
        let m = &self.matrix;
        let axis = |c: usize| sim_math::normalize([m[c][0], m[c][1], m[c][2]]);
        let [x, y, z] = [axis(0), axis(1), axis(2)];
        std::array::from_fn(|r| x[r] * v[0] + y[r] * v[1] + z[r] * v[2])
    }
}

/// Random draws for one spawn: `index` picks one of 16 values.
struct Draw<'a> {
    seed: &'a RandomSeed,
    tick: u64,
    stream: u64,
    spawn: u64,
}

impl Draw<'_> {
    fn unit(&self, index: u64) -> f32 {
        self.seed
            .unit(self.tick, self.stream ^ (self.spawn << 4 | index))
    }

    fn range(&self, index: u64, [min, max]: [f32; 2]) -> f32 {
        min + (max - min) * self.unit(index)
    }
}

/// Unit vector within `spread` radians of the normalized `axis`.
fn cone_direction(axis: [f32; 3], spread: f32, u: f32, v: f32) -> [f32; 3] {
    let spread = spread.clamp(0.0, PI);
    let cos_max = sim_math::sin_cos(spread).1;
    let cos = 1.0 - u * (1.0 - cos_max);
    let sin = (1.0 - cos * cos).max(0.0).sqrt();
    let (sp, cp) = sim_math::sin_cos(v * TAU);
    // Any two axes perpendicular to `axis`.
    let helper = if axis[1].abs() < 0.99 {
        [0.0, 1.0, 0.0]
    } else {
        [1.0, 0.0, 0.0]
    };
    let side = sim_math::normalize(cross(helper, axis));
    let up = cross(axis, side);
    std::array::from_fn(|i| axis[i] * cos + (side[i] * cp + up[i] * sp) * sin)
}

fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

/// A new particle in emitter space (`Local`) or world space (`World`).
/// The item `unit` (0 to 1) falls on, or `None` when `items` is empty.
fn pick<T: Copy>(items: &[T], unit: f32) -> Option<T> {
    let index = (unit * items.len() as f32) as usize;
    items.get(index.min(items.len().saturating_sub(1))).copied()
}

fn spawn(emitter: &ParticleEmitter, frame: Frame, draw: &Draw) -> Particle {
    let axis = {
        let length = sim_math::length(emitter.direction);
        if length > 0.0 {
            emitter.direction.map(|v| v / length)
        } else {
            [0.0, 1.0, 0.0]
        }
    };
    let size = emitter.shape_size;
    let radius = size[0].max(0.0);
    let (offset, direction) = match emitter.shape {
        EmitterShape::Point => ([0.0; 3], None),
        EmitterShape::Box => (
            [0, 1, 2].map(|i| (draw.unit(i) * 2.0 - 1.0) * size[i as usize]),
            None,
        ),
        EmitterShape::Sphere => {
            let dir = cone_direction(axis, PI, draw.unit(0), draw.unit(1));
            let r = radius * draw.unit(2).cbrt();
            (dir.map(|v| v * r), None)
        }
        EmitterShape::Cone | EmitterShape::Circle => {
            let (s, c) = sim_math::sin_cos(draw.unit(0) * TAU);
            let r = if emitter.shape == EmitterShape::Circle {
                radius
            } else {
                radius * draw.unit(1).sqrt()
            };
            let offset = [c * r, 0.0, s * r];
            let direction = (emitter.shape == EmitterShape::Cone).then(|| {
                // Lean outward by the spread at the rim.
                let lean = if radius > 0.0 {
                    emitter.spread.clamp(0.0, PI * 0.5) * r / radius
                } else {
                    0.0
                };
                let (ls, lc) = sim_math::sin_cos(lean);
                sim_math::normalize([c * ls, lc, s * ls])
            });
            (offset, direction)
        }
    };
    let local_direction = direction.unwrap_or_else(|| {
        cone_direction(axis, emitter.spread, draw.unit(3), draw.unit(4))
    });
    let speed = draw.range(5, emitter.speed);
    let lifetime = draw.range(6, emitter.lifetime).max(1e-3);
    let mut particle = Particle {
        position: offset,
        velocity: local_direction.map(|v| v * speed),
        age: 0.0,
        lifetime,
        size: draw.range(7, emitter.size),
        rotation: draw.range(8, emitter.rotation),
        spin: draw.range(9, emitter.spin),
        tint: pick(&emitter.start_colors, draw.unit(10)).unwrap_or([1.0; 4]),
    };
    if emitter.space == ParticleSpace::World {
        particle.position = frame.point(offset);
        particle.velocity = frame.direction(particle.velocity);
    }
    particle
}

/// Moves `particle` by one step of `dt` seconds at simulation time `time`.
fn integrate(
    emitter: &ParticleEmitter,
    particle: &mut Particle,
    dt: f32,
    time: f32,
) {
    let mut accel = emitter.wind;
    accel[1] -= emitter.gravity;
    if emitter.turbulence != 0.0 {
        let f = emitter.turbulence_frequency;
        let p = particle.position;
        let wave = |a: f32, b: f32, phase: f32| {
            sim_math::sin_cos(a * f + time * 1.3 + phase).0
                + sim_math::sin_cos(b * f * 1.7 - time * 0.9 + phase).1
        };
        accel[0] += emitter.turbulence * 0.5 * wave(p[1], p[2], 0.0);
        accel[1] += emitter.turbulence * 0.5 * wave(p[2], p[0], 2.1);
        accel[2] += emitter.turbulence * 0.5 * wave(p[0], p[1], 4.2);
    }
    let damp = (1.0 - emitter.drag * dt).max(0.0);
    for ((velocity, position), accel) in particle
        .velocity
        .iter_mut()
        .zip(&mut particle.position)
        .zip(accel)
    {
        *velocity = (*velocity + accel * dt) * damp;
        *position += *velocity * dt;
    }
    particle.rotation += particle.spin * dt;
    particle.age += dt;
}

/// Advances one emitter by one fixed step `tick` of `dt` seconds.
fn step(
    emitter: &ParticleEmitter,
    system: &mut ParticleSystem,
    frame: Frame,
    seed: &RandomSeed,
    tick: u64,
    stream: u64,
    dt: f32,
) {
    if system.paused {
        return;
    }
    let time = system.time;
    system.particles.retain_mut(|particle| {
        integrate(emitter, particle, dt, time);
        particle.age < particle.lifetime
    });
    if !system.playing {
        return;
    }
    let duration = emitter.duration.max(1e-3);
    let before = system.time;
    let after = before + dt;
    system.time = after;
    let mut count = 0_u64;
    // Rate only within the cycle (or forever when looping).
    let emitting = emitter.looping || before < duration;
    if emitting && emitter.rate > 0.0 {
        let span = if emitter.looping {
            dt
        } else {
            after.min(duration) - before
        };
        system.pending += emitter.rate * span;
        let whole = system.pending.floor();
        system.pending -= whole;
        count += whole as u64;
    }
    for burst in &emitter.bursts {
        // Fires when the cycle time crosses `burst.time` in this step.
        let fires = if emitter.looping {
            let cycle = (before / duration).floor();
            let mut at = cycle * duration + burst.time;
            if at < before {
                at += duration;
            }
            at >= before && at < after
        } else {
            burst.time >= before && burst.time < after
        };
        if fires {
            count += u64::from(burst.count);
        }
    }
    if !emitter.looping && after >= duration && system.particles.is_empty() {
        system.playing = false;
    }
    if emitter.simulation == ParticleSimulation::Gpu {
        // The renderer spawns them; newer spawns overwrite the oldest.
        system.spawned += count;
        return;
    }
    let room =
        u64::from(emitter.max_particles).saturating_sub(system.alive() as u64);
    for _ in 0..count.min(room) {
        let draw = Draw {
            seed,
            tick,
            stream,
            spawn: system.spawned,
        };
        system.spawned += 1;
        system.particles.push(spawn(emitter, frame, &draw));
    }
}

/// Per fixed step: adds a [`ParticleSystem`] to every new emitter.
pub(super) fn add_particle_systems(
    mut commands: Commands,
    emitters: Query<Entity, (With<ParticleEmitter>, Without<ParticleSystem>)>,
) {
    for entity in &emitters {
        commands.entity(entity).insert(ParticleSystem::default());
    }
}

type EmitterParts = (
    Entity,
    &'static mut ParticleEmitter,
    &'static mut ParticleSystem,
    Option<&'static GlobalTransform>,
    Option<&'static crate::Transform>,
    Option<&'static SceneId>,
);

/// Stream key of an emitter: its `SceneId`, else its entity.
fn stream_key(entity: Entity, id: Option<&SceneId>) -> u64 {
    let key = id.map_or(entity.to_bits(), |id| {
        let bits = id.0.as_u128();
        (bits as u64) ^ ((bits >> 64) as u64)
    });
    RandomSeed::stream("particles", key)
}

fn emitter_frame(
    global: Option<&GlobalTransform>,
    transform: Option<&crate::Transform>,
) -> Frame {
    Frame {
        matrix: global.map(|g| g.matrix).unwrap_or_else(|| {
            let mut matrix = GlobalTransform::default().matrix;
            if let Some(t) = transform {
                matrix[3] = [t.position[0], t.position[1], t.position[2], 1.0];
            }
            matrix
        }),
    }
}

/// Per fixed step: applies commands and advances every particle system.
pub(super) fn update_particles(
    time: Res<FrameTime>,
    seed: Res<RandomSeed>,
    mut emitters: Query<EmitterParts>,
) {
    let dt = time.fixed_delta.as_secs_f32();
    for parts in &mut emitters {
        advance(parts, &seed, time.fixed_tick, dt);
    }
}

/// Applies an emitter's command and advances it one step `tick`.
fn advance(
    (entity, mut emitter, mut system, global, transform, id): QueryItem<
        '_,
        '_,
        EmitterParts,
    >,
    seed: &RandomSeed,
    tick: u64,
    dt: f32,
) {
    let stream = stream_key(entity, id);
    let frame = emitter_frame(global, transform);
    let command = std::mem::take(&mut emitter.command);
    apply_command(&mut system, command);
    if !system.started {
        system.started = true;
        system.playing |= emitter.autoplay;
        if emitter.prewarm && system.playing {
            prewarm(&emitter, &mut system, frame, seed, tick, stream, dt);
        }
    }
    step(&emitter, &mut system, frame, seed, tick, stream, dt);
}

/// Clock of [`preview_particles`]: time not yet stepped, and its own tick.
#[derive(Resource, Default)]
pub struct ParticlePreview {
    pending: Duration,
    tick: u64,
}

/// Steps every emitter by `real` time in fixed steps, outside the game's
/// schedule: the editor's live preview while the scene is stopped.
/// Particles are visual only, so the preview's own tick never reaches game
/// state. At most four steps run per call; a long frame drops the rest.
pub fn preview_particles(world: &mut World, real: Duration) {
    let missing = world
        .query_filtered::<Entity, (With<ParticleEmitter>, Without<ParticleSystem>)>()
        .iter(world)
        .collect::<Vec<_>>();
    for entity in missing {
        world.entity_mut(entity).insert(ParticleSystem::default());
    }
    let fixed = world
        .get_resource::<FrameTime>()
        .map_or(FrameTime::default().fixed_delta, |time| time.fixed_delta);
    let seed = world
        .get_resource::<RandomSeed>()
        .copied()
        .unwrap_or_default();
    let mut preview =
        world.get_resource_or_insert_with(ParticlePreview::default);
    preview.pending += real;
    let mut steps = 0_u64;
    while preview.pending >= fixed && !fixed.is_zero() {
        preview.pending -= fixed;
        steps += 1;
    }
    let steps = steps.min(4);
    let first = preview.tick;
    preview.tick += steps;
    let mut emitters = world.query::<EmitterParts>();
    for tick in first..first + steps {
        for parts in emitters.iter_mut(world) {
            advance(parts, &seed, tick, fixed.as_secs_f32());
        }
    }
}

/// Drops every live particle system, so the next step starts each emitter
/// fresh: autoplay and prewarm run again. The editor calls it when Play
/// begins, so preview particles do not carry into the game.
pub fn reset_particles(world: &mut World) {
    let live = world
        .query_filtered::<Entity, With<ParticleSystem>>()
        .iter(world)
        .collect::<Vec<_>>();
    for entity in live {
        world.entity_mut(entity).remove::<ParticleSystem>();
    }
}

fn apply_command(system: &mut ParticleSystem, command: ParticleCommand) {
    match command {
        ParticleCommand::None => {}
        ParticleCommand::Play => {
            if !system.playing {
                system.time = 0.0;
            }
            system.playing = true;
            system.paused = false;
            system.started = true;
        }
        ParticleCommand::Pause => system.paused = true,
        ParticleCommand::Stop => {
            system.playing = false;
            system.paused = false;
            system.started = true;
        }
        ParticleCommand::Restart => {
            system.particles.clear();
            system.time = 0.0;
            system.pending = 0.0;
            system.playing = true;
            system.paused = false;
            system.started = true;
        }
    }
}

/// Plays one cycle (at most ten seconds) before the first visible step.
/// Ticks before the scene's first count backward from `tick`, so prewarm
/// draws its own random values.
fn prewarm(
    emitter: &ParticleEmitter,
    system: &mut ParticleSystem,
    frame: Frame,
    seed: &RandomSeed,
    tick: u64,
    stream: u64,
    dt: f32,
) {
    if dt <= 0.0 {
        return;
    }
    let seconds = emitter.duration.clamp(0.0, 10.0);
    let steps = (seconds / dt) as u64;
    let warm = RandomSeed::stream("particles_prewarm", stream);
    for index in 0..steps {
        let virtual_tick = tick.wrapping_sub(steps - index);
        step(emitter, system, frame, seed, virtual_tick, warm, dt);
    }
}

/// Per fixed step, after physics: restarts emitters with `on_collision`
/// whose body started touching something. Uses the body's contacts.
pub(super) fn restart_on_contact(
    physics: Res<super::PhysicsWorld>,
    mut emitters: Query<(Entity, &mut ParticleEmitter)>,
    mut touching: bevy_ecs::prelude::Local<std::collections::BTreeSet<Entity>>,
) {
    let now: std::collections::BTreeSet<Entity> = physics
        .contacts()
        .iter()
        .flat_map(|contact| [contact.a, contact.b])
        .collect();
    for (entity, mut emitter) in &mut emitters {
        if emitter.on_collision
            && now.contains(&entity)
            && !touching.contains(&entity)
        {
            emitter.restart();
        }
    }
    *touching = now;
}

/// One particle as the renderer draws it.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ParticleInstance {
    /// World-space center.
    pub position: [f32; 3],
    /// Width in metres.
    pub size: f32,
    /// Linear RGB already scaled by emissive, and alpha.
    pub color: [f32; 4],
    /// World-space vector the quad stretches along; zero for billboards.
    pub stretch: [f32; 3],
    /// Rotation in radians around the view axis.
    pub rotation: f32,
}

/// The particles of one emitter, drawn by one instanced draw.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ParticleBatch {
    pub entity: Option<Entity>,
    pub blend: ParticleBlend,
    pub sprite: ParticleSprite,
    pub instances: Vec<ParticleInstance>,
}

/// What the renderer needs to simulate one GPU emitter.
#[derive(Clone, Debug, PartialEq)]
pub struct GpuParticleBatch {
    pub entity: Entity,
    /// Random stream of the emitter's spawns.
    pub stream: u64,
    /// Spawns so far; the renderer spawns the ones it has not seen.
    pub spawned: u64,
    /// Ring of particle slots, 1..=`MAX_GPU_PARTICLES`.
    pub max_particles: u32,
    pub blend: ParticleBlend,
    pub sprite: ParticleSprite,
    /// Shape as 0 point, 1 box, 2 sphere, 3 cone, 4 circle.
    pub shape: u32,
    /// Trail points each particle keeps: 0, or `GPU_TRAIL_POINTS`.
    pub trail_points: u32,
    /// Sub-emitter's parent emitter and spawns per parent death, set by
    /// render extraction when the parent is a `Gpu` emitter.
    pub parent: Option<(Entity, u32)>,
    /// Settings the compute shader reads: emitter matrix columns, then
    /// direction and spread, wind and gravity, lifetime and speed ranges,
    /// size range, drag and emissive, shape size and collide, trail seconds
    /// and points, then `GPU_PARTICLE_CURVE` samples of color and of size
    /// over life.
    pub rows: Vec<[f32; 4]>,
}

/// Samples of color and size over life a GPU emitter takes.
// ponytail: curves are sampled at 8 points and lerped between.
pub const GPU_PARTICLE_CURVE: usize = 8;

/// Past positions a trailing GPU particle keeps, evenly spread over
/// `ParticleEmitter::trail`.
// ponytail: fixed; long trails get coarse corners, raise if they show.
pub const GPU_TRAIL_POINTS: u32 = 8;
/// Builds the settings of one GPU emitter.
#[must_use]
pub fn gpu_particle_batch(
    entity: Entity,
    emitter: &ParticleEmitter,
    system: &ParticleSystem,
    global: Option<&GlobalTransform>,
    id: Option<&SceneId>,
) -> GpuParticleBatch {
    let matrix = emitter_frame(global, None).matrix;
    let direction = sim_math::normalize(emitter.direction);
    let mut rows = matrix.to_vec();
    rows.extend([
        [direction[0], direction[1], direction[2], emitter.spread],
        [
            emitter.wind[0],
            emitter.wind[1],
            emitter.wind[2],
            emitter.gravity,
        ],
        [
            emitter.lifetime[0],
            emitter.lifetime[1],
            emitter.speed[0],
            emitter.speed[1],
        ],
        [
            emitter.size[0],
            emitter.size[1],
            emitter.drag,
            emitter.emissive,
        ],
        [
            emitter.shape_size[0],
            emitter.shape_size[1],
            emitter.shape_size[2],
            f32::from(u8::from(emitter.collide)),
        ],
    ]);
    let trail_points = if emitter.trail > 0.0 {
        GPU_TRAIL_POINTS
    } else {
        0
    };
    rows.push([emitter.trail.max(0.0), trail_points as f32, 0.0, 0.0]);
    let life = |sample: usize| sample as f32 / (GPU_PARTICLE_CURVE - 1) as f32;
    rows.extend((0..GPU_PARTICLE_CURVE).map(|i| emitter.color_at(life(i))));
    rows.extend(
        (0..GPU_PARTICLE_CURVE)
            .map(|i| [emitter.size_at(life(i)), 0.0, 0.0, 0.0]),
    );
    GpuParticleBatch {
        entity,
        stream: stream_key(entity, id),
        spawned: system.spawned,
        max_particles: emitter.max_particles.clamp(1, MAX_GPU_PARTICLES),
        blend: emitter.blend,
        sprite: emitter.sprite,
        shape: emitter.shape as u32,
        trail_points,
        parent: None,
        rows,
    }
}

/// Builds the draw list of one emitter from its live particles.
#[must_use]
pub fn particle_batch(
    emitter: &ParticleEmitter,
    system: &ParticleSystem,
    global: Option<&GlobalTransform>,
) -> ParticleBatch {
    let frame = emitter_frame(global, None);
    let local = emitter.space == ParticleSpace::Local;
    let instances = system
        .particles
        .iter()
        .map(|particle| {
            let life = (particle.age / particle.lifetime).clamp(0.0, 1.0);
            let mut color = emitter.color_at(life);
            for (channel, tint) in color.iter_mut().zip(particle.tint) {
                *channel *= tint;
            }
            for channel in &mut color[..3] {
                *channel *= emitter.emissive;
            }
            let (position, velocity) = if local {
                (
                    frame.point(particle.position),
                    frame.direction(particle.velocity),
                )
            } else {
                (particle.position, particle.velocity)
            };
            let stretch = match emitter.facing {
                ParticleFacing::Billboard => [0.0; 3],
                ParticleFacing::Velocity => {
                    velocity.map(|v| v * emitter.stretch)
                }
            };
            ParticleInstance {
                position,
                size: particle.size * emitter.size_at(life),
                color,
                stretch,
                rotation: particle.rotation,
            }
        })
        .filter(|instance| instance.color[3] > 0.0 && instance.size > 0.0)
        .collect();
    ParticleBatch {
        entity: None,
        blend: emitter.blend,
        sprite: emitter.sprite,
        instances,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(emitter: &ParticleEmitter, ticks: u64, seed: u64) -> ParticleSystem {
        let mut system = ParticleSystem {
            playing: emitter.autoplay,
            started: true,
            ..ParticleSystem::default()
        };
        let frame = emitter_frame(None, None);
        let seed = RandomSeed(seed);
        for tick in 0..ticks {
            step(emitter, &mut system, frame, &seed, tick, 7, 1.0 / 60.0);
        }
        system
    }

    #[test]
    fn rate_emits_and_lifetime_removes() {
        let emitter = ParticleEmitter {
            rate: 60.0,
            lifetime: [0.5, 0.5],
            ..ParticleEmitter::default()
        };
        let system = run(&emitter, 60, 1);
        // Half a second of particles at 60 per second, give or take one.
        assert!((29..=31).contains(&system.alive()), "{}", system.alive());
        assert_eq!(system.spawned, 60);
    }

    #[test]
    fn gpu_emitters_only_count_spawns_and_pack_their_settings() {
        let emitter = ParticleEmitter {
            rate: 60.0,
            simulation: ParticleSimulation::Gpu,
            collide: true,
            trail: 0.5,
            max_particles: u32::MAX,
            shape: EmitterShape::Sphere,
            ..ParticleEmitter::default()
        };
        let system = run(&emitter, 60, 1);
        assert_eq!(system.alive(), 0, "the renderer owns the particles");
        assert_eq!(system.spawned, 60);
        let global = GlobalTransform {
            matrix: [
                [1.0, 0.0, 0.0, 0.0],
                [0.0, 1.0, 0.0, 0.0],
                [0.0, 0.0, 1.0, 0.0],
                [3.0, 4.0, 5.0, 1.0],
            ],
        };
        let entity = Entity::from_raw_u32(3).unwrap();
        let batch =
            gpu_particle_batch(entity, &emitter, &system, Some(&global), None);
        assert_eq!(batch.spawned, 60);
        assert_eq!(batch.max_particles, MAX_GPU_PARTICLES);
        assert_eq!(batch.shape, 2);
        assert_eq!(batch.rows.len(), 10 + 2 * GPU_PARTICLE_CURVE);
        assert_eq!(batch.rows[9], [0.5, 8.0, 0.0, 0.0], "trail");
        assert_eq!(batch.trail_points, GPU_TRAIL_POINTS);
        assert_eq!(batch.rows[3], [3.0, 4.0, 5.0, 1.0]);
        assert_eq!(batch.rows[6], [1.0, 2.0, 1.0, 2.0], "lifetime, speed");
        assert_eq!(batch.rows[8][3], 1.0, "collide");
        // Faded in from transparent, white in the middle of life.
        assert_eq!(batch.rows[10][3], 0.0);
        assert_eq!(batch.rows[10 + 3], [1.0; 4]);
        assert_eq!(batch.rows[10 + GPU_PARTICLE_CURVE], [1.0, 0.0, 0.0, 0.0]);
    }

    #[test]
    fn same_seed_same_particles_other_seed_differs() {
        let emitter = ParticleEmitter {
            spread: PI,
            turbulence: 2.0,
            ..ParticleEmitter::default()
        };
        assert_eq!(run(&emitter, 90, 5), run(&emitter, 90, 5));
        assert_ne!(
            run(&emitter, 90, 5).particles,
            run(&emitter, 90, 6).particles
        );
    }

    #[test]
    fn bursts_respect_max_and_one_shot_stops() {
        let emitter = ParticleEmitter {
            rate: 0.0,
            bursts: vec![
                ParticleBurst {
                    time: 0.0,
                    count: 30,
                },
                ParticleBurst {
                    time: 0.5,
                    count: 30,
                },
            ],
            max_particles: 40,
            looping: false,
            duration: 1.0,
            lifetime: [0.8, 0.8],
            ..ParticleEmitter::default()
        };
        let mut system = run(&emitter, 2, 1);
        assert_eq!(system.alive(), 30);
        let frame = emitter_frame(None, None);
        for tick in 2..40 {
            step(
                &emitter,
                &mut system,
                frame,
                &RandomSeed(1),
                tick,
                7,
                1.0 / 60.0,
            );
        }
        assert_eq!(system.alive(), 40, "second burst capped");
        for tick in 40..200 {
            step(
                &emitter,
                &mut system,
                frame,
                &RandomSeed(1),
                tick,
                7,
                1.0 / 60.0,
            );
        }
        assert_eq!(system.alive(), 0);
        assert!(!system.playing);
    }

    #[test]
    fn looping_bursts_repeat_each_cycle() {
        let emitter = ParticleEmitter {
            rate: 0.0,
            bursts: vec![ParticleBurst {
                time: 0.25,
                count: 5,
            }],
            duration: 0.5,
            lifetime: [10.0, 10.0],
            ..ParticleEmitter::default()
        };
        // Two seconds is four cycles.
        assert_eq!(run(&emitter, 120, 1).spawned, 20);
    }

    #[test]
    fn spawn_shapes_stay_inside_their_size() {
        for shape in [
            EmitterShape::Point,
            EmitterShape::Box,
            EmitterShape::Sphere,
            EmitterShape::Cone,
            EmitterShape::Circle,
        ] {
            let emitter = ParticleEmitter {
                shape,
                shape_size: [1.0, 2.0, 3.0],
                speed: [0.0, 0.0],
                rate: 600.0,
                ..ParticleEmitter::default()
            };
            let system = run(&emitter, 2, 3);
            assert!(!system.particles.is_empty());
            for particle in &system.particles {
                let [x, y, z] = particle.position;
                let inside = match shape {
                    EmitterShape::Point => x == 0.0 && y == 0.0 && z == 0.0,
                    EmitterShape::Box => {
                        x.abs() <= 1.0 && y.abs() <= 2.0 && z.abs() <= 3.0
                    }
                    EmitterShape::Sphere => (x * x + y * y + z * z) <= 1.0001,
                    EmitterShape::Cone => y == 0.0 && x * x + z * z <= 1.0001,
                    EmitterShape::Circle => {
                        y == 0.0 && (x * x + z * z - 1.0).abs() < 1e-3
                    }
                };
                assert!(inside, "{shape:?} {:?}", particle.position);
            }
        }
    }

    #[test]
    fn spread_zero_launches_along_direction_and_gravity_pulls() {
        let emitter = ParticleEmitter {
            direction: [1.0, 0.0, 0.0],
            spread: 0.0,
            speed: [2.0, 2.0],
            gravity: 10.0,
            rate: 60.0,
            ..ParticleEmitter::default()
        };
        let system = run(&emitter, 30, 1);
        let oldest = system.particles[0];
        assert!((oldest.velocity[0] - 2.0).abs() < 1e-4);
        assert!(oldest.velocity[1] < -4.0);
        assert!(oldest.velocity[2].abs() < 1e-4);
    }

    #[test]
    fn curves_and_fades() {
        let emitter = ParticleEmitter {
            size_over_life: vec![
                CurveKey { t: 0.0, value: 0.0 },
                CurveKey { t: 1.0, value: 2.0 },
            ],
            color_over_life: vec![
                ColorKey {
                    t: 0.0,
                    color: [1.0, 0.0, 0.0, 1.0],
                },
                ColorKey {
                    t: 1.0,
                    color: [0.0, 0.0, 1.0, 1.0],
                },
            ],
            fade_in: 0.0,
            fade_out: 0.5,
            ..ParticleEmitter::default()
        };
        assert!((emitter.size_at(0.25) - 0.5).abs() < 1e-6);
        let color = emitter.color_at(0.75);
        assert!((color[0] - 0.25).abs() < 1e-6);
        assert!((color[2] - 0.75).abs() < 1e-6);
        assert!((color[3] - 0.5).abs() < 1e-6);
        assert_eq!(ParticleEmitter::default().size_at(0.3), 1.0);
    }

    #[test]
    fn start_colors_tint_each_particle_with_one_of_them() {
        let colors = vec![[1.0, 0.0, 0.0, 1.0], [0.0, 1.0, 0.0, 1.0]];
        let emitter = ParticleEmitter {
            rate: 600.0,
            start_colors: colors.clone(),
            ..ParticleEmitter::default()
        };
        let system = run(&emitter, 10, 3);
        assert!(system.particles.len() > 50);
        assert!(system.particles.iter().all(|p| colors.contains(&p.tint)));
        for color in &colors {
            assert!(system.particles.iter().any(|p| p.tint == *color));
        }
        assert_eq!(pick::<u8>(&[], 0.5), None);
        assert_eq!(pick(&[1, 2], 0.999_999), Some(2));
    }

    #[test]
    fn local_space_particles_follow_the_emitter() {
        let emitter = ParticleEmitter {
            space: ParticleSpace::Local,
            speed: [0.0, 0.0],
            rate: 60.0,
            fade_in: 0.0,
            ..ParticleEmitter::default()
        };
        let system = run(&emitter, 5, 1);
        let mut moved = GlobalTransform::default();
        moved.matrix[3] = [5.0, 0.0, 0.0, 1.0];
        let batch = particle_batch(&emitter, &system, Some(&moved));
        assert!(!batch.instances.is_empty());
        assert!(batch.instances.iter().all(|i| i.position[0] == 5.0));
    }

    #[test]
    fn prewarm_fills_before_the_first_step() {
        let emitter = ParticleEmitter {
            prewarm: true,
            rate: 30.0,
            lifetime: [2.0, 2.0],
            duration: 2.0,
            ..ParticleEmitter::default()
        };
        let mut system = ParticleSystem::default();
        let frame = emitter_frame(None, None);
        system.started = true;
        system.playing = true;
        prewarm(
            &emitter,
            &mut system,
            frame,
            &RandomSeed(1),
            0,
            7,
            1.0 / 60.0,
        );
        assert!(system.alive() > 50, "{}", system.alive());
    }
}
