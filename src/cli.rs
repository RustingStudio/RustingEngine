//! Window-free project and scene operations used by the `rusting` CLI.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::asset_import::{
    self, asset_references, AssetImportError, AssetProvenance, ImportReport,
    ImportSettings,
};
use crate::project::{
    create_project_from, open_project, OpenProject, ProjectError,
    ProjectTemplate,
};
use crate::runtime::{
    cook_scene, read_scene_bytes, read_scene_document, SceneDocument,
    SceneEntity, SceneIoError,
};

/// Stable envelope for this and future commands, including scene patches and capture.
#[derive(Debug, Serialize)]
pub struct CliResult {
    pub schema_version: u32,
    pub ok: bool,
    pub data: Value,
    pub diagnostics: Vec<Diagnostic>,
}

#[derive(Debug, Default, Serialize)]
pub struct Diagnostic {
    pub code: &'static str,
    pub severity: &'static str,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<PathBuf>,
    /// 1-based line and column in `file`, for text the tools could not parse.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub column: Option<u32>,
    /// JSON pointer into `file`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scene_location: Option<String>,
    /// The scene object the diagnostic is about.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub entity: Option<EntityRef>,
    /// A `scene patch` operation that fixes the problem for certain;
    /// `rusting fix` applies every one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fix: Option<Value>,
}

#[derive(Debug, Serialize)]
pub struct EntityRef {
    pub id: Uuid,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

impl EntityRef {
    fn of(entity: &SceneEntity) -> Self {
        Self {
            id: entity.id,
            name: entity.name.clone(),
        }
    }
}

/// `--limit`, `--fields` and `--summary`: keep a listing command's output
/// within an agent's budget.
#[derive(Debug, Default)]
pub struct Shape {
    pub limit: Option<usize>,
    pub fields: Option<Vec<String>>,
    pub summary: bool,
}

impl Shape {
    /// Applies the options to every array directly under `data`: `fields`
    /// keeps only those keys of each object, `limit` keeps the first items,
    /// and `summary` replaces the array with `{"count": N}`. Each cut array's
    /// omitted count goes under `data.omitted`.
    pub fn apply(&self, data: &mut Value) {
        let Some(map) = data.as_object_mut() else {
            return;
        };
        let mut omitted = serde_json::Map::new();
        for (key, value) in map.iter_mut() {
            let Some(items) = value.as_array_mut() else {
                continue;
            };
            if let Some(fields) = &self.fields {
                for item in items.iter_mut() {
                    if let Some(object) = item.as_object_mut() {
                        object.retain(|name, _| fields.contains(name));
                    }
                }
            }
            let total = items.len();
            if self.summary {
                *value = serde_json::json!({ "count": total });
                continue;
            }
            if let Some(limit) = self.limit.filter(|limit| *limit < total) {
                items.truncate(limit);
                omitted.insert(key.clone(), (total - limit).into());
            }
        }
        if !omitted.is_empty() {
            map.insert("omitted".into(), Value::Object(omitted));
        }
    }
}

impl CliResult {
    pub fn success(data: Value) -> Self {
        Self {
            schema_version: 1,
            ok: true,
            data,
            diagnostics: Vec::new(),
        }
    }

    pub fn failure(
        code: &'static str,
        message: impl Into<String>,
        file: Option<PathBuf>,
    ) -> Self {
        Self {
            schema_version: 1,
            ok: false,
            data: Value::Null,
            diagnostics: vec![Diagnostic {
                code,
                severity: "error",
                message: message.into(),
                file,
                ..Diagnostic::default()
            }],
        }
    }

    pub fn exit_code(&self) -> i32 {
        if self.ok {
            0
        } else if self.diagnostics.iter().any(|d| d.code == "CLI_USAGE") {
            2
        } else {
            1
        }
    }
}

fn project_error(error: ProjectError, path: &Path) -> CliResult {
    let code = match error {
        ProjectError::InvalidName | ProjectError::InvalidBinaryName => {
            "PROJECT_INVALID"
        }
        ProjectError::MissingParent(_) | ProjectError::MissingFile(_) => {
            "PROJECT_MISSING_FILE"
        }
        ProjectError::AlreadyExists(_) => "PROJECT_EXISTS",
        ProjectError::UnsupportedVersion(_) => "PROJECT_VERSION",
        ProjectError::Json(_) => "PROJECT_MANIFEST_JSON",
        ProjectError::Io(_) => "PROJECT_IO",
    };
    CliResult::failure(code, error.to_string(), Some(path.to_owned()))
}

fn scene_error(error: SceneIoError, path: &Path) -> CliResult {
    let code = match error {
        SceneIoError::Io(ref error)
            if error.kind() == std::io::ErrorKind::ResourceBusy =>
        {
            "LEASE_HELD"
        }
        SceneIoError::Io(_) => "SCENE_IO",
        SceneIoError::Source(_) => "SCENE_JSON",
        SceneIoError::UnsupportedVersion(_) => "SCENE_VERSION",
        SceneIoError::DuplicateEntity(_)
        | SceneIoError::DuplicateName(_)
        | SceneIoError::MissingParent(_)
        | SceneIoError::HierarchyCycle(_) => "SCENE_STRUCTURE",
        SceneIoError::Reflection(_) => "SCENE_COMPONENT_FIELD",
        _ => "SCENE_INVALID",
    };
    let mut result =
        CliResult::failure(code, error.to_string(), Some(path.to_owned()));
    locate_scene_error(&error, path, &mut result.diagnostics[0]);
    result
}

/// Points a scene load error at the line, or at the object, that caused it.
fn locate_scene_error(
    error: &SceneIoError,
    path: &Path,
    diagnostic: &mut Diagnostic,
) {
    if let SceneIoError::Source(error) = error {
        diagnostic.line = u32::try_from(error.line()).ok();
        diagnostic.column = u32::try_from(error.column()).ok();
        let missing = error
            .to_string()
            .strip_prefix("missing field `")
            .and_then(|rest| rest.split('`').next().map(str::to_owned));
        if let Some((key, fix)) =
            missing.and_then(|missing| misspelled_key(path, &missing))
        {
            diagnostic.message +=
                &format!("; `{key}` looks like a misspelling of it");
            diagnostic.fix = Some(fix);
        }
        return;
    }
    // Structure errors come after decoding, so the file still decodes.
    let Some(document) = std::fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<SceneDocument>(&bytes).ok())
    else {
        return;
    };
    let Some(index) = crate::runtime::error_object(&document, error) else {
        return;
    };
    let mut pointer = format!("/entities/{index}");
    if let SceneIoError::Reflection(error) = error {
        let key = error.component.replace('~', "~0").replace('/', "~1");
        pointer = format!("{pointer}/components/{key}");
    }
    diagnostic.scene_location = Some(pointer);
    diagnostic.entity = Some(EntityRef::of(&document.entities[index]));
}

/// A `rename_key` fix for a required field the scene file misspells: the
/// only key within two edits of `missing` in an object that lacks it, when
/// that key appears once in the file.
fn misspelled_key(path: &Path, missing: &str) -> Option<(String, Value)> {
    fn walk(
        value: &Value,
        pointer: &str,
        missing: &str,
        out: &mut Vec<(String, String)>,
    ) {
        match value {
            Value::Object(object) => {
                for (key, child) in object {
                    let child_pointer = format!(
                        "{pointer}/{}",
                        crate::scene_patch::escape(key)
                    );
                    if !object.contains_key(missing)
                        && crate::scene_patch::edit_distance(key, missing) <= 2
                    {
                        out.push((key.clone(), child_pointer.clone()));
                    }
                    walk(child, &child_pointer, missing, out);
                }
            }
            Value::Array(items) => {
                for (index, item) in items.iter().enumerate() {
                    walk(item, &format!("{pointer}/{index}"), missing, out);
                }
            }
            _ => {}
        }
    }
    let text = std::fs::read_to_string(path).ok()?;
    let raw: Value = serde_json::from_str(&text).ok()?;
    let mut candidates = Vec::new();
    walk(&raw, "", missing, &mut candidates);
    let [(key, pointer)] = candidates.as_slice() else {
        return None;
    };
    (key_positions(&text, key).len() == 1).then(|| {
        (
            key.clone(),
            json!({"op": "rename_key", "path": pointer, "to": missing}),
        )
    })
}

/// Byte offsets of `"key"` used as an object key (followed by `:`).
fn key_positions(text: &str, key: &str) -> Vec<usize> {
    let quoted = format!("\"{key}\"");
    text.match_indices(&quoted)
        .filter(|(start, _)| {
            text[start + quoted.len()..].trim_start().starts_with(':')
        })
        .map(|(start, _)| start)
        .collect()
}

fn project_data(project: &OpenProject) -> Value {
    json!({
        "root": project.root,
        "manifest_path": project.root.join("project.json"),
        "manifest": project.manifest,
        "main_scene": project.scene_path,
        "cooked_scene": project.root.join(&project.manifest.cooked_scene),
        "cargo_manifest": project.root.join("Cargo.toml"),
        "code_path": project.code_path,
        "binary_name": project.manifest.binary_name,
        "engine_version": env!("CARGO_PKG_VERSION"),
    })
}

pub fn new_project(
    parent: &Path,
    name: &str,
    template: ProjectTemplate,
) -> CliResult {
    match create_project_from(parent, name, template) {
        Ok(project) => {
            let mut data = project_data(&project);
            data["next"] = json!(
                "Run `rusting check`. The first build compiles the engine \
                 and takes a few minutes; later builds take seconds."
            );
            CliResult::success(data)
        }
        Err(error) => project_error(error, &parent.join(name)),
    }
}

/// `rusting systems`: the engine's ECS systems with the components and
/// resources each reads and writes, plus the game code functions of the
/// project at `root` (if it is one; see `game_system_access`). `reads` or
/// `writes` keeps the systems that read or write that type name; systems
/// with `all` access may touch anything, so they are always kept.
pub fn system_access(
    root: &Path,
    reads: Option<&str>,
    writes: Option<&str>,
) -> CliResult {
    use crate::runtime::{HybridPhysicsPlugin, RenderExtractPlugin};
    use crate::{App, AssetPlugin};
    let mut app = App::new();
    app.add_plugin(AssetPlugin)
        .and_then(|app| app.add_plugin(HybridPhysicsPlugin))
        .and_then(|app| app.add_plugin(RenderExtractPlugin))
        .expect("the engine plugins build on a new app");
    let names = |list: &[String], wanted: Option<&str>| {
        wanted.is_none_or(|wanted| list.iter().any(|name| name == wanted))
    };
    let mut systems: Vec<_> = app
        .into_system_access()
        .into_iter()
        .map(|system| {
            json!({
                "stage": format!("{:?}", system.stage),
                "name": system.name,
                "reads": system.reads,
                "writes": system.writes,
                "all": system.all,
            })
        })
        .collect();
    if root.join("project.json").is_file() {
        systems.extend(game_system_access(root));
    }
    let list = |system: &Value, key: &str| -> Vec<String> {
        serde_json::from_value(system[key].clone()).unwrap_or_default()
    };
    systems.retain(|system| {
        system["all"] == true
            || (names(&list(system, "reads"), reads)
                || names(&list(system, "writes"), reads))
                && names(&list(system, "writes"), writes)
    });
    CliResult::success(json!({ "systems": systems }))
}

/// What a `GameScene` method reads (`false`) or writes (`true`).
const GAME_SCENE_ACCESS: &[(&str, bool, &str)] = &[
    ("set_position", true, "Transform"),
    ("move_by", true, "Transform"),
    ("move_x", true, "Transform"),
    ("move_y", true, "Transform"),
    ("move_z", true, "Transform"),
    ("set_rotation", true, "Transform"),
    ("rotate_by", true, "Transform"),
    ("rotate_x", true, "Transform"),
    ("rotate_y", true, "Transform"),
    ("rotate_z", true, "Transform"),
    ("set_scale", true, "Transform"),
    ("position", false, "Transform"),
    ("rotation", false, "Transform"),
    ("scale", false, "Transform"),
    ("reparent", true, "Parent"),
    ("set_counter", true, "Counter"),
    ("add_to_counter", true, "Counter"),
    ("counter", true, "Counter"),
    ("counter_value", false, "Counter"),
    ("counter_or", false, "Counter"),
    ("counter_complete", false, "Counter"),
    ("counters", false, "Counter"),
    ("start_cooldown", true, "Counter"),
    ("load_counters", true, "Counter"),
    ("save_counters", false, "Counter"),
    ("cooldown_ready", false, "Counter"),
    ("cooldown_left", false, "Counter"),
    ("damage", true, "Health"),
    ("health", false, "Health"),
    ("same_team", false, "Health"),
    ("roll_loot_table", false, "LootTable"),
    ("start_dialogue", true, "Dialogue"),
    ("advance_dialogue", true, "Dialogue"),
    ("dialogue_line", false, "Dialogue"),
    ("set_state", true, "ObjectState"),
    ("state", false, "ObjectState"),
    ("state_seconds", false, "ObjectState"),
    ("set_visible", true, "Visibility"),
    ("set_color", true, "MeshRenderer"),
    ("set_emissive", true, "MeshRenderer"),
    ("set_material", true, "MeshRenderer"),
    ("color", false, "MeshRenderer"),
    ("set_player", true, "PlayerController"),
    ("player", false, "PlayerController"),
    ("set_linear_velocity", true, "RigidBody"),
    ("set_angular_velocity", true, "RigidBody"),
    ("reset_body", true, "RigidBody"),
    ("set_body_kind", true, "RigidBody"),
    ("linear_velocity", false, "RigidBody"),
    ("angular_velocity", false, "RigidBody"),
    ("set_light", true, "PointLight"),
    ("set_light", true, "SpotLight"),
    ("set_tile", true, "TileMap"),
    ("tile", false, "TileMap"),
];

/// The game code functions under `root/src` that touch components, found
/// by a text scan: bevy system parameters (`Query<&mut T>` writes `T`,
/// `Res<T>` reads it, a `World` parameter is `all`), `GameScene` calls from
/// [`GAME_SCENE_ACCESS`], `set_field` paths (`/components/game.health/..`
/// writes `game.health`, `/point_light/..` writes `PointLight`) and
/// `world()` (`all`). Stage is `Game`; `file` and `line` locate the `fn`.
// ponytail: a text scan, so calls through helpers, macros or variables named
// like a method are missed or misread; per-call tracking in GameScene would be
// exact.
fn game_system_access(root: &Path) -> Vec<Value> {
    let mut files = Vec::new();
    rust_files(&root.join("src"), &mut files);
    files.sort();
    let mut systems = Vec::new();
    for file in files {
        let Ok(text) = std::fs::read_to_string(&file) else {
            continue;
        };
        for (start, _) in text.match_indices("fn ") {
            if text[..start]
                .chars()
                .next_back()
                .is_some_and(|c| c.is_alphanumeric() || c == '_')
            {
                continue;
            }
            let rest = &text[start + 3..];
            let Some(open) = rest.find(['{', ';']) else {
                continue;
            };
            if rest.as_bytes()[open] == b';' {
                continue;
            }
            let signature = &rest[..open];
            let mut depth = 0;
            let close = rest[open..]
                .find(|c| {
                    depth += match c {
                        '{' => 1,
                        '}' => -1,
                        _ => 0,
                    };
                    depth == 0
                })
                .map_or(rest.len(), |end| open + end);
            let body = &rest[open..close];
            let mut reads = BTreeSet::new();
            let mut writes = BTreeSet::new();
            let mut all = has_word(signature, "World");
            for (kind, written) in [("ResMut<", true), ("Res<", false)] {
                for (at, _) in signature.match_indices(kind) {
                    if signature[..at]
                        .chars()
                        .next_back()
                        .is_some_and(|c| c.is_alphanumeric() || c == '_')
                    {
                        continue;
                    }
                    let name = path_tail(&signature[at + kind.len()..]);
                    if written { &mut writes } else { &mut reads }.insert(name);
                }
            }
            for (at, _) in signature.match_indices("Query<") {
                let mut depth = 0;
                let inner = &signature[at + 6..];
                let end = inner
                    .find(|c| {
                        depth += match c {
                            '<' => 1,
                            '>' => -1,
                            _ => 0,
                        };
                        depth < 0
                    })
                    .unwrap_or(inner.len());
                for part in inner[..end].split('&').skip(1) {
                    match part.trim_start().strip_prefix("mut ") {
                        Some(name) => writes.insert(path_tail(name)),
                        None => reads.insert(path_tail(part)),
                    };
                }
            }
            if signature.contains("GameScene") {
                all |= body.contains(".world()");
                for (method, written, name) in GAME_SCENE_ACCESS {
                    let call = format!(".{method}(");
                    let called = body.match_indices(&call).any(|(at, _)| {
                        let argument = body[at + call.len()..].trim_start();
                        !argument.starts_with('|')
                            && !argument.starts_with("move")
                    });
                    if called {
                        if *written { &mut writes } else { &mut reads }
                            .insert((*name).to_owned());
                    }
                }
                for (at, _) in body.match_indices(".set_field(") {
                    let statement = &body[at..];
                    let statement = &statement
                        [..statement.find(';').unwrap_or(statement.len())];
                    let Some(path) = statement.split("\"/").nth(1) else {
                        continue;
                    };
                    let mut parts = path.split(['/', '"']);
                    let name = match parts.next() {
                        Some("components") => {
                            parts.next().unwrap_or_default().to_owned()
                        }
                        Some(section) => section
                            .split('_')
                            .map(|word| {
                                let mut chars = word.chars();
                                chars.next().map_or_else(String::new, |first| {
                                    first.to_uppercase().chain(chars).collect()
                                })
                            })
                            .collect(),
                        None => continue,
                    };
                    writes.insert(name);
                }
            }
            reads.retain(|name: &String| {
                !name.is_empty() && !writes.contains(name)
            });
            writes.retain(|name| !name.is_empty());
            if reads.is_empty() && writes.is_empty() && !all {
                continue;
            }
            systems.push(json!({
                "stage": "Game",
                "name": signature
                    .chars()
                    .take_while(|c| c.is_alphanumeric() || *c == '_')
                    .collect::<String>(),
                "file": file.strip_prefix(root).unwrap_or(&file),
                "line": text[..start].lines().count() + 1,
                "reads": reads,
                "writes": writes,
                "all": all,
            }));
        }
    }
    systems
}

/// Whether `name` appears in `text` as a whole word.
fn has_word(text: &str, name: &str) -> bool {
    let word =
        |c: Option<char>| c.is_some_and(|c| c.is_alphanumeric() || c == '_');
    text.match_indices(name).any(|(at, _)| {
        !word(text[..at].chars().next_back())
            && !word(text[at + name.len()..].chars().next())
    })
}

/// The last segment of the type path at the start of `text`:
/// `crate::game::Health, ...` gives `Health`.
fn path_tail(text: &str) -> String {
    let path: String = text
        .trim_start()
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_' || *c == ':')
        .collect();
    path.rsplit("::").next().unwrap_or_default().to_owned()
}

/// `rusting merge`: three-way merge of scene files by entity and field (see
/// [`crate::scene_merge::merge_scenes`]), written to `output`. Each
/// conflict is a `SCENE_MERGE_CONFLICT` error and keeps our value, so a git
/// merge driver sees a failed merge.
pub fn merge_scene(
    base: &Path,
    ours: &Path,
    theirs: &Path,
    output: &Path,
) -> CliResult {
    let mut sides = Vec::new();
    for path in [base, ours, theirs] {
        match read_scene(path).map(serde_json::to_value) {
            Ok(Ok(value)) => sides.push(value),
            Ok(Err(error)) => {
                return CliResult::failure(
                    "SCENE_JSON",
                    error.to_string(),
                    Some(path.to_owned()),
                )
            }
            Err(result) => return result,
        }
    }
    let (merged, conflicts) =
        crate::scene_merge::merge_scenes(&sides[0], &sides[1], &sides[2]);
    let written = serde_json::to_vec(&merged)
        .map_err(SceneIoError::from)
        .and_then(|bytes| crate::runtime::parse_scene_document(&bytes))
        .and_then(|document| Ok(serde_json::to_vec_pretty(&document)?))
        .and_then(|bytes| Ok(crate::runtime::write_atomic(output, &bytes)?));
    if let Err(error) = written {
        return scene_error(error, output);
    }
    CliResult {
        schema_version: 1,
        ok: conflicts.is_empty(),
        data: json!({ "output": output, "conflicts": conflicts }),
        diagnostics: conflicts
            .iter()
            .map(|conflict| Diagnostic {
                code: "SCENE_MERGE_CONFLICT",
                severity: "error",
                message: format!(
                    "both sides changed `{}` of {}; kept ours",
                    conflict.field,
                    match (&conflict.name, &conflict.entity) {
                        (Some(name), _) => format!("`{name}`"),
                        (None, Some(id)) => format!("entity {id}"),
                        (None, None) => "the scene".to_owned(),
                    }
                ),
                file: Some(output.to_owned()),
                ..Diagnostic::default()
            })
            .collect(),
    }
}

pub fn inspect_project(root: &Path) -> CliResult {
    match open_project(root) {
        Ok(project) => CliResult::success(project_data(&project)),
        Err(error) => project_error(error, root),
    }
}

fn scene_warnings(path: &Path, document: &SceneDocument) -> Vec<Diagnostic> {
    let mut diagnostics = missing_asset_diagnostics(path, document);
    diagnostics.extend(terrain_layer_diagnostics(path, document));
    diagnostics.extend(bodiless_collider_diagnostics(document));
    diagnostics
}

/// A `SCENE_COLLIDER_WITHOUT_BODY` per collider with no `physics_body`.
/// Physics, raycasts and `aim` skip such a collider. Player and platformer
/// controllers are exempt: their collider is only their own shape.
fn bodiless_collider_diagnostics(document: &SceneDocument) -> Vec<Diagnostic> {
    use crate::runtime::{
        PLATFORMER_CONTROLLER_COMPONENT, PLAYER_CONTROLLER_COMPONENT,
    };
    document
        .entities
        .iter()
        .enumerate()
        .filter(|(_, entity)| {
            entity.collider.is_some()
                && entity.physics_body.is_none()
                && !entity.components.contains_key(PLAYER_CONTROLLER_COMPONENT)
                && !entity
                    .components
                    .contains_key(PLATFORMER_CONTROLLER_COMPONENT)
        })
        .map(|(index, entity)| Diagnostic {
            code: "SCENE_COLLIDER_WITHOUT_BODY",
            severity: "warning",
            message: format!(
                "`{}` has a collider but no physics_body, so physics, raycasts and aim ignore it; add `\"physics_body\": {{\"simulation\": \"Static\", \"solver\": \"Full\"}}` (\"Cpu\" for a moving body)",
                entity.name.as_deref().unwrap_or("unnamed entity")
            ),
            scene_location: Some(format!("/entities/{index}/collider")),
            entity: Some(EntityRef::of(entity)),
            ..Diagnostic::default()
        })
        .collect()
}

fn missing_asset_diagnostics(
    path: &Path,
    document: &SceneDocument,
) -> Vec<Diagnostic> {
    let base = path.parent().unwrap_or(Path::new("."));
    asset_references(document)
        .into_iter()
        .filter_map(|(asset, index)| {
            let resolved = if asset.is_absolute() {
                asset.clone()
            } else {
                base.join(&asset)
            };
            (!resolved.is_file()).then(|| Diagnostic {
                code: "SCENE_MISSING_ASSET",
                severity: "warning",
                message: format!(
                    "referenced asset `{}` does not exist",
                    asset.display()
                ),
                file: Some(resolved),
                scene_location: Some(format!(
                    "/entities/{index}/mesh_renderer"
                )),
                entity: Some(EntityRef::of(&document.entities[index])),
                ..Diagnostic::default()
            })
        })
        .collect()
}

/// A `SCENE_MISSING_ASSET` per terrain layer image that does not exist, and
/// a `SCENE_TERRAIN_ROOT_PATH` per one found only relative to the project
/// root instead of the scene file.
fn terrain_layer_diagnostics(
    path: &Path,
    document: &SceneDocument,
) -> Vec<Diagnostic> {
    let base = path.parent().unwrap_or(Path::new("."));
    let base = std::path::absolute(base).unwrap_or_else(|_| base.to_owned());
    let mut diagnostics = Vec::new();
    for (index, entity) in document.entities.iter().enumerate() {
        let Some(mut terrain) = entity
            .components
            .get(crate::runtime::TERRAIN_COMPONENT)
            .and_then(|text| serde_json::from_str::<Value>(text).ok())
        else {
            continue;
        };
        for layer in crate::runtime::terrain_layer_paths(&mut terrain) {
            let (resolved, legacy) = crate::runtime::resolve_terrain_layer(
                &base,
                Path::new(layer.as_str()),
            );
            let (code, message) = if !resolved.is_file() {
                (
                    "SCENE_MISSING_ASSET",
                    format!("terrain layer image `{layer}` does not exist"),
                )
            } else if legacy {
                (
                    "SCENE_TERRAIN_ROOT_PATH",
                    format!(
                        "terrain layer image `{layer}` is relative to the project root; write it relative to the scene file"
                    ),
                )
            } else {
                continue;
            };
            diagnostics.push(Diagnostic {
                code,
                severity: "warning",
                message,
                file: Some(resolved),
                scene_location: Some(format!(
                    "/entities/{index}/components/rusting.terrain"
                )),
                entity: Some(EntityRef::of(entity)),
                ..Diagnostic::default()
            });
        }
    }
    diagnostics
}

/// A `SCENE_MISSING_ASSET` per sound cue whose clip is not a file under
/// `assets/`. Clips are relative to `assets/`, unlike the scene-relative
/// asset `reference`, so a pasted `../assets/...` path is caught here.
fn sound_clip_diagnostics(
    root: &Path,
    document: &SceneDocument,
) -> Vec<Diagnostic> {
    document
        .entities
        .iter()
        .enumerate()
        .filter_map(|(index, entity)| {
            let cue: crate::runtime::SoundCue = serde_json::from_str(
                entity.components.get("rusting.sound_cue")?,
            )
            .ok()?;
            if crate::sfx::clip(&cue.clip).is_some() {
                return None;
            }
            let resolved = root.join("assets").join(&cue.clip);
            (!resolved.is_file()).then(|| Diagnostic {
                code: "SCENE_MISSING_ASSET",
                severity: "warning",
                message: format!(
                    "sound clip `{}` is not a file under assets/; clips are relative to assets/ (`sounds/hit.wav`), not to the scene",
                    cue.clip
                ),
                file: Some(resolved),
                scene_location: Some(format!(
                    "/entities/{index}/components/rusting.sound_cue"
                )),
                entity: Some(EntityRef::of(entity)),
                ..Diagnostic::default()
            })
        })
        .collect()
}

/// A `CODE_MISSING_ASSET` per literal path in game code, such as
/// `load_text("charts/easy.json")` or `play_sound("sfx/hit.wav", ..)`, that
/// is not a file under `assets/`. Paths built at run time are not checked.
fn code_asset_diagnostics(root: &Path) -> Vec<Diagnostic> {
    code_asset_literals(root)
        .into_iter()
        .filter(|(_, _, path)| !root.join("assets").join(path).is_file())
        .map(|(file, line, path)| Diagnostic {
            code: "CODE_MISSING_ASSET",
            severity: "warning",
            message: format!(
                "{}:{line}: `{path}` is not a file under assets/",
                file.strip_prefix(root).unwrap_or(&file).display(),
            ),
            file: Some(file),
            ..Diagnostic::default()
        })
        .collect()
}

/// Every literal asset path in game code as (file, 1-based line, path), in
/// file order.
fn code_asset_literals(root: &Path) -> Vec<(PathBuf, usize, String)> {
    code_literals(
        root,
        &[
            "load_text(\"",
            "play_sound(\"",
            "play_sound_looped(\"",
            "play_sound_with(\"",
            "spawn_prefab(\"",
        ],
    )
}

/// The first string argument of every `calls` entry (written up to its
/// opening quote) in game code as (file, 1-based line, literal).
fn code_literals(root: &Path, calls: &[&str]) -> Vec<(PathBuf, usize, String)> {
    let mut files = Vec::new();
    rust_files(&root.join("src"), &mut files);
    files.sort();
    let mut literals = Vec::new();
    for file in files {
        let Ok(text) = std::fs::read_to_string(&file) else {
            continue;
        };
        for (line, content) in text.lines().enumerate() {
            for call in calls {
                for (start, _) in content.match_indices(call) {
                    let rest = &content[start + call.len()..];
                    if let Some(path) = rest.split('"').next() {
                        literals.push((
                            file.clone(),
                            line + 1,
                            path.to_owned(),
                        ));
                    }
                }
            }
        }
    }
    literals
}

/// `rusting project summary`: every scene and prefab with the components
/// and assets it uses, every game code file with its `GameScene` functions
/// and the asset paths it names, and how many entities use each component.
/// While the JSON is over `budget` tokens the longest list is halved; the
/// cut item count is `data.omitted`.
pub fn project_summary(root: &Path, budget: usize) -> CliResult {
    let project = match open_project(root) {
        Ok(project) => project,
        Err(error) => return project_error(error, root),
    };
    let root = &project.root;
    let relative =
        |path: &Path| path.strip_prefix(root).unwrap_or(path).to_owned();
    let mut scene_files = Vec::new();
    for folder in ["scenes", "assets"] {
        scene_files_in(&root.join(folder), &mut scene_files);
    }
    scene_files.sort();
    let mut component_entities = BTreeMap::<String, usize>::new();
    let mut diagnostics = Vec::new();
    let mut scenes = Vec::new();
    for path in &scene_files {
        let document = match read_scene(path) {
            Ok(document) => document,
            Err(result) => {
                diagnostics.extend(result.diagnostics);
                continue;
            }
        };
        let mut used = BTreeSet::new();
        for entity in &document.entities {
            let names = entity_components(entity);
            for name in &names {
                *component_entities.entry(name.clone()).or_default() += 1;
            }
            used.extend(names);
        }
        let assets: BTreeSet<_> = asset_references(&document)
            .into_iter()
            .map(|(path, _)| path)
            .collect();
        scenes.push(json!({
            "path": relative(path),
            "entities": document.entities.len(),
            "components": used,
            "assets": assets,
        }));
    }
    let mut code_files = Vec::new();
    rust_files(&root.join("src"), &mut code_files);
    code_files.sort();
    let literals = code_asset_literals(root);
    let systems: Vec<_> = code_files
        .iter()
        .map(|file| {
            let text = std::fs::read_to_string(file).unwrap_or_default();
            let assets: BTreeSet<_> = literals
                .iter()
                .filter(|(owner, _, _)| owner == file)
                .map(|(_, _, path)| path)
                .collect();
            json!({
                "file": relative(file),
                "functions": game_scene_functions(&text),
                "assets": assets,
            })
        })
        .collect();
    let components: Vec<_> = component_entities
        .into_iter()
        .map(|(name, entities)| json!({"name": name, "entities": entities}))
        .collect();
    let mut data = json!({
        "root": root,
        "main_scene": relative(&project.scene_path),
        "imported_assets": crate::asset_import::list_assets(root).assets.len(),
        "scenes": scenes,
        "systems": systems,
        "components": components,
    });
    let mut omitted = 0;
    while crate::docs::tokens(&data.to_string()) > budget {
        let Some(list) = longest_list(&data, String::new())
            .and_then(|(_, pointer)| data.pointer_mut(&pointer))
            .and_then(Value::as_array_mut)
        else {
            break;
        };
        let keep = list.len() / 2;
        omitted += list.len() - keep;
        list.truncate(keep);
    }
    data["omitted"] = json!(omitted);
    CliResult {
        diagnostics,
        ..CliResult::success(data)
    }
}

/// The built-in sections (`collider`, `camera`, ...) and named components
/// (`rusting.sound_cue`, game components) an entity has.
fn entity_components(entity: &SceneEntity) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    if let Ok(Value::Object(fields)) = serde_json::to_value(entity) {
        names.extend(fields.into_iter().filter_map(|(key, value)| {
            let skip = matches!(
                key.as_str(),
                "id" | "parent" | "name" | "classes" | "components"
            );
            (!skip && !value.is_null()).then_some(key)
        }));
    }
    names.extend(entity.components.keys().cloned());
    names
}

/// What `rusting lease` does to a path.
pub enum LeaseAction {
    Claim(std::time::Duration),
    Release,
}

/// Claims or releases a lease on `path` for `holder` (default
/// `RUSTING_AGENT`). A lease held by someone else is `LEASE_HELD`.
pub fn lease(
    path: &Path,
    holder: Option<&str>,
    action: LeaseAction,
) -> CliResult {
    use crate::runtime::lease;
    let Some(holder) = holder.map(str::to_owned).or_else(lease::current_agent)
    else {
        return CliResult::failure(
            "CLI_USAGE",
            format!("name the holder with --as NAME or {}", lease::AGENT_ENV),
            None,
        );
    };
    let done = match action {
        LeaseAction::Claim(duration) => lease::claim(path, &holder, duration)
            .map(|lease| json!({ "lease": lease })),
        LeaseAction::Release => {
            lease::release(path, &holder).map(|()| json!({ "released": path }))
        }
    };
    match done {
        Ok(data) => CliResult::success(data),
        Err(error) => CliResult::failure(
            if error.kind() == std::io::ErrorKind::ResourceBusy {
                "LEASE_HELD"
            } else {
                "LEASE_IO"
            },
            error.to_string(),
            Some(path.to_owned()),
        ),
    }
}

/// Commands that write no file: the only ones `--read-only` lets run,
/// besides `scene patch` and `fix` with `--dry-run`. `inspect --tick`, `check`, `run` and `test`
/// build the game into `target/` and `build/`, so they are not here.
pub const READ_ONLY_COMMANDS: &[&[&str]] = &[
    &["--version"],
    &["-V"],
    &["version"],
    &["doctor"],
    &["project", "inspect"],
    &["project", "summary"],
    &["impact"],
    &["lease", "list"],
    &["provenance"],
    &["log"],
    &["systems"],
    &["docs"],
    &["explain"],
    &["schema"],
    &["validate"],
    &["lint"],
    &["diff"],
    &["scene", "inspect"],
    &["scene", "map"],
    &["scene", "query"],
    &["asset", "list"],
    &["preset", "list"],
    &["effect", "list"],
    &["recipe", "list"],
];

/// Environment variable that turns on read-only mode like `--read-only`.
pub const READ_ONLY_ENV: &str = "RUSTING_READ_ONLY";

/// `READ_ONLY` for a command that may write files, so read-only mode
/// refuses it before it starts.
pub fn read_only_refusal(command: &[&str]) -> Option<CliResult> {
    // Only the dry-run shapes the command line parses as dry runs, so a
    // `--dry-run` passed as some flag's value does not count.
    let dry_run = matches!(
        command,
        ["scene", "patch", _, _, "--dry-run"]
            | ["scene", "patch", _, "--dry-run", _]
            | ["fix", _, "--dry-run"]
            | ["fix", "--dry-run", _]
    );
    let allowed = dry_run
        || READ_ONLY_COMMANDS
            .iter()
            .any(|words| command.starts_with(words));
    (!allowed).then(|| {
        let name = command
            .iter()
            .take_while(|word| !word.starts_with('-'))
            .take(2)
            .copied()
            .collect::<Vec<_>>()
            .join(" ");
        CliResult::failure(
            "READ_ONLY",
            format!(
                "`{name}` can write files, and --read-only (or {READ_ONLY_ENV}) allows only commands that do not: {}, and scene patch or fix with --dry-run",
                READ_ONLY_COMMANDS
                    .iter()
                    .map(|words| words.join(" "))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            None,
        )
    })
}

/// Environment variable that confines commands to a folder like
/// `--confine`.
pub const CONFINE_ENV: &str = "RUSTING_CONFINE";

/// Environment variable that keeps builds off the network like
/// `--offline`.
pub const OFFLINE_ENV: &str = "RUSTING_OFFLINE";

/// Environment variable naming a file that every `rusting` command appends
/// one JSON line to: its arguments, `ok`, diagnostic codes and duration.
/// The agent benchmark runner reads it to count commands and rebuilds.
pub const COMMAND_LOG_ENV: &str = "RUSTING_COMMAND_LOG";

/// `OUTSIDE_CONFINE` when the working folder or an argument that names a path is
/// outside `dir` (a canonical folder). An argument counts as a path when
/// it exists or holds a path separator; any other word is at most a path
/// relative to the working folder, so it stays inside with it.
pub fn confine_refusal(command: &[&str], dir: &Path) -> Option<CliResult> {
    let outside = std::iter::once(&".").chain(command).find(|arg| {
        let path = Path::new(arg);
        (path.exists()
            || arg.contains('/')
            || arg.contains(std::path::MAIN_SEPARATOR))
            && !crate::runtime::lease::resolve(path)
                .is_some_and(|real| real.starts_with(dir))
    })?;
    Some(CliResult::failure(
        "OUTSIDE_CONFINE",
        format!(
            "`{outside}` is outside {}, the folder --confine (or {CONFINE_ENV}) limits commands to",
            dir.display()
        ),
        None,
    ))
}

/// Operations recorded in the project's journal, newest first, each with
/// the files it changed and their content hashes before and after.
pub fn journal_log(root: &Path) -> CliResult {
    let mut operations: Vec<Value> = Vec::new();
    for entry in crate::runtime::journal::entries(root) {
        let change = json!({"file": entry.file, "before": entry.before, "after": entry.after});
        match operations
            .iter_mut()
            .find(|op| op["op"] == entry.op.as_str())
        {
            Some(op) => op["files"].as_array_mut().unwrap().push(change),
            None => operations.push(json!({
                "op": entry.op,
                "time": entry.time,
                "tool": entry.tool,
                "command": entry.command,
                "files": [change],
            })),
        }
    }
    operations.reverse();
    CliResult::success(json!({ "journal": operations }))
}

/// Undoes one journaled operation; see [`crate::runtime::journal::revert`].
pub fn revert(root: &Path, op: &str) -> CliResult {
    use crate::runtime::journal::RevertError;
    match crate::runtime::journal::revert(root, op) {
        Ok(files) => CliResult::success(json!({ "reverted": op, "files": files })),
        Err(RevertError::UnknownOperation(op)) => CliResult::failure(
            "JOURNAL_UNKNOWN_OP",
            format!("no operation `{op}` in the journal; `rusting log` lists them"),
            None,
        ),
        Err(RevertError::Changed { file, by }) => CliResult::failure(
            "REVERT_CONFLICT",
            match by {
                Some(by) => format!("`{file}` was changed again by operation `{by}`; revert that one first"),
                None => format!("`{file}` was changed outside the journal since"),
            },
            Some(root.join(file)),
        ),
        Err(RevertError::Io(error)) if error.kind() == std::io::ErrorKind::ResourceBusy => {
            CliResult::failure("LEASE_HELD", error.to_string(), None)
        }
        Err(RevertError::Io(error)) => CliResult::failure("IO_ERROR", error.to_string(), None),
    }
}

/// Live leases of the project at `root`.
pub fn lease_list(root: &Path) -> CliResult {
    CliResult::success(json!({ "leases": crate::runtime::lease::leases(root) }))
}

/// `rusting impact`: what a change to `target` touches. A target that is a
/// file (relative to the project or to `assets/`) matches scenes that
/// reference it and code that names it; any other target is a component
/// name, matching entities that have it and code lines that mention it.
/// Scenarios run the main scene and the game code, so a hit in either
/// affects every scenario; otherwise only scenarios whose text names the
/// target are listed (`scenario_files`).
pub fn impact(root: &Path, target: &str) -> CliResult {
    let project = match open_project(root) {
        Ok(project) => project,
        Err(error) => return project_error(error, root),
    };
    let root = &project.root;
    let relative =
        |path: &Path| path.strip_prefix(root).unwrap_or(path).to_owned();
    let canonical = |path: PathBuf| path.canonicalize().ok();
    let file = [root.join(target), root.join("assets").join(target)]
        .into_iter()
        .find(|path| path.is_file())
        .and_then(canonical);
    let mut scene_files = Vec::new();
    for folder in ["scenes", "assets"] {
        scene_files_in(&root.join(folder), &mut scene_files);
    }
    scene_files.sort();
    let main_scene = canonical(project.scene_path.clone());
    let mut main_hit = false;
    let mut scenes = Vec::new();
    for path in &scene_files {
        let Ok(document) = read_scene(path) else {
            continue;
        };
        let base = path.parent().unwrap_or(root);
        let mut entities: BTreeSet<String> = BTreeSet::new();
        let name = |index: usize| {
            let entity = &document.entities[index];
            entity.name.clone().unwrap_or_else(|| entity.id.to_string())
        };
        match &file {
            Some(file) => entities.extend(
                asset_references(&document)
                    .into_iter()
                    .filter(|(asset, _)| {
                        canonical(base.join(asset)).as_ref() == Some(file)
                    })
                    .map(|(_, index)| name(index)),
            ),
            None => entities.extend(
                (0..document.entities.len())
                    .filter(|&index| {
                        entity_components(&document.entities[index])
                            .contains(target)
                    })
                    .map(name),
            ),
        }
        let itself = canonical(path.clone()) == file && file.is_some();
        if entities.is_empty() && !itself {
            continue;
        }
        main_hit |= canonical(path.clone()) == main_scene;
        // ponytail: first 10 names; `scene query --component` lists all.
        let count = entities.len();
        let entities: Vec<_> = entities.into_iter().take(10).collect();
        scenes.push(json!({
            "path": relative(path),
            "entity_count": count,
            "entities": entities,
        }));
    }
    let code: Vec<_> = match &file {
        Some(file) => code_asset_literals(root)
            .into_iter()
            .filter(|(_, _, path)| {
                canonical(root.join("assets").join(path)).as_ref() == Some(file)
            })
            .map(|(source, line, _)| (source, line))
            .collect(),
        None => {
            let mut sources = Vec::new();
            rust_files(&root.join("src"), &mut sources);
            sources.sort();
            sources
                .into_iter()
                .flat_map(|source| {
                    let text =
                        std::fs::read_to_string(&source).unwrap_or_default();
                    text.lines()
                        .enumerate()
                        .filter(|(_, line)| line.contains(target))
                        .map(|(line, _)| (source.clone(), line + 1))
                        .collect::<Vec<_>>()
                })
                .collect()
        }
    };
    let code: Vec<_> = code
        .into_iter()
        .map(|(source, line)| json!({"file": relative(&source), "line": line}))
        .collect();
    let every = main_hit || !code.is_empty();
    let mut scenarios: Vec<_> = std::fs::read_dir(root.join("tests"))
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .filter(|path| {
            every
                || std::fs::read_to_string(path)
                    .is_ok_and(|text| text.contains(target))
        })
        .map(|path| relative(&path))
        .collect();
    scenarios.sort();
    CliResult::success(json!({
        "target": target,
        "kind": if file.is_some() { "file" } else { "component" },
        "scenes": scenes,
        "code": code,
        "scenario_files": scenarios,
    }))
}

fn scene_files_in(folder: &Path, files: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(folder).into_iter().flatten().flatten() {
        let path = entry.path();
        if path.extension().is_some_and(|ext| ext == "rscene") {
            files.push(path);
        } else if path.is_dir() {
            scene_files_in(&path, files);
        }
    }
}

/// Names of the functions in Rust source whose signature names `GameScene`.
fn game_scene_functions(text: &str) -> Vec<String> {
    let mut names = Vec::new();
    for (start, _) in text.match_indices("fn ") {
        if text[..start]
            .chars()
            .next_back()
            .is_some_and(|c| c.is_alphanumeric() || c == '_')
        {
            continue;
        }
        let rest = &text[start + 3..];
        let signature = &rest[..rest.find(['{', ';']).unwrap_or(rest.len())];
        if signature.contains("GameScene") {
            names.push(
                signature
                    .chars()
                    .take_while(|c| c.is_alphanumeric() || *c == '_')
                    .collect(),
            );
        }
    }
    names
}

/// JSON pointer of the longest array with more than one item in `value`.
fn longest_list(value: &Value, pointer: String) -> Option<(usize, String)> {
    let children: Vec<_> = match value {
        Value::Array(items) => items
            .iter()
            .enumerate()
            .map(|(index, item)| (item, format!("{pointer}/{index}")))
            .collect(),
        Value::Object(fields) => fields
            .iter()
            .map(|(key, item)| (item, format!("{pointer}/{key}")))
            .collect(),
        _ => return None,
    };
    let own = value
        .as_array()
        .filter(|items| items.len() > 1)
        .map(|items| (items.len(), pointer));
    children
        .into_iter()
        .filter_map(|(item, pointer)| longest_list(item, pointer))
        .chain(own)
        .max_by_key(|(len, _)| *len)
}

/// A portable record of what a build of the project is made of: the
/// engine and `rusting*` crates from `Cargo.lock`, a hash of the game code,
/// every imported asset with its hash and provenance, generator hooks,
/// scene hashes, and each scenario's seed, ticks and hash. Hashes are the
/// FNV-1a 64 that `.rmeta` files record, so two reports compare line by line.
pub fn provenance(root: &Path) -> CliResult {
    let project = match open_project(root) {
        Ok(project) => project,
        Err(error) => return project_error(error, root),
    };
    let root = &project.root;
    let relative = |path: &Path| {
        path.strip_prefix(root)
            .unwrap_or(path)
            .to_string_lossy()
            .replace('\\', "/")
    };
    // `Cargo.lock` is TOML; each package is a `[[package]]` block of
    // `key = "value"` lines.
    let lock =
        std::fs::read_to_string(root.join("Cargo.lock")).unwrap_or_default();
    let crates: Vec<Value> = lock
        .split("[[package]]")
        .filter_map(|block| {
            let field = |key: &str| {
                block.lines().find_map(|line| {
                    let value = line.strip_prefix(key)?.trim_start();
                    Some(value.strip_prefix('=')?.trim().trim_matches('"').to_owned())
                })
            };
            let name = field("name ")?;
            name.starts_with("rusting").then(|| {
                json!({"name": name, "version": field("version "), "source": field("source ")})
            })
        })
        .collect();
    let mut code = Vec::new();
    rust_files(&root.join("src"), &mut code);
    code.sort();
    let code_bytes: Vec<u8> = code
        .iter()
        .flat_map(|path| {
            let mut bytes = relative(path).into_bytes();
            bytes.extend(std::fs::read(path).unwrap_or_default());
            bytes
        })
        .collect();
    let assets: Vec<Value> = crate::asset_import::list_assets(root)
        .assets
        .into_iter()
        .map(|asset| {
            json!({
                "path": asset.path,
                "id": asset.id,
                "hash": file_revision(&root.join(&asset.path)),
                "source": asset.source,
            })
        })
        .collect();
    let mut scene_files = Vec::new();
    for folder in ["scenes", "assets"] {
        scene_files_in(&root.join(folder), &mut scene_files);
    }
    scene_files.sort();
    let scenes: Vec<Value> = scene_files
        .iter()
        .map(
            |path| json!({"path": relative(path), "hash": file_revision(path)}),
        )
        .collect();
    let mut scenario_files: Vec<_> = std::fs::read_dir(root.join("tests"))
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .collect();
    scenario_files.sort();
    let scenarios: Vec<Value> = scenario_files
        .iter()
        .map(|path| {
            let scenario: Value = std::fs::read(path)
                .ok()
                .and_then(|bytes| serde_json::from_slice(&bytes).ok())
                .unwrap_or_default();
            json!({
                "path": relative(path),
                "seed": scenario.get("seed").cloned().unwrap_or(json!(0)),
                "ticks": scenario["ticks"],
                "hash": file_revision(path),
            })
        })
        .collect();
    let generators: BTreeMap<_, _> = project
        .manifest
        .generators
        .iter()
        .map(|(name, hook)| {
            (
                name,
                json!({"command": hook.command, "license": hook.license}),
            )
        })
        .collect();
    CliResult::success(json!({
        "project": project.manifest.name,
        "tool_version": env!("CARGO_PKG_VERSION"),
        "crates": crates,
        "code_hash": crate::runtime::scene_revision(&code_bytes),
        "code_files": code.len(),
        "determinism": project.manifest.determinism,
        "assets": assets,
        "generators": generators,
        "scene_hashes": scenes,
        "scenario_hashes": scenarios,
    }))
}

fn rust_files(folder: &Path, files: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(folder).into_iter().flatten().flatten() {
        let path = entry.path();
        if path.is_dir() {
            rust_files(&path, files);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            files.push(path);
        }
    }
}

/// One error per scene key that loading ignores. A key within two edits of
/// exactly one unset known key, written once in the file, gets a rename fix.
fn dropped_field_diagnostics(
    path: &Path,
    document: &SceneDocument,
) -> Vec<Diagnostic> {
    let Some((text, raw)) =
        std::fs::read_to_string(path).ok().and_then(|text| {
            let raw = serde_json::from_str::<Value>(&text).ok()?;
            Some((text, raw))
        })
    else {
        return Vec::new();
    };
    crate::scene_patch::dropped_fields(&raw, document)
        .into_iter()
        .map(|field| {
            let entity = field.entity.map(|index| &document.entities[index]);
            let pointer = match field.entity {
                Some(index) => format!("/entities/{index}{}", field.path),
                None => field.path.clone(),
            };
            let key = field.path.rsplit('/').next().unwrap_or_default();
            let key = key.replace("~1", "/").replace("~0", "~");
            let fix = field
                .suggestion
                .as_ref()
                .filter(|_| key_positions(&text, &key).len() == 1)
                .map(|suggestion| {
                    json!({"op": "rename_key", "path": pointer, "to": suggestion})
                });
            let hint = match &field.suggestion {
                Some(suggestion) => format!("did you mean `{suggestion}`?"),
                None => format!("known fields: {}", field.known.join(", ")),
            };
            Diagnostic {
                code: "SCENE_UNKNOWN_FIELD",
                severity: "error",
                message: format!(
                    "`{key}` is not a field here and loading ignores it; {hint}"
                ),
                file: Some(path.to_owned()),
                scene_location: Some(pointer),
                entity: entity.map(EntityRef::of),
                fix,
                ..Diagnostic::default()
            }
        })
        .collect()
}

/// One error per simulation part in the main scene that does not support
/// the project's `determinism` mode, located at the first body using it.
fn determinism_diagnostics(
    project: &OpenProject,
    document: &SceneDocument,
) -> Vec<Diagnostic> {
    let mode = project.manifest.determinism;
    let mut parts = std::collections::BTreeMap::new();
    let mut first_body = std::collections::BTreeMap::new();
    for (index, entity) in document.entities.iter().enumerate() {
        let Some(body) = &entity.physics_body else {
            continue;
        };
        if let Some((part, supports)) =
            crate::runtime::body_part(body, Some(&project.root))
        {
            first_body.entry(part.clone()).or_insert(index);
            parts.insert(part, supports);
        }
    }
    let Err(error) = crate::runtime::check_parts(mode, &parts) else {
        return Vec::new();
    };
    error
        .offenders
        .into_iter()
        .map(|offender| Diagnostic {
            code: "DETERMINISM_UNSUPPORTED",
            severity: "error",
            message: format!(
                "`{}` supports determinism {:?}, but project.json asks for {mode:?}",
                offender.part, offender.supports
            ),
            file: Some(project.scene_path.clone()),
            scene_location: Some(format!(
                "/entities/{}/physics_body",
                first_body[&offender.part]
            )),
            entity: Some(EntityRef::of(
                &document.entities[first_body[&offender.part]],
            )),
            ..Diagnostic::default()
        })
        .collect()
}

fn read_scene(path: &Path) -> Result<SceneDocument, CliResult> {
    read_scene_document(path).map_err(|error| scene_error(error, path))
}

pub fn inspect_scene(path: &Path) -> CliResult {
    let document = match read_scene(path) {
        Ok(document) => document,
        Err(result) => return result,
    };
    // The runtime loader migrates old scenes in memory. Report both versions.
    let source_version = read_scene_bytes(path).ok().and_then(|bytes| {
        serde_json::from_slice::<Value>(&bytes)
            .ok()
            .and_then(|value| {
                value.get("format_version").and_then(Value::as_u64)
            })
    });
    let cameras: Vec<_> = document.entities.iter().filter_map(|entity| entity.camera.map(|camera| json!({"id": entity.id, "name": entity.name, "active": camera.active, "priority": camera.priority, "projection": camera.projection}))).collect();
    let classes: BTreeSet<_> = document
        .entities
        .iter()
        .flat_map(|entity| entity.classes.iter().cloned())
        .collect();
    let assets: BTreeSet<_> = asset_references(&document)
        .into_iter()
        .map(|(path, _)| path)
        .collect();
    let mut result = CliResult::success(json!({
        "path": path, "name": document.name, "revision": file_revision(path),
        "scene_version": source_version.unwrap_or(u64::from(document.format_version)),
        "loaded_scene_version": document.format_version,
        "entity_count": document.entities.len(), "cameras": cameras,
        "classes": classes, "referenced_assets": assets,
        "entities": document.entities.iter()
            .map(|entity| json!({"id": entity.id, "name": entity.name}))
            .collect::<Vec<_>>(),
    }));
    result.diagnostics = scene_warnings(path, &document);
    result
}

#[derive(Debug, Clone)]
pub enum SceneFilter {
    Id(Uuid),
    Name(String),
    Class(String),
    Component(String),
    All,
}

fn matches_filter(entity: &SceneEntity, filter: &SceneFilter) -> bool {
    match filter {
        SceneFilter::Id(id) => entity.id == *id,
        SceneFilter::Name(name) => entity.name.as_deref() == Some(name),
        SceneFilter::Class(class) => {
            entity.classes.iter().any(|candidate| candidate == class)
        }
        SceneFilter::Component(component) => match component.as_str() {
            "transform" => entity.transform.is_some(),
            "mesh_renderer" => entity.mesh_renderer.is_some(),
            "camera" => entity.camera.is_some(),
            "visible" => entity.visible.is_some(),
            "physics_body" => entity.physics_body.is_some(),
            "rigid_body" => entity.rigid_body.is_some(),
            "collider" => entity.collider.is_some(),
            "collision_layers" => entity.collision_layers.is_some(),
            "gpu_physics_watch" => entity.gpu_physics_watch.is_some(),
            "directional_light" => entity.directional_light.is_some(),
            "point_light" => entity.point_light.is_some(),
            "spot_light" => entity.spot_light.is_some(),
            name => entity.components.contains_key(name),
        },
        SceneFilter::All => true,
    }
}

pub fn query_scene(path: &Path, filter: &SceneFilter) -> CliResult {
    let document = match read_scene(path) {
        Ok(document) => document,
        Err(result) => return result,
    };
    let mut entities: Vec<_> = document
        .entities
        .iter()
        .filter(|entity| matches_filter(entity, filter))
        .collect();
    entities.sort_by_key(|entity| entity.id);
    // Registered components are stored as JSON text; print them as objects.
    let entities: Vec<Value> = entities
        .iter()
        .map(|entity| {
            let mut value = json!(entity);
            if let Some(map) = value["components"].as_object_mut() {
                for component in map.values_mut() {
                    if let Some(parsed) = component
                        .as_str()
                        .and_then(|text| serde_json::from_str(text).ok())
                    {
                        *component = parsed;
                    }
                }
            }
            value
        })
        .collect();
    let mut result = CliResult::success(
        json!({"path": path, "revision": file_revision(path), "count": entities.len(), "entities": entities}),
    );
    result.diagnostics = scene_warnings(path, &document);
    result
}

fn file_revision(path: &Path) -> Option<String> {
    read_scene_bytes(path)
        .ok()
        .map(|bytes| crate::runtime::scene_revision(&bytes))
}

/// Converts the scene at `path` to folder form (`split`: a `.rscene`
/// directory with one file per entity, for fewer merge conflicts) or back
/// to a single file. Does nothing when it already has that form.
pub fn convert_scene(path: &Path, split: bool) -> CliResult {
    // Canonical, so the first patch afterwards rewrites only what it changes.
    let bytes = match read_scene(path).and_then(|document| {
        serde_json::to_vec_pretty(&document)
            .map_err(|error| scene_error(error.into(), path))
    }) {
        Ok(bytes) => bytes,
        Err(result) => return result,
    };
    let changed = path.is_dir() != split;
    let converted = (|| {
        if !changed {
            return Ok(());
        }
        crate::runtime::lease::check_write(path)?;
        // Journal what the old form held, so `rusting revert` restores it.
        let mut old = Vec::new();
        let mut pending = vec![path.to_path_buf()];
        while let Some(next) = pending.pop() {
            // Links are removed, not followed: never read outside the scene.
            let kind = std::fs::symlink_metadata(&next)?.file_type();
            if kind.is_symlink() {
                continue;
            }
            if kind.is_dir() {
                for entry in std::fs::read_dir(&next)? {
                    pending.push(entry?.path());
                }
            } else {
                old.push((std::fs::read(&next)?, next));
            }
        }
        if split {
            std::fs::remove_file(path)?;
            std::fs::create_dir(path)?;
        } else {
            std::fs::remove_dir_all(path)?;
        }
        for (bytes, file) in &old {
            crate::runtime::journal::record(file, Some(bytes), None)?;
        }
        crate::runtime::write_atomic(path, &bytes)
    })();
    match converted {
        Ok(()) => CliResult::success(json!({
            "scene": path,
            "form": if split { "folder" } else { "file" },
            "changed": changed,
        })),
        Err(error) => scene_error(error.into(), path),
    }
}

pub fn patch_scene(path: &Path, patch_path: &Path, dry_run: bool) -> CliResult {
    let patch = match std::fs::read(patch_path)
        .map_err(|error| error.to_string())
        .and_then(|bytes| {
            serde_json::from_slice::<crate::scene_patch::ScenePatch>(&bytes)
                .map_err(|error| error.to_string())
        }) {
        Ok(patch) => patch,
        Err(message) => {
            return CliResult::failure(
                "PATCH_JSON",
                message,
                Some(patch_path.to_owned()),
            )
        }
    };
    apply_scene_patch(path, &patch, Some(patch_path), dry_run)
}

/// Places a glTF or GLB model in a scene as one object named `name`, with a
/// child per glTF node and per extra primitive, through the patch path.
/// The model's node animations become `rusting.animation` clips on that
/// object; the first one autoplays.
/// Mesh and texture pieces are written next to the model file, so keep the
/// model under `assets/` (`rusting asset import` puts it there).
#[cfg(feature = "gltf")]
pub fn add_model(
    scene: &Path,
    model: &Path,
    name: &str,
    dry_run: bool,
) -> CliResult {
    use crate::runtime::{hierarchy::set_parent, RenderExtractPlugin, SceneId};
    use crate::{App, AssetPlugin};
    let mut app = App::new();
    let entities = app
        .add_plugin(AssetPlugin)
        .and_then(|app| app.add_plugin(RenderExtractPlugin))
        .map_err(|error| error.to_string())
        .and_then(|app| {
            let world = app.world_mut();
            let mut assets = world.resource_mut::<crate::assets::AssetServer>();
            let mut nodes = assets
                .import_gltf_scene(model)
                .map_err(|error| error.to_string())?;
            let mut taken: std::collections::HashSet<String> =
                crate::runtime::read_scene_document(scene)
                    .map(|document| {
                        document
                            .entities
                            .into_iter()
                            .filter_map(|entity| entity.name)
                            .collect()
                    })
                    .unwrap_or_default();
            if !taken.insert(name.to_owned()) {
                return Err(format!(
                    "scene already has an object named `{name}`; pass \
                     --name to name the model's root differently"
                ));
            }
            unique_node_names(&mut nodes, &mut taken);
            let clips = assets
                .import_gltf_animations(model, &nodes)
                .map_err(|error| error.to_string())?;
            let root = world
                .spawn((
                    SceneId::new(),
                    crate::runtime::Name(name.into()),
                    crate::Transform::default(),
                ))
                .id();
            if let Some(first) = clips.first() {
                world.entity_mut(root).insert(crate::runtime::Animation {
                    autoplay: first.name.clone(),
                    clips,
                    ..crate::runtime::Animation::default()
                });
            }
            let spawned =
                crate::assets::spawn_gltf_nodes_in_world(world, &nodes, None)
                    .map_err(|error| error.to_string())?;
            for entity in spawned {
                if world.get::<crate::runtime::Parent>(entity).is_none() {
                    set_parent(world, entity, root)
                        .map_err(|error| error.to_string())?;
                }
            }
            let mut document = crate::runtime::scene_document(world, name)
                .map_err(|error| error.to_string())?;
            let folder = scene
                .parent()
                .filter(|folder| !folder.as_os_str().is_empty());
            crate::runtime::relativize_scene_assets(
                &mut document,
                folder.unwrap_or(Path::new(".")),
            )
            .map_err(|error| error.to_string())?;
            Ok(document.entities)
        });
    let entities = match entities {
        Ok(entities) => entities,
        Err(message) => {
            return CliResult::failure(
                "MODEL_IMPORT",
                message,
                Some(model.to_owned()),
            )
        }
    };
    let patch = crate::scene_patch::ScenePatch {
        expected_revision: None,
        operations: entities
            .iter()
            .map(|entity| crate::scene_patch::PatchOperation::Create {
                entity: crate::scene_patch::entity_form(entity),
            })
            .collect(),
    };
    apply_scene_patch(scene, &patch, None, dry_run)
}

/// Gives every node and extra primitive a name not in `taken`, adding
/// " 2", " 3" and so on to repeats, since scene names must be unique.
#[cfg(feature = "gltf")]
fn unique_node_names(
    nodes: &mut [crate::assets::ImportedGltfNode],
    taken: &mut std::collections::HashSet<String>,
) {
    let mut unique = |name: &mut String| {
        let base = name.clone();
        let mut count = 1;
        while !taken.insert(name.clone()) {
            count += 1;
            *name = format!("{base} {count}");
        }
    };
    for node in nodes {
        unique(&mut node.name);
        for primitive in node.primitives.iter_mut().skip(1) {
            unique(&mut primitive.name);
        }
    }
}

/// Copies clip `clip` of object `from` onto object `to`'s skeleton (see
/// `runtime::retarget_clip`) and saves it on `to`'s animation through the
/// patch path, replacing a clip of the same name. Rest poses are the
/// scene's transforms.
pub fn retarget_clip(
    scene: &Path,
    from: &str,
    clip: &str,
    to: &str,
    dry_run: bool,
) -> CliResult {
    use crate::runtime::{
        load_scene, Animation, Name, RenderExtractPlugin, SceneLoadMode,
    };
    use crate::{App, AssetPlugin};
    let mut app = App::new();
    if let Err(error) = app
        .add_plugin(AssetPlugin)
        .and_then(|app| app.add_plugin(RenderExtractPlugin))
    {
        return CliResult::failure("RETARGET_FAILED", error.to_string(), None);
    }
    let world = app.world_mut();
    world
        .resource_mut::<crate::runtime::SceneComponentRegistry>()
        .keep_unregistered();
    if let Err(error) = load_scene(world, scene, SceneLoadMode::Replace) {
        return scene_error(error, scene);
    }
    let names: Vec<_> = world
        .query::<(bevy_ecs::entity::Entity, &Name)>()
        .iter(world)
        .map(|(entity, name)| (entity, name.0.clone()))
        .collect();
    let named = |name: &str| {
        names
            .iter()
            .find(|(_, n)| n == name)
            .map(|(entity, _)| *entity)
            .ok_or_else(|| format!("no object is named `{name}`"))
    };
    let world = &*world;
    let animation = named(from).and_then(|source| {
        let target = named(to)?;
        let clip = crate::runtime::retarget_clip(world, source, clip, target)?;
        let mut animation = world
            .get::<Animation>(target)
            .cloned()
            .unwrap_or_else(|| Animation {
                clips: Vec::new(),
                autoplay: String::new(),
                ..Animation::default()
            });
        match animation.clip(&clip.name) {
            Some(i) => animation.clips[i] = clip,
            None => animation.clips.push(clip),
        }
        Ok(animation)
    });
    let animation = match animation {
        Ok(animation) => animation,
        Err(message) => {
            return CliResult::failure(
                "RETARGET_FAILED",
                message,
                Some(scene.to_owned()),
            )
        }
    };
    let patch = crate::scene_patch::ScenePatch {
        expected_revision: None,
        operations: vec![crate::scene_patch::PatchOperation::Set {
            id: crate::scene_patch::EntityRef::Name(to.into()),
            path: format!(
                "/components/{}",
                crate::runtime::ANIMATION_COMPONENT
            ),
            value: serde_json::to_value(&animation)
                .expect("animations serialize"),
            expected: None,
        }],
    };
    apply_scene_patch(scene, &patch, None, dry_run)
}

/// `patch_file` is where the patch came from, if it is a file, so an
/// operation error can point into it.
fn apply_scene_patch(
    path: &Path,
    patch: &crate::scene_patch::ScenePatch,
    patch_file: Option<&Path>,
    dry_run: bool,
) -> CliResult {
    match crate::scene_patch::patch_scene_file(path, patch, dry_run) {
        Ok(outcome) => {
            let mut result = CliResult::success(
                json!({"path": path, "dry_run": dry_run, "patch": outcome}),
            );
            result.diagnostics = outcome
                .unvalidated_components
                .iter()
                .map(|name| Diagnostic {
                    code: "PATCH_UNVALIDATED_COMPONENT",
                    severity: "warning",
                    message: format!(
                        "component `{name}` is not built in; the game validates it on load"
                    ),
                    file: Some(path.to_owned()),
                    ..Diagnostic::default()
                })
                .collect();
            result
        }
        Err(error) => {
            use crate::scene_patch::PatchError;
            let mut result = CliResult::failure(
                error.code(),
                error.to_string(),
                Some(path.to_owned()),
            );
            let diagnostic = &mut result.diagnostics[0];
            match &error {
                PatchError::Operation { operation, .. }
                | PatchError::Field { operation, .. } => {
                    if let Some(patch_file) = patch_file {
                        diagnostic.file = Some(patch_file.to_owned());
                        diagnostic.scene_location =
                            Some(format!("/operations/{operation}"));
                    }
                }
                PatchError::Scene(error) => {
                    locate_scene_error(error, path, diagnostic);
                }
                PatchError::InvalidObject { id, name, .. } => {
                    diagnostic.entity = Some(EntityRef {
                        id: *id,
                        name: name.clone(),
                    });
                }
                _ => {}
            }
            if let PatchError::Field { id, .. } = &error {
                diagnostic.entity = read_scene_document(path)
                    .ok()
                    .and_then(|scene| {
                        scene
                            .entities
                            .iter()
                            .find(|e| e.id == *id)
                            .map(EntityRef::of)
                    })
                    .or(Some(EntityRef {
                        id: *id,
                        name: None,
                    }));
            }
            result.data =
                json!({"path": path, "revision": file_revision(path)});
            result
        }
    }
}

pub fn validate_project(root: &Path) -> CliResult {
    let project = match open_project(root) {
        Ok(project) => project,
        Err(error) => return project_error(error, root),
    };
    let document = match read_scene(&project.scene_path) {
        Ok(document) => document,
        Err(result) => return result,
    };
    let mut diagnostics = scene_warnings(&project.scene_path, &document);
    // Imported-asset errors (missing files or dependencies, bad metadata)
    // block shipping; license and not-imported warnings do not.
    diagnostics.extend(
        list_assets(&project.root)
            .diagnostics
            .into_iter()
            .filter(|d| d.severity == "error"),
    );
    diagnostics.extend(determinism_diagnostics(&project, &document));
    diagnostics
        .extend(dropped_field_diagnostics(&project.scene_path, &document));
    diagnostics.extend(sound_clip_diagnostics(&project.root, &document));
    diagnostics.extend(code_asset_diagnostics(&project.root));
    let valid = diagnostics.is_empty();
    CliResult {
        schema_version: 1,
        ok: valid,
        data: json!({"root": project.root, "main_scene": project.scene_path, "entity_count": document.entities.len()}),
        diagnostics: diagnostics
            .into_iter()
            .map(|mut d| {
                d.severity = "error";
                d
            })
            .collect(),
    }
}

/// `rusting lint`: presentation checks on the main scene, as warnings.
pub fn lint_project(root: &Path) -> CliResult {
    let project = match open_project(root) {
        Ok(project) => project,
        Err(error) => return project_error(error, root),
    };
    let document = match read_scene(&project.scene_path) {
        Ok(document) => document,
        Err(result) => return result,
    };
    let mut diagnostics = lint_scene_with_fill(
        &document,
        &code_strings(&project.root),
        button_fill(&project.root),
    );
    let mut files = Vec::new();
    rust_files(&project.root.join("src"), &mut files);
    let code: String = files
        .iter()
        .filter_map(|file| std::fs::read_to_string(file).ok())
        .collect();
    diagnostics.extend(lint_no_ending(&document, &code));
    diagnostics.extend(lint_silent_goal(&document, &code));
    diagnostics.extend(lint_missing_names(&project.root));
    diagnostics.extend(lint_color_only_status(&project.root));
    diagnostics.extend(lint_missing_translations(&project.root, &document));
    diagnostics
        .extend(terrain_layer_diagnostics(&project.scene_path, &document));
    CliResult {
        schema_version: 1,
        ok: diagnostics.is_empty(),
        data: json!({"root": project.root, "main_scene": project.scene_path, "warnings": diagnostics.len()}),
        diagnostics,
    }
}

/// `LINT_MISSING_TRANSLATION` per key that a `tr("key")` or
/// `tr_count("key", ..)` call in game code, or a `{tr:key}` in the main
/// scene's HUD text, uses but a locale file in `assets/locales/` lacks.
/// `tr_count` needs `key.one` and `key.other`.
// ponytail: literal keys only; keys built at run time are not checked.
fn lint_missing_translations(
    root: &Path,
    document: &SceneDocument,
) -> Vec<Diagnostic> {
    let mut uses = Vec::new();
    for (call, forms) in [
        (".tr(\"", &[""][..]),
        ("tr_count(\"", &[".one", ".other"][..]),
    ] {
        for (file, line, key) in code_literals(root, &[call]) {
            let place = format!(
                "{}:{line}",
                file.strip_prefix(root).unwrap_or(&file).display()
            );
            for form in forms {
                uses.push((place.clone(), format!("{key}{form}")));
            }
        }
    }
    for entity in &document.entities {
        let Some(hud) = entity
            .components
            .get(crate::runtime::HUD_ELEMENT_COMPONENT)
            .and_then(|hud| {
                serde_json::from_str::<crate::runtime::HudElement>(hud).ok()
            })
        else {
            continue;
        };
        for piece in hud.text.split("{tr:").skip(1) {
            if let Some(key) = piece.split('}').next() {
                let name = entity.name.as_deref().unwrap_or("unnamed");
                uses.push((format!("HUD `{name}`"), key.to_owned()));
            }
        }
    }
    let Ok(entries) = std::fs::read_dir(root.join("assets/locales")) else {
        return Vec::new();
    };
    let mut files: Vec<_> = entries
        .filter_map(|entry| Some(entry.ok()?.path()))
        .filter(|path| path.extension().is_some_and(|e| e == "json"))
        .collect();
    files.sort();
    let mut diagnostics = Vec::new();
    for file in files {
        let strings: BTreeMap<String, String> = std::fs::read_to_string(&file)
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default();
        let shown = file
            .strip_prefix(root)
            .unwrap_or(&file)
            .display()
            .to_string();
        for (place, key) in &uses {
            if !strings.contains_key(key) {
                diagnostics.push(Diagnostic {
                    code: "LINT_MISSING_TRANSLATION",
                    severity: "warning",
                    message: format!("{place}: `{key}` has no text in {shown}"),
                    file: Some(file.clone()),
                    ..Diagnostic::default()
                });
            }
        }
    }
    diagnostics
}

/// `LINT_COLOR_ONLY_STATUS` on the first `set_hud("Name", ...)` call that
/// changes the element's `color` when no call ever changes its `text` and
/// its scene text has no `{counter}` readout: the state it shows is lost
/// on a colour-blind player.
// ponytail: a text search; a literal name only, and a `)` inside a string
// in the closure ends the call early.
fn lint_color_only_status(root: &Path) -> Vec<Diagnostic> {
    const CALL: &str = ".set_hud(\"";
    let mut scene_files = Vec::new();
    for folder in ["scenes", "assets"] {
        scene_files_in(&root.join(folder), &mut scene_files);
    }
    // Scene HUD colours, and names whose text changes by itself through a
    // counter readout.
    let mut readouts = BTreeSet::new();
    let mut scene_rgb = std::collections::BTreeMap::new();
    for entity in scene_files
        .iter()
        .filter_map(|path| read_scene(path).ok())
        .flat_map(|document| document.entities)
    {
        let (Some(name), Some(hud)) = (
            entity.name,
            entity
                .components
                .get(crate::runtime::HUD_ELEMENT_COMPONENT)
                .and_then(|hud| {
                    serde_json::from_str::<crate::runtime::HudElement>(hud).ok()
                }),
        ) else {
            continue;
        };
        if hud.text.contains('{') {
            readouts.insert(name.clone());
        }
        scene_rgb.insert(name, hud.color[..3].to_vec());
    }
    let mut files = Vec::new();
    rust_files(&root.join("src"), &mut files);
    files.sort();
    // Per name: whether any call sets the text, the first colour call, and
    // the RGB of each colour set (`None` when not a literal array).
    #[derive(Default)]
    struct Calls {
        text: bool,
        first: Option<String>,
        rgb: Vec<Option<Vec<f32>>>,
    }
    let mut calls = std::collections::BTreeMap::<String, Calls>::new();
    for file in files {
        let Ok(text) = std::fs::read_to_string(&file) else {
            continue;
        };
        for (start, _) in text.match_indices(CALL) {
            let rest = &text[start + CALL.len()..];
            let Some((name, _)) = rest.split_once('"') else {
                continue;
            };
            let mut depth = 1;
            let end = rest
                .char_indices()
                .find(|&(_, c)| {
                    depth += match c {
                        '(' => 1,
                        ')' => -1,
                        _ => 0,
                    };
                    depth == 0
                })
                .map_or(rest.len(), |(at, _)| at);
            let body: String =
                rest[..end].chars().filter(|c| !c.is_whitespace()).collect();
            let entry = calls.entry(name.to_owned()).or_default();
            entry.text |= body.contains(".text");
            for (at, _) in body.match_indices(".color=") {
                let value = &body[at + ".color=".len()..];
                entry.rgb.push(value.strip_prefix('[').and_then(|value| {
                    value
                        .split([',', ']'])
                        .take(3)
                        .map(|c| c.trim_end_matches("f32").parse().ok())
                        .collect()
                }));
                if entry.first.is_none() {
                    let line = text[..start].lines().count().max(1);
                    entry.first = Some(format!(
                        "{}:{line}",
                        file.strip_prefix(root).unwrap_or(&file).display()
                    ));
                }
            }
        }
    }
    let mut diagnostics = Vec::new();
    for (name, calls) in calls {
        let Some(at) = calls.first else {
            continue;
        };
        // Only brightness or alpha changes when every colour is a literal
        // with the scene's RGB: visible without telling hues apart.
        let mut rgb = calls
            .rgb
            .into_iter()
            .chain(scene_rgb.get(&name).cloned().map(Some));
        let first = rgb.next().flatten();
        let one_hue = first.is_some()
            && rgb.all(|other| other.is_some() && other == first);
        if calls.text || readouts.contains(&name) || one_hue {
            continue;
        }
        let file = root.join(at.split(':').next().unwrap_or_default());
        diagnostics.push(Diagnostic {
            code: "LINT_COLOR_ONLY_STATUS",
            severity: "warning",
            message: format!(
                "{at}: HUD `{name}` changes colour but never its text, so a colour-blind player misses what it shows; change the text too (a word or a symbol)"
            ),
            file: Some(file),
            ..Diagnostic::default()
        });
    }
    diagnostics
}

/// `LINT_NO_ENDING` on the first counter when a scene keeps counters but
/// nothing can end a round: no counter has a target (which `requires` HUD
/// screens and `counter_complete` read), and game code never calls
/// `counter_complete`, `load_scene` or `quit`. A scene with no counters is
/// a toy or a sandbox and is not judged.
// ponytail: a text search of the code, so a call in a comment counts.
fn lint_no_ending(document: &SceneDocument, code: &str) -> Option<Diagnostic> {
    let counters: Vec<(usize, crate::runtime::Counter)> = document
        .entities
        .iter()
        .enumerate()
        .filter_map(|(index, e)| {
            let text = e.components.get(crate::runtime::COUNTER_COMPONENT)?;
            Some((index, serde_json::from_str(text).ok()?))
        })
        .collect();
    let (index, _) = counters.first()?;
    if counters.iter().any(|(_, counter)| counter.target.is_some())
        || ["counter_complete(", "load_scene(", "quit("]
            .iter()
            .any(|call| code.contains(call))
    {
        return None;
    }
    let entity = &document.entities[*index];
    Some(Diagnostic {
        code: "LINT_NO_ENDING",
        severity: "warning",
        message: format!(
            "the scene keeps {} counter(s) but nothing ends a round: no counter has a target and game code never calls counter_complete, load_scene or quit",
            counters.len()
        ),
        scene_location: Some(format!("/entities/{index}")),
        entity: Some(EntityRef::of(entity)),
        ..Diagnostic::default()
    })
}

/// `LINT_SILENT_GOAL` on each counter with a target that nothing reacts
/// to: no HUD element `requires` it or reads it out as `{name}`, no
/// pickup `requires` it, and game code never names it in a string. The
/// player reaches the goal and nothing shows or sounds.
// ponytail: a text search of the code, so the name in a comment counts.
fn lint_silent_goal(document: &SceneDocument, code: &str) -> Vec<Diagnostic> {
    use crate::runtime::{
        Counter, HudElement, Pickup, COUNTER_COMPONENT, HUD_ELEMENT_COMPONENT,
        PICKUP_COMPONENT,
    };
    let parse = |e: &SceneEntity, key: &str| e.components.get(key).cloned();
    let mut reacting = BTreeSet::new();
    for entity in &document.entities {
        if let Some(hud) = parse(entity, HUD_ELEMENT_COMPONENT)
            .and_then(|text| serde_json::from_str::<HudElement>(&text).ok())
        {
            reacting.extend(hud.requires);
            let mut rest = hud.text.as_str();
            while let Some((_, after)) = rest.split_once('{') {
                let Some((name, after)) = after.split_once('}') else {
                    break;
                };
                reacting.insert(name.to_owned());
                rest = after;
            }
        }
        if let Some(pickup) = parse(entity, PICKUP_COMPONENT)
            .and_then(|text| serde_json::from_str::<Pickup>(&text).ok())
        {
            reacting.extend(pickup.requires);
        }
    }
    document
        .entities
        .iter()
        .enumerate()
        .filter_map(|(index, entity)| {
            let counter: Counter =
                serde_json::from_str(&parse(entity, COUNTER_COMPONENT)?).ok()?;
            let name = counter.name;
            if counter.target.is_none()
                || reacting.contains(&name)
                || code.contains(&format!("\"{name}\""))
            {
                return None;
            }
            Some(Diagnostic {
                code: "LINT_SILENT_GOAL",
                severity: "warning",
                message: format!(
                    "counter `{name}` has a target but nothing reacts when it is reached: no HUD element requires or shows it, no pickup requires it, and game code never names it"
                ),
                scene_location: Some(format!("/entities/{index}")),
                entity: Some(EntityRef::of(entity)),
                ..Diagnostic::default()
            })
        })
        .collect()
}

/// A `LINT_MISSING_OBJECT` per literal object name in game code, such as
/// `scene.object("Playr")`, that no entity in any scene or prefab under
/// scenes/ or assets/ has, and a `LINT_MISSING_ACTION` per literal action,
/// such as `scene.pressed("jmup")`, that no `rusting.input_action` there,
/// `rebind` call or built-in player action defines, a `LINT_MISSING_CLASS`
/// per literal class in `in_class`, such as `scene.in_class("enemys")`,
/// that no entity has and no other literal in game code names, a
/// `LINT_MISSING_EVENT` per literal `gpu_events` name that no other literal
/// in game code and no scene names, and a
/// `LINT_MISSING_FILE` per literal scene path or sound clip, such as
/// `scene.load_scene("scenes/levl_2.rscene")` or a `spawn_prefab` path,
/// with no file there (a
/// built-in `sfx:` clip needs none). Names built at run time
/// are not checked, and a literal on a line that spawns something may be
/// the spawned name.
fn lint_missing_names(root: &Path) -> Vec<Diagnostic> {
    const OBJECT_CALLS: &[&str] = &[
        ".object(\"",
        ".despawn(\"",
        ".set_visible(\"",
        ".set_color(\"",
        ".set_emissive(\"",
        ".flash(\"",
        ".squash(\"",
        ".add_trauma(\"",
        ".set_hud(\"",
        ".set_active_camera(\"",
        ".set_linear_velocity(\"",
        ".reset_body(\"",
        ".add_class(\"",
        ".play_animation(\"",
        ".crossfade(\"",
        ".trigger(\"",
        ".spawn_copy(\"",
        ".set_look(\"",
        ".angular_velocity(\"",
        ".basis(\"",
        ".camera_fov(\"",
        ".color(\"",
        ".dash(\"",
        ".initial(\"",
        ".is_limp(\"",
        ".is_playing(\"",
        ".linear_velocity(\"",
        ".play_sound_on(\"",
        ".reparent(\"",
        ".reset_ragdoll(\"",
        ".set_angular_velocity(\"",
        ".set_animation_speed(\"",
        ".set_body_kind(\"",
        ".set_camera(\"",
        ".set_camera_fov(\"",
        ".set_light(\"",
        ".set_material(\"",
        ".set_mouse_look(\"",
        ".set_ragdoll(\"",
        ".set_ragdoll_muscle(\"",
        ".set_text(\"",
        ".set_text_in_font(\"",
        ".set_tile(\"",
        ".spawn_copy_at_root(\"",
        ".stop_animation(\"",
        ".take_root_motion(\"",
        ".tile(\"",
        ".touching(\"",
        ".watch_gpu_object(\"",
    ];
    const CLASS_CALL: &str = ".in_class(\"";
    const EVENT_CALL: &str = ".gpu_events(\"";
    const ACTION_CALLS: [&str; 4] =
        [".pressed(\"", ".held(\"", ".press_tick(\"", ".binding(\""];
    // Scene paths are relative to the project, clips to `assets/`.
    const FILE_CALLS: [(&str, &str); 5] = [
        (".load_scene(\"", ""),
        (".spawn_prefab(\"", "assets"),
        (".play_sound(\"", "assets"),
        (".play_sound_looped(\"", "assets"),
        (".play_sound_with(\"", "assets"),
    ];
    let mut scene_files = Vec::new();
    for folder in ["scenes", "assets"] {
        scene_files_in(&root.join(folder), &mut scene_files);
    }
    let entities: Vec<SceneEntity> = scene_files
        .iter()
        .filter_map(|path| read_scene(path).ok())
        .flat_map(|document| document.entities)
        .collect();
    let mut names: BTreeSet<String> =
        entities.iter().filter_map(|e| e.name.clone()).collect();
    let mut actions: BTreeSet<String> = entities
        .iter()
        .filter_map(|e| {
            let text =
                e.components.get(crate::runtime::INPUT_ACTION_COMPONENT)?;
            serde_json::from_str::<crate::runtime::InputAction>(text).ok()
        })
        .map(|input| input.action)
        .chain(crate::runtime::PLAYER_ACTIONS.map(str::to_owned))
        .collect();
    let mut files = Vec::new();
    rust_files(&root.join("src"), &mut files);
    files.sort();
    let texts: Vec<(PathBuf, String)> = files
        .into_iter()
        .filter_map(|file| {
            Some((file.clone(), std::fs::read_to_string(&file).ok()?))
        })
        .collect();
    // `const CARD: &str = "Read Tag";` names whose constant a spawn uses.
    let consts: Vec<(&str, &str)> = texts
        .iter()
        .flat_map(|(_, text)| text.lines())
        .filter_map(|line| {
            let (ident, rest) = line
                .trim()
                .strip_prefix("const ")?
                .split_once(": &str = \"")?;
            Some((ident, rest.split('"').next()?))
        })
        .collect();
    let literals = |line: &str| -> Vec<String> {
        let mut found: Vec<String> = line
            .split('"')
            .skip(1)
            .step_by(2)
            .map(str::to_owned)
            .collect();
        found.extend(
            consts
                .iter()
                .filter(|(ident, _)| line.contains(ident))
                .map(|(_, name)| (*name).to_owned()),
        );
        found
    };
    // A class is known when a scene entity has it or a literal outside an
    // `in_class` call names it (an `add_class` argument, a helper's class
    // parameter), so only a name that appears in `in_class` alone warns.
    let mut classes: BTreeSet<String> =
        entities.iter().flat_map(|e| e.classes.clone()).collect();
    let mut uses = std::collections::BTreeMap::<&str, i32>::new();
    for line in texts.iter().flat_map(|(_, text)| text.lines()) {
        for literal in line.split('"').skip(1).step_by(2) {
            *uses.entry(literal).or_default() += 1;
        }
        for call in [CLASS_CALL, EVENT_CALL] {
            for (start, _) in line.match_indices(call) {
                let rest = &line[start + call.len()..];
                if let Some((name, _)) = rest.split_once('"') {
                    *uses.entry(name).or_default() -= 1;
                }
            }
        }
    }
    let named_elsewhere: BTreeSet<String> = uses
        .into_iter()
        .filter(|&(_, count)| count > 0)
        .map(|(name, _)| name.to_owned())
        .collect();
    classes.extend(named_elsewhere.iter().cloned());
    // A GPU event is named by a `GpuPhysicsRule` in code or a rule in a
    // scene file, so any other literal or any scene text naming it counts.
    let scene_text: String = scene_files
        .iter()
        .filter_map(|path| std::fs::read_to_string(path).ok())
        .collect();
    let mut events = named_elsewhere;
    for line in texts.iter().flat_map(|(_, text)| text.lines()) {
        for (start, _) in line.match_indices(EVENT_CALL) {
            let rest = &line[start + EVENT_CALL.len()..];
            if let Some((name, _)) = rest.split_once('"') {
                if scene_text.contains(&format!("\"{name}\""))
                    || scene_text.contains(&format!("\\\"{name}\\\""))
                {
                    events.insert(name.to_owned());
                }
            }
        }
    }
    for line in texts.iter().flat_map(|(_, text)| text.lines()) {
        if line.contains("spawn") {
            names.extend(literals(line));
        }
        if line.contains("rebind(") {
            actions.extend(literals(line));
        }
    }
    let mut diagnostics = Vec::new();
    for (file, text) in &texts {
        for (line, content) in text.lines().enumerate() {
            let checks = [
                (
                    OBJECT_CALLS,
                    &names,
                    "LINT_MISSING_OBJECT",
                    "no scene has an object named",
                ),
                (
                    &ACTION_CALLS[..],
                    &actions,
                    "LINT_MISSING_ACTION",
                    "no input action is named",
                ),
                (
                    &[CLASS_CALL][..],
                    &classes,
                    "LINT_MISSING_CLASS",
                    "no object or code puts anything in class",
                ),
                (
                    &[EVENT_CALL][..],
                    &events,
                    "LINT_MISSING_EVENT",
                    "no GPU physics rule raises an event named",
                ),
            ];
            for (calls, known, code, what) in checks {
                for call in calls {
                    for (start, _) in content.match_indices(call) {
                        let rest = &content[start + call.len()..];
                        let Some((name, _)) = rest.split_once('"') else {
                            continue;
                        };
                        if known.contains(name) {
                            continue;
                        }
                        let hint = crate::project_runner::nearest_hint(
                            known.iter().map(String::as_str),
                            name,
                        );
                        diagnostics.push(Diagnostic {
                            code,
                            severity: "warning",
                            message: format!(
                                "{}:{}: {what} `{name}`{hint}",
                                file.strip_prefix(root)
                                    .unwrap_or(file)
                                    .display(),
                                line + 1,
                            ),
                            file: Some(file.clone()),
                            ..Diagnostic::default()
                        });
                    }
                }
            }
            for (call, folder) in FILE_CALLS {
                for (start, _) in content.match_indices(call) {
                    let rest = &content[start + call.len()..];
                    let Some((name, _)) = rest.split_once('"') else {
                        continue;
                    };
                    let path = root.join(folder).join(name);
                    if name.starts_with(crate::sfx::CLIP_PREFIX)
                        || path.exists()
                    {
                        continue;
                    }
                    // Close names in the same folder, written the same way.
                    let prefix =
                        name.rsplit_once('/').map_or("", |(dir, _)| dir);
                    let siblings: Vec<String> = path
                        .parent()
                        .and_then(|dir| std::fs::read_dir(dir).ok())
                        .into_iter()
                        .flatten()
                        .flatten()
                        .map(|entry| {
                            let file = entry.file_name();
                            let file = file.to_string_lossy();
                            if prefix.is_empty() {
                                file.into_owned()
                            } else {
                                format!("{prefix}/{file}")
                            }
                        })
                        .collect();
                    let hint = crate::project_runner::nearest_hint(
                        siblings.iter().map(String::as_str),
                        name,
                    );
                    let under = if folder.is_empty() {
                        "the project"
                    } else {
                        "assets/"
                    };
                    diagnostics.push(Diagnostic {
                        code: "LINT_MISSING_FILE",
                        severity: "warning",
                        message: format!(
                            "{}:{}: no file `{name}` under {under}{hint}",
                            file.strip_prefix(root).unwrap_or(file).display(),
                            line + 1,
                        ),
                        file: Some(file.clone()),
                        ..Diagnostic::default()
                    });
                }
            }
        }
    }
    diagnostics
}

/// Local half sizes of a solid collider and its entity's built-in mesh when
/// any axis differs by more than a factor of 2. Sensors are meant to be
/// bigger or smaller than what is drawn, and a player's capsule has nothing
/// to do with its mesh, so both are skipped.
fn collider_mismatch(entity: &SceneEntity) -> Option<([f32; 3], [f32; 3])> {
    use crate::assets::PrimitiveShape as P;
    use crate::runtime::{
        ColliderShape, SceneMesh, PLAYER_CONTROLLER_COMPONENT,
    };
    let collider = entity.collider.as_ref().filter(|c| !c.sensor)?;
    if entity.components.contains_key(PLAYER_CONTROLLER_COMPONENT) {
        return None;
    }
    let mesh = match entity.mesh_renderer.as_ref()?.mesh {
        SceneMesh::BuiltinCube
        | SceneMesh::BuiltinSphere
        | SceneMesh::BuiltinPrimitive(
            P::Cube | P::RoundedCube | P::Sphere | P::Cylinder,
        ) => [0.5; 3],
        SceneMesh::BuiltinPrimitive(P::Capsule) => [0.5, 1.0, 0.5],
        _ => return None,
    };
    let body = match collider.shape {
        ColliderShape::Box { half_extents } => half_extents,
        ColliderShape::Sphere { radius } => [radius; 3],
        ColliderShape::Capsule {
            half_height,
            radius,
        } => [radius, half_height + radius, radius],
        _ => return None,
    };
    let off = mesh
        .iter()
        .zip(body)
        .any(|(mesh, body)| !(0.5..=2.0).contains(&(body / mesh)));
    off.then_some((mesh, body))
}

/// HUD text smaller than this, in logical pixels, warns `LINT_TEXT_SMALL`.
const MIN_HUD_FONT_SIZE: f32 = 14.0;
/// The smallest view, in logical pixels, that HUD text must fit.
const LINT_VIEW: [f32; 2] = [1280.0, 720.0];

/// Every string literal in the game code under `root/src`: the object
/// names game code may move, hide or delete.
fn code_strings(root: &Path) -> BTreeSet<String> {
    let mut files = Vec::new();
    rust_files(&root.join("src"), &mut files);
    files
        .iter()
        .filter_map(|file| std::fs::read_to_string(file).ok())
        .flat_map(|text| {
            text.split('"')
                .skip(1)
                .step_by(2)
                .map(str::to_owned)
                .collect::<Vec<_>>()
        })
        .collect()
}

/// Lays out HUD text the way the runtime HUD draws it (egui's proportional
/// font, no wrapping) and returns its size in logical pixels.
#[cfg(feature = "ui")]
fn hud_text_size() -> impl FnMut(&str, f32) -> Option<[f32; 2]> {
    let context = egui::Context::default();
    // Fonts exist only after the first pass.
    let _ = context.run(egui::RawInput::default(), |_| {});
    move |text, size| {
        let galley = context.fonts(|fonts| {
            fonts.layout_no_wrap(
                text.to_owned(),
                egui::FontId::proportional(size),
                egui::Color32::WHITE,
            )
        });
        Some([galley.size().x, galley.size().y])
    }
}

/// The runtime's button fill: egui's dark one, restyled by the project's
/// `assets/ui/theme.json` when it has a readable one.
#[cfg(feature = "ui")]
fn button_fill(root: &Path) -> [f32; 3] {
    let mut style = egui::Style {
        visuals: egui::Visuals::dark(),
        ..egui::Style::default()
    };
    if let Some(theme) = std::fs::read(root.join("assets/ui/theme.json"))
        .ok()
        .and_then(|bytes| {
            serde_json::from_slice::<crate::runtime::UiTheme>(&bytes).ok()
        })
    {
        theme.apply(&mut style);
    }
    let [r, g, b, _] = style
        .visuals
        .widgets
        .inactive
        .weak_bg_fill
        .to_array()
        .map(|c| f32::from(c) / 255.0);
    [r, g, b]
}

#[cfg(not(feature = "ui"))]
fn button_fill(_: &Path) -> [f32; 3] {
    [0.0; 3]
}

/// The WCAG 2 contrast ratio of HUD button text against the button `fill`,
/// with the minimum for its size (4.5, or 3 from 24 px), when it falls
/// short. Text with no button has the scene behind it, which only a
/// rendered frame shows, so it is not judged.
#[cfg(feature = "ui")]
fn button_contrast(
    button: bool,
    color: [f32; 4],
    font_size: f32,
    fill: [f32; 3],
) -> Option<(f32, f32)> {
    let ratio = contrast(color, fill);
    let minimum = minimum_contrast(font_size);
    (button && ratio < minimum).then_some((ratio, minimum))
}

/// The WCAG 2 minimum contrast for text of `font_size` px: 4.5, or 3 from
/// 24 px.
#[cfg(feature = "ui")]
fn minimum_contrast(font_size: f32) -> f32 {
    if font_size >= 24.0 {
        3.0
    } else {
        4.5
    }
}

/// The WCAG 2 contrast ratio of sRGB `color`, blended by its alpha, over
/// the sRGB `back`.
#[cfg(feature = "ui")]
fn contrast(color: [f32; 4], back: [f32; 3]) -> f32 {
    let luminance = |c: [f32; 3]| {
        let linear = |v: f32| {
            if v <= 0.040_45 {
                v / 12.92
            } else {
                ((v + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * linear(c[0]) + 0.7152 * linear(c[1]) + 0.0722 * linear(c[2])
    };
    let alpha = color[3].clamp(0.0, 1.0);
    let text: [f32; 3] = std::array::from_fn(|i| {
        color[i].clamp(0.0, 1.0) * alpha + back[i] * (1.0 - alpha)
    });
    let (a, b) = (luminance(text), luminance(back));
    (a.max(b) + 0.05) / (a.min(b) + 0.05)
}

/// A `CAPTURE_TEXT_CONTRAST` warning per run of UI text with nothing
/// painted under it whose median contrast against the scene pixels behind
/// its box, in `background` (the frame without the UI), is below the WCAG
/// minimum for its size. Text on a button or caption box is left to
/// `LINT_TEXT_CONTRAST`.
#[cfg(feature = "ui")]
fn text_contrast_warnings(
    texts: &[crate::rendering::capture::TextSpot],
    background: &[u8],
    [width, height]: [u32; 2],
) -> Vec<Diagnostic> {
    let mut warnings = Vec::new();
    for spot in texts.iter().filter(|spot| !spot.backed) {
        let clamp = |v: f32, max: u32| (v.max(0.0) as u32).min(max);
        let (x0, x1) = (clamp(spot.rect[0], width), clamp(spot.rect[2], width));
        let (y0, y1) =
            (clamp(spot.rect[1], height), clamp(spot.rect[3], height));
        let mut ratios: Vec<f32> = (y0..y1)
            .flat_map(|y| (x0..x1).map(move |x| ((y * width + x) * 4) as usize))
            .filter_map(|at| background.get(at..at + 3))
            .map(|rgb| {
                contrast(
                    spot.color,
                    std::array::from_fn(|i| f32::from(rgb[i]) / 255.0),
                )
            })
            .collect();
        if ratios.is_empty() {
            continue;
        }
        ratios.sort_by(f32::total_cmp);
        let median = ratios[ratios.len() / 2];
        let minimum = minimum_contrast(spot.size);
        if median < minimum {
            warnings.push(Diagnostic {
                code: "CAPTURE_TEXT_CONTRAST",
                severity: "warning",
                message: format!(
                    "HUD text `{}` at {:.0},{:.0} has a median contrast of {median:.1}:1 against the scene behind it, below {minimum}:1 for {:.0} px text",
                    spot.text, spot.rect[0], spot.rect[1], spot.size
                ),
                ..Diagnostic::default()
            });
        }
    }
    warnings
}

#[cfg(not(feature = "ui"))]
fn button_contrast(
    _: bool,
    _: [f32; 4],
    _: f32,
    _: [f32; 3],
) -> Option<(f32, f32)> {
    None
}

/// Without egui there are no fonts to measure with.
#[cfg(not(feature = "ui"))]
fn hud_text_size() -> impl FnMut(&str, f32) -> Option<[f32; 2]> {
    |_, _| None
}

/// The `LINT_*` warnings of one scene document.
/// `named_in_code` are the names game code mentions; walls with those names
/// may be doors, so the reachability check walks through them.
#[cfg(test)]
fn lint_scene(
    document: &SceneDocument,
    named_in_code: &BTreeSet<String>,
) -> Vec<Diagnostic> {
    let unthemed = button_fill(Path::new("no-project"));
    lint_scene_with_fill(document, named_in_code, unthemed)
}

/// The `LINT_*` warnings of one scene document, judging button text
/// against `button_fill`, the fill the project's UI theme gives buttons.
/// Chunks the `rusting.terrain` on `entity` draws over its
/// `rusting.heightfield`, when it has both.
fn terrain_chunks(entity: &SceneEntity) -> Option<usize> {
    use crate::runtime::{HEIGHTFIELD_COMPONENT, TERRAIN_COMPONENT};
    let component = |key| entity.components.get(key).map(String::as_str);
    let terrain: crate::runtime::Terrain =
        serde_json::from_str(component(TERRAIN_COMPONENT)?).ok()?;
    let field: crate::runtime::Heightfield =
        serde_json::from_str(component(HEIGHTFIELD_COMPONENT)?).ok()?;
    Some(terrain.chunk_count(&field))
}

fn lint_scene_with_fill(
    document: &SceneDocument,
    named_in_code: &BTreeSet<String>,
    button_fill: [f32; 3],
) -> Vec<Diagnostic> {
    use crate::runtime::{
        ColliderShape, HudAnchor, HudElement, DEFAULT_PLAYER_SHAPE,
        HUD_ELEMENT_COMPONENT, PLATFORMER_CONTROLLER_COMPONENT,
        PLAYER_CONTROLLER_COMPONENT,
    };
    let by_id: std::collections::HashMap<_, _> =
        document.entities.iter().map(|e| (e.id, e)).collect();
    // HUD `{counter}` placeholders show each counter's starting value.
    let counters: Vec<crate::runtime::Counter> = document
        .entities
        .iter()
        .filter_map(|e| e.components.get(crate::runtime::COUNTER_COMPONENT))
        .filter_map(|text| serde_json::from_str(text).ok())
        .collect();
    let mut measure = hud_text_size();
    // ponytail: scales multiply per axis and ignore rotation; enough for
    // sanity ranges, not for exact sizes under rotated parents.
    let world_scale = |entity: &SceneEntity| {
        let mut scale = [1.0f32; 3];
        let mut next = Some(entity);
        for _ in 0..64 {
            let Some(current) = next else { break };
            if let Some(transform) = &current.transform {
                for (total, own) in scale.iter_mut().zip(transform.scale) {
                    *total *= own;
                }
            }
            next = current.parent.and_then(|id| by_id.get(&id).copied());
        }
        scale
    };
    let mut diagnostics = Vec::new();
    let mut warn = |code, index: usize, location: &str, message: String| {
        let entity = &document.entities[index];
        diagnostics.push(Diagnostic {
            code,
            severity: "warning",
            message: format!(
                "`{}` {message}",
                entity.name.as_deref().unwrap_or("unnamed entity")
            ),
            scene_location: Some(format!("/entities/{index}{location}")),
            entity: Some(EntityRef::of(entity)),
            ..Diagnostic::default()
        });
    };
    // World matrices from the parent chain, as the runtime composes them.
    let world_matrix = |entity: &SceneEntity| {
        let mut matrix = nalgebra::Matrix4::<f32>::identity();
        let mut next = Some(entity);
        for _ in 0..64 {
            let Some(current) = next else { break };
            if let Some(transform) = &current.transform {
                let local = crate::runtime::sim_math::transform_matrix(
                    &crate::Transform::from(*transform),
                );
                matrix = nalgebra::Matrix4::from(local) * matrix;
            }
            next = current.parent.and_then(|id| by_id.get(&id).copied());
        }
        matrix
    };
    let is_ancestor = |ancestor: uuid::Uuid, entity: &SceneEntity| {
        let mut next = entity.parent;
        for _ in 0..64 {
            match next {
                Some(id) if id == ancestor => return true,
                Some(id) => next = by_id.get(&id).and_then(|e| e.parent),
                None => break,
            }
        }
        false
    };
    // Player bodies are left out: an eye camera sits inside one, parented
    // or not, and the body is not drawn from there.
    let solids: Vec<_> = document
        .entities
        .iter()
        .filter(|entity| {
            !entity.components.contains_key(PLAYER_CONTROLLER_COMPONENT)
                && !entity
                    .components
                    .contains_key(PLATFORMER_CONTROLLER_COMPONENT)
        })
        .filter_map(|entity| {
            let collider = entity.collider.as_ref()?;
            let inverse = world_matrix(entity).try_inverse()?;
            (!collider.sensor).then_some((entity, collider.shape, inverse))
        })
        .collect();
    // The renderer uploads visible directional, then point, then spot
    // lights, each in spawn order, and drops the rest past the budget.
    let shown = |entity: &SceneEntity| {
        let mut next = Some(entity);
        for _ in 0..64 {
            let Some(current) = next else { break };
            if current.visible == Some(false) {
                return false;
            }
            next = current.parent.and_then(|id| by_id.get(&id).copied());
        }
        true
    };
    let quality = document.render.quality;
    let budget = crate::rendering::scene_renderer::light_budget(quality);
    let has = |entity: &SceneEntity, key| match key {
        "directional_light" => entity.directional_light.is_some(),
        "point_light" => entity.point_light.is_some(),
        _ => entity.spot_light.is_some(),
    };
    let over_budget: Vec<_> =
        ["directional_light", "point_light", "spot_light"]
            .into_iter()
            .flat_map(|key| {
                document
                    .entities
                    .iter()
                    .enumerate()
                    .filter(move |(_, entity)| {
                        has(entity, key) && shown(entity)
                    })
                    .map(move |(index, _)| (index, key))
            })
            .skip(budget)
            .collect();
    // Whether the world point `at` is inside a collider with this inverse
    // world matrix.
    // ponytail: mesh colliders are skipped; the scene file has no mesh
    // bounds.
    let inside = |shape: &ColliderShape,
                  inverse: &nalgebra::Matrix4<f32>,
                  at: nalgebra::Vector4<f32>| {
        let local = (inverse * at).xyz();
        match shape {
            ColliderShape::Box { half_extents } => {
                (0..3).all(|axis| local[axis].abs() < half_extents[axis])
            }
            ColliderShape::Sphere { radius } => local.norm() < *radius,
            ColliderShape::Capsule {
                half_height,
                radius,
            } => {
                let core = local[1].clamp(-half_height, *half_height);
                (local - nalgebra::Vector3::new(0.0, core, 0.0)).norm()
                    < *radius
            }
            _ => false,
        }
    };
    // Cameras and sensors (pickups, goals, triggers) whose centre starts
    // inside a solid, with that solid's name.
    let mut inside_solid = Vec::new();
    for (index, entity) in document.entities.iter().enumerate() {
        let sensor = entity.collider.as_ref().is_some_and(|c| c.sensor);
        if entity.camera.is_none() && !sensor {
            continue;
        }
        let at = world_matrix(entity).column(3).xyz().push(1.0);
        for (solid, shape, inverse) in &solids {
            if solid.id == entity.id || is_ancestor(solid.id, entity) {
                continue;
            }
            if inside(shape, inverse, at) {
                inside_solid.push((index, solid.name.clone()));
                break;
            }
        }
    }
    let unreachable = unreachable_goals(
        document,
        &solids,
        named_in_code,
        &world_matrix,
        &is_ancestor,
        &inside,
    );
    for (index, entity) in document.entities.iter().enumerate() {
        if let Some(chunks) = terrain_chunks(entity).filter(|&n| n > 300) {
            warn(
                "LINT_TERRAIN_CHUNKS",
                index,
                "/components/rusting.terrain",
                format!(
                    "draws its terrain as {chunks} chunks, one draw each per view and shadow cascade, which makes the frame CPU-bound; raise chunk_cells or set it to 0 to pick the size from the heightfield"
                ),
            );
        }
        if let Some(player) = unreachable
            .iter()
            .find(|(goal, _)| *goal == index)
            .map(|(_, player)| player)
        {
            warn(
                "LINT_GOAL_UNREACHABLE",
                index,
                "/transform/position",
                format!(
                    "is a sensor at the height of player `{player}` that no walk or jump from the player's start reaches; walls or ledges too high to jump close off every route"
                ),
            );
        }
        if let Some((_, solid)) =
            inside_solid.iter().find(|(inside, _)| *inside == index)
        {
            let solid = solid.as_deref().unwrap_or("unnamed entity");
            if entity.camera.is_some() {
                warn(
                    "LINT_CAMERA_INSIDE",
                    index,
                    "/transform/position",
                    format!(
                        "is a camera that starts inside the collider of `{solid}`, so the first frame shows its inside"
                    ),
                );
            } else {
                warn(
                    "LINT_GOAL_INSIDE",
                    index,
                    "/transform/position",
                    format!(
                        "is a sensor whose centre is inside the solid collider of `{solid}`, so a player is stopped before reaching it"
                    ),
                );
            }
        }
        for (_, key) in over_budget.iter().filter(|(at, _)| *at == index) {
            warn(
                "LINT_LIGHT_BUDGET",
                index,
                &format!("/{key}"),
                format!(
                    "has a {key} past the {budget}-light budget of quality {quality:?}, so the renderer drops it"
                ),
            );
        }
        if let Some(transform) = &entity.transform {
            if transform.scale.contains(&0.0) {
                warn(
                    "LINT_ZERO_SCALE",
                    index,
                    "/transform/scale",
                    format!("has scale {:?}, so it vanishes", transform.scale),
                );
            }
        }
        let hud = entity
            .components
            .get(HUD_ELEMENT_COMPONENT)
            .and_then(|text| serde_json::from_str::<HudElement>(text).ok());
        if let Some(hud) = hud {
            if hud.font_size < MIN_HUD_FONT_SIZE {
                warn(
                    "LINT_TEXT_SMALL",
                    index,
                    &format!("/components/{HUD_ELEMENT_COMPONENT}"),
                    format!(
                        "has HUD text at font_size {}, below the {MIN_HUD_FONT_SIZE} px that stays readable",
                        hud.font_size
                    ),
                );
            }
            if let Some((ratio, minimum)) = button_contrast(
                hud.button,
                hud.color,
                hud.font_size,
                button_fill,
            ) {
                warn(
                    "LINT_TEXT_CONTRAST",
                    index,
                    &format!("/components/{HUD_ELEMENT_COMPONENT}/color"),
                    format!(
                        "has HUD button text at contrast {ratio:.1}:1 against the button fill, below the {minimum}:1 that stays readable"
                    ),
                );
            }
            // Where the text's anchored corner lands in the smallest
            // supported view; a camera's viewport is left out.
            let [w, h] = LINT_VIEW;
            let (x, y, inward) = match hud.anchor {
                HudAnchor::TopLeft => (0.0, 0.0, [1.0, 1.0]),
                HudAnchor::Top => (w / 2.0, 0.0, [1.0, 1.0]),
                HudAnchor::TopRight => (w, 0.0, [-1.0, 1.0]),
                HudAnchor::Center => (w / 2.0, h / 2.0, [1.0, 1.0]),
                HudAnchor::BottomLeft => (0.0, h, [1.0, -1.0]),
                HudAnchor::Bottom => (w / 2.0, h, [1.0, -1.0]),
                HudAnchor::BottomRight => (w, h, [-1.0, -1.0]),
            };
            let at =
                [x + hud.offset[0] * inward[0], y + hud.offset[1] * inward[1]];
            let outside =
                !(0.0..=w).contains(&at[0]) || !(0.0..=h).contains(&at[1]);
            // The text's box, placed as the HUD places it: the anchored
            // corner (or centre) at `at`.
            let text = crate::runtime::hud_text(
                &hud.text,
                counters.iter().map(|counter| (counter, None)),
                // ponytail: measures `{tr:key}` as the key; measure each
                // locale when games ship translations.
                None,
            );
            let overflow = measure(&text, hud.font_size).and_then(|size| {
                let fraction = |anchor: f32, view: f32| anchor / view;
                let min = [
                    at[0] - size[0] * fraction(x, w),
                    at[1] - size[1] * fraction(y, h),
                ];
                let past = [
                    (-min[0]).max(min[0] + size[0] - w),
                    (-min[1]).max(min[1] + size[1] - h),
                ];
                (past[0] > 0.5 || past[1] > 0.5).then_some((size, past))
            });
            if let (None, false, Some((size, past))) =
                (&hud.camera, outside, overflow)
            {
                warn(
                    "LINT_TEXT_OVERFLOW",
                    index,
                    &format!("/components/{HUD_ELEMENT_COMPONENT}/text"),
                    format!(
                        "has HUD text {:.0} x {:.0} px that runs {:.0} px past the edge of a {w} x {h} view",
                        size[0], size[1], past[0].max(past[1])
                    ),
                );
            }
            if hud.camera.is_none() && outside {
                warn(
                    "LINT_TEXT_OFFSCREEN",
                    index,
                    &format!("/components/{HUD_ELEMENT_COMPONENT}/offset"),
                    format!(
                        "has HUD text anchored {:?} with offset {:?}, which places it at {at:?}, outside a {w} x {h} view",
                        hud.anchor, hud.offset
                    ),
                );
            }
        }
        if let Some((mesh, body)) = collider_mismatch(entity) {
            warn(
                "LINT_COLLIDER_MISMATCH",
                index,
                "/collider/shape",
                format!(
                    "has a collider of half size {body:?} on a mesh of half size {mesh:?}, so it is hit where it is not drawn or not hit where it is"
                ),
            );
        }
        if entity.components.contains_key(PLAYER_CONTROLLER_COMPONENT) {
            let shape = entity
                .collider
                .as_ref()
                .map_or(DEFAULT_PLAYER_SHAPE, |collider| collider.shape);
            let local = match shape {
                ColliderShape::Capsule {
                    half_height,
                    radius,
                } => 2.0 * (half_height + radius),
                ColliderShape::Box { half_extents } => 2.0 * half_extents[1],
                ColliderShape::Sphere { radius } => 2.0 * radius,
                _ => 1.8,
            };
            let height = local * world_scale(entity)[1].abs();
            if !(0.5..=3.0).contains(&height) {
                warn(
                    "LINT_PLAYER_SCALE",
                    index,
                    "/transform/scale",
                    format!(
                        "is a player {height:.2} m tall; expected 0.5 to 3 m"
                    ),
                );
            }
        }
        let lights = [
            entity.directional_light.as_ref().map(|light| {
                ("directional_light", light.illuminance, 1.0, light.color)
            }),
            entity.point_light.as_ref().map(|light| {
                ("point_light", light.intensity, light.range, light.color)
            }),
            entity.spot_light.as_ref().map(|light| {
                ("spot_light", light.intensity, light.range, light.color)
            }),
        ];
        for (key, intensity, range, color) in lights.into_iter().flatten() {
            // Intensity 0 is how games start a light they switch on in
            // code (a flashlight, a scare flash), so only below 0 counts.
            let problem = if intensity < 0.0 {
                format!("intensity {intensity}")
            } else if range <= 0.0 {
                format!("range {range}")
            } else if color == [0.0; 3] {
                "a black color".to_owned()
            } else {
                continue;
            };
            warn(
                "LINT_LIGHT_OFF",
                index,
                &format!("/{key}"),
                format!("has a {key} with {problem}, so it gives no light"),
            );
        }
    }
    diagnostics
}

/// Sensors (pickups, goals, triggers) at the first player's height that no
/// walk from the player's start reaches, as (entity index, player name).
/// Fixed solids (no body, or a `Fixed` one) are rasterized into an XZ grid
/// wherever they cover the part of the player's body above what it can step
/// or jump over; the grid is flood-filled from the player. A sensor in a
/// blocked cell sits on or in something and is not judged, nor is one above
/// or below the player's floor. Walls named in game code may be doors and
/// do not block.
// ponytail: one floor, a point-sized body and cell-centre samples, so thin
// gaps and walls under a cell are missed (a miss, never a false warning
// from them); a door found only through a parent's name or a computed name
// is a wall here.
/// Whether a world point lies inside a collider shape placed by a world matrix.
type InsideTest = dyn Fn(
    &crate::runtime::ColliderShape,
    &nalgebra::Matrix4<f32>,
    nalgebra::Vector4<f32>,
) -> bool;

fn unreachable_goals(
    document: &SceneDocument,
    solids: &[(
        &SceneEntity,
        crate::runtime::ColliderShape,
        nalgebra::Matrix4<f32>,
    )],
    named_in_code: &BTreeSet<String>,
    world_matrix: &dyn Fn(&SceneEntity) -> nalgebra::Matrix4<f32>,
    is_ancestor: &dyn Fn(uuid::Uuid, &SceneEntity) -> bool,
    inside: &InsideTest,
) -> Vec<(usize, String)> {
    use crate::runtime::{
        ColliderShape, PlayerController, RigidBodyKind, DEFAULT_PLAYER_SHAPE,
        PLAYER_CONTROLLER_COMPONENT,
    };
    let Some(player) = document
        .entities
        .iter()
        .find(|e| e.components.contains_key(PLAYER_CONTROLLER_COMPONENT))
    else {
        return Vec::new();
    };
    let controller: PlayerController =
        serde_json::from_str(&player.components[PLAYER_CONTROLLER_COMPONENT])
            .unwrap_or_default();
    let matrix = world_matrix(player);
    let half = match player
        .collider
        .as_ref()
        .map_or(DEFAULT_PLAYER_SHAPE, |c| c.shape)
    {
        ColliderShape::Capsule {
            half_height,
            radius,
        } => half_height + radius,
        ColliderShape::Box { half_extents } => half_extents[1],
        ColliderShape::Sphere { radius } => radius,
        _ => 0.9,
    } * matrix.fixed_view::<3, 1>(0, 1).norm();
    let centre = matrix.column(3).xyz();
    let feet = centre.y - half;
    let apex = if controller.gravity > 0.0 {
        controller.jump_speed.powi(2) / (2.0 * controller.gravity)
    } else {
        0.0
    };
    // The part of the body a wall must cover to stop the player.
    let band = [
        feet + controller.max_step_height.max(apex) + 0.01,
        feet + 2.0 * half,
    ];
    if band[0] >= band[1] {
        return Vec::new();
    }
    // World bounds of every fixed solid that reaches into the band.
    let walls: Vec<_> = solids
        .iter()
        .filter(|(entity, ..)| {
            entity.id != player.id
                && !is_ancestor(entity.id, player)
                && entity
                    .name
                    .as_ref()
                    .is_none_or(|name| !named_in_code.contains(name))
                && entity
                    .rigid_body
                    .as_ref()
                    .is_none_or(|body| body.kind == RigidBodyKind::Fixed)
        })
        .filter_map(|(entity, shape, inverse)| {
            let extent = match shape {
                ColliderShape::Box { half_extents } => *half_extents,
                ColliderShape::Sphere { radius } => [*radius; 3],
                ColliderShape::Capsule {
                    half_height,
                    radius,
                } => [*radius, half_height + radius, *radius],
                _ => return None,
            };
            let to_world = world_matrix(entity);
            let mut low = [f32::INFINITY; 3];
            let mut high = [f32::NEG_INFINITY; 3];
            for corner in 0..8 {
                let local = nalgebra::Vector4::new(
                    if corner & 1 == 0 {
                        -extent[0]
                    } else {
                        extent[0]
                    },
                    if corner & 2 == 0 {
                        -extent[1]
                    } else {
                        extent[1]
                    },
                    if corner & 4 == 0 {
                        -extent[2]
                    } else {
                        extent[2]
                    },
                    1.0,
                );
                let at = to_world * local;
                for axis in 0..3 {
                    low[axis] = low[axis].min(at[axis]);
                    high[axis] = high[axis].max(at[axis]);
                }
            }
            (low[1] < band[1] && high[1] > band[0])
                .then_some((shape, inverse, low, high))
        })
        .collect();
    let goals: Vec<_> = document
        .entities
        .iter()
        .enumerate()
        .filter(|(_, e)| {
            e.collider.as_ref().is_some_and(|c| c.sensor)
                && e.id != player.id
                && !is_ancestor(player.id, e)
                && !is_ancestor(e.id, player)
        })
        .map(|(index, e)| (index, world_matrix(e).column(3).xyz()))
        .filter(|(_, at)| (feet - 0.5..=band[1] + 0.5).contains(&at.y))
        .collect();
    if walls.is_empty() || goals.is_empty() {
        return Vec::new();
    }
    let mut low = [centre.x, centre.z];
    let mut high = low;
    for (.., wall_low, wall_high) in &walls {
        for (axis, world) in [(0, 0), (1, 2)] {
            low[axis] = low[axis].min(wall_low[world]);
            high[axis] = high[axis].max(wall_high[world]);
        }
    }
    // One open cell of margin all round, so the outside connects.
    let cell = ((high[0] - low[0]).max(high[1] - low[1]) / 1024.0).max(0.25);
    let low = [low[0] - cell, low[1] - cell];
    let size = [
        ((high[0] - low[0]) / cell) as usize + 2,
        ((high[1] - low[1]) / cell) as usize + 2,
    ];
    let index_of = |x: f32, z: f32| {
        let column = ((x - low[0]) / cell) as usize;
        let row = ((z - low[1]) / cell) as usize;
        (column < size[0] && row < size[1]).then_some(row * size[0] + column)
    };
    let heights: Vec<f32> = {
        let steps = ((band[1] - band[0]) / 0.25).ceil().max(1.0) as usize;
        (0..=steps)
            .map(|step| {
                band[0] + (band[1] - band[0]) * step as f32 / steps as f32
            })
            .collect()
    };
    let mut blocked = vec![false; size[0] * size[1]];
    for (shape, inverse, wall_low, wall_high) in &walls {
        let first = [
            ((wall_low[0] - low[0]) / cell).floor().max(0.0) as usize,
            ((wall_low[2] - low[1]) / cell).floor().max(0.0) as usize,
        ];
        let last = [
            (((wall_high[0] - low[0]) / cell) as usize).min(size[0] - 1),
            (((wall_high[2] - low[1]) / cell) as usize).min(size[1] - 1),
        ];
        for row in first[1]..=last[1] {
            for column in first[0]..=last[0] {
                let x = low[0] + (column as f32 + 0.5) * cell;
                let z = low[1] + (row as f32 + 0.5) * cell;
                let cell_blocked = &mut blocked[row * size[0] + column];
                if !*cell_blocked {
                    *cell_blocked = heights.iter().any(|y| {
                        inside(
                            shape,
                            inverse,
                            nalgebra::Vector4::new(x, *y, z, 1.0),
                        )
                    });
                }
            }
        }
    }
    let Some(start) = index_of(centre.x, centre.z).filter(|at| !blocked[*at])
    else {
        return Vec::new();
    };
    let mut reached = vec![false; blocked.len()];
    reached[start] = true;
    let mut queue = std::collections::VecDeque::from([start]);
    while let Some(at) = queue.pop_front() {
        let (column, row) = (at % size[0], at / size[0]);
        let neighbours = [
            (column > 0).then(|| at - 1),
            (column + 1 < size[0]).then(|| at + 1),
            (row > 0).then(|| at - size[0]),
            (row + 1 < size[1]).then(|| at + size[0]),
        ];
        for next in neighbours.into_iter().flatten() {
            if !blocked[next] && !reached[next] {
                reached[next] = true;
                queue.push_back(next);
            }
        }
    }
    let name = player
        .name
        .clone()
        .unwrap_or_else(|| "unnamed entity".into());
    goals
        .into_iter()
        .filter(|(_, at)| {
            index_of(at.x, at.z)
                .is_some_and(|cell| !blocked[cell] && !reached[cell])
        })
        .map(|(index, _)| (index, name.clone()))
        .collect()
}

/// Applies every certain fix `validate` finds to the main scene. Each fix
/// renames one misspelled key in place in the file text, so nothing else in
/// the file changes; validation then runs again, because a file that did not
/// load can show its next problem only once the first is fixed. With
/// `dry_run`, only the first round is reported and nothing is written.
/// Problems without a certain fix stay in the diagnostics.
pub fn fix_project(root: &Path, dry_run: bool) -> CliResult {
    let mut applied = Vec::new();
    if let Some(outdated) = outdated_agents(root) {
        let path = root.join("AGENTS.md");
        if !dry_run {
            let refreshed = std::fs::copy(&path, root.join("AGENTS.md.old"))
                .and_then(|_| {
                    crate::runtime::write_atomic(
                        &path,
                        PROJECT_AGENTS.as_bytes(),
                    )
                });
            if let Err(error) = refreshed {
                return CliResult::failure(
                    "IO_ERROR",
                    error.to_string(),
                    Some(path),
                );
            }
        }
        applied.push(json!({
            "message": "replaced AGENTS.md with this engine's copy; the old one is AGENTS.md.old",
            "file": outdated.file,
            "operation": "refresh_agents",
        }));
    }
    // Each round renames at least one key, so this bounds a file with many.
    for _ in 0..64 {
        let mut validation = validate_project(root);
        let (fixes, remaining): (Vec<_>, Vec<_>) = validation
            .diagnostics
            .drain(..)
            .partition(|diagnostic| diagnostic.fix.is_some());
        if fixes.is_empty() || dry_run {
            applied.extend(fixes.iter().map(fix_summary));
            return fix_result(validation, remaining, applied, dry_run);
        }
        for fix in &fixes {
            let scene = fix.file.clone().unwrap_or_default();
            if let Err(error) = apply_rename(&scene, fix.fix.as_ref()) {
                return scene_error(error.into(), &scene);
            }
            applied.push(fix_summary(fix));
        }
    }
    CliResult::failure(
        "SCENE_INVALID",
        "the scene still needs key renames after 64 rounds of `rusting fix`",
        None,
    )
}

fn fix_summary(fix: &Diagnostic) -> Value {
    json!({"message": fix.message, "file": fix.file, "operation": fix.fix})
}

fn fix_result(
    mut result: CliResult,
    remaining: Vec<Diagnostic>,
    fixes: Vec<Value>,
    dry_run: bool,
) -> CliResult {
    if !result.data.is_object() {
        result.data = json!({});
    }
    result.data["dry_run"] = json!(dry_run);
    result.data["fixed"] = json!(fixes.len());
    result.data["fixes"] = Value::Array(fixes);
    result.ok = result.ok && remaining.iter().all(|d| d.severity != "error");
    result.diagnostics.extend(remaining);
    result
}

/// Renames the one key a `rename_key` fix names, in place in the file text.
fn apply_rename(scene: &Path, fix: Option<&Value>) -> std::io::Result<()> {
    let text = std::fs::read_to_string(scene)?;
    let fix = fix.cloned().unwrap_or_default();
    let (Some(pointer), Some(to)) = (fix["path"].as_str(), fix["to"].as_str())
    else {
        return Err(std::io::Error::other("the fix is not a key rename"));
    };
    let key = pointer.rsplit('/').next().unwrap_or_default();
    let key = key.replace("~1", "/").replace("~0", "~");
    let [start] = key_positions(&text, &key)[..] else {
        return Err(std::io::Error::other(format!(
            "`{key}` is no longer a key exactly once in the file"
        )));
    };
    let quoted_len = key.len() + 2;
    let text = format!(
        "{}{}{}",
        &text[..start],
        serde_json::to_string(to).unwrap_or_default(),
        &text[start + quoted_len..]
    );
    crate::runtime::write_atomic(scene, text.as_bytes())
}

pub fn cook_project(root: &Path) -> CliResult {
    let project = match open_project(root) {
        Ok(project) => project,
        Err(error) => return project_error(error, root),
    };
    let validation = validate_project(root);
    if !validation.ok {
        return validation;
    }
    let destination = project.root.join(&project.manifest.cooked_scene);
    match cook_scene(&project.scene_path, &destination) {
        Ok(()) => CliResult::success(
            json!({"root": project.root, "source": project.scene_path, "output": destination}),
        ),
        Err(error) => scene_error(error, &project.scene_path),
    }
}

/// Provenance and settings flags shared by `asset import` and `reimport`.
#[derive(Clone, Debug, Default)]
pub struct AssetImportFlags {
    pub folder: PathBuf,
    pub from: Option<PathBuf>,
    pub provenance: AssetProvenance,
    pub max_size: Option<u32>,
    /// `--linear`: cook images as linear data textures.
    pub linear: bool,
    pub dry_run: bool,
}

fn asset_result(
    result: Result<ImportReport, AssetImportError>,
    root: &Path,
) -> CliResult {
    match result {
        Ok(report) => {
            let diagnostics = report
                .warnings
                .iter()
                .map(|warning| Diagnostic {
                    code: if warning.starts_with(asset_import::EDITED_COPY) {
                        "ASSET_EDITED_COPY"
                    } else {
                        "ASSET_NO_LICENSE"
                    },
                    severity: "warning",
                    message: warning.clone(),
                    file: Some(root.join(&report.path)),
                    ..Diagnostic::default()
                })
                .collect();
            CliResult {
                diagnostics,
                ..CliResult::success(json!({ "asset": report }))
            }
        }
        Err(error) => CliResult::failure(
            error.code(),
            error.to_string(),
            error.path().map(Path::to_owned),
        ),
    }
}

pub fn import_asset(
    root: &Path,
    source: &Path,
    flags: &AssetImportFlags,
) -> CliResult {
    let project = match open_project(root) {
        Ok(project) => project,
        Err(error) => return project_error(error, root),
    };
    let settings = ImportSettings {
        max_size: flags.max_size,
        linear: flags.linear,
    };
    asset_result(
        asset_import::import_asset(
            &project.root,
            source,
            &flags.folder,
            &flags.provenance,
            &settings,
            flags.dry_run,
        ),
        &project.root,
    )
}

pub fn reimport_asset(
    root: &Path,
    asset: &str,
    flags: &AssetImportFlags,
) -> CliResult {
    let project = match open_project(root) {
        Ok(project) => project,
        Err(error) => return project_error(error, root),
    };
    let settings =
        (flags.max_size.is_some() || flags.linear).then_some(ImportSettings {
            max_size: flags.max_size,
            linear: flags.linear,
        });
    asset_result(
        asset_import::reimport_asset(
            &project.root,
            asset,
            flags.from.as_deref(),
            &flags.provenance,
            settings.as_ref(),
            flags.dry_run,
        ),
        &project.root,
    )
}

/// Bakes a tree's foliage into alpha-tested cards, and an impostor with
/// `options.impostor`, next to `model` or at `out` (a path without
/// extension). The files are plain GLB; import them with `asset import`.
#[cfg(feature = "gltf")]
pub fn bake_foliage(
    model: &Path,
    out: Option<&Path>,
    options: &crate::foliage_bake::FoliageBakeOptions,
) -> CliResult {
    let out = out.map_or_else(|| model.with_extension(""), Path::to_owned);
    match crate::foliage_bake::bake_foliage(model, &out, options) {
        Ok(bake) => CliResult::success(json!({
            "source_triangles": bake.source_triangles,
            "foliage_triangles": bake.foliage_triangles,
            "cards": bake.cards,
            "card_triangles": bake.card_triangles,
            "impostor_triangles": options.impostor.then_some(4),
            "coverage": bake.coverage,
            "files": bake.files,
        })),
        Err(error) => CliResult::failure(
            "ASSET_BAKE_FAILED",
            error,
            Some(model.to_owned()),
        ),
    }
}

/// Runs a `project.json` generator hook and imports its file into
/// `flags.folder`, or replaces `replace` when given.
pub fn generate_asset(
    root: &Path,
    hook: &str,
    prompt: &str,
    replace: Option<&str>,
    flags: &AssetImportFlags,
) -> CliResult {
    let project = match open_project(root) {
        Ok(project) => project,
        Err(error) => return project_error(error, root),
    };
    let target = match replace {
        Some(asset) => asset_import::GenerateTarget::Replace { asset },
        None => asset_import::GenerateTarget::Import {
            folder: &flags.folder,
        },
    };
    asset_result(
        asset_import::generate_asset(
            &project.root,
            &project.manifest.generators,
            hook,
            prompt,
            target,
            flags.dry_run,
        ),
        &project.root,
    )
}

/// Runs the stale `project.json` scene generators, or the ones in `names`.
pub fn generate_scenes(
    root: &Path,
    names: &[&str],
    force: bool,
    dry_run: bool,
) -> CliResult {
    let project = match open_project(root) {
        Ok(project) => project,
        Err(error) => return project_error(error, root),
    };
    match asset_import::generate_scenes(
        &project.root,
        &project.manifest.scene_generators,
        names,
        force,
        dry_run,
        // Each check plays its scenario on the staged scene.
        &mut |scenario, scene| {
            let run = run_game_project(
                &project.root,
                RunOptions {
                    scenario: Some(scenario.to_owned()),
                    scene: Some(scene.to_owned()),
                    ..RunOptions::default()
                },
            );
            if run.ok {
                return Ok(());
            }
            Err(run
                .diagnostics
                .iter()
                .map(|diagnostic| diagnostic.message.as_str())
                .collect::<Vec<_>>()
                .join("; "))
        },
    ) {
        Ok(scenes) => {
            CliResult::success(json!({ "scenes": scenes, "dry_run": dry_run }))
        }
        Err(error) => CliResult::failure(
            error.code(),
            error.to_string(),
            error.path().map(Path::to_owned),
        ),
    }
}

/// Imported assets with references; asset errors make the result fail.
pub fn list_assets(root: &Path) -> CliResult {
    let project = match open_project(root) {
        Ok(project) => project,
        Err(error) => return project_error(error, root),
    };
    let catalog = asset_import::list_assets(&project.root);
    CliResult {
        schema_version: 1,
        ok: catalog.issues.iter().all(|issue| issue.severity != "error"),
        data: json!({ "assets": catalog.assets }),
        diagnostics: catalog
            .issues
            .into_iter()
            .map(|issue| Diagnostic {
                code: issue.code,
                severity: issue.severity,
                message: issue.message,
                file: Some(issue.file),
                ..Diagnostic::default()
            })
            .collect(),
    }
}

/// `rusting docs`: with no query the item list, else a search, one item, or
/// the brief. `budget` is in tokens.
pub fn docs(action: DocsAction<'_>) -> CliResult {
    use crate::docs::{brief, find, items, search, tokens, within_budget};
    match action {
        DocsAction::List => {
            let list: Vec<_> = items()
                .iter()
                .map(|item| json!({"id": item.id, "kind": item.kind, "title": item.title}))
                .collect();
            CliResult::success(json!({ "items": list }))
        }
        DocsAction::Search(query, limit) => {
            let (matches, total) = search(query, limit);
            let mut data = json!({
                "query": query,
                "matches": matches,
                "shown": matches.len(),
                "total": total,
            });
            if total == 0 {
                data["verdict"] = json!(format!(
                    "No matches for `{query}`. Every word must appear in an item; try fewer words."
                ));
            }
            CliResult::success(data)
        }
        DocsAction::Show(id, budget) => match find(id) {
            Some(item) => {
                let (mut text, truncated) = within_budget(&item.text, budget);
                // `tokens` counts the page text only, not the notice below.
                let shown = tokens(&text);
                if truncated {
                    let more = item.text.lines().count() - text.lines().count();
                    text += &format!(
                        "\n... {more} more lines; pass `--budget {}` to see all\n",
                        tokens(&item.text) + 50
                    );
                }
                CliResult::success(json!({
                    "id": item.id,
                    "title": item.title,
                    "text": text,
                    "tokens": shown,
                    "truncated": truncated,
                    "total_tokens": tokens(&item.text),
                }))
            }
            None => {
                let (hits, _) = search(id, 5);
                let near: Vec<_> =
                    hits.iter().filter_map(|hit| hit["id"].as_str()).collect();
                CliResult::failure(
                    "CLI_USAGE",
                    format!(
                        "no docs item `{id}`; {}",
                        if near.is_empty() {
                            "run `rusting docs` to list them".to_owned()
                        } else {
                            format!("closest: {}", near.join(", "))
                        }
                    ),
                    None,
                )
            }
        },
        DocsAction::Brief(budget) => {
            let (text, listed, total) = brief(budget);
            CliResult::success(json!({
                "text": text,
                "tokens": tokens(&text),
                "listed": listed,
                "total": total,
            }))
        }
    }
}

/// What `rusting docs` was asked to do.
pub enum DocsAction<'a> {
    List,
    /// Query and the most matches to return.
    Search(&'a str, usize),
    /// Item id and token budget.
    Show(&'a str, usize),
    /// Token budget.
    Brief(usize),
}

/// Explains one diagnostic code, or lists every code when `code` is `None`.
pub fn explain(code: Option<&str>) -> CliResult {
    use crate::diagnostics::{lookup, to_json, CODES};
    match code {
        None => CliResult::success(
            json!({ "codes": CODES.iter().map(to_json).collect::<Vec<_>>() }),
        ),
        Some(code) => match lookup(&code.to_ascii_uppercase()) {
            Some(info) => CliResult::success(to_json(info)),
            None => {
                let prefix = code.split('_').next().unwrap_or(code);
                let similar: Vec<_> = CODES
                    .iter()
                    .filter(|info| {
                        info.code.starts_with(&prefix.to_ascii_uppercase())
                    })
                    .map(|info| info.code)
                    .collect();
                CliResult::failure(
                    "CLI_USAGE",
                    if similar.is_empty() {
                        format!("no diagnostic code `{code}`; run `rusting explain` to list them")
                    } else {
                        format!(
                            "no diagnostic code `{code}`; similar: {}",
                            similar.join(", ")
                        )
                    },
                    None,
                )
            }
        },
    }
}

pub fn list_presets() -> CliResult {
    CliResult::success(json!({ "presets": crate::art_direction::PRESETS }))
}

/// Applies an art-direction preset to a scene as one scene patch.
/// `only` limits it to some of `art_direction::PRESET_SCOPES`.
pub fn apply_preset(
    path: &Path,
    name: &str,
    only: &[&str],
    dry_run: bool,
) -> CliResult {
    let scopes = crate::art_direction::PRESET_SCOPES;
    if let Some(scope) = only.iter().find(|scope| !scopes.contains(scope)) {
        return CliResult::failure(
            "PRESET_SCOPE_UNKNOWN",
            format!(
                "no preset scope `{scope}`; choose from {}",
                scopes.join(", ")
            ),
            None,
        );
    }
    let Some(preset) = crate::art_direction::preset(name) else {
        let names: Vec<_> = crate::art_direction::PRESETS
            .iter()
            .map(|preset| preset.name)
            .collect();
        return CliResult::failure(
            "PRESET_UNKNOWN",
            format!("no preset `{name}`; choose one of {}", names.join(", ")),
            None,
        );
    };
    let bytes = match read_scene_bytes(path) {
        Ok(bytes) => bytes,
        Err(error) => return scene_error(error, path),
    };
    let document = match read_scene(path) {
        Ok(document) => document,
        Err(result) => return result,
    };
    let mut patch = crate::art_direction::preset_patch(&document, preset, only);
    patch.expected_revision = Some(crate::runtime::scene_revision(&bytes));
    apply_scene_patch(path, &patch, None, dry_run)
}

pub fn list_effects() -> CliResult {
    let effects: Vec<_> = crate::runtime::EFFECT_PRESETS
        .iter()
        .map(|preset| {
            json!({
                "name": preset.name,
                "summary": preset.summary,
                "height": preset.height,
                "component": preset.emitter(),
            })
        })
        .collect();
    CliResult::success(json!({ "effects": effects }))
}

/// Where `effect apply` puts a preset: on an existing object, or on a new
/// one at a position.
pub enum EffectTarget<'a> {
    On(&'a str),
    New {
        name: Option<&'a str>,
        at: Option<[f32; 3]>,
    },
}

/// Applies a particle effect preset as one scene patch.
pub fn apply_effect(
    path: &Path,
    name: &str,
    target: EffectTarget,
    dry_run: bool,
) -> CliResult {
    let Some(preset) = crate::runtime::effect_preset(name) else {
        let names: Vec<_> = crate::runtime::EFFECT_PRESETS
            .iter()
            .map(|preset| preset.name)
            .collect();
        return CliResult::failure(
            "EFFECT_UNKNOWN",
            format!("no effect `{name}`; choose one of {}", names.join(", ")),
            None,
        );
    };
    let bytes = match read_scene_bytes(path) {
        Ok(bytes) => bytes,
        Err(error) => return scene_error(error, path),
    };
    let document = match read_scene(path) {
        Ok(document) => document,
        Err(result) => return result,
    };
    let component = json!(preset.emitter());
    let key =
        format!("/components/{}", crate::runtime::PARTICLE_EMITTER_COMPONENT);
    let operation = match target {
        EffectTarget::On(object) => crate::scene_patch::PatchOperation::Set {
            id: crate::scene_patch::EntityRef::Name(object.to_owned()),
            path: key,
            value: component,
            expected: None,
        },
        EffectTarget::New { name: object, at } => {
            let title = name
                .split('_')
                .map(|word| {
                    let mut chars = word.chars();
                    chars.next().map_or_else(String::new, |first| {
                        first.to_uppercase().chain(chars).collect()
                    })
                })
                .collect::<Vec<_>>()
                .join(" ");
            let object = object.map_or_else(
                || crate::art_direction::unused_name(&document, &title),
                str::to_owned,
            );
            let at = at.unwrap_or([0.0, preset.height, 0.0]);
            crate::scene_patch::PatchOperation::Create {
                entity: json!({
                    "name": object,
                    "transform": {"position": at, "rotation": [0.0, 0.0, 0.0], "scale": [1.0, 1.0, 1.0]},
                    "components": {crate::runtime::PARTICLE_EMITTER_COMPONENT: component},
                }),
            }
        }
    };
    let patch = crate::scene_patch::ScenePatch {
        expected_revision: Some(crate::runtime::scene_revision(&bytes)),
        operations: vec![operation],
    };
    apply_scene_patch(path, &patch, None, dry_run)
}

pub fn list_recipes() -> CliResult {
    let recipes: Vec<_> = crate::recipes::RECIPES
        .iter()
        .map(|recipe| {
            json!({"name": recipe.name, "summary": recipe.summary, "call": recipe.call})
        })
        .collect();
    CliResult::success(json!({ "recipes": recipes }))
}

/// `rusting recipe apply`: writes `src/<name>.rs` and
/// `tests/<name>.json` and adds the recipe's objects to the main scene as
/// one patch. Never overwrites a file; `src/main.rs` is left for the caller
/// to wire.
pub fn apply_recipe(root: &Path, name: &str, dry_run: bool) -> CliResult {
    let Some(recipe) = crate::recipes::recipe(name) else {
        let names: Vec<_> =
            crate::recipes::RECIPES.iter().map(|r| r.name).collect();
        return CliResult::failure(
            "RECIPE_UNKNOWN",
            format!("no recipe `{name}`; choose one of {}", names.join(", ")),
            None,
        );
    };
    let project = match open_project(root) {
        Ok(project) => project,
        Err(error) => return project_error(error, root),
    };
    let source = recipe
        .source
        .map(|_| project.root.join("src").join(format!("{name}.rs")));
    let scenario = project.root.join("tests").join(format!("{name}.json"));
    for path in source.iter().chain([&scenario]) {
        if path.exists() {
            return CliResult::failure(
                "PROJECT_EXISTS",
                format!("{} already exists; the recipe is applied or the name is taken", path.display()),
                Some(path.clone()),
            );
        }
    }
    let bytes = match read_scene_bytes(&project.scene_path) {
        Ok(bytes) => bytes,
        Err(error) => return scene_error(error, &project.scene_path),
    };
    let document = match read_scene(&project.scene_path) {
        Ok(document) => document,
        Err(result) => return result,
    };
    let Some(player) = document
        .entities
        .iter()
        .find(|entity| entity.name.as_deref() == Some("Player"))
    else {
        return CliResult::failure(
            "RECIPE_NEEDS_PLAYER",
            "the main scene has no object named `Player`",
            Some(project.scene_path),
        );
    };
    let (operations, test) = match (recipe.build)(player) {
        Ok(built) => built,
        Err(message) => {
            return CliResult::failure(
                "RECIPE_NEEDS_CONTROLLER",
                message,
                Some(project.scene_path),
            )
        }
    };
    let patch = crate::scene_patch::ScenePatch {
        expected_revision: Some(crate::runtime::scene_revision(&bytes)),
        operations: operations
            .into_iter()
            .map(|operation| serde_json::from_value(operation).unwrap())
            .collect(),
    };
    let mut result =
        apply_scene_patch(&project.scene_path, &patch, None, dry_run);
    if !result.ok {
        return result;
    }
    if !dry_run {
        let written = source
            .iter()
            .zip(recipe.source)
            .try_for_each(|(path, text)| {
                crate::runtime::write_atomic(path, text.as_bytes())
            })
            .and_then(|()| {
                std::fs::create_dir_all(project.root.join("tests"))?;
                crate::runtime::write_atomic(
                    &scenario,
                    serde_json::to_string_pretty(&test).unwrap().as_bytes(),
                )
            });
        if let Err(error) = written {
            return CliResult::failure(
                "IO_ERROR",
                error.to_string(),
                Some(scenario),
            );
        }
    }
    result.data["source"] = json!(source);
    result.data["scenario"] = json!(scenario);
    result.data["next"] = json!(match recipe.call {
        Some(call) => format!(
            "Add `mod {name};` to src/main.rs, call `{call}` from `update` (`scene` and `time` are its arguments; drop the `_` from `_time`), then run `rusting test`."
        ),
        None => "Nothing to wire in code; run `rusting test`.".to_owned(),
    });
    result
}

fn tool_available(tool: &str) -> bool {
    Command::new(tool).arg("--version").output().is_ok()
}

/// `probe` also opens the device a run would pick and runs a tiny GPU job on
/// it, on a separate thread with a 15 s limit so a wedged driver cannot hang
/// the CLI.
pub fn doctor(probe: bool) -> CliResult {
    let mut devices = Vec::new();
    if let Ok(library) = vulkano::VulkanLibrary::new() {
        if let Ok(instance) =
            vulkano::instance::Instance::new(library, Default::default())
        {
            if let Ok(found) = instance.enumerate_physical_devices() {
                devices = found
                    .map(|device| device.properties().device_name.clone())
                    .collect();
            }
        }
    }
    CliResult::success(json!({
        "engine_version": env!("CARGO_PKG_VERSION"),
        "platform": {"os": std::env::consts::OS, "arch": std::env::consts::ARCH},
        "build_tools": {"cargo": tool_available("cargo"), "rustc": tool_available("rustc"), "glslc": tool_available("glslc")},
        "vulkan": {"available": !devices.is_empty(), "devices": devices},
        "probe": probe.then(probe_gpu),
    }))
}

fn probe_gpu() -> Value {
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = sender.send(crate::rendering::probe_vulkan());
    });
    match receiver.recv_timeout(Duration::from_secs(15)) {
        Ok(Ok(device)) => json!({"ok": true, "selected_device": device}),
        Ok(Err(error)) => json!({"ok": false, "error": error}),
        Err(_) => {
            json!({"ok": false, "error": "the GPU probe did not finish in 15 s"})
        }
    }
}

/// Output of one child process run by the CLI.
struct ProcessRun {
    /// Exit code; `None` when killed at the timeout or by a signal.
    code: Option<i32>,
    success: bool,
    timed_out: bool,
    stdout: String,
    stderr: String,
    duration: Duration,
}

impl ProcessRun {
    fn data(&self) -> Value {
        json!({
            "exit_code": self.code,
            "timed_out": self.timed_out,
            "duration_ms": self.duration.as_millis() as u64,
            "stdout": self.stdout,
            "stderr": self.stderr,
        })
    }
}

/// Runs `command` to completion, or kills it after `timeout`.
fn run_process(
    command: &mut Command,
    timeout: Option<Duration>,
) -> std::io::Result<ProcessRun> {
    run_process_echoing(command, timeout, false)
}

/// [`run_process`], also copying the child's stderr line by line to this
/// process's stderr while it runs when `echo` is set.
fn run_process_echoing(
    command: &mut Command,
    timeout: Option<Duration>,
    echo: bool,
) -> std::io::Result<ProcessRun> {
    use std::io::{BufRead, Read};

    let start = Instant::now();
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let read = |pipe: Option<Box<dyn Read + Send>>| {
        std::thread::spawn(move || {
            let mut text = String::new();
            if let Some(mut pipe) = pipe {
                let mut bytes = Vec::new();
                let _ = pipe.read_to_end(&mut bytes);
                text = String::from_utf8_lossy(&bytes).into_owned();
            }
            text
        })
    };
    let stdout = read(child.stdout.take().map(|pipe| Box::new(pipe) as _));
    let stderr = match (echo, child.stderr.take()) {
        (true, Some(pipe)) => std::thread::spawn(move || {
            let mut text = String::new();
            let mut pipe = std::io::BufReader::new(pipe);
            let mut line = Vec::new();
            while pipe.read_until(b'\n', &mut line).unwrap_or(0) > 0 {
                let chunk = String::from_utf8_lossy(&line);
                eprint!("{chunk}");
                text.push_str(&chunk);
                line.clear();
            }
            text
        }),
        (_, pipe) => read(pipe.map(|pipe| Box::new(pipe) as _)),
    };
    let mut timed_out = false;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if timeout.is_some_and(|timeout| start.elapsed() >= timeout) {
            timed_out = true;
            let _ = child.kill();
            break child.wait()?;
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    Ok(ProcessRun {
        code: status.code(),
        success: status.success(),
        timed_out,
        stdout: stdout.join().unwrap_or_default(),
        stderr: stderr.join().unwrap_or_default(),
        duration: start.elapsed(),
    })
}

/// Last `lines` lines of `text`, for failure messages.
fn tail(text: &str, lines: usize) -> String {
    let all: Vec<_> = text.lines().collect();
    all[all.len().saturating_sub(lines)..].join("\n")
}

/// Runs `cargo <command>` on the project; `Err` is the failed result.
/// Per-file build and asset reload errors, with relative paths resolved
/// against the project root.
fn reload_diagnostics(root: &Path, output: &str) -> Vec<Diagnostic> {
    crate::project::reload_diagnostics(output)
        .into_iter()
        .map(|diagnostic| Diagnostic {
            code: match diagnostic.kind {
                "shader" => "SHADER_BUILD_ERROR",
                "asset" => "ASSET_RELOAD_FAILED",
                _ => "RUST_BUILD_ERROR",
            },
            severity: if diagnostic.kind == "asset" {
                "warning"
            } else {
                "error"
            },
            message: diagnostic.message,
            file: diagnostic.file.map(|file| root.join(file)),
            line: diagnostic.line,
            ..Diagnostic::default()
        })
        .collect()
}

/// Engine patterns that fix common rustc errors in game code: a match is an
/// error line containing every needle of a row.
const ENGINE_HINTS: &[(&[&str], &str)] = &[
    (
        &["error[E0499]", "scene"],
        "A `GameObject` or counter borrow keeps `scene` busy. Copy the values you need out (`let x = scene.object(\"A\").position();`), or finish each object chain in one statement before the next `scene` call.",
    ),
    (
        &["error[E0502]", "scene"],
        "A borrow from `scene` is still alive. Read into a local first, then write: `let p = scene.object(\"A\").position(); scene.object(\"B\").set_position(p);`.",
    ),
    (
        &["error[E0599]", "GameScene"],
        "`GameScene` has no such method. Run `rusting docs search <word>` to find the real name in the API index.",
    ),
    (
        &["error[E0599]", "GameObject"],
        "`GameObject` has no such method. Run `rusting docs search GameObject` to list its methods.",
    ),
    (
        &["error[E0432]", "rusting_engine"],
        "Import from the prelude: `use rusting_engine::prelude::*;`. Run `rusting docs show api/GameScene::object` for signatures.",
    ),
    (
        &["error[E0308]", "f64"],
        "Scene values are `f32`. Write `1.0_f32`, `x as f32`, or let inference pick `f32` by removing a `f64` annotation.",
    ),
    (
        &["error[E0308]", "[f32; 3]"],
        "Positions, rotations (radians) and scales are `[f32; 3]` arrays: `[x, y, z]`, not a tuple or a slice.",
    ),
];

/// One `RUST_ENGINE_HINT` per distinct engine pattern seen in rustc output.
fn engine_hints(root: &Path, output: &str) -> Vec<Diagnostic> {
    let mut hints: Vec<Diagnostic> = Vec::new();
    for line in output.lines() {
        for (needles, hint) in ENGINE_HINTS {
            if !needles.iter().all(|needle| line.contains(needle))
                || hints.iter().any(|seen| seen.message == *hint)
            {
                continue;
            }
            let mut place =
                line.split(": ").next().unwrap_or_default().split(':');
            let file = place.next().filter(|f| f.ends_with(".rs"));
            let number = place.next().and_then(|n| n.parse().ok());
            hints.push(Diagnostic {
                code: "RUST_ENGINE_HINT",
                severity: "hint",
                message: (*hint).to_owned(),
                file: file.map(|file| root.join(file)),
                line: number,
                ..Diagnostic::default()
            });
        }
    }
    hints
}

fn cargo(
    project: &OpenProject,
    command: &str,
    release: bool,
    target: Option<&str>,
    features: &[String],
) -> Result<ProcessRun, CliResult> {
    let mut cargo = Command::new("cargo");
    cargo.arg(command);
    if crate::runtime::lease::offline() {
        cargo.arg("--offline");
    }
    if release {
        cargo.arg("--release");
    }
    if let Some(target) = target {
        cargo.args(["--target", target]);
    }
    if !features.is_empty() {
        cargo.args(["--features", &features.join(",")]);
    }
    cargo
        .args(["--message-format", "short", "--manifest-path"])
        .arg(project.root.join("Cargo.toml"))
        .current_dir(&project.root);
    let run = run_process(&mut cargo, None).map_err(|error| {
        CliResult::failure(
            "BUILD_TOOL_MISSING",
            format!("could not run cargo: {error}"),
            None,
        )
    })?;
    if run.success {
        return Ok(run);
    }
    let mut result = CliResult::failure(
        "BUILD_FAILED",
        format!("cargo {command} failed:\n{}", tail(&run.stderr, 40)),
        Some(project.root.join("Cargo.toml")),
    );
    result.data = json!({"root": project.root, "cargo": run.data()});
    result
        .diagnostics
        .extend(reload_diagnostics(&project.root, &run.stderr));
    result
        .diagnostics
        .extend(engine_hints(&project.root, &run.stderr));
    Err(result)
}

/// Writes a deliberately failing scenario for `name`. The check stays red
/// until the agent names a real entity and value.
fn scaffold_scenario(root: &Path, name: &str) -> Result<PathBuf, CliResult> {
    let path = root.join("tests").join(format!("{name}.json"));
    if path.exists() {
        return Err(CliResult::failure(
            "PROJECT_EXISTS",
            format!("{} already exists", path.display()),
            Some(path),
        ));
    }
    let scenario = json!({
        "name": format!("{name} works (edit this scenario)"),
        "ticks": 10,
        "steps": [{"tick": 1, "expect": {
            "entity": "TODO entity name",
            "path": "/transform/position",
            "exists": true
        }}]
    });
    let written = std::fs::create_dir_all(root.join("tests")).and_then(|()| {
        crate::runtime::write_atomic(
            &path,
            serde_json::to_string_pretty(&scenario).unwrap().as_bytes(),
        )
    });
    match written {
        Ok(()) => Ok(path),
        Err(error) => Err(CliResult::failure(
            "IO_ERROR",
            error.to_string(),
            Some(path),
        )),
    }
}

fn scaffold_name(name: &str) -> Result<(), CliResult> {
    let valid = name.chars().next().is_some_and(|c| c.is_ascii_lowercase())
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
    if valid {
        return Ok(());
    }
    Err(CliResult::failure(
        "CLI_USAGE",
        format!("`{name}` is not a snake_case name like `spin_coins`"),
        None,
    ))
}

/// `rusting add scenario`: a failing scenario in `tests/`.
pub fn add_scenario(root: &Path, name: &str) -> CliResult {
    if let Err(result) = scaffold_name(name) {
        return result;
    }
    match scaffold_scenario(root, name) {
        Ok(path) => CliResult::success(json!({
            "scenario": path,
            "next": "Name a real entity and expected value in the scenario, then run `rusting test`.",
        })),
        Err(result) => result,
    }
}

/// `rusting add system`: a documented stub in `src/main.rs` plus a failing
/// scenario. The stub is not called until `update` calls it.
pub fn add_system(root: &Path, name: &str) -> CliResult {
    if let Err(result) = scaffold_name(name) {
        return result;
    }
    let code_path = root.join("src/main.rs");
    let source = match std::fs::read_to_string(&code_path) {
        Ok(source) => source,
        Err(error) => {
            return CliResult::failure(
                "PROJECT_MISSING_FILE",
                error.to_string(),
                Some(code_path),
            )
        }
    };
    if source.contains(&format!("fn {name}(")) {
        return CliResult::failure(
            "PROJECT_EXISTS",
            format!("`fn {name}` already exists in src/main.rs"),
            Some(code_path),
        );
    }
    let scenario = match scaffold_scenario(root, name) {
        Ok(path) => path,
        Err(result) => return result,
    };
    let stub = format!(
        "\n/// TODO: describe what `{name}` does each tick.\nfn {name}(_scene: &mut GameScene<'_>, _time: &FrameTime) {{\n    todo!(\"implement {name}\");\n}}\n"
    );
    if let Err(error) =
        crate::runtime::write_atomic(&code_path, (source + &stub).as_bytes())
    {
        return CliResult::failure(
            "IO_ERROR",
            error.to_string(),
            Some(code_path),
        );
    }
    CliResult::success(json!({
        "function": code_path,
        "scenario": scenario,
        "next": format!("Call `{name}(scene, time)` from `update`, replace the `todo!`, and make the scenario check real values."),
    }))
}

/// Every leaf of `value` keyed by its JSON pointer path.
fn flatten_leaves(
    value: &Value,
    path: String,
    out: &mut BTreeMap<String, Value>,
) {
    match value {
        Value::Object(map) if !map.is_empty() => {
            for (key, inner) in map {
                flatten_leaves(inner, format!("{path}/{key}"), out);
            }
        }
        Value::Array(items)
            if !items.is_empty() && items.iter().any(|i| i.is_object()) =>
        {
            for (index, inner) in items.iter().enumerate() {
                flatten_leaves(inner, format!("{path}/{index}"), out);
            }
        }
        leaf => {
            out.insert(path, leaf.clone());
        }
    }
}

pub(crate) fn leaf_changes(before: &Value, after: &Value) -> Vec<Value> {
    let (mut a, mut b) = (BTreeMap::new(), BTreeMap::new());
    flatten_leaves(before, String::new(), &mut a);
    flatten_leaves(after, String::new(), &mut b);
    let paths: BTreeSet<_> = a.keys().chain(b.keys()).cloned().collect();
    paths
        .into_iter()
        .filter(|path| a.get(path) != b.get(path))
        .map(|path| json!({"path": path, "before": a.get(&path), "after": b.get(&path)}))
        .collect()
}

/// `rusting diff`: entities added, removed and changed (by ID) between two
/// scenes, with the path, old value and new value of every changed field.
pub fn diff_scenes(before: &Path, after: &Path) -> CliResult {
    let mut documents = Vec::new();
    for path in [before, after] {
        match read_scene(path) {
            Ok(document) => documents
                .push(serde_json::to_value(document).unwrap_or(Value::Null)),
            Err(result) => return result,
        }
    }
    let by_id = |document: &Value| -> BTreeMap<String, Value> {
        document["entities"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|entity| {
                (
                    entity["id"].as_str().unwrap_or("").to_owned(),
                    entity.clone(),
                )
            })
            .collect()
    };
    let (old, new) = (by_id(&documents[0]), by_id(&documents[1]));
    let brief =
        |entity: &Value| json!({"id": entity["id"], "name": entity["name"]});
    let added: Vec<_> = new
        .iter()
        .filter(|(id, _)| !old.contains_key(*id))
        .map(|(_, e)| brief(e))
        .collect();
    let removed: Vec<_> = old
        .iter()
        .filter(|(id, _)| !new.contains_key(*id))
        .map(|(_, e)| brief(e))
        .collect();
    let changed: Vec<_> = old
        .iter()
        .filter_map(|(id, entity)| {
            let changes = leaf_changes(entity, new.get(id)?);
            (!changes.is_empty()).then(|| json!({"id": id, "name": new[id]["name"], "changes": changes}))
        })
        .collect();
    let mut scene_a = documents[0].clone();
    let mut scene_b = documents[1].clone();
    scene_a["entities"] = Value::Null;
    scene_b["entities"] = Value::Null;
    let scene = leaf_changes(&scene_a, &scene_b);
    CliResult::success(json!({
        "identical": added.is_empty() && removed.is_empty() && changed.is_empty() && scene.is_empty(),
        "scene": scene,
        "added": added,
        "removed": removed,
        "changed": changed,
    }))
}

/// `rusting scene map`: each tile map as a character grid with a legend.
/// Other entities that sit over a map are drawn as letters, so a model
/// without vision can check a 2D layout.
pub fn map_scene(path: &Path) -> CliResult {
    let document = match read_scene(path) {
        Ok(document) => document,
        Err(result) => return result,
    };
    let origin = |entity: &SceneEntity| {
        entity
            .transform
            .map_or([0.0, 0.0], |t| [t.position[0], t.position[1]])
    };
    let mut maps = Vec::new();
    for entity in &document.entities {
        let Some(text) =
            entity.components.get(crate::runtime::TILE_MAP_COMPONENT)
        else {
            continue;
        };
        let Ok(tile_map) =
            serde_json::from_str::<crate::runtime::TileMap>(text)
        else {
            continue;
        };
        let mut grid: Vec<Vec<char>> = tile_map
            .rows
            .iter()
            .map(|row| row.chars().collect())
            .collect();
        let mut legend: Vec<Value> = tile_map
            .tiles
            .iter()
            .map(|(key, kind)| json!({"symbol": key, "meaning": if kind.solid { "solid tile" } else { "tile" }}))
            .collect();
        let mut markers = ('A'..='Z')
            .filter(|c| !tile_map.tiles.contains_key(&c.to_string()));
        let base = origin(entity);
        for other in document
            .entities
            .iter()
            .filter(|o| o.id != entity.id && o.transform.is_some())
        {
            let at = origin(other);
            let column = ((at[0] - base[0]) / tile_map.tile_size).floor();
            let row = (-(at[1] - base[1]) / tile_map.tile_size).floor();
            let in_grid = column >= 0.0
                && row >= 0.0
                && grid
                    .get(row as usize)
                    .is_some_and(|r| (column as usize) < r.len());
            let Some(marker) = in_grid.then(|| markers.next()).flatten() else {
                continue;
            };
            grid[row as usize][column as usize] = marker;
            legend.push(json!({"symbol": marker.to_string(), "entity": other.name, "id": other.id, "column": column as usize, "row": row as usize}));
        }
        maps.push(json!({
            "entity": entity.name,
            "id": entity.id,
            "tile_size": tile_map.tile_size,
            "rows": grid.iter().map(|row| row.iter().collect::<String>()).collect::<Vec<_>>(),
            "legend": legend,
        }));
    }
    CliResult::success(json!({"maps": maps}))
}

/// `rusting inspect --tick N`: runs the game without a window to tick `N`
/// and reports the scene form of each named entity (all named entities of
/// the main scene when `entities` is empty) after that tick's update.
pub fn inspect_tick(root: &Path, tick: u32, entities: &[String]) -> CliResult {
    let project = match open_project(root) {
        Ok(project) => project,
        Err(error) => return project_error(error, root),
    };
    let names: Vec<String> = if entities.is_empty() {
        match read_scene(&project.scene_path) {
            Ok(document) => document
                .entities
                .iter()
                .filter_map(|entity| entity.name.clone())
                .collect(),
            Err(result) => return result,
        }
    } else {
        entities.to_vec()
    };
    let steps: Vec<_> = names
        .iter()
        .map(|name| json!({"tick": tick, "log": {"entity": name, "path": ""}}))
        .collect();
    let scenario = json!({"name": format!("inspect tick {tick}"), "ticks": tick, "steps": steps});
    let file = project.root.join("build/inspect-tick.json");
    let written = std::fs::create_dir_all(project.root.join("build"))
        .and_then(|()| std::fs::write(&file, scenario.to_string()));
    if let Err(error) = written {
        return CliResult::failure("IO_ERROR", error.to_string(), Some(file));
    }
    let mut result = run_game_project(
        root,
        RunOptions {
            scenario: Some(file),
            ..RunOptions::default()
        },
    );
    if result.ok {
        let state: serde_json::Map<String, Value> = result.data["scenario"]
            ["steps"]
            .as_array()
            .into_iter()
            .flatten()
            .zip(&names)
            .map(|(step, name)| (name.clone(), step["actual"].clone()))
            .collect();
        let mut state = state;
        for entity in state.values_mut() {
            if let Some(feel) = controller_feel(&entity["components"]) {
                entity["feel"] = feel;
            }
        }
        result.data = json!({"tick": tick, "entities": state});
    }
    result
}

/// Jump and run numbers in physical units for a player or platformer
/// controller in an entity's `components`, with `notes` naming any number
/// outside its genre range. Both controllers set their speed directly, so
/// top speed takes one fixed step, stopping is instant and air control is
/// full.
fn controller_feel(components: &Value) -> Option<Value> {
    use crate::runtime::{
        PlatformerController, PlayerController,
        PLATFORMER_CONTROLLER_COMPONENT, PLAYER_CONTROLLER_COMPONENT,
    };
    // ponytail: rough ranges read off common games, not measured
    // studies; (top speed m/s, jump apex m, air time s).
    let (genre, speed, jump, gravity, sprint, ranges) = if let Some(value) =
        components.get(PLAYER_CONTROLLER_COMPONENT)
    {
        let c: PlayerController = serde_json::from_value(value.clone()).ok()?;
        let ranges = [[3.0, 8.0], [0.4, 1.6], [0.4, 1.2]];
        let sprint = c.walk_speed * c.sprint_multiplier;
        (
            "first or third person",
            c.walk_speed,
            c.jump_speed,
            c.gravity,
            Some(sprint),
            ranges,
        )
    } else {
        let value = components.get(PLATFORMER_CONTROLLER_COMPONENT)?;
        let c: PlatformerController =
            serde_json::from_value(value.clone()).ok()?;
        let ranges = [[4.0, 12.0], [1.0, 5.0], [0.5, 1.2]];
        (
            "2D platformer",
            c.run_speed,
            c.jump_speed,
            c.gravity,
            None,
            ranges,
        )
    };
    let apex_s = if gravity > 0.0 {
        jump / gravity
    } else {
        f32::INFINITY
    };
    let apex_m = jump * apex_s / 2.0;
    let air_s = 2.0 * apex_s;
    let mut notes = Vec::new();
    for ((label, value), [low, high]) in [
        ("top speed (m/s)", speed),
        ("jump apex (m)", apex_m),
        ("air time (s)", air_s),
    ]
    .into_iter()
    .zip(ranges)
    {
        if !(low..=high).contains(&value) {
            notes.push(format!(
                "{label} {value:.2} is outside the {genre} range {low} to {high}"
            ));
        }
    }
    Some(json!({
        "genre": genre,
        "top_speed_m_s": speed,
        "sprint_speed_m_s": sprint,
        "time_to_top_speed_s": crate::runtime::FrameTime::default().fixed_delta.as_secs_f32(),
        "stopping_distance_m": 0.0,
        "air_control": 1.0,
        "jump_apex_m": apex_m,
        "jump_apex_s": apex_s,
        "air_time_s": air_s,
        "jump_distance_m": speed * air_s,
        "ranges": {"top_speed_m_s": ranges[0], "jump_apex_m": ranges[1], "air_time_s": ranges[2]},
        "notes": notes,
    }))
}

/// Validates the project and compiles it with the debug build that `run`
/// and `test` reuse.
pub fn check_project(root: &Path) -> CliResult {
    let validation = validate_project(root);
    if !validation.ok {
        return validation;
    }
    let project = match open_project(root) {
        Ok(project) => project,
        Err(error) => return project_error(error, root),
    };
    // `cargo build`, not `cargo check`: the debug build is the one `run` and
    // `test` reuse, so checking first costs no second engine compile.
    match cargo(&project, "build", false, None, &[]) {
        Ok(run) => {
            let mut result = CliResult::success(
                json!({"root": project.root, "cargo": run.data()}),
            );
            result.diagnostics.extend(outdated_cli(&project.root));
            result.diagnostics.extend(outdated_agents(&project.root));
            result
        }
        Err(result) => result,
    }
}

/// The AGENTS.md `rusting new` writes.
const PROJECT_AGENTS: &str = include_str!("project_agents.md");

/// An `AGENTS_OUTDATED` warning when the project's AGENTS.md is an engine
/// copy (it opens like one) that differs from this engine's.
fn outdated_agents(root: &Path) -> Option<Diagnostic> {
    let path = root.join("AGENTS.md");
    let text = std::fs::read_to_string(&path).ok()?;
    let opening = "# Agent guide\n\nThis is a Rusting game project";
    (text.starts_with(opening) && text != PROJECT_AGENTS).then(|| Diagnostic {
        code: "AGENTS_OUTDATED",
        severity: "warning",
        message: "AGENTS.md differs from this engine's copy, so it may miss \
                  newer features; `rusting fix` replaces it and keeps yours \
                  as AGENTS.md.old"
            .into(),
        file: Some(path),
        ..Diagnostic::default()
    })
}

/// A `CLI_OUTDATED` warning when the game's `rusting_engine` has another
/// version than this CLI, or is a path dependency with source newer than
/// this executable (an engine edited after `cargo install`).
fn outdated_cli(root: &Path) -> Option<Diagnostic> {
    // Cargo.lock lists `name`, then `version`, then `source` only for a
    // registry or git package.
    let lock = std::fs::read_to_string(root.join("Cargo.lock")).ok()?;
    let package = lock
        .split("[[package]]")
        .find(|package| package.contains("name = \"rusting_engine\""))?;
    let version = package
        .lines()
        .find_map(|line| line.strip_prefix("version = "))?
        .trim_matches('"');
    let manifest = std::fs::read_to_string(root.join("Cargo.toml")).ok()?;
    let engine_path = manifest
        .lines()
        .find(|line| line.trim_start().starts_with("rusting_engine"))
        .and_then(|line| line.split("path = \"").nth(1))
        .and_then(|rest| rest.split('"').next())
        .filter(|_| !package.contains("source = "));
    let install = engine_path.map_or_else(
        || "cargo install rusting_engine --locked".to_owned(),
        |path| {
            format!(
                "cargo install --path {} --locked",
                root.join(path).display()
            )
        },
    );
    let message = if version != env!("CARGO_PKG_VERSION") {
        format!(
            "this CLI is RustingEngine {}, the game builds against {version}",
            env!("CARGO_PKG_VERSION")
        )
    } else {
        let source = root.join(engine_path?).join("src");
        let installed = std::env::current_exe()
            .ok()?
            .metadata()
            .ok()?
            .modified()
            .ok()?;
        let (newest, file) = newest_modified(&source)?;
        let later = newest
            .duration_since(installed)
            .ok()
            .filter(|later| !later.is_zero())?
            .as_secs();
        // Expected while the engine is being edited, so it shows once a
        // day per project instead of hiding real warnings on every check.
        if !once_a_day(&root.join("build/cli-outdated-shown")) {
            return None;
        }
        let later = match later {
            0..3600 => format!("{} min", later / 60),
            3600..86_400 => format!("{} h", later / 3600),
            _ => format!("{} days", later / 86_400),
        };
        format!(
            "the engine source changed after this CLI was built: {} is {later} newer{}",
            file.display(),
            engine_commits_since(&source, installed)
        )
    };
    Some(Diagnostic {
        code: "CLI_OUTDATED",
        severity: "warning",
        message: format!(
            "{message}; docs and schema may be stale. Reinstall: {install}"
        ),
        ..Diagnostic::default()
    })
}

/// The engine's git commits under `source` after `since`, newest first, as
/// "; engine commits since: ..." so a reader sees what the CLI misses.
/// Empty outside a git checkout or without such commits.
fn engine_commits_since(source: &Path, since: std::time::SystemTime) -> String {
    let Ok(since) = since.duration_since(std::time::UNIX_EPOCH) else {
        return String::new();
    };
    let log = Command::new("git")
        .arg("-C")
        .arg(source)
        .args(["log", "--format=%h %s"])
        .arg(format!("--since=@{}", since.as_secs()))
        .args(["--", "."])
        .output();
    let log = match log {
        Ok(log) if log.status.success() => log.stdout,
        _ => return String::new(),
    };
    let commits: Vec<_> = String::from_utf8_lossy(&log)
        .lines()
        .map(str::to_owned)
        .collect();
    if commits.is_empty() {
        return String::new();
    }
    let more = commits.len().saturating_sub(5);
    let mut text = format!(
        "; engine commits since: {}",
        commits[..commits.len() - more].join("; ")
    );
    if more > 0 {
        text += &format!("; and {more} more");
    }
    text
}

/// True, and touches `marker`, when `marker` is missing or older than a
/// day.
fn once_a_day(marker: &Path) -> bool {
    let recent = std::fs::metadata(marker)
        .and_then(|meta| meta.modified())
        .ok()
        .and_then(|time| time.elapsed().ok())
        .is_some_and(|age| age.as_secs() < 86_400);
    if recent {
        return false;
    }
    if let Some(folder) = marker.parent() {
        let _ = std::fs::create_dir_all(folder);
    }
    // Never follow a planted symlink, and never truncate: only the
    // modified time of a plain file is touched.
    if std::fs::symlink_metadata(marker)
        .is_ok_and(|meta| meta.file_type().is_symlink())
    {
        return false;
    }
    let _ = std::fs::File::options()
        .create(true)
        .write(true)
        .truncate(false)
        .open(marker)
        .and_then(|file| file.set_modified(std::time::SystemTime::now()));
    true
}

/// The newest file under `folder` and when it changed.
fn newest_modified(folder: &Path) -> Option<(std::time::SystemTime, PathBuf)> {
    std::fs::read_dir(folder)
        .ok()?
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            if path.is_dir() {
                newest_modified(&path)
            } else {
                Some((entry.metadata().ok()?.modified().ok()?, path))
            }
        })
        .max()
}

/// How `rusting run` and `rusting test` start the game.
#[derive(Clone, Debug, Default)]
pub struct RunOptions {
    pub release: bool,
    /// Run this many ticks without a window, then exit.
    pub headless_ticks: Option<u32>,
    /// Stop a game still running after this long. Reaching it is not a
    /// failure: the game ran without crashing until then.
    pub timeout: Option<Duration>,
    /// Run this scenario file instead, and fail when it fails.
    pub scenario: Option<PathBuf>,
    /// Record the windowed session's input to this replay file on exit.
    pub record: Option<PathBuf>,
    /// Play this replay file back headless, failing where a tick's state
    /// hash differs from the recording.
    pub replay: Option<PathBuf>,
    /// Rewrite scenario `golden` images instead of comparing them.
    pub update_golden: bool,
    /// Run the scenario to its last tick after a failed check.
    pub keep_going: bool,
    /// Measure this many windowed frames, report their times, and close.
    pub bench: Option<u32>,
    /// Copy the game's stderr to this process's stderr as it arrives
    /// (`--stderr`), as well as keeping it for the result.
    pub echo_stderr: bool,
    /// Scene components every scene load leaves out (`--without`); the run
    /// then passes only when its scenario fails.
    pub without: Vec<String>,
    /// Seed for the game's random streams (`--seed`), 0 by default.
    pub seed: Option<u64>,
    /// Run this already built game executable (`--exe`), such as an
    /// export, from its own folder instead of cooking and building the
    /// project.
    pub exe: Option<PathBuf>,
    /// Run as a dedicated server (`--server`): no window, real-time ticks
    /// until the game quits or `timeout` stops it.
    pub server: bool,
    /// Load this scene instead of the cooked main scene, through
    /// `RUSTING_SCENE_PATH`, such as a generated scene being checked.
    pub scene: Option<PathBuf>,
}

/// Cooks the main scene, builds the game, and runs it as a debug session:
/// the game's standard input and output carry the line protocol of
/// [`crate::debug_session`]. Returns only a failure; on success the game
/// ran until `quit` or end of input.
pub fn debug_game_project(root: &Path) -> CliResult {
    let cooked = cook_project(root);
    if !cooked.ok {
        return cooked;
    }
    let project = match open_project(root) {
        Ok(project) => project,
        Err(error) => return project_error(error, root),
    };
    if let Err(result) = cargo(&project, "build", false, None, &[]) {
        return result;
    }
    let executable = match crate::project::built_executable(
        &project.root,
        &project.root.join("Cargo.toml"),
        &project.manifest.binary_name,
        None,
        false,
    ) {
        Ok(executable) => executable,
        Err(error) => return CliResult::failure("BUILD_FAILED", error, None),
    };
    match Command::new(&executable)
        .current_dir(&project.root)
        .env(crate::debug_session::DEBUG_SESSION_ENV, "1")
        .status()
    {
        Ok(status) if status.success() => CliResult::success(json!({})),
        Ok(status) => CliResult::failure(
            "GAME_FAILED",
            format!("the game exited with {status}"),
            Some(executable),
        ),
        Err(error) => CliResult::failure(
            "GAME_FAILED",
            format!("could not start {}: {error}", executable.display()),
            Some(executable),
        ),
    }
}

/// Cooks the main scene, builds the game, and runs it from the project
/// folder, the same way the editor's Build and Run does.
pub fn run_game_project(root: &Path, options: RunOptions) -> CliResult {
    if options.without.is_empty() || options.scenario.is_none() {
        return run_game_project_once(root, options);
    }
    let names = options.without.join(", ");
    let scenario = options.scenario.clone();
    let mut result = run_game_project_once(root, options);
    let scenario_failed =
        result.diagnostics.first().is_some_and(|diagnostic| {
            // A game that crashes without the component also fails.
            matches!(diagnostic.code, "SCENARIO_FAILED" | "GAME_FAILED")
        });
    if scenario_failed {
        let failure = result.diagnostics.remove(0).message;
        result.ok = true;
        result.data["without"] =
            json!({"components": names, "failure": failure});
        result
    } else if result.ok {
        CliResult::failure(
            "SCENARIO_TOO_WEAK",
            format!(
                "the scenario still passes without {names}, so it does not \
                 check what {names} does; add a step that fails without it, \
                 or check the name against the scene (`collider`, \
                 `rigid_body`, or a registered component)"
            ),
            scenario,
        )
    } else {
        result
    }
}

/// Runs `scenario` once per seed with seeded random presses of its named
/// actions (the scenario's `fuzz` section) and stops at the first seed that
/// fails a step or an invariant, or crashes the game. That run is written as
/// a ready scenario file, `build/fuzz/seed-N.json`, with the presses as
/// ordinary steps up to the failing tick.
pub fn fuzz_game_project(
    root: &Path,
    scenario: &Path,
    seeds: std::ops::Range<u64>,
    actions: &[String],
) -> CliResult {
    let base: serde_json::Value = match std::fs::read(scenario)
        .map_err(|error| error.to_string())
        .and_then(|bytes| {
            serde_json::from_slice(&bytes).map_err(|error| error.to_string())
        }) {
        Ok(base) => base,
        Err(error) => {
            return CliResult::failure(
                "FILE_NOT_FOUND",
                format!("cannot read scenario {}: {error}", scenario.display()),
                Some(scenario.to_path_buf()),
            )
        }
    };
    let folder = root.join("build/fuzz");
    let _ = std::fs::create_dir_all(&folder);
    let run_file = folder.join("run.json");
    let mut tried = 0;
    for seed in seeds {
        let mut run = base.clone();
        // The scenario's own `fuzz` actions and rate stay unless named.
        run["fuzz"]["seed"] = json!(seed);
        if !actions.is_empty() {
            run["fuzz"]["actions"] = json!(actions);
        }
        if let Err(error) = std::fs::write(&run_file, run.to_string()) {
            return CliResult::failure(
                "IO_ERROR",
                error.to_string(),
                Some(run_file),
            );
        }
        let mut result = run_game_project(
            root,
            RunOptions {
                scenario: Some(run_file.clone()),
                ..RunOptions::default()
            },
        );
        tried += 1;
        let failed = result.diagnostics.first().is_some_and(|diagnostic| {
            matches!(diagnostic.code, "SCENARIO_FAILED" | "GAME_FAILED")
        });
        if result.ok || !failed {
            if result.ok {
                continue;
            }
            return result;
        }
        // The presses become plain steps; a crash without a report keeps
        // the `fuzz` section, which replays the same presses.
        let report = &result.data["scenario"];
        if let (Some(tick), Some(steps)) = (
            report["first_failure"]["tick"].as_u64(),
            report["fuzz_steps"].as_array(),
        ) {
            let mut steps: Vec<_> = steps
                .iter()
                .filter(|step| step["tick"].as_u64() <= Some(tick))
                .cloned()
                .collect();
            if let Some(own) = base["steps"].as_array() {
                steps.extend(own.iter().cloned());
            }
            steps.sort_by_key(|step| step["tick"].as_u64());
            run.as_object_mut().map(|run| run.remove("fuzz"));
            run["steps"] = json!(steps);
            run["ticks"] = json!(tick);
        }
        let found = folder.join(format!("seed-{seed}.json"));
        let text = serde_json::to_string_pretty(&run).unwrap_or_default();
        if let Err(error) = std::fs::write(&found, text + "\n") {
            return CliResult::failure(
                "IO_ERROR",
                error.to_string(),
                Some(found),
            );
        }
        let diagnostic = &mut result.diagnostics[0];
        diagnostic.message = format!(
            "seed {seed} fails; replay it with `rusting test {} {}`:\n{}",
            root.display(),
            found.display(),
            diagnostic.message
        );
        diagnostic.file = Some(found.clone());
        result.data["fuzz"] =
            json!({"seed": seed, "tried": tried, "scenario": found});
        return result;
    }
    CliResult::success(json!({"fuzz": {"tried": tried, "failed": null}}))
}

fn run_game_project_once(root: &Path, options: RunOptions) -> CliResult {
    let start = Instant::now();
    let headless = options.headless_ticks.is_some()
        || options.scenario.is_some()
        || options.server;
    if options.record.is_some() && (headless || options.replay.is_some()) {
        return CliResult::failure(
            "CLI_USAGE",
            "--record records a windowed session; it cannot be combined with \
             --ticks, --scenario or --replay (a scenario file is already a \
             recording)",
            None,
        );
    }
    if let Some(replay) = &options.replay {
        if headless {
            return CliResult::failure(
                "CLI_USAGE",
                "--replay runs headless on its own; drop --ticks and --scenario",
                None,
            );
        }
        if !replay.is_file() {
            return CliResult::failure(
                "FILE_NOT_FOUND",
                format!("no replay file at {}", replay.display()),
                Some(replay.clone()),
            );
        }
    }
    let (root, executable, build_duration) = match &options.exe {
        Some(exe) => {
            let Some(executable) = std::path::absolute(exe)
                .ok()
                .filter(|executable| executable.is_file())
            else {
                return CliResult::failure(
                    "FILE_NOT_FOUND",
                    format!("no game executable at {}", exe.display()),
                    Some(exe.clone()),
                );
            };
            // Reports and test data go under the project's build folder,
            // never into the export.
            let _ = std::fs::create_dir_all(root.join("build"));
            (root.to_path_buf(), executable, Duration::ZERO)
        }
        None => {
            let cooked = cook_project(root);
            if !cooked.ok {
                return cooked;
            }
            let project = match open_project(root) {
                Ok(project) => project,
                Err(error) => return project_error(error, root),
            };
            let build =
                match cargo(&project, "build", options.release, None, &[]) {
                    Ok(build) => build,
                    Err(result) => return result,
                };
            match crate::project::built_executable(
                &project.root,
                &project.root.join("Cargo.toml"),
                &project.manifest.binary_name,
                None,
                options.release,
            ) {
                Ok(executable) => (project.root, executable, build.duration),
                Err(error) => {
                    return CliResult::failure("BUILD_FAILED", error, None)
                }
            }
        }
    };
    let mut game = Command::new(&executable);
    match &options.exe {
        // An export finds its scene and assets beside itself.
        Some(_) => {
            game.current_dir(executable.parent().unwrap_or(&root))
                .env_remove("RUSTING_SCENE_PATH");
        }
        None => {
            game.current_dir(&root);
        }
    }
    if let Some(scene) = &options.scene {
        game.env("RUSTING_SCENE_PATH", scene);
    }
    let final_scene = root.join("build/final.rscene");
    if let Some(ticks) = options.headless_ticks {
        let _ = std::fs::remove_file(&final_scene);
        game.env(crate::project::HEADLESS_TICKS_ENV, ticks.to_string())
            .env(crate::project::FINAL_SCENE_OUT_ENV, &final_scene);
    }
    if options.server {
        game.env(crate::project::SERVER_ENV, "1");
    }
    if let Some(frames) = options.bench {
        game.env(crate::project::BENCH_FRAMES_ENV, frames.to_string());
    }
    if let Some(seed) = options.seed {
        game.env(crate::project::SEED_ENV, seed.to_string());
    }
    for (path, variable) in [
        (&options.record, crate::project::REPLAY_OUT_ENV),
        (&options.replay, crate::project::REPLAY_PLAY_ENV),
    ] {
        if let Some(path) = path {
            // The game runs in the project root, not the caller's folder.
            game.env(variable, std::path::absolute(path).unwrap_or_default());
        }
    }
    // Each run gets its own report file, so two runs in one project at
    // once each read their own; the last one is kept as
    // build/scenario-report.json.
    let last_report = root.join("build/scenario-report.json");
    let report_path = root.join(format!(
        "build/scenario-report.{}.json",
        uuid::Uuid::new_v4().simple()
    ));
    if let Some(scenario) = &options.scenario {
        let scenario = match std::path::absolute(scenario) {
            Ok(scenario) if scenario.is_file() => scenario,
            _ => {
                return CliResult::failure(
                    "FILE_NOT_FOUND",
                    format!("no scenario file at {}", scenario.display()),
                    Some(scenario.clone()),
                )
            }
        };
        let _ = std::fs::create_dir_all(root.join("build"));
        // Each scenario starts with an empty data folder of its own, so
        // saves from one run never leak into the next.
        let user_data = root
            .join("build/test-userdata")
            .join(scenario.file_stem().unwrap_or_default());
        let _ = std::fs::remove_dir_all(&user_data);
        game.env(crate::project::USER_DATA_ENV, user_data);
        game.env(crate::scenario::TEST_SCENARIO_ENV, scenario)
            .env(crate::scenario::TEST_REPORT_ENV, &report_path);
        if options.update_golden {
            game.env(crate::scenario::UPDATE_GOLDEN_ENV, "1");
        }
        if options.keep_going {
            game.env(crate::scenario::KEEP_GOING_ENV, "1");
        }
        if !options.without.is_empty() {
            game.env(crate::project::WITHOUT_ENV, options.without.join(","));
        }
    }
    // A recording game closes itself at the timeout so it can save; the
    // kill comes later, for a game that hangs.
    let mut timeout = options.timeout;
    if let (Some(limit), Some(_)) = (timeout, &options.record) {
        game.env(
            crate::project::QUIT_AFTER_MS_ENV,
            limit.as_millis().to_string(),
        );
        timeout = Some(limit + Duration::from_secs(10));
    }
    let launched_after = start.elapsed();
    let run = match run_process_echoing(&mut game, timeout, options.echo_stderr)
    {
        Ok(run) => run,
        Err(error) => {
            return CliResult::failure(
                "GAME_FAILED",
                format!("could not start {}: {error}", executable.display()),
                Some(executable),
            )
        }
    };
    let first_frame = crate::project::first_frame_ms(&run.stderr);
    let mut data = json!({
        "root": root,
        "executable": executable,
        "headless_ticks": options.headless_ticks,
        "build": {"duration_ms": build_duration.as_millis() as u64},
        "game": run.data(),
        "timings": {
            "build_ms": build_duration.as_millis() as u64,
            "game_first_frame_ms": first_frame,
            // Includes loading the scene; a debug build shows here first.
            "headless_ms_per_tick": crate::project::tick_time_ms(&run.stderr),
            // Windowed frame times from `--bench`, after the warm-up.
            "bench": crate::project::bench_result(&run.stderr),
            "command_to_first_frame_ms": first_frame
                .map(|ms| launched_after.as_millis() as u64 + ms),
        },
    });
    // Game output can only hold asset reload failures, not build errors.
    let mut asset_warnings = reload_diagnostics(&root, &run.stderr);
    asset_warnings
        .retain(|diagnostic| diagnostic.code == "ASSET_RELOAD_FAILED");
    let mut result = 'result: {
        if let Some(scenario) = options.scenario {
            let text = std::fs::read_to_string(&report_path).ok();
            let _ = match text {
                Some(_) => std::fs::rename(&report_path, &last_report),
                None => std::fs::remove_file(&last_report),
            };
            let report = text.and_then(|text| {
                serde_json::from_str::<crate::scenario::ScenarioReport>(&text)
                    .ok()
            });
            let Some(report) = report else {
                let mut result = CliResult::failure(
                    "GAME_FAILED",
                    format!(
                        "game wrote no scenario report:\n{}",
                        tail(&run.stderr, 40)
                    ),
                    Some(scenario),
                );
                result.data = data;
                break 'result result;
            };
            data["scenario"] = json!(report);
            let Some(failure) = &report.first_failure else {
                break 'result CliResult::success(data);
            };
            // With `keep_going`, every failed step, not just the first.
            let message = report
                .steps
                .iter()
                .filter(|step| !step.ok)
                .map(|step| {
                    format!(
                        "tick {} step {}: {}",
                        step.tick, step.step, step.message
                    )
                })
                .collect::<Vec<_>>();
            let mut message = if message.is_empty() {
                format!(
                    "tick {} step {}: {}",
                    failure.tick, failure.step, failure.message
                )
            } else {
                message.join("\n")
            };
            if !run.stderr.trim().is_empty() {
                message += &format!(
                    "\ngame stderr (last 20 lines):\n{}",
                    tail(&run.stderr, 20)
                );
            }
            let mut result =
                CliResult::failure("SCENARIO_FAILED", message, Some(scenario));
            result.data = data;
            break 'result result;
        }
        if final_scene.is_file() && options.headless_ticks.is_some() {
            data["final_scene"] = json!(final_scene);
        }
        if run.success || run.timed_out {
            break 'result CliResult::success(data);
        }
        let mut result = CliResult::failure(
            "GAME_FAILED",
            format!(
                "game exited with {}:\n{}",
                run.code
                    .map_or("a signal".into(), |code| format!("code {code}")),
                tail(&run.stderr, 40)
            ),
            Some(executable),
        );
        result.data = data;
        result
    };
    result.diagnostics.extend(asset_warnings);
    result
}

/// Runs every `.json` scenario in `folder`, in name order, and fails when
/// any of them fails.
pub fn test_game_folder(
    root: &Path,
    folder: &Path,
    options: RunOptions,
) -> CliResult {
    let mut files: Vec<PathBuf> = match std::fs::read_dir(folder) {
        Ok(entries) => entries
            .filter_map(|entry| entry.ok().map(|entry| entry.path()))
            .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
            .collect(),
        Err(error) => {
            return CliResult::failure(
                "FILE_NOT_FOUND",
                format!("cannot read {}: {error}", folder.display()),
                Some(folder.to_path_buf()),
            )
        }
    };
    files.sort();
    if files.is_empty() {
        return CliResult::failure(
            "FILE_NOT_FOUND",
            format!("no scenario files in {}", folder.display()),
            Some(folder.to_path_buf()),
        );
    }
    let mut runs = Vec::new();
    let mut first_failure = None;
    for file in files {
        let result = run_game_project(
            root,
            RunOptions {
                scenario: Some(file.clone()),
                ..options.clone()
            },
        );
        let message = match result.diagnostics.first() {
            Some(diagnostic) if !result.ok => diagnostic.message.clone(),
            _ => format!(
                "{} after {} ticks",
                result.data["scenario"]["name"],
                result.data["scenario"]["ticks_run"]
            ),
        };
        let logs: Vec<_> = result.data["scenario"]["steps"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|step| {
                step["message"].as_str().is_some_and(|message| {
                    message.starts_with("log: ") || message.contains("; warning: ")
                })
            })
            .map(|step| json!({"tick": step["tick"], "message": step["message"]}))
            .collect();
        let perf = &result.data["scenario"]["perf"];
        runs.push(json!({
            "file": file,
            "ok": result.ok,
            "message": message,
            "logs": logs,
            "perf": {
                "tick_ms_mean": perf["tick_ms_mean"],
                "tick_ms_p95": perf["tick_ms_p95"],
                "tick_ms_max": perf["tick_ms_max"],
                "draws": perf["render"]["draws"],
                "triangles": perf["render"]["triangles"],
            },
        }));
        if !result.ok && first_failure.is_none() {
            first_failure = Some(result);
        }
    }
    let data = json!({"root": root, "scenarios": runs});
    // The editor's Agent panel shows the newest run from this file; a
    // `--without` run inverts pass and fail, so it is not saved there.
    if options.without.is_empty() {
        let _ = std::fs::write(
            root.join(crate::project::TEST_RESULTS_FILE),
            serde_json::to_vec_pretty(&data).unwrap_or_default(),
        );
    }
    match first_failure {
        None => CliResult::success(data),
        Some(mut result) => {
            result.data = data;
            result
        }
    }
}

/// One way to build and run the game for [`check_game_determinism`].
struct DeterminismConfig {
    name: &'static str,
    release: bool,
    /// Pin the game to one CPU with `taskset`, so bevy and rayon start one
    /// worker thread instead of one per core.
    one_cpu: bool,
}

/// Builds the game in each configuration, runs it headless for `ticks`
/// ticks, and compares every tick's state hash with the first
/// configuration's: debug, release, and release on one CPU when `taskset`
/// is available. At the first divergence both runs repeat up to that tick
/// to name the first divergent entity. GPU-class bodies do not simulate
/// headless, so vendor and driver differences are not covered.
pub fn check_game_determinism(root: &Path, ticks: u32) -> CliResult {
    let cooked = cook_project(root);
    if !cooked.ok {
        return cooked;
    }
    let project = match open_project(root) {
        Ok(project) => project,
        Err(error) => return project_error(error, root),
    };
    let mut configs = vec![
        DeterminismConfig {
            name: "debug",
            release: false,
            one_cpu: false,
        },
        DeterminismConfig {
            name: "release",
            release: true,
            one_cpu: false,
        },
    ];
    if tool_available("taskset") {
        configs.push(DeterminismConfig {
            name: "release-one-cpu",
            release: true,
            one_cpu: true,
        });
    }
    let out_dir = root.join("build/determinism");
    if let Err(error) = std::fs::create_dir_all(&out_dir) {
        return CliResult::failure(
            "IO_ERROR",
            error.to_string(),
            Some(out_dir),
        );
    }
    let run = |config: &DeterminismConfig, ticks: u32| {
        headless_state_hashes(&project, config, ticks, &out_dir)
    };
    let mut reports = Vec::new();
    for config in &configs {
        if let Err(result) = cargo(&project, "build", config.release, None, &[])
        {
            return result;
        }
        match run(config, ticks) {
            Ok(report) => reports.push(report),
            Err(result) => return result,
        }
    }
    let mut data = json!({
        "root": root,
        "ticks": ticks,
        "configurations": configs
            .iter()
            .zip(&reports)
            .map(|(config, report)| json!({
                "name": config.name,
                "final_hash": report.ticks.last().map(|tick| tick.1),
            }))
            .collect::<Vec<_>>(),
    });
    for (config, report) in configs.iter().zip(&reports).skip(1) {
        let Some(tick) = crate::runtime::first_divergent_tick(
            &reports[0].ticks,
            &report.ticks,
        ) else {
            continue;
        };
        // Repeat both runs up to the divergent tick for entity hashes there.
        let tick_count = u32::try_from(tick).unwrap_or(ticks);
        let entity =
            match (run(&configs[0], tick_count), run(config, tick_count)) {
                (Ok(first), Ok(second)) => {
                    crate::runtime::first_divergent_entity(
                        &first.entities,
                        &second.entities,
                    )
                }
                (Err(result), _) | (_, Err(result)) => return result,
            };
        data["divergence"] = json!({
            "configurations": [configs[0].name, config.name],
            "tick": tick,
            "entity": entity,
        });
        let mut result = CliResult::failure(
            "DETERMINISM_DIVERGED",
            format!(
                "{} and {} diverge at tick {tick}{}",
                configs[0].name,
                config.name,
                entity
                    .and_then(|entity| entity.name)
                    .map(|name| format!(", first in `{name}`"))
                    .unwrap_or_default()
            ),
            None,
        );
        result.data = data;
        return result;
    }
    CliResult::success(data)
}

/// Runs a built game headless for `ticks` ticks and reads the state hash
/// of every tick, plus each entity's hash at the end.
fn headless_state_hashes(
    project: &OpenProject,
    config: &DeterminismConfig,
    ticks: u32,
    out_dir: &Path,
) -> Result<crate::runtime::StateHashReport, CliResult> {
    let executable = crate::project::built_executable(
        &project.root,
        &project.root.join("Cargo.toml"),
        &project.manifest.binary_name,
        None,
        config.release,
    )
    .map_err(|error| CliResult::failure("BUILD_FAILED", error, None))?;
    // Absolute, since the game runs in the project root: a relative root
    // would be joined twice.
    let report_path = std::path::absolute(
        out_dir.join(format!("{}-{ticks}.json", config.name)),
    )
    .map_err(|error| {
        CliResult::failure("IO_ERROR", error.to_string(), Some(out_dir.into()))
    })?;
    let _ = std::fs::remove_file(&report_path);
    let mut game = if config.one_cpu {
        let mut game = Command::new("taskset");
        game.args(["--cpu-list", "0"]).arg(&executable);
        game
    } else {
        Command::new(&executable)
    };
    game.current_dir(&project.root)
        .env(crate::project::HEADLESS_TICKS_ENV, ticks.to_string())
        .env(crate::project::STATE_HASH_OUT_ENV, &report_path);
    let output = run_process(&mut game, None).map_err(|error| {
        CliResult::failure(
            "GAME_FAILED",
            format!("could not start {}: {error}", executable.display()),
            Some(executable.clone()),
        )
    })?;
    std::fs::read_to_string(&report_path)
        .ok()
        .and_then(|text| {
            serde_json::from_str::<crate::runtime::StateHashReport>(&text).ok()
        })
        .ok_or_else(|| {
            CliResult::failure(
                "GAME_FAILED",
                format!(
                    "{} run wrote no state hashes:\n{}",
                    config.name,
                    tail(&output.stderr, 40)
                ),
                Some(executable),
            )
        })
}

/// Runs two copies of a game (two builds, or two revisions of its scenes,
/// such as two git worktrees) headless for `ticks` ticks and reports the
/// first tick whose state hash differs and the first entity that differs
/// there, matched by scene ID.
pub fn bisect_game_projects(
    first: &Path,
    second: &Path,
    ticks: u32,
) -> CliResult {
    let config = DeterminismConfig {
        name: "bisect",
        release: false,
        one_cpu: false,
    };
    let mut projects = Vec::new();
    for root in [first, second] {
        let cooked = cook_project(root);
        if !cooked.ok {
            return cooked;
        }
        let project = match open_project(root) {
            Ok(project) => project,
            Err(error) => return project_error(error, root),
        };
        if let Err(result) = cargo(&project, "build", false, None, &[]) {
            return result;
        }
        let out_dir = project.root.join("build/bisect");
        if let Err(error) = std::fs::create_dir_all(&out_dir) {
            return CliResult::failure(
                "IO_ERROR",
                error.to_string(),
                Some(out_dir),
            );
        }
        projects.push((project, out_dir));
    }
    let run = |index: usize, ticks: u32| {
        let (project, out_dir) = &projects[index];
        headless_state_hashes(project, &config, ticks, out_dir)
    };
    let (a, b) = match (run(0, ticks), run(1, ticks)) {
        (Ok(a), Ok(b)) => (a, b),
        (Err(result), _) | (_, Err(result)) => return result,
    };
    let mut data = json!({
        "roots": [projects[0].0.root, projects[1].0.root],
        "ticks": ticks,
        "final_hashes": [a.ticks.last().map(|t| t.1), b.ticks.last().map(|t| t.1)],
    });
    let Some(tick) = crate::runtime::first_divergent_tick(&a.ticks, &b.ticks)
    else {
        data["verdict"] = json!(format!("identical on all {ticks} ticks"));
        return CliResult::success(data);
    };
    // Repeat both runs up to the divergent tick for entity hashes there.
    let tick_count = u32::try_from(tick).unwrap_or(ticks);
    let entity = match (run(0, tick_count), run(1, tick_count)) {
        (Ok(a), Ok(b)) => {
            crate::runtime::first_divergent_entity(&a.entities, &b.entities)
        }
        (Err(result), _) | (_, Err(result)) => return result,
    };
    data["divergence"] = json!({"tick": tick, "entity": entity});
    let mut result = CliResult::failure(
        "DETERMINISM_DIVERGED",
        format!(
            "the two runs diverge at tick {tick}{}; compare that tick's \
             state with `rusting run <root> --ticks {tick}` in each and \
             `rusting diff` on the two build/final.rscene files",
            entity
                .and_then(|entity| entity.name)
                .map(|name| format!(", first in `{name}`"))
                .unwrap_or_default()
        ),
        None,
    );
    result.data = data;
    result
}

/// Runs a scenario in a debug and then a release build and compares the
/// CPU state hash of every tick, plus the GPU body hash of every tick both
/// runs delivered. `gpu` requires GPU hashes. Same machine and driver only:
/// it finds unseeded randomness and order-dependent code, not vendor
/// differences.
pub fn check_scenario_determinism(
    root: &Path,
    scenario: &Path,
    gpu: bool,
) -> CliResult {
    let mut runs = Vec::new();
    let mut scenario_passed = true;
    for (name, release) in [("debug", false), ("release", true)] {
        eprintln!("determinism: building and running the {name} build...");
        let result = run_game_project(
            root,
            RunOptions {
                release,
                scenario: Some(scenario.to_path_buf()),
                keep_going: true,
                ..RunOptions::default()
            },
        );
        // A failed expect step still leaves hashes worth comparing: a stale
        // pinned hash is exactly what this check helps to re-pin.
        let hashes = |key: &str| -> BTreeMap<u64, u64> {
            serde_json::from_value::<Vec<(u64, u64)>>(
                result.data["scenario"][key].clone(),
            )
            .unwrap_or_default()
            .into_iter()
            .collect()
        };
        let (cpu, gpu_hashes) =
            (hashes("state_hashes"), hashes("gpu_state_hashes"));
        if cpu.is_empty() && gpu_hashes.is_empty() && !result.ok {
            return result;
        }
        scenario_passed &= result.ok;
        if gpu && gpu_hashes.is_empty() {
            return CliResult::failure(
                "DETERMINISM_NO_GPU_STATE",
                "the scenario recorded no GPU state hashes; set \"gpu\": true in it and give it GPU physics bodies",
                Some(scenario.to_path_buf()),
            );
        }
        runs.push((cpu, gpu_hashes));
    }
    let compare = |first: &BTreeMap<u64, u64>, second: &BTreeMap<u64, u64>| {
        first
            .iter()
            .filter_map(|(tick, hash)| {
                second.get(tick).map(|other| (*tick, *hash, *other))
            })
            .collect::<Vec<_>>()
    };
    let cpu = compare(&runs[0].0, &runs[1].0);
    let gpu_compared = compare(&runs[0].1, &runs[1].1);
    let mut data = json!({
        "root": root,
        "scenario": scenario,
        "ticks_compared": cpu.len(),
        "final_hash": cpu.last().map(|entry| entry.1),
        "gpu_ticks_compared": gpu_compared.len(),
        "gpu_final_hash": gpu_compared.last().map(|entry| entry.1),
        "scenario_passed": scenario_passed,
    });
    let diverged = |hashes: &[(u64, u64, u64)]| {
        hashes
            .iter()
            .find(|(_, first, second)| first != second)
            .map(|entry| entry.0)
    };
    let divergence = diverged(&cpu)
        .map(|tick| (tick, "CPU world state"))
        .or_else(|| diverged(&gpu_compared).map(|tick| (tick, "GPU bodies")));
    match divergence {
        None => {
            let run = runs[0].0.keys().last().copied().unwrap_or_default();
            data["ticks_run"] = json!(run);
            data["verdict"] = json!(format!(
                "deterministic: debug == release on all {run} ticks (the hash at each tick covers the whole state){}{}",
                cpu.last()
                    .map(|entry| format!(", final hash {:#018x}", entry.1))
                    .unwrap_or_default(),
                if scenario_passed { "" } else { "; the scenario itself failed" }
            ));
            CliResult::success(data)
        }
        Some((tick, state)) => {
            data["divergence"] = json!({"tick": tick, "state": state});
            let mut result = CliResult::failure(
                "DETERMINISM_DIVERGED",
                format!(
                    "the debug and release runs of the scenario diverge at tick {tick} ({state})"
                ),
                Some(scenario.to_path_buf()),
            );
            result.data = data;
            result
        }
    }
}

/// Ticks the exported game runs headless to verify it.
const EXPORT_VERIFY_TICKS: u32 = 60;

/// Cooks, builds a release, and packages the game into a new folder under
/// `parent`, the same way the editor's Export does. A native export is then
/// verified: a copy of the folder in a fresh temporary location must run
/// `EXPORT_VERIFY_TICKS` headless ticks and exit cleanly. Cross-target
/// exports are not run.
pub fn export_game_project(
    root: &Path,
    parent: &Path,
    target: Option<&str>,
    preset: Option<&str>,
) -> CliResult {
    let cooked = cook_project(root);
    if !cooked.ok {
        return cooked;
    }
    let project = match open_project(root) {
        Ok(project) => project,
        Err(error) => return project_error(error, root),
    };
    let preset = match preset {
        None => crate::project::ExportPreset {
            target: target.map(str::to_owned),
            ..Default::default()
        },
        Some(name) => {
            let Some(preset) = project.manifest.export_presets.get(name) else {
                let known: Vec<&str> = project
                    .manifest
                    .export_presets
                    .keys()
                    .map(String::as_str)
                    .collect();
                return CliResult::failure(
                    "EXPORT_PRESET_UNKNOWN",
                    format!(
                        "project.json has no export preset `{name}`; it has: {}",
                        if known.is_empty() {
                            "none".to_owned()
                        } else {
                            known.join(", ")
                        }
                    ),
                    Some(project.root.join("project.json")),
                );
            };
            preset.clone()
        }
    };
    let target = preset.target.as_deref();
    if let Err(result) =
        cargo(&project, "build", true, target, &preset.features)
    {
        return result;
    }
    let exported = match crate::project::export_built_game(
        &project.root,
        &project.root.join("Cargo.toml"),
        parent,
        &project.manifest.name,
        &project.manifest.binary_name,
        &project.manifest.cooked_scene,
        target,
    ) {
        Ok(exported) => exported,
        Err(error) => {
            return CliResult::failure(
                "EXPORT_FAILED",
                error,
                Some(parent.to_owned()),
            )
        }
    };
    let excluded =
        match crate::project::remove_excluded(&exported, &preset.exclude) {
            Ok(excluded) => excluded,
            Err(error) => {
                return CliResult::failure(
                    "EXPORT_FAILED",
                    format!("could not leave out excluded assets: {error}"),
                    Some(exported),
                )
            }
        };
    let packed = if preset.pack {
        match crate::project::pack_export(&exported) {
            Ok(packed) => packed,
            Err(error) => {
                return CliResult::failure(
                    "EXPORT_FAILED",
                    format!("could not pack the exported assets: {error}"),
                    Some(exported),
                )
            }
        }
    } else {
        Vec::new()
    };
    if let Some((program, arguments)) = preset.sign.split_first() {
        let executable = exported.join(crate::project::executable_name(
            &project.manifest.binary_name,
            target,
        ));
        let mut sign = Command::new(program);
        sign.current_dir(&project.root);
        for argument in arguments {
            sign.arg(argument.replace("{exe}", &executable.to_string_lossy()));
        }
        let message =
            match run_process(&mut sign, Some(Duration::from_secs(600))) {
                Ok(run) if run.success => None,
                Ok(run) => Some(format!(
                    "the sign command failed:\n{}",
                    tail(&run.stderr, 40)
                )),
                Err(error) => Some(format!("could not run {program}: {error}")),
            };
        if let Some(message) = message {
            // An unsigned folder must not pass for a finished export.
            let _ = std::fs::remove_dir_all(&exported);
            return CliResult::failure(
                "EXPORT_SIGN_FAILED",
                message,
                Some(project.root.join("project.json")),
            );
        }
    }
    let mut files: Vec<_> = walk_files(&exported)
        .into_iter()
        .filter_map(|file| {
            file.strip_prefix(&exported).ok().map(Path::to_path_buf)
        })
        .collect();
    files.sort();
    let mut data = json!({
        "root": project.root,
        "export_path": exported,
        "target": target,
        "features": preset.features,
        "excluded": excluded,
        "packed": packed,
        "signed": !preset.sign.is_empty(),
        "files": files,
        "verified": null,
        "zip": null,
    });
    let mut result = if target.is_some() {
        let mut result = CliResult::success(data);
        result.diagnostics.push(Diagnostic {
            code: "EXPORT_NOT_VERIFIED",
            severity: "warning",
            message: "cross-target exports are not run on this machine".into(),
            file: Some(exported.clone()),
            ..Diagnostic::default()
        });
        result
    } else {
        match verify_export(&exported, &project.manifest.binary_name) {
            Ok(run) if run.success => {
                data["verified"] = run.data();
                CliResult::success(data)
            }
            verified => {
                return export_verify_failure(&verified, data, exported)
            }
        }
    };
    if preset.zip {
        let mut zip = exported.clone().into_os_string();
        zip.push(".zip");
        let zip = PathBuf::from(zip);
        if let Err(error) = crate::project::zip_folder(&exported, &zip) {
            return CliResult::failure(
                "EXPORT_FAILED",
                format!("could not zip the export: {error}"),
                Some(exported),
            );
        }
        result.data["zip"] = json!(zip);
    }
    result
}

fn export_verify_failure(
    verified: &Result<ProcessRun, String>,
    mut data: Value,
    exported: PathBuf,
) -> CliResult {
    data["verified"] = match verified {
        Ok(run) => run.data(),
        Err(error) => json!({"error": error}),
    };
    let message = match &verified {
        Ok(run) => format!(
            "exported game failed its headless run:\n{}",
            tail(&run.stderr, 40)
        ),
        Err(error) => format!("could not verify the export: {error}"),
    };
    let mut result =
        CliResult::failure("EXPORT_VERIFY_FAILED", message, Some(exported));
    result.data = data;
    result
}

/// Runs a copy of the export from a fresh temporary folder, so it cannot
/// read files through the export's own path.
fn verify_export(
    exported: &Path,
    binary_name: &str,
) -> Result<ProcessRun, String> {
    let isolated = std::env::temp_dir()
        .join(format!("rusting-export-verify-{}", Uuid::new_v4()));
    let result =
        crate::project::copy_directory(exported, &isolated).and_then(|()| {
            let executable = isolated
                .join(crate::project::executable_name(binary_name, None));
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(
                    &executable,
                    std::fs::Permissions::from_mode(0o755),
                )
                .map_err(|error| error.to_string())?;
            }
            let mut game = Command::new(&executable);
            game.current_dir(&isolated)
                .env(
                    crate::project::HEADLESS_TICKS_ENV,
                    EXPORT_VERIFY_TICKS.to_string(),
                )
                .env_remove("RUSTING_SCENE_PATH");
            run_process(&mut game, Some(Duration::from_secs(120)))
                .map_err(|error| error.to_string())
        });
    let _ = std::fs::remove_dir_all(&isolated);
    result
}

fn walk_files(folder: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(folder) else {
        return Vec::new();
    };
    entries
        .flatten()
        .flat_map(|entry| {
            let path = entry.path();
            if path.is_dir() {
                walk_files(&path)
            } else {
                vec![path]
            }
        })
        .collect()
}

/// What `rusting capture` renders.
#[derive(Clone, Debug)]
pub struct CaptureOptions {
    /// Camera by persistent ID or name; `None` uses the scene's active
    /// camera, as the game would.
    pub camera: Option<String>,
    /// Fixed ticks to simulate before the captured frame.
    pub tick: u32,
    pub extent: [u32; 2],
    /// PNG destination. Not written when Vulkan is unavailable.
    pub output: PathBuf,
    /// Pixels, from the top-left corner, to map to scene objects.
    pub picks: Vec<[u32; 2]>,
    /// Rectangles `[x, y, width, height]` whose covering objects are listed.
    pub pick_rects: Vec<[u32; 4]>,
    /// False leaves the HUD and other runtime UI out (`--no-hud`).
    pub hud: bool,
    /// Renders from a camera placed here (`--at X,Y,Z`) instead of a scene
    /// camera; its lens comes from `camera` or the scene's active camera.
    pub at: Option<[f32; 3]>,
    /// Point the `at` camera faces (`--look-at X,Y,Z`).
    pub look_at: Option<[f32; 3]>,
    /// Yaw and pitch in radians for the `at` camera (`--look YAW,PITCH`);
    /// zero faces -Z and positive pitch looks up.
    pub look: Option<[f32; 2]>,
}

/// `rusting capture --game`: builds the project that holds `path` and
/// captures its game, game code included, at `options.tick` through a
/// one-step scenario. Picks and placed cameras are not supported here.
pub fn capture_game(path: &Path, options: &CaptureOptions) -> CliResult {
    let absolute = std::path::absolute(path).unwrap_or_else(|_| path.into());
    let Some(root) = absolute
        .ancestors()
        .find(|dir| dir.join("project.json").is_file())
    else {
        return CliResult::failure(
            "PROJECT_MISSING_FILE",
            "--game needs a path inside a project (a folder with project.json)",
            Some(path.into()),
        );
    };
    let output = std::path::absolute(&options.output)
        .unwrap_or_else(|_| options.output.clone());
    let tick = options.tick.max(1);
    let scenario = json!({
        "name": format!("capture tick {tick}"),
        "ticks": tick,
        "capture_size": options.extent,
        "steps": [{"tick": tick, "capture": {
            "path": output, "camera": options.camera, "hud": options.hud,
        }}],
    });
    let file = root.join("build/capture-game.json");
    let written = std::fs::create_dir_all(root.join("build"))
        .and_then(|()| std::fs::write(&file, scenario.to_string()));
    if let Err(error) = written {
        return CliResult::failure("IO_ERROR", error.to_string(), Some(file));
    }
    let mut result = run_game_project(
        root,
        RunOptions {
            scenario: Some(file),
            ..RunOptions::default()
        },
    );
    if result.ok {
        result.data = json!({"output": output, "tick": tick,
            "size": options.extent, "game": true});
    }
    result
}

/// Loads a scene, simulates `tick` fixed ticks, and renders one camera
/// offscreen to a PNG. Only the last few ticks are rendered, unless the
/// scene has GPU bodies: those advance only while frames render, so then
/// every tick is, as in the game. Game code from the project is not run. Picks and camera data
/// come from the CPU, so they are reported, with a `VULKAN_UNAVAILABLE`
/// error, even when no Vulkan device exists.
pub fn capture_scene(scene: &Path, options: &CaptureOptions) -> CliResult {
    use crate::rendering::capture::HeadlessCapture;
    use crate::runtime::{
        load_scene, HybridPhysicsPlugin, RenderCameraOverride,
        RenderExtractPlugin, SceneLoadMode,
    };
    use crate::{App, AssetPlugin};

    let [width, height] = options.extent;
    if width == 0 || height == 0 {
        return CliResult::failure(
            "CLI_USAGE",
            "capture size must be at least 1x1",
            None,
        );
    }
    if let Some(&[x, y]) = options
        .picks
        .iter()
        .find(|[x, y]| *x >= width || *y >= height)
    {
        return CliResult::failure(
            "CLI_USAGE",
            format!("pick {x},{y} is outside the {width}x{height} capture"),
            None,
        );
    }
    let document = match read_scene(scene) {
        Ok(document) => document,
        Err(result) => return result,
    };
    let warnings = scene_warnings(scene, &document);

    let mut app = App::new();
    if let Err(error) = app
        .add_plugin(AssetPlugin)
        .and_then(|app| app.add_plugin(HybridPhysicsPlugin))
        .and_then(|app| app.add_plugin(RenderExtractPlugin))
    {
        return CliResult::failure("CAPTURE_FAILED", error.to_string(), None);
    }
    // HUD fonts and other files under `assets/` resolve from the project.
    #[cfg(feature = "window")]
    if let Some(root) = std::path::absolute(scene)
        .unwrap_or_else(|_| scene.into())
        .ancestors()
        .find(|dir| dir.join("project.json").is_file())
    {
        app.world_mut()
            .insert_resource(crate::project_runner::ProjectFolder(
                root.to_owned(),
            ));
    }
    // Game code does not run during a capture, so its components are kept
    // as data rather than rejected.
    app.world_mut()
        .resource_mut::<crate::runtime::SceneComponentRegistry>()
        .keep_unregistered();
    if let Err(error) =
        load_scene(app.world_mut(), scene, SceneLoadMode::Replace)
    {
        return scene_error(error, scene);
    }
    if let Some(wanted) = &options.camera {
        let Some(camera) =
            crate::scenario::find_camera(app.world_mut(), wanted)
        else {
            return CliResult::failure(
                "CAMERA_NOT_FOUND",
                format!("no camera has the ID or name `{wanted}`"),
                Some(scene.to_owned()),
            );
        };
        app.world_mut()
            .resource_mut::<RenderCameraOverride>()
            .entity = Some(camera);
    }
    if let Some(at) = options.at {
        let world = app.world_mut();
        let lens = match world.resource::<RenderCameraOverride>().entity {
            Some(entity) => {
                world.get::<crate::runtime::Camera>(entity).copied()
            }
            None => world
                .query::<&crate::runtime::Camera>()
                .iter(world)
                .filter(|camera| camera.active)
                .max_by_key(|camera| camera.priority)
                .copied(),
        };
        let [yaw, pitch] = match options.look_at {
            Some(target) => {
                let d =
                    [target[0] - at[0], target[1] - at[1], target[2] - at[2]];
                [(-d[0]).atan2(-d[2]), d[1].atan2(d[0].hypot(d[2]))]
            }
            None => options.look.unwrap_or_default(),
        };
        let camera = world
            .spawn((
                crate::runtime::Name("Capture Camera".into()),
                crate::Transform {
                    position: at,
                    rotation: [pitch, yaw, 0.0],
                    scale: [1.0; 3],
                },
                crate::runtime::Camera {
                    active: true,
                    viewport: None,
                    ..lens.unwrap_or_default()
                },
            ))
            .id();
        world.resource_mut::<RenderCameraOverride>().entity = Some(camera);
    }

    let mut gpu_error = None;
    let mut capture = match HeadlessCapture::new(options.extent) {
        Ok(capture) => Some(capture),
        Err(error) => {
            gpu_error = Some(("VULKAN_UNAVAILABLE", error));
            None
        }
    };
    for tick in 0..=options.tick {
        let delta = crate::scenario::tick_delta(&app, tick);
        let render =
            tick + crate::scenario::RENDER_WARMUP_TICKS >= options.tick;
        let stepped = match capture.as_mut() {
            Some(capture) if render => capture.frame(&mut app, delta),
            Some(capture)
                if crate::scenario::has_gpu_bodies(app.world_mut()) =>
            {
                capture.step_physics(&mut app, delta)
            }
            Some(capture) => capture.update(&mut app, delta),
            None => app
                .update(delta)
                .map(drop)
                .map_err(|error| error.to_string()),
        };
        if let Err(error) = stepped {
            return CliResult::failure(
                "CAPTURE_FAILED",
                format!("tick {tick}: {error}"),
                None,
            );
        }
    }
    #[cfg(feature = "ui")]
    let mut warnings = warnings;
    #[cfg(feature = "ui")]
    if let Some(capture) = capture.as_mut().filter(|_| options.hud) {
        if capture.texts().iter().any(|spot| !spot.backed) {
            match capture.view_rgba(app.world(), None) {
                Ok(background) => warnings.extend(text_contrast_warnings(
                    capture.texts(),
                    &background,
                    options.extent,
                )),
                Err(error) => {
                    return CliResult::failure("CAPTURE_FAILED", error, None)
                }
            }
        }
    }
    let rgba = match capture.as_mut() {
        Some(capture) if !options.hud => {
            match capture.view_rgba(app.world(), None) {
                Ok(rgba) => Some(rgba),
                Err(error) => {
                    return CliResult::failure("CAPTURE_FAILED", error, None)
                }
            }
        }
        capture => capture.map(|capture| capture.rgba()),
    };
    let render = capture
        .as_mut()
        .map_or(Value::Null, |capture| capture.metadata(&app));
    let Some(view) =
        crate::scenario::CameraView::active(app.world(), options.extent)
    else {
        return CliResult::failure(
            "CAMERA_NOT_FOUND",
            "the scene has no active camera; pass --camera",
            Some(scene.to_owned()),
        );
    };
    let picks: Vec<_> = options
        .picks
        .iter()
        .map(|&pixel| {
            let mut pick = view.pick(app.world_mut(), pixel);
            pick["color"] = json!(rgba.as_ref().map(|rgba| {
                let at = ((pixel[1] * width + pixel[0]) * 4) as usize;
                [rgba[at], rgba[at + 1], rgba[at + 2], rgba[at + 3]]
            }));
            pick
        })
        .collect();
    let pick_rects: Vec<_> = options
        .pick_rects
        .iter()
        .map(|&[x, y, w, h]| {
            view.pick_rect(app.world_mut(), [x, y, x + w, y + h])
        })
        .collect();
    let mut data = json!({
        "scene": scene,
        "tick": options.tick,
        "extent": options.extent,
        "output": Value::Null,
        "camera": view.data(app.world()),
        "render": render,
        "pick_method": "mesh_bounds",
        "picks": picks,
        "pick_rects": pick_rects,
    });

    let mut result = match (gpu_error, capture) {
        (Some((code, error)), _) => {
            let mut result = CliResult::failure(code, error, None);
            result.data = data;
            result
        }
        (None, Some(_)) => {
            let saved = crate::rendering::capture::save_rgba(
                &options.output,
                rgba.as_deref().unwrap_or_default(),
                options.extent,
            );
            if let Err(error) = saved {
                return CliResult::failure(
                    "CAPTURE_FAILED",
                    error,
                    Some(options.output.clone()),
                );
            }
            data["output"] = json!(options.output);
            CliResult::success(data)
        }
        (None, None) => unreachable!("a capture exists without a GPU error"),
    };
    result.diagnostics.extend(warnings);
    result
}

#[cfg(test)]
mod shape_tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn an_unknown_export_preset_fails_and_names_the_known_ones() {
        let parent = std::env::temp_dir()
            .join(format!("rusting-preset-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&parent).unwrap();
        assert!(new_project(&parent, "Pre", ProjectTemplate::Basic3d).ok);
        let root = parent.join("Pre");
        let manifest = root.join("project.json");
        let mut project: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&manifest).unwrap()).unwrap();
        project["export_presets"] = json!({"linux": {"exclude": ["**/*.psd"]}});
        std::fs::write(&manifest, project.to_string()).unwrap();
        let result = export_game_project(&root, &parent, None, Some("windows"));
        assert!(!result.ok);
        assert_eq!(result.diagnostics[0].code, "EXPORT_PRESET_UNKNOWN");
        assert!(
            result.diagnostics[0].message.ends_with("it has: linux"),
            "{}",
            result.diagnostics[0].message
        );
        let _ = std::fs::remove_dir_all(parent);
    }

    #[test]
    #[cfg(unix)]
    fn echoed_stderr_is_still_kept_whole() {
        let mut command = Command::new("sh");
        command.args(["-c", "printf 'one\\ntwo' >&2; echo out"]);
        let run = run_process_echoing(&mut command, None, true).unwrap();
        assert_eq!(run.stderr, "one\ntwo");
        assert_eq!(run.stdout, "out\n");
        assert!(run.success);
    }

    #[test]
    #[cfg(unix)]
    fn exe_runs_a_scenario_in_a_built_game_from_its_own_folder() {
        use std::os::unix::fs::PermissionsExt;
        let root = std::env::temp_dir()
            .join(format!("rusting-test-exe-{}", std::process::id()));
        let export = root.join("export");
        std::fs::create_dir_all(&export).unwrap();
        let scenario = root.join("smoke.json");
        std::fs::write(&scenario, "{}").unwrap();
        // Stands in for an exported game: it writes a passing report and
        // records the folder it ran in.
        let game = export.join("game");
        std::fs::write(
            &game,
            "#!/bin/sh\npwd > ran_in\nprintf '{\"name\":\"smoke\",\
             \"seed\":0,\"passed\":true,\"ticks_run\":3,\
             \"first_failure\":null,\"steps\":[],\"captures\":[]}' \
             > \"$RUSTING_TEST_REPORT\"\n",
        )
        .unwrap();
        std::fs::set_permissions(&game, std::fs::Permissions::from_mode(0o755))
            .unwrap();
        let options = RunOptions {
            scenario: Some(scenario),
            exe: Some(game),
            ..RunOptions::default()
        };
        let result = run_game_project(&root, options);
        assert!(result.ok, "{:?}", result.diagnostics);
        assert_eq!(result.data["scenario"]["ticks_run"], 3);
        let ran_in = std::fs::read_to_string(export.join("ran_in")).unwrap();
        assert_eq!(
            Path::new(ran_in.trim()).canonicalize().unwrap(),
            export.canonicalize().unwrap()
        );
        // No project here: nothing was cooked or built.
        assert!(!root.join("target").exists());
        let missing = run_game_project(
            &root,
            RunOptions {
                scenario: Some(root.join("smoke.json")),
                exe: Some(root.join("missing")),
                ..RunOptions::default()
            },
        );
        assert_eq!(missing.diagnostics[0].code, "FILE_NOT_FOUND");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    #[cfg(unix)]
    fn two_runs_at_once_each_read_their_own_scenario_report() {
        use std::os::unix::fs::PermissionsExt;
        let root = std::env::temp_dir()
            .join(format!("rusting-test-reports-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        // `first` writes its report, then outlives `second`, which writes
        // its own in between.
        let game = root.join("game");
        std::fs::write(
            &game,
            "#!/bin/sh
name=$(cat \"$RUSTING_TEST_SCENARIO\")
             report() { printf '{\"name\":\"%s\",\"seed\":0,             \"passed\":true,\"ticks_run\":3,\"first_failure\":null,             \"steps\":[],\"captures\":[]}' \"$name\"              > \"$RUSTING_TEST_REPORT\"; }
             if [ \"$name\" = first ]; then report; sleep 1;              else sleep 0.3; report; fi
",
        )
        .unwrap();
        std::fs::set_permissions(&game, std::fs::Permissions::from_mode(0o755))
            .unwrap();
        let runs: Vec<_> = ["first", "second"]
            .into_iter()
            .map(|name| {
                let scenario = root.join(format!("{name}.json"));
                std::fs::write(&scenario, name).unwrap();
                let (root, game) = (root.clone(), game.clone());
                std::thread::spawn(move || {
                    let options = RunOptions {
                        scenario: Some(scenario),
                        exe: Some(game),
                        ..RunOptions::default()
                    };
                    run_game_project(&root, options)
                })
            })
            .collect();
        for (run, name) in runs.into_iter().zip(["first", "second"]) {
            let result = run.join().unwrap();
            assert!(result.ok, "{:?}", result.diagnostics);
            assert_eq!(result.data["scenario"]["name"], name);
        }
        // The last run to finish leaves its report for `--full` readers.
        let last =
            std::fs::read_to_string(root.join("build/scenario-report.json"))
                .unwrap();
        assert!(last.contains("\"first\""), "{last}");
        let left = std::fs::read_dir(root.join("build"))
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect::<Vec<_>>();
        assert_eq!(left, ["scenario-report.json"]);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    #[cfg(feature = "gltf")]
    fn a_gltf_model_lands_in_a_scene_under_one_named_object() {
        let root = std::env::temp_dir()
            .join(format!("rusting-add-model-{}", std::process::id()));
        let models = root.join("assets/models");
        std::fs::create_dir_all(&models).unwrap();
        let model = models.join("environment.gltf");
        std::fs::copy("samples/vertical_slice/environment.gltf", &model)
            .unwrap();
        let scene = root.join("scenes/main.rscene");
        std::fs::create_dir_all(scene.parent().unwrap()).unwrap();
        std::fs::write(
            &scene,
            r#"{"version": 9, "name": "Main", "entities": []}"#,
        )
        .unwrap();
        let result = add_model(&scene, &model, "Courtyard", false);
        assert!(result.ok, "{:?}", result.diagnostics);
        let document = read_scene_document(&scene).unwrap();
        let top: Vec<_> = document
            .entities
            .iter()
            .filter(|entity| entity.parent.is_none())
            .collect();
        assert_eq!(top.len(), 1);
        assert_eq!(top[0].name.as_deref(), Some("Courtyard"));
        let mesh = document
            .entities
            .iter()
            .find_map(|entity| entity.mesh_renderer.as_ref())
            .unwrap();
        // Paths are relative to the scene, like a hand-written patch.
        assert!(
            matches!(&mesh.mesh, crate::runtime::SceneMesh::AssetPath(path)
            if path.starts_with("../assets/models"))
        );
        // A second copy and a root named like a node both get suffixes.
        let copy = add_model(&scene, &model, "Courtyard 2", false);
        assert!(copy.ok, "{:?}", copy.diagnostics);
        let again = add_model(&scene, &model, "Courtyard", false);
        assert!(!again.ok);
        assert!(
            again.diagnostics[0].message.contains("pass --name"),
            "{:?}",
            again.diagnostics
        );
        let names: Vec<_> = read_scene_document(&scene)
            .unwrap()
            .entities
            .into_iter()
            .filter_map(|entity| entity.name)
            .collect();
        assert!(names.contains(&"Floor 2".to_owned()), "{names:?}");
        assert!(names.contains(&"West wall 2".to_owned()), "{names:?}");
        let other = root.join("scenes/other.rscene");
        std::fs::write(
            &other,
            r#"{"version": 9, "name": "Other", "entities": []}"#,
        )
        .unwrap();
        assert!(add_model(&other, &model, "Floor", false).ok);
        let names: Vec<_> = read_scene_document(&other)
            .unwrap()
            .entities
            .into_iter()
            .filter_map(|entity| entity.name)
            .collect();
        assert!(names.contains(&"Floor 2".to_owned()), "{names:?}");
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn outdated_cli_names_the_engine_commits_it_misses() {
        let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let head = Command::new("git")
            .arg("-C")
            .arg(&source)
            .args(["log", "-1", "--format=%ct %h %s", "--", "."])
            .output();
        let Some(head) = head
            .ok()
            .filter(|out| out.status.success() && !out.stdout.is_empty())
        else {
            return; // Not a git checkout, such as a crates.io download.
        };
        let head = String::from_utf8(head.stdout).unwrap();
        let (time, subject) = head.trim().split_once(' ').unwrap();
        let time = std::time::UNIX_EPOCH
            + std::time::Duration::from_secs(time.parse::<u64>().unwrap() - 1);
        let text = engine_commits_since(&source, time);
        assert!(text.starts_with("; engine commits since: "), "{text}");
        assert!(text.contains(subject), "{text}");
        let later =
            std::time::SystemTime::now() + std::time::Duration::from_secs(60);
        assert_eq!(engine_commits_since(&source, later), "");
    }

    #[test]
    fn once_a_day_lets_one_call_through_per_day() {
        let marker = std::env::temp_dir()
            .join(format!("rusting-once-{}", uuid::Uuid::new_v4()))
            .join("build/cli-outdated-shown");
        assert!(once_a_day(&marker));
        assert!(!once_a_day(&marker));
        let yesterday = std::time::SystemTime::now()
            - std::time::Duration::from_secs(90_000);
        std::fs::File::options()
            .write(true)
            .open(&marker)
            .unwrap()
            .set_modified(yesterday)
            .unwrap();
        assert!(once_a_day(&marker));
        #[cfg(unix)]
        {
            let target = marker.with_file_name("target");
            std::fs::write(&target, "keep").unwrap();
            std::fs::remove_file(&marker).unwrap();
            std::os::unix::fs::symlink(&target, &marker).unwrap();
            assert!(!once_a_day(&marker));
            assert_eq!(std::fs::read_to_string(&target).unwrap(), "keep");
        }
        std::fs::remove_dir_all(marker.parent().unwrap().parent().unwrap())
            .unwrap();
    }

    #[test]
    fn an_old_engine_agents_md_is_flagged_and_refreshed() {
        let root = std::env::temp_dir()
            .join(format!("rusting-agents-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("AGENTS.md");
        std::fs::write(
            &path,
            "# Agent guide\n\nThis is a Rusting game project. Old.",
        )
        .unwrap();
        assert_eq!(outdated_agents(&root).unwrap().code, "AGENTS_OUTDATED");
        let _ = fix_project(&root, false);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), PROJECT_AGENTS);
        assert!(std::fs::read_to_string(root.join("AGENTS.md.old"))
            .unwrap()
            .ends_with("Old."));
        assert!(outdated_agents(&root).is_none());
        std::fs::write(&path, "# My own guide").unwrap();
        assert!(outdated_agents(&root).is_none());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn record_and_replay_mistakes_fail_before_the_build() {
        let code = |options: RunOptions| {
            let result = run_game_project(Path::new("/nonexistent"), options);
            result.diagnostics[0].code
        };
        let ticks = Some(200);
        let record = Some(PathBuf::from("r.rec"));
        assert_eq!(
            code(RunOptions {
                headless_ticks: ticks,
                record,
                ..RunOptions::default()
            }),
            "CLI_USAGE"
        );
        let missing = Some(PathBuf::from("/nonexistent/missing.rec"));
        assert_eq!(
            code(RunOptions {
                headless_ticks: ticks,
                replay: missing.clone(),
                ..RunOptions::default()
            }),
            "CLI_USAGE"
        );
        let result = run_game_project(
            Path::new("/nonexistent"),
            RunOptions {
                replay: missing,
                ..RunOptions::default()
            },
        );
        assert_eq!(result.diagnostics[0].code, "FILE_NOT_FOUND");
        assert!(result.diagnostics[0].message.contains("missing.rec"));
    }

    #[test]
    fn shape_limits_projects_and_summarizes_listings() {
        let data = || json!({"n": 1, "xs": [{"a": 1, "b": 2}, {"a": 3, "b": 4}, {"a": 5}]});
        let mut limited = data();
        Shape {
            limit: Some(2),
            fields: Some(vec!["a".into()]),
            summary: false,
        }
        .apply(&mut limited);
        assert_eq!(limited["xs"], json!([{"a": 1}, {"a": 3}]));
        assert_eq!(limited["omitted"], json!({"xs": 1}));
        assert_eq!(limited["n"], 1);
        let mut counted = data();
        Shape {
            summary: true,
            ..Shape::default()
        }
        .apply(&mut counted);
        assert_eq!(counted["xs"], json!({"count": 3}));
    }
}

#[cfg(test)]
mod hint_tests {
    #[test]
    fn capture_game_needs_a_path_inside_a_project() {
        let dir = std::env::temp_dir().join("rusting-capture-game-no-project");
        let options = super::CaptureOptions {
            camera: None,
            tick: 1,
            extent: [8, 8],
            output: dir.join("shot.png"),
            picks: Vec::new(),
            pick_rects: Vec::new(),
            hud: true,
            at: None,
            look_at: None,
            look: None,
        };
        let result =
            super::capture_game(&dir.join("scenes/main.rscene"), &options);
        assert!(!result.ok);
        assert_eq!(result.diagnostics[0].code, "PROJECT_MISSING_FILE");
    }

    use super::*;

    #[test]
    fn rustc_errors_get_the_engine_pattern_that_fixes_them() {
        let output = "src/main.rs:12:9: error[E0499]: cannot borrow `*scene` as mutable more than once at a time\n\
src/main.rs:13:9: error[E0599]: no method named `teleport` found for struct `GameScene<'_>` in the current scope\n\
src/main.rs:20:1: error[E0499]: cannot borrow `*scene` as mutable more than once at a time\n\
src/main.rs:30:5: error[E0425]: cannot find value `x` in this scope";
        let hints = engine_hints(Path::new("/p"), output);
        assert_eq!(hints.len(), 2, "one per pattern: {hints:?}");
        assert_eq!(hints[0].code, "RUST_ENGINE_HINT");
        assert_eq!(hints[0].line, Some(12));
        assert_eq!(hints[0].file.as_deref(), Some(Path::new("/p/src/main.rs")));
        assert!(hints[1].message.contains("rusting docs search"));
    }

    #[test]
    fn controller_feel_reports_jump_and_speed_in_physical_units() {
        // Default player: 5 m/s jump under 9.81 m/s2 peaks at 1.27 m.
        let feel =
            controller_feel(&json!({"rusting.player_controller": {}})).unwrap();
        assert!((feel["jump_apex_m"].as_f64().unwrap() - 1.274).abs() < 0.01);
        assert!((feel["air_time_s"].as_f64().unwrap() - 1.019).abs() < 0.01);
        assert_eq!(feel["notes"], json!([]), "{feel}");
        // A moon jump and a crawl fall outside the platformer ranges.
        let feel = controller_feel(&json!({"rusting.platformer_controller":
            {"run_speed": 2.0, "jump_speed": 20.0, "gravity": 10.0}}))
        .unwrap();
        assert_eq!(feel["jump_apex_m"], json!(20.0));
        assert_eq!(feel["jump_distance_m"], json!(8.0));
        assert_eq!(feel["notes"].as_array().unwrap().len(), 3, "{feel}");
        assert!(controller_feel(&json!({})).is_none());
    }

    #[test]
    fn lint_flags_giant_players_zero_scales_and_dead_lights() {
        let id = |n: u8| format!("00000000-0000-0000-0000-0000000000{n:02}");
        let cam = json!({"projection": {"Perspective": {"vertical_fov_radians": 1.0,
            "near": 0.1, "far": 100.0}}, "active": true, "priority": 0});
        let scale = |s: [f32; 3]| json!({"position": [0.0, 0.0, 0.0], "rotation": [0.0, 0.0, 0.0], "scale": s});
        let player = json!({"rusting.player_controller": "{}"});
        let at = |p: [f32; 3]| json!({"position": p, "rotation": [0.0, 0.0, 0.0], "scale": [1.0, 1.0, 1.0]});
        let mesh = |shape: &str| {
            json!({"mesh": {"BuiltinPrimitive": shape},
            "material": "BuiltinError", "cast_shadows": true, "receive_shadows": true})
        };
        let body = |shape: Value, sensor: bool| {
            json!({"shape": shape,
            "friction": 0.5, "restitution": 0.0, "sensor": sensor})
        };
        let scene = json!({"format_version": 7, "name": "Lint", "entities": [
            {"id": id(1), "name": "Giant", "transform": scale([1.0, 20.0, 1.0]),
             "components": player},
            // 1.8 m capsule under a 0.5 parent scale: 0.9 m, fine.
            {"id": id(2), "name": "Rig", "transform": scale([0.5, 0.5, 0.5])},
            {"id": id(3), "parent": id(2), "name": "Kid",
             "transform": scale([1.0, 1.0, 1.0]), "components": player},
            {"id": id(4), "name": "Flat", "transform": scale([1.0, 0.0, 1.0])},
            {"id": id(5), "name": "Lamp", "point_light":
             {"color": [1.0, 1.0, 1.0], "intensity": 5.0, "range": 0.0}},
            {"id": id(6), "name": "Torch", "point_light":
             {"color": [1.0, 1.0, 1.0], "intensity": 0.0, "range": 8.0}},
            {"id": id(7), "name": "Sun", "directional_light":
             {"color": [0.0, 0.0, 0.0], "illuminance": 1.0, "shadows": true}},
            // A 4 m wall turned 90 degrees, so its long side runs along Z.
            {"id": id(8), "name": "Wall", "transform": {"position": [10.0, 1.0, 0.0],
             "rotation": [0.0, 1.5707964, 0.0], "scale": [1.0, 1.0, 1.0]},
             "collider": {"shape": {"Box": {"half_extents": [2.0, 1.0, 0.2]}},
              "friction": 0.5, "restitution": 0.0, "sensor": false}},
            {"id": id(9), "name": "Stuck Cam", "camera": cam,
             "transform": {"position": [10.0, 1.0, 1.5],
              "rotation": [0.0, 0.0, 0.0], "scale": [1.0, 1.0, 1.0]}},
            {"id": id(10), "name": "Clear Cam", "camera": cam,
             "transform": {"position": [11.5, 1.0, 0.0],
              "rotation": [0.0, 0.0, 0.0], "scale": [1.0, 1.0, 1.0]}},
            // Inside its own player's collider: exempt.
            {"id": id(11), "parent": id(3), "name": "Eye Cam", "camera": cam,
             "transform": scale([1.0, 1.0, 1.0])},
                    // A 1 m cube with a 3 m wide box collider; a ball that matches;
            // an oversized trigger zone, which is fine.
            {"id": id(12), "name": "Crate", "transform": at([-20.0, 0.0, 0.0]), "mesh_renderer": mesh("Cube"),
             "collider": body(json!({"Box": {"half_extents": [1.5, 0.5, 0.5]}}), false)},
            {"id": id(13), "name": "Ball", "transform": at([-30.0, 0.0, 0.0]), "mesh_renderer": mesh("Sphere"),
             "collider": body(json!({"Sphere": {"radius": 0.45}}), false)},
            {"id": id(14), "name": "Zone", "transform": at([-40.0, 0.0, 0.0]), "mesh_renderer": mesh("Cube"),
             "collider": body(json!({"Box": {"half_extents": [5.0, 5.0, 5.0]}}), true)},
            // A 3 m capsule: a camera in its top cap is inside, one just
            // past its side is not.
            {"id": id(15), "name": "Pillar", "transform": at([50.0, 0.0, 0.0]),
             "collider": body(json!({"Capsule": {"half_height": 1.0, "radius": 0.5}}), false)},
            {"id": id(16), "name": "Pillar Cam", "camera": cam, "transform": at([50.0, 1.3, 0.0])},
            {"id": id(17), "name": "Side Cam", "camera": cam, "transform": at([50.6, 0.0, 0.0])},
            // Inside the 0.9 m `Kid` player but not its child: exempt too.
            {"id": id(18), "name": "Loose Eye Cam", "camera": cam, "transform": at([0.0, 0.3, 0.0])},
            // A coin buried in the pillar warns; one beside it does not.
            {"id": id(19), "name": "Buried Coin", "transform": at([50.0, -0.5, 0.0]),
             "collider": body(json!({"Sphere": {"radius": 0.3}}), true)},
            {"id": id(20), "name": "Free Coin", "transform": at([51.5, 0.0, 0.0]),
             "collider": body(json!({"Sphere": {"radius": 0.3}}), true)},
            // HUD text: 10 px is too small; 1400 px right of the top-left
            // corner is off a 1280 px view; 40 px up from the bottom is fine,
            // and so is any offset inside a camera's own viewport.
            {"id": id(21), "name": "Tiny Text", "components": {"rusting.hud":
             json!({"text": "hp", "font_size": 10.0}).to_string()}},
            {"id": id(22), "name": "Lost Text", "components": {"rusting.hud":
             json!({"text": "score", "offset": [1400.0, 16.0]}).to_string()}},
            {"id": id(23), "name": "Footer", "components": {"rusting.hud":
             json!({"text": "ok", "anchor": "Bottom", "offset": [0.0, 40.0]}).to_string()}},
            {"id": id(24), "name": "Cam Text", "components": {"rusting.hud":
             json!({"text": "cam", "offset": [1400.0, 16.0], "camera": "Clear Cam"}).to_string()}},
        ]});
        let mut scene = scene;
        scene["entities"][2]["collider"] = json!({"shape": {"Box":
            {"half_extents": [0.3, 0.9, 0.3]}}, "friction": 0.5,
            "restitution": 0.0, "sensor": false});
        let document =
            crate::runtime::parse_scene_document(scene.to_string().as_bytes())
                .unwrap();
        let found: Vec<_> = lint_scene(&document, &BTreeSet::new())
            .into_iter()
            .map(|d| (d.code, d.entity.unwrap().name.unwrap_or_default()))
            .collect();
        assert_eq!(
            found,
            [
                ("LINT_PLAYER_SCALE", "Giant".to_owned()),
                ("LINT_ZERO_SCALE", "Flat".to_owned()),
                ("LINT_LIGHT_OFF", "Lamp".to_owned()),
                ("LINT_LIGHT_OFF", "Sun".to_owned()),
                ("LINT_CAMERA_INSIDE", "Stuck Cam".to_owned()),
                ("LINT_COLLIDER_MISMATCH", "Crate".to_owned()),
                ("LINT_CAMERA_INSIDE", "Pillar Cam".to_owned()),
                ("LINT_GOAL_INSIDE", "Buried Coin".to_owned()),
                ("LINT_TEXT_SMALL", "Tiny Text".to_owned()),
                ("LINT_TEXT_OFFSCREEN", "Lost Text".to_owned()),
            ],
            "Kid (0.9 m), Torch (switched on in code), Clear Cam, Eye Cam, Ball and Zone pass"
        );
    }

    #[test]
    fn lint_warns_about_terrains_with_more_than_300_chunks() {
        let id = |n: u16| format!("00000000-0000-0000-0000-00000000{n:04}");
        let heights = json!(vec![vec![0.0; 1501]; 1501]).to_string();
        let field = format!(r#"{{"heights": {heights}, "spacing": 2.0}}"#);
        let scene = json!({"format_version": 7, "name": "Island", "entities": [
            {"id": id(1), "name": "Small Chunks", "components": {
                "rusting.heightfield": field,
                "rusting.terrain": json!({"chunk_cells": 64}).to_string()}},
            {"id": id(2), "name": "Auto Chunks", "components": {
                "rusting.heightfield": field,
                "rusting.terrain": json!({}).to_string()}},
        ]});
        let document =
            crate::runtime::parse_scene_document(scene.to_string().as_bytes())
                .unwrap();
        let found: Vec<_> = lint_scene(&document, &BTreeSet::new())
            .into_iter()
            .map(|d| (d.code, d.entity.unwrap().name.unwrap_or_default()))
            .collect();
        assert_eq!(found, [("LINT_TERRAIN_CHUNKS", "Small Chunks".to_owned())]);
    }

    #[test]
    fn validate_reports_missing_and_root_relative_terrain_layers() {
        let root = std::env::temp_dir()
            .join(format!("rusting-test-terrain-paths-{}", std::process::id()));
        std::fs::create_dir_all(root.join("scenes")).unwrap();
        std::fs::create_dir_all(root.join("assets")).unwrap();
        std::fs::write(root.join("project.json"), "{}").unwrap();
        std::fs::write(root.join("assets/grass.png"), "").unwrap();
        let layers = json!({"layers": [
            {"texture": "../assets/grass.png"},
            {"texture": "assets/grass.png"},
            {"texture": "../assets/rock.png", "normal": ""},
        ]});
        let scene = json!({"format_version": 7, "name": "Island", "entities": [
            {"id": "00000000-0000-0000-0000-000000000001", "name": "Ground",
             "components": {"rusting.terrain": layers.to_string()}},
        ]});
        let document =
            crate::runtime::parse_scene_document(scene.to_string().as_bytes())
                .unwrap();
        let found: Vec<_> = terrain_layer_diagnostics(
            &root.join("scenes/main.rscene"),
            &document,
        )
        .into_iter()
        .map(|d| (d.code, d.file.unwrap()))
        .collect();
        assert_eq!(
            found,
            [
                ("SCENE_TERRAIN_ROOT_PATH", root.join("assets/grass.png")),
                ("SCENE_MISSING_ASSET", root.join("assets/rock.png")),
            ]
        );
        // Loading resolves both spellings to the same file.
        let mut document = document;
        crate::runtime::absolutize_scene_assets(
            &mut document,
            &root.join("scenes"),
        )
        .unwrap();
        let terrain: crate::runtime::Terrain = serde_json::from_str(
            &document.entities[0].components["rusting.terrain"],
        )
        .unwrap();
        let grass = root.canonicalize().unwrap().join("assets/grass.png");
        assert_eq!(terrain.layers[0].texture, grass);
        assert_eq!(terrain.layers[1].texture, grass);
        assert!(terrain.layers[2].normal.as_os_str().is_empty());
        // Saving writes every layer relative to the scene again.
        crate::runtime::relativize_scene_assets(
            &mut document,
            &root.join("scenes"),
        )
        .unwrap();
        let terrain: crate::runtime::Terrain = serde_json::from_str(
            &document.entities[0].components["rusting.terrain"],
        )
        .unwrap();
        assert_eq!(terrain.layers[1].texture, Path::new("../assets/grass.png"));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn lint_names_visible_lights_past_the_quality_budget() {
        let id = |n: u16| format!("00000000-0000-0000-0000-00000000{n:04}");
        let point =
            json!({"color": [1.0, 1.0, 1.0], "intensity": 5.0, "range": 8.0});
        // Spot first in the file, but the renderer takes point lights first.
        let mut entities = vec![
            json!({"id": id(9990), "name": "Spot", "spot_light": {"color": [1.0, 1.0, 1.0],
                "intensity": 5.0, "range": 8.0, "inner_angle": 0.3, "outer_angle": 0.5}}),
            json!({"id": id(9991), "name": "Hidden", "visible": false, "point_light": point}),
            json!({"id": id(9992), "name": "Off Set", "visible": false}),
            json!({"id": id(9993), "parent": id(9992), "name": "Set Lamp", "point_light": point}),
        ];
        let eco = crate::rendering::scene_renderer::light_budget(
            crate::runtime::QualityProfile::Eco,
        ) as u16;
        for n in 0..eco {
            entities.push(json!({"id": id(n), "name": format!("Lamp {n}"), "point_light": point}));
        }
        let scene = json!({"format_version": 7, "name": "Lights", "entities": entities,
            "render": {"quality": "Eco", "culling": "Auto"}});
        let document =
            crate::runtime::parse_scene_document(scene.to_string().as_bytes())
                .unwrap();
        let found: Vec<_> = lint_scene(&document, &BTreeSet::new())
            .into_iter()
            .map(|d| (d.code, d.entity.unwrap().name.unwrap_or_default()))
            .collect();
        assert_eq!(found, [("LINT_LIGHT_BUDGET", "Spot".to_owned())]);
    }

    #[test]
    fn lint_flags_goals_nothing_reacts_to() {
        let entity = |name: &str, key: &str, value: serde_json::Value| {
            json!({"id": uuid::Uuid::new_v4(), "name": name,
                "components": {key: value.to_string()}})
        };
        let counter = |name: &str| {
            entity(name, "rusting.counter", json!({"name": name, "target": 3}))
        };
        let scene = json!({"format_version": 7, "name": "Goals", "entities": [
            counter("gems"), counter("keys"), counter("laps"), counter("coins"),
            counter("kills"),
            entity("Free", "rusting.counter", json!({"name": "score"})),
            entity("Door", "rusting.pickup", json!({"counter": "x", "requires": "keys"})),
            entity("Won", "rusting.hud", json!({"text": "Done", "requires": "laps"})),
            entity("Coins", "rusting.hud", json!({"text": "Coins {coins}/3"}))]});
        let document =
            crate::runtime::parse_scene_document(scene.to_string().as_bytes())
                .unwrap();
        let found = lint_silent_goal(
            &document,
            "if scene.counter_complete(\"kills\") {}",
        );
        let names: Vec<_> = found
            .iter()
            .map(|d| d.entity.as_ref().unwrap().name.as_deref().unwrap())
            .collect();
        assert_eq!(
            names,
            ["gems"],
            "a pickup or HUD requiring it, a readout, code naming it and no target pass"
        );
        assert_eq!(found[0].code, "LINT_SILENT_GOAL");
    }

    #[test]
    fn lint_warns_when_counters_exist_but_nothing_ends_a_round() {
        let scene = |target: serde_json::Value| {
            let counter =
                json!({"name": "score", "value": 0, "target": target});
            let scene = json!({"format_version": 7, "name": "Ending", "entities": [
                {"id": "00000000-0000-0000-0000-000000000001", "name": "Score",
                 "components": {"rusting.counter": counter.to_string()}}]});
            crate::runtime::parse_scene_document(scene.to_string().as_bytes())
                .unwrap()
        };
        let endless = scene(serde_json::Value::Null);
        let warning = lint_no_ending(&endless, "fn update() {}").unwrap();
        assert_eq!(warning.code, "LINT_NO_ENDING");
        assert_eq!(warning.entity.unwrap().name.as_deref(), Some("Score"));
        // A target, or code that ends the round itself, is an ending.
        assert!(lint_no_ending(&scene(json!(10)), "").is_none());
        let code = "if scene.counter_value(\"score\") > 9 { scene.load_scene(\"scenes/2.rscene\"); }";
        assert!(lint_no_ending(&endless, code).is_none());
        // No counters: a toy or a sandbox, not judged.
        let empty = crate::runtime::parse_scene_document(
            br#"{"format_version": 7, "name": "Toy", "entities": []}"#,
        )
        .unwrap();
        assert!(lint_no_ending(&empty, "").is_none());
    }

    #[cfg(feature = "ui")]
    #[test]
    fn captured_text_is_judged_against_the_scene_behind_it() {
        use crate::rendering::capture::TextSpot;
        // An 8 x 2 frame: white on the left half, black on the right.
        let background: Vec<u8> = (0..16)
            .flat_map(|i| if i % 8 < 4 { [255; 4] } else { [0, 0, 0, 255] })
            .collect();
        let spot = |text: &str, left: f32, color: [f32; 4], backed| TextSpot {
            text: text.to_owned(),
            rect: [left, 0.0, left + 4.0, 2.0],
            color,
            size: 16.0,
            backed,
        };
        let white = [1.0; 4];
        let texts = [
            spot("On White", 0.0, white, false),
            spot("On Black", 4.0, white, false),
            spot("On Button", 0.0, white, true),
            spot("Faint", 4.0, [1.0, 1.0, 1.0, 0.2], false),
            // Half over white, half over black: the median pixel is over
            // white, so black text passes.
            spot("Across", 2.0, [0.0, 0.0, 0.0, 1.0], false),
        ];
        let warned: Vec<_> =
            text_contrast_warnings(&texts, &background, [8, 2])
                .into_iter()
                .map(|warning| warning.message)
                .collect();
        assert_eq!(
            warned,
            [
                "HUD text `On White` at 0,0 has a median contrast of 1.0:1 against the scene behind it, below 4.5:1 for 16 px text",
                "HUD text `Faint` at 4,0 has a median contrast of 1.7:1 against the scene behind it, below 4.5:1 for 16 px text",
            ]
        );
    }

    #[cfg(feature = "ui")]
    #[test]
    fn lint_measures_hud_text_that_runs_past_the_view() {
        let hud = |name: &str, hud: serde_json::Value| json!({"name": name, "components": {"rusting.hud": hud.to_string()}});
        let long =
            "Collect every golden acorn before the storm reaches the valley";
        let mut scene = json!({"format_version": 7, "name": "Hud", "entities": [
            // 700 px in from the left, a long line runs off the right side.
            hud("Long Left", json!({"text": long, "font_size": 24.0, "offset": [700.0, 16.0]})),
            // The same line anchored top right grows left and fits.
            hud("Long Right", json!({"text": long, "font_size": 24.0, "anchor": "TopRight"})),
            // Centred 40 lines of 24 px text are taller than 720 px.
            hud("Tall", json!({"text": "line\n".repeat(40), "font_size": 24.0, "anchor": "Center", "offset": [0.0, 0.0]})),
            // A counter placeholder is measured at the counter's start value.
            hud("Score", json!({"text": "Score {score}", "anchor": "TopRight"})),
            {"name": "Score Counter", "components": {"rusting.counter":
             json!({"name": "score", "value": 0}).to_string()}},
            // Button text against the dark button fill: grey, faint white
            // and mid grey at 18 px fall short; white, mid grey at 28 px
            // (large text needs 3:1) and dark text with no button pass.
            hud("Grey Button", json!({"text": "Go", "button": true, "color": [0.4, 0.4, 0.4, 1.0]})),
            hud("Faint Button", json!({"text": "Go", "button": true, "color": [1.0, 1.0, 1.0, 0.2]})),
            hud("Mid Button", json!({"text": "Go", "button": true, "color": [0.57, 0.57, 0.57, 1.0]})),
            hud("Big Mid Button", json!({"text": "Go", "button": true, "font_size": 28.0, "color": [0.57, 0.57, 0.57, 1.0]})),
            hud("White Button", json!({"text": "Go", "button": true})),
            hud("Dark Label", json!({"text": "Go", "color": [0.1, 0.1, 0.1, 1.0]})),
        ]});
        for (index, entity) in scene["entities"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .enumerate()
        {
            entity["id"] =
                json!(format!("00000000-0000-0000-0000-{:012}", index + 1));
        }
        let document =
            crate::runtime::parse_scene_document(scene.to_string().as_bytes())
                .unwrap();
        let found: Vec<_> = lint_scene(&document, &BTreeSet::new())
            .into_iter()
            .map(|d| (d.code, d.entity.unwrap().name.unwrap_or_default()))
            .collect();
        assert_eq!(
            found,
            [
                ("LINT_TEXT_OVERFLOW", "Long Left".to_owned()),
                ("LINT_TEXT_OVERFLOW", "Tall".to_owned()),
                ("LINT_TEXT_CONTRAST", "Grey Button".to_owned()),
                ("LINT_TEXT_CONTRAST", "Faint Button".to_owned()),
                ("LINT_TEXT_CONTRAST", "Mid Button".to_owned()),
            ]
        );
        // A theme with light buttons makes the default white text the
        // unreadable one.
        let root = std::env::temp_dir()
            .join(format!("rusting-theme-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(root.join("assets/ui")).unwrap();
        std::fs::write(
            root.join("assets/ui/theme.json"),
            r#"{"button_fill": [0.9, 0.9, 0.9, 1.0]}"#,
        )
        .unwrap();
        let themed: Vec<_> = lint_scene_with_fill(
            &document,
            &BTreeSet::new(),
            button_fill(&root),
        )
        .into_iter()
        .filter(|d| d.code == "LINT_TEXT_CONTRAST")
        .map(|d| d.entity.unwrap().name.unwrap_or_default())
        .collect();
        std::fs::remove_dir_all(&root).unwrap();
        assert!(themed.contains(&"White Button".to_owned()), "{themed:?}");
        assert!(!themed.contains(&"Grey Button".to_owned()), "{themed:?}");
    }

    #[test]
    fn lint_flags_goals_walled_off_from_the_player() {
        let mut next = 0u8;
        let mut id = || {
            next += 1;
            format!("00000000-0000-0000-0000-0000000000{next:02}")
        };
        let at = |x: f32, y: f32, z: f32| json!({"position": [x, y, z], "rotation": [0.0, 0.0, 0.0], "scale": [1.0, 1.0, 1.0]});
        let solid = |half: [f32; 3]| {
            json!({"shape": {"Box": {"half_extents": half}}, "friction": 0.5,
                "restitution": 0.0, "sensor": false})
        };
        let coin = json!({"shape": {"Sphere": {"radius": 0.3}}, "friction": 0.5,
            "restitution": 0.0, "sensor": true});
        let mut entities = vec![
            json!({"id": id(), "name": "Player", "transform": at(0.0, 0.9, 0.0),
            "components": {"rusting.player_controller": "{}"}}),
        ];
        // Four walls of a 4 m room around (x, 0), `height` m tall; the first
        // wall is called `first`.
        let mut room = |x: f32, height: f32, first: &str| {
            let walls = [
                (x - 2.1, 0.0, [0.1, height / 2.0, 2.2]),
                (x + 2.1, 0.0, [0.1, height / 2.0, 2.2]),
                (x, -2.1, [2.2, height / 2.0, 0.1]),
                (x, 2.1, [2.2, height / 2.0, 0.1]),
            ];
            for (n, (wx, wz, half)) in walls.into_iter().enumerate() {
                let name = if n == 0 {
                    first.to_owned()
                } else {
                    format!("Wall {x} {n}")
                };
                entities.push(json!({"id": id(), "name": name,
                    "transform": at(wx, height / 2.0, wz), "collider": solid(half)}));
            }
        };
        room(8.0, 3.0, "Wall");
        room(16.0, 3.0, "Door");
        room(24.0, 1.0, "Low Wall");
        for (name, x, y) in [
            ("Locked Coin", 8.0, 1.0),
            ("High Coin", 8.0, 6.0),
            ("Door Coin", 16.0, 1.0),
            ("Ledge Coin", 24.0, 1.0),
            ("Free Coin", -3.0, 1.0),
        ] {
            entities.push(
                json!({"id": id(), "name": name, "transform": at(x, y, 0.0),
                "collider": coin}),
            );
        }
        let scene =
            json!({"format_version": 7, "name": "Rooms", "entities": entities});
        let document =
            crate::runtime::parse_scene_document(scene.to_string().as_bytes())
                .unwrap();
        let found: Vec<_> =
            lint_scene(&document, &BTreeSet::from(["Door".to_owned()]))
                .into_iter()
                .map(|d| (d.code, d.entity.unwrap().name.unwrap_or_default()))
                .collect();
        assert_eq!(
            found,
            [("LINT_GOAL_UNREACHABLE", "Locked Coin".to_owned())],
            "the 3 m room is closed; a door named in code, a 1 m wall the player jumps, a coin above the floor and an open coin pass"
        );
        let doors_closed = lint_scene(&document, &BTreeSet::new());
        assert_eq!(doors_closed.len(), 2);
    }

    #[test]
    fn literal_asset_paths_in_game_code_must_be_files() {
        let root = std::env::temp_dir()
            .join(format!("rusting-code-assets-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::create_dir_all(root.join("assets/charts")).unwrap();
        std::fs::write(root.join("assets/charts/easy.json"), "{}").unwrap();
        std::fs::write(
            root.join("src/main.rs"),
            "let a = scene.load_text(\"charts/easy.json\");\n\
             let b = scene.load_text(\"charts/hard.json\");\n\
             scene.play_sound(&clip, 1.0);\n",
        )
        .unwrap();
        let found = code_asset_diagnostics(&root);
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(
            found[0].message,
            "src/main.rs:2: `charts/hard.json` is not a file under assets/"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn lint_flags_hud_status_shown_by_colour_alone() {
        let root = std::env::temp_dir()
            .join(format!("rusting-colour-only-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::create_dir_all(root.join("scenes")).unwrap();
        let hud = |name: &str, text: &str| {
            json!({"id": uuid::Uuid::new_v4(), "name": name,
            "components": {"rusting.hud": json!({"text": text}).to_string()}})
        };
        let scene = json!({"format_version": 7, "name": "Main", "entities": [
            hud("Dot", "+"), hud("Judge", "-"), hud("Health", "HP {hp}"), hud("Label", "Go"), hud("Aim", "+")]});
        std::fs::write(root.join("scenes/main.rscene"), scene.to_string())
            .unwrap();
        std::fs::write(
            root.join("src/main.rs"),
            "scene.set_hud(\"Label\", |h| h.font_size = 20.0);\n\
             scene.set_hud(\"Dot\", |h| h.color = [1.0, 0.0, 0.0, f(1)]);\n\
             scene.set_hud(\"Judge\", |hud| {\n\
                 hud.color = color;\n\
             });\n\
             scene.set_hud(\"Judge\", |hud| hud.text = word.into());\n\
             scene.set_hud(\"Health\", |hud| hud.color = red);\n\
             scene.set_hud(\"Aim\", |h| h.color = [1.0, 1.0, 1.0, if on { 0.9 } else { 0.3 }]);\n",
        )
        .unwrap();
        let found = lint_color_only_status(&root);
        let messages: Vec<_> = found.iter().map(|d| &d.message).collect();
        assert_eq!(messages.len(), 1, "{messages:?}");
        assert!(
            messages[0].starts_with("src/main.rs:2: HUD `Dot` changes colour"),
            "a size change, a colour set with the text in another call, a counter readout and an alpha-only change on the scene's white pass: {messages:?}"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn lint_flags_translation_keys_a_locale_lacks() {
        let root = std::env::temp_dir()
            .join(format!("rusting-locales-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::create_dir_all(root.join("assets/locales")).unwrap();
        std::fs::write(
            root.join("src/main.rs"),
            "let a = scene.tr(\"title\");\n\
             let b = scene.tr_count(\"coins\", n);\n\
             let c = s.to_str(\"x\");\n",
        )
        .unwrap();
        std::fs::write(
            root.join("assets/locales/en.json"),
            r#"{"title": "Hi", "coins.one": "1 coin", "coins.other": "{count} coins", "menu": "Menu"}"#,
        )
        .unwrap();
        std::fs::write(
            root.join("assets/locales/fr.json"),
            r#"{"title": "Salut", "coins.other": "{count} pièces"}"#,
        )
        .unwrap();
        let scene: SceneDocument = serde_json::from_value(json!({
            "format_version": 7, "name": "Main", "entities": [
            {"id": uuid::Uuid::new_v4(), "name": "Menu", "components": {"rusting.hud":
                json!({"text": "{tr:menu} {coins}"}).to_string()}}]}))
        .unwrap();
        let found = lint_missing_translations(&root, &scene);
        let messages: Vec<_> = found.iter().map(|d| &d.message).collect();
        assert_eq!(
            messages,
            [
                "src/main.rs:2: `coins.one` has no text in assets/locales/fr.json",
                "HUD `Menu`: `menu` has no text in assets/locales/fr.json",
            ]
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn lint_flags_names_nothing_defines() {
        let root = std::env::temp_dir()
            .join(format!("rusting-code-names-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::create_dir_all(root.join("scenes")).unwrap();
        let scene = json!({"format_version": 7, "name": "Main", "entities": [
            {"id": uuid::Uuid::new_v4(), "name": "Player", "classes": ["hero"]},
            {"id": uuid::Uuid::new_v4(), "components": {"rusting.input_action":
                "{\"action\":\"jump\",\"inputs\":[\"Space\"]}"}}]});
        std::fs::write(root.join("scenes/main.rscene"), scene.to_string())
            .unwrap();
        std::fs::write(
            root.join("src/main.rs"),
            "const CARD: &str = \"Card\";\n\
             scene.object(\"Player\").position();\n\
             scene.flash(\"Playr\");\n\
             scene.spawn_copy(\"Player\", \"Ghost\", at);\n\
             scene.spawn_cube(CARD, at);\n\
             scene.set_visible(\"Ghost\", true);\n\
             scene.set_visible(CARD, true);\n\
             scene.despawn(\"Card\");\n\
             scene.despawn(&name);\n\
             scene.pressed(\"jump\");\n\
             scene.held(\"jmup\");\n\
             scene.held(\"player.sprint\");\n\
             scene.rebind(\"dash\", &keys);\n\
             scene.pressed(\"dash\");\n\
             scene.load_scene(\"scenes/main.rscene\");\n\
             scene.load_scene(\"scenes/mian.rscene\");\n\
             scene.play_sound(\"sfx:coin 7\", 1.0);\n\
             scene.play_sound(\"hit.wav\", 1.0);\n\
             scene.in_class(\"hero\");\n\
             scene.add_class(&shot, \"shot\");\n\
             for shot in scene.in_class(\"shot\") {}\n\
             spawn(scene, \"Enemy Template\", &name, \"enemy\", at);\n\
             for enemy in scene.in_class(\"enemys\") {}\n\
             scene.spawn_prefab(\"prefabs/tlie.rscene\", \"Tile\", at);\n\
             scene.set_material(\"Plyer\", material);\n\
             scene.gpu_events(\"landed\");\n\
             watch(GpuPhysicsRule::new(\"landed\", condition));\n\
             scene.gpu_events(\"landd\");\n",
        )
        .unwrap();
        let found = lint_missing_names(&root);
        let messages: Vec<_> = found.iter().map(|d| &d.message).collect();
        assert_eq!(
            messages,
            [
                "src/main.rs:3: no scene has an object named `Playr`; did you mean `Player`?",
                "src/main.rs:11: no input action is named `jmup`; did you mean `jump`?",
                "src/main.rs:16: no file `scenes/mian.rscene` under the project; did you mean `scenes/main.rscene`?",
                "src/main.rs:18: no file `hit.wav` under assets/",
                "src/main.rs:23: no object or code puts anything in class `enemys`; did you mean `enemy`?",
                "src/main.rs:24: no file `prefabs/tlie.rscene` under assets/",
                "src/main.rs:25: no scene has an object named `Plyer`; did you mean `Player`?",
                "src/main.rs:28: no GPU physics rule raises an event named `landd`; did you mean `landed`?",
            ],
            "a scene name, a spawned literal, a spawned constant, a run-time name, a scene action, a player action, a rebound action, an existing scene, a built-in clip, a scene class, an added class, a helper's class and a GPU event a rule names pass"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn provenance_records_crates_code_assets_scenes_and_scenarios() {
        let parent = std::env::temp_dir()
            .join(format!("rusting-provenance-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&parent).unwrap();
        create_project_from(&parent, "boxy", ProjectTemplate::Puzzle).unwrap();
        let root = parent.join("boxy");
        std::fs::write(
            root.join("Cargo.lock"),
            "[[package]]\nname = \"rusting_engine\"\nversion = \"0.1.0\"\nsource = \"git+https://example.com/e#abc\"\n\n[[package]]\nname = \"serde\"\nversion = \"1.0.0\"\n",
        )
        .unwrap();

        let before = provenance(&root).data;
        assert_eq!(
            before["crates"],
            json!([{"name": "rusting_engine", "version": "0.1.0", "source": "git+https://example.com/e#abc"}])
        );
        assert_eq!(before["scene_hashes"][0]["path"], "scenes/main.rscene");
        assert_eq!(before["scenario_hashes"][0]["path"], "tests/solve.json");
        assert!(before["scenario_hashes"][0]["ticks"].as_u64().is_some());

        std::fs::write(root.join("src/extra.rs"), "fn f() {}\n").unwrap();
        let after = provenance(&root).data;
        assert_ne!(after["code_hash"], before["code_hash"]);
        assert_eq!(after["scene_hashes"], before["scene_hashes"]);
        std::fs::remove_dir_all(parent).unwrap();
    }

    #[test]
    fn log_lists_operations_and_revert_undoes_one() {
        let parent = std::env::temp_dir()
            .join(format!("rusting-journal-cli-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&parent).unwrap();
        create_project_from(&parent, "boxy", ProjectTemplate::Puzzle).unwrap();
        let root = parent.join("boxy");
        let original = std::fs::read(root.join("src/main.rs")).unwrap();
        {
            let _op = crate::runtime::journal::begin("add system . spin");
            assert!(add_system(&root, "spin").ok);
        }
        let log = journal_log(&root).data;
        let op = log["journal"][0]["op"].as_str().unwrap().to_owned();
        assert_eq!(log["journal"][0]["command"], "add system . spin");
        let files: Vec<_> = log["journal"][0]["files"]
            .as_array()
            .unwrap()
            .iter()
            .map(|file| file["file"].as_str().unwrap())
            .collect();
        assert!(files.contains(&"src/main.rs"), "{files:?}");

        assert_eq!(
            revert(&root, "nope").diagnostics[0].code,
            "JOURNAL_UNKNOWN_OP"
        );
        assert!(revert(&root, &op).ok);
        assert_eq!(std::fs::read(root.join("src/main.rs")).unwrap(), original);
        assert!(!root.join("tests/spin.json").exists());
        assert_eq!(
            journal_log(&root).data["journal"][0]["command"],
            format!("revert {op}")
        );
        std::fs::remove_dir_all(parent).unwrap();
    }

    #[test]
    fn revert_undoes_scene_split_and_join() {
        let parent = std::env::temp_dir()
            .join(format!("rusting-split-revert-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&parent).unwrap();
        create_project_from(&parent, "boxy", ProjectTemplate::Puzzle).unwrap();
        let root = parent.join("boxy");
        let scene = root.join("scenes/main.rscene");
        let original = std::fs::read(&scene).unwrap();
        let last_op = || {
            journal_log(&root).data["journal"][0]["op"]
                .as_str()
                .unwrap()
                .to_owned()
        };
        {
            let _op = crate::runtime::journal::begin("scene split");
            assert!(convert_scene(&scene, true).ok);
        }
        assert!(scene.is_dir());
        assert!(revert(&root, &last_op()).ok);
        assert_eq!(std::fs::read(&scene).unwrap(), original);

        {
            let _op = crate::runtime::journal::begin("scene split");
            assert!(convert_scene(&scene, true).ok);
        }
        let folder = read_scene_bytes(&scene).unwrap();
        // A link in the folder is removed, never read into the journal.
        #[cfg(unix)]
        std::os::unix::fs::symlink(
            root.join("project.json"),
            scene.join("outside.json"),
        )
        .unwrap();
        {
            let _op = crate::runtime::journal::begin("scene join");
            assert!(convert_scene(&scene, false).ok);
        }
        assert!(scene.is_file());
        let log = journal_log(&root).data;
        assert!(!log.to_string().contains("outside.json"), "{log}");
        assert!(revert(&root, &last_op()).ok);
        assert!(scene.is_dir());
        assert_eq!(read_scene_bytes(&scene).unwrap(), folder);
        std::fs::remove_dir_all(parent).unwrap();
    }

    #[test]
    fn a_leased_scene_refuses_writes_from_other_agents() {
        let parent = std::env::temp_dir()
            .join(format!("rusting-lease-cli-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&parent).unwrap();
        create_project_from(&parent, "boxy", ProjectTemplate::Puzzle).unwrap();
        let root = parent.join("boxy");
        let scene = root.join("scenes/main.rscene");
        let hour = LeaseAction::Claim(std::time::Duration::from_secs(3600));

        assert!(lease(&scene, Some("alice"), hour).ok);
        let taken = lease(
            &root.join("scenes"),
            Some("bob"),
            LeaseAction::Claim(std::time::Duration::from_secs(60)),
        );
        assert_eq!(taken.diagnostics[0].code, "LEASE_HELD");
        assert!(taken.diagnostics[0].message.contains("`alice`"));
        assert_eq!(lease_list(&root).data["leases"][0]["holder"], "alice");
        // This test process is not alice, so its write is refused.
        let refused = merge_scene(&scene, &scene, &scene, &scene);
        assert_eq!(refused.diagnostics[0].code, "LEASE_HELD");

        assert!(lease(&scene, Some("alice"), LeaseAction::Release).ok);
        assert!(merge_scene(&scene, &scene, &scene, &scene).ok);
        std::fs::remove_dir_all(parent).unwrap();
    }

    #[test]
    fn merge_combines_scene_edits_and_names_real_conflicts() {
        let parent = std::env::temp_dir()
            .join(format!("rusting-merge-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&parent).unwrap();
        create_project_from(&parent, "boxy", ProjectTemplate::Puzzle).unwrap();
        let root = parent.join("boxy");
        assert!(std::fs::read_to_string(root.join(".gitattributes"))
            .unwrap()
            .contains("merge=rusting-scene"));
        let base = root.join("scenes/main.rscene");
        let scene: Value =
            serde_json::from_slice(&std::fs::read(&base).unwrap()).unwrap();
        let side = |name: &str, entity: usize, new_name: &str| {
            let mut scene = scene.clone();
            scene["entities"][entity]["name"] = json!(new_name);
            let path = parent.join(name);
            std::fs::write(&path, serde_json::to_vec(&scene).unwrap()).unwrap();
            path
        };
        let ours = side("ours.rscene", 0, "Ours");
        let theirs = side("theirs.rscene", 1, "Theirs");
        let output = parent.join("merged.rscene");

        let clean = merge_scene(&base, &ours, &theirs, &output);
        assert!(clean.ok, "{:?}", clean.diagnostics);
        let merged: Value =
            serde_json::from_slice(&std::fs::read(&output).unwrap()).unwrap();
        assert_eq!(merged["entities"][0]["name"], "Ours");
        assert_eq!(merged["entities"][1]["name"], "Theirs");

        let theirs = side("theirs.rscene", 0, "Theirs");
        let conflict = merge_scene(&base, &ours, &theirs, &output);
        assert!(!conflict.ok);
        assert_eq!(conflict.diagnostics[0].code, "SCENE_MERGE_CONFLICT");
        assert_eq!(conflict.data["conflicts"][0]["field"], "name");
        std::fs::remove_dir_all(parent).unwrap();
    }

    #[test]
    fn impact_lists_the_scenes_code_and_scenarios_a_change_touches() {
        let parent = std::env::temp_dir()
            .join(format!("rusting-impact-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&parent).unwrap();
        create_project_from(&parent, "boxy", ProjectTemplate::Puzzle).unwrap();
        let root = parent.join("boxy");
        std::fs::create_dir_all(root.join("assets/data")).unwrap();
        std::fs::write(root.join("assets/data/level.json"), "{}").unwrap();
        std::fs::write(
            root.join("src/extra.rs"),
            "fn f(scene: &GameScene) {\n    scene.load_text(\"data/level.json\");\n}\n",
        )
        .unwrap();

        let file = impact(&root, "data/level.json").data;
        assert_eq!(file["kind"], "file");
        assert_eq!(file["code"], json!([{"file": "src/extra.rs", "line": 2}]));
        assert_eq!(file["scenario_files"], json!(["tests/solve.json"]));

        let camera = impact(&root, "camera").data;
        assert_eq!(camera["kind"], "component");
        assert_eq!(camera["scenes"][0]["path"], "scenes/main.rscene");
        assert!(camera["scenes"][0]["entity_count"].as_u64().unwrap() > 0);
        assert_eq!(camera["scenario_files"], json!(["tests/solve.json"]));

        let none = impact(&root, "game.nothing_uses_this").data;
        assert_eq!(none["scenes"], json!([]));
        assert_eq!(none["code"], json!([]));
        assert_eq!(none["scenario_files"], json!([]));
        std::fs::remove_dir_all(parent).unwrap();
    }

    #[test]
    fn systems_name_who_writes_a_component() {
        let writers = system_access(Path::new("."), None, Some("Transform"));
        assert!(writers.ok);
        let systems = writers.data["systems"].as_array().unwrap();
        let named = |name: &str| systems.iter().find(|s| s["name"] == name);
        let face = named("player_face").unwrap();
        assert_eq!(face["stage"], "FixedUpdate");
        assert!(face["writes"]
            .as_array()
            .unwrap()
            .contains(&json!("Transform")));
        assert!(face["reads"]
            .as_array()
            .unwrap()
            .contains(&json!("FrameTime")));
        assert!(named("draw_hud").is_none());
        assert_eq!(named("propagate_transforms").unwrap()["all"], true);
        let all = system_access(Path::new("."), None, None).data["systems"]
            .as_array()
            .unwrap()
            .len();
        assert!(systems.len() < all);
    }

    #[test]
    fn systems_scan_game_code_for_who_writes_a_component() {
        let parent = std::env::temp_dir()
            .join(format!("rusting-systems-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&parent).unwrap();
        create_project_from(&parent, "boxy", ProjectTemplate::Puzzle).unwrap();
        let root = parent.join("boxy");
        std::fs::write(
            root.join("src/extra.rs"),
            "fn regen(mut q: Query<(Entity, &mut game::Health), With<Enemy>>, t: Res<Time>) {}\n\
             fn shove(scene: &mut GameScene<'_>) {\n    \
                 scene.object(\"Crate\").set_position([0.0; 3]);\n    \
                 scene.add_to_counter(\"moves\", 1);\n    \
                 let _ = scene.set_field(\"Boss\", \"/components/game.health/value\", json!(3));\n    \
                 let i = [1].iter().position(|x| *x == 1);\n\
             }\n\
             fn raw(world: &mut World) {}\n\
             fn plain(x: i32) -> i32 { x }\n",
        )
        .unwrap();
        let game = |writes: &str| -> Vec<Value> {
            let result = system_access(&root, None, Some(writes));
            assert!(result.ok);
            result.data["systems"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|s| s["stage"] == "Game" && s["file"] == "src/extra.rs")
                .cloned()
                .collect()
        };
        let health = game("Health");
        let regen = health.iter().find(|s| s["name"] == "regen").unwrap();
        assert_eq!(regen["line"], 1);
        assert_eq!(regen["reads"], json!(["Time"]));
        assert!(health
            .iter()
            .any(|s| s["name"] == "raw" && s["all"] == true));
        assert!(health.iter().all(|s| s["name"] != "shove"));
        let shove = &game("Transform")[0];
        assert_eq!(
            (shove["name"].as_str(), shove["line"].as_u64()),
            (Some("shove"), Some(2))
        );
        assert_eq!(shove["reads"], json!([]));
        assert_eq!(
            shove["writes"],
            json!(["Counter", "Transform", "game.health"])
        );
        assert!(game("game.health").iter().any(|s| s["name"] == "shove"));
        let all = system_access(&root, None, None).data["systems"].clone();
        assert!(all.as_array().unwrap().iter().all(|s| s["name"] != "plain"));
        std::fs::remove_dir_all(parent).unwrap();
    }

    #[test]
    fn project_summary_lists_scenes_code_and_components_within_budget() {
        let parent = std::env::temp_dir()
            .join(format!("rusting-summary-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&parent).unwrap();
        create_project_from(&parent, "boxy", ProjectTemplate::Puzzle).unwrap();
        let root = parent.join("boxy");
        std::fs::write(
            root.join("src/extra.rs"),
            "pub fn spawn_crate(\n    scene: &mut GameScene<'_>,\n) {\n    scene.spawn_prefab(\"props/crate.rscene\", \"Crate\", t);\n}\nfn helper() {}\n",
        )
        .unwrap();

        let full = project_summary(&root, usize::MAX);
        assert!(full.ok, "{:?}", full.diagnostics);
        let data = &full.data;
        assert_eq!(data["omitted"], 0);
        let main = &data["scenes"][0];
        assert_eq!(main["path"], "scenes/main.rscene");
        assert!(main["entities"].as_u64().unwrap() > 0);
        assert!(main["components"]
            .as_array()
            .unwrap()
            .contains(&json!("transform")));
        let extra = data["systems"]
            .as_array()
            .unwrap()
            .iter()
            .find(|system| system["file"] == "src/extra.rs")
            .unwrap();
        assert_eq!(extra["functions"], json!(["spawn_crate"]));
        assert_eq!(extra["assets"], json!(["props/crate.rscene"]));
        assert!(data["components"]
            .as_array()
            .unwrap()
            .iter()
            .any(|component| component["name"] == "camera"));

        let cut = project_summary(&root, 150);
        assert!(cut.data["omitted"].as_u64().unwrap() > 0);
        assert!(cut.data.to_string().len() < full.data.to_string().len());
        std::fs::remove_dir_all(parent).unwrap();
    }
}

/// Checks a mod folder's `mod.json` and packs the folder into the `.rmod`
/// archive `out`. `out` is always an argument, so --confine checks it.
pub fn pack_mod(folder: &Path, out: &Path) -> CliResult {
    match crate::mods::pack_mod(folder, out) {
        Ok(manifest) => CliResult::success(json!({
            "name": manifest.name,
            "version": manifest.version,
            "archive": out,
        })),
        Err(error) => {
            CliResult::failure(error.code, error.message, Some(error.path))
        }
    }
}

/// The mods in `<root>/mods` that a game there would load, in load order,
/// the files and scene fields more than one of them changes, and an error
/// diagnostic for each mod left out.
pub fn list_mods(root: &Path) -> CliResult {
    let set = crate::mods::load_mods(&root.join("mods"));
    let mods: Vec<Value> = set
        .mods
        .iter()
        .map(|loaded| {
            json!({
                "name": loaded.manifest.name,
                "version": loaded.manifest.version,
                "depends": loaded.manifest.depends,
                "path": loaded.path,
            })
        })
        .collect();
    CliResult {
        schema_version: 1,
        ok: set.errors.is_empty(),
        data: json!({
            "mods": mods,
            "conflicts": crate::mods::conflicts(root, &set.mods),
        }),
        diagnostics: set
            .errors
            .into_iter()
            .map(|error| Diagnostic {
                code: error.code,
                severity: "error",
                message: error.message,
                file: Some(error.path),
                ..Diagnostic::default()
            })
            .collect(),
    }
}
