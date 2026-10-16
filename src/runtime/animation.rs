//! Keyframe animation: `rusting.animation` holds named clips of keyed
//! tracks. Each fixed tick the playing clip is sampled and written to the
//! entity or a named descendant: transform, material color, emissive,
//! visibility, or any numeric field of a registered component. Time comes
//! from the fixed tick only, so playback is deterministic.

use std::collections::BTreeMap;

use bevy_ecs::entity::Entity;
use bevy_ecs::prelude::{Component, Resource, World};
use serde::{Deserialize, Serialize};

use super::{
    Children, FrameTime, MeshRenderer, Name, SceneId, TweenRepeat, Visibility,
};
use crate::assets::{AssetServer, Handle, MaterialAsset};
use crate::Transform;

/// What a track writes. Values are `[x, y, z]` for transforms (rotation in
/// radians), linear RGBA for `Color`, linear RGB for `Emissive`, one value
/// for `Visible` (shown at 0.5 and above) and for `Field`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum AnimationProperty {
    #[default]
    Position,
    Rotation,
    Scale,
    Color,
    Emissive,
    Visible,
    /// Rotation as a quaternion `[x, y, z, w]`, blended and normalized, then
    /// written as `Rotation`. Imported glTF clips use it; it does not flip
    /// at ±180° like Euler keys.
    Orientation,
    /// A numeric field of a registered scene component, by JSON pointer:
    /// `{ "Field": { "component": "rusting.point_light", "path": "/intensity" } }`.
    /// Several values write an array.
    Field {
        component: String,
        path: String,
    },
}

impl AnimationProperty {
    /// Values per key.
    #[must_use]
    pub fn width(&self) -> usize {
        match self {
            Self::Color | Self::Orientation => 4,
            Self::Position | Self::Rotation | Self::Scale | Self::Emissive => 3,
            Self::Visible | Self::Field { .. } => 1,
        }
    }
}

/// How a track moves between keys.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize,
)]
pub enum Interpolation {
    /// Holds each key until the next.
    Step,
    #[default]
    Linear,
    /// Catmull-Rom through the keys.
    Smooth,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Keyframe {
    pub time: f32,
    pub value: Vec<f32>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AnimationTrack {
    /// Empty for the animated entity itself, else a path of child names
    /// such as `Arm/Hand`.
    pub target: String,
    pub property: AnimationProperty,
    pub interpolation: Interpolation,
    /// Sorted by time; the player sorts them if they are not.
    pub keys: Vec<Keyframe>,
}

/// A named moment in a clip, sent to game code as an [`AnimationEvent`].
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AnimationMarker {
    pub time: f32,
    pub name: String,
}

/// One clip of a blend space and its place on the parameter axis.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct BlendPoint {
    pub clip: String,
    pub at: f32,
    /// Place on the second axis of a 2D blend space.
    pub at_y: f32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AnimationClip {
    pub name: String,
    /// Seconds; 0 uses the last key or marker (1 for a blend space).
    pub duration: f32,
    pub repeat: TweenRepeat,
    pub tracks: Vec<AnimationTrack>,
    pub events: Vec<AnimationMarker>,
    /// A blend space: these clips, stretched to this clip's length so
    /// their cycles stay in step, mixed by the two points around the
    /// `blend_parameter` value. Empty for an ordinary clip.
    pub blend: Vec<BlendPoint>,
    pub blend_parameter: String,
    /// Set for a 2D blend space: `blend_parameter` is the x axis, this the
    /// y axis, and every point is weighted by gradient bands.
    pub blend_parameter_y: String,
}

impl Default for AnimationClip {
    fn default() -> Self {
        Self {
            name: "clip".into(),
            duration: 0.0,
            repeat: TweenRepeat::Loop,
            tracks: Vec::new(),
            events: Vec::new(),
            blend: Vec::new(),
            blend_parameter: String::new(),
            blend_parameter_y: String::new(),
        }
    }
}

impl AnimationClip {
    /// Playing length in seconds.
    #[must_use]
    pub fn length(&self) -> f32 {
        if self.duration > 0.0 {
            return self.duration;
        }
        let keys = self
            .tracks
            .iter()
            .flat_map(|t| t.keys.iter().map(|k| k.time));
        let length = keys
            .chain(self.events.iter().map(|e| e.time))
            .fold(0.0, f32::max);
        if length == 0.0 && !self.blend.is_empty() {
            return 1.0;
        }
        length
    }

    /// Clip time at `elapsed` seconds of play, and whether a `Once` clip
    /// has ended.
    #[must_use]
    pub fn local_time(&self, elapsed: f32) -> (f32, bool) {
        let length = self.length();
        if length <= 0.0 {
            return (0.0, self.repeat == TweenRepeat::Once);
        }
        match self.repeat {
            TweenRepeat::Once => (elapsed.min(length), elapsed >= length),
            TweenRepeat::Loop => (elapsed.rem_euclid(length), false),
            TweenRepeat::PingPong => {
                let phase = elapsed.rem_euclid(2.0 * length);
                (length - (phase - length).abs(), false)
            }
        }
    }

    /// Markers crossed while play time went from `before` to `after`, as
    /// `(play time, marker)` in time order. Half-open, so a marker on a
    /// tick boundary fires once; a `Once` clip's end is closed.
    fn crossed(&self, before: f32, after: f32) -> Vec<(f32, &AnimationMarker)> {
        let length = self.length();
        let mut hits = Vec::new();
        for marker in &self.events {
            let t = marker.time;
            let mut occurs = |at: f32| {
                if before <= at && at < after {
                    hits.push((at, marker));
                }
            };
            match self.repeat {
                TweenRepeat::Once => {
                    if before <= t
                        && (t < after || (after >= length && t <= length))
                    {
                        hits.push((t, marker));
                    }
                }
                _ if length <= 0.0 => {}
                TweenRepeat::Loop => {
                    // ponytail: at most 64 cycles per tick; more means a clip
                    // far shorter than a tick, where markers mean nothing.
                    let first = (before / length).floor() as i64;
                    let last =
                        ((after / length).floor() as i64).min(first + 64);
                    for cycle in first..=last {
                        occurs(cycle as f32 * length + t);
                    }
                }
                TweenRepeat::PingPong => {
                    let period = 2.0 * length;
                    let first = (before / period).floor() as i64;
                    let last =
                        ((after / period).floor() as i64).min(first + 64);
                    for cycle in first..=last {
                        let start = cycle as f32 * period;
                        occurs(start + t);
                        if t > 0.0 && t < length {
                            occurs(start + period - t);
                        }
                    }
                }
            }
        }
        hits.sort_by(|a, b| {
            a.0.total_cmp(&b.0).then_with(|| a.1.name.cmp(&b.1.name))
        });
        hits
    }
}

impl AnimationTrack {
    /// The track's value at clip time `t`; `None` without keys.
    #[must_use]
    pub fn sample(&self, t: f32) -> Option<Vec<f32>> {
        let keys = &self.keys;
        let last = keys.len().checked_sub(1)?;
        let next = keys.partition_point(|key| key.time <= t);
        if next == 0 || next > last {
            return Some(keys[next.min(last)].value.clone());
        }
        let (a, b) = (&keys[next - 1], &keys[next]);
        let span = b.time - a.time;
        let u = if span > 0.0 { (t - a.time) / span } else { 1.0 };
        let width = a.value.len().min(b.value.len());
        Some(match self.interpolation {
            Interpolation::Step => a.value.clone(),
            Interpolation::Linear => {
                blend(&self.property, &a.value, &b.value, u)
            }
            Interpolation::Smooth => {
                let b = near(&self.property, &a.value, &b.value);
                let p0 = near(
                    &self.property,
                    &a.value,
                    &keys[next.saturating_sub(2)].value,
                );
                let p3 =
                    near(&self.property, &b, &keys[(next + 1).min(last)].value);
                (0..width)
                    .map(|i| {
                        let at = |v: &Vec<f32>| {
                            v.get(i).copied().unwrap_or(a.value[i])
                        };
                        catmull_rom(at(&p0), a.value[i], b[i], at(&p3), u)
                    })
                    .collect()
            }
        })
    }
}

fn lerp(a: &[f32], b: &[f32], u: f32) -> Vec<f32> {
    a.iter().zip(b).map(|(a, b)| a + (b - a) * u).collect()
}

/// `lerp` that turns an `Orientation` the short way round.
fn blend(
    property: &AnimationProperty,
    a: &[f32],
    b: &[f32],
    u: f32,
) -> Vec<f32> {
    lerp(a, &near(property, a, b), u)
}

/// `b`, negated when it is an `Orientation` on the far side of `a`: q and
/// -q are the same turn, and only the near one blends without a spin.
fn near(property: &AnimationProperty, a: &[f32], b: &[f32]) -> Vec<f32> {
    let dot: f32 = a.iter().zip(b).map(|(a, b)| a * b).sum();
    if *property == AnimationProperty::Orientation && dot < 0.0 {
        b.iter().map(|v| -v).collect()
    } else {
        b.to_vec()
    }
}

fn catmull_rom(p0: f32, p1: f32, p2: f32, p3: f32, u: f32) -> f32 {
    let (u2, u3) = (u * u, u * u * u);
    0.5 * (2.0 * p1
        + (p2 - p0) * u
        + (2.0 * p0 - 5.0 * p1 + 4.0 * p2 - p3) * u2
        + (3.0 * p1 - p0 - 3.0 * p2 + p3) * u3)
}

/// Live change asked for by game code or the editor; the next fixed tick
/// applies it.
#[derive(Clone, Debug, PartialEq)]
pub enum AnimationCommand {
    Play(String),
    Stop,
    /// Blends from the current pose into the clip over this many seconds.
    Crossfade(String, f32),
}

/// How a transition tests its parameter against `value`.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize,
)]
pub enum AnimationCompare {
    #[default]
    Above,
    Below,
    Equal,
}

/// A state machine edge: when the playing clip is `from` and the test
/// holds, crossfade to `to`. The first matching transition in list order
/// wins, once per tick.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AnimationTransition {
    /// Clip name; empty matches any clip except `to`.
    pub from: String,
    pub to: String,
    /// Parameter to test; empty skips the test. A missing parameter is 0.
    pub parameter: String,
    pub compare: AnimationCompare,
    pub value: f32,
    /// Also wait until `from` has played its whole length once.
    pub at_end: bool,
    /// Crossfade seconds; 0 cuts.
    pub fade: f32,
}

impl AnimationTransition {
    fn passes(&self, parameters: &BTreeMap<String, f32>) -> bool {
        if self.parameter.is_empty() {
            return true;
        }
        let value = parameters.get(&self.parameter).copied().unwrap_or(0.0);
        match self.compare {
            AnimationCompare::Above => value > self.value,
            AnimationCompare::Below => value < self.value,
            AnimationCompare::Equal => value == self.value,
        }
    }
}

/// A clip that plays on top of the state machine from scene start, on its
/// own clock, such as waving arms over a walk or breathing over any pose.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AnimationLayer {
    pub clip: String,
    /// 0 to 1; how much of the layer shows.
    pub weight: f32,
    /// Parameter read as the weight instead, when set.
    pub weight_parameter: String,
    /// Adds how far the clip has moved from its first frame instead of
    /// replacing the pose; only changes tracks the state pose keys.
    pub additive: bool,
    /// Target paths the layer may write, with their children (`Spine`
    /// also covers `Spine/Arm`; empty string is the object itself). Empty
    /// writes every track.
    pub mask: Vec<String>,
}

impl Default for AnimationLayer {
    fn default() -> Self {
        Self {
            clip: String::new(),
            weight: 1.0,
            weight_parameter: String::new(),
            additive: false,
            mask: Vec::new(),
        }
    }
}

/// What happens to the horizontal motion of the root bone's `Position`
/// track. Every policy but `Off` keeps the root bone in place on x and z,
/// at its first-frame spot, and measures how far it would have moved.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize,
)]
pub enum RootMotion {
    /// The clip moves the root bone as keyed.
    #[default]
    Off,
    /// Only collected: game code takes it with `take_root_motion` and moves
    /// the character itself, such as through `move_character`.
    InPlace,
    /// Moves this object's transform. For objects without a physics body.
    Transform,
    /// Sets this object's GPU body velocity through the physics command
    /// bridge (angular velocity is set to 0). Bodies without a
    /// `PhysicsId` do not move.
    Velocity,
}

/// Keyframe clips for this entity and its named descendants. See the
/// module docs.
#[derive(Component, Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Animation {
    pub clips: Vec<AnimationClip>,
    /// Clip played when the scene starts; empty plays nothing.
    pub autoplay: String,
    /// Playback rate; 1 is real time, 0 freezes.
    pub speed: f32,
    /// Named numbers game code sets for `transitions` to test.
    pub parameters: BTreeMap<String, f32>,
    /// State machine edges between clips, checked each tick.
    pub transitions: Vec<AnimationTransition>,
    /// Clips played on top of the state machine, in order.
    pub layers: Vec<AnimationLayer>,
    pub root_motion: RootMotion,
    /// Path of the bone whose `Position` track carries the motion; empty
    /// is this object's own track.
    pub root_bone: String,
    /// Humanoid names of this rig's bones, for retargeting clips between
    /// skeletons; empty matches bones by name.
    pub humanoid: Vec<super::HumanoidBone>,
    #[serde(skip)]
    pub command: Option<AnimationCommand>,
}

impl Default for Animation {
    fn default() -> Self {
        Self {
            clips: vec![AnimationClip::default()],
            autoplay: "clip".into(),
            speed: 1.0,
            parameters: BTreeMap::new(),
            transitions: Vec::new(),
            layers: Vec::new(),
            root_motion: RootMotion::Off,
            root_bone: String::new(),
            humanoid: Vec::new(),
            command: None,
        }
    }
}

impl Animation {
    #[must_use]
    pub fn clip(&self, name: &str) -> Option<usize> {
        self.clips.iter().position(|clip| clip.name == name)
    }
}

/// Where a clip is in its play.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ClipTime {
    pub clip: usize,
    /// Seconds played, speed applied.
    pub elapsed: f32,
}

/// Playback state the player adds next to each [`Animation`].
#[derive(Component, Clone, Debug, Default, PartialEq)]
pub struct AnimationPlayer {
    pub current: Option<ClipTime>,
    pub playing: bool,
    /// Clip being faded out, and fade progress and length in seconds.
    pub fading: Option<(ClipTime, f32, f32)>,
    started: bool,
    /// Material copies this player owns, one per colored target.
    materials: Vec<(Entity, Handle<MaterialAsset>)>,
    /// Seconds played by each of `Animation::layers`.
    layers: Vec<f32>,
    /// Root motion of the last tick, in this object's local frame.
    pub root_delta: [f32; 3],
    /// Root motion gathered since game code last took it.
    pub root_motion: [f32; 3],
    /// Each animated transform value from before this player first wrote
    /// it, for crossfades to ease tracks the new clip leaves back to.
    rest: Vec<Write>,
}

/// Sent when a playing clip passes one of its markers. Delivered in tick
/// order, then by animated entity, then by time.
#[derive(Clone, Debug, PartialEq)]
pub struct AnimationEvent {
    /// Fixed tick the marker was passed on.
    pub tick: u64,
    /// The animated entity.
    pub entity: Entity,
    /// Name of the animated object, empty without one.
    pub object: String,
    /// Name of the clip that holds the marker.
    pub clip: String,
    /// The marker's name, as written in the clip.
    pub name: String,
}

#[derive(Clone, Debug, PartialEq)]
struct Write {
    target: String,
    property: AnimationProperty,
    value: Vec<f32>,
}

fn track_pose(clip: &AnimationClip, t: f32) -> Vec<Write> {
    clip.tracks
        .iter()
        .filter_map(|track| {
            Some(Write {
                target: track.target.clone(),
                property: track.property.clone(),
                value: track.sample(t)?,
            })
        })
        .collect()
}

fn pose(animation: &Animation, clip: &AnimationClip, t: f32) -> Vec<Write> {
    let mut writes = track_pose(clip, t);
    let parameter =
        |name: &String| animation.parameters.get(name).copied().unwrap_or(0.0);
    let mut points: Vec<([f32; 2], &AnimationClip)> = clip
        .blend
        .iter()
        .filter_map(|point| {
            let child = &animation.clips[animation.clip(&point.clip)?];
            Some(([point.at, point.at_y], child))
        })
        .collect();
    if points.is_empty() {
        return writes;
    }
    let value = [
        parameter(&clip.blend_parameter),
        parameter(&clip.blend_parameter_y),
    ];
    let weights: Vec<(f32, &AnimationClip)> =
        if clip.blend_parameter_y.is_empty() {
            points.sort_by(|a, b| a.0[0].total_cmp(&b.0[0]));
            let after = points.partition_point(|point| point.0[0] <= value[0]);
            let (low, high) = (
                points[after.saturating_sub(1)],
                points[after.min(points.len() - 1)],
            );
            let span = high.0[0] - low.0[0];
            let weight = if span > 0.0 {
                (value[0] - low.0[0]) / span
            } else {
                0.0
            };
            vec![(1.0 - weight, low.1), (weight, high.1)]
        } else {
            band_weights(&points, value)
        };
    // ponytail: one level; a blend point that is itself a blend space
    // plays only its own tracks.
    let phase = t / clip.length();
    let sample =
        |child: &AnimationClip| track_pose(child, phase * child.length());
    let mut mixed = Vec::new();
    let mut total = 0.0;
    for (weight, child) in weights {
        if weight <= 0.0 {
            continue;
        }
        total += weight;
        let mut next = sample(child);
        mix(&mut next, std::mem::take(&mut mixed), weight / total, &[]);
        mixed = next;
    }
    mix(&mut writes, mixed, 1.0, &[]);
    writes
}

/// Gradient band weights of a 2D blend space, normalized: each point's
/// weight falls to 0 at every other point, so a point plays alone on its
/// spot and neighbors mix between. Points sit in the clip's list order.
fn band_weights<'a>(
    points: &[([f32; 2], &'a AnimationClip)],
    p: [f32; 2],
) -> Vec<(f32, &'a AnimationClip)> {
    let mut weights: Vec<(f32, &AnimationClip)> = points
        .iter()
        .enumerate()
        .map(|(i, &(a, clip))| {
            let weight = points
                .iter()
                .enumerate()
                .filter(|&(j, _)| j != i)
                .map(|(_, &(b, _))| {
                    let ab = [b[0] - a[0], b[1] - a[1]];
                    let length = ab[0] * ab[0] + ab[1] * ab[1];
                    if length == 0.0 {
                        return 1.0;
                    }
                    let along = (p[0] - a[0]) * ab[0] + (p[1] - a[1]) * ab[1];
                    (1.0 - along / length).clamp(0.0, 1.0)
                })
                .fold(1.0, f32::min);
            (weight, clip)
        })
        .collect();
    let total: f32 = weights.iter().map(|w| w.0).sum();
    if total > 0.0 {
        for weight in &mut weights {
            weight.0 /= total;
        }
    }
    weights
}

/// Blends `from` into `writes` by `weight` (1 keeps `writes`). A write only
/// `from` has blends toward its `rest` value, or is kept without one;
/// writes only `writes` has are kept.
fn mix(writes: &mut Vec<Write>, from: Vec<Write>, weight: f32, rest: &[Write]) {
    for mut from in from {
        let p = &from.property;
        match writes.iter_mut().find(|w| same(w, &from)) {
            Some(to) => to.value = blend(p, &from.value, &to.value, weight),
            None => {
                if let Some(rest) = rest.iter().find(|r| same(r, &from)) {
                    from.value = blend(p, &from.value, &rest.value, weight);
                }
                writes.push(from);
            }
        }
    }
}

/// Per fixed step, after tweens: applies commands, advances every player,
/// writes the sampled pose and sends marker events.
pub(super) fn advance_animations(world: &mut World) {
    let time = world.resource::<FrameTime>();
    let (dt, tick) = (time.fixed_delta.as_secs_f32(), time.fixed_tick);
    step_animations(world, dt, tick);
}

/// One player step of `dt` seconds; `tick` stamps the events.
pub fn step_animations(world: &mut World, dt: f32, tick: u64) {
    let mut query = world.query::<(
        Entity,
        &mut Animation,
        Option<&mut AnimationPlayer>,
        Option<&SceneId>,
        Option<&Name>,
    )>();
    let mut missing = Vec::new();
    let mut steps = Vec::new();
    for (entity, mut animation, player, id, name) in query.iter_mut(world) {
        let Some(mut player) = player else {
            missing.push(entity);
            continue;
        };
        let sorted = |clip: &AnimationClip| {
            clip.tracks
                .iter()
                .all(|t| t.keys.is_sorted_by(|a, b| a.time <= b.time))
        };
        if !animation.clips.iter().all(sorted) {
            for track in animation.clips.iter_mut().flat_map(|c| &mut c.tracks)
            {
                track.keys.sort_by(|a, b| a.time.total_cmp(&b.time));
            }
        }
        let command = animation.command.take();
        let (writes, events) = advance(&animation, &mut player, command, dt);
        if writes.is_empty() && events.is_empty() {
            continue;
        }
        let key = (id.map(|id| id.0.as_u128()), entity.to_bits());
        let object = name.map(|n| n.0.clone()).unwrap_or_default();
        let events: Vec<AnimationEvent> = events
            .into_iter()
            .map(|(clip, name)| AnimationEvent {
                tick,
                entity,
                object: object.clone(),
                clip,
                name,
            })
            .collect();
        steps.push((key, entity, writes, events));
    }
    // New players start next tick, like particle systems.
    for entity in missing {
        world.entity_mut(entity).insert(AnimationPlayer::default());
    }
    steps.sort_by_key(|step| step.0);
    for (_, entity, writes, events) in steps {
        for write in writes {
            keep_rest(world, entity, &write);
            apply(world, entity, write);
        }
        move_root(world, entity, dt);
        if let Some(mut queue) =
            world.get_resource_mut::<super::EventQueue<AnimationEvent>>()
        {
            for event in events {
                queue.send(event);
            }
        }
    }
    super::solve_ik(world);
    super::step_ragdolls(world, dt);
}

/// Applies the last tick's root motion by the object's policy.
fn move_root(world: &mut World, entity: Entity, dt: f32) {
    let Some(policy) = world.get::<Animation>(entity).map(|a| a.root_motion)
    else {
        return;
    };
    let Some(delta) =
        world.get::<AnimationPlayer>(entity).map(|p| p.root_delta)
    else {
        return;
    };
    if delta == [0.0; 3] || dt <= 0.0 {
        return;
    }
    let Some(transform) = world.get::<Transform>(entity).copied() else {
        return;
    };
    let [x, y, z] = transform.rotation;
    let scaled =
        nalgebra::Vector3::from(std::array::from_fn::<f32, 3, _>(|i| {
            delta[i] * transform.scale[i]
        }));
    let moved = super::sim_math::rotation_from_euler(x, y, z) * scaled;
    match policy {
        RootMotion::Off | RootMotion::InPlace => {}
        RootMotion::Transform => {
            if let Some(mut transform) = world.get_mut::<Transform>(entity) {
                for i in 0..3 {
                    transform.position[i] += moved[i];
                }
            }
        }
        RootMotion::Velocity => {
            let Some(&id) = world.get::<super::PhysicsId>(entity) else {
                return;
            };
            if let Some(mut commands) =
                world.get_resource_mut::<super::GpuPhysicsCommands>()
            {
                commands.push(
                    id,
                    super::GpuBodyCommand::SetVelocity {
                        linear: (moved / dt).into(),
                        angular: [0.0; 3],
                    },
                );
            }
        }
    }
}

fn advance(
    animation: &Animation,
    player: &mut AnimationPlayer,
    command: Option<AnimationCommand>,
    dt: f32,
) -> (Vec<Write>, Vec<(String, String)>) {
    let (mut writes, events) = advance_state(animation, player, command, dt);
    let step = dt * animation.speed.max(0.0);
    player.root_delta = [0.0; 3];
    if animation.root_motion != RootMotion::Off && !writes.is_empty() {
        extract_root_motion(animation, player, &mut writes, step);
    }
    player.layers.resize(animation.layers.len(), 0.0);
    for (layer, elapsed) in animation.layers.iter().zip(&mut player.layers) {
        *elapsed += step;
        let weight = if layer.weight_parameter.is_empty() {
            layer.weight
        } else {
            animation
                .parameters
                .get(&layer.weight_parameter)
                .copied()
                .unwrap_or(0.0)
        }
        .clamp(0.0, 1.0);
        let Some(clip) = animation.clip(&layer.clip) else {
            continue;
        };
        if weight <= 0.0 {
            continue;
        }
        let clip = &animation.clips[clip];
        let (t, _) = clip.local_time(*elapsed);
        let masked = |write: &Write| {
            layer.mask.is_empty()
                || layer.mask.iter().any(|path| {
                    write.target == *path
                        || write
                            .target
                            .strip_prefix(path.as_str())
                            .is_some_and(|rest| rest.starts_with('/'))
                })
        };
        let layered = pose(animation, clip, t).into_iter().filter(masked);
        if layer.additive {
            let reference = pose(animation, clip, 0.0);
            for write in layered {
                add(&mut writes, &write, &reference, weight);
            }
        } else {
            for write in layered {
                match writes.iter_mut().find(|w| same(w, &write)) {
                    Some(to) => {
                        to.value =
                            blend(&to.property, &to.value, &write.value, weight)
                    }
                    None => writes.push(write),
                }
            }
        }
    }
    (writes, events)
}

/// Keeps the root bone at its first-frame x and z and records how far the
/// state clip (and a clip fading out) moved it this tick.
fn extract_root_motion(
    animation: &Animation,
    player: &mut AnimationPlayer,
    writes: &mut [Write],
    step: f32,
) {
    let root = |writes: Vec<Write>| {
        writes
            .into_iter()
            .find(|w| {
                w.target == animation.root_bone
                    && w.property == AnimationProperty::Position
            })
            .map(|w| {
                std::array::from_fn(|i| w.value.get(i).copied().unwrap_or(0.0))
            })
    };
    let position = |clip: &AnimationClip, t: f32| -> [f32; 3] {
        root(pose(animation, clip, t)).unwrap_or([0.0; 3])
    };
    // ponytail: samples whole poses for one track; fine for a few
    // characters, cache per-clip root tracks if it shows in profiles.
    let delta = |time: ClipTime| -> [f32; 3] {
        let Some(clip) = animation.clips.get(time.clip) else {
            return [0.0; 3];
        };
        let (t0, _) = clip.local_time((time.elapsed - step).max(0.0));
        let (t1, _) = clip.local_time(time.elapsed);
        let moved = |a: f32, b: f32| {
            let (a, b) = (position(clip, a), position(clip, b));
            [b[0] - a[0], 0.0, b[2] - a[2]]
        };
        if clip.repeat == TweenRepeat::Loop && t1 < t0 {
            let (end, start) = (moved(t0, clip.length()), moved(0.0, t1));
            [end[0] + start[0], 0.0, end[2] + start[2]]
        } else {
            moved(t0, t1)
        }
    };
    let Some(current) = player.current else {
        return;
    };
    let mut moved = delta(current);
    if let Some((old, done, length)) = player.fading {
        let weight = (done / length).min(1.0);
        let old = delta(old);
        for (moved, old) in moved.iter_mut().zip(old) {
            *moved = old + (*moved - old) * weight;
        }
    }
    player.root_delta = moved;
    for (total, moved) in player.root_motion.iter_mut().zip(moved) {
        *total += moved;
    }
    if let Some(clip) = animation.clips.get(current.clip) {
        let rest = position(clip, 0.0);
        if let Some(write) = writes.iter_mut().find(|w| {
            w.target == animation.root_bone
                && w.property == AnimationProperty::Position
        }) {
            write.value.resize(3, 0.0);
            write.value[0] = rest[0];
            write.value[2] = rest[2];
        }
    }
}

fn same(a: &Write, b: &Write) -> bool {
    a.target == b.target && a.property == b.property
}

/// Adds `weight` of how far `layer` has moved from the layer clip's first
/// frame onto the matching write in `writes`; tracks the state pose does
/// not key are left alone.
fn add(writes: &mut [Write], layer: &Write, reference: &[Write], weight: f32) {
    let Some(base) = writes.iter_mut().find(|w| same(w, layer)) else {
        return;
    };
    let Some(rest) = reference.iter().find(|w| same(w, layer)) else {
        return;
    };
    let (b, l, r) = (&mut base.value, &layer.value, &rest.value);
    match layer.property {
        AnimationProperty::Visible => {}
        AnimationProperty::Orientation => {
            let quaternion = |v: &[f32]| {
                let at =
                    |i: usize| v.get(i).copied().unwrap_or(f32::from(i == 3));
                nalgebra::Quaternion::new(at(3), at(0), at(1), at(2))
            };
            // The delta in the joint's own frame, so it bends the joint
            // the same way whatever the state pose turned it to.
            let mut delta = quaternion(r).conjugate() * quaternion(l);
            if delta.w < 0.0 {
                delta = -delta;
            }
            let delta = nalgebra::Quaternion::identity().lerp(&delta, weight);
            let q = (quaternion(b) * delta).normalize();
            *b = vec![q.i, q.j, q.k, q.w];
        }
        AnimationProperty::Scale => {
            for ((b, l), r) in b.iter_mut().zip(l).zip(r) {
                if *r != 0.0 {
                    *b *= 1.0 + weight * (l / r - 1.0);
                }
            }
        }
        _ => {
            for ((b, l), r) in b.iter_mut().zip(l).zip(r) {
                *b += weight * (l - r);
            }
        }
    }
}

fn advance_state(
    animation: &Animation,
    player: &mut AnimationPlayer,
    mut command: Option<AnimationCommand>,
    dt: f32,
) -> (Vec<Write>, Vec<(String, String)>) {
    if !player.started {
        player.started = true;
        if command.is_none() && !animation.autoplay.is_empty() {
            command = Some(AnimationCommand::Play(animation.autoplay.clone()));
        }
    }
    if command.is_none() {
        command = next_state(animation, player);
    }
    match command {
        Some(AnimationCommand::Play(name)) => {
            start(animation, player, &name, 0.0)
        }
        Some(AnimationCommand::Crossfade(name, seconds)) => {
            start(animation, player, &name, seconds);
        }
        Some(AnimationCommand::Stop) => {
            player.playing = false;
            player.fading = None;
        }
        None => {}
    }
    let Some(current) = player.current.filter(|_| player.playing) else {
        return (Vec::new(), Vec::new());
    };
    let Some(clip) = animation.clips.get(current.clip) else {
        player.playing = false;
        return (Vec::new(), Vec::new());
    };
    let step = dt * animation.speed.max(0.0);
    let elapsed = current.elapsed + step;
    let events = clip
        .crossed(current.elapsed, elapsed)
        .into_iter()
        .map(|(_, marker)| (clip.name.clone(), marker.name.clone()))
        .collect();
    player.current = Some(ClipTime { elapsed, ..current });
    let (t, finished) = clip.local_time(elapsed);
    let mut writes = pose(animation, clip, t);
    if let Some((old, done, length)) = player.fading {
        let done = done + step;
        let weight = (done / length).min(1.0);
        if let Some(old_clip) = animation.clips.get(old.clip) {
            let old = ClipTime {
                elapsed: old.elapsed + step,
                ..old
            };
            player.fading = Some((old, done, length));
            let (t, _) = old_clip.local_time(old.elapsed);
            let old = pose(animation, old_clip, t);
            mix(&mut writes, old, weight, &player.rest);
        }
        if weight >= 1.0 {
            player.fading = None;
        }
    }
    if finished {
        player.playing = false;
    }
    (writes, events)
}

/// The first transition out of the current clip whose test holds, as a
/// crossfade. A stopped clip only leaves once it has played to its end.
fn next_state(
    animation: &Animation,
    player: &AnimationPlayer,
) -> Option<AnimationCommand> {
    let current = player.current?;
    let clip = animation.clips.get(current.clip)?;
    let ended = current.elapsed >= clip.length();
    if !player.playing && !ended {
        return None;
    }
    animation
        .transitions
        .iter()
        .find(|edge| {
            edge.to != clip.name
                && (edge.from.is_empty() || edge.from == clip.name)
                && (!edge.at_end || ended)
                && edge.passes(&animation.parameters)
        })
        .map(|edge| AnimationCommand::Crossfade(edge.to.clone(), edge.fade))
}

fn start(
    animation: &Animation,
    player: &mut AnimationPlayer,
    name: &str,
    fade: f32,
) {
    let Some(clip) = animation.clip(name) else {
        return;
    };
    player.fading = player
        .current
        .filter(|_| player.playing && fade > 0.0)
        .map(|current| (current, 0.0, fade));
    player.current = Some(ClipTime { clip, elapsed: 0.0 });
    player.playing = true;
}

/// Authored transform and visibility of objects the editor posed with
/// [`apply_preview_pose`]. Scene documents (saves, Undo snapshots) write
/// these instead of the posed values, so a scrub never leaks into the scene.
#[derive(Resource, Default, Clone, Debug)]
pub struct PreviewRestPose(pub Vec<(SceneId, Transform, Option<Visibility>)>);

/// Writes the transform and visibility tracks of `clip` at clip time `t`
/// to `entity` and its children without playing it: the editor's scrub
/// preview. Color, emissive and field tracks only play in the game.
pub fn apply_preview_pose(
    world: &mut World,
    entity: Entity,
    animation: &Animation,
    clip: &AnimationClip,
    t: f32,
) {
    for write in pose(animation, clip, t) {
        if !matches!(
            write.property,
            AnimationProperty::Color
                | AnimationProperty::Emissive
                | AnimationProperty::Field { .. }
        ) {
            apply(world, entity, write);
        }
    }
}

/// The named descendant at `path` (`Arm/Hand`), or `root` for an empty path.
/// `..` steps to the parent. A step also matches the
/// `"<parent name>/<step>"` names that `spawn_copy` gives copied children,
/// so paths authored on a template work on its copies.
#[must_use]
pub fn find_target(world: &World, root: Entity, path: &str) -> Option<Entity> {
    path.split('/').filter(|part| !part.is_empty()).try_fold(
        root,
        |at, part| {
            if part == ".." {
                return world.get::<super::Parent>(at).map(|parent| parent.0);
            }
            let parent = world.get::<Name>(at).map(|name| name.0.as_str());
            world.get::<Children>(at)?.0.iter().copied().find(|child| {
                world
                    .get::<Name>(*child)
                    .is_some_and(|name| is_step(&name.0, parent, part))
            })
        },
    )
}

/// True when a child called `child`, under a parent called `parent`, is
/// the path step `step`: its own name, or a copy's `"<parent>/<step>"`.
pub(crate) fn is_step(child: &str, parent: Option<&str>, step: &str) -> bool {
    child == step
        || parent.is_some_and(|parent| {
            child
                .strip_prefix(parent)
                .and_then(|rest| rest.strip_prefix('/'))
                == Some(step)
        })
}

/// Records the transform value `write` is about to replace, the first time
/// the player at `root` writes that track.
fn keep_rest(world: &mut World, root: Entity, write: &Write) {
    let known = world
        .get::<AnimationPlayer>(root)
        .is_none_or(|player| player.rest.iter().any(|r| same(r, write)));
    let target = find_target(world, root, &write.target);
    let Some(transform) = target
        .filter(|_| !known)
        .and_then(|t| world.get::<Transform>(t))
    else {
        return;
    };
    let value = match write.property {
        AnimationProperty::Position => transform.position.to_vec(),
        AnimationProperty::Rotation => transform.rotation.to_vec(),
        AnimationProperty::Scale => transform.scale.to_vec(),
        AnimationProperty::Orientation => {
            let [roll, pitch, yaw] = transform.rotation;
            let q =
                nalgebra::UnitQuaternion::from_euler_angles(roll, pitch, yaw);
            vec![q.i, q.j, q.k, q.w]
        }
        _ => return,
    };
    let rest = Write {
        value,
        ..write.clone()
    };
    if let Some(mut player) = world.get_mut::<AnimationPlayer>(root) {
        player.rest.push(rest);
    }
}

fn apply(world: &mut World, root: Entity, write: Write) {
    let Some(target) = find_target(world, root, &write.target) else {
        return;
    };
    let value = write.value;
    let vec3 = |default: [f32; 3]| {
        std::array::from_fn(|i| value.get(i).copied().unwrap_or(default[i]))
    };
    match write.property {
        AnimationProperty::Position
        | AnimationProperty::Rotation
        | AnimationProperty::Scale => {
            let Some(mut transform) = world.get_mut::<Transform>(target) else {
                return;
            };
            let slot = match write.property {
                AnimationProperty::Position => &mut transform.position,
                AnimationProperty::Rotation => &mut transform.rotation,
                _ => &mut transform.scale,
            };
            let new = vec3(*slot);
            if *slot != new {
                *slot = new;
            }
        }
        AnimationProperty::Orientation => {
            let [x, y, z, w] = std::array::from_fn(|i| {
                value.get(i).copied().unwrap_or(f32::from(i == 3))
            });
            let Some(quaternion) = nalgebra::UnitQuaternion::try_new(
                nalgebra::Quaternion::new(w, x, y, z),
                f32::EPSILON,
            ) else {
                return;
            };
            let (roll, pitch, yaw) = quaternion.euler_angles();
            let new = [roll, pitch, yaw];
            if let Some(mut transform) = world.get_mut::<Transform>(target) {
                if transform.rotation != new {
                    transform.rotation = new;
                }
            }
        }
        AnimationProperty::Visible => {
            let visible = value.first().is_some_and(|v| *v >= 0.5);
            match world.get_mut::<Visibility>(target) {
                Some(mut current) if current.visible != visible => {
                    current.visible = visible;
                }
                Some(_) => {}
                None => {
                    world.entity_mut(target).insert(Visibility { visible });
                }
            }
        }
        AnimationProperty::Color | AnimationProperty::Emissive => {
            let Some(handle) = owned_material(world, root, target) else {
                return;
            };
            let Some(mut assets) = world.get_resource_mut::<AssetServer>()
            else {
                return;
            };
            let Some(material) = assets.materials.get(handle) else {
                return;
            };
            let color = write.property == AnimationProperty::Color;
            let changed = if color {
                let c = material.base_color;
                let new: [f32; 4] = std::array::from_fn(|i| {
                    value.get(i).copied().unwrap_or(c[i])
                });
                (new != c).then_some((Some(new), None))
            } else {
                let new = vec3(material.emissive);
                (new != material.emissive).then_some((None, Some(new)))
            };
            // `get_mut` bumps the revision the renderer re-uploads on.
            let Some((base, emissive)) = changed else {
                return;
            };
            if let Some(material) = assets.materials.get_mut(handle) {
                if let Some(base) = base {
                    material.base_color = base;
                }
                if let Some(emissive) = emissive {
                    material.emissive = emissive;
                }
            }
        }
        AnimationProperty::Field { component, path } => {
            let json = match value.as_slice() {
                [one] => serde_json::json!(one),
                many => serde_json::json!(many),
            };
            // ponytail: round-trips the whole component through JSON each
            // tick and resets its unsaved state; a typed setter per field if
            // field tracks get hot.
            let _ = super::set_registered_component_field(
                world, target, &component, &path, json,
            );
        }
    }
}

/// The player's own copy of `target`'s material, so recoloring one object
/// leaves others that shared the material alone and edits stay in place.
fn owned_material(
    world: &mut World,
    root: Entity,
    target: Entity,
) -> Option<Handle<MaterialAsset>> {
    let current = world.get::<MeshRenderer>(target)?.material;
    let player = world.get::<AnimationPlayer>(root)?;
    if player.materials.contains(&(target, current)) {
        return Some(current);
    }
    let mut assets = world.get_resource_mut::<AssetServer>()?;
    let copy = assets.materials.get(current)?.clone();
    let handle = assets.materials.insert(copy);
    world.get_mut::<MeshRenderer>(target)?.material = handle;
    let mut player = world.get_mut::<AnimationPlayer>(root)?;
    player.materials.retain(|(entity, _)| *entity != target);
    player.materials.push((target, handle));
    Some(handle)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track(
        property: AnimationProperty,
        keys: &[(f32, &[f32])],
    ) -> AnimationTrack {
        AnimationTrack {
            target: String::new(),
            property,
            interpolation: Interpolation::Linear,
            keys: keys
                .iter()
                .map(|(time, value)| Keyframe {
                    time: *time,
                    value: value.to_vec(),
                })
                .collect(),
        }
    }

    fn clip(
        name: &str,
        repeat: TweenRepeat,
        tracks: Vec<AnimationTrack>,
    ) -> AnimationClip {
        AnimationClip {
            name: name.into(),
            repeat,
            tracks,
            ..AnimationClip::default()
        }
    }

    fn world_with(animation: Animation) -> (World, Entity) {
        let mut world = World::new();
        world.insert_resource(
            super::super::EventQueue::<AnimationEvent>::default(),
        );
        let entity = world
            .spawn((animation, Transform::default(), Name("Door".into())))
            .id();
        (world, entity)
    }

    fn run(world: &mut World, ticks: std::ops::Range<u64>) {
        for tick in ticks {
            step_animations(world, 0.1, tick);
        }
    }

    fn events(world: &mut World) -> Vec<(u64, String)> {
        let mut queue =
            world.resource_mut::<super::super::EventQueue<AnimationEvent>>();
        queue.begin_frame();
        queue.iter().map(|e| (e.tick, e.name.clone())).collect()
    }

    #[test]
    fn interpolation_modes_sample_between_keys() {
        let mut t = track(
            AnimationProperty::Position,
            &[(0.0, &[0.0]), (1.0, &[1.0]), (2.0, &[0.0])],
        );
        assert_eq!(t.sample(-1.0), Some(vec![0.0]));
        assert_eq!(t.sample(0.25), Some(vec![0.25]));
        assert_eq!(t.sample(5.0), Some(vec![0.0]));
        t.interpolation = Interpolation::Step;
        assert_eq!(t.sample(0.9), Some(vec![0.0]));
        assert_eq!(t.sample(1.0), Some(vec![1.0]));
        t.interpolation = Interpolation::Smooth;
        let smooth = t.sample(0.5).unwrap()[0];
        assert!(smooth > 0.5 && smooth < 0.7, "{smooth}");
        assert_eq!(t.sample(1.0), Some(vec![1.0]));
    }

    #[test]
    fn repeat_modes_map_play_time() {
        let mut c = clip(
            "a",
            TweenRepeat::Loop,
            vec![track(
                AnimationProperty::Position,
                &[(0.0, &[0.0]), (2.0, &[1.0])],
            )],
        );
        assert_eq!(c.local_time(5.0), (1.0, false));
        c.repeat = TweenRepeat::PingPong;
        assert_eq!(c.local_time(3.0), (1.0, false));
        c.repeat = TweenRepeat::Once;
        assert_eq!(c.local_time(3.0), (2.0, true));
    }

    #[test]
    fn autoplay_moves_the_entity_and_once_stops_at_the_end() {
        let rise = clip(
            "rise",
            TweenRepeat::Once,
            vec![track(
                AnimationProperty::Position,
                &[(0.0, &[0.0, 0.0, 0.0]), (1.0, &[0.0, 2.0, 0.0])],
            )],
        );
        let (mut world, door) = world_with(Animation {
            clips: vec![rise],
            autoplay: "rise".into(),
            ..Animation::default()
        });
        run(&mut world, 0..6); // first tick adds the player
        let y = world.get::<Transform>(door).unwrap().position[1];
        assert!((y - 1.0).abs() < 1e-5, "{y}");
        run(&mut world, 6..20);
        assert_eq!(world.get::<Transform>(door).unwrap().position[1], 2.0);
        assert!(!world.get::<AnimationPlayer>(door).unwrap().playing);
    }

    #[test]
    fn markers_fire_once_per_pass_in_tick_order() {
        let mut spin = clip("spin", TweenRepeat::Loop, vec![]);
        spin.duration = 1.0;
        spin.events = vec![
            AnimationMarker {
                time: 0.0,
                name: "start".into(),
            },
            AnimationMarker {
                time: 0.5,
                name: "half".into(),
            },
        ];
        let (mut world, _) = world_with(Animation {
            clips: vec![spin],
            autoplay: "spin".into(),
            ..Animation::default()
        });
        run(&mut world, 0..17);
        let names: Vec<_> =
            events(&mut world).into_iter().map(|(_, n)| n).collect();
        assert_eq!(names, ["start", "half", "start", "half"]);
    }

    #[test]
    fn crossfade_blends_and_stop_holds_the_pose() {
        let at = |x: f32| {
            clip(
                &x.to_string(),
                TweenRepeat::Loop,
                vec![track(
                    AnimationProperty::Position,
                    &[(0.0, &[x, 0.0, 0.0]), (1.0, &[x, 0.0, 0.0])],
                )],
            )
        };
        let (mut world, door) = world_with(Animation {
            clips: vec![at(0.0), at(10.0)],
            autoplay: "0".into(),
            ..Animation::default()
        });
        run(&mut world, 0..3);
        world.get_mut::<Animation>(door).unwrap().command =
            Some(AnimationCommand::Crossfade("10".into(), 1.0));
        run(&mut world, 3..8);
        let x = world.get::<Transform>(door).unwrap().position[0];
        assert!((x - 5.0).abs() < 1e-4, "{x}");
        run(&mut world, 8..14);
        assert_eq!(world.get::<Transform>(door).unwrap().position[0], 10.0);
        world.get_mut::<Animation>(door).unwrap().command =
            Some(AnimationCommand::Stop);
        world.get_mut::<Transform>(door).unwrap().position[0] = 3.0;
        run(&mut world, 14..16);
        assert_eq!(world.get::<Transform>(door).unwrap().position[0], 3.0);
    }

    #[test]
    fn orientation_blends_take_the_short_way_round() {
        let half = std::f32::consts::FRAC_1_SQRT_2;
        // -(90 degrees about Y): the same turn as +90, other sign.
        let quarter: &[f32] = &[0.0, -half, 0.0, -half];
        let eighth = nalgebra::UnitQuaternion::from_axis_angle(
            &nalgebra::Vector3::y_axis(),
            std::f32::consts::FRAC_PI_4,
        );
        let is_eighth = |v: &[f32]| {
            let q = nalgebra::Quaternion::new(v[3], v[0], v[1], v[2]);
            (q.normalize().dot(eighth.quaternion()).abs() - 1.0).abs() < 1e-4
        };
        let mut t = track(
            AnimationProperty::Orientation,
            &[(0.0, &[0.0, 0.0, 0.0, 1.0]), (1.0, quarter)],
        );
        assert!(is_eighth(&t.sample(0.5).unwrap()));
        t.interpolation = Interpolation::Smooth;
        assert!(is_eighth(&t.sample(0.5).unwrap()));
        let write = |value: &[f32]| Write {
            target: String::new(),
            property: AnimationProperty::Orientation,
            value: value.to_vec(),
        };
        let mut writes = vec![write(quarter)];
        mix(&mut writes, vec![write(&[0.0, 0.0, 0.0, 1.0])], 0.5, &[]);
        assert!(is_eighth(&writes[0].value));
    }

    #[test]
    fn a_crossfade_eases_tracks_the_new_clip_leaves_back_to_rest() {
        let (mut world, door) = world_with(Animation {
            clips: vec![
                clip(
                    "lift",
                    TweenRepeat::Loop,
                    vec![track(
                        AnimationProperty::Position,
                        &[(0.0, &[0.0, 2.0, 0.0]), (1.0, &[0.0, 2.0, 0.0])],
                    )],
                ),
                clip(
                    "grow",
                    TweenRepeat::Loop,
                    vec![track(
                        AnimationProperty::Scale,
                        &[(0.0, &[2.0, 2.0, 2.0]), (1.0, &[2.0, 2.0, 2.0])],
                    )],
                ),
            ],
            autoplay: "lift".into(),
            ..Animation::default()
        });
        run(&mut world, 0..3);
        assert_eq!(world.get::<Transform>(door).unwrap().position[1], 2.0);
        world.get_mut::<Animation>(door).unwrap().command =
            Some(AnimationCommand::Crossfade("grow".into(), 1.0));
        run(&mut world, 3..8);
        let y = world.get::<Transform>(door).unwrap().position[1];
        assert!((y - 1.0).abs() < 1e-4, "{y}");
        run(&mut world, 8..14);
        assert_eq!(world.get::<Transform>(door).unwrap().position[1], 0.0);
    }

    #[test]
    fn transitions_follow_parameters_and_clip_ends() {
        let at = |name: &str, x: f32, repeat| {
            clip(
                name,
                repeat,
                vec![track(
                    AnimationProperty::Position,
                    &[(0.0, &[x, 0.0, 0.0]), (0.5, &[x, 0.0, 0.0])],
                )],
            )
        };
        let edge = |from: &str, to: &str, parameter: &str, compare, value| {
            AnimationTransition {
                from: from.into(),
                to: to.into(),
                parameter: parameter.into(),
                compare,
                value,
                ..AnimationTransition::default()
            }
        };
        let (mut world, hero) = world_with(Animation {
            clips: vec![
                at("idle", 0.0, TweenRepeat::Loop),
                at("run", 10.0, TweenRepeat::Loop),
                at("jump", 20.0, TweenRepeat::Once),
            ],
            autoplay: "idle".into(),
            transitions: vec![
                edge("", "jump", "jump", AnimationCompare::Equal, 1.0),
                edge("idle", "run", "speed", AnimationCompare::Above, 1.0),
                edge("run", "idle", "speed", AnimationCompare::Below, 1.0),
                AnimationTransition {
                    at_end: true,
                    ..edge("jump", "idle", "", AnimationCompare::Above, 0.0)
                },
            ],
            ..Animation::default()
        });
        let x =
            |world: &World| world.get::<Transform>(hero).unwrap().position[0];
        let set = |world: &mut World, name: &str, value: f32| {
            world
                .get_mut::<Animation>(hero)
                .unwrap()
                .parameters
                .insert(name.into(), value);
        };
        run(&mut world, 0..3);
        assert_eq!(x(&world), 0.0);
        set(&mut world, "speed", 4.0);
        run(&mut world, 3..4);
        assert_eq!(x(&world), 10.0);
        // "jump" stays until the clip ends, then "idle" takes over and
        // the speed sends it back to "run".
        set(&mut world, "jump", 1.0);
        run(&mut world, 4..5);
        assert_eq!(x(&world), 20.0);
        set(&mut world, "jump", 0.0);
        run(&mut world, 5..8);
        assert_eq!(x(&world), 20.0);
        run(&mut world, 8..10);
        assert_eq!(x(&world), 0.0);
        run(&mut world, 10..11);
        assert_eq!(x(&world), 10.0);
        set(&mut world, "speed", 0.0);
        run(&mut world, 11..12);
        assert_eq!(x(&world), 0.0);
    }

    #[test]
    fn blend_spaces_mix_neighbors_in_step() {
        let walk = clip(
            "walk",
            TweenRepeat::Loop,
            vec![track(
                AnimationProperty::Position,
                &[(0.0, &[0.0]), (1.0, &[1.0])],
            )],
        );
        let run = clip(
            "run",
            TweenRepeat::Loop,
            vec![track(
                AnimationProperty::Position,
                &[(0.0, &[0.0]), (0.5, &[3.0])],
            )],
        );
        let mut animation = Animation {
            clips: vec![walk, run],
            ..Animation::default()
        };
        let mut moving = clip("move", TweenRepeat::Loop, vec![]);
        moving.blend_parameter = "speed".into();
        moving.blend = vec![
            BlendPoint {
                clip: "run".into(),
                at: 2.0,
                at_y: 0.0,
            },
            BlendPoint {
                clip: "walk".into(),
                at: 0.0,
                at_y: 0.0,
            },
        ];
        assert_eq!(moving.length(), 1.0);
        let x =
            |animation: &Animation| pose(animation, &moving, 0.5)[0].value[0];
        // Halfway through the cycle: walk is at 0.5, run (stretched to
        // 1 s) at 1.5.
        animation.parameters.insert("speed".into(), 1.0);
        assert_eq!(x(&animation), 1.0);
        animation.parameters.insert("speed".into(), 0.0);
        assert_eq!(x(&animation), 0.5);
        animation.parameters.insert("speed".into(), 9.0);
        assert_eq!(x(&animation), 1.5);
    }

    #[test]
    fn two_d_blend_spaces_play_points_alone_and_mix_between() {
        let still = |name: &str, x: f32| {
            clip(
                name,
                TweenRepeat::Loop,
                vec![track(AnimationProperty::Position, &[(0.0, &[x])])],
            )
        };
        let mut animation = Animation {
            clips: vec![
                still("idle", 0.0),
                still("run", 4.0),
                still("strafe", 2.0),
            ],
            ..Animation::default()
        };
        let mut moving = clip("move", TweenRepeat::Loop, vec![]);
        moving.blend_parameter = "x".into();
        moving.blend_parameter_y = "y".into();
        let point = |clip: &str, at: f32, at_y: f32| BlendPoint {
            clip: clip.into(),
            at,
            at_y,
        };
        moving.blend = vec![
            point("idle", 0.0, 0.0),
            point("run", 0.0, 1.0),
            point("strafe", 1.0, 0.0),
        ];
        let mut x = |at: f32, at_y: f32| {
            animation.parameters.insert("x".into(), at);
            animation.parameters.insert("y".into(), at_y);
            pose(&animation, &moving, 0.0)[0].value[0]
        };
        assert_eq!(x(0.0, 0.0), 0.0);
        assert_eq!(x(0.0, 1.0), 4.0);
        assert_eq!(x(1.0, 0.0), 2.0);
        // Halfway to run: idle and run share the weight; strafe has none.
        assert!((x(0.0, 0.5) - 2.0).abs() < 1e-6);
        // Past run on its own axis, run plays alone.
        assert_eq!(x(0.0, 3.0), 4.0);
        let between = x(0.3, 0.3);
        assert!(between > 0.0 && between < 4.0);
    }

    #[test]
    fn layers_override_masked_tracks_and_add_motion() {
        let at = |target: &str, track: AnimationTrack| AnimationTrack {
            target: target.into(),
            ..track
        };
        let position =
            |keys: &[(f32, &[f32])]| track(AnimationProperty::Position, keys);
        let walk = clip(
            "walk",
            TweenRepeat::Loop,
            vec![
                position(&[(0.0, &[1.0, 0.0, 0.0]), (2.0, &[1.0, 0.0, 0.0])]),
                at("Body/Arm", position(&[(0.0, &[0.0, 0.0, 0.0])])),
                at("Head", position(&[(0.0, &[0.0, 5.0, 0.0])])),
                at(
                    "Head",
                    track(
                        AnimationProperty::Orientation,
                        &[(0.0, &[0.0, 0.70710677, 0.0, 0.70710677])],
                    ),
                ),
            ],
        );
        let wave = clip(
            "wave",
            TweenRepeat::Loop,
            vec![
                position(&[(0.0, &[9.0, 9.0, 9.0])]),
                at("Body/Arm", position(&[(0.0, &[4.0, 0.0, 0.0])])),
            ],
        );
        let nod = clip(
            "nod",
            TweenRepeat::Loop,
            vec![
                at(
                    "Head",
                    position(&[
                        (0.0, &[0.0, 0.0, 0.0]),
                        (1.0, &[0.0, 2.0, 0.0]),
                    ]),
                ),
                at(
                    "Head",
                    track(
                        AnimationProperty::Orientation,
                        &[
                            (0.0, &[0.0, 0.0, 0.0, 1.0]),
                            (1.0, &[0.70710677, 0.0, 0.0, 0.70710677]),
                        ],
                    ),
                ),
            ],
        );
        let mut animation = Animation {
            clips: vec![walk, wave, nod],
            autoplay: "walk".into(),
            layers: vec![
                AnimationLayer {
                    clip: "wave".into(),
                    weight: 0.5,
                    mask: vec!["Body".into()],
                    ..AnimationLayer::default()
                },
                AnimationLayer {
                    clip: "nod".into(),
                    weight_parameter: "nod".into(),
                    additive: true,
                    ..AnimationLayer::default()
                },
            ],
            ..Animation::default()
        };
        animation.parameters.insert("nod".into(), 1.0);
        let mut player = AnimationPlayer::default();
        // Half a second in: nod is half way, a quarter turn about x.
        let (writes, _) = advance(&animation, &mut player, None, 0.5);
        let value = |target: &str, property: AnimationProperty| {
            writes
                .iter()
                .find(|w| w.target == target && w.property == property)
                .map(|w| w.value.clone())
                .unwrap()
        };
        // The mask keeps the root; the arm is half way to the wave.
        assert_eq!(value("", AnimationProperty::Position), [1.0, 0.0, 0.0]);
        assert_eq!(
            value("Body/Arm", AnimationProperty::Position),
            [2.0, 0.0, 0.0]
        );
        assert_eq!(value("Head", AnimationProperty::Position), [0.0, 6.0, 0.0]);
        // Additive turn in the head's own frame: base (90° about y) then
        // the nod's sampled turn about x, which is the head's local x.
        let q = value("Head", AnimationProperty::Orientation);
        let q = nalgebra::UnitQuaternion::from_quaternion(
            nalgebra::Quaternion::new(q[3], q[0], q[1], q[2]),
        );
        let base = nalgebra::UnitQuaternion::from_euler_angles(
            0.0,
            std::f32::consts::FRAC_PI_2,
            0.0,
        );
        // Linear keys blend quaternions per component, so half way is
        // not exactly half the angle; compare against the sampled value.
        let sampled = nalgebra::UnitQuaternion::from_quaternion(
            nalgebra::Quaternion::new(
                0.5 + 0.5 * 0.70710677,
                0.5 * 0.70710677,
                0.0,
                0.0,
            ),
        );
        assert!((q.angle_to(&(base * sampled))) < 1e-5);
        // Weight 0 turns the additive layer off.
        animation.parameters.insert("nod".into(), 0.0);
        let (writes, _) = advance(&animation, &mut player, None, 0.5);
        let head = writes
            .iter()
            .find(|w| {
                w.target == "Head" && w.property == AnimationProperty::Position
            })
            .unwrap();
        assert_eq!(head.value, [0.0, 5.0, 0.0]);
    }

    #[test]
    fn root_motion_moves_the_object_by_policy_and_holds_the_root_bone() {
        let mut stride = track(
            AnimationProperty::Position,
            &[(0.0, &[0.0, 1.0, 0.0]), (1.0, &[2.0, 1.5, 0.0])],
        );
        stride.target = "Hips".into();
        let animation = |policy| Animation {
            clips: vec![clip("walk", TweenRepeat::Loop, vec![stride.clone()])],
            autoplay: "walk".into(),
            root_motion: policy,
            root_bone: "Hips".into(),
            ..Animation::default()
        };
        let turned = Transform {
            rotation: [0.0, std::f32::consts::FRAC_PI_2, 0.0],
            ..Transform::default()
        };
        // Eleven 0.1 s ticks after the player starts: 1.1 s, one wrap.
        let (mut world, hero) = world_with(animation(RootMotion::Transform));
        *world.get_mut::<Transform>(hero).unwrap() = turned;
        run(&mut world, 0..12);
        let position = world.get::<Transform>(hero).unwrap().position;
        // Local +x turned a quarter about y is world -z.
        assert!(position[0].abs() < 1e-4, "{position:?}");
        assert!(position[1].abs() < 1e-6);
        assert!((position[2] + 2.2).abs() < 1e-4, "{position:?}");

        let (mut world, hero) = world_with(animation(RootMotion::InPlace));
        run(&mut world, 0..12);
        assert_eq!(world.get::<Transform>(hero).unwrap().position, [0.0; 3]);
        let mut player = world.get_mut::<AnimationPlayer>(hero).unwrap();
        let taken = std::mem::take(&mut player.root_motion);
        assert!((taken[0] - 2.2).abs() < 1e-4 && taken[1] == 0.0);

        // The root bone keeps its first-frame x and z but still bobs.
        let mut animation = animation(RootMotion::InPlace);
        let mut player = AnimationPlayer::default();
        let (writes, _) = advance(&animation, &mut player, None, 0.5);
        assert_eq!(writes[0].value, [0.0, 1.25, 0.0]);

        animation.root_motion = RootMotion::Velocity;
        let (mut world, hero) = world_with(animation);
        *world.get_mut::<Transform>(hero).unwrap() = turned;
        let id = super::super::PhysicsId {
            slot: 3,
            generation: 1,
        };
        world.entity_mut(hero).insert(id);
        world.insert_resource(super::super::GpuPhysicsCommands::default());
        run(&mut world, 0..2);
        let commands = &world.resource::<super::super::GpuPhysicsCommands>();
        let [(
            body,
            super::super::GpuBodyCommand::SetVelocity { linear, angular },
        )] = commands.commands.as_slice()
        else {
            panic!("{:?}", commands.commands);
        };
        assert_eq!(*body, id);
        assert_eq!(*angular, [0.0; 3]);
        assert!(linear[0].abs() < 1e-4 && (linear[2] + 2.0).abs() < 1e-4);
        assert_eq!(world.get::<Transform>(hero).unwrap().position, [0.0; 3]);
    }

    #[test]
    fn color_tracks_recolor_only_their_own_copy() {
        let glow = clip(
            "glow",
            TweenRepeat::Loop,
            vec![track(
                AnimationProperty::Color,
                &[(0.0, &[1.0, 0.0, 0.0, 1.0])],
            )],
        );
        let (mut world, door) = world_with(Animation {
            clips: vec![glow],
            autoplay: "glow".into(),
            ..Animation::default()
        });
        let mut assets = AssetServer::default();
        let shared = assets.materials.insert(MaterialAsset::default());
        let mesh =
            assets.builtin_primitive(crate::assets::PrimitiveShape::Cube);
        world.insert_resource(assets);
        let renderer = MeshRenderer {
            mesh,
            material: shared,
            cast_shadows: true,
            receive_shadows: true,
        };
        world.entity_mut(door).insert(renderer);
        let other = world.spawn(renderer).id();
        run(&mut world, 0..4);
        let assets = world.resource::<AssetServer>();
        let own = world.get::<MeshRenderer>(door).unwrap().material;
        assert_ne!(own, shared);
        assert_eq!(
            assets.materials.get(own).unwrap().base_color,
            [1.0, 0.0, 0.0, 1.0]
        );
        assert_eq!(world.get::<MeshRenderer>(other).unwrap().material, shared);
        let revision = assets.materials.revision(own);
        run(&mut world, 4..8);
        assert_eq!(
            world.resource::<AssetServer>().materials.revision(own),
            revision,
            "unchanged values do not re-upload"
        );
        assert_eq!(world.get::<MeshRenderer>(door).unwrap().material, own);
    }

    #[test]
    fn child_targets_and_visibility() {
        let blink = clip(
            "blink",
            TweenRepeat::Loop,
            vec![AnimationTrack {
                target: "Lamp".into(),
                interpolation: Interpolation::Step,
                ..track(
                    AnimationProperty::Visible,
                    &[(0.0, &[0.0]), (1.0, &[1.0])],
                )
            }],
        );
        let (mut world, door) = world_with(Animation {
            clips: vec![blink],
            autoplay: "blink".into(),
            ..Animation::default()
        });
        let lamp = world.spawn(Name("Lamp".into())).id();
        world.entity_mut(door).insert(Children(vec![lamp]));
        run(&mut world, 0..3);
        assert_eq!(
            world.get::<Visibility>(lamp),
            Some(&Visibility { visible: false })
        );
    }

    #[test]
    fn same_steps_give_the_same_pose() {
        let wobble = clip(
            "w",
            TweenRepeat::PingPong,
            vec![AnimationTrack {
                interpolation: Interpolation::Smooth,
                ..track(
                    AnimationProperty::Rotation,
                    &[
                        (0.0, &[0.0, 0.0, 0.0]),
                        (0.7, &[1.0, 2.0, 0.0]),
                        (1.3, &[0.0, -1.0, 3.0]),
                    ],
                )
            }],
        );
        let pose = || {
            let (mut world, door) = world_with(Animation {
                clips: vec![wobble.clone()],
                autoplay: "w".into(),
                speed: 1.7,
                ..Animation::default()
            });
            run(&mut world, 0..97);
            world
                .get::<Transform>(door)
                .unwrap()
                .rotation
                .map(f32::to_bits)
        };
        assert_eq!(pose(), pose());
    }
}
