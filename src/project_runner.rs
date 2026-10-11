//! Windowed runtime runner for native Rust game projects.

use std::collections::{HashMap, HashSet};
use std::error::Error;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Instant;

use bevy_ecs::entity::Entity;
use bevy_ecs::prelude::{Mut, Resource, World};
use vulkano::format::Format;
use vulkano::image::ImageUsage;
use vulkano::swapchain::Surface;
use vulkano::VulkanError;
use vulkano_util::context::VulkanoContext;
use vulkano_util::window::{VulkanoWindows, WindowDescriptor};
use winit::application::ApplicationHandler;
use winit::dpi::PhysicalSize;
use winit::event::{DeviceEvent, DeviceId, WindowEvent};
use winit::event_loop::{ActiveEventLoop, EventLoop};
use winit::keyboard::PhysicalKey;
use winit::window::{CursorGrabMode, Fullscreen, Window, WindowId};

use crate::rendering::frame_pacer::{select_present_mode, FramePacer};
use crate::rendering::render_scale::ScaledTarget;
use crate::rendering::scene_renderer::SceneRenderer;
use crate::runtime::{
    apply_gpu_state_samples, load_scene, load_scene_document,
    record_gpu_state_hashes, route_gpu_physics_events, scene_document,
    write_atomic, AppError, EventQueue, FrameTime, GpuEventRegistry,
    GpuPhysicsClassWatches, GpuPhysicsEvent, GpuPhysicsEventsLost,
    GpuPhysicsRule, GpuPhysicsWatch, HybridPhysicsPlugin, MeshRenderer, Name,
    PhysicsBackendStatus, Plugin, RenderExtractPlugin, RenderSettings,
    RenderWorld, RuntimeInput, SceneDocument, SceneLoadMode, ScheduleStage,
};
use crate::{App, AssetPlugin, AssetServer, Transform};

/// Result returned by the convenient native game entry point.
pub type GameResult<T = ()> = Result<T, Box<dyn Error>>;

/// Options applied when one class becomes GPU simulated.
#[derive(Clone, Debug)]
pub struct GpuBodySettings {
    /// Compute solver selected for every matching object.
    pub solver: crate::runtime::PhysicsSolver,
    /// Project-relative shader used when `solver` is Custom.
    pub custom_shader: Option<String>,
    /// Mass, velocity, and gravity values copied into GPU body state.
    pub rigid_body: crate::runtime::RigidBody,
    /// Collision shape and surface values used by collision solvers.
    /// `friction` decides whether a dense pile flows: about 0.05 for
    /// plastic balls; 0.3 can jam a packed tube.
    pub collider: crate::runtime::Collider,
    /// Collision groups used when collision solvers are connected.
    pub collision_layers: crate::runtime::CollisionLayers,
    /// What the GPU sends back. `SelectedState` fills
    /// [`GameScene::gpu_state`] every physics frame; the default sends only
    /// events.
    pub sync: crate::runtime::PhysicsSyncMode,
}

impl Default for GpuBodySettings {
    fn default() -> Self {
        Self {
            solver: crate::runtime::PhysicsSolver::Full,
            custom_shader: None,
            rigid_body: crate::runtime::RigidBody::default(),
            collider: crate::runtime::Collider::default(),
            collision_layers: crate::runtime::CollisionLayers::default(),
            sync: crate::runtime::PhysicsSyncMode::default(),
        }
    }
}

/// Reusable settings for cubes created by native Rust game code.
#[derive(Clone, Debug, Default)]
pub struct CubeSpawn {
    /// Classes assigned to every cube created with this template.
    pub classes: crate::runtime::ObjectClasses,
}

impl CubeSpawn {
    /// Creates an empty cube template.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds one class to every cube created with this template.
    #[must_use]
    pub fn class(mut self, class: impl Into<String>) -> Self {
        self.classes.add(class);
        self
    }
}

/// Reusable settings for spheres created by native Rust game code.
#[derive(Clone, Debug, Default)]
pub struct SphereSpawn {
    /// Classes assigned to every sphere created with this template.
    pub classes: crate::runtime::ObjectClasses,
    /// Number of vertical sphere subdivisions used by the shared mesh.
    pub subdivisions: u32,
}

impl SphereSpawn {
    /// Creates a sphere template with moderate mesh quality: 16
    /// subdivisions, 1,024 triangles per sphere.
    #[must_use]
    pub fn new() -> Self {
        Self {
            subdivisions: 16,
            ..Self::default()
        }
    }

    /// Changes the sphere mesh quality. Meshes are cached by this value.
    /// A sphere has `4 * value * value` triangles (3: 36, 6: 144, 16:
    /// 1,024); for thousands of small spheres 3 to 6 is plenty.
    #[must_use]
    pub fn subdivisions(mut self, value: u32) -> Self {
        self.subdivisions = value.clamp(2, 128);
        self
    }

    /// Adds one class to every sphere created with this template.
    #[must_use]
    pub fn class(mut self, class: impl Into<String>) -> Self {
        self.classes.add(class);
        self
    }
}

/// Cached procedural sphere meshes shared by all native-spawned spheres.
#[derive(Resource, Clone, Default)]
struct SphereMeshCache(
    HashMap<u32, crate::assets::Handle<crate::assets::MeshAsset>>,
);

/// The scene as it was loaded when the game started, for
/// [`GameScene::restart`].
#[derive(Resource)]
struct StartingScene(SceneDocument);

/// The folder holding the game's `project.json`, which
/// [`GameScene::load_scene`] resolves scene paths against.
#[derive(Resource)]
pub(crate) struct ProjectFolder(pub(crate) PathBuf);

/// A copy of the scene taken by [`GameScene::snapshot`], for
/// [`GameScene::restore`].
#[derive(Clone)]
pub struct GameSnapshot {
    scene: SceneDocument,
    once: GameOnceState,
    physics: Option<crate::runtime::PhysicsWorld>,
    next_order: Option<crate::runtime::NextSpawnOrder>,
    fluids: Vec<(uuid::Uuid, crate::runtime::Fluid)>,
    /// Each scene object's id, entity, solve order and sleep state.
    objects:
        Vec<(uuid::Uuid, Entity, Option<crate::runtime::SpawnOrder>, bool)>,
}

/// [`GameScene::restart`], for scenario `restart` steps.
pub(crate) fn restart_scene(world: &mut World) -> bool {
    let Some(start) = world.remove_resource::<StartingScene>() else {
        return false;
    };
    let loaded = load_scene_document(world, &start.0, SceneLoadMode::Replace);
    world.insert_resource(start);
    if let Err(error) = loaded {
        eprintln!("restart: the starting scene no longer loads: {error}");
        return false;
    }
    forget_old_scene(world);
    // Every GPU body starts again from the scene file, with rule edge and
    // cooldown state cleared, instead of whatever survived the reload.
    if let Some(mut commands) =
        world.get_resource_mut::<crate::runtime::GpuPhysicsCommands>()
    {
        commands.reset_to_authored = true;
    }
    true
}

/// `key` inside the user data folder, refusing paths that leave it.
fn user_data_path(key: &str) -> std::io::Result<PathBuf> {
    let relative = Path::new(key);
    let inside = relative
        .components()
        .all(|part| matches!(part, std::path::Component::Normal(_)));
    if !inside || key.is_empty() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("data key `{key}` must be a relative path without `..`"),
        ));
    }
    Ok(crate::project::user_data_folder().join(relative))
}

/// `<folder>/<name>` for each file directly in `path`, sorted.
fn keys_in(path: &Path, folder: &str) -> Vec<String> {
    let mut keys: Vec<String> = std::fs::read_dir(path)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_file()))
        .filter_map(|entry| {
            Some(format!("{folder}/{}", entry.file_name().to_str()?))
        })
        .collect();
    keys.sort();
    keys
}

/// Keys already executed through [`GameScene::once`].
#[derive(Resource, Clone, Default)]
struct GameOnceState(HashSet<String>);

/// Window changes game code asked for. The window runner applies them.
#[derive(Resource, Default)]
struct WindowRequest {
    fullscreen: bool,
    size: Option<[u32; 2]>,
    title: Option<String>,
}

/// Convenient access to objects in the loaded scene.
///
/// This is a small API over the ECS, not another scripting language. Advanced
/// systems can still query the ECS world directly.
pub struct GameScene<'world> {
    /// ECS world that owns all scene objects and components.
    pub(crate) world: &'world mut World,
}

impl GameScene<'_> {
    /// Runs setup code once per round: [`Self::restart`] runs it again.
    ///
    /// This keeps large procedural scene creation out of the per-frame path
    /// while preserving the short `rusting_game!` API.
    pub fn once(
        &mut self,
        key: impl Into<String>,
        action: impl FnOnce(&mut GameScene<'_>),
    ) {
        let key = key.into();
        let should_run = self
            .world
            .get_resource_or_insert_with(GameOnceState::default)
            .0
            .insert(key);
        if should_run {
            action(self);
        }
    }

    /// Reads a text file (a JSON level, a dialogue script) relative to the
    /// project's `assets` folder, as sounds are: `"charts/easy.json"`. It
    /// works from any working directory. `rusting check` reports a literal
    /// path here that is not a file.
    ///
    /// # Errors
    /// Returns the error from reading the file.
    pub fn load_text(&self, path: &str) -> std::io::Result<String> {
        std::fs::read_to_string(self.assets_folder().join(path))
    }

    /// Switches to the translations in `assets/locales/<locale>.json`, a
    /// flat JSON object of key to text, so [`Self::tr`] and HUD
    /// `{tr:key}` placeholders read them from the next frame. Warns and
    /// returns `false`, keeping the current locale, when the file is
    /// missing or not such an object.
    pub fn set_locale(&mut self, locale: &str) -> bool {
        let path = format!("locales/{locale}.json");
        let strings = self.load_text(&path).ok().and_then(|text| {
            serde_json::from_str::<std::collections::BTreeMap<String, String>>(
                &text,
            )
            .ok()
        });
        let Some(strings) = strings else {
            eprintln!("warning: `assets/{path}` is not a translation table");
            return false;
        };
        self.world.insert_resource(crate::runtime::Translations {
            locale: locale.to_owned(),
            strings,
        });
        true
    }

    /// The locale [`Self::set_locale`] last switched to, or `""`.
    #[must_use]
    pub fn locale(&self) -> String {
        self.translations().locale.clone()
    }

    /// The current locale's text for `key`, or `key` itself when it has
    /// none.
    #[must_use]
    pub fn tr(&self, key: &str) -> String {
        self.translations().get(key).to_owned()
    }

    /// The current locale's `key.<form>` for `count`, with `{count}`
    /// replaced. The form follows the locale's plural rule: `one` or
    /// `other` in English, `one`, `few` or `many` in Russian or Polish
    /// (`ru`, `pl`), `one` for 0 and 1 in French. Falls back to
    /// `key.other`, then `key`.
    #[must_use]
    pub fn tr_count(&self, key: &str, count: i64) -> String {
        self.translations().plural(key, count)
    }

    fn translations(
        &self,
    ) -> std::borrow::Cow<'_, crate::runtime::Translations> {
        self.world
            .get_resource::<crate::runtime::Translations>()
            .map_or_else(Default::default, std::borrow::Cow::Borrowed)
    }

    fn assets_folder(&self) -> PathBuf {
        self.world
            .get_resource::<ProjectFolder>()
            .map(|folder| folder.0.join("assets"))
            .unwrap_or_else(|| PathBuf::from("assets"))
    }

    /// Plays a clip once. `clip` is relative to the project's `assets`
    /// folder (`"sfx/hit.wav"`); WAV, Ogg, MP3 and FLAC load. Returns an ID
    /// for [`Self::stop_sound`]. Headless runs have no audio device: the
    /// request is counted by [`Self::sounds_requested`] and dropped.
    pub fn play_sound(
        &mut self,
        clip: &str,
        volume: f32,
    ) -> crate::runtime::SoundId {
        self.play_sound_with(
            clip,
            crate::runtime::Sound {
                volume,
                ..Default::default()
            },
        )
    }

    /// Plays a clip again and again until [`Self::stop_sound`].
    pub fn play_sound_looped(
        &mut self,
        clip: &str,
        volume: f32,
    ) -> crate::runtime::SoundId {
        self.play_sound_with(
            clip,
            crate::runtime::Sound {
                volume,
                looped: true,
                ..Default::default()
            },
        )
    }

    /// Plays a clip with a pan, a bus, a start tick or a world position:
    ///
    /// ```ignore
    /// scene.play_sound_with("music/bar.ogg", Sound {
    ///     bus: "music".into(),
    ///     at_tick: Some(next_bar_tick),
    ///     ..Sound::default()
    /// });
    /// ```
    ///
    /// Every sound starts a fixed delay after the fixed tick it belongs to,
    /// whatever the frame rate, so sounds keep in time with fixed ticks.
    /// With `position` or `follow`, the listener (the active camera unless
    /// [`Self::set_listener`] names one) hears the sound from its side, and
    /// its volume falls as `2 / distance` past 2 m.
    pub fn play_sound_with(
        &mut self,
        clip: &str,
        mut sound: crate::runtime::Sound,
    ) -> crate::runtime::SoundId {
        if let Some(entity) = sound.follow {
            if self.world.get_entity(entity).is_ok() {
                let matrix = world_matrix(self.world, entity);
                let at = matrix.transform_point(&nalgebra::Point3::origin());
                sound.position = Some(at.coords.into());
            }
        }
        let tick = self
            .world
            .get_resource::<FrameTime>()
            .map_or(0, |time| time.fixed_tick);
        self.audio().play(clip, &sound, tick)
    }

    /// Plays a clip that follows the named object every frame, like
    /// footsteps. `None` when no object has that name.
    pub fn play_sound_on(
        &mut self,
        name: &str,
        clip: &str,
        sound: crate::runtime::Sound,
    ) -> Option<crate::runtime::SoundId> {
        let entity = find_named_entity(self.world, name)?;
        Some(self.play_sound_with(
            clip,
            crate::runtime::Sound {
                follow: Some(entity),
                ..sound
            },
        ))
    }

    /// Changes a sound's speed and pitch together, like a tape, over `fade`
    /// seconds: 0.5 is half speed and an octave lower.
    pub fn set_sound_rate(
        &mut self,
        id: crate::runtime::SoundId,
        rate: f32,
        fade: f32,
    ) {
        self.audio().set_rate(id, rate, fade);
    }

    /// Moves a positioned sound to a world position.
    pub fn set_sound_position(
        &mut self,
        id: crate::runtime::SoundId,
        position: [f32; 3],
    ) {
        self.audio().set_position(id, position);
    }

    /// Pauses a sound where it is; [`Self::resume_sound`] continues it.
    pub fn pause_sound(&mut self, id: crate::runtime::SoundId) {
        self.audio().pause(id);
    }

    /// Continues a paused sound from where it stopped.
    pub fn resume_sound(&mut self, id: crate::runtime::SoundId) {
        self.audio().resume(id);
    }

    /// Pauses every playing sound on `bus`, or every sound with `None`:
    /// `pause_sounds(None)` for a pause menu.
    pub fn pause_sounds(&mut self, bus: Option<&str>) {
        self.audio().pause_bus(bus);
    }

    /// Resumes the paused sounds on `bus`, or every one with `None`.
    pub fn resume_sounds(&mut self, bus: Option<&str>) {
        self.audio().resume_bus(bus);
    }

    /// Sounds started and not yet ended, oldest first, with clip, bus and
    /// whether they are paused. It follows the audio device, so do not
    /// branch simulation on it: a headless replay has no device.
    pub fn playing_sounds(&mut self) -> Vec<crate::runtime::ActiveSound> {
        self.audio().playing()
    }

    /// Jumps a sound to `seconds` into its clip.
    pub fn seek_sound(&mut self, id: crate::runtime::SoundId, seconds: f32) {
        self.audio().seek(id, seconds);
    }

    /// Hears positioned sounds from the named object instead of the active
    /// camera; `None` goes back to the camera. Returns false when no object
    /// has that name.
    pub fn set_listener(&mut self, name: Option<&str>) -> bool {
        let entity = match name {
            Some(name) => match find_named_entity(self.world, name) {
                Some(entity) => Some(entity),
                None => return false,
            },
            None => None,
        };
        self.audio().set_listener(entity);
        true
    }

    /// Sets one of a bus's effects over `fade` seconds; `""` is the main
    /// output. A low-pass behind glass, a reverb for a big room:
    ///
    /// ```ignore
    /// scene.set_bus_effect("world", BusEffect::LowPass { cutoff_hz: 900.0 }, 0.3);
    /// ```
    pub fn set_bus_effect(
        &mut self,
        bus: &str,
        effect: crate::runtime::BusEffect,
        fade: f32,
    ) {
        self.audio().set_bus_effect(bus, effect, fade);
    }

    /// Most sounds a bus plays at once (64 by default). Past it, a new
    /// sound replaces the lowest-priority, then quietest, playing one, or
    /// is dropped when it ranks lowest itself.
    pub fn set_bus_voice_limit(&mut self, bus: &str, limit: usize) {
        self.audio().set_bus_voice_limit(bus, limit);
    }

    /// Multiplies the size of HUD text and captions by `scale`, clamped to
    /// 0.5 to 3, for players who need larger text; 1 is as authored. Game
    /// menus drawn with [`Self::ui`] read it from [`Self::text_scale`].
    pub fn set_text_scale(&mut self, scale: f32) {
        let scale = if scale.is_finite() {
            scale.clamp(0.5, 3.0)
        } else {
            1.0
        };
        self.world.insert_resource(crate::runtime::TextScale(scale));
    }

    /// The text size multiplier from [`Self::set_text_scale`], 1 by default.
    #[must_use]
    pub fn text_scale(&self) -> f32 {
        self.world
            .get_resource::<crate::runtime::TextScale>()
            .map_or(1.0, |scale| scale.0)
    }

    /// Shows or hides sound captions on the HUD, at `size` logical pixels.
    pub fn set_captions(&mut self, enabled: bool, size: f32) {
        self.world
            .insert_resource(crate::runtime::CaptionSettings { enabled, size });
    }

    /// Moves a playing sound's volume to `volume` over `fade` seconds; 0 is
    /// at once.
    pub fn set_sound_volume(
        &mut self,
        id: crate::runtime::SoundId,
        volume: f32,
        fade: f32,
    ) {
        self.audio().set_volume(id, volume, fade);
    }

    /// Moves a bus's volume (see [`crate::runtime::Sound::bus`]) to
    /// `volume` over `fade` seconds. Duck the music under an alarm with
    /// `set_bus_volume("music", 0.3, 0.2)` while the alarm plays on `sfx`.
    pub fn set_bus_volume(&mut self, bus: &str, volume: f32, fade: f32) {
        self.audio().set_bus_volume(bus, volume, fade);
    }

    /// Silences `bus` without forgetting its volume; `false` brings it
    /// back. `""` mutes everything.
    pub fn mute_bus(&mut self, bus: &str, muted: bool) {
        self.audio().mute_bus(bus, muted);
    }

    /// While any bus is soloed, only soloed buses play: `solo_bus("voice",
    /// true)` to hear only dialogue while tuning it.
    pub fn solo_bus(&mut self, bus: &str, solo: bool) {
        self.audio().solo_bus(bus, solo);
    }

    /// Stops one sound. An ID that already ended is ignored.
    pub fn stop_sound(&mut self, id: crate::runtime::SoundId) {
        self.audio().stop(id);
    }

    /// Stops every playing sound.
    pub fn stop_all_sounds(&mut self) {
        self.audio().stop_all();
    }

    /// Linear gain for every sound, 1 by default.
    pub fn set_master_volume(&mut self, volume: f32) {
        self.audio().set_master_volume(volume);
    }

    /// Sounds requested since the game started, whether or not a device
    /// played them. Includes [`SoundCue`](crate::runtime::SoundCue)s.
    pub fn sounds_requested(&mut self) -> u64 {
        self.audio().requested()
    }

    fn audio(&mut self) -> Mut<'_, crate::runtime::AudioQueue> {
        self.world
            .get_resource_or_insert_with(crate::runtime::AudioQueue::default)
    }

    /// Puts every scene object back as the game started: objects spawned
    /// since are removed, and moved, hidden or despawned ones return with
    /// their starting components. [`Self::once`] blocks run again.
    /// Counters saved in the scene go back to their starting values;
    /// counters made by [`Self::set_counter`] are gone until code sets them
    /// again. Time (`fixed_tick` keeps counting), input and resources carry
    /// on, and so do playing sounds: restart
    /// stops none, so a loop started in a `once` block would play twice.
    /// Call [`Self::stop_all_sounds`] (or stop the loop by its ID) before
    /// restarting. Returns false when the game was not
    /// started from a scene file, or when that file no longer loads (the
    /// error is logged and the scene stays as it was).
    ///
    /// To carry a value across, such as the night to play, save it first
    /// and read it back in setup:
    ///
    /// ```ignore
    /// scene.save_data("session.json", &night.to_string()).ok();
    /// scene.restart();
    /// // in a `once` block:
    /// let night = scene.load_data("session.json").and_then(|t| t.parse().ok()).unwrap_or(1);
    /// scene.set_counter("night", night);
    /// ```
    pub fn restart(&mut self) -> bool {
        restart_scene(self.world)
    }

    /// Replaces every scene object with the scene at `path`, relative to
    /// the project folder (the one holding `project.json`), for example
    /// `"scenes/level_2.rscene"`. The new scene becomes the one
    /// [`Self::restart`] returns to, and [`Self::once`] blocks run again.
    /// Time, input, resources and playing sounds carry on (see
    /// [`Self::restart`]). Counters are scene objects, so
    /// read any value to keep before loading and set it again after.
    ///
    /// # Errors
    /// Returns an error when the file is missing or is not a valid scene;
    /// the current scene then stays as it was.
    pub fn load_scene(
        &mut self,
        path: impl AsRef<Path>,
    ) -> Result<(), crate::runtime::SceneIoError> {
        let path = match self.world.get_resource::<ProjectFolder>() {
            Some(folder) => folder.0.join(path),
            None => path.as_ref().to_path_buf(),
        };
        load_scene(self.world, &path, SceneLoadMode::Replace)?;
        let start = scene_document(self.world, "start")?;
        self.world.insert_resource(StartingScene(start));
        forget_old_scene(self.world);
        Ok(())
    }

    /// Copies every scene object, the `once` keys and the physics solver
    /// state, to put back later with [`Self::restore`] while the game runs
    /// (a rewind, a checkpoint).
    ///
    /// # Errors
    /// Returns an error when a scene object cannot be saved, as for saving
    /// the scene to a file.
    pub fn snapshot(
        &mut self,
    ) -> Result<GameSnapshot, crate::runtime::SceneIoError> {
        let scene = scene_document(self.world, "snapshot")?;
        let mut query = self.world.query::<(
            Entity,
            &crate::runtime::SceneId,
            Option<&crate::runtime::SpawnOrder>,
            bevy_ecs::query::Has<crate::runtime::Sleeping>,
        )>();
        let objects = query
            .iter(self.world)
            .map(|(entity, id, order, asleep)| {
                (id.0, entity, order.copied(), asleep)
            })
            .collect();
        Ok(GameSnapshot {
            scene,
            once: self
                .world
                .get_resource::<GameOnceState>()
                .cloned()
                .unwrap_or_default(),
            physics: self
                .world
                .get_resource::<crate::runtime::PhysicsWorld>()
                .cloned(),
            next_order: self
                .world
                .get_resource::<crate::runtime::NextSpawnOrder>()
                .copied(),
            fluids: crate::runtime::capture_fluids(self.world),
            objects,
        })
    }

    /// Puts the scene back as it was at [`Self::snapshot`]: objects spawned
    /// since are removed and the others return with their components,
    /// velocities and sleep state. Physics then continues exactly as it did
    /// after the snapshot. Time, input, resources and the scene
    /// [`Self::restart`] returns to carry on. Return from `update` after
    /// this, as after `restart`.
    ///
    /// # Errors
    /// Returns an error when the copy cannot be loaded; the scene then
    /// stays as it was.
    pub fn restore(
        &mut self,
        snapshot: &GameSnapshot,
    ) -> Result<(), crate::runtime::SceneIoError> {
        load_scene_document(
            self.world,
            &snapshot.scene,
            SceneLoadMode::Replace,
        )?;
        let mut query =
            self.world.query::<(Entity, &crate::runtime::SceneId)>();
        let now: HashMap<uuid::Uuid, Entity> = query
            .iter(self.world)
            .map(|(entity, id)| (id.0, entity))
            .collect();
        let mut renamed = HashMap::new();
        for (id, old, order, asleep) in &snapshot.objects {
            let Some(&entity) = now.get(id) else {
                continue;
            };
            renamed.insert(*old, entity);
            let mut entity = self.world.entity_mut(entity);
            match order {
                Some(order) => entity.insert(*order),
                None => entity.remove::<crate::runtime::SpawnOrder>(),
            };
            if *asleep {
                entity.insert(crate::runtime::Sleeping);
            }
        }
        if let Some(next) = snapshot.next_order {
            self.world.insert_resource(next);
        }
        if let Some(physics) = &snapshot.physics {
            let mut physics = physics.clone();
            physics.rename_bodies(&renamed);
            self.world.insert_resource(physics);
        }
        crate::runtime::restore_fluids(self.world, &snapshot.fluids);
        self.world.insert_resource(snapshot.once.clone());
        self.world.remove_resource::<SceneNameIndex>();
        Ok(())
    }

    /// egui context for this frame's runtime UI, drawn over the game view.
    /// See [`crate::runtime::RuntimeUi`].
    #[cfg(feature = "ui")]
    #[must_use]
    pub fn ui(&self) -> egui::Context {
        self.world
            .resource::<crate::runtime::RuntimeUi>()
            .context()
            .clone()
    }

    /// Creates one visible built-in cube with a unique object name. Panics
    /// when a live object, including one from the scene file, has the name.
    pub fn spawn_cube(
        &mut self,
        name: impl Into<String>,
        transform: Transform,
        template: &CubeSpawn,
    ) -> Entity {
        let (mesh, material) = {
            let assets = self.world.resource::<AssetServer>();
            (assets.fallback_mesh, assets.fallback_material)
        };
        self.spawn_renderable(
            name.into(),
            transform,
            mesh,
            material,
            template.classes.clone(),
        )
    }

    /// Creates a cube using a material handle from the asset server.
    pub fn spawn_cube_with_material(
        &mut self,
        name: impl Into<String>,
        transform: Transform,
        template: &CubeSpawn,
        material: crate::assets::Handle<crate::assets::MaterialAsset>,
    ) -> Entity {
        let mesh = self.world.resource::<AssetServer>().fallback_mesh;
        self.spawn_renderable(
            name.into(),
            transform,
            mesh,
            material,
            template.classes.clone(),
        )
    }

    /// Registers a material and returns its generational asset handle.
    pub fn create_material(
        &mut self,
        material: crate::assets::MaterialAsset,
    ) -> crate::assets::Handle<crate::assets::MaterialAsset> {
        self.world
            .resource_mut::<AssetServer>()
            .materials
            .insert(material)
    }

    /// Changes the color used to clear the game render target.
    pub fn set_background_color(&mut self, color: [f32; 4]) {
        self.world.resource_mut::<RenderSettings>().background_color = color;
    }

    /// Turns screen-space reflections on or off. Off saves the scene copy,
    /// mip chain and depth pyramid passes when nothing refracts; a game with
    /// no mirror-like surfaces gains about half a millisecond a frame.
    pub fn set_reflections(&mut self, enabled: bool) {
        self.world.resource_mut::<RenderSettings>().reflections = enabled;
    }

    /// Draws shadow edges with one shadow-map tap (`true`) for crisp,
    /// low-poly shadows, or with 3x3 filtering (`false`, the default).
    pub fn set_hard_shadows(&mut self, hard: bool) {
        self.world.resource_mut::<RenderSettings>().hard_shadows = hard;
    }

    /// Renders the 3D scene at `scale` of the window size, from 0.25 to 2.0,
    /// and stretches it over the window. Below 1 trades sharpness for frame
    /// rate; above 1 supersamples. UI stays at full resolution. Captures and
    /// scenario screenshots use it too.
    pub fn set_render_scale(&mut self, scale: f32) {
        if scale.is_finite() {
            let (low, high) =
                crate::rendering::render_scale::RENDER_SCALE_RANGE;
            self.world.resource_mut::<RenderSettings>().render_scale =
                scale.clamp(low, high);
        }
    }

    /// Stretches a [`Self::set_render_scale`] frame with nearest-neighbour
    /// filtering (`true`) for square pixels, a retro or CCTV look, or with
    /// smooth filtering (`false`, the default).
    pub fn set_pixelated(&mut self, pixelated: bool) {
        self.world.resource_mut::<RenderSettings>().pixelated = pixelated;
    }

    /// Above 0, stretches a [`Self::set_render_scale`] frame below 1 with
    /// an edge-preserving filter and sharpening of this strength (up to
    /// 1), like FSR 1, instead of blur. 0 (the default) is a plain stretch.
    pub fn set_upscale_sharpness(&mut self, sharpness: f32) {
        if sharpness.is_finite() {
            self.world
                .resource_mut::<RenderSettings>()
                .upscale_sharpness = sharpness.clamp(0.0, 1.0);
        }
    }

    /// Sets the exposure of the scene's `rusting.tone_mapping`, adding a
    /// linear one when the scene has none. 1 is neutral; above 1 brightens
    /// a dark camera feed. Negative or non-finite values are ignored.
    pub fn set_exposure(&mut self, exposure: f32) {
        if !(exposure.is_finite() && exposure >= 0.0) {
            return;
        }
        let mut query = self.world.query::<&mut crate::runtime::ToneMapping>();
        if let Some(mut tone) = query.iter_mut(self.world).next() {
            tone.exposure = exposure;
        } else {
            self.world.spawn(crate::runtime::ToneMapping {
                exposure,
                ..crate::runtime::ToneMapping::default()
            });
        }
    }

    /// Turns the mouse look of player `name` on or off. Off frees a
    /// captured cursor so menus and in-world screens can be clicked; on
    /// captures it again at the next left click. False without such a
    /// player.
    pub fn set_mouse_look(&mut self, name: &str, enabled: bool) -> bool {
        let Some(entity) = find_named_entity(self.world, name) else {
            return false;
        };
        let Some(mut player) = self
            .world
            .get_mut::<crate::runtime::PlayerController>(entity)
        else {
            return false;
        };
        player.mouse_look = enabled;
        true
    }

    /// The scale from [`Self::set_render_scale`].
    #[must_use]
    pub fn render_scale(&self) -> f32 {
        self.world.resource::<RenderSettings>().render_scale
    }

    /// Waits for the display refresh before showing each frame.
    pub fn set_vsync(&mut self, enabled: bool) {
        self.world.resource_mut::<RenderSettings>().vsync = enabled;
    }

    /// Caps the frame rate. `None` removes the cap.
    pub fn set_max_fps(&mut self, fps: Option<u32>) {
        let mut settings = self.world.resource_mut::<RenderSettings>();
        settings.limit_fps = fps.is_some();
        if let Some(fps) = fps {
            settings.max_fps = fps.max(1);
        }
    }

    /// Switches the window to borderless fullscreen on its current monitor,
    /// or back to a window. Applied after this frame; headless runs ignore
    /// it.
    pub fn set_fullscreen(&mut self, fullscreen: bool) {
        self.world
            .get_resource_or_insert_with(WindowRequest::default)
            .fullscreen = fullscreen;
    }

    /// The value from [`Self::set_fullscreen`].
    #[must_use]
    pub fn fullscreen(&self) -> bool {
        self.world
            .get_resource::<WindowRequest>()
            .is_some_and(|request| request.fullscreen)
    }

    /// Asks for a window of `size` pixels after this frame. The platform may
    /// pick another size, and fullscreen ignores it; read the result with
    /// [`Self::viewport_size`] on a later frame.
    pub fn set_window_size(&mut self, size: [u32; 2]) {
        self.world
            .get_resource_or_insert_with(WindowRequest::default)
            .size = Some(size.map(|side| side.max(1)));
    }

    /// Sets the window title after this frame. Headless runs ignore it.
    pub fn set_window_title(&mut self, title: impl Into<String>) {
        self.world
            .get_resource_or_insert_with(WindowRequest::default)
            .title = Some(title.into());
    }

    /// Creates one visible procedural sphere with a unique object name. Panics
    /// when a live object, including one from the scene file, has the name.
    pub fn spawn_sphere(
        &mut self,
        name: impl Into<String>,
        transform: Transform,
        template: &SphereSpawn,
    ) -> Entity {
        let subdivisions = template.subdivisions;
        let mesh = self
            .world
            .get_resource::<SphereMeshCache>()
            .and_then(|cache| cache.0.get(&subdivisions).copied())
            .unwrap_or_else(|| {
                let mesh =
                    self.world.resource_mut::<AssetServer>().meshes.insert(
                        crate::assets::procedural_sphere_mesh(subdivisions),
                    );
                self.world
                    .get_resource_or_insert_with(SphereMeshCache::default)
                    .0
                    .insert(subdivisions, mesh);
                mesh
            });
        self.spawn_renderable(
            name.into(),
            transform,
            mesh,
            self.world.resource::<AssetServer>().fallback_material,
            template.classes.clone(),
        )
    }

    /// Creates a sphere using a material handle from the asset server.
    pub fn spawn_sphere_with_material(
        &mut self,
        name: impl Into<String>,
        transform: Transform,
        template: &SphereSpawn,
        material: crate::assets::Handle<crate::assets::MaterialAsset>,
    ) -> Entity {
        let subdivisions = template.subdivisions;
        let mesh = self
            .world
            .get_resource::<SphereMeshCache>()
            .and_then(|cache| cache.0.get(&subdivisions).copied())
            .unwrap_or_else(|| {
                let mesh =
                    self.world.resource_mut::<AssetServer>().meshes.insert(
                        crate::assets::procedural_sphere_mesh(subdivisions),
                    );
                self.world
                    .get_resource_or_insert_with(SphereMeshCache::default)
                    .0
                    .insert(subdivisions, mesh);
                mesh
            });
        self.spawn_renderable(
            name.into(),
            transform,
            mesh,
            material,
            template.classes.clone(),
        )
    }

    /// Inserts the shared components used by cube and sphere primitives.
    fn spawn_renderable(
        &mut self,
        name: String,
        transform: Transform,
        mesh: crate::assets::Handle<crate::assets::MeshAsset>,
        material: crate::assets::Handle<crate::assets::MaterialAsset>,
        classes: crate::runtime::ObjectClasses,
    ) -> Entity {
        // The index keeps names of despawned or renamed objects, so only a
        // live entity that still carries the name blocks the spawn.
        if find_named_entity(self.world, &name).is_some() {
            name_taken(&name);
        }
        let order = crate::runtime::next_spawn_order(self.world);
        let entity = self
            .world
            .spawn((
                crate::runtime::SceneId::new(),
                order,
                Name(name.clone()),
                transform,
                crate::runtime::MeshRenderer {
                    mesh,
                    material,
                    cast_shadows: true,
                    receive_shadows: true,
                },
                crate::runtime::Visibility::default(),
            ))
            .id();
        if !classes.names.is_empty() {
            self.world.entity_mut(entity).insert(classes);
        }
        self.world
            .resource_mut::<SceneNameIndex>()
            .entities
            .insert(name, entity);
        entity
    }

    /// Enables GPU physics for every object in one class.
    ///
    /// Call this from [`Self::once`] after procedural objects are spawned.
    pub fn apply_gpu_physics_to_class(
        &mut self,
        class: &str,
        settings: &GpuBodySettings,
    ) -> usize {
        let entities = {
            let mut query = self
                .world
                .query::<(Entity, &crate::runtime::ObjectClasses)>();
            query
                .iter(self.world)
                .filter_map(|(entity, classes)| {
                    classes.contains(class).then_some(entity)
                })
                .collect::<Vec<_>>()
        };
        for entity in &entities {
            self.world.entity_mut(*entity).insert((
                crate::runtime::PhysicsBody {
                    simulation: crate::runtime::SimulationClass::Gpu,
                    solver: settings.solver,
                    custom_shader: settings.custom_shader.clone(),
                },
                settings.rigid_body,
                settings.collider,
                settings.collision_layers,
                settings.sync,
            ));
        }
        entities.len()
    }

    /// Applies `edit` to the rigid body of `name` and wakes it. Returns
    /// false when the object or its rigid body does not exist.
    fn edit_body(
        &mut self,
        name: &str,
        edit: impl FnOnce(&mut crate::runtime::RigidBody),
    ) -> bool {
        let Some(entity) = find_named_entity(self.world, name) else {
            return false;
        };
        let Some(mut rigid_body) =
            self.world.get_mut::<crate::runtime::RigidBody>(entity)
        else {
            return false;
        };
        edit(&mut rigid_body);
        self.world
            .entity_mut(entity)
            .remove::<crate::runtime::Sleeping>();
        true
    }

    /// Sets a body's linear velocity, in metres per second, and wakes it.
    ///
    /// A CPU body moves with it from the next physics step. A GPU body reads
    /// it at the next GPU extraction, after which the compute shader owns it,
    /// so set it during setup.
    pub fn set_linear_velocity(
        &mut self,
        name: &str,
        velocity: [f32; 3],
    ) -> bool {
        self.edit_body(name, |body| body.linear_velocity = velocity)
    }

    /// Moves the named player or platformer controller at `velocity`
    /// (metres per second; a platformer ignores Z) for `seconds`, in place
    /// of walking, jumping and gravity; walls still stop it. False when the
    /// object has no controller.
    pub fn dash(
        &mut self,
        name: &str,
        velocity: [f32; 3],
        seconds: f32,
    ) -> bool {
        let Some(entity) = find_named_entity(self.world, name) else {
            return false;
        };
        let mut object = self.world.entity_mut(entity);
        if let Some(mut player) =
            object.get_mut::<crate::runtime::PlayerController>()
        {
            player.dash_velocity = velocity;
            player.dash_left = seconds;
        } else if let Some(mut player) =
            object.get_mut::<crate::runtime::PlatformerController>()
        {
            player.dash_velocity = velocity;
            player.dash_left = seconds;
        } else {
            return false;
        }
        true
    }

    /// A body's linear velocity in metres per second, or `None` when the
    /// object or its rigid body does not exist.
    #[must_use]
    pub fn linear_velocity(&mut self, name: &str) -> Option<[f32; 3]> {
        let entity = find_named_entity(self.world, name)?;
        self.world
            .get::<crate::runtime::RigidBody>(entity)
            .map(|body| body.linear_velocity)
    }

    /// Sets a body's angular velocity, in radians per second about each
    /// world axis, and wakes it. GPU bodies read it as for
    /// [`Self::set_linear_velocity`].
    pub fn set_angular_velocity(
        &mut self,
        name: &str,
        velocity: [f32; 3],
    ) -> bool {
        self.edit_body(name, |body| body.angular_velocity = velocity)
    }

    /// A body's angular velocity in radians per second, or `None` when the
    /// object or its rigid body does not exist.
    #[must_use]
    pub fn angular_velocity(&mut self, name: &str) -> Option<[f32; 3]> {
        let entity = find_named_entity(self.world, name)?;
        self.world
            .get::<crate::runtime::RigidBody>(entity)
            .map(|body| body.angular_velocity)
    }

    /// Stops a body completely: zeroes both velocities, forgets the
    /// solver's contact impulses and sleep count for it, and wakes it. Put
    /// it back in place first with [`GameObject::set_position`] and
    /// [`GameObject::set_rotation`]. Returns false when the object or its
    /// rigid body does not exist.
    pub fn reset_body(&mut self, name: &str) -> bool {
        let reset = self.edit_body(name, |body| {
            body.linear_velocity = [0.0; 3];
            body.angular_velocity = [0.0; 3];
        });
        if let (true, Some(entity)) =
            (reset, find_named_entity(self.world, name))
        {
            if let Some(mut physics) = self
                .world
                .get_resource_mut::<crate::runtime::PhysicsWorld>()
            {
                physics.forget_body(entity);
            }
        }
        reset
    }

    /// Changes a body's kind, for example a `Kinematic` template copied
    /// with [`Self::spawn_copy`] into a `Dynamic` projectile. A body that
    /// becomes `Kinematic` or `Fixed` stops where it is: kinematic bodies
    /// move by their velocity, so an old one would carry it away. Returns
    /// false when the object or its rigid body does not exist.
    pub fn set_body_kind(
        &mut self,
        name: &str,
        kind: crate::runtime::RigidBodyKind,
    ) -> bool {
        self.edit_body(name, |body| {
            if body.kind != kind
                && kind != crate::runtime::RigidBodyKind::Dynamic
            {
                body.linear_velocity = [0.0; 3];
                body.angular_velocity = [0.0; 3];
            }
            body.kind = kind;
        })
    }

    /// How the object called `name` was when the current scene loaded (by
    /// [`Self::restart`] and [`Self::load_scene`] too), so a round can put
    /// it back without reloading. `None` for objects spawned during play
    /// and when the game was not started from a scene file.
    #[must_use]
    pub fn initial(&self, name: &str) -> Option<InitialState> {
        use crate::runtime::SceneMaterial;
        let start = self.world.get_resource::<StartingScene>()?;
        let entity = start
            .0
            .entities
            .iter()
            .find(|entity| entity.name.as_deref() == Some(name))?;
        let material =
            entity
                .mesh_renderer
                .as_ref()
                .and_then(|renderer| match &renderer.material {
                    SceneMaterial::Inline(material) => Some(material),
                    SceneMaterial::BuiltinError => None,
                });
        Some(InitialState {
            transform: entity
                .transform
                .map(Transform::from)
                .unwrap_or_default(),
            color: material.map(|material| material.base_color),
            emissive: material.map(|material| material.emissive),
            body_kind: entity.rigid_body.map(|body| body.kind),
            visible: entity.visible.unwrap_or(true),
        })
    }

    /// Points the player controller on `name` at `yaw` and `pitch`, in
    /// radians (yaw 0 faces -Z). Returns false when the object has no
    /// player controller.
    pub fn set_look(&mut self, name: &str, yaw: f32, pitch: f32) -> bool {
        let Some(entity) = find_named_entity(self.world, name) else {
            return false;
        };
        let Some(mut player) = self
            .world
            .get_mut::<crate::runtime::PlayerController>(entity)
        else {
            return false;
        };
        player.yaw = yaw;
        player.pitch = pitch;
        true
    }

    /// Returns a scene object by name.
    ///
    /// # Panics
    ///
    /// Panics with a descriptive message if the object or its transform does
    /// not exist. Use [`Self::try_object`] when absence is expected.
    pub fn object(&mut self, name: &str) -> GameObject<'_> {
        if self.try_object(name).is_none() {
            panic!(
                "scene object `{name}` does not exist or has no Transform{}",
                nearest_names_hint(self.world, name)
            );
        }
        self.try_object(name).expect("checked above")
    }

    /// The ECS world, for anything this API does not cover.
    ///
    /// Preferred: the other `GameScene` methods; use this only when none fits.
    pub fn world(&mut self) -> &mut World {
        self.world
    }

    /// The `rusting.counter` called `name`, to read or change its `value`.
    /// With duplicate names, the one with the lowest scene ID wins, as for
    /// pickups and HUD text.
    ///
    /// Preferred: [`Self::counter_value`], [`Self::add_to_counter`] and
    /// [`Self::set_counter`], which keep no borrow of the scene.
    pub fn counter(
        &mut self,
        name: &str,
    ) -> Option<Mut<'_, crate::runtime::Counter>> {
        let mut counters = self.world.query::<(
            &mut crate::runtime::Counter,
            Option<&crate::runtime::SceneId>,
        )>();
        crate::runtime::find_counter(counters.iter_mut(self.world), name)
    }

    /// The value of the counter called `name` (counters are `i32`; store
    /// fractions as thousandths), or 0 when there is none.
    /// Reading a counter that does not exist prints a warning once per
    /// name, because it is usually a typo or a missing scene object.
    /// Takes `&self`, so read-only helpers need no `&mut GameScene`.
    #[must_use]
    pub fn counter_value(&self, name: &str) -> i32 {
        self.read_counter(name, |counter| counter.value)
            .unwrap_or_default()
    }

    /// The value of the counter called `name`, or `default` when there is
    /// none. Unlike [`Self::counter_value`] a missing counter does not warn,
    /// so game state can live in counters that are created on first write
    /// without creating each one at start.
    #[must_use]
    pub fn counter_or(&self, name: &str, default: i32) -> i32 {
        self.find_counter(name, |counter| counter.value)
            .unwrap_or(default)
    }

    /// Applies `read` to the counter called `name`, or warns once and gives
    /// `None` when there is none.
    fn read_counter<T>(
        &self,
        name: &str,
        read: impl FnOnce(&crate::runtime::Counter) -> T,
    ) -> Option<T> {
        let found = self.find_counter(name, read);
        if found.is_none() {
            warn_missing_counter(name, || {
                let mut counters =
                    self.world.try_query::<&crate::runtime::Counter>()?;
                let names = counters.iter(self.world).map(|c| c.name.as_str());
                Some(nearest_hint(names, name))
            });
        }
        found
    }

    fn find_counter<T>(
        &self,
        name: &str,
        read: impl FnOnce(&crate::runtime::Counter) -> T,
    ) -> Option<T> {
        self.world
            .try_query::<(
                &crate::runtime::Counter,
                Option<&crate::runtime::SceneId>,
            )>()
            .and_then(|mut counters| {
                crate::runtime::find_counter(counters.iter(self.world), name)
                    .map(read)
            })
    }

    /// Adds `amount` (which may be negative) to the counter called `name`
    /// and returns its new value. A missing counter is created at 0 first,
    /// as for [`Self::set_counter`].
    pub fn add_to_counter(&mut self, name: &str, amount: i32) -> i32 {
        let mut counter = self.counter_or_create(name);
        counter.value += amount;
        counter.value
    }

    /// Sets the counter called `name` to `value`. A missing counter is
    /// created: an object named `name` holding only a `rusting.counter`
    /// with no target, so game state needs no scene object per value.
    /// Scenarios reach it with `{"counter": "name", ...}`; `restart` drops
    /// it with the rest of the round.
    pub fn set_counter(&mut self, name: &str, value: i32) {
        self.counter_or_create(name).value = value;
    }

    fn counter_or_create(
        &mut self,
        name: &str,
    ) -> Mut<'_, crate::runtime::Counter> {
        if self.counter(name).is_none() {
            let order = crate::runtime::next_spawn_order(self.world);
            self.world.spawn((
                Name(name.to_owned()),
                crate::runtime::SceneId::new(),
                order,
                crate::runtime::Counter {
                    name: name.to_owned(),
                    value: 0,
                    target: None,
                },
            ));
        }
        self.counter(name).expect("created above")
    }

    /// Whether the cooldown called `name` has run out, or never started.
    /// Cooldowns count fixed ticks, so pause and time scale stop and slow
    /// them like the simulation and a scenario repeats them exactly. The
    /// counter `name` holds the fixed tick the cooldown ends on.
    #[must_use]
    pub fn cooldown_ready(&self, name: &str) -> bool {
        self.cooldown_left(name) == 0.0
    }

    /// Starts the cooldown called `name` for `seconds` of game time, from
    /// this tick. See [`Self::cooldown_ready`].
    pub fn start_cooldown(&mut self, name: &str, seconds: f32) {
        let time = self.frame_time();
        let ticks =
            (f64::from(seconds) / time.fixed_delta.as_secs_f64()).ceil();
        let end = time.fixed_tick as f64 + ticks.max(0.0);
        self.set_counter(name, end.min(f64::from(i32::MAX)) as i32);
    }

    /// Seconds of game time left on the cooldown called `name`, 0 when it
    /// is ready. Use it for a HUD meter.
    #[must_use]
    pub fn cooldown_left(&self, name: &str) -> f32 {
        let time = self.frame_time();
        let end = i64::from(self.counter_or(name, 0));
        let left = (end - time.fixed_tick as i64).max(0);
        (left as f64 * time.fixed_delta.as_secs_f64()) as f32
    }

    /// Puts the status `effect` (such as `"poisoned"` or `"stunned"`) on
    /// the named object for `seconds` of game time. A longer remaining
    /// time is kept, so reapplying refreshes but never shortens. Statuses
    /// are cooldowns in the counter `"<object>/<effect>"`, so they count
    /// fixed ticks and snapshots and [`Self::save_counters`] keep them.
    pub fn add_status(&mut self, object: &str, effect: &str, seconds: f32) {
        let key = format!("{object}/{effect}");
        if self.cooldown_left(&key) < seconds {
            self.start_cooldown(&key, seconds);
        }
    }

    /// Whether the named object has the status `effect`. See
    /// [`Self::add_status`].
    #[must_use]
    pub fn has_status(&self, object: &str, effect: &str) -> bool {
        self.status_left(object, effect) > 0.0
    }

    /// Seconds left on the status `effect` of the named object, 0 when it
    /// has none.
    #[must_use]
    pub fn status_left(&self, object: &str, effect: &str) -> f32 {
        self.cooldown_left(&format!("{object}/{effect}"))
    }

    /// Ends the status `effect` on the named object early.
    pub fn clear_status(&mut self, object: &str, effect: &str) {
        self.set_counter(&format!("{object}/{effect}"), 0);
    }

    fn frame_time(&self) -> FrameTime {
        self.world
            .get_resource::<FrameTime>()
            .copied()
            .unwrap_or_default()
    }

    /// Takes `amount` hit points from the named object's `rusting.health`
    /// (a negative amount heals, up to `max`) and returns what is left,
    /// never below 0. Check for 0 to defeat it. `None`, changing nothing,
    /// when no object has that name or it has no health. Each change
    /// sends a [`crate::runtime::Damaged`] signal to the object.
    pub fn damage(&mut self, name: &str, amount: i32) -> Option<i32> {
        let entity = find_named_entity(self.world, name)?;
        let mut health =
            self.world.get_mut::<crate::runtime::Health>(entity)?;
        let max = health.max.max(health.value);
        health.value = health.value.saturating_sub(amount).clamp(0, max);
        let health = health.value;
        self.world.trigger(crate::runtime::Damaged {
            entity,
            amount,
            health,
        });
        Some(health)
    }

    /// The named object's hit points, or `None` when it has no
    /// `rusting.health`.
    #[must_use]
    pub fn health(&mut self, name: &str) -> Option<i32> {
        let entity = find_named_entity(self.world, name)?;
        self.world
            .get::<crate::runtime::Health>(entity)
            .map(|health| health.value)
    }

    /// Whether both named objects have `rusting.health` on the same
    /// non-empty team, so friendly fire can be skipped.
    #[must_use]
    pub fn same_team(&mut self, a: &str, b: &str) -> bool {
        let mut team = |name: &str| {
            let entity = find_named_entity(self.world, name)?;
            self.world
                .get::<crate::runtime::Health>(entity)
                .map(|health| health.team.clone())
                .filter(|team| !team.is_empty())
        };
        let a = team(a);
        a.is_some() && a == team(b)
    }

    /// Puts the named object's state machine in `state`, as a
    /// `rusting.state` component scenarios can expect on. Returns `true`
    /// when the state changed, so enter actions go in an `if`; the
    /// component's own enter and exit actions run too. Setting the
    /// current state again keeps its start tick and returns `false`.
    /// Warns once and returns `false` when no object has that name.
    pub fn set_state(&mut self, name: &str, state: &str) -> bool {
        let tick = self.frame_time().fixed_tick;
        let Some(entity) = find_or_warn(self.world, name) else {
            return false;
        };
        if self
            .world
            .get::<crate::runtime::ObjectState>(entity)
            .is_some_and(|current| current.state == state)
        {
            return false;
        }
        // Keeps the scene's transitions.
        let mut object = self.world.entity_mut(entity);
        let mut current = object
            .entry::<crate::runtime::ObjectState>()
            .or_default()
            .into_mut();
        let changes = current.changes(&current.state, state);
        current.state = state.to_owned();
        current.since_tick = tick;
        for (counter, add) in changes {
            self.add_to_counter(&counter, add);
        }
        true
    }

    /// The named object's state from [`Self::set_state`] or its scene, or
    /// `""` when it has none.
    #[must_use]
    pub fn state(&mut self, name: &str) -> String {
        self.object_state(name)
            .map(|state| state.state.clone())
            .unwrap_or_default()
    }

    /// Seconds of game time since the named object entered its current
    /// state, counted in fixed ticks; 0 when it has no state.
    #[must_use]
    pub fn state_seconds(&mut self, name: &str) -> f32 {
        let time = self.frame_time();
        self.object_state(name).map_or(0.0, |state| {
            (time.fixed_tick.saturating_sub(state.since_tick) as f64
                * time.fixed_delta.as_secs_f64()) as f32
        })
    }

    fn object_state(
        &mut self,
        name: &str,
    ) -> Option<&crate::runtime::ObjectState> {
        let entity = find_named_entity(self.world, name)?;
        self.world.get::<crate::runtime::ObjectState>(entity)
    }

    /// The run's seed: a scenario's `seed`, `rusting run --seed`, or 0.
    /// Use it for things that must not change with the tick, such as a
    /// level layout generated once.
    #[must_use]
    pub fn seed(&self) -> u64 {
        self.world
            .get_resource::<crate::runtime::RandomSeed>()
            .map_or(0, |seed| seed.0)
    }

    /// A value in `[0, 1)` from the run's seed, the fixed tick and
    /// `stream`. The same seed gives the same game, so scenarios repeat;
    /// draw with a different `stream` for each value needed in one tick.
    #[must_use]
    pub fn random(&self, stream: u64) -> f32 {
        let tick = self
            .world
            .get_resource::<FrameTime>()
            .map_or(0, |time| time.fixed_tick);
        let seed = self
            .world
            .get_resource::<crate::runtime::RandomSeed>()
            .copied()
            .unwrap_or_default();
        seed.unit(tick, crate::runtime::RandomSeed::stream("game", stream))
    }

    /// Picks one entry of a loot table of `(item, weight)` pairs, each with
    /// a chance of its weight over the total, using [`Self::random`] with
    /// `stream`, so the same seed rolls the same loot. `None` when every
    /// weight is 0.
    #[must_use]
    pub fn roll_loot(
        &self,
        table: &[(&str, u32)],
        stream: u64,
    ) -> Option<String> {
        weighted_pick(table, self.random(stream)).map(str::to_owned)
    }

    /// Rolls the `rusting.loot_table` on the object called `name`, as
    /// [`Self::roll_loot`] does. Warns once and gives `None` when no
    /// object has that name; `None` too when it has no table or every
    /// weight is 0.
    #[must_use]
    pub fn roll_loot_table(
        &mut self,
        name: &str,
        stream: u64,
    ) -> Option<String> {
        let entity = find_or_warn(self.world, name)?;
        let table = self.world.get::<crate::runtime::LootTable>(entity)?;
        let entries: Vec<(&str, u32)> = table
            .entries
            .iter()
            .map(|entry| (entry.item.as_str(), entry.weight))
            .collect();
        weighted_pick(&entries, self.random(stream)).map(str::to_owned)
    }

    /// Starts the `rusting.dialogue` on the object called `name` at its
    /// first line, even when it is already running. Returns `false` when
    /// it has no dialogue or no lines; warns once when no object has that
    /// name.
    pub fn start_dialogue(&mut self, name: &str) -> bool {
        let Some(entity) = find_or_warn(self.world, name) else {
            return false;
        };
        let Some(mut dialogue) =
            self.world.get_mut::<crate::runtime::Dialogue>(entity)
        else {
            return false;
        };
        let Some(first) = dialogue.lines.first().map(|line| line.id.clone())
        else {
            return false;
        };
        dialogue.current = first;
        true
    }

    /// The line the named object's dialogue shows, with its speaker, text
    /// and choice texts translated by [`Self::tr`]; `None` when it is not
    /// running.
    #[must_use]
    pub fn dialogue_line(
        &mut self,
        name: &str,
    ) -> Option<crate::runtime::DialogueLine> {
        let entity = find_or_warn(self.world, name)?;
        let dialogue = self.world.get::<crate::runtime::Dialogue>(entity)?;
        let mut line = dialogue
            .lines
            .iter()
            .find(|line| line.id == dialogue.current && !line.id.is_empty())?
            .clone();
        line.speaker = self.tr(&line.speaker);
        line.text = self.tr(&line.text);
        for choice in &mut line.choices {
            choice.text = self.tr(&choice.text);
        }
        Some(line)
    }

    /// Moves the named object's dialogue on: to the shown line's `next`
    /// when it has no choices, else to choice `choice`'s `next` after
    /// adding its `add` to its counter. An empty or unknown `next` ends
    /// the dialogue. A choice index the line lacks changes nothing.
    /// Returns whether the dialogue is still running.
    pub fn advance_dialogue(&mut self, name: &str, choice: usize) -> bool {
        let Some(line) = self.dialogue_line(name) else {
            return false;
        };
        let next = if line.choices.is_empty() {
            line.next
        } else {
            let Some(picked) = line.choices.get(choice) else {
                return true;
            };
            if !picked.counter.is_empty() {
                self.add_to_counter(&picked.counter, picked.add);
            }
            picked.next.clone()
        };
        let Some(entity) = find_or_warn(self.world, name) else {
            return false;
        };
        let Some(mut dialogue) =
            self.world.get_mut::<crate::runtime::Dialogue>(entity)
        else {
            return false;
        };
        let running = !next.is_empty()
            && dialogue.lines.iter().any(|line| line.id == next);
        dialogue.current = if running { next } else { String::new() };
        running
    }

    /// Takes `amount` from the counter `name` (such as an item count or
    /// money) when it holds at least that much, and returns whether it
    /// did; otherwise the counter is unchanged.
    pub fn spend(&mut self, name: &str, amount: i32) -> bool {
        let enough = self.counter_or(name, 0) >= amount;
        if enough {
            self.add_to_counter(name, -amount);
        }
        enough
    }

    /// True when the counter called `name` has reached its target. A
    /// missing counter is false and warns once, as for
    /// [`Self::counter_value`].
    #[must_use]
    pub fn counter_complete(&self, name: &str) -> bool {
        self.read_counter(name, crate::runtime::Counter::complete)
            .unwrap_or_default()
    }

    /// True on the frame `action` was pressed; inside a tick function (see
    /// [`run_game_with_tick`]), true on the tick the press happened in by
    /// its recorded time (see [`Self::press_tick`]), even when frames
    /// without a tick came between or one frame ran several ticks. Actions come from
    /// `rusting.input_action` components, the built-in `player.*` actions,
    /// or [`crate::runtime::ActionMap`] bindings made in code.
    #[must_use]
    pub fn pressed(&self, action: &str) -> bool {
        self.world
            .resource::<crate::runtime::ActionMap>()
            .just_pressed(self.world.resource::<RuntimeInput>(), action)
    }

    /// When `action` was pressed this frame, in fractional fixed ticks:
    /// `121.4` is 40% of a tick after tick 121. A window records the key or
    /// mouse event's own time, so a rhythm game can judge finer than a tick.
    /// A scenario press or tap step with `"at": 0.4` gives `tick + 0.4`;
    /// without `at`, and from gamepads, it is the frame's tick. `None` when
    /// the action was not pressed this frame.
    #[must_use]
    pub fn press_tick(&self, action: &str) -> Option<f64> {
        if !self.pressed(action) {
            return None;
        }
        let actions = self.world.resource::<crate::runtime::ActionMap>();
        actions
            .press_tick(self.world.resource::<RuntimeInput>(), action)
            .or_else(|| {
                let time = self.world.get_resource::<FrameTime>()?;
                Some(time.fixed_tick as f64)
            })
    }

    /// True while `action` is held down.
    #[must_use]
    pub fn held(&self, action: &str) -> bool {
        self.world
            .resource::<crate::runtime::ActionMap>()
            .held(self.world.resource::<RuntimeInput>(), action)
    }

    /// Names of the HUD buttons (`rusting.hud` with `button` true) clicked
    /// since the last frame. A click shows here on the frame after it.
    #[must_use]
    pub fn clicked(&self) -> Vec<String> {
        let Some(pressed) = self
            .world
            .get_resource::<EventQueue<crate::runtime::HudButtonPressed>>()
        else {
            return Vec::new();
        };
        pressed
            .iter()
            .filter_map(|click| self.world.get::<Name>(click.entity))
            .map(|name| name.0.clone())
            .collect()
    }

    /// Cursor position in pixels from the top-left corner of the view, or
    /// `None` before the cursor first enters it.
    #[must_use]
    pub fn cursor(&self) -> Option<[f32; 2]> {
        self.world.resource::<RuntimeInput>().cursor_position()
    }

    /// View size in pixels. In `rusting test` it is the scenario's
    /// `capture_size` from tick 0.
    #[must_use]
    pub fn viewport_size(&self) -> [f32; 2] {
        self.world.resource::<RuntimeInput>().viewport_size()
    }

    /// False while the game window has lost focus (alt-tab): held keys were
    /// released, so pause here if the game should not run unattended. Always
    /// true headless; a scenario's `focus` step changes it.
    #[must_use]
    pub fn window_focused(&self) -> bool {
        self.world.resource::<RuntimeInput>().focused()
    }

    /// Names of the keys and gamepad buttons pressed this frame (`KeyA`,
    /// `Space`, `ArrowUp`, `PadSouth`), for a "press a key" rebinding
    /// prompt; pass one to [`Self::rebind`].
    #[must_use]
    pub fn keys_pressed(&self) -> Vec<String> {
        let input = self.world.resource::<RuntimeInput>();
        input
            .keys_pressed()
            .map(|key| format!("{key:?}"))
            .chain(input.pads_pressed().map(|button| format!("Pad{button:?}")))
            .collect()
    }

    /// A gamepad stick's tilt, each axis -1..1 with `[0, 1]` fully up;
    /// `[0, 0]` with no pad. For analog control; for digital actions bind
    /// `PadLeftStickUp` and the other stick directions instead.
    #[must_use]
    pub fn stick(&self, stick: crate::runtime::Stick) -> [f32; 2] {
        self.world.resource::<RuntimeInput>().stick(stick)
    }

    /// The inputs the scene's `rusting.input_action` binds to `action`,
    /// as [`Self::rebind`] takes them; empty when there is none. Bindings
    /// made in code with [`crate::runtime::ActionMap`] are left out.
    #[must_use]
    pub fn binding(&mut self, action: &str) -> Vec<String> {
        let mut query = self.world.query::<&crate::runtime::InputAction>();
        query
            .iter(self.world)
            .find(|found| found.action == action)
            .map(|found| found.inputs.clone())
            .unwrap_or_default()
    }

    /// Binds `action` to exactly `inputs` (key names as in
    /// [`Self::keys_pressed`], or `MouseLeft`, `MouseRight`, `MouseMiddle`)
    /// by editing the scene's `rusting.input_action` for it, or adding one.
    /// Bindings made in code with [`crate::runtime::ActionMap`] stay. The
    /// new binding is not saved; store it with [`Self::save_data`] and call
    /// this again at start.
    ///
    /// # Errors
    /// Returns an error naming the first unknown input; nothing changes.
    pub fn rebind(
        &mut self,
        action: &str,
        inputs: &[&str],
    ) -> Result<(), String> {
        for input in inputs {
            crate::runtime::parse_input(input)?;
        }
        let inputs: Vec<String> =
            inputs.iter().map(|&input| input.to_owned()).collect();
        let mut query = self.world.query::<&mut crate::runtime::InputAction>();
        if let Some(mut found) = query
            .iter_mut(self.world)
            .find(|found| found.action == action)
        {
            found.inputs = inputs;
            return Ok(());
        }
        self.world.spawn((
            Name(format!("input {action}")),
            crate::runtime::SceneId::new(),
            crate::runtime::InputAction {
                action: action.to_owned(),
                inputs,
            },
        ));
        Ok(())
    }

    /// Like [`Self::rebind`], but replaces only the inputs on the same
    /// device as `inputs` (keyboard and mouse, or gamepad) and keeps the
    /// rest, in their place: rebinding `jump` from `Space` to `KeyJ` keeps
    /// `PadSouth`.
    ///
    /// # Errors
    /// Returns an error naming the first unknown input; nothing changes.
    pub fn rebind_device(
        &mut self,
        action: &str,
        inputs: &[&str],
    ) -> Result<(), String> {
        let pad = |name: &str| name.starts_with("Pad");
        let old = self.binding(action);
        let same = |name: &str| inputs.iter().any(|&new| pad(new) == pad(name));
        let at = old.iter().position(|name| same(name)).unwrap_or(old.len());
        let mut all: Vec<&str> = old
            .iter()
            .map(String::as_str)
            .filter(|&name| !same(name))
            .collect();
        let at = at.min(all.len());
        all.splice(at..at, inputs.iter().copied());
        self.rebind(action, &all)
    }

    /// Writes `text` to the file `key` in the game's user data folder,
    /// creating folders as needed. The folder is `RUSTING_USER_DATA` when
    /// set, else `~/.local/share/<game>` on Linux,
    /// `~/Library/Application Support/<game>` on macOS and
    /// `%APPDATA%\<game>` on Windows, where `<game>` is the executable name.
    /// `key` is a relative path such as `settings.txt` or `saves/1.json`.
    /// `rusting test` gives each scenario its own empty folder; a
    /// scenario's `files` field seeds it before tick 0.
    ///
    /// # Errors
    /// Returns an error for an absolute `key` or one with `..`, or when
    /// the file cannot be written.
    pub fn save_data(&self, key: &str, text: &str) -> std::io::Result<()> {
        let path = user_data_path(key)?;
        if let Some(folder) = path.parent() {
            std::fs::create_dir_all(folder)?;
        }
        // A crash mid-write keeps the old file instead of a cut-off one.
        crate::runtime::write_atomic_unchecked(path, text.as_bytes())
    }

    /// Saves the counters called `names` under `key` (a save slot such as
    /// `saves/1.json`) with the game's save `version`, as JSON through
    /// [`Self::save_data`]. A counter that does not exist saves as 0.
    /// For an autosave, save when [`Self::cooldown_ready`]`("autosave")`
    /// and then [`Self::start_cooldown`]`("autosave", 60.0)`; the
    /// cooldown counts fixed ticks, so a paused game does not autosave.
    ///
    /// # Errors
    /// Returns the error from [`Self::save_data`].
    pub fn save_counters(
        &self,
        key: &str,
        version: u32,
        names: &[&str],
    ) -> std::io::Result<()> {
        self.save_data(key, &self.counter_save(version, names))
    }

    /// The text [`Self::save_counters`] writes.
    fn counter_save(&self, version: u32, names: &[&str]) -> String {
        let counters: serde_json::Map<String, serde_json::Value> = names
            .iter()
            .map(|&name| (name.to_owned(), self.counter_or(name, 0).into()))
            .collect();
        let save =
            serde_json::json!({"version": version, "counters": counters});
        format!("{save:#}\n")
    }

    /// Sets every counter saved under `key` by [`Self::save_counters`] and
    /// returns the save's version, so a newer game can migrate an old
    /// save: read the old counters, set the new ones. `None` when there is
    /// no such save; a file that is not a counter save also warns.
    pub fn load_counters(&mut self, key: &str) -> Option<u32> {
        let text = self.load_data(key)?;
        let version = self.apply_counter_save(&text);
        if version.is_none() {
            eprintln!("warning: `{key}` is not a save from save_counters");
        }
        version
    }

    /// Sets the counters in `text` from [`Self::counter_save`] and returns
    /// its version; `None`, setting nothing, when it is not such a save.
    fn apply_counter_save(&mut self, text: &str) -> Option<u32> {
        let save: serde_json::Value = serde_json::from_str(text).ok()?;
        let version = u32::try_from(save["version"].as_u64()?).ok()?;
        let counters = save["counters"]
            .as_object()?
            .iter()
            .map(|(name, value)| {
                Some((name.clone(), i32::try_from(value.as_i64()?).ok()?))
            })
            .collect::<Option<Vec<_>>>()?;
        for (name, value) in counters {
            self.set_counter(&name, value);
        }
        Some(version)
    }

    /// Saves the registered `components` (such as `rusting.health` or
    /// `rusting.state`) of the objects called `objects` under `key`, with
    /// the game's save `version`, as JSON through [`Self::save_data`].
    /// Objects that do not exist and components an object lacks are left
    /// out. Each value keeps its component version, so loading an old
    /// save runs the component's migrations.
    ///
    /// # Errors
    /// Returns the error from [`Self::save_data`].
    pub fn save_objects(
        &mut self,
        key: &str,
        version: u32,
        objects: &[&str],
        components: &[&str],
    ) -> std::io::Result<()> {
        let text = object_save(self.world, version, objects, components);
        self.save_data(key, &text)
    }

    /// Restores every component saved under `key` by
    /// [`Self::save_objects`] onto the objects with the saved names and
    /// returns the save's version. `None` when there is no such save; a
    /// file that is not an object save warns, and so does each saved
    /// object that is missing or component that does not load.
    pub fn load_objects(&mut self, key: &str) -> Option<u32> {
        let text = self.load_data(key)?;
        let version = apply_object_save(self.world, &text);
        if version.is_none() {
            eprintln!("warning: `{key}` is not a save from save_objects");
        }
        version
    }

    /// The text [`Self::save_data`] stored under `key`, or `None` when
    /// there is no such file.
    #[must_use]
    pub fn load_data(&self, key: &str) -> Option<String> {
        std::fs::read_to_string(user_data_path(key).ok()?).ok()
    }

    /// The keys of the files [`Self::save_data`] stored in the user data
    /// folder `folder` (such as `saves`), sorted, for a load menu's save
    /// slots: `["saves/1.json", "saves/2.json"]`. Empty when there is no
    /// such folder.
    #[must_use]
    pub fn saved_keys(&self, folder: &str) -> Vec<String> {
        user_data_path(folder)
            .map(|path| keys_in(&path, folder))
            .unwrap_or_default()
    }

    /// Deletes the file [`Self::save_data`] stored under `key`. Returns
    /// false when there was none.
    pub fn delete_data(&self, key: &str) -> bool {
        user_data_path(key).is_ok_and(|path| std::fs::remove_file(path).is_ok())
    }

    /// Ends the game after this frame: the window closes, a headless run
    /// stops, and a scenario ends its run (check it with `expect_quit`).
    pub fn quit(&mut self) {
        self.world
            .resource_mut::<crate::runtime::ExitState>()
            .requested = true;
    }

    /// Stops fixed ticks while `paused`: physics, tweens, player
    /// controllers, emitters and `rusting.counter` changes from pickups.
    /// This update function still runs every frame, so a pause menu draws
    /// and reads input. `FrameTime::fixed_tick` and `elapsed` stop too, so
    /// tick-indexed randomness and timers resume where they were;
    /// `FrameTime::frame` keeps counting. Scenario tick numbers count frames
    /// and keep going.
    pub fn set_paused(&mut self, paused: bool) {
        self.world
            .resource_mut::<crate::runtime::TimeControl>()
            .paused = paused;
    }

    /// True while [`Self::set_paused`] holds fixed ticks.
    #[must_use]
    pub fn paused(&self) -> bool {
        self.world.resource::<crate::runtime::TimeControl>().paused
    }

    /// Runs game time at `scale` times real time: 0.5 is slow motion, 2
    /// is double speed. Fixed ticks keep their length and only come more
    /// or less often, so a scenario gives the same result at any scale.
    /// Negative scales count as 0.
    pub fn set_time_scale(&mut self, scale: f64) {
        self.world
            .resource_mut::<crate::runtime::TimeControl>()
            .time_scale = scale.max(0.0);
    }

    /// The scale set by [`Self::set_time_scale`], 1 by default.
    #[must_use]
    pub fn time_scale(&self) -> f64 {
        self.world
            .resource::<crate::runtime::TimeControl>()
            .time_scale
    }

    /// Every counter's name and value, sorted by name, for saving game
    /// state that lives in counters.
    #[must_use]
    pub fn counters(&self) -> Vec<(String, i32)> {
        let mut counters: Vec<_> = self
            .world
            .try_query::<&crate::runtime::Counter>()
            .map(|mut query| {
                query
                    .iter(self.world)
                    .map(|counter| (counter.name.clone(), counter.value))
                    .collect()
            })
            .unwrap_or_default();
        counters.sort();
        counters.dedup_by(|a, b| a.0 == b.0);
        counters
    }

    /// First collider on a ray from `origin` along `direction` within
    /// `max_distance` metres, sensors included. A ray starting inside a
    /// collider passes through it. The hit holds the object's `name`, the
    /// world `point`, the surface `normal` and the `distance` from `origin`.
    /// Only colliders with a `physics_body` count (`Static` for one that
    /// never moves); `rusting check` warns with `SCENE_COLLIDER_WITHOUT_BODY`
    /// about a collider without one. A hit on a ragdoll body reports its
    /// bone's name, and [`Self::raycast_skipping`] skips it by the bone's
    /// classes, so a character can skip its own limbs.
    #[must_use]
    pub fn raycast(
        &self,
        origin: [f32; 3],
        direction: [f32; 3],
        max_distance: f32,
    ) -> Option<RayHit> {
        self.raycast_skipping(origin, direction, max_distance, &[])
    }

    /// [`Self::raycast`] that passes through objects in any of
    /// `skip_classes`: `raycast_skipping(eye, dir, 20.0, &["glass"])`.
    #[must_use]
    pub fn raycast_skipping(
        &self,
        origin: [f32; 3],
        direction: [f32; 3],
        max_distance: f32,
        skip_classes: &[&str],
    ) -> Option<RayHit> {
        self.cast(origin, direction, max_distance, skip_classes, &[], false)
    }

    /// [`Self::raycast`] that hits only objects in at least one of
    /// `classes` and passes through everything else:
    /// `raycast_only(eye, dir, 500.0, &["island"])` finds the island under
    /// a point even with boats, birds and water in the way. Empty
    /// `classes` hits nothing.
    #[must_use]
    pub fn raycast_only(
        &self,
        origin: [f32; 3],
        direction: [f32; 3],
        max_distance: f32,
        classes: &[&str],
    ) -> Option<RayHit> {
        if classes.is_empty() {
            return None;
        }
        self.cast(origin, direction, max_distance, &[], classes, false)
    }

    /// [`Self::raycast`] that passes through hidden objects: one whose
    /// `visible` is false, or that has a hidden parent, such as a template
    /// kept for [`Self::spawn_copy`]. Use it for line of sight; plain
    /// `raycast` keeps hitting invisible walls.
    #[must_use]
    pub fn raycast_visible(
        &self,
        origin: [f32; 3],
        direction: [f32; 3],
        max_distance: f32,
    ) -> Option<RayHit> {
        self.cast(origin, direction, max_distance, &[], &[], true)
    }

    fn cast(
        &self,
        origin: [f32; 3],
        direction: [f32; 3],
        max_distance: f32,
        skip_classes: &[&str],
        only_classes: &[&str],
        visible_only: bool,
    ) -> Option<RayHit> {
        // A ragdoll body stands for its bone.
        let owner = |entity| {
            self.world
                .get::<crate::runtime::RagdollPart>(entity)
                .map_or(entity, |part| part.bone)
        };
        // No lookups per collider unless classes filter the ray.
        let keep = |entity| {
            let entity = owner(entity);
            if visible_only
                && !crate::runtime::visible_in_hierarchy(self.world, entity)
            {
                return false;
            }
            if skip_classes.is_empty() && only_classes.is_empty() {
                return true;
            }
            let classes =
                self.world.get::<crate::runtime::ObjectClasses>(entity);
            let any_of = |wanted: &[&str]| {
                classes.is_some_and(|classes| {
                    wanted.iter().any(|class| classes.contains(class))
                })
            };
            !any_of(skip_classes)
                && (only_classes.is_empty() || any_of(only_classes))
        };
        let hit = self
            .world
            .get_resource::<crate::runtime::PhysicsWorld>()?
            .raycast_where(origin, direction, max_distance, u32::MAX, keep)?;
        Some(RayHit {
            name: self
                .world
                .get::<Name>(owner(hit.entity))
                .map(|name| name.0.clone())
                .unwrap_or_default(),
            point: hit.point,
            normal: hit.normal,
            distance: hit.distance,
        })
    }

    /// Casts [`Self::raycast`] from the active camera through the center of
    /// the view: what a first-person crosshair points at. Uses the camera's
    /// transforms as they are now, so an aim set this frame counts.
    #[must_use]
    pub fn aim(&mut self, max_distance: f32) -> Option<RayHit> {
        let (origin, forward) = self.camera_ray()?;
        self.raycast(origin, forward, max_distance)
    }

    /// The named object's world `[right, up, forward]` unit vectors, from
    /// its transform and its parents' as they are now. Forward is -Z.
    /// Rotations apply X first, then Y, then Z.
    #[must_use]
    pub fn basis(&mut self, name: &str) -> Option<[[f32; 3]; 3]> {
        let entity = find_named_entity(self.world, name)?;
        let matrix = world_matrix(self.world, entity);
        let axis = |v: nalgebra::Vector3<f32>| -> [f32; 3] {
            matrix.transform_vector(&v).normalize().into()
        };
        Some([
            axis(nalgebra::Vector3::x()),
            axis(nalgebra::Vector3::y()),
            axis(-nalgebra::Vector3::z()),
        ])
    }

    /// The active camera's world position and unit forward direction, from
    /// its transforms as they are now: where a thrown or fired object
    /// starts and which way it goes. `None` without an active camera.
    #[must_use]
    pub fn camera_ray(&mut self) -> Option<([f32; 3], [f32; 3])> {
        let (camera, _) = self.active_camera()?;
        let matrix = world_matrix(self.world, camera);
        let forward = matrix
            .transform_vector(&-nalgebra::Vector3::z())
            .normalize();
        let origin = matrix.transform_point(&nalgebra::Point3::origin());
        Some((origin.coords.into(), forward.into()))
    }

    /// The world ray under the mouse cursor, from the active camera: where
    /// a top-down or point-and-click game aims. Pass it to
    /// [`Self::raycast`] to find the ground point under the cursor. `None`
    /// before the cursor has entered the window or without an active
    /// camera. A player controller with `mouse_look` captures the cursor,
    /// so click through [`Self::aim`] in a first-person view instead.
    #[must_use]
    pub fn pointer_ray(&mut self) -> Option<([f32; 3], [f32; 3])> {
        let input = self.world.resource::<RuntimeInput>();
        let (cursor, size) = (input.cursor_position()?, input.viewport_size());
        if size.contains(&0.0) {
            return None;
        }
        let (entity, camera) = self.active_camera()?;
        let transform = crate::runtime::GlobalTransform {
            matrix: world_matrix(self.world, entity).into(),
        };
        let ray = crate::runtime::picking::scene_ray(
            cursor, [0.0; 2], size, camera, transform,
        )?;
        Some((ray.origin.into(), ray.direction.into()))
    }

    /// Where the world `point` shows in the active camera's view, as
    /// fractions from the top-left corner (`[0.5, 0.5]` is the center, as a
    /// scenario `pointer` step takes it). `None` when the point is behind
    /// the camera, nearer than its near plane, past its far plane or outside
    /// the view, or without an active camera. A wall in front does not
    /// count: cast [`Self::raycast_visible`] toward the point for that.
    #[must_use]
    pub fn on_screen(&mut self, point: [f32; 3]) -> Option<[f32; 2]> {
        let size = self.viewport_size();
        let (entity, camera) = self.active_camera()?;
        let transform = crate::runtime::GlobalTransform {
            matrix: world_matrix(self.world, entity).into(),
        };
        let clip =
            crate::runtime::picking::clip_from_world(size, camera, transform)?
                * nalgebra::Vector4::new(point[0], point[1], point[2], 1.0);
        if clip.w <= f32::EPSILON {
            return None;
        }
        let ndc = clip.xyz() / clip.w;
        let inside = |v: f32, low: f32| (low..=1.0).contains(&v);
        (inside(ndc.x, -1.0) && inside(ndc.y, -1.0) && inside(ndc.z, 0.0))
            // Vulkan puts the top of the view at NDC y = -1.
            .then(|| [(ndc.x + 1.0) * 0.5, (ndc.y + 1.0) * 0.5])
    }

    /// Makes the named camera the only active one, so it renders and aims.
    /// Returns false, changing nothing, when no camera has that name.
    pub fn set_active_camera(&mut self, name: &str) -> bool {
        let Some(chosen) =
            find_named_entity(self.world, name).filter(|entity| {
                self.world.get::<crate::runtime::Camera>(*entity).is_some()
            })
        else {
            return false;
        };
        let mut cameras =
            self.world.query::<(Entity, &mut crate::runtime::Camera)>();
        for (entity, mut camera) in cameras.iter_mut(self.world) {
            let active = entity == chosen;
            if camera.active != active {
                camera.active = active;
            }
        }
        true
    }

    /// Sets the vertical field of view, in radians, of the named
    /// perspective camera, for a zoom or a sprint effect. Returns false,
    /// changing nothing, when no camera has that name or it is
    /// orthographic.
    pub fn set_camera_fov(
        &mut self,
        name: &str,
        vertical_fov_radians: f32,
    ) -> bool {
        let Some(mut camera) =
            find_named_entity(self.world, name).and_then(|entity| {
                self.world.get_mut::<crate::runtime::Camera>(entity)
            })
        else {
            return false;
        };
        match &mut camera.projection {
            crate::runtime::Projection::Perspective {
                vertical_fov_radians: fov,
                ..
            } => {
                *fov = vertical_fov_radians;
                true
            }
            crate::runtime::Projection::Orthographic { .. } => false,
        }
    }

    /// The vertical field of view, in radians, of the named perspective
    /// camera; `None` when there is no such camera or it is orthographic.
    pub fn camera_fov(&mut self, name: &str) -> Option<f32> {
        let entity = find_named_entity(self.world, name)?;
        match self.world.get::<crate::runtime::Camera>(entity)?.projection {
            crate::runtime::Projection::Perspective {
                vertical_fov_radians,
                ..
            } => Some(vertical_fov_radians),
            crate::runtime::Projection::Orthographic { .. } => None,
        }
    }

    /// Turns the named camera on or off and sets the part of the window it
    /// draws into (`[x, y, width, height]` fractions, `None` for all of
    /// it), leaving other cameras alone; two active cameras with
    /// viewports make split screen. Returns false when no camera has that
    /// name.
    pub fn set_camera(
        &mut self,
        name: &str,
        active: bool,
        viewport: Option<[f32; 4]>,
    ) -> bool {
        let Some(mut camera) =
            find_named_entity(self.world, name).and_then(|entity| {
                self.world.get_mut::<crate::runtime::Camera>(entity)
            })
        else {
            return false;
        };
        camera.active = active;
        camera.viewport = viewport;
        true
    }

    /// The camera that aims: the highest-priority active one, preferring
    /// one that fills the window.
    fn active_camera(&mut self) -> Option<(Entity, crate::runtime::Camera)> {
        crate::runtime::active_camera(self.world)
    }

    /// Removes the named object and its children. Returns false when no
    /// object has that name.
    pub fn despawn(&mut self, name: &str) -> bool {
        let Some(entity) = find_named_entity(self.world, name) else {
            return false;
        };
        let _ = rusting_core::hierarchy::clear_parent(self.world, entity);
        crate::runtime::despawn_tree(self.world, entity);
        true
    }

    /// Copies the named object and its children, with every component, as a
    /// new object called `name` at local `position` under the same parent.
    /// Use it to spawn enemies or projectiles from an object authored in the
    /// scene: keep that template hidden and out of the way, then show each
    /// copy with [`Self::set_visible`]. A copied child is named
    /// `"<name>/<child name>"`, so names stay unique. Returns `None` and
    /// warns once, with the nearest names, when `template` does not exist.
    ///
    /// # Panics
    ///
    /// Panics if an object called `name`, or one of the copied children's
    /// names, already exists.
    pub fn spawn_copy(
        &mut self,
        template: &str,
        name: impl Into<String>,
        position: [f32; 3],
    ) -> Option<Entity> {
        let name = name.into();
        let template = find_or_warn(self.world, template)?;
        let copy = copy_tree(self.world, template, Some(&name));
        if let Some(mut transform) = self.world.get_mut::<Transform>(copy) {
            transform.position = position;
        }
        if let Some(parent) = self
            .world
            .get::<crate::runtime::Parent>(template)
            .map(|p| p.0)
        {
            let _ =
                rusting_core::hierarchy::set_parent(self.world, copy, parent);
        }
        Some(copy)
    }

    /// Like [`Self::spawn_copy`], but the copy has no parent and takes the
    /// whole `transform` (position, rotation and scale) in world space, so a
    /// template kept under a hidden, offset root does not pass that root's
    /// visibility or offset on to its copies.
    ///
    /// # Panics
    ///
    /// Panics if an object called `name`, or one of the copied children's
    /// names, already exists.
    pub fn spawn_copy_at_root(
        &mut self,
        template: &str,
        name: impl Into<String>,
        transform: Transform,
    ) -> Option<Entity> {
        let name = name.into();
        let template = find_or_warn(self.world, template)?;
        let copy = copy_tree(self.world, template, Some(&name));
        self.world.entity_mut(copy).insert(transform);
        Some(copy)
    }

    /// A spawner with a cap: copies `template` as [`Self::spawn_copy`]
    /// does, named `"<template> <n>"` with the lowest free `n` from 1, while
    /// fewer than `limit` such copies exist. At the cap it spawns nothing,
    /// adds 1 to the counter `"<template> refused"` and warns once, so a
    /// full spawner never drops silently. Returns the copy's name.
    pub fn spawn_numbered(
        &mut self,
        template: &str,
        position: [f32; 3],
        limit: usize,
    ) -> Option<String> {
        let prefix = format!("{template} ");
        let mut taken = vec![false; limit];
        for name in self.world.query::<&Name>().iter(self.world) {
            if let Some(n) = name.0.strip_prefix(&prefix) {
                if let Some(slot) = n.parse::<usize>().ok().and_then(|n| {
                    n.checked_sub(1).and_then(|i| taken.get_mut(i))
                }) {
                    *slot = true;
                }
            }
        }
        let Some(free) = taken.iter().position(|taken| !taken) else {
            let counter = format!("{template} refused");
            if self.counter_or(&counter, 0) == 0 {
                eprintln!(
                    "warning: `{template}` is at its limit of {limit} copies; \
                     the counter `{counter}` counts refused spawns"
                );
            }
            self.add_to_counter(&counter, 1);
            return None;
        };
        let name = format!("{prefix}{}", free + 1);
        self.spawn_copy(template, name.as_str(), position)?;
        Some(name)
    }

    /// Places the scene file `path` (relative to the project's `assets`
    /// folder, such as `"prefabs/tile.rscene"`) as a prefab: a new root
    /// object called `name` at `transform`, with the file's objects as its
    /// children under their own names. The file is read once per path.
    /// Returns `None` and warns when the file does not load.
    pub fn spawn_prefab(
        &mut self,
        path: &str,
        name: impl Into<String>,
        transform: Transform,
    ) -> Option<Entity> {
        let file = self.assets_folder().join(path);
        let placed = self
            .world
            .resource_mut::<crate::AssetServer>()
            .load_prefab(&file)
            .map_err(|error| error.to_string())
            .and_then(|prefab| {
                crate::runtime::spawn_prefab(self.world, prefab, transform)
                    .map_err(|error| error.to_string())
            });
        match placed {
            Ok(root) => {
                self.world.entity_mut(root).insert(Name(name.into()));
                Some(root)
            }
            Err(error) => {
                eprintln!("warning: prefab `{path}` was not placed: {error}");
                None
            }
        }
    }

    /// The name of `entity`, such as [`crate::runtime::PlayerController`]'s
    /// `wall` or `floor`; `None` when it is unnamed or gone.
    #[must_use]
    pub fn name_of(&self, entity: Entity) -> Option<String> {
        self.world.get::<Name>(entity).map(|name| name.0.clone())
    }

    /// Whether `entity` is in `class`; `false` when it is gone.
    #[must_use]
    pub fn has_class(&self, entity: Entity, class: &str) -> bool {
        self.world
            .get::<crate::runtime::ObjectClasses>(entity)
            .is_some_and(|classes| classes.contains(class))
    }

    /// Puts the named object in `class`, so a copy spawned from a
    /// classless template joins the objects game code loops over. Returns
    /// whether it was added: `false` when it already was in the class or
    /// does not exist.
    pub fn add_class(&mut self, name: &str, class: &str) -> bool {
        let Some(entity) = find_or_warn(self.world, name) else {
            return false;
        };
        let mut entity = self.world.entity_mut(entity);
        if !entity.contains::<crate::runtime::ObjectClasses>() {
            entity.insert(crate::runtime::ObjectClasses::default());
        }
        entity
            .get_mut::<crate::runtime::ObjectClasses>()
            .is_some_and(|mut classes| classes.add(class))
    }

    /// Takes the named object out of `class`, so loops over the class skip
    /// it while it stays in the scene. Returns whether it was removed:
    /// `false` when it was not in the class or does not exist.
    pub fn remove_class(&mut self, name: &str, class: &str) -> bool {
        let Some(entity) = find_or_warn(self.world, name) else {
            return false;
        };
        self.world
            .get_mut::<crate::runtime::ObjectClasses>(entity)
            .is_some_and(|mut classes| classes.remove(class))
    }

    /// Names of the objects in `class`, sorted, so game code can loop over
    /// spawned copies. Unnamed objects are left out.
    #[must_use]
    pub fn in_class(&mut self, class: &str) -> Vec<String> {
        let mut query = self
            .world
            .query::<(&Name, &crate::runtime::ObjectClasses)>();
        let mut names: Vec<_> = query
            .iter(self.world)
            .filter(|(_, classes)| classes.contains(class))
            .map(|(name, _)| name.0.clone())
            .collect();
        names.sort();
        names
    }

    /// A hash of the positions, rotations, scales and velocities of the
    /// named objects in `class`, in name order. Unlike the full state hash
    /// it leaves out the clock and everything else, so two rounds that
    /// leave the class in the same state hash the same at any tick. Equal
    /// floats bit for bit give equal hashes; any drift changes it.
    ///
    /// It reads every object in the class, so its cost grows with the
    /// class: milliseconds for tens of thousands of bodies. Call it on
    /// checkpoint ticks, not every frame.
    #[must_use]
    pub fn state_hash(&mut self, class: &str) -> u64 {
        let mut hasher = crate::runtime::StateHasher::default();
        for name in self.in_class(class) {
            let Some(entity) = find_named_entity(self.world, &name) else {
                continue;
            };
            std::fmt::Write::write_str(&mut hasher, &name).ok();
            if let Some(transform) = self.world.get::<Transform>(entity) {
                hasher.floats(&transform.position);
                hasher.floats(&transform.rotation);
                hasher.floats(&transform.scale);
            }
            if let Some(body) =
                self.world.get::<crate::runtime::RigidBody>(entity)
            {
                hasher.floats(&body.linear_velocity);
                hasher.floats(&body.angular_velocity);
            }
        }
        hasher.finish()
    }

    /// Moves the named object under `parent` (`None` makes it a root). Its
    /// `Transform` is kept as is, so it is now relative to the new parent:
    /// set its position after the call, such as a lantern's offset in a
    /// hand. False when an object is missing or the move would make a loop.
    pub fn reparent(&mut self, name: &str, parent: Option<&str>) -> bool {
        let Some(child) = find_named_entity(self.world, name) else {
            return false;
        };
        match parent {
            Some(parent) => {
                find_named_entity(self.world, parent).is_some_and(|parent| {
                    crate::runtime::hierarchy::set_parent(
                        self.world, child, parent,
                    )
                    .is_ok()
                })
            }
            None => crate::runtime::hierarchy::clear_parent(self.world, child)
                .is_ok(),
        }
    }

    /// Changes the point or spot light on the named object: its linear RGB
    /// `color`, `intensity` and `range` in metres, each kept when `None`.
    /// An intensity of 0 turns it off. False when it has no such light.
    pub fn set_light(
        &mut self,
        name: &str,
        color: Option<[f32; 3]>,
        intensity: Option<f32>,
        range: Option<f32>,
    ) -> bool {
        let Some(entity) = find_named_entity(self.world, name) else {
            return false;
        };
        let mut entity = self.world.entity_mut(entity);
        let apply = |light_color: &mut [f32; 3],
                     light_intensity: &mut f32,
                     light_range: &mut f32| {
            *light_color = color.unwrap_or(*light_color);
            *light_intensity = intensity.unwrap_or(*light_intensity);
            *light_range = range.unwrap_or(*light_range);
        };
        if let Some(mut light) = entity.get_mut::<crate::runtime::PointLight>()
        {
            let light = &mut *light;
            apply(&mut light.color, &mut light.intensity, &mut light.range);
            true
        } else if let Some(mut light) =
            entity.get_mut::<crate::runtime::SpotLight>()
        {
            let light = &mut *light;
            apply(&mut light.color, &mut light.intensity, &mut light.range);
            true
        } else {
            false
        }
    }

    /// Changes the named object's `rusting.hud` element: its text, size,
    /// color, anchor or offset. Returns false when it has none.
    ///
    /// ```ignore
    /// scene.set_hud("Judgement", |hud| {
    ///     hud.text = "PERFECT".into();
    ///     hud.color = [1.0, 0.8, 0.2, 1.0];
    ///     hud.font_size = 40.0;
    /// });
    /// ```
    pub fn set_hud(
        &mut self,
        name: &str,
        edit: impl FnOnce(&mut crate::runtime::HudElement),
    ) -> bool {
        let Some(entity) = find_named_entity(self.world, name) else {
            return false;
        };
        let Some(mut hud) =
            self.world.get_mut::<crate::runtime::HudElement>(entity)
        else {
            return false;
        };
        edit(&mut hud);
        true
    }

    /// Shows or hides the named object. Hidden objects still collide.
    pub fn set_visible(&mut self, name: &str, visible: bool) {
        if let Some(entity) = find_named_entity(self.world, name) {
            self.world
                .entity_mut(entity)
                .insert(crate::runtime::Visibility { visible });
        }
    }

    /// The named object's base color (RGBA, linear), if it has a mesh.
    #[must_use]
    pub fn color(&mut self, name: &str) -> Option<[f32; 4]> {
        let entity = find_named_entity(self.world, name)?;
        let handle = self.world.get::<MeshRenderer>(entity)?.material;
        let assets = self.world.get_resource::<AssetServer>()?;
        Some(assets.materials.get(handle)?.base_color)
    }

    /// Changes the named object's base color (RGBA, linear). Other objects
    /// that shared its material keep theirs.
    pub fn set_color(&mut self, name: &str, color: [f32; 4]) {
        self.edit_material(name, |material| material.base_color = color);
    }

    /// Changes the named object's emitted light (RGB, linear; above 1
    /// glows with bloom). Other objects that shared its material keep theirs.
    ///
    /// Cheap enough to ease hundreds of objects every tick: objects given
    /// the same color share one material, and materials no object shows any
    /// more are removed.
    pub fn set_emissive(&mut self, name: &str, emissive: [f32; 3]) {
        self.edit_material(name, |material| material.emissive = emissive);
    }

    /// Puts `material` on the named object, for example one made with
    /// [`Self::create_material`]. Other objects keep theirs.
    pub fn set_material(
        &mut self,
        name: &str,
        material: crate::assets::Handle<crate::assets::MaterialAsset>,
    ) {
        let Some(entity) = find_or_warn(self.world, name) else {
            return;
        };
        if let Some(mut renderer) = self.world.get_mut::<MeshRenderer>(entity) {
            renderer.material = material;
        }
    }

    /// Registers a texture, such as one from
    /// [`crate::text_texture::text_texture`], for use in a material.
    pub fn create_texture(
        &mut self,
        texture: crate::assets::TextureAsset,
    ) -> crate::assets::Handle<crate::assets::TextureAsset> {
        self.world
            .resource_mut::<AssetServer>()
            .textures
            .insert(texture)
    }

    /// Edits a texture from [`Self::create_texture`], to draw into it while
    /// the game runs (paper, a map, a monitor). Changing its pixels, or its
    /// size, uploads it again before the next frame. Returns false when the
    /// handle is unknown.
    pub fn edit_texture(
        &mut self,
        handle: crate::assets::Handle<crate::assets::TextureAsset>,
        edit: impl FnOnce(&mut crate::assets::TextureAsset),
    ) -> bool {
        let mut assets = self.world.resource_mut::<AssetServer>();
        let Some(texture) = assets.textures.get_mut(handle) else {
            return false;
        };
        edit(texture);
        true
    }

    /// Draws `text` into a texture and shows it as the named object's base
    /// color map, for signs, labels, paper and monitor overlays. Lines split
    /// on `\n`. Returns the texture size in texels, so the mesh can be
    /// scaled to `size[0] / size[1]`, or `None` when no object has the
    /// name. Use an Unlit material for glowing text. Each distinct text is
    /// drawn once and kept, so a clock that cycles through its strings
    /// costs one texture per string.
    #[cfg(feature = "ui")]
    pub fn set_text(
        &mut self,
        name: &str,
        text: &str,
        style: crate::text_texture::TextStyle,
    ) -> Option<[u32; 2]> {
        self.draw_text(name, text, style, None)
    }

    /// Like [`Self::set_text`], in a TTF or OTF font file relative to the
    /// project's `assets` folder (`"fonts/pencil.ttf"`). Returns `None`,
    /// with a warning, when the file is missing or not a font.
    #[cfg(feature = "ui")]
    pub fn set_text_in_font(
        &mut self,
        name: &str,
        text: &str,
        style: crate::text_texture::TextStyle,
        font: &str,
    ) -> Option<[u32; 2]> {
        self.draw_text(name, text, style, Some(font))
    }

    #[cfg(feature = "ui")]
    fn draw_text(
        &mut self,
        name: &str,
        text: &str,
        style: crate::text_texture::TextStyle,
        font: Option<&str>,
    ) -> Option<[u32; 2]> {
        find_named_entity(self.world, name)?;
        let key = format!("{text}\u{0}{style:?}\u{0}{font:?}");
        let cached = self
            .world
            .get_resource::<TextTextures>()
            .and_then(|cache| cache.0.get(&key).copied());
        let texture = match cached {
            Some(texture) => texture,
            None => {
                let drawn = match font {
                    None => crate::text_texture::text_texture(text, style),
                    Some(font) => {
                        let path = self.assets_folder().join(font);
                        let drawn =
                            std::fs::read(&path).ok().and_then(|bytes| {
                                crate::text_texture::text_texture_in_font(
                                    text, style, &bytes,
                                )
                            });
                        let Some(drawn) = drawn else {
                            eprintln!(
                                "warning: set_text_in_font: {} is not a \
                                 readable TTF or OTF font; the call did nothing.",
                                path.display()
                            );
                            return None;
                        };
                        drawn
                    }
                };
                let texture = self.create_texture(drawn);
                self.world
                    .get_resource_or_insert_with(TextTextures::default)
                    .0
                    .insert(key, texture);
                texture
            }
        };
        self.edit_material(name, |material| {
            material.base_color_texture = Some(texture);
        });
        self.world
            .resource::<AssetServer>()
            .textures
            .get(texture)
            .map(|texture| texture.size)
    }

    /// Gives the named object a changed copy of its material. Equal
    /// materials are shared, as the scene loader shares them, so switching
    /// between a few colors does not grow the material list.
    fn edit_material(
        &mut self,
        name: &str,
        edit: impl FnOnce(&mut crate::assets::MaterialAsset),
    ) {
        let Some(entity) = find_or_warn(self.world, name) else {
            return;
        };
        let Some(handle) = self
            .world
            .get::<MeshRenderer>(entity)
            .map(|renderer| renderer.material)
        else {
            return;
        };
        let Some(assets) = self.world.get_resource::<AssetServer>() else {
            return;
        };
        let Some(mut material) = assets.materials.get(handle).cloned() else {
            return;
        };
        edit(&mut material);
        let key = material_key(&material);
        let mut edited = std::mem::take(
            &mut *self.world.get_resource_or_init::<EditedMaterials>(),
        );
        let bucket = edited.by_key.entry(key).or_default();
        let mut assets = self.world.resource_mut::<AssetServer>();
        let shared = bucket
            .iter()
            .copied()
            .find(|handle| assets.materials.get(*handle) == Some(&material));
        let handle = shared.unwrap_or_else(|| {
            let handle = assets.materials.insert(material);
            bucket.push(handle);
            edited.count += 1;
            handle
        });
        if let Some(mut renderer) = self.world.get_mut::<MeshRenderer>(entity) {
            renderer.material = handle;
        }
        if edited.count >= edited.sweep_at.max(EditedMaterials::FIRST_SWEEP) {
            edited.sweep(self.world);
        }
        self.world.insert_resource(edited);
    }

    /// The character in the named `rusting.tile_map` cell under world
    /// `position` (x and y; z is ignored), `.` for an empty cell or one past
    /// the grid. `None` when the map does not exist or `position` is left of
    /// or above the map origin.
    #[must_use]
    pub fn tile(&mut self, map: &str, position: [f32; 3]) -> Option<char> {
        let entity = find_named_entity(self.world, map)?;
        let origin = self.world.get::<Transform>(entity)?.position;
        let map = self.world.get::<crate::runtime::TileMap>(entity)?;
        let (column, row) =
            map.cell_at([position[0] - origin[0], position[1] - origin[1]])?;
        Some(map.cell(column, row))
    }

    /// Writes `character` into the named tile map's cell under world
    /// `position`, growing the grid as needed; the map's tiles and colliders
    /// rebuild on the next fixed step. Returns false when the map does not
    /// exist, `position` is outside it, or the cell already held it.
    pub fn set_tile(
        &mut self,
        map: &str,
        position: [f32; 3],
        character: char,
    ) -> bool {
        let Some(entity) = find_named_entity(self.world, map) else {
            return false;
        };
        let Some(origin) =
            self.world.get::<Transform>(entity).map(|t| t.position)
        else {
            return false;
        };
        let Some(mut map) =
            self.world.get_mut::<crate::runtime::TileMap>(entity)
        else {
            return false;
        };
        let local = [position[0] - origin[0], position[1] - origin[1]];
        let Some((column, row)) = map.cell_at(local) else {
            return false;
        };
        // Compare first so an unchanged cell does not mark the map changed.
        map.cell(column, row) != character
            && map.set_cell(column, row, character)
    }

    /// Fires the named object's `rusting.sound_cue` and
    /// `rusting.burst_emitter` and restarts its `rusting.particle_emitter`,
    /// the way a pickup does when collected.
    pub fn trigger(&mut self, name: &str) {
        let Some(entity) = find_or_warn(self.world, name) else {
            return;
        };
        if let Some(mut cue) =
            self.world.get_mut::<crate::runtime::SoundCue>(entity)
        {
            cue.trigger();
        }
        if let Some(mut emitter) =
            self.world.get_mut::<crate::runtime::BurstEmitter>(entity)
        {
            emitter.trigger();
        }
        self.particles(name, crate::runtime::ParticleCommand::Restart);
    }

    /// Squashes the named object by `amount` (positive flattens, negative
    /// stretches) through its `rusting.squash`, giving it a default one
    /// first if it has none.
    pub fn squash(&mut self, name: &str, amount: f32) {
        let Some(entity) = find_or_warn(self.world, name) else {
            return;
        };
        let mut entity = self.world.entity_mut(entity);
        if !entity.contains::<crate::runtime::Squash>() {
            entity.insert(crate::runtime::Squash::default());
        }
        if let Some(mut squash) = entity.get_mut::<crate::runtime::Squash>() {
            squash.squash(amount);
        }
    }

    /// Moves one `Transform` property of the named object from its current
    /// value to `to` over `seconds` of game time, then holds it there, like
    /// Godot's `tween_property`. It replaces any tween the object had, so
    /// calling it again mid-way turns toward the new target. Use it on
    /// objects without a dynamic body (a sliding door, a coin flying to the
    /// HUD); rotation is in radians. Returns `false` when the object does
    /// not exist.
    pub fn tween(
        &mut self,
        name: &str,
        property: crate::runtime::TweenProperty,
        to: [f32; 3],
        seconds: f32,
        easing: crate::runtime::Easing,
    ) -> bool {
        use crate::runtime::TweenProperty;
        let Some(entity) = find_or_warn(self.world, name) else {
            return false;
        };
        let mut entity = self.world.entity_mut(entity);
        let transform = entity.get::<Transform>().copied().unwrap_or_default();
        let from = match property {
            TweenProperty::Position => transform.position,
            TweenProperty::Rotation => transform.rotation,
            TweenProperty::Scale => transform.scale,
        };
        entity.insert(crate::runtime::Tween {
            property,
            from,
            to,
            duration: seconds.max(f32::EPSILON),
            delay: 0.0,
            easing,
            repeat: crate::runtime::TweenRepeat::Once,
            elapsed: 0.0,
        });
        true
    }

    /// Flashes the named object and its children through its
    /// `rusting.flash`, giving it a default white one first if it has none.
    pub fn flash(&mut self, name: &str) {
        let Some(entity) = find_or_warn(self.world, name) else {
            return;
        };
        let mut entity = self.world.entity_mut(entity);
        if !entity.contains::<crate::runtime::Flash>() {
            entity.insert(crate::runtime::Flash::default());
        }
        if let Some(mut flash) = entity.get_mut::<crate::runtime::Flash>() {
            flash.flash();
        }
    }

    /// Freezes the whole game for `seconds` (0 to 1) of real time on a
    /// heavy hit. Fixed ticks then carry on as if nothing happened, so
    /// simulation results and headless runs are unchanged.
    pub fn hit_stop(&mut self, seconds: f32) {
        self.world
            .resource_mut::<crate::runtime::HitStop>()
            .stop(seconds);
    }

    /// Adds trauma (0 to 1) to the named camera's `rusting.camera_shake`,
    /// giving it a default one first if it has none.
    pub fn add_trauma(&mut self, name: &str, amount: f32) {
        let Some(entity) = find_or_warn(self.world, name) else {
            return;
        };
        let mut entity = self.world.entity_mut(entity);
        if !entity.contains::<crate::runtime::CameraShake>() {
            entity.insert(crate::runtime::CameraShake::default());
        }
        if let Some(mut shake) = entity.get_mut::<crate::runtime::CameraShake>()
        {
            shake.add_trauma(amount);
        }
    }

    /// Plays, pauses, stops or restarts the named object's
    /// `rusting.particle_emitter` on the next fixed tick. `Stop` lets live
    /// particles finish.
    pub fn particles(
        &mut self,
        name: &str,
        command: crate::runtime::ParticleCommand,
    ) {
        let Some(entity) = find_or_warn(self.world, name) else {
            return;
        };
        if let Some(mut emitter) = self
            .world
            .get_mut::<crate::runtime::ParticleEmitter>(entity)
        {
            emitter.command = command;
        }
    }

    /// Plays the named object's `rusting.animation` clip from its start on
    /// the next fixed tick.
    pub fn play_animation(&mut self, name: &str, clip: &str) {
        self.animate(name, crate::runtime::AnimationCommand::Play(clip.into()));
    }

    /// Blends the named object from its current clip into `clip` over
    /// `seconds`.
    pub fn crossfade(&mut self, name: &str, clip: &str, seconds: f32) {
        self.animate(
            name,
            crate::runtime::AnimationCommand::Crossfade(clip.into(), seconds),
        );
    }

    /// Stops the named object's animation, holding its current pose.
    pub fn stop_animation(&mut self, name: &str) {
        self.animate(name, crate::runtime::AnimationCommand::Stop);
    }

    /// Sets how fast the named object's animation plays; 1 is real time.
    pub fn set_animation_speed(&mut self, name: &str, speed: f32) {
        let Some(entity) = find_or_warn(self.world, name) else {
            return;
        };
        if let Some(mut animation) =
            self.world.get_mut::<crate::runtime::Animation>(entity)
        {
            animation.speed = speed.max(0.0);
        }
    }

    /// Makes the named character's `rusting.ragdoll` go limp (`true`) or
    /// get back up (`false`) next fixed step.
    pub fn set_ragdoll(&mut self, name: &str, limp: bool) {
        let Some(entity) = find_or_warn(self.world, name) else {
            return;
        };
        if let Some(mut ragdoll) =
            self.world.get_mut::<crate::runtime::Ragdoll>(entity)
        {
            ragdoll.command = Some(limp);
        }
    }

    /// Gives the named character a `rusting.ragdoll` whose bones come from
    /// the joints of the `Skin`s under it (see
    /// [`crate::runtime::ragdoll_bones`]), sharing `total_mass` kg. Keeps
    /// the other settings of a ragdoll it already has. Returns the bone
    /// count; 0 when the object is missing or has no skinned joints.
    pub fn generate_ragdoll(&mut self, name: &str, total_mass: f32) -> usize {
        let Some(entity) = find_or_warn(self.world, name) else {
            return 0;
        };
        let bones =
            crate::runtime::ragdoll_bones(self.world, entity, total_mass);
        let count = bones.len();
        let mut ragdoll = self
            .world
            .get::<crate::runtime::Ragdoll>(entity)
            .cloned()
            .unwrap_or_default();
        ragdoll.bones = bones;
        self.world.entity_mut(entity).insert(ragdoll);
        count
    }

    /// Sets the named character's ragdoll muscle stiffness in Hz. Above 0
    /// makes it an active ragdoll; 0 lets it go limp and then return to
    /// plain animation.
    pub fn set_ragdoll_muscle(&mut self, name: &str, hz: f32) {
        let Some(entity) = find_or_warn(self.world, name) else {
            return;
        };
        if let Some(mut ragdoll) =
            self.world.get_mut::<crate::runtime::Ragdoll>(entity)
        {
            ragdoll.muscle = hz.max(0.0);
        }
    }

    /// Drops the named character's ragdoll bodies and puts its bones back on
    /// their animated pose. An active ragdoll (`muscle` above 0) respawns
    /// its bodies on the next tick, at rest on the bones. Call it right
    /// after `set_position` to teleport a ragdoll without its limbs
    /// trailing behind; a limp character stands up at once. False when the
    /// object has no ragdoll.
    pub fn reset_ragdoll(&mut self, name: &str) -> bool {
        find_named_entity(self.world, name).is_some_and(|entity| {
            crate::runtime::reset_ragdoll(self.world, entity)
        })
    }

    /// Whether the named character is limp (not animated or blending back).
    #[must_use]
    pub fn is_limp(&mut self, name: &str) -> bool {
        find_named_entity(self.world, name)
            .and_then(|entity| {
                self.world.get::<crate::runtime::RagdollState>(entity)
            })
            .is_some_and(|state| {
                state.phase == crate::runtime::RagdollPhase::Limp
            })
    }

    /// Sets a parameter the named object's animation transitions test,
    /// such as `speed` or `grounded`. The next fixed tick follows the first
    /// transition that matches.
    pub fn set_animation_parameter(
        &mut self,
        name: &str,
        parameter: &str,
        value: f32,
    ) {
        let Some(entity) = find_or_warn(self.world, name) else {
            return;
        };
        if let Some(mut animation) =
            self.world.get_mut::<crate::runtime::Animation>(entity)
        {
            animation.parameters.insert(parameter.into(), value);
        }
    }

    fn animate(
        &mut self,
        name: &str,
        command: crate::runtime::AnimationCommand,
    ) {
        let Some(entity) = find_or_warn(self.world, name) else {
            return;
        };
        if let Some(mut animation) =
            self.world.get_mut::<crate::runtime::Animation>(entity)
        {
            animation.command = Some(command);
        }
    }

    /// True while the named object plays `clip`. A `Once` clip stops
    /// playing at its end.
    #[must_use]
    pub fn is_playing(&mut self, name: &str, clip: &str) -> bool {
        let Some(entity) = find_named_entity(self.world, name) else {
            return false;
        };
        let (Some(animation), Some(player)) = (
            self.world.get::<crate::runtime::Animation>(entity),
            self.world.get::<crate::runtime::AnimationPlayer>(entity),
        ) else {
            return false;
        };
        player.playing
            && player.current.is_some_and(|current| {
                Some(current.clip) == animation.clip(clip)
            })
    }

    /// Root motion the object's clips produced since the last call, in its
    /// local frame, and resets it. Needs `root_motion` set on its
    /// animation; `InPlace` leaves applying it to game code.
    pub fn take_root_motion(&mut self, name: &str) -> [f32; 3] {
        find_named_entity(self.world, name)
            .and_then(|entity| {
                self.world
                    .get_mut::<crate::runtime::AnimationPlayer>(entity)
            })
            .map(|mut player| std::mem::take(&mut player.root_motion))
            .unwrap_or_default()
    }

    /// Despawns loose dynamic CPU bodies that fall below `y`, so a rolled-away
    /// barrel does not fall forever; `None` turns the kill plane off.
    pub fn set_kill_y(&mut self, y: Option<f32>) {
        self.world
            .resource_mut::<crate::runtime::PhysicsSettings>()
            .kill_y = y;
    }

    /// Splits each CPU physics step into `substeps` (at least 1). More
    /// substeps hold long joint chains and heavy-on-light stacks tighter, at
    /// that many times the solver cost.
    pub fn set_physics_substeps(&mut self, substeps: u32) {
        self.world
            .resource_mut::<crate::runtime::PhysicsSettings>()
            .substeps = substeps.max(1);
    }

    /// Locks every GPU body's motion along or around world axes;
    /// `AxisLock::PLANE_XY` keeps a 2D game's GPU bodies in its plane.
    pub fn set_gpu_axis_lock(&mut self, locks: crate::runtime::AxisLock) {
        self.world
            .resource_mut::<crate::runtime::PhysicsSettings>()
            .gpu_locks = locks;
    }

    /// Names of the bodies the kill plane despawned since the last frame,
    /// in body order. Unnamed bodies are left out.
    #[must_use]
    pub fn fell_out(&self) -> Vec<String> {
        self.world
            .resource::<EventQueue<crate::runtime::FellOut>>()
            .iter()
            .filter_map(|event| event.name.clone())
            .collect()
    }

    /// Animation markers passed since the last frame, in tick order.
    #[must_use]
    pub fn animation_events(&self) -> Vec<crate::runtime::AnimationEvent> {
        self.world
            .resource::<EventQueue<crate::runtime::AnimationEvent>>()
            .iter()
            .cloned()
            .collect()
    }

    /// Names of the objects touching `name` in the last physics step, sensors
    /// included. Empty when `name` does not exist.
    #[must_use]
    pub fn touching(&mut self, name: &str) -> Vec<String> {
        let Some(entity) = find_named_entity(self.world, name) else {
            return Vec::new();
        };
        let Some(physics) =
            self.world.get_resource::<crate::runtime::PhysicsWorld>()
        else {
            return Vec::new();
        };
        let mut others: Vec<Entity> = physics
            .contacts()
            .iter()
            .filter_map(|contact| match (contact.a, contact.b) {
                (a, other) | (other, a) if a == entity => Some(other),
                _ => None,
            })
            .collect();
        // A player controller stops a skin short of what it stands on or
        // walks into, so the solver never lists those contacts.
        if let Some(player) =
            self.world.get::<crate::runtime::PlayerController>(entity)
        {
            for other in player
                .floor
                .map(|(floor, _)| floor)
                .into_iter()
                .chain(player.wall)
            {
                if !others.contains(&other) {
                    others.push(other);
                }
            }
        }
        others
            .into_iter()
            .filter_map(|other| self.world.get::<Name>(other))
            .map(|other| other.0.clone())
            .collect()
    }

    /// Sets a field of object `name` by the JSON pointer that scenario
    /// `set` steps use: `/transform/position/1`, `/point_light/intensity`,
    /// or `/components/rusting.fog/density` for any registered component
    /// (`rusting.color_grading`, `rusting.player_controller`, ...). The
    /// error names the object, path and problem.
    pub fn set_field(
        &mut self,
        name: &str,
        path: &str,
        value: serde_json::Value,
    ) -> Result<(), String> {
        let set = crate::scenario::Assignment {
            entity: name.to_owned(),
            counter: None,
            path: path.to_owned(),
            value,
        };
        crate::scenario::assign(self.world, &set).map(|_| ())
    }

    /// The player controller on a named object, with its live state:
    /// `grounded`, `velocity` (what the body really moved per second last
    /// step), `vertical_speed`, `yaw` and `pitch`. `None` when the object or
    /// its controller does not exist.
    #[must_use]
    pub fn player(
        &mut self,
        name: &str,
    ) -> Option<crate::runtime::PlayerController> {
        let entity = find_named_entity(self.world, name)?;
        self.world
            .get::<crate::runtime::PlayerController>(entity)
            .copied()
    }

    /// Edits the player controller on a named object, such as its
    /// `walk_speed`, `sprint_multiplier` or `jump_speed`; the change applies
    /// from the next step. Returns false when the object or its controller
    /// does not exist.
    pub fn set_player(
        &mut self,
        name: &str,
        edit: impl FnOnce(&mut crate::runtime::PlayerController),
    ) -> bool {
        let Some(entity) = find_named_entity(self.world, name) else {
            return false;
        };
        let Some(mut player) = self
            .world
            .get_mut::<crate::runtime::PlayerController>(entity)
        else {
            return false;
        };
        edit(&mut player);
        true
    }

    /// Tries to return a scene object by name.
    pub fn try_object(&mut self, name: &str) -> Option<GameObject<'_>> {
        let entity = find_named_entity(self.world, name)?;
        self.world
            .get_mut::<Transform>(entity)
            .map(|transform| GameObject { entity, transform })
    }

    /// Adds one GPU condition to a named object if it is not already present.
    ///
    /// This method is safe to call from the short update function every frame.
    pub fn watch_gpu_object(&mut self, name: &str, rule: GpuPhysicsRule) {
        let entity = find_named_entity(self.world, name).unwrap_or_else(|| {
            panic!(
                "scene object `{name}` does not exist{}",
                nearest_names_hint(self.world, name)
            )
        });
        if let Some(mut watch) = self.world.get_mut::<GpuPhysicsWatch>(entity) {
            if !watch.rules.contains(&rule) {
                watch.rules.push(rule);
            }
        } else {
            self.world
                .entity_mut(entity)
                .insert(GpuPhysicsWatch { rules: vec![rule] });
        }
    }

    /// Adds one GPU condition to every GPU body in the requested class.
    ///
    /// Objects receive classes in the editor Inspector or through the
    /// [`crate::runtime::ObjectClasses`] component. One object may have several
    /// classes, but the same rule is never added to it twice.
    /// This method is safe to call from the short update function every frame.
    pub fn watch_gpu_class(&mut self, class: &str, rule: GpuPhysicsRule) {
        self.world
            .resource_mut::<GpuPhysicsClassWatches>()
            .add(class, rule);
    }

    /// Replaces the custom GLSL that runs over every GPU body after each
    /// fixed step; see `rusting docs show guide/gpu-condition-shaders`.
    /// Calling it every frame with new `params` and the same `glsl` does not
    /// recompile.
    pub fn set_gpu_condition_shaders(
        &mut self,
        shaders: Vec<crate::runtime::GpuConditionShader>,
    ) {
        self.world
            .insert_resource(crate::runtime::GpuConditionShaders(shaders));
    }

    /// Returns GPU physics events with the requested registered name.
    #[must_use]
    pub fn gpu_events(&self, name: &str) -> Vec<GpuPhysicsEvent> {
        let Some(event_id) = self.world.resource::<GpuEventRegistry>().id(name)
        else {
            return Vec::new();
        };
        self.world
            .resource::<EventQueue<GpuPhysicsEvent>>()
            .iter()
            .filter(|event| event.event_id == event_id)
            .copied()
            .collect()
    }

    /// Where the GPU last put a GPU body, one to three frames late. The
    /// object's `Transform` keeps its spawn pose. Filled every physics frame
    /// for bodies with `sync: PhysicsSyncMode::SelectedState`, or once after
    /// `request_gpu_class_snapshot(scene.world(), class)`; `None` before
    /// that.
    #[must_use]
    pub fn gpu_state(
        &mut self,
        name: &str,
    ) -> Option<crate::runtime::GpuStateMirror> {
        let entity = find_named_entity(self.world, name)?;
        self.world
            .get::<crate::runtime::GpuStateMirror>(entity)
            .copied()
    }

    /// Counts the GPU bodies in object class `class` whose mirrored
    /// position lies inside the box from `min` to `max` (world meters,
    /// edges included). It reads each body's latest
    /// [`crate::runtime::GpuStateMirror`], so call
    /// `request_gpu_class_snapshot(scene.world(), class)` first and count
    /// a few frames later; bodies with no mirror yet are not counted.
    #[must_use]
    pub fn count_gpu_bodies_in_box(
        &mut self,
        class: &str,
        min: [f32; 3],
        max: [f32; 3],
    ) -> usize {
        self.world
            .query::<(
                &crate::runtime::ObjectClasses,
                &crate::runtime::GpuStateMirror,
            )>()
            .iter(self.world)
            .filter(|(classes, mirror)| {
                let at = mirror.transform.position;
                classes.contains(class)
                    && (0..3)
                        .all(|axis| (min[axis]..=max[axis]).contains(&at[axis]))
            })
            .count()
    }

    /// Sends a command to the GPU body named `name`: move it, set its
    /// velocity, push it, or ask for its state. It applies before the next
    /// GPU tick. Returns `false` when no such body has a GPU physics id
    /// yet (the engine assigns ids after the frame a body spawns).
    ///
    /// ```ignore
    /// scene.gpu_command("Ball#7", GpuBodyCommand::Teleport(Transform::new([0.0, 5.0, 0.0])));
    /// scene.gpu_command("Ball#7", GpuBodyCommand::SetVelocity { linear: [0.0; 3], angular: [0.0; 3] });
    /// ```
    pub fn gpu_command(
        &mut self,
        name: &str,
        command: crate::runtime::GpuBodyCommand,
    ) -> bool {
        let id = find_named_entity(self.world, name).and_then(|entity| {
            self.world
                .get_resource::<crate::runtime::PhysicsIdRegistry>()?
                .id_for(entity)
        });
        let Some(id) = id else {
            return false;
        };
        self.world
            .get_resource_or_insert_with(
                crate::runtime::GpuPhysicsCommands::default,
            )
            .push(id, command);
        true
    }
}

/// Mutable high-level access to one scene object's transform.
pub struct GameObject<'world> {
    entity: Entity,
    /// Transform borrowed from the real ECS object.
    transform: Mut<'world, Transform>,
}

impl GameObject<'_> {
    /// The ECS entity, for components this API does not wrap; reach them
    /// through [`GameScene::world`] after this borrow ends.
    #[must_use]
    pub fn entity(&self) -> Entity {
        self.entity
    }

    /// Returns the current local X, Y, and Z position.
    #[must_use]
    pub fn position(&self) -> [f32; 3] {
        self.transform.position
    }

    /// Replaces the local X, Y, and Z position.
    pub fn set_position(&mut self, position: [f32; 3]) -> &mut Self {
        self.transform.position = position;
        self
    }

    /// Adds an X, Y, and Z offset to the current position.
    pub fn move_by(&mut self, offset: [f32; 3]) -> &mut Self {
        for (position, offset) in self.transform.position.iter_mut().zip(offset)
        {
            *position += offset;
        }
        self
    }

    /// Moves the object along its X axis.
    pub fn move_x(&mut self, distance: f32) -> &mut Self {
        self.transform.position[0] += distance;
        self
    }

    /// Moves the object along its Y axis.
    pub fn move_y(&mut self, distance: f32) -> &mut Self {
        self.transform.position[1] += distance;
        self
    }

    /// Moves the object along its Z axis.
    pub fn move_z(&mut self, distance: f32) -> &mut Self {
        self.transform.position[2] += distance;
        self
    }

    /// Returns the local X, Y, and Z rotation in radians.
    #[must_use]
    pub fn rotation(&self) -> [f32; 3] {
        self.transform.rotation
    }

    /// Returns the local X, Y, and Z scale.
    #[must_use]
    pub fn scale(&self) -> [f32; 3] {
        self.transform.scale
    }

    /// Replaces the local X, Y, and Z rotation in radians.
    pub fn set_rotation(&mut self, rotation: [f32; 3]) -> &mut Self {
        self.transform.rotation = rotation;
        self
    }

    /// Turns the object so its forward axis (-Z) points at `target`, with
    /// no roll, like Godot's `look_at`. `target` is in the same space as
    /// `position()`, so siblings and root objects can face each other
    /// directly. Nothing changes when `target` is the object's position.
    pub fn look_at(&mut self, target: [f32; 3]) -> &mut Self {
        let at = self.transform.position;
        let [dx, dy, dz] = [0, 1, 2].map(|i| target[i] - at[i]);
        let flat = dx.hypot(dz);
        if flat == 0.0 && dy == 0.0 {
            return self;
        }
        self.transform.rotation = [dy.atan2(flat), (-dx).atan2(-dz), 0.0];
        self
    }

    /// Adds rotation in radians to all three axes.
    pub fn rotate_by(&mut self, rotation: [f32; 3]) -> &mut Self {
        for (current, rotation) in
            self.transform.rotation.iter_mut().zip(rotation)
        {
            *current += rotation;
        }
        self
    }

    /// Adds rotation in radians to X axis
    pub fn rotate_x(&mut self, rotation: f32) -> &mut Self {
        self.transform.rotation[0] += rotation;
        self
    }
    /// Adds rotation in radians to Y axis
    pub fn rotate_y(&mut self, rotation: f32) -> &mut Self {
        self.transform.rotation[1] += rotation;
        self
    }
    /// Adds rotation in radians to Y axis
    pub fn rotate_z(&mut self, rotation: f32) -> &mut Self {
        self.transform.rotation[2] += rotation;
        self
    }

    /// Replaces the local size on all three axes
    pub fn set_scale(&mut self, scale: [f32; 3]) -> &mut Self {
        self.transform.scale = scale;
        self
    }
}

/// Counter names game code read without such a counter, warned once each
/// per process. Only feeds warnings, never game state.
static MISSING_COUNTERS: std::sync::Mutex<std::collections::BTreeSet<String>> =
    std::sync::Mutex::new(std::collections::BTreeSet::new());

/// `hint` runs only for the first warning of a name, so a counter read
/// every tick does not search the names every tick.
fn warn_missing_counter(name: &str, hint: impl FnOnce() -> Option<String>) {
    let mut missing = MISSING_COUNTERS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if missing.insert(name.to_owned()) {
        let hint = hint().unwrap_or_default();
        eprintln!(
            "warning: game code read counter `{name}`, which does not \
             exist{hint}; it reads as 0. Add a `rusting.counter` named `{name}` to the \
             scene, create it with `set_counter` or `add_to_counter`, or \
             read it with `counter_or` to give a default without this \
             warning."
        );
    }
}

/// Object names that game code passed to a setter but that did not exist,
/// so each is warned about once.
static MISSING_OBJECTS: std::sync::Mutex<std::collections::BTreeSet<String>> =
    std::sync::Mutex::new(std::collections::BTreeSet::new());

/// [`find_named_entity`] for setters that return nothing, such as
/// `set_color` and `trigger`: a missing name warns once with the nearest
/// object names instead of doing nothing silently.
fn find_or_warn(world: &mut World, name: &str) -> Option<Entity> {
    let found = find_named_entity(world, name);
    if found.is_none() {
        let mut missing = MISSING_OBJECTS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if missing.insert(name.to_owned()) {
            let hint = nearest_names_hint(world, name);
            eprintln!(
                "warning: game code named object `{name}`, which does not \
                 exist{hint}; the call did nothing. Use `try_object` to \
                 check first."
            );
        }
    }
    found
}

/// Connects object names to ECS IDs after the first lookup.
///
/// This avoids searching every object again on later frames.
#[derive(Resource, Clone, Default)]
struct SceneNameIndex {
    /// Object names resolved once instead of searching 10,000 objects again.
    entities: HashMap<String, Entity>,
    /// True after names from the loaded scene were copied into this map.
    initialized: bool,
}

/// Materials made by [`GameScene::set_color`] and its siblings, by value, so
/// equal edits share one asset without searching every material. Easing a
/// color makes a new material each tick; [`Self::sweep`] removes the ones
/// no object shows any more.
#[derive(Resource, Clone, Default)]
struct EditedMaterials {
    by_key:
        HashMap<u64, Vec<crate::assets::Handle<crate::assets::MaterialAsset>>>,
    /// Materials in `by_key`.
    count: usize,
    /// Sweep when `count` reaches this.
    sweep_at: usize,
}

impl EditedMaterials {
    const FIRST_SWEEP: usize = 1024;

    /// Removes edited materials that no mesh renderer uses. Runs when the
    /// count doubles, so it costs O(1) per edit on average.
    fn sweep(&mut self, world: &mut World) {
        let used: HashSet<_> = world
            .query::<&MeshRenderer>()
            .iter(world)
            .map(|renderer| renderer.material)
            .collect();
        let mut assets = world.resource_mut::<AssetServer>();
        for bucket in self.by_key.values_mut() {
            bucket.retain(|handle| {
                used.contains(handle)
                    || assets.materials.remove(*handle).is_err()
            });
        }
        self.by_key.retain(|_, bucket| !bucket.is_empty());
        self.count = self.by_key.values().map(Vec::len).sum();
        self.sweep_at = (self.count * 2).max(Self::FIRST_SWEEP);
    }
}

/// A hash of the material fields edits change, for [`EditedMaterials`].
fn material_key(material: &crate::assets::MaterialAsset) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    material.name.hash(&mut hasher);
    for value in material.base_color.iter().chain(&material.emissive) {
        value.to_bits().hash(&mut hasher);
    }
    material.base_color_texture.hash(&mut hasher);
    material.emissive_texture.hash(&mut hasher);
    hasher.finish()
}

/// Builds the fast name index once after a scene is loaded.
fn ensure_scene_name_index(world: &mut World) {
    if world
        .get_resource::<SceneNameIndex>()
        .is_some_and(|index| index.initialized)
    {
        return;
    }
    let entries = {
        let mut query = world.query::<(Entity, &Name)>();
        query
            .iter(world)
            .map(|(entity, name)| (name.0.clone(), entity))
            .collect::<Vec<_>>()
    };
    let mut entities = HashMap::with_capacity(entries.len());
    for (name, entity) in entries {
        assert!(
            entities.insert(name.clone(), entity).is_none(),
            "scene object name `{name}` is not unique"
        );
    }
    world.insert_resource(SceneNameIndex {
        entities,
        initialized: true,
    });
}

/// `; did you mean `A`, `B`?` with the scene object names closest to `name`,
/// or an empty string when none is close.
fn nearest_names_hint(world: &mut World, name: &str) -> String {
    let mut names = world.query::<&Name>();
    nearest_hint(names.iter(world).map(|other| other.0.as_str()), name)
}

/// `; did you mean ...?` with the up to three `names` within a
/// case-insensitive edit distance of half `name`'s length (at least two).
pub(crate) fn nearest_hint<'a>(
    names: impl Iterator<Item = &'a str>,
    name: &str,
) -> String {
    let limit = (name.chars().count() / 2).max(2);
    let mut close: Vec<(usize, &str)> = names
        .filter_map(|other| {
            let distance = crate::scene_patch::edit_distance(
                &name.to_lowercase(),
                &other.to_lowercase(),
            );
            (distance <= limit).then_some((distance, other))
        })
        .collect();
    close.sort_unstable();
    close.dedup();
    close.truncate(3);
    if close.is_empty() {
        return String::new();
    }
    let list: Vec<String> = close
        .iter()
        .map(|(_, other)| format!("`{other}`"))
        .collect();
    format!("; did you mean {}?", list.join(", "))
}

/// Finds a named ECS object and saves the result for later calls.
/// Panics for a spawn whose name is taken, with the usual cause: the scene
/// file (or a template it came from) already has an object of that name.
fn name_taken(name: &str) -> ! {
    panic!(
        "scene object `{name}` already exists; object names are unique. If the \
         scene file declares it (templates ship names such as `Box 1` and \
         `Ball 1`), delete it there or spawn under another name"
    );
}

/// The entry of `table` that `unit`, in `[0, 1)`, falls in when each
/// entry takes its weight's share of the line.
fn weighted_pick<'a>(table: &[(&'a str, u32)], unit: f32) -> Option<&'a str> {
    let total: u64 = table.iter().map(|&(_, weight)| u64::from(weight)).sum();
    let mut left =
        ((f64::from(unit) * total as f64) as u64).min(total.checked_sub(1)?);
    table.iter().find_map(|&(item, weight)| {
        if left < u64::from(weight) {
            return Some(item);
        }
        left -= u64::from(weight);
        None
    })
}

/// The text [`GameScene::save_objects`] writes.
fn object_save(
    world: &mut World,
    version: u32,
    objects: &[&str],
    components: &[&str],
) -> String {
    let mut saved = serde_json::Map::new();
    for &object in objects {
        let Some(entity) = find_named_entity(world, object) else {
            continue;
        };
        let values = crate::runtime::registered_component_values(world, entity)
            .unwrap_or_default()
            .into_iter()
            .filter(|(name, _)| components.contains(&name.as_str()))
            .filter_map(|(name, text)| {
                Some((
                    name,
                    serde_json::from_str::<serde_json::Value>(&text).ok()?,
                ))
            })
            .collect::<serde_json::Map<_, _>>();
        saved.insert(object.to_owned(), values.into());
    }
    let save = serde_json::json!({"version": version, "objects": saved});
    format!("{save:#}\n")
}

/// Restores the components in `text` from [`object_save`] and returns its
/// version; `None`, restoring nothing, when it is not such a save.
fn apply_object_save(world: &mut World, text: &str) -> Option<u32> {
    let save: serde_json::Value = serde_json::from_str(text).ok()?;
    let version = u32::try_from(save["version"].as_u64()?).ok()?;
    let objects = save["objects"].as_object()?;
    if objects.values().any(|values| !values.is_object()) {
        return None;
    }
    for (object, values) in objects {
        let Some(entity) = find_or_warn(world, object) else {
            continue;
        };
        for (name, value) in values.as_object().into_iter().flatten() {
            let text = value.to_string();
            if let Err(error) = crate::runtime::set_registered_component(
                world, entity, name, &text,
            ) {
                eprintln!("warning: `{object}` {name} did not load: {error}");
            }
        }
    }
    Some(version)
}

fn find_named_entity(world: &mut World, name: &str) -> Option<Entity> {
    ensure_scene_name_index(world);
    let indexed = world
        .resource::<SceneNameIndex>()
        .entities
        .get(name)
        .copied()
        .filter(|entity| {
            world
                .get::<Name>(*entity)
                .is_some_and(|current| current.0 == name)
        });
    if indexed.is_some() {
        return indexed;
    }
    // Spawned or renamed through `world()`, which the index does not see.
    // ponytail: linear scan on a miss; track Name changes if misses get hot.
    let mut names = world.query::<(Entity, &Name)>();
    let entity = names
        .iter(world)
        .find(|(_, current)| current.0 == name)
        .map(|(entity, _)| entity)?;
    world
        .resource_mut::<SceneNameIndex>()
        .entities
        .insert(name.to_owned(), entity);
    Some(entity)
}

/// How an object was when the current scene loaded; see
/// [`GameScene::initial`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct InitialState {
    /// Local transform, relative to the parent if there is one.
    pub transform: Transform,
    /// Base color and emitted light of an inline material; `None` without
    /// a mesh or with a material file.
    pub color: Option<[f32; 4]>,
    pub emissive: Option<[f32; 3]>,
    pub body_kind: Option<crate::runtime::RigidBodyKind>,
    pub visible: bool,
}

/// What [`GameScene::raycast`] and [`GameScene::aim`] hit.
#[derive(Clone, Debug, PartialEq)]
pub struct RayHit {
    /// Scene name of the object hit; empty for an unnamed one.
    pub name: String,
    /// World position where the ray meets the surface.
    pub point: [f32; 3],
    /// Unit surface normal at `point`, facing the ray.
    pub normal: [f32; 3],
    /// Metres from the ray origin to `point`.
    pub distance: f32,
}

/// `entity`'s world matrix from the current transforms of it and its
/// parents.
fn world_matrix(world: &World, entity: Entity) -> nalgebra::Matrix4<f32> {
    let mut matrix = nalgebra::Matrix4::identity();
    let mut current = Some(entity);
    while let Some(entity) = current {
        if let Some(transform) = world.get::<Transform>(entity) {
            matrix = nalgebra::Matrix4::from(transform.to_matrix()) * matrix;
        }
        current = world
            .get::<crate::runtime::Parent>(entity)
            .map(|parent| parent.0);
    }
    matrix
}

/// Drops state that belonged to the replaced scene: the name index, the
/// `once` keys, and the solver's contacts, warm starts and sleep counters,
/// so physics after a reload repeats the first load exactly.
fn forget_old_scene(world: &mut World) {
    world.remove_resource::<SceneNameIndex>();
    world.remove_resource::<GameOnceState>();
    if world.contains_resource::<crate::runtime::PhysicsWorld>() {
        world.insert_resource(crate::runtime::PhysicsWorld::default());
    }
}

/// Clones `entity` and its descendants, naming the copy `name` (unnamed
/// when `None`). Each copy gets a new `SceneId`; named children become
/// `"<name>/<child>"`, parented to the copy. Panics when a copy's name is
/// taken, so the name index never points two names at one object.
fn copy_tree(world: &mut World, entity: Entity, name: Option<&str>) -> Entity {
    if let Some(name) = name {
        if find_named_entity(world, name).is_some() {
            name_taken(name);
        }
    }
    let copy =
        world
            .entity_mut(entity)
            .clone_and_spawn_with_opt_out(|builder| {
                builder.deny::<(
                    crate::runtime::Parent,
                    crate::runtime::Children,
                    crate::runtime::SceneId,
                    crate::runtime::Sleeping,
                    Name,
                )>();
            });
    let order = crate::runtime::next_spawn_order(world);
    world
        .entity_mut(copy)
        .insert((crate::runtime::SceneId::new(), order));
    if let Some(name) = name {
        world.entity_mut(copy).insert(Name(name.to_owned()));
        world
            .resource_mut::<SceneNameIndex>()
            .entities
            .insert(name.to_owned(), copy);
    }
    let children = world
        .get::<crate::runtime::Children>(entity)
        .map(|children| children.0.clone())
        .unwrap_or_default();
    for child in children {
        let child_name = world
            .get::<Name>(child)
            .zip(name)
            .map(|(child, parent)| format!("{parent}/{}", child.0));
        let child = copy_tree(world, child, child_name.as_deref());
        let _ = rusting_core::hierarchy::set_parent(world, child, copy);
    }
    copy
}

/// Signature used by the concise native Rust game update API.
pub type GameUpdate = for<'world> fn(&mut GameScene<'world>, &FrameTime);

/// Update function stored inside the ECS world.
#[derive(Resource, Clone, Copy)]
struct GameUpdateFunction(GameUpdate);

/// Tick function stored inside the ECS world.
#[derive(Resource, Clone, Copy)]
struct GameTickFunction(GameUpdate);

/// Input edges not yet given to a tick: from frames that ran no fixed tick,
/// or presses timed after the ticks a frame ran, held for the next tick so
/// [`GameScene::pressed`] inside a tick function misses no press.
#[derive(Resource, Clone, Default)]
struct TickInput {
    pending: RuntimeInput,
    /// A tick already ran this frame and took the frame's edges.
    ticked: bool,
}

/// Installs the easy game update function into the normal ECS schedule.
#[derive(Clone, Copy)]
struct SimpleGamePlugin {
    /// User function called once per rendered frame.
    update: GameUpdate,
    /// User function called once per fixed tick, before the frame's update.
    tick: Option<GameUpdate>,
    /// Registers the game's own scene components (`components:` in
    /// [`rusting_game!`]).
    components: Option<GameComponents>,
}

/// Registers a game's own scene components; written by `rusting_game!`
/// from its `components: [Type => "game.name"]` list.
pub type GameComponents =
    fn(&mut App) -> Result<(), crate::runtime::SceneIoError>;

impl Plugin for SimpleGamePlugin {
    fn build(&self, app: &mut App) -> Result<(), AppError> {
        if let Some(components) = self.components {
            components(app).map_err(|error| AppError::PluginSetup {
                plugin: "rusting_game",
                message: error.to_string(),
            })?;
        }
        app.add_frame_start(expand_spawn_grids);
        app.insert_resource(GameUpdateFunction(self.update));
        app.add_system(ScheduleStage::Update, run_simple_game_update);
        if let Some(tick) = self.tick {
            app.insert_resource(GameTickFunction(tick));
            app.insert_resource(TickInput::default());
            app.add_system(ScheduleStage::FixedUpdate, run_simple_game_tick);
            app.register_snapshot_component::<GameTickFunction>()
                .register_snapshot_component::<TickInput>();
        }
        app.register_snapshot_component::<GameUpdateFunction>()
            .register_snapshot_component::<GameOnceState>()
            .register_snapshot_component::<SceneNameIndex>()
            .ignore_in_snapshots::<EditedMaterials>()
            .register_snapshot_component::<SphereMeshCache>();
        Ok(())
    }
}

/// Copies each `rusting.spawn_grid` object onto its grid, in spawn order,
/// and removes the component so it copies once.
fn expand_spawn_grids(world: &mut World) {
    use crate::runtime::SpawnGrid;
    let mut grids: Vec<_> = world
        .query::<(Entity, &SpawnGrid)>()
        .iter(world)
        .map(|(entity, grid)| (entity, *grid))
        .collect();
    if grids.is_empty() {
        return;
    }
    grids.sort_by_key(|(entity, _)| entity.index());
    for (template, grid) in grids {
        world.entity_mut(template).remove::<SpawnGrid>();
        let name = world.get::<Name>(template).map(|name| name.0.clone());
        let origin = world
            .get::<Transform>(template)
            .map_or([0.0; 3], |transform| transform.position);
        let parent = world.get::<crate::runtime::Parent>(template).map(|p| p.0);
        let [nx, ny, nz] = grid.count;
        let mut index = 0;
        for z in 0..nz {
            for y in 0..ny {
                for x in 0..nx {
                    if [x, y, z] == [0; 3] {
                        continue;
                    }
                    index += 1;
                    let copy_name =
                        name.as_ref().map(|name| format!("{name}#{index}"));
                    let copy = copy_tree(world, template, copy_name.as_deref());
                    if let Some(mut transform) =
                        world.get_mut::<Transform>(copy)
                    {
                        for (axis, cell) in [x, y, z].into_iter().enumerate() {
                            transform.position[axis] =
                                origin[axis] + cell as f32 * grid.spacing[axis];
                        }
                    }
                    if let Some(parent) = parent {
                        let _ = rusting_core::hierarchy::set_parent(
                            world, copy, parent,
                        );
                    }
                }
            }
        }
    }
}

fn run_simple_game_update(world: &mut World) {
    // Copy these small values before giving the whole world to GameScene.
    let time = *world.resource::<FrameTime>();
    let update = world.resource::<GameUpdateFunction>().0;
    run_hud_buttons(&mut GameScene { world });
    update(&mut GameScene { world }, &time);
    let input = world.resource::<RuntimeInput>().clone();
    if let Some(mut state) = world.get_resource_mut::<TickInput>() {
        if !state.ticked {
            state.pending.merge_frame_edges(&input);
        }
        state.ticked = false;
    }
}

/// Runs HUD buttons that need no game code. A clicked button whose text
/// is `{dialogue:Object/n}` picks choice `n` of that object's dialogue, or
/// moves past a line with no choices. A clicked `{binding:action}` button
/// waits for a key: the next one pressed replaces the action's inputs on
/// its device (keyboard and mouse, or gamepad), Escape cancels.
fn run_hud_buttons(scene: &mut GameScene) {
    let waiting = scene
        .world
        .get_resource::<crate::runtime::RebindWait>()
        .and_then(|wait| wait.0.clone());
    if let Some(action) = waiting {
        if let Some(key) = scene.keys_pressed().into_iter().next() {
            if key != "Escape" {
                let _ = scene.rebind_device(&action, &[&key]);
            }
            scene
                .world
                .insert_resource(crate::runtime::RebindWait(None));
        }
    }
    let Some(pressed) = scene
        .world
        .get_resource::<EventQueue<crate::runtime::HudButtonPressed>>()
    else {
        return;
    };
    let clicked: Vec<String> = pressed
        .iter()
        .filter_map(|click| {
            let hud = scene
                .world
                .get::<crate::runtime::HudElement>(click.entity)?;
            let inner = hud.text.trim().strip_prefix('{')?.strip_suffix('}')?;
            Some(inner.to_owned())
        })
        .collect();
    for inner in clicked {
        if let Some((object, n)) = inner
            .strip_prefix("dialogue:")
            .and_then(|key| key.rsplit_once('/'))
        {
            if let Some(choice) =
                n.parse::<usize>().ok().and_then(|n| n.checked_sub(1))
            {
                scene.advance_dialogue(object, choice);
            }
        } else if let Some(action) = inner.strip_prefix("binding:") {
            scene.world.insert_resource(crate::runtime::RebindWait(Some(
                action.to_owned(),
            )));
        }
    }
}

/// Runs the game's tick function with this tick's input. A press goes to
/// the tick it happened in by its recorded time: when a frame runs several
/// ticks each sees its own presses, and a press timed after the frame's
/// last tick waits for the next one. Untimed presses and releases go to the
/// frame's first tick.
fn run_simple_game_tick(world: &mut World) {
    let time = *world.resource::<FrameTime>();
    let tick = world.resource::<GameTickFunction>().0;
    let mut input = world.resource::<RuntimeInput>().clone();
    let mut state = world.resource_mut::<TickInput>();
    if state.ticked {
        input.clear_frame_edges();
    }
    state.ticked = true;
    input.merge_frame_edges(&std::mem::take(&mut state.pending));
    // During a step `fixed_tick` counts the ticks before it, so this step
    // covers presses timed before `fixed_tick + 1`.
    state.pending = input.take_presses_after(time.fixed_tick as f64 + 1.0);
    let mut frame =
        std::mem::replace(&mut *world.resource_mut::<RuntimeInput>(), input);
    tick(&mut GameScene { world }, &time);
    // ponytail: only cursor capture carries back from the tick's input copy;
    // carry more fields if tick code starts changing input state.
    frame.set_cursor_captured(
        world.resource::<RuntimeInput>().cursor_captured(),
    );
    *world.resource_mut::<RuntimeInput>() = frame;
}

/// What a code reload carries from the old game process to the new one.
#[derive(serde::Serialize, serde::Deserialize)]
struct CodeReloadState {
    /// Every scene object with its reflected components.
    scene: SceneDocument,
    /// [`GameScene::once`] keys that already ran.
    once: Vec<String>,
}

/// Writes the scene objects and the finished [`GameScene::once`] keys to
/// `path`. Resources and components with no reflection registration are
/// not kept.
fn save_code_reload_state(
    runtime: &mut App,
    path: &Path,
) -> Result<(), Box<dyn Error>> {
    let world = runtime.world_mut();
    let scene = scene_document(world, "code reload")?;
    let mut once = world
        .get_resource::<GameOnceState>()
        .map(|state| state.0.iter().cloned().collect::<Vec<_>>())
        .unwrap_or_default();
    once.sort();
    write_atomic(path, &serde_json::to_vec(&CodeReloadState { scene, once })?)?;
    Ok(())
}

/// Replaces the freshly loaded scene with the state saved before a code
/// reload. Startup systems and [`GameScene::once`] blocks that already ran
/// do not run again. On error the fresh scene stays, so the game starts
/// clean.
fn restore_code_reload_state(
    runtime: &mut App,
    path: &Path,
) -> Result<(), Box<dyn Error>> {
    let state: CodeReloadState = serde_json::from_slice(&std::fs::read(path)?)?;
    let world = runtime.world_mut();
    load_scene_document(world, &state.scene, SceneLoadMode::Replace)?;
    world.insert_resource(GameOnceState(state.once.into_iter().collect()));
    // Names now belong to the restored entities.
    world.remove_resource::<SceneNameIndex>();
    runtime.skip_startup();
    Ok(())
}

/// Builds the same ECS and asset stack for windowed and headless games.
fn load_project_runtime<P: Plugin>(
    scene_path: &Path,
    plugin: P,
) -> Result<App, Box<dyn Error>> {
    let mut runtime = App::new();
    runtime.add_plugin(AssetPlugin)?;
    runtime.add_plugin(HybridPhysicsPlugin)?;
    runtime.add_plugin(RenderExtractPlugin)?;
    runtime.add_plugin(plugin)?;
    let folder = scene_path
        .ancestors()
        .skip(1)
        .find(|folder| folder.join("project.json").is_file())
        .or_else(|| scene_path.parent())
        .unwrap_or(Path::new("."));
    match load_scene(runtime.world_mut(), scene_path, SceneLoadMode::Replace) {
        // A `rusting` CLI built from another engine version cooks a layout
        // this game cannot read. When the source scene is there (a dev
        // project or an export, which copies `scenes/`), load it instead of
        // failing every run.
        Err(crate::runtime::SceneIoError::Compiled(error)) => {
            let Some(source) = project_source_scene(folder, scene_path) else {
                return Err(format!(
                    "{} was cooked by a different engine build than this \
                     game, or is damaged ({error}), and its source scene is \
                     not next to it. Recook it with a `rusting` CLI built \
                     from the engine the game builds against.",
                    scene_path.display()
                )
                .into());
            };
            eprintln!(
                "warning: {} was cooked by a different engine build than this \
                 game ({error}); loading {} instead. Reinstall the `rusting` \
                 CLI from the engine the game builds against.",
                scene_path.display(),
                source.display()
            );
            load_scene(runtime.world_mut(), &source, SceneLoadMode::Replace)?;
        }
        result => {
            result?;
        }
    }
    let start = scene_document(runtime.world_mut(), "start")?;
    runtime.insert_resource(StartingScene(start));
    if let Some(warning) = stale_cooked_scene_warning(folder, scene_path) {
        eprintln!("{warning}");
    }
    request_project_window(runtime.world_mut(), folder);
    apply_ui_theme(runtime.world_mut(), folder)?;
    runtime.insert_resource(ProjectFolder(folder.to_path_buf()));
    apply_seed(
        runtime.world_mut(),
        std::env::var_os(crate::project::SEED_ENV),
    )?;
    crate::runtime::check_determinism(runtime.world_mut())?;
    Ok(runtime)
}

/// Starts the game's random streams from `seed`, the value of
/// [`crate::project::SEED_ENV`], when it is set.
fn apply_seed(
    world: &mut World,
    seed: Option<std::ffi::OsString>,
) -> Result<(), String> {
    let Some(seed) = seed else {
        return Ok(());
    };
    let seed =
        seed.to_str()
            .and_then(|seed| seed.parse().ok())
            .ok_or(format!(
                "{} must be a whole number",
                crate::project::SEED_ENV
            ))?;
    world.insert_resource(crate::runtime::RandomSeed(seed));
    Ok(())
}

/// Asks for the `window` size in the folder's `project.json`, if any. The
/// window opens at its default size and takes this one after the first
/// frame, the same way [`GameScene::set_window_size`] works. A
/// `ui_base_size` there scales the HUD to fit the window; see
/// [`crate::runtime::RuntimeUi::set_base_size`].
fn request_project_window(world: &mut World, folder: &Path) {
    let project = std::fs::read(folder.join("project.json"))
        .ok()
        .and_then(|bytes| {
            serde_json::from_slice::<serde_json::Value>(&bytes).ok()
        })
        .unwrap_or_default();
    #[cfg(feature = "ui")]
    if let (Ok(size), Some(mut ui)) = (
        serde_json::from_value(project["ui_base_size"].clone()),
        world.get_resource_mut::<crate::runtime::RuntimeUi>(),
    ) {
        ui.set_base_size(Some(size));
    }
    if let Ok(size) = serde_json::from_value(project["window"].clone()) {
        GameScene { world }.set_window_size(size);
    }
    if let Some(title) = project["window_title"].as_str() {
        GameScene { world }.set_window_title(title);
    }
}

/// Styles HUD buttons and menus from `assets/ui/theme.json`, if the
/// project has one. A build without the `ui` feature draws no UI to style.
#[cfg(feature = "ui")]
fn apply_ui_theme(world: &mut World, folder: &Path) -> Result<(), String> {
    let path = folder.join("assets/ui/theme.json");
    let Ok(bytes) = std::fs::read(&path) else {
        return Ok(());
    };
    let theme: crate::runtime::UiTheme = serde_json::from_slice(&bytes)
        .map_err(|error| format!("{}: {error}", path.display()))?;
    if let Some(mut ui) = world.get_resource_mut::<crate::runtime::RuntimeUi>()
    {
        ui.set_theme(&theme);
    }
    Ok(())
}

#[cfg(not(feature = "ui"))]
fn apply_ui_theme(_: &mut World, _: &Path) -> Result<(), String> {
    Ok(())
}

/// A warning when `scene_path` is the project's cooked main scene and the
/// source scene changed after the last cook. Running the binary directly
/// after `scene patch` otherwise plays the old level without a word.
fn stale_cooked_scene_warning(
    folder: &Path,
    scene_path: &Path,
) -> Option<String> {
    // An exported game has no Cargo.toml, and copying sets its file times.
    if !folder.join("Cargo.toml").is_file() {
        return None;
    }
    let source = project_source_scene(folder, scene_path)?;
    let modified =
        |path: &Path| std::fs::metadata(path).and_then(|m| m.modified()).ok();
    (modified(&source)? > modified(scene_path)?).then(|| {
        format!(
            "warning: {} is older than {}, so the game runs the scene as it \
             was at the last cook. Run `rusting cook .` first (`rusting run` \
             and `rusting test` cook for you).",
            scene_path.display(),
            source.display()
        )
    })
}

/// The source of `scene_path` when it is the cooked main scene named in the
/// project's `project.json`.
fn project_source_scene(folder: &Path, scene_path: &Path) -> Option<PathBuf> {
    let manifest: crate::project::ProjectManifest = serde_json::from_slice(
        &std::fs::read(folder.join("project.json")).ok()?,
    )
    .ok()?;
    let cooked = folder.join(&manifest.cooked_scene).canonicalize().ok()?;
    let source = folder.join(&manifest.main_scene);
    (cooked == scene_path.canonicalize().ok()? && source.is_file())
        .then_some(source)
}

/// Textures drawn by [`GameScene::set_text`], by text, style and font.
#[cfg(feature = "ui")]
#[derive(Resource, Default)]
struct TextTextures(
    HashMap<String, crate::assets::Handle<crate::assets::TextureAsset>>,
);

/// egui input translation and painting for [`crate::runtime::RuntimeUi`].
#[cfg(feature = "ui")]
struct RuntimeUiPainter {
    input: egui_winit::State,
    painter: crate::rendering::egui_painter::EguiPainter,
    textures: egui::TexturesDelta,
    primitives: Vec<egui::ClippedPrimitive>,
    pixels_per_point: f32,
}

/// Images the game's swapchain asks for. [`probe_swapchain_images`] lowers it
/// when the surface allows fewer.
static SWAPCHAIN_IMAGES: AtomicU32 = AtomicU32::new(4);
/// Whether swapchain images accept a blit, which render scale needs.
static SWAPCHAIN_BLIT: AtomicBool = AtomicBool::new(false);

/// Sets [`SWAPCHAIN_IMAGES`] to four, or to the surface's maximum when that is
/// lower. Only a window has a surface, so a hidden one is made and dropped;
/// on any failure the default stays and window creation reports the error.
fn probe_swapchain_images(
    event_loop: &ActiveEventLoop,
    vulkan: &VulkanoContext,
) {
    let Ok(window) = event_loop
        .create_window(Window::default_attributes().with_visible(false))
    else {
        return;
    };
    let Ok(surface) =
        Surface::from_window(vulkan.instance().clone(), Arc::new(window))
    else {
        return;
    };
    let Ok(capabilities) = vulkan
        .device()
        .physical_device()
        .surface_capabilities(&surface, Default::default())
    else {
        return;
    };
    let wanted = capabilities.max_image_count.map_or(4, |max| max.min(4));
    SWAPCHAIN_IMAGES.store(wanted, Ordering::Relaxed);
    SWAPCHAIN_BLIT.store(
        capabilities
            .supported_usage_flags
            .contains(ImageUsage::TRANSFER_DST),
        Ordering::Relaxed,
    );
}

/// Owns the operating-system window and all Vulkan presentation state.
struct WindowRunner {
    /// Text shown in the game window title bar.
    title: String,
    /// Vulkan device, queues, and memory allocators.
    vulkan: VulkanoContext,
    /// Winit windows connected to Vulkan swapchains.
    windows: VulkanoWindows,
    /// Renderer created after the operating system opens the window.
    scene_renderer: Option<SceneRenderer>,
    /// Requests frames immediately or waits when an FPS limit is enabled.
    frame_pacer: FramePacer,
    /// VSync value currently applied to the swapchain.
    applied_vsync: Option<bool>,
    /// Cursor capture currently applied to the window.
    applied_cursor_capture: bool,
    /// Fullscreen state currently applied to the window.
    applied_fullscreen: bool,
    /// Offscreen image for `RenderSettings::render_scale`.
    scaled: Option<ScaledTarget>,
    /// Steps `GpuCloth` volumes; made with the renderer.
    cloth: Option<crate::rendering::gpu_cloth::GpuClothRunner>,
    #[cfg(feature = "ui")]
    ui: Option<RuntimeUiPainter>,
}

impl WindowRunner {
    fn new(title: String) -> Self {
        Self {
            title,
            vulkan: VulkanoContext::new(crate::rendering::vulkano_config()),
            windows: VulkanoWindows::default(),
            scene_renderer: None,
            frame_pacer: FramePacer::default(),
            applied_vsync: None,
            applied_cursor_capture: false,
            applied_fullscreen: false,
            scaled: None,
            cloth: None,
            #[cfg(feature = "ui")]
            ui: None,
        }
    }

    fn resumed(&mut self, event_loop: &ActiveEventLoop, runtime: &mut App) {
        // Winit can resume more than once. The renderer must be created once.
        if self.scene_renderer.is_some() {
            return;
        }
        probe_swapchain_images(event_loop, &self.vulkan);
        // Create the operating-system window and its Vulkan swapchain.
        self.windows.create_window(
            event_loop,
            &self.vulkan,
            &WindowDescriptor {
                title: self.title.clone(),
                width: 1440.0,
                height: 900.0,
                ..WindowDescriptor::default()
            },
            |create_info| {
                create_info.image_format = Format::B8G8R8A8_UNORM;
                create_info.min_image_count = create_info
                    .min_image_count
                    .max(SWAPCHAIN_IMAGES.load(Ordering::Relaxed));
                if SWAPCHAIN_BLIT.load(Ordering::Relaxed) {
                    create_info.image_usage |= ImageUsage::TRANSFER_DST;
                }
            },
        );
        // Apply project render settings before the first presented frame.
        let renderer = self.windows.get_primary_renderer_mut().unwrap();
        let settings = runtime.world().resource::<RenderSettings>();
        let mode = select_present_mode(
            &renderer.graphics_queue(),
            &renderer.surface(),
            settings.vsync,
        );
        if std::env::var_os(PERF_ENV).is_some() {
            eprintln!(
                "[rusting] present mode {mode:?}, render scale blit {}",
                SWAPCHAIN_BLIT.load(Ordering::Relaxed)
            );
        }
        renderer.set_present_mode(mode);
        self.applied_vsync = Some(settings.vsync);
        let initial_size = renderer.window().inner_size();
        runtime
            .world_mut()
            .resource_mut::<RuntimeInput>()
            .record_viewport_size([
                initial_size.width as f32,
                initial_size.height as f32,
            ]);
        self.scene_renderer = Some(
            SceneRenderer::new(
                renderer.graphics_queue(),
                self.vulkan.memory_allocator().clone(),
                renderer.swapchain_format(),
                renderer.swapchain_image_size(),
            )
            .expect("failed to create game scene renderer"),
        );
        self.cloth = crate::rendering::gpu_cloth::GpuClothRunner::new(
            self.vulkan.device().clone(),
            renderer.graphics_queue(),
        )
        .map_err(|error| eprintln!("GPU cloth: {error}"))
        .ok();
        self.scaled = SWAPCHAIN_BLIT
            .load(Ordering::Relaxed)
            .then(|| ScaledTarget::new(self.vulkan.memory_allocator().clone()));
        #[cfg(feature = "ui")]
        if let Some(runtime_ui) =
            runtime.world().get_resource::<crate::runtime::RuntimeUi>()
        {
            let painter = crate::rendering::egui_painter::EguiPainter::new(
                renderer.graphics_queue(),
                self.vulkan.memory_allocator().clone(),
                renderer.swapchain_format(),
            )
            .expect("failed to create runtime UI painter");
            let context = runtime_ui.context();
            let input = egui_winit::State::new(
                context.clone(),
                context.viewport_id(),
                event_loop,
                Some(renderer.window().scale_factor() as f32),
                None,
                Some(painter.max_texture_side()),
            );
            self.ui = Some(RuntimeUiPainter {
                input,
                painter,
                textures: egui::TexturesDelta::default(),
                primitives: Vec::new(),
                pixels_per_point: 1.0,
            });
        }
    }

    /// Gives a window event to the runtime UI. Returns true when the UI
    /// used it, so gameplay input should not see it.
    fn ui_event(&mut self, window_id: WindowId, event: &WindowEvent) -> bool {
        #[cfg(feature = "ui")]
        if let (Some(ui), Some(renderer)) =
            (self.ui.as_mut(), self.windows.get_renderer(window_id))
        {
            return ui.input.on_window_event(renderer.window(), event).consumed;
        }
        let _ = (window_id, event);
        false
    }

    /// Hands window input to the UI pass that the next update runs.
    fn begin_ui_frame(&mut self, window_id: WindowId, runtime: &mut App) {
        #[cfg(feature = "ui")]
        if let (Some(ui), Some(renderer), Some(mut runtime_ui)) = (
            self.ui.as_mut(),
            self.windows.get_renderer(window_id),
            runtime
                .world_mut()
                .get_resource_mut::<crate::runtime::RuntimeUi>(),
        ) {
            let window = renderer.window();
            let size = window.inner_size();
            runtime_ui.fit_window(
                [size.width as f32, size.height as f32],
                window.scale_factor() as f32,
            );
            runtime_ui.set_input(ui.input.take_egui_input(window));
        }
        let _ = (window_id, runtime);
    }

    /// Tessellates the finished UI pass for [`Self::render`].
    fn finish_ui_frame(&mut self, window_id: WindowId, runtime: &mut App) {
        #[cfg(feature = "ui")]
        if let (Some(ui), Some(renderer), Some(output)) = (
            self.ui.as_mut(),
            self.windows.get_renderer(window_id),
            runtime
                .world_mut()
                .get_resource_mut::<crate::runtime::RuntimeUi>()
                .and_then(|mut runtime_ui| runtime_ui.take_output()),
        ) {
            ui.input.handle_platform_output(
                renderer.window(),
                output.platform_output,
            );
            ui.textures.append(output.textures_delta);
            ui.pixels_per_point = output.pixels_per_point;
            ui.primitives = ui
                .input
                .egui_ctx()
                .tessellate(output.shapes, output.pixels_per_point);
        }
        let _ = (window_id, runtime);
    }

    fn request_next_frame(
        &mut self,
        event_loop: &ActiveEventLoop,
        runtime: &App,
    ) {
        if let Some(renderer) = self.windows.get_primary_renderer_mut() {
            self.frame_pacer.request_next_frame(
                event_loop,
                renderer.window(),
                runtime.world().resource::<RenderSettings>(),
            );
        }
    }

    /// Applies [`GameScene::set_fullscreen`] and
    /// [`GameScene::set_window_size`].
    fn apply_window_request(&mut self, world: &mut World) {
        let Some(mut request) = world.get_resource_mut::<WindowRequest>()
        else {
            return;
        };
        let size = request.size.take();
        let title = request.title.take();
        let fullscreen = request.fullscreen;
        let Some(renderer) = self.windows.get_primary_renderer() else {
            return;
        };
        let window = renderer.window();
        if let Some(title) = title {
            window.set_title(&title);
            self.title = title;
        }
        if self.applied_fullscreen != fullscreen {
            window.set_fullscreen(
                fullscreen.then_some(Fullscreen::Borderless(None)),
            );
            self.applied_fullscreen = fullscreen;
        }
        if let Some([width, height]) = size {
            let _ = window.request_inner_size(PhysicalSize::new(width, height));
        }
    }

    /// Hides and locks the cursor while gameplay asks for mouse look.
    fn apply_cursor_capture(&mut self, captured: bool) {
        if self.applied_cursor_capture == captured {
            return;
        }
        let Some(renderer) = self.windows.get_primary_renderer() else {
            return;
        };
        let window = renderer.window();
        let grab = if captured {
            // Some platforms support only one of the two grab modes.
            window
                .set_cursor_grab(CursorGrabMode::Locked)
                .or_else(|_| window.set_cursor_grab(CursorGrabMode::Confined))
        } else {
            window.set_cursor_grab(CursorGrabMode::None)
        };
        if let Err(error) = grab {
            eprintln!("cursor capture failed: {error}");
        }
        window.set_cursor_visible(!captured);
        self.applied_cursor_capture = captured;
    }

    fn resize(&mut self, window_id: WindowId) {
        self.windows.get_renderer_mut(window_id).unwrap().resize();
    }

    fn render(
        &mut self,
        window_id: WindowId,
        runtime: &App,
    ) -> Result<(), String> {
        let renderer = self.windows.get_renderer_mut(window_id).unwrap();
        let vsync = runtime.world().resource::<RenderSettings>().vsync;
        if self.applied_vsync != Some(vsync) {
            renderer.set_present_mode(select_present_mode(
                &renderer.graphics_queue(),
                &renderer.surface(),
                vsync,
            ));
            self.applied_vsync = Some(vsync);
        }

        let acquired = renderer.acquire(None, |_| {});
        match acquired {
            Ok(future) => {
                let world = runtime.world();
                let future = crate::rendering::render_scale::render_game(
                    self.scene_renderer.as_mut().unwrap(),
                    self.scaled.as_mut(),
                    future,
                    renderer.swapchain_image_view(),
                    world.resource::<RenderSettings>(),
                    world.resource::<RenderWorld>(),
                    world.resource::<AssetServer>(),
                    |_, future, _| Ok(future),
                )
                .map_err(|error| format!("scene rendering failed: {error}"))?;
                #[cfg(feature = "ui")]
                let future = match self.ui.as_mut() {
                    Some(ui) => {
                        let future = ui
                            .painter
                            .paint(
                                future,
                                renderer.swapchain_image_view(),
                                ui.pixels_per_point,
                                &ui.primitives,
                                &ui.textures,
                            )
                            .map_err(|error| {
                                format!("runtime UI painting failed: {error}")
                            })?;
                        ui.textures.clear();
                        future
                    }
                    None => future,
                };
                renderer.present(future, false);
                Ok(())
            }
            Err(VulkanError::OutOfDate) => {
                renderer.resize();
                Ok(())
            }
            Err(error) => Err(format!("swapchain acquisition failed: {error}")),
        }
    }
}

struct ProjectApplication {
    window: WindowRunner,
    /// ECS world, schedules, assets, and game plugin.
    runtime: App,
    /// Time of the previous frame, used to calculate delta time.
    previous_frame: Instant,
    gamepads: crate::runtime::Gamepads,
    /// State file and the flag the standard input reader sets when the
    /// editor asks for a code reload.
    code_reload: Option<(PathBuf, Arc<AtomicBool>)>,
    /// Set by `RUSTING_PERF` or F3: frames counted since the last printed
    /// line.
    perf: Option<(Instant, u32)>,
    /// Time spent in `runtime.update` and in `render` since the last line.
    perf_spent: [std::time::Duration; 2],
    /// Each frame's length in milliseconds since the last line, and when
    /// the last frame ended.
    perf_frames: (Vec<f64>, Option<Instant>),
    /// Plays the audio queue; `None` without the `audio` feature or a device.
    audio: Option<crate::audio_output::AudioOutput>,
    /// Set by [`crate::project::QUIT_AFTER_MS_ENV`]: close at this time.
    quit_at: Option<Instant>,
    /// Set by [`crate::project::BENCH_FRAMES_ENV`]: frames still to skip,
    /// frames to measure, the lengths measured so far in milliseconds, and
    /// each measured frame's CPU and GPU milliseconds.
    bench: Option<BenchRun>,
}

/// A bench run's frames still to skip, frames to measure, frame lengths in
/// milliseconds, and each measured frame's CPU and GPU milliseconds.
type BenchRun = (u32, usize, Vec<f64>, Vec<[f64; 2]>);

/// Environment variable that makes a running game print, once a second, its
/// frame rate, frame-time percentiles and where the frame time goes.
pub const PERF_ENV: &str = "RUSTING_PERF";

/// p50, p95, p99 and the largest of some frame lengths (nearest rank);
/// zeros when there are none. Sorts `lengths`.
fn frame_percentiles(lengths: &mut [f64]) -> [f64; 4] {
    lengths.sort_by(f64::total_cmp);
    let rank = |p: f64| {
        let index = (p * lengths.len() as f64).ceil() as usize;
        lengths.get(index.saturating_sub(1)).copied().unwrap_or(0.0)
    };
    [rank(0.5), rank(0.95), rank(0.99), rank(1.0)]
}

impl ProjectApplication {
    /// Wraps a prepared runtime. The window opens when winit resumes.
    fn new(title: String, mut runtime: App) -> Self {
        runtime
            .world_mut()
            .resource_mut::<PhysicsBackendStatus>()
            .gpu_dynamic_available = true;
        Self {
            window: WindowRunner::new(title),
            runtime,
            previous_frame: Instant::now(),
            gamepads: crate::runtime::Gamepads::new(),
            code_reload: None,
            perf: std::env::var_os(PERF_ENV).map(|_| (Instant::now(), 0)),
            perf_spent: [std::time::Duration::ZERO; 2],
            perf_frames: (Vec::new(), None),
            audio: crate::audio_output::AudioOutput::open(),
            quit_at: std::env::var(crate::project::QUIT_AFTER_MS_ENV)
                .ok()
                .and_then(|ms| ms.parse().ok())
                .map(|ms| {
                    Instant::now() + std::time::Duration::from_millis(ms)
                }),
            bench: std::env::var(crate::project::BENCH_FRAMES_ENV)
                .ok()
                .and_then(|frames| frames.parse().ok())
                .map(|frames: usize| {
                    let warmup = crate::project::BENCH_WARMUP_FRAMES;
                    (warmup, frames.max(1), Vec::new(), Vec::new())
                }),
        }
    }

    /// The fractional fixed tick of an input event arriving now: the game
    /// stood at `fixed_tick + accumulator` when the frame started.
    fn event_tick(&self) -> f64 {
        let world = self.runtime.world();
        let control = world.resource::<crate::runtime::TimeControl>();
        let step = control.fixed_delta.as_secs_f64();
        let since = match control.paused {
            true => 0.0,
            false => {
                (control.accumulator().as_secs_f64()
                    + self.previous_frame.elapsed().as_secs_f64()
                        * control.time_scale.max(0.0))
                    / step
            }
        };
        world.resource::<FrameTime>().fixed_tick as f64 + since
    }

    /// Hands this frame's sound requests to the audio device, or drops them.
    fn play_audio(&mut self) {
        let world = self.runtime.world_mut();
        crate::runtime::route_sound_events(world);
        let commands =
            world.resource_mut::<crate::runtime::AudioQueue>().drain();
        let Some(audio) = &mut self.audio else {
            return;
        };
        let root = world
            .get_resource::<ProjectFolder>()
            .map(|folder| folder.0.join("assets"))
            .unwrap_or_default();
        // Real time now is `accumulator` plus the time since the frame
        // started past tick `now`. A sound for tick T starts one fixed step
        // after T's real time, so start jitter does not depend on the frame.
        let control = world.resource::<crate::runtime::TimeControl>();
        let (step, paused) = (control.fixed_delta, control.paused);
        let past = control.accumulator().as_secs_f64()
            / control.time_scale.max(1e-6)
            + self.previous_frame.elapsed().as_secs_f64();
        let now = world.resource::<FrameTime>().fixed_tick;
        let delay = |tick: u64| {
            let ahead = (tick as f64 - now as f64 + 1.0) * step.as_secs_f64();
            match paused {
                true => std::time::Duration::ZERO,
                false => std::time::Duration::from_secs_f64(
                    (ahead - past).clamp(0.0, 3600.0),
                ),
            }
        };
        for command in commands {
            audio.run(&root, command, delay);
        }
        world
            .resource_mut::<crate::runtime::AudioQueue>()
            .retain_sounds(|id| audio.has_sound(id.0));
    }

    /// Adds this frame's CPU work (`update` plus extraction, preparation and
    /// recording, not waits on the GPU) and the newest GPU pass time to a
    /// bench run past its warm-up.
    fn record_bench_work(&mut self, update: std::time::Duration) {
        let Some((0, _, _, work)) = &mut self.bench else {
            return;
        };
        let Some(renderer) = self.window.scene_renderer.as_mut() else {
            return;
        };
        let mut cpu = *self
            .runtime
            .world()
            .resource::<crate::runtime::CpuFrameTimings>();
        renderer.write_cpu_timings(&mut cpu);
        let cpu = update + cpu.extraction + cpu.preparation + cpu.recording;
        let gpu = renderer.gpu_pass_times().total();
        work.push([cpu, gpu].map(|time| time.as_secs_f64() * 1000.0));
    }

    /// F3: starts or stops the once-a-second perf line and its overlay.
    fn toggle_perf_overlay(&mut self) {
        let world = self.runtime.world_mut();
        if self.perf.take().is_some() {
            world.remove_resource::<crate::runtime::PerfOverlay>();
            if let Some(window) = self.window.windows.get_primary_window() {
                window.set_title(&self.window.title);
            }
        } else {
            self.perf = Some((Instant::now(), 0));
            self.perf_frames = Default::default();
            self.perf_spent = Default::default();
            world.insert_resource(crate::runtime::PerfOverlay("-- fps".into()));
        }
    }

    /// Prints `[rusting] perf` once a second while `RUSTING_PERF` or F3 has
    /// it on, and shows fps and frame time in the title and the overlay.
    fn report_perf(&mut self) {
        let Some((since, frames)) = &mut self.perf else {
            return;
        };
        *frames += 1;
        let now = Instant::now();
        let (lengths, last) = &mut self.perf_frames;
        if let Some(last) = last.replace(now) {
            lengths.push((now - last).as_secs_f64() * 1000.0);
        }
        let elapsed = since.elapsed();
        if elapsed < std::time::Duration::from_secs(1) {
            return;
        }
        let [p50, p95, p99, max] =
            frame_percentiles(&mut std::mem::take(lengths));
        let fps = f64::from(*frames) / elapsed.as_secs_f64();
        let [update, render] =
            std::mem::take(&mut self.perf_spent).map(|time| time / *frames);
        (*since, *frames) = (Instant::now(), 0);
        let summary =
            format!("{fps:.0} fps  {:.2} ms  p95 {p95:.2} ms", 1000.0 / fps);
        if let Some(window) = self.window.windows.get_primary_window() {
            window.set_title(&format!("{} - {summary}", self.window.title));
        }
        self.runtime
            .world_mut()
            .insert_resource(crate::runtime::PerfOverlay(summary));
        let ms = |time: std::time::Duration| time.as_secs_f64() * 1000.0;
        let mut cpu = *self
            .runtime
            .world()
            .resource::<crate::runtime::CpuFrameTimings>();
        let Some(renderer) = self.window.scene_renderer.as_mut() else {
            return;
        };
        renderer.write_cpu_timings(&mut cpu);
        let gpu = renderer.gpu_pass_times();
        let counters = renderer.render_counters();
        let mut line = format!(
            "[rusting] perf {fps:.0} fps ({:.2} ms/frame) | frame p50 {p50:.2} p95 {p95:.2} p99 {p99:.2} max {max:.2} ms | update {:.2} render {:.2} ms | CPU fixed {:.2} update {:.2} post_update {:.2} extract {:.2} prepare {:.2} record {:.2} ms | GPU {:.2} ms",
            1000.0 / fps,
            ms(update),
            ms(render),
            ms(cpu.physics),
            ms(cpu.update),
            ms(cpu.post_update),
            ms(cpu.extraction),
            ms(cpu.preparation),
            ms(cpu.recording),
            ms(gpu.total()),
        );
        for (pass, time) in &gpu.0 {
            line += &format!(" | {} {:.2}", pass.label(), ms(*time));
        }
        line += &format!(
            " | draws {} dispatches {} triangles {}",
            counters.draws, counters.dispatches, counters.triangles
        );
        eprintln!("{line}");
    }
}

impl ApplicationHandler for ProjectApplication {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        self.window.resumed(event_loop, &mut self.runtime);
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_id: WindowId,
        event: WindowEvent,
    ) {
        // The swapchain probe's hidden window reports events too.
        if self.window.windows.get_renderer(window_id).is_none() {
            return;
        }
        // Presses the UI uses stay out of gameplay; releases always pass so
        // no key or button stays held.
        if self.window.ui_event(window_id, &event)
            && match &event {
                WindowEvent::KeyboardInput { event, .. } => {
                    event.state.is_pressed()
                }
                WindowEvent::MouseInput { state, .. } => state.is_pressed(),
                _ => false,
            }
        {
            return;
        }
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => {
                self.window.resize(window_id);
                self.runtime
                    .world_mut()
                    .resource_mut::<RuntimeInput>()
                    .record_viewport_size([
                        size.width as f32,
                        size.height as f32,
                    ]);
            }
            WindowEvent::ScaleFactorChanged { .. } => {
                self.window.resize(window_id);
            }
            WindowEvent::Focused(focused) => {
                self.runtime
                    .world_mut()
                    .resource_mut::<RuntimeInput>()
                    .record_focus(focused);
            }
            WindowEvent::KeyboardInput { event, .. } => {
                if let PhysicalKey::Code(code) = event.physical_key {
                    if code == crate::runtime::KeyCode::F3
                        && event.state.is_pressed()
                        && !event.repeat
                    {
                        self.toggle_perf_overlay();
                    }
                    let tick = self.event_tick();
                    let mut input =
                        self.runtime.world_mut().resource_mut::<RuntimeInput>();
                    input.record_key(code, event.state.is_pressed());
                    if event.state.is_pressed() && !event.repeat {
                        input.record_press_tick(
                            crate::runtime::InputBinding::Key(code),
                            tick,
                        );
                    }
                }
            }
            WindowEvent::MouseInput { state, button, .. } => {
                let tick = self.event_tick();
                let mut input =
                    self.runtime.world_mut().resource_mut::<RuntimeInput>();
                if state.is_pressed() {
                    input.record_press_tick(
                        crate::runtime::InputBinding::Mouse(button),
                        tick,
                    );
                }
                input.record_mouse_button(button, state.is_pressed());
            }
            WindowEvent::CursorMoved { position, .. } => {
                self.runtime
                    .world_mut()
                    .resource_mut::<RuntimeInput>()
                    .record_cursor_position([
                        position.x as f32,
                        position.y as f32,
                    ]);
            }
            WindowEvent::RedrawRequested => {
                // Completed GPU events and synchronized state enter ECS before
                // this frame starts, so Rust update systems can read them.
                let scene_renderer =
                    self.window.scene_renderer.as_mut().unwrap();
                let raw_events = scene_renderer.take_completed_physics_events();
                let states = scene_renderer.take_completed_physics_states();
                let hashes =
                    scene_renderer.take_completed_physics_state_hashes();
                let lost = scene_renderer.take_physics_events_lost();
                let capacity = scene_renderer.capacity_diagnostics();
                self.runtime.world_mut().insert_resource(capacity);
                if lost > 0 {
                    self.runtime
                        .world_mut()
                        .resource_mut::<EventQueue<GpuPhysicsEventsLost>>()
                        .send(GpuPhysicsEventsLost { count: lost });
                }
                if !raw_events.is_empty() {
                    route_gpu_physics_events(
                        self.runtime.world_mut(),
                        &raw_events,
                    );
                }
                if !states.is_empty() {
                    apply_gpu_state_samples(self.runtime.world_mut(), &states);
                }
                record_gpu_state_hashes(self.runtime.world_mut(), &hashes);
                if let Some(runner) = &mut self.window.cloth {
                    if let Err(error) =
                        crate::rendering::gpu_cloth::service_gpu_cloths(
                            self.runtime.world_mut(),
                            runner,
                        )
                    {
                        eprintln!("GPU cloth: {error}");
                    }
                }
                // Delta time tells gameplay how much real time passed.
                let now = Instant::now();
                let delta = now.saturating_duration_since(self.previous_frame);
                self.previous_frame = now;
                if let Some((warmup, frames, lengths, work)) = &mut self.bench {
                    match warmup.checked_sub(1) {
                        Some(left) => *warmup = left,
                        None => lengths.push(delta.as_secs_f64() * 1000.0),
                    }
                    if lengths.len() >= *frames {
                        eprintln!(
                            "{}",
                            crate::project::bench_line(lengths, work)
                        );
                        event_loop.exit();
                        return;
                    }
                }
                self.gamepads.poll(
                    &mut self
                        .runtime
                        .world_mut()
                        .resource_mut::<RuntimeInput>(),
                );
                self.window.begin_ui_frame(window_id, &mut self.runtime);
                let update_start = Instant::now();
                let delta = crate::runtime::after_hit_stop(
                    self.runtime.world_mut(),
                    delta,
                );
                let updated = self.runtime.update(delta);
                let update_time = update_start.elapsed();
                self.perf_spent[0] += update_time;
                if let Err(error) = updated {
                    eprintln!("runtime update failed: {error}");
                    event_loop.exit();
                    return;
                }
                self.window.finish_ui_frame(window_id, &mut self.runtime);
                if self.runtime.exit_requested()
                    || self.quit_at.is_some_and(|at| Instant::now() >= at)
                {
                    event_loop.exit();
                    return;
                }
                self.play_audio();
                if let Some(mut assets) =
                    self.runtime.world_mut().get_resource_mut::<AssetServer>()
                {
                    for failure in assets.take_reload_failures() {
                        eprintln!("hot reload failed: {failure}");
                    }
                }
                let mut input =
                    self.runtime.world_mut().resource_mut::<RuntimeInput>();
                input.clear_frame_edges();
                let captured = input.cursor_captured();
                self.window.apply_cursor_capture(captured);
                self.window.apply_window_request(self.runtime.world_mut());
                let render_start = Instant::now();
                let rendered = self.window.render(window_id, &self.runtime);
                self.perf_spent[1] += render_start.elapsed();
                self.record_bench_work(update_time);
                match rendered {
                    Ok(()) => {
                        announce_first_frame();
                        self.report_perf();
                    }
                    Err(error) => {
                        eprintln!("{error}");
                        event_loop.exit();
                    }
                }
            }
            _ => {}
        }
    }

    fn device_event(
        &mut self,
        _event_loop: &ActiveEventLoop,
        _device_id: DeviceId,
        event: DeviceEvent,
    ) {
        if let DeviceEvent::MouseMotion { delta } = event {
            self.runtime
                .world_mut()
                .resource_mut::<RuntimeInput>()
                .record_mouse_motion([delta.0 as f32, delta.1 as f32]);
        }
    }

    fn exiting(&mut self, _event_loop: &ActiveEventLoop) {
        // The egui clipboard talks to the Wayland connection, which closes
        // when the event loop returns; dropping it later crashes the exit.
        #[cfg(feature = "ui")]
        {
            self.window.ui = None;
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        if let Some((path, requested)) = &self.code_reload {
            if requested.load(Ordering::Relaxed) {
                // The editor rebuilds and starts the game again after this
                // process exits.
                if let Err(error) =
                    save_code_reload_state(&mut self.runtime, path)
                {
                    eprintln!(
                        "{} could not save the scene: {error}",
                        crate::project::CODE_RELOAD_CLEAN_MARKER
                    );
                }
                event_loop.exit();
                return;
            }
        }
        self.window.request_next_frame(event_loop, &self.runtime);
    }
}

/// Opens a window and runs a prepared runtime until the window closes.
/// With [`crate::project::REPLAY_OUT_ENV`] set, records the session and
/// writes the replay there on exit, or a scenario when the path ends in
/// `.scenario.json`.
pub(crate) fn run_windowed(
    title: String,
    mut runtime: App,
) -> Result<(), Box<dyn Error>> {
    let replay_out = std::env::var_os(crate::project::REPLAY_OUT_ENV);
    if replay_out.is_some() {
        runtime.start_recording();
    }
    let event_loop = EventLoop::new()?;
    let mut application = ProjectApplication::new(title, runtime);
    if let Some(path) = std::env::var_os(crate::project::CODE_RELOAD_STATE_ENV)
    {
        let requested = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&requested);
        std::thread::spawn(move || {
            for line in std::io::stdin().lines().map_while(Result::ok) {
                if line.trim() == crate::project::CODE_RELOAD_SAVE_COMMAND {
                    flag.store(true, Ordering::Relaxed);
                }
            }
        });
        application.code_reload = Some((PathBuf::from(path), requested));
    }
    event_loop.run_app(&mut application)?;
    if let (Some(path), Some(replay)) =
        (replay_out, application.runtime.finish_recording())
    {
        let path = PathBuf::from(path);
        let name = path.file_name().unwrap_or_default().to_string_lossy();
        // A `.scenario.json` path asks for a test, not a replay.
        let bytes = match name.strip_suffix(".scenario.json") {
            Some(stem) => serde_json::to_vec_pretty(
                &crate::scenario::scenario_from_replay(
                    stem,
                    &replay,
                    application
                        .runtime
                        .world()
                        .resource::<crate::runtime::ActionMap>(),
                ),
            )?,
            None => serde_json::to_vec(&replay)?,
        };
        std::fs::write(path, bytes)?;
    }
    Ok(())
}

pub use crate::project::HEADLESS_TICKS_ENV;

/// Set when [`run_project`] starts; first-frame timing counts from here.
static GAME_START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();

/// Prints [`crate::project::FIRST_FRAME_MARKER`] once, so the editor and
/// `rusting run` can measure save-to-first-playable-frame time.
fn announce_first_frame() {
    static ANNOUNCED: std::sync::Once = std::sync::Once::new();
    ANNOUNCED.call_once(|| {
        let start = GAME_START.get_or_init(Instant::now);
        eprintln!(
            "{} {} ms",
            crate::project::FIRST_FRAME_MARKER,
            start.elapsed().as_millis()
        );
    });
}

/// Runs a cooked scene with a native game-defined Rust plugin.
///
/// With [`HEADLESS_TICKS_ENV`] set, runs that many ticks through
/// [`run_project_headless`] instead of opening a window. With
/// [`crate::scenario::TEST_SCENARIO_ENV`] set, runs that scenario file
/// through [`run_project_scenario`] instead. With
/// [`crate::project::REPLAY_PLAY_ENV`] set, plays that replay headless and
/// fails if it diverges. With [`crate::project::SERVER_ENV`] set, runs as a
/// dedicated server through [`run_project_server`].
///
/// # Arguments
/// * `title` - Text shown in the game window title bar.
/// * `scene_path` - Path to cooked `.rscene.bin` data.
/// * `plugin` - Native Rust systems and resources used by this game.
pub fn run_project<P: Plugin>(
    title: impl Into<String>,
    scene_path: impl Into<PathBuf>,
    plugin: P,
) -> Result<(), Box<dyn Error>> {
    GAME_START.get_or_init(Instant::now);
    // Unit tests share one process, and its panics are expected.
    if !cfg!(test) {
        crate::crash::install_crash_reporter();
    }
    if std::env::var_os(crate::debug_session::DEBUG_SESSION_ENV).is_some() {
        let mut runtime = load_project_runtime(&scene_path.into(), plugin)?;
        crate::debug_session::run_debug_session(
            &mut runtime,
            std::io::stdin().lock(),
            std::io::stdout().lock(),
        )?;
        return Ok(());
    }
    if let Some(scenario) = std::env::var_os(crate::scenario::TEST_SCENARIO_ENV)
    {
        let report = std::env::var_os(crate::scenario::TEST_REPORT_ENV);
        return run_project_scenario(
            scene_path,
            plugin,
            PathBuf::from(scenario),
            report.map(PathBuf::from),
        );
    }
    if let Some(ticks) = std::env::var_os(HEADLESS_TICKS_ENV) {
        let ticks = ticks
            .to_str()
            .and_then(|ticks| ticks.parse().ok())
            .ok_or(format!("{HEADLESS_TICKS_ENV} must be a tick count"))?;
        return run_project_headless(scene_path, plugin, ticks);
    }
    if std::env::var_os(crate::project::SERVER_ENV).is_some() {
        return run_project_server(scene_path, plugin);
    }
    let mut runtime = load_project_runtime(&scene_path.into(), plugin)?;
    if let Some(path) = std::env::var_os(crate::project::REPLAY_PLAY_ENV) {
        let replay: crate::runtime::Replay =
            serde_json::from_slice(&std::fs::read(path)?)?;
        return match crate::runtime::play_replay(&mut runtime, &replay)? {
            None => Ok(()),
            Some(tick) => Err(format!(
                "replay diverges from the recording at tick {tick}"
            )
            .into()),
        };
    }
    if let Some(path) = std::env::var_os(crate::project::CODE_RELOAD_STATE_ENV)
        .map(PathBuf::from)
        .filter(|path| path.is_file())
    {
        match restore_code_reload_state(&mut runtime, &path) {
            Ok(()) => eprintln!("{}", crate::project::CODE_RELOAD_KEPT_MARKER),
            Err(error) => eprintln!(
                "{} the saved scene does not fit the new code: {error}",
                crate::project::CODE_RELOAD_CLEAN_MARKER
            ),
        }
        // A crash on the next run must not restore this state again.
        let _ = std::fs::remove_file(&path);
    }
    run_windowed(title.into(), runtime)
}

/// Runs a cooked scene for a fixed number of ticks with no window, Vulkan
/// device, or renderer, then returns. For CI and machines with no display.
/// With [`crate::project::STATE_HASH_OUT_ENV`] set, writes the run's
/// [`crate::runtime::StateHashReport`] to that file. With
/// [`crate::project::FINAL_SCENE_OUT_ENV`] set, saves the scene as it stands
/// after the last tick to that file.
///
/// # Arguments
/// * `scene_path` - Path to cooked `.rscene.bin` data.
/// * `plugin` - Native Rust systems and resources used by this game.
/// * `ticks` - Number of fixed-step updates to run before returning.
pub fn run_project_headless<P: Plugin>(
    scene_path: impl Into<PathBuf>,
    plugin: P,
    ticks: u32,
) -> Result<(), Box<dyn Error>> {
    let (mut runtime, report, elapsed) =
        simulate_timed(scene_path.into(), plugin, ticks)?;
    eprintln!(
        "{} {:.3}",
        crate::project::TICK_TIME_MARKER,
        elapsed.as_secs_f64() * 1000.0 / f64::from(ticks.max(1))
    );
    if let Some(path) = std::env::var_os(crate::project::FINAL_SCENE_OUT_ENV) {
        crate::runtime::save_scene(
            runtime.world_mut(),
            path,
            format!("after tick {ticks}"),
        )?;
    }
    if let Some(path) = std::env::var_os(crate::project::STATE_HASH_OUT_ENV) {
        std::fs::write(path, serde_json::to_vec(&report)?)?;
    }
    Ok(())
}

/// Runs a cooked scene as a dedicated server: no window, Vulkan device, or
/// renderer, one fixed tick per `fixed_delta` of real time, until game code
/// quits. Inserts [`crate::net::DedicatedServer`] so game code knows to
/// host, and keeps its load figures current. Prints a warning when a
/// stall drops ticks.
///
/// # Arguments
/// * `scene_path` - Path to cooked `.rscene.bin` data.
/// * `plugin` - Native Rust systems and resources used by this game.
pub fn run_project_server<P: Plugin>(
    scene_path: impl Into<PathBuf>,
    plugin: P,
) -> Result<(), Box<dyn Error>> {
    let mut runtime = load_project_runtime(&scene_path.into(), plugin)?;
    let mut server = crate::net::DedicatedServer::default();
    let step = runtime.world().resource::<FrameTime>().fixed_delta;
    let mut deadline = Instant::now();
    loop {
        runtime.world_mut().insert_resource(server);
        let started = Instant::now();
        runtime.update(step)?;
        runtime
            .world_mut()
            .resource_mut::<RuntimeInput>()
            .clear_frame_edges();
        announce_first_frame();
        if runtime.exit_requested() {
            return Ok(());
        }
        let now = Instant::now();
        server.ticks += 1;
        server.last_budget = (now - started).as_secs_f32() / step.as_secs_f32();
        server.budget += (server.last_budget - server.budget) * 0.05;
        deadline += step;
        if deadline > now {
            std::thread::sleep(deadline - now);
        } else if now - deadline > step * 10 {
            // Too far behind (a long stall): drop the lost ticks instead of
            // running them back to back.
            let dropped =
                ((now - deadline).as_secs_f64() / step.as_secs_f64()) as u64;
            server.dropped_ticks += dropped;
            eprintln!(
                "server overloaded: skipped {dropped} ticks after a stall"
            );
            deadline = now;
        } else {
            server.late_ticks += 1;
        }
    }
}

/// Loads a cooked scene and runs `ticks` updates of one `fixed_delta` each
/// (one fixed step each at time scale 1) with no window, surface, Vulkan
/// device, or renderer, then returns the runtime and a report of every
/// tick's state hash (`StateHashes` keeps only the recent ones). GPU-class
/// bodies do not move without a renderer; a warning names how many there
/// are.
pub fn simulate_project_headless<P: Plugin>(
    scene_path: impl Into<PathBuf>,
    plugin: P,
    ticks: u32,
) -> Result<(App, crate::runtime::StateHashReport), Box<dyn Error>> {
    let (runtime, report, _) =
        simulate_timed(scene_path.into(), plugin, ticks)?;
    Ok((runtime, report))
}

/// [`simulate_project_headless`] plus the wall time of the tick loop alone,
/// without the scene load.
fn simulate_timed<P: Plugin>(
    scene_path: PathBuf,
    plugin: P,
    ticks: u32,
) -> Result<
    (App, crate::runtime::StateHashReport, std::time::Duration),
    Box<dyn Error>,
> {
    let mut runtime = load_project_runtime(&scene_path, plugin)?;
    let world = runtime.world_mut();
    let gpu_bodies = world
        .query::<&crate::runtime::PhysicsBody>()
        .iter(world)
        .filter(|body| body.uses_gpu())
        .count();
    if gpu_bodies > 0 {
        eprintln!(
            "headless run: {gpu_bodies} GPU physics bodies stay still without a renderer"
        );
    }

    let delta = runtime.world().resource::<FrameTime>().fixed_delta;
    let mut hashes = Vec::with_capacity(ticks as usize);
    let start = Instant::now();
    for _ in 0..ticks {
        runtime.update(delta)?;
        // As the windowed loop does, so a press game code injects is
        // "just pressed" for one update only.
        runtime
            .world_mut()
            .resource_mut::<RuntimeInput>()
            .clear_frame_edges();
        announce_first_frame();
        if runtime.exit_requested() {
            break;
        }
        hashes.extend(
            runtime
                .world()
                .get_resource::<crate::runtime::StateHashes>()
                .and_then(|recorded| recorded.recent.back().copied()),
        );
    }
    let elapsed = start.elapsed();
    let entities =
        crate::runtime::named_entity_state_hashes(runtime.world_mut());
    let report = crate::runtime::StateHashReport {
        ticks: hashes,
        entities,
    };
    Ok((runtime, report, elapsed))
}

/// Runs a [`crate::scenario::Scenario`] file against a cooked scene with no
/// window. Captures render offscreen when Vulkan is available. Writes the
/// [`crate::scenario::ScenarioReport`] as JSON to `report_path`, or to
/// standard output without one, and fails when the scenario fails.
pub fn run_project_scenario<P: Plugin>(
    scene_path: impl Into<PathBuf>,
    plugin: P,
    scenario_path: PathBuf,
    report_path: Option<PathBuf>,
) -> Result<(), Box<dyn Error>> {
    let mut scenario: crate::scenario::Scenario =
        serde_json::from_str(&std::fs::read_to_string(&scenario_path)?)?;
    scenario.update_golden =
        std::env::var_os(crate::scenario::UPDATE_GOLDEN_ENV).is_some();
    scenario.keep_going |=
        std::env::var_os(crate::scenario::KEEP_GOING_ENV).is_some();
    start_stall_watchdog(&scenario, report_path.clone());
    let mut runtime = load_project_runtime(&scene_path.into(), plugin)?;
    let base = scenario_path.parent().unwrap_or(Path::new("."));
    let report = crate::scenario::run_scenario(&mut runtime, &scenario, base);
    let text = serde_json::to_string_pretty(&report)?;
    match report_path {
        Some(path) => std::fs::write(path, text)?,
        None => println!("{text}"),
    }
    match report.first_failure {
        None => Ok(()),
        Some(failure) => {
            let more =
                report.steps.iter().filter(|step| !step.ok).count().max(1) - 1;
            let more = if more > 0 {
                format!(" ({more} more failed checks in the report)")
            } else {
                String::new()
            };
            Err(format!(
                "scenario `{}` failed at tick {} step {}: {}{more}",
                report.name, failure.tick, failure.step, failure.message
            )
            .into())
        }
    }
}

/// Starts a thread that, when no scenario tick finishes within
/// [`crate::scenario::STALL_SECS_ENV`] seconds, writes a failed report
/// naming the last finished tick and exits the process with code 1. A game
/// that deadlocks then fails its test instead of hanging `rusting test`.
fn start_stall_watchdog(
    scenario: &crate::scenario::Scenario,
    report_path: Option<PathBuf>,
) {
    use crate::scenario::{ScenarioReport, StepResult, TICKS_FINISHED};
    let secs = std::env::var(crate::scenario::STALL_SECS_ENV)
        .ok()
        .and_then(|secs| secs.parse().ok())
        .unwrap_or(crate::scenario::DEFAULT_STALL_SECS);
    // Unit tests run scenarios in one long-lived process, where the thread
    // would outlive its run and exit the process during later slow tests.
    // ponytail: no per-run stop signal; add one if a process ever runs
    // several scenarios outside tests.
    if secs == 0 || cfg!(test) {
        return;
    }
    let limit = std::time::Duration::from_secs(secs);
    let (name, seed) = (scenario.name.clone(), scenario.seed);
    std::thread::spawn(move || {
        let finished = crate::scenario::wait_for_stall(
            limit,
            std::time::Duration::from_millis(200),
            || TICKS_FINISHED.load(std::sync::atomic::Ordering::Relaxed),
        );
        let failure = StepResult {
            tick: u32::try_from(finished.saturating_sub(1)).unwrap_or(u32::MAX),
            step: 0,
            ok: false,
            message: crate::scenario::stall_message(limit, finished),
            actual: serde_json::Value::Null,
        };
        eprintln!("scenario `{name}`: {}", failure.message);
        let report = ScenarioReport {
            name,
            seed,
            passed: false,
            ticks_run: finished,
            first_failure: Some(failure.clone()),
            steps: vec![failure],
            captures: Vec::new(),
            trace: Vec::new(),
            fuzz_steps: Vec::new(),
            coverage: serde_json::Value::Null,
            explore: serde_json::Value::Null,
            perf: Default::default(),
            gpu_state_hashes: Vec::new(),
            state_hashes: Vec::new(),
        };
        if let (Some(path), Ok(text)) =
            (report_path, serde_json::to_string_pretty(&report))
        {
            let _ = std::fs::write(path, text);
        }
        std::process::exit(1);
    });
}

/// Runs a cooked scene using one short native Rust update function.
///
/// # Arguments
/// * `scene_path` - Path to cooked `.rscene.bin` data.
/// * `update` - Function called once per rendered frame.
pub fn run_game(
    scene_path: impl Into<PathBuf>,
    update: GameUpdate,
) -> GameResult {
    run_project(
        "RustingEngine Game",
        scene_path,
        SimpleGamePlugin {
            update,
            tick: None,
            components: None,
        },
    )
}

/// Runs a cooked scene with a per-frame update and a per-fixed-tick
/// function. Put game state that must step once per tick in `tick`, and
/// drawing (HUD, egui) in `update`.
///
/// # Arguments
/// * `scene_path` - Path to cooked `.rscene.bin` data.
/// * `update` - Function called once per rendered frame, after its ticks.
/// * `tick` - Function called once per fixed tick. [`GameScene::pressed`]
///   there is true for a press made since the previous tick.
pub fn run_game_with_tick(
    scene_path: impl Into<PathBuf>,
    update: GameUpdate,
    tick: GameUpdate,
) -> GameResult {
    run_project(
        "RustingEngine Game",
        scene_path,
        SimpleGamePlugin {
            update,
            tick: Some(tick),
            components: None,
        },
    )
}

/// Runs a cooked scene like [`run_game_with_tick`], after registering the
/// game's own scene components. `rusting_game!` calls this for its
/// `components:` list.
///
/// # Arguments
/// * `scene_path` - Path to cooked `.rscene.bin` data.
/// * `update` - Function called once per rendered frame.
/// * `tick` - Optional function called once per fixed tick.
/// * `components` - Registers the game's components with
///   [`App::register_scene_component`].
pub fn run_game_with_components(
    scene_path: impl Into<PathBuf>,
    update: GameUpdate,
    tick: Option<GameUpdate>,
    components: GameComponents,
) -> GameResult {
    run_project(
        "RustingEngine Game",
        scene_path,
        SimpleGamePlugin {
            update,
            tick,
            components: Some(components),
        },
    )
}

/// Finds cooked data beside an exported executable, then falls back to the
/// Cargo project path used during development.
#[must_use]
pub fn resolve_game_scene_path(
    relative: impl AsRef<std::path::Path>,
    project_root: impl AsRef<std::path::Path>,
) -> PathBuf {
    let relative = relative.as_ref();
    if let Some(path) = std::env::var_os("RUSTING_SCENE_PATH") {
        return PathBuf::from(path);
    }
    if let Ok(executable) = std::env::current_exe() {
        if let Some(folder) = executable.parent() {
            let packaged = folder.join(relative);
            if packaged.is_file() {
                return packaged;
            }
        }
    }
    project_root.as_ref().join(relative)
}

/// Generates the native game entry point while keeping gameplay in normal
/// Rust. The scene path is relative to the game project's `Cargo.toml`.
#[macro_export]
macro_rules! rusting_game {
    ($update:path, components: [$($component:ty => $name:literal),* $(,)?]) => {
        $crate::rusting_game!(@components $update, None, [$($component => $name),*]);
    };
    ($update:path, tick: $tick:path, components: [$($component:ty => $name:literal),* $(,)?]) => {
        $crate::rusting_game!(@components $update, Some($tick), [$($component => $name),*]);
    };
    (@components $update:path, $tick:expr, [$($component:ty => $name:literal),*]) => {
        /// Registers this game's own scene components.
        fn rusting_game_components(
            app: &mut $crate::App,
        ) -> Result<(), $crate::runtime::SceneIoError> {
            $(app.register_scene_component::<$component>($name)?;)*
            Ok(())
        }

        fn main() -> $crate::project_runner::GameResult {
            let scene = $crate::project_runner::resolve_game_scene_path(
                "build/main.rscene.bin",
                env!("CARGO_MANIFEST_DIR"),
            );
            $crate::project_runner::run_game_with_components(
                scene,
                $update,
                $tick,
                rusting_game_components,
            )
        }
    };
    ($update:path) => {
        $crate::rusting_game!("build/main.rscene.bin", $update);
    };
    ($update:path, tick: $tick:path) => {
        $crate::rusting_game!("build/main.rscene.bin", $update, tick: $tick);
    };
    ($scene:literal, $update:path, tick: $tick:path) => {
        fn main() -> $crate::project_runner::GameResult {
            let scene = $crate::project_runner::resolve_game_scene_path(
                $scene,
                env!("CARGO_MANIFEST_DIR"),
            );
            $crate::project_runner::run_game_with_tick(scene, $update, $tick)
        }
    };
    ($scene:literal, $update:path) => {
        fn main() -> $crate::project_runner::GameResult {
            let scene = $crate::project_runner::resolve_game_scene_path(
                $scene,
                env!("CARGO_MANIFEST_DIR"),
            );
            $crate::project_runner::run_game(scene, $update)
        }
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(feature = "ui")]
    #[test]
    fn a_ui_theme_file_styles_the_runtime_ui() {
        let folder = std::env::temp_dir()
            .join(format!("rusting-theme-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(folder.join("assets/ui")).unwrap();
        let mut world = World::new();
        world.insert_resource(crate::runtime::RuntimeUi::default());
        apply_ui_theme(&mut world, &folder).unwrap();
        let path = folder.join("assets/ui/theme.json");
        std::fs::write(&path, r#"{"button_padding": [20, 10]}"#).unwrap();
        apply_ui_theme(&mut world, &folder).unwrap();
        let padding = world
            .resource::<crate::runtime::RuntimeUi>()
            .context()
            .style()
            .spacing
            .button_padding;
        assert_eq!(padding, egui::vec2(20.0, 10.0));
        // A typo fails the start with the file and field named.
        std::fs::write(&path, r#"{"buton_padding": [20, 10]}"#).unwrap();
        let error = apply_ui_theme(&mut world, &folder).unwrap_err();
        assert!(
            error.contains("theme.json") && error.contains("buton_padding")
        );
        std::fs::remove_dir_all(&folder).unwrap();
    }

    #[test]
    fn project_json_window_size_is_asked_for() {
        let folder = std::env::temp_dir()
            .join(format!("rusting-window-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&folder).unwrap();
        let mut world = World::new();
        request_project_window(&mut world, &folder);
        assert!(world.get_resource::<WindowRequest>().is_none());
        std::fs::write(
            folder.join("project.json"),
            r#"{"name": "n", "window": [1920, 1080], "ui_base_size": [960, 540]}"#,
        )
        .unwrap();
        #[cfg(feature = "ui")]
        world.insert_resource(crate::runtime::RuntimeUi::default());
        request_project_window(&mut world, &folder);
        assert_eq!(world.resource::<WindowRequest>().size, Some([1920, 1080]));
        assert_eq!(world.resource::<WindowRequest>().title, None);
        std::fs::write(
            folder.join("project.json"),
            r#"{"name": "n", "window_title": "Rusting Raft"}"#,
        )
        .unwrap();
        request_project_window(&mut world, &folder);
        assert_eq!(
            world.resource::<WindowRequest>().title.as_deref(),
            Some("Rusting Raft")
        );
        #[cfg(feature = "ui")]
        {
            // A 1920x1080 window at 1.5x DPI shows the 960x540 HUD at 2x.
            let mut ui = world.resource_mut::<crate::runtime::RuntimeUi>();
            ui.fit_window([1920.0, 1080.0], 1.5);
            // egui takes a new zoom when the next pass begins.
            ui.context().begin_pass(egui::RawInput::default());
            let zoom = ui.context().zoom_factor();
            let _ = ui.context().end_pass();
            assert!((zoom * 1.5 - 2.0).abs() < 1e-4, "{zoom}");
        }
        std::fs::remove_dir_all(&folder).unwrap();
    }

    #[test]
    fn clicking_a_dialogue_choice_button_steps_the_dialogue() {
        use crate::runtime::{Dialogue, DialogueChoice, DialogueLine};
        let mut world = World::new();
        let line = |id: &str, next: &str, choices| DialogueLine {
            id: id.into(),
            next: next.into(),
            choices,
            ..DialogueLine::default()
        };
        let to = |next: &str| DialogueChoice {
            next: next.into(),
            counter: "asked".into(),
            add: 1,
            ..DialogueChoice::default()
        };
        let smith = world
            .spawn((
                Name("Smith".into()),
                Dialogue {
                    lines: vec![
                        line("hi", "ask", vec![]),
                        line("ask", "", vec![to("hi"), to("")]),
                    ],
                    current: "hi".into(),
                },
            ))
            .id();
        let hud = |text: &str| crate::runtime::HudElement {
            text: text.into(),
            button: true,
            ..Default::default()
        };
        let next = world.spawn(hud("{dialogue:Smith/1}")).id();
        let second = world.spawn(hud(" {dialogue:Smith/2} ")).id();
        let other = world.spawn(hud("Quit")).id();
        let click = |world: &mut World, entities: &[Entity]| {
            let mut queue =
                EventQueue::<crate::runtime::HudButtonPressed>::default();
            for &entity in entities {
                queue.send(crate::runtime::HudButtonPressed { entity });
            }
            queue.begin_frame();
            world.insert_resource(queue);
            run_hud_buttons(&mut GameScene { world });
            world.get::<Dialogue>(smith).unwrap().current.clone()
        };
        assert_eq!(click(&mut world, &[other]), "hi");
        assert_eq!(click(&mut world, &[next]), "ask", "Continue");
        assert_eq!(click(&mut world, &[second]), "", "an empty next ends");
        assert_eq!(GameScene { world: &mut world }.counter_value("asked"), 1);
    }

    #[test]
    fn clicking_a_binding_button_rebinds_to_the_next_key() {
        use crate::runtime::KeyCode;
        let mut world = World::new();
        world.insert_resource(RuntimeInput::default());
        let button = world
            .spawn(crate::runtime::HudElement {
                text: "{binding:jump}".into(),
                button: true,
                ..Default::default()
            })
            .id();
        let frame = |world: &mut World, click: bool, key: Option<KeyCode>| {
            let mut queue =
                EventQueue::<crate::runtime::HudButtonPressed>::default();
            if click {
                queue.send(crate::runtime::HudButtonPressed { entity: button });
            }
            queue.begin_frame();
            world.insert_resource(queue);
            let mut input = RuntimeInput::default();
            if let Some(key) = key {
                input.record_key(key, true);
            }
            world.insert_resource(input);
            run_hud_buttons(&mut GameScene { world });
            GameScene { world }.binding("jump")
        };
        GameScene { world: &mut world }
            .rebind("jump", &["Space", "PadSouth"])
            .unwrap();
        assert_eq!(frame(&mut world, true, None), ["Space", "PadSouth"]);
        assert_eq!(
            world.resource::<crate::runtime::RebindWait>().0.as_deref(),
            Some("jump")
        );
        assert_eq!(
            frame(&mut world, false, Some(KeyCode::KeyJ)),
            ["KeyJ", "PadSouth"],
            "a key replaces the key and keeps the pad button"
        );
        assert_eq!(
            frame(&mut world, false, Some(KeyCode::KeyK)),
            ["KeyJ", "PadSouth"],
            "only the key after a click rebinds"
        );
        frame(&mut world, true, None);
        assert_eq!(
            frame(&mut world, false, Some(KeyCode::Escape)),
            ["KeyJ", "PadSouth"],
            "Escape cancels"
        );
        assert_eq!(world.resource::<crate::runtime::RebindWait>().0, None);
    }

    #[test]
    fn text_scale_is_clamped_and_read_back() {
        let mut world = World::new();
        let mut scene = GameScene { world: &mut world };
        assert!((scene.text_scale() - 1.0).abs() < f32::EPSILON);
        scene.set_text_scale(1.5);
        assert!((scene.text_scale() - 1.5).abs() < f32::EPSILON);
        scene.set_text_scale(9.0);
        assert!((scene.text_scale() - 3.0).abs() < f32::EPSILON);
        scene.set_text_scale(f32::NAN);
        assert!((scene.text_scale() - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn clicked_names_the_hud_buttons_pressed_last_frame() {
        let mut world = World::new();
        assert!(GameScene { world: &mut world }.clicked().is_empty());
        let button = world.spawn(Name("Resume".into())).id();
        let mut queue =
            EventQueue::<crate::runtime::HudButtonPressed>::default();
        queue.send(crate::runtime::HudButtonPressed { entity: button });
        queue.begin_frame();
        world.insert_resource(queue);
        assert_eq!(GameScene { world: &mut world }.clicked(), ["Resume"]);
    }

    #[test]
    fn frame_percentiles_use_the_nearest_rank() {
        // 100 frames of 10 ms with six slow ones: p95 sees the slow tail.
        let mut lengths = vec![10.0; 94];
        lengths.extend([20.0, 20.0, 20.0, 20.0, 20.0, 40.0]);
        lengths.reverse();
        assert_eq!(frame_percentiles(&mut lengths), [10.0, 20.0, 20.0, 40.0]);
        assert_eq!(frame_percentiles(&mut []), [0.0; 4]);
    }

    #[test]
    fn a_cooked_scene_older_than_its_source_is_reported() {
        let folder = std::env::temp_dir()
            .join(format!("rusting-stale-cook-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(folder.join("scenes")).unwrap();
        std::fs::create_dir_all(folder.join("build")).unwrap();
        std::fs::write(
            folder.join("project.json"),
            r#"{"name": "g", "main_scene": "scenes/main.rscene", "cooked_scene": "build/main.rscene.bin"}"#,
        )
        .unwrap();
        let source = folder.join("scenes/main.rscene");
        let cooked = folder.join("build/main.rscene.bin");
        std::fs::write(&source, "{}").unwrap();
        std::fs::write(&cooked, "").unwrap();
        let age = |path: &Path, seconds: u64| {
            std::fs::File::options()
                .write(true)
                .open(path)
                .unwrap()
                .set_modified(
                    std::time::SystemTime::UNIX_EPOCH
                        + std::time::Duration::from_secs(seconds),
                )
                .unwrap();
        };
        age(&cooked, 1_000);
        age(&source, 2_000);
        // No Cargo.toml: an exported folder, whose file times mean nothing.
        assert_eq!(stale_cooked_scene_warning(&folder, &cooked), None);
        std::fs::write(folder.join("Cargo.toml"), "").unwrap();
        let warning = stale_cooked_scene_warning(&folder, &cooked).unwrap();
        assert!(warning.contains("rusting cook"), "{warning}");
        age(&cooked, 3_000);
        assert_eq!(stale_cooked_scene_warning(&folder, &cooked), None);
        let _ = std::fs::remove_dir_all(folder);
    }

    #[test]
    fn a_cooked_scene_from_another_engine_build_falls_back_to_the_source() {
        let folder = std::env::temp_dir()
            .join(format!("rusting-mismatch-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(folder.join("build")).unwrap();
        std::fs::write(
            folder.join("project.json"),
            r#"{"name": "g", "main_scene": "scenes/main.rscene", "cooked_scene": "build/main.rscene.bin"}"#,
        )
        .unwrap();
        let mut editor = App::new();
        editor.add_plugin(AssetPlugin).unwrap();
        editor.spawn((Name("Ball".into()), Transform::new([0.0, 5.0, 0.0])));
        crate::runtime::save_scene(
            editor.world_mut(),
            folder.join("scenes/main.rscene"),
            "main",
        )
        .unwrap();
        // A layout this engine cannot decode, as an older CLI would cook.
        let cooked = folder.join("build/main.rscene.bin");
        std::fs::write(&cooked, b"RSCENE01\x09\0\0\0\x10\x10\x10").unwrap();
        fn idle(_: &mut GameScene<'_>, _: &FrameTime) {}
        let plugin = || SimpleGamePlugin {
            update: idle,
            tick: None,
            components: None,
        };
        // An export (no Cargo.toml) carries `scenes/` too.
        let mut runtime = load_project_runtime(&cooked, plugin()).unwrap();
        let mut scene = GameScene {
            world: runtime.world_mut(),
        };
        assert!(scene.try_object("Ball").is_some());
        // With no source, the error says what to do.
        std::fs::remove_dir_all(folder.join("scenes")).unwrap();
        let error = load_project_runtime(&cooked, plugin())
            .err()
            .unwrap()
            .to_string();
        let _ = std::fs::remove_dir_all(&folder);
        assert!(
            error.contains("cooked by a different engine build")
                && error.contains("Recook it"),
            "{error}"
        );
    }

    #[test]
    fn gpu_bodies_are_counted_inside_a_box_from_their_mirrors() {
        use crate::runtime::{GpuStateMirror, ObjectClasses};
        let mut world = World::new();
        let mirror = |x: f32| GpuStateMirror {
            tick: 3,
            transform: Transform {
                position: [x, 0.5, 0.0],
                ..Transform::default()
            },
            linear_velocity: [0.0; 3],
            angular_velocity: [0.0; 3],
            custom_values: None,
        };
        for x in [0.0, 1.0, 2.5] {
            world.spawn((ObjectClasses::new(["ball"]), mirror(x)));
        }
        // Inside, but another class, or no snapshot yet.
        world.spawn((ObjectClasses::new(["crate"]), mirror(0.0)));
        world.spawn(ObjectClasses::new(["ball"]));
        let mut scene = GameScene { world: &mut world };
        let count = |scene: &mut GameScene<'_>, class| {
            scene.count_gpu_bodies_in_box(
                class,
                [-1.0, 0.0, -1.0],
                [1.0, 1.0, 1.0],
            )
        };
        assert_eq!(count(&mut scene, "ball"), 2);
        assert_eq!(count(&mut scene, "crate"), 1);
        assert_eq!(count(&mut scene, "enemy"), 0);
    }

    #[test]
    fn gpu_commands_reach_the_named_body_once_it_has_an_id() {
        use crate::runtime::{
            GpuBodyCommand, GpuPhysicsCommands, PhysicsIdRegistry,
        };
        let mut world = World::new();
        let ball = world.spawn(Name("Ball".into())).id();
        world.spawn(Name("Fresh".into()));
        let mut registry = PhysicsIdRegistry::default();
        registry.assign(world.spawn_empty().id());
        let id = registry.assign(ball);
        world.insert_resource(registry);
        let mut scene = GameScene { world: &mut world };
        let teleport =
            GpuBodyCommand::Teleport(Transform::new([0.0, 5.0, 0.0]));
        assert!(scene.gpu_command("Ball", teleport));
        assert!(!scene.gpu_command("Fresh", teleport), "no id yet");
        assert!(!scene.gpu_command("Missing", teleport));
        assert_eq!(
            world.resource::<GpuPhysicsCommands>().commands,
            [(id, teleport)]
        );
    }

    #[test]
    fn hit_stop_holds_back_real_time_then_lets_it_through() {
        use crate::runtime::{after_hit_stop, HitStop};
        use std::time::Duration;
        let ms = Duration::from_millis;
        let mut world = World::new();
        world.init_resource::<HitStop>();
        let mut scene = GameScene { world: &mut world };
        scene.hit_stop(0.05);
        scene.hit_stop(0.02); // a shorter stop does not cut it short
        scene.hit_stop(-1.0);
        assert_eq!(after_hit_stop(&mut world, ms(16)), ms(0));
        assert_eq!(after_hit_stop(&mut world, ms(16)), ms(0));
        assert_eq!(after_hit_stop(&mut world, ms(16)), ms(0));
        assert_eq!(after_hit_stop(&mut world, ms(16)), ms(14));
        assert_eq!(after_hit_stop(&mut world, ms(16)), ms(16));
        GameScene { world: &mut world }.hit_stop(9.0);
        assert_eq!(world.resource::<HitStop>().remaining, ms(1000));
    }

    #[test]
    fn a_timed_press_reaches_the_tick_it_happened_in() {
        use crate::runtime::{InputBinding, KeyCode};
        use std::sync::Mutex;
        static SEEN: Mutex<Vec<u64>> = Mutex::new(Vec::new());
        fn idle(_: &mut GameScene<'_>, _: &FrameTime) {}
        fn tick(scene: &mut GameScene<'_>, time: &FrameTime) {
            if scene.pressed("fire") {
                SEEN.lock().unwrap().push(time.fixed_tick);
            }
        }
        let mut app = App::new();
        app.add_plugin(SimpleGamePlugin {
            update: idle,
            tick: Some(tick),
            components: None,
        })
        .unwrap();
        app.world_mut()
            .resource_mut::<crate::runtime::ActionMap>()
            .bind("fire", InputBinding::Key(KeyCode::KeyF));
        app.world_mut()
            .resource_mut::<crate::runtime::ActionMap>()
            .bind("fire", InputBinding::Key(KeyCode::KeyG));
        let step = app
            .world()
            .resource::<crate::runtime::TimeControl>()
            .fixed_delta;
        // One frame runs ticks 0, 1 and 2. A press at 1.5 belongs to tick
        // 1; one at 3.2 happened after them and waits for tick 3.
        let mut input = app.world_mut().resource_mut::<RuntimeInput>();
        input.record_key(KeyCode::KeyF, true);
        input.record_press_tick(InputBinding::Key(KeyCode::KeyF), 1.5);
        input.record_key(KeyCode::KeyF, false);
        input.record_key(KeyCode::KeyG, true);
        input.record_press_tick(InputBinding::Key(KeyCode::KeyG), 3.2);
        app.update_exact(step * 3).unwrap();
        app.world_mut()
            .resource_mut::<RuntimeInput>()
            .clear_frame_edges();
        assert_eq!(*SEEN.lock().unwrap(), [1]);
        app.update_exact(step * 2).unwrap();
        assert_eq!(*SEEN.lock().unwrap(), [1, 3]);
    }

    #[test]
    fn a_tick_function_sees_a_press_from_a_frame_without_a_tick() {
        use crate::runtime::{InputBinding, KeyCode};
        use std::time::Duration;
        static TICKS: AtomicU32 = AtomicU32::new(0);
        static PRESSES: AtomicU32 = AtomicU32::new(0);
        fn idle(_: &mut GameScene<'_>, _: &FrameTime) {}
        fn tick(scene: &mut GameScene<'_>, _: &FrameTime) {
            TICKS.fetch_add(1, Ordering::Relaxed);
            if scene.pressed("fire") {
                PRESSES.fetch_add(1, Ordering::Relaxed);
            }
        }
        let mut app = App::new();
        app.add_plugin(SimpleGamePlugin {
            update: idle,
            tick: Some(tick),
            components: None,
        })
        .unwrap();
        app.world_mut()
            .resource_mut::<crate::runtime::ActionMap>()
            .bind("fire", InputBinding::Key(KeyCode::KeyF));
        let step = app
            .world()
            .resource::<crate::runtime::TimeControl>()
            .fixed_delta;
        let frame = |app: &mut App, delta: Duration, press: bool| {
            let mut input = app.world_mut().resource_mut::<RuntimeInput>();
            input.record_key(KeyCode::KeyF, press);
            app.update_exact(delta).unwrap();
            app.world_mut()
                .resource_mut::<RuntimeInput>()
                .clear_frame_edges();
        };
        // A fast frame with the press runs no tick; the next frame runs
        // three. Only the first of the three sees the press.
        frame(&mut app, step / 4, true);
        assert_eq!(TICKS.load(Ordering::Relaxed), 0);
        frame(&mut app, step * 3, false);
        assert_eq!(TICKS.load(Ordering::Relaxed), 3);
        assert_eq!(PRESSES.load(Ordering::Relaxed), 1);
        // A press on a frame that ticks counts once too.
        frame(&mut app, step, true);
        frame(&mut app, step, false);
        assert_eq!(TICKS.load(Ordering::Relaxed), 5);
        assert_eq!(PRESSES.load(Ordering::Relaxed), 2);
        // Pause stops `fixed_tick` and tick functions; `frame` counts on.
        GameScene {
            world: app.world_mut(),
        }
        .set_paused(true);
        let before = *app.world().resource::<FrameTime>();
        frame(&mut app, step * 2, false);
        let after = *app.world().resource::<FrameTime>();
        assert_eq!(after.fixed_tick, before.fixed_tick);
        assert_eq!(after.frame, before.frame + 1);
        assert_eq!(TICKS.load(Ordering::Relaxed), 5);
    }

    #[test]
    fn headless_simulation_runs_exact_ticks_from_a_cooked_scene() {
        let directory = std::env::temp_dir()
            .join(format!("rusting-headless-{}", uuid::Uuid::new_v4()));
        let source = directory.join("main.rscene");
        let cooked = directory.join("main.rscene.bin");
        let mut editor = App::new();
        editor.add_plugin(AssetPlugin).unwrap();
        editor.spawn((
            Name("Ball".into()),
            Transform::new([0.0, 5.0, 0.0]),
            crate::runtime::PhysicsBody::default(),
            crate::runtime::RigidBody::default(),
            crate::runtime::Collider::default(),
        ));
        crate::runtime::save_scene(editor.world_mut(), &source, "main")
            .unwrap();
        crate::runtime::cook_scene(&source, &cooked).unwrap();
        fn idle(_: &mut GameScene<'_>, _: &FrameTime) {}
        let (mut runtime, report) = simulate_project_headless(
            &cooked,
            SimpleGamePlugin {
                update: idle,
                tick: None,
                components: None,
            },
            30,
        )
        .unwrap();
        std::fs::remove_dir_all(&directory).unwrap();

        assert_eq!(runtime.world().resource::<FrameTime>().fixed_tick, 30);
        let hashes = &runtime
            .world()
            .resource::<crate::runtime::StateHashes>()
            .recent;
        assert_eq!(hashes.len(), 30);
        assert!(hashes.iter().eq(&report.ticks));
        assert_eq!(hashes.back().unwrap().0, 30);
        let mut scene = GameScene {
            world: runtime.world_mut(),
        };
        assert!(scene.object("Ball").position()[1] < 5.0);
    }

    #[test]
    fn a_dedicated_server_ticks_in_real_time_until_the_game_quits() {
        static TICKS: AtomicU32 = AtomicU32::new(0);
        fn serve(scene: &mut GameScene<'_>, _: &FrameTime) {
            let ticks = TICKS.fetch_add(1, Ordering::Relaxed);
            let server = *scene.world.resource::<crate::net::DedicatedServer>();
            assert_eq!(server.ticks, u64::from(ticks));
            if ticks == 2 {
                // Overrun the step: tick 3 starts late and reports it.
                std::thread::sleep(FrameTime::default().fixed_delta * 3);
            }
            if ticks == 3 {
                assert!(server.last_budget > 2.5, "{server:?}");
                assert!(server.budget > 0.0);
                assert!(server.late_ticks >= 1, "{server:?}");
                assert_eq!(server.dropped_ticks, 0);
            }
            if ticks + 1 == 6 {
                scene.quit();
            }
        }
        let directory = std::env::temp_dir()
            .join(format!("rusting-server-{}", uuid::Uuid::new_v4()));
        let source = directory.join("main.rscene");
        let cooked = directory.join("main.rscene.bin");
        let mut editor = App::new();
        editor.add_plugin(AssetPlugin).unwrap();
        crate::runtime::save_scene(editor.world_mut(), &source, "main")
            .unwrap();
        crate::runtime::cook_scene(&source, &cooked).unwrap();
        let start = Instant::now();
        run_project_server(
            &cooked,
            SimpleGamePlugin {
                update: serve,
                tick: None,
                components: None,
            },
        )
        .unwrap();
        let elapsed = start.elapsed();
        std::fs::remove_dir_all(&directory).unwrap();
        assert_eq!(TICKS.load(Ordering::Relaxed), 6);
        // Five sleeps of one fixed step each, not a busy loop.
        let step = FrameTime::default().fixed_delta;
        assert!(elapsed >= step * 5, "{elapsed:?}");
    }

    #[test]
    fn an_export_layout_finds_its_assets_next_to_project_json() {
        // What `rusting export` writes: no Cargo.toml, the cooked scene
        // under `build/`, `project.json` and `assets/` at the top.
        let directory = std::env::temp_dir()
            .join(format!("rusting-export-{}", uuid::Uuid::new_v4()));
        let source = directory.join("main.rscene");
        let cooked = directory.join("build/main.rscene.bin");
        std::fs::create_dir_all(directory.join("build")).unwrap();
        std::fs::write(directory.join("project.json"), "{}").unwrap();
        let mut editor = App::new();
        editor.add_plugin(AssetPlugin).unwrap();
        crate::runtime::save_scene(editor.world_mut(), &source, "main")
            .unwrap();
        crate::runtime::cook_scene(&source, &cooked).unwrap();
        fn idle(_: &mut GameScene<'_>, _: &FrameTime) {}
        let (runtime, _) = simulate_project_headless(
            &cooked,
            SimpleGamePlugin {
                update: idle,
                tick: None,
                components: None,
            },
            1,
        )
        .unwrap();
        assert_eq!(runtime.world().resource::<ProjectFolder>().0, directory);
        std::fs::remove_dir_all(&directory).unwrap();
    }

    #[test]
    fn headless_simulation_clears_a_press_game_code_injects_after_one_update() {
        use crate::runtime::KeyCode;
        static PRESSED: AtomicBool = AtomicBool::new(false);
        static PRESSES: AtomicU32 = AtomicU32::new(0);
        // Like a playtest bot: press R once and keep it held.
        fn press_once(scene: &mut GameScene<'_>, _: &FrameTime) {
            let mut input = scene.world.resource_mut::<RuntimeInput>();
            if !PRESSED.swap(true, Ordering::Relaxed) {
                input.record_key(KeyCode::KeyR, true);
            }
            if input.key_just_pressed(KeyCode::KeyR) {
                PRESSES.fetch_add(1, Ordering::Relaxed);
            }
        }
        let directory = std::env::temp_dir()
            .join(format!("rusting-headless-{}", uuid::Uuid::new_v4()));
        let source = directory.join("main.rscene");
        let cooked = directory.join("main.rscene.bin");
        let mut editor = App::new();
        editor.add_plugin(AssetPlugin).unwrap();
        crate::runtime::save_scene(editor.world_mut(), &source, "main")
            .unwrap();
        crate::runtime::cook_scene(&source, &cooked).unwrap();
        simulate_project_headless(
            &cooked,
            SimpleGamePlugin {
                update: press_once,
                tick: None,
                components: None,
            },
            10,
        )
        .unwrap();
        std::fs::remove_dir_all(&directory).unwrap();
        assert_eq!(PRESSES.load(Ordering::Relaxed), 1);
    }

    #[derive(
        bevy_ecs::component::Component,
        Clone,
        Default,
        serde::Serialize,
        serde::Deserialize,
    )]
    struct Health {
        hp: i32,
    }

    crate::reflect! {
        struct Health {
            hp: i32,
        }
    }

    /// The same component after a change that old saved values do not fit.
    #[derive(
        bevy_ecs::component::Component,
        Clone,
        Default,
        serde::Serialize,
        serde::Deserialize,
    )]
    struct HealthText {
        hp: String,
    }

    crate::reflect! {
        struct HealthText {
            hp: String,
        }
    }

    /// One build of a test game: its update function and its `Health`.
    struct TestGame {
        update: GameUpdate,
        changed_health: bool,
    }

    impl Plugin for TestGame {
        fn build(&self, app: &mut App) -> Result<(), AppError> {
            let registered = if self.changed_health {
                app.register_scene_component::<HealthText>("test.health")
                    .map(drop)
            } else {
                app.register_scene_component::<Health>("test.health")
                    .map(drop)
            };
            registered.map_err(|error| AppError::PluginSetup {
                plugin: "TestGame",
                message: error.to_string(),
            })?;
            app.add_plugin(SimpleGamePlugin {
                update: self.update,
                tick: None,
                components: None,
            })?;
            app.add_system(ScheduleStage::Startup, |world: &mut World| {
                world.spawn((
                    crate::runtime::SceneId::new(),
                    Name("Marker".into()),
                ));
            });
            Ok(())
        }
    }

    /// A game written the way `rusting_game!` documents custom components.
    #[allow(dead_code)]
    mod component_game {
        use crate::prelude::*;

        #[derive(Component, Clone, Default, Serialize, Deserialize)]
        #[serde(crate = "crate::serde")]
        pub struct Night {
            pub power: i32,
        }

        crate::reflect! {
            struct Night {
                power: i32 { doc: "power left" },
            }
        }

        fn update(_scene: &mut GameScene<'_>, _time: &FrameTime) {}

        crate::rusting_game!(update, components: [Night => "game.night"]);

        pub(super) const COMPONENTS: super::GameComponents =
            rusting_game_components;
    }

    #[test]
    fn rusting_game_components_are_saved_in_scenes() {
        let mut app = App::new();
        app.add_plugin(AssetPlugin).unwrap();
        app.add_plugin(SimpleGamePlugin {
            update: |_, _| {},
            tick: None,
            components: Some(component_game::COMPONENTS),
        })
        .unwrap();
        app.spawn((
            crate::runtime::SceneId::new(),
            Name("Clock".into()),
            component_game::Night { power: 7 },
        ));
        let path = std::env::temp_dir().join(format!(
            "rusting-components-{}.rscene",
            uuid::Uuid::new_v4()
        ));
        crate::runtime::save_scene(app.world_mut(), &path, "main").unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        std::fs::remove_file(&path).ok();
        let document: serde_json::Value = serde_json::from_str(&text).unwrap();
        let clock = document["entities"]
            .as_array()
            .unwrap()
            .iter()
            .find(|entity| entity["name"] == "Clock")
            .unwrap();
        // Scenes keep a game component as its JSON text.
        let night: serde_json::Value = serde_json::from_str(
            clock["components"]["game.night"].as_str().unwrap(),
        )
        .unwrap();
        assert_eq!(night["power"], 7, "{text}");
    }

    fn count_named(world: &mut World, name: &str) -> usize {
        let mut query = world.query::<&Name>();
        query.iter(world).filter(|named| named.0 == name).count()
    }

    #[test]
    fn code_reload_keeps_the_scene_and_runs_the_new_code() {
        let directory = std::env::temp_dir()
            .join(format!("rusting-code-reload-{}", uuid::Uuid::new_v4()));
        let source = directory.join("main.rscene");
        let cooked = directory.join("main.rscene.bin");
        let state = directory.join("state.json");
        let mut editor = App::new();
        editor.add_plugin(AssetPlugin).unwrap();
        editor.spawn((Name("Ball".into()), Transform::new([0.0, 0.0, 0.0])));
        crate::runtime::save_scene(editor.world_mut(), &source, "main")
            .unwrap();
        crate::runtime::cook_scene(&source, &cooked).unwrap();

        // The old code spawns a coin, gives the ball health, then moves the
        // ball right and hurts it every frame.
        fn old_code(scene: &mut GameScene<'_>, _: &FrameTime) {
            scene.once("setup", |scene| {
                scene.spawn_cube(
                    "Coin",
                    Transform::default(),
                    &CubeSpawn::new(),
                );
                let ball = find_named_entity(scene.world, "Ball").unwrap();
                scene.world.entity_mut(ball).insert(Health { hp: 10 });
            });
            scene.object("Ball").move_x(1.0);
            let ball = find_named_entity(scene.world, "Ball").unwrap();
            scene.world.get_mut::<Health>(ball).unwrap().hp -= 1;
        }
        // The changed system moves it up instead. Its setup runs only on a
        // clean start.
        fn new_code(scene: &mut GameScene<'_>, _: &FrameTime) {
            scene.once("setup", |scene| {
                scene.spawn_cube(
                    "Fresh",
                    Transform::default(),
                    &CubeSpawn::new(),
                );
            });
            scene.object("Ball").move_y(1.0);
        }
        let frame = std::time::Duration::from_millis(16);
        let mut old = load_project_runtime(
            &cooked,
            TestGame {
                update: old_code,
                changed_health: false,
            },
        )
        .unwrap();
        for _ in 0..3 {
            old.update(frame).unwrap();
        }
        save_code_reload_state(&mut old, &state).unwrap();

        let mut new = load_project_runtime(
            &cooked,
            TestGame {
                update: new_code,
                changed_health: false,
            },
        )
        .unwrap();
        restore_code_reload_state(&mut new, &state).unwrap();
        for _ in 0..2 {
            new.update(frame).unwrap();
        }
        let world = new.world_mut();
        let ball = find_named_entity(world, "Ball").unwrap();
        assert_eq!(
            world.get::<Transform>(ball).unwrap().position,
            [3.0, 2.0, 0.0]
        );
        assert_eq!(world.get::<Health>(ball).unwrap().hp, 7);
        assert_eq!(count_named(world, "Coin"), 1);
        assert_eq!(count_named(world, "Fresh"), 0);
        // Startup ran in the old process only.
        assert_eq!(count_named(world, "Marker"), 1);

        // Saved values that do not fit the changed component start clean.
        let mut changed = load_project_runtime(
            &cooked,
            TestGame {
                update: new_code,
                changed_health: true,
            },
        )
        .unwrap();
        let error = restore_code_reload_state(&mut changed, &state)
            .unwrap_err()
            .to_string();
        assert!(error.contains("test.health"), "{error}");
        changed.update(frame).unwrap();
        let world = changed.world_mut();
        let ball = find_named_entity(world, "Ball").unwrap();
        assert_eq!(
            world.get::<Transform>(ball).unwrap().position,
            [0.0, 1.0, 0.0]
        );
        assert_eq!(count_named(world, "Coin"), 0);
        assert_eq!(count_named(world, "Fresh"), 1);
        assert_eq!(count_named(world, "Marker"), 1);
        std::fs::remove_dir_all(&directory).unwrap();
    }

    #[test]
    fn press_tick_is_the_event_time_or_the_frame_tick() {
        use crate::runtime::{ActionMap, InputBinding, KeyCode};
        let mut world = World::new();
        world.insert_resource(FrameTime {
            fixed_tick: 122,
            ..FrameTime::default()
        });
        let mut actions = ActionMap::default();
        actions.bind("mine", InputBinding::Key(KeyCode::Space));
        actions.bind("menu", InputBinding::Key(KeyCode::Escape));
        world.insert_resource(actions);
        let mut input = RuntimeInput::default();
        input.record_key(KeyCode::Space, true);
        input.record_press_tick(InputBinding::Key(KeyCode::Space), 121.4);
        input.record_press_tick(InputBinding::Key(KeyCode::Space), 121.9);
        input.record_key(KeyCode::Escape, true);
        world.insert_resource(input);
        let scene = GameScene { world: &mut world };
        assert_eq!(scene.press_tick("mine"), Some(121.4));
        assert_eq!(scene.press_tick("menu"), Some(122.0));
        assert_eq!(scene.press_tick("jump"), None);
    }

    #[test]
    fn sound_requests_are_queued_counted_and_drained() {
        let mut world = World::new();
        let mut scene = GameScene { world: &mut world };
        let id = scene.play_sound("sfx/hit.wav", 0.5);
        scene.play_sound_looped("sfx/hum.ogg", 1.0);
        scene.stop_sound(id);
        scene.set_master_volume(0.2);
        assert_eq!(scene.sounds_requested(), 2);
        let commands =
            world.resource_mut::<crate::runtime::AudioQueue>().drain();
        assert_eq!(commands.len(), 4);
        assert!(matches!(
            &commands[1],
            crate::runtime::AudioCommand::Play {
                sound: crate::runtime::Sound { looped: true, .. },
                ..
            }
        ));
    }

    #[test]
    fn restart_puts_the_starting_scene_back_and_reruns_setup() {
        let directory = std::env::temp_dir()
            .join(format!("rusting-restart-{}", uuid::Uuid::new_v4()));
        let source = directory.join("main.rscene");
        let cooked = directory.join("main.rscene.bin");
        let mut editor = App::new();
        editor.add_plugin(AssetPlugin).unwrap();
        editor.spawn((Name("Ball".into()), Transform::new([0.0, 0.0, 0.0])));
        editor.spawn((Name("Gate".into()), Transform::default()));
        crate::runtime::save_scene(editor.world_mut(), &source, "main")
            .unwrap();
        crate::runtime::cook_scene(&source, &cooked).unwrap();

        // Setup spawns a coin; the ball moves right each frame, the gate is
        // removed, and the third frame restarts.
        fn code(scene: &mut GameScene<'_>, _: &FrameTime) {
            if scene.object("Ball").position()[0] == 1.0 {
                scene.set_counter("score", 5);
            }
            scene.once("setup", |scene| {
                scene.spawn_cube(
                    "Coin",
                    Transform::default(),
                    &CubeSpawn::new(),
                );
                scene.play_sound_looped("hum.wav", 1.0);
            });
            scene.despawn("Gate");
            scene.object("Ball").move_x(1.0);
            if scene.object("Ball").position()[0] >= 3.0 {
                assert!(scene.restart());
            }
        }
        let frame = std::time::Duration::from_millis(16);
        let mut game = load_project_runtime(
            &cooked,
            TestGame {
                update: code,
                changed_health: false,
            },
        )
        .unwrap();
        for _ in 0..3 {
            game.update(frame).unwrap();
        }
        let world = game.world_mut();
        let ball = find_named_entity(world, "Ball").unwrap();
        assert_eq!(world.get::<Transform>(ball).unwrap().position, [0.0; 3]);
        assert_eq!(count_named(world, "Gate"), 1);
        assert_eq!(count_named(world, "Coin"), 0);
        assert_eq!(count_named(world, "score"), 0, "set_counter counters go");
        assert!(
            world
                .resource::<crate::runtime::RenderWorld>()
                .gpu_physics_reset,
            "GPU bodies restart from the scene file"
        );
        game.update(frame).unwrap();
        let world = game.world_mut();
        assert_eq!(count_named(world, "Coin"), 1, "setup ran again");
        assert_eq!(count_named(world, "Gate"), 0);
        let ball = find_named_entity(world, "Ball").unwrap();
        assert_eq!(world.get::<Transform>(ball).unwrap().position[0], 1.0);
        // Restart stops no sound: the looped hum from setup is started a
        // second time, as the restart docs warn.
        let commands =
            world.resource_mut::<crate::runtime::AudioQueue>().drain();
        let plays = commands
            .iter()
            .filter(|command| {
                matches!(command, crate::runtime::AudioCommand::Play { .. })
            })
            .count();
        assert_eq!(plays, 2);
        assert!(!commands.iter().any(|command| matches!(
            command,
            crate::runtime::AudioCommand::Stop(..)
                | crate::runtime::AudioCommand::StopAll
        )));

        let mut world = World::new();
        assert!(!GameScene { world: &mut world }.restart());
        std::fs::remove_dir_all(&directory).unwrap();
    }

    #[test]
    fn basis_forward_follows_the_rotation_order() {
        let mut world = World::new();
        let rotation = [0.3_f32, 1.0, 0.0];
        world.spawn((
            Name("Cam".into()),
            Transform {
                rotation,
                ..Transform::default()
            },
        ));
        let mut scene = GameScene { world: &mut world };
        let [_, _, forward] = scene.basis("Cam").unwrap();
        let (x, y) = (rotation[0], rotation[1]);
        let expected = [-y.sin() * x.cos(), x.sin(), -y.cos() * x.cos()];
        for (a, b) in forward.iter().zip(expected) {
            assert!((a - b).abs() < 1e-5, "{forward:?} {expected:?}");
        }
    }

    #[test]
    fn reparent_and_set_light_move_and_dim_a_lantern() {
        use crate::runtime::{Parent, PointLight};
        let mut world = World::new();
        let lantern = world
            .spawn((
                Name("Lantern".into()),
                Transform::default(),
                PointLight {
                    color: [1.0; 3],
                    intensity: 5.0,
                    range: 4.0,
                    shadows: false,
                    radius: 0.0,
                },
            ))
            .id();
        let hand = world
            .spawn((Name("Hand".into()), Transform::default()))
            .id();
        let mut scene = GameScene { world: &mut world };
        assert!(scene.reparent("Lantern", Some("Hand")));
        assert_eq!(
            scene.world().get::<Parent>(lantern).map(|p| p.0),
            Some(hand)
        );
        assert!(!scene.reparent("Hand", Some("Lantern")), "a loop");
        assert!(scene.reparent("Lantern", None));
        assert!(scene.world().get::<Parent>(lantern).is_none());
        assert!(scene.set_light("Lantern", None, Some(0.0), Some(2.0)));
        assert!(!scene.set_light("Hand", None, Some(1.0), None));
        let light = scene.world().get::<PointLight>(lantern).unwrap();
        assert_eq!(
            (light.color, light.intensity, light.range),
            ([1.0; 3], 0.0, 2.0)
        );
    }

    #[test]
    fn video_settings_write_render_settings_and_the_window_request() {
        let mut world = World::new();
        world.insert_resource(RenderSettings::default());
        let mut scene = GameScene { world: &mut world };
        scene.set_render_scale(0.1);
        assert_eq!(scene.render_scale(), 0.25);
        scene.set_render_scale(f32::NAN);
        assert_eq!(scene.render_scale(), 0.25);
        scene.set_vsync(true);
        scene.set_max_fps(Some(0));
        assert!(!scene.fullscreen());
        scene.set_fullscreen(true);
        scene.set_window_size([1280, 0]);
        assert!(scene.fullscreen());
        let settings = world.resource::<RenderSettings>();
        assert!(settings.vsync && settings.limit_fps);
        assert_eq!(settings.max_fps, 1);
        assert_eq!(world.resource::<WindowRequest>().size, Some([1280, 1]));
        GameScene { world: &mut world }.set_max_fps(None);
        assert!(!world.resource::<RenderSettings>().limit_fps);
        let mut scene = GameScene { world: &mut world };
        scene.set_exposure(2.5);
        scene.set_exposure(-1.0);
        let mut tones = world.query::<&crate::runtime::ToneMapping>();
        let tone: Vec<_> = tones.iter(&world).collect();
        assert_eq!(tone.len(), 1);
        assert_eq!(tone[0].exposure, 2.5);
        GameScene { world: &mut world }.set_exposure(0.5);
        assert_eq!(tones.iter(&world).next().unwrap().exposure, 0.5);
        let guard = world
            .spawn((
                Name("Guard".into()),
                crate::runtime::PlayerController::default(),
            ))
            .id();
        let mut scene = GameScene { world: &mut world };
        assert!(scene.set_mouse_look("Guard", false));
        assert!(!scene.set_mouse_look("Nobody", false));
        assert!(
            !world
                .get::<crate::runtime::PlayerController>(guard)
                .unwrap()
                .mouse_look
        );
        world
            .insert_resource(crate::runtime::SceneComponentRegistry::default());
        let mut scene = GameScene { world: &mut world };
        scene
            .set_field(
                "Guard",
                "/components/rusting.player_controller/walk_speed",
                2.5.into(),
            )
            .unwrap();
        let error = scene.set_field(
            "Guard",
            "/components/rusting.player_controller/nope",
            1.into(),
        );
        assert!(error.unwrap_err().contains("nope"));
        assert!(scene.set_field("Nobody", "/visible", true.into()).is_err());
        assert_eq!(
            world
                .get::<crate::runtime::PlayerController>(guard)
                .unwrap()
                .walk_speed,
            2.5
        );
    }

    #[test]
    fn body_calls_stop_spin_and_reset_bodies_and_set_look_turns_the_player() {
        use crate::runtime::{PlayerController, RigidBody, RigidBodyKind};
        let mut world = World::new();
        let crate_entity = world
            .spawn((
                Name("Crate".into()),
                Transform::default(),
                RigidBody {
                    kind: RigidBodyKind::Dynamic,
                    ..RigidBody::default()
                },
                crate::runtime::Sleeping,
            ))
            .id();
        world.spawn((
            Name("Player".into()),
            Transform::default(),
            PlayerController::default(),
        ));
        let mut scene = GameScene { world: &mut world };
        assert!(scene.set_angular_velocity("Crate", [0.0, 2.0, 0.0]));
        assert_eq!(scene.angular_velocity("Crate"), Some([0.0, 2.0, 0.0]));
        assert!(scene.set_linear_velocity("Crate", [1.0, 0.0, 0.0]));
        // Becoming kinematic freezes the body instead of carrying it off.
        assert!(scene.set_body_kind("Crate", RigidBodyKind::Kinematic));
        assert_eq!(scene.linear_velocity("Crate"), Some([0.0; 3]));
        assert_eq!(scene.angular_velocity("Crate"), Some([0.0; 3]));
        assert!(scene.set_body_kind("Crate", RigidBodyKind::Dynamic));
        assert!(scene.set_angular_velocity("Crate", [3.0, 0.0, 0.0]));
        assert!(scene.set_linear_velocity("Crate", [0.0, 5.0, 0.0]));
        assert!(scene.reset_body("Crate"));
        assert_eq!(scene.linear_velocity("Crate"), Some([0.0; 3]));
        assert_eq!(scene.angular_velocity("Crate"), Some([0.0; 3]));
        assert!(!scene.reset_body("Nothing"));
        assert!(scene
            .world()
            .get::<crate::runtime::Sleeping>(crate_entity)
            .is_none());

        assert!(scene.set_look("Player", 1.5, -0.25));
        assert!(!scene.set_look("Crate", 0.0, 0.0), "no controller");
        let mut players = scene.world().query::<&PlayerController>();
        let player = players.single(scene.world()).unwrap();
        assert_eq!((player.yaw, player.pitch), (1.5, -0.25));
    }

    #[test]
    fn restart_repeats_a_physics_pile_exactly() {
        use crate::runtime::{
            Collider, ColliderShape, PhysicsBody, RigidBody, RigidBodyKind,
            SimulationClass,
        };
        let directory = std::env::temp_dir()
            .join(format!("rusting-restart-pile-{}", uuid::Uuid::new_v4()));
        let source = directory.join("main.rscene");
        let cooked = directory.join("main.rscene.bin");
        let mut editor = App::new();
        editor.add_plugin(AssetPlugin).unwrap();
        let body = |kind| {
            (
                PhysicsBody {
                    simulation: SimulationClass::Cpu,
                    ..PhysicsBody::default()
                },
                RigidBody {
                    kind,
                    ..RigidBody::default()
                },
                Collider {
                    shape: ColliderShape::Box {
                        half_extents: [0.5; 3],
                    },
                    ..Collider::default()
                },
            )
        };
        let mut floor = Transform::new([0.0, -0.5, 0.0]);
        floor.scale = [20.0, 1.0, 20.0];
        editor.spawn((Name("Floor".into()), floor, body(RigidBodyKind::Fixed)));
        for index in 0..12 {
            let offset = index as f32;
            editor.spawn((
                Name(format!("Box {index}")),
                crate::runtime::ObjectClasses::new(["box"]),
                Transform::new([
                    (offset * 0.37).sin() * 0.4,
                    0.6 + offset * 1.05,
                    (offset * 0.61).cos() * 0.4,
                ]),
                body(RigidBodyKind::Dynamic),
            ));
        }
        crate::runtime::save_scene(editor.world_mut(), &source, "main")
            .unwrap();
        // Patched files list objects in creation order, not by id, while a
        // restart reloads a capture sorted by id.
        let mut json: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&source).unwrap()).unwrap();
        json["entities"].as_array_mut().unwrap().reverse();
        std::fs::write(&source, json.to_string()).unwrap();
        crate::runtime::cook_scene(&source, &cooked).unwrap();

        fn code(_: &mut GameScene<'_>, _: &FrameTime) {}
        let mut game = load_project_runtime(
            &cooked,
            TestGame {
                update: code,
                changed_health: false,
            },
        )
        .unwrap();
        let frame = game.world().resource::<FrameTime>().fixed_delta;
        let run = |game: &mut App| {
            for _ in 0..90 {
                game.update(frame).unwrap();
            }
            let world = game.world_mut();
            let hash = GameScene { world: &mut *world }.state_hash("box");
            let poses = (0..12)
                .map(|index| {
                    let entity =
                        find_named_entity(world, &format!("Box {index}"))
                            .unwrap();
                    let transform = world.get::<Transform>(entity).unwrap();
                    (transform.position, transform.rotation)
                })
                .collect::<Vec<_>>();
            (hash, poses)
        };
        let start = GameScene {
            world: game.world_mut(),
        }
        .state_hash("box");
        let first = run(&mut game);
        let mut scene = GameScene {
            world: game.world_mut(),
        };
        let initial = scene.initial("Box 11").unwrap();
        assert_eq!(
            initial.transform.position,
            [
                (11.0f32 * 0.37).sin() * 0.4,
                0.6 + 11.0 * 1.05,
                (11.0f32 * 0.61).cos() * 0.4
            ]
        );
        assert_eq!(initial.body_kind, Some(RigidBodyKind::Dynamic));
        assert!(initial.visible);
        assert_eq!(scene.initial("Nothing"), None);
        assert!(scene.restart());
        let second = run(&mut game);
        assert_ne!(first.0, start, "the pile moved");
        assert_eq!(first, second, "tolerance 0, and equal class hashes");

        // A snapshot taken mid-fall, with warm starts and some sleepers,
        // resumes exactly as the run that went on from it.
        for _ in 0..20 {
            game.update(frame).unwrap();
        }
        let snapshot = GameScene {
            world: game.world_mut(),
        }
        .snapshot()
        .unwrap();
        let went_on = run(&mut game);
        GameScene {
            world: game.world_mut(),
        }
        .restore(&snapshot)
        .unwrap();
        assert_eq!(run(&mut game), went_on, "restored run matches");
        std::fs::remove_dir_all(&directory).unwrap();
    }

    #[test]
    fn load_scene_switches_to_another_project_scene() {
        let directory = std::env::temp_dir()
            .join(format!("rusting-load-scene-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(directory.join("levels")).unwrap();
        std::fs::write(directory.join("project.json"), "{}").unwrap();
        let source = directory.join("main.rscene");
        let cooked = directory.join("build/main.rscene.bin");
        let mut editor = App::new();
        editor.add_plugin(AssetPlugin).unwrap();
        let ball = editor.spawn((Name("Ball".into()), Transform::default()));
        crate::runtime::save_scene(editor.world_mut(), &source, "main")
            .unwrap();
        crate::runtime::cook_scene(&source, &cooked).unwrap();
        editor.world_mut().despawn(ball);
        editor.spawn((Name("Door".into()), Transform::default()));
        crate::runtime::save_scene(
            editor.world_mut(),
            directory.join("levels/2.rscene"),
            "level 2",
        )
        .unwrap();

        // The first level loads the second at once; the second spawns a coin
        // once per round and restarts when the door has moved twice.
        fn code(scene: &mut GameScene<'_>, _: &FrameTime) {
            if scene.try_object("Ball").is_some() {
                assert!(scene.load_scene("levels/missing.rscene").is_err());
                assert!(scene.try_object("Ball").is_some(), "scene kept");
                scene.load_scene("levels/2.rscene").unwrap();
                return;
            }
            scene.once("setup", |scene| {
                scene.spawn_cube(
                    "Coin",
                    Transform::default(),
                    &CubeSpawn::new(),
                );
            });
            scene.object("Door").move_x(1.0);
            if scene.object("Door").position()[0] >= 2.0 {
                assert!(scene.restart());
            }
        }
        let frame = std::time::Duration::from_millis(16);
        let mut game = load_project_runtime(
            &cooked,
            TestGame {
                update: code,
                changed_health: false,
            },
        )
        .unwrap();
        game.update(frame).unwrap();
        let world = game.world_mut();
        assert_eq!(count_named(world, "Ball"), 0);
        assert_eq!(count_named(world, "Door"), 1);
        game.update(frame).unwrap();
        assert_eq!(count_named(game.world_mut(), "Coin"), 1);
        // Restarting returns to the second level, not the first.
        game.update(frame).unwrap();
        let world = game.world_mut();
        assert_eq!(count_named(world, "Ball"), 0);
        assert_eq!(count_named(world, "Coin"), 0);
        let door = find_named_entity(world, "Door").unwrap();
        assert_eq!(world.get::<Transform>(door).unwrap().position, [0.0; 3]);
        std::fs::remove_dir_all(&directory).unwrap();
    }

    #[test]
    fn named_game_object_moves_without_an_ecs_query_in_game_code() {
        let mut world = World::new();
        world.spawn((Name("Orange Cube".into()), Transform::default()));

        let mut scene = GameScene { world: &mut world };
        scene
            .object("Orange Cube")
            .move_x(2.0)
            .move_y(3.0)
            .move_z(4.0);

        assert_eq!(scene.object("Orange Cube").position(), [2.0, 3.0, 4.0]);
    }

    #[test]
    fn despawned_name_can_be_spawned_again() {
        let mut world = World::new();
        world.insert_resource(AssetServer::default());
        let mut scene = GameScene { world: &mut world };
        let first =
            scene.spawn_cube("Shot", Transform::default(), &CubeSpawn::new());
        scene.world.despawn(first);
        let second =
            scene.spawn_cube("Shot", Transform::default(), &CubeSpawn::new());
        assert_ne!(first, second);
    }

    #[test]
    fn a_run_seed_sets_the_random_seed() {
        let mut world = World::new();
        world.insert_resource(crate::runtime::RandomSeed(0));
        super::apply_seed(&mut world, None).unwrap();
        assert_eq!(world.resource::<crate::runtime::RandomSeed>().0, 0);
        super::apply_seed(&mut world, Some("42".into())).unwrap();
        assert_eq!(world.resource::<crate::runtime::RandomSeed>().0, 42);
        let error = super::apply_seed(&mut world, Some("x".into()));
        assert!(error.unwrap_err().contains("RUSTING_SEED"));
    }

    #[test]
    fn game_randomness_repeats_for_a_seed_and_varies_by_tick_and_stream() {
        let draw = |seed: u64, tick: u64, stream: u64| {
            let mut world = World::new();
            world.insert_resource(crate::runtime::RandomSeed(seed));
            world.insert_resource(FrameTime {
                fixed_tick: tick,
                ..FrameTime::default()
            });
            GameScene { world: &mut world }.random(stream)
        };
        let value = draw(7, 3, 0);
        assert!((0.0..1.0).contains(&value));
        assert_eq!(value, draw(7, 3, 0));
        assert_ne!(value, draw(8, 3, 0));
        assert_ne!(value, draw(7, 4, 0));
        assert_ne!(value, draw(7, 3, 1));
        // A world without a seed or time still draws, and its seed is 0.
        let mut world = World::new();
        let _ = GameScene { world: &mut world }.random(0);
        assert_eq!(GameScene { world: &mut world }.seed(), 0);
        world.insert_resource(crate::runtime::RandomSeed(42));
        assert_eq!(GameScene { world: &mut world }.seed(), 42);
    }

    #[test]
    fn set_player_edits_the_named_controller() {
        let mut world = World::new();
        world.spawn((
            Name("Player".into()),
            crate::runtime::PlayerController::default(),
        ));
        world.spawn(Name("Crate".into()));
        let mut scene = GameScene { world: &mut world };
        assert!(scene.set_player("Player", |pc| pc.walk_speed = 9.5));
        assert_eq!(scene.player("Player").unwrap().walk_speed, 9.5);
        assert!(!scene.set_player("Crate", |pc| pc.walk_speed = 1.0));
        assert!(!scene.set_player("Nobody", |pc| pc.walk_speed = 1.0));
    }

    #[test]
    fn game_code_draws_into_a_texture_it_created() {
        let mut world = World::new();
        world.insert_resource(AssetServer::default());
        let mut scene = GameScene { world: &mut world };
        let paper = scene.create_texture(crate::assets::TextureAsset {
            size: [1, 1],
            rgba8: vec![255; 4],
            color_space: crate::assets::TextureColorSpace::Srgb,
            sampler: crate::assets::TextureSampler::default(),
        });
        let revision = |world: &World| {
            world.resource::<AssetServer>().textures.revision(paper)
        };
        let before = revision(scene.world);
        assert!(scene.edit_texture(paper, |texture| texture.rgba8[0] = 0));
        // The renderer uploads a texture again when its revision moves.
        assert!(revision(scene.world) > before);
        let texture = scene.world.resource::<AssetServer>().textures.get(paper);
        assert_eq!(texture.unwrap().rgba8[0], 0);
    }

    #[test]
    fn game_code_reads_and_sets_counters_by_name() {
        let mut world = World::new();
        world.spawn(crate::runtime::Counter {
            name: "gems".into(),
            value: 2,
            target: Some(5),
        });
        let mut scene = GameScene { world: &mut world };
        scene.counter("gems").unwrap().value += 3;
        assert!(scene.counter("gems").unwrap().complete());
        assert!(scene.counter("coins").is_none());
        // The shorthands read, add and check without a borrow held open.
        assert_eq!(scene.add_to_counter("gems", -1), 4);
        assert_eq!(scene.counter_value("gems"), 4);
        assert!(!scene.counter_complete("gems"));
        // Reading a missing counter is 0; writing one creates it.
        assert_eq!(scene.counter_value("coins"), 0);
        assert!(!scene.counter_complete("coins"));
        assert!(scene.counter("coins").is_none(), "reads create nothing");
        assert_eq!(scene.add_to_counter("coins", 1), 1);
        scene.set_counter("gems", 9);
        assert_eq!(scene.counter_value("gems"), 9);
        scene.set_counter("coins", 3);
        assert_eq!(scene.counter_value("coins"), 3);
        scene.set_counter("night", 2);
        assert_eq!(scene.counter_value("night"), 2);
        assert!(!scene.counter_complete("night"), "created with no target");
        assert!(MISSING_COUNTERS.lock().unwrap().contains("coins"));
        // `counter_or` reads a default without warning (night-market F15).
        assert_eq!(scene.counter_or("unset_queue_slot", 7), 7);
        assert_eq!(scene.counter_or("gems", 7), 9);
        assert!(!MISSING_COUNTERS
            .lock()
            .unwrap()
            .contains("unset_queue_slot"));
        // A misspelt read names the close counters once.
        let names = ["money", "night", "coins"].into_iter();
        assert_eq!(nearest_hint(names, "mony"), "; did you mean `money`?");
        assert_eq!(nearest_hint(["money"].into_iter(), "screen"), "");
    }

    #[test]
    fn a_setter_given_a_missing_name_warns_once() {
        let mut world = World::new();
        world.spawn((Name("Lamp".into()), Transform::new([0.0; 3])));
        let mut scene = GameScene { world: &mut world };
        scene.set_color("Lamp", [1.0; 4]);
        scene.set_color("Lmap_typo", [1.0; 4]);
        scene.trigger("Lmap_typo");
        assert!(scene
            .spawn_copy("Lamp_template", "Copy", [0.0; 3])
            .is_none());
        let missing = MISSING_OBJECTS.lock().unwrap();
        assert!(missing.contains("Lmap_typo"));
        assert!(missing.contains("Lamp_template"));
        assert!(!missing.contains("Lamp"));
        drop(missing);
        assert_eq!(
            nearest_names_hint(&mut world, "Lmap"),
            "; did you mean `Lamp`?"
        );
    }

    #[test]
    fn game_code_recolors_one_object_without_touching_shared_materials() {
        let mut app = crate::App::new();
        app.add_plugin(crate::AssetPlugin).unwrap();
        let world = app.world_mut();
        let (mesh, material) = {
            let assets = world.resource::<AssetServer>();
            (assets.fallback_mesh, assets.fallback_material)
        };
        let renderer = MeshRenderer {
            mesh,
            material,
            cast_shadows: true,
            receive_shadows: true,
        };
        world.spawn((Name("A".into()), Transform::default(), renderer));
        world.spawn((Name("B".into()), Transform::default(), renderer));
        let count =
            |world: &World| world.resource::<AssetServer>().materials.len();
        let before = count(world);
        let mut scene = GameScene { world };
        let original = scene.color("A").unwrap();
        scene.set_color("A", [1.0, 0.0, 0.0, 1.0]);
        scene.set_emissive("A", [2.0, 0.0, 0.0]);
        assert_eq!(scene.color("A"), Some([1.0, 0.0, 0.0, 1.0]));
        assert_eq!(scene.color("B"), Some(original));
        assert_eq!(scene.color("Nowhere"), None);
        // Recoloring B the same way shares A's material.
        scene.set_color("B", [1.0, 0.0, 0.0, 1.0]);
        scene.set_emissive("B", [2.0, 0.0, 0.0]);
        let material = |scene: &mut GameScene<'_>, name| {
            let entity = find_named_entity(scene.world, name).unwrap();
            scene.world.get::<MeshRenderer>(entity).unwrap().material
        };
        assert_eq!(material(&mut scene, "A"), material(&mut scene, "B"));
        assert_eq!(count(scene.world), before + 2);
    }

    #[test]
    fn easing_many_colors_keeps_the_material_count_bounded() {
        let mut app = crate::App::new();
        app.add_plugin(crate::AssetPlugin).unwrap();
        let world = app.world_mut();
        let (mesh, material) = {
            let assets = world.resource::<AssetServer>();
            (assets.fallback_mesh, assets.fallback_material)
        };
        for index in 0..4000 {
            world.spawn((
                Name(format!("Bear {index}")),
                Transform::default(),
                MeshRenderer {
                    mesh,
                    material,
                    cast_shadows: true,
                    receive_shadows: true,
                },
            ));
        }
        let mut scene = GameScene { world };
        let names: Vec<String> =
            (0..900).map(|index| format!("Bear {index}")).collect();
        let start = Instant::now();
        for tick in 0..20 {
            for (index, name) in names.iter().enumerate() {
                let glow = (tick * 900 + index) as f32 * 1e-4;
                scene.set_emissive(name, [glow, 0.0, 0.0]);
            }
        }
        let per_call = start.elapsed() / (20 * 900);
        eprintln!("set_emissive: {per_call:?} per call");
        // 18,000 distinct colors, of which only the last 900 are on screen.
        let materials = scene.world.resource::<AssetServer>().materials.len();
        assert!(materials < 4000, "{materials} materials kept");
        let entity = find_named_entity(scene.world, "Bear 899").unwrap();
        let handle = scene.world.get::<MeshRenderer>(entity).unwrap().material;
        let assets = scene.world.resource::<AssetServer>();
        let glow = (19.0 * 900.0 + 899.0) * 1e-4;
        assert_eq!(assets.materials.get(handle).unwrap().emissive[0], glow);
    }

    #[test]
    #[cfg(feature = "ui")]
    fn game_code_puts_text_and_materials_on_existing_objects() {
        let mut app = crate::App::new();
        app.add_plugin(crate::AssetPlugin).unwrap();
        let world = app.world_mut();
        let (mesh, material) = {
            let assets = world.resource::<AssetServer>();
            (assets.fallback_mesh, assets.fallback_material)
        };
        let renderer = MeshRenderer {
            mesh,
            material,
            cast_shadows: true,
            receive_shadows: true,
        };
        world.spawn((Name("Sign".into()), Transform::default(), renderer));
        world.spawn((Name("Other".into()), Transform::default(), renderer));
        let textures =
            |world: &World| world.resource::<AssetServer>().textures.len();
        let before = textures(world);
        let mut scene = GameScene { world };
        let material_of = |scene: &mut GameScene<'_>, name| {
            let entity = find_named_entity(scene.world, name).unwrap();
            scene.world.get::<MeshRenderer>(entity).unwrap().material
        };
        let texture_of = |scene: &mut GameScene<'_>, name| {
            let handle = material_of(scene, name);
            let assets = scene.world.resource::<AssetServer>();
            assets.materials.get(handle).unwrap().base_color_texture
        };
        let style = crate::text_texture::TextStyle::default();
        let size = scene.set_text("Sign", "AISLE 4", style).unwrap();
        assert!(size[0] > size[1], "one line is wider than tall: {size:?}");
        let first = texture_of(&mut scene, "Sign").unwrap();
        assert_ne!(texture_of(&mut scene, "Other"), Some(first));
        // Switching back to a string drawn before reuses its texture.
        scene.set_text("Sign", "AISLE 5", style);
        assert_ne!(texture_of(&mut scene, "Sign"), Some(first));
        scene.set_text("Sign", "AISLE 4", style);
        assert_eq!(texture_of(&mut scene, "Sign"), Some(first));
        assert_eq!(textures(scene.world), before + 2);
        assert_eq!(scene.set_text("Nowhere", "x", style), None);
        // A font file under assets/ draws the same text as a new texture.
        let folder = std::env::temp_dir()
            .join(format!("rusting-set-text-font-{}", std::process::id()));
        std::fs::create_dir_all(folder.join("assets/fonts")).unwrap();
        let hack = &egui::FontDefinitions::default().font_data["Hack"].font;
        std::fs::write(folder.join("assets/fonts/hack.ttf"), hack).unwrap();
        std::fs::write(folder.join("assets/fonts/bad.ttf"), "no").unwrap();
        scene.world.insert_resource(ProjectFolder(folder.clone()));
        scene
            .set_text_in_font("Sign", "AISLE 4", style, "fonts/hack.ttf")
            .unwrap();
        assert_ne!(texture_of(&mut scene, "Sign"), Some(first));
        assert_eq!(textures(scene.world), before + 3);
        for font in ["fonts/bad.ttf", "fonts/missing.ttf"] {
            assert_eq!(scene.set_text_in_font("Sign", "x", style, font), None);
        }
        std::fs::remove_dir_all(folder).unwrap();
        // set_material swaps in a whole material made by game code.
        let red = scene.create_material(crate::assets::MaterialAsset {
            base_color: [1.0, 0.0, 0.0, 1.0],
            ..Default::default()
        });
        scene.set_material("Other", red);
        assert_eq!(material_of(&mut scene, "Other"), red);
        assert_eq!(scene.color("Other"), Some([1.0, 0.0, 0.0, 1.0]));
    }

    #[test]
    #[should_panic(expected = "did you mean `Crate`")]
    fn a_missing_object_name_panic_lists_the_nearest_names() {
        let mut world = World::new();
        world.spawn((Name("Crate".into()), Transform::new([0.0; 3])));
        world.spawn((Name("Ground".into()), Transform::new([0.0; 3])));
        GameScene { world: &mut world }.object("crate_");
    }

    #[test]
    fn game_code_reads_and_writes_tile_map_cells_by_world_position() {
        let mut world = World::new();
        world.spawn((
            Name("Level".into()),
            Transform::new([-2.0, 1.0, 0.0]),
            crate::runtime::TileMap {
                rows: vec!["#.o".into()],
                ..Default::default()
            },
        ));
        let mut scene = GameScene { world: &mut world };
        assert_eq!(scene.tile("Level", [-1.5, 0.5, 3.0]), Some('#'));
        assert_eq!(scene.tile("Level", [0.2, 0.9, 0.0]), Some('o'));
        assert_eq!(scene.tile("Level", [5.0, -4.0, 0.0]), Some('.'));
        assert_eq!(scene.tile("Level", [-2.5, 0.5, 0.0]), None);
        assert_eq!(scene.tile("Nowhere", [0.0; 3]), None);
        assert!(scene.set_tile("Level", [0.2, 0.9, 0.0], '*'));
        assert!(!scene.set_tile("Level", [0.2, 0.9, 0.0], '*'));
        assert!(scene.set_tile("Level", [-1.5, -0.5, 0.0], '#'));
        assert!(!scene.set_tile("Level", [-2.5, 0.5, 0.0], '#'));
        let map = world
            .query::<&crate::runtime::TileMap>()
            .single(&world)
            .unwrap();
        assert_eq!(map.rows, vec!["#.*", "#"]);
    }

    #[test]
    fn aim_hits_what_the_camera_faces_and_despawn_removes_it() {
        let mut app = App::new();
        let world = app.world_mut();
        let player = world
            .spawn((Name("Player".into()), Transform::new([0.0, 1.0, 0.0])))
            .id();
        let camera = world
            .spawn((
                Name("Camera".into()),
                // Turned 90 degrees left, so it faces -X.
                Transform::new([0.0, 0.5, 0.0]).with_rotation(
                    0.0,
                    std::f32::consts::FRAC_PI_2,
                    0.0,
                ),
                crate::runtime::Camera {
                    active: true,
                    ..Default::default()
                },
            ))
            .id();
        rusting_core::hierarchy::set_parent(world, camera, player).unwrap();
        for (name, x) in [("Left", -5.0), ("Right", 5.0)] {
            world.spawn((
                Name(name.into()),
                Transform::new([x, 1.5, 0.0]),
                crate::runtime::PhysicsBody::default(),
                crate::runtime::RigidBody {
                    kind: crate::runtime::RigidBodyKind::Fixed,
                    ..Default::default()
                },
                crate::runtime::Collider::default(),
            ));
        }
        let step = app.world().resource::<FrameTime>().fixed_delta;
        app.update(step).unwrap();
        let mut scene = GameScene {
            world: app.world_mut(),
        };
        let hit = scene.aim(20.0).expect("the camera faces Left");
        assert_eq!(hit.name, "Left");
        assert!((hit.point[0] + 4.5).abs() < 1e-3, "{hit:?}");
        assert!(scene.despawn("Left"));
        assert!(!scene.despawn("Left"));
        assert!(scene.try_object("Left").is_none());

        // Switching to a camera that faces +X aims at Right instead.
        let back = scene
            .world()
            .spawn((
                Name("Back Camera".into()),
                Transform::new([0.0, 1.5, 0.0]).with_rotation(
                    0.0,
                    -std::f32::consts::FRAC_PI_2,
                    0.0,
                ),
                crate::runtime::Camera::default(),
            ))
            .id();
        assert!(!scene.set_active_camera("Right"), "not a camera");
        assert!(scene.set_active_camera("Back Camera"));
        assert_eq!(scene.object("Back Camera").entity(), back);
        assert!(
            !scene
                .world()
                .get::<crate::runtime::Camera>(camera)
                .unwrap()
                .active
        );
        let hit = scene.aim(20.0).expect("the back camera faces Right");
        assert_eq!(
            scene.world().get::<Name>(back).unwrap().as_str(),
            "Back Camera"
        );
        assert_eq!(hit.name, "Right");

        assert!(scene.set_camera_fov("Back Camera", 0.5));
        assert_eq!(scene.camera_fov("Back Camera"), Some(0.5));
        assert!(!scene.set_camera_fov("Right", 0.5), "not a camera");
        assert_eq!(scene.camera_fov("Right"), None);
    }

    #[test]
    fn a_kinematic_template_copy_launches_along_the_camera_ray() {
        let mut app = App::new();
        let world = app.world_mut();
        world.spawn((
            Name("Camera".into()),
            // Turned 90 degrees left, so it faces -X.
            Transform::new([0.0, 2.0, 0.0]).with_rotation(
                0.0,
                std::f32::consts::FRAC_PI_2,
                0.0,
            ),
            crate::runtime::Camera {
                active: true,
                ..Default::default()
            },
        ));
        world.spawn((
            crate::runtime::SceneId::new(),
            Name("Ball".into()),
            Transform::new([0.0, -30.0, 0.0]),
            crate::runtime::PhysicsBody::default(),
            crate::runtime::RigidBody {
                kind: crate::runtime::RigidBodyKind::Kinematic,
                ..Default::default()
            },
            crate::runtime::Collider {
                shape: crate::runtime::ColliderShape::Sphere { radius: 0.2 },
                ..Default::default()
            },
        ));
        let step = app.world().resource::<FrameTime>().fixed_delta;
        let mut scene = GameScene {
            world: app.world_mut(),
        };
        let (origin, forward) = scene.camera_ray().unwrap();
        assert!((origin[1] - 2.0).abs() < 1e-5, "{origin:?}");
        assert!((forward[0] + 1.0).abs() < 1e-5, "{forward:?}");
        scene.spawn_copy("Ball", "Ball 1", origin);
        assert!(scene
            .set_body_kind("Ball 1", crate::runtime::RigidBodyKind::Dynamic));
        assert!(scene.set_linear_velocity("Ball 1", forward.map(|v| v * 10.0)));
        assert_eq!(
            scene.linear_velocity("Ball 1"),
            Some(forward.map(|v| v * 10.0))
        );
        assert_eq!(scene.linear_velocity("Nothing"), None);
        assert!(!scene
            .set_body_kind("Nothing", crate::runtime::RigidBodyKind::Dynamic));
        for _ in 0..30 {
            app.update(step).unwrap();
        }
        let mut scene = GameScene {
            world: app.world_mut(),
        };
        let ball = scene.object("Ball 1").position();
        assert!(ball[0] < -4.0 && ball[1] < 2.0, "flew and fell: {ball:?}");
        assert_eq!(scene.object("Ball").position(), [0.0, -30.0, 0.0]);
    }

    #[test]
    fn pointer_ray_goes_through_the_cursor() {
        let mut app = App::new();
        app.world_mut().spawn((
            Transform::new([0.0, 10.0, 0.0]).with_rotation(
                -std::f32::consts::FRAC_PI_2,
                0.0,
                0.0,
            ),
            crate::runtime::Camera {
                active: true,
                ..Default::default()
            },
        ));
        let mut scene = GameScene {
            world: app.world_mut(),
        };
        assert_eq!(scene.pointer_ray(), None, "no cursor yet");
        let mut input = scene.world.resource_mut::<RuntimeInput>();
        input.record_viewport_size([800.0, 600.0]);
        input.record_cursor_position([400.0, 300.0]);
        let (origin, down) = scene.pointer_ray().unwrap();
        assert!((origin[0].abs() + origin[2].abs()) < 1e-4, "{origin:?}");
        assert!((down[1] + 1.0).abs() < 1e-4, "{down:?}");
        scene
            .world
            .resource_mut::<RuntimeInput>()
            .record_cursor_position([800.0, 300.0]);
        let (_, right) = scene.pointer_ray().unwrap();
        assert!(right[0] > 0.1 && right[1] < 0.0, "{right:?}");
    }

    #[test]
    fn on_screen_places_points_in_the_view_and_rejects_the_rest() {
        let mut app = App::new();
        // 10 m up, looking straight down.
        app.world_mut().spawn((
            Transform::new([0.0, 10.0, 0.0]).with_rotation(
                -std::f32::consts::FRAC_PI_2,
                0.0,
                0.0,
            ),
            crate::runtime::Camera {
                active: true,
                ..Default::default()
            },
        ));
        let mut scene = GameScene {
            world: app.world_mut(),
        };
        scene
            .world
            .resource_mut::<RuntimeInput>()
            .record_viewport_size([800.0, 600.0]);
        let center = scene.on_screen([0.0; 3]).unwrap();
        assert!((center[0] - 0.5).abs() < 1e-4, "{center:?}");
        assert!((center[1] - 0.5).abs() < 1e-4, "{center:?}");
        let right = scene.on_screen([2.0, 0.0, 0.0]).unwrap();
        assert!(right[0] > 0.6, "{right:?}");
        // -Z is the top of a view looking down with no yaw.
        let top = scene.on_screen([0.0, 0.0, -2.0]).unwrap();
        assert!(top[1] < 0.4, "{top:?}");
        assert_eq!(scene.on_screen([0.0, 20.0, 0.0]), None, "behind");
        assert_eq!(scene.on_screen([100.0, 0.0, 0.0]), None, "off the side");
    }

    #[test]
    fn spawn_prefab_places_a_scene_file_under_a_named_root() {
        let folder = std::env::temp_dir()
            .join(format!("rusting-spawn-prefab-{}", std::process::id()));
        std::fs::create_dir_all(folder.join("assets/prefabs")).unwrap();
        std::fs::write(
            folder.join("assets/prefabs/tile.rscene"),
            serde_json::json!({"version": 9, "name": "Tile", "entities": [
                {"id": uuid::Uuid::new_v4(), "name": "Wall"}]})
            .to_string(),
        )
        .unwrap();
        let mut app = App::new();
        app.add_plugin(crate::AssetPlugin).unwrap();
        app.insert_resource(ProjectFolder(folder.clone()));
        let turned = Transform {
            rotation: [0.0, 1.5, 0.0],
            ..Transform::new([4.0, 0.0, 2.0])
        };
        let mut scene = GameScene {
            world: app.world_mut(),
        };
        let root = scene
            .spawn_prefab("prefabs/tile.rscene", "Tile 1", turned)
            .unwrap();
        assert!(scene
            .spawn_prefab("prefabs/none.rscene", "x", turned)
            .is_none());
        let world = app.world();
        assert_eq!(world.get::<Name>(root).unwrap().0, "Tile 1");
        assert_eq!(world.get::<Transform>(root), Some(&turned));
        let children = &world.get::<crate::runtime::Children>(root).unwrap().0;
        assert_eq!(world.get::<Name>(children[0]).unwrap().0, "Wall");
        std::fs::remove_dir_all(folder).unwrap();
    }

    #[test]
    #[should_panic(
        expected = "`Copy/Glow` already exists; object names are unique. If the scene file declares it"
    )]
    fn spawn_copy_refuses_a_child_name_that_is_taken() {
        let mut app = App::new();
        let world = app.world_mut();
        let template = world.spawn(Name("Ember".into())).id();
        let glow = world.spawn(Name("Glow".into())).id();
        rusting_core::hierarchy::set_parent(world, glow, template).unwrap();
        world.spawn(Name("Copy/Glow".into()));
        let mut scene = GameScene {
            world: app.world_mut(),
        };
        scene.spawn_copy("Ember", "Copy", [0.0; 3]);
    }

    #[test]
    fn spawn_grids_copy_the_tree_onto_cells_once() {
        let mut app = App::new();
        let world = app.world_mut();
        let pit = world.spawn((Name("Pit".into()), Transform::default())).id();
        let ball = world
            .spawn((
                crate::runtime::SceneId::new(),
                Name("Ball".into()),
                Transform::new([1.0, 2.0, 0.0]),
                crate::runtime::SpawnGrid {
                    count: [3, 2, 1],
                    spacing: [0.5, 1.0, 9.0],
                },
            ))
            .id();
        let shine = world
            .spawn((Name("Shine".into()), Transform::default()))
            .id();
        rusting_core::hierarchy::set_parent(world, shine, ball).unwrap();
        rusting_core::hierarchy::set_parent(world, ball, pit).unwrap();
        expand_spawn_grids(world);
        expand_spawn_grids(world);
        let mut scene = GameScene { world };
        assert_eq!(scene.object("Ball").position(), [1.0, 2.0, 0.0]);
        assert_eq!(scene.object("Ball#2").position(), [2.0, 2.0, 0.0]);
        assert_eq!(scene.object("Ball#5").position(), [2.0, 3.0, 0.0]);
        assert!(scene.try_object("Ball#6").is_none(), "copied once");
        assert!(scene.try_object("Ball#5/Shine").is_some());
        let copy = scene.object("Ball#3").entity();
        let world = app.world();
        assert_eq!(world.get::<crate::runtime::Parent>(copy).unwrap().0, pit);
        assert!(world.get::<crate::runtime::SpawnGrid>(copy).is_none());
        assert!(world.get::<crate::runtime::SpawnGrid>(ball).is_none());
    }

    #[test]
    fn look_at_points_the_forward_axis_at_the_target() {
        let mut world = World::new();
        let turret = world
            .spawn((Name("Turret".into()), Transform::new([1.0, 1.0, 1.0])))
            .id();
        let mut scene = GameScene { world: &mut world };
        for target in [[4.0, 1.0, 1.0], [1.0, 3.0, -1.0], [-2.0, 0.0, 5.0]] {
            scene.object("Turret").look_at(target);
            let m = scene.world.get::<Transform>(turret).unwrap().to_matrix();
            let forward = [-m[2][0], -m[2][1], -m[2][2]];
            let to = [0, 1, 2].map(|i| target[i] - 1.0);
            let length = to.iter().map(|v| v * v).sum::<f32>().sqrt();
            for i in 0..3 {
                assert!(
                    (forward[i] - to[i] / length).abs() < 1e-5,
                    "{target:?}"
                );
            }
        }
        let before = scene.object("Turret").rotation();
        scene.object("Turret").look_at([1.0, 1.0, 1.0]);
        assert_eq!(scene.object("Turret").rotation(), before);
    }

    #[test]
    fn spawn_numbered_reuses_free_numbers_and_counts_refusals() {
        let mut world = World::new();
        world.spawn((Name("Bullet".into()), Transform::new([0.0; 3])));
        let mut scene = GameScene { world: &mut world };
        assert_eq!(
            scene
                .spawn_numbered("Bullet", [1.0, 0.0, 0.0], 2)
                .as_deref(),
            Some("Bullet 1")
        );
        assert_eq!(
            scene
                .spawn_numbered("Bullet", [2.0, 0.0, 0.0], 2)
                .as_deref(),
            Some("Bullet 2")
        );
        assert_eq!(scene.spawn_numbered("Bullet", [0.0; 3], 2), None);
        assert_eq!(scene.spawn_numbered("Bullet", [0.0; 3], 2), None);
        assert_eq!(scene.counter_value("Bullet refused"), 2);
        scene.despawn("Bullet 1");
        assert_eq!(
            scene
                .spawn_numbered("Bullet", [3.0, 0.0, 0.0], 2)
                .as_deref(),
            Some("Bullet 1")
        );
        assert_eq!(scene.object("Bullet 1").position(), [3.0, 0.0, 0.0]);
        assert_eq!(scene.spawn_numbered("Missing", [0.0; 3], 2), None);
    }

    #[test]
    fn set_state_records_the_entry_tick_and_reports_changes() {
        let mut world = World::new();
        world.insert_resource(FrameTime {
            fixed_tick: 10,
            ..FrameTime::default()
        });
        world.spawn((Name("Enemy".into()), Transform::new([0.0; 3])));
        let mut scene = GameScene { world: &mut world };
        assert_eq!(scene.state("Enemy"), "");
        assert!(scene.set_state("Enemy", "patrol"));
        assert!(!scene.set_state("Enemy", "patrol"));
        assert!(!scene.set_state("Missing", "patrol"));
        world.resource_mut::<FrameTime>().fixed_tick = 40;
        let mut scene = GameScene { world: &mut world };
        assert_eq!(scene.state("Enemy"), "patrol");
        assert!((scene.state_seconds("Enemy") - 0.5).abs() < 1e-6);
        assert!(!scene.set_state("Enemy", "patrol"));
        assert!(scene.set_state("Enemy", "chase"));
        assert_eq!(scene.state_seconds("Enemy"), 0.0);
        assert!(crate::runtime::SceneComponentRegistry::default()
            .names()
            .any(|name| name == crate::runtime::OBJECT_STATE_COMPONENT));
    }

    #[test]
    fn set_state_runs_the_enter_and_exit_actions() {
        use crate::runtime::{ObjectState, StateAction};
        let mut world = World::new();
        world.insert_resource(FrameTime::default());
        let action = |state: &str, exit, counter: &str, add| StateAction {
            state: state.into(),
            exit,
            counter: counter.into(),
            add,
        };
        world.spawn((
            Name("Enemy".into()),
            ObjectState {
                state: "patrol".into(),
                actions: vec![
                    action("chase", false, "chasers", 1),
                    action("chase", true, "chasers", -1),
                    action("patrol", true, "", 5),
                ],
                ..ObjectState::default()
            },
        ));
        let mut scene = GameScene { world: &mut world };
        assert!(scene.set_state("Enemy", "chase"));
        assert_eq!(scene.counter_value("chasers"), 1);
        assert!(!scene.set_state("Enemy", "chase"));
        assert_eq!(scene.counter_value("chasers"), 1);
        assert!(scene.set_state("Enemy", "patrol"));
        assert_eq!(scene.counter_value("chasers"), 0);
    }

    #[test]
    fn dialogues_step_through_lines_and_choices() {
        use crate::runtime::{Dialogue, DialogueChoice, DialogueLine};
        let line = |id: &str, text: &str, next: &str, choices| DialogueLine {
            id: id.into(),
            speaker: "smith".into(),
            text: text.into(),
            next: next.into(),
            choices,
        };
        let choice = |next: &str, counter: &str| DialogueChoice {
            text: format!("pick {next}"),
            next: next.into(),
            counter: counter.into(),
            add: 2,
        };
        let mut world = World::new();
        world.insert_resource(crate::runtime::Translations {
            locale: "de".into(),
            strings: [("smith", "Schmied"), ("hello", "Hallo")]
                .map(|(k, v)| (k.to_owned(), v.to_owned()))
                .into(),
        });
        world.spawn((
            Name("Smith".into()),
            Dialogue {
                lines: vec![
                    line("hi", "hello", "ask", vec![]),
                    line(
                        "ask",
                        "need",
                        "",
                        vec![choice("bye", "quest"), choice("", "")],
                    ),
                    line("bye", "farewell", "gone", vec![]),
                ],
                current: String::new(),
            },
        ));
        world.spawn(Name("Rock".into()));
        let mut scene = GameScene { world: &mut world };
        assert_eq!(scene.dialogue_line("Smith"), None, "not started");
        assert!(scene.start_dialogue("Smith"));
        let shown = scene.dialogue_line("Smith").unwrap();
        assert_eq!(
            (shown.speaker.as_str(), shown.text.as_str()),
            ("Schmied", "Hallo")
        );
        assert!(scene.advance_dialogue("Smith", 0));
        assert_eq!(
            scene.dialogue_line("Smith").unwrap().choices[0].text,
            "pick bye"
        );
        assert!(scene.advance_dialogue("Smith", 5), "a missing choice stays");
        assert_eq!(scene.dialogue_line("Smith").unwrap().id, "ask");
        assert!(scene.advance_dialogue("Smith", 0));
        assert_eq!(scene.counter_value("quest"), 2);
        assert!(!scene.advance_dialogue("Smith", 0), "unknown next ends");
        assert_eq!(scene.dialogue_line("Smith"), None);
        assert!(scene.start_dialogue("Smith"));
        assert!(scene.advance_dialogue("Smith", 0));
        assert!(!scene.advance_dialogue("Smith", 1), "empty next ends");
        assert_eq!(
            scene.counter_value("quest"),
            2,
            "an empty counter adds nothing"
        );
        assert!(!scene.start_dialogue("Rock"));
        assert!(!scene.advance_dialogue("Missing", 0));
    }

    #[test]
    fn set_locale_switches_translations_and_keeps_them_on_a_bad_file() {
        let folder = std::env::temp_dir()
            .join(format!("rusting-locale-{}", uuid::Uuid::new_v4()));
        let locales = folder.join("assets/locales");
        std::fs::create_dir_all(&locales).unwrap();
        std::fs::write(
            locales.join("de.json"),
            r#"{"menu.play": "Spielen", "lives.one": "{count} Leben", "lives.other": "{count} Leben!"}"#,
        )
        .unwrap();
        std::fs::write(locales.join("bad.json"), "[1]").unwrap();
        let mut world = World::new();
        world.insert_resource(ProjectFolder(folder.clone()));
        let mut scene = GameScene { world: &mut world };
        assert_eq!(scene.tr("menu.play"), "menu.play");
        assert!(scene.set_locale("de"));
        assert_eq!(scene.tr("menu.play"), "Spielen");
        assert_eq!(scene.tr_count("lives", 1), "1 Leben");
        assert_eq!(scene.tr_count("lives", 3), "3 Leben!");
        assert!(!scene.set_locale("bad"));
        assert!(!scene.set_locale("missing"));
        assert_eq!(scene.locale(), "de");
        assert_eq!(scene.tr("menu.play"), "Spielen");
        let _ = std::fs::remove_dir_all(&folder);
    }

    #[test]
    fn saved_keys_list_the_files_of_a_save_folder_in_order() {
        let folder = std::env::temp_dir()
            .join(format!("rusting-saves-{}", uuid::Uuid::new_v4()));
        assert!(keys_in(&folder, "saves").is_empty());
        std::fs::create_dir_all(folder.join("old")).unwrap();
        for name in ["2.json", "10.json", "1.json"] {
            std::fs::write(folder.join(name), "{}").unwrap();
        }
        assert_eq!(
            keys_in(&folder, "saves"),
            ["saves/1.json", "saves/10.json", "saves/2.json"]
        );
        let world = &mut World::new();
        assert!(GameScene { world }.saved_keys("../x").is_empty());
        let _ = std::fs::remove_dir_all(&folder);
    }

    #[test]
    fn counters_round_trip_through_a_versioned_save() {
        let mut world = World::new();
        let mut scene = GameScene { world: &mut world };
        scene.set_counter("money", 1250);
        scene.set_counter("night", 3);
        scene.set_counter("not_saved", 9);
        let save = scene.counter_save(2, &["money", "night", "unborn"]);
        let mut world = World::new();
        let mut scene = GameScene { world: &mut world };
        assert_eq!(scene.apply_counter_save(&save), Some(2));
        assert_eq!(scene.counter_value("money"), 1250);
        assert_eq!(scene.counter_value("night"), 3);
        assert_eq!(scene.counter_value("unborn"), 0);
        assert_eq!(scene.counter_or("not_saved", -1), -1);
        let bad = r#"{"version": 1, "counters": {"a": 1, "b": "x"}}"#;
        assert_eq!(scene.apply_counter_save(bad), None);
        assert_eq!(scene.counter_or("a", -1), -1);
        assert_eq!(scene.apply_counter_save("night=3"), None);
    }

    #[test]
    fn every_registered_component_round_trips_through_an_object_save() {
        let world_with = |names: &[&str]| {
            let mut world = World::new();
            world.insert_resource(
                crate::runtime::SceneComponentRegistry::default(),
            );
            for name in names {
                world.spawn(Name((*name).into()));
            }
            world
        };
        let mut world = world_with(&["Hero"]);
        let components = crate::runtime::registered_component_names(&world);
        let hero = find_named_entity(&mut world, "Hero").unwrap();
        for name in &components {
            crate::runtime::add_registered_component(&mut world, hero, name)
                .unwrap();
        }
        let mut scene = GameScene { world: &mut world };
        scene.damage("Hero", 1);
        scene.set_state("Hero", "chase");
        let names: Vec<&str> = components.iter().map(String::as_str).collect();
        let before =
            crate::runtime::registered_component_values(&world, hero).unwrap();
        let save = object_save(&mut world, 4, &["Hero", "Ghost"], &names);
        assert!(!save.contains("Ghost"));
        let mut loaded = world_with(&["Hero"]);
        assert_eq!(apply_object_save(&mut loaded, &save), Some(4));
        let hero = find_named_entity(&mut loaded, "Hero").unwrap();
        let after =
            crate::runtime::registered_component_values(&loaded, hero).unwrap();
        for (name, value) in &before {
            let restored = after.iter().find(|(other, _)| other == name);
            assert_eq!(restored.map(|(_, v)| v), Some(value), "{name}");
        }
        assert!(before.iter().any(|(_, v)| v.contains("chase")));
        assert_eq!(apply_object_save(&mut loaded, r#"{"version": 1}"#), None);
    }

    #[test]
    fn statuses_last_their_duration_and_reapplying_never_shortens() {
        let mut world = World::new();
        world.insert_resource(FrameTime::default());
        let mut scene = GameScene { world: &mut world };
        scene.add_status("Orc", "poisoned", 1.0);
        scene.add_status("Orc", "poisoned", 0.5);
        assert!((scene.status_left("Orc", "poisoned") - 1.0).abs() < 1e-3);
        assert!(!scene.has_status("Goblin", "poisoned"));
        let ticks = (1.0 / FrameTime::default().fixed_delta.as_secs_f64())
            .ceil() as u64;
        world.resource_mut::<FrameTime>().fixed_tick = ticks - 1;
        let mut scene = GameScene { world: &mut world };
        assert!(scene.has_status("Orc", "poisoned"));
        scene.add_status("Orc", "stunned", 2.0);
        scene.clear_status("Orc", "stunned");
        assert!(!scene.has_status("Orc", "stunned"));
        world.resource_mut::<FrameTime>().fixed_tick = ticks;
        let scene = GameScene { world: &mut world };
        assert!(!scene.has_status("Orc", "poisoned"));
    }

    #[test]
    fn loot_rolls_follow_weights_and_spend_needs_enough() {
        let table = [("coin", 3), ("never", 0), ("gem", 1)];
        assert_eq!(weighted_pick(&table, 0.0), Some("coin"));
        assert_eq!(weighted_pick(&table, 0.74), Some("coin"));
        assert_eq!(weighted_pick(&table, 0.75), Some("gem"));
        assert_eq!(weighted_pick(&table, 0.999_999), Some("gem"));
        assert_eq!(weighted_pick(&[("never", 0)], 0.5), None);
        assert_eq!(weighted_pick(&[], 0.5), None);
        let mut world = World::new();
        let scene = GameScene { world: &mut world };
        let rolls: Vec<_> =
            (0..64).map(|i| scene.roll_loot(&table, i)).collect();
        assert!(
            rolls.contains(&Some("coin".into()))
                && rolls.contains(&Some("gem".into()))
        );
        assert!(!rolls.contains(&Some("never".into())));
        assert_eq!(scene.roll_loot(&table, 7), rolls[7]);
        let entry = |item: &str, weight| crate::runtime::LootEntry {
            item: item.into(),
            weight,
        };
        scene.world.spawn((
            Name("Chest".into()),
            crate::runtime::LootTable {
                entries: vec![
                    entry("coin", 3),
                    entry("never", 0),
                    entry("gem", 1),
                ],
            },
        ));
        scene.world.spawn(Name("Rock".into()));
        let mut scene = GameScene { world: &mut world };
        for i in 0..64 {
            assert_eq!(scene.roll_loot_table("Chest", i), rolls[i as usize]);
        }
        assert_eq!(scene.roll_loot_table("Rock", 0), None);
        assert_eq!(scene.roll_loot_table("Missing", 0), None);
        let mut scene = GameScene { world: &mut world };
        scene.set_counter("arrows", 2);
        assert!(!scene.spend("arrows", 3));
        assert!(scene.spend("arrows", 2));
        assert_eq!(scene.counter_value("arrows"), 0);
    }

    #[test]
    fn damage_sends_a_damaged_signal_to_connected_handlers() {
        use crate::runtime::{Damaged, Health, Signal};
        use bevy_ecs::prelude::ResMut;
        use bevy_ecs::system::In;
        #[derive(Resource, Default)]
        struct Hits(Vec<(i32, i32)>);
        let mut app = App::new();
        app.insert_resource(Hits::default());
        app.add_signal_handler(
            "hurt",
            |In(signal): In<Signal<Damaged>>, mut hits: ResMut<Hits>| {
                hits.0.push((signal.event.amount, signal.event.health));
            },
        )
        .unwrap();
        let orc = app.spawn((Name("Orc".into()), Health::default()));
        app.spawn((Name("Rock".into()), Health::default()));
        app.connect(orc, "hurt", orc).unwrap();
        let mut scene = GameScene {
            world: app.world_mut(),
        };
        scene.damage("Orc", 2);
        scene.damage("Rock", 1);
        scene.damage("Orc", -5);
        app.world_mut().flush();
        assert_eq!(app.world().resource::<Hits>().0, [(2, 1), (-5, 3)]);
    }

    #[test]
    fn damage_clamps_heals_to_max_and_teams_mark_allies() {
        use crate::runtime::Health;
        let mut world = World::new();
        let team = |team: &str| Health {
            value: 3,
            max: 3,
            team: team.into(),
        };
        world.spawn((Name("Orc".into()), team("enemies")));
        world.spawn((Name("Goblin".into()), team("enemies")));
        world.spawn((Name("Hero".into()), team("")));
        world.spawn(Name("Rock".into()));
        let mut scene = GameScene { world: &mut world };
        assert_eq!(scene.damage("Orc", 2), Some(1));
        assert_eq!(scene.damage("Orc", -5), Some(3));
        assert_eq!(scene.damage("Orc", 10), Some(0));
        assert_eq!(scene.health("Orc"), Some(0));
        assert_eq!(scene.damage("Rock", 1), None);
        assert_eq!(scene.damage("Missing", 1), None);
        assert!(scene.same_team("Orc", "Goblin"));
        assert!(!scene.same_team("Orc", "Hero"));
        assert!(!scene.same_team("Hero", "Hero"));
        assert!(!scene.same_team("Rock", "Rock"));
    }

    #[test]
    fn cooldowns_count_fixed_ticks() {
        let mut world = World::new();
        world.insert_resource(FrameTime {
            fixed_tick: 100,
            ..FrameTime::default()
        });
        let mut scene = GameScene { world: &mut world };
        assert!(scene.cooldown_ready("dash"));
        scene.start_cooldown("dash", 0.5);
        assert!(!scene.cooldown_ready("dash"));
        assert!((scene.cooldown_left("dash") - 0.5).abs() < 1e-6);
        world.resource_mut::<FrameTime>().fixed_tick = 129;
        let scene = GameScene { world: &mut world };
        assert!(!scene.cooldown_ready("dash"));
        world.resource_mut::<FrameTime>().fixed_tick = 130;
        let scene = GameScene { world: &mut world };
        assert!(scene.cooldown_ready("dash"));
        assert_eq!(scene.counter_value("dash"), 130);
        world.insert_resource(crate::runtime::TimeControl::default());
        let mut scene = GameScene { world: &mut world };
        scene.set_time_scale(-2.0);
        assert_eq!(scene.time_scale(), 0.0);
    }

    #[test]
    fn tween_starts_from_the_current_value_and_restarts_on_a_new_call() {
        use crate::runtime::{Easing, Tween, TweenProperty, TweenRepeat};
        let mut world = World::new();
        let door = world
            .spawn((Name("Door".into()), Transform::new([1.0, 0.0, 0.0])))
            .id();
        let mut scene = GameScene { world: &mut world };
        assert!(scene.tween(
            "Door",
            TweenProperty::Position,
            [1.0, 3.0, 0.0],
            0.5,
            Easing::QuadOut,
        ));
        assert!(!scene.tween(
            "Missing",
            TweenProperty::Scale,
            [2.0; 3],
            1.0,
            Easing::Linear,
        ));
        let tween = *world.get::<Tween>(door).unwrap();
        assert_eq!(tween.from, [1.0, 0.0, 0.0]);
        assert_eq!(tween.repeat, TweenRepeat::Once);
        let mut half = tween;
        half.elapsed = 0.25;
        assert!(half.sample()[1] > 1.5);
        world.get_mut::<Transform>(door).unwrap().position = [1.0, 2.0, 0.0];
        world.get_mut::<Tween>(door).unwrap().elapsed = 0.3;
        let mut scene = GameScene { world: &mut world };
        scene.tween(
            "Door",
            TweenProperty::Position,
            [1.0, 0.0, 0.0],
            0.5,
            Easing::Linear,
        );
        let tween = world.get::<Tween>(door).unwrap();
        assert_eq!((tween.from, tween.elapsed), ([1.0, 2.0, 0.0], 0.0));
    }

    #[test]
    fn spawn_copy_clones_the_template_tree_and_in_class_lists_it() {
        let mut app = App::new();
        let world = app.world_mut();
        let arena = world
            .spawn((Name("Arena".into()), Transform::default()))
            .id();
        let template = world
            .spawn((
                crate::runtime::SceneId::new(),
                Name("Ember".into()),
                Transform::new([0.0, -50.0, 0.0]),
                crate::runtime::ObjectClasses {
                    names: vec!["ember".into()],
                },
                crate::runtime::PhysicsBody::default(),
                crate::runtime::Collider::default(),
                crate::runtime::Visibility { visible: false },
            ))
            .id();
        let glow = world
            .spawn((Name("Glow".into()), Transform::new([0.0, 1.0, 0.0])))
            .id();
        let spark = world.spawn(Name("Spark".into())).id();
        rusting_core::hierarchy::set_parent(world, spark, glow).unwrap();
        rusting_core::hierarchy::set_parent(world, glow, template).unwrap();
        rusting_core::hierarchy::set_parent(world, template, arena).unwrap();
        let mut scene = GameScene {
            world: app.world_mut(),
        };
        let copy = scene
            .spawn_copy("Ember", "Ember 1", [3.0, 0.5, 0.0])
            .unwrap();
        // Animation tracks and skin joints authored on the template (child
        // paths, `..` steps) find the copy's own children.
        let world = &*scene.world;
        let copy_spark =
            crate::runtime::find_target(world, copy, "Glow/Spark").unwrap();
        assert_eq!(
            world.get::<Name>(copy_spark).unwrap().0,
            "Ember 1/Glow/Spark"
        );
        assert_eq!(
            crate::runtime::find_target(world, copy_spark, "../../Glow"),
            world.get::<crate::runtime::Parent>(copy_spark).map(|p| p.0)
        );
        assert_eq!(
            crate::runtime::find_target(world, template, "Glow/Spark"),
            Some(spark)
        );
        scene.set_visible("Ember 1", true);
        assert!(scene.spawn_copy("Missing", "Ember 2", [0.0; 3]).is_none());
        assert_eq!(scene.in_class("ember"), ["Ember", "Ember 1"]);
        assert_eq!(scene.name_of(copy).as_deref(), Some("Ember 1"));
        assert!(scene.has_class(copy, "ember"));
        assert!(!scene.has_class(glow, "ember"));
        assert!(scene.remove_class("Ember 1", "ember"));
        assert!(!scene.remove_class("Ember 1", "ember"));
        assert!(!scene.remove_class("Glow", "ember"));
        assert_eq!(scene.in_class("ember"), ["Ember"]);
        assert!(scene.add_class("Ember 1", "ember"));
        assert!(scene.binding("jump").is_empty());
        scene.rebind("jump", &["Space", "PadSouth"]).unwrap();
        assert_eq!(scene.binding("jump"), ["Space", "PadSouth"]);
        scene.rebind_device("jump", &["KeyJ"]).unwrap();
        assert_eq!(scene.binding("jump"), ["KeyJ", "PadSouth"]);
        scene.rebind_device("jump", &["PadEast"]).unwrap();
        assert_eq!(scene.binding("jump"), ["KeyJ", "PadEast"]);
        assert!(scene.rebind_device("jump", &["PadNope"]).is_err());
        assert_eq!(scene.binding("jump"), ["KeyJ", "PadEast"]);
        assert_eq!(scene.object("Ember 1").position(), [3.0, 0.5, 0.0]);
        assert_eq!(scene.object("Ember").position(), [0.0, -50.0, 0.0]);
        let world = app.world();
        assert!(world.get::<crate::runtime::Collider>(copy).is_some());
        assert!(
            world
                .get::<crate::runtime::Visibility>(copy)
                .unwrap()
                .visible
        );
        assert!(
            !world
                .get::<crate::runtime::Visibility>(template)
                .unwrap()
                .visible
        );
        assert_ne!(
            world.get::<crate::runtime::SceneId>(copy),
            world.get::<crate::runtime::SceneId>(template)
        );
        assert_eq!(world.get::<crate::runtime::Parent>(copy).unwrap().0, arena);
        let turned = Transform {
            rotation: [0.0, 1.5, 0.0],
            ..Transform::new([8.0, 0.0, 2.0])
        };
        let mut scene = GameScene {
            world: app.world_mut(),
        };
        let root_copy = scene
            .spawn_copy_at_root("Ember", "Ember 3", turned)
            .unwrap();
        let world = app.world();
        assert!(world.get::<crate::runtime::Parent>(root_copy).is_none());
        assert_eq!(world.get::<Transform>(root_copy), Some(&turned));
        assert_eq!(
            world
                .get::<crate::runtime::Children>(arena)
                .unwrap()
                .0
                .len(),
            2
        );
        let children = &world.get::<crate::runtime::Children>(copy).unwrap().0;
        assert_eq!(children.len(), 1);
        assert_ne!(children[0], glow);
        assert_eq!(world.get::<Name>(children[0]).unwrap().0, "Ember 1/Glow");
        assert_eq!(
            world.get::<crate::runtime::Parent>(children[0]).unwrap().0,
            copy
        );
        assert_eq!(
            world.get::<crate::runtime::Children>(template).unwrap().0,
            [glow]
        );
        let child = children[0];
        let step = world.resource::<FrameTime>().fixed_delta;
        app.update(step).unwrap();
        let global = app
            .world()
            .get::<crate::runtime::GlobalTransform>(child)
            .expect("the copied child gets a world transform");
        assert_eq!(global.matrix[3][..3], [3.0, 1.5, 0.0]);
    }

    #[test]
    fn scene_input_actions_bind_and_report_presses() {
        let mut app = App::new();
        app.world_mut().spawn(crate::runtime::InputAction {
            action: "fire".into(),
            inputs: vec!["KeyF".into(), "MouseLeft".into()],
        });
        app.update(std::time::Duration::from_millis(16)).unwrap();
        app.world_mut()
            .resource_mut::<RuntimeInput>()
            .record_mouse_button(crate::runtime::MouseButton::Left, true);
        let scene = GameScene {
            world: app.world_mut(),
        };
        assert!(scene.pressed("fire") && scene.held("fire"));
        assert!(!scene.pressed("jump"));
        let bad = serde_json::from_str::<crate::runtime::InputAction>(
            r#"{"action": "fire", "inputs": ["KeyQQ"]}"#,
        );
        assert!(bad
            .unwrap_err()
            .to_string()
            .contains("unknown input `KeyQQ`"));
    }

    #[test]
    fn optional_object_lookup_handles_missing_names() {
        let mut world = World::new();
        let mut scene = GameScene { world: &mut world };
        assert!(scene.try_object("Missing").is_none());
    }

    #[test]
    fn concise_gpu_watch_registration_is_idempotent() {
        let mut app = App::new();
        app.add_plugin(HybridPhysicsPlugin).unwrap();
        let entity = app.spawn((
            Name("Cube".into()),
            Transform::default(),
            crate::runtime::PhysicsBody {
                simulation: crate::runtime::SimulationClass::Gpu,
                ..Default::default()
            },
        ));
        let rule = GpuPhysicsRule::new(
            "cube_fell",
            crate::runtime::GpuCondition::position_y().less_than(-100.0),
        );

        let mut scene = GameScene {
            world: app.world_mut(),
        };
        scene.watch_gpu_object("Cube", rule.clone());
        scene.watch_gpu_object("Cube", rule);

        assert_eq!(
            scene
                .world
                .get::<GpuPhysicsWatch>(entity)
                .unwrap()
                .rules
                .len(),
            1
        );
    }

    #[test]
    fn class_gpu_watch_registration_is_idempotent() {
        let mut app = App::new();
        app.add_plugin(HybridPhysicsPlugin).unwrap();
        let rule = GpuPhysicsRule::new(
            "body_fell",
            crate::runtime::GpuCondition::position_y().less_than(-100.0),
        );

        let mut scene = GameScene {
            world: app.world_mut(),
        };
        scene.watch_gpu_class("falling_cubes", rule.clone());
        scene.watch_gpu_class("falling_cubes", rule);

        assert_eq!(
            scene.world.resource::<GpuPhysicsClassWatches>().classes
                ["falling_cubes"]
                .len(),
            1
        );
    }

    #[test]
    fn concise_api_spawns_ten_thousand_gpu_cubes_only_once() {
        const BODY_COUNT: usize = 10_000;

        let mut app = App::new();
        app.add_plugin(AssetPlugin).unwrap();
        app.add_plugin(HybridPhysicsPlugin).unwrap();
        let mut scene = GameScene {
            world: app.world_mut(),
        };
        scene.once("spawn_test_cubes", |scene| {
            let cube = CubeSpawn::new().class("gravity").class("falling_cubes");
            for index in 0..BODY_COUNT {
                scene.spawn_cube(
                    format!("Physics Cube {index}"),
                    Transform::new([index as f32, 0.0, 0.0]),
                    &cube,
                );
            }
            assert_eq!(
                scene.apply_gpu_physics_to_class(
                    "gravity",
                    &GpuBodySettings::default(),
                ),
                BODY_COUNT
            );
        });
        scene.once("spawn_test_cubes", |_| {
            panic!("a completed once block ran twice")
        });

        let mut query = scene.world.query::<(
            &crate::runtime::ObjectClasses,
            &crate::runtime::PhysicsBody,
        )>();
        assert_eq!(query.iter(scene.world).count(), BODY_COUNT);
        assert!(query.iter(scene.world).all(|(classes, physics)| {
            classes.contains("falling_cubes") && physics.uses_gpu()
        }));
    }

    #[test]
    fn sphere_spawning_reuses_mesh_for_matching_subdivisions() {
        let mut app = App::new();
        app.add_plugin(AssetPlugin).unwrap();
        let mut scene = GameScene {
            world: app.world_mut(),
        };
        let sphere = SphereSpawn::new().subdivisions(8).class("gravity");
        let first = scene.spawn_sphere(
            "Sphere A",
            Transform::new([0.0, 1.0, 0.0]),
            &sphere,
        );
        let second = scene.spawn_sphere(
            "Sphere B",
            Transform::new([0.0, 2.0, 0.0]),
            &sphere,
        );
        let first_mesh = scene
            .world
            .get::<crate::runtime::MeshRenderer>(first)
            .unwrap()
            .mesh;
        let second_mesh = scene
            .world
            .get::<crate::runtime::MeshRenderer>(second)
            .unwrap()
            .mesh;
        assert_eq!(first_mesh, second_mesh);
        let mesh = scene
            .world
            .resource::<AssetServer>()
            .meshes
            .get(first_mesh)
            .unwrap();
        assert!(!mesh.vertices.is_empty());
        assert!(!mesh.indices.is_empty());
        assert!(scene
            .world
            .get::<crate::runtime::ObjectClasses>(first)
            .unwrap()
            .contains("gravity"));
    }

    /// Unclaimed F8: 20,000 static meshes with a child each must not cost
    /// the tick much (about 0.6 ms in release on the dev machine).
    #[test]
    #[ignore = "timing: run with --release on real hardware"]
    fn static_props_cost_little_per_tick() {
        use crate::assets::PrimitiveShape;
        fn noop(_: &mut GameScene, _: &FrameTime) {}
        let mut app = App::new();
        app.add_plugin(AssetPlugin).unwrap();
        app.add_plugin(HybridPhysicsPlugin).unwrap();
        app.add_plugin(RenderExtractPlugin).unwrap();
        app.add_plugin(SimpleGamePlugin {
            update: noop,
            tick: None,
            components: None,
        })
        .unwrap();
        let (mesh, material) = {
            let assets = app.world().resource::<crate::assets::AssetServer>();
            (
                assets.builtin_primitives[&PrimitiveShape::Cube],
                assets.fallback_material,
            )
        };
        for i in 0..20_000 {
            let world = app.world_mut();
            let parent = world
                .spawn((
                    crate::runtime::SceneId::new(),
                    Name(format!("Item{i}")),
                    Transform::new([i as f32 * 0.1, 0.0, 0.0]),
                    MeshRenderer {
                        mesh,
                        material,
                        cast_shadows: true,
                        receive_shadows: true,
                    },
                ))
                .id();
            let child = world
                .spawn((
                    crate::runtime::SceneId::new(),
                    Name(format!("Tag{i}")),
                    Transform::new([0.0, 1.0, 0.0]),
                ))
                .id();
            rusting_core::hierarchy::set_parent(world, child, parent).unwrap();
        }
        for _ in 0..10 {
            app.update(std::time::Duration::from_millis(16)).unwrap();
        }
        let mut sum = [0.0f64; 4];
        for _ in 0..100 {
            app.update(std::time::Duration::from_millis(16)).unwrap();
            let t = *app.world().resource::<crate::runtime::CpuFrameTimings>();
            for (s, d) in sum.iter_mut().zip([
                t.physics,
                t.update,
                t.post_update,
                t.extraction,
            ]) {
                *s += d.as_secs_f64() * 10.0;
            }
        }
        eprintln!("ms per tick (fixed, update, post_update, extract): {sum:?}");
        assert!(sum.iter().sum::<f64>() < 5.0, "{sum:?}");
    }
}
