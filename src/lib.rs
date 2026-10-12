// Recipe sources name the crate as a game does; this lets the engine's
// tests compile them unchanged.
extern crate self as rusting_engine;

pub mod annotate;
pub mod art_direction;
pub mod asset_import;
pub mod assets;
pub mod collab;
pub mod cookbook;
pub mod core;
pub mod crash;
pub mod debug_session;
pub mod demo;
pub mod diagnostics;
pub mod docs;
// pub mod effects;
#[cfg(feature = "window")]
mod audio_output;
pub mod cli;
#[cfg(feature = "editor")]
pub mod editor;
pub mod engine;
pub mod fontgen;
pub mod geometry;
pub mod input;
pub mod meshgen;
pub mod mods;
pub mod net;
pub mod platform;
pub mod procgen;
pub mod project;
#[cfg(feature = "window")]
pub mod project_runner;
pub mod recipes;
pub mod reflect;
pub mod rendering;
pub mod runtime;
pub mod scenario;
pub mod scene_merge;
pub mod scene_patch;
pub mod schema;
pub mod sfx;
pub mod steam;
#[cfg(test)]
pub mod tests;
pub mod texgen;
#[cfg(feature = "ui")]
pub mod text_texture;

pub use assets::{
    spawn_gltf_nodes, spawn_gltf_nodes_in_world, AlphaMode, AssetPlugin,
    AssetServer, DataAsset, Handle, ImportedGltfLight, ImportedGltfNode,
    ImportedGltfPrimitive, MaterialAsset, MaterialModel, MeshAsset,
    PrimitiveShape, SceneAsset, TextureAsset, TextureColorSpace, TextureFilter,
    TextureSampler, TextureWrap,
};
/// For game code that saves settings or state as JSON.
pub use bevy_ecs;
pub use core::collisions::CollisionType;
pub use core::{Material, MaterialBuilder, Physics, Transform};
#[cfg(feature = "editor")]
pub use editor::EditorPlugin;
/// The egui version [`runtime::RuntimeUi`] draws with.
#[cfg(feature = "ui")]
pub use egui;
pub use engine::{Engine, PerspectiveCamera};
pub use geometry::Mesh;
pub use rendering::compute_profile::ComputeShaderType;
pub use runtime::{
    App, EngineBuilder, GpuCondition, GpuEventMode, GpuEventPayload,
    GpuPhysicsClassWatches, GpuPhysicsEvent, GpuPhysicsRule, GpuPhysicsWatch,
    HybridPhysicsPlugin, ObjectClasses, PhysicsId, Plugin, RenderSettings,
};
pub use serde;
pub use serde_json;

/// Common imports for concise native Rust gameplay code.
#[cfg(feature = "window")]
pub mod prelude {
    pub use crate::project_runner::{
        CubeSpawn, GameObject, GameResult, GameScene, GameSnapshot,
        GpuBodySettings, InitialState, RayHit, SphereSpawn,
    };
    pub use crate::rendering::scene_renderer::RenderCapacityDiagnostics;
    pub use crate::runtime::{
        ActiveSound, BeatClock, BusEffect, Caption, Easing, FrameTime,
        GpuBodyCommand, GpuCondition, GpuConditionShader, GpuConditionShaders,
        GpuEventMode, GpuEventPayload, GpuFieldCondition, GpuPhysicsEvent,
        GpuPhysicsEventsLost, GpuPhysicsRule, GpuPhysicsWatch, GpuStateField,
        HudAnchor, HudElement, Name, ObjectClasses, ParticleCommand,
        PhysicsSolver, PhysicsSyncMode, PhysicsWorld, PlayerController,
        RigidBody, RigidBodyKind, Sound, SoundId, Stick, TweenProperty,
        WaypointGraph,
    };
    pub use crate::rusting_game;
    pub use crate::{AlphaMode, MaterialAsset, MaterialModel};
    pub use crate::{AssetServer, Transform};
    pub use crate::{TextureAsset, TextureColorSpace, TextureSampler};
    pub use bevy_ecs::entity::Entity;
    pub use bevy_ecs::world::World;
    // A game's own components need `#[derive(Component, Serialize,
    // Deserialize)]` without adding `bevy_ecs` or `serde` to its
    // Cargo.toml. The `bevy_ecs` name lets the `Component` derive find its
    // crate; serde's derive needs `#[serde(crate = "rusting_engine::serde")]`.
    pub use bevy_ecs::{self, component::Component};
    pub use serde::{self, Deserialize, Serialize};
}
