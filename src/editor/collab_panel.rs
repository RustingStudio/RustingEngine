//! Collaboration area and the per-frame sync of a live session.
//!
//! The protocol lives in [`crate::collab`]. Each frame [`collab_tick`]
//! handles network events, sends the open scene's changes as field
//! operations at 10 Hz, autosaves on the host, and syncs other project files
//! once a second.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use bevy_ecs::prelude::{Resource, World};
use serde_json::Value;

use crate::collab::files::{self, FileWatcher};
use crate::collab::ops::{self, FieldKey, FieldOp};
use crate::collab::{self, Message, PeerNames, Presence, SecureNet};
use crate::net::{NetEvent, NetSession, PeerId, HOST};
use crate::runtime::{scene_document, SceneDocument, SceneLoadMode};

use super::gui_elements::{kit, EditorTheme};
use super::{
    BuildRequest, ConsoleLevel, EditorConsole, EditorGizmoDrag, EditorHistory,
    EditorIcon, EditorMode, EditorState,
};

/// How often the open scene is diffed and its changes sent.
const SCENE_POLL: Duration = Duration::from_millis(100);
/// How often project files are checked for changes.
const FILE_POLL: Duration = Duration::from_secs(1);
/// How long the host waits after an accepted edit before writing the scene.
const AUTOSAVE_DELAY: Duration = Duration::from_secs(1);
/// Remote operations kept for Undo snapshots before they are applied.
const REBASE_LIMIT: usize = 1000;
/// Largest non-scene file sent while editing, under the message limit.
const MAX_FILE_PUT: u64 = 3 * collab::FILE_CHUNK as u64;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Link {
    #[default]
    Direct,
    Relay,
}

/// The Collaboration area's form and the running session, if any.
#[derive(Resource)]
pub(super) struct CollabState {
    name: String,
    link: Link,
    port: String,
    /// The session key: made when hosting, typed when joining.
    key: String,
    address: String,
    relay: String,
    token: String,
    code: String,
    status: Option<String>,
    /// Leave was pressed; the next tick ends the session.
    leave: bool,
    session: Option<Session>,
    /// Project folders an ended session left with code from others that
    /// the user did not trust; their builds still ask.
    untrusted: HashSet<PathBuf>,
    /// A build waiting for the trust prompt's answer, with the untrusted
    /// folder it builds when no session is open.
    pending_build: Option<(Option<PathBuf>, BuildRequest)>,
}

impl Default for CollabState {
    fn default() -> Self {
        let name = std::env::var("USER")
            .or_else(|_| std::env::var("USERNAME"))
            .unwrap_or_else(|_| "Guest".into());
        Self {
            name,
            link: Link::Direct,
            port: "7777".into(),
            key: String::new(),
            address: "127.0.0.1:7777".into(),
            relay: String::new(),
            token: String::new(),
            code: String::new(),
            status: None,
            leave: false,
            session: None,
            untrusted: HashSet::new(),
            pending_build: None,
        }
    }
}

struct Session {
    net: Arc<SecureNet>,
    /// Canonical project folder: the real project on the host, the mirror on
    /// a guest.
    root: PathBuf,
    /// The editor's project root text once this session's project is open.
    project: Option<String>,
    names: PeerNames,
    /// What each other peer is looking at.
    presence: BTreeMap<PeerId, Presence>,
    /// This editor's last sent presence, and when.
    sent_presence: Presence,
    last_presence: Instant,
    /// Project copies the host is sending to new guests, and their stop
    /// flag.
    streams: Vec<JoinHandle<std::io::Result<()>>>,
    stop: Arc<AtomicBool>,
    /// Open scene path and its last state shared with the host. A guest's
    /// copy includes its own batches the host has not echoed yet.
    synced: Option<(String, Value)>,
    /// Scene a guest asked the host for; its batches wait for the answer.
    awaiting: Option<String>,
    /// Keys of this guest's batches the host has not echoed yet, oldest
    /// first. Remote writes to them are skipped: these batches come later in
    /// host order and win.
    inflight: VecDeque<HashSet<FieldKey>>,
    /// Remote operations not yet applied to the Undo snapshots.
    rebase: Vec<FieldOp>,
    last_scene_poll: Instant,
    last_file_poll: Instant,
    /// When the host first applied an edit it has not written to disk.
    unsaved_since: Option<Instant>,
    watcher: FileWatcher,
    /// Project files came from another peer: always on a guest, and on the
    /// host once a guest sent a file.
    foreign_code: bool,
    /// The user agreed to run this session's code.
    trusted: bool,
}

impl Session {
    fn new(net: SecureNet, root: PathBuf, name: &str) -> Self {
        let now = Instant::now();
        Self {
            names: PeerNames::from([(net.id(), name.to_owned())]),
            foreign_code: net.id() != HOST,
            net: Arc::new(net),
            root,
            project: None,
            presence: BTreeMap::new(),
            sent_presence: Presence::default(),
            last_presence: now,
            streams: Vec::new(),
            stop: Arc::new(AtomicBool::new(false)),
            synced: None,
            awaiting: None,
            inflight: VecDeque::new(),
            rebase: Vec::new(),
            last_scene_poll: now,
            last_file_poll: now,
            unsaved_since: None,
            trusted: false,
            watcher: FileWatcher::default(),
        }
    }

    fn is_host(&self) -> bool {
        self.net.id() == HOST
    }

    fn scene_folder(&self, scene: &str) -> PathBuf {
        let path = self.root.join(scene);
        path.parent().unwrap_or(&self.root).to_path_buf()
    }

    /// Sends to the host, or to every guest when this is the host.
    fn share(&self, message: &Message) -> Result<(), String> {
        collab::broadcast(&self.net, message).map_err(|e| e.to_string())
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

fn log(world: &mut World, level: ConsoleLevel, message: impl Into<String>) {
    if let Some(mut console) = world.get_resource_mut::<EditorConsole>() {
        console.push_from(level, "Collaboration", message);
    }
}

/// Runs one frame of the session. Returns a project folder to open: the
/// guest's mirror once the host finished sending it.
pub(super) fn collab_tick(
    world: &mut World,
    state: &mut EditorState,
    history: &mut EditorHistory,
) -> Option<PathBuf> {
    let mut collab = world.remove_resource::<CollabState>()?;
    let mut open = None;
    let mut ended = None;
    if std::mem::take(&mut collab.leave) {
        if let Some(mut session) = collab.session.take() {
            flush(world, &mut session);
            collab.status = Some("Left the session".into());
            ended = Some(session);
        }
    }
    if let Some(session) = &mut collab.session {
        match tick(world, state, history, session) {
            Ok(request) => open = request,
            Err(reason) => {
                log(world, ConsoleLevel::Warning, &reason);
                if session.is_host() {
                    flush(world, session);
                }
                collab.status = Some(reason);
                ended = collab.session.take();
            }
        }
    }
    if let Some(session) = ended.filter(|s| s.foreign_code && !s.trusted) {
        collab.untrusted.insert(session.root.clone());
    }
    world.insert_resource(collab);
    open
}

fn tick(
    world: &mut World,
    state: &mut EditorState,
    history: &mut EditorHistory,
    s: &mut Session,
) -> Result<Option<PathBuf>, String> {
    let mut open = None;
    match &s.project {
        Some(text) if *text != state.project_root => {
            return Err("Another project was opened; left the session".into());
        }
        None if !state.project_root.is_empty()
            && Path::new(&state.project_root).canonicalize().ok().as_ref()
                == Some(&s.root) =>
        {
            s.project = Some(state.project_root.clone());
            s.watcher = FileWatcher::new(&s.root);
        }
        _ => {}
    }
    let (done, sending) = std::mem::take(&mut s.streams)
        .into_iter()
        .partition::<Vec<_>, _>(JoinHandle::is_finished);
    s.streams = sending;
    for stream in done {
        if let Ok(Err(error)) = stream.join() {
            log(
                world,
                ConsoleLevel::Error,
                format!("Could not send the project: {error}"),
            );
        }
    }
    if s.project.is_some() {
        follow_open_scene(world, state, history, s)?;
    }
    // A reload respawns every entity, so it waits for drags to finish.
    let busy = world
        .get_resource::<EditorGizmoDrag>()
        .is_some_and(EditorGizmoDrag::is_active)
        || state.tile_stroke_undo.is_some();
    if !busy {
        for event in s.net.poll() {
            match event {
                NetEvent::Connected(_) => {}
                NetEvent::Disconnected(peer) => {
                    s.presence.remove(&peer);
                    if let Some(name) = s.names.remove(&peer) {
                        log(world, ConsoleLevel::Info, format!("{name} left"));
                    }
                    let _ = s.share(&Message::Left { peer });
                }
                NetEvent::Closed => {
                    return Err(if s.net.refused() {
                        "The host refused the session key".into()
                    } else if s.is_host() {
                        "The session closed".into()
                    } else {
                        format!(
                            "The host left; the copy stays at {}",
                            s.root.display()
                        )
                    });
                }
                NetEvent::Message { from, bytes } => {
                    let Some(message) = collab::decode(&bytes) else {
                        continue;
                    };
                    if s.is_host() {
                        host_message(world, state, history, s, from, message);
                    } else if from == HOST {
                        if let Some(path) =
                            guest_message(world, state, history, s, message)
                        {
                            open = Some(path);
                        }
                    }
                }
            }
        }
    }
    send_local_changes(world, state, s);
    send_presence(world, state, s);
    if s.unsaved_since
        .is_some_and(|since| since.elapsed() >= AUTOSAVE_DELAY)
    {
        flush(world, s);
    }
    if s.project.is_some() && s.last_file_poll.elapsed() >= FILE_POLL {
        s.last_file_poll = Instant::now();
        send_changed_files(world, s);
    }
    Ok(open)
}

/// Starts syncing the scene the editor has open when it changes.
fn follow_open_scene(
    world: &mut World,
    state: &EditorState,
    history: &mut EditorHistory,
    s: &mut Session,
) -> Result<(), String> {
    let open = Some(state.scene_path.clone())
        .filter(|scene| collab::scene_path(&s.root, scene).is_ok());
    let current = s
        .synced
        .as_ref()
        .map(|(scene, _)| scene.clone())
        .or_else(|| s.awaiting.clone());
    if open == current {
        return Ok(());
    }
    flush(world, s);
    s.rebase.clear();
    s.inflight.clear();
    s.synced = None;
    s.awaiting = None;
    let Some(scene) = open else { return Ok(()) };
    if s.is_host() {
        if let Ok(doc) = live_shared(world, state, s, &scene) {
            s.synced = Some((scene, doc));
        }
    } else {
        // A scene this guest made reaches the host first; the host keeps its
        // own copy of any scene it has.
        if let Some(put) = file_put(world, s, &scene) {
            collab::send(&s.net, HOST, &put).map_err(|e| e.to_string())?;
        }
        // The mirror's copy may be stale; the host's answer replaces it.
        collab::send(
            &s.net,
            HOST,
            &Message::SceneRequest {
                scene: scene.clone(),
            },
        )
        .map_err(|e| e.to_string())?;
        s.awaiting = Some(scene);
        history.undo.clear();
        history.redo.clear();
    }
    Ok(())
}

fn host_message(
    world: &mut World,
    state: &mut EditorState,
    history: &mut EditorHistory,
    s: &mut Session,
    from: PeerId,
    message: Message,
) {
    match message {
        Message::Hello { name } => {
            log(world, ConsoleLevel::Info, format!("{name} joined"));
            s.names.insert(from, name);
            let mut known = vec![presence_message(s, HOST, &s.sent_presence)];
            known.extend(
                s.presence
                    .iter()
                    .map(|(peer, seen)| presence_message(s, *peer, seen)),
            );
            for message in &known {
                let _ = collab::send(&s.net, from, message);
            }
            let (net, root, stop) =
                (s.net.clone(), s.root.clone(), s.stop.clone());
            // ponytail: a file the host changes while it is being read can
            // reach the guest as the older copy after its newer `FilePut`.
            // Pause the file poll during joins if that shows up.
            s.streams.push(std::thread::spawn(move || {
                collab::stream_project(&net, from, &root, &stop)
            }));
        }
        Message::SceneRequest { scene } => {
            let _ = collab::send(&s.net, from, &scene_state(s, &scene));
        }
        Message::Batch { scene, ops, .. } => {
            let result = match &s.synced {
                Some((open, doc)) if *open == scene => {
                    collab::checked_apply(doc, &ops).and_then(|_| {
                        apply_remote(world, state, s, &ops, &HashSet::new())
                    })
                }
                _ => collab::read_shared_scene(&s.root, &scene)
                    .and_then(|doc| collab::checked_apply(&doc, &ops))
                    .and_then(|doc| {
                        collab::write_shared_scene(&s.root, &scene, &doc)
                    }),
            };
            match result {
                Ok(()) => {
                    let _ = s.share(&Message::Batch {
                        origin: from,
                        scene,
                        ops,
                    });
                }
                Err(reason) => {
                    let name = s.names.get(&from).cloned().unwrap_or_default();
                    log(
                        world,
                        ConsoleLevel::Warning,
                        format!("Rejected an edit from {name}: {reason}"),
                    );
                    let rejected = Message::Rejected {
                        scene: scene.clone(),
                        reason,
                    };
                    let _ = collab::send(&s.net, from, &rejected);
                    let _ = collab::send(&s.net, from, &scene_state(s, &scene));
                }
            }
            flush_rebase_if_long(s, history);
        }
        Message::FilePut { path, bytes }
            if receive_file(world, state, s, &path, &bytes) =>
        {
            let _ = s.share(&Message::FilePut { path, bytes });
        }
        Message::FileDelete { path } if receive_delete(world, s, &path) => {
            let _ = s.share(&Message::FileDelete { path });
        }
        Message::Presence { presence, .. } => {
            let _ = s.share(&presence_message(s, from, &presence));
            s.presence.insert(from, presence);
        }
        _ => {}
    }
}

fn guest_message(
    world: &mut World,
    state: &mut EditorState,
    history: &mut EditorHistory,
    s: &mut Session,
    message: Message,
) -> Option<PathBuf> {
    match message {
        Message::FileData {
            path,
            bytes,
            append,
        } => {
            if let Err(error) =
                collab::write_received(&s.root, &path, &bytes, append)
            {
                log(world, ConsoleLevel::Error, error);
            }
        }
        Message::FilesDone => return Some(s.root.clone()),
        Message::SceneState { scene, doc } => {
            if s.awaiting.as_ref() != Some(&scene) {
                return None;
            }
            let Ok(doc) = serde_json::from_str::<Value>(&doc) else {
                return None;
            };
            s.awaiting = None;
            s.inflight.clear();
            s.rebase.clear();
            history.undo.clear();
            history.redo.clear();
            history.pending_inspector = None;
            if let Err(error) = apply_live(world, state, s, &scene, doc.clone())
            {
                log(world, ConsoleLevel::Error, error);
            }
            s.synced = Some((scene, doc));
        }
        Message::Batch { origin, scene, ops } => {
            if s.synced.as_ref().is_none_or(|(open, _)| *open != scene) {
                return None;
            }
            if origin == s.net.id() && s.inflight.pop_front().is_some() {
                return None;
            }
            let inflight: HashSet<FieldKey> =
                s.inflight.iter().flatten().cloned().collect();
            if let Err(error) = apply_remote(world, state, s, &ops, &inflight) {
                log(world, ConsoleLevel::Error, error);
            }
            flush_rebase_if_long(s, history);
        }
        Message::Rejected { scene, reason } => {
            log(
                world,
                ConsoleLevel::Warning,
                format!("The host rejected an edit to {scene}: {reason}"),
            );
            if s.synced.as_ref().is_some_and(|(open, _)| *open == scene) {
                s.synced = None;
                s.inflight.clear();
                s.awaiting = Some(scene);
            }
        }
        Message::FilePut { path, bytes } => {
            receive_file(world, state, s, &path, &bytes);
        }
        Message::FileDelete { path } => {
            receive_delete(world, s, &path);
        }
        Message::Presence {
            peer,
            name,
            presence,
        } => {
            if peer != s.net.id() {
                s.names.insert(peer, name);
                s.presence.insert(peer, presence);
            }
        }
        Message::Left { peer } => {
            s.names.remove(&peer);
            s.presence.remove(&peer);
        }
        Message::Hello { .. } | Message::SceneRequest { .. } => {}
    }
    None
}

fn presence_message(s: &Session, peer: PeerId, presence: &Presence) -> Message {
    Message::Presence {
        peer,
        name: s.names.get(&peer).cloned().unwrap_or_default(),
        presence: presence.clone(),
    }
}

/// Tells the others when this editor's scene, selection or camera changed,
/// at most at the scene poll rate.
fn send_presence(world: &World, state: &EditorState, s: &mut Session) {
    if s.project.is_none() || s.last_presence.elapsed() < SCENE_POLL {
        return;
    }
    let now = Presence {
        scene: state.scene_path.clone(),
        selection: state
            .selection
            .iter()
            .filter_map(|&entity| world.get::<crate::runtime::SceneId>(entity))
            .map(|id| id.0.to_string())
            .collect(),
        camera: state
            .editor_camera
            .and_then(|camera| {
                world.get::<crate::runtime::GlobalTransform>(camera)
            })
            .map(|transform| transform.matrix),
    };
    if now != s.sent_presence {
        s.last_presence = Instant::now();
        s.sent_presence = now;
        let _ = s.share(&presence_message(s, s.net.id(), &s.sent_presence));
    }
}

/// Other people in the open scene, with their marker colors.
fn others_here(s: &Session) -> impl Iterator<Item = (PeerId, &Presence)> {
    let open = s.synced.as_ref().map(|(scene, _)| scene);
    s.presence
        .iter()
        .filter(move |(_, presence)| Some(&presence.scene) == open)
        .map(|(peer, presence)| (*peer, presence))
}

fn session(world: &World) -> Option<&Session> {
    world.get_resource::<CollabState>()?.session.as_ref()
}

/// Editor cameras of the others in this scene, as world matrices with the
/// person's marker color.
pub(super) fn remote_cameras(world: &World) -> Vec<([[f32; 4]; 4], [f32; 4])> {
    let Some(s) = session(world) else {
        return Vec::new();
    };
    others_here(s)
        .filter_map(|(peer, presence)| {
            let color = egui::Rgba::from(peer_color(peer)).to_array();
            presence.camera.map(|camera| (camera, color))
        })
        .collect()
}

/// A peer's marker color, from the theme.
fn peer_color(peer: PeerId) -> egui::Color32 {
    let palette = [
        EditorTheme::AXIS[0],
        EditorTheme::AXIS[1],
        EditorTheme::AXIS[2],
        EditorTheme::WARNING,
        EditorTheme::LINK,
    ];
    palette[peer as usize % palette.len()]
}

/// Entities other people in this scene have selected, by scene ID, with
/// their names and marker colors.
pub(super) fn remote_selection(
    world: &World,
) -> HashMap<uuid::Uuid, Vec<(String, egui::Color32)>> {
    let mut marks: HashMap<_, Vec<_>> = HashMap::new();
    let Some(s) = session(world) else {
        return marks;
    };
    for (peer, presence) in others_here(s) {
        let name = s.names.get(&peer).cloned().unwrap_or_default();
        for id in presence.selection.iter().filter_map(|id| id.parse().ok()) {
            marks
                .entry(id)
                .or_default()
                .push((name.clone(), peer_color(peer)));
        }
    }
    marks
}

/// Lets `request` run unless it would build code other peers sent that
/// the user has not trusted, in this session or one that ended in the
/// project at `project_root`; then it waits for the prompt.
pub(super) fn gate_build(
    world: &mut World,
    project_root: &str,
    request: BuildRequest,
) -> Option<BuildRequest> {
    let mut collab = world.resource_mut::<CollabState>();
    let ended = Path::new(project_root)
        .canonicalize()
        .ok()
        .filter(|root| collab.untrusted.contains(root));
    let untrusted = match &collab.session {
        Some(s) => s.foreign_code && !s.trusted,
        None => ended.is_some(),
    };
    if !untrusted {
        return Some(request);
    }
    collab.pending_build =
        Some((ended.filter(|_| collab.session.is_none()), request));
    None
}

/// Asks once per session before code from other peers is built or run.
/// Returns the waiting build when the user trusts the session.
pub(super) fn draw_trust_prompt(
    context: &egui::Context,
    world: &mut World,
) -> Option<BuildRequest> {
    let mut collab = world.resource_mut::<CollabState>();
    collab.pending_build.as_ref()?;
    let mut answer = None;
    egui::Modal::new(egui::Id::new("collab_trust_prompt")).show(
        context,
        |ui| {
            ui.set_width(360.0);
            ui.heading("Run Shared Code?");
            ui.label(
                "This project holds code from other people in the session. \
             Building or running it can do anything your user account can \
             do. Run it only if you trust everyone in the session.",
            );
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if kit::action_button(
                    ui,
                    EditorIcon::Play,
                    "Trust and Run",
                    true,
                    true,
                )
                .clicked()
                {
                    answer = Some(true);
                }
                if ui.button("Cancel").clicked() {
                    answer = Some(false);
                }
            });
        },
    );
    let answer = answer?;
    let (ended, request) = collab.pending_build.take()?;
    if !answer {
        return None;
    }
    match (collab.session.as_mut(), ended) {
        (Some(s), _) => s.trusted = true,
        (None, Some(root)) => {
            collab.untrusted.remove(&root);
        }
        (None, None) => {}
    }
    Some(request)
}

/// The host's current state of `scene`, or why it has none.
fn scene_state(s: &Session, scene: &str) -> Message {
    let doc = match &s.synced {
        Some((open, doc)) if open == scene => Ok(doc.clone()),
        _ => collab::read_shared_scene(&s.root, scene),
    };
    match doc {
        Ok(doc) => Message::SceneState {
            scene: scene.to_owned(),
            doc: doc.to_string(),
        },
        Err(reason) => Message::Rejected {
            scene: scene.to_owned(),
            reason,
        },
    }
}

/// The editor's authored scene in shared form: the live world while
/// editing, the snapshot that Stop restores while playing.
fn live_shared(
    world: &mut World,
    state: &EditorState,
    s: &Session,
    scene: &str,
) -> Result<Value, String> {
    let document = if state.mode == EditorMode::Edit {
        scene_document(world, "Main Scene").map_err(|e| e.to_string())?
    } else {
        state
            .play_snapshot
            .clone()
            .ok_or("the scene is playing without a snapshot")?
    };
    collab::to_shared(document, &s.scene_folder(scene))
}

/// Replaces the authored scene with `doc`, keeping the selection.
fn apply_live(
    world: &mut World,
    state: &mut EditorState,
    s: &Session,
    scene: &str,
    doc: Value,
) -> Result<(), String> {
    let document = collab::from_shared(doc, &s.scene_folder(scene))?;
    if state.mode != EditorMode::Edit {
        state.play_snapshot = Some(document);
        return Ok(());
    }
    super::view::reload_keeping_selection(world, state, |world| {
        crate::runtime::load_scene_document(
            world,
            &document,
            SceneLoadMode::Replace,
        )
    })
    .map_err(|e| e.to_string())?;
    super::timeline::scene_replaced(world);
    Ok(())
}

/// Sets moves, renames and visibility changes on the live entities, the
/// common edits while someone drags. Returns false, changing nothing, when
/// `ops` hold any other edit, which needs a reload.
fn patch_in_place(world: &mut World, ops: &[FieldOp]) -> bool {
    use crate::runtime::{Name, SceneId, SceneTransform, Visibility};
    enum Patch {
        Transform(crate::Transform),
        Name(String),
        Visible(bool),
    }
    let mut query = world.query::<(bevy_ecs::entity::Entity, &SceneId)>();
    let entities: HashMap<String, _> = query
        .iter(world)
        .map(|(entity, id)| (id.0.to_string(), entity))
        .collect();
    let mut patches = Vec::new();
    for op in ops {
        let (Some(id), Some(text)) = (&op.entity, &op.value) else {
            return false;
        };
        let Some(&entity) = entities.get(id) else {
            return false;
        };
        let patch = match op.field.as_str() {
            "transform" => serde_json::from_str::<SceneTransform>(text)
                .map(|t| Patch::Transform(t.into())),
            "name" => serde_json::from_str(text).map(Patch::Name),
            "visible" => serde_json::from_str(text).map(Patch::Visible),
            _ => return false,
        };
        let Ok(patch) = patch else { return false };
        patches.push((entity, patch));
    }
    for (entity, patch) in patches {
        let mut entity = world.entity_mut(entity);
        match patch {
            Patch::Transform(transform) => entity.insert(transform),
            Patch::Name(name) => entity.insert(Name(name)),
            Patch::Visible(visible) => entity.insert(Visibility { visible }),
        };
    }
    true
}

/// Applies another peer's batch to the synced state and the open scene. The
/// open scene skips fields this editor changed and has not sent, and fields
/// in `inflight`, which this guest's later batches overwrite. Other edits
/// reload the whole scene.
fn apply_remote(
    world: &mut World,
    state: &mut EditorState,
    s: &mut Session,
    ops: &[FieldOp],
    inflight: &HashSet<FieldKey>,
) -> Result<(), String> {
    let Some((scene, synced)) = s.synced.clone() else {
        return Ok(());
    };
    let live = live_shared(world, state, s, &scene)?;
    let mut skip = ops::pending_keys(&synced, &live);
    skip.extend(inflight.iter().cloned());
    let mut next_synced = synced;
    ops::apply_ops(&mut next_synced, ops, inflight);
    let mut next_live = live.clone();
    ops::apply_ops(&mut next_live, ops, &skip);
    if next_live != live {
        let changed = ops::diff_scene(&live, &next_live);
        if state.mode != EditorMode::Edit || !patch_in_place(world, &changed) {
            apply_live(world, state, s, &scene, next_live)?;
        }
    }
    s.synced = Some((scene, next_synced));
    s.rebase.extend_from_slice(ops);
    if s.is_host() {
        s.unsaved_since.get_or_insert_with(Instant::now);
    }
    Ok(())
}

/// Diffs the open scene against the synced state and sends the changes.
fn send_local_changes(world: &mut World, state: &EditorState, s: &mut Session) {
    if state.mode != EditorMode::Edit
        || s.awaiting.is_some()
        || s.last_scene_poll.elapsed() < SCENE_POLL
    {
        return;
    }
    s.last_scene_poll = Instant::now();
    let Some((scene, synced)) = s.synced.clone() else {
        return;
    };
    let Ok(live) = live_shared(world, state, s, &scene) else {
        return;
    };
    let ops = ops::diff_scene(&synced, &live);
    if ops.is_empty() {
        return;
    }
    if !s.is_host() {
        s.inflight.push_back(ops.iter().map(FieldOp::key).collect());
    } else {
        s.unsaved_since.get_or_insert_with(Instant::now);
    }
    s.synced = Some((scene.clone(), live));
    let batch = Message::Batch {
        origin: s.net.id(),
        scene,
        ops,
    };
    if let Err(error) = s.share(&batch) {
        log(
            world,
            ConsoleLevel::Error,
            format!("Could not send: {error}"),
        );
    }
}

/// Writes the host's accepted state of the open scene to disk.
fn flush(world: &mut World, s: &mut Session) {
    if s.unsaved_since.take().is_none() {
        return;
    }
    let Some((scene, doc)) = &s.synced else {
        return;
    };
    let result = collab::write_shared_scene(&s.root, scene, doc)
        .and_then(|()| collab::scene_path(&s.root, scene));
    match result {
        // Recorded so the editor does not treat the write as an outside change.
        Ok(path) => super::SceneFileRevision::record(world, &path),
        Err(error) => log(
            world,
            ConsoleLevel::Error,
            format!("Could not save {scene}: {error}"),
        ),
    }
}

fn flush_rebase_if_long(s: &mut Session, history: &mut EditorHistory) {
    if s.rebase.len() > REBASE_LIMIT {
        rebase_history(s, history);
    }
}

/// Applies the remote operations received so far to every Undo and Redo
/// snapshot, so Undo reverts only this user's edits.
fn rebase_history(s: &mut Session, history: &mut EditorHistory) {
    let ops = std::mem::take(&mut s.rebase);
    let Some((scene, _)) = &s.synced else { return };
    if ops.is_empty() {
        return;
    }
    let folder = s.scene_folder(scene);
    let rebase = |document: &mut SceneDocument| {
        let Ok(mut doc) = collab::to_shared(document.clone(), &folder) else {
            return;
        };
        ops::apply_ops(&mut doc, &ops, &HashSet::new());
        if let Ok(next) = collab::from_shared(doc, &folder) {
            *document = next;
        }
    };
    let mut layer = Some(history);
    while let Some(history) = layer {
        history.undo.iter_mut().for_each(&rebase);
        history.redo.iter_mut().for_each(&rebase);
        history.pending_inspector.iter_mut().for_each(&rebase);
        layer = history.preview_before.as_deref_mut();
    }
}

/// Called before Undo or Redo reads a snapshot.
pub(super) fn before_undo(world: &mut World, history: &mut EditorHistory) {
    if let Some(mut collab) = world.get_resource_mut::<CollabState>() {
        if let Some(session) = &mut collab.session {
            rebase_history(session, history);
        }
    }
}

/// Writes a file a peer sent and reports whether it was written.
fn receive_file(
    world: &mut World,
    state: &mut EditorState,
    s: &mut Session,
    path: &str,
    bytes: &[u8],
) -> bool {
    // Scenes change through operations; a whole scene only creates one.
    if files::is_scene(path) && s.root.join(path).exists() {
        return false;
    }
    if let Err(error) = collab::write_received(&s.root, path, bytes, false) {
        log(world, ConsoleLevel::Warning, error);
        return false;
    }
    s.watcher.note(&s.root, path);
    s.foreign_code = true;
    // The Code area shows the new text unless it holds unsaved edits.
    if s.project.is_some() && state.code_path == path && !state.code_dirty {
        state.code_source = String::from_utf8_lossy(bytes).into_owned();
    }
    true
}

/// Deletes a file a peer deleted and reports whether it was deleted. The
/// open scene stays, and its current state is sent back so the peers that
/// deleted it get it again.
fn receive_delete(world: &mut World, s: &mut Session, path: &str) -> bool {
    if let Some((scene, doc)) =
        s.synced.as_ref().filter(|(open, _)| open == path)
    {
        log(
            world,
            ConsoleLevel::Warning,
            format!("{path} was deleted by someone else; it stays open here"),
        );
        let saved = collab::write_shared_scene(&s.root, scene, doc)
            .and_then(|()| collab::scene_path(&s.root, scene));
        if let Ok(full) = saved {
            super::SceneFileRevision::record(world, &full);
            s.watcher.note(&s.root, path);
        }
        if let Some(put) = file_put(world, s, path) {
            let _ = s.share(&put);
        }
        return false;
    }
    if let Err(error) = collab::delete_received(&s.root, path) {
        log(world, ConsoleLevel::Warning, error);
        return false;
    }
    s.watcher.note(&s.root, path);
    true
}

/// A `FilePut` of the file at `path`, or `None` when it is missing or too
/// large to send while editing.
fn file_put(world: &mut World, s: &Session, path: &str) -> Option<Message> {
    let full = s.root.join(path);
    if std::fs::metadata(&full).ok()?.len() > MAX_FILE_PUT {
        log(
            world,
            ConsoleLevel::Warning,
            format!("{path} is too large to sync while editing"),
        );
        return None;
    }
    let bytes = std::fs::read(&full).ok()?;
    Some(Message::FilePut {
        path: path.to_owned(),
        bytes,
    })
}

/// Sends project files that changed on this machine. Scenes are sent only
/// when new; their edits travel as operations.
fn send_changed_files(world: &mut World, s: &mut Session) {
    let changes = s.watcher.changed(&s.root);
    let written = changes.added.into_iter().chain(
        changes
            .modified
            .into_iter()
            .filter(|path| !files::is_scene(path)),
    );
    let mut messages: Vec<Message> = written
        .filter_map(|path| file_put(world, s, &path))
        .collect();
    messages.extend(
        changes
            .removed
            .into_iter()
            .map(|path| Message::FileDelete { path }),
    );
    for message in messages {
        if let Err(error) = s.share(&message) {
            log(
                world,
                ConsoleLevel::Error,
                format!("Could not send: {error}"),
            );
        }
    }
}

fn start_host(
    collab: &CollabState,
    state: &EditorState,
) -> Result<Session, String> {
    if state.project_root.is_empty() {
        return Err("Open a project to host it".into());
    }
    let root = Path::new(&state.project_root)
        .canonicalize()
        .map_err(|e| e.to_string())?;
    let net = match collab.link {
        Link::Direct => {
            let port = collab
                .port
                .trim()
                .parse()
                .map_err(|_| format!("{:?} is not a port", collab.port))?;
            NetSession::host(port, "")
        }
        Link::Relay => {
            NetSession::host_room(collab.relay.trim(), &collab.token)
        }
    }
    .map_err(|e| e.to_string())?;
    let net = SecureNet::new(net, &collab.key).map_err(|e| e.to_string())?;
    let mut session = Session::new(net, root, collab.name.trim());
    session.project = Some(state.project_root.clone());
    session.watcher = FileWatcher::new(&session.root);
    Ok(session)
}

fn start_join(
    collab: &CollabState,
    state: &EditorState,
) -> Result<Session, String> {
    if state.scene_dirty || state.code_dirty {
        return Err("Save or discard your changes before joining".into());
    }
    let key = match collab.link {
        Link::Direct => collab.address.trim(),
        Link::Relay => collab.code.trim(),
    };
    let root = collab::mirror_root(key);
    // The mirror is a cache: an earlier session's files would linger.
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).map_err(|e| e.to_string())?;
    let root = root.canonicalize().map_err(|e| e.to_string())?;
    let net = match collab.link {
        Link::Direct => NetSession::join(collab.address.trim(), ""),
        Link::Relay => {
            NetSession::join_room(collab.relay.trim(), key, &collab.token)
        }
    }
    .map_err(|e| e.to_string())?;
    let net = SecureNet::new(net, &collab.key).map_err(|e| e.to_string())?;
    collab::send(
        &net,
        HOST,
        &Message::Hello {
            name: collab.name.trim().to_owned(),
        },
    )
    .map_err(|e| e.to_string())?;
    Ok(Session::new(net, root, collab.name.trim()))
}

fn text_field(ui: &mut egui::Ui, text: &mut String, password: bool) {
    ui.add(
        egui::TextEdit::singleline(text)
            .password(password)
            .desired_width(170.0),
    );
}

/// Draws the Collaboration area: Host and Join forms, or the running
/// session.
pub(super) fn draw_collab_area(
    ui: &mut egui::Ui,
    collab: &mut CollabState,
    state: &EditorState,
) {
    egui::ScrollArea::vertical().show(ui, |ui| {
        ui.spacing_mut().item_spacing.y = 8.0;
        if collab.session.is_some() {
            draw_session(ui, collab);
        } else {
            draw_forms(ui, collab, state);
        }
        ui.label(
            egui::RichText::new(
                "Traffic is encrypted with the session key. Share the key \
                 only with people you trust.",
            )
            .small()
            .color(EditorTheme::WARNING),
        );
        if let Some(status) = &collab.status {
            kit::footer(ui, status);
        }
    });
}

fn draw_forms(
    ui: &mut egui::Ui,
    collab: &mut CollabState,
    state: &EditorState,
) {
    kit::card(
        ui,
        EditorIcon::Eye,
        "Collaboration",
        "Edit one project with others, live",
        |ui, rows| {
            rows.row(ui, "Your name", "Shown to the others", |ui| {
                text_field(ui, &mut collab.name, false);
            });
            rows.row(ui, "Connection", "By address, or a relay room", |ui| {
                kit::segmented(
                    ui,
                    "collab_link",
                    &mut collab.link,
                    &[(Link::Direct, "Direct"), (Link::Relay, "Relay")],
                );
            });
            if collab.link == Link::Relay {
                rows.row(ui, "Relay", "Relay server address", |ui| {
                    text_field(ui, &mut collab.relay, false);
                });
                rows.row(ui, "Relay token", "Empty if it has none", |ui| {
                    text_field(ui, &mut collab.token, true);
                });
            }
        },
    );
    let mut host = false;
    kit::card(
        ui,
        EditorIcon::Play,
        "Host",
        "Share the open project; your disk keeps it",
        |ui, rows| {
            if collab.link == Link::Direct {
                rows.row(ui, "Port", "Others join your-ip:port", |ui| {
                    text_field(ui, &mut collab.port, false);
                });
            }
            let ready = !state.project_root.is_empty();
            host =
                kit::action_button(ui, EditorIcon::Play, "Host", true, ready)
                    .clicked();
        },
    );
    let mut join = false;
    kit::card(
        ui,
        EditorIcon::Folder,
        "Join",
        "Opens a copy of the host's project",
        |ui, rows| {
            match collab.link {
                Link::Direct => {
                    rows.row(ui, "Address", "host-ip:port", |ui| {
                        text_field(ui, &mut collab.address, false);
                    });
                }
                Link::Relay => {
                    rows.row(ui, "Room code", "From the host", |ui| {
                        text_field(ui, &mut collab.code, false);
                    });
                }
            }
            rows.row(ui, "Session key", "From the host", |ui| {
                text_field(ui, &mut collab.key, true);
            });
            join =
                kit::action_button(ui, EditorIcon::Folder, "Join", true, true)
                    .clicked();
        },
    );
    let started = if host {
        collab.key = collab::secure::new_key();
        Some(start_host(collab, state))
    } else if join {
        Some(start_join(collab, state))
    } else {
        None
    };
    match started {
        Some(Ok(session)) => {
            collab.status = None;
            collab.session = Some(session);
        }
        Some(Err(error)) => collab.status = Some(error),
        None => {}
    }
}

fn draw_session(ui: &mut egui::Ui, collab: &mut CollabState) {
    let Some(s) = &collab.session else { return };
    let (title, how) = match (s.is_host(), s.net.room_code()) {
        (true, Some(code)) => ("Hosting", format!("Room code {code}")),
        (true, None) => ("Hosting", format!("Port {}", collab.port.trim())),
        (false, _) => ("Joined", format!("Copy in {}", s.root.display())),
    };
    let people = s.names.values().cloned().collect::<Vec<_>>().join(", ");
    let scene = match (&s.project, &s.synced, &s.awaiting) {
        (None, _, _) => "Receiving the project…".to_owned(),
        (_, Some((scene, _)), _) => scene.clone(),
        (_, _, Some(scene)) => format!("{scene} (waiting for the host)"),
        _ => "No scene open".to_owned(),
    };
    let mut leave = false;
    kit::card(ui, EditorIcon::Eye, title, &how, |ui, rows| {
        rows.row(ui, "People", "Known to this editor", |ui| {
            ui.label(&people);
        });
        rows.row(ui, "Scene", "Edits sync live", |ui| {
            ui.label(&scene);
        });
        if s.is_host() {
            rows.row(
                ui,
                "Session key",
                "Give it to the people you invite",
                |ui| {
                    ui.label(egui::RichText::new(&collab.key).monospace());
                    if kit::action_button(
                        ui,
                        EditorIcon::File,
                        "Copy",
                        false,
                        true,
                    )
                    .clicked()
                    {
                        ui.ctx().copy_text(collab.key.clone());
                    }
                },
            );
        }
        leave = kit::action_button(ui, EditorIcon::Close, "Leave", false, true)
            .clicked();
    });
    collab.leave |= leave;
}

#[cfg(test)]
mod tests;
