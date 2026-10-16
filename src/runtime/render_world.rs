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
    /// World position and settings of the first
    /// [`LightProbeGrid`](super::LightProbeGrid) in entity order.
    pub light_probe_grid: Option<([f32; 3], super::LightProbeGrid)>,
    /// Entities with a [`Lightmap`](super::Lightmap), in entity order; an
    /// entity's place in this list picks its lightmap slot.
    pub lightmaps: Vec<(Entity, super::Lightmap)>,
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
    /// Each [`Sprite`](super::Sprite) whose image is loaded or that has
    /// none, in canvas order.
    pub sprites: Vec<ExtractedSprite>,
    /// Each [`Light2d`](super::Light2d) in entity order.
    pub lights_2d: Vec<ExtractedLight2d>,
    /// World-space edges `[ax, ay, bx, by]` of each
    /// [`LightOccluder2d`](super::LightOccluder2d) in entity order.
    pub occluder_edges_2d: Vec<[f32; 4]>,
    /// The first [`Ambient2d`](super::Ambient2d) color in entity order.
    pub ambient_2d: Option<[f32; 3]>,
    pub bloom: Option<super::Bloom>,
    pub auto_exposure: Option<super::AutoExposure>,
    pub depth_of_field: Option<super::DepthOfField>,
    pub motion_blur: Option<super::MotionBlur>,
    pub color_grading: Option<super::ColorGrading>,
    /// Loaded image and intensity of the first loaded
    /// [`ColorLut`](super::ColorLut).
    pub color_lut:
        Option<(crate::assets::Handle<crate::assets::TextureAsset>, f32)>,
    /// Source of the first [`PostShader`](super::PostShader) in entity
    /// order; empty when there is none.
    pub post_shader: String,
    /// Source of the first [`SkyShader`](super::SkyShader) in entity
    /// order; empty when there is none.
    pub sky_shader: String,
    /// Source of the first [`FogShader`](super::FogShader) in entity
    /// order; empty when there is none.
    pub fog_shader: String,
    /// Meshes that show a camera's image, in entity order.
    pub screens: Vec<ExtractedScreen>,
    pub ambient_occlusion: Option<super::AmbientOcclusion>,
    /// Live particles of each visible emitter, in entity order; each batch
    /// is one instanced draw.
    pub particles: Vec<super::ParticleBatch>,
    /// Visible GPU-simulated emitters, in entity order.
    pub gpu_particles: Vec<super::GpuParticleBatch>,
    /// Emitters drawn in the 2D pass, with their canvas layer and world z,
    /// in canvas order.
    pub particles_2d: Vec<(Entity, i32, f32)>,
    /// Meshes the renderer deforms on the GPU this frame, with
    /// [`super::GpuSkinning`]; empty otherwise.
    pub skin_jobs: Vec<super::SkinJob>,
    /// Source of each emitter's [`ParticleShader`](super::ParticleShader).
    pub particle_shaders: HashMap<Entity, String>,
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
    /// The scene's [`super::Wind`] as a field filling the world: foliage and
    /// swaying materials lean in it, GPU bodies do not feel it.
    pub wind: Option<super::GpuForceField>,
    /// Resolved [`super::GpuConditionShader`]s, in dispatch order.
    pub gpu_condition_shaders: Vec<ExtractedConditionShader>,
    /// Wrapped `PhysicsSolver::Custom` hook files, one per distinct path,
    /// dispatched before the condition shaders. Refreshed only when the GPU
    /// bodies are re-extracted.
    pub gpu_solver_shaders: Vec<String>,
    pub physics_tick: u64,
    pub fixed_delta_seconds: f32,
    pub elapsed_seconds: f32,
    /// Global wetness from 0 to 1 set by `GameScene::set_wetness`; darkens
    /// and smooths `wettable` materials.
    pub wetness: f32,
    pub physics_gravity: [f32; 3],
    /// `AxisLock::mask` of `PhysicsSettings::gpu_locks`.
    pub physics_lock_mask: u32,
    /// The [`super::GpuSimulationLod`] policy, when set.
    pub gpu_lod: Option<super::GpuSimulationLod>,
    /// [`super::SimulationObserver`] positions; LOD needs at least one.
    pub gpu_observers: Vec<[f32; 3]>,
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
    /// From `RenderSettings::max_texture_size`; 0 is no cap.
    pub max_texture_size: u32,
    /// Drawn tint of each flashing renderable, sorted by entity: rgb from
    /// its own or an ancestor's [`super::Flash`], a the strength.
    pub flashes: Vec<(Entity, [f32; 4])>,
    /// Renderables on [`super::RenderLayer::Viewmodel`], sorted. Empty
    /// unless the active camera has a [`super::ViewmodelCamera`].
    pub viewmodels: Vec<Entity>,
    /// The active camera's [`super::ViewmodelCamera`] field of view.
    pub viewmodel_fov: Option<f32>,
    /// Gerstner waves the scene vertex shader adds to each water surface,
    /// by surface entity, sorted by entity. The surface mesh stays flat.
    pub water_waves: Vec<(Entity, Vec<super::GpuWave>)>,
    /// Each renderable's [`super::MaterialOverride`], sorted by entity.
    pub material_overrides: Vec<(Entity, super::MaterialOverride)>,
    /// Index of each entity in `renderables`.
    index: HashMap<Entity, usize>,
    /// Each `Foliage` copy: its place in `renderables`, its unleaned
    /// matrix and its owner's `sway`.
    pub(crate) foliage: Vec<(usize, f32)>,
    renderables_signature: Option<u64>,
    /// Tick and component counts seen by the last signature hash.
    renderables_fingerprint: Option<(Tick, [usize; 9])>,
    /// [`foliage_share`] the renderables were built with.
    foliage_share: f32,
}

impl RenderWorld {
    /// Material keys of the `Foliage` copies, so GPU timing can tell
    /// foliage draws apart.
    pub(crate) fn foliage_materials(&self) -> std::collections::HashSet<u64> {
        self.foliage
            .iter()
            .filter_map(|(at, _)| self.renderables.get(*at))
            .map(|renderable| renderable.material.key())
            .collect()
    }

    /// Index into `renderables` and sway of each `Foliage` copy. The scene
    /// vertex shaders lean these by the force fields at their base.
    pub(crate) fn foliage_sways(
        &self,
    ) -> impl Iterator<Item = (usize, f32)> + '_ {
        self.foliage.iter().copied()
    }
}

/// One [`Sprite`](super::Sprite) ready to draw.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ExtractedSprite {
    /// Maps the unit quad, -0.5..0.5 in X and Y, to world space.
    pub matrix: [[f32; 4]; 4],
    pub color: [f32; 4],
    /// Image coordinates at the quad's `[left, top, right, bottom]`, with
    /// the flips applied.
    pub uv: [f32; 4],
    /// `None` draws plain color.
    pub texture: Option<Handle<crate::assets::TextureAsset>>,
    /// Tangent-space normal map; `None` lights the sprite flat.
    pub normal_map: Option<Handle<crate::assets::TextureAsset>>,
    pub layer: i32,
    /// Drawn inside the 3D scene; these sort before every canvas sprite.
    pub in_scene: bool,
}

/// One [`Light2d`](super::Light2d) ready to light sprites.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ExtractedLight2d {
    /// World position; only X and Y light.
    pub position: [f32; 3],
    /// Color times intensity.
    pub color: [f32; 3],
    pub radius: f32,
    /// Height above the sprites, for normal-mapped ones.
    pub height: f32,
}

/// Splits canvas-ordered sprites into runs that share an image, one draw
/// call each. Batching never reorders sprites, so overlaps stay correct.
pub fn sprite_batches(
    sprites: &[ExtractedSprite],
) -> Vec<std::ops::Range<usize>> {
    let mut batches: Vec<std::ops::Range<usize>> = Vec::new();
    for (index, sprite) in sprites.iter().enumerate() {
        match batches.last_mut() {
            Some(batch)
                if (
                    sprites[batch.start].texture,
                    sprites[batch.start].normal_map,
                ) == (sprite.texture, sprite.normal_map) =>
            {
                batch.end = index + 1;
            }
            _ => batches.push(index..index + 1),
        }
    }
    batches
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
    let skin_jobs = super::skin_jobs(world);
    world.resource_mut::<RenderWorld>().skin_jobs = skin_jobs;
    super::terrain::build_terrains(world);
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
    let light_probe_grid = world
        .query::<(Entity, &GlobalTransform, &super::LightProbeGrid)>()
        .iter(world)
        .min_by_key(|(entity, ..)| entity.index())
        .map(|(_, transform, grid)| {
            let [x, y, z, _] = transform.matrix[3];
            ([x, y, z], *grid)
        });
    let mut lightmaps = world
        .query::<(Entity, &super::Lightmap)>()
        .iter(world)
        .map(|(entity, lightmap)| (entity, *lightmap))
        .collect::<Vec<_>>();
    lightmaps.sort_by_key(|(entity, _)| *entity);
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
    let sprites = {
        let mut query =
            world.query::<(Entity, &GlobalTransform, &super::Sprite)>();
        let mut sprites = query
            .iter(world)
            .filter(|(_, _, sprite)| {
                sprite.handle.is_some() || sprite.texture.as_os_str().is_empty()
            })
            .map(|(entity, transform, sprite)| {
                let [width, height] = sprite.size;
                let matrix = nalgebra::Matrix4::from(transform.matrix)
                    * nalgebra::Matrix4::new_nonuniform_scaling(
                        &nalgebra::Vector3::new(width, height, 1.0),
                    );
                let [mut left, mut top, mut right, mut bottom] = sprite.region;
                if sprite.flip_x {
                    std::mem::swap(&mut left, &mut right);
                }
                if sprite.flip_y {
                    std::mem::swap(&mut top, &mut bottom);
                }
                let extracted = ExtractedSprite {
                    matrix: matrix.into(),
                    color: sprite.color,
                    uv: [left, top, right, bottom],
                    texture: sprite.handle,
                    normal_map: sprite.normal_handle,
                    layer: sprite.layer,
                    in_scene: sprite.in_scene,
                };
                (entity.index(), extracted)
            })
            .collect::<Vec<_>>();
        sprites.sort_by(|(a_entity, a), (b_entity, b)| {
            b.in_scene
                .cmp(&a.in_scene)
                .then(a.layer.cmp(&b.layer))
                .then(a.matrix[3][2].total_cmp(&b.matrix[3][2]))
                .then(a_entity.cmp(b_entity))
        });
        sprites.into_iter().map(|(_, sprite)| sprite).collect()
    };
    let lights_2d = {
        let mut query =
            world.query::<(Entity, &GlobalTransform, &super::Light2d)>();
        let mut lights = query
            .iter(world)
            .map(|(entity, transform, light)| {
                let position = transform.matrix[3];
                let extracted = ExtractedLight2d {
                    position: [position[0], position[1], position[2]],
                    color: light.color.map(|channel| channel * light.intensity),
                    radius: light.radius,
                    height: light.height,
                };
                (entity.index(), extracted)
            })
            .collect::<Vec<_>>();
        lights.sort_by_key(|(entity, _)| *entity);
        lights.into_iter().map(|(_, light)| light).collect()
    };
    let occluder_edges_2d = {
        let mut query =
            world
                .query::<(Entity, &GlobalTransform, &super::LightOccluder2d)>();
        let mut occluders = query
            .iter(world)
            .map(|(entity, transform, occluder)| {
                let m = transform.matrix;
                let points = occluder
                    .points
                    .iter()
                    .map(|&[x, y]| {
                        [
                            m[0][0] * x + m[1][0] * y + m[3][0],
                            m[0][1] * x + m[1][1] * y + m[3][1],
                        ]
                    })
                    .collect::<Vec<_>>();
                let edges = (0..points.len())
                    .map(|i| {
                        let [a, b] =
                            [points[i], points[(i + 1) % points.len()]];
                        [a[0], a[1], b[0], b[1]]
                    })
                    .collect::<Vec<_>>();
                (entity.index(), edges)
            })
            .collect::<Vec<_>>();
        occluders.sort_by_key(|(entity, _)| *entity);
        occluders.into_iter().flat_map(|(_, edges)| edges).collect()
    };
    let ambient_2d = world
        .query::<(Entity, &super::Ambient2d)>()
        .iter(world)
        .min_by_key(|(entity, _)| entity.index())
        .map(|(_, ambient)| ambient.color);
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
    let post_shader =
        first_source::<super::PostShader>(world, |shader| &shader.source);
    let sky_shader =
        first_source::<super::SkyShader>(world, |shader| &shader.source);
    let fog_shader =
        first_source::<super::FogShader>(world, |shader| &shader.source);
    let screens = collect_screens(world);
    let ambient_occlusion = collect_first::<super::AmbientOcclusion>(world);
    let particles = collect_particles(world);
    let gpu_particles = collect_gpu_particles(world);
    let particles_2d = {
        let mut query = world.query_filtered::<
            (Entity, &GlobalTransform, &super::Particles2d),
            With<super::ParticleEmitter>,
        >();
        let mut emitters = query
            .iter(world)
            .map(|(entity, transform, particles)| {
                (entity, particles.layer, transform.matrix[3][2])
            })
            .collect::<Vec<_>>();
        emitters.sort_by(|a, b| {
            a.1.cmp(&b.1)
                .then(a.2.total_cmp(&b.2))
                .then(a.0.index().cmp(&b.0.index()))
        });
        emitters
    };
    let particle_shaders = world
        .query_filtered::<(Entity, &super::ParticleShader), With<super::ParticleEmitter>>()
        .iter(world)
        .map(|(entity, shader)| (entity, shader.source.clone()))
        .collect();
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
    let (gpu_colliders, gpu_force_fields) =
        match world.get_resource::<super::PhysicsWorld>() {
            Some(physics)
                if !gpu_physics.as_ref().is_some_and(Vec::is_empty)
                    || !gpu_particles.is_empty() =>
            {
                (physics.gpu_colliders(), physics.gpu_force_fields(world))
            }
            // Foliage and swaying materials lean by the fields in the
            // vertex shader. A scene without field sensors sends none.
            Some(physics) => (Vec::new(), physics.gpu_force_fields(world)),
            None => Default::default(),
        };
    let wind = super::Wind::of_scene(world).map(|wind| wind.gpu_field());
    let gpu_lod = world.get_resource::<super::GpuSimulationLod>().copied();
    let gpu_observers = if gpu_lod.is_some() {
        world
            .query_filtered::<
                (&crate::Transform, Option<&GlobalTransform>),
                With<super::SimulationObserver>,
            >()
            .iter(world)
            .map(|(transform, global)| {
                global.map_or(transform.position, |global| {
                    let [x, y, z, _] = global.matrix[3];
                    [x, y, z]
                })
            })
            .collect()
    } else {
        Vec::new()
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
        max_texture_size,
    ) = (
        render_settings.background_color,
        render_settings.quality,
        render_settings.culling,
        render_settings.antialiasing,
        render_settings.shadows,
        render_settings.hard_shadows,
        render_settings.reflections,
        render_settings.max_texture_size,
    );

    let flashes = collect_flashes(world);
    let viewmodel_fov = active_camera.as_ref().and_then(|camera| {
        world
            .get::<super::ViewmodelCamera>(camera.entity)
            .map(|viewmodel| viewmodel.vertical_fov_radians)
    });
    let mut viewmodels = Vec::new();
    if viewmodel_fov.is_some() {
        viewmodels = world
            .query_filtered::<(Entity, &super::RenderLayer), With<MeshRenderer>>()
            .iter(world)
            .filter(|(_, layer)| **layer == super::RenderLayer::Viewmodel)
            .map(|(entity, _)| entity)
            .collect();
        viewmodels.sort();
    }
    let seconds = time.fixed_tick as f64 * time.fixed_delta.as_secs_f64();
    let mut water_waves = world
        .query::<(&super::WaterBody, &super::WaterMesh)>()
        .iter(world)
        .map(|(water, mesh)| {
            (mesh.entity(), water.gpu_waves(water.clock(seconds)))
        })
        .collect::<Vec<_>>();
    water_waves.sort_by_key(|(entity, _)| *entity);
    let mut material_overrides = world
        .query_filtered::<(Entity, &super::MaterialOverride), With<MeshRenderer>>()
        .iter(world)
        .map(|(entity, values)| (entity, *values))
        .collect::<Vec<_>>();
    material_overrides.sort_by_key(|(entity, _)| *entity);
    let new_foliage = renderables
        .as_ref()
        .map(|renderables| collect_foliage(world, renderables));
    let mut render_world = world.resource_mut::<RenderWorld>();
    render_world.flashes = flashes;
    render_world.viewmodels = viewmodels;
    render_world.viewmodel_fov = viewmodel_fov;
    render_world.water_waves = water_waves;
    render_world.material_overrides = material_overrides;
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
            // Foliage gives one entity many renderables, so a group that
            // gains copies would count below zero.
            let removed = (render_world.renderables.len() + added)
                .saturating_sub(renderables.len());
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
    if let Some(foliage) = new_foliage {
        render_world.foliage = foliage;
    }
    render_world.renderables_signature = renderables_signature;
    render_world.active_camera = active_camera;
    render_world.views = views;
    render_world.tone_mapping = tone_mapping;
    render_world.fog = fog;
    render_world.procedural_sky = procedural_sky;
    render_world.fog_volumes = fog_volumes;
    render_world.decals = decals;
    render_world.sprites = sprites;
    render_world.lights_2d = lights_2d;
    render_world.occluder_edges_2d = occluder_edges_2d;
    render_world.ambient_2d = ambient_2d;
    render_world.bloom = bloom;
    render_world.auto_exposure = auto_exposure;
    render_world.depth_of_field = depth_of_field;
    render_world.motion_blur = motion_blur;
    render_world.color_grading = color_grading;
    render_world.color_lut = color_lut;
    render_world.post_shader = post_shader;
    render_world.sky_shader = sky_shader;
    render_world.fog_shader = fog_shader;
    render_world.screens = screens;
    render_world.ambient_occlusion = ambient_occlusion;
    render_world.particles = particles;
    render_world.gpu_particles = gpu_particles;
    render_world.particles_2d = particles_2d;
    render_world.particle_shaders = particle_shaders;
    render_world.environment = environment;
    render_world.reflection_probes = reflection_probes;
    render_world.light_probe_grid = light_probe_grid;
    render_world.lightmaps = lightmaps;
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
    render_world.wind = wind;
    render_world.physics_tick = time.fixed_tick;
    render_world.fixed_delta_seconds = time.fixed_delta.as_secs_f32();
    render_world.elapsed_seconds = time.elapsed.as_secs_f32();
    render_world.physics_gravity = physics_settings.gravity;
    render_world.physics_lock_mask = physics_settings.gpu_locks.mask();
    render_world.gpu_lod = gpu_lod;
    render_world.gpu_observers = gpu_observers;
    render_world.physics_enabled = physics_settings.enabled;
    render_world.background_color = background_color;
    render_world.quality = quality;
    render_world.culling = culling;
    render_world.antialiasing = antialiasing;
    render_world.shadows = shadows;
    render_world.hard_shadows = hard_shadows;
    render_world.reflections_disabled = !reflections;
    render_world.max_texture_size = max_texture_size;
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

fn collect_gpu_particles(world: &mut World) -> Vec<super::GpuParticleBatch> {
    // ponytail: the scene wind is sampled at each emitter's origin once a
    // frame; GPU particles far from it miss the gusts' drift.
    let wind = super::Wind::of_scene(world);
    let seconds = world
        .get_resource::<super::FrameTime>()
        .map_or(0.0, |time| time.elapsed.as_secs_f32());
    let mut query = world.query::<(
        Entity,
        &super::ParticleEmitter,
        &super::ParticleSystem,
        Option<&GlobalTransform>,
        Option<&super::SceneId>,
    )>();
    let world = &*world;
    let mut batches = query
        .iter(world)
        .filter(|(entity, emitter, ..)| {
            emitter.simulation == super::ParticleSimulation::Gpu
                && visible_in_hierarchy(world, *entity)
        })
        .map(|(entity, emitter, system, global, id)| {
            let mut batch =
                super::gpu_particle_batch(entity, emitter, system, global, id);
            if let Some(wind) = &wind {
                let [x, y, z, _] = batch.rows[3];
                let push = wind.push_at([x, y, z], seconds);
                for (row, push) in batch.rows[5].iter_mut().zip(push) {
                    *row += push * emitter.wind_factor;
                }
            }
            if emitter.on_parent_death > 0 {
                batch.parent = world
                    .get::<super::Parent>(entity)
                    .map(|parent| (parent.0, emitter.on_parent_death));
            }
            batch
        })
        .collect::<Vec<_>>();
    // One level: a sub-emitter's parent must be a plain GPU emitter.
    let parents = batches
        .iter()
        .filter(|batch| batch.parent.is_none())
        .map(|batch| (batch.entity, batch.max_particles))
        .collect::<std::collections::HashMap<_, _>>();
    for batch in &mut batches {
        let Some((parent, burst)) = batch.parent else {
            continue;
        };
        match parents.get(&parent) {
            Some(slots) => {
                let most = super::MAX_GPU_PARTICLES / burst * burst;
                batch.max_particles = slots.saturating_mul(burst).min(most);
            }
            None => batch.parent = None,
        }
    }
    // Parents step before their sub-emitters.
    batches.sort_by_key(|batch| (batch.parent.is_some(), batch.entity));
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
    let mut counts = [0; 9];
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
        Option<Ref<super::Foliage>>,
        Option<Ref<super::TerrainChunks>>,
        Option<Ref<super::Heightfield>>,
    ), Or<(
        With<MeshRenderer>,
        With<Visibility>,
        With<super::Parent>,
        With<RenderBounds>,
        With<super::TerrainChunks>,
        With<super::Heightfield>,
        With<super::Foliage>,
    )>>();
    for (
        entity,
        transform,
        renderer,
        visibility,
        parent,
        bounds,
        skinned,
        foliage,
        chunks,
        field,
    ) in query.iter(world)
    {
        // A heightfield moving or changing moves the foliage set on it.
        let scattered =
            foliage.is_some() || chunks.is_some() || field.is_some();
        let ticks = [
            transform.map(|value| value.last_changed()),
            renderer.map(|value| value.last_changed()),
            visibility.map(|value| value.last_changed()),
            parent.map(|value| value.last_changed()),
            bounds.map(|value| value.last_changed()),
            skinned.map(|value| value.last_changed()),
            foliage.map(|value| value.last_changed()),
            chunks.map(|value| value.last_changed()),
            field.map(|value| value.last_changed()),
        ];
        for (slot, (count, tick)) in counts.iter_mut().zip(ticks).enumerate() {
            if let Some(tick) = tick {
                *count += 1;
                if newer(tick) {
                    // Foliage copies and terrain chunks share their
                    // owner's entity, so a moved owner or part rebuilds.
                    if slot == 0
                        && !scattered
                        && foliage_owner(world, entity).is_none()
                    {
                        moved.push(entity);
                    } else {
                        changed = true;
                    }
                }
            }
        }
    }
    let share = resolved_foliage_share(world);
    let mut render_world = world.resource_mut::<RenderWorld>();
    changed |= render_world
        .renderables_fingerprint
        .is_none_or(|(_, previous)| previous != counts);
    changed |= render_world.foliage_share != share;
    render_world.foliage_share = share;
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
    let mut terrains =
        world.query::<(Entity, &GlobalTransform, &super::TerrainChunks)>();
    let world = &*world;
    for (entity, transform, renderer, bounds, skinned) in query.iter(world) {
        if !visible_in_hierarchy(world, entity) {
            continue;
        }
        let foliage = foliage_owner(world, entity).map(|(_, foliage)| foliage);
        count += 1;
        entity.to_bits().hash(&mut hasher);
        skinned
            .map_or(renderer.mesh, |skinned| skinned.mesh)
            .key()
            .hash(&mut hasher);
        renderer.material.key().hash(&mut hasher);
        renderer.cast_shadows.hash(&mut hasher);
        renderer.receive_shadows.hash(&mut hasher);
        format!("{bounds:?}{foliage:?}").hash(&mut hasher);
        for row in transform.matrix {
            for value in row {
                value.to_bits().hash(&mut hasher);
            }
        }
    }
    for (entity, transform, chunks) in terrains.iter(world) {
        if !visible_in_hierarchy(world, entity) {
            continue;
        }
        count += 1;
        entity.to_bits().hash(&mut hasher);
        chunks.hash(&mut hasher);
        for row in transform.matrix {
            for value in row {
                value.to_bits().hash(&mut hasher);
            }
        }
    }
    count.hash(&mut hasher);
    resolved_foliage_share(world).to_bits().hash(&mut hasher);
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

/// Each `Foliage` copy in `renderables` with its sway.
fn collect_foliage(
    world: &World,
    renderables: &[ExtractedRenderable],
) -> Vec<(usize, f32)> {
    renderables
        .iter()
        .enumerate()
        .filter_map(|(at, renderable)| {
            let (_, foliage) = foliage_owner(world, renderable.entity)?;
            Some((at, foliage.sway))
        })
        .collect()
}

/// Share of each `Foliage`'s copies drawn for a resolved profile: Eco and
/// Balanced thin grass and trees, keeping the first copies of the scatter.
#[must_use]
pub fn foliage_share(profile: super::QualityProfile) -> f32 {
    match profile {
        super::QualityProfile::Eco => 0.3,
        super::QualityProfile::Balanced => 0.6,
        super::QualityProfile::High | super::QualityProfile::Auto => 1.0,
    }
}

/// [`foliage_share`] of the profile the renderer resolves; `Auto` before a
/// renderer starts draws every copy.
fn resolved_foliage_share(world: &World) -> f32 {
    let requested = world
        .get_resource::<super::RenderSettings>()
        .map_or(super::QualityProfile::Auto, |settings| settings.quality);
    foliage_share(
        world
            .get_resource::<crate::rendering::scene_renderer::RendererCapabilities>()
            .map_or(requested, |capabilities| {
                crate::rendering::scene_renderer::resolve_quality(
                    requested,
                    capabilities,
                )
            }),
    )
}

/// The `Foliage` on `entity` or its nearest ancestor, and the object that
/// holds it, so every part of a multi-part model scatters as one group.
// ponytail: each part leans in the wind from its own origin, so leaves on a
// tall trunk lag the trunk's tip; lean from the group's base if it shows.
fn foliage_owner(
    world: &World,
    entity: Entity,
) -> Option<(Entity, &super::Foliage)> {
    let mut current = entity;
    // The step limit guards against a damaged scene with a parent cycle.
    for _ in 0..1024 {
        if let Some(foliage) = world.get::<super::Foliage>(current) {
            return Some((current, foliage));
        }
        current = world.get::<super::Parent>(current)?.0;
    }
    None
}

/// A heightfield's world matrix, its inverse, and the terrain drawing it.
type Ground<'a> = (
    nalgebra::Matrix4<f32>,
    nalgebra::Matrix4<f32>,
    &'a super::Heightfield,
    Option<&'a super::Terrain>,
);

/// Applies [`super::Foliage::conform`] and [`super::Foliage::layers`] to one
/// copy's world matrix; `None` drops the copy.
fn settle(
    foliage: &super::Foliage,
    matrix: nalgebra::Matrix4<f32>,
    grounds: &[Ground],
) -> Option<nalgebra::Matrix4<f32>> {
    if !foliage.conform && foliage.layers.is_empty() {
        return Some(matrix);
    }
    let at =
        nalgebra::Point3::from(matrix.fixed_view::<3, 1>(0, 3).into_owned());
    let found =
        grounds
            .iter()
            .find_map(|(to_world, to_local, field, terrain)| {
                let local = to_local.transform_point(&at);
                let (height, weights) =
                    super::terrain::ground(field, *terrain, local.x, local.z)?;
                let surface = to_world.transform_point(&nalgebra::Point3::new(
                    local.x, height, local.z,
                ));
                Some((surface.y, terrain.map(|_| weights)))
            });
    if !foliage.layers.is_empty() {
        let weights = found.and_then(|(_, weights)| weights)?;
        let chosen = foliage
            .layers
            .iter()
            .filter_map(|&layer| weights.get(layer as usize))
            .sum::<f32>();
        if chosen < 0.5 {
            return None;
        }
    }
    match found {
        Some((y, _)) if foliage.conform => Some(
            nalgebra::Matrix4::new_translation(&nalgebra::Vector3::new(
                0.0,
                y - at.y,
                0.0,
            )) * matrix,
        ),
        _ => Some(matrix),
    }
}

fn collect_renderables(world: &mut World) -> Vec<ExtractedRenderable> {
    let mut query = world.query::<(
        Entity,
        &GlobalTransform,
        &MeshRenderer,
        Option<&RenderBounds>,
        Option<&super::SkinnedMesh>,
    )>();
    let mut fields = world.query::<(
        &GlobalTransform,
        &super::Heightfield,
        Option<&super::Terrain>,
    )>();
    let share = resolved_foliage_share(world);
    let world_ref = &*world;
    let grounds = fields
        .iter(world_ref)
        .filter_map(|(transform, field, terrain)| {
            let matrix = nalgebra::Matrix4::from(transform.matrix);
            Some((matrix, matrix.try_inverse()?, field, terrain))
        })
        .collect::<Vec<_>>();
    let mut renderables = query
        .iter(world_ref)
        .filter(|(entity, ..)| visible_in_hierarchy(world_ref, *entity))
        .flat_map(|(entity, transform, renderer, bounds, skinned)| {
            let renderable = ExtractedRenderable {
                entity,
                transform: *transform,
                mesh: skinned.map_or(renderer.mesh, |skinned| skinned.mesh),
                material: renderer.material,
                cast_shadows: renderer.cast_shadows,
                receive_shadows: renderer.receive_shadows,
                bounds: bounds.copied(),
            };
            let Some((owner, foliage)) = foliage_owner(world_ref, entity)
            else {
                return vec![renderable];
            };
            // Each part keeps its place in the group: a copy of the group
            // is placed and settled, then the part rides on it.
            let model = nalgebra::Matrix4::from(transform.matrix);
            let group = world_ref
                .get::<GlobalTransform>(owner)
                .map_or(model, |owner| nalgebra::Matrix4::from(owner.matrix));
            let part = group
                .try_inverse()
                .unwrap_or_else(nalgebra::Matrix4::identity)
                * model;
            let placements = foliage.placements();
            let drawn = (placements.len() as f32 * share).ceil() as usize;
            placements
                .into_iter()
                .take(drawn)
                .filter_map(|local| settle(foliage, group * local, &grounds))
                .map(|matrix| ExtractedRenderable {
                    transform: GlobalTransform {
                        matrix: (matrix * part).into(),
                    },
                    ..renderable
                })
                .collect()
        })
        .collect::<Vec<_>>();
    let mut terrains =
        world.query::<(Entity, &GlobalTransform, &super::TerrainChunks)>();
    let world_ref = &*world;
    for (entity, transform, chunks) in terrains.iter(world_ref) {
        if !visible_in_hierarchy(world_ref, entity) {
            continue;
        }
        renderables.extend(chunks.meshes.iter().map(|&mesh| {
            ExtractedRenderable {
                entity,
                transform: *transform,
                mesh,
                material: chunks.material,
                cast_shadows: chunks.cast_shadows,
                receive_shadows: true,
                bounds: None,
            }
        }));
    }
    // The sort is stable, so one owner's copies keep their scatter order.
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
/// The shader source of the first `T` in entity order, or empty.
fn first_source<T: Component>(
    world: &mut World,
    source: impl Fn(&T) -> &String,
) -> String {
    let mut query = world.query::<(Entity, &T)>();
    query
        .iter(world)
        .min_by_key(|(entity, _)| entity.index())
        .map(|(_, shader)| source(shader).clone())
        .unwrap_or_default()
}

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
    fn conforming_foliage_sits_on_the_ground_of_its_chosen_layers() {
        use crate::runtime::{Foliage, Heightfield, Terrain, TerrainLayer};
        let mut app = App::new();
        app.add_plugin(RenderExtractPlugin).unwrap();
        let server = AssetServer::default();
        let renderer = renderer(&server);
        app.insert_resource(server);
        // Rises 0.5 m per metre along +X; layer 1 covers x >= 0.
        let slope = |lift: f32| Heightfield {
            heights: (0..11)
                .map(|_| (0..11).map(|c| c as f32 * 0.5 + lift).collect())
                .collect(),
            spacing: 1.0,
            ..Heightfield::default()
        };
        let terrain = Terrain {
            layers: vec![TerrainLayer::default(), TerrainLayer::default()],
            splat: (0..11)
                .map(|_| {
                    (0..11)
                        .map(|c| {
                            if c >= 5 {
                                [0.0, 1.0, 0.0, 0.0]
                            } else {
                                [1.0, 0.0, 0.0, 0.0]
                            }
                        })
                        .collect()
                })
                .collect(),
            ..Terrain::default()
        };
        let ground =
            app.spawn((Transform::new([0.0, 2.0, 0.0]), slope(0.0), terrain));
        let grass = Foliage {
            count: 200,
            radius: 4.0,
            conform: true,
            layers: vec![1],
            ..Foliage::default()
        };
        let owner =
            app.spawn((Transform::new([0.0, 10.0, 0.0]), renderer, grass));
        let adrift = Foliage {
            count: 20,
            conform: true,
            ..Foliage::default()
        };
        let far =
            app.spawn((Transform::new([100.0, 0.0, 0.0]), renderer, adrift));
        let copies = |app: &mut App, entity| {
            app.update(Duration::from_secs_f64(1.0 / 60.0)).unwrap();
            app.world()
                .resource::<RenderWorld>()
                .renderables
                .iter()
                .filter(|renderable| renderable.entity == entity)
                .map(|renderable| renderable.transform.matrix[3])
                .collect::<Vec<_>>()
        };
        for lift in [0.0, 1.0] {
            if lift > 0.0 {
                app.world_mut().entity_mut(ground).insert(slope(lift));
            }
            let placed = copies(&mut app, owner);
            assert!(
                (40..160).contains(&placed.len()),
                "about half stand on layer 1: {}",
                placed.len()
            );
            for [x, y, _, _] in placed {
                assert!(x >= -0.5 - 1e-4, "a copy on layer 0 at x {x}");
                let surface = 2.0 + lift + 0.5 * (x + 5.0);
                assert!((y - surface).abs() < 1e-3, "{y} vs {surface}");
            }
        }
        // Off every heightfield, conforming copies keep the disc height.
        let placed = copies(&mut app, far);
        assert_eq!(placed.len(), 20);
        assert!(placed.iter().all(|at| at[1] == 0.0));
    }

    #[test]
    fn quality_profiles_thin_foliage_from_the_first_copies() {
        use crate::runtime::{Foliage, QualityProfile, RenderSettings};
        let mut app = App::new();
        app.add_plugin(RenderExtractPlugin).unwrap();
        let server = AssetServer::default();
        let renderer = renderer(&server);
        app.insert_resource(server);
        let foliage = Foliage {
            count: 10,
            ..Foliage::default()
        };
        let owner =
            app.spawn((Transform::default(), renderer, foliage.clone()));
        let mut copies = |quality| {
            app.world_mut().resource_mut::<RenderSettings>().quality = quality;
            app.update(Duration::from_secs_f64(1.0 / 60.0)).unwrap();
            app.world()
                .resource::<RenderWorld>()
                .renderables
                .iter()
                .filter(|renderable| renderable.entity == owner)
                .map(|renderable| renderable.transform.matrix)
                .collect::<Vec<_>>()
        };
        let all = copies(QualityProfile::High);
        assert_eq!(all.len(), 10);
        assert_eq!(copies(QualityProfile::Eco), all[..3]);
        assert_eq!(copies(QualityProfile::Balanced), all[..6]);
        assert_eq!(copies(QualityProfile::Auto), all);
        assert_eq!(foliage_share(QualityProfile::Eco), 0.3);
    }

    #[test]
    fn foliage_on_a_parent_scatters_every_part_together() {
        use crate::runtime::Foliage;
        let mut app = App::new();
        app.add_plugin(RenderExtractPlugin).unwrap();
        let server = AssetServer::default();
        let renderer = renderer(&server);
        app.insert_resource(server);
        let foliage = Foliage {
            count: 5,
            radius: 3.0,
            ..Foliage::default()
        };
        let palm =
            app.spawn((Transform::new([0.0, 10.0, 0.0]), foliage.clone()));
        let trunk = app.spawn((Transform::default(), renderer));
        let leaves = app.spawn((Transform::new([0.0, 2.0, 0.0]), renderer));
        for part in [trunk, leaves] {
            rusting_core::hierarchy::set_parent(app.world_mut(), part, palm)
                .unwrap();
        }
        let copies = |app: &mut App, entity| {
            app.update(Duration::from_secs_f64(1.0 / 60.0)).unwrap();
            app.world()
                .resource::<RenderWorld>()
                .renderables
                .iter()
                .filter(|renderable| renderable.entity == entity)
                .map(|renderable| renderable.transform.matrix[3])
                .collect::<Vec<_>>()
        };
        copies(&mut app, trunk);
        let (bases, tops) = (copies(&mut app, trunk), copies(&mut app, leaves));
        assert_eq!(bases.len(), 5);
        for ((base, top), local) in
            bases.iter().zip(&tops).zip(foliage.placements())
        {
            assert_eq!(base[0], local[(0, 3)]);
            assert_eq!(base[1], 10.0);
            // Leaves stand on their own trunk, raised by its scale.
            let scale = local.fixed_view::<3, 1>(0, 1).norm();
            assert!((top[0] - base[0]).abs() < 1e-5);
            assert!((top[1] - base[1] - 2.0 * scale).abs() < 1e-5);
        }
        let sways = app
            .world()
            .resource::<RenderWorld>()
            .foliage_sways()
            .count();
        assert_eq!(sways, 10);
        // A moved part still draws all its copies.
        app.world_mut()
            .get_mut::<Transform>(leaves)
            .unwrap()
            .position[1] = 3.0;
        assert_eq!(copies(&mut app, leaves).len(), 5);
        app.world_mut().get_mut::<Foliage>(palm).unwrap().count = 7;
        assert_eq!(copies(&mut app, leaves).len(), 7);
    }

    #[test]
    fn foliage_copies_carry_their_sway_and_the_wind_to_the_renderer() {
        use crate::runtime::{
            Collider, ColliderShape, FieldKind, Foliage, ForceField,
            PhysicsBody, RigidBody, RigidBodyKind,
        };
        let mut app = App::new();
        app.add_plugin(RenderExtractPlugin).unwrap();
        let server = AssetServer::default();
        let renderer = renderer(&server);
        app.insert_resource(server);
        let foliage = Foliage {
            count: 50,
            radius: 2.0,
            ..Foliage::default()
        };
        let owner = app.spawn((
            Transform::new([10.0, 0.0, 0.0]),
            renderer,
            foliage.clone(),
        ));
        app.spawn((Transform::default(), renderer));
        let wind = app.spawn((
            Transform::new([10.0, 0.0, 0.0]),
            PhysicsBody::default(),
            RigidBody {
                kind: RigidBodyKind::Fixed,
                ..RigidBody::default()
            },
            Collider {
                shape: ColliderShape::Box {
                    half_extents: [3.0; 3],
                },
                sensor: true,
                ..Collider::default()
            },
            ForceField {
                kind: FieldKind::Wind,
                strength: 10.0,
                direction: [1.0, 0.0, 0.0],
                ..ForceField::default()
            },
        ));
        let step = |app: &mut App| {
            for _ in 0..3 {
                app.update(Duration::from_secs_f64(1.0 / 60.0)).unwrap();
            }
        };
        step(&mut app);
        let copies = |app: &App| {
            app.world()
                .resource::<RenderWorld>()
                .renderables
                .iter()
                .filter(|renderable| renderable.entity == owner)
                .map(|renderable| renderable.transform.matrix)
                .collect::<Vec<_>>()
        };
        let standing = copies(&app);
        assert_eq!(standing.len(), 50, "the owner draws only its copies");
        assert_eq!(app.world().resource::<RenderWorld>().renderables.len(), 51);
        assert_eq!(foliage.placements(), foliage.placements());
        let render_world = app.world().resource::<RenderWorld>();
        for matrix in &standing {
            let [x, _, z, _] = matrix[3];
            assert!(((x - 10.0).powi(2) + z * z).sqrt() <= 2.0 + 1e-4);
            // The vertex shader leans copies; the CPU matrix stays upright.
            assert!(matrix[1][0].abs() < 1e-6 && matrix[1][2].abs() < 1e-6);
        }
        let sways = render_world.foliage_sways().collect::<Vec<_>>();
        assert_eq!(sways.len(), 50);
        for (at, sway) in sways {
            assert_eq!(render_world.renderables[at].entity, owner);
            assert_eq!(sway, Foliage::default().sway);
        }
        // The wind reaches the shader without GPU physics in the scene.
        assert_eq!(render_world.gpu_force_fields.len(), 1);
        assert_eq!(render_world.gpu_force_fields[0].field.strength, 10.0);
        app.world_mut().entity_mut(wind).remove::<ForceField>();
        step(&mut app);
        let render_world = app.world().resource::<RenderWorld>();
        assert!(render_world.gpu_force_fields.is_empty());
    }

    #[test]
    fn scene_wind_reaches_the_lean_and_gpu_particles_but_not_bodies() {
        use crate::runtime::{
            ParticleEmitter, ParticleSimulation, ParticleSystem, Wind,
        };
        let mut app = App::new();
        app.add_plugin(RenderExtractPlugin).unwrap();
        let wind = Wind {
            direction: [0.0, 0.0, 1.0],
            strength: 3.0,
            gusts: 0.0,
        };
        app.spawn(wind.clone());
        app.spawn((
            ParticleEmitter {
                simulation: ParticleSimulation::Gpu,
                wind: [1.0, 0.0, 0.0],
                wind_factor: 2.0,
                ..ParticleEmitter::default()
            },
            ParticleSystem::default(),
        ));
        app.update(Duration::from_secs_f64(1.0 / 60.0)).unwrap();
        let render_world = app.world().resource::<RenderWorld>();
        let lean = render_world.wind.as_ref().map(|found| &found.field);
        assert_eq!(lean, Some(&wind.field()));
        assert!(render_world.gpu_force_fields.is_empty(), "no body feels it");
        let row = render_world.gpu_particles[0].rows[5];
        assert_eq!(row[..3], [1.0, 0.0, 6.0]);
    }

    #[test]
    fn sub_emitters_follow_a_plain_gpu_parent_and_step_after_it() {
        use crate::runtime::{
            ParticleEmitter, ParticleSimulation, ParticleSystem,
        };
        let gpu = |on_parent_death| {
            (
                ParticleEmitter {
                    simulation: ParticleSimulation::Gpu,
                    max_particles: 10,
                    on_parent_death,
                    ..ParticleEmitter::default()
                },
                ParticleSystem::default(),
            )
        };
        let mut world = World::new();
        // Spawned first, so it sorts first by entity alone.
        let child = world.spawn(gpu(3)).id();
        let orphan = world.spawn(gpu(3)).id();
        let parent = world.spawn(gpu(0)).id();
        let grandchild = world.spawn(gpu(2)).id();
        world
            .entity_mut(child)
            .insert(crate::runtime::Parent(parent));
        world
            .entity_mut(grandchild)
            .insert(crate::runtime::Parent(child));
        let batches = collect_gpu_particles(&mut world);
        let found = batches
            .iter()
            .map(|batch| (batch.entity, batch.parent, batch.max_particles))
            .collect::<Vec<_>>();
        // The sub-emitter steps last, with a slot per parent slot spawn.
        assert_eq!(found[3], (child, Some((parent, 3)), 30));
        // No parent, and no sub-emitter parent: plain emitters.
        for plain in [orphan, parent, grandchild] {
            assert!(found[..3].contains(&(plain, None, 10)));
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
    fn viewmodels_extract_only_under_a_viewmodel_camera() {
        use crate::runtime::{RenderLayer, ViewmodelCamera};
        let mut app = App::new();
        app.add_plugin(RenderExtractPlugin).unwrap();
        let server = AssetServer::default();
        let camera = app.spawn((
            Transform::default(),
            Camera {
                active: true,
                ..Camera::default()
            },
        ));
        let tool = app.spawn((
            Transform::default(),
            renderer(&server),
            RenderLayer::Viewmodel,
        ));
        app.spawn((
            Transform::default(),
            renderer(&server),
            RenderLayer::World,
        ));
        app.update(Duration::ZERO).unwrap();
        let render_world = app.world().resource::<RenderWorld>();
        assert!(render_world.viewmodels.is_empty());
        assert_eq!(render_world.viewmodel_fov, None);

        app.world_mut().entity_mut(camera).insert(ViewmodelCamera {
            vertical_fov_radians: 1.0,
        });
        app.update(Duration::ZERO).unwrap();
        let render_world = app.world().resource::<RenderWorld>();
        assert_eq!(render_world.viewmodels, vec![tool]);
        assert_eq!(render_world.viewmodel_fov, Some(1.0));
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
    fn sprites_draw_in_canvas_order_and_batch_by_image() {
        use crate::runtime::Sprite;
        let mut app = App::new();
        app.add_plugin(RenderExtractPlugin).unwrap();
        let mut textures = crate::assets::Assets::default();
        let image = || crate::assets::TextureAsset {
            size: [1, 1],
            rgba8: vec![255; 4],
            color_space: crate::assets::TextureColorSpace::Srgb,
            sampler: crate::assets::TextureSampler::default(),
        };
        let a = textures.insert(image());
        let b = textures.insert(image());
        let sprite = |layer, handle, z: f32| {
            (
                Transform::new([0.0, 0.0, z]),
                Sprite {
                    texture: "x.png".into(),
                    layer,
                    handle: Some(handle),
                    ..Sprite::default()
                },
            )
        };
        // Spawned out of order: the higher layer first, nearer z first.
        app.spawn(sprite(1, a, -5.0));
        app.spawn(sprite(0, b, 2.0));
        app.spawn(sprite(0, a, 1.0));
        app.spawn(sprite(0, a, 1.0));
        app.spawn((
            Transform::new([3.0, 0.0, 0.0]),
            Sprite {
                texture: "missing.png".into(),
                ..Sprite::default()
            },
        ));
        app.spawn((
            Transform::new([3.0, 0.0, 0.0]),
            Sprite {
                size: [2.0, 4.0],
                region: [0.25, 0.0, 0.5, 1.0],
                flip_x: true,
                layer: 0,
                ..Sprite::default()
            },
        ));
        app.update(Duration::ZERO).unwrap();
        let sprites = &app.world().resource::<RenderWorld>().sprites;
        assert_eq!(sprites.len(), 5, "the unloaded image is skipped");
        let order: Vec<_> = sprites
            .iter()
            .map(|sprite| (sprite.layer, sprite.matrix[3][2], sprite.texture))
            .collect();
        assert_eq!(
            order,
            [
                (0, 0.0, None),
                (0, 1.0, Some(a)),
                (0, 1.0, Some(a)),
                (0, 2.0, Some(b)),
                (1, -5.0, Some(a)),
            ],
            "layer, then z, then entity order"
        );
        assert_eq!(sprite_batches(sprites), [0..1, 1..3, 3..4, 4..5]);
        let corner = nalgebra::Matrix4::from(sprites[0].matrix)
            * nalgebra::Vector4::new(0.5, 0.5, 0.0, 1.0);
        assert_eq!(corner, nalgebra::Vector4::new(4.0, 2.0, 0.0, 1.0));
        assert_eq!(sprites[0].uv, [0.5, 0.0, 0.25, 1.0], "flip_x swaps u");
    }

    #[test]
    fn in_scene_sprites_extract_before_every_canvas_sprite() {
        use crate::runtime::Sprite;
        let mut app = App::new();
        app.add_plugin(RenderExtractPlugin).unwrap();
        let sprite = |layer, in_scene, z: f32| {
            (
                Transform::new([0.0, 0.0, z]),
                Sprite {
                    layer,
                    in_scene,
                    ..Sprite::default()
                },
            )
        };
        app.spawn(sprite(-3, false, 0.0));
        app.spawn(sprite(5, true, 0.0));
        app.spawn(sprite(0, true, -1.0));
        app.update(Duration::ZERO).unwrap();
        let order: Vec<_> = app
            .world()
            .resource::<RenderWorld>()
            .sprites
            .iter()
            .map(|sprite| (sprite.in_scene, sprite.layer))
            .collect();
        assert_eq!(order, [(true, 0), (true, 5), (false, -3)]);
    }

    #[test]
    fn sprite_normal_maps_load_as_linear_data_and_split_batches() {
        use crate::runtime::Sprite;
        let mut app = App::new();
        app.add_plugin(crate::assets::AssetPlugin).unwrap();
        app.add_plugin(RenderExtractPlugin).unwrap();
        let normal = app
            .world_mut()
            .resource_mut::<crate::assets::AssetServer>()
            .textures
            .insert_with_path(
                "bumps.png",
                crate::assets::TextureAsset {
                    size: [1, 1],
                    rgba8: vec![128, 128, 255, 255],
                    color_space: crate::assets::TextureColorSpace::Srgb,
                    sampler: crate::assets::TextureSampler::default(),
                },
            )
            .unwrap();
        // Same image and layer; only the normal map differs.
        app.spawn((Transform::default(), Sprite::default()));
        app.spawn((
            Transform::default(),
            Sprite {
                normal_map: "bumps.png".into(),
                ..Sprite::default()
            },
        ));
        app.update(Duration::ZERO).unwrap();
        let assets = app.world().resource::<crate::assets::AssetServer>();
        assert_eq!(
            assets.textures.get(normal).unwrap().color_space,
            crate::assets::TextureColorSpace::Linear
        );
        let sprites = &app.world().resource::<RenderWorld>().sprites;
        assert_eq!(
            sprites
                .iter()
                .map(|sprite| sprite.normal_map)
                .collect::<Vec<_>>(),
            [None, Some(normal)]
        );
        assert_eq!(sprite_batches(sprites), [0..1, 1..2]);
    }

    #[test]
    fn light_occluders_2d_extract_closed_world_edges_in_entity_order() {
        use crate::runtime::LightOccluder2d;
        let mut app = App::new();
        app.add_plugin(RenderExtractPlugin).unwrap();
        // Scaled by 2 and moved, so points land in world space.
        let mut transform = Transform::new([10.0, 20.0, 5.0]);
        transform.scale = [2.0; 3];
        app.spawn((
            transform,
            LightOccluder2d {
                points: vec![[0.0, 0.0], [1.0, 0.0], [0.0, 1.0]],
            },
        ));
        app.spawn((
            Transform::default(),
            LightOccluder2d {
                points: vec![[0.0, 0.0], [3.0, 0.0]],
            },
        ));
        app.update(Duration::ZERO).unwrap();
        let render_world = app.world().resource::<RenderWorld>();
        assert_eq!(
            render_world.occluder_edges_2d,
            [
                [10.0, 20.0, 12.0, 20.0],
                [12.0, 20.0, 10.0, 22.0],
                [10.0, 22.0, 10.0, 20.0],
                [0.0, 0.0, 3.0, 0.0],
                [3.0, 0.0, 0.0, 0.0],
            ]
        );
    }

    #[test]
    fn particles_2d_extract_emitters_in_canvas_order() {
        use crate::runtime::{ParticleEmitter, Particles2d};
        let mut app = App::new();
        app.add_plugin(RenderExtractPlugin).unwrap();
        let high = app.spawn((
            Transform::new([0.0, 0.0, -5.0]),
            ParticleEmitter::default(),
            Particles2d { layer: 1 },
        ));
        let near = app.spawn((
            Transform::new([0.0, 0.0, 2.0]),
            ParticleEmitter::default(),
            Particles2d { layer: 0 },
        ));
        let far = app.spawn((
            Transform::new([0.0, 0.0, 1.0]),
            ParticleEmitter::default(),
            Particles2d { layer: 0 },
        ));
        // No emitter, so nothing to draw.
        app.spawn((Transform::default(), Particles2d { layer: 0 }));
        app.update(Duration::ZERO).unwrap();
        let render_world = app.world().resource::<RenderWorld>();
        assert_eq!(
            render_world.particles_2d,
            [(far, 0, 1.0), (near, 0, 2.0), (high, 1, -5.0)]
        );
    }

    #[test]
    fn lights_2d_extract_in_entity_order_with_the_first_ambient() {
        use crate::runtime::{Ambient2d, Light2d};
        let mut app = App::new();
        app.add_plugin(RenderExtractPlugin).unwrap();
        app.spawn((
            Transform::new([1.0, 2.0, 3.0]),
            Light2d {
                color: [1.0, 0.5, 0.0],
                intensity: 2.0,
                radius: 4.0,
                height: 1.5,
            },
        ));
        app.spawn((Transform::default(), Light2d::default()));
        app.spawn(Ambient2d {
            color: [0.1, 0.2, 0.3],
        });
        app.spawn(Ambient2d::default());
        app.update(Duration::ZERO).unwrap();
        let render_world = app.world().resource::<RenderWorld>();
        assert_eq!(
            render_world.lights_2d,
            [
                ExtractedLight2d {
                    position: [1.0, 2.0, 3.0],
                    color: [2.0, 1.0, 0.0],
                    radius: 4.0,
                    height: 1.5,
                },
                ExtractedLight2d {
                    position: [0.0; 3],
                    color: [1.0; 3],
                    radius: 5.0,
                    height: 0.5,
                },
            ]
        );
        assert_eq!(render_world.ambient_2d, Some([0.1, 0.2, 0.3]));
    }

    #[test]
    fn particle_shaders_reach_the_renderer_only_on_emitters() {
        use crate::runtime::{ParticleEmitter, ParticleShader};
        let mut app = App::new();
        app.add_plugin(RenderExtractPlugin).unwrap();
        let shader = || ParticleShader {
            source: "#rusting particle 1".into(),
        };
        let emitter = app.spawn((ParticleEmitter::default(), shader()));
        app.spawn(shader());
        app.update(Duration::ZERO).unwrap();
        let shaders = &app.world().resource::<RenderWorld>().particle_shaders;
        assert_eq!(shaders.len(), 1, "{shaders:?}");
        assert_eq!(shaders[&emitter], shader().source);
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
    fn simulation_lod_extracts_observers_and_picks_each_body_rate() {
        let mut app = App::new();
        app.add_plugin(RenderExtractPlugin).unwrap();
        app.spawn((
            Transform::new([1.0, 2.0, 3.0]),
            super::super::SimulationObserver,
        ));
        app.update(Duration::ZERO).unwrap();
        // No policy, no LOD.
        assert!(app
            .world()
            .resource::<RenderWorld>()
            .gpu_observers
            .is_empty());
        let lod = super::super::GpuSimulationLod {
            full_radius: 10.0,
            reduced_radius: 30.0,
            reduced_stride: 4,
        };
        app.insert_resource(lod);
        app.update(Duration::ZERO).unwrap();
        let render_world = app.world().resource::<RenderWorld>();
        assert_eq!(render_world.gpu_lod, Some(lod));
        assert_eq!(render_world.gpu_observers, vec![[1.0, 2.0, 3.0]]);
        // Near: every tick. Mid: one 4-tick step on ticks where
        // tick + slot is a multiple of 4. Far: frozen.
        assert_eq!(lod.steps(100.0, 7, 0), 1);
        let mid: Vec<_> =
            (0..8).map(|tick| lod.steps(400.0, tick, 1)).collect();
        assert_eq!(mid, [0, 0, 0, 4, 0, 0, 0, 4]);
        assert_eq!(lod.steps(901.0, 3, 1), 0);
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
