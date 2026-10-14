//! Legacy `Engine` facade built on the ECS runtime.
//!
//! `Engine` keeps the original builder API (`add_cube`, `add_sphere`,
//! `add_gltf`, `set_light`, ...) but spawns ECS entities into an [`App`]
//! and runs through the same window runner as cooked projects.

use std::collections::{HashMap, HashSet};
use std::f32::consts::FRAC_PI_2;

use bevy_ecs::prelude::*;

#[cfg(feature = "gltf")]
use crate::assets::ImportedGltfNode;
use crate::assets::{
    procedural_sphere_mesh, AssetError, AssetPlugin, AssetServer, Handle,
    MaterialAsset, MaterialModel, MeshAsset, TextureAsset,
};
use crate::core::{Material, Physics, Transform};
use crate::rendering::compute_profile::ComputeShaderType;
use crate::runtime::{
    AmbientLight, App, Camera, Collider, ColliderShape, DirectionalLight,
    FrameTime, HybridPhysicsPlugin, KeyCode, MeshRenderer, PhysicsBody,
    PhysicsSolver, Projection, RenderExtractPlugin, RenderSettings, RigidBody,
    RigidBodyKind, RuntimeInput, ScheduleStage, SimulationClass,
};
use crate::CollisionType;

/// Fly camera driven by WASD, Space/Left Control and mouse look.
///
/// Escape captures the mouse; movement and look only work while captured.
/// Hold Left Shift to move twice as fast.
#[derive(Resource, Clone, Copy, Debug)]
pub struct PerspectiveCamera {
    /// Camera position in world space (X, Y, Z)
    pub position: [f32; 3],
    /// Horizontal angle in radians; 90 degrees looks down -Z.
    pub yaw: f32,
    /// Vertical angle in radians; positive looks down.
    pub pitch: f32,
    /// Vertical field of view in radians
    pub fov: f32,
    /// Near clipping plane distance
    pub near: f32,
    /// Far clipping plane distance
    pub far: f32,
}

impl PerspectiveCamera {
    /// Creates a camera at `[0, 5, 20]` looking toward the origin.
    ///
    /// # Arguments
    /// * `fov` - Vertical field of view in degrees
    /// * `near` - Near clipping plane
    /// * `far` - Far clipping plane
    pub fn new(fov: f32, near: f32, far: f32) -> Self {
        Self {
            position: [0.0, 5.0, 20.0],
            yaw: 90.0f32.to_radians(),
            pitch: 0.0,
            fov: fov.to_radians(),
            near,
            far,
        }
    }

    /// Turns the camera by a raw mouse delta in pixels.
    pub fn look(&mut self, delta: [f32; 2]) {
        self.yaw += delta[0] * 0.001;
        self.pitch = (self.pitch + delta[1] * 0.001).clamp(-1.5, 1.5);
    }

    /// Moves the camera from held keys when `mouse_captured` is true.
    ///
    /// # Arguments
    /// * `keys` - Set of currently pressed keys
    /// * `sprint` - Movement speed multiplier (2.0 for sprint, 1.0 for normal)
    /// * `dt` - Time since last frame in seconds
    /// * `mouse_captured` - Whether movement is active
    pub fn update(
        &mut self,
        keys: &HashSet<KeyCode>,
        sprint: f32,
        dt: f32,
        mouse_captured: bool,
    ) {
        if !mouse_captured {
            return;
        }
        let (sin, cos) = self.yaw.sin_cos();
        let forward = [-cos, 0.0, -sin];
        let right = [sin, 0.0, -cos];
        let speed = 50.0 * sprint * dt;
        for (key, direction, sign) in [
            (KeyCode::KeyW, forward, 1.0),
            (KeyCode::KeyS, forward, -1.0),
            (KeyCode::KeyD, right, 1.0),
            (KeyCode::KeyA, right, -1.0),
            (KeyCode::Space, [0.0, 1.0, 0.0], 1.0),
            (KeyCode::ControlLeft, [0.0, 1.0, 0.0], -1.0),
        ] {
            if keys.contains(&key) {
                for (position, axis) in self.position.iter_mut().zip(direction)
                {
                    *position += axis * speed * sign;
                }
            }
        }
    }

    /// ECS transform for this camera (forward is the transform's -Z axis).
    pub fn transform(&self) -> Transform {
        Transform {
            position: self.position,
            rotation: [-self.pitch, FRAC_PI_2 - self.yaw, 0.0],
            ..Transform::default()
        }
    }
}

/// Entity that [`fly_camera`] moves.
#[derive(Resource)]
struct FlyCameraEntity(Entity);

/// Legacy fly-camera controls over the ECS camera entity.
fn fly_camera(
    time: Res<FrameTime>,
    mut input: ResMut<RuntimeInput>,
    mut camera: ResMut<PerspectiveCamera>,
    target: Res<FlyCameraEntity>,
    mut transforms: Query<&mut Transform>,
) {
    if input.key_just_pressed(KeyCode::Escape) {
        let captured = !input.cursor_captured();
        input.set_cursor_captured(captured);
    }
    let captured = input.cursor_captured();
    if captured {
        camera.look(input.mouse_motion());
    }
    let keys: HashSet<KeyCode> = [
        KeyCode::KeyW,
        KeyCode::KeyA,
        KeyCode::KeyS,
        KeyCode::KeyD,
        KeyCode::Space,
        KeyCode::ControlLeft,
    ]
    .into_iter()
    .filter(|&key| input.key_held(key))
    .collect();
    let sprint = if input.key_held(KeyCode::ShiftLeft) {
        2.0
    } else {
        1.0
    };
    camera.update(&keys, sprint, time.delta_seconds(), captured);
    if let Ok(mut transform) = transforms.get_mut(target.0) {
        let next = camera.transform();
        transform.position = next.position;
        transform.rotation = next.rotation;
    }
}

/// Maps a legacy compute shader to ECS simulation settings.
fn simulation_for(
    shader: ComputeShaderType,
) -> (SimulationClass, PhysicsSolver) {
    match shader {
        ComputeShaderType::FullPhysics | ComputeShaderType::GridCollision => {
            (SimulationClass::Gpu, PhysicsSolver::Full)
        }
        ComputeShaderType::MidPhysic => {
            (SimulationClass::Gpu, PhysicsSolver::Simplified)
        }
        ComputeShaderType::NoCollision => {
            (SimulationClass::Gpu, PhysicsSolver::NoCollision)
        }
        ComputeShaderType::Space => {
            (SimulationClass::Gpu, PhysicsSolver::Space)
        }
        ComputeShaderType::Static => {
            (SimulationClass::Static, PhysicsSolver::Full)
        }
        ComputeShaderType::Empty
        | ComputeShaderType::GridBuild
        | ComputeShaderType::Cull => {
            (SimulationClass::None, PhysicsSolver::Full)
        }
    }
}

fn body_kind(simulation: SimulationClass) -> RigidBodyKind {
    if simulation == SimulationClass::Static {
        RigidBodyKind::Fixed
    } else {
        RigidBodyKind::Dynamic
    }
}

/// Maps legacy physics settings to ECS physics components.
fn physics_components(phys: &Physics) -> (PhysicsBody, RigidBody, Collider) {
    let (simulation, solver) = simulation_for(phys.compute_shader);
    let shape = match phys.collision_type {
        CollisionType::Box => ColliderShape::Box {
            half_extents: [0.5; 3],
        },
        CollisionType::Sphere => ColliderShape::Sphere { radius: 0.5 },
    };
    (
        PhysicsBody {
            simulation,
            solver,
            custom_shader: None,
        },
        RigidBody {
            kind: body_kind(simulation),
            mass: phys.mass,
            linear_velocity: phys.linear_velocity,
            angular_velocity: phys.angular_velocity,
            gravity_scale: phys.gravity_scale,
        },
        Collider {
            shape,
            friction: phys.friction,
            restitution: phys.bounciness,
            ..Collider::default()
        },
    )
}

/// ECS rotation whose -Z axis points along `direction`.
pub(crate) fn rotation_facing(direction: [f32; 3]) -> [f32; 3] {
    let length = direction.iter().map(|axis| axis * axis).sum::<f32>().sqrt();
    if length <= f32::EPSILON {
        return [-FRAC_PI_2, 0.0, 0.0];
    }
    let [x, y, z] = direction.map(|axis| axis / length);
    [y.asin(), (-x).atan2(-z), 0.0]
}

/// High-level scene builder that runs on the ECS runtime.
///
/// Add objects, then call [`Engine::run`] to open the window.
pub struct Engine {
    /// Window title
    title: String,
    /// ECS world that holds the scene
    runtime: App,
    /// Rendering and frame-rate options
    pub render_settings: RenderSettings,
    /// The fly camera used when the window opens
    pub camera: PerspectiveCamera,
    /// Light position, color and intensity from [`Engine::set_light`]
    light: ([f32; 3], [f32; 3], f32),
    /// Lighting model forced on every object at run time
    scene_shader: Option<MaterialModel>,
    /// Physics shader forced on every body at run time
    scene_physic: Option<ComputeShaderType>,
    /// Sphere meshes by subdivision level
    sphere_meshes: HashMap<u32, Handle<MeshAsset>>,
    /// Materials by the `Debug` text of their legacy material
    materials: HashMap<String, Handle<MaterialAsset>>,
    /// Textures by index returned from [`Engine::load_texture`]
    textures: Vec<Handle<TextureAsset>>,
    /// Loaded textures map: Path -> texture index
    pub textures_cache: HashMap<String, usize>,
    /// Imported glTF node lists by path
    #[cfg(feature = "gltf")]
    pub gltf_cache: HashMap<String, Vec<ImportedGltfNode>>,
}

impl Engine {
    /// Creates an engine with default render settings.
    ///
    /// # Examples
    /// ```no_run
    /// let engine = rusting_engine::Engine::new("My Game");
    /// ```
    pub fn new(title: &str) -> Self {
        Self::with_render_settings(title, RenderSettings::default())
    }

    /// Creates an engine with explicit rendering and frame-rate settings.
    pub fn with_render_settings(
        title: &str,
        render_settings: RenderSettings,
    ) -> Self {
        let mut runtime = App::new();
        runtime
            .add_plugin(AssetPlugin)
            .and_then(|runtime| runtime.add_plugin(HybridPhysicsPlugin))
            .and_then(|runtime| runtime.add_plugin(RenderExtractPlugin))
            .expect("built-in plugins register once");
        Self {
            title: title.to_string(),
            runtime,
            render_settings,
            camera: PerspectiveCamera::new(45.0, 0.1, 1000.0),
            light: ([0.0, 10.0, 0.0], [1.0; 3], 50.0),
            scene_shader: None,
            scene_physic: None,
            sphere_meshes: HashMap::new(),
            materials: HashMap::new(),
            textures: Vec::new(),
            textures_cache: HashMap::new(),
            #[cfg(feature = "gltf")]
            gltf_cache: HashMap::new(),
        }
    }

    fn assets(&mut self) -> Mut<'_, AssetServer> {
        self.runtime.world_mut().resource_mut::<AssetServer>()
    }

    fn texture(&self, index: usize) -> Handle<TextureAsset> {
        *self.textures.get(index).unwrap_or_else(|| {
            panic!("texture index {index} was not returned by load_texture")
        })
    }

    /// Converts a legacy material to a shared ECS material handle.
    fn material(&mut self, mat: &Material) -> Handle<MaterialAsset> {
        let key = format!("{mat:?}");
        if let Some(&handle) = self.materials.get(&key) {
            return handle;
        }
        let asset = self.material_asset(mat, MaterialAsset::default());
        let handle = self.assets().materials.insert(asset);
        self.materials.insert(key, handle);
        handle
    }

    /// Applies legacy material values over `base`, keeping `base` textures
    /// the legacy material does not set.
    fn material_asset(
        &self,
        mat: &Material,
        base: MaterialAsset,
    ) -> MaterialAsset {
        let [r, g, b] = mat.color;
        MaterialAsset {
            model: mat.model,
            base_color: [r, g, b, 1.0],
            emissive: mat.color.map(|channel| channel * mat.emissive),
            metallic: mat.metalness,
            roughness: mat.roughness,
            base_color_texture: mat
                .base_color_texture
                .map(|index| self.texture(index))
                .or(base.base_color_texture),
            metallic_roughness_texture: mat
                .metallic_roughness_texture
                .map(|index| self.texture(index))
                .or(base.metallic_roughness_texture),
            ..base
        }
    }

    fn spawn(
        &mut self,
        transform: Transform,
        mesh: Handle<MeshAsset>,
        material: Handle<MaterialAsset>,
        phys: &Physics,
    ) {
        self.runtime.spawn((
            transform,
            MeshRenderer {
                mesh,
                material,
                cast_shadows: true,
                receive_shadows: true,
            },
            physics_components(phys),
        ));
    }

    /// Sets the main light.
    ///
    /// The light shines from `pos` toward the origin. Intensity fades with
    /// distance the way the old point light did, capped at full sunlight.
    ///
    /// # Arguments
    /// * `pos` - Light position in world space (X, Y, Z)
    /// * `color` - Light color as RGB values (typically 0.0-1.0)
    /// * `intensity` - Light brightness multiplier
    pub fn set_light(
        &mut self,
        pos: [f32; 3],
        color: [f32; 3],
        intensity: f32,
    ) {
        self.light = (pos, color, intensity);
    }

    /// Adds a unit cube.
    ///
    /// # Arguments
    /// * `transform` - Position, rotation, and scale of the cube
    /// * `mat` - Material properties (color, shader, roughness, metalness)
    /// * `phys` - Physics properties (collision type, mass, bounciness, etc.)
    pub fn add_cube(
        &mut self,
        transform: Transform,
        mat: &Material,
        phys: &Physics,
    ) {
        let mesh = self.assets().fallback_mesh;
        let material = self.material(mat);
        self.spawn(transform, mesh, material, phys);
    }

    /// Adds a sphere of radius 1 (scaled by `transform`).
    ///
    /// # Arguments
    /// * `transform` - Position, rotation, and scale of the sphere
    /// * `mat` - Material properties (color, shader, roughness, metalness)
    /// * `phys` - Physics properties (collision type, mass, bounciness, etc.)
    /// * `subdiv` - Number of subdivisions (higher = smoother sphere, costs more)
    pub fn add_sphere(
        &mut self,
        mut transform: Transform,
        mat: &Material,
        phys: &Physics,
        subdiv: u32,
    ) {
        let mesh = match self.sphere_meshes.get(&subdiv) {
            Some(&mesh) => mesh,
            None => {
                let mesh =
                    self.assets().meshes.insert(procedural_sphere_mesh(subdiv));
                self.sphere_meshes.insert(subdiv, mesh);
                mesh
            }
        };
        // The procedural sphere has radius 0.5; the legacy one had radius 1.
        transform.scale = transform.scale.map(|axis| axis * 2.0);
        let material = self.material(mat);
        self.spawn(transform, mesh, material, phys);
    }

    /// Loads an image file as a texture and returns its index.
    ///
    /// Repeated calls with the same path return the cached index.
    ///
    /// # Errors
    /// Returns [`AssetError`] if the file cannot be read or decoded.
    pub fn load_texture(&mut self, path: &str) -> Result<usize, AssetError> {
        if let Some(&index) = self.textures_cache.get(path) {
            return Ok(index);
        }
        let handle = self.assets().load_texture(path)?;
        self.textures.push(handle);
        let index = self.textures.len() - 1;
        self.textures_cache.insert(path.to_string(), index);
        Ok(index)
    }

    /// Adds every mesh of a glTF file as its own object.
    ///
    /// `mat` overrides the imported color, roughness, metalness and
    /// emission; imported textures stay unless `mat` sets its own. Cameras
    /// and lights in the file are ignored.
    ///
    /// # Arguments
    /// * `transform` - Position, rotation, scale of the model
    /// * `mat` - Material properties to apply to all meshes
    /// * `phys` - Physics properties for every mesh
    /// * `path` - Path to the .gltf or .glb file
    ///
    /// # Errors
    /// Returns [`AssetError`] if the file cannot be imported.
    #[cfg(feature = "gltf")]
    pub fn add_gltf(
        &mut self,
        transform: Transform,
        mat: &Material,
        phys: &Physics,
        path: &str,
    ) -> Result<(), AssetError> {
        let nodes = match self.gltf_cache.get(path) {
            Some(nodes) => nodes.clone(),
            None => {
                let nodes = self.assets().import_gltf_scene(path)?;
                self.gltf_cache.insert(path.to_string(), nodes.clone());
                nodes
            }
        };
        let root = nalgebra::Matrix4::from(transform.to_matrix());
        for node in &nodes {
            let mut world = nalgebra::Matrix4::from(node.transform.to_matrix());
            let mut parent = node.parent;
            while let Some(index) = parent {
                world =
                    nalgebra::Matrix4::from(nodes[index].transform.to_matrix())
                        * world;
                parent = nodes[index].parent;
            }
            let node_transform = Transform::from_matrix(root * world);
            for primitive in &node.primitives {
                let base = self
                    .assets()
                    .materials
                    .get(primitive.material)
                    .cloned()
                    .unwrap_or_default();
                let asset = self.material_asset(mat, base);
                let material = self.assets().materials.insert(asset);
                self.spawn(node_transform, primitive.mesh, material, phys);
            }
        }
        Ok(())
    }

    /// Forces one lighting model on every object.
    pub fn set_scene_shader(&mut self, model: MaterialModel) {
        self.scene_shader = Some(model);
    }

    /// Returns objects to their own material model.
    pub fn clear_scene_shader(&mut self) {
        self.scene_shader = None;
    }

    /// Forces one physics shader on every body.
    ///
    /// # Arguments
    /// * `shader` - The compute shader type to use for physics
    pub fn set_scene_physic(&mut self, shader: ComputeShaderType) {
        self.scene_physic = Some(shader);
    }

    /// Returns bodies to their own physics shader.
    pub fn clear_scene_physic(&mut self) {
        self.scene_physic = None;
    }

    /// Applies scene overrides, lights and the camera, and returns the
    /// runtime ready to run.
    fn into_app(mut self) -> (String, App) {
        let world = self.runtime.world_mut();
        if let Some(model) = self.scene_shader {
            let handles: Vec<_> = world
                .query::<&MeshRenderer>()
                .iter(world)
                .map(|renderer| renderer.material)
                .collect();
            let mut assets = world.resource_mut::<AssetServer>();
            for handle in handles {
                if let Some(material) = assets.materials.get_mut(handle) {
                    material.model = model;
                }
            }
        }
        if let Some(shader) = self.scene_physic {
            let (simulation, solver) = simulation_for(shader);
            for (mut body, mut rigid) in world
                .query::<(&mut PhysicsBody, &mut RigidBody)>()
                .iter_mut(world)
            {
                body.simulation = simulation;
                body.solver = solver;
                rigid.kind = body_kind(simulation);
            }
        }

        let (pos, color, intensity) = self.light;
        let distance_squared = pos.iter().map(|axis| axis * axis).sum::<f32>();
        let radiance = intensity / (1.0 + 0.005 * distance_squared);
        self.runtime.spawn((
            Transform {
                rotation: rotation_facing(pos.map(|axis| -axis)),
                ..Transform::default()
            },
            DirectionalLight {
                color,
                illuminance: 100_000.0 * radiance.min(1.0),
                ..DirectionalLight::default()
            },
        ));
        self.runtime.spawn(AmbientLight {
            color,
            intensity: 0.05,
        });

        let camera = self.runtime.spawn((
            self.camera.transform(),
            Camera {
                projection: Projection::Perspective {
                    vertical_fov_radians: self.camera.fov,
                    near: self.camera.near,
                    far: self.camera.far,
                },
                active: true,
                priority: 100,
                viewport: None,
            },
        ));
        self.runtime
            .insert_resource(self.render_settings)
            .insert_resource(self.camera)
            .insert_resource(FlyCameraEntity(camera))
            .add_systems(ScheduleStage::Update, fly_camera);
        (self.title, self.runtime)
    }

    /// Opens the window and runs until it closes.
    ///
    /// Press Escape to capture/release the mouse for camera look.
    /// Hold Left Shift to sprint.
    #[cfg(feature = "window")]
    pub fn run(self) {
        let (title, runtime) = self.into_app();
        if let Err(error) = crate::project_runner::run_windowed(title, runtime)
        {
            eprintln!("engine stopped: {error}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn forward(transform: &Transform) -> [f32; 3] {
        let matrix = nalgebra::Matrix4::from(transform.to_matrix());
        let forward =
            matrix.transform_vector(&nalgebra::Vector3::new(0.0, 0.0, -1.0));
        [forward.x, forward.y, forward.z]
    }

    fn assert_near(actual: [f32; 3], expected: [f32; 3]) {
        for (a, e) in actual.iter().zip(expected) {
            assert!((a - e).abs() < 1e-4, "{actual:?} != {expected:?}");
        }
    }

    #[test]
    fn cube_and_sphere_spawn_renderable_physics_bodies() {
        let mut engine = Engine::new("test");
        let phys = Physics::default()
            .compute_shader(ComputeShaderType::MidPhysic)
            .collision_type(CollisionType::Box)
            .mass(3.0);
        engine.add_cube(
            Transform::default(),
            &Material::standard().build(),
            &phys,
        );
        engine.add_sphere(
            Transform::default(),
            &Material::standard().color([1.0, 0.0, 0.0]).build(),
            &Physics::default()
                .compute_shader(ComputeShaderType::Static)
                .collision_type(CollisionType::Sphere),
            2,
        );
        let (_, mut app) = engine.into_app();
        let world = app.world_mut();
        let mut bodies: Vec<_> = world
            .query::<(&Transform, &PhysicsBody, &RigidBody, &Collider)>()
            .iter(world)
            .map(|(transform, body, rigid, collider)| {
                (
                    transform.scale,
                    body.simulation,
                    body.solver,
                    rigid.kind,
                    rigid.mass,
                    collider.shape,
                )
            })
            .collect();
        bodies.sort_by(|a, b| a.0[0].total_cmp(&b.0[0]));
        assert_eq!(
            bodies,
            vec![
                (
                    [1.0; 3],
                    SimulationClass::Gpu,
                    PhysicsSolver::Simplified,
                    RigidBodyKind::Dynamic,
                    3.0,
                    ColliderShape::Box {
                        half_extents: [0.5; 3]
                    },
                ),
                (
                    [2.0; 3],
                    SimulationClass::Static,
                    PhysicsSolver::Full,
                    RigidBodyKind::Fixed,
                    1.0,
                    ColliderShape::Sphere { radius: 0.5 },
                ),
            ]
        );
        assert_eq!(world.query::<&MeshRenderer>().iter(world).count(), 2);
        assert_eq!(world.query::<&Camera>().iter(world).count(), 1);
        assert_eq!(world.query::<&DirectionalLight>().iter(world).count(), 1);
    }

    #[test]
    fn scene_overrides_apply_at_run() {
        let mut engine = Engine::new("test");
        engine.add_cube(
            Transform::default(),
            &Material::standard().build(),
            &Physics::default(),
        );
        engine.set_scene_shader(MaterialModel::Unlit);
        engine.set_scene_physic(ComputeShaderType::NoCollision);
        let (_, mut app) = engine.into_app();
        let world = app.world_mut();
        let (renderer, body) = world
            .query::<(&MeshRenderer, &PhysicsBody)>()
            .single(world)
            .unwrap();
        let (material, solver) = (renderer.material, body.solver);
        assert_eq!(solver, PhysicsSolver::NoCollision);
        let assets = world.resource::<AssetServer>();
        assert_eq!(
            assets.materials.get(material).unwrap().model,
            MaterialModel::Unlit
        );
    }

    #[test]
    fn camera_transform_looks_where_w_moves() {
        let mut camera = PerspectiveCamera::new(45.0, 0.1, 100.0);
        assert_near(forward(&camera.transform()), [0.0, 0.0, -1.0]);
        camera.yaw = 0.3;
        let start = camera.position;
        // 50 units/s for 0.02 s moves exactly one unit.
        camera.update(&HashSet::from([KeyCode::KeyW]), 1.0, 0.02, true);
        let moved = [
            camera.position[0] - start[0],
            camera.position[1] - start[1],
            camera.position[2] - start[2],
        ];
        assert_near(forward(&camera.transform()), moved);
        camera.pitch = 0.4;
        assert!(forward(&camera.transform())[1] < 0.0, "pitch looks down");
    }

    #[test]
    fn light_faces_away_from_its_position() {
        let transform = Transform {
            rotation: rotation_facing([-1.0, -2.0, -3.0]),
            ..Transform::default()
        };
        let length = 14.0f32.sqrt();
        assert_near(
            forward(&transform),
            [-1.0 / length, -2.0 / length, -3.0 / length],
        );
    }

    #[test]
    #[cfg_attr(
        not(feature = "gpu-tests"),
        ignore = "run with `--features gpu-tests` on a machine with a Vulkan driver"
    )]
    fn engine_cubes_and_spheres_render_through_ecs() {
        use crate::rendering::scene_renderer::{
            SceneRenderOptions, SceneRenderer,
        };
        use crate::rendering::swapchain::OFFSCREEN_COLOR_FORMAT;
        use crate::runtime::RenderWorld;
        use std::sync::Arc;
        use vulkano::command_buffer::allocator::StandardCommandBufferAllocator;
        use vulkano::image::view::ImageView;
        use vulkano::image::{Image, ImageCreateInfo, ImageUsage};
        use vulkano::memory::allocator::{
            AllocationCreateInfo, StandardMemoryAllocator,
        };
        use vulkano::sync::GpuFuture;

        if vulkano::VulkanLibrary::new().is_err() {
            eprintln!("skipping: no Vulkan driver present");
            return;
        }
        let mut engine = Engine::new("test");
        let still =
            Physics::default().compute_shader(ComputeShaderType::Static);
        let unlit = |color| {
            Material::standard()
                .color(color)
                .model(MaterialModel::Unlit)
                .build()
        };
        engine.add_cube(
            Transform {
                position: [-1.5, 5.0, 15.0],
                scale: [2.0; 3],
                ..Transform::default()
            },
            &unlit([1.0, 0.0, 0.0]),
            &still,
        );
        engine.add_sphere(
            Transform::new([1.5, 5.0, 15.0]),
            &unlit([0.0, 1.0, 0.0]),
            &still,
            3,
        );
        let (_, mut app) = engine.into_app();
        app.update(std::time::Duration::from_millis(16)).unwrap();

        let extent = [64, 64];
        let base = crate::rendering::test_support::headless_device();
        let memory_allocator =
            Arc::new(StandardMemoryAllocator::new_default(base.device.clone()));
        let mut renderer = SceneRenderer::new(
            base.queue.clone(),
            memory_allocator.clone(),
            OFFSCREEN_COLOR_FORMAT,
            extent,
        )
        .unwrap();
        let image = Image::new(
            memory_allocator.clone(),
            ImageCreateInfo {
                format: OFFSCREEN_COLOR_FORMAT,
                extent: [extent[0], extent[1], 1],
                usage: ImageUsage::COLOR_ATTACHMENT | ImageUsage::TRANSFER_SRC,
                ..Default::default()
            },
            AllocationCreateInfo::default(),
        )
        .unwrap();
        renderer
            .render(
                vulkano::sync::now(base.device.clone()).boxed(),
                ImageView::new_default(image.clone()).unwrap(),
                extent,
                SceneRenderOptions::game(extent),
                app.world().resource::<RenderWorld>(),
                app.world().resource::<AssetServer>(),
            )
            .unwrap()
            .then_signal_fence_and_flush()
            .unwrap()
            .wait(None)
            .unwrap();
        let pixels = crate::rendering::readback::read_back_image(
            &base.device,
            &base.queue,
            &memory_allocator,
            &Arc::new(StandardCommandBufferAllocator::new(
                base.device.clone(),
                Default::default(),
            )),
            &image,
        );
        // Pixels are [b, g, r, a]: the cube is red, the sphere green.
        let count = |matches: fn(&[u8]) -> bool| {
            pixels
                .as_chunks::<4>()
                .0
                .iter()
                .filter(|pixel| matches(*pixel))
                .count()
        };
        let red = count(|p| p[2] > 200 && p[1] < 40 && p[0] < 40);
        let green = count(|p| p[1] > 200 && p[2] < 40 && p[0] < 40);
        assert!(red > 20, "cube covers {red} pixels");
        assert!(green > 20, "sphere covers {green} pixels");
    }
}
