# Live collaboration: design

Date: 2026-10-12. Status: approved design, not implemented.

## Goal

Two or more people edit one project at the same time, each in their own
editor. The project owner hosts a session; others join it and see every
edit within a frame or two. The whole project is shared: all scenes,
assets, code, and settings.

Asynchronous collaboration (git plus the `rusting merge` driver in
`src/scene_merge.rs`) already exists and is out of scope.

## Decisions

| Question | Decision |
|---|---|
| Mode | Live co-editing. |
| Scope | Whole project: scenes, assets, code, `project.json`, settings. |
| Topology | The owner's editor is the host (listen server). Guests join by address (`NetSession::host` / `NetSession::join`) or by relay room code (`NetSession::host_room` / `NetSession::join_room`). No new server. |
| Conflicts | The host applies edits in arrival order. The last write to a field wins. |
| Join | The host streams the project to the guest. The guest needs nothing beforehand. |
| Undo | Undo reverts only the user's own edits. |
| Code | Code files sync like any file. A guest that runs or builds project code confirms once per session. |
| Edit transport | Diff undo snapshots into per-field operations (approach A). Explicit per-site commands (B) and whole-scene broadcast (C) were rejected: B touches every edit site and drifts; C loses concurrent edits. A CRDT library is not needed because the host orders everything. |

## Architecture

### Roles

- **Host.** The owner's editor. Its disk holds the real project. It keeps
  the authoritative in-memory state, assigns a sequence number (`seq`) to
  every accepted edit, and broadcasts it.
- **Guest.** Holds a mirror of the project in
  `~/.cache/rusting/collab/<session-id>/`. It sends edits to the host and
  applies what the host broadcasts. It never writes to the host's disk
  directly.

### New code

- `src/collab.rs`: the protocol messages, the scene field diff and apply
  functions, the host operation log, path validation, and the file
  poller. No GPU and no egui, so everything is unit-testable.
- `src/editor/collab_panel.rs`: the Host and Join controls, the peer list,
  the plain-text traffic warning, and the code trust prompt. Built from
  `src/editor/gui_elements/` and `EditorTheme`.
- Hooks in `src/editor/mod.rs`: the sync system, remote-operation apply,
  and the undo change.

### Scene operations

```rust
pub struct FieldOp {
    pub scene: String,          // project-relative scene path
    pub entity: Option<String>, // entity ID; None for a scene-level field
    pub field: String,          // "transform", "components/game.health", "entity", ...
    pub value: serde_json::Value, // Null deletes; field "entity" carries the whole entity on create
}
```

Fields are split the same way `scene_merge.rs` splits them: each top-level
entity field is one field, and each `components` entry is its own field.
Creating an entity is a `field: "entity"` operation with the whole entity
as its value. Deleting one is the same field with `Null`. Hierarchy order
changes are a scene-level `order` field holding the entity ID list.

### Edit flow

1. A local edit happens as it does today, and its undo snapshot is pushed.
2. A sync system runs each frame while a session is active. When the scene
   changed since `last_synced` (a flag set by `push_undo`, and at 10 Hz
   during gizmo or Inspector drags), it takes `scene_document`, diffs it
   against `last_synced`, and produces `Vec<FieldOp>`.
3. A guest sends the batch to the host. The host checks it (see Security),
   applies it, stamps it with the next `seq`, and broadcasts it to every
   peer, including the sender. Every peer therefore ends in host order.
4. A peer that receives a batch applies it to its document and reloads the
   scene with `load_scene_document`, keeping selection and the editor
   camera. It sets `last_synced` to the result so remote edits are never
   sent back.

`ponytail:` step 4 reloads the whole scene per remote batch. Apply per
entity if large scenes stutter.

### Scenes that are not open

The host keeps each scene that any peer touched as in-memory JSON.
Operations name their scene, so a peer can edit any scene. Save from any
peer asks the host to write that scene to its disk with `write_atomic`.
The dirty flag is shared.

### Joining

1. The guest connects, with the session password if one is set.
2. The host sends a manifest (path, size, hash) and then the file bytes in
   1 MB chunks, under `MAX_MESSAGE` (16 MB). It skips `target/`, `build/`,
   and `.git/`.
3. The host sends its in-memory unsaved scenes and the current `seq`.
4. The guest writes the mirror and opens it. Batches after that `seq`
   follow.

### Encoding

Control messages and operation batches are JSON, because operation values
are `serde_json::Value` and bincode cannot decode it. File chunks are raw
bytes after a one-byte tag. No new dependency.

### Own-edit undo

Today Undo restores a whole snapshot. In a session that would erase other
people's edits. Each undo entry therefore also stores the field keys its
step changed (known from the step 2 diff). In a session, Undo and Redo
send operations that restore only those fields from the snapshot. Remote
edits do not touch the local undo stack. Outside a session, Undo is
unchanged.

### Other files

Assets, `src/*.rs`, wasm modules, `project.json`, and settings sync as
whole files. Host and guests poll project file modification times once a
second (standard library, no `notify` dependency). A changed file is sent
as `FilePut { path, bytes, hash }` to the host, which writes it and
broadcasts it. Last write wins. A guest can edit code in its own IDE inside
the mirror folder. Scene files are excluded: operations own them.

### Presence

Each peer sends its name, color, and selection when they change. The
Hierarchy shows a colored dot and the name next to entities that others
selected, and the viewport outlines them in that color. Remote camera
markers are deferred to the `docs/editor-overhaul.md` backlog.

### Play mode and code trust

Play mode is local to each person and never synced. When a guest starts
Play or a build that compiles or runs project code (Rust plugins or wasm
scripts), the editor asks once per session: "Run code from <host>?".
Declining leaves the guest able to edit but not run.

## Security

These checks are at trust boundaries and are not simplified.

- Every received path is checked on both sides: relative, no `..`, not
  absolute, and it must not resolve outside the project or mirror root
  through a symlink.
- The host applies each batch to a copy and runs
  `validate_scene_structure`. A batch that fails is rejected whole, and the
  sender gets an error and resynchronises from the host.
- The session password keeps strangers out, but traffic is plain text. The
  panel says "Use on a LAN or a trusted relay only."
- Guest code execution needs the trust prompt above.

## Error handling

- A guest that disconnects mid-edit loses only unsent edits. The host's
  disk changes only on Save, through `write_atomic`.
- If the host leaves (`NetEvent::Closed`), guests keep their mirror open
  read-only with a "Host left" message.
- Batches travel over TCP, which is ordered and reliable, so `seq` gaps
  cannot happen. A rejected batch is the only resync trigger.

## Testing

Unit tier, no GPU, using `NetSession::loopback` where a network is needed:

- Diff then apply reproduces the edited document (create, delete, field
  change, component add and remove, reorder).
- Two peers edit different entities at once; both edits survive on every
  peer.
- Two peers edit the same field; every peer converges on host order.
- Own-edit undo restores the user's field and leaves a remote edit to
  another field alone.
- Path validation rejects `../x`, `/etc/x`, and a symlink escape.
- An invalid batch is rejected and the host state is unchanged.
- End to end: a host and two guests on loopback; a guest joins, receives
  the project, edits, and all three converge.

## Phases

Each phase is one roadmap item and ships on its own. Phase 2 already gives
usable live co-editing of one scene.

1. `collab.rs` field diff and apply, with tests.
2. Host and join, project streaming, live sync of the open scene,
   collaboration panel.
3. Multiple scenes and shared Save.
4. Own-edit undo.
5. File sync by modification-time polling.
6. Presence: names, colors, selection.
7. Code trust prompt.

## Out of scope

Remote camera markers, rejoin download cache by hash, encryption,
per-user permissions, chat, and offline editing merged back into a
session.
