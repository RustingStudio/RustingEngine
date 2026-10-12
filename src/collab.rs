//! Live collaboration: several editors share one project through a host.
//!
//! The project owner hosts with [`NetSession`](crate::net::NetSession); guests join by address or
//! relay room code. Every message is encrypted end to end with the
//! session key the host shows ([`SecureNet`]). The host streams the project to each guest, then orders
//! every edit: scene edits travel as per-field [`ops::FieldOp`] batches, and
//! other files as whole files. The host applies each batch and broadcasts it
//! to every peer, the sender included, so all peers end in host order.
//!
//! This module holds the protocol and the parts that need no editor; the
//! editor's Collaboration area drives it each frame.

use std::collections::{BTreeMap, HashSet};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use bincode::Options;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::net::{PeerId, MAX_MESSAGE};
use crate::runtime::{
    absolutize_scene_assets, relativize_scene_assets, validate_scene_structure,
    SceneDocument,
};

pub mod files;
pub mod ops;
pub mod secure;

pub use secure::SecureNet;

use ops::FieldOp;

/// Bytes per `FileData` message, well under [`MAX_MESSAGE`].
pub const FILE_CHUNK: usize = 4 << 20;

/// Everything collaborators send each other.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Message {
    /// A guest's first message, naming its user.
    Hello { name: String },
    /// Part of a project file, guest-bound during a join. `append` adds
    /// to the file instead of starting it.
    FileData {
        path: String,
        bytes: Vec<u8>,
        append: bool,
    },
    /// The whole project has been sent; the guest can open it.
    FilesDone,
    /// A guest asks for a scene's current state before editing it.
    SceneRequest { scene: String },
    /// A scene's current state, as JSON text with scene-relative assets.
    SceneState { scene: String, doc: String },
    /// Field operations on a scene. Guests send them to the host; the host
    /// broadcasts each accepted batch to every peer in its order, with
    /// `origin` set to the peer that made it.
    Batch {
        origin: PeerId,
        scene: String,
        ops: Vec<FieldOp>,
    },
    /// The host refused a batch; a `SceneState` follows to resync.
    Rejected { scene: String, reason: String },
    /// A whole non-scene file, written as is.
    FilePut { path: String, bytes: Vec<u8> },
    /// A file was deleted.
    FileDelete { path: String },
    /// Who a peer is, what it has selected and where its camera is.
    Presence {
        peer: PeerId,
        name: String,
        presence: Presence,
    },
    /// A peer left the session.
    Left { peer: PeerId },
}

fn wire() -> impl Options {
    // Room for encryption: 18 bytes per 64 KiB piece, under 64 KiB in all.
    bincode::DefaultOptions::new().with_limit((MAX_MESSAGE - (64 << 10)) as u64)
}

#[must_use]
pub fn encode(message: &Message) -> Vec<u8> {
    wire().serialize(message).unwrap_or_default()
}

/// `None` for bytes that are not a message, which callers drop.
#[must_use]
pub fn decode(bytes: &[u8]) -> Option<Message> {
    wire().deserialize(bytes).ok()
}

pub fn send(net: &SecureNet, to: PeerId, message: &Message) -> io::Result<()> {
    net.send(to, &encode(message))
}

pub fn broadcast(net: &SecureNet, message: &Message) -> io::Result<()> {
    net.broadcast(&encode(message))
}

/// Sends every shared file under `root` to `peer` in [`FILE_CHUNK`] pieces,
/// then `FilesDone`. Blocks until the bytes are written to the socket, so
/// the editor runs it on a thread; `stop` ends it between chunks.
pub fn stream_project(
    net: &SecureNet,
    peer: PeerId,
    root: &Path,
    stop: &AtomicBool,
) -> io::Result<()> {
    let stopped = || {
        if stop.load(Ordering::Relaxed) {
            Err(io::Error::from(io::ErrorKind::Interrupted))
        } else {
            Ok(())
        }
    };
    for path in files::scan_project(root).into_keys() {
        stopped()?;
        let bytes = std::fs::read(root.join(&path))?;
        let mut chunks = bytes.chunks(FILE_CHUNK);
        let first = chunks.next().unwrap_or_default();
        send(
            net,
            peer,
            &Message::FileData {
                path: path.clone(),
                bytes: first.to_vec(),
                append: false,
            },
        )?;
        for chunk in chunks {
            stopped()?;
            send(
                net,
                peer,
                &Message::FileData {
                    path: path.clone(),
                    bytes: chunk.to_vec(),
                    append: true,
                },
            )?;
        }
    }
    send(net, peer, &Message::FilesDone)
}

/// Writes a file a peer sent under `root`, refusing unsafe paths.
pub fn write_received(
    root: &Path,
    path: &str,
    bytes: &[u8],
    append: bool,
) -> Result<(), String> {
    let relative = files::shareable(path)
        .ok_or_else(|| format!("refused path {path:?}"))?;
    let target = files::inside_root(root, &relative)
        .ok_or_else(|| format!("path {path:?} leaves the project"))?;
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let result = if append {
        use std::io::Write;
        std::fs::OpenOptions::new()
            .append(true)
            .open(&target)
            .and_then(|mut file| file.write_all(bytes))
    } else {
        std::fs::write(&target, bytes)
    };
    result.map_err(|e| format!("{path}: {e}"))
}

/// Deletes a file a peer deleted, refusing the paths [`write_received`]
/// refuses. A file that is already gone is not an error.
pub fn delete_received(root: &Path, path: &str) -> Result<(), String> {
    let relative = files::shareable(path)
        .ok_or_else(|| format!("refused path {path:?}"))?;
    let target = files::inside_root(root, &relative)
        .ok_or_else(|| format!("path {path:?} leaves the project"))?;
    match std::fs::remove_file(&target) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => {
            Err(format!("{path}: {error}"))
        }
        _ => Ok(()),
    }
}

/// A scene document as the JSON that peers exchange: asset paths relative
/// to the scene's folder, so they mean the same file on every machine.
pub fn to_shared(
    mut document: SceneDocument,
    scene_folder: &Path,
) -> Result<Value, String> {
    relativize_scene_assets(&mut document, scene_folder)
        .map_err(|e| e.to_string())?;
    serde_json::to_value(document).map_err(|e| e.to_string())
}

/// The inverse of [`to_shared`]: checks the structure and makes asset
/// paths absolute again.
pub fn from_shared(
    value: Value,
    scene_folder: &Path,
) -> Result<SceneDocument, String> {
    let mut document: SceneDocument =
        serde_json::from_value(value).map_err(|e| e.to_string())?;
    validate_scene_structure(&document).map_err(|e| e.to_string())?;
    absolutize_scene_assets(&mut document, scene_folder)
        .map_err(|e| e.to_string())?;
    Ok(document)
}

/// Applies `ops` to a copy of `doc` and checks the result loads, so a bad
/// batch never reaches the shared state.
pub fn checked_apply(doc: &Value, ops: &[FieldOp]) -> Result<Value, String> {
    let mut next = doc.clone();
    ops::apply_ops(&mut next, ops, &HashSet::new());
    let document: SceneDocument =
        serde_json::from_value(next.clone()).map_err(|e| e.to_string())?;
    validate_scene_structure(&document).map_err(|e| e.to_string())?;
    Ok(next)
}

/// The scene at `root/scene` in shared form, read from disk.
pub fn read_shared_scene(root: &Path, scene: &str) -> Result<Value, String> {
    let path = scene_path(root, scene)?;
    let document = crate::runtime::read_scene_document(&path)
        .map_err(|e| e.to_string())?;
    // Files hold scene-relative paths already; this only normalizes them.
    let folder = path.parent().unwrap_or(root);
    let mut absolute = document;
    absolutize_scene_assets(&mut absolute, folder)
        .map_err(|e| e.to_string())?;
    to_shared(absolute, folder)
}

/// Writes a shared-form scene to `root/scene` atomically.
pub fn write_shared_scene(
    root: &Path,
    scene: &str,
    doc: &Value,
) -> Result<(), String> {
    let path = scene_path(root, scene)?;
    let bytes = serde_json::to_vec_pretty(doc).map_err(|e| e.to_string())?;
    crate::runtime::write_atomic(&path, &bytes).map_err(|e| e.to_string())
}

/// `root/scene` for a scene path a peer named, checked like any file.
pub fn scene_path(root: &Path, scene: &str) -> Result<PathBuf, String> {
    let relative = files::shareable(scene)
        .filter(|_| files::is_scene(scene))
        .ok_or_else(|| format!("not a scene path: {scene:?}"))?;
    files::inside_root(root, &relative)
        .ok_or_else(|| format!("scene {scene:?} leaves the project"))
}

/// Folder that holds a guest's copy of the project for one session key
/// (an address or room code).
#[must_use]
pub fn mirror_root(key: &str) -> PathBuf {
    let cache = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME").map(|home| Path::new(&home).join(".cache"))
        })
        .or_else(|| std::env::var_os("LOCALAPPDATA").map(PathBuf::from))
        .unwrap_or_else(std::env::temp_dir);
    let key: String = key
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    cache.join("rusting").join("collab").join(key)
}

/// What a peer is looking at.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Presence {
    /// Open scene path.
    pub scene: String,
    /// Selected entity IDs.
    pub selection: Vec<String>,
    /// The editor camera's world matrix, columns first.
    pub camera: Option<[[f32; 4]; 4]>,
}

/// Names of the peers of a session, the host's included.
pub type PeerNames = BTreeMap<PeerId, String>;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::{NetEvent, NetSession, HOST};

    #[test]
    fn messages_round_trip() {
        let message = Message::Batch {
            origin: 1,
            scene: "scenes/main.rscene".into(),
            ops: vec![FieldOp {
                entity: Some("a".into()),
                field: "transform".into(),
                value: Some("{\"position\":[1,2,3]}".into()),
            }],
        };
        assert_eq!(decode(&encode(&message)), Some(message));
        assert_eq!(decode(b"\xff\xff garbage"), None);
    }

    fn temp(name: &str) -> PathBuf {
        let root = std::env::temp_dir()
            .join(format!("rusting-collab-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    #[test]
    fn project_streams_byte_exact_in_chunks() {
        let (host, mut guests) = NetSession::loopback(1);
        let key = secure::new_key();
        let host = SecureNet::new(host, &key).unwrap();
        let guest = SecureNet::new(guests.remove(0), &key).unwrap();
        let _ = host.poll();
        let _ = guest.poll();
        let source = temp("source");
        let mirror = temp("mirror");
        std::fs::create_dir_all(source.join("assets")).unwrap();
        std::fs::create_dir_all(source.join("target")).unwrap();
        let big: Vec<u8> = (0..9 << 20).map(|i| (i % 251) as u8).collect();
        std::fs::write(source.join("assets/big.bin"), &big).unwrap();
        std::fs::write(source.join("project.json"), "{}").unwrap();
        std::fs::write(source.join("target/skip"), "x").unwrap();
        let _ = host.poll();
        stream_project(&host, 1, &source, &AtomicBool::new(false)).unwrap();
        let mut done = false;
        let mut chunks = 0;
        for event in guest.poll() {
            let NetEvent::Message { from: HOST, bytes } = event else {
                continue;
            };
            match decode(&bytes).unwrap() {
                Message::FileData {
                    path,
                    bytes,
                    append,
                } => {
                    chunks += 1;
                    write_received(&mirror, &path, &bytes, append).unwrap();
                }
                Message::FilesDone => done = true,
                other => panic!("unexpected {other:?}"),
            }
        }
        assert!(done);
        assert_eq!(chunks, 4, "3 chunks of big.bin and project.json");
        assert_eq!(std::fs::read(mirror.join("assets/big.bin")).unwrap(), big);
        assert!(!mirror.join("target").exists());
        std::fs::remove_dir_all(&source).unwrap();
        std::fs::remove_dir_all(&mirror).unwrap();
    }

    #[test]
    fn received_files_cannot_leave_the_root() {
        let root = temp("escape");
        assert!(write_received(&root, "../evil", b"x", false).is_err());
        assert!(write_received(&root, "/tmp/evil", b"x", false).is_err());
        assert!(write_received(&root, ".git/hooks/pre-commit", b"x", false)
            .is_err());
        assert!(!root.join(".git").exists());
        assert!(write_received(&root, "ok/file.txt", b"x", false).is_ok());
        assert!(delete_received(&root, "../evil").is_err());
        assert!(delete_received(&root, "ok/file.txt").is_ok());
        assert!(!root.join("ok/file.txt").exists());
        assert!(
            delete_received(&root, "ok/file.txt").is_ok(),
            "already gone"
        );
        assert!(scene_path(&root, "notes.txt").is_err());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn checked_apply_rejects_duplicate_entities() {
        let id = uuid::Uuid::new_v4().to_string();
        let doc = serde_json::json!({
            "format_version": 9, "name": "S",
            "entities": [{"id": id, "name": "A"}]
        });
        let ops = vec![FieldOp {
            entity: Some("other".into()),
            field: "entity".into(),
            value: Some(serde_json::json!({"id": id, "name": "B"}).to_string()),
        }];
        assert!(checked_apply(&doc, &ops).is_err());
    }
}
