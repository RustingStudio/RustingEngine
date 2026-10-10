//! Replicated spawning and component synchronization.
//!
//! The host marks objects with [`Replicated`] and lists, by registry name,
//! which components to copy. [`Replication::delta`] reads them through
//! reflection (their scene form), rounds the fields given a quantization
//! step, and returns only what changed since the last call: new objects,
//! changed components, removed components and despawns. Clients feed every
//! such message to [`Replica::apply`], which matches objects by
//! [`SceneId`], so objects both sides loaded from the same scene file are
//! updated in place and new ones are spawned.
//!
//! For state sent unreliably every tick, [`Replication::snapshot`] numbers
//! each capture and [`Replication::snapshot_for`] encodes it against the
//! last snapshot that client acknowledged ([`Replica::ack`]), so a lost
//! message costs nothing but a bigger next one.

use super::{invalid, PeerId, HOST};
use crate::runtime::{
    registered_component_values, remove_registered_component,
    set_registered_component, Name, SceneId,
};
use crate::Transform;
use bevy_ecs::prelude::*;
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::io;
use uuid::Uuid;

/// Marks a replication message, so game messages pass by.
const MAGIC: &[u8; 4] = b"\0rep";
/// Marks a numbered snapshot, and a client's acknowledgement of one.
const SNAPSHOT: &[u8; 4] = b"\0rsn";
const ACK: &[u8; 4] = b"\0rak";
/// Marks the whole replicated state, a state hash, and a client's request
/// for a resynchronization.
const FULL: &[u8; 4] = b"\0rfl";
const CHECKSUM: &[u8; 4] = b"\0rhs";
const RESYNC: &[u8; 4] = b"\0rrs";
/// Snapshots each side keeps to encode against, about a second at 60 Hz.
/// A client whose last acknowledgement is older gets a whole snapshot.
pub const SNAPSHOT_HISTORY: usize = 64;
/// Pseudo-components for the parts of an object that are not registered
/// scene components.
pub const TRANSFORM: &str = "transform";
pub const NAME: &str = "name";
/// Most objects a [`Replica`] tracks, so a host cannot make a client spawn
/// without end.
pub const MAX_OBJECTS: usize = 16_384;

/// Marks an object the host replicates to clients.
#[derive(Component, Clone, Copy, Debug, Default)]
pub struct Replicated;

/// What a client sent the host about replication.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FromClient {
    /// It acknowledged a snapshot.
    Ack,
    /// It found a desync and needs the whole state.
    Resync,
}

/// The result of [`Replica::verify`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Check {
    Match,
    /// This world differs from the host's state.
    Desync,
    /// The hash is for a snapshot this client has not applied (yet).
    Stale,
}

/// The host's latest transform for an object, when the [`Replica`] is
/// [`interpolated`](Replica::interpolated); `Transform` is then what the
/// client shows.
#[derive(Component, Clone, Copy, Debug, Default, PartialEq)]
pub struct NetTransform(pub Transform);

/// Replicated state of one object: component name to scene form.
type Object = Map<String, Value>;
type State = BTreeMap<Uuid, Object>;

/// The host's side: what to send, and what it already sent.
#[derive(Clone, Debug, Default)]
pub struct Replication {
    components: Vec<String>,
    steps: Vec<(String, f64)>,
    sent: State,
    /// Numbered snapshots, oldest first, and what each client acknowledged.
    history: VecDeque<(u64, State)>,
    acked: BTreeMap<PeerId, u64>,
    /// Checksums sent, and the count when each client last resynced: a
    /// client may resync once per checksum, so a few bytes of requests
    /// cannot make the host send the whole state over and over.
    checksums: u64,
    resynced: BTreeMap<PeerId, u64>,
}

impl Replication {
    pub fn new() -> Self {
        Self::default()
    }

    /// Replicates a component: a registry name such as `rusting.health`,
    /// or [`TRANSFORM`] or [`NAME`].
    pub fn component(&mut self, name: &str) -> &mut Self {
        self.components.push(name.to_owned());
        self
    }

    /// Rounds every number at `pointer` (a component name and a field
    /// path, such as `/transform/position`) to a multiple of `step`, so
    /// tiny changes send nothing and clients see the same rounded values.
    /// A client's value is then within `step / 2` of the host's. Setting
    /// a pointer again replaces its step.
    ///
    /// # Panics
    ///
    /// When `step` is not a positive finite number.
    pub fn quantize(&mut self, pointer: &str, step: f64) -> &mut Self {
        assert!(
            step.is_finite() && step > 0.0,
            "quantization step for {pointer} must be positive, not {step}"
        );
        self.steps.retain(|(kept, _)| kept != pointer);
        self.steps.push((pointer.to_owned(), step));
        self
    }

    /// Each quantized field path and its step, in the order set, so a game
    /// can log or check the precision it replicates with.
    pub fn precision(&self) -> &[(String, f64)] {
        &self.steps
    }

    /// What changed since the last call, as one message for
    /// [`NetSession::broadcast`](super::NetSession::broadcast), or `None`
    /// when nothing did. Send it reliably: each message builds on the one
    /// before.
    pub fn delta(&mut self, world: &mut World) -> io::Result<Option<Vec<u8>>> {
        let now = self.capture(world)?;
        let patch = diff(&self.sent, &now);
        self.sent = now;
        Ok((!patch.is_empty()).then(|| encode(&patch)))
    }

    /// Everything sent so far as one message, for a client that just
    /// joined or asked to resynchronize. Send it to that client before the
    /// next delta. The client replaces its replicated objects with it,
    /// despawning any the host does not have.
    pub fn full(&self) -> Vec<u8> {
        let all: Map<String, Value> = self
            .sent
            .iter()
            .map(|(id, object)| (id.to_string(), Value::Object(object.clone())))
            .collect();
        let mut bytes = FULL.to_vec();
        bytes.extend(serde_json::to_vec(&all).expect("JSON serializes"));
        bytes
    }

    /// A hash of the latest state, for [`Replica::verify`]: of the latest
    /// snapshot, or of what [`delta`](Self::delta) last sent when no
    /// snapshot was taken. Send it every second or so, the same way as
    /// the state (reliably after a delta). Each one lets every client
    /// resynchronize once.
    pub fn checksum(&mut self) -> Vec<u8> {
        self.checksums += 1;
        let (seq, state) = self
            .history
            .back()
            .map_or((0, &self.sent), |(seq, state)| (*seq, state));
        [
            CHECKSUM.as_slice(),
            &seq.to_le_bytes(),
            &hash(state).to_le_bytes(),
        ]
        .concat()
    }

    /// Captures the replicated state as the next numbered snapshot and
    /// returns its number. Then send [`snapshot_for`](Self::snapshot_for)
    /// each client, unreliably.
    pub fn snapshot(&mut self, world: &mut World) -> io::Result<u64> {
        let now = self.capture(world)?;
        let seq = self.history.back().map_or(1, |(seq, _)| seq + 1);
        if self.history.len() == SNAPSHOT_HISTORY {
            self.history.pop_front();
        }
        self.history.push_back((seq, now));
        Ok(seq)
    }

    /// The latest snapshot for `peer`, encoded as the changes since the
    /// last snapshot it acknowledged, or whole when it acknowledged none
    /// still kept. `None` before the first [`snapshot`](Self::snapshot).
    pub fn snapshot_for(&self, peer: PeerId) -> Option<Vec<u8>> {
        let (seq, now) = self.history.back()?;
        let acked = self.acked.get(&peer).copied().unwrap_or(0);
        let empty = State::new();
        let (base, before) = self
            .history
            .iter()
            .find(|(seq, _)| *seq == acked)
            .map_or((0, &empty), |(seq, state)| (*seq, state));
        let mut bytes = SNAPSHOT.to_vec();
        bytes.extend(seq.to_le_bytes());
        bytes.extend(base.to_le_bytes());
        bytes.extend(
            serde_json::to_vec(&diff(before, now)).expect("JSON serializes"),
        );
        Some(bytes)
    }

    /// Reads a client's acknowledgement ([`Replica::ack`]) or request for
    /// a resynchronization ([`Replica::resync`]). Returns `None` for any
    /// other message. On a request, the next
    /// [`snapshot_for`](Self::snapshot_for) that client is whole; with
    /// deltas, send it [`full`](Self::full).
    pub fn accept(
        &mut self,
        from: PeerId,
        bytes: &[u8],
    ) -> Option<Result<FromClient, String>> {
        if bytes == RESYNC {
            if self.resynced.get(&from) == Some(&self.checksums) {
                return Some(Err(format!(
                    "peer {from} asked to resync again before a new checksum"
                )));
            }
            self.resynced.insert(from, self.checksums);
            self.acked.remove(&from);
            return Some(Ok(FromClient::Resync));
        }
        let body = bytes.strip_prefix(ACK)?;
        let latest = self.history.back().map_or(0, |(seq, _)| *seq);
        Some(match <[u8; 8]>::try_from(body).map(u64::from_le_bytes) {
            Ok(seq) if seq <= latest => {
                let acked = self.acked.entry(from).or_default();
                *acked = (*acked).max(seq);
                Ok(FromClient::Ack)
            }
            Ok(seq) => Err(format!("peer {from} acked unsent snapshot {seq}")),
            Err(_) => Err(format!("peer {from} sent a bad ack")),
        })
    }

    /// Forgets a client that left.
    pub fn forget(&mut self, peer: PeerId) {
        self.acked.remove(&peer);
        self.resynced.remove(&peer);
    }

    fn capture(&self, world: &mut World) -> io::Result<State> {
        let mut query = world.query_filtered::<(
            Entity,
            &SceneId,
            Option<&Transform>,
            Option<&NetTransform>,
            Option<&Name>,
        ), With<Replicated>>();
        let found: Vec<_> = query
            .iter(world)
            .map(|(entity, id, transform, net, name)| {
                (
                    entity,
                    id.0,
                    // On an interpolated client, the host's transform.
                    net.map(|net| net.0).or(transform.copied()),
                    name.map(|name| name.0.clone()),
                )
            })
            .collect();
        let mut objects = BTreeMap::new();
        for (entity, id, transform, name) in found {
            let mut object = Object::new();
            let values = registered_component_values(world, entity)
                .map_err(|error| invalid(&error.to_string()))?;
            for (key, text) in values {
                if self.components.contains(&key) {
                    let value = serde_json::from_str(&text)?;
                    object.insert(key, value);
                }
            }
            if let Some(transform) = transform.filter(|_| self.wants(TRANSFORM))
            {
                object.insert(TRANSFORM.into(), transform_value(&transform));
            }
            if let Some(name) = name.filter(|_| self.wants(NAME)) {
                object.insert(NAME.into(), Value::String(name));
            }
            let mut value = Value::Object(object);
            for (pointer, step) in &self.steps {
                if let Some(field) = value.pointer_mut(pointer) {
                    round(field, *step);
                }
            }
            let Value::Object(object) = value else {
                unreachable!()
            };
            objects.insert(id, object);
        }
        Ok(objects)
    }

    fn wants(&self, name: &str) -> bool {
        self.components.iter().any(|wanted| wanted == name)
    }
}

/// A client's side: applies the host's messages to its world.
#[derive(Clone, Debug, Default)]
pub struct Replica {
    /// What to accept and how to capture it, as the host does.
    config: Replication,
    entities: BTreeMap<Uuid, Entity>,
    /// Snapshots received, oldest first; the last one is in the world.
    states: VecDeque<(u64, State)>,
    interpolate: bool,
}

impl Replica {
    /// A replica that accepts only the components `replication` lists.
    /// Build the same [`Replication`] on every peer and pass it here.
    pub fn new(replication: &Replication) -> Self {
        Self {
            config: Replication {
                components: replication.components.clone(),
                steps: replication.steps.clone(),
                ..Replication::default()
            },
            entities: BTreeMap::new(),
            states: VecDeque::new(),
            interpolate: false,
        }
    }

    /// Writes received transforms into [`NetTransform`] instead of
    /// `Transform`, for [`Interpolation`](super::interpolate::Interpolation)
    /// to present smoothly. An object's first transform goes to both.
    pub fn interpolated(mut self) -> Self {
        self.interpolate = true;
        self
    }

    /// Applies a message `from` a peer, made by [`Replication::delta`] or
    /// [`Replication::full`]. Returns `None` for a message that is not a
    /// replication message, and an error for a bad one or one that does
    /// not come from the host; a refused message changes nothing.
    ///
    /// It touches only objects marked [`Replicated`]: ones it spawned, and
    /// ones the game marked, such as objects loaded from the same scene.
    pub fn apply(
        &mut self,
        world: &mut World,
        from: PeerId,
        bytes: &[u8],
    ) -> Option<Result<(), String>> {
        let kinds = [MAGIC, SNAPSHOT, FULL];
        let (kind, body) = kinds.iter().find_map(|kind| {
            Some((*kind, bytes.strip_prefix(kind.as_slice())?))
        })?;
        if from != HOST {
            return Some(Err(format!(
                "peer {from} sent replication; only the host may"
            )));
        }
        Some(if kind == SNAPSHOT {
            self.apply_snapshot(world, body)
        } else if kind == FULL {
            parse(body)
                .and_then(|patch| merge(&State::new(), patch))
                .and_then(|now| self.replace(world, &now))
        } else {
            parse(body).and_then(|patch| self.apply_patch(world, patch))
        })
    }

    /// Compares this world with a [`Replication::checksum`] `from` the
    /// host. Returns `None` for any other message. On
    /// [`Check::Desync`], report it and send the host
    /// [`resync`](Self::resync).
    pub fn verify(
        &self,
        world: &mut World,
        from: PeerId,
        bytes: &[u8],
    ) -> Option<Result<Check, String>> {
        let body = bytes.strip_prefix(CHECKSUM)?;
        if from != HOST {
            return Some(Err(format!("peer {from} sent a checksum")));
        }
        let Some((seq, expected)) = body
            .split_at_checked(8)
            .filter(|(_, hash)| hash.len() == 8)
            .map(|(seq, hash)| {
                let read = |b: &[u8]| u64::from_le_bytes(b.try_into().unwrap());
                (read(seq), read(hash))
            })
        else {
            return Some(Err("bad checksum".into()));
        };
        let latest = self.states.back().map_or(0, |(seq, _)| *seq);
        if seq != latest {
            return Some(Ok(Check::Stale));
        }
        Some(
            self.config
                .capture(world)
                .map(|state| {
                    if hash(&state) == expected {
                        Check::Match
                    } else {
                        Check::Desync
                    }
                })
                .map_err(|error| error.to_string()),
        )
    }

    /// A request for the host to resynchronize this client, after
    /// [`verify`](Self::verify) found a desync.
    pub fn resync(&self) -> Vec<u8> {
        RESYNC.to_vec()
    }

    /// Makes the world's replicated objects match `now` exactly, whatever
    /// happened to them locally.
    fn replace(
        &mut self,
        world: &mut World,
        now: &State,
    ) -> Result<(), String> {
        let current = self
            .config
            .capture(world)
            .map_err(|error| error.to_string())?;
        self.apply_patch(world, diff(&current, now))
    }

    /// The acknowledgement of the latest snapshot applied, for the client
    /// to send the host (unreliably is fine), or `None` before the first.
    pub fn ack(&self) -> Option<Vec<u8>> {
        let (seq, _) = self.states.back()?;
        Some([ACK.as_slice(), &seq.to_le_bytes()].concat())
    }

    fn apply_snapshot(
        &mut self,
        world: &mut World,
        body: &[u8],
    ) -> Result<(), String> {
        let (seq, rest) = body.split_at_checked(8).ok_or("short snapshot")?;
        let (base, patch) = rest.split_at_checked(8).ok_or("short snapshot")?;
        let seq = u64::from_le_bytes(seq.try_into().expect("8 bytes"));
        let base = u64::from_le_bytes(base.try_into().expect("8 bytes"));
        let latest = self.states.back().map_or(0, |(seq, _)| *seq);
        if seq <= latest {
            return Ok(()); // Overtaken by a newer snapshot.
        }
        let empty = State::new();
        let before = match base {
            0 => &empty,
            _ => self
                .states
                .iter()
                .find(|(kept, _)| *kept == base)
                .map(|(_, state)| state)
                .ok_or_else(|| {
                    format!("snapshot {seq} needs unknown {base}")
                })?,
        };
        let now = merge(before, parse(patch)?)?;
        if base == 0 {
            // Whole: after a join or a resync, match it exactly.
            self.replace(world, &now)?;
        } else {
            let current = self.states.back().map_or(&empty, |(_, state)| state);
            self.apply_patch(world, diff(current, &now))?;
        }
        // The host encodes later snapshots against `base` or newer.
        self.states.retain(|(kept, _)| *kept >= base);
        if self.states.len() == SNAPSHOT_HISTORY {
            self.states.pop_front();
        }
        self.states.push_back((seq, now));
        Ok(())
    }

    fn apply_patch(
        &mut self,
        world: &mut World,
        patch: Map<String, Value>,
    ) -> Result<(), String> {
        // Check the whole message before changing anything.
        let mut changes = Vec::with_capacity(patch.len());
        let mut tracked = self.entities.len();
        let mut seen = BTreeSet::new();
        for (key, change) in patch {
            let id =
                Uuid::parse_str(&key).map_err(|error| error.to_string())?;
            // One id in two spellings (case, braces, urn:) would pass the
            // checks twice against the same world state.
            if !seen.insert(id) {
                return Err(format!("{id} appears twice"));
            }
            let entity = self.find(world, id)?;
            match &change {
                Value::Object(fields) => {
                    if let Some(name) = fields
                        .keys()
                        .find(|name| !self.config.components.contains(name))
                    {
                        return Err(format!("{name} is not replicated"));
                    }
                    if entity.is_none() {
                        tracked += 1;
                    }
                }
                Value::Null => {}
                _ => return Err(format!("{id}: expected an object or null")),
            }
            changes.push((id, entity, change));
        }
        if tracked > MAX_OBJECTS {
            return Err(format!("more than {MAX_OBJECTS} replicated objects"));
        }
        for (id, entity, change) in changes {
            let Value::Object(fields) = change else {
                if let Some(entity) = entity {
                    world.despawn(entity);
                }
                self.entities.remove(&id);
                continue;
            };
            let entity = entity
                .unwrap_or_else(|| world.spawn((Replicated, SceneId(id))).id());
            self.entities.insert(id, entity);
            for (name, value) in fields {
                apply_component(world, entity, &name, value, self.interpolate)
                    .map_err(|error| format!("{name} on {id}: {error}"))?;
            }
        }
        Ok(())
    }

    /// The [`Replicated`] entity for `id`, or `None` when there is none.
    /// Fails when an unmarked object has that id, which the host may not
    /// touch.
    fn find(
        &self,
        world: &mut World,
        id: Uuid,
    ) -> Result<Option<Entity>, String> {
        if let Some(entity) = self.entities.get(&id) {
            if world.get_entity(*entity).is_ok() {
                return Ok(Some(*entity));
            }
        }
        // ponytail: scans every SceneId once per new object; keep an
        // index if replicated scenes get large.
        let found = world
            .query::<(Entity, &SceneId, Has<Replicated>)>()
            .iter(world)
            .find(|(_, scene_id, _)| scene_id.0 == id);
        match found {
            None => Ok(None),
            Some((entity, _, true)) => Ok(Some(entity)),
            Some(_) => Err(format!("{id} is not marked Replicated here")),
        }
    }
}

fn apply_component(
    world: &mut World,
    entity: Entity,
    name: &str,
    value: Value,
    interpolate: bool,
) -> Result<(), String> {
    match (name, value) {
        (TRANSFORM, Value::Null) => {}
        (TRANSFORM, value) => {
            let mut transform = Transform::default();
            let read = |field: &str, into: &mut [f32; 3]| {
                if let Some(Value::Array(items)) = value.get(field) {
                    for (slot, item) in into.iter_mut().zip(items) {
                        *slot = item.as_f64().unwrap_or_default() as f32;
                    }
                }
            };
            read("position", &mut transform.position);
            read("rotation", &mut transform.rotation);
            read("scale", &mut transform.scale);
            let mut entity = world.entity_mut(entity);
            if !interpolate || !entity.contains::<NetTransform>() {
                entity.insert(transform);
            }
            if interpolate {
                entity.insert(NetTransform(transform));
            }
        }
        (NAME, Value::String(name)) => {
            world.entity_mut(entity).insert(Name(name));
        }
        (NAME, _) => {
            world.entity_mut(entity).remove::<Name>();
        }
        (name, Value::Null) => {
            remove_registered_component(world, entity, name)
                .map_err(|error| error.to_string())?;
        }
        (name, value) => {
            set_registered_component(world, entity, name, &value.to_string())
                .map_err(|error| error.to_string())?;
        }
    }
    Ok(())
}

fn transform_value(transform: &Transform) -> Value {
    json!({
        "position": transform.position,
        "rotation": transform.rotation,
        "scale": transform.scale,
    })
}

fn round(value: &mut Value, step: f64) {
    match value {
        Value::Number(number) => {
            if let Some(x) = number.as_f64() {
                *value = json!((x / step).round() * step);
            }
        }
        Value::Array(items) => {
            items.iter_mut().for_each(|item| round(item, step))
        }
        Value::Object(fields) => {
            fields.values_mut().for_each(|item| round(item, step))
        }
        _ => {}
    }
}

/// `before` with `patch` applied, refusing an id given twice.
fn merge(before: &State, patch: Map<String, Value>) -> Result<State, String> {
    let mut now = before.clone();
    let mut seen = BTreeSet::new();
    for (key, change) in patch {
        let id = Uuid::parse_str(&key).map_err(|error| error.to_string())?;
        if !seen.insert(id) {
            return Err(format!("{id} appears twice"));
        }
        match change {
            Value::Null => {
                now.remove(&id);
            }
            Value::Object(fields) => {
                let object = now.entry(id).or_default();
                for (name, value) in fields {
                    match value {
                        Value::Null => object.remove(&name),
                        value => object.insert(name, value),
                    };
                }
            }
            _ => return Err(format!("{id}: expected an object or null")),
        }
    }
    Ok(now)
}

/// FNV-1a of the state's JSON: the same on every machine and build.
fn hash(state: &State) -> u64 {
    serde_json::to_vec(state)
        .expect("JSON serializes")
        .iter()
        .fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
            (hash ^ u64::from(*byte)).wrapping_mul(0x0100_0000_01b3)
        })
}

/// The patch that turns `before` into `now`: new objects whole, changed
/// and removed (null) components, and despawned (null) objects.
fn diff(before: &State, now: &State) -> Map<String, Value> {
    let mut patch = Map::new();
    for (id, object) in now {
        let key = id.to_string();
        match before.get(id) {
            None => {
                patch.insert(key, Value::Object(object.clone()));
            }
            Some(before) => {
                let mut changes = Map::new();
                for (name, value) in object {
                    if before.get(name) != Some(value) {
                        changes.insert(name.clone(), value.clone());
                    }
                }
                for name in before.keys() {
                    if !object.contains_key(name) {
                        changes.insert(name.clone(), Value::Null);
                    }
                }
                if !changes.is_empty() {
                    patch.insert(key, Value::Object(changes));
                }
            }
        }
    }
    for id in before.keys() {
        if !now.contains_key(id) {
            patch.insert(id.to_string(), Value::Null);
        }
    }
    patch
}

fn parse(body: &[u8]) -> Result<Map<String, Value>, String> {
    serde_json::from_slice(body).map_err(|error| error.to_string())
}

fn encode(patch: &Map<String, Value>) -> Vec<u8> {
    let mut bytes = MAGIC.to_vec();
    bytes.extend(serde_json::to_vec(patch).expect("JSON values serialize"));
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::{Health, SceneComponentRegistry};

    fn world() -> World {
        let mut world = World::new();
        world.insert_resource(SceneComponentRegistry::default());
        world
    }

    fn health(world: &World, entity: Entity) -> Option<i32> {
        world.get::<Health>(entity).map(|health| health.value)
    }

    #[test]
    fn quantized_fields_stay_within_half_a_step() {
        let mut replication = Replication::new();
        replication
            .quantize("/transform/position", 0.5)
            .quantize("/transform/rotation", 0.001)
            .quantize("/transform/position", 0.01);
        assert_eq!(
            replication.precision(),
            [
                ("/transform/rotation".to_owned(), 0.001),
                ("/transform/position".to_owned(), 0.01),
            ]
        );
        for x in [-2.5137, 0.0049, 0.0051, 1234.5678] {
            let mut value = json!(x);
            round(&mut value, 0.01);
            assert!((value.as_f64().unwrap() - x).abs() <= 0.005 + 1e-12);
        }
        let zero = std::panic::catch_unwind(|| {
            Replication::new().quantize("/transform/position", 0.0);
        });
        assert!(zero.is_err(), "a zero step would turn values into null");
    }

    #[test]
    fn a_desync_is_detected_and_repaired_by_a_resync() {
        let (mut host, mut client) = (world(), world());
        let mut replication = Replication::new();
        replication
            .component(TRANSFORM)
            .component("rusting.health")
            .quantize("/transform/position", 0.01);
        let health_of = |value| Health {
            value,
            max: 10,
            team: String::new(),
        };
        host.spawn((
            Replicated,
            SceneId::new(),
            Transform::new([1.234, 0.0, 0.0]),
            health_of(5),
        ));
        let mut replica = Replica::new(&replication);

        // Deltas: same state, same hash, quantized values included.
        let delta = replication.delta(&mut host).unwrap().unwrap();
        replica.apply(&mut client, HOST, &delta).unwrap().unwrap();
        let check = |replica: &Replica, client: &mut World, bytes: &[u8]| {
            replica.verify(client, HOST, bytes).unwrap().unwrap()
        };
        assert_eq!(
            check(&replica, &mut client, &replication.checksum()),
            Check::Match
        );

        // Something on the client breaks its copy and adds a stray object.
        let copy = client
            .query_filtered::<Entity, With<Health>>()
            .single(&client)
            .unwrap();
        client.entity_mut(copy).insert(health_of(1));
        client.spawn((Replicated, SceneId::new(), health_of(9)));
        assert_eq!(
            check(&replica, &mut client, &replication.checksum()),
            Check::Desync
        );

        // The host answers a resync with the whole state, which replaces.
        assert_eq!(
            replication.accept(1, &replica.resync()),
            Some(Ok(FromClient::Resync))
        );
        assert!(
            replication.accept(1, &replica.resync()).unwrap().is_err(),
            "once per checksum"
        );
        replica
            .apply(&mut client, HOST, &replication.full())
            .unwrap()
            .unwrap();
        assert_eq!(
            check(&replica, &mut client, &replication.checksum()),
            Check::Match
        );
        assert_eq!(client.query::<&Health>().iter(&client).count(), 1);

        // Snapshots: a resync makes the next one whole, which also replaces.
        replication.snapshot(&mut host).unwrap();
        let first = replication.snapshot_for(1).unwrap();
        replica.apply(&mut client, HOST, &first).unwrap().unwrap();
        replication
            .accept(1, &replica.ack().unwrap())
            .unwrap()
            .unwrap();
        client.entity_mut(copy).insert(health_of(2));
        replication.snapshot(&mut host).unwrap();
        let stale = replication.checksum();
        assert_eq!(check(&replica, &mut client, &stale), Check::Stale);
        let second = replication.snapshot_for(1).unwrap();
        replica.apply(&mut client, HOST, &second).unwrap().unwrap();
        assert_eq!(check(&replica, &mut client, &stale), Check::Desync);
        replication.accept(1, &replica.resync()).unwrap().unwrap();
        host.spawn((Replicated, SceneId::new(), health_of(3)));
        replication.snapshot(&mut host).unwrap();
        let whole = replication.snapshot_for(1).unwrap();
        replica.apply(&mut client, HOST, &whole).unwrap().unwrap();
        assert_eq!(
            check(&replica, &mut client, &replication.checksum()),
            Check::Match
        );
        assert!(replica.verify(&mut client, 2, &stale).unwrap().is_err());
    }

    #[test]
    fn snapshots_encode_against_the_last_acknowledged_one() {
        let (mut host, mut client) = (world(), world());
        let mut replication = Replication::new();
        replication
            .component(TRANSFORM)
            .component(NAME)
            .component("rusting.health");
        let health_of = |value| Health {
            value,
            max: 10,
            team: String::new(),
        };
        let raft = host
            .spawn((
                Replicated,
                SceneId::new(),
                health_of(5),
                Transform::default(),
            ))
            .id();
        let mut replica = Replica::new(&replication);
        let copy = |client: &mut World| {
            client
                .query_filtered::<Entity, With<Health>>()
                .iter(client)
                .next()
        };

        // No acknowledgement yet: the first snapshot is whole.
        assert_eq!(replication.snapshot(&mut host).unwrap(), 1);
        let first = replication.snapshot_for(1).unwrap();
        replica.apply(&mut client, HOST, &first).unwrap().unwrap();
        let entity = copy(&mut client).unwrap();
        assert_eq!(health(&client, entity), Some(5));
        let ack = replica.ack().unwrap();
        assert_eq!(replication.accept(1, &ack), Some(Ok(FromClient::Ack)));

        // Snapshot 2 is lost. Snapshot 3 is encoded against 1, so it still
        // carries the name 2 added, but not the unchanged transform.
        host.entity_mut(raft).insert(Name("Raft".into()));
        replication.snapshot(&mut host).unwrap();
        let lost = replication.snapshot_for(1).unwrap();
        host.entity_mut(raft).insert(health_of(7));
        replication.snapshot(&mut host).unwrap();
        let third = replication.snapshot_for(1).unwrap();
        assert!(third.len() < replication.snapshot_for(2).unwrap().len());
        replica.apply(&mut client, HOST, &third).unwrap().unwrap();
        assert_eq!(health(&client, entity), Some(7));
        assert_eq!(client.get::<Name>(entity).unwrap().0, "Raft");

        // A late snapshot changes nothing; one against an unknown base
        // fails; acks for unsent snapshots and from the wrong side fail.
        replica.apply(&mut client, HOST, &lost).unwrap().unwrap();
        assert_eq!(health(&client, entity), Some(7));
        let mut forged = third.clone();
        forged[4..12].copy_from_slice(&9u64.to_le_bytes());
        forged[12..20].copy_from_slice(&8u64.to_le_bytes());
        assert!(replica.apply(&mut client, HOST, &forged).unwrap().is_err());
        assert!(replica.apply(&mut client, 2, &third).unwrap().is_err());
        let mut far = ack.clone();
        far[4..].copy_from_slice(&99u64.to_le_bytes());
        assert!(replication.accept(1, &far).unwrap().is_err());

        // Despawning travels too, and the client prunes old snapshots.
        replication
            .accept(1, &replica.ack().unwrap())
            .unwrap()
            .unwrap();
        host.despawn(raft);
        replication.snapshot(&mut host).unwrap();
        let gone = replication.snapshot_for(1).unwrap();
        replica.apply(&mut client, HOST, &gone).unwrap().unwrap();
        assert_eq!(copy(&mut client), None);
        assert_eq!(replica.states.len(), 2);
        assert_eq!(replication.accept(1, b"game bytes"), None);
    }

    #[test]
    fn deltas_spawn_update_and_despawn_quantized_objects() {
        let (mut host, mut client) = (world(), world());
        let mut replication = Replication::new();
        replication
            .component(TRANSFORM)
            .component(NAME)
            .component("rusting.health")
            .quantize("/transform/position", 0.01);
        let raft = host
            .spawn((
                Replicated,
                SceneId::new(),
                Name("Raft".into()),
                Transform::new([1.004, 2.0, 3.0]),
                Health {
                    value: 5,
                    max: 10,
                    team: String::new(),
                },
            ))
            .id();
        host.spawn((SceneId::new(), Name("Local only".into())));
        let mut replica = Replica::new(&replication);

        let spawn = replication.delta(&mut host).unwrap().unwrap();
        replica.apply(&mut client, HOST, &spawn).unwrap().unwrap();
        let mut names = client.query::<(Entity, &Name, &Transform)>();
        let found: Vec<_> = names
            .iter(&client)
            .map(|(e, n, t)| (e, n.0.clone(), *t))
            .collect();
        assert_eq!(found.len(), 1, "only replicated objects travel");
        let (copy, name, transform) = found[0].clone();
        assert_eq!(name, "Raft");
        assert_eq!(transform.position, [1.0, 2.0, 3.0], "rounded to 0.01");
        assert_eq!(health(&client, copy), Some(5));

        // Nothing changed, or less than the step: nothing to send.
        assert_eq!(replication.delta(&mut host).unwrap(), None);
        host.get_mut::<Transform>(raft).unwrap().position[0] = 1.001;
        assert_eq!(replication.delta(&mut host).unwrap(), None);

        // Only the changed component travels.
        host.get_mut::<Health>(raft).unwrap().value = 3;
        let update = replication.delta(&mut host).unwrap().unwrap();
        let text = String::from_utf8_lossy(&update[4..]).into_owned();
        assert!(
            text.contains("rusting.health") && !text.contains("transform"),
            "{text}"
        );
        replica.apply(&mut client, HOST, &update).unwrap().unwrap();
        assert_eq!(health(&client, copy), Some(3));

        // A late joiner gets everything in one message.
        let (mut late, mut late_replica) =
            (world(), Replica::new(&replication));
        late_replica
            .apply(&mut late, HOST, &replication.full())
            .unwrap()
            .unwrap();
        let mut healths = late.query::<&Health>();
        assert_eq!(
            healths.iter(&late).map(|h| h.value).collect::<Vec<_>>(),
            [3]
        );

        host.entity_mut(raft).remove::<Health>();
        let removed = replication.delta(&mut host).unwrap().unwrap();
        replica.apply(&mut client, HOST, &removed).unwrap().unwrap();
        assert_eq!(health(&client, copy), None);

        host.despawn(raft);
        let gone = replication.delta(&mut host).unwrap().unwrap();
        replica.apply(&mut client, HOST, &gone).unwrap().unwrap();
        assert!(client.get_entity(copy).is_err());
        assert_eq!(replica.apply(&mut client, HOST, b"game bytes"), None);
        assert!(replica
            .apply(&mut client, HOST, b"\0repnot json")
            .unwrap()
            .is_err());
    }

    #[test]
    fn objects_from_the_same_scene_update_in_place() {
        let (mut host, mut client) = (world(), world());
        let id = SceneId::new();
        host.spawn((Replicated, id, Transform::new([5.0, 0.0, 0.0])));
        let on_client =
            client.spawn((Replicated, id, Transform::default())).id();
        let mut replication = Replication::new();
        replication.component(TRANSFORM);
        let message = replication.delta(&mut host).unwrap().unwrap();
        Replica::new(&replication)
            .apply(&mut client, HOST, &message)
            .unwrap()
            .unwrap();
        assert_eq!(client.query::<&SceneId>().iter(&client).count(), 1);
        assert_eq!(
            client.get::<Transform>(on_client).unwrap().position[0],
            5.0
        );
    }

    #[test]
    fn a_replica_refuses_what_the_host_may_not_send() {
        let mut client = world();
        let mut replication = Replication::new();
        replication.component(TRANSFORM);
        let mut replica = Replica::new(&replication);
        let local = SceneId::new();
        let menu = client.spawn((local, Name("Menu".into()))).id();
        let message = |patch: Value| encode(patch.as_object().unwrap());
        let new = Uuid::new_v4().to_string();

        let spawn = message(json!({ &new: {"transform": {}} }));
        assert!(replica.apply(&mut client, 1, &spawn).unwrap().is_err());
        let health = message(json!({ &new: {"rusting.health": {}} }));
        assert!(replica.apply(&mut client, HOST, &health).unwrap().is_err());
        let despawn = message(json!({ local.0.to_string(): null }));
        assert!(replica.apply(&mut client, HOST, &despawn).unwrap().is_err());
        assert!(client.get_entity(menu).is_ok());
        assert_eq!(client.query::<&SceneId>().iter(&client).count(), 1);

        let flood: Map<String, Value> = (0..=MAX_OBJECTS)
            .map(|_| (Uuid::new_v4().to_string(), json!({})))
            .collect();
        let refused = replica.apply(&mut client, HOST, &encode(&flood));
        assert!(refused.unwrap().is_err());
        assert_eq!(client.query::<&SceneId>().iter(&client).count(), 1);

        let id = Uuid::new_v4();
        let twice = message(json!({
            id.to_string(): {},
            id.to_string().to_uppercase(): {},
        }));
        assert!(replica.apply(&mut client, HOST, &twice).unwrap().is_err());
        assert_eq!(client.query::<&SceneId>().iter(&client).count(), 1);
    }
}
