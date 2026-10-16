//! Game-feel kit: tweens, event-triggered sounds, burst effects, and a
//! scene-data HUD.
//!
//! Each piece is an ordinary registered scene component, so scene files,
//! `scene patch`, scenarios, and the editor inspector see and edit it:
//!
//! - [`Tween`] animates one `Transform` property between two values with an
//!   [`Easing`] curve, once, looping, or ping-ponging.
//! - [`SoundCue`] sends a [`SoundEvent`] when its body starts touching
//!   something or when game code calls [`SoundCue::trigger`]. The runner
//!   turns the event into an [`AudioQueue`](super::AudioQueue) request and
//!   plays it; see `GameScene::play_sound` for sounds from game code.
//! - [`BurstEmitter`] spawns short-lived [`BurstParticle`] entities that fly
//!   out, fall, and shrink. Particles copy the emitter's `MeshRenderer`.
//!   With a `rate` it emits every fixed step, across an `area` box.
//! - [`HudElement`] draws a text label or button over the game view (with the
//!   `ui` feature) and sends [`HudButtonPressed`] when a button is clicked.
//!   `{name}` in its text shows the value of the [`Counter`] called `name`.
//! - [`Counter`] is a named integer with an optional target, and a
//!   [`Pickup`] adds to one when a player body touches it, then hides itself
//!   and fires the cue and emitter on the same entity.
//!
//! Tweens, triggers, and particles run on the fixed step, so scenario tests
//! see the same values on every run. Burst directions come from
//! [`RandomSeed`] indexed by the fixed tick and the emitter's `SceneId`.

use std::collections::{BTreeMap, BTreeSet};
use std::f32::consts::{PI, TAU};
use std::time::Duration;

use bevy_ecs::change_detection::DetectChangesMut;
use bevy_ecs::entity::Entity;
use bevy_ecs::prelude::{
    Commands, Component, Or, Query, Res, ResMut, Resource, With, World,
};
use serde::{Deserialize, Serialize};

use super::sim_math;
use super::{
    EventQueue, FrameTime, GlobalTransform, MeshRenderer, PhysicsWorld,
    RandomSeed, SceneId,
};
use crate::Transform;

/// Shape of a tween's progress over time. `apply` maps 0..1 to 0..1 (Back
/// overshoots past 1 on the way).
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize,
)]
pub enum Easing {
    #[default]
    Linear,
    QuadIn,
    QuadOut,
    QuadInOut,
    CubicOut,
    SineInOut,
    BackOut,
    BounceOut,
}

impl Easing {
    #[must_use]
    pub fn apply(self, t: f32) -> f32 {
        let t = t.clamp(0.0, 1.0);
        match self {
            Self::Linear => t,
            Self::QuadIn => t * t,
            Self::QuadOut => 1.0 - (1.0 - t) * (1.0 - t),
            Self::QuadInOut => {
                if t < 0.5 {
                    2.0 * t * t
                } else {
                    let u = -2.0 * t + 2.0;
                    1.0 - u * u / 2.0
                }
            }
            Self::CubicOut => 1.0 - (1.0 - t) * (1.0 - t) * (1.0 - t),
            Self::SineInOut => -(sim_math::sin_cos(PI * t).1 - 1.0) / 2.0,
            Self::BackOut => {
                let c1 = 1.70158;
                let u = t - 1.0;
                1.0 + (c1 + 1.0) * u * u * u + c1 * u * u
            }
            Self::BounceOut => {
                let (n, d) = (7.5625, 2.75);
                if t < 1.0 / d {
                    n * t * t
                } else if t < 2.0 / d {
                    let t = t - 1.5 / d;
                    n * t * t + 0.75
                } else if t < 2.5 / d {
                    let t = t - 2.25 / d;
                    n * t * t + 0.9375
                } else {
                    let t = t - 2.625 / d;
                    n * t * t + 0.984_375
                }
            }
        }
    }
}

/// The `Transform` field a [`Tween`] writes.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize,
)]
pub enum TweenProperty {
    #[default]
    Position,
    Rotation,
    Scale,
}

/// What a [`Tween`] does after reaching `to`.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize,
)]
pub enum TweenRepeat {
    /// Stops at `to`.
    #[default]
    Once,
    /// Jumps back to `from` and plays again.
    Loop,
    /// Plays back to `from`, then forward again.
    PingPong,
}

/// Animates one `Transform` property from `from` to `to`. Times are in
/// seconds of game time; rotation values are radians. One tween per entity.
#[derive(Component, Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Tween {
    pub property: TweenProperty,
    pub from: [f32; 3],
    pub to: [f32; 3],
    pub duration: f32,
    /// Seconds to hold `from` before starting.
    pub delay: f32,
    pub easing: Easing,
    pub repeat: TweenRepeat,
    /// Time played so far, including the delay.
    #[serde(skip)]
    pub elapsed: f32,
}

impl Default for Tween {
    fn default() -> Self {
        Self {
            property: TweenProperty::Position,
            from: [0.0; 3],
            to: [0.0, 1.0, 0.0],
            duration: 1.0,
            delay: 0.0,
            easing: Easing::QuadInOut,
            repeat: TweenRepeat::PingPong,
            elapsed: 0.0,
        }
    }
}

impl Tween {
    /// Progress through the curve, 0..1, before easing.
    #[must_use]
    pub fn progress(&self) -> f32 {
        if self.duration <= 0.0 {
            return 1.0;
        }
        let played = (self.elapsed - self.delay).max(0.0) / self.duration;
        match self.repeat {
            TweenRepeat::Once => played.min(1.0),
            TweenRepeat::Loop => played.fract(),
            TweenRepeat::PingPong => {
                let phase = played % 2.0;
                if phase > 1.0 {
                    2.0 - phase
                } else {
                    phase
                }
            }
        }
    }

    /// True once a `Once` tween has reached `to`.
    #[must_use]
    pub fn finished(&self) -> bool {
        self.repeat == TweenRepeat::Once
            && self.elapsed >= self.delay + self.duration
    }

    /// The property value at the current time.
    #[must_use]
    pub fn sample(&self) -> [f32; 3] {
        let eased = self.easing.apply(self.progress());
        std::array::from_fn(|axis| {
            self.from[axis] + (self.to[axis] - self.from[axis]) * eased
        })
    }

    /// Starts again from `from`.
    pub fn restart(&mut self) {
        self.elapsed = 0.0;
    }
}

/// How long a [`SoundCue`]'s caption shows.
pub const CUE_CAPTION_SECONDS: f32 = 2.0;

/// Sent by a [`SoundCue`] when it fires. Visible to readers on the next
/// frame, like other events.
#[derive(Clone, Debug, PartialEq)]
pub struct SoundEvent {
    pub entity: Entity,
    /// Asset path of the clip, relative to the project's `assets` folder.
    pub clip: String,
    pub volume: f32,
    /// The cue's caption, empty for none.
    pub caption: String,
}

/// Camera trauma shake: game code adds trauma on a hit or explosion, the
/// shake strength is `trauma` squared, and trauma falls by `decay` per
/// second. The shake moves only the drawn view, never the camera's
/// `Transform`, so it cannot drift.
#[derive(Component, Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CameraShake {
    /// 0 to 1.
    pub trauma: f32,
    /// Trauma lost per second.
    pub decay: f32,
    /// Largest offset in metres along the camera's right, up and back axes.
    pub max_offset: [f32; 3],
    /// Largest roll in radians.
    pub max_roll: f32,
    /// Shake speed in hertz.
    pub frequency: f32,
}

impl Default for CameraShake {
    fn default() -> Self {
        Self {
            trauma: 0.0,
            decay: 1.5,
            max_offset: [0.3, 0.3, 0.0],
            max_roll: 0.05,
            frequency: 12.0,
        }
    }
}

impl CameraShake {
    /// Adds trauma, kept within 0 to 1.
    pub fn add_trauma(&mut self, amount: f32) {
        self.trauma = (self.trauma + amount).clamp(0.0, 1.0);
    }

    /// `matrix` moved and rolled in its own frame by the shake at
    /// `seconds` of game time.
    #[must_use]
    pub fn apply(&self, matrix: [[f32; 4]; 4], seconds: f32) -> [[f32; 4]; 4] {
        let strength = self.trauma * self.trauma;
        if strength <= 0.0 {
            return matrix;
        }
        // Two sines per axis at unrelated phases read as noise and stay
        // smooth from frame to frame.
        let phase = TAU * self.frequency * seconds;
        let wave = |axis: f32| {
            strength
                * (0.6 * (phase + axis * 1.7).sin()
                    + 0.4 * (2.3 * phase + axis * 4.1).sin())
        };
        let mut out = matrix;
        for (axis, offset) in self.max_offset.into_iter().enumerate() {
            let amount = offset * wave(axis as f32);
            for row in 0..3 {
                out[3][row] += matrix[axis][row] * amount;
            }
        }
        let (sin, cos) = (self.max_roll * wave(3.0)).sin_cos();
        for row in 0..3 {
            out[0][row] = matrix[0][row] * cos + matrix[1][row] * sin;
            out[1][row] = matrix[1][row] * cos - matrix[0][row] * sin;
        }
        out
    }
}

/// Bulk spawn: a scene object with this component is copied, children and
/// all, onto a grid of `count` cells `spacing` apart when the game starts
/// (or a scene loads), so a ball pit or a crowd needs one authored object
/// instead of thousands. The original fills cell 0, 0, 0; copies go along
/// positive local X, Y and Z and are named `"<name>#<n>"` from 1. Copies
/// are made in a running game only, never in the editor, and the component
/// is removed once used, so a saved state does not copy again.
#[derive(Component, Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SpawnGrid {
    /// Cells along X, Y and Z, original included.
    pub count: [u32; 3],
    /// Distance between cells along X, Y and Z, in meters.
    pub spacing: [f32; 3],
}

impl Default for SpawnGrid {
    fn default() -> Self {
        Self {
            count: [1; 3],
            spacing: [1.0; 3],
        }
    }
}

/// Squash and stretch: game code calls `scene.squash(name, amount)` on a
/// landing or hit, and a damped spring wobbles the object's `Transform`
/// scale back to rest, keeping its volume. Positive amounts flatten it,
/// negative ones stretch it tall. Colliders scale with the `Transform`, so
/// put this on a visible child, not the physics body.
#[derive(Component, Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Squash {
    /// Spring stiffness per second squared; higher wobbles faster.
    pub stiffness: f32,
    /// Damping per second; higher settles sooner.
    pub damping: f32,
    /// Current squash, -0.8 to 0.8; 0 is at rest.
    #[serde(skip)]
    pub amount: f32,
    #[serde(skip)]
    pub velocity: f32,
    /// The scale the object returns to, taken when a squash starts.
    #[serde(skip)]
    pub rest: Option<[f32; 3]>,
}

impl Default for Squash {
    fn default() -> Self {
        Self {
            stiffness: 400.0,
            damping: 14.0,
            amount: 0.0,
            velocity: 0.0,
            rest: None,
        }
    }
}

impl Squash {
    /// Adds `amount` of squash (negative stretches).
    pub fn squash(&mut self, amount: f32) {
        self.amount = (self.amount + amount).clamp(-0.8, 0.8);
    }
}

/// Hit flash: game code calls `scene.flash(name)` and the object and its
/// children draw tinted toward `color`, fading back over `duration`
/// seconds. Only the drawn colors change; the shared material does not.
#[derive(Component, Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Flash {
    /// Linear RGB the object turns at full strength.
    pub color: [f32; 3],
    /// Seconds to fade from full tint back to none.
    pub duration: f32,
    /// Seconds of fade left.
    #[serde(skip)]
    pub remaining: f32,
}

impl Default for Flash {
    fn default() -> Self {
        Self {
            color: [1.0; 3],
            duration: 0.12,
            remaining: 0.0,
        }
    }
}

impl Flash {
    /// Starts the flash again at full strength.
    pub fn flash(&mut self) {
        self.remaining = self.duration;
    }

    /// Tint strength now, 1 when the flash starts and 0 once it ends.
    #[must_use]
    pub fn strength(&self) -> f32 {
        if self.duration > 0.0 {
            (self.remaining / self.duration).clamp(0.0, 1.0)
        } else {
            0.0
        }
    }
}

/// Real time left on a hit-stop freeze. The windowed loop holds back that
/// much real time from the runtime, so fixed ticks pause for a moment and
/// then carry on exactly as they would have. Simulation results never
/// change, and headless runs, which step ticks directly, ignore it.
#[derive(Resource, Clone, Copy, Debug, Default, PartialEq)]
pub struct HitStop {
    pub remaining: Duration,
}

impl HitStop {
    /// Freezes for `seconds` (0 to 1), unless a longer freeze is running.
    pub fn stop(&mut self, seconds: f32) {
        // Whole microseconds, so 0.05 is exactly 50 ms rather than f32 noise.
        let seconds = Duration::from_micros(
            (seconds.clamp(0.0, 1.0) * 1e6).round() as u64,
        );
        self.remaining = self.remaining.max(seconds);
    }
}

/// Takes a real frame delta and returns what is left of it after any
/// running hit-stop has eaten its share.
pub fn after_hit_stop(world: &mut World, delta: Duration) -> Duration {
    let Some(mut stop) = world.get_resource_mut::<HitStop>() else {
        return delta;
    };
    if stop.remaining.is_zero() {
        return delta;
    }
    let eaten = stop.remaining.min(delta);
    stop.remaining -= eaten;
    delta - eaten
}

/// Per fixed step: fades hit flashes.
pub(super) fn fade_flashes(
    time: Res<FrameTime>,
    mut flashes: Query<&mut Flash>,
) {
    let dt = time.fixed_delta.as_secs_f32();
    for mut flash in &mut flashes {
        if flash.remaining > 0.0 {
            flash.remaining = (flash.remaining - dt).max(0.0);
        }
    }
}

/// Per fixed step: moves squash springs and scales their objects.
pub(super) fn spring_squash(
    time: Res<FrameTime>,
    mut squashes: Query<(&mut Squash, &mut Transform)>,
) {
    let dt = time.fixed_delta.as_secs_f32();
    for (mut squash, mut transform) in &mut squashes {
        if squash.amount == 0.0 && squash.velocity == 0.0 {
            continue;
        }
        let rest = *squash.rest.get_or_insert(transform.scale);
        let force = -squash.stiffness * squash.amount
            - squash.damping * squash.velocity;
        squash.velocity += force * dt;
        squash.amount = (squash.amount + squash.velocity * dt).clamp(-0.8, 0.8);
        if squash.amount.abs() < 1e-4 && squash.velocity.abs() < 1e-3 {
            squash.amount = 0.0;
            squash.velocity = 0.0;
            squash.rest = None;
            transform.scale = rest;
            continue;
        }
        let tall = 1.0 - squash.amount;
        let wide = 1.0 / tall.sqrt();
        transform.scale = [rest[0] * wide, rest[1] * tall, rest[2] * wide];
    }
}

/// Per fixed step: lets camera trauma fade.
pub(super) fn decay_camera_shake(
    time: Res<FrameTime>,
    mut shakes: Query<&mut CameraShake>,
) {
    let dt = time.fixed_delta.as_secs_f32();
    for mut shake in &mut shakes {
        if shake.trauma > 0.0 {
            shake.trauma = (shake.trauma - shake.decay * dt).max(0.0);
        }
    }
}

/// Asks for a sound. See the module docs.
#[derive(Component, Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SoundCue {
    pub clip: String,
    /// Linear gain, 1 is the clip's own level.
    pub volume: f32,
    /// Fire when this body's collider starts touching another one.
    pub on_collision: bool,
    /// Caption shown for [`CUE_CAPTION_SECONDS`] each time it fires, such
    /// as `[glass breaks]`; empty shows none.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub caption: String,
    /// Closing speed in m/s at which a collision plays at full `volume`;
    /// slower hits play quieter in proportion. 0 plays every hit at full
    /// `volume`.
    pub full_volume_speed: f32,
    /// With `on_collision`, only touches of a collider whose
    /// `PhysicsMaterial::name` is this count; empty for any.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub with_material: String,
    /// Fire when a GPU physics event with this registered name arrives for
    /// this body; empty for none.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub on_gpu_event: String,
    /// Set by [`SoundCue::trigger`]; the next fixed step fires and clears it.
    #[serde(skip)]
    pub triggered: bool,
    #[serde(skip)]
    pub touching: bool,
    /// Closing speed of the contact that triggered it; `None` when game
    /// code did.
    #[serde(skip)]
    pub hit_speed: Option<f32>,
}

impl Default for SoundCue {
    fn default() -> Self {
        Self {
            clip: String::new(),
            volume: 1.0,
            on_collision: true,
            caption: String::new(),
            full_volume_speed: 0.0,
            with_material: String::new(),
            on_gpu_event: String::new(),
            triggered: false,
            touching: false,
            hit_speed: None,
        }
    }
}

impl SoundCue {
    pub fn trigger(&mut self) {
        self.triggered = true;
        self.hit_speed = None;
    }
}

/// Spawns a burst of particles. See the module docs.
#[derive(Component, Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct BurstEmitter {
    pub count: u32,
    /// Launch speed in metres per second.
    pub speed: f32,
    /// Seconds each particle lives.
    pub lifetime: f32,
    /// Particle size as a factor of the emitter's scale.
    pub particle_scale: f32,
    /// Downward acceleration in metres per second squared.
    pub gravity: f32,
    /// Fire when this body's collider starts touching another one.
    pub on_collision: bool,
    /// Particles per second emitted every fixed step while above 0, with
    /// no trigger: rain, smoke, sparks.
    pub rate: f32,
    /// Half extents in metres of a world-aligned box around the emitter
    /// where particles start; 0 starts them all at its center.
    pub area: [f32; 3],
    /// Height factor of each particle, so 8 makes a falling streak.
    pub stretch: f32,
    /// Carries the fraction of a particle `rate` has not emitted yet.
    #[serde(skip)]
    pub pending: f32,
    /// Set by [`BurstEmitter::trigger`]; the next fixed step fires and
    /// clears it.
    #[serde(skip)]
    pub triggered: bool,
    #[serde(skip)]
    pub touching: bool,
}

impl Default for BurstEmitter {
    fn default() -> Self {
        Self {
            count: 12,
            speed: 3.0,
            lifetime: 0.6,
            particle_scale: 0.15,
            gravity: 9.81,
            on_collision: true,
            rate: 0.0,
            area: [0.0; 3],
            stretch: 1.0,
            pending: 0.0,
            triggered: false,
            touching: false,
        }
    }
}

impl BurstEmitter {
    pub fn trigger(&mut self) {
        self.triggered = true;
    }
}

/// One particle from a [`BurstEmitter`]. Runtime only; never saved.
#[derive(Component, Clone, Copy, Debug, PartialEq)]
pub struct BurstParticle {
    pub velocity: [f32; 3],
    pub gravity: f32,
    pub remaining: f32,
    pub lifetime: f32,
    pub scale: [f32; 3],
}

/// Screen corner, edge, or center a [`HudElement`] is placed against.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize,
)]
pub enum HudAnchor {
    #[default]
    TopLeft,
    Top,
    TopRight,
    Center,
    BottomLeft,
    Bottom,
    BottomRight,
}

/// A text label or button drawn over the game view. `offset` is in logical
/// pixels and points from the anchor toward the screen center.
#[derive(Component, Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct HudElement {
    pub text: String,
    pub anchor: HudAnchor,
    pub offset: [f32; 2],
    pub font_size: f32,
    /// sRGB text color with alpha.
    pub color: [f32; 4],
    /// Draw as a button that sends [`HudButtonPressed`] when clicked.
    pub button: bool,
    /// Name of a [`Counter`] that must reach its target before this element
    /// shows.
    pub requires: Option<String>,
    /// Name of a camera: the element anchors to that camera's viewport and
    /// shows only while the camera is active.
    pub camera: Option<String>,
    /// Name of an object: the element sits on that object's place on
    /// screen (plus `follow_offset` in world space), aligned by `anchor`,
    /// with `offset` in pixels (+y down). Hidden while the object is behind
    /// the camera or missing.
    pub follow: Option<String>,
    pub follow_offset: [f32; 3],
    /// A TTF or OTF file relative to `assets/`, such as `fonts/pixel.ttf`;
    /// none uses the built-in font. Glyphs the file lacks come from the
    /// built-in font.
    pub font: Option<String>,
}

impl Default for HudElement {
    fn default() -> Self {
        Self {
            text: "Text".into(),
            anchor: HudAnchor::TopLeft,
            offset: [16.0, 16.0],
            font_size: 18.0,
            color: [1.0; 4],
            button: false,
            requires: None,
            camera: None,
            follow: None,
            follow_offset: [0.0; 3],
            font: None,
        }
    }
}

/// A weighted loot table on a named object, edited in the scene and rolled
/// by game code's `GameScene::roll_loot_table`.
#[derive(
    Component, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize,
)]
#[serde(default)]
pub struct LootTable {
    pub entries: Vec<LootEntry>,
}

/// A branching conversation on a named object, edited in the scene and
/// stepped by game code's `GameScene::start_dialogue` and
/// `advance_dialogue`. `current` is the id of the line being shown, empty
/// when the dialogue is not running, so snapshots and saves keep the spot.
#[derive(
    Component, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize,
)]
#[serde(default)]
pub struct Dialogue {
    pub lines: Vec<DialogueLine>,
    pub current: String,
}

/// One [`Dialogue`] line. `speaker`, `text` and choice texts are
/// translation keys (shown as they are when the locale lacks them). With
/// no choices the dialogue moves on to `next`; an empty or unknown `next`
/// ends it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct DialogueLine {
    pub id: String,
    pub speaker: String,
    pub text: String,
    pub next: String,
    pub choices: Vec<DialogueChoice>,
}

/// A player answer on a [`DialogueLine`]: picking it adds `add` to the
/// counter `counter` (empty for none) and moves on to `next`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct DialogueChoice {
    pub text: String,
    pub next: String,
    pub counter: String,
    pub add: i32,
}

/// One [`LootTable`] entry: `item` is picked with a chance of `weight`
/// over the table's total.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct LootEntry {
    pub item: String,
    pub weight: u32,
}

/// Hit points of a player, enemy or crate, changed by game code's
/// `GameScene::damage`. Objects with the same non-empty `team` are allies.
/// Scenarios can expect `/components/rusting.health/value`.
#[derive(Component, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Health {
    pub value: i32,
    /// Healing stops here.
    pub max: i32,
    pub team: String,
}

/// Sent by `GameScene::damage` to the damaged object, so a handler
/// connected with `App::connect` and registered as
/// `In<Signal<Damaged>>` reacts to hits and heals.
#[derive(bevy_ecs::event::EntityEvent, Clone, Debug, PartialEq, Eq)]
pub struct Damaged {
    pub entity: Entity,
    /// The amount asked for; negative heals.
    pub amount: i32,
    /// Hit points after the change.
    pub health: i32,
}

impl Default for Health {
    fn default() -> Self {
        Self {
            value: 3,
            max: 3,
            team: String::new(),
        }
    }
}

/// The current state of an object's state machine, such as an enemy's
/// `"patrol"` or `"chase"`, set by game code's `GameScene::set_state`.
/// Scenarios can expect `/components/rusting.state/state`.
///
/// `transitions` make it a state machine the engine runs: each fixed tick
/// the first transition that applies moves the object to its `to` state.
#[derive(
    Component, Clone, Debug, Default, PartialEq, Serialize, Deserialize,
)]
#[serde(default)]
pub struct ObjectState {
    pub state: String,
    /// The fixed tick the state was entered on.
    pub since_tick: u64,
    pub transitions: Vec<StateTransition>,
    pub actions: Vec<StateAction>,
}

impl ObjectState {
    /// The counter additions `actions` make when leaving `from` for `to`:
    /// exit actions of `from`, then enter actions of `to`.
    pub fn changes(&self, from: &str, to: &str) -> Vec<(String, i32)> {
        let exits = self.actions.iter().filter(|a| a.exit && a.state == from);
        let enters = self.actions.iter().filter(|a| !a.exit && a.state == to);
        exits
            .chain(enters)
            .filter(|action| !action.counter.is_empty())
            .map(|action| (action.counter.clone(), action.add))
            .collect()
    }
}

/// Adds `add` to the counter `counter` (created when missing) when an
/// [`ObjectState`] enters `state`, or leaves it when `exit` is set, by a
/// transition or `GameScene::set_state`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct StateAction {
    pub state: String,
    pub exit: bool,
    pub counter: String,
    pub add: i32,
}

/// One edge of an [`ObjectState`] machine. It applies when the object is
/// in `from` (or `from` is empty), has been there `after_seconds`, and the
/// counter called `counter` (when not empty) is at least `at_least`.
/// When `held` is not empty, its input action must be held too, and when
/// `touching` is not empty, the object must touch an object of that name.
/// Taking it adds `then_add` to the counter `then_counter` (when not
/// empty), creating the counter if needed.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct StateTransition {
    pub from: String,
    pub to: String,
    pub after_seconds: f32,
    pub counter: String,
    pub at_least: i32,
    pub held: String,
    pub touching: String,
    pub then_counter: String,
    pub then_add: i32,
}

/// Moves each [`ObjectState`] along its first applying transition. Guards
/// read the counters from before this tick's transitions, and the counter
/// additions apply after all of them, so the order of objects does not
/// matter.
pub fn run_state_machines(
    mut commands: Commands,
    time: Res<FrameTime>,
    mut machines: Query<(Entity, &mut ObjectState)>,
    mut counters: Query<(&mut Counter, Option<&SceneId>)>,
    (actions, input): (
        Option<Res<super::ActionMap>>,
        Option<Res<super::RuntimeInput>>,
    ),
    physics: Option<Res<PhysicsWorld>>,
    names: Query<&super::Name>,
) {
    let touching = |entity: Entity, name: &str| {
        name.is_empty()
            || physics.as_ref().is_some_and(|physics| {
                physics.contacts().iter().any(|contact| {
                    let other = if contact.a == entity {
                        contact.b
                    } else if contact.b == entity {
                        contact.a
                    } else {
                        return false;
                    };
                    names.get(other).is_ok_and(|other| other.0 == name)
                })
            })
    };
    let held = |action: &str| {
        action.is_empty()
            || actions
                .as_ref()
                .zip(input.as_ref())
                .is_some_and(|(actions, input)| actions.held(input, action))
    };
    let mut adds = std::collections::BTreeMap::<String, i32>::new();
    let delta = time.fixed_delta.as_secs_f64();
    for (entity, mut machine) in &mut machines {
        let elapsed =
            time.fixed_tick.saturating_sub(machine.since_tick) as f64 * delta;
        let next = machine.transitions.iter().find(|edge| {
            (edge.from.is_empty() || edge.from == machine.state)
                && edge.to != machine.state
                && elapsed + 1e-9 >= f64::from(edge.after_seconds)
                && held(&edge.held)
                && touching(entity, &edge.touching)
                && (edge.counter.is_empty()
                    || find_counter(counters.iter(), &edge.counter)
                        .is_some_and(|counter| counter.value >= edge.at_least))
        });
        if let Some(edge) = next {
            let mut changes = vec![(edge.then_counter.clone(), edge.then_add)];
            changes.extend(machine.changes(&machine.state, &edge.to));
            for (counter, value) in changes {
                if !counter.is_empty() {
                    let add = adds.entry(counter).or_default();
                    *add = add.saturating_add(value);
                }
            }
            let to = edge.to.clone();
            machine.state = to;
            machine.since_tick = time.fixed_tick;
        }
    }
    for (name, add) in adds {
        match find_counter(counters.iter_mut(), &name) {
            Some(mut counter) => {
                counter.value = counter.value.saturating_add(add);
            }
            // Made the way `GameScene::set_counter` makes one, so
            // scenarios and saves find it.
            None => commands.queue(move |world: &mut World| {
                let order = super::next_spawn_order(world);
                world.spawn((
                    super::Name(name.clone()),
                    SceneId::new(),
                    order,
                    Counter {
                        name,
                        value: add,
                        target: None,
                    },
                ));
            }),
        }
    }
}

/// A named integer shown by HUD `{name}` placeholders and raised by
/// [`Pickup`]s.
#[derive(Component, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Counter {
    pub name: String,
    pub value: i32,
    /// The counter is complete once `value` reaches this.
    pub target: Option<i32>,
}

impl Default for Counter {
    fn default() -> Self {
        Self {
            name: "score".into(),
            value: 0,
            target: None,
        }
    }
}

impl Counter {
    #[must_use]
    pub fn complete(&self) -> bool {
        self.target.is_some_and(|target| self.value >= target)
    }
}

/// Collected once when a body with a `PlatformerController` or
/// `PlayerController` touches this entity's collider (make it a sensor).
/// Collecting adds `value` to the [`Counter`] named `counter`, hides the
/// entity, and triggers its [`SoundCue`] and [`BurstEmitter`].
#[derive(Component, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Pickup {
    pub counter: String,
    pub value: i32,
    /// Name of a [`Counter`] that must be complete before this can be
    /// collected.
    pub requires: Option<String>,
    pub collected: bool,
}

impl Default for Pickup {
    fn default() -> Self {
        Self {
            counter: "score".into(),
            value: 1,
            requires: None,
            collected: false,
        }
    }
}

/// The counter called `name` with the lowest `SceneId`, so duplicates
/// resolve the same way on every run.
pub(crate) fn find_counter<'a, C: std::ops::Deref<Target = Counter>>(
    counters: impl Iterator<Item = (C, Option<&'a SceneId>)>,
    name: &str,
) -> Option<C> {
    counters
        .filter(|(counter, _)| counter.name == name)
        .min_by_key(|(_, id)| id.map(|id| id.0))
        .map(|(counter, _)| counter)
}

/// Multiplies the size of HUD text and captions, for players who need
/// larger text; set by `GameScene::set_text_scale`. 1 is as authored.
#[derive(Resource, Clone, Copy, Debug, PartialEq)]
pub struct TextScale(pub f32);

impl Default for TextScale {
    fn default() -> Self {
        Self(1.0)
    }
}

/// The action a `{binding:action}` HUD button waits to rebind: the next
/// key pressed replaces its inputs on that device, Escape cancels.
#[derive(Resource, Clone, Debug, Default, PartialEq, Eq)]
pub struct RebindWait(pub Option<String>);

/// The current locale's strings, set by `GameScene::set_locale` from
/// `assets/locales/<locale>.json`, a flat JSON object of key to text.
#[derive(Resource, Clone, Debug, Default, PartialEq, Eq)]
pub struct Translations {
    pub locale: String,
    pub strings: std::collections::BTreeMap<String, String>,
}

impl Translations {
    /// The text for `key`, or `key` itself when the table lacks it, so a
    /// missing translation shows where it is missing.
    #[must_use]
    pub fn get<'a>(&'a self, key: &'a str) -> &'a str {
        self.strings.get(key).map_or(key, String::as_str)
    }

    /// The text for `key.<form>`, with `{count}` replaced by `count`,
    /// where the form is the locale's CLDR plural category for `count`
    /// (`one`, `few`, `many` or `other`); `key.other` when the table lacks
    /// that form, and `key` when it lacks both.
    #[must_use]
    pub fn plural(&self, key: &str, count: i64) -> String {
        let form = plural_form(&self.locale, count);
        self.strings
            .get(&format!("{key}.{form}"))
            .or_else(|| self.strings.get(&format!("{key}.other")))
            .map_or(key, String::as_str)
            .replace("{count}", &count.to_string())
    }
}

/// The CLDR plural category of a whole `count` in `locale` (its language
/// before any `-` or `_`).
// ponytail: integer rules for the common game languages; every other
// language uses English's one/other. Add Arabic's zero/two and Welsh when
// a game ships them.
fn plural_form(locale: &str, count: i64) -> &'static str {
    let language = locale.split(['-', '_']).next().unwrap_or_default();
    let n = count.unsigned_abs();
    let (ten, hundred) = (n % 10, n % 100);
    let few = (2..=4).contains(&ten) && !(12..=14).contains(&hundred);
    match language {
        "ja" | "zh" | "ko" | "th" | "vi" | "id" | "ms" | "tr" => "other",
        "fr" | "pt" if n <= 1 => "one",
        "ru" | "uk" | "be" | "sr" | "hr" | "bs" => match () {
            () if ten == 1 && hundred != 11 => "one",
            () if few => "few",
            () => "many",
        },
        "pl" => match () {
            () if n == 1 => "one",
            () if few => "few",
            () => "many",
        },
        "cs" | "sk" => match n {
            1 => "one",
            2..=4 => "few",
            _ => "other",
        },
        _ if n == 1 => "one",
        _ => "other",
    }
}

/// Replaces every `{tr:key}` with its translation, then every `{name}`
/// whose name is a counter with its value, so a translation can hold
/// counter placeholders.
#[must_use]
pub fn hud_text<'a>(
    text: &str,
    counters: impl Iterator<Item = (&'a Counter, Option<&'a SceneId>)> + Clone,
    translations: Option<&Translations>,
) -> String {
    let translated = fill_placeholders(text, |name| {
        let key = name.strip_prefix("tr:")?;
        Some(translations.map_or(key, |t| t.get(key)).to_owned())
    });
    fill_placeholders(&translated, |name| {
        find_counter(counters.clone(), name)
            .map(|counter| counter.value.to_string())
    })
}

/// The text of a `{dialogue:Object}` HUD placeholder: the line the named
/// object's dialogue shows, as `speaker: text` translated; and of
/// `{dialogue:Object/n}`: the line's choice `n` (from 1), or `Continue`
/// (the `dialogue.continue` translation) for `/1` on a line with no
/// choices. Empty when the dialogue is not running or has no such
/// choice; `None` when no object with a dialogue has that name.
#[must_use]
pub fn dialogue_placeholder<'a>(
    placeholder: &str,
    mut dialogues: impl Iterator<Item = (&'a super::Name, &'a Dialogue)>,
    translations: Option<&Translations>,
) -> Option<String> {
    let key = placeholder.strip_prefix("dialogue:")?;
    let (object, choice) = match key.rsplit_once('/') {
        Some((object, n)) => (object, Some(n.parse::<usize>().ok()?)),
        None => (key, None),
    };
    let (_, dialogue) = dialogues.find(|(name, _)| name.0 == object)?;
    let tr = |key: &str| translations.map_or(key, |t| t.get(key)).to_owned();
    let Some(line) = dialogue
        .lines
        .iter()
        .find(|line| line.id == dialogue.current && !line.id.is_empty())
    else {
        return Some(String::new());
    };
    Some(match choice {
        None if line.speaker.is_empty() => tr(&line.text),
        None => format!("{}: {}", tr(&line.speaker), tr(&line.text)),
        Some(1) if line.choices.is_empty() => translations
            .and_then(|t| t.strings.get("dialogue.continue").cloned())
            .unwrap_or_else(|| "Continue".to_owned()),
        Some(n) => n
            .checked_sub(1)
            .and_then(|i| line.choices.get(i))
            .map(|choice| tr(&choice.text))
            .unwrap_or_default(),
    })
}

/// The text of a `{state:Object}` HUD placeholder: the named object's
/// `rusting.state`, translated when the locale has it as a key, so a
/// quest stage such as `quest.find_hammer` reads as a sentence. `None`
/// when no object with a state has that name.
#[must_use]
pub fn state_placeholder<'a>(
    placeholder: &str,
    mut states: impl Iterator<Item = (&'a super::Name, &'a ObjectState)>,
    translations: Option<&Translations>,
) -> Option<String> {
    let object = placeholder.strip_prefix("state:")?;
    let (_, state) = states.find(|(name, _)| name.0 == object)?;
    Some(
        translations
            .map_or(&*state.state, |t| t.get(&state.state))
            .to_owned(),
    )
}

/// The text of a `{binding:action}` HUD placeholder: the inputs the
/// scene's `rusting.input_action` binds to `action`, joined with ` / `,
/// or `Press a key` (the `binding.waiting` translation) while `waiting`
/// names that action. `None` for other placeholders.
#[must_use]
pub fn binding_placeholder<'a>(
    placeholder: &str,
    mut actions: impl Iterator<Item = &'a super::InputAction>,
    waiting: Option<&str>,
    translations: Option<&Translations>,
) -> Option<String> {
    let action = placeholder.strip_prefix("binding:")?;
    if waiting == Some(action) {
        return Some(
            translations
                .and_then(|t| t.strings.get("binding.waiting").cloned())
                .unwrap_or_else(|| "Press a key".to_owned()),
        );
    }
    Some(
        actions
            .find(|found| found.action == action)
            .map(|found| found.inputs.join(" / "))
            .unwrap_or_default(),
    )
}

/// Replaces every `{name}` that `value` knows; leaves the rest as written.
fn fill_placeholders(
    text: &str,
    value: impl Fn(&str) -> Option<String>,
) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        let filled = after
            .find('}')
            .and_then(|close| value(&after[..close]).map(|text| (text, close)));
        match filled {
            Some((text, close)) => {
                out.push_str(&text);
                rest = &after[close + 1..];
            }
            None => {
                out.push('{');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// Scene-wide clear color behind everything drawn, as linear RGBA. The
/// first one found sets [`RenderSettings::background_color`](super::RenderSettings).
#[derive(Component, Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SceneBackground {
    pub color: [f32; 4],
}

impl Default for SceneBackground {
    fn default() -> Self {
        Self {
            color: super::RenderSettings::default().background_color,
        }
    }
}

/// Per frame: copies the scene's background into the render settings.
pub(super) fn apply_scene_background(
    settings: Option<ResMut<super::RenderSettings>>,
    backgrounds: Query<(&SceneBackground, Option<&SceneId>)>,
) {
    let first = backgrounds
        .iter()
        .min_by_key(|(_, id)| id.map(|id| id.0))
        .map(|(background, _)| background.color);
    if let (Some(mut settings), Some(color)) = (settings, first) {
        if settings.background_color != color {
            settings.background_color = color;
        }
    }
}

/// Daylight sky computed from the sun: blue overhead, pale toward the
/// horizon, orange at sunset, dark at night. It is drawn behind everything
/// and lights surfaces from every side and in their reflections, like an
/// [`EnvironmentMap`] (which wins for lighting when both exist). The sun
/// is the shadow-casting directional light, else the first one. The one
/// on the entity with the lowest ID is used.
#[derive(Component, Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ProceduralSky {
    /// Multiplies the sky's brightness, which follows the sun's.
    pub intensity: f32,
    /// 0 is clear air, 1 a hazy, washed-out sky with a wide glow around
    /// the sun.
    pub haze: f32,
    /// Linear RGB albedo of the ground seen below the horizon.
    pub ground_color: [f32; 3],
    /// Diameter of the drawn sun disc in degrees; 0 hides it. The real sun
    /// is 0.53.
    pub sun_size: f32,
    /// Share of the sky under clouds: 0 clear, 1 overcast. Clouds catch
    /// the sun at their edges and hide its disc.
    pub cloud_cover: f32,
    /// 0 white clouds, 1 dark storm clouds where they are thick.
    pub cloud_darkness: f32,
    /// How fast the clouds drift, in metres per second at their height of
    /// 2 km.
    pub cloud_speed: f32,
    /// Direction the clouds drift toward, as world x and z.
    pub wind_direction: [f32; 2],
}

impl Default for ProceduralSky {
    fn default() -> Self {
        Self {
            intensity: 1.0,
            haze: 0.2,
            ground_color: [0.3, 0.28, 0.25],
            sun_size: 1.0,
            cloud_cover: 0.0,
            cloud_darkness: 0.3,
            cloud_speed: 10.0,
            wind_direction: [1.0, 0.0],
        }
    }
}

/// Equirectangular (2:1) sky image that surfaces reflect and that lights
/// them from every side, replacing the [`SkyLight`](super::SkyLight)
/// hemisphere. Rough surfaces sample blurrier mip levels. A Radiance `.hdr`
/// image keeps light brighter than white, such as the sun. With
/// `background`, the image is also drawn behind the scene. The one on the
/// entity with the lowest ID is used.
#[derive(Component, Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct EnvironmentMap {
    /// Image path relative to the project's `assets` folder.
    pub texture: std::path::PathBuf,
    pub intensity: f32,
    /// Draws the image behind the scene, in place of the clear color.
    pub background: bool,
    /// The loaded `texture`, set by the engine.
    #[serde(skip)]
    pub handle: Option<crate::assets::Handle<crate::assets::TextureAsset>>,
}

impl Default for EnvironmentMap {
    fn default() -> Self {
        Self {
            texture: std::path::PathBuf::new(),
            intensity: 1.0,
            background: false,
            handle: None,
        }
    }
}

/// Box around the entity's position whose surfaces reflect the scene as
/// seen from that position, in place of the [`EnvironmentMap`]. Reflections
/// are projected onto the box walls, so a probe sized to its room lines up
/// with the walls. Overlapping probes blend; up to four are used.
// ponytail: captured when a probe is added or changes, not every frame, so
// moving objects do not show in probe reflections.
#[derive(Component, Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ReflectionProbe {
    /// Half size of the box along each world axis.
    pub extents: [f32; 3],
    pub intensity: f32,
}

impl Default for ReflectionProbe {
    fn default() -> Self {
        Self {
            extents: [5.0; 3],
            intensity: 1.0,
        }
    }
}

/// Grid of light probes filling a world-aligned box of half size `extents`
/// around the object: each probe records the light arriving from six
/// directions, and surfaces inside the box take their ambient light from
/// the nearest probes instead of the sky or environment, so a dark room
/// stays dark and a red wall tints what stands next to it. The first one in
/// entity order is used. Baked when the grid is added or changes; the High
/// quality profile also recaptures two probes per frame, so moving objects
/// and lights relight the grid within a few frames (one bounce). Eco and
/// Balanced keep the baked probes.
#[derive(Component, Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct LightProbeGrid {
    /// Half size of the box along each world axis.
    pub extents: [f32; 3],
    /// Probes along each axis, spread from wall to wall. The renderer
    /// clamps each to 1..=8 and the total to 64.
    pub resolution: [u32; 3],
    pub intensity: f32,
}

impl Default for LightProbeGrid {
    fn default() -> Self {
        Self {
            extents: [5.0; 3],
            resolution: [3; 3],
            intensity: 1.0,
        }
    }
}

/// Bakes the light bouncing onto this object into a lightmap: six square
/// faces of `resolution` texels wrapped around its mesh's box. Each texel
/// sees the sky and the direct light reflected off other lightmapped
/// objects, and replaces the sky, environment and probe light on the
/// object's surface. Direct light stays real-time.
// ponytail: box-projected, so concave meshes share texels between hidden
// and visible faces; a UV2 unwrap would fix that. Baked when lightmapped
// objects, their meshes or materials, or lights change.
#[derive(Component, Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Lightmap {
    /// Texels along each side of each face, clamped to 2..=64.
    pub resolution: u32,
}

impl Default for Lightmap {
    fn default() -> Self {
        Self { resolution: 16 }
    }
}

/// Scatters `count` copies of this object's mesh over a disc of `radius`
/// metres on its local XZ plane, each turned and scaled at random by
/// `seed`, in place of the single object. Copies share the object's
/// material, so they draw as one instanced batch. Wind and other force
/// fields lean each copy away from its base by `sway` per m/s² of push.
/// `conform` sets each copy on the heightfield below it, and `layers` keeps
/// only the copies standing on chosen terrain layers. On a parent, every
/// mesh under it is copied with each placement, so a two-mesh palm
/// scatters whole.
#[derive(Component, Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Foliage {
    /// Copies drawn, at most [`MAX_FOLIAGE_COPIES`].
    pub count: u32,
    /// Disc radius in metres.
    pub radius: f32,
    /// Picks the scatter; the same seed gives the same layout.
    pub seed: u32,
    /// Smallest and largest uniform scale of a copy.
    pub scale_min: f32,
    pub scale_max: f32,
    /// Lean per m/s² of force field push, in metres per metre of height.
    pub sway: f32,
    /// Moves each copy up or down onto the [`crate::runtime::Heightfield`]
    /// straight below or above it, so the scatter follows hills instead of
    /// the object's flat disc. Copies off every heightfield stay where the
    /// disc puts them. Placed again when the object or ground changes.
    pub conform: bool,
    /// Indices into the [`crate::runtime::Terrain`] layers under the copies:
    /// a copy is drawn only where these layers make up at least half of
    /// the ground's blend, such as grass but not sand. Empty draws every
    /// copy; copies off every terrain are dropped.
    pub layers: Vec<u32>,
}

/// Most copies one [`Foliage`] draws.
pub const MAX_FOLIAGE_COPIES: u32 = 65_536;

impl Default for Foliage {
    fn default() -> Self {
        Self {
            count: 100,
            radius: 5.0,
            seed: 0,
            scale_min: 0.8,
            scale_max: 1.2,
            sway: 0.03,
            conform: false,
            layers: Vec::new(),
        }
    }
}

impl Foliage {
    /// Local matrix of each copy: a point in the disc, a yaw and a scale.
    #[must_use]
    pub fn placements(&self) -> Vec<nalgebra::Matrix4<f32>> {
        let seed = crate::runtime::RandomSeed(u64::from(self.seed));
        let streams = [
            "foliage_angle",
            "foliage_distance",
            "foliage_yaw",
            "foliage_scale",
        ]
        .map(|name| crate::runtime::RandomSeed::stream(name, 0));
        (0..u64::from(self.count.min(MAX_FOLIAGE_COPIES)))
            .map(|index| {
                let [angle, distance, yaw, scale] =
                    streams.map(|stream| seed.unit(index, stream));
                // sqrt spreads copies evenly over the disc's area.
                let distance = self.radius.max(0.0) * distance.sqrt();
                let angle = angle * TAU;
                let scale =
                    self.scale_min + (self.scale_max - self.scale_min) * scale;
                nalgebra::Matrix4::new_translation(&nalgebra::Vector3::new(
                    distance * angle.cos(),
                    0.0,
                    distance * angle.sin(),
                )) * nalgebra::Matrix4::from_axis_angle(
                    &nalgebra::Vector3::y_axis(),
                    yaw * TAU,
                ) * nalgebra::Matrix4::new_scaling(scale)
            })
            .collect()
    }
}

/// Shear that leans everything above world point `base` by `lean` (x and
/// z metres per metre of height), keeping heights.
#[must_use]
pub fn lean_matrix(base: [f32; 3], lean: [f32; 2]) -> nalgebra::Matrix4<f32> {
    let base = nalgebra::Vector3::from(base);
    #[rustfmt::skip]
    let shear = nalgebra::Matrix4::new(
        1.0, lean[0], 0.0, 0.0,
        0.0, 1.0, 0.0, 0.0,
        0.0, lean[1], 1.0, 0.0,
        0.0, 0.0, 0.0, 1.0,
    );
    nalgebra::Matrix4::new_translation(&base)
        * shear
        * nalgebra::Matrix4::new_translation(&-base)
}

/// Exponential height fog: the scene fades into `color` with distance, and
/// the fog thins out above `height`. Looking toward the sun brightens it.
/// The one on the entity with the lowest ID is used.
#[derive(Component, Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Fog {
    /// Linear RGB color that distant objects fade into.
    pub color: [f32; 3],
    /// Extinction per metre at `height`. 0.01 hides things about 300 m away.
    pub density: f32,
    /// World height, in metres, where the fog has its full `density`.
    pub height: f32,
    /// How fast the fog thins with height, per metre. 0 makes it uniform.
    pub height_falloff: f32,
    /// Brightens fog toward the shadow-casting (or first) directional light.
    pub sun_scatter: f32,
    /// How much the background behind everything fades into the fog.
    pub sky_affect: f32,
    /// How much shadows block the glow toward the sun, 0 to 1: at 1 the
    /// fog behind shadow-casting objects stays dark, showing light shafts.
    /// Needs the sun to cast shadows.
    pub light_shafts: f32,
}

impl Default for Fog {
    fn default() -> Self {
        Self {
            color: [0.5, 0.6, 0.7],
            density: 0.01,
            height: 0.0,
            height_falloff: 0.1,
            sun_scatter: 0.3,
            sky_affect: 1.0,
            light_shafts: 0.0,
        }
    }
}

impl Fog {
    /// Blends toward `other` by `t` (0 keeps `self`, 1 gives `other`).
    #[must_use]
    pub fn lerp(self, other: Self, t: f32) -> Self {
        let mix = |a: f32, b: f32| a + (b - a) * t;
        Self {
            color: std::array::from_fn(|i| mix(self.color[i], other.color[i])),
            density: mix(self.density, other.density),
            height: mix(self.height, other.height),
            height_falloff: mix(self.height_falloff, other.height_falloff),
            sun_scatter: mix(self.sun_scatter, other.sun_scatter),
            sky_affect: mix(self.sky_affect, other.sky_affect),
            light_shafts: mix(self.light_shafts, other.light_shafts),
        }
    }
}

/// Uniform fog filling a box of half size `extents` around the object,
/// turned and scaled with it: mist in a valley, smoke in a room. Volumes
/// stack in front of `rusting.fog`; the renderer uses the first eight in
/// entity order.
#[derive(Component, Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct FogVolume {
    /// Linear RGB color the box fades into.
    pub color: [f32; 3],
    /// Extinction per metre inside the box. 1 hides things 3 m deep.
    pub density: f32,
    /// Half size of the box in the object's local axes, in metres.
    pub extents: [f32; 3],
}

impl Default for FogVolume {
    fn default() -> Self {
        Self {
            color: [0.6, 0.6, 0.65],
            density: 0.5,
            extents: [2.0, 2.0, 2.0],
        }
    }
}

/// Makes this object's `rusting.fog` and `rusting.color_grading` apply only
/// around it: fully while the active camera is inside the box of half size
/// `extents` around the object's position (world axes), fading out over
/// `blend` metres outside the box. Where volumes overlap, the higher
/// `priority` wins. Fog and grading on objects without a volume apply
/// everywhere else.
#[derive(Component, Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PostVolume {
    /// Half size of the box in metres, along the world axes.
    pub extents: [f32; 3],
    /// Distance outside the box, in metres, over which the volume fades
    /// out. 0 switches at the box wall.
    pub blend: f32,
    /// Higher volumes blend over lower ones where they overlap.
    pub priority: i32,
}

impl Default for PostVolume {
    fn default() -> Self {
        Self {
            extents: [5.0, 3.0, 5.0],
            blend: 1.0,
            priority: 0,
        }
    }
}

impl PostVolume {
    /// How much the volume applies to a camera at `eye`: 1 inside the box,
    /// fading to 0 at `blend` metres outside it.
    #[must_use]
    pub fn weight(&self, center: [f32; 3], eye: [f32; 3]) -> f32 {
        let outside = (0..3)
            .map(|i| {
                ((eye[i] - center[i]).abs() - self.extents[i])
                    .max(0.0)
                    .powi(2)
            })
            .sum::<f32>()
            .sqrt();
        if outside <= 0.0 {
            1.0
        } else if self.blend > 0.0 {
            (1.0 - outside / self.blend).max(0.0)
        } else {
            0.0
        }
    }
}

/// Glow around bright pixels. Light above `threshold` spreads over the
/// screen before tone mapping. The one on the entity with the lowest ID is
/// used.
#[derive(Component, Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Bloom {
    /// Share of the light above `threshold` added back as glow.
    pub intensity: f32,
    /// Linear brightness where the glow starts, with a soft knee below it.
    pub threshold: f32,
    /// 0 keeps the glow tight around bright pixels; 1 spreads it widely.
    pub spread: f32,
}

impl Default for Bloom {
    fn default() -> Self {
        Self {
            intensity: 0.5,
            threshold: 1.0,
            spread: 0.7,
        }
    }
}

/// Exposure that follows the scene's brightness, like an eye adapting. The
/// renderer measures the average brightness of the lit image every frame
/// and moves the exposure toward `key / average`, within the limits. It
/// multiplies the `ToneMapping` exposure, which stays as compensation. The
/// one on the entity with the lowest ID is used.
#[derive(Component, Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AutoExposure {
    /// Average linear brightness the image is brought to; 0.18 is mid grey.
    pub key: f32,
    /// Lowest exposure, reached in very bright scenes.
    pub min_exposure: f32,
    /// Highest exposure, reached in very dark scenes.
    pub max_exposure: f32,
    /// How fast the exposure follows a change, per second. The first
    /// frame adapts at once.
    pub speed: f32,
}

impl Default for AutoExposure {
    fn default() -> Self {
        Self {
            key: 0.18,
            min_exposure: 0.25,
            max_exposure: 4.0,
            speed: 1.5,
        }
    }
}

/// Camera lens blur: objects away from `focus_distance` blur, the more the
/// farther from it, up to `blur`, the blur radius far behind the focus as a
/// fraction of the screen height. An object at half the focus distance
/// blurs as much. The one on the entity with the lowest ID is used.
#[derive(Component, Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DepthOfField {
    /// Distance from the camera in metres that stays sharp.
    pub focus_distance: f32,
    /// Largest blur radius as a fraction of the screen height.
    pub blur: f32,
}

impl Default for DepthOfField {
    fn default() -> Self {
        Self {
            focus_distance: 10.0,
            blur: 0.01,
        }
    }
}

/// Camera motion blur: the image smears along the screen motion of each
/// pixel since the last frame, from the camera moving or turning. Objects
/// moving on their own do not blur. A jump of more than about a sixth of
/// the screen in one frame counts as a cut and does not blur. The one on
/// the entity with the lowest ID is used.
#[derive(Component, Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct MotionBlur {
    /// Fraction of the last frame's motion the smear covers, like a
    /// camera's shutter: 0.5 is a 180-degree shutter, 1 the whole frame.
    pub intensity: f32,
}

impl Default for MotionBlur {
    fn default() -> Self {
        Self { intensity: 0.5 }
    }
}

/// Shows what a camera sees on this object's mesh, like a CCTV monitor. The
/// camera's image replaces the base color and emissive maps of the object's
/// material, so a material with black base color and emissive `[1, 1, 1]`
/// is an unlit screen. Screens that share a material show the same feed;
/// give each screen its own material (a different `name` is enough). The
/// camera can stay inactive.
#[derive(Component, Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CameraScreen {
    /// Name of the camera entity to show.
    pub camera: String,
    /// Feed resolution in pixels.
    pub size: [u32; 2],
    /// Draws the feed every this many frames; 2 halves its cost. Screens
    /// with the same value take turns, so they do not all draw together.
    pub update_every: u32,
    /// Off keeps the last image without drawing; swap the material to show
    /// a dark monitor. A feed is drawn once when it first appears either way.
    pub enabled: bool,
    /// The feed's own color grading, such as scanlines and grain for a CCTV
    /// look. Without one the feed uses the scene's.
    pub grading: Option<ColorGrading>,
    /// Multiplies the scene's tone mapping exposure for this feed only, so
    /// dark rooms read on a monitor without lighting them for the player.
    /// 1 shows the feed as the player would see it.
    pub exposure: f32,
}

impl Default for CameraScreen {
    fn default() -> Self {
        Self {
            camera: String::new(),
            size: [640, 360],
            update_every: 1,
            enabled: true,
            grading: None,
            exposure: 1.0,
        }
    }
}

/// Color grading applied after tone mapping: contrast, saturation, a tint
/// for dark and for bright tones, a vignette, and film and CRT/VHS effects
/// (grain, chromatic aberration, scanlines, color bleed, a rolling noise
/// band, distortion). The defaults change nothing. The one on the entity with the lowest ID is used.
#[derive(Component, Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ColorGrading {
    /// Contrast around mid grey. 1 leaves the image as is; 1.2 deepens
    /// shadows and brightens highlights.
    pub contrast: f32,
    /// 0 is grey, 1 leaves colors as they are, above 1 makes them stronger.
    pub saturation: f32,
    /// Linear RGB multiplied into dark tones, e.g. a cool `[0.9, 0.95, 1.1]`.
    pub shadows: [f32; 3],
    /// Linear RGB multiplied into bright tones, e.g. a warm `[1.1, 1.0, 0.9]`.
    pub highlights: [f32; 3],
    /// How much the corners darken. 0 is none, 1 makes them black.
    pub vignette: f32,
    /// Film grain strength, 0 to 1. The pattern changes every fixed tick
    /// and is the same for the same tick, so captures stay repeatable.
    pub grain: f32,
    /// Splits red and blue toward the edges like a cheap lens, 0 to 1.
    pub chromatic_aberration: f32,
    /// Darkens every other pixel row like a CRT, 0 to 1. Clearest on small
    /// camera-screen feeds.
    pub scanlines: f32,
    /// Smears color sideways while keeping the brightness sharp, like VHS
    /// tape, 0 to 1.
    pub color_bleed: f32,
    /// A band of static that rolls down the image, 0 to 1.
    pub noise_band: f32,
    /// Bulges the image like a curved tube and wobbles its rows over time,
    /// 0 to 1. Corners pushed off the image turn black.
    pub distortion: f32,
    /// Rain on the camera lens, 0 to 1: round drops that appear, bend the
    /// image behind them like small lenses and fade after a few seconds.
    /// Higher values cover more of the lens. Follows the fixed tick.
    pub lens_droplets: f32,
}

impl ColorGrading {
    /// The default, which changes nothing; usable in constants.
    pub const DEFAULT: Self = Self {
        contrast: 1.0,
        saturation: 1.0,
        shadows: [1.0; 3],
        highlights: [1.0; 3],
        vignette: 0.0,
        grain: 0.0,
        chromatic_aberration: 0.0,
        scanlines: 0.0,
        color_bleed: 0.0,
        noise_band: 0.0,
        distortion: 0.0,
        lens_droplets: 0.0,
    };

    /// Blends toward `other` by `t` (0 keeps `self`, 1 gives `other`).
    #[must_use]
    pub fn lerp(self, other: Self, t: f32) -> Self {
        let mix = |a: f32, b: f32| a + (b - a) * t;
        let mix3 =
            |a: [f32; 3], b: [f32; 3]| std::array::from_fn(|i| mix(a[i], b[i]));
        Self {
            contrast: mix(self.contrast, other.contrast),
            saturation: mix(self.saturation, other.saturation),
            shadows: mix3(self.shadows, other.shadows),
            highlights: mix3(self.highlights, other.highlights),
            vignette: mix(self.vignette, other.vignette),
            grain: mix(self.grain, other.grain),
            chromatic_aberration: mix(
                self.chromatic_aberration,
                other.chromatic_aberration,
            ),
            scanlines: mix(self.scanlines, other.scanlines),
            color_bleed: mix(self.color_bleed, other.color_bleed),
            noise_band: mix(self.noise_band, other.noise_band),
            distortion: mix(self.distortion, other.distortion),
            lens_droplets: mix(self.lens_droplets, other.lens_droplets),
        }
    }
}

impl Default for ColorGrading {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// Screen-space ambient occlusion: darkens ambient and sky light in creases
/// and corners where nearby geometry blocks it. Off on the Eco quality
/// profile. The one on the entity with the lowest ID is used.
#[derive(Component, Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AmbientOcclusion {
    /// World-space distance, in metres, that is searched for occluders.
    pub radius: f32,
    /// Exponent on the occlusion. Above 1 darkens creases more.
    pub intensity: f32,
    /// Strength of light bounced off nearby surfaces within `radius`
    /// (screen-space indirect light): a red wall tints the floor beside
    /// it. 0 turns it off; it runs on the High quality profile only.
    pub indirect: f32,
}

impl Default for AmbientOcclusion {
    fn default() -> Self {
        Self {
            radius: 1.0,
            intensity: 1.0,
            indirect: 0.0,
        }
    }
}

/// Projects an image through the [`SpotLight`](super::SpotLight) on the
/// same entity, like a slide in a projector: the light's color is
/// multiplied by the image, which fills the outer cone with its top
/// toward the light's up (+Y). Black parts block the light.
// ponytail: four cookies per frame (the first four cookie spot lights in
// entity order); move to a texture array if scenes need more.
#[derive(
    Component, Clone, Debug, Default, PartialEq, Serialize, Deserialize,
)]
#[serde(default)]
pub struct LightCookie {
    /// Image path relative to the project's `assets` folder.
    pub texture: std::path::PathBuf,
    /// The loaded `texture`, set by the engine.
    #[serde(skip)]
    pub handle: Option<crate::assets::Handle<crate::assets::TextureAsset>>,
}

/// Paints an image onto the surfaces inside a box of half size `extents`
/// around the object, projected along its forward axis (-Z) with the
/// image's top along +Y: a poster on a wall, a crack, a puddle stain.
/// Surfaces facing away from the projection get none of it.
// ponytail: sixteen decals and four distinct images per frame (the first
// in entity order); move to a texture array and clustered decals if
// scenes need more.
#[derive(Component, Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Decal {
    /// Image path relative to the project's `assets` folder; empty paints
    /// plain `color`.
    pub texture: std::path::PathBuf,
    /// Linear RGBA multiplied with the image; alpha scales its coverage.
    pub color: [f32; 4],
    /// Half size of the box in the object's local axes, in metres.
    pub extents: [f32; 3],
    /// Roughness painted with the image, by its coverage: low for a wet
    /// stain or a puddle. `None` keeps the surface's.
    pub roughness: Option<f32>,
    /// When its parent object breaks ([`super::Fracture`]): true moves the
    /// decal onto the piece nearest it, so it stays on that piece's surface;
    /// false removes it with the object.
    pub survive_fracture: bool,
    /// The loaded `texture`, set by the engine.
    #[serde(skip)]
    pub handle: Option<crate::assets::Handle<crate::assets::TextureAsset>>,
}

impl Default for Decal {
    fn default() -> Self {
        Self {
            texture: std::path::PathBuf::new(),
            color: [1.0; 4],
            extents: [0.5, 0.5, 0.25],
            roughness: None,
            survive_fracture: true,
            handle: None,
        }
    }
}

/// Color lookup table applied after tone mapping and [`ColorGrading`]: an
/// image strip of N squares of N x N pixels side by side (for example
/// 256 x 16 or 1024 x 32). Red runs across each square, green down it, and
/// blue from square to square; an identity strip changes nothing. Grade a
/// screenshot with an identity strip pasted in, in any image editor, then
/// save the strip. The one on the entity with the lowest ID is used.
#[derive(Component, Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ColorLut {
    /// Image path relative to the project's `assets` folder.
    pub texture: std::path::PathBuf,
    /// 0 leaves the image as is, 1 applies the table fully.
    pub intensity: f32,
    /// The loaded `texture`, set by the engine.
    #[serde(skip)]
    pub handle: Option<crate::assets::Handle<crate::assets::TextureAsset>>,
}

/// A `#rusting post 1` shader run on every pixel of the finished image,
/// after tone mapping, [`ColorGrading`] and [`ColorLut`]. See
/// `docs/surface-shaders.md`. A shader that fails to compile leaves the
/// image as is; the error is in `SceneRenderer::post_shader_error`. The one
/// on the entity with the lowest ID is used.
#[derive(
    Component, Clone, Debug, Default, PartialEq, Serialize, Deserialize,
)]
#[serde(default)]
pub struct PostShader {
    /// GLSL source starting with `#rusting post 1`.
    pub source: String,
}

/// A `#rusting sky 1` shader that colors the background wherever no
/// surface was drawn, starting from the [`ProceduralSky`] or background
/// [`EnvironmentMap`] if there is one. Fog still covers it. See
/// `docs/surface-shaders.md`. A shader that fails to compile leaves the
/// built-in background; the error is in `SceneRenderer::sky_shader_error`.
/// The one on the entity with the lowest ID is used.
#[derive(
    Component, Clone, Debug, Default, PartialEq, Serialize, Deserialize,
)]
#[serde(default)]
pub struct SkyShader {
    /// GLSL source starting with `#rusting sky 1`.
    pub source: String,
}

/// A `#rusting fog 1` shader that changes the fog over every surface and
/// the background, starting from the [`Fog`] and [`FogVolume`]s if there
/// are any. See `docs/surface-shaders.md`. A shader that fails to compile
/// leaves the built-in fog; the error is in `SceneRenderer::sky_shader_error`
/// and `SceneRenderer::surface_shader_errors`. The one on the entity with
/// the lowest ID is used.
#[derive(
    Component, Clone, Debug, Default, PartialEq, Serialize, Deserialize,
)]
#[serde(default)]
pub struct FogShader {
    /// GLSL source starting with `#rusting fog 1`.
    pub source: String,
}

impl Default for ColorLut {
    fn default() -> Self {
        Self {
            texture: std::path::PathBuf::new(),
            intensity: 1.0,
            handle: None,
        }
    }
}

/// Loads the image of each added or changed [`EnvironmentMap`],
/// [`LightCookie`], [`Decal`], [`ColorLut`] and [`super::Sprite`]. An image whose load failed (file missing or not written
/// yet) is tried again about every two seconds.
pub(super) fn load_environment_maps(
    assets: Option<ResMut<crate::assets::AssetServer>>,
    mut maps: Query<&mut EnvironmentMap>,
    mut cookies: Query<&mut LightCookie>,
    mut luts: Query<&mut ColorLut>,
    mut decals: Query<&mut Decal>,
    mut sprites: Query<&mut super::Sprite>,
    mut frame: bevy_ecs::prelude::Local<u32>,
) {
    let Some(mut assets) = assets else {
        return;
    };
    *frame = frame.wrapping_add(1);
    let retry_frame = (*frame).is_multiple_of(120);
    let mut load = |changed: bool,
                    texture: &std::path::Path,
                    handle: &mut Option<
        crate::assets::Handle<crate::assets::TextureAsset>,
    >,
                    what: &str| {
        let retry =
            handle.is_none() && !texture.as_os_str().is_empty() && retry_frame;
        if !changed && !retry {
            return;
        }
        *handle = (!texture.as_os_str().is_empty())
            .then(|| {
                assets
                    .textures
                    .handle_for_path(texture)
                    .map(Ok)
                    .unwrap_or_else(|| assets.load_texture(texture))
                    .map_err(|error| eprintln!("{what}: {error}"))
                    .ok()
            })
            .flatten();
    };
    for mut map in &mut maps {
        let changed =
            bevy_ecs::change_detection::DetectChanges::is_changed(&map);
        let map = map.bypass_change_detection();
        load(changed, &map.texture, &mut map.handle, "environment map");
    }
    for mut cookie in &mut cookies {
        let changed =
            bevy_ecs::change_detection::DetectChanges::is_changed(&cookie);
        let cookie = cookie.bypass_change_detection();
        load(changed, &cookie.texture, &mut cookie.handle, "light cookie");
    }
    for mut lut in &mut luts {
        let changed =
            bevy_ecs::change_detection::DetectChanges::is_changed(&lut);
        let lut = lut.bypass_change_detection();
        load(changed, &lut.texture, &mut lut.handle, "color lut");
    }
    for mut decal in &mut decals {
        let changed =
            bevy_ecs::change_detection::DetectChanges::is_changed(&decal);
        let decal = decal.bypass_change_detection();
        load(changed, &decal.texture, &mut decal.handle, "decal");
    }
    for mut sprite in &mut sprites {
        let changed =
            bevy_ecs::change_detection::DetectChanges::is_changed(&sprite);
        let sprite = sprite.bypass_change_detection();
        load(changed, &sprite.texture, &mut sprite.handle, "sprite");
        load(
            changed,
            &sprite.normal_map,
            &mut sprite.normal_handle,
            "sprite normal map",
        );
    }
    // Normal maps are data, not color, whatever their file says.
    // ponytail: a hot reload of the file turns it back to sRGB until the
    // sprite changes; flag linear at decode if that bites.
    for sprite in &sprites {
        let Some(handle) = sprite.normal_handle else {
            continue;
        };
        if assets.textures.get(handle).is_some_and(|texture| {
            texture.color_space == crate::assets::TextureColorSpace::Srgb
        }) {
            assets.textures.get_mut(handle).unwrap().color_space =
                crate::assets::TextureColorSpace::Linear;
        }
    }
}

/// Sent the frame a HUD button is clicked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HudButtonPressed {
    pub entity: Entity,
}

/// Per fixed step: advances every tween and writes its property.
pub(super) fn advance_tweens(
    time: Res<FrameTime>,
    mut tweens: Query<(&mut Tween, &mut Transform)>,
) {
    let dt = time.fixed_delta.as_secs_f32();
    for (mut tween, mut transform) in &mut tweens {
        if tween.finished() {
            continue;
        }
        tween.elapsed += dt;
        let value = tween.sample();
        match tween.property {
            TweenProperty::Position => transform.position = value,
            TweenProperty::Rotation => transform.rotation = value,
            TweenProperty::Scale => transform.scale = value,
        }
    }
}

/// The material name a contact touches on `other`: a heightfield cell's
/// or tagged triangle's own surface where it has one, else the collider's
/// [`super::PhysicsMaterial`].
pub(super) fn surface_name<'a>(
    surface: Option<u8>,
    field: Option<&'a super::Heightfield>,
    tagged: Option<&'a super::MeshSurfaces>,
    material: Option<&'a super::PhysicsMaterial>,
) -> Option<&'a str> {
    surface
        .and_then(|index| {
            let surfaces = field
                .map(|field| &field.surfaces)
                .or(tagged.map(|tagged| &tagged.surfaces))?;
            surfaces.get(usize::from(index))
        })
        .map(|surface| surface.name.as_str())
        .or_else(|| material.map(|material| material.name.as_str()))
}

/// Per fixed step, after physics: marks cues and emitters whose body
/// started touching something.
pub(super) fn trigger_on_contact(
    physics: Res<PhysicsWorld>,
    materials: Query<&super::PhysicsMaterial>,
    fields: Query<&super::Heightfield>,
    tagged: Query<&super::MeshSurfaces>,
    mut cues: Query<(Entity, &mut SoundCue)>,
    mut emitters: Query<(Entity, &mut BurstEmitter)>,
) {
    if cues.is_empty() && emitters.is_empty() {
        return;
    }
    // Fastest closing speed per touching body, and per body and the
    // material of what it touches.
    let mut touching = BTreeMap::<Entity, f32>::new();
    let mut by_material = BTreeMap::<(Entity, &str), f32>::new();
    for contact in physics.contacts() {
        for (body, other) in [(contact.a, contact.b), (contact.b, contact.a)] {
            let speed = touching.entry(body).or_default();
            *speed = speed.max(contact.speed);
            let name = surface_name(
                contact.surface,
                fields.get(other).ok(),
                tagged.get(other).ok(),
                materials.get(other).ok(),
            );
            if let Some(name) = name {
                let speed = by_material.entry((body, name)).or_default();
                *speed = speed.max(contact.speed);
            }
        }
    }
    for (entity, mut cue) in &mut cues {
        let hit = if cue.with_material.is_empty() {
            touching.get(&entity)
        } else {
            by_material.get(&(entity, cue.with_material.as_str()))
        }
        .filter(|_| cue.on_collision)
        .copied();
        if hit.is_some() && !cue.touching {
            cue.triggered = true;
            cue.hit_speed = hit;
        }
        cue.touching = hit.is_some();
    }
    for (entity, mut emitter) in &mut emitters {
        let now = emitter.on_collision && touching.contains_key(&entity);
        emitter.triggered |= now && !emitter.touching;
        emitter.touching = now;
    }
}

/// Per frame: triggers sound cues named by this frame's GPU physics events.
/// The cue fires on the next fixed step.
pub(super) fn trigger_on_gpu_events(
    events: Option<Res<EventQueue<super::GpuPhysicsEvent>>>,
    registry: Option<Res<super::GpuEventRegistry>>,
    mut cues: Query<&mut SoundCue>,
) {
    let (Some(events), Some(registry)) = (events, registry) else {
        return;
    };
    for event in events.iter() {
        let Ok(mut cue) = cues.get_mut(event.entity) else {
            continue;
        };
        if !cue.on_gpu_event.is_empty()
            && registry.id(&cue.on_gpu_event) == Some(event.event_id)
        {
            cue.trigger();
        }
    }
}

/// Bodies that collect pickups.
type IsPlayer = Or<(
    With<super::PlatformerController>,
    With<super::PlayerController>,
)>;

type PickupParts = (
    Entity,
    &'static mut Pickup,
    Option<&'static SceneId>,
    Option<&'static mut SoundCue>,
    Option<&'static mut BurstEmitter>,
);

/// Per fixed step, after physics: collects pickups a player touches, in
/// `SceneId` order.
pub(super) fn collect_pickups(
    mut commands: Commands,
    physics: Res<PhysicsWorld>,
    players: Query<(), IsPlayer>,
    mut pickups: Query<PickupParts>,
    mut counters: Query<(&mut Counter, Option<&SceneId>)>,
) {
    let touched: BTreeSet<Entity> = physics
        .contacts()
        .iter()
        .flat_map(|contact| [(contact.a, contact.b), (contact.b, contact.a)])
        .filter(|(_, other)| players.contains(*other))
        .map(|(entity, _)| entity)
        .collect();
    if touched.is_empty() {
        return;
    }
    let mut ready: Vec<_> = pickups
        .iter_mut()
        .filter(|(entity, pickup, ..)| {
            !pickup.collected && touched.contains(entity)
        })
        .collect();
    ready.sort_by_key(|(entity, _, id, ..)| (id.map(|id| id.0), *entity));
    for (entity, mut pickup, _, cue, emitter) in ready {
        if let Some(required) = &pickup.requires {
            let done = find_counter(counters.iter(), required)
                .is_some_and(|counter| counter.complete());
            if !done {
                continue;
            }
        }
        pickup.collected = true;
        if let Some(mut counter) =
            find_counter(counters.iter_mut(), &pickup.counter)
        {
            counter.value += pickup.value;
        }
        commands
            .entity(entity)
            .insert(super::Visibility { visible: false });
        if let Some(mut cue) = cue {
            cue.trigger();
        }
        if let Some(mut emitter) = emitter {
            emitter.trigger();
        }
    }
}

/// Per fixed step: sends a [`SoundEvent`] for every triggered cue.
pub(super) fn fire_sound_cues(
    mut events: ResMut<EventQueue<SoundEvent>>,
    mut cues: Query<(Entity, &mut SoundCue)>,
) {
    for (entity, mut cue) in &mut cues {
        if std::mem::take(&mut cue.triggered) {
            let mut volume = cue.volume;
            match cue.hit_speed.take() {
                Some(speed) if cue.full_volume_speed > 0.0 => {
                    volume *= (speed / cue.full_volume_speed).min(1.0);
                }
                _ => {}
            }
            events.send(SoundEvent {
                entity,
                clip: cue.clip.clone(),
                volume,
                caption: cue.caption.clone(),
            });
        }
    }
}

type EmitterParts = (
    &'static mut BurstEmitter,
    &'static Transform,
    Option<&'static GlobalTransform>,
    Option<&'static SceneId>,
    Option<&'static MeshRenderer>,
    Entity,
);

/// Per fixed step: spawns particles for every triggered emitter and for
/// every emitter with a `rate`.
pub(super) fn fire_bursts(
    mut commands: Commands,
    time: Res<FrameTime>,
    seed: Res<RandomSeed>,
    mut emitters: Query<EmitterParts>,
) {
    let dt = time.fixed_delta.as_secs_f32();
    for (mut emitter, transform, global, id, mesh, entity) in &mut emitters {
        let mut count = 0;
        if std::mem::take(&mut emitter.triggered) {
            count += u64::from(emitter.count);
        }
        if emitter.rate > 0.0 {
            emitter.pending += emitter.rate * dt;
            let whole = emitter.pending.floor();
            emitter.pending -= whole;
            count += whole as u64;
        }
        if count == 0 {
            continue;
        }
        let origin = global.map_or(transform.position, |global| {
            let column = global.matrix[3];
            [column[0], column[1], column[2]]
        });
        // Authored emitters keep their stream across loads; others fall
        // back to the entity, which repeats for the same spawn order.
        let key = id.map_or(entity.to_bits(), |id| {
            let bits = id.0.as_u128();
            (bits as u64) ^ ((bits >> 64) as u64)
        });
        let stream = RandomSeed::stream("bursts", key);
        let area = RandomSeed::stream("burst_area", key);
        let mut scale =
            transform.scale.map(|axis| axis * emitter.particle_scale);
        scale[1] *= emitter.stretch;
        for index in 0..count {
            let up = seed.unit(time.fixed_tick, stream ^ (index << 1));
            let turn = seed.unit(time.fixed_tick, stream ^ (index << 1 | 1));
            let side = (1.0 - up * up).sqrt();
            let (sin, cos) = (turn * TAU).sin_cos();
            let velocity =
                [side * cos, up, side * sin].map(|axis| axis * emitter.speed);
            let particle = BurstParticle {
                velocity,
                gravity: emitter.gravity,
                remaining: emitter.lifetime,
                lifetime: emitter.lifetime,
                scale,
            };
            let position = std::array::from_fn(|axis| {
                let unit = seed
                    .unit(time.fixed_tick, area ^ (index * 3 + axis as u64));
                origin[axis] + (unit * 2.0 - 1.0) * emitter.area[axis]
            });
            let transform = Transform {
                position,
                scale,
                ..Transform::default()
            };
            let mut spawned = commands.spawn((transform, particle));
            if let Some(mesh) = mesh {
                spawned.insert(*mesh);
            }
        }
    }
}

/// Per fixed step: moves, shrinks, and removes burst particles.
pub(super) fn update_burst_particles(
    mut commands: Commands,
    time: Res<FrameTime>,
    mut particles: Query<(Entity, &mut BurstParticle, &mut Transform)>,
) {
    let dt = time.fixed_delta.as_secs_f32();
    for (entity, mut particle, mut transform) in &mut particles {
        particle.remaining -= dt;
        if particle.remaining <= 0.0 {
            commands.entity(entity).despawn();
            continue;
        }
        particle.velocity[1] -= particle.gravity * dt;
        for axis in 0..3 {
            transform.position[axis] += particle.velocity[axis] * dt;
        }
        let left = particle.remaining / particle.lifetime;
        transform.scale = particle.scale.map(|axis| axis * left);
    }
}

/// Frame rate and frame time the window runner shows in the top-right
/// corner while F3 or `RUSTING_PERF` turns it on. Absent, nothing is drawn.
#[derive(Resource, Clone, Debug, Default, PartialEq)]
pub struct PerfOverlay(pub String);

/// The [`PerfOverlay`] text, top right over a dark band.
#[cfg(feature = "ui")]
fn draw_perf_overlay(context: &egui::Context, overlay: Option<&PerfOverlay>) {
    let Some(PerfOverlay(text)) = overlay else {
        return;
    };
    egui::Area::new(egui::Id::new("rusting.perf_overlay"))
        .sense(egui::Sense::hover())
        .anchor(egui::Align2::RIGHT_TOP, egui::vec2(-8.0, 8.0))
        .fade_in(false)
        .order(egui::Order::Foreground)
        .show(context, |ui| {
            egui::Frame::new()
                .fill(egui::Color32::from_black_alpha(170))
                .inner_margin(egui::Margin::symmetric(8, 4))
                .corner_radius(4.0)
                .show(ui, |ui| {
                    ui.add(
                        egui::Label::new(
                            egui::RichText::new(text)
                                .monospace()
                                .color(egui::Color32::WHITE),
                        )
                        .selectable(false),
                    );
                });
        });
}

/// Caption lines of the sounds playing, bottom center over a dark band.
#[cfg(feature = "ui")]
fn draw_captions(
    context: &egui::Context,
    audio: Option<&super::AudioQueue>,
    settings: &super::CaptionSettings,
) {
    let lines = audio.map(super::AudioQueue::captions).unwrap_or_default();
    if !settings.enabled || lines.is_empty() {
        return;
    }
    let screen = context.screen_rect();
    // Hover only, like the HUD, so captions never take focus.
    egui::Area::new(egui::Id::new("rusting.captions"))
        .sense(egui::Sense::hover())
        .anchor(egui::Align2::CENTER_BOTTOM, egui::vec2(0.0, -48.0))
        .fade_in(false)
        .order(egui::Order::Foreground)
        .show(context, |ui| {
            ui.set_max_width(screen.width() * 0.8);
            egui::Frame::new()
                .fill(egui::Color32::from_black_alpha(170))
                .inner_margin(egui::Margin::symmetric(12, 6))
                .corner_radius(4.0)
                .show(ui, |ui| {
                    for line in lines {
                        ui.add(
                            egui::Label::new(
                                egui::RichText::new(line)
                                    .size(settings.size)
                                    .color(egui::Color32::WHITE),
                            )
                            .selectable(false),
                        );
                    }
                });
        });
}

/// The font files HUD elements asked for, loaded into egui.
#[cfg(feature = "ui")]
#[derive(Default)]
pub(super) struct HudFonts {
    /// Every file asked for, false when it did not load.
    tried: BTreeMap<String, bool>,
    definitions: egui::FontDefinitions,
}

/// The egui family for `font` under `assets`, loading the file on first
/// use. `None` when the file is not a readable font, and on the frame it
/// loads: egui binds new fonts at the start of the next pass, and naming an
/// unbound family panics.
#[cfg(feature = "ui")]
fn hud_font(
    context: &egui::Context,
    fonts: &mut HudFonts,
    assets: &std::path::Path,
    font: &str,
) -> Option<egui::FontFamily> {
    let family = egui::FontFamily::Name(font.into());
    if let Some(&loaded) = fonts.tried.get(font) {
        return (loaded && context.fonts(|f| f.families().contains(&family)))
            .then_some(family);
    }
    let path = assets.join(font);
    let bytes = crate::assets::read_file(&path).ok().filter(|bytes| {
        // egui panics on a font it cannot parse; this one checks safely.
        crate::text_texture::text_texture_in_font(
            "Aa",
            crate::text_texture::TextStyle::default(),
            bytes,
        )
        .is_some()
    });
    fonts.tried.insert(font.to_owned(), bytes.is_some());
    let Some(bytes) = bytes else {
        eprintln!(
            "warning: rusting.hud: {} is not a readable TTF or OTF font; \
             the text uses the built-in font.",
            path.display()
        );
        return None;
    };
    let definitions = &mut fonts.definitions;
    definitions.font_data.insert(
        font.to_owned(),
        std::sync::Arc::new(egui::FontData::from_owned(bytes)),
    );
    let fallback = definitions.families[&egui::FontFamily::Proportional].iter();
    let list = std::iter::once(font.to_owned()).chain(fallback.cloned());
    definitions.families.insert(family, list.collect());
    context.set_fonts(definitions.clone());
    None
}

/// Per frame: draws HUD elements in reading order and reports clicks.
#[cfg(feature = "ui")]
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
pub(super) fn draw_hud(
    ui: Res<super::RuntimeUi>,
    mut pressed: ResMut<EventQueue<HudButtonPressed>>,
    elements: Query<(Entity, &HudElement, Option<&SceneId>)>,
    counters: Query<(&Counter, Option<&SceneId>)>,
    visibility: Query<&super::Visibility>,
    parents: Query<&super::Parent>,
    cameras: Query<(
        Entity,
        &super::Name,
        &super::Camera,
        Option<&GlobalTransform>,
    )>,
    audio: Option<Res<super::AudioQueue>>,
    caption_settings: Option<Res<super::CaptionSettings>>,
    perf: Option<Res<PerfOverlay>>,
    (translations, dialogues, states, text_scale, actions, rebind, named): (
        Option<Res<Translations>>,
        Query<(&super::Name, &Dialogue)>,
        Query<(&super::Name, &ObjectState)>,
        Option<Res<TextScale>>,
        Query<&super::InputAction>,
        Option<Res<RebindWait>>,
        Query<(&super::Name, &GlobalTransform)>,
    ),
    (folder, mut fonts): (
        Option<Res<crate::project_runner::ProjectFolder>>,
        bevy_ecs::system::Local<HudFonts>,
    ),
) {
    let assets = folder.map_or_else(
        || std::path::PathBuf::from("assets"),
        |folder| folder.0.join("assets"),
    );
    let scale = text_scale.map_or(1.0, |scale| scale.0);
    let captions = caption_settings.as_deref().cloned().unwrap_or_default();
    draw_perf_overlay(ui.context(), perf.as_deref());
    draw_captions(
        ui.context(),
        audio.as_deref(),
        &super::CaptionSettings {
            size: captions.size * scale,
            ..captions
        },
    );
    // Hidden like a mesh: by itself or by any parent.
    let visible = |entity: Entity| {
        std::iter::successors(Some(entity), |&entity| {
            parents.get(entity).ok().map(|parent| parent.0)
        })
        .take(1024)
        .all(|entity| visibility.get(entity).map_or(true, |v| v.visible))
    };
    let mut elements: Vec<_> = elements.iter().collect();
    elements.sort_by_key(|(entity, _, id)| (id.map(|id| id.0), *entity));
    // Each shown element with its area, alignment and anchored point.
    let mut placed = Vec::new();
    for (entity, element, _) in elements {
        if !visible(entity) {
            continue;
        }
        if let Some(required) = &element.requires {
            if !find_counter(counters.iter(), required)
                .is_some_and(|counter| counter.complete())
            {
                continue;
            }
        }
        let screen = ui.context().screen_rect();
        // The named camera, else the one the window shows, as
        // `audio::active_camera` picks it.
        let camera = match &element.camera {
            None => cameras
                .iter()
                .filter(|(.., camera, _)| camera.active)
                .max_by_key(|(entity, _, camera, _)| {
                    (
                        camera.viewport.is_none(),
                        camera.priority,
                        std::cmp::Reverse(entity.to_bits()),
                    )
                }),
            Some(wanted) => {
                let found = cameras.iter().find(|(_, name, camera, _)| {
                    name.0 == *wanted && camera.active
                });
                if found.is_none() {
                    continue;
                }
                found
            }
        };
        let area = match camera.and_then(|(.., camera, _)| camera.viewport) {
            Some([x, y, w, h]) if element.camera.is_some() => {
                egui::Rect::from_min_size(
                    screen.min + egui::vec2(x, y) * screen.size(),
                    egui::vec2(w, h) * screen.size(),
                )
            }
            _ => screen,
        };
        let (align, inward) = match element.anchor {
            HudAnchor::TopLeft => (egui::Align2::LEFT_TOP, [1.0, 1.0]),
            HudAnchor::Top => (egui::Align2::CENTER_TOP, [1.0, 1.0]),
            HudAnchor::TopRight => (egui::Align2::RIGHT_TOP, [-1.0, 1.0]),
            HudAnchor::Center => (egui::Align2::CENTER_CENTER, [1.0, 1.0]),
            HudAnchor::BottomLeft => (egui::Align2::LEFT_BOTTOM, [1.0, -1.0]),
            HudAnchor::Bottom => (egui::Align2::CENTER_BOTTOM, [1.0, -1.0]),
            HudAnchor::BottomRight => {
                (egui::Align2::RIGHT_BOTTOM, [-1.0, -1.0])
            }
        };
        let offset = egui::vec2(
            element.offset[0] * inward[0],
            element.offset[1] * inward[1],
        );
        let point = match &element.follow {
            None => align.pos_in_rect(&area) + offset,
            Some(target) => {
                let Some(on_screen) = camera.and_then(|(.., camera, view)| {
                    let (_, at) =
                        named.iter().find(|(name, _)| name.0 == *target)?;
                    let at = at.matrix[3];
                    let point = [0, 1, 2]
                        .map(|axis| at[axis] + element.follow_offset[axis]);
                    super::picking::project_point(
                        point.into(),
                        *camera,
                        *view?,
                        area.min.into(),
                        area.size().into(),
                    )
                }) else {
                    continue;
                };
                egui::pos2(on_screen[0], on_screen[1])
                    + egui::vec2(element.offset[0], element.offset[1])
            }
        };
        placed.push((entity, element, align, point));
    }
    // Drawn in reading order, top to bottom then left to right, because
    // egui moves keyboard and gamepad focus in drawing order.
    placed.sort_by(|a, b| {
        (a.3.y.total_cmp(&b.3.y)).then(a.3.x.total_cmp(&b.3.x))
    });
    for (entity, element, align, point) in placed {
        let [r, g, b, a] =
            element.color.map(|c| (c.clamp(0.0, 1.0) * 255.0) as u8);
        let filled = fill_placeholders(&element.text, |placeholder| {
            dialogue_placeholder(
                placeholder,
                dialogues.iter(),
                translations.as_deref(),
            )
            .or_else(|| {
                state_placeholder(
                    placeholder,
                    states.iter(),
                    translations.as_deref(),
                )
            })
            .or_else(|| {
                binding_placeholder(
                    placeholder,
                    actions.iter(),
                    rebind.as_ref().and_then(|wait| wait.0.as_deref()),
                    translations.as_deref(),
                )
            })
        });
        let filled =
            hud_text(&filled, counters.iter(), translations.as_deref());
        // An empty element draws nothing, so a dialogue's unused choice
        // buttons hide.
        if filled.trim().is_empty() {
            continue;
        }
        let mut text = egui::RichText::new(filled)
            .size(element.font_size * scale)
            .color(egui::Color32::from_rgba_unmultiplied(r, g, b, a));
        if let Some(font) = &element.font {
            if let Some(family) =
                hud_font(ui.context(), &mut fonts, &assets, font)
            {
                text = text.family(family);
            }
        }
        // Measure this frame's text: an anchored egui area places itself by
        // last frame's size, so a value that grew ran past the edge.
        let context = ui.context();
        let galley = egui::WidgetText::from(text.clone()).into_galley_impl(
            context,
            &context.style(),
            egui::text::TextWrapping::no_max_width(),
            egui::FontSelection::Default,
            egui::Align::LEFT,
        );
        let padding = if element.button {
            2.0 * context.style().spacing.button_padding
        } else {
            egui::Vec2::ZERO
        };
        let position = align.anchor_size(point, galley.size() + padding).min;
        let id = egui::Id::new(("rusting.hud", entity));
        if !element.button {
            // Painted straight onto a layer: a new egui area stays hidden
            // for its first frame, which a tick-0 capture is. Like a Godot
            // label, plain text lets the pointer through.
            context
                .layer_painter(egui::LayerId::new(egui::Order::Middle, id))
                .galley(position, galley, egui::Color32::PLACEHOLDER);
            continue;
        }
        // Hover only: an area that senses clicks takes keyboard and gamepad
        // focus ahead of its button. No fade-in: an element shown for a few
        // ticks would stay faint.
        egui::Area::new(id)
            .sense(egui::Sense::hover())
            .fixed_pos(position)
            .fade_in(false)
            .show(context, |ui| {
                if ui.button(text).clicked() {
                    pressed.send(HudButtonPressed { entity });
                }
            });
    }
}

#[cfg(all(test, feature = "ui"))]
mod tests {
    use super::*;

    #[test]
    fn hud_fonts_bind_on_the_pass_after_they_load() {
        let assets = std::env::temp_dir()
            .join(format!("rusting-hud-font-{}", std::process::id()));
        std::fs::create_dir_all(assets.join("fonts")).unwrap();
        let pixel = crate::fontgen::synth("pixel").unwrap();
        std::fs::write(assets.join("fonts/pixel.ttf"), pixel).unwrap();
        std::fs::write(assets.join("fonts/bad.ttf"), "no").unwrap();
        let context = egui::Context::default();
        let mut fonts = HudFonts::default();
        let mut pass = |font: &str| {
            let mut family = None;
            let _ = context.run(egui::RawInput::default(), |context| {
                family = hud_font(context, &mut fonts, &assets, font);
            });
            family
        };
        // Loaded on the first pass, usable from the second.
        assert_eq!(pass("fonts/pixel.ttf"), None);
        let family = pass("fonts/pixel.ttf").expect("bound after one pass");
        assert_eq!(family, egui::FontFamily::Name("fonts/pixel.ttf".into()));
        for font in ["fonts/bad.ttf", "fonts/missing.ttf"] {
            assert_eq!(pass(font), None);
            assert_eq!(pass(font), None, "{font} never binds");
        }
        // The pixel font draws text differently from the built-in one,
        // and the fallback keeps characters it lacks.
        let width = |family: egui::FontFamily, text: &str| {
            context.fonts(|fonts| {
                fonts
                    .layout_no_wrap(
                        text.into(),
                        egui::FontId::new(16.0, family),
                        egui::Color32::WHITE,
                    )
                    .size()
                    .x
            })
        };
        let proportional = egui::FontFamily::Proportional;
        assert_ne!(
            width(family.clone(), "Score"),
            width(proportional, "Score")
        );
        assert!(width(family, "é") > 0.0);
        std::fs::remove_dir_all(assets).unwrap();
    }
}
