//! Versioned, editor-authored scene files and compiled runtime scene data.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::error::Error;
use std::fmt::{Display, Formatter};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use bevy_ecs::component::Component;
use bevy_ecs::entity::Entity;
use bevy_ecs::prelude::{Resource, World};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::assets::{
    AlphaMode, AssetServer, Handle, MaterialAsset, MaterialModel,
    PrimitiveShape, TextureAsset,
};
use crate::reflect::{
    self, FieldMigration, Reflect, ReflectError, ReflectProblem, TypeInfo,
};
use crate::Transform;

use super::{
    AmbientLight, Camera, Collider, CollisionLayers, DirectionalLight,
    GpuPhysicsWatch, MeshRenderer, Name, ObjectClasses, Parent, PhysicsBody,
    PointLight, Projection, RenderBounds, RenderSettings, RigidBody, SceneId,
    SkyLight, SpotLight, ToneMapping, Visibility,
};
use crate::runtime::DeterminismMode;
use crate::runtime::{CullingMode, QualityProfile};

pub const SCENE_FORMAT_VERSION: u32 = 10;
const COMPILED_MAGIC: &[u8; 8] = b"RSCENE01";

#[derive(Debug)]
pub enum SceneIoError {
    Io(std::io::Error),
    Source(serde_json::Error),
    Compiled(Box<bincode::ErrorKind>),
    UnsupportedVersion(u32),
    DuplicateComponent(String),
    UnknownComponent(String),
    Component {
        name: String,
        message: String,
    },
    MissingAssetServer,
    MissingAssetPath(PathBuf),
    AssetLoad {
        path: PathBuf,
        message: String,
    },
    UnsavedMesh(u64),
    UnsavedTexture(u64),
    DuplicateEntity(Uuid),
    DuplicateName(String),
    MissingParent(Uuid),
    HierarchyCycle(Uuid),
    /// The file changed on disk after the editor loaded or saved it.
    Conflict(PathBuf),
    Runtime(super::AppError),
    /// A component value that does not fit its reflected type.
    Reflection(Box<ReflectError>),
    /// The source scene of a [`super::SceneInstance`] could not be placed.
    Instance {
        path: PathBuf,
        error: Box<SceneIoError>,
    },
    /// A scene instance contains, directly or through other instances, the
    /// scene it is placed in.
    InstanceCycle(PathBuf),
    /// An instance operation named an object that no scene instance placed.
    NotAnInstance(Uuid),
    /// A prefab handle whose scene is not loaded in the `AssetServer`.
    MissingPrefab(u64),
    /// A signal connection of `object` names an unregistered handler or an
    /// object that does not exist.
    Connection {
        object: Uuid,
        problem: super::SignalError,
    },
}

impl Display for SceneIoError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => Display::fmt(error, formatter),
            Self::Source(error) => Display::fmt(error, formatter),
            Self::Compiled(error) => Display::fmt(error, formatter),
            Self::UnsupportedVersion(version) => {
                write!(formatter, "unsupported scene format version {version}")
            }
            Self::DuplicateComponent(name) => {
                write!(
                    formatter,
                    "scene component `{name}` is already registered"
                )
            }
            Self::UnknownComponent(name) => {
                write!(formatter, "scene uses unregistered component `{name}`")
            }
            Self::Component { name, message } => {
                write!(
                    formatter,
                    "failed to process component `{name}`: {message}"
                )
            }
            Self::MissingAssetServer => formatter
                .write_str("AssetPlugin must be installed before scene I/O"),
            Self::MissingAssetPath(path) => {
                write!(
                    formatter,
                    "scene asset `{}` is not loaded",
                    path.display()
                )
            }
            Self::AssetLoad { path, message } => {
                write!(
                    formatter,
                    "could not load scene asset `{}`: {message}",
                    path.display()
                )
            }
            Self::UnsavedMesh(key) => {
                write!(
                    formatter,
                    "mesh {key} has no asset path and cannot be saved"
                )
            }
            Self::UnsavedTexture(key) => {
                write!(
                    formatter,
                    "texture {key} has no asset path and cannot be saved"
                )
            }
            Self::DuplicateEntity(id) => {
                write!(formatter, "scene contains duplicate object ID {id}")
            }
            Self::DuplicateName(name) => {
                write!(
                    formatter,
                    "scene contains duplicate object name `{name}`"
                )
            }
            Self::MissingParent(id) => {
                write!(formatter, "scene parent {id} does not exist")
            }
            Self::HierarchyCycle(id) => {
                write!(formatter, "scene object {id} has a parent cycle")
            }
            Self::Conflict(path) => write!(
                formatter,
                "{} changed on disk since it was loaded; Load takes that \
                 change, Save again overwrites it",
                path.display()
            ),
            Self::Runtime(error) => Display::fmt(error, formatter),
            Self::Reflection(error) => Display::fmt(error, formatter),
            Self::Instance { path, error } => write!(
                formatter,
                "could not instance scene `{}`: {error}",
                path.display()
            ),
            Self::InstanceCycle(path) => write!(
                formatter,
                "scene `{}` contains an instance of itself",
                path.display()
            ),
            Self::MissingPrefab(key) => {
                write!(formatter, "prefab {key} is not loaded")
            }
            Self::NotAnInstance(id) => {
                write!(formatter, "object {id} is not part of a scene instance")
            }
            Self::Connection { object, problem } => {
                write!(
                    formatter,
                    "signal connection on object {object}: {problem}"
                )
            }
        }
    }
}

impl Error for SceneIoError {}

impl From<std::io::Error> for SceneIoError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<serde_json::Error> for SceneIoError {
    fn from(error: serde_json::Error) -> Self {
        Self::Source(error)
    }
}

impl From<Box<bincode::ErrorKind>> for SceneIoError {
    fn from(error: Box<bincode::ErrorKind>) -> Self {
        Self::Compiled(error)
    }
}

impl From<super::AppError> for SceneIoError {
    fn from(error: super::AppError) -> Self {
        Self::Runtime(error)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct SceneDocument {
    #[serde(default)]
    pub format_version: u32,
    pub name: String,
    pub entities: Vec<SceneEntity>,
    /// Cooked version 5 scenes end before it.
    #[serde(default)]
    pub render: SceneRenderSettings,
    /// Stays the last field: cooked version 6 scenes end before it.
    #[serde(default)]
    pub simulation: SceneSimulationSettings,
}

/// Simulation settings that ship with the game. `cook_scene` copies them
/// from the project's `project.json`; a replacing load inserts them as
/// resources.
#[derive(
    Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq,
)]
pub struct SceneSimulationSettings {
    pub determinism: DeterminismMode,
}

/// Cooked shape of version 6, before simulation settings.
#[derive(Serialize, Deserialize)]
struct LegacySceneDocumentV6 {
    format_version: u32,
    name: String,
    entities: Vec<SceneEntityV7>,
    render: SceneRenderSettings,
}

impl From<LegacySceneDocumentV6> for SceneDocument {
    fn from(document: LegacySceneDocumentV6) -> Self {
        Self {
            format_version: document.format_version,
            name: document.name,
            entities: document.entities.into_iter().map(Into::into).collect(),
            render: document.render,
            simulation: SceneSimulationSettings::default(),
        }
    }
}

/// The scene's [`RenderSettings`] that ship with the game. A replacing load
/// applies them; an additive load leaves the current ones.
#[derive(
    Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq,
)]
pub struct SceneRenderSettings {
    pub quality: QualityProfile,
    pub culling: CullingMode,
}

/// Cooked shape of version 5, before render settings.
#[derive(Serialize, Deserialize)]
struct LegacySceneDocumentV5 {
    format_version: u32,
    name: String,
    entities: Vec<SceneEntityV7>,
}

impl From<LegacySceneDocumentV5> for SceneDocument {
    fn from(document: LegacySceneDocumentV5) -> Self {
        Self {
            format_version: document.format_version,
            name: document.name,
            entities: document.entities.into_iter().map(Into::into).collect(),
            render: SceneRenderSettings::default(),
            simulation: SceneSimulationSettings::default(),
        }
    }
}

/// Cooked shape of version 8, before material names, and with
/// [`SceneMaterialDataV9`] the shape of version 9, before flat shading.
#[derive(Serialize, Deserialize)]
struct LegacySceneDocumentV8<M = SceneMaterialDataV8> {
    format_version: u32,
    name: String,
    entities: Vec<SceneEntityV8<M>>,
    render: SceneRenderSettings,
    simulation: SceneSimulationSettings,
}

#[derive(Serialize, Deserialize)]
struct SceneEntityV8<M = SceneMaterialDataV8> {
    id: Uuid,
    parent: Option<Uuid>,
    name: Option<String>,
    classes: Vec<String>,
    transform: Option<SceneTransform>,
    mesh_renderer: Option<SceneMeshRendererV8<M>>,
    camera: Option<SceneCamera>,
    visible: Option<bool>,
    physics_body: Option<PhysicsBody>,
    rigid_body: Option<RigidBody>,
    collider: Option<Collider>,
    collision_layers: Option<CollisionLayers>,
    gpu_physics_watch: Option<GpuPhysicsWatch>,
    components: BTreeMap<String, String>,
    directional_light: Option<DirectionalLight>,
    point_light: Option<PointLight>,
    spot_light: Option<SpotLight>,
}

#[derive(Serialize, Deserialize)]
struct SceneMeshRendererV8<M> {
    mesh: SceneMesh,
    material: SceneMaterialV8<M>,
    cast_shadows: bool,
    receive_shadows: bool,
}

#[derive(Serialize, Deserialize)]
enum SceneMaterialV8<M> {
    BuiltinError,
    Inline(M),
}

#[derive(Clone, Serialize, Deserialize)]
struct SceneMaterialDataV8 {
    model: SceneMaterialModel,
    alpha_mode: SceneAlphaMode,
    base_color: [f32; 4],
    emissive: [f32; 3],
    metallic: f32,
    roughness: f32,
    transmission: f32,
    ior: f32,
    thickness: f32,
    uv_scale: [f32; 2],
    uv_offset: [f32; 2],
    base_color_texture: Option<PathBuf>,
    normal_texture: Option<PathBuf>,
    metallic_roughness_texture: Option<PathBuf>,
    occlusion_texture: Option<PathBuf>,
    emissive_texture: Option<PathBuf>,
}

/// Cooked material of version 9, before flat shading. Bincode writes a
/// nested struct as its fields in order, so this is the name followed by
/// the version 8 fields.
#[derive(Serialize, Deserialize)]
struct SceneMaterialDataV9 {
    name: String,
    rest: SceneMaterialDataV8,
}

impl From<SceneMaterialDataV9> for SceneMaterialData {
    fn from(material: SceneMaterialDataV9) -> Self {
        Self {
            name: material.name,
            ..material.rest.into()
        }
    }
}

impl From<SceneMaterialDataV8> for SceneMaterialData {
    fn from(material: SceneMaterialDataV8) -> Self {
        Self {
            name: String::new(),
            model: material.model,
            alpha_mode: material.alpha_mode,
            base_color: material.base_color,
            emissive: material.emissive,
            metallic: material.metallic,
            roughness: material.roughness,
            transmission: material.transmission,
            ior: material.ior,
            thickness: material.thickness,
            uv_scale: material.uv_scale,
            uv_offset: material.uv_offset,
            flat_shading: false,
            base_color_texture: material.base_color_texture,
            normal_texture: material.normal_texture,
            metallic_roughness_texture: material.metallic_roughness_texture,
            occlusion_texture: material.occlusion_texture,
            emissive_texture: material.emissive_texture,
        }
    }
}

impl<M: Into<SceneMaterialData>> From<LegacySceneDocumentV8<M>>
    for SceneDocument
{
    fn from(document: LegacySceneDocumentV8<M>) -> Self {
        Self {
            format_version: document.format_version,
            name: document.name,
            entities: document.entities.into_iter().map(Into::into).collect(),
            render: document.render,
            simulation: document.simulation,
        }
    }
}

impl<M: Into<SceneMaterialData>> From<SceneEntityV8<M>> for SceneEntity {
    fn from(entity: SceneEntityV8<M>) -> Self {
        Self {
            id: entity.id,
            parent: entity.parent,
            name: entity.name,
            classes: entity.classes,
            transform: entity.transform,
            mesh_renderer: entity.mesh_renderer.map(|renderer| {
                SceneMeshRenderer {
                    mesh: renderer.mesh,
                    material: match renderer.material {
                        SceneMaterialV8::BuiltinError => {
                            SceneMaterial::BuiltinError
                        }
                        SceneMaterialV8::Inline(material) => {
                            SceneMaterial::Inline(material.into())
                        }
                    },
                    cast_shadows: renderer.cast_shadows,
                    receive_shadows: renderer.receive_shadows,
                }
            }),
            camera: entity.camera,
            visible: entity.visible,
            physics_body: entity.physics_body,
            rigid_body: entity.rigid_body,
            collider: entity.collider,
            collision_layers: entity.collision_layers,
            gpu_physics_watch: entity.gpu_physics_watch,
            components: entity.components,
            directional_light: entity.directional_light,
            point_light: entity.point_light,
            spot_light: entity.spot_light,
        }
    }
}

/// Cooked shape of version 7, before transmission and UV scale on inline
/// materials.
#[derive(Serialize, Deserialize)]
struct LegacySceneDocumentV7 {
    format_version: u32,
    name: String,
    entities: Vec<SceneEntityV7>,
    render: SceneRenderSettings,
    simulation: SceneSimulationSettings,
}

impl From<LegacySceneDocumentV7> for SceneDocument {
    fn from(document: LegacySceneDocumentV7) -> Self {
        Self {
            format_version: document.format_version,
            name: document.name,
            entities: document.entities.into_iter().map(Into::into).collect(),
            render: document.render,
            simulation: document.simulation,
        }
    }
}

/// [`SceneEntity`] as versions 5 to 7 wrote it. Bincode reads fields by
/// position, so the old material layout needs its own types.
#[derive(Serialize, Deserialize)]
struct SceneEntityV7 {
    id: Uuid,
    parent: Option<Uuid>,
    name: Option<String>,
    #[serde(default)]
    classes: Vec<String>,
    transform: Option<SceneTransform>,
    mesh_renderer: Option<SceneMeshRendererV7>,
    camera: Option<SceneCamera>,
    visible: Option<bool>,
    #[serde(default)]
    physics_body: Option<PhysicsBody>,
    #[serde(default)]
    rigid_body: Option<RigidBody>,
    #[serde(default)]
    collider: Option<Collider>,
    #[serde(default)]
    collision_layers: Option<CollisionLayers>,
    #[serde(default)]
    gpu_physics_watch: Option<GpuPhysicsWatch>,
    #[serde(default)]
    components: BTreeMap<String, String>,
    #[serde(default)]
    directional_light: Option<DirectionalLight>,
    #[serde(default)]
    point_light: Option<PointLight>,
    #[serde(default)]
    spot_light: Option<SpotLight>,
}

#[derive(Serialize, Deserialize)]
struct SceneMeshRendererV7 {
    mesh: SceneMesh,
    material: SceneMaterialV7,
    cast_shadows: bool,
    receive_shadows: bool,
}

#[derive(Serialize, Deserialize)]
enum SceneMaterialV7 {
    BuiltinError,
    Inline(SceneMaterialDataV7),
}

#[derive(Serialize, Deserialize)]
struct SceneMaterialDataV7 {
    model: SceneMaterialModel,
    #[serde(default)]
    alpha_mode: SceneAlphaMode,
    base_color: [f32; 4],
    emissive: [f32; 3],
    metallic: f32,
    roughness: f32,
    base_color_texture: Option<PathBuf>,
    normal_texture: Option<PathBuf>,
    metallic_roughness_texture: Option<PathBuf>,
    occlusion_texture: Option<PathBuf>,
    emissive_texture: Option<PathBuf>,
}

impl From<SceneEntityV7> for SceneEntity {
    fn from(entity: SceneEntityV7) -> Self {
        Self {
            id: entity.id,
            parent: entity.parent,
            name: entity.name,
            classes: entity.classes,
            transform: entity.transform,
            mesh_renderer: entity.mesh_renderer.map(|renderer| {
                SceneMeshRenderer {
                    mesh: renderer.mesh,
                    material: match renderer.material {
                        SceneMaterialV7::BuiltinError => {
                            SceneMaterial::BuiltinError
                        }
                        SceneMaterialV7::Inline(material) => {
                            SceneMaterial::Inline(SceneMaterialData {
                                name: String::new(),
                                model: material.model,
                                alpha_mode: material.alpha_mode,
                                base_color: material.base_color,
                                emissive: material.emissive,
                                metallic: material.metallic,
                                roughness: material.roughness,
                                transmission: 0.0,
                                ior: default_ior(),
                                thickness: 0.0,
                                uv_scale: default_uv_scale(),
                                uv_offset: [0.0; 2],
                                flat_shading: false,
                                base_color_texture: material.base_color_texture,
                                normal_texture: material.normal_texture,
                                metallic_roughness_texture: material
                                    .metallic_roughness_texture,
                                occlusion_texture: material.occlusion_texture,
                                emissive_texture: material.emissive_texture,
                            })
                        }
                    },
                    cast_shadows: renderer.cast_shadows,
                    receive_shadows: renderer.receive_shadows,
                }
            }),
            camera: entity.camera,
            visible: entity.visible,
            physics_body: entity.physics_body,
            rigid_body: entity.rigid_body,
            collider: entity.collider,
            collision_layers: entity.collision_layers,
            gpu_physics_watch: entity.gpu_physics_watch,
            components: entity.components,
            directional_light: entity.directional_light,
            point_light: entity.point_light,
            spot_light: entity.spot_light,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct SceneEntity {
    pub id: Uuid,
    pub parent: Option<Uuid>,
    pub name: Option<String>,
    #[serde(default)]
    pub classes: Vec<String>,
    pub transform: Option<SceneTransform>,
    pub mesh_renderer: Option<SceneMeshRenderer>,
    pub camera: Option<SceneCamera>,
    pub visible: Option<bool>,
    #[serde(default)]
    pub physics_body: Option<PhysicsBody>,
    #[serde(default)]
    pub rigid_body: Option<RigidBody>,
    #[serde(default)]
    pub collider: Option<Collider>,
    #[serde(default)]
    pub collision_layers: Option<CollisionLayers>,
    #[serde(default)]
    pub gpu_physics_watch: Option<GpuPhysicsWatch>,
    #[serde(default)]
    pub components: BTreeMap<String, String>,
    #[serde(default)]
    pub directional_light: Option<DirectionalLight>,
    #[serde(default)]
    pub point_light: Option<PointLight>,
    #[serde(default)]
    pub spot_light: Option<SpotLight>,
}

/// Version 4 stored lights, but materials had no alpha mode.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
struct LegacySceneDocumentV4 {
    format_version: u32,
    name: String,
    entities: Vec<LegacySceneEntityV4>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
struct LegacySceneEntityV4 {
    id: Uuid,
    parent: Option<Uuid>,
    name: Option<String>,
    classes: Vec<String>,
    transform: Option<SceneTransform>,
    mesh_renderer: Option<LegacySceneMeshRendererV4>,
    camera: Option<SceneCamera>,
    visible: Option<bool>,
    physics_body: Option<PhysicsBody>,
    rigid_body: Option<RigidBody>,
    collider: Option<Collider>,
    collision_layers: Option<CollisionLayers>,
    gpu_physics_watch: Option<GpuPhysicsWatch>,
    components: BTreeMap<String, String>,
    directional_light: Option<DirectionalLight>,
    point_light: Option<PointLight>,
    spot_light: Option<SpotLight>,
}

impl From<LegacySceneDocumentV4> for SceneDocument {
    fn from(document: LegacySceneDocumentV4) -> Self {
        Self {
            format_version: document.format_version,
            name: document.name,
            entities: document
                .entities
                .into_iter()
                .map(|entity| SceneEntity {
                    id: entity.id,
                    parent: entity.parent,
                    name: entity.name,
                    classes: entity.classes,
                    transform: entity.transform,
                    mesh_renderer: entity.mesh_renderer.map(Into::into),
                    camera: entity.camera,
                    visible: entity.visible,
                    physics_body: entity.physics_body,
                    rigid_body: entity.rigid_body,
                    collider: entity.collider,
                    collision_layers: entity.collision_layers,
                    gpu_physics_watch: entity.gpu_physics_watch,
                    components: entity.components,
                    directional_light: entity.directional_light,
                    point_light: entity.point_light,
                    spot_light: entity.spot_light,
                })
                .collect(),
            render: SceneRenderSettings::default(),
            simulation: SceneSimulationSettings::default(),
        }
    }
}

/// Mesh renderer shape shared by cooked versions 1 through 4.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
struct LegacySceneMeshRendererV4 {
    mesh: SceneMesh,
    material: LegacySceneMaterialV4,
    cast_shadows: bool,
    receive_shadows: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
enum LegacySceneMaterialV4 {
    BuiltinError,
    Inline(LegacySceneMaterialDataV4),
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
struct LegacySceneMaterialDataV4 {
    model: SceneMaterialModel,
    base_color: [f32; 4],
    emissive: [f32; 3],
    metallic: f32,
    roughness: f32,
    base_color_texture: Option<PathBuf>,
    normal_texture: Option<PathBuf>,
    metallic_roughness_texture: Option<PathBuf>,
    occlusion_texture: Option<PathBuf>,
    emissive_texture: Option<PathBuf>,
}

impl From<LegacySceneMeshRendererV4> for SceneMeshRenderer {
    fn from(renderer: LegacySceneMeshRendererV4) -> Self {
        Self {
            mesh: renderer.mesh,
            material: match renderer.material {
                LegacySceneMaterialV4::BuiltinError => {
                    SceneMaterial::BuiltinError
                }
                LegacySceneMaterialV4::Inline(material) => {
                    SceneMaterial::Inline(SceneMaterialData {
                        name: String::new(),
                        model: material.model,
                        alpha_mode: SceneAlphaMode::Opaque,
                        base_color: material.base_color,
                        emissive: material.emissive,
                        metallic: material.metallic,
                        roughness: material.roughness,
                        transmission: 0.0,
                        ior: default_ior(),
                        thickness: 0.0,
                        uv_scale: default_uv_scale(),
                        uv_offset: [0.0; 2],
                        flat_shading: false,
                        base_color_texture: material.base_color_texture,
                        normal_texture: material.normal_texture,
                        metallic_roughness_texture: material
                            .metallic_roughness_texture,
                        occlusion_texture: material.occlusion_texture,
                        emissive_texture: material.emissive_texture,
                    })
                }
            },
            cast_shadows: renderer.cast_shadows,
            receive_shadows: renderer.receive_shadows,
        }
    }
}

/// Version 3 stored classes and GPU watches, but no authored lights.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
struct LegacySceneDocumentV3 {
    format_version: u32,
    name: String,
    entities: Vec<LegacySceneEntityV3>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
struct LegacySceneEntityV3 {
    id: Uuid,
    parent: Option<Uuid>,
    name: Option<String>,
    classes: Vec<String>,
    transform: Option<SceneTransform>,
    mesh_renderer: Option<LegacySceneMeshRendererV4>,
    camera: Option<SceneCamera>,
    visible: Option<bool>,
    physics_body: Option<PhysicsBody>,
    rigid_body: Option<RigidBody>,
    collider: Option<Collider>,
    collision_layers: Option<CollisionLayers>,
    gpu_physics_watch: Option<GpuPhysicsWatch>,
    components: BTreeMap<String, String>,
}

impl From<LegacySceneDocumentV3> for SceneDocument {
    fn from(document: LegacySceneDocumentV3) -> Self {
        Self {
            format_version: document.format_version,
            name: document.name,
            entities: document
                .entities
                .into_iter()
                .map(|entity| SceneEntity {
                    id: entity.id,
                    parent: entity.parent,
                    name: entity.name,
                    classes: entity.classes,
                    transform: entity.transform,
                    mesh_renderer: entity.mesh_renderer.map(Into::into),
                    camera: entity.camera,
                    visible: entity.visible,
                    physics_body: entity.physics_body,
                    rigid_body: entity.rigid_body,
                    collider: entity.collider,
                    collision_layers: entity.collision_layers,
                    gpu_physics_watch: entity.gpu_physics_watch,
                    components: entity.components,
                    directional_light: None,
                    point_light: None,
                    spot_light: None,
                })
                .collect(),
            render: SceneRenderSettings::default(),
            simulation: SceneSimulationSettings::default(),
        }
    }
}

/// Cooked version 1 did not store programmable GPU physics watches.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
struct LegacySceneDocumentV1 {
    format_version: u32,
    name: String,
    entities: Vec<LegacySceneEntityV1>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
struct LegacySceneEntityV1 {
    id: Uuid,
    parent: Option<Uuid>,
    name: Option<String>,
    transform: Option<SceneTransform>,
    mesh_renderer: Option<LegacySceneMeshRendererV4>,
    camera: Option<SceneCamera>,
    visible: Option<bool>,
    physics_body: Option<PhysicsBody>,
    rigid_body: Option<RigidBody>,
    collider: Option<Collider>,
    collision_layers: Option<CollisionLayers>,
    components: BTreeMap<String, String>,
}

impl From<LegacySceneDocumentV1> for SceneDocument {
    fn from(document: LegacySceneDocumentV1) -> Self {
        Self {
            format_version: document.format_version,
            name: document.name,
            entities: document
                .entities
                .into_iter()
                .map(|entity| SceneEntity {
                    id: entity.id,
                    parent: entity.parent,
                    name: entity.name,
                    classes: Vec::new(),
                    transform: entity.transform,
                    mesh_renderer: entity.mesh_renderer.map(Into::into),
                    camera: entity.camera,
                    visible: entity.visible,
                    physics_body: entity.physics_body,
                    rigid_body: entity.rigid_body,
                    collider: entity.collider,
                    collision_layers: entity.collision_layers,
                    gpu_physics_watch: None,
                    components: entity.components,
                    directional_light: None,
                    point_light: None,
                    spot_light: None,
                })
                .collect(),
            render: SceneRenderSettings::default(),
            simulation: SceneSimulationSettings::default(),
        }
    }
}

/// Cooked version 2 stored GPU watches but did not store object classes.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
struct LegacySceneDocumentV2 {
    format_version: u32,
    name: String,
    entities: Vec<LegacySceneEntityV2>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
struct LegacySceneEntityV2 {
    id: Uuid,
    parent: Option<Uuid>,
    name: Option<String>,
    transform: Option<SceneTransform>,
    mesh_renderer: Option<LegacySceneMeshRendererV4>,
    camera: Option<SceneCamera>,
    visible: Option<bool>,
    physics_body: Option<PhysicsBody>,
    rigid_body: Option<RigidBody>,
    collider: Option<Collider>,
    collision_layers: Option<CollisionLayers>,
    gpu_physics_watch: Option<GpuPhysicsWatch>,
    components: BTreeMap<String, String>,
}

impl From<LegacySceneDocumentV2> for SceneDocument {
    fn from(document: LegacySceneDocumentV2) -> Self {
        Self {
            format_version: document.format_version,
            name: document.name,
            entities: document
                .entities
                .into_iter()
                .map(|entity| SceneEntity {
                    id: entity.id,
                    parent: entity.parent,
                    name: entity.name,
                    classes: Vec::new(),
                    transform: entity.transform,
                    mesh_renderer: entity.mesh_renderer.map(Into::into),
                    camera: entity.camera,
                    visible: entity.visible,
                    physics_body: entity.physics_body,
                    rigid_body: entity.rigid_body,
                    collider: entity.collider,
                    collision_layers: entity.collision_layers,
                    gpu_physics_watch: entity.gpu_physics_watch,
                    components: entity.components,
                    directional_light: None,
                    point_light: None,
                    spot_light: None,
                })
                .collect(),
            render: SceneRenderSettings::default(),
            simulation: SceneSimulationSettings::default(),
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq)]
pub struct SceneTransform {
    pub position: [f32; 3],
    pub rotation: [f32; 3],
    pub scale: [f32; 3],
}

impl From<Transform> for SceneTransform {
    fn from(transform: Transform) -> Self {
        Self {
            position: transform.position,
            rotation: transform.rotation,
            scale: transform.scale,
        }
    }
}

impl From<SceneTransform> for Transform {
    fn from(transform: SceneTransform) -> Self {
        Self {
            position: transform.position,
            rotation: transform.rotation,
            scale: transform.scale,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum SceneMesh {
    BuiltinCube,
    BuiltinSphere,
    AssetPath(PathBuf),
    // Keep new variants after the original three so old bincode discriminants
    // remain valid.
    BuiltinPrimitive(PrimitiveShape),
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[allow(clippy::large_enum_variant)] // one value per renderer while saving; boxing buys nothing
pub enum SceneMaterial {
    BuiltinError,
    Inline(SceneMaterialData),
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct SceneMaterialData {
    /// Editor name, such as "glass"; empty when the material has none.
    /// Cooked version 8 scenes have no name.
    #[serde(default)]
    pub name: String,
    pub model: SceneMaterialModel,
    #[serde(default)]
    pub alpha_mode: SceneAlphaMode,
    pub base_color: [f32; 4],
    pub emissive: [f32; 3],
    pub metallic: f32,
    pub roughness: f32,
    // The cooked form is bincode, so these fields are always written.
    #[serde(default)]
    pub transmission: f32,
    #[serde(default = "default_ior")]
    pub ior: f32,
    #[serde(default)]
    pub thickness: f32,
    #[serde(default = "default_uv_scale")]
    pub uv_scale: [f32; 2],
    #[serde(default)]
    pub uv_offset: [f32; 2],
    /// Light each triangle with its own face normal for a faceted look.
    /// Cooked version 9 scenes end the shading fields before it.
    #[serde(default)]
    pub flat_shading: bool,
    pub base_color_texture: Option<PathBuf>,
    pub normal_texture: Option<PathBuf>,
    pub metallic_roughness_texture: Option<PathBuf>,
    pub occlusion_texture: Option<PathBuf>,
    pub emissive_texture: Option<PathBuf>,
}

fn default_uv_scale() -> [f32; 2] {
    [1.0; 2]
}

fn default_ior() -> f32 {
    MaterialAsset::default().ior
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq)]
pub enum SceneAlphaMode {
    #[default]
    Opaque,
    Mask {
        cutoff: f32,
    },
    Blend,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum SceneMaterialModel {
    Pbr,
    Unlit,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct SceneMeshRenderer {
    pub mesh: SceneMesh,
    pub material: SceneMaterial,
    pub cast_shadows: bool,
    pub receive_shadows: bool,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq)]
pub struct SceneCamera {
    pub projection: SceneProjection,
    pub active: bool,
    pub priority: i32,
    // No skip_serializing_if: cooked scenes are bincode, which needs every
    // field written.
    #[serde(default)]
    pub viewport: Option<[f32; 4]>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq)]
pub enum SceneProjection {
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

#[derive(Clone)]
struct ComponentRegistration {
    ty: Arc<ComponentType>,
    to_text: fn(&World, Entity) -> Option<serde_json::Result<String>>,
    to_value: fn(&World, Entity) -> Option<serde_json::Result<Value>>,
    from_value: fn(&mut World, Entity, Value) -> serde_json::Result<()>,
    insert_default: fn(&mut World, Entity),
    remove: fn(&mut World, Entity),
}

/// What the registry knows about a component's type besides its Rust code.
#[derive(Clone)]
struct ComponentType {
    info: TypeInfo,
    migrations: Vec<FieldMigration>,
    /// Holds entities or handles, which save in a different form.
    references: bool,
}

impl ComponentType {
    fn version(&self) -> u32 {
        self.migrations.len() as u32
    }
}

/// Allowlist for game-defined, compiled Rust components stored in scenes.
#[derive(Resource)]
pub struct SceneComponentRegistry {
    registrations: BTreeMap<String, ComponentRegistration>,
    /// See [`SceneComponentRegistry::keep_unregistered`].
    keep_unregistered: bool,
}

/// Saved JSON of the components a tool had no registration for, kept so a
/// save writes them back unchanged. See
/// [`SceneComponentRegistry::keep_unregistered`].
#[derive(Component, Clone, Debug, Default, PartialEq, Eq)]
pub struct UnregisteredComponents(pub BTreeMap<String, String>);

/// Registry name of the built-in ambient light, stored like a game
/// component so adding it did not change the scene binary layout.
pub const AMBIENT_LIGHT_COMPONENT: &str = "rusting.ambient_light";
/// Registry name of the built-in hemisphere sky light.
pub const SKY_LIGHT_COMPONENT: &str = "rusting.sky_light";
/// Registry name of the built-in exposure and tone mapping settings.
pub const TONE_MAPPING_COMPONENT: &str = "rusting.tone_mapping";
/// Registry name of the built-in render-only visibility bounds.
pub const RENDER_BOUNDS_COMPONENT: &str = "rusting.render_bounds";
/// Registry name of the built-in per-body GPU synchronization mode.
pub const PHYSICS_SYNC_COMPONENT: &str = "rusting.physics_sync";
/// Registry name of the built-in automatic CPU/GPU allocation marker.
pub const AUTO_SIMULATION_COMPONENT: &str = "rusting.auto_simulation";
/// Registry name of the built-in first-person player controller.
pub const PLAYER_CONTROLLER_COMPONENT: &str = "rusting.player_controller";
/// Registry name of the built-in transform tween.
pub const TWEEN_COMPONENT: &str = "rusting.tween";
/// Registry name of the built-in bulk spawn grid.
pub const SPAWN_GRID_COMPONENT: &str = "rusting.spawn_grid";
/// Registry name of the built-in squash and stretch spring.
pub const SQUASH_COMPONENT: &str = "rusting.squash";
/// Registry name of the built-in hit flash.
pub const FLASH_COMPONENT: &str = "rusting.flash";
pub const MATERIAL_OVERRIDE_COMPONENT: &str = "rusting.material_override";
/// Registry name of the built-in camera trauma shake.
pub const CAMERA_SHAKE_COMPONENT: &str = "rusting.camera_shake";
/// Registry name of the built-in event-triggered sound cue.
pub const SOUND_CUE_COMPONENT: &str = "rusting.sound_cue";
/// Registry name of the built-in listener reverb zone.
pub const REVERB_ZONE_COMPONENT: &str = "rusting.reverb_zone";
/// Registry name of the built-in slide and roll sound loop.
pub const SLIDE_SOUND_COMPONENT: &str = "rusting.slide_sound";
/// Registry name of the built-in particle burst emitter.
pub const BURST_EMITTER_COMPONENT: &str = "rusting.burst_emitter";
/// Registry name of the built-in particle emitter.
pub const PARTICLE_EMITTER_COMPONENT: &str = "rusting.particle_emitter";
/// Registry name of the built-in particle shader.
pub const PARTICLE_SHADER_COMPONENT: &str = "rusting.particle_shader";
/// Registry name of the built-in keyframe animation.
pub const ANIMATION_COMPONENT: &str = "rusting.animation";
/// Registry name of the built-in skinned mesh joints.
pub const SKIN_COMPONENT: &str = "rusting.skin";
/// Registry name of the built-in inverse kinematics.
pub const IK_COMPONENT: &str = "rusting.ik";
/// Registry name of the built-in rope.
pub const ROPE_COMPONENT: &str = "rusting.rope";
/// Registry name of the built-in ragdoll.
pub const RAGDOLL_COMPONENT: &str = "rusting.ragdoll";
/// Registry name of the built-in blend shape weights.
pub const MORPH_COMPONENT: &str = "rusting.morph";
/// Registry name of the built-in fluid block.
pub const FLUID_BLOCK_COMPONENT: &str = "rusting.fluid_block";
/// Registry name of the built-in soft-body block.
pub const SOFT_BLOCK_COMPONENT: &str = "rusting.soft_block";
/// Registry name of the built-in cloth sheet.
pub const CLOTH_COMPONENT: &str = "rusting.cloth";
pub const WATER_COMPONENT: &str = "rusting.water";
pub const GRAVITY_VOLUME_COMPONENT: &str = "rusting.gravity_volume";
pub const VEHICLE_COMPONENT: &str = "rusting.vehicle";
pub const FORCE_FIELD_COMPONENT: &str = "rusting.force_field";
pub const AXIS_LOCK_COMPONENT: &str = "rusting.axis_lock";
pub const HEIGHTFIELD_COMPONENT: &str = "rusting.heightfield";
pub const POLYGON_COMPONENT: &str = "rusting.polygon";
/// Registry name of the built-in pre-fracture settings.
pub const FRACTURE_COMPONENT: &str = "rusting.fracture";
pub const MESH_SURFACES_COMPONENT: &str = "rusting.mesh_surfaces";
pub const PHYSICS_MATERIAL_COMPONENT: &str = "rusting.physics_material";
/// Registry name of the built-in HUD text or button.
pub const HUD_ELEMENT_COMPONENT: &str = "rusting.hud";
/// Registry name of the built-in reflected sky image.
pub const ENVIRONMENT_MAP_COMPONENT: &str = "rusting.environment_map";
/// Registry name of the built-in spot light image.
pub const LIGHT_COOKIE_COMPONENT: &str = "rusting.light_cookie";
/// Registry name of the built-in reflection probe.
pub const REFLECTION_PROBE_COMPONENT: &str = "rusting.reflection_probe";
/// Registry name of the built-in light probe grid.
pub const LIGHT_PROBE_GRID_COMPONENT: &str = "rusting.light_probe_grid";
/// Registry name of the built-in baked lightmap.
pub const LIGHTMAP_COMPONENT: &str = "rusting.lightmap";
/// Registry name of the built-in foliage scatter.
pub const FOLIAGE_COMPONENT: &str = "rusting.foliage";
/// Registry name of the built-in heightmap terrain.
pub const TERRAIN_COMPONENT: &str = "rusting.terrain";
/// Registry name of the built-in height fog.
pub const FOG_COMPONENT: &str = "rusting.fog";
/// Registry name of the built-in bloom.
pub const BLOOM_COMPONENT: &str = "rusting.bloom";
/// Registry name of the built-in eye adaptation.
pub const AUTO_EXPOSURE_COMPONENT: &str = "rusting.auto_exposure";
pub const COLOR_GRADING_COMPONENT: &str = "rusting.color_grading";
/// Registry name of the built-in color lookup table.
pub const COLOR_LUT_COMPONENT: &str = "rusting.color_lut";
pub const POST_SHADER_COMPONENT: &str = "rusting.post_shader";
pub const SKY_SHADER_COMPONENT: &str = "rusting.sky_shader";
/// Registry name of the built-in fog shader.
pub const FOG_SHADER_COMPONENT: &str = "rusting.fog_shader";
/// Registry name of the built-in lens blur.
pub const DEPTH_OF_FIELD_COMPONENT: &str = "rusting.depth_of_field";
/// Registry name of the built-in camera motion blur.
pub const MOTION_BLUR_COMPONENT: &str = "rusting.motion_blur";
/// Registry name of the built-in daylight sky.
pub const PROCEDURAL_SKY_COMPONENT: &str = "rusting.procedural_sky";
/// Registry name of the built-in box of local fog.
pub const FOG_VOLUME_COMPONENT: &str = "rusting.fog_volume";
/// Registry name of the built-in projected image.
pub const DECAL_COMPONENT: &str = "rusting.decal";
/// Registry name of the built-in fog and grading area.
pub const POST_VOLUME_COMPONENT: &str = "rusting.post_volume";
pub const CAMERA_SCREEN_COMPONENT: &str = "rusting.camera_screen";
/// Registry name of the built-in screen-space ambient occlusion.
pub const AMBIENT_OCCLUSION_COMPONENT: &str = "rusting.ambient_occlusion";
/// Registry name of the built-in scene clear color.
pub const BACKGROUND_COMPONENT: &str = "rusting.background";
/// Registry name of the built-in named counter.
pub const COUNTER_COMPONENT: &str = "rusting.counter";
/// Registry name of the built-in branching conversation.
pub const DIALOGUE_COMPONENT: &str = "rusting.dialogue";
/// Registry name of the built-in hit points.
pub const HEALTH_COMPONENT: &str = "rusting.health";
/// Registry name of the built-in weighted loot table.
pub const LOOT_TABLE_COMPONENT: &str = "rusting.loot_table";
/// Registry name of the built-in state machine state.
pub const OBJECT_STATE_COMPONENT: &str = "rusting.state";
/// Registry name of the built-in collectable.
pub const PICKUP_COMPONENT: &str = "rusting.pickup";
pub const CONNECTIONS_COMPONENT: &str = "rusting.connections";
/// Registry name of the built-in CPU physics joint.
pub const JOINT_COMPONENT: &str = "rusting.joint";
/// Registry name of the built-in reduced-coordinate articulation root.
pub const ARTICULATION_COMPONENT: &str = "rusting.articulation";
/// Registry name of the built-in text tile map.
pub const TILE_MAP_COMPONENT: &str = "rusting.tile_map";
/// Registry name of the built-in scene-data input binding.
pub const INPUT_ACTION_COMPONENT: &str = "rusting.input_action";
/// Registry name of the built-in side-view platformer controller.
pub const PLATFORMER_CONTROLLER_COMPONENT: &str =
    "rusting.platformer_controller";

impl Default for SceneComponentRegistry {
    fn default() -> Self {
        let mut registry = Self {
            registrations: BTreeMap::new(),
            keep_unregistered: false,
        };
        registry
            .register::<AmbientLight>(AMBIENT_LIGHT_COMPONENT)
            .expect("empty registry has no duplicates");
        registry
            .register::<SkyLight>(SKY_LIGHT_COMPONENT)
            .expect("empty registry has no duplicates");
        registry
            .register::<ToneMapping>(TONE_MAPPING_COMPONENT)
            .expect("empty registry has no duplicates");
        registry
            .register::<RenderBounds>(RENDER_BOUNDS_COMPONENT)
            .expect("empty registry has no duplicates");
        registry
            .register::<super::PhysicsSyncMode>(PHYSICS_SYNC_COMPONENT)
            .expect("empty registry has no duplicates");
        registry
            .register::<super::AutoSimulation>(AUTO_SIMULATION_COMPONENT)
            .expect("empty registry has no duplicates");
        registry
            .register::<super::PlayerController>(PLAYER_CONTROLLER_COMPONENT)
            .expect("empty registry has no duplicates");
        registry
            .register::<super::Tween>(TWEEN_COMPONENT)
            .expect("empty registry has no duplicates");
        registry
            .register::<super::SoundCue>(SOUND_CUE_COMPONENT)
            .expect("empty registry has no duplicates");
        registry
            .register::<super::ReverbZone>(REVERB_ZONE_COMPONENT)
            .expect("empty registry has no duplicates");
        registry
            .register::<super::SlideSound>(SLIDE_SOUND_COMPONENT)
            .expect("empty registry has no duplicates");
        registry
            .register::<super::CameraShake>(CAMERA_SHAKE_COMPONENT)
            .expect("empty registry has no duplicates");
        registry
            .register::<super::Squash>(SQUASH_COMPONENT)
            .expect("empty registry has no duplicates");
        registry
            .register::<super::Flash>(FLASH_COMPONENT)
            .expect("empty registry has no duplicates");
        registry
            .register::<super::MaterialOverride>(MATERIAL_OVERRIDE_COMPONENT)
            .expect("empty registry has no duplicates");
        registry
            .register::<super::SpawnGrid>(SPAWN_GRID_COMPONENT)
            .expect("empty registry has no duplicates");
        registry
            .register::<super::BurstEmitter>(BURST_EMITTER_COMPONENT)
            .expect("empty registry has no duplicates");
        registry
            .register::<super::ParticleEmitter>(PARTICLE_EMITTER_COMPONENT)
            .expect("empty registry has no duplicates");
        registry
            .register::<super::ParticleShader>(PARTICLE_SHADER_COMPONENT)
            .expect("empty registry has no duplicates");
        registry
            .register::<super::Animation>(ANIMATION_COMPONENT)
            .expect("empty registry has no duplicates");
        registry
            .register::<super::Skin>(SKIN_COMPONENT)
            .expect("empty registry has no duplicates");
        registry
            .register::<super::Morph>(MORPH_COMPONENT)
            .expect("empty registry has no duplicates");
        registry
            .register::<super::Ik>(IK_COMPONENT)
            .expect("empty registry has no duplicates");
        registry
            .register::<super::Ragdoll>(RAGDOLL_COMPONENT)
            .expect("empty registry has no duplicates");
        registry
            .register::<super::Rope>(ROPE_COMPONENT)
            .expect("empty registry has no duplicates");
        registry
            .register::<super::FluidBlock>(FLUID_BLOCK_COMPONENT)
            .expect("empty registry has no duplicates");
        registry
            .register::<super::SoftBlock>(SOFT_BLOCK_COMPONENT)
            .expect("empty registry has no duplicates");
        registry
            .register::<super::ClothSheet>(CLOTH_COMPONENT)
            .expect("empty registry has no duplicates");
        registry
            .register::<super::WaterBody>(WATER_COMPONENT)
            .expect("empty registry has no duplicates");
        registry
            .register::<super::GravityVolume>(GRAVITY_VOLUME_COMPONENT)
            .expect("empty registry has no duplicates");
        registry
            .register::<super::Vehicle>(VEHICLE_COMPONENT)
            .expect("empty registry has no duplicates");
        registry
            .register::<super::ForceField>(FORCE_FIELD_COMPONENT)
            .expect("empty registry has no duplicates");
        registry
            .register::<super::AxisLock>(AXIS_LOCK_COMPONENT)
            .expect("empty registry has no duplicates");
        registry
            .register::<super::Heightfield>(HEIGHTFIELD_COMPONENT)
            .expect("empty registry has no duplicates");
        registry
            .register::<super::Polygon2d>(POLYGON_COMPONENT)
            .expect("empty registry has no duplicates");
        registry
            .register::<super::Fracture>(FRACTURE_COMPONENT)
            .expect("empty registry has no duplicates");
        registry
            .register::<super::MeshSurfaces>(MESH_SURFACES_COMPONENT)
            .expect("empty registry has no duplicates");
        registry
            .register::<super::PhysicsMaterial>(PHYSICS_MATERIAL_COMPONENT)
            .expect("empty registry has no duplicates");
        registry
            .register::<super::HudElement>(HUD_ELEMENT_COMPONENT)
            .expect("empty registry has no duplicates");
        registry
            .register::<super::SceneBackground>(BACKGROUND_COMPONENT)
            .expect("empty registry has no duplicates");
        registry
            .register::<super::EnvironmentMap>(ENVIRONMENT_MAP_COMPONENT)
            .expect("empty registry has no duplicates");
        registry
            .register::<super::LightCookie>(LIGHT_COOKIE_COMPONENT)
            .expect("empty registry has no duplicates");
        registry
            .register::<super::ReflectionProbe>(REFLECTION_PROBE_COMPONENT)
            .expect("empty registry has no duplicates");
        registry
            .register::<super::LightProbeGrid>(LIGHT_PROBE_GRID_COMPONENT)
            .expect("empty registry has no duplicates");
        registry
            .register::<super::Lightmap>(LIGHTMAP_COMPONENT)
            .expect("empty registry has no duplicates");
        registry
            .register::<super::Foliage>(FOLIAGE_COMPONENT)
            .expect("empty registry has no duplicates");
        registry
            .register::<super::Terrain>(TERRAIN_COMPONENT)
            .expect("empty registry has no duplicates");
        registry
            .register::<super::Fog>(FOG_COMPONENT)
            .expect("empty registry has no duplicates");
        registry
            .register::<super::Bloom>(BLOOM_COMPONENT)
            .expect("empty registry has no duplicates");
        registry
            .register::<super::AutoExposure>(AUTO_EXPOSURE_COMPONENT)
            .expect("empty registry has no duplicates");
        registry
            .register::<super::ColorGrading>(COLOR_GRADING_COMPONENT)
            .expect("empty registry has no duplicates");
        registry
            .register::<super::ColorLut>(COLOR_LUT_COMPONENT)
            .expect("empty registry has no duplicates");
        registry
            .register::<super::PostShader>(POST_SHADER_COMPONENT)
            .expect("empty registry has no duplicates");
        registry
            .register::<super::SkyShader>(SKY_SHADER_COMPONENT)
            .expect("empty registry has no duplicates");
        registry
            .register::<super::FogShader>(FOG_SHADER_COMPONENT)
            .expect("empty registry has no duplicates");
        registry
            .register::<super::DepthOfField>(DEPTH_OF_FIELD_COMPONENT)
            .expect("empty registry has no duplicates");
        registry
            .register::<super::MotionBlur>(MOTION_BLUR_COMPONENT)
            .expect("empty registry has no duplicates");
        registry
            .register::<super::PostVolume>(POST_VOLUME_COMPONENT)
            .expect("empty registry has no duplicates");
        registry
            .register::<super::FogVolume>(FOG_VOLUME_COMPONENT)
            .expect("empty registry has no duplicates");
        registry
            .register::<super::Decal>(DECAL_COMPONENT)
            .expect("empty registry has no duplicates");
        registry
            .register::<super::ProceduralSky>(PROCEDURAL_SKY_COMPONENT)
            .expect("empty registry has no duplicates");
        registry
            .register::<super::CameraScreen>(CAMERA_SCREEN_COMPONENT)
            .expect("empty registry has no duplicates");
        registry
            .register::<super::AmbientOcclusion>(AMBIENT_OCCLUSION_COMPONENT)
            .expect("empty registry has no duplicates");
        registry
            .register::<super::InputAction>(INPUT_ACTION_COMPONENT)
            .expect("empty registry has no duplicates");
        registry
            .register::<super::Counter>(COUNTER_COMPONENT)
            .expect("empty registry has no duplicates");
        registry
            .register::<super::Pickup>(PICKUP_COMPONENT)
            .expect("empty registry has no duplicates");
        registry
            .register::<super::ObjectState>(OBJECT_STATE_COMPONENT)
            .expect("empty registry has no duplicates");
        registry
            .register::<super::Health>(HEALTH_COMPONENT)
            .expect("empty registry has no duplicates");
        registry
            .register::<super::Dialogue>(DIALOGUE_COMPONENT)
            .expect("empty registry has no duplicates");
        registry
            .register::<super::LootTable>(LOOT_TABLE_COMPONENT)
            .expect("empty registry has no duplicates");
        registry
            .register::<super::TileMap>(TILE_MAP_COMPONENT)
            .expect("empty registry has no duplicates");
        registry
            .register::<super::PlatformerController>(
                PLATFORMER_CONTROLLER_COMPONENT,
            )
            .expect("empty registry has no duplicates");
        registry
            .register::<super::SceneInstance>(super::SCENE_INSTANCE_COMPONENT)
            .expect("empty registry has no duplicates");
        registry
            .register::<super::Connections>(CONNECTIONS_COMPONENT)
            .expect("empty registry has no duplicates");
        registry
            .register::<super::Joint>(JOINT_COMPONENT)
            .expect("empty registry has no duplicates");
        registry
            .register::<super::Articulation>(ARTICULATION_COMPONENT)
            .expect("empty registry has no duplicates");
        registry
    }
}

impl SceneComponentRegistry {
    /// For tools that load scenes without the game's plugins, such as the
    /// editor and `rusting capture`: a component with no registration is
    /// kept as saved JSON in [`UnregisteredComponents`] instead of failing
    /// the load. Games leave this off, so a misspelled or unregistered
    /// component still stops them.
    pub fn keep_unregistered(&mut self) {
        self.keep_unregistered = true;
    }

    pub fn register<T>(
        &mut self,
        name: impl Into<String>,
    ) -> Result<(), SceneIoError>
    where
        T: Component + Serialize + DeserializeOwned + Default + Reflect,
    {
        let name = name.into();
        if self.registrations.contains_key(&name) {
            return Err(SceneIoError::DuplicateComponent(name));
        }
        let info = T::type_info();
        self.registrations.insert(
            name,
            ComponentRegistration {
                ty: Arc::new(ComponentType {
                    references: info.has_references(),
                    info,
                    migrations: Vec::new(),
                }),
                to_text: component_text::<T>,
                to_value: component_value::<T>,
                from_value: insert_component_value::<T>,
                insert_default: insert_default_component::<T>,
                remove: remove_component::<T>,
            },
        );
        Ok(())
    }

    /// Records the next step in a component's history and raises its
    /// version by one. Values saved from then on carry `"$version"`, and
    /// older values run the steps they missed when they load. Register
    /// steps oldest first, and never remove one: saved versions count them.
    pub fn add_migration(
        &mut self,
        name: &str,
        migration: FieldMigration,
    ) -> Result<(), SceneIoError> {
        let registration = self
            .registrations
            .get_mut(name)
            .ok_or_else(|| SceneIoError::UnknownComponent(name.into()))?;
        let ty = Arc::make_mut(&mut registration.ty);
        if !matches!(ty.info, TypeInfo::Struct(_)) {
            return Err(reflect_error(
                name,
                ty,
                ty.version(),
                (String::new(), ReflectProblem::NotAStruct),
            ));
        }
        ty.migrations.push(migration);
        Ok(())
    }

    /// The reflected type of a registered component.
    #[must_use]
    pub fn info(&self, name: &str) -> Option<&TypeInfo> {
        self.registrations
            .get(name)
            .map(|registration| &registration.ty.info)
    }

    #[must_use]
    pub fn names(&self) -> impl ExactSizeIterator<Item = &str> {
        self.registrations.keys().map(String::as_str)
    }
}

/// Returns the scene form of every registered component on an entity, as
/// JSON text keyed by registry name.
pub fn registered_component_values(
    world: &World,
    entity: Entity,
) -> Result<Vec<(String, String)>, SceneIoError> {
    let registry = world.resource::<SceneComponentRegistry>();
    registry
        .registrations
        .iter()
        .filter_map(|(name, registration)| {
            capture_component(world, entity, name, registration)
                .transpose()
                .map(|result| result.map(|value| (name.clone(), value)))
        })
        .collect()
}

#[must_use]
pub fn registered_component_names(world: &World) -> Vec<String> {
    world
        .resource::<SceneComponentRegistry>()
        .registrations
        .keys()
        .cloned()
        .collect()
}

/// The reflected type of a registered component.
#[must_use]
pub fn registered_component_info<'a>(
    world: &'a World,
    name: &str,
) -> Option<&'a TypeInfo> {
    world.resource::<SceneComponentRegistry>().info(name)
}

fn registration(
    world: &World,
    name: &str,
) -> Result<ComponentRegistration, SceneIoError> {
    world
        .resource::<SceneComponentRegistry>()
        .registrations
        .get(name)
        .cloned()
        .ok_or_else(|| SceneIoError::UnknownComponent(name.into()))
}

/// Replaces a component with the value in `serialized`, in scene form.
pub fn set_registered_component(
    world: &mut World,
    entity: Entity,
    name: &str,
    serialized: &str,
) -> Result<(), SceneIoError> {
    let registration = registration(world, name)?;
    restore_component(world, entity, name, &registration, serialized, None)
}

pub fn add_registered_component(
    world: &mut World,
    entity: Entity,
    name: &str,
) -> Result<(), SceneIoError> {
    (registration(world, name)?.insert_default)(world, entity);
    Ok(())
}

pub fn remove_registered_component(
    world: &mut World,
    entity: Entity,
    name: &str,
) -> Result<(), SceneIoError> {
    if let Some(mut kept) = world.get_mut::<UnregisteredComponents>(entity) {
        if kept.0.remove(name).is_some() {
            return Ok(());
        }
    }
    (registration(world, name)?.remove)(world, entity);
    Ok(())
}

/// Reads one field of a registered component by JSON pointer into its
/// scene form, as animation tracks and tools address properties. `None`
/// when the entity lacks the component or an optional value on the way
/// is `null`.
pub fn registered_component_field(
    world: &World,
    entity: Entity,
    name: &str,
    pointer: &str,
) -> Result<Option<Value>, SceneIoError> {
    let registration = registration(world, name)?;
    let ty = &registration.ty;
    if ty.info.at(pointer).is_none() {
        return Err(reflect_error(
            name,
            ty,
            ty.version(),
            (pointer.into(), ReflectProblem::NoSuchPath),
        ));
    }
    let Some(value) = capture_value(world, entity, name, &registration)? else {
        return Ok(None);
    };
    Ok(value.pointer(pointer).cloned())
}

/// Writes one field of a registered component by JSON pointer into its
/// scene form. The value is checked against the field's type, then the
/// whole component goes through the same path as a scene load.
pub fn set_registered_component_field(
    world: &mut World,
    entity: Entity,
    name: &str,
    pointer: &str,
    value: Value,
) -> Result<(), SceneIoError> {
    let registration = registration(world, name)?;
    let ty = &registration.ty;
    let located = |problem| {
        reflect_error(name, ty, ty.version(), (pointer.to_owned(), problem))
    };
    let field = ty
        .info
        .at(pointer)
        .ok_or_else(|| located(ReflectProblem::NoSuchPath))?;
    if !field.accepts(&value) {
        return Err(located(ReflectProblem::WrongKind(field.kind_name())));
    }
    let mut component = capture_value(world, entity, name, &registration)?
        .ok_or_else(|| SceneIoError::Component {
            name: name.into(),
            message: "the entity does not have this component".into(),
        })?;
    // A pointer into a variant the value does not hold, or through a null
    // option, has nothing to write to.
    let slot = component
        .pointer_mut(pointer)
        .ok_or_else(|| located(ReflectProblem::NoSuchPath))?;
    *slot = value;
    restore_value(world, entity, name, &registration, component, None)
}

fn reflect_error(
    name: &str,
    ty: &ComponentType,
    saved_version: u32,
    (path, problem): reflect::Located,
) -> SceneIoError {
    SceneIoError::Reflection(Box::new(ReflectError {
        component: name.into(),
        saved_version,
        current_version: ty.version(),
        path,
        problem,
        object: None,
    }))
}

impl SceneIoError {
    /// Records the scene object a component value error came from.
    fn at_object(mut self, id: Uuid) -> Self {
        if let Self::Reflection(error) = &mut self {
            error.object.get_or_insert(id);
        }
        self
    }
}

fn serde_error(name: &str, error: &serde_json::Error) -> SceneIoError {
    SceneIoError::Component {
        name: name.into(),
        message: error.to_string(),
    }
}

/// The component's scene form, with `"$version"` when it has migrations.
fn capture_value(
    world: &World,
    entity: Entity,
    name: &str,
    registration: &ComponentRegistration,
) -> Result<Option<Value>, SceneIoError> {
    let Some(value) = (registration.to_value)(world, entity) else {
        return Ok(None);
    };
    let mut value = value.map_err(|error| serde_error(name, &error))?;
    let ty = &registration.ty;
    if ty.references {
        reflect::to_scene_form(&ty.info, &mut value, world).map_err(
            |located| reflect_error(name, ty, ty.version(), located),
        )?;
    }
    if let (Value::Object(map), version @ 1..) = (&mut value, ty.version()) {
        map.insert(reflect::VERSION_KEY.into(), version.into());
    }
    Ok(Some(value))
}

fn capture_component(
    world: &World,
    entity: Entity,
    name: &str,
    registration: &ComponentRegistration,
) -> Result<Option<String>, SceneIoError> {
    let ty = &registration.ty;
    if !ty.references && ty.migrations.is_empty() {
        // Serde's output is already the scene form.
        return (registration.to_text)(world, entity)
            .transpose()
            .map_err(|error| serde_error(name, &error));
    }
    Ok(capture_value(world, entity, name, registration)?
        .map(|value| value.to_string()))
}

/// Parses, migrates, and checks a saved component, then inserts it.
/// `ids` maps scene IDs to entities during a load.
fn restore_component(
    world: &mut World,
    entity: Entity,
    name: &str,
    registration: &ComponentRegistration,
    serialized: &str,
    ids: Option<&HashMap<Uuid, Entity>>,
) -> Result<(), SceneIoError> {
    let value = serde_json::from_str(serialized)
        .map_err(|error| serde_error(name, &error))?;
    restore_value(world, entity, name, registration, value, ids)
}

fn restore_value(
    world: &mut World,
    entity: Entity,
    name: &str,
    registration: &ComponentRegistration,
    mut value: Value,
    ids: Option<&HashMap<Uuid, Entity>>,
) -> Result<(), SceneIoError> {
    let ty = &registration.ty;
    let saved = reflect::take_version(&mut value)
        .map_err(|located| reflect_error(name, ty, 0, located))?;
    let error = |located| reflect_error(name, ty, saved, located);
    reflect::migrate(&mut value, saved, &ty.migrations).map_err(error)?;
    if ty.references {
        reflect::from_scene_form(&ty.info, &mut value, world, ids)
            .map_err(error)?;
    } else {
        reflect::walk(&ty.info, &mut value, &mut |_, _| Ok(()))
            .map_err(error)?;
    }
    (registration.from_value)(world, entity, value)
        .map_err(|error| serde_error(name, &error))
}

fn component_text<T>(
    world: &World,
    entity: Entity,
) -> Option<serde_json::Result<String>>
where
    T: Component + Serialize,
{
    world.get::<T>(entity).map(serde_json::to_string)
}

fn component_value<T>(
    world: &World,
    entity: Entity,
) -> Option<serde_json::Result<Value>>
where
    T: Component + Serialize,
{
    world.get::<T>(entity).map(serde_json::to_value)
}

fn insert_component_value<T>(
    world: &mut World,
    entity: Entity,
    value: Value,
) -> serde_json::Result<()>
where
    T: Component + DeserializeOwned,
{
    let component = serde_json::from_value::<T>(value)?;
    world.entity_mut(entity).insert(component);
    Ok(())
}

fn insert_default_component<T>(world: &mut World, entity: Entity)
where
    T: Component + Default,
{
    world.entity_mut(entity).insert(T::default());
}

fn remove_component<T>(world: &mut World, entity: Entity)
where
    T: Component,
{
    world.entity_mut(entity).remove::<T>();
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SceneLoadMode {
    #[default]
    Replace,
    Additive,
}

pub fn scene_document(
    world: &mut World,
    name: impl Into<String>,
) -> Result<SceneDocument, SceneIoError> {
    scene_document_with(world, name, false, None)
}

/// Like [`scene_document`], but an entity whose mesh or textures were made in
/// code (so have no asset path) is kept without its `mesh_renderer` instead of
/// failing the whole document. For reading state, never for saving.
pub fn scene_document_lenient(
    world: &mut World,
    name: impl Into<String>,
) -> Result<SceneDocument, SceneIoError> {
    scene_document_with(world, name, true, None)
}

/// One entity as [`scene_document_lenient`] would write it, without
/// capturing the rest of the scene. `None` when it has no `SceneId`.
pub fn scene_entity_lenient(
    world: &mut World,
    entity: Entity,
) -> Result<Option<SceneEntity>, SceneIoError> {
    Ok(scene_document_with(world, "", true, Some(entity))?
        .entities
        .pop())
}

fn scene_document_with(
    world: &mut World,
    name: impl Into<String>,
    lenient: bool,
    only: Option<Entity>,
) -> Result<SceneDocument, SceneIoError> {
    let registrations = world
        .resource::<SceneComponentRegistry>()
        .registrations
        .clone();
    let directional_lights = {
        let mut query = world.query::<(Entity, &DirectionalLight)>();
        query
            .iter(world)
            .map(|(entity, light)| (entity, *light))
            .collect::<HashMap<_, _>>()
    };
    let point_lights = {
        let mut query = world.query::<(Entity, &PointLight)>();
        query
            .iter(world)
            .map(|(entity, light)| (entity, *light))
            .collect::<HashMap<_, _>>()
    };
    let spot_lights = {
        let mut query = world.query::<(Entity, &SpotLight)>();
        query
            .iter(world)
            .map(|(entity, light)| (entity, *light))
            .collect::<HashMap<_, _>>()
    };
    let mut query = world.query::<(
        Entity,
        &SceneId,
        Option<&Parent>,
        Option<&Name>,
        Option<&ObjectClasses>,
        Option<&Transform>,
        Option<&MeshRenderer>,
        Option<&Camera>,
        Option<&Visibility>,
        Option<&PhysicsBody>,
        Option<&RigidBody>,
        Option<&Collider>,
        Option<&CollisionLayers>,
        Option<&GpuPhysicsWatch>,
    )>();
    let mut raw = query
        .iter(world)
        .filter(|item| only.is_none_or(|only| item.0 == only))
        .map(
            |(
                entity,
                id,
                parent,
                name,
                classes,
                transform,
                renderer,
                camera,
                visibility,
                physics_body,
                rigid_body,
                collider,
                collision_layers,
                gpu_physics_watch,
            )| {
                (
                    entity,
                    *id,
                    parent.copied(),
                    name.cloned(),
                    classes.cloned(),
                    transform.copied(),
                    renderer.copied(),
                    camera.copied(),
                    visibility.copied(),
                    physics_body.cloned(),
                    rigid_body.copied(),
                    collider.copied(),
                    collision_layers.copied(),
                    gpu_physics_watch.cloned(),
                )
            },
        )
        .collect::<Vec<_>>();
    if let Some(rest) = world.get_resource::<super::PreviewRestPose>() {
        for (_, id, _, _, _, transform, _, _, visibility, ..) in &mut raw {
            if let Some((_, rest_transform, rest_visibility)) =
                rest.0.iter().find(|rest| rest.0 == *id)
            {
                *transform = Some(*rest_transform);
                *visibility = *rest_visibility;
            }
        }
    }
    let assets = world
        .get_resource::<AssetServer>()
        .ok_or(SceneIoError::MissingAssetServer)?;
    let mut entities = Vec::with_capacity(raw.len());
    for (
        entity,
        id,
        parent,
        name,
        classes,
        transform,
        renderer,
        camera,
        visibility,
        physics_body,
        rigid_body,
        collider,
        collision_layers,
        gpu_physics_watch,
    ) in raw
    {
        let parent = parent
            .map(|parent| {
                world
                    .get::<SceneId>(parent.0)
                    .map(|parent| parent.0)
                    .ok_or(SceneIoError::MissingParent(id.0))
            })
            .transpose()?;
        let mesh_renderer = match renderer
            .map(|renderer| scene_renderer(renderer, assets))
            .transpose()
        {
            Ok(renderer) => renderer,
            Err(
                SceneIoError::UnsavedMesh(_) | SceneIoError::UnsavedTexture(_),
            ) if lenient => None,
            Err(error) => return Err(error),
        };
        let mut components = world
            .get::<UnregisteredComponents>(entity)
            .map(|kept| kept.0.clone())
            .unwrap_or_default();
        for (component_name, registration) in &registrations {
            if let Some(value) =
                capture_component(world, entity, component_name, registration)?
            {
                components.insert(component_name.clone(), value);
            }
        }
        if let Some(member) = world.get::<super::InstanceMember>(entity) {
            components.insert(
                super::INSTANCE_MEMBER_KEY.to_owned(),
                super::scene_instance::member_marker(*member),
            );
        }
        if let Some(expanded) = world.get::<super::InstanceExpanded>(entity) {
            components.insert(
                super::scene_instance::INSTANCE_EXPANDED_KEY.to_owned(),
                serde_json::to_string(expanded)?,
            );
        }
        entities.push(SceneEntity {
            id: id.0,
            parent,
            name: name.map(|name| name.0),
            classes: classes.unwrap_or_default().names,
            transform: transform.map(Into::into),
            mesh_renderer,
            camera: camera.map(scene_camera),
            visible: visibility.map(|visibility| visibility.visible),
            physics_body,
            rigid_body,
            collider,
            collision_layers,
            gpu_physics_watch,
            components,
            directional_light: directional_lights.get(&entity).copied(),
            point_light: point_lights.get(&entity).copied(),
            spot_light: spot_lights.get(&entity).copied(),
        });
    }
    entities.sort_by_key(|entity| entity.id);
    let render = world
        .get_resource::<RenderSettings>()
        .map(|settings| SceneRenderSettings {
            quality: settings.quality,
            culling: settings.culling,
        })
        .unwrap_or_default();
    let document = SceneDocument {
        format_version: SCENE_FORMAT_VERSION,
        name: name.into(),
        entities,
        render,
        simulation: SceneSimulationSettings {
            determinism: world
                .get_resource::<DeterminismMode>()
                .copied()
                .unwrap_or_default(),
        },
    };
    // One entity's parent is not in its own document.
    if only.is_none() {
        validate_scene_structure(&document)?;
    }
    Ok(document)
}

pub fn save_scene(
    world: &mut World,
    path: impl AsRef<Path>,
    name: impl Into<String>,
) -> Result<(), SceneIoError> {
    let mut document = scene_document(world, name)?;
    // Instance members come back from their source file on the next load,
    // with the overrides this writes.
    super::scene_instance::fold_instance_members(&mut document)?;
    let path = path.as_ref();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
        relativize_scene_assets(&mut document, parent)?;
    }
    write_atomic(path, &serde_json::to_vec_pretty(&document)?)?;
    Ok(())
}

/// Writes a variant of the scene at `base` to `path`, like Godot's inherited
/// scenes. The variant holds one object that instances `base`, so every
/// object and field comes from `base` until the variant overrides it, and
/// later edits to `base` reach every field the variant did not change.
/// Fails without writing when `base` cannot load or already contains `path`.
pub fn save_scene_variant(
    base: impl AsRef<Path>,
    path: impl AsRef<Path>,
) -> Result<(), SceneIoError> {
    let (base, path) = (absolute_path(base.as_ref())?, path.as_ref());
    let stem = |path: &Path| {
        path.file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or("Scene")
            .to_owned()
    };
    let root: SceneEntity = serde_json::from_value(serde_json::json!({
        "id": Uuid::new_v4(),
        "name": stem(&base),
        "transform": SceneTransform::from(Transform::default()),
        "components": {
            super::SCENE_INSTANCE_COMPONENT:
                serde_json::to_string(&super::SceneInstance {
                    source: base.clone(),
                })?,
        },
    }))?;
    let mut document = SceneDocument {
        format_version: SCENE_FORMAT_VERSION,
        name: stem(path),
        entities: vec![root],
        render: Default::default(),
        simulation: Default::default(),
    };
    // Expanding once proves `base` loads and does not contain the variant.
    super::scene_instance::expand_instances(
        &document,
        &mut vec![absolute_path(path)?],
    )?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
        relativize_scene_assets(&mut document, parent)?;
    }
    write_atomic(path, &serde_json::to_vec_pretty(&document)?)?;
    Ok(())
}

/// Writes through a synced temporary file and renames it over `path`, so a
/// crash mid-write never leaves a truncated file behind. Each call has its
/// own temporary name, so parallel runs that cook the same scene do not
/// rename one another's file away. Refused while another agent leases
/// `path` (see [`super::lease`]); recorded in the operation journal while
/// a command runs (see [`super::journal`]).
pub fn write_atomic(
    path: impl AsRef<Path>,
    bytes: &[u8],
) -> std::io::Result<()> {
    let path = path.as_ref();
    if path.is_dir() {
        return write_scene_folder(path, bytes);
    }
    super::lease::check_write(path)?;
    let before = super::journal::active()
        .then(|| std::fs::read(path).ok())
        .flatten();
    write_atomic_unchecked(path, bytes)?;
    super::journal::record(path, before.as_deref(), Some(bytes))
}

/// Bytes of the scene at `path`. A scene in folder form (a `.rscene`
/// directory from `rusting scene split`) comes back joined, as the pretty
/// JSON a single-file scene holds, so revisions and patches work the same
/// on both forms.
pub fn read_scene_bytes(
    path: impl AsRef<Path>,
) -> Result<Vec<u8>, SceneIoError> {
    let path = path.as_ref();
    if !path.is_dir() {
        return Ok(std::fs::read(path)?);
    }
    let mut document: Value =
        serde_json::from_slice(&std::fs::read(path.join("scene.json"))?)?;
    let ids: Vec<Uuid> = serde_json::from_value(
        document
            .get_mut("entities")
            .map(Value::take)
            .unwrap_or_default(),
    )?;
    let entities = ids
        .iter()
        .map(|id| {
            Ok(serde_json::from_slice(&std::fs::read(entity_file(
                path, *id,
            ))?)?)
        })
        .collect::<Result<Vec<Value>, SceneIoError>>()?;
    document["entities"] = Value::Array(entities);
    let document = parse_scene_document(&serde_json::to_vec(&document)?)?;
    Ok(serde_json::to_vec_pretty(&document)?)
}

fn entity_file(folder: &Path, id: Uuid) -> PathBuf {
    folder.join("entities").join(format!("{id}.json"))
}

/// Splits a scene's bytes into folder form under `folder`: `scene.json`
/// holds everything but the entities, in place of which it lists their
/// IDs in order, and `entities/<id>.json` holds each entity. Only files
/// whose content changed are written, so one entity's edit touches one
/// file.
fn write_scene_folder(folder: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let invalid = |message: &str| {
        std::io::Error::new(std::io::ErrorKind::InvalidData, message)
    };
    let mut document: Value = serde_json::from_slice(bytes)?;
    let Some(Value::Array(entities)) =
        document.get_mut("entities").map(Value::take)
    else {
        return Err(invalid("a scene needs an `entities` array"));
    };
    let write_changed = |path: &Path, bytes: &[u8]| {
        if std::fs::read(path).ok().as_deref() == Some(bytes) {
            return Ok(());
        }
        write_atomic(path, bytes)
    };
    let entities_folder = folder.join("entities");
    // A linked `entities` would let the cleanup below delete files elsewhere.
    if std::fs::symlink_metadata(&entities_folder)
        .is_ok_and(|meta| meta.file_type().is_symlink())
    {
        return Err(invalid("a scene's `entities` folder may not be a link"));
    }
    std::fs::create_dir_all(&entities_folder)?;
    let mut ids = Vec::new();
    for entity in &entities {
        let id = entity
            .get("id")
            .and_then(Value::as_str)
            .and_then(|id| Uuid::parse_str(id).ok())
            .ok_or_else(|| invalid("every entity needs a UUID `id`"))?;
        write_changed(
            &entity_file(folder, id),
            &serde_json::to_vec_pretty(entity)?,
        )?;
        ids.push(id);
    }
    document["entities"] = serde_json::to_value(&ids)?;
    write_changed(
        &folder.join("scene.json"),
        &serde_json::to_vec_pretty(&document)?,
    )?;
    for entry in std::fs::read_dir(&entities_folder)? {
        let file = entry?.path();
        // Only entity files this form writes: `<uuid>.json`.
        let entity = file.extension().is_some_and(|ext| ext == "json")
            && file
                .file_stem()
                .and_then(|stem| stem.to_str())
                .is_some_and(|stem| Uuid::parse_str(stem).is_ok());
        let gone =
            entity && !ids.iter().any(|id| entity_file(folder, *id) == file);
        if gone {
            super::lease::check_write(&file)?;
            let before = super::journal::active()
                .then(|| std::fs::read(&file).ok())
                .flatten();
            std::fs::remove_file(&file)?;
            super::journal::record(&file, before.as_deref(), None)?;
        }
    }
    Ok(())
}

pub(crate) fn write_atomic_unchecked(
    path: impl AsRef<Path>,
    bytes: &[u8],
) -> std::io::Result<()> {
    use std::io::Write;
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let path = path.as_ref();
    let mut temporary = path.as_os_str().to_owned();
    let call = NEXT.fetch_add(1, Ordering::Relaxed);
    temporary.push(format!(".{}.{call}.tmp", std::process::id()));
    let temporary = std::path::PathBuf::from(temporary);
    let written = (|| {
        let mut file = std::fs::File::create(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&temporary, path)
    })();
    if written.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    written
}

pub fn load_scene(
    world: &mut World,
    path: impl AsRef<Path>,
    mode: SceneLoadMode,
) -> Result<usize, SceneIoError> {
    let path = path.as_ref();
    let bytes = read_scene_bytes(path)?;
    let mut document = decode_scene(&bytes)?;
    if let Some(parent) = path.parent() {
        absolutize_scene_assets(&mut document, parent)?;
    }
    load_scene_document(world, &document, mode)
}

/// Despawns every entity spawned by a loaded scene (anything carrying
/// [`SceneId`]), leaving non-scene resources and entities untouched.
///
/// Returns the number of entities removed.
pub fn unload_scene(world: &mut World) -> usize {
    let mut query =
        world.query_filtered::<Entity, bevy_ecs::query::With<SceneId>>();
    let entities = query.iter(world).collect::<Vec<_>>();
    let count = entities.len();
    for entity in entities {
        world.despawn(entity);
    }
    count
}

/// The `determinism` of the nearest `project.json` above `scene`, which
/// overrides the value in the text scene.
fn project_determinism(scene: &Path) -> Option<DeterminismMode> {
    let manifest = scene
        .ancestors()
        .skip(1)
        .map(|folder| folder.join("project.json"))
        .find(|path| path.is_file())?;
    let text = std::fs::read_to_string(manifest).ok()?;
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;
    serde_json::from_value(value.get("determinism")?.clone()).ok()
}

pub fn cook_scene(
    source: impl AsRef<Path>,
    destination: impl AsRef<Path>,
) -> Result<(), SceneIoError> {
    let source = source.as_ref();
    let destination = destination.as_ref();
    let mut document: SceneDocument =
        serde_json::from_slice(&read_scene_bytes(source)?)?;
    migrate_scene_document(&mut document)?;
    validate_version(&document)?;
    validate_scene_structure(&document)?;
    if let Some(parent) = source.parent() {
        absolutize_scene_assets(&mut document, parent)?;
    }
    // A cooked scene carries its instance members, so a game ships without
    // the source scenes.
    let mut document = super::scene_instance::expand_instances(
        &document,
        &mut vec![absolute_path(source)?],
    )?
    .into_owned();
    validate_scene_structure(&document)?;
    if let Some(determinism) = project_determinism(source) {
        document.simulation.determinism = determinism;
    }
    if let Some(parent) = destination.parent() {
        relativize_scene_assets(&mut document, parent)?;
    }
    let mut bytes = COMPILED_MAGIC.to_vec();
    bytes.extend(bincode::serialize(&document)?);
    if let Some(parent) = destination.parent() {
        std::fs::create_dir_all(parent)?;
    }
    write_atomic(destination, &bytes)?;
    Ok(())
}

/// Applies one path conversion to every mesh and texture in a scene,
/// including reflected handles in components.
fn map_scene_asset_paths(
    document: &mut SceneDocument,
    mut convert: impl FnMut(&Path) -> Result<PathBuf, SceneIoError>,
) -> Result<(), SceneIoError> {
    for entity in &mut document.entities {
        super::scene_instance::map_instance_source(entity, &mut convert)?;
        super::scene_instance::map_override_paths(entity, &mut convert)?;
        for serialized in entity.components.values_mut() {
            // Only reflected handles write this key; skip parsing the rest.
            if !serialized.contains(reflect::ASSET_KEY) {
                continue;
            }
            let Ok(mut value) = serde_json::from_str::<Value>(serialized)
            else {
                continue;
            };
            reflect::map_asset_paths(&mut value, &mut convert)?;
            *serialized = value.to_string();
        }
        let Some(renderer) = &mut entity.mesh_renderer else {
            continue;
        };
        if let SceneMesh::AssetPath(path) = &mut renderer.mesh {
            *path = convert(path)?;
        }
        let SceneMaterial::Inline(material) = &mut renderer.material else {
            continue;
        };
        for current in [
            &mut material.base_color_texture,
            &mut material.normal_texture,
            &mut material.metallic_roughness_texture,
            &mut material.occlusion_texture,
            &mut material.emissive_texture,
        ]
        .into_iter()
        .flatten()
        {
            *current = convert(current)?;
        }
    }
    Ok(())
}

/// Converts scene-relative asset paths to normalized absolute paths.
pub(super) fn absolutize_scene_assets(
    document: &mut SceneDocument,
    scene_folder: &Path,
) -> Result<(), SceneIoError> {
    let base = absolute_path(scene_folder)?;
    map_scene_asset_paths(document, |path| {
        if path.is_absolute() {
            Ok(path.to_path_buf())
        } else {
            Ok(normalize_lexical(&base.join(path)))
        }
    })
}

/// Converts absolute paths to paths relative to the scene being written.
pub fn relativize_scene_assets(
    document: &mut SceneDocument,
    scene_folder: &Path,
) -> Result<(), SceneIoError> {
    let base = absolute_path(scene_folder)?;
    map_scene_asset_paths(document, |path| {
        let target = absolute_path(path)?;
        Ok(path_relative_to(&base, &target).unwrap_or(target))
    })
}

fn absolute_path(path: &Path) -> Result<PathBuf, SceneIoError> {
    if let Ok(path) = path.canonicalize() {
        return Ok(path);
    }
    if path.is_absolute() {
        Ok(normalize_lexical(path))
    } else {
        Ok(normalize_lexical(&std::env::current_dir()?.join(path)))
    }
}

/// Removes `.` and `..` without requiring the final file to exist.
pub(crate) fn normalize_lexical(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                normalized.pop();
            }
            component => normalized.push(component.as_os_str()),
        }
    }
    normalized
}

/// Builds a relative path when both paths use the same platform root.
pub(crate) fn path_relative_to(base: &Path, target: &Path) -> Option<PathBuf> {
    let base = base.components().collect::<Vec<_>>();
    let target = target.components().collect::<Vec<_>>();
    let common = base
        .iter()
        .zip(&target)
        .take_while(|(left, right)| left == right)
        .count();
    if common == 0 {
        return None;
    }
    let mut relative = PathBuf::new();
    for component in &base[common..] {
        if matches!(component, std::path::Component::Normal(_)) {
            relative.push("..");
        }
    }
    for component in &target[common..] {
        relative.push(component.as_os_str());
    }
    Some(relative)
}

pub fn load_scene_document(
    world: &mut World,
    document: &SceneDocument,
    mode: SceneLoadMode,
) -> Result<usize, SceneIoError> {
    // Migration only raises `format_version`, which nothing below reads, so
    // an older document is accepted as-is instead of being cloned first.
    if document.format_version > SCENE_FORMAT_VERSION {
        return Err(SceneIoError::UnsupportedVersion(document.format_version));
    }
    let document =
        &*super::scene_instance::expand_instances(document, &mut Vec::new())?;
    static WITHOUT: std::sync::OnceLock<Vec<String>> =
        std::sync::OnceLock::new();
    let without = WITHOUT.get_or_init(|| {
        std::env::var(crate::project::WITHOUT_ENV)
            .map(|names| names.split(',').map(str::to_owned).collect())
            .unwrap_or_default()
    });
    let stripped;
    let document = if without.is_empty() {
        document
    } else {
        stripped = without_components(document, without);
        &stripped
    };
    validate_scene_structure(document)?;
    let registry = world.resource::<SceneComponentRegistry>();
    let registrations = registry.registrations.clone();
    let keep_unregistered = registry.keep_unregistered;
    for entity in &document.entities {
        for name in entity.components.keys() {
            if !keep_unregistered
                && !registrations.contains_key(name)
                && !super::scene_instance::is_instance_key(name)
            {
                return Err(SceneIoError::UnknownComponent(name.clone()));
            }
        }
    }

    // Resolve every referenced asset before replacing the current world. A
    // missing asset must not erase the scene that is already open.
    let prepared = prepare_assets(world, document)?;
    preload_component_assets(world, document, &registrations)?;

    // The old scene stays until the new one is complete, so a failing parent
    // link or custom component leaves the open scene untouched.
    let replaced = if mode == SceneLoadMode::Replace {
        let mut query =
            world.query_filtered::<Entity, bevy_ecs::query::With<SceneId>>();
        query.iter(world).collect::<Vec<_>>()
    } else {
        Vec::new()
    };

    // Number the objects by id, not by file order: a captured scene lists
    // them by id, so a reload solves physics in the order the first load did.
    let base = if mode == SceneLoadMode::Replace {
        0
    } else {
        world
            .get_resource::<super::NextSpawnOrder>()
            .map_or(0, |next| next.0)
    };
    let mut ids: Vec<_> =
        document.entities.iter().map(|entity| entity.id).collect();
    ids.sort_unstable();
    world.insert_resource(super::NextSpawnOrder(base + ids.len() as u64));
    let mut spawned = HashMap::new();
    for (scene_entity, renderer) in document.entities.iter().zip(prepared) {
        let rank = ids.binary_search(&scene_entity.id).unwrap_or_default();
        let mut entity = world.spawn((
            SceneId(scene_entity.id),
            super::SpawnOrder(base + rank as u64),
        ));
        if let Some(name) = &scene_entity.name {
            entity.insert(Name(name.clone()));
        }
        if !scene_entity.classes.is_empty() {
            entity.insert(ObjectClasses::new(scene_entity.classes.clone()));
        }
        if let Some(transform) = scene_entity.transform {
            entity.insert(Transform::from(transform));
        }
        if let Some(renderer) = renderer {
            entity.insert(renderer);
        }
        if let Some(camera) = scene_entity.camera {
            entity.insert(runtime_camera(camera));
        }
        if let Some(visible) = scene_entity.visible {
            entity.insert(Visibility { visible });
        }
        if let Some(physics_body) = &scene_entity.physics_body {
            entity.insert(physics_body.clone());
        }
        if let Some(rigid_body) = scene_entity.rigid_body {
            entity.insert(rigid_body);
        }
        if let Some(collider) = scene_entity.collider {
            entity.insert(collider);
        }
        if let Some(collision_layers) = scene_entity.collision_layers {
            entity.insert(collision_layers);
        }
        if let Some(gpu_physics_watch) = &scene_entity.gpu_physics_watch {
            entity.insert(gpu_physics_watch.clone());
        }
        if let Some(light) = scene_entity.directional_light {
            entity.insert(light);
        }
        if let Some(light) = scene_entity.point_light {
            entity.insert(light);
        }
        if let Some(light) = scene_entity.spot_light {
            entity.insert(light);
        }
        spawned.insert(scene_entity.id, entity.id());
    }

    let linked = (|| {
        for scene_entity in &document.entities {
            let entity = spawned[&scene_entity.id];
            if let Some(parent) = scene_entity.parent {
                let parent = spawned
                    .get(&parent)
                    .copied()
                    .ok_or(SceneIoError::MissingParent(parent))?;
                super::hierarchy::set_parent(world, entity, parent)?;
            }
            let mut unregistered = BTreeMap::new();
            for (name, serialized) in &scene_entity.components {
                if name == super::INSTANCE_MEMBER_KEY {
                    let member =
                        super::scene_instance::parse_member_marker(serialized)?;
                    world.entity_mut(entity).insert(member);
                    continue;
                }
                if name == super::scene_instance::INSTANCE_EXPANDED_KEY {
                    let expanded =
                        super::scene_instance::parse_expanded(serialized)?;
                    world.entity_mut(entity).insert(expanded);
                    continue;
                }
                match registrations.get(name) {
                    Some(registration) => restore_component(
                        world,
                        entity,
                        name,
                        registration,
                        serialized,
                        Some(&spawned),
                    )
                    .map_err(|error| error.at_object(scene_entity.id))?,
                    None => {
                        unregistered.insert(name.clone(), serialized.clone());
                    }
                }
            }
            if !unregistered.is_empty() {
                world
                    .entity_mut(entity)
                    .insert(UnregisteredComponents(unregistered));
            }
        }
        // Tools load scenes without the game's handlers.
        if !keep_unregistered {
            super::signals::validate_connections(world, &spawned)?;
        }
        Ok(())
    })();
    let discarded = if linked.is_ok() {
        if mode == SceneLoadMode::Replace {
            if let Some(mut settings) =
                world.get_resource_mut::<RenderSettings>()
            {
                settings.quality = document.render.quality;
                settings.culling = document.render.culling;
            }
            world.insert_resource(document.simulation.determinism);
        }
        replaced
    } else {
        spawned.into_values().collect()
    };
    for entity in discarded {
        world.despawn(entity);
    }
    linked.map(|()| document.entities.len())
}

/// `document` with these components left out of every entity: a built-in
/// section such as `collider`, or a registered component by name.
pub fn without_components(
    document: &SceneDocument,
    names: &[String],
) -> SceneDocument {
    let mut document = document.clone();
    for entity in &mut document.entities {
        let mut value = serde_json::to_value(&*entity).unwrap_or_default();
        for name in names {
            if !matches!(name.as_str(), "id" | "parent" | "name") {
                if let Some(object) = value.as_object_mut() {
                    object.remove(name);
                }
            }
        }
        if let Ok(mut stripped) = serde_json::from_value::<SceneEntity>(value) {
            stripped.components.retain(|name, _| !names.contains(name));
            *entity = stripped;
        }
    }
    document
}

/// Checks every stable ID and parent chain before replacing the open scene.
pub fn validate_scene_structure(
    document: &SceneDocument,
) -> Result<(), SceneIoError> {
    let mut parents = HashMap::new();
    let mut names = HashSet::new();
    for entity in &document.entities {
        if parents.insert(entity.id, entity.parent).is_some() {
            return Err(SceneIoError::DuplicateEntity(entity.id));
        }
        // Two instances of one scene have members with the same names.
        if let Some(name) = entity
            .name
            .as_deref()
            .filter(|_| !super::scene_instance::is_member(entity))
        {
            if !names.insert(name) {
                return Err(SceneIoError::DuplicateName(name.to_owned()));
            }
        }
    }
    // Objects added under an instance's objects name a parent that only
    // exists once the instance is expanded; loading checks them again then.
    let unexpanded = document
        .entities
        .iter()
        .any(super::scene_instance::is_unexpanded_root);
    for entity in &document.entities {
        let mut ancestor = entity.parent;
        let mut visited = HashSet::new();
        while let Some(id) = ancestor {
            if !visited.insert(id) || id == entity.id {
                return Err(SceneIoError::HierarchyCycle(entity.id));
            }
            ancestor = match parents.get(&id) {
                Some(parent) => *parent,
                None if unexpanded => None,
                None => return Err(SceneIoError::MissingParent(id)),
            };
        }
    }
    Ok(())
}

/// Index in `document.entities` of the object a structure or component
/// value error is about, so tools can point at it.
#[must_use]
pub fn error_object(
    document: &SceneDocument,
    error: &SceneIoError,
) -> Option<usize> {
    let entities = &document.entities;
    let last_with_id =
        |id: Uuid| entities.iter().rposition(|entity| entity.id == id);
    match error {
        SceneIoError::DuplicateEntity(id)
        | SceneIoError::HierarchyCycle(id) => last_with_id(*id),
        SceneIoError::DuplicateName(name) => entities
            .iter()
            .rposition(|entity| entity.name.as_deref() == Some(name)),
        SceneIoError::MissingParent(parent) => entities
            .iter()
            .position(|entity| entity.parent == Some(*parent)),
        SceneIoError::Reflection(error) => error.object.and_then(last_with_id),
        _ => None,
    }
}

fn decode_scene(bytes: &[u8]) -> Result<SceneDocument, SceneIoError> {
    let mut document = if let Some(compiled) =
        bytes.strip_prefix(COMPILED_MAGIC)
    {
        // `format_version` is the first field of every cooked shape. Version 4
        // is dispatched on it because a v4 material can also parse as v5.
        let version = crate::assets::deserialize_bounded::<u32>(compiled)?;
        if version == 4 {
            crate::assets::deserialize_bounded::<LegacySceneDocumentV4>(
                compiled,
            )?
            .into()
        } else if version == 9 {
            crate::assets::deserialize_bounded::<
                LegacySceneDocumentV8<SceneMaterialDataV9>,
            >(compiled)?
            .into()
        } else if version == 8 {
            crate::assets::deserialize_bounded::<LegacySceneDocumentV8>(
                compiled,
            )?
            .into()
        } else if version == 7 {
            crate::assets::deserialize_bounded::<LegacySceneDocumentV7>(
                compiled,
            )?
            .into()
        } else if version == 6 {
            // A v6 scene would also parse as v5 and drop its render settings.
            crate::assets::deserialize_bounded::<LegacySceneDocumentV6>(
                compiled,
            )?
            .into()
        } else {
            match crate::assets::deserialize_bounded(compiled) {
                Ok(document) => document,
                Err(current_error) => {
                    if let Ok(legacy) = crate::assets::deserialize_bounded::<
                        LegacySceneDocumentV5,
                    >(compiled)
                    {
                        legacy.into()
                    } else if let Ok(legacy) =
                        crate::assets::deserialize_bounded::<
                            LegacySceneDocumentV3,
                        >(compiled)
                    {
                        legacy.into()
                    } else if let Ok(legacy) =
                        crate::assets::deserialize_bounded::<
                            LegacySceneDocumentV2,
                        >(compiled)
                    {
                        legacy.into()
                    } else {
                        let legacy = crate::assets::deserialize_bounded::<
                            LegacySceneDocumentV1,
                        >(compiled)
                        .map_err(|_| current_error)?;
                        legacy.into()
                    }
                }
            }
        }
    } else {
        serde_json::from_slice(bytes)?
    };
    migrate_scene_document(&mut document)?;
    validate_version(&document)?;
    Ok(document)
}

/// Reads a text or cooked scene with the runtime's migrations and structural checks.
/// Asset paths remain as authored so callers can report useful source locations.
pub fn read_scene_document(
    path: impl AsRef<Path>,
) -> Result<SceneDocument, SceneIoError> {
    parse_scene_document(&read_scene_bytes(path)?)
}

/// [`read_scene_document`] for bytes already in memory.
pub fn parse_scene_document(
    bytes: &[u8],
) -> Result<SceneDocument, SceneIoError> {
    let document = decode_scene(bytes)?;
    validate_scene_structure(&document)?;
    Ok(document)
}

/// Content hash of a scene file's bytes. Scene patches and the editor
/// compare it to detect edits made by someone else since they last read it.
#[must_use]
pub fn scene_revision(bytes: &[u8]) -> String {
    // FNV-1a 64: stable across platforms and releases, unlike `DefaultHasher`.
    let hash = bytes.iter().fold(0xcbf2_9ce4_8422_2325_u64, |hash, &byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3)
    });
    format!("{hash:016x}")
}

/// Upgrades old text-scene shapes before normal validation and loading.
fn migrate_scene_document(
    document: &mut SceneDocument,
) -> Result<(), SceneIoError> {
    match document.format_version {
        0..=9 => {
            // Versions before programmable GPU watches, render settings, and
            // simulation settings
            // use safe defaults for the fields that were added later.
            document.format_version = SCENE_FORMAT_VERSION;
            Ok(())
        }
        SCENE_FORMAT_VERSION => Ok(()),
        version => Err(SceneIoError::UnsupportedVersion(version)),
    }
}

fn validate_version(document: &SceneDocument) -> Result<(), SceneIoError> {
    if document.format_version == SCENE_FORMAT_VERSION {
        Ok(())
    } else {
        Err(SceneIoError::UnsupportedVersion(document.format_version))
    }
}

fn scene_renderer(
    renderer: MeshRenderer,
    assets: &AssetServer,
) -> Result<SceneMeshRenderer, SceneIoError> {
    let mesh = if let Some(shape) = assets.primitive_for_handle(renderer.mesh) {
        match shape {
            PrimitiveShape::Cube => SceneMesh::BuiltinCube,
            PrimitiveShape::Sphere => SceneMesh::BuiltinSphere,
            shape => SceneMesh::BuiltinPrimitive(shape),
        }
    } else if let Some(path) = assets.meshes.path(renderer.mesh) {
        SceneMesh::AssetPath(path.to_path_buf())
    } else {
        return Err(SceneIoError::UnsavedMesh(renderer.mesh.key()));
    };
    let material = if renderer.material == assets.fallback_material {
        SceneMaterial::BuiltinError
    } else {
        let material = assets
            .materials
            .get(renderer.material)
            .ok_or(SceneIoError::MissingAssetServer)?;
        SceneMaterial::Inline(scene_material(material, assets)?)
    };
    Ok(SceneMeshRenderer {
        mesh,
        material,
        cast_shadows: renderer.cast_shadows,
        receive_shadows: renderer.receive_shadows,
    })
}

fn scene_material(
    material: &MaterialAsset,
    assets: &AssetServer,
) -> Result<SceneMaterialData, SceneIoError> {
    let texture_path = |texture: Option<Handle<TextureAsset>>| {
        // The built-in white texture has no file. The renderer binds white
        // for a missing map, so saving it as no texture looks the same.
        texture
            .filter(|handle| *handle != assets.fallback_texture)
            .map(|handle| {
                assets
                    .textures
                    .path(handle)
                    .map(Path::to_path_buf)
                    .ok_or(SceneIoError::UnsavedTexture(handle.key()))
            })
            .transpose()
    };
    Ok(SceneMaterialData {
        name: material.name.clone(),
        model: match material.model {
            MaterialModel::Pbr => SceneMaterialModel::Pbr,
            MaterialModel::Unlit => SceneMaterialModel::Unlit,
        },
        alpha_mode: match material.alpha_mode {
            AlphaMode::Opaque => SceneAlphaMode::Opaque,
            AlphaMode::Mask { cutoff } => SceneAlphaMode::Mask { cutoff },
            AlphaMode::Blend => SceneAlphaMode::Blend,
        },
        base_color: material.base_color,
        emissive: material.emissive,
        metallic: material.metallic,
        roughness: material.roughness,
        transmission: material.transmission,
        ior: material.ior,
        thickness: material.thickness,
        uv_scale: material.uv_scale,
        uv_offset: material.uv_offset,
        flat_shading: material.flat_shading,
        base_color_texture: texture_path(material.base_color_texture)?,
        normal_texture: texture_path(material.normal_texture)?,
        metallic_roughness_texture: texture_path(
            material.metallic_roughness_texture,
        )?,
        occlusion_texture: texture_path(material.occlusion_texture)?,
        emissive_texture: texture_path(material.emissive_texture)?,
    })
}

/// Loads every asset that registered components reference by handle, so a
/// missing one fails before the open scene is replaced. Restoring the
/// components later finds them already loaded.
fn preload_component_assets(
    world: &mut World,
    document: &SceneDocument,
    registrations: &BTreeMap<String, ComponentRegistration>,
) -> Result<(), SceneIoError> {
    for entity in &document.entities {
        for (name, serialized) in &entity.components {
            let Some(registration) = registrations.get(name) else {
                continue;
            };
            let ty = &registration.ty;
            if !ty.references {
                continue;
            }
            let mut value: Value = serde_json::from_str(serialized)
                .map_err(|error| serde_error(name, &error))?;
            let saved =
                reflect::take_version(&mut value).map_err(|located| {
                    reflect_error(name, ty, 0, located).at_object(entity.id)
                })?;
            let error = |located| {
                reflect_error(name, ty, saved, located).at_object(entity.id)
            };
            reflect::migrate(&mut value, saved, &ty.migrations)
                .map_err(error)?;
            let mut assets = world
                .get_resource_mut::<AssetServer>()
                .ok_or(SceneIoError::MissingAssetServer)?;
            reflect::walk(&ty.info, &mut value, &mut |info, value| {
                let TypeInfo::Handle(kind) = info else {
                    return Ok(());
                };
                // Data saved in the scene loads with its component.
                let Some(path) =
                    value.get(reflect::ASSET_KEY).and_then(Value::as_str)
                else {
                    return Ok(());
                };
                kind.load(&mut assets, Path::new(path))
                    .map(drop)
                    .map_err(|error| ReflectProblem::Asset(error.to_string()))
            })
            .map_err(error)?;
        }
    }
    Ok(())
}

fn prepare_assets(
    world: &mut World,
    document: &SceneDocument,
) -> Result<Vec<Option<MeshRenderer>>, SceneIoError> {
    let mut assets = world
        .get_resource_mut::<AssetServer>()
        .ok_or(SceneIoError::MissingAssetServer)?;
    document
        .entities
        .iter()
        .map(|entity| {
            let Some(renderer) = &entity.mesh_renderer else {
                return Ok(None);
            };
            let mesh = match &renderer.mesh {
                SceneMesh::BuiltinCube => assets.fallback_mesh,
                SceneMesh::BuiltinSphere => assets.builtin_sphere,
                SceneMesh::BuiltinPrimitive(shape) => {
                    assets.builtin_primitive(*shape)
                }
                SceneMesh::AssetPath(path) => {
                    if let Some(handle) = assets.meshes.handle_for_path(path) {
                        handle
                    } else {
                        assets.load_mesh(path).map_err(|error| {
                            SceneIoError::AssetLoad {
                                path: path.clone(),
                                message: error.to_string(),
                            }
                        })?
                    }
                }
            };
            let material = match &renderer.material {
                SceneMaterial::BuiltinError => assets.fallback_material,
                SceneMaterial::Inline(material) => {
                    let material = runtime_material(material, &mut assets)?;
                    let existing = assets.materials.iter().find_map(
                        |(handle, existing)| {
                            (*existing == material).then_some(handle)
                        },
                    );
                    existing
                        .unwrap_or_else(|| assets.materials.insert(material))
                }
            };
            Ok(Some(MeshRenderer {
                mesh,
                material,
                cast_shadows: renderer.cast_shadows,
                receive_shadows: renderer.receive_shadows,
            }))
        })
        .collect()
}

fn runtime_material(
    material: &SceneMaterialData,
    assets: &mut AssetServer,
) -> Result<MaterialAsset, SceneIoError> {
    fn texture(
        assets: &mut AssetServer,
        path: &Option<PathBuf>,
    ) -> Result<Option<Handle<TextureAsset>>, SceneIoError> {
        path.as_ref()
            .map(|path| {
                if let Some(handle) = assets.textures.handle_for_path(path) {
                    Ok(handle)
                } else {
                    assets.load_texture(path).map_err(|error| {
                        SceneIoError::AssetLoad {
                            path: path.clone(),
                            message: error.to_string(),
                        }
                    })
                }
            })
            .transpose()
    }
    Ok(MaterialAsset {
        name: material.name.clone(),
        model: match material.model {
            SceneMaterialModel::Pbr => MaterialModel::Pbr,
            SceneMaterialModel::Unlit => MaterialModel::Unlit,
        },
        alpha_mode: match material.alpha_mode {
            SceneAlphaMode::Opaque => AlphaMode::Opaque,
            SceneAlphaMode::Mask { cutoff } => AlphaMode::Mask { cutoff },
            SceneAlphaMode::Blend => AlphaMode::Blend,
        },
        base_color: material.base_color,
        emissive: material.emissive,
        metallic: material.metallic,
        roughness: material.roughness,
        transmission: material.transmission,
        ior: material.ior,
        thickness: material.thickness,
        uv_scale: material.uv_scale,
        uv_offset: material.uv_offset,
        flat_shading: material.flat_shading,
        splat: false,
        // ponytail: scene files do not save surface shaders yet; that
        // needs a cooked format version.
        shader: String::new(),
        base_color_texture: texture(assets, &material.base_color_texture)?,
        normal_texture: texture(assets, &material.normal_texture)?,
        metallic_roughness_texture: texture(
            assets,
            &material.metallic_roughness_texture,
        )?,
        occlusion_texture: texture(assets, &material.occlusion_texture)?,
        emissive_texture: texture(assets, &material.emissive_texture)?,
    })
}

fn scene_camera(camera: Camera) -> SceneCamera {
    SceneCamera {
        projection: match camera.projection {
            Projection::Perspective {
                vertical_fov_radians,
                near,
                far,
            } => SceneProjection::Perspective {
                vertical_fov_radians,
                near,
                far,
            },
            Projection::Orthographic {
                vertical_size,
                near,
                far,
            } => SceneProjection::Orthographic {
                vertical_size,
                near,
                far,
            },
        },
        active: camera.active,
        priority: camera.priority,
        viewport: camera.viewport,
    }
}

fn runtime_camera(camera: SceneCamera) -> Camera {
    Camera {
        projection: match camera.projection {
            SceneProjection::Perspective {
                vertical_fov_radians,
                near,
                far,
            } => Projection::Perspective {
                vertical_fov_radians,
                near,
                far,
            },
            SceneProjection::Orthographic {
                vertical_size,
                near,
                far,
            } => Projection::Orthographic {
                vertical_size,
                near,
                far,
            },
        },
        active: camera.active,
        priority: camera.priority,
        viewport: camera.viewport,
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn parallel_atomic_writes_of_one_file_all_succeed() {
        // Parallel `rusting run`s cook the same scene: a shared temporary
        // name let one writer rename the other's file away (os error 2).
        let folder = std::env::temp_dir()
            .join(format!("rusting-write-atomic-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&folder).unwrap();
        let path = folder.join("main.rscene");
        std::thread::scope(|scope| {
            for writer in 0..8u8 {
                let path = &path;
                scope.spawn(move || {
                    for _ in 0..50 {
                        super::write_atomic(path, &[writer; 4096]).unwrap();
                    }
                });
            }
        });
        let bytes = std::fs::read(&path).unwrap();
        assert!(bytes.len() == 4096 && bytes.iter().all(|b| *b == bytes[0]));
        let files = std::fs::read_dir(&folder).unwrap().count();
        assert_eq!(files, 1, "temporary files left behind");
        std::fs::remove_dir_all(&folder).unwrap();
    }

    #[test]
    fn without_components_leaves_out_sections_and_registered_components() {
        let document: SceneDocument = serde_json::from_value(serde_json::json!({
            "format_version": SCENE_FORMAT_VERSION,
            "name": "Main",
            "entities": [{
                "id": uuid::Uuid::new_v4(),
                "parent": null,
                "name": "Crate",
                "transform": null,
                "mesh_renderer": null,
                "camera": null,
                "visible": null,
                "collider": {"shape": {"Box": {"half_extents": [0.5, 0.5, 0.5]}},
                    "friction": 0.5, "restitution": 0.0, "sensor": false},
                "components": {"rusting.hud": "{}", "game.spin": "{}"},
            }],
        }))
        .unwrap();
        assert!(document.entities[0].collider.is_some());
        let names = ["collider".to_owned(), "game.spin".to_owned()];
        let stripped = without_components(&document, &names);
        let entity = &stripped.entities[0];
        assert!(entity.collider.is_none());
        assert_eq!(
            entity.components.keys().collect::<Vec<_>>(),
            ["rusting.hud"]
        );
        assert_eq!(entity.name.as_deref(), Some("Crate"));
        // `id`, `parent` and `name` are never left out.
        let kept = without_components(&document, &["name".to_owned()]);
        assert_eq!(kept.entities[0].name.as_deref(), Some("Crate"));
    }

    use std::time::Duration;

    use super::*;
    use crate::runtime::{
        App, GpuCondition, GpuEventPayload, GpuPhysicsRule, PhysicsSolver,
        RenderExtractPlugin, SimulationClass, ToneMapper,
    };
    use crate::MaterialAsset;

    #[derive(
        Component,
        Clone,
        Copy,
        Debug,
        Default,
        Serialize,
        Deserialize,
        PartialEq,
    )]
    struct GameplayTag {
        speed: f32,
    }

    crate::reflect! {
        struct GameplayTag {
            speed: f32,
        }
    }

    fn scene_app() -> App {
        let mut app = App::new();
        app.add_plugin(crate::AssetPlugin).unwrap();
        app.add_plugin(RenderExtractPlugin).unwrap();
        app.register_scene_component::<GameplayTag>("gameplay_tag")
            .unwrap();
        app
    }

    #[test]
    fn copies_of_the_builtin_error_material_still_save() {
        // Editing an object that uses the error material copies it, and the
        // copy keeps the built-in white texture, which has no file.
        let mut app = scene_app();
        let (mesh, material) = {
            let mut assets = app.world_mut().resource_mut::<AssetServer>();
            let copy = assets
                .materials
                .get(assets.fallback_material)
                .cloned()
                .unwrap();
            (assets.fallback_mesh, assets.materials.insert(copy))
        };
        app.spawn((
            SceneId::new(),
            MeshRenderer {
                mesh,
                material,
                cast_shadows: true,
                receive_shadows: true,
            },
        ));
        let document = scene_document(app.world_mut(), "Main").unwrap();
        let Some(SceneMeshRenderer {
            material: SceneMaterial::Inline(material),
            ..
        }) = &document.entities[0].mesh_renderer
        else {
            panic!("expected an inline material");
        };
        assert_eq!(material.base_color_texture, None);
    }

    #[test]
    fn tools_keep_unregistered_components_through_a_round_trip() {
        let mut app = scene_app();
        app.spawn((Name("Flag".into()), Transform::default()));
        let mut document = scene_document(app.world_mut(), "Main").unwrap();
        document.entities[0]
            .components
            .insert("game.spin".into(), r#"{"speed":2.0}"#.into());

        assert!(matches!(
            load_scene_document(
                app.world_mut(),
                &document,
                SceneLoadMode::Replace
            ),
            Err(SceneIoError::UnknownComponent(name)) if name == "game.spin"
        ));

        app.world_mut()
            .resource_mut::<SceneComponentRegistry>()
            .keep_unregistered();
        load_scene_document(app.world_mut(), &document, SceneLoadMode::Replace)
            .unwrap();
        let saved = scene_document(app.world_mut(), "Main").unwrap();
        assert_eq!(
            saved.entities[0].components,
            document.entities[0].components
        );
    }

    #[test]
    fn saving_is_stable_so_one_edit_changes_one_line() {
        let mut app = scene_app();
        for index in 0..6 {
            app.spawn((
                Name(format!("Box {index}")),
                Transform::new([index as f32, 0.5, 0.0]),
                GameplayTag { speed: 1.0 },
            ));
        }
        let text = |app: &mut crate::runtime::App| {
            let document = scene_document(app.world_mut(), "Test").unwrap();
            String::from_utf8(serde_json::to_vec_pretty(&document).unwrap())
                .unwrap()
        };
        let before = text(&mut app);
        let document = parse_scene_document(before.as_bytes()).unwrap();
        load_scene_document(app.world_mut(), &document, SceneLoadMode::Replace)
            .unwrap();
        app.update(Duration::ZERO).unwrap();
        assert_eq!(text(&mut app), before, "load and save changed the file");

        let mut query = app.world_mut().query::<&mut Name>();
        for mut name in query.iter_mut(app.world_mut()) {
            if name.0 == "Box 3" {
                name.0 = "Crate".into();
            }
        }
        let after = text(&mut app);
        let changed: Vec<_> = before
            .lines()
            .zip(after.lines())
            .filter(|(old, new)| old != new)
            .collect();
        assert_eq!(before.lines().count(), after.lines().count());
        assert_eq!(changed.len(), 1, "{changed:?}");
    }

    #[test]
    fn scene_round_trip_preserves_hierarchy_assets_and_registered_components() {
        let mut app = scene_app();
        let (mesh, material) = {
            let mut assets = app.world_mut().resource_mut::<AssetServer>();
            let material = assets.materials.insert(MaterialAsset {
                base_color: [0.2, 0.4, 0.8, 1.0],
                ..MaterialAsset::default()
            });
            (assets.fallback_mesh, material)
        };
        let parent = app.spawn((Name("Root".into()), Transform::default()));
        let child = app.spawn((
            Name("Cube".into()),
            Transform::new([1.0, 2.0, 3.0]),
            MeshRenderer {
                mesh,
                material,
                cast_shadows: true,
                receive_shadows: false,
            },
            GameplayTag { speed: 2.5 },
            PhysicsBody {
                simulation: SimulationClass::Gpu,
                solver: PhysicsSolver::Simplified,
                custom_shader: None,
            },
            RigidBody {
                mass: 12.0,
                ..RigidBody::default()
            },
            Collider {
                restitution: 0.75,
                ..Collider::default()
            },
            GpuPhysicsWatch {
                rules: vec![GpuPhysicsRule::new(
                    "cube_fell",
                    GpuCondition::position_y().less_than(-100.0),
                )
                .payload(GpuEventPayload::Position)],
            },
            ObjectClasses::new(["falling_cubes", "gravity"]),
        ));
        app.set_parent(child, parent).unwrap();
        let document = scene_document(app.world_mut(), "Test").unwrap();

        load_scene_document(app.world_mut(), &document, SceneLoadMode::Replace)
            .unwrap();
        app.update(Duration::ZERO).unwrap();

        let mut query = app.world_mut().query::<(
            &Name,
            &Transform,
            Option<&GameplayTag>,
            Option<&Parent>,
            Option<&PhysicsBody>,
            Option<&RigidBody>,
            Option<&Collider>,
            Option<&GpuPhysicsWatch>,
            Option<&ObjectClasses>,
        )>();
        let entities = query.iter(app.world()).collect::<Vec<_>>();
        assert_eq!(entities.len(), 2);
        let (
            _,
            transform,
            tag,
            parent,
            physics,
            rigid_body,
            collider,
            watch,
            classes,
        ) = entities.iter().find(|(name, ..)| name.0 == "Cube").unwrap();
        assert_eq!(transform.position, [1.0, 2.0, 3.0]);
        assert_eq!(tag.copied(), Some(GameplayTag { speed: 2.5 }));
        assert!(parent.is_some());
        assert_eq!(
            physics.map(|physics| physics.simulation),
            Some(SimulationClass::Gpu)
        );
        assert_eq!(rigid_body.map(|body| body.mass), Some(12.0));
        assert_eq!(collider.map(|collider| collider.restitution), Some(0.75));
        assert_eq!(
            watch
                .and_then(|watch| watch.rules.first())
                .map(|rule| rule.event.as_str()),
            Some("cube_fell")
        );
        assert_eq!(
            classes.map(|classes| classes.names.as_slice()),
            Some(["falling_cubes".to_owned(), "gravity".to_owned()].as_slice())
        );
    }

    #[test]
    fn failed_replace_keeps_the_open_scene() {
        let mut app = scene_app();
        app.spawn((Name("Open".into()), GameplayTag { speed: 1.0 }));
        let mut document = scene_document(app.world_mut(), "Test").unwrap();
        document.entities[0].name = Some("Broken".into());
        document.entities[0]
            .components
            .insert("gameplay_tag".into(), "not a component".into());

        assert!(load_scene_document(
            app.world_mut(),
            &document,
            SceneLoadMode::Replace
        )
        .is_err());

        let mut query = app.world_mut().query::<(&Name, &GameplayTag)>();
        let entities = query.iter(app.world()).collect::<Vec<_>>();
        assert_eq!(entities.len(), 1);
        assert_eq!(entities[0].0 .0, "Open");
    }

    #[test]
    fn builtin_primitive_round_trips_without_an_asset_file() {
        let mut app = scene_app();
        let (mesh, material) = {
            let assets = app.world().resource::<AssetServer>();
            (
                assets.builtin_primitive(PrimitiveShape::Torus),
                assets.fallback_material,
            )
        };
        app.spawn((
            Name("Torus".into()),
            Transform::default(),
            MeshRenderer {
                mesh,
                material,
                cast_shadows: true,
                receive_shadows: true,
            },
        ));
        let document = scene_document(app.world_mut(), "Primitives").unwrap();
        assert_eq!(
            document.entities[0].mesh_renderer.as_ref().unwrap().mesh,
            SceneMesh::BuiltinPrimitive(PrimitiveShape::Torus)
        );

        load_scene_document(app.world_mut(), &document, SceneLoadMode::Replace)
            .unwrap();
        let expected = app
            .world()
            .resource::<AssetServer>()
            .builtin_primitive(PrimitiveShape::Torus);
        let renderer = app
            .world_mut()
            .query::<&MeshRenderer>()
            .single(app.world())
            .unwrap();
        assert_eq!(renderer.mesh, expected);
    }

    #[test]
    fn unloading_a_scene_despawns_only_scene_entities() {
        let mut app = scene_app();
        app.spawn((Name("Cube".into()), Transform::default()));
        let document = scene_document(app.world_mut(), "Scene").unwrap();
        load_scene_document(app.world_mut(), &document, SceneLoadMode::Replace)
            .unwrap();
        let non_scene_entity =
            app.world_mut().spawn(Name("Global".into())).id();

        let removed = unload_scene(app.world_mut());

        assert_eq!(removed, 1);
        assert_eq!(
            app.world_mut()
                .query::<&SceneId>()
                .iter(app.world())
                .count(),
            0
        );
        assert!(app.world().get_entity(non_scene_entity).is_ok());
    }

    #[test]
    fn all_authored_light_types_round_trip_together() {
        let mut app = scene_app();
        app.spawn((
            Name("Sun".into()),
            Transform::default(),
            DirectionalLight {
                color: [1.0, 0.8, 0.6],
                illuminance: 50_000.0,
                shadows: false,
            },
        ));
        app.spawn((
            Name("Lamp".into()),
            Transform::new([2.0, 3.0, 4.0]),
            PointLight {
                color: [0.4, 0.7, 1.0],
                intensity: 750.0,
                range: 12.0,
                shadows: false,
                radius: 0.0,
            },
        ));
        app.spawn((
            Name("Spot".into()),
            Transform::default(),
            SpotLight::default(),
        ));
        let ambient = AmbientLight {
            color: [0.2, 0.3, 0.9],
            intensity: 0.4,
        };
        let sky = SkyLight {
            sky_color: [0.5, 0.6, 1.0],
            ground_color: [0.2, 0.1, 0.0],
            intensity: 0.7,
        };
        let tone = ToneMapping {
            mapper: ToneMapper::Aces,
            exposure: 1.5,
        };
        let bounds = RenderBounds::Sphere {
            center: [0.0, 1.0, 0.0],
            radius: 2.5,
        };
        app.spawn((Name("Sky".into()), ambient, sky, tone, bounds));
        let document = scene_document(app.world_mut(), "Lights").unwrap();
        assert!(document.entities.iter().any(|entity| entity
            .components
            .contains_key(AMBIENT_LIGHT_COMPONENT)));
        let mut cooked = COMPILED_MAGIC.to_vec();
        cooked.extend(bincode::serialize(&document).unwrap());
        let document = decode_scene(&cooked).unwrap();
        assert_eq!(
            document
                .entities
                .iter()
                .filter(|entity| entity.directional_light.is_some())
                .count(),
            1
        );
        assert_eq!(
            document
                .entities
                .iter()
                .filter(|entity| entity.point_light.is_some())
                .count(),
            1
        );
        assert_eq!(
            document
                .entities
                .iter()
                .filter(|entity| entity.spot_light.is_some())
                .count(),
            1
        );

        load_scene_document(app.world_mut(), &document, SceneLoadMode::Replace)
            .unwrap();
        let directional = app
            .world_mut()
            .query::<&DirectionalLight>()
            .iter(app.world())
            .count();
        let points = app
            .world_mut()
            .query::<&PointLight>()
            .iter(app.world())
            .count();
        let spots = app
            .world_mut()
            .query::<&SpotLight>()
            .iter(app.world())
            .count();
        assert_eq!((directional, points, spots), (1, 1, 1));
        let ambients = app
            .world_mut()
            .query::<&AmbientLight>()
            .iter(app.world())
            .copied()
            .collect::<Vec<_>>();
        assert_eq!(ambients, [ambient]);
        let skies = app
            .world_mut()
            .query::<&SkyLight>()
            .iter(app.world())
            .copied()
            .collect::<Vec<_>>();
        assert_eq!(skies, [sky]);
        let tones = app
            .world_mut()
            .query::<&ToneMapping>()
            .iter(app.world())
            .copied()
            .collect::<Vec<_>>();
        assert_eq!(tones, [tone]);
        let saved_bounds = app
            .world_mut()
            .query::<&RenderBounds>()
            .iter(app.world())
            .copied()
            .collect::<Vec<_>>();
        assert_eq!(saved_bounds, [bounds]);
    }

    #[test]
    fn source_and_compiled_scene_decode_to_same_document() {
        let mut app = scene_app();
        app.spawn((
            Name("Camera".into()),
            Transform::default(),
            Camera::default(),
        ));
        app.spawn((
            Name("Split".into()),
            Transform::default(),
            Camera {
                viewport: Some([0.5, 0.0, 0.5, 1.0]),
                ..Camera::default()
            },
        ));
        let document = scene_document(app.world_mut(), "Compile").unwrap();
        let source = serde_json::to_vec(&document).unwrap();
        let mut compiled = COMPILED_MAGIC.to_vec();
        compiled.extend(bincode::serialize(&document).unwrap());
        assert_eq!(decode_scene(&source).unwrap(), document);
        assert_eq!(decode_scene(&compiled).unwrap(), document);
    }

    #[test]
    fn version_one_cooked_scene_migrates_without_gpu_watch_data() {
        let legacy = LegacySceneDocumentV1 {
            format_version: 1,
            name: "Old Cooked Scene".into(),
            entities: Vec::new(),
        };
        let mut bytes = COMPILED_MAGIC.to_vec();
        bytes.extend(bincode::serialize(&legacy).unwrap());

        let migrated = decode_scene(&bytes).unwrap();

        assert_eq!(migrated.format_version, SCENE_FORMAT_VERSION);
        assert_eq!(migrated.name, "Old Cooked Scene");
    }

    #[test]
    fn version_two_cooked_scene_migrates_without_object_classes() {
        let legacy = LegacySceneDocumentV2 {
            format_version: 2,
            name: "Scene Before Classes".into(),
            entities: Vec::new(),
        };
        let mut bytes = COMPILED_MAGIC.to_vec();
        bytes.extend(bincode::serialize(&legacy).unwrap());

        let migrated = decode_scene(&bytes).unwrap();

        assert_eq!(migrated.format_version, SCENE_FORMAT_VERSION);
        assert_eq!(migrated.name, "Scene Before Classes");
    }

    #[test]
    fn version_three_cooked_scene_migrates_without_authored_lights() {
        let entity_id = Uuid::new_v4();
        let legacy = LegacySceneDocumentV3 {
            format_version: 3,
            name: "Scene Before Lights".into(),
            entities: vec![LegacySceneEntityV3 {
                id: entity_id,
                parent: None,
                name: Some("Unlit Entity".into()),
                classes: Vec::new(),
                transform: Some(SceneTransform {
                    position: [0.0; 3],
                    rotation: [0.0; 3],
                    scale: [1.0; 3],
                }),
                mesh_renderer: None,
                camera: None,
                visible: Some(true),
                physics_body: None,
                rigid_body: None,
                collider: None,
                collision_layers: None,
                gpu_physics_watch: None,
                components: BTreeMap::new(),
            }],
        };
        let mut bytes = COMPILED_MAGIC.to_vec();
        bytes.extend(bincode::serialize(&legacy).unwrap());

        let migrated = decode_scene(&bytes).unwrap();

        assert_eq!(migrated.format_version, SCENE_FORMAT_VERSION);
        assert_eq!(migrated.name, "Scene Before Lights");
        assert_eq!(migrated.entities[0].id, entity_id);
        assert_eq!(migrated.entities[0].directional_light, None);
        assert_eq!(migrated.entities[0].point_light, None);
        assert_eq!(migrated.entities[0].spot_light, None);
    }

    #[test]
    fn version_five_scenes_load_with_default_render_settings() {
        let legacy = LegacySceneDocumentV5 {
            format_version: 5,
            name: "Scene Before Render Settings".into(),
            entities: Vec::new(),
        };
        let mut bytes = COMPILED_MAGIC.to_vec();
        bytes.extend(bincode::serialize(&legacy).unwrap());
        let json = serde_json::to_vec(&legacy).unwrap();
        for bytes in [bytes, json] {
            let migrated = decode_scene(&bytes).unwrap();
            assert_eq!(migrated.format_version, SCENE_FORMAT_VERSION);
            assert_eq!(migrated.name, "Scene Before Render Settings");
            assert_eq!(migrated.render, SceneRenderSettings::default());
        }
    }

    #[test]
    fn version_seven_cooked_scenes_read_materials_without_the_new_fields() {
        let legacy = LegacySceneDocumentV7 {
            format_version: 7,
            name: "Scene Before Material Transmission".into(),
            entities: vec![SceneEntityV7 {
                id: Uuid::new_v4(),
                parent: None,
                name: Some("Box".into()),
                classes: Vec::new(),
                transform: None,
                mesh_renderer: Some(SceneMeshRendererV7 {
                    mesh: SceneMesh::BuiltinCube,
                    material: SceneMaterialV7::Inline(SceneMaterialDataV7 {
                        model: SceneMaterialModel::Pbr,
                        alpha_mode: SceneAlphaMode::Blend,
                        base_color: [0.1, 0.2, 0.3, 0.4],
                        emissive: [0.0; 3],
                        metallic: 0.5,
                        roughness: 0.6,
                        base_color_texture: Some("a.png".into()),
                        normal_texture: None,
                        metallic_roughness_texture: None,
                        occlusion_texture: None,
                        emissive_texture: None,
                    }),
                    cast_shadows: true,
                    receive_shadows: false,
                }),
                camera: None,
                visible: None,
                physics_body: None,
                rigid_body: None,
                collider: None,
                collision_layers: None,
                gpu_physics_watch: None,
                components: BTreeMap::new(),
                directional_light: None,
                point_light: None,
                spot_light: None,
            }],
            render: SceneRenderSettings::default(),
            simulation: SceneSimulationSettings::default(),
        };
        let mut bytes = COMPILED_MAGIC.to_vec();
        bytes.extend(bincode::serialize(&legacy).unwrap());
        let migrated = decode_scene(&bytes).unwrap();
        assert_eq!(migrated.format_version, SCENE_FORMAT_VERSION);
        let renderer = migrated.entities[0].mesh_renderer.as_ref().unwrap();
        let SceneMaterial::Inline(material) = &renderer.material else {
            panic!("inline material expected");
        };
        assert_eq!(material.roughness, 0.6);
        assert_eq!(material.base_color_texture, Some("a.png".into()));
        assert_eq!(material.uv_scale, [1.0; 2]);
        assert_eq!(material.transmission, 0.0);
        assert!(renderer.cast_shadows && !renderer.receive_shadows);
    }

    #[test]
    fn version_eight_and_nine_cooked_scenes_migrate_their_materials() {
        let data = SceneMaterialDataV8 {
            model: SceneMaterialModel::Pbr,
            alpha_mode: SceneAlphaMode::Opaque,
            base_color: [0.1, 0.2, 0.3, 1.0],
            emissive: [0.0; 3],
            metallic: 0.5,
            roughness: 0.6,
            transmission: 0.7,
            ior: 1.4,
            thickness: 0.2,
            uv_scale: [2.0, 3.0],
            uv_offset: [0.1, 0.2],
            base_color_texture: None,
            normal_texture: None,
            metallic_roughness_texture: None,
            occlusion_texture: None,
            emissive_texture: None,
        };
        let v8 = migrate_legacy(8, data.clone());
        assert_eq!(v8.name, "");
        assert_eq!(v8.transmission, 0.7);
        assert_eq!(v8.uv_scale, [2.0, 3.0]);
        let v9 = migrate_legacy(
            9,
            SceneMaterialDataV9 {
                name: "glass".into(),
                rest: data,
            },
        );
        assert_eq!(v9.name, "glass");
        assert_eq!(v9.transmission, 0.7);
        assert!(!v9.flat_shading);
    }

    /// Cooks a one-box scene in an old format and reads it back.
    fn migrate_legacy<M: Serialize>(
        format_version: u32,
        material: M,
    ) -> SceneMaterialData {
        let legacy = LegacySceneDocumentV8 {
            format_version,
            name: "Scene Before Material Names".into(),
            entities: vec![SceneEntityV8 {
                id: Uuid::new_v4(),
                parent: None,
                name: Some("Box".into()),
                classes: Vec::new(),
                transform: None,
                mesh_renderer: Some(SceneMeshRendererV8 {
                    mesh: SceneMesh::BuiltinCube,
                    material: SceneMaterialV8::Inline(material),
                    cast_shadows: true,
                    receive_shadows: true,
                }),
                camera: None,
                visible: None,
                physics_body: None,
                rigid_body: None,
                collider: None,
                collision_layers: None,
                gpu_physics_watch: None,
                components: BTreeMap::new(),
                directional_light: None,
                point_light: None,
                spot_light: None,
            }],
            render: SceneRenderSettings::default(),
            simulation: SceneSimulationSettings::default(),
        };
        let mut bytes = COMPILED_MAGIC.to_vec();
        bytes.extend(bincode::serialize(&legacy).unwrap());
        let migrated = decode_scene(&bytes).unwrap();
        assert_eq!(migrated.format_version, SCENE_FORMAT_VERSION);
        let renderer = migrated.entities[0].mesh_renderer.as_ref().unwrap();
        let SceneMaterial::Inline(material) = &renderer.material else {
            panic!("inline material expected");
        };
        material.clone()
    }

    #[test]
    fn material_names_survive_the_scene_form() {
        let mut assets = AssetServer::default();
        let material = MaterialAsset {
            name: "glass".into(),
            ..MaterialAsset::default()
        };
        let data = scene_material(&material, &assets).unwrap();
        assert_eq!(data.name, "glass");
        assert_eq!(runtime_material(&data, &mut assets).unwrap(), material);
    }

    #[test]
    fn version_six_cooked_scenes_keep_render_settings() {
        let render = SceneRenderSettings {
            quality: QualityProfile::High,
            culling: CullingMode::FrustumAndOcclusion,
        };
        let legacy = LegacySceneDocumentV6 {
            format_version: 6,
            name: "Scene Before Simulation Settings".into(),
            entities: Vec::new(),
            render,
        };
        let mut bytes = COMPILED_MAGIC.to_vec();
        bytes.extend(bincode::serialize(&legacy).unwrap());
        let migrated = decode_scene(&bytes).unwrap();
        assert_eq!(migrated.format_version, SCENE_FORMAT_VERSION);
        assert_eq!(migrated.render, render);
        assert_eq!(migrated.simulation, SceneSimulationSettings::default());
    }

    #[test]
    fn cook_copies_project_determinism_and_replacing_loads_insert_it() {
        let root = std::env::temp_dir()
            .join(format!("rusting-determinism-{}", Uuid::new_v4()));
        let source = root.join("scenes/main.rscene");
        let cooked = root.join("build/main.rscene.bin");
        std::fs::create_dir_all(source.parent().unwrap()).unwrap();
        std::fs::write(root.join("project.json"), r#"{"determinism":"Local"}"#)
            .unwrap();
        let mut editor_app = scene_app();
        save_scene(editor_app.world_mut(), &source, "Main").unwrap();
        cook_scene(&source, &cooked).unwrap();

        let mut game_app = scene_app();
        load_scene(game_app.world_mut(), &cooked, SceneLoadMode::Replace)
            .unwrap();
        assert_eq!(
            game_app.world().get_resource::<DeterminismMode>(),
            Some(&DeterminismMode::Local)
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn version_four_cooked_scene_migrates_materials_to_opaque_alpha() {
        let legacy = LegacySceneDocumentV4 {
            format_version: 4,
            name: "Scene Before Alpha".into(),
            entities: vec![LegacySceneEntityV4 {
                id: Uuid::new_v4(),
                parent: None,
                name: Some("Cube".into()),
                classes: Vec::new(),
                transform: None,
                mesh_renderer: Some(LegacySceneMeshRendererV4 {
                    mesh: SceneMesh::BuiltinCube,
                    material: LegacySceneMaterialV4::Inline(
                        LegacySceneMaterialDataV4 {
                            model: SceneMaterialModel::Pbr,
                            base_color: [0.0, 0.2, 0.3, 0.4],
                            emissive: [0.0; 3],
                            metallic: 0.0,
                            roughness: 0.5,
                            base_color_texture: None,
                            normal_texture: None,
                            metallic_roughness_texture: None,
                            occlusion_texture: None,
                            emissive_texture: None,
                        },
                    ),
                    cast_shadows: true,
                    receive_shadows: false,
                }),
                camera: None,
                visible: Some(true),
                physics_body: None,
                rigid_body: None,
                collider: None,
                collision_layers: None,
                gpu_physics_watch: None,
                components: BTreeMap::new(),
                directional_light: Some(DirectionalLight::default()),
                point_light: None,
                spot_light: None,
            }],
        };
        let mut bytes = COMPILED_MAGIC.to_vec();
        bytes.extend(bincode::serialize(&legacy).unwrap());

        let migrated = decode_scene(&bytes).unwrap();

        assert_eq!(migrated.format_version, SCENE_FORMAT_VERSION);
        let entity = &migrated.entities[0];
        assert_eq!(entity.directional_light, Some(DirectionalLight::default()));
        let renderer = entity.mesh_renderer.as_ref().unwrap();
        assert!(renderer.cast_shadows && !renderer.receive_shadows);
        let SceneMaterial::Inline(material) = &renderer.material else {
            panic!("inline material expected");
        };
        assert_eq!(material.alpha_mode, SceneAlphaMode::Opaque);
        assert_eq!(material.base_color, [0.0, 0.2, 0.3, 0.4]);
    }

    #[test]
    fn material_alpha_mode_survives_scene_round_trip() {
        let mut assets = AssetServer::default();
        for alpha_mode in [
            AlphaMode::Opaque,
            AlphaMode::Mask { cutoff: 0.25 },
            AlphaMode::Blend,
        ] {
            let material = MaterialAsset {
                alpha_mode,
                ..MaterialAsset::default()
            };
            let saved = scene_material(&material, &assets).unwrap();
            let json = serde_json::to_vec(&saved).unwrap();
            let decoded: SceneMaterialData =
                serde_json::from_slice(&json).unwrap();
            let restored = runtime_material(&decoded, &mut assets).unwrap();
            assert_eq!(restored.alpha_mode, alpha_mode);
        }
    }

    #[test]
    fn unversioned_text_scene_migrates_but_newer_scene_is_rejected() {
        let mut app = scene_app();
        app.spawn((Name("Legacy".into()), Transform::default()));
        let current = scene_document(app.world_mut(), "Legacy").unwrap();
        let mut legacy = serde_json::to_value(&current).unwrap();
        legacy.as_object_mut().unwrap().remove("format_version");

        let migrated =
            decode_scene(&serde_json::to_vec(&legacy).unwrap()).unwrap();
        assert_eq!(migrated.format_version, SCENE_FORMAT_VERSION);
        assert_eq!(migrated.entities, current.entities);

        let mut newer = current;
        newer.format_version = SCENE_FORMAT_VERSION + 1;
        assert!(matches!(
            decode_scene(&serde_json::to_vec(&newer).unwrap()),
            Err(SceneIoError::UnsupportedVersion(_))
        ));
    }

    #[test]
    fn invalid_hierarchy_is_rejected_before_the_open_scene_is_replaced() {
        let mut app = scene_app();
        let root = app.spawn((Name("Root".into()), Transform::default()));
        let child = app.spawn((Name("Child".into()), Transform::default()));
        app.set_parent(child, root).unwrap();
        let original = scene_document(app.world_mut(), "Original").unwrap();
        let mut invalid = original.clone();
        let root_id = invalid
            .entities
            .iter()
            .find(|entity| entity.name.as_deref() == Some("Root"))
            .unwrap()
            .id;
        let child_id = invalid
            .entities
            .iter()
            .find(|entity| entity.name.as_deref() == Some("Child"))
            .unwrap()
            .id;
        invalid
            .entities
            .iter_mut()
            .find(|entity| entity.id == root_id)
            .unwrap()
            .parent = Some(child_id);

        assert!(matches!(
            load_scene_document(
                app.world_mut(),
                &invalid,
                SceneLoadMode::Replace
            ),
            Err(SceneIoError::HierarchyCycle(_))
        ));
        assert_eq!(
            scene_document(app.world_mut(), "Original").unwrap(),
            original
        );
    }

    #[test]
    fn duplicate_object_names_are_rejected() {
        let mut app = scene_app();
        app.spawn((Name("Cube".into()), Transform::default()));
        app.spawn((Name("Cube".into()), Transform::default()));

        assert!(matches!(
            scene_document(app.world_mut(), "Duplicates"),
            Err(SceneIoError::DuplicateName(name)) if name == "Cube"
        ));
    }

    #[test]
    fn scenes_saved_with_old_simulation_names_still_load() {
        let old: Vec<SimulationClass> =
            serde_json::from_str(r#"["Gameplay", "GpuDynamic"]"#).unwrap();
        assert_eq!(old, [SimulationClass::Cpu, SimulationClass::Gpu]);
        assert_eq!(
            serde_json::to_string(&SimulationClass::Gpu).unwrap(),
            r#""Gpu""#
        );
    }

    #[test]
    fn saved_scene_can_be_cooked_and_loaded_by_a_fresh_runtime() {
        let test_directory = std::env::temp_dir()
            .join(format!("rusting-scene-test-{}", Uuid::new_v4()));
        let source = test_directory.join("scene.rscene");
        let compiled = test_directory.join("scene.rscene.bin");

        let mut editor_app = scene_app();
        editor_app.spawn((
            Name("Runtime Cube".into()),
            Transform::new([4.0, 5.0, 6.0]),
            GameplayTag { speed: 3.0 },
        ));
        let shipped = SceneRenderSettings {
            quality: QualityProfile::High,
            culling: CullingMode::FrustumAndOcclusion,
        };
        {
            let mut settings =
                editor_app.world_mut().resource_mut::<RenderSettings>();
            settings.quality = shipped.quality;
            settings.culling = shipped.culling;
        }
        save_scene(editor_app.world_mut(), &source, "Runtime").unwrap();
        cook_scene(&source, &compiled).unwrap();

        let mut game_app = scene_app();
        load_scene(game_app.world_mut(), &compiled, SceneLoadMode::Additive)
            .unwrap();
        let settings = game_app.world().resource::<RenderSettings>();
        assert_eq!(
            (settings.quality, settings.culling),
            (QualityProfile::Auto, CullingMode::Auto),
            "an additive load keeps the current render settings"
        );
        load_scene(game_app.world_mut(), &compiled, SceneLoadMode::Replace)
            .unwrap();
        let settings = game_app.world().resource::<RenderSettings>();
        assert_eq!(
            (settings.quality, settings.culling),
            (shipped.quality, shipped.culling)
        );
        let mut query = game_app
            .world_mut()
            .query::<(&Name, &Transform, &GameplayTag)>();
        let (name, transform, tag) = query.single(game_app.world()).unwrap();
        assert_eq!(name.0, "Runtime Cube");
        assert_eq!(transform.position, [4.0, 5.0, 6.0]);
        assert_eq!(tag.speed, 3.0);

        std::fs::remove_file(compiled).unwrap();
        std::fs::remove_file(source).unwrap();
        std::fs::remove_dir(test_directory).unwrap();
    }

    #[test]
    fn cooked_scene_asset_paths_remain_project_relative_and_reloadable() {
        let project = std::env::temp_dir()
            .join(format!("rusting-portable-scene-{}", Uuid::new_v4()));
        let assets_folder = project.join("assets");
        let scenes_folder = project.join("scenes");
        let build_folder = project.join("build");
        std::fs::create_dir_all(&assets_folder).unwrap();
        std::fs::create_dir_all(&scenes_folder).unwrap();
        let texture_path = assets_folder.join("pixel.png");
        image::RgbaImage::from_raw(1, 1, vec![255, 128, 0, 255])
            .unwrap()
            .save(&texture_path)
            .unwrap();
        let source = scenes_folder.join("main.rscene");
        let cooked = build_folder.join("main.rscene.bin");
        let mut editor = scene_app();
        let (mesh, material) = {
            let mut assets = editor.world_mut().resource_mut::<AssetServer>();
            let texture = assets.load_texture(&texture_path).unwrap();
            let material = assets.materials.insert(MaterialAsset {
                base_color_texture: Some(texture),
                ..MaterialAsset::default()
            });
            (assets.fallback_mesh, material)
        };
        editor.spawn((
            Name("Textured Cube".into()),
            Transform::default(),
            MeshRenderer {
                mesh,
                material,
                cast_shadows: true,
                receive_shadows: true,
            },
        ));

        save_scene(editor.world_mut(), &source, "Portable").unwrap();
        let saved: SceneDocument =
            serde_json::from_slice(&std::fs::read(&source).unwrap()).unwrap();
        let saved_texture = saved.entities[0]
            .mesh_renderer
            .as_ref()
            .and_then(|renderer| match &renderer.material {
                SceneMaterial::Inline(material) => {
                    material.base_color_texture.as_ref()
                }
                SceneMaterial::BuiltinError => None,
            })
            .unwrap();
        assert!(!saved_texture.is_absolute());
        assert!(saved_texture.ends_with(Path::new("assets").join("pixel.png")));
        cook_scene(&source, &cooked).unwrap();

        let mut runtime = scene_app();
        load_scene(runtime.world_mut(), &cooked, SceneLoadMode::Replace)
            .unwrap();
        assert_eq!(runtime.world().resource::<AssetServer>().textures.len(), 2);
        std::fs::remove_dir_all(project).unwrap();
    }

    #[cfg(feature = "gltf")]
    #[test]
    fn gltf_materials_survive_save_and_fresh_load_unless_overridden() {
        use crate::assets::{
            spawn_gltf_nodes, AlphaMode, TextureColorSpace, TextureFilter,
        };

        let folder = std::env::temp_dir()
            .join(format!("rusting-gltf-material-scene-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&folder).unwrap();
        image::RgbaImage::from_raw(1, 1, vec![128, 128, 255, 255])
            .unwrap()
            .save(folder.join("normal.png"))
            .unwrap();
        let positions: Vec<u8> =
            [0.0f32, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0]
                .iter()
                .flat_map(|value| value.to_le_bytes())
                .collect();
        std::fs::write(folder.join("tri.bin"), &positions).unwrap();
        let gltf = folder.join("tri.gltf");
        std::fs::write(
            &gltf,
            r#"{
              "asset": {"version": "2.0"},
              "buffers": [{"uri": "tri.bin", "byteLength": 36}],
              "bufferViews": [{"buffer": 0, "byteLength": 36}],
              "accessors": [{"bufferView": 0, "componentType": 5126,
                "count": 3, "type": "VEC3",
                "min": [0, 0, 0], "max": [1, 1, 0]}],
              "images": [{"uri": "normal.png"}],
              "samplers": [{"magFilter": 9728}],
              "textures": [{"source": 0, "sampler": 0}],
              "materials": [{
                "pbrMetallicRoughness": {"baseColorFactor": [0.2, 0.4, 0.6, 1],
                  "roughnessFactor": 0.3},
                "normalTexture": {"index": 0},
                "alphaMode": "BLEND"}],
              "meshes": [{"primitives": [
                {"attributes": {"POSITION": 0}, "material": 0}]}],
              "nodes": [{"name": "Tri", "mesh": 0}]
            }"#,
        )
        .unwrap();
        let scene = folder.join("main.rscene");
        let mut editor = scene_app();
        let nodes = editor
            .world_mut()
            .resource_mut::<AssetServer>()
            .import_gltf_scene(&gltf)
            .unwrap();
        spawn_gltf_nodes(&mut editor, &nodes, None).unwrap();
        save_scene(editor.world_mut(), &scene, "Imported").unwrap();

        let mut runtime = scene_app();
        load_scene(runtime.world_mut(), &scene, SceneLoadMode::Replace)
            .unwrap();
        let world = runtime.world_mut();
        let material_handle = world
            .query::<&MeshRenderer>()
            .single(world)
            .unwrap()
            .material;
        let assets = world.resource::<AssetServer>();
        let material = assets.materials.get(material_handle).unwrap();
        assert_eq!(material.base_color, [0.2, 0.4, 0.6, 1.0]);
        assert_eq!(material.roughness, 0.3);
        assert_eq!(material.alpha_mode, AlphaMode::Blend);
        let normal = assets.textures.get(material.normal_texture.unwrap());
        let normal = normal.unwrap();
        assert_eq!(normal.color_space, TextureColorSpace::Linear);
        assert_eq!(normal.sampler.mag_filter, TextureFilter::Nearest);

        let mut overridden = scene_app();
        let replacement = overridden
            .world_mut()
            .resource_mut::<AssetServer>()
            .materials
            .insert(MaterialAsset::default());
        let entities =
            spawn_gltf_nodes(&mut overridden, &nodes, Some(replacement))
                .unwrap();
        assert_eq!(
            overridden
                .world()
                .get::<MeshRenderer>(entities[0])
                .unwrap()
                .material,
            replacement
        );
        std::fs::remove_dir_all(folder).unwrap();
    }
}
