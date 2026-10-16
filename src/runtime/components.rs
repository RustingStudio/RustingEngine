pub use rusting_core::components::{
    AmbientLight, Antialiasing, AxisLock, Camera, Children, Collider,
    ColliderShape, CollisionLayers, CullingMode, DirectionalLight,
    GlobalTransform, Name, ObjectClasses, Parent, PhysicsBackendStatus,
    PhysicsBody, PhysicsSettings, PhysicsSolver, PointLight, Projection,
    QualityProfile, RenderBounds, RenderSettings, RigidBody, RigidBodyKind,
    SceneId, ShadowQuality, SimulationClass, SkyLight, SpotLight, ToneMapper,
    ToneMapping, Visibility,
};

use bevy_ecs::component::Component;

use crate::assets::{Handle, MaterialAsset, MeshAsset};

/// Semantic rendering component. GPU batches are derived during extraction.
#[derive(Component, Clone, Copy, Debug, PartialEq, Eq)]
pub struct MeshRenderer {
    pub mesh: Handle<MeshAsset>,
    pub material: Handle<MaterialAsset>,
    pub cast_shadows: bool,
    pub receive_shadows: bool,
}

/// Which layer draws a [`MeshRenderer`]. While the active camera has a
/// [`ViewmodelCamera`], `Viewmodel` meshes draw over the world with that
/// camera's own field of view, so a held tool never pokes into walls.
#[derive(
    Component,
    Clone,
    Copy,
    Debug,
    Default,
    PartialEq,
    Eq,
    serde::Serialize,
    serde::Deserialize,
)]
pub enum RenderLayer {
    #[default]
    World,
    Viewmodel,
}

/// On a camera: draws [`RenderLayer::Viewmodel`] meshes over the world with
/// this vertical field of view. Perspective cameras only.
#[derive(
    Component,
    Clone,
    Copy,
    Debug,
    PartialEq,
    serde::Serialize,
    serde::Deserialize,
)]
#[serde(default)]
pub struct ViewmodelCamera {
    pub vertical_fov_radians: f32,
}

impl Default for ViewmodelCamera {
    fn default() -> Self {
        Self {
            vertical_fov_radians: std::f32::consts::FRAC_PI_3,
        }
    }
}

/// Per-object values that replace its material's: a red and a blue crate
/// share one material and still draw in one instanced batch. Unset fields
/// keep the material's value.
#[derive(
    Component,
    Clone,
    Copy,
    Debug,
    Default,
    PartialEq,
    serde::Serialize,
    serde::Deserialize,
)]
#[serde(default)]
pub struct MaterialOverride {
    /// Linear RGBA, multiplied into the base color texture.
    pub base_color: Option<[f32; 4]>,
    /// Linear RGB glow.
    pub emissive: Option<[f32; 3]>,
    pub metallic: Option<f32>,
    pub roughness: Option<f32>,
    pub uv_scale: Option<[f32; 2]>,
    pub uv_offset: Option<[f32; 2]>,
}
