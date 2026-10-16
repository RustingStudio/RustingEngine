# Missing in RustingEngine

Things Rusting Raft needed that RustingEngine does not have yet (checked against
RustingEngine 2.4.1, 10e9702). Each entry says what the game wrote instead, so the
workaround can go once the engine has the feature.

## Features

### Raycasts before the first physics step

`GameScene::raycast` reads `PhysicsWorld` bodies, which only `step_cpu_physics` fills
(`set_bodies`, `src/runtime/cpu_physics.rs:1948`). On the first fixed tick
(`fixed_tick` 0) the game's tick sees no collider at all. Child colliders (the island
ground) also take the `GlobalTransform` propagated after the previous frame
(`src/runtime/cpu_physics.rs:2196`), and the first windowed frame runs several steps
(up to 8) before any propagation, so their bodies sit at unpropagated poses until a step
in a later frame: a grid sampled at `fixed_tick` 1 found 484 of 8660 heights.
Scenarios do not show it, since their tick 0 is a zero-delta frame that propagates first.

- Instead: `tick` in `src/app.rs` builds `SceneGround` once `fixed_tick` is past the
  value it had in the first frame, and the session waits for it.
- Wanted: bodies built and posed from propagated `GlobalTransform`s when the scene loads,
  so rays hit from tick 0.

Status: done (this commit, test: `rays_hit_child_colliders_before_the_first_physics_step`).
`cpu_physics::pose_bodies` propagates transforms and builds the bodies when a project loads,
on `GameScene::load_scene` / `restart` (`forget_old_scene`) and after a code-reload restore.
Passed: `cargo fmt --all --check`, clippy default / `--no-default-features` /
`--features gpu-tests` with `-D warnings`, `cargo test --workspace`, and the locked
`--features gpu-tests` run.

### Hardened relay

2.4.0's `run_relay_with` (`src/net.rs:863`, `RelayLimits`) caps connections overall and
per address (IPv6 per /64), new connections per address per minute and the read rate per
connection, skips failed accepts, and gives each handshake its own thread and a deadline.
Still missing next to the game's relay: forwarding writes straight to each socket while
holding the one lock over every room (its `ponytail:` note, `src/net.rs:829`; `forward`,
`src/net.rs:1048`), so a peer that stops reading stalls forwarding, room opens and joins
in every room on the relay for up to 5 s (the write timeout) instead of filling its own
send queue; no cap on open rooms or on clients in a room; no separate limit on room opens
and join attempts; no idle timeout after the handshake; a sender over the rate is slowed,
not dropped.

- Instead: Rusting Raft ships its own std-only relay (`relay/`, `docs/relay.md`) with
  per-IP limits, join and room rate limits, a handshake deadline, per-connection
  bandwidth caps and send queues, and a systemd sandbox unit
  (`deploy/rusting-relay.service`). The game client talks to it with `net/` (`raft_net`),
  whose protocol differs from `NetSession`'s.
- Wanted: those limits in `run_relay`, so games can use `NetSession::host_room` /
  `join_room` directly.

Status: done (this commit, tests: `a_peer_that_stops_reading_stalls_only_itself`,
`a_relay_caps_rooms_clients_and_room_attempts`, `a_relay_drops_a_sender_over_its_rate`,
`a_relay_closes_a_connection_idle_after_its_handshake`,
`a_throttle_allows_one_seconds_burst_then_refuses`,
`a_client_flooding_a_slow_host_is_cut_off_not_the_room_or_others`). Each relay connection writes through
its own bounded queue and writer thread (`RelayPeer`), so nothing touches a socket under the
rooms lock; `RelayLimits` gains `max_rooms`, `max_clients_per_room`,
`room_attempts_per_minute` (per address, refused attempts count), `idle_timeout` and
`send_queue` (bytes); a sender over `bytes_per_second`, or a message larger than one second
of it, is dropped. Each client holds at most `send_queue / max_clients_per_room` of its host's queue; a client
over its share is dropped, not the host or other clients. Known limit: without a token, 16
addresses (16 connections each) can hold all 256 rooms; run a public relay with a token. Passed: `cargo fmt --all --check`,
clippy default / `--no-default-features` / `--features gpu-tests` with `-D warnings`,
`cargo test --workspace`, and the locked `--features gpu-tests` run.

### Scenario step for the mouse wheel

Scenario steps (`StepAction`, `src/scenario.rs:1046`) can move the pointer, click a
label and press actions, but cannot turn the mouse wheel. The game's long windows
(Workbench, Logbook) sit in an egui `ScrollArea` (`src/ui/windows.rs:75`), and `click`
only finds text that is visible, so rows below the fold (Wood Farm, Radio Station) cannot
be clicked by label.

- Instead: `tests/flow_05_farm.json` and `tests/flow_06_radio.json` drag the scroll bar:
  a `pointer` step on the bar, `press` `use` (bound to `MouseLeft`), more `pointer`
  steps, then `release`. This depends on the bar's pixel position.
- Wanted: a `{"scroll": [dx, dy]}` step, or a `click` that scrolls the target into view
  first.

Status: done (this commit, test: `scroll_steps_turn_the_wheel_over_an_egui_scroll_area`).
`StepAction::Scroll` records wheel lines in `RuntimeInput::record_scroll`; the headless egui
pass sends them as `Event::MouseWheel`, and the windowed runtime records `MouseWheel` events
too, so game code reads `RuntimeInput::scroll()`. Passed: `cargo fmt --all --check`, clippy
default / `--no-default-features` / `--features gpu-tests` with `-D warnings`,
`cargo test --workspace`, and the locked `--features gpu-tests` run.

### Cursor grab that follows mouse look and focus

The controller's look code (`player_look`, `src/runtime/player.rs:326`) frees the cursor
when mouse look goes off or Esc goes down, but grabs it again only on the next left click.
So after a window closes (`GameScene::set_mouse_look(.., true)`,
`src/project_runner.rs:872`) the camera does not turn until the player clicks. Losing focus
also keeps the grab: `RuntimeInput::record_focus` (`crates/rusting-core/src/input.rs:235`)
releases keys only, and `GameScene` has no call to grab or free the cursor.

- Instead: `cursor` in `src/app.rs` writes `RuntimeInput::set_cursor_captured` through
  `scene.world().resource_mut::<RuntimeInput>()` every frame: grabbed while mouse look is
  on, so a window closing or an Esc that opens nothing (placing a part) keeps the camera
  turning. Focus loss or the Windows key frees it until the next click, and that click
  does not fire the held tool. The engine's look code runs after the game's update, so
  an Esc the game used still frees the cursor for that one frame.
- Wanted: `GameScene::set_cursor_captured(bool)`, a grab when `set_mouse_look` turns
  look on while the window has focus, a release on focus loss, and a way for the game to
  keep an Esc it used (cancel placing a part) from freeing the cursor.

Status: done (this commit, test: `cursor_grab_follows_mouse_look_focus_and_the_game`).
`GameScene::set_cursor_captured` writes `RuntimeInput::set_cursor_captured`, which also marks
a grab asked for this frame; `player_look` skips its Esc release on that frame.
`set_mouse_look(.., true)` grabs at once while focused, and `RuntimeInput::record_focus(false)`
frees the cursor. Not done: keeping the regrab click from reaching game tools (game code can
check `cursor_captured()` before acting on a click). Passed: `cargo fmt --all --check`,
clippy default / `--no-default-features` / `--features gpu-tests` with `-D warnings`,
`cargo test --workspace`, and the locked `--features gpu-tests` run.

### Scenario step for a raw key

Scenario `press` and `tap` (`StepAction`, `src/scenario.rs:1046`) take a named action
only. A key that no action uses, such as the Windows key (`SuperLeft`), cannot be pressed
in a scenario.

- Instead: the unit test `cursor_follows_look_and_stays_free_after_focus_loss_until_a_click` in
  `src/app.rs` covers the Windows key; `tests/ui_cursor.json` covers focus loss.
- Wanted: a `{"key": "SuperLeft"}` step that presses one key code.

Status: done (this commit, test: `key_steps_tap_a_key_no_action_uses`). `StepAction::Key`
takes a winit `KeyCode` name, records the press before the tick's update and the release
before the next, like `tap`; an unknown name fails to parse. Passed: `cargo fmt --all --check`,
clippy default / `--no-default-features` / `--features gpu-tests` with `-D warnings`,
`cargo test --workspace`, and the locked `--features gpu-tests` run.

### Quaternion-safe animation blending

Clip sampling (`AnimationTrack::sample`, `lerp` in `src/runtime/animation.rs:265`) and
the crossfade (`mix`, `src/runtime/animation.rs:591`) lerp Orientation keys component by
component, with no sign check (the result is only normalised when written). Two keys for
the same rotation with opposite signs (q and -q) pass near zero in between, and the bone
collapses or spins. A
crossfade also keeps a track that only the old clip keys at full weight, so that bone
stays where the old clip left it.

- Instead: the hand-keyed "swim" and "tread" clips (`swim_clips` in
  `tools/scene/build_scene.py`) keep w <= 0 like the imported clips, add a second key at
  the same time wherever the sign flips, and key Position and Orientation on every bone.
  The crawl arms are keyed as a swing from rest with no twist (`stroke`), so a full
  stroke ends on the same key instead of the negated one. Every clip on a swimming human
  also gets rest keys for the bones it leaves alone, so the swim roll does not stay on
  after the fade into a clip that never keys the root.
- Wanted: nlerp or slerp with a shortest-path sign flip for Orientation in `sample` and
  `mix`, and a crossfade that eases tracks only one side has back to rest.

Status: done (this commit, tests: `orientation_blends_take_the_short_way_round`,
`a_crossfade_eases_tracks_the_new_clip_leaves_back_to_rest`). `sample` (Linear and Smooth),
`mix` and override layers nlerp Orientation after flipping the far key to the near sign.
`AnimationPlayer` records each Position / Rotation / Scale / Orientation value before it
first writes it, and a crossfade blends a track only the old clip keys toward that rest
value, ending on it. Tracks only the new clip keys still start at full weight. Passed:
`cargo fmt --all --check`, clippy default / `--no-default-features` / `--features gpu-tests`
with `-D warnings`, `cargo test --workspace`, and the locked `--features gpu-tests` run.

## Features for v2 (checked against RustingEngine 2.5.6)

What the v2 plan (`docs/superpowers/specs/2026-10-14-rusting-raft-v2-design.md`) needs.
E1 to E6 were planned in spec 4.3; E7 onward turned up while building it.

### E1. Open ocean in `rusting.water` (staged)

`rusting.water` is a fixed rectangle of three sines. It has no endless sea around the camera,
no Gerstner waves, no way to change the waves at runtime, and no game clock.

- Instead: nothing. The change is written and tested (the water tests pass, clippy is clean)
  and the game already uses it. It is uncommitted in `~/RustingEngine` on local branch
  `raft-v2-gaps` (made from the v2.5.6 tag), with a `CHANGELOG.md` `[Unreleased]` entry. Commit
  it there for v2.5.7.
- Wanted: `WaterBody` gains `waves: Vec<OceanWave>` (`direction`, `length`, `height`,
  `steepness`, with analytic normals), `ocean: bool` (the grid follows the active camera, its
  cells grow away from it, and bodies float everywhere) and `time: Option<f64>` (a game clock in
  place of the fixed tick's). `WaterBody::wave` returns the swayed Gerstner surface. `WaterBody`
  stops being `Copy`, and `wave` takes its time as `f64`.

Status: split into E1a (waves, clock, `f64` time) and E1b (`ocean`). The staged change on
`raft-v2-gaps` is not on this machine, so both are rebuilt here.
E1a: done (this commit, tests: `gerstner_waves_sharpen_crests_and_keep_time_in_f64`,
`a_water_clock_moves_the_mesh_in_place_of_the_fixed_tick`). `WaterBody` gains
`waves: Vec<OceanWave>` (`direction`, `length`, `height`, `steepness`; deep-water speed,
analytic normals) and `time: Option<f64>`; `WaterBody::clock` picks the seconds the mesh,
buoyancy and swimmers use. `wave` takes `f64` time, wraps phases in `f64`, and returns the
swayed surface over `(x, z)` (8 fixed-point steps back to the swayed start point). `WaterBody`
is no longer `Copy`. Passed: `cargo fmt --all --check`, clippy default /
`--no-default-features` / `--features gpu-tests` with `-D warnings`, `cargo test --workspace`,
and the locked `--features gpu-tests` run.
E1b: done (this commit, test: `an_ocean_follows_the_camera_and_floats_bodies_everywhere`).
`WaterBody::ocean` makes `contains` true everywhere (buoyancy and swimming), and `sync_water`
centers the grid on the active camera (same pick as `active_camera`, snapped to whole center
cells) with cells a quarter of the even size at the center growing to 1.75x at the rim.
Passed: `cargo fmt --all --check`, clippy default / `--no-default-features` /
`--features gpu-tests` with `-D warnings`, `cargo test --workspace`, and the locked
`--features gpu-tests` run.

### E2. Clouds on `rusting.procedural_sky`

- Instead: the sky has no clouds, so weather barely shows overhead.
- Wanted: `cloud_cover`, `cloud_darkness`, `cloud_speed` and a wind direction, with clouds that
  catch and dim the sun.
- Status: done (this commit, test: `clouds_grey_the_sky_and_hide_the_sun` (GPU) and
  `clouds_drift_with_the_wind_and_clamp_their_cover`). `ProceduralSky` gains `cloud_cover`,
  `cloud_darkness`, `cloud_speed` and `wind_direction`; the background pass draws an fbm sheet
  2 km up that drifts with `elapsed_seconds`, glows toward the sun and hides its disc. The sky
  bake (lighting, reflections) stays cloudless so it does not rebake as clouds move. Passed: fmt,
  clippy (default, no-default-features, gpu-tests), `cargo test --workspace`, gpu-tests under the
  lock. Looks on real hardware: needs hardware check.

### E3. Particles stopped by roofs

- Instead: rain falls through roofs and into cabins.
- Wanted: an emitter option that kills particles below a height field or inside
  `rusting.particle_blocker` boxes.
- Status: done (this commit, test: `blocked_rain_stops_at_roofs_and_the_ground`).
  `ParticleEmitter::blockers` kills CPU particles inside `ParticleBlocker { extents }` boxes
  (turned and scaled with the object) and below any `Heightfield`, in the fixed step and the
  editor preview. GPU simulated emitters ignore it. Passed: fmt, clippy (default,
  no-default-features, gpu-tests), `cargo test --workspace`, gpu-tests under the lock.

### E4. Global wetness

- Instead: only terrain can look wet (`set_terrain_look`). Decks, sails and rocks stay dry in
  rain.
- Wanted: one global `wetness` value from 0 to 1. Materials marked `wettable` get darker albedo
  and lower roughness as it rises.
- Status: done (this commit, test: `wetness_darkens_only_wettable_materials`). Scene files do
  not save `wettable` yet (needs a cooked format version); games set it in code.

### E5. GPU skinning (done)

The engine has had GPU skinning (`rendering::gpu_skinning`) since 2.5.0, so nothing is
needed here. What is still missing is E15.

### E6. Lens droplets post effect

- Wanted: a post effect for rain on the camera lens, with a strength from 0 to 1. It is for
  the first-person view in storms.
- Status: done (this commit, test: `lens_droplets_bend_the_image_and_follow_the_tick`).
  `rusting.color_grading` `lens_droplets`, drawn in the tone-map pass.

### E7. Viewmodel layer with its own FOV

A camera has only `projection`, `active`, `priority` and `viewport`. There is no render layer
and no second pass that clears depth.

- Instead: the held tool is a child of the camera, 0.45 m in front. It shares the world FOV and
  can poke into walls.
- Wanted: a render layer on `MeshRenderer`, and a camera that draws only that layer over the
  main view after clearing depth, with its own FOV (spec 5).

Status: split into E7a (renderer and components) and E7b (scene files, editor, schema).
E7a: done (this commit, tests: `viewmodel_meshes_draw_over_walls_with_their_own_fov` (GPU),
`viewmodels_extract_only_under_a_viewmodel_camera`). The layer is a `RenderLayer` component
(`World`, `Viewmodel`) next to `MeshRenderer`, since `MeshRenderer` literals are everywhere.
The camera side is `ViewmodelCamera { vertical_fov_radians }` on the active camera. There is
no second pass: viewmodel meshes scale clip-space xy to their FOV and squash their depth
toward the near plane, so they draw over walls in the same pass and keep true world lighting.
Frustum culling still uses the main camera's FOV.
E7b: done (this commit, test: `examples_load_through_the_scene_parser_and_registry_unchanged`).
Scene files, snapshots, the editor and the schema know `rusting.render_layer` and
`rusting.viewmodel_camera`.

### E8. Reading the resolved quality profile

`resolve_quality` runs inside the renderer. On an integrated GPU it picks Eco, under 4 GB of
VRAM it picks Balanced, and otherwise High. Game code only sees the requested
`RenderSettings::quality`.

- Instead: game-side content (ocean grid, foliage counts, rain particles, view distance) treats
  Auto like Medium.
- Wanted: a read-only `RenderSettings::resolved_quality`, or a `GameScene` getter.
- Status: done (this commit, test: `resolved_quality_follows_the_gpu_once_a_renderer_starts`).
  `GameScene::resolved_quality()`; the game runner now puts `RendererCapabilities` in the
  world, as the editor already did. It stays `Auto` before a renderer starts.

### E9. Shadow cascade count

`ShadowQuality` sets the map size and distance: Low 1024 and 30 m, Medium 2048 and 50 m,
High 4096 and 80 m. It does not set how many cascades there are.

- Wanted: a cascade count per setting. Spec 4.2 asks for 2 on Low, 3 on Medium and 4 on
  High/Ultra.
- Status: done (this commit, test: `shadow_settings_override_the_profile`).
  `shadow_settings` returns the cascade count: 2 on Low/Eco, 3 on Medium/Balanced and 4 on
  High. The atlas stays 2x2, so fewer cascades leave tiles unused at the same texel size.

### E10. Texture resolution cap

- Wanted: a setting that limits loaded textures to a maximum size or mip level. Spec 4.2 ships
  2K textures with 1K mips, and Low should load only the 1K level so it fits the iGPU's memory
  and bandwidth.
- Status: done (this commit, test: `max_texture_size_reuploads_textures_at_the_capped_size`,
  `texture_cap_halves_each_layer_until_it_fits`). `set_max_texture_size(1024)`; HDR textures
  keep full size.

### E11. Water waves on the GPU

`sync_water` rebuilds and re-uploads the whole water mesh every fixed tick (the `ponytail` note
in `water.rs` says so too). An ocean at resolution 96 has about 9,400 vertices, which means
about 0.6 MB and roughly 1 ms of CPU per tick. Storm seas need finer grids.

- Wanted: wave displacement in the vertex shader.
- Status: split into E11a (the vertex shader displaces a flat grid by `RenderWorld::water_waves`)
  and E11b (`sync_water` builds the flat grid once and extraction fills the waves each tick).
  - E11a Status: done (84f4f6b, test: `water_waves_lift_a_flat_grid_in_the_vertex_shader`).
  - E11b Status: done (this commit, tests: `gpu_waves_trace_the_surface_the_cpu_samples`,
    `a_water_clock_sets_the_waves_in_place_of_the_fixed_tick`).

### E12. Foam on wave crests

Foam is drawn only at the shore (`shore_foam`). Vertex colour cannot add it either, because it
multiplies the base colour and so cannot turn blue water white.

- Wanted: a `crest_foam` strength in the water shader, driven by how much the Gerstner sway
  pinches the surface (its Jacobian). Spec 4.1 asks for foam on crests and around the raft.
- Status: done (this commit, test: `crest_foam_whitens_where_steep_waves_pinch_the_surface`).

### E13. `rusting test` building once

`rusting test . --keep-going` runs `cargo build` again before every scenario.

- Instead: no source edits while the suite runs, since later scenarios would pick up the
  changes or fail.
- Wanted: build once and run every scenario with that binary. This is faster and keeps a laptop
  cooler.
- Status: done (this commit, test: `a_scenario_folder_runs_every_scenario_in_the_one_build`).

### E14. Mouse wheel input actions

`rusting.input_action` takes keys, `MouseLeft`/`MouseRight`/`MouseMiddle` and gamepad buttons
(`src/runtime/actions.rs:33`), but not the wheel.

- Instead: the 10-slot hotbar is picked with keys 1-0 only.
- Wanted: `WheelUp` / `WheelDown` as pressable inputs (one press per wheel line), or
  `GameScene::scroll_delta()`.
- Status: done (this commit, `GameScene::scroll_delta()`, test: `a_tick_function_reads_each_wheel_turn_once`).

### E15. Copies keep animation and skin

`copy_tree` (`src/project_runner.rs:4107`) copies the meshes, but the copy's Animation tracks
still point at the original's bones, so only the original animates.

- Instead: the first shark plays its clips. A second or third shark only sways
  (`src/view/movers.rs`, ONE_INSTANCE). Players use a fixed pool of 7 avatars in the scene.
- Wanted: `copy_tree` remaps Animation tracks and skin joints onto the copied bones, so a copy
  animates on its own.
- Status: done (this commit, test: `copies_of_copies_animate_their_own_bones`). Tracks and
  joints already resolve copied bones by path; a copy of a copy named them
  `"Shark 3/Shark 2/Tail"`, so path `Tail` missed. `copy_tree` now drops the old prefix.

### E16. Listing a node's children

`GameScene` has no way to list the children of a named node (only `SceneTree::children`
inside the engine), and `set_color` changes one node.

- Instead: the four coloured parts of a raft tile are written out in the game code
  (`TILE_PARTS` in `src/view/mod.rs`), so a damaged tile can be darkened part by part.
- Wanted: `GameScene::children(name) -> Vec<String>`, or `set_color_tree(name, tint)` that
  multiplies the colour of a node and every node under it.
- Status: done (this commit, test: `children_lists_the_named_parts_of_a_node`).
  `GameScene::children`; tint a tree with it plus `color`/`set_color`.

### E17. Foliage of a multi-part model

`Foliage` scatters copies of one object's mesh. The Kenney palms are two meshes (trunk and
leaves), so they cannot be foliage.

- Instead: palms are placed one by one at the sampled terrain height, each with a `sway`
  material (`tools/scene/build_scene.py`).
- Wanted: `Foliage` on a parent copies the whole subtree as one instanced group, or takes a
  list of meshes that share each copy's transform.
- Status: done (this commit, test: `foliage_on_a_parent_scatters_every_part_together`).
  Each mesh under a `Foliage` parent draws one instanced batch per part.

### E18. Foliage density per quality preset

`Foliage::count` is fixed; the quality profile does not thin it.

- Instead: the client stores each foliage's base count and rescales `Foliage::count` when the
  graphics preset changes (`src/app.rs`, `Graphics::foliage_share`: Low 0.3, Medium 0.6,
  High/Ultra 1.0).
- Wanted: a per-profile foliage share (and far impostors on Ultra) in the engine.
- Status: done (this commit, test: `quality_profiles_thin_foliage_from_the_first_copies`).
  `foliage_share` (Eco 0.3, Balanced 0.6, High 1.0) thins every scatter at extraction. There
  is no Ultra profile, so far impostors are left out; `rusting asset bake-foliage` cards cover them.

### E19. Heightfield from an image

`Heightfield.heights` is inline `Vec<Vec<f32>>` in the scene JSON. The 14 island terrains
add about 160k samples (~45k lines) and 0.8 MB to `scenes/main.rscene`.

- Instead: the generator writes the samples inline.
- Wanted: a heightfield that reads a 16-bit PNG or raw file under `assets/` with a height
  scale, like the textures do.
- Status: done (this commit, test: `heightfields_read_their_heights_from_an_image_under_the_scene`).
  `Heightfield.image` (relative to the scene) and `height_scale`; saved scenes drop the samples.

### E20. World pose of a named bone

`GameScene` has no `bone_world(object, bone)`. `skinning::joint_matrices` gives skin
matrices, not a world pose.

- Instead: the octopus holds a player at a fixed offset from its body. (Finding the bone with
  `scene.world()` plus `scene_tree::find_path` and combining its parents' transforms by hand
  would work but reaches into engine internals.)
- Wanted: `GameScene::bone_world(object, bone) -> Option<[f32; 16]>` (or position and
  rotation) after this frame's animation.
- Status: done (this commit, test: `bone_world_gives_a_named_bones_pose_in_the_world`).

### E21. Global wind for foliage

Foliage and `sway` materials lean only inside force fields (`field_pushes` skips copies
outside a field), and a Wind force field also pushes every GPU particle
(`gpu_particles.comp:157`).

- Instead: one scene-wide `wind` sensor box (2400 x 300 x 2400 m) with a weak Wind field
  (0.5 m/s²), so rain, smoke and fire are pushed too; higher `sway` values make up for it.
- Wanted: a scene-level wind (direction, strength, gusts) that sways foliage and materials
  without being a force field, with a per-emitter wind factor for particles.
- Status: done (this commit, test: `scene_wind_pushes_particles_by_their_wind_factor`,
  `scene_wind_reaches_the_lean_and_gpu_particles_but_not_bodies`,
  `swaying_materials_bend_plain_meshes_in_wind_fields`). `rusting.wind` and
  `ParticleEmitter::wind_factor`.

## Bugs seen

- The frame cap (`FramePacer::request_next_frame`, `src/rendering/frame_pacer.rs:77`)
  sets `next_frame = now + interval` from the moment the loop wakes, so each late wake
  from `ControlFlow::WaitUntil` adds to the period and the rate settles below the cap.
  `next_frame += interval` (back to `now` only when far behind) holds the cap. The
  game's Max FPS setting (`GameScene::set_max_fps`) inherits this.
  Status: already present (`src/rendering/frame_pacer.rs:96`, `FramePacer::due` steps
  `next_frame += interval`; test `the_limiter_holds_its_rate_through_late_wakes_and_uneven_frames`).
- `FrameTime::real_delta` is documented as real time since the last frame, but
  `RuntimeApp::update` (`src/runtime/mod.rs:686`) clamps it to one fixed step while the
  app is overloaded, so a frame counter built on it never reads below 60. The game's FPS
  counter (`src/ui/mod.rs`) reads `Instant` instead.
  Status: done (this commit, test: `fixed_steps_slower_than_real_time_fall_back_to_slow_motion`
  asserts `real_delta` 50 ms while `delta` is clamped to 1 ms). `App::update` clamps only the
  clock; replays keep the real delta in `ReplayFrame::real_nanos`. Passed: `cargo fmt --all
  --check`, clippy default / `--no-default-features` / `--features gpu-tests` with `-D warnings`,
  `cargo test --workspace`, and the locked `--features gpu-tests` run.

