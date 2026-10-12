# Live Collaboration Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Two or more editors share one project live: the owner hosts, guests join by address or room code, and scene and file edits reach everyone.

**Architecture:** A host-ordered operation stream over the existing `NetSession`. Each peer polls its open scene at 10 Hz, diffs it against the last synced state into per-field operations, and sends them to the host, which validates, orders, applies, and broadcasts them to every peer including the sender. Non-scene files sync as whole files by modification-time polling. The core lives in `src/collab.rs` and `src/collab/` with no egui; the editor glue and panel live in `src/editor/collab_panel.rs`.

**Tech Stack:** Rust, bevy_ecs, egui, `serde_json`, `bincode` 1.3, `uuid`, `crate::net::NetSession`. No new dependency.

**Spec:** `docs/superpowers/specs/2026-10-12-live-collaboration-design.md`

## Global Constraints

- No dependency beyond those already in `Cargo.toml`.
- Every received path is checked: relative, no `..`, not absolute, inside the project or mirror root.
- The host validates every scene batch (`validate_scene_structure`) before applying it; a failing batch is rejected whole.
- Scene edits stay behind the snapshot undo system.
- Panels use `src/editor/gui_elements/` widgets and `EditorTheme` colors.
- Full check: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings` (also `--no-default-features`, `--features gpu-tests`), `cargo test --workspace`.

## Spec refinements made while reading the code

These keep the approved behaviour with less code:

- Change detection polls `scene_document` at 10 Hz in Edit mode instead of hooking `push_undo`; every edit path, drags included, is caught.
- Own-edit undo: when a remote batch arrives, it is also applied to every undo and redo snapshot. Undo then restores only what the local user changed. No per-step field keys.
- Save: the host writes accepted edits to its disk (open scene debounced 1 s through `save_editor_scene`; other scenes at once). A guest's Save only writes its mirror.
- A guest that opens a scene asks the host for its current state and ignores that scene's operations until the answer arrives.
- Wire format: one bincode `Message` enum; scene values travel as JSON text inside it, because bincode cannot hold `serde_json::Value`.
- Scene asset paths are absolute in memory. Sync documents are relativized to the scene folder before diffing and absolutized after applying.

## File structure

- Create `src/collab.rs`: `Message`, `Role`, `encode`/`decode`, `CollabSession` (network + ordering, no world access).
- Create `src/collab/ops.rs`: `FieldOp`, `FieldKey`, `diff_scene`, `apply_ops`, `pending_keys`.
- Create `src/collab/files.rs`: `safe_relative`, `scan_project`, `FileWatcher`.
- Create `src/editor/collab_panel.rs`: `CollabState` resource, `draw_collab_area`, `collab_tick`.
- Modify `src/lib.rs`: `pub mod collab;`.
- Modify `src/runtime/scene_file.rs`: `absolutize_scene_assets` becomes `pub`.
- Modify `src/editor/mod.rs`, `src/editor/dock.rs`, `src/editor/view.rs`, `src/editor/hierarchy.rs`: panel, tick, presence, trust prompt.
- Docs: `docs/collaboration.md`, `docs/editor-overhaul.md` backlog line, `roadmap.md` items.

---

### Task 1: Scene field diff and apply (`src/collab/ops.rs`)

**Interfaces:**
- Produces:
  - `pub struct FieldOp { pub entity: Option<String>, pub field: String, pub value: Option<String> }` (value is JSON text; `None` removes)
  - `pub type FieldKey = (Option<String>, String);` and `FieldOp::key(&self) -> FieldKey`
  - `pub fn diff_scene(before: &Value, after: &Value) -> Vec<FieldOp>`
  - `pub fn apply_ops(doc: &mut Value, ops: &[FieldOp], skip: &HashSet<FieldKey>)`
  - `pub fn pending_keys(synced: &Value, live: &Value) -> HashSet<FieldKey>`

Field split: scene-level keys except `entities`; per entity `"entity"` (create with whole value, delete with `None`), each top-level entity key except `id`/`components`, and `components/<name>`; scene-level `"order"` holding the ID list when order differs after the other operations.

- [ ] Tests first: round trip for field change, component add/remove, create, delete, reorder, scene-level change; `apply_ops` skips keys in `skip`; operations on a missing entity are ignored.
- [ ] Implement; `cargo test --lib collab::ops`.

### Task 2: Paths and file polling (`src/collab/files.rs`)

**Interfaces:**
- `pub fn safe_relative(path: &str) -> Option<PathBuf>`: `None` for empty, absolute, `..`, or a Windows prefix.
- `pub fn scan_project(root: &Path) -> BTreeMap<String, (SystemTime, u64)>` skipping `target`, `build`, `.git`, and `*.editor.json`.
- `pub struct FileWatcher { known: BTreeMap<..> }` with `new(root)`, `changed(&mut self, root) -> Vec<String>`, `note(&mut self, root, path)` (records a file the session itself wrote so it is not sent back).
- `pub fn is_scene(path: &str) -> bool` for `.rscene` paths.

- [ ] Tests: `safe_relative` rejects `../x`, `/etc/x`, `a/../../x`, accepts `assets/a.png`; watcher reports a new and a modified file once, ignores `target/` and noted writes.
- [ ] Implement.

### Task 3: Protocol and session (`src/collab.rs`)

**Interfaces:**
- `pub enum Message { Hello{name}, Welcome{peer, names}, FileData{path, bytes, append}, FilesDone{main_scene}, SceneRequest{scene}, SceneState{scene, doc}, Batch{scene, ops}, Rejected{scene, reason}, FilePut{path, bytes}, Presence{peer, name, scene, selection}, Left{peer} }`
- `pub fn encode(&Message) -> Vec<u8>`, `pub fn decode(&[u8]) -> Option<Message>`
- `pub struct CollabSession { pub net: NetSession, pub peer_names: BTreeMap<PeerId,String>, .. }` with `host(port,password)`, `host_room(relay, token)`, `join(addr,password,name)`, `join_room(relay, code, token, name)`, `is_host()`, `send_host(&Message)`, `send_to(peer,&Message)`, `broadcast(&Message)`.
- `pub fn stream_project(net: &NetSession, peer, root, main_scene)`: sends `FileData` chunks (4 MiB) then `FilesDone`.

- [ ] Tests: encode/decode round trip; `stream_project` over `NetSession::loopback(1)` delivers every file byte-exact, splits a 9 MiB file, skips `target/`.
- [ ] Implement.

### Task 4: Editor glue, live scene sync, and panel

**Interfaces:**
- `pub struct CollabState` (Resource): `session: Option<CollabSession>`, `name`, form fields, `synced: Option<(String, Value)>` (scene path + last synced doc), `awaiting: Option<String>`, `last_poll: Instant`, `host_dirty_since: Option<Instant>`, `mirror_root`, `open_request: Option<PathBuf>`, `watcher`, `presence: BTreeMap<PeerId, Presence>`, `trusted: bool`, `status`.
- `pub(super) fn collab_tick(world, state: &mut EditorState, history: &mut EditorHistory)` called from `draw_editor_view` right after `reload_external_scene_change`.
- `pub(super) fn draw_collab_area(ui, collab: &mut CollabState, state: &EditorState)`.

Tick order: poll events (host: Connected → flush open scene, `stream_project`; Batch → validate on copy, apply to file or live, broadcast; SceneRequest → answer; FilePut → write, broadcast); guest: FileData/FilesDone → write mirror, request open; SceneState → load; Batch → apply to synced, live (skip pending keys), and every history snapshot; then the 10 Hz local diff and send; then host autosave; then file watcher once a second.

- [ ] Editor test: two `App`s with `EditorPlugin`, `NetSession::loopback(1)`, both with the same scene; move an object on the guest, tick both, host world has the new position; rename on host, guest sees it; same field edited on both converges.
- [ ] Editor test: guest undo after a remote edit keeps the remote edit.
- [ ] Implement, add `EditorPanel::Collaboration`, wire into `view.rs`.

### Task 5: Files, presence, trust prompt

- [ ] Test: a guest file change reaches the host and the other guest; a path with `..` is rejected.
- [ ] Presence: `Presence` message on selection change; Hierarchy draws a colored dot with the peer's name tooltip.
- [ ] Trust prompt: a guest pressing Play or Build gets a confirmation; declining blocks the run.
- [ ] Docs page `docs/collaboration.md`, roadmap entries, full check.
