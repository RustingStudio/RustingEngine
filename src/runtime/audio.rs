//! Sound requests. Presentation only: nothing here reads or writes
//! simulation state, and the queue is not part of state hashes.
//!
//! Game code and [`SoundCue`](super::SoundCue)s push [`AudioCommand`]s onto
//! the [`AudioQueue`]. The windowed runner plays them through the audio
//! device each frame (`audio` feature). Headless runs and `rusting test`
//! have no device: nothing drains the queue (it keeps the newest
//! [`QUEUE_LIMIT`] commands), and [`AudioQueue::requested`] still counts
//! sounds that game code asked for. The windowed runner and scenarios turn
//! `SoundCue` events into requests; scenarios read them through the
//! `audio:` entity (`/requested`, `/clips/<clip>`).
//!
//! [`route_sound_events`] also moves sounds that follow an entity, picks the
//! listener, raycasts occlusion and advances caption clocks. It only reads
//! the world, so turning audio off leaves replay hashes unchanged.

use bevy_ecs::prelude::{Entity, Resource, World};

use super::{EventQueue, SoundEvent};

/// Beats counted on fixed ticks with integer math, so a beat lands on the
/// same tick every run: beat `k` starts on the first tick at or after `k`
/// beats of real time. At 60 ticks a second, 120 BPM is every 30 ticks and
/// 128 BPM alternates 28 and 29.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BeatClock {
    pub bpm: u32,
    /// Tick of beat 0.
    pub start_tick: u64,
    /// Fixed ticks in one minute: 3600 at 60 ticks a second.
    pub ticks_per_minute: u64,
}

impl BeatClock {
    /// `fixed_delta` is [`FrameTime::fixed_delta`](super::FrameTime).
    #[must_use]
    pub fn new(
        bpm: u32,
        start_tick: u64,
        fixed_delta: std::time::Duration,
    ) -> Self {
        Self {
            bpm: bpm.max(1),
            start_tick,
            ticks_per_minute: (60.0 / fixed_delta.as_secs_f64()).round() as u64,
        }
    }

    /// Tick beat `beat` starts on.
    #[must_use]
    pub fn tick_of(&self, beat: u64) -> u64 {
        let bpm = u64::from(self.bpm);
        self.start_tick + (beat * self.ticks_per_minute).div_ceil(bpm)
    }

    /// The beat playing on `tick`; `None` before beat 0.
    #[must_use]
    pub fn beat_at(&self, tick: u64) -> Option<u64> {
        let since = tick.checked_sub(self.start_tick)?;
        Some(since * u64::from(self.bpm) / self.ticks_per_minute)
    }

    /// Whether a beat starts on `tick`.
    #[must_use]
    pub fn is_beat(&self, tick: u64) -> bool {
        self.beat_at(tick)
            .is_some_and(|beat| self.tick_of(beat) == tick)
    }

    /// Ticks from `tick` until the next beat starts, 1 or more.
    #[must_use]
    pub fn ticks_to_next(&self, tick: u64) -> u64 {
        let next = self.beat_at(tick).map_or(0, |beat| beat + 1);
        self.tick_of(next) - tick
    }

    /// How far `tick` is into its beat, from 0 up to 1.
    #[must_use]
    pub fn phase(&self, tick: u64) -> f64 {
        let Some(since) = tick.checked_sub(self.start_tick) else {
            return 0.0;
        };
        let scaled = since * u64::from(self.bpm);
        (scaled % self.ticks_per_minute) as f64 / self.ticks_per_minute as f64
    }

    /// Beats that start after tick `after` and up to tick `upto`. An update
    /// that ran several fixed ticks loops over all of them.
    #[must_use]
    pub fn beats_between(&self, after: u64, upto: u64) -> std::ops::Range<u64> {
        let first = self.beat_at(after).map_or(0, |beat| beat + 1);
        let end = self.beat_at(upto).map_or(0, |beat| beat + 1);
        first..end.max(first)
    }
}

/// Commands kept when nothing drains the queue (headless runs).
pub const QUEUE_LIMIT: usize = 1024;

/// Metres per second, for doppler.
const SPEED_OF_SOUND: f32 = 343.0;

/// Voices a bus plays at once until [`AudioQueue::set_bus_voice_limit`].
pub const DEFAULT_VOICE_LIMIT: usize = 64;

/// Highest voice limit a bus takes; kira holds this many sounds per track.
pub const MAX_VOICE_LIMIT: usize = 256;

/// How a sound plays: [`GameScene::play_sound_with`](crate::project_runner::GameScene::play_sound_with).
#[derive(Clone, Debug, PartialEq)]
pub struct Sound {
    /// Linear gain, 1 is the clip's own level.
    pub volume: f32,
    /// -1 is the left speaker only, 0 both, 1 the right only. Constant
    /// power: each side plays at 1 at pan 0, and the near side at √2
    /// (+3 dB) at pan ±1, so a hard-panned sound is louder on its side.
    /// Scenarios see the result as `gain` under `audio:/playing`.
    pub pan: f32,
    /// Bus the sound plays on (`"music"`, `"sfx"`); empty is the main
    /// output. A bus is created the first time it is named.
    pub bus: String,
    pub looped: bool,
    /// Fixed tick the sound starts on. `None` is the tick it was asked for
    /// on. A later tick schedules it, so it starts exactly then.
    pub at_tick: Option<u64>,
    /// World position. The listener (see [`AudioQueue::set_listener`])
    /// hears it from its side, and its volume falls as `2 / distance` past
    /// 2 m. Replaces `pan`.
    pub position: Option<[f32; 3]>,
    /// Playback speed, like a tape: 0.5 plays half as fast and an octave
    /// lower, 2 twice as fast and an octave higher. 1 is the clip as
    /// recorded.
    pub rate: f32,
    /// Plays the clip from its end to its start.
    pub reverse: bool,
    /// Follows this entity's world position every frame; sets `position`.
    pub follow: Option<Entity>,
    /// Muffles the sound and lowers its volume while a collider is between
    /// the listener and the sound. Needs `position` or `follow`.
    pub occlude: bool,
    /// When a bus is at its voice limit, a new sound replaces the playing
    /// one with the lowest priority, then the quietest (after distance and
    /// occlusion); on a tie the oldest goes. A new sound that does not rank
    /// strictly higher is dropped. Compared within one bus only. 128 by
    /// default.
    pub priority: u8,
    /// Subtitle lines shown while the sound plays, timed in seconds of the
    /// clip (they follow `rate`, pauses and seeks).
    pub captions: Vec<Caption>,
    /// Doppler strength for a positioned sound: 1 raises its pitch as it
    /// closes on the listener and lowers it as it moves away, like a real
    /// passing siren; 0.5 half as much; 0 never. Comes from how fast the
    /// distance changes, so either side moving counts.
    pub doppler: f32,
    /// How a positioned sound's volume falls with distance.
    pub falloff: Falloff,
}

/// How a positioned sound's volume falls with distance from the listener.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Falloff {
    /// Full volume within `near` metres, then `near / distance`, like a
    /// real point source. The default, with `near` 2.
    Inverse { near: f32 },
    /// Full within `near`, then `(near / distance)²`: dies out faster,
    /// for small sounds like a ticking clock.
    InverseSquare { near: f32 },
    /// Full within `near`, silent past `far`, a straight line between.
    Linear { near: f32, far: f32 },
    /// Same volume at any distance; the sound still pans.
    Off,
}

impl Default for Falloff {
    fn default() -> Self {
        Self::Inverse { near: 2.0 }
    }
}

impl Falloff {
    /// Volume factor at `distance` metres, 0 to 1.
    #[must_use]
    pub fn gain(self, distance: f32) -> f32 {
        match self {
            Self::Inverse { near } => (near / distance).min(1.0),
            Self::InverseSquare { near } => (near / distance).min(1.0).powi(2),
            Self::Linear { near, far: _ } if distance <= near => 1.0,
            Self::Linear { near, far } => {
                (1.0 - (distance - near) / (far - near).max(1e-6)).max(0.0)
            }
            Self::Off => 1.0,
        }
    }
}

impl Default for Sound {
    fn default() -> Self {
        Self {
            volume: 1.0,
            pan: 0.0,
            bus: String::new(),
            looped: false,
            at_tick: None,
            position: None,
            rate: 1.0,
            reverse: false,
            follow: None,
            occlude: false,
            priority: 128,
            captions: Vec::new(),
            doppler: 0.0,
            falloff: Falloff::default(),
        }
    }
}

/// One subtitle line: `text` shows from `start` to `end` seconds into the
/// clip.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Caption {
    pub start: f32,
    pub end: f32,
    pub text: String,
}

impl Caption {
    #[must_use]
    pub fn new(start: f32, end: f32, text: impl Into<String>) -> Self {
        Self {
            start,
            end,
            text: text.into(),
        }
    }
}

/// Whether the HUD shows captions, and their text size in logical pixels.
/// Presentation only: a settings screen writes it.
#[derive(Resource, Clone, Debug, PartialEq)]
pub struct CaptionSettings {
    pub enabled: bool,
    pub size: f32,
}

impl Default for CaptionSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            size: 22.0,
        }
    }
}

/// An effect on a bus. Each bus has one of each kind, off until set; set
/// `mix` to 0 (or `cutoff_hz` to 20 000 or more) to turn one off again.
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize)]
#[serde(tag = "kind")]
pub enum BusEffect {
    /// Removes sound above `cutoff_hz`: a wall or a closed door.
    LowPass { cutoff_hz: f32 },
    /// A room: `room` 0 is small and dry, near 1 rings for seconds;
    /// `damping` 0..1 darkens the tail; `mix` 0..1 is the wet share.
    Reverb { room: f32, damping: f32, mix: f32 },
    /// Soft clipping, like tape saturation: `drive` is decibels of gain
    /// into the clipper (0..24); `mix` 0..1 is the wet share.
    Distortion { drive: f32, mix: f32 },
    /// Three-band equalizer, gains in decibels (-24..24): a low shelf
    /// below 200 Hz, a bell at 1 kHz and a high shelf above 5 kHz. All 0
    /// is off.
    Eq {
        low_db: f32,
        mid_db: f32,
        high_db: f32,
    },
    /// Turns loud parts down: sound above `threshold_db` (dBFS, 0 or less)
    /// rises only 1/`ratio` as fast; `makeup_db` raises the result. A ratio
    /// of 1 is off; 20 or more acts as a limiter.
    Compressor {
        threshold_db: f32,
        ratio: f32,
        makeup_db: f32,
    },
}

impl BusEffect {
    /// Whether the effect changes the sound at all.
    #[must_use]
    pub fn active(&self) -> bool {
        match *self {
            Self::LowPass { cutoff_hz } => cutoff_hz < 20_000.0,
            Self::Reverb { mix, .. } | Self::Distortion { mix, .. } => {
                mix > 0.0
            }
            Self::Eq {
                low_db,
                mid_db,
                high_db,
            } => [low_db, mid_db, high_db] != [0.0; 3],
            Self::Compressor { ratio, .. } => ratio > 1.0,
        }
    }
}

/// Names one playing sound so it can be stopped later.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SoundId(pub u64);

/// A sound game code started that has not ended, from
/// [`AudioQueue::playing`].
#[derive(Clone, Debug, PartialEq)]
pub struct ActiveSound {
    pub id: SoundId,
    pub clip: String,
    /// Bus it plays on; empty is the main output.
    pub bus: String,
    pub looped: bool,
    pub paused: bool,
}

/// One request for the audio device.
#[derive(Clone, Debug, PartialEq)]
pub enum AudioCommand {
    Play {
        id: SoundId,
        /// Path relative to the project's `assets` folder.
        clip: String,
        /// Fixed tick the sound starts on.
        tick: u64,
        /// `pan` is clamped to -1..1; `follow` and `captions` are handled
        /// before the device sees them.
        sound: Sound,
    },
    Stop(SoundId),
    /// Moves a playing sound's gain to `volume` over `fade` seconds.
    SetVolume {
        id: SoundId,
        volume: f32,
        fade: f32,
    },
    /// Moves a playing sound's speed and pitch to `rate` over `fade`
    /// seconds.
    SetRate {
        id: SoundId,
        rate: f32,
        fade: f32,
    },
    /// Moves a positioned sound.
    SetPosition {
        id: SoundId,
        position: [f32; 3],
    },
    /// How blocked a sound is, 0 clear to 1 behind a wall.
    SetOcclusion {
        id: SoundId,
        amount: f32,
    },
    /// Where positioned sounds are heard from.
    SetListener {
        position: [f32; 3],
        /// Unit vector to the listener's right.
        right: [f32; 3],
    },
    Pause(SoundId),
    Resume(SoundId),
    /// Jumps to `seconds` into the clip.
    Seek {
        id: SoundId,
        seconds: f32,
    },
    /// Moves a bus's gain to `volume` over `fade` seconds.
    SetBusVolume {
        bus: String,
        volume: f32,
        fade: f32,
    },
    /// Sets one of a bus's effects over `fade` seconds.
    SetBusEffect {
        bus: String,
        effect: BusEffect,
        fade: f32,
    },
    /// Silences a bus without forgetting its volume.
    SetBusMute {
        bus: String,
        muted: bool,
    },
    /// While any named bus is soloed, only soloed buses play. The main
    /// output `""` is never silenced by solo.
    SetBusSolo {
        bus: String,
        solo: bool,
    },
    /// Multiplies a sound's rate by a doppler pitch factor.
    SetDoppler {
        id: SoundId,
        pitch: f32,
    },
    /// Most sounds the bus plays at once, 1 to [`MAX_VOICE_LIMIT`].
    SetBusVoiceLimit {
        bus: String,
        limit: usize,
    },
    StopAll,
    /// Linear gain applied to every sound.
    SetMasterVolume(f32),
}

/// A sound the queue keeps updating after it starts: it follows an entity,
/// is occluded, or has captions.
#[derive(Clone, Debug)]
struct Tracked {
    follow: Option<Entity>,
    position: Option<[f32; 3]>,
    occlude: bool,
    occlusion: f32,
    captions: Vec<Caption>,
    /// Fixed tick the sound starts on.
    start: u64,
    /// Seconds into the clip, for captions.
    clock: f64,
    rate: f32,
    paused: bool,
    doppler: f32,
    /// Distance from the listener last tick, for doppler.
    distance: Option<f32>,
    /// Doppler pitch factor last sent.
    pitch: f32,
}

/// Gives a bus a room's reverb while the listener is inside this entity's
/// collider (usually a sensor): a cave, a hall, a bathroom. Where zones
/// overlap, the one first in entity order wins. Leaving turns the reverb
/// off again, so a zone owns the reverb on its bus.
#[derive(
    bevy_ecs::component::Component,
    Clone,
    Debug,
    PartialEq,
    serde::Serialize,
    serde::Deserialize,
)]
#[serde(default)]
pub struct ReverbZone {
    /// Bus the reverb goes on; empty is every sound.
    pub bus: String,
    /// 0 small and dry, near 1 rings for seconds.
    pub room: f32,
    /// 0..1, darkens the tail.
    pub damping: f32,
    /// 0..1 wet share.
    pub mix: f32,
    /// Seconds the reverb takes to come in and go out.
    pub fade: f32,
}

impl Default for ReverbZone {
    fn default() -> Self {
        Self {
            bus: String::new(),
            room: 0.8,
            damping: 0.5,
            mix: 0.4,
            fade: 0.5,
        }
    }
}

impl ReverbZone {
    fn effect(&self, mix: f32) -> BusEffect {
        BusEffect::Reverb {
            room: self.room,
            damping: self.damping,
            mix,
        }
    }
}

/// Loops a clip on this body while it slides or rolls against another
/// collider: a scraping crate, a rolling barrel. Volume follows the speed
/// across the contact. CPU bodies only.
#[derive(
    bevy_ecs::component::Component,
    Clone,
    Debug,
    PartialEq,
    serde::Serialize,
    serde::Deserialize,
)]
#[serde(default)]
pub struct SlideSound {
    pub clip: String,
    /// Linear gain at `full_volume_speed` and above.
    pub volume: f32,
    /// Sliding speed in m/s that plays at full `volume`.
    pub full_volume_speed: f32,
    /// Slower than this (m/s) is silent.
    pub min_speed: f32,
    /// Empty is the main output.
    pub bus: String,
    /// Clips for sliding on a named `PhysicsMaterial` (or heightfield or
    /// triangle surface); other surfaces play `clip`. An empty clip is
    /// silent there.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub by_material: Vec<SurfaceClip>,
    /// The loop while it plays.
    #[serde(skip)]
    pub playing: Option<SoundId>,
    /// The clip that loop plays.
    #[serde(skip)]
    pub playing_clip: String,
    /// Volume last sent.
    #[serde(skip)]
    pub level: f32,
}

impl Default for SlideSound {
    fn default() -> Self {
        Self {
            clip: String::new(),
            volume: 1.0,
            full_volume_speed: 4.0,
            min_speed: 0.2,
            bus: String::new(),
            by_material: Vec::new(),
            playing: None,
            playing_clip: String::new(),
            level: 0.0,
        }
    }
}

/// One [`SlideSound::by_material`] entry.
#[derive(
    Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize,
)]
#[serde(default)]
pub struct SurfaceClip {
    pub material: String,
    pub clip: String,
}

/// Starts, turns and stops each [`SlideSound`] loop from the last fixed
/// step's contacts.
fn route_slide_sounds(world: &mut World, tick: u64) {
    let slides: Vec<(Entity, SlideSound)> = world
        .query::<(Entity, &SlideSound)>()
        .iter(world)
        .map(|(entity, slide)| (entity, slide.clone()))
        .collect();
    if slides.is_empty() {
        return;
    }
    let velocity = |entity| {
        world
            .get::<super::RigidBody>(entity)
            .map_or(nalgebra::Vector3::zeros(), |body| {
                body.linear_velocity.into()
            })
    };
    // Fastest speed across a touching contact, per body, and the material
    // name of what it slides on there (the first contact wins a tie).
    let mut speeds =
        std::collections::BTreeMap::<Entity, (f32, Option<String>)>::new();
    if let Some(physics) = world.get_resource::<super::PhysicsWorld>() {
        for contact in physics.contacts().iter().filter(|c| !c.sensor) {
            let normal = nalgebra::Vector3::from(contact.normal);
            let relative = velocity(contact.a) - velocity(contact.b);
            let slide = (relative - normal * relative.dot(&normal)).norm();
            for (body, other) in
                [(contact.a, contact.b), (contact.b, contact.a)]
            {
                let fastest = speeds.entry(body).or_default();
                if slide > fastest.0 {
                    let name = super::game_feel::surface_name(
                        contact.surface,
                        world.get(other),
                        world.get(other),
                        world.get(other),
                    );
                    *fastest = (slide, name.map(str::to_owned));
                }
            }
        }
    }
    let mut changed = Vec::new();
    let Some(mut queue) = world.get_resource_mut::<AudioQueue>() else {
        return;
    };
    for (entity, slide) in slides {
        let (speed, material) = speeds.remove(&entity).unwrap_or_default();
        let clip = material
            .and_then(|name| {
                slide
                    .by_material
                    .iter()
                    .find(|entry| entry.material == name)
            })
            .map_or(&slide.clip, |entry| &entry.clip);
        let level = if speed < slide.min_speed || clip.is_empty() {
            0.0
        } else {
            slide.volume * (speed / slide.full_volume_speed.max(1e-3)).min(1.0)
        };
        let mut next = slide.clone();
        if let Some(id) = slide.playing {
            if level == 0.0 || *clip != slide.playing_clip {
                queue.stop(id);
                next.playing = None;
                next.level = 0.0;
            } else if (level - slide.level).abs() > 0.01 {
                queue.set_volume(id, level, SLIDE_FADE_SECONDS);
                next.level = level;
            }
        }
        if next.playing.is_none() && level > 0.0 {
            let sound = Sound {
                volume: level,
                bus: slide.bus.clone(),
                looped: true,
                follow: Some(entity),
                ..Sound::default()
            };
            next.playing = Some(queue.play(clip, &sound, tick));
            next.playing_clip.clone_from(clip);
            next.level = level;
        }
        if next != slide {
            changed.push((entity, next));
        }
    }
    for (entity, slide) in changed {
        world.entity_mut(entity).insert(slide);
    }
}

/// How fast a slide loop follows a change in speed.
const SLIDE_FADE_SECONDS: f32 = 0.05;

/// Requests waiting for the audio device, plus the presentation state that
/// follows sounds after they start (listener, attached sounds, captions).
/// Nothing here feeds the simulation.
#[derive(Resource, Clone, Debug, Default)]
pub struct AudioQueue {
    commands: Vec<AudioCommand>,
    next_id: u64,
    requested: u64,
    /// Plays requested per clip path, never reset.
    clips: std::collections::BTreeMap<String, u64>,
    /// Entity sounds are heard from; `None` is the active camera.
    listener: Option<Entity>,
    /// Last listener sent to the device.
    heard_from: Option<([f32; 3], [f32; 3])>,
    /// Reverb zone the listener was last inside.
    reverb_zone: Option<(Entity, ReverbZone)>,
    tracked: std::collections::BTreeMap<SoundId, Tracked>,
    /// Every sound started and not yet ended, stopped or dropped.
    active: std::collections::BTreeMap<SoundId, ActiveSound>,
    /// Fixed tick captions were last advanced to.
    caption_tick: u64,
}

impl AudioQueue {
    /// Asks for a clip to start on fixed tick `tick` (or `sound.at_tick`).
    /// Returns the ID that stops it.
    pub fn play(&mut self, clip: &str, sound: &Sound, tick: u64) -> SoundId {
        let id = SoundId(self.next_id);
        self.next_id += 1;
        self.requested += 1;
        *self.clips.entry(clip.to_owned()).or_default() += 1;
        let start = sound.at_tick.unwrap_or(tick);
        self.active.insert(
            id,
            ActiveSound {
                id,
                clip: clip.to_owned(),
                bus: sound.bus.clone(),
                looped: sound.looped,
                paused: false,
            },
        );
        if sound.follow.is_some()
            || ((sound.occlude || sound.doppler > 0.0)
                && sound.position.is_some())
            || !sound.captions.is_empty()
        {
            self.tracked.insert(
                id,
                Tracked {
                    follow: sound.follow,
                    position: sound.position,
                    occlude: sound.occlude,
                    occlusion: 0.0,
                    captions: sound.captions.clone(),
                    start,
                    clock: 0.0,
                    rate: sound.rate,
                    paused: false,
                    doppler: sound.doppler,
                    distance: None,
                    pitch: 1.0,
                },
            );
        }
        let mut sound = sound.clone();
        sound.pan = sound.pan.clamp(-1.0, 1.0);
        sound.captions.clear();
        self.push(AudioCommand::Play {
            id,
            clip: clip.to_string(),
            tick: start,
            sound,
        });
        id
    }

    fn push(&mut self, command: AudioCommand) {
        if self.commands.len() >= QUEUE_LIMIT {
            self.commands.remove(0);
        }
        self.commands.push(command);
    }

    pub fn stop(&mut self, id: SoundId) {
        self.tracked.remove(&id);
        self.active.remove(&id);
        self.push(AudioCommand::Stop(id));
    }

    pub fn stop_all(&mut self) {
        self.tracked.clear();
        self.active.clear();
        self.push(AudioCommand::StopAll);
    }

    pub fn set_master_volume(&mut self, volume: f32) {
        self.push(AudioCommand::SetMasterVolume(volume));
    }

    pub fn set_volume(&mut self, id: SoundId, volume: f32, fade: f32) {
        self.push(AudioCommand::SetVolume { id, volume, fade });
    }

    /// Changes speed and pitch together, like a tape, over `fade` seconds.
    pub fn set_rate(&mut self, id: SoundId, rate: f32, fade: f32) {
        if let Some(tracked) = self.tracked.get_mut(&id) {
            tracked.rate = rate;
        }
        self.push(AudioCommand::SetRate { id, rate, fade });
    }

    /// Moves a positioned sound. A sound that follows an entity moves
    /// back to it next frame.
    pub fn set_position(&mut self, id: SoundId, position: [f32; 3]) {
        if let Some(tracked) = self.tracked.get_mut(&id) {
            tracked.position = Some(position);
        }
        self.push(AudioCommand::SetPosition { id, position });
    }

    pub fn pause(&mut self, id: SoundId) {
        if let Some(tracked) = self.tracked.get_mut(&id) {
            tracked.paused = true;
        }
        if let Some(active) = self.active.get_mut(&id) {
            active.paused = true;
        }
        self.push(AudioCommand::Pause(id));
    }

    pub fn resume(&mut self, id: SoundId) {
        if let Some(tracked) = self.tracked.get_mut(&id) {
            tracked.paused = false;
        }
        if let Some(active) = self.active.get_mut(&id) {
            active.paused = false;
        }
        self.push(AudioCommand::Resume(id));
    }

    /// Pauses every playing sound on `bus`, or on every bus with `None`.
    pub fn pause_bus(&mut self, bus: Option<&str>) {
        for id in self.on_bus(bus, false) {
            self.pause(id);
        }
    }

    /// Resumes every paused sound on `bus`, or on every bus with `None`.
    pub fn resume_bus(&mut self, bus: Option<&str>) {
        for id in self.on_bus(bus, true) {
            self.resume(id);
        }
    }

    fn on_bus(&self, bus: Option<&str>, paused: bool) -> Vec<SoundId> {
        self.active
            .values()
            .filter(|sound| sound.paused == paused)
            .filter(|sound| bus.is_none_or(|bus| sound.bus == bus))
            .map(|sound| sound.id)
            .collect()
    }

    /// Sounds started and not yet ended, oldest first. Ended means the
    /// audio device finished, stopped or dropped them. With no device a
    /// sound stays listed until it is stopped.
    #[must_use]
    pub fn playing(&self) -> Vec<ActiveSound> {
        self.active.values().cloned().collect()
    }

    /// Jumps to `seconds` into the clip.
    pub fn seek(&mut self, id: SoundId, seconds: f32) {
        if let Some(tracked) = self.tracked.get_mut(&id) {
            tracked.clock = f64::from(seconds.max(0.0));
        }
        self.push(AudioCommand::Seek { id, seconds });
    }

    /// Hears positioned sounds from `entity` instead of the active camera;
    /// `None` goes back to the camera. A booth player keeps hearing from
    /// the booth while the screen shows a monitor camera.
    pub fn set_listener(&mut self, entity: Option<Entity>) {
        self.listener = entity;
    }

    pub fn set_bus_volume(&mut self, bus: &str, volume: f32, fade: f32) {
        self.push(AudioCommand::SetBusVolume {
            bus: bus.to_owned(),
            volume,
            fade,
        });
    }

    pub fn set_bus_effect(&mut self, bus: &str, effect: BusEffect, fade: f32) {
        self.push(AudioCommand::SetBusEffect {
            bus: bus.to_owned(),
            effect,
            fade,
        });
    }

    pub fn mute_bus(&mut self, bus: &str, muted: bool) {
        self.push(AudioCommand::SetBusMute {
            bus: bus.to_owned(),
            muted,
        });
    }

    pub fn solo_bus(&mut self, bus: &str, solo: bool) {
        self.push(AudioCommand::SetBusSolo {
            bus: bus.to_owned(),
            solo,
        });
    }

    pub fn set_bus_voice_limit(&mut self, bus: &str, limit: usize) {
        self.push(AudioCommand::SetBusVoiceLimit {
            bus: bus.to_owned(),
            limit: limit.clamp(1, MAX_VOICE_LIMIT),
        });
    }

    /// Plays requested so far, played or not. Never reset.
    #[must_use]
    pub fn requested(&self) -> u64 {
        self.requested
    }

    /// Plays requested per clip path, played or not. Never reset.
    #[must_use]
    pub fn requested_clips(&self) -> &std::collections::BTreeMap<String, u64> {
        &self.clips
    }

    /// Caption lines showing now, oldest sound first.
    #[must_use]
    pub fn captions(&self) -> Vec<&str> {
        self.tracked
            .values()
            .filter(|tracked| tracked.caption_tick_started(self.caption_tick))
            .flat_map(|tracked| {
                let clock = tracked.clock as f32;
                tracked
                    .captions
                    .iter()
                    .filter(move |line| line.start <= clock && clock < line.end)
                    .map(|line| line.text.as_str())
            })
            .collect()
    }

    /// Forgets the sounds the device says have ended.
    pub fn retain_sounds(&mut self, mut playing: impl FnMut(SoundId) -> bool) {
        self.active.retain(|id, _| playing(*id));
        // Captions stay up for their whole time even when the clip is
        // shorter, so a short cue's caption can be read.
        self.tracked.retain(|id, tracked| {
            self.active.contains_key(id) || !tracked.captions.is_empty()
        });
    }

    /// Takes the waiting commands; the runner calls this once per frame.
    pub fn drain(&mut self) -> Vec<AudioCommand> {
        std::mem::take(&mut self.commands)
    }
}

impl Tracked {
    fn caption_tick_started(&self, tick: u64) -> bool {
        !self.captions.is_empty() && !self.paused && tick >= self.start
    }
}

/// Pan and distance gain of a sound at `position` heard from `ear`, whose
/// right is the unit vector `right`, with the volume following `falloff`.
#[must_use]
pub fn spatialize(
    ear: [f32; 3],
    right: [f32; 3],
    position: [f32; 3],
    falloff: Falloff,
) -> (f32, f32) {
    let offset = [0, 1, 2].map(|axis| position[axis] - ear[axis]);
    let distance = offset.iter().map(|v| v * v).sum::<f32>().sqrt();
    let pan = if distance > 1e-4 {
        (offset.iter().zip(right).map(|(a, b)| a * b).sum::<f32>() / distance)
            .clamp(-1.0, 1.0)
    } else {
        0.0
    };
    (pan, falloff.gain(distance))
}

/// Fixed tick a clip of `seconds` started on `start` ends at `rate`.
#[must_use]
pub fn end_tick(
    start: u64,
    seconds: f64,
    rate: f32,
    fixed_delta: std::time::Duration,
) -> u64 {
    let real = seconds / f64::from(rate.abs().max(1e-3));
    start + (real / fixed_delta.as_secs_f64()).ceil() as u64
}

/// Entity whose camera aims: the highest-priority active one, preferring
/// one that fills the window. Same tie-break as the renderer.
pub fn active_camera(world: &mut World) -> Option<(Entity, super::Camera)> {
    let mut cameras = world.query::<(Entity, &super::Camera)>();
    cameras
        .iter(world)
        .filter(|(_, camera)| camera.active)
        .max_by_key(|(entity, camera)| camera_rank(*entity, camera))
        .map(|(entity, camera)| (entity, *camera))
}

/// Order active cameras compete in; the highest aims.
pub(crate) fn camera_rank(
    entity: Entity,
    camera: &super::Camera,
) -> (bool, i32, std::cmp::Reverse<u64>) {
    (
        camera.viewport.is_none(),
        camera.priority,
        std::cmp::Reverse(entity.to_bits()),
    )
}

/// Turns the [`SoundEvent`]s visible this frame into play requests, then
/// updates the listener, sounds that follow entities, occlusion and
/// captions. The runners call it once after each update; it is not a
/// schedule system because adding one changes system order, and replays
/// hash that. It only reads the world, apart from the audio queue.
pub fn route_sound_events(world: &mut World) {
    let tick = world
        .get_resource::<crate::runtime::FrameTime>()
        .map_or(0, |time| time.fixed_tick);
    route_slide_sounds(world, tick);
    let step = world
        .get_resource::<crate::runtime::FrameTime>()
        .map_or(1.0 / 60.0, |time| time.fixed_delta.as_secs_f64());
    let clips: Vec<SoundEvent> = world
        .get_resource::<EventQueue<SoundEvent>>()
        .map(|events| events.iter().cloned().collect())
        .unwrap_or_default();
    let Some(listener) = world
        .get_resource::<AudioQueue>()
        .map(|queue| queue.listener)
    else {
        return;
    };
    let listener = listener
        .filter(|entity| world.get_entity(*entity).is_ok())
        .or_else(|| active_camera(world).map(|(entity, _)| entity));
    let ear = listener.and_then(|entity| {
        let matrix = super::ik::world_matrix(world, entity)?;
        let position = matrix.transform_point(&nalgebra::Point3::origin());
        let right = matrix
            .transform_vector(&nalgebra::Vector3::x())
            .try_normalize(1e-6)
            .unwrap_or_else(nalgebra::Vector3::x);
        Some((position.coords.into(), right.into()))
    });
    // Where each tracked sound is now, and how blocked.
    let tracked: Vec<(SoundId, Tracked)> = world
        .resource::<AudioQueue>()
        .tracked
        .iter()
        .map(|(id, tracked)| (*id, tracked.clone()))
        .collect();
    let zone = ear.and_then(|(at, _)| {
        let physics = world.get_resource::<super::PhysicsWorld>()?;
        physics
            .overlap_sphere(at, 0.01, u32::MAX)
            .into_iter()
            .find_map(|entity| {
                Some((entity, world.get::<ReverbZone>(entity)?.clone()))
            })
    });
    let mut placed = Vec::new();
    for (id, tracked) in &tracked {
        let mut position = tracked.position;
        if let Some(entity) = tracked.follow {
            if let Some(matrix) = super::ik::world_matrix(world, entity) {
                let at = matrix.transform_point(&nalgebra::Point3::origin());
                position = Some(at.coords.into());
            }
        }
        let occlusion = match (tracked.occlude, position, ear) {
            (true, Some(at), Some((ear, _))) => {
                occlusion(world, ear, at, [listener, tracked.follow])
            }
            _ => 0.0,
        };
        placed.push((*id, position, occlusion));
    }
    let Some(mut queue) = world.get_resource_mut::<AudioQueue>() else {
        return;
    };
    for event in clips {
        let sound = Sound {
            volume: event.volume,
            captions: (!event.caption.is_empty())
                .then(|| {
                    Caption::new(0.0, super::CUE_CAPTION_SECONDS, event.caption)
                })
                .into_iter()
                .collect(),
            ..Sound::default()
        };
        queue.play(&event.clip, &sound, tick);
    }
    if queue.reverb_zone != zone {
        if let Some((_, left)) = queue.reverb_zone.take() {
            queue.set_bus_effect(&left.bus, left.effect(0.0), left.fade);
        }
        if let Some((_, inside)) = &zone {
            queue.set_bus_effect(
                &inside.bus,
                inside.effect(inside.mix),
                inside.fade,
            );
        }
        queue.reverb_zone = zone;
    }
    if let Some((position, right)) = ear {
        if queue.heard_from != Some((position, right)) {
            queue.heard_from = Some((position, right));
            queue.push(AudioCommand::SetListener { position, right });
        }
    }
    let elapsed = tick.saturating_sub(queue.caption_tick) as f64 * step;
    for (id, position, amount) in placed {
        let Some(tracked) = queue.tracked.get_mut(&id) else {
            continue;
        };
        let mut pitch = None;
        if let (true, Some(at), Some((ear, _))) =
            (tracked.doppler > 0.0, position, ear)
        {
            let distance = (0..3)
                .map(|axis| (at[axis] - ear[axis]).powi(2))
                .sum::<f32>()
                .sqrt();
            if elapsed > 0.0 {
                if let Some(before) = tracked.distance {
                    let closing = (before - distance) / elapsed as f32;
                    let factor = (SPEED_OF_SOUND
                        / (SPEED_OF_SOUND - closing * tracked.doppler))
                        .clamp(0.5, 2.0);
                    if (factor - tracked.pitch).abs() > 1e-3 {
                        tracked.pitch = factor;
                        pitch = Some(factor);
                    }
                }
                tracked.distance = Some(distance);
            }
        }
        if let Some(pitch) = pitch {
            queue.push(AudioCommand::SetDoppler { id, pitch });
        }
        let Some(tracked) = queue.tracked.get_mut(&id) else {
            continue;
        };
        let moved = position.filter(|_| tracked.follow.is_some());
        let moved = moved.filter(|at| tracked.position != Some(*at));
        let occluded = (tracked.occlusion != amount).then_some(amount);
        tracked.position = position;
        tracked.occlusion = amount;
        if let Some(position) = moved {
            queue.push(AudioCommand::SetPosition { id, position });
        }
        if let Some(amount) = occluded {
            queue.push(AudioCommand::SetOcclusion { id, amount });
        }
    }
    // Captions: clip time advances with the fixed ticks since last frame.
    let since = queue.caption_tick;
    queue.caption_tick = tick;
    queue.tracked.retain(|_, tracked| {
        if !tracked.paused {
            let ticks = tick.saturating_sub(since.max(tracked.start));
            tracked.clock += ticks as f64 * step * f64::from(tracked.rate);
        }
        // Captioned sounds that only showed text are done after the last
        // line.
        tracked.follow.is_some()
            || tracked.doppler > 0.0
            || tracked.occlude
            || tracked
                .captions
                .iter()
                .any(|line| f64::from(line.end) > tracked.clock)
    });
}

/// 1 when a collider sits between `ear` and `at`, else 0. Skips the
/// listener's and the followed entity's own colliders.
fn occlusion(
    world: &World,
    ear: [f32; 3],
    at: [f32; 3],
    skip: [Option<Entity>; 2],
) -> f32 {
    let Some(physics) = world.get_resource::<super::PhysicsWorld>() else {
        return 0.0;
    };
    let direction = [0, 1, 2].map(|axis| at[axis] - ear[axis]);
    let distance = direction.iter().map(|v| v * v).sum::<f32>().sqrt();
    // Stop short of the sound so a collider around its source is not a
    // wall.
    let reach = distance - 0.25;
    if reach <= 0.0 {
        return 0.0;
    }
    let hit =
        physics.raycast_where(ear, direction, reach, u32::MAX, |entity| {
            !skip.contains(&Some(entity))
        });
    if hit.is_some() {
        1.0
    } else {
        0.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn playing_lists_live_sounds_and_pauses_them_by_bus() {
        let mut queue = AudioQueue::default();
        let on = |bus: &str| Sound {
            bus: bus.to_owned(),
            ..Sound::default()
        };
        let music = queue.play("music.ogg", &on("music"), 0);
        let hum = queue.play("hum.ogg", &on("music"), 0);
        let step = queue.play("step.ogg", &on("sfx"), 0);
        queue.drain();
        queue.pause_bus(Some("music"));
        let paused = |queue: &AudioQueue| {
            let sounds = queue.playing();
            sounds
                .iter()
                .filter(|s| s.paused)
                .map(|s| s.id)
                .collect::<Vec<_>>()
        };
        assert_eq!(paused(&queue), [music, hum]);
        assert_eq!(
            queue.drain(),
            [AudioCommand::Pause(music), AudioCommand::Pause(hum)]
        );
        queue.pause_bus(None);
        assert_eq!(queue.drain(), [AudioCommand::Pause(step)]);
        queue.resume_bus(Some("sfx"));
        assert_eq!(paused(&queue), [music, hum]);
        queue.resume_bus(None);
        assert_eq!(paused(&queue), []);
        // The device finished `hum`; `step` is stopped by hand.
        queue.retain_sounds(|id| id != hum);
        queue.stop(step);
        let live = queue.playing();
        assert_eq!(live.len(), 1);
        assert_eq!((live[0].id, live[0].clip.as_str()), (music, "music.ogg"));
        assert_eq!(live[0].bus, "music");
    }

    #[test]
    fn beat_clock_lands_on_whole_ticks() {
        let step = std::time::Duration::from_secs_f64(1.0 / 60.0);
        let clock = BeatClock::new(128, 2, step);
        let ticks: Vec<_> = (0..5).map(|beat| clock.tick_of(beat)).collect();
        assert_eq!(ticks, [2, 31, 59, 87, 115]);
        assert_eq!(clock.beat_at(1), None);
        assert_eq!(clock.beat_at(58), Some(1));
        assert!(clock.is_beat(59) && !clock.is_beat(60));
        assert_eq!(clock.ticks_to_next(59), 28);
        assert_eq!(clock.ticks_to_next(0), 2);
        assert_eq!(clock.beats_between(30, 90), 1..4);
        assert_eq!(clock.beats_between(0, 1), 0..0);
        let even = BeatClock::new(120, 0, step);
        assert_eq!(even.tick_of(3), 90);
        assert_eq!(even.phase(45), 0.5);
    }

    #[test]
    fn requests_get_distinct_ids_and_drain_once() {
        let mut queue = AudioQueue::default();
        let a = queue.play("sfx/a.wav", &Sound::default(), 0);
        let b = queue.play("sfx/b.wav", &Sound::default(), 0);
        queue.stop(a);
        assert_ne!(a, b);
        assert_eq!(queue.requested(), 2);
        assert_eq!(queue.requested_clips()["sfx/b.wav"], 1);
        assert_eq!(queue.drain().len(), 3);
        assert!(queue.drain().is_empty());
        for _ in 0..QUEUE_LIMIT + 10 {
            queue.set_master_volume(1.0);
        }
        assert_eq!(queue.drain().len(), QUEUE_LIMIT);
        assert_eq!(queue.requested(), 2);
    }

    #[test]
    fn half_rate_doubles_the_end_tick() {
        let step = std::time::Duration::from_secs_f64(1.0 / 60.0);
        assert_eq!(end_tick(10, 2.0, 1.0, step), 130);
        assert_eq!(end_tick(10, 2.0, 0.5, step), 250);
        assert_eq!(end_tick(10, 2.0, 2.0, step), 70);
    }

    #[test]
    fn spatialize_pans_to_the_side_and_falls_off_past_two_metres() {
        let right = [1.0, 0.0, 0.0];
        let (pan, gain) =
            spatialize([0.0; 3], right, [-5.0, 0.0, 0.0], Falloff::default());
        assert_eq!((pan, gain), (-1.0, 0.4));
        let (pan, gain) =
            spatialize([0.0; 3], right, [0.0, 0.0, -1.0], Falloff::default());
        assert_eq!((pan, gain), (0.0, 1.0));
        assert_eq!(spatialize([0.0; 3], right, [0.0; 3], Falloff::Off).0, 0.0);
        let near =
            |falloff: Falloff| [1.0, 4.0, 8.0, 16.0].map(|d| falloff.gain(d));
        assert_eq!(
            near(Falloff::InverseSquare { near: 2.0 }),
            [1.0, 0.25, 0.0625, 0.015625]
        );
        assert_eq!(
            near(Falloff::Linear {
                near: 2.0,
                far: 10.0
            }),
            [1.0, 0.75, 0.25, 0.0]
        );
        assert_eq!(near(Falloff::Off), [1.0; 4]);
    }

    #[test]
    fn bus_voice_limits_clamp_and_inactive_effects_report_off() {
        let mut queue = AudioQueue::default();
        queue.set_bus_voice_limit("bears", 0);
        queue.set_bus_voice_limit("bears", 10_000);
        let limits: Vec<_> = queue
            .drain()
            .into_iter()
            .filter_map(|command| match command {
                AudioCommand::SetBusVoiceLimit { limit, .. } => Some(limit),
                _ => None,
            })
            .collect();
        assert_eq!(limits, [1, MAX_VOICE_LIMIT]);
        assert!(!BusEffect::LowPass {
            cutoff_hz: 20_000.0
        }
        .active());
        assert!(!BusEffect::Distortion {
            drive: 6.0,
            mix: 0.0
        }
        .active());
        assert!(!BusEffect::Compressor {
            threshold_db: -20.0,
            ratio: 1.0,
            makeup_db: 0.0
        }
        .active());
        assert!(BusEffect::Reverb {
            room: 0.5,
            damping: 0.5,
            mix: 0.2
        }
        .active());
    }
}
