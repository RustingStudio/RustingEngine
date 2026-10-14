//! Input-driven scenario tests.
//!
//! A scenario presses and releases named actions at fixed ticks, checks
//! reflected scene state and events, and optionally captures frames. It runs
//! inside the game process, so the game's own systems run as they would in
//! play. The same seed gives the same run: the scenario sets
//! [`RandomSeed`], and every tick advances by exactly one fixed step.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use bevy_ecs::prelude::{Entity, World};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::rendering::capture::HeadlessCapture;
use crate::runtime::{
    picking, scene_entity_lenient, set_registered_component,
    set_registered_component_field, ActionMap, Camera, CollisionEvent,
    EventQueue, FrameTime, GlobalTransform, InputBinding, MeshRenderer,
    MouseButton, Name, RandomSeed, RenderWorld, RuntimeInput, SceneId,
    SceneTransform, Stick,
};
use crate::{App, AssetServer, Transform};

/// Environment variable naming the scenario file a game build runs instead
/// of opening a window.
pub const TEST_SCENARIO_ENV: &str = "RUSTING_TEST_SCENARIO";
/// Set by `rusting test --update-golden`: `golden` images are rewritten
/// from this run's captures instead of compared.
pub const UPDATE_GOLDEN_ENV: &str = "RUSTING_UPDATE_GOLDEN";
/// Set by `rusting determinism --scenario`: run every tick, as with
/// `keep_going`, so a failed check does not cut the hash comparison short.
pub const KEEP_GOING_ENV: &str = "RUSTING_KEEP_GOING";
/// Environment variable naming where the game writes its
/// [`ScenarioReport`] as JSON.
pub const TEST_REPORT_ENV: &str = "RUSTING_TEST_REPORT";
/// Seconds a scenario run may go without finishing a tick before the game
/// reports a stall and exits; `0` turns the watchdog off. Default
/// [`DEFAULT_STALL_SECS`].
pub const STALL_SECS_ENV: &str = "RUSTING_TEST_STALL_SECS";
/// Default for [`STALL_SECS_ENV`]: long enough for a first capture to build
/// its pipelines on a software device.
pub const DEFAULT_STALL_SECS: u64 = 60;

/// Ticks the running scenario has finished, for the stall watchdog only.
/// Never read by simulation code.
pub(crate) static TICKS_FINISHED: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);

/// Polls `ticks_finished` every `poll` and returns its value once it has
/// not changed for `limit`: the run is stuck after that many ticks.
pub(crate) fn wait_for_stall(
    limit: Duration,
    poll: Duration,
    ticks_finished: impl Fn() -> u64,
) -> u64 {
    let mut last = ticks_finished();
    let mut since = std::time::Instant::now();
    loop {
        std::thread::sleep(poll);
        let now = ticks_finished();
        if now != last {
            last = now;
            since = std::time::Instant::now();
        } else if since.elapsed() >= limit {
            return last;
        }
    }
}

/// The failure a stalled run reports after `finished` ticks.
pub(crate) fn stall_message(limit: Duration, finished: u64) -> String {
    let last = match finished {
        0 => "tick 0 never finished".to_owned(),
        ticks => format!("the last finished tick is {}", ticks - 1),
    };
    format!(
        "no tick finished in {} s; {last}. Look for a deadlock (a Mutex locked twice) or an endless loop in game code; raise the limit with {STALL_SECS_ENV}",
        limit.as_secs()
    )
}

/// Time to advance before tick `tick`: nothing for tick 0, which only
/// extracts the loaded scene, then exactly one fixed step per tick.
#[must_use]
pub fn tick_delta(app: &App, tick: u32) -> Duration {
    if tick == 0 {
        Duration::ZERO
    } else {
        app.world().resource::<FrameTime>().fixed_delta
    }
}

/// `counter:<name>` in place of an entity name means the entity holding the
/// `rusting.counter` called `<name>`.
pub const COUNTER_PREFIX: &str = "counter:";

/// In place of an entity name: the sounds game code and sound cues asked
/// for, as `{"requested": n, "clips": {"<clip path>": n}, "level": [l, r],
/// "playing": [...]}`. A `/` in a clip path is `~1` in a JSON pointer:
/// `/clips/sfx~1hit.wav`; a clip that never played reads as 0. `level` is the RMS of the mix during the last
/// tick per speaker, `peak` the largest sample magnitude that tick (1.0 is
/// full scale), `clipped` the samples at or over full scale since tick 0,
/// `playing` lists the sounds that have not ended, `buses` each bus's
/// voices, limit, dropped and stolen sounds, active effects and its own
/// `level` and `peak` over the last tick (after its effects and volume;
/// the main bus `""`, written `/buses//level`, is the mix before the
/// master volume), and
/// `dropped` the sounds voice limits refused or stopped, all from an
/// offline kira mix of what the game asked for. `captions` lists the
/// caption lines showing.
pub const AUDIO_ENTITY: &str = "audio:";

/// `class:<name>` in place of an entity name reads the members of an object
/// class as `{"count": n, "gpu": {"count": n, "min": [x, y, z], "max": [x,
/// y, z]}}`, where `gpu` covers the members whose GPU pose has arrived.
/// `class:<name> in x,y,z x,y,z` adds `"inside"`: how many of those lie in
/// that box. Reading it asks for a fresh GPU snapshot of the class, which
/// arrives one to three ticks later, so check it with `within` or `until`.
pub const CLASS_PREFIX: &str = "class:";

/// The offline mix as `audio:` reports it after the last tick.
#[derive(bevy_ecs::prelude::Resource, Clone, Default)]
struct AudioMix {
    level: [f32; 2],
    peak: [f32; 2],
    clipped: u64,
    playing: Vec<crate::audio_output::PlayingSound>,
    buses: std::collections::BTreeMap<String, crate::audio_output::BusReport>,
}

/// Appends the state hashes recorded since the last call, so the report
/// keeps every tick, not only the `STATE_HASH_HISTORY` the world holds.
fn collect_hashes(world: &World, report: &mut ScenarioReport) {
    let Some(hashes) = world.get_resource::<crate::runtime::StateHashes>()
    else {
        return;
    };
    for (history, kept) in [
        (&hashes.recent, &mut report.state_hashes),
        (&hashes.gpu, &mut report.gpu_state_hashes),
    ] {
        let last = kept.last().map(|entry| entry.0);
        kept.extend(
            history
                .iter()
                .filter(|entry| last.is_none_or(|last| entry.0 > last)),
        );
    }
}

/// Plays this tick's sound requests into the offline mix and renders one
/// fixed step of audio, appended to `samples` when given.
fn mix_tick(
    app: &mut App,
    mixer: &mut crate::audio_output::OfflineMixer,
    samples: Option<&mut Vec<f32>>,
) {
    let world = app.world_mut();
    let commands = world
        .get_resource_mut::<crate::runtime::AudioQueue>()
        .map(|mut queue| queue.drain())
        .unwrap_or_default();
    let assets = world
        .get_resource::<crate::project_runner::ProjectFolder>()
        .map(|folder| folder.0.join("assets"))
        .unwrap_or_default();
    let time = *world.resource::<FrameTime>();
    let step = time.fixed_delta.as_secs_f64();
    let delay = |tick: u64| {
        Duration::from_secs_f64(
            tick.saturating_sub(time.fixed_tick) as f64 * step,
        )
    };
    for command in commands {
        mixer.run(&assets, command, delay);
    }
    if let Some(mut queue) =
        world.get_resource_mut::<crate::runtime::AudioQueue>()
    {
        queue.retain_sounds(|id| mixer.has_sound(id.0));
    }
    let frames =
        (step * f64::from(crate::audio_output::MIX_RATE)).round() as usize;
    let block = mixer.render(frames);
    let rms = |channel: usize| {
        let sum: f32 =
            block.iter().skip(channel).step_by(2).map(|s| s * s).sum();
        (sum / frames.max(1) as f32).sqrt()
    };
    let peak = |channel: usize| {
        block
            .iter()
            .skip(channel)
            .step_by(2)
            .fold(0.0_f32, |peak, s| peak.max(s.abs()))
    };
    let clipped = world
        .get_resource::<AudioMix>()
        .map_or(0, |mix| mix.clipped)
        + block.iter().filter(|s| s.abs() >= 1.0).count() as u64;
    let mix = AudioMix {
        level: [rms(0), rms(1)],
        peak: [peak(0), peak(1)],
        clipped,
        playing: mixer.playing(),
        buses: mixer.buses(),
    };
    world.insert_resource(mix);
    if let Some(samples) = samples {
        samples.extend_from_slice(&block);
    }
}

/// Path of a counter's value in its entity's scene form.
const COUNTER_VALUE: &str = "/components/rusting.counter/value";

/// Finds an entity by persistent ID, by name, or by counter name
/// (`counter:<name>`). With several matches, the lowest persistent ID wins,
/// so the choice does not depend on spawn order.
pub fn find_entity(
    world: &mut World,
    wanted: &str,
    accept: impl Fn(&World, Entity) -> bool,
) -> Option<Entity> {
    if let Some(counter) = wanted.strip_prefix(COUNTER_PREFIX) {
        let mut query =
            world
                .query::<(Entity, &crate::runtime::Counter, Option<&SceneId>)>(
                );
        let mut matches: Vec<_> = query
            .iter(world)
            .filter(|(_, found, _)| found.name == counter)
            .map(|(entity, _, scene_id)| (scene_id.map(|id| id.0), entity))
            .collect();
        matches.sort();
        return matches
            .into_iter()
            .map(|(_, entity)| entity)
            .find(|&entity| accept(world, entity));
    }
    let id = Uuid::parse_str(wanted).ok();
    let mut query = world.query::<(Entity, Option<&SceneId>, Option<&Name>)>();
    let mut matches: Vec<_> = query
        .iter(world)
        .filter(|(_, scene_id, name)| {
            scene_id.is_some_and(|scene_id| Some(scene_id.0) == id)
                || name.is_some_and(|name| name.0 == wanted)
        })
        .map(|(entity, scene_id, _)| (scene_id.map(|id| id.0), entity))
        .collect();
    matches.sort();
    matches
        .into_iter()
        .map(|(_, entity)| entity)
        .find(|&entity| accept(world, entity))
}

/// Finds a camera by persistent ID or name.
pub fn find_camera(world: &mut World, wanted: &str) -> Option<Entity> {
    find_entity(world, wanted, |world, entity| {
        world.get::<Camera>(entity).is_some()
    })
}

/// The camera the last extracted frame renders from, at an image size.
pub struct CameraView {
    entity: Entity,
    camera: Camera,
    transform: GlobalTransform,
    extent: [u32; 2],
}

impl CameraView {
    /// The camera selected by the last render extraction.
    #[must_use]
    pub fn active(world: &World, extent: [u32; 2]) -> Option<Self> {
        let active = world.resource::<RenderWorld>().active_camera?;
        Some(Self {
            entity: active.entity,
            camera: *world.get::<Camera>(active.entity)?,
            transform: active.transform,
            extent,
        })
    }

    /// A named camera's view at its own viewport's pixel size within
    /// `extent`, whether or not it is active.
    #[must_use]
    pub fn of(world: &World, entity: Entity, extent: [u32; 2]) -> Option<Self> {
        let camera = *world.get::<Camera>(entity)?;
        let extent = match camera.viewport {
            Some(viewport) => {
                crate::rendering::render_scale::viewport_pixels(
                    viewport, extent,
                )
                .extent
            }
            None => extent,
        };
        Some(Self {
            entity,
            camera,
            transform: *world.get::<GlobalTransform>(entity)?,
            extent,
        })
    }

    #[must_use]
    pub fn extent(&self) -> [u32; 2] {
        self.extent
    }

    /// Projects a world point to a pixel of the image; `None` behind the
    /// camera.
    #[must_use]
    pub fn project(&self, point: [f32; 3]) -> Option<[f32; 2]> {
        picking::project_point(
            nalgebra::Vector3::from(point),
            self.camera,
            self.transform,
            [0.0, 0.0],
            [self.extent[0] as f32, self.extent[1] as f32],
        )
    }

    /// Mesh entities that can show in this view, with their bounds and
    /// matrices worked out once so many rays can be cast cheaply. Meshes
    /// entirely outside the view frustum are left out.
    fn pick_targets(&self, world: &mut World) -> Vec<PickTarget> {
        let size = [self.extent[0] as f32, self.extent[1] as f32];
        let Some(clip_from_world) =
            picking::clip_from_world(size, self.camera, self.transform)
        else {
            return Vec::new();
        };
        let mut meshes =
            world.query::<(Entity, &MeshRenderer, &GlobalTransform)>();
        let assets = world.resource::<AssetServer>();
        let mut bounds = std::collections::HashMap::new();
        meshes
            .iter(world)
            .filter_map(|(entity, renderer, transform)| {
                let (minimum, maximum) = *bounds
                    .entry(renderer.mesh)
                    .or_insert_with(|| {
                        assets
                            .meshes
                            .get(renderer.mesh)
                            .and_then(picking::mesh_bounds)
                    })
                    .as_ref()?;
                let world_from_local =
                    picking::matrix_from_array(transform.matrix);
                let clip_from_local = clip_from_world * world_from_local;
                let corners = (0..8).map(|corner| {
                    let pick = |axis: usize| {
                        if corner >> axis & 1 == 0 {
                            minimum[axis]
                        } else {
                            maximum[axis]
                        }
                    };
                    clip_from_local
                        * nalgebra::Vector4::new(pick(0), pick(1), pick(2), 1.0)
                });
                let corners: Vec<_> = corners.collect();
                let outside = |test: fn(&nalgebra::Vector4<f32>) -> bool| {
                    corners.iter().all(test)
                };
                if outside(|c| c.x < -c.w)
                    || outside(|c| c.x > c.w)
                    || outside(|c| c.y < -c.w)
                    || outside(|c| c.y > c.w)
                    || outside(|c| c.w <= 0.0)
                {
                    return None;
                }
                Some(PickTarget {
                    entity,
                    local_from_world: world_from_local.try_inverse()?,
                    world_from_local,
                    minimum,
                    maximum,
                })
            })
            .collect()
    }

    /// The nearest target under `pixel`: distance, entity and world point.
    fn nearest(
        &self,
        targets: &[PickTarget],
        pixel: [u32; 2],
    ) -> Option<(f32, Entity, nalgebra::Vector3<f32>)> {
        let ray = picking::scene_ray(
            [pixel[0] as f32 + 0.5, pixel[1] as f32 + 0.5],
            [0.0, 0.0],
            [self.extent[0] as f32, self.extent[1] as f32],
            self.camera,
            self.transform,
        )?;
        targets
            .iter()
            .filter_map(|target| {
                let origin = target.local_from_world * ray.origin.push(1.0);
                let direction =
                    target.local_from_world * ray.direction.push(0.0);
                let local = picking::ray_aabb(
                    origin.xyz(),
                    direction.xyz(),
                    target.minimum,
                    target.maximum,
                )?;
                let point = (target.world_from_local
                    * (origin + direction * local))
                    .xyz();
                Some(((point - ray.origin).norm(), target.entity, point))
            })
            .min_by(|left, right| left.0.total_cmp(&right.0))
    }

    /// The object under `pixel`, found by casting a ray against mesh bounds.
    /// Returns its persistent ID, name, distance and world position, or a
    /// null `id` when the ray hits nothing.
    // ponytail: bounds-only picking, like gameplay clicks; a rendered ID
    // buffer would give exact silhouettes.
    pub fn pick(&self, world: &mut World, pixel: [u32; 2]) -> Value {
        let targets = self.pick_targets(world);
        pick_json(world, pixel, self.nearest(&targets, pixel))
    }

    /// Every object that covers part of `rect` (`[x0, y0, x1, y1]`, end
    /// exclusive, clamped to the image), with the share of the rectangle it
    /// is the nearest hit for. Ordered by share, then ID; `none` is the
    /// share that hits nothing.
    // ponytail: samples at most 64x64 pixel centers with bounds picking, so
    // shares are estimates; a rendered ID buffer would make them exact.
    pub fn pick_rect(&self, world: &mut World, rect: [u32; 4]) -> Value {
        let x1 = rect[2].min(self.extent[0]);
        let y1 = rect[3].min(self.extent[1]);
        let (x0, y0) = (rect[0].min(x1), rect[1].min(y1));
        let step = |len: u32| len.div_ceil(64).max(1) as usize;
        let targets = self.pick_targets(world);
        let mut counts: BTreeMap<Option<Uuid>, (u32, Value)> = BTreeMap::new();
        let mut samples = 0_u32;
        for y in (y0..y1).step_by(step(y1 - y0)) {
            for x in (x0..x1).step_by(step(x1 - x0)) {
                let hit = self.nearest(&targets, [x, y]);
                let id = hit.and_then(|(_, entity, _)| {
                    world.get::<SceneId>(entity).map(|id| id.0)
                });
                counts
                    .entry(id)
                    .or_insert_with(|| (0, pick_json(world, [x, y], hit)))
                    .0 += 1;
                samples += 1;
            }
        }
        let share = |count: u32| f64::from(count) / f64::from(samples.max(1));
        let mut hits: Vec<_> = counts
            .iter()
            .filter(|(id, _)| id.is_some())
            .map(|(_, (count, hit))| {
                json!({"id": hit["id"], "name": hit["name"], "share": share(*count)})
            })
            .collect();
        hits.sort_by(|a, b| {
            b["share"]
                .as_f64()
                .partial_cmp(&a["share"].as_f64())
                .unwrap()
        });
        let none = counts.get(&None).map_or(0, |(count, _)| *count);
        json!({"rect": [x0, y0, x1, y1], "samples": samples, "none": share(none), "entities": hits})
    }

    /// Persistent ID, name, projection and transforms of the camera.
    #[must_use]
    pub fn data(&self, world: &World) -> Value {
        let transform = world.get::<Transform>(self.entity);
        json!({
            "id": world.get::<SceneId>(self.entity).map(|id| id.0),
            "name": world.get::<Name>(self.entity).map(|name| &name.0),
            "projection": format!("{:?}", self.camera.projection),
            "position": transform.map(|transform| transform.position),
            "rotation": transform.map(|transform| transform.rotation),
            "world_matrix": self.transform.matrix,
        })
    }
}

/// A mesh prepared for many ray casts by [`CameraView::pick_rect`].
struct PickTarget {
    entity: Entity,
    world_from_local: nalgebra::Matrix4<f32>,
    local_from_world: nalgebra::Matrix4<f32>,
    minimum: nalgebra::Vector3<f32>,
    maximum: nalgebra::Vector3<f32>,
}

fn pick_json(
    world: &World,
    pixel: [u32; 2],
    hit: Option<(f32, Entity, nalgebra::Vector3<f32>)>,
) -> Value {
    match hit {
        Some((distance, entity, point)) => json!({
            "pixel": pixel,
            "id": world.get::<SceneId>(entity).map(|id| id.0),
            "name": world.get::<Name>(entity).map(|name| &name.0),
            "distance": distance,
            "world_position": [point.x, point.y, point.z],
        }),
        None => json!({"pixel": pixel, "id": null}),
    }
}

/// A stored mix a scenario's audio must match.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct AudioReference {
    /// WAV file, relative to the scenario file.
    pub path: PathBuf,
    /// Largest RMS of the sample-by-sample difference that still passes.
    #[serde(default = "default_audio_tolerance")]
    pub tolerance: f32,
}

fn default_audio_tolerance() -> f32 {
    0.01
}

/// A scripted run: named inputs, checks and captures at fixed ticks.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Scenario {
    pub name: String,
    /// Rewrites `golden` images from this run instead of comparing; set
    /// by `rusting test --update-golden`.
    #[serde(skip)]
    pub update_golden: bool,
    /// Written to [`RandomSeed`] before the first tick.
    #[serde(default)]
    pub seed: u64,
    /// Last tick to simulate; the run covers ticks 0 through `ticks`.
    pub ticks: u32,
    /// Image size of every capture.
    #[serde(default = "default_capture_size")]
    pub capture_size: [u32; 2],
    #[serde(default)]
    pub steps: Vec<ScenarioStep>,
    /// Runs on after a failed step, so one run reports every failure. A
    /// check that failed is not repeated on later ticks.
    #[serde(default)]
    pub keep_going: bool,
    /// Every capture also writes `<name>.annotated.png`, with a box and
    /// short ID over each mesh entity, and `<name>.annotated.json`, which
    /// maps each short ID to the entity's name, ID and pixel box.
    #[serde(default)]
    pub annotate: bool,
    /// Writes one image with every `capture` frame in a grid, each labelled
    /// with its tick, so a single image shows motion. Path relative to the
    /// scenario file.
    #[serde(default)]
    pub contact_sheet: Option<PathBuf>,
    /// Writes everything the game played as a stereo 48 kHz WAV file. Path
    /// relative to the scenario file.
    #[serde(default)]
    pub audio_out: Option<PathBuf>,
    /// Compares the whole mix with a reference WAV, as `golden` does for
    /// images; `rusting test --update-golden` writes it.
    #[serde(default)]
    pub audio_reference: Option<AudioReference>,
    /// Limits on the run's cost; each one exceeded fails the run. Timing
    /// limits depend on the machine, so set them with headroom and read
    /// `perf.environment` before comparing runs.
    #[serde(default)]
    pub budgets: Option<Budgets>,
    /// Checked after every tick. The first tick one fails ends the run, or,
    /// with `keep_going`, is reported once. A missing entity or path passes,
    /// so a despawned object does not break an invariant about it; set
    /// `exists` to require it.
    #[serde(default)]
    pub invariants: Vec<Expectation>,
    /// Opens the headless Vulkan device even with no `capture` step, so GPU
    /// bodies simulate and GPU events arrive. Without it (or a capture) GPU
    /// bodies stay where they spawned. Fails the run when there is no
    /// Vulkan device; software Vulkan such as lavapipe works but is slow.
    #[serde(default)]
    pub gpu: bool,
    /// Files copied into the game's user data folder before tick 0, as
    /// `{"saves/1.txt": "fixtures/old_save.txt"}`: the key is the path
    /// `GameScene::load_data` reads, the value a file relative to the
    /// scenario file.
    #[serde(default)]
    pub files: BTreeMap<String, PathBuf>,
    /// Presses random actions: before each tick, each action flips between
    /// pressed and released with chance `rate`. The presses come from a
    /// stream seeded by `fuzz.seed`, so one seed always plays the same, and
    /// the report lists them as `fuzz_steps`. `rusting fuzz` sets it.
    #[serde(default)]
    pub fuzz: Option<Fuzz>,
    /// Walks the `PlayerController` toward each goal with the `player.*`
    /// actions, jumping when stuck, and fails naming the goals it could not
    /// reach. The report lists them as `explore`.
    #[serde(default)]
    pub explore: Option<Explore>,
    /// Runs one tick per fixed step of real time, as a played game does,
    /// instead of as fast as possible. Use it for a client that talks to a
    /// server running in real time, such as `rusting run --server`.
    /// [`crate::net::NetSession::simulate`] delays count from each tick's
    /// planned time, so a slow capture frame does not move them.
    #[serde(default)]
    pub realtime: bool,
}

/// A scenario's random input, see [`Scenario::fuzz`].
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Fuzz {
    #[serde(default)]
    pub seed: u64,
    /// Actions to press; empty means every action the scene binds. Name
    /// actions that game code binds in `update` here, since they are not
    /// bound before the first tick.
    #[serde(default)]
    pub actions: Vec<String>,
    #[serde(default = "default_fuzz_rate")]
    pub rate: f32,
}

fn default_fuzz_rate() -> f32 {
    0.1
}

impl Fuzz {
    /// The press and release steps of ticks 1 through `ticks`.
    pub fn steps(&self, actions: &[String], ticks: u32) -> Vec<ScenarioStep> {
        let stream = RandomSeed(self.seed);
        let mut held = vec![false; actions.len()];
        let mut steps = Vec::new();
        for tick in 1..=ticks {
            for (index, action) in actions.iter().enumerate() {
                if stream.unit(u64::from(tick), index as u64) >= self.rate {
                    continue;
                }
                held[index] = !held[index];
                let action = action.clone();
                steps.push(ScenarioStep {
                    tick,
                    until: None,
                    within: None,
                    at: None,
                    action: if held[index] {
                        StepAction::Press(action)
                    } else {
                        StepAction::Release(action)
                    },
                });
            }
        }
        steps
    }
}

/// A scenario's explorer bot, see [`Scenario::explore`].
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Explore {
    /// Entity names or IDs, or `[x, y, z]` points, to reach in order, so a
    /// list of points walks a route. Empty means every sensor collider,
    /// nearest first.
    #[serde(default)]
    pub goals: Vec<ExploreGoal>,
    /// Ticks to spend on one goal before calling it unreachable.
    #[serde(default = "default_goal_ticks")]
    pub ticks_per_goal: u32,
}

/// One goal of [`Explore`]: an entity by name or ID, or a world point.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(untagged)]
pub enum ExploreGoal {
    Entity(String),
    Point([f32; 3]),
}

impl From<&str> for ExploreGoal {
    fn from(name: &str) -> Self {
        Self::Entity(name.to_owned())
    }
}

/// Where an explorer goal is: a live entity or a fixed point.
#[derive(Clone, Copy)]
enum GoalSpot {
    Entity(Entity),
    Point([f32; 3]),
}

impl GoalSpot {
    fn at(self, world: &World) -> Option<[f32; 3]> {
        match self {
            Self::Entity(entity) => position(world, entity),
            Self::Point(point) => Some(point),
        }
    }
}

fn default_goal_ticks() -> u32 {
    600
}

/// Horizontal metres from a goal that count as reaching it.
const EXPLORE_REACH: f32 = 0.75;
/// Ticks without getting 5 cm closer to the goal that count as stuck.
const EXPLORE_STUCK_TICKS: u32 = 30;

/// Live state of [`Explore`] during a run.
struct Explorer {
    settings: Explore,
    /// Goals not yet tried; the first is the current one.
    pending: Vec<(GoalSpot, String)>,
    /// Set when the goal order was not given and the nearest goal must be
    /// picked again.
    resort: bool,
    started: u32,
    /// The closest distance to the current goal so far, and its tick.
    best: (f32, u32),
    held: Vec<&'static str>,
    goals: Vec<Value>,
    stuck: Vec<Value>,
}

fn position(world: &World, entity: Entity) -> Option<[f32; 3]> {
    let column = world.get::<GlobalTransform>(entity)?.matrix[3];
    Some([column[0], column[1], column[2]])
}

impl Explorer {
    fn new(world: &mut World, settings: &Explore) -> Result<Self, String> {
        let mut pending = Vec::new();
        for goal in &settings.goals {
            pending.push(match goal {
                ExploreGoal::Entity(name) => {
                    let entity = find_entity(world, name, |_, _| true)
                        .ok_or_else(|| {
                            format!("explore: no entity `{name}`")
                        })?;
                    (GoalSpot::Entity(entity), name.clone())
                }
                ExploreGoal::Point(point) => {
                    (GoalSpot::Point(*point), format!("{point:?}"))
                }
            });
        }
        let resort = pending.is_empty();
        if resort {
            let mut query = world.query::<(
                Entity,
                &crate::runtime::Collider,
                Option<&Name>,
                Option<&SceneId>,
            )>();
            let mut sensors: Vec<_> = query
                .iter(world)
                .filter(|(_, collider, ..)| collider.sensor)
                .map(|(entity, _, name, id)| {
                    let label = name.map_or_else(
                        || {
                            id.map_or(format!("{entity}"), |id| {
                                id.0.to_string()
                            })
                        },
                        |name| name.0.clone(),
                    );
                    (id.map(|id| id.0), entity, label)
                })
                .collect();
            sensors.sort();
            pending = sensors
                .into_iter()
                .map(|(_, e, label)| (GoalSpot::Entity(e), label))
                .collect();
        }
        Ok(Self {
            settings: settings.clone(),
            pending,
            resort,
            started: 0,
            best: (f32::MAX, 0),
            held: Vec::new(),
            goals: Vec::new(),
            stuck: Vec::new(),
        })
    }

    /// Presses the actions that walk toward the current goal, before the
    /// tick's update.
    fn steer(&mut self, world: &mut World, tick: u32) -> Result<(), String> {
        use crate::runtime::{
            PLAYER_BACK, PLAYER_FORWARD, PLAYER_JUMP, PLAYER_LEFT, PLAYER_RIGHT,
        };
        let mut query =
            world.query::<(Entity, &crate::runtime::PlayerController)>();
        let Some((player, controller)) = query.iter(world).next() else {
            return Err(
                "explore needs an entity with a PlayerController".into()
            );
        };
        let yaw = controller.yaw;
        let Some(at) = position(world, player) else {
            return Ok(());
        };
        let mut wanted = Vec::new();
        loop {
            if self.resort {
                self.resort = false;
                // A stable sort keeps scene-ID order between equal distances.
                self.pending.sort_by(|a, b| {
                    let distance = |spot: GoalSpot| {
                        spot.at(world).map_or(f32::MAX, |p| {
                            (p[0] - at[0]).hypot(p[2] - at[2])
                        })
                    };
                    distance(a.0).total_cmp(&distance(b.0))
                });
            }
            let Some(&(goal, ref name)) = self.pending.first() else {
                break;
            };
            let target = goal.at(world);
            let reached = target.is_none_or(|target| {
                (target[0] - at[0]).hypot(target[2] - at[2]) < EXPLORE_REACH
            });
            let timed_out = tick - self.started > self.settings.ticks_per_goal;
            if !reached && !timed_out {
                let target = target.unwrap_or(at);
                let (dx, dz) = (target[0] - at[0], target[2] - at[2]);
                let (sin, cos) = crate::runtime::sim_math::sin_cos(yaw);
                // Forward is -Z turned by yaw; right is +X turned by yaw.
                let forward = -sin * dx - cos * dz;
                let right = cos * dx - sin * dz;
                let dead = 0.2;
                for (amount, positive, negative) in [
                    (forward, PLAYER_FORWARD, PLAYER_BACK),
                    (right, PLAYER_RIGHT, PLAYER_LEFT),
                ] {
                    if amount > dead {
                        wanted.push(positive);
                    } else if amount < -dead {
                        wanted.push(negative);
                    }
                }
                let distance = dx.hypot(dz);
                if distance < self.best.0 - 0.05 {
                    self.best = (distance, tick);
                } else if tick - self.best.1 >= EXPLORE_STUCK_TICKS {
                    self.stuck.push(serde_json::json!({
                        "tick": tick, "goal": name, "position": at,
                    }));
                    self.best.1 = tick;
                    wanted.push(PLAYER_JUMP);
                }
                break;
            }
            self.goals.push(serde_json::json!({
                "name": name,
                "reached": reached.then_some(tick),
            }));
            self.pending.remove(0);
            self.started = tick;
            self.best = (f32::MAX, tick);
            self.resort |= self.settings.goals.is_empty();
        }
        for action in self.held.clone() {
            if !wanted.contains(&action) {
                press(world, action, false, None)?;
            }
        }
        for &action in &wanted {
            if !self.held.contains(&action) {
                press(world, action, true, None)?;
            }
        }
        self.held = wanted;
        Ok(())
    }

    /// Goals left when the run ends count as unreachable.
    fn finish(mut self) -> (Value, Vec<String>) {
        let mut missed: Vec<String> = Vec::new();
        for (_, name) in self.pending.drain(..) {
            self.goals
                .push(serde_json::json!({"name": name, "reached": null}));
        }
        for goal in &self.goals {
            if goal["reached"].is_null() {
                missed.push(goal["name"].as_str().unwrap_or_default().into());
            }
        }
        (
            serde_json::json!({"goals": self.goals, "stuck": self.stuck}),
            missed,
        )
    }
}

/// A scenario that presses and releases the named actions a recorded play
/// session held, on the ticks it held them, so a human can hand a bug over
/// as a test. A press and release between two frames becomes a `tap`.
// ponytail: actions only; sticks and the cursor are not converted, add
// them when a pointer-driven bug needs recording.
#[must_use]
pub fn scenario_from_replay(
    name: &str,
    replay: &crate::runtime::Replay,
    map: &ActionMap,
) -> serde_json::Value {
    let mut held = std::collections::BTreeSet::new();
    let mut steps = Vec::new();
    for frame in &replay.frames {
        let Some(input) = &frame.input else { continue };
        let tick = frame.tick.saturating_sub(replay.start_tick);
        for action in map.actions() {
            let now = map.held(input, action);
            let kind = match (held.contains(action), now) {
                (false, true) => "press",
                (true, false) => "release",
                (false, false) if map.just_pressed(input, action) => "tap",
                _ => continue,
            };
            if now {
                held.insert(action.to_owned());
            } else {
                held.remove(action);
            }
            steps.push(serde_json::json!({"tick": tick, kind: action}));
        }
    }
    let ticks = replay
        .frames
        .last()
        .map_or(0, |frame| frame.tick.saturating_sub(replay.start_tick));
    serde_json::json!({
        "name": name,
        "seed": replay.seed,
        "ticks": ticks,
        "steps": steps,
    })
}

/// Limits on [`PerfReport`] values. Draw and triangle limits apply to the
/// last rendered frame; setting one renders every tick, and without Vulkan
/// they are ignored.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Budgets {
    pub max_tick_ms: Option<f64>,
    pub mean_tick_ms: Option<f64>,
    pub p95_tick_ms: Option<f64>,
    /// Limit on [`PerfReport::cpu_ms_mean`]; not checked where it is null.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mean_cpu_ms: Option<f64>,
    pub max_draws: Option<u32>,
    pub max_triangles: Option<u64>,
    /// Limit on `perf.render.lights`; renders every tick like `max_draws`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_lights: Option<u32>,
    /// Limit on [`PerfReport::entities_max`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_entities: Option<u64>,
}

/// What a run cost, in a stable JSON shape.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub struct PerfReport {
    /// Engine version, OS, architecture, build profile and, when a frame was
    /// rendered, the device and driver.
    pub environment: Value,
    /// Wall-clock milliseconds of each whole tick (simulation, and rendering
    /// when the scenario captures).
    pub tick_ms_mean: f64,
    pub tick_ms_p95: f64,
    pub tick_ms_max: f64,
    /// Wall-clock milliseconds of the whole run per tick, including the
    /// checks, captures and logs between ticks that `tick_ms_*` leave out.
    #[serde(default)]
    pub wall_ms_mean: f64,
    /// CPU milliseconds of every game thread over the whole run per tick,
    /// counted like `wall_ms_mean`. Other processes slow it far less than
    /// wall time. Linux only, in 10 ms steps over the run; null elsewhere.
    #[serde(default)]
    pub cpu_ms_mean: Option<f64>,
    /// Mean CPU milliseconds per tick in each part of the frame: `fixed`
    /// (physics and the game's fixed systems), `update` (the game's
    /// systems), `post_update` (transform propagation and other engine
    /// work) and `extract` (copying the scene for the renderer).
    #[serde(default)]
    pub stages_ms_mean: Value,
    /// The most live entities after any tick: scene objects, spawned
    /// copies, counters and other game entities, not engine resources.
    #[serde(default)]
    pub entities_max: u64,
    /// Resident memory of the game process in MiB at the end of the run
    /// (`rss_mb`) and its peak since the process started (`rss_mb_peak`),
    /// so a soak run can show the game does not grow. Linux only; null
    /// elsewhere.
    #[serde(default)]
    pub rss_mb: Option<f64>,
    #[serde(default)]
    pub rss_mb_peak: Option<f64>,
    /// Draws, triangles, visible instances and GPU milliseconds (`gpu_ms`)
    /// of the last rendered frame; `gpu_ms_p50`, `gpu_ms_p95` and
    /// `gpu_ms_max` over all `gpu_frames` frames drawn (every tick with a render
    /// budget or `gpu`, else the few ticks before each image step; GPU
    /// physics steps on the other ticks without drawing); `passes_ms`, the
    /// last frame's GPU milliseconds per pass (`Shadow`, `Scene`,
    /// `ToneMap`..) and per part of a pass (`Shadow.cascade0`,
    /// `Scene.opaque`, `Scene.terrain`, `Scene.foliage`, `Scene.sky`); and
    /// `cameras`:
    /// `[{name, gpu_ms, draws, triangles}]`, one per viewport camera, then
    /// one per camera screen drawn that frame (with `"screen": true`);
    /// and `dropped_lights`, the lights past the frame's light cap. Null
    /// without a renderer (no capture step, `gpu`, or render budget).
    pub render: Value,
}

fn default_capture_size() -> [u32; 2] {
    [1280, 720]
}

/// One step, for example `{"tick": 10, "press": "jump"}`.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ScenarioStep {
    /// Inputs apply before this tick's update; checks and captures run
    /// after it.
    pub tick: u32,
    /// Repeats a check on every tick through this one, so a failure
    /// reports the first tick it stopped holding.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub until: Option<u32>,
    /// Passes on the first tick through this one where the check holds, so
    /// a test need not know the exact tick something happens.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub within: Option<u32>,
    /// For `press` and `tap`: the fraction of the tick (0 to 1) the press
    /// lands at, so `GameScene::press_tick` gives `tick + at`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<f64>,
    #[serde(flatten)]
    pub action: StepAction,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StepAction {
    /// Presses every input bound to a named action.
    Press(String),
    /// Releases every input bound to a named action.
    Release(String),
    /// Presses a named action and releases it before the next tick, so
    /// each tap gives `pressed` one edge.
    Tap(String),
    /// Checks one value of an entity's reflected scene state.
    Expect(Expectation),
    /// Counts events seen by gameplay from tick 0 through this tick.
    ExpectEvents(EventExpectation),
    /// Writes the frame to a path relative to the scenario file: either
    /// the path alone or `{"path", "camera", "hud", "golden", "tolerance"}`.
    Capture(CaptureStep),
    /// Changes one value before the tick's update, to set up a test: move
    /// the player, fill a counter.
    Set(Assignment),
    /// Clicks the topmost UI text that equals this label (an egui button,
    /// a HUD button, or text game code drew): moves the cursor to its
    /// center, presses the left mouse button, and releases it before the
    /// next tick. Fails listing the texts on screen when none matches.
    /// The object form `{"text" or "starts_with", "index"}` matches a
    /// prefix and picks the nth match in reading order.
    Click(ClickTarget),
    /// Puts the scene back as the game started, as `GameScene::restart`
    /// does, before this tick's update. Files in the user data folder stay,
    /// so a save made earlier can be loaded after it.
    Restart(bool),
    /// Checks that game code called `GameScene::quit` (true) or did not
    /// (false). A quit ends the run after that tick's checks; steps at
    /// later ticks then fail.
    ExpectQuit(bool),
    /// Checks a file in the user data folder, such as a save the game
    /// wrote. Unlike other checks it may run after the game quits.
    ExpectFile(FileExpectation),
    /// Moves the mouse cursor to this point of the view, as fractions of
    /// its width and height from the top-left corner: `[0.5, 0.5]` is the
    /// center.
    Pointer([f32; 2]),
    /// Tilts the gamepad's left stick, each axis -1..1 (`[0, 1]` is fully
    /// up), until another step moves it. Past half tilt it also presses the
    /// stick's direction (`PadLeftStickUp`, ...).
    LeftStick([f32; 2]),
    /// Tilts the gamepad's right stick, as `left_stick` does.
    RightStick([f32; 2]),
    /// Gives the window focus (true) or takes it away (false) before the
    /// tick's update, as alt-tab does: losing it releases held inputs and
    /// makes `GameScene::window_focused` false.
    Focus(bool),
    /// Checks where an entity is in the frame: on screen, inside a screen
    /// rectangle, covering a share of the frame, or hidden behind others.
    ExpectScreen(ScreenExpectation),
    /// Checks the mean color and spread of a region of the frame.
    ExpectPixels(PixelExpectation),
    /// Records one value of an entity's scene form in the report, on every
    /// tick through `until`. Never fails; use it to debug a scenario.
    Log(LoggedValue),
}

/// A `click` target: a label, or `{"text": "-", "index": 2}` /
/// `{"starts_with": "Slot 1"}`.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(untagged)]
pub enum ClickTarget {
    Label(String),
    Find {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        text: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        starts_with: Option<String>,
        /// 0-based, in reading order (top to bottom, then left to right).
        /// Copies drawn within 4 px of each other count once.
        /// Without it the topmost drawn match wins.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        index: Option<usize>,
    },
}

/// A `capture` step: `"shots/a.png"` or the object form.
#[derive(Clone, Debug, Serialize)]
pub struct CaptureStep {
    pub path: PathBuf,
    /// Renders through this camera (ID or name), full frame, in place of
    /// the game's own view. Such a capture has no HUD, unless the camera is
    /// the one the game already shows in a single view.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub camera: Option<String>,
    /// False leaves the HUD and other runtime UI out.
    #[serde(default = "yes")]
    pub hud: bool,
    /// Compares the capture with this image, relative to the scenario file.
    /// `rusting test --update-golden` writes it instead.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub golden: Option<PathBuf>,
    /// Largest of the three per-channel mean differences, 0 to 255, that still
    /// matches the golden image.
    #[serde(default = "default_tolerance")]
    pub tolerance: f64,
}

fn yes() -> bool {
    true
}

fn default_tolerance() -> f64 {
    1.0
}

impl<'de> Deserialize<'de> for CaptureStep {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Full {
            path: PathBuf,
            #[serde(default)]
            camera: Option<String>,
            #[serde(default = "yes")]
            hud: bool,
            #[serde(default)]
            golden: Option<PathBuf>,
            #[serde(default = "default_tolerance")]
            tolerance: f64,
        }
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Form {
            Path(PathBuf),
            Full(Full),
        }
        Ok(match Form::deserialize(deserializer)? {
            Form::Path(path) => CaptureStep {
                path,
                camera: None,
                hud: true,
                golden: None,
                tolerance: default_tolerance(),
            },
            Form::Full(f) => CaptureStep {
                path: f.path,
                camera: f.camera,
                hud: f.hud,
                golden: f.golden,
                tolerance: f.tolerance,
            },
        })
    }
}

/// An `expect_pixels` step. Colors are sRGB, 0 to 255.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct PixelExpectation {
    /// `[left, top, right, bottom]` as fractions of the frame, or of the
    /// viewport of `camera` when it is set. The whole frame by default.
    #[serde(default = "whole_frame")]
    pub region: [f32; 4],
    /// Makes `region` relative to this camera's viewport (ID or name).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub camera: Option<String>,
    /// Lowest mean `[r, g, b]` of the region.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mean_min: Option<[f64; 3]>,
    /// Highest mean `[r, g, b]` of the region.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mean_max: Option<[f64; 3]>,
    /// Lowest standard deviation of the region's brightness: above 0 the
    /// region is not one flat color.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stddev_min: Option<f64>,
    /// Highest standard deviation of the region's brightness.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stddev_max: Option<f64>,
    /// An earlier capture (relative to the scenario file) to compare the
    /// same region with, by `difference_min` and `difference_max`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub differs_from: Option<PathBuf>,
    /// Lowest largest-channel mean difference (0..255) from `differs_from`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub difference_min: Option<f64>,
    /// Highest largest-channel mean difference (0..255) from `differs_from`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub difference_max: Option<f64>,
}

fn whole_frame() -> [f32; 4] {
    [0.0, 0.0, 1.0, 1.0]
}

/// An `expect_file` step: `{"path": "settings.cfg", "contains": "volume=60"}`.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FileExpectation {
    /// Relative to the user data folder.
    pub path: String,
    /// `false` checks that the file does not exist.
    #[serde(default = "yes")]
    pub exists: bool,
    /// Text the file must contain.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contains: Option<String>,
}

fn check_file(expect: &FileExpectation) -> Check {
    let path = crate::project::user_data_folder().join(&expect.path);
    let text = std::fs::read_to_string(&path).ok();
    let shown = path.display();
    match (&text, expect.exists, &expect.contains) {
        (None, true, _) => {
            Err((format!("{shown} does not exist"), Value::Null))
        }
        (Some(_), false, _) => {
            Err((format!("{shown} exists"), Value::Bool(true)))
        }
        (None, false, _) => Ok(format!("{shown} does not exist")),
        (Some(text), true, Some(wanted)) if !text.contains(wanted.as_str()) => {
            Err((
                format!("{shown} does not contain `{wanted}`"),
                Value::String(text.chars().take(400).collect()),
            ))
        }
        (Some(_), true, _) => Ok(format!("{shown} is as expected")),
    }
}

/// The value a `log` step records.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct LoggedValue {
    /// Entity name or scene ID.
    #[serde(default)]
    pub entity: String,
    /// Counter name, in place of `entity` and `path`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub counter: Option<String>,
    /// JSON pointer into the entity's scene form, as for `expect`.
    #[serde(default)]
    pub path: String,
}

/// A write to `/transform/...` or `/components/<name>/...` of an entity's
/// scene form.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Assignment {
    /// Persistent ID or name.
    #[serde(default)]
    pub entity: String,
    /// Counter name, in place of `entity` and `path`: sets its value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub counter: Option<String>,
    #[serde(default)]
    pub path: String,
    pub value: Value,
}

/// A check on the entity's scene form, the same JSON that scene files and
/// `rusting scene query` use. Registered components are parsed, so
/// `/components/rusting.player_controller/move_speed` reaches a field.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Expectation {
    /// Persistent ID or name.
    #[serde(default)]
    pub entity: String,
    /// Counter name, in place of `entity` and `path`: checks its value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub counter: Option<String>,
    /// JSON pointer, for example `/transform/position/1`. Empty for the
    /// whole entity.
    #[serde(default)]
    pub path: String,
    /// Whether the entity and path exist, for example `false` for an object
    /// the game despawned.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exists: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub equals: Option<Value>,
    /// Fails when the value equals this (with `tolerance` for numbers).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub not_equals: Option<Value>,
    /// Passes when the number is greater; an array compares its length.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub greater_than: Option<f64>,
    /// Passes when the number is less; an array compares its length.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub less_than: Option<f64>,
    /// Allowed difference when `equals` compares numbers, also each number
    /// inside an array or object such as a position.
    #[serde(default)]
    pub tolerance: f64,
    /// No number at the path is NaN or infinite; JSON reads those back as
    /// `null`, so a `null` inside a vector fails.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub finite: bool,
}

/// A check on where a mesh entity appears in the frame of the active
/// camera, at `capture_size`. Uses bounds picking like `capture --pick`.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ScreenExpectation {
    /// Persistent ID or name.
    pub entity: String,
    /// Some of the entity is visible: its projected box touches the frame
    /// and at least one point of the box shows it in front of others.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_screen: Option<bool>,
    /// Its box touches the frame but all of it is behind other objects.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub occluded: Option<bool>,
    /// Its clipped box lies inside this rectangle, as fractions of the frame
    /// `[left, top, right, bottom]`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inside: Option<[f32; 4]>,
    /// The share of the whole frame the entity is the nearest hit for, 0 to 1.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_share: Option<f64>,
    /// Projects through this camera (ID or name) instead of the active
    /// one. `inside` and `min_share` are then fractions of its viewport.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub camera: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct EventExpectation {
    pub kind: EventKind,
    /// Counts only events that involve this entity (persistent ID or name).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entity: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at_least: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at_most: Option<usize>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    /// CPU physics [`CollisionEvent`]s.
    Collision,
}

/// Turns a `counter` shorthand into the entity and path it stands for.
fn resolve_counter(
    entity: &mut String,
    path: &mut String,
    counter: Option<&String>,
) {
    if let Some(counter) = counter {
        *entity = format!("{COUNTER_PREFIX}{counter}");
        if path.is_empty() {
            COUNTER_VALUE.clone_into(path);
        }
    }
}

impl Scenario {
    /// A copy where every `counter` shorthand names its entity and path.
    #[must_use]
    pub fn with_counters_resolved(&self) -> Self {
        let mut scenario = self.clone();
        for step in &mut scenario.steps {
            match &mut step.action {
                StepAction::Expect(e) => {
                    resolve_counter(
                        &mut e.entity,
                        &mut e.path,
                        e.counter.as_ref(),
                    );
                }
                StepAction::Set(e) => {
                    resolve_counter(
                        &mut e.entity,
                        &mut e.path,
                        e.counter.as_ref(),
                    );
                }
                StepAction::Log(e) => {
                    resolve_counter(
                        &mut e.entity,
                        &mut e.path,
                        e.counter.as_ref(),
                    );
                }
                _ => {}
            }
        }
        for e in &mut scenario.invariants {
            resolve_counter(&mut e.entity, &mut e.path, e.counter.as_ref());
        }
        scenario
    }

    /// Rejects steps that could never run or never fail.
    pub fn validate(&self) -> Result<(), String> {
        for (index, step) in self.steps.iter().enumerate() {
            let fail = |message: &str| Err(format!("step {index}: {message}"));
            let last = step.until.or(step.within).unwrap_or(step.tick);
            if last > self.ticks || last < step.tick {
                return fail("tick or until is outside the scenario");
            }
            let repeatable = matches!(
                step.action,
                StepAction::Expect(_)
                    | StepAction::ExpectEvents(_)
                    | StepAction::ExpectScreen(_)
                    | StepAction::ExpectPixels(_)
            );
            if step.within.is_some() && !repeatable
                || step.until.is_some()
                    && !repeatable
                    && !matches!(step.action, StepAction::Log(_))
            {
                return fail(
                    "only expect and log steps accept until or within",
                );
            }
            if step.until.is_some() && step.within.is_some() {
                return fail("until and within cannot both be set");
            }
            let target = match &step.action {
                StepAction::Expect(e) => Some((&e.entity, &e.counter)),
                StepAction::Set(e) => Some((&e.entity, &e.counter)),
                StepAction::Log(e) => Some((&e.entity, &e.counter)),
                _ => None,
            };
            if let Some((entity, counter)) = target {
                if entity.is_empty() == counter.is_none() {
                    return fail("give either entity or counter");
                }
            }
            match &step.action {
                StepAction::Expect(expect)
                    if expect.equals.is_none()
                        && expect.not_equals.is_none()
                        && expect.greater_than.is_none()
                        && expect.less_than.is_none()
                        && expect.exists.is_none() =>
                {
                    return fail(
                        "expect needs equals, not_equals, greater_than, less_than or exists",
                    )
                }
                StepAction::ExpectScreen(expect)
                    if expect.on_screen.is_none()
                        && expect.occluded.is_none()
                        && expect.inside.is_none()
                        && expect.min_share.is_none() =>
                {
                    return fail(
                        "expect_screen needs on_screen, occluded, inside or min_share",
                    )
                }
                StepAction::ExpectPixels(expect)
                    if expect.differs_from.is_some()
                        != (expect.difference_min.is_some()
                            || expect.difference_max.is_some()) =>
                {
                    return fail(
                        "expect_pixels differs_from goes with difference_min or difference_max",
                    )
                }
                StepAction::ExpectPixels(expect)
                    if expect.mean_min.is_none()
                        && expect.mean_max.is_none()
                        && expect.stddev_min.is_none()
                        && expect.stddev_max.is_none()
                        && expect.differs_from.is_none() =>
                {
                    return fail(
                        "expect_pixels needs mean_min, mean_max, stddev_min, stddev_max or differs_from",
                    )
                }
                StepAction::ExpectEvents(expect)
                    if expect.at_least.is_none()
                        && expect.at_most.is_none() =>
                {
                    return fail("expect_events needs at_least or at_most")
                }
                _ => {}
            }
        }
        for (index, invariant) in self.invariants.iter().enumerate() {
            if invariant.entity.is_empty() == invariant.counter.is_none() {
                return Err(format!(
                    "invariant {index}: give either entity or counter"
                ));
            }
            if invariant.equals.is_none()
                && invariant.not_equals.is_none()
                && invariant.greater_than.is_none()
                && invariant.less_than.is_none()
                && invariant.exists.is_none()
                && !invariant.finite
            {
                return Err(format!(
                    "invariant {index}: needs equals, not_equals, \
                     greater_than, less_than, exists or finite"
                ));
            }
        }
        if self.capture_size.contains(&0) {
            return Err("capture_size must be at least 1x1".into());
        }
        Ok(())
    }
}

/// Outcome of one step on one tick.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct StepResult {
    pub tick: u32,
    /// Index into the scenario's steps.
    pub step: usize,
    pub ok: bool,
    pub message: String,
    /// The value a failed check saw.
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub actual: Value,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct ScenarioReport {
    pub name: String,
    pub seed: u64,
    pub passed: bool,
    /// Fixed ticks simulated before the run ended.
    pub ticks_run: u64,
    /// The first failed step; the run stops there unless `keep_going`.
    pub first_failure: Option<StepResult>,
    /// Every step result in order, ending at the first failure unless
    /// `keep_going`. Repeated
    /// checks appear once, at their last passing tick or their failure.
    pub steps: Vec<StepResult>,
    pub captures: Vec<PathBuf>,
    /// What happened, in tick order: inputs pressed or released, values set
    /// by steps and collisions between entities. At most `TRACE_LIMIT`
    /// entries; later events are dropped.
    #[serde(default)]
    pub trace: Vec<TraceEvent>,
    /// Timing and render counters of the run.
    #[serde(default)]
    pub perf: PerfReport,
    /// `(tick, hash)` of the GPU body buffers, hashed on the GPU after each
    /// tick, for `gpu` runs. Readback lags one to three frames, so the last
    /// ticks may be missing.
    #[serde(default)]
    pub gpu_state_hashes: Vec<(u64, u64)>,
    /// `(tick, hash)` of the CPU-visible world state after each tick.
    #[serde(default)]
    pub state_hashes: Vec<(u64, u64)>,
    /// The steps a `fuzz` scenario pressed, in the scenario step form.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fuzz_steps: Vec<Value>,
    /// What the run exercised, comparing the scene before tick 0 with the
    /// scene at the end: `entities_changed` (`{name, id, sections}`),
    /// `entities_added`, `entities_removed`, `entities_unchanged` (a
    /// count), `sections_changed` and `sections_untouched` (scene sections
    /// and components no entity changed), `inputs` (actions pressed) and
    /// `events` (trace entries by kind). A value that changed and came back
    /// counts as unchanged.
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub coverage: Value,
    /// With [`Scenario::explore`]: `goals` (each `name` and the `reached`
    /// tick or null) and `stuck` (tick, goal and position where the player
    /// stopped moving and jumped).
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub explore: Value,
}

/// Most trace entries one report keeps.
pub const TRACE_LIMIT: usize = 1000;

/// One entry of a scenario's event trace.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct TraceEvent {
    pub tick: u32,
    /// `input`, `set` or `collision`.
    pub kind: String,
    pub detail: Value,
}

/// One collision seen by gameplay, with persistent IDs.
struct SeenEvent {
    a: Option<Uuid>,
    b: Option<Uuid>,
}

/// Saves the frame, and with `annotate` its annotated copy and legend.
fn save_capture(
    pixels: &[u8],
    world: &mut World,
    scenario: &Scenario,
    path: &Path,
    saved: &mut Vec<PathBuf>,
) -> Check {
    crate::rendering::capture::save_rgba(path, pixels, scenario.capture_size)
        .map_err(|error| (error, Value::Null))?;
    saved.push(path.to_path_buf());
    if scenario.annotate {
        let extent = scenario.capture_size;
        let notes = crate::annotate::annotations(world, extent);
        let mut pixels = pixels.to_vec();
        crate::annotate::draw(&mut pixels, extent, &notes);
        let image = path.with_extension("annotated.png");
        let legend = path.with_extension("annotated.json");
        let wrote = image::save_buffer(
            &image,
            &pixels,
            extent[0],
            extent[1],
            image::ExtendedColorType::Rgba8,
        )
        .map_err(|error| error.to_string())
        .and_then(|()| {
            let data: Vec<_> = notes.iter().map(|n| n.data()).collect();
            std::fs::write(
                &legend,
                serde_json::to_string_pretty(&data).unwrap(),
            )
            .map_err(|error| error.to_string())
        });
        wrote.map_err(|error| (error, Value::Null))?;
        saved.extend([image, legend]);
    }
    Ok(format!("captured {}", path.display()))
}

/// Compares a capture with its golden image by the mean difference of each
/// color channel, or writes the golden image when `update` is set.
fn compare_golden(
    pixels: &[u8],
    extent: [u32; 2],
    golden: &Path,
    tolerance: f64,
    update: bool,
) -> Check {
    if update {
        crate::rendering::capture::save_rgba(golden, pixels, extent)
            .map_err(|error| (error, Value::Null))?;
        return Ok(format!("; wrote golden {}", golden.display()));
    }
    let image = image::open(golden)
        .map_err(|error| {
            (
                format!(
                    "cannot read golden {}: {error}; `rusting test \
                     --update-golden` writes it",
                    golden.display()
                ),
                Value::Null,
            )
        })?
        .into_rgba8();
    if image.dimensions() != (extent[0], extent[1]) {
        return Err((
            format!(
                "golden {} is {:?}, the capture is {extent:?}",
                golden.display(),
                image.dimensions()
            ),
            Value::Null,
        ));
    }
    let mut sums = [0.0_f64; 3];
    let mut max = 0_u8;
    for (a, b) in pixels.chunks_exact(4).zip(image.as_raw().chunks_exact(4)) {
        for channel in 0..3 {
            let difference = a[channel].abs_diff(b[channel]);
            sums[channel] += f64::from(difference);
            max = max.max(difference);
        }
    }
    let count = f64::from(extent[0] * extent[1]).max(1.0);
    let mean = sums.map(|sum| sum / count);
    let worst = mean.iter().copied().fold(0.0, f64::max);
    let actual = json!({"mean_difference": mean, "max_difference": max});
    if worst > tolerance {
        return Err((
            format!(
                "capture differs from golden {}: largest channel mean \
                 difference {worst:.3} (R {:.3}, G {:.3}, B {:.3}), \
                 tolerance {tolerance}",
                golden.display(),
                mean[0],
                mean[1],
                mean[2]
            ),
            actual,
        ));
    }
    Ok(format!(
        "; matches golden {} (mean difference {worst:.3})",
        golden.display()
    ))
}

/// Compares a run's mix with a reference WAV by the RMS of their
/// difference, or writes the reference when `update` is set.
fn compare_mix(
    mixed: &[f32],
    reference: &Path,
    tolerance: f32,
    update: bool,
) -> Check {
    if update {
        crate::audio_output::write_wav(reference, mixed)
            .map_err(|error| (error.to_string(), Value::Null))?;
        return Ok(format!("wrote audio reference {}", reference.display()));
    }
    let stored = crate::audio_output::read_wav(reference).map_err(|error| {
        (
            format!(
                "cannot read audio reference {}: {error}; `rusting test \
                 --update-golden` writes it",
                reference.display()
            ),
            Value::Null,
        )
    })?;
    if stored.len() != mixed.len() {
        return Err((
            format!(
                "audio reference {} has {} samples, the mix has {}",
                reference.display(),
                stored.len(),
                mixed.len()
            ),
            Value::Null,
        ));
    }
    let sum: f32 = mixed
        .iter()
        .zip(&stored)
        .map(|(a, b)| (a - b) * (a - b))
        .sum();
    let rms = (sum / mixed.len().max(1) as f32).sqrt();
    if rms > tolerance {
        return Err((
            format!(
                "mix differs from audio reference {}: RMS difference \
                 {rms:.5}, tolerance {tolerance}",
                reference.display()
            ),
            json!({"rms_difference": rms}),
        ));
    }
    Ok(format!(
        "matches audio reference {} (RMS difference {rms:.5})",
        reference.display()
    ))
}

/// Mean `[r, g, b]` and brightness standard deviation of a pixel
/// rectangle `[x0, y0, x1, y1]`, end exclusive.
fn region_stats(pixels: &[u8], width: u32, rect: [u32; 4]) -> ([f64; 3], f64) {
    let mut sums = [0.0_f64; 3];
    let mut luma = Vec::new();
    for y in rect[1]..rect[3] {
        for x in rect[0]..rect[2] {
            let at = ((y * width + x) * 4) as usize;
            let rgb = [0, 1, 2].map(|c| f64::from(pixels[at + c]));
            for c in 0..3 {
                sums[c] += rgb[c];
            }
            luma.push(0.2126 * rgb[0] + 0.7152 * rgb[1] + 0.0722 * rgb[2]);
        }
    }
    let count = luma.len().max(1) as f64;
    let mean_luma = luma.iter().sum::<f64>() / count;
    let variance = luma
        .iter()
        .map(|value| (value - mean_luma).powi(2))
        .sum::<f64>()
        / count;
    (sums.map(|sum| sum / count), variance.sqrt())
}

/// The largest of the R, G and B mean differences of two frames in `rect`.
fn region_difference(a: &[u8], b: &[u8], width: u32, rect: [u32; 4]) -> f64 {
    let mut sums = [0.0_f64; 3];
    for y in rect[1]..rect[3] {
        for x in rect[0]..rect[2] {
            let at = ((y * width + x) * 4) as usize;
            for c in 0..3 {
                sums[c] += f64::from(a[at + c].abs_diff(b[at + c]));
            }
        }
    }
    let count = f64::from((rect[2] - rect[0]) * (rect[3] - rect[1])).max(1.0);
    sums.iter().map(|sum| sum / count).fold(0.0, f64::max)
}

fn check_pixels(
    world: &mut World,
    expect: &PixelExpectation,
    pixels: &[u8],
    extent: [u32; 2],
    base: &Path,
) -> Check {
    // The region is a fraction of the camera's viewport, or of the frame.
    let mut frame = [0.0, 0.0, 1.0, 1.0];
    if let Some(name) = &expect.camera {
        let camera = find_camera(world, name).ok_or_else(|| {
            (
                format!("no camera has the ID or name `{name}`"),
                Value::Null,
            )
        })?;
        if let Some(viewport) =
            world.get::<Camera>(camera).and_then(|c| c.viewport)
        {
            frame = viewport;
        }
    }
    let [l, t, r, b] = expect.region;
    let pixel = |fraction: f32, start: f32, size: f32, side: u32| {
        (((start + fraction * size) * side as f32).round() as u32).min(side)
    };
    let rect = [
        pixel(l, frame[0], frame[2], extent[0]),
        pixel(t, frame[1], frame[3], extent[1]),
        pixel(r, frame[0], frame[2], extent[0]),
        pixel(b, frame[1], frame[3], extent[1]),
    ];
    if rect[0] >= rect[2] || rect[1] >= rect[3] {
        return Err((
            format!("region {:?} covers no pixels", expect.region),
            Value::Null,
        ));
    }
    let (mean, stddev) = region_stats(pixels, extent[0], rect);
    let mut actual = json!({"rect": rect, "mean": mean, "stddev": stddev});
    if let Some(earlier) = &expect.differs_from {
        let path = base.join(earlier);
        let image = image::open(&path)
            .map_err(|error| {
                (
                    format!("cannot read {}: {error}", path.display()),
                    Value::Null,
                )
            })?
            .into_rgba8();
        if image.dimensions() != (extent[0], extent[1]) {
            return Err((
                format!(
                    "{} is {:?}, the capture is {extent:?}",
                    path.display(),
                    image.dimensions()
                ),
                Value::Null,
            ));
        }
        actual["difference"] =
            json!(region_difference(pixels, image.as_raw(), extent[0], rect));
    }
    let difference = actual["difference"].as_f64().unwrap_or(0.0);
    let fail = |wanted: String| {
        Err((
            format!("pixels are {actual}, expected {wanted}"),
            actual.clone(),
        ))
    };
    if let Some(min) = expect.mean_min {
        if (0..3).any(|c| mean[c] < min[c]) {
            return fail(format!("mean_min {min:?}"));
        }
    }
    if let Some(max) = expect.mean_max {
        if (0..3).any(|c| mean[c] > max[c]) {
            return fail(format!("mean_max {max:?}"));
        }
    }
    if expect.stddev_min.is_some_and(|min| stddev < min) {
        return fail(format!("stddev_min {:?}", expect.stddev_min));
    }
    if expect.stddev_max.is_some_and(|max| stddev > max) {
        return fail(format!("stddev_max {:?}", expect.stddev_max));
    }
    if expect.difference_min.is_some_and(|min| difference < min) {
        return fail(format!("difference_min {:?}", expect.difference_min));
    }
    if expect.difference_max.is_some_and(|max| difference > max) {
        return fail(format!("difference_max {:?}", expect.difference_max));
    }
    Ok(format!("pixels are {actual} as expected"))
}

/// Runs `scenario` on a loaded game. Relative capture paths resolve against
/// `base`, normally the scenario file's folder.
/// Ticks rendered before each capture or pixel check of a scenario. One
/// fills occlusion culling's history; the rest let a camera screen with a
/// small `update_every` draw its feed.
pub(crate) const RENDER_WARMUP_TICKS: u32 = 4;

/// GPU bodies advance only through the renderer, so a run with any steps
/// GPU physics every tick, drawn or not.
pub(crate) fn has_gpu_bodies(world: &mut World) -> bool {
    world
        .query::<&crate::runtime::PhysicsBody>()
        .iter(world)
        .any(|body| body.simulation == crate::runtime::SimulationClass::Gpu)
}

/// Whether `camera` is the only view the game drew last frame, so a capture
/// through it is the game's own frame, HUD included.
fn shown_alone(world: &World, camera: Entity) -> bool {
    world
        .get_resource::<crate::runtime::RenderWorld>()
        .is_some_and(|render| {
            render.views.is_empty()
                && render
                    .active_camera
                    .is_some_and(|shown| shown.entity == camera)
        })
}

pub fn run_scenario(
    app: &mut App,
    scenario: &Scenario,
    base: &Path,
) -> ScenarioReport {
    let mut report = ScenarioReport {
        name: scenario.name.clone(),
        seed: scenario.seed,
        passed: false,
        ticks_run: 0,
        first_failure: None,
        steps: Vec::new(),
        captures: Vec::new(),
        trace: Vec::new(),
        perf: PerfReport::default(),
        gpu_state_hashes: Vec::new(),
        state_hashes: Vec::new(),
        fuzz_steps: Vec::new(),
        coverage: Value::Null,
        explore: Value::Null,
    };
    if let Err(message) = scenario.validate() {
        report.first_failure = Some(StepResult {
            tick: 0,
            step: 0,
            ok: false,
            message,
            actual: Value::Null,
        });
        return report;
    }
    let mut scenario = scenario.with_counters_resolved();
    if let Some(fuzz) = &scenario.fuzz {
        let mut actions = fuzz.actions.clone();
        if actions.is_empty() {
            crate::runtime::bind_input_actions(app.world_mut());
            let map = app.world().resource::<ActionMap>();
            actions = map.actions().into_iter().map(str::to_owned).collect();
        }
        let steps = fuzz.steps(&actions, scenario.ticks);
        report.fuzz_steps = steps
            .iter()
            .map(|step| serde_json::to_value(step).unwrap_or_default())
            .collect();
        scenario.steps.extend(steps);
    }
    let scenario = &scenario;
    app.world_mut().insert_resource(RandomSeed(scenario.seed));
    let mut explorer = match &scenario.explore {
        Some(explore) => match Explorer::new(app.world_mut(), explore) {
            Ok(explorer) => Some(explorer),
            Err(message) => {
                report.first_failure = Some(StepResult {
                    tick: 0,
                    step: 0,
                    ok: false,
                    message,
                    actual: Value::Null,
                });
                return report;
            }
        },
        None => None,
    };
    // A headless view is the capture size from tick 0, for layout, the UI
    // pass and pointer steps alike.
    app.world_mut()
        .resource_mut::<RuntimeInput>()
        .record_viewport_size(scenario.capture_size.map(|side| side as f32));
    if let Err(message) = seed_files(scenario, base) {
        report.first_failure = Some(StepResult {
            tick: 0,
            step: 0,
            ok: false,
            message,
            actual: Value::Null,
        });
        return report;
    }
    let render_budget = scenario.budgets.as_ref().is_some_and(|budgets| {
        budgets.max_draws.is_some()
            || budgets.max_triangles.is_some()
            || budgets.max_lights.is_some()
    });
    let wants_capture = scenario.gpu
        || render_budget
        || scenario.steps.iter().any(|step| {
            matches!(
                step.action,
                StepAction::Capture(_) | StepAction::ExpectPixels(_)
            )
        });
    let mut capture =
        wants_capture.then(|| HeadlessCapture::new(scenario.capture_size));
    // Ticks that must render: the image steps and a few ticks before each,
    // for occlusion history and camera screens. A run with GPU physics or
    // render budgets renders every tick instead (see `render_tick`).
    let image_ticks: Vec<(u32, u32)> = scenario
        .steps
        .iter()
        .filter(|step| {
            matches!(
                step.action,
                StepAction::Capture(_) | StepAction::ExpectPixels(_)
            )
        })
        .map(|step| {
            let last = step.until.or(step.within).unwrap_or(step.tick);
            (step.tick.saturating_sub(RENDER_WARMUP_TICKS), last)
        })
        .collect();
    let mut mixer = crate::audio_output::OfflineMixer::offline();
    // The whole mix is kept only for the steps that read it; a long soak
    // would otherwise grow by 384 KB per second of game time.
    let keep_mix =
        scenario.audio_out.is_some() || scenario.audio_reference.is_some();
    let mut mixed = Vec::new();
    if let (true, Some(Err(error))) = (scenario.gpu, capture.as_ref()) {
        report.steps.push(StepResult {
            tick: 0,
            step: 0,
            ok: false,
            message: format!("gpu: no Vulkan device: {error}"),
            actual: Value::Null,
        });
        report.first_failure = report.steps.first().cloned();
        return report;
    }
    let mut events = Vec::new();
    let mut frames: Vec<(u32, Vec<u8>)> = Vec::new();
    let mut tick_ms: Vec<f64> = Vec::new();
    let mut live = app.world_mut().query_filtered::<(), bevy_ecs::query::Without<bevy_ecs::resource::IsResource>>();
    let mut entities_max = 0;
    let run_started = std::time::Instant::now();
    let cpu_started = process_cpu_ms();
    let mut stages = [0.0_f64; 4];
    let scene_at_start =
        crate::runtime::scene_document_lenient(app.world_mut(), "").ok();
    // Repeated checks report once; this holds their last passing result.
    let mut pending: Vec<Option<StepResult>> = vec![None; scenario.steps.len()];
    // `within` checks that already passed.
    let mut passed = vec![false; scenario.steps.len()];
    // Invariants that already failed.
    let mut broken = vec![false; scenario.invariants.len()];

    let fixed_delta = app.world().resource::<FrameTime>().fixed_delta;
    'ticks: for tick in 0..=scenario.ticks {
        if scenario.realtime {
            let due = run_started + fixed_delta * tick;
            std::thread::sleep(
                due.saturating_duration_since(std::time::Instant::now()),
            );
            // A capture can make this tick late; simulated net delays still
            // count from the tick's planned time, so removing a capture does
            // not move when messages arrive.
            crate::net::pin_clock(Some(due));
        }
        for (index, step) in scenario.steps.iter().enumerate() {
            if step.tick + 1 == tick {
                // The press already reported any unbound action.
                if let StepAction::Tap(action) = &step.action {
                    let _ = press(app.world_mut(), action, false, None);
                }
                if let StepAction::Click(_) = &step.action {
                    app.world_mut()
                        .resource_mut::<RuntimeInput>()
                        .record_mouse_button(MouseButton::Left, false);
                }
            }
            if step.tick != tick {
                continue;
            }
            let result = match &step.action {
                StepAction::Press(action) | StepAction::Tap(action) => {
                    match step.at {
                        Some(at) if !(0.0..1.0).contains(&at) => Err(format!(
                            "at is {at}; it must be from 0 up to 1"
                        )),
                        at => press(
                            app.world_mut(),
                            action,
                            true,
                            at.map(|at| f64::from(tick) + at),
                        ),
                    }
                }
                StepAction::Release(action) => {
                    press(app.world_mut(), action, false, None)
                }
                StepAction::Set(assignment) => {
                    assign(app.world_mut(), assignment)
                }
                StepAction::Pointer(point) => point_at(app.world_mut(), *point),
                StepAction::LeftStick(tilt) => {
                    tilt_stick(app.world_mut(), Stick::Left, *tilt)
                }
                StepAction::RightStick(tilt) => {
                    tilt_stick(app.world_mut(), Stick::Right, *tilt)
                }
                StepAction::Focus(focused) => {
                    app.world_mut()
                        .resource_mut::<RuntimeInput>()
                        .record_focus(*focused);
                    Ok(String::new())
                }
                StepAction::Click(label) => click(app.world_mut(), label),
                StepAction::Restart(true) => restart(app.world_mut()),
                _ => continue,
            };
            let failed = result.is_err();
            if !failed && report.trace.len() < TRACE_LIMIT {
                let (kind, detail) = match &step.action {
                    StepAction::Press(action) => {
                        ("input", serde_json::json!({"press": action}))
                    }
                    StepAction::Release(action) => {
                        ("input", serde_json::json!({"release": action}))
                    }
                    StepAction::Tap(action) => {
                        ("input", serde_json::json!({"tap": action}))
                    }
                    StepAction::Set(assignment) => {
                        ("set", serde_json::json!(assignment))
                    }
                    StepAction::Click(label) => {
                        ("input", serde_json::json!({"click": label}))
                    }
                    StepAction::Restart(_) => {
                        ("restart", serde_json::json!({"restart": true}))
                    }
                    _ => ("input", Value::Null),
                };
                report.trace.push(TraceEvent {
                    tick,
                    kind: kind.into(),
                    detail,
                });
            }
            report.steps.push(StepResult {
                tick,
                step: index,
                ok: !failed,
                message: result.unwrap_or_else(|error| error),
                actual: Value::Null,
            });
            if failed && !scenario.keep_going {
                break 'ticks;
            }
        }

        if let Some(explorer) = &mut explorer {
            if let Err(message) = explorer.steer(app.world_mut(), tick) {
                report.steps.push(StepResult {
                    tick,
                    step: 0,
                    ok: false,
                    message,
                    actual: Value::Null,
                });
                break 'ticks;
            }
        }
        let delta = tick_delta(app, tick);
        let started = std::time::Instant::now();
        let render = scenario.gpu
            || render_budget
            || image_ticks
                .iter()
                .any(|(first, last)| (*first..=*last).contains(&tick));
        let updated = match capture.as_mut() {
            Some(Ok(capture)) if render => capture.frame(app, delta),
            // GPU bodies advance on every tick, drawn or not.
            Some(Ok(capture)) if has_gpu_bodies(app.world_mut()) => {
                capture.step_physics(app, delta)
            }
            Some(Ok(capture)) => capture.update(app, delta),
            _ => app
                .update(delta)
                .map(drop)
                .map_err(|error| error.to_string()),
        };
        // Sound cues become requests here as in the windowed runner, so
        // `audio:` sees them.
        crate::runtime::route_sound_events(app.world_mut());
        if let Some(timings) = app
            .world()
            .get_resource::<crate::runtime::CpuFrameTimings>()
        {
            for (sum, time) in stages.iter_mut().zip([
                timings.physics,
                timings.update,
                timings.post_update,
                timings.extraction,
            ]) {
                *sum += time.as_secs_f64() * 1000.0;
            }
        }
        if let Some(mixer) = &mut mixer {
            mix_tick(app, mixer, keep_mix.then_some(&mut mixed));
        }
        collect_hashes(app.world(), &mut report);
        if let Err(error) = updated {
            report.steps.push(StepResult {
                tick,
                step: 0,
                ok: false,
                message: format!("game update failed: {error}"),
                actual: Value::Null,
            });
            break;
        }
        tick_ms.push(started.elapsed().as_secs_f64() * 1000.0);
        entities_max = entities_max.max(live.iter(app.world()).count() as u64);
        report.ticks_run = app.world().resource::<FrameTime>().fixed_tick;
        TICKS_FINISHED
            .store(u64::from(tick) + 1, std::sync::atomic::Ordering::Relaxed);
        let world = app.world_mut();
        let collisions: Vec<_> = world
            .resource::<EventQueue<CollisionEvent>>()
            .iter()
            .map(|event| (event.a, event.b))
            .collect();
        let seen: Vec<_> = collisions
            .into_iter()
            .map(|(a, b)| SeenEvent {
                a: world.get::<SceneId>(a).map(|id| id.0),
                b: world.get::<SceneId>(b).map(|id| id.0),
            })
            .collect();
        for event in &seen {
            if report.trace.len() < TRACE_LIMIT {
                report.trace.push(TraceEvent {
                    tick,
                    kind: "collision".into(),
                    detail: serde_json::json!({"a": event.a, "b": event.b}),
                });
            }
        }
        events.extend(seen);

        for (index, invariant) in scenario.invariants.iter().enumerate() {
            if broken[index] {
                continue;
            }
            // ponytail: rebuilds the scene document per invariant per tick;
            // cache one snapshot per tick if a run with many is too slow.
            let missing = if invariant.entity != AUDIO_ENTITY
                && find_entity(world, &invariant.entity, |_, _| true).is_none()
            {
                true
            } else {
                // An entity that exists but cannot be read is a failure,
                // which `check_value` reports.
                reflected(world, &invariant.entity)
                    .is_ok_and(|state| state.pointer(&invariant.path).is_none())
            };
            if missing && invariant.exists.is_none() {
                continue;
            }
            if let Err((message, actual)) = check_value(world, invariant) {
                broken[index] = true;
                report.steps.push(StepResult {
                    tick,
                    step: 0,
                    ok: false,
                    message: format!("invariant {index}: {message}"),
                    actual,
                });
                if !scenario.keep_going {
                    break 'ticks;
                }
            }
        }
        for (index, step) in scenario.steps.iter().enumerate() {
            let last = step.until.or(step.within).unwrap_or(step.tick);
            if !(step.tick..=last).contains(&tick) || passed[index] {
                continue;
            }
            let outcome = match &step.action {
                StepAction::Expect(expect) => check_value(world, expect),
                StepAction::ExpectEvents(expect) => {
                    check_events(world, expect, &events)
                }
                StepAction::ExpectScreen(expect) => {
                    check_screen(world, expect, scenario.capture_size)
                }
                StepAction::ExpectFile(expect) => check_file(expect),
                StepAction::ExpectQuit(wanted) => {
                    let quit =
                        world.resource::<crate::runtime::ExitState>().requested;
                    if quit == *wanted {
                        Ok(format!("quit is {quit}"))
                    } else {
                        Err((
                            format!("quit is {quit}, expected {wanted}"),
                            Value::Bool(quit),
                        ))
                    }
                }
                StepAction::Capture(step) => {
                    let relative = &step.path;
                    let path = base.join(relative);
                    // `tests/x.png` in a scenario under `tests/` is the
                    // common slip; paths start at the scenario's folder.
                    let doubled = base.file_name() == relative.iter().next();
                    let warning = if doubled {
                        format!(
                            "; warning: capture paths are relative to {}, \
                             so this wrote {}",
                            base.display(),
                            path.display()
                        )
                    } else {
                        String::new()
                    };
                    match capture.as_mut() {
                        Some(Ok(capture)) => (|| {
                            let camera = match &step.camera {
                                Some(name) => Some(
                                    find_camera(world, name).ok_or_else(|| {
                                        (
                                            format!("no camera has the ID or name `{name}`"),
                                            Value::Null,
                                        )
                                    })?,
                                ),
                                None => None,
                            };
                            let camera =
                                camera.filter(|&c| !shown_alone(world, c));
                            let pixels = if camera.is_some() || !step.hud {
                                capture
                                    .view_rgba(world, camera)
                                    .map_err(|error| (error, Value::Null))?
                            } else {
                                capture.rgba()
                            };
                            if scenario.contact_sheet.is_some() {
                                frames.push((tick, pixels.clone()));
                            }
                            let mut message = save_capture(
                                &pixels,
                                world,
                                scenario,
                                &path,
                                &mut report.captures,
                            )?;
                            if let Some(golden) = &step.golden {
                                message += &compare_golden(
                                    &pixels,
                                    scenario.capture_size,
                                    &base.join(golden),
                                    step.tolerance,
                                    scenario.update_golden,
                                )?;
                            }
                            Ok(message + &warning)
                        })(),
                        Some(Err(error)) => {
                            Ok(format!("capture skipped: {error}"))
                        }
                        None => unreachable!("captures open a renderer"),
                    }
                }
                StepAction::ExpectPixels(expect) => match capture.as_ref() {
                    Some(Ok(capture)) => check_pixels(
                        world,
                        expect,
                        &capture.rgba(),
                        scenario.capture_size,
                        base,
                    ),
                    Some(Err(error)) => Err((
                        format!("expect_pixels needs a renderer: {error}"),
                        Value::Null,
                    )),
                    None => unreachable!("pixel checks open a renderer"),
                },
                StepAction::Log(logged) => {
                    let subject =
                        format!("`{}` {}", logged.entity, logged.path);
                    let (actual, message) =
                        match reflected(world, &logged.entity) {
                            Err(error) => (
                                Value::Null,
                                format!("log: {subject}: {error}"),
                            ),
                            Ok(state) => match state.pointer(&logged.path) {
                                Some(value) => (
                                    value.clone(),
                                    format!("log: {subject} is {value}"),
                                ),
                                None => (
                                    Value::Null,
                                    format!(
                                        "log: {subject} does not exist{}",
                                        near_paths(&state, &logged.path)
                                    ),
                                ),
                            },
                        };
                    report.steps.push(StepResult {
                        tick,
                        step: index,
                        ok: true,
                        message,
                        actual,
                    });
                    continue;
                }
                StepAction::Press(_)
                | StepAction::Release(_)
                | StepAction::Tap(_)
                | StepAction::Set(_)
                | StepAction::Click(_)
                | StepAction::Restart(_)
                | StepAction::Pointer(_)
                | StepAction::LeftStick(_)
                | StepAction::RightStick(_)
                | StepAction::Focus(_) => continue,
            };
            let (ok, message, actual) = match outcome {
                Ok(message) => (true, message, Value::Null),
                Err((message, actual)) => {
                    (false, message + &inputs_at(scenario, tick), actual)
                }
            };
            let result = StepResult {
                tick,
                step: index,
                ok,
                message,
                actual,
            };
            if step.within.is_some() && !ok && tick < last {
                continue;
            }
            passed[index] = step.within.is_some() && ok;
            if !ok || tick == last || passed[index] {
                pending[index] = None;
                report.steps.push(result);
                if !ok {
                    if !scenario.keep_going {
                        break 'ticks;
                    }
                    passed[index] = true;
                }
            } else {
                pending[index] = Some(result);
            }
        }
        app.world_mut()
            .resource_mut::<RuntimeInput>()
            .clear_frame_edges();
        if app.exit_requested() {
            for (index, step) in scenario.steps.iter().enumerate() {
                if step.tick <= tick {
                    continue;
                }
                if let StepAction::ExpectFile(expect) = &step.action {
                    let (ok, (message, actual)) = match check_file(expect) {
                        Ok(message) => (true, (message, Value::Null)),
                        Err(failed) => (false, failed),
                    };
                    report.steps.push(StepResult {
                        tick: step.tick,
                        step: index,
                        ok,
                        message,
                        actual,
                    });
                } else if is_check(&step.action) {
                    report.steps.push(StepResult {
                        tick: step.tick,
                        step: index,
                        ok: false,
                        message: format!(
                            "the game quit at tick {tick}, before this step"
                        ),
                        actual: Value::Null,
                    });
                }
            }
            break;
        }
    }
    crate::net::pin_clock(None);
    if let (Some(sheet), false) = (&scenario.contact_sheet, frames.is_empty()) {
        let extent = scenario.capture_size;
        let (pixels, size) =
            crate::annotate::contact_sheet(&frames, extent, 320);
        let path = base.join(sheet);
        let written = path
            .parent()
            .map_or(Ok(()), std::fs::create_dir_all)
            .map_err(|error| error.to_string())
            .and_then(|()| {
                image::save_buffer(
                    &path,
                    &pixels,
                    size[0],
                    size[1],
                    image::ExtendedColorType::Rgba8,
                )
                .map_err(|error| error.to_string())
            });
        match written {
            Ok(()) => report.captures.push(path),
            Err(error) => report.steps.push(StepResult {
                tick: report.ticks_run as u32,
                step: 0,
                ok: false,
                message: format!("contact sheet: {error}"),
                actual: Value::Null,
            }),
        }
    }
    if let Some(out) = &scenario.audio_out {
        let path = base.join(out);
        match crate::audio_output::write_wav(&path, &mixed) {
            Ok(()) => report.captures.push(path),
            Err(error) => report.steps.push(StepResult {
                tick: report.ticks_run as u32,
                step: 0,
                ok: false,
                message: format!("audio_out: {error}"),
                actual: Value::Null,
            }),
        }
    }
    if let Some(reference) = &scenario.audio_reference {
        let path = base.join(&reference.path);
        let (ok, message, actual) = match compare_mix(
            &mixed,
            &path,
            reference.tolerance,
            scenario.update_golden,
        ) {
            Ok(message) => (true, message, Value::Null),
            Err((message, actual)) => (false, message, actual),
        };
        report.steps.push(StepResult {
            tick: report.ticks_run as u32,
            step: 0,
            ok,
            message,
            actual,
        });
    }
    let render = match capture.as_mut() {
        Some(Ok(capture)) => Some(capture.metadata(app)),
        _ => None,
    };
    report.perf = perf_report(&tick_ms, render);
    report.perf.wall_ms_mean = run_started.elapsed().as_secs_f64() * 1000.0
        / tick_ms.len().max(1) as f64;
    report.perf.cpu_ms_mean = cpu_started
        .zip(process_cpu_ms())
        .map(|(start, end)| (end - start) / tick_ms.len().max(1) as f64);
    report.perf.entities_max = entities_max;
    report.perf.rss_mb = process_memory_mb("VmRSS:");
    report.perf.rss_mb_peak = process_memory_mb("VmHWM:");
    let mean = stages.map(|sum| sum / tick_ms.len().max(1) as f64);
    report.perf.stages_ms_mean = json!({"fixed": mean[0], "update": mean[1],
        "post_update": mean[2], "extract": mean[3]});
    collect_hashes(app.world(), &mut report);
    if let (Some(start), Ok(end)) = (
        &scene_at_start,
        crate::runtime::scene_document_lenient(app.world_mut(), ""),
    ) {
        report.coverage = coverage(start, &end, &report.trace);
    }
    if let Some(explorer) = explorer {
        let (explored, missed) = explorer.finish();
        if !missed.is_empty() {
            report.steps.push(StepResult {
                tick: report.ticks_run as u32,
                step: 0,
                ok: false,
                message: format!(
                    "explore: could not reach {}",
                    missed
                        .iter()
                        .map(|name| format!("`{name}`"))
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
                actual: explored.clone(),
            });
        }
        report.explore = explored;
    }
    if let Some(budgets) = &scenario.budgets {
        for message in over_budget(budgets, &report.perf) {
            report.steps.push(StepResult {
                tick: report.ticks_run as u32,
                step: 0,
                ok: false,
                message: format!("budget: {message}"),
                actual: serde_json::to_value(&report.perf).unwrap_or_default(),
            });
        }
    }
    report.first_failure = report.steps.iter().find(|step| !step.ok).cloned();
    report.passed = report.first_failure.is_none();
    report
}

/// [`ScenarioReport::coverage`] from the scene before and after a run.
fn coverage(
    start: &crate::runtime::SceneDocument,
    end: &crate::runtime::SceneDocument,
    trace: &[TraceEvent],
) -> Value {
    use std::collections::BTreeSet;
    // Each entity's sections as "transform", "components/rusting.counter".
    let sections = |entity: &crate::runtime::SceneEntity| {
        let value = serde_json::to_value(entity).unwrap_or_default();
        let mut out = BTreeMap::new();
        for (key, part) in value.as_object().into_iter().flatten() {
            match (key.as_str(), part) {
                ("id" | "name" | "parent", _) => {}
                ("components", Value::Object(components)) => {
                    for (name, component) in components {
                        out.insert(
                            format!("components/{name}"),
                            component.clone(),
                        );
                    }
                }
                _ => {
                    out.insert(key.clone(), part.clone());
                }
            }
        }
        out
    };
    let label = |entity: &crate::runtime::SceneEntity| json!({"name": entity.name, "id": entity.id});
    let before: BTreeMap<_, _> = start
        .entities
        .iter()
        .map(|entity| (entity.id, entity))
        .collect();
    let after: BTreeMap<_, _> = end
        .entities
        .iter()
        .map(|entity| (entity.id, entity))
        .collect();
    let (mut changed, mut unchanged) = (Vec::new(), 0);
    let (mut touched, mut present) = (BTreeSet::new(), BTreeSet::new());
    for (id, old) in &before {
        let old_sections = sections(old);
        present.extend(old_sections.keys().cloned());
        let Some(new) = after.get(id) else { continue };
        let new_sections = sections(new);
        let keys: BTreeSet<_> = old_sections
            .keys()
            .chain(new_sections.keys())
            .cloned()
            .collect();
        let differ: Vec<_> = keys
            .into_iter()
            .filter(|key| old_sections.get(key) != new_sections.get(key))
            .collect();
        if differ.is_empty() {
            unchanged += 1;
        } else {
            touched.extend(differ.iter().cloned());
            let mut entry = label(new);
            entry["sections"] = json!(differ);
            changed.push(entry);
        }
    }
    let added: Vec<_> = after
        .iter()
        .filter(|(id, _)| !before.contains_key(id))
        .map(|(_, entity)| label(entity))
        .collect();
    let removed: Vec<_> = before
        .iter()
        .filter(|(id, _)| !after.contains_key(id))
        .map(|(_, entity)| label(entity))
        .collect();
    let mut events = BTreeMap::<&str, u64>::new();
    let mut inputs = BTreeSet::new();
    for event in trace {
        *events.entry(event.kind.as_str()).or_default() += 1;
        if let Some(action) = event.detail["press"].as_str() {
            inputs.insert(action);
        }
    }
    json!({
        "entities_changed": changed,
        "entities_added": added,
        "entities_removed": removed,
        "entities_unchanged": unchanged,
        "sections_changed": touched,
        "sections_untouched": present.difference(&touched).collect::<Vec<_>>(),
        "inputs": inputs,
        "events": events,
    })
}

fn perf_report(tick_ms: &[f64], render: Option<Value>) -> PerfReport {
    let mut sorted = tick_ms.to_vec();
    sorted.sort_by(f64::total_cmp);
    let at = |fraction: f64| {
        sorted
            .get(
                ((sorted.len() as f64 * fraction).ceil() as usize)
                    .saturating_sub(1),
            )
            .copied()
            .unwrap_or(0.0)
    };
    let mut environment = json!({
        "engine_version": env!("CARGO_PKG_VERSION"),
        "os": std::env::consts::OS,
        "arch": std::env::consts::ARCH,
        "profile": if cfg!(debug_assertions) { "debug" } else { "release" },
        "cpus": std::thread::available_parallelism().map_or(1, usize::from),
        "load_average": load_average(),
    });
    let mut counters = Value::Null;
    if let Some(meta) = render {
        environment["device"] = meta["device"].clone();
        environment["driver"] = meta["driver"].clone();
        counters = json!({
            "draws": meta["draws"],
            "triangles": meta["triangles"],
            "visible_instances": meta["visible_instances"],
            "gpu_ms": meta["gpu_ms"],
            "gpu_frames": meta["gpu_frames"],
            "gpu_ms_p50": meta["gpu_ms_p50"],
            "gpu_ms_p95": meta["gpu_ms_p95"],
            "gpu_ms_max": meta["gpu_ms_max"],
            "passes_ms": meta["passes_ms"],
            "cameras": meta["cameras"],
            "dropped_lights": meta["dropped_lights"],
            "lights": meta["lights"],
            "physics_grid_overflow": meta["physics_grid_overflow"],
            "physics_oversized_bodies": meta["physics_oversized_bodies"],
            "physics_fallback_tests": meta["physics_fallback_tests"],
        });
    }
    PerfReport {
        environment,
        tick_ms_mean: sorted.iter().sum::<f64>() / sorted.len().max(1) as f64,
        tick_ms_p95: at(0.95),
        tick_ms_max: at(1.0),
        wall_ms_mean: 0.0,
        cpu_ms_mean: None,
        stages_ms_mean: Value::Null,
        entities_max: 0,
        rss_mb: None,
        rss_mb_peak: None,
        render: counters,
    }
}

/// CPU time this process has used, all threads, on Linux.
fn process_cpu_ms() -> Option<f64> {
    let stat = std::fs::read_to_string("/proc/self/stat").ok()?;
    // Fields after the parenthesised name; utime and stime are the 12th
    // and 13th, in USER_HZ (always 100 in /proc).
    let mut fields = stat.rsplit_once(')')?.1.split_whitespace().skip(11);
    let user: u64 = fields.next()?.parse().ok()?;
    let system: u64 = fields.next()?.parse().ok()?;
    Some((user + system) as f64 * 10.0)
}

/// A `/proc/self/status` memory line such as `VmRSS:` in MiB, on Linux.
fn process_memory_mb(field: &str) -> Option<f64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let kib: f64 = status
        .lines()
        .find_map(|line| line.strip_prefix(field))?
        .split_whitespace()
        .next()?
        .parse()
        .ok()?;
    Some(kib / 1024.0)
}

/// The one-minute load average on Linux, `None` elsewhere.
fn load_average() -> Option<f64> {
    std::fs::read_to_string("/proc/loadavg")
        .ok()?
        .split_whitespace()
        .next()?
        .parse()
        .ok()
}

fn over_budget(budgets: &Budgets, perf: &PerfReport) -> Vec<String> {
    let mut over = Vec::new();
    // Wall-clock budgets fail on a busy machine with nothing wrong in the
    // game, so say how busy it was.
    let load = match (
        perf.environment["load_average"].as_f64(),
        perf.environment["cpus"].as_u64(),
    ) {
        (Some(load), Some(cpus)) if load > cpus as f64 / 2.0 => format!(
            " (load average {load:.1} on {cpus} CPUs: rerun it alone \
             before treating it as real)"
        ),
        _ => String::new(),
    };
    let mut ms = |name: &str, limit: Option<f64>, value: f64| {
        if let Some(limit) = limit.filter(|limit| value > *limit) {
            over.push(format!(
                "{name} is {value:.3} ms, over the {limit} ms limit{load}"
            ));
        }
    };
    ms("max_tick_ms", budgets.max_tick_ms, perf.tick_ms_max);
    ms("mean_tick_ms", budgets.mean_tick_ms, perf.tick_ms_mean);
    ms("p95_tick_ms", budgets.p95_tick_ms, perf.tick_ms_p95);
    if let Some(cpu) = perf.cpu_ms_mean {
        ms("mean_cpu_ms", budgets.mean_cpu_ms, cpu);
    }
    if let (Some(limit), Some(draws)) =
        (budgets.max_draws, perf.render["draws"].as_u64())
    {
        if draws > u64::from(limit) {
            over.push(format!(
                "max_draws: {draws} draws, over the limit of {limit}"
            ));
        }
    }
    if let (Some(limit), Some(triangles)) =
        (budgets.max_triangles, perf.render["triangles"].as_u64())
    {
        if triangles > limit {
            over.push(format!("max_triangles: {triangles} triangles, over the limit of {limit}"));
        }
    }
    if let (Some(limit), Some(lights)) =
        (budgets.max_lights, perf.render["lights"].as_u64())
    {
        if lights > u64::from(limit) {
            over.push(format!(
                "max_lights: {lights} lights, over the limit of {limit}"
            ));
        }
    }
    if let Some(limit) = budgets
        .max_entities
        .filter(|limit| perf.entities_max > *limit)
    {
        over.push(format!(
            "max_entities: {} entities, over the limit of {limit}",
            perf.entities_max
        ));
    }
    over
}

pub(crate) fn press(
    world: &mut World,
    action: &str,
    pressed: bool,
    press_tick: Option<f64>,
) -> Result<String, String> {
    // Scene bindings are added by the first update; tick 0 comes before it.
    crate::runtime::bind_input_actions(world);
    let bindings = world.resource::<ActionMap>().bindings(action).to_vec();
    if bindings.is_empty() {
        return Err(format!("action `{action}` has no bindings"));
    }
    let mut input = world.resource_mut::<RuntimeInput>();
    for binding in bindings {
        if let Some(tick) = press_tick {
            input.record_press_tick(binding, tick);
        }
        match binding {
            InputBinding::Key(key) => input.record_key(key, pressed),
            InputBinding::Pad(button) => {
                input.record_pad_button(button, pressed);
            }
            InputBinding::Mouse(button) => {
                input.record_mouse_button(button, pressed);
            }
        }
    }
    let verb = if pressed { "pressed" } else { "released" };
    Ok(format!("{verb} `{action}`"))
}

fn tilt_stick(
    world: &mut World,
    stick: Stick,
    tilt: [f32; 2],
) -> Result<String, String> {
    world
        .resource_mut::<RuntimeInput>()
        .record_stick(stick, tilt);
    Ok(format!("{stick:?} stick at {tilt:?}"))
}

/// Places the cursor at `point`, a fraction of the view.
fn point_at(world: &mut World, point: [f32; 2]) -> Result<String, String> {
    let mut input = world.resource_mut::<RuntimeInput>();
    let size = input.viewport_size();
    input.record_cursor_position([point[0] * size[0], point[1] * size[1]]);
    Ok(format!("pointer at {point:?}"))
}

/// Points at the UI text `label` drawn last frame and presses the left
/// mouse button; the run loop releases it next tick.
fn click(world: &mut World, target: &ClickTarget) -> Result<String, String> {
    #[cfg(feature = "ui")]
    {
        let (label, prefix, index) = match target {
            ClickTarget::Label(label) => (label.as_str(), false, None),
            ClickTarget::Find {
                text: Some(text),
                starts_with: None,
                index,
            } => (text.as_str(), false, *index),
            ClickTarget::Find {
                text: None,
                starts_with: Some(prefix),
                index,
            } => (prefix.as_str(), true, *index),
            ClickTarget::Find { .. } => {
                return Err(
                    "click needs exactly one of `text` and `starts_with`"
                        .into(),
                )
            }
        };
        let at = world
            .get_resource::<crate::runtime::RuntimeUi>()
            .ok_or("this build has no runtime UI")?
            .find_text_where(label, prefix, index)?;
        let mut input = world.resource_mut::<RuntimeInput>();
        input.record_cursor_position(at);
        input.record_mouse_button(MouseButton::Left, true);
        Ok(format!("clicked `{label}` at {at:?}"))
    }
    #[cfg(not(feature = "ui"))]
    {
        let _ = (world, target);
        Err("click needs the engine's `ui` feature".into())
    }
}

fn restart(world: &mut World) -> Result<String, String> {
    #[cfg(feature = "window")]
    if crate::project_runner::restart_scene(world) {
        return Ok("restarted the scene".into());
    }
    let _ = world;
    Err("restart needs a game started from a scene file".into())
}

/// Copies the scenario's `files` into the user data folder.
fn seed_files(scenario: &Scenario, base: &Path) -> Result<(), String> {
    let folder = crate::project::user_data_folder();
    for (key, source) in &scenario.files {
        let relative = Path::new(key);
        if !relative
            .components()
            .all(|part| matches!(part, std::path::Component::Normal(_)))
        {
            return Err(format!("files: `{key}` must be a relative path"));
        }
        let target = folder.join(relative);
        let source = base.join(source);
        target
            .parent()
            .map_or(Ok(()), std::fs::create_dir_all)
            .and_then(|()| std::fs::copy(&source, &target))
            .map_err(|error| {
                format!(
                    "files: copying {} to {}: {error}",
                    source.display(),
                    target.display()
                )
            })?;
    }
    Ok(())
}

fn is_check(action: &StepAction) -> bool {
    matches!(
        action,
        StepAction::Expect(_)
            | StepAction::ExpectEvents(_)
            | StepAction::ExpectScreen(_)
            | StepAction::ExpectQuit(_)
            | StepAction::ExpectFile(_)
            | StepAction::ExpectPixels(_)
            | StepAction::Capture(_)
    )
}

/// Names the inputs that ran before the checks of `tick`, because a check
/// on the same tick as a click sees the click's effect.
fn inputs_at(scenario: &Scenario, tick: u32) -> String {
    let inputs: Vec<_> = scenario
        .steps
        .iter()
        .filter(|step| step.tick == tick && !is_check(&step.action))
        .filter(|step| !matches!(step.action, StepAction::Log(_)))
        .filter_map(|step| serde_json::to_value(&step.action).ok())
        .map(|action| action.to_string())
        .collect();
    if inputs.is_empty() {
        String::new()
    } else {
        format!(
            " (inputs at this tick ran before the check: {})",
            inputs.join(", ")
        )
    }
}

type Check = Result<String, (String, Value)>;

/// What `class:<name>[ in min max]` reads; see [`CLASS_PREFIX`].
fn class_state(world: &mut World, wanted: &str) -> Result<Value, String> {
    let (class, bounds) = match wanted.split_once(" in ") {
        None => (wanted, None),
        Some((class, bounds)) => {
            let corner = |text: &str| -> Option<[f32; 3]> {
                let values = text
                    .split(',')
                    .map(|part| part.trim().parse().ok())
                    .collect::<Option<Vec<f32>>>()?;
                values.try_into().ok()
            };
            let corners = bounds
                .split_whitespace()
                .map(corner)
                .collect::<Option<Vec<_>>>();
            match corners.as_deref() {
                Some(&[min, max]) => (class, Some((min, max))),
                _ => {
                    return Err(format!(
                        "`{CLASS_PREFIX}{wanted}`: write the box as \
                         `in minX,minY,minZ maxX,maxY,maxZ`"
                    ))
                }
            }
        }
    };
    // ponytail: a check every tick reads the whole class back every tick;
    // fine for tests, add a snapshot interval if big classes get slow.
    if world.contains_resource::<crate::runtime::GpuPhysicsCommands>() {
        crate::runtime::request_gpu_class_snapshot(world, class);
    }
    let mut count = 0;
    let mut positions = Vec::new();
    for (classes, mirror) in world
        .query::<(
            &crate::runtime::ObjectClasses,
            Option<&crate::runtime::GpuStateMirror>,
        )>()
        .iter(world)
    {
        if classes.contains(class) {
            count += 1;
            positions.extend(mirror.map(|mirror| mirror.transform.position));
        }
    }
    let fold = |pick: fn(f32, f32) -> f32| {
        positions.iter().copied().reduce(|a, b| {
            [pick(a[0], b[0]), pick(a[1], b[1]), pick(a[2], b[2])]
        })
    };
    let mut gpu = json!({
        "count": positions.len(),
        "min": fold(f32::min),
        "max": fold(f32::max),
    });
    if let Some((min, max)) = bounds {
        gpu["inside"] = json!(positions
            .iter()
            .filter(|at| (0..3)
                .all(|axis| (min[axis]..=max[axis]).contains(&at[axis])))
            .count());
    }
    Ok(json!({"count": count, "gpu": gpu}))
}

/// The entity's scene form, with registered components parsed.
pub(crate) fn reflected(
    world: &mut World,
    wanted: &str,
) -> Result<Value, String> {
    if wanted == AUDIO_ENTITY {
        let queue = world
            .get_resource::<crate::runtime::AudioQueue>()
            .cloned()
            .unwrap_or_default();
        let mix = world
            .get_resource::<AudioMix>()
            .cloned()
            .unwrap_or_default();
        return Ok(serde_json::json!({
            "requested": queue.requested(),
            "clips": queue.requested_clips(),
            "level": mix.level,
            "peak": mix.peak,
            "clipped": mix.clipped,
            "playing": mix.playing,
            "buses": mix.buses,
            "dropped": mix.buses.values()
                .map(|bus| bus.dropped + bus.stolen).sum::<u64>(),
            "captions": queue.captions(),
        }));
    }
    if let Some(class) = wanted.strip_prefix(CLASS_PREFIX) {
        return class_state(world, class);
    }
    let found = find_entity(world, wanted, |_, _| true);
    if let (None, Some(counter)) = (found, wanted.strip_prefix(COUNTER_PREFIX))
    {
        // Game code reads a counter nobody created yet as 0; so do checks.
        return Ok(serde_json::json!({
            "missing": true,
            "components": {"rusting.counter": {"name": counter, "value": 0}},
        }));
    }
    let entity = found
        .ok_or_else(|| format!("no entity has the ID or name `{wanted}`"))?;
    let gpu_state =
        world.get::<crate::runtime::GpuStateMirror>(entity).copied();
    // Only this entity: capturing the whole scene per check per tick
    // dominated long runs of big scenes.
    let entity = scene_entity_lenient(world, entity)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| format!("`{wanted}` has no persistent ID"))?;
    let mut value =
        serde_json::to_value(entity).map_err(|error| error.to_string())?;
    if let Some(state) = gpu_state {
        value["gpu_state"] = serde_json::json!({
            "tick": state.tick,
            "position": state.transform.position,
            "rotation": state.transform.rotation,
            "linear_velocity": state.linear_velocity,
            "angular_velocity": state.angular_velocity,
        });
    }
    if let Some(Value::Object(components)) = value.get_mut("components") {
        for component in components.values_mut() {
            if let Some(parsed) = component
                .as_str()
                .and_then(|text| serde_json::from_str(text).ok())
            {
                *component = parsed;
            }
        }
    }
    Ok(value)
}

pub(crate) fn assign(
    world: &mut World,
    set: &Assignment,
) -> Result<String, String> {
    let counter = set.entity.strip_prefix(COUNTER_PREFIX);
    if let (Some(name), COUNTER_VALUE) = (counter, set.path.as_str()) {
        // Same rule as game code: setting a missing counter creates it.
        let value = set.value.as_i64().ok_or_else(|| {
            format!("counter `{name}`: expected a whole number")
        })?;
        crate::project_runner::GameScene { world }
            .set_counter(name, value as i32);
        return Ok(format!("`{}` {} set to {value}", set.entity, set.path));
    }
    let entity =
        find_entity(world, &set.entity, |_, _| true).ok_or_else(|| {
            match counter {
                Some(name) => format!("no counter is named `{name}`"),
                None => {
                    format!("no entity has the ID or name `{}`", set.entity)
                }
            }
        })?;
    let subject = format!("`{}` {}", set.entity, set.path);
    if let Some(rest) = set.path.strip_prefix("/components/") {
        let (name, field) = rest.split_once('/').unwrap_or((rest, ""));
        if field.is_empty() {
            set_registered_component(
                world,
                entity,
                name,
                &set.value.to_string(),
            )
        } else {
            set_registered_component_field(
                world,
                entity,
                name,
                &format!("/{field}"),
                set.value.clone(),
            )
        }
        .map_err(|error| format!("{subject}: {error}"))?;
    } else if let Some(field) = set.path.strip_prefix("/transform") {
        let transform = world
            .get::<Transform>(entity)
            .ok_or_else(|| format!("`{}` has no transform", set.entity))?;
        let mut value = serde_json::to_value(SceneTransform::from(*transform))
            .expect("transforms serialize");
        let slot = value
            .pointer_mut(field)
            .ok_or_else(|| format!("{subject} does not exist"))?;
        *slot = set.value.clone();
        let transform: SceneTransform = serde_json::from_value(value)
            .map_err(|error| format!("{subject}: {error}"))?;
        let transform = Transform::from(transform);
        // The controller rewrites its body's rotation from yaw every tick,
        // so a rotation set turns the controller instead.
        if field.starts_with("/rotation") {
            if let Some(mut player) =
                world.get_mut::<crate::runtime::PlayerController>(entity)
            {
                player.pitch = transform.rotation[0];
                player.yaw = transform.rotation[1];
            }
        }
        world.entity_mut(entity).insert(transform);
    } else if let Some(field) = set.path.strip_prefix("/visible") {
        if !field.is_empty() {
            return Err(format!("{subject} does not exist"));
        }
        let visible = set
            .value
            .as_bool()
            .ok_or_else(|| format!("{subject}: expected true or false"))?;
        world
            .entity_mut(entity)
            .insert(crate::runtime::Visibility { visible });
    } else {
        use crate::runtime::{
            Collider, CollisionLayers, DirectionalLight, PhysicsBody,
            PointLight, RigidBody, SpotLight,
        };
        let (key, field) = set.path[1..]
            .split_once('/')
            .map_or((&set.path[1..], ""), |(key, field)| (key, field));
        let field = if field.is_empty() {
            String::new()
        } else {
            format!("/{field}")
        };
        let edit = |world: &mut World| -> Result<(), String> {
            match key {
                "rigid_body" => {
                    set_part::<RigidBody>(world, entity, &field, set)
                }
                "collider" => set_part::<Collider>(world, entity, &field, set),
                "physics_body" => {
                    set_part::<PhysicsBody>(world, entity, &field, set)
                }
                "collision_layers" => {
                    set_part::<CollisionLayers>(world, entity, &field, set)
                }
                "point_light" => {
                    set_part::<PointLight>(world, entity, &field, set)
                }
                "spot_light" => {
                    set_part::<SpotLight>(world, entity, &field, set)
                }
                "directional_light" => {
                    set_part::<DirectionalLight>(world, entity, &field, set)
                }
                _ => Err("set reaches /transform, /visible, /rigid_body, \
                     /collider, /physics_body, /collision_layers, the light \
                     fields and /components/... only"
                    .to_owned()),
            }
        };
        edit(world).map_err(|error| format!("{subject}: {error}"))?;
    }
    Ok(format!("{subject} set to {}", set.value))
}

/// Replaces the field at `pointer` (or all of it, when empty) of a built-in
/// component the entity already has.
fn set_part<T>(
    world: &mut World,
    entity: Entity,
    pointer: &str,
    set: &Assignment,
) -> Result<(), String>
where
    T: bevy_ecs::component::Component<
            Mutability = bevy_ecs::component::Mutable,
        > + Serialize
        + serde::de::DeserializeOwned,
{
    let mut part = world
        .get_mut::<T>(entity)
        .ok_or_else(|| "the entity does not have that component".to_owned())?;
    let mut value =
        serde_json::to_value(&*part).map_err(|error| error.to_string())?;
    let slot = value
        .pointer_mut(pointer)
        .ok_or_else(|| "that field does not exist".to_owned())?;
    *slot = set.value.clone();
    *part = serde_json::from_value(value).map_err(|error| error.to_string())?;
    Ok(())
}

fn check_screen(
    world: &mut World,
    expect: &ScreenExpectation,
    extent: [u32; 2],
) -> Check {
    let subject = format!("`{}`", expect.entity);
    let entity =
        find_entity(world, &expect.entity, |_, _| true).ok_or_else(|| {
            (
                format!("no entity has the ID or name `{}`", expect.entity),
                Value::Null,
            )
        })?;
    let id = world.get::<SceneId>(entity).map(|id| id.0);
    let view = match &expect.camera {
        Some(name) => find_camera(world, name)
            .and_then(|camera| CameraView::of(world, camera, extent))
            .ok_or_else(|| {
                (
                    format!("no camera has the ID or name `{name}`"),
                    Value::Null,
                )
            })?,
        None => CameraView::active(world, extent).ok_or_else(|| {
            ("the scene has no active camera".to_owned(), Value::Null)
        })?,
    };
    let extent = view.extent();
    let note = crate::annotate::annotations_from(world, &view)
        .into_iter()
        .find(|note| Some(note.id) == id);
    let share_of = |world: &mut World, rect: [u32; 4]| {
        view.pick_rect(world, rect)["entities"]
            .as_array()
            .and_then(|hits| hits.iter().find(|hit| hit["id"] == json!(id)))
            .map_or(0.0, |hit| hit["share"].as_f64().unwrap_or(0.0))
    };
    let (box_share, frame_share) = match &note {
        Some(note) => {
            let [l, t, r, b] = note.rect.map(|v| v.max(0) as u32);
            (
                share_of(world, [l, t, r + 1, b + 1]),
                share_of(world, [0, 0, extent[0], extent[1]]),
            )
        }
        None => (0.0, 0.0),
    };
    let visible = box_share > 0.0;
    let actual = json!({
        "rect": note.as_ref().map(|note| note.rect),
        "visible": visible,
        "box_share": box_share,
        "frame_share": frame_share,
    });
    let fail = |wanted: String| {
        Err((
            format!("{subject} is {actual}, expected {wanted}"),
            actual.clone(),
        ))
    };
    if let Some(wanted) = expect.on_screen {
        if visible != wanted {
            return fail(format!("on_screen {wanted}"));
        }
    }
    if let Some(wanted) = expect.occluded {
        if (note.is_some() && !visible) != wanted {
            return fail(format!("occluded {wanted}"));
        }
    }
    if let Some([left, top, right, bottom]) = expect.inside {
        let (w, h) = (extent[0] as f32, extent[1] as f32);
        let inside = note.as_ref().is_some_and(|note| {
            let [l, t, r, b] = note.rect.map(|v| v as f32);
            l >= left * w && t >= top * h && r <= right * w && b <= bottom * h
        });
        if !inside {
            return fail(format!("inside {:?}", expect.inside));
        }
    }
    if let Some(min) = expect.min_share {
        if frame_share < min {
            return fail(format!("frame share of at least {min}"));
        }
    }
    Ok(format!("{subject} is on the screen as expected"))
}

/// "; did you mean ..." with the three paths in `state` closest to `path`,
/// those ending in the same key first.
fn near_paths(state: &Value, path: &str) -> String {
    if path.starts_with("/gpu_state") && state.get("gpu_state").is_none() {
        return "; /gpu_state exists only on a GPU body with \
                `sync: PhysicsSyncMode::SelectedState` or `FullState`, \
                after its first readback"
            .into();
    }
    fn walk(value: &Value, at: String, out: &mut Vec<String>) {
        let children: Vec<(String, &Value)> = match value {
            Value::Object(map) => map
                .iter()
                .map(|(key, child)| {
                    (key.replace('~', "~0").replace('/', "~1"), child)
                })
                .collect(),
            Value::Array(items) => items
                .iter()
                .take(4)
                .enumerate()
                .map(|(index, child)| (index.to_string(), child))
                .collect(),
            _ => Vec::new(),
        };
        for (key, child) in children {
            let child_path = format!("{at}/{key}");
            out.push(child_path.clone());
            walk(child, child_path, out);
        }
    }
    let mut paths = Vec::new();
    walk(state, String::new(), &mut paths);
    let last = |path: &str| path.rsplit('/').next().unwrap_or("").to_owned();
    paths.sort_by_key(|candidate| {
        (
            last(candidate) != last(path),
            crate::scene_patch::edit_distance(candidate, path),
        )
    });
    if paths.is_empty() {
        return String::new();
    }
    format!("; did you mean {}", paths[..paths.len().min(3)].join(", "))
}

fn check_value(world: &mut World, expect: &Expectation) -> Check {
    let subject = format!("`{}` {}", expect.entity, expect.path);
    let state = reflected(world, &expect.entity);
    if let Some(wanted) = expect.exists {
        // A counter nobody created reads as 0 but does not exist.
        let found = state.as_ref().is_ok_and(|state| {
            state.get("missing").is_none()
                && state.pointer(&expect.path).is_some()
        });
        if found != wanted {
            let missing = if wanted {
                "does not exist"
            } else {
                "still exists"
            };
            return Err((format!("{subject} {missing}"), Value::Bool(found)));
        }
        if !found {
            return Ok(format!("{subject} does not exist"));
        }
    }
    let state = state.map_err(|error| (error, Value::Null))?;
    // A clip that never played has no entry; it reads as 0 plays.
    let unplayed = expect.entity == AUDIO_ENTITY
        && expect
            .path
            .strip_prefix("/clips/")
            .is_some_and(|clip| !clip.is_empty() && !clip.contains('/'));
    let actual = state.pointer(&expect.path).cloned();
    let actual = actual.or_else(|| unplayed.then(|| json!(0)));
    let actual = actual.ok_or_else(|| {
        (
            format!(
                "{subject} does not exist{}",
                near_paths(&state, &expect.path)
            ),
            Value::Null,
        )
    })?;
    // An array compares by its length: `/playing` greater_than 2 means at
    // least three sounds.
    let number = actual
        .as_f64()
        .or_else(|| actual.as_array().map(|items| items.len() as f64));
    if expect.finite && has_null_number(&actual) {
        return Err((
            format!("{subject} is {actual}, expected finite numbers"),
            actual,
        ));
    }
    let fail = |wanted: String| {
        Err((
            format!("{subject} is {actual}, expected {wanted}"),
            actual.clone(),
        ))
    };
    if let Some(equals) = &expect.equals {
        if !close(&actual, equals, expect.tolerance) {
            return fail(format!("{equals} ± {}", expect.tolerance));
        }
    }
    if let Some(differs) = &expect.not_equals {
        if close(&actual, differs, expect.tolerance) {
            return fail(format!("not {differs} ± {}", expect.tolerance));
        }
    }
    if let Some(bound) = expect.greater_than {
        if number.is_none_or(|number| number <= bound) {
            return fail(format!("greater than {bound}"));
        }
    }
    if let Some(bound) = expect.less_than {
        if number.is_none_or(|number| number >= bound) {
            return fail(format!("less than {bound}"));
        }
    }
    Ok(format!("{subject} is {actual}"))
}

/// Whether a value is `null` or holds a `null` array element or field.
fn has_null_number(value: &Value) -> bool {
    match value {
        Value::Null => true,
        Value::Array(items) => items.iter().any(has_null_number),
        Value::Object(fields) => fields.values().any(has_null_number),
        _ => false,
    }
}

/// Whether `actual` equals `wanted`, with numbers, including those inside
/// arrays and objects such as a position, allowed to differ by `tolerance`.
fn close(actual: &Value, wanted: &Value, tolerance: f64) -> bool {
    match (actual, wanted) {
        (Value::Number(a), Value::Number(b)) => {
            // A whole number is not f32 state: counters above 2^24 must
            // match exactly.
            let whole = |n: &serde_json::Number| n.is_i64() || n.is_u64();
            a.as_f64().zip(b.as_f64()).is_some_and(|(x, y)| {
                // Engine state is f32. serde_json parses decimal text only to
                // within an f64 ulp, so an exact f32 also matches any
                // spelling that rounds to the same f32.
                (x - y).abs() <= tolerance
                    || (!(whole(a) && whole(b))
                        && f64::from(x as f32) == x
                        && x as f32 == y as f32)
            })
        }
        (Value::Array(a), Value::Array(b)) => {
            a.len() == b.len()
                && a.iter().zip(b).all(|(a, b)| close(a, b, tolerance))
        }
        (Value::Object(a), Value::Object(b)) => {
            a.len() == b.len()
                && a.iter().all(|(key, a)| {
                    b.get(key).is_some_and(|b| close(a, b, tolerance))
                })
        }
        _ => actual == wanted,
    }
}

fn check_events(
    world: &mut World,
    expect: &EventExpectation,
    events: &[SeenEvent],
) -> Check {
    let involved = match &expect.entity {
        Some(wanted) => Some(
            find_entity(world, wanted, |_, _| true)
                .and_then(|entity| world.get::<SceneId>(entity))
                .map(|id| id.0)
                .ok_or_else(|| {
                    (
                        format!("no entity has the ID or name `{wanted}`"),
                        Value::Null,
                    )
                })?,
        ),
        None => None,
    };
    let count = events
        .iter()
        .filter(|event| {
            involved.is_none_or(|id| event.a == Some(id) || event.b == Some(id))
        })
        .count();
    let subject = match &expect.entity {
        Some(entity) => {
            format!("{:?} events involving `{entity}`", expect.kind)
        }
        None => format!("{:?} events", expect.kind),
    };
    if expect.at_least.is_some_and(|least| count < least)
        || expect.at_most.is_some_and(|most| count > most)
    {
        return Err((
            format!(
                "{subject}: {count}, expected {}..={}",
                expect.at_least.unwrap_or(0),
                expect.at_most.map_or("any".into(), |most| most.to_string())
            ),
            json!(count),
        ));
    }
    Ok(format!("{subject}: {count}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::{
        Collider, ColliderShape, HybridPhysicsPlugin, InputBinding, KeyCode,
        PhysicsBody, RenderExtractPlugin, RigidBody, RigidBodyKind,
        ScheduleStage,
    };

    #[test]
    fn a_stalled_run_reports_its_last_finished_tick() {
        use std::sync::atomic::{AtomicU64, Ordering};
        use std::sync::Arc;
        let ticks = Arc::new(AtomicU64::new(0));
        let worker = Arc::clone(&ticks);
        // Ten ticks finish, then the game hangs.
        let game = std::thread::spawn(move || {
            for _ in 0..10 {
                worker.fetch_add(1, Ordering::Relaxed);
                std::thread::sleep(Duration::from_millis(5));
            }
        });
        let finished = wait_for_stall(
            Duration::from_millis(150),
            Duration::from_millis(10),
            || ticks.load(Ordering::Relaxed),
        );
        game.join().unwrap();
        assert_eq!(finished, 10);
        let message = stall_message(Duration::from_secs(60), finished);
        assert!(message.starts_with("no tick finished in 60 s"), "{message}");
        assert!(message.contains("last finished tick is 9"), "{message}");
        assert!(message.contains(STALL_SECS_ENV), "{message}");
        assert!(stall_message(Duration::from_secs(60), 0)
            .contains("tick 0 never finished"));
    }
    #[test]
    fn set_reaches_built_in_components() {
        let mut world = World::new();
        let entity = world
            .spawn((
                Name("Crate".into()),
                RigidBody::default(),
                Collider::default(),
            ))
            .id();
        let set = |path: &str, value: Value| Assignment {
            entity: "Crate".into(),
            counter: None,
            path: path.into(),
            value,
        };
        assign(&mut world, &set("/collider/friction", json!(0.0))).unwrap();
        assign(&mut world, &set("/rigid_body/mass", json!(0.2))).unwrap();
        assign(&mut world, &set("/visible", json!(false))).unwrap();
        assert_eq!(world.get::<Collider>(entity).unwrap().friction, 0.0);
        assert_eq!(world.get::<RigidBody>(entity).unwrap().mass, 0.2);
        assert!(
            !world
                .get::<crate::runtime::Visibility>(entity)
                .unwrap()
                .visible
        );
        let missing =
            assign(&mut world, &set("/point_light/range", json!(3.0)));
        assert!(missing.unwrap_err().contains("does not have"));
        let typo = assign(&mut world, &set("/collider/frction", json!(1.0)));
        assert!(typo.unwrap_err().contains("does not exist"));
    }

    #[test]
    fn a_press_before_the_first_update_leaves_no_stale_binding() {
        use crate::runtime::InputAction;
        let mut app = App::new();
        let owner = app
            .world_mut()
            .spawn(InputAction {
                action: "fire".into(),
                inputs: vec!["KeyF".into()],
            })
            .id();
        press(app.world_mut(), "fire", true, None).unwrap();
        app.update(std::time::Duration::from_millis(16)).unwrap();
        app.world_mut().despawn(owner);
        app.update(std::time::Duration::from_millis(16)).unwrap();
        assert!(app
            .world()
            .resource::<ActionMap>()
            .bindings("fire")
            .is_empty());
    }

    #[test]
    fn a_press_at_a_fraction_records_that_press_tick() {
        use crate::runtime::InputAction;
        let mut app = App::new();
        app.world_mut().spawn(InputAction {
            action: "fire".into(),
            inputs: vec!["KeyF".into()],
        });
        press(app.world_mut(), "fire", true, Some(121.4)).unwrap();
        let world = app.world();
        let tick = world
            .resource::<ActionMap>()
            .press_tick(world.resource::<RuntimeInput>(), "fire");
        assert_eq!(tick, Some(121.4));
    }

    #[test]
    fn whole_numbers_above_the_f32_range_match_exactly() {
        assert!(!close(
            &serde_json::json!(16_777_216_u64),
            &serde_json::json!(16_777_217_u64),
            0.0
        ));
        assert!(close(
            &serde_json::json!(16_777_217_u64),
            &serde_json::json!(16_777_217_u64),
            0.0
        ));
    }

    #[test]
    fn a_printed_f32_matches_itself_with_tolerance_zero() {
        let actual = serde_json::json!(-1.733_868_f32);
        let printed: Value = serde_json::from_str(&actual.to_string()).unwrap();
        assert!(close(&actual, &printed, 0.0));
        let nearby: Value =
            serde_json::from_str("-1.7338680028915403").unwrap();
        assert!(close(&actual, &nearby, 0.0));
        assert!(!close(&actual, &serde_json::json!(-1.7338), 0.0));
    }

    #[test]
    fn tolerance_applies_to_every_number_in_a_vector_or_object() {
        let position = serde_json::json!([1.5, -2.0, 0.100_000_001_5]);
        assert!(close(&position, &serde_json::json!([1.5, -2, 0.1]), 1e-3));
        assert!(!close(&position, &serde_json::json!([1.5, -2, 0.1]), 0.0));
        assert!(!close(&position, &serde_json::json!([1.5, -2.0]), 1.0));
        let color = serde_json::json!({"r": 0.5, "name": "red"});
        assert!(close(
            &color,
            &serde_json::json!({"r": 0.51, "name": "red"}),
            0.02
        ));
        assert!(!close(
            &color,
            &serde_json::json!({"r": 0.5, "name": "blue"}),
            1.0
        ));
    }

    #[test]
    fn pad_presses_and_stick_steps_reach_runtime_input() {
        let mut world = World::new();
        world.insert_resource(RuntimeInput::default());
        world.insert_resource(ActionMap::default());
        let pad = crate::runtime::parse_input("PadSouth").unwrap();
        assert!(crate::runtime::parse_input("PadSouthh")
            .unwrap_err()
            .contains("PadDpadUp"));
        world.resource_mut::<ActionMap>().bind("jump", pad);
        press(&mut world, "jump", true, None).unwrap();
        tilt_stick(&mut world, Stick::Left, [0.0, 1.0]).unwrap();
        let input = world.resource::<RuntimeInput>();
        assert!(input.pad_held(crate::runtime::PadButton::South));
        assert!(input.pad_held(crate::runtime::PadButton::LeftStickUp));
        assert_eq!(input.stick(Stick::Left), [0.0, 1.0]);
    }

    /// A cube falling onto fixed ground, and a game system that lifts the
    /// ground by 1 while `jump` is held and writes a seeded value to the
    /// ground's x.
    fn game() -> App {
        let mut app = App::new();
        app.add_plugin(crate::AssetPlugin).unwrap();
        app.add_plugin(HybridPhysicsPlugin).unwrap();
        app.add_plugin(RenderExtractPlugin).unwrap();
        app.world_mut()
            .resource_mut::<ActionMap>()
            .bind("jump", InputBinding::Key(KeyCode::Space));
        app.add_system(ScheduleStage::Update, |world: &mut World| {
            let held = world
                .resource::<ActionMap>()
                .held(world.resource::<RuntimeInput>(), "jump");
            let tick = world.resource::<FrameTime>().fixed_tick;
            let x = world.resource::<RandomSeed>().unit(tick, 0);
            let mut query = world.query::<(&Name, &mut Transform)>();
            for (name, mut transform) in query.iter_mut(world) {
                if name.0 == "Ground" {
                    transform.position =
                        [x, if held { 1.0 } else { -0.5 }, 0.0];
                }
            }
        });
        for (name, y, kind, half) in [
            ("Cube", 3.0, RigidBodyKind::Dynamic, [0.5; 3]),
            ("Ground", -0.5, RigidBodyKind::Fixed, [5.0, 0.5, 5.0]),
        ] {
            app.world_mut().spawn((
                SceneId(Uuid::new_v4()),
                Name(name.into()),
                Transform::new([0.0, y, 0.0]),
                PhysicsBody::default(),
                RigidBody {
                    kind,
                    ..RigidBody::default()
                },
                Collider {
                    shape: ColliderShape::Box { half_extents: half },
                    ..Collider::default()
                },
            ));
        }
        app
    }

    fn scenario(ticks: u32, steps: Value) -> Scenario {
        serde_json::from_value(
            json!({"name": "test", "seed": 7, "ticks": ticks, "steps": steps}),
        )
        .unwrap()
    }

    fn run(scenario: &Scenario) -> ScenarioReport {
        run_scenario(&mut game(), scenario, Path::new("."))
    }

    #[test]
    fn set_turns_a_player_controllers_look() {
        let mut app = game();
        app.world_mut().spawn((
            SceneId(Uuid::new_v4()),
            Name("Seat".into()),
            Transform::new([0.0, 5.0, 0.0]),
            crate::runtime::PlayerController::default(),
        ));
        let look = "/components/rusting.player_controller";
        let report = run_scenario(
            &mut app,
            &scenario(
                4,
                json!([
                    {"tick": 1, "set": {"entity": "Seat",
                        "path": format!("{look}/yaw"), "value": 1.0}},
                    {"tick": 1, "set": {"entity": "Seat",
                        "path": format!("{look}/pitch"), "value": -0.3}},
                    {"tick": 2, "expect": {"entity": "Seat",
                        "path": "/transform/rotation/1", "equals": 1.0,
                        "tolerance": 1e-5}},
                    {"tick": 2, "expect": {"entity": "Seat",
                        "path": format!("{look}/pitch"), "equals": -0.3,
                        "tolerance": 1e-5}},
                    // A rotation set turns the controller, which owns it.
                    {"tick": 3, "set": {"entity": "Seat",
                        "path": "/transform/rotation", "value": [0.2, 1.5, 0.0]}},
                    {"tick": 4, "expect": {"entity": "Seat",
                        "path": format!("{look}/yaw"), "equals": 1.5,
                        "tolerance": 1e-5}},
                    {"tick": 4, "expect": {"entity": "Seat",
                        "path": format!("{look}/pitch"), "equals": 0.2,
                        "tolerance": 1e-5}},
                    {"tick": 4, "expect": {"entity": "Seat",
                        "path": "/transform/rotation/1", "equals": 1.5,
                        "tolerance": 1e-5}},
                ]),
            ),
            Path::new("."),
        );
        assert!(report.passed, "{:#?}", report.steps);
    }

    #[test]
    fn a_focus_step_releases_held_inputs_until_focus_returns() {
        let mut app = game();
        let ground = |tick, y: f64| {
            json!({"tick": tick, "expect": {"entity": "Ground",
                "path": "/transform/position/1", "equals": y}})
        };
        let report = run_scenario(
            &mut app,
            &scenario(
                8,
                json!([
                    {"tick": 1, "press": "jump"},
                    ground(2, 1.0),
                    {"tick": 3, "focus": false},
                    ground(3, -0.5),
                    {"tick": 5, "focus": true},
                    {"tick": 6, "press": "jump"},
                    ground(6, 1.0),
                ]),
            ),
            Path::new("."),
        );
        assert!(report.passed, "{:#?}", report.steps);
        assert!(app.world().resource::<RuntimeInput>().focused());
        app.world_mut()
            .resource_mut::<RuntimeInput>()
            .record_focus(false);
        let scene = crate::project_runner::GameScene {
            world: app.world_mut(),
        };
        assert!(!scene.window_focused());
        scene
            .world
            .resource_mut::<RuntimeInput>()
            .record_focus(true);
        assert!(scene.window_focused());
    }

    #[test]
    fn keep_going_reports_every_failed_check() {
        let mut scenario = scenario(
            5,
            json!([
                {"tick": 1, "until": 3, "expect": {"entity": "Cube",
                    "path": "/transform/position/1", "equals": 99.0}},
                {"tick": 4, "expect": {"entity": "Cube",
                    "path": "/transform/position/1", "equals": -99.0}},
            ]),
        );
        assert_eq!(run(&scenario).steps.len(), 1);
        scenario.keep_going = true;
        let report = run(&scenario);
        assert!(!report.passed);
        let failed: Vec<_> = report.steps.iter().filter(|s| !s.ok).collect();
        assert_eq!(
            failed.iter().map(|s| (s.tick, s.step)).collect::<Vec<_>>(),
            [(1, 0), (4, 1)]
        );
        assert_eq!(report.ticks_run, 5);
    }

    #[test]
    fn screen_checks_see_a_mesh_in_front_of_a_camera_and_behind_a_wall() {
        use crate::runtime::{Camera, MeshRenderer, Projection};
        let mut app = App::new();
        app.add_plugin(crate::AssetPlugin).unwrap();
        app.add_plugin(RenderExtractPlugin).unwrap();
        let (mesh, material) = {
            let assets = app.world().resource::<AssetServer>();
            (assets.fallback_mesh, assets.fallback_material)
        };
        app.spawn((
            Transform::new([0.0, 0.0, 5.0]),
            Camera {
                projection: Projection::Perspective {
                    vertical_fov_radians: 1.0,
                    near: 0.1,
                    far: 100.0,
                },
                active: true,
                priority: 0,
                viewport: None,
            },
        ));
        // `Near` at the origin, `Wall` big and closer, `Far` hidden behind
        // it, `Side` far off to the right of the frame.
        for (name, at, scale) in [
            ("Near", [0.0, 0.0, 0.0], 1.0),
            ("Wall", [0.0, 0.0, 2.0], 6.0),
            ("Far", [0.0, 0.0, -3.0], 1.0),
            ("Side", [40.0, 0.0, 0.0], 1.0),
        ] {
            app.spawn((
                SceneId(Uuid::new_v4()),
                Name(name.into()),
                Transform::new(at).with_scale(scale, scale, scale),
                MeshRenderer {
                    mesh,
                    material,
                    cast_shadows: true,
                    receive_shadows: true,
                },
            ));
        }
        let mut scenario = scenario(
            1,
            json!([
                {"tick": 1, "expect_screen": {"entity": "Wall", "on_screen": true,
                    "inside": [0.0, 0.0, 1.0, 1.0], "min_share": 0.2}},
                {"tick": 1, "expect_screen": {"entity": "Far", "occluded": true}},
                {"tick": 1, "expect_screen": {"entity": "Side", "on_screen": false}},
                {"tick": 1, "expect_screen": {"entity": "Far", "on_screen": true}},
                {"tick": 1, "expect_screen": {"entity": "Near", "min_share": 0.01}},
            ]),
        );
        scenario.keep_going = true;
        let report = run_scenario(&mut app, &scenario, Path::new("."));
        let failed: Vec<_> = report
            .steps
            .iter()
            .filter(|s| !s.ok)
            .map(|s| s.step)
            .collect();
        assert_eq!(failed, [3, 4], "{:?}", report.steps);
    }

    #[test]
    fn invariants_are_checked_every_tick_and_report_the_first_bad_tick() {
        let mut holds = scenario(4, json!([]));
        holds.invariants = serde_json::from_value(json!([
            {"entity": "Cube", "path": "/transform/position", "finite": true},
            {"entity": "Cube", "path": "/transform/position/1", "greater_than": -1000.0},
            {"entity": "Gone", "path": "/transform", "finite": true},
        ]))
        .unwrap();
        let report = run(&holds);
        assert!(report.passed, "{:?}", report.steps);

        let mut broken = scenario(4, json!([]));
        broken.invariants = serde_json::from_value(json!([
            {"entity": "Cube", "path": "/transform/position/1", "less_than": -1000.0},
        ]))
        .unwrap();
        let failure = run(&broken).first_failure.unwrap();
        assert_eq!(failure.tick, 0, "checked after the first tick");
        assert!(failure.message.starts_with("invariant 0:"), "{failure:?}");

        assert!(has_null_number(&json!([0.0, null, 1.0])));
        assert!(!has_null_number(&json!([0.0, 2.0])));

        let mut empty = scenario(1, json!([]));
        empty.invariants =
            serde_json::from_value(json!([{"entity": "Cube"}])).unwrap();
        assert!(run(&empty)
            .first_failure
            .unwrap()
            .message
            .contains("invariant 0"));
    }

    #[test]
    fn coverage_lists_what_a_run_changed_and_left_alone() {
        let mut app = game();
        app.world_mut().spawn((
            SceneId(Uuid::new_v4()),
            Name("Sign".into()),
            Transform::new([4.0, 0.0, 0.0]),
        ));
        let scenario = scenario(30, json!([{"tick": 2, "press": "jump"}]));
        let coverage =
            run_scenario(&mut app, &scenario, Path::new(".")).coverage;
        let changed: Vec<_> = coverage["entities_changed"]
            .as_array()
            .unwrap()
            .iter()
            .map(|entity| entity["name"].as_str().unwrap())
            .collect();
        assert!(changed.contains(&"Cube"), "{coverage}");
        assert!(!changed.contains(&"Sign"), "{coverage}");
        assert_eq!(coverage["entities_unchanged"], 1, "{coverage}");
        assert_eq!(coverage["inputs"], json!(["jump"]));
        assert_eq!(coverage["events"]["input"], 1, "{coverage}");
        let untouched = coverage["sections_untouched"].as_array().unwrap();
        assert!(!untouched.is_empty(), "{coverage}");
        assert!(!untouched.contains(&json!("transform")), "{coverage}");
    }

    #[test]
    fn explorer_walks_to_reachable_goals_and_reports_caged_ones() {
        use crate::runtime::{
            Collider, PlayerController, RigidBody, RigidBodyKind,
            DEFAULT_PLAYER_SHAPE,
        };
        let mut app = App::new();
        let mut body = |name: &str, at: [f32; 3], half: [f32; 3], sensor| {
            let kind = if name == "Player" {
                RigidBodyKind::Kinematic
            } else {
                RigidBodyKind::Fixed
            };
            let shape = if name == "Player" {
                DEFAULT_PLAYER_SHAPE
            } else {
                ColliderShape::Box { half_extents: half }
            };
            app.world_mut()
                .spawn((
                    SceneId(Uuid::new_v4()),
                    Name(name.into()),
                    Transform::new(at),
                    PhysicsBody::default(),
                    RigidBody {
                        kind,
                        ..RigidBody::default()
                    },
                    Collider {
                        shape,
                        sensor,
                        ..Collider::default()
                    },
                ))
                .id()
        };
        body("Ground", [0.0, -0.5, 0.0], [20.0, 0.5, 20.0], false);
        body("Coin", [3.0, 0.5, -4.0], [0.3; 3], true);
        body("Flag", [-2.0, 0.5, 3.0], [0.3; 3], true);
        // Four walls 2 m high around the cage goal.
        body("Caged", [-8.0, 0.5, -8.0], [0.3; 3], true);
        for (x, z, half) in [
            (-8.0, -9.5, [2.0, 1.0, 0.2]),
            (-8.0, -6.5, [2.0, 1.0, 0.2]),
            (-9.5, -8.0, [0.2, 1.0, 2.0]),
            (-6.5, -8.0, [0.2, 1.0, 2.0]),
        ] {
            body("Wall", [x, 1.0, z], half, false);
        }
        let player = body("Player", [0.0, 1.0, 0.0], [0.0; 3], false);
        app.world_mut().entity_mut(player).insert(PlayerController {
            yaw: 0.7,
            ..PlayerController::default()
        });
        let mut scenario = scenario(1500, json!([]));
        scenario.explore = Some(Explore {
            goals: Vec::new(),
            ticks_per_goal: 400,
        });
        let report = run_scenario(&mut app, &scenario, Path::new("."));
        let goals = &report.explore["goals"];
        let reached = |name: &str| {
            goals
                .as_array()
                .unwrap()
                .iter()
                .find(|goal| goal["name"] == name)
                .map(|goal| goal["reached"].clone())
                .unwrap()
        };
        assert!(reached("Coin").is_u64(), "{}", report.explore);
        assert!(reached("Flag").is_u64(), "{}", report.explore);
        assert!(reached("Caged").is_null(), "{}", report.explore);
        // The nearer flag (3.6 m) goes before the coin (5 m).
        assert_eq!(goals[0]["name"], "Flag", "{}", report.explore);
        assert!(
            !report.explore["stuck"].as_array().unwrap().is_empty(),
            "{}",
            report.explore
        );
        let failure = report.first_failure.unwrap();
        assert_eq!(failure.message, "explore: could not reach `Caged`");
        let mixed: Explore =
            serde_json::from_value(json!({"goals": ["Coin", [1, 0, 2.5]]}))
                .unwrap();
        assert_eq!(
            mixed.goals,
            ["Coin".into(), ExploreGoal::Point([1.0, 0.0, 2.5])]
        );
        // A list of points walks a route in its order.
        let route = [[2.0, 1.0, 2.0], [-3.0, 1.0, -1.0]];
        scenario.explore = Some(Explore {
            goals: route.map(ExploreGoal::Point).to_vec(),
            ticks_per_goal: 400,
        });
        let report = run_scenario(&mut app, &scenario, Path::new("."));
        let goals = report.explore["goals"].as_array().unwrap();
        assert_eq!(goals.len(), 2, "{}", report.explore);
        assert_eq!(goals[0]["name"], "[2.0, 1.0, 2.0]");
        let ticks: Vec<_> =
            goals.iter().map(|goal| goal["reached"].as_u64()).collect();
        assert!(ticks[0].unwrap() < ticks[1].unwrap(), "{}", report.explore);
        // A named goal list is walked in its order.
        scenario.explore = Some(Explore {
            goals: vec!["Nowhere".into()],
            ticks_per_goal: 10,
        });
        let report = run_scenario(&mut App::new(), &scenario, Path::new("."));
        assert_eq!(
            report.first_failure.unwrap().message,
            "explore: no entity `Nowhere`"
        );
    }

    #[test]
    fn a_recorded_session_becomes_press_release_and_tap_steps() {
        use crate::runtime::{Replay, ReplayFrame, REPLAY_FORMAT_VERSION};
        let mut map = ActionMap::default();
        map.bind("jump", InputBinding::Key(KeyCode::Space));
        map.bind("left", InputBinding::Key(KeyCode::ArrowLeft));
        map.bind("left", InputBinding::Key(KeyCode::KeyA));
        let mut input = RuntimeInput::default();
        let mut frames = Vec::new();
        let mut frame = |tick, input: Option<&RuntimeInput>| {
            frames.push(ReplayFrame {
                tick,
                delta_nanos: 16_000_000,
                input: input.cloned(),
            });
        };
        frame(10, Some(&input));
        input.record_key(KeyCode::ArrowLeft, true);
        frame(12, Some(&input));
        input.clear_frame_edges();
        input.record_key(KeyCode::KeyA, true);
        frame(13, Some(&input));
        input.clear_frame_edges();
        input.record_key(KeyCode::ArrowLeft, false);
        input.record_key(KeyCode::KeyA, false);
        input.record_key(KeyCode::Space, true);
        input.record_key(KeyCode::Space, false);
        frame(15, Some(&input));
        frame(16, None);
        frame(20, None);
        let replay = Replay {
            format_version: REPLAY_FORMAT_VERSION,
            seed: 7,
            start_tick: 10,
            frames,
            hashes: Vec::new(),
        };
        let scenario = scenario_from_replay("bug", &replay, &map);
        assert_eq!(
            scenario,
            serde_json::json!({"name": "bug", "seed": 7, "ticks": 10,
                "steps": [{"tick": 2, "press": "left"},
                          {"tick": 5, "tap": "jump"},
                          {"tick": 5, "release": "left"}]})
        );
        let parsed: Scenario = serde_json::from_value(scenario).unwrap();
        assert_eq!(parsed.steps.len(), 3);
    }

    #[test]
    fn fuzz_presses_seeded_actions_and_its_steps_replay_the_failure() {
        // The ground rises while `jump` is held, breaking the invariant.
        let mut fuzzed = scenario(120, json!([]));
        fuzzed.invariants = serde_json::from_value(json!([
            {"entity": "Ground", "path": "/transform/position/1", "less_than": 0.5},
        ]))
        .unwrap();
        fuzzed.fuzz = Some(Fuzz {
            seed: 3,
            ..serde_json::from_value(json!({})).unwrap()
        });
        let report = run(&fuzzed);
        let failure = report.first_failure.clone().unwrap();
        assert!(failure.message.starts_with("invariant 0"), "{failure:?}");
        let first = &report.fuzz_steps[0];
        assert_eq!(first["press"], "jump", "{first}");
        assert_eq!(failure.tick, first["tick"].as_u64().unwrap() as u32);
        assert_eq!(run(&fuzzed).fuzz_steps, report.fuzz_steps, "same seed");

        // The reported steps alone replay the same failure.
        let mut replay = fuzzed.clone();
        replay.fuzz = None;
        replay.steps =
            serde_json::from_value(json!(report.fuzz_steps)).unwrap();
        assert_eq!(run(&replay).first_failure.unwrap().tick, failure.tick);

        let other = Fuzz {
            seed: 4,
            ..fuzzed.fuzz.clone().unwrap()
        };
        let jump = ["jump".to_owned()];
        assert_ne!(
            serde_json::to_string(&other.steps(&jump, 120)).unwrap(),
            serde_json::to_string(&fuzzed.fuzz.unwrap().steps(&jump, 120))
                .unwrap()
        );
    }

    #[test]
    #[cfg_attr(
        not(feature = "gpu-tests"),
        ignore = "run with `--features gpu-tests` on a machine with a Vulkan driver"
    )]
    fn the_gpu_flag_opens_the_device_without_a_capture_step() {
        let mut scenario = scenario(2, json!([]));
        scenario.gpu = true;
        let report = run(&scenario);
        assert!(report.passed, "{:?}", report.steps);
        assert!(!report.perf.render.is_null(), "a renderer was opened");
    }

    #[test]
    #[cfg_attr(
        not(feature = "gpu-tests"),
        ignore = "run with `--features gpu-tests` on a machine with a Vulkan driver"
    )]
    fn gpu_bodies_step_without_drawing_and_hash_the_same() {
        let gpu_game = || {
            let mut app = App::new();
            app.add_plugin(crate::AssetPlugin).unwrap();
            app.add_plugin(HybridPhysicsPlugin).unwrap();
            app.add_plugin(RenderExtractPlugin).unwrap();
            for index in 0..8 {
                app.world_mut().spawn((
                    SceneId(Uuid::new_v4()),
                    Name(format!("Gpu{index}")),
                    Transform::new([
                        index as f32 * 0.3,
                        2.0 + index as f32,
                        0.0,
                    ]),
                    PhysicsBody {
                        simulation: crate::runtime::SimulationClass::Gpu,
                        ..PhysicsBody::default()
                    },
                    RigidBody::default(),
                    Collider::default(),
                ));
            }
            app
        };
        let folder = std::env::temp_dir()
            .join(format!("rusting-physics-only-{}", std::process::id()));
        let mut drawn = scenario(30, json!([]));
        drawn.gpu = true;
        drawn.capture_size = [64, 48];
        let drawn = run_scenario(&mut gpu_game(), &drawn, &folder);
        let mut skipped =
            scenario(30, json!([{"tick": 30, "capture": "end.png"}]));
        skipped.capture_size = [64, 48];
        let skipped = run_scenario(&mut gpu_game(), &skipped, &folder);
        let _ = std::fs::remove_dir_all(&folder);
        assert!(
            drawn.passed && skipped.passed,
            "{:?}",
            skipped.first_failure
        );
        assert!(
            drawn.gpu_state_hashes.len() >= 20,
            "{:?}",
            drawn.gpu_state_hashes
        );
        assert_eq!(drawn.gpu_state_hashes, skipped.gpu_state_hashes);
        assert!(drawn.perf.render["gpu_frames"].as_u64().unwrap() >= 30);
        assert_eq!(
            skipped.perf.render["gpu_frames"],
            RENDER_WARMUP_TICKS + 1,
            "only the ticks before the capture draw"
        );
    }

    #[test]
    #[cfg_attr(
        not(feature = "gpu-tests"),
        ignore = "run with `--features gpu-tests` on a machine with a Vulkan driver"
    )]
    fn contact_grid_overflow_is_in_perf_and_the_world() {
        let mut app = App::new();
        app.add_plugin(crate::AssetPlugin).unwrap();
        app.add_plugin(HybridPhysicsPlugin).unwrap();
        app.add_plugin(RenderExtractPlugin).unwrap();
        // Twenty kinematic bodies in one spot: one cell holds eight.
        for index in 0..20 {
            app.world_mut().spawn((
                SceneId(Uuid::new_v4()),
                Name(format!("Crowd{index}")),
                Transform::new([index as f32 * 0.01, 0.0, 0.0]),
                PhysicsBody {
                    simulation: crate::runtime::SimulationClass::Gpu,
                    ..PhysicsBody::default()
                },
                RigidBody {
                    kind: crate::runtime::RigidBodyKind::Kinematic,
                    ..RigidBody::default()
                },
                Collider::default(),
            ));
        }
        let folder = std::env::temp_dir()
            .join(format!("rusting-grid-overflow-{}", std::process::id()));
        let mut run = scenario(10, json!([]));
        run.gpu = true;
        run.capture_size = [64, 48];
        let report = run_scenario(&mut app, &run, &folder);
        let _ = std::fs::remove_dir_all(&folder);
        assert!(report.passed, "{:?}", report.first_failure);
        let overflow = &report.perf.render["physics_grid_overflow"];
        assert!(overflow.as_u64().unwrap() > 0, "{}", report.perf.render);
        assert!(
            report.perf.render["physics_fallback_tests"]
                .as_u64()
                .unwrap()
                > 0
        );
        let world = app
            .world()
            .resource::<crate::rendering::scene_renderer::RenderCapacityDiagnostics>();
        assert!(world.physics_grid_overflow > 0, "{world:?}");
    }

    #[test]
    fn perf_is_reported_and_budgets_fail_the_run() {
        let mut passing = scenario(3, json!([]));
        passing.budgets = Some(Budgets {
            max_tick_ms: Some(60_000.0),
            ..Budgets::default()
        });
        let report = run(&passing);
        assert!(report.passed, "{:?}", report.steps);
        assert!(report.perf.tick_ms_max >= report.perf.tick_ms_mean);
        assert!(report.perf.wall_ms_mean >= report.perf.tick_ms_mean);
        assert!(report.perf.tick_ms_p95 > 0.0);
        assert_eq!(report.perf.environment["os"], std::env::consts::OS);
        assert!(report.perf.render.is_null());
        assert!(report.perf.entities_max > 0);
        let stages = &report.perf.stages_ms_mean;
        for stage in ["fixed", "update", "post_update", "extract"] {
            assert!(stages[stage].as_f64() > Some(0.0), "{stages}");
        }
        assert!(report.perf.environment["cpus"].as_u64() > Some(0));
        let mut busy = report.perf.clone();
        busy.tick_ms_max = 50.0;
        busy.environment["cpus"] = json!(4);
        busy.environment["load_average"] = json!(6.5);
        let limit = Budgets {
            max_tick_ms: Some(10.0),
            ..Budgets::default()
        };
        let message = &over_budget(&limit, &busy)[0];
        assert!(message.ends_with("(load average 6.5 on 4 CPUs: rerun it alone before treating it as real)"), "{message}");
        busy.environment["load_average"] = json!(1.0);
        assert!(over_budget(&limit, &busy)[0].ends_with("ms limit"));
        if cfg!(target_os = "linux") {
            assert!(report.perf.cpu_ms_mean.is_some_and(|ms| ms >= 0.0));
            let rss = report.perf.rss_mb.unwrap();
            assert!(rss > 1.0 && report.perf.rss_mb_peak >= Some(rss));
        }
        let cpu = Budgets {
            mean_cpu_ms: Some(1.0),
            ..Budgets::default()
        };
        busy.cpu_ms_mean = None;
        assert!(over_budget(&cpu, &busy).is_empty(), "not measured");
        busy.cpu_ms_mean = Some(5.0);
        assert!(over_budget(&cpu, &busy)[0]
            .starts_with("mean_cpu_ms is 5.000 ms, over the 1 ms limit"));
        busy.render = json!({"lights": 33});
        let lights = Budgets {
            max_lights: Some(32),
            ..Budgets::default()
        };
        assert_eq!(
            over_budget(&lights, &busy),
            ["max_lights: 33 lights, over the limit of 32"]
        );

        let mut crowded = scenario(3, json!([]));
        crowded.budgets = Some(Budgets {
            max_entities: Some(report.perf.entities_max - 1),
            ..Budgets::default()
        });
        let crowded = run(&crowded);
        assert!(!crowded.passed);
        assert!(crowded
            .first_failure
            .unwrap()
            .message
            .starts_with("budget: max_entities"));

        let mut failing = scenario(3, json!([]));
        failing.budgets = Some(Budgets {
            max_tick_ms: Some(0.0),
            ..Budgets::default()
        });
        let report = run(&failing);
        assert!(!report.passed);
        assert!(report
            .first_failure
            .unwrap()
            .message
            .starts_with("budget: max_tick_ms"));
    }

    #[test]
    #[cfg_attr(
        not(feature = "gpu-tests"),
        ignore = "run with `--features gpu-tests` on a machine with a Vulkan driver"
    )]
    fn a_render_budget_reports_render_counters_without_a_capture() {
        use crate::runtime::{Camera, MeshRenderer, Projection};
        let mut app = App::new();
        app.add_plugin(crate::AssetPlugin).unwrap();
        app.add_plugin(RenderExtractPlugin).unwrap();
        let (mesh, material) = {
            let assets = app.world().resource::<AssetServer>();
            (assets.fallback_mesh, assets.fallback_material)
        };
        app.spawn((
            Transform::new([0.0, 0.0, 5.0]),
            Camera {
                projection: Projection::Perspective {
                    vertical_fov_radians: 1.0,
                    near: 0.1,
                    far: 100.0,
                },
                active: true,
                priority: 0,
                viewport: None,
            },
        ));
        app.spawn((
            SceneId(Uuid::new_v4()),
            Transform::default(),
            MeshRenderer {
                mesh,
                material,
                cast_shadows: true,
                receive_shadows: true,
            },
        ));
        app.spawn((
            Transform::new([0.0, 2.0, 0.0]),
            crate::runtime::PointLight::default(),
        ));
        // A render budget renders without a capture step.
        let mut scenario = scenario(2, json!([]));
        scenario.capture_size = [160, 90];
        scenario.budgets = Some(Budgets {
            max_draws: Some(0),
            ..Budgets::default()
        });
        let report = run_scenario(&mut app, &scenario, &std::env::temp_dir());
        assert!(report.perf.render["draws"].as_u64().unwrap() > 0);
        assert!(report.perf.render["gpu_ms"].is_number());
        assert_eq!(report.perf.render["dropped_lights"], 0);
        assert_eq!(report.perf.render["lights"], 1);
        let render = &report.perf.render;
        let frames = render["gpu_frames"].as_u64().unwrap();
        assert!(frames >= report.ticks_run, "{render}");
        let p95 = render["gpu_ms_p95"].as_f64().unwrap();
        assert!(p95 <= render["gpu_ms_max"].as_f64().unwrap(), "{render}");
        let passes = render["passes_ms"].as_object().unwrap();
        assert!(passes.contains_key("Scene.opaque"), "{render}");
        assert!(report.perf.environment["device"].is_string());
        assert!(!report.passed);
        assert!(report
            .first_failure
            .unwrap()
            .message
            .starts_with("budget: max_draws"));
        // A capture renders only the ticks just before it, so a long run
        // with one late capture is not rendered throughout.
        let folder = std::env::temp_dir()
            .join(format!("rusting-late-capture-{}", std::process::id()));
        let mut late = scenario.clone();
        late.budgets = None;
        late.ticks = 40;
        late.steps = serde_json::from_value(
            json!([{"tick": 40, "capture": "late.png"}]),
        )
        .unwrap();
        late.capture_size = [160, 90];
        let report = run_scenario(&mut app, &late, &folder);
        assert!(report.passed, "{:?}", report.first_failure);
        assert_eq!(
            report.perf.render["gpu_frames"],
            RENDER_WARMUP_TICKS + 1,
            "{}",
            report.perf.render
        );
        assert!(folder.join("late.png").exists());
        let _ = std::fs::remove_dir_all(&folder);
    }

    #[test]
    #[cfg_attr(
        not(feature = "gpu-tests"),
        ignore = "run with `--features gpu-tests` on a machine with a Vulkan driver"
    )]
    fn pixel_checks_goldens_and_named_cameras() {
        use crate::runtime::{Camera, MeshRenderer, Projection};
        let mut app = App::new();
        app.add_plugin(crate::AssetPlugin).unwrap();
        app.add_plugin(RenderExtractPlugin).unwrap();
        let (mesh, material) = {
            let assets = app.world().resource::<AssetServer>();
            (assets.fallback_mesh, assets.fallback_material)
        };
        let camera = |active| Camera {
            projection: Projection::Perspective {
                vertical_fov_radians: 1.0,
                near: 0.1,
                far: 100.0,
            },
            active,
            priority: 0,
            viewport: None,
        };
        app.spawn((
            Name("Main".into()),
            Transform::new([0.0, 0.0, 3.0]),
            camera(true),
        ));
        let mut away = Transform::new([0.0, 0.0, 3.0]);
        away.rotation = [0.0, std::f32::consts::PI, 0.0];
        app.spawn((Name("Away".into()), away, camera(false)));
        app.spawn((
            SceneId(Uuid::new_v4()),
            Name("Cube".into()),
            Transform::default(),
            MeshRenderer {
                mesh,
                material,
                cast_shadows: true,
                receive_shadows: true,
            },
        ));
        let directory = std::env::temp_dir()
            .join(format!("rusting-golden-{}", std::process::id()));
        let mut run = scenario(
            1,
            json!([
                {"tick": 1, "expect_pixels": {"stddev_min": 1.0}},
                {"tick": 1, "expect_screen": {"entity": "Cube", "on_screen": true}},
                {"tick": 1, "expect_screen": {"entity": "Cube", "camera": "Away",
                    "on_screen": false}},
                {"tick": 1, "capture": {"path": "main.png", "golden": "main.golden.png"}},
                {"tick": 1, "capture": {"path": "away.png", "camera": "Away",
                    "golden": "away.golden.png"}},
                {"tick": 1, "expect_pixels": {"differs_from": "main.png",
                    "difference_max": 1.0}},
                {"tick": 1, "expect_pixels": {"differs_from": "away.png",
                    "difference_min": 1.0}},
            ]),
        );
        run.capture_size = [64, 48];
        run.update_golden = true;
        let report = run_scenario(&mut app, &run, &directory);
        assert!(report.passed, "{:?}", report.first_failure);
        assert!(directory.join("away.golden.png").is_file());

        // The second run compares; the away view must not match the main one.
        run.update_golden = false;
        let report = run_scenario(&mut app, &run, &directory);
        assert!(report.passed, "{:?}", report.first_failure);
        std::fs::copy(
            directory.join("main.golden.png"),
            directory.join("away.golden.png"),
        )
        .unwrap();
        let report = run_scenario(&mut app, &run, &directory);
        let failure = report.first_failure.unwrap();
        assert!(
            failure.message.contains("differs from golden"),
            "{failure:?}"
        );
        let _ = std::fs::remove_dir_all(directory);
    }

    #[test]
    fn capturing_through_the_shown_camera_keeps_the_hud() {
        use crate::runtime::{ExtractedCamera, Projection, RenderWorld};
        let mut world = World::new();
        let (shown, other) =
            (world.spawn_empty().id(), world.spawn_empty().id());
        assert!(!shown_alone(&world, shown), "nothing drawn yet");
        let view = |entity| ExtractedCamera {
            entity,
            transform: Default::default(),
            projection: Projection::Orthographic {
                vertical_size: 1.0,
                near: 0.1,
                far: 10.0,
            },
            priority: 0,
        };
        let mut render = RenderWorld::default();
        render.active_camera = Some(view(shown));
        world.insert_resource(render);
        assert!(shown_alone(&world, shown));
        assert!(!shown_alone(&world, other));
        world
            .resource_mut::<RenderWorld>()
            .views
            .push((view(other), [0.0, 0.0, 0.5, 1.0]));
        assert!(!shown_alone(&world, shown), "a split view is not one frame");
    }

    #[test]
    fn differs_from_needs_a_difference_limit() {
        let steps = |expect: Value| {
            json!({"name": "t", "seed": 1, "ticks": 2,
                "steps": [{"tick": 1, "expect_pixels": expect}]})
        };
        for (expect, ok) in [
            (json!({"differs_from": "a.png"}), false),
            (json!({"difference_min": 2.0}), false),
            (
                json!({"differs_from": "a.png", "difference_min": 2.0}),
                true,
            ),
        ] {
            let parsed = serde_json::from_value::<Scenario>(steps(expect))
                .map_err(|error| error.to_string())
                .and_then(|run| run.validate().map_err(|e| e.to_string()));
            assert_eq!(parsed.is_ok(), ok, "{parsed:?}");
        }
        let a = [10, 0, 0, 255, 0, 0, 0, 255];
        let b = [0, 0, 0, 255, 0, 0, 30, 255];
        assert_eq!(region_difference(&a, &b, 2, [0, 0, 2, 1]), 15.0);
        assert_eq!(region_difference(&a, &b, 2, [0, 0, 1, 1]), 10.0);
    }

    #[test]
    fn capture_steps_take_a_path_or_options() {
        let run = scenario(
            1,
            json!([
                {"tick": 1, "capture": "a.png"},
                {"tick": 1, "capture": {"path": "b.png", "hud": false,
                    "golden": "g.png", "tolerance": 2.0}},
            ]),
        );
        let StepAction::Capture(plain) = &run.steps[0].action else {
            panic!()
        };
        assert!(plain.hud && plain.golden.is_none() && plain.tolerance == 1.0);
        let StepAction::Capture(full) = &run.steps[1].action else {
            panic!()
        };
        assert!(!full.hud && full.tolerance == 2.0);
        assert_eq!(full.golden.as_deref(), Some(Path::new("g.png")));
    }

    #[test]
    fn region_stats_and_golden_differences() {
        // 2x1: black and white.
        let pixels = [0, 0, 0, 255, 255, 255, 255, 255];
        let (mean, stddev) = region_stats(&pixels, 2, [0, 0, 2, 1]);
        assert_eq!(mean, [127.5; 3]);
        assert!((stddev - 127.5).abs() < 1e-6);
        let golden = std::env::temp_dir()
            .join(format!("rusting-golden-unit-{}.png", std::process::id()));
        assert!(compare_golden(&pixels, [2, 1], &golden, 0.0, true).is_ok());
        assert!(compare_golden(&pixels, [2, 1], &golden, 0.0, false).is_ok());
        let shifted = [4, 4, 4, 255, 255, 255, 255, 255];
        let (message, actual) =
            compare_golden(&shifted, [2, 1], &golden, 1.0, false).unwrap_err();
        assert!(message.contains("mean difference 2.000"), "{message}");
        assert_eq!(actual["max_difference"], 4);
        let _ = std::fs::remove_file(golden);
    }

    #[test]
    fn the_trace_lists_inputs_sets_and_collisions_by_tick() {
        let report = run(&scenario(
            90,
            json!([
                {"tick": 5, "press": "jump"},
                {"tick": 6, "release": "jump"},
                {"tick": 7, "set": {"entity": "Ground",
                    "path": "/transform/position", "value": [0.0, -0.5, 0.0]}},
            ]),
        ));
        let kinds: Vec<_> = report
            .trace
            .iter()
            .map(|event| (event.tick, event.kind.as_str()))
            .collect();
        assert_eq!(&kinds[..3], [(5, "input"), (6, "input"), (7, "set")]);
        assert!(kinds.iter().any(|(_, kind)| *kind == "collision"));
        assert!(kinds.windows(2).all(|pair| pair[0].0 <= pair[1].0));
    }

    #[test]
    fn set_steps_move_entities_before_the_tick_runs() {
        let report = run(&scenario(
            20,
            json!([
                {"tick": 10, "set": {"entity": "Cube",
                    "path": "/transform/position", "value": [0.0, 20.0, 0.0]}},
                {"tick": 10, "expect": {"entity": "Cube",
                    "path": "/transform/position/1", "greater_than": 19.0}},
                {"tick": 11, "set": {"entity": "Cube",
                    "path": "/mesh", "value": 1}},
            ]),
        ));
        assert!(report.steps[0].ok && report.steps[1].ok, "{report:#?}");
        assert!(!report.passed);
        assert!(report.steps[2].message.contains("set reaches /transform"));
    }

    #[test]
    fn scenarios_expect_an_object_state() {
        let mut app = game();
        app.world_mut().spawn((
            SceneId(Uuid::new_v4()),
            Name("Guard".into()),
            crate::runtime::ObjectState {
                state: "chase".into(),
                ..Default::default()
            },
        ));
        let scenario = scenario(
            1,
            json!([{"tick": 1, "expect": {"entity": "Guard",
                "path": "/components/rusting.state/state", "equals": "chase"}}]),
        );
        let report = run_scenario(&mut app, &scenario, Path::new("."));
        assert!(report.passed, "{report:#?}");
    }

    #[test]
    fn counter_shorthand_sets_checks_and_logs_a_counter_by_name() {
        let mut app = game();
        app.world_mut().spawn((
            SceneId(Uuid::new_v4()),
            Name("Score Counter".into()),
            crate::runtime::Counter {
                name: "score".into(),
                value: 0,
                target: None,
            },
        ));
        let scenario = scenario(
            4,
            json!([
                {"tick": 2, "set": {"counter": "score", "value": 5}},
                {"tick": 2, "expect": {"counter": "score", "equals": 5}},
                {"tick": 3, "expect": {"counter": "score", "not_equals": 4}},
                {"tick": 3, "log": {"counter": "score"}},
                {"tick": 3, "set": {"counter": "fresh", "value": 2}},
                {"tick": 3, "expect": {"counter": "fresh", "equals": 2}},
                {"tick": 4, "expect": {"counter": "score", "not_equals": 5}},
            ]),
        );
        let report = run_scenario(&mut app, &scenario, Path::new("."));
        assert!(report.steps[..6].iter().all(|s| s.ok), "{report:#?}");
        let log = report.steps.iter().find(|s| s.step == 3).unwrap();
        assert_eq!(log.actual, json!(5));
        let last = report.first_failure.expect("not_equals 5 fails");
        assert!(last.message.contains("expected not 5"), "{}", last.message);

        // A counter nobody created yet reads as 0, as in game code.
        let unborn = super::tests::scenario(
            1,
            json!([
                {"tick": 1, "expect": {"counter": "plays", "equals": 0}},
                {"tick": 1, "expect": {"counter": "plays", "exists": false}},
            ]),
        );
        let report = run_scenario(&mut game(), &unborn, Path::new("."));
        assert!(report.passed, "{report:#?}");

        let both = scenario_from(json!({"tick": 1, "expect": {
            "counter": "score", "entity": "Cube", "equals": 1}}));
        assert!(both.validate().unwrap_err().contains("either entity"));
        let neither =
            scenario_from(json!({"tick": 1, "expect": {"equals": 1}}));
        assert!(neither.validate().is_err());
    }

    #[test]
    fn a_realtime_scenario_runs_one_tick_per_fixed_step_of_real_time() {
        let mut app = game();
        let step = app.world().resource::<FrameTime>().fixed_delta;
        let mut paced = scenario(6, json!([]));
        paced.realtime = true;
        let started = std::time::Instant::now();
        assert!(run_scenario(&mut app, &paced, Path::new(".")).passed);
        assert!(started.elapsed() >= step * 6, "{:?}", started.elapsed());
    }

    #[test]
    fn the_audio_entity_counts_sound_cue_requests_per_clip() {
        let mut app = game();
        let mut cue = crate::runtime::SoundCue {
            clip: "sfx/hit.wav".into(),
            ..Default::default()
        };
        cue.trigger();
        app.world_mut().spawn(cue);
        let report = run_scenario(
            &mut app,
            &scenario(
                4,
                json!([
                    {"tick": 0, "expect": {"entity": "audio:",
                        "path": "/requested", "equals": 0}},
                    {"tick": 3, "expect": {"entity": "audio:",
                        "path": "/clips/sfx~1hit.wav", "equals": 1}},
                    {"tick": 4, "expect": {"entity": "audio:",
                        "path": "/requested", "equals": 1}},
                    {"tick": 4, "expect": {"entity": "audio:",
                        "path": "/clips/sfx~1miss.wav", "equals": 0}},
                    {"tick": 4, "expect": {"entity": "audio:",
                        "path": "/clips/sfx~1miss.wav", "exists": false}},
                ]),
            ),
            Path::new("."),
        );
        assert!(report.passed, "{report:#?}");
    }

    #[cfg(feature = "audio")]
    #[test]
    fn a_sound_cue_caption_shows_for_two_seconds() {
        let mut app = game();
        let mut cue = crate::runtime::SoundCue {
            clip: "sfx/glass.wav".into(),
            caption: "[glass breaks]".into(),
            ..Default::default()
        };
        cue.trigger();
        app.world_mut().spawn(cue);
        let report = run_scenario(
            &mut app,
            &scenario(
                130,
                json!([
                    {"tick": 0, "expect": {"entity": "audio:",
                        "path": "/captions", "equals": []}},
                    {"tick": 60, "expect": {"entity": "audio:",
                        "path": "/captions", "equals": ["[glass breaks]"]}},
                    {"tick": 130, "expect": {"entity": "audio:",
                        "path": "/captions", "equals": []}},
                ]),
            ),
            Path::new("."),
        );
        assert!(report.passed, "{report:#?}");
    }

    #[cfg(feature = "audio")]
    #[test]
    fn the_offline_mix_reports_levels_pan_buses_and_scheduled_starts() {
        use crate::runtime::{AudioQueue, Sound};
        let directory = std::env::temp_dir()
            .join(format!("rusting-mix-{}", Uuid::new_v4()));
        let tone: Vec<f32> = (0..48_000)
            .flat_map(|i| {
                let s = (i as f32 * 440.0 * std::f32::consts::TAU / 48_000.0)
                    .sin()
                    * 0.5;
                [s, s]
            })
            .collect();
        crate::audio_output::write_wav(
            &directory.join("assets/tone.wav"),
            &tone,
        )
        .unwrap();
        let mut app = game();
        app.world_mut()
            .insert_resource(crate::project_runner::ProjectFolder(
                directory.clone(),
            ));
        app.add_system(ScheduleStage::Update, |world: &mut World| {
            let tick = world.resource::<FrameTime>().fixed_tick;
            let mut queue =
                world.get_resource_or_insert_with(AudioQueue::default);
            match tick {
                2 => {
                    let left = Sound {
                        pan: -1.0,
                        bus: "music".into(),
                        ..Sound::default()
                    };
                    queue.play("tone.wav", &left, tick);
                }
                10 => queue.set_bus_volume("music", 0.0, 0.0),
                14 => {
                    let later = Sound {
                        at_tick: Some(20),
                        ..Sound::default()
                    };
                    queue.play("tone.wav", &later, tick);
                }
                _ => {}
            }
        });
        let mut run = scenario(
            20,
            json!([
                {"tick": 1, "expect": {"entity": "audio:", "path": "/level/0",
                    "equals": 0.0}},
                {"tick": 3, "expect": {"entity": "audio:", "path": "/level/0",
                    "greater_than": 0.1}},
                {"tick": 3, "expect": {"entity": "audio:", "path": "/level/1",
                    "less_than": 0.001}},
                {"tick": 3, "expect": {"entity": "audio:", "path": "/peak/0",
                    "greater_than": 0.1}},
                {"tick": 3, "expect": {"entity": "audio:", "path": "/peak/0",
                    "less_than": 1.0}},
                {"tick": 3, "expect": {"entity": "audio:", "path": "/clipped",
                    "equals": 0}},
                {"tick": 3, "expect": {"entity": "audio:",
                    "path": "/playing/0/bus", "equals": "music"}},
                {"tick": 3, "expect": {"entity": "audio:",
                    "path": "/buses/music/level/0", "greater_than": 0.1}},
                {"tick": 3, "expect": {"entity": "audio:",
                    "path": "/buses/music/level/1", "less_than": 0.001}},
                {"tick": 3, "expect": {"entity": "audio:",
                    "path": "/buses/music/peak/0", "greater_than": 0.1}},
                {"tick": 3, "expect": {"entity": "audio:",
                    "path": "/buses//level/0", "greater_than": 0.1}},
                {"tick": 12, "expect": {"entity": "audio:",
                    "path": "/buses/music/level/0", "less_than": 0.001}},
                {"tick": 20, "expect": {"entity": "audio:",
                    "path": "/buses//level/1", "greater_than": 0.1}},
                {"tick": 20, "expect": {"entity": "audio:",
                    "path": "/buses/music/level/0", "less_than": 0.001}},
                {"tick": 12, "expect": {"entity": "audio:", "path": "/level/0",
                    "less_than": 0.001}},
                {"tick": 18, "expect": {"entity": "audio:", "path": "/level/1",
                    "less_than": 0.001}},
                {"tick": 20, "expect": {"entity": "audio:", "path": "/level/1",
                    "greater_than": 0.1}},
            ]),
        );
        run.audio_out = Some("mix.wav".into());
        let report = run_scenario(&mut app, &run, &directory);
        assert!(report.passed, "{report:#?}");
        // 21 ticks of 800 stereo frames, 16-bit, after a 44-byte header.
        let written = std::fs::metadata(directory.join("mix.wav")).unwrap();
        assert_eq!(written.len(), 44 + 21 * 800 * 4);
        std::fs::remove_dir_all(&directory).unwrap();
    }

    #[cfg(feature = "audio")]
    #[test]
    fn mute_and_solo_silence_buses_and_keep_their_volume() {
        use crate::runtime::Sound;
        let directory = tone_project(2.0);
        let mut app = audio_game(&directory, |tick, scene| match tick {
            1 => {
                for (bus, pan) in [("music", -1.0), ("sfx", 1.0)] {
                    let sound = Sound {
                        pan,
                        bus: bus.into(),
                        ..Sound::default()
                    };
                    scene.play_sound_with("tone.wav", sound);
                }
                scene.set_bus_volume("music", 0.5, 0.0);
            }
            10 => scene.solo_bus("sfx", true),
            20 => scene.mute_bus("sfx", true),
            30 => scene.solo_bus("sfx", false),
            40 => scene.mute_bus("sfx", false),
            _ => {}
        });
        let level = |tick: u32, side: u32, loud: bool| {
            let check = if loud { "greater_than" } else { "less_than" };
            let bound = if loud { 0.05 } else { 0.001 };
            json!({"tick": tick, "expect": {"entity": "audio:",
                "path": format!("/level/{side}"), check: bound}})
        };
        let mut steps = vec![
            // Both play; then solo leaves only sfx (right).
            level(5, 0, true),
            level(5, 1, true),
            level(15, 0, false),
            level(15, 1, true),
            // Muting the soloed bus silences everything.
            level(25, 0, false),
            level(25, 1, false),
            // Solo off: music returns at its own volume, sfx stays muted.
            level(35, 0, true),
            level(35, 1, false),
            level(45, 0, true),
            level(45, 1, true),
        ];
        for (path, value) in [("muted", true), ("solo", true)] {
            steps.push(json!({"tick": 25, "expect": {"entity": "audio:",
                "path": format!("/buses/sfx/{path}"), "equals": value}}));
        }
        // Music keeps its 0.5 volume: RMS 0.25, where full volume is 0.5.
        for tick in [5, 35] {
            steps.push(json!({"tick": tick, "expect": {"entity": "audio:",
                "path": "/buses/music/level/0", "equals": 0.25,
                "tolerance": 0.05}}));
        }
        let report =
            run_scenario(&mut app, &scenario(45, json!(steps)), &directory);
        assert!(report.passed, "{report:#?}");
        std::fs::remove_dir_all(&directory).unwrap();
    }

    /// A project folder holding `assets/tone.wav`: a 440 Hz sine at half
    /// scale, `seconds` long.
    #[cfg(feature = "audio")]
    fn tone_project(seconds: f32) -> PathBuf {
        let directory = std::env::temp_dir()
            .join(format!("rusting-audio-{}", Uuid::new_v4()));
        let frames = (seconds * 48_000.0) as usize;
        let tone: Vec<f32> = (0..frames)
            .flat_map(|i| {
                let s = (i as f32 * 440.0 * std::f32::consts::TAU / 48_000.0)
                    .sin()
                    * 0.5;
                [s, s]
            })
            .collect();
        crate::audio_output::write_wav(
            &directory.join("assets/tone.wav"),
            &tone,
        )
        .unwrap();
        directory
    }

    /// [`game`] in `directory`, with `play` run as game code every tick and
    /// an active camera at `[0, 2, 0]` looking down -Z.
    #[cfg(feature = "audio")]
    fn audio_game(
        directory: &Path,
        play: impl Fn(u64, &mut crate::project_runner::GameScene<'_>)
            + Send
            + Sync
            + 'static,
    ) -> App {
        let mut app = game();
        app.world_mut()
            .insert_resource(crate::project_runner::ProjectFolder(
                directory.to_path_buf(),
            ));
        app.world_mut().spawn((
            Name("Eye".into()),
            Transform::new([0.0, 2.0, 0.0]),
            crate::runtime::Camera {
                active: true,
                ..Default::default()
            },
        ));
        app.add_system(ScheduleStage::Update, move |world: &mut World| {
            let tick = world.resource::<FrameTime>().fixed_tick;
            play(tick, &mut crate::project_runner::GameScene { world });
        });
        app
    }

    #[cfg(feature = "audio")]
    #[test]
    fn rate_pause_resume_and_seek_show_in_playing() {
        use crate::runtime::{Sound, SoundId};
        let directory = tone_project(4.0);
        let mut app = audio_game(&directory, |tick, scene| match tick {
            1 => {
                scene.play_sound_with(
                    "tone.wav",
                    Sound {
                        bus: "tape".into(),
                        ..Sound::default()
                    },
                );
                scene.play_sound_with(
                    "tone.wav",
                    Sound {
                        rate: 0.5,
                        ..Sound::default()
                    },
                );
            }
            // Two seconds after the tape started.
            121 => scene.pause_sound(SoundId(0)),
            181 => scene.resume_sound(SoundId(0)),
            200 => scene.seek_sound(SoundId(1), 0.25),
            _ => {}
        });
        let near = |tick: u32, path: &str, value: f64| {
            json!({"tick": tick, "expect": {"entity": "audio:", "path": path,
                "equals": value, "tolerance": 0.05}})
        };
        let report = run_scenario(
            &mut app,
            &scenario(
                241,
                json!([
                    near(2, "/playing/1/rate", 0.5),
                    near(2, "/playing/0/remaining", 4.0),
                    // Half speed: the same clip takes twice as long.
                    near(2, "/playing/1/remaining", 8.0),
                    {"tick": 150, "expect": {"entity": "audio:",
                        "path": "/playing/0/paused", "equals": true}},
                    near(150, "/playing/0/position", 2.0),
                    near(181, "/playing/0/position", 2.0),
                    near(241, "/playing/0/position", 3.0),
                    near(201, "/playing/1/position", 0.25),
                ]),
            ),
            &directory,
        );
        assert!(report.passed, "{report:#?}");
        std::fs::remove_dir_all(&directory).unwrap();
    }

    #[cfg(feature = "audio")]
    #[test]
    fn an_attached_sound_pans_as_its_entity_moves_and_the_listener_can_move() {
        use crate::runtime::Sound;
        let directory = tone_project(2.0);
        let mut app = audio_game(&directory, |tick, scene| match tick {
            1 => {
                scene.play_sound_on("Walker", "tone.wav", Sound::default());
            }
            10 => {
                scene.object("Walker").set_position([5.0, 2.0, 0.0]);
            }
            20 => {
                assert!(scene.set_listener(Some("Booth")));
            }
            _ => {}
        });
        app.world_mut()
            .spawn((Name("Walker".into()), Transform::new([-5.0, 2.0, 0.0])));
        app.world_mut()
            .spawn((Name("Booth".into()), Transform::new([10.0, 2.0, 0.0])));
        let report = run_scenario(
            &mut app,
            &scenario(
                22,
                json!([
                    {"tick": 5, "expect": {"entity": "audio:",
                        "path": "/playing/0/pan", "less_than": -0.9}},
                    {"tick": 5, "expect": {"entity": "audio:",
                        "path": "/playing/0/volume", "equals": 0.4,
                        "tolerance": 0.01}},
                    {"tick": 12, "expect": {"entity": "audio:",
                        "path": "/playing/0/pan", "greater_than": 0.9}},
                    // An array compares its length: one sound plays.
                    {"tick": 12, "expect": {"entity": "audio:",
                        "path": "/playing", "greater_than": 0.0}},
                    {"tick": 12, "expect": {"entity": "audio:",
                        "path": "/playing", "less_than": 2.0}},
                    // The booth is right of the walker.
                    {"tick": 22, "expect": {"entity": "audio:",
                        "path": "/playing/0/pan", "less_than": -0.9}},
                ]),
            ),
            &directory,
        );
        assert!(report.passed, "{report:#?}");
        std::fs::remove_dir_all(&directory).unwrap();
    }

    #[cfg(feature = "audio")]
    #[test]
    fn a_sound_follows_its_own_falloff_curve() {
        use crate::runtime::{Falloff, Sound};
        let directory = tone_project(2.0);
        let mut app = audio_game(&directory, |tick, scene| {
            if tick == 1 {
                for falloff in [
                    Falloff::default(),
                    Falloff::Linear {
                        near: 2.0,
                        far: 10.0,
                    },
                ] {
                    let sound = Sound {
                        position: Some([0.0, 2.0, -6.0]),
                        falloff,
                        ..Sound::default()
                    };
                    scene.play_sound_with("tone.wav", sound);
                }
            }
        });
        let volume = |sound: u32, value: f64| {
            json!({"tick": 5, "expect": {"entity": "audio:",
                "path": format!("/playing/{sound}/volume"), "equals": value,
                "tolerance": 0.001}})
        };
        let steps = json!([volume(0, 1.0 / 3.0), volume(1, 0.5)]);
        let report = run_scenario(&mut app, &scenario(5, steps), &directory);
        assert!(report.passed, "{report:#?}");
        std::fs::remove_dir_all(&directory).unwrap();
    }

    #[cfg(feature = "audio")]
    #[test]
    fn doppler_raises_pitch_on_approach_and_lowers_it_going_away() {
        use crate::runtime::Sound;
        let directory = tone_project(4.0);
        let mut app = audio_game(&directory, |tick, scene| {
            if tick == 1 {
                let siren = Sound {
                    doppler: 1.0,
                    looped: true,
                    ..Sound::default()
                };
                scene.play_sound_on("Car", "tone.wav", siren);
                let plain = Sound {
                    looped: true,
                    ..Sound::default()
                };
                scene.play_sound_on("Car", "tone.wav", plain);
            }
            // 30 m/s along -Z past the camera at the origin.
            let z = -40.0 + 0.5 * tick as f32;
            scene.object("Car").set_position([1.0, 2.0, z]);
        });
        app.world_mut()
            .spawn((Name("Car".into()), Transform::new([1.0, 2.0, -40.0])));
        let rate = |tick: u32, sound: u32, value: f64| {
            json!({"tick": tick, "expect": {"entity": "audio:",
                "path": format!("/playing/{sound}/rate"), "equals": value,
                "tolerance": 0.01}})
        };
        // 343 / (343 - 30) closing, 343 / (343 + 30) going away.
        let report = run_scenario(
            &mut app,
            &scenario(
                130,
                json!([
                    rate(30, 0, 1.096),
                    rate(30, 1, 1.0),
                    rate(130, 0, 0.92),
                    rate(130, 1, 1.0),
                ]),
            ),
            &directory,
        );
        assert!(report.passed, "{report:#?}");
        std::fs::remove_dir_all(&directory).unwrap();
    }

    #[cfg(feature = "audio")]
    #[test]
    fn bus_effects_show_change_the_mix_and_leave_state_hashes_alone() {
        use crate::runtime::{BusEffect, Sound};
        let directory = tone_project(2.0);
        let play = |effects: bool| {
            move |tick: u64,
                  scene: &mut crate::project_runner::GameScene<'_>| {
                if tick == 1 {
                    scene.play_sound_with(
                        "tone.wav",
                        Sound {
                            bus: "world".into(),
                            ..Sound::default()
                        },
                    );
                }
                if effects && tick == 10 {
                    let muffle = BusEffect::LowPass { cutoff_hz: 100.0 };
                    scene.set_bus_effect("world", muffle, 0.0);
                    let room = BusEffect::Reverb {
                        room: 0.9,
                        damping: 0.3,
                        mix: 0.4,
                    };
                    scene.set_bus_effect("world", room, 0.0);
                    let tape = BusEffect::Distortion {
                        drive: 12.0,
                        mix: 0.0,
                    };
                    scene.set_bus_effect("tape", tape, 0.0);
                }
            }
        };
        let steps = json!([
            {"tick": 5, "expect": {"entity": "audio:",
                "path": "/level/0", "greater_than": 0.3}},
            {"tick": 20, "expect": {"entity": "audio:",
                "path": "/buses/world/effects/0/kind", "equals": "Reverb"}},
            {"tick": 20, "expect": {"entity": "audio:",
                "path": "/buses/world/effects/1/cutoff_hz", "equals": 100.0}},
            // A distortion with no mix is off.
            {"tick": 20, "expect": {"entity": "audio:",
                "path": "/buses/tape/effects", "equals": []}},
            {"tick": 20, "expect": {"entity": "audio:",
                "path": "/level/0", "less_than": 0.2}},
        ]);
        let with = run_scenario(
            &mut audio_game(&directory, play(true)),
            &scenario(20, steps),
            &directory,
        );
        assert!(with.passed, "{with:#?}");
        let without = run_scenario(
            &mut audio_game(&directory, play(false)),
            &scenario(20, json!([])),
            &directory,
        );
        assert!(!with.state_hashes.is_empty());
        assert_eq!(with.state_hashes, without.state_hashes);
        std::fs::remove_dir_all(&directory).unwrap();
    }

    #[cfg(feature = "audio")]
    #[test]
    fn a_compressor_turns_a_loud_bus_down() {
        use crate::runtime::{BusEffect, Sound};
        let directory = tone_project(2.0);
        let mut app = audio_game(&directory, |tick, scene| {
            if tick == 1 {
                scene.play_sound_with(
                    "tone.wav",
                    Sound {
                        bus: "world".into(),
                        ..Sound::default()
                    },
                );
            }
            if tick == 10 {
                let limiter = BusEffect::Compressor {
                    threshold_db: -24.0,
                    ratio: 20.0,
                    makeup_db: 0.0,
                };
                scene.set_bus_effect("world", limiter, 0.0);
            }
        });
        let steps = json!([
            {"tick": 5, "expect": {"entity": "audio:",
                "path": "/level/0", "greater_than": 0.3}},
            {"tick": 20, "expect": {"entity": "audio:",
                "path": "/buses/world/effects/0/ratio", "equals": 20.0}},
            {"tick": 20, "expect": {"entity": "audio:",
                "path": "/level/0", "less_than": 0.15}},
        ]);
        let report = run_scenario(&mut app, &scenario(20, steps), &directory);
        assert!(report.passed, "{report:#?}");
        std::fs::remove_dir_all(&directory).unwrap();
    }

    #[cfg(feature = "audio")]
    #[test]
    fn the_eq_cuts_only_the_band_a_tone_sits_in() {
        use crate::runtime::{BusEffect, Sound};
        let directory = tone_project(2.0);
        let mut app = audio_game(&directory, |tick, scene| {
            if tick == 1 {
                for bus in ["treble", "middle"] {
                    let sound = Sound {
                        bus: bus.into(),
                        ..Sound::default()
                    };
                    scene.play_sound_with("tone.wav", sound);
                }
            }
            if tick == 10 {
                let cut = |band: usize| {
                    let mut db = [0.0; 3];
                    db[band] = -24.0;
                    BusEffect::Eq {
                        low_db: db[0],
                        mid_db: db[1],
                        high_db: db[2],
                    }
                };
                // The 440 Hz tone is near the 1 kHz bell, far from 5 kHz.
                scene.set_bus_effect("treble", cut(2), 0.0);
                scene.set_bus_effect("middle", cut(1), 0.0);
            }
        });
        let steps = json!([
            {"tick": 20, "expect": {"entity": "audio:",
                "path": "/buses/middle/effects/0/mid_db", "equals": -24.0}},
            {"tick": 20, "expect": {"entity": "audio:",
                "path": "/buses/treble/level/0", "greater_than": 0.3}},
            {"tick": 20, "expect": {"entity": "audio:",
                "path": "/buses/middle/level/0", "less_than": 0.2}},
        ]);
        let report = run_scenario(&mut app, &scenario(20, steps), &directory);
        assert!(report.passed, "{report:#?}");
        std::fs::remove_dir_all(&directory).unwrap();
    }

    #[cfg(feature = "audio")]
    #[test]
    fn a_thousand_sounds_in_one_tick_stay_within_the_voice_limit() {
        use crate::runtime::Sound;
        let directory = tone_project(1.0);
        let mut app = audio_game(&directory, |tick, scene| {
            if tick != 1 {
                return;
            }
            scene.set_bus_voice_limit("chorus", 32);
            let echo = Sound {
                bus: "chorus".into(),
                volume: 0.01,
                ..Sound::default()
            };
            for _ in 0..1000 {
                scene.play_sound_with("tone.wav", echo.clone());
            }
            // Outranks every echo, so it replaces one.
            let mascot = Sound {
                priority: 255,
                ..echo.clone()
            };
            scene.play_sound_with("tone.wav", mascot);
            // Priority compares within a bus only: a full chorus does not
            // stop a priority 0 sound on another bus.
            let whisper = Sound {
                bus: "voice".into(),
                priority: 0,
                ..echo
            };
            scene.play_sound_with("tone.wav", whisper);
        });
        let report = run_scenario(
            &mut app,
            &scenario(
                3,
                json!([
                    {"tick": 2, "expect": {"entity": "audio:",
                        "path": "/buses/chorus/voices", "equals": 32}},
                    {"tick": 2, "expect": {"entity": "audio:",
                        "path": "/buses/chorus/dropped", "equals": 968}},
                    {"tick": 2, "expect": {"entity": "audio:",
                        "path": "/buses/chorus/stolen", "equals": 1}},
                    {"tick": 2, "expect": {"entity": "audio:",
                        "path": "/dropped", "equals": 969}},
                    {"tick": 2, "expect": {"entity": "audio:",
                        "path": "/buses/voice/voices", "equals": 1}},
                    {"tick": 2, "expect": {"entity": "audio:",
                        "path": "/requested", "equals": 1002}},
                ]),
            ),
            &directory,
        );
        assert!(report.passed, "{report:#?}");
        std::fs::remove_dir_all(&directory).unwrap();
    }

    #[cfg(feature = "audio")]
    #[test]
    fn captions_show_while_their_sound_plays() {
        use crate::runtime::{Caption, Sound};
        let directory = tone_project(1.0);
        let mut app = audio_game(&directory, |tick, scene| {
            if tick == 1 {
                scene.play_sound_with(
                    "tone.wav",
                    Sound {
                        captions: vec![
                            Caption::new(0.0, 0.5, "one bear"),
                            Caption::new(0.5, 1.0, "two bears"),
                        ],
                        ..Sound::default()
                    },
                );
            }
        });
        let report = run_scenario(
            &mut app,
            &scenario(
                70,
                json!([
                    {"tick": 0, "expect": {"entity": "audio:",
                        "path": "/captions", "equals": []}},
                    {"tick": 10, "expect": {"entity": "audio:",
                        "path": "/captions", "equals": ["one bear"]}},
                    {"tick": 45, "expect": {"entity": "audio:",
                        "path": "/captions", "equals": ["two bears"]}},
                    {"tick": 45, "expect": {"entity": "audio:",
                        "path": "/playing/0/clip", "equals": "tone.wav"}},
                    {"tick": 70, "expect": {"entity": "audio:",
                        "path": "/captions", "equals": []}},
                    {"tick": 70, "expect": {"entity": "audio:",
                        "path": "/playing", "equals": []}},
                ]),
            ),
            &directory,
        );
        assert!(report.passed, "{report:#?}");
        std::fs::remove_dir_all(&directory).unwrap();
    }

    #[cfg(feature = "audio")]
    #[test]
    fn a_paused_sound_hides_its_caption_until_resumed() {
        use crate::runtime::{Caption, Sound, SoundId};
        let directory = tone_project(1.0);
        let mut app = audio_game(&directory, |tick, scene| match tick {
            1 => {
                scene.play_sound_with(
                    "tone.wav",
                    Sound {
                        captions: vec![Caption::new(0.0, 0.9, "one bear")],
                        ..Sound::default()
                    },
                );
            }
            20 => scene.pause_sound(SoundId(0)),
            40 => scene.resume_sound(SoundId(0)),
            _ => {}
        });
        let report = run_scenario(
            &mut app,
            &scenario(
                45,
                json!([
                    {"tick": 10, "expect": {"entity": "audio:",
                        "path": "/captions", "equals": ["one bear"]}},
                    {"tick": 30, "expect": {"entity": "audio:",
                        "path": "/captions", "equals": []}},
                    {"tick": 45, "expect": {"entity": "audio:",
                        "path": "/captions", "equals": ["one bear"]}},
                ]),
            ),
            &directory,
        );
        assert!(report.passed, "{report:#?}");
        std::fs::remove_dir_all(&directory).unwrap();
    }

    #[cfg(feature = "audio")]
    #[test]
    fn a_reverb_zone_adds_reverb_while_the_listener_is_inside() {
        let directory = tone_project(1.0);
        let mut app = audio_game(&directory, |tick, scene| {
            if tick == 10 {
                scene.object("Eye").set_position([20.0, 2.0, 0.0]);
            }
        });
        app.world_mut().spawn((
            SceneId(Uuid::new_v4()),
            Name("Cave".into()),
            Transform::new([0.0, 2.0, 0.0]),
            PhysicsBody::default(),
            RigidBody {
                kind: RigidBodyKind::Fixed,
                ..RigidBody::default()
            },
            Collider {
                shape: ColliderShape::Box {
                    half_extents: [3.0, 3.0, 3.0],
                },
                sensor: true,
                ..Collider::default()
            },
            crate::runtime::ReverbZone {
                bus: "world".into(),
                ..Default::default()
            },
        ));
        let steps = json!([
            {"tick": 5, "expect": {"entity": "audio:",
                "path": "/buses/world/effects/0/kind", "equals": "Reverb"}},
            {"tick": 14, "expect": {"entity": "audio:",
                "path": "/buses/world/effects", "equals": []}},
        ]);
        let report = run_scenario(&mut app, &scenario(14, steps), &directory);
        assert!(report.passed, "{report:#?}");
        std::fs::remove_dir_all(&directory).unwrap();
    }

    #[cfg(feature = "audio")]
    #[test]
    fn a_wall_between_listener_and_sound_lowers_its_volume() {
        use crate::runtime::{Sound, SoundId};
        let directory = tone_project(2.0);
        let mut app = audio_game(&directory, |tick, scene| match tick {
            1 => {
                scene.play_sound_with(
                    "tone.wav",
                    Sound {
                        position: Some([0.0, 2.0, -5.0]),
                        occlude: true,
                        ..Sound::default()
                    },
                );
            }
            10 => scene.set_sound_position(SoundId(0), [5.0, 2.0, 0.0]),
            _ => {}
        });
        app.world_mut().spawn((
            SceneId(Uuid::new_v4()),
            Name("Shelf".into()),
            Transform::new([2.5, 2.0, 0.0]),
            PhysicsBody::default(),
            RigidBody {
                kind: RigidBodyKind::Fixed,
                ..RigidBody::default()
            },
            Collider {
                shape: ColliderShape::Box {
                    half_extents: [0.2, 2.0, 2.0],
                },
                ..Collider::default()
            },
        ));
        let report = run_scenario(
            &mut app,
            &scenario(
                14,
                json!([
                    {"tick": 5, "expect": {"entity": "audio:",
                        "path": "/playing/0/volume", "equals": 0.4,
                        "tolerance": 0.01}},
                    {"tick": 14, "expect": {"entity": "audio:",
                        "path": "/playing/0/occlusion", "equals": 1.0}},
                    {"tick": 14, "expect": {"entity": "audio:",
                        "path": "/playing/0/volume", "equals": 0.12,
                        "tolerance": 0.01}},
                ]),
            ),
            &directory,
        );
        assert!(report.passed, "{report:#?}");
        std::fs::remove_dir_all(&directory).unwrap();
    }

    #[cfg(feature = "audio")]
    #[test]
    fn the_mix_matches_its_stored_reference_within_tolerance() {
        use crate::runtime::Sound;
        let directory = tone_project(1.0);
        let run = |volume: f32, update: bool| {
            let mut app = audio_game(&directory, move |tick, scene| {
                if tick == 1 {
                    let tone = Sound {
                        volume,
                        pan: -0.5,
                        ..Sound::default()
                    };
                    scene.play_sound_with("tone.wav", tone);
                }
            });
            let mut run = scenario(30, json!([]));
            run.update_golden = update;
            run.audio_reference = Some(AudioReference {
                path: "reference.wav".into(),
                tolerance: 0.01,
            });
            run_scenario(&mut app, &run, &directory)
        };
        let missing = run(1.0, false);
        assert!(!missing.passed);
        assert!(run(1.0, true).passed);
        let same = run(1.0, false);
        assert!(same.passed, "{same:#?}");
        let quieter = run(0.8, false);
        assert!(!quieter.passed);
        assert!(
            quieter
                .steps
                .last()
                .unwrap()
                .message
                .contains("RMS difference"),
            "{quieter:#?}"
        );
        std::fs::remove_dir_all(&directory).unwrap();
    }

    fn scenario_from(step: Value) -> Scenario {
        scenario(2, json!([step]))
    }

    #[test]
    fn inputs_state_and_events_pass_at_fixed_ticks() {
        let report = run(&scenario(
            90,
            json!([
                {"tick": 0, "expect": {"entity": "Cube",
                    "path": "/transform/position/1", "equals": 3.0}},
                {"tick": 10, "expect": {"entity": "Cube",
                    "path": "/transform/position/1", "less_than": 3.0}},
                {"tick": 90, "expect_events": {"kind": "collision",
                    "entity": "Cube", "at_least": 1}},
                {"tick": 91, "press": "jump"},
            ]),
        ));
        // Tick 91 is past the end, so validation rejects it.
        assert!(!report.passed);
        assert!(report.first_failure.unwrap().message.contains("outside"));

        let report = run(&scenario(
            90,
            json!([
                {"tick": 0, "expect": {"entity": "Cube",
                    "path": "/transform/position/1", "equals": 3.0}},
                {"tick": 10, "expect": {"entity": "Cube",
                    "path": "/transform/position/1", "less_than": 3.0}},
                {"tick": 90, "expect_events": {"kind": "collision",
                    "entity": "Cube", "at_least": 1}},
                {"tick": 5, "press": "jump"},
                {"tick": 5, "expect": {"entity": "Ground",
                    "path": "/transform/position/1", "equals": 1.0}},
                {"tick": 6, "release": "jump"},
                {"tick": 6, "expect": {"entity": "Ground",
                    "path": "/transform/position/1", "equals": -0.5}},
            ]),
        ));
        assert!(report.passed, "{report:#?}");
        assert_eq!(report.ticks_run, 90);

        // A tap is a press that releases itself before the next tick.
        let report = run(&scenario(
            10,
            json!([
                {"tick": 5, "tap": "jump"},
                {"tick": 5, "expect": {"entity": "Ground",
                    "path": "/transform/position/1", "equals": 1.0}},
                {"tick": 6, "expect": {"entity": "Ground",
                    "path": "/transform/position/1", "equals": -0.5}},
            ]),
        ));
        assert!(report.passed, "{report:#?}");
    }

    #[test]
    fn pointer_steps_place_the_cursor_in_the_capture_sized_view() {
        let mut app = game();
        let mut run = scenario(3, json!([{"tick": 2, "pointer": [0.25, 0.5]}]));
        run.capture_size = [800, 600];
        assert!(run_scenario(&mut app, &run, Path::new(".")).passed);
        let input = app.world().resource::<RuntimeInput>();
        assert_eq!(input.viewport_size(), [800.0, 600.0]);
        assert_eq!(input.cursor_position(), Some([200.0, 300.0]));
        let step: ScenarioStep =
            serde_json::from_value(json!({"tick": 3, "pointer": [0.5, 0.5]}))
                .unwrap();
        assert!(matches!(step.action, StepAction::Pointer([0.5, 0.5])));
    }

    #[cfg(feature = "ui")]
    #[test]
    fn click_steps_press_egui_buttons_by_label_and_quit_ends_the_run() {
        let mut app = game();
        app.add_system(ScheduleStage::Update, |world: &mut World| {
            let context = world
                .resource::<crate::runtime::RuntimeUi>()
                .context()
                .clone();
            egui::CentralPanel::default().show(&context, |ui| {
                if ui.button("Settings").clicked() {
                    world.spawn((
                        Name("opened".into()),
                        SceneId(Uuid::new_v4()),
                    ));
                }
                if ui.button("Quit").clicked() {
                    world
                        .resource_mut::<crate::runtime::ExitState>()
                        .requested = true;
                }
            });
        });
        let report = run_scenario(
            &mut app,
            &scenario(
                30,
                json!([
                    {"tick": 1, "click": "Settings"},
                    {"tick": 3, "expect": {"entity": "opened", "path": "/name", "equals": "opened"}},
                    {"tick": 3, "expect_quit": false},
                    {"tick": 4, "click": "Quit"},
                    {"tick": 5, "expect_quit": true},
                    {"tick": 6, "expect_file": {"path": "no-such-save.cfg", "exists": false}},
                    {"tick": 20, "expect_quit": true},
                ]),
            ),
            Path::new("."),
        );
        let failure =
            report.first_failure.as_ref().expect("tick 20 never runs");
        assert_eq!(failure.tick, 20, "{report:#?}");
        assert!(failure.message.contains("quit at tick 5"), "{failure:?}");
        assert!(report.steps.iter().filter(|step| step.ok).count() >= 5);
        assert!(
            report.steps.iter().any(|step| step.tick == 6 && step.ok),
            "expect_file runs after the quit: {report:#?}"
        );

        let missing = run(&scenario(3, json!([{"tick": 1, "click": "Load"}])));
        let message = &missing.first_failure.unwrap().message;
        assert!(message.contains("no UI text `Load`"), "{message}");
    }

    #[cfg(feature = "ui")]
    #[test]
    fn hud_text_shows_the_counters_game_code_set_on_the_same_tick() {
        let mut app = game();
        app.world_mut().spawn((
            crate::runtime::Counter {
                name: "beat".into(),
                ..Default::default()
            },
            crate::runtime::HudElement {
                text: "BEAT {beat}".into(),
                ..Default::default()
            },
        ));
        app.add_system(ScheduleStage::Update, |world: &mut World| {
            let tick = world.resource::<FrameTime>().fixed_tick as i32;
            let mut counters = world.query::<&mut crate::runtime::Counter>();
            for mut counter in counters.iter_mut(world) {
                counter.value = tick;
            }
        });
        // A click finds the text the previous tick drew.
        let report = run_scenario(
            &mut app,
            &scenario(6, json!([{"tick": 6, "click": "BEAT 5"}])),
            Path::new("."),
        );
        assert!(report.passed, "{report:#?}");
    }

    #[test]
    fn a_missing_path_suggests_the_nearest_real_ones() {
        let state = json!({"components": {"rusting.fog": {"density": 0.1, "color": [0, 0, 0]}},
            "mesh_renderer": {"material": {"Inline": {"transmission": 0.5}}}});
        let hint = near_paths(&state, "/scene/fog/density");
        assert!(
            hint.starts_with("; did you mean /components/rusting.fog/density"),
            "{hint}"
        );
        let hint = near_paths(&state, "/mesh_renderer/material/transmission");
        assert!(
            hint.contains("/mesh_renderer/material/Inline/transmission"),
            "{hint}"
        );
    }

    #[test]
    fn hashes_are_kept_past_the_world_history() {
        let mut world = World::new();
        world.insert_resource(crate::runtime::StateHashes::default());
        let mut report = ScenarioReport {
            name: String::new(),
            seed: 0,
            passed: false,
            ticks_run: 0,
            first_failure: None,
            steps: Vec::new(),
            captures: Vec::new(),
            trace: Vec::new(),
            perf: PerfReport::default(),
            gpu_state_hashes: Vec::new(),
            state_hashes: Vec::new(),
            fuzz_steps: Vec::new(),
            coverage: Value::Null,
            explore: Value::Null,
        };
        for tick in 0..3000_u64 {
            let mut hashes =
                world.resource_mut::<crate::runtime::StateHashes>();
            hashes.recent.push_back((tick, tick * 7));
            if hashes.recent.len() > 1024 {
                hashes.recent.pop_front();
            }
            collect_hashes(&world, &mut report);
            collect_hashes(&world, &mut report);
        }
        assert_eq!(report.state_hashes.len(), 3000);
        assert_eq!(report.state_hashes[2999], (2999, 2999 * 7));
    }

    #[test]
    fn gpu_bodies_report_the_gpu_pose_under_gpu_state() {
        let mut app = game();
        let world = app.world_mut();
        let ground = find_entity(world, "Ground", |_, _| true).unwrap();
        world
            .entity_mut(ground)
            .insert(crate::runtime::GpuStateMirror {
                tick: 4,
                transform: Transform::new([1.0, 2.0, 3.0]),
                linear_velocity: [0.0; 3],
                angular_velocity: [0.0; 3],
                custom_values: None,
            });
        let state = reflected(world, "Ground").unwrap();
        assert_eq!(state["gpu_state"]["position"], json!([1.0, 2.0, 3.0]));
        assert_eq!(state["gpu_state"]["tick"], 4);
    }

    #[test]
    fn class_entities_count_members_and_gpu_bodies_in_a_box() {
        use crate::runtime::{GpuStateMirror, ObjectClasses};
        let mut world = World::new();
        for x in [0.0, 1.0, 4.0] {
            world.spawn((
                ObjectClasses::new(["ball"]),
                GpuStateMirror {
                    tick: 2,
                    transform: Transform::new([x, 0.5, -x]),
                    linear_velocity: [0.0; 3],
                    angular_velocity: [0.0; 3],
                    custom_values: None,
                },
            ));
        }
        world.spawn(ObjectClasses::new(["ball"]));
        world.spawn(ObjectClasses::new(["crate"]));
        let state =
            reflected(&mut world, "class:ball in -1,0,-2 2,1,1").unwrap();
        assert_eq!(state["count"], 4);
        assert_eq!(state["gpu"]["count"], 3);
        assert_eq!(state["gpu"]["min"], json!([0.0, 0.5, -4.0]));
        assert_eq!(state["gpu"]["max"], json!([4.0, 0.5, 0.0]));
        assert_eq!(state["gpu"]["inside"], 2);
        let crates = reflected(&mut world, "class:crate").unwrap();
        assert_eq!(
            crates,
            json!({"count": 1, "gpu": {"count": 0, "min": null, "max": null}})
        );
        assert!(reflected(&mut world, "class:ball in 1,2 3").is_err());
    }

    #[test]
    fn a_failed_check_names_inputs_that_ran_on_its_tick() {
        let report = run(&scenario(
            5,
            json!([
                {"tick": 2, "press": "jump"},
                {"tick": 2, "expect": {"entity": "Ground",
                    "path": "/transform/position/1", "equals": -0.5}},
            ]),
        ));
        let message = report.first_failure.unwrap().message;
        assert!(
            message.contains(
                r#"inputs at this tick ran before the check: {"press":"jump"}"#
            ),
            "{message}"
        );
    }

    #[test]
    fn failure_trace_names_the_first_failing_tick() {
        let report = run(&scenario(
            60,
            json!([
                {"tick": 0, "until": 60, "expect": {"entity": "Cube",
                    "path": "/transform/position/1", "greater_than": 2.5}},
            ]),
        ));
        assert!(!report.passed);
        let failure = report.first_failure.unwrap();
        assert!(failure.tick > 5 && failure.tick < 60, "{failure:?}");
        assert!(failure.actual.as_f64().unwrap() <= 2.5);
        // The run stops at the failure and reports only that step.
        assert_eq!(report.steps.len(), 1);
        assert_eq!(report.ticks_run, u64::from(failure.tick));

        let unbound = run(&scenario(1, json!([{"tick": 1, "press": "fly"}])));
        assert!(unbound.first_failure.unwrap().message.contains("fly"));
    }

    #[test]
    fn within_passes_on_the_first_tick_the_check_holds() {
        let falls = |within: u32| {
            run(&scenario(
                60,
                json!([{"tick": 0, "within": within, "expect": {"entity": "Cube",
                    "path": "/transform/position/1", "less_than": 2.5}}]),
            ))
        };
        let report = falls(60);
        assert!(report.passed, "{report:#?}");
        let pass = &report.steps[0];
        assert!(pass.tick > 5 && pass.tick < 60, "{pass:?}");

        let report = falls(3);
        assert!(!report.passed);
        assert_eq!(report.first_failure.unwrap().tick, 3);
    }

    #[test]
    fn log_steps_record_a_value_every_tick_and_never_fail() {
        let report = run(&scenario(
            10,
            json!([
                {"tick": 1, "until": 4, "log": {"entity": "Cube",
                    "path": "/transform/position/1"}},
                {"tick": 5, "log": {"entity": "Ghost", "path": ""}},
            ]),
        ));
        assert!(report.passed, "{report:#?}");
        let ticks: Vec<_> = report.steps.iter().map(|step| step.tick).collect();
        assert_eq!(ticks, [1, 2, 3, 4, 5]);
        assert!(report.steps[3].actual.as_f64().unwrap() < 3.0, "falling");
        assert_eq!(report.steps[4].actual, Value::Null);
        assert!(report.steps[4].message.contains("Ghost"), "{report:#?}");
        assert!(!report.steps[4].message.contains(" is null"));
    }

    #[test]
    fn exists_checks_entities_and_paths() {
        let check = |entity: &str, path: &str, exists: bool| {
            run(&scenario(
                1,
                json!([{"tick": 1, "expect": {"entity": entity, "path": path, "exists": exists}}]),
            ))
            .passed
        };
        assert!(check("Cube", "", true));
        assert!(check("Cube", "/transform", true));
        assert!(check("Ghost", "", false));
        assert!(check("Cube", "/components/nope", false));
        assert!(!check("Cube", "", false));
        assert!(!check("Ghost", "", true));
    }

    #[test]
    fn seed_reaches_game_code_and_repeats() {
        let ground_x = |seed: u64| {
            let mut scenario = scenario(3, json!([]));
            scenario.seed = seed;
            let mut app = game();
            assert!(run_scenario(&mut app, &scenario, Path::new(".")).passed);
            let mut query = app.world_mut().query::<(&Name, &Transform)>();
            query
                .iter(app.world())
                .find(|(name, _)| name.0 == "Ground")
                .unwrap()
                .1
                .position[0]
        };
        assert_eq!(ground_x(1), ground_x(1));
        assert_ne!(ground_x(1), ground_x(2));
    }

    #[test]
    fn five_hundred_of_five_thousand_bears_move_in_one_tick() {
        use crate::runtime::{
            render_benchmark_bear, spawn_render_benchmark_extras,
            RenderBenchmarkExtras,
        };
        let mut app = App::new();
        app.add_plugin(crate::AssetPlugin).unwrap();
        app.add_plugin(RenderExtractPlugin).unwrap();
        let bears = spawn_render_benchmark_extras(
            app.world_mut(),
            RenderBenchmarkExtras {
                bears: 5000,
                ..Default::default()
            },
        );
        // Scenario checks find entities by persistent ID.
        for &bear in &bears {
            app.world_mut()
                .entity_mut(bear)
                .insert(SceneId(Uuid::new_v4()));
        }
        app.add_system(ScheduleStage::Update, move |world: &mut World| {
            if world.resource::<FrameTime>().fixed_tick != 2 {
                return;
            }
            for &bear in &bears[..500] {
                world.get_mut::<Transform>(bear).unwrap().position[1] = 5.0;
            }
        });
        let bear = |index| render_benchmark_bear(index);
        let report = run_scenario(
            &mut app,
            &scenario(
                3,
                json!([
                    {"tick": 1, "expect": {"entity": bear(0),
                        "path": "/transform/position/1", "less_than": 1.0}},
                    {"tick": 3, "expect": {"entity": bear(0),
                        "path": "/transform/position/1", "equals": 5.0}},
                    {"tick": 3, "expect": {"entity": bear(499),
                        "path": "/transform/position/1", "equals": 5.0}},
                    {"tick": 3, "expect": {"entity": bear(500),
                        "path": "/transform/position/1", "less_than": 1.0}},
                ]),
            ),
            Path::new("."),
        );
        assert!(report.passed, "{report:#?}");
        // The renderer sees exactly the moved instances.
        let render = app.world().resource::<RenderWorld>();
        let raised = render
            .renderables
            .iter()
            .filter(|r| r.transform.matrix[3][1] > 4.0)
            .count();
        assert_eq!((render.renderables.len(), raised), (5000, 500));
    }
}
