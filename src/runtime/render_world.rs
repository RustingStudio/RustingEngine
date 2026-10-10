//! Renderer-facing snapshot extracted from canonical gameplay ECS state.

use std::collections::HashMap;
use std::hash::{Hash, Hasher};

use bevy_ecs::change_detection::{DetectChanges, Tick};
use bevy_ecs::entity::Entity;
use bevy_ecs::prelude::{Component, Or, Ref, Resource, With, Without, World};

use crate::assets::{Handle, MaterialAsset, MeshAsset};

use super::{
    AmbientLight, App, AppError, Camera, DirectionalLight, GlobalTransform,
    MeshRenderer, Plugin, PointLight, Projection, RenderBounds, ScheduleStage,
    SkyLight, SpotLight, ToneMapping, Visibility,
};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ExtractedRenderable {
    pub entity: Entity,
    pub transform: GlobalTransform,
    pub mesh: Handle<MeshAsset>,
    pub material: Handle<MaterialAsset>,
    pub cast_shadows: bool,
    pub receive_shadows: bool,
    /// The entity's local-space `RenderBounds` override. Without one the
    /// renderer culls with the box around the mesh it actually draws, so
    /// bounds follow mesh loads and reloads.
    pub bounds: Option<RenderBounds>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ExtractedCamera {
    pub entity: Entity,
    pub transform: GlobalTransform,
    pub projection: Projection,
    pub priority: i32,
}

/// A [`CameraScreen`](super::CameraScreen) ready to draw: the camera's image
/// replaces the maps of `material`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ExtractedScreen {
    pub material: crate::assets::Handle<crate::assets::MaterialAsset>,
    pub camera: ExtractedCamera,
    pub size: [u32; 2],
    /// The screen's own mesh and placement, to skip feeds nobody sees.
    pub mesh: Handle<MeshAsset>,
    pub transform: GlobalTransform,
    pub update_every: u32,
    pub enabled: bool,
    /// The feed's own color grading; `None` uses the scene's.
    pub grading: Option<super::ColorGrading>,
    /// Multiplies the scene's tone mapping exposure for this feed.
    pub exposure: f32,
}

/// Optional camera selected by a tool such as the editor Scene viewport.
/// Runtime Game views leave this empty and use the highest-priority active
/// gameplay camera.
#[derive(Resource, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RenderCameraOverride {
    pub entity: Option<Entity>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ExtractedDirectionalLight {
    pub entity: Entity,
    pub transform: GlobalTransform,
    pub light: DirectionalLight,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ExtractedPointLight {
    pub entity: Entity,
    pub transform: GlobalTransform,
    pub light: PointLight,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ExtractedSpotLight {
    pub entity: Entity,
    pub transform: GlobalTransform,
    pub light: SpotLight,
    /// The loaded image of a [`LightCookie`](super::LightCookie) on the
    /// same entity.
    pub cookie: Option<crate::assets::Handle<crate::assets::TextureAsset>>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ExtractionReport {
    pub added: usize,
    pub changed: usize,
    pub removed: usize,
    pub total: usize,
}

/// One [`super::GpuConditionShader`] as the renderer needs it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ExtractedConditionShader {
    /// GLSL after [`super::GpuConditionShader::resolve`].
    pub source: String,
    pub params: Vec<[f32; 4]>,
    /// At least 1.
    pub events_per_body: u32,
}

impl From<String> for ExtractedConditionShader {
    fn from(source: String) -> Self {
        Self {
            source,
            params: Vec::new(),
            events_per_body: 1,
        }
    }
}

/// Data consumed by the renderer, separate from the gameplay world.
#[derive(Resource, Default, Clone)]
pub struct RenderWorld {
    pub renderables: Vec<ExtractedRenderable>,
    /// Changes only when the extracted object list or one of its transforms
    /// changes. The renderer uses this instead of comparing every object.
    pub renderables_revision: u64,
    /// Changes when objects are added, removed, hidden or change mesh,
    /// material or bounds, but not when they only move. Mesh preparation
    /// keys on this so moving objects skip it.
    pub renderables_layout_revision: u64,
    pub active_camera: Option<ExtractedCamera>,
    /// Active cameras with a viewport (`[x, y, width, height]` fractions
    /// of the target), lowest priority first. They draw over the active
    /// camera, which is one of them when no active camera fills the
    /// window. Empty while a tool overrides the camera.
    pub views: Vec<(ExtractedCamera, [f32; 4])>,
    pub directional_lights: Vec<ExtractedDirectionalLight>,
    pub point_lights: Vec<ExtractedPointLight>,
    pub spot_lights: Vec<ExtractedSpotLight>,
    pub ambient_light: Option<AmbientLight>,
    pub sky_light: Option<SkyLight>,
    /// Loaded texture, intensity and background flag of the scene's first
    /// loaded [`EnvironmentMap`](super::EnvironmentMap).
    pub environment: Option<(
        crate::assets::Handle<crate::assets::TextureAsset>,
        f32,
        bool,
    )>,
    /// World position and settings of each
    /// [`ReflectionProbe`](super::ReflectionProbe), in entity order.
    pub reflection_probes: Vec<([f32; 3], super::ReflectionProbe)>,
    pub tone_mapping: Option<ToneMapping>,
    pub fog: Option<super::Fog>,
    pub procedural_sky: Option<super::ProceduralSky>,
    /// Box-to-world matrix (transform times half size) and settings of
    /// each [`FogVolume`](super::FogVolume), in entity order.
    pub fog_volumes: Vec<([[f32; 4]; 4], super::FogVolume)>,
    /// Box-to-world matrix (transform times half size) and settings of
    /// each [`Decal`](super::Decal) whose image is loaded or that has none,
    /// in entity order.
    pub decals: Vec<([[f32; 4]; 4], super::Decal)>,
    pub bloom: Option<super::Bloom>,
    pub auto_exposure: Option<super::AutoExposure>,
    pub depth_of_field: Option<super::DepthOfField>,
    pub motion_blur: Option<super::MotionBlur>,
    pub color_grading: Option<super::ColorGrading>,
    /// Loaded image and intensity of the first loaded
    /// [`ColorLut`](super::ColorLut).
    pub color_lut:
        Option<(crate::assets::Handle<crate::assets::TextureAsset>, f32)>,
    /// Meshes that show a camera's image, in entity order.
    pub screens: Vec<ExtractedScreen>,
    pub ambient_occlusion: Option<super::AmbientOcclusion>,
    /// Live particles of each visible emitter, in entity order; each batch
    /// is one instanced draw.
    pub particles: Vec<super::ParticleBatch>,
    pub lights_revision: u64,
    pub report: ExtractionReport,
    /// Bodies whose newest runtime transforms will be owned by GPU compute.
    pub gpu_physics: Vec<super::ExtractedGpuPhysicsBody>,
    /// Signature of a rule-free GPU body set reused between render frames.
    pub gpu_physics_signature: Option<u64>,
    /// Changes only when CPU data used to create GPU physics buffers changes.
    pub gpu_physics_revision: u64,
    /// Commands taken from [`super::GpuPhysicsCommands`] by the last
    /// extraction that found any.
    pub gpu_physics_commands: Vec<(super::PhysicsId, super::GpuBodyCommand)>,
    /// GPU tick each command applies before, parallel to
    /// `gpu_physics_commands`; a missing entry means the next GPU tick.
    pub gpu_physics_command_ticks: Vec<u64>,
    /// Whether the batch asks for every body's state; see
    /// [`super::GpuPhysicsCommands::read_all_states`].
    pub gpu_physics_read_all: bool,
    /// Whether the batch restarts GPU bodies from their authored state; see
    /// [`super::GpuPhysicsCommands::reset_to_authored`].
    pub gpu_physics_reset: bool,
    /// Bumps with each new batch in `gpu_physics_commands`, so a renderer
    /// that draws twice without an extraction does not apply it twice.
    pub gpu_physics_commands_serial: u64,
    /// Solid CPU and static colliders from the last CPU physics step, for
    /// GPU bodies to collide against.
    pub gpu_colliders: Vec<super::GpuCollider>,
    /// Force-field sensors from the last CPU physics step, which push GPU
    /// bodies too.
    pub gpu_force_fields: Vec<super::GpuForceField>,
    /// Resolved [`super::GpuConditionShader`]s, in dispatch order.
    pub gpu_condition_shaders: Vec<ExtractedConditionShader>,
    /// Wrapped `PhysicsSolver::Custom` hook files, one per distinct path,
    /// dispatched before the condition shaders. Refreshed only when the GPU
    /// bodies are re-extracted.
    pub gpu_solver_shaders: Vec<String>,
    pub physics_tick: u64,
    pub fixed_delta_seconds: f32,
    pub elapsed_seconds: f32,
    pub physics_gravity: [f32; 3],
    /// `AxisLock::mask` of `PhysicsSettings::gpu_locks`.
    pub physics_lock_mask: u32,
    pub physics_enabled: bool,
    pub background_color: [f32; 4],
    /// Requested profile; the renderer resolves `Auto` from device capabilities.
    pub quality: super::QualityProfile,
    pub culling: super::CullingMode,
    pub antialiasing: super::Antialiasing,
    pub shadows: super::ShadowQuality,
    /// From `RenderSettings::hard_shadows`.
    pub hard_shadows: bool,
    /// Set from `RenderSettings::reflections` being off; the default keeps
    /// reflections on.
    pub reflections_disabled: bool,
    /// Drawn tint of each flashing renderable, sorted by entity: rgb from
    /// its own or an ancestor's [`super::Flash`], a the strength.
    pub flashes: Vec<(Entity, [f32; 4])>,
    /// Index of each entity in `renderables`.
    index: HashMap<Entity, usize>,
    renderables_signature: Option<u64>,
    /// Tick and component counts seen by the last signature hash.
    renderables_fingerprint: Option<(Tick, [usize; 6])>,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct RenderExtractPlugin;

impl Plugin for RenderExtractPlugin {
    fn build(&self, app: &mut App) -> Result<(), AppError> {
        app.insert_resource(RenderWorld::default())
            .insert_resource(RenderCameraOverride::default())
            .add_systems(ScheduleStage::RenderExtract, extract_render_world);
        Ok(())
    }
}

pub fn extract_render_world(world: &mut World) {
    super::update_skins(world);
    // Most game frames do not add objects or change their CPU transforms.
    // Hashing in place is much cheaper than allocating and sorting a new list
    // of ten thousand objects only to discover that nothing changed.
    let previous_renderables_signature =
        world.resource::<RenderWorld>().renderables_signature;
    let mut moved = Vec::new();
    let renderables_signature = match renderables_changed(world) {
        RenderablesChange::None => previous_renderables_signature,
        RenderablesChange::Rebuild => Some(renderables_signature(world)),
        // Objects that only moved update in place. The signature goes
        // stale, so the next real change rebuilds.
        RenderablesChange::Moved(entities) => {
            moved = entities
                .into_iter()
                .filter_map(|entity| {
                    Some((entity, *world.get::<GlobalTransform>(entity)?))
                })
                .collect();
            None
        }
    };
    let renderables = (moved.is_empty()
        && previous_renderables_signature != renderables_signature)
        .then(|| collect_renderables(world));
    let mut active_camera = collect_active_camera(world);
    let mut views = collect_views(world);
    let seconds = world.resource::<super::FrameTime>().elapsed.as_secs_f32();
    for camera in active_camera
        .iter_mut()
        .chain(views.iter_mut().map(|(camera, _)| camera))
    {
        if let Some(shake) = world.get::<super::CameraShake>(camera.entity) {
            camera.transform.matrix =
                shake.apply(camera.transform.matrix, seconds);
        }
    }
    let directional_lights = collect_directional_lights(world);
    let point_lights = collect_point_lights(world);
    let spot_lights = collect_spot_lights(world);
    let ambient_light = collect_first::<AmbientLight>(world);
    let sky_light = collect_first::<SkyLight>(world);
    let environment = {
        let mut query = world.query::<(Entity, &super::EnvironmentMap)>();
        query
            .iter(world)
            .filter_map(|(entity, map)| {
                Some((
                    entity.index(),
                    map.handle?,
                    map.intensity,
                    map.background,
                ))
            })
            .min_by_key(|(entity, ..)| *entity)
            .map(|(_, handle, intensity, background)| {
                (handle, intensity, background)
            })
    };
    let reflection_probes = {
        let mut query =
            world
                .query::<(Entity, &GlobalTransform, &super::ReflectionProbe)>();
        let mut probes = query
            .iter(world)
            .map(|(entity, transform, probe)| {
                let [x, y, z, _] = transform.matrix[3];
                (entity.index(), [x, y, z], *probe)
            })
            .collect::<Vec<_>>();
        probes.sort_by_key(|(entity, ..)| *entity);
        probes
            .into_iter()
            .map(|(_, center, probe)| (center, probe))
            .collect()
    };
    let fog_volumes = {
        let mut query =
            world.query::<(Entity, &GlobalTransform, &super::FogVolume)>();
        let mut volumes = query
            .iter(world)
            .map(|(entity, transform, volume)| {
                let [x, y, z] = volume.extents;
                let matrix = nalgebra::Matrix4::from(transform.matrix)
                    * nalgebra::Matrix4::new_nonuniform_scaling(
                        &nalgebra::Vector3::new(x, y, z),
                    );
                (entity.index(), matrix.into(), *volume)
            })
            .collect::<Vec<_>>();
        volumes.sort_by_key(|(entity, ..)| *entity);
        volumes
            .into_iter()
            .map(|(_, matrix, volume)| (matrix, volume))
            .collect()
    };
    let decals = {
        let mut query =
            world.query::<(Entity, &GlobalTransform, &super::Decal)>();
        let mut decals = query
            .iter(world)
            .filter(|(_, _, decal)| {
                decal.handle.is_some() || decal.texture.as_os_str().is_empty()
            })
            .map(|(entity, transform, decal)| {
                let [x, y, z] = decal.extents;
                let matrix = nalgebra::Matrix4::from(transform.matrix)
                    * nalgebra::Matrix4::new_nonuniform_scaling(
                        &nalgebra::Vector3::new(x, y, z),
                    );
                (entity.index(), matrix.into(), decal.clone())
            })
            .collect::<Vec<_>>();
        decals.sort_by_key(|(entity, ..)| *entity);
        decals
            .into_iter()
            .map(|(_, matrix, decal)| (matrix, decal))
            .collect()
    };
    let tone_mapping = collect_first::<ToneMapping>(world);
    let procedural_sky = collect_first::<super::ProceduralSky>(world);
    let eye = active_camera.as_ref().map(|camera| {
        let [x, y, z, _] = camera.transform.matrix[3];
        [x, y, z]
    });
    let fog = collect_blended(
        world,
        eye,
        super::Fog {
            density: 0.0,
            sky_affect: 0.0,
            ..super::Fog::default()
        },
        super::Fog::lerp,
    );
    let bloom = collect_first::<super::Bloom>(world);
    let auto_exposure = collect_first::<super::AutoExposure>(world);
    let depth_of_field = collect_first::<super::DepthOfField>(world);
    let motion_blur = collect_first::<super::MotionBlur>(world);
    let color_grading = collect_blended(
        world,
        eye,
        super::ColorGrading::DEFAULT,
        super::ColorGrading::lerp,
    );
    let color_lut = {
        let mut query = world.query::<(Entity, &super::ColorLut)>();
        query
            .iter(world)
            .filter_map(|(entity, lut)| {
                Some((entity.index(), lut.handle?, lut.intensity))
            })
            .min_by_key(|(entity, ..)| *entity)
            .map(|(_, handle, intensity)| (handle, intensity))
    };
    let screens = collect_screens(world);
    let ambient_occlusion = collect_first::<super::AmbientOcclusion>(world);
    let particles = collect_particles(world);
    let has_gpu_physics_resources = world
        .contains_resource::<super::PhysicsIdRegistry>()
        && world.contains_resource::<super::GpuEventRegistry>()
        && world.contains_resource::<super::GpuPhysicsClassWatches>();
    super::hybrid_physics::stamp_gpu_commands(world);
    let commands = world
        .get_resource_mut::<super::GpuPhysicsCommands>()
        .map(|mut commands| std::mem::take(&mut *commands))
        .unwrap_or_default();
    // ponytail: resolves every frame; a handful of short strings. Track a
    // revision on the resource if scenes ever carry many large shaders.
    let condition_shaders = world
        .get_resource::<super::GpuConditionShaders>()
        .map(|shaders| shaders.0.clone())
        .unwrap_or_default();
    let condition_shaders =
        match world.get_resource_mut::<super::GpuEventRegistry>() {
            Some(mut registry) => condition_shaders
                .iter()
                .map(|shader| ExtractedConditionShader {
                    source: shader.resolve(&mut registry),
                    params: shader.params.clone(),
                    events_per_body: shader.events_per_body.max(1),
                })
                .collect(),
            None => Vec::new(),
        };
    let gpu_physics_signature = has_gpu_physics_resources
        .then(|| super::hybrid_physics::simple_gpu_physics_signature(world));
    let previous_gpu_signature =
        world.resource::<RenderWorld>().gpu_physics_signature;
    let gpu_physics = if gpu_physics_signature.is_some()
        && gpu_physics_signature == previous_gpu_signature
    {
        None
    } else if has_gpu_physics_resources {
        Some(super::hybrid_physics::extract_gpu_physics_bodies(world))
    } else {
        Some(Vec::new())
    };
    // ponytail: rebuilt every frame from the CPU step; a revision would skip
    // the upload for static-only scenes if colliders ever number thousands.
    let (gpu_colliders, gpu_force_fields) = match world
        .get_resource::<super::PhysicsWorld>()
    {
        Some(physics) if !gpu_physics.as_ref().is_some_and(Vec::is_empty) => {
            (physics.gpu_colliders(), physics.gpu_force_fields(world))
        }
        _ => Default::default(),
    };
    let time = *world.resource::<super::FrameTime>();
    let physics_settings = world.resource::<super::PhysicsSettings>().clone();
    let render_settings = world.resource::<super::RenderSettings>();
    let (
        background_color,
        quality,
        culling,
        antialiasing,
        shadows,
        hard_shadows,
        reflections,
    ) = (
        render_settings.background_color,
        render_settings.quality,
        render_settings.culling,
        render_settings.antialiasing,
        render_settings.shadows,
        render_settings.hard_shadows,
        render_settings.reflections,
    );

    let flashes = collect_flashes(world);
    let mut render_world = world.resource_mut::<RenderWorld>();
    render_world.flashes = flashes;
    match renderables {
        None => {
            // Moving objects keep their place in the sorted list, since the
            // order does not depend on the transform. GPU-owned effects
            // often rewrite transforms unchanged; those bump nothing.
            let render_world = &mut *render_world;
            let mut changed = 0;
            for (entity, transform) in moved {
                let Some(&at) = render_world.index.get(&entity) else {
                    continue;
                };
                let renderable = &mut render_world.renderables[at];
                if renderable.transform != transform {
                    renderable.transform = transform;
                    changed += 1;
                }
            }
            render_world.report = ExtractionReport {
                changed,
                total: render_world.renderables.len(),
                ..ExtractionReport::default()
            };
            if changed > 0 {
                render_world.renderables_revision =
                    render_world.renderables_revision.wrapping_add(1);
            }
        }
        Some(renderables) => {
            let mut added = 0;
            let mut changed = 0;
            for renderable in &renderables {
                match render_world.index.get(&renderable.entity) {
                    None => added += 1,
                    Some(&at)
                        if render_world.renderables[at] != *renderable =>
                    {
                        changed += 1;
                    }
                    Some(_) => {}
                }
            }
            let removed =
                render_world.renderables.len() + added - renderables.len();
            render_world.index = renderables
                .iter()
                .enumerate()
                .map(|(at, renderable)| (renderable.entity, at))
                .collect();
            render_world.report = ExtractionReport {
                added,
                changed,
                removed,
                total: renderables.len(),
            };
            render_world.renderables = renderables;
            render_world.renderables_revision =
                render_world.renderables_revision.wrapping_add(1);
            render_world.renderables_layout_revision =
                render_world.renderables_layout_revision.wrapping_add(1);
        }
    }
    render_world.renderables_signature = renderables_signature;
    render_world.active_camera = active_camera;
    render_world.views = views;
    render_world.tone_mapping = tone_mapping;
    render_world.fog = fog;
    render_world.procedural_sky = procedural_sky;
    render_world.fog_volumes = fog_volumes;
    render_world.decals = decals;
    render_world.bloom = bloom;
    render_world.auto_exposure = auto_exposure;
    render_world.depth_of_field = depth_of_field;
    render_world.motion_blur = motion_blur;
    render_world.color_grading = color_grading;
    render_world.color_lut = color_lut;
    render_world.screens = screens;
    render_world.ambient_occlusion = ambient_occlusion;
    render_world.particles = particles;
    render_world.environment = environment;
    render_world.reflection_probes = reflection_probes;
    if render_world.directional_lights != directional_lights
        || render_world.point_lights != point_lights
        || render_world.spot_lights != spot_lights
        || render_world.ambient_light != ambient_light
        || render_world.sky_light != sky_light
    {
        render_world.lights_revision =
            render_world.lights_revision.wrapping_add(1);
        render_world.directional_lights = directional_lights;
        render_world.point_lights = point_lights;
        render_world.spot_lights = spot_lights;
        render_world.ambient_light = ambient_light;
        render_world.sky_light = sky_light;
    }
    // A changed signature can still extract identical bodies (a tick bumped
    // without an edit). Bump the revision only on a real change; each bump
    // makes the renderer rebuild its physics tables.
    if let Some(gpu_physics) =
        gpu_physics.filter(|bodies| *bodies != render_world.gpu_physics)
    {
        render_world.gpu_solver_shaders = custom_solver_shaders(&gpu_physics);
        render_world.gpu_physics = gpu_physics;
        render_world.gpu_physics_revision =
            render_world.gpu_physics_revision.wrapping_add(1);
    }
    render_world.gpu_physics_signature = gpu_physics_signature;
    if !commands.commands.is_empty()
        || commands.read_all_states
        || commands.reset_to_authored
    {
        render_world.gpu_physics_commands = commands.commands;
        render_world.gpu_physics_command_ticks = commands.apply_ticks;
        render_world.gpu_physics_read_all = commands.read_all_states;
        render_world.gpu_physics_reset = commands.reset_to_authored;
        render_world.gpu_physics_commands_serial += 1;
    }
    render_world.gpu_condition_shaders = condition_shaders;
    render_world.gpu_colliders = gpu_colliders;
    render_world.gpu_force_fields = gpu_force_fields;
    render_world.physics_tick = time.fixed_tick;
    render_world.fixed_delta_seconds = time.fixed_delta.as_secs_f32();
    render_world.elapsed_seconds = time.elapsed.as_secs_f32();
    render_world.physics_gravity = physics_settings.gravity;
    render_world.physics_lock_mask = physics_settings.gpu_locks.mask();
    render_world.physics_enabled = physics_settings.enabled;
    render_world.background_color = background_color;
    render_world.quality = quality;
    render_world.culling = culling;
    render_world.antialiasing = antialiasing;
    render_world.shadows = shadows;
    render_world.hard_shadows = hard_shadows;
    render_world.reflections_disabled = !reflections;
}

fn collect_particles(world: &mut World) -> Vec<super::ParticleBatch> {
    let mut query = world.query::<(
        Entity,
        &super::ParticleEmitter,
        &super::ParticleSystem,
        Option<&GlobalTransform>,
    )>();
    let world = &*world;
    let mut batches = query
        .iter(world)
        .filter(|(entity, _, system, _)| {
            !system.particles.is_empty() && visible_in_hierarchy(world, *entity)
        })
        .map(|(entity, emitter, system, global)| super::ParticleBatch {
            entity: Some(entity),
            ..super::particle_batch(emitter, system, global)
        })
        .filter(|batch| !batch.instances.is_empty())
        .collect::<Vec<_>>();
    batches.sort_by_key(|batch| batch.entity.map(Entity::index));
    batches
}

enum RenderablesChange {
    None,
    /// Only these entities' `GlobalTransform`s changed.
    Moved(Vec<Entity>),
    Rebuild,
}

/// `None` when no `GlobalTransform`, `MeshRenderer`, `Visibility`, or
/// `Parent` changed since the last call and none was removed. Removing one
/// lowers a count; adding one back is itself a change. A static scene then
/// skips the per-object hash and parent-chain walks, and a scene where
/// objects only move updates just those objects.
fn renderables_changed(world: &mut World) -> RenderablesChange {
    let this_run = world.increment_change_tick();
    let last_run = world
        .resource::<RenderWorld>()
        .renderables_fingerprint
        .map(|(tick, _)| tick);
    let newer = |tick: Tick| {
        last_run.is_none_or(|last_run| tick.is_newer_than(last_run, this_run))
    };
    let mut counts = [0; 6];
    let mut changed = false;
    let mut moved = Vec::new();
    let mut query = world.query_filtered::<(
        Entity,
        Option<Ref<GlobalTransform>>,
        Option<Ref<MeshRenderer>>,
        Option<Ref<Visibility>>,
        Option<Ref<super::Parent>>,
        Option<Ref<RenderBounds>>,
        Option<Ref<super::SkinnedMesh>>,
    ), Or<(
        With<MeshRenderer>,
        With<Visibility>,
        With<super::Parent>,
        With<RenderBounds>,
    )>>();
    for (entity, transform, renderer, visibility, parent, bounds, skinned) in
        query.iter(world)
    {
        let ticks = [
            transform.map(|value| value.last_changed()),
            renderer.map(|value| value.last_changed()),
            visibility.map(|value| value.last_changed()),
            parent.map(|value| value.last_changed()),
            bounds.map(|value| value.last_changed()),
            skinned.map(|value| value.last_changed()),
        ];
        for (slot, (count, tick)) in counts.iter_mut().zip(ticks).enumerate() {
            if let Some(tick) = tick {
                *count += 1;
                if newer(tick) {
                    if slot == 0 {
                        moved.push(entity);
                    } else {
                        changed = true;
                    }
                }
            }
        }
    }
    let mut render_world = world.resource_mut::<RenderWorld>();
    changed |= render_world
        .renderables_fingerprint
        .is_none_or(|(_, previous)| previous != counts);
    render_world.renderables_fingerprint = Some((this_run, counts));
    if changed {
        RenderablesChange::Rebuild
    } else if moved.is_empty() {
        RenderablesChange::None
    } else {
        RenderablesChange::Moved(moved)
    }
}

/// Creates a small fingerprint without allocating or sorting render objects.
fn renderables_signature(world: &mut World) -> u64 {
    let mut hasher = super::FastHasher::default();
    let mut count = 0_u64;
    let mut query = world.query::<(
        Entity,
        &GlobalTransform,
        &MeshRenderer,
        Option<&RenderBounds>,
        Option<&super::SkinnedMesh>,
    )>();
    let world = &*world;
    for (entity, transform, renderer, bounds, skinned) in query.iter(world) {
        if !visible_in_hierarchy(world, entity) {
            continue;
        }
        count += 1;
        entity.to_bits().hash(&mut hasher);
        skinned
            .map_or(renderer.mesh, |skinned| skinned.mesh)
            .key()
            .hash(&mut hasher);
        renderer.material.key().hash(&mut hasher);
        renderer.cast_shadows.hash(&mut hasher);
        renderer.receive_shadows.hash(&mut hasher);
        format!("{bounds:?}").hash(&mut hasher);
        for row in transform.matrix {
            for value in row {
                value.to_bits().hash(&mut hasher);
            }
        }
    }
    count.hash(&mut hasher);
    hasher.finish()
}

/// An entity renders only when it and every ancestor are visible, like
/// Blender's and Godot's hide-with-parent behavior.
// ponytail: walks the parent chain per renderable; cache per frame if deep
// hierarchies show up in extraction profiles.
pub fn visible_in_hierarchy(world: &World, entity: Entity) -> bool {
    let mut current = Some(entity);
    // The step limit guards against a damaged scene with a parent cycle.
    for _ in 0..1024 {
        let Some(entity) = current else {
            return true;
        };
        if world
            .get::<Visibility>(entity)
            .is_some_and(|visibility| !visibility.visible)
        {
            return false;
        }
        current = world.get::<super::Parent>(entity).map(|parent| parent.0);
    }
    true
}

/// Renderables under an active [`super::Flash`], with the nearest flashing
/// ancestor's tint (the object itself counts).
fn collect_flashes(world: &mut World) -> Vec<(Entity, [f32; 4])> {
    let mut active = world.query::<&super::Flash>();
    if !active.iter(world).any(|flash| flash.strength() > 0.0) {
        return Vec::new();
    }
    let mut renderers = world.query_filtered::<Entity, With<MeshRenderer>>();
    let mut flashes = renderers
        .iter(world)
        .filter_map(|entity| {
            let mut current = Some(entity);
            // The step limit guards against a damaged scene with a cycle.
            for _ in 0..1024 {
                let at = current?;
                if let Some(flash) = world.get::<super::Flash>(at) {
                    let strength = flash.strength();
                    if strength > 0.0 {
                        let [r, g, b] = flash.color;
                        return Some((entity, [r, g, b, strength]));
                    }
                }
                current = world.get::<super::Parent>(at).map(|parent| parent.0);
            }
            None
        })
        .collect::<Vec<_>>();
    flashes.sort_by_key(|(entity, _)| *entity);
    flashes
}

fn collect_renderables(world: &mut World) -> Vec<ExtractedRenderable> {
    let mut query = world.query::<(
        Entity,
        &GlobalTransform,
        &MeshRenderer,
        Option<&RenderBounds>,
        Option<&super::SkinnedMesh>,
    )>();
    let world_ref = &*world;
    let mut renderables = query
        .iter(world_ref)
        .filter(|(entity, ..)| visible_in_hierarchy(world_ref, *entity))
        .map(|(entity, transform, renderer, bounds, skinned)| {
            ExtractedRenderable {
                entity,
                transform: *transform,
                mesh: skinned.map_or(renderer.mesh, |skinned| skinned.mesh),
                material: renderer.material,
                cast_shadows: renderer.cast_shadows,
                receive_shadows: renderer.receive_shadows,
                bounds: bounds.copied(),
            }
        })
        .collect::<Vec<_>>();
    renderables.sort_by_key(|renderable| {
        (
            renderable.mesh.key(),
            renderable.material.key(),
            renderable.entity.to_bits(),
        )
    });
    renderables
}

fn collect_active_camera(world: &mut World) -> Option<ExtractedCamera> {
    let override_entity = world.resource::<RenderCameraOverride>().entity;
    let mut query = world.query::<(Entity, &GlobalTransform, &Camera)>();
    if let Some(entity) = override_entity {
        if let Ok((entity, transform, camera)) = query.get(world, entity) {
            return Some(ExtractedCamera {
                entity,
                transform: *transform,
                projection: camera.projection,
                priority: camera.priority,
            });
        }
    }
    query
        .iter(world)
        .filter(|(_, _, camera)| camera.active)
        .max_by_key(|(entity, _, camera)| {
            (
                camera.viewport.is_none(),
                camera.priority,
                std::cmp::Reverse(entity.to_bits()),
            )
        })
        .map(|(entity, transform, camera)| ExtractedCamera {
            entity,
            transform: *transform,
            projection: camera.projection,
            priority: camera.priority,
        })
}

fn collect_views(world: &mut World) -> Vec<(ExtractedCamera, [f32; 4])> {
    if world.resource::<RenderCameraOverride>().entity.is_some() {
        return Vec::new();
    }
    let mut query = world.query::<(Entity, &GlobalTransform, &Camera)>();
    let mut views: Vec<_> = query
        .iter(world)
        .filter(|(_, _, camera)| camera.active)
        .filter_map(|(entity, transform, camera)| {
            let view = ExtractedCamera {
                entity,
                transform: *transform,
                projection: camera.projection,
                priority: camera.priority,
            };
            Some((view, camera.viewport?))
        })
        .collect();
    views.sort_by_key(|(camera, _)| {
        (camera.priority, std::cmp::Reverse(camera.entity.to_bits()))
    });
    views
}

fn collect_screens(world: &mut World) -> Vec<ExtractedScreen> {
    let mut cameras =
        world.query::<(Entity, &super::Name, &GlobalTransform, &Camera)>();
    let cameras: Vec<_> = cameras
        .iter(world)
        .map(|(entity, name, transform, camera)| {
            (
                name.0.clone(),
                ExtractedCamera {
                    entity,
                    transform: *transform,
                    projection: camera.projection,
                    priority: camera.priority,
                },
            )
        })
        .collect();
    let mut query = world.query::<(
        Entity,
        &super::CameraScreen,
        &MeshRenderer,
        &GlobalTransform,
    )>();
    let mut screens: Vec<_> = query
        .iter(world)
        .filter(|(_, screen, ..)| !screen.size.contains(&0))
        .filter_map(|(entity, screen, renderer, transform)| {
            let (_, camera) =
                cameras.iter().find(|(name, _)| *name == screen.camera)?;
            Some((
                entity.index(),
                ExtractedScreen {
                    material: renderer.material,
                    camera: *camera,
                    size: screen.size,
                    mesh: renderer.mesh,
                    transform: *transform,
                    update_every: screen.update_every,
                    enabled: screen.enabled,
                    grading: screen.grading,
                    exposure: screen.exposure,
                },
            ))
        })
        .collect();
    screens.sort_by_key(|(entity, _)| *entity);
    screens.into_iter().map(|(_, screen)| screen).collect()
}

fn collect_directional_lights(
    world: &mut World,
) -> Vec<ExtractedDirectionalLight> {
    let mut query =
        world.query::<(Entity, &GlobalTransform, &DirectionalLight)>();
    let world = &*world;
    let mut lights = query
        .iter(world)
        .filter(|(entity, ..)| visible_in_hierarchy(world, *entity))
        .map(|(entity, transform, light)| ExtractedDirectionalLight {
            entity,
            transform: *transform,
            light: *light,
        })
        .collect::<Vec<_>>();
    lights.sort_by_key(|light| light.entity.index());
    lights
}

fn collect_point_lights(world: &mut World) -> Vec<ExtractedPointLight> {
    let mut query = world.query::<(Entity, &GlobalTransform, &PointLight)>();
    let world = &*world;
    let mut lights = query
        .iter(world)
        .filter(|(entity, ..)| visible_in_hierarchy(world, *entity))
        .map(|(entity, transform, light)| ExtractedPointLight {
            entity,
            transform: *transform,
            light: *light,
        })
        .collect::<Vec<_>>();
    lights.sort_by_key(|light| light.entity.index());
    lights
}

fn collect_spot_lights(world: &mut World) -> Vec<ExtractedSpotLight> {
    let mut query = world.query::<(
        Entity,
        &GlobalTransform,
        &SpotLight,
        Option<&super::LightCookie>,
    )>();
    let world = &*world;
    let mut lights = query
        .iter(world)
        .filter(|(entity, ..)| visible_in_hierarchy(world, *entity))
        .map(|(entity, transform, light, cookie)| ExtractedSpotLight {
            entity,
            transform: *transform,
            light: *light,
            cookie: cookie.and_then(|cookie| cookie.handle),
        })
        .collect::<Vec<_>>();
    lights.sort_by_key(|light| light.entity.index());
    lights
}

/// The environment component on the lowest entity, so the pick is stable.
fn collect_first<T: Component + Copy>(world: &mut World) -> Option<T> {
    let mut query = world.query::<(Entity, &T)>();
    query
        .iter(world)
        // `Entity::to_bits` stores the index inverted, so compare indices.
        .min_by_key(|(entity, _)| entity.index())
        .map(|(_, light)| *light)
}

/// The first `T` outside any [`PostVolume`](super::PostVolume), with the
/// volumes around `eye` blended over it in priority order. `none` stands in
/// for a missing global value when a volume applies.
fn collect_blended<T: Component + Copy>(
    world: &mut World,
    eye: Option<[f32; 3]>,
    none: T,
    lerp: fn(T, T, f32) -> T,
) -> Option<T> {
    let mut query =
        world.query_filtered::<(Entity, &T), Without<super::PostVolume>>();
    let global = query
        .iter(world)
        .min_by_key(|(entity, _)| entity.index())
        .map(|(_, value)| *value);
    let Some(eye) = eye else {
        return global;
    };
    let mut query =
        world.query::<(Entity, &GlobalTransform, &super::PostVolume, &T)>();
    let mut volumes = query
        .iter(world)
        .filter_map(|(entity, transform, volume, value)| {
            let [x, y, z, _] = transform.matrix[3];
            let weight = volume.weight([x, y, z], eye);
            (weight > 0.0).then_some((
                volume.priority,
                entity.index(),
                weight,
                *value,
            ))
        })
        .collect::<Vec<_>>();
    if volumes.is_empty() {
        return global;
    }
    volumes.sort_by_key(|(priority, entity, ..)| (*priority, *entity));
    Some(
        volumes
            .into_iter()
            .fold(global.unwrap_or(none), |value, (_, _, weight, local)| {
                lerp(value, local, weight)
            }),
    )
}

/// Reads each distinct custom solver file once, in path order. A file that
/// cannot be read becomes a shader that fails to compile with the reason, so
/// it shows up in `SceneRenderer::condition_shader_errors`.
// ponytail: files are read only when GPU bodies are re-extracted, so edits
// need a scene change or restart; watch the files if hot reload is wanted.
fn custom_solver_shaders(
    bodies: &[super::ExtractedGpuPhysicsBody],
) -> Vec<String> {
    let paths = bodies
        .iter()
        .filter_map(|body| body.custom_shader.as_deref())
        .collect::<std::collections::BTreeSet<_>>();
    paths
        .into_iter()
        .map(|path| {
            let glsl = std::fs::read_to_string(path).unwrap_or_else(|error| {
                format!("#error cannot read custom solver {path}: {error}")
            });
            super::custom_solver_source(path, &glsl)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use crate::assets::AssetServer;
    use crate::Transform;

    use super::*;

    fn renderer(server: &AssetServer) -> MeshRenderer {
        MeshRenderer {
            mesh: server.fallback_mesh,
            material: server.fallback_material,
            cast_shadows: true,
            receive_shadows: true,
        }
    }

    #[test]
    fn screens_find_their_camera_by_name() {
        let mut app = App::new();
        app.add_plugin(RenderExtractPlugin).unwrap();
        let server = AssetServer::default();
        let renderer = renderer(&server);
        app.insert_resource(server);
        let camera = Camera {
            active: false,
            ..Camera::default()
        };
        let cam = app.spawn((
            Transform::new([0.0, 0.0, 5.0]),
            super::super::Name("Cam B".into()),
            camera,
        ));
        let screen = |name: &str, size| super::super::CameraScreen {
            camera: name.into(),
            size,
            ..Default::default()
        };
        app.spawn((Transform::default(), renderer, screen("Cam B", [64, 32])));
        app.spawn((Transform::default(), renderer, screen("Nobody", [64, 32])));
        app.spawn((Transform::default(), renderer, screen("Cam B", [0, 32])));
        app.update(Duration::ZERO).unwrap();
        let screens = &app.world().resource::<RenderWorld>().screens;
        assert_eq!(screens.len(), 1);
        assert_eq!(screens[0].camera.entity, cam);
        assert_eq!(screens[0].size, [64, 32]);
    }

    #[test]
    fn extraction_tracks_changes_removals_and_stable_order() {
        let mut app = App::new();
        app.add_plugin(RenderExtractPlugin).unwrap();
        let server = AssetServer::default();
        let renderer = renderer(&server);
        app.insert_resource(server);
        let first = app.spawn((Transform::new([1.0, 0.0, 0.0]), renderer));
        let second = app.spawn((Transform::new([2.0, 0.0, 0.0]), renderer));

        app.update(Duration::ZERO).unwrap();
        let render_world = app.world().resource::<RenderWorld>();
        assert_eq!(render_world.report.added, 2);

        app.world_mut()
            .get_mut::<Transform>(second)
            .unwrap()
            .position[0] = 3.0;
        let layout = app
            .world()
            .resource::<RenderWorld>()
            .renderables_layout_revision;
        app.update(Duration::ZERO).unwrap();
        let render_world = app.world().resource::<RenderWorld>();
        assert_eq!(render_world.report.changed, 1);
        // Moving updates the object in place without a new layout.
        assert_eq!(render_world.renderables_layout_revision, layout);
        let moved = render_world
            .renderables
            .iter()
            .find(|renderable| renderable.entity == second)
            .unwrap();
        assert_eq!(moved.transform.matrix[3][0], 3.0);

        app.despawn(first).unwrap();
        app.update(Duration::ZERO).unwrap();
        let render_world = app.world().resource::<RenderWorld>();
        assert_eq!(render_world.report.removed, 1);
        assert_eq!(render_world.report.total, 1);
    }

    #[test]
    fn camera_shake_moves_the_drawn_view_and_fades_back_to_rest() {
        let mut app = App::new();
        app.add_plugin(RenderExtractPlugin).unwrap();
        let rest = Transform {
            position: [1.0, 2.0, 3.0],
            ..Transform::default()
        };
        let camera = app.spawn((
            rest,
            Camera {
                active: true,
                ..Camera::default()
            },
            super::super::CameraShake::default(),
        ));
        let drawn = |app: &App| {
            app.world()
                .resource::<RenderWorld>()
                .active_camera
                .unwrap()
                .transform
                .matrix
        };
        let frame = |app: &mut App| {
            app.update(Duration::from_secs_f64(1.0 / 60.0)).unwrap();
        };
        frame(&mut app);
        let still = drawn(&app);
        assert_eq!(still[3], [1.0, 2.0, 3.0, 1.0]);

        app.world_mut()
            .get_mut::<super::super::CameraShake>(camera)
            .unwrap()
            .add_trauma(5.0);
        frame(&mut app);
        let shaken = drawn(&app);
        let moved = (0..3)
            .map(|i| (shaken[3][i] - still[3][i]).abs())
            .sum::<f32>();
        assert!(moved > 0.01 && moved < 0.6, "{shaken:?}");
        assert_eq!(app.world().get::<Transform>(camera), Some(&rest));
        // Trauma 1 at 1.5 per second is gone within 40 ticks.
        for _ in 0..40 {
            frame(&mut app);
        }
        assert_eq!(drawn(&app), still);
        assert_eq!(
            app.world()
                .get::<super::super::CameraShake>(camera)
                .unwrap()
                .trauma,
            0.0
        );
    }

    #[test]
    fn extraction_selects_highest_priority_active_camera() {
        let mut app = App::new();
        app.add_plugin(RenderExtractPlugin).unwrap();
        app.spawn((
            Transform::default(),
            Camera {
                active: true,
                priority: 1,
                ..Camera::default()
            },
        ));
        let expected = app.spawn((
            Transform::default(),
            Camera {
                active: true,
                priority: 10,
                ..Camera::default()
            },
        ));

        app.update(Duration::ZERO).unwrap();
        assert_eq!(
            app.world()
                .resource::<RenderWorld>()
                .active_camera
                .map(|camera| camera.entity),
            Some(expected)
        );
    }

    #[test]
    fn post_volumes_blend_fog_and_grading_around_the_camera() {
        use crate::runtime::{ColorGrading, Fog, PostVolume};
        let mut app = App::new();
        app.add_plugin(RenderExtractPlugin).unwrap();
        let camera = app.spawn((
            Transform::default(),
            Camera {
                active: true,
                ..Camera::default()
            },
        ));
        // Office: warm grade everywhere without fog.
        app.spawn(ColorGrading {
            saturation: 2.0,
            ..ColorGrading::DEFAULT
        });
        // Hall: fog and a grey grade in a box 10 m along X, fading over 2 m.
        app.spawn((
            Transform::new([10.0, 0.0, 0.0]),
            PostVolume {
                extents: [2.0, 2.0, 2.0],
                blend: 2.0,
                priority: 0,
            },
            Fog {
                density: 0.1,
                ..Fog::default()
            },
            ColorGrading {
                saturation: 0.0,
                ..ColorGrading::DEFAULT
            },
        ));
        let mut at = |x: f32| {
            app.world_mut()
                .get_mut::<Transform>(camera)
                .unwrap()
                .position = [x, 0.0, 0.0];
            app.update(Duration::ZERO).unwrap();
            let world = app.world().resource::<RenderWorld>();
            (
                world.fog.map(|fog| fog.density),
                world.color_grading.unwrap().saturation,
            )
        };
        assert_eq!(at(0.0), (None, 2.0), "outside: global grade, no fog");
        assert_eq!(at(10.0), (Some(0.1), 0.0), "inside: the hall's");
        let (fog, saturation) = at(7.0);
        assert!((fog.unwrap() - 0.05).abs() < 1e-6, "half way, got {fog:?}");
        assert!(
            (saturation - 1.0).abs() < 1e-6,
            "half way, got {saturation}"
        );
    }

    #[test]
    fn decals_wait_for_their_image() {
        use crate::runtime::Decal;
        let mut app = App::new();
        app.add_plugin(RenderExtractPlugin).unwrap();
        app.spawn((
            Transform::default(),
            Decal {
                texture: "missing.png".into(),
                ..Decal::default()
            },
        ));
        app.spawn((
            Transform::new([1.0, 0.0, 0.0]),
            Decal {
                extents: [2.0, 1.0, 0.5],
                ..Decal::default()
            },
        ));
        app.update(Duration::ZERO).unwrap();
        let decals = &app.world().resource::<RenderWorld>().decals;
        assert_eq!(decals.len(), 1, "the unloaded image is skipped");
        let corner = nalgebra::Matrix4::from(decals[0].0)
            * nalgebra::Vector4::new(1.0, 1.0, 1.0, 1.0);
        assert_eq!(corner, nalgebra::Vector4::new(3.0, 1.0, 0.5, 1.0));
    }

    #[test]
    fn fog_volumes_carry_their_box_in_entity_order() {
        use crate::runtime::FogVolume;
        let mut app = App::new();
        app.add_plugin(RenderExtractPlugin).unwrap();
        let first = app.spawn((
            Transform::new([1.0, 2.0, 3.0]),
            FogVolume {
                extents: [2.0, 1.0, 0.5],
                ..FogVolume::default()
            },
        ));
        app.spawn((Transform::default(), FogVolume::default()));
        app.update(Duration::ZERO).unwrap();
        let volumes = &app.world().resource::<RenderWorld>().fog_volumes;
        assert_eq!(volumes.len(), 2);
        assert_eq!(volumes[0].1.extents, [2.0, 1.0, 0.5]);
        // The box corner (1, 1, 1) lands at position plus extents.
        let corner = nalgebra::Matrix4::from(volumes[0].0)
            * nalgebra::Vector4::new(1.0, 1.0, 1.0, 1.0);
        assert_eq!(corner, nalgebra::Vector4::new(3.0, 3.0, 3.5, 1.0));
        app.world_mut().despawn(first);
        app.update(Duration::ZERO).unwrap();
        let volumes = &app.world().resource::<RenderWorld>().fog_volumes;
        assert_eq!(volumes.len(), 1);
    }

    #[test]
    fn viewport_cameras_draw_over_the_full_window_one_in_priority_order() {
        let mut app = App::new();
        app.add_plugin(RenderExtractPlugin).unwrap();
        let camera = |priority, viewport| Camera {
            active: true,
            priority,
            viewport,
            ..Camera::default()
        };
        let left = [0.0, 0.0, 0.5, 1.0];
        let right = [0.5, 0.0, 0.5, 1.0];
        let top = app.spawn((Transform::default(), camera(9, Some(right))));
        let low = app.spawn((Transform::default(), camera(2, Some(left))));
        app.update(Duration::ZERO).unwrap();
        let world = app.world().resource::<RenderWorld>();
        let order: Vec<_> =
            world.views.iter().map(|(c, v)| (c.entity, *v)).collect();
        assert_eq!(order, [(low, left), (top, right)]);
        // No camera fills the window: the highest view aims.
        assert_eq!(world.active_camera.map(|c| c.entity), Some(top));

        let full = app.spawn((Transform::default(), camera(0, None)));
        app.update(Duration::ZERO).unwrap();
        let world = app.world().resource::<RenderWorld>();
        assert_eq!(world.active_camera.map(|c| c.entity), Some(full));
        assert_eq!(world.views.len(), 2);
    }

    #[test]
    fn camera_override_can_select_an_inactive_editor_camera() {
        let mut app = App::new();
        app.add_plugin(RenderExtractPlugin).unwrap();
        app.spawn((
            Transform::default(),
            Camera {
                active: true,
                priority: 10,
                ..Camera::default()
            },
        ));
        let editor_camera = app.spawn((
            Transform::new([0.0, 3.0, 8.0]),
            Camera {
                active: false,
                ..Camera::default()
            },
        ));
        app.world_mut()
            .resource_mut::<RenderCameraOverride>()
            .entity = Some(editor_camera);

        app.update(Duration::ZERO).unwrap();
        assert_eq!(
            app.world()
                .resource::<RenderWorld>()
                .active_camera
                .map(|camera| camera.entity),
            Some(editor_camera)
        );
    }

    #[test]
    fn hidden_entities_are_removed_from_render_world() {
        let mut app = App::new();
        app.add_plugin(RenderExtractPlugin).unwrap();
        let server = AssetServer::default();
        let renderer = renderer(&server);
        app.insert_resource(server);
        let entity =
            app.spawn((Transform::default(), renderer, Visibility::default()));
        app.update(Duration::ZERO).unwrap();
        assert_eq!(app.world().resource::<RenderWorld>().report.total, 1);

        app.world_mut()
            .get_mut::<Visibility>(entity)
            .unwrap()
            .visible = false;
        app.update(Duration::ZERO).unwrap();
        let render_world = app.world().resource::<RenderWorld>();
        assert_eq!(render_world.report.total, 0);
        assert_eq!(render_world.report.removed, 1);
    }

    #[test]
    fn removing_a_hidden_visibility_after_a_quiet_frame_shows_the_object() {
        let mut app = App::new();
        app.add_plugin(RenderExtractPlugin).unwrap();
        let server = AssetServer::default();
        let renderer = renderer(&server);
        app.insert_resource(server);
        let entity = app.spawn((
            Transform::default(),
            renderer,
            Visibility { visible: false },
        ));
        app.update(Duration::ZERO).unwrap();
        app.update(Duration::ZERO).unwrap();
        assert_eq!(app.world().resource::<RenderWorld>().report.total, 0);

        // A removal changes no remaining component; only the count drops.
        app.world_mut().entity_mut(entity).remove::<Visibility>();
        app.update(Duration::ZERO).unwrap();
        assert_eq!(app.world().resource::<RenderWorld>().report.total, 1);
    }

    #[test]
    fn lights_under_hidden_objects_are_not_extracted() {
        let mut app = App::new();
        app.add_plugin(RenderExtractPlugin).unwrap();
        let parent =
            app.spawn((Transform::default(), Visibility { visible: false }));
        let light = app.spawn((Transform::default(), PointLight::default()));
        app.set_parent(light, parent).unwrap();
        app.update(Duration::ZERO).unwrap();
        assert!(app
            .world()
            .resource::<RenderWorld>()
            .point_lights
            .is_empty());

        app.world_mut()
            .get_mut::<Visibility>(parent)
            .unwrap()
            .visible = true;
        app.update(Duration::ZERO).unwrap();
        assert_eq!(app.world().resource::<RenderWorld>().point_lights.len(), 1);
    }

    #[test]
    fn render_bounds_overrides_are_extracted_and_track_edits() {
        let mut app = App::new();
        app.add_plugin(RenderExtractPlugin).unwrap();
        let server = AssetServer::default();
        let renderer = renderer(&server);
        app.insert_resource(server);
        let entity = app.spawn((
            Transform::new([5.0, 0.0, 0.0]),
            renderer,
            RenderBounds::Sphere {
                center: [0.0, 1.0, 0.0],
                radius: 2.0,
            },
        ));
        let bounds = |app: &App| {
            app.world().resource::<RenderWorld>().renderables[0].bounds
        };
        app.update(Duration::ZERO).unwrap();
        assert_eq!(
            bounds(&app),
            Some(RenderBounds::Sphere {
                center: [0.0, 1.0, 0.0],
                radius: 2.0,
            }),
            "the override stays in local space"
        );

        let edited = RenderBounds::Aabb {
            min: [-1.0; 3],
            max: [1.0; 3],
        };
        *app.world_mut().get_mut::<RenderBounds>(entity).unwrap() = edited;
        app.update(Duration::ZERO).unwrap();
        assert_eq!(app.world().resource::<RenderWorld>().report.changed, 1);
        assert_eq!(bounds(&app), Some(edited));

        app.world_mut().entity_mut(entity).remove::<RenderBounds>();
        app.update(Duration::ZERO).unwrap();
        assert_eq!(bounds(&app), None, "the renderer uses the mesh box");
    }

    #[test]
    fn a_flash_tints_the_object_and_its_children_then_fades() {
        let mut app = App::new();
        app.add_plugin(RenderExtractPlugin).unwrap();
        let server = AssetServer::default();
        let renderer = renderer(&server);
        app.insert_resource(server);
        let enemy = app.spawn((
            Transform::default(),
            renderer,
            super::super::Flash {
                color: [1.0, 0.0, 0.0],
                duration: 0.1,
                ..Default::default()
            },
        ));
        let body = app.spawn((
            Transform::default(),
            renderer,
            super::super::Parent(enemy),
        ));
        app.spawn((Transform::default(), renderer));
        let frame = |app: &mut App| {
            app.update(Duration::from_secs_f64(1.0 / 60.0)).unwrap();
            app.world().resource::<RenderWorld>().flashes.clone()
        };
        assert!(frame(&mut app).is_empty(), "no flash until one starts");
        app.world_mut()
            .get_mut::<super::super::Flash>(enemy)
            .unwrap()
            .flash();
        let flashes = frame(&mut app);
        let mut expected = vec![enemy, body];
        expected.sort();
        assert_eq!(
            flashes
                .iter()
                .map(|(entity, _)| *entity)
                .collect::<Vec<_>>(),
            expected
        );
        let [r, g, b, strength] = flashes[0].1;
        assert_eq!([r, g, b], [1.0, 0.0, 0.0]);
        // One 1/60 s tick into a 0.1 s flash.
        assert!((strength - (1.0 - 1.0 / 6.0)).abs() < 1e-3, "{strength}");
        for _ in 0..6 {
            frame(&mut app);
        }
        assert!(frame(&mut app).is_empty(), "faded after 0.1 s");
    }

    #[test]
    fn hiding_a_parent_hides_its_children() {
        let mut app = App::new();
        app.add_plugin(RenderExtractPlugin).unwrap();
        let server = AssetServer::default();
        let renderer = renderer(&server);
        app.insert_resource(server);
        let parent = app.spawn((Transform::default(), Visibility::default()));
        app.spawn((
            Transform::default(),
            renderer,
            super::super::Parent(parent),
        ));
        app.update(Duration::ZERO).unwrap();
        assert_eq!(app.world().resource::<RenderWorld>().report.total, 1);

        app.world_mut()
            .get_mut::<Visibility>(parent)
            .unwrap()
            .visible = false;
        app.update(Duration::ZERO).unwrap();
        assert_eq!(app.world().resource::<RenderWorld>().report.total, 0);
    }
}
