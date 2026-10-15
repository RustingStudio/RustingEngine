//! Input recording and replay. A [`Replay`] holds what drives the
//! simulation from a known start: each frame's real delta and the input its
//! systems saw, tagged with the fixed tick the frame starts on, plus the seed
//! and the world-state hash of every tick. Playing it back into the same
//! scene must reproduce those hashes.

use std::fmt;
use std::time::Duration;

use bevy_ecs::prelude::World;
use serde::{Deserialize, Serialize};

use super::{
    first_divergent_tick, App, AppError, FrameTime, RandomSeed, RuntimeInput,
    SnapshotError, StateHashes, WorldSnapshot,
};

/// Version written to [`Replay::format_version`]; playback rejects others.
pub const REPLAY_FORMAT_VERSION: u32 = 1;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Replay {
    pub format_version: u32,
    /// [`RandomSeed`] when recording started.
    pub seed: u64,
    /// Fixed ticks completed when recording started.
    pub start_tick: u64,
    pub frames: Vec<ReplayFrame>,
    /// `(tick, hash)` of every tick recorded, as in [`StateHashes`].
    pub hashes: Vec<(u64, u64)>,
}

/// One `App::update`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ReplayFrame {
    /// Fixed ticks completed before the frame.
    pub tick: u64,
    /// Real time the frame advanced the clock by.
    pub delta_nanos: u64,
    /// [`FrameTime::real_delta`] when it differs from the clock's delta
    /// (slow motion under overload).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub real_nanos: Option<u64>,
    /// Input the frame's systems saw; `None` when it equals the previous
    /// frame's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<RuntimeInput>,
}

/// Appends each `App::update` to a replay; see [`App::start_recording`].
pub struct ReplayRecorder {
    replay: Replay,
    last_input: Option<RuntimeInput>,
}

impl ReplayRecorder {
    pub(super) fn new(world: &World) -> Self {
        Self {
            replay: Replay {
                format_version: REPLAY_FORMAT_VERSION,
                seed: world
                    .get_resource::<RandomSeed>()
                    .map_or(0, |seed| seed.0),
                start_tick: world.resource::<FrameTime>().fixed_tick,
                frames: Vec::new(),
                hashes: Vec::new(),
            },
            last_input: None,
        }
    }

    /// Before the frame's systems run.
    pub(super) fn frame_start(
        &mut self,
        world: &World,
        delta: Duration,
        real: Duration,
    ) {
        let nanos =
            |d: Duration| u64::try_from(d.as_nanos()).unwrap_or(u64::MAX);
        let input = world.get_resource::<RuntimeInput>();
        let changed = input != self.last_input.as_ref();
        self.replay.frames.push(ReplayFrame {
            tick: world.resource::<FrameTime>().fixed_tick,
            delta_nanos: nanos(delta),
            real_nanos: (real != delta).then(|| nanos(real)),
            input: input.filter(|_| changed).cloned(),
        });
        if changed {
            self.last_input = input.cloned();
        }
    }

    /// After the frame's fixed steps.
    pub(super) fn frame_hashes(&mut self, world: &World, steps: u32) {
        if let Some(hashes) = world.get_resource::<StateHashes>() {
            self.replay.hashes.extend(latest_hashes(hashes, steps));
        }
    }

    pub(super) fn finish(self) -> Replay {
        self.replay
    }
}

/// The last `steps` recorded hashes, oldest first.
fn latest_hashes(
    hashes: &StateHashes,
    steps: u32,
) -> impl Iterator<Item = (u64, u64)> + '_ {
    let skip = hashes.recent.len().saturating_sub(steps as usize);
    hashes.recent.iter().skip(skip).copied()
}

#[derive(Debug)]
pub enum ReplayError {
    UnsupportedVersion(u32),
    /// The app is not at the tick the replay starts on.
    StartTick {
        expected: u64,
        actual: u64,
    },
    App(AppError),
    /// A re-simulated tick's hash differs from the recording.
    Diverged {
        tick: u64,
    },
    Snapshot(SnapshotError),
}

impl fmt::Display for ReplayError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedVersion(version) => write!(
                formatter,
                "replay format {version} is not supported (expected {REPLAY_FORMAT_VERSION})"
            ),
            Self::StartTick { expected, actual } => write!(
                formatter,
                "replay starts on tick {expected}, but the app is on tick {actual}"
            ),
            Self::App(error) => error.fmt(formatter),
            Self::Diverged { tick } => write!(
                formatter,
                "tick {tick} does not match the recording"
            ),
            Self::Snapshot(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for ReplayError {}

/// Checks that `app` can play `replay` and sets the seed.
fn prepare(app: &mut App, replay: &Replay) -> Result<(), ReplayError> {
    if replay.format_version != REPLAY_FORMAT_VERSION {
        return Err(ReplayError::UnsupportedVersion(replay.format_version));
    }
    let actual = app.world().resource::<FrameTime>().fixed_tick;
    if actual != replay.start_tick {
        return Err(ReplayError::StartTick {
            expected: replay.start_tick,
            actual,
        });
    }
    // Write resources in place: inserting one allocates an entity and
    // would shift the ids of entities spawned later.
    let world = app.world_mut();
    match world.get_resource_mut::<RandomSeed>() {
        Some(mut seed) => seed.0 = replay.seed,
        None if replay.seed != 0 => {
            world.insert_resource(RandomSeed(replay.seed))
        }
        None => {}
    }
    Ok(())
}

/// Runs one recorded frame. `input` carries the latest recorded input.
/// Returns the frame's new `(tick, hash)` pairs.
fn play_frame(
    app: &mut App,
    frame: &ReplayFrame,
    input: &mut RuntimeInput,
) -> Result<Vec<(u64, u64)>, ReplayError> {
    if let Some(recorded) = &frame.input {
        input.clone_from(recorded);
    }
    if let Some(mut current) =
        app.world_mut().get_resource_mut::<RuntimeInput>()
    {
        current.clone_from(input);
    }
    let report = app
        .update_timed(
            Duration::from_nanos(frame.delta_nanos),
            Duration::from_nanos(frame.real_nanos.unwrap_or(frame.delta_nanos)),
        )
        .map_err(ReplayError::App)?;
    Ok(app
        .world()
        .get_resource::<StateHashes>()
        .map(|hashes| latest_hashes(hashes, report.fixed_steps).collect())
        .unwrap_or_default())
}

/// Plays `replay` into `app`, which must hold the scene the replay was
/// recorded in, at the replay's start tick. Sets the seed, then runs every
/// frame with its delta and input. Returns the first tick whose hash
/// differs from the recording, or `None` when all match.
pub fn play_replay(
    app: &mut App,
    replay: &Replay,
) -> Result<Option<u64>, ReplayError> {
    prepare(app, replay)?;
    let mut input = RuntimeInput::default();
    let mut hashes = Vec::with_capacity(replay.hashes.len());
    for frame in &replay.frames {
        hashes.extend(play_frame(app, frame, &mut input)?);
    }
    Ok(first_divergent_tick(&replay.hashes, &hashes))
}

/// A snapshot and the replay position it was taken at.
struct Checkpoint {
    /// Next frame to play.
    frame: usize,
    input: RuntimeInput,
    world: WorldSnapshot,
}

/// Plays a replay and moves to any tick in it. Going forward plays frames;
/// going back restores the nearest earlier snapshot into a new app and
/// re-simulates from there. Snapshots are taken every `snapshot_interval`
/// ticks the first time playback passes them, and every re-simulated tick
/// is checked against the recorded hashes.
pub struct ReplaySeeker<F> {
    replay: Replay,
    make_app: F,
    snapshot_interval: u64,
    app: App,
    /// Next frame to play.
    frame: usize,
    input: RuntimeInput,
    /// In tick order.
    checkpoints: Vec<Checkpoint>,
}

impl<F> ReplaySeeker<F>
where
    F: FnMut() -> Result<App, AppError>,
{
    /// `make_app` builds the app the replay was recorded in, at its start
    /// tick, the same way every time.
    pub fn new(
        replay: Replay,
        snapshot_interval: u64,
        mut make_app: F,
    ) -> Result<Self, ReplayError> {
        let mut app = make_app().map_err(ReplayError::App)?;
        prepare(&mut app, &replay)?;
        Ok(Self {
            replay,
            make_app,
            snapshot_interval: snapshot_interval.max(1),
            app,
            frame: 0,
            input: RuntimeInput::default(),
            checkpoints: Vec::new(),
        })
    }

    #[must_use]
    pub fn app(&self) -> &App {
        &self.app
    }

    /// Fixed ticks completed at the current position.
    #[must_use]
    pub fn tick(&self) -> u64 {
        self.app.world().resource::<FrameTime>().fixed_tick
    }

    /// Snapshots taken so far.
    #[must_use]
    pub fn snapshot_count(&self) -> usize {
        self.checkpoints.len()
    }

    /// Moves to the first frame boundary at or after `tick`, or to the end
    /// of the replay, and returns the tick reached. A frame can run several
    /// ticks, so the result can be past `tick`.
    pub fn seek(&mut self, tick: u64) -> Result<u64, ReplayError> {
        let current = self.tick();
        let nearest = self
            .checkpoints
            .iter()
            .rposition(|checkpoint| checkpoint.world.tick() <= tick);
        let nearest_tick =
            nearest.map(|index| self.checkpoints[index].world.tick());
        if tick < current || nearest_tick.is_some_and(|t| t > current) {
            let mut app = (self.make_app)().map_err(ReplayError::App)?;
            prepare(&mut app, &self.replay)?;
            match nearest {
                Some(index) => {
                    let checkpoint = &self.checkpoints[index];
                    app.restore(&checkpoint.world)
                        .map_err(ReplayError::Snapshot)?;
                    self.frame = checkpoint.frame;
                    self.input.clone_from(&checkpoint.input);
                }
                None => {
                    self.frame = 0;
                    self.input = RuntimeInput::default();
                }
            }
            self.app = app;
        }
        while self.frame < self.replay.frames.len() && self.tick() < tick {
            self.step()?;
        }
        Ok(self.tick())
    }

    fn step(&mut self) -> Result<(), ReplayError> {
        let frame = &self.replay.frames[self.frame];
        for (tick, hash) in play_frame(&mut self.app, frame, &mut self.input)? {
            let recorded = self
                .replay
                .hashes
                .binary_search_by_key(&tick, |&(tick, _)| tick)
                .ok()
                .map(|index| self.replay.hashes[index].1);
            if recorded.is_some_and(|recorded| recorded != hash) {
                return Err(ReplayError::Diverged { tick });
            }
        }
        self.frame += 1;
        let tick = self.tick();
        let last = self
            .checkpoints
            .last()
            .map_or(self.replay.start_tick, |checkpoint| {
                checkpoint.world.tick()
            });
        if tick >= last + self.snapshot_interval {
            let world = self.app.snapshot().map_err(ReplayError::Snapshot)?;
            self.checkpoints.push(Checkpoint {
                frame: self.frame,
                input: self.input.clone(),
                world,
            });
        }
        Ok(())
    }
}
