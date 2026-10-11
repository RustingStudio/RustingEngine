use std::time::Duration;

use bevy_ecs::prelude::{Commands, Query, Res, ResMut, Resource, With};

use crate::Transform;

use super::*;

#[derive(Resource, Default)]
struct Counts {
    startup: u32,
    fixed: u32,
    update: u32,
    extracted: u32,
    events_seen: usize,
}

fn startup(mut counts: ResMut<Counts>) {
    counts.startup += 1;
}

fn fixed(mut counts: ResMut<Counts>) {
    counts.fixed += 1;
}

fn update(mut counts: ResMut<Counts>) {
    counts.update += 1;
}

fn extract(mut counts: ResMut<Counts>) {
    counts.extracted += 1;
}

#[test]
fn schedule_types_are_exact_core_reexports() {
    fn core_stage_to_runtime(
        stage: rusting_core::schedule::ScheduleStage,
    ) -> ScheduleStage {
        stage
    }
    fn runtime_stage_to_core(
        stage: ScheduleStage,
    ) -> rusting_core::schedule::ScheduleStage {
        stage
    }
    fn core_report_to_runtime(
        report: rusting_core::schedule::FrameReport,
    ) -> FrameReport {
        report
    }
    fn runtime_report_to_core(
        report: FrameReport,
    ) -> rusting_core::schedule::FrameReport {
        report
    }

    let _stage = runtime_stage_to_core(core_stage_to_runtime(
        rusting_core::schedule::ScheduleStage::Update,
    ));
    let _report = runtime_report_to_core(core_report_to_runtime(
        rusting_core::schedule::FrameReport::default(),
    ));
}

#[test]
fn time_types_are_exact_core_reexports() {
    fn core_frame_to_runtime(time: rusting_core::time::FrameTime) -> FrameTime {
        time
    }
    fn runtime_frame_to_core(time: FrameTime) -> rusting_core::time::FrameTime {
        time
    }
    fn core_control_to_runtime(
        control: rusting_core::time::TimeControl,
    ) -> TimeControl {
        control
    }
    fn runtime_control_to_core(
        control: TimeControl,
    ) -> rusting_core::time::TimeControl {
        control
    }

    let _time =
        runtime_frame_to_core(core_frame_to_runtime(FrameTime::default()));
    let _control = runtime_control_to_core(core_control_to_runtime(
        TimeControl::default(),
    ));
}

#[test]
fn zero_fixed_delta_after_build_maps_to_app_error_without_advancing_time() {
    let mut app = EngineBuilder::new().build().unwrap();
    app.world_mut().resource_mut::<TimeControl>().fixed_delta = Duration::ZERO;

    assert_eq!(
        app.update(Duration::from_secs(1)),
        Err(AppError::InvalidFixedDelta)
    );
    let time = app.world().resource::<FrameTime>();
    assert_eq!(time.frame, 0);
    assert_eq!(time.real_delta, Duration::ZERO);
    assert_eq!(time.delta, Duration::ZERO);
    assert_eq!(time.elapsed, Duration::ZERO);
    assert_eq!(time.fixed_tick, 0);
}

#[test]
fn schedules_and_fixed_catch_up_are_deterministic() {
    let mut app = EngineBuilder::new()
        .fixed_delta(Duration::from_millis(10))
        .max_fixed_steps(3)
        .build()
        .unwrap();
    app.insert_resource(Counts::default())
        .add_systems(ScheduleStage::Startup, startup)
        .add_systems(ScheduleStage::FixedUpdate, fixed)
        .add_systems(ScheduleStage::Update, update)
        .add_systems(ScheduleStage::RenderExtract, extract);

    let first = app.update(Duration::from_millis(35)).unwrap();
    let second = app.update(Duration::from_millis(5)).unwrap();
    let counts = app.world().resource::<Counts>();

    assert_eq!(first.fixed_steps, 3);
    assert_eq!(second.fixed_steps, 1);
    assert_eq!(counts.startup, 1);
    assert_eq!(counts.fixed, 4);
    assert_eq!(counts.update, 2);
    assert_eq!(counts.extracted, 2);
}

#[test]
fn fixed_steps_slower_than_real_time_fall_back_to_slow_motion() {
    fn sluggish(_: ResMut<Counts>) {
        std::thread::sleep(Duration::from_millis(3));
    }
    let mut app = EngineBuilder::new()
        .fixed_delta(Duration::from_millis(1))
        .max_fixed_steps(4)
        .build()
        .unwrap();
    app.insert_resource(Counts::default())
        .add_systems(ScheduleStage::FixedUpdate, sluggish);
    // Each step takes 3 ms to simulate 1 ms, so catching up never ends.
    let steps = (0..6)
        .map(|_| app.update(Duration::from_millis(50)).unwrap().fixed_steps)
        .collect::<Vec<_>>();
    assert_eq!(steps[0], 4);
    assert_eq!(steps[4..], [1, 1], "one step per frame: {steps:?}");
}

fn slow(_: ResMut<Counts>) {
    std::thread::sleep(Duration::from_millis(2));
}

#[test]
fn update_times_physics_and_extraction() {
    let mut app = EngineBuilder::new()
        .fixed_delta(Duration::from_millis(10))
        .build()
        .unwrap();
    app.insert_resource(Counts::default())
        .add_systems(ScheduleStage::FixedUpdate, slow)
        .add_systems(ScheduleStage::RenderExtract, slow);

    app.update(Duration::from_millis(10)).unwrap();
    let timings = *app.world().resource::<CpuFrameTimings>();
    assert!(timings.physics >= Duration::from_millis(2), "{timings:?}");
    assert!(
        timings.extraction >= Duration::from_millis(2),
        "{timings:?}"
    );

    // A frame without a fixed step spends no time in physics.
    app.update(Duration::ZERO).unwrap();
    let timings = *app.world().resource::<CpuFrameTimings>();
    assert!(timings.physics < Duration::from_millis(2), "{timings:?}");
}

#[test]
fn pause_and_single_step_keep_variable_systems_alive() {
    let mut app = EngineBuilder::new()
        .fixed_delta(Duration::from_millis(10))
        .build()
        .unwrap();
    app.insert_resource(Counts::default())
        .add_systems(ScheduleStage::FixedUpdate, fixed)
        .add_systems(ScheduleStage::Update, update);
    app.world_mut().resource_mut::<TimeControl>().pause();

    assert_eq!(app.update(Duration::from_secs(1)).unwrap().fixed_steps, 0);
    app.world_mut().resource_mut::<TimeControl>().step();
    assert_eq!(app.update(Duration::from_secs(1)).unwrap().fixed_steps, 1);

    let counts = app.world().resource::<Counts>();
    assert_eq!(counts.fixed, 1);
    assert_eq!(counts.update, 2);
    let time = app.world().resource::<FrameTime>();
    assert_eq!(time.delta, Duration::ZERO);
    assert_eq!(time.elapsed, Duration::from_millis(10));
}

#[derive(Clone, Copy)]
struct TestEvent;

fn count_events(
    events: Res<EventQueue<TestEvent>>,
    mut counts: ResMut<Counts>,
) {
    counts.events_seen += events.len();
}

#[test]
fn typed_events_are_visible_for_one_frame() {
    let mut app = App::new();
    app.insert_resource(Counts::default())
        .add_event::<TestEvent>()
        .add_systems(ScheduleStage::Update, count_events);
    app.send_event(TestEvent);

    app.update(Duration::ZERO).unwrap();
    app.update(Duration::ZERO).unwrap();

    assert_eq!(app.world().resource::<Counts>().events_seen, 1);
}

#[test]
fn adding_an_event_type_twice_does_not_duplicate_maintenance() {
    let mut app = App::new();
    app.add_event::<TestEvent>().add_event::<TestEvent>();
    app.send_event(TestEvent);

    app.update(Duration::ZERO).unwrap();
    assert_eq!(app.world().resource::<EventQueue<TestEvent>>().len(), 1);

    app.update(Duration::ZERO).unwrap();
    assert!(app.world().resource::<EventQueue<TestEvent>>().is_empty());
}

#[derive(bevy_ecs::component::Component)]
struct DeferredSpawn;

fn queue_spawn(mut commands: Commands) {
    commands.spawn(DeferredSpawn);
}

fn count_spawned(
    query: Query<(), With<DeferredSpawn>>,
    mut counts: ResMut<Counts>,
) {
    counts.extracted = query.iter().count() as u32;
}

#[test]
fn deferred_commands_are_applied_between_ordered_schedules() {
    let mut app = App::new();
    app.insert_resource(Counts::default())
        .add_systems(ScheduleStage::Update, queue_spawn)
        .add_systems(ScheduleStage::PostUpdate, count_spawned);

    app.update(Duration::ZERO).unwrap();
    assert_eq!(app.world().resource::<Counts>().extracted, 1);
}

#[test]
fn hierarchy_propagates_and_rejects_cycles() {
    let mut app = App::new();
    let root = app.spawn(Transform::new([1.0, 0.0, 0.0]));
    let child = app.spawn(Transform::new([2.0, 0.0, 0.0]));
    let grandchild = app.spawn(Transform::new([4.0, 0.0, 0.0]));
    app.set_parent(child, root).unwrap();
    app.set_parent(grandchild, child).unwrap();

    app.update(Duration::ZERO).unwrap();
    let global = app.world().get::<GlobalTransform>(grandchild).unwrap();
    assert!((global.matrix[3][0] - 7.0).abs() < f32::EPSILON);
    assert!(matches!(
        app.set_parent(root, grandchild),
        Err(AppError::HierarchyCycle { .. })
    ));
}

struct CountingPlugin;

impl Plugin for CountingPlugin {
    fn build(&self, app: &mut App) -> Result<(), AppError> {
        app.insert_resource(Counts::default())
            .add_systems(ScheduleStage::Startup, startup);
        Ok(())
    }
}

#[test]
fn plugins_configure_apps_and_cannot_be_added_twice() {
    let mut app = App::new();
    app.add_plugin(CountingPlugin).unwrap();
    assert!(matches!(
        app.add_plugin(CountingPlugin),
        Err(AppError::DuplicatePlugin(_))
    ));
    app.update(Duration::ZERO).unwrap();
    assert_eq!(app.world().resource::<Counts>().startup, 1);
}

#[test]
fn rendering_is_uncapped_by_default() {
    let settings = RenderSettings::default();
    assert!(!settings.vsync);
    assert!(!settings.limit_fps);
    assert!(settings.max_fps > 0);
}

#[test]
fn the_reflections_setting_reaches_the_render_world() {
    let mut app = App::new();
    app.add_plugin(RenderExtractPlugin).unwrap();
    assert!(RenderSettings::default().reflections);
    app.update(Duration::ZERO).unwrap();
    assert!(!app.world().resource::<RenderWorld>().reflections_disabled);
    app.world_mut().resource_mut::<RenderSettings>().reflections = false;
    app.update(Duration::ZERO).unwrap();
    assert!(app.world().resource::<RenderWorld>().reflections_disabled);
}

#[test]
fn physics_ids_reject_delayed_events_after_slot_reuse() {
    let first = Entity::from_raw_u32(10).unwrap();
    let second = Entity::from_raw_u32(11).unwrap();
    let mut ids = PhysicsIdRegistry::default();

    let old_id = ids.assign(first);
    assert_eq!(ids.resolve(old_id), Some(first));
    ids.release(first);
    let new_id = ids.assign(second);

    assert_eq!(old_id.slot, new_id.slot);
    assert_ne!(old_id.generation, new_id.generation);
    assert_eq!(ids.resolve(old_id), None);
    assert_eq!(ids.resolve(new_id), Some(second));
}

#[test]
fn rust_condition_builder_compiles_to_postfix_gpu_instructions() {
    let condition = GpuCondition::position_y()
        .less_than(-100.0)
        .and(GpuCondition::velocity_y().less_than(0.0))
        .or(!GpuCondition::sleeping());

    let instructions = condition.compile().unwrap();

    // Two comparisons, AND, sleeping, NOT, OR.
    assert_eq!(instructions.len(), 6);
    assert_eq!(instructions[0].values[0], -100.0);
    assert_eq!(instructions[1].values[0], 0.0);
    assert_ne!(instructions[2].opcode, instructions[5].opcode);

    let operators = (GpuCondition::position_y().less_than(-100.0)
        & GpuCondition::velocity_y().less_than(0.0))
        | !GpuCondition::sleeping();
    assert_eq!(
        format!("{:?}", operators.compile().unwrap()),
        format!("{instructions:?}")
    );
}

#[test]
fn one_class_rule_is_prepared_for_ten_thousand_matching_gpu_bodies() {
    const BODY_COUNT: usize = 10_000;

    let mut app = App::new();
    app.add_plugin(HybridPhysicsPlugin).unwrap();
    let rule = GpuPhysicsRule::new(
        "body_fell",
        GpuCondition::position_y().less_than(-100.0),
    );
    {
        let mut watches =
            app.world_mut().resource_mut::<GpuPhysicsClassWatches>();
        watches.add("falling_cubes", rule.clone());
        // The same rule reached through two classes must still emit only once.
        watches.add("gravity", rule);
    }

    for index in 0..BODY_COUNT {
        app.spawn((
            Transform {
                position: [index as f32, 0.0, 0.0],
                ..Transform::default()
            },
            PhysicsBody {
                simulation: SimulationClass::Gpu,
                ..PhysicsBody::default()
            },
            RigidBody::default(),
            ObjectClasses::new(["falling_cubes", "gravity"]),
        ));
    }
    app.spawn((
        Transform::default(),
        PhysicsBody {
            simulation: SimulationClass::Gpu,
            ..PhysicsBody::default()
        },
        RigidBody::default(),
        ObjectClasses::new(["unrelated"]),
    ));

    // PostUpdate assigns one stable PhysicsId to every GPU-owned body.
    app.update(Duration::ZERO).unwrap();
    let extracted =
        super::hybrid_physics::extract_gpu_physics_bodies(app.world_mut());

    assert_eq!(
        app.world().resource::<GpuPhysicsClassWatches>().classes
            ["falling_cubes"]
            .len(),
        1
    );
    assert_eq!(extracted.len(), BODY_COUNT + 1);
    assert_eq!(
        extracted
            .iter()
            .filter(|body| body.rules.len() == 1)
            .count(),
        BODY_COUNT
    );
    assert_eq!(
        extracted
            .iter()
            .filter(|body| body.rules.is_empty())
            .count(),
        1
    );
    assert!(extracted
        .windows(2)
        .all(|bodies| bodies[0].physics_id != bodies[1].physics_id));
}

#[test]
fn watched_gpu_bodies_keep_their_revision_while_unchanged() {
    let mut app = App::new();
    app.add_plugin(HybridPhysicsPlugin).unwrap();
    app.add_plugin(RenderExtractPlugin).unwrap();
    app.world_mut()
        .resource_mut::<GpuPhysicsClassWatches>()
        .add(
            "falling",
            GpuPhysicsRule::new(
                "fell",
                GpuCondition::position_y().less_than(-100.0),
            ),
        );
    app.spawn((
        Transform::default(),
        PhysicsBody {
            simulation: SimulationClass::Gpu,
            ..PhysicsBody::default()
        },
        RigidBody::default(),
        ObjectClasses::new(["falling"]),
    ));
    app.update(Duration::ZERO).unwrap();
    app.update(Duration::ZERO).unwrap();
    let revision = app.world().resource::<RenderWorld>().gpu_physics_revision;
    assert_eq!(app.world().resource::<RenderWorld>().gpu_physics.len(), 1);

    app.update(Duration::ZERO).unwrap();
    app.update(Duration::ZERO).unwrap();

    // Each revision bump restarts the GPU simulation from authored poses.
    assert_eq!(
        app.world().resource::<RenderWorld>().gpu_physics_revision,
        revision
    );
}

#[test]
fn watched_gpu_bodies_skip_extraction_until_a_watch_changes() {
    let mut app = App::new();
    app.add_plugin(HybridPhysicsPlugin).unwrap();
    app.add_plugin(RenderExtractPlugin).unwrap();
    let body = app.spawn((
        Transform::default(),
        PhysicsBody {
            simulation: SimulationClass::Gpu,
            ..PhysicsBody::default()
        },
        RigidBody::default(),
        GpuPhysicsWatch {
            rules: vec![GpuPhysicsRule::new(
                "fell",
                GpuCondition::position_y().less_than(-100.0),
            )],
        },
    ));
    app.update(Duration::ZERO).unwrap();
    let signature =
        super::hybrid_physics::simple_gpu_physics_signature(app.world_mut());
    app.update(Duration::ZERO).unwrap();
    // A stable signature lets extraction skip cloning every watched body.
    assert_eq!(
        super::hybrid_physics::simple_gpu_physics_signature(app.world_mut()),
        signature
    );
    let revision = app.world().resource::<RenderWorld>().gpu_physics_revision;

    app.world_mut()
        .get_mut::<GpuPhysicsWatch>(body)
        .unwrap()
        .rules[0]
        .cooldown_seconds = 1.0;
    app.update(Duration::ZERO).unwrap();

    let render_world = app.world().resource::<RenderWorld>();
    assert_eq!(render_world.gpu_physics_revision, revision.wrapping_add(1));
    assert_eq!(render_world.gpu_physics[0].rules.len(), 1);
}

#[test]
fn gpu_event_and_instruction_layouts_are_stable() {
    use std::mem::{offset_of, size_of};

    assert_eq!(size_of::<PhysicsId>(), 8);
    assert_eq!(size_of::<GpuConditionInstruction>(), 32);
    assert_eq!(offset_of!(GpuConditionInstruction, values), 16);
    assert_eq!(size_of::<RawGpuPhysicsEvent>(), 48);
    assert_eq!(offset_of!(RawGpuPhysicsEvent, tick_low), 16);
    assert_eq!(offset_of!(RawGpuPhysicsEvent, payload), 32);
}

#[test]
fn raw_gpu_events_reach_the_live_ecs_entity() {
    let mut app = App::new();
    app.add_plugin(HybridPhysicsPlugin).unwrap();
    let entity = app.spawn(PhysicsBody {
        simulation: SimulationClass::Gpu,
        ..PhysicsBody::default()
    });
    app.update(Duration::from_secs_f64(1.0 / 60.0)).unwrap();

    let physics_id = *app.world().get::<PhysicsId>(entity).unwrap();
    let event_id = app
        .world_mut()
        .resource_mut::<GpuEventRegistry>()
        .register("cube_fell");
    let tick = u64::from(u32::MAX) + 25;
    let raw = RawGpuPhysicsEvent {
        body_slot: physics_id.slot,
        body_generation: physics_id.generation,
        event_id: event_id.0,
        tick_low: tick as u32,
        tick_high: (tick >> 32) as u32,
        payload_kind: GpuEventPayload::Position as u32,
        payload: [1.0, -101.0, 2.0, 1.0],
        ..Default::default()
    };

    let report = route_gpu_physics_events(app.world_mut(), &[raw]);
    assert_eq!(report.delivered, 1);
    app.update(Duration::ZERO).unwrap();

    let events = app.world().resource::<EventQueue<GpuPhysicsEvent>>();
    let event = events.iter().next().unwrap();
    assert_eq!(event.entity, entity);
    assert_eq!(event.tick, tick);
    assert_eq!(event.payload[1], -101.0);
}

#[test]
fn gpu_events_are_delivered_in_tick_and_body_order() {
    let mut app = App::new();
    app.add_plugin(HybridPhysicsPlugin).unwrap();
    let bodies = [
        app.spawn(PhysicsBody::default()),
        app.spawn(PhysicsBody::default()),
    ]
    .map(|entity| {
        app.world_mut()
            .resource_mut::<PhysicsIdRegistry>()
            .assign(entity)
    });
    let event_id = app
        .world_mut()
        .resource_mut::<GpuEventRegistry>()
        .register("hit");
    let raw = |body: PhysicsId, tick: u32| RawGpuPhysicsEvent {
        body_slot: body.slot,
        body_generation: body.generation,
        event_id: event_id.0,
        tick_low: tick,
        ..Default::default()
    };

    // The GPU appends in atomic order; this is one possible buffer order.
    route_gpu_physics_events(
        app.world_mut(),
        &[raw(bodies[1], 2), raw(bodies[1], 1), raw(bodies[0], 2)],
    );
    app.update(Duration::ZERO).unwrap();

    let order: Vec<_> = app
        .world()
        .resource::<EventQueue<GpuPhysicsEvent>>()
        .iter()
        .map(|event| (event.tick, event.physics_id))
        .collect();
    assert_eq!(order, [(1, bodies[1]), (2, bodies[0]), (2, bodies[1])]);
}

#[test]
fn sync_mode_controls_rules_and_state_mirror() {
    let mut app = App::new();
    app.add_plugin(HybridPhysicsPlugin).unwrap();
    app.world_mut()
        .resource_mut::<GpuPhysicsClassWatches>()
        .add(
            "watched",
            GpuPhysicsRule::new(
                "fell",
                GpuCondition::position_y().less_than(0.0),
            ),
        );
    let spawn = |app: &mut App, sync| {
        app.spawn((
            Transform::default(),
            PhysicsBody {
                simulation: SimulationClass::Gpu,
                ..PhysicsBody::default()
            },
            RigidBody::default(),
            ObjectClasses::new(["watched"]),
            sync,
        ))
    };
    let silent = spawn(&mut app, PhysicsSyncMode::None);
    let mirrored = spawn(&mut app, PhysicsSyncMode::SelectedState);
    app.update(Duration::ZERO).unwrap();

    let extracted =
        super::hybrid_physics::extract_gpu_physics_bodies(app.world_mut());
    let sync_and_rules: Vec<_> = extracted
        .iter()
        .map(|body| (body.sync, body.rules.len()))
        .collect();
    assert_eq!(
        sync_and_rules,
        [
            (PhysicsSyncMode::None, 0),
            (PhysicsSyncMode::SelectedState, 1)
        ]
    );

    let id = app.world().get::<PhysicsId>(mirrored).copied().unwrap();
    let sample = |tick, y| GpuStateSample {
        physics_id: id,
        tick,
        transform: Transform::default().with_position(0.0, y, 0.0),
        linear_velocity: [0.0, -1.0, 0.0],
        angular_velocity: [0.0; 3],
        custom_values: None,
    };
    let removed = PhysicsId {
        generation: id.generation + 1,
        ..id
    };
    let report = apply_gpu_state_samples(
        app.world_mut(),
        &[
            sample(5, -1.0),
            // A late sample from an older frame never rewinds the mirror.
            sample(4, 3.0),
            GpuStateSample {
                physics_id: removed,
                ..sample(6, 0.0)
            },
        ],
    );
    assert_eq!((report.delivered, report.stale), (1, 1));
    let mirror = app.world().get::<GpuStateMirror>(mirrored).unwrap();
    assert_eq!(mirror.transform.position[1], -1.0);
    assert_eq!(mirror.age_ticks(8), 3);
    // The authored transform stays the editor's source of truth.
    assert_eq!(
        app.world().get::<Transform>(mirrored).unwrap().position[1],
        0.0
    );
    assert!(app.world().get::<GpuStateMirror>(silent).is_none());
}

#[test]
fn gpu_commands_reach_the_render_world_once_per_batch() {
    let mut app = App::new();
    app.add_plugin(HybridPhysicsPlugin).unwrap();
    app.add_plugin(RenderExtractPlugin).unwrap();
    let body = PhysicsId {
        slot: 4,
        generation: 2,
    };
    app.world_mut()
        .resource_mut::<GpuPhysicsCommands>()
        .push(body, GpuBodyCommand::Impulse([0.0, 5.0, 0.0]));
    app.update(Duration::ZERO).unwrap();

    let render_world = app.world().resource::<RenderWorld>();
    assert_eq!(
        render_world.gpu_physics_commands,
        [(body, GpuBodyCommand::Impulse([0.0, 5.0, 0.0]))]
    );
    assert_eq!(render_world.gpu_physics_commands_serial, 1);
    assert!(app
        .world()
        .resource::<GpuPhysicsCommands>()
        .commands
        .is_empty());

    // Without new commands the serial stays, so the batch is not re-sent.
    app.update(Duration::ZERO).unwrap();
    assert_eq!(
        app.world()
            .resource::<RenderWorld>()
            .gpu_physics_commands_serial,
        1
    );

    // A reset alone is a batch too.
    app.world_mut()
        .resource_mut::<GpuPhysicsCommands>()
        .reset_to_authored = true;
    app.update(Duration::ZERO).unwrap();
    let render_world = app.world().resource::<RenderWorld>();
    assert!(render_world.gpu_physics_reset);
    assert!(render_world.gpu_physics_commands.is_empty());
    assert_eq!(render_world.gpu_physics_commands_serial, 2);
}

#[test]
fn class_snapshots_request_one_read_per_member() {
    let mut app = App::new();
    app.add_plugin(HybridPhysicsPlugin).unwrap();
    let world = app.world_mut();
    let id = |slot| PhysicsId {
        slot,
        generation: 1,
    };
    world.spawn((id(0), ObjectClasses::new(["debris"])));
    world.spawn((id(1), ObjectClasses::new(["debris", "hot"])));
    world.spawn((id(2), ObjectClasses::new(["enemy"])));
    world.spawn(ObjectClasses::new(["debris"]));

    assert_eq!(request_gpu_class_snapshot(world, "debris"), 2);
    let mut commands = world.resource::<GpuPhysicsCommands>().commands.clone();
    commands.sort_by_key(|(id, _)| id.slot);
    assert_eq!(
        commands,
        [
            (id(0), GpuBodyCommand::ReadState),
            (id(1), GpuBodyCommand::ReadState)
        ]
    );
}

#[test]
fn condition_shaders_reach_the_render_world_with_registered_events() {
    let mut app = App::new();
    app.add_plugin(HybridPhysicsPlugin).unwrap();
    app.add_plugin(RenderExtractPlugin).unwrap();
    let existing = app
        .world_mut()
        .resource_mut::<GpuEventRegistry>()
        .register("landed");
    app.world_mut().resource_mut::<GpuConditionShaders>().0 = vec![
        GpuConditionShader {
            events: vec!["fell".into(), "landed".into()],
            glsl: "void condition(inout PhysicsState body) {}".into(),
            params: vec![[1.0, 2.0, 3.0, 4.0]],
            events_per_body: 3,
        },
        GpuConditionShader {
            events: Vec::new(),
            glsl: "void condition(inout PhysicsState body) {}".into(),
            ..Default::default()
        },
    ];
    app.update(Duration::ZERO).unwrap();

    let fell = app
        .world()
        .resource::<GpuEventRegistry>()
        .id("fell")
        .unwrap();
    let sources = &app.world().resource::<RenderWorld>().gpu_condition_shaders;
    assert_eq!(sources[0].params, [[1.0, 2.0, 3.0, 4.0]]);
    assert_eq!(sources[0].events_per_body, 3);
    assert_eq!(sources[1].events_per_body, 1, "0 counts as 1");
    assert_eq!(
        sources[0].source,
        format!(
            "const uint EVENTS[2] = uint[]({}u, {}u);\n#line 1\n\
             void condition(inout PhysicsState body) {{}}",
            fell.0, existing.0
        )
    );
    // No events means no table: GLSL has no zero-length arrays.
    assert_eq!(
        sources[1].source,
        "#line 1\nvoid condition(inout PhysicsState body) {}"
    );
}

#[test]
fn environment_maps_resolve_their_texture_and_reach_the_render_world() {
    let mut app = App::new();
    app.add_plugin(crate::assets::AssetPlugin).unwrap();
    app.add_plugin(RenderExtractPlugin).unwrap();
    let texture = app
        .world_mut()
        .resource_mut::<crate::assets::AssetServer>()
        .textures
        .insert_with_path(
            "sky.png",
            crate::assets::TextureAsset {
                size: [1, 1],
                rgba8: vec![255; 4],
                color_space: crate::assets::TextureColorSpace::Srgb,
                sampler: crate::assets::TextureSampler::default(),
            },
        )
        .unwrap();
    // An empty path loads nothing, so the second map is the one used.
    app.spawn(EnvironmentMap::default());
    app.spawn(EnvironmentMap {
        texture: "sky.png".into(),
        intensity: 2.0,
        ..EnvironmentMap::default()
    });
    app.update(Duration::ZERO).unwrap();
    assert_eq!(
        app.world().resource::<RenderWorld>().environment,
        Some((texture, 2.0))
    );
}

#[test]
fn clicking_a_rendered_cube_fires_a_click_event() {
    let mut app = App::new();
    app.add_plugin(crate::assets::AssetPlugin).unwrap();
    app.add_plugin(RenderExtractPlugin).unwrap();
    let (mesh, material) = {
        let assets = app.world().resource::<crate::assets::AssetServer>();
        (assets.fallback_mesh, assets.fallback_material)
    };
    app.spawn((
        Transform::default().with_position(0.0, 0.0, 5.0),
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
    let cube = app.spawn((
        Transform::default(),
        MeshRenderer {
            mesh,
            material,
            cast_shadows: true,
            receive_shadows: true,
        },
    ));

    // Frame 1 only extracts the active camera into `RenderWorld`; click
    // routing reads that extraction, so the click itself is set up for the
    // next frame.
    app.update(Duration::ZERO).unwrap();

    {
        let mut input = app.world_mut().resource_mut::<RuntimeInput>();
        input.record_viewport_size([800.0, 600.0]);
        input.record_cursor_position([400.0, 300.0]);
        input.record_mouse_button(MouseButton::Left, true);
    }
    // Frame 2 runs `route_click_events`, which enqueues the event as
    // `pending`. `EventQueue::begin_frame` swaps `pending` into `current` at
    // the *start* of a frame, so the event only becomes readable in the
    // frame after it was sent.
    app.update(Duration::ZERO).unwrap();
    app.update(Duration::ZERO).unwrap();

    let events = app.world().resource::<EventQueue<ClickEvent>>();
    let event = events.iter().next().expect("a click event was fired");
    assert_eq!(event.entity, cube);
    assert_eq!(event.button, MouseButton::Left);
}

fn cpu_body(
    world: &mut bevy_ecs::world::World,
    position: [f32; 3],
    shape: ColliderShape,
    kind: RigidBodyKind,
) -> bevy_ecs::entity::Entity {
    world
        .spawn((
            Transform::new(position),
            PhysicsBody::default(),
            RigidBody {
                kind,
                ..RigidBody::default()
            },
            Collider {
                shape,
                ..Collider::default()
            },
        ))
        .id()
}

fn run_fixed_steps(app: &mut App, steps: u32) {
    for _ in 0..steps {
        app.update(Duration::from_secs_f64(1.0 / 60.0)).unwrap();
    }
}

const UNIT_BOX: ColliderShape = ColliderShape::Box {
    half_extents: [0.5; 3],
};

fn cpu_ground(world: &mut bevy_ecs::world::World) {
    cpu_body(
        world,
        [0.0, -0.5, 0.0],
        ColliderShape::Box {
            half_extents: [5.0, 0.5, 5.0],
        },
        RigidBodyKind::Fixed,
    );
}

#[test]
fn cpu_tilted_box_tips_over_onto_a_face() {
    let mut app = App::new();
    let world = app.world_mut();
    cpu_ground(world);
    let tilted =
        cpu_body(world, [0.0, 1.5, 0.0], UNIT_BOX, RigidBodyKind::Dynamic);
    world.get_mut::<Transform>(tilted).unwrap().rotation = [0.3, 0.0, 0.2];
    run_fixed_steps(&mut app, 300);

    // Resting on an edge or corner would leave it above half its size.
    let world = app.world();
    let position = world.get::<Transform>(tilted).unwrap().position;
    assert!(
        (position[1] - 0.5).abs() < 0.03,
        "box rests at {position:?}"
    );
    let spin = world.get::<RigidBody>(tilted).unwrap().angular_velocity;
    assert!(
        spin.iter().all(|w| w.abs() < 0.1),
        "box still spins {spin:?}"
    );
}

#[test]
fn cpu_sphere_sliding_on_the_ground_starts_rolling() {
    let mut app = App::new();
    let world = app.world_mut();
    cpu_ground(world);
    let ball = cpu_body(
        world,
        [0.0, 0.5, 0.0],
        ColliderShape::Sphere { radius: 0.5 },
        RigidBodyKind::Dynamic,
    );
    world.get_mut::<RigidBody>(ball).unwrap().linear_velocity = [3.0, 0.0, 0.0];
    run_fixed_steps(&mut app, 60);

    // Friction turns sliding into rolling: spin matches speed over radius.
    let rigid = app.world().get::<RigidBody>(ball).unwrap();
    let speed = rigid.linear_velocity[0];
    assert!(speed > 1.0 && speed < 3.0, "ball moves {speed}");
    let rolling = -speed / 0.5;
    assert!(
        (rigid.angular_velocity[2] - rolling).abs() < 0.1 * rolling.abs(),
        "ball spins {:?} at speed {speed}",
        rigid.angular_velocity
    );
}

#[test]
fn cpu_convex_mesh_lands_flat_on_a_triangle_mesh_floor() {
    use crate::assets::{MeshAsset, MeshVertex, PrimitiveShape};
    let mut app = App::new();
    app.add_plugin(crate::assets::AssetPlugin).unwrap();
    let (cube, floor, material) = {
        let mut assets =
            app.world_mut().resource_mut::<crate::assets::AssetServer>();
        let floor = assets.meshes.insert(MeshAsset {
            vertices: [
                [-10.0, -10.0],
                [10.0, -10.0],
                [10.0, 10.0],
                [-10.0, 10.0],
            ]
            .map(|[x, z]| MeshVertex {
                position: [x, 0.0, z],
                ..MeshVertex::default()
            })
            .to_vec(),
            indices: vec![0, 2, 1, 0, 3, 2],
        });
        (
            assets.builtin_primitives[&PrimitiveShape::Cube],
            floor,
            assets.fallback_material,
        )
    };
    let world = app.world_mut();
    let mut spawn = |mesh, position, shape, kind| {
        let entity = cpu_body(world, position, shape, kind);
        world.entity_mut(entity).insert(MeshRenderer {
            mesh,
            material,
            cast_shadows: true,
            receive_shadows: true,
        });
        entity
    };
    // A dynamic triangle mesh still never moves.
    let ground = spawn(
        floor,
        [0.0; 3],
        ColliderShape::TriangleMesh,
        RigidBodyKind::Dynamic,
    );
    let falling = spawn(
        cube,
        [0.0, 1.5, 0.0],
        ColliderShape::ConvexMesh,
        RigidBodyKind::Dynamic,
    );
    world.get_mut::<Transform>(falling).unwrap().rotation = [0.3, 0.0, 0.2];
    run_fixed_steps(&mut app, 300);

    let world = app.world();
    assert_eq!(world.get::<Transform>(ground).unwrap().position, [0.0; 3]);
    let position = world.get::<Transform>(falling).unwrap().position;
    assert!(
        (position[1] - 0.5).abs() < 0.03,
        "cube rests at {position:?}"
    );
    let hit = world
        .resource::<PhysicsWorld>()
        .raycast([4.0, 5.0, 4.0], [0.0, -1.0, 0.0], 20.0, u32::MAX)
        .unwrap();
    assert_eq!(hit.entity, ground);
    assert!((hit.distance - 5.0).abs() < 1e-4);
}

#[test]
fn a_barrel_with_a_convex_cylinder_collider_rolls() {
    use crate::assets::PrimitiveShape;
    let mut app = App::new();
    app.add_plugin(crate::assets::AssetPlugin).unwrap();
    let (cylinder, material) = {
        let assets = app.world().resource::<crate::assets::AssetServer>();
        (
            assets.builtin_primitives[&PrimitiveShape::Cylinder],
            assets.fallback_material,
        )
    };
    let world = app.world_mut();
    cpu_body(
        world,
        [0.0, -0.5, 0.0],
        ColliderShape::Box {
            half_extents: [20.0, 0.5, 20.0],
        },
        RigidBodyKind::Fixed,
    );
    // A unit cylinder on its side, axis along Z, rolled along X.
    let barrel = cpu_body(
        world,
        [0.0, 0.51, 0.0],
        ColliderShape::ConvexMesh,
        RigidBodyKind::Dynamic,
    );
    world.entity_mut(barrel).insert(MeshRenderer {
        mesh: cylinder,
        material,
        cast_shadows: true,
        receive_shadows: true,
    });
    let mut transform = world.get_mut::<Transform>(barrel).unwrap();
    transform.rotation = [std::f32::consts::FRAC_PI_2, 0.0, 0.0];
    world.get_mut::<RigidBody>(barrel).unwrap().linear_velocity =
        [3.0, 0.0, 0.0];
    run_fixed_steps(&mut app, 60);
    let world = app.world();
    let position = world.get::<Transform>(barrel).unwrap().position;
    let spin = world.get::<RigidBody>(barrel).unwrap().angular_velocity;
    // The barrel rolls, spinning about Z, and stays on its side.
    assert!(position[0] > 1.0, "barrel at {position:?}");
    assert!(spin[2] < -0.5, "spin {spin:?}");
    assert!((position[1] - 0.5).abs() < 0.05, "barrel at {position:?}");
}

#[test]
fn cpu_boxes_fall_and_rest_in_a_stack_on_static_ground() {
    let mut app = App::new();
    let world = app.world_mut();
    let ground = cpu_body(
        world,
        [0.0, -0.5, 0.0],
        ColliderShape::Box {
            half_extents: [5.0, 0.5, 5.0],
        },
        RigidBodyKind::Fixed,
    );
    let boxes = [0.5, 1.6, 2.7].map(|y| {
        cpu_body(world, [0.0, y, 0.0], UNIT_BOX, RigidBodyKind::Dynamic)
    });
    run_fixed_steps(&mut app, 180);

    let world = app.world();
    assert_eq!(world.get::<Transform>(ground).unwrap().position[1], -0.5);
    for (level, entity) in boxes.into_iter().enumerate() {
        let position = world.get::<Transform>(entity).unwrap().position;
        let expected = 0.5 + level as f32;
        assert!(
            (position[1] - expected).abs() < 0.03,
            "box {level} rests at {position:?}"
        );
        // Landing leaves a millimetre of sideways slip and a sliver of tilt.
        assert!(
            position[0].abs() < 5e-3 && position[2].abs() < 5e-3,
            "box {level} drifts to {position:?}"
        );
        let rotation = world.get::<Transform>(entity).unwrap().rotation;
        assert!(
            rotation.iter().all(|angle| angle.abs() < 0.01),
            "box {level} tilts to {rotation:?}"
        );
        let velocity = world.get::<RigidBody>(entity).unwrap().linear_velocity;
        assert!(velocity[1].abs() < 0.2, "box {level} moves {velocity:?}");
    }
    let hit = world
        .resource::<PhysicsWorld>()
        .raycast([0.0, 10.0, 0.0], [0.0, -1.0, 0.0], 20.0, u32::MAX)
        .unwrap();
    assert_eq!(hit.entity, boxes[2]);
    assert!((hit.point[1] - 3.0).abs() < 0.03);
}

#[test]
fn cpu_restitution_bounces_and_sensors_only_report() {
    let mut app = App::new();
    let world = app.world_mut();
    cpu_body(
        world,
        [0.0, -0.5, 0.0],
        ColliderShape::Box {
            half_extents: [5.0, 0.5, 5.0],
        },
        RigidBodyKind::Fixed,
    );
    let ball = cpu_body(
        world,
        [0.0, 3.0, 0.0],
        ColliderShape::Sphere { radius: 0.5 },
        RigidBodyKind::Dynamic,
    );
    world.get_mut::<Collider>(ball).unwrap().restitution = 0.8;
    let sensor =
        cpu_body(world, [4.0, 0.5, 0.0], UNIT_BOX, RigidBodyKind::Fixed);
    world.get_mut::<Collider>(sensor).unwrap().sensor = true;
    let walker = cpu_body(
        world,
        [2.0, 0.5, 0.0],
        ColliderShape::Sphere { radius: 0.4 },
        RigidBodyKind::Kinematic,
    );
    world.get_mut::<RigidBody>(walker).unwrap().linear_velocity =
        [3.0, 0.0, 0.0];

    let mut peak_after_bounce = f32::MIN;
    let mut bounced = false;
    let mut sensor_events = 0;
    for _ in 0..90 {
        run_fixed_steps(&mut app, 1);
        let world = app.world();
        let ball_state = world.get::<RigidBody>(ball).unwrap();
        bounced |= ball_state.linear_velocity[1] > 1.0;
        if bounced {
            peak_after_bounce = peak_after_bounce
                .max(world.get::<Transform>(ball).unwrap().position[1]);
        }
        sensor_events += world
            .resource::<EventQueue<CollisionEvent>>()
            .iter()
            .filter(|event| event.sensor)
            .count();
    }
    assert!(bounced, "the ball never bounced");
    assert!(
        (1.5..3.0).contains(&peak_after_bounce),
        "bounce peak {peak_after_bounce}"
    );
    // The kinematic walker passed through the sensor, which never pushed it.
    assert!(sensor_events > 0);
    let walker_x = app.world().get::<Transform>(walker).unwrap().position[0];
    assert!((walker_x - (2.0 + 3.0 * 1.5)).abs() < 1e-3, "{walker_x}");
}

#[test]
fn cpu_physics_is_deterministic_and_ignores_gpu_bodies() {
    let run = || {
        let mut app = App::new();
        let world = app.world_mut();
        cpu_body(
            world,
            [0.0, -0.5, 0.0],
            ColliderShape::Box {
                half_extents: [5.0, 0.5, 5.0],
            },
            RigidBodyKind::Fixed,
        );
        let bodies = (0..6)
            .map(|index| {
                let shape = if index % 2 == 0 {
                    UNIT_BOX
                } else {
                    ColliderShape::Capsule {
                        half_height: 0.3,
                        radius: 0.3,
                    }
                };
                cpu_body(
                    world,
                    [index as f32 * 0.3, 1.0 + index as f32, 0.1],
                    shape,
                    RigidBodyKind::Dynamic,
                )
            })
            .collect::<Vec<_>>();
        let gpu =
            cpu_body(world, [0.0, 5.0, 0.0], UNIT_BOX, RigidBodyKind::Dynamic);
        world.get_mut::<PhysicsBody>(gpu).unwrap().simulation =
            SimulationClass::Gpu;
        run_fixed_steps(&mut app, 120);
        let world = app.world();
        assert_eq!(world.get::<Transform>(gpu).unwrap().position[1], 5.0);
        bodies
            .iter()
            .map(|entity| world.get::<Transform>(*entity).unwrap().position)
            .collect::<Vec<_>>()
    };
    assert_eq!(run(), run());
}

#[test]
fn cpu_collision_layers_filter_pairs_and_queries() {
    let mut app = App::new();
    let world = app.world_mut();
    let ground = cpu_body(
        world,
        [0.0, -0.5, 0.0],
        ColliderShape::Box {
            half_extents: [5.0, 0.5, 5.0],
        },
        RigidBodyKind::Fixed,
    );
    world.entity_mut(ground).insert(CollisionLayers {
        memberships: 0b01,
        filters: 0b01,
    });
    let solid =
        cpu_body(world, [-2.0, 0.5, 0.0], UNIT_BOX, RigidBodyKind::Dynamic);
    // On layer 2 only: the ground does not accept it, so it falls through.
    let ghost =
        cpu_body(world, [2.0, 0.5, 0.0], UNIT_BOX, RigidBodyKind::Dynamic);
    world.entity_mut(ghost).insert(CollisionLayers {
        memberships: 0b10,
        filters: u32::MAX,
    });
    run_fixed_steps(&mut app, 60);

    let world = app.world();
    assert!(
        (world.get::<Transform>(solid).unwrap().position[1] - 0.5).abs() < 0.03
    );
    assert!(world.get::<Transform>(ghost).unwrap().position[1] < -2.0);
    let physics = world.resource::<PhysicsWorld>();
    let down = [0.0, -1.0, 0.0];
    let hit = physics
        .raycast([-2.0, 5.0, 0.0], down, 20.0, u32::MAX)
        .unwrap();
    assert_eq!(hit.entity, solid);
    let hit = physics.raycast([-2.0, 5.0, 0.0], down, 20.0, 0b01).unwrap();
    assert_eq!(hit.entity, solid, "the solid box has no layers: all bits");
    let ground_only = physics.overlap_sphere([4.0, 0.0, 0.0], 0.5, 0b01);
    assert_eq!(ground_only, vec![ground]);
    assert!(physics
        .overlap_sphere([4.0, 0.0, 0.0], 0.5, 0b100)
        .is_empty());
}

#[test]
fn cpu_bodies_sleep_when_still_and_wake_on_touch_or_edit() {
    let mut app = App::new();
    let world = app.world_mut();
    let ground = cpu_body(
        world,
        [0.0, -0.5, 0.0],
        ColliderShape::Box {
            half_extents: [5.0, 0.5, 5.0],
        },
        RigidBodyKind::Fixed,
    );
    let resting =
        cpu_body(world, [0.0, 0.5, 0.0], UNIT_BOX, RigidBodyKind::Dynamic);
    let pushed =
        cpu_body(world, [3.0, 0.5, 0.0], UNIT_BOX, RigidBodyKind::Dynamic);
    run_fixed_steps(&mut app, SLEEP_STEPS + 10);
    assert!(app.world().get::<Sleeping>(resting).is_some());
    assert!(app.world().get::<Sleeping>(pushed).is_some());

    // A sleeping body stays put and keeps no change-detection churn.
    let before = *app.world().get::<Transform>(resting).unwrap();
    run_fixed_steps(&mut app, 10);
    assert_eq!(*app.world().get::<Transform>(resting).unwrap(), before);
    // Sleepers are not tested against the ground, yet still touch it.
    let touches = |app: &App, body| {
        app.world()
            .resource::<PhysicsWorld>()
            .contacts()
            .iter()
            .any(|contact| {
                (contact.a, contact.b) == (body, ground)
                    || (contact.a, contact.b) == (ground, body)
            })
    };
    assert!(touches(&app, resting));
    assert!(touches(&app, pushed));

    // Gameplay velocity wakes a body.
    app.world_mut()
        .get_mut::<RigidBody>(pushed)
        .unwrap()
        .linear_velocity = [0.0, 4.0, 0.0];
    run_fixed_steps(&mut app, 1);
    assert!(app.world().get::<Sleeping>(pushed).is_none());
    assert!(app.world().get::<Transform>(pushed).unwrap().position[1] > 0.5);
    run_fixed_steps(&mut app, 2);
    assert!(
        !touches(&app, pushed),
        "a body that left the ground lets go"
    );

    // A falling box wakes the sleeper it lands on.
    let dropped = cpu_body(
        app.world_mut(),
        [0.0, 3.0, 0.0],
        UNIT_BOX,
        RigidBodyKind::Dynamic,
    );
    let mut woke = false;
    for _ in 0..60 {
        run_fixed_steps(&mut app, 1);
        woke |= app.world().get::<Sleeping>(resting).is_none();
    }
    assert!(woke, "the landing box must wake the resting one");
    let top = app.world().get::<Transform>(dropped).unwrap().position[1];
    assert!((top - 1.5).abs() < 0.05, "dropped box rests on top: {top}");
}

#[test]
fn sleeping_cpu_bodies_fall_when_their_support_is_removed_or_moved() {
    let mut app = App::new();
    let world = app.world_mut();
    let trapdoor = |world: &mut World, x| {
        cpu_body(
            world,
            [x, -0.5, 0.0],
            ColliderShape::Box {
                half_extents: [1.0, 0.5, 1.0],
            },
            RigidBodyKind::Fixed,
        )
    };
    let removed = trapdoor(world, -3.0);
    let moved = trapdoor(world, 3.0);
    let on_removed =
        cpu_body(world, [-3.0, 0.5, 0.0], UNIT_BOX, RigidBodyKind::Dynamic);
    let on_moved =
        cpu_body(world, [3.0, 0.5, 0.0], UNIT_BOX, RigidBodyKind::Dynamic);
    run_fixed_steps(&mut app, SLEEP_STEPS + 10);
    assert!(app.world().get::<Sleeping>(on_removed).is_some());
    assert!(app.world().get::<Sleeping>(on_moved).is_some());

    app.world_mut().despawn(removed);
    app.world_mut()
        .get_mut::<Transform>(moved)
        .unwrap()
        .position[1] = -5.0;
    run_fixed_steps(&mut app, 30);
    for body in [on_removed, on_moved] {
        let y = app.world().get::<Transform>(body).unwrap().position[1];
        assert!(y < -0.5, "the box falls once its trapdoor goes: {y}");
    }
}

#[test]
fn fast_cpu_bodies_do_not_tunnel_through_thin_walls() {
    let mut app = App::new();
    let world = app.world_mut();
    world.resource_mut::<PhysicsSettings>().gravity = [0.0; 3];
    cpu_body(
        world,
        [0.0, 0.0, -5.0],
        ColliderShape::Box {
            half_extents: [2.0, 2.0, 0.05],
        },
        RigidBodyKind::Fixed,
    );
    let bullet = cpu_body(
        world,
        [0.0; 3],
        ColliderShape::Sphere { radius: 0.1 },
        RigidBodyKind::Dynamic,
    );
    // 270 m/s moves 4.5 m per 60 Hz step: z = -4.5, then -9 without a sweep.
    world.get_mut::<RigidBody>(bullet).unwrap().linear_velocity =
        [0.0, 0.0, -270.0];
    run_fixed_steps(&mut app, 5);
    let z = app.world().get::<Transform>(bullet).unwrap().position[2];
    assert!(z > -5.0 && z < -4.8, "bullet stops at the wall: {z}");
}

#[test]
fn a_fast_ball_between_two_dynamic_boxes_pushes_both_back() {
    let mut app = App::new();
    let world = app.world_mut();
    world.resource_mut::<PhysicsSettings>().gravity = [0.0; 3];
    let half_extents = [0.3; 3];
    // A 0.02 m seam: the ball's center ray passes between the boxes.
    let boxes = [-0.31, 0.31].map(|x| {
        cpu_body(
            world,
            [x, 0.0, -5.0],
            ColliderShape::Box { half_extents },
            RigidBodyKind::Dynamic,
        )
    });
    let ball = cpu_body(
        world,
        [0.0; 3],
        ColliderShape::Sphere { radius: 0.2 },
        RigidBodyKind::Dynamic,
    );
    // 22 m/s moves 0.37 m per step, farther than the ball's radius.
    world.get_mut::<RigidBody>(ball).unwrap().linear_velocity =
        [0.0, 0.0, -22.0];
    run_fixed_steps(&mut app, 30);
    let z = |entity| app.world().get::<Transform>(entity).unwrap().position[2];
    for block in boxes {
        assert!(
            z(block) < -5.3,
            "the ball pushes each box back: {}",
            z(block)
        );
    }
    // Unhindered, the ball would reach z = -11; the boxes took its momentum.
    let ball_z = z(ball);
    assert!(ball_z > -8.0, "the ball hands its momentum over: {ball_z}");
}

#[test]
fn gpu_bodies_extract_their_collider_and_see_cpu_colliders() {
    let mut app = App::new();
    app.add_plugin(HybridPhysicsPlugin).unwrap();
    app.add_plugin(RenderExtractPlugin).unwrap();
    let world = app.world_mut();
    cpu_body(
        world,
        [0.0, -0.5, 0.0],
        ColliderShape::Box {
            half_extents: [5.0, 0.5, 5.0],
        },
        RigidBodyKind::Fixed,
    );
    let sensor =
        cpu_body(world, [3.0, 0.0, 0.0], UNIT_BOX, RigidBodyKind::Fixed);
    world.get_mut::<Collider>(sensor).unwrap().sensor = true;
    let gpu =
        cpu_body(world, [0.0, 5.0, 0.0], UNIT_BOX, RigidBodyKind::Dynamic);
    world.get_mut::<PhysicsBody>(gpu).unwrap().simulation =
        SimulationClass::Gpu;
    run_fixed_steps(&mut app, 2);

    let render_world = app.world().resource::<RenderWorld>();
    assert_eq!(render_world.gpu_physics.len(), 1);
    let (collider, _) = render_world.gpu_physics[0].collider.unwrap();
    assert_eq!(collider.shape, UNIT_BOX);
    // The ground only: sensors and GPU bodies are not GPU colliders.
    assert_eq!(render_world.gpu_colliders.len(), 1);
    let ground = render_world.gpu_colliders[0];
    assert_eq!(ground.shape, [0.0, 5.0, 0.5, 5.0]);
    assert_eq!(ground.model[3], [0.0, -0.5, 0.0, 1.0]);

    // Editing a GPU body's collider rebuilds its GPU tables.
    let revision = render_world.gpu_physics_revision;
    app.world_mut().get_mut::<Collider>(gpu).unwrap().friction = 0.9;
    run_fixed_steps(&mut app, 1);
    assert_ne!(
        app.world().resource::<RenderWorld>().gpu_physics_revision,
        revision
    );
}

#[test]
fn custom_solver_files_reach_the_render_world_once_per_path() {
    let dir = std::env::temp_dir()
        .join(format!("rusting-custom-solver-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("rise.glsl").to_string_lossy().into_owned();
    std::fs::write(&path, "void solve(inout PhysicsState body) {}").unwrap();
    let missing = dir.join("missing.glsl").to_string_lossy().into_owned();

    let mut app = App::new();
    app.add_plugin(HybridPhysicsPlugin).unwrap();
    app.add_plugin(RenderExtractPlugin).unwrap();
    let world = app.world_mut();
    for (solver, shader) in [
        (PhysicsSolver::Custom, Some(&path)),
        (PhysicsSolver::Custom, Some(&path)),
        (PhysicsSolver::Custom, Some(&missing)),
        // A leftover path on a non-custom body is ignored.
        (PhysicsSolver::Full, Some(&path)),
    ] {
        let body = cpu_body(world, [0.0; 3], UNIT_BOX, RigidBodyKind::Dynamic);
        let mut physics = world.get_mut::<PhysicsBody>(body).unwrap();
        physics.simulation = SimulationClass::Gpu;
        physics.solver = solver;
        physics.custom_shader = shader.cloned();
    }
    run_fixed_steps(&mut app, 1);

    let render_world = app.world().resource::<RenderWorld>();
    let shaders = render_world
        .gpu_physics
        .iter()
        .map(|body| body.custom_shader.as_deref())
        .collect::<Vec<_>>();
    assert_eq!(shaders.iter().filter(|shader| shader.is_some()).count(), 3);
    let solvers = &render_world.gpu_solver_shaders;
    assert_eq!(solvers.len(), 2, "{solvers:?}");
    let rise = format!("SOLVER_ID = {}u", custom_solver_id(&path));
    assert!(solvers.iter().any(|source| source.contains(&rise)
        && source.contains("void solve(inout PhysicsState body) {}")));
    assert!(solvers
        .iter()
        .any(|source| source.contains("#error cannot read custom solver")));
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn cpu_shape_casts_stop_before_colliders_and_characters_slide() {
    let mut app = App::new();
    let world = app.world_mut();
    let ground = cpu_body(
        world,
        [0.0, -0.5, 0.0],
        ColliderShape::Box {
            half_extents: [10.0, 0.5, 10.0],
        },
        RigidBodyKind::Fixed,
    );
    let wall = cpu_body(
        world,
        [3.0, 1.0, 0.0],
        ColliderShape::Box {
            half_extents: [0.5, 1.0, 5.0],
        },
        RigidBodyKind::Fixed,
    );
    run_fixed_steps(&mut app, 1);
    let physics = app.world().resource::<PhysicsWorld>();
    let ball = ColliderShape::Sphere { radius: 0.3 };
    let capsule = ColliderShape::Capsule {
        half_height: 0.5,
        radius: 0.3,
    };

    let down = physics
        .shape_cast(
            ball,
            [0.0, 1.0, 0.0],
            [0.0, -1.0, 0.0],
            5.0,
            u32::MAX,
            None,
        )
        .unwrap();
    assert_eq!(down.entity, ground);
    assert!((down.distance - 0.7).abs() < 1e-3, "{down:?}");
    assert!(down.normal[1] > 0.99, "{down:?}");

    let side = physics
        .shape_cast(
            capsule,
            [0.0, 1.0, 0.0],
            [1.0, 0.0, 0.0],
            5.0,
            u32::MAX,
            None,
        )
        .unwrap();
    assert_eq!(side.entity, wall);
    assert!((side.distance - 2.2).abs() < 1e-3, "{side:?}");
    assert!(side.normal[0] < -0.99, "{side:?}");
    // Excluded, masked-out, or out-of-range colliders are not hit.
    let cast = |exclude, mask, max| {
        physics.shape_cast(
            capsule,
            [0.0, 1.0, 0.0],
            [1.0, 0.0, 0.0],
            max,
            mask,
            exclude,
        )
    };
    assert_eq!(cast(Some(wall), u32::MAX, 5.0), None);
    assert_eq!(cast(None, 0, 5.0), None);
    assert_eq!(cast(None, u32::MAX, 2.0), None);

    // A ball resting on the ground can lift off but not sink.
    let resting = [0.0, 0.29, 0.0];
    assert_eq!(
        physics.shape_cast(ball, resting, [0.0, 1.0, 0.0], 1.0, u32::MAX, None),
        None
    );
    let sink = physics
        .shape_cast(ball, resting, [0.0, -1.0, 0.0], 1.0, u32::MAX, None)
        .unwrap();
    assert_eq!((sink.entity, sink.distance), (ground, 0.0));

    // Moving down and into the wall slides along the floor to the wall.
    let moved = physics.move_character(
        capsule,
        [0.0, 1.0, 0.0],
        [5.0, -2.0, 1.0],
        u32::MAX,
        None,
    );
    assert!(moved.grounded);
    assert!((moved.position[0] - 2.19).abs() < 0.02, "{moved:?}");
    assert!((moved.position[1] - 0.81).abs() < 0.02, "{moved:?}");
    // The motion along the wall is kept.
    assert!((moved.position[2] - 1.0).abs() < 0.02, "{moved:?}");
}

#[test]
fn gpu_events_place_move_and_remove_a_cpu_query_proxy() {
    let mut app = App::new();
    app.add_plugin(HybridPhysicsPlugin).unwrap();
    let (place, remove) = {
        let mut registry = app.world_mut().resource_mut::<GpuEventRegistry>();
        (registry.register("seen"), registry.register("gone"))
    };
    let body = app.spawn((
        Transform::default(),
        PhysicsBody {
            simulation: SimulationClass::Gpu,
            ..PhysicsBody::default()
        },
        GpuQueryProxy {
            collider: Collider {
                shape: ColliderShape::Sphere { radius: 0.5 },
                ..Collider::default()
            },
            layers: CollisionLayers::default(),
            place_on: place,
            remove_on: Some(remove),
        },
    ));
    app.update(Duration::ZERO).unwrap();
    let id = *app.world().get::<PhysicsId>(body).unwrap();
    let send = |app: &mut App, event: GpuEventId, x: f32| {
        let raw = RawGpuPhysicsEvent {
            body_slot: id.slot,
            body_generation: id.generation,
            event_id: event.0,
            payload_kind: GpuEventPayload::Position as u32,
            payload: [x, 0.0, 0.0, 0.0],
            ..Default::default()
        };
        route_gpu_physics_events(app.world_mut(), &[raw]);
        app.update(Duration::ZERO).unwrap();
        run_fixed_steps(app, 1);
    };
    let down = [0.0, -1.0, 0.0];
    let hit_x = |app: &App, x: f32| {
        app.world()
            .resource::<PhysicsWorld>()
            .raycast([x, 5.0, 0.0], down, 20.0, u32::MAX)
            .map(|hit| hit.entity)
    };

    send(&mut app, place, 3.0);
    let proxy = hit_x(&app, 3.0).expect("proxy is placed");
    assert_eq!(
        app.world().get::<GpuProxyOf>(proxy),
        Some(&GpuProxyOf(body))
    );

    send(&mut app, place, -3.0);
    assert_eq!(hit_x(&app, 3.0), None);
    assert_eq!(hit_x(&app, -3.0), Some(proxy), "the same proxy moves");

    send(&mut app, remove, 0.0);
    assert_eq!(hit_x(&app, -3.0), None);
    assert!(app.world().get_entity(proxy).is_err());
}

#[test]
fn auto_simulation_picks_cpu_or_gpu_and_stays_overridable() {
    let mut app = App::new();
    app.add_plugin(HybridPhysicsPlugin).unwrap();
    app.insert_resource(PhysicsBackendStatus {
        gameplay_available: true,
        gpu_dynamic_available: true,
    });
    app.insert_resource(AutoAllocationPolicy {
        gpu_min_bodies: 3,
        ..AutoAllocationPolicy::default()
    });
    let auto = |app: &mut App, extra: PhysicsBody| {
        app.spawn((Transform::default(), extra, AutoSimulation::default()))
    };
    let decision = |app: &App, entity| {
        let decision = app.world().get::<AutoSimulation>(entity).unwrap();
        let class = app.world().get::<PhysicsBody>(entity).unwrap().simulation;
        let decision = decision.decision.unwrap();
        assert_eq!(decision.class, class);
        (class, decision.reason)
    };

    // Two flexible bodies stay under the threshold.
    let first = auto(&mut app, PhysicsBody::default());
    let second = auto(&mut app, PhysicsBody::default());
    app.update(Duration::ZERO).unwrap();
    for entity in [first, second] {
        assert_eq!(
            decision(&app, entity),
            (SimulationClass::Cpu, AllocationReason::FewBodies)
        );
    }

    // A third reaches it: only the new body goes to the GPU, and gets its
    // GPU ID in the same update. Decided bodies never migrate.
    let third = auto(&mut app, PhysicsBody::default());
    app.update(Duration::ZERO).unwrap();
    assert_eq!(
        decision(&app, third),
        (SimulationClass::Gpu, AllocationReason::ManyBodies)
    );
    assert!(app.world().get::<PhysicsId>(third).is_some());
    assert_eq!(decision(&app, first).0, SimulationClass::Cpu);

    // Hard requirements win over the count.
    let kinematic = auto(&mut app, PhysicsBody::default());
    app.world_mut().entity_mut(kinematic).insert(RigidBody {
        kind: RigidBodyKind::Kinematic,
        ..RigidBody::default()
    });
    let sensor = auto(&mut app, PhysicsBody::default());
    app.world_mut().entity_mut(sensor).insert(Collider {
        sensor: true,
        ..Collider::default()
    });
    let watched = auto(&mut app, PhysicsBody::default());
    app.world_mut()
        .entity_mut(watched)
        .insert(PhysicsSyncMode::SelectedState);
    let custom = auto(
        &mut app,
        PhysicsBody {
            solver: PhysicsSolver::Custom,
            ..PhysicsBody::default()
        },
    );
    let wall = auto(
        &mut app,
        PhysicsBody {
            simulation: SimulationClass::Static,
            ..PhysicsBody::default()
        },
    );
    app.update(Duration::ZERO).unwrap();
    assert_eq!(decision(&app, kinematic).1, AllocationReason::Kinematic);
    assert_eq!(decision(&app, sensor).1, AllocationReason::CpuOnlyCollider);
    assert_eq!(
        decision(&app, watched).1,
        AllocationReason::ReadsStateEveryTick
    );
    assert_eq!(
        decision(&app, custom),
        (SimulationClass::Gpu, AllocationReason::CustomSolver)
    );
    assert_eq!(
        decision(&app, wall),
        (SimulationClass::Static, AllocationReason::NotDynamic)
    );

    // Manual selection: without the component the class is left alone.
    let manual = app.spawn((
        Transform::default(),
        PhysicsBody {
            simulation: SimulationClass::Gpu,
            ..PhysicsBody::default()
        },
    ));
    app.world_mut().entity_mut(first).remove::<AutoSimulation>();
    app.world_mut()
        .get_mut::<PhysicsBody>(first)
        .unwrap()
        .simulation = SimulationClass::Gpu;
    // Clearing a decision asks for a new one under the current policy.
    app.world_mut()
        .resource_mut::<AutoAllocationPolicy>()
        .gpu_min_bodies = 2;
    app.world_mut()
        .get_mut::<AutoSimulation>(second)
        .unwrap()
        .decision = None;
    app.update(Duration::ZERO).unwrap();
    let world = app.world();
    assert_eq!(
        world.get::<PhysicsBody>(manual).unwrap().simulation,
        SimulationClass::Gpu
    );
    assert_eq!(
        world.get::<PhysicsBody>(first).unwrap().simulation,
        SimulationClass::Gpu
    );
    assert_eq!(
        decision(&app, second),
        (SimulationClass::Gpu, AllocationReason::ManyBodies)
    );

    // Without a GPU backend new bodies fall back to the CPU.
    app.insert_resource(PhysicsBackendStatus::default());
    let offline = auto(
        &mut app,
        PhysicsBody {
            solver: PhysicsSolver::Custom,
            ..PhysicsBody::default()
        },
    );
    app.update(Duration::ZERO).unwrap();
    assert_eq!(
        decision(&app, offline),
        (SimulationClass::Cpu, AllocationReason::NoGpuBackend)
    );

    // Measured CPU physics time over budget sends flexible bodies to the
    // GPU below the count threshold. The update reads last frame's time.
    app.insert_resource(PhysicsBackendStatus {
        gameplay_available: true,
        gpu_dynamic_available: true,
    });
    app.world_mut()
        .resource_mut::<AutoAllocationPolicy>()
        .gpu_min_bodies = 1_000;
    app.world_mut().resource_mut::<CpuFrameTimings>().physics =
        Duration::from_millis(10);
    let late = auto(&mut app, PhysicsBody::default());
    app.update(Duration::ZERO).unwrap();
    assert_eq!(
        decision(&app, late),
        (SimulationClass::Gpu, AllocationReason::CpuOverBudget)
    );
    // Diagnostics explain the decision with the policy's thresholds.
    let world = app.world_mut();
    let policy = *world.resource::<AutoAllocationPolicy>();
    let explain = |entity| {
        world
            .get::<AutoSimulation>(entity)
            .unwrap()
            .decision
            .unwrap()
            .explain(&policy)
    };
    assert_eq!(
        explain(late),
        "On the GPU: CPU physics took over 4ms last frame."
    );
}

#[test]
fn player_controller_falls_walks_jumps_and_stops_at_walls() {
    let mut app = App::new();
    cpu_ground(app.world_mut());
    let wall = cpu_body(
        app.world_mut(),
        [0.0, 1.0, -2.0],
        ColliderShape::Box {
            half_extents: [5.0, 1.0, 0.5],
        },
        RigidBodyKind::Fixed,
    );
    let trigger = cpu_body(
        app.world_mut(),
        [0.0, 0.5, -1.0],
        UNIT_BOX,
        RigidBodyKind::Fixed,
    );
    app.world_mut().get_mut::<Collider>(trigger).unwrap().sensor = true;
    // A kinematic capsule collider lets sensors see the player.
    let player = cpu_body(
        app.world_mut(),
        [0.0, 3.0, 0.0],
        DEFAULT_PLAYER_SHAPE,
        RigidBodyKind::Kinematic,
    );
    app.world_mut()
        .entity_mut(player)
        .insert(PlayerController::default());
    let body =
        |app: &App| app.world().get::<Transform>(player).unwrap().position;
    let state =
        |app: &App| *app.world().get::<PlayerController>(player).unwrap();
    // Runs fixed steps; true when the player touched the sensor.
    let run = |app: &mut App, steps| {
        let mut triggered = false;
        for _ in 0..steps {
            run_fixed_steps(app, 1);
            triggered |= app
                .world()
                .resource::<EventQueue<CollisionEvent>>()
                .iter()
                .any(|event| {
                    let pair = [event.a, event.b];
                    event.sensor
                        && pair.contains(&trigger)
                        && pair.contains(&player)
                });
        }
        triggered
    };

    // Gravity lands the capsule (0.9 m below its center) on the floor.
    assert!(!run(&mut app, 90));
    assert!(state(&app).grounded);
    assert!((body(&app)[1] - 0.91).abs() < 0.02, "{:?}", body(&app));

    // Forward walks toward -Z through the sensor until the wall stops it.
    app.world_mut()
        .resource_mut::<RuntimeInput>()
        .record_key(KeyCode::KeyW, true);
    let triggered = run(&mut app, 60);
    let stopped = body(&app);
    assert!((stopped[2] + 1.19).abs() < 0.02, "{stopped:?}");
    assert_eq!(stopped[0], 0.0);
    assert!(triggered, "walking through the sensor reported nothing");
    let ahead = app
        .world()
        .resource::<PhysicsWorld>()
        .raycast(stopped, [0.0, 0.0, -1.0], 1.0, 0b1)
        .unwrap();
    assert_eq!(ahead.entity, wall);

    // Jump leaves the floor, then gravity brings the player back.
    let mut input = app.world_mut().resource_mut::<RuntimeInput>();
    input.record_key(KeyCode::KeyW, false);
    input.record_key(KeyCode::Space, true);
    run(&mut app, 10);
    app.world_mut()
        .resource_mut::<RuntimeInput>()
        .clear_frame_edges();
    assert!(!state(&app).grounded);
    assert!(body(&app)[1] > 1.3, "{:?}", body(&app));
    run(&mut app, 90);
    assert!(state(&app).grounded);
    assert!((body(&app)[1] - 0.91).abs() < 0.02, "{:?}", body(&app));
}

#[test]
fn player_look_needs_captured_cursor_and_clamps_pitch() {
    let mut app = App::new();
    let player = app.spawn((Transform::default(), PlayerController::default()));
    let camera =
        app.spawn((Transform::new([0.0, 0.7, 0.0]), Camera::default()));
    app.set_parent(camera, player).unwrap();
    let look = |app: &mut App, motion: [f32; 2]| {
        app.world_mut()
            .resource_mut::<RuntimeInput>()
            .record_mouse_motion(motion);
        app.update(Duration::ZERO).unwrap();
        app.world_mut()
            .resource_mut::<RuntimeInput>()
            .clear_frame_edges();
        let player = *app.world().get::<PlayerController>(player).unwrap();
        (player.yaw, player.pitch)
    };
    assert_eq!(look(&mut app, [100.0, 0.0]), (0.0, 0.0));
    // First person ignores camera_height (0.6): the child keeps 0.7.
    assert_eq!(
        app.world().get::<Transform>(camera).unwrap().position,
        [0.0, 0.7, 0.0]
    );

    app.world_mut()
        .resource_mut::<RuntimeInput>()
        .record_mouse_button(MouseButton::Left, true);
    let (yaw, pitch) = look(&mut app, [100.0, 0.0]);
    assert!(app.world().resource::<RuntimeInput>().cursor_captured());
    assert!((yaw + 0.2).abs() < 1e-6 && pitch == 0.0);
    let (_, pitch) = look(&mut app, [0.0, -10_000.0]);
    assert!((pitch - 89.0_f32.to_radians()).abs() < 1e-6);
    {
        let world = app.world_mut();
        let mut seated = world.get_mut::<PlayerController>(player).unwrap();
        seated.pitch_limits = [-0.7, 0.7];
        seated.yaw_limits = Some([-1.0, 1.0]);
    }
    assert_eq!(look(&mut app, [10_000.0, 0.0]), (-1.0, 0.7));
    assert_eq!(look(&mut app, [-20_000.0, 10_000.0]), (1.0, -0.7));
    let camera_pitch =
        app.world().get::<Transform>(camera).unwrap().rotation[0];
    assert_eq!(camera_pitch, -0.7, "the clamp lands before the view moves");
    {
        let world = app.world_mut();
        let mut free = world.get_mut::<PlayerController>(player).unwrap();
        free.pitch_limits = PlayerController::default().pitch_limits;
        free.yaw_limits = None;
    }
    let (yaw, pitch) = look(&mut app, [-6_000.0, -10_000.0]);
    assert!((yaw - 13.0).abs() < 1e-4, "{yaw}");
    assert!((pitch - 89.0_f32.to_radians()).abs() < 1e-6);
    // Yaw turns the body; pitch tilts only the camera child.
    let world = app.world();
    assert_eq!(
        world.get::<Transform>(player).unwrap().rotation,
        [0.0, yaw, 0.0]
    );
    assert_eq!(
        world.get::<Transform>(camera).unwrap().rotation,
        [pitch, 0.0, 0.0]
    );

    app.world_mut()
        .resource_mut::<RuntimeInput>()
        .record_key(KeyCode::Escape, true);
    look(&mut app, [0.0; 2]);
    assert!(!app.world().resource::<RuntimeInput>().cursor_captured());

    // The right stick looks with no captured cursor, at 3 rad/s full tilt.
    app.world_mut()
        .resource_mut::<RuntimeInput>()
        .record_stick(crate::runtime::Stick::Right, [1.0, 0.0]);
    app.update(Duration::from_millis(100)).unwrap();
    let turned = app.world().get::<PlayerController>(player).unwrap().yaw;
    assert!((turned - (yaw - 0.3)).abs() < 1e-4, "{turned} {yaw}");

    // Without mouse look a click leaves the cursor free, and an offset
    // places the first-person camera.
    {
        let world = app.world_mut();
        let mut controller = world.get_mut::<PlayerController>(player).unwrap();
        controller.mouse_look = false;
        controller.camera_offset = [0.5, 0.2, 0.0];
        world
            .resource_mut::<RuntimeInput>()
            .record_mouse_button(MouseButton::Left, true);
    }
    look(&mut app, [0.0; 2]);
    assert!(!app.world().resource::<RuntimeInput>().cursor_captured());
    assert_eq!(
        app.world().get::<Transform>(camera).unwrap().position,
        [0.5, 0.2, 0.0]
    );
}

#[test]
fn third_person_camera_orbits_behind_the_body() {
    let mut app = App::new();
    let player = app.spawn((
        Transform::default(),
        PlayerController {
            camera_distance: 4.0,
            camera_height: 1.0,
            pitch: -0.5,
            ..PlayerController::default()
        },
    ));
    let camera = app.spawn((Transform::default(), Camera::default()));
    app.set_parent(camera, player).unwrap();
    app.update(Duration::ZERO).unwrap();
    // Looking down, the camera rises above the orbit center and stays
    // behind the body (+Z, since the body faces -Z).
    let camera = *app.world().get::<Transform>(camera).unwrap();
    let [x, y, z] = camera.position;
    assert_eq!(x, 0.0);
    assert!((y - (1.0 + 4.0 * 0.5_f32.sin())).abs() < 1e-4, "{y}");
    assert!((z - 4.0 * 0.5_f32.cos()).abs() < 1e-4, "{z}");
    assert_eq!(camera.rotation, [-0.5, 0.0, 0.0]);
}

#[test]
fn third_person_camera_stops_in_front_of_a_wall_behind_the_body() {
    let mut app = App::new();
    // A wall 2 m behind the body; its near face is at z = 1.5.
    cpu_body(
        app.world_mut(),
        [0.0, 1.0, 2.0],
        ColliderShape::Box {
            half_extents: [5.0, 3.0, 0.5],
        },
        RigidBodyKind::Fixed,
    );
    // The player's own capsule must not block its camera.
    let player = cpu_body(
        app.world_mut(),
        [0.0, 1.0, 0.0],
        DEFAULT_PLAYER_SHAPE,
        RigidBodyKind::Kinematic,
    );
    app.world_mut().entity_mut(player).insert(PlayerController {
        camera_distance: 4.0,
        camera_height: 0.5,
        ..PlayerController::default()
    });
    let camera = app.spawn((Transform::default(), Camera::default()));
    app.set_parent(camera, player).unwrap();
    run_fixed_steps(&mut app, 1);
    app.update(Duration::ZERO).unwrap();
    let z = app.world().get::<Transform>(camera).unwrap().position[2];
    assert!(z > 1.0 && z < 1.31, "{z}");
}

#[test]
fn over_the_shoulder_camera_stops_at_walls_beside_and_behind_the_shoulder() {
    // yaw 0 faces -Z with the right shoulder at +X; a quarter turn left
    // puts the right shoulder at -Z and behind at +X.
    for (yaw, pillar, wall) in [
        (0.0, [0.6, 1.5, 2.5], [1.0, 1.5, 0.0]),
        (
            std::f32::consts::FRAC_PI_2,
            [2.5, 1.5, -0.6],
            [0.0, 1.5, -1.0],
        ),
    ] {
        let camera_at = |obstacle: [f32; 3], half_extents: [f32; 3]| {
            let mut app = App::new();
            cpu_body(
                app.world_mut(),
                obstacle,
                ColliderShape::Box { half_extents },
                RigidBodyKind::Fixed,
            );
            let player = cpu_body(
                app.world_mut(),
                [0.0, 1.0, 0.0],
                DEFAULT_PLAYER_SHAPE,
                RigidBodyKind::Kinematic,
            );
            app.world_mut().entity_mut(player).insert(PlayerController {
                camera_distance: 4.0,
                camera_height: 0.5,
                camera_offset: [0.6, 0.0, 0.0],
                yaw,
                ..PlayerController::default()
            });
            let camera = app.spawn((Transform::default(), Camera::default()));
            app.set_parent(camera, player).unwrap();
            run_fixed_steps(&mut app, 1);
            app.update(Duration::ZERO).unwrap();
            app.world().get::<Transform>(camera).unwrap().position
        };
        // A pillar behind the shoulder but not behind the body: the camera
        // stops in front of its near face, 2.2 m back.
        let [x, _, z] = camera_at(pillar, [0.3, 3.0, 0.3]);
        assert!((x - 0.6).abs() < 1e-4, "{x}");
        assert!(z > 1.8 && z < 2.2, "{z}");
        // A post whose near face is 0.7 m to the right pulls the shoulder
        // in before the camera goes back, so the camera never ends up in it.
        let [x, _, z] = camera_at(wall, [0.3, 3.0, 0.3]);
        assert!(x > 0.4 && x < 0.55, "{x}");
        assert!((z - 4.0).abs() < 1e-3, "{z}");
    }
}

#[test]
fn player_controller_settings_round_trip_through_scene_registry() {
    let mut app = App::new();
    let player = app.spawn((
        Transform::default(),
        PlayerController {
            walk_speed: 7.0,
            yaw: 1.0,
            vertical_speed: -3.0,
            ..PlayerController::default()
        },
    ));
    let values = registered_component_values(app.world(), player).unwrap();
    let (_, json) = values
        .iter()
        .find(|(name, _)| name == PLAYER_CONTROLLER_COMPONENT)
        .unwrap();
    let loaded: PlayerController = serde_json::from_str(json).unwrap();
    assert_eq!(loaded.walk_speed, 7.0);
    assert_eq!(loaded.yaw, 1.0);
    // Live motion state is not saved.
    assert_eq!(loaded.vertical_speed, 0.0);
}

#[cfg(feature = "ui")]
#[test]
fn runtime_ui_systems_draw_and_receive_clicks_each_update() {
    #[derive(Resource, Default)]
    struct Hud {
        button: Option<egui::Rect>,
        clicks: u32,
    }
    fn hud(ui: Res<RuntimeUi>, mut hud: ResMut<Hud>) {
        egui::Area::new("hud".into()).show(ui.context(), |ui| {
            ui.label("Score 3");
            let response = ui.button("Restart");
            hud.button = Some(response.rect);
            hud.clicks += u32::from(response.clicked());
        });
    }
    let mut app = App::new();
    app.world_mut().init_resource::<Hud>();
    app.add_system(ScheduleStage::Update, hud);
    let frame = Duration::from_millis(16);

    // egui lays new areas out invisibly on their first pass.
    app.update(frame).unwrap();
    app.update(frame).unwrap();
    let output = app.world_mut().resource_mut::<RuntimeUi>().take_output();
    let output = output.expect("every update finishes a UI pass");
    assert!(!output.shapes.is_empty());
    assert!(!output.textures_delta.set.is_empty(), "font atlas upload");
    let context = app.world().resource::<RuntimeUi>().context().clone();
    assert!(!context
        .tessellate(output.shapes, output.pixels_per_point)
        .is_empty());

    // A press and release on the button reach the system as one click.
    let center = app.world().resource::<Hud>().button.unwrap().center();
    let button = |pressed| egui::Event::PointerButton {
        pos: center,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: egui::Modifiers::NONE,
    };
    app.world_mut()
        .resource_mut::<RuntimeUi>()
        .set_input(egui::RawInput {
            events: vec![egui::Event::PointerMoved(center), button(true)],
            ..Default::default()
        });
    app.update(frame).unwrap();
    app.world_mut()
        .resource_mut::<RuntimeUi>()
        .set_input(egui::RawInput {
            events: vec![button(false)],
            ..Default::default()
        });
    app.update(frame).unwrap();
    assert_eq!(app.world().resource::<Hud>().clicks, 1);
    // Input is used once; the next pass has none.
    app.update(frame).unwrap();
    assert_eq!(app.world().resource::<Hud>().clicks, 1);
}

#[test]
fn easing_curves_start_at_zero_and_end_at_one() {
    for easing in [
        Easing::Linear,
        Easing::QuadIn,
        Easing::QuadOut,
        Easing::QuadInOut,
        Easing::CubicOut,
        Easing::SineInOut,
        Easing::BackOut,
        Easing::BounceOut,
    ] {
        assert!(easing.apply(0.0).abs() < 1e-5, "{easing:?}");
        assert!((easing.apply(1.0) - 1.0).abs() < 1e-5, "{easing:?}");
    }
    assert!(Easing::QuadIn.apply(0.5) < 0.5);
    assert!(Easing::QuadOut.apply(0.5) > 0.5);
    assert!(Easing::BackOut.apply(0.8) > 1.0);
}

#[test]
fn animations_run_on_the_fixed_step_and_send_marker_events() {
    let mut app = App::new();
    let key = |time, value: &[f32]| Keyframe {
        time,
        value: value.to_vec(),
    };
    let clip = AnimationClip {
        name: "open".into(),
        repeat: TweenRepeat::Once,
        tracks: vec![
            AnimationTrack {
                property: AnimationProperty::Position,
                keys: vec![
                    key(0.0, &[0.0, 0.0, 0.0]),
                    key(1.0, &[2.0, 0.0, 0.0]),
                ],
                ..AnimationTrack::default()
            },
            AnimationTrack {
                property: AnimationProperty::Field {
                    component: TWEEN_COMPONENT.into(),
                    path: "/duration".into(),
                },
                keys: vec![key(0.0, &[1.0]), key(1.0, &[3.0])],
                ..AnimationTrack::default()
            },
        ],
        events: vec![AnimationMarker {
            time: 1.0,
            name: "opened".into(),
        }],
        ..AnimationClip::default()
    };
    let door = app.spawn((
        Transform::default(),
        Name("Door".into()),
        // Writes scale, so it does not fight the position track.
        Tween {
            property: TweenProperty::Scale,
            ..Tween::default()
        },
        Animation {
            clips: vec![clip],
            autoplay: "open".into(),
            ..Animation::default()
        },
    ));
    let mut seen = Vec::new();
    for _ in 0..90 {
        run_fixed_steps(&mut app, 1);
        let events = app.world().resource::<EventQueue<AnimationEvent>>();
        seen.extend(events.iter().map(|e| (e.object.clone(), e.name.clone())));
    }
    assert_eq!(seen, [("Door".to_owned(), "opened".to_owned())]);
    let world = app.world();
    assert_eq!(world.get::<Transform>(door).unwrap().position[0], 2.0);
    assert_eq!(world.get::<Tween>(door).unwrap().duration, 3.0);
    assert!(!world.get::<AnimationPlayer>(door).unwrap().playing);
}

#[test]
fn state_machines_follow_timed_and_counter_guarded_transitions() {
    let mut app = App::new();
    let edge = |from: &str, to: &str| StateTransition {
        from: from.into(),
        to: to.into(),
        ..StateTransition::default()
    };
    let guard = app.spawn(ObjectState {
        state: "idle".into(),
        transitions: vec![
            StateTransition {
                after_seconds: 0.5,
                ..edge("idle", "patrol")
            },
            StateTransition {
                counter: "alarm".into(),
                at_least: 1,
                then_counter: "chases".into(),
                then_add: 2,
                ..edge("", "chase")
            },
        ],
        ..ObjectState::default()
    });
    let alarm = app.spawn(Counter {
        name: "alarm".into(),
        value: 0,
        target: None,
    });
    let state = |app: &App| {
        app.world().get::<ObjectState>(guard).unwrap().state.clone()
    };
    // Fixed ticks count from 0, so tick 30 (0.5 s) is the 31st step.
    run_fixed_steps(&mut app, 30);
    assert_eq!(state(&app), "idle");
    run_fixed_steps(&mut app, 1);
    assert_eq!(state(&app), "patrol");
    app.world_mut().get_mut::<Counter>(alarm).unwrap().value = 1;
    run_fixed_steps(&mut app, 1);
    assert_eq!(state(&app), "chase");
    let chases = app
        .world_mut()
        .query::<&Counter>()
        .iter(app.world())
        .find(|counter| counter.name == "chases")
        .map(|counter| counter.value);
    assert_eq!(chases, Some(2));
    // `to` equal to the current state never re-enters it.
    let since = app.world().get::<ObjectState>(guard).unwrap().since_tick;
    run_fixed_steps(&mut app, 5);
    assert_eq!(
        app.world().get::<ObjectState>(guard).unwrap().since_tick,
        since
    );
}

#[test]
fn state_transitions_run_exit_then_enter_actions() {
    let mut app = App::new();
    let action = |state: &str, exit, add| StateAction {
        state: state.into(),
        exit,
        counter: "log".into(),
        add,
    };
    let lamp = app.spawn(ObjectState {
        state: "off".into(),
        transitions: vec![
            StateTransition {
                from: "off".into(),
                to: "on".into(),
                ..StateTransition::default()
            },
            StateTransition {
                from: "on".into(),
                to: "off".into(),
                after_seconds: 1.0,
                ..StateTransition::default()
            },
        ],
        actions: vec![
            action("off", true, 1),
            action("on", false, 10),
            action("on", true, 100),
        ],
        ..ObjectState::default()
    });
    let log = |app: &mut App| {
        let mut counters = app.world_mut().query::<&Counter>();
        counters
            .iter(app.world())
            .find(|counter| counter.name == "log")
            .map(|counter| counter.value)
    };
    run_fixed_steps(&mut app, 1);
    assert_eq!(log(&mut app), Some(11));
    run_fixed_steps(&mut app, 60);
    // Off again on tick 60, 1 s later: leaving on adds 100.
    assert_eq!(app.world().get::<ObjectState>(lamp).unwrap().state, "off");
    assert_eq!(log(&mut app), Some(111));
}

#[test]
fn state_transitions_can_wait_for_a_contact() {
    let mut app = App::new();
    let world = app.world_mut();
    let plate = cpu_body(world, [0.0; 3], UNIT_BOX, RigidBodyKind::Fixed);
    world.get_mut::<Collider>(plate).unwrap().sensor = true;
    world.entity_mut(plate).insert(ObjectState {
        state: "up".into(),
        transitions: vec![StateTransition {
            to: "down".into(),
            touching: "Crate".into(),
            ..StateTransition::default()
        }],
        ..ObjectState::default()
    });
    let wall =
        cpu_body(world, [0.5, 0.0, 0.0], UNIT_BOX, RigidBodyKind::Kinematic);
    world.entity_mut(wall).insert(Name("Wall".into()));
    let state = |app: &App| {
        app.world().get::<ObjectState>(plate).unwrap().state.clone()
    };
    run_fixed_steps(&mut app, 3);
    assert_eq!(state(&app), "up");
    let crate_ = cpu_body(
        app.world_mut(),
        [-0.5, 0.0, 0.0],
        UNIT_BOX,
        RigidBodyKind::Kinematic,
    );
    app.world_mut()
        .entity_mut(crate_)
        .insert(Name("Crate".into()));
    run_fixed_steps(&mut app, 2);
    assert_eq!(state(&app), "down");
}

#[test]
fn state_transitions_can_wait_for_a_held_input_action() {
    let mut app = App::new();
    let door = app.spawn(ObjectState {
        state: "shut".into(),
        transitions: vec![StateTransition {
            to: "open".into(),
            held: PLAYER_JUMP.into(),
            ..StateTransition::default()
        }],
        ..ObjectState::default()
    });
    let state =
        |app: &App| app.world().get::<ObjectState>(door).unwrap().state.clone();
    run_fixed_steps(&mut app, 3);
    assert_eq!(state(&app), "shut");
    app.world_mut()
        .resource_mut::<RuntimeInput>()
        .record_key(KeyCode::Space, true);
    run_fixed_steps(&mut app, 1);
    assert_eq!(state(&app), "open");
}

#[test]
fn tweens_play_once_loop_and_ping_pong_on_the_fixed_step() {
    let mut app = App::new();
    let tween = |repeat| Tween {
        from: [0.0; 3],
        to: [2.0, 0.0, 0.0],
        duration: 1.0,
        delay: 0.5,
        easing: Easing::Linear,
        repeat,
        ..Tween::default()
    };
    let once = app.spawn((Transform::default(), tween(TweenRepeat::Once)));
    let looped = app.spawn((Transform::default(), tween(TweenRepeat::Loop)));
    let ping = app.spawn((Transform::default(), tween(TweenRepeat::PingPong)));
    let scaled = app.spawn((
        Transform::default(),
        Tween {
            property: TweenProperty::Scale,
            from: [1.0; 3],
            to: [3.0; 3],
            repeat: TweenRepeat::Once,
            ..tween(TweenRepeat::Once)
        },
    ));
    let x = |app: &App, entity| {
        app.world().get::<Transform>(entity).unwrap().position[0]
    };
    // Delay holds `from`.
    run_fixed_steps(&mut app, 30);
    assert!(x(&app, once).abs() < 1e-4);
    // Halfway through the first play.
    run_fixed_steps(&mut app, 30);
    assert!((x(&app, once) - 1.0).abs() < 1e-3, "{}", x(&app, once));
    // 1.25 s into playing: Once holds `to`, Loop restarted, PingPong returns.
    run_fixed_steps(&mut app, 45);
    assert!((x(&app, once) - 2.0).abs() < 1e-4);
    assert!(app.world().get::<Tween>(once).unwrap().finished());
    assert!((x(&app, looped) - 0.5).abs() < 1e-3, "{}", x(&app, looped));
    assert!((x(&app, ping) - 1.5).abs() < 1e-3, "{}", x(&app, ping));
    let scale = app.world().get::<Transform>(scaled).unwrap().scale;
    assert!((scale[1] - 3.0).abs() < 1e-4, "{scale:?}");
}

#[test]
fn squash_flattens_keeps_volume_and_springs_back_to_rest() {
    let mut app = App::new();
    let rest = [2.0, 2.0, 2.0];
    let body = app.spawn((
        Transform {
            scale: rest,
            ..Transform::default()
        },
        Squash::default(),
    ));
    run_fixed_steps(&mut app, 2);
    assert_eq!(app.world().get::<Transform>(body).unwrap().scale, rest);
    app.world_mut().get_mut::<Squash>(body).unwrap().squash(0.4);
    run_fixed_steps(&mut app, 1);
    let scale = app.world().get::<Transform>(body).unwrap().scale;
    assert!(scale[1] < 1.4 && scale[0] > 2.0, "{scale:?}");
    let volume = scale[0] * scale[1] * scale[2];
    assert!((volume - 8.0).abs() < 1e-3, "{volume}");
    // It overshoots into a stretch before settling.
    let mut tallest = 0.0f32;
    for _ in 0..30 {
        run_fixed_steps(&mut app, 1);
        tallest =
            tallest.max(app.world().get::<Transform>(body).unwrap().scale[1]);
    }
    assert!(tallest > 2.0, "{tallest}");
    run_fixed_steps(&mut app, 180);
    assert_eq!(app.world().get::<Transform>(body).unwrap().scale, rest);
    assert_eq!(app.world().get::<Squash>(body).unwrap().rest, None);
}

#[test]
fn harder_landings_play_louder() {
    let landing = |height: f32, full_volume_speed: f32| {
        let mut app = App::new();
        cpu_ground(app.world_mut());
        let body = cpu_body(
            app.world_mut(),
            [0.0, height, 0.0],
            UNIT_BOX,
            RigidBodyKind::Dynamic,
        );
        app.world_mut().entity_mut(body).insert(SoundCue {
            clip: "sounds/land.ogg".into(),
            full_volume_speed,
            ..SoundCue::default()
        });
        for _ in 0..240 {
            run_fixed_steps(&mut app, 1);
            let sounds = app.world().resource::<EventQueue<SoundEvent>>();
            if let Some(sound) = sounds.iter().next() {
                return sound.volume;
            }
        }
        panic!("no landing sound from {height} m");
    };
    let soft = landing(1.0, 20.0);
    let hard = landing(6.0, 20.0);
    assert!(soft > 0.0 && soft < hard && hard < 1.0, "{soft} {hard}");
    // A low full-volume speed caps at the cue's volume; 0 ignores speed.
    assert_eq!(landing(6.0, 0.5), 1.0);
    assert_eq!(landing(1.0, 0.0), 1.0);
}

#[test]
fn a_gpu_physics_event_plays_the_cue_that_names_it() {
    let mut app = App::new();
    app.add_plugin(HybridPhysicsPlugin).unwrap();
    let cue = |event: &str| SoundCue {
        clip: format!("sounds/{event}.ogg"),
        on_collision: false,
        on_gpu_event: event.into(),
        ..SoundCue::default()
    };
    let bodies = [cue("shatter"), cue("splash")].map(|cue| {
        let entity = app.spawn((PhysicsBody::default(), cue));
        app.world_mut()
            .resource_mut::<PhysicsIdRegistry>()
            .assign(entity)
    });
    let mut registry = app.world_mut().resource_mut::<GpuEventRegistry>();
    let [shatter, _splash] =
        ["shatter", "splash"].map(|name| registry.register(name));
    // Only the first body gets the event its cue names.
    let raw = |body: PhysicsId| RawGpuPhysicsEvent {
        body_slot: body.slot,
        body_generation: body.generation,
        event_id: shatter.0,
        tick_low: 1,
        ..Default::default()
    };
    route_gpu_physics_events(
        app.world_mut(),
        &[raw(bodies[0]), raw(bodies[1])],
    );

    let mut played = Vec::new();
    for _ in 0..4 {
        run_fixed_steps(&mut app, 1);
        let sounds = app.world().resource::<EventQueue<SoundEvent>>();
        played.extend(sounds.iter().map(|sound| sound.clip.clone()));
    }
    assert_eq!(played, ["sounds/shatter.ogg"]);
}

#[test]
fn a_sliding_crate_loops_its_scrape_until_friction_stops_it() {
    let mut app = App::new();
    cpu_ground(app.world_mut());
    let crate_body = cpu_body(
        app.world_mut(),
        [0.0, 0.5, 0.0],
        UNIT_BOX,
        RigidBodyKind::Dynamic,
    );
    app.world_mut().entity_mut(crate_body).insert(SlideSound {
        clip: "sounds/scrape.ogg".into(),
        ..SlideSound::default()
    });
    // Settle, then shove it sideways.
    run_fixed_steps(&mut app, 30);
    route_sound_events(app.world_mut());
    assert!(app.world().resource::<AudioQueue>().playing().is_empty());
    app.world_mut()
        .get_mut::<RigidBody>(crate_body)
        .unwrap()
        .linear_velocity = [6.0, 0.0, 0.0];
    let mut volumes = Vec::new();
    let mut stopped = false;
    for _ in 0..300 {
        run_fixed_steps(&mut app, 1);
        route_sound_events(app.world_mut());
        let mut queue = app.world_mut().resource_mut::<AudioQueue>();
        for command in queue.drain() {
            match command {
                AudioCommand::Play { sound, .. } => {
                    assert!(sound.looped);
                    assert_eq!(sound.follow, Some(crate_body));
                    volumes.push(sound.volume);
                }
                AudioCommand::SetVolume { volume, .. } => volumes.push(volume),
                AudioCommand::Stop(_) => stopped = true,
                _ => {}
            }
        }
        if stopped {
            break;
        }
    }
    assert!(stopped, "the scrape never stopped: {volumes:?}");
    assert_eq!(volumes.first(), Some(&1.0), "{volumes:?}");
    // Friction slows it, so the scrape fades.
    assert!(
        volumes.windows(2).all(|pair| pair[1] <= pair[0]),
        "{volumes:?}"
    );
    assert!(volumes.len() > 2, "{volumes:?}");
    assert!(app.world().resource::<AudioQueue>().playing().is_empty());
}

#[test]
fn landing_fires_one_sound_and_a_seeded_burst_that_expires() {
    let run = || {
        let mut app = App::new();
        cpu_ground(app.world_mut());
        let crate_body = cpu_body(
            app.world_mut(),
            [0.0, 1.0, 0.0],
            UNIT_BOX,
            RigidBodyKind::Dynamic,
        );
        app.world_mut().entity_mut(crate_body).insert((
            SceneId(uuid::Uuid::from_u128(7)),
            SoundCue {
                clip: "sounds/land.ogg".into(),
                ..SoundCue::default()
            },
            BurstEmitter {
                count: 5,
                lifetime: 0.25,
                ..BurstEmitter::default()
            },
        ));
        let mut sounds = Vec::new();
        let mut first_burst = None;
        for _ in 0..90 {
            run_fixed_steps(&mut app, 1);
            sounds.extend(
                app.world()
                    .resource::<EventQueue<SoundEvent>>()
                    .iter()
                    .cloned(),
            );
            let world = app.world_mut();
            let mut particles = world.query::<(&BurstParticle, &Transform)>();
            if first_burst.is_none() && particles.iter(world).len() > 0 {
                let mut positions: Vec<_> = particles
                    .iter(world)
                    .map(|(particle, _)| particle.velocity)
                    .collect();
                positions.sort_by(|a, b| a.partial_cmp(b).unwrap());
                first_burst = Some(positions);
            }
        }
        let left = app
            .world_mut()
            .query::<&BurstParticle>()
            .iter(app.world())
            .count();
        (app, crate_body, sounds, first_burst.unwrap(), left)
    };
    let (mut app, crate_body, sounds, burst, left) = run();
    assert_eq!(sounds.len(), 1, "{sounds:?}");
    assert_eq!(sounds[0].entity, crate_body);
    assert_eq!(sounds[0].clip, "sounds/land.ogg");
    assert_eq!(burst.len(), 5);
    for velocity in &burst {
        let speed = velocity.iter().map(|axis| axis * axis).sum::<f32>();
        assert!(speed.sqrt() <= 3.0 + 1e-4, "{velocity:?}");
    }
    assert_eq!(left, 0, "particles outlive their lifetime");
    // The same seed and tick give the same burst.
    assert_eq!(run().3, burst);

    // Game code can fire either one directly.
    app.world_mut()
        .get_mut::<SoundCue>(crate_body)
        .unwrap()
        .trigger();
    app.world_mut()
        .get_mut::<BurstEmitter>(crate_body)
        .unwrap()
        .trigger();
    let asked = app.world().resource::<AudioQueue>().requested();
    run_fixed_steps(&mut app, 2);
    assert_eq!(app.world().resource::<EventQueue<SoundEvent>>().len(), 1);
    // The runner turns the cue's event into exactly one play request.
    route_sound_events(app.world_mut());
    assert_eq!(app.world().resource::<AudioQueue>().requested(), asked + 1);
    assert_eq!(
        app.world_mut()
            .query::<&BurstParticle>()
            .iter(app.world())
            .count(),
        5
    );
}

#[cfg(feature = "ui")]
#[test]
fn hud_buttons_take_focus_in_reading_order() {
    let mut app = App::new();
    // Scene ids run opposite to the layout: Quit sorts first, Play last.
    let mut button = |id: u128, text: &str, y: f32| {
        let entity = app.spawn(HudElement {
            text: text.into(),
            anchor: HudAnchor::Center,
            offset: [0.0, y],
            button: true,
            ..HudElement::default()
        });
        app.world_mut()
            .entity_mut(entity)
            .insert(SceneId(uuid::Uuid::from_u128(id)));
        entity
    };
    button(1, "Quit", 60.0);
    button(2, "Options", 0.0);
    let play = button(3, "Play", -60.0);
    let screen =
        egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 600.0));
    let key = |key| egui::Event::Key {
        key,
        physical_key: Some(key),
        pressed: true,
        repeat: false,
        modifiers: egui::Modifiers::default(),
    };
    for events in [
        vec![],
        vec![],
        vec![key(egui::Key::Tab)],
        vec![],
        vec![key(egui::Key::Enter)],
        vec![],
    ] {
        app.world_mut()
            .resource_mut::<RuntimeUi>()
            .set_input(egui::RawInput {
                screen_rect: Some(screen),
                events,
                ..egui::RawInput::default()
            });
        app.update(Duration::from_millis(16)).unwrap();
    }
    let pressed: Vec<_> = app
        .world()
        .resource::<EventQueue<HudButtonPressed>>()
        .iter()
        .copied()
        .collect();
    assert_eq!(pressed, vec![HudButtonPressed { entity: play }]);
}

#[cfg(feature = "ui")]
#[test]
fn hud_draws_scene_text_and_reports_button_clicks() {
    let mut app = App::new();
    app.spawn(HudElement {
        text: "Score: 3".into(),
        ..HudElement::default()
    });
    let button = app.spawn(HudElement {
        text: "Restart".into(),
        anchor: HudAnchor::TopLeft,
        offset: [100.0, 100.0],
        button: true,
        ..HudElement::default()
    });
    let screen =
        egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 600.0));
    let frame = |app: &mut App, events: Vec<egui::Event>| {
        app.world_mut()
            .resource_mut::<RuntimeUi>()
            .set_input(egui::RawInput {
                screen_rect: Some(screen),
                events,
                ..egui::RawInput::default()
            });
        app.update(Duration::from_millis(16)).unwrap();
    };
    // egui sizes new areas invisibly on their first frame.
    frame(&mut app, Vec::new());
    frame(&mut app, Vec::new());
    let output = app
        .world_mut()
        .resource_mut::<RuntimeUi>()
        .take_output()
        .unwrap();
    assert!(!output.shapes.is_empty());

    let at = egui::pos2(110.0, 110.0);
    let press = |pressed| egui::Event::PointerButton {
        pos: at,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: egui::Modifiers::default(),
    };
    frame(&mut app, vec![egui::Event::PointerMoved(at), press(true)]);
    frame(&mut app, vec![press(false)]);
    // Events sent during a frame are visible on the next one.
    frame(&mut app, Vec::new());
    let pressed: Vec<_> = app
        .world()
        .resource::<EventQueue<HudButtonPressed>>()
        .iter()
        .copied()
        .collect();
    assert_eq!(pressed, vec![HudButtonPressed { entity: button }]);
}

#[cfg(feature = "ui")]
#[test]
fn text_scale_enlarges_hud_text() {
    let mut app = App::new();
    app.spawn(HudElement {
        text: "Score".into(),
        anchor: HudAnchor::TopLeft,
        offset: [0.0, 0.0],
        font_size: 20.0,
        ..HudElement::default()
    });
    let frame = |app: &mut App| {
        app.world_mut()
            .resource_mut::<RuntimeUi>()
            .set_input(egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(800.0, 600.0),
                )),
                ..egui::RawInput::default()
            });
        app.update(Duration::from_millis(16)).unwrap();
        app.world()
            .resource::<RuntimeUi>()
            .find_text("Score")
            .unwrap()
    };
    let [_, plain] = frame(&mut app);
    app.insert_resource(TextScale(2.0));
    let [_, doubled] = frame(&mut app);
    assert!(
        (doubled / plain - 2.0).abs() < 0.1,
        "the text's centre moves down with its height: {plain} then {doubled}"
    );
}

#[cfg(feature = "ui")]
#[test]
fn the_perf_overlay_draws_only_while_its_resource_exists() {
    let mut app = App::new();
    let frame = |app: &mut App| {
        app.world_mut()
            .resource_mut::<RuntimeUi>()
            .set_input(egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(800.0, 600.0),
                )),
                ..egui::RawInput::default()
            });
        app.update(Duration::from_millis(16)).unwrap();
    };
    let text = "60 fps  16.67 ms  p95 17.00 ms";
    app.insert_resource(PerfOverlay(text.into()));
    frame(&mut app);
    frame(&mut app);
    let [x, y] = app.world().resource::<RuntimeUi>().find_text(text).unwrap();
    assert!(x > 400.0 && y < 100.0, "top right, not at {x}, {y}");
    app.world_mut().remove_resource::<PerfOverlay>();
    frame(&mut app);
    assert!(app.world().resource::<RuntimeUi>().find_text(text).is_err());
}

#[cfg(feature = "ui")]
#[test]
fn a_hud_element_follows_an_object_on_screen() {
    let mut app = App::new();
    app.spawn((
        Name("Eye".into()),
        Camera {
            active: true,
            ..Camera::default()
        },
        Transform::new([0.0, 0.0, 10.0]),
    ));
    let enemy = app.spawn((Name("Enemy".into()), Transform::new([0.0; 3])));
    app.spawn(HudElement {
        text: "Grunt".into(),
        anchor: HudAnchor::Center,
        offset: [0.0, 0.0],
        follow: Some("Enemy".into()),
        follow_offset: [0.0, 1.0, 0.0],
        ..HudElement::default()
    });
    let tag = |app: &mut App| {
        app.world_mut()
            .resource_mut::<RuntimeUi>()
            .set_input(egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(800.0, 600.0),
                )),
                ..egui::RawInput::default()
            });
        app.update(Duration::from_millis(16)).unwrap();
        app.world().resource::<RuntimeUi>().find_text("Grunt")
    };
    // Straight ahead, lifted 1 m: centred across, above the middle.
    let [x, y] = tag(&mut app).unwrap();
    assert!((x - 400.0).abs() < 1.0 && y < 290.0, "{x} {y}");
    // It moves with the object.
    app.world_mut()
        .get_mut::<Transform>(enemy)
        .unwrap()
        .position = [3.0, 0.0, 0.0];
    let [right, _] = tag(&mut app).unwrap();
    assert!(right > x + 50.0, "{x} then {right}");
    // Behind the camera it hides.
    app.world_mut()
        .get_mut::<Transform>(enemy)
        .unwrap()
        .position = [0.0, 0.0, 20.0];
    assert!(tag(&mut app).is_err());
}

#[cfg(feature = "ui")]
#[test]
fn hud_elements_follow_their_camera_viewport() {
    let mut app = App::new();
    let camera = app.spawn((
        Name("Right".into()),
        Camera {
            viewport: Some([0.5, 0.0, 0.5, 1.0]),
            active: true,
            ..Camera::default()
        },
    ));
    app.spawn(HudElement {
        text: "P2".into(),
        anchor: HudAnchor::Center,
        offset: [0.0, 0.0],
        camera: Some("Right".into()),
        ..HudElement::default()
    });
    let center = |app: &mut App| {
        for _ in 0..2 {
            app.world_mut().resource_mut::<RuntimeUi>().set_input(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(800.0, 600.0),
                    )),
                    ..egui::RawInput::default()
                },
            );
            app.update(Duration::from_millis(16)).unwrap();
        }
        app.world().resource::<RuntimeUi>().find_text("P2").ok()
    };
    let [x, y] = center(&mut app).unwrap();
    assert!(
        (x - 600.0).abs() < 2.0 && (y - 300.0).abs() < 2.0,
        "{x} {y}"
    );
    app.world_mut().get_mut::<Camera>(camera).unwrap().active = false;
    assert_eq!(center(&mut app), None);
}

#[cfg(feature = "ui")]
#[test]
fn hidden_hud_elements_are_not_drawn() {
    let mut app = App::new();
    let parent = app.spawn(Visibility { visible: false });
    let text = app.spawn(HudElement {
        text: "Out of balls".into(),
        ..HudElement::default()
    });
    app.set_parent(text, parent).unwrap();
    let shapes = |app: &mut App| {
        // egui sizes new areas invisibly on their first frame.
        for _ in 0..2 {
            app.world_mut()
                .resource_mut::<RuntimeUi>()
                .set_input(egui::RawInput::default());
            app.update(Duration::from_millis(16)).unwrap();
        }
        app.world_mut()
            .resource_mut::<RuntimeUi>()
            .take_output()
            .unwrap()
            .shapes
            .len()
    };
    assert_eq!(shapes(&mut app), 0);
    app.world_mut()
        .get_mut::<Visibility>(parent)
        .unwrap()
        .visible = true;
    assert!(shapes(&mut app) > 0);
}

#[test]
fn tile_map_cells_are_found_by_position_and_painted_with_padding() {
    let mut map = TileMap {
        tile_size: 0.5,
        rows: vec!["#".into()],
        ..TileMap::default()
    };
    assert_eq!(map.cell_at([0.1, -0.1]), Some((0, 0)));
    assert_eq!(map.cell_at([1.2, -0.6]), Some((2, 1)));
    assert_eq!(map.cell_at([-0.1, -0.1]), None);
    assert_eq!(map.cell_at([0.1, 0.1]), None);
    assert!(map.set_cell(2, 1, '#'));
    assert_eq!(map.rows, vec!["#", "..#"]);
    assert!(!map.set_cell(2, 1, '#'));
    // Erasing outside the grid changes nothing.
    assert!(!map.set_cell(5, 4, '.'));
    assert_eq!(map.rows, vec!["#", "..#"]);
    assert!(map.set_cell(0, 0, '.'));
    assert_eq!(map.rows[0], ".");
}

#[test]
fn tile_map_rectangles_and_flood_fills_paint_regions() {
    let mut map = TileMap {
        rows: vec!["#.#".into(), "#..".into(), "###".into()],
        ..TileMap::default()
    };
    // The open cells join through (1, 1); the fill stops at the walls.
    assert!(map.fill(1, 0, 'o'));
    assert_eq!(map.rows, vec!["#o#", "#oo", "###"]);
    assert!(!map.fill(1, 0, 'o'));
    // Filling a wall repaints the joined wall cells only; the top right
    // one touches them by a corner, not an edge.
    assert!(map.fill(0, 0, 'x'));
    assert_eq!(map.rows, vec!["xo#", "xoo", "xxx"]);
    assert!(map.fill_rect((2, 3), (1, 2), '#'));
    assert_eq!(map.rows, vec!["xo#", "xoo", "x##", ".##"]);
    assert!(!map.fill_rect((1, 2), (2, 3), '#'));
    // A fill outside the used area does nothing rather than grow the grid.
    assert!(!map.fill(5, 1, 'x'));
    assert_eq!(map.rows.len(), 4);
    // A line is one cell per step along its longer side.
    assert_eq!(
        TileMap::line_cells((0, 0), (3, 1)),
        [(0, 0), (1, 0), (2, 1), (3, 1)]
    );
    assert_eq!(
        TileMap::line_cells((2, 2), (2, 0)),
        [(2, 2), (2, 1), (2, 0)]
    );
    assert!(map.fill_line((0, 3), (2, 3), 'o'));
    assert_eq!(map.rows[3], "ooo");
}

#[test]
fn tile_maps_spawn_merged_colliders_and_rebuild_or_clean_up() {
    let mut app = App::new();
    app.add_plugin(crate::assets::AssetPlugin).unwrap();
    let map = app.spawn((
        Transform::new([-2.0, 1.0, 0.0]),
        TileMap {
            tile_size: 0.5,
            rows: vec!["#..#".into(), "##x#".into()],
            tiles: std::collections::BTreeMap::from([
                ("#".into(), TileKind::default()),
                (
                    "x".into(),
                    TileKind {
                        solid: false,
                        ..TileKind::default()
                    },
                ),
            ]),
        },
    ));
    let parts = |app: &mut App| {
        let world = app.world_mut();
        let mut boxes: Vec<_> = world
            .query::<(&TileOf, &Collider, &Transform)>()
            .iter(world)
            .map(|(_, collider, transform)| {
                let ColliderShape::Box { half_extents } = collider.shape else {
                    panic!("tile colliders are boxes");
                };
                (transform.position, half_extents)
            })
            .collect();
        boxes.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let drawn = world
            .query::<(&TileOf, &MeshRenderer)>()
            .iter(world)
            .count();
        (drawn, boxes)
    };
    app.update(Duration::ZERO).unwrap();
    let (drawn, boxes) = parts(&mut app);
    assert_eq!(drawn, 6);
    // Row 0: two single tiles; row 1: "##" and "#", split by the non-solid x.
    assert_eq!(
        boxes,
        vec![
            ([-1.75, 0.75, 0.0], [0.25, 0.25, 0.25]),
            ([-1.5, 0.25, 0.0], [0.5, 0.25, 0.25]),
            ([-0.25, 0.25, 0.0], [0.25, 0.25, 0.25]),
            ([-0.25, 0.75, 0.0], [0.25, 0.25, 0.25]),
        ]
    );

    app.world_mut().get_mut::<TileMap>(map).unwrap().rows = vec!["#".into()];
    app.update(Duration::ZERO).unwrap();
    assert_eq!(parts(&mut app).0, 1);

    app.despawn(map).unwrap();
    app.update(Duration::ZERO).unwrap();
    let world = app.world_mut();
    assert_eq!(world.query::<&TileOf>().iter(world).count(), 0);
}

#[test]
fn platformer_runs_jumps_and_lands_on_tiles() {
    let mut app = App::new();
    app.add_plugin(crate::assets::AssetPlugin).unwrap();
    app.spawn((
        Transform::new([-5.0, 0.0, 0.0]),
        TileMap {
            rows: vec!["##########".into()],
            ..TileMap::default()
        },
    ));
    let player = app.spawn((
        Transform::new([-2.5, 1.0, 0.0]),
        PlatformerController::default(),
    ));
    let state = |app: &App| {
        (
            *app.world().get::<PlatformerController>(player).unwrap(),
            app.world().get::<Transform>(player).unwrap().position,
        )
    };
    run_fixed_steps(&mut app, 60);
    let (landed, start) = state(&app);
    assert!(landed.grounded);
    // Tile tops are at y = 0; the default capsule is 1 m tall.
    assert!((start[1] - 0.5).abs() < 0.02, "{start:?}");

    let mut input = app.world_mut().resource_mut::<RuntimeInput>();
    input.record_key(KeyCode::ArrowRight, true);
    input.record_key(KeyCode::Space, true);
    run_fixed_steps(&mut app, 1);
    app.world_mut()
        .resource_mut::<RuntimeInput>()
        .clear_frame_edges();
    run_fixed_steps(&mut app, 10);
    let (airborne, high) = state(&app);
    assert!(!airborne.grounded);
    assert!(high[1] > start[1] + 0.8, "{high:?}");
    assert!(high[0] > start[0] + 0.5, "{high:?}");
    assert_eq!(high[2], 0.0);
    run_fixed_steps(&mut app, 60);
    let (back, end) = state(&app);
    assert!(back.grounded);
    assert!((end[1] - 0.5).abs() < 0.02, "{end:?}");
}

#[test]
fn air_jumps_allow_that_many_jumps_before_landing() {
    let mut app = App::new();
    cpu_ground(app.world_mut());
    let world = app.world_mut();
    let runner = world
        .spawn((
            Transform::new([-3.0, 1.0, 0.0]),
            PlatformerController {
                air_jumps: 1,
                ..PlatformerController::default()
            },
        ))
        .id();
    let walker = world
        .spawn((
            Transform::new([3.0, 1.0, 0.0]),
            PlayerController {
                air_jumps: 1,
                ..PlayerController::default()
            },
        ))
        .id();
    // (vertical speed, air jumps used, grounded) of both bodies.
    let state = |app: &App| {
        let runner = app.world().get::<PlatformerController>(runner).unwrap();
        let walker = app.world().get::<PlayerController>(walker).unwrap();
        [
            (
                runner.vertical_speed,
                runner.air_jumps_used,
                runner.grounded,
            ),
            (
                walker.vertical_speed,
                walker.air_jumps_used,
                walker.grounded,
            ),
        ]
    };
    let press = |app: &mut App| {
        let mut input = app.world_mut().resource_mut::<RuntimeInput>();
        input.record_key(KeyCode::Space, false);
        input.clear_frame_edges();
        input.record_key(KeyCode::Space, true);
        run_fixed_steps(app, 1);
        app.world_mut()
            .resource_mut::<RuntimeInput>()
            .clear_frame_edges();
        // The press is read after this frame's fixed step; the next one
        // jumps.
        run_fixed_steps(app, 1);
    };
    run_fixed_steps(&mut app, 90);
    assert!(state(&app).iter().all(|body| body.2), "{:?}", state(&app));

    // Ground jump, then fall for a while.
    press(&mut app);
    run_fixed_steps(&mut app, 40);
    for (speed, used, grounded) in state(&app) {
        assert!(speed < 0.0 && !grounded && used == 0, "{:?}", state(&app));
    }
    // The air jump rises again and counts as used.
    press(&mut app);
    for (speed, used, _) in state(&app) {
        assert!(speed > 0.0 && used == 1, "{:?}", state(&app));
    }
    run_fixed_steps(&mut app, 40);
    // No air jumps left: a third press keeps falling.
    press(&mut app);
    for (speed, used, _) in state(&app) {
        assert!(speed < 0.0 && used == 1, "{:?}", state(&app));
    }
    run_fixed_steps(&mut app, 120);
    for (_, used, grounded) in state(&app) {
        assert!(grounded && used == 0, "{:?}", state(&app));
    }
}

#[test]
fn controllers_ride_moving_platforms_and_stop_at_ceilings() {
    let mut app = App::new();
    let world = app.world_mut();
    let platform = cpu_body(
        world,
        [0.0, -0.5, 0.0],
        ColliderShape::Box {
            half_extents: [2.0, 0.5, 2.0],
        },
        RigidBodyKind::Kinematic,
    );
    world.entity_mut(platform).insert(Tween {
        from: [0.0, -0.5, 0.0],
        to: [3.0, -0.5, 0.0],
        duration: 1.0,
        // Both bodies land before the platform starts.
        delay: 0.5,
        easing: Easing::Linear,
        repeat: TweenRepeat::Once,
        ..Tween::default()
    });
    let runner = world
        .spawn((
            Transform::new([0.0, 0.6, 0.0]),
            PlatformerController::default(),
        ))
        .id();
    let walker = world
        .spawn((Transform::new([0.0, 1.0, 1.0]), PlayerController::default()))
        .id();
    run_fixed_steps(&mut app, 120);
    for body in [runner, walker] {
        let x = app.world().get::<Transform>(body).unwrap().position[0];
        assert!((x - 3.0).abs() < 0.01, "rode to {x}");
    }

    // A jump into a low ceiling stops rising instead of sticking to it.
    let world = app.world_mut();
    cpu_body(world, [3.0, 1.6, 0.0], UNIT_BOX, RigidBodyKind::Fixed);
    world
        .get_mut::<PlatformerController>(runner)
        .unwrap()
        .vertical_speed = 8.0;
    run_fixed_steps(&mut app, 3);
    let runner = *app.world().get::<PlatformerController>(runner).unwrap();
    assert!(runner.vertical_speed < 0.0, "{runner:?}");
}

#[test]
fn a_turning_platform_carries_and_turns_its_rider() {
    let mut app = App::new();
    let platform = cpu_body(
        app.world_mut(),
        [0.0, -0.5, 0.0],
        ColliderShape::Box {
            half_extents: [3.0, 0.5, 3.0],
        },
        RigidBodyKind::Kinematic,
    );
    let walker = app
        .world_mut()
        .spawn((Transform::new([1.5, 1.0, 0.0]), PlayerController::default()))
        .id();
    run_fixed_steps(&mut app, 60);
    // A quarter turn around +Y in 90 steps.
    let steps = 90;
    for step in 1..=steps {
        app.world_mut()
            .get_mut::<Transform>(platform)
            .unwrap()
            .rotation[1] =
            std::f32::consts::FRAC_PI_2 * step as f32 / steps as f32;
        run_fixed_steps(&mut app, 1);
    }
    let at = app.world().get::<Transform>(walker).unwrap().position;
    assert!(at[0].abs() < 0.05 && (at[2] + 1.5).abs() < 0.05, "{at:?}");
    let yaw = app.world().get::<PlayerController>(walker).unwrap().yaw;
    assert!((yaw - std::f32::consts::FRAC_PI_2).abs() < 0.02, "{yaw}");
}

#[test]
fn swimming_players_float_at_the_surface_dive_and_rise() {
    use crate::runtime::WaterBody;
    let mut app = App::new();
    app.add_plugin(crate::assets::AssetPlugin).unwrap();
    app.world_mut().spawn((
        Transform::new([0.0, 0.0, 0.0]),
        WaterBody {
            wave_height: 0.0,
            ..WaterBody::default()
        },
    ));
    let player = app
        .world_mut()
        .spawn((
            Transform::new([0.0, -3.0, 0.0]),
            PlayerController {
                swim_speed: 2.0,
                ..PlayerController::default()
            },
        ))
        .id();
    let height =
        |app: &App| app.world().get::<Transform>(player).unwrap().position[1];
    run_fixed_steps(&mut app, 240);
    assert!(
        app.world()
            .get::<PlayerController>(player)
            .unwrap()
            .swimming
    );
    assert!((height(&app) + 0.5).abs() < 0.05, "{}", height(&app));

    app.world_mut()
        .resource_mut::<RuntimeInput>()
        .record_key(KeyCode::KeyC, true);
    run_fixed_steps(&mut app, 60);
    assert!(height(&app) < -2.0, "dived to {}", height(&app));

    let mut input = app.world_mut().resource_mut::<RuntimeInput>();
    input.record_key(KeyCode::KeyC, false);
    input.record_key(KeyCode::Space, true);
    input.clear_frame_edges();
    run_fixed_steps(&mut app, 120);
    assert!((height(&app) + 0.5).abs() < 0.05, "{}", height(&app));

    // Without swimming the same player sinks.
    app.world_mut()
        .get_mut::<PlayerController>(player)
        .unwrap()
        .swim_speed = 0.0;
    run_fixed_steps(&mut app, 60);
    assert!(height(&app) < -2.0, "sank to {}", height(&app));
}

#[test]
fn pickups_count_hide_and_unlock_in_scene_id_order() {
    let mut app = App::new();
    let world = app.world_mut();
    let player = cpu_body(world, [0.0; 3], UNIT_BOX, RigidBodyKind::Kinematic);
    world.entity_mut(player).insert(PlatformerController {
        gravity: 0.0,
        ..PlatformerController::default()
    });
    let pickup = |world: &mut bevy_ecs::world::World, id, pickup| {
        let entity =
            cpu_body(world, [0.5, 0.0, 0.0], UNIT_BOX, RigidBodyKind::Fixed);
        world.get_mut::<Collider>(entity).unwrap().sensor = true;
        world
            .entity_mut(entity)
            .insert((SceneId(uuid::Uuid::from_u128(id)), pickup));
        entity
    };
    // The goal sorts first, so on the first tick the coins are not yet
    // complete and it waits one more tick.
    let goal = pickup(
        world,
        1,
        Pickup {
            counter: "won".into(),
            requires: Some("coins".into()),
            ..Pickup::default()
        },
    );
    let coin = pickup(
        world,
        2,
        Pickup {
            counter: "coins".into(),
            ..Pickup::default()
        },
    );
    world.entity_mut(coin).insert(BurstEmitter {
        count: 3,
        on_collision: false,
        ..BurstEmitter::default()
    });
    pickup(
        world,
        3,
        Pickup {
            counter: "coins".into(),
            value: 2,
            ..Pickup::default()
        },
    );
    let counter = |world: &mut bevy_ecs::world::World, name: &str, target| {
        world
            .spawn(Counter {
                name: name.into(),
                value: 0,
                target: Some(target),
            })
            .id()
    };
    let coins = counter(world, "coins", 3);
    let won = counter(world, "won", 1);

    let mut ticks = 0;
    while !app.world().get::<Pickup>(coin).unwrap().collected {
        assert!(ticks < 10, "the coin was never collected");
        run_fixed_steps(&mut app, 1);
        ticks += 1;
    }
    let world = app.world();
    assert_eq!(world.get::<Counter>(coins).unwrap().value, 3);
    assert!(!world.get::<Pickup>(goal).unwrap().collected);
    assert!(!world.get::<Visibility>(coin).unwrap().visible);
    run_fixed_steps(&mut app, 2);
    let world = app.world_mut();
    assert!(world.get::<Pickup>(goal).unwrap().collected);
    assert_eq!(world.get::<Counter>(won).unwrap().value, 1);
    // Collected pickups stay collected while the player keeps touching.
    assert_eq!(world.get::<Counter>(coins).unwrap().value, 3);
    assert_eq!(world.query::<&BurstParticle>().iter(world).count(), 3);
}

#[test]
fn hud_text_fills_counter_placeholders() {
    let coins = Counter {
        name: "coins".into(),
        value: 4,
        target: Some(5),
    };
    let counters = [(&coins, None)];
    assert_eq!(
        hud_text(
            "Coins {coins}/5 {missing} {",
            counters.iter().copied(),
            None
        ),
        "Coins 4/5 {missing} {"
    );
    assert_eq!(
        hud_text("{tr:hud.coins}", counters.iter().copied(), None),
        "hud.coins"
    );
    let french = Translations {
        locale: "fr".into(),
        strings: [
            ("hud.coins", "Pièces {coins}/5"),
            ("coins.one", "{count} pièce"),
            ("coins.other", "{count} pièces"),
        ]
        .map(|(k, v)| (k.to_owned(), v.to_owned()))
        .into(),
    };
    assert_eq!(
        hud_text(
            "{tr:hud.coins} {tr:gone}",
            counters.iter().copied(),
            Some(&french)
        ),
        "Pièces 4/5 gone"
    );
    assert_eq!(french.plural("coins", 1), "1 pièce");
    assert_eq!(french.plural("coins", 0), "0 pièce");
    assert_eq!(french.plural("coins", 2), "2 pièces");
    assert_eq!(french.plural("lives", 2), "lives");
    let russian = Translations {
        locale: "ru-RU".into(),
        strings: [
            ("coins.one", "{count} монета"),
            ("coins.few", "{count} монеты"),
            ("coins.many", "{count} монет"),
            ("lives.other", "жизни: {count}"),
        ]
        .map(|(k, v)| (k.to_owned(), v.to_owned()))
        .into(),
    };
    let coins: Vec<_> = [1, 21, 11, 3, 22, 12, 5, 0]
        .map(|n| russian.plural("coins", n))
        .into();
    assert_eq!(
        coins,
        [
            "1 монета",
            "21 монета",
            "11 монет",
            "3 монеты",
            "22 монеты",
            "12 монет",
            "5 монет",
            "0 монет"
        ]
    );
    assert_eq!(
        russian.plural("lives", 1),
        "жизни: 1",
        "falls back to other"
    );
}

#[test]
fn dialogue_placeholders_show_the_line_and_its_choices() {
    let choice = |text: &str| DialogueChoice {
        text: text.into(),
        ..DialogueChoice::default()
    };
    let mut dialogue = Dialogue {
        lines: vec![
            DialogueLine {
                id: "hi".into(),
                speaker: "smith".into(),
                text: "hello".into(),
                choices: vec![choice("buy"), choice("bye")],
                ..DialogueLine::default()
            },
            DialogueLine {
                id: "sign".into(),
                text: "Keep out".into(),
                ..DialogueLine::default()
            },
        ],
        current: "hi".into(),
    };
    let german = Translations {
        locale: "de".into(),
        strings: [
            ("smith", "Schmied"),
            ("buy", "Kaufen"),
            ("dialogue.continue", "Weiter"),
        ]
        .map(|(k, v)| (k.to_owned(), v.to_owned()))
        .into(),
    };
    let name = Name("Smith".into());
    let fill = |dialogue: &Dialogue, placeholder: &str| {
        dialogue_placeholder(
            placeholder,
            [(&name, dialogue)].into_iter(),
            Some(&german),
        )
    };
    assert_eq!(
        fill(&dialogue, "dialogue:Smith").as_deref(),
        Some("Schmied: hello")
    );
    assert_eq!(
        fill(&dialogue, "dialogue:Smith/1").as_deref(),
        Some("Kaufen")
    );
    assert_eq!(fill(&dialogue, "dialogue:Smith/2").as_deref(), Some("bye"));
    assert_eq!(fill(&dialogue, "dialogue:Smith/3").as_deref(), Some(""));
    assert_eq!(fill(&dialogue, "dialogue:Smith/0").as_deref(), Some(""));
    assert_eq!(fill(&dialogue, "dialogue:Rock"), None);
    assert_eq!(fill(&dialogue, "coins"), None);
    dialogue.current = "sign".into();
    assert_eq!(
        fill(&dialogue, "dialogue:Smith").as_deref(),
        Some("Keep out")
    );
    assert_eq!(
        fill(&dialogue, "dialogue:Smith/1").as_deref(),
        Some("Weiter")
    );
    assert_eq!(fill(&dialogue, "dialogue:Smith/2").as_deref(), Some(""));
    dialogue.current.clear();
    assert_eq!(fill(&dialogue, "dialogue:Smith").as_deref(), Some(""));
}

#[test]
fn binding_placeholders_show_the_inputs_or_the_wait() {
    let jump = InputAction {
        action: "jump".into(),
        inputs: vec!["Space".into(), "KeyW".into()],
    };
    let show = |placeholder: &str, waiting: Option<&str>| {
        binding_placeholder(placeholder, [&jump].into_iter(), waiting, None)
    };
    assert_eq!(show("binding:jump", None).as_deref(), Some("Space / KeyW"));
    assert_eq!(show("binding:fire", None).as_deref(), Some(""));
    assert_eq!(
        show("binding:jump", Some("jump")).as_deref(),
        Some("Press a key")
    );
    assert_eq!(show("state:jump", None), None);
}

#[test]
fn state_placeholders_show_the_translated_state() {
    let quest = ObjectState {
        state: "quest.hammer".into(),
        ..ObjectState::default()
    };
    let name = Name("Quest".into());
    let english = Translations {
        locale: "en".into(),
        strings: [("quest.hammer", "Find the hammer")]
            .map(|(k, v)| (k.to_owned(), v.to_owned()))
            .into(),
    };
    let fill = |state: &ObjectState, placeholder: &str, t| {
        state_placeholder(placeholder, [(&name, state)].into_iter(), t)
    };
    assert_eq!(
        fill(&quest, "state:Quest", Some(&english)).as_deref(),
        Some("Find the hammer")
    );
    assert_eq!(
        fill(&quest, "state:Quest", None).as_deref(),
        Some("quest.hammer")
    );
    assert_eq!(fill(&quest, "state:Door", Some(&english)), None);
    assert_eq!(fill(&quest, "Quest", Some(&english)), None);
}

#[test]
fn shader_pragmas_declare_determinism() {
    let source =
        "#version 450\n  // rusting: determinism = Local\nvoid main() {}";
    assert_eq!(shader_determinism(source), Some(DeterminismMode::Local));
    assert_eq!(
        shader_determinism("// rusting: determinism = cross-platform"),
        Some(DeterminismMode::CrossPlatform)
    );
    assert_eq!(shader_determinism("void main() {}"), None);
    assert_eq!(shader_determinism("// rusting: determinism = fast"), None);
}

#[test]
fn determinism_check_names_every_part_below_the_project_mode() {
    use bevy_ecs::world::World;

    let mut world = World::new();
    world.spawn(PhysicsBody {
        simulation: SimulationClass::Cpu,
        ..PhysicsBody::default()
    });
    world.spawn(PhysicsBody {
        simulation: SimulationClass::Gpu,
        solver: PhysicsSolver::Custom,
        custom_shader: Some("missing/shader.comp".into()),
    });
    let mut support = DeterminismSupport::default();
    support.declare("game_ai", DeterminismMode::CrossPlatform);
    support.declare("game_ai", DeterminismMode::Local);
    world.insert_resource(support);

    world.insert_resource(DeterminismMode::Off);
    assert_eq!(check_determinism(&mut world), Ok(()));

    world.insert_resource(DeterminismMode::Local);
    let error = check_determinism(&mut world).unwrap_err();
    assert_eq!(
        error.offenders,
        vec![DeterminismOffender {
            part: "gpu_shader:missing/shader.comp".into(),
            supports: DeterminismMode::Off,
        }],
        "a custom shader without a pragma supports only Off"
    );

    world.insert_resource(DeterminismMode::CrossPlatform);
    let error = check_determinism(&mut world).unwrap_err();
    let parts: Vec<_> = error
        .offenders
        .iter()
        .map(|offender| offender.part.as_str())
        .collect();
    assert_eq!(
        parts,
        ["cpu_physics", "game_ai", "gpu_shader:missing/shader.comp"],
        "offenders are sorted, and a second declaration keeps the weaker mode"
    );
    assert!(error.to_string().contains("cpu_physics (supports Local)"));
}

#[test]
fn engine_fixed_update_systems_have_one_order() {
    use bevy_ecs::schedule::{LogLevel, ScheduleBuildSettings};
    let mut app = EngineBuilder::new().build().unwrap();
    app.fixed_update.set_build_settings(ScheduleBuildSettings {
        ambiguity_detection: LogLevel::Error,
        ..Default::default()
    });
    let App {
        world,
        fixed_update,
        ..
    } = &mut app;
    fixed_update.initialize(world).unwrap();
}

#[test]
fn simulation_bits_depend_on_ticks_not_frame_pacing() {
    let run = |frames: &[Duration]| {
        let mut app = App::new();
        cpu_ground(app.world_mut());
        for (index, height) in [1.0, 2.1, 3.2].into_iter().enumerate() {
            let body = cpu_body(
                app.world_mut(),
                [0.1 * index as f32, height, 0.0],
                UNIT_BOX,
                RigidBodyKind::Dynamic,
            );
            let mut rigid = app.world_mut().get_mut::<RigidBody>(body).unwrap();
            rigid.angular_velocity = [0.3, 0.0, 0.7];
        }
        let mut frame = 0;
        while app.world().resource::<FrameTime>().fixed_tick < 120 {
            app.update(frames[frame % frames.len()]).unwrap();
            frame += 1;
        }
        assert_eq!(app.world().resource::<FrameTime>().fixed_tick, 120);
        let world = app.world_mut();
        let mut bodies = world
            .query::<(bevy_ecs::entity::Entity, &Transform, &RigidBody)>()
            .iter(world)
            .map(|(entity, transform, rigid)| (entity, *transform, *rigid))
            .collect::<Vec<_>>();
        bodies.sort_by_key(|(entity, ..)| *entity);
        // Debug prints the shortest string that round-trips, so equal
        // strings mean equal bits.
        format!("{bodies:?}")
    };
    let tick = Duration::from_secs_f64(1.0 / 60.0);
    let half = tick / 2;
    let steady = run(&[tick]);
    let uneven = run(&[tick * 3, half, tick - half, Duration::ZERO, tick * 5]);
    assert_eq!(steady, uneven);
}

#[test]
fn fixed_steps_see_their_own_tick_and_stamp_gpu_commands_for_the_next() {
    #[derive(Resource, Default)]
    struct Seen(Vec<u64>);
    let id = PhysicsId {
        slot: 0,
        generation: 0,
    };
    let mut app = App::new();
    app.world_mut().init_resource::<Seen>();
    app.world_mut().init_resource::<GpuPhysicsCommands>();
    app.add_system(
        ScheduleStage::FixedUpdate,
        move |time: Res<FrameTime>,
              mut seen: ResMut<Seen>,
              mut commands: ResMut<GpuPhysicsCommands>| {
            seen.0.push(time.fixed_tick);
            commands.push(id, GpuBodyCommand::Impulse([1.0; 3]));
        },
    );
    app.add_system(
        ScheduleStage::Update,
        move |mut commands: ResMut<GpuPhysicsCommands>| {
            commands.push(id, GpuBodyCommand::ReadState);
        },
    );
    let tick = Duration::from_secs_f64(1.0 / 60.0);
    app.update(tick * 3).unwrap();
    assert_eq!(app.world().resource::<Seen>().0, [0, 1, 2]);
    assert_eq!(app.world().resource::<FrameTime>().fixed_tick, 3);
    // The command pushed during tick n applies before GPU tick n; the one
    // pushed in Update after tick 3 waits for tick 4.
    super::hybrid_physics::stamp_gpu_commands(app.world_mut());
    assert_eq!(
        app.world().resource::<GpuPhysicsCommands>().apply_ticks,
        [1, 2, 3, 4]
    );
}

#[test]
fn emitters_without_scene_ids_draw_their_own_bursts() {
    let mut app = App::new();
    for x in [0.0, 5.0] {
        let mut emitter = BurstEmitter {
            count: 3,
            ..BurstEmitter::default()
        };
        emitter.trigger();
        app.world_mut()
            .spawn((Transform::new([x, 0.0, 0.0]), emitter));
    }
    run_fixed_steps(&mut app, 1);
    let world = app.world_mut();
    let mut velocities = world
        .query::<&BurstParticle>()
        .iter(world)
        .map(|particle| format!("{:?}", particle.velocity))
        .collect::<Vec<_>>();
    assert_eq!(velocities.len(), 6);
    velocities.sort();
    velocities.dedup();
    assert_eq!(velocities.len(), 6, "two emitters shared a stream");
}

#[test]
fn an_emitter_with_a_rate_rains_streaks_over_its_area() {
    let mut app = App::new();
    let rain = BurstEmitter {
        rate: 120.0,
        area: [5.0, 0.0, 5.0],
        stretch: 8.0,
        speed: 0.0,
        on_collision: false,
        ..BurstEmitter::default()
    };
    app.world_mut()
        .spawn((Transform::new([0.0, 10.0, 0.0]), rain));
    let dt = app
        .world()
        .resource::<FrameTime>()
        .fixed_delta
        .as_secs_f32();
    run_fixed_steps(&mut app, 30);
    let world = app.world_mut();
    let drops: Vec<Transform> = world
        .query_filtered::<&Transform, With<BurstParticle>>()
        .iter(world)
        .copied()
        .collect();
    let expected = (120.0 * dt * 30.0).floor() as usize;
    assert!(drops.len().abs_diff(expected) <= 1, "{} drops", drops.len());
    assert!(drops.iter().all(|drop| {
        drop.position[0].abs() <= 5.0
            && drop.position[2].abs() <= 5.0
            && (drop.scale[1] - 8.0 * drop.scale[0]).abs() < 1e-4
    }));
    let spread = drops.iter().map(|drop| drop.position[0]);
    let (low, high) = spread.fold((f32::MAX, f32::MIN), |(low, high), x| {
        (low.min(x), high.max(x))
    });
    assert!(high - low > 5.0, "drops bunched in {low}..{high}");
}

#[test]
fn state_hashes_cover_every_tick_and_only_simulation_state() {
    #[derive(Resource)]
    struct Nudge(Option<u64>);
    let run = |frames: &[Duration], nudge: Option<u64>| {
        let mut app = App::new();
        cpu_ground(app.world_mut());
        for (index, height) in [1.0, 2.1, 3.2].into_iter().enumerate() {
            cpu_body(
                app.world_mut(),
                [0.1 * index as f32, height, 0.0],
                UNIT_BOX,
                RigidBodyKind::Dynamic,
            );
        }
        // A GPU body's CPU Transform is a late readback copy, so frame
        // timing may change it without changing the hash.
        app.world_mut().spawn((
            Transform::default(),
            PhysicsBody {
                simulation: SimulationClass::Gpu,
                ..PhysicsBody::default()
            },
        ));
        app.world_mut().insert_resource(Nudge(nudge));
        app.add_system(
            ScheduleStage::FixedUpdate,
            |time: Res<FrameTime>,
             nudge: Res<Nudge>,
             mut bodies: Query<&mut RigidBody>| {
                if nudge.0 == Some(time.fixed_tick) {
                    for mut rigid in &mut bodies {
                        if rigid.kind == RigidBodyKind::Dynamic {
                            let speed = rigid.linear_velocity[0];
                            rigid.linear_velocity[0] =
                                f32::from_bits(speed.to_bits() + 1);
                            break;
                        }
                    }
                }
            },
        );
        app.add_system(
            ScheduleStage::Update,
            |time: Res<FrameTime>,
             mut bodies: Query<(&mut Transform, &PhysicsBody)>| {
                for (mut transform, body) in &mut bodies {
                    if body.uses_gpu() {
                        transform.position[0] = time.frame as f32;
                    }
                }
            },
        );
        let mut frame = 0;
        while app.world().resource::<FrameTime>().fixed_tick < 120 {
            app.update(frames[frame % frames.len()]).unwrap();
            frame += 1;
        }
        let hashes = &app.world().resource::<StateHashes>().recent;
        hashes.iter().copied().take(120).collect::<Vec<_>>()
    };
    let tick = Duration::from_secs_f64(1.0 / 60.0);
    let steady = run(&[tick], None);
    assert_eq!(
        steady.iter().map(|(tick, _)| *tick).collect::<Vec<_>>(),
        (1..=120).collect::<Vec<_>>()
    );
    let uneven = run(&[tick * 3, tick / 2, tick / 2, Duration::ZERO], None);
    assert_eq!(steady, uneven);
    // One ULP of velocity during the step that ends at tick 41.
    let nudged = run(&[tick], Some(40));
    for ((tick, before), (_, after)) in steady.iter().zip(&nudged) {
        assert_eq!(before == after, *tick <= 40, "tick {tick}");
    }
}

#[test]
fn state_hash_ignores_resources_that_shift_entity_ids() {
    // Resources are entities: the windowed runtime inserting one more than
    // a headless replay must not change the hash of the same bodies.
    #[derive(Resource)]
    struct WindowOnly;
    let hashes = |extra: bool| {
        let mut app = App::new();
        app.add_plugin(crate::assets::AssetPlugin).unwrap();
        if extra {
            app.world_mut().insert_resource(WindowOnly);
        }
        cpu_ground(app.world_mut());
        // Lands and rests, so the solver's per-entity carry-over is hashed.
        cpu_body(
            app.world_mut(),
            [0.0, 0.6, 0.0],
            UNIT_BOX,
            RigidBodyKind::Dynamic,
        );
        // Tiles spawn during the first update, and the player's floor
        // names one of them.
        app.spawn((
            Transform::new([-5.0, 3.0, 0.0]),
            TileMap {
                rows: vec!["####".into()],
                ..TileMap::default()
            },
        ));
        let player = app.spawn((
            Transform::new([-3.5, 4.0, 0.0]),
            PlatformerController::default(),
        ));
        run_fixed_steps(&mut app, 60);
        let floor = app.world().get::<PlatformerController>(player).unwrap();
        assert!(floor.floor.is_some());
        app.world().resource::<StateHashes>().recent.clone()
    };
    assert_eq!(hashes(false), hashes(true));
}

#[test]
fn compare_runs_reports_the_first_divergent_tick_and_body() {
    #[derive(Resource)]
    struct Nudge(u64);
    let scene = |nudge: Option<u64>, seed: u64| {
        let mut app = App::new();
        app.world_mut().insert_resource(RandomSeed(seed));
        cpu_ground(app.world_mut());
        for (name, height) in [("Low", 1.0), ("Middle", 2.1), ("High", 3.2)] {
            let body = cpu_body(
                app.world_mut(),
                [0.0, height, 0.0],
                UNIT_BOX,
                RigidBodyKind::Dynamic,
            );
            app.world_mut().entity_mut(body).insert(Name(name.into()));
        }
        if let Some(tick) = nudge {
            app.world_mut().insert_resource(Nudge(tick));
            app.add_system(
                ScheduleStage::FixedUpdate,
                |time: Res<FrameTime>,
                 nudge: Res<Nudge>,
                 mut bodies: Query<(&Name, &mut RigidBody)>| {
                    for (name, mut rigid) in &mut bodies {
                        if time.fixed_tick == nudge.0 && name.0 == "Middle" {
                            rigid.linear_velocity[2] = 1e-6;
                        }
                    }
                },
            );
        }
        app
    };
    assert_eq!(compare_runs(|| scene(None, 7), 90).unwrap(), None);

    let mut runs = 0;
    let divergence = compare_runs(
        || {
            runs += 1;
            // Tick 5, before the boxes touch. Once they are stacked the
            // solver spreads the change to High within the same tick, and
            // High comes first in entity order.
            scene((runs == 2).then_some(5), 7)
        },
        90,
    )
    .unwrap()
    .unwrap();
    assert_eq!(divergence.tick, 6);
    let entity = divergence.entity.unwrap();
    assert_eq!(entity.name.as_deref(), Some("Middle"));

    let mut runs = 0;
    let divergence = compare_runs(
        || {
            runs += 1;
            scene(None, runs)
        },
        90,
    )
    .unwrap()
    .unwrap();
    assert_eq!((divergence.tick, divergence.entity), (1, None));

    let ticks = [(1, 10), (2, 20), (3, 30)];
    assert_eq!(first_divergent_tick(&ticks, &ticks), None);
    assert_eq!(first_divergent_tick(&ticks, &[(1, 10), (2, 21)]), Some(2));
    assert_eq!(first_divergent_tick(&ticks[..1], &ticks), Some(2));
}

#[test]
fn divergent_entities_pair_by_scene_id_across_scene_revisions() {
    let id = |n: u128| Some(uuid::Uuid::from_u128(n));
    let entry = |entity, name: &str, scene_id, hash| EntityStateHash {
        entity,
        name: Some(name.into()),
        scene_id,
        hash,
    };
    let old = [entry(1, "Floor", id(1), 10), entry(2, "Crate", id(2), 20)];
    // The new revision adds a lamp first, shifting every entity.
    let same = [
        entry(1, "Lamp", id(3), 30),
        entry(2, "Floor", id(1), 10),
        entry(3, "Crate", id(2), 20),
    ];
    let found = first_divergent_entity(&old, &same).unwrap();
    assert_eq!(found.name.as_deref(), Some("Lamp"), "only the new lamp");
    let moved = [entry(2, "Floor", id(1), 10), entry(3, "Crate", id(2), 21)];
    let found = first_divergent_entity(&old, &moved).unwrap();
    assert_eq!(found.name.as_deref(), Some("Crate"));
    assert!(first_divergent_entity(&old, &old).is_none());
}

#[test]
fn replays_reproduce_recorded_hashes_and_find_changed_input() {
    let scene = || {
        let mut app = App::new();
        app.add_plugin(crate::assets::AssetPlugin).unwrap();
        app.world_mut().insert_resource(RandomSeed(11));
        app.spawn((
            Transform::new([-5.0, 0.0, 0.0]),
            TileMap {
                rows: vec!["##########".into()],
                ..TileMap::default()
            },
        ));
        app.spawn((
            Transform::new([-2.5, 1.0, 0.0]),
            PlatformerController::default(),
        ));
        app
    };
    // Uneven frames, some with no fixed step and some with several, and
    // input edges cleared after each frame the way the window runner does.
    let mut app = scene();
    app.start_recording();
    for frame in 0..90u64 {
        let mut input = app.world_mut().resource_mut::<RuntimeInput>();
        match frame {
            20 => input.record_key(KeyCode::ArrowRight, true),
            30 | 55 => input.record_key(KeyCode::Space, true),
            31 | 56 => input.record_key(KeyCode::Space, false),
            70 => input.record_key(KeyCode::ArrowRight, false),
            _ => {}
        }
        let millis = [16, 33, 5, 50][frame as usize % 4];
        app.update(Duration::from_millis(millis)).unwrap();
        app.world_mut()
            .resource_mut::<RuntimeInput>()
            .clear_frame_edges();
    }
    let replay = app.finish_recording().unwrap();
    assert_eq!(replay.format_version, REPLAY_FORMAT_VERSION);
    assert_eq!((replay.seed, replay.start_tick), (11, 0));
    assert_eq!(replay.frames.len(), 90);
    let last_tick = app.world().resource::<FrameTime>().fixed_tick;
    assert_eq!(replay.hashes.len() as u64, last_tick);
    // Only frames whose input changed store it.
    let stored = replay.frames.iter().filter(|f| f.input.is_some()).count();
    assert!(stored < 20, "{stored}");

    let text = serde_json::to_string(&replay).unwrap();
    let replay: Replay = serde_json::from_str(&text).unwrap();
    assert_eq!(play_replay(&mut scene(), &replay).unwrap(), None);

    // Drop the second jump press. `platformer_jump` buffers presses in
    // Update, after the frame's fixed steps, so the change shows on the
    // first tick of the next frame. Replays cover Update-stage input too.
    let mut edited = replay.clone();
    let input = edited.frames[55].input.as_mut().unwrap();
    input.record_key(KeyCode::Space, false);
    input.clear_frame_edges();
    let expected = edited.frames[56].tick + 1;
    assert!(expected > edited.frames[55].tick + 1);
    assert_eq!(play_replay(&mut scene(), &edited).unwrap(), Some(expected));

    let mut old = replay;
    old.format_version = 0;
    assert!(matches!(
        play_replay(&mut scene(), &old),
        Err(ReplayError::UnsupportedVersion(0))
    ));
}

/// A tile map, a platformer, and an emitter whose particles keep spawning
/// and despawning, so entity indices are reused at rising generations.
fn churning_scene() -> (App, Entity) {
    let mut app = App::new();
    app.add_plugin(crate::assets::AssetPlugin).unwrap();
    app.world_mut().insert_resource(RandomSeed(5));
    app.spawn((
        Transform::new([-5.0, 0.0, 0.0]),
        TileMap {
            rows: vec!["##########".into()],
            ..TileMap::default()
        },
    ));
    app.spawn((
        Transform::new([-2.5, 1.0, 0.0]),
        PlatformerController::default(),
    ));
    let emitter = app.spawn((
        Transform::new([0.0, 3.0, 0.0]),
        BurstEmitter {
            count: 30,
            lifetime: 0.1,
            ..BurstEmitter::default()
        },
    ));
    (app, emitter)
}

fn churn_frame(app: &mut App, emitter: Entity, frame: u64) {
    if frame.is_multiple_of(5) {
        app.world_mut()
            .get_mut::<BurstEmitter>(emitter)
            .unwrap()
            .triggered = true;
    }
    let mut input = app.world_mut().resource_mut::<RuntimeInput>();
    match frame {
        10 => input.record_key(KeyCode::ArrowRight, true),
        25 | 70 => input.record_key(KeyCode::Space, true),
        26 | 71 => input.record_key(KeyCode::Space, false),
        _ => {}
    }
    let millis = [16, 33, 5, 50][frame as usize % 4];
    app.update(Duration::from_millis(millis)).unwrap();
    app.world_mut()
        .resource_mut::<RuntimeInput>()
        .clear_frame_edges();
}

fn entity_ids(snapshot: &WorldSnapshot) -> Vec<Entity> {
    snapshot.entities.iter().map(|saved| saved.entity).collect()
}

#[test]
fn snapshots_restore_entity_ids_and_state_into_a_new_app() {
    let (mut original, emitter) = churning_scene();
    // Frame 37 is mid-burst: particles from frame 35 are alive.
    for frame in 0..38 {
        churn_frame(&mut original, emitter, frame);
    }
    let snapshot = original.snapshot().unwrap();
    // Reused indices at raised generations, which the restore must match.
    assert!(
        entity_ids(&snapshot)
            .iter()
            .any(|entity| entity.generation().to_bits() > 0),
        "{:?} {:?}",
        entity_ids(&snapshot),
        snapshot.allocator
    );
    // Freed but not yet reusable, which bevy hides.
    assert!(
        !snapshot.allocator.local.is_empty(),
        "{:?}",
        snapshot.allocator
    );

    let (mut restored, _) = churning_scene();
    restored.restore(&snapshot).unwrap();
    let again = restored.snapshot().unwrap();
    assert_eq!(entity_ids(&again), entity_ids(&snapshot));
    assert_eq!(again.allocator, snapshot.allocator);

    for frame in 38..90 {
        churn_frame(&mut original, emitter, frame);
        churn_frame(&mut restored, emitter, frame);
    }
    let hashes =
        |app: &App| app.world().resource::<StateHashes>().recent.clone();
    assert!(hashes(&original).len() > 50);
    assert_eq!(hashes(&restored), hashes(&original));
    let (original, restored) =
        (original.snapshot().unwrap(), restored.snapshot().unwrap());
    assert_eq!(entity_ids(&restored), entity_ids(&original));
    assert_eq!(restored.allocator, original.allocator);
}

#[test]
fn snapshots_name_unregistered_types() {
    #[derive(bevy_ecs::component::Component, Clone)]
    struct Unlisted;

    let (mut app, _) = churning_scene();
    app.spawn(Unlisted);
    let Err(SnapshotError::Unregistered(types)) = app.snapshot() else {
        panic!("snapshot took an unregistered component");
    };
    assert_eq!(types.len(), 1);
    assert!(types[0].ends_with("Unlisted"), "{types:?}");
    app.register_snapshot_component::<Unlisted>();
    assert!(app.snapshot().is_ok());
}

/// A 0.2 m ball `offset` from a world pivot at [0, 3, 0], held by `kind`
/// with the ball-side anchor on the pivot and both frames turned by `frame`.
fn jointed_ball(
    app: &mut App,
    offset: [f32; 3],
    kind: JointKind,
    frame: [f32; 3],
) -> bevy_ecs::entity::Entity {
    let pivot = [0.0, 3.0, 0.0];
    let ball = cpu_body(
        app.world_mut(),
        [
            pivot[0] + offset[0],
            pivot[1] + offset[1],
            pivot[2] + offset[2],
        ],
        ColliderShape::Sphere { radius: 0.2 },
        RigidBodyKind::Dynamic,
    );
    app.world_mut().entity_mut(ball).insert(Joint {
        frame,
        target_frame: frame,
        ..Joint::new(
            kind,
            bevy_ecs::entity::Entity::PLACEHOLDER,
            offset.map(|value| -value),
            pivot,
        )
    });
    ball
}

/// Turns the joint X axis onto world +Z.
const X_TO_Z: [f32; 3] = [0.0, -std::f32::consts::FRAC_PI_2, 0.0];

fn from_pivot(app: &App, ball: bevy_ecs::entity::Entity) -> [f32; 3] {
    let position = app.world().get::<Transform>(ball).unwrap().position;
    [position[0], position[1] - 3.0, position[2]]
}

fn length(vector: [f32; 3]) -> f32 {
    vector.iter().map(|value| value * value).sum::<f32>().sqrt()
}

#[test]
fn hinge_pendulum_keeps_its_pivot_and_swings_in_its_plane() {
    let mut app = App::new();
    let hinge = JointKind::Hinge {
        limit: None,
        spring: None,
        motor: None,
    };
    let ball = jointed_ball(&mut app, [1.0, 0.0, 0.0], hinge, X_TO_Z);
    let mut lowest = 0.0_f32;
    for _ in 0..120 {
        run_fixed_steps(&mut app, 1);
        let offset = from_pivot(&app, ball);
        assert!((length(offset) - 1.0).abs() < 0.02, "arm {offset:?}");
        assert!(offset[2].abs() < 1e-3, "left its plane: {offset:?}");
        lowest = lowest.min(offset[1]);
    }
    assert!(lowest < -0.95, "never swung through the bottom: {lowest}");
    let spin = app.world().get::<RigidBody>(ball).unwrap().angular_velocity;
    assert!(spin[0].abs() < 1e-3 && spin[1].abs() < 1e-3, "{spin:?}");
}

#[test]
fn hinge_limits_stop_the_swing_and_motors_drive_it() {
    let mut app = App::new();
    let limited = JointKind::Hinge {
        limit: Some([-0.5, 0.5]),
        spring: None,
        motor: None,
    };
    let ball = jointed_ball(&mut app, [1.0, 0.0, 0.0], limited, X_TO_Z);
    for _ in 0..120 {
        run_fixed_steps(&mut app, 1);
        let offset = from_pivot(&app, ball);
        let angle = offset[1].atan2(offset[0]);
        assert!(angle > -0.55, "swung past the limit: {angle}");
    }

    let mut app = App::new();
    let motor = JointKind::Hinge {
        limit: None,
        spring: None,
        motor: Some(JointMotor {
            speed: 2.0,
            max_force: 100.0,
        }),
    };
    let ball = jointed_ball(&mut app, [1.0, 0.0, 0.0], motor, X_TO_Z);
    app.world_mut()
        .get_mut::<RigidBody>(ball)
        .unwrap()
        .gravity_scale = 0.0;
    run_fixed_steps(&mut app, 60);
    let spin = app.world().get::<RigidBody>(ball).unwrap().angular_velocity;
    assert!((spin[2] - 2.0).abs() < 0.05, "motor spins {spin:?}");
}

#[test]
fn sliders_move_along_their_axis_only() {
    let mut app = App::new();
    let slider = JointKind::Slider {
        limit: Some([-10.0, 1.5]),
        spring: None,
        motor: None,
    };
    let body = jointed_ball(&mut app, [0.0; 3], slider, [0.0; 3]);
    app.world_mut()
        .get_mut::<RigidBody>(body)
        .unwrap()
        .linear_velocity = [2.0, 1.0, 0.5];
    run_fixed_steps(&mut app, 60);
    let transform = *app.world().get::<Transform>(body).unwrap();
    let [x, y, z] = transform.position;
    assert!((x - 1.5).abs() < 0.03, "stops at its upper limit: {x}");
    assert!((y - 3.0).abs() < 0.02 && z.abs() < 0.02, "{:?}", [x, y, z]);
    assert!(transform.rotation.iter().all(|angle| angle.abs() < 1e-3));
}

#[test]
fn fixed_joints_hold_a_cantilever_in_place() {
    let mut app = App::new();
    let body = cpu_body(
        app.world_mut(),
        [1.0, 3.0, 0.0],
        UNIT_BOX,
        RigidBodyKind::Dynamic,
    );
    app.world_mut().entity_mut(body).insert(Joint::new(
        JointKind::Fixed,
        bevy_ecs::entity::Entity::PLACEHOLDER,
        [-1.0, 0.0, 0.0],
        [0.0, 3.0, 0.0],
    ));
    run_fixed_steps(&mut app, 120);
    let transform = *app.world().get::<Transform>(body).unwrap();
    assert!(
        (transform.position[1] - 3.0).abs() < 0.02,
        "sagged to {:?}",
        transform.position
    );
    assert!(
        transform.rotation[2].abs() < 0.02,
        "{:?}",
        transform.rotation
    );
}

#[test]
fn ropes_cap_the_distance_and_springs_settle_at_their_stretch() {
    let mut app = App::new();
    let rope = JointKind::Distance { min: 0.0, max: 1.0 };
    let ball = jointed_ball(&mut app, [0.5, 0.0, 0.0], rope, [0.0; 3]);
    // Anchors at the ball center and the pivot.
    app.world_mut().get_mut::<Joint>(ball).unwrap().anchor = [0.0; 3];
    for _ in 0..120 {
        run_fixed_steps(&mut app, 1);
        let arm = length(from_pivot(&app, ball));
        assert!(arm < 1.03, "rope stretched to {arm}");
    }

    let mut app = App::new();
    let spring = JointKind::Spring {
        rest_length: 1.0,
        stiffness: 100.0,
        damping: 5.0,
    };
    let ball = jointed_ball(&mut app, [0.0, -1.0, 0.0], spring, [0.0; 3]);
    app.world_mut().get_mut::<Joint>(ball).unwrap().anchor = [0.0; 3];
    let mut lowest = 0.0_f32;
    for _ in 0..300 {
        run_fixed_steps(&mut app, 1);
        lowest = lowest.min(from_pivot(&app, ball)[1]);
    }
    let mass = app.world().get::<RigidBody>(ball).unwrap().mass;
    let rest = -1.0 - mass * 9.81 / 100.0;
    let y = from_pivot(&app, ball)[1];
    assert!(lowest < rest - 0.02, "never bounced past rest: {lowest}");
    assert!((y - rest).abs() < 0.02, "settled at {y}, expected {rest}");
}

#[test]
fn cone_twist_joints_keep_the_swing_inside_the_cone() {
    let mut app = App::new();
    let cone = JointKind::ConeTwist {
        swing: 0.3,
        twist: [-0.1, 0.1],
    };
    let ball = jointed_ball(&mut app, [1.0, 0.0, 0.0], cone, [0.0; 3]);
    for _ in 0..120 {
        run_fixed_steps(&mut app, 1);
        let offset = from_pivot(&app, ball);
        let swing = (offset[1].hypot(offset[2]) / length(offset)).asin();
        assert!(swing < 0.35, "swung {swing} out of the cone");
    }
    let offset = from_pivot(&app, ball);
    assert!(offset[1] < -0.2, "hangs at the cone edge: {offset:?}");
}

#[test]
fn joints_link_bodies_without_contacts_and_round_trip_through_scenes() {
    let run = || {
        let mut app = App::new();
        let world = app.world_mut();
        cpu_ground(world);
        let upper =
            cpu_body(world, [0.0, 3.0, 0.0], UNIT_BOX, RigidBodyKind::Dynamic);
        let lower =
            cpu_body(world, [0.3, 2.2, 0.0], UNIT_BOX, RigidBodyKind::Dynamic);
        // Overlapping boxes would fly apart if they collided.
        world.entity_mut(lower).insert(Joint::new(
            JointKind::BallSocket,
            upper,
            [0.0, 0.4, 0.0],
            [0.3, -0.4, 0.0],
        ));
        run_fixed_steps(&mut app, 90);
        let world = app.world();
        assert!(!world
            .resource::<PhysicsWorld>()
            .contacts()
            .iter()
            .any(|contact| [contact.a, contact.b] == [upper, lower]
                || [contact.a, contact.b] == [lower, upper]));
        let position =
            |entity| world.get::<Transform>(entity).unwrap().position;
        [position(upper), position(lower)]
    };
    let first = run();
    assert_eq!(first, run(), "joints are not deterministic");
    assert!(first[1][1] < first[0][1], "{first:?}");

    let game = || {
        let mut app = App::new();
        app.add_plugin(crate::AssetPlugin).unwrap();
        app
    };
    let mut app = game();
    let target = app.spawn(Transform::default());
    let joint = Joint {
        frame: [0.1, 0.2, 0.3],
        collide_connected: true,
        ..Joint::new(
            JointKind::Generic {
                linear: [
                    JointAxis::LOCKED,
                    JointAxis::FREE,
                    JointAxis {
                        motion: AxisMotion::Limited {
                            min: -1.0,
                            max: 2.0,
                        },
                        spring: Some(JointSpring {
                            target: 0.5,
                            stiffness: 10.0,
                            damping: 1.0,
                        }),
                        motor: None,
                    },
                ],
                angular: [JointAxis::FREE; 3],
            },
            target,
            [1.0, 0.0, 0.0],
            [0.0, 2.0, 0.0],
        )
    };
    let body = app.spawn((Transform::default(), joint));
    let document = scene_document(app.world_mut(), "joints").unwrap();
    let mut loaded = game();
    load_scene_document(loaded.world_mut(), &document, SceneLoadMode::Replace)
        .unwrap();
    let id = |app: &App, entity| app.world().get::<SceneId>(entity).unwrap().0;
    let (body_id, target_id) = (id(&app, body), id(&app, target));
    let mut query = loaded.world_mut().query::<(Entity, &SceneId)>();
    let mut find = |wanted| {
        query
            .iter(loaded.world())
            .find(|(_, id)| id.0 == wanted)
            .unwrap()
            .0
    };
    let (body, target) = (find(body_id), find(target_id));
    assert_eq!(
        *loaded.world().get::<Joint>(body).unwrap(),
        Joint { target, ..joint }
    );
}

#[test]
fn the_kill_plane_despawns_fallen_bodies_and_sends_fell_out() {
    let mut app = App::new();
    let world = app.world_mut();
    cpu_ground(world);
    let resting =
        cpu_body(world, [0.0, 0.5, 0.0], UNIT_BOX, RigidBodyKind::Dynamic);
    let barrel =
        cpu_body(world, [20.0, 0.5, 0.0], UNIT_BOX, RigidBodyKind::Dynamic);
    world.entity_mut(barrel).insert(Name("Barrel".into()));
    let lid = world.spawn(Transform::default()).id();
    crate::runtime::hierarchy::set_parent(world, lid, barrel).unwrap();
    // Off by default: the barrel keeps falling.
    run_fixed_steps(&mut app, 120);
    assert!(app.world().get_entity(barrel).is_ok());
    app.world_mut().resource_mut::<PhysicsSettings>().kill_y = Some(-50.0);
    let mut fell = Vec::new();
    for _ in 0..240 {
        run_fixed_steps(&mut app, 1);
        fell.extend(
            app.world()
                .resource::<EventQueue<FellOut>>()
                .iter()
                .cloned(),
        );
    }
    assert_eq!(
        fell,
        [FellOut {
            entity: barrel,
            name: Some("Barrel".into())
        }]
    );
    let world = app.world();
    assert!(
        world.get_entity(barrel).is_err() && world.get_entity(lid).is_err()
    );
    assert!(world.get_entity(resting).is_ok());
    assert!(world
        .resource::<PhysicsWorld>()
        .contacts()
        .iter()
        .all(|contact| contact.a != barrel && contact.b != barrel));
}

#[test]
fn gravity_volumes_replace_the_scene_gravity_for_bodies_inside() {
    let mut app = App::new();
    let world = app.world_mut();
    let volume = |world: &mut bevy_ecs::world::World, at, shape, gravity| {
        let entity = cpu_body(world, at, shape, RigidBodyKind::Fixed);
        world.get_mut::<Collider>(entity).unwrap().sensor = true;
        world.entity_mut(entity).insert(gravity);
    };
    let ball = ColliderShape::Sphere { radius: 0.25 };
    // A zero-g room with a sideways wind tunnel of higher priority inside.
    volume(
        world,
        [0.0; 3],
        ColliderShape::Box {
            half_extents: [5.0; 3],
        },
        GravityVolume::default(),
    );
    volume(
        world,
        [3.0, 0.0, 0.0],
        ColliderShape::Box {
            half_extents: [1.0; 3],
        },
        GravityVolume {
            gravity: [5.0, 0.0, 0.0],
            priority: 1,
            ..GravityVolume::default()
        },
    );
    // A small planet pulling toward its centre.
    volume(
        world,
        [100.0, 0.0, 0.0],
        ColliderShape::Sphere { radius: 10.0 },
        GravityVolume {
            toward_center: 5.0,
            ..GravityVolume::default()
        },
    );
    let floating = cpu_body(world, [0.0; 3], ball, RigidBodyKind::Dynamic);
    let blown = cpu_body(world, [3.0, 0.0, 0.0], ball, RigidBodyKind::Dynamic);
    let above =
        cpu_body(world, [100.0, 5.0, 0.0], ball, RigidBodyKind::Dynamic);
    let below =
        cpu_body(world, [100.0, -5.0, 0.0], ball, RigidBodyKind::Dynamic);
    let outside =
        cpu_body(world, [50.0, 0.0, 0.0], ball, RigidBodyKind::Dynamic);
    run_fixed_steps(&mut app, 30);
    let world = app.world();
    let velocity =
        |entity| world.get::<RigidBody>(entity).unwrap().linear_velocity;
    let near = |a: [f32; 3], b: [f32; 3]| {
        a.iter().zip(b).all(|(a, b)| (a - b).abs() < 0.1)
    };
    assert!(
        near(velocity(floating), [0.0; 3]),
        "{:?}",
        velocity(floating)
    );
    assert!(
        near(velocity(blown), [2.5, 0.0, 0.0]),
        "{:?}",
        velocity(blown)
    );
    assert!(
        near(velocity(above), [0.0, -2.5, 0.0]),
        "{:?}",
        velocity(above)
    );
    assert!(
        near(velocity(below), [0.0, 2.5, 0.0]),
        "{:?}",
        velocity(below)
    );
    assert!(velocity(outside)[1] < -4.5, "{:?}", velocity(outside));
}

#[test]
fn force_fields_push_one_way_out_around_and_in_gusts_and_add_up() {
    let run = || {
        let mut app = App::new();
        app.add_force_field_function("spring", |sample| {
            [-sample.offset[0], 0.0, 0.0]
        });
        let world = app.world_mut();
        let sensor = |world: &mut bevy_ecs::world::World, at, half: f32| {
            let entity = cpu_body(
                world,
                at,
                ColliderShape::Box {
                    half_extents: [half; 3],
                },
                RigidBodyKind::Fixed,
            );
            world.get_mut::<Collider>(entity).unwrap().sensor = true;
            entity
        };
        // Zero g everywhere, so only the fields move the balls.
        let room = sensor(world, [40.0, 0.0, 0.0], 100.0);
        world.entity_mut(room).insert(GravityVolume::default());
        let field = |world: &mut bevy_ecs::world::World, at, field| {
            let entity = sensor(world, at, 3.0);
            world.entity_mut(entity).insert(field);
        };
        let up = ForceField {
            strength: 4.0,
            ..ForceField::default()
        };
        field(world, [0.0; 3], up.clone());
        field(
            world,
            [0.0; 3],
            ForceField {
                strength: 1.0,
                ..up.clone()
            },
        );
        let radial = ForceField {
            kind: FieldKind::Radial,
            strength: -4.0,
            ..up.clone()
        };
        field(world, [20.0, 0.0, 0.0], radial);
        let vortex = ForceField {
            kind: FieldKind::Vortex,
            ..up.clone()
        };
        field(world, [40.0, 0.0, 0.0], vortex);
        let faded = ForceField {
            falloff_distance: 4.0,
            ..up.clone()
        };
        field(world, [60.0, 0.0, 0.0], faded);
        let wind = ForceField {
            kind: FieldKind::Wind,
            direction: [1.0, 0.0, 0.0],
            turbulence: 0.5,
            ..up.clone()
        };
        field(world, [80.0, 0.0, 0.0], wind);
        let spring = ForceField {
            kind: FieldKind::Custom,
            strength: 1.0,
            function: "spring".to_owned(),
            ..up
        };
        field(world, [100.0, 0.0, 0.0], spring);
        let ball = ColliderShape::Sphere { radius: 0.25 };
        let balls = [
            [0.0; 3],
            [22.0, 0.0, 0.0],
            [42.0, 0.0, 0.0],
            [62.0, 0.0, 0.0],
            [80.0, 0.0, 0.0],
            [102.0, 0.0, 0.0],
        ]
        .map(|at| cpu_body(world, at, ball, RigidBodyKind::Dynamic));
        run_fixed_steps(&mut app, 30);
        balls.map(|ball| {
            app.world().get::<RigidBody>(ball).unwrap().linear_velocity
        })
    };
    let [summed, pulled, swirled, faded, blown, sprung] = run();
    let near = |a: [f32; 3], b: [f32; 3], within: f32| {
        a.iter().zip(b).all(|(a, b)| (a - b).abs() < within)
    };
    // Half a second of 4 + 1 m/s² up.
    assert!(near(summed, [0.0, 2.5, 0.0], 0.05), "{summed:?}");
    assert!(near(pulled, [-2.0, 0.0, 0.0], 0.05), "{pulled:?}");
    // Up × +x is -z: counterclockwise seen from above.
    assert!(near(swirled, [0.0, 0.0, -2.0], 0.3), "{swirled:?}");
    assert!(swirled[0] < 0.0, "turning toward the axis {swirled:?}");
    // 2 m out of a 4 m falloff: about half strength.
    assert!(near(faded, [0.0, 1.0, 0.0], 0.15), "{faded:?}");
    assert!(
        blown[0] > 1.0 && blown[0] < 3.0 && (blown[0] - 2.0).abs() > 0.05,
        "gusty, not steady {blown:?}"
    );
    // x'' = -x from x = 2: v = -2 sin(0.5 s).
    assert!(near(sprung, [-0.96, 0.0, 0.0], 0.05), "{sprung:?}");
    assert_eq!(run(), [summed, pulled, swirled, faded, blown, sprung]);
}

#[test]
fn a_box_pile_steps_the_same_on_one_and_many_workers() {
    let run = |workers| {
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(workers)
            .build()
            .unwrap();
        pool.install(|| {
            let mut app = App::new();
            let world = app.world_mut();
            cpu_ground(world);
            // 200 boxes in a loose 10 x 10 x 2 block fall into a heap.
            for i in 0..200 {
                let at = [
                    (i % 10) as f32 * 1.05 + 0.01 * (i / 100) as f32,
                    0.6 + (i / 100) as f32 * 1.1,
                    (i / 10 % 10) as f32 * 1.05,
                ];
                cpu_body(world, at, UNIT_BOX, RigidBodyKind::Dynamic);
            }
            // 30 three-box towers apart from the heap and each other: one
            // contact island each, solved on separate workers.
            for i in 0..90 {
                let at = [
                    -5.0 - (i / 3 % 6) as f32 * 3.0,
                    0.5 + (i % 3) as f32 * 1.02,
                    (i / 18) as f32 * 3.0,
                ];
                cpu_body(world, at, UNIT_BOX, RigidBodyKind::Dynamic);
            }
            run_fixed_steps(&mut app, 60);
            crate::runtime::state_hash::world_state_hash(app.world_mut())
        })
    };
    let one = run(1);
    assert_eq!(run(4), one);
    assert_eq!(run(7), one);
}

#[test]
fn bodies_rest_on_flat_heightfield_cells_and_roll_off_its_hills() {
    let mut app = App::new();
    let world = app.world_mut();
    // 8 x 8 m of ground with one 2 m peak in the middle.
    let mut heights = vec![vec![0.0; 5]; 5];
    heights[2][2] = 2.0;
    let field = Heightfield {
        heights,
        spacing: 2.0,
        ..Heightfield::default()
    };
    let mesh = field.mesh();
    assert_eq!((mesh.vertices.len(), mesh.indices.len()), (25, 96));
    assert!(mesh.vertices.iter().all(|vertex| vertex.normal[1] > 0.5));
    let ground = cpu_body(
        world,
        [0.0; 3],
        ColliderShape::Heightfield,
        RigidBodyKind::Fixed,
    );
    world.entity_mut(ground).insert(field);
    let ball = ColliderShape::Sphere { radius: 0.25 };
    let flat = cpu_body(world, [-3.0, 1.0, -3.0], ball, RigidBodyKind::Dynamic);
    let slope = cpu_body(world, [1.0, 2.0, 0.0], ball, RigidBodyKind::Dynamic);
    let at = |app: &App, entity| {
        app.world().get::<Transform>(entity).unwrap().position
    };
    run_fixed_steps(&mut app, 60);
    // Down the hill, still on the grid.
    let rolled = at(&app, slope);
    assert!(rolled[0] > 2.0 && rolled[1] > 0.0, "{rolled:?}");
    run_fixed_steps(&mut app, 120);
    let rest = at(&app, flat);
    assert!((rest[1] - 0.25).abs() < 0.05, "{rest:?}");
    assert!((rest[0] + 3.0).abs() < 0.05, "{rest:?}");
}

#[test]
fn heightfield_cells_with_their_own_surface_change_friction_and_sound() {
    let mut app = App::new();
    let world = app.world_mut();
    // Two 8 m cells: ice on the left, the collider's own ground on the
    // right (index 9 is past the end of surfaces).
    let ground = cpu_body(
        world,
        [0.0; 3],
        ColliderShape::Heightfield,
        RigidBodyKind::Fixed,
    );
    world.entity_mut(ground).insert(Heightfield {
        heights: vec![vec![0.0; 3]; 2],
        spacing: 8.0,
        cells: vec![vec![0, 9]],
        surfaces: vec![GroundSurface {
            name: "ice".into(),
            friction: 0.0,
            restitution: 0.0,
        }],
    });
    let slide = |app: &mut App, x: f32| {
        let world = app.world_mut();
        let crate_ = cpu_body(
            world,
            [x, 0.25, -3.0],
            ColliderShape::Box {
                half_extents: [0.25; 3],
            },
            RigidBodyKind::Dynamic,
        );
        world.get_mut::<RigidBody>(crate_).unwrap().linear_velocity =
            [0.0, 0.0, 3.0];
        world.entity_mut(crate_).insert(SoundCue {
            clip: "sfx/creak.wav".into(),
            with_material: "ice".into(),
            ..SoundCue::default()
        });
        crate_
    };
    let on_ice = slide(&mut app, -4.0);
    let on_ground = slide(&mut app, 4.0);
    run_fixed_steps(&mut app, 60);
    let world = app.world();
    let z = |entity| world.get::<Transform>(entity).unwrap().position[2];
    // Frictionless ice keeps 3 m/s for the second; friction 0.5 stops the
    // other crate in under a metre.
    assert!(z(on_ice) > -0.3, "ice {}", z(on_ice));
    assert!(z(on_ground) < -1.8, "ground {}", z(on_ground));
    assert!(world.get::<SoundCue>(on_ice).unwrap().touching);
    assert!(!world.get::<SoundCue>(on_ground).unwrap().touching);
}

#[test]
fn mesh_triangles_with_their_own_surface_change_friction_and_sound() {
    use crate::assets::{MeshAsset, MeshVertex};
    let mut app = App::new();
    app.add_plugin(crate::assets::AssetPlugin).unwrap();
    let (floor, material) = {
        let mut assets =
            app.world_mut().resource_mut::<crate::assets::AssetServer>();
        let floor = assets.meshes.insert(MeshAsset {
            vertices: [
                [-8.0, -8.0],
                [0.0, -8.0],
                [0.0, 8.0],
                [-8.0, 8.0],
                [8.0, -8.0],
                [8.0, 8.0],
            ]
            .map(|[x, z]| MeshVertex {
                position: [x, 0.0, z],
                ..MeshVertex::default()
            })
            .to_vec(),
            // Two left triangles, then two right ones.
            indices: vec![0, 2, 1, 0, 3, 2, 1, 5, 4, 1, 2, 5],
        });
        (floor, assets.fallback_material)
    };
    let world = app.world_mut();
    let ground = cpu_body(
        world,
        [0.0; 3],
        ColliderShape::TriangleMesh,
        RigidBodyKind::Fixed,
    );
    // Ice on the left half; the right triangles have no entry, so they
    // keep the collider's own friction.
    world.entity_mut(ground).insert((
        MeshRenderer {
            mesh: floor,
            material,
            cast_shadows: true,
            receive_shadows: true,
        },
        MeshSurfaces {
            triangles: vec![0, 0],
            surfaces: vec![GroundSurface {
                name: "ice".into(),
                friction: 0.0,
                restitution: 0.0,
            }],
        },
    ));
    let slide = |app: &mut App, x: f32| {
        let world = app.world_mut();
        let crate_ = cpu_body(
            world,
            [x, 0.25, -3.0],
            ColliderShape::Box {
                half_extents: [0.25; 3],
            },
            RigidBodyKind::Dynamic,
        );
        world.get_mut::<RigidBody>(crate_).unwrap().linear_velocity =
            [0.0, 0.0, 3.0];
        world.entity_mut(crate_).insert(SoundCue {
            clip: "sfx/creak.wav".into(),
            with_material: "ice".into(),
            ..SoundCue::default()
        });
        crate_
    };
    let on_ice = slide(&mut app, -4.0);
    let on_ground = slide(&mut app, 4.0);
    run_fixed_steps(&mut app, 60);
    let world = app.world();
    let z = |entity| world.get::<Transform>(entity).unwrap().position[2];
    assert!(z(on_ice) > -0.3, "ice {}", z(on_ice));
    assert!(z(on_ground) < -1.8, "ground {}", z(on_ground));
    assert!(world.get::<SoundCue>(on_ice).unwrap().touching);
    assert!(!world.get::<SoundCue>(on_ground).unwrap().touching);
}

#[test]
fn collider_children_move_with_their_dynamic_parent_as_one_body() {
    let mut app = App::new();
    let world = app.world_mut();
    cpu_body(
        world,
        [0.0, -0.5, 0.0],
        ColliderShape::Box {
            half_extents: [10.0, 0.5, 10.0],
        },
        RigidBodyKind::Fixed,
    );
    // A 2 x 2 m table top with four 1 m legs under its corners.
    let table = cpu_body(
        world,
        [0.0, 2.0, 0.0],
        ColliderShape::Box {
            half_extents: [1.0, 0.1, 1.0],
        },
        RigidBodyKind::Dynamic,
    );
    let leg = ColliderShape::Box {
        half_extents: [0.1, 0.5, 0.1],
    };
    let legs =
        [[0.8, 0.8], [0.8, -0.8], [-0.8, 0.8], [-0.8, -0.8]].map(|[x, z]| {
            let leg = cpu_body(
                app.world_mut(),
                [x, -0.6, z],
                leg,
                RigidBodyKind::Fixed,
            );
            app.set_parent(leg, table).unwrap();
            leg
        });
    run_fixed_steps(&mut app, 120);
    let world = app.world();
    let pose = world.get::<Transform>(table).unwrap();
    // Standing level on its legs: the leg bottoms 1.1 m below the top.
    assert!((pose.position[1] - 1.1).abs() < 0.03, "{:?}", pose.position);
    assert!(pose.rotation.iter().all(|angle| angle.abs() < 0.02));
    assert_eq!(world.get::<Transform>(legs[0]).unwrap().position[1], -0.6);
    // A ray at leg height reports the table.
    let hit = world
        .resource::<PhysicsWorld>()
        .raycast([5.0, 0.5, 0.8], [-1.0, 0.0, 0.0], 10.0, u32::MAX)
        .unwrap();
    assert_eq!(hit.entity, table);
    assert!((hit.distance - 4.1).abs() < 0.03, "{hit:?}");
}

#[test]
fn joints_break_past_their_force_or_torque_and_send_an_event() {
    // A ball hanging 1 m under the pivot, and a box held out 1 m from it.
    let hang = |break_force| {
        let mut app = App::new();
        let ball = jointed_ball(
            &mut app,
            [0.0, -1.0, 0.0],
            JointKind::BallSocket,
            [0.0; 3],
        );
        app.world_mut().get_mut::<Joint>(ball).unwrap().break_force =
            break_force;
        let weight = app.world().get::<RigidBody>(ball).unwrap().mass * 9.81;
        (app, ball, weight)
    };
    let (mut app, ball, weight) = hang(1.0);
    assert!(weight > 1.5, "{weight}");
    run_fixed_steps(&mut app, 1);
    // Events become readable in the frame after they are sent.
    app.update(Duration::ZERO).unwrap();
    let world = app.world();
    assert!(world.get::<Joint>(ball).is_none(), "joint held {weight} N");
    let events: Vec<_> = world
        .resource::<EventQueue<JointBroken>>()
        .iter()
        .copied()
        .collect();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].joint, ball);
    assert_eq!(events[0].target, bevy_ecs::entity::Entity::PLACEHOLDER);
    assert!(
        (events[0].force - weight).abs() < 0.1 * weight,
        "{events:?}"
    );
    run_fixed_steps(&mut app, 30);
    assert!(from_pivot(&app, ball)[1] < -1.5, "broken joint still holds");

    let (mut app, ball, weight) = hang(0.0);
    app.world_mut().get_mut::<Joint>(ball).unwrap().break_force = weight * 2.0;
    run_fixed_steps(&mut app, 60);
    assert!(app.world().get::<Joint>(ball).is_some());
    assert!((from_pivot(&app, ball)[1] + 1.0).abs() < 0.02);

    let cantilever = |break_torque| {
        let mut app = App::new();
        let body = cpu_body(
            app.world_mut(),
            [1.0, 3.0, 0.0],
            UNIT_BOX,
            RigidBodyKind::Dynamic,
        );
        let joint = Joint {
            break_torque,
            ..Joint::new(
                JointKind::Fixed,
                bevy_ecs::entity::Entity::PLACEHOLDER,
                [-1.0, 0.0, 0.0],
                [0.0, 3.0, 0.0],
            )
        };
        app.world_mut().entity_mut(body).insert(joint);
        let torque = app.world().get::<RigidBody>(body).unwrap().mass * 9.81;
        run_fixed_steps(&mut app, 10);
        (app.world().get::<Joint>(body).is_some(), torque)
    };
    let (_, torque) = cantilever(0.0);
    assert!(!cantilever(torque * 0.5).0, "held {torque} N·m");
    assert!(cantilever(torque * 2.0).0, "broke under {torque} N·m");
}

/// Hangs `links` spheres in a row along +X from a fixed articulated base at
/// [0, 3, 0], each hinged at the previous one's center 0.5 m away.
fn articulated_chain(
    app: &mut App,
    links: usize,
    kind: JointKind,
) -> Vec<bevy_ecs::entity::Entity> {
    let world = app.world_mut();
    let base = cpu_body(
        world,
        [0.0, 3.0, 0.0],
        ColliderShape::Sphere { radius: 0.1 },
        RigidBodyKind::Fixed,
    );
    world.entity_mut(base).insert(Articulation {});
    let mut chain = vec![base];
    for index in 1..=links {
        let link = cpu_body(
            world,
            [0.5 * index as f32, 3.0, 0.0],
            ColliderShape::Sphere { radius: 0.1 },
            RigidBodyKind::Dynamic,
        );
        world.entity_mut(link).insert(Joint {
            frame: X_TO_Z,
            target_frame: X_TO_Z,
            ..Joint::new(kind, chain[index - 1], [-0.5, 0.0, 0.0], [0.0; 3])
        });
        chain.push(link);
    }
    chain
}

fn distance(app: &App, a: bevy_ecs::entity::Entity, b: Entity) -> f32 {
    let position =
        |entity| app.world().get::<Transform>(entity).unwrap().position;
    let (a, b) = (position(a), position(b));
    length([a[0] - b[0], a[1] - b[1], a[2] - b[2]])
}

/// Potential and kinetic energy of `links`, treating each as a 0.1 m ball.
fn chain_energy(app: &App, links: &[Entity]) -> f32 {
    links
        .iter()
        .map(|link| {
            let height =
                app.world().get::<Transform>(*link).unwrap().position[1];
            let rigid = app.world().get::<RigidBody>(*link).unwrap();
            let (speed, spin) = (
                length(rigid.linear_velocity),
                length(rigid.angular_velocity),
            );
            rigid.mass
                * (9.81 * height + 0.5 * speed * speed + 0.002 * spin * spin)
        })
        .sum()
}

#[test]
fn articulated_chains_swing_without_their_joints_drifting() {
    for kind in [
        JointKind::BallSocket,
        JointKind::Hinge {
            limit: None,
            spring: None,
            motor: None,
        },
    ] {
        let mut app = App::new();
        let chain = articulated_chain(&mut app, 8, kind);
        // Self-contacts would add their push-out; this measures the joints.
        for link in &chain {
            app.world_mut().entity_mut(*link).insert(CollisionLayers {
                memberships: 0,
                filters: 0,
            });
        }
        let start = chain_energy(&app, &chain[1..]);
        let mut lowest = f32::INFINITY;
        for _ in 0..240 {
            run_fixed_steps(&mut app, 1);
            for pair in chain.windows(2) {
                let gap = distance(&app, pair[0], pair[1]);
                assert!(
                    (gap - 0.5).abs() < 1e-4,
                    "{kind:?} link drifted: {gap}"
                );
            }
            let energy = chain_energy(&app, &chain[1..]);
            // The tip's whip peaks near 5% over; a solver that gains
            // energy grows without bound instead.
            assert!(energy < start * 1.06, "{kind:?} gained energy: {energy}");
            let tip = app.world().get::<Transform>(chain[8]).unwrap().position;
            assert!(tip[2].abs() < 1e-3, "{kind:?} left its plane: {tip:?}");
            lowest = lowest.min(tip[1]);
        }
        assert!(lowest < -0.5, "{kind:?} chain did not fall: {lowest}");
    }
}

#[test]
fn articulated_hinge_limits_hold_exactly_and_motors_drive_them() {
    let limited = JointKind::Hinge {
        limit: Some([-0.5, 0.5]),
        spring: None,
        motor: None,
    };
    let mut app = App::new();
    let chain = articulated_chain(&mut app, 1, limited);
    for _ in 0..120 {
        run_fixed_steps(&mut app, 1);
        let offset = app.world().get::<Transform>(chain[1]).unwrap().position;
        let angle = (offset[1] - 3.0).atan2(offset[0]);
        assert!(angle >= -0.5 - 1e-4, "swung past the limit: {angle}");
    }

    let motor = JointKind::Hinge {
        limit: None,
        spring: None,
        motor: Some(JointMotor {
            speed: 2.0,
            max_force: 100.0,
        }),
    };
    let mut app = App::new();
    let chain = articulated_chain(&mut app, 1, motor);
    app.world_mut()
        .get_mut::<RigidBody>(chain[1])
        .unwrap()
        .gravity_scale = 0.0;
    run_fixed_steps(&mut app, 60);
    let spin = app
        .world()
        .get::<RigidBody>(chain[1])
        .unwrap()
        .angular_velocity;
    assert!((spin[2] - 2.0).abs() < 0.05, "motor spins {spin:?}");
}

#[test]
fn floating_articulations_land_rest_and_round_trip_through_scenes() {
    let run = || {
        let mut app = App::new();
        let world = app.world_mut();
        cpu_ground(world);
        let torso =
            cpu_body(world, [0.0, 2.0, 0.0], UNIT_BOX, RigidBodyKind::Dynamic);
        world.entity_mut(torso).insert(Articulation {});
        let mut parts = vec![torso];
        // Each ball turns about its own center, held 0.9 m from the torso's.
        for side in [-1.0_f32, 1.0] {
            let arm = cpu_body(
                world,
                [side * 0.9, 2.0, 0.0],
                ColliderShape::Sphere { radius: 0.3 },
                RigidBodyKind::Dynamic,
            );
            world.entity_mut(arm).insert(Joint::new(
                JointKind::BallSocket,
                torso,
                [0.0; 3],
                [side * 0.9, 0.0, 0.0],
            ));
            parts.push(arm);
        }
        run_fixed_steps(&mut app, 240);
        for arm in &parts[1..] {
            assert!((distance(&app, torso, *arm) - 0.9).abs() < 1e-4);
        }
        parts
            .iter()
            .map(|part| app.world().get::<Transform>(*part).unwrap().position)
            .collect::<Vec<_>>()
    };
    let first = run();
    assert_eq!(first, run(), "articulations are not deterministic");
    assert!(first.iter().all(|position| position[1] > 0.2), "{first:?}");
    assert!(first[0][1] < 0.7, "the torso did not land: {first:?}");

    let game = || {
        let mut app = App::new();
        app.add_plugin(crate::AssetPlugin).unwrap();
        app
    };
    let mut app = game();
    app.spawn((Transform::default(), Articulation {}));
    let document = scene_document(app.world_mut(), "articulation").unwrap();
    let mut loaded = game();
    load_scene_document(loaded.world_mut(), &document, SceneLoadMode::Replace)
        .unwrap();
    let mut query = loaded.world_mut().query::<&Articulation>();
    assert_eq!(query.iter(loaded.world()).count(), 1);
}

#[test]
fn atmosphere_settings_come_from_the_lowest_entity() {
    let mut app = App::new();
    app.add_plugin(RenderExtractPlugin).unwrap();
    app.update(Duration::ZERO).unwrap();
    let render_world = app.world().resource::<RenderWorld>();
    assert_eq!(render_world.fog, None);
    assert_eq!(render_world.bloom, None);
    assert_eq!(render_world.auto_exposure, None);
    assert_eq!(render_world.ambient_occlusion, None);
    let fog = Fog {
        density: 0.2,
        ..Fog::default()
    };
    app.spawn(fog);
    app.spawn(Fog::default());
    app.spawn(Bloom::default());
    app.spawn(AutoExposure::default());
    app.spawn(AmbientOcclusion::default());
    app.update(Duration::ZERO).unwrap();
    let render_world = app.world().resource::<RenderWorld>();
    assert_eq!(render_world.fog, Some(fog));
    assert_eq!(render_world.bloom, Some(Bloom::default()));
    assert_eq!(render_world.auto_exposure, Some(AutoExposure::default()));
    assert_eq!(
        render_world.ambient_occlusion,
        Some(AmbientOcclusion::default())
    );
}

#[test]
fn removing_an_unregistered_component_drops_its_kept_json() {
    let mut world = bevy_ecs::world::World::new();
    world.insert_resource(SceneComponentRegistry::default());
    let entity = world
        .spawn(UnregisteredComponents(
            [("game.spin".to_owned(), "{\"speed\":2}".to_owned())].into(),
        ))
        .id();

    remove_registered_component(&mut world, entity, "game.spin").unwrap();

    assert!(world
        .get::<UnregisteredComponents>(entity)
        .unwrap()
        .0
        .is_empty());
    assert!(
        remove_registered_component(&mut world, entity, "game.spin").is_err()
    );
}

#[test]
fn players_step_onto_low_ledges_but_not_high_ones_and_can_leave_crates_alone() {
    // Walks the player toward -Z into a box of `height` for two seconds and
    // returns where it ended and where a dynamic crate beside it ended.
    let walk = |height: f32, player: PlayerController| {
        let mut app = App::new();
        let world = app.world_mut();
        cpu_body(
            world,
            [0.0, -0.5, 0.0],
            ColliderShape::Box {
                half_extents: [10.0, 0.5, 10.0],
            },
            RigidBodyKind::Fixed,
        );
        cpu_body(
            world,
            [0.0, height / 2.0, -7.0],
            ColliderShape::Box {
                half_extents: [2.0, height / 2.0, 5.0],
            },
            RigidBodyKind::Fixed,
        );
        let crate_ = cpu_body(
            world,
            [0.5, 0.25, 0.0],
            ColliderShape::Box {
                half_extents: [0.25; 3],
            },
            RigidBodyKind::Dynamic,
        );
        let body = cpu_body(
            world,
            [0.0, 0.91, 0.0],
            DEFAULT_PLAYER_SHAPE,
            RigidBodyKind::Kinematic,
        );
        world.entity_mut(body).insert(player);
        run_fixed_steps(&mut app, 30);
        app.world_mut()
            .resource_mut::<RuntimeInput>()
            .record_key(KeyCode::KeyW, true);
        run_fixed_steps(&mut app, 120);
        let at =
            |entity| app.world().get::<Transform>(entity).unwrap().position;
        (at(body), at(crate_))
    };
    let (on_low, _) = walk(0.25, PlayerController::default());
    assert!(on_low[2] < -2.5 && on_low[1] > 1.1, "{on_low:?}");
    let (at_high, _) = walk(0.5, PlayerController::default());
    assert!(at_high[2] > -2.0 && at_high[1] < 0.95, "{at_high:?}");
    let flat = PlayerController {
        max_step_height: 0.0,
        ..PlayerController::default()
    };
    let (at_low, _) = walk(0.25, flat);
    assert!(at_low[2] > -2.0 && at_low[1] < 0.95, "{at_low:?}");

    // The crate starts overlapping the player, which walks into it (+X).
    let into_crate = |push_bodies| PlayerController {
        yaw: -std::f32::consts::FRAC_PI_2,
        push_bodies,
        ..PlayerController::default()
    };
    let (_, crate_pushed) = walk(0.25, into_crate(true));
    let (player, crate_kept) = walk(0.25, into_crate(false));
    assert!(crate_pushed[0] > 0.53, "{crate_pushed:?}");
    assert!((crate_kept[0] - 0.5).abs() < 1e-3, "{crate_kept:?}");
    assert!(player[0] < 0.1, "{player:?}");
}

#[test]
fn a_walking_player_keeps_pushing_a_crate_it_did_not_start_in() {
    let mut app = App::new();
    let world = app.world_mut();
    cpu_body(
        world,
        [0.0, -0.5, 0.0],
        ColliderShape::Box {
            half_extents: [20.0, 0.5, 20.0],
        },
        RigidBodyKind::Fixed,
    );
    let crate_ = cpu_body(
        world,
        [0.0, 0.5, -2.0],
        ColliderShape::Box {
            half_extents: [0.5; 3],
        },
        RigidBodyKind::Dynamic,
    );
    let body = cpu_body(
        world,
        [0.0, 0.91, 0.0],
        DEFAULT_PLAYER_SHAPE,
        RigidBodyKind::Kinematic,
    );
    world.entity_mut(body).insert(PlayerController::default());
    run_fixed_steps(&mut app, 30);
    app.world_mut()
        .resource_mut::<RuntimeInput>()
        .record_key(KeyCode::KeyW, true);
    run_fixed_steps(&mut app, 180);
    let at = |entity| app.world().get::<Transform>(entity).unwrap().position;
    // Three seconds at 4 m/s: the crate is shoved well past where the
    // player first met it, and the player follows behind it.
    assert!(at(crate_)[2] < -6.0, "crate at {:?}", at(crate_));
    assert!(
        at(body)[2] < at(crate_)[2] + 1.0,
        "player at {:?}",
        at(body)
    );
    assert!(
        (at(crate_)[1] - 0.5).abs() < 0.05,
        "crate at {:?}",
        at(crate_)
    );
    let player = app.world().get::<PlayerController>(body).unwrap();
    assert!(player.grounded);
    assert!(player.floor.is_some(), "the floor counts as touched");
    assert_eq!(player.wall, Some(crate_));
    assert!(
        (player.velocity[2] + 4.0).abs() < 0.5,
        "{:?}",
        player.velocity
    );
}

#[test]
fn a_player_teleported_into_a_platform_stands_on_top_of_it() {
    let mut app = App::new();
    let world = app.world_mut();
    cpu_body(
        world,
        [0.0, -0.5, 0.0],
        ColliderShape::Box {
            half_extents: [10.0, 0.5, 10.0],
        },
        RigidBodyKind::Fixed,
    );
    let lift = cpu_body(
        world,
        [0.0, 0.375, 0.0],
        ColliderShape::Box {
            half_extents: [1.0, 0.375, 1.0],
        },
        RigidBodyKind::Kinematic,
    );
    // The capsule bottom is 0.91 below its center: this sinks it 0.46 into
    // the lift's top at 0.75, as Lantern Keeper's scenario teleport did.
    let body = cpu_body(
        world,
        [0.0, 1.2, 0.0],
        DEFAULT_PLAYER_SHAPE,
        RigidBodyKind::Kinematic,
    );
    world.entity_mut(body).insert(PlayerController::default());
    run_fixed_steps(&mut app, 10);
    let y = app.world().get::<Transform>(body).unwrap().position[1];
    assert!((y - 1.66).abs() < 0.05, "player at y {y}");
    let player = app.world().get::<PlayerController>(body).unwrap();
    assert_eq!(player.floor.map(|floor| floor.0), Some(lift));
}

#[test]
fn a_player_with_a_turn_speed_faces_its_rig_where_it_walks() {
    let mut app = App::new();
    let world = app.world_mut();
    cpu_body(
        world,
        [0.0, -0.5, 0.0],
        ColliderShape::Box {
            half_extents: [10.0, 0.5, 10.0],
        },
        RigidBodyKind::Fixed,
    );
    let body = cpu_body(
        world,
        [0.0, 0.91, 0.0],
        DEFAULT_PLAYER_SHAPE,
        RigidBodyKind::Kinematic,
    );
    world.entity_mut(body).insert(PlayerController {
        turn_speed: std::f32::consts::TAU,
        ..PlayerController::default()
    });
    let rig = world.spawn(Transform::default()).id();
    super::hierarchy::set_parent(world, rig, body).unwrap();
    // Right is +X at yaw 0; a model facing -Z turns -90° about Y to face it.
    world
        .resource_mut::<RuntimeInput>()
        .record_key(KeyCode::KeyD, true);
    run_fixed_steps(&mut app, 30);
    let yaw = app.world().get::<Transform>(rig).unwrap().rotation[1];
    assert!(
        (yaw + std::f32::consts::FRAC_PI_2).abs() < 1e-3,
        "rig yaw {yaw}"
    );
    let body_yaw = app.world().get::<Transform>(body).unwrap().rotation[1];
    assert_eq!(body_yaw, 0.0);
}

#[test]
fn a_crate_dropped_on_a_player_that_pushes_nothing_rests_on_it() {
    let mut app = App::new();
    let world = app.world_mut();
    cpu_body(
        world,
        [0.0, -0.5, 0.0],
        ColliderShape::Box {
            half_extents: [10.0, 0.5, 10.0],
        },
        RigidBodyKind::Fixed,
    );
    let body = cpu_body(
        world,
        [0.0, 0.91, 0.0],
        DEFAULT_PLAYER_SHAPE,
        RigidBodyKind::Kinematic,
    );
    world.entity_mut(body).insert(PlayerController {
        push_bodies: false,
        ..PlayerController::default()
    });
    let crate_ = cpu_body(
        world,
        [0.0, 3.0, 0.0],
        ColliderShape::Box {
            half_extents: [0.25; 3],
        },
        RigidBodyKind::Dynamic,
    );
    run_fixed_steps(&mut app, 120);
    let y = |entity| app.world().get::<Transform>(entity).unwrap().position[1];
    assert!(y(crate_) > 1.8, "crate sank to {}", y(crate_));
    assert!((y(body) - 0.91).abs() < 0.05, "player moved to {}", y(body));
}

#[test]
fn fluid_volumes_step_with_the_fixed_tick() {
    let mut app = App::new();
    let settings = FluidSettings {
        bounds: ([-0.5, 0.0, -0.5], [0.5, 2.0, 0.5]),
        ..FluidSettings::default()
    };
    let fluid = Fluid::block([-0.2, 1.0, -0.2], [4, 4, 4], settings.spacing);
    let start = fluid.positions.clone();
    let entity = app
        .world_mut()
        .spawn(FluidVolume::new(settings, fluid))
        .id();
    run_fixed_steps(&mut app, 30);
    let volume = app.world().get::<FluidVolume>(entity).unwrap();
    assert_eq!(volume.fluid.positions.len(), start.len());
    let mean = |points: &[[f32; 3]]| {
        points.iter().map(|p| p[1]).sum::<f32>() / points.len() as f32
    };
    assert!(mean(&volume.fluid.positions) < mean(&start) - 0.3);
}

/// An 8 kg jelly cube of 0.2 m hanging under a 0.2 m box at `[0, 2, 0]`,
/// its top face attached to the box. Returns (box, jelly).
fn jelly_under_a_box(
    app: &mut App,
    kind: RigidBodyKind,
) -> (bevy_ecs::entity::Entity, bevy_ecs::entity::Entity) {
    let world = app.world_mut();
    let body = cpu_body(
        world,
        [0.0, 2.0, 0.0],
        ColliderShape::Box {
            half_extents: [0.1; 3],
        },
        kind,
    );
    let mut rigid = world.get_mut::<RigidBody>(body).unwrap();
    rigid.mass = 8.0;
    rigid.gravity_scale = 0.0;
    let settings = SoftBodySettings {
        floor: None,
        damping: 0.0,
        edge_compliance: 1e-5,
        ..SoftBodySettings::default()
    };
    let cube =
        SoftBody::block([-0.1, 1.7, -0.1], [2, 2, 2], 0.1, 1000.0).unwrap();
    let mut volume = SoftBodyVolume::new(settings, cube);
    let transform = *world.get::<Transform>(body).unwrap();
    assert_eq!(
        volume.attach_near([0.0, 2.0, 0.0], 0.18, body, &transform),
        9
    );
    (body, world.spawn(volume).id())
}

#[test]
fn a_hanging_soft_body_pulls_its_dynamic_body_down_by_its_weight() {
    let mut app = App::new();
    let (body, jelly) = jelly_under_a_box(&mut app, RigidBodyKind::Dynamic);
    run_fixed_steps(&mut app, 60);
    // Only the jelly feels gravity, so after one second the box and jelly
    // together carry the jelly's weight times one second of momentum.
    let world = app.world();
    let soft = &world.get::<SoftBodyVolume>(jelly).unwrap().body;
    let (mass, momentum) = soft
        .velocities
        .iter()
        .zip(&soft.inverse_masses)
        .fold((0.0, 0.0), |(mass, momentum), (velocity, weight)| {
            (mass + 1.0 / weight, momentum + velocity[1] / weight)
        });
    let rigid = world.get::<RigidBody>(body).unwrap();
    let total = momentum + rigid.mass * rigid.linear_velocity[1];
    let expected = -mass * 9.81;
    assert!(
        (total - expected).abs() < 0.05 * expected.abs(),
        "momentum {total}, expected {expected}"
    );
    let shared = expected / (mass + rigid.mass);
    assert!(
        rigid.linear_velocity[1] < 0.7 * shared,
        "the box fell at only {:?}, shared speed {shared}",
        rigid.linear_velocity
    );
    assert!(
        length(rigid.angular_velocity) < 0.5,
        "the box spins at {:?}",
        rigid.angular_velocity
    );
}

#[test]
fn a_soft_body_follows_a_moving_kinematic_body() {
    let mut app = App::new();
    let (body, jelly) = jelly_under_a_box(&mut app, RigidBodyKind::Kinematic);
    app.world_mut()
        .get_mut::<RigidBody>(body)
        .unwrap()
        .linear_velocity = [1.0, 0.0, 0.0];
    run_fixed_steps(&mut app, 60);
    let world = app.world();
    let origin = world.get::<Transform>(body).unwrap().position;
    assert!((origin[0] - 1.0).abs() < 0.05, "box at {origin:?}");
    let volume = world.get::<SoftBodyVolume>(jelly).unwrap();
    for attachment in &volume.attachments {
        let at = volume.body.positions[attachment.particle];
        for axis in 0..3 {
            let want = origin[axis] + attachment.local[axis];
            assert!((at[axis] - want).abs() < 0.03, "{at:?} not at {want}");
        }
    }
    let bottom = volume
        .body
        .positions
        .iter()
        .map(|p| p[1])
        .fold(f32::MAX, f32::min);
    let mean_x = volume.body.positions.iter().map(|p| p[0]).sum::<f32>()
        / volume.body.positions.len() as f32;
    assert!(bottom > 1.5, "the jelly tore loose: bottom at {bottom}");
    assert!(mean_x > 0.8, "the jelly stayed behind at x {mean_x}");
}

#[test]
fn a_dynamic_box_dropped_on_a_jelly_comes_to_rest_on_it() {
    let mut app = App::new();
    let world = app.world_mut();
    let crate_box = cpu_body(
        world,
        [0.0, 1.0, 0.0],
        ColliderShape::Box {
            half_extents: [0.15; 3],
        },
        RigidBodyKind::Dynamic,
    );
    world.get_mut::<RigidBody>(crate_box).unwrap().mass = 2.0;
    let settings = SoftBodySettings {
        edge_compliance: 1e-4,
        damping: 2.0,
        ..SoftBodySettings::default()
    };
    let pad =
        SoftBody::block([-0.3, 0.0, -0.3], [3, 2, 3], 0.2, 500.0).unwrap();
    world.spawn(SoftBodyVolume::new(settings, pad));
    run_fixed_steps(&mut app, 120);
    let world = app.world();
    let rest = world.get::<Transform>(crate_box).unwrap().position;
    let speed = world.get::<RigidBody>(crate_box).unwrap().linear_velocity;
    assert!(rest[1] > 0.45, "the box sank into the jelly to {rest:?}");
    assert!(rest[1] < 0.6, "the box floats at {rest:?}");
    assert!(speed[1].abs() < 0.3, "the box still moves at {speed:?}");
}

#[test]
fn fluid_volumes_with_a_visual_own_one_entity_per_particle() {
    use crate::assets::PrimitiveShape;
    let mut app = App::new();
    app.add_plugin(crate::assets::AssetPlugin).unwrap();
    let (mesh, material) = {
        let assets = app.world().resource::<crate::assets::AssetServer>();
        (
            assets.builtin_primitives[&PrimitiveShape::Sphere],
            assets.fallback_material,
        )
    };
    let settings = FluidSettings::default();
    let fluid = Fluid::block([0.0, 1.0, 0.0], [2, 2, 2], settings.spacing);
    let mut volume = FluidVolume::new(settings, fluid);
    volume.visual = Some(MeshRenderer {
        mesh,
        material,
        cast_shadows: false,
        receive_shadows: false,
    });
    let entity = app.world_mut().spawn(volume).id();
    run_fixed_steps(&mut app, 3);
    let world = app.world_mut();
    let count = |world: &mut bevy_ecs::world::World| {
        world.query::<&MeshRenderer>().iter(world).count()
    };
    assert_eq!(count(world), 8);
    assert_eq!(
        world
            .query::<&crate::runtime::FluidParticle>()
            .iter(world)
            .count(),
        8
    );
    world.get_mut::<FluidVolume>(entity).unwrap().visual = None;
    run_fixed_steps(&mut app, 2);
    assert_eq!(count(app.world_mut()), 0);
}

fn fluid_visual_app() -> (App, bevy_ecs::entity::Entity) {
    use crate::assets::PrimitiveShape;
    let mut app = App::new();
    app.add_plugin(crate::assets::AssetPlugin).unwrap();
    let (mesh, material) = {
        let assets = app.world().resource::<crate::assets::AssetServer>();
        (
            assets.builtin_primitives[&PrimitiveShape::Sphere],
            assets.fallback_material,
        )
    };
    let settings = FluidSettings::default();
    let fluid = Fluid::block([0.0, 1.0, 0.0], [2, 2, 2], settings.spacing);
    let mut volume = FluidVolume::new(settings, fluid);
    volume.visual = Some(MeshRenderer {
        mesh,
        material,
        cast_shadows: false,
        receive_shadows: false,
    });
    let entity = app.world_mut().spawn(volume).id();
    (app, entity)
}

fn fluid_particle_count(app: &mut App) -> usize {
    let world = app.world_mut();
    world
        .query::<&crate::runtime::FluidParticle>()
        .iter(world)
        .count()
}

#[test]
fn despawning_a_fluid_volume_removes_its_particle_entities() {
    let (mut app, volume) = fluid_visual_app();
    run_fixed_steps(&mut app, 3);
    assert_eq!(fluid_particle_count(&mut app), 8);
    app.world_mut().despawn(volume);
    run_fixed_steps(&mut app, 2);
    assert_eq!(fluid_particle_count(&mut app), 0);
}

#[test]
fn a_copied_fluid_volume_gets_its_own_particle_entities() {
    let (mut app, volume) = fluid_visual_app();
    run_fixed_steps(&mut app, 3);
    let copy = app.world().get::<FluidVolume>(volume).unwrap().clone();
    let copy = app.world_mut().spawn(copy).id();
    run_fixed_steps(&mut app, 2);
    assert_eq!(fluid_particle_count(&mut app), 16);
    let original = app.world().get::<FluidVolume>(volume).unwrap();
    let copied = app.world().get::<FluidVolume>(copy).unwrap();
    assert!(original
        .particles
        .iter()
        .all(|p| !copied.particles.contains(p)));
}

#[test]
fn fluids_snapshot_and_restore_to_the_same_future() {
    let (mut original, volume) = fluid_visual_app();
    run_fixed_steps(&mut original, 3);
    let snapshot = original.snapshot().unwrap();
    run_fixed_steps(&mut original, 5);
    let (mut restored, _) = fluid_visual_app();
    restored.restore(&snapshot).unwrap();
    run_fixed_steps(&mut restored, 5);
    let hash = |app: &App, entity| {
        app.world()
            .get::<FluidVolume>(entity)
            .unwrap()
            .fluid
            .state_hash()
    };
    assert_eq!(hash(&restored, volume), hash(&original, volume));
    assert_eq!(fluid_particle_count(&mut restored), 8);
}

#[test]
fn a_light_ball_floats_in_a_fluid_and_a_heavy_one_sinks() {
    let rest_height = |mass: f32, solver: FluidSolver| {
        let mut app = App::new();
        cpu_ground(app.world_mut());
        let settings = FluidSettings {
            bounds: ([-0.5, 0.0, -0.5], [0.5, 2.0, 0.5]),
            solver,
            // The grid's cells (twice the spacing) must be smaller than the
            // ball, or the hole it leaves in the water stays open.
            spacing: match solver {
                FluidSolver::Particles | FluidSolver::Granular => 0.1,
                FluidSolver::Grid => 0.05,
            },
            ..FluidSettings::default()
        };
        // Water at 1000 kg per cubic meter for either spacing.
        let settings = FluidSettings {
            particle_mass: 1000.0 * settings.spacing.powi(3),
            ..settings
        };
        // The same 0.9 by 0.5 by 0.9 m block either way.
        let per_meter = (1.0 / settings.spacing).round() as u32;
        let fluid = Fluid::block(
            [-0.4, 0.0, -0.4],
            [9 * per_meter / 10, 5 * per_meter / 10, 9 * per_meter / 10],
            settings.spacing,
        );
        app.world_mut().spawn(FluidVolume::new(settings, fluid));
        let ball = cpu_body(
            app.world_mut(),
            [0.0, 0.6, 0.0],
            ColliderShape::Sphere { radius: 0.15 },
            RigidBodyKind::Dynamic,
        );
        app.world_mut().get_mut::<RigidBody>(ball).unwrap().mass = mass;
        run_fixed_steps(&mut app, 240);
        app.world().get::<Transform>(ball).unwrap().position[1]
    };
    // The grid solver floats it the same way.
    for solver in [FluidSolver::Particles, FluidSolver::Grid] {
        let light = rest_height(7.0, solver);
        let heavy = rest_height(50.0, solver);
        assert!(heavy < 0.3, "{solver:?}: heavy ball rests at {heavy}");
        assert!(
            light > heavy + 0.05,
            "{solver:?}: light {light} vs heavy {heavy}"
        );
    }
}

#[test]
fn a_flowing_fluid_pushes_a_floating_ball_along() {
    let drift = |flow: f32| {
        let mut app = App::new();
        cpu_ground(app.world_mut());
        let settings = FluidSettings {
            bounds: ([-1.0, 0.0, -0.5], [1.0, 2.0, 0.5]),
            ..FluidSettings::default()
        };
        let mut fluid =
            Fluid::block([-0.9, 0.0, -0.4], [19, 5, 9], settings.spacing);
        for velocity in &mut fluid.velocities {
            velocity[0] = flow;
        }
        app.world_mut().spawn(FluidVolume::new(settings, fluid));
        let ball = cpu_body(
            app.world_mut(),
            [-0.3, 0.3, 0.0],
            ColliderShape::Sphere { radius: 0.15 },
            RigidBodyKind::Dynamic,
        );
        app.world_mut().get_mut::<RigidBody>(ball).unwrap().mass = 7.0;
        run_fixed_steps(&mut app, 12);
        app.world().get::<Transform>(ball).unwrap().position[0] + 0.3
    };
    let (still, flowing) = (drift(0.0), drift(2.0));
    assert!(still.abs() < 0.02, "a still pool moves the ball {still}");
    assert!(flowing > 0.05, "the stream moves the ball {flowing}");
}

#[test]
fn a_light_box_floats_in_a_fluid_and_a_heavy_one_sinks() {
    let rest_height = |mass: f32| {
        let mut app = App::new();
        cpu_ground(app.world_mut());
        let settings = FluidSettings {
            bounds: ([-0.5, 0.0, -0.5], [0.5, 2.0, 0.5]),
            ..FluidSettings::default()
        };
        let fluid =
            Fluid::block([-0.4, 0.0, -0.4], [9, 5, 9], settings.spacing);
        app.world_mut().spawn(FluidVolume::new(settings, fluid));
        let body = cpu_body(
            app.world_mut(),
            [0.0, 0.6, 0.0],
            ColliderShape::Box {
                half_extents: [0.12; 3],
            },
            RigidBodyKind::Dynamic,
        );
        app.world_mut().get_mut::<RigidBody>(body).unwrap().mass = mass;
        run_fixed_steps(&mut app, 240);
        let y = app.world().get::<Transform>(body).unwrap().position[1];
        assert!(y.is_finite());
        y
    };
    let light = rest_height(5.0);
    let heavy = rest_height(100.0);
    assert!(heavy < 0.3, "heavy box rests at {heavy}");
    assert!(light > heavy + 0.05, "light {light} vs heavy {heavy}");
}

#[test]
fn a_light_capsule_floats_in_a_fluid_and_a_heavy_one_sinks() {
    let rest_height = |mass: f32| {
        let mut app = App::new();
        cpu_ground(app.world_mut());
        let settings = FluidSettings {
            bounds: ([-0.5, 0.0, -0.5], [0.5, 2.0, 0.5]),
            ..FluidSettings::default()
        };
        let fluid =
            Fluid::block([-0.4, 0.0, -0.4], [9, 5, 9], settings.spacing);
        app.world_mut().spawn(FluidVolume::new(settings, fluid));
        let body = cpu_body(
            app.world_mut(),
            [0.0, 0.6, 0.0],
            ColliderShape::Capsule {
                half_height: 0.1,
                radius: 0.12,
            },
            RigidBodyKind::Dynamic,
        );
        app.world_mut().get_mut::<RigidBody>(body).unwrap().mass = mass;
        run_fixed_steps(&mut app, 240);
        let y = app.world().get::<Transform>(body).unwrap().position[1];
        assert!(y.is_finite());
        y
    };
    let light = rest_height(5.0);
    let heavy = rest_height(100.0);
    assert!(heavy < 0.3, "heavy capsule rests at {heavy}");
    assert!(light > heavy + 0.05, "light {light} vs heavy {heavy}");
}

#[test]
fn fluid_blocks_round_trip_through_scenes_and_become_volumes() {
    let block = FluidBlock {
        count_x: 3,
        count_y: 2,
        count_z: 4,
        viscosity: 0.2,
        solver: FluidSolver::Granular,
        friction: 0.9,
        visible: false,
        ..FluidBlock::default()
    };
    let mut app = App::new();
    app.add_plugin(crate::assets::AssetPlugin).unwrap();
    let entity = app.spawn((Transform::new([1.0, 2.0, 3.0]), block));
    let document = scene_document(app.world_mut(), "fluid").unwrap();
    let mut loaded = App::new();
    loaded.add_plugin(crate::assets::AssetPlugin).unwrap();
    load_scene_document(loaded.world_mut(), &document, SceneLoadMode::Replace)
        .unwrap();
    let mut query = loaded.world_mut().query::<&FluidBlock>();
    assert_eq!(query.single(loaded.world()).unwrap(), &block);
    run_fixed_steps(&mut loaded, 2);
    let mut query = loaded.world_mut().query::<&FluidVolume>();
    let volume = query.single(loaded.world()).unwrap();
    assert_eq!(volume.fluid.positions.len(), 24);
    assert_eq!(volume.settings.bounds.0[1], 1.5);
    assert_eq!(volume.settings.solver, FluidSolver::Granular);
    assert_eq!(volume.settings.friction, 0.9);
    let _ = entity;
}

#[test]
fn soft_blocks_round_trip_through_scenes_and_land_on_a_box() {
    let block = SoftBlock {
        count_x: 2,
        count_y: 1,
        count_z: 3,
        softness: 1e-3,
        tear_strain: 0.5,
        ..SoftBlock::default()
    };
    let mut app = App::new();
    app.add_plugin(crate::assets::AssetPlugin).unwrap();
    app.spawn((Transform::new([0.0, 1.0, 0.0]), block));
    app.spawn((
        Transform::new([0.0, -0.5, 0.0]),
        PhysicsBody::default(),
        RigidBody {
            kind: RigidBodyKind::Fixed,
            ..RigidBody::default()
        },
        Collider {
            shape: ColliderShape::Box {
                half_extents: [2.0, 0.5, 2.0],
            },
            ..Collider::default()
        },
    ));
    let document = scene_document(app.world_mut(), "soft").unwrap();
    let mut loaded = App::new();
    loaded.add_plugin(crate::assets::AssetPlugin).unwrap();
    load_scene_document(loaded.world_mut(), &document, SceneLoadMode::Replace)
        .unwrap();
    let mut query = loaded.world_mut().query::<&SoftBlock>();
    assert_eq!(query.single(loaded.world()).unwrap(), &block);
    run_fixed_steps(&mut loaded, 120);
    let mut query = loaded.world_mut().query::<&SoftBodyVolume>();
    let volume = query.single(loaded.world()).unwrap();
    assert_eq!(volume.body.positions.len(), 3 * 2 * 4);
    assert_eq!(volume.body.tetrahedra.len(), 6 * 6);
    assert_eq!(volume.settings.tear_strain, Some(0.5));
    let bottom = volume
        .body
        .positions
        .iter()
        .map(|p| p[1])
        .fold(f32::MAX, f32::min);
    assert!((-0.01..0.03).contains(&bottom), "rests at {bottom}");
}

#[test]
fn a_visible_soft_block_draws_one_skin_that_goes_with_it() {
    let mut app = App::new();
    app.add_plugin(crate::assets::AssetPlugin).unwrap();
    let block =
        app.spawn((Transform::new([0.0, 1.0, 0.0]), SoftBlock::default()));
    let hidden = app.spawn((
        Transform::new([3.0, 1.0, 0.0]),
        SoftBlock {
            visible: false,
            ..SoftBlock::default()
        },
    ));
    run_fixed_steps(&mut app, 2);
    let skins = |app: &mut App| {
        let mut query = app
            .world_mut()
            .query::<(&crate::runtime::FluidParticle, &MeshRenderer)>();
        query
            .iter(app.world())
            .map(|(owner, renderer)| (owner.0, renderer.mesh))
            .collect::<Vec<_>>()
    };
    let drawn = skins(&mut app);
    assert_eq!(drawn.len(), 1);
    assert_eq!(drawn[0].0, block);
    let mesh = drawn[0].1;
    let low = |app: &App| {
        let assets = app.world().resource::<crate::assets::AssetServer>();
        assets
            .meshes
            .get(mesh)
            .unwrap()
            .vertices
            .iter()
            .map(|v| v.position[1])
            .fold(f32::MAX, f32::min)
    };
    let before = low(&app);
    run_fixed_steps(&mut app, 10);
    assert!(
        low(&app) < before - 0.01,
        "the skin did not follow the fall"
    );
    assert_eq!(skins(&mut app).len(), 1);
    app.world_mut().despawn(block);
    run_fixed_steps(&mut app, 2);
    assert!(skins(&mut app).is_empty());
    let assets = app.world().resource::<crate::assets::AssetServer>();
    assert!(assets.meshes.get(mesh).is_none());
    let _ = hidden;
}

#[test]
fn cloth_round_trips_through_scenes_hangs_on_its_holder_and_drapes() {
    let fixed_box = |position: [f32; 3], half_extents: [f32; 3]| {
        (
            Transform::new(position),
            PhysicsBody::default(),
            RigidBody {
                kind: RigidBodyKind::Fixed,
                ..RigidBody::default()
            },
            Collider {
                shape: ColliderShape::Box { half_extents },
                ..Collider::default()
            },
        )
    };
    let mut app = App::new();
    app.add_plugin(crate::assets::AssetPlugin).unwrap();
    let pole = app.spawn(fixed_box([0.0, 2.05, 3.0], [0.5, 0.05, 0.05]));
    let flag = ClothSheet {
        count_x: 6,
        count_y: 6,
        holder: pole,
        wind: [0.0, 0.0, 2.0],
        ..ClothSheet::default()
    };
    // The flag's top edge sits on the pole's bottom face.
    app.spawn((Transform::new([0.0, 1.7, 3.0]), flag));
    app.spawn(fixed_box([0.0, -0.5, 0.0], [2.0, 0.5, 2.0]));
    let tarp = ClothSheet {
        count_x: 6,
        count_y: 6,
        pin_top: false,
        visible: false,
        ..ClothSheet::default()
    };
    app.spawn((Transform::new([0.0, 1.0, 0.0]), tarp));
    let document = scene_document(app.world_mut(), "cloth").unwrap();
    let mut loaded = App::new();
    loaded.add_plugin(crate::assets::AssetPlugin).unwrap();
    load_scene_document(loaded.world_mut(), &document, SceneLoadMode::Replace)
        .unwrap();
    let mut sheets = loaded
        .world_mut()
        .query::<(bevy_ecs::entity::Entity, &ClothSheet)>();
    let sheets: Vec<_> = sheets
        .iter(loaded.world())
        .map(|(entity, sheet)| (entity, *sheet))
        .collect();
    assert_eq!(sheets.len(), 2);
    let (flag_entity, loaded_flag) =
        *sheets.iter().find(|(_, sheet)| sheet.pin_top).unwrap();
    let (tarp_entity, _) =
        *sheets.iter().find(|(_, sheet)| !sheet.pin_top).unwrap();
    let pole_position = loaded
        .world()
        .get::<Transform>(loaded_flag.holder)
        .expect("the holder is remapped to the loaded pole")
        .position;
    assert_eq!(pole_position, [0.0, 2.05, 3.0]);
    assert_eq!(
        loaded_flag,
        ClothSheet {
            holder: loaded_flag.holder,
            ..flag
        }
    );
    run_fixed_steps(&mut loaded, 90);
    let world = loaded.world();
    let flag = world.get::<ClothVolume>(flag_entity).unwrap();
    assert_eq!(flag.attachments.len(), 7);
    for particle in 0..7 {
        let top = flag.cloth.positions[particle];
        assert!((top[1] - 2.0).abs() < 0.01, "top row moved to {top:?}");
    }
    let blown = flag.cloth.positions.iter().map(|p| p[2]).sum::<f32>()
        / flag.cloth.positions.len() as f32;
    assert!(blown > 3.02, "the wind did not blow the flag: {blown}");
    let tarp = world.get::<ClothVolume>(tarp_entity).unwrap();
    assert!(tarp.skin.is_none());
    for p in &tarp.cloth.positions {
        assert!(p[1] > -0.02, "the tarp fell through the box: {p:?}");
    }
    // Only the flag draws: one two-sided mesh.
    let mut drawn = loaded
        .world_mut()
        .query::<(&crate::runtime::FluidParticle, &MeshRenderer)>();
    let drawn: Vec<_> = drawn
        .iter(loaded.world())
        .map(|(owner, renderer)| (owner.0, renderer.mesh))
        .collect();
    assert_eq!(drawn.len(), 1);
    assert_eq!(drawn[0].0, flag_entity);
    let assets = loaded.world().resource::<crate::assets::AssetServer>();
    let mesh = assets.meshes.get(drawn[0].1).unwrap();
    assert_eq!(mesh.vertices.len(), 2 * 49);
    assert_eq!(mesh.indices.len(), 2 * 3 * 72);
}

#[test]
fn huge_soft_blocks_are_shrunk_to_the_cap() {
    let block = SoftBlock {
        count_x: 1000,
        count_y: 1000,
        count_z: 2,
        ..SoftBlock::default()
    };
    let volume = block.volume([0.0; 3]).unwrap();
    let particles = volume.body.positions.len() as u64;
    assert!(particles <= crate::runtime::soft_body::MAX_SOFT_BLOCK_PARTICLES);
    assert!(particles > 1000, "shrunk too far to {particles}");
}

#[test]
fn a_fluid_surface_is_one_entity_whose_mesh_follows_the_particles() {
    let mut app = App::new();
    app.add_plugin(crate::assets::AssetPlugin).unwrap();
    let material = app
        .world()
        .resource::<crate::assets::AssetServer>()
        .fallback_material;
    let settings = FluidSettings::default();
    let fluid = Fluid::block([0.0, 1.0, 0.0], [3, 3, 3], settings.spacing);
    let mut volume = FluidVolume::new(settings, fluid);
    volume.surface = Some(crate::runtime::FluidSurface::new(material));
    app.world_mut().spawn(volume);
    run_fixed_steps(&mut app, 1);
    let world = app.world_mut();
    let handles: Vec<_> = world
        .query::<&MeshRenderer>()
        .iter(world)
        .map(|renderer| renderer.mesh)
        .collect();
    assert_eq!(
        handles.len(),
        1,
        "one surface entity, no sphere per particle"
    );
    let assets = world.resource::<crate::assets::AssetServer>();
    let before = assets.meshes.get(handles[0]).unwrap().clone();
    assert!(before.vertices.len() > 100);
    run_fixed_steps(&mut app, 20);
    let world = app.world_mut();
    assert_eq!(world.query::<&MeshRenderer>().iter(world).count(), 1);
    let after = world
        .resource::<crate::assets::AssetServer>()
        .meshes
        .get(handles[0])
        .unwrap();
    assert_ne!(*after, before, "the surface moves with the fluid");
}

#[test]
fn a_water_body_draws_one_surface_floats_a_light_ball_and_carries_it() {
    use crate::runtime::WaterBody;
    let end = |mass: f32| {
        let mut app = App::new();
        app.add_plugin(crate::assets::AssetPlugin).unwrap();
        app.world_mut().spawn((
            Transform::new([0.0, 0.0, 0.0]),
            WaterBody {
                wave_height: 0.0,
                flow_speed: 2.0,
                ..WaterBody::default()
            },
        ));
        let ball = cpu_body(
            app.world_mut(),
            [0.0, 0.0, 0.0],
            ColliderShape::Sphere { radius: 0.15 },
            RigidBodyKind::Dynamic,
        );
        app.world_mut().get_mut::<RigidBody>(ball).unwrap().mass = mass;
        run_fixed_steps(&mut app, 240);
        let world = app.world_mut();
        assert_eq!(world.query::<&MeshRenderer>().iter(world).count(), 1);
        world.get::<Transform>(ball).unwrap().position
    };
    let light = end(7.0);
    let heavy = end(50.0);
    assert!(light[1] > -0.1, "light ball floats: {light:?}");
    assert!(heavy[1] < -1.0, "heavy ball sinks: {heavy:?}");
    assert!(light[0] > 0.5, "the current carries the ball: {light:?}");
}

#[test]
fn raycast_visible_passes_through_hidden_objects_and_their_children() {
    let mut app = App::new();
    let world = app.world_mut();
    let wall =
        cpu_body(world, [0.0, 0.0, -6.0], UNIT_BOX, RigidBodyKind::Fixed);
    world.entity_mut(wall).insert(Name("Wall".into()));
    let template = world
        .spawn((Name("Template".into()), Visibility { visible: false }))
        .id();
    let desk =
        cpu_body(world, [0.0, 0.0, -3.0], UNIT_BOX, RigidBodyKind::Fixed);
    world.entity_mut(desk).insert(Name("Desk".into()));
    rusting_core::hierarchy::set_parent(world, desk, template).unwrap();
    run_fixed_steps(&mut app, 2);
    let scene = crate::project_runner::GameScene {
        world: app.world_mut(),
    };
    let ahead = [0.0, 0.0, -1.0];
    let hit = |hit: Option<crate::project_runner::RayHit>| hit.unwrap().name;
    assert_eq!(hit(scene.raycast([0.0; 3], ahead, 10.0)), "Desk");
    assert_eq!(hit(scene.raycast_visible([0.0; 3], ahead, 10.0)), "Wall");
}

#[test]
fn a_fixed_gpu_box_is_solid_to_cpu_queries_but_a_dynamic_one_is_not() {
    let mut app = App::new();
    let world = app.world_mut();
    let fixed =
        cpu_body(world, [0.0, 0.0, -3.0], UNIT_BOX, RigidBodyKind::Fixed);
    world.get_mut::<PhysicsBody>(fixed).unwrap().simulation =
        SimulationClass::Gpu;
    let dynamic =
        cpu_body(world, [0.0, 0.0, 3.0], UNIT_BOX, RigidBodyKind::Dynamic);
    world.get_mut::<PhysicsBody>(dynamic).unwrap().simulation =
        SimulationClass::Gpu;
    run_fixed_steps(&mut app, 2);
    let physics = app.world().resource::<PhysicsWorld>();
    assert!(physics
        .raycast([0.0; 3], [0.0, 0.0, -1.0], 10.0, u32::MAX)
        .is_some());
    assert!(physics
        .raycast([0.0; 3], [0.0, 0.0, 1.0], 10.0, u32::MAX)
        .is_none());
    // GPU bodies collide with each other on the GPU, not through this list.
    assert!(physics.gpu_colliders().is_empty());
}

#[test]
fn a_switched_off_fluid_surface_and_its_mesh_are_removed() {
    let mut app = App::new();
    app.add_plugin(crate::assets::AssetPlugin).unwrap();
    let material = app
        .world()
        .resource::<crate::assets::AssetServer>()
        .fallback_material;
    let settings = FluidSettings::default();
    let fluid = Fluid::block([0.0, 1.0, 0.0], [3, 3, 3], settings.spacing);
    let mut volume = FluidVolume::new(settings, fluid);
    volume.surface = Some(crate::runtime::FluidSurface::new(material));
    let owner = app.world_mut().spawn(volume).id();
    run_fixed_steps(&mut app, 2);
    let meshes = |app: &mut App| {
        app.world()
            .resource::<crate::assets::AssetServer>()
            .meshes
            .len()
    };
    let with_surface = meshes(&mut app);
    app.world_mut()
        .get_mut::<FluidVolume>(owner)
        .unwrap()
        .surface = None;
    run_fixed_steps(&mut app, 2);
    let world = app.world_mut();
    assert_eq!(world.query::<&MeshRenderer>().iter(world).count(), 0);
    assert_eq!(meshes(&mut app), with_surface - 1);
}

#[test]
fn a_scene_input_action_unbinds_when_its_component_goes() {
    use crate::runtime::{ActionMap, InputAction};
    let mut app = App::new();
    let owner = app
        .world_mut()
        .spawn(InputAction {
            action: "fire".into(),
            inputs: vec!["KeyF".into()],
        })
        .id();
    run_fixed_steps(&mut app, 1);
    app.update(Duration::from_millis(16)).unwrap();
    assert_eq!(
        app.world().resource::<ActionMap>().bindings("fire").len(),
        1
    );
    app.world_mut().despawn(owner);
    app.update(Duration::from_millis(16)).unwrap();
    assert!(app
        .world()
        .resource::<ActionMap>()
        .bindings("fire")
        .is_empty());
}

#[test]
fn a_tiny_time_scale_under_overload_does_not_panic() {
    fn sluggish(_: ResMut<Counts>) {
        std::thread::sleep(Duration::from_millis(3));
    }
    let mut app = EngineBuilder::new()
        .fixed_delta(Duration::from_millis(1))
        .max_fixed_steps(4)
        .build()
        .unwrap();
    app.insert_resource(Counts::default())
        .add_systems(ScheduleStage::FixedUpdate, sluggish);
    app.update(Duration::from_millis(50)).unwrap();
    // Overloaded now; one fixed step of scaled time overflows `Duration`.
    app.world_mut().resource_mut::<TimeControl>().time_scale = 1e-300;
    app.update(Duration::from_millis(50)).unwrap();
}

#[test]
fn tile_map_rectangles_stop_at_the_cell_cap() {
    let mut map = TileMap::default();
    let edge = MAX_TILE_CELLS as f32 * map.tile_size;
    assert_eq!(
        map.cell_at([edge - 0.01, -0.01]),
        Some((MAX_TILE_CELLS - 1, 0))
    );
    assert_eq!(map.cell_at([edge + 0.01, -0.01]), None);
    // One rebuild per row, so the widest rectangle is quick.
    let far = (MAX_TILE_CELLS - 1, 3);
    assert!(map.fill_rect((0, 0), far, '#'));
    assert_eq!(map.rows.len(), 4);
    assert!(map.rows.iter().all(|row| row.len() == MAX_TILE_CELLS));
    assert!(!map.fill_rect((0, 0), far, '#'));
}

#[test]
fn a_platformer_rides_a_platform_whose_parent_moves() {
    let mut app = App::new();
    let world = app.world_mut();
    let root = world
        .spawn((
            Transform::new([0.0; 3]),
            Tween {
                from: [0.0; 3],
                to: [3.0, 0.0, 0.0],
                duration: 1.0,
                delay: 0.5,
                easing: Easing::Linear,
                repeat: TweenRepeat::Once,
                ..Tween::default()
            },
        ))
        .id();
    let platform = cpu_body(
        world,
        [0.0, -0.5, 0.0],
        ColliderShape::Box {
            half_extents: [2.0, 0.5, 2.0],
        },
        RigidBodyKind::Kinematic,
    );
    let runner = world
        .spawn((
            Transform::new([0.0, 0.6, 0.0]),
            PlatformerController::default(),
        ))
        .id();
    app.set_parent(platform, root).unwrap();
    run_fixed_steps(&mut app, 120);
    let x = app.world().get::<Transform>(runner).unwrap().position[0];
    assert!((x - 3.0).abs() < 0.05, "rode to {x}");
}

/// A two-bone character standing on the ground with a ragdoll that a
/// ball thrown at it knocks down. Returns the app, the character, its hips
/// and the ball.
fn ragdoll_scene(muscle: f32) -> (App, Entity, Entity, Entity) {
    let mut app = App::new();
    let world = app.world_mut();
    cpu_ground(world);
    let hero = world
        .spawn((
            Name("Hero".into()),
            Transform::new([0.0, 0.9, 0.0]),
            PhysicsBody {
                simulation: SimulationClass::Static,
                ..PhysicsBody::default()
            },
            Collider {
                shape: ColliderShape::Box {
                    half_extents: [0.25, 0.9, 0.25],
                },
                ..Collider::default()
            },
            Ragdoll {
                bones: vec![
                    RagdollBone {
                        path: "Hips".into(),
                        length: 0.3,
                        radius: 0.12,
                        mass: 10.0,
                        ..RagdollBone::default()
                    },
                    RagdollBone {
                        path: "Hips/Spine".into(),
                        length: 0.5,
                        radius: 0.12,
                        mass: 12.0,
                        ..RagdollBone::default()
                    },
                ],
                hit_speed: 3.0,
                recover_after: 1.0,
                blend_time: 0.5,
                muscle,
                command: None,
            },
        ))
        .id();
    let hips = world
        .spawn((Name("Hips".into()), Transform::new([0.0, 0.1, 0.0])))
        .id();
    let spine = world
        .spawn((Name("Spine".into()), Transform::new([0.0, 0.3, 0.0])))
        .id();
    hierarchy::set_parent(world, hips, hero).unwrap();
    hierarchy::set_parent(world, spine, hips).unwrap();
    let ball = cpu_body(
        world,
        [2.0, 1.4, 0.0],
        ColliderShape::Sphere { radius: 0.2 },
        RigidBodyKind::Dynamic,
    );
    let mut body = world.get_mut::<RigidBody>(ball).unwrap();
    body.linear_velocity = [-12.0, 0.0, 0.0];
    body.mass = 5.0;
    body.gravity_scale = 0.0;
    (app, hero, hips, ball)
}

fn phase(app: &App, hero: Entity) -> RagdollPhase {
    app.world()
        .get::<RagdollState>(hero)
        .map_or(RagdollPhase::Animated, |state| state.phase)
}

#[test]
fn reset_ragdoll_teleports_an_active_ragdoll_with_its_bodies() {
    let (mut app, hero, _, _) = ragdoll_scene(20.0);
    run_fixed_steps(&mut app, 2);
    let old = app.world().get::<RagdollState>(hero).unwrap().parts[0];
    let was = app.world().get::<Transform>(old).unwrap().position;
    app.world_mut().get_mut::<Transform>(hero).unwrap().position[0] += 5.0;
    let mut scene = crate::project_runner::GameScene {
        world: app.world_mut(),
    };
    assert!(scene.reset_ragdoll("Hero"));
    assert!(!scene.reset_ragdoll("Nobody"));
    assert!(app.world().get_entity(old).is_err(), "old bodies are gone");
    run_fixed_steps(&mut app, 1);
    assert_eq!(phase(&app, hero), RagdollPhase::Active);
    let part = app.world().get::<RagdollState>(hero).unwrap().parts[0];
    let at = app.world().get::<Transform>(part).unwrap().position;
    assert!((at[0] - was[0] - 5.0).abs() < 0.05, "hips body at {at:?}");
    let speed = nalgebra::Vector3::from(
        app.world().get::<RigidBody>(part).unwrap().linear_velocity,
    );
    assert!(speed.norm() < 1.0, "respawned moving at {speed}");
}

#[test]
fn an_active_ragdoll_returns_an_unkeyed_rotation_to_its_pose() {
    let (mut app, hero, hips, _) = ragdoll_scene(20.0);
    run_fixed_steps(&mut app, 1);
    assert_eq!(phase(&app, hero), RagdollPhase::Active);
    let part = app.world().get::<RagdollState>(hero).unwrap().parts[0];
    app.world_mut()
        .get_mut::<RigidBody>(part)
        .unwrap()
        .angular_velocity = [0.0, 0.0, 8.0];
    // A clip that keys only the hips' position, every tick.
    for _ in 0..90 {
        app.world_mut().get_mut::<Transform>(hips).unwrap().position =
            [0.0, 0.1, 0.0];
        run_fixed_steps(&mut app, 1);
    }
    let turn = quaternion_of(&app, hips);
    assert!(
        turn.angle() < 0.2,
        "hips stayed turned by {} rad",
        turn.angle()
    );
}

#[test]
fn ray_hits_on_ragdoll_bodies_carry_the_bone_name_and_classes() {
    let (mut app, hero, hips, _) = ragdoll_scene(20.0);
    app.world_mut().entity_mut(hips).insert(ObjectClasses {
        names: vec!["animatronic".into()],
    });
    app.world_mut().entity_mut(hero).insert(ObjectClasses {
        names: vec!["hero".into()],
    });
    run_fixed_steps(&mut app, 1);
    assert_eq!(phase(&app, hero), RagdollPhase::Active);
    // The physics world picks the new bodies up on the next tick.
    run_fixed_steps(&mut app, 1);
    let part = app.world().get::<RagdollState>(hero).unwrap().parts[0];
    let centre = app.world().get::<Transform>(part).unwrap().position;
    let from = [centre[0], centre[1], centre[2] + 3.0];
    let scene = crate::project_runner::GameScene {
        world: app.world_mut(),
    };
    let down = [0.0, 0.0, -1.0];
    let hit = scene.raycast_skipping(from, down, 10.0, &["hero"]).unwrap();
    assert_eq!(hit.name, "Hips");
    let skipped =
        scene.raycast_skipping(from, down, 10.0, &["hero", "animatronic"]);
    assert_ne!(skipped.map(|hit| hit.name), Some("Hips".to_owned()));
}

#[test]
fn raycast_only_hits_the_named_classes_and_passes_the_rest() {
    let mut app = App::new();
    let world = app.world_mut();
    let named = |world: &mut bevy_ecs::world::World, y: f32, name: &str| {
        let entity =
            cpu_body(world, [0.0, y, 0.0], UNIT_BOX, RigidBodyKind::Fixed);
        world.entity_mut(entity).insert((
            Name(name.into()),
            ObjectClasses {
                names: vec![name.to_lowercase()],
            },
        ));
    };
    // A bird and a boat above the island.
    named(world, 0.0, "Island");
    named(world, 3.0, "Boat");
    named(world, 6.0, "Bird");
    run_fixed_steps(&mut app, 1);
    let scene = crate::project_runner::GameScene {
        world: app.world_mut(),
    };
    let (from, down) = ([0.0, 10.0, 0.0], [0.0, -1.0, 0.0]);
    let name =
        |hit: Option<crate::project_runner::RayHit>| hit.map(|hit| hit.name);
    assert_eq!(name(scene.raycast(from, down, 20.0)), Some("Bird".into()));
    let island = scene.raycast_only(from, down, 20.0, &["island"]).unwrap();
    assert_eq!(island.name, "Island");
    assert!((island.distance - 9.5).abs() < 1e-4, "{}", island.distance);
    assert_eq!(
        name(scene.raycast_only(from, down, 20.0, &["island", "boat"])),
        Some("Boat".into())
    );
    assert_eq!(name(scene.raycast_only(from, down, 20.0, &[])), None);
}

#[test]
fn ragdolls_go_limp_on_a_hit_fall_and_blend_back_deterministically() {
    let (mut app, hero, hips, _) = ragdoll_scene(0.0);
    let mut steps = 0;
    while phase(&app, hero) == RagdollPhase::Animated {
        run_fixed_steps(&mut app, 1);
        steps += 1;
        assert!(steps < 30, "the ball never knocked the character down");
    }
    let parts = app.world().get::<RagdollState>(hero).unwrap().parts.clone();
    assert_eq!(parts.len(), 2);
    let joint = app
        .world()
        .get::<Joint>(parts[1])
        .expect("spine is jointed");
    assert_eq!(joint.target, parts[0]);
    assert!(app.world().get::<Collider>(hero).is_none());
    // The bodies fall and the bones follow them.
    run_fixed_steps(&mut app, 50);
    let fallen = app.world().get::<GlobalTransform>(hips).unwrap().matrix[3];
    let parts_y = app.world().get::<Transform>(parts[0]).unwrap().position[1];
    assert!(parts_y < 0.8, "hips body at {parts_y}");
    let hips_local = *app.world().get::<Transform>(hips).unwrap();
    assert_ne!(hips_local.position, [0.0, 0.1, 0.0]);
    // Same scene, same ticks, same pose.
    let (mut again, hero_b, hips_b, _) = ragdoll_scene(0.0);
    run_fixed_steps(&mut again, steps + 50);
    assert_eq!(phase(&again, hero_b), RagdollPhase::Limp);
    assert_eq!(*again.world().get::<Transform>(hips_b).unwrap(), hips_local);
    // It gets up where it lies and blends back to its old pose.
    run_fixed_steps(&mut app, 15);
    assert_eq!(phase(&app, hero), RagdollPhase::Blending);
    assert!(parts
        .iter()
        .all(|&part| app.world().get_entity(part).is_err()));
    assert!(app.world().get::<Collider>(hero).is_some());
    let root = app.world().get::<Transform>(hero).unwrap().position;
    assert!(
        (root[0] - fallen[0]).abs() < 0.2 && (root[2] - fallen[2]).abs() < 0.2,
        "the character stands where its hips lay: {root:?} {fallen:?}"
    );
    run_fixed_steps(&mut app, 31);
    assert_eq!(phase(&app, hero), RagdollPhase::Animated);
    let back = app.world().get::<Transform>(hips).unwrap();
    assert!(back
        .position
        .iter()
        .zip([0.0, 0.1, 0.0])
        .all(|(a, b)| (a - b).abs() < 1e-5));
    assert!(back.rotation.iter().all(|a| a.abs() < 1e-5), "{back:?}");
    // Game code turns it on and off.
    app.world_mut().get_mut::<Ragdoll>(hero).unwrap().command = Some(true);
    run_fixed_steps(&mut app, 1);
    assert_eq!(phase(&app, hero), RagdollPhase::Limp);
    app.world_mut().get_mut::<Ragdoll>(hero).unwrap().command = Some(false);
    run_fixed_steps(&mut app, 1);
    assert_eq!(phase(&app, hero), RagdollPhase::Blending);
}

#[test]
fn active_ragdolls_hold_the_pose_shrug_off_pushes_and_get_back_up() {
    let (mut app, hero, _, ball) = ragdoll_scene(10.0);
    app.world_mut().despawn(ball);
    run_fixed_steps(&mut app, 1);
    assert_eq!(phase(&app, hero), RagdollPhase::Active);
    let parts = app.world().get::<RagdollState>(hero).unwrap().parts.clone();
    let spine_tilt = |app: &App| {
        let up = quaternion_of(app, parts[1]) * nalgebra::Vector3::y();
        up.y.clamp(-1.0, 1.0).acos()
    };
    let hips_y =
        |app: &App| app.world().get::<Transform>(parts[0]).unwrap().position[1];
    // Muscles hold the rest pose up against gravity.
    run_fixed_steps(&mut app, 120);
    assert!(spine_tilt(&app) < 0.05, "spine tilts {}", spine_tilt(&app));
    assert!(
        (hips_y(&app) - 1.15).abs() < 0.05,
        "hips at {}",
        hips_y(&app)
    );
    // A shove bends the spine, and the muscles pull it back.
    app.world_mut()
        .get_mut::<RigidBody>(parts[1])
        .unwrap()
        .angular_velocity = [0.0, 0.0, 30.0];
    run_fixed_steps(&mut app, 2);
    assert!(spine_tilt(&app) > 0.1, "spine tilts {}", spine_tilt(&app));
    run_fixed_steps(&mut app, 60);
    assert!(spine_tilt(&app) < 0.05, "spine tilts {}", spine_tilt(&app));
    // A clip bending the spine (written each tick) is followed.
    let spine = find_target(app.world(), hero, "Hips/Spine").unwrap();
    for _ in 0..120 {
        app.world_mut()
            .get_mut::<Transform>(spine)
            .unwrap()
            .rotation = [0.0, 0.0, 0.5];
        run_fixed_steps(&mut app, 1);
    }
    assert!(
        // Gravity on the leaning spine sags a 10 Hz muscle a little.
        (spine_tilt(&app) - 0.5).abs() < 0.08,
        "{}",
        spine_tilt(&app)
    );
    // Limp, it falls; recovered, the muscles stand it back up.
    app.world_mut().get_mut::<Ragdoll>(hero).unwrap().command = Some(true);
    run_fixed_steps(&mut app, 40);
    assert_eq!(phase(&app, hero), RagdollPhase::Limp);
    assert!(hips_y(&app) < 0.5, "hips at {}", hips_y(&app));
    app.world_mut().get_mut::<Ragdoll>(hero).unwrap().command = Some(false);
    run_fixed_steps(&mut app, 120);
    assert_eq!(phase(&app, hero), RagdollPhase::Active);
    assert!(
        (hips_y(&app) - 1.15).abs() < 0.05,
        "hips at {}",
        hips_y(&app)
    );
    assert!(spine_tilt(&app) < 0.05, "spine tilts {}", spine_tilt(&app));
    // Muscles off: limp, then plain animation once it recovers.
    app.world_mut().get_mut::<Ragdoll>(hero).unwrap().muscle = 0.0;
    run_fixed_steps(&mut app, 1);
    assert_eq!(phase(&app, hero), RagdollPhase::Limp);
    run_fixed_steps(&mut app, 62);
    assert_eq!(phase(&app, hero), RagdollPhase::Blending);
    assert!(parts
        .iter()
        .all(|&part| app.world().get_entity(part).is_err()));
}

fn quaternion_of(app: &App, entity: Entity) -> nalgebra::UnitQuaternion<f32> {
    let [roll, pitch, yaw] =
        app.world().get::<Transform>(entity).unwrap().rotation;
    nalgebra::UnitQuaternion::from_rotation_matrix(
        &sim_math::rotation_from_euler(roll, pitch, yaw),
    )
}

#[test]
fn player_controller_crouches_under_a_table_and_stands_only_with_room() {
    let mut app = App::new();
    cpu_ground(app.world_mut());
    // A table top 0.75 m above the floor, 2 m ahead.
    cpu_body(
        app.world_mut(),
        [0.0, 0.8, -2.0],
        ColliderShape::Box {
            half_extents: [1.0, 0.05, 1.0],
        },
        RigidBodyKind::Fixed,
    );
    let player = cpu_body(
        app.world_mut(),
        [0.0, 1.0, 0.0],
        DEFAULT_PLAYER_SHAPE,
        RigidBodyKind::Kinematic,
    );
    let camera = app
        .world_mut()
        .spawn((
            Transform {
                position: [0.0, 0.7, 0.0],
                ..Transform::default()
            },
            Camera::default(),
        ))
        .id();
    app.world_mut().entity_mut(player).insert(PlayerController {
        crouch_height: 0.7,
        ..PlayerController::default()
    });
    app.set_parent(camera, player).unwrap();
    let body =
        |app: &App| app.world().get::<Transform>(player).unwrap().position;
    let state =
        |app: &App| *app.world().get::<PlayerController>(player).unwrap();
    let key = |app: &mut App, key, down| {
        app.world_mut()
            .resource_mut::<RuntimeInput>()
            .record_key(key, down);
    };
    run_fixed_steps(&mut app, 60);
    assert!((body(&app)[1] - 0.91).abs() < 0.02, "{:?}", body(&app));

    // Standing, the 1.8 m body stops at the table edge.
    key(&mut app, KeyCode::KeyW, true);
    run_fixed_steps(&mut app, 60);
    assert!(body(&app)[2] > -0.8, "{:?}", body(&app));

    // Crouched to 0.7 m it walks under; feet stay on the floor and the
    // camera drops by the 1.1 m the body lost.
    key(&mut app, KeyCode::KeyC, true);
    run_fixed_steps(&mut app, 40);
    assert!(state(&app).crouched);
    assert!((body(&app)[2] + 2.0).abs() < 0.3, "{:?}", body(&app));
    assert!((body(&app)[1] - 0.36).abs() < 0.02, "{:?}", body(&app));
    app.update(Duration::ZERO).unwrap();
    let eye = app.world().get::<Transform>(camera).unwrap().position[1];
    assert!((eye - 0.15).abs() < 1e-4, "{eye}");

    // Releasing crouch under the table keeps it crouched.
    key(&mut app, KeyCode::KeyW, false);
    key(&mut app, KeyCode::KeyC, false);
    run_fixed_steps(&mut app, 10);
    assert!(state(&app).crouched);

    // Out the other side it stands up again and the camera comes back.
    key(&mut app, KeyCode::KeyW, true);
    run_fixed_steps(&mut app, 50);
    assert!(!state(&app).crouched, "{:?}", body(&app));
    assert!((body(&app)[1] - 0.91).abs() < 0.02, "{:?}", body(&app));
    app.update(Duration::ZERO).unwrap();
    let eye = app.world().get::<Transform>(camera).unwrap().position[1];
    assert!((eye - 0.7).abs() < 1e-4, "{eye}");
}

/// Cost of `GameScene::raycast` per ray, the way a game ray-traces a small
/// camera feed (split-signal F23). Prints the cost; the bound only catches a
/// gross regression.
#[test]
#[ignore = "timing: run on a quiet machine with --release -- --ignored --nocapture"]
fn raycast_cost_per_ray() {
    let mut app = App::new();
    let world = app.world_mut();
    cpu_ground(world);
    // A room of 200 static boxes, like a level's walls and props.
    for index in 0..200 {
        let (x, z) = ((index % 20) as f32 - 9.5, (index / 20) as f32 - 4.5);
        cpu_body(world, [x, 0.5, z], UNIT_BOX, RigidBodyKind::Fixed);
    }
    run_fixed_steps(&mut app, 1);
    let scene = crate::project_runner::GameScene {
        world: app.world_mut(),
    };
    // One 80x45 feed: 3,600 rays fanned down over the room.
    let rays = 3_600;
    let start = std::time::Instant::now();
    let mut hits = 0;
    for ray in 0..rays {
        let (u, v) = ((ray % 80) as f32 / 80.0, (ray / 80) as f32 / 45.0);
        let direction = [u - 0.5, -0.4 - v * 0.5, -1.0];
        hits += usize::from(
            scene.raycast([0.0, 4.0, 8.0], direction, 50.0).is_some(),
        );
    }
    let per_ray = start.elapsed().as_secs_f64() * 1e6 / f64::from(rays);
    println!(
        "raycast: {per_ray:.2} us per ray over 201 colliders, {hits} hits"
    );
    assert!(hits > rays as usize / 2);
    assert!(per_ray < 200.0, "{per_ray:.2} us per ray");

    // Island heights: straight down onto a 256 x 256 heightfield, 131,072
    // triangles, once per metre.
    let mut app = App::new();
    let world = app.world_mut();
    let ground = cpu_body(
        world,
        [0.0; 3],
        ColliderShape::Heightfield,
        RigidBodyKind::Fixed,
    );
    let heights = (0..257)
        .map(|row| {
            (0..257)
                .map(|column| ((row * 7 + column * 13) % 17) as f32 * 0.2)
                .collect()
        })
        .collect();
    world
        .entity_mut(ground)
        .insert(crate::runtime::Heightfield {
            heights,
            spacing: 1.0,
            ..crate::runtime::Heightfield::default()
        });
    run_fixed_steps(&mut app, 1);
    let scene = crate::project_runner::GameScene {
        world: app.world_mut(),
    };
    let start = std::time::Instant::now();
    let mut hits = 0;
    for x in -128..128 {
        for z in -128..128 {
            let from = [x as f32 + 0.5, 50.0, z as f32 + 0.5];
            hits += usize::from(
                scene.raycast(from, [0.0, -1.0, 0.0], 100.0).is_some(),
            );
        }
    }
    let per_ray = start.elapsed().as_secs_f64() * 1e6 / 65_536.0;
    println!("heightfield: {per_ray:.2} us per ray over 131,072 triangles");
    assert_eq!(hits, 65_536);
    assert!(per_ray < 200.0, "{per_ray:.2} us per ray");
}

/// A 20-ball chain held out level from a fixed anchor, each ball 0.3 m
/// from the last, whose tip ball is 50 times heavier than the rest. Returns
/// the anchor and the balls.
fn heavy_tipped_chain(app: &mut App, articulated: bool) -> Vec<Entity> {
    let world = app.world_mut();
    let sphere = |radius| ColliderShape::Sphere { radius };
    let anchor =
        cpu_body(world, [0.0, 10.0, 0.0], sphere(0.05), RigidBodyKind::Fixed);
    if articulated {
        world.entity_mut(anchor).insert(Articulation {});
    }
    let mut chain = vec![anchor];
    for index in 1..=20 {
        let ball = cpu_body(
            world,
            [0.3 * index as f32, 10.0, 0.0],
            sphere(0.1),
            RigidBodyKind::Dynamic,
        );
        if index == 20 {
            world.get_mut::<RigidBody>(ball).unwrap().mass = 50.0;
        }
        world.entity_mut(ball).insert(Joint::new(
            JointKind::BallSocket,
            chain[index - 1],
            [-0.3, 0.0, 0.0],
            [0.0; 3],
        ));
        chain.push(ball);
    }
    chain
}

/// Largest and final link stretch of `chain` over `steps` fixed steps.
fn chain_stretch(app: &mut App, chain: &[Entity], steps: u32) -> (f32, f32) {
    let mut worst = 0.0_f32;
    let mut last = 0.0;
    for _ in 0..steps {
        run_fixed_steps(app, 1);
        last = chain
            .windows(2)
            .map(|pair| (distance(app, pair[0], pair[1]) - 0.3).abs())
            .fold(0.0, f32::max);
        worst = worst.max(last);
    }
    (worst, last)
}

#[test]
fn long_chains_with_a_heavy_tip_hold_with_substeps_or_an_articulation() {
    // One substep stretches this chain by metres while it whips down and
    // leaves it 0.4 m long at rest; eight hold it within centimetres.
    let mut app = App::new();
    app.world_mut().resource_mut::<PhysicsSettings>().substeps = 8;
    let chain = heavy_tipped_chain(&mut app, false);
    let (worst, last) = chain_stretch(&mut app, &chain, 300);
    assert!(worst < 0.02, "substepped chain stretched {worst}");
    assert!(last < 0.002, "substepped chain ended {last} long");

    let mut app = App::new();
    let chain = heavy_tipped_chain(&mut app, true);
    let (worst, _) = chain_stretch(&mut app, &chain, 300);
    assert!(worst < 1e-4, "articulated chain stretched {worst}");
}

#[test]
fn heavy_boxes_rest_on_light_ones_with_substeps() {
    let mut app = App::new();
    app.world_mut().resource_mut::<PhysicsSettings>().substeps = 4;
    let world = app.world_mut();
    cpu_ground(world);
    let light =
        cpu_body(world, [0.0, 0.5, 0.0], UNIT_BOX, RigidBodyKind::Dynamic);
    let heavy =
        cpu_body(world, [0.0, 1.5, 0.0], UNIT_BOX, RigidBodyKind::Dynamic);
    world.get_mut::<RigidBody>(heavy).unwrap().mass = 100.0;
    let position = |app: &App, entity| {
        app.world().get::<Transform>(entity).unwrap().position
    };
    for _ in 0..180 {
        run_fixed_steps(&mut app, 1);
        let (low, high) = (position(&app, light), position(&app, heavy));
        // 100:1 on one substep sinks 6 cm into the light box and slides
        // the stack 0.2 m sideways.
        assert!(high[1] - low[1] > 0.99, "sank to {low:?} {high:?}");
        for drift in [low[0], low[2], high[0], high[2]] {
            assert!(drift.abs() < 0.01, "slid to {low:?} {high:?}");
        }
        // Pairs touching in several substeps send one event per step.
        let events = app.world().resource::<EventQueue<CollisionEvent>>();
        let pairs: Vec<_> = events.iter().map(|e| (e.a, e.b)).collect();
        let unique: std::collections::HashSet<_> = pairs.iter().collect();
        assert_eq!(pairs.len(), unique.len(), "repeated events {pairs:?}");
    }
}

#[test]
fn walking_players_stay_grounded_down_ramps_and_low_steps() {
    // Walks the player toward -Z for one second from `start` over
    // `ground` and returns the steps it spent off the ground and its end.
    let walk = |ground: &dyn Fn(&mut bevy_ecs::world::World), start| {
        let mut app = App::new();
        let world = app.world_mut();
        ground(world);
        let body = cpu_body(
            world,
            start,
            DEFAULT_PLAYER_SHAPE,
            RigidBodyKind::Kinematic,
        );
        world.entity_mut(body).insert(PlayerController::default());
        run_fixed_steps(&mut app, 30);
        app.world_mut()
            .resource_mut::<RuntimeInput>()
            .record_key(KeyCode::KeyW, true);
        let mut airborne = 0;
        for _ in 0..60 {
            run_fixed_steps(&mut app, 1);
            let player = app.world().get::<PlayerController>(body).unwrap();
            airborne += u32::from(!player.grounded);
        }
        (
            airborne,
            app.world().get::<Transform>(body).unwrap().position,
        )
    };
    // A 20 degree ramp rising toward +Z.
    let ramp = |world: &mut bevy_ecs::world::World| {
        let ramp = cpu_body(
            world,
            [0.0, 0.0, 0.0],
            ColliderShape::Box {
                half_extents: [3.0, 0.5, 8.0],
            },
            RigidBodyKind::Fixed,
        );
        world.get_mut::<Transform>(ramp).unwrap().rotation =
            [-20_f32.to_radians(), 0.0, 0.0];
    };
    let (airborne, end) = walk(&ramp, [0.0, 3.0, 3.0]);
    assert!(end[2] < 0.0, "did not walk down: {end:?}");
    assert_eq!(airborne, 0, "left the ramp, ended at {end:?}");

    // A 0.2 m step down from a platform to the floor.
    let step = |world: &mut bevy_ecs::world::World| {
        cpu_ground(world);
        cpu_body(
            world,
            [0.0, 0.1, 2.0],
            ColliderShape::Box {
                half_extents: [2.0, 0.1, 2.0],
            },
            RigidBodyKind::Fixed,
        );
    };
    let (airborne, end) = walk(&step, [0.0, 1.2, 1.0]);
    assert!(end[2] < -1.0, "did not walk off: {end:?}");
    assert_eq!(airborne, 0, "fell off the step, ended at {end:?}");

    // A 1 m ledge is past the snap reach, so the player falls off it.
    let ledge = |world: &mut bevy_ecs::world::World| {
        cpu_ground(world);
        cpu_body(
            world,
            [0.0, 0.5, 2.0],
            ColliderShape::Box {
                half_extents: [2.0, 0.5, 2.0],
            },
            RigidBodyKind::Fixed,
        );
    };
    let (airborne, end) = walk(&ledge, [0.0, 2.0, 1.0]);
    assert!(airborne > 5, "stuck to the ledge: {airborne} {end:?}");
    assert!(end[1] < 1.0, "did not land below: {end:?}");
}

#[test]
fn dynamic_players_walk_jump_stay_upright_and_shove_by_mass() {
    let mut app = App::new();
    let world = app.world_mut();
    cpu_ground(world);
    let body = cpu_body(
        world,
        [0.0, 0.91, 0.0],
        DEFAULT_PLAYER_SHAPE,
        RigidBodyKind::Dynamic,
    );
    world.get_mut::<RigidBody>(body).unwrap().mass = 80.0;
    world.entity_mut(body).insert(PlayerController::default());
    // A light crate and a heavy one ahead of the player (-Z), side by side.
    let light = cpu_body(
        world,
        [-1.0, 0.25, -2.0],
        ColliderShape::Box {
            half_extents: [0.25; 3],
        },
        RigidBodyKind::Dynamic,
    );
    let heavy = cpu_body(
        world,
        [1.0, 0.25, -2.0],
        ColliderShape::Box {
            half_extents: [0.25; 3],
        },
        RigidBodyKind::Dynamic,
    );
    world.get_mut::<RigidBody>(heavy).unwrap().mass = 2000.0;
    run_fixed_steps(&mut app, 30);
    let player =
        |app: &App| *app.world().get::<PlayerController>(body).unwrap();
    assert!(player(&app).grounded, "never landed");

    // Jump: rises, then lands again.
    let mut input = app.world_mut().resource_mut::<RuntimeInput>();
    input.record_key(KeyCode::Space, true);
    run_fixed_steps(&mut app, 1);
    let mut input = app.world_mut().resource_mut::<RuntimeInput>();
    input.record_key(KeyCode::Space, false);
    input.clear_frame_edges();
    let mut peak = 0.0_f32;
    for _ in 0..90 {
        run_fixed_steps(&mut app, 1);
        peak =
            peak.max(app.world().get::<Transform>(body).unwrap().position[1]);
    }
    assert!(peak > 1.5, "jumped only to {peak}");
    assert!(player(&app).grounded, "did not land");

    // Walk into each crate in turn: the light one is shoved, the heavy one
    // barely moves.
    let at = |app: &App, entity| {
        app.world().get::<Transform>(entity).unwrap().position
    };
    for x in [-1.0, 1.0] {
        app.world_mut().get_mut::<Transform>(body).unwrap().position =
            [x, 0.91, 0.0];
        app.world_mut()
            .get_mut::<RigidBody>(body)
            .unwrap()
            .linear_velocity = [0.0; 3];
        app.world_mut()
            .resource_mut::<RuntimeInput>()
            .record_key(KeyCode::KeyW, true);
        run_fixed_steps(&mut app, 60);
        app.world_mut()
            .resource_mut::<RuntimeInput>()
            .record_key(KeyCode::KeyW, false);
        run_fixed_steps(&mut app, 30);
        let rotation = app.world().get::<Transform>(body).unwrap().rotation;
        assert!(
            rotation[0].abs() < 1e-4 && rotation[2].abs() < 1e-4,
            "tipped over: {rotation:?}"
        );
    }
    assert!(
        at(&app, light)[2] < -3.0,
        "light crate stayed at {:?}",
        at(&app, light)
    );
    assert!(
        at(&app, heavy)[2] > -2.3,
        "heavy crate went to {:?}",
        at(&app, heavy)
    );
}

#[test]
fn a_motorised_sweeper_knocks_a_dynamic_player_away() {
    let mut app = App::new();
    let world = app.world_mut();
    cpu_ground(world);
    let start = [0.0, 0.91, 2.0];
    let body =
        cpu_body(world, start, DEFAULT_PLAYER_SHAPE, RigidBodyKind::Dynamic);
    world.get_mut::<RigidBody>(body).unwrap().mass = 80.0;
    world.entity_mut(body).insert(PlayerController::default());
    // A 20 kg bar on a hinge about world Y, its motor turning it into the
    // player standing 2 m from the pivot. The motor must beat the walk
    // brake (80 kg x 40 m/s² = 3200 N, so 6400 N·m at 2 m): Hammer Run's
    // 5000 N·m sweepers stall against a dynamic player.
    let pivot = [0.0, 0.6, 0.0];
    let bar = cpu_body(
        world,
        pivot,
        ColliderShape::Box {
            half_extents: [2.8, 0.2, 0.2],
        },
        RigidBodyKind::Dynamic,
    );
    let mut rigid = world.get_mut::<RigidBody>(bar).unwrap();
    rigid.mass = 20.0;
    rigid.gravity_scale = 0.0;
    let frame = [0.0, 0.0, std::f32::consts::FRAC_PI_2];
    world.entity_mut(bar).insert(Joint {
        frame,
        target_frame: frame,
        ..Joint::new(
            JointKind::Hinge {
                limit: None,
                spring: None,
                motor: Some(JointMotor {
                    speed: 1.5,
                    max_force: 20000.0,
                }),
            },
            bevy_ecs::entity::Entity::PLACEHOLDER,
            [0.0; 3],
            pivot,
        )
    });
    run_fixed_steps(&mut app, 150);
    let end = app.world().get::<Transform>(body).unwrap().position;
    let moved =
        ((end[0] - start[0]).powi(2) + (end[2] - start[2]).powi(2)).sqrt();
    assert!(moved > 1.0, "knocked only {moved} m, to {end:?}");
    let spin = app.world().get::<RigidBody>(bar).unwrap().angular_velocity;
    assert!(spin[1].abs() > 1.0, "the sweeper stalled: {spin:?}");
}

#[test]
fn ragdolls_generate_from_skinned_joints_and_go_limp() {
    let mut app = App::new();
    let world = app.world_mut();
    cpu_ground(world);
    let hero = world
        .spawn((Name("Hero".into()), Transform::new([0.0, 0.0, 0.0])))
        .id();
    let named = |world: &mut World, name: &str, y: f32, parent| {
        let entity = world
            .spawn((Name(name.into()), Transform::new([0.0, y, 0.0])))
            .id();
        hierarchy::set_parent(world, entity, parent).unwrap();
        entity
    };
    let armature = named(world, "Armature", 0.0, hero);
    let hips = named(world, "Hips", 1.0, armature);
    let spine = named(world, "Spine", 0.3, hips);
    named(world, "Head", 0.4, spine);
    let finger = named(world, "Finger", 0.04, spine);
    named(world, "Tip", 0.03, finger);
    let body = named(world, "Body", 0.0, hero);
    world.entity_mut(body).insert(Skin {
        joints: [
            "Hips",
            "Hips/Spine",
            "Hips/Spine/Head",
            "Hips/Spine/Finger",
            "Hips/Spine/Finger/Tip",
        ]
        .map(|path| format!("../Armature/{path}"))
        .into(),
        inverse_bind: Vec::new(),
    });

    let bones = ragdoll_bones(world, hero, 60.0);
    let found: Vec<_> = bones
        .iter()
        .map(|bone| (bone.path.as_str(), bone.length))
        .collect();
    assert_eq!(
        found,
        [
            ("Armature/Hips", 0.3),
            ("Armature/Hips/Spine", 0.4),
            ("Armature/Hips/Spine/Head", 0.2),
        ],
        "finger bones are too short for bodies"
    );
    let mass: f32 = bones.iter().map(|bone| bone.mass).sum();
    assert!((mass - 60.0).abs() < 1e-3, "masses sum to {mass}");
    assert!(bones[1].mass > bones[0].mass && bones[0].mass > bones[2].mass);

    world.entity_mut(hero).insert(Ragdoll {
        bones,
        command: Some(true),
        ..Ragdoll::default()
    });
    run_fixed_steps(&mut app, 60);
    let state = app.world().get::<RagdollState>(hero).unwrap();
    assert_eq!(state.phase, RagdollPhase::Limp);
    assert_eq!(state.parts.len(), 3);
    // The head started 1.7 m up.
    let end = app
        .world()
        .get::<Transform>(state.parts[2])
        .unwrap()
        .position;
    assert!(end[1] < 1.2, "head body stayed at {end:?}");
}

#[test]
fn tall_box_stacks_settle_and_sleep_with_cached_contacts() {
    let mut app = App::new();
    let world = app.world_mut();
    cpu_ground(world);
    // Ten unit boxes, each dropped 1 cm above the one below.
    let boxes: Vec<_> = (0..10)
        .map(|level| {
            let y = 0.5 + level as f32 * 1.01;
            cpu_body(world, [0.0, y, 0.0], UNIT_BOX, RigidBodyKind::Dynamic)
        })
        .collect();
    run_fixed_steps(&mut app, 300);
    for (level, &entity) in boxes.iter().enumerate() {
        assert!(
            app.world().get::<Sleeping>(entity).is_some(),
            "box {level} never settled"
        );
        let at = app.world().get::<Transform>(entity).unwrap().position;
        // Each contact rests about the penetration slop deep.
        let height = 0.5 + level as f32 * 0.995;
        assert!(
            at[0].hypot(at[2]) < 0.025 && (at[1] - height).abs() < 0.01,
            "box {level} at {at:?}"
        );
    }
}

#[test]
fn a_hit_on_the_bottom_of_a_sleeping_stack_wakes_and_topples_it() {
    let mut app = App::new();
    let world = app.world_mut();
    cpu_ground(world);
    let boxes: Vec<_> = (0..5)
        .map(|level| {
            let y = 0.5 + level as f32;
            cpu_body(world, [0.0, y, 0.0], UNIT_BOX, RigidBodyKind::Dynamic)
        })
        .collect();
    run_fixed_steps(&mut app, 120);
    assert!(boxes
        .iter()
        .all(|&entity| app.world().get::<Sleeping>(entity).is_some()));

    // A heavy ball knocks the bottom box out from under the stack.
    let world = app.world_mut();
    let ball = cpu_body(
        world,
        [-3.0, 0.5, 0.0],
        ColliderShape::Sphere { radius: 0.4 },
        RigidBodyKind::Dynamic,
    );
    let mut body = world.get_mut::<RigidBody>(ball).unwrap();
    body.linear_velocity = [15.0, 0.0, 0.0];
    body.mass = 50.0;
    run_fixed_steps(&mut app, 120);
    let top = app.world().get::<Transform>(boxes[4]).unwrap().position;
    assert!(top[1] < 4.0, "top box stayed up at {top:?}");
}

#[test]
fn bullets_never_tunnel_through_thin_walls_of_any_kind() {
    use crate::assets::{MeshAsset, MeshVertex, PrimitiveShape};
    let bullets = [
        ColliderShape::Sphere { radius: 0.05 },
        ColliderShape::Box {
            half_extents: [0.05; 3],
        },
        ColliderShape::Capsule {
            half_height: 0.1,
            radius: 0.03,
        },
    ];
    let mut tunneled = Vec::new();
    for wall in 0..4 {
        for (b, bullet) in bullets.iter().enumerate() {
            for speed in [60.0_f32, 400.0] {
                for angle in [0.0_f32, 0.5, 1.0] {
                    let mut app = App::new();
                    app.add_plugin(crate::assets::AssetPlugin).unwrap();
                    let (cube, quad, material) = {
                        let mut assets =
                            app.world_mut()
                                .resource_mut::<crate::assets::AssetServer>();
                        let quad = assets.meshes.insert(MeshAsset {
                            vertices: [
                                [-20.0, -20.0],
                                [20.0, -20.0],
                                [20.0, 20.0],
                                [-20.0, 20.0],
                            ]
                            .map(|[x, y]| MeshVertex {
                                position: [x, y, 0.0],
                                ..MeshVertex::default()
                            })
                            .to_vec(),
                            indices: vec![0, 1, 2, 0, 2, 3],
                        });
                        (
                            assets.builtin_primitives[&PrimitiveShape::Cube],
                            quad,
                            assets.fallback_material,
                        )
                    };
                    let world = app.world_mut();
                    world.resource_mut::<PhysicsSettings>().gravity = [0.0; 3];
                    // A 2 cm box, a 2 cm hull, a one-sided triangle quad
                    // facing the bullet, or a heavy dynamic 2 cm plate.
                    let (shape, mesh) = match wall {
                        0 | 3 => (
                            ColliderShape::Box {
                                half_extents: [20.0, 20.0, 0.01],
                            },
                            None,
                        ),
                        1 => (ColliderShape::ConvexMesh, Some(cube)),
                        _ => (ColliderShape::TriangleMesh, Some(quad)),
                    };
                    let target = cpu_body(
                        world,
                        [0.0, 0.0, -5.0],
                        shape,
                        if wall == 3 {
                            RigidBodyKind::Dynamic
                        } else {
                            RigidBodyKind::Fixed
                        },
                    );
                    world.get_mut::<RigidBody>(target).unwrap().mass = 1000.0;
                    if let Some(mesh) = mesh {
                        world.entity_mut(target).insert(MeshRenderer {
                            mesh,
                            material,
                            cast_shadows: false,
                            receive_shadows: false,
                        });
                        if wall == 1 {
                            world.get_mut::<Transform>(target).unwrap().scale =
                                [40.0, 40.0, 0.02];
                        }
                    }
                    let shot = cpu_body(
                        world,
                        [0.0; 3],
                        *bullet,
                        RigidBodyKind::Dynamic,
                    );
                    world.get_mut::<RigidBody>(shot).unwrap().linear_velocity =
                        [speed * angle.sin(), 0.0, -speed * angle.cos()];
                    run_fixed_steps(&mut app, 20);
                    let z =
                        app.world().get::<Transform>(shot).unwrap().position[2]
                            - app
                                .world()
                                .get::<Transform>(target)
                                .unwrap()
                                .position[2]
                            - 5.0;
                    // Stopped at the wall, not short of it or past it.
                    if !(-5.0..-4.7).contains(&z) {
                        tunneled.push((wall, b, speed, angle, z));
                    }
                }
            }
        }
    }
    assert!(
        tunneled.is_empty(),
        "(wall, bullet, speed, angle, z): {tunneled:?}"
    );
}

fn drive_a_car() -> (Vec<String>, Transform) {
    let mut app = App::new();
    let world = app.world_mut();
    cpu_body(
        world,
        [0.0, -0.5, 0.0],
        ColliderShape::Box {
            half_extents: [200.0, 0.5, 200.0],
        },
        RigidBodyKind::Fixed,
    );
    let car = cpu_body(
        world,
        [0.0, 1.0, 0.0],
        ColliderShape::Box {
            half_extents: [0.9, 0.3, 2.0],
        },
        RigidBodyKind::Dynamic,
    );
    world.get_mut::<RigidBody>(car).unwrap().mass = 1200.0;
    world.entity_mut(car).insert(Vehicle::default());
    let mut log = Vec::new();
    let mut phase = |app: &mut App, steps, throttle, brake, steer| {
        let mut vehicle = app.world_mut().get_mut::<Vehicle>(car).unwrap();
        (vehicle.throttle, vehicle.brake, vehicle.steer) =
            (throttle, brake, steer);
        run_fixed_steps(app, steps);
        let world = app.world();
        let at = *world.get::<Transform>(car).unwrap();
        let v = world.get::<RigidBody>(car).unwrap().linear_velocity;
        let vehicle = world.get::<Vehicle>(car).unwrap();
        log.push(format!(
            "at {:?} rot {:?} v {v:?} gear {} grounded {}",
            at.position,
            at.rotation,
            vehicle.gear,
            vehicle.wheels.iter().filter(|w| w.grounded).count()
        ));
        (at, v, vehicle.gear)
    };
    // Settles level on its springs.
    let (at, v, _) = phase(&mut app, 120, 0.0, 0.0, 0.0);
    assert!((at.position[1] - 0.866).abs() < 0.03, "{log:?}");
    assert!(at.rotation[0].abs() < 0.01 && at.rotation[2].abs() < 0.01);
    assert!(v.iter().all(|v| v.abs() < 0.05), "{log:?}");
    // Full throttle drives forward (-Z), straight, and shifts up.
    let (at, _, gear) = phase(&mut app, 300, 1.0, 0.0, 0.0);
    assert!(
        at.position[2] < -30.0 && at.position[0].abs() < 0.5,
        "{log:?}"
    );
    assert!(gear > 0, "{log:?}");
    // Steering right turns the heading toward +X (yaw goes down).
    let (at, ..) = phase(&mut app, 120, 0.3, 0.0, 1.0);
    assert!(at.rotation[1] < -0.5, "{log:?}");
    assert!(at.rotation[0].abs() < 0.2 && at.rotation[2].abs() < 0.2);
    // Brakes stop it.
    let (at, v, _) = phase(&mut app, 300, 0.0, 1.0, 0.0);
    assert!(v[0].hypot(v[2]) < 0.2, "{log:?}");
    (log, at)
}

#[test]
fn raycast_cars_settle_drive_turn_brake_and_replay_exactly() {
    let (_, at) = drive_a_car();
    assert_eq!(drive_a_car().1, at);
}

#[test]
fn physics_materials_combine_friction_and_bounce_and_name_hit_sounds() {
    let material = |name: &str, friction, restitution| PhysicsMaterial {
        name: name.into(),
        friction_combine: friction,
        restitution_combine: restitution,
    };
    // A box sliding at 5 m/s, friction 0.1 against a friction-1 floor.
    let slide = |on_box: Option<PhysicsMaterial>, on_floor| {
        let mut app = App::new();
        let world = app.world_mut();
        let floor = cpu_body(
            world,
            [0.0, -0.5, 0.0],
            ColliderShape::Box {
                half_extents: [50.0, 0.5, 50.0],
            },
            RigidBodyKind::Fixed,
        );
        world.get_mut::<Collider>(floor).unwrap().friction = 1.0;
        let crate_ =
            cpu_body(world, [0.0, 0.5, 0.0], UNIT_BOX, RigidBodyKind::Dynamic);
        world.get_mut::<Collider>(crate_).unwrap().friction = 0.1;
        world.get_mut::<RigidBody>(crate_).unwrap().linear_velocity =
            [5.0, 0.0, 0.0];
        if let Some(material) = on_box {
            world.entity_mut(crate_).insert(material);
        }
        if let Some(material) = on_floor {
            world.entity_mut(floor).insert(material);
        }
        run_fixed_steps(&mut app, 360);
        app.world().get::<Transform>(crate_).unwrap().position[0]
    };
    use CombineMode::{Default as Mean, Max, Min, Multiply};
    let mean = slide(None, None);
    let ice = slide(Some(material("ice", Min, Mean)), None);
    // The floor's Max outranks the box's Min.
    let grippy = slide(
        Some(material("ice", Min, Mean)),
        Some(material("rubber", Max, Mean)),
    );
    // sqrt(0.1) stops it in about 4 m, 0.1 in about 12.7 m, 1.0 in 1.3 m.
    assert!((mean - 4.0).abs() < 0.5, "mean {mean}");
    assert!((ice - 12.7).abs() < 1.0, "ice {ice}");
    assert!((grippy - 1.3).abs() < 0.3, "grippy {grippy}");

    // A bouncy ball on a dead floor: Default bounces, Multiply does not.
    let bounce = |on_floor: PhysicsMaterial, cue: &str| {
        let mut app = App::new();
        let world = app.world_mut();
        let floor = cpu_body(
            world,
            [0.0, -0.5, 0.0],
            ColliderShape::Box {
                half_extents: [5.0, 0.5, 5.0],
            },
            RigidBodyKind::Fixed,
        );
        world.entity_mut(floor).insert(on_floor);
        let ball = cpu_body(
            world,
            [0.0, 2.0, 0.0],
            ColliderShape::Sphere { radius: 0.5 },
            RigidBodyKind::Dynamic,
        );
        world.get_mut::<Collider>(ball).unwrap().restitution = 0.9;
        world.entity_mut(ball).insert(SoundCue {
            clip: "sfx/clang.wav".into(),
            with_material: cue.into(),
            ..SoundCue::default()
        });
        let mut top = 0.0_f32;
        let mut sounds = 0;
        let mut landed = false;
        for _ in 0..120 {
            run_fixed_steps(&mut app, 1);
            let y = app.world().get::<Transform>(ball).unwrap().position[1];
            landed |= y < 0.6;
            if landed {
                top = top.max(y);
            }
            sounds += app
                .world()
                .resource::<EventQueue<SoundEvent>>()
                .iter()
                .count();
        }
        (top, sounds)
    };
    let (high, metal_sounds) = bounce(material("metal", Mean, Mean), "metal");
    let (low, wood_sounds) = bounce(material("wood", Mean, Multiply), "metal");
    assert!(high > 1.5, "bounced to {high}");
    assert!(low < 0.6, "dead floor bounced to {low}");
    assert!(metal_sounds > 0, "a metal hit sounds");
    assert_eq!(wood_sounds, 0, "a wood hit stays silent");
}

#[test]
fn axis_locks_keep_a_2d_body_in_its_plane_on_a_sideways_slope() {
    let mut app = App::new();
    let world = app.world_mut();
    let slick = |world: &mut bevy_ecs::world::World, entity| {
        world.get_mut::<Collider>(entity).unwrap().friction = 0.0;
    };
    // Two ramps that fall away toward z, and a box resting on each.
    let on_ramp = |world: &mut bevy_ecs::world::World, x: f32| {
        let ramp = cpu_body(
            world,
            [x, 0.0, 0.0],
            ColliderShape::Box {
                half_extents: [2.0, 0.25, 4.0],
            },
            RigidBodyKind::Fixed,
        );
        let crate_ =
            cpu_body(world, [x, 0.76, 0.0], UNIT_BOX, RigidBodyKind::Dynamic);
        for entity in [ramp, crate_] {
            world.get_mut::<Transform>(entity).unwrap().rotation =
                [0.4, 0.0, 0.0];
            slick(world, entity);
        }
        crate_
    };
    let locked = on_ramp(world, 0.0);
    world.entity_mut(locked).insert(AxisLock::PLANE_XY);
    let free = on_ramp(world, 10.0);
    cpu_body(
        world,
        [20.0, -0.5, 0.0],
        ColliderShape::Box {
            half_extents: [10.0, 0.5, 10.0],
        },
        RigidBodyKind::Fixed,
    );
    let ball = cpu_body(
        world,
        [20.0, 0.5, 0.0],
        ColliderShape::Sphere { radius: 0.5 },
        RigidBodyKind::Dynamic,
    );
    world.entity_mut(ball).insert(AxisLock::PLANE_XY);
    world.get_mut::<RigidBody>(ball).unwrap().linear_velocity = [2.0, 0.0, 3.0];
    run_fixed_steps(&mut app, 60);
    let pose = |entity| *app.world().get::<Transform>(entity).unwrap();
    assert!(pose(free).position[2].abs() > 0.5, "{:?}", pose(free));
    let held = pose(locked);
    assert_eq!(held.position[2], 0.0, "{held:?}");
    assert!((held.rotation[0] - 0.4).abs() < 1e-3, "{held:?}");
    assert!(held.position[1] > 0.5, "still on its ramp {held:?}");
    let rolled = pose(ball);
    assert_eq!(rolled.position[2], 0.0, "{rolled:?}");
    assert!(rolled.position[0] > 21.0, "{rolled:?}");
    let spin = app.world().get::<RigidBody>(ball).unwrap().angular_velocity;
    assert_eq!([spin[0], spin[1]], [0.0, 0.0], "turns only about z");
}

#[test]
fn polygon_bodies_slide_down_a_chain_valley_and_settle_at_its_bottom() {
    let mut app = App::new();
    let world = app.world_mut();
    let outline = |points: &[[f32; 2]], closed| Polygon2d {
        points: points.to_vec(),
        depth: 1.0,
        closed,
    };
    let valley = cpu_body(
        world,
        [0.0, 0.0, 0.0],
        ColliderShape::Chain,
        RigidBodyKind::Fixed,
    );
    world
        .entity_mut(valley)
        .insert(outline(&[[-6.0, 6.0], [0.0, 0.0], [6.0, 6.0]], false));
    // A chain on a dynamic body never moves: it is static only.
    let ledge = cpu_body(
        world,
        [20.0, 3.0, 0.0],
        ColliderShape::Chain,
        RigidBodyKind::Dynamic,
    );
    world
        .entity_mut(ledge)
        .insert(outline(&[[-1.0, 0.0], [1.0, 0.0]], false));
    let crate_ = cpu_body(
        world,
        [-3.0, 4.5, 0.0],
        ColliderShape::Polygon,
        RigidBodyKind::Dynamic,
    );
    let square = [[-0.5, -0.5], [0.5, -0.5], [0.5, 0.5], [-0.5, 0.5]];
    world
        .entity_mut(crate_)
        .insert((outline(&square, true), AxisLock::PLANE_XY));
    run_fixed_steps(&mut app, 300);
    let pose = |entity| *app.world().get::<Transform>(entity).unwrap();
    let settled = pose(crate_);
    assert!(settled.position[0].abs() < 1.0, "{settled:?}");
    assert!(
        (0.5..1.3).contains(&settled.position[1]),
        "rests in the valley, not through it {settled:?}"
    );
    assert_eq!(settled.position[2], 0.0, "{settled:?}");
    assert_eq!(pose(ledge).position, [20.0, 3.0, 0.0]);
}

#[test]
fn ropes_hold_weights_go_slack_and_tow_bodies() {
    let mut app = App::new();
    let world = app.world_mut();
    let cube = ColliderShape::Box {
        half_extents: [0.25; 3],
    };
    // A 5 kg weight on a 3 m rope from a world point.
    let weight = cpu_body(world, [0.0, 4.0, 0.0], cube, RigidBodyKind::Dynamic);
    world.get_mut::<RigidBody>(weight).unwrap().mass = 5.0;
    world.entity_mut(weight).insert(Rope {
        target_anchor: [0.0, 6.0, 0.0],
        length: 3.0,
        ..Rope::default()
    });
    // A tug drives off at 2 m/s with a crate on a 2 m rope behind it.
    cpu_body(
        world,
        [20.0, -0.5, 0.0],
        ColliderShape::Box {
            half_extents: [20.0, 0.5, 5.0],
        },
        RigidBodyKind::Fixed,
    );
    let tug =
        cpu_body(world, [12.0, 0.25, 0.0], cube, RigidBodyKind::Kinematic);
    world.get_mut::<RigidBody>(tug).unwrap().linear_velocity = [2.0, 0.0, 0.0];
    let towed =
        cpu_body(world, [11.0, 0.25, 0.0], cube, RigidBodyKind::Dynamic);
    world.entity_mut(towed).insert(Rope {
        target: tug,
        length: 2.0,
        ..Rope::default()
    });
    run_fixed_steps(&mut app, 180);
    let at = |app: &App, entity| {
        app.world().get::<Transform>(entity).unwrap().position
    };
    let hanging = at(&app, weight);
    assert!(
        (2.7..3.1).contains(&hanging[1]) && hanging[0].abs() < 0.05,
        "hangs at the rope's length below the hook {hanging:?}"
    );
    let beads = &app.world().get::<RopeState>(weight).unwrap().beads;
    assert_eq!(beads.len(), 8);
    assert!(app.world().get::<RopeBead>(beads[0]).is_some());
    let (tug_at, crate_at) = (at(&app, tug), at(&app, towed));
    assert!(crate_at[0] > 13.0, "towed {crate_at:?}");
    assert!(
        (1.7..2.1).contains(&(tug_at[0] - crate_at[0])),
        "follows at the rope's length {tug_at:?} {crate_at:?}"
    );
    // A rope never pushes: kicked up, the weight rises past the slack.
    app.world_mut()
        .get_mut::<RigidBody>(weight)
        .unwrap()
        .linear_velocity = [0.0, 4.0, 0.0];
    run_fixed_steps(&mut app, 12);
    assert!(
        at(&app, weight)[1] > hanging[1] + 0.5,
        "{:?}",
        at(&app, weight)
    );
}

#[test]
fn a_2d_pin_joint_swings_in_plane_and_rays_hit_polygons_and_chains() {
    let mut app = App::new();
    let world = app.world_mut();
    let floor = cpu_body(
        world,
        [0.0, 0.0, 0.0],
        ColliderShape::Chain,
        RigidBodyKind::Fixed,
    );
    world.entity_mut(floor).insert(Polygon2d {
        points: vec![[-5.0, -3.0], [5.0, -3.0]],
        depth: 1.0,
        closed: false,
    });
    let wedge = cpu_body(
        world,
        [4.0, -1.0, 0.0],
        ColliderShape::Polygon,
        RigidBodyKind::Fixed,
    );
    world.entity_mut(wedge).insert(Polygon2d::default());
    // A plank pinned at its left end: a ball socket on a body whose
    // angular x and y are locked is Godot's PinJoint2D.
    let plank = cpu_body(
        world,
        [1.0, 3.0, 0.0],
        ColliderShape::Polygon,
        RigidBodyKind::Dynamic,
    );
    world.entity_mut(plank).insert((
        Polygon2d {
            points: vec![[-1.0, -0.1], [1.0, -0.1], [1.0, 0.1], [-1.0, 0.1]],
            ..Polygon2d::default()
        },
        AxisLock::PLANE_XY,
        Joint::new(
            JointKind::BallSocket,
            bevy_ecs::entity::Entity::PLACEHOLDER,
            [-1.0, 0.0, 0.0],
            [0.0, 3.0, 0.0],
        ),
    ));
    let mut lowest = f32::MAX;
    for _ in 0..90 {
        run_fixed_steps(&mut app, 1);
        let pose = *app.world().get::<Transform>(plank).unwrap();
        let arm = [pose.position[0], pose.position[1] - 3.0];
        assert!((arm[0].hypot(arm[1]) - 1.0).abs() < 0.05, "{pose:?}");
        assert_eq!(pose.position[2], 0.0, "{pose:?}");
        lowest = lowest.min(pose.position[1]);
    }
    assert!(lowest < 2.1, "swung down: {lowest}");
    let spin = app
        .world()
        .get::<RigidBody>(plank)
        .unwrap()
        .angular_velocity;
    assert_eq!([spin[0], spin[1]], [0.0, 0.0]);

    let physics = app.world().resource::<PhysicsWorld>();
    let down = physics
        .raycast([3.0, 5.0, 0.0], [0.0, -1.0, 0.0], 20.0, u32::MAX)
        .unwrap();
    assert_eq!(down.entity, floor);
    assert!((down.distance - 8.0).abs() < 1e-4, "{down:?}");
    assert!((down.normal[1].abs() - 1.0).abs() < 1e-4, "{down:?}");
    let side = physics
        .raycast([8.0, -1.0, 0.0], [-1.0, 0.0, 0.0], 20.0, u32::MAX)
        .unwrap();
    assert_eq!(side.entity, wedge);
    assert!((side.distance - 3.75).abs() < 1e-4, "{side:?}");
    assert!(side.normal[0] > 0.5, "{side:?}");
    assert_eq!(
        physics.overlap_sphere([4.0, -1.0, 0.0], 0.01, u32::MAX),
        [wedge]
    );
}

#[test]
fn a_fluid_surface_reaches_rendering_only_through_extraction() {
    let mut app = App::new();
    app.add_plugin(crate::assets::AssetPlugin).unwrap();
    app.add_plugin(RenderExtractPlugin).unwrap();
    let material = app
        .world()
        .resource::<crate::assets::AssetServer>()
        .fallback_material;
    let settings = FluidSettings::default();
    let fluid = Fluid::block([0.0, 1.0, 0.0], [3, 3, 3], settings.spacing);
    let mut volume = FluidVolume::new(settings, fluid);
    volume.surface = Some(crate::runtime::FluidSurface::new(material));
    app.world_mut().spawn(volume);
    run_fixed_steps(&mut app, 2);
    // The renderer sees one extracted renderable: a mesh asset and a
    // material, not the particle buffers.
    let extracted = app.world().resource::<RenderWorld>().renderables.clone();
    assert_eq!(extracted.len(), 1);
    let mesh = extracted[0].mesh;
    assert_eq!(extracted[0].material, material);
    let revision = |app: &App| {
        app.world()
            .resource::<crate::assets::AssetServer>()
            .meshes
            .revision(mesh)
            .unwrap()
    };
    let before = revision(&app);
    run_fixed_steps(&mut app, 5);
    // The mesh asset is rewritten in place, so the renderer re-uploads it by
    // its revision while the extracted handle stays the same.
    assert!(revision(&app) > before);
    let extracted = &app.world().resource::<RenderWorld>().renderables;
    assert_eq!(extracted.len(), 1);
    assert_eq!(extracted[0].mesh, mesh);
}

#[test]
fn a_streaming_budget_keeps_the_nearest_gpu_bodies_and_saves_the_rest() {
    let mut app = App::new();
    app.add_plugin(HybridPhysicsPlugin).unwrap();
    app.insert_resource(GpuStreamingBudget { max_resident: 1 });
    let observer = app.spawn((Transform::default(), SimulationObserver));
    let ball = |app: &mut App, x: f32| {
        app.spawn((
            Transform::default().with_position(x, 0.0, 0.0),
            PhysicsBody {
                simulation: SimulationClass::Gpu,
                ..PhysicsBody::default()
            },
            RigidBody::default(),
            PhysicsSyncMode::SelectedState,
        ))
    };
    let near = ball(&mut app, 1.0);
    let far = ball(&mut app, 10.0);
    app.update(Duration::ZERO).unwrap();
    // The GPU has moved the far ball on since it was authored.
    let id = *app.world().get::<PhysicsId>(far).unwrap();
    apply_gpu_state_samples(
        app.world_mut(),
        &[GpuStateSample {
            physics_id: id,
            tick: 1,
            transform: Transform::default().with_position(12.0, 0.0, 0.0),
            linear_velocity: [3.0, 0.0, 0.0],
            angular_velocity: [0.0, 1.0, 0.0],
            custom_values: None,
        }],
    );
    let step = app.world().resource::<TimeControl>().fixed_delta;
    app.update(step).unwrap();

    let world = app.world();
    assert!(!world.entity(near).contains::<StreamedOut>());
    assert!(world.entity(far).contains::<StreamedOut>());
    assert!(!world.entity(far).contains::<GpuStateMirror>());
    assert_eq!(
        world.get::<Transform>(far).unwrap().position,
        [12.0, 0.0, 0.0]
    );
    let rigid = world.get::<RigidBody>(far).unwrap();
    assert_eq!(rigid.linear_velocity, [3.0, 0.0, 0.0]);
    assert_eq!(rigid.angular_velocity, [0.0, 1.0, 0.0]);
    let resident: Vec<_> =
        super::hybrid_physics::extract_gpu_physics_bodies(app.world_mut())
            .iter()
            .map(|body| body.entity)
            .collect();
    assert_eq!(resident, [near]);

    // The observer walks over: the far ball comes back from its saved
    // state and the near one leaves.
    app.world_mut()
        .get_mut::<Transform>(observer)
        .unwrap()
        .position = [12.0, 0.0, 0.0];
    // Its world position propagates after the fixed step: one tick late.
    app.update(step).unwrap();
    app.update(step).unwrap();
    let world = app.world();
    assert!(world.entity(near).contains::<StreamedOut>());
    assert!(!world.entity(far).contains::<StreamedOut>());
    let resident: Vec<_> =
        super::hybrid_physics::extract_gpu_physics_bodies(app.world_mut())
            .iter()
            .map(|body| (body.entity, body.transform.position))
            .collect();
    assert_eq!(resident, [(far, [12.0, 0.0, 0.0])]);
}
