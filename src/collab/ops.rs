//! Per-field scene operations: the diff of two scene documents in JSON form
//! and its application. Fields are split as `scene_merge` splits them: each
//! top-level entity field, and each `components` entry on its own.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// One field set or removed. `entity` is the entity ID, or `None` for a
/// scene-level field. On an entity, field `entity` creates the entity
/// (whole value) or deletes it (`None`); `components/<name>` is one
/// component. The scene-level field `order` holds the entity ID list.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FieldOp {
    pub entity: Option<String>,
    pub field: String,
    /// JSON text of the new value; `None` removes the field.
    pub value: Option<String>,
}

/// What an operation writes: entity ID (or `None`) and field.
pub type FieldKey = (Option<String>, String);

impl FieldOp {
    #[must_use]
    pub fn key(&self) -> FieldKey {
        (self.entity.clone(), self.field.clone())
    }
}

const ENTITY: &str = "entity";
const ORDER: &str = "order";
const COMPONENTS: &str = "components/";

fn op(entity: Option<&str>, field: &str, value: Option<&Value>) -> FieldOp {
    FieldOp {
        entity: entity.map(str::to_owned),
        field: field.to_owned(),
        value: value.map(Value::to_string),
    }
}

fn entities(scene: &Value) -> Vec<&Map<String, Value>> {
    scene
        .get("entities")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_object)
        .collect()
}

fn id(entity: &Map<String, Value>) -> Option<&str> {
    entity.get("id").and_then(Value::as_str)
}

fn ids(scene: &Value) -> Vec<String> {
    entities(scene)
        .into_iter()
        .filter_map(|e| id(e).map(str::to_owned))
        .collect()
}

/// Pushes one operation per field of `ours` that differs from `base`,
/// skipping `skip`; a missing field is a removal.
fn diff_fields(
    base: &Map<String, Value>,
    ours: &Map<String, Value>,
    skip: &[&str],
    mut push: impl FnMut(&str, Option<&Value>),
) {
    let keys: Vec<&String> = base.keys().chain(ours.keys()).collect();
    let mut seen = HashSet::new();
    for key in keys {
        if skip.contains(&key.as_str()) || !seen.insert(key) {
            continue;
        }
        let after = ours.get(key);
        if base.get(key) != after {
            push(key, after);
        }
    }
}

/// The operations that turn `before` into `after`.
#[must_use]
pub fn diff_scene(before: &Value, after: &Value) -> Vec<FieldOp> {
    let empty = Map::new();
    let object = |v: &Value| v.as_object().cloned().unwrap_or_default();
    let mut ops = Vec::new();
    diff_fields(&object(before), &object(after), &["entities"], |k, v| {
        ops.push(op(None, k, v));
    });
    let old = entities(before);
    let find = |id_: &str| old.iter().copied().find(|e| id(e) == Some(id_));
    let new = entities(after);
    for entity in &new {
        let Some(key) = id(entity) else { continue };
        let Some(base) = find(key) else {
            ops.push(op(
                Some(key),
                ENTITY,
                Some(&Value::Object((*entity).clone())),
            ));
            continue;
        };
        diff_fields(base, entity, &["id", "components"], |k, v| {
            ops.push(op(Some(key), k, v));
        });
        let part = |e: &Map<String, Value>| {
            e.get("components").and_then(Value::as_object).cloned()
        };
        let (b, o) = (part(base), part(entity));
        diff_fields(
            b.as_ref().unwrap_or(&empty),
            o.as_ref().unwrap_or(&empty),
            &[],
            |k, v| ops.push(op(Some(key), &format!("{COMPONENTS}{k}"), v)),
        );
    }
    for entity in &old {
        if let Some(key) = id(entity) {
            if !new.iter().any(|e| id(e) == Some(key)) {
                ops.push(op(Some(key), ENTITY, None));
            }
        }
    }
    // Creates append, so order differs only when something moved.
    let mut applied = before.clone();
    apply_ops(&mut applied, &ops, &HashSet::new());
    let order = ids(after);
    if ids(&applied) != order {
        ops.push(op(None, ORDER, Some(&Value::from(order))));
    }
    ops
}

/// Applies `ops` in order, skipping operations whose key is in `skip`.
/// Operations on an entity that does not exist are ignored, except a
/// create; values that are not valid JSON are ignored.
pub fn apply_ops(doc: &mut Value, ops: &[FieldOp], skip: &HashSet<FieldKey>) {
    let Some(scene) = doc.as_object_mut() else {
        return;
    };
    for op in ops.iter().filter(|op| !skip.contains(&op.key())) {
        let value = match &op.value {
            Some(text) => match serde_json::from_str::<Value>(text) {
                Ok(value) => Some(value),
                Err(_) => continue,
            },
            None => None,
        };
        let Some(entity) = &op.entity else {
            if op.field == ORDER {
                reorder(scene, value.as_ref());
            } else if op.field != "entities" {
                set(scene, &op.field, value);
            }
            continue;
        };
        let list = scene
            .entry("entities")
            .or_insert_with(|| Value::Array(Vec::new()));
        let Some(list) = list.as_array_mut() else {
            continue;
        };
        let index = list
            .iter()
            .position(|e| e.get("id").and_then(Value::as_str) == Some(entity));
        if op.field == ENTITY {
            match (index, value) {
                (Some(i), Some(value)) => list[i] = value,
                (None, Some(value)) => list.push(value),
                (Some(i), None) => {
                    list.remove(i);
                }
                (None, None) => {}
            }
            continue;
        }
        let Some(target) = index.and_then(|i| list[i].as_object_mut()) else {
            continue;
        };
        match op.field.strip_prefix(COMPONENTS) {
            Some(name) => {
                let components = target
                    .entry("components")
                    .or_insert_with(|| Value::Object(Map::new()));
                if let Some(components) = components.as_object_mut() {
                    set(components, name, value);
                }
            }
            None if op.field != "id" => set(target, &op.field, value),
            None => {}
        }
    }
}

fn set(object: &mut Map<String, Value>, key: &str, value: Option<Value>) {
    match value {
        Some(value) => {
            object.insert(key.to_owned(), value);
        }
        None => {
            object.remove(key);
        }
    }
}

/// Sorts entities by the ID list; entities it does not name keep their
/// relative order after the named ones.
fn reorder(scene: &mut Map<String, Value>, order: Option<&Value>) {
    let Some(order) = order.and_then(Value::as_array) else {
        return;
    };
    let Some(Value::Array(list)) = scene.get_mut("entities") else {
        return;
    };
    let rank = |e: &Value| {
        let id = e.get("id");
        order
            .iter()
            .position(|o| Some(o) == id)
            .unwrap_or(order.len())
    };
    list.sort_by_key(rank);
}

/// Keys of the fields that differ between the last synced state and the
/// live scene: local edits not sent yet.
#[must_use]
pub fn pending_keys(synced: &Value, live: &Value) -> HashSet<FieldKey> {
    diff_scene(synced, live).iter().map(FieldOp::key).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn entity(id: &str, x: f64) -> Value {
        json!({"id": id, "name": id, "transform": {"position": [x, 0.0, 0.0]}, "components": {}})
    }

    fn scene(entities: Vec<Value>) -> Value {
        json!({"format_version": 9, "name": "S", "entities": entities})
    }

    fn round_trip(before: &Value, after: &Value) -> Vec<FieldOp> {
        let ops = diff_scene(before, after);
        let mut applied = before.clone();
        apply_ops(&mut applied, &ops, &HashSet::new());
        assert_eq!(&applied, after, "ops: {ops:?}");
        ops
    }

    #[test]
    fn identical_scenes_have_no_ops() {
        let s = scene(vec![entity("a", 0.0)]);
        assert!(diff_scene(&s, &s).is_empty());
    }

    #[test]
    fn field_change_is_one_op_on_that_field() {
        let before = scene(vec![entity("a", 0.0), entity("b", 0.0)]);
        let after = scene(vec![entity("a", 3.0), entity("b", 0.0)]);
        let ops = round_trip(&before, &after);
        assert_eq!(ops.len(), 1);
        assert_eq!(ops[0].key(), (Some("a".into()), "transform".into()));
    }

    #[test]
    fn components_are_separate_fields() {
        let before = scene(vec![entity("a", 0.0)]);
        let mut tagged = entity("a", 0.0);
        tagged["components"] = json!({"game.hp": 3, "game.tag": "x"});
        let after = scene(vec![tagged.clone()]);
        let ops = round_trip(&before, &after);
        assert_eq!(ops.len(), 2);
        assert!(ops.iter().all(|op| op.field.starts_with("components/")));
        tagged["components"] = json!({"game.tag": "x"});
        let removed = round_trip(&after, &scene(vec![tagged]));
        assert_eq!(removed[0].field, "components/game.hp");
        assert_eq!(removed[0].value, None);
    }

    #[test]
    fn create_delete_and_reorder_round_trip() {
        let before = scene(vec![entity("a", 0.0), entity("b", 0.0)]);
        round_trip(
            &before,
            &scene(vec![entity("a", 0.0), entity("b", 0.0), entity("c", 1.0)]),
        );
        round_trip(&before, &scene(vec![entity("b", 0.0)]));
        let ops = round_trip(
            &before,
            &scene(vec![entity("b", 0.0), entity("a", 0.0)]),
        );
        assert_eq!(ops.len(), 1);
        assert_eq!(ops[0].field, "order");
        // A create in the middle needs an order op as well.
        round_trip(
            &before,
            &scene(vec![entity("a", 0.0), entity("c", 1.0), entity("b", 0.0)]),
        );
    }

    #[test]
    fn scene_level_fields_round_trip() {
        let before = scene(vec![entity("a", 0.0)]);
        let mut after = before.clone();
        after["name"] = json!("Renamed");
        after["render"] = json!({"fog": true});
        let ops = round_trip(&before, &after);
        assert!(ops.iter().all(|op| op.entity.is_none()));
    }

    #[test]
    fn skip_keeps_pending_local_fields() {
        let mut doc = scene(vec![entity("a", 0.0)]);
        let ops = diff_scene(&doc, &scene(vec![entity("a", 5.0)]));
        let skip =
            HashSet::from([(Some("a".to_owned()), "transform".to_owned())]);
        apply_ops(&mut doc, &ops, &skip);
        assert_eq!(doc, scene(vec![entity("a", 0.0)]));
    }

    #[test]
    fn ops_on_missing_entities_are_ignored() {
        let mut doc = scene(vec![entity("a", 0.0)]);
        let ops = diff_scene(
            &scene(vec![entity("z", 0.0)]),
            &scene(vec![entity("z", 4.0)]),
        );
        apply_ops(&mut doc, &ops, &HashSet::new());
        assert_eq!(doc, scene(vec![entity("a", 0.0)]));
    }

    #[test]
    fn concurrent_edits_to_different_entities_both_survive() {
        let base = scene(vec![entity("a", 0.0), entity("b", 0.0)]);
        let from_a =
            diff_scene(&base, &scene(vec![entity("a", 1.0), entity("b", 0.0)]));
        let from_b =
            diff_scene(&base, &scene(vec![entity("a", 0.0), entity("b", 2.0)]));
        let mut host = base.clone();
        apply_ops(&mut host, &from_a, &HashSet::new());
        apply_ops(&mut host, &from_b, &HashSet::new());
        assert_eq!(host, scene(vec![entity("a", 1.0), entity("b", 2.0)]));
    }

    #[test]
    fn pending_keys_name_unsent_fields() {
        let synced = scene(vec![entity("a", 0.0)]);
        let live = scene(vec![entity("a", 1.0)]);
        assert_eq!(
            pending_keys(&synced, &live),
            HashSet::from([(Some("a".to_owned()), "transform".to_owned())])
        );
    }
}
