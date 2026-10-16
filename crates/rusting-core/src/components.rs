use bevy_ecs::component::Component;
use bevy_ecs::entity::Entity;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Persistent scene identity. Unlike a Bevy [`Entity`], this survives saving,
/// loading, and a new process.
#[derive(Component, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SceneId(pub Uuid);

impl SceneId {
    #[must_use]
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for SceneId {
    fn default() -> Self {
        Self::new()
    }
}

/// World-space transform derived from [`crate::transform::Transform`] and hierarchy.
#[derive(Component, Clone, Copy, Debug, PartialEq)]
pub struct GlobalTransform {
    pub matrix: [[f32; 4]; 4],
}

impl Default for GlobalTransform {
    fn default() -> Self {
        Self {
            matrix: nalgebra::Matrix4::<f32>::identity().into(),
        }
    }
}

#[derive(Component, Clone, Copy, Debug, PartialEq, Eq)]
pub struct Parent(pub Entity);

#[derive(Component, Clone, Debug, Default, PartialEq, Eq)]
pub struct Children(pub Vec<Entity>);

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Projection {
    Perspective {
        vertical_fov_radians: f32,
        near: f32,
        far: f32,
    },
    Orthographic {
        vertical_size: f32,
        near: f32,
        far: f32,
    },
}

impl Default for Projection {
    fn default() -> Self {
        Self::Perspective {
            vertical_fov_radians: std::f32::consts::FRAC_PI_3,
            near: 0.1,
            far: 1_000.0,
        }
    }
}

#[derive(Component, Clone, Copy, Debug, Default, PartialEq)]
pub struct Camera {
    pub projection: Projection,
    pub active: bool,
    pub priority: i32,
    /// Part of the window this camera draws into, as fractions
    /// `[x, y, width, height]` from the top-left corner. `None` fills the
    /// window.
    pub viewport: Option<[f32; 4]>,
}

#[derive(Component, Clone, Debug, PartialEq, Eq)]
pub struct Name(pub String);

impl Name {
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::ops::Deref for Name {
    type Target = str;

    fn deref(&self) -> &str {
        &self.0
    }
}

/// Reusable classes assigned to one scene object.
///
/// Names identify one object. Classes select any number of objects, and one
/// object can belong to several classes at the same time.
#[derive(
    Component, Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize,
)]
pub struct ObjectClasses {
    /// Class names such as `gravity`, `enemy`, or `falling_cubes`.
    pub names: Vec<String>,
}

impl ObjectClasses {
    /// Creates a clean class list without empty or repeated names.
    #[must_use]
    pub fn new(classes: impl IntoIterator<Item = impl Into<String>>) -> Self {
        let mut result = Self::default();
        for class in classes {
            result.add(class);
        }
        result
    }

    /// Adds a class if this object does not already have it.
    pub fn add(&mut self, class: impl Into<String>) -> bool {
        let class = class.into();
        let class = class.trim();
        if class.is_empty() || self.contains(class) {
            return false;
        }
        self.names.push(class.to_owned());
        true
    }

    /// Removes a class from this object.
    pub fn remove(&mut self, class: &str) -> bool {
        let old_len = self.names.len();
        self.names.retain(|current| current != class);
        self.names.len() != old_len
    }

    /// Returns true when this object belongs to the requested class.
    #[must_use]
    pub fn contains(&self, class: &str) -> bool {
        self.names.iter().any(|current| current == class)
    }
}

#[derive(Component, Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct DirectionalLight {
    pub color: [f32; 3],
    pub illuminance: f32,
    pub shadows: bool,
}

impl DirectionalLight {
    /// Illuminance that lights a white surface facing the light at 1 before
    /// exposure, like a point light of 1000 up close or ambient intensity 1.
    pub const LUX_PER_UNIT: f32 = 100_000.0;
}

impl Default for DirectionalLight {
    fn default() -> Self {
        Self {
            color: [1.0; 3],
            illuminance: 100_000.0,
            shadows: true,
        }
    }
}

#[derive(Component, Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct PointLight {
    pub color: [f32; 3],
    pub intensity: f32,
    pub range: f32,
    /// Casts shadows through six cube faces of the shadow map. Only when
    /// no directional or spot light in the frame casts shadows.
    #[serde(default)]
    pub shadows: bool,
    /// Radius in metres of the glowing sphere. A larger source gives
    /// wider, softer highlights on shiny surfaces; 0 is a point.
    #[serde(default)]
    pub radius: f32,
}

#[derive(Component, Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct SpotLight {
    pub color: [f32; 3],
    pub intensity: f32,
    pub range: f32,
    /// Fully illuminated cone angle in radians.
    pub inner_angle: f32,
    /// Outer cone angle in radians where illumination reaches zero.
    pub outer_angle: f32,
    /// Casts shadows through one shadow map. A frame has one shadowed
    /// light: a directional light with shadows wins over spot lights.
    #[serde(default)]
    pub shadows: bool,
    /// Radius in metres of the glowing sphere. A larger source gives
    /// wider, softer highlights on shiny surfaces; 0 is a point.
    #[serde(default)]
    pub radius: f32,
}

impl Default for SpotLight {
    fn default() -> Self {
        Self {
            color: [1.0; 3],
            intensity: 1_000.0,
            range: 10.0,
            inner_angle: 20.0_f32.to_radians(),
            outer_angle: 35.0_f32.to_radians(),
            shadows: false,
            radius: 0.0,
        }
    }
}

impl Default for PointLight {
    fn default() -> Self {
        Self {
            color: [1.0; 3],
            intensity: 1_000.0,
            range: 10.0,
            shadows: false,
            radius: 0.0,
        }
    }
}

#[derive(Component, Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct AmbientLight {
    pub color: [f32; 3],
    pub intensity: f32,
}

impl Default for AmbientLight {
    fn default() -> Self {
        Self {
            color: [1.0; 3],
            intensity: 0.1,
        }
    }
}

/// Hemisphere environment light: surfaces facing +Y see `sky_color`,
/// surfaces facing -Y see `ground_color`, and the two blend in between.
/// Adds to any [`AmbientLight`].
#[derive(Component, Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct SkyLight {
    pub sky_color: [f32; 3],
    pub ground_color: [f32; 3],
    pub intensity: f32,
}

impl Default for SkyLight {
    fn default() -> Self {
        Self {
            sky_color: [0.6, 0.75, 1.0],
            ground_color: [0.3, 0.25, 0.2],
            intensity: 0.3,
        }
    }
}

/// Curve that maps HDR scene color into the displayable 0..1 range.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize,
)]
pub enum ToneMapper {
    /// Clips at 1, like Godot's default. Leaves values below 1 unchanged.
    #[default]
    Linear,
    /// `c / (1 + c)`: never clips, compresses highlights softly.
    Reinhard,
    /// Filmic ACES curve (Narkowicz fit): more contrast, saturated mids.
    Aces,
}

/// Exposure and tone mapping applied to the lit scene before display, like
/// the tonemap settings of Godot's `Environment`. The first one found in the
/// world is used; without one the scene uses `Linear` at exposure 1.
#[derive(Component, Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct ToneMapping {
    pub mapper: ToneMapper,
    /// Scene color is multiplied by this before the curve.
    pub exposure: f32,
}

impl Default for ToneMapping {
    fn default() -> Self {
        Self {
            mapper: ToneMapper::Linear,
            exposure: 1.0,
        }
    }
}

/// Render-only bounds used for visibility, separate from the physics
/// `Collider`. Stored in the entity's local space; render extraction
/// transforms them into world space every time the entity is extracted.
#[derive(Component, Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub enum RenderBounds {
    Sphere { center: [f32; 3], radius: f32 },
    Aabb { min: [f32; 3], max: [f32; 3] },
}

/// The built-in unit cube's box.
impl Default for RenderBounds {
    fn default() -> Self {
        RenderBounds::Aabb {
            min: [-0.5; 3],
            max: [0.5; 3],
        }
    }
}

impl RenderBounds {
    /// Returns the bounds of this volume after `matrix` (a column-major
    /// `GlobalTransform` matrix). A sphere stays a sphere whose radius grows
    /// by the largest axis scale; a box becomes the axis-aligned box around
    /// its transformed corners, so both stay conservative under rotation and
    /// non-uniform scale.
    pub fn transformed(&self, matrix: &[[f32; 4]; 4]) -> RenderBounds {
        let matrix = nalgebra::Matrix4::from(*matrix);
        let linear = matrix.fixed_view::<3, 3>(0, 0);
        let translation = matrix.fixed_view::<3, 1>(0, 3);
        match *self {
            RenderBounds::Sphere { center, radius } => {
                let scale = (0..3)
                    .map(|axis| linear.column(axis).norm())
                    .fold(0.0_f32, f32::max);
                let center =
                    linear * nalgebra::Vector3::from(center) + translation;
                RenderBounds::Sphere {
                    center: center.into(),
                    radius: radius * scale,
                }
            }
            RenderBounds::Aabb { min, max } => {
                // Arvo's method: each output axis takes the smaller and the
                // larger product per input axis.
                let mut world_min: [f32; 3] = translation.into();
                let mut world_max = world_min;
                for row in 0..3 {
                    for column in 0..3 {
                        let a = linear[(row, column)] * min[column];
                        let b = linear[(row, column)] * max[column];
                        world_min[row] += a.min(b);
                        world_max[row] += a.max(b);
                    }
                }
                RenderBounds::Aabb {
                    min: world_min,
                    max: world_max,
                }
            }
        }
    }
}

#[derive(Component, Clone, Copy, Debug, PartialEq, Eq)]
pub struct Visibility {
    pub visible: bool,
}

impl Default for Visibility {
    fn default() -> Self {
        Self { visible: true }
    }
}

#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize,
)]
pub enum QualityProfile {
    Auto,
    Eco,
    #[default]
    Balanced,
    High,
}

/// How the renderer skips objects outside the view.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize,
)]
pub enum CullingMode {
    /// The renderer picks: small scenes draw everything, the rest cull by
    /// frustum on the CPU or GPU.
    #[default]
    Auto,
    /// Draw every visible object.
    Disabled,
    /// Skip objects whose world bounds lie outside the camera frustum.
    Frustum,
    /// Frustum culling plus GPU occlusion culling against a depth pyramid
    /// of the objects visible last frame. Needs the GPU culling path, so it
    /// always runs there, whatever the scene size.
    FrustumAndOcclusion,
}

/// Anti-aliasing. `Auto` follows the quality profile: off on `Eco`, the
/// most multisamples the device offers (up to 4) otherwise. A count the
/// device lacks falls back to the next lower one. `Fxaa` renders one sample
/// and smooths edges found in the final image instead: cheaper than MSAA,
/// also smooths edges inside textures and alpha-cut leaves, a little softer.
/// `Taa` renders one sample from a slightly different spot each frame and
/// blends the frames: the smoothest edges, with some blur on fast motion.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize,
)]
pub enum Antialiasing {
    #[default]
    Auto,
    Off,
    Msaa2,
    Msaa4,
    Fxaa,
    Taa,
}

/// Shadow map resolution, reach and sun cascades. `Auto` follows the
/// quality profile.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize,
)]
pub enum ShadowQuality {
    #[default]
    Auto,
    /// 1024 px map, 30 m reach, 2 cascades.
    Low,
    /// 2048 px map, 50 m reach, 3 cascades.
    Medium,
    /// 4096 px map, 80 m reach, 4 cascades.
    High,
}

#[derive(bevy_ecs::prelude::Resource, Clone, Debug, PartialEq)]
pub struct RenderSettings {
    pub quality: QualityProfile,
    pub vsync: bool,
    pub limit_fps: bool,
    pub max_fps: u32,
    /// Fraction of the target size the 3D scene renders at, 0.25 to 2.0.
    /// The frame is stretched over the target; UI stays at full resolution.
    pub render_scale: f32,
    /// Stretch a scaled frame with nearest-neighbour filtering, so a low
    /// `render_scale` shows square pixels instead of blur.
    pub pixelated: bool,
    /// Above 0, a `render_scale` below 1 is stretched with an
    /// edge-preserving filter and contrast-adaptive sharpening of this
    /// strength (up to 1), like FSR 1, instead of a plain linear stretch.
    pub upscale_sharpness: f32,
    /// RGBA color used to clear the game render target before drawing.
    pub background_color: [f32; 4],
    pub culling: CullingMode,
    pub antialiasing: Antialiasing,
    pub shadows: ShadowQuality,
    /// One shadow-map tap per pixel instead of 3x3 filtering: crisp,
    /// stair-stepped shadow edges for a low-poly look.
    pub hard_shadows: bool,
    /// Screen-space reflections and refraction of the opaque scene. Off
    /// skips the scene copy, mip chain and depth pyramid.
    pub reflections: bool,
}

impl Default for RenderSettings {
    fn default() -> Self {
        Self {
            quality: QualityProfile::Auto,
            vsync: false,
            limit_fps: false,
            max_fps: 120,
            render_scale: 1.0,
            pixelated: false,
            upscale_sharpness: 0.0,
            background_color: [0.025, 0.04, 0.07, 1.0],
            culling: CullingMode::Auto,
            antialiasing: Antialiasing::Auto,
            shadows: ShadowQuality::Auto,
            hard_shadows: false,
            reflections: true,
        }
    }
}

/// Freezes a dynamic body's motion along world axes, like Godot's axis
/// locks. A 2D body in the XY plane locks linear z and angular x and y.
#[derive(
    Component,
    Clone,
    Copy,
    Debug,
    Default,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
)]
#[serde(default)]
pub struct AxisLock {
    /// No movement along world x, y, z.
    pub linear: [bool; 3],
    /// No turning around world x, y, z.
    pub angular: [bool; 3],
}

impl AxisLock {
    /// The XY-plane lock of a 2D body.
    pub const PLANE_XY: Self = Self {
        linear: [false, false, true],
        angular: [true, true, false],
    };

    /// Bits 0 to 2 lock linear x, y, z and bits 3 to 5 angular x, y, z.
    #[must_use]
    pub fn mask(&self) -> u32 {
        self.linear
            .iter()
            .chain(&self.angular)
            .enumerate()
            .map(|(bit, &locked)| u32::from(locked) << bit)
            .sum()
    }
}

#[derive(bevy_ecs::prelude::Resource, Clone, Debug, PartialEq)]
pub struct PhysicsSettings {
    pub gravity: [f32; 3],
    pub enabled: bool,
    /// Loose dynamic CPU bodies below this height are despawned and send
    /// `FellOut`; `None` keeps them falling.
    pub kill_y: Option<f32>,
    /// CPU solver substeps per fixed step. More substeps hold long joint
    /// chains and heavy-on-light stacks tighter, at that many times the
    /// solver cost.
    pub substeps: u32,
    /// Axis locks every GPU body shares; [`AxisLock::PLANE_XY`] keeps a 2D
    /// game's GPU bodies in its plane. CPU bodies take their own
    /// `rusting.axis_lock` instead.
    pub gpu_locks: AxisLock,
}

/// Reports which physics backends are connected to the ECS scene runner.
/// The compatibility engine facade has its own legacy GPU path and does not
/// use this resource.
#[derive(
    bevy_ecs::prelude::Resource, Clone, Copy, Debug, Default, PartialEq, Eq,
)]
pub struct PhysicsBackendStatus {
    pub gameplay_available: bool,
    pub gpu_dynamic_available: bool,
}

impl Default for PhysicsSettings {
    fn default() -> Self {
        Self {
            gravity: [0.0, -9.81, 0.0],
            enabled: true,
            kill_y: None,
            substeps: 1,
            gpu_locks: AxisLock::default(),
        }
    }
}

/// Says where the newest runtime physics state of an entity lives.
#[derive(
    Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq, Serialize,
)]
pub enum SimulationClass {
    /// No simulation; the body is ignored by every solver.
    None,
    /// Never moves; a collider for both the CPU and the GPU solvers.
    Static,
    /// Simulated on the CPU, so gameplay reads current state directly.
    #[default]
    #[serde(alias = "Gameplay")]
    Cpu,
    /// Simulated on the GPU; the CPU sees events and selected readbacks.
    #[serde(alias = "GpuDynamic")]
    Gpu,
}

#[allow(non_upper_case_globals)]
impl SimulationClass {
    #[deprecated(note = "renamed to `SimulationClass::Cpu`")]
    pub const Gameplay: Self = Self::Cpu;
    #[deprecated(note = "renamed to `SimulationClass::Gpu`")]
    pub const GpuDynamic: Self = Self::Gpu;
}

/// Built-in GPU compute profile, or a project-provided compute shader.
#[derive(
    Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq, Serialize,
)]
pub enum PhysicsSolver {
    #[default]
    Full,
    Simplified,
    NoCollision,
    Space,
    Custom,
}

/// Semantic physics configuration authored by the editor.
///
/// Static and disabled bodies are deliberately excluded from dynamic compute
/// dispatches. Custom shader paths are project-relative and validated by the
/// editor before they are saved.
#[derive(Component, Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct PhysicsBody {
    pub simulation: SimulationClass,
    pub solver: PhysicsSolver,
    pub custom_shader: Option<String>,
}

impl Default for PhysicsBody {
    fn default() -> Self {
        Self {
            simulation: SimulationClass::Cpu,
            solver: PhysicsSolver::Full,
            custom_shader: None,
        }
    }
}

impl PhysicsBody {
    #[must_use]
    pub fn participates_in_dynamic_simulation(&self) -> bool {
        matches!(self.simulation, SimulationClass::Cpu | SimulationClass::Gpu)
    }

    #[must_use]
    pub fn uses_gpu(&self) -> bool {
        self.simulation == SimulationClass::Gpu
    }
}

#[derive(
    Component,
    Clone,
    Copy,
    Debug,
    Default,
    Deserialize,
    PartialEq,
    Eq,
    Serialize,
)]
pub enum RigidBodyKind {
    Fixed,
    #[default]
    Dynamic,
    Kinematic,
}

#[derive(Component, Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
pub struct RigidBody {
    pub kind: RigidBodyKind,
    pub mass: f32,
    pub linear_velocity: [f32; 3],
    pub angular_velocity: [f32; 3],
    pub gravity_scale: f32,
}

impl Default for RigidBody {
    fn default() -> Self {
        Self {
            kind: RigidBodyKind::Dynamic,
            mass: 1.0,
            linear_velocity: [0.0; 3],
            angular_velocity: [0.0; 3],
            gravity_scale: 1.0,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
pub enum ColliderShape {
    Box {
        half_extents: [f32; 3],
    },
    Sphere {
        radius: f32,
    },
    Capsule {
        half_height: f32,
        radius: f32,
    },
    /// The convex hull of the entity's `MeshRenderer` mesh, scaled by its
    /// transform. Dynamic bodies may use it.
    ConvexMesh,
    /// The triangles of the entity's `MeshRenderer` mesh, scaled by its
    /// transform. Static only: a body with it never moves.
    TriangleMesh,
    /// The entity's `rusting.heightfield` height grid, scaled by its
    /// transform. Static only, like `TriangleMesh`.
    Heightfield,
    /// The convex hull of the entity's `rusting.polygon` outline, extruded
    /// along Z. Dynamic bodies may use it; the 2D convex polygon.
    Polygon,
    /// The segments of the entity's `rusting.polygon` outline as thin
    /// walls, extruded along Z. Static only; the 2D segment chain.
    Chain,
}

#[derive(Component, Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
pub struct Collider {
    pub shape: ColliderShape,
    pub friction: f32,
    pub restitution: f32,
    pub sensor: bool,
}

impl Default for Collider {
    fn default() -> Self {
        Self {
            shape: ColliderShape::Box {
                half_extents: [0.5; 3],
            },
            friction: 0.5,
            restitution: 0.0,
            sensor: false,
        }
    }
}

#[derive(
    Component, Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize,
)]
pub struct CollisionLayers {
    pub memberships: u32,
    pub filters: u32,
}

impl Default for CollisionLayers {
    fn default() -> Self {
        Self {
            memberships: u32::MAX,
            filters: u32::MAX,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy_ecs::{component::Component, resource::Resource};

    fn assert_component<T: Component>() {}
    fn assert_resource<T: Resource>() {}

    #[test]
    fn canonical_types_implement_bevy_ecs_traits() {
        assert_component::<SceneId>();
        assert_component::<GlobalTransform>();
        assert_component::<Parent>();
        assert_component::<Children>();
        assert_component::<Camera>();
        assert_component::<Name>();
        assert_component::<ObjectClasses>();
        assert_component::<DirectionalLight>();
        assert_component::<PointLight>();
        assert_component::<SpotLight>();
        assert_component::<AmbientLight>();
        assert_component::<SkyLight>();
        assert_component::<ToneMapping>();
        assert_component::<RenderBounds>();
        assert_component::<Visibility>();
        assert_component::<PhysicsBody>();
        assert_component::<RigidBodyKind>();
        assert_component::<RigidBody>();
        assert_component::<Collider>();
        assert_component::<CollisionLayers>();

        assert_resource::<RenderSettings>();
        assert_resource::<PhysicsSettings>();
        assert_resource::<PhysicsBackendStatus>();
    }

    #[test]
    fn defaults_are_ready_for_a_basic_scene() {
        let identity: [[f32; 4]; 4] =
            nalgebra::Matrix4::<f32>::identity().into();
        assert_eq!(GlobalTransform::default().matrix, identity);
        assert!(Visibility::default().visible);
        assert_eq!(RigidBody::default().mass, 1.0);
        assert_eq!(PhysicsSettings::default().gravity, [0.0, -9.81, 0.0]);
        assert_eq!(CollisionLayers::default().memberships, u32::MAX);
        assert_eq!(RenderSettings::default().quality, QualityProfile::Auto);
    }

    #[test]
    fn render_bounds_stay_conservative_under_transforms() {
        use nalgebra::{Matrix4, Rotation3, Vector3};
        let matrix: [[f32; 4]; 4] =
            (Matrix4::new_translation(&Vector3::new(10.0, 0.0, 0.0))
                * Rotation3::from_axis_angle(
                    &Vector3::z_axis(),
                    std::f32::consts::FRAC_PI_4,
                )
                .to_homogeneous()
                * Matrix4::new_nonuniform_scaling(&Vector3::new(
                    2.0, 1.0, 3.0,
                )))
            .into();
        let RenderBounds::Sphere { center, radius } = (RenderBounds::Sphere {
            center: [1.0, 0.0, 0.0],
            radius: 1.0,
        })
        .transformed(&matrix) else {
            panic!("a sphere stays a sphere");
        };
        let half = std::f32::consts::FRAC_1_SQRT_2;
        let expected = [10.0 + 2.0 * half, 2.0 * half, 0.0];
        for (value, expected) in center.iter().zip(expected) {
            assert!((value - expected).abs() < 1e-5, "{center:?}");
        }
        assert!((radius - 3.0).abs() < 1e-5, "largest axis scale");

        let RenderBounds::Aabb { min, max } = (RenderBounds::Aabb {
            min: [-1.0; 3],
            max: [1.0; 3],
        })
        .transformed(&matrix) else {
            panic!("a box stays a box");
        };
        // The 2x1 face rotated 45 degrees spans (2 + 1) / sqrt(2) on x and
        // y; z only scales.
        let reach = 3.0 * half;
        let expected_min = [10.0 - reach, -reach, -3.0];
        let expected_max = [10.0 + reach, reach, 3.0];
        for axis in 0..3 {
            assert!((min[axis] - expected_min[axis]).abs() < 1e-5, "{min:?}");
            assert!((max[axis] - expected_max[axis]).abs() < 1e-5, "{max:?}");
        }
    }

    #[test]
    fn object_classes_clean_and_edit_names() {
        let mut classes =
            ObjectClasses::new([" gravity ", "", "gravity", "enemy"]);
        assert_eq!(classes.names, ["gravity", "enemy"]);
        assert!(classes.contains("gravity"));
        assert!(!classes.add("enemy"));
        assert!(classes.remove("enemy"));
        assert!(!classes.contains("enemy"));
    }

    #[test]
    fn physics_body_helpers_follow_simulation_class() {
        let mut body = PhysicsBody::default();
        assert!(body.participates_in_dynamic_simulation());
        assert!(!body.uses_gpu());

        body.simulation = SimulationClass::Gpu;
        assert!(body.participates_in_dynamic_simulation());
        assert!(body.uses_gpu());

        body.simulation = SimulationClass::Static;
        assert!(!body.participates_in_dynamic_simulation());
        assert!(!body.uses_gpu());
    }
}
