//! ECS application runtime and canonical scene components.
//!
//! This module is deliberately independent of Vulkan. Rendering and editor
//! integrations consume the ECS state through the `RenderExtract` schedule.

mod actions;
mod animation;
mod audio;
mod classes;
mod click;
pub mod cloth;
mod components;
mod cpu_physics;
mod determinism;
mod effect_presets;
mod events;
mod flip;
pub mod fluid;
pub mod fluid_surface;
mod game_feel;
pub mod gpu_cloth;
pub(crate) mod hierarchy;
mod hybrid_physics;
mod ik;
mod input;
pub mod journal;
pub mod lease;
mod particles;
mod physics_benchmark;
pub mod picking;
mod player;
mod ragdoll;
mod render_benchmark;
mod render_world;
mod replay;
mod retarget;
mod rope;
mod scene_file;
mod scene_instance;
mod scene_tree;
mod signals;
pub mod sim_math;
mod skinning;
mod snapshot;
pub mod soft_body;
mod state_hash;
#[cfg(test)]
mod tests;
mod time;
mod two_d;
#[cfg(feature = "ui")]
mod ui;
mod vehicle;
pub mod water;
mod waypoints;

pub use actions::{
    bind_input_actions, parse_input, ActionMap, InputAction, InputBinding,
};
pub use animation::*;
pub use audio::{
    active_camera, end_tick, route_sound_events, spatialize, ActiveSound,
    AudioCommand, AudioQueue, BeatClock, BusEffect, Caption, CaptionSettings,
    Falloff, ReverbZone, SlideSound, Sound, SoundId, DEFAULT_VOICE_LIMIT,
    MAX_VOICE_LIMIT, QUEUE_LIMIT,
};
pub use classes::ClassIndex;
pub use cloth::{Cloth, ClothSettings, ClothSheet, ClothVolume};
pub use components::*;
pub(crate) use cpu_physics::{gpu_shape_words, next_spawn_order};
pub use cpu_physics::{
    Articulation, AxisMotion, CharacterMove, CollisionEvent, CombineMode,
    Contact, FellOut, FieldFunction, FieldKind, FieldSample, ForceField,
    ForceFieldFunctions, GpuCollider, GpuForceField, GravityVolume,
    GroundSurface, Heightfield, Joint, JointAxis, JointBroken, JointKind,
    JointMotor, JointSpring, MeshSurfaces, NextSpawnOrder, PhysicsMaterial,
    PhysicsWorld, Polygon2d, RayHit, Sleeping, SpawnOrder, SLEEP_STEPS,
};
pub use determinism::*;
pub use effect_presets::*;
pub use events::EventQueue;
pub use fluid::{
    capture_fluids, restore_fluids, Fluid, FluidBlock, FluidParticle,
    FluidSettings, FluidSolver, FluidSurface, FluidVolume,
};
pub use game_feel::*;
pub use hierarchy::{propagate_transforms, HierarchyDiagnostics};
pub use hybrid_physics::*;
pub use ik::*;
#[cfg(feature = "window")]
pub use input::Gamepads;
pub use input::{KeyCode, MouseButton, PadButton, RuntimeInput, Stick};
pub use particles::*;
pub use physics_benchmark::{
    BenchmarkBody, PhysicsBenchmark, BENCHMARK_TOWER_HEIGHT,
};
pub use player::*;
pub use ragdoll::*;
pub use render_benchmark::*;
pub use render_world::*;
pub use replay::*;
pub use retarget::*;
pub use rope::{Rope, RopeBead, RopeState, MAX_SEGMENTS as MAX_ROPE_SEGMENTS};
pub use rusting_core::app::AppError;
pub use rusting_core::input::ClickEvent;
pub use rusting_core::schedule::{CpuFrameTimings, FrameReport, ScheduleStage};
pub use scene_file::*;
pub use scene_instance::{
    edit_instance, member_id, spawn_prefab, InstanceEdit, InstanceExpanded,
    InstanceMember, Prefab, SceneInstance, INSTANCE_MEMBER_KEY,
    SCENE_INSTANCE_COMPONENT,
};
pub use scene_tree::SceneTree;
pub use signals::{
    Added, Connection, Connections, Removed, Signal, SignalError, SignalEvent,
};
pub use skinning::{
    joint_matrices, morph_mesh, skin_mesh, update_skins, Deformers, Morph,
    Skin, SkinnedMesh,
};
pub use snapshot::{SnapshotError, WorldSnapshot};
pub use soft_body::{
    Anchor, AnchorBody, Obstacle, ObstacleShape, SoftAttachment, SoftBlock,
    SoftBody, SoftBodySettings, SoftBodyVolume,
};
pub use state_hash::*;
pub use time::{FrameTime, RandomSeed, TimeControl};
pub use two_d::*;
#[cfg(feature = "ui")]
pub use ui::{RuntimeUi, UiTheme};
pub use vehicle::{Vehicle, Wheel};
pub use water::{WaterBody, WaterMesh};
pub use waypoints::WaypointGraph;

use std::hash::Hasher;
use std::time::{Duration, Instant};

use bevy_ecs::bundle::Bundle;
use bevy_ecs::entity::Entity;
use bevy_ecs::prelude::{IntoScheduleConfigs, Resource, Schedule, World};
use bevy_ecs::system::ScheduleSystem;

/// Small non-cryptographic hasher for per-frame ECS change fingerprints.
/// Stable handles and float bits do not need the cost of a DOS-resistant map
/// hasher; this value is only used to decide whether cached data needs a check.
pub(super) struct FastHasher(u64);

impl Default for FastHasher {
    fn default() -> Self {
        Self(0xcbf2_9ce4_8422_2325)
    }
}

impl Hasher for FastHasher {
    fn finish(&self) -> u64 {
        self.0
    }

    fn write(&mut self, bytes: &[u8]) {
        for byte in bytes {
            self.0 ^= u64::from(*byte);
            self.0 = self.0.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }

    fn write_u8(&mut self, value: u8) {
        self.write_u64(u64::from(value));
    }

    fn write_u32(&mut self, value: u32) {
        self.write_u64(u64::from(value));
    }

    fn write_u64(&mut self, value: u64) {
        self.0 ^= value;
        self.0 = self.0.wrapping_mul(0x0000_0100_0000_01b3);
    }
}

/// A reusable unit of runtime configuration.
pub trait Plugin: Send + Sync + 'static {
    fn build(&self, app: &mut App) -> Result<(), AppError>;

    fn name(&self) -> &'static str {
        std::any::type_name::<Self>()
    }
}

/// Set by [`App::request_exit`] and `GameScene::quit`: the window runner
/// closes after the frame, a headless run stops, and a scenario ends its
/// run.
#[derive(Resource, Clone, Default)]
pub(crate) struct ExitState {
    pub(crate) requested: bool,
}

/// Owns the canonical ECS world and all engine schedules.
pub struct App {
    world: World,
    startup: Schedule,
    fixed_update: Schedule,
    update: Schedule,
    post_update: Schedule,
    render_extract: Schedule,
    startup_complete: bool,
    event_maintenance: Vec<fn(&mut World)>,
    plugins: Vec<&'static str>,
    replay_recorder: Option<ReplayRecorder>,
    snapshot_types: snapshot::SnapshotTypes,
    /// The last frame hit the fixed-step cap and its steps took longer than
    /// the time they simulate; see [`App::update`].
    overloaded: bool,
    overload_reported: bool,
}

impl Default for App {
    fn default() -> Self {
        let mut world = World::new();
        world.insert_resource(FrameTime::default());
        world.insert_resource(TimeControl::default());
        world.insert_resource(RandomSeed::default());
        world.insert_resource(ExitState::default());
        world.insert_resource(HierarchyDiagnostics::default());
        world.insert_resource(RenderSettings::default());
        world.insert_resource(PhysicsSettings::default());
        world.insert_resource(PhysicsBackendStatus {
            gameplay_available: true,
            ..PhysicsBackendStatus::default()
        });
        world.init_resource::<PhysicsWorld>();
        world.insert_resource(SceneComponentRegistry::default());
        world.init_resource::<CpuFrameTimings>();
        world.init_resource::<HitStop>();
        // Bevy adds this on the first schedule run. Adding it here keeps its
        // entity id the same before and after, which snapshots rely on.
        world.init_resource::<bevy_ecs::schedule::Schedules>();
        classes::install(&mut world);
        world.init_resource::<crate::assets::DataAssetTypes>();
        input::install(&mut world);
        actions::install(&mut world);
        player::bind_default_actions(&mut world.resource_mut::<ActionMap>());
        #[cfg(feature = "ui")]
        world.init_resource::<RuntimeUi>();

        // Systems a game adds to FixedUpdate without an order still run in
        // one order every tick: the single-threaded executor follows the
        // build's topological sort instead of thread timing.
        // ponytail: gives up parallel fixed-step systems; order them
        // explicitly and switch back if a game needs the threads.
        let mut fixed_update = Schedule::default();
        fixed_update
            .set_executor(bevy_ecs::schedule::SingleThreadedExecutor::new());
        let mut post_update = Schedule::default();
        post_update.add_systems(propagate_transforms);

        let mut app = Self {
            world,
            startup: Schedule::default(),
            fixed_update,
            update: Schedule::default(),
            post_update,
            render_extract: Schedule::default(),
            startup_complete: false,
            event_maintenance: Vec::new(),
            plugins: Vec::new(),
            replay_recorder: None,
            snapshot_types: snapshot::SnapshotTypes::default(),
            overloaded: false,
            overload_reported: false,
        };
        snapshot::register_engine_types(&mut app.snapshot_types);
        app.add_event::<ClickEvent>();
        app.add_system(ScheduleStage::Update, click::route_click_events);
        app.add_event::<CollisionEvent>();
        app.add_event::<JointBroken>();
        app.add_event::<FellOut>();
        app.add_event::<AnimationEvent>();
        // One chain: these systems all write `Transform`, and unordered
        // systems would run in whatever order threads finish.
        app.add_systems(
            ScheduleStage::FixedUpdate,
            (
                (
                    vehicle::drive_vehicles,
                    rope::build_ropes,
                    cpu_physics::step_cpu_physics,
                )
                    .chain(),
                player::player_move,
                player::player_face,
                two_d::platformer_move,
                game_feel::trigger_on_contact,
                game_feel::collect_pickups,
                game_feel::fire_sound_cues,
                // Existing particles move before new ones spawn at rest.
                game_feel::update_burst_particles,
                game_feel::fire_bursts,
                (
                    particles::restart_on_contact,
                    particles::add_particle_systems,
                    particles::update_particles,
                )
                    .chain(),
                (
                    game_feel::advance_tweens,
                    game_feel::spring_squash,
                    game_feel::decay_camera_shake,
                    game_feel::fade_flashes,
                    game_feel::run_state_machines,
                )
                    .chain(),
                animation::advance_animations,
                (
                    fluid::spawn_fluid_volumes,
                    soft_body::spawn_soft_bodies,
                    cloth::spawn_cloths,
                )
                    .chain(),
                fluid::couple_fluids,
                (
                    fluid::step_fluids,
                    soft_body::step_soft_bodies,
                    soft_body::sync_soft_skins,
                )
                    .chain(),
                fluid::sync_fluid_visuals,
                fluid::reap_surfaces,
                fluid::sync_fluid_surfaces,
                water::float_in_water,
                water::sync_water,
            )
                .chain(),
        );
        app.add_system(ScheduleStage::Update, player::player_look);
        app.add_system(ScheduleStage::Update, game_feel::trigger_on_gpu_events);
        app.add_system(ScheduleStage::Update, actions::bind_input_actions);
        app.add_event::<SoundEvent>();
        app.insert_resource(AudioQueue::default());
        app.add_event::<HudButtonPressed>();
        #[cfg(feature = "ui")]
        // After game code, so the HUD shows this frame's counters, and
        // after propagation, so followed objects are where they are now.
        app.add_system(
            ScheduleStage::PostUpdate,
            game_feel::draw_hud.after(propagate_transforms),
        );
        app.add_system(ScheduleStage::Update, two_d::build_tile_maps);
        app.add_system(
            ScheduleStage::Update,
            game_feel::apply_scene_background,
        );
        app.add_system(ScheduleStage::Update, game_feel::load_environment_maps);
        app.add_system(ScheduleStage::Update, two_d::platformer_jump);
        app
    }
}

impl App {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn world(&self) -> &World {
        &self.world
    }

    pub fn world_mut(&mut self) -> &mut World {
        &mut self.world
    }

    pub fn insert_resource<R: Resource>(&mut self, resource: R) -> &mut Self {
        self.world.insert_resource(resource);
        self
    }

    pub fn spawn<B: Bundle>(&mut self, bundle: B) -> Entity {
        let mut entity = self.world.spawn(bundle);
        if !entity.contains::<SceneId>() {
            entity.insert(SceneId::new());
        }
        entity.id()
    }

    /// Allows a game plugin to persist one of its compiled Rust components in
    /// scene files. Describe the type with [`reflect!`](crate::reflect!)
    /// first; the description drives saving, the Inspector, and field paths.
    /// Registration does not add scripting or dynamic dispatch to normal ECS
    /// queries.
    pub fn register_scene_component<T>(
        &mut self,
        name: impl Into<String>,
    ) -> Result<&mut Self, SceneIoError>
    where
        T: bevy_ecs::component::Component
            + serde::Serialize
            + serde::de::DeserializeOwned
            + Default
            + crate::reflect::Reflect,
    {
        self.world
            .resource_mut::<SceneComponentRegistry>()
            .register::<T>(name)?;
        Ok(self)
    }

    /// Registers a game's [`DataAsset`](crate::assets::DataAsset) type, so
    /// the editor can create, list and edit its `.rdata` files. Loading
    /// works without registering; this only tells tools about the type.
    pub fn register_data_asset<T: crate::assets::DataAsset>(
        &mut self,
    ) -> &mut Self {
        self.world
            .resource_mut::<crate::assets::DataAssetTypes>()
            .register::<T>();
        self
    }

    /// Records a renamed or removed field of a registered component, so
    /// scenes saved before the change still load. See
    /// [`SceneComponentRegistry::add_migration`].
    pub fn migrate_scene_component(
        &mut self,
        name: &str,
        migration: crate::reflect::FieldMigration,
    ) -> Result<&mut Self, SceneIoError> {
        self.world
            .resource_mut::<SceneComponentRegistry>()
            .add_migration(name, migration)?;
        Ok(self)
    }

    pub fn despawn(&mut self, entity: Entity) -> Result<(), AppError> {
        if self.world.despawn(entity) {
            Ok(())
        } else {
            Err(AppError::MissingEntity(entity))
        }
    }

    pub fn add_systems<M>(
        &mut self,
        stage: ScheduleStage,
        systems: impl IntoScheduleConfigs<ScheduleSystem, M>,
    ) -> &mut Self {
        self.schedule_mut(stage).add_systems(systems);
        self
    }

    pub fn add_system<M>(
        &mut self,
        stage: ScheduleStage,
        system: impl IntoScheduleConfigs<ScheduleSystem, M>,
    ) -> &mut Self {
        self.add_systems(stage, system)
    }

    pub fn add_plugin<P: Plugin>(
        &mut self,
        plugin: P,
    ) -> Result<&mut Self, AppError> {
        let name = plugin.name();
        if self.plugins.contains(&name) {
            return Err(AppError::DuplicatePlugin(name));
        }
        plugin.build(self)?;
        self.plugins.push(name);
        Ok(self)
    }

    /// Names `function` for `rusting.force_field` fields of kind `Custom`
    /// whose `function` is `name`.
    pub fn add_force_field_function(
        &mut self,
        name: &str,
        function: FieldFunction,
    ) -> &mut Self {
        self.world
            .get_resource_or_insert_with(ForceFieldFunctions::default)
            .0
            .insert(name.to_owned(), function);
        self
    }

    /// Runs `system` at the start of every frame, before the fixed steps.
    pub fn add_frame_start(&mut self, system: fn(&mut World)) -> &mut Self {
        self.event_maintenance.push(system);
        self
    }

    pub fn add_event<T: Send + Sync + 'static>(&mut self) -> &mut Self {
        if !self.world.contains_resource::<EventQueue<T>>() {
            self.world.insert_resource(EventQueue::<T>::default());
            self.event_maintenance.push(events::begin_frame::<T>);
        }
        self
    }

    pub fn send_event<T: Send + Sync + 'static>(&mut self, event: T) {
        let mut events = self
            .world
            .get_resource_mut::<EventQueue<T>>()
            .unwrap_or_else(|| {
                panic!(
                    "event `{}` was not registered",
                    std::any::type_name::<T>()
                )
            });
        events.send(event);
    }

    /// Registers a named signal handler: a system whose input,
    /// `In<Signal<E>>`, says which signal it answers. Entities run it through
    /// their [`Connections`]. Register handlers during setup, like systems;
    /// see the `signals` module.
    pub fn add_signal_handler<E: SignalEvent, M>(
        &mut self,
        name: impl Into<String>,
        handler: impl bevy_ecs::system::IntoSystem<
            bevy_ecs::system::In<Signal<E>>,
            (),
            M,
        >,
    ) -> Result<&mut Self, SignalError> {
        signals::SignalHandlers::register(
            &mut self.world,
            name.into(),
            handler,
        )?;
        Ok(self)
    }

    /// Connects handler `handler` on `source`, with `target` as receiver.
    pub fn connect(
        &mut self,
        source: Entity,
        handler: &str,
        target: Entity,
    ) -> Result<&mut Self, SignalError> {
        if !signals::SignalHandlers::contains(&self.world, handler) {
            return Err(SignalError::UnknownHandler(handler.to_owned()));
        }
        if self.world.get_entity(target).is_err() {
            return Err(SignalError::MissingEntity(target));
        }
        let mut source = self
            .world
            .get_entity_mut(source)
            .map_err(|_| SignalError::MissingEntity(source))?;
        let connection = Connection {
            handler: handler.to_owned(),
            target,
        };
        if let Some(mut connections) = source.get_mut::<Connections>() {
            connections.list.push(connection);
        } else {
            source.insert(Connections {
                list: vec![connection],
            });
        }
        Ok(self)
    }

    pub fn request_exit(&mut self) {
        self.world.resource_mut::<ExitState>().requested = true;
    }

    #[must_use]
    pub fn exit_requested(&self) -> bool {
        self.world.resource::<ExitState>().requested
    }

    pub fn set_parent(
        &mut self,
        child: Entity,
        parent: Entity,
    ) -> Result<(), AppError> {
        hierarchy::set_parent(&mut self.world, child, parent)
    }

    pub fn clear_parent(&mut self, child: Entity) -> Result<(), AppError> {
        hierarchy::clear_parent(&mut self.world, child)
    }

    /// Records every following [`App::update`] into a [`Replay`]. The
    /// recorder lives outside the `World`: in bevy 0.19 a resource is an
    /// entity, and inserting one would shift the ids of later entities.
    pub fn start_recording(&mut self) {
        self.replay_recorder = Some(ReplayRecorder::new(&self.world));
    }

    /// Stops recording and returns the replay, or `None` when not recording.
    pub fn finish_recording(&mut self) -> Option<Replay> {
        self.replay_recorder.take().map(ReplayRecorder::finish)
    }

    /// Lets [`App::snapshot`] copy component or resource `T` (in bevy 0.19
    /// a resource is a component on its own entity). Every type in the
    /// world needs this or [`App::ignore_in_snapshots`].
    pub fn register_snapshot_component<T>(&mut self) -> &mut Self
    where
        T: bevy_ecs::component::Component<
                Mutability = bevy_ecs::component::Mutable,
            > + Clone,
    {
        self.snapshot_types.register::<T>();
        self
    }

    /// Leaves `T` out of snapshots: a restore keeps the value the app
    /// already has. For caches, handles to external state, and values fixed
    /// after setup.
    pub fn ignore_in_snapshots<T: 'static>(&mut self) -> &mut Self {
        self.snapshot_types.ignore::<T>();
        self
    }

    /// Copies the whole world between frames; see [`WorldSnapshot`].
    pub fn snapshot(&mut self) -> Result<WorldSnapshot, SnapshotError> {
        let entities =
            snapshot::capture_world(&mut self.world, &self.snapshot_types)?;
        let allocator = snapshot::read_allocator(&mut self.world);
        snapshot::write_allocator(&mut self.world, &allocator);
        Ok(WorldSnapshot {
            tick: self.world.resource::<FrameTime>().fixed_tick,
            startup_complete: self.startup_complete,
            entities,
            allocator,
        })
    }

    /// Entities in an [`ObjectClasses`] class, in ascending entity order.
    /// One hash lookup; see [`ClassIndex`].
    pub fn class_members(
        &self,
        class: &str,
    ) -> impl Iterator<Item = Entity> + '_ {
        self.world.resource::<ClassIndex>().members(class)
    }

    /// Puts the world back to `snapshot`. The app must be built the same
    /// way as the one snapshotted (same plugins and scene) and not be past
    /// the snapshot; a new app from the same setup always works. On error
    /// the app is left half restored; build a new one.
    pub fn restore(
        &mut self,
        snapshot: &WorldSnapshot,
    ) -> Result<(), SnapshotError> {
        let mute = |world: &mut World, muted| {
            if let Some(mut handlers) =
                world.get_resource_mut::<signals::SignalHandlers>()
            {
                handlers.muted = muted;
            }
        };
        mute(&mut self.world, true);
        let restored = snapshot::restore_world(&mut self.world, snapshot);
        mute(&mut self.world, false);
        restored?;
        classes::rebuild(&mut self.world);
        self.startup_complete = snapshot.startup_complete;
        Ok(())
    }

    /// Marks startup systems as already run, for a world restored from a
    /// state where they ran.
    #[cfg_attr(not(feature = "window"), allow(dead_code))]
    pub(crate) fn skip_startup(&mut self) {
        self.startup_complete = true;
    }

    /// Advances every schedule once using a caller-provided real-frame delta.
    ///
    /// When the fixed steps take longer than the time they simulate (a
    /// debug build, too many bodies), catching up would make every frame
    /// slower than the last. The frame after such a frame counts as one
    /// fixed step long, so the game runs in slow motion instead of
    /// freezing, and a warning is printed once.
    pub fn update(
        &mut self,
        real_delta: Duration,
    ) -> Result<FrameReport, AppError> {
        let control = self.world.resource::<TimeControl>();
        // Clamped before recording, so a replay steps the same way. One
        // fixed step of scaled time, whatever `time_scale` is.
        let real_delta = match control.time_scale {
            scale if self.overloaded && scale > 0.0 && scale.is_finite() => {
                // A tiny scale overflows `Duration`; no clamp is needed then.
                Duration::try_from_secs_f64(
                    control.fixed_delta.as_secs_f64() / scale,
                )
                .map_or(real_delta, |step| real_delta.min(step))
            }
            _ => real_delta,
        };
        self.update_exact(real_delta)
    }

    /// [`Self::update`] without the slow-motion clamp. Replay uses it: the
    /// recorded delta is already clamped, and whether this machine is
    /// overloaded must not change how a replay steps.
    pub(crate) fn update_exact(
        &mut self,
        real_delta: Duration,
    ) -> Result<FrameReport, AppError> {
        let fixed_delta = self.world.resource::<TimeControl>().fixed_delta;
        if !self.startup_complete {
            self.startup.run(&mut self.world);
            self.startup_complete = true;
        }

        for maintain in &self.event_maintenance {
            maintain(&mut self.world);
        }

        if let Some(recorder) = &mut self.replay_recorder {
            recorder.frame_start(&self.world, real_delta);
        }
        let fixed_steps = time::advance(&mut self.world, real_delta)?;
        let start = Instant::now();
        // `advance` counts the frame's steps at once; each step sees the
        // ticks completed before it, so tick-indexed values differ per tick.
        let end_tick = self.world.resource::<FrameTime>().fixed_tick;
        let first_tick = end_tick.saturating_sub(u64::from(fixed_steps));
        for step in 0..u64::from(fixed_steps) {
            self.world.resource_mut::<FrameTime>().fixed_tick =
                first_tick + step;
            self.fixed_update.run(&mut self.world);
            hybrid_physics::stamp_gpu_commands(&mut self.world);
            state_hash::record_state_hash(&mut self.world);
        }
        self.world.resource_mut::<FrameTime>().fixed_tick = end_tick;
        if let Some(recorder) = &mut self.replay_recorder {
            recorder.frame_hashes(&self.world, fixed_steps);
        }
        let physics = start.elapsed();
        let cap = self.world.resource::<TimeControl>().max_fixed_steps;
        let slow = physics > fixed_delta.saturating_mul(fixed_steps);
        // Enter slow motion only on a capped frame; leave it once the steps
        // keep up again.
        if fixed_steps > 0 {
            self.overloaded =
                slow && (self.overloaded || fixed_steps >= cap.max(2));
        }
        if self.overloaded && !self.overload_reported {
            self.overload_reported = true;
            eprintln!(
                "warning: {fixed_steps} fixed steps took {physics:.0?} but simulate {:.0?}; the game now runs in slow motion. A debug build of the engine is the usual cause: add `[profile.dev.package.\"*\"] opt-level = 3` to the game's Cargo.toml, or use fewer bodies.",
                fixed_delta.saturating_mul(fixed_steps)
            );
        }
        #[cfg(feature = "ui")]
        if self.world.contains_resource::<RuntimeUi>() {
            self.world.resource_scope(
                |world, mut ui: bevy_ecs::world::Mut<RuntimeUi>| {
                    ui.begin_pass(world.resource::<RuntimeInput>());
                },
            );
        }
        let start = Instant::now();
        self.update.run(&mut self.world);
        let update = start.elapsed();
        let start = Instant::now();
        self.post_update.run(&mut self.world);
        let post_update = start.elapsed();
        #[cfg(feature = "ui")]
        if let Some(mut ui) = self.world.get_resource_mut::<RuntimeUi>() {
            ui.end_pass();
        }
        let start = Instant::now();
        self.render_extract.run(&mut self.world);
        let extraction = start.elapsed();
        if let Some(mut timings) =
            self.world.get_resource_mut::<CpuFrameTimings>()
        {
            timings.physics = physics;
            timings.update = update;
            timings.post_update = post_update;
            timings.extraction = extraction;
        }

        Ok(FrameReport {
            fixed_steps,
            exit_requested: self.exit_requested(),
        })
    }

    /// Runs a simple main-thread loop until [`App::request_exit`] is called.
    pub fn run(mut self) -> Result<(), AppError> {
        let mut previous = Instant::now();
        while !self.exit_requested() {
            let now = Instant::now();
            self.update(now.saturating_duration_since(previous))?;
            previous = now;
            std::thread::yield_now();
        }
        Ok(())
    }

    fn schedule_mut(&mut self, stage: ScheduleStage) -> &mut Schedule {
        match stage {
            ScheduleStage::Startup => &mut self.startup,
            ScheduleStage::FixedUpdate => &mut self.fixed_update,
            ScheduleStage::Update => &mut self.update,
            ScheduleStage::PostUpdate => &mut self.post_update,
            ScheduleStage::RenderExtract => &mut self.render_extract,
        }
    }

    /// Which components and resources each ECS system reads and writes, by
    /// stage, in the order the systems were added. Reading the access
    /// initializes every system again, so this consumes the app.
    #[must_use]
    pub fn into_system_access(mut self) -> Vec<SystemAccess> {
        use bevy_ecs::query::ComponentAccessKind;
        use bevy_ecs::system::System;
        let mut found = Vec::new();
        for stage in [
            ScheduleStage::Startup,
            ScheduleStage::FixedUpdate,
            ScheduleStage::Update,
            ScheduleStage::PostUpdate,
            ScheduleStage::RenderExtract,
        ] {
            let schedule = match stage {
                ScheduleStage::Startup => &mut self.startup,
                ScheduleStage::FixedUpdate => &mut self.fixed_update,
                ScheduleStage::Update => &mut self.update,
                ScheduleStage::PostUpdate => &mut self.post_update,
                ScheduleStage::RenderExtract => &mut self.render_extract,
            };
            let systems = &mut schedule.graph_mut().systems;
            let keys: Vec<_> = systems.iter().map(|(key, ..)| key).collect();
            for key in keys {
                let Some(system) = systems.get_mut(key) else {
                    continue;
                };
                let access = system.initialize(&mut self.world);
                let mut entry = SystemAccess {
                    stage,
                    name: system.name().shortname().to_string(),
                    reads: Vec::new(),
                    writes: Vec::new(),
                    all: system.is_exclusive(),
                };
                match access.combined_access().try_iter_access() {
                    Ok(kinds) => {
                        let components = self.world.components();
                        let name = |id| {
                            components.get_name(id).map_or_else(
                                || format!("{id:?}"),
                                |name| name.shortname().to_string(),
                            )
                        };
                        for kind in kinds {
                            match kind {
                                ComponentAccessKind::Shared(id) => {
                                    entry.reads.push(name(id));
                                }
                                ComponentAccessKind::Exclusive(id) => {
                                    entry.writes.push(name(id));
                                }
                                ComponentAccessKind::Archetypal(_) => {}
                            }
                        }
                    }
                    Err(_) => entry.all = true,
                }
                entry.reads.sort();
                entry.writes.sort();
                found.push(entry);
            }
        }
        found
    }
}

/// One ECS system's data access; see [`App::into_system_access`].
#[derive(Debug, Clone)]
pub struct SystemAccess {
    pub stage: ScheduleStage,
    pub name: String,
    /// Components and resources read but not written.
    pub reads: Vec<String>,
    pub writes: Vec<String>,
    /// The system takes the whole `World` (or unbounded access), so it may
    /// read and write anything.
    pub all: bool,
}

/// Fallible builder for the ECS runtime.
pub struct EngineBuilder {
    fixed_delta: Duration,
    max_fixed_steps: u32,
    plugins: Vec<Box<dyn Plugin>>,
}

impl Default for EngineBuilder {
    fn default() -> Self {
        Self {
            fixed_delta: Duration::from_secs_f64(1.0 / 60.0),
            max_fixed_steps: 8,
            plugins: Vec::new(),
        }
    }
}

impl EngineBuilder {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn fixed_delta(mut self, fixed_delta: Duration) -> Self {
        self.fixed_delta = fixed_delta;
        self
    }

    #[must_use]
    pub fn max_fixed_steps(mut self, max_fixed_steps: u32) -> Self {
        self.max_fixed_steps = max_fixed_steps.max(1);
        self
    }

    #[must_use]
    pub fn plugin<P: Plugin>(mut self, plugin: P) -> Self {
        self.plugins.push(Box::new(plugin));
        self
    }

    pub fn build(self) -> Result<App, AppError> {
        if self.fixed_delta.is_zero() {
            return Err(AppError::InvalidFixedDelta);
        }
        let mut app = App::new();
        {
            let mut control = app.world.resource_mut::<TimeControl>();
            control.fixed_delta = self.fixed_delta;
            control.max_fixed_steps = self.max_fixed_steps;
        }
        for plugin in self.plugins {
            let name = plugin.name();
            if app.plugins.contains(&name) {
                return Err(AppError::DuplicatePlugin(name));
            }
            plugin.build(&mut app)?;
            app.plugins.push(name);
        }
        Ok(app)
    }
}

/// Systems registered once by [`run_edit_mode_systems`], so their change
/// detection carries over from one frame to the next.
#[derive(bevy_ecs::prelude::Resource)]
struct EditModeSystems([bevy_ecs::system::SystemId; 3]);

/// Runs the scene-data systems whose output the editor needs while editing,
/// when no App schedule runs: tile maps build their tiles and
/// `rusting.background` sets the clear color.
pub fn run_edit_mode_systems(world: &mut World) {
    let systems = match world.get_resource::<EditModeSystems>() {
        Some(systems) => systems.0,
        None => {
            let systems = [
                world.register_system(two_d::build_tile_maps),
                world.register_system(game_feel::apply_scene_background),
                world.register_system(game_feel::load_environment_maps),
            ];
            world.insert_resource(EditModeSystems(systems));
            systems
        }
    };
    for system in systems {
        if let Err(error) = world.run_system(system) {
            eprintln!("edit mode system: {error}");
        }
    }
}

/// Despawns `entity` and its children, deepest first.
pub(crate) fn despawn_tree(world: &mut World, entity: Entity) {
    let children = world
        .get::<Children>(entity)
        .map(|children| children.0.clone())
        .unwrap_or_default();
    for child in children {
        despawn_tree(world, child);
    }
    world.despawn(entity);
}
