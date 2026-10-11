# Editor overhaul

The owner asked for a large design and feature update of the egui editor,
aiming at a real-engine editor. The visual and interaction direction is
Blender + Godot. The work is split into waves; each wave ships working,
tested code before the next begins.

## Wave 1 (in progress): visual pass, inspector, hierarchy

### Visual design pass

- New `EditorTheme` palette: Godot-style dark neutral panels with one accent
  color, Blender-style compact spacing and area headers.
- Every dock area gets a header strip with an editor-type switcher (the
  `dock.rs` area tree stays the layout model).
- One toolbar style and one icon set across all panels.
- Theme and UI scale persist per user (roadmap Milestone 6, "DPI scaling,
  font configuration, and theme persistence").

### Inspector (property-row system)

- Module `src/editor/inspector/`:
  - `widgets.rs`: Godot-style property rows (label left, widget right) and
    shared widgets: vec3 with colored X/Y/Z fields, color, angle in degrees,
    enum, asset handle.
  - `json.rs`: custom (game-defined) components are stored as JSON. Their
    values render as typed widgets: numbers as drag values, bools as
    checkboxes, 3-number arrays as vec3, strings as text, nested objects as
    sub-sections. Raw JSON text editing is removed from the normal path.
  - `mod.rs`: the Inspector panel; built-in components are drawn as
    collapsible sections using the shared widgets.
- Every edit still flows through the snapshot undo system.

### Hierarchy

- Search/filter box.
- Per-type icons (camera, light, mesh, empty).
- Blender-style visibility eye toggle.
- Double-click inline rename.
- Drag and drop reparenting (roadmap Milestone 6).
- Right-click context menu: add child, duplicate, rename, delete.
- Multi-select (Ctrl toggles, Shift selects a range). The selection has a
  primary entity; the gizmo and Inspector act on the primary, delete and
  duplicate act on the whole selection.

### Wave 1 progress

Done (unit tests in `editor::hierarchy`, `editor::inspector::{widgets, json}`):

- Palette, compact density, flat list rows, area chrome.
- Hierarchy: search that keeps ancestors of matches, per-type icons, eye
  toggle (`EntityRequest::SetVisible`, undoable), double-click / F2 rename,
  Delete deletes the selection, dragging a selected row reparents the
  whole selection, multi-select with Ctrl and Shift and a
  primary object (`EditorState::selection` + `selected`), delete and
  duplicate act on the whole selection.
- Inspector module with property rows, axis-colored vec3, color, angle,
  choice, and Godot-style collapsible sections; custom JSON components are
  edited as typed fields live, and drags coalesce into one Undo step.

- UI scale and text size persist per user (`EditorPreferences`, View >
  UI Scale / Text Size). There is one palette, so no theme choice is stored
  yet. Widgets with a hard-coded `FontId` ignore the text size.
- Editor-type icon in each area header.
- Hiding a parent hides its children in rendering.
- Viewport shows bounds for every selected object (primary in yellow, others
  in orange); Shift-click in the viewport adds, activates, or deselects
  Blender style.
- Hierarchy rows hidden by themselves or an ancestor are dimmed.
- Scene View mouse navigation: middle drag orbits around a pivot in front of
  the camera, Shift + middle drag pans, the wheel dollies toward the pivot,
  and F (focus) moves the pivot onto the selected object. Right drag still
  flies.
- Project panel "Scene Rendering" section: Quality (Auto, Eco, Balanced,
  High) and Culling (Auto, Off, Frustum, Frustum + Occlusion). Both are
  saved in the scene file (format 6) and load with the scene in the
  exported game. Edits are undoable scene changes (unit test
  `scene_render_settings_edits_are_undoable_scene_changes`).
- Inspector "Material" section for the selected Mesh Renderer: model, alpha
  mode and cutoff, base color and opacity, metallic, roughness, emissive,
  and the five texture slots. Each slot picks a loaded texture or loads an
  image file (copied into the project `assets` folder). "New Material"
  gives the object its own default material. Editing a material that other
  renderers share copies it first. Edits are undoable (unit test
  `material_edits_copy_shared_materials_and_edit_owned_ones_in_place`).
- Native file dialogs run on a worker thread (`editor::file_dialogs`), so
  the window keeps answering the window manager and is not reported as "not
  responding". While a dialog is open, a small modal blocks the panels.
- Cameras and lights draw Blender-style wire shapes in the Scene View
  (camera frustum with an up triangle, sun with rays and a direction line,
  point light circles, spot cone) in the selection colors. Clicking within
  6 points of a shape selects the object (unit test
  `clicks_near_light_and_camera_shapes_select_them`).
- glTF models are added to the scene like Blender's importer: one new empty
  object named after the file holds the file's node tree (names, local
  transforms, meshes, materials, cameras, lights). "Add to Scene" in the
  Assets panel and "Import Files..." both do this, select the new root, and
  are undoable. The old "Imported glTF primitives / Use on Selected" list is
  gone. Primitives without normals get flat face normals, as glTF requires
  (unit test `added_models_keep_their_node_tree_under_one_saved_root`).
- Assets area is a Godot-style FileSystem tree (`editor::assets_panel`):
  folders first and foldable, type icons, colored type badges, a filter
  that opens folders on the way to matches, and hidden `.rmesh`/`.rtexture`
  caches. Double-click adds a model to the scene or puts an image on the
  selected object as its base color; right-click lists the actions; models
  drag into the Scene View. The "Loaded textures" text list is gone (unit
  test `rows_put_folders_first_hide_caches_and_fold`).
- Image rows show a thumbnail (larger on hover) and drag onto a Hierarchy
  object (base color) or an Inspector texture slot (unit tests
  `image_rows_decode_a_few_thumbnails_per_frame`,
  `dropping_an_image_on_hierarchy_rows_and_texture_slots_assigns_it`).

Open:

- Toolbar consistency pass.
- egui's built-in Ctrl +/- zoom is not persisted; only View > UI Scale is.
- Asset handle fields show the raw key; show the asset path and accept drops
  once the asset browser exists.

## Backlog (later waves)

Kept here so it is not lost between sessions. Roadmap milestones that own an
item are named in parentheses.

### Scene editing UX

- Numpad views: no auto-orthographic on a numpad view (Blender's
  auto-perspective), and the orthographic state is not saved with the editor layout. An orthographic
  view clips objects behind the camera; pull the camera back when zoomed in
  close. Home frames only objects with a mesh.
- Selection outline is a wire box of the bounds, not a pixel silhouette
  of the mesh (M6).
- Camera and light shapes: the camera frame is fixed at 16:9, and there
  are no per-type icons.
- Gizmo polish: visual restyle; snap increments are saved only with
  Save Layout (`editor_layout.json`, `snap` field), not automatically (M6).
- Multiple viewports and orthographic views (M20).
- Game components the editor has no registration for (for example a
  game's `coin_run.spin`) load as `UnregisteredComponents`, save back
  unchanged, and show in the Inspector as read-only JSON that can be
  removed. Editing them needs JSON validated on the next game load.
- Scene instances (prefabs): changes to placed objects save as overrides,
  but nothing shows which fields are overridden. The Inspector should mark
  overridden fields with a per-field revert arrow, as Godot does (only
  whole-object and whole-instance revert exist). Unpack is always complete;
  a one-level unpack that keeps nested instances linked is missing, and
  Undo does not restore a source file written by Apply to Source. The
  Hierarchy should mark instance roots and tint their children,
  editing `source` in the Inspector should re-read the scene at once, and
  scenes should also be draggable into the Scene View like models.
- Reflected Inspector gaps: a string-keyed map (such as `TileMap.tiles`)
  adds entries by key and removes them, but renaming a key is a remove and
  an add; the asset drop-down
  for handle fields lists only assets that are already loaded, with no file
  browser or drag from the Assets panel.

### Asset workflow

- Asset browser: a grid view, thumbnails decoded on a worker and refreshed
  when the file changes, and dropped models placed under the cursor instead
  of at the origin (M6).
- glTF import: vertex colors (`COLOR_0`), `KHR_texture_transform`, and
  import options (scale, up axis, "apply transforms") are not read.
  Re-adding a file imports its meshes again instead of sharing them.
- Typed asset assignment follow-up: standalone reusable collider shape
  assets do not exist yet. Mesh colliders use the renderer's typed mesh
  handle, while primitive collider shapes are typed enum variants.
- Per-asset import settings and re-import (M20). The library and CLI side
  exists (`crate::asset_import`, `rusting asset import|reimport|list`,
  `<file>.rmeta` sidecars with ID, settings, dependencies and provenance).
  The asset browser has a Replace… action with a dry-run preview and hides
  `.rmeta` sidecars. It still needs an import dialog with provenance fields
  (its Import button copies files without a sidecar), license/changed/
  not-imported badges, and a Generate dialog that runs a `project.json`
  generator hook with a prompt (`rusting asset generate` covers it today).
- Art-direction presets: `rusting preset apply` has no editor picker yet.
  Presets should skip ACES tone mapping and keep HUD size hierarchy on 2D
  unlit scenes; the starter exercise corrects both by patch today.
- Image textures always load as sRGB, both from the Inspector and from
  scene files. Normal, metal/rough, and occlusion maps need a linear load
  path, and the scene file needs to store the color space per slot.
- Material assets as files (`.rmaterial`) that several scenes share. Today
  every scene stores its materials inline.
- Data assets: Inspector edits to a `.rdata` file are file writes with no
  undo. The Handle field picker lists only data assets that are already
  loaded, so it cannot pick an unloaded `.rdata` file. Renamed or removed
  fields fail to load instead of migrating. A unique value cannot be
  saved out as a new `.rdata` file yet (Godot's Save As on a sub-resource),
  and every scene load or Inspector edit of a unique value adds a copy
  while the old one stays in memory. A `.rdata` file open in the Inspector
  does not re-read changes made outside the editor; the next edit
  overwrites them.
- LOD group authoring: edit a mesh's `.rlod` levels and hand-over values in
  the Inspector, preview the active level in the viewport, and hot reload
  `.rlod` files. Import glTF `MSFT_lod` into LOD groups.

### Panels and tools

- Console: engine code (renderer, physics, runtime) has no log sink, so
  only editor messages reach it. Status lines get their level from
  keywords; give `scene_message` a level instead.
- Profiler follow-up: add per-system CPU spans, capture/export, and dedicated
  GPU allocation accounting to the M6 dockable history panel (M20).
- Render settings and physics diagnostics follow-up: expose more optional
  device capabilities and explain capacity warnings inline. The M6 panels
  already show `RenderCapacityDiagnostics`, including missing asset counts.
- Shortcut follow-ups: each action has at most two keys; make modal keys
  (Escape cancels a gizmo drag) rebindable.
- Global scaling at arbitrary object angles is axis weighted because scene
  transforms store rotation and scale without shear; support exact global
  scaling if the scene transform format gains a shear component.
- Project settings: the Project panel is a card page built from
  `gui_elements::kit` (banner, category rail, setting rows, switches,
  segmented controls, stat tiles). Follow-ups: move the Inspector, Render
  Settings and Physics Diagnostics panels onto the same kit, add physics and
  input map categories (M20), and wire `render_scale` and `background_color`
  into the page.
- Project-wide search (M20).
- 2D path: the Inspector's Tile Painter paints, draws lines, fills
  rectangles and flood fills the selected tile map in the Scene View, and
  outlines the target cells. Flood fill stays inside the grid's used area.
  The brush palette shows each tile's color, and D, R, L and B pick the
  tools while a brush is active. Follow-up: texture thumbnails in the
  palette.

### Play workflow

- Native Stop/Restart and embedded Preview with Pause/Step are implemented
  in M6. Preview runs the editor's ECS systems and restores the authored
  scene, undo history, and transient entities on Stop.
- Reload Code: reload automatically when a project `.rs` file is saved.
  Keep resources across a reload (they need a reflection registration
  first). A reload restarts the game process, so the window and GPU
  resources are created again; the Windows path, where a running game's
  executable is locked, has not been tried.
- Remote scene tree and inspector for a running game (M20).
- Show `RuntimeUi` in the embedded Preview. Update systems already draw
  into the App's `RuntimeUi` pass while Preview runs, but the Game area does
  not paint that output or forward pointer and key input to it. Native Play
  shows it because the game window runner paints it.

### Rendering integration

- Smoke-test editor window resize and close-button shutdown by hand after
  the `EditorWindowRunner::draw` split; an 8-second launch passed (M1).
- Engine-owned egui painter is done (`rendering::egui_painter`). Texture
  uploads wait on their own submission; replace with a staging ring if the
  stall shows up in profiles.
- Golden-image test for editor compositing (M6 exit gate).
- Profiler area with per-pass GPU timestamps and CPU/GPU history is done.
  Add per-pass history selection and frame capture/export (M20).
- Anti-aliasing and shadow quality in Render Settings are session
  options. Save them in project settings (or `SceneRenderSettings`, which
  needs cooked scene format 9 with a legacy struct for format 8).
- Atmosphere (fog, bloom, ambient occlusion): the Scene view's viewport
  menu switches each effect off for editing, and a selected Fog draws its
  full-density and 1/e heights as squares over the grid. Dragging a square
  sets `height` or `height_falloff`, with one Undo step. Fog is
  applied twice where refraction or SSR samples an already fogged scene
  color; subtract it when that shows.
- The depth prepass for ambient occlusion misses objects that come into
  view on an occlusion-culling frame, for one frame. Redraw the late
  batches into the prepass if the flicker shows.
- Render Bounds and reflection probe handles edit only the active object
  in a multi-selection. A dragged handle jumps by up to the pick radius
  (it snaps to the pointer, not the grab offset).

### Deferred from the `issues.md` audit

Items the audit found but left open, with the reason for each.

- Partial instance-buffer uploads. They need one instance buffer per frame
  in flight plus dirty tracking. The unused `dirty_ranges` code was removed.
- Reuse scratch `Vec`s in extraction, and avoid cloning blended instances
  before sorting. Minor cost.
- Sweep file-backed meshes and textures after a scene `Replace` load. Ten
  call sites load with `Replace`, and game code can hold path-loaded handles
  without `retain`, so an automatic sweep could free live assets. It needs
  handle ownership first.
- Make `Assets::len()` O(1) and move the hot-reload file scan to a worker.
  Both scans are small today.
- Remove the legacy engine's double fence wait. Removing it creates a
  write-after-read hazard on the indirect and physics buffers against the
  previous frame's draw. Check on real hardware with validation layers.
- Rewrite the legacy camera sign convention and remove `camera_rotate`. Needs
  visual checks. Only the `cgmath` removal was done.
- Replace the remaining `unwrap` calls in the legacy engine with errors.
- Enable device features at device creation instead of assuming them.
- Cache the Hierarchy tree and rebuild it only on change detection.
- Check the resource-state hazard with validation layers on real hardware.
- A reflection probe's face handles resize its box symmetrically. Add
  Ctrl-drag to move one face only (it shifts the probe's position too).
- Use the GPU pose in the editor during Play. Bodies with
  `PhysicsSyncMode::SelectedState` or `FullState` get a `GpuStateMirror`,
  but picking, focus, the gizmo, and Save still read the authored
  `Transform`. Picking and focus should prefer the mirror when present; a
  "Keep simulated pose" action should copy it into `Transform` through undo.

### Outside scene edits

- The editor reloads a clean scene when `rusting scene patch` or another
  tool changes its file, and refuses one Save when it has unsaved edits.
  Add a conflict dialog that shows the patch diff and offers keep mine,
  take theirs, or a per-field merge. The watch compares the file time
  first; switch to a file watcher if coarse clocks miss changes.
- Agent panel pending diff: add per-field Accept.
- Agent panel (roadmap Milestone L4): a Blender-style area that lists the
  operation journal of agent edits, shows each pending diff with accept and
  reject, highlights the affected entities in the Hierarchy and Scene View,
  and shows the last scenario results. Accept and reject go through snapshot
  undo like any other scene edit.

### Agent panel design

An area type in the editor-type switcher beside Hierarchy and Console,
named "Agent". Blender-style: compact header, flat rows, no modal dialogs.

- **Header:** area switcher, a Journal / Pending / Results tab strip, and a
  `Pause agent edits` toggle. While paused, outside writes queue as Pending
  instead of reloading.
- **Journal:** one row per outside write the editor saw, newest first: time,
  source (`rusting scene patch`, `rusting mcp`, other), the
  `OutsideChange` counts (added, changed, removed), and the Undo step it
  made. Click a row to select its touched entities (`highlight_scene_ids`).
  Kept in memory for the session, capped like `EditorHistory::MAX_SNAPSHOTS`.
- **Pending:** a diff the editor has not applied: the file changed while the
  scene had unsaved edits, or edits are paused. Each entity shows per-field
  rows (path, old, new) from the same leaf diff `rusting diff` uses. Buttons:
  Accept (apply behind one snapshot undo), Reject (keep the GUI scene and
  record the reject), and Accept all. Accept applies only the chosen
  entities' fields, so a field-level merge replaces the keep-mine or
  take-theirs dialog above.
- **Results:** the last `rusting test --json` run for the open project:
  one row per scenario with pass or fail, tick, and the failing step's
  `expected` and `actual`; perf and budgets from the `perf` report. Read
  from the newest report file, refreshed when it changes.
- **State:** `AgentPanelState` in `EditorState` holds the journal, the
  pending diffs and the pause flag. Widgets come from `gui_elements` and
  colors from `EditorTheme`; no per-panel colors.
- **Rules:** every Accept and Reject goes through `remember_scene_before_edit`;
  an external write never replaces unsaved GUI work without a Pending row.
- **Tests (no GPU):** a clean write journals one row and highlights its
  entities; a dirty-scene write becomes Pending and leaves the scene
  untouched; Accept applies one entity's fields and Undo reverts them;
  Reject leaves the scene and the file unchanged.
- **Order of work:** the journal first (data already exists in
  `reload_external_scene_change`), then Pending with per-field Accept, then
  Results.

## Deferred: particle editor

The particle Inspector (`src/editor/inspector/particles.rs`) ships the
sections, range, gradient and curve widgets, presets, and the live preview.
Later:

- A standalone Particle panel (Blender-style area type) with a larger
  curve and gradient editor and a bigger isolated preview.
- Keyboard nudging and box selection of curve and gradient keys.
- Per-key interpolation (step or smooth) once the runtime supports it.
- A Scene view gizmo for `shape_size` and `direction`.
- Save a tuned emitter as a project preset.

## Deferred: animation timeline

The Timeline area (`src/editor/timeline.rs`) ships the clip header,
transport, ruler scrub, key insert/move/delete, interpolation per track,
record mode and the rest-pose preview. Later:

- Zoom and pan of the time axis; the span is the clip length plus 10%.
- Drag and rename event markers in the Timeline (the Inspector edits them
  today).
- Box select, multi-key move, copy and paste of keys.
- A curve editor (Blender's Graph Editor) for per-channel values.
- Color, emissive and field track preview in Edit mode.
- A Delete-key shortcut scoped to the Timeline area, once shortcuts know
  the hovered area (it now deletes the object).
- Blend space editor: a 1D strip or 2D plane of the `blend` points with a
  draggable marker for the parameter values (Godot's BlendSpace2D editor).
- A "Retarget from…" button in the Animation inspector that picks a
  source object and clip and runs `retarget_clip` through snapshot undo,
  plus a humanoid-map editor that suggests bones by name.
- Ragdoll bone editor: a "Generate from skeleton" button that lists the
  bones with lengths from their children, and capsule and joint-limit
  gizmos in the viewport (Blender's bone envelope display).
- Preview animation layers in the Timeline, with a mask picker that lists
  the object's child paths.

## Deferred: materials and mirrors

- Glass / Mirror presets in the Material section (Opacity + roughness + reflectivity in one click).
- A true planar mirror (second camera render into a texture). Today Reflection Probe + low roughness only approximates one.
- Reword the Reflection Probe environment picker text.

## Deferred: skinned meshes

- Skeleton overlay: done (joint crosses and bone lines in front of meshes,
  click a bone to select its joint). Left: hide it per object or by an
  overlay toggle, and draw Blender octahedral bones instead of lines.
- Blend shape sliders: show `rusting.morph` weights with the glTF target
  names (`extras.targetNames`, not imported yet) as 0–1 sliders.
- IK overlay: draw a line from each `rusting.ik` object to its target and
  pole in the Scene view, and a "Create IK Target" button that spawns an
  empty at the chain end and links it.
- A Skin section in the Inspector that lists joints by name, with missing
  paths shown as warnings, instead of the raw matrix list.
- Pose mode: rotate joints with the gizmo while the Timeline is in Record,
  without selecting each joint in the Scene tree first.

## Deferred: HDR skies

- The Assets panel, its import dialog and `asset_import` list only PNG, JPEG,
  BMP and TGA, so a Radiance `.hdr` sky has to be copied under `assets/` by
  hand. Accept `.hdr` there (no thumbnail needed at first) and offer it in
  the Environment Map texture picker.

## Deferred: light probe grids

- A selected `rusting.light_probe_grid` shows no box or probe dots in the
  viewport. Reuse the reflection probe's face handles for `extents`, draw a
  dot at each probe position, and add a "Rebake" button (the grid rebakes
  only when a field changes).

## Deferred: lightmaps

- `rusting.lightmap` rebakes only when an object, mesh, material or light
  changes. Add a "Rebake lightmaps" button and a texel density preview
  (checker overlay sized by `resolution`) on selected lightmapped objects.

## Deferred: shader graph editor

- `ShaderGraph` (`src/rendering/shader_graph.rs`) compiles nodes to a
  surface shader but has no editor yet. Add a node canvas area (Blender's
  shader editor layout: nodes with typed sockets, drag-to-wire, a node
  search popup) that edits a material's graph and writes the compiled
  source to its `shader` through snapshot undo. Shader errors already show
  in Render Settings.
