//! Scene instancing: one scene placed inside another as a child subtree
//! that keeps a link to its source file.
//!
//! An object with a [`SceneInstance`] component is the instance root. On
//! load, the source scene's objects are added under it as *members*. Member
//! IDs are derived from the root ID and the source object's ID, so they are
//! the same on every load.
//!
//! Text saves drop the members and keep the root, its link, and the
//! *overrides*: a sparse JSON diff per member against a fresh copy of the
//! source. Loading applies the diff to the source as it is now, so a source
//! edit reaches every instance that did not override that property. Cooked
//! scenes and editor snapshots keep the members, so a shipped game needs no
//! source files and Undo does not re-read them.

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use bevy_ecs::component::Component;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use uuid::Uuid;

use super::scene_file::{
    absolutize_scene_assets, relativize_scene_assets, write_atomic,
};
use super::{read_scene_document, SceneDocument, SceneEntity, SceneIoError};

/// Registry name of the built-in scene instance link.
pub const SCENE_INSTANCE_COMPONENT: &str = "rusting.scene_instance";

/// Components-map key that marks an object added by an instance. It holds
/// `{"source": "<ID of the object in the source scene>"}`. It is handled by
/// the scene loader directly and is not a registered component.
pub const INSTANCE_MEMBER_KEY: &str = "rusting.instance_member";

/// Components-map key on an instance root whose members are already in the
/// document (editor snapshots and cooked scenes), so loading does not add
/// them again. Holds `{"source": path}`, the scene the members came from.
pub const INSTANCE_EXPANDED_KEY: &str = "rusting.instance_expanded";

/// Components-map key on an instance root in a text scene. Holds
/// `{"<source object ID>": diff}`, where the diff has the shape of the
/// saved object with only the changed fields, `{"$removed": true}` for a
/// removed map entry or component, and `null` for a deleted object.
pub const INSTANCE_OVERRIDES_KEY: &str = "rusting.instance_overrides";

const REMOVED: &str = "$removed";

/// Keys that the scene loader handles itself instead of the registry.
pub(super) fn is_instance_key(name: &str) -> bool {
    [
        INSTANCE_MEMBER_KEY,
        INSTANCE_EXPANDED_KEY,
        INSTANCE_OVERRIDES_KEY,
    ]
    .contains(&name)
}

/// Places the scene at `source` under this object when the scene loads. An
/// empty path places nothing.
#[derive(
    Component, Clone, Debug, Default, PartialEq, Serialize, Deserialize,
)]
#[serde(default)]
pub struct SceneInstance {
    /// The source scene. Saved relative to the scene that contains it.
    pub source: PathBuf,
}

/// Marks an object that a [`SceneInstance`] added. Text saves store only
/// how it differs from the source.
#[derive(Component, Clone, Copy, Debug, PartialEq, Eq)]
pub struct InstanceMember {
    /// The object's ID in the source scene.
    pub source_id: Uuid,
}

/// Marks an instance root whose members were added. See
/// `INSTANCE_EXPANDED_KEY`.
#[derive(Component, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstanceExpanded {
    /// The source scene at the time the members were added. When the link
    /// is changed afterwards, a save stores no overrides for the old
    /// members.
    pub source: PathBuf,
}

/// Reads the value stored under [`INSTANCE_EXPANDED_KEY`].
pub(super) fn parse_expanded(
    serialized: &str,
) -> Result<InstanceExpanded, SceneIoError> {
    serde_json::from_str(serialized).map_err(|error| SceneIoError::Component {
        name: INSTANCE_EXPANDED_KEY.to_owned(),
        message: error.to_string(),
    })
}

#[derive(Serialize, Deserialize)]
struct MemberMarker {
    source: Uuid,
}

/// The marker JSON stored under [`INSTANCE_MEMBER_KEY`].
pub(super) fn member_marker(member: InstanceMember) -> String {
    serde_json::to_string(&MemberMarker {
        source: member.source_id,
    })
    .expect("a UUID always serializes")
}

/// Reads a marker written by [`member_marker`].
pub(super) fn parse_member_marker(
    serialized: &str,
) -> Result<InstanceMember, SceneIoError> {
    let marker: MemberMarker =
        serde_json::from_str(serialized).map_err(|error| {
            SceneIoError::Component {
                name: INSTANCE_MEMBER_KEY.to_owned(),
                message: error.to_string(),
            }
        })?;
    Ok(InstanceMember {
        source_id: marker.source,
    })
}

pub(super) fn is_member(entity: &SceneEntity) -> bool {
    entity.components.contains_key(INSTANCE_MEMBER_KEY)
}

/// Whether loading will add members under this entity.
pub(super) fn is_unexpanded_root(entity: &SceneEntity) -> bool {
    !entity.components.contains_key(INSTANCE_EXPANDED_KEY)
        && instance_source(entity).is_some()
}

/// The source path of an entity's instance link, if it has a non-empty one.
pub(super) fn instance_source(entity: &SceneEntity) -> Option<PathBuf> {
    let serialized = entity.components.get(SCENE_INSTANCE_COMPONENT)?;
    let value: Value = serde_json::from_str(serialized).ok()?;
    let source = value.get("source")?.as_str()?;
    (!source.is_empty()).then(|| PathBuf::from(source))
}

/// Rewrites the source paths of an entity's instance link and expansion
/// record.
pub(super) fn map_instance_source(
    entity: &mut SceneEntity,
    convert: &mut impl FnMut(&Path) -> Result<PathBuf, SceneIoError>,
) -> Result<(), SceneIoError> {
    for key in [SCENE_INSTANCE_COMPONENT, INSTANCE_EXPANDED_KEY] {
        if let Some(serialized) = entity.components.get_mut(key) {
            map_source_field(serialized, convert)?;
        }
    }
    Ok(())
}

fn map_source_field(
    serialized: &mut String,
    convert: &mut impl FnMut(&Path) -> Result<PathBuf, SceneIoError>,
) -> Result<(), SceneIoError> {
    let Ok(mut value) = serde_json::from_str::<Value>(serialized) else {
        return Ok(());
    };
    let Some(Value::String(source)) = value.get_mut("source") else {
        return Ok(());
    };
    if source.is_empty() {
        return Ok(());
    }
    let converted = convert(Path::new(source.as_str()))?;
    *source = crate::runtime::portable_path(&converted);
    *serialized = value.to_string();
    Ok(())
}

/// The ID of the object that instance `root` creates for source object
/// `source`. FNV-1a over both IDs, so it is stable across runs, platforms
/// and releases.
#[must_use]
pub fn member_id(root: Uuid, source: Uuid) -> Uuid {
    let hash = |seed: u64| {
        root.as_bytes().iter().chain(source.as_bytes()).fold(
            seed,
            |hash, &byte| {
                (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3)
            },
        )
    };
    let high = hash(0xcbf2_9ce4_8422_2325);
    let low = hash(0x8422_2325_cbf2_9ce4);
    let mut bytes = [0; 16];
    bytes[..8].copy_from_slice(&high.to_be_bytes());
    bytes[8..].copy_from_slice(&low.to_be_bytes());
    uuid::Builder::from_custom_bytes(bytes).into_uuid()
}

/// Rewrites the asset paths inside an instance root's overrides. Paths in
/// component handles (`$asset`) are converted by the caller's generic pass.
pub(super) fn map_override_paths(
    entity: &mut SceneEntity,
    convert: &mut impl FnMut(&Path) -> Result<PathBuf, SceneIoError>,
) -> Result<(), SceneIoError> {
    const PATHS: [&str; 7] = [
        "/mesh_renderer/mesh/AssetPath",
        "/mesh_renderer/material/Inline/base_color_texture",
        "/mesh_renderer/material/Inline/normal_texture",
        "/mesh_renderer/material/Inline/metallic_roughness_texture",
        "/mesh_renderer/material/Inline/occlusion_texture",
        "/mesh_renderer/material/Inline/emissive_texture",
        "/components/rusting.scene_instance/source",
    ];
    let Some(serialized) = entity.components.get_mut(INSTANCE_OVERRIDES_KEY)
    else {
        return Ok(());
    };
    let Ok(Value::Object(mut members)) = serde_json::from_str(serialized)
    else {
        return Ok(());
    };
    for diff in members.values_mut() {
        for pointer in PATHS {
            if let Some(Value::String(path)) = diff.pointer_mut(pointer) {
                if !path.is_empty() {
                    let converted = convert(Path::new(path.as_str()))?;
                    *path = crate::runtime::portable_path(&converted);
                }
            }
        }
    }
    *serialized = Value::Object(members).to_string();
    Ok(())
}

fn wrap_instance_error(source: &Path, error: SceneIoError) -> SceneIoError {
    match error {
        SceneIoError::InstanceCycle(_) | SceneIoError::Instance { .. } => error,
        error => SceneIoError::Instance {
            path: source.to_owned(),
            error: Box::new(error),
        },
    }
}

/// Adds the members of every instance root that has none yet. `document`
/// asset paths must already be absolute. `stack` holds the source files
/// being expanded, to reject a scene that contains itself.
pub(super) fn expand_instances<'a>(
    document: &'a SceneDocument,
    stack: &mut Vec<PathBuf>,
) -> Result<Cow<'a, SceneDocument>, SceneIoError> {
    let pending = document
        .entities
        .iter()
        .enumerate()
        .filter(|(_, entity)| {
            !entity.components.contains_key(INSTANCE_EXPANDED_KEY)
        })
        .filter_map(|(index, entity)| Some((index, instance_source(entity)?)))
        .collect::<Vec<_>>();
    if pending.is_empty() {
        return Ok(Cow::Borrowed(document));
    }
    let mut document = document.clone();
    for (index, source) in pending {
        let root = &mut document.entities[index];
        let overrides = root
            .components
            .remove(INSTANCE_OVERRIDES_KEY)
            .map(|text| serde_json::from_str::<Map<String, Value>>(&text))
            .transpose()
            .map_err(|error| SceneIoError::Component {
                name: INSTANCE_OVERRIDES_KEY.to_owned(),
                message: error.to_string(),
            })?
            .unwrap_or_default();
        root.components.insert(
            INSTANCE_EXPANDED_KEY.to_owned(),
            serde_json::to_string(&InstanceExpanded {
                source: source.clone(),
            })?,
        );
        let root = root.id;
        let mut members = instance_members(root, &source, stack)
            .map_err(|error| wrap_instance_error(&source, error))?;
        apply_overrides(&mut members, &overrides)?;
        document.entities.extend(members);
    }
    // An object added under an instance's object whose source no longer has
    // that object moves to the top of the scene instead of failing the load.
    detach_orphans(&mut document);
    Ok(Cow::Owned(document))
}

/// Replaces an instance's members in `document` with the overrides on
/// their roots, for a text save. Reads each source scene again to compare.
pub(super) fn fold_instance_members(
    document: &mut SceneDocument,
) -> Result<(), SceneIoError> {
    let current = document
        .entities
        .iter()
        .filter(|entity| is_member(entity))
        .map(|entity| (entity.id, entity))
        .collect::<HashMap<_, _>>();
    let mut folded = Vec::new();
    for root in document.entities.iter().filter(|entity| !is_member(entity)) {
        // A root that was never expanded has no members to compare.
        let (Some(source), Some(expanded)) = (
            instance_source(root),
            root.components.get(INSTANCE_EXPANDED_KEY),
        ) else {
            continue;
        };
        // After the link changed, the old members say nothing about the
        // new source.
        if parse_expanded(expanded)?.source != source {
            continue;
        }
        let baseline = instance_members(root.id, &source, &mut Vec::new())
            .map_err(|error| wrap_instance_error(&source, error))?;
        let mut overrides = Map::new();
        for base in &baseline {
            let diff = match current.get(&base.id) {
                None => Some(Value::Null),
                Some(now) => diff(&entity_tree(base)?, &entity_tree(now)?),
            };
            if let Some(diff) = diff {
                let source_id =
                    parse_member_marker(&base.components[INSTANCE_MEMBER_KEY])?
                        .source_id;
                overrides.insert(source_id.to_string(), diff);
            }
        }
        folded.push((
            root.id,
            (!overrides.is_empty())
                .then(|| Value::Object(overrides).to_string()),
        ));
    }
    let folded = folded.into_iter().collect::<HashMap<_, _>>();
    document.entities.retain(|entity| !is_member(entity));
    for entity in &mut document.entities {
        entity.components.remove(INSTANCE_EXPANDED_KEY);
        if let Some(Some(overrides)) = folded.get(&entity.id) {
            entity
                .components
                .insert(INSTANCE_OVERRIDES_KEY.to_owned(), overrides.clone());
        }
    }
    Ok(())
}

/// A saved object as JSON with its components parsed, so a diff can reach
/// single component fields.
fn entity_tree(entity: &SceneEntity) -> Result<Value, SceneIoError> {
    let mut tree = serde_json::to_value(entity)?;
    if let Some(Value::Object(components)) = tree.get_mut("components") {
        for value in components.values_mut() {
            if let Some(parsed) = value
                .as_str()
                .and_then(|text| serde_json::from_str(text).ok())
            {
                *value = parsed;
            }
        }
    }
    Ok(tree)
}

/// The inverse of [`entity_tree`].
fn tree_entity(mut tree: Value) -> Result<SceneEntity, SceneIoError> {
    if let Some(Value::Object(components)) = tree.get_mut("components") {
        for value in components.values_mut() {
            if !value.is_string() {
                *value = Value::String(value.to_string());
            }
        }
    }
    Ok(serde_json::from_value(tree)?)
}

/// The fields of `current` that differ from `base`, as a sparse copy of
/// `current`, or `None` when they are equal.
fn diff(base: &Value, current: &Value) -> Option<Value> {
    let (Value::Object(base), Value::Object(current)) = (base, current) else {
        return (base != current).then(|| current.clone());
    };
    let mut changed = Map::new();
    for (key, now) in current {
        let entry = match base.get(key) {
            Some(before) => diff(before, now),
            None => Some(now.clone()),
        };
        if let Some(entry) = entry {
            changed.insert(key.clone(), entry);
        }
    }
    for key in base.keys().filter(|key| !current.contains_key(*key)) {
        changed.insert(key.clone(), removed());
    }
    (!changed.is_empty()).then_some(Value::Object(changed))
}

fn removed() -> Value {
    serde_json::json!({ REMOVED: true })
}

/// Applies a [`diff`] to `target`.
fn merge(target: &mut Value, patch: &Value) {
    match (target, patch) {
        (Value::Object(target), Value::Object(patch))
            if *patch != *removed().as_object().expect("an object") =>
        {
            for (key, value) in patch {
                if *value == removed() {
                    target.remove(key);
                } else if let Some(existing) = target.get_mut(key) {
                    merge(existing, value);
                } else {
                    target.insert(key.clone(), value.clone());
                }
            }
        }
        (target, patch) => *target = patch.clone(),
    }
}

/// Applies saved overrides to fresh members. A deleted member takes its
/// source children with it.
fn apply_overrides(
    members: &mut Vec<SceneEntity>,
    overrides: &Map<String, Value>,
) -> Result<(), SceneIoError> {
    if overrides.is_empty() {
        return Ok(());
    }
    let mut deleted = HashSet::new();
    for member in members.iter_mut() {
        let source_id =
            parse_member_marker(&member.components[INSTANCE_MEMBER_KEY])?
                .source_id;
        match overrides.get(&source_id.to_string()) {
            None => {}
            Some(Value::Null) => {
                deleted.insert(member.id);
            }
            Some(patch) => {
                let id = member.id;
                let mut tree = entity_tree(member)?;
                merge(&mut tree, patch);
                *member = tree_entity(tree)?;
                member.id = id;
            }
        }
    }
    loop {
        let before = members.len();
        members.retain(|member| {
            let gone = deleted.contains(&member.id)
                || member
                    .parent
                    .is_some_and(|parent| deleted.contains(&parent));
            if gone {
                deleted.insert(member.id);
            }
            !gone
        });
        if members.len() == before {
            return Ok(());
        }
    }
}

/// The objects instance `root` adds for the scene at `source`.
fn instance_members(
    root: Uuid,
    source: &Path,
    stack: &mut Vec<PathBuf>,
) -> Result<Vec<SceneEntity>, SceneIoError> {
    Ok(place_members(root, read_source(source, stack)?))
}

/// The scene at `source` with absolute asset paths and its own instances
/// expanded.
fn read_source(
    source: &Path,
    stack: &mut Vec<PathBuf>,
) -> Result<SceneDocument, SceneIoError> {
    if stack.iter().any(|open| open == source) {
        return Err(SceneIoError::InstanceCycle(source.to_owned()));
    }
    let mut prefab = read_scene_document(source)?;
    if let Some(folder) = source.parent() {
        absolutize_scene_assets(&mut prefab, folder)?;
    }
    stack.push(source.to_owned());
    let prefab = expand_instances(&prefab, stack).map(Cow::into_owned);
    stack.pop();
    prefab
}

/// The objects of an expanded source scene as members of the instance at
/// `root`.
fn place_members(root: Uuid, prefab: SceneDocument) -> Vec<SceneEntity> {
    let ids = prefab
        .entities
        .iter()
        .map(|entity| (entity.id, member_id(root, entity.id)))
        .collect::<HashMap<_, _>>();
    let mut members = prefab.entities;
    for member in &mut members {
        remap_components(member, &ids);
        member.components.insert(
            INSTANCE_MEMBER_KEY.to_owned(),
            member_marker(InstanceMember {
                source_id: member.id,
            }),
        );
        member.parent = Some(member.parent.map_or(root, |parent| ids[&parent]));
        member.id = ids[&member.id];
    }
    members
}

/// A scene file loaded once for placing many times from gameplay code.
/// Load it with `AssetServer::load_prefab` and place it with
/// [`spawn_prefab`].
#[derive(Clone, Debug)]
pub struct Prefab {
    source: PathBuf,
    document: SceneDocument,
}

impl Prefab {
    /// The scene file this prefab was loaded from.
    #[must_use]
    pub fn source(&self) -> &Path {
        &self.source
    }
}

impl crate::AssetServer {
    /// Loads the text or cooked scene at `path`, with the instances inside
    /// it, once per path. An exported game ships the project's `assets`
    /// folder, so keep scenes placed at runtime there.
    pub fn load_prefab(
        &mut self,
        path: impl AsRef<Path>,
    ) -> Result<crate::Handle<Prefab>, crate::assets::AssetError> {
        self.prefabs.get_or_insert_with(path, |path| {
            let source =
                path.canonicalize().unwrap_or_else(|_| path.to_owned());
            let document =
                read_source(&source, &mut Vec::new()).map_err(|error| {
                    crate::assets::AssetError::Load {
                        path: path.to_owned(),
                        message: error.to_string(),
                    }
                })?;
            Ok(Prefab { source, document })
        })
    }
}

/// Counts runtime prefab placements, so each placement gets the same
/// object IDs in every run that places prefabs in the same order.
#[derive(bevy_ecs::prelude::Resource, Default)]
struct PrefabPlacements(u64);

/// Places a loaded prefab in the world as one new root object (a linked
/// [`SceneInstance`]) at `transform`, and returns that root. Object IDs come
/// from a per-world placement count, not a random source, so a replay
/// places the same IDs. From a system, queue it:
/// `commands.queue(move |world: &mut World| spawn_prefab(world, coin, at).map(drop))`.
pub fn spawn_prefab(
    world: &mut bevy_ecs::world::World,
    prefab: crate::Handle<Prefab>,
    transform: crate::Transform,
) -> Result<bevy_ecs::entity::Entity, SceneIoError> {
    let prefab = world
        .resource::<crate::AssetServer>()
        .prefabs
        .get(prefab)
        .ok_or(SceneIoError::MissingPrefab(prefab.key()))?
        .clone();
    let count = {
        let mut placements = world.get_resource_or_init::<PrefabPlacements>();
        placements.0 += 1;
        placements.0
    };
    // Never a saved scene's random ID: those are version 4 UUIDs.
    let root = member_id(Uuid::nil(), Uuid::from_u128(u128::from(count)));
    let name = prefab
        .source
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("Prefab");
    // Scene saves need unique names; the count keeps them apart.
    let name = format!("{name} #{count}");
    let root_entity: SceneEntity = serde_json::from_value(serde_json::json!({
        "id": root,
        "name": name,
        "transform": super::SceneTransform::from(transform),
        "components": {
            SCENE_INSTANCE_COMPONENT: serde_json::to_string(&SceneInstance {
                source: prefab.source.clone(),
            })?,
            INSTANCE_EXPANDED_KEY: serde_json::to_string(&InstanceExpanded {
                source: prefab.source.clone(),
            })?,
        },
    }))?;
    let mut entities = vec![root_entity];
    entities.extend(place_members(root, prefab.document.clone()));
    let document = SceneDocument {
        entities,
        ..prefab.document
    };
    super::load_scene_document(
        world,
        &document,
        super::SceneLoadMode::Additive,
    )?;
    // ponytail: one scan over scene objects per placement; return the
    // spawned map from the loader if games place thousands per frame.
    let mut query =
        world.query::<(bevy_ecs::entity::Entity, &super::SceneId)>();
    query
        .iter(world)
        .find_map(|(entity, id)| (id.0 == root).then_some(entity))
        .ok_or(SceneIoError::MissingParent(root))
}

/// Rewrites the object IDs inside `entity`'s components through `ids`.
fn remap_components(entity: &mut SceneEntity, ids: &HashMap<Uuid, Uuid>) {
    for (name, serialized) in &mut entity.components {
        if name == INSTANCE_MEMBER_KEY {
            continue;
        }
        // Only components that mention a mapped ID need rewriting.
        if !ids.keys().any(|id| serialized.contains(&id.to_string())) {
            continue;
        }
        let Ok(mut value) = serde_json::from_str::<Value>(serialized) else {
            continue;
        };
        remap_ids(&mut value, ids);
        *serialized = value.to_string();
    }
}

/// An operation on a placed scene instance. [`edit_instance`] applies it to
/// a [`scene_document`](super::scene_document) of the open scene, which the
/// caller loads back (after taking an undo snapshot).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InstanceEdit {
    /// Resets one placed object to its values in the source scene.
    RevertObject,
    /// Resets every placed object of the instance and brings back deleted
    /// ones. Objects added under them are kept.
    Revert,
    /// Writes the instance's objects, and objects added under it, into the
    /// source scene. Other instances of that source pick up the change and
    /// keep their own overrides.
    ApplyToSource,
    /// Turns the instance and the instances inside it into ordinary objects
    /// with no link. Names that now clash get a number.
    UnpackCompletely,
}

/// Applies `edit` to the instance that placed the object `id` (for
/// [`InstanceEdit::RevertObject`], to that object alone). `id` may be the
/// instance root or any object it placed. `document` paths must be
/// absolute, as they are in a document built from the world.
pub fn edit_instance(
    document: &mut SceneDocument,
    id: Uuid,
    edit: InstanceEdit,
) -> Result<(), SceneIoError> {
    let (root, source) = owning_instance(document, id)?;
    let members = members_of(document, root);
    let baseline = || {
        instance_members(root, &source, &mut Vec::new())
            .map_err(|error| wrap_instance_error(&source, error))
    };
    match edit {
        InstanceEdit::RevertObject => {
            if !members.contains(&id) {
                return Err(SceneIoError::NotAnInstance(id));
            }
            // An object the current source no longer has stays as it is.
            if let Some(base) =
                baseline()?.into_iter().find(|base| base.id == id)
            {
                let index = document
                    .entities
                    .iter()
                    .position(|entity| entity.id == id)
                    .expect("members come from the document");
                document.entities[index] = base;
            }
        }
        InstanceEdit::Revert => {
            let baseline = baseline()?;
            document
                .entities
                .retain(|entity| !members.contains(&entity.id));
            document.entities.extend(baseline);
        }
        InstanceEdit::ApplyToSource => {
            apply_to_source(document, root, &source, &members)?;
        }
        InstanceEdit::UnpackCompletely => unpack(document, root, &members),
    }
    detach_orphans(document);
    Ok(())
}

/// The instance root that placed `id` (the nearest ancestor-or-self that is
/// not a member), with its source. Fails unless that root is an expanded
/// instance.
fn owning_instance(
    document: &SceneDocument,
    id: Uuid,
) -> Result<(Uuid, PathBuf), SceneIoError> {
    let by_id = document
        .entities
        .iter()
        .map(|entity| (entity.id, entity))
        .collect::<HashMap<_, _>>();
    let root = owner(&by_id, id).ok_or(SceneIoError::NotAnInstance(id))?;
    let root = by_id[&root];
    match (
        instance_source(root),
        root.components.get(INSTANCE_EXPANDED_KEY),
    ) {
        (Some(source), Some(_)) => Ok((root.id, source)),
        _ => Err(SceneIoError::NotAnInstance(id)),
    }
}

/// The nearest ancestor-or-self of `id` that is not an instance member.
fn owner(by_id: &HashMap<Uuid, &SceneEntity>, id: Uuid) -> Option<Uuid> {
    let mut current = *by_id.get(&id)?;
    while is_member(current) {
        current = by_id.get(&current.parent?)?;
    }
    Some(current.id)
}

/// The objects that the instance at `root` placed, nested ones included.
fn members_of(document: &SceneDocument, root: Uuid) -> HashSet<Uuid> {
    let by_id = document
        .entities
        .iter()
        .map(|entity| (entity.id, entity))
        .collect::<HashMap<_, _>>();
    document
        .entities
        .iter()
        .filter(|entity| {
            is_member(entity) && owner(&by_id, entity.id) == Some(root)
        })
        .map(|entity| entity.id)
        .collect()
}

/// Every object below `root`, at any depth.
fn descendants(document: &SceneDocument, root: Uuid) -> HashSet<Uuid> {
    let mut found = HashSet::from([root]);
    loop {
        let count = found.len();
        for entity in &document.entities {
            if entity.parent.is_some_and(|parent| found.contains(&parent)) {
                found.insert(entity.id);
            }
        }
        if found.len() == count {
            found.remove(&root);
            return found;
        }
    }
}

/// Moves objects whose parent is gone to the top of the scene.
fn detach_orphans(document: &mut SceneDocument) {
    let ids = document
        .entities
        .iter()
        .map(|entity| entity.id)
        .collect::<HashSet<_>>();
    for entity in &mut document.entities {
        if entity.parent.is_some_and(|parent| !ids.contains(&parent)) {
            entity.parent = None;
        }
    }
}

fn apply_to_source(
    document: &mut SceneDocument,
    root: Uuid,
    source: &Path,
    members: &HashSet<Uuid>,
) -> Result<(), SceneIoError> {
    let below = descendants(document, root);
    // The source as it expands on its own, for the member markers of the
    // instances inside it.
    let mut written = read_scene_document(source)?;
    let folder = source.parent().unwrap_or(Path::new(""));
    absolutize_scene_assets(&mut written, folder)?;
    let markers = expand_instances(&written, &mut vec![source.to_owned()])?
        .entities
        .iter()
        .map(|entity| {
            (
                entity.id,
                entity.components.get(INSTANCE_MEMBER_KEY).cloned(),
            )
        })
        .collect::<HashMap<_, _>>();
    let mut ids = HashMap::new();
    for entity in &document.entities {
        let source_id = match entity.components.get(INSTANCE_MEMBER_KEY) {
            Some(marker) if members.contains(&entity.id) => {
                parse_member_marker(marker)?.source_id
            }
            _ => entity.id,
        };
        ids.insert(entity.id, source_id);
    }
    written.entities = Vec::new();
    for entity in document.entities.iter().filter(|e| below.contains(&e.id)) {
        let mut entity = entity.clone();
        if members.contains(&entity.id) {
            entity.components.remove(INSTANCE_MEMBER_KEY);
            if let Some(Some(marker)) = markers.get(&ids[&entity.id]) {
                entity
                    .components
                    .insert(INSTANCE_MEMBER_KEY.to_owned(), marker.clone());
            }
        }
        remap_components(&mut entity, &ids);
        entity.id = ids[&entity.id];
        entity.parent = entity
            .parent
            .filter(|&parent| parent != root)
            .map(|parent| ids[&parent]);
        written.entities.push(entity);
    }
    fold_instance_members(&mut written)?;
    // Objects added under the instance may place a scene that contains the
    // source; expanding once rejects that before anything is written.
    super::validate_scene_structure(&written)?;
    let expanded = expand_instances(&written, &mut vec![source.to_owned()])?;
    super::validate_scene_structure(&expanded)?;

    // The rest of the open scene keeps its overrides against the old
    // source, and this instance keeps none.
    let mut open = document.clone();
    open.entities.retain(|entity| !below.contains(&entity.id));
    for entity in &mut open.entities {
        if entity.id == root {
            entity.components.remove(INSTANCE_EXPANDED_KEY);
        }
    }
    fold_instance_members(&mut open)?;

    relativize_scene_assets(&mut written, folder)?;
    write_atomic(source, &serde_json::to_vec_pretty(&written)?)?;
    *document = expand_instances(&open, &mut Vec::new())?.into_owned();
    Ok(())
}

fn unpack(document: &mut SceneDocument, root: Uuid, members: &HashSet<Uuid>) {
    let mut names = document
        .entities
        .iter()
        .filter(|entity| !members.contains(&entity.id))
        .filter_map(|entity| entity.name.clone())
        .collect::<HashSet<_>>();
    for entity in &mut document.entities {
        if entity.id == root || members.contains(&entity.id) {
            for key in [
                SCENE_INSTANCE_COMPONENT,
                INSTANCE_MEMBER_KEY,
                INSTANCE_EXPANDED_KEY,
                INSTANCE_OVERRIDES_KEY,
            ] {
                entity.components.remove(key);
            }
        }
        if !members.contains(&entity.id) {
            continue;
        }
        if let Some(name) = &mut entity.name {
            let base = name.clone();
            let mut number = 2;
            while names.contains(name.as_str()) {
                *name = format!("{base} {number}");
                number += 1;
            }
            names.insert(name.clone());
        }
    }
}

/// Replaces every string in `value` that is a source object ID with the
/// matching member ID, so object references inside the source scene point
/// at this instance's copies.
fn remap_ids(value: &mut Value, ids: &HashMap<Uuid, Uuid>) {
    match value {
        Value::String(text) => {
            if let Some(mapped) =
                Uuid::parse_str(text).ok().and_then(|id| ids.get(&id))
            {
                *text = mapped.to_string();
            }
        }
        Value::Array(items) => {
            items.iter_mut().for_each(|item| remap_ids(item, ids));
        }
        Value::Object(fields) => {
            fields.values_mut().for_each(|field| remap_ids(field, ids));
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use bevy_ecs::entity::Entity;

    use super::*;
    use crate::runtime::{
        cook_scene, load_scene, save_scene, scene_document, App, Children,
        Name, SceneId, SceneLoadMode,
    };
    use crate::Transform;

    #[derive(
        Component, Clone, Debug, Default, Serialize, Deserialize, PartialEq,
    )]
    struct Follow {
        target: Option<Entity>,
    }

    crate::reflect! {
        struct Follow {
            target: Option<Entity>,
        }
    }

    fn app() -> App {
        let mut app = App::new();
        app.add_plugin(crate::AssetPlugin).unwrap();
        app.register_scene_component::<Follow>("test.follow")
            .unwrap();
        app
    }

    fn folder(test: &str) -> PathBuf {
        let folder = std::env::temp_dir()
            .join(format!("rusting-instance-{test}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&folder);
        std::fs::create_dir_all(folder.join("prefabs")).unwrap();
        folder
    }

    /// Saves `prefabs/coin.rscene`: a Coin with a Sparkle child that
    /// follows it.
    fn save_coin(folder: &Path) -> PathBuf {
        let mut app = app();
        let coin = app.spawn((
            SceneId::new(),
            Name("Coin".into()),
            Transform::default(),
        ));
        let sparkle = app.spawn((
            SceneId::new(),
            Name("Sparkle".into()),
            Transform::default(),
            Follow { target: Some(coin) },
        ));
        crate::runtime::hierarchy::set_parent(app.world_mut(), sparkle, coin)
            .unwrap();
        let path = folder.join("prefabs/coin.rscene");
        save_scene(app.world_mut(), &path, "Coin").unwrap();
        path
    }

    /// Saves a scene holding one instance root per source.
    fn save_with_instances(path: &Path, sources: &[(&str, &Path)]) {
        let mut app = app();
        for (name, source) in sources {
            app.spawn((
                SceneId::new(),
                Name((*name).into()),
                Transform::default(),
                SceneInstance {
                    source: source.to_path_buf(),
                },
            ));
        }
        save_scene(app.world_mut(), path, "Main").unwrap();
    }

    fn named(app: &mut App, name: &str) -> Vec<Entity> {
        let world = app.world_mut();
        let mut query = world.query::<(Entity, &Name)>();
        query
            .iter(world)
            .filter(|(_, found)| found.0 == name)
            .map(|(entity, _)| entity)
            .collect()
    }

    fn scene_id(app: &App, entity: Entity) -> Uuid {
        app.world().get::<SceneId>(entity).unwrap().0
    }

    #[test]
    fn instances_add_the_source_scene_under_the_root_with_stable_ids() {
        let folder = folder("expand");
        let coin = save_coin(&folder);
        let main = folder.join("main.rscene");
        save_with_instances(&main, &[("Left", &coin), ("Right", &coin)]);

        let mut app = app();
        load_scene(app.world_mut(), &main, SceneLoadMode::Replace).unwrap();
        let coins = named(&mut app, "Coin");
        let sparkles = named(&mut app, "Sparkle");
        assert_eq!((coins.len(), sparkles.len()), (2, 2));
        let ids = coins
            .iter()
            .chain(&sparkles)
            .map(|&entity| scene_id(&app, entity))
            .collect::<std::collections::BTreeSet<_>>();

        for &sparkle in &sparkles {
            // The reference points at this instance's own Coin.
            let target = app.world().get::<Follow>(sparkle).unwrap().target;
            let parent = app
                .world()
                .get::<crate::runtime::Parent>(sparkle)
                .unwrap()
                .0;
            assert_eq!(target, Some(parent));
            let member = *app.world().get::<InstanceMember>(sparkle).unwrap();
            let root =
                app.world().get::<crate::runtime::Parent>(parent).unwrap().0;
            assert!(app.world().get::<SceneInstance>(root).is_some());
            assert_eq!(
                scene_id(&app, sparkle),
                member_id(scene_id(&app, root), member.source_id)
            );
        }

        let mut again = self::app();
        load_scene(again.world_mut(), &main, SceneLoadMode::Replace).unwrap();
        let reloaded = named(&mut again, "Coin")
            .into_iter()
            .chain(named(&mut again, "Sparkle"))
            .map(|entity| scene_id(&again, entity))
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(reloaded, ids);
        std::fs::remove_dir_all(folder).unwrap();
    }

    #[test]
    fn saves_keep_the_link_and_snapshots_keep_the_members_once() {
        let folder = folder("save");
        let coin = save_coin(&folder);
        let main = folder.join("main.rscene");
        save_with_instances(&main, &[("Spawner", &coin)]);
        let mut app = app();
        load_scene(app.world_mut(), &main, SceneLoadMode::Replace).unwrap();

        // An editor snapshot holds the members and does not add them again.
        let snapshot = scene_document(app.world_mut(), "Main").unwrap();
        assert_eq!(snapshot.entities.len(), 3);
        crate::runtime::load_scene_document(
            app.world_mut(),
            &snapshot,
            SceneLoadMode::Replace,
        )
        .unwrap();
        assert_eq!(named(&mut app, "Sparkle").len(), 1);

        save_scene(app.world_mut(), &main, "Main").unwrap();
        let saved = crate::runtime::read_scene_document(&main).unwrap();
        assert_eq!(saved.entities.len(), 1);
        assert_eq!(
            instance_source(&saved.entities[0]),
            Some(PathBuf::from("prefabs").join("coin.rscene"))
        );
        std::fs::remove_dir_all(folder).unwrap();
    }

    #[test]
    fn cooked_scenes_load_without_the_source_scene() {
        let folder = folder("cook");
        let coin = save_coin(&folder);
        let main = folder.join("main.rscene");
        save_with_instances(&main, &[("Spawner", &coin)]);
        let cooked = folder.join("build/main.rscene");
        cook_scene(&main, &cooked).unwrap();
        std::fs::remove_dir_all(folder.join("prefabs")).unwrap();

        let mut app = app();
        load_scene(app.world_mut(), &cooked, SceneLoadMode::Replace).unwrap();
        let [sparkle] = named(&mut app, "Sparkle")[..] else {
            panic!("expected one Sparkle");
        };
        assert!(app.world().get::<InstanceMember>(sparkle).is_some());
        std::fs::remove_dir_all(folder).unwrap();
    }

    #[test]
    fn instances_nest_and_a_scene_cannot_contain_itself() {
        let folder = folder("nest");
        let coin = save_coin(&folder);
        let row = folder.join("prefabs/row.rscene");
        save_with_instances(&row, &[("A", &coin), ("B", &coin)]);
        let main = folder.join("main.rscene");
        save_with_instances(&main, &[("Row 1", &row), ("Row 2", &row)]);

        let mut app = app();
        load_scene(app.world_mut(), &main, SceneLoadMode::Replace).unwrap();
        assert_eq!(named(&mut app, "Sparkle").len(), 4);
        let a = named(&mut app, "A")[0];
        let children = app.world().get::<Children>(a).unwrap();
        assert_eq!(children.0.len(), 1);

        let looped = folder.join("prefabs/loop.rscene");
        save_with_instances(&looped, &[("Self", &looped)]);
        let mut app = self::app();
        let error =
            load_scene(app.world_mut(), &looped, SceneLoadMode::Replace)
                .unwrap_err();
        assert!(
            matches!(&error, SceneIoError::InstanceCycle(path) if *path == looped),
            "{error}"
        );
        std::fs::remove_dir_all(folder).unwrap();
    }

    #[test]
    fn a_missing_source_fails_the_load_and_keeps_the_open_scene() {
        let folder = folder("missing");
        let main = folder.join("main.rscene");
        save_with_instances(&main, &[("Spawner", &folder.join("gone.rscene"))]);
        let mut app = app();
        app.spawn((SceneId::new(), Name("Open".into())));
        let error = load_scene(app.world_mut(), &main, SceneLoadMode::Replace)
            .unwrap_err();
        assert!(
            matches!(&error, SceneIoError::Instance { path, .. }
                if path.ends_with("gone.rscene")),
            "{error}"
        );
        assert_eq!(named(&mut app, "Open").len(), 1);
        assert!(named(&mut app, "Spawner").is_empty());
        std::fs::remove_dir_all(folder).unwrap();
    }

    #[test]
    fn an_empty_source_places_nothing() {
        let mut app = app();
        app.spawn((SceneId::new(), SceneInstance::default()));
        let document = scene_document(app.world_mut(), "Main").unwrap();
        assert!(matches!(
            expand_instances(&document, &mut Vec::new()),
            Ok(Cow::Borrowed(_))
        ));
    }

    /// The member called `name` under the instance root called `root`.
    fn member(app: &mut App, root: &str, name: &str) -> Option<Entity> {
        let root = named(app, root)[0];
        named(app, name).into_iter().find(|&entity| {
            let mut current = entity;
            while let Some(parent) =
                app.world().get::<crate::runtime::Parent>(current)
            {
                if parent.0 == root {
                    return true;
                }
                current = parent.0;
            }
            false
        })
    }

    #[test]
    fn overrides_are_saved_and_source_edits_reach_the_rest() {
        let folder = folder("overrides");
        let coin = save_coin(&folder);
        let main = folder.join("main.rscene");
        save_with_instances(&main, &[("Left", &coin), ("Right", &coin)]);
        let mut app = app();
        load_scene(app.world_mut(), &main, SceneLoadMode::Replace).unwrap();

        // Left: move the Sparkle, drop its Follow, and add an object under
        // the Coin. Right: delete the Sparkle.
        let sparkle = member(&mut app, "Left", "Sparkle").unwrap();
        app.world_mut()
            .get_mut::<Transform>(sparkle)
            .unwrap()
            .position = [0.0, 3.0, 0.0];
        app.world_mut().entity_mut(sparkle).remove::<Follow>();
        let left_coin = member(&mut app, "Left", "Coin").unwrap();
        let extra = app.spawn((SceneId::new(), Name("Extra".into())));
        crate::runtime::hierarchy::set_parent(
            app.world_mut(),
            extra,
            left_coin,
        )
        .unwrap();
        let right_sparkle = member(&mut app, "Right", "Sparkle").unwrap();
        app.world_mut().despawn(right_sparkle);
        save_scene(app.world_mut(), &main, "Main").unwrap();

        let saved = crate::runtime::read_scene_document(&main).unwrap();
        assert_eq!(saved.entities.len(), 3, "Left, Right and Extra");
        let text = std::fs::read_to_string(&main).unwrap();
        assert!(!text.contains(INSTANCE_EXPANDED_KEY));

        // Edit the source: move the Sparkle and scale the Coin.
        let mut source: Value =
            serde_json::from_slice(&std::fs::read(&coin).unwrap()).unwrap();
        for entity in source["entities"].as_array_mut().unwrap() {
            match entity["name"].as_str().unwrap() {
                "Coin" => {
                    entity["transform"]["scale"] =
                        serde_json::json!([2.0, 2.0, 2.0])
                }
                _ => {
                    entity["transform"]["position"] =
                        serde_json::json!([5.0, 0.0, 0.0])
                }
            }
        }
        std::fs::write(&coin, source.to_string()).unwrap();

        let mut app = self::app();
        load_scene(app.world_mut(), &main, SceneLoadMode::Replace).unwrap();
        let sparkle = member(&mut app, "Left", "Sparkle").unwrap();
        assert_eq!(
            app.world().get::<Transform>(sparkle).unwrap().position,
            [0.0, 3.0, 0.0],
            "the override wins"
        );
        assert!(app.world().get::<Follow>(sparkle).is_none());
        assert!(member(&mut app, "Right", "Sparkle").is_none());
        for root in ["Left", "Right"] {
            let coin = member(&mut app, root, "Coin").unwrap();
            assert_eq!(
                app.world().get::<Transform>(coin).unwrap().scale,
                [2.0, 2.0, 2.0],
                "the source edit reaches {root}"
            );
        }
        let extra = named(&mut app, "Extra")[0];
        let left_coin = member(&mut app, "Left", "Coin").unwrap();
        assert_eq!(
            app.world().get::<crate::runtime::Parent>(extra).unwrap().0,
            left_coin
        );

        // A source that drops the Coin leaves Extra at the top of the scene.
        source["entities"] = serde_json::json!([]);
        std::fs::write(&coin, source.to_string()).unwrap();
        let mut app = self::app();
        load_scene(app.world_mut(), &main, SceneLoadMode::Replace).unwrap();
        let extra = named(&mut app, "Extra")[0];
        assert!(app.world().get::<crate::runtime::Parent>(extra).is_none());
        std::fs::remove_dir_all(folder).unwrap();
    }

    #[test]
    fn a_changed_link_saves_no_overrides_for_the_old_members() {
        let folder = folder("relink");
        let coin = save_coin(&folder);
        let main = folder.join("main.rscene");
        save_with_instances(&main, &[("Spawner", &coin)]);
        let mut app = app();
        load_scene(app.world_mut(), &main, SceneLoadMode::Replace).unwrap();
        let other = folder.join("prefabs/other.rscene");
        save_with_instances(&other, &[]);
        let spawner = named(&mut app, "Spawner")[0];
        app.world_mut()
            .get_mut::<SceneInstance>(spawner)
            .unwrap()
            .source = other.clone();
        save_scene(app.world_mut(), &main, "Main").unwrap();
        let saved = crate::runtime::read_scene_document(&main).unwrap();
        assert_eq!(saved.entities.len(), 1);
        assert!(!saved.entities[0]
            .components
            .contains_key(INSTANCE_OVERRIDES_KEY));
        std::fs::remove_dir_all(folder).unwrap();
    }

    #[test]
    fn variants_inherit_base_edits_under_their_own_overrides() {
        let folder = folder("variant");
        let coin = save_coin(&folder);
        let gold = folder.join("prefabs/gold.rscene");
        crate::runtime::save_scene_variant(&coin, &gold).unwrap();

        // The variant scales its Coin.
        let mut app = app();
        load_scene(app.world_mut(), &gold, SceneLoadMode::Replace).unwrap();
        let variant_coin = member(&mut app, "coin", "Coin").unwrap();
        app.world_mut()
            .get_mut::<Transform>(variant_coin)
            .unwrap()
            .scale = [3.0, 3.0, 3.0];
        save_scene(app.world_mut(), &gold, "gold").unwrap();

        // A level places the variant and moves its Sparkle.
        let main = folder.join("main.rscene");
        save_with_instances(&main, &[("Spawner", &gold)]);
        let mut app = self::app();
        load_scene(app.world_mut(), &main, SceneLoadMode::Replace).unwrap();
        let sparkle = member(&mut app, "Spawner", "Sparkle").unwrap();
        app.world_mut()
            .get_mut::<Transform>(sparkle)
            .unwrap()
            .position = [0.0, 1.0, 0.0];
        save_scene(app.world_mut(), &main, "Main").unwrap();

        // The base moves the Coin, scales it, and moves the Sparkle.
        let mut source: Value =
            serde_json::from_slice(&std::fs::read(&coin).unwrap()).unwrap();
        for entity in source["entities"].as_array_mut().unwrap() {
            entity["transform"]["position"] =
                serde_json::json!([7.0, 0.0, 0.0]);
            entity["transform"]["scale"] = serde_json::json!([2.0, 2.0, 2.0]);
        }
        std::fs::write(&coin, source.to_string()).unwrap();

        let mut app = self::app();
        load_scene(app.world_mut(), &main, SceneLoadMode::Replace).unwrap();
        let coin_entity = member(&mut app, "Spawner", "Coin").unwrap();
        let transform = *app.world().get::<Transform>(coin_entity).unwrap();
        assert_eq!(transform.position, [7.0, 0.0, 0.0], "the base edit");
        assert_eq!(transform.scale, [3.0, 3.0, 3.0], "the variant wins");
        let sparkle = member(&mut app, "Spawner", "Sparkle").unwrap();
        let transform = *app.world().get::<Transform>(sparkle).unwrap();
        assert_eq!(transform.position, [0.0, 1.0, 0.0], "the level wins");
        assert_eq!(transform.scale, [2.0, 2.0, 2.0], "the base edit");

        // A variant of itself, or of a scene built from it, is rejected
        // without writing.
        let before = std::fs::read(&coin).unwrap();
        for (base, path) in [(&coin, &coin), (&gold, &coin)] {
            let error =
                crate::runtime::save_scene_variant(base, path).unwrap_err();
            assert!(
                matches!(&error, SceneIoError::InstanceCycle(_)),
                "{error}"
            );
        }
        assert_eq!(std::fs::read(&coin).unwrap(), before);
        std::fs::remove_dir_all(folder).unwrap();
    }

    /// Runs `edit` on `entity` the way the editor does: through a document
    /// of the world, loaded back in place.
    fn run_edit(
        app: &mut App,
        entity: Entity,
        edit: InstanceEdit,
    ) -> Result<(), SceneIoError> {
        let id = scene_id(app, entity);
        let mut document = scene_document(app.world_mut(), "Main")?;
        edit_instance(&mut document, id, edit)?;
        crate::runtime::load_scene_document(
            app.world_mut(),
            &document,
            SceneLoadMode::Replace,
        )?;
        Ok(())
    }

    fn position(app: &App, entity: Entity) -> [f32; 3] {
        app.world().get::<Transform>(entity).unwrap().position
    }

    #[test]
    fn revert_resets_one_object_or_the_whole_instance() {
        let folder = folder("revert");
        let coin = save_coin(&folder);
        let main = folder.join("main.rscene");
        save_with_instances(&main, &[("Left", &coin)]);
        let mut app = app();
        load_scene(app.world_mut(), &main, SceneLoadMode::Replace).unwrap();
        let sparkle = member(&mut app, "Left", "Sparkle").unwrap();
        app.world_mut()
            .get_mut::<Transform>(sparkle)
            .unwrap()
            .position = [0.0, 3.0, 0.0];
        let coin_entity = member(&mut app, "Left", "Coin").unwrap();
        app.world_mut()
            .get_mut::<Transform>(coin_entity)
            .unwrap()
            .position = [1.0, 0.0, 0.0];
        let extra = app.spawn((SceneId::new(), Name("Extra".into())));
        crate::runtime::hierarchy::set_parent(
            app.world_mut(),
            extra,
            coin_entity,
        )
        .unwrap();

        run_edit(&mut app, sparkle, InstanceEdit::RevertObject).unwrap();
        let sparkle = member(&mut app, "Left", "Sparkle").unwrap();
        assert_eq!(position(&app, sparkle), [0.0; 3]);
        let coin_entity = member(&mut app, "Left", "Coin").unwrap();
        assert_eq!(position(&app, coin_entity), [1.0, 0.0, 0.0]);

        app.world_mut().despawn(sparkle);
        let left = named(&mut app, "Left")[0];
        run_edit(&mut app, left, InstanceEdit::Revert).unwrap();
        let coin_entity = member(&mut app, "Left", "Coin").unwrap();
        assert_eq!(position(&app, coin_entity), [0.0; 3]);
        let sparkle = member(&mut app, "Left", "Sparkle").unwrap();
        let target = app.world().get::<Follow>(sparkle).unwrap().target;
        assert_eq!(target, Some(coin_entity), "the deleted Sparkle is back");
        let extra = named(&mut app, "Extra")[0];
        assert_eq!(
            app.world().get::<crate::runtime::Parent>(extra).unwrap().0,
            coin_entity,
            "added objects stay"
        );

        let left = named(&mut app, "Left")[0];
        assert!(matches!(
            run_edit(&mut app, left, InstanceEdit::RevertObject),
            Err(SceneIoError::NotAnInstance(_))
        ));
        assert!(matches!(
            run_edit(&mut app, extra, InstanceEdit::Revert),
            Err(SceneIoError::NotAnInstance(_))
        ));
        std::fs::remove_dir_all(folder).unwrap();
    }

    #[test]
    fn apply_writes_the_source_and_other_instances_keep_their_overrides() {
        let folder = folder("apply");
        let coin = save_coin(&folder);
        let main = folder.join("main.rscene");
        save_with_instances(&main, &[("Left", &coin), ("Right", &coin)]);
        let mut app = app();
        load_scene(app.world_mut(), &main, SceneLoadMode::Replace).unwrap();
        let right_sparkle = member(&mut app, "Right", "Sparkle").unwrap();
        app.world_mut()
            .get_mut::<Transform>(right_sparkle)
            .unwrap()
            .position = [0.0, 9.0, 0.0];
        let left_coin = member(&mut app, "Left", "Coin").unwrap();
        app.world_mut()
            .get_mut::<Transform>(left_coin)
            .unwrap()
            .position = [4.0, 0.0, 0.0];
        let extra = app.spawn((
            SceneId::new(),
            Name("Extra".into()),
            Follow {
                target: Some(left_coin),
            },
        ));
        crate::runtime::hierarchy::set_parent(
            app.world_mut(),
            extra,
            left_coin,
        )
        .unwrap();

        run_edit(&mut app, left_coin, InstanceEdit::ApplyToSource).unwrap();
        let written = crate::runtime::read_scene_document(&coin).unwrap();
        assert_eq!(written.entities.len(), 3, "Coin, Sparkle and Extra");
        for root in ["Left", "Right"] {
            let coin_entity = member(&mut app, root, "Coin").unwrap();
            assert_eq!(position(&app, coin_entity), [4.0, 0.0, 0.0], "{root}");
            let extra = member(&mut app, root, "Extra").unwrap();
            let target = app.world().get::<Follow>(extra).unwrap().target;
            assert_eq!(target, Some(coin_entity), "{root}");
        }
        let right_sparkle = member(&mut app, "Right", "Sparkle").unwrap();
        assert_eq!(position(&app, right_sparkle), [0.0, 9.0, 0.0]);
        save_scene(app.world_mut(), &main, "Main").unwrap();
        let saved = crate::runtime::read_scene_document(&main).unwrap();
        let mut overrides = saved
            .entities
            .iter()
            .filter(|entity| {
                entity.components.contains_key(INSTANCE_OVERRIDES_KEY)
            })
            .filter_map(|entity| entity.name.as_deref());
        assert_eq!(overrides.next(), Some("Right"));
        assert_eq!(overrides.next(), None);

        // Applying to a scene of instances keeps them as linked instances.
        let row = folder.join("prefabs/row.rscene");
        save_with_instances(&row, &[("A", &coin), ("B", &coin)]);
        save_with_instances(&main, &[("Row", &row)]);
        let mut app = self::app();
        load_scene(app.world_mut(), &main, SceneLoadMode::Replace).unwrap();
        let a_coin = member(&mut app, "A", "Coin").unwrap();
        app.world_mut()
            .get_mut::<Transform>(a_coin)
            .unwrap()
            .position = [0.0, 0.0, 2.0];
        run_edit(&mut app, a_coin, InstanceEdit::ApplyToSource).unwrap();
        let written = crate::runtime::read_scene_document(&row).unwrap();
        assert_eq!(written.entities.len(), 2, "A and B stay instances");
        let mut app = self::app();
        load_scene(app.world_mut(), &main, SceneLoadMode::Replace).unwrap();
        let a_coin = member(&mut app, "A", "Coin").unwrap();
        assert_eq!(position(&app, a_coin), [0.0, 0.0, 2.0]);
        let b_coin = member(&mut app, "B", "Coin").unwrap();
        assert_eq!(position(&app, b_coin), [4.0, 0.0, 0.0]);
        std::fs::remove_dir_all(folder).unwrap();
    }

    #[test]
    fn unpack_turns_an_instance_into_ordinary_objects() {
        let folder = folder("unpack");
        let coin = save_coin(&folder);
        let main = folder.join("main.rscene");
        save_with_instances(&main, &[("Left", &coin), ("Right", &coin)]);
        let mut app = app();
        load_scene(app.world_mut(), &main, SceneLoadMode::Replace).unwrap();
        for root in ["Left", "Right"] {
            let root = named(&mut app, root)[0];
            run_edit(&mut app, root, InstanceEdit::UnpackCompletely).unwrap();
        }
        let mut names = {
            let world = app.world_mut();
            let mut query = world.query_filtered::<&Name, (
                bevy_ecs::query::Without<InstanceMember>,
                bevy_ecs::query::Without<SceneInstance>,
            )>();
            query
                .iter(world)
                .map(|name| name.0.clone())
                .collect::<Vec<_>>()
        };
        names.sort();
        assert_eq!(
            names,
            ["Coin", "Coin 2", "Left", "Right", "Sparkle", "Sparkle 2"]
        );
        let sparkle = named(&mut app, "Sparkle 2")[0];
        let coin_entity = named(&mut app, "Coin 2")[0];
        let target = app.world().get::<Follow>(sparkle).unwrap().target;
        assert_eq!(target, Some(coin_entity));

        // The unpacked objects save as themselves and no longer follow the
        // source.
        save_scene(app.world_mut(), &main, "Main").unwrap();
        std::fs::remove_file(&coin).unwrap();
        let mut app = self::app();
        load_scene(app.world_mut(), &main, SceneLoadMode::Replace).unwrap();
        assert_eq!(named(&mut app, "Coin 2").len(), 1);
        std::fs::remove_dir_all(folder).unwrap();
    }

    #[test]
    fn gameplay_code_places_loaded_prefabs_with_repeatable_ids() {
        let folder = folder("runtime");
        let coin = save_coin(&folder);
        let place = |folder: &Path| {
            let mut app = app();
            let handle = app
                .world_mut()
                .resource_mut::<crate::AssetServer>()
                .load_prefab(&coin)
                .unwrap();
            let again = app
                .world_mut()
                .resource_mut::<crate::AssetServer>()
                .load_prefab(&coin)
                .unwrap();
            assert_eq!(handle.key(), again.key(), "one load per path");
            let first = spawn_prefab(
                app.world_mut(),
                handle,
                Transform {
                    position: [1.0, 0.0, 0.0],
                    ..Transform::default()
                },
            )
            .unwrap();
            // Systems queue it; the source file is no longer needed.
            let _ = std::fs::rename(&coin, folder.join("moved.rscene"));
            app.world_mut().commands().queue(
                move |world: &mut bevy_ecs::world::World| {
                    spawn_prefab(world, handle, Transform::default()).map(drop)
                },
            );
            app.world_mut().flush();
            let _ = std::fs::rename(folder.join("moved.rscene"), &coin);
            (app, first)
        };
        let (mut app, first) = place(&folder);
        assert_eq!(
            app.world().get::<Transform>(first).unwrap().position[0],
            1.0
        );
        assert!(app.world().get::<SceneInstance>(first).is_some());
        assert_eq!(app.world().get::<Name>(first).unwrap().0, "coin #1");
        let roots = [first, named(&mut app, "coin #2")[0]];
        for root in &roots {
            let root_name = scene_id(&app, *root);
            let coin_entity = named(&mut app, "Coin")
                .into_iter()
                .find(|&entity| {
                    app.world().get::<crate::runtime::Parent>(entity).unwrap().0
                        == *root
                })
                .unwrap_or_else(|| panic!("{root_name} has a Coin"));
            let sparkle = named(&mut app, "Sparkle")
                .into_iter()
                .find(|&entity| {
                    app.world().get::<crate::runtime::Parent>(entity).unwrap().0
                        == coin_entity
                })
                .unwrap();
            let target = app.world().get::<Follow>(sparkle).unwrap().target;
            assert_eq!(target, Some(coin_entity));
        }
        let mut ids = roots
            .iter()
            .map(|&root| scene_id(&app, root))
            .collect::<Vec<_>>();
        ids.sort();

        let (mut replay, _) = place(&folder);
        let mut replayed = ["coin #1", "coin #2"]
            .into_iter()
            .map(|name| {
                let root = named(&mut replay, name)[0];
                scene_id(&replay, root)
            })
            .collect::<Vec<_>>();
        replayed.sort();
        assert_eq!(ids, replayed, "the same placements get the same IDs");

        // A save keeps the placed prefabs as links.
        let main = folder.join("main.rscene");
        save_scene(app.world_mut(), &main, "Main").unwrap();
        let saved = crate::runtime::read_scene_document(&main).unwrap();
        assert_eq!(saved.entities.len(), 2);

        assert!(matches!(
            spawn_prefab(
                app.world_mut(),
                crate::Handle::from_key(u64::MAX),
                Transform::default()
            ),
            Err(SceneIoError::MissingPrefab(_))
        ));
        std::fs::remove_dir_all(folder).unwrap();
    }

    #[test]
    fn diffs_merge_back_to_the_current_value() {
        use serde_json::json;
        let base = json!({
            "name": "Coin",
            "mesh": {"AssetPath": "coin.rmesh"},
            "components": {"a": {"x": 1, "y": [1, 2]}, "b": {"z": true}},
            "visible": null,
        });
        let current = json!({
            "name": "Coin",
            "mesh": {"BuiltinPrimitive": "Quad"},
            "components": {"a": {"x": 2, "y": [1, 2]}, "c": {}},
            "visible": false,
        });
        let patch = diff(&base, &current).unwrap();
        assert_eq!(patch["components"]["a"], json!({"x": 2}));
        assert_eq!(patch["components"]["b"], removed());
        let mut merged = base.clone();
        merge(&mut merged, &patch);
        assert_eq!(merged, current);
        assert_eq!(diff(&current, &current), None);
    }

    #[test]
    fn override_asset_paths_are_converted() {
        let mut entity: SceneEntity = serde_json::from_value(serde_json::json!({
            "id": Uuid::nil(),
            "parent": null,
            "name": "Spawner",
            "transform": null,
            "mesh_renderer": null,
            "camera": null,
            "visible": null,
            "components": {
                INSTANCE_OVERRIDES_KEY: serde_json::json!({
                    Uuid::nil().to_string(): {
                        "mesh_renderer": {"mesh": {"AssetPath": "a.rmesh"}},
                        "components": {
                            "rusting.scene_instance": {"source": "b.rscene"},
                        },
                    },
                })
                .to_string(),
            },
        }))
        .unwrap();
        map_override_paths(&mut entity, &mut |path| {
            Ok(Path::new("/project").join(path))
        })
        .unwrap();
        let overrides: Value =
            serde_json::from_str(&entity.components[INSTANCE_OVERRIDES_KEY])
                .unwrap();
        let diff = &overrides[Uuid::nil().to_string()];
        assert_eq!(
            diff["mesh_renderer"]["mesh"]["AssetPath"],
            "/project/a.rmesh"
        );
        assert_eq!(
            diff["components"]["rusting.scene_instance"]["source"],
            "/project/b.rscene"
        );
    }
}
