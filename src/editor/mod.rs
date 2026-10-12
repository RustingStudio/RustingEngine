//! Feature-gated egui editor state and ECS panels.
//!
//! The window runner calls [`draw_editor_view`] inside its egui frame and
//! composites the resulting shapes over the Vulkan scene.

mod agent_panel;
mod assets_panel;
mod collab_panel;
mod diagnostics;
mod dock;
mod file_dialogs;
pub mod gui_elements;
mod hierarchy;
mod icons;
mod inspector;
mod overlay;
mod picking;
pub mod profiler;
mod project;
mod project_settings;
mod shortcuts;
mod timeline;
pub mod view;

use dock::EditorLayoutFile;
pub use dock::{EditorDockNode, EditorPanel, EditorSplitAxis};
use file_dialogs::{DialogPurpose, FileDialogs};
use hierarchy::{collect_entities, draw_hierarchy_area};
pub use icons::EditorIcon;
pub use inspector::InspectorRegistry;
use inspector::{draw_inspector_area, ComponentEdit};
pub use view::draw_editor_view;

use crate::project::export_built_game;
#[cfg(test)]
use crate::project::package_game_files;
pub use project::{
    create_project, create_project_from, open_project, EditorPreferences,
    OpenProject, ProjectError, ProjectManagerState, ProjectManifest,
    ProjectTemplate, RecentProject, PROJECT_FORMAT_VERSION,
};
pub use shortcuts::{
    add_mouse_delta, editor_navigation_active, handle_keyboard_input,
    handle_mouse_button_input, handle_mouse_wheel, release_editor_navigation,
    ui_receives_during_navigation, update_fly_camera, EditorAction,
    EditorCommandQueue, EditorFlyCamera, EditorShortcuts, EditorTransformMode,
    KeyBinding, SceneViewAction, ShortcutAction, ShortcutContext,
    TransformModes,
};

use bevy_ecs::entity::Entity;
use bevy_ecs::prelude::{Resource, World};
use egui::{CentralPanel, Context, TopBottomPanel};
use std::path::PathBuf;

use crate::rendering::debug_overlay::RenderDebugOverlay;
use crate::rendering::scene_renderer::{SceneDebugView, SceneEffects};
use crate::runtime::{
    add_registered_component, cook_scene, load_scene,
    registered_component_names, registered_component_values,
    remove_registered_component, save_scene, scene_document,
    set_registered_component, App, AppError, Camera, Collider, CollisionLayers,
    DirectionalLight, GlobalTransform, MeshRenderer, Name, ObjectClasses,
    Parent, PhysicsBackendStatus, PhysicsBody, Plugin, PointLight,
    RenderBounds, RenderCameraOverride, RenderSettings, RenderWorld, RigidBody,
    SceneDocument, SceneId, SceneLoadMode, SpotLight, Visibility,
};
use crate::Transform;
use crate::{AssetServer, Handle, MaterialAsset, PrimitiveShape};

/// True when the editor must redraw every frame: the game is playing or the
/// fly camera moves without window events. Otherwise it redraws only on input,
/// egui repaint requests, and a slow idle tick.
pub fn editor_needs_continuous_redraw(world: &World) -> bool {
    world
        .get_resource::<EditorState>()
        .is_some_and(|state| state.mode == EditorMode::Play)
        || world
            .get_resource::<EditorFlyCamera>()
            .is_some_and(|fly| fly.active || fly.drag.is_some())
        || previewing_particles(world)
        || world
            .get_resource::<EditorState>()
            .is_some_and(|state| state.timeline.playing)
}

fn previewing_particles(world: &World) -> bool {
    world
        .get_resource::<EditorState>()
        .is_some_and(|state| state.mode == EditorMode::Edit)
        && world.iter_entities().any(|entity| {
            entity.get::<crate::runtime::ParticleSystem>().is_some_and(
                |system| {
                    !system.paused && (system.playing || system.alive() > 0)
                },
            )
        })
}

/// Live particle preview while the scene is stopped, like Godot's emitting
/// preview: emitters play in the Scene view without starting the game.
/// Call once per frame before the runtime update.
pub fn update_edit_preview(world: &mut World, delta: std::time::Duration) {
    if world
        .get_resource::<EditorState>()
        .is_some_and(|state| state.mode == EditorMode::Edit)
    {
        crate::runtime::preview_particles(world, delta);
    }
}

/// Editor view state saved beside a scene, like Godot's per-scene edit
/// state. The game never reads it, so the scene file stays clean.
#[derive(serde::Serialize, serde::Deserialize, Default, Debug, PartialEq)]
struct EditorSceneMetadata {
    camera_position: Option<[f32; 3]>,
    camera_rotation: Option<[f32; 3]>,
    orbit_distance: Option<f32>,
    /// Selected objects by `SceneId`; the first one is the active object.
    selection: Vec<uuid::Uuid>,
}

/// Sidecar path for a scene's editor metadata: `main.rscene` becomes
/// `main.rscene.editor.json`.
fn editor_metadata_path(scene: &std::path::Path) -> PathBuf {
    let mut path = scene.as_os_str().to_owned();
    path.push(".editor.json");
    PathBuf::from(path)
}

/// Scene file revision the editor last loaded or saved. An agent's
/// `rusting scene patch` changes the file under the open editor; comparing
/// revisions turns that into a reload or a conflict instead of lost work.
#[derive(Resource, Clone, Debug, Default)]
pub struct SceneFileRevision {
    path: Option<PathBuf>,
    revision: String,
    modified: Option<std::time::SystemTime>,
    /// The file as loaded or saved, so a pending outside write is diffed
    /// against it and the user's unsaved edits do not count as the agent's.
    base: Option<SceneDocument>,
}

impl SceneFileRevision {
    fn read(path: &std::path::Path) -> Option<Self> {
        let bytes = crate::runtime::read_scene_bytes(path).ok()?;
        Some(Self {
            // Canonical, so project and Save paths to one file compare equal.
            path: path.canonicalize().ok(),
            revision: crate::runtime::scene_revision(&bytes),
            modified: std::fs::metadata(path)
                .and_then(|metadata| metadata.modified())
                .ok(),
            base: None,
        })
    }

    fn record(world: &mut World, path: &std::path::Path) {
        let base = crate::runtime::read_scene_document(path).ok();
        world.insert_resource(Self {
            base,
            ..Self::read(path).unwrap_or_default()
        });
    }
}

/// Reloads the open scene when another tool changed its file. A clean scene
/// reloads behind an Undo snapshot, so Undo reverts the outside change. A
/// scene with unsaved edits, or any scene while agent edits are paused,
/// keeps its state and shows the write as pending in the Agent area: Accept
/// reloads it behind an Undo snapshot, Reject drops it, and the next Save
/// refuses once before it overwrites.
fn reload_external_scene_change(
    world: &mut World,
    state: &mut EditorState,
    history: &mut EditorHistory,
) {
    let Some(known) = world.get_resource::<SceneFileRevision>() else {
        return;
    };
    let Some(path) = known.path.clone() else {
        return;
    };
    if state.mode != EditorMode::Edit {
        return;
    }
    let modified = std::fs::metadata(&path)
        .and_then(|metadata| metadata.modified())
        .ok();
    let (accepted, reject, paused) = world
        .get_resource_mut::<agent_panel::AgentJournal>()
        .map_or((None, false, false), |mut journal| {
            let pending = journal.pending.as_ref().map(|p| p.revision.clone());
            let accept = std::mem::take(&mut journal.accept_requested);
            let reject = std::mem::take(&mut journal.reject_requested);
            let answer = (
                pending.clone().filter(|_| accept),
                reject && pending.is_some(),
                journal.paused,
            );
            if answer.0.is_some() || answer.1 {
                journal.pending = None;
                journal.pending_scene = None;
                journal.pending_fields.clear();
                journal.pending_removed.clear();
            }
            answer
        });
    if reject {
        state.scene_message = Some(format!(
            "Rejected the outside change to {}; Save overwrites it",
            path.display()
        ));
        return;
    }
    let accept_entity = world
        .get_resource_mut::<agent_panel::AgentJournal>()
        .and_then(|mut journal| journal.accept_entity.take());
    if let Some(id) = accept_entity {
        accept_pending_entity(world, state, history, &path, id);
        return;
    }
    let known = world.resource::<SceneFileRevision>();
    if accepted.is_none() && modified == known.modified {
        return;
    }
    let known = known.revision.clone();
    let Some(disk) = SceneFileRevision::read(&path) else {
        return;
    };
    if disk.revision == known {
        world.resource_mut::<SceneFileRevision>().modified = disk.modified;
        return;
    }
    // Accept applies only the revision the user reviewed; a newer write
    // becomes the pending entry instead.
    let stale = accepted.as_ref().is_some_and(|r| *r != disk.revision);
    let accept = accepted.is_some() && !stale;
    if !accept && (stale || state.scene_dirty || paused) {
        world.resource_mut::<SceneFileRevision>().modified = disk.modified;
        let theirs = crate::runtime::read_scene_document(&path).ok();
        let ours = scene_document(world, "Editor Scene").ok();
        set_pending(world, &path, &disk.revision, ours, theirs);
        state.scene_message = Some(if stale {
            format!(
                "{} changed again before Accept; review the new pending \
                 change in the Agent area",
                path.display()
            )
        } else if paused && !state.scene_dirty {
            format!(
                "{} changed on disk while agent edits are paused; Accept \
                 or Reject it in the Agent area",
                path.display()
            )
        } else {
            format!(
                "{} changed on disk while this scene has unsaved edits; \
                 Accept in the Agent area or Load takes the outside change, \
                 Save overwrites it",
                path.display()
            )
        });
        return;
    }
    if let Err(error) = remember_scene_before_edit(world, history) {
        state.scene_message = Some(error);
        return;
    }
    let result = view::reload_keeping_selection(world, state, |world| {
        load_scene(world, &path, SceneLoadMode::Replace)
    });
    state.scene_message = Some(match result {
        Ok(()) => {
            history.redo.clear();
            let revision = disk.revision.clone();
            world.insert_resource(SceneFileRevision {
                base: crate::runtime::read_scene_document(&path).ok(),
                ..disk
            });
            let diff = history
                .undo
                .back()
                .zip(scene_document(world, "Outside Change").ok())
                .map(|(before, after)| outside_change(before, &after))
                .unwrap_or_default();
            highlight_scene_ids(world, state, &diff.touched());
            if let Some(mut journal) =
                world.get_resource_mut::<agent_panel::AgentJournal>()
            {
                journal.record(agent_panel::JournalEntry {
                    path: path.display().to_string(),
                    summary: diff.summary(),
                    ids: diff.touched(),
                    revision,
                });
            }
            format!(
                "Reloaded outside change to {} ({}); Undo reverts it",
                path.display(),
                diff.summary()
            )
        }
        Err(error) => {
            history.undo.pop_back();
            world.resource_mut::<SceneFileRevision>().modified = disk.modified;
            format!("Could not reload outside change: {error}")
        }
    });
}

/// Records an outside write the editor has not applied as the Agent area's
/// pending entry, or clears it when the scenes no longer differ.
fn set_pending(
    world: &mut World,
    path: &std::path::Path,
    revision: &str,
    ours: Option<SceneDocument>,
    theirs: Option<SceneDocument>,
) {
    let base = world
        .get_resource::<SceneFileRevision>()
        .and_then(|known| known.base.clone());
    let (diff, fields) = match (&ours, &theirs) {
        (Some(ours), Some(theirs)) => {
            // What the write changed since the file was loaded or saved,
            // minus what the editor scene already matches.
            let mut diff =
                outside_change(base.as_ref().unwrap_or(ours), theirs);
            let entity = |document: &SceneDocument, id| {
                document
                    .entities
                    .iter()
                    .find(|entity| entity.id == id)
                    .map(|entity| serde_json::to_value(entity).ok())
            };
            let differs =
                |id: &uuid::Uuid| entity(ours, *id) != entity(theirs, *id);
            diff.added.retain(differs);
            diff.changed.retain(differs);
            diff.removed.retain(differs);
            let fields = pending_fields(ours, theirs, &diff.touched());
            (diff, fields)
        }
        _ => Default::default(),
    };
    let removed = diff
        .removed
        .iter()
        .map(|&id| {
            let name = ours
                .iter()
                .flat_map(|ours| &ours.entities)
                .find(|entity| entity.id == id)
                .and_then(|entity| entity.name.clone());
            (id, name.unwrap_or_else(|| id.to_string()))
        })
        .collect();
    let Some(mut journal) =
        world.get_resource_mut::<agent_panel::AgentJournal>()
    else {
        return;
    };
    let empty = diff == OutsideChange::default() && theirs.is_some();
    journal.pending = (!empty).then(|| agent_panel::JournalEntry {
        path: path.display().to_string(),
        summary: diff.summary(),
        ids: diff.touched(),
        revision: revision.to_owned(),
    });
    journal.pending_scene = theirs.filter(|_| !empty);
    journal.pending_fields = fields;
    journal.pending_removed = removed;
}

/// "path: old -> new" for each changed field of these entities.
fn pending_fields(
    ours: &SceneDocument,
    theirs: &SceneDocument,
    ids: &[uuid::Uuid],
) -> Vec<(uuid::Uuid, String)> {
    let value = |document: &SceneDocument, id| {
        document
            .entities
            .iter()
            .find(|entity| entity.id == id)
            .map(|entity| serde_json::to_value(entity).unwrap_or_default())
            .unwrap_or_default()
    };
    let show = |value: &serde_json::Value| match value {
        serde_json::Value::Null => "none".to_owned(),
        value => value.to_string(),
    };
    ids.iter()
        .flat_map(|&id| {
            crate::cli::leaf_changes(&value(ours, id), &value(theirs, id))
                .into_iter()
                .map(move |change| {
                    (
                        id,
                        format!(
                            "{}: {} -> {}",
                            change["path"].as_str().unwrap_or_default(),
                            show(&change["before"]),
                            show(&change["after"])
                        ),
                    )
                })
        })
        .collect()
}

/// Applies one entity of the pending outside write to the editor scene,
/// behind an Undo snapshot, and keeps the rest pending.
fn accept_pending_entity(
    world: &mut World,
    state: &mut EditorState,
    history: &mut EditorHistory,
    path: &std::path::Path,
    id: uuid::Uuid,
) {
    let Some(journal) = world.get_resource::<agent_panel::AgentJournal>()
    else {
        return;
    };
    let (Some(theirs), Some(pending)) =
        (journal.pending_scene.clone(), journal.pending.clone())
    else {
        return;
    };
    let entity = theirs.entities.iter().find(|e| e.id == id).cloned();
    let removed = journal.pending_removed.iter().find(|(r, _)| *r == id);
    let removed = removed.map(|(_, name)| name.clone());
    if entity.is_none() && removed.is_none() {
        return;
    }
    let mut merged = match scene_document(world, "Editor Scene") {
        Ok(document) => document,
        Err(error) => {
            state.scene_message = Some(error.to_string());
            return;
        }
    };
    let name = match entity {
        Some(entity) => {
            let name = entity.name.clone().unwrap_or_else(|| id.to_string());
            match merged.entities.iter_mut().find(|e| e.id == id) {
                Some(slot) => *slot = entity,
                None => merged.entities.push(entity),
            }
            name
        }
        None => {
            // A removal takes the entity's descendants with it.
            let mut gone = std::collections::HashSet::from([id]);
            while let Some(child) = merged.entities.iter().find(|e| {
                !gone.contains(&e.id)
                    && e.parent.is_some_and(|p| gone.contains(&p))
            }) {
                gone.insert(child.id);
            }
            merged.entities.retain(|e| !gone.contains(&e.id));
            removed.unwrap_or_default()
        }
    };
    if let Err(error) = remember_scene_before_edit(world, history) {
        state.scene_message = Some(error);
        return;
    }
    let result = view::reload_keeping_selection(world, state, |world| {
        crate::runtime::load_scene_document(
            world,
            &merged,
            SceneLoadMode::Replace,
        )
    });
    if let Err(error) = result {
        history.undo.pop_back();
        state.scene_message = Some(error.to_string());
        return;
    }
    history.redo.clear();
    state.scene_dirty = true;
    highlight_scene_ids(world, state, &[id]);
    let ours = scene_document(world, "Editor Scene").ok();
    set_pending(world, path, &pending.revision, ours, Some(theirs));
    state.scene_message = Some(format!(
        "Accepted the outside change to {name}; Undo reverts it"
    ));
}

/// Entities an outside write added, removed, or changed, by scene ID.
#[derive(Debug, Default, PartialEq, Eq)]
struct OutsideChange {
    added: Vec<uuid::Uuid>,
    removed: Vec<uuid::Uuid>,
    changed: Vec<uuid::Uuid>,
}

impl OutsideChange {
    /// IDs still in the scene that the change touched.
    fn touched(&self) -> Vec<uuid::Uuid> {
        self.added.iter().chain(&self.changed).copied().collect()
    }

    fn summary(&self) -> String {
        format!(
            "{} added, {} changed, {} removed",
            self.added.len(),
            self.changed.len(),
            self.removed.len()
        )
    }
}

fn outside_change(
    before: &SceneDocument,
    after: &SceneDocument,
) -> OutsideChange {
    let by_id = |document: &SceneDocument| {
        document
            .entities
            .iter()
            .map(|entity| {
                (entity.id, serde_json::to_value(entity).unwrap_or_default())
            })
            .collect::<std::collections::BTreeMap<_, _>>()
    };
    let (old, new) = (by_id(before), by_id(after));
    OutsideChange {
        added: new
            .keys()
            .filter(|id| !old.contains_key(id))
            .copied()
            .collect(),
        removed: old
            .keys()
            .filter(|id| !new.contains_key(id))
            .copied()
            .collect(),
        changed: new
            .iter()
            .filter(|(id, entity)| {
                old.get(id).is_some_and(|was| was != *entity)
            })
            .map(|(id, _)| *id)
            .collect(),
    }
}

/// Selects the entities with these scene IDs, so the Hierarchy and Scene
/// View highlight them. Leaves the selection alone when none exist.
fn highlight_scene_ids(
    world: &mut World,
    state: &mut EditorState,
    ids: &[uuid::Uuid],
) {
    let mut query = world.query::<(Entity, &SceneId)>();
    let entities: Vec<Entity> = query
        .iter(world)
        .filter(|(_, id)| ids.contains(&id.0))
        .map(|(entity, _)| entity)
        .collect();
    if let Some(&first) = entities.first() {
        state.selected = Some(first);
        state.selection = entities;
    }
}

/// Saves the scene, then the editor camera pose and selection beside it.
/// A failed metadata write does not fail the save. Refuses once with
/// [`crate::runtime::SceneIoError::Conflict`] when the file changed since the
/// editor loaded or saved it; saving again overwrites.
pub fn save_editor_scene(
    world: &mut World,
    state: &EditorState,
    path: &std::path::Path,
) -> Result<(), crate::runtime::SceneIoError> {
    if let Some(mut known) = world.get_resource_mut::<SceneFileRevision>() {
        if known.path.is_some() && known.path == path.canonicalize().ok() {
            if let Some(disk) = SceneFileRevision::read(path) {
                if disk.revision != known.revision {
                    *known = disk;
                    return Err(crate::runtime::SceneIoError::Conflict(
                        path.to_owned(),
                    ));
                }
            }
        }
    }
    save_scene(world, path, "Main Scene")?;
    SceneFileRevision::record(world, path);
    let camera = state
        .editor_camera
        .and_then(|camera| world.get::<Transform>(camera));
    let selection = state
        .selected
        .into_iter()
        .chain(state.selection.iter().copied())
        .filter_map(|entity| world.get::<SceneId>(entity).map(|id| id.0));
    let mut metadata = EditorSceneMetadata {
        camera_position: camera.map(|transform| transform.position),
        camera_rotation: camera.map(|transform| transform.rotation),
        orbit_distance: world
            .get_resource::<EditorFlyCamera>()
            .map(|fly| fly.orbit_distance),
        selection: Vec::new(),
    };
    for id in selection {
        if !metadata.selection.contains(&id) {
            metadata.selection.push(id);
        }
    }
    if let Ok(bytes) = serde_json::to_vec_pretty(&metadata) {
        let _ =
            crate::runtime::write_atomic(editor_metadata_path(path), &bytes);
    }
    Ok(())
}

/// Replaces the scene, then restores the editor camera pose and selection
/// saved beside it. A missing or unreadable sidecar leaves the camera as is
/// and clears the selection.
pub fn load_editor_scene(
    world: &mut World,
    state: &mut EditorState,
    path: &std::path::Path,
) -> Result<usize, crate::runtime::SceneIoError> {
    let count = load_scene(world, path, SceneLoadMode::Replace)?;
    SceneFileRevision::record(world, path);
    state.selected = None;
    state.selection.clear();
    let Some(metadata) = std::fs::read(editor_metadata_path(path))
        .ok()
        .and_then(|bytes| {
            serde_json::from_slice::<EditorSceneMetadata>(&bytes).ok()
        })
    else {
        return Ok(count);
    };
    if let Some(mut transform) = state
        .editor_camera
        .and_then(|camera| world.get_mut::<Transform>(camera))
    {
        if let Some(position) = metadata.camera_position {
            transform.position = position;
        }
        if let Some(rotation) = metadata.camera_rotation {
            transform.rotation = rotation;
        }
    }
    if let (Some(distance), Some(mut fly)) = (
        metadata.orbit_distance,
        world.get_resource_mut::<EditorFlyCamera>(),
    ) {
        fly.orbit_distance = distance;
    }
    let mut query = world.query::<(Entity, &SceneId)>();
    let by_id: std::collections::HashMap<_, _> = query
        .iter(world)
        .map(|(entity, id)| (id.0, entity))
        .collect();
    state.selection = metadata
        .selection
        .iter()
        .filter_map(|id| by_id.get(id).copied())
        .collect();
    state.selected = state.selection.first().copied();
    Ok(count)
}

/// Applies the editor's compact dark workspace theme to an egui context.
/// Also restores the user's saved UI scale and text size. The OS display
/// scale comes from the window through egui-winit and multiplies both.
pub fn configure_editor_style(context: &Context) {
    let preferences = EditorPreferences::load();
    gui_elements::EditorTheme::apply(context);
    context.set_zoom_factor(preferences.ui_scale);
    apply_font_scale(context, preferences.font_scale);
}

/// Sets every egui text style to `scale` times its default size.
/// ponytail: widgets that build their own `FontId` keep a fixed size; route
/// them through text styles if they must follow this setting.
pub fn apply_font_scale(context: &Context, scale: f32) {
    let defaults = egui::Style::default().text_styles;
    context.style_mut(|style| {
        for (text_style, font) in &mut style.text_styles {
            if let Some(default) = defaults.get(text_style) {
                font.size = default.size * scale;
            }
        }
    });
}

/// Tile Painter tools, as in Godot's tile map editor.
#[derive(
    Clone,
    Copy,
    Debug,
    Default,
    PartialEq,
    Eq,
    Hash,
    serde::Serialize,
    serde::Deserialize,
)]
pub enum TileTool {
    /// Clicks and drags paint the cells under the pointer.
    #[default]
    Paint,
    /// A drag fills the rectangle between the pressed and released cells.
    Rectangle,
    /// A click flood fills the joined cells that match the clicked one.
    Fill,
    /// A drag paints a straight line of cells from the pressed cell to the
    /// released one.
    Line,
}

/// Editor interaction mode. Edit state never advances gameplay fixed updates.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum EditorMode {
    #[default]
    Edit,
    Play,
    Paused,
}

/// Cargo profile used when the editor starts the native game.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum GameBuildProfile {
    /// Compiles quickly and keeps debug information for normal development.
    #[default]
    Debug,
    /// Takes longer to compile, but enables the game's optimizations.
    Release,
}

impl GameBuildProfile {
    /// Short name shown in the editor's Play selector.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Debug => "Debug",
            Self::Release => "Release",
        }
    }

    /// Cargo flag needed by this profile. Debug is Cargo's default.
    const fn cargo_flag(self) -> Option<&'static str> {
        match self {
            Self::Debug => None,
            Self::Release => Some("--release"),
        }
    }
}

/// A Scene View handle that edits one component field when dragged.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum SceneHandle {
    /// A fog height square: `main` for the one at `height`, else the
    /// falloff square. `anchor` is the grabbed point on its edge.
    FogHeight { main: bool, anchor: [f32; 3] },
    /// A reflection probe box face on world `axis` (0 = X).
    ProbeFace { axis: usize },
    /// A `RenderBounds` handle from `render_bounds_handles`: a box face
    /// or a point on the sphere.
    BoundsFace { index: usize },
}

/// Internal mode of the first live viewport. Areas use [`EditorPanel`] for
/// their independently selected content.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum EditorWorkspace {
    #[default]
    Scene,
    Game,
    Code,
}

/// Persistent editor selection, project state, and area layout.
#[derive(Resource, Clone, Debug)]
pub struct EditorState {
    /// Primary (active) selected object: the gizmo and Inspector act on it.
    pub selected: Option<Entity>,
    /// Every selected object. Always contains `selected` when it is set; the
    /// Hierarchy resets it to just `selected` when the two disagree.
    pub selection: Vec<Entity>,
    /// Name filter typed into the Hierarchy search box.
    pub hierarchy_filter: String,
    /// Tile character that Scene View clicks paint into the selected tile
    /// map; `.` erases. `None` leaves clicks to selection and the gizmo.
    pub tile_brush: Option<char>,
    /// Scene before the current tile stroke; it becomes an Undo step only
    /// once the stroke changes a cell.
    pub tile_stroke_undo: Option<SceneDocument>,
    /// How Scene View strokes apply the tile brush.
    pub tile_tool: TileTool,
    /// Particle Play/Pause/Restart/Stop pressed in the Inspector, sent to
    /// the emitter after drawing. Transport is not a scene edit.
    pub particle_command: Option<(Entity, crate::runtime::ParticleCommand)>,
    /// Clip, playhead, and record state of the Timeline area.
    pub timeline: timeline::TimelineState,
    /// Pressed cell of a Rectangle or Line drag.
    pub tile_rect_start: Option<(usize, usize)>,
    /// Scene View handle being dragged, and the entity it edits.
    pub scene_handle: Option<(Entity, SceneHandle)>,
    /// Tells the editor if the game is stopped, playing, or paused.
    pub mode: EditorMode,
    /// Chooses a fast Debug build or an optimized Release build for Play.
    pub game_build_profile: GameBuildProfile,
    /// Scene path relative to the selected project folder.
    pub scene_path: String,
    /// Folder that contains the current game project.
    pub project_root: String,
    /// Last save, load, layout, or component message.
    pub scene_message: Option<String>,
    /// True when the scene changed after its last successful save or load.
    pub scene_dirty: bool,
    /// Editable name copied from the selected object.
    pub rename_draft: String,
    /// Object currently showing an inline rename field in Hierarchy.
    pub rename_target: Option<Entity>,
    /// Whether the centered Add Object catalog is visible.
    pub add_object_modal_open: bool,
    /// Parent assigned to the next object created through the catalog.
    pub add_object_parent: Option<Entity>,
    /// New class name being typed in the Inspector.
    pub class_draft: String,
    /// Text being edited for custom serialized components.
    /// Camera mode used by the first live 3D area.
    pub workspace: EditorWorkspace,
    /// Camera used by Scene View instead of the game's camera.
    pub editor_camera: Option<Entity>,
    /// Copy of the scene restored when Stop is pressed.
    pub play_snapshot: Option<SceneDocument>,
    /// All entities that existed before embedded preview started.
    pub preview_entities: Option<std::collections::HashSet<Entity>>,
    /// Dirty flag to restore after transient preview edits are discarded.
    pub preview_scene_dirty: Option<bool>,
    /// Source path relative to the selected project folder.
    pub code_path: String,
    /// Editable text loaded from `code_path`.
    pub code_source: String,
    /// Last message created by the Code Editor.
    pub code_message: Option<String>,
    /// True when code text changed but was not saved.
    pub code_dirty: bool,
    /// Tree that stores every editor area and divider.
    pub dock_layout: EditorDockNode,
    /// Area changed by the main toolbar and marked with a blue border.
    pub active_area: u64,
    /// Unique ID reserved for the next split area.
    pub next_area_id: u64,
}

/// egui texture that shows the offscreen image the live 3D view renders into.
pub const SCENE_VIEW_TEXTURE: egui::TextureId = egui::TextureId::User(0);

/// Physical pixel rectangle occupied by the editor's live scene view.
///
/// This is intentionally editor-owned layout data. The window runner converts
/// it into a renderer viewport, keeping egui out of the rendering API.
#[derive(Resource, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EditorViewport {
    /// Top-left viewport position in physical window pixels.
    pub offset: [u32; 2],
    /// Width and height in physical window pixels.
    pub extent: [u32; 2],
    /// False when the layout does not contain a live 3D area.
    pub valid: bool,
    /// True when the pointer is over the view and no egui popup, menu, or
    /// viewport toolbar covers it.
    pub hovered: bool,
}

/// Options for helpers visible only while authoring in the Scene View.
///
/// These are not scene components. Saving a scene or running a cooked game
/// never includes a grid, selection axes, or any other editor helper.
#[derive(Resource, Clone, Copy, Debug, PartialEq)]
pub struct EditorGizmoSettings {
    /// Draw a ground grid on the XZ plane.
    pub show_grid: bool,
    /// Draw red X, green Y, and blue Z axes on the selected object.
    pub show_selected_axes: bool,
    /// Draw a yellow box around the selected render mesh.
    pub show_selected_bounds: bool,
    /// Diagnostic shading of the Scene View; Play always renders lit.
    pub shading: SceneDebugView,
    /// Atmosphere effects the Scene View draws; Play draws all of them.
    pub effects: SceneEffects,
}

#[derive(
    Clone,
    Copy,
    Debug,
    PartialEq,
    Eq,
    Hash,
    serde::Serialize,
    serde::Deserialize,
)]
pub enum GizmoAxis {
    X,
    Y,
    Z,
}

/// Translation drag retained across egui frames.
#[derive(Resource, Clone, Debug, Default)]
pub struct EditorGizmoDrag {
    pub mode: Option<TransformModes>,
    pub snap_enabled: bool,
    pub snap_steps: shortcuts::SnapSteps,
    pub global_axes: bool,
    /// Squared projection of each world scale axis onto local scale axes.
    pub global_scale_weights: [[f32; 3]; 3],
    pub modal: bool,
    pub axis_mask: [bool; 3],
    pub active_axis: Option<GizmoAxis>,
    pub entity: Option<Entity>,
    pub start_pointer: Option<egui::Pos2>,
    pub original_transform: Option<Transform>,
    pub world_axis: [f32; 3],
    pub local_delta_axis: [f32; 3],
    pub screen_axis: [f32; 2],
    pub pixels_per_world_unit: f32,
    pub gizmo_axis_length: f32,
    pub origin_screen: Option<egui::Pos2>,
    pub world_axes: [[f32; 3]; 3],
    pub local_delta_axes: [[f32; 3]; 3],
    pub screen_vectors: [[f32; 2]; 3],
    pub rotation_screen_signs: [f32; 3],
    pub view_rotation_axis: [f32; 3],
    pub move_axis_origin: [f32; 3],
    pub move_axis_direction: [f32; 3],
    pub move_drag_plane_normal: [f32; 3],
    pub move_start_axis_parameter: Option<f32>,
    undo_snapshot: Option<SceneDocument>,
}

impl EditorGizmoDrag {
    #[must_use]
    pub fn is_active(&self) -> bool {
        self.mode.is_some()
    }
}

/// Shading the renderer uses this frame: the Scene View's chosen shading,
/// or [`SceneDebugView::Lit`] in every other workspace.
pub fn editor_debug_view(world: &World) -> SceneDebugView {
    if world.resource::<EditorState>().workspace == EditorWorkspace::Scene {
        world.resource::<EditorGizmoSettings>().shading
    } else {
        SceneDebugView::Lit
    }
}

/// Atmosphere effects the renderer draws this frame: the Scene View's
/// toggles, or all of them in every other workspace.
pub fn editor_scene_effects(world: &World) -> SceneEffects {
    if world.resource::<EditorState>().workspace == EditorWorkspace::Scene {
        world.resource::<EditorGizmoSettings>().effects
    } else {
        SceneEffects::default()
    }
}

impl Default for EditorGizmoSettings {
    fn default() -> Self {
        Self {
            show_grid: true,
            show_selected_axes: true,
            show_selected_bounds: true,
            shading: SceneDebugView::Lit,
            effects: SceneEffects::default(),
        }
    }
}

/// Editor-built geometry consumed by the renderer for the current frame.
#[derive(Resource, Clone, Debug, Default, PartialEq)]
pub struct EditorDebugOverlay(pub RenderDebugOverlay);

/// Scene snapshots used by Undo and Redo.
#[derive(Resource, Clone, Debug, Default)]
struct EditorHistory {
    /// Scene states restored by Undo, newest last.
    undo: std::collections::VecDeque<SceneDocument>,
    /// Scene states restored by Redo, newest last.
    redo: Vec<SceneDocument>,
    /// Scene state from before the current continuous Inspector edit.
    pending_inspector: Option<SceneDocument>,
    preview_before: Option<Box<EditorHistory>>,
}

impl EditorHistory {
    /// Keeps history useful without growing memory forever.
    const MAX_SNAPSHOTS: usize = 100;

    fn push_undo(&mut self, document: SceneDocument) {
        if self.undo.back() != Some(&document) {
            self.undo.push_back(document);
            if self.undo.len() > Self::MAX_SNAPSHOTS {
                self.undo.pop_front();
            }
        }
        self.redo.clear();
    }
}

impl Default for EditorState {
    fn default() -> Self {
        Self {
            selected: None,
            selection: Vec::new(),
            hierarchy_filter: String::new(),
            tile_brush: None,
            tile_stroke_undo: None,
            tile_tool: TileTool::Paint,
            particle_command: None,
            timeline: timeline::TimelineState::default(),
            tile_rect_start: None,
            scene_handle: None,
            mode: EditorMode::Edit,
            game_build_profile: GameBuildProfile::Debug,
            scene_path: String::new(),
            project_root: String::new(),
            scene_message: None,
            scene_dirty: false,
            rename_draft: String::new(),
            rename_target: None,
            add_object_modal_open: false,
            add_object_parent: None,
            class_draft: String::new(),
            workspace: EditorWorkspace::Scene,
            editor_camera: None,
            play_snapshot: None,
            preview_entities: None,
            preview_scene_dirty: None,
            code_path: "src/main.rs".into(),
            code_source: String::new(),
            code_message: None,
            code_dirty: false,
            dock_layout: EditorDockNode::default_layout(),
            active_area: 2,
            next_area_id: 5,
        }
    }
}

/// Switches every editor document path to one validated project.
fn set_open_project_paths(state: &mut EditorState, project: &OpenProject) {
    state.project_root = project.root.display().to_string();
    state.scene_path = editor_relative_path(&project.manifest.main_scene);
    let code_path = project
        .code_path
        .strip_prefix(&project.root)
        .unwrap_or(std::path::Path::new("src/main.rs"));
    state.code_path = editor_relative_path(code_path);
    // Never show text left over from the previously opened project.
    state.code_source.clear();
    state.code_dirty = false;
    state.rename_target = None;
}

/// Structured editor console entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConsoleEntry {
    /// Controls the color and importance of this message.
    pub level: ConsoleLevel,
    /// Part of the editor or engine that reported it, such as "Assets".
    pub source: &'static str,
    /// Text shown in the Console area.
    pub message: String,
    /// How many times in a row the same message arrived.
    pub count: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConsoleLevel {
    /// Normal status message.
    Info,
    /// Something may be wrong, but the editor can continue.
    Warning,
    /// An operation failed.
    Error,
}

#[derive(Resource, Clone, Debug, Default)]
pub struct EditorConsole {
    /// Messages kept in the order they were added.
    entries: Vec<ConsoleEntry>,
    /// Console area text filter.
    filter: String,
    /// Levels the Console area hides, indexed by `ConsoleLevel as usize`.
    hidden: [bool; 3],
    /// Scene and Code status lines already copied into the console.
    status_seen: [Option<String>; 2],
}

impl EditorConsole {
    /// Oldest messages are dropped past this count.
    pub const CAPACITY: usize = 2000;

    /// Adds one editor message to the bottom of the Console area.
    pub fn push(&mut self, level: ConsoleLevel, message: impl Into<String>) {
        self.push_from(level, "Editor", message);
    }

    /// Adds one message from `source`. A repeat of the last message only
    /// raises its count.
    pub fn push_from(
        &mut self,
        level: ConsoleLevel,
        source: &'static str,
        message: impl Into<String>,
    ) {
        let message = message.into();
        if let Some(last) = self.entries.last_mut() {
            if last.level == level
                && last.source == source
                && last.message == message
            {
                last.count += 1;
                return;
            }
        }
        if self.entries.len() == Self::CAPACITY {
            self.entries.remove(0);
        }
        self.entries.push(ConsoleEntry {
            level,
            source,
            message,
            count: 1,
        });
    }

    #[must_use]
    pub fn entries(&self) -> &[ConsoleEntry] {
        &self.entries
    }

    /// Removes every old console message.
    pub fn clear(&mut self) {
        self.entries.clear();
    }
}

/// Installs editor state, viewport data, and the message console.
///
/// Add this after render extraction when building an editor application:
///
/// ```no_run
/// # use rusting_engine::{App, EditorPlugin};
/// # use rusting_engine::runtime::RenderExtractPlugin;
/// let mut app = App::new();
/// app.add_plugin(RenderExtractPlugin)?;
/// app.add_plugin(EditorPlugin)?;
/// # Ok::<(), rusting_engine::runtime::AppError>(())
/// ```
#[derive(Clone, Copy, Debug, Default)]
pub struct EditorPlugin;

impl Plugin for EditorPlugin {
    fn build(&self, app: &mut App) -> Result<(), AppError> {
        // These resources keep GUI data inside the same ECS world as the game.
        app.insert_resource(EditorState::default())
            .insert_resource(EditorViewport::default())
            .insert_resource(EditorGizmoSettings::default())
            .insert_resource(EditorGizmoDrag::default())
            .insert_resource(EditorDebugOverlay::default())
            .insert_resource(EditorShortcuts::default())
            .insert_resource(EditorCommandQueue::default())
            .insert_resource(InspectorRegistry::default())
            .insert_resource(EditorTransformMode::default())
            .insert_resource(EditorFlyCamera::default())
            .insert_resource(EditorConsole::default())
            .insert_resource(agent_panel::AgentJournal::default())
            .insert_resource(collab_panel::CollabState::default())
            .insert_resource(profiler::EditorProfiler::default())
            .insert_resource(EditorHistory::default())
            .insert_resource(PendingDestructiveAction::default())
            .insert_resource(EditorAssetState::default())
            .insert_resource(EditorBuildState::default())
            .insert_resource(ProjectManagerState::default())
            .insert_resource(FileDialogs::default());
        // The editor does not link the game's plugins; keep their
        // components as saved JSON so opening and saving a scene keeps them.
        app.world_mut()
            .resource_mut::<crate::runtime::SceneComponentRegistry>()
            .keep_unregistered();
        Ok(())
    }
}

/// Work requested by one Project Manager button.
#[derive(Clone)]
enum ProjectRequest {
    /// Creates a project inside the selected parent folder.
    Create {
        parent: PathBuf,
        name: String,
        template: ProjectTemplate,
    },
    /// Opens a folder that already contains a RustingEngine project.
    Open(PathBuf),
}

/// Action held while the editor asks what to do with unsaved changes.
#[derive(Clone)]
enum DestructiveRequest {
    NewScene,
    ReloadScene,
    OpenScene(PathBuf),
    OpenProject(ProjectRequest),
}

/// Keeps a confirmation request alive between GUI frames.
#[derive(Resource, Clone, Default)]
struct PendingDestructiveAction {
    request: Option<DestructiveRequest>,
}

/// Assets area filter, tree state, and its last status message.
#[derive(Resource, Clone, Default)]
struct EditorAssetState {
    /// Text used to filter project asset paths.
    filter: String,
    /// Highlighted file or folder in the Assets tree.
    selected: Option<PathBuf>,
    /// Folders folded shut in the Assets tree.
    collapsed: std::collections::HashSet<PathBuf>,
    /// Result of the latest import or assignment.
    message: Option<String>,
    /// Cached `project_asset_files` result, so the disk is not walked every
    /// frame.
    files: Vec<PathBuf>,
    /// Project root and time of the last scan; `None` forces a rescan.
    files_scanned: Option<(String, std::time::Instant)>,
    /// Image previews by path; `None` marks a file that failed to decode.
    thumbnails: std::collections::HashMap<PathBuf, Option<egui::TextureHandle>>,
    /// Imported asset a Replace dialog is choosing a new file for.
    replace_target: Option<PathBuf>,
    /// Dry-run result shown for confirmation before a replacement.
    replace_preview: Option<ReplacePreview>,
    /// Data asset file the Inspector shows instead of the selected object.
    inspected: Option<DataInspection>,
}

/// A `.rdata` file open in the Inspector.
#[derive(Clone)]
struct DataInspection {
    path: PathBuf,
    type_name: String,
    /// The file's value in scene form, as the Inspector edits it.
    value: serde_json::Value,
    /// Hierarchy selection when the file was opened. Selecting another
    /// object returns the Inspector to objects.
    selection: Option<Entity>,
    /// Edited since the last write.
    dirty: bool,
    /// Why the last read or write failed.
    error: Option<String>,
}

impl DataInspection {
    fn open(path: PathBuf, selection: Option<Entity>) -> Self {
        let (type_name, value, error) =
            match crate::assets::read_data_file(&path) {
                Ok((type_name, value)) => (type_name, value, None),
                Err(error) => (
                    String::new(),
                    serde_json::Value::Null,
                    Some(error.to_string()),
                ),
            };
        Self {
            path,
            type_name,
            value,
            selection,
            dirty: false,
            error,
        }
    }
}

/// Writes a new data asset of `type_name` with its default value in
/// `folder`, named after the type (`enemy_stats.rdata`,
/// `enemy_stats_2.rdata`, ...).
fn new_data_asset(
    folder: &std::path::Path,
    registered: &crate::assets::DataAssetType,
) -> Result<PathBuf, String> {
    let stem = registered
        .name
        .rsplit('.')
        .next()
        .filter(|stem| !stem.is_empty())
        .unwrap_or("data");
    let path = (1..)
        .map(|number| {
            let suffix = if number == 1 {
                String::new()
            } else {
                format!("_{number}")
            };
            folder.join(format!(
                "{stem}{suffix}.{}",
                crate::assets::DATA_EXTENSION
            ))
        })
        .find(|path| !path.exists())
        .expect("a free file name always exists");
    std::fs::create_dir_all(folder).map_err(|error| error.to_string())?;
    crate::assets::write_data_file(
        &path,
        registered.name,
        registered.default_value(),
    )
    .map_err(|error| error.to_string())?;
    Ok(path)
}

/// A checked but unwritten asset replacement.
#[derive(Clone)]
struct ReplacePreview {
    target: PathBuf,
    source: PathBuf,
    report: Result<crate::asset_import::ImportReport, String>,
}

impl EditorAssetState {
    /// Rescans the project's `assets` folder when the project changed or the
    /// cached list is older than one second.
    fn refresh_files(&mut self, project_root: &str) {
        let fresh = self.files_scanned.as_ref().is_some_and(|(root, time)| {
            root == project_root
                && time.elapsed() < std::time::Duration::from_secs(1)
        });
        if !fresh {
            self.files = project_asset_files(project_root);
            self.files_scanned =
                Some((project_root.to_owned(), std::time::Instant::now()));
        }
    }
}

/// Cargo work that can run without freezing the editor window.
#[derive(Clone)]
enum BuildRequest {
    Check,
    BuildAndRun {
        /// Profile selected beside the editor's Play button.
        profile: GameBuildProfile,
    },
    Export {
        parent: PathBuf,
        project_name: String,
        binary_name: String,
        cooked_scene: PathBuf,
        /// Cargo target triple to cross-build for; `None` builds for this
        /// system.
        target: Option<&'static str>,
    },
}

/// Windows target used when exporting from another system. The GNU flavor
/// only needs `rustup target add` and mingw-w64, not the MSVC tools.
pub(super) const WINDOWS_TARGET: &str = "x86_64-pc-windows-gnu";

/// Result sent from the Cargo worker back to the main editor thread.
struct BuildFinished {
    success: bool,
    output: String,
}

/// One update sent by the compiler or running game to the editor.
enum BuildWorkerMessage {
    /// Text that should appear immediately in Cargo Output.
    Output(String),
    /// Final status after Cargo or the native game exits.
    Finished(BuildFinished),
}

/// Current compiler state displayed below the Rust Code Editor.
#[derive(Resource, Default)]
struct EditorBuildState {
    /// True while Cargo is checking or compiling the project.
    running: bool,
    /// Combined Cargo output kept for the Code and Console areas.
    output: String,
    /// Byte position already copied into the Console panel.
    console_cursor: usize,
    /// Receiver is locked only because ECS resources must be thread-safe.
    receiver:
        std::sync::Mutex<Option<std::sync::mpsc::Receiver<BuildWorkerMessage>>>,
    /// Set by Stop; the worker kills its current process when it sees it.
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// Set by Reload Code; the worker asks the running game to save its
    /// scene and exit.
    save_state: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// The next Play continues from the scene the game saved.
    code_reload: bool,
    /// The current Play continues from a saved scene.
    resuming: bool,
    /// Restart only after the worker confirms its process has exited.
    restart_requested: bool,
    restart_ready: bool,
    /// When Play or Restart started a build; the scene was saved just before.
    play_started: Option<std::time::Instant>,
    /// Build time of the current Play, once Cargo finished.
    built_after: Option<std::time::Duration>,
    /// Timing and per-file error messages for the Console.
    notices: Vec<(ConsoleLevel, &'static str, String)>,
}

impl EditorBuildState {
    /// Starts one Cargo task. A second click is ignored until it finishes.
    fn start(
        &mut self,
        project_root: PathBuf,
        request: BuildRequest,
    ) -> Result<(), String> {
        if self.running {
            return Err("A Cargo task is already running".into());
        }
        let manifest = project_root.join("Cargo.toml");
        if !manifest.is_file() {
            return Err(format!("{} is missing", manifest.display()));
        }
        let (sender, receiver) = std::sync::mpsc::channel();
        *self.receiver.get_mut().map_err(|error| error.to_string())? =
            Some(receiver);
        self.running = true;
        self.output = match &request {
            BuildRequest::Check => "Running cargo check...".into(),
            BuildRequest::BuildAndRun { profile } => {
                format!("Building {} game...", profile.label())
            }
            BuildRequest::Export { target: None, .. } => {
                "Building game for export...".into()
            }
            BuildRequest::Export {
                target: Some(target),
                ..
            } => format!(
                "Building game for {target} export. This needs \
                 `rustup target add {target}` and a matching linker \
                 (mingw-w64 for Windows)...\n"
            ),
        };
        self.console_cursor = 0;
        let play = matches!(request, BuildRequest::BuildAndRun { .. });
        self.play_started = play.then(std::time::Instant::now);
        self.built_after = None;
        self.resuming = std::mem::take(&mut self.code_reload) && play;
        if play && !self.resuming {
            let _ = std::fs::remove_file(code_reload_state_path(&project_root));
        }
        self.stop = std::sync::Arc::default();
        let stop = std::sync::Arc::clone(&self.stop);
        self.save_state = std::sync::Arc::default();
        let save_state = std::sync::Arc::clone(&self.save_state);
        std::thread::spawn(move || {
            let command = match &request {
                BuildRequest::Check => "check",
                BuildRequest::BuildAndRun { .. }
                | BuildRequest::Export { .. } => "build",
            };
            let profile = match &request {
                BuildRequest::BuildAndRun { profile } => Some(*profile),
                BuildRequest::Export { .. } => Some(GameBuildProfile::Release),
                BuildRequest::Check => None,
            };
            let mut cargo = std::process::Command::new("cargo");
            cargo.arg(command);
            if let Some(flag) = profile.and_then(GameBuildProfile::cargo_flag) {
                cargo.arg(flag);
            }
            cargo
                .args(["--message-format", "short", "--manifest-path"])
                .arg(&manifest)
                .current_dir(&project_root);
            if let BuildRequest::Export {
                target: Some(target),
                ..
            } = &request
            {
                cargo.args(["--target", target]);
                if target.contains("windows-gnu") {
                    // Players should not get a console window beside the
                    // game.
                    cargo.env(
                        format!(
                            "CARGO_TARGET_{}_RUSTFLAGS",
                            target.to_uppercase().replace('-', "_")
                        ),
                        "-Clink-arg=-mwindows",
                    );
                }
            }
            let finished = match run_streamed(&mut cargo, &sender, &stop, None)
            {
                Ok(None) => BuildFinished {
                    success: false,
                    output: "\nCargo task stopped.\n".into(),
                },
                Ok(Some(status)) => {
                    let mut success = status.success();
                    if success {
                        if let BuildRequest::BuildAndRun { profile } = &request
                        {
                            return run_native_game(
                                &project_root,
                                &manifest,
                                *profile,
                                sender,
                                &stop,
                                &save_state,
                            );
                        } else if let BuildRequest::Export {
                            parent,
                            project_name,
                            binary_name,
                            cooked_scene,
                            target,
                        } = &request
                        {
                            match export_built_game(
                                &project_root,
                                &manifest,
                                parent,
                                project_name,
                                binary_name,
                                cooked_scene,
                                *target,
                            ) {
                                Ok(path) => {
                                    let _ = sender.send(
                                        BuildWorkerMessage::Output(format!(
                                            "\nExported game to {}\n",
                                            path.display()
                                        )),
                                    );
                                }
                                Err(error) => {
                                    success = false;
                                    let _ = sender.send(
                                        BuildWorkerMessage::Output(format!(
                                            "\nBuild passed, but export failed: {error}\n"
                                        )),
                                    );
                                }
                            }
                        }
                    }
                    BuildFinished {
                        success,
                        output: if success {
                            "\nCargo task finished successfully.\n".into()
                        } else {
                            "\nCargo task failed.\n".into()
                        },
                    }
                }
                Err(error) => BuildFinished {
                    success: false,
                    output: format!("Could not start Cargo: {error}"),
                },
            };
            let _ = sender.send(BuildWorkerMessage::Finished(finished));
        });
        Ok(())
    }

    /// Asks the worker to kill the running Cargo task or native game.
    fn request_stop(&mut self) {
        self.restart_requested = false;
        self.code_reload = false;
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
    }

    fn request_restart(&mut self) {
        if self.running {
            self.restart_requested = true;
            self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
        }
    }

    /// Asks the running game to save its scene and exit, then rebuilds and
    /// starts it again from that scene. While Cargo is still building, this
    /// restarts the build.
    fn request_code_reload(&mut self) {
        if !self.running {
            return;
        }
        self.restart_requested = true;
        self.code_reload = true;
        let flag = if self.built_after.is_some() {
            &self.save_state
        } else {
            &self.stop
        };
        flag.store(true, std::sync::atomic::Ordering::Relaxed);
    }

    fn take_restart_ready(&mut self) -> bool {
        std::mem::take(&mut self.restart_ready)
    }

    /// Receives new compiler and game output without blocking the editor.
    fn poll(&mut self) -> Option<bool> {
        let mut messages = Vec::new();
        let mut disconnected = false;
        {
            let receiver = self.receiver.get_mut().ok()?.as_ref()?;
            loop {
                match receiver.try_recv() {
                    Ok(message) => messages.push(message),
                    Err(std::sync::mpsc::TryRecvError::Empty) => break,
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                        disconnected = true;
                        break;
                    }
                }
            }
        }

        let mut finished_status = None;
        for message in messages {
            match message {
                BuildWorkerMessage::Output(text) => {
                    self.note_output(&text);
                    self.append_output(&text);
                }
                BuildWorkerMessage::Finished(finished) => {
                    self.append_output(&finished.output);
                    self.running = false;
                    if !finished.success && self.built_after.is_none() {
                        self.note_build_errors();
                        if self.resuming && !self.restart_requested {
                            self.code_reload = true;
                            self.notices.push((
                                ConsoleLevel::Warning,
                                "Play",
                                "The reloaded code did not build. The game's \
                                 saved scene is kept: fix the error and press \
                                 Play to continue from it."
                                    .into(),
                            ));
                        }
                    }
                    self.play_started = None;
                    finished_status = Some(finished.success);
                }
            }
        }
        if finished_status.is_some() {
            self.restart_ready = std::mem::take(&mut self.restart_requested);
            *self.receiver.get_mut().ok()? = None;
            finished_status
        } else if disconnected {
            if self.running {
                self.running = false;
                self.restart_ready =
                    std::mem::take(&mut self.restart_requested);
                self.append_output("\nCargo worker stopped unexpectedly.\n");
                *self.receiver.get_mut().ok()? = None;
                Some(false)
            } else {
                None
            }
        } else {
            None
        }
    }

    /// Records build time, the game's first playable frame, and asset
    /// reload failures from one piece of worker output.
    fn note_output(&mut self, text: &str) {
        let Some(started) = self.play_started else {
            return;
        };
        if text.contains(BUILD_PASSED) {
            self.built_after = Some(started.elapsed());
        }
        for line in text.lines() {
            if line.starts_with(crate::project::CODE_RELOAD_KEPT_MARKER) {
                self.notices.push((
                    ConsoleLevel::Info,
                    "Play",
                    "Code reload kept the scene".into(),
                ));
            } else if let Some(reason) =
                line.strip_prefix(crate::project::CODE_RELOAD_CLEAN_MARKER)
            {
                self.notices.push((
                    ConsoleLevel::Warning,
                    "Play",
                    format!("Code reload restarted the game clean:{reason}"),
                ));
            }
        }
        if let Some(game_ms) = crate::project::first_frame_ms(text) {
            self.notices.push((
                ConsoleLevel::Info,
                "Play",
                format!(
                    "First playable frame {:.2} s after save (build {:.2} s, \
                     game startup {game_ms} ms)",
                    started.elapsed().as_secs_f64(),
                    self.built_after.unwrap_or_default().as_secs_f64(),
                ),
            ));
        }
        for failure in crate::project::reload_diagnostics(text) {
            if failure.kind == "asset" {
                self.notices.push((
                    ConsoleLevel::Error,
                    "Assets",
                    failure.message,
                ));
            }
        }
    }

    /// Adds one Console error per Rust or shader error in the build output.
    fn note_build_errors(&mut self) {
        for error in crate::project::reload_diagnostics(&self.output) {
            let (source, location) = match error.kind {
                "shader" => ("Shader", error.file),
                "rust" => ("Rust", error.file),
                _ => continue,
            };
            let location = match (location, error.line) {
                (Some(file), Some(line)) => {
                    format!("{}:{line}: ", file.display())
                }
                (Some(file), None) => format!("{}: ", file.display()),
                _ => String::new(),
            };
            self.notices.push((
                ConsoleLevel::Error,
                source,
                format!("{location}{}", error.message),
            ));
        }
    }

    fn take_notices(&mut self) -> Vec<(ConsoleLevel, &'static str, String)> {
        std::mem::take(&mut self.notices)
    }

    /// Keeps recent output while preventing an unlimited memory allocation.
    fn append_output(&mut self, text: &str) {
        const MAX_OUTPUT_BYTES: usize = 500_000;
        self.output.push_str(text);
        if self.output.len() > MAX_OUTPUT_BYTES {
            let mut remove = self.output.len() - MAX_OUTPUT_BYTES;
            while !self.output.is_char_boundary(remove) {
                remove += 1;
            }
            const MARKER: &str = "[older output truncated]\n";
            self.output.drain(..remove);
            self.output.insert_str(0, MARKER);
            // Keep the Console cursor on the same text; unsent text that was
            // dropped restarts the Console at the marker.
            self.console_cursor = match self.console_cursor.checked_sub(remove)
            {
                Some(cursor) => cursor + MARKER.len(),
                None => 0,
            };
        }
    }

    /// Returns build text that has not yet been sent to the Console panel.
    fn take_console_output(&mut self) -> String {
        let text = self.output[self.console_cursor..].to_owned();
        self.console_cursor = self.output.len();
        text
    }
}

/// Output line the worker sends when the game build succeeded.
const BUILD_PASSED: &str = "Build passed.";

/// File that carries a running game's scene across a code reload.
fn code_reload_state_path(project_root: &std::path::Path) -> PathBuf {
    project_root.join("build").join("code_reload_state.json")
}

/// How long a game may take to save its scene before it is killed.
const CODE_RELOAD_SAVE_TIMEOUT: std::time::Duration =
    std::time::Duration::from_secs(10);

/// Starts the native game and forwards both output streams to the editor.
fn run_native_game(
    project_root: &std::path::Path,
    manifest: &std::path::Path,
    profile: GameBuildProfile,
    sender: std::sync::mpsc::Sender<BuildWorkerMessage>,
    stop: &std::sync::atomic::AtomicBool,
    save_state: &std::sync::atomic::AtomicBool,
) {
    let _ = sender.send(BuildWorkerMessage::Output(format!(
        "\n{BUILD_PASSED} Starting native game...\n"
    )));
    // On Unix `cargo run` replaces itself with the game, so Stop kills the
    // game; on Windows Cargo's job object takes the game down with it.
    let mut cargo = std::process::Command::new("cargo");
    cargo.arg("run");
    if let Some(flag) = profile.cargo_flag() {
        cargo.arg(flag);
    }
    cargo
        .arg("--manifest-path")
        .arg(manifest)
        .current_dir(project_root)
        .env(
            crate::project::CODE_RELOAD_STATE_ENV,
            code_reload_state_path(project_root),
        );
    let finished =
        match run_streamed(&mut cargo, &sender, stop, Some(save_state)) {
            Ok(Some(status)) if status.success() => BuildFinished {
                success: true,
                output: "\nNative game exited successfully.\n".into(),
            },
            Ok(Some(status)) => BuildFinished {
                success: false,
                output: format!("\nNative game exited with status {status}.\n"),
            },
            Ok(None) => BuildFinished {
                success: false,
                output: "\nNative game stopped.\n".into(),
            },
            Err(error) => BuildFinished {
                success: false,
                output: format!("\nCould not run native game: {error}\n"),
            },
        };
    let _ = sender.send(BuildWorkerMessage::Finished(finished));
}

/// Runs one command and sends each stdout and stderr line to the editor as
/// it arrives. Returns `None` when `stop` was set and the process was killed.
/// With `save_state`, the command's standard input is a pipe; setting the
/// flag sends the game [`crate::project::CODE_RELOAD_SAVE_COMMAND`] and kills
/// it if it has not exited after [`CODE_RELOAD_SAVE_TIMEOUT`].
// ponytail: killing `cargo build` can leave its rustc children running until
// they finish; add a process group kill if that becomes a problem.
fn run_streamed(
    command: &mut std::process::Command,
    sender: &std::sync::mpsc::Sender<BuildWorkerMessage>,
    stop: &std::sync::atomic::AtomicBool,
    save_state: Option<&std::sync::atomic::AtomicBool>,
) -> std::io::Result<Option<std::process::ExitStatus>> {
    use std::io::{BufRead, Write};
    use std::process::Stdio;

    if save_state.is_some() {
        command.stdin(Stdio::piped());
    }
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let mut stdin = child.stdin.take();
    let mut save_requested: Option<std::time::Instant> = None;
    let pipes: [Option<Box<dyn std::io::Read + Send>>; 2] = [
        child.stdout.take().map(|pipe| Box::new(pipe) as _),
        child.stderr.take().map(|pipe| Box::new(pipe) as _),
    ];
    let readers: Vec<_> = pipes
        .into_iter()
        .flatten()
        .map(|pipe| {
            let sender = sender.clone();
            std::thread::spawn(move || {
                for line in std::io::BufReader::new(pipe).lines() {
                    let text = match line {
                        Ok(line) => format!("{line}\n"),
                        Err(error) => {
                            format!("Could not read output: {error}\n")
                        }
                    };
                    let _ = sender.send(BuildWorkerMessage::Output(text));
                }
            })
        })
        .collect();

    let status = loop {
        if let Some(status) = child.try_wait()? {
            break Some(status);
        }
        if stop.load(std::sync::atomic::Ordering::Relaxed) {
            let _ = child.kill();
            let _ = child.wait();
            break None;
        }
        if save_state.is_some_and(|flag| {
            flag.swap(false, std::sync::atomic::Ordering::Relaxed)
        }) {
            if let Some(stdin) = &mut stdin {
                let _ = writeln!(
                    stdin,
                    "{}",
                    crate::project::CODE_RELOAD_SAVE_COMMAND
                )
                .and_then(|()| stdin.flush());
            }
            save_requested = Some(std::time::Instant::now());
        }
        if save_requested
            .is_some_and(|since| since.elapsed() > CODE_RELOAD_SAVE_TIMEOUT)
        {
            let _ = child.kill();
            let _ = child.wait();
            let _ = sender.send(BuildWorkerMessage::Output(format!(
                "{} the game did not save its scene within {} s\n",
                crate::project::CODE_RELOAD_CLEAN_MARKER,
                CODE_RELOAD_SAVE_TIMEOUT.as_secs()
            )));
            break None;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    };
    for reader in readers {
        let _ = reader.join();
    }
    Ok(status)
}

/// Work selected inside the Assets area and applied after drawing.
enum AssetRequest {
    ImportFiles(Vec<PathBuf>),
    /// Dry-runs replacing `EditorAssetState::replace_target` with a file.
    PreviewReplacement(PathBuf),
    /// Writes the replacement in `EditorAssetState::replace_preview`.
    ConfirmReplacement,
    LoadTexture(PathBuf),
    /// Adds a project glTF file to the scene as one object tree.
    AddModel(PathBuf),
    /// Places a saved scene in the open scene as a linked instance.
    InstanceScene(PathBuf),
    /// Writes a new scene next to this one that inherits it.
    NewSceneVariant(PathBuf),
    /// Writes a new data asset of a registered type into a folder.
    NewDataAsset {
        folder: PathBuf,
        type_name: &'static str,
    },
    /// Writes the data asset open in the Inspector and reloads it.
    SaveDataAsset,
    /// Loads an image and uses it as the selected object's base color.
    AssignTexture(PathBuf),
    /// Gives the selected renderer a fresh default material.
    NewMaterial,
    /// Assign an already-loaded typed mesh to one scene renderer.
    AssignMesh {
        entity: Entity,
        handle: Handle<crate::assets::MeshAsset>,
    },
    /// Assign an already-loaded typed material to one scene renderer.
    AssignMaterial {
        entity: Entity,
        handle: Handle<MaterialAsset>,
    },
    /// Picks an image file for one `inspector::TEXTURE_SLOTS` slot.
    LoadMaterialTexture(usize),
    /// Puts the image a `LoadMaterialTexture` dialog chose into the slot.
    SetMaterialTexture {
        entity: Entity,
        slot: usize,
        path: PathBuf,
    },
}

/// Scene-object operation requested by the Hierarchy area.
enum EntityRequest {
    /// Adds an object that only has a name and transform.
    CreateEmpty(Option<Entity>),
    /// Adds an object made of registered components with default values,
    /// such as a World Environment or a HUD element.
    CreateWith {
        name: &'static str,
        components: &'static [&'static str],
        parent: Option<Entity>,
    },
    /// Adds one of the engine's built-in procedural meshes.
    CreatePrimitive(PrimitiveShape, Option<Entity>),
    /// Adds an inactive perspective camera.
    CreateCamera(Option<Entity>),
    /// Adds an authored light used by editor and game rendering.
    CreateLight(EditorLightType, Option<Entity>),
    /// Copies every serialized component of each object.
    Duplicate(Vec<Entity>),
    /// Removes objects and all of their children.
    Delete(Vec<Entity>),
    /// Shows or hides one object (the Hierarchy eye toggle).
    SetVisible(Entity, bool),
    /// Changes the display name stored in the scene.
    Rename(Entity, String),
    /// Moves one object below another object, or back to the scene root.
    Reparent(Vec<Entity>, Option<Entity>),
    /// Reverts, applies or unpacks the scene instance that holds an object.
    Instance(Entity, crate::runtime::InstanceEdit),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EditorLightType {
    Directional,
    Point,
    Spot,
}

impl EditorLightType {
    const ALL: [Self; 3] = [Self::Directional, Self::Point, Self::Spot];

    const fn label(self) -> &'static str {
        match self {
            Self::Directional => "Directional Light",
            Self::Point => "Point Light",
            Self::Spot => "Spot Light",
        }
    }
}

/// Stores the current scene before an editor command changes it.
fn remember_scene_before_edit(
    world: &mut World,
    history: &mut EditorHistory,
) -> Result<(), String> {
    if let Some(document) = history.pending_inspector.take() {
        history.push_undo(document);
    }
    scene_document(world, "Undo Snapshot")
        .map(|document| history.push_undo(document))
        .map_err(|error| format!("Could not create undo snapshot: {error}"))
}

/// Gives a new object a readable name that is not already in the Hierarchy.
fn unique_object_name(world: &mut World, base: &str) -> String {
    let mut query = world.query::<&Name>();
    let names = query
        .iter(world)
        .map(|name| name.0.clone())
        .collect::<std::collections::HashSet<_>>();
    free_name(&names, base)
}

fn free_name(names: &std::collections::HashSet<String>, base: &str) -> String {
    if !names.contains(base) {
        return base.into();
    }
    for number in 2.. {
        let candidate = format!("{base} {number}");
        if !names.contains(&candidate) {
            return candidate;
        }
    }
    unreachable!("a free object name always exists")
}

/// Places the scene at `path` in the open scene as one new object with a
/// `SceneInstance` link, like Godot's "Instantiate Child Scene". Returns the
/// new root object. The caller takes the undo snapshot.
fn instance_scene_in_scene(
    world: &mut World,
    path: &std::path::Path,
    open_scene: Option<&std::path::Path>,
) -> Result<Entity, String> {
    let source = path.canonicalize().map_err(|error| error.to_string())?;
    if open_scene
        .and_then(|open| open.canonicalize().ok())
        .as_ref()
        == Some(&source)
    {
        return Err("A scene cannot contain an instance of itself".into());
    }
    let base = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("Scene");
    let id = SceneId::new().0;
    let entity: crate::runtime::SceneEntity = serde_json::from_value(
        serde_json::json!({
            "id": id,
            "parent": null,
            "name": unique_object_name(world, base),
            "transform": crate::runtime::SceneTransform::from(Transform::default()),
            "mesh_renderer": null,
            "camera": null,
            "visible": null,
            "components": {
                crate::runtime::SCENE_INSTANCE_COMPONENT:
                    serde_json::json!({"source": source}).to_string(),
            },
        }),
    )
    .map_err(|error| error.to_string())?;
    let document = crate::runtime::SceneDocument {
        format_version: crate::runtime::SCENE_FORMAT_VERSION,
        name: base.to_owned(),
        entities: vec![entity],
        render: Default::default(),
        simulation: Default::default(),
    };
    crate::runtime::load_scene_document(
        world,
        &document,
        SceneLoadMode::Additive,
    )
    .map_err(|error| error.to_string())?;
    let mut query = world.query::<(Entity, &SceneId)>();
    query
        .iter(world)
        .find_map(|(entity, found)| (found.0 == id).then_some(entity))
        .ok_or_else(|| "The instance was not added".to_owned())
}

/// Runs `edit` on the scene instance that holds `entity` and reloads the
/// scene. Returns `entity` again after the reload. The caller takes the undo
/// snapshot; Apply to Source also writes the source file, which Undo does
/// not restore.
fn edit_scene_instance(
    world: &mut World,
    entity: Entity,
    edit: crate::runtime::InstanceEdit,
) -> Result<Option<Entity>, String> {
    let id = world
        .get::<SceneId>(entity)
        .ok_or("Selected object is not part of the saved scene")?
        .0;
    let mut document = scene_document(world, "Main Scene")
        .map_err(|error| error.to_string())?;
    crate::runtime::edit_instance(&mut document, id, edit)
        .map_err(|error| error.to_string())?;
    crate::runtime::load_scene_document(
        world,
        &document,
        SceneLoadMode::Replace,
    )
    .map_err(|error| error.to_string())?;
    let mut query = world.query::<(Entity, &SceneId)>();
    Ok(query
        .iter(world)
        .find_map(|(entity, found)| (found.0 == id).then_some(entity)))
}

/// Writes the inspected data asset and reloads it, so every handle to the
/// file sees the edit.
fn save_data_inspection(
    world: &mut World,
    inspection: &DataInspection,
) -> Result<String, String> {
    crate::assets::write_data_file(
        &inspection.path,
        &inspection.type_name,
        inspection.value.clone(),
    )
    .map_err(|error| error.to_string())?;
    let registered = world
        .resource::<crate::assets::DataAssetTypes>()
        .get(&inspection.type_name)
        .cloned()
        .ok_or_else(|| {
            format!("`{}` is not registered", inspection.type_name)
        })?;
    let mut assets = world.resource_mut::<AssetServer>();
    registered
        .reload(&mut assets, &inspection.path)
        .map_err(|error| error.to_string())?;
    Ok(format!("Saved {}", inspection.path.display()))
}

/// Writes `<name>_variant.rscene` (or `_variant_2`, ...) next to `base`, a
/// scene that inherits `base` like a Godot inherited scene. Returns the new
/// file. The open scene does not change, so there is no undo step.
fn new_scene_variant(base: &std::path::Path) -> Result<PathBuf, String> {
    let stem = base
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("scene");
    let path = (1..)
        .map(|number| {
            let suffix = if number == 1 {
                String::new()
            } else {
                format!("_{number}")
            };
            base.with_file_name(format!("{stem}_variant{suffix}.rscene"))
        })
        .find(|path| !path.exists())
        .expect("a free file name always exists");
    crate::runtime::save_scene_variant(base, &path)
        .map_err(|error| error.to_string())?;
    Ok(path)
}

/// Adds a glTF file to the scene like Blender's importer: one new empty
/// object named after the file, at the origin, holding the file's node tree
/// with its names, local transforms, meshes, materials, cameras, and lights,
/// and its node animations as `rusting.animation` clips on the new object.
/// Imported names that clash with existing objects get a number. Returns the
/// new root object. The caller takes the undo snapshot.
fn add_model_to_scene(
    world: &mut World,
    path: &std::path::Path,
) -> Result<Entity, String> {
    let mut nodes = world
        .resource_mut::<AssetServer>()
        .import_gltf_scene(path)
        .map_err(|error| error.to_string())?;
    let before = world
        .query::<Entity>()
        .iter(world)
        .collect::<std::collections::HashSet<_>>();
    let base = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("Model");
    let root_name = unique_object_name(world, base);
    let root = world
        .spawn((SceneId::new(), Name(root_name), Transform::default()))
        .id();
    let entities = crate::spawn_gltf_nodes_in_world(world, &nodes, None)
        .map_err(|error| error.to_string())?;
    for (node, &entity) in nodes.iter().zip(&entities) {
        if node.parent.is_none() {
            crate::runtime::hierarchy::set_parent(world, entity, root)
                .map_err(|error| error.to_string())?;
        }
    }
    let mut query = world.query::<(Entity, &Name)>();
    let mut names = std::collections::HashSet::new();
    let mut added = Vec::new();
    for (entity, name) in query.iter(world) {
        if before.contains(&entity) || entity == root {
            names.insert(name.0.clone());
        } else {
            added.push(entity);
        }
    }
    for entity in added {
        let base = world.get::<Name>(entity).map_or("", |name| &name.0);
        let base = if base.is_empty() { "Node" } else { base };
        let name = free_name(&names, base);
        names.insert(name.clone());
        world.entity_mut(entity).insert(Name(name));
    }
    // Clip tracks and skin joints name nodes by path, so they use the
    // renamed names.
    for (node, &entity) in nodes.iter_mut().zip(&entities) {
        if let Some(name) = world.get::<Name>(entity) {
            node.name.clone_from(&name.0);
        }
    }
    crate::assets::insert_gltf_skins(world, &nodes, &entities);
    let clips = world
        .resource::<AssetServer>()
        .import_gltf_animations(path, &nodes)
        .map_err(|error| error.to_string())?;
    if let Some(first) = clips.first() {
        world.entity_mut(root).insert(crate::runtime::Animation {
            autoplay: first.name.clone(),
            clips,
            ..crate::runtime::Animation::default()
        });
    }
    Ok(root)
}

/// Returns normal files below a project's `assets` folder without following
/// directory symbolic links.
fn project_asset_files(project_root: &str) -> Vec<PathBuf> {
    let root = PathBuf::from(project_root).join("assets");
    let mut pending = vec![root];
    let mut files = Vec::new();
    while let Some(folder) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(folder) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if file_type.is_symlink() {
                continue;
            }
            if file_type.is_dir() {
                pending.push(entry.path());
            } else if file_type.is_file() {
                files.push(entry.path());
            }
            if files.len() >= 10_000 {
                break;
            }
        }
    }
    files.sort();
    files
}

/// Copies an external file into the project without replacing an existing
/// asset. Name collisions receive `_2`, `_3`, and so on.
/// Replaces an imported asset from `source` through the project importer,
/// keeping its ID; `dry_run` only checks.
fn replace_asset(
    project_root: &str,
    target: &std::path::Path,
    source: &std::path::Path,
    dry_run: bool,
) -> Result<crate::asset_import::ImportReport, String> {
    crate::asset_import::reimport_asset(
        std::path::Path::new(project_root),
        &target.to_string_lossy(),
        Some(source),
        &crate::asset_import::AssetProvenance::default(),
        None,
        dry_run,
    )
    .map_err(|error| error.to_string())
}

fn copy_into_project_assets(
    project_root: &str,
    source: &std::path::Path,
) -> Result<PathBuf, String> {
    if !source.is_file() {
        return Err(format!("{} is not a file", source.display()));
    }
    let asset_folder = PathBuf::from(project_root).join("assets");
    std::fs::create_dir_all(&asset_folder)
        .map_err(|error| format!("Could not create assets folder: {error}"))?;
    let source = source
        .canonicalize()
        .map_err(|error| format!("Could not open asset: {error}"))?;
    let asset_folder = asset_folder
        .canonicalize()
        .map_err(|error| format!("Could not open assets folder: {error}"))?;
    if source.starts_with(&asset_folder) {
        return Ok(source);
    }
    let file_name = source
        .file_name()
        .ok_or_else(|| "Asset path has no file name".to_owned())?;
    let mut destination = asset_folder.join(file_name);
    let stem = source
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("asset");
    let extension = source.extension().and_then(|value| value.to_str());
    for number in 2.. {
        if !destination.exists() {
            break;
        }
        let name = extension.map_or_else(
            || format!("{stem}_{number}"),
            |extension| format!("{stem}_{number}.{extension}"),
        );
        destination = asset_folder.join(name);
    }
    std::fs::copy(&source, &destination).map_err(|error| {
        format!(
            "Could not copy {} to {}: {error}",
            source.display(),
            destination.display()
        )
    })?;
    Ok(destination)
}

/// Draws the startup Project Manager and returns one requested action.
fn draw_project_manager(
    context: &Context,
    manager: &mut ProjectManagerState,
    dialogs: &mut FileDialogs,
) -> Option<ProjectRequest> {
    if !manager.open {
        return None;
    }

    let mut request = None;
    egui::Window::new("RustingEngine Project Manager")
        .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
        .collapsible(false)
        .resizable(true)
        .default_width(620.0)
        .show(context, |ui| {
            ui.heading("Open a project");
            if manager.recent_projects.is_empty() {
                ui.label("No recent projects yet.");
            } else {
                egui::ScrollArea::vertical()
                    .max_height(160.0)
                    .show(ui, |ui| {
                        for project in &manager.recent_projects {
                            let text = format!(
                                "{}\n{}",
                                project.name,
                                project.path.display()
                            );
                            if ui.button(text).clicked() {
                                request = Some(ProjectRequest::Open(
                                    project.path.clone(),
                                ));
                            }
                        }
                    });
            }
            if ui.button("Browse for existing project...").clicked() {
                dialogs.pick_folder(
                    DialogPurpose::OpenProject,
                    rfd::AsyncFileDialog::new()
                        .set_title("Open RustingEngine Project"),
                );
            }

            ui.separator();
            ui.heading("Create a project");
            ui.horizontal(|ui| {
                ui.label("Name");
                ui.text_edit_singleline(&mut manager.project_name);
            });
            ui.horizontal(|ui| {
                ui.label("Template");
                gui_elements::EditorTheme::toolbar_combo_box(
                    ui,
                    "new_project_template",
                    manager.template.label(),
                    180.0,
                    |ui| {
                        for choice in ProjectTemplate::ALL {
                            if gui_elements::EditorTheme::menu_choice(
                                ui,
                                choice.label(),
                                manager.template == choice,
                                true,
                            )
                            .clicked()
                            {
                                manager.template = choice;
                            }
                        }
                    },
                );
            });
            ui.horizontal(|ui| {
                ui.label("Parent folder");
                if manager.parent_directory.as_os_str().is_empty() {
                    ui.colored_label(
                        gui_elements::EditorTheme::TEXT_MUTED,
                        "Not selected",
                    );
                } else {
                    ui.monospace(
                        manager.parent_directory.display().to_string(),
                    );
                }
                if ui.button("Browse...").clicked() {
                    let mut dialog = rfd::AsyncFileDialog::new()
                        .set_title("Choose Project Parent Folder");
                    if !manager.parent_directory.as_os_str().is_empty() {
                        dialog =
                            dialog.set_directory(&manager.parent_directory);
                    }
                    dialogs.pick_folder(
                        DialogPurpose::ProjectParentFolder,
                        dialog,
                    );
                }
            });
            if ui.button("Create Project").clicked() {
                if manager.parent_directory.as_os_str().is_empty() {
                    dialogs.pick_folder(
                        DialogPurpose::CreateProjectIn,
                        rfd::AsyncFileDialog::new()
                            .set_title("Choose Where to Create the Project"),
                    );
                } else {
                    request = Some(ProjectRequest::Create {
                        parent: manager.parent_directory.clone(),
                        name: manager.project_name.clone(),
                        template: manager.template,
                    });
                }
            }
            if let Some(message) = &manager.message {
                ui.separator();
                ui.label(message);
            }
        });
    request
}

/// Draws one fixed-size button in an editor area header.
///
/// The rectangle and icon are painted separately. Different icon sizes can
/// therefore never change the size or vertical position of the button.
fn dock_header_button(
    ui: &mut egui::Ui,
    icon: EditorIcon,
    hover_text: &str,
) -> egui::Response {
    gui_elements::icon_button_sized(
        ui,
        icon,
        false,
        true,
        egui::vec2(22.0, 20.0),
    )
    .on_hover_text(hover_text)
}

#[derive(Clone, Copy)]
enum DockAction {
    /// Area ID and direction for a new divider.
    Split(u64, EditorSplitAxis),
    /// Area ID that should be removed.
    Close(u64),
}

/// Draws one layout node and all children below it.
///
/// # Arguments
/// * `ui` - Egui area used to paint controls.
/// * `node` - Area or split currently being drawn.
/// * `rect` - Screen space available to this node.
/// * `active_area` - ID of the area with the blue border.
/// * `actions` - Changes that will be applied after drawing is finished.
/// * `show_panel` - Draws the selected content inside a leaf area.
fn show_dock_node(
    ui: &mut egui::Ui,
    node: &mut EditorDockNode,
    rect: egui::Rect,
    active_area: &mut u64,
    actions: &mut Vec<DockAction>,
    show_panel: &mut impl FnMut(&mut egui::Ui, EditorPanel),
) {
    const DIVIDER: f32 = 4.0;
    match node {
        EditorDockNode::Area { id, panel } => {
            let id = *id;
            // Leave breathing room between the panel and its resize divider.
            let panel_rect = rect.shrink(1.0);
            // Every area gets its own ID space. Two areas can show the same
            // panel without Egui thinking that their controls are duplicates.
            // A leaf draws its header first and panel content below it.
            ui.scope_builder(
                egui::UiBuilder::new()
                    .id_salt(("dock_area", id))
                    .max_rect(panel_rect)
                    .layout(egui::Layout::top_down(egui::Align::Min)),
                |ui| {
                    // A panel must never draw or receive clicks over its neighbor.
                    ui.set_clip_rect(ui.clip_rect().intersect(panel_rect));
                    // Clicking any empty space or control selects this area.
                    // `rect_contains_pointer` respects layers, so a popup from
                    // another area floating over this one does not select it.
                    let pressed_inside = ui
                        .input(|input| input.pointer.primary_pressed())
                        && ui.rect_contains_pointer(panel_rect);
                    if pressed_inside {
                        *active_area = id;
                    }
                    let active = *active_area == id;
                    if !matches!(panel, EditorPanel::Scene | EditorPanel::Game)
                    {
                        // Normal panels need a solid background. A 3D view stays
                        // transparent because Vulkan draws below egui.
                        ui.painter().rect_filled(
                            panel_rect,
                            4.0,
                            gui_elements::EditorTheme::PANEL,
                        );
                    }
                    // Blender marks the active area subtly, not with a thick frame.
                    let stroke = if active {
                        egui::Stroke::new(
                            1.0_f32,
                            gui_elements::EditorTheme::ACCENT,
                        )
                    } else {
                        egui::Stroke::new(
                            1.0_f32,
                            gui_elements::EditorTheme::BORDER,
                        )
                    };
                    egui::Frame::NONE
                        .fill(gui_elements::EditorTheme::PANEL_RAISED)
                        .corner_radius(egui::CornerRadius {
                            nw: 4,
                            ne: 4,
                            sw: 0,
                            se: 0,
                        })
                        .inner_margin(egui::Margin::symmetric(4, 2))
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                // Blender shows the editor type as an icon
                                // before its switcher.
                                let (icon_rect, _) = ui.allocate_exact_size(
                                    egui::vec2(18.0, 18.0),
                                    egui::Sense::hover(),
                                );
                                icons::paint_editor_icon(
                                    ui.painter(),
                                    panel.icon(),
                                    icon_rect,
                                    if active {
                                        gui_elements::EditorTheme::ACCENT_HOVER
                                    } else {
                                        gui_elements::EditorTheme::TEXT_MUTED
                                    },
                                );
                                let buttons_width = 22.0 * 3.0
                                    + ui.spacing().item_spacing.x * 2.0;
                                let selector_width = (ui.available_width()
                                    - buttons_width
                                    - 12.0)
                                    .clamp(60.0, 140.0);
                                gui_elements::EditorTheme::toolbar_combo_box_with_popup(
                                    ui,
                                    ("dock_panel_selector", id),
                                    panel.title(),
                                    selector_width,
                                    220.0,
                                    |ui| {
                                            for choice in EditorPanel::ALL {
                                                if gui_elements::EditorTheme::menu_choice(
                                                    ui,
                                                    choice.title(),
                                                    *panel == choice,
                                                    true,
                                                )
                                                .clicked()
                                                {
                                                    *panel = choice;
                                                }
                                            }
                                        },
                                );
                                // Keep all three buttons together on the right side.
                                ui.add_space(
                                    (ui.available_width() - buttons_width)
                                        .max(0.0),
                                );
                                if dock_header_button(
                                    ui,
                                    EditorIcon::SplitColumns,
                                    "Split into left/right areas",
                                )
                                .clicked()
                                {
                                    actions.push(DockAction::Split(
                                        id,
                                        EditorSplitAxis::Columns,
                                    ));
                                }
                                if dock_header_button(
                                    ui,
                                    EditorIcon::SplitRows,
                                    "Split into top/bottom areas",
                                )
                                .clicked()
                                {
                                    actions.push(DockAction::Split(
                                        id,
                                        EditorSplitAxis::Rows,
                                    ));
                                }
                                if dock_header_button(
                                    ui,
                                    EditorIcon::Close,
                                    "Close this area",
                                )
                                .clicked()
                                {
                                    actions.push(DockAction::Close(id));
                                }
                            });
                        });
                    egui::Frame::NONE
                        .inner_margin(egui::Margin::same(6))
                        .show(ui, |ui| show_panel(ui, *panel));
                    // Paint the selection border last so panel contents cannot hide it.
                    ui.painter().rect_stroke(
                        panel_rect,
                        4.0,
                        stroke,
                        egui::StrokeKind::Inside,
                    );
                },
            );
        }
        EditorDockNode::Split {
            axis,
            ratio,
            first,
            second,
        } => {
            // Keep both children large enough to stay usable.
            let ratio_clamped = ratio.clamp(0.1, 0.9);
            *ratio = ratio_clamped;
            // Turn the ratio into two child rectangles and one thin divider.
            let (first_rect, divider_rect, second_rect) = match axis {
                EditorSplitAxis::Columns => {
                    let split =
                        rect.left() + (rect.width() - DIVIDER) * ratio_clamped;
                    (
                        egui::Rect::from_min_max(
                            rect.min,
                            egui::pos2(split, rect.bottom()),
                        ),
                        egui::Rect::from_min_max(
                            egui::pos2(split, rect.top()),
                            egui::pos2(split + DIVIDER, rect.bottom()),
                        ),
                        egui::Rect::from_min_max(
                            egui::pos2(split + DIVIDER, rect.top()),
                            rect.max,
                        ),
                    )
                }
                EditorSplitAxis::Rows => {
                    let split =
                        rect.top() + (rect.height() - DIVIDER) * ratio_clamped;
                    (
                        egui::Rect::from_min_max(
                            rect.min,
                            egui::pos2(rect.right(), split),
                        ),
                        egui::Rect::from_min_max(
                            egui::pos2(rect.left(), split),
                            egui::pos2(rect.right(), split + DIVIDER),
                        ),
                        egui::Rect::from_min_max(
                            egui::pos2(rect.left(), split + DIVIDER),
                            rect.max,
                        ),
                    )
                }
            };
            let divider_id = ui.id().with((
                "dock_divider",
                first_rect.min.x.to_bits(),
                first_rect.min.y.to_bits(),
                second_rect.max.x.to_bits(),
                second_rect.max.y.to_bits(),
            ));
            let response = ui.interact(
                divider_rect,
                divider_id,
                egui::Sense::click_and_drag(),
            );
            if response.dragged() {
                // Mouse position becomes the new size ratio while dragging.
                if let Some(pointer) = response.interact_pointer_pos() {
                    *ratio = match axis {
                        EditorSplitAxis::Columns => {
                            (pointer.x - rect.left()) / rect.width()
                        }
                        EditorSplitAxis::Rows => {
                            (pointer.y - rect.top()) / rect.height()
                        }
                    }
                    .clamp(0.1, 0.9);
                }
            }
            if response.hovered() || response.dragged() {
                ui.ctx().set_cursor_icon(match axis {
                    EditorSplitAxis::Columns => {
                        egui::CursorIcon::ResizeHorizontal
                    }
                    EditorSplitAxis::Rows => egui::CursorIcon::ResizeVertical,
                });
            }
            ui.painter().rect_filled(
                divider_rect,
                0.0,
                if response.hovered() || response.dragged() {
                    gui_elements::EditorTheme::ACCENT_HOVER
                } else {
                    gui_elements::EditorTheme::BORDER_SOFT
                },
            );
            // Splits can contain more splits, so draw both children the same way.
            show_dock_node(
                ui,
                first,
                first_rect,
                active_area,
                actions,
                show_panel,
            );
            show_dock_node(
                ui,
                second,
                second_rect,
                active_area,
                actions,
                show_panel,
            );
        }
    }
}

/// Applies split and close requests after the layout is no longer borrowed.
///
/// Egui creates actions while drawing. Waiting until drawing is finished keeps
/// Rust from borrowing and changing the same tree at the same time.
fn apply_dock_actions(state: &mut EditorState, actions: Vec<DockAction>) {
    for action in actions {
        match action {
            DockAction::Split(id, axis) => {
                let new_id = state.next_area_id;
                state.next_area_id = state.next_area_id.saturating_add(1);
                if state.dock_layout.split(id, axis, new_id) {
                    state.active_area = new_id;
                }
            }
            DockAction::Close(id) => {
                if state.dock_layout.close(id) && state.active_area == id {
                    state.active_area = state.dock_layout.first_area_id();
                }
            }
        }
    }
}

/// Checks that an editable source file stays inside the selected game project.
///
/// * `project_root` - Folder containing the game's `project.json`.
/// * `path` - Rust or shader file requested by Code Editor.
fn project_source_path(
    project_root: &str,
    path: &str,
) -> Result<PathBuf, String> {
    let path = project_content_path(project_root, path)?;
    let supported = path.extension().and_then(|extension| extension.to_str());
    if !matches!(supported, Some("rs" | "glsl" | "comp" | "vert" | "frag")) {
        return Err(
            "Supported source types: .rs, .glsl, .comp, .vert, .frag".into()
        );
    }
    Ok(path)
}

/// Resolves one project-relative file without allowing it to leave the root.
fn project_content_path(
    project_root: &str,
    path: &str,
) -> Result<PathBuf, String> {
    use std::path::Component;

    let root = std::path::Path::new(project_root);
    if root.as_os_str().is_empty() || !root.join("project.json").is_file() {
        return Err(
            "Selected project must contain a project.json manifest".into()
        );
    }
    let requested = std::path::Path::new(path);
    if requested
        .components()
        .any(|part| matches!(part, Component::ParentDir))
    {
        return Err(
            "Project paths must stay inside the selected game project".into()
        );
    }
    let absolute_root = root
        .canonicalize()
        .map_err(|error| format!("Could not open project folder: {error}"))?;
    let path = if requested.is_absolute() {
        requested.to_path_buf()
    } else {
        absolute_root.join(requested)
    };
    // Walk up to the closest existing folder. This allows a new `scripts`
    // folder while still catching an existing symbolic link that leaves the
    // project.
    let mut existing_ancestor = path.as_path();
    while !existing_ancestor.exists() {
        existing_ancestor = existing_ancestor.parent().ok_or_else(|| {
            "Project path must stay inside the project".to_owned()
        })?;
    }
    let checked_ancestor = existing_ancestor
        .canonicalize()
        .map_err(|error| format!("Could not check source path: {error}"))?;
    if !checked_ancestor.starts_with(&absolute_root) {
        return Err(
            "Project paths must stay inside the selected game project".into()
        );
    }
    Ok(path)
}

/// Converts a checked project file back into the short path stored by the GUI.
fn project_relative_path(
    project_root: &str,
    path: &std::path::Path,
) -> Result<String, String> {
    let absolute = project_content_path(project_root, &path.to_string_lossy())?;
    let root = std::path::Path::new(project_root)
        .canonicalize()
        .map_err(|error| format!("Could not open project folder: {error}"))?;
    absolute
        .strip_prefix(root)
        .map_err(|_| "File must stay inside the selected project".to_owned())
        .map(editor_relative_path)
}

/// Uses one portable separator style for paths shown and stored by the editor.
///
/// `PathBuf` keeps native separators for real filesystem work. Text fields,
/// scene files, project files, and tests use `/` so a project has the same
/// document paths on Windows and Linux.
fn editor_relative_path(path: &std::path::Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

/// Performs quick checks before source text is saved.
///
/// Rust still receives its complete check from Cargo during Build & Run.
fn validate_project_source(
    project_root: &str,
    path: &str,
    source: &str,
) -> Result<(), String> {
    let path = project_source_path(project_root, path)?;
    if source.trim().is_empty() {
        return Err("Source file is empty".into());
    }
    match path.extension().and_then(|extension| extension.to_str()) {
        Some("comp" | "vert" | "frag") => {
            if !source.contains("#version") {
                return Err(
                    "GLSL shader is missing a #version directive".into()
                );
            }
            if !source.contains("void main") {
                return Err("GLSL shader is missing void main()".into());
            }
            if path
                .extension()
                .is_some_and(|extension| extension == "comp")
                && !source.contains("local_size_")
            {
                return Err(
                    "Compute shader is missing a local workgroup size".into()
                );
            }
        }
        Some("rs") if !source.contains('{') => {
            return Err("Rust source does not contain a code block".into());
        }
        _ => {}
    }
    Ok(())
}

/// Validates and saves text from Code Editor inside the game project.
fn save_project_source(
    project_root: &str,
    path: &str,
    source: &str,
) -> Result<(), String> {
    validate_project_source(project_root, path, source)?;
    let path = project_source_path(project_root, path)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| {
            format!("Could not create {}: {error}", parent.display())
        })?;
    }
    crate::runtime::write_atomic(&path, source.as_bytes())
        .map_err(|error| format!("Could not save {}: {error}", path.display()))
}

#[cfg(test)]
mod tests;
