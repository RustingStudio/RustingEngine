//! Two editors in one process, joined over a loopback session.

use super::*;
use crate::runtime::{App, Name, SceneId};
use crate::Transform;

fn editor_app() -> App {
    let mut app = App::new();
    app.add_plugin(crate::AssetPlugin).unwrap();
    app.add_plugin(crate::runtime::RenderExtractPlugin).unwrap();
    app.add_plugin(super::super::EditorPlugin).unwrap();
    app
}

/// Opens the project at `root` the way the Project Manager does.
fn open(world: &mut World, root: &Path) {
    let project = super::super::open_project(root).unwrap();
    let mut state = world.resource_mut::<EditorState>();
    super::super::set_open_project_paths(&mut state, &project);
    let mut state = world.remove_resource::<EditorState>().unwrap();
    super::super::load_editor_scene(world, &mut state, &project.scene_path)
        .unwrap();
    world.insert_resource(state);
}

/// One editor frame of collaboration, with the poll timers already due.
fn tick(world: &mut World) -> Option<PathBuf> {
    if let Some(s) = world.resource_mut::<CollabState>().session.as_mut() {
        let due = Instant::now().checked_sub(FILE_POLL).unwrap();
        s.last_scene_poll = due;
        s.last_file_poll = due;
        s.last_presence = due;
    }
    let mut state = world.remove_resource::<EditorState>().unwrap();
    let mut history = world.remove_resource::<EditorHistory>().unwrap();
    let open = collab_tick(world, &mut state, &mut history);
    world.insert_resource(state);
    world.insert_resource(history);
    open
}

/// Waits until the host has sent the project to every new guest.
fn wait_for_streams(world: &mut World) {
    let collab = world.resource::<CollabState>();
    let s = collab.session.as_ref().unwrap();
    while !s.streams.iter().all(JoinHandle::is_finished) {
        std::thread::yield_now();
    }
}

fn session_mut(world: &mut World) -> &mut Session {
    world
        .resource_mut::<CollabState>()
        .into_inner()
        .session
        .as_mut()
        .unwrap()
}

fn find(world: &mut World, name: &str) -> bevy_ecs::entity::Entity {
    let mut query = world.query::<(bevy_ecs::entity::Entity, &Name)>();
    query
        .iter(world)
        .find(|(_, n)| n.0 == name)
        .map(|(entity, _)| entity)
        .unwrap_or_else(|| panic!("no {name}"))
}

fn x(world: &mut World, name: &str) -> f32 {
    let entity = find(world, name);
    world.get::<Transform>(entity).unwrap().position[0]
}

fn set_x(world: &mut World, name: &str, value: f32) {
    let entity = find(world, name);
    world.get_mut::<Transform>(entity).unwrap().position[0] = value;
}

fn rename(world: &mut World, from: &str, to: &str) {
    let entity = find(world, from);
    world.get_mut::<Name>(entity).unwrap().0 = to.into();
}

/// A host with a project holding "Box", and a guest that joined it and
/// opened the host's copy.
fn joined(name: &str) -> (App, App, PathBuf) {
    let folder = std::env::temp_dir()
        .join(format!("rusting-collab-{name}-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&folder).unwrap();
    let project = super::super::create_project(&folder, "Shared").unwrap();
    let mut host = editor_app();
    open(host.world_mut(), &project.root);
    host.world_mut().spawn((
        SceneId::default(),
        Name("Box".into()),
        Transform::default(),
    ));
    let world = host.world_mut();
    let state = world.remove_resource::<EditorState>().unwrap();
    super::super::save_editor_scene(world, &state, &project.scene_path)
        .unwrap();
    world.insert_resource(state);

    let (host_net, mut guests) = NetSession::loopback(1);
    let key = collab::secure::new_key();
    let host_net = SecureNet::new(host_net, &key).unwrap();
    let guest_net = SecureNet::new(guests.remove(0), &key).unwrap();
    let root = project.root.canonicalize().unwrap();
    let mut session = Session::new(host_net, root.clone(), "host");
    session.project = Some(project.root.display().to_string());
    session.watcher = FileWatcher::new(&root);
    host.world_mut().resource_mut::<CollabState>().session = Some(session);

    let mirror = folder.join("mirror");
    std::fs::create_dir_all(&mirror).unwrap();
    let mirror = mirror.canonicalize().unwrap();
    mark_untrusted(&mirror).unwrap(); // as `start_join` does
    collab::send(
        &guest_net,
        HOST,
        &Message::Hello {
            name: "guest".into(),
        },
    )
    .unwrap();
    let mut guest = editor_app();
    guest.world_mut().resource_mut::<CollabState>().session =
        Some(Session::new(guest_net, mirror.clone(), "guest"));

    tick(host.world_mut()); // answers the handshake
    tick(guest.world_mut()); // sends Hello
    tick(host.world_mut()); // starts sending the project
    wait_for_streams(host.world_mut());
    let opened = tick(guest.world_mut()).expect("project received");
    assert_eq!(opened, mirror);
    open(guest.world_mut(), &mirror);
    tick(guest.world_mut()); // asks for the scene
    tick(host.world_mut()); // answers
    tick(guest.world_mut()); // loads the answer
    let collab = guest.world().resource::<CollabState>();
    let session = collab.session.as_ref().unwrap();
    assert!(session.awaiting.is_none());
    assert_eq!(session.synced.as_ref().unwrap().0, "scenes/main.rscene");
    (host, guest, folder)
}

#[test]
fn edits_reach_the_other_editor_and_same_field_converges() {
    let (mut host, mut guest, folder) = joined("edits");
    let (h, g) = (host.world_mut(), guest.world_mut());

    set_x(g, "Box", 3.0);
    tick(g);
    tick(h);
    assert_eq!(x(h, "Box"), 3.0, "guest move reaches the host");
    tick(g);

    rename(h, "Box", "Crate");
    tick(h);
    tick(g);
    assert_eq!(x(g, "Crate"), 3.0, "host rename reaches the guest");

    // Both move it in the same frame: the host's later write wins.
    set_x(g, "Crate", 1.0);
    set_x(h, "Crate", 2.0);
    tick(g);
    tick(h);
    tick(g);
    assert_eq!(x(h, "Crate"), 2.0);
    assert_eq!(x(g, "Crate"), 2.0);
    assert!(guest
        .world()
        .resource::<CollabState>()
        .session
        .as_ref()
        .unwrap()
        .inflight
        .is_empty());

    // The host autosaves accepted edits.
    let h = host.world_mut();
    h.resource_mut::<CollabState>()
        .session
        .as_mut()
        .unwrap()
        .unsaved_since = Instant::now().checked_sub(AUTOSAVE_DELAY);
    tick(h);
    let saved =
        std::fs::read_to_string(folder.join("Shared/scenes/main.rscene"))
            .unwrap();
    assert!(saved.contains("Crate"), "{saved}");
    std::fs::remove_dir_all(folder).unwrap();
}

#[test]
fn undo_reverts_own_edit_and_keeps_remote_edit() {
    let (mut host, mut guest, folder) = joined("undo");
    let (h, g) = (host.world_mut(), guest.world_mut());

    let before = scene_document(g, "Main Scene").unwrap();
    g.resource_mut::<EditorHistory>().push_undo(before);
    rename(g, "Box", "Mine");
    tick(g);
    tick(h);
    tick(g);
    set_x(h, "Mine", 5.0);
    tick(h);
    tick(g);
    assert_eq!(x(g, "Mine"), 5.0);

    // Undo as the Undo button does it.
    let mut history = g.remove_resource::<EditorHistory>().unwrap();
    before_undo(g, &mut history);
    let previous = history.undo.pop_back().unwrap();
    crate::runtime::load_scene_document(g, &previous, SceneLoadMode::Replace)
        .unwrap();
    g.insert_resource(history);
    assert_eq!(x(g, "Box"), 5.0, "the host's move survives the undo");

    tick(g);
    tick(h);
    assert_eq!(x(h, "Box"), 5.0, "the undo reaches the host");
    std::fs::remove_dir_all(folder).unwrap();
}

#[test]
fn files_sync_both_ways_and_unsafe_paths_are_refused() {
    let (mut host, mut guest, folder) = joined("files");
    let (h, g) = (host.world_mut(), guest.world_mut());
    let mirror = g
        .resource::<CollabState>()
        .session
        .as_ref()
        .unwrap()
        .root
        .clone();
    let root = folder.join("Shared");

    std::fs::write(mirror.join("notes.txt"), "from guest").unwrap();
    tick(g);
    tick(h);
    assert_eq!(
        std::fs::read_to_string(root.join("notes.txt")).unwrap(),
        "from guest"
    );
    tick(g);
    std::fs::write(root.join("src/extra.rs"), "// host").unwrap();
    tick(h);
    tick(g);
    assert_eq!(
        std::fs::read_to_string(mirror.join("src/extra.rs")).unwrap(),
        "// host"
    );

    let mut state = g.remove_resource::<EditorState>().unwrap();
    let mut collab = g.remove_resource::<CollabState>().unwrap();
    let s = collab.session.as_mut().unwrap();
    assert!(!receive_file(g, &mut state, s, "../escape.txt", b"x"));
    assert!(!folder.join("escape.txt").exists());
    std::fs::remove_dir_all(folder).unwrap();
}

#[test]
fn selection_shows_on_the_other_editor() {
    let (mut host, mut guest, folder) = joined("presence");
    let (h, g) = (host.world_mut(), guest.world_mut());
    let entity = find(g, "Box");
    let id = g.get::<SceneId>(entity).unwrap().0;
    g.resource_mut::<EditorState>().selection = vec![entity];
    tick(g);
    tick(h);
    let marks = remote_selection(h);
    assert_eq!(marks[&id][0].0, "guest", "{marks:?}");

    g.resource_mut::<EditorState>().selection.clear();
    tick(g);
    tick(h);
    assert!(remote_selection(h).is_empty());
    std::fs::remove_dir_all(folder).unwrap();
}

#[test]
fn guest_build_waits_for_trust_once() {
    let (mut host, mut guest, folder) = joined("trust");
    let g = guest.world_mut();
    let mirror = session_mut(g).root.to_string_lossy().into_owned();
    assert!(gate_build(g, &mirror, BuildRequest::Check).is_none());
    assert!(g.resource::<CollabState>().pending_build.is_some());
    // Leaving does not trust the mirror's code.
    g.resource_mut::<CollabState>().leave = true;
    tick(g);
    assert!(g.resource::<CollabState>().session.is_none());
    assert!(gate_build(g, &mirror, BuildRequest::Check).is_none());
    // Trusting it once lets later builds run.
    let (root, _) = g
        .resource_mut::<CollabState>()
        .pending_build
        .take()
        .unwrap();
    std::fs::remove_file(root.join(UNTRUSTED)).unwrap();
    assert!(gate_build(g, &mirror, BuildRequest::Check).is_some());
    // The host runs its own code without asking.
    let root = session_mut(host.world_mut())
        .root
        .to_string_lossy()
        .into_owned();
    assert!(gate_build(host.world_mut(), &root, BuildRequest::Check).is_some());
    std::fs::remove_dir_all(folder).unwrap();
}

#[test]
fn deletes_and_new_scenes_reach_the_other_editor() {
    let (mut host, mut guest, folder) = joined("delete");
    let (h, g) = (host.world_mut(), guest.world_mut());
    let mirror = session_mut(g).root.clone();
    let root = folder.join("Shared");

    std::fs::write(mirror.join("notes.txt"), "x").unwrap();
    tick(g);
    tick(h);
    tick(g);
    std::fs::remove_file(mirror.join("notes.txt")).unwrap();
    tick(g);
    tick(h);
    assert!(
        !root.join("notes.txt").exists(),
        "guest delete reaches the host"
    );

    // A scene someone has open is never deleted under them, and comes back
    // to the peer that deleted it.
    std::fs::remove_file(root.join("scenes/main.rscene")).unwrap();
    tick(h);
    tick(g);
    assert!(mirror.join("scenes/main.rscene").exists());
    tick(h);
    assert!(
        root.join("scenes/main.rscene").exists(),
        "restored on the host"
    );

    // A scene the guest makes reaches the host whole.
    std::fs::write(mirror.join("scenes/new.rscene"), "guest scene").unwrap();
    tick(g);
    tick(h);
    assert_eq!(
        std::fs::read_to_string(root.join("scenes/new.rscene")).unwrap(),
        "guest scene"
    );
    // Later whole-scene writes are refused; edits travel as operations.
    let mut state = h.remove_resource::<EditorState>().unwrap();
    let mut collab = h.remove_resource::<CollabState>().unwrap();
    let s = collab.session.as_mut().unwrap();
    assert!(!receive_file(h, &mut state, s, "scenes/new.rscene", b"x"));
    std::fs::remove_dir_all(folder).unwrap();
}

#[test]
fn moves_patch_the_live_entity_in_place() {
    let (mut host, mut guest, folder) = joined("patch");
    let (h, g) = (host.world_mut(), guest.world_mut());
    let before = find(g, "Box");
    set_x(h, "Box", 4.0);
    rename(h, "Box", "Moved");
    tick(h);
    tick(g);
    assert_eq!(x(g, "Moved"), 4.0);
    assert_eq!(find(g, "Moved"), before, "no reload");
    std::fs::remove_dir_all(folder).unwrap();
}

#[test]
fn editor_camera_shows_on_the_other_editor() {
    let (mut host, mut guest, folder) = joined("camera");
    let (h, g) = (host.world_mut(), guest.world_mut());
    let camera = g.spawn(crate::runtime::GlobalTransform::default()).id();
    g.resource_mut::<EditorState>().editor_camera = Some(camera);
    tick(g);
    tick(h);
    assert_eq!(remote_cameras(h).len(), 1);
    std::fs::remove_dir_all(folder).unwrap();
}
