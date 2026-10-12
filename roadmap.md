# RustingEngine Roadmap

RustingEngine is currently a functional Vulkan renderer prototype with a native Rust ECS runtime, an egui editor, and a GPU-accelerated hybrid physics bridge. The goal of this roadmap is to turn it into a complete, general-purpose Windows/Linux game engine in the same class as Godot — covering 3D and 2D rendering, animation, audio, UI, navigation, networking, a full editor, and export — whose defining strength is physics. RustingEngine should be the engine people pick *because* of its physics, and the best engine to build games with an LLM coding agent.

The roadmap covers these tracks:

- **The engine** (Milestones 0-23): Godot-class feature breadth, with physics as the flagship subsystem. Milestones 0-7 build the foundation and a vertical slice; Milestone 8 makes simulation deterministic; Milestones 9-23 reach feature parity with Godot while taking physics well beyond it.
- **LLM-native development** (Milestones L1-L10): the engine as the best place for an LLM agent to write, run, observe, test, and ship a game, proven by a published agent benchmark.
- **Engine expansion** (Milestones 30-35): gameplay framework, large worlds, cinematics, procedural content, modding, and XR, which a general-purpose engine needs beyond the Godot parity map.
- ***Sundering*** (Milestones 24-29): a deterministic, networked, destructible 5v5 competitive game built on the engine. It is the engine's hardest physics customer and its proof that the physics claims are real.

This document is the implementation source of truth. Tasks should be completed in dependency order, kept behind compiling intermediate states, and verified against the acceptance gates at the end of each milestone.

Long-lived ownership boundaries and dependency rules are recorded in [`architecture.md`](architecture.md). Roadmap work must preserve those boundaries or document a migration before changing them.

## Working agreement for implementation sessions

This roadmap lists outcomes, not tasks. Turning an outcome into a task is part of the work, not a reason to stop.

### How to pick an item

Pick the lowest-numbered unchecked item whose dependencies are satisfied. Milestones 2, 3, 4, and 5 are parallel tracks and may be interleaved. After Milestone 9, Milestones 10-22 are parallel tracks and may be interleaved, subject to the dependencies stated at the top of each milestone; physics milestones (10-12) take priority when two items are otherwise equally ready. The LLM-native milestones (L1-L10) and the engine expansion milestones (30-35) are parallel tracks too; while the owner's current focus is agent-built games, LLM-native items take priority over every other track. If an item is too large for one session, split it, implement the first part, and record the split in this file as sub-items.

### What counts as verification

Not every item is provable the same way. Match the proof to the item instead of treating "I cannot run the game" as a universal blocker.

| Kind of item | Required proof |
| --- | --- |
| Types, APIs, error handling, identity mapping | Unit test |
| ECS, assets, serialization, scene round-trip | Unit test, no GPU |
| Extraction, dirty ranges, ordering, capacity policy | Unit test on data structures, no GPU |
| Buffer layout, push constants, vertex formats | Generated/reflected layout assertion, no GPU |
| Shader source changes | Shader compiles in the build, plus a layout assertion |
| Compute dispatch behaviour, readback, physics results | Headless Vulkan test, software device acceptable |
| Rendered output correctness | Headless offscreen render plus golden-image comparison |
| CPU physics behaviour (stacking, joints, CCD, queries) | Deterministic headless unit/scenario test, no GPU |
| Editor panel behaviour | Unit test on editor state/commands; golden image for compositing |
| Audio mixing and DSP | Offline render to buffer plus sample comparison, no device |
| Frame pacing, throughput, memory growth | Hardware GPU run, cannot be verified in software |
| Determinism across vendors | Two different Vulkan implementations |

Only the last two rows genuinely require hardware. Everything above them is verifiable on a headless machine once Milestone 0.5 exists. Until Milestone 0.5 exists, GPU-behaviour items are blocked by missing tooling, not by the roadmap.

### When blocked

Do not stop with nothing delivered. In order:

1. Implement the part that is verifiable now, behind a compiling intermediate state.
2. Write the unverified part as a test marked `#[ignore]` with the reason, so the proof is ready when hardware is.
3. Add the blocker as a new unchecked item in this file, naming what is missing.
4. Move to the next item.

### Constraints must live in the repository

Any file or subsystem an implementation session must not touch belongs in `AGENTS.md` at the repository root, with the reason. A constraint that exists only in one session's context cannot be respected by the next session and must not be invented.

### Performance numbers

Performance targets are acceptance gates for a milestone, never individual tasks. Never treat a frame-rate number as an item to implement.

## Product target

The engine's 1.0 release (Milestone 23) should provide everything a team expects from Godot, plus physics no general-purpose engine offers:

- A real application runtime with ECS entities, components, resources, schedules, events, observers, reflection, input, and fixed updates.
- Reusable scene composition: prefabs/instanced scenes with nested overrides, groups, signals, and data resources.
- A Vulkan renderer with explicit frames in flight, forward PBR, shadows, transparency, HDR, global illumination, reflection probes, screen-space effects, volumetric fog, post-processing, decals, GPU particles, culling, LOD, and profiling.
- A first-class 2D pipeline: sprites, tilemaps, 2D lights, 2D particles, and 2D physics.
- Custom shaders through a stable engine shader language and a visual shader graph.
- Skeletal and property animation, animation state machines, blend spaces, IK, retargeting, and tweens.
- An audio engine with buses, effects, streaming, and 3D spatialization.
- A runtime UI toolkit with layout containers, themes, rich text, localization, and accessibility.
- Navigation meshes, agents, and avoidance for 2D and 3D.
- High-level multiplayer: RPCs, replicated spawning/synchronization, and deterministic rollback.
- Typed, deduplicated assets with glTF import, per-asset import settings, texture compression, scene serialization, and hot reload.
- An in-engine egui editor at Godot parity: viewport, hierarchy, inspector, asset browser, console, debugger, profiler, gizmos, animation/shader/tilemap editors, project settings, export, and an editor plugin API.
- Fast iteration: Rust game-code hot reload and an optional sandboxed scripting layer.
- Export to Windows, Linux, and macOS, then Android.
- Documentation, tutorials, templates, and demo projects for every major feature.
- **Best-in-class physics** — see the next section.
- **Best-in-class LLM development** — a versioned, budgeted, machine-readable engine surface; a generation-friendly Rust API; model-oriented observation, verification, and quality lints; a warm daemon and MCP adapter; and a published agent benchmark. See the LLM-native track.
- A gameplay framework (saves, state machines, behaviour trees, dialogue, inventory), terrain and streaming large worlds, cinematics, and procedural content. See the engine expansion track.

Primary platforms are Windows and Linux desktop. Native Rust systems are the gameplay API, and games remain normal Cargo projects that can use external libraries. A versioned custom physics-compute ABI is part of the hybrid milestone. Deferred rendering, web export, iOS, and consoles remain out of scope until after 1.0.

## Physics is the flagship

Godot ships a capable general-purpose physics engine (Godot Physics or Jolt). RustingEngine must be measurably better, not merely equivalent. "Best physics" is defined by the following pillars, each owned by a milestone and each proven by a benchmark or test rather than asserted:

| Pillar | What it means | Owner |
| --- | --- | --- |
| Scale | Hundreds of thousands to millions of simultaneously simulated bodies through GPU solvers, with per-body CPU/GPU allocation | M5, M11 |
| Gameplay connection | GPU simulation is never a black box: typed conditions, asynchronous events, commands, and selective readback reach normal Rust gameplay code | M5 |
| Determinism | Bit-identical simulation across runs, builds, thread counts, and (where proven) GPU vendors; replay and rollback built in | M8 |
| Breadth | Rigid, articulated, character, vehicle, ragdoll, soft-body, cloth, rope, fluid, granular, and fracture simulation in one world with two-way coupling | M10, M11 |
| Correctness | Stable stacking, continuous collision, robust contacts, and conservation behaviour verified by a regression scenario suite | M5, M10 |
| Tooling | A physics debugger with recording, timeline scrubbing, per-body inspection, and authoring tools in the editor | M12 |
| Integration | Animation, particles, audio, navigation, and networking consume physics through the same bridge instead of parallel ad-hoc systems | M13-M19 |
| Evidence | A published, repeatable benchmark suite comparing RustingEngine against Jolt, PhysX, Rapier, and Godot Physics on identical scenes | M12 |

Every other subsystem is judged against Godot parity. Physics is judged against the best standalone physics engines.

## Godot parity map

Each Godot feature area has an owning milestone. A feature area is at parity when its milestone's exit gate passes.

| Godot area | RustingEngine equivalent | Milestone |
| --- | --- | --- |
| Node tree, scenes, `PackedScene` instancing, inheritance | ECS hierarchy, prefabs with nested overrides | M1, M9 |
| Signals, groups | Typed events, observers, entity groups | M1, M9 |
| Resources (`.tres`) | Typed data assets with reflection | M2, M9 |
| GDScript / C# / GDExtension | Native Rust, Rust hot reload, optional WASM scripting | M9 |
| Godot Physics / Jolt 3D | Hybrid CPU/GPU physics | M5, M10-M12 |
| Soft bodies, cloth | XPBD deformables | M11 |
| Forward+/Mobile/Compatibility renderers | Forward PBR with quality profiles and capability fallback | M3, M4 |
| SDFGI/VoxelGI/LightmapGI, probes, SSAO/SSR/SSIL, fog, glow | Advanced rendering | M13 |
| Shading language, visual shaders | Engine shader language and shader graph | M13 |
| GPUParticles, decals, MultiMesh, CSG, GridMap | Advanced rendering and editor tools | M13, M20 |
| AnimationPlayer, AnimationTree, IK, tweens | Animation system | M14 |
| Audio buses, effects, 3D audio | Audio engine | M15 |
| Control nodes, themes, RichTextLabel, translations | Runtime UI and localization | M16 |
| 2D renderer, TileMap, 2D physics, 2D lights | 2D engine | M17 |
| NavigationServer, agents, avoidance | Navigation | M18 |
| High-level multiplayer, ENet, WebSocket, HTTP | Networking | M19 |
| Editor docks, debugger, profilers, editor plugins, asset library | Editor parity | M20 |
| Export templates, platforms | Platforms and export | M21 |
| Documentation, demo projects | Documentation and ecosystem | M22 |

## Second product target: Sundering

*Sundering* is a 5v5 competitive game whose terrain is made of millions of simulated chunks that come apart permanently. Its design document lives at `~/Code/games/Sundering/README.md`. It is built after the engine's physics pillars exist and consumes them harder than any other project will.

Sundering demands four things a general-purpose engine does not provide by default:

- **Determinism.** Competitive play requires the simulation to produce identical results for every participant. This constrains the physics solver, the GPU reduction strategy, and the ordering of every simulation operation. The engine provides it in Milestone 8.
- **Authoritative networking.** A server owns the simulation; clients must stay consistent with it under real latency and loss. The engine provides general networking in Milestone 19; Sundering adds competitive authority in Milestone 24.
- **Destructible terrain as simulation state.** Terrain is not authored geometry. It is a first-class simulated entity that feeds collision, navigation, and vision. It builds on the engine's fracture support from Milestone 11.
- **Navigation over geometry that changes every tick.** Nav meshes assume a static world. Sundering has no static world. It builds on the engine's navigation from Milestone 18.

**Determinism cannot be retrofitted.** Milestone 8 defines the full requirements and their acceptance gate, but three of its ordering rules must be respected while Milestone 5 is implemented, or that work will need rewriting. They are restated at the top of Milestone 5.

Milestones 24-29 depend on Milestones 8, 10, 11, 18, and 19. Milestone 8 must precede any networking milestone, because the determinism result decides which networking model is available.

## Architectural direction

The project should become a Cargo workspace with one-way dependencies:

```text
rusting-math       deterministic math, fixed-point, seeded RNG streams
      ↑
rusting-core       ECS components, schedules, time, input, hierarchy, events, reflection
      ↑
rusting-assets     typed handles, cache, importers, serialization, prefabs, hot reload
      ↑
rusting-physics    2D/3D CPU physics, GPU compute simulation, deformables, fluids, synchronization, queries
      ↑
rusting-terrain    heightmap and volumetric terrain, fracture, structural load, Scar persistence
      ↑
rusting-nav        navigation meshes/volumes, flow fields, agents, avoidance
rusting-anim       skeletal and property animation, state machines, IK, physics-driven animation
      ↑
rusting-render     Vulkan context, extraction, frame graph, 2D/3D passes, materials, shaders, particles, profiling
rusting-net        transport, wire protocol, RPC, replication, rollback, interest mechanism
rusting-audio      device management, mixing, buses, effects, spatialization
rusting-ui         runtime UI layout, themes, text shaping, localization, accessibility
      ↑
rusting-gameplay   teams, match state, abilities as forces, vision, bots
      ↑
rusting-editor     egui panels, viewport, inspector, gizmos, play controls, editor plugins
rusting-script     optional sandboxed WASM scripting host
      ↑
rusting-engine     plugins, application facade, compatibility API, export
      ↑
game projects      vertical slice, demo projects, Sundering
```

`rusting-nav` and `rusting-anim` are siblings. `rusting-render`, `rusting-net`, `rusting-audio`, and `rusting-ui` are siblings at one level and must not depend on each other; UI and particles reach the screen through render extraction, not by calling the renderer. `rusting-editor` and `rusting-script` are siblings. Full layer definitions are in [`architecture.md`](architecture.md).

Dependency cycles between runtime, renderer, physics, assets, and editor are not allowed. ECS entities are the canonical identity and authored scene state. A GPU-owned body's newest runtime transform may live on the GPU; ECS keeps its stable ID, settings, last synchronized state, and pending events. Render batches, physics arrays, indirect buffers, and other GPU representations are derived data.

The existing `Engine::new`, `add_cube`, `add_sphere`, `add_gltf`, and `run` API remains temporarily available as a deprecated compatibility facade implemented over the new runtime.

## Current focus: agent-built games and the editor

Owner direction (2026-09): the editor, example games, and a workflow where an
agent builds a whole game end to end with the `rusting` CLI and project files
alone, with no MCP server or glue scripts. Deep physics items are paused.

Method: build a small game with the CLI only, log every gap, fix the gaps,
keep the game as a sample, then repeat with another genre. Editor work comes
from the `docs/editor-overhaul.md` backlog, and the CLI and the editor should
share one undoable command layer.

- [x] Game 1, Hammer Run (`samples/hammer_run`). Evidence: `rusting check`
  and its three scenarios pass from the repository; captures show the course,
  the HUD and a collected gem; `hammer_hits` fails with the respawn code
  removed.
- [x] Gap: `scene patch` rejected partial built-in sections. Missing fields
  now take their defaults. Evidence: `scene_patch::tests::created_sections_take_defaults_for_missing_fields`.
- [x] Gap: patches needed UUIDs, so an agent had to query first. `id` and
  `parent` take unique names. Evidence: `scene_patch::tests::operations_address_entities_by_unique_name`.
- [x] Gap: new projects had no guide for an agent. `rusting new` writes
  `AGENTS.md`. Evidence: `project::tests` create-project test.
- [x] Gap: scenarios could only check exact ticks or "holds through".
  `within` passes on the first tick a check holds. Evidence:
  `scenario::tests::within_passes_on_the_first_tick_the_check_holds`.
- [x] Gap: game code could not see contacts or the ECS world.
  `GameScene::touching` and `GameScene::world`. Evidence: Hammer Run's
  `hammer_hits` scenario.
- [x] Gap: CLI commands needed an explicit project root; they default to
  `.`. Evidence: `project_root_defaults_to_the_current_folder` in
  `src/bin/rusting.rs`.
- [x] Gap: `scene patch` could not change scene-level fields. `set_scene`
  does. Evidence: `scene_patch::tests::set_scene_changes_scene_fields_but_not_entities`.
- [x] Gap: a scenario could not set up state. `set` steps write a transform
  or component value before a tick. Evidence:
  `scenario::tests::set_steps_move_entities_before_the_tick_runs`, and a
  Hammer Run scenario that sets the `Goal Count` counter and the player's
  position.
- [x] Gap: `GameScene` had no counter helper. `GameScene::counter(name)`.
  Evidence: `project_runner::tests::game_code_reads_and_sets_counters_by_name`.
  Other components stay behind `world()`.
- [x] Gap (deferred with deep physics): a kinematic player is never pushed
  by dynamic bodies, so hazards need game code. Pushing it out of overlaps
  was tried: the solver treats the kinematic player as infinite mass, so a
  motorised sweeper stalls against it and only nudges it a few centimetres.
  Real knockback needs the player as a finite-mass body in the solver. The
  agent guide documents the `touching` pattern instead. Closed by the
  dynamic player option (Milestone 10): a `PlayerController` on a `Dynamic`
  body is a finite-mass solver body. Evidence:
  `runtime::tests::a_motorised_sweeper_knocks_a_dynamic_player_away` (a
  20 kg bar on a 20000 N·m hinge motor carries an 80 kg idle player more
  than 1 m and keeps turning). Limit: the walk brakes at up to 40 m/s² times
  the mass (3200 N for 80 kg), so Hammer Run's 5000 N·m sweepers still
  stall against a player 2 m from the pivot; `src/project_agents.md` says
  so. Kinematic players are unchanged. Full check 2026-10-10: fmt and
  clippy x3 clean; lib 821 tests, 922 with gpu-tests.
- [x] Gap: `run --ticks` reported nothing about the end state. It saves the
  scene to `build/final.rscene` for `scene query`; game `eprintln!` output is
  in the `--json` result. Evidence: `rusting run samples/hammer_run --ticks
  120` then `rusting scene query samples/hammer_run/build/final.rscene`.
- [x] Game 2, Target Range (`samples/target_range`), a first-person
  shooting gallery. Evidence: `rusting test samples/target_range
  samples/target_range/tests` passes all three scenarios; a capture shows
  the targets, crosshair and HUD.
- [x] Gap: games had no custom input. `rusting.input_action` binds a named
  action to keys and mouse buttons from the scene; `GameScene::pressed` and
  `held` read it. Evidence:
  `project_runner::tests::scene_input_actions_bind_and_report_presses`.
- [x] Gap: game code could not ray cast or remove objects.
  `GameScene::raycast`, `aim` (along the active camera), `despawn`,
  `set_visible` and `trigger`. Evidence:
  `project_runner::tests::aim_hits_what_the_camera_faces_and_despawn_removes_it`.
- [x] Gap: a scenario could not check that an object is gone. Expectations
  take `"exists": false`. Evidence:
  `scenario::tests::exists_checks_entities_and_paths` and Target Range's
  `first_shot`.
- [x] Gap: `rusting test` ran one file per call. With a folder, or no
  argument (`tests/`), it runs every scenario and lists each result.
  Evidence: `rusting test` in `samples/target_range`.
- [x] Gap: `rusting new` wrote no `.gitignore`, so `target/` and `build/`
  showed up in git. Evidence: `project::tests` create-project test.
- [x] Game 3, Sky Hop (`samples/sky_hop`), a 2D platformer. Evidence:
  `rusting test` in `samples/sky_hop` passes all four scenarios, including
  `clear_level`, which finishes the level with timed inputs only; a capture
  shows the tile map, coin and HUD.
- [x] Gap: character controllers did not ride moving platforms and stuck to
  ceilings while rising. Both now follow the floor body's motion and stop
  rising at a ceiling. Evidence:
  `runtime::tests::controllers_ride_moving_platforms_and_stop_at_ceilings`
  and Sky Hop's `ride_lift`.
- [x] Gap: the editor showed no tile map tiles or background color until
  Play, so Sky Hop's level was blank while editing. Edit mode now runs
  `build_tile_maps` and `apply_scene_background`; a clicked tile selects its
  map, and tiles stay out of the Hierarchy and the saved scene. Evidence:
  `editor::tests::edit_mode_builds_tile_maps_without_listing_or_saving_tiles`.
- [x] Gap: tile maps could only be edited as text rows. The Inspector's
  Tile Painter paints and erases cells from the Scene View, one Undo step
  per stroke. Evidence:
  `editor::picking::tests::scene_view_points_paint_the_tile_map_cell_under_them`
  and `runtime::tests::tile_map_cells_are_found_by_position_and_painted_with_padding`.
- [x] Tile Painter tools, as in Godot: Rectangle fills the cells between
  the pressed and released cells, Fill flood fills the joined cells that
  match the clicked one, and the cell under the pointer (or the dragged
  rectangle) is outlined in the Scene View. Each is one Undo step.
  Evidence: `runtime::tests::tile_map_rectangles_and_flood_fills_paint_regions`,
  `editor::overlay::tests::tile_rect_lines_outline_the_cells_between_two_corners`
  and `editor::picking::tests::scene_view_points_paint_the_tile_map_cell_under_them`.
  The Scene View input path has no automated test; try it by hand.
- [x] Tile Painter Line tool paints Bresenham's line from the pressed cell
  to the released one, with each cell outlined while dragging, and Fill
  no longer grows the grid when clicked outside the used area. Evidence:
  `runtime::tests::tile_map_rectangles_and_flood_fills_paint_regions`
  (line cells, `fill_line`, fill outside the grid). The Scene View input
  path has no automated test; try it by hand.
- [x] Tile Painter palette: the brush is a row of swatches, one per tile in
  its color with its character on top, then Erase and Off, as in Godot's
  TileSet panel. Evidence:
  `editor::inspector::tests::the_tile_palette_shows_each_tile_color_and_picks_a_brush`.
- [x] Tile Painter tool shortcuts, Godot's keys: D Paint, R Rectangle, L
  Line, B Fill. They apply only while a brush is active on a selected tile
  map in Edit mode, where they win over the Scene View keys (R rotates
  otherwise), and they are rebindable in their own Keyboard Shortcuts
  section. Evidence:
  `editor::shortcuts::tests::tile_tool_keys_apply_only_while_a_brush_paints_a_tile_map`.
- [x] Game 4, Ember Arena (`samples/ember_arena`), a third-person arena
  survival game lit by fog, bloom, ambient occlusion and point lights.
  Evidence: `rusting test` in `samples/ember_arena` passes all three
  scenarios (`ember_burns`, `pulse_quenches`, `game_over`); captures in
  `tests/shots/` show the fogged arena, gate glow and ember lights.
- [x] Gap: game code could not spawn copies of a scene object, so enemies
  had to be pre-placed. `GameScene::spawn_copy` copies a template object and
  its children (children are named `"<copy>/<child>"` so names stay unique),
  and `GameScene::in_class` lists the objects in a class. Evidence:
  `project_runner::tests::spawn_copy_clones_the_template_tree_and_in_class_lists_it`.
- [x] Gap (Godot `remove_from_group`): game code could put an object in a
  class with `GameScene::add_class` but not take it out, so a hidden or
  pooled object stayed in every loop over its class. Acceptance: removing
  a copy from its class drops it from `in_class`, a second removal or a
  classless object reports `false`, and the object can rejoin.
  `GameScene::remove_class` does this. Evidence:
  `project_runner::tests::spawn_copy_clones_the_template_tree_and_in_class_lists_it`
  (2026-10-09).
- [x] Gap (Godot `tween_property`): a tween could only be authored in the
  scene, so game code could not slide a door open or fly a coin to the
  HUD without writing the motion per frame. Acceptance: one call starts a
  one-shot tween of position, rotation or scale from the object's current
  value, a second call restarts from wherever it is, and a missing object
  reports `false`. `GameScene::tween(name, property, to, seconds, easing)`
  does this; `TweenProperty` and `Easing` are in the prelude. Evidence:
  `project_runner::tests::tween_starts_from_the_current_value_and_restarts_on_a_new_call`
  (2026-10-09).
- [x] Gap (Godot `look_at`): turning a turret, enemy or camera toward a
  point took hand-written `atan2` with the engine's -Z forward and Euler
  order. Acceptance: after the call the object's forward axis, read from
  its transform matrix, points at the target for targets level, above and
  below it, and a target on the object leaves it unchanged.
  `GameObject::look_at(target)` does this. Evidence:
  `project_runner::tests::look_at_points_the_forward_axis_at_the_target`
  (2026-10-09).
- [x] Gap: a patch that created a registered component with some fields
  missing failed with "missing field". Registered components now take
  defaults like built-in sections. Evidence:
  `scene_patch::tests::created_sections_take_defaults_for_missing_fields`.
- [x] Gap: lights on hidden objects still lit the scene, so a hidden
  template's glow showed. Light extraction skips objects hidden by
  themselves or a parent, as meshes already did. Evidence:
  `runtime::render_world::tests::lights_under_hidden_objects_are_not_extracted`.
- [x] Game 5, Tower Topple (`samples/tower_topple`), a first-person game:
  thrown balls knock two towers and a pyramid off their stands, with ten
  balls per round. Evidence: `rusting test` in `samples/tower_topple`
  passes all three scenarios (`topple_pyramid`, `win_and_restart`,
  `out_of_balls`); `tests/shots/` shows the throw and the win text.
- [x] Gap: game code could not launch a projectile: there was no camera
  ray to aim along, a template copy could not switch from kinematic to
  dynamic, and a velocity set on a sleeping body was lost.
  `GameScene::camera_ray`, `GameScene::set_body_kind`, and
  `set_linear_velocity` waking the body cover it. Evidence:
  `project_runner::tests::a_kinematic_template_copy_launches_along_the_camera_ray`.
- [x] Gap: a round could not be restarted without resetting every object
  by hand. `GameScene::restart` reloads the starting scene and reruns
  `once` setup. Evidence:
  `project_runner::tests::restart_puts_the_starting_scene_back_and_reruns_setup`.
- [x] Gap: hidden HUD elements were still drawn, so a win message could
  not be hidden by game code. Evidence:
  `runtime::tests::hidden_hud_elements_are_not_drawn`.
- [x] Gap: a thrown ball passed through the pyramid. The continuous
  collision sweep cast only the ball's center, which slipped through the
  seam between two blocks, and it stopped the ball dead on a dynamic
  target instead of pushing it. The sweep now casts against colliders
  grown by the ball's radius and leaves the momentum to the contact solve.
  Evidence:
  `runtime::tests::a_fast_ball_between_two_dynamic_boxes_pushes_both_back`
  (fails on the old sweep) and `fast_cpu_bodies_do_not_tunnel_through_thin_walls`.
- [x] Gap: HUD text wrapped when it grew ("14 / 14" broke onto two lines).
  HUD labels now break only at `\n`. Evidence: `tests/shots/win.png` in
  `samples/tower_topple`.
- [x] Game 6, Crate Keeper (`samples/crate_keeper`), a top-down
  crate-pushing puzzle on a tile map grid. Evidence: `rusting test` in
  `samples/crate_keeper` passes all three scenarios (`solve`, `blocked`,
  `restart`); `tests/shots/solved.png` shows every crate on a gold square
  and the win text.
- [x] Gap: game code could not read the tile map, so grid games had to
  duplicate the level layout in code. `GameScene::tile` reads and
  `GameScene::set_tile` writes the cell under a world position. Evidence:
  `project_runner::tests::game_code_reads_and_writes_tile_map_cells_by_world_position`.
- [x] Gap: scenario `tolerance` applied only to single numbers, so a
  position check failed on `0.10000000149` against `0.1`. It now applies to
  every number in an array or object. Evidence:
  `scenario::tests::tolerance_applies_to_every_number_in_a_vector_or_object`.
- [x] Game 7, Brick Bounce (`samples/brick_bounce`), a 2D brick breaker
  with a dynamic ball bouncing off fixed walls and bricks and a kinematic
  paddle. Evidence: `rusting test` in `samples/brick_bounce` passes all
  four scenarios (`first_hit`, `lose_ball`, `paddle_limits`,
  `win_and_restart`); `tests/shots/playing.png` shows a broken brick and
  the returning ball. The ball's z stays 0 through bounces.
- [x] Gap: game code could set a body's velocity but not read it, so
  constant-speed balls needed `world()` queries. `GameScene::linear_velocity`
  reads it. Evidence:
  `project_runner::tests::a_kinematic_template_copy_launches_along_the_camera_ray`.
- [x] Game 8, Core Defense (`samples/core_defense`), a top-down turret
  shooter aimed with the mouse cursor. Evidence: `rusting test` in
  `samples/core_defense` passes all four scenarios (`aimed_shot`,
  `missed_shot`, `core_falls`, `defended`); `tests/shots/firing.png` shows a
  bolt flying at the first drone.
- [x] Gap: game code had no ray through the mouse cursor, and scenarios
  could not move the cursor, so mouse aiming could not be written or tested.
  `GameScene::pointer_ray` returns it, and the `pointer` scenario step places
  the cursor at a fraction of the view. Evidence:
  `project_runner::tests::pointer_ray_goes_through_the_cursor`,
  `scenario::tests::pointer_steps_place_the_cursor_in_the_capture_sized_view`.
- [x] Gap: `GameObject` could set rotation and scale but not read them back.
  `GameObject::rotation` and `GameObject::scale` read them. Evidence: Core
  Defense bolts fly along their stored yaw.
- [x] Game 9, Putt Course (`samples/putt_course`), a mini golf hole with a
  dynamic ball rolling on physics, a charged putt aimed with the mouse, a
  kinematic sweeper and a sensor cup. Evidence: `rusting test` in
  `samples/putt_course` passes all four scenarios (`first_putt`,
  `overcharge`, `sink`, `too_fast`); `tests/shots/aiming.png` shows the aim
  arrow at half power and `tests/shots/sunk.png` the win text after one
  stroke.
- [x] Gap: six of eight games defined their own `value`, `add` and
  `complete` counter helpers, and a helper that borrows the scene cannot be
  nested in another scene call. `GameScene::counter_value`,
  `add_to_counter` and `counter_complete` do it in one call. Evidence:
  `project_runner::tests::game_code_reads_and_sets_counters_by_name`.
- [x] Gap: game code had no short way to draw seeded random numbers; it
  had to read `RandomSeed` and `FrameTime` through `world()`, and earlier
  samples used fixed lists instead. `GameScene::random(stream)` draws from
  the seed, the fixed tick and the stream. Evidence:
  `project_runner::tests::game_randomness_repeats_for_a_seed_and_varies_by_tick_and_stream`.
- [x] Game 10, Lantern Grid (`samples/lantern_grid`), a lights-out puzzle
  played with mouse clicks on a top-down orthographic board. Evidence:
  `rusting test samples/lantern_grid` passes both scenarios (`center`,
  `solve`); `tests/shots/scrambled.png` shows the 11 lit lanterns of the
  fixed scramble and `tests/shots/solved.png` the full board with the win
  text after three moves.
- [x] Gap: game code could not recolor one object. Scene materials are
  shared by value, so editing one through `world()` recolored every object
  with the same color. `GameScene::color`, `set_color` and `set_emissive`
  give the object its own material. Evidence:
  `project_runner::tests::game_code_recolors_one_object_without_touching_shared_materials`.
- [x] Gap: `rusting test samples/game` read the project folder as a
  scenario and failed on `project.json`, and a scenario path was resolved
  from the current folder rather than the project. A lone project argument
  now runs its `tests/` folder, and scenario paths are looked up in the
  project first. Evidence:
  `rusting::tests::project_root_defaults_to_the_current_folder`;
  `rusting test samples/lantern_grid` from the repository root.
- [x] Game 11, Snake Trail (`samples/snake_trail`), a snake game on the
  2D template: grid steps on fixed ticks, walls read from a
  `rusting.tile_map`, and a tail grown with `spawn_copy`. Evidence:
  `rusting test samples/snake_trail` passes all three scenarios (`eat`,
  `crash`, `win`); `tests/shots/grown.png` shows the four-cell snake after
  the first apple and `tests/shots/won.png` the win text.
- [x] Gap: game code had no way to set a counter to a value; Putt Course,
  Lantern Grid and Snake Trail each wrote the same borrow-and-assign
  helper. `GameScene::set_counter` does it. Evidence:
  `project_runner::tests::game_code_reads_and_sets_counters_by_name`.
- [x] Game 12, Night Vault (`samples/night_vault`), a third-person stealth
  game at night: guards patrol on fixed-tick paths with spot lights, see
  the player through a view cone and a line-of-sight raycast, and raise an
  alert meter; crates and hedges block their view. Evidence:
  `rusting test samples/night_vault` passes all three scenarios (`spotted`,
  `hidden`, `heist`); `tests/shots/spotted.png` shows the lose text with a
  guard's lamp on the floor and `tests/shots/escaped.png` the win text at
  the vault door.
- [x] Gap: the third-person camera went through walls behind the player
  and showed only the sky. A camera-sized sphere cast from the orbit
  center now stops the camera in front of the first solid collider,
  ignoring the player's own body. Evidence:
  `runtime::tests::third_person_camera_stops_in_front_of_a_wall_behind_the_body`;
  the Night Vault captures with the player backed against a hedge.
- [x] Gap: game code could only restart the scene it started with, so a
  game had no levels, menus or separate end screens.
  `GameScene::load_scene(path)` replaces the scene with another project
  scene file, which `restart` then returns to. Evidence:
  `project_runner::tests::load_scene_switches_to_another_project_scene`
  (a missing file leaves the scene as it was; `once` runs again; restart
  returns to the loaded level). `rusting export` still ships only the main
  scene.
- [x] `skills/rusting-game/SKILL.md`: a skill file for LLM agents that
  build games on the engine, with the CLI loop, scene and patch format,
  the `GameScene` API, determinism rules for game code, the scenario format
  and its pitfalls, and a done checklist, taken from the twelve sample
  games.
- [ ] Next: the LLM-native track. Milestone L1 diagnostic codes, `rusting explain`, diagnostic locations and `rusting fix` are done; every L1 item is done (its exit gate needs a fresh-agent trial, which is not verifiable here); continue with L2, then L3 annotated captures and the event trace, since those remove the most guesswork seen in the twelve samples.
- [x] Gap: `rusting export` should copy every scene a game loads, not only
  the main one. Export now copies the project's `scenes/` folder; checked in `export_package_contains_executable_scene_assets_and_readme`.
- [x] Gap: a misspelled field in a partial scene component is dropped and
  takes its default without a diagnostic (Night Vault's moon used
  `intensity` instead of `illuminance` and lit the night at full sun).
  `rusting scene patch` and `rusting check` should report unknown fields.
  Done for `scene patch`: unit tests `a_misspelled_field_in_a_section_is_an_error` and `a_full_material_and_light_pass_the_field_check`; full check green. `rusting check` reads scenes strictly through serde already.
- [x] Gap (Same Shift): game projects built the engine unoptimized, so a
  busy CPU physics scene took 20 to 35 ms per tick and the fixed-step
  catch-up spiraled to near 0 FPS. The `rusting new` template and every
  sample `Cargo.toml` now build dependencies with `opt-level = 3`. When a
  frame hits `max_fixed_steps` and its steps take longer than the time they
  simulate, the next frames run one step each (slow motion instead of a
  freeze) until steps are fast again, with one warning that names the
  likely cause. The clamp happens before the replay recorder, so replays
  step the same way. `rusting run --ticks N --json` reports
  `timings.headless_ms_per_tick`. Evidence:
  `runtime::tests::fixed_steps_slower_than_real_time_fall_back_to_slow_motion`.
- [x] Gap (Same Shift): `restart` and `load_scene` did not repeat physics,
  because the CPU solver ordered bodies by `Entity`, and a reload hands out
  entities in a different order. Bodies now sort by `SpawnOrder`, the
  object's rank by scene ID within its document; runtime spawns take the
  next number. Joints and articulations index bodies by map and sort by
  body order. Evidence:
  `project_runner::tests::restart_repeats_a_physics_pile_exactly` (a
  12-box pile saved in reverse ID order matches after `restart` with
  tolerance 0; fails with the old entity sort).
- [x] Gap (Same Shift): game code had no snapshot, body reset, starting
  state, look direction or class hash. Added `GameScene::snapshot`,
  `restore` (keeps velocities, sleep and solver caches, renamed to the new
  entities), `reset_body`, `set_angular_velocity`, `angular_velocity`,
  `initial`, `set_look` and `state_hash(class)`; `set_body_kind` to
  `Kinematic` or `Fixed` now zeroes velocities. The prelude exports
  `Entity`, `World`, `Name`, `RigidBody`, `PlayerController` and
  `PhysicsWorld`. Evidence:
  `project_runner::tests::body_calls_stop_spin_and_reset_bodies_and_set_look_turns_the_player`
  and `restart_repeats_a_physics_pile_exactly` (restored mid-fall run
  matches the run that went on; fails without the solver-state rename).
- [x] Gap (Same Shift): sleeping bodies floated when their support was
  removed or moved. Sleepers near a body that gameplay removed, moved or
  changed wake on the next step. Evidence:
  `runtime::tests::sleeping_cpu_bodies_fall_when_their_support_is_removed_or_moved`.
- [x] Gap (Same Shift): scenarios had no way to record a value. A `log`
  step records `entity.path` (every tick with `until`) and never fails;
  `rusting test --json` lists the lines. Evidence:
  `scenario::tests::log_steps_record_a_value_every_tick_and_never_fail`.
- [x] Gap (Same Shift): GPU bodies reach game code 1 to 3 frames late and
  cannot be reset or teleported after setup. Document their feature set
  and let game code move one to a pose with zero velocity. Closed by
  existing work: `GameScene::gpu_command` with `GpuBodyCommand::Teleport`
  and `SetVelocity` (v1.3.0), applied before the next GPU tick; the GPU
  test `commands_apply_once_before_the_step_and_reject_stale_bodies`
  covers both. The lateness and the reset recipe are in
  `docs/pitfalls.md` and `docs/determinism.md` (2026-10-09 audit).
- [x] Gap (Same Shift): a camera that renders into a texture or a screen
  rectangle, for monitors and split screen.
  Done 2026-10-10 (bookkeeping): `rusting.camera_screen` renders a camera into a texture (split-signal F7, 4688062), and `Camera.viewport` draws one into a screen rectangle for split screen.
- [x] Gap: `RenderSettings::render_scale` is read by the window and headless
  capture: the scene renders into an offscreen image of the scaled size and a
  linear blit stretches it over the target (`rendering::render_scale`). See
  Night Market F10.
- [x] Gap: `rusting capture` does not run game code. `--game` builds the
  project the scene path is in and captures its game, code included, at
  `--tick` through a one-step scenario (`build/capture-game.json`);
  `--camera`, `--size` and `--no-hud` apply. Evidence: on the reference
  `reaction-new-game` project, `capture --game --tick 150` shows the `Go!`
  that game code sets and the plain capture shows the scene's
  `Wait for it...` (RTX 3060, 2026-10-09); unit test
  `capture_game_needs_a_path_inside_a_project`.
- [x] Gap: a scenario stops at its first failed check. `"keep_going": true` in the scenario file reports every failure; test `keep_going_reports_every_failed_check`.
- [x] Gap: a windowed `rusting run` shows no on-screen FPS or frame time.
  `RUSTING_PERF=1` prints fps, update/render split, CPU phases, GPU pass
  times and draw counts to stderr once a second and shows fps and frame
  time in the window title. Now it also draws fps, frame time and p95 in
  the top-right corner (the `PerfOverlay` resource, painted by the HUD
  pass), and F3 toggles the report in a running game. Evidence: unit test
  `the_perf_overlay_draws_only_while_its_resource_exists`; an 8000-frame
  `RUSTING_PERF=1` window run of the empty template (RTX 3060, 2026-10-09)
  printed three perf lines at about 2400 fps with no error. The corner
  text and the F3 key were not checked by eye. Full AGENTS.md check clean.
- [x] Perf (Same Shift, RTX 3060, 1080p, clean GPU): 355 to about 1000 fps.
  Swapchain image count 2 to 4 (Wayland throttled Immediate with 2), a
  `reflections` render setting that skips the scene copy, mip chain and depth
  pyramid, and batching by mesh and texture group (97 to 18 draws, 0.56 to
  0.22 ms recording). Evidence: `RUSTING_PERF=1` lines in CHANGELOG, test
  `materials_that_differ_only_in_color_share_a_batch`,
  `the_reflections_setting_reaches_the_render_world`; full check green with
  and without `--features gpu-tests`. Note: a second game instance on the
  GPU halves every number; close it before measuring.
- [x] Gap (Same Shift): a scenario `expect` with tolerance 0 failed on
  every printed spelling of an f32 value, because serde_json parses decimal
  text only to within an f64 ulp. A value that is an exact f32 now also
  matches any number that rounds to the same f32. Evidence:
  `scenario::tests::a_printed_f32_matches_itself_with_tolerance_zero`.
- [x] Gap (Same Shift): cube side faces showed images upside down. Cube
  UVs follow glTF: v = 0 is the image's first row, at the top of each side
  face and the -Z edge of the top and bottom faces (spheres already did).
  Evidence:
  `assets::tests::cube_side_faces_show_the_first_image_row_at_the_top`.
- [x] Gap (Same Shift): textures could not repeat. Materials have
  `uv_scale` and `uv_offset`, saved in scenes (default `[1, 1]` and
  `[0, 0]`), reflected for the Inspector and applied in the vertex shader.
  Evidence: `rendering::scene_renderer::tests::render_instance_layout_matches_shader_struct`
  checks the new `uv_transform` field against the shader struct.
- [x] Gap (Same Shift): `rusting schema` called scene texture slots a
  "project path"; they are plain strings relative to the scene file, not
  `{"$asset": ...}`. The `mesh_renderer` entry now says so and lists
  `uv_scale` and `uv_offset`.
- [x] Gap (Same Shift): the player controller climbed boxes on the round
  bottom of its capsule and could not be kept from pushing dynamic bodies.
  `PlayerController` has `max_slope` (default 45°), `max_step_height`
  (default 0.3 m) and `push_bodies` (default true), and
  `PhysicsWorld::move_character_on_foot` walks with the first two. With
  `push_bodies` off the solver drops the player's contacts with dynamic
  bodies, which still block its movement. Evidence:
  `runtime::tests::players_step_onto_low_ledges_but_not_high_ones_and_can_leave_crates_alone`.


Goal: establish a trustworthy baseline before adding architecture or features.

### Completed

- [x] Make `MaterialBuilder::default()` agree with `Material::default()`.
- [x] Copy all material properties when creating cube and sphere instances.
- [x] Cache sphere meshes by subdivision level.
- [x] Replace mutable batch/index instance handles with stable IDs.
- [x] Align all physics compute-shader instance fields with the Rust `InstanceData` layout.
- [x] Align physics push constants and add explicit padding.
- [x] Repair the `NoCollision` shader's mass, gravity, friction, restitution, and angular-velocity interpretation.
- [x] Make broad-phase sphere and box radius conventions agree with collision shaders.
- [x] Add compile-time CPU structure size/offset assertions.
- [x] Add tests that enforce common layouts across every compute shader.
- [x] Make `cargo fmt --check`, all-target tests, strict clippy, and all-target checking pass.
- [x] Upgrade Vulkano 0.33 to 0.35 and migrate pipeline, descriptor, image, allocation, and command APIs.
- [x] Remove `vulkano-win` and migrate event dispatch to winit 0.30's `ApplicationHandler` lifecycle.
- [x] Negotiate surface format, color space, image count, composite alpha, and present mode.
- [x] Enable the Khronos validation layer automatically in debug builds when it is installed.
- [x] Enable synchronization validation and opt-in GPU-assisted validation with `RUSTING_GPU_VALIDATION=1`.

### Remaining

- [x] Move compatibility-facade window and surface creation into `ApplicationHandler::resumed`, removing the last deprecated winit call. Evidence: `Engine` now builds an ECS `App` and `run()` hands it to `project_runner::run_windowed`, whose `WindowRunner` creates the window in `resumed()`. `rendering::init_vulkan` (the eager window and the last `#[allow(deprecated)] create_window`) is deleted; its validation-layer setup moved to `rendering::vulkano_config`, and `RUSTING_VULKAN_DEVICE` now applies to every window. Verified: `RUSTING_VULKAN_DEVICE=llvmpipe ./target/debug/user_main` prints `Using device llvmpipe ...`; an unknown selector panics with `matches no available Vulkan device; available devices: [NVIDIA GeForce RTX 3060, llvmpipe ...]`; the 8-second `user_main` and `editor` runs print no validation warnings or errors. Limit: the main queue no longer gets a debug name on windowed runs because `vulkano_util` creates the queues.
- [x] Replace public initialization and asset-loading panics with typed errors and `Result` APIs.
  - [x] `geometry::gltf_loader::load_gltf_scene` returns `Result<_, GltfLoadError>` and `Engine::add_gltf` returns `Result<(), AssetError>` instead of panicking on a missing/corrupt glTF file or a primitive without positions.
  - [x] `Engine::load_texture` returns `Result<usize, AssetError>` instead of panicking on a missing/corrupt image file.
  - [x] Remaining production `.unwrap()`/`.expect()`/`panic!` call sites outside tests audited (~220 across `scene/mod.rs`, `engine/mod.rs`, `rendering/*`, `project_runner.rs`). Findings: no genuine unconverted caller-recoverable site remains (checked for file/filesystem loading specifically — the only production `fs::`/`File` usage is a compiled-in shader module load and this session's own golden-image test helper). The rest fall into three buckets that are correctly left as panics, not oversights: (1) internal invariants after initialization — ECS resource/component lookups, GPU pipeline/queue state assumed valid once the device exists; (2) documented API panics that already ship a non-panicking alternative, e.g. `GameScene::object` panics but `GameScene::try_object` returns `Option`; (3) the legacy `Engine`/`scene::RenderScene` compatibility facade (`engine/mod.rs`, `scene/mod.rs`), already deferred to the Milestone 1 facade replacement per the compatibility-facade item above — converting its error style now would be churn thrown away at that rewrite. No new unchecked item added since nothing actionable was found outside what's already tracked.
- [x] Add Vulkan debug names and scoped command-buffer labels. `init_vulkan` named the main queue via `Device::set_debug_utils_object_name` when `ext_debug_utils` was enabled (removed with `init_vulkan` in Milestone 0's resumed-window item); `SceneRenderer::render` wraps its command buffer in a `begin_debug_utils_label`/`end_debug_utils_label` scope ("SceneRenderer::render"), both gated on `instance().enabled_extensions().ext_debug_utils` so release/no-validation builds pay nothing. Verified live on real hardware (NVIDIA RTX 3060): `rendering::debug_utils_tests::object_names_and_command_buffer_labels_are_accepted_by_the_driver` creates an instance with `ext_debug_utils` forced on, names a queue, and records/submits a command buffer with a begin/end label pair, asserting the driver accepts both calls with no validation error. `cargo test --lib --features gpu-tests`: 146 passed.
- [x] Validate rendering, resizing, minimizing, restoring, and shutdown on Linux and Windows. Partially verified live in this environment (Linux, Wayland session, real NVIDIA RTX 3060): `./target/debug/game testGame/build/main.rscene.bin` opens a window and renders continuously for 5s under `timeout` with no panic/validation output, then exits cleanly (SIGTERM). Resizing, minimizing, and restoring need a window-manager automation tool (`xdotool`/`wmctrl`, neither installed, not added since installing new system packages is outside this session's scope) to script without a human; Windows has no machine available in this environment at all. See new item below for the remaining manual QA pass.
- [ ] Manually verify window resize, minimize, and restore on Linux (with `xdotool`/`wmctrl` installed, or by hand) and repeat the full rendering/resize/minimize/restore/shutdown pass on a Windows machine — blocked in this environment per item above.
- [x] Replace fixed grid limits with explicit capacity tracking and overflow reporting. Never silently omit bodies. Evidence: the rebuilt `Engine` maps `GridCollision` and `FullPhysics` to `PhysicsSolver::Full`, so every runner now uses the ECS contact grid. That grid grows with the body count, reports overflow in `RenderCapacityDiagnostics`, and keeps every body through its fallback list (Milestone 5 overflow items; `gpu_bodies_collide_with_each_other_through_grid_and_fallback`). The old `grid_build.comp` with `MAX_PER_CELL = 128` is still compiled, but only the legacy `RenderScene` code and its tests use it; that code is deleted by the Milestone 1 cleanup item.
- [x] Add generated/reflected layout checks for storage buffers, uniforms, vertex data, and push constants. `render_instance_layout_matches_shader_struct`, `light_gpu_layouts_match_shader_structs`, and `hybrid_physics_gpu_layouts_match_shader_structs` (`rendering/scene_renderer.rs`) now compare CPU upload structs' `size_of`/`offset_of` against the `vulkano_shaders`-generated types for the same GLSL struct/block (`vertex_shader::RenderInstance`/`Camera`, `fragment_shader::Light`, `physics_shader::PhysicsState`/`ConditionInstruction`/`RuleState`/`PhysicsEvent`/`PhysicsPush`) — reflected straight from the compiled SPIR-V — instead of hardcoded magic-number offsets that could silently drift from the shader source. `GpuEventHeader` has no named GLSL struct (bare buffer block) so it keeps its one hand-computed size assertion. Verified: `cargo test --lib --features gpu-tests`: 146 passed.

### Exit gate

- Debug validation produces no errors during startup, rendering, resize, minimize/restore, and shutdown.
- Linux and Windows smoke tests render at least 1,000 frames.
- No known CPU/GPU layout mismatch remains.
- Formatting, strict clippy, tests, shader compilation, and all-target checking pass in CI.

## Milestone 0.5: Verifiable development environment

Goal: make GPU-touching work provable on a machine with no display and no second GPU. This milestone precedes all remaining GPU work. Without it, most of Milestones 3, 4, 5, 8, 11, 13, 25, and 26 cannot be verified by anyone, including CI.

### Software Vulkan device

- [x] Document Mesa lavapipe as the supported software Vulkan implementation. On Arch the package is `vulkan-swrast`; on Debian/Ubuntu it is `mesa-vulkan-drivers`. See `docs/dev-environment.md`.
- [x] Document the invocation. Correction: on this machine (Arch, current `vulkan-swrast`) the ICD file is `/usr/share/vulkan/icd.d/lvp_icd.json`, not `lvp_icd.x86_64.json` — the exact name is distro/version-dependent, see `docs/dev-environment.md`.
- [x] Verify `vulkaninfo --summary` reports a second device with `vendorID = 0x10005` when the variable is set. Confirmed on this machine.
- [x] Record required Vulkan features and extensions, and which of them lavapipe does not provide. `init_vulkan` requests only the `khr_swapchain` device extension and no explicit device features; lavapipe supports `khr_swapchain` via the standard surface path, so it provides everything the engine currently requests. See `docs/dev-environment.md`.

### Device selection

- [x] Add `RUSTING_VULKAN_DEVICE` to select a physical device by index or by name substring. `rendering::select_device_index` (pure, unit-tested) plus wiring in `init_vulkan_headless` and `vulkano_config` (every window).
- [x] Log the selected device name, vendor ID, driver version, and API version at startup. Verified live: `Selected Vulkan device: llvmpipe (LLVM 22.1.8, 256 bits) (vendor 0x10005, driver 109060098, api 1.4.354)` with `RUSTING_VULKAN_DEVICE=llvmpipe` and lavapipe's ICD.
- [x] Fail with a typed error naming every available device when the selection matches nothing. Never fall back silently. `DeviceSelectionError` lists every candidate device name; `vulkano_config` and `init_vulkan_headless` panic with its `Display` message, because `VulkanoContext` itself panics on a failed device pick.

### Headless mode

- [x] Create instance, physical device, and logical device without a surface, a swapchain, or a winit window. `rendering::init_vulkan_headless`/`HeadlessVulkanBase`, sharing device-selection logic with the windowed path via `select_physical_device`. Verified live (no window created): `Selected headless Vulkan device: NVIDIA GeForce RTX 3060 (vendor 0x10de, driver 2580660800, api 1.4.351)`.
- [x] Add offscreen render targets with the same formats used by the windowed path. `swapchain::create_offscreen_target`/`OffscreenTarget` (color `B8G8R8A8_SRGB` + `TRANSFER_SRC` for later readback, depth `D16_UNORM`), sharing `create_render_pass_for_format` with the windowed `create_render_pass`. Verified live against `init_vulkan_headless` on real hardware, no window created.
- [x] Add a fenced image readback returning CPU pixel data. `readback::read_back_image` (copy-to-buffer command, `then_signal_fence_and_flush`, `wait(None)`, host-visible buffer read). Verified live against `init_vulkan_headless`/`create_offscreen_target` on real hardware.
- [x] Add a fenced storage-buffer readback returning CPU data after a compute dispatch. `readback::read_back_buffer<T>` (copy-buffer command, same fence pattern as `read_back_image`). Verified live: `reads_back_storage_buffer_results_after_a_compute_dispatch` builds a `ComputePipeline` from `src/shaders/compute/test_double.comp`, dispatches against a 64-element storage buffer on real hardware (`NVIDIA GeForce RTX 3060`), and asserts every value was doubled.
- [x] Add `--headless` to the game binary, running a fixed number of ticks and exiting with a status code. `project_runner::run_project_headless` builds the same `App`/plugin stack as `run_project` (`AssetPlugin`, `HybridPhysicsPlugin`, `RenderExtractPlugin`, game plugin) with no `VulkanoContext`, window, or renderer, and runs 60 fixed-step ticks; `src/bin/game.rs` wires `--headless` to it. Verified live: `./target/debug/game --headless testGame/build/main.rscene.bin` opens no window and exits 0.

### Test harness

- [x] Add a `gpu-tests` feature. GPU tests compile always and run only under that feature. `gpu-tests = []` in `Cargo.toml`; every GPU test carries `#[cfg_attr(not(feature = "gpu-tests"), ignore = "...")]`. Verified live: `cargo test --lib` shows `4 ignored`; `cargo test --lib --features gpu-tests` runs all 142 with 0 ignored, both clean under `cargo clippy --workspace --all-targets [--features gpu-tests] -- -D warnings`.
- [x] Add a shared test fixture that creates a headless device once per test binary. `rendering::test_support::headless_device()` (`OnceLock<HeadlessVulkanBase>`), used by `swapchain::tests` and both `readback::tests`. Verified live: `cargo test --lib --features gpu-tests -- --nocapture` prints "Selected headless Vulkan device" only twice for 4 GPU tests (once for the fixture's one-time init, once for the unrelated `headless_device_tests` test that exercises `init_vulkan_headless` directly).
- [x] Skip with an explicit message, not a failure, when no Vulkan device is present. Every GPU test still opens with `if VulkanLibrary::new().is_err() { eprintln!("skipping: no Vulkan driver present"); return; }` in addition to the `gpu-tests` ignore gate, so a `--features gpu-tests` run on a driverless machine reports pass, not failure.
- [x] Add golden-image comparison with a per-pixel tolerance, writing actual/expected/difference images to an artifact directory on mismatch. `rendering::test_support::assert_matches_golden_image(actual, golden_path, artifact_dir, tolerance)` (per-channel `abs_diff`, dimension check first, writes `actual.png`/`expected.png`/`diff.png` via the `image` crate on any mismatch, no GPU required). Verified live: `identical_image_matches_its_own_golden_with_zero_tolerance`, `a_small_difference_within_tolerance_passes`, and `a_mismatch_beyond_tolerance_panics_and_writes_artifact_images` (which asserts the three artifact files exist after a caught panic) all pass under `cargo test --lib`.
- [x] Add a compute-dispatch fixture: upload known input, dispatch, read back, assert. `rendering::test_support::dispatch_and_read_back` (generic over the storage-buffer element type; builds the pipeline/layout/descriptor set, dispatches, reads back via `readback::read_back_buffer`). `readback::tests::reads_back_storage_buffer_results_after_a_compute_dispatch` now calls it instead of repeating the boilerplate inline. Verified live against real hardware: same doubling assertion still passes.
- [x] Mark every test whose result depends on real hardware timing `#[ignore]`, with the reason in the attribute. Audited: no test in the crate asserts on wall-clock elapsed time, FPS, or frame pacing — `Duration` values in `runtime::tests` and `project_runner` tests are deterministic simulated inputs (fixed `Duration::from_millis(...)` passed to `App::update`), not measured real time, so nothing currently qualifies. Note for future work: apply this to any new test that measures real elapsed time, frame pacing, or throughput.

### Repository constraints

- [x] Add `AGENTS.md` recording files that implementation sessions must not modify, and why.
- [x] Record the verification tier table from the working agreement above, or link to it.

### Exit gate

- A headless compute test and a headless offscreen render test both pass under lavapipe on a machine with no display.
- The same two tests pass on the NVIDIA device.
- CI runs the `gpu-tests` feature under lavapipe on every pull request.
- `RUSTING_VULKAN_DEVICE` selects between lavapipe and hardware on a machine where both are present.

## Milestone 1: Workspace and runtime foundation

Goal: create the engine runtime that owns canonical scene state and system execution.

### Workspace

- [x] Convert the package into a Cargo workspace using the architectural dependency direction above. Verified: root `Cargo.toml` already had `[workspace] members = ["testGame"]`; added `crates/rusting-core` as the first real architectural-layer member (bottom of the dependency direction). `cargo build --workspace` and `cargo clippy --workspace --all-targets -- -D warnings` clean.
- [x] Move reusable public data types into `rusting-core` without Vulkan dependencies. First increment: moved `Transform` and `CollisionType` into `crates/rusting-core`. Second increment: moved the Vulkan-independent canonical scene, hierarchy, camera, naming/classification, light, visibility, render-settings, physics-settings/status, and rigid-body/collider data from `src/runtime/components.rs` into `crates/rusting-core/src/components.rs`; `rusting_engine::runtime::*` remains compatible through re-exports. Third increment: moved the generic `EventQueue<T>` and its frame-buffer semantics into `crates/rusting-core/src/events.rs`; the engine retains only the `World` adapter and its public runtime path remains an exact re-export of the core type. Fourth increment: moved the Vulkan-independent `FrameTime` resource and its duration accessors into `crates/rusting-core/src/time.rs`; the engine retains time control and advancement policy while its public runtime path remains an exact re-export of the core type. Fifth increment: moved hierarchy mutation, transform propagation, malformed-hierarchy diagnostics, and the typed `HierarchyError` into `crates/rusting-core/src/hierarchy.rs`; the engine adapter preserves the existing `AppError` variants and public diagnostics/propagation paths. Sixth increment: moved the public `ScheduleStage` and `FrameReport` scheduler data types into `crates/rusting-core/src/schedule.rs`; schedule storage and execution remain engine-local while the existing runtime paths are exact re-exports. Seventh increment: moved `TimeControl`, its private accumulator/queued-step state, the deterministic advancement algorithm, and typed `TimeAdvanceError` into `crates/rusting-core/src/time.rs`; the engine retains only the `World`/`AppError` compatibility adapter and its public time resource paths remain exact re-exports. Eighth increment: moved `RuntimeInput`, `ActionMap`, `InputBinding`, and the `KeyCode`/`MouseButton` re-exports into `crates/rusting-core/src/input.rs` (adds a `winit` dependency to core for the key/button types only; no Vulkan); `src/runtime/input.rs`/`actions.rs` keep only the `World` install adapters and exact re-exports. Built-in `CollisionEvent` remains engine-local with the CPU physics implementation. `MeshRenderer` stays engine-local because it uses engine asset handles. `src/core/material.rs` and `src/core/physics.rs` also remain engine-local because they depend on rendering registries. Audit complete: remaining public runtime types are domain-owned rather than core data: `MeshRenderer` and scene documents depend on assets, physics events and GPU IDs belong to the planned physics crate, render snapshots and picking belong to the planned render crate, and `App`/`Plugin`/`EngineBuilder` remain engine application logic. `cargo tree -p rusting-core` shows no Vulkan, egui, image, or glTF dependency.
  - [x] Ninth increment: moved `CpuFrameTimings` to `rusting-core::schedule` as a Vulkan-independent ECS resource and preserved `rusting_engine::runtime::CpuFrameTimings` as an exact re-export. Evidence: core resource test passes; `cargo fmt --all --check`, strict workspace clippy (default, `--no-default-features`, and `--features gpu-tests`), `cargo test --workspace`, and `cargo test --workspace --features gpu-tests -- --test-threads=1` pass. The parallel GPU suite crashed with SIGSEGV in the test process; the serial run passed all 312 engine library tests.
  - [x] Tenth increment: moved the Vulkan-independent `AppError` enum and its error formatting into `rusting-core::app`, preserving `rusting_engine::runtime::AppError` as an exact re-export. Evidence: `app::tests::errors_preserve_entity_and_plugin_context` passes; formatting, strict workspace clippy in all three configurations, default workspace tests, and serial `gpu-tests` all pass.
  - [x] Eleventh increment: moved the asset-independent `ClickEvent` data type into `rusting-core::input` while keeping gameplay picking and event routing in the engine; `rusting_engine::runtime::ClickEvent` remains an exact re-export. Evidence: `runtime::tests::clicking_a_rendered_cube_fires_a_click_event` passes and exercises event delivery through the old path.
- [x] Keep compatibility re-exports in `rusting-engine` during migration. `src/core/mod.rs` re-exports `rusting_core::{transform, collisions}` at their original `crate::core::transform`/`crate::core::collisions` paths, so every existing caller (`src/tests.rs`, `src/scene/mod.rs`, `src/engine/mod.rs`, `src/core/physics.rs`) compiles unchanged. Confirmed `src/editor/mod.rs`/`src/editor/view.rs` never import `crate::core::` at all, so this migration step could not have touched the protected editor/gizmo files even indirectly; verified via `git status` that both files are untouched.
- [x] Add feature flags for editor, validation, experimental GPU physics, and optional importers. `editor` and `window` already gated their modules. `validation` now force-enables the Khronos validation layer (when installed) in non-debug builds too (now in `rendering::validation_instance_info`, used by `vulkano_config`: `cfg!(debug_assertions) || cfg!(feature = "validation")`). New `gltf` feature (default, implied by `editor` because `src/editor/view.rs` calls `AssetServer::import_gltf`) makes the `gltf` crate optional and gates `geometry::gltf_loader`, `Engine::add_gltf`, `AssetServer::import_gltf`, and the `gltf_test` example. `experimental-gpu-physics` exists but gates nothing yet: all current GPU physics is the shipped hybrid path, so the flag is reserved for Milestone 5 GPU solvers. Verified: strict clippy over `--workspace --all-targets` clean for default, `--no-default-features`, `--no-default-features --features window`, `--no-default-features --features gltf,validation`, and `--features gpu-tests`; `cargo test --workspace` passes with default and `--no-default-features`.

### ECS and application lifecycle

- [x] Add standalone `bevy_ecs` with its parallel scheduler enabled.
- [x] Introduce a Vulkan-independent `App` and fallible `EngineBuilder`.
- [x] Add `add_plugin`, `add_system`, `add_systems`, `insert_resource`, `spawn`, `despawn`, `run`, and controlled shutdown.
- [x] Define ordered schedules: `Startup`, `FixedUpdate`, `Update`, `PostUpdate`, and `RenderExtract`.
- [x] Support command-buffered entity spawning and despawning inside systems through Bevy `Commands`.
- [x] Implement deterministic fixed timestep with configurable maximum catch-up limits.
- [x] Add pause, single-step, time scale, elapsed time, frame delta, and fixed delta resources.
- [x] Add typed events with frame-bounded lifetime.

### Canonical components and resources

- [x] `Transform` and `GlobalTransform` ECS components.
- [x] Parent/child hierarchy, cycle rejection, diagnostics, and cycle-safe propagation.
- [x] `Camera` and perspective/orthographic projection settings.
- [x] Deterministic active-camera selection by active flag, priority, and stable entity identity.
- [x] `MeshRenderer` and visibility components using typed asset handles.
- [x] Directional, point, and ambient light components.
- [x] `RigidBody`, primitive `Collider`, sensor, and collision-layer components.
- [x] `GpuEffectBody` marker for bodies currently assigned to GPU simulation. Removed in Milestone 5: it only mirrored `PhysicsBody::uses_gpu`, which `SimulationClass::Gpu` now states directly.
- [x] `FrameTime`, `TimeControl`, `RenderSettings`, `PhysicsSettings`, and `QualityProfile` resources.
- [x] Input action mapping with keyboard, mouse, and gamepad-ready abstractions.

### Runtime ownership

- [ ] Split the monolithic loop into `WindowRunner`, `Renderer`, `PhysicsWorld`, `AssetServer`, and optional `EditorState`.
  - [x] Windowed and headless project runners now use one `load_project_runtime` function to install assets, hybrid physics, render extraction, the game plugin, and the cooked scene in the same order. GPU availability remains a windowed runner setting. Evidence: default workspace tests and serial `gpu-tests` pass; the full window/renderer ownership split remains open.
  - [x] Extracted `WindowRunner` from `ProjectApplication` in `src/project_runner.rs`. It owns the Vulkan context, windows, scene renderer, VSync state, resize, frame pacing, swapchain acquisition, rendering, and presentation; `ProjectApplication` keeps the ECS runtime and coordinates input, GPU result delivery, and updates. Evidence: `cargo check --workspace`, `cargo run --bin game -- --headless testGame/build/main.rscene.bin`, formatting, strict workspace clippy (default, no default features, GPU tests), default workspace tests, and serial GPU tests pass. The legacy `Engine` facade and editor window loops still need the same separation.
  - [x] Extracted `EditorWindowRunner` from `EditorApplication` in `src/bin/editor.rs`. It owns the Vulkan context, window/swapchain, scene renderer, offscreen scene target, egui painter state, VSync state, and frame pacer, and initializes them on `resumed`. `EditorApplication` keeps scene ECS, input, and update coordination. Evidence: `cargo check --workspace --bin editor`, formatting, strict workspace clippy in all three configurations, default workspace tests, and serial GPU tests pass. Editor draw/present delegation and a live GUI smoke run remain open in `docs/editor-overhaul.md`.
  - [x] Rebuilt the legacy `Engine` facade over `App` + ECS + the shared `WindowRunner`. `add_cube`/`add_sphere`/`add_gltf` spawn `MeshRenderer`, `PhysicsBody`, `RigidBody`, and `Collider` entities; legacy compute shaders map to `SimulationClass`/`PhysicsSolver`; `set_light` becomes a `DirectionalLight` plus `AmbientLight`; the fly camera is an `Update` system that uses the new `RuntimeInput::mouse_motion` and cursor capture. Breaking changes: `PerspectiveCamera::new` drops the aspect ratio and `update` no longer returns a view matrix; `gltf_cache` holds imported nodes; the C-key culling toggle is gone; `run()` needs the `window` feature. Evidence: `engine::tests` (component mapping, scene overrides, camera and light directions, and the GPU render test below); `user_main` (10k PBR cubes) ran 8 s with no errors. Not smoke-run: `native_texture` and `gltf_test` need the untracked `testModels/` directory, which is missing here.
  - [x] Delete the legacy `scene::RenderScene`, `ShaderRegistry`, `ComputeShaderRegistry` grid path, and `geometry::gltf_loader` now that no runner uses them. Removed the retired scene, registry, pipeline, render, and glTF loader modules and disconnected the legacy shader module. Kept the public `ComputeShaderType` and physics profile mapping in `rendering::compute_profile` for the `Engine` facade, with a `rendering::compute_registry` compatibility re-export that contains no pipeline registry. Replaced `src/tests.rs` with its still relevant material tests; the old registry GPU test and layout checks exercised only the retired path, while `SceneRenderer` has active GPU rendering, physics, and shader layout coverage. Evidence: `cargo fmt --all --check`, strict workspace clippy (default, `--no-default-features`, and `--features gpu-tests`), `cargo test --workspace`, and serial `cargo test --workspace --features gpu-tests -- --test-threads=1` all pass; `rg` finds no Rust references to the removed scene, shader-registry, or glTF loader modules.
  - [x] Moved the editor scene draw, egui paint, and present sequence into `EditorWindowRunner::draw`; `EditorApplication::window_event` now only updates the ECS, builds the GUI, extracts, and delegates drawing. Evidence: formatting, strict workspace clippy in all three configurations, default workspace tests, and serial GPU tests pass; `timeout 8 cargo run --bin editor` on Wayland opened the editor and ran with no error output until killed. Interactive resize and window-close checks remain open in `docs/editor-overhaul.md`.
- [x] Keep the window, ECS world, and rendering submission main-thread owned.
  Evidence: thread audit (2026-09-23) found only `std::thread::yield_now()` in
  `App::run` and editor cargo-build/stdout worker threads in `src/editor/mod.rs`;
  no thread touches the window, `World`, or Vulkan submission.
- [x] Remove unnecessary `Arc<Mutex<_>>` usage from camera and scene state.
  Evidence: legacy `Engine` now owns `camera: PerspectiveCamera` and
  `scene: RenderScene` directly. Same change fixed a pre-existing
  `AccessConflict(DeviceRead)` panic in `prepare_frame_ubo` by keeping one
  fence per frame slot and waiting only before reusing that slot. Clippy/tests
  clean; `user_main` stress_pbr ran 8 s at ~1000-1300 FPS without panic.
- [x] Use worker tasks only for safe asset decoding and preparation.
  Evidence: `Assets::load_async` runs only the CPU loader on a worker; results
  are published on the main thread by `poll_loads` (see Milestone 2).
- [x] Execute animations as ECS systems instead of passive data.
  The unused legacy `SceneObject::animation` field was removed with
  `RenderScene`. Real ECS animation remains part of Milestone 14; it needs
  clip sampling, pose application, and a fixed/update schedule integration.
  Done 2026-10-10 (bookkeeping): clips run as ECS systems, `animation::advance_animations` in the update schedule and `step_animations` (`src/runtime/animation.rs`), with blending, crossfades and root motion (Milestone 14).

### Exit gate

- A headless test app can run startup, fixed, variable, hierarchy, and event systems deterministically.
- Entities remain stable through spawn, despawn, sorting, and render extraction.
- Pause and single-step work without affecting editor interaction.
- The old facade can spawn and render cubes and spheres through ECS components. Met: GPU test `engine::tests::engine_cubes_and_spheres_render_through_ecs` builds an `Engine` with a red cube and a green sphere, runs one `App` update, renders the extracted `RenderWorld` offscreen, and finds both colors (llvmpipe and RTX 3060 via `--features gpu-tests`).

## Milestone 2: Asset system and scene persistence

Goal: make assets stable, deduplicated, reloadable, and serializable.

### Typed assets

- [x] Add generational `Handle<MeshAsset>`, `Handle<TextureAsset>`, `Handle<MaterialAsset>`, and `Handle<SceneAsset>`.
- [x] Build typed asset storage with generation checks, explicit reference tracking, and frame-deferred destruction.
- [x] Canonicalize asset paths and deduplicate repeated loads.
- [x] Separate CPU mesh/material assets from renderer-owned prepared GPU mesh buffers.
- [x] Add synchronous loading state inspection and structured asset errors.
- [x] Add worker-backed asynchronous loading states.
  Evidence: `LoadState::Loading`, `Assets::load_async` (path dedup, panic
  capture, failed-path retry, cancel via `remove`) and `Assets::poll_loads`,
  polled each `Update` by `AssetPlugin`. Unit test
  `async_loads_publish_success_failure_panic_and_discard_cancelled`.
- [x] Add default/fallback mesh, material, and texture assets.

### Import pipeline

- [x] Deduplicate glTF meshes, images, and materials by synthesized asset path, including sampler identity.
- [x] Import base-color textures with sRGB formats.
- [x] Import normal, metallic-roughness, occlusion, and emissive textures with correct linear/sRGB treatment.
- [x] Import sampler filtering and wrapping modes.
  Evidence: `TextureAsset::sampler` (`TextureSampler` with mag/min/mipmap
  `TextureFilter` and U/V `TextureWrap`) is filled from glTF samplers; the
  sampler index is part of the synthesized texture path. Unit test
  `gltf_import_carries_sampler_filtering_and_wrapping`. API note: adds a public
  field to `TextureAsset`. The renderer still uses one shared linear/repeat
  sampler; per-texture samplers land when the ECS renderer uploads
  `TextureAsset`s (GPU tier).
- [x] Generate or import tangents.
  Evidence: glTF `TANGENT` is imported when present; otherwise
  `assets::generate_tangents` derives tangents and handedness from triangle UV
  gradients, with a perpendicular fallback for degenerate UVs. Unit test
  `generated_tangents_follow_uvs_handedness_and_degenerate_fallback`.
- [x] Support alpha opaque, mask, and blend modes. First increment (data):
  `AlphaMode::{Opaque, Mask { cutoff }, Blend}` on `MaterialAsset`, imported
  from glTF `alphaMode`/`alphaCutoff`, saved as `SceneAlphaMode` in scene
  format 5. Cooked v1-v4 scenes migrate to `Opaque` (v4 is dispatched on its
  leading `format_version` because a v4 material can also parse as v5).
  Tests: `gltf_import_carries_sampler_state_and_alpha_mode`,
  `material_alpha_mode_survives_scene_round_trip`,
  `version_four_cooked_scene_migrates_materials_to_opaque_alpha`; cooked v3
  `testGame` scene still runs. Second increment (renderer): the ECS
  `SceneRenderer` discards masked fragments below the cutoff, writes alpha 1
  for opaque/mask, and draws blended instances last through a second pipeline
  (alpha blend, depth test without depth write), sorted back to front along
  the camera view each frame. GPU test
  `alpha_modes_render_opaque_mask_and_sorted_blend` (RTX 3060, headless
  readback) fails when sorting is disabled; unit test
  `blended_objects_render_last_unbatched_and_back_to_front`. Known ceiling:
  blended GPU-physics bodies sort by their last CPU transform.
- [x] Import node hierarchy and cameras/lights where available.
  `AssetServer::import_gltf_scene` returns one `ImportedGltfNode` per glTF
  node (name, parent index, local `Transform` from the decomposed TRS,
  mesh primitives, `Camera`, `KHR_lights_punctual` light); `spawn_gltf_nodes`
  spawns them with `Parent` links, extra primitives as child entities.
  Evidence: unit test
  `gltf_scene_import_spawns_hierarchy_cameras_and_lights` (parent/child
  links, quaternion to Euler, orthographic camera, spot light, spawned
  components). Light intensities keep glTF physical units. The editor's
  flat `import_gltf` path is unchanged (AGENTS.md); wiring the editor to the
  node import is the owner's call.
- [x] Sample imported UVs and material textures in the ECS renderer.
  Found in the `issues.md` audit. `SceneVertex` now carries UVs, and the
  fragment shader multiplies the material color by the base-color texture
  (set 1, one descriptor set per texture, white when a material has none or
  its texture is still loading). Uploads go through a staging buffer copied
  in the frame's own command buffer, with no CPU wait. sRGB textures use an
  `_SRGB` format; each glTF sampler (filter, wrap) gets a cached `Sampler`.
  A changed texture revision re-uploads, and removed textures leave the
  cache like meshes. Evidence: GPU tests
  `base_color_texture_tints_the_surface_and_reloads_next_frame` (fails with
  sampling disabled in the shader) and `material_without_texture_samples_white`.
  Known ceiling: no mip chain yet. The other maps landed with the Milestone 4
  PBR pass.
- [x] Add skins and animation after the static scene path is stable.
  Blocked: needs the skeleton/clip/pose runtime from Milestone 14; import
  lands together with that runtime rather than as data nothing consumes.
  Done 2026-10-10 (bookkeeping): skins and morph targets are item 7 of the asset plan (CPU skinning; GPU skinning is open there).
- [x] Preserve glTF materials unless the caller explicitly supplies an override.
  `spawn_gltf_nodes(app, nodes, material_override)` keeps each primitive's
  imported material when the override is `None`. Fixed a real loss: glTF
  textures used synthesized `.rtexture` keys that never existed on disk, so a
  saved scene with a glTF material failed to load in a fresh process. The
  importer now writes `.rtexture` (bincode, carrying color space and sampler)
  next to the source like `.rmesh`, and `load_texture` decodes it. Evidence:
  unit test `gltf_materials_survive_save_and_fresh_load_unless_overridden`
  (factors, alpha mode, linear normal map, sampler survive save plus fresh
  load; override replaces material); fails with the `.rtexture` write
  removed.

### Scene serialization

- [x] Define versioned, human-readable `.rscene` files with Serde plus a compact cooked binary form.
- [x] Assign stable UUIDs to serialized scene objects.
- [x] Store asset paths/UUIDs rather than runtime handles or ECS entity IDs.
- [x] Add an allowlisted component serialization registry.
- [x] Serialize hierarchy, transforms, renderers, lights, physics, and editor metadata.
  Hierarchy, transforms, renderers, cameras, visibility, classes,
  directional/point/spot lights, and physics were already typed scene fields.
  The missing `AmbientLight` component is now built into the default
  `SceneComponentRegistry` as `rusting.ambient_light`, so the scene binary
  layout is unchanged and the editor can add it like other registered
  components. Evidence: `all_authored_light_types_round_trip_together` now
  round-trips an ambient light through cooked bytes and fails with the
  registration removed. Editor metadata is split out below.
- [x] Serialize per-scene editor metadata (editor camera pose, selection).
  `save_editor_scene` writes the editor camera position, rotation, orbit
  distance, and selection (by `SceneId`, active object first) to a
  `<scene>.editor.json` sidecar, like Godot's per-scene edit state, so the
  game's scene file stays clean. `load_editor_scene` restores them after a
  Replace load; a missing or corrupt sidecar only clears the selection. Every
  editor save and open path, and the startup restore, uses the pair.
  Evidence: unit test
  `editor_scene_restores_camera_pose_and_selection_after_reload`.
- [x] Reject unsupported scene versions with a clear structured error.
- [x] Add explicit migration for legacy unversioned project and text-scene files.
- [x] Add scene save, load, additive load, and unload operations.

### Hot reload

- [x] Watch source assets and scenes for changes.
  `Assets::changed()` compares each path-backed slot's file modification
  time with the one recorded at load (stdlib polling, no new dependency);
  missing files are ignored so save-by-rename never drops a value. It works
  for every asset type, including `scenes`. `AssetPlugin` scans every
  `HOT_RELOAD_SCAN_INTERVAL` (500 ms). Known ceilings: one `stat` per
  watched file per scan; editing a source `.gltf` does not re-import it
  (only rewritten `.rmesh`/`.rtexture` reload).
- [x] Decode changed assets in worker tasks.
  `Assets::reload_async` decodes on a worker while the old value stays
  visible; `poll_loads` publishes the new value and bumps the revision, and a
  failed decode keeps the last good value and lands in
  `take_reload_failures`. `AssetServer::reload_changed` wires meshes and
  textures. Evidence: unit test
  `changed_files_reload_on_workers_and_failures_keep_last_value` (unchanged
  file skipped, old value visible mid-decode, new value published with a
  higher revision, corrupt rewrite keeps the old value and reports one
  failure, deleted file ignored).
- [x] Swap prepared resources at safe frame boundaries.
  Reloads publish in `poll_loads` during the ECS `Update` stage, so assets
  never change mid-render; `SceneRenderer::prepare_visible_meshes` sees the
  new revision at the start of the next frame and uploads fresh buffers
  instead of writing into in-use ones. Evidence: GPU test
  `hot_reloaded_mesh_swaps_next_frame_and_old_buffers_outlive_in_flight_frame`
  (frame 2 shows the reloaded mesh while frame 1 is still unwaited; a
  control run without the reload reads white, so the black result proves the
  swap).
- [x] Keep old GPU resources alive until every referencing frame completes.
  Old buffers are released by ownership: each submitted command buffer holds
  `Arc`s to the buffers it binds, and the frame future keeps the command
  buffer until its fence completes. Evidence: the same GPU test holds a
  `Weak` to the replaced vertex buffer; it stays alive while frame 1 is in
  flight and is freed once frame 1 is dropped after completion. Textures
  follow the same pattern: each frame's command buffer holds the descriptor
  set, and with it the image, it binds.
- [x] Surface reload success and failure in the editor console.
  `poll_loads` records the path of each published hot reload;
  `AssetServer::take_reloaded()` drains them, and the editor logs "Hot
  reloaded <path>" at Info and each failure at Error. Evidence: unit test
  `changed_files_reload_on_workers_and_failures_keep_last_value` (one path
  after a good reload, none after a failed one).

### Exit gate

- Loading the same asset path returns the same live asset identity.
- Saving and reloading a representative scene preserves all allowlisted state.
- Asset reload during rendering produces no invalid descriptor/resource use.
- Golden tests verify glTF channels, alpha modes, normals, and tangents.

## Milestone 3: Render extraction and frame management

Goal: eliminate frame-loop stalls and host/GPU races while making rendering derived from ECS state.

### Render extraction

- [x] Define a render world separate from the gameplay world.
- [x] Extract cameras, visible renderers, global transforms, lights, and material handles during `RenderExtract`.
- [x] Diff extracted state so unchanged render data produces no dirty uploads.
- [x] Build deterministic render keys and stable ordering from typed handles and entity identity.
- [x] Track contiguous dirty upload ranges and full-range reorder/removal updates.
- [x] Grow GPU buffers based on demand and device memory budgets.
  Render-instance uploads (rebuilt whenever anything moves) used a dedicated
  allocation per change; they now come from a vulkano `SubbufferAllocator`
  whose arenas start at 256 KiB, double when an upload outgrows them, and
  are reused once no in-flight frame references them. One upload is capped
  at half the largest device-local heap (`transient_upload_budget`); going
  over returns a `SceneRenderError` instead of allocating. Mesh buffers stay
  exact-size per revision. Evidence: unit test
  `transient_budget_is_half_the_largest_device_local_heap`; GPU test
  `instance_uploads_reuse_arenas_grow_on_demand_and_respect_budget` (two
  small uploads share one arena buffer, 5,000 instances grow it and still
  render, a budget one byte short fails with an explicit error). Known
  ceiling: the budget uses static heap size, not live usage
  (`VK_EXT_memory_budget` is not exposed by vulkano 0.35).
- [x] Add explicit errors or fallback paths for allocation/capacity failures.
  Audit of `SceneRenderer`: every Vulkan buffer/descriptor/command
  allocation already maps to `SceneRenderError`; instance uploads over
  budget are an explicit error; missing meshes fall back to the cube and
  missing materials render magenta. The two silent paths now report:
  lights past `MAX_LIGHTS` (64) keep the first 64 and count the rest in
  `RenderCapacityDiagnostics::dropped_lights`; GPU physics event-buffer
  overflow accumulates into `physics_events_dropped` (still logged). Both
  are read through `SceneRenderer::capacity_diagnostics()`. Evidence: GPU
  test `lights_over_capacity_render_first_max_lights_and_report_the_rest`
  (67 point lights upload 64 and report 3 dropped; back to 1 light reports
  0). Physics overflow counting has no dedicated test; it sits on the same
  readback path as the existing GPU physics event tests.

### Frames in flight

- [x] Create two or three explicit `FrameContext` objects.
  `SceneRenderer` owns `[FrameContext; FRAMES_IN_FLIGHT]` (2), used
  round-robin by `render`. Each context holds the fence of its last
  submission, which also keeps that submission's resources alive. Every
  frame now ends in a fence signal before the caller's present, not only
  physics frames. Physics readbacks share that fence. Evidence: GPU test
  `frame_contexts_bound_frames_in_flight_and_wait_only_on_reuse`.
- [x] Give each context its own fence, command allocator/pool state, uniform allocations, indirect buffers, visible lists, and transient descriptors.
  Each `SceneRenderer` `FrameContext` owns its fence and a host-visible
  `SubbufferAllocator` (64 KiB first arena). Debug-overlay vertices and GPU
  physics readback buffers now come from that arena instead of a dedicated
  allocation per frame. The physics descriptor set that references them is
  per-frame and pooled. The rest of the list maps as follows:
  command buffers come from vulkano's `StandardCommandBufferAllocator`,
  which pools per thread and recycles a buffer when its frame drops it.
  `SceneRenderer` has no uniform buffers (the camera is a push constant)
  and no indirect buffers. Visible lists are CPU-only. Evidence: GPU tests
  `transient_uploads_come_from_per_context_arenas_across_reuse` (the debug
  line renders in each of 4 frames, two full laps, and the two contexts
  never share an arena) and
  `physics_events_read_back_from_transient_arenas_every_tick` (one
  `WhileTrue` event per tick for 4 ticks, tick numbers match, no overflow).
  Before this test, no test covered the readback path through
  `SceneRenderer`. The legacy `Engine` still shares one indirect buffer per
  batch across frames; see the next two items.
- [x] Wait only when reusing a frame context whose fence has not completed.
  At the start of `render`, contexts whose fence already signaled are
  released without waiting. The one context being reused is waited on only
  if its fence has not signaled; that wait bounds CPU run-ahead to
  `FRAMES_IN_FLIGHT` frames. Evidence: the same GPU test hands back two
  frames unflushed. The third `render` submits and waits for frame 1 only;
  frame 2's fence stays unsignaled. With the wait removed, the test fails
  ("reusing context 0 first submitted and waited for its frame"). Frame
  pacing on real hardware is not verified here (hardware tier).
- [x] Chain acquire, transfer/compute, render, and present without CPU `wait()` calls in the normal frame path.
  In the product path (`project_runner` + `SceneRenderer`), one future
  chain runs from acquire to present: the acquire future is `render`'s
  `before`, physics compute and drawing share one command buffer, then come
  the frame fence and `present(.., false)`, which does not wait. Physics
  events are collected with `is_signaled` plus a zero-timeout cleanup, so
  that never blocks either. The only CPU block left is the bounded
  context-reuse wait from the item above. Evidence: the frame-context test
  shows a context that is not being reused is never waited on. Out of
  scope: the legacy `Engine` facade still waits on its separate compute and
  cull submissions. It follows the Milestone 0 precedent: that facade is
  deferred to its Milestone 1 replacement, not patched.
- [x] Retain every submitted future/fence until completion.
  Frame fences stay in their `FrameContext` until they signal or their
  context is reused after a wait. The present future is retained by
  `vulkano_util` (`previous_frame_end`), and physics readbacks by
  `pending_physics`. Evidence: the frame-context test asserts no
  unfinished fence is released. The hot-reload test shows old mesh buffers
  live until the frame that used them completed and the next frame
  released its fence.
- [x] Add deferred GPU-resource destruction queues per frame context.
  No separate queue was added; ownership already provides the deferral.
  A vulkano command buffer holds an `Arc` to every buffer, image, descriptor
  set and framebuffer it uses. Each `FrameContext` fence holds that command
  buffer until the GPU finishes. So anything the renderer replaces (mesh
  and instance buffers, lights, the depth target and framebuffers on
  resize) is freed only after its last frame completes. It is freed on the
  next `render` after that, with no wait. Evidence: GPU tests
  `hot_reloaded_mesh_swaps_next_frame_and_old_buffers_outlive_in_flight_frame`
  (mesh buffers) and
  `resize_defers_old_depth_destruction_until_its_frame_completes` (the
  depth target is replaced while frame 1 is pending, stays alive until
  frame 1 completes, then is released).
- [x] Clear culling counters and indirect commands using queued GPU operations.
  The legacy `Engine` reset `instance_count` with a host write every frame
  while the previous frame could still draw from that buffer. This made
  `user_main` panic in 2 of 10 runs with `AccessConflict(DeviceRead)`. The
  reset is now `record_reset_instance_count` (`src/scene/mod.rs`), a
  `fill_buffer` of that one field recorded before the cull dispatch. The
  cull and physics submissions are now chained after the previous frame
  instead of starting from `sync::now`. Evidence: GPU test
  `instance_count_reset_is_a_queued_gpu_fill_of_that_field_only`, and
  `user_main` ran to the timeout in 25 of 25 runs, about 3000 FPS as before.
  Known ceiling (`ponytail:` comment): on frames that run physics or
  culling, the CPU now waits for the previous frame, so CPU and GPU do not
  overlap there. Per-slot buffers would restore the overlap.
- [x] Remove host writes to buffers that may still be in GPU use.
  Audit of every `.write()` in the render paths: the legacy per-slot UBO is
  written only after that slot's fence wait. `SceneRenderer` writes only
  fresh suballocations: instance and transient arenas are reused only once
  no frame holds them, and vulkano's host-access lock rejects any
  conflicting write with an error, never a race. The one racing write, the
  indirect reset in the legacy `Engine`, was removed in the item above.

### Capabilities and fallback policy

- [x] Define a low-end Vulkan baseline suitable for Intel UHD 620-class hardware.
  `LOW_END_BASELINE` (`src/rendering/scene_renderer.rs`) requires:
  - Vulkan 1.1
  - `maxComputeWorkGroupInvocations` and `maxComputeWorkGroupSize[0]` of
    at least 256, which the physics shader's `local_size_x` needs
  - `maxPushConstantsSize` of at least 128 (the largest block used is 96
    bytes)
  - `maxStorageBufferRange` of at least 128 MiB
  - `D32_SFLOAT` usable as a depth attachment
  Every limit except compute invocations is the Vulkan-required minimum.
  No optional features or extensions are required. `SceneRenderer::new`
  reads `DeviceLimits` from the physical device and refuses a device
  below the baseline with an error that names the device and every
  shortfall. The instance upload budget is now also clamped to
  `maxStorageBufferRange`, because half of a large heap could exceed the
  range one storage binding may cover. Evidence: unit test
  `baseline_shortfalls_name_every_missing_property`, and GPU test
  `test_device_meets_the_low_end_baseline` on the headless test device.
  Not verified on a real UHD 620 (hardware tier). Its limits are expected,
  not measured, to meet the baseline.
- [x] Detect optional capabilities and expose them through renderer capabilities.
  `SceneRenderer::capabilities()` returns `RendererCapabilities`
  (`src/rendering/scene_renderer.rs`), detected once in `SceneRenderer::new`:
  - device name, integrated-GPU flag, and largest device-local heap size
  - multi-draw indirect, indirect-count drawing (core feature or
    `VK_KHR_draw_indirect_count`), bindless textures (all four descriptor
    indexing bits), and `VK_EXT_memory_budget`, each reported as
    `supported` by the GPU and `enabled` on the device
  - timestamp queries on graphics and compute queues

  A pass may use a feature only when `Capability::usable()` holds. No optional
  feature is enabled on the device yet, because no pass uses one.
  Evidence:
  - Unit test
    `optional_features_need_every_bindless_bit_and_accept_either_indirect_count_source`.
  - GPU test
    `renderer_reports_detected_capabilities_and_enables_none_it_does_not_use`.
    On the RTX 3060 it reports all four as supported and none as enabled.
- [x] Provide fallbacks for bindless descriptors, indirect-count drawing, and other advanced features.
  Audit of every draw call and shader: `SceneRenderer` uses only direct
  `draw`/`draw_indexed` and fixed descriptor sets, so its current path is
  already the baseline fallback. No shader declares a GLSL extension or a
  descriptor array. The GPU suite runs on a device with no optional feature
  enabled, which the capabilities test asserts. A pass that adopts an
  advanced feature later must branch on `Capability::usable()` and keep this
  path tested.
  The audit found one path that silently depended on an optional feature:
  the legacy `Engine` culling path wrote each batch's offset into the
  indirect command's `firstInstance`, which needs
  `drawIndirectFirstInstance`. That feature is never enabled, so the result
  was undefined. Now `firstInstance` is 0 and `base.vert` adds the batch
  offset from the push constant it already receives
  (`data[v_visible_list_offset + gl_InstanceIndex]`). A dead, unused
  indirect staging buffer next to it was deleted.
  Evidence: the shader compiles and the smoke runs pass. The culled image
  itself is not checked by a test; the exit gate's culling-equivalence check
  covers that.
- [x] Implement `QualityProfile::{Auto, Eco, Balanced, High}`.
  `RenderSettings::quality` is now extracted into `RenderWorld::quality`
  every frame. `resolve_quality` (`src/rendering/scene_renderer.rs`) turns
  `Auto` into a concrete profile from `RendererCapabilities`:
  - `Eco` on integrated GPUs
  - `Balanced` below 4 GiB of device-local memory
  - `High` otherwise
  Concrete profiles pass through unchanged. Today the resolved profile sets
  the per-frame light budget: 16, 32 or 64 lights (`light_budget`). Lights
  past the budget are dropped and counted in
  `RenderCapacityDiagnostics::dropped_lights`. A profile change rebuilds the
  light list even when the lights did not change. Later budgets (shadows,
  culling mode, substeps) plug into the same resolved profile.
  Evidence:
  - Unit test
    `auto_quality_resolves_from_device_class_and_concrete_profiles_pass_through`.
  - GPU test `lights_over_capacity_render_first_max_lights_and_report_the_rest`
    now switches High to Eco with unchanged lights and checks the 16-light
    upload. It fails (3 dropped instead of 51) without the rebuild on profile
    change.
  The `Auto` heuristic is static, marked with a `ponytail:` comment. Whether
  it picks the right profile on a real UHD 620 is hardware tier.
  The editor's Project area shows the resolved profile, marked "(Auto)"
  when it was picked automatically, and counts loaded LOD groups next to
  meshes. Evidence:
  `editor::tests::project_area_shows_lod_groups_and_the_resolved_auto_quality`.
- [x] Remove shader variants as user-facing performance controls; choose implementation variants automatically.
  The ECS/editor path (`SceneRenderer`) exposes no performance variant:
  - It builds one scene pipeline.
  - Materials choose appearance through `MaterialModel::{Pbr, Unlit}`, never cost.
  - Cost is set only through `QualityProfile`, which `Auto` resolves from
    capabilities (item above).
  The legacy `Engine` path's public `ShaderType` (including the
  benchmark-only `Heavy`) was removed by the Milestone 4 item "Replace
  public `ShaderType` with `MaterialModel`...". Solver selection (`PhysicsSolver`) is a semantic
  choice, and "Keep manual CPU/GPU and solver selection available" keeps it
  manual on purpose. Evidence: code audit only; no code changed.

### Exit gate

- No normal-frame CPU fence waits remain.
- Validation reports no synchronization or resource-lifetime errors.
- Two/three frames in flight can run for 10,000 frames without UBO, indirect-buffer, or descriptor races.
- Culling enabled and disabled produce equivalent visible geometry.

## Milestone 4: Rendering baseline

Goal: deliver a coherent, production-shaped forward renderer for the vertical slice.

### Pass schedule

- [x] Batch opaque ECS renderables by mesh and material and draw them through a cached GPU instance buffer.
- [x] Directional shadow-map pass.
  `SceneRenderer` records a depth-only pass into a 2048x2048 `D32_SFLOAT`
  shadow map before the main pass, drawing opaque/mask batches with
  `cast_shadows` (others collapse to a zero-area triangle in the shadow
  vertex shader). It shares the main pipeline layout and set 0, uses
  slope-scaled depth bias, and is always cleared so the main pass samples
  initialized depth. The orthographic light box covers `SHADOW_DISTANCE`
  (50 m) in front of the camera. Evidence: GPU test
  `directional_light_shadow_darkens_receivers_only_when_enabled` (off-view
  caster darkens the floor; non-caster, non-receiver, and `shadows: false`
  stay lit); fails with r=253 when the shadow factor is forced to 1.
  Known ceilings: one cascade, no texel snapping (edges shimmer as the
  camera moves), masked casters cast fully opaque, blended casters none.
- [x] Opaque forward PBR pass.
  The ECS fragment shader is metallic-roughness Cook-Torrance (GGX, Smith-
  Schlick, Schlick Fresnel) for every light kind, plus emissive. Light units
  are unchanged: a white Lambert surface facing a unit light still reflects
  1. `MaterialModel::Unlit` outputs the base color. Built-in primitives now
  get real tangents from `generate_tangents` instead of placeholders.
  Ambient became a sky/ground hemisphere with the sky/environment item.
  Evidence: GPU test `pbr_maps_and_unlit_model_shape_the_lit_color` (unlit
  ignores light; PBR without light is black; emissive factor and map; the
  occlusion map darkens ambient; a rough metal is darker than a dielectric
  and a metallic-roughness map with blue 0 restores the dielectric; a normal
  map tilted toward a grazing light brightens the surface). Disabling the
  normal, metallic-roughness, occlusion, or emissive sample in the shader
  each fails it.
- [x] Transparent forward pass with back-to-front sorting.
  Already delivered by Milestone 2 "Support alpha opaque, mask, and blend
  modes": second pipeline, per-frame back-to-front sort. Evidence: GPU test
  `alpha_modes_render_opaque_mask_and_sorted_blend` (fails with sorting
  disabled), unit test `blended_objects_render_last_unbatched_and_back_to_front`.
- [x] HDR render target and tone-mapping pass. The scene pass lights into an
  `R16G16B16A16_SFLOAT` target (subpass 0); subpass 1 reads it as an input
  attachment and maps it into the output with a `ToneMapping` component
  (`Linear` clip by default like Godot, `Reinhard`, `Aces`, plus exposure;
  saved as `rusting.tone_mapping`). Debug views and editor lines skip the
  curve. Evidence: GPU test `hdr_values_above_one_survive_until_tone_mapping`
  (fails with an 8-bit target, with the Reinhard branch removed, and with the
  debug-view bypass removed); scene-file round trip includes the component;
  all earlier renderer GPU tests pass unchanged.
- [x] Bootstrap Vulkan egui overlay/compositing pass for the runnable editor.
- [x] Engine-owned egui compositing pass shared with the production renderer.
  `rendering::egui_painter::EguiPainter` draws egui meshes in its own
  load/store render pass over any target, including the `SceneRenderer`
  output; `egui_winit_vulkano` is removed. It is behind the `editor` feature
  because that feature owns the egui dependency. Evidence: GPU tests
  `meshes_blend_over_the_target_in_points_and_respect_clip_rects` and
  `texture_patches_update_one_region_and_freed_textures_stop_drawing` (they
  fail with the sRGB output conversion removed, with the clip scissor
  widened to the full target, and with partial updates written at offset 0);
  the editor ran 8 s on Linux/Wayland and drew its UI with no validation
  panic.
- [x] Explicit pass resources, transitions, and debug labels.
  `rendering::frame_passes` declares the frame's passes in order (Uploads,
  Physics, Shadow, Scene, ToneMap, DebugOverlay), the resources each one
  reads and writes, and the image layout it needs. `layout_transitions()`
  derives the three layout changes (material textures and the shadow map to
  shader-read before Scene, HDR color to shader-read before ToneMap). The
  shadow render pass takes its final layout from the table, so the render
  pass does that transition; vulkano still inserts the remaining barriers
  until the Milestone 13 render graph. Each pass is recorded under its own
  debug-utils label inside the frame's `SceneRenderer::render` label, and
  `SceneRenderer::last_frame_passes()` reports the recorded order. The
  headless test instance now enables `ext_debug_utils` when the driver has
  it, so every GPU test records the labels. Evidence: unit tests in
  `frame_passes` (every read follows a write, unique labels, exact
  transition list); GPU test
  `frames_record_the_declared_passes_with_their_layouts` (pass order, shadow
  final layout, HDR input layout; fails with the shadow final layout
  removed); the 38 `scene_renderer` GPU tests under
  `VK_LAYER_KHRONOS_validation` on an RTX 3060 report no validation error,
  while a removed Shadow label begin reports
  `VUID-vkCmdEndDebugUtilsLabelEXT-commandBuffer-01912`.

### Materials and lighting

- [x] Replace public `ShaderType` with `MaterialModel::{Pbr, Unlit}` plus internal shader variants.
  The legacy `Material` now carries `model: MaterialModel` (builder
  `.model(...)`), and `Engine::set_scene_shader` takes a `MaterialModel`.
  `ShaderType` is no longer re-exported from the crate root; it stays in
  `rendering::shader_registry` as the legacy renderer's internal variant
  enum, mapped by `From<MaterialModel>`. The `Emissive`, `NormalDebug`, and
  `Heavy` variants are no longer selectable from the public API; the Pbr
  variant already renders emissive. Evidence: unit tests
  `material_builder_sets_model`, `material_builder_model_can_be_overridden`,
  and `instance_material_copy_preserves_every_property` (an Unlit material
  maps to the Unlit variant); doc tests on `Material` and `MaterialBuilder::model`.
- [x] Complete metallic-roughness PBR texture binding and sampling.
  Set 1 is now one cached descriptor set per material holding its five maps
  (white where absent), rebuilt only when a map is re-uploaded. Metallic and
  roughness factors, emissive, and the model travel in the instance buffer,
  which is already rebuilt on material revision changes. Evidence: as above.
- [x] Support base color, normal, metallic-roughness, occlusion, and emissive maps.
  Normal maps use the vertex tangent and handedness (glTF convention);
  metallic-roughness reads blue and green, occlusion red. Evidence: as above.
- [x] Support alpha cutoff and alpha blending.
  Already delivered by Milestone 2 "Support alpha opaque, mask, and blend
  modes": second pipeline, per-frame back-to-front sort. Evidence: GPU test
  `alpha_modes_render_opaque_mask_and_sorted_blend` (fails with sorting
  disabled), unit test `blended_objects_render_last_unbatched_and_back_to_front`.
  Later: blending is premultiplied. Diffuse and emissive scale by alpha,
  specular and environment reflections do not, so glass at alpha 0 still
  mirrors its surroundings. Known ceiling: no refraction or colored tint of
  what is behind; that needs a copy of the opaque color (Milestone 13
  "Refractive glass"). Evidence: GPU test
  `clear_glass_shows_what_is_behind_it_and_reflects_the_environment` (fails
  with the old `SrcAlpha` color factor: no red reflection).
- [x] Add one shadowed directional light.
  The first uploaded `DirectionalLight` with `shadows: true` is shadowed;
  its index reaches the fragment shader in `light_info.y`, and receivers
  (`receive_shadows`) apply 3x3 PCF through a comparison sampler. Evidence:
  same GPU test as the shadow-map pass.
- [x] Add several point lights with capability-appropriate limits.
  Point (and spot) lights share the light storage buffer with directional
  lights and fade to zero at `range` (squared linear fade). The per-frame
  budget comes from the resolved `QualityProfile`: Eco 16, Balanced 32,
  High 64 (`MAX_LIGHTS`), with `Auto` resolved from device capabilities.
  Lights past the budget are dropped and counted in
  `RenderCapacityDiagnostics::dropped_lights`. Known ceiling: every
  fragment loops over every uploaded light; no clustered culling yet.
  Evidence: GPU test `several_point_lights_add_up_and_fade_out_at_their_range`
  (a second light adds its color, a light past its range adds nothing, two
  equal lights are brighter than one); forcing the range fade to 1 or
  capping the shader loop at one light each fails it. GPU test
  `lights_over_capacity_render_first_max_lights_and_report_the_rest` and
  unit test for the ordered profile budgets cover the limits.
- [x] Add sky/environment ambient lighting.
  New `SkyLight { sky_color, ground_color, intensity }` component
  (registered as `rusting.sky_light`, so the editor can add it and scenes
  round-trip it) adds a hemisphere on top of any `AmbientLight`: +Y faces see
  the sky, -Y faces the ground. Diffuse samples it along the normal and
  specular along the reflection with roughness-aware Fresnel, so metals are
  no longer black without direct light. The 0.12 gray fallback applies only
  when neither component exists. The camera push constant grew to 128 bytes
  (`ground_ambient`), the guaranteed minimum. Known ceiling: two colors, no
  cubemap or HDRI; the sky system milestone brings image-based lighting.
  Evidence: GPU test `sky_light_lights_up_faces_with_sky_and_down_faces_with_ground`
  (top face red sky, bottom face blue ground, smooth metal reflects the sky,
  gray fallback without either light); ignoring the normal's Y, using the
  normal's Z, or dropping the environment specular each fails it. Unit test
  `all_authored_light_types_round_trip_together` round-trips a sky light.
  `base_color_texture_tints_the_surface_and_reloads_next_frame` now uses an
  unlit material, since a lit dielectric correctly reflects 4% of the
  environment.
- [x] Add material/texture fallback behavior and diagnostic rendering.
  Missing meshes draw the fallback cube and missing materials draw
  magenta (both existed). New: a referenced map that is missing or failed
  to upload binds a per-slot stand-in (magenta base color, flat normal,
  white for the rest) instead of white, so a broken base color map is
  visible and a broken normal map no longer tilts the shading.
  `RenderCapacityDiagnostics` now counts `missing_meshes`,
  `missing_materials`, and `missing_textures`. `SceneRenderOptions` gained
  `debug_view: SceneDebugView::{Lit, Unshaded, Normals}` (shader reads it
  from `light_info.z`); the editor's viewport settings popup has a
  "Viewport shading" section, and only the Scene workspace uses it.
  Evidence: GPU tests `missing_assets_draw_visible_fallbacks_and_are_counted`
  (a removed or malformed base color map draws magenta; a missing normal
  map under a grazing light matches no normal map; a removed material and
  mesh draw a magenta cube; all counters) and
  `debug_views_show_unshaded_base_color_and_normals`. Replacing the magenta
  or flat-normal stand-in with white, or disabling the normals view, each
  fails them. Unit test `scene_view_shading_applies_only_in_the_scene_workspace`.

### Visibility and quality

- [x] Add `RenderBounds` as a rendering-only component independent from the physics `Collider`.
  `rusting_core::components::RenderBounds` (default: the unit cube's box),
  saved as `rusting.render_bounds`. Evidence: scene-file round-trip test
  includes it.
- [x] Support local bounding spheres and axis-aligned boxes, transformed into world bounds during render preparation.
  `RenderBounds::{Sphere, Aabb}` are local; `RenderBounds::transformed`
  scales a sphere's radius by the largest axis scale and turns a box into
  the axis-aligned box of its transformed corners (Arvo). Extraction carries
  the local override in `ExtractedRenderable::bounds`; `SceneRenderer`
  transforms it (or the mesh box) into world bounds when it prepares
  instances. Evidence: unit tests
  `render_bounds_stay_conservative_under_transforms` (rotated, non-uniformly
  scaled, translated sphere and box; fails without the per-axis min/max) and
  `render_bounds_overrides_are_extracted_and_track_edits` (fails when the
  `RenderBounds` change tick is not tracked).
- [x] Generate default render bounds automatically for primitives and imported meshes, with an explicit editor override for unusual meshes and animations.
  An object without `RenderBounds` gets the box around the vertices of the
  mesh the renderer actually uploaded (`PreparedMesh::bounds`), so built-in
  primitives, imported glTF meshes, the fallback mesh shown while a mesh
  loads, and reloaded meshes are all covered. The prepared instances record
  the mesh revisions their bounds came from and rebuild when one changes,
  without a scene change. A `RenderBounds` component overrides the mesh box.
  Inspector editing of that override is the later "Render Bounds editing"
  item. Evidence: GPU test
  `culling_follows_the_camera_and_mesh_edits_without_scene_changes` (a mesh
  edit alone un-culls an object; fails when the mesh revisions are not
  checked).
- [x] Add `CullingMode::{Auto, Disabled, Frustum, FrustumAndOcclusion}`. Start with the first three modes; reserve occlusion culling for a later pass.
  `RenderSettings::culling` (default `Auto`) is extracted into
  `RenderWorld::culling`. When culling is on, `SceneRenderer` tests each
  instance's world bounds against the Gribb-Hartmann planes of the camera
  clip matrix on the CPU. It compacts the survivors into a visible list:
  each batch stays contiguous and draws once, and each kept blended instance
  gets its own slot. The main and blend vertex shaders read the instance
  index from that list as a per-instance vertex attribute. The list is
  cached until the camera clip matrix, the culling mode, or the prepared
  instances change, so a still camera costs nothing. The shadow pass still
  draws every caster from the instance buffer directly.
  `FrustumAndOcclusion` culls by frustum for now. How `Auto` picks a path
  is described in the selection and small-scene items below. The CPU path
  never culls GPU-physics bodies, because their CPU transform is stale.
  Frames with GPU bodies take the GPU path, which culls them. Evidence:
  - Unit tests: `frustum_test_keeps_bounds_that_touch_the_view_and_drops_the_rest`
    (sphere and box on each side of the side, near, and far planes) and
    `compaction_keeps_batches_contiguous_and_slots_blended_instances`.
  - GPU tests: `culling_follows_the_camera_and_mesh_edits_without_scene_changes`
    (the kept instance of a partly culled batch draws, a mesh edit
    un-culls, a camera move re-culls);
    `directional_light_shadow_darkens_receivers_only_when_enabled` (an
    off-view caster is culled and still casts, `Disabled` culls nothing,
    an override outside the view removes the floor from the image); and
    `gpu_bodies_are_not_culled_by_their_stale_cpu_transform`.
  - Mutations that fail these tests: the shader indexing with
    `gl_InstanceIndex`, the cache ignoring mesh revisions, the cache
    ignoring the camera, drawing whole batches, culling GPU bodies,
    dropping plane normalization for spheres, and the OpenGL near plane
    (`w + z`).
  - The validation layer reports 0 errors across 42 `scene_renderer` tests.
- [x] Select CPU or GPU frustum culling by object count, simulation ownership, device capability, and quality profile.
  `select_culling_path` (`src/rendering/scene_renderer.rs`) returns a
  `CullingPath` each frame:
  - `Disabled` gives `Direct`: every instance is drawn.
  - Any rendered GPU-owned body gives `Gpu`, because only the GPU has its
    newest transform.
  - Otherwise `Gpu` from `GPU_CULL_MIN_INSTANCES` (4096) instances, or four
    times that on integrated GPUs and with the resolved `Eco` profile.
  - Below the threshold, `Cpu`.

  The threshold is untimed and marked with a `ponytail:` comment; the
  profiler item replaces it with measured cull times.
  `SceneRenderer::last_culling_path()` reports the choice.
  `last_frame_culled()` is now `Option<usize>`: `None` on the GPU path,
  whose count stays on the GPU.
  Evidence: unit test
  `culling_path_follows_mode_ownership_count_device_and_quality`, which
  covers the threshold edge, GPU ownership, integrated GPUs, `Eco`, and
  `Disabled`.
- [x] Cull GPU-owned objects directly from their newest GPU physics transforms. Do not read every transform back to the CPU merely to decide visibility.
  The `cull_shader` compute pass (`FramePass::Culling`, after Physics)
  takes each instance's model from the physics state when it is a GPU
  body, and from the instance buffer otherwise. It transforms the local
  bounding sphere and tests the six clip planes. Nothing is read back to
  the CPU.
  The GPU tests a sphere around the mesh or `RenderBounds` box, so it keeps
  some instances that the CPU box test drops (`ponytail:` comment).
  Evidence: GPU test
  `gpu_culling_counts_visible_instances_from_gpu_transforms_into_indirect_draws`.
  A GPU body has its stale CPU transform in the view center and its GPU
  state at x = -3. The cull pass counts only the static cube in that
  batch. With the camera moved to x = -3, it counts only the body, and the
  body draws at its GPU position.
  `gpu_bodies_are_not_culled_by_their_stale_cpu_transform` now runs on the
  GPU path and still draws the body.
- [x] Make GPU culling write a compact visible-instance list and indirect draw commands consumed by the normal instanced renderer.
  Each GPU-culled frame allocates fresh buffers:
  - an indirect command per opaque batch and per blended instance, written
    by the host with `instanceCount` 0
  - a visible list sized like the instance buffer

  The pass `atomicAdd`s each visible instance into its draw's count and
  writes the instance index into that draw's range of the list. The main
  and blend pipelines are unchanged. They read `instance_index` from the
  list as a per-instance vertex attribute.
  Each draw binds the list sliced at its group's start and calls
  `draw_indexed_indirect` with `firstInstance` 0. This avoids
  `drawIndirectFirstInstance`, which is not enabled. The CPU path now draws
  through the same slices.
  Evidence:
  - The GPU test above reads back the commands after the fence: `[1, 1, 1]`
    (one opaque batch, two blended draws), then `[1, 0, 0]` after the camera
    moves. The frame's passes include `Culling`.
  - Unit test `cull_gpu_layouts_match_shader_structs` checks the struct
    sizes against the std430 and push-constant layouts, and the box-to-sphere
    radius.
  - Mutation checks that fail the tests: the shader ignoring the physics
    model, the shader skipping the plane test, all blended instances
    counting into one draw, and selection ignoring GPU ownership.
  - Not caught by a test: drawing the full batch range directly instead of
    indirectly, and ignoring the list slot base. Both leave the tested pixels
    unchanged.
  - The validation layer reports 0 errors across 45 `scene_renderer` tests.
- [x] Let `Auto` bypass the culling dispatch for small scenes where direct rendering is cheaper, using a tested threshold before timing-based selection exists.
  `select_culling_path` returns `Direct` for `Auto` below
  `AUTO_DIRECT_MAX_INSTANCES` (64) instances, even when there are GPU
  bodies: drawing everything is correct whoever owns the transform. An
  explicit `Frustum` still culls small scenes. The threshold is untimed
  (`ponytail:` comment); the profiler item replaces it with the measured
  break-even. The `SlabScene` GPU tests set `Frustum`, because every test
  scene is below the threshold.
  Evidence:
  - Unit test `culling_path_follows_mode_ownership_count_device_and_quality`
    covers 63 instances (`Direct`, with or without GPU bodies), 64
    (`Cpu`), and `Frustum` at 10 instances (`Cpu`, or `Gpu` with a GPU
    body).
  - GPU test `gpu_bodies_are_not_culled_by_their_stale_cpu_transform`
    switches to `Auto`. The frame takes `Direct` with no `Culling` pass,
    culls nothing, and still draws the body.
  - Mutation checks that fail the tests: removing the bypass, and `<=`
    in place of `<` at the threshold.
- [x] Keep render visibility separate from simulation activity: a culled object continues physics unless an explicit simulation-distance policy says otherwise.
  Culling results live only inside `SceneRenderer`: the visible list, the
  indirect counts, and `last_frame_culled`. No simulation code reads them.
  A search for `CullingPath`, `last_frame_culled`, and `Visibility` outside
  the renderer finds only extraction, the scene file, and the editor. The
  GPU physics dispatch runs over every body in the physics buffer before
  the cull pass, and the CPU physics systems do not query `Visibility`.
  No simulation-distance policy exists yet.
  Evidence: GPU test `culled_gpu_bodies_keep_simulating`. A GPU body sits
  at y = 5, above the ±1 view. On each of 4 ticks its draw count is 0
  (culled), and its `position_y < 5` rule fires, so gravity keeps moving
  it. With gravity removed, the test fails.
- [x] Add Render Bounds editing and debug visualization to the Inspector and viewport.
  The Mesh Renderer section has a "Render Bounds" choice: Mesh (no
  override), Box (Min/Max), or Sphere (Center/Radius). A new Box or Sphere
  starts from the mesh box, so the volume does not jump. Edits use the
  same Inspector snapshot path as every other field, so Undo covers them;
  choosing Mesh removes the component. With "Selected Bounds" on, the
  viewport outline of a selected object with an override shows the
  world-space volume that culling tests: the world box around the
  transformed corners, or three great circles of the scaled sphere.
  Without an override it still shows the mesh box.
  Evidence: unit tests `render_bounds_kinds_start_from_the_mesh_box`,
  `render_bounds_outline_uses_the_culled_world_volume` (box corners and
  sphere radius after translation and non-uniform scale), and
  `selected_outline_shows_the_render_bounds_override` (12 mesh-box lines
  become 96 sphere lines of radius 3). fmt and clippy are clean in all
  three configurations. Tests: 190+49+15, and 226+49+15 with `gpu-tests`.
  Not covered by a test: driving the egui combo box through a real frame.
  Later: a selected override shows a handle on each box face (or at the
  sphere's radius on each local axis). Dragging one moves that face along
  the object's local axis, stopping at the opposite face, or sets the
  radius; each drag is one Undo step. "Fit to Mesh" wraps the override
  around the mesh box again. Evidence: unit test
  `dragging_render_bounds_handles_moves_one_face_in_local_space` (a 2x
  X-scaled box's +X face dragged 2 m moves local max X by 1, min stays,
  face clamping and sphere radius).
- [x] Report submitted, visible, and culled instance counts plus culling compute time in the profiler.
  `SceneRenderer::culling_stats()` returns `CullingStats`: the path,
  submitted, visible, and culled counts, and the culling time. The
  `Direct` and `Cpu` paths time the CPU visibility pass. The `Gpu` path
  writes two timestamps around the cull dispatch and sums the indirect
  commands' instance counts. Both are read back once that frame's fence
  signals, without waiting, so the numbers lag a frame or two. The time is
  `None` when the queue has no timestamp support. The editor binary stores
  the stats as a resource each frame. The editor stats line then shows
  "Culling Gpu | 4 submitted, 3 visible, 1 culled | 0.012 ms". There is no
  dedicated profiler panel yet; that is backlog work in
  `docs/editor-overhaul.md`. The `GPU_CULL_MIN_INSTANCES` and
  `AUTO_DIRECT_MAX_INSTANCES` thresholds are still untimed. Tuning them
  needs these numbers from real hardware.
  Evidence: GPU test
  `gpu_culling_counts_visible_instances_from_gpu_transforms_into_indirect_draws`
  checks the read-back counts (4/3/1, then 1 visible and 3 culled) and that
  the time is `Some`. It fails when `culling_stats` skips the readback, and
  when the end timestamp goes to the wrong query. The CPU-path counts
  (2/1/1) and time are checked in the shadow-caster culling GPU test. Unit
  test `culling_stats_label_shows_counts_and_time` covers the label. The
  validation layer reports 0 errors across the 46 `scene_renderer` tests.
  fmt and clippy are clean in all three configurations. Tests: 191+49+15,
  and 227+49+15 with `gpu-tests`.
- [x] Add hierarchical-Z occlusion culling after frustum culling, depth-pyramid generation, and conservative-bound tests are stable.
  `CullingMode::FrustumAndOcclusion` now selects `CullingPath::GpuOcclusion`,
  a two-phase GPU cull. The early phase draws the opaque instances that
  passed the frustum and were visible last frame. A compute pass then
  builds a farthest-depth pyramid from that depth. The late phase tests
  every frustum-visible instance against the pyramid. It writes this
  frame's visibility history and draws only the instances the early phase
  skipped, in a late render pass that loads HDR and depth. Blended
  instances skip the history and go through the late phase. The occlusion
  test projects the corners of each bounding sphere's box and treats a box
  that crosses the near plane as visible. It then picks the mip where the
  rectangle covers at most two texels per axis and compares the nearest
  box depth with the farthest pyramid depth. The frame schedule gains the
  `DepthPyramid`, `OcclusionCulling`, and `LateScene` passes. The culling
  time sums both cull dispatches.
  Evidence: GPU test
  `occlusion_culls_hidden_objects_and_draws_revealed_ones_the_same_frame`
  hides a cube behind a wall. It checks the early and late draw counts
  over three frames ([0,2], [2,0], [1,0]) and the stats (2 submitted, 1
  visible, 1 culled). After the camera moves past the wall, the cube draws
  in the late phase of the same frame and the center pixel is red. GPU test
  `depth_pyramid_mips_hold_the_farthest_depth_below_them` reads back every
  mip of a 7×5 pyramid and compares it with a CPU max reduction, including
  the odd last row and column. Mutations the tests catch: `occluded()`
  always false, the early phase ignoring history, the late phase drawing
  early instances again, dropping the odd-edge fold, last texel instead of
  max, and the copy writing cleared depth. The validation layer, with
  synchronization validation on, reports 0 errors and no hazards across
  the 48 `scene_renderer` tests. fmt and clippy are clean in all three
  configurations. Tests: 191+49+15, and 229+49+15 with `gpu-tests`.
  Known limits: mip 0 of the pyramid is full resolution, a render-world
  instance rebuild resets the visibility history (one frame of extra
  late-phase draws), and the stats do not split frustum-culled from
  occluded counts.
- [x] LOD asset groups and distance/error-based selection.
  `LodGroupAsset` lists mesh levels finest first. Each level has a
  hand-over value. The `Distance` metric hands over at a world distance.
  The `ScreenSize` metric hands over when the bounding sphere covers less
  than a fraction of the view height, which tracks projected error across
  fields of view. `load_mesh` also loads a `.rlod` JSON file next to the
  mesh. Its level paths are relative to that file. A group applies to
  every renderable whose mesh is its finest level. The renderer expands
  each such renderable into one instance per level and gives each one a
  `[start, end)` range of a LOD value that grows with distance. The CPU
  cull and the GPU cull shader keep the instance whose range holds the
  value, so exactly one level draws, and nothing draws past the last
  level. Scenes with LOD groups skip the `Direct` path, and `Disabled`
  culling still selects levels. Only the finest level casts shadows.
  Evidence: GPU test
  `lod_groups_draw_the_level_for_the_camera_distance_on_every_path` moves
  an orthographic camera through level 0, level 1, and past the group on
  the `Cpu` (frustum and `Disabled`) and `GpuOcclusion` paths. It checks
  the drawn mesh per batch, the center pixel, the stats (2 submitted, 1
  visible, 1 culled), and that only level 0 casts shadows. Unit test
  `lod_value_grows_with_distance_like_group_ranges` checks level choice for
  perspective and orthographic cameras and `world_sphere`. Unit test
  `lod_group_beside_a_mesh_loads_with_it_and_rejects_bad_hand_overs` covers
  `.rlod` loading, deduplication, and the rejection of empty groups and of
  hand-overs that do not grow. Mutations the tests catch: the shader
  ignoring the range, the CPU cull ignoring the range, every level casting
  shadows, and path selection ignoring LOD groups. The validation layer,
  with synchronization validation on, reports 0 errors across the 50
  `scene_renderer` tests. fmt and clippy are clean in all three
  configurations. Tests: 193+49+15, and 232+49+15 with `gpu-tests`.
  Known limits: level changes pop with no cross-fade or hysteresis, the
  shadow pass draws level 0 at every range, `.rlod` files do not hot
  reload, and glTF `MSFT_lod` is not imported. Editor authoring is in the
  `docs/editor-overhaul.md` backlog.
- [x] Transparent sorting.
  Already delivered by Milestone 2 "Support alpha opaque, mask, and blend
  modes": second pipeline, per-frame back-to-front sort. Evidence: GPU test
  `alpha_modes_render_opaque_mask_and_sorted_blend` (fails with sorting
  disabled), unit test `blended_objects_render_last_unbatched_and_back_to_front`.
- [x] Shadow resolution and distance scaling by quality profile.
  `shadow_settings` maps the resolved profile to the directional shadow
  map size and the view distance it covers: `Eco` 1024 texels over 30
  units, `Balanced` 2048 over 50, and `High` 4096 over 80. The renderer
  recreates the shadow map and its framebuffer when the size changes.
  In-flight frames keep the old map alive. The shadow viewport and the
  light-space box follow the new values.
  Evidence: GPU test
  `directional_light_shadow_darkens_receivers_only_when_enabled` moves the
  camera 40 units from a shadowed floor. It checks that `Eco` leaves the
  floor lit (outside its distance) and that `Balanced` and `High` shadow it.
  It also checks the map size of each profile, including a switch back to
  `Eco`. Mutations the test catches: a fixed shadow distance, no map
  recreation, and a fixed viewport size. The validation layer, with
  synchronization validation on, reports 0 errors across the 50
  `scene_renderer` tests. fmt and clippy are clean in all three
  configurations. Tests: 193+49+15, and 232+49+15 with `gpu-tests`.
  Known limits: still one cascade with no texel snapping, and the sizes
  are fixed per profile rather than scaled with the output resolution.
- [x] Optional anisotropic filtering based on device support.
  Every device path (`init_vulkan`, `init_vulkan_headless`, and
  `rendering::vulkano_config` for the editor and exported games) enables
  `samplerAnisotropy` when the device has it. `vulkano_config` probes the
  device `VulkanoContext` will pick first, because `VulkanoContext` panics on
  a requested feature the device lacks. Texture samplers with linear
  minification filter at up to 16x, capped by the device limit; nearest
  filtering stays unfiltered. `RendererCapabilities::sampler_anisotropy`
  reports it. Evidence: unit test
  `anisotropy_needs_the_feature_and_linear_filtering_and_caps_at_16`, and
  GPU test `devices_enable_anisotropy_when_the_driver_supports_it` (fails
  when the headless device does not enable the feature, checked on an RTX
  3060). The editor starts without a panic with the feature on.
  Known limit: textures still have no mip chain, so distant textures alias.
- [x] Optional MSAA based on device support. The scene subpass renders
  into multisampled HDR and depth targets that resolve into the existing
  single-sample ones, so tone mapping, editor lines, and the depth pyramid
  are unchanged. `RendererCapabilities::msaa_samples` is 4 or 2 when the
  device can multisample both targets and resolve depth (Vulkan 1.2 or
  `VK_KHR_depth_stencil_resolve`), else 1. The MSAA passes and pipelines are
  built once at startup and share the single-sample pipeline layouts; each
  frame picks them unless the resolved quality is `Eco`, and the
  multisampled targets are freed when a frame goes back to 1x. Evidence:
  unit test
  `msaa_takes_the_largest_shared_count_up_to_four_with_a_depth_resolve`,
  and GPU test `msaa_smooths_edges_except_on_eco` (a slanted edge has only
  two shades on `Eco` and blended shades on `High`), plus the full GPU
  suite, all passing on an RTX 3060 and on llvmpipe. The NVIDIA driver lost
  the device (Xid 69) when the occlusion early pass ended its empty tone
  mapping subpass with the multisampled pipeline still bound; binding the
  tone mapping pipeline there fixes it.
  Known limits: depth resolves from sample 0, so the occlusion pyramid can
  be slightly non-conservative at silhouettes; 8x is not offered; no
  per-frame toggle other than the quality profile.

### Profiling

- [x] Vulkan timestamp queries per pass.
  Every `FramePass` writes a start and end timestamp (queries `2p` and
  `2p + 1` of a per-frame-context pool, reset at the start of the command
  buffer) through the new `PassRecorder`, which also does the debug labels.
  Once the frame's fence signals, `SceneRenderer::gpu_pass_times` returns
  `GpuPassTimes`, the time of each pass in recording order. The editor
  stats line shows the total and each pass. `CullingStats::time` now comes
  from the Culling, DepthPyramid, and OcclusionCulling pass times instead of
  its own timestamp pairs. Evidence: GPU test
  `frames_record_the_declared_passes_with_their_layouts` checks that every
  recorded pass, including those inside the render pass, has a time and that
  the total is above zero; `gpu_culling_counts_visible_instances_...` still
  checks the cull time. Both pass on an RTX 3060 and llvmpipe, and with the
  Khronos validation layer. Unit test
  `gpu_pass_times_label_shows_total_then_each_pass`.
  Known limits: times arrive one or two frames late; pass timestamps use
  `AllCommands`, so overlap between passes counts toward the earlier one;
  timestamp wrap-around reads as zero for one frame.
- [x] CPU timings for extraction, preparation, command recording, physics, and editor.
  `CpuFrameTimings` is a resource: `App::update` fills `physics` (all
  FixedUpdate steps) and `extraction` (RenderExtract);
  `SceneRenderer::write_cpu_timings` fills `preparation` (fence wait to
  command buffer start) and `recording` (builder creation to build); the
  editor fills `editor` (egui run and tessellation) and adds its second
  `extract_render_world` to `extraction`. The editor stats line shows all
  five. Evidence: unit tests `update_times_physics_and_extraction` and
  `cpu_timings_label_shows_each_part`; GPU test
  `frames_record_the_declared_passes_with_their_layouts` checks that
  preparation and recording are above zero on an RTX 3060 and llvmpipe.
  Known limits: `physics` covers every FixedUpdate system, not only physics;
  the fence wait and the egui paint are not counted.
- [x] Counters for draws, dispatches, triangles, visible instances, upload bytes, and GPU memory.
  `SceneRenderer::render_counters` returns the `RenderCounters` resource.
  Draws, dispatches, and direct-draw triangles are counted as commands are
  recorded; indirect draws add the triangles read back from their GPU-culled
  commands. Visible instances come from `CullingStats`. Upload bytes add up
  every host-written buffer of the frame: meshes, texture staging, instance
  and visibility lists, lights, cull commands, physics state, and debug
  lines. GPU memory sums the device memory blocks in the renderer's
  allocator pools. The editor stats line shows all six. Evidence: GPU test
  `frames_record_the_declared_passes_with_their_layouts` checks exact draw,
  dispatch, and triangle counts plus nonzero upload and memory;
  `gpu_culling_counts_visible_instances_...` checks that the triangle count
  includes the read-back indirect triangles and that 3 instances are
  visible. Both pass on an RTX 3060 and llvmpipe, and with the Khronos
  validation layer. Unit test `render_counters_label_shows_each_counter`.
  Known limits: indirect triangles arrive a frame or two late; GPU memory
  leaves out vulkano's dedicated allocations (large images) and other
  allocators such as the egui painter's.
- [x] Named GPU allocations/resources where practical.
  `name_object` sets a `VK_EXT_debug_utils` object name whenever the
  instance enables the extension. Named: the scene HDR and depth targets
  (with "(MSAA)" on the multisampled ones), the shadow map, the depth
  pyramid, every texture image and mesh vertex/index buffer (by asset path,
  or `#<handle key>` for assets built in code), the lights and GPU physics
  buffers, every render pass and pipeline, and the timestamp query pools.
  Evidence: headless GPU tests enable `ext_debug_utils` whenever the driver
  has it, and `name_object` has a `debug_assert` on the driver's result, so
  all 249 tests (RTX 3060 and llvmpipe) name every object without a
  rejection. The Khronos validation layer reports nothing on the
  `scene_renderer` tests. Unit test
  `asset_names_use_the_path_or_the_handle_key`.
  Known limits: suballocator arenas (instance and visibility lists, cull
  commands, per-frame uploads) stay unnamed. The windowed editor enables
  `ext_debug_utils` only with validation (debug builds with the layer
  installed, or `--features validation`). Names in RenderDoc or Nsight are
  not checked here.

### Exit gate

- Golden images cover primitive normals, PBR reference spheres, texture channels, alpha modes, shadow depth, and tone mapping.
- The renderer remains validation-clean through resize and asset reload.
- GPU timings and memory usage are visible to runtime code and the editor.

## Milestone 5: Hybrid physics

Goal: let each body use the processing unit and solver that fits its job while preserving a practical two-way connection between GPU simulation and normal Rust gameplay code.

### Determinism constraints, required from the start

Milestone 8 defines the full determinism requirements and owns their acceptance gate. Three of those rules cannot be retrofitted without rewriting the solver, so they apply while this milestone is implemented:

- [x] Never accumulate simulation state through order-dependent atomic operations. Use per-body accumulation slots or fixed-order reductions. Atomics remain acceptable for counters that do not feed simulation results. Audit: the ECS physics shader integrates each body alone and uses atomics only for the event counter and overflow count; `cull.comp` uses one for the visible list. `basic.comp` solved grid contacts in atomic cell-slot order, and each contact changes `vel`/`pos` for the next, so results changed with the grid build's thread order. It now visits each cell in ascending body index. Verified: `compute_registry::tests::grid_contacts_do_not_depend_on_cell_insertion_order` runs one step with the cell listed forward and reversed and requires bit-identical output; it failed before the fix and passes on llvmpipe and the RTX 3060. Known limit: a cell over 128 bodies still drops an arbitrary set; overflow handling is a later item. Rules recorded in `AGENTS.md` "Physics determinism".
- [x] Give body iteration and constraint ordering a stable sort key that survives buffer compaction, re-sorting, and insertion order. `basic.comp` orders contacts by body index (see above); `space.comp` already loops bodies in index order. GPU events reach Rust in atomic append order, so `route_gpu_physics_events` now sorts them by tick, `PhysicsId` slot and generation, event ID and flags; the slot is the stable key that survives buffer rebuilds. Verified: `runtime::tests::gpu_events_are_delivered_in_tick_and_body_order`.
- [x] Source any randomness inside a solver from a seeded, tick-indexed stream. Never read a global or thread-local RNG from simulation code. Audit: no solver, physics shader or runtime physics code uses randomness today (`grep` for `rand`, `random`, `thread_rng` finds no use; the `rand` dependency in `Cargo.toml` is declared but unused). The rule is recorded in `AGENTS.md` so the first solver that needs randomness follows it.

### Ownership and public model

- [x] Replace the old effect-only distinction with `SimulationClass::{Static, Cpu, Gpu}`. The selected class says where the newest runtime physics state lives. `Gameplay` is now `Cpu` and `GpuDynamic` is now `Gpu`; `None` stays for bodies no solver touches. Old scene files load through `#[serde(alias)]`, and old Rust code compiles through deprecated associated constants. The `GpuEffectBody` marker is removed: the editor and the game runner inserted it exactly when `uses_gpu()` was true, and nothing read it. The Inspector names the classes "CPU" and "GPU". Verified: `scene_file::tests::scenes_saved_with_old_simulation_names_still_load` and the existing physics routing, extraction and scene round-trip tests.
- [x] Add `PhysicsSyncMode::{None, Events, SelectedState, FullState}`. Synchronization is an explicit cost chosen per body or group, not a hidden full-scene copy. Evidence: per-body `PhysicsSyncMode` component (default `Events`, saved as `rusting.physics_sync`); `None` drops watch rules at extraction, only `SelectedState`/`FullState` bodies get a readback copy. `runtime::tests::sync_mode_controls_rules_and_state_mirror` and GPU test `scene_renderer::tests::only_state_synchronized_bodies_read_back_their_state` (llvmpipe and RTX 3060). Group-level selection is left to classes/prefabs setting the component.
- [x] Give GPU-simulated bodies a stable, generation-checked `PhysicsId` that is valid in ECS, GPU buffers, and events even after bodies are removed or buffers are sorted. Extend the same ID to the command bridge when commands are added.
- [x] Keep authored settings and identity in ECS while recording when mirrored transform/velocity data was produced and how many frames old it is. Evidence: `apply_gpu_state_samples` writes a `GpuStateMirror { tick, transform, velocities, custom_values }` and never touches the authored `Transform`; `GpuStateMirror::age_ticks` gives the age; stale generations are counted and older samples never rewind the mirror (`sync_mode_controls_rules_and_state_mirror`).
- [x] Allow CPU and GPU bodies in the same scene and allow static colliders to be consumed by both solvers. Evidence: the CPU solver skips GPU bodies (`runtime::tests::cpu_physics_is_deterministic_and_ignores_gpu_bodies`) and uses `Static` colliders. Extraction sends every non-sensor CPU/static collider (`PhysicsWorld::gpu_colliders`) and each GPU body's own shape to the native shader, which pushes GPU bodies out of them with friction, restitution and collision layers (`runtime::tests::gpu_bodies_extract_their_collider_and_see_cpu_colliders`, GPU test `scene_renderer::tests::gpu_bodies_rest_on_cpu_colliders_unless_filtered_or_no_collision`, llvmpipe and RTX 3060). Limit: GPU bodies do not collide with each other yet.
- [x] Show simulation owner, synchronization mode, readback age, and approximate synchronization cost in the editor. Evidence: the Inspector Physics section shows the owner (`Simulation`: CPU/GPU/Static), and for GPU bodies a `Sync` choice (undoable, stored as `rusting.physics_sync`), `Sync Cost` (bytes per tick from `PhysicsSyncMode::STATE_READBACK_BYTES`, compile-time checked against the GPU body layout), and `Last Readback` tick and age from `GpuStateMirror`. `editor::tests::inspector_shows_gpu_sync_mode_and_readback_age`. The editor preview does not simulate yet, so the age updates only where the mirror is applied (native Play).

### Programmable GPU condition and event bridge

- [x] Define a compact `GpuPhysicsEvent` layout shared by Rust and every physics shader: body ID, registered event ID, tick, flags, and a configurable small payload.
- [x] Provide a typed Rust condition builder for common GPU state fields, comparisons, boolean combinations, ranges, collision state, sleeping state, timers, and per-body custom values. This is a Rust API, not a separate scripting language.
- [x] Define event modes such as `OnEnter`, `OnExit`, `WhileTrue`, `Once`, and cooldown/rate-limited emission so conditions do not accidentally flood the readback buffer.
- [x] Upload built-in per-body condition instructions and rule parameters as compact GPU buffers. CPU-only custom-value uploads remain part of the command bridge.
- [x] Allow shared rules to target explicit multi-class object groups while keeping separate edge and cooldown state per body. Unrelated GPU bodies are not affected, and only matching body IDs and selected payloads return to Rust.
- [x] Add a custom condition-compute hook for arbitrary GLSL logic over GPU-accessible state. Custom physics and condition shaders use the same `emit_event(...)` ABI as built-in rules. Evidence: `GpuConditionShaders` holds `GpuConditionShader { events, glsl }`; the GLSL defines `condition(inout PhysicsState body)` and runs over every GPU body after each fixed step. It includes the same `src/shaders/physics_abi.glsl` as the built-in shader (body layout, push constants, `emit_event(body, event_id, payload_kind, payload)`), and `EVENTS[i]` holds the registered IDs, so its events arrive as ordinary `GpuPhysicsEvent`s. The renderer compiles sources at runtime with the system `glslc` (no new crate; `RUSTING_GLSLC` overrides the path), caches them by source, and skips a shader that fails to compile, keeping the message in `SceneRenderer::condition_shader_errors`. GPU test `scene_renderer::tests::custom_condition_shaders_emit_events_and_skip_broken_sources` (llvmpipe and RTX 3060); `runtime::tests::condition_shaders_reach_the_render_world_with_registered_events`. Needs `glslc` installed at runtime; precompiled SPIR-V input is not supported yet.
- [x] Allow events to return selected position, velocity, angular velocity, or custom-value payloads so a second state readback is often unnecessary. Add collision/contact payloads with the spatial solver.
- [x] Treat `Y < -100` only as the first end-to-end acceptance example. The implementation must not hard-code an axis, threshold, or event meaning. Evidence: `-100` appears only in tests and examples; the shader reads any `GpuStateField` by code and compares with the uploaded threshold. GPU test `scene_renderer::tests::conditions_use_any_field_and_see_custom_value_commands` fires an `OnEnter` rule on `PositionX > 10 || Custom(2) >= 3`, once from the start pose and once after a `SetCustomValues` command (llvmpipe and RTX 3060).
- [x] Compare previous and current condition results so edge-triggered events are emitted exactly when a condition changes state.
- [x] Let the native physics compute shader append events with an atomic counter into a per-frame event buffer.
- [x] Keep event output in per-frame mapped readback allocations so only emitted records cross into Rust; move the write target fully device-local if profiling shows mapped writes are costly on discrete GPUs.
- [x] Consume completed readback buffers asynchronously after their frame fence signals. Normal frames never wait for unfinished physics work.
- [x] Convert `PhysicsId` values back into live ECS entities and expose events to Rust systems through the engine event API.
- [x] Define event latency clearly: GPU events normally reach CPU gameplay one to three frames later. Logic requiring same-tick answers must use CPU simulation or an explicit blocking query. Evidence: `runtime::hybrid_physics` module docs, section "Latency" (events and `GpuStateMirror` carry the tick they describe; same-tick logic uses `SimulationClass::Cpu`). The blocking query is the open "explicit blocking readback API" item below.
- [x] Track event-buffer overflow, resize it within the configured memory budget, and provide an overflow fallback. Events must never disappear silently. Evidence: the per-frame event buffer is sized rules × fixed steps in the frame (each rule fires at most once per tick), so it cannot overflow below the `MAX_PHYSICS_EVENTS` budget (12 MiB). Past the budget, lost events are counted in `RenderCapacityDiagnostics::physics_events_dropped`, logged, and sent to gameplay as `GpuPhysicsEventsLost` so it can resynchronize from body state. GPU test `scene_renderer::tests::event_buffer_fits_every_step_and_reports_losses_past_budget` (three ticks in one frame deliver all 300 events; with a 60-event budget, 40 are reported lost). The budget is a constant, not derived from device memory yet.

### CPU to GPU command bridge

- [x] Add a compact command stream for spawn, despawn, teleport, velocity change, force, impulse, wake, solver change, and watch-condition updates. Evidence: `GpuPhysicsCommands::push(PhysicsId, GpuBodyCommand)` with `Teleport`, `SetVelocity`, `Impulse`, `Force` (one tick), and `SetCustomValues`, 80 bytes each. Spawn, despawn, solver, and watch changes go through the ECS components: extraction rebuilds the tables and `carry_regions` keeps surviving bodies' simulated state, so they need no command. Wake is left out until bodies can sleep. GPU test `scene_renderer::tests::commands_apply_once_before_the_step_and_reject_stale_bodies` (llvmpipe and RTX 3060) and `runtime::tests::gpu_commands_reach_the_render_world_once_per_batch`.
- [x] Upload commands in batches through each frame context instead of mapping or rewriting the complete physics buffer. Evidence: one batch per frame in the frame context's transient allocator, bound as binding 5 of the physics shader; the state buffer is never rewritten for a command. GPU test `scene_renderer::tests::commands_apply_once_before_the_step_and_reject_stale_bodies` (llvmpipe and RTX 3060) and `runtime::tests::gpu_commands_reach_the_render_world_once_per_batch`.
- [x] Apply commands before the fixed GPU step and reject commands whose body generation is stale. Evidence: the shader applies each body's commands (binary search in the index-sorted batch, submission order kept) before integrating on the frame's first step only; stale or unknown bodies are rejected on the CPU and counted in `RenderCapacityDiagnostics::physics_commands_rejected`, and the shader rechecks the generation. Commands wait while no fixed step runs and a batch is applied once. GPU test `scene_renderer::tests::commands_apply_once_before_the_step_and_reject_stale_bodies` (llvmpipe and RTX 3060) and `runtime::tests::gpu_commands_reach_the_render_world_once_per_batch`.
- [x] Support CPU-controlled kinematic bodies that collide with GPU bodies without transferring every GPU body to the CPU. Evidence: kinematic CPU colliders are uploaded with their velocity each frame, and the GPU contact uses the relative velocity, so a moving wall carries GPU bodies without any GPU state coming back. GPU test `scene_renderer::tests::kinematic_cpu_colliders_push_gpu_bodies` (llvmpipe and RTX 3060). Limit: one-way; GPU bodies do not push CPU bodies back.
- [x] Record command count and uploaded bytes for profiling. Evidence: `RenderCounters::physics_commands` and `physics_command_bytes`, asserted in GPU test `scene_renderer::tests::commands_apply_once_before_the_step_and_reject_stale_bodies` (llvmpipe.

### Selective state synchronization

- [x] Support asynchronous requests for selected transforms, velocities, sleeping state, or contact data by stable body ID. Evidence: `GpuPhysicsCommands::push(id, GpuBodyCommand::ReadState)` reads that body's full state back once after the next step, arriving as a `GpuStateMirror` like per-tick sync; stale IDs are rejected. Sleeping and contact data do not exist on the GPU path yet. GPU test `scene_renderer::tests::one_shot_state_reads_return_requested_bodies_once` (llvmpipe and RTX 3060).
- [x] Provide batched region/group snapshots for gameplay systems that need more than events. Evidence: `request_gpu_class_snapshot(world, class)` queues one `ReadState` per GPU body in an object class; the whole group reads back in one frame and arrives as `GpuStateMirror`s with the same tick. `runtime::tests::class_snapshots_request_one_read_per_member`; delivery is covered by GPU test `scene_renderer::tests::one_shot_state_reads_return_requested_bodies_once`. Regions are selected on the GPU instead: a `WhileTrue` rule with `inside` position conditions and a position payload, or a `GpuConditionShader`, returns only the bodies inside.
- [x] Keep full-state readback available for debugging, save-state capture, editor inspection, and tests, but keep it off the normal gameplay path. Evidence: `GpuPhysicsCommands::read_all_states` is a one-shot flag (default off) that copies every body once; the per-tick path copies only `SelectedState`/`FullState` bodies. GPU test `scene_renderer::tests::one_shot_state_reads_return_requested_bodies_once` (llvmpipe and RTX 3060).
- [x] Triple-buffer readback storage with frame contexts so GPU writes, transfer copies, and CPU reads never race. Evidence: event and state readback slices come from the per-frame-context transient allocator and are read only after that context's fence signals (`collect_physics_readbacks`); `only_state_synchronized_bodies_read_back_their_state` runs `2 * FRAMES_IN_FLIGHT` ticks and checks every sample is from its own tick.
- [x] Add an explicit blocking readback API only for tooling and exceptional cases, with a name and warning that make its performance cost obvious. Evidence: `SceneRenderer::block_until_physics_readbacks_complete`, documented as slow and not for gameplay; GPU test `scene_renderer::tests::one_shot_state_reads_return_requested_bodies_once` (llvmpipe and RTX 3060) uses it instead of waiting on the frame.
- [x] Support play/stop snapshots without requiring continuous full-state synchronization. Evidence: the authored ECS scene is the play snapshot, since GPU state only reaches `GpuStateMirror` and never the authored `Transform`. Stop sets `GpuPhysicsCommands::reset_to_authored`, which restarts every GPU body from its authored components with rule state cleared, and reads nothing back. A mid-play state captured once with `read_all_states` resumes through `GpuPhysicsCommands::restore` in the same batch as the reset. GPU test `scene_renderer::tests::reset_restarts_from_authored_state_and_restore_resumes_a_snapshot` (llvmpipe and RTX 3060); `runtime::tests::gpu_commands_reach_the_render_world_once_per_batch`.

### Self-written CPU physics and queries

- [x] Add an engine-owned CPU `PhysicsWorld` for bodies that require immediate gameplay answers. Evidence: `runtime::cpu_physics` runs in every `App`'s `FixedUpdate` for `SimulationClass::Cpu` bodies, with `Static` bodies as colliders. It integrates gravity and velocity, resolves contacts with 8 sequential-impulse iterations (restitution, friction, position correction), writes `Transform` and `RigidBody::linear_velocity` back on the same tick, and keeps a `PhysicsWorld` resource with `contacts()`, `raycast`, and `overlap_sphere`. `CollisionEvent` now comes from real contacts instead of bounding spheres. GPU bodies are ignored. Tests: `runtime::tests::cpu_boxes_fall_and_rest_in_a_stack_on_static_ground`, `cpu_restitution_bounces_and_sensors_only_report`, `cpu_physics_is_deterministic_and_ignores_gpu_bodies`, and the pair and ray tests in `runtime::cpu_physics::tests`. Limits: contacts apply no torque yet and child bodies are treated as fixed colliders. The pair search is now a sort-and-sweep broad phase (see the GPU solvers section).
- [x] Support fixed, dynamic, and kinematic rigid bodies plus boxes, spheres, capsules, convex meshes, and static triangle meshes. Evidence: fixed, dynamic, and kinematic bodies with box (oriented, SAT), sphere, and capsule colliders work (see `PhysicsWorld` above). `ColliderShape::ConvexMesh` uses the convex hull of the entity's `MeshRenderer` mesh and `ColliderShape::TriangleMesh` its triangles (static only; a body with it never moves), both scaled by the transform and cached per mesh revision. Pairs with a mesh collider use GJK on the shape cores plus EPA for overlaps; rays hit hull faces from outside and triangle meshes from either side; resting mesh contacts get up to four points. The Inspector offers both shapes. Tests: `runtime::cpu_physics::tests::mesh_colliders_match_primitives_and_report_normals_from_a_to_b`, `rays_hit_hulls_from_outside_and_triangle_meshes_from_either_side`, and `runtime::tests::cpu_convex_mesh_lands_flat_on_a_triangle_mesh_floor`. Limits: each triangle of a mesh is tested (no BVH yet), GPU bodies see a mesh collider as its bounding box, and `shape_cast`/`move_character` take primitive shapes only.
- [x] Add triggers, collision layers, raycasts, shape casts, overlap queries, character movement, and the joints required by the vertical slice. Progress: triggers (`Collider::sensor` reports without a response), `PhysicsWorld::raycast`, and `PhysicsWorld::overlap_sphere` are done. `CollisionLayers` filter contacts and query masks (`runtime::tests::cpu_collision_layers_filter_pairs_and_queries`). `PhysicsWorld::shape_cast` sweeps an unrotated box, sphere or capsule and stops just before the first solid collider, and `PhysicsWorld::move_character` moves a character shape with collide-and-slide and reports `grounded` (`runtime::tests::cpu_shape_casts_stop_before_colliders_and_characters_slide`). Joints: the Milestone 7 vertical slice lists none, so none are required here; the full joint set is its own later item ("Joints: fixed, hinge, slider, ...").
- [x] Add sleeping, continuous collision detection, stable contact generation, and iterative solving. Progress: iterative solving is done (sequential impulses with accumulated clamping; a three-box stack rests within 3 cm). Sleeping is done: still bodies get `Sleeping` after `SLEEP_STEPS` and wake on touch or edit (`runtime::tests::cpu_bodies_sleep_when_still_and_wake_on_touch_or_edit`). CCD for fast bodies uses a center-ray sweep (`runtime::tests::fast_cpu_bodies_do_not_tunnel_through_thin_walls`, which fails without the sweep). Contact manifolds are done: two boxes touching face to face get up to four points (incident face clipped to the reference face), and the solver applies impulses with angular response, per-point Coulomb friction, and warm starting from the last step (`runtime::tests::cpu_tilted_box_tips_over_onto_a_face`, `runtime::tests::cpu_sphere_sliding_on_the_ground_starts_rolling`; the stack test now also checks tilt below 0.01 rad).
- [x] Allow selected GPU events to create, update, or remove CPU proxy bodies when gameplay needs an approximate local query representation. Evidence: `GpuQueryProxy` on a GPU body names a `place_on` event (with a `Position` payload) and an optional `remove_on` event; `sync_gpu_query_proxies` keeps one `Static` proxy entity marked `GpuProxyOf`, removes it with the body or the component, and GPU bodies skip proxies. Test `gpu_events_place_move_and_remove_a_cpu_query_proxy` routes raw events and raycasts the proxy after it is placed, moved and removed. Limit: the proxy pose is as old as the event.

### GPU solvers and custom allocation

- [x] Connect per-object `ComputeShaderType` selection to the ECS/editor game runner instead of only the compatibility `Engine` path. Evidence: the ECS path selects per body through `PhysicsBody::solver` (the Inspector's GPU Solver), which the shared native shader reads from `custom_values.x`: Full, Simplified, NoCollision (skips collider contacts) and Space (point attractor). `Custom` now works too: the body's hook file defines `void solve(inout PhysicsState body)`, extraction reads each distinct file once (`RenderWorld::gpu_solver_shaders`), and the renderer compiles it like a condition shader that runs only for bodies whose `properties.w` holds that file's `custom_solver_id`; the built-in step leaves their motion alone. The Inspector's Open in Code Editor starts a missing file from a template. GPU test `scene_renderer::tests::custom_solvers_move_only_their_own_bodies` (llvmpipe and RTX 3060) and `runtime::tests::custom_solver_files_reach_the_render_world_once_per_path`. Limits: Simplified behaves like Full, and solver files reload only when GPU bodies are re-extracted.
- [x] Preserve mixed `Static`, `NoCollision`, simplified, full, spatial-grid, and custom compute batches in one scene. A scene-wide override remains a debugging tool only. Evidence: in the ECS path Static colliders and NoCollision, Full, Simplified, Space and Custom GPU bodies mix in one scene (`gpu_bodies_rest_on_cpu_colliders_unless_filtered_or_no_collision`, `custom_solvers_move_only_their_own_bodies`), and Full, Simplified and Space bodies now also collide with each other through a spatial-hash grid (`src/shaders/compute/physics_contacts.comp`) while NoCollision and Custom bodies skip it. GPU test `scene_renderer::tests::gpu_bodies_collide_with_each_other_through_grid_and_fallback` (llvmpipe).
- [x] Rename the stable form of `ComputeShaderType::Test` to describe its actual solver while keeping a temporary compatibility alias. Evidence: the variant is now `ComputeShaderType::GridCollision` (the spatial-hash collision solver in `basic.comp`); `ComputeShaderType::Test` remains as a deprecated associated constant that still works in expressions and patterns (`compute_registry::tests::old_test_name_still_selects_the_grid_solver`). Examples use the new name.
- [x] Add a true spatial broad phase to every built-in collision solver. Broad phases prune separated bodies, but dense overlapping scenes can still require quadratic contact work.
  - [x] Make the CPU `PhysicsWorld` sweep the axis with the greatest center spread and reject pairs separated on either other axis before the exact shape test. Pairs return in stable body order. Evidence: `broad_phase_finds_the_same_pairs_as_testing_every_pair` matches brute force on 200 scattered bodies; `broad_phase_prunes_a_tall_column_without_changing_pair_order` reduces a 200-body X-overlapping column to its one actual candidate pair.
  - [x] GPU body-to-body contacts use a spatial hash with a cell size derived from body sizes (`contact_grid_cell_size`), checking 27 neighbouring cells plus the fallback list. The legacy shader path is no longer compiled.
  - [x] Give the native GPU shader a spatial broad phase for CPU/static colliders. The renderer builds a stackless bounding-volume tree from each frame's collider snapshot, in stable collider order, and uploads it beside the colliders. The shader traverses from each GPU body's current pose, skipping separated subtrees; after a contact changes the pose it restarts at the root after the last processed collider, preserving the old list-order semantics. A CPU test checks 128 separated colliders yield one candidate and fewer than 20 visited nodes; the headless GPU tests `gpu_bodies_rest_on_cpu_colliders_unless_filtered_or_no_collision` and `gpu_collider_tree_rechecks_later_branches_after_a_push` check late collider discovery, layers/solver filtering, and correction into a previously separated branch. The `ColliderNode` upload has a shader-reflected layout assertion. The tree is rebuilt each physics frame so kinematic colliders move with their CPU poses.
- [x] Replace fixed hash capacities with device-budgeted growable buffers. The active `PhysicsContactGrid` is device-local, sized from the body count (two hash cells per body), and grows by doubling. Its hash is capped at a sixteenth of the largest device-local heap (`contact_grid_hash_stays_inside_its_memory_budget`); bodies that do not fit spill into a fallback list that preserves all bodies. GPU test `gpu_bodies_collide_with_each_other_through_grid_and_fallback` reruns with a zero budget (one hash cell) and gets bit-identical results, with overflow reported. The fixed-capacity legacy `basic.comp` path was removed from compilation in Milestone 1. Verified with default and serial `gpu-tests` workspace suites.
- [x] Track grid-cell overflow, hash collisions, oversized-body count, and total fallback work. Evidence: the contact passes add them to `contacts` in the event header, and `RenderCapacityDiagnostics` reports `physics_grid_overflow`, `physics_oversized_bodies`, `physics_grid_hash_collisions` and `physics_fallback_tests` for the latest completed physics frame. `gpu_bodies_collide_with_each_other_through_grid_and_fallback` asserts overflow, oversized and fallback counts; `hybrid_physics_gpu_layouts_match_shader_structs` checks the header layout. Limit: the legacy `GridCollision` path is not tracked.
- [x] Implement a tested overflow fallback that preserves every body. Evidence: in the ECS contact grid, a body whose cell is full or which is bigger than one cell goes to a fallback list that every body tests, and oversized bodies test every body, so no contact is skipped. `gpu_bodies_collide_with_each_other_through_grid_and_fallback` crowds twelve overlapping pebbles into one eight-slot cell and puts a small sphere on an oversized one: every pebble separates, the small sphere rests on top, and a second run matches bit for bit. Limit: the legacy `GridCollision` shader still drops bodies past `MAX_PER_CELL`.
- [x] Define a versioned custom-compute ABI for instance state, commands, condition inputs, custom values, and event output. Evidence: `src/shaders/physics_abi.glsl` is included by the built-in shader and every custom condition shader. It holds the `PhysicsState` layout (with `custom_values`), the `BodyCommand` layout and `COMMAND_*` kinds, the `PhysicsEvent` layout, bindings 0/3/4, the push constants, and `emit_event`, and it defines `RUSTING_PHYSICS_ABI_VERSION` so a shader can `#error` on a mismatch. Rust exposes the same number as `runtime::GPU_PHYSICS_ABI_VERSION`. GPU test `scene_renderer::tests::hybrid_physics_gpu_layouts_match_shader_structs` checks every Rust mirror against the reflected shader structs and the version against the GLSL define. Built-in condition instructions and rule state stay private to the built-in shader; custom shaders receive their inputs as `custom_values` and push constants.
- [x] Let custom shaders emit the same typed events as built-in solvers so Rust gameplay can react without downloading complete buffers. Evidence: custom condition shaders call the shared `emit_event` with IDs from `EVENTS[i]`, registered by name in `GpuEventRegistry`, and the events arrive as ordinary `GpuPhysicsEvent`s. GPU test `scene_renderer::tests::custom_condition_shaders_emit_events_and_skip_broken_sources` (llvmpipe and RTX 3060).

### Profiling and automatic allocation

- [x] Measure CPU physics time, GPU physics time, dispatch count, command bytes, event bytes, selected-state bytes, synchronization latency, and overflow counts. Evidence: CPU physics time is `CpuFrameTimings::physics` and GPU physics time the `FramePass::Physics` timestamp pair; `RenderCounters` adds `physics_dispatches`, `physics_commands`/`physics_command_bytes`, `physics_event_bytes`, `physics_state_bytes` and `physics_readback_latency_frames`, and `RenderCapacityDiagnostics` holds event loss and contact-grid overflow, oversized, hash-collision and fallback counts. The editor's Stats panel shows them on a "GPU Physics" line (`editor::view::gizmo_tests::physics_counters_label_shows_traffic_and_fallbacks`); `gpu_bodies_collide_with_each_other_through_grid_and_fallback` checks the dispatch, event-byte and latency counters on a real frame. Limit: latency is counted in rendered frames, not milliseconds.
- [x] Add repeatable 1K, 10K, and 100K body benchmark scenes covering falling, stacking, debris, and mixed solvers. Evidence: `runtime::PhysicsBenchmark` (`Falling`, `Stacking`, `Debris`, `Mixed`) builds any body count as a pure function with no RNG; `runtime::physics_benchmark::tests` checks 1K/10K/100K counts, identical output across calls, no starting overlaps, and the Mixed solver interleave. `cargo run --release --example physics_bench -- <scene> <count>` runs one in the game window and prints ms/frame. GPU test `scene_renderer::tests::benchmark_scenes_run_on_the_gpu_and_repeat_exactly` (1K bodies, 90 ticks, headless Vulkan) requires colliding bodies to stay on the ground, ten-cube towers to stand, and Debris and Mixed to repeat bit for bit. It found two bugs, both fixed: towers sank through each other (the contact pass now uses shock propagation: a supported body below counts as immovable), and crowded cells broke determinism (contact sums are now fixed-point integers, so atomic list order cannot change them). Limit: 10K/100K frame times are real-hardware numbers and are not recorded here.
- [x] Add an optional `Auto` allocation policy that uses body requirements, query needs, hardware capabilities, transfer cost, and measured timings. Evidence: the `runtime::AutoSimulation` component (scene key `rusting.auto_simulation`) opts a body in; `allocate_auto_simulation` runs in PostUpdate before GPU IDs are assigned. Requirements come first: non-dynamic bodies, no GPU backend, custom solvers (GPU), kinematic bodies, sensor and mesh colliders, and sync modes that read state back every tick (query needs and transfer cost) all force a class. Flexible bodies go to the GPU when there are at least `AutoAllocationPolicy::gpu_min_bodies` of them or when the last `CpuFrameTimings::physics` exceeded `cpu_physics_budget`, else to the CPU. Test `runtime::tests::auto_simulation_picks_cpu_or_gpu_and_stays_overridable` covers every reason. Limit: decisions are sticky; a decided body never migrates, and transfer cost is judged by sync mode, not by measured bytes.
- [x] Keep manual CPU/GPU and solver selection available; automatic allocation must be observable and overridable. Evidence: bodies without `AutoSimulation` keep their manual `PhysicsBody::simulation` and solver; the decision and its `AllocationReason` are public on the component, and clearing it makes the body decide again (runtime test above). The Inspector's Physics section has an "Auto CPU/GPU" checkbox (undoable component add/remove) that shows the decision in place of the manual choice (`editor::tests::inspector_shows_the_auto_simulation_decision`).

### Exit gate

- A scene with at least 10,000 GPU-simulated cubes can evaluate a user-configured condition and emit a Rust event when any cube crosses `Y = -100` without copying all cube transforms or blocking the frame loop. Replacing that rule with another supported or custom condition does not require engine changes.
- Tests cover triggers, raycasts, stacking, tunneling, collision layers, fixed-step independence, stale IDs, and CPU/GPU event delivery.
- GPU event and grid overflow are observable and their fallbacks never silently omit bodies or events.
- CPU/GPU commands and events remain correct with multiple frames in flight.
- Immediate gameplay queries never pretend that delayed GPU mirrors are current; state age is available to callers.
- Per-body ownership, solver, synchronization mode, traffic, latency, and overflow are visible in runtime diagnostics and the editor.

## Milestone 6: Integrated egui editor

Goal: allow scenes to be built, inspected, saved, and played without leaving the engine.

The editor should use `egui` and `egui-winit`. Rendering should go through an engine-owned Vulkan painter/integration layer so renderer compatibility and resource lifetime remain under project control.

### Editor shell

- [x] Add a startup Project Manager with create, open, folder selection, validation, and recent projects.
- [x] Create complete standalone Cargo game templates without overwriting existing folders.
- [x] Store project format versions and reject projects made by a newer editor.
- [x] Add a feature-gated editor plugin that can be excluded from runtime builds.
- [x] Build the initial egui shell with toolbar, hierarchy, transform inspector, viewport placeholder, and structured console.
- [x] Connect play, pause, single-step, and stop controls to runtime time control.
- [x] Show typed asset inventory, mesh/material handles, and live render-extraction dirty ranges in editor panels.
- [x] Add a runnable Vulkan editor window with winit input, resize handling, DPI-aware egui rendering, and presentation.
- [x] Render extracted ECS meshes as a depth-tested Vulkan scene beneath the editor UI.
- [x] Add revision-aware GPU mesh preparation so asset mutation invalidates cached buffers without changing handles.
- [x] Drive the scene viewport and camera aspect ratio from the DPI-aware central-panel pixel rectangle.
- [x] Expose camera FOV, clipping planes, priority, and active state in the inspector.
- [x] Add Scene, Game, and Code workspaces with editor/game camera selection.
- [x] Add a project-local Rust/GLSL editor with open, validation, and save actions.
- [x] Run Cargo checks and Debug/Release builds in a worker and show compiler output inside Code Editor.
- [x] Export a native release folder with the executable, cooked scene, project assets, license, and run instructions.
- [x] Store portable scene-relative asset paths and resolve packaged scene data beside the executable.
- [x] Add a separate native Rust Cargo game project, project-local source editing, and Debug/Release build/run from the editor.
- [x] Add a concise native Rust scene API for common transform operations without hiding the ECS from advanced games.
- [x] Add a dockable area-tree layout with selectable editor types and project-local persistence.
- [x] Replace the bootstrap Vulkan egui integration with an engine-owned texture/mesh upload path and render pass. Evidence: see Milestone 4 "Engine-owned egui compositing pass" (`EguiPainter` GPU tests, editor smoke run).
- [x] Add a central editor-shortcut action map; `Numpad 0` toggles Scene View fly-camera pointer capture while Escape remains a normal UI key.
- [x] Add a Settings panel for rebinding and persisting shortcuts, then route every editor command through the same action map. Evidence: the "Keyboard Shortcuts" area type lists every action by context (Editor, Scene View, Transform, Fly Camera); clicking a key waits for the next key press, Escape cancels, and a key taken from another action in the same context leaves that action unbound. The map is saved to `editor_shortcuts.json` in the user config folder and loaded over the defaults at startup. Undo, Redo, Save scene, Delete selection and Rename are now `EditorAction`s (Ctrl+Z, Ctrl+Shift+Z, Ctrl+S, Delete, F2) that the window-event handler queues and the view runs like the menu entries; the Hierarchy's hard-coded Delete/X/F2 keys are gone. Tests: `editor::shortcuts::tests::{editor_actions_need_their_exact_modifiers, captured_key_binds_the_waiting_action_and_escape_cancels, saved_shortcuts_load_over_the_defaults}`, `editor::tests::{queued_shortcuts_delete_and_undo_like_the_menu, shortcuts_area_lists_every_action_with_its_key}`; the editor starts cleanly. Each action can also take a second key (right-click removes it); Redo also answers Ctrl+Y. A file saved before second keys existed keeps the default ones (test `second_keys_trigger_actions_and_move_between_actions`). Limits: at most two keys per action, and X does not delete by default because it picks the X axis in the Scene View; widget-local keys (Enter/Escape in the rename field, Escape to cancel a gizmo drag) stay fixed. Follow-ups are in `docs/editor-overhaul.md`.
- [x] Route keyboard and mouse focus correctly between all viewport navigation modes and UI. Evidence: the Scene View takes pointer events only when egui's topmost layer under the cursor is the viewport (`EditorViewport::hovered`, test `editor::shortcuts::tests::covered_viewport_does_not_take_pointer_events`), and keys only when no text field wants them. Losing window focus ends fly and orbit navigation (`release_editor_navigation`). Pointer capture falls back from `Locked` to `Confined` on Windows and X11. While fly or orbit navigation is active, the editor now keeps presses, pointer motion, the wheel and text away from egui, so they cannot click, scroll or type into UI under the hidden cursor; releases still pass, so egui never keeps a button held (`ui_receives_during_navigation`, test `navigation_keeps_presses_and_motion_away_from_the_ui`). The view also drops text-field focus while navigating. Editor commands run only when no text field has focus and no navigation or transform is active. Limit: the grab fallback and focus loss need a real window to verify by hand.
- [x] Add DPI scaling, font configuration, and theme persistence.
  Evidence: OS display scale reaches egui through egui-winit (initial
  `scale_factor` plus `ScaleFactorChanged`); `EditorPreferences` gained
  `font_scale` beside `ui_scale`, View menu has TEXT SIZE choices, and both
  persist in `editor_preferences.json` without dropping each other
  (`editor_preferences_round_trip_and_fall_back_to_defaults`,
  `font_scale_multiplies_every_default_text_size`). Limits: one built-in
  palette, so there is no theme choice to persist yet; widgets that build
  their own `FontId` keep a fixed size.

### Core panels

- [x] Add project asset file import, filtering, typed texture loading, glTF primitive import, and selected-object assignment.
- [x] Persist imported glTF geometry as reloadable engine-native `.rmesh` assets.
- [x] Scene viewport rendered to an editor texture.
  Evidence: the editor renders the live 3D view into an offscreen image
  sized to the view (`scene_view_target` in `src/bin/editor.rs`) and egui
  draws it as `SCENE_VIEW_TEXTURE` through
  `EguiPainter::set_native_texture`
  (`native_textures_show_an_image_rendered_elsewhere`, gpu-tests;
  `scene_area_draws_the_offscreen_view_image_over_its_viewport`). Editor
  smoke run is clean. Limits: one live view as before; with no 3D area the
  scene still renders into the window under the panels, so GPU physics keeps
  running.
- [x] Entity hierarchy with filtering, selection, reparenting, and drag/drop.
  Evidence: search keeps matches and their ancestors
  (`search_keeps_matches_and_their_ancestors_only`), Ctrl/Shift
  multi-select with a primary object
  (`clicks_select_toggle_and_extend_with_a_primary_object`), drag/drop
  rejects cycles (`drag_parenting_rejects_self_and_descendant_cycles`), and
  dragging a selected row reparents the whole selection in one undoable
  scene edit that keeps world positions and nested children
  (`reparenting_a_selection_keeps_world_positions_and_nested_children`).
  Limits: no sibling reordering; after a reparent only the first moved
  object stays selected.
- [x] Component inspector driven by an allowlisted reflection/editor registry.
  Evidence: only components in the `SceneComponentRegistry` allowlist reach
  the Inspector; `InspectorRegistry::register::<T>(name, draw)` gives one a
  typed section, and the rest are edited as JSON fields (serde is the
  reflection). Typed edits go through the registry and the Inspector undo
  snapshot (`registered_inspectors_draw_and_edit_their_component`). Limits:
  built-in components (Transform, lights, physics) keep their hand-written
  sections instead of registry entries.
  Reflected fields show their `doc` hint under the label in the label's
  tooltip, as Godot shows property descriptions. Evidence:
  `editor::inspector::widgets::tests::a_described_row_shows_its_doc_in_the_label_tooltip`.
  A field drawn as a foldable group (a nested struct, list or map) shows
  its doc in the group header's tooltip. Evidence:
  `editor::inspector::reflected::tests::a_described_section_shows_its_doc_on_the_header`.
  A string-keyed map field adds an entry from a key field and removes one
  with its button; an empty or existing key adds nothing. Evidence:
  `editor::inspector::reflected::tests::map_entries_can_be_added_by_key_and_removed`.
- [x] Asset browser with folders, thumbnails, filtering, and drag/drop assignment.
  Evidence: the Assets area is a foldable FileSystem tree with a filter
  (`rows_put_folders_first_hide_caches_and_fold`). Image rows show a decoded
  preview, two new decodes per frame, with a larger one on hover
  (`image_rows_decode_a_few_thumbnails_per_frame`). Models drag into the
  Scene View; images drag onto a Hierarchy object (base color) or an
  Inspector texture slot, as one Undo step each
  (`dropping_an_image_on_hierarchy_rows_and_texture_slots_assigns_it`).
  Limits: list view only (no grid), previews decode on the UI thread and do
  not refresh when the file changes, and dropped models land at the origin.
- [x] Console with structured logs, filtering, warnings, and asset/validation errors.
  Evidence: `ConsoleEntry` carries a level, a source ("Assets", "Build",
  "Scene", "Code") and a repeat count; the Console area has Info/Warnings/
  Errors toggles with counts, a text filter, and Clear. Asset request
  results, hot reload failures, and scene load/save/validation status lines
  are logged (`console_groups_repeats_filters_and_records_status_lines`).
  Limits: status lines get their level from keywords, the log keeps the
  last 2000 entries, and engine code outside the editor has no log sink yet.
- [x] Profiler with CPU spans, GPU pass timings, counters, and memory usage. The dockable Profiler area shows a 180-render-frame CPU/GPU history graph, five measured CPU spans, every available completed GPU pass timestamp, draw/dispatch/triangle and culling counts, upload bytes, GPU allocator-pool memory, and physics overflow/event loss. Pause and Clear operate on the bounded history. The editor records one sample after publishing renderer diagnostics. Evidence: `history_is_bounded_and_pause_freezes_the_visible_frame`, `profiler_area_shows_timing_and_memory_sections`, and the full workspace and serial GPU test suites. GPU times can lag CPU frames; the allocator-pool figure excludes dedicated allocations.
- [x] Add dedicated render settings and physics diagnostics panels. The area switcher now opens Render Settings (scene quality/culling with snapshot undo, display pacing, resolved device profile, culling and missing-asset counts) and Physics Diagnostics (backend status, body/tick counts, GPU work/readback, overflow and rejected-event counts). Evidence: `panels_expose_missing_assets_and_physics_overflow`, `scene_render_settings_edits_are_undoable_scene_changes`, and the full workspace checks.
- [x] Add a typed physics inspector for simulation ownership, GPU solver profile, rigid body, and collider settings.

### Scene editing

- [x] Add editor-only Scene View overlays: XZ grid, selected-object local axes, and real mesh-bounds box.
- [x] Select the nearest rendered object by clicking Scene View, using an editor ray against transformed mesh bounds instead of requiring a physics collider.
- [x] Add a selection outline that remains readable when the object is behind other geometry. Selected mesh bounds and camera/light wire shapes now use the depth-independent debug pass with thicker lines; grid and unselected helpers remain depth tested. Evidence: `selection_outline_ignores_depth_without_changing_other_helpers` and the full workspace checks, including serial GPU tests. This is a wire outline of the authored bounds, not a pixel silhouette.
- [x] Translate, rotate, and scale gizmos with local/global modes and snapping. The Scene View toolbar now toggles world or object axes and snaps move to 1 unit, rotation to 15°, and scale to 0.1 factor steps. Global scale projects the chosen world axes onto local scale axes because `Transform` has no shear representation. Each drag still takes one snapshot undo step. The menu beside Snap sets the move, rotation and scale increments. Evidence: `gizmo_snap_quantizes_move_scale_and_rotation`, `global_gizmo_axes_ignore_object_rotation`, `global_rotation_uses_parent_space_axis`, `global_scale_on_rotated_object_uses_matching_local_axis`, and the full workspace checks.
- [x] Add FPS-style Scene View fly camera: Numpad 0 captures/releases the pointer, mouse changes yaw/pitch, WASD moves, Space/Ctrl move vertically, and Shift boosts speed.
- [x] Add camera orbit, pan, focus-selection, and framing controls after fly camera input is stable. Middle drag orbits around the view pivot, Shift+middle drag pans, the wheel dollies, and F or the Scene View Frame button centers and fits the selected mesh or object. Evidence: `orbit_keeps_the_pivot_fixed_and_dolly_moves_toward_it`, `focus_places_selected_object_in_front_of_camera`, and the full workspace checks. Home frames every rendered object, and Numpad 1, 3 and 7 turn to the front, right and top views around the same pivot. Evidence: `numpad_views_keep_the_pivot_and_frame_all_sees_every_mesh`. Ctrl turns to the opposite views, Numpad 9 looks at the pivot from the other side of the current view (evidence: the same test, which checks right to left and a tilted view to its mirror), and Numpad 5 (or VIEW > Orthographic in the viewport menu) toggles an orthographic view sized to the orbit pivot. Evidence: `orthographic_view_matches_the_pivot_size_and_follows_the_dolly`.
- [x] Create empty, cube, and camera objects; duplicate, rename, delete, and reparent entities.
- [x] Add/remove/edit registered compiled components through the generic JSON inspector.
- [x] Assign meshes, materials, textures, and physics shapes by typed handle. Mesh Renderer now lists saveable `Handle<MeshAsset>` and loaded `Handle<MaterialAsset>` choices with snapshot undo; the Material section already picks `Handle<TextureAsset>` values, and the typed ColliderShape picker selects primitive or mesh collider shapes (mesh shapes use the renderer's typed mesh handle). Evidence: `mesh_picker_excludes_transient_unsaved_handles`, `typed_asset_assignment_snapshots_once_and_rejects_unsaved_meshes`, and the full workspace checks. Physics shapes are enum variants, not standalone shape assets.
- [x] Scene new/open/save/save-as operations with native file pickers.

### Play workflow and history

- [x] Make Play save and cook the scene, compile the real Rust project, and launch its native game window.
- [x] Use fast Debug builds by default and allow optimized Release play tests.
- [x] Add stop and restart controls for the native game process. Stop already cancels Cargo or the child game; Restart is now available in the toolbar and Code area, waits for worker exit, then saves, cooks, builds, and launches again. A later Stop cancels a queued restart. Evidence: `restart_waits_for_worker_exit_and_stop_cancels_pending_restart` and the full workspace checks.
- [x] Add an optional embedded preview for workflows that do not need compiled Rust systems. Preview resumes the editor's ECS runtime in the Game area without a build; Pause, Resume, and Step control fixed time. Stop restores the authored scene, removes preview-spawned entities, and restores Undo/Redo history and the dirty flag. Evidence: `embedded_preview_restores_authored_scene_after_simulation` and the full workspace checks. Project Rust binaries still require native Play.
- [x] Make runtime-spawned entities visually distinct where useful. Entities created after embedded preview starts carry a `[Runtime]` badge in Hierarchy and a preview-only warning in Inspector; they disappear when preview stops. Evidence: `only_entities_created_during_preview_get_runtime_badges`, `embedded_preview_restores_authored_scene_after_simulation`, and the full workspace checks.
- [x] Implement snapshot-based undo/redo for scene and Inspector edits.
- [x] Group continuous Inspector field edits into single undo transactions.
- [x] Mark scenes dirty and prompt before destructive new/load/project-switch actions.

### Exit gate

- A user can construct, save, reload, play, pause, step, and restore a scene entirely through the editor.
- Undo/redo covers transforms, hierarchy, entity lifecycle, and component edits.
- Editor compositing has a golden-image test.
- Runtime-only builds do not depend on editor code.

## Milestone 7: Vertical slice

Goal: prove that the engine architecture works as a usable game-development stack.

### Required content

- [x] One representative imported environment with PBR assets. `samples/vertical_slice/environment.gltf` is a reproducible courtyard fixture with 11 nodes, three PBR materials, and embedded base-color and metallic/roughness maps; the normal glTF importer creates typed cooked meshes and textures for it. Evidence: `pbr_environment_imports_geometry_materials_and_maps`. The sample README records the import path and asset provenance.
- [x] Player camera/controller. `PlayerController` (`src/runtime/player.rs`, built into every `App` and saved as `rusting.player_controller`) walks a body entity with `PhysicsWorld::move_character` each fixed step, as its `Collider` shape or a default 1.8 m capsule, with gravity, jump, and sprint from the `player.*` actions (default WASD/arrows, Space, Shift). Mouse look yaws the body and pitches its `Camera` children; left click captures the cursor and Escape releases it; pitch clamps to ±89°. Evidence: `player_controller_falls_walks_jumps_and_stops_at_walls`, `player_look_needs_captured_cursor_and_clamps_pitch`, `player_controller_settings_round_trip_through_scene_registry`, and the full workspace checks including serial GPU tests. Limits: no step climbing or slope limit beyond sliding, collider scale is ignored, and a ceiling does not cancel upward speed.
- [x] CPU collisions, triggers, and at least one immediate physics query. A player body with a kinematic CPU capsule collider is stopped by a fixed wall, reported by a sensor it walks through (`CollisionEvent` with `sensor`), and the wall is found by an immediate `PhysicsWorld::raycast` on the same tick. Evidence: `player_controller_falls_walks_jumps_and_stops_at_walls`, plus the existing `cpu_restitution_bounces_and_sensors_only_report`, `cpu_collision_layers_filter_pairs_and_queries`, and `cpu_shape_casts_stop_before_colliders_and_characters_slide`. Sensors report every touching step; there are no separate enter/exit events.
- [x] Directional shadows, point lights, sky/environment light, and transparent content. One headless frame combines a shadowed directional light, a point light, a `SkyLight`, and an alpha-blended pane; each light uses its own color channel, and turning each feature off removes only its own contribution while the floor stays visible through the pane. Evidence: GPU test `vertical_slice_lighting_combines_shadow_point_sky_and_blend` (llvmpipe), on top of the per-feature tests from Milestone 3. The scene is test slabs, not the imported courtyard; shadows remain one directional map without cascades.
- [x] At least 10,000 GPU bodies that evaluate configurable conditions and send typed events to Rust gameplay without full-state readback. Evidence: GPU test `ten_thousand_gpu_bodies_report_condition_events_without_state_readback` (llvmpipe) runs 10,000 falling `Events`-sync bodies for 30 ticks with an `OnEnter` `position_y < 0` rule: exactly the 5,000 bodies that cross report one event each with their crossing position, nothing is dropped, and `physics_state_bytes` stays 0 while `physics_event_bytes` counts the event traffic. Delivery to typed `GpuPhysicsEvent`s on live entities is covered by `raw_gpu_events_reach_the_live_ecs_entity` and `gpu_events_are_delivered_in_tick_and_body_order`; custom condition shaders by `custom_condition_shaders_emit_events_and_skip_broken_sources`. `cargo run --example hybrid_10k` shows the same rule in a window. Frame time at 10,000 bodies is a hardware number and is not recorded here.
- [x] CPU-to-GPU commands that alter selected GPU bodies while the simulation is running. Evidence: GPU test `commands_alter_only_selected_bodies_mid_simulation` (llvmpipe) lets four bodies fall for 10 ticks, then sends an `Impulse` to one and `SetVelocity` to another: only those two change on tick 11, the others keep plain gravity, and all four keep simulating from the new state. `commands_apply_once_before_the_step_and_reject_stale_bodies` covers teleport, force, custom values, once-only application, and stale `PhysicsId` rejection; `gpu_commands_reach_the_render_world_once_per_batch` covers the ECS `GpuPhysicsCommands` path.
- [x] Runtime UI and editor UI. Runtime UI: the new `ui` feature (egui and egui-winit, already dependencies; `editor` enables it) gives every `App` a `RuntimeUi` resource. `App::update` opens one egui pass around `Update` and `PostUpdate`, so gameplay systems draw HUDs and menus with `ui.context()` (or `GameScene::ui()`), and headless runs behave the same. The game window runner feeds window input to that pass, keeps presses the UI uses away from `RuntimeInput`, and paints the result over the scene with the shared `EguiPainter`. Editor UI is the Milestone 6 egui editor. Evidence: `runtime_ui_systems_draw_and_receive_clicks_each_update` (an ECS system draws a label and button, and one press and release reaches it as one click), GPU test `runtime_ui_drawn_by_an_ecs_system_paints_over_the_game_view` (llvmpipe), and a 10 s windowed run of `cargo run --example hybrid_10k`, whose HUD now shows FPS and fallen cubes, with no error output (RTX 3060, Hyprland). The HUD was not checked by eye. Embedded editor Preview does not show `RuntimeUi` yet; that is logged in `docs/editor-overhaul.md`. The Milestone 16 UI toolkit (layout containers, themes, localization) is still to come.
- [x] Scene persistence and live asset reload. Evidence: integration test `vertical_slice_scene_persists_and_reloads_assets_live` imports the courtyard glTF, adds a player body with `PlayerController` and a child camera plus a directional light, saves the scene, and loads it into a fresh `App` with `AssetPlugin`: every name, the controller settings, the camera parent link, the light, and the mesh handles come back. It then rewrites the floor's cooked `.rmesh` file, and the running app's hot-reload scan publishes the new mesh with a higher revision on a later `update`, with no restart. The Milestone 3 hot-reload items cover worker decoding, failed reloads, and GPU resource lifetime. Editing the source `.gltf` still does not re-import it.
- [x] Runtime editing through play/pause/stop. In the embedded preview, a dynamic CPU body falls while playing, holds still while paused, takes a transform edit made through the editor's snapshot-undo path, advances one fixed step from the edited pose on Step, keeps falling from there on Resume, and returns to its authored pose on Stop with the preview's undo entries dropped. Pause and Resume share `set_embedded_preview_paused`. The test found a bug: bevy 0.19 stores resources and observers as entities, so Stop despawned the ones that systems created during the preview and panicked on a stale command. Stop now skips `IsResource` and `Observer` entities. Evidence: `preview_edits_while_paused_drive_the_simulation_until_stop` (panicked before the fix), `embedded_preview_restores_authored_scene_after_simulation`, and `only_entities_created_during_preview_get_runtime_badges`. Native Play runs a separate process and has no live editing; that is the Milestone 20 remote inspector.
- [x] Eco, Balanced, and High quality comparisons. GPU test `quality_profiles_compare_on_one_scene` renders one 32x32 scene (a floor under a red shadowed sun and 40 green point lights) at each profile and prints the comparison. On llvmpipe:

  | Profile | Lights kept (dropped) | Shadow map | MSAA | Mean red | Mean green |
  |---|---|---|---|---|---|
  | Eco | 16 (25) | 1024 px | 1x | 130.0 | 27.9 |
  | Balanced | 32 (9) | 2048 px | 4x | 130.0 | 56.2 |
  | High | 41 (0) | 4096 px | 4x | 130.0 | 69.5 |

  The sun counts toward the light budget. The test checks that the sun looks the same on every profile, that each higher profile keeps more point lights and lights more of the floor, and the shadow map size and MSAA sample count for each profile (MSAA follows the device's sample count). Shadow distance follows `shadow_settings` (30, 50 and 80 m); it is not rendered at range in a test. Per-feature tests: `msaa_smooths_edges_except_on_eco`, `lights_over_capacity_render_first_max_lights_and_report_the_rest`, `culling_path_follows_mode_ownership_count_device_and_quality`, and `auto_quality_resolves_from_device_class_and_concrete_profiles_pass_through`. Frame-time cost per profile is a hardware number and is not recorded here.

### Performance target

Milestone 7 and Milestone 29 have deliberately different hardware targets. The engine vertical slice must run on low-end integrated graphics. Sundering must not: continuous destruction with thousands of debris bodies and GPU navigation has a hardware floor, and pretending otherwise would distort the engine's quality profiles. Neither target is allowed to weaken the other.

- [x] Define a fixed benchmark scene and camera path. Evidence: `spawn_render_benchmark` and `render_benchmark_camera` (`src/runtime/render_benchmark.rs`) build a fixed scene. It has a ground, 40 pillars with spheres in five PBR materials, 1,000 `PhysicsBenchmark::Mixed` GPU bodies with landing events, a shadowed sun, and 24 point lights. The camera path orbits once in 600 frames with two dives, from 40 m to 16 m. Unit tests `camera_path_is_closed_repeatable_and_faces_the_scene` and `scene_spawns_the_same_fixed_content_every_time` pass.
- [ ] Target 1920×1080 at 60 FPS on approximately Intel UHD 620-class hardware using Eco/Auto settings. Not verifiable here: this machine has no integrated GPU. For reference, the benchmark at 1080p Eco reaches a frame-time p95 of 3.0 ms on an RTX 3060 and 62.6 ms on llvmpipe (`benchmarks/README.md`). Run `cargo run --release --example render_bench -- eco` on UHD 620-class hardware to close this item.
- [x] Record CPU frame time, GPU pass time, draw/dispatch count, triangles, memory, visible instances, upload bytes, physics bodies, event/readback bytes, synchronization latency, and overflow. Evidence: `run_render_benchmark` (`src/rendering/benchmark.rs`) records each of these into a JSON `RenderBenchmarkReport`. The report gives frame, CPU and GPU times as mean, p95 and max, and mean GPU time per pass. The GPU test `benchmark_records_every_metric_along_the_camera_path` checks that every counter is populated on a 30-frame run, including the 9 lights Eco drops. The editor profiler shows the same counters live.
- [x] Store performance baselines and reject material regressions rather than relying only on FPS logs. Evidence: `render_bench <quality> <baseline.json>` writes a missing baseline, or compares against an existing one and exits 1 on any regression. `RenderBenchmarkReport::regressions` applies the rules. Work counters may grow at most 10% when extent and quality match. Times are compared only on the same device and driver, and may grow at most 10% and 0.5 ms. Any new overflow is a regression. Unit test `regressions_flag_material_growth_only` covers these rules. `benchmarks/rtx3060-linux-eco.json` is stored, and two later runs reported no regressions against it. CI enforcement waits for the scheduled performance job in the CI matrix.
- [x] Document tested drivers, operating systems, resolutions, and quality settings. Evidence: `benchmarks/README.md` lists the RTX 3060 (NVIDIA 615.71.09) at every profile and llvmpipe (Mesa 26.2.3) on Eco. All runs were 1080p on CachyOS Linux 7.2.8. The README also names the configurations not yet tested: UHD 620, Windows, AMD, and presented frame pacing.

### Fast game creation and agent feedback loop

A small browser game can feel more polished during development because code changes are quick to preview, input can be automated, and the resulting frame is easy to inspect. RustingEngine needs an equivalent **edit → run → observe → revise** loop for native games. This is a workflow and game-feel target, not a requirement to add an AI model to the engine. The detailed backlog is in [`missingFeatures.md`](missingFeatures.md); the items below are the Milestone 7 acceptance path. Later milestones can deepen each subsystem without postponing its first usable slice.

- [x] Provide the first window-free `rusting` CLI slice: `doctor`, `new`, project/scene inspection, stable ID/name/class/component queries, `validate`, and `cook`, using shared project and scene library operations. Evidence: `tests/cli.rs` covers `testGame`, generated projects, malformed input, missing assets, query ordering, JSON errors, and exit codes; `cargo test --workspace` passed, and `cargo test --workspace --features gpu-tests -- --test-threads=1` passed.
- [x] Complete the scriptable project loop with `check`, `run`, and `export` commands. Reuse the editor's build/play/export operations; return the same versioned result envelope and verify the exported game from an isolated folder. See Milestone 21.
  - Verified: `rusting check`, `rusting run [--release] [--ticks N] [--timeout S]` and `rusting export <root> <parent> [--target T]` live in `src/cli.rs` and return the `schema_version` 1 envelope. Export uses the editor's `export_built_game`, now in `src/project.rs`, and the editor export test still passes. `run --ticks N` sets `RUSTING_HEADLESS_TICKS`, which `run_project` reads to call `run_project_headless`. A native export is copied to a fresh temporary folder and must run 60 headless ticks with exit code 0; a cross-target export is not run and carries an `EXPORT_NOT_VERIFIED` warning. `generated_project_checks_runs_and_exports_a_verified_game` (ignored, real cargo builds; run with `cargo test --test cli -- --ignored`) passed: check, a 30-tick run with exit 0, a verified export containing the cooked scene, and a corrupted scene that makes `run` exit 1. `run_and_export_reject_malformed_flags_as_usage_errors` covers flag errors with exit 2.
- [x] Add a public offscreen `capture` command that renders a specified camera and tick to PNG, reports camera and render metadata, and maps picked pixels to persistent scene IDs. Keep CPU-only validation available when Vulkan is absent. See Milestones 4 and 20.
  - Verified: `rusting capture <scene> <out.png> [--camera ID|NAME] [--tick N] [--size WxH] [--pick X,Y]...` (`capture_scene` in `src/cli.rs`) loads the scene, renders every tick up to N offscreen so GPU physics advances, and writes the last frame as a PNG. It reports camera ID, name, projection, transform and world matrix; device, driver, requested and resolved quality, draws, triangles, visible instances and dropped lights; and, per pick, the persistent ID, name, hit distance, world position and rendered RGBA. Picks use CPU rays against mesh bounds (the same method as gameplay clicks), so a rendered ID buffer is still missing for exact silhouettes. `try_init_vulkan_headless` returns an error instead of panicking, and without a device the command still reports the camera and picks with a `VULKAN_UNAVAILABLE` error. Game code is not run. Tests: `capture_without_vulkan_still_reports_camera_and_picks_per_tick` (the device selector is forced to miss; a falling CPU-physics cube is under the pixel at tick 0 and gone at tick 60; also covers an unknown camera and an out-of-range pick) and `capture_renders_a_png_and_maps_pixels_to_scene_ids` (`--features gpu-tests`; a 320x180 PNG whose pixel matches the reported pick color, and camera selection by name and by ID).
- [x] Add input-driven `test` scenarios: named actions at fixed ticks, assertions on reflected game state and events, optional captures, reproducible seeds, and a compact failure trace with the first failing tick. This is the first slice of the broader replay work in Milestone 8.
  - Verified: `src/scenario.rs` defines the JSON `Scenario` (`name`, `seed`, `ticks`, `capture_size`, `steps`) with `press`/`release` of named `ActionMap` actions, `expect` (a JSON pointer into the entity's reflected scene form, with registered components parsed; `equals` with `tolerance`, `greater_than`, `less_than`, and `until` to hold a check across ticks), `expect_events` (collision counts, optionally per entity), and `capture` (offscreen PNG through the shared `HeadlessCapture`, skipped without Vulkan). `run_scenario` sets the new `RandomSeed` resource (splitmix64 indexed by fixed tick and stream, in `rusting-core`), advances exactly one fixed step per tick, stops at the first failure, and reports `first_failure` with tick, step index, message and the observed value. `run_project` runs a scenario when `RUSTING_TEST_SCENARIO` is set and writes the report to `RUSTING_TEST_REPORT`; `rusting test <root> <scenario.json> [--release] [--timeout S]` builds the game, runs it, and returns `SCENARIO_FAILED` with `tick T step S: ...`. Tests: `scenario::tests` (actions change game state at the pressed tick, a falling cube and collision events pass, an out-of-range step and an unbound action fail, an `until` check reports its first failing tick and stops the run there, and the same seed reproduces a seeded game value while another seed changes it) and `generated_game_passes_and_fails_scenarios_with_a_tick_trace` (ignored, real cargo build; passed with a capture step and a failing run that exits 1). Only CPU collision events are countable so far; GPU physics and click events are left to Milestone 8 replay work. Full check passed: fmt, clippy in all three configurations, `cargo test --workspace` (lib 266 passed), and `--features gpu-tests -- --test-threads=1` (lib 328 passed).
- [x] Publish a generated capability and scene/component schema catalog. Give each operation defaults, examples, units, valid ranges, and GPU/readback cost; test examples against the parser and editor reflection registry. See Milestone 9.
  - Verified: `rusting schema [--json]` prints `rusting_engine::schema::catalog()` (`src/schema.rs`, catalog version 1). It lists the 13 CLI operations from one `OPERATIONS` table that now also builds `rusting --help`, each with usage, summary, flag defaults, an example, and GPU/readback cost; all 16 scene entity sections and all 7 registered components, each with summary, GPU cost, default, example, and per-field unit, range and notes; the scenario format; and the 144-byte physics-sync readback size. Defaults are not hand-written: they are the scene form of each component's `Default`, saved through `scene_document` and the component registry. Tests: `every_saved_field_is_documented_and_every_section_is_listed` (the entity keys the scene writer saves equal the documented sections, the catalog's components equal the `SceneComponentRegistry`, every saved leaf field is documented and every documented field exists; removing one field's docs makes it fail), `examples_load_through_the_scene_parser_and_registry_unchanged` (all examples form one scene that parses, loads, and saves back identically, and each component example restores through `set_registered_component`; this caught that `BuiltinPrimitive: Cube` saves as `BuiltinCube`), `catalog_lists_every_operation_with_defaults_and_cost`, and `schema_catalog_examples_parse_and_help_lists_every_operation` in `tests/cli.rs` (every example runs without a usage error and every usage appears in `--help`). The editor's typed inspectors are not part of the catalog; they draw the same registered components. Full check passed: fmt, clippy in all three configurations, `cargo test --workspace` (lib 269 passed), and `--features gpu-tests -- --test-threads=1` (lib 331 passed).
- [x] Support atomic scene patch batches with persistent IDs, dry-run diffs, revision checks, validation, and editor snapshot undo. An agent and a person editing the same open scene must see conflicts instead of silently replacing each other's work. See Milestones 6 and 9.
  - Verified: `rusting scene patch <scene> <patch.json> [--dry-run]` uses `src/scene_patch.rs`. Operations `create`, `set`, `remove` (with optional per-field `expected`), `reparent`, `duplicate` (one entity) and `delete` (with descendants) address entities by persistent ID and fields by JSON pointer into the scene form with registered components parsed; `/id` cannot change. The patched document must deserialize as a `SceneDocument`, pass `validate_scene_structure`, and restore every built-in component through the registry; unknown components are written with a `PATCH_UNVALIDATED_COMPONENT` warning. Nothing is written on any error, and the file is re-read before the atomic write. The result carries `revision_before`/`revision_after` (FNV-1a 64 of the file bytes, `scene_revision`), created and deleted IDs, and per-leaf `{id, name, path, before, after}` changes. `scene inspect` and `scene query` report `revision`; a stale `expected_revision` or `expected` value gives `SCENE_CONFLICT`. The editor records the revision on load, project open and save (`SceneFileRevision`). Each frame in Edit mode, a changed file reloads behind a `remember_scene_before_edit` snapshot when the scene is clean, keeping the selection; with unsaved edits it keeps them and reports the conflict, and `save_editor_scene` returns `SceneIoError::Conflict` once before it overwrites. Tests: `scene_patch::tests` (a six-operation batch and its diff; eight failing batches, including a failure after an earlier operation succeeded, a stale revision, a field conflict, duplicate names, a cycle, a bad type, a bad built-in component value and an `/id` change, all leave the file byte-identical; a dry run does not write), `scene_patch_dry_runs_writes_and_reports_revision_conflicts` in `tests/cli.rs`, and `outside_scene_patch_reloads_under_undo_or_conflicts_with_unsaved_edits` in the editor tests. The editor watch compares the file time first, so a writer on a file system with a coarse clock can be missed until the next change; there is no merge UI yet. Full check passed: fmt, clippy in all three configurations, `cargo test --workspace` (lib 272 passed), and `--features gpu-tests -- --test-threads=1` (lib 334 passed).
- [x] Add a fast preview/restart path for gameplay edits, with measured save-to-first-playable-frame time and clear diagnostics when Rust, shaders, or assets fail to reload. State-preserving Rust reload can follow in Milestone 9; a reliable clean restart is the initial gate.
  - Verified: the clean restart path is the editor's existing Restart (save, stop the game, wait for the worker to confirm the exit, rebuild, relaunch; `restart_waits_for_worker_exit_and_stop_cancels_pending_restart`). A game now prints `FIRST_FRAME_MARKER` (`[rusting] first playable frame after N ms`, to standard error) once, after its first presented frame or first headless tick, counted from `run_project`. The editor's `EditorBuildState` times Play/Restart from the save to that line and writes `First playable frame X s after save (build Y s, game startup N ms)` to the Console. `rusting run` returns `timings` (`build_ms`, `game_first_frame_ms`, `command_to_first_frame_ms`). `crate::project::reload_diagnostics` splits `cargo --message-format short` output into per-file Rust errors and shader errors (`vulkano_shaders` compiles shaders at build time, so they are build errors that name a shader file) and reads `hot reload failed:` lines from the game; the asset server already keeps the last good asset after a failed reload. The editor shows one Console error per Rust or shader error and one per failed asset, and the CLI returns `RUST_BUILD_ERROR`, `SHADER_BUILD_ERROR` and `ASSET_RELOAD_FAILED` diagnostics with the file. Tests: `reload_output_is_split_into_file_diagnostics_and_first_frame_time`, `play_reports_first_frame_time_and_per_file_reload_errors`, and `generated_project_checks_runs_and_exports_a_verified_game` (ignored, real cargo builds; passed with first-frame timings and a Rust error that names `src/main.rs`). Measured here with `rusting run --ticks 1` on a generated project (debug, headless, shared target folder): cold build 3228 ms to first frame, no change 96 ms, after touching `src/main.rs` 638 ms, with game startup 7–8 ms each time. Windowed first-frame time includes Vulkan and window setup and needs a real display to measure. State-preserving Rust reload stays in Milestone 9. Full check passed: fmt, clippy in all three configurations, `cargo test --workspace` (lib 274 passed), and `--features gpu-tests -- --test-threads=1` (lib 336 passed).
- [x] Deliver a small game-feel kit: one reusable camera/controller recipe, input actions, tween/easing, event-triggered sound, burst effects, and a minimal runtime HUD with text and buttons. Expose these through ordinary scene data and Rust APIs so a generated game is inspectable and editable. Expand them in Milestones 9 and 14–17.
  - Verified: the camera/controller recipe is the existing `rusting.player_controller` (`PlayerController` plus a child `Camera`), and input actions are the existing `ActionMap` with the `player.*` defaults. New in `src/runtime/game_feel.rs`, each a registered scene component with a Rust API and a schema catalog entry: `rusting.tween` (`Tween`: Position, Rotation or Scale from `from` to `to` over `duration` after `delay`, eight `Easing` curves, Once/Loop/PingPong), `rusting.sound_cue` (`SoundCue` sends a `SoundEvent{entity, clip, volume}` when its body starts touching another collider or after `trigger()`), `rusting.burst_emitter` (`BurstEmitter` spawns `count` `BurstParticle` entities that copy the emitter's `MeshRenderer`, fly out, fall and shrink, then despawn), and `rusting.hud` (`HudElement` text or button at an anchor, drawn through `RuntimeUi`; a click sends `HudButtonPressed`). Tweens, contact triggers and particles run on the fixed step after CPU physics, and burst directions come from `RandomSeed` indexed by the fixed tick and the emitter's `SceneId`. The engine has no audio output: playing the clip needs an audio crate, which is a new dependency that needs the owner's approval, so a game plays `SoundEvent` itself for now. Tests: `easing_curves_start_at_zero_and_end_at_one`, `tweens_play_once_loop_and_ping_pong_on_the_fixed_step`, `landing_fires_one_sound_and_a_seeded_burst_that_expires` (one sound on landing, five particles within launch speed, identical velocities on a second run, none left after their lifetime, manual triggers), `hud_draws_scene_text_and_reports_button_clicks` (egui pointer press and release over the button), and the schema tests, whose examples load through the scene parser and registry. Full check passed: fmt, clippy in all three configurations, `cargo test --workspace` (lib 278 passed), and `--features gpu-tests -- --test-threads=1` (lib 340 passed).
- [x] Provide a quick 2D creation path for games that do not need 3D: sprites, a simple tile grid, orthographic camera, screen-space UI, and a playable 2D starter scene. Keep this a first slice of Milestone 17 rather than requiring its entire renderer and physics scope.
  - Verified: 2D games live in the XY plane with the existing renderer and CPU physics. Sprites are `MeshRenderer`s with the new `PrimitiveShape::Quad` (upright, facing +Z, UV origin top-left) and an unlit material; the camera is the existing orthographic `Camera`; screen-space UI is `rusting.hud`. New in `src/runtime/two_d.rs`, both registered scene components with schema entries: `rusting.tile_map` (`TileMap`: text rows, one `TileKind{color, texture, solid}` per character; `build_tile_maps` spawns one unsaved quad per tile and one fixed box collider per run of solid tiles in a row, and respawns on change or removal) and `rusting.platformer_controller` (`PlatformerController`: run, jump and gravity through `move_character` on the fixed step, with a 0.1 s jump buffer and ledge grace). `rusting new <parent> <name> --template 2d` writes a side-view platformer: a tile level, a player with an orange sprite and a following orthographic camera, a sensor goal with a burst emitter, and a HUD hint. Generated projects now enable the `ui` feature so the HUD draws. Tests: `tile_maps_spawn_merged_colliders_and_rebuild_or_clean_up`, `platformer_runs_jumps_and_lands_on_tiles`, `the_2d_template_is_a_playable_side_view_level` (the generated scene loads, the player rests on the ground, seven merged colliders, and running with a jump clears the two-tile step and lands more than 3 m right), and the schema tests. `rusting capture` of the generated scene at tick 60 on an RTX 3060 showed the ground, steps, platform and player sprite. The editor does not build tile maps in Edit mode yet (backlog in `docs/editor-overhaul.md`). Full check passed: fmt, clippy in all three configurations, `cargo test --workspace` (lib 281 passed), and `--features gpu-tests -- --test-threads=1` (lib 343 passed).
- [x] Make assets easy to replace and polish: import images and glTF with stable IDs, settings, dependency reporting, reimport, and source/license provenance. Provide starter art-direction presets for lighting, camera, typography, and color that remain editable rather than baking a style into the engine. See Milestones 2, 13, and 16.
  - Verified: `src/asset_import.rs` imports png/jpeg/bmp/tga/gltf/glb files into `<project>/assets/<folder>/` after loading them as the runtime would (image decode, `AssetServer::import_gltf`), copies a glTF's external buffers and images (URIs that leave its folder are rejected), and writes `<file>.rmeta` with a new UUID, `ImportSettings` (`max_size` downscales images), dependencies, an FNV-1a content hash, and `AssetProvenance` (original, author, license, url, generator, notes). `reimport_asset` finds the asset by ID or path, replaces it from `--from`, the recorded original, or itself, and keeps the ID; given provenance fields and settings replace the recorded ones. `list_assets` reports each asset with the project scenes that reference it, and flags missing files and dependencies, invalid or duplicate metadata, files changed since import, files without `.rmeta`, and missing licenses; `validate` now fails on the error-level ones. CLI: `rusting asset import|reimport|list`, in `--help` and the schema catalog. `src/art_direction.rs` has four presets (`daylight`, `golden_hour`, `night`, `flat_toy`) that `rusting preset apply <scene> <name> [--dry-run]` writes as one revision-checked scene patch: the sun (a `Sun` entity is created when the scene has none), ambient and sky light, tone mapping, the new `rusting.background` component (`SceneBackground`, copied into `RenderSettings` each frame), perspective camera field of view, and HUD font size plus the new `HudElement::color`. Reapplying edits the same entities, and the editor sees the change as an outside patch under undo. Tests: `images_import_with_settings_provenance_and_a_stable_id_on_reimport`, `gltf_import_copies_and_reports_dependencies`, `presets_patch_lighting_camera_text_and_background_and_reapply_in_place`, `every_preset_has_a_unique_name_and_sane_values`, `assets_import_list_and_reimport_with_a_stable_id` and `art_presets_list_and_apply_as_scene_patches` in `tests/cli.rs`, and the schema tests. `rusting capture` of the 3D template on an RTX 3060 before and after each preset showed distinct backgrounds, light, and field of view. Editor import dialog, Replace action, and preset picker are in the `docs/editor-overhaul.md` backlog. Full check passed: fmt, clippy in all three configurations, `cargo test --workspace` (lib 285 passed), and `--features gpu-tests -- --test-threads=1` (lib 347 passed).
- [x Expose optional, model-agnostic asset-generator hooks that accept generated images, models, and audio through the normal importer, then validate file type, dimensions/geometry, dependencies, and license/provenance metadata. Give the editor and CLI the same preview and replace operation; game projects must remain usable without an AI service.
  - Verified: `ProjectManifest.generators` (optional, omitted from `project.json` when empty) maps a hook name to `GeneratorHook{command, license, settings}`. `asset_import::generate_asset` runs the command from the project root without a shell, with `RUSTING_PROMPT` and an empty `RUSTING_OUTPUT_DIR`, reads the last stdout line as JSON (`file` plus any `AssetProvenance` field), and passes the file to `import_asset`, or to `reimport_asset` for `--replace`, so it gets the same file type, image size, glTF geometry and dependency, and metadata checks. It records `original: "generator:<hook>"`, the hook as generator, and the prompt as notes. Output without a license from the hook or its config is rejected (`GENERATOR_FAILED`), as are failed runs and unparsable output; unknown hooks give `GENERATOR_UNKNOWN`. The importer now accepts WAV (sample rate and length are read from the `fmt ` and `data` chunks) and Ogg (magic only; a `ponytail:` note defers length to Milestone 15). `import_asset` and `reimport_asset` take `dry_run`, which runs every check on a staged copy in the temp folder and returns the report with `dry_run: true`. CLI: `rusting asset generate <project> <hook> <prompt> [--to F | --replace ASSET] [--dry-run]`, plus `--dry-run` on `asset import` and `asset reimport`, in `--help` and the schema catalog. Editor: an imported file's right-click menu has Replace…, which picks a file of the same type, shows the dry-run preview (size, referencing scenes, license, copied dependencies, or the blocking error) in a modal, and calls the same `reimport_asset` on confirm; the asset server's hot reload picks up the file. The Assets tree hides `.rmeta` sidecars. Nothing at runtime depends on a hook. A Generate dialog in the editor is in the `docs/editor-overhaul.md` backlog. Tests: `audio_imports_and_dry_runs_write_nothing`, `generator_hooks_import_and_replace_through_the_importer` (a `sh` hook: preview, import with hook settings, replace keeping the ID, and unlicensed, silent, failing and unknown hooks), `generator_hooks_preview_and_import_through_the_cli` in `tests/cli.rs`, `replacing_an_imported_asset_previews_then_writes_on_confirm` (the editor's dialog result, preview modal, and Replace click), and the schema tests. Full check passed: fmt, clippy in all three configurations, `cargo test --workspace` (lib 288 passed), and `--features gpu-tests -- --test-threads=1` (lib 350 passed).
- [x] Ship one small, complete starter game and a repeatable acceptance exercise: from a one-paragraph request, create, modify, run, visually inspect, test, and export it using public tools. Record time to first playable frame, manual interventions, failed edits, and whether the requested mechanics and presentation are visible. Keep the CLI, editor, and optional MCP adapter on the same service layer; MCP is an adapter after these operations work locally.
  - Verified: `rusting new --template starter` creates Coin Run (tile map, platformer, five `rusting.pickup` coins, `rusting.counter` HUD with `{name}` placeholders, flag gated by `requires`, "You win!" HUD). `samples/starter_game/request.md` is the one-paragraph request and `win.scenario.json` plays it to the win. Tests: `pickups_count_hide_and_unlock_in_scene_id_order`, `hud_text_fills_counter_placeholders`, `the_starter_template_wins_its_sample_scenario`, plus the ignored `tests/starter_game.rs` exercise (new, preset, query, patch dry-run and apply, validate, run, capture with pick, test, export). `HeadlessCapture` now paints the runtime HUD, checked in the captured win frame. `benchmarks/starter-game-rtx3060-linux.json`: game first frame 20 ms, command to first frame 4.9 s warm (22.3 s cold build), 0 failed edits, 0 manual interventions, 2 corrective edits (background, tone mapping after `golden_hour`), mechanics visible, first coin picked on screen, win frame captured. The MCP adapter stays deferred (missingFeatures.md section 9); the CLI and editor share the library calls. Full check: fmt, clippy default / no-default-features / gpu-tests, `cargo test --workspace` (lib 291), gpu-tests (lib 353).

The fast-feedback bar is informed by [Vite's hot module replacement](https://vite.dev/guide/features) and [Playwright's action, screenshot, and error traces](https://playwright.dev/docs/trace-viewer). These are workflow references, not dependencies or a plan to turn the native engine into a web stack.

### Exit gate

- The vertical slice is playable from a clean checkout on Windows and Linux.
- Its scenes can be edited and saved in the integrated editor.
- Automated smoke, visual, physics, serialization, and performance tests pass.
- Packaging produces a runnable build with required assets and licenses.
- A fresh project can be created, edited, run, captured, tested with named input, and exported from public commands; a human can inspect and undo an agent's scene changes.
- The starter game has visible UI, audio/feedback, and a coherent editable visual style, verified by a capture and a scenario rather than by compilation alone.

## Milestone 8: Deterministic simulation and replay

Goal: make the simulation produce bit-identical results from the same inputs on every supported device, and make any divergence detectable and locatable. Determinism is an engine feature available to every game, not only to Sundering, and it is one of the physics pillars.

### Engine-level opt-in

- [x] Expose `DeterminismMode::{Off, Local, CrossPlatform}` per project, so games that do not need determinism do not pay for fixed-point math or fixed-order reductions. `Local` guarantees identical results on one machine and build; `CrossPlatform` enforces every rule in this milestone.
  - Verified: `determinism` in `project.json` (`Off` default, omitted when Off). `cook_scene` copies the nearest `project.json` value into the cooked scene (scene format 7, new last field `simulation`; cooked v6 is dispatched on its version so its render settings survive), and a replacing load inserts the `DeterminismMode` resource. `Off` runs no check and adds no work. Tests: `cook_copies_project_determinism_and_replacing_loads_insert_it`, `version_six_cooked_scenes_keep_render_settings`. Full check: fmt, clippy in all three configurations, `cargo test --workspace` (lib 295, cli 13), gpu-tests (lib 357).
- [x] Validate at startup that every registered solver, system, and custom compute shader supports the selected mode, and fail with a structured error naming the offender otherwise.
  - Verified: `check_determinism` runs in `load_project_runtime` (windowed, headless, and scenario runs) and returns a `DeterminismError { mode, offenders: [{part, supports}] }` sorted by part name. Parts: `cpu_physics` and each `gpu_solver:*` support `Local` (body-ordered, no order-dependent atomics, float math not pinned yet); a custom shader declares its level with a `// rusting: determinism = <mode>` line and otherwise supports `Off`; game systems declare theirs in the `DeterminismSupport` resource (a second declaration keeps the weaker mode). Undeclared game systems are not detected automatically. `rusting validate` reports one `DETERMINISM_UNSUPPORTED` error per part with the first body's scene location. Tests: `determinism_check_names_every_part_below_the_project_mode`, `shader_pragmas_declare_determinism`, CLI `validate_names_parts_below_the_project_determinism_mode`. Same full check as above.

### Deterministic math

- [x] Choose and document the simulation number format: fixed-point integer math, or strictly constrained IEEE-754 with fast-math disabled, fused-multiply-add behaviour pinned, and operation order fixed.
  - Verified: `docs/determinism.md` chooses constrained IEEE-754 `f32` over fixed-point (keeps the existing solvers; no `shaderInt64` need), lists the correctly rounded operations allowed as hardware ops, the ones replaced by shared routines (division, `sqrt`, `inversesqrt`, `dot`/`cross`/`length`/`normalize`, transcendentals), and the forbidden ones (FMA contraction, fast-math and relaxed precision, reassociation, NaN, subnormals; `CrossPlatform` needs Vulkan float controls for denormal preservation and RTE rounding). It records that the solvers meet `Local` only today. Documentation item; no code change.
- [x] Implement the chosen format once in a shared math module used by the CPU solver and every physics compute shader.
  - Verified: `src/shaders/sim_math.glsl` (`sim_recip`, `sim_div`, `sim_rsqrt`, `sim_sqrt`, `sim_dot`, `sim_length`, `sim_normalize`, `sim_mul`, `sim_mul_transposed`; bit-trick seed, three Newton steps, then the best of the result and its two neighbours, all `precise`) replaces every GLSL division, `sqrt`, `dot`, `length`, `normalize`, and matrix-vector product in the live physics shaders (`physics.comp`, `physics_contacts.comp`, `physics_shapes.glsl`). `runtime::sim_math` is the bit-exact Rust reference. The CPU solver keeps Rust's native `/` and `sqrt`, which IEEE-754 already defines exactly on every supported CPU (documented in `docs/determinism.md`); its transcendental calls move to the module in the transcendental item. Tests: GPU `shader_sim_math_matches_the_rust_reference_bit_for_bit` (64 cases, including one that differs under FMA contraction; removing `precise` makes it fail on lavapipe), unit `round_values_are_exact_and_the_rest_stay_within_two_ulp`; `commands_apply_once_before_the_step_and_reject_stale_bodies` still gets exact `3.0` from `3 / 1`. The routines always run, even with `Off`; they add a few ALU operations per contact; the GPU physics cost has not been re-measured on hardware. Full check: fmt, clippy in three configurations, `cargo test --workspace` (lib 296), gpu-tests (lib 359).
- [x] Forbid relaxed precision and fast-math in simulation shaders. Both remain available to rendering shaders.
  - Verified: every float operation in `physics.comp`, `physics_contacts.comp` (both passes), and `physics_shapes.glsl` is `precise`, with `precise` locals for values that only feed comparisons; `round` became `roundEven`. `simulation_shaders_are_precise_and_never_relaxed` (gpu-tests, needs `glslc`, documented in `docs/dev-environment.md`) compiles those shaders and `sim_math_test.comp` and fails on any float arithmetic without `NoContraction`, any `RelaxedPrecision` decoration, or any GLSL.std.450 call outside the exact set. `simulation_violations_flag_fusable_relaxed_and_inexact_code` feeds the checker hand-written SPIR-V with each violation. Removing the `radius_squared` `precise` makes the shader test fail. Rendering shaders are not checked. Full check: fmt, clippy in three configurations, `cargo test --workspace` (lib 297), gpu-tests (lib 361).
- [x] Replace transcendental functions in simulation paths with tabulated or polynomial implementations that have defined results on every device.
  - Verified: `runtime::sim_math` gains `sin_cos`, `atan2`, and `asin` (Cephes polynomials using only `+`, `-`, `*`, `/`, `sqrt`, and the exact `%`), plus `rotation_from_euler`, `euler_from_rotation`, `rotation_from_scaled_axis`, and `transform_matrix`. The CPU solver uses them for loading body rotations, integrating angular velocity, and writing rotations back; GPU bodies upload `transform_matrix`. The GPU physics shaders call no transcendental function, and `simulation_shaders_are_precise_and_never_relaxed` rejects any GLSL.std.450 call outside the exact set. Tests: `transcendentals_stay_close_to_f64_and_keep_their_bits` (8001 points; within 4e-7 of `f64` and `sin_cos` for `|x| <= 8192`; a checksum of every result bit is pinned and passes in debug and release), `rotation_helpers_match_nalgebra` (within 1e-6 of nalgebra; Euler round trip within 1e-5). Existing CPU physics tests pass unchanged. Not done: the checksum has only run on x86-64 here; ECS readback, player movement, and tween easing moved to `sim_math` in the simulation-state item below. Full check: fmt, clippy in three configurations, `cargo test --workspace` (lib 299), gpu-tests (lib 363).
- [x] Document which engine values are simulation state, which must be deterministic, and which are presentation only and may differ per machine.
  - Verified: `docs/determinism.md` "Simulation state" lists the components, resources, GPU buffers, events, and input that must be deterministic, the presentation-only values, the rule that presentation never writes simulation state, and the known gap (GPU events, proxies, and mirrors arrive one to three frames late). Fixes found while listing: the GPU clock `pc.elapsed` was the frame-timed game clock and is now `fixed_tick * fixed_delta` (GPU test `gpu_timers_count_simulated_ticks_not_the_frame_clock` sets the frame clock to 1000 s and sees the 0.04 s timer fire only from tick 3; it fails on the old code); teleport commands and GPU readback now convert with `sim_math::transform_matrix` and `transform_from_matrix` (round trip checked in `rotation_helpers_match_nalgebra`); player movement and tween easing use `sim_math::sin_cos` and explicit products instead of `powi`. Full check: fmt, clippy in three configurations, `cargo test --workspace` (lib 299), gpu-tests (lib 364).

### Deterministic execution order

- [x] Make broad-phase cell assignment, pair generation, and constraint ordering deterministic under any workgroup or thread scheduling.
  - Verified: found and fixed thread-dependent system order. `FixedUpdate` used bevy's multi-threaded executor, and five engine systems that write `Transform` (player and platformer movement, bursts, burst particles, tweens) had no order against CPU physics or each other. `engine_fixed_update_systems_have_one_order` builds the schedule with ambiguity detection set to error; it failed with 12 conflicts before the fix. The engine's fixed systems are now one chain, and `FixedUpdate` runs on the single-threaded executor, so unordered game systems follow the topological sort instead of thread timing. Already deterministic and now documented in `docs/determinism.md` "Execution order": the CPU solver sorts bodies by `Entity` and pairs into body order (`broad_phase_finds_the_same_pairs_as_testing_every_pair`, `broad_phase_prunes_a_tall_column_without_changing_pair_order`); GPU bodies upload by slot and colliders by entity; GPU contact cells and fallback fill in atomic order but sum in integers, and `gpu_bodies_collide_with_each_other_through_grid_and_fallback` gets the same bits from two runs and from a run with no hash memory. The chain updates existing burst particles before spawning new ones (`landing_fires_one_sound_and_a_seeded_burst_that_expires` caught the other order). Not covered: which events a GPU event-buffer overflow drops (counted and logged). Full check: fmt, clippy in three configurations, `cargo test --workspace` (lib 300), gpu-tests (lib 365).
- [x] Make solver iteration count and convergence criteria independent of timing, frame rate, and hardware.
  - Verified: already true, now documented in `docs/determinism.md` "Execution order": the CPU solver runs a fixed `SOLVER_ITERATIONS` (8) with no convergence exit, sleep counts `SLEEP_STEPS` ticks, the GPU runs one contact pass and one step per tick, and every fixed system (CPU physics, player, platformer, tweens, burst particles) reads `fixed_delta`, never the frame delta. New test `simulation_bits_depend_on_ticks_not_frame_pacing` runs a spinning three-box stack for 120 ticks at one tick per frame and in uneven frames (3, 0.5, 0.5, 0, 5 ticks) and compares every `Transform` and `RigidBody` bit; making CPU physics read the frame delta makes it fail. Not covered: `player_look` and `platformer_jump` sample input per frame (Replay items). Full check: fmt, clippy in three configurations, `cargo test --workspace` (lib 301), gpu-tests (lib 366).
- [x] Apply CPU-to-GPU commands in a deterministic order that does not depend on arrival time within a tick.
  - Verified: found and fixed three frame-dependent paths. (1) `time::advance` added all of a frame's steps to `FrameTime::fixed_tick` before `FixedUpdate` ran, so every step in a multi-tick frame saw the same tick (and tick-seeded bursts repeated); `App::update` now sets each step's tick. (2) The renderer applied a frame's whole command batch before its first GPU step; `GpuPhysicsCommands::apply_ticks` now records the GPU tick each command applies before (stamped after every fixed step and at extraction), the renderer sorts uploads by step and body, and the new push constant `command_first` gives each step its own range. (3) Ticks past `MAX_PHYSICS_STEPS_PER_FRAME` were dropped; they now wait for the next frame. Tests: `fixed_steps_see_their_own_tick_and_stamp_gpu_commands_for_the_next` (a three-tick frame sees ticks 0, 1, 2; stamps are 1, 2, 3 for fixed-step commands and 4 for an `Update` command), GPU `commands_apply_on_their_tick_however_frames_batch_ticks` (commands for ticks 3 and 6 give the same bits with one tick per frame, frames of three, and a ten-tick frame carrying two over; it fails when commands apply at the frame's first step). Documented in `docs/determinism.md` "Execution order". Full check: fmt, clippy in three configurations, `cargo test --workspace` (lib 302), gpu-tests (lib 368).
- [x] Add a seeded, tick-indexed RNG with independent streams per subsystem, and route all simulation randomness through it.
  - Verified: `RandomSeed::value(tick, stream)` (seeded splitmix64, already present) gains `RandomSeed::stream(subsystem, key)`, which hashes the subsystem name (FNV-1a) into the stream so subsystems that pick the same keys stay independent. Bursts, the only engine simulation randomness, now draw from `"bursts"` keyed by `SceneId`, or by entity when there is none; before, every emitter without a `SceneId` shared stream 0 and fired identical bursts. The previous item made the tick index correct in multi-tick frames. Audit: no simulation code reads `rand` (unused dependency), a thread-local generator, `Uuid::new_v4`, or `HashMap` iteration order, and the GPU physics shaders draw no random values. Tests: `subsystem_streams_are_independent_and_fixed` (value pinned), `emitters_without_scene_ids_draw_their_own_bursts` (fails with the old shared stream), `landing_fires_one_sound_and_a_seeded_burst_that_expires` (same seed and tick repeat). Documented in `docs/determinism.md` "Execution order". Full check: fmt, clippy in three configurations, `cargo test --workspace` (core 54, lib 303), gpu-tests (lib 369).
- [x] Make floating-point-to-integer conversions, clamping, and saturation behaviour explicit and identical across backends.
  Verified: `sim_to_int` in `sim_math.glsl` mirrors Rust `as i32` (`sim_math::to_int`: truncate, saturate, NaN to 0); the contact grid's `cell_of` and `to_fixed` use it; `shader_sim_math_matches_the_rust_reference_bit_for_bit` covers NaN, infinities, `±2^31`, `±3e9`, and halves; rules in `docs/determinism.md`. Full check green (lib 303, gpu 369).

### Verification harness

- [x] Compute a world-state hash every tick over all simulation state, cheap enough to leave enabled in release builds.
  Verified: `world_state_hash` runs after every fixed step into `StateHashes` (last 1024 ticks); `state_hashes_cover_every_tick_and_only_simulation_state` (pacing-independent, one-ULP change detected from its tick on, GPU readback `Transform` ignored; fails when readback copies are hashed); release cost about 0.4 ms per tick for 10,000 CPU bodies. CPU-visible state only; GPU buffers are the next item. Full check green (lib 304, gpu 370).
- [x] Include GPU body buffers in the world-state hash: hash each body's `PhysicsState` on the GPU, reduce in slot order, and read it back tagged with its tick.
  Verified: `physics_hash.comp` runs after every GPU tick and sums per-body hashes (state and rules) with integer atomics, so the order cannot change the result (an integer sum, not a slot-order fold); results reach `StateHashes::gpu` tagged with their tick; `commands_apply_on_their_tick_however_frames_batch_ticks` asserts one hash per tick, the same bits for 1/3/8+carry-over batching, and a changed hash from the tick of a one-ULP command change. Full check green (lib 304, gpu 370).
- [x] Add a headless simulation mode that runs with no window, surface, or renderer.
  Verified: `simulate_project_headless` (behind `run_project_headless`, `RUSTING_HEADLESS_TICKS`, and `rusting run --ticks`) builds only the ECS and asset stack, steps one `fixed_delta` per update, warns that GPU bodies stay still, and returns the `App`; `headless_simulation_runs_exact_ticks_from_a_cooked_scene` saves, cooks, and runs a scene for exactly 30 ticks with 30 recorded hashes. Full check green (lib 306, gpu 372).
- [x] Add a determinism runner that executes the same inputs for N ticks twice and compares per-tick hashes.
  Verified: `compare_runs` steps two apps in lockstep and compares every tick's hash; `compare_runs_reports_the_first_divergent_tick_and_body` passes identical runs, finds a velocity change on tick 6, and a seed change on tick 1. In-process only; separate processes come with the configurations item. Full check green (lib 306, gpu 372).
- [ ] Extend the runner across configurations: debug against release, differing CPU thread counts, differing GPU vendors, differing driver versions.
  Partial: `rusting determinism <project> [--ticks N]` builds debug and release, runs each headless in its own process with `RUSTING_STATE_HASH_OUT`, reruns release pinned to one CPU with `taskset` (one bevy and rayon thread), and compares every tick. Starter template: debug, release, and one-CPU runs give the same 120 hashes; a debug-only `1e-4` nudge of `Player` on tick 30 fails with `DETERMINISM_DIVERGED`, tick 31, `Player` and its `SceneId`. `first_divergent_tick` checked in `compare_runs_reports_the_first_divergent_tick_and_body`. Open: GPU vendors and driver versions, which need GPU bodies in the headless hash and real hardware. Full check green (lib 306, gpu 372).
- [x] Report the first divergent tick and the first divergent body, not merely that a mismatch occurred.
  Verified: in process, `compare_runs` reports tick, entity, `Name`, and `SceneId` (test above). Across processes, `rusting determinism` reruns both configurations up to the divergent tick and compares their per-entity hashes (`first_divergent_entity`); the injected nudge above names tick 31 and `Player`. Full check green (lib 306, gpu 372).
- [x] Run the determinism suite in CI on every change to physics, math, or shader code.
  Verified locally: `.github/workflows/determinism.yml` runs on pushes and pull requests touching `src/runtime`, `src/shaders`, `src/rendering`, `src/project_runner.rs`, `crates/rusting-core`, or `Cargo.lock`. It installs lavapipe and `glslc`, runs `cargo test --workspace --features gpu-tests` on lavapipe (bit-for-bit shader math, `precise` scan, batched GPU hashes), then `rusting determinism` for 600 ticks on a fresh starter project. Both steps run here as written under bash: lavapipe (llvmpipe, Mesa 26.2.3) gpu-tests lib 372 passed; the determinism step exits 0. Not yet run on GitHub runners. Full check green (lib 306, gpu 372).

### Replay

- [x] Record match input streams with tick numbers, a seed, and a format version.
  - Verified: `src/runtime/replay.rs` defines the JSON `Replay` (`format_version` 1, `seed` from `RandomSeed`, `start_tick`, per-frame `tick`, `delta_nanos` and `RuntimeInput` stored only when it changes, plus every tick's hash). `App::start_recording`/`finish_recording` hold the recorder outside the World, since a bevy resource allocates an entity and would shift entity order and hashes. `RuntimeInput` now uses `BTreeSet` and derives serde (winit `serde` feature). A game run with `RUSTING_REPLAY_OUT` writes its replay on exit (windowed path not exercised here, no display test). Test: `replays_reproduce_recorded_hashes_and_find_changed_input` records 90 uneven frames with held and pressed keys and stores under 20 inputs. Full check green (lib 307, gpu 373).
- [x] Replay a recorded stream and verify it reproduces the recorded per-tick hash sequence.
  - Verified: `play_replay` checks the format version and start tick, sets the seed, feeds each frame's delta and input, and returns the first divergent tick. The same test round-trips the replay through JSON and reproduces all 140 hashes; removing one jump press reports the tick after that frame (the press is buffered in `Update`), and version 0 is rejected. `RUSTING_REPLAY_PLAY` plays a replay headless and fails with the divergent tick. Full check green (lib 307, gpu 373).
- [x] Support replay seeking by re-simulating forward from periodic full snapshots.
  - Verified: `src/runtime/snapshot.rs` adds `App::snapshot`/`restore`: every entity (resources included) with its registered components, plus the entity allocator's state, including bevy's hidden local free list, read by allocating and freeing, then rebuilt. Unregistered types fail with their names (bevy_ecs `debug` feature). `ReplaySeeker` snapshots every N ticks and seeks by restoring the nearest snapshot into a new app and re-simulating, still checking each tick's hash. Tests: `snapshots_restore_entity_ids_and_state_into_a_new_app` (particles reuse ids at raised generations; the restored app matches ids, allocator and 52 frames of hashes), `snapshots_name_unregistered_types`, and `replays_of_the_starter_template_seek_through_snapshots` (seeks 240, 10, 190, 60, 240 with 4 snapshots, checks the counters, and reports a tampered hash at tick 170). Full check green (lib 311, gpu 377).
- [x] Keep replays independent of render settings, resolution, quality profile, and window size.
  - Verified by construction: playback runs headless with no renderer, and the viewport size that click picking reads is part of the recorded `RuntimeInput`, so the playback window never enters the simulation. GPU physics bodies are not in the world-state hash yet, so replays cover CPU simulation only. Full check green (lib 307, gpu 373).

### Exit gate

- 10,000 GPU bodies simulated for 10,000 ticks produce identical per-tick world-state hashes on at least two GPU vendors, and on both debug and release builds.
- A recorded replay reproduces its original hash sequence exactly.
- A deliberately introduced divergence is reported with its first divergent tick and body.
- If bit-identical cross-vendor results prove unachievable, that result is documented with evidence, and Milestones 19 and 24 proceed with server-authoritative networking without cross-vendor rollback.

## Milestone 9: Scene composition, reflection, and iteration speed

Goal: give the engine Godot's authoring model — reusable scenes, signals, groups, data resources, and fast code iteration — expressed through ECS instead of a node tree.

Depends on: Milestones 1, 2, and 6.

### Reflection

- [x] Add a reflection registry for components, resources, and asset types: field names, types, ranges, defaults, and editor hints, derived from Rust types rather than hand-maintained tables.
  Verified: `src/reflect/` adds the `Reflect` trait and the `reflect!` macro (our own, no new crate). The macro destructures every field and matches every variant, so a description that misses or misnames a field does not compile. Hints are `unit`, `min`, `max`, `doc`, and `color`; defaults come from `Default`. Components register through `register_scene_component` (which now requires `Reflect`); resources and asset types register in `TypeRegistry` (`rusting.render_settings`, `physics_settings`, `random_seed`, `determinism`, and the `rusting.material` asset). All 16 built-in components are described in `src/reflect/builtin.rs`, and `rusting schema` (catalog version 2) derives component fields, resources, and asset types from them instead of the old hand-written table. Tests: `the_macro_describes_structs_newtypes_and_enums`, `builtin_types_are_described_and_listed`, `every_saved_field_is_documented_and_every_section_is_listed` (checks pitch range `-1.55..1.55`, `/tiles/*/solid`, `/alpha_mode/Mask/cutoff`). Full check: fmt, clippy in three configurations, `cargo test --workspace` (core 54, lib 317, doc 16), gpu-tests (lib 383).
- [ ] Drive scene serialization, the Inspector, undo/redo, and animation property tracks from the same registry. Replace the generic JSON inspector path once parity is reached.
  Partial: scene save and load run through the registration's `TypeInfo` (walk, migrate, entity and handle conversion); the Inspector's generic JSON editor (`inspector/json.rs`) is replaced by `inspector/reflected.rs`, which draws typed widgets (numbers with ranges and units that do not clamp saved values on draw, linear and sRGB colors, enums, options, lists with add/remove, and drop-downs of scene objects and loaded assets for references, so a reference is never a half-typed ID) and applies edits through the existing snapshot undo path. `registered_component_field` and `set_registered_component_field` read and write one field by JSON pointer with type checks; this is the path API animation property tracks will use, but animation tracks themselves do not exist yet (Milestone 14 owns them), so this item stays open. Map key add/rename, doc tooltips, and an asset picker for assets not yet loaded are in the `docs/editor-overhaul.md` backlog. Tests: `field_paths_read_and_write_checked_values`, `field_names_become_captions`, `references_start_at_the_first_object_or_stay_empty`, `drawing_without_input_leaves_the_value_unchanged`, `registered_inspectors_draw_and_edit_their_component`. Full check: fmt, clippy in three configurations, `cargo test --workspace` (core 54, lib 317, doc 16), gpu-tests (lib 383).
- [x] Support reflected enums, nested structs, collections, typed handles, and entity references.
  Verified: `TypeInfo` covers unit, struct, and tuple enum variants, nested structs, arrays, `Vec`, `Option`, string-keyed `BTreeMap`/`HashMap`, `Handle<T>` for texture and mesh assets, and `Entity`. An `Entity` saves as the target's `SceneId` and is remapped on load; a handle saves as `{"$asset": path}` and loads the asset. A reference to an entity that is gone or has no `SceneId` saves as `null`; `null` or an ID no object has loads as `None` in an `Option<Entity>` and as `Entity::PLACEHOLDER` otherwise, so deleting a referenced object, a despawned target, or a preview-only object never blocks saving, undo, or loading. Handle assets load in `preload_component_assets` before the open scene is replaced, so a missing one leaves it untouched. Tests: `entity_references_and_handles_save_as_ids_and_asset_paths`, `the_macro_describes_structs_newtypes_and_enums`. Full check: fmt, clippy in three configurations, `cargo test --workspace` (core 54, lib 317, doc 16), gpu-tests (lib 383).
- [x] Reject or migrate renamed/removed reflected fields with a structured, versioned error instead of silently dropping data.
  Verified: unknown fields and variants fail the load with `ReflectError { component, saved_version, current_version, path, problem }` (CLI code `SCENE_COMPONENT_FIELD`); the message suggests a migration. `App::migrate_scene_component` takes `FieldMigration::rename` or `remove`; each raises the component version, saves write `"$version"`, older saves run the missed migrations, and newer ones fail with `NewerVersion`. Tests: `unknown_fields_and_variants_are_rejected_with_their_path`, `migrations_rename_and_remove_fields_and_newer_versions_fail`. Documented in tutorial 3 and `docs/concepts.md`. Full check: fmt, clippy in three configurations, `cargo test --workspace` (core 54, lib 317, doc 16), gpu-tests (lib 383).

### Prefabs and scene instancing

- [x] Instance a saved scene as a child subtree of another scene, keeping a link to its source asset.
  Verified: the built-in `rusting.scene_instance` component (`SceneInstance { source }`, in `src/runtime/scene_instance.rs`) links an object to a scene file, saved relative to the scene that holds it. `load_scene_document` adds the source scene's objects under that object before validation, with IDs from `member_id(root, source_id)` (FNV-1a, stable across loads and platforms) and entity references inside their components pointed at the copies. Added objects carry `InstanceMember` and the `rusting.instance_member` marker, so text saves drop them and keep only the link, while editor undo snapshots and cooked scenes keep them without reading the source again; a cooked game ships without the source scenes. A missing source fails the load with `SceneIoError::Instance` and leaves the open scene untouched. The editor's Assets panel instances a scene by double-click or `Instance in Scene`, through snapshot undo, and refuses the open scene. Changing `source` takes effect on the next load. Tests: `instances_add_the_source_scene_under_the_root_with_stable_ids`, `saves_keep_the_link_and_snapshots_keep_the_members_once`, `cooked_scenes_load_without_the_source_scene`, `instances_nest_and_a_scene_cannot_contain_itself`, `a_missing_source_fails_the_load_and_keeps_the_open_scene`, `an_empty_source_places_nothing`, `scenes_instance_as_one_linked_root_but_not_into_themselves`. Full check: fmt, clippy in three configurations, `cargo test --workspace` (core 54, lib 324, doc 16), gpu-tests (lib 390).
- [x] Store per-instance overrides as property diffs against the source, so source edits propagate to every instance that did not override that property.
  Verified: a text save (`fold_instance_members`) expands each instance's source again and stores, on the root under `rusting.instance_overrides`, a sparse JSON diff per member keyed by source object ID: only changed fields (component fields separately), `{"$removed": true}` for a removed component or map entry, and `null` for a deleted object. Loading merges the diffs into the source as it is now, so unchanged fields follow source edits; deleting an object removes its source children too. Objects the user adds under an instance's objects save normally, because member IDs are stable; if a later source drops their parent they move to the top of the scene instead of failing the load. Asset paths inside diffs are saved relative like every other path. Expanded roots carry `rusting.instance_expanded` with the source they came from, so snapshots never expand twice (also when every member was deleted), and a changed link saves no stale diffs. Tests: `overrides_are_saved_and_source_edits_reach_the_rest`, `a_changed_link_saves_no_overrides_for_the_old_members`, `diffs_merge_back_to_the_current_value`, `override_asset_paths_are_converted`. Full check: fmt, clippy in three configurations, `cargo test --workspace` (core 54, lib 328, doc 16), gpu-tests (lib 394).
- [x] Support nested prefabs and prefab variants (inheritance), with cycle rejection.
  Verified: nested instances expand recursively and a scene that contains itself, directly or through other scenes, fails with `SceneIoError::InstanceCycle` (`instances_nest_and_a_scene_cannot_contain_itself`). A variant is an ordinary scene whose one object instances the base scene, so it needs no new file format: its own edits save as overrides on that object, and a scene that places the variant saves its overrides against the expanded variant. Overrides therefore stack base, then variant, then level, and a base edit reaches every field neither layer changed. `save_scene_variant(base, path)` writes one, and first expands it once, so a missing base or a base that already contains `path` fails without writing. The editor's Assets menu for a `.rscene` file has New Variant, which writes `<name>_variant.rscene` (or `_variant_2`, ...) next to it and selects it; it does not change the open scene, so it adds no undo step. Structure note: the variant's objects sit under its one root, so a placed variant is one level deeper than a placed base. Tests: `variants_inherit_base_edits_under_their_own_overrides` (base, variant and level each change a different field; the base edit shows where no layer overrode it, and variants of themselves or of a scene built from them are rejected with the base unchanged) and `scenes_instance_as_one_linked_root_but_not_into_themselves` (New Variant names and content). Full check: fmt, clippy in three configurations, `cargo test --workspace` (core 54, lib 329, doc 16), gpu-tests (lib 395).
- [x] Revert, apply-to-source, and unpack-instance operations with undo.
  Verified: `edit_instance(document, id, InstanceEdit)` in `src/runtime/scene_instance.rs` works on a document of the open scene, and `id` may be the instance root or any object it placed. `RevertObject` replaces one placed object with its source values. `Revert` replaces all of them, which brings deleted ones back and keeps objects added under them. `ApplyToSource` maps the placed objects back to their source IDs (object references inside components too), restores the member markers of instances inside the source, folds those into overrides, validates and expands the result once (so an added instance that contains the source is rejected before writing), and writes the source file. Before writing, it saves the rest of the open scene's overrides against the old source and then expands again, so other instances pick up the change and keep their own overrides, and the applied instance keeps none. `UnpackCompletely` drops the link and every member marker, including those of instances inside it, and numbers names that now clash. Right-clicking an instance root or a placed object in the Hierarchy shows an INSTANCE section with the four actions. They run as `EntityRequest::Instance` behind the usual undo snapshot and keep the selection. Undo restores the open scene, but it does not restore a source file that Apply to Source wrote. Tests: `revert_resets_one_object_or_the_whole_instance`, `apply_writes_the_source_and_other_instances_keep_their_overrides` (including a source made of nested instances, which stay linked), `unpack_turns_an_instance_into_ordinary_objects` (the scene loads after the source is deleted), and `scenes_instance_as_one_linked_root_but_not_into_themselves` (the editor actions reload and keep the object). Full check: fmt, clippy in three configurations, `cargo test --workspace` (core 54, lib 332, doc 16), gpu-tests (lib 398).
- [x] Instance prefabs at runtime from gameplay code through typed handles.
  Verified: `AssetServer::load_prefab(path)` reads a `.rscene` file once into a `Prefab` asset in `AssetServer::prefabs` and returns a `Handle<Prefab>`; loading the same path again returns the same handle. `spawn_prefab(world, handle, transform)` in `src/runtime/scene_instance.rs` adds an instance root named `"<file stem> #<n>"` at `transform` with the prefab's objects under it, through the same `load_scene_document` path as scene loading (additive), so object references inside the prefab point at the new copies. Root IDs come from a per-world placement counter (`member_id(nil, n)`), so a replay that places the same prefabs in the same order gets the same IDs, and member IDs follow from the root as in saved scenes. A saved scene keeps each runtime instance as a link like any other. Systems call it through `commands.queue(move |world: &mut World| spawn_prefab(world, handle, transform).map(drop))`; a stale handle fails with `SceneIoError::MissingPrefab`. Limits: a changed prefab file is not reloaded while the game runs, runtime prefabs must live under `assets/` to be exported, and each placement scans for the root by `SceneId`. Test: `gameplay_code_places_loaded_prefabs_with_repeatable_ids` (one load per path, placement at the given transform, a second placement queued from commands after the file was renamed away, references pointing at each placement's own objects, identical IDs on replay, links kept on save, missing handle error). Full check: fmt, clippy in three configurations, `cargo test --workspace` (core 54, lib 333, doc 16), gpu-tests (lib 399).

### Signals, groups, and observers

- [x] Add entity-targeted observers that react to component insertion/removal and to typed events, as the ECS equivalent of Godot signals.
  Verified: `src/runtime/signals.rs`. `App::add_signal_handler(name, system)` registers a named Rust system whose input says the signal it answers: `In<Signal<E>>` for a typed bevy `EntityEvent` (sent with `commands.trigger`), `In<Signal<Added<T>>>` or `In<Signal<Removed<T>>>` for component `T` (a despawn counts as a removal). The emitting entity carries a `Connections` component (handler name plus receiver entity; `App::connect` checks both), and one bevy observer per signal type queues the matching handlers in connection order; `Signal` carries source, target and event. Bevy's per-entity `observe` is not used, because its observer entities hold boxed systems that a snapshot cannot copy; `Connections` is plain data, so snapshots and replays keep it, and `App::restore` mutes signals so writing components back runs no handler. Handlers must be registered during setup; a handler that re-emits its own signal does not run inside itself. `SignalError` converts to `AppError` for `Plugin::build`. Tests: `connected_handlers_answer_events_and_component_changes` (typed event through commands, two handlers in order, unconnected entity silent, add and despawn), `snapshots_keep_connections_and_restores_fire_nothing` (fails if the mute is removed), `connections_name_registered_handlers_and_live_entities`. Documented in `docs/concepts.md`. Full check: fmt, clippy in three configurations, `cargo test --workspace` (core 54, lib 336, doc 16), gpu-tests (lib 402).
- [x] Allow scene files to store event-to-handler connections between entities by `SceneId`, with registered Rust handler names, validated at load time.
  Verified: `Connections { list: [Connection { handler, target }] }` is a reflected built-in scene component, `rusting.connections`; the reflection saves `target` as the object's scene ID and loads it back to the entity, so the Inspector, scene patches, `rusting schema`, prefab ID remapping and cooked scenes handle it like any other object reference. `load_scene_document` checks every connection of the objects it spawned, in object-ID order, before replacing the open scene: an unregistered handler or a target that is not in the world fails with `SceneIoError::Connection { object, problem }` and leaves the open scene as it was. Tools that load scenes without the game's code (`keep_unregistered`: the editor, `rusting capture`) skip the check. Test: `scenes_save_connections_by_object_id_and_check_them_on_load` (save writes the target's ID, a new app loads it and the handler runs with the loaded entities, unknown handler and dangling target fail with the object ID and keep the scene, tools mode loads). The schema catalog lists the component with an example (`every_saved_field_is_documented_and_every_section_is_listed`). Full check: fmt, clippy in three configurations, `cargo test --workspace` (core 54, lib 337, doc 16), gpu-tests (lib 403).
- [x] Add entity groups/tags with fast membership queries and editor assignment.
  Verified: groups and tags are the existing object classes (`ObjectClasses`), which scenes already save per object and the Inspector's Classes section already assigns with undo. New in `src/runtime/classes.rs`: the `ClassIndex` resource maps each class name to its members in ascending entity-index order, kept current by `on_insert` and `on_discard` hooks on `ObjectClasses`, so inserts, replacements, removals, despawns and scene loads (including the editor's undo, which reloads the scene) update it. Snapshots skip the index and `App::restore` rebuilds it, because a restore overwrites components in place, past the hooks. Read it with `Res<ClassIndex>` (`members`, `contains`) or `App::class_members`. Editing `ObjectClasses` through `Mut` bypasses the index; this is documented. Test: `class_index_follows_insert_replace_remove_despawn_and_restore`. Documented in `docs/concepts.md`. Full check: fmt, clippy in three configurations, `cargo test --workspace` (core 54, lib 338, doc 16), gpu-tests (lib 404).
- [x] Add a typed scene-tree query API for common "find child / find in group / find by name" operations without hiding ECS queries.
  Verified: `src/runtime/scene_tree.rs`. `SceneTree` is a bevy `SystemParam` built from plain queries over `Name`, `Parent` and `Children` plus `Res<ClassIndex>`. Its methods return only entities, so component access stays in the system's own queries. Methods: `find_by_name` (the lowest entity index among objects with the name, so storage order does not change the answer; scans every named object), `child`, `find_path` (Godot-style `Arm/Hand` with `..`), `parent`, `children`, `descendants` (depth first, in hierarchy order), `name`, `in_class` and `is_in_class`. Test: `scene_tree_finds_children_paths_names_and_classes` (duplicate names, missing child, `..` path, empty path, descendants order, class members); the doc example compiles as a doctest. Documented in `docs/concepts.md`. Full check: fmt, clippy in three configurations, `cargo test --workspace` (core 54, lib 339, doc 17), gpu-tests (lib 405).

### Data resources

- [x] Add user-defined typed data assets (the equivalent of Godot `Resource`/`.tres`), serialized through reflection and editable in the Inspector. Evidence: `DataAsset`, `App::register_data_asset` and `AssetServer::load_data`/`save_data`/`reload_data` in `src/assets/data.rs` store `.rdata` JSON through the reflection walker. Unit test `data_assets_load_share_save_and_reload_through_handles` covers shared handles, nested references, relative paths, scene save and load of a `Handle` field, and reload. `bad_data_files_fail_with_the_reason` covers a wrong type, an unknown field and a reference cycle. The editor's Assets `New` menu and the Inspector's data asset view are covered by `data_assets_are_created_edited_and_reloaded_from_the_editor`. `cargo fmt --all --check`, clippy with `-D warnings` in all three configurations, `cargo test --workspace` (lib 346) and `cargo test --workspace --features gpu-tests -- --test-threads=1` (lib 412) passed. Undo for data asset edits and a picker for unloaded files are in the `docs/editor-overhaul.md` backlog.
- [x] Support shared versus per-instance-unique resource references. Evidence: a data asset handle saves as `{"$asset": path}` when it points at a file and is shared, or as `{"$data": value}` when its value has no file, and each `$data` loads as its own copy (`handle_to_scene`/`handle_from_scene` in `src/reflect/mod.rs`, used by scenes and `.rdata` files). `AssetServer::duplicate_data` makes a unique copy in game code, and the Inspector's `Unique` choice makes one from the current file and edits it in place. Unit test `unique_data_assets_are_private_copies_saved_in_place` covers duplicate independence, a unique value inside a `.rdata` file, a scene with one shared and two unique references loading as one file handle and two distinct copies, and a file reload that leaves the copies alone. Full check passed: fmt, clippy with `-D warnings` in all three configurations, `cargo test --workspace` (lib 347), and gpu-tests (lib 413). Saving a unique value out as a file is in the `docs/editor-overhaul.md` backlog.
- [x] Hot reload data assets into running play sessions. Evidence: `AssetPlugin`'s `poll_asset_loads` scan (every `HOT_RELOAD_SCAN_INTERVAL`, 500 ms) now also calls `DataAssetTypes::reload_changed`. It uses the same file-time check as meshes and textures to find changed `.rdata` files and reads each one again into its existing handle, which raises the asset's revision. Unique copies are left alone. Reloads and failures go through `AssetServer::take_reloaded` and `take_reload_failures`, which the editor Console and `project_runner` (`hot reload failed:`) already report. Unit test `changed_data_files_reload_into_a_running_game` covers a rewrite picked up by `App::update`, a raised revision, the reported path, an untouched unique copy, and a broken rewrite that keeps the last value and reports one failure. Full check passed: fmt, clippy with `-D warnings` in all three configurations, `cargo test --workspace` (lib 348), and gpu-tests (lib 414). Decoding runs on the main thread (`ponytail:` note; data files are small JSON).

### Iteration speed

- [x] Hot reload game Rust code during play, preserving ECS state through the reflection registry. Fall back to a clean restart with a clear message when the change is layout-incompatible. The owner chose a state-keeping restart instead of a dynamic-library game module (no new dependency, no ABI risk; costs one window and Vulkan start per reload). Evidence: the editor's `Reload Code` (`EditorBuildState::request_code_reload`) sends `save-state` on the game's standard input. The game writes a `scene_document` of every object plus its finished `once` keys to `build/code_reload_state.json` (`CODE_RELOAD_STATE_ENV`) and exits. The editor then rebuilds and starts it again, and `run_project` restores that state over the cooked scene with startup systems skipped. `load_scene_document` is transactional, so a saved component that no longer fits its type leaves the fresh scene and prints `code reload restarted clean:` with the reason, which the Console shows as a warning. A failed build keeps the saved state for the next Play, and a game that does not save within 10 s is killed and started clean. Unit test `code_reload_keeps_the_scene_and_runs_the_new_code` runs one game build, saves, then restores into a build with a changed update system: the position from the old code is kept and the new code moves the object on, a runtime-added component keeps its value, and neither the `once` block nor the startup system runs again. A changed component type starts clean with the error naming the component. Editor tests `streamed_game_gets_the_save_command_on_standard_input` and `code_reload_saves_a_running_game_and_resumes_after_a_failed_build` cover the process and state wiring. A manual end-to-end check used a generated project on Wayland with an RTX 3060. The old code moved a spawned cube along x for 1 s, and after the save and rebuild the new code moved it along y from the same x (4.26). The first frame came back 1.35 s after the reload started, and both processes exited 0. This run found and fixed an exit crash in the egui clipboard on Wayland. Full check passed: fmt, clippy with `-D warnings` in all three configurations, `cargo test --workspace` (lib 351), and gpu-tests (lib 417). Automatic reload on save and keeping resources are in the `docs/editor-overhaul.md` backlog.
- [x] Measure and report edit-to-running-game latency for a representative project; keep incremental Debug builds under a documented target. Evidence: `tests/iteration_latency.rs` (ignored; `cargo test --test iteration_latency -- --ignored --nocapture`) creates the starter template project, runs `rusting run --ticks 1 --json` and reports the median of three samples of `build_ms` and `command_to_first_frame_ms` for no change and for an edit to `src/main.rs`. It fails when the Rust edit takes more than the documented 3000 ms target. The target and results are in `docs/dev-environment.md` "Iteration speed". Run on 2026-09-28 (Ryzen 5 7600X, headless, shared `target/cli-games`): first build 40259 ms, no change 129 ms, Rust edit 853 ms (build 824 ms); test passed. Windowed Reload Code measured 1.35 s to the first frame (previous item).
- [x] Define an optional sandboxed WASM scripting host (`rusting-script`) exposing the reflected ECS API, for modding and designer-level logic. Native Rust remains the primary gameplay API. Evidence: new workspace crate `crates/rusting-script` (the owner approved the `wasmi` 2.0 dependency, with its `deterministic` feature). Games opt in with `ScriptPlugin`, which registers the `rusting.script` component (a `.wasm` or `.wat` module path) and runs each script's `start()` once and `update(delta_seconds)` every frame, in entity order. Host functions in module `rusting` (`log`, `entity`, `find`, `action`, `get`, `set`) read and write fields by component name and JSON pointer through `registered_component_field` and `set_registered_component_field`, so values are checked against the reflected type; `transform` addresses the object's transform. Sandbox: no WASI imports, 10 million fuel per call, 16 MiB memory, and a trap, limit or load error stops only that script with a `[rusting] script <path> stopped:` log line. Unit tests in `crates/rusting-script/src/tests.rs`: `scripts_read_and_write_reflected_fields` (start moves the object, update copies another object's field through JSON), `bad_requests_return_a_status_instead_of_stopping_the_script` (unknown component and wrong kind return -3, a missing entity -1, and a changed module path reloads), `sandbox_limits_stop_only_the_offending_script` (an endless loop, a 64 MiB memory, a non-Wasm `.wasm`, and a missing file each stop while another script keeps running), and `removed_scripts_are_dropped`. The host API, limits and examples are in `docs/scripting.md`. The Rust guest example there was not compiled, because the `wasm32-unknown-unknown` target is not installed on this machine. Full check passed: fmt, clippy with `-D warnings` in all three configurations, `cargo test --workspace` (lib 351, rusting-script 4), and gpu-tests (lib 417).
- [x] Deliver project templates for 3D first-person, 3D third-person, 2D platformer, and physics sandbox games. Evidence: `ProjectTemplate` gains `FirstPerson3d` (`first-person`), `ThirdPerson3d` (`third-person`) and `PhysicsSandbox` (`sandbox`) next to the existing `2d` platformer, with `ALL`, `name` and `label`; `rusting new --template` takes every name and the editor's Create Project form has a Template picker (`ProjectManagerState::template`). The player templates build a lit floor with fixed crates and a kinematic capsule `rusting.player_controller` with a child camera; third person adds a visible body and the new `PlayerController::camera_distance`/`camera_height`, which orbit the camera behind the body. The sandbox drops nine dynamic boxes and two balls onto a walled floor under a fixed camera. Unit tests: `third_person_camera_orbits_behind_the_body`; in `src/project.rs`, `template_names_round_trip`, `the_player_templates_stand_walk_and_frame_the_camera` (both player templates land grounded at y 0.9, walk forward with W, and the camera sits at eye height or behind and above), and `the_sandbox_template_settles_its_pile_on_the_floor` (after 20 s every box is still and every body is above the floor and inside the walls); the existing 2D template test still passes. Real GPU (RTX 3060) `rusting capture --tick 120` of each new template showed the expected frames, and `rusting run --ticks 60` of a generated third-person project built and exited 0 (first frame 15 ms). Full check passed: fmt, clippy with `-D warnings` in all three configurations, `cargo test --workspace` (lib 355, rusting-script 4), and gpu-tests (lib 421).

### Exit gate

- A prefab edited in the editor updates every instance that did not override the edited property, and overrides survive save and reload.
- An observer connected in a scene file fires the registered Rust handler at runtime.
- A reflected component added in game code appears in the Inspector, serializes, and animates without editor changes.
- Changing a gameplay system during play reloads it without losing scene state.

## Milestone 10: Advanced rigid-body physics

Goal: exceed Godot's built-in 3D physics feature set on the CPU path while keeping every feature available to hybrid CPU/GPU scenes.

Depends on: Milestones 5 and 8 (all new solver work follows the determinism rules).

### Constraints and articulations

- [x] Joints: fixed, hinge, slider, ball-socket, cone-twist, distance, spring, and a generic 6-DOF joint with per-axis limits, springs, and motors. Evidence: the `Joint` component (`rusting.joint`, `src/runtime/joints.rs`) links a CPU body to another CPU body or to the world. Every `JointKind` is a preset of six axes (locked, free, or limited, each with an optional soft spring and a force-capped motor), plus a cone limit and an anchor-distance limit or spring. The CPU solver builds one velocity row per constraint in joint entity order, warm-starts them from the last step (`PhysicsWorld` state, hashed in entity order), and solves them before contacts with 4 passes per iteration. Jointed pairs skip contacts unless `collide_connected` is set, and a moving or motor-driven side wakes the other. The component is reflected, listed in `rusting schema`, snapshot-registered, and saves `target` as a scene ID. Tests: `runtime::tests::hinge_pendulum_keeps_its_pivot_and_swings_in_its_plane`, `hinge_limits_stop_the_swing_and_motors_drive_it`, `sliders_move_along_their_axis_only`, `fixed_joints_hold_a_cantilever_in_place`, `ropes_cap_the_distance_and_springs_settle_at_their_stretch`, `cone_twist_joints_keep_the_swing_inside_the_cone`, `joints_link_bodies_without_contacts_and_round_trip_through_scenes` (also checks determinism), and `runtime::cpu_physics::joints::tests::swing_twist_splits_rotations_about_each_axis`. Full check passed: lib 363 tests, with `gpu-tests` 429. Limits: CPU bodies only (GPU solvers have no joints), and the anchor rows are iterated rather than solved as one 3×3 block.
- [x] Breakable joints with force/torque thresholds that emit typed events. Evidence: `Joint::break_force` (N) and `Joint::break_torque` (N·m), 0 meaning unbreakable, are reflected and listed in `rusting schema`. After the velocity solve, each breakable joint's load is its accumulated linear-row impulse (force) and purely angular-row impulse (torque) over the step length. A joint past either limit has its `Joint` component removed and sends a typed `JointBroken { joint, target, force, torque }` through `EventQueue<JointBroken>`, in joint entity order. The queue is registered by `App::new` and captured by snapshots. Test: `runtime::tests::joints_break_past_their_force_or_torque_and_send_an_event` covers a hanging ball whose joint breaks at 1 N and reports its weight, then falls; the same joint at twice its weight holds; and a 1 m cantilever breaks below its gravity torque and holds above it. Full check passed: lib 364 tests, with `gpu-tests` 430. Limit: the joint breaks after the step that overloaded it, so that step's impulse is still applied.
- [x] Reduced-coordinate articulations (Featherstone) for robots, chains, and ragdolls that need exact joint limits without drift. Evidence: `rusting.articulation` (`Articulation`, reflected, in `rusting schema`, captured by snapshots) on a root body turns the `Fixed`, `Hinge`, `Slider`, and `BallSocket` joints of its tree (breadth-first, joint entity order) into joint coordinates; other joints stay maximal and couple through the tree. `src/runtime/articulation.rs` reads the coordinates from the link poses each step and places every link by forward kinematics, builds the joint-space mass matrix from each link's Jacobian, and applies gravity and velocity-product forces from a recursive pass, taken at the midpoint speeds so fast chains do not gain energy. Contacts and joint rows on a link use `Gᵀ H⁻¹ G` as effective mass and move the tree through `H⁻¹`; hinge and slider limits, springs, and motors are joint-space rows, and each coordinate is clamped to its limit after integration. A dynamic root floats with six coordinates. Tests: `runtime::tests::articulated_chains_swing_without_their_joints_drifting` (8-link ball-socket and hinge chains, every link exactly 0.5 m from its parent within 1e-4 on every step for 4 s, energy never above 106% of the start, the chain stays in its plane); `articulated_hinge_limits_hold_exactly_and_motors_drive_them` (the angle never passes its −0.5 rad limit by more than 1e-4, where the maximal joint allows 0.05; a motor reaches 2 rad/s); `floating_articulations_land_rest_and_round_trip_through_scenes` (a floating torso with two ball-socket links lands on the ground with its links exactly in place, is bit-identical across runs, and the component round-trips through a scene). Full check passed: lib 367 tests, with `gpu-tests` 433. Limits: dense `H` (O(n³) per step; articulated-body algorithm for trees past ~30 links), no cone limits on ball sockets in a tree, reduced joints do not break, articulations never sleep, and a whipping chain tip overshoots its energy by up to about 5%.
- [x] Stable long joint chains and high mass ratios verified by regression scenes. Evidence: `runtime::tests::long_chains_with_a_heavy_tip_hold_with_substeps_or_an_articulation` hangs a 20-ball ball-socket chain with a 50:1 tip level from a fixed anchor and lets it fall for 5 s. With 8 substeps no link stretches past 0.02 m and every link ends within 0.002 m; with `Articulation` on the anchor and 1 substep no link stretches past 1e-4. One substep stretched the same chain by 6.6 m while it whipped and left it 0.41 m long at rest. `heavy_boxes_rest_on_light_ones_with_substeps` rests 100 kg on 1 kg on the ground for 3 s with 4 substeps: the heavy box never sinks more than 1 cm into the light one and neither slides 1 cm; with 1 substep it sank 6 cm and the stack slid 0.35 m. Limits: CPU solver only; the maximal-joint solver still needs substeps for mass ratios like these.

### Bodies and characters

- [x] Kinematic character controller with slopes, steps, ground snapping, moving platforms, and push interaction with dynamic bodies. Evidence: `PlayerController` walks with `PhysicsWorld::move_character_on_foot` (`max_slope`, `max_step_height`). Ground snapping is new: a grounded walker that is not rising snaps down at most `max_step_height` through `PhysicsWorld::snap_to_floor`, which also holds the body on a ledge's edge while a floor below its center is in reach. Tests: `runtime::tests::walking_players_stay_grounded_down_ramps_and_low_steps` (no airborne step walking down a 20° ramp, 57 of 60 before; none walking off a 0.2 m step; a 1 m ledge still drops the player), `players_step_onto_low_ledges_but_not_high_ones_and_can_leave_crates_alone` (steps and push), `controllers_ride_moving_platforms_and_stop_at_ceilings`, `a_turning_platform_carries_and_turns_its_rider`, `a_platformer_rides_a_platform_whose_parent_moves`, `a_walking_player_keeps_pushing_a_crate_it_did_not_start_in`. Limit: snapping uses the step height as its reach; there is no separate snap length.
- [x] Physics-based (dynamic) character option for games that want fully simulated movement. Evidence: a `PlayerController` on a dynamic `RigidBody` walks by a bounded steering force, jumps, and never tips (its inertia is locked); `dynamic_players_walk_jump_stay_upright_and_shove_by_mass` jumps above 1.5 m and lands, shoves a light crate past z = -3 while a 2000 kg crate stays within 0.3 m, and keeps rotation x/z under 1e-4. Limits: no crouch, swim, dash, or platform riding for dynamic players. Full check 2026-10-10: fmt and clippy x3 clean; lib 794 tests, 891 with gpu-tests; surveyor scenarios pass, unclaimed all but its own red-first w3_drift_unseen.
- [x] Raycast and wheel-collider vehicle models with suspension, tire friction curves, differential, and drivetrain.
  Evidence: `rusting.vehicle` (`src/runtime/vehicle.rs`): one suspension ray per wheel, spring-damper load, a Pacejka-shaped slip curve for slip angle and slip ratio inside a friction circle, an open differential, a flat-torque engine with flywheel inertia, and an automatic gearbox. `raycast_cars_settle_drive_turn_brake_and_replay_exactly` settles a 1200 kg car level at its spring height, drives it 40 m in 5 s while shifting up, turns it right, brakes it to a stop, and replays it bit for bit. Deviation: wheels are rays, not shape-cast wheel colliders, and the ground body gets no reaction force; a limited-slip differential is not modelled.
- [x] Ragdolls generated from skeletons, with a handoff API to and from animation (consumed by Milestone 14). Evidence: the handoff is Milestone 14's `rusting.ragdoll` (limp, active, blend-back). Generation is new: `runtime::ragdoll_bones(world, root, total_mass)` and `GameScene::generate_ragdoll(name, total_mass)` build one `RagdollBone` per `Skin` joint under the character, long to its farthest child joint (half the parent for end bones), radius a quarter of the length, mass shared by volume, and skip bones under 5 cm. Test: `runtime::tests::ragdolls_generate_from_skinned_joints_and_go_limp` (hips, spine and head get bones of 0.3, 0.4 and 0.2 m; a two-bone finger gets none; masses sum to 60 kg and follow volume; the generated ragdoll goes limp with three bodies and the head body falls). Docs: docs/animation.md "Ragdolls". Limits: capsules only, all joints the default cone-twist; the editor bone generator stays deferred in docs/editor-overhaul.md. Full check 2026-10-10: fmt and clippy x3 clean; lib 795 tests, 892 with gpu-tests.
- [x] Convex hulls, triangle meshes and heightfields. Evidence: `ConvexMesh` and `TriangleMesh` colliders came with earlier work (`cpu_convex_mesh_lands_flat_on_a_triangle_mesh_floor`, `a_barrel_with_a_convex_cylinder_collider_rolls`). New: `ColliderShape::Heightfield` reads a `rusting.heightfield` grid (`heights[row][column]`, `spacing`) on the same object and collides as its triangles (static only), cached by entity and change tick; `Heightfield::mesh()` gives the same surface with normals and UVs to draw. Test `bodies_rest_on_flat_heightfield_cells_and_roll_off_its_hills`: a 5 x 5 grid makes 25 vertices and 32 upward triangles, a ball on a flat cell rests at 0.25 m, and a ball on the peak's slope rolls down past x = 2 m. Limits: triangles are tested one by one with no grid lookup, so very large fields cost per-step time; GPU bodies see the field as its bounding box.
- [x] Compound shapes: several child colliders moving as one body. Evidence: `collider_children_move_with_their_dynamic_parent_as_one_body` drops a table top with four child legs; it stands level 1.1 m up on its legs, and a ray at leg height hits the table.
- [x] Per-cell surfaces on heightfields. Evidence: `heightfield_cells_with_their_own_surface_change_friction_and_sound` slides one crate over an ice cell (keeps 3 m/s) and one over plain ground (stops in under a metre); only the ice crate's `with_material: "ice"` cue hears it.
- [x] Per-triangle materials on triangle meshes. `rusting.mesh_surfaces` (`MeshSurfaces { triangles, surfaces }`) tags each triangle of a `TriangleMesh` collider with a `GroundSurface` (friction, restitution, sound material); untagged triangles keep the collider's own values. Importer tags are not needed: glTF primitives already import as separate meshes, each with its own collider. Evidence: unit test `mesh_triangles_with_their_own_surface_change_friction_and_sound` (a crate on the ice half keeps sliding and its `with_material: "ice"` cue sounds; a crate on the untagged half stops in under a metre and stays silent). Full check 2026-10-10: fmt and clippy x3 clean; lib 809 tests, 908 with gpu-tests.

### Materials, forces, and fields

- [x] Physics materials with friction, restitution, combine modes, and per-material contact events. Evidence: `rusting.physics_material` (`PhysicsMaterial { name, friction_combine, restitution_combine }`, `src/runtime/cpu_physics.rs`) sets each side's `CombineMode` (Default, Average, Min, Multiply, Max; where sides differ the later wins, as in PhysX) for the collider's own `friction` and `restitution`; `SoundCue::with_material` fires only on touches of a collider with that material name. Test `physics_materials_combine_friction_and_bounce_and_name_hit_sounds`: a box at 5 m/s slides 4 m with the default geometric mean, 12.7 m with Min on the box, and 1.3 m when the floor's Max outranks it; a 0.9-restitution ball bounces above 1.5 m on a Default floor and not at all on a Multiply floor; a cue with `with_material: "metal"` sounds on a metal floor and stays silent on wood. Limits: GPU bodies ignore combine modes; materials are per collider, not per triangle (that stays with the compound-shapes item).
- [x] Force fields on CPU bodies: directional, radial, vortex and wind with turbulence. Evidence: `rusting.force_field` on a sensor body adds a mass-independent acceleration next to gravity, summed over overlapping fields in contact order; wind gusts are sines of fixed time and position, not an RNG. `force_fields_push_one_way_out_around_and_in_gusts_and_add_up` checks two stacked up fields add to 2.5 m/s after 0.5 s, a negative radial pulls in, a vortex turns counterclockwise, falloff halves the push at half distance, wind gusts away from steady, and a second run matches bit for bit.
- [x] Force fields part 2a: the same fields on GPU bodies. Evidence: `PhysicsWorld::gpu_force_fields` sends field sensors to the render world, which uploads them at binding 14; `physics.comp` adds their push next to gravity for a body whose centre is within its bounding radius of the field. `force_fields_push_gpu_bodies_inside_them` (gpu-tests) lifts a ball at 20 - 9.81 m/s², lets a ball beside the field fall, and blows a ball sideways in gusty wind; `shader_sim_math_matches_the_rust_reference_bit_for_bit` now covers the new GLSL `sim_sin_cos` the wind uses.
- [x] Force fields part 2b: custom field functions. Evidence: `FieldKind::Custom` names a plain Rust `fn(FieldSample) -> [f32; 3]` registered with `App::add_force_field_function`; `force_fields_push_one_way_out_around_and_in_gusts_and_add_up` adds a `spring` field (push = -offset.x) that takes a ball released 2 m out to -0.96 m/s after 0.5 s (-2 sin 0.5) and replays bit for bit. Custom fields stay CPU-only; `gpu_force_fields` leaves them out.
- [x] Buoyancy and drag against water volumes. Evidence: done by earlier work. `rusting.water` (`WaterBody`, `float_in_water` in `src/runtime/water.rs`) lifts dynamic CPU bodies with sphere, box or capsule colliders by their share under the wave surface, damps them with drag and carries them with the current, in body-order-independent fashion; `FluidBlock` volumes float bodies too. Tests: `runtime::tests::a_water_body_draws_one_surface_floats_a_light_ball_and_carries_it`, `a_light_ball_floats_in_a_fluid_and_a_heavy_one_sinks`, `a_light_box_floats_in_a_fluid_and_a_heavy_one_sinks`, `a_light_capsule_floats_in_a_fluid_and_a_heavy_one_sinks`. Limits: no buoyancy torque (boxes do not tilt with waves), mesh colliders and GPU bodies do not float.
- [x] Gravity volumes and per-body gravity scale (planetary gravity, zero-g zones). Evidence: `RigidBody::gravity_scale` already scaled gravity per body. `rusting.gravity_volume` (`GravityVolume { gravity, toward_center, priority }`, `src/runtime/cpu_physics.rs`) on a CPU body with a sensor collider replaces the scene gravity for overlapping dynamic CPU bodies; `toward_center` pulls toward the volume centre (planets); overlaps pick the highest priority, then the first in body order, so contact order does not matter. Registered in scene_file, reflect, snapshot, schema and placement help; docs "Gravity volumes" in `docs/concepts.md`. Not covered: GPU bodies, the player controller, articulated bodies, and waking sleepers when a volume moves onto them. Test: `gravity_volumes_replace_the_scene_gravity_for_bodies_inside` (zero-g, a higher-priority sideways tunnel inside it, a planet pulling from above and below, and an outside body falling). Full check 2026-10-10: fmt and clippy x3 clean; workspace tests pass, also with gpu-tests.

### Solver quality

- [x] Simulation islands with sleeping and wake propagation. Evidence: dynamic CPU bodies sleep after `SLEEP_STEPS` still steps; sleeper pairs and sleeper-fixed pairs are skipped by the narrow phase; a moving body touching a sleeper wakes it, and a body that was asleep last step and moved wakes every sleeper whose bounds it reaches, so waking spreads one contact layer per step (`wake_on_lost_support`). Removing or moving support wakes the pile. New test `runtime::tests::a_hit_on_the_bottom_of_a_sleeping_stack_wakes_and_topples_it` (a five-box stack sleeps; a 50 kg ball knocks out the bottom box and the top box falls); older `cpu_bodies_sleep_when_still_and_wake_on_touch_or_edit` and `sleeping_cpu_bodies_fall_when_their_support_is_removed_or_moved`. Deviation: bodies sleep one by one, not as a whole island graph; a body can sleep while a slow neighbour still moves, and wakes on its next touch.
- [x] Speculative contacts plus swept CCD for fast bodies; bullets through thin walls never tunnel in regression scenes. Evidence: swept CCD (`sweep_fast_bodies`) casts each body that would move past its inner radius from its centre against targets grown by its rounding radius; fixed and kinematic targets stop it at the surface, dynamic ones hand over momentum next step. New regression `runtime::tests::bullets_never_tunnel_through_thin_walls_of_any_kind`: sphere, box and capsule bullets at 60 and 400 m/s, 0, 0.5 and 1 rad from the normal, against a 2 cm box, a 2 cm convex hull, a one-sided triangle quad and a heavy dynamic 2 cm plate: all 72 shots stop within 0.3 m in front of the wall. Older: `fast_cpu_bodies_do_not_tunnel_through_thin_walls`, `a_fast_ball_between_two_dynamic_boxes_pushes_both_back`. Deviation: no speculative contacts; the swept ray covers the acceptance. Limits: rotation is not swept (a fast-spinning thin plank can pass through), and a bullet whose centre ray misses a hull edge it grazes can clip it.
- [x] Substepping with a documented stability/cost trade-off per quality profile. Evidence: `PhysicsSettings::substeps` (reflected, default 1, hashed only when not 1 so older state hashes hold; `ProjectRunner::set_physics_substeps`) runs each CPU fixed step as that many equal steps with contacts, joints and gravity recomputed each time. A pair touching in several substeps sends one `CollisionEvent` per fixed step, impacts from every substep are kept, and bodies count toward sleep only on the last substep. The trade-off table (stretch and time for 1, 4 and 8 substeps) is in docs/concepts.md "Substeps". Deviation: substeps are a scene setting, not part of the render quality profile, because a profile picked from the GPU class would make the simulation differ between machines. Tests: the two regression tests of the item above; the stack test also checks events are not repeated.
- [x] Contact caching and warm starting for stable stacks. Evidence: `PhysicsWorld` keeps last step's impulses per contact pair and manifold point (matched within 5 cm in the first body's frame, saved and hashed in entity order) and starts the solver from them. New regression `runtime::tests::tall_box_stacks_settle_and_sleep_with_cached_contacts`: ten unit boxes dropped 1 cm apart settle and all sleep within 5 s; every box sits within 1 cm of its slop-deep rest height and the top one within 2.5 cm of the column axis. `cpu_boxes_fall_and_rest_in_a_stack_on_static_ground` keeps the three-box case within 5 mm. Limit: the top of a ten-box stack ends about 2 cm off axis from the landing.
- [x] Multithreaded CPU solver, part 1: the narrow phase runs on rayon workers and its results are identical for any worker count. Evidence: `find_contacts` splits broad-phase pairs across the rayon pool (64 pairs at least per task) and collects them in pair order, so the solver sees the same contacts in the same order. `a_box_pile_steps_the_same_on_one_and_many_workers` drops 200 boxes into a heap for 60 steps on pools of 1, 4 and 7 workers and gets one world state hash. Speedup is hardware-only and not measured here. Full check 2026-10-10: fmt and clippy x3 clean; lib 802 tests, 899 with gpu-tests (the gpu run hangs about one time in three on a5f8a36 too, see issues.md).
- [x] Multithreaded CPU solver, part 2: solve independent islands in parallel, still identical for any worker count. Evidence: `solve_free_islands` groups contact points into islands by union-find over moving bodies (fixed and kinematic bodies do not join them) and solves every island without a joint or articulation on rayon workers, each in contact order; the rest are solved in turn with the joints as before. `a_box_pile_steps_the_same_on_one_and_many_workers` now adds 30 separate three-box towers next to the 200-box heap; pools of 1, 4 and 7 workers give one state hash, and it equals the hash of the old serial solver bit for bit. Limits: a single big island (one heap) still runs on one worker; jointed and articulated islands stay serial. Speedup is hardware-only.

### 2D physics

- [x] 2D rigid bodies, part 1: Godot-style axis locks keep circle (sphere), capsule and box bodies in a plane on the shared 3D solver. Evidence: `rusting.axis_lock` (`AxisLock { linear, angular }`, `AxisLock::PLANE_XY`) zeroes locked rows of the world inverse inertia, clears locked velocity components after the velocity solve and restores locked coordinates after position correction. `axis_locks_keep_a_2d_body_in_its_plane_on_a_sideways_slope` puts a locked and a free box on frictionless ramps that fall toward z: the free box slides off in z, the locked one keeps z exactly 0 and its tilt; a locked ball launched at [2, 0, 3] rolls past x 21 with z exactly 0 and spins only about z. Full check 2026-10-10: fmt and clippy x3 clean; lib 806 tests, 904 with gpu-tests.
- [x] 2D rigid bodies, part 2: convex polygon and segment chain shapes. Evidence: `ColliderShape::Polygon` (convex hull, dynamic bodies may use it) and `ColliderShape::Chain` (thin static walls) read the outline of a `rusting.polygon` component (`Polygon2d { points, depth, closed }`), extruded along Z and cached by entity and change tick like heightfields; `Polygon2d::mesh` draws the same shape. `polygon_bodies_slide_down_a_chain_valley_and_settle_at_its_bottom` drops an axis-locked square polygon onto the left slope of a V-shaped chain: it slides to the bottom (|x| < 1, y 0.5 to 1.3, z exactly 0), and a chain on a Dynamic body stays where it was. Full check 2026-10-10: fmt and clippy x3 clean; lib 807 tests, 905 with gpu-tests.
- [x] 2D rigid bodies, part 3: 2D joints and queries (point, ray and shape casts in the plane). Evidence: no new API was needed. A `BallSocket` on a body with `AxisLock::PLANE_XY` is Godot's PinJoint2D, and a hinge whose X axis is world Z already swings in plane (`hinge_pendulum_keeps_its_pivot_and_swings_in_its_plane`). `raycast`, `overlap_sphere` (a tiny radius is a point query) and `shape_cast` see Polygon and Chain colliders. `a_2d_pin_joint_swings_in_plane_and_rays_hit_polygons_and_chains` pins a locked polygon plank by one end: it swings below y 2.1 with its arm at 1 m (±0.05), z exactly 0 and no x or y spin. A ray down hits the chain floor 8 m away, a ray from the side hits the triangle polygon 3.75 m away with an outward normal, and a point overlap finds the triangle. `shape_cast` casts Box, Sphere and Capsule shapes only, as documented. Full check 2026-10-10: fmt and clippy x3 clean; lib 808 tests, 906 with gpu-tests.
- [x] Hybrid 2D GPU bodies with the same condition/event/command bridge as 3D. Evidence: `PhysicsSettings::gpu_locks` (`GameScene::set_gpu_axis_lock`) reaches `physics.comp` and `physics_contacts.comp` as the `lock_mask` push constant; integration, collider pushes and body-body pushes keep locked linear axes, and locked spin is cleared. GPU bodies stay the same 3D bodies, so conditions, events, commands and readback work unchanged. `gpu_axis_locks_keep_2d_bodies_in_their_plane` (lavapipe or a real GPU) runs two overlapping balls offset in z under gravity with a z part for 30 ticks: unlocked they leave the plane; with `AxisLock::PLANE_XY` each keeps its starting z exactly with zero z velocity and zero x and y spin, falls below y -1 and still pushes its neighbour apart in x. Limit: one lock set for every GPU body in the scene; per-body GPU locks need a free word in the body ABI. Full check 2026-10-10: fmt and clippy x3 clean; lib 808 tests, 907 with gpu-tests.

### Exit gate

- A regression scenario suite (pyramid stacks, joint chains, high mass ratios, CCD bullets, ragdoll piles, vehicles on uneven terrain) runs headless in CI with recorded expected results.
- Every joint type, character controller behaviour, and vehicle model has a test and a demo scene.
- CPU results are identical across worker-thread counts.
- 2D and 3D physics run in the same project without interference.

## Milestone 11: Deformable, continuum, and large-scale GPU physics

Goal: provide physics Godot does not have — deformables, fluids, granular media, runtime fracture, and million-body scale — all coupled in one world.

Depends on: Milestones 5, 8, and 10.

### Deformables

- [x] XPBD soft bodies from tetrahedral meshes with volume preservation, attachment to rigid bodies, and tearing. Done in steps 1 to 6 below (solver core, ECS component and two-way rigid attachment, tearing, collision with colliders, scene form, drawing); the GPU path is the separate GPU deformables item.
  - [x] Step 1, the solver core: `runtime::soft_body` (`SoftBody`, `SoftBodySettings`) is a CPU XPBD solver. Every tetrahedron edge is a distance constraint and every tetrahedron a volume constraint, each with its own compliance; small substeps with one Gauss-Seidel pass in ascending index order; a floor plane with static friction; velocity damping; pinned particles (inverse mass 0). `SoftBody::block` cuts a box into cubes of six Kuhn tetrahedra. No atomics, no randomness. Tests: `a_block_is_six_tetrahedra_per_cube_and_fills_its_box` (volume and mass match the box; flat and out-of-range tetrahedra are refused), `a_dropped_jelly_lands_on_the_floor_and_keeps_its_volume` (a 1 m jelly dropped from 1 m rests on the floor, top above 0.8 m, volume never below 95 % of rest, slower than 0.2 m/s after 5 s), `a_pinned_beam_holds_its_end_and_softer_beams_sag_more` (pinned particles keep their exact bits; compliance 1e-4 sags more than 4 times compliance 1e-7), `the_same_start_gives_the_same_bits`. Limits: a stiff 0.8 m cantilever still sags about 5 cm at 10 substeps (XPBD with few passes is soft); the floor is the only collider. Open: ECS component and scene form, rigid-body attachment and collision, tearing, drawing. Full check 2026-10-10: fmt and clippy x3 clean; lib 825 tests, 926 with gpu-tests.
  - [x] Step 2, the ECS component and rigid-body attachment: `SoftBodyVolume { settings, body, attachments }` steps in the `FixedUpdate` chain after `step_fluids` (`step_soft_bodies`, volumes in `SpawnOrder`), and is kept in snapshots. A `SoftAttachment` holds a particle on a point fixed in a rigid body's frame (`SoftBodyVolume::attach_near` attaches every particle within a radius). `SoftBody::step_coupled` solves each attachment every substep as a zero-length XPBD constraint between particle and body with the body's inverse mass and world inverse inertia, so a dynamic body carries the soft body and is pulled and turned by it; kinematic and fixed bodies only carry. Rigid physics has already moved the body through the tick, so the soft step moves it through the same tick again from its start pose and writes back the pose and velocities it ends with (a first version that only added impulses afterwards spun a small box up to 100 rad/s in under a second). Tests: `a_hanging_soft_body_pulls_its_dynamic_body_down_by_its_weight` (an 8 kg jelly hanging from a weightless 8 kg box: after 1 s box and jelly carry the jelly's weight times 1 s of momentum within 5 %, the box falls at least 0.7 times the shared speed, and spins under 0.5 rad/s), `a_soft_body_follows_a_moving_kinematic_body` (attached particles stay within 3 cm of their points on a box moving 1 m/s; the jelly comes along). Limits: attachments to a child body hold nothing; rigid bodies do not collide with soft bodies yet; the soft step does not see the rigid solver's contacts, so a body pulled by a soft body can be nudged into another collider for one tick. Open: scene form, collision with rigid bodies, tearing, drawing, GPU path. Full check 2026-10-10: fmt and clippy x3 clean; lib 827 tests, 928 with gpu-tests.
  - [x] Step 3, tearing: `SoftBodySettings::tear_strain` (default `None`) ends each step with `SoftBody::tear`, which removes every tetrahedron holding an edge stretched past `rest * (1 + strain)` and then every edge no remaining tetrahedron holds, in index order. Particles keep their mass, so pieces that break off fly on. Tests: `an_overstretched_column_tears_and_drops_its_lower_half` (a two-cell column pinned at the top with its bottom thrown down at 20 m/s: with `tear_strain` 0.5 at least 6 tetrahedra tear and the bottom falls below -2 m in 1 s; without it nothing tears and it stays above -0.5 m), `tearing_keeps_only_edges_of_remaining_tetrahedra`. Limits: tearing removes whole tetrahedra instead of splitting particles, so a cut loses the material around it. Open: scene form, collision with rigid bodies, drawing, GPU path. Full check 2026-10-10: fmt and clippy x3 clean; lib 829 tests, 930 with gpu-tests.
  - [x] Step 4, collision with rigid bodies: each substep pushes particles out of sphere, box and capsule colliders (`ObstacleShape`, scaled like rigid physics) along the nearest way out, as the same XPBD position constraint the attachments use, so a dynamic body is pushed back and turned and fixed or kinematic ones only push. `step_soft_bodies` takes every collider with a `PhysicsBody` and no parent whose bounding sphere comes near the volume, sorted by `SpawnOrder`, and skips the bodies the volume is attached to. Tests: `a_jelly_dropped_on_a_fixed_box_rests_on_its_top` (bottom within 0.49..0.52 on a box top at 0.5, box unmoved), `a_ball_thrown_into_a_floating_jelly_shares_its_momentum` (2 kg ball at 3 m/s: total momentum within 0.3 of 6, ball slows below 2 m/s, no particle left inside), `runtime::tests::a_dynamic_box_dropped_on_a_jelly_comes_to_rest_on_it` (a 2 kg box with no ground under it settles on a jelly pad at 0.45..0.6 m). Limits: no friction against colliders; points only, so a thin collider can pass between particles; mesh, heightfield and 2D colliders are ignored; every collider is tested against every volume each tick. Open: scene form, drawing, GPU path. Full check 2026-10-10: fmt and clippy x3 clean; lib 832 tests, 933 with gpu-tests (the net handshake timing test failed once under the serial run and passed alone three times; not this lane).
  - [x] Step 5, the scene form: `rusting.soft_block` (`SoftBlock { spacing, count_x/y/z, density, softness, squash, damping, tear_strain, substeps }`) is reflected, registered for scene files, listed in the schema, the agent guide and the editor's component help. `spawn_soft_bodies` gives each block without one a `SoftBodyVolume` centered on the entity on its first fixed tick, capped at `MAX_SOFT_BLOCK_PARTICLES` (4096) by halving the longest axis. Tests: `runtime::tests::soft_blocks_round_trip_through_scenes_and_land_on_a_box` (the block survives save and load unchanged, becomes 24 particles and 36 tetrahedra with tearing on, and rests within -0.01..0.03 m on a fixed box top at 0), `huge_soft_blocks_are_shrunk_to_the_cap`. Limits: attachments are not in the scene form; play-mode restore starts the block fresh instead of keeping its particles. Open: drawing, GPU path. Full check 2026-10-10: fmt and clippy x3 clean; lib 834 tests, 935 with gpu-tests (the net handshake timing test failed once in the plain run and passed alone; not this lane).
  - [x] Step 6, drawing: `SoftBodyVolume::skin` (`SoftSkin`) draws the faces no two tetrahedra share (`skin_mesh`, wound outward, smooth normals), so a torn body shows its cuts. `sync_soft_skins` runs after the soft step, rebuilds the mesh only when the state hash or tetrahedron count changed, and spawns one skin entity with the fluid surface markers, so the editor hides it and `reap_surfaces` despawns it and frees its mesh when the volume goes. `SoftBlock::visible` (default true) gives a block a shared "Soft Block Jelly" material. Tests: `the_skin_is_the_outer_faces_wound_outward` (12 triangles for one cube, 48 for 2x2x2, every face and normal pointing out), `runtime::tests::a_visible_soft_block_draws_one_skin_that_goes_with_it` (one skin for the visible block and none for the hidden one, it follows the fall, and despawning the block removes the entity and its mesh). Limits: one material per block; no UVs. Full check 2026-10-10: fmt and clippy x3 clean; lib 836 tests, 937 with gpu-tests.
- [x] Cloth with self-collision, wind interaction, attachment/pinning, and tearing. Done in steps 1 and 2 below, on the CPU; the GPU path is the separate GPU deformables item.
  - [x] Step 1, the solver core: `Cloth` (`src/runtime/cloth.rs`) is XPBD cloth over triangles: stretch constraints on edges, bend constraints between the far corners of triangles sharing an edge, per-triangle wind drag along the normal, self-collision through a grid walked in ascending particle order (pairs closer at rest than twice the thickness are skipped), pinning, tearing that removes the triangles around an overstretched edge and relinks the rest, and the soft-body rigid-body anchors and collider obstacles. The substep loop pieces (`predict`, `advance_bodies`, `solve_distances`, `solve_holds`, `finish_substep`) moved out of `SoftBody` into shared functions. Tests in `runtime::cloth::tests`: `a_grid_has_edges_bends_and_its_mass`, `a_sheet_hangs_from_its_pins_without_stretching_and_repeats` (strain under 5 %, same state hash twice), `wind_blows_a_hanging_sheet_downwind`, `self_collision_keeps_two_layers_apart`, `an_overstretched_edge_tears_the_triangles_around_it`, `a_sheet_drapes_over_a_fixed_ball`; the soft-body tests still pass. Full check 2026-10-10: fmt and clippy x3 clean; lib 842 tests (the flaky net handshake test failed once and passed alone), 943 with gpu-tests.
  - [x] Step 2, scenes and drawing: `rusting.cloth` (`ClothSheet`: spacing, `count_x/y`, density, stretch, bend, damping, wind, drag, thickness, `self_collision`, `tear_strain`, substeps, `pin_top`, `holder`, `visible`) is reflected, listed in `rusting schema` and the agent guide, saved in scenes with `holder` remapped, and has inspector help. `spawn_cloths` turns a sheet into a `ClothVolume` (kept in snapshots) in the entity's XY plane; `pin_top` attaches the top row to `holder`, or pins it in place when `holder` is null. `step_soft_bodies` steps soft bodies then cloths in spawn order through one shared `couple` (anchors, nearby colliders, dynamic body write-back), and `sync_soft_skins` draws both through a shared `sync_skin`; `cloth_mesh` is two-sided (front and back vertices with opposite normals). Test `runtime::tests::cloth_round_trips_through_scenes_hangs_on_its_holder_and_drapes` (a flag on a fixed pole round-trips with its holder, keeps its top row on the pole, is blown downwind and draws one 98-vertex two-sided mesh; a hidden tarp drops onto a box without falling through and draws nothing). The schema example uses numbers exact in f32 because components with entity fields save through JSON values, which widen f32. Limits: one material per sheet, no UVs, frictionless contact with colliders. Full check 2026-10-10: fmt and clippy x3 clean; lib 843 tests, 944 with gpu-tests.
- [x] Ropes and cables with rigid-body attachment and correct tension transfer. `rusting.rope` (`Rope`) spawns a chain of sphere beads between a body and a target body or world point, joined by `Distance { min: 0 }` joints, so the rope goes slack but never stretches past its length and pulls on both ends; `RopeState` lists the beads for drawing. Built from CPU rigid bodies, not an XPBD deformable; the GPU deformable path is its own item. Evidence: unit test `ropes_hold_weights_go_slack_and_tow_bodies` (a 5 kg weight hangs at the rope's length below a world hook, a kinematic tug tows a crate at the rope's length behind it, and a weight kicked upward rises past the slack instead of being pushed back). Full check 2026-10-10: fmt and clippy x3 clean; lib 810 tests, 909 with gpu-tests.
- [x] GPU execution path for deformables with the same event and readback bridge as rigid GPU bodies. Done in Steps 1 to 6 below: one bit-exact kernel for cloth and soft bodies, stepped from the frame loop, read back by `PhysicsId` and tick, with floor contact through `GpuPhysicsEvent`.
  - [x] Step 1, the cloth kernel: `src/shaders/compute/cloth.comp` runs XPBD cloth steps (gravity, damping, pins, stretch and bend constraints, floor) in one workgroup. `runtime::gpu_cloth` packs a `Cloth` into one word buffer with its constraints greedily colored so no two of a color share a particle; the shader solves each color in parallel between barriers, and `step_on_cpu` is the bit-exact Rust reference through `sim_math`. No atomics. Tests: `the_shader_matches_the_cpu_reference_bit_for_bit` (gpu-tests: 81 particles, 60 steps of 8 substeps with the floor hit, equal words), `colors_never_share_a_particle_and_the_reference_hangs_the_sheet` (120 steps, pins stay, bottom near 1 m, repeat gives the same words); `cloth.comp` added to `simulation_shaders_are_precise_and_never_relaxed`. Full check passed. Limits: one workgroup of 256 lanes (up to 4096 particles); wind, self-collision, tearing, anchors and soft-body tetrahedra stay CPU-only; its results differ from the CPU `Cloth::step` (color order, `sim_math`). Open: dispatch from the renderer through the physics readback bridge and events.
  - [x] Step 2, the runner: `rendering::gpu_cloth::GpuClothRunner` submits packed cloth words to a queue and returns at once; `take_completed` hands back only submissions whose fence has signaled, each as a `GpuClothState` tagged with the body's `PhysicsId` and the fixed tick it reached, the same never-wait contract as the rigid readback (`in_flight`, `block_until_complete` for tools and tests). Test `submissions_return_cpu_reference_bits_tagged_with_body_and_tick` (gpu-tests: two chained 30-step submissions in flight at once, both equal to `step_on_cpu` with their id and tick, nothing returned twice); it replaces Step 1's `the_shader_matches_the_cpu_reference_bit_for_bit`. Full check passed. Limits: one host-visible buffer per submission; not yet driven by `SceneRenderer` or the ECS. Open: run `ClothVolume`s marked for the GPU from the frame loop, mirror their particles back, and route cloth events through `GpuPhysicsEvent`.
  - [x] Step 3, cloth in the frame loop: a `GpuCloth` component moves a `ClothVolume` off the fixed-tick solver (`step_soft_bodies` skips it). `rendering::gpu_cloth::service_gpu_cloths`, called once per frame by the game window and `HeadlessCapture` next to the rigid readback, writes finished particles back into the volume (so its skin draws them), records the tick they show in `GpuCloth::tick`, gives the entity a `PhysicsId`, and submits each idle cloth from that tick up to the current fixed tick, one submission in flight per cloth. Test `a_gpu_cloth_follows_the_fixed_tick_with_the_cpu_reference_bits` (gpu-tests, `HeadlessCapture`: 12 frames, the cloth trails the fixed tick by at most 4 ticks, and its positions and velocities equal one `step_on_cpu` run of the same number of steps, so uneven batches give the same bits; the free edge swings down). Fix found on the way: a signaled submission must be waited with a zero timeout before its buffer can be read. Full check passed (gpu-tests run 947 ok); `net::tests::a_handshake_must_arrive_whole_in_time_and_stay_small` (a timing test outside this lane) failed twice under the parallel CPU run and passed alone. Limits: the volume shows the cloth one to three frames late; attachments, wind, self-collision and tearing are ignored while `GpuCloth` is on; not saved in scenes or snapshots. Open: cloth events through `GpuPhysicsEvent`, soft-body tetrahedra on the GPU.
  - [x] Step 4, cloth events: `GpuCloth::floor_event` names a registered `GpuEventId`. `rendering::gpu_cloth::apply_gpu_cloth_states` (run by `service_gpu_cloths` on each finished submission) finds the lowest-index free particle on the floor (`runtime::gpu_cloth::floor_contact`, read from the finished words, no atomics) and, when the cloth was not already on the floor, sends a `RawGpuPhysicsEvent` with the cloth's `PhysicsId`, the tick reached and a `Contact` payload (position, particle index) through `route_gpu_physics_events`, so gameplay reads it from `EventQueue<GpuPhysicsEvent>` like rigid GPU events. `GpuCloth::on_floor` keeps the state between submissions. Tests: `a_cloth_reaching_the_floor_sends_its_event_once` (an airborne state sends nothing, two landed states send one event at tick 120 with the entity, `Contact` kind and y 0), `the_first_free_particle_on_the_floor_is_the_contact`. Full check passed (gpu-tests run 949 ok); `net::tests::a_handshake_must_arrive_whole_in_time_and_stay_small` again failed once under the parallel CPU run and passed alone. Limits: one event kind (first floor contact, sent on enter); contact inside a batch is seen only at its end tick.
  - [x] Step 5, soft-body tetrahedra in the kernel: `cloth.comp` gains a second colored constraint pass, tetrahedron volume constraints (four corners, rest volume, alpha), after the distance colors, so one kernel steps both `Cloth` and `SoftBody`. `runtime::gpu_cloth::pack_soft_body` packs a soft body's edges and tetrahedra (both colored greedily, 64 colors at most), `unpack_soft_body` reads it back, and `step_on_cpu` mirrors the volume pass bit for bit (`1/6` written as the same float bits on both sides, cross products and dots in a fixed order). No atomics. Tests: `a_soft_block_steps_on_the_gpu_with_the_cpu_reference_bits` (gpu-tests: a 4x4x4-cell block, 90 steps of 10 substeps, landing on the floor, equal words), `the_reference_drops_a_soft_block_that_keeps_its_volume` (120 steps, rests at y 0, volume within 2 % of rest, repeat gives the same words). Full check passed (gpu-tests run 951 ok); the `net` handshake timing test failed once under the parallel CPU run and passed alone. Limits: anchors, obstacles and tearing stay CPU-only; results differ from `SoftBody::step` (color order, `sim_math`). Open: a `GpuSoftBody` component stepping `SoftBodyVolume`s from the frame loop like `GpuCloth`.
  - [x] Step 6, soft bodies in the frame loop: `GpuSoftBody` (the `GpuCloth` component under a soft-body name) moves a `SoftBodyVolume` off the fixed-tick solver (`step_soft_bodies` now skips marked soft bodies too); `service_gpu_cloths` packs either volume kind and `apply_gpu_cloth_states` writes either back, with the same tick tracking and floor event. Test `a_gpu_soft_body_follows_the_fixed_tick_with_the_cpu_reference_bits` (gpu-tests, `HeadlessCapture`: a 3x3x3-cell block, 12 frames, trails the fixed tick by at most 4 ticks, positions and velocities equal one `step_on_cpu` run, the block falls). Also moved `service_gpu_cloths`'s doc comment back onto it (Step 4 had left it on `apply_gpu_cloth_states`). Full check passed (gpu-tests run 952 ok); the `net` handshake timing test failed once under the parallel CPU run and passed alone. Limits: as Steps 1 to 5 (one workgroup, no anchors, obstacles, wind, self-collision or tearing on the GPU; not saved in scenes or snapshots).

### Fluids and granular media

- [x] Particle-based fluid (PBF or SPH) with two-way rigid-body coupling and buoyancy. Done in the steps below; the GPU port, surface and FLIP option are their own items.
  - [x] Step 1, the solver core: `runtime::fluid` (`Fluid`, `FluidSettings`) is a CPU position based fluid with a hash-grid neighbor search, sorted neighbor lists, Jacobi passes into separate buffers, XSPH viscosity, a box container and `state_hash`. No atomics, no randomness. Tests: `the_same_start_gives_the_same_bits` (30 steps, equal hashes), `a_column_falls_settles_and_stays_inside_the_container` (768 particles, 240 steps, all finite and inside the box, top below 1 m, speed below 3 m/s), `a_settled_pool_keeps_its_density_near_rest` (mean density ratio in 0.5 to 1.6). Full check passed. Limits: the pool settles about 1.6 times denser than rest (walls have no ghost particles and only over-density is pushed apart); artificial pressure is off (`SCORR`, its scale was wrong for these units); results are deterministic for one particle order but not invariant under reordering the array (float sum order). Open: scene format, rigid-body coupling, buoyancy, rendering, GPU port.
  - [x] Step 2, the ECS component: `FluidVolume { settings, fluid }` steps in the `FixedUpdate` chain after CPU physics (`step_fluids`). Test `runtime::tests::fluid_volumes_step_with_the_fixed_tick` (64 particles fall 0.3 m or more in 30 ticks). Full check passed. Limits: runtime only, made by game code; not reflected, not saved in scenes, not in snapshots.
  - [x] Step 3, drawing: `FluidVolume::visual` (a `MeshRenderer`) makes `sync_fluid_visuals` keep one entity per particle, moved each fixed tick, and despawn them when the visual is removed. Test `runtime::tests::fluid_volumes_with_a_visual_own_one_entity_per_particle` (8 particles give 8 renderers; removing the visual gives 0). Full check passed. Limits: one entity per particle, so a few thousand particles is the practical ceiling until fluids get an instanced draw path or a surface mesh; the particle entities carry a `FluidParticle` marker, and the editor Hierarchy and Scene View picking skip them (tests `editor::tests::fluid_particles_are_not_listed_in_the_hierarchy`, and the marker count in the visual test; full check passed with all three feature sets).
  - [x] Step 4, rigid-body coupling and buoyancy: `couple_fluids` runs before `step_fluids`. A dynamic body with a sphere collider gets an upward force of rest density times the sphere volume its overlapping particles fill times gravity, plus drag, and particles inside the sphere are pushed out to its surface. Bodies and volumes go in entity order. Test `runtime::tests::a_light_ball_floats_in_a_fluid_and_a_heavy_one_sinks` (a 7 kg ball of 0.15 m radius floats at y 0.25 in a pool about 0.3 m deep; a 50 kg ball rests on the floor at y 0.145). Full check passed. Box colliders work the same way (rotation and scale respected; particles leave through the nearest face). Test `runtime::tests::a_light_box_floats_in_a_fluid_and_a_heavy_one_sinks` (5 kg floats above a 100 kg box resting on the floor); full check passed. Capsules too (test `a_light_capsule_floats_in_a_fluid_and_a_heavy_one_sinks`; the capsule is a core segment along its local Y with a radius). Limits: sphere, box and capsule colliders only (meshes are skipped); every body checks every particle (O(bodies x particles)); the force is analytic, so the fluid does not push the body sideways and the body's momentum reaches the fluid only through displacement; a body far lighter than the water it displaces is launched hard (that is the physics, but the explicit step is stiff).
  - [x] Step 5, the scene component: `rusting.fluid_block` (`FluidBlock`: spacing, `count_x/y/z`, `container_half_extents`, iterations, viscosity, `visible`) is reflected, listed in `rusting schema`, saved in scenes and registered for snapshots. `spawn_fluid_volumes` turns each block into a `FluidVolume` (box centered on the entity, particles on its floor, spheres with the fallback material when visible). Test `runtime::tests::fluid_blocks_round_trip_through_scenes_and_become_volumes` (a 3x2x4 block round-trips through a scene document and becomes 24 particles in a box whose floor is at y 1.5). Full check passed. Limits: the particles themselves are not in scenes or snapshots, the editor shows no fluid preview until play. (Snapshots now do keep the fluid: see Step 6.)
  - [x] Step 6, review fixes. `couple_fluids` visits bodies and volumes in `SpawnOrder` (entity order only for objects without one), and returns at once with no volume. `FluidParticle(owner)` names the volume that made a particle: removing a volume despawns its particles, and a copied volume makes its own. `FluidVolume` and `FluidParticle` are registered for snapshots, so a restore resumes the same fluid. Cooked scenes are format 8 (`SCENE_FORMAT_VERSION`); v7, v6 and v5 files load through legacy material structs. Tests: `despawning_a_fluid_volume_removes_its_particle_entities`, `a_copied_fluid_volume_gets_its_own_particle_entities`, `fluids_snapshot_and_restore_to_the_same_future`, `version_seven_cooked_scenes_read_materials_without_the_new_fields`. Also fixed from the same review: swapchain image count clamps to the surface maximum (`probe_swapchain_images`); the CCD sweep adds its pad only for grown shapes; `slide` measures step height at the contact position; a `push_bodies: false` player keeps its contacts (crates rest on it, `a_crate_dropped_on_a_player_that_pushes_nothing_rests_on_it`) but shoves nothing; the slow-motion clamp scales with `time_scale` and replay skips it (`update_exact`); `set_scene` refuses the root path; `schema::defaults` is cached and its `mesh_renderer` default comes from `MaterialAsset::default()`; whole numbers compare exactly in scenario `equals` (`whole_numbers_above_the_f32_range_match_exactly`); headless ms per tick excludes scene load; reflection probes and lights sort by entity index; `pointer` listed under scenario steps. Full check: fmt, clippy in all three configurations, `cargo test --workspace` (lib 445, cli 13), gpu-tests (lib 520, cli 13).
  - [x] Material names. `MaterialAsset::name` (reflected, scene-saved; cooked format 9, format 8 loads through `LegacySceneDocumentV8`; glTF import keeps the glTF name). Mesh Renderer combos show names or "Material N"/"Mesh N". Tests: `version_eight_cooked_scenes_read_materials_without_a_name`, `material_names_survive_the_scene_form`. Full check in the same run as the Project Settings item.
  - [x] Fluid surface. `fluid_surface::surface_mesh` splats particles on a grid in index order and extracts the surface by marching tetrahedra (outward faces from the density gradient); `sync_fluid_surfaces` keeps one entity and one mesh per volume and rewrites the mesh each fixed tick (the renderer re-uploads on the asset revision). Tests: `a_block_of_particles_gets_one_closed_outward_surface`, `the_surface_is_reproducible_and_empty_fluid_still_draws`, `a_fluid_surface_is_one_entity_whose_mesh_follows_the_particles`. How it looks on screen is not judged here. Deferred: GPU surface pass, screen-space fluid, shared vertices.
  - [x] Water body and opacity 0. `rusting.water` (`runtime::water`): `sync_water` rewrites a wave grid mesh each fixed tick, `float_in_water` gives buoyancy, drag and a current to dynamic bodies. Tests: `waves_are_reproducible_and_bounded`, `the_mesh_grid_matches_the_rectangle`, `a_water_body_draws_one_surface_floats_a_light_ball_and_carries_it`. The scene shader discards Blend fragments with alpha 0 and no transmission; gpu test `a_blend_material_at_opacity_zero_draws_nothing` (the clear-glass test now uses alpha 0.02). Deferred: rotated water, wave torque on boxes, shore foam, river splines, water material look (no screenshot taken).
  - [x] Editor redesign, part 1. The Project panel is a Project Settings page (`src/editor/project_settings.rs`) built from new `gui_elements::kit` widgets: category rail (Overview, Rendering, Display, Export, Diagnostics), cards, setting rows, switch, segmented control, stat tiles, CPU frame bar. The Console has a framed toolbar, striped rows and level stripes. Opacity below 1 turns an Opaque material into Blend. Add Component is a searchable grouped picker (`inspector/add_component.rs`); Inspector sections are bordered cards. Test `project_settings::tests::every_category_draws_its_cards`; the two editor text tests were updated for the new layout. Full check passed: fmt, clippy in all three configurations, `cargo test --workspace` and with gpu-tests. Not judged by eye: no screenshot was taken, so spacing and colors need a look in the editor.
  - [x] Two-way momentum. Step 4 only moved particles out of a body, so a stream could not push a body and a moving body did not carry the fluid. Now each particle pushed out of a sphere, box or capsule also trades momentum with the body along the way out (`fluid::trade`): a particle closing on the body leaves at the same speed along the normal as the body, and the body takes the momentum the particle lost (particle mass over body mass), so total momentum is kept. Particles in fixed order, bodies in `SpawnOrder`, as before. Tests: `a_flowing_fluid_pushes_a_floating_ball_along` (a pool flowing at 2 m/s moves a floating 7 kg ball over 0.05 m in 12 ticks, a still pool under 0.02 m; without the trade the ball moved 0), `a_trade_keeps_momentum_and_only_acts_on_closing_particles`; the float and sink tests still pass. Full check passed (gpu-tests run 954 ok). Limits: linear momentum only (no torque from the fluid, no body spin given to it).
- [x] Grid-based or hybrid (FLIP/APIC) option for large water volumes, chosen by the simulation class. `FluidSettings::solver` (`FluidSolver::Particles` or `Grid`, reflected and saved as `solver` on `rusting.fluid_block`) picks the solver. `Grid` is FLIP (`runtime::flip`): particles splat their velocities onto a staggered grid of cells twice the particle spacing over the container, a Gauss-Seidel pressure solve (15 sweeps per density iteration) makes the water cells free of divergence, and the particles take 95 % of the grid's velocity change plus 5 % of its velocity. A cell holding more than its rest share of eight particles asks for outflow (drift correction), so the pool keeps its volume. No neighbor search, so it carries far more particles than the position based solver. Particles splat in index order, cells sweep in index order, no atomics or randomness. Coupling, buoyancy and the surface work unchanged on its particles. Tests: `flip::tests::a_grid_column_spreads_over_the_floor_and_keeps_its_volume` (a 1 m column of 1280 particles settles with mean height in 0.06 to 0.12 m, about 0.08 m expected, all inside, under 1.5 m/s after 4 s), `the_same_grid_start_gives_the_same_bits`, `a_light_ball_floats_in_a_fluid_and_a_heavy_one_sinks` (now run on both solvers), `fluid_blocks_round_trip_through_scenes_and_become_volumes` (saves `solver: Grid`). Full check passed (gpu-tests run 987 ok). Review fix: the grid holds at most `MAX_CELLS` (2^21) cells; a bigger or infinite box gets coarser cells instead of an unbounded allocation (test `a_huge_box_gets_a_coarser_grid_not_a_huge_one`). Full check passed (gpu-tests run 988 ok). Limits: the grid does not see bodies, so a body must be larger than a cell (the float test uses 0.05 m spacing for a 0.15 m ball; at 0.1 m spacing the hole around the ball stays open and it sinks); the container is the only solid; PIC/FLIP, not APIC; CPU only. The roadmap's "simulation class" is read as the fluid's own solver choice, not the rigid-body `SimulationClass`.
- [x] Granular material (sand, gravel, rubble) with piling and angle-of-repose behaviour. `FluidSolver::Granular` (`runtime::granular`) steps a fluid volume's particles as grains of diameter `spacing`: position based contacts solved pair by pair (Gauss-Seidel in index order, after averaged Jacobi corrections let heaps slump) push overlapping grains apart and cancel their sliding by Coulomb friction (`FluidSettings::friction`, also `friction` on `rusting.fluid_block`, default 0.6); the container floor and walls hold grains with the same friction. Friction is what keeps a slope. Coupling, buoyancy forces and drawing use the same volume path. Tests: `grains_pile_into_a_heap_and_friction_steepens_it` (a 1 m column of 980 grains collapses for 6 s into a heap whose 95th-percentile height over reach is 0.33 with friction 0.6 and 0.17 without, mean speed under 0.1 m/s), `the_same_grains_give_the_same_bits`, `fluid_blocks_round_trip_through_scenes_and_become_volumes` (saves `solver: Granular`, `friction: 0.9`). Full check passed (gpu-tests run 990 ok). Limits: round grains with sliding friction only (no rolling resistance or cohesion, so the angle tops out near 30 degrees); bodies push grains out and trade momentum with them as with water, but get no buoyancy from grains (a body rests on sand only through those contacts, not tested on its own); the visible surface uses the water material; at most a few thousand grains (neighbor search per step); CPU only.
- [x] Fluid surface extraction for rendering through extraction, never by rendering reading solver buffers directly. Already true since the Fluid surface step: `sync_fluid_surfaces` rebuilds a mesh asset from the particles each fixed tick, the surface entity has a plain `MeshRenderer`, and `RenderExtract` hands the renderer an `ExtractedRenderable` (mesh handle, material, transform) like any other object; the renderer re-uploads the mesh when its asset revision rises. Nothing in `src/rendering`, `src/shaders` or `src/bin` names a fluid type (checked with grep). New test `a_fluid_surface_reaches_rendering_only_through_extraction` (with `RenderExtractPlugin`: one extracted renderable with the surface material; five ticks later the same handle, a higher mesh revision). Full check passed (gpu-tests run 991 ok). Limits: the CPU builds the mesh and uploads it each tick (no GPU surface pass yet); grains and water share the water material.

### Fracture and destruction

- [x] Pre-fractured assets authored in the editor (Voronoi and slicing) with connectivity graphs.
  Evidence: `rusting.fracture` (`Fracture`: Voronoi `pieces` around points from the `seed` stream, then `slices` planes) is a reflected component the inspector edits and scenes save; `Fracture::fracture(mesh)` returns convex pieces (fit for `ConvexMesh`) and the links of pieces sharing a face, capped at 256 pieces. Tests in `runtime::fracture`: two slices quarter a box into 4 pieces of volume 2 with only the 4 face links; 12 Voronoi cells plus a slice fill a triangular prism to 0.5 m³ within 1e-3 as one connected graph; the same seed gives the same pieces and another seed differs; a huge piece count stays capped. The source counts as convex; the visual preview is the Milestone 12 "Fracture authoring preview" item. Full check: CPU lib 879 ok, gpu-tests run 996 ok.
- [x] Runtime fracture from impact energy and stress, deterministic under the seeded tick RNG.
  Evidence: `Fracture::break_energy` (J, 0 never) and `break_fractures`, run right after the CPU step: each contact's impact energy (half the reduced mass times the closing speed squared) is summed per body over the tick, so several hits at once add up as stress; a body at or over its threshold is replaced, in `SceneId` order, by one dynamic `ConvexMesh` body per pre-fractured piece (mesh recentred on its centre of volume, mass shared by volume, velocity from the body's motion at that point), and the energy beyond the threshold scatters the pieces from the hardest hit with jitter drawn from `RandomSeed` at the fixed tick. Test `a_hard_landing_breaks_a_crate_and_a_soft_one_does_not`: a 10 kg crate breaking at 100 J stays whole after a 0.2 m drop (about 20 J) and breaks into 6 pieces weighing 10 kg after a 3 m drop (about 290 J); the pieces settle near the ground; and a second run gives the same bits. Full check: CPU lib 880 ok, gpu-tests run 997 ok.
- [x] Debris lifetime, budget, sleeping, and merge policies. Never drop bodies arbitrarily at the cap.
  Evidence: break pieces carry `Debris` (break group, policy, tick it fell asleep), and `manage_debris` runs after `break_fractures`. Sleeping is the CPU solver's own (`Sleeping` after 30 still steps); policies only touch still debris, so nothing vanishes in flight. `Fracture::debris_lifetime` (s, 0 keeps) despawns a piece that long after it sleeps and frees its mesh; `Fracture::merge_debris` turns a break whose pieces all sleep into one `Fixed` `TriangleMesh` body in world space with their summed mass. `DebrisBudget` (resource, default 512, 0 no cap): a break at the cap first removes the longest-sleeping debris (by sleep tick, then spawn order) and, when too little sleeps, leaves the object whole that tick. Tests in `runtime::fracture`: `sleeping_debris_leaves_after_its_lifetime` (6 pieces, all gone with their meshes 1 s after sleeping); `a_settled_break_merges_into_one_static_body` (one fixed triangle-mesh body, 10 kg, 1 m³, on the ground); `at_the_budget_a_break_waits_for_sleeping_debris` (budget 8: two crates landing together give 6 pieces and one whole crate; a later crate after the first pieces sleep breaks and leaves exactly 8 debris). Full check: CPU lib 883 ok, gpu-tests run 1000 ok. Limits: the budget is a resource, not yet a scene setting; a merged body no longer breaks again.
- [x] Destruction events consumable by audio, particles, navigation, and gameplay.
  Evidence: each break sends one `Fractured` event (registered on the `App`, kept in snapshots), after the object is despawned and its pieces spawned: the object's entity and `Name`, the piece entities in spawn order, the hardest hit's world point, the tick's impact energy, the mass, and the world box the object filled (for navigation and other spatial caches). Audio and particles take point and energy, navigation the box, gameplay the entity, name and pieces. A break the debris budget holds back sends nothing. Determinism fix found here: with bevy 0.19 resources are entities, so registering any event shifted entity ids, and objects without a `SceneId` keyed their break order and scatter stream on the entity id; they now key on `SpawnOrder` first. Tests in `runtime::fracture`: `a_break_sends_one_fractured_event_and_a_soft_landing_none` (crates dropped from 0.2 m and 3 m: one event, for the named 3 m crate, 10 kg, 100 to 400 J, hit on the ground under it, box 1 m around it, six live `Debris` pieces); `breaks_do_not_depend_on_entity_ids` (seven extra empty entities give the same landed pieces; fails with the old entity-id key). Full check: CPU lib 885 ok, gpu-tests run 1002 ok (doc tests run with `TMPDIR` on disk; `/tmp` was full).

### Scale

- [ ] One unified GPU broad phase shared by rigid, deformable, particle, and fluid solvers.
  - [x] Step 1, the shared pass: `src/shaders/compute/broad_phase.comp`, run by `rendering::gpu_broad_phase::GpuBroadPhase::pairs`, finds every overlapping pair among axis-aligned `BroadProxy` boxes (`runtime::broad_phase`), each with a `layer` and `mask` like `CollisionLayers`, so rigid bodies, cloth and soft-body particles and fluid particles can share one pass and each solver keeps the pairs it wants. Uniform grid cells (the largest box's extent) hash into buckets: atomic counts, a scan, an atomic fill; then each proxy gathers its higher-index partners into its own row, kept ascending. Each proxy enters a bucket once and a pair counts only in the cell of its overlap's low corner, so atomics decide only fill order and the rows are the same whatever the scheduling. A row holds `capacity` partners, keeps the lowest past that, and reports the proxy in `BroadPairs::overflowed`. `pack` refuses non-finite or inverted boxes, more than `MAX_BROAD_PROXIES` (65536) and cells past 2^24; `pairs_on_cpu` is the brute-force reference. Tests: `the_gpu_broad_phase_finds_the_reference_pairs` (gpu-tests: 3000 boxes of four sizes and three layers give the reference's 25458 pairs; at capacity 2, 4756 pairs and 1795 overflowed rows, equal on three runs; all points, the empty set and an inverted box), `the_reference_pairs_overlapping_boxes_whose_layers_meet`, `packing_refuses_bad_boxes_and_sizes_its_buffers`, `a_proxy_enters_each_bucket_once`. Limits: one workgroup of 256 lanes; `pairs` blocks on the GPU; one huge box coarsens the grid for all; no solver reads the pairs yet. Open: build proxies from GPU rigid bodies, `GpuCloth`/`GpuSoftBody` and particles, let the solvers consume the pairs, a fenced runner like `GpuClothRunner`. Full check 2026-10-11: fmt and clippy x3 clean; lib 888 tests (121 ignored), 1006 with gpu-tests.
  - [x] Step 2, the fenced runner: `GpuBroadPhase::submit(tick, proxies, capacity)` returns at once (`false` when `pack` refuses), `take_completed` returns only searches whose fence has signaled as `(tick, BroadPairs)` in submission order, `in_flight` and `block_until_complete` (tests and tools) as on `GpuClothRunner`; the blocking `pairs` is gone. `pack` also refuses buffers past `u32::MAX` words, since the shader's row offsets are 32-bit and would wrap (found by a security review of Step 1). Tests: `the_gpu_broad_phase_finds_the_reference_pairs` now also keeps two searches in flight and gets both back once, in order, with their ticks; `packing_refuses_bad_boxes_and_sizes_its_buffers` checks capacity `u32::MAX`. Open: proxies from GPU rigid bodies, `GpuCloth`/`GpuSoftBody` and particles, and solvers reading the pairs. Full check 2026-10-11: fmt and clippy x3 clean; lib 888 tests (121 ignored), 1006 with gpu-tests.
  - [ ] Paused 2026-10-11 (lane physics) for an owner decision: the rigid GPU path already has its own multi-workgroup contact grid (`physics_contacts.comp`, `PhysicsContactGrid` in `scene_renderer.rs`) with no proxy cap, while the shared pass runs in one workgroup of at most 65536 proxies. Unifying means either making the shared pass multi-workgroup and moving rigid contacts onto it inside `scene_renderer.rs` (the render lane's file), or keeping the rigid grid and using the shared pass only for deformable and particle proxies. GPU cloth and soft bodies also need collider support in `cloth.comp` before they have pairs to consume.
- [ ] Multi-GPU-queue scheduling: async compute for physics overlapping graphics where supported, with a fallback on single-queue devices.
- [x] GPU simulation LOD: distant or unobserved regions step at reduced rates or freeze, with explicit, deterministic policies.
  Evidence (2026-10-11, lane physics): `GpuSimulationLod { full_radius, reduced_radius, reduced_stride }` with `SimulationObserver` entities (gameplay points such as players, not the render camera, so every peer steps the same bodies). A dynamic GPU body steps every tick within `full_radius` of its nearest observer, takes one `reduced_stride`-tick step every `reduced_stride` ticks out to `reduced_radius` (phase spread by `PhysicsId` slot), and freezes beyond, keeping its state. A body that does not step on a tick holds still in contacts like a fixed one. No resource or no observers: every body steps every tick. `lod_steps` in `physics_shapes.glsl` mirrors `GpuSimulationLod::steps`; observers go in binding 15 and the policy in four push-constant words appended to the ABI block (old custom shaders unaffected). Both types are snapshot state. Ceiling: a body that changes rate mid-stride gains or loses up to one stride of time. Tests: `render_world::tests::simulation_lod_extracts_observers_and_picks_each_body_rate`, `scene_renderer::tests::simulation_lod_slows_distant_gpu_bodies_and_freezes_far_ones` (near and mid balls both fall for exactly one second over 60 ticks, mid waits on tick 61, far never moves). Full check: fmt, clippy ×3, lib 898, gpu-tests lib 1017.
- [x] Streaming simulation regions in and out of GPU memory within a device budget.
  Evidence (2026-10-11, lane physics): `GpuStreamingBudget { max_resident }` keeps at most that many GPU bodies (each one `GpuBodyState` in device memory) on the GPU, the nearest to a `SimulationObserver` with ties broken by `PhysicsId` slot, decided each fixed tick. The rest stream out: each takes its last read-back state into its `Transform` and `RigidBody`, gains `StreamedOut`, and is left out of GPU extraction (the renderer then draws it at its `Transform`); late readback for it is ignored. It streams back in from that state when it ranks inside the budget again. No budget or no observers: nothing streams. Both types are snapshot state. Ceilings: no hysteresis at the budget's edge; the saved state is the last read-back one (exact for bodies `GpuSimulationLod` froze); `PhysicsSyncMode::None` bodies come back from their last `Transform`; observer world positions lag one frame. Test: `runtime::tests::a_streaming_budget_keeps_the_nearest_gpu_bodies_and_saves_the_rest` (far ball saved at its read-back pose and velocity and left out of extraction, near kept; the observer walks over and they swap, the far ball returning from its saved pose). Full check: fmt, clippy ×3, lib 899, gpu-tests lib 1018.

### Coupling

- [x] Two-way coupling between rigid, articulated, deformable, fluid, and granular simulation in one scene. Steps 1–3 below. Rigid ↔ fluid and granular was already in `couple_fluids`, and rigid ↔ deformable in the soft and cloth step.
  - [x] Step 1 (2026-10-11, lane physics): deformables meet fluid and granular particles. `step_soft_bodies` makes each nearby grain a dynamic sphere (its mass, half the fluid spacing plus half the deformable's shortest rest edge across). The XPBD substeps then solve it as an obstacle, so pins and stiff constraints carry the grains' weight, and the grains' new states are written back. Pinned particles now push obstacles too; before this, water leaked past a pinned edge. Known ceiling (`ponytail:` on `grain_bodies`): grains over about 30× the mass of the particles under them tear through, and grains finer than about 3/4 of the deformable spacing slip between particles. Evidence: `soft_body::tests::a_light_jelly_floats_on_water_and_sand_and_a_heavy_one_sinks`. On water a 300 kg/m³ jelly rests with its bottom at 0.19 m and a 5000 kg/m³ jelly sinks to 0.06 m. On sand they rest at 0.15 m and 0 m. `water_poured_on_a_pinned_sheet_stays_on_it_and_sags_it`: 48 kg of water stays above 0.5 m on a 10 kg pinned sheet, no grain is lost, and the middle sags below 0.95 m (alone it hangs at 0.97 m). Full check: fmt, clippy ×3, lib 890, gpu-tests lib 1008.
  - [x] Step 2 (2026-10-11, lane physics): fluid and granular volumes meet each other. After every volume steps, in `SpawnOrder`, `separate_volumes` makes four Gauss-Seidel passes. Each pass pushes apart particles of different volumes that are closer than the mean of their spacings, shares the push by particle mass (momentum kept) and turns it into velocity, and keeps each particle in its own container. Known ceiling (`ponytail:`): volumes feel each other's weight but not each other's pressure. Evidence: `fluid::tests::heavy_grains_sink_through_water_light_ones_float_and_a_wave_pushes_a_pile`. 3 kg grains dropped on a pool of 1 kg drops sink to the floor, and no grain ends closer than 0.05 m to a drop. 0.3 kg grains rest with their bottom at 0.06 m. A water wave thrown at 4 m/s moves a sand pile's mean x from 0.10 m to 0.18 m. Full check: fmt, clippy ×3, lib 891, gpu-tests lib 1009.
  - [x] Step 3 (2026-10-11, lane physics): rigid, articulated, deformable, fluid and granular bodies in one scene. Articulated links now take pushes from outside the tree. Before, `Tree::read` took a hinge's speed from the link's spin alone, so the linear pushes that fluids and deformables give link bodies between steps were dropped; a paddle swung through water as if it were dry. `Tree::absorb` turns whatever link velocity the tree did not produce into joint speeds through `H⁻¹ Jᵀ` of its momentum, so velocities the tree made itself are unchanged. Evidence: `articulation::tests::a_push_from_outside_moves_a_hanging_link_and_keeps_its_joint`. A hanging hinged bob pushed at 1 m/s sideways and 5 m/s along its rod rises the expected 5 cm, and its rod stays within 1 mm of 0.5 m. Without the fix it never moves. `soft_body::tests::every_kind_of_body_shares_one_tank_and_runs_the_same_twice` puts a water pool, falling sand, a 300 kg/m³ jelly, a pinned sheet with a dynamic ball on it and an articulated hinged paddle in one 1.2 m tank for 4 s. The water and the floating jelly stop the paddle (it swings back to under 0.4 m; dry, it swings back to over 0.45 m). The jelly floats with its bottom over 0.04 m (dry, it lands on the floor), the ball stays on the sheet, every value is finite, and a second run gives the same bits. Full check: fmt, clippy ×3, lib 893, gpu-tests lib 1011.
- [x] Mixed CPU/GPU ownership for coupled objects with defined latency and state age.
  - [x] Step 1 (2026-10-11, lane physics): GPU bodies couple with CPU fluid and granular volumes. A dynamic GPU body that reads its state back (`SelectedState` or `FullState`, so it has a `GpuStateMirror`) now takes part in `couple_fluids`. The fluid meets it at the mirrored pose, carried forward by its velocity over the state's age. What the fluid does to its velocity goes back as a `GpuBodyCommand::Impulse` of mass × velocity change, which applies before the next GPU tick. Defined latency: the body feels the fluid one tick later, and the fluid sees the body as old as its read-back state. State older than `MAX_COUPLED_STATE_AGE` (8 ticks) is not coupled. The latency rules are written in the `hybrid_physics` module docs. Evidence: `fluid::tests::water_pushes_a_gpu_body_through_impulses_from_its_read_back_state`. A submerged 1 kg GPU ball with fresh state gets one upward impulse. A ball whose 2-tick-old state is outside the water, moving at 18 m/s into it, is met where it is now (impulse up and against its motion). With state 9 ticks old no command is sent. Open: `water.rs` buoyancy, soft bodies and cloth, and CPU rigid contacts with dynamic GPU bodies. Full check: fmt, clippy ×3, lib 894, gpu-tests lib 1012.
  - [x] Step 2 (2026-10-11, lane physics): water buoyancy floats GPU bodies the same way. `float_in_water` meets a GPU body at its read-back state and sends lift and drag back as impulses, sorted by `SpawnOrder` so the command order does not depend on query order. Fluids and water share `hybrid_physics::coupled_gpu_state` and `MAX_COUPLED_STATE_AGE`. Evidence: `water::tests::water_floats_a_gpu_body_through_impulses_from_its_read_back_state`. A half-submerged 10 kg GPU ball with fresh state gets one upward impulse. A ball whose 3-tick-old state is off the water, moving back over it at 60 m/s, gets lift and drag against its motion. With state 9 ticks old no command is sent. Open: soft bodies and cloth, and CPU rigid contacts with dynamic GPU bodies. Full check: fmt, clippy ×3, lib 895, gpu-tests lib 1013.
  - [x] Step 3 (2026-10-11, lane physics): soft bodies and cloth meet GPU bodies the same way. A dynamic GPU body joins the XPBD step as an obstacle or anchor body at its read-back state. The change the step makes to its velocity goes back as an impulse, and its `Transform` is never written. Before this, a soft body or cloth wrote the body's authored `Transform`, which restarted the GPU body from that pose. Without fresh state the body is left out. Known ceiling (`ponytail:`): no angular impulse goes back, so a deformable cannot spin a GPU body. Evidence: `soft_body::tests::a_gpu_ball_and_a_sheet_push_each_other_through_read_back_state`. A 1 kg GPU ball falling at 2 m/s onto a pinned sheet gets one upward impulse, keeps its authored pose, and presses the sheet's middle down more than 5 mm further than with state 9 ticks old, which sends nothing. Open: CPU rigid contacts with dynamic GPU bodies. Full check: fmt, clippy ×3, lib 896, gpu-tests lib 1014.
  - [x] Step 4 (2026-10-11, lane physics): CPU rigid bodies meet dynamic GPU bodies. A dynamic GPU body with fresh read-back state (age at most `MAX_COUPLED_STATE_AGE`, 8 ticks) joins the CPU step as a kinematic body at its mirrored pose and velocity; a fixed GPU body joins as a fixed one. The CPU step never writes a GPU body back, and two such proxies do not collide with each other. The GPU still meets CPU bodies as colliders from the last CPU step, so the push back is felt one GPU tick later. Shared helper `hybrid_physics::coupled_gpu_state` serves fluids, water, soft bodies and CPU rigid bodies. Ceiling: momentum is not exactly kept, and the proxy pose is not advanced between substeps. Evidence: `cpu_physics::tests::a_cpu_body_is_pushed_by_a_gpu_body_at_its_read_back_state` (pushed when fresh, untouched when stale, GPU body's authored `Transform` kept). Full check: fmt, clippy ×3, lib 897, gpu-tests lib 1015; the dedicated-server real-time test flaked once under load and passes alone.

### Exit gate

- A demo scene combines cloth, soft bodies, fluid, granular media, and runtime fracture interacting with rigid bodies, with correct events delivered to Rust gameplay.
- Deformable, fluid, and fracture scenarios have headless GPU tests under lavapipe.
- A 1,000,000-body GPU scene runs within the hardware budget recorded for the benchmark machine.
- Fracture is deterministic across runs.

## Milestone 12: Physics tooling, authoring, and evidence

Goal: make physics the best-understood part of the engine — visible, debuggable, authorable, and benchmarked against competitors.

Depends on: Milestones 5, 6, and 10. Deformable and fluid tooling follows Milestone 11.

### Physics debugger

- [ ] Debug draw for shapes, contacts, normals, impulses, joints, islands, sleeping state, broad-phase cells, and GPU ownership.
  - [x] Shapes, contacts, normals, impulses, joints, sleeping state and GPU ownership. `PhysicsWorld::debug_draw` adds the last step's collider wireframes to a `RenderDebugOverlay`, coloured by kind, sleep, sensor and GPU ownership (`physics_debug_colors`). It also draws contact crosses with their normals, solver normal impulses at each manifold point, and joint anchors with the link between them. The editor Scene view draws it while a `PhysicsDebugDraw` resource exists; the header toggle is in the `docs/editor-overhaul.md` backlog. Evidence: `physics_debug::tests::debug_draw_shows_shapes_contacts_impulses_joints_and_sleep`. It checks 12 edges per box in the right colour, a GPU-owned body, a contact cross and normal, four upward impulses under a resting crate, joint anchors at both ends, and the sleeping colour once the crate sleeps. fmt, all three clippy configurations, `cargo test --workspace` (lib 900) and the gpu-tests run (lib 1019) pass.
  - [x] Islands. `PhysicsWorld::debug_draw` (new `PhysicsDebugDraw::islands`, on by default) draws a box (`colors::ISLAND`) around each island of two or more dynamic bodies, rebuilt from the last step's contacts and the `Joint`s; fixed, kinematic, sensor and GPU bodies split islands as in the solver, so the step itself records nothing new. Ceiling (`ponytail:`): articulation links join only through contacts. Evidence: `physics_debug::tests::debug_draw_boxes_islands_of_touching_and_jointed_bodies` (two crates stacked on the ground give one box around both; a lone crate on the same ground gives none; two boxes jointed 2 m apart in the air give one box; 24 island lines). fmt, all three clippy configurations, `cargo test --workspace` (lib 902) and the gpu-tests run (lib 1022) pass.
  - [ ] Broad-phase cells, once the shared GPU broad phase drives the solvers (paused above for an owner decision).
- [ ] Record physics sessions (from play mode or a running game) and scrub a timeline tick by tick, using Milestone 8 replay.
- [ ] Per-body inspector showing state history, contacts, applied forces, owning solver, sync mode, and readback age.
- [ ] Remote physics debugging of a running game process from the editor.
- [ ] Heatmaps for solver cost, contact density, and overflow by region.

### Authoring tools

- [ ] Collider editing gizmos, automatic collider generation, and convex decomposition (V-HACD-class) for imported meshes.
- [ ] Joint placement and limit-editing gizmos with live preview.
- [ ] Ragdoll creation wizard from a skeleton.
- [ ] Physics material library, collision-layer matrix editor, and force-field visualization.
- [ ] Simulate-in-editor: run physics on selected objects without entering play mode, then keep or discard the result (for placing props naturally).
- [ ] Fracture authoring preview.

### Profiling

- [ ] Physics profiler panel: CPU/GPU solver time per stage, body/contact/constraint counts, command/event/readback bytes, overflow, and synchronization latency.
- [x] Automatic-allocation diagnostics explaining why each body was placed on CPU or GPU.
  Evidence: `AllocationDecision::explain` turns each decision into a sentence with the `AutoAllocationPolicy` thresholds filled in. The inspector shows it under the Auto CPU/GPU row. `auto_simulation_picks_cpu_or_gpu_and_stays_overridable` checks the over-budget sentence. `inspector_shows_the_auto_simulation_decision` checks the many-bodies sentence. fmt, all three clippy configurations, `cargo test --workspace` (lib 899) and the gpu-tests run (lib 1018) all pass.

### Benchmarks and evidence

- [x] Standard benchmark scenes: stacking stability, 100K pile, ragdoll count, joint-chain stability, CCD bullets, vehicle stress, cloth, fluid, and fracture.
  - [x] Stacking stability, joint-chain stability and CCD bullets. `SuiteScene::run(size, steps)` builds the same bodies every run, steps them at 60 Hz on the CPU solver and returns a `SuiteScore`: mean step time and one error number. For stacking it is the largest cube drift. For the chain it is the widest gap between joined anchors over the run. For bullets it is the share that passes a 10 cm wall at 300 m/s. `cargo run --release --example physics_suite` prints them. Measured (release, this machine): a 10-cube tower drifts 3.5 cm, 15 cubes 16 cm, and 20 cubes fall over (66 m at 600 steps). A swinging 20-link ball-socket chain opens 0.43 m (10 links: 0.29 m). 400 bullets: none tunnel, 1.2 ms per step. Evidence: `physics_suite::tests::standard_scenes_stay_within_their_error_bounds` holds 10-cube drift < 5 cm, chain gap < 0.5 m (the current baseline) and zero tunnelling. fmt, all three clippy configurations, `cargo test --workspace` (lib 901) and the gpu-tests run (lib 1020) pass.
  - [ ] Solver quality the suite exposed: a 20-cube tower must stand (drift under 5 cm) and a 20-link chain must stay under 5 cm of joint gap. Tried 2026-10-11, not committed:
    - 16 global iterations gave 5.3 cm on the 20-cube tower, but 12, 20 and 24 gave about 10 cm, so the result is chaotic.
    - Per-island iterations (one per body, 8 to 32) gave 35 cm.
    - Whole-island sleep gave 2.1 m: the tower never sleeps and tips over.
    - A trace shows each cube tilting a few milliradians while it settles; partial sleep then freezes the lower cubes, and the wake at about step 200 drops the tower 20 cm.

    The root cause is the sequential-impulse contact solve tilting cubes as they settle. Look at symmetric manifold ordering or a contact-point reduction, and at position correction (0.8 of the overlap per step is aggressive). The chain does not improve with iterations; look at its joint rows. Then tighten the test bounds.
  - [x] Cloth and fluid. `SuiteScene::ClothHang` hangs a `size` by `size` sheet of 5 cm cells by its two top corners and scores the largest edge strain seen in any step. `SuiteScene::FluidDrop` drops a cube of about `size` particles 1 m into a column that fits it and scores how much lower its mean height settles than at rest spacing. Measured (release, this machine): a 40 by 40 sheet stretches 25% at its pinned corners (9.2 ms per step); 1000 fluid particles lose 21% of their height (2.1 ms per step). Evidence: the same test holds a 10 by 10 sheet under 5% strain (it reaches 2.4%) and a 216-particle drop under 40% height loss (it loses 33%, a baseline: four density iterations are soft). fmt, all three clippy configurations, `cargo test --workspace` (lib 901) and the gpu-tests run (lib 1020) pass.
  - [x] Vehicle stress and fracture. `SuiteScene::VehicleStress` runs `size` raycast cars 12 m apart at full throttle and full right lock and scores the most any car tilted from vertical. `SuiteScene::FractureDrop` drops `size` 10 kg crates (6 pieces, break at 100 J) 3 m and scores the share of the `6 * size` pieces not resting above the floor at the end, so a crate that never broke counts too. Measured (release, this machine): 25 cars tilt at most 0.069 rad (0.11 ms per step); 50 crates lose no piece (0.94 ms per step). Evidence: the same test holds 4 cars under 0.2 rad over 240 steps and 4 crates at zero missing pieces over 90 steps. fmt, all three clippy configurations, `cargo test --workspace` (lib 901) and the gpu-tests run (lib 1020) pass.
  - [x] Ragdoll count. `SuiteScene::RagdollPile` drops `size` limp seven-bone ragdolls (hips, spine, head, two arms, two legs; default cone-twist joints) 2 m apart and scores the widest gap between joined anchors in any step; a ragdoll missing a joint scores infinite, so a scene that never went limp cannot pass. Measured (release, this machine, load average about 18 from other lanes so the time is noisy): 100 ragdolls open at most 1.5 cm, 1.6 to 3.9 ms per step. Evidence: the same test holds 4 ragdolls under 5 cm over 120 steps. fmt, all three clippy configurations, `cargo test --workspace` (lib 901) and the gpu-tests run (lib 1020) pass.
  - [x] 100K pile. `a_100k_pile_lands_on_the_gpu_without_sinking` (gpu-tests) runs `PhysicsBenchmark::Debris` with 100 000 cubes on the GPU for 300 ticks and scores how far each cube's lowest corner ends below the ground. Measured (RTX 3060, release, blocking on every tick's readback): 34 to 42 ms per tick, 86 cubes more than 5 cm into the ground, the deepest 0.41 m, 44 grid-cell overflows. Evidence: the test held that baseline (at most 150 cubes past 5 cm, none past 0.5 m) until the sinking fix below. fmt, all three clippy configurations, `cargo test --workspace` (lib 901) and the gpu-tests run (lib 1021) pass.
  - [x] GPU pile sinking the 100K pile exposed. Root cause: a body that fell asleep in `physics.comp` snapped back to its position from before the collider pass, so each tick the weight above pressed the sleeping bottom layer a little further into the ground and the ground's correction was thrown away. A sleeping body is now pushed out of the colliders again after the snap. Measured (RTX 3060, release): the deepest corner of the 100K pile is 0.0000002 m into the ground (was 0.41 m), none past 5 cm (was 86), 41 ms per blocking tick. Evidence: `a_100k_pile_lands_on_the_gpu_without_sinking` now holds no cube past 5 cm and the deepest under 1 cm. fmt, all three clippy configurations, `cargo test --workspace` (lib 901) and the gpu-tests run (lib 1021) pass.
- [ ] Reference implementations of the same scenes on Jolt, PhysX, Rapier, and Godot Physics, run by the same harness.
- [ ] Publish results with hardware, driver, and settings; regressions against stored baselines fail CI.

### Exit gate

- A physics bug in a recorded session can be located by scrubbing to the tick and inspecting the body in the editor.
- Colliders, joints, ragdolls, and materials can be authored entirely in the editor.
- The comparative benchmark report is generated reproducibly and checked into the repository.

## Milestone 13: Advanced rendering

Goal: reach Godot's Forward+ visual feature set and give users programmable shading.

Depends on: Milestones 3 and 4.

### Global illumination and reflections

- [x] Reflection probes with box projection and blending.
  `rusting.reflection_probe` holds the half size of a world-aligned box
  around its object. When a probe is added or changes, a temporary renderer
  draws six 90-degree faces from its center, so the main renderer's caches,
  GPU physics and occlusion history stay untouched. The faces are copied into
  one mipmapped array image with six layers per probe. Up to four probes are
  used (`MAX_REFLECTION_PROBES`). The fragment shader's `surroundings` casts
  the reflection direction to the box wall, looks up the face toward that
  point, fades each probe in over the outer tenth of its box, and blends the
  result over the environment. Rough surfaces use lower mips. Limits
  (`ponytail:` in code): probes are static captures, faces are tone mapped
  with Linear so captured light clamps at 1, and moving a probe rebuilds the
  capture pipelines.
  Evidence: `reflection_probe_shows_the_scene_behind_the_camera` renders a
  mirror floor under an unlit red ceiling that covers the -X half and is
  behind the camera. With no probe, both sides are dark. With a probe, the
  -X side is red and the +X side is not. Flipping the face lookup's x axis
  fails the test. Full check: fmt, clippy three ways, `cargo test
  --workspace` (474 passed, 76 ignored), and GPU tests (546 passed).
  Editor: a selected probe shows a handle at each box face; dragging one
  sets that axis of `extents` (the box stays centered), as one Undo step.
  Evidence: unit test
  `dragging_scene_handles_resizes_a_probe_and_thins_fog`.
- [x] Refractive glass: `transmission`, `ior` and `thickness` on materials, sampling a copy of the opaque scene color so glass bends and tints what is behind it. The scene render pass must split after the opaque draws in every variant (MSAA, occlusion culling).
  Materials carry `transmission`, `ior` and `thickness`. A frame with a
  transmissive draw ends the scene pass after the opaque draws, copies HDR
  into a mipmapped `scene_color` image (`FramePass::SceneColor`), and draws
  the blended list in the late pass (`FramePass::Transparent`). The shader
  refracts the view ray once at the entry face, offsets it by `thickness`,
  and samples the copy at mip `roughness * last_mip`, tinted by base color.
  Limits: no exit-face refraction, and blended objects behind glass are not
  in the copy. glTF `KHR_materials_transmission` import needs a new `gltf`
  crate feature and is not done. Evidence: GPU test
  `transmissive_glass_tints_the_refracted_backdrop` under frustum and
  occlusion culling (fails with the refraction path turned off: 255 255
  255); `transitions_list_every_layout_change_between_passes`.
- [x] Baked lightmaps with a GPU lightmapper and light probes for dynamic objects.
  Light probes: `rusting.light_probe_grid` (extents, resolution,
  intensity) spreads up to 64 probes (1..=8 per axis) wall to wall through a
  world-aligned box. When the grid is added or changes, the shared
  `capture_faces` helper (also used by reflection probes) renders six
  16x16 R32G32B32A32 faces per probe with a temporary renderer; they are
  read back and `ambient_cube` integrates them on the CPU into irradiance
  over pi toward each axis. Set 2 binding 11 holds the cubes; the fragment
  shader's `probe_light` blends the eight nearest probes trilinearly,
  weights the six sides by the squared normal, and replaces the sky,
  environment and ambient diffuse light inside the box (fading in over the
  outer tenth). Limits: static bake (moving objects and lights do not
  update it), every surface uses probes since there are no lightmaps yet,
  probes inside geometry read black, captured light clamps at 1, no editor
  gizmo (deferred in `docs/editor-overhaul.md`). Evidence: GPU test
  `light_probes_light_a_wall_with_the_red_slab_behind_the_camera` (a white
  wall turns red from a probe that sees an unlit red slab behind the camera,
  stays gray with the grid elsewhere or removed); it failed with the probe
  weight zeroed (255 255 255) and with the +Z/-Z sides swapped (202 202
  202). Unit tests `ambient_cubes_integrate_the_light_from_each_side` and
  `light_probe_grids_clamp_and_spread_wall_to_wall`.
  Lightmaps: `rusting.lightmap` (resolution 2..=64, default 16) wraps six
  square faces of texels around the object's local mesh box (box
  projection, no UV2). When a lightmapped object, its mesh or material, a
  light, the ambient or the sky changes, `bake_lightmaps` runs the
  `lightmap_bake.comp` compute shader once per slot: each texel is projected
  onto the object's own triangles, then 64 cosine-weighted Hammersley rays
  (rotated per texel) hit every lightmapped triangle; a hit adds albedo times
  direct light (shadow rays to each light) plus emissive, a miss adds the
  sky/ground hemisphere ambient. Set 2 binding 12 holds the texels; the
  fragment shader blends the three faces the normal points at (squared
  weights, bilinear) and the result replaces the ambient term after the
  probe mix, so direct light stays real time. Limits: box projection shares
  texels on concave meshes, brute force tracing capped at 16384 triangles
  (no BVH), one bounce, the bake ignores the environment map and light
  cookies, static bake (moving objects do not update it), no editor rebake
  button (deferred in `docs/editor-overhaul.md`). Evidence: GPU test
  `lightmaps_bounce_a_sunlit_red_slab_onto_the_floor` (a white floor under
  a sunlit red slab, with ambient off, reads red only when both are
  lightmapped, and stays dark with one of them or none); it failed with the
  bake's `albedo * direct` zeroed ([0, 0, 0]). Unit test
  `lightmaps_map_the_mesh_box_onto_the_unit_cube_and_skip_the_rest`. Full
  check: fmt, clippy (default, `--no-default-features`, `gpu-tests`),
  `cargo test --workspace` and the gpu-tests run (1000 lib tests) pass.
- [x] One real-time GI technique (probe-based DDGI or voxel/SDF GI) with a quality-profile fallback to ambient probes.
  Probe-based: on the High quality profile `rusting.light_probe_grid`
  recaptures `LIGHT_PROBES_PER_FRAME` (2) probes per frame round robin
  (`relight_light_probes`), so a grid of 64 probes follows moving objects
  and lights within 32 frames. Each recapture renders the probe's six
  16x16 faces with a capture renderer that `capture_faces` now keeps
  between calls (format and size match), reads them back and patches the
  probe's ambient cube into a fresh buffer, so frames in flight keep the
  old one. Eco and Balanced fall back to the probes baked when the grid
  changes. Limits: one bounce (captures do not see the probe light), each
  recapture blocks on its readback (ponytail note: move the reduction to
  compute if it shows in frame pacing), up to 32 frames of lag on a full
  grid. Evidence: GPU test
  `high_quality_relights_light_probes_when_the_red_slab_moves_away` (a
  probe sees an unlit red slab; after the slab moves aside the wall turns
  gray on High and stays red on Balanced); it failed with the relight call
  removed ([247, 56, 56]). The light probe test still passes with the
  cached capturer. Full check: fmt, clippy (default,
  `--no-default-features`, `gpu-tests`), `cargo test --workspace` and the
  gpu-tests run (1001 lib tests) pass.
- [x] Sky system: procedural physical sky, HDRI skies, and sky-derived ambient/specular.
  Partial: `EnvironmentMap { texture, intensity }` (`rusting.environment_map`)
  loads an equirectangular image and replaces the `SkyLight` hemisphere as the
  ambient source. Diffuse samples the second-smallest mip along the normal;
  specular samples mip `roughness * last_mip` along the reflection. Textures
  now get a linear-blit mip chain, and samplers now allow every mip (the old
  default clamped the LOD to 0). Known ceilings: 8-bit images only, so no
  `.hdr`/EXR and no light brighter than `intensity`; box-filtered mips instead
  of GGX prefiltering; the map is not drawn as the background. Evidence: GPU
  test `smooth_metal_reflects_the_environment_map` (a smooth metal slab
  reflects the red upper half from above and the blue lower half from below;
  fully rough, it shows a blend of both). It failed before the pole clamp
  (a repeating sampler blended the top row with the bottom one) and before
  the mip chain (rough metal stayed pure red). Unit test
  `environment_maps_resolve_their_texture_and_reach_the_render_world`.
  Partial (procedural sky): `ProceduralSky { intensity, haze, ground_color,
  sun_size }` (`rusting.procedural_sky`) draws a single-scattering daylight
  sky (Rayleigh air, Mie haze, Kasten-Young air mass, `src/shaders/sky.glsl`)
  behind the scene, with a sun disc, under the fog. The sun is the shadowed
  directional light, else the first one. Without an environment map, a
  compute pass bakes the sky into a 128x64 half-float equirect with a mip
  chain, re-baked only when the sun or settings change, and binds it as the
  environment, so surfaces get sky ambient and specular. Known ceilings: one
  scattering bounce, no night sky or moon, no `.hdr` HDRI loading, and
  environment maps are still not drawn as the background. Evidence: GPU test
  `the_procedural_sky_follows_the_sun_and_lights_the_scene` (horizon blue at
  noon, red at sunset, a bright disc, and a slab lit with no ambient light);
  it failed with the sky pass gated off (black background) and with the bake
  not bound (slab unchanged). Unit test
  `the_sun_is_the_shadowed_light_and_sets_the_sky_brightness`.
  HDRI: textures loaded from Radiance `.hdr` files (own RGBE decoder in
  `src/assets/radiance.rs`, flat and run-length scanlines) upload as
  `R16G16B16A16_SFLOAT` (`TextureColorSpace::Hdr`), so environment light
  keeps values above 1. `EnvironmentMap.background` draws the map behind the
  scene in the sky pass, under the fog. Done with known ceilings: box-filtered
  mips instead of GGX prefiltering, single-scattering sky with no night sky,
  old-style run-length `.hdr` files rejected, and the editor's asset import
  does not list `.hdr` yet (deferred in `docs/editor-overhaul.md`).
  Evidence: GPU test
  `hdr_environment_maps_draw_behind_the_scene_brighter_than_white` (no
  background without the flag; a 0.25 red texel shows mid red, a 4.0 texel
  clips to full red and still shows mid red at intensity 1/16); it failed
  with the background flag ignored and with the lookup turned half a circle.
  Unit tests `radiance_files_decode_flat_and_run_length_scanlines` and
  `half_floats_round_and_clamp`.

### Screen-space and post effects

- [x] SSAO, SSR, and screen-space indirect lighting, each independently toggled by quality profile.
  Done: SSR off on `Eco`, SSAO off on `Eco`, indirect light on `High` only.
  Partial (SSR): frames with a PBR batch under roughness 0.5
  (`SSR_MAX_ROUGHNESS`) split like refraction frames. The `SceneColor` pass
  also copies opaque depth into depth pyramid mip 0; the `Transparent` pass
  redraws the glossy batches with a reflection overlay pipeline (fragment
  specialization `PASS = 2`, `LessOrEqual` depth, additive blend) that
  marches the reflected ray through that depth and adds the difference
  between the traced color and the environment reflection. Blended and
  transmissive surfaces trace inline. The weight fades at the screen edge,
  at the end of the march, and toward the roughness limit. `Eco` skips it.
  Limits: linear march, no Hi-Z; glossy surfaces do not reflect each other
  or blended objects. Evidence: GPU test
  `mirror_floor_reflects_the_scene_in_screen_space` (red wall in the floor
  on High, plain gray hemisphere on Eco); pass table
  `transitions_list_every_layout_change_between_passes`.
  Partial (SSAO): `rusting.ambient_occlusion` (radius, intensity) turns it
  on. A `DepthPrepass` pass draws the early-culled (else GPU-culled)
  batches into scene depth with alpha-mask discard; the `AmbientOcclusion`
  compute pass traces 16 rotated hemisphere samples per pixel against that
  depth, then a 4x4 depth-aware blur writes an R32F factor that set 2
  binding 6 multiplies into ambient, sky and environment light. The main
  pass then loads the prepassed depth. Off on `Eco`, in probe captures and
  in non-Lit views. Limits: objects that come into view on an
  occlusion-culling frame miss the prepass for one frame; no temporal
  accumulation. Evidence: GPU tests
  `ambient_occlusion_darkens_surfaces_next_to_occluders` (floor next to a
  box darker than open floor, unchanged with the component removed) and
  `atmosphere_effects_combine_with_every_scene_path` (frustum and
  occlusion culling, MSAA off and 4x, opaque and glossy); pass table
  `transitions_list_every_layout_change_between_passes`.
  Indirect light: `rusting.ambient_occlusion.indirect` above 0 turns it on,
  `High` profile only. Those frames make the `SceneColor` copy; the next
  frame's `AmbientOcclusion` trace reprojects each occluding sample into that
  copy with last frame's view projection and averages the colors it sees
  (cosine-weighted samples, so the mean is irradiance over pi). The blur
  filters it with the same depth weights into an RGBA16F target that set 2
  binding 10 adds to the diffuse term. Limits: one frame of lag, the first
  frame after a resize has none, only on-screen occluders inside the radius
  bounce light, single bounce. Evidence: GPU test
  `indirect_light_bleeds_a_bright_surface_onto_its_neighbour` (a dark wall
  turns red beside a glowing red slab on `High`, stays dark at strength 0
  and on `Balanced`); it failed (red 0 -> 0) with the binding 10 add gated
  off and with the reprojection matrix zeroed. Pass table tests
  `every_read_follows_a_write_of_the_same_resource` and
  `transitions_list_every_layout_change_between_passes`.
- [x] Bloom/glow, depth of field, motion blur, auto-exposure, color grading LUTs, vignette, and chromatic aberration.
  Partial (bloom): `rusting.bloom` (intensity, threshold, spread). After the
  transparent pass, the `Bloom` compute pass prefilters the HDR target to
  half resolution (13-tap with Karis weights against fireflies, soft knee
  at the threshold), downsamples it through up to 7 mips, and upsamples
  back with a 3x3 tent mixed by spread. Tone mapping adds mip 0 times the
  intensity before the curve. Frames with bloom split the main pass like
  refraction frames. Off in probe captures and non-Lit views. Evidence: GPU
  tests `bloom_spreads_bright_light_into_its_surroundings` (dark pixels
  beside an emissive quad brighten only with bloom on) and
  `atmosphere_effects_combine_with_every_scene_path`.
  Done elsewhere: vignette and chromatic aberration are `ColorGrading`
  fields (art direction C and the CRT/VHS item).
  Partial (auto-exposure): `rusting.auto_exposure` (key, min_exposure,
  max_exposure, speed). After the transparent pass, the `Exposure` compute
  pass takes 4096 bilinear taps of the HDR target in one workgroup, sums
  log luminance with a fixed-order reduction, and moves a persistent
  exposure factor toward `key / average` in log space by
  `1 - exp(-speed * dt)` (wall-clock dt; the first frame after a frame
  without it snaps). Tone mapping multiplies the `ToneMapping` exposure by
  it. Off in probe captures and non-Lit views; camera screens adapt in
  their own renderer. Limits: plain average with no histogram or center
  weighting; a bright spot between taps is missed. Evidence: GPU test
  `auto_exposure_brings_the_scene_to_its_key` (a 0.02 slab at key 0.06
  matches a fixed exposure of 3; speed 0 holds it when the key changes; a
  frame without it resets; `max_exposure` clamps), which fails when the
  meter writes 1.0; pass table `transitions_list_every_layout_change_between_passes`;
  unit test `atmosphere_settings_come_from_the_lowest_entity`. Full check:
  fmt, clippy three ways, `cargo test --workspace` (820 passed, 105
  ignored) and GPU tests (922 passed). Depth of field, motion blur and
  color grading LUTs are not started.
  Partial (color grading LUTs): `rusting.color_lut` (texture, intensity).
  The texture is a strip of N squares of N x N texels (256x16, 1024x32);
  the lowest entity with a loaded strip wins. Tone mapping, after
  `ColorGrading` and before the film effects, sRGB-encodes the color, reads
  two neighboring squares and blends them by blue, then mixes by
  intensity. Lit view only; a camera screen's own grading skips it; a strip
  whose width is not its height squared is ignored. Docs in
  `docs/look-and-feel.md`. Evidence: GPU test
  `color_lut_remaps_the_final_image` (a red-blue swap strip turns a red
  unlit slab blue at intensity 1 and shows both at 0.5), which fails when
  the shader drops the lookup. Full check: fmt, clippy three ways,
  `cargo test --workspace` (821 passed, 111 ignored) and GPU tests (929
  passed). Depth of field and motion blur are not started.
  Partial (depth of field): `rusting.depth_of_field` (focus_distance,
  blur). Frames with it take the scene-color copy path and also copy the
  opaque depth into depth-pyramid mip 0. Tone mapping turns depth back into
  distance with four projection entries (perspective and orthographic),
  sets the blur radius to `blur * |1 - focus / distance|` capped at `blur`
  (a fraction of the screen height), and gathers 32 golden-angle taps;
  nearer taps sharper than their distance stay out, so sharp edges do not
  smear over a blurred background. Push constants packed to 128 bytes
  (sampled and auto-exposure flags in one word; LUT size from the texture).
  Lit view only. Limits: a blurred foreground keeps a hard outline over a
  sharp background (no scatter or max-blur tiles); transparent objects blur
  with what is behind them. Evidence: GPU test
  `depth_of_field_blurs_only_away_from_the_focus` (an edge slab 5 m away
  matches the plain frame at focus 5 and softens at 2.5 and 50, for both
  projections); it fails when the gather is skipped, the distance sign is
  flipped, or the depth formula is scaled. Full check: fmt, clippy three
  ways, `cargo test --workspace` (821 passed, 112 ignored; the tools
  lane's `a_handshake_must_arrive_whole_in_time_and_stay_small` failed once
  under load and passes alone) and GPU tests (930 passed). Motion blur is
  not started.
  Done (motion blur): `rusting.motion_blur` (intensity, the shutter
  share). The renderer keeps last frame's view projection while it is on;
  tone mapping reprojects each pixel's opaque depth through `previous *
  inverse(current)` (a small storage buffer at set 1, binding 2) and
  averages 16 taps along the screen motion times intensity, centered on the
  pixel. No last frame means no blur; motion longer than a sixth of the
  screen height is a cut. Off in probe captures and non-Lit views. Limits:
  camera motion only (no per-object velocity buffer); where motion blur
  applies it replaces depth of field for that pixel. Evidence: GPU test
  `motion_blur_smears_only_while_the_camera_moves` (sharp on the first and
  still frames, soft edge on a 0.2-unit slide, sharp again when held, sharp
  on a 1-unit jump); it fails when the cut limit is lifted, the gather is
  skipped, or the last frame stops updating. Full check: fmt, clippy three
  ways, `cargo test --workspace` (821 passed, 113 ignored; the tools lane's
  flaky handshake test failed once under load and passes alone) and GPU
  tests (931 passed). With bloom, auto exposure, LUTs, depth of field and
  motion blur in, and vignette and chromatic aberration in `ColorGrading`,
  the item is done.
- [x] Temporal anti-aliasing and FXAA; optional upscaling (FSR-class) behind capability checks.
  Partial (FXAA): `Antialiasing::Fxaa` (Project Settings and Render
  Settings list it as "FXAA") renders the scene at 1 sample and runs a
  console-style FXAA (Lottes) in the tone-mapping pass. It reads the HDR
  copy and finds edges on the exposed, compressed luma. Flat areas exit
  early, and the far tap falls back to the near tap when it leaves the
  local luma range. GPU test `fxaa_smooths_a_stair_stepped_edge` draws a
  tilted white slab at 64x64. Without FXAA, fewer than 4 pixels fall
  between the dark and white levels; with it, more than 30 do. The
  background and the slab interior stay unchanged. Mutation checks: an
  edge test that always exits early, and a blend that returns the center
  pixel, both fail the test. Full check: fmt, clippy x3 clean;
  workspace tests 820 passed (flaky net handshake test passes alone);
  gpu-tests 932 passed.
  Partial (TAA): `Antialiasing::Taa` ("TAA") renders 1 sample and shifts
  the projection by a Halton(2,3) sub-pixel offset, cycling every 8
  frames. Shadow cascades and reprojection use the unshifted matrix. The
  `TemporalHistory` compute pass (`post_effects.rs`) runs after the scene
  color copy. It reprojects each pixel into last frame's history using
  depth and last frame's unshifted view projection, clamps the history to
  the pixel's 3x3 neighborhood, and blends in 10% of the new frame. The
  result goes back over the copy, so DOF, motion blur and grading run on
  top of it. The history starts over after a resize, a viewport change or
  a frame without TAA. Probe captures turn TAA off. GPU test
  `taa_smooths_a_still_edge_and_leaves_no_ghost_when_it_moves`: the first
  TAA frame matches no AA (fewer than 4 in-between pixels). After 24
  frames, more than 30 pixels are in between, and the background and slab
  interior are unchanged. Moving the slab off screen leaves no pixel
  brighter than dark+20. Mutation checks: no jitter, and no history,
  each give 0 in-between pixels; no clamp leaves a 243 ghost. Full check:
  fmt, clippy x3 clean; workspace tests 820 passed (the flaky net
  handshake test passes alone); gpu-tests 933 passed. Limits: no test
  covers reprojection under camera motion. Split views that share one
  renderer reset the history every view, so they get no TAA.
  Upscaling: `RenderSettings::upscale_sharpness` (0 to 1, default 0; game
  API `set_upscale_sharpness`). Above 0, a `render_scale` below 1 is
  stretched by a full-screen pass, not the linear blit. The pass does a
  9-tap Catmull-Rom filter, clamped to the 4 nearest source texels so
  edges do not ring, then AMD CAS-style contrast-adaptive sharpening. The
  capability check is the existing render-scale one: the scaled image's
  format must support blit and linear filtering, else the frame renders
  at scale 1. Pixelated and split-viewport blits keep the old path. GPU
  test `sharp_upscale_keeps_edges_steeper_than_the_linear_stretch`, at
  render scale 0.5 on 64x64: summed squared neighbor steps are 30056
  linear vs 62520 sharp (the test requires more than 1.5x). Center and
  corners match within 8. Mutation checks: with the sharp path off, both
  give 30056; with no sharpening lobe, 33448; both fail. Limit: the test
  cannot tell Catmull-Rom from bilinear in the center tap, because
  sharpening dominates. Full check: fmt, clippy x3 clean; workspace tests
  821 passed; gpu-tests 934 passed.
- [x] Volumetric fog with light scattering and fog volumes.
  Partial (height fog): `rusting.fog` (color, density, height,
  height_falloff, sun_scatter, sky_affect). `src/shaders/fog.glsl`
  integrates exponential height fog along the camera ray in closed form,
  with in-scattering toward the shadow-casting (or first) directional
  light. Opaque, blended and transmissive surfaces apply it in the
  fragment shader; a full-screen sky pass at the far plane fades the
  background. Evidence: GPU test `fog_hides_distant_surfaces_and_the_sky`
  (distant red wall and the sky turn fog-colored, near surfaces keep their
  color) and `atmosphere_effects_combine_with_every_scene_path`; unit tests
  `atmosphere_settings_come_from_the_lowest_entity` and
  `atmosphere_settings_go_on_one_environment_object`.
  Partial (light shafts): `rusting.fog` `light_shafts` (0..1) scales the
  sun glow by the share of 16 hard shadow-map taps along the camera ray
  that the sun reaches (cascade picked per tap, per-pixel interleaved
  gradient offset), in both the surface fog and the sky pass. Evidence:
  GPU test `light_shafts_keep_fog_in_a_casters_shadow_dark` (fog in a
  slab's shadow looking into the sun: 255 with shafts off, under 30 with
  shafts on, bright again when the sun casts no shadows); dropping the
  visibility factor or replacing the shadow tap with 1.0 fails it.
  Fog volumes: `rusting.fog_volume` (color, density, extents) fills an
  oriented box (object transform times half size) with uniform fog. Up to
  8 volumes, in entity order, go to the main pass as world-to-box
  matrices; `fog_volumes()` in `fog.glsl` clips each camera segment to
  each box and composites them in front of the height fog, on surfaces
  and in the sky pass (which now also runs when only volumes exist). The
  editor's fog switch and debug views turn them off. Evidence: GPU test
  `fog_volumes_fill_only_their_turned_box` (a quarter-turned red box turns
  the middle column red and leaves the edge column white, hides nothing
  from behind the slab, reddens the sky in front of it and leaves the sky
  beside it black); unit test `fog_volumes_carry_their_box_in_entity_order`.
  Uploading the box matrix without inverting it, or skipping the sky pass
  for volumes, fails the GPU test. Known limits: no froxel grid (the
  march runs per shaded pixel); fog volumes are unlit (their color does
  not react to lights or shadows); only the sun makes shafts, not spot or
  point lights; volume edges are hard.
  Editor: the Scene view's viewport menu has Scene Effects toggles that
  leave fog, bloom or ambient occlusion out of the Scene view only (Play
  and games draw them all), and a selected Fog draws squares at its
  full-density height and its 1/e height. Dragging the first square sets
  `height`; dragging the second sets `height_falloff`; each drag is one
  Undo step. Evidence: unit tests
  `scene_view_shading_and_effects_apply_only_in_the_scene_workspace`,
  `fog_height_squares_mark_full_density_and_the_thinned_height` and
  `fog_squares_are_grabbed_and_dragged_to_a_height`; the fog,
  bloom and ambient occlusion GPU tests render each effect switched off.

### Lighting and shadows

- [x] Cascaded shadow maps for directional lights; shadows for point and spot lights.
  - Partial (cascades, 2026-10-10): the shadowed directional light renders 4
    cascades into the 2x2 tiles of the existing shadow map
    (`scene_renderer.rs` `cascade_end`, `frustum_slice_sphere`,
    `cascade_view_projection`). Splits blend log and even (0.75/0.25) up to
    the profile's shadow distance; each cascade bounds its frustum slice
    with a sphere and snaps to whole texels, so edges do not shimmer. The
    fragment shader picks the cascade by camera depth and clamps taps inside
    its tile. Spot lights keep one whole-map view; they already had shadows.
    Evidence: GPU test `near_cascade_resolves_a_thin_shadow` (a 3 cm rod one
    unit from the camera darkens the floor to r < 30); with every cascade
    stretched to the full distance (mutation of `cascade_end`) it fails
    with r=249 and the six older shadow tests still pass. Full check:
    fmt, clippy x3 clean; `cargo test --workspace` 819 passed, 1 failed
    (`net::tests::a_handshake_must_arrive_whole_in_time_and_stay_small`
    gets `WouldBlock`, not `TimedOut`, under load; outside this item, passes
    alone); gpu-tests run 923 lib tests passed.
    Open then: point-light shadows, cascade blending, per-cascade culling.
  - Done (point lights, 2026-10-10): `PointLight` gains `shadows` (serde
    default false; schema, inspector checkbox and `docs/lighting.md`
    updated). A shadowed point light, used when no directional or spot
    light casts shadows, renders six 90-degree faces (two texels wider, so
    PCF taps stay on the tile) into a 3x3 atlas of the shadow map
    (`point_shadow_view_projections`); the fragment shader picks the face by
    the major axis away from the light. Evidence: GPU test
    `point_light_shadow_falls_through_each_cube_face` darkens the floor to
    r < 30 behind an occluder through the -X and the -Z face, leaves it lit
    without shadows and without a caster (no self-shadowing); forcing every
    pixel onto face +Z fails it (r=255). Full check: fmt, clippy x3 clean;
    `cargo test --workspace` 819 passed, 1 failed (the same net handshake
    timing test, `WouldBlock` vs `TimedOut`, outside this lane); gpu-tests
    run 924 lib tests passed.
    Not done: blending between cascades, per-cascade or per-face caster
    culling.
- [x] Clustered lighting for many point and spot lights.
  - Done (2026-10-10): `build_light_clusters` (`scene_renderer.rs`) bins
    each point and spot light's range sphere into a 16x9 screen-tile by 24
    exponential-depth-slice grid every frame, on the CPU, into one
    transient buffer (an offset/count pair per cluster, then light
    indices, at most 256 per cluster). The fragment shader shades every
    directional light, then only its cluster's lights. `MAX_LIGHTS` rises
    from 64 to 1024 (budgets Eco 256, Balanced 512, High 1024); docs,
    schema, lint text and the lint test follow. Evidence: unit test
    `light_clusters_list_only_the_lights_that_reach_them` (near, far,
    behind-camera, around-camera and off-screen lights land in the right
    clusters only); GPU test `clustered_point_lights_each_light_their_own_spot`
    (49 lights under a slanted perspective camera: every spot lit, every
    gap dark); it fails when the shader's slice count (24 to 12) or tile
    grid (16x9 to 9x16) disagrees with the CPU. `quality_profiles_compare_on_one_scene`
    now uses 600 lights. Full check: fmt, clippy x3 clean;
    `cargo test --workspace` 821 passed; gpu-tests run 926 lib tests passed.
    Not done: GPU-side binning, per-cluster brightness sorting before the cap.
- [x] Area-light approximation and light cookies.
  - Done: point and spot lights take a `radius` (sphere source, default 0).
    Specular uses Karis' representative point on the sphere nearest the
    reflection ray, with the GGX lobe widened and renormalized by
    `(a / a')^2`; diffuse is unchanged. New `rusting.light_cookie`
    component (image path) on a spot light projects the image through the
    outer cone, top along the light's +Y; up to four cookie images per
    frame in set 2 binding 8, loaded like the environment map. Schema,
    reflection, scene file, snapshot, inspector ("Source Radius"), Add
    Component text and docs/lighting.md follow. Evidence: GPU tests
    `sphere_light_widens_the_highlight_on_glossy_surfaces` (black glossy
    floor: 16 bright pixels for a point, 148 for radius 0.4; fails at 16
    vs 16 when the shader ignores the radius) and
    `spot_light_cookie_projects_its_image_onto_the_floor` (four colored
    quadrants land in the matching screen quadrants; fails when the
    image's up is flipped). Full check: fmt, clippy x3 clean;
    `cargo test --workspace` 820 passed plus the load-flaky net handshake
    test, which passes alone; gpu-tests run 928 lib tests passed.
    Not done: rectangle/tube area lights (LTC), cookies on point lights.

### Geometry and effects

- [x] GPU particle system reusing the physics compute infrastructure, with collision against physics shapes, attractors, sub-emitters, and trails.
  Done on lane/render in four steps. Step 1, GPU simulation: `rusting.particle_emitter`
  takes `simulation` (`Cpu` default, `Gpu`). A GPU emitter's fixed step only
  counts spawns (same rate, bursts, cycles and seed stream as the CPU path);
  each frame `gpu_particles.comp` respawns the ring slots the new spawns
  land in (spawn `n` in slot `n % max_particles`, up to
  `MAX_GPU_PARTICLES` = 1048576), moves every live particle with gravity,
  wind and drag, and writes its billboard straight into a vertex buffer the
  existing particle pipelines draw, so nothing is read back. Shapes,
  spread, lifetime, speed and size ranges match the CPU path; color and
  size over life are sampled at 8 points. Inspector: Simulation choice,
  Max Particles up to 1048576 for GPU. Limits: no turbulence, start
  colors, rotation, spin, `Local` space, `Velocity` facing or collision
  restarts; alpha particles are not sorted; steps by frame time, so they
  freeze while `elapsed_seconds` does. Evidence: GPU test
  `gpu_particles_spawn_move_and_die_on_the_gpu` (an 8x8 pixel red square
  spawns, moves 1 m under 100 m/s² wind in 0.1 s, and is gone past its
  0.15 s lifetime); it failed with the position update removed from the
  shader ("moved by the wind"). Unit tests
  `gpu_emitters_only_count_spawns_and_pack_their_settings` and
  `new_spawns_cap_at_one_ring_and_restart_from_zero`. Full check: fmt,
  clippy (default, `--no-default-features`, `gpu-tests`), `cargo test
  --workspace` and the gpu-tests run (1004 lib tests) pass.
  Step 2, attractors and collision, reusing the physics compute
  infrastructure: GPU particles feel every GPU force field (directional,
  radial attractors and repellers, vortex, wind gusts) through
  `field_push`, now in `src/shaders/physics_fields.glsl` and shared with
  `physics.comp`. With `collide` on, they bounce off the physics colliders
  (`physics_shapes.glsl` `shape_distance`, collider restitution). They read
  the same collider and field uploads (bindings 7 and 14) as GPU physics,
  now extracted when a GPU emitter exists. Inspector: Collide checkbox for
  GPU emitters. Limits: each particle tests every collider, ignores
  collision layers, friction and its own size. Evidence: GPU test
  `gpu_particles_feel_force_fields_and_stop_at_colliders` (a 100 m/s²
  field moves a particle 1 m in 0.1 s, then a wall from x = 0.6 holds it).
  It failed with the field push zeroed ("pushed by the field") and with
  collision disabled ("stopped by the wall"). The unit test also checks
  the packed `collide` row. Full check: fmt, clippy (default,
  `--no-default-features`, `gpu-tests`), `cargo test --workspace` (877
  lib) and the gpu-tests run (1005 lib) pass.
  Step 3, trails: `trail` (seconds, GPU emitters) keeps
  `GPU_TRAIL_POINTS` = 8 past positions per slot after its three particle
  rows, a new one every `trail / 8` seconds of age (at most one per frame).
  Each slot draws its billboard plus 8 velocity-style stretched segments
  back through the points, thinning and fading toward the tail, all in the
  same instanced draw. Inspector: Trail drag for GPU emitters. Limits:
  trail spacing is coarse when frames are longer than `trail / 8`; a
  trailing emitter needs 9 billboards per slot. Evidence: GPU test
  `gpu_particle_trails_draw_the_path_behind_each_particle` (a particle
  flying 1 m in 6 frames leaves red behind it with `trail` 0.1 s and none
  with 0). It failed with the segment size zeroed ("its trail behind it:
  0"). The unit test checks the trail row. Full check: fmt, clippy
  (default, `--no-default-features`, `gpu-tests`), `cargo test --workspace`
  (877 lib) and the gpu-tests run (1006 lib) pass.
  Step 4, sub-emitters: a GPU emitter with `on_parent_death` N under a
  parent GPU emitter gets N slots per parent slot. Each frame it steps
  right after its parent, and when a parent particle dies (the shader
  flags the death and keeps the spawn number), its N slots respawn at the
  death point with the child's own settings and a random stream from the
  parent's spawn number. One level only: a sub-emitter under a sub-emitter
  is a plain emitter. Inspector: On Parent Death drag for GPU emitters.
  Evidence: unit test
  `sub_emitters_follow_a_plain_gpu_parent_and_step_after_it` (parent
  lookup, slot count, step order, orphans and second-level emitters are
  plain); GPU test `gpu_sub_emitters_spawn_where_parent_particles_die` (a
  green parent dies at x = -0.5 and red children appear there, away from
  the child emitter's own transform, then fly 1 m in 0.1 s). It failed
  with the parent death check removed ("parent died, children spawned").
  Full check: fmt, clippy (default, `--no-default-features`,
  `gpu-tests`), `cargo test --workspace` (878 lib) and the gpu-tests run
  (1008 lib) pass.
- [ ] Particle emission driven by physics events: impacts, fracture, debris settling.
  Partial (lane/render): `ParticleEmitter` gains `impact_speed`,
  `impact_count` and `on_settle` (`emit_on_impacts` in
  `src/runtime/particles.rs`, after `update_particles`). Each contact of
  the emitter's CPU body closing at `impact_speed` m/s or faster spawns
  `impact_count` particles at the contact point, launched around the
  contact normal away from the other body; `on_settle` spawns them at the
  body when it gets `Sleeping`. Contacts come in solver order and spawns
  take the emitter's spawn numbers, so runs repeat. Inspector rows, schema
  text and `docs/effects.md` "Impacts and settling" cover it. Evidence:
  `impacts_and_settling_spawn_particles_where_the_body_hits` (a dropped
  box spawns a multiple of 6 particles at the floor flying straight up on
  the landing step, none from resting contacts, 6 more the step it
  sleeps, and the same particles on a rerun). It failed with the speed
  threshold removed and with the launch direction flipped. Full check:
  fmt, clippy x3, `cargo test --workspace` (lib 886) and the gpu-tests run
  (lib 1018) pass.
  Not done: fracture events (the engine has no fracture yet); GPU emitters
  spawn impact particles at the emitter, not the contact point.
- [ ] Decals, including decals that survive or are invalidated by fracture.
  Partial (lane/render): `rusting.decal` projects an image (or plain color)
  along its object's -Z into a box of half size `extents`, painting base
  color on surfaces facing the projection; up to 16 decals and 4 distinct
  images per frame (set 2 binding 9), later ones over earlier.
  Evidence: GPU test
  `decals_paint_their_image_on_surfaces_facing_the_projection` (image
  quadrants land on the matching screen quadrants, a decal turned away
  paints nothing, a half-alpha color decal tints only its box); unit test
  `decals_wait_for_their_image`. Mutations (flipped uv, no facing fade, no
  box test) each fail the GPU test. Full check: workspace lib 872 passed,
  gpu-tests run 994 passed.
  Not done: fracture survival or invalidation (the engine has no fracture
  yet), decal normals and roughness, decals on skinned or moving objects
  following them.
- [x] Instanced multi-mesh rendering and foliage scattering with wind driven by physics force fields.
  Evidence: `rusting.foliage` (`Foliage`) on an object with a mesh_renderer
  draws `count` copies of its mesh (at most 65536) in place of the object,
  scattered over a disc of `radius` with a seeded yaw and scale. Extraction
  expands the copies into renderables that share the owner's entity, so
  they join the existing mesh+material instanced batches and cull one by
  one; each mesh in a multi-mesh scatter is its own Foliage object. Every
  frame `PhysicsWorld::field_accelerations` samples the force fields at
  each copy's base, and the copy leans by `sway` per m/s² of push (a shear
  keeping heights, capped at 45 degrees), so Wind fields ripple foliage
  with their gusts. Unit test
  `foliage_scatters_copies_that_lean_with_wind_fields` (50 copies inside
  the disc, deterministic layout, 0.3 m/m lean under 10 m/s² wind at sway
  0.03, upright again once the field is removed); GPU test
  `foliage_copies_lean_in_wind_fields_when_drawn` (a pillar extracted
  through a real app: upright pixels lit in still air, the top shifted
  0.36 m right under wind). Mutation (lean zeroed) fails both tests.
  Full check: workspace lib 879 passed, gpu-tests run 1010 passed.
  Not done: lean in the vertex shader (CPU per copy and frame now), rule
  scattering by slope, height or mask (Milestone 20 item), lean from GPU
  physics fields alone.
- [x] Heightmap terrain rendering with texture splatting and LOD.
  `rusting.terrain` (`src/runtime/terrain.rs`) draws the object's
  `Heightfield` as chunks of `chunk_cells` cells, each a `LodGroupAsset`
  of full, half and quarter detail meshes switching at `lod_distance` and
  twice it, with skirts hiding cracks. Up to four layers (texture tinted by
  color) pack into a 2x2 atlas; the new `MaterialAsset::splat` makes the
  main fragment shader blend its quadrants by the vertex-color weights,
  from `splat[row][column]` or height bands. Built at extraction, so the
  editor shows it; rebuilt on change. Evidence: unit tests
  `terrain_splits_into_chunks_with_coarser_lod_meshes`,
  `terrain_layers_blend_by_height_bands_or_splat_weights`,
  `terrain_assets_rebuild_on_change_and_free_on_removal`; GPU test
  `terrain_paints_splat_layers_and_drops_detail_with_distance` (an orange
  sample column paints at full detail and vanishes at the quarter level).
  Mutations (splat branch off, LOD ranges infinite) fail them. Full check:
  fmt, clippy x3 clean; workspace lib 882 passed, gpu-tests run 1014
  passed (one transient doctest failure passed on rerun).
- [x] Automatic mesh LOD generation at import.
  glTF import (`import_gltf` in `src/assets/mod.rs`) gives each static mesh
  (no skin, no morph targets) of 512 triangles or more up to three coarser
  levels at about 1/2, 1/4 and 1/8 of its triangles, written next to the
  cooked mesh as `<mesh>.lodN.rmesh`, plus a `<mesh>.rlod` with screen-size
  hand-overs at 0.3, 0.12 and 0.05, which `load_mesh` picks up on later
  loads. `simplify_mesh` (`src/assets/simplify.rs`) clusters vertices on a
  uniform grid, binary-searching the finest grid that meets the target, and
  keeps winding. Evidence: `simplified_meshes_meet_the_target_and_keep_their_shape`
  (2048-triangle grid lands in 512..=1024 and 16..=64 triangles, faces still
  point up, extent kept) and `gltf_import_generates_lod_levels_for_large_meshes`
  (1152-triangle glTF gets 3+ levels each at least 25% smaller, last level
  has no `until`, and a fresh server's `load_mesh` loads the group). A
  mutation that skipped the grid search failed both. Full check: fmt,
  clippy x3, `cargo test --workspace` (lib 885) and the gpu-tests run
  (lib 1017) pass.
- [ ] Rendering of deformables, fluid surfaces, and debris produced by Milestone 11.
  Partial (lane/render): cloth sheets take a material. `rusting.cloth`
  gains `color` (linear RGBA) and `texture` (asset path); `spawn_cloths`
  gives a sheet with either set its own material (a texture that fails to
  load leaves the color), and `cloth_mesh` writes UVs from the rest shape
  (`rest_uvs`: projected on the first triangle's plane, filling 0..1, top
  row at v = 0), so the image folds and tears with the cloth. Evidence:
  unit test `cloth_uvs_follow_the_rest_grid_on_both_sides` (UVs stay on the
  rest grid on both sides after the particles move);
  `cloth_round_trips_through_scenes_hangs_on_its_holder_and_drapes` now
  round-trips color and texture and checks the drawn material's color;
  GPU tests `cloth_textures_land_upright_and_unmirrored` (a 2x2 image's
  quadrants land on the matching screen quadrants) and
  `meshes_rebuilt_in_place_redraw_where_their_vertices_went` (a mesh asset
  edited in place under the same handle redraws at its new place with
  frustum and occlusion culling, as cloth, soft-body skins and fluid
  surfaces do each tick). Mutation (v flipped) fails the UV and GPU tests.
  Full check: fmt, clippy x3, `cargo test --workspace` (lib 887) and the
  gpu-tests run (lib 1021) pass.
  Not done: debris (the engine has no fracture yet), soft-body skin
  textures, fluid surface shading beyond its material, UVs for cloth
  curved at rest.
- [x] Effect budget and quality-profile scaling for particles and decals.
  `particle_budget` and `decal_budget` (`src/rendering/scene_renderer.rs`,
  next to `light_budget`) cap each frame by the resolved quality profile:
  1048576 / 524288 / 131072 particles (High and Auto / Balanced / Eco),
  CPU particles and GPU emitter slots together, and 16 / 8 / 4 decals.
  Past the particle cap every emitter draws the same share of its
  particles, a prefix (the oldest CPU particles, whole GPU slots with
  their trails), so the kept set holds still; simulation is untouched, so
  runs repeat on every profile. `RenderCapacityDiagnostics` gains
  `dropped_particles` and `dropped_decals`. Docs: `docs/effects.md`
  "Budgets", schema text for decals and particle emitters. Evidence: GPU
  test `effect_budgets_shrink_with_the_quality_profile` (six decals paint
  at High and Balanced, four at Eco with 2 dropped; 140000 CPU particles
  plus a 131072-slot GPU emitter draw all at High and Balanced, while at
  Eco the last CPU particle, a green square, is not drawn and about
  total - budget are counted dropped). Mutations (no particle share, Eco
  decal cap at 16) each fail it. Full check: fmt, clippy x3,
  `cargo test --workspace` (lib 887) and the gpu-tests run (lib 1022)
  pass.
  Not done: per-emitter priorities, budgets by screen size or distance.

### Programmable shading

- [x] A versioned engine shader language (GLSL-based with engine includes) for surface, unlit, sky, particle, fog, and post-process shaders, with stable uniforms/built-ins.
  Partial: surface and unlit. `MaterialAsset::shader` takes GLSL headed
  `#rusting surface 1` that defines `void surface(SurfaceInput i, inout
  Surface s)`; the built-ins (uv, world_position, view_direction,
  vertex_color, time; base_color, normal, metallic, roughness, emissive)
  live in `src/shaders/surface.glsl`, and the scene fragment shader moved to
  `src/shaders/fragment/scene.frag` with the hook. The renderer splices the
  hook in, compiles it with `glslc` once per source and sample count, and
  draws those materials with their own opaque, blended and overlay
  pipelines; batches split by shader. Unknown headers and compile errors
  draw the built-in surface and land in
  `SceneRenderer::surface_shader_errors` with the user's line numbers.
  Unit test `surface_shaders_need_the_versioned_header`; GPU test
  `surface_shaders_color_their_material_per_pixel` (a uv gradient beside a
  built-in grey quad, `i.time` following the clock, version 2 and a typo
  falling back with errors naming version and `:3:`). Dropping the hook's
  base_color fails it; so did batching without the shader in the group key.
  Guide `rusting docs show guide/surface-shaders`. Full check: fmt,
  clippy x3, `cargo test --workspace` (lib 888) and the gpu-tests run
  (lib 1025) pass.
  Post-process: a `PostShader` component (`rusting.post_shader`, saved in
  scene files) holds GLSL headed `#rusting post 1` that defines `void
  post(PostInput i, inout vec3 color)` (uv, pixel, texel, time from the
  fixed tick; `scene_at(uv)` reads the HDR image). The tone-map shader
  moved to `src/shaders/fragment/tonemap.frag` with the hook after
  grading, LUT and film effects; the renderer builds one tone-map pipeline
  per source and sample count and forces the HDR copy on while it runs.
  Errors leave the image as is and land in `SceneRenderer::post_shader_error`.
  GPU test `post_shaders_rework_the_finished_image` (uv gradient,
  `scene_at` shifting the quad half an image, time following the tick,
  version 2 and a typo leaving the image unchanged with errors naming
  version and `:3:`, clearing it restores the built-in); binding the
  built-in pipeline instead fails it. Full check: fmt, clippy x3,
  `cargo test --workspace` (lib 888) and the gpu-tests run (lib 1026) pass.
  Sky: a `SkyShader` component (`rusting.sky_shader`) holds GLSL headed
  `#rusting sky 1` defining `void sky(SkyInput i, inout vec3 color)`
  (direction, time) over the procedural sky or background map, or black;
  the sky fragment shader moved to `src/shaders/fragment/sky.frag` with the
  hook, the background pass runs whenever a sky shader is set, and errors
  keep the built-in background and land in `SceneRenderer::sky_shader_error`.
  The post and sky pipelines share one per-source cache. GPU test
  `sky_shaders_paint_only_the_background` (the background turns the
  view direction's blue while the quad in front keeps its color, time
  follows the clock, version 2 and a typo leave the background with errors
  naming version and `:3:`); dropping the hook call fails it. Full check:
  fmt, clippy x3, `cargo test --workspace` (lib 888) and the gpu-tests run
  (lib 1027) pass.
  Particle: a `ParticleShader` component (`rusting.particle_shader`) next
  to a `ParticleEmitter` holds GLSL headed `#rusting particle 1` defining
  `void particle(ParticleInput i, inout vec4 color)` (corner, particle
  color, sprite, time); the particle fragment shader moved to
  `src/shaders/fragment/particle.frag` with the hook, each emitter's CPU and
  GPU draws pick alpha and additive pipelines built from its shader
  (cached per source), and errors keep the built-in look and land in
  `SceneRenderer::particle_shader_errors`. GPU test
  `particle_shaders_change_only_their_emitter` (the shaded emitter turns
  red while the other stays white, alpha 0 by corner leaves background,
  time follows the clock, a typo keeps the built-in look with the error);
  dropping the hook call fails it. Unit test
  `particle_shaders_reach_the_renderer_only_on_emitters`. The GPU-simulated
  draw path is wired the same way but not covered by its own test. Full
  check: fmt, clippy x3, `cargo test --workspace` (lib 889) and the
  gpu-tests run (lib 1029) pass.
  Fog: a `FogShader` component (`rusting.fog_shader`) holds GLSL headed
  `#rusting fog 1` defining `void fog(FogInput i, inout vec4 haze)`
  (start, direction, distance, background, time) over the `Fog` and
  `FogVolume` result; `fog.glsl` calls the hook at the end of `fog_at` and
  `fog_sky`, `hooked_glsl` now takes several hooks so the scene shader gets
  the surface and fog hooks and the sky shader gets the sky and fog hooks,
  and while a fog shader is set every material draws through the surface
  pipeline cache (keyed by surface shader, fog shader and samples) and the
  background pass runs. GPU test `fog_shaders_cover_surfaces_and_the_background`
  (the quad turns red and the background green, alpha 0.5 dims the quad,
  time follows the clock, a typo keeps the built-in look with an error
  naming `:3:`); dropping the hook call in `fog_at` fails it. Unit test
  `surface_shaders_need_the_versioned_header` covers both hook pairs.
  Full check: fmt, clippy x3, `cargo test --workspace` (lib 889) and the
  gpu-tests run (lib 1030) pass.
  Not done: reflections and environment lighting
  ignore the sky shader; scene files saving a material's
  `shader` (needs a cooked format version); the depth prepass and shadows
  ignore the surface shader; post shaders cannot read depth.
- [ ] A visual shader graph that compiles to the same language.
  Partial (lane/render): `rendering::shader_graph::ShaderGraph` is serde
  data (nodes `Input`, `Constant`, `Add`, `Subtract`, `Multiply`, `Mix`,
  `Sin`, `Fract`, `Component`; outputs onto `Surface` fields) whose
  `compile` emits `#rusting surface 1` GLSL, every node a `vec4` read only
  by later nodes. Evidence: `graphs_compile_in_node_order_and_reject_bad_wiring`
  (exact GLSL, serde round trip, forward reads, missing nodes, bad lanes
  and NaN rejected) and `surface_shaders_color_their_material_per_pixel`
  (a graph-built `mix(blue, red, uv.x)` draws the hand-written gradient's
  pixels with no shader errors; it failed with `Mix` arguments swapped).
  `docs/surface-shaders.md` "Shader graphs" covers it. Full check: fmt,
  clippy x3, `cargo test --workspace` (lib 890) and the gpu-tests run
  (lib 1031) pass.
  Not done: the editor node canvas (backlog in `docs/editor-overhaul.md`).
- [x] Shader hot reload with error reporting in the editor.
  Evidence: the renderer keys every shader build by its source text and
  drops builds no material, emitter or scene uses, so editing a shader in
  the Inspector rebuilds it the next frame. `SceneRenderer::shader_errors`
  gathers surface, sky/fog, particle, post and condition errors into the
  `ShaderErrors` resource; the editor inserts it each frame and Render
  Settings lists it under "Shader errors". `surface_shaders_color_their_material_per_pixel`
  now fixes a broken material in place and sees its error leave while an
  unfixed one stays, and the gradient return; it failed with the
  surface cache's pruning disabled. `panels_expose_missing_assets_and_physics_overflow`
  checks the panel text; it failed with the error label removed. Full
  check: fmt, clippy x3, `cargo test --workspace` (lib 889) and the
  gpu-tests run (lib 1030) pass.
  Not done: shaders read from files on disk (sources live in scene text).
- [x] Material instancing with per-instance parameter overrides.
  Evidence: `rusting.material_override` (base_color, emissive, metallic,
  roughness, uv_scale, uv_offset; null keeps the material's value) is
  extracted into `RenderWorld::material_overrides` and written into the
  per-instance upload, so overridden objects stay in their material's batch.
  GPU test `material_overrides_recolor_one_object_inside_the_shared_batch`:
  two quads share one grey material; overriding one red keeps the other grey
  at the same draw count, switching to blue redraws it, clearing restores
  grey. Removing the override from the instance-cache comparison fails it
  ("overridden: [188, 188, 188]"). Applies to the object only, not children.

### Frame structure

- [ ] A render graph that schedules passes, transient resources, and barriers automatically.
- [ ] Frame pacing that holds a stable presentation cadence under variable GPU load.
- [ ] Measure and report end-to-end input latency.

### Exit gate

- Golden images cover probes, GI fallback, SSAO/SSR, fog, bloom, tone mapping, particles, decals, and custom shaders.
- A user-written surface shader and a visual-graph shader render correctly without engine changes.
- Every effect degrades gracefully on the low-end baseline defined in Milestone 3.

## Milestone 14: Animation

Goal: match Godot's animation system and make physics-driven animation a first-class feature.

Depends on: Milestones 2, 9 (reflection), and 10 (ragdolls).

### Core animation

- [x] Skeletal animation with GPU skinning and blend shapes (morph targets). Done 2026-10-11: CPU skinning and blend shapes were items 6 and 7 of the Effects and animation track; GPU skinning is step 1 of that item's "Not done" list (below), done with a compute pass instead of skinned vertex shader variants. Tests: `gpu_skinning_draws_what_cpu_skinning_draws` (gpu-tests), `gpu_skinning_keeps_the_copy_cooked_and_lists_a_job`.
- [x] glTF skin, morph target, and animation import, completing the Milestone 2 deferral. Done in the Effects and animation track, items 6 and 7 (node clips, `rusting.skin` with `.rskin`, `rusting.morph` with `.rmorph` and weight channels). Tests: `add_model_keeps_gltf_skin_and_bends_the_mesh`, `add_model_keeps_gltf_morph_weights_and_reshapes_every_part`, `every_primitive_of_a_skinned_node_gets_the_skin`.
- [ ] Property animation of any reflected field (the equivalent of `AnimationPlayer`), including method-call and event tracks.
- [ ] Tweens for scripted interpolation with easing curves.

### Blending and control

- [~] Animation state machine with transitions, conditions, and sync groups.
  - Done (2026-10-04): `rusting.animation` gains `parameters` (name to number) and `transitions` (`AnimationTransition { from, to, parameter, compare: Above|Below|Equal, value, at_end, fade }`). Each tick without a command, `next_state` takes the first transition in list order out of the playing clip (empty `from` = any clip except `to`) whose test holds and crossfades. `at_end` waits for one full play; an ended `Once` clip can leave, a stopped one cannot. Game code: `GameScene::set_animation_parameter`; a Field track on `/parameters/<name>` keys them. Reflected, so the Inspector edits both lists. Schema example and summary, `docs/animation.md` "State machine", SKILL.md, project AGENTS template.
  - Test: `runtime::animation::tests::transitions_follow_parameters_and_clip_ends` (idle to run on speed, any to jump on Equal, jump back to idle at its end, then to run again, run to idle when speed drops).
  - Full check: fmt and clippy ×3 clean. `cargo test --workspace` passes (566 lib tests); with gpu-tests 651 lib, 26 CLI, 5 vertical slice, 17 doc tests pass.
  - Not done: sync groups; trigger parameters that reset after use (set back to 0 from game code for now); a state-machine graph editor (see Milestone 15 animation editor item).
- [x] 1D/2D blend spaces, additive layers, and bone masks.
  - Done (2026-10-04): 1D blend spaces. An `AnimationClip` with `blend: Vec<BlendPoint { clip, at }>` and `blend_parameter` mixes the two point clips around the parameter value, each stretched to the blend clip's length (1 s when 0) so cycles stay in step; outside the range the end clip plays alone. Its own tracks and markers play on top. It is an ordinary clip, so play, crossfade and transitions treat it as a state. Crossfades now share the same `mix` helper. The Timeline preview passes the whole `Animation` so it shows the mix. Reflected (Inspector), schema example and summary, `docs/animation.md` "Blend spaces".
  - Test: `runtime::animation::tests::blend_spaces_mix_neighbors_in_step` (half walk/half run at the midpoint, run stretched to 1 s, clamped at both ends, unsorted points).
  - Full check: fmt and clippy ×3 clean. `cargo test --workspace` passes (567 lib tests); with gpu-tests 652 lib, 26 CLI, 5 vertical slice, 17 doc tests pass.
  - Done (2026-10-04): 2D blend spaces. `blend_parameter_y` set on a blend clip makes it 2D; each `BlendPoint` also has `at_y`. Weights come from gradient band interpolation (`band_weights`): a point's weight is the minimum over every other point of `1 - (p - a)·(b - a)/|b - a|²`, clamped to 0..1, then all are normalized, so a point plays alone on its spot and past the outer points the nearest edge clips play; points need not form a grid. The 1D and 2D paths now share one weighted mix loop (running average via `mix`, zero weights skipped). Reflected, schema example and summary, `docs/animation.md` "Blend spaces". Test: `two_d_blend_spaces_play_points_alone_and_mix_between` (each point alone on its spot, half way between idle and run with strafe at 0, past run on its axis, an interior mix). Full check: fmt, clippy in three configurations, `cargo test --workspace` 574 lib, 25 CLI, 5 vertical slice, 17 doc; gpu-tests (`--test-threads=1`) 659 lib, 26 CLI, 5 vertical slice, 17 doc.
  - Done (2026-10-04): layers and bone masks. `Animation::layers: Vec<AnimationLayer { clip, weight, weight_parameter, additive, mask }>` play clips over the state machine, each on its own clock from scene start (seconds kept per layer in `AnimationPlayer`), in list order. Override layers lerp matching tracks by weight and write tracks the state lacks at full value. Additive layers add the clip's change from its own first frame onto tracks the state keys: positions, Euler rotations and colors add, scales multiply, `Orientation` applies `ref⁻¹·layer` (hemisphere fixed, lerped from identity by weight) in the joint's own frame. `mask` keeps writes whose target equals a path or lies under it (`Body` covers `Body/Arm`). `weight_parameter` reads the weight from a parameter so game code fades a layer with `set_animation_parameter`. Reflected (Inspector), schema example and summary, `docs/animation.md` "Layers", SKILL.md and the project AGENTS template. Test: `layers_override_masked_tracks_and_add_motion` (mask keeps the root, override at weight 0.5, additive position, additive orientation in the joint frame against the sampled key, weight parameter 0 turns it off). Full check: fmt, clippy in three configurations, `cargo test --workspace` 575 lib, 25 CLI, 5 vertical slice, 17 doc; gpu-tests (`--test-threads=1`) 660 lib, 26 CLI, 5 vertical slice, 17 doc.
  - Limits: a blend point that is itself a blend space plays only its own tracks; layer markers send no events; the Timeline preview does not show layers.
- [x] Root motion with an explicit policy that never feeds back into deterministic simulation unless routed through the physics command bridge.
  - Done (2026-10-04): `Animation::root_motion: RootMotion { Off, InPlace, Transform, Velocity }` and `root_bone` (path of the bone whose `Position` track carries the motion; empty is the object's own track). With any policy but `Off`, `extract_root_motion` pins that track's x and z at the state clip's first frame (height still plays) and measures the horizontal distance the state clip moved it this tick, across `Loop` wraps (end of cycle plus start of next) and blended with a fading clip by fade weight; the result lands in `AnimationPlayer::root_delta` (this tick) and `root_motion` (accumulated). `InPlace` only collects it: game code calls `GameScene::take_root_motion(name)` and moves the character itself, e.g. through `move_character`. `Transform` adds it to the object's position, turned by its rotation (`sim_math::rotation_from_euler`) and scaled, for objects without physics bodies. `Velocity` is the only policy that reaches the simulation, and only through the command bridge: it pushes `GpuBodyCommand::SetVelocity { linear: delta / dt (world), angular: 0 }` for the object's `PhysicsId`. Reflected (Inspector), schema example and summary, `docs/animation.md` "Root motion" and the game-code list, SKILL.md and the project AGENTS template. Test: `runtime::animation::tests::root_motion_moves_the_object_by_policy_and_holds_the_root_bone` (11 ticks of a 2 m/s loop with one wrap move a quarter-turned object 2.2 m along world -z; `InPlace` leaves it put and collects 2.2 m; the root bone keeps x and z but bobs; `Velocity` pushes one -z 2 m/s command and does not move the transform). Full check: fmt, clippy in three configurations, `cargo test --workspace` 576 lib, 25 CLI, 5 vertical slice, 17 doc; gpu-tests (`--test-threads=1`) 661 lib, 26 CLI, 5 vertical slice, 17 doc.
  - Limits: yaw (turning in place) and vertical motion are not extracted; `Velocity` zeroes angular velocity; `Transform` on an object with a CPU body is overwritten by physics; layers do not contribute root motion.
- [~] Two-bone, FABRIK, look-at, and foot-placement IK.
  - Done (2026-10-04): `rusting.ik` (`src/runtime/ik.rs`) with `LookAt` (local `forward` axis at the target, shortest arc) and `TwoBone` (analytic law-of-cosines solve of parent and grandparent, out-of-reach targets clamped, straight chains bent toward the pole or a fallback side, optional `pole` twist about root→target). `weight` nlerps local rotations; 0 skips the solve. Solved at the end of `step_animations` in scene ID then entity order, from world poses rebuilt from the `Transform` chain with `sim_math` (GlobalTransform is stale in the fixed step); trig through `sim_math::atan2`/`sqrt`, no new schedule system. Registered in scene file, reflect (Inspector entity pickers for target and pole), snapshot, schema, placement help, `docs.rs`; `docs/animation.md` "Inverse kinematics".
  - Tests: `runtime::ik::tests::two_bone_reaches_the_target_bent_or_straight` (3 targets × bent/straight chain, within 1e-3), `two_bone_bends_toward_the_pole_and_clamps_far_targets`, `look_at_points_forward_at_the_target_with_weight`.
  - Full check: fmt and clippy ×3 clean. `cargo test --workspace` passes (570 lib tests); with gpu-tests 655 lib, 26 CLI, 5 vertical slice, 17 doc tests pass (a first gpu-tests run had 6 scene_renderer tests fail inside vulkano from GPU contention; they pass alone and on rerun).
  - Done (2026-10-04, foot placement): `IkKind::Foot` with `reach` (default 0.5 m). It raycasts the CPU `PhysicsWorld` (last step's colliders) straight down from `reach` above the character's floor (nearest ancestor with `Animation`, else the top ancestor), skipping colliders in the character's hierarchy, and two-bone solves to the animated foot position shifted by the ground height minus the floor height. Full check: fmt and clippy ×3 clean; `cargo test --workspace` 571 lib; gpu-tests 656 lib, 26 CLI, 5 vertical slice, 17 doc. Test: `runtime::cpu_physics::tests::foot_ik_stands_on_the_ground_and_skips_its_own_colliders` (foot over a 0.2 m step with a collider on the foot itself lands at y 0.2).
  - Done (2026-10-04, hips and slopes): `Foot` turns the foot by the shortest arc from world up to the ground normal, applied to its pre-solve world rotation, and lowers the hips (the thigh's parent, unless that is the character) by the largest drop any of its feet needs to reach (leg length × 0.999), times weight; drops are computed from ground targets found before any hip moves. New runtime-only `IkPose { before, after }` (snapshot-registered, not saved) records each joint IK writes; next tick a joint whose transform still equals `after` goes back to `before` first, so weights below 1, tilts and hip drops never pile up on joints no clip drives. Test: `runtime::cpu_physics::tests::foot_ik_lowers_the_hips_tilts_to_the_slope_and_does_not_pile_up` (0.3 rad slope 0.28 m below the floor: foot on the hit point, foot up axis on the normal within 1e-4, hips below 0.75 m, tick 1 and tick 3 poses equal); `look_at_points_forward_at_the_target_with_weight` now checks weight 0 returns the head to its rest rotation. Full check: fmt and clippy ×3 clean; `cargo test --workspace` 572 lib; gpu-tests 657 lib, 26 CLI, 5 vertical slice, 17 doc.
  - Done (2026-10-04, FABRIK): `IkKind::Chain` with `joints` (default 3). Collects the tip and up to `joints` ancestors, pins the top joint, straightens toward out-of-reach targets, otherwise runs at most 16 FABRIK passes (stops when the tip is within 1e-4 of the chain length), then turns each joint top first by the shortest arc from its current to its new child direction, blended by weight. Fixed pass count and `sim_math` normalization keep it deterministic. Test: `runtime::ik::tests::chain_bends_every_joint_to_reach_or_straightens_toward_far_targets` (5-joint chain reaches (1, 0.8, 0.3) within 1e-3 with the top joint fixed; a far target gives a straight 2 m chain). Full check: fmt and clippy ×3 clean; `cargo test --workspace` 573 lib; gpu-tests 658 lib (run with `--test-threads=1`: a parallel run hung once and then had 7 scene_renderer tests fail inside vulkano while a game held 4.8 GB of the GPU; all pass single-threaded), 26 CLI, 5 vertical slice, 17 doc.
  - Not done: pole or joint limits for `Chain`, up-vector / twist limits for LookAt, joint angle limits, an editor gizmo that draws the target and pole lines.
- [x] Skeleton retargeting between humanoid rigs.
  - Done (2026-10-04): `runtime::retarget_clip(world, from, clip, to)` (`src/runtime/retarget.rs`) and `rusting scene retarget <scene> <from> <clip> <to> [--dry-run]`, which saves the result on the target's `rusting.animation` through the scene patch path (replacing a same-name clip; error code `RETARGET_FAILED`). Bones pair by the new `Animation::humanoid` map (`HumanoidBone { bone, path }`) or by bone name lowercased with rig prefixes (`mixamorig:`) and separators dropped; target bones without a map are found breadth first. Rest poses come from the scene transforms. Each `Rotation`/`Orientation` key becomes an `Orientation` key that keeps its turn from the source rest in the rig frame (`q_t = P_t⁻¹ · P_s q_s W_s⁻¹ · W_t`), with hemisphere continuity; hips `Position` keys are turned into the target parent frame and scaled by hip height ratio; tracks on the object itself are kept. Reflected, schema example and operation, `docs/animation.md` "Retargeting", SKILL.md, AGENTS template; editor button deferred in `docs/editor-overhaul.md`. Tests: `runtime::retarget::tests::clips_move_a_differently_built_rig_alike` (rest maps to the target's quarter-turned rest; a 0.7 rad rig-y swing gives the same rig-frame swing within 1e-5; hips on a 2× taller rig move 2× as far; unknown bones and clips handled) and CLI `scene_retarget_saves_a_clip_on_the_other_rig` (dry run, write, missing clip). Full check: fmt, clippy ×3 clean; `cargo test --workspace` 577 lib, 26 CLI, 5 vertical slice, 17 doc; gpu-tests (`--test-threads=1`) 662 lib, 27 CLI, 5 vertical slice, 17 doc.
  - Limits: bone length differences other than hip height are not compensated; position and scale keys of other bones are dropped; blend spaces go point clip by point clip; no twist or swing limits.

### Physics-driven animation

- [x] Ragdoll handoff between animation and physics on physics events, with blend-back.
  - Done (2026-10-04): `rusting.ragdoll` (`Ragdoll { bones: Vec<RagdollBone>, hit_speed, recover_after, blend_time }`, `src/runtime/ragdoll.rs`; reflected, in `rusting schema`, captured by snapshots with `RagdollState`) runs inside `step_animations` after IK, so the FixedUpdate system list and replay hashes are unchanged. Going limp (game code `GameScene::set_ragdoll(name, true)`, or a non-sensor contact or continuous-collision impact on the character's own colliders, descendants or parts at `hit_speed` m/s or more) spawns one capsule CPU body per bone, as root entities, with the velocity of the previous animated pose, each jointed to the nearest ancestor bone's body by `joint` (default `ConeTwist`) with `frame` putting X along the bone; the character's collider is taken off. While limp, bone locals follow the bodies. Recovery (after `recover_after` s, or `set_ragdoll(name, false)`) moves the root's x/z under the hips, despawns the bodies, restores the collider and blends each bone (lerp/nlerp) from the limp pose to the animated pose, or to the pose before going limp for bones no clip drives, over `blend_time`. Physics additions: `Contact::speed` (closing speed before the solve) and `PhysicsWorld::impacts()` (fast bodies stopped by CCD at fixed or kinematic colliders, which never show a contact speed). Roots and bones are visited in (SceneId, entity) and depth order. `GameScene::is_limp`; `docs/animation.md` "Ragdolls", SKILL.md, AGENTS template; editor bone generator and gizmos deferred in `docs/editor-overhaul.md`. Test: `runtime::tests::ragdolls_go_limp_on_a_hit_fall_and_blend_back_deterministically` (a 12 m/s ball knocks a two-bone character down within 30 ticks; spine body jointed to hips body; collider removed; the hips body falls below 0.8 m and the bone follows; a second run gives a bit-identical bone pose; after 1 s it stands where the hips lay within 0.2 m, bodies gone, collider back, and blends to the exact rest pose within 1e-5; `command` toggles limp and blending). Full check: fmt, clippy ×3 clean; `cargo test --workspace` 578 lib, 26 CLI, 5 vertical slice, 17 doc; gpu-tests (`--test-threads=1`) 663 lib, 27 CLI, 5 vertical slice, 17 doc.
  - Limits: CPU physics only; getting up keeps height and facing (no get-up clips yet); bone bodies may touch non-adjacent bone bodies; removing `rusting.ragdoll` while limp leaves its bodies behind.
- [x] Active ragdolls: powered articulations that track animation targets while reacting physically to hits.
  - Done (2026-10-04): `Ragdoll::muscle` (Hz; reflected, schema example) makes a `rusting.ragdoll` active. Its capsule bodies spawn on the first tick (phase `RagdollPhase::Active`). Each tick `drive` (`src/runtime/ragdoll.rs`) takes the animated bone locals as targets (bones the module wrote last tick fall back to the pose before the handoff). It turns each body toward its target relative to its parent body, and holds the top body to the animated hips, with an implicit critically damped spring `ω' = (ω + k·dt·e) / (1 + c·dt + k·dt²)` (k = (2πf)², c = 2·2πf), so it is stable at any step. Driven bodies are kept awake (`PhysicsWorld::keep_awake`). Bones then follow the bodies. A hit at `hit_speed` or `set_ragdoll(name, true)` drops the muscles (`Limp`). Recovery moves the root under the hips and ramps the muscles from 0 to full over `blend_time`. Setting `muscle` to 0 while active goes limp and then back to plain animation. Game code: `GameScene::set_ragdoll_muscle`. Docs: `docs/animation.md` "Active ragdolls", schema summary, the SKILL.md animation API row, and the AGENTS template. Test: `runtime::tests::active_ragdolls_hold_the_pose_shrug_off_pushes_and_get_back_up` (10 Hz muscles hold a two-bone character upright: spine tilt < 0.05 rad, hips within 0.05 m; a 30 rad/s shove tilts the spine past 0.1 rad and it returns within 1 s; a 0.5 rad keyed spine bend is tracked within 0.08 rad; limp, it falls below 0.5 m; recovered, it stands back up within 2 s; muscle 0 goes limp, then removes the bodies and blends). Full check: fmt, clippy ×3 clean; `cargo test --workspace` 579 lib, 26 CLI, 5 vertical slice, 17 doc; gpu-tests (`--test-threads=1`) 664 lib, 27 CLI, 5 vertical slice, 17 doc.
  - Deviation and limits: the muscles drive the maximal-joint bodies' velocities, not reduced-coordinate articulation motors. Muscles push only their own body, with no reaction on the parent, and the hips hold is a "hand of god" force, so the character cannot lose balance on its own; only `hit_speed` or game code drops it. Steady sag is about 0.05 rad on a loaded 10 Hz joint. CPU physics only.
- [ ] Procedural secondary motion (jiggle bones, tails, hair) through the physics solver rather than a separate spring system.
- [ ] Cloth attached to skinned characters (from Milestone 11).

### Exit gate

- An imported, skinned character blends through a state machine, uses IK on uneven ground, and hands off to an active ragdoll when hit.
- A property track animates a custom reflected component.
- Animation playback results are identical across runs.

## Milestone 15: Audio

Goal: provide Godot-equivalent audio, driven naturally by physics.

Depends on: Milestone 1. Physics-driven audio depends on Milestones 5 and 10.

First slice built (owner approved `kira`, 2026-10-01): `src/runtime/audio.rs` (`AudioQueue`, `AudioCommand`, `SoundId`; presentation only, not in snapshots) and `src/audio_output.rs` (kira playback, feature `audio`, on with `window`). `GameScene::play_sound`, `play_sound_looped`, `stop_sound`, `stop_all_sounds`, `set_master_volume`, `sounds_requested`; `SoundEvent`s from `rusting.sound_cue` become play requests. WAV, Ogg, MP3 and FLAC decode through kira. No device means silent, with one stderr line. Real speaker output is not verified here. Sound events are routed by the runner after each update (`route_sound_events`), not by a schedule system: adding a system changed replay hashes (`replays_reproduce_recorded_hashes_and_find_changed_input` diverged at tick 1). Headless runs keep the newest 1,024 commands and count only game-code requests. Tests: `requests_get_distinct_ids_and_drain_once`, `sound_requests_are_queued_counted_and_drained`, and the cue check in `landing_fires_one_sound_and_a_seeded_burst_that_expires`. Full check passed under lavapipe: fmt, clippy in three configurations, `cargo test --workspace` and with `--features gpu-tests` (exit 0, 16 result lines each). Not done: buses, effects, spatial, streaming, hot-plug, voice limits, offline render.

- [ ] Audio device management with hot-plug, output selection, and a no-device fallback.
  - [x] No-device fallback: `AudioOutput::new` returns `None` with one stderr line and the game runs silent (`src/audio_output.rs`). Real devices are not verifiable here.
  - [ ] Hot-plug and output selection.
- [ ] Mixer with buses, sends, volume/mute/solo, and bus effects (reverb, delay, EQ, compressor, limiter, filters).
  - [x] Named buses with faded volume (`GameScene::set_bus_volume`), pause by bus, and `BusEffect` low-pass, reverb and distortion (`set_bus_effect`). Evidence (audit 2026-10-09): `scenario::tests::the_offline_mix_reports_levels_pan_buses_and_scheduled_starts`, `bus_effects_show_change_the_mix_and_leave_state_hashes_alone`, `runtime::audio::tests::playing_lists_live_sounds_and_pauses_them_by_bus`.
  - [x] Mute and solo (`GameScene::mute_bus`, `solo_bus`); a bus keeps its volume through both. Verified by `scenario::tests::mute_and_solo_silence_buses_and_keep_their_volume` (offline mix levels per side; fails when silencing is skipped).
  - [x] Compressor and limiter: `BusEffect::Compressor { threshold_db, ratio, makeup_db }`; a ratio of 20 or more limits. Verified by `scenario::tests::a_compressor_turns_a_loud_bus_down` (bus level falls from over 0.3 to under 0.15; fails when the ratio is not applied).
  - [x] Three-band EQ: `BusEffect::Eq { low_db, mid_db, high_db }`. Verified by `scenario::tests::the_eq_cuts_only_the_band_a_tone_sits_in` (a 440 Hz tone drops under 0.2 with the mid band cut and stays over 0.3 with the high band cut; fails when band gains are not applied).
  - [ ] Sends and a delay effect. Kira fixes a delay's time when the track is built, so a delay needs its time chosen when the bus is created.
- [x] WAV, OGG Vorbis, and FLAC import; streaming playback for long assets. Evidence (audit 2026-10-09): kira decodes WAV, Ogg, MP3 and FLAC; files over the streaming size stream from disk in windows and in the offline mix (`audio_output::tests::large_files_stream_and_still_mix`; `streamed` in `audio:` reports).
- [x] 3D spatial audio with attenuation curves, doppler, and an occlusion approximation using physics raycasts.
  - [x] Pan and distance falloff from a listener (`set_listener`), sounds attached to moving entities, and occlusion by a physics raycast. Evidence (audit 2026-10-09): `runtime::audio::tests::spatialize_pans_to_the_side_and_falls_off_past_two_metres`, `scenario::tests::an_attached_sound_pans_as_its_entity_moves_and_the_listener_can_move`, `a_wall_between_listener_and_sound_lowers_its_volume`.
  - [x] Doppler: `Sound::doppler` scales pitch by the change in listener distance. Verified by `scenario::tests::doppler_raises_pitch_on_approach_and_lowers_it_going_away` (30 m/s pass: rate 1.096 then 0.92; a sound without doppler stays 1).
  - [x] Authored attenuation curves: `Sound::falloff` (`Inverse`, `InverseSquare`, `Linear`, `Off`). Verified by `runtime::audio::tests::spatialize_pans_to_the_side_and_falls_off_past_two_metres` (curve values) and `scenario::tests::a_sound_follows_its_own_falloff_curve` (mixer volume at 6 m: 1/3 inverse, 0.5 linear).
- [x] Reverb zones tied to physics volumes: `rusting.reverb_zone` on a sensor collider. Verified by `scenario::tests::a_reverb_zone_adds_reverb_while_the_listener_is_inside` (reverb on `world` while the camera is inside, off after it leaves).
- [x] Event-driven playback triggered by gameplay events and by GPU physics events.
  Gameplay code calls `SoundCue::trigger` or `play_sound`; `SoundCue::on_gpu_event` plays on a named GPU physics event for its body. Evidence: `runtime::tests::a_gpu_physics_event_plays_the_cue_that_names_it` (mutation-checked: matching every event plays both bodies' cues).
- [x] Impact sounds scaled by contact closing speed: `SoundCue::full_volume_speed`. Verified by `runtime::tests::harder_landings_play_louder` (a 6 m drop plays louder than a 1 m drop; 0 ignores speed).
- [ ] Scrape and roll sounds while bodies slide or roll, and sound choice by physics material.
  - [x] Scrape and roll loops: `rusting.slide_sound` loops a clip whose volume follows the speed across a contact. Verified by `runtime::tests::a_sliding_crate_loops_its_scrape_until_friction_stops_it` (a shoved crate starts the loop at full volume, fades as friction slows it, and stops; fails when the stop is skipped).
  - [ ] Sound choice by physics material. Needs the Milestone 11 physics materials first.
- [x] Voice limiting and priority so large destruction events do not exhaust the mixer. Evidence (audit 2026-10-09): `GameScene::set_bus_voice_limit` and per-sound `priority`; `scenario::tests::a_thousand_sounds_in_one_tick_stay_within_the_voice_limit` and `runtime::audio::tests::bus_voice_limits_clamp_and_inactive_effects_report_off`.
- [x] Offline render path for deterministic audio tests. Evidence (audit 2026-10-09): `OfflineMixer` renders each scenario tick with kira and no device, and scenario `audio:` checks read its level, peak, pan and playing list (`scenario::tests::the_offline_mix_reports_levels_pan_buses_and_scheduled_starts`).
- [x] Audio state is presentation only and never affects simulation or replay hashes. Evidence (audit 2026-10-09): `AudioQueue` is not in snapshots or state hashes and sound events are routed by the runner, not a schedule system; `scenario::tests::bus_effects_show_change_the_mix_and_leave_state_hashes_alone` compares state hashes with and without audio effects.

### Exit gate

- An offline-rendered mix matches a reference buffer within tolerance.
- A physics scene with thousands of contacts produces voice-limited impact audio without dropouts.
- Disabling all audio does not change replay hashes.

## Milestone 16: Runtime UI, text, and localization

Goal: give games a runtime UI toolkit equivalent to Godot's Control system.

Depends on: Milestones 1, 3, and 9.

- [ ] Runtime UI layer independent of the editor's egui usage, rendered through extraction.
- [ ] Layout containers: box, grid, margin, scroll, split, tab, and anchors/offsets for free placement.
- [ ] Widgets: label, button, toggle, slider, text input, dropdown, list, tree, progress bar, image, and panel.
- [ ] Text shaping with Unicode, bidirectional text, font fallback, SDF font rendering, and rich text markup.
- [x] Themes and styles editable as data assets. `assets/ui/theme.json`
  (`UiTheme`: button, hover, pressed and panel fills, text colour, corner
  radius, button padding; unknown fields rejected) restyles the runtime UI
  at start, and `rusting lint` judges button contrast against its fill.
  Evidence: `runtime::ui::tests::a_theme_restyles_buttons_and_panels`,
  `project_runner::tests::a_ui_theme_file_styles_the_runtime_ui` (a typo
  fails the start naming the field) and
  `cli::hint_tests::lint_measures_hud_text_that_runs_past_the_view` (light
  theme flags white button text; mutation-checked against ignoring the
  fill). Hot reload of the theme while the game runs is not done.
- [ ] Focus navigation for keyboard and gamepad, and input routing between UI and gameplay.
- [x] Data-bound HUD elements and world-space indicators. Data binding is
  the `{counter}`, `{state:}`, `{dialogue:}` and `{tr:}` placeholders
  (`hud_text_fills_counter_placeholders`). `rusting.hud` `follow` and
  `follow_offset` pin an element over a named object through the active or
  named camera, hidden behind it; evidence
  `runtime::tests::a_hud_element_follows_an_object_on_screen`
  (mutation-checked against dropping `follow_offset`).
- [x] Scaling across resolutions, aspect ratios, and DPI settings. DPI was
  already covered by egui-winit's native scale. `project.json`
  `ui_base_size` lays the HUD out for a base screen and scales it uniformly
  to fit the window (Godot `canvas_items` stretch, aspect kept); headless
  runs scale from the viewport and `find_text` reports window pixels.
  Evidence: `runtime::ui::tests::a_base_size_scales_the_ui_to_fit_the_screen`
  (2x at twice the base, height-limited on a wider screen, unchanged
  without a base) and `project_runner::tests::project_json_window_size_is_asked_for`
  (a 1920x1080 window at 1.5x DPI zooms a 960x540 base to 2 pixels per
  point).
- [x] Localization: translation tables, pluralization, locale switching at runtime, and extraction of translatable strings.
  - [x] Translation tables, one/other plurals and runtime locale switching. `GameScene::set_locale(locale)` reads `assets/locales/<locale>.json` (key to text) into the `Translations` resource, keeping the old locale on a bad file; `tr(key)` and `tr_count(key, count)` read it, with the key as the fallback; HUD text's `{tr:key}` placeholders follow the locale each frame and may hold counter placeholders. Verified by `set_locale_switches_translations_and_keeps_them_on_a_bad_file` and `hud_text_fills_counter_placeholders` (translated HUD text with a counter, a missing key, plurals).
  - [x] Per-locale plural rules beyond one/other.
    Evidence: `Translations::plural` picks the CLDR integer category for the locale's language (French 0 and 1, Russian-family and Polish one/few/many, Czech/Slovak few, CJK other) and falls back to `key.other`; `runtime::tests::hud_text_fills_counter_placeholders` checks French 0/1/2 and Russian 1, 21, 11, 3, 22, 12, 5, 0.
  - [x] Extraction of translatable strings: a CLI command that lists `{tr:key}` and `tr("key")` uses missing from each locale file.
    Evidence: `rusting lint` warns `LINT_MISSING_TRANSLATION` per key; `cli::tests::lint_flags_translation_keys_a_locale_lacks` covers `tr`, both `tr_count` forms, a HUD `{tr:key}` and a `to_str("` look-alike.
- [ ] Accessibility: screen-reader metadata, scalable text, and color-blind-safe defaults.
  - [x] Scalable text. `GameScene::set_text_scale(scale)` (0.5 to 3, 1 as authored) multiplies HUD text and caption sizes; `text_scale()` reads it back for game menus.
    - Verified (2026-10-09): `runtime::tests::text_scale_enlarges_hud_text` (a top-left label's centre moves down by 2x at scale 2) and `project_runner::tests::text_scale_is_clamped_and_read_back` (default 1, clamp to 3, NaN resets to 1).
  - [ ] Screen-reader metadata. egui's AccessKit output needs the `accesskit` feature and a platform adapter, which are new dependencies; ask the owner first.
  - [x] Color-blind-safe defaults: a default HUD and debug palette that stays distinct under deuteranopia and protanopia, checked by a simulated-vision test. The runtime HUD defaults to one colour (white), so the palettes that carry meaning are the editor's: gizmo and theme axis colours now differ in lightness (X red leans pink, Y green leans yellow), and warning/error and active/selected/plain wireframe colours were already apart.
    - Verified (2026-10-09): `editor::view::gizmo_tests::axis_colors_survive_color_blindness` simulates protanopia and deuteranopia with the Machado 2009 matrices and requires CIE76 delta E of at least 25 between every pair; the old gizmo X/Y pair was 18.8 apart under deuteranopia.
- [x] Input remapping UI component built on the action map. A HUD button whose text is `{binding:action}` shows the scene's `rusting.input_action` inputs for that action joined with ` / `; clicking it shows `Press a key` (the `binding.waiting` translation) and the next key pressed becomes the action's only input through `GameScene::rebind`, Escape cancels. Bindings made in code with `ActionMap` are not shown, the same as `GameScene::binding`.
  - Verified (2026-10-09): `runtime::tests::binding_placeholders_show_the_inputs_or_the_wait` and `project_runner::tests::clicking_a_binding_button_rebinds_to_the_next_key` (click, then KeyJ rebinds; a later key does nothing; Escape cancels).
- [x] Decouple input sampling rate from the simulation tick without introducing nondeterminism. Window events are recorded as they arrive with their time in fractional ticks (`press_tick`); a frame that runs several fixed ticks now gives each timed press to the tick it happened in, and a press timed after the frame's last tick waits for the next tick instead of landing early. Untimed presses (gamepads) and releases go to the frame's first tick as before. The split depends only on the recorded press times, which replays store with each frame's input, so it is deterministic.
  - Verified (2026-10-09): `project_runner::tests::a_timed_press_reaches_the_tick_it_happened_in` (one 3-tick frame with presses at 1.5 and 3.2: tick 1 sees the first, tick 3 in the next frame the second; before the change tick 0 saw both) and `a_tick_function_sees_a_press_from_a_frame_without_a_tick` still passes. `rusting test` on metronome-mines (including `w2_subtick`, judged -8 ms) and forever-bear reports no failing scenario.

### Exit gate

- A menu, settings screen with rebinding, and HUD can be built from data assets and work with mouse, keyboard, and gamepad.
- Golden images cover layout, text shaping (including right-to-left), and theming.
- Switching locale at runtime updates every translated string.

## Milestone 17: 2D engine

Goal: make RustingEngine a complete 2D engine, not a 3D engine with a flat camera.

Depends on: Milestones 3, 4, and 10 (2D physics).

- [ ] 2D renderer with sprites, sprite batching, z-ordering, and canvas layers.
- [ ] Sprite sheets and sprite animation.
- [ ] Tilemaps with multiple layers, autotiling rules, tile collision shapes, and tile navigation data.
- [ ] 2D lights, shadows from occluder polygons, and normal-mapped sprites.
- [ ] 2D GPU particles with physics-event emission.
- [ ] 2D camera with smoothing, limits, and pixel-perfect mode.
- [ ] 2D hybrid GPU physics showcases (thousands of physics sprites with events to Rust gameplay).
- [ ] Mixing 2D layers over or inside 3D scenes.

### Exit gate

- A 2D platformer template with tilemap, lights, particles, and physics is playable and editable in the editor.
- Golden images cover sprites, tilemaps, and 2D lighting.

## Milestone 18: Navigation

Goal: provide Godot-equivalent navigation for 2D and 3D, with runtime updates that respond to physics.

Depends on: Milestones 5 and 10.

- [ ] Navigation mesh baking from collision geometry for 3D, and from tilemaps/polygons for 2D.
- [ ] Runtime incremental re-baking of changed tiles within a per-tick budget.
- [ ] Path queries, off-mesh links (jumps, ladders, doors), and area costs.
- [ ] Navigation agents with path following and velocity-obstacle avoidance.
- [ ] Obstacles derived from physics bodies, including moving and sleeping bodies.
- [ ] Navigation debug visualization and profiler counters.
- [ ] Deterministic query and avoidance results for deterministic projects.

### Exit gate

- Agents route around physics bodies that are knocked into their path at runtime.
- Re-baking stays within its per-tick budget and reports overruns.
- Navigation results are identical across runs.

## Milestone 19: Networking and multiplayer

Goal: provide Godot-equivalent high-level multiplayer, plus deterministic rollback that Godot cannot offer.

Depends on: Milestones 8 and 9. The Milestone 8 result decides whether rollback is offered across vendors.

- [ ] Transport abstraction with UDP (reliable and unreliable channels), WebSocket, and loopback for tests. Partly done 2026-10-10: loopback. `NetSession::loopback(clients)` returns a host and clients joined in one process with no sockets or threads (a third `Route` beside direct TCP and relay), taking the same `send`, `broadcast`, `poll`, `peers`, `simulate` and `stats` calls; the host's first poll reports each client `Connected`, a dropped client is `Disconnected`, a dropped host sends `Closed`. Docs: `docs/networking.md` "Tests without sockets". Evidence: unit test `net::tests::loopback_sessions_act_like_a_host_and_its_clients`. Full check: fmt, clippy (default, no-default-features, gpu-tests), rustdoc, tests (lib 824, 925 with gpu-tests) green. Partly done 2026-10-10: unreliable UDP channel. `send_unreliable` and `broadcast_unreliable` send up to `MAX_UNRELIABLE` (1200) bytes over a UDP socket on the host's TCP port; the host hands each client a random token in WELCOME (`PROTOCOL_VERSION` 3), datagrams carry token and sender id so strangers cannot inject, and must come from the client's TCP IP address so a sniffed token cannot redirect the host's datagrams at another machine, and the host learns a client's address from its first datagram. With no UDP route (relay, loopback, address not yet known) they go reliably. `simulate` drops them at the loss rate instead of resending, out of order. Docs: `docs/networking.md` "Unreliable messages". Evidence: unit test `net::tests::unreliable_messages_go_over_udp_and_loss_drops_them` (UDP both ways, 12-byte header in stats, size cap, loss drops, spoofed source IP ignored, loopback fallback). Full check: fmt, clippy (default, no-default-features, gpu-tests), rustdoc, tests (lib 825, 926 with gpu-tests) green. Left open: WebSocket, deferred until there is a web client to talk to (web export is a post-1.0 decision, Milestone 21; desktop peers use TCP and UDP), and reliable messages over UDP (they stay on TCP).
- [x] Versioned wire protocol with compatibility rejection at connect time. Evidence: unit test `a_host_refuses_another_protocol_version` (a join with version 99 gets a REJECT naming it). The TCP transport, listen-server and relay room codes from the Rusting Raft track below are the first step of the transport and topology items; UDP channels, WebSocket and dedicated-server builds are still open.
- [x] Remote procedure calls on entities with authority checks and reliability modes. Evidence 2026-10-10: `net::rpc::Rpcs` registers methods with an `Authority` (`Host`, `Owner`, `Anyone`) and a `Reliability` (`Reliable` over TCP, `Unreliable` over UDP); `call` sends a call on a scene object by its unique name (client to host, host to every client), `accept` returns the checked `RpcCall`, an error for a refused or malformed one, or `None` for a plain game message. The host checks each client call against its own owner table (`set_owner`, host by default), so a forged call is refused. Docs: `docs/networking.md` "Remote procedure calls", Godot guide `@rpc` row. Evidence: unit test `net::rpc::tests::the_host_checks_who_may_call_each_method` (owner call accepted, non-owner refused locally and on the host, host-only and unknown methods and a truncated call refused, host call reaches both clients, game bytes pass through). Full check: fmt, clippy (default, no-default-features, gpu-tests), rustdoc, tests (lib 826, 927 with gpu-tests) green.
- [x] Replicated spawning and component synchronization, configured through reflection, with delta compression and quantization. Evidence 2026-10-10: `net::replicate`. The host marks objects `Replicated` and lists components by registry name (plus `transform` and `name`); `Replication::delta` reads them through reflection (scene form), rounds fields under `quantize(pointer, step)`, and returns only new objects, changed components, removed components and despawns since the last call, or nothing; `full()` catches up a late joiner. `Replica::apply` on a client matches objects by `SceneId` (updating ones loaded from the same scene in place, spawning others) and passes other messages by. Delta granularity is per component; messages are JSON; entity and handle references are not remapped yet. Docs: `docs/networking.md` "Replicated objects", Godot guide row. Evidence: unit tests `net::replicate::tests::deltas_spawn_update_and_despawn_quantized_objects` (only marked objects travel, rounding, no message for no or sub-step change, only the changed component in an update, late joiner, component removal, despawn, bad and foreign messages) and `objects_from_the_same_scene_update_in_place`. Full check: fmt, clippy (default, no-default-features, gpu-tests), rustdoc, tests (lib 828, 929 with gpu-tests) green. Hardened 2026-10-10 after security review: a `Replica` (built from the `Replication`) takes messages only from `HOST`, only for listed components, touches only objects marked `Replicated`, caps tracked objects at `MAX_OBJECTS`, and validates a whole message before changing anything; test `a_replica_refuses_what_the_host_may_not_send`. Full check green (lib 829, 930 with gpu-tests).
- [x] Client-server and listen-server topologies; headless dedicated-server builds. Evidence 2026-10-10: `NetSession` already gives client-server (star through the host) and listen-server (the host plays too) sessions. New: `RUSTING_SERVER` (`project::SERVER_ENV`) makes `run_project` call `run_project_server`, which loads the scene with no window, Vulkan device or renderer, inserts the `net::DedicatedServer` resource, and ticks one fixed step per fixed step of real time (sleeping to a deadline, dropping lost ticks after a stall of 10 steps) until game code quits. `rusting run --server` sets it, counts as headless for `--record`, and is in `rusting schema`; an exported game takes `RUSTING_SERVER=1`. Docs: `docs/networking.md` "Listen servers and dedicated servers". Evidence: unit test `project_runner::tests::a_dedicated_server_ticks_in_real_time_until_the_game_quits` (the resource is present, the game quits after 6 ticks, the run takes at least 5 fixed steps of wall time). Full check: fmt, clippy (default, no-default-features, gpu-tests), rustdoc, tests (lib 830, 931 with gpu-tests) green.
- [x] Client-side prediction and reconciliation for player-controlled entities. Evidence 2026-10-10: `net::predict::Prediction<I>`. The client `push`es each tick's input (and applies and sends it itself); `reconcile(acked_tick, host_state, step)` forgets inputs the host applied and replays the rest on the host's state with the shared step function, so a right prediction changes nothing and a wrong one is fixed. At most `MAX_PENDING` (240) inputs are kept. Generic over state and input, so it serves a transform, a body or a whole player struct; correction smoothing is left to game code. Docs: `docs/networking.md` "Predicting the player's own object". Evidence: unit test `net::predict::tests::reconcile_replays_unconfirmed_inputs_on_the_host_state` (agreeing host, pushed-back host, stale reply, input cap). Full check: fmt, clippy (default, no-default-features, gpu-tests), rustdoc, tests (lib 831, 932 with gpu-tests) green.
- [ ] Deterministic rollback netcode for physics-heavy games, using Milestone 8 snapshots and input streams. Blocked 2026-10-10 (lane tools): rollback restores the same app to an earlier tick, but `App::restore` refuses an app that has used more entity indices than the snapshot or raised a generation past it (bevy generations only go up), so any spawn between the snapshot and the rollback breaks it. Needs the runtime to rewind entity allocation (or to keep simulation identity off `Entity`) first.
- [x] Physics-aware replication: replicate commands and events instead of full body state wherever determinism allows. Evidence 2026-10-10: objects marked `net::replicate::Simulated` on every peer have their transform left out of replication and checksums (other listed components still travel), and `net::lockstep` sends what happened instead: the host calls `lockstep::send(session, tick, hash, &commands)` each tick, reliably, with a game-computed hash of the state before the tick; a client's `CommandStream::accept` buffers them (host only, no repeats, at most `input::MAX_AHEAD` ahead) and `ready()` hands out each tick's commands in order only once they arrived, so the client runs the same ticks with the same commands and compares hashes. Repair of a mismatch is left to the game (drop `Simulated` and transforms replicate again). Docs: `docs/networking.md` "Commands instead of bodies". Evidence: unit test `net::lockstep::tests::clients_run_the_hosts_commands_instead_of_receiving_bodies` (body never in a delta, client reaches the host's state and hash every tick, repeated, far-future, client-sent and malformed commands refused, game bytes pass). Full check: fmt, clippy (default, no-default-features, gpu-tests), rustdoc, tests (lib 874, 991 with gpu-tests) green.
- [x] Network simulation (latency, jitter, loss) for testing, and a network profiler. `NetSession::simulate(NetConditions { latency, jitter, loss, seed })` holds incoming events before `poll` returns them; loss keeps TCP semantics (a lost message waits one more round trip, at least 200 ms, and holds back the ones behind it), and a seeded SplitMix64 makes delays repeat. `NetSession::stats()` returns `NetStats` (messages and wire bytes each way, events held). Docs: `docs/networking.md` "Testing a bad connection". Evidence: unit tests `net::tests::simulated_conditions_delay_events_in_order` (latency and jitter bounds, order under 100% loss, same seed same delays) and `a_session_counts_traffic_and_holds_messages_under_latency` (a real host holds a message 300 ms; byte counts include the 9-byte header). Also fixed: a handshake deadline that expired inside a read reported `WouldBlock` (Unix's read timeout) instead of `TimedOut`, which made `a_handshake_must_arrive_whole_in_time_and_stay_small` flaky under load. Full check 2026-10-10: fmt, clippy (default, no-default-features, gpu-tests), tests (lib 822, 923 with gpu-tests) green. Left open: a profiler panel in the editor; the counters are there for a HUD.
- [ ] HTTP client for services and downloads. Partly done 2026-10-11: plain HTTP/1.1 on std only. `net::http::{get, post, request}` block for a whole response (status, lower-case headers, body capped at `MAX_BODY` 64 MiB), reading Content-Length, chunked and read-to-close bodies and following up to `MAX_REDIRECTS` (5) redirects (303 and 301/302 turn into GET, 307/308 resend the body); `download(url, path)` streams to `<path>.part` and renames it only when whole and 2xx; `fetch(..)` runs a request on its own thread and returns a `PendingHttp` a game polls once a frame. Methods and header names must be tokens, and header values and URLs (redirect targets included) may hold no spaces or control characters, so nothing can inject a header; URLs with a user name are refused; the caller's headers go only to the first URL's host and are dropped once a redirect leaves it (unit test `caller_headers_stay_with_the_first_host_across_redirects`, added after a security review of the first commit); connects and reads time out after 30 s. Docs: `docs/networking.md` "HTTP requests" and "Limits". Evidence: unit tests `net::http::tests::requests_read_lengths_chunks_and_closes_and_follow_redirects`, `downloads_land_whole_or_not_at_all_and_fetch_runs_off_thread` and `urls_and_headers_that_cannot_be_sent_fail_before_connecting` against a local TCP server (passed 8 runs in a row). Full check: fmt, clippy (default, no-default-features, gpu-tests), rustdoc, tests (lib 878, 995 with gpu-tests) green. Blocked on owner: HTTPS needs a TLS dependency (for example `rustls` with `webpki-roots`, or `ureq`), which AGENTS.md says needs approval; until then `https://` URLs fail with `ErrorKind::Unsupported`.

### Exit gate

- A physics sandbox runs with four clients under simulated 150 ms latency and 2% loss without visible desync.
- Rollback reproduces identical simulation state on every peer in a deterministic test.
- RPC, spawning, and synchronization have loopback tests.

## Milestone 20: Editor parity

Goal: bring the editor to Godot-level completeness on top of the Milestone 6 foundation.

Depends on: Milestone 6, and the milestone that owns each edited subsystem.

### Workflow

- [ ] Remote scene tree and inspector for a running game process.
- [ ] Runtime debugger panel: errors with stack traces, monitors (FPS, memory, physics, rendering counters), and custom monitors.
- [ ] Unified profiler with CPU spans, GPU pass timings, physics stages, network traffic, and memory.
- [ ] Project settings editor, input map editor, and per-asset import settings dock with re-import.
- [ ] Multiple scene tabs and multiple viewports (split views, orthographic views).
- [ ] 2D viewport with snapping and pixel grid.
- [ ] Search across the project: files, entities, components, and settings.
- [ ] Version-control integration showing changed files and scene diffs.

### Specialized editors

- [ ] Animation editor with timeline, curves, onion skinning, and state-machine graph.
- [ ] Shader editor and visual shader graph editor.
- [ ] Tilemap and tile-set editor.
- [ ] Grid/modular level editing and constructive solid geometry blocking tools.
- [ ] Particle system editor with live preview.
- [ ] Theme and UI layout editor.
- [ ] Audio bus editor.
- [ ] Navigation baking controls and visualization.

### Extensibility

- [ ] Editor plugin API for custom panels, inspectors, gizmos, importers, and tools, loaded from project crates.
- [ ] Asset library browser for installing templates, plugins, and assets into a project.
- [ ] In-editor API documentation for engine and reflected project types.

### Exit gate

- Every subsystem milestone's authoring tasks can be completed without leaving the editor.
- A third-party editor plugin adds a panel, a custom inspector, and an importer without engine changes.
- Editor state, layouts, and settings persist across sessions.

## Milestone 21: Platforms, export, and distribution

Goal: ship games to every supported platform from the editor.

Depends on: Milestones 3 and 6.

- [ ] Export presets per platform with feature flags, asset filters, and encryption of packed data. Partly done 2026-10-11: presets, feature flags and asset filters. `project.json` `export_presets` maps a name to an `ExportPreset`: `target`, Cargo `features` of the game crate, and `exclude` patterns (`*` within one name, `**` any folders; unknown keys are refused). `rusting export <project> <folder> --preset <name>` builds with them, and `project::remove_excluded` deletes matching files under `assets/` and `scenes/` only, before the headless verify run. The JSON reports `features` and `excluded`. An unknown name fails with `EXPORT_PRESET_UNKNOWN`, which lists the known ones. Docs: `docs/assets.md` "Export presets", `rusting schema` (project.json and the export usage). Evidence: unit tests `project::tests::export_excludes_match_paths_and_leave_the_rest` and `cli::shape_tests::an_unknown_export_preset_fails_and_names_the_known_ones`. By hand, a game with a `loud` feature and a preset `{"features": ["loud"], "exclude": ["**/*.psd"]}` exported `ok`: the verify run printed the feature-gated line, `assets/art/hero.psd` was left out and `assets/art/keep.txt` kept. Full check: fmt, clippy (default, no-default-features, gpu-tests), rustdoc, tests (lib 886, 1003 with gpu-tests) green. Left open: encryption of packed data, which needs a cipher dependency (owner approval) and the packed archive item first. The editor's export button does not offer presets yet.
- [ ] Packed asset archives with compression and streaming.
  - [x] Pack format, streaming reader and runtime fallback. Done 2026-10-12: `assets::pack_folder` writes an `RPAK` archive (magic, a sorted index of `/`-separated paths with offsets and lengths, then the file bodies; std only, the same folder gives the same bytes). `assets::PackArchive` checks the index (magic, ranges inside the file, no absolute, `\\` or `..` paths) and streams one file through a seeked, length-limited reader without reading the rest. `assets::read_file` and `assets::file_len` read a file that is missing on disk from `<folder>.rpak` beside any of its parent folders, with the opened index cached, and a loose file still wins. Asset loads (textures through the in-memory decoder, HDR, meshes, cooked assets, LOD groups, data files), scenes in file and folder form, project text and fonts, HUD fonts, and audio clips go through them; a packed clip decodes from the encoded bytes in memory (`from_cursor`), whole or streamed. Export preset `pack: true` packs the exported `assets/` and `scenes/` into `assets.rpak` and `scenes.rpak` after excluded files are removed and deletes the loose folders; the export JSON lists `packed`. Docs: `docs/assets.md` "Export presets", `rusting schema`. Evidence: unit tests `assets::pack::tests::a_packed_folder_reads_back_each_file_and_streams_one`, `a_corrupt_or_unsafe_pack_is_refused`, `a_missing_file_is_read_from_the_pack_beside_its_folder` (a texture loads through `AssetServer` from the pack, a missing path is `NotFound`, a loose file wins), `packed_scenes_load_in_file_and_folder_form`, `audio_output::tests::a_packed_clip_plays_whole_and_streamed`, and `project::tests::export_excludes_match_paths_and_leave_the_rest` (`pack_export` leaves only the two packs and both read back). Full check: fmt, clippy (default, no-default-features, gpu-tests), tests (lib 1122 with gpu-tests) green. Left open: a glTF model imported while the game runs (`add_gltf`) writes cooked meshes beside its source and does not work packed; the pack index is cached for the run.
  - [ ] Compression of packed files. Needs `flate2` (already in the lock through `png`) as a direct dependency; asked in `issues.md`.
- [ ] Texture compression per platform (BCn desktop, ASTC/ETC2 mobile) through KTX2.
- [ ] Windows and Linux release polish: installers/archives, icons, and code-signing hooks. Partly done 2026-10-11: icons and code-signing hooks. `project.json` `icon` (a PNG, kept under `assets/` so exports carry it) becomes the game window's icon through `WindowRequest`. A file that cannot be read warns and keeps the system icon. An export preset's `sign` command (program and arguments, `{exe}` replaced by the exported executable) runs in the project folder before verification. If it fails, the export folder is deleted and export fails with `EXPORT_SIGN_FAILED`; the JSON reports `signed`. Docs: `docs/assets.md` "Icon" and "Export presets", `rusting schema`. Evidence: unit test `project_runner::tests::project_json_window_size_is_asked_for` (a 32x32 PNG loads as the icon, a broken file leaves none). By hand on a test game: preset `sign: ["sh", "-c", "echo signed > \"$0.sig\"", "{exe}"]` exported `ok` with `signed: true` and `boom.sig` beside the executable. A sign command that exits 3 failed with `EXPORT_SIGN_FAILED` (`no key` from its stderr) and left the parent folder empty. Full check: fmt, clippy (default, no-default-features, gpu-tests), rustdoc, tests (lib 886, 1003 with gpu-tests) green. Left open: installers and archives (zip or tar.gz needs a compression dependency, so it needs owner approval; a Windows installer needs NSIS or WiX), and the icon embedded in the `.exe` (needs a resource compiler such as `winres`).
- [ ] Steam Deck/Proton verification.
- [ ] macOS through MoltenVK, with capability fallbacks for missing Vulkan features.
- [ ] Android export with touch input, lifecycle handling, and mobile quality profiles.
- [x] Platform services abstraction for save paths, achievements, and storefront SDK hooks. Done 2026-10-11: `platform::PlatformServices` (name, optional own `save_folder`, `unlock_achievement`, `achievement_unlocked`, `set_stat`, `stat`) is the hook a storefront SDK implements in the game crate and installs with `platform::set_platform_services`. Game code calls the free functions `platform::unlock_achievement`, `achievement_unlocked`, `set_stat`, `stat`, `platform_name`. The default `LocalPlatform` keeps achievements and stats in `platform.json` in the user data folder (written atomically; a broken file reads as empty). `project::user_data_folder` now uses `RUSTING_USER_DATA`, then the platform's own save folder, then `project::default_user_data_folder`, so `save_data` and crash reports follow a cloud-synced folder. `steam` docs point to `platform`; the Steamworks backend still waits on the `steamworks` dependency. Docs: `docs/menus-and-ui.md` "Achievements, stats and storefronts". Evidence: unit test `platform::tests::local_services_remember_achievements_and_a_store_can_replace_them` (unlock twice stores once, a new instance reads the file back, a broken file reads empty, an installed store takes the calls and the previous one is restored). Full check: fmt, clippy (default, no-default-features, gpu-tests), rustdoc, tests (lib 887, 1004 with gpu-tests) green.
- [x] Crash reporting with symbolicated stack traces. Done 2026-10-11: `run_project` installs `crash::install_crash_reporter` (not in unit tests). It chains the previous hook, so the panic still prints. A panic writes `crashes/crash-<unix ms>.txt` in the user data folder, holding the engine version, OS and architecture, executable, thread, panic message and location, and a `std::backtrace` trace. It prints the path, and the folder keeps the newest `MAX_CRASH_REPORTS` (20). Release builds have function names; `debug = "line-tables-only"` adds file and line. The exported README.txt names the folder. Docs: `docs/assets.md` "Crash reports". Evidence: unit test `crash::tests::crash_reports_name_the_panic_and_its_functions_and_stay_bounded` (the trace names the test's own function; other files in the folder stay). By hand, a release game that indexes past an empty Vec in `update` wrote a report with `panic: index out of bounds ...`, `at: src/main.rs:5:18` and the frame `boom::update`. The same game built with `CARGO_PROFILE_RELEASE_DEBUG=line-tables-only` added `at <file>:<line>` to each frame. Full check: fmt, clippy (default, no-default-features, gpu-tests), rustdoc, tests (lib 884, 1001 with gpu-tests) green.
- [x] Record web, iOS, and console export as post-1.0 decisions with the blocking technical reasons. Done 2026-10-11: `docs/platforms.md` ("Platforms", also `rusting docs show guide/platforms`, linked from `docs/README.md`) has a status table for every platform. For each post-1.0 platform it lists the blockers and what would unblock it. Web: a Vulkan-only `vulkano` renderer with GLSL/SPIR-V shaders, threads that need `SharedArrayBuffer`, `std::net` sockets, the file system, and a native-process test runner. iOS: the macOS MoltenVK fallbacks and Android touch and lifecycle work come first, plus memory limits and App Store packaging. Consoles: NDA SDKs that cannot live in an open repository, no Vulkan on PlayStation or Xbox, no public Rust targets or crate backends, and certification. Evidence: the docs tests (`docs::tests`, 27 passed) load the new page; `rusting docs search "web export"` finds `guide/platforms`. Full check: fmt, clippy (default, no-default-features, gpu-tests), rustdoc, tests (lib 884; gpu-tests run green).

### Exit gate

- A demo project exports and runs from the editor on Windows, Linux, and macOS.
- An Android build runs the physics sandbox template on a mid-range device.
- Crash reports from exported builds resolve to source locations.

## Milestone 22: Documentation, samples, and ecosystem

Goal: make the engine learnable and adoptable the way Godot is.

Depends on: the features being documented. Documentation for each feature lands with that feature; this milestone covers the structure and the gaps.

- [x] User manual covering every subsystem, with a dedicated physics guide that explains ownership, synchronization, determinism, and solver choice. Partly done 2026-10-11: the physics guide, `docs/physics.md`, served as `rusting docs show manual/physics` and linked from `docs/README.md` and the physics section of `docs/concepts.md`. Ownership: the four simulation classes, body kinds, and the `rusting.auto_simulation` rules and thresholds (taken from `required_class` and `AutoAllocationPolicy`). Synchronization: CPU same-tick state; GPU scene edits, commands and the tick they apply on, one-way coupling with CPU colliders, force fields, the four `rusting.physics_sync` modes with their readback cost, watch rules and event modes, `GpuStateMirror` age, class snapshots, `GpuQueryProxy`, and the limits (event overflow, no renderer, no joints, sensors or mesh colliders on the GPU). Determinism: a summary with the game-code rules, linking `docs/determinism.md`. Solver choice: a job-to-feature table, the CPU solver and substeps, the GPU solver profiles (read from `physics.comp`: `Simplified` runs the same step as `Full` today, `Custom` skips gravity and contacts), and GPU cloth. Its Rust calls (`gpu_state`, `age_ticks`, `request_gpu_class_snapshot`, `gpu_command`, `state_hash`, `set_physics_substeps`) compiled in a throwaway game; its commands pass `docs::tests::documented_commands_name_real_operations_and_flags`. Full check 2026-10-11: fmt, clippy (default, no-default-features, gpu-tests), rustdoc, tests (lib 879, 996 with gpu-tests) green. Input page 2026-10-11: `docs/input.md`, served as `rusting docs show manual/input` and linked from `docs/README.md` and the input section of `docs/concepts.md`: binding actions with `rusting.input_action`, every input name (keys, mouse, gamepad buttons, stick directions, checked against the parser), the default `player.*` bindings for keyboard and gamepad, the `GameScene` input calls, reading presses per frame or per tick with `press_tick`, focus loss, rebinding with `rebind`, `rebind_device` and `binding`, raw `RuntimeInput`, `ActionMap` and `ClickEvent`, and the scenario input steps. Its Rust snippets compiled in a throwaway game. Full check 2026-10-11: fmt, clippy (default, no-default-features, gpu-tests), rustdoc, tests (lib 879, 996 with gpu-tests) green. Assets page 2026-10-11: `docs/assets.md`, served as `rusting docs show manual/assets` and linked from `docs/README.md` and "Data assets" in `docs/concepts.md`: `asset import` (file kinds, flags, error codes), the `.rmeta` record, referring to assets from scenes (texture slots, `scene add-model`, sound paths), `asset reimport`, `asset list` checks, the four built-in generators and project generator hooks, and `export` (contents, the 60-tick check from a fresh folder, `--target`). Every command and output was run on a throwaway project (import, reimport with `--max-size`, generate texture, mesh and sfx, a texture patch, add-model, list, validate, export). The agent guides now come first among the guides, so `rusting docs --brief` still lists `guide/agent-skill` within 600 tokens. Full check 2026-10-11: fmt, clippy (default, no-default-features, gpu-tests), rustdoc, tests (lib 879, 996 with gpu-tests) green. 2D page 2026-10-11: `docs/2d.md`, served as `rusting docs show manual/2d` and linked from `docs/README.md`: the XY-plane layout, orthographic cameras, sprites (unlit `Quad`, `Mask` cutout, sprite-sheet frames through `uv_scale` and `uv_offset`, read from the vertex shader), `rusting.tile_map` (texture paths relative to `assets/`, solid runs, the editor Tile Painter and its keys, `tile` and `set_tile`), `rusting.platformer_controller` (the 0.1 s jump grace, ceilings, moving platforms, `dash`), `rusting.axis_lock`, `rusting.polygon` with `Polygon` and `Chain` colliders, the HUD, and scenario tests. Its Rust calls compiled in a throwaway game; its sprite JSON was patched into a `2d` template project and captured, showing the cut-out sprite. With this, every subsystem has a manual page or guide. Full check 2026-10-11: fmt, clippy (default, no-default-features, gpu-tests), rustdoc, tests (lib 879, 996 with gpu-tests) green.
- [x] Generated API reference for engine crates and reflected types. Reflected types: `rusting docs` now has one generated page per entity section, registered component, resource and asset type (`component/rusting.fog`, `component/transform`, `resource/rusting.physics_settings`, `asset/rusting.material`), built from the `rusting schema` catalog with each field's path, unit, range and doc plus the default and example, so `rusting docs search` finds fields and the pages cannot drift from the code (`docs::tests::every_reflected_type_has_a_reference_page` checks every registered component has one). Engine crates: `cargo doc --workspace --no-deps` had 22 broken intra-doc links (links to private items, `rotation[0]` read as links, the `reflect` module/macro ambiguity); all fixed, and CI plus `scripts/check_release.sh` now build it with `RUSTDOCFLAGS=-D warnings`. `docs/README.md` says where each reference lives. Full check 2026-10-10: fmt, clippy (default, no-default-features, gpu-tests), tests (lib 823, 924 with gpu-tests), rustdoc with `-D warnings` green.
- [x] Step-by-step tutorials: first 3D game, first 2D game, physics sandbox, multiplayer physics game. Partly done 2026-10-11: first 3D game is tutorial 1 (hello cube) with tutorials 3 and 4, first 2D game is tutorial 2 (Coin Run). New tutorial 5, `docs/tutorials/05-physics-sandbox.md` (physics sandbox), served as `rusting docs show tutorial/physics-sandbox`: Fixed and Dynamic bodies, a restitution patch with `expected` (and its conflict on a rerun), a `rusting.input_action` throw action, a test-first `throw.json` scenario, `spawn_copy` and `camera_ray` throw code, and `rusting determinism`. Every command and output was run in order on a fresh `sandbox` project (the throw test failed before the code and both scenarios passed after; determinism: debug == release on all 120 ticks). Its commands pass `docs::tests::documented_commands_name_real_operations_and_flags`; the CLI docs test brief budget rose from 500 to 600 tokens so `guide/agent-skill` still fits after the new page. Full check 2026-10-11: fmt, clippy (default, no-default-features, gpu-tests), rustdoc, tests (lib 878, 995 with gpu-tests) green. Finished 2026-10-11 with tutorial 6, `docs/tutorials/06-multiplayer-physics.md` (multiplayer physics), served as `rusting docs show tutorial/multiplayer-physics`: one executable is a dedicated server (`rusting run --server`), a client (`CRATE_DROP_JOIN=address`) or a single-player game; the host simulates and sends quantized transforms through `Replication`, clients apply them with `Replica` to kinematic copies and send throw requests, not results. New scenario field `"realtime": true` (schema and agent skill updated) runs one tick per fixed step of real time so a client scenario can talk to a live server; test `a_realtime_scenario_runs_one_tick_per_fixed_step_of_real_time`. Fixed a bug the tutorial hit: `{"counter": ..., "exists": false}` always failed because a missing counter reads as 0; `counter_shorthand_sets_checks_and_logs_a_counter_by_name` now checks it (fails without the fix). Verified on the tutorial project: its `online.json` client scenario passes against `rusting run --server` and fails offline; two clients against one server, the second saw the first one's throw; the Tutorial 5 offline tests still pass. Not verified here: windowed clients. Full check 2026-10-11: fmt, clippy (default, no-default-features, gpu-tests), rustdoc, tests (lib 879; 1120 workspace with gpu-tests, locked) green.
- [ ] Demo projects per feature area, with a physics showcase gallery (destruction, fluids, cloth, vehicles, ragdolls, million-body scenes).
- [x] Migration guide for Godot users mapping nodes, signals, resources, and scripts to RustingEngine concepts. `docs/godot-migration.md` ("Coming from Godot"), linked from `docs/README.md` and served offline as `rusting docs show guide/godot-migration`: tables for nodes, scenes and the tree, script callbacks, signals and groups, resources and autoloads, project/editor/export, plus what has no counterpart yet. Every call and component it names was checked against the API table and `rusting schema`; its Rust example built as a throwaway example (`cargo build --example`, removed after). Its `rusting ...` commands pass `docs::tests::documented_commands_name_real_operations_and_flags`. Full check 2026-10-10: fmt, clippy (default, no-default-features, gpu-tests), tests (lib 822, 923 with gpu-tests) green.
- [x] Contribution guide, plugin authoring guide, and release notes process. `CONTRIBUTING.md` now covers the three clippy runs, serial GPU tests, the dependency, determinism and snapshot-undo rules, and a "Roadmap and changelog" section (evidence on checked items, one `CHANGELOG.md` line per user-visible change under `[Unreleased]`). `RELEASE.md` gained a "Release notes" section: picking the version from the changelog, renaming `[Unreleased]`, section order, and pasting into the GitHub Release. New `docs/plugin-authoring.md` ("Writing a plugin"), linked from `docs/README.md` and served as `rusting docs show guide/plugin-authoring`: a reusable plugin crate, naming prefixes, `FixedUpdate` and determinism, `reflect!` and migrations, setup errors, actions, and unit-testing a plugin on a bare `App`. Its plugin and test code built and ran as a throwaway example (the test assertion held), removed after. Full check 2026-10-10: fmt, clippy (default, no-default-features, gpu-tests), tests (lib 822, 923 with gpu-tests) green.

### Exit gate

- A new user can build and export the first-game tutorial from a clean install using only the documentation.
- Every demo project builds and runs in CI.

## Milestone 23: Engine 1.0 release

Goal: declare Godot-class parity with physics leadership, backed by evidence.

Depends on: Milestones 9-22.

- [ ] Every row in the Godot parity map has a passing exit gate.
- [ ] Every physics pillar has its test or benchmark evidence checked in.
- [ ] Reference games built entirely with the engine: a 3D action game, a 2D platformer, a vehicle/physics sandbox, and a multiplayer physics game.
- [ ] Public API stability policy, semantic versioning, and a deprecation process.
- [ ] Performance baselines recorded for the low-end, mid-range, and high-end reference machines.

### Exit gate

- All reference games are playable from a clean checkout, editable in the editor, and exportable to every supported platform.
- The comparative physics benchmark report shows where RustingEngine leads, and any area where it does not is documented with a follow-up item.

## LLM-native track

Milestones L1-L10 make RustingEngine the best engine to build games with an LLM coding agent. They run in parallel with Milestones 9-23 and have priority while the owner's current focus (agent-built games) holds.

The starting point already exists: the window-free `rusting` CLI, the `schema_version` 1 result envelope, `rusting schema`, atomic `scene patch` batches with dry runs and revision checks, input-driven scenarios, offscreen `capture` with pixel picking, the generated project `AGENTS.md`, `skills/rusting-game/SKILL.md`, and twelve sample games whose gaps were logged and fixed. `missingFeatures.md` holds the earlier backlog; its open sections are absorbed below.

### LLM-native pillars

"Best for LLM coding" is defined by the following pillars. Like the physics pillars, each is owned by a milestone and proven by a test or by the agent benchmark (Milestone L9), not asserted.

| Pillar | What it means | Owner |
| --- | --- | --- |
| Legibility | An agent learns the whole engine surface from the installed version, in budgeted text, with no web search and no guessing from training data | L1 |
| Generation-friendly API | Game code an LLM writes compiles on the first or second try: no lifetime puzzles, no nested borrows, one obvious way to do each thing | L2 |
| Observability | Every mutation has a way to see its effect as text, structured state, an annotated image, or an event trace | L3 |
| Fast loop | Edit to verified result in seconds, through a warm daemon and a no-rebuild logic lane | L4, L8 |
| Verifiability | Behaviour is proven by scenarios, invariants, fuzzing and bots, and a scenario's strength is itself checked | L5 |
| Quality floor | A generated game looks, sounds, and feels finished by default; lints catch what a model cannot see | L6 |
| Scale | Agents stay effective on a large project and several agents can work on one project at once | L10 |
| Safety | Agent actions are scoped, journaled, reviewable and reversible by a human | L4, L8 |
| Evidence | A published, repeatable agent benchmark compares RustingEngine against Godot and others on identical requests | L9 |

### Design rules for the track

- One truth, several interfaces: CLI, daemon, MCP adapter, editor, and Rust API call the same operations and validation code.
- Small outputs by default: summaries, filters, pagination, and stable references; details on demand. Every command that can print a lot takes `--limit` and `--fields`.
- Every error names the next command or edit that fixes it.
- Text formats that diff well: sorted keys, stable ordering, one value per line where it matters for merges.
- No model lock-in: projects and tools stay fully useful with no AI service, account, or network connection. Nothing in the engine calls a model.

## Milestone L1: Machine-readable engine surface

Goal: an agent can learn everything it needs about the installed engine version from the engine itself, within a stated text budget.

Depends on: Milestone 7 agent loop (done).

### Diagnostics

- [x] Stable diagnostic codes for every validation, patch, scenario, import, and build error, kept in one registry with a uniqueness test. The existing symbolic codes (`SCENE_CONFLICT`, `ASSET_NO_LICENSE`) are kept rather than renumbered: they were already stable in the JSON envelope and a model reads a name more reliably than a number. Evidence: `src/diagnostics.rs` registers all 52 codes; `diagnostics::tests::every_emitted_code_is_registered_and_every_registered_code_is_emitted` scans `src/` for code literals and fails on an unregistered code or an entry nothing emits; `codes_are_sorted_unique_and_fully_documented`.
- [x] `rusting explain <code>` prints the cause, the fix, and an example command or patch for each code; `rusting explain` lists every code; a mistyped code is a usage error that lists codes with the same prefix; every human-readable failure ends with `Run \`rusting explain CODE\` for the cause and a fix.` The project `AGENTS.md` and the skill file mention it. Evidence: `rusting::tests::explain_describes_a_code_and_suggests_similar_ones_for_a_typo`, `diagnostics::tests::json_patch_examples_parse_as_patches`; full check green (fmt, clippy default / no-default-features / gpu-tests, `cargo test --workspace` with and without gpu-tests, lib 541 with gpu-tests). Limit: command examples starting with `rusting` are not yet parsed by a test.
- [x] Each diagnostic in `--json` output carries the file, the JSON pointer or line, the entity ID and name, and a severity. `Diagnostic` has `line`, `column`, `scene_location` (JSON pointer into `file`) and `entity` (`id`, `name`), filled wherever the tools know them: scene JSON errors give line and column; duplicate IDs and names, missing parents, parent cycles and component value errors point at `/entities/N` (or `/entities/N/components/NAME`) and the object, through the shared `runtime::error_object`; patch operation errors point at `/operations/N` of the patch file; a patch that breaks a scene rule (`PATCH_INVALID`) names the object; missing assets and determinism errors name the object; build errors give the line. The human output prints the same location. Evidence: `tests/cli.rs` `scene_and_patch_errors_point_at_the_line_object_and_operation` and `missing_asset_reference_fails_validation_with_location`; `reflect::tests` asserts `ReflectError::object` on a failed scene load; full check green (lib 541 with gpu-tests). Limit: `PATCH_INVALID` for a default or an unknown scene setting has only its message, and a patch's broken object has no pointer because it is not on disk yet.
- [x] Machine-applicable fixes: when a fix is certain (a misspelled field, a missing default section, a wrong unit), the diagnostic carries a ready `scene patch` operation, and `rusting fix` applies all certain fixes with a dry-run diff first. Done for misspelled keys, the only certain case the tools detect today. `validate` now reports every scene key that loading drops as `SCENE_UNKNOWN_FIELD` (no hits on the 16 scenes in the repository); a misspelled required field (`SCENE_JSON` "missing field") is matched to the key that misspells it. A key within two edits of exactly one unset known key, written once in the file, gets `fix: {"op": "rename_key", "path", "to"}`. The fix is a rename in the file text rather than a `scene patch` operation, because a patch rewrites the whole scene and would delete the other unknown keys the user still has to look at; and a file whose required field is misspelled does not load, so a patch cannot apply to it. `rusting fix [root] [--dry-run]` applies fixes in rounds until none are left, since the next missing field shows only after the first is fixed; `--dry-run` lists the first round and writes nothing. Evidence: `tests/cli.rs` `fix_renames_misspelled_scene_keys_in_place_and_keeps_the_rest`, `scene_patch::tests::dropped_fields_suggest_only_one_close_unset_key`; full check green (lib 542 with gpu-tests). Not done: a missing section is already filled from defaults by `scene patch`; wrong units have no detector yet; registered component strings are checked on load (`SCENE_COMPONENT_FIELD`) and get no fix.

### Version-matched reference

- [x] `rusting docs` serves the manual, the Rust API index, the schema catalog, and sample snippets offline from the installed version, with `search <query>` and `show <item>`. Done for the manual, tutorials, both agent guides, every command (from the schema catalog) and every diagnostic code; pages are embedded with `include_str!`, so the text always matches the binary. `search` needs every word and ranks id and title hits first; `show` accepts `kind/name` or an unambiguous bare name and cuts at a line to fit `--budget`. Evidence: `docs::tests` (5: unique ids with title and summary, bare-name lookup, search ranking, budget cuts) and `tests/cli.rs` `docs_lists_searches_shows_and_briefs_within_a_token_budget`; full check green (lib 547 with gpu-tests). Not done: the Rust API index and sample snippets are not items yet (next two L1 items).
- [x] `rusting docs --brief [--budget N]` prints a compact `llms.txt`-style overview sized to a token budget, with pointers to the detailed entries. The default budget is 2000 tokens (four characters each); items are listed in manual, tutorial, guide, command, code order and the tail is replaced by "N more items; `rusting docs` lists them all." Evidence: `docs::tests::budget_cuts_at_a_line_and_the_brief_fits_it` checks 300, 1000 and 4000 tokens and that a huge budget lists all; the CLI test above.
- [x] A generated Rust API index for the gameplay surface: `rusting docs` lists an `api/Type::method` item for every public method of `GameScene`, `GameObject`, `CubeSpawn` and `SphereSpawn`, with signature and doc comment (units, `# Errors`), read from `project_runner.rs` at build time via `include_str!`, so it cannot drift. Evidence: `docs::tests::api_index_lists_documented_gameplay_methods` (more than 30 items, every one with a summary); full check green (lib 548 with gpu-tests; `headless_device` tests flake under parallel load and pass alone or on rerun). Not done: the prelude free items and registered components are not indexed, and examples in doc comments are not yet run (next-but-one L1 item).
- [x] JSON Schema files (draft 2020-12) for scenes, patches, scenarios, `project.json` and `.rmeta`, emitted by `rusting schema --json-schema` (`schema::json_schemas`). Scene entity sections and registered components come from the catalog tables; patch operations list all seven ops with required fields. Evidence: `schema::tests::json_schemas_describe_every_key_the_engine_writes` (top-level keys match what serde writes for scene, patch, scenario and project), `json_schema_patch_operations_match_the_parser`, `json_schema_entity_lists_every_section_and_component`, `tests/cli.rs` `schema_json_schema_prints_a_schema_per_file_kind`; full check green (lib 551 with gpu-tests). Not done: `$schema` is not written into engine files (a scene with an unknown key reports `SCENE_UNKNOWN_FIELD`, and the cooked format is binary, so it needs a format decision); component and section values are not described below the top level (the catalog lists fields); `.rmeta` keys are not cross-checked against serde.
- [x] (Partly) Every `rusting ...` command in the schema catalog, the manual, tutorials, the skill file and the project `AGENTS.md` is extracted and checked on every test run: it must name a real operation (longest match, so `scene patch` beats `scene`) and use only flags that operation's usage lists. A stale command fails the build. Evidence: `docs::tests::documented_commands_name_real_operations_and_flags` (over 40 commands; a mutation check with a misspelled command and flag in the skill file failed it with both named); full check green (lib 552 with gpu-tests). Not done: the commands are checked, not executed, because most need a project fixture; doc-comment Rust examples in the API index are not compiled; JSON patch examples in the code registry are already parsed by `diagnostics::tests::json_patch_examples_parse_as_patches`.

### Budgeted output

- [x] `--limit N`, `--fields a,b` and `--summary` on the listing commands (`project inspect`, `scene inspect`, `scene query`, `asset list`, `preset list`, `explain`; `docs` keeps its own `--budget`). They apply to every array directly under `data`: `--fields` keeps those keys of each object, `--limit` keeps the first N and reports the cut count under `data.omitted`, `--summary` replaces each array with `{"count": N}`. A bad value is a `CLI_USAGE` error. Evidence: `cli::shape_tests::shape_limits_projects_and_summarizes_listings` and `tests/cli.rs` `listing_flags_cut_output_and_every_json_result_reports_its_size`; full check green (lib 553 with gpu-tests). Limit: arrays nested deeper than `data.<key>` are not cut, and `docs search` results are not shaped because `data.matches` already has its own limit.
- [x] Each `--json` result carries `size: {data_bytes, data_tokens}` (bytes of `data`, tokens at four characters), so an agent can decide whether to ask for more. Evidence: the CLI test above compares the size of a cut and a full result. The envelope field is additive; `schema_version` stays 1.

### Exit gate

- A fresh agent with no prior RustingEngine knowledge and no network access builds a sample-sized game using only `rusting docs`, `rusting schema`, and `rusting explain`.
- Every error the engine can emit has a code, an explanation, and a tested example.

## Milestone L2: Generation-friendly gameplay API

Goal: gameplay code written by an LLM compiles and behaves as intended on the first or second attempt.

Depends on: Milestone L1 for the API index; Milestone 9 for reflection and prefabs.

- [ ] An API audit rule, enforced by a test over the public gameplay surface: no public gameplay function returns a borrow that blocks another `GameScene` call, and no gameplay type needs an explicit lifetime. Handles are `Copy` IDs. Partly done: `docs::tests::gameplay_methods_do_not_return_new_borrows` fails on any new public `GameScene`, `GameObject`, `CubeSpawn` or `SphereSpawn` method returning a borrow (the `&mut Self` chaining builder on `GameObject` is exempt), and fails when a known violation is fixed but left on the list, so the list only shrinks. Four violations remain and are listed in the test: `GameScene::object`, `try_object` (return `GameObject<'_>`), `counter` (returns `Mut<'_, Counter>`) and `world` (the ECS escape hatch). Left open: replacing `GameObject<'_>` with a `Copy` ID handle rewrites every sample and the skill file, so it needs the owner's go-ahead. Full check green (lib 554 with gpu-tests). Slice 2026-10-12: the test now covers every type in `rusting_engine::prelude`, found by reading the prelude and scanning every `impl` block under `src/`, not only the API index types, and also fails when a prelude struct or enum gains a lifetime parameter (known: `GameScene`, which games name as `&mut GameScene` with the lifetime elided, and `GameObject`). The wider scan found two more pre-existing borrows, `PhysicsWorld::contacts` and `impacts` (read-only slices of a resource reached through `world`), now on the list. Evidence: `docs::tests::gameplay_methods_do_not_return_new_borrows` passes and its stale-entry check proves every listed borrow and lifetime is still detected. Full check green (lib 1117 with gpu-tests).
- [ ] One obvious way per task: overlapping helpers are merged or deprecated, and the API index marks the preferred call. Partly done: a `Preferred:` paragraph in a doc comment names the call to use instead, and `docs::tests::preferred_calls_exist_in_the_index` fails when a named call is not in the API index. Marked so far: `GameScene::counter` (prefer `counter_value`, `add_to_counter`, `set_counter`) and `GameScene::world`. Left open: nothing is merged or deprecated, because choosing the winner among `move_x`/`move_by`, `object`/`try_object` and similar pairs changes the public API and every sample, which is the owner's call. Full check green (lib 555 with gpu-tests).
- [x] Typed, fallible lookups everywhere a generated name can be wrong (`try_object` style), with a diagnostic that lists the nearest existing names. Partly done: `GameScene::object` and `watch_gpu_object` panics now end with ``; did you mean `Crate`?`` listing up to three scene object names within a case-insensitive edit distance of half the name length (at least two). Evidence: `project_runner::tests::a_missing_object_name_panic_lists_the_nearest_names`; full check green (lib 556 with gpu-tests). Left open: the other name-taking methods (`counter`, `set_color`, `spawn_copy`'s template and so on) keep their current behaviour, most return `Option` or `bool` already and have no message to add to; the hint is a panic message, not a diagnostic code. Slice 2026-10-07: the missing-counter warning from `counter_value` and `counter_complete` also ends with ``; did you mean `money`?`` listing close counter names, computed only for a name's first warning. `nearest_hint` is shared with the object panic. Evidence: `game_code_reads_and_sets_counters_by_name` checks the hint for a misspelt and an unrelated name; `a_missing_object_name_panic_lists_the_nearest_names` still passes. Slice 2026-10-07 (2): the setters that return nothing (`set_color`, `set_emissive`, `set_material`, `trigger`, `particles`, the animation and ragdoll setters) and `spawn_copy`'s template warn once per missing name with the same hint instead of doing nothing silently; setters that return `bool` are left as they are. Evidence: `a_setter_given_a_missing_name_warns_once`. Slice 2026-10-09: the diagnostic code. `rusting lint` warns `LINT_MISSING_OBJECT` on a literal object name passed to `object`, `despawn`, `set_visible`, `set_color`, `flash`, `set_hud` and the other object setters when no entity in any scene or prefab has it, with the same close-name hint; literals on a spawn line and `const` names a spawn uses are counted as existing. Evidence: `lint_flags_object_names_no_scene_has` (a misspelt `Playr` warns with `did you mean \`Player\``; a scene name, a spawned literal, a spawned constant and a run-time name pass). Through the real binary, a roguelike template copy given `scene.flash("Stairz")` warned with `did you mean \`Stairs\``. Every sample, RustingGames project and template lints clean; before the `const` rule, unclaimed warned on its spawned `Read Tag` card. Slice 2026-10-09 (2): `LINT_MISSING_ACTION` does the same for a literal action read by `pressed`, `held`, `press_tick` or `binding` that no `rusting.input_action`, `rebind` call or built-in `player.*` action defines; the test is now `lint_flags_object_and_action_names_nothing_defines` (a misspelt `jmup` warns with `did you mean \`jump\``; a scene action, `player.sprint` and a rebound action pass), and the roguelike copy given `pressed("restrat")` warned through the real binary. Every sample, RustingGames project and template still lints clean. Counters are not linted: a trial over every RustingGames project found 25 reads of counters that scenarios `set` or code writes through computed names (forever-bear `BigButton.force_pose`, unclaimed tuning counters, playplace `balls_*`), and none was a typo; the runtime warning with its hint covers them. Slice 2026-10-09 (3): `LINT_MISSING_FILE` warns on a literal scene path in `load_scene` (relative to the project) or clip in `play_sound`, `play_sound_looped` or `play_sound_with` (relative to `assets/`) with no file there, listing close names in the same folder; built-in `sfx:` clips need no file. Evidence: `lint_flags_object_and_action_names_nothing_defines` now also passes an existing scene and `sfx:coin 7` and flags `scenes/mian.rscene` (with `did you mean \`scenes/main.rscene\``) and a missing `hit.wav`. Through the real binary a roguelike copy (folder-form scene) flagged both; all 12 literal loads and clips in the samples and RustingGames projects exist, and every one still lints clean. Slice 2026-10-09 (4): `LINT_MISSING_CLASS` warns on a literal class in `in_class` that no scene or prefab entity has and no other string in game code names, so an `add_class` argument or a spawn helper's class parameter counts as defining it. Evidence: `lint_flags_names_nothing_defines` (renamed; `enemys` warns with `did you mean \`enemy\`` when `enemy` is only passed to a helper; a scene class and an `add_class` class pass). Through the real binary a roguelike template copy given `in_class("enemys")` warned on both lines; all 15 templates, every sample and every RustingGames project lint clean. Slice 2026-10-09 (5): `LINT_MISSING_FILE` also checks a literal `spawn_prefab` path (relative to `assets/`); `lint_flags_names_nothing_defines` flags a misspelt `prefabs/tlie.rscene`. No sample or RustingGames project passes a literal prefab path, and all still lint clean. Slice 2026-10-09 (6): `LINT_MISSING_OBJECT` covers every remaining `GameScene` call whose first argument is an object (32 more, such as `set_material`, `tile`, `touching` and `stop_animation`), and `LINT_MISSING_EVENT` warns on a literal `gpu_events` name that no other literal in game code (a `GpuPhysicsRule::new` argument) and no scene names. Evidence: `lint_flags_names_nothing_defines` also flags `set_material("Plyer")` and `gpu_events("landd")` (with `did you mean \`landed\``) while an event a rule names passes. Through the real binary a roguelike copy given `stop_animation("Stairz")` and a playplace copy given `gpu_events("gaet")` warned with the right hint; a first draft that treated `gpu_events` as an object call flagged five event names across playplace, metronome-mines and forty-thousand-and-one, and now every sample, template and RustingGames project lints clean of missing names. Not linted, by design: counters (see above) and audio buses, which are created the first time a call names them.
- [x] Engine-aware compiler hints: when `cargo` fails, `rusting check` (and `run`, `test`, `export`, which share the build step) adds one `RUST_ENGINE_HINT` diagnostic (severity `hint`, with file and line) per pattern found in rustc's output: scene borrow conflicts (E0499, E0502), unknown `GameScene` or `GameObject` methods (E0599, points at `rusting docs search`), a bad `rusting_engine` import (E0432, points at the prelude), and `f64` or `[f32; 3]` mismatches (E0308). The table is `cli::ENGINE_HINTS`; add a row per new pattern. Evidence: `cli::hint_tests::rustc_errors_get_the_engine_pattern_that_fixes_them` (line and file taken from the error, one hint per pattern, unrelated errors ignored); the code is in the registry, so `diagnostics::tests` covers explain. Full check green (lib 557 with gpu-tests). Limit: matching is on rustc's short message text, so a rustc wording change silently drops a hint; the hints are not yet tried on a real failing project in a test.
- [x] Scaffolding commands: `rusting add system <name>`, `rusting add component <name>`, and `rusting add scenario <name>` write a registered, documented stub plus a failing scenario that proves it runs.
  (Partly.) `add scenario` and `add system` are done: `add system` appends a documented `todo!` stub to `src/main.rs` and writes a failing scenario; the stub is not called until `update` calls it. `add component` is not done: it needs a way for game code to register a scene component, which is an owner decision. Evidence: `add_scaffolds_a_stub_and_a_failing_scenario` in `tests/cli.rs` (stub, duplicate refusal, bad name, scenario shape); full AGENTS.md check passes (gpu-tests needed a rerun after a known stall). The scenario being red on `rusting test` is not run here.
- [ ] (Blocked on owner: changes the public API and every sample.) Typed units at the API boundary (metres, seconds, radians, degrees) where a mix-up is a common generated bug, without making ordinary code verbose.
- [ ] (Blocked: no deprecation exists yet to migrate; `rusting fix` already applies scene key renames.) Deprecations carry machine-applicable migrations, and `rusting migrate` upgrades scenes and game code across engine versions, because models are trained on older APIs.
- [x] Measure edit-to-diagnostic time for game code and keep it within a stated budget with a shared build cache across projects. Done 2026-10-07: docs/concepts.md "Build times" states a 3 s budget from save to diagnostic once the engine is built, and documents `CARGO_TARGET_DIR` as the shared build cache (Cargo already supports it, and `rusting export` finds the binary through `cargo metadata`'s `target_directory`, so no engine code was needed). Evidence, measured with a script that runs `rusting check --json` on a scratch copy of forever-bear (3,673 lines of game code, Ryzen 5 7600X): cold first build 121.8 s; with the engine built, no edit 1.05 s, an added type error reported as `RUST_BUILD_ERROR` in 0.91 s, the fixed build green in 2.11 s; a second copy under another package name sharing the first one's target directory built cold in 3.1 s instead of 122 s. Unit test `build_time_budget_and_shared_target_are_documented` finds the section. Limit: timing is hardware-dependent, so the budget is not enforced by a test; the scaffold does not set a shared target directory by default.

### Exit gate

- In the agent benchmark (Milestone L9), the median task needs at most one corrective build.
- An engine upgrade across one minor version is applied to every sample by `rusting migrate` with no hand edits.

## Milestone L3: Observability for models

Goal: an agent can see what the game did, as text or as an image built for a vision model, without guessing.

Depends on: Milestone 7 capture and scenarios; Milestone 8 replay for tick-exact inspection.

### Structured state

- [x] `rusting inspect <project> --tick N` pauses a headless run at a tick and reports selected entities, contacts, recent events, the active camera, the HUD/UI tree, and the freshness of every GPU-owned value.
  (Partly.) `rusting inspect [root] --tick N [--entity NAME]...` builds the game, runs it headless to tick N (through a generated `build/inspect-tick.json` scenario of `log` steps) and reports each entity's full scene form after that tick; with no `--entity`, every named entity of the main scene. Evidence: ran it on a generated project (Cube at tick 5 returned its transform and mesh); `inspect_tick_reports_entity_state_after_that_tick` in `tests/cli.rs` is `#[ignore]` (real cargo build, 103 s) and passes with `--ignored`; full AGENTS.md check green. Left open: contacts, recent events, the active camera, the HUD tree and GPU-value freshness; it pauses by running to the tick, not by attaching to a live game (L4 debug protocol). Unnamed entities are not listed.
- [x] A per-tick event trace (collisions, triggers, counter changes, spawns, despawns, inputs, scene loads, sounds, errors) written as JSON lines, with filters by entity, class, and event kind.
  (Partly.) Every `ScenarioReport` (so `rusting test --json`) has a `trace`: `input`, `set` and `collision` entries with tick and detail (collisions by persistent ID), in tick order, capped at 1000. Evidence: `scenario::tests::the_trace_lists_inputs_sets_and_collisions_by_tick`; full AGENTS.md check green. Left open: triggers, counter changes, spawns, despawns, scene loads, sounds and errors; JSON-lines output; filters by entity, class and kind (use `--fields` or `jq` for now).
- [x] `rusting diff <a> <b>` compares two scenes or two ticks semantically: entities added, removed, and changed, by ID and name, with field-level values.
  (Partly.) Scene files only: `rusting diff <scene-a> <scene-b>` matches entities by ID and reports added, removed and changed entities (with JSON path, before and after for each field) and scene-level field changes. Comparing two ticks needs `rusting inspect --tick`, which is not built. Evidence: `diff_reports_added_removed_and_changed_entities_by_id` in `tests/cli.rs`; full AGENTS.md check green. Limit: a rename plus ID change shows as remove plus add.
- [x] A text view of 2D, tile, and grid scenes (a character map with a legend) so a model without vision can check a layout.
  (Partly.) `rusting scene map <scene>` prints every `rusting.tile_map` as character rows plus a legend, and draws other named entities that sit over a map's cells as letters with name, ID, column and row. Evidence: `scene_map_draws_tile_rows_and_places_entities_on_them` in `tests/cli.rs`; full AGENTS.md check green. Left open: sprite-only 2D scenes without a tile map, and scene state at a tick (needs `inspect --tick`). Entities placed over a map ignore rotation and size.

### Images built for models

- [x] Annotated captures: optional overlays with entity names, short IDs, bounding boxes, collider outlines, velocity arrows, and a legend, so a model can refer to what it sees by ID.
  (Partly.) A scenario with `"annotate": true` makes every `capture` step also write `<name>.annotated.png` (a box and a 4-hex-digit short ID per mesh entity) and `<name>.annotated.json` (the legend: short ID, entity name, full ID, pixel box). Evidence: ran it on a generated project under lavapipe and opened the PNG (the Cube's box and `4033` label sit on the cube; the legend lists the same ID); `annotate::tests::annotations_box_a_visible_mesh_around_the_frame_center` (projection, no GPU) and `draw_outlines_the_box_and_writes_the_short_id`; full AGENTS.md check green. Left open: collider outlines, velocity arrows, names drawn on the image (the font has only hex digits, names are in the legend), occlusion (hidden entities still get a box), and a `rusting capture --annotate` flag for scene captures.
- [x] Contact sheets: one image with frames from several ticks in a grid, labelled by tick, so one image shows motion.
  A scenario with `"contact_sheet": "shots/sheet.png"` writes one grid image of every `capture` frame, each tile 320 px wide with its tick in a corner label (the frames are still saved on their own too). Evidence: ran a 3-capture scenario on a generated project under lavapipe and opened the PNG (tiles labelled 1, 3, 6, empty slot black); `annotate::tests::a_contact_sheet_tiles_frames_in_a_grid_with_tick_labels`; full AGENTS.md check green. The columns and tile width are fixed (square-ish grid, 320 px); a sheet is written only when at least one capture ran.
- [x] Region picking: `--pick-rect` returns every entity ID that covers a rectangle and its pixel share.
  (Partly.) `rusting capture ... --pick-rect X,Y,W,H` (repeatable) adds `pick_rects` to the result: per rectangle, every entity ID and name that is the nearest hit, with its share, plus the `none` share that hits nothing (`CameraView::pick_rect`). Evidence: `capture_without_vulkan_still_reports_camera_and_picks_per_tick` (rectangle over the cube lists it with a share above 0; sky rectangle lists nothing); full AGENTS.md check green. Open limits: shares are estimates from at most 64x64 sampled pixel centers using bounds picking (not silhouettes); only the nearest hit counts, so occluded entities are not listed.
- [x] Scenario checks on screen presence: an entity is on screen, inside a screen rectangle, covers at least a pixel share, or is occluded. These replace most golden images in agent-written tests.
  (Partly.) New scenario step `expect_screen` with `on_screen`, `occluded`, `inside` (box within fractions of the frame) and `min_share` (share of the frame the entity is the nearest hit for); works with `until`/`within`, needs no Vulkan, and failures report the box, `box_share` and `frame_share`. Evidence: `scenario::tests::screen_checks_see_a_mesh_in_front_of_a_camera_and_behind_a_wall` (visible wall passes on_screen/inside/min_share; a mesh behind the wall is occluded and not on_screen; an off-frame mesh is not on screen; wrong expectations fail with steps 3 and 4); schema and docs tests; full AGENTS.md check green. Open limits: bounds picking sampled at 64x64 (a tiny or thin visible sliver can read as occluded); `inside` uses the box clipped to the frame; only mesh entities count; not run in a real game process.

### Budgets

- [x] Performance reports in stable JSON (CPU/GPU time per phase, draws, dispatches, upload and readback bytes, asset memory), with budget assertions in scenarios and environment metadata in every result.
  (Partly.) Every scenario report in `rusting test --json` now has `perf`: `tick_ms_mean`, `tick_ms_p95`, `tick_ms_max` (wall clock per whole tick), `render` (draws, triangles, visible instances of the last captured frame, null without a capture) and `environment` (engine version, OS, arch, debug or release, device and driver when a frame rendered). A scenario `budgets` object (`max_tick_ms`, `mean_tick_ms`, `p95_tick_ms`, `max_draws`, `max_triangles`) fails the run with a `budget:` step. Evidence: `scenario::tests::perf_is_reported_and_budgets_fail_the_run`; `a_capturing_run_reports_render_counters_and_draw_budgets` (`--features gpu-tests`, lavapipe: draws above 0, device named, `max_draws: 0` fails); schema test; full AGENTS.md check green. Open: CPU/GPU time per phase, dispatches, upload and readback bytes, asset memory, GPU timings; `environment` is only in scenario results, not every CLI result; timing budgets are machine-dependent so real-hardware numbers are unverified here.

### Exit gate

- Each benchmark task in Milestone L9 can be completed by a text-only model, using text views, traces and diffs instead of images.
- A vision model identifies the entity behind any visible object in an annotated capture by ID.

## Milestone L4: Live session and editor bridge

Goal: an agent works against a warm, running engine instead of paying cold-start costs, and a human sees and controls what it does.

Depends on: Milestone L1; Milestone 6 editor and snapshot undo.

- [x] `rusting serve`: a local daemon that keeps a project loaded, the build warm, and shaders compiled, and speaks JSON-RPC over stdio. Every CLI operation is available through it with the same result envelope.
  (Partly.) `rusting serve` reads one JSON-RPC 2.0 request per line on stdin (`method` is the command words, `params` the remaining arguments as strings) and writes one response per line whose `result` is the same envelope as `--json` (including `size`, `--limit`, `--fields`, `--summary`); `shutdown` or end of input stops it; standard error codes -32700, -32600, -32601, -32602, and -32603 (a panicking operation does not kill the daemon). Evidence: `serve_answers_json_rpc_lines_with_the_cli_envelope` in `tests/cli.rs` (matches the cold CLI result, unknown method, bad params, parse error, nothing answered after `shutdown`); schema/docs tests; full AGENTS.md check green. Open: the daemon does not yet keep a project loaded, the build warm or shaders compiled (each request does the same work as the CLI, minus the process start); the 10x speed gate was not measured; operations still use their own output paths, so a long-running one blocks the daemon.
- [x] (Partly) Debug protocol over the daemon: attach to a running game, pause, step N ticks, set values, query, capture, and resume.
  Evidence: `rusting debug [root]` cooks, builds and runs the game with `RUSTING_DEBUG_SESSION=1`; `src/debug_session.rs` reads JSON lines `{"id","cmd"}` (step, get, set, press, release, capture, tick, quit) and the game stays paused between commands, so `step` is resume. Tests: `the_game_is_paused_between_commands_and_steps_exactly`, ignored real-build `debug_session_steps_and_inspects_a_running_game` (passes with `--ignored`). Full check green: fmt, clippy x3, test plain and `--features gpu-tests`.
  Open: `capture` in a session is not verified against a real game; `rusting serve` does not relay to `rusting debug`.
- [x] (Partly) MCP adapter over the same daemon (stdio, project-root scoped, read-only and mutating tools marked), carried from `missingFeatures.md` section 9. It must give the same results as the CLI.
  Evidence: `rusting mcp [root]` speaks MCP over stdio (`initialize`, `ping`, `tools/list`, `tools/call`); one tool per operation (underscored names, `serve`/`debug`/`mcp` excluded) with `readOnlyHint` from `schema::READ_ONLY`; `tools/call` runs through `serve_request`, so the text result is the `--json` envelope and `isError` mirrors `ok`. Scoped by `set_current_dir(root)`; absolute paths and `..` arguments are refused (-32602). Test: `mcp_lists_tools_and_calls_them_like_the_cli`. Full check green: fmt, clippy x3, test plain and `--features gpu-tests`.
  Open: no CLI-versus-MCP identity test over many operations (gate at the L10 "identical results" item); root scoping is argument-checked, not a sandbox, so a spawned build can still read elsewhere.
- [x] (Partly) Live editor bridge: agent edits to an open scene go through the editor's snapshot undo, show a diff, and highlight the affected entities. An external write never silently replaces unsaved GUI work; conflicts are reported.
  Evidence: the editor watches the open scene file (`reload_external_scene_change`). A clean scene reloads behind one Undo snapshot; the status message now carries "N added, N changed, N removed" (`outside_change`) and the added and changed entities become the selection (`highlight_scene_ids`). A scene with unsaved edits keeps them, reports the conflict, and Save refuses once. Test: `outside_scene_patch_reloads_under_undo_or_conflicts_with_unsaved_edits` (asserts the counts and one highlighted entity). Full check green: fmt, clippy x3, test plain and `--features gpu-tests`.
  Open: file-watch only, no socket, so the editor learns of a write on its next poll; the diff is a count, not a per-field view (the Agent panel item adds that with accept and reject); highlight replaces the user's selection.
- [x] (Partly) Editor Agent panel, in the Blender area style: the journal of agent operations, pending diffs with accept and reject, and the last scenario results. Add its design to `docs/editor-overhaul.md`.
  Progress: design written (`docs/editor-overhaul.md`, "Agent panel design"). Journal slice built: `EditorPanel::Agent` area (`src/editor/agent_panel.rs`), an `AgentJournal` resource fed by `reload_external_scene_change` (path, summary, changed IDs), newest first, click selects the entry's entities, Clear button. Verified: unit test `outside_scene_patch_reloads_under_undo_or_conflicts_with_unsaved_edits` asserts one journal entry holding the patched ID; `every_editor_panel_draws_even_when_optional_resources_are_missing` draws the new panel. 2026-10-01, lavapipe: fmt clean; clippy plain, `--no-default-features`, `--features gpu-tests` clean; `cargo test --workspace` exit 0, 16 result lines, 0 failed; same with `--features gpu-tests`. Results slice 2026-10-07: `rusting test <project>` (folder run) saves its envelope data to `build/test-results.json` (`project::TEST_RESULTS_FILE`); the Agent area has a Journal / Results segmented strip, and Results lists each scenario with a pass or fail pill, the file name and a failure's message, rereading the file when its modification time changes. Evidence: unit test `editor::agent_panel::tests::test_results_are_read_from_the_file_rusting_test_writes` (missing file is `None`, two rows parsed, the panel draws and caches them); `rusting test samples/forever_bear_booth --json` wrote the file with `night.json` passing. Pending slice 2026-10-07: the Journal tab has a `Pause agent edits` toggle; a write that arrives while the scene has unsaved edits or edits are paused becomes one pending row (path, the added/changed/removed counts between the editor scene and the file, touched IDs) with Accept (reloads behind one Undo snapshot), Reject (keeps the scene; Save still refuses once) and Select. Evidence: `outside_scene_patch_reloads_under_undo_or_conflicts_with_unsaved_edits` now also pauses, checks the pending row's summary and IDs, rejects (scene and file unchanged), then accepts a later write and checks the Undo snapshot restores the old name. Per-entity slice 2026-10-07: the pending row lists each touched entity with its own Accept and up to 8 `path: old -> new` field rows (`cli::leaf_changes`, the `rusting diff` leaf diff); an entity Accept merges that entity from the reviewed file into the editor scene behind one Undo snapshot, marks the scene unsaved and keeps the rest pending. Evidence: `a_pending_outside_write_is_accepted_one_entity_at_a_time` (rename plus a new entity; accepting the new one leaves the rename pending, the Undo snapshot lacks it, accepting the second clears the row). Removed-entity slice 2026-10-07: the pending row also lists entities the write removes, each with its own Accept, which deletes the entity and its descendants from the editor scene behind one Undo snapshot; a failed entity Accept drops its Undo snapshot. Evidence: the same test deletes a saved entity in the patch, checks `pending_removed`, accepts the removal and checks the entity is gone. Baseline fix 2026-10-07: the pending diff is taken against the file as the editor last loaded or saved it (`SceneFileRevision::base`), minus entities the editor scene already matches, so unsaved GUI edits no longer show as agent changes (the same test adds an unsaved entity and fails with `1 removed` without the fix). Perf slice 2026-10-07: each scenario in `build/test-results.json` carries `perf` (tick ms mean, p95, max, and draws and triangles when it rendered) and the Results tab shows it under the row; budget failures show as the failure message. Evidence: the results unit test checks the perf line and that an older file without `perf` shows none; `rusting test samples/forever_bear_booth --json` wrote `night.json` with 0.60 ms mean, 35 draws, 791584 triangles. Checked as Partly 2026-10-07: journal, pending diff with Accept and Reject (whole file and per entity), and the last scenario results with perf are in. Left: per-field Accept (in `docs/editor-overhaul.md` backlog); the journal is session-only (not saved); the panel is not checked visually.
- [x] (Partly) Every result reports whether a change touched disk, the editor buffer, or the running game.
  Evidence: the `--json` envelope (CLI, `serve`, `mcp`) carries `touched {disk, editor, game}` from `schema::touched`; `disk` is true only for `WRITES_PROJECT` commands that succeed and are not `--dry-run`. `rusting debug` replies carry it with `game` true after step, set, press, release, tick. Tests: `touched_reports_project_writes_but_not_reads_dry_runs_or_failures`, `the_game_is_paused_between_commands_and_steps_exactly`.
  Open: `editor` is always false from the CLI (the open editor reloads on its next poll); no per-command list of which files changed.
  Verification note: the checks recorded for the debug-protocol, MCP and editor-bridge items above counted partial output of runs that a `timeout` cut off, so they were not a full pass. Re-run on 2026-10-01 after fixing the `mcp` missing-root exit code (1, not 2): fmt, clippy default, `cargo test --workspace` and with `--features gpu-tests` all exit 0 with 16 `test result` lines and 0 failures. The real GPU driver was wedged, so both runs used lavapipe (`VK_DRIVER_FILES=/usr/share/vulkan/icd.d/lvp_icd.json`). Clippy `--no-default-features` and `--features gpu-tests` re-run clean after the change.

### Exit gate

- A patch, test, and capture round trip through the daemon is at least ten times faster than the cold CLI on the same machine (hardware gate).
- An agent and a person edit the same open scene; every conflict is reported and every agent change is undoable from the editor.

## Milestone L5: Behaviour verification toolkit

Goal: an agent can prove a game works, and prove that its proof is strong enough.

Depends on: Milestone 7 scenarios; Milestone 8 replay and state hashes.

- [x] (Partly) Declared invariants: a `rusting.invariant` component or scenario section (the player is never below the floor, no NaN positions, a counter never exceeds its target) checked every tick in `run`, `test`, and fuzz runs.
  Evidence: top-level `invariants` list in a scenario; each entry has the `expect` fields plus `finite` (no NaN or infinite number; JSON reads those back as `null`). Checked after every tick, reported as `invariant N: ...` at the first failing tick, once each with `keep_going`. A missing entity or path passes unless `exists` is set. `rusting schema scenario` and `rusting explain` list it. Test `invariants_are_checked_every_tick_and_report_the_first_bad_tick` covers holding, broken, despawned-entity, empty-invariant and null-number cases. 2026-10-01, lavapipe: fmt clean; clippy plain, `--no-default-features`, `--features gpu-tests` clean; `cargo test --workspace` exit 0, 16 result lines, 0 failed; same with `--features gpu-tests`. Open: checked in `rusting test` scenarios only, not in plain `rusting run`; no `rusting.invariant` component form; the scene document is rebuilt per invariant per tick (slow with many); `rusting fuzz` checks a scenario's invariants on every seed.
- [x] Seeded input fuzzing: `rusting fuzz` presses random named actions for N ticks over many seeds and reports the first seed and tick that breaks an invariant, as a ready scenario file.
  Done 2026-10-07: a scenario `fuzz` section (`seed`, `actions`, `rate`) expands into press and release steps from `RandomSeed(seed).unit(tick, action)`, listed in the report as `fuzz_steps`; `rusting fuzz [root] scenario [--seeds N] [--first-seed N] [--action NAME]...` runs one seed per process and writes the first failure (step, invariant or crash) to `build/fuzz/seed-N.json` with the presses as plain steps up to the failing tick. Evidence: unit test `fuzz_presses_seeded_actions_and_its_steps_replay_the_failure` (same seed gives the same presses, the presses alone replay the failing tick, another seed differs). On `samples/forever_bear_booth`, `night.json` passed 3 seeds with 809 presses in the last run; a scratch scenario with the invariant "Player x > -0.5" and `--action player.left` failed on seed 0 at tick 12, and `rusting test` on the written `seed-0.json` (one press at tick 5, 12 ticks) failed at the same tick with the same value. Limits: relative capture paths in the scenario resolve from `build/fuzz/`; no shrinking beyond cutting at the failing tick; sticks and pointer are not fuzzed.
- [x] (Partly) Explorer bot: a goal-seeking player that tries to reach pickups, sensors, and win conditions and reports unreachable goals and stuck states.
  Done 2026-10-07 for `PlayerController` games: a scenario `explore` section (`goals`, `ticks_per_goal` default 600) walks the player toward each goal with the `player.*` actions (the goal delta turned into the body's yaw frame), jumps when it has not got 5 cm closer in 30 ticks (logged in `explore.stuck`), counts a goal reached within 0.75 m horizontally or when it despawns, and fails with "explore: could not reach ..." for goals past `ticks_per_goal`. Without `goals` it visits every sensor collider, nearest first. Evidence: unit test `explorer_walks_to_reachable_goals_and_reports_caged_ones` (two open sensors reached, nearer first; a walled-in sensor unreachable with stuck entries; an unknown named goal fails at tick 0). Real run on a copy of `samples/night_vault` with `{"explore": {"ticks_per_goal": 450}}` and 1800 ticks: reached Key 1 (tick 164), Key 3 (460), Vault Door (615) and Key 2 (885), no stuck entries. Left: 2D and custom player controllers; no path finding (greedy straight-line steering); win conditions other than reaching an entity.
- [x] (Partly) Scenario strength check: `rusting test --without <system|component>` runs a scenario with a named system or component switched off and expects it to fail. This turns the manual "fails with the code removed" evidence into a command.
  Components done 2026-10-07: `--without NAME` (repeatable) sets `RUSTING_WITHOUT` on the game, and `load_scene_document` passes every scene through `without_components` (a built-in section such as `collider`, or a registered component); the result inverts, passing on `SCENARIO_FAILED` or `GAME_FAILED` and failing with `SCENARIO_TOO_WEAK` when the scenario still passes. Evidence: unit test `without_components_leaves_out_sections_and_registered_components`; on `samples/forever_bear_booth`, `--without transform` passed (the game panicked on a missing Transform) and `--without rusting.player_controller` and `--without rusting.sky_light` failed with `SCENARIO_TOO_WEAK` (night.json checks neither). Left: systems, because games run one `update` function with no named systems to switch off; it needs named game systems first.
- [x] (Partly) Coverage: which systems, components, entities, and events a scenario exercised, so an agent sees untested mechanics.
  Done 2026-10-07 for components, entities and events: `ScenarioReport.coverage` compares the scene before tick 0 with the end scene and lists `entities_changed` (with the changed sections), `entities_added`, `entities_removed`, `entities_unchanged`, `sections_changed`, `sections_untouched`, `inputs` and `events` (trace entries by kind). Evidence: unit test `coverage_lists_what_a_run_changed_and_left_alone` (falling cube changed, a static sign unchanged, `jump` in inputs, `transform` not untouched). On `samples/forever_bear_booth` `night.json`: Big Button and Player changed (`transform`), 54 unchanged, 390 added (spawned bears and plates), 20 sections untouched including `collider` and `rusting.sky_light`. Left: systems (games have one `update` function and no named systems); values that change and return count as unchanged; no per-tick sampling.
- [x] (Partly) Record a human play session in the editor or the game window as a scenario file, so a human can hand an agent a bug as a test.
  Done 2026-10-07 for the game window: `rusting run --record tests/bug.scenario.json` writes a scenario instead of a replay when the window closes; `scenario_from_replay` turns the recorded input into `press`, `release` and `tap` steps for the game's named actions (through its `ActionMap`), with ticks counted from the recording start, the recorded seed, and `ticks` set to the last frame. Evidence: unit test `a_recorded_session_becomes_press_release_and_tap_steps` (two keys bound to one action give one press and one release, a press and release between frames gives a tap, the output parses as a `Scenario`). Left: the editor's play mode; sticks and the cursor are not converted; no human-driven window run was possible here.
- [x] Divergence bisect: given two builds or two scene revisions, report the first tick and entity where state hashes differ.
  Done 2026-10-07: `rusting bisect <root> <other-root> [--ticks N]` cooks and builds both copies (for example two git worktrees), runs each headless with per-tick state hashes, and at the first differing tick reruns both to that tick and names the first differing entity; `first_divergent_entity` now pairs entities by scene ID, so a revision that adds an entity does not blame every later one. Evidence: unit test `divergent_entities_pair_by_scene_id_across_scene_revisions` (an added lamp shifts entity indices; only the lamp is reported, a changed crate is found, equal lists pass). Real runs on `samples/forever_bear_booth` against a copy: identical copies passed "identical on all 120 ticks"; with the copy's `Desk` moved 0.25 m it failed with DETERMINISM_DIVERGED "diverge at tick 1, first in `Desk`". Limits: debug builds only; it compares plain headless runs, not scenario input; it does not bisect git history itself (run it per revision).

### Exit gate

- Every sample game has invariants, a fuzz run with no failures over 100 seeds, and scenarios that pass the strength check.
- A seeded regression inserted into a sample is found by `rusting fuzz` or the explorer bot without a hand-written scenario.

## Milestone L6: Quality floor and game feel

Goal: a game made by an agent looks, sounds, and feels finished by default, and the engine reports what a model cannot perceive.

Depends on: Milestones 15 and 16 for audio and UI breadth; the existing game-feel kit and art-direction presets.

- [x] `rusting lint`: presentation and design checks with diagnostic codes, including HUD text contrast and off-screen text, camera inside geometry, zero-intensity or clipped lighting, collider and mesh mismatch, scale sanity (a 40 m player), pickups or goals with no reachable path, events with no audio or visual feedback, and games with no win or lose state.
  - [x] Command and static scene checks. Done 2026-10-07: `rusting lint [root]` reports `LINT_PLAYER_SCALE` (a `PlayerController` body outside 0.5 to 3 m tall, from its collider or the default capsule times the per-axis scale of it and its parents), `LINT_ZERO_SCALE` (a zero scale axis) and `LINT_LIGHT_OFF` (negative intensity, zero range or black color; intensity 0 is allowed because games switch such lights on in code), and fails when any check warns. Evidence: unit test `lint_flags_giant_players_zero_scales_and_dead_lights` (a 36 m player, a zero-height entity, a zero-range lamp and a black sun warn; a 0.9 m player under a 0.5 parent scale and an intensity-0 torch pass). Every sample with a project.json and every RustingGames project with one lints clean; the first version flagged intensity-0 lights in five games (flashlights, scare flashes), which led to allowing intensity 0.
  - [x] (Partly) Camera inside geometry: `LINT_CAMERA_INSIDE` warns when a camera starts inside a solid Box or Sphere collider, with parent transforms and rotation applied; a camera inside its own player's collider is exempt. Verified by `lint_flags_giant_players_zero_scales_and_dead_lights` (a camera 1.5 m along a wall turned 90 degrees warns; one beside it and one on the player pass), and every sample and RustingGames project lints clean.
  - [x] Light budget: `LINT_LIGHT_BUDGET` names each visible light past the
    scene quality's budget (Eco 16, Balanced 32, High and Auto 64), in the
    renderer's upload order (directional, then point, then spot lights, each
    in scene order), skipping lights hidden on themselves or a parent. From
    forever-bear F38. Verified by
    `lint_names_visible_lights_past_the_quality_budget` (16 point lights at
    Eco push out the spot light that comes first in the file; a hidden light
    and a light under a hidden parent do not count). Run over every
    RustingGames project and sample, it flags only forever-bear, whose four
    `Day Light` spot lights are past its Balanced budget.
  - [x] Collider and mesh mismatch: `LINT_COLLIDER_MISMATCH` warns when a
    solid box, sphere or capsule collider is more than twice or less than
    half the size of its entity's built-in cube, rounded cube, sphere,
    cylinder or capsule mesh on any axis. Sensors and player controllers are
    exempt; imported meshes are skipped (no bounds in the scene file).
    Verified by `lint_flags_giant_players_zero_scales_and_dead_lights` (a
    1 m cube with a 3 m wide box warns; a ball with a 0.45 radius and an
    oversized sensor zone pass). Every sample and RustingGames project
    lints with no new warnings.
  - [x] Capsule colliders for the camera check. `LINT_CAMERA_INSIDE` now catches a camera inside a capsule (the core segment along local Y plus the radius), and skips every player or platformer controller body, not only the camera's own parent, since an eye camera sits inside it either way. There is no cylinder collider shape; mesh colliders stay skipped because the scene file has no mesh bounds.
    - Verified: `lint_flags_giant_players_zero_scales_and_dead_lights` flags a camera in the top cap of a 3 m capsule and passes one 0.1 m past its side; an unparented camera inside a player's box passes, and the test fails when the player exemption is removed. Every sample and RustingGames project lints with no camera warning; before the exemption the capsule check flagged forever-bear's unparented `Focus Camera` at the player's eye.
  - [x] Pickups or goals buried in geometry: `LINT_GOAL_INSIDE` warns when a sensor collider's centre starts inside another entity's solid box, sphere or capsule (player bodies and the sensor's ancestors aside), so walking up to it can never reach it. Shares the inside test with `LINT_CAMERA_INSIDE`.
    - Verified: `lint_flags_giant_players_zero_scales_and_dead_lights` flags a coin sunk into a capsule pillar and passes one beside it. Every sample, RustingGames project and `rusting new` template lints with no new warning.
  - [x] Pickups or goals with no reachable path through open space. `LINT_GOAL_UNREACHABLE` rasterizes fixed solid box, sphere and capsule colliders (no body or a `Fixed` one) into an XZ grid (0.25 m cells, coarser past 256 m) wherever they cover the part of the first player's body above what it can step or jump over (`max(max_step_height, jump apex)` from its controller), flood-fills from the player, and warns on each sensor at the player's floor height whose open cell is not reached. Walls whose names appear as string literals in game code count as doors and are walked through.
    - Verified: `lint_flags_goals_walled_off_from_the_player` (a coin in a closed 3 m room warns; a coin behind a wall named in code, one inside 1 m walls the default player jumps, one 6 m above the floor and an open one pass; with no names from code the door room warns too). Every sample, RustingGames project and `rusting new` template lints clean; before the door rule, split-signal's `Vault` behind its code-opened `Door A` and `Door B` warned. Each project lints in under 0.3 s (forever-bear, 3,895 entities, 0.2 s). Limits: one floor and a point-sized body, so a gap narrower than the player passes and a level reached by stairs is not judged; a door named only through a parent or a computed name is a wall.
  - [x] Static HUD text checks: `LINT_TEXT_SMALL` warns on a `rusting.hud` `font_size` below 14 px, and `LINT_TEXT_OFFSCREEN` when the text's anchored corner (anchor plus offset, which points toward the centre) lands outside a 1280 x 720 view. Elements tied to a camera's viewport are left out.
    - Verified: `lint_flags_giant_players_zero_scales_and_dead_lights` flags 10 px text and text 1400 px right of the top-left corner, and passes a bottom-anchored footer and an off-view offset tied to a camera. Every sample, RustingGames project and `rusting new` template lints with no text warning.
  - [x] HUD text that runs off the edge: `LINT_TEXT_OVERFLOW` lays each `rusting.hud` text out in the HUD's own egui font at its `font_size`, with `{counter}` placeholders at the counters' starting values, places the box the way the HUD anchors it, and warns when it runs more than half a pixel past a 1280 x 720 view. Needs the `ui` feature (on by default); without it the check is skipped.
    - Verified: `lint_measures_hud_text_that_runs_past_the_view` (a long line 700 px in from the left and 40 centred lines warn; the same line anchored top right, which grows left, and a counter readout pass). A 32 px banner added to a `rusting new` 3d project at offset 400 warned "992 x 37 px that runs 112 px past the edge" through the real binary. Every sample, RustingGames project and `rusting new` template lints clean. Limits: text set from game code at run time and counters past their start value are not measured, and button padding is left out.
  - [x] Games with no win or lose state: `LINT_NO_ENDING` warns on the first counter when a scene keeps counters but none has a target and the game code never calls `counter_complete`, `load_scene` or `quit`. A scene with no counters (a toy, a sandbox) is not judged.
    - Verified: `lint_warns_when_counters_exist_but_nothing_ends_a_round` (an endless `score` warns on `Score`; a target, or code calling `load_scene`, passes; an empty scene passes). Through the real binary, a 3d template project given a target-less `score` counter by `scene patch` failed `rusting lint` with LINT_NO_ENDING; all 15 `rusting new` templates, every sample and every RustingGames project lint clean. Limit: the code check is a text search, so a call in a comment counts as an ending.
  - [x] HUD text contrast (needs a rendered frame): `CAPTURE_TEXT_CONTRAST`, see the accessibility item below.
  - [x] Events with no audio or visual feedback: `LINT_SILENT_GOAL` warns on each counter with a target that nothing reacts to (no HUD element `requires` it or reads it out as `{name}`, no pickup `requires` it, and game code never names it in a string), so reaching it shows and sounds nothing. Other scene events already give feedback by themselves: a collected pickup vanishes, and a `sound_cue` or `burst_emitter` is the feedback.
    - Verified: `lint_flags_goals_nothing_reacts_to` (only the counter nothing reads warns; a pickup or HUD that requires it, a `{coins}` readout, code naming it, and a counter with no target pass). Through the real binary every sample, fresh template and RustingGames project lints clean except Lantern Keeper, whose `anim_init` counter (target 1) nothing reads: a leftover flag, a true hit. Limit: the code check is a text search for the quoted name.
- [x] Controller feel metrics in physical units (jump apex height and time, time to top speed, stopping distance, air control), reported by `rusting inspect` and compared with documented genre ranges. Done 2026-10-07: `rusting inspect --tick N` adds `feel` to each entity with a player or platformer controller, with genre ranges (first or third person: 3 to 8 m/s, 0.4 to 1.6 m apex, 0.4 to 1.2 s air time; 2D platformer: 4 to 12 m/s, 1 to 5 m, 0.5 to 1.2 s) and `notes` for numbers outside them. Both controllers set their speed directly, so time to top speed is one fixed step, stopping distance 0 and air control full. Evidence: unit test `controller_feel_reports_jump_and_speed_in_physical_units` (default player 1.27 m apex and 1.02 s air time with no notes; a 20 m moon jump at 2 m/s gets three notes), and a real `rusting inspect` of a copy of `hammer_run` reported 1.27 m apex, 4.08 m jump distance and 7.2 m/s sprint for its player.
- [x] More data-driven juice: screen shake, hit-stop, squash and stretch, camera trauma, flashes, and particle and sound presets, each a registered component with schema entries.
  - [x] Screen shake and camera trauma. Done 2026-10-07: `rusting.camera_shake` (`CameraShake`: trauma, decay, max_offset, max_roll, frequency) is registered with schema, reflect and snapshot entries; game code calls `scene.add_trauma(name, amount)`, strength is trauma squared, trauma decays on the fixed step, and render extraction moves and rolls only the drawn view, so the camera's `Transform` never drifts. Evidence: unit test `camera_shake_moves_the_drawn_view_and_fades_back_to_rest` (trauma moves the extracted view by more than 1 cm and less than 60 cm, the `Transform` is unchanged, and after 40 ticks the view is exactly at rest and trauma is 0).
  - [x] Squash and stretch: `rusting.squash` and `scene.squash(name, amount)`
    wobble an object's `Transform` scale on a damped spring and keep its
    volume. Verified by `squash_flattens_keeps_volume_and_springs_back_to_rest`
    (volume stays 8 within 1e-3, it overshoots into a stretch, and it ends at
    the exact rest scale).
  - [x] Flashes: `rusting.flash` (`color`, `duration`) and
    `scene.flash(name)` tint an object and its children toward a color,
    fading back over the duration. Extraction lists the flashing renderables
    (nearest flashing ancestor wins) and the renderer mixes the tint into
    each instance's color and emissive, so the shared material is untouched.
    Verified by `a_flash_tints_the_object_and_its_children_then_fades`
    (parent and child tinted, a sibling not, strength 5/6 after one tick of
    a 0.1 s flash, gone after 0.1 s) and the GPU test
    `a_flash_tints_the_drawn_object_and_fades_back` (the centre pixel turns
    green, then returns to the exact material color).
  - [x] Hit-stop: `scene.hit_stop(seconds)` (0 to 1 s, a longer running
    stop wins) sets the `HitStop` resource, and the windowed loop holds back
    that much real time before `App::update`. Fixed ticks pause and then
    carry on exactly as before, so simulation results, replays and headless
    runs (which step ticks directly) are unchanged. It is a call rather than
    a component because it freezes the whole game, not one object. Verified
    by `hit_stop_holds_back_real_time_then_lets_it_through` (a 50 ms stop
    eats three 16 ms frames and 2 ms of the fourth, a shorter stop does not
    cut it short, and 9 s clamps to 1 s). How long it feels on screen is a
    real-hardware check.
  - [x] Particle and sound presets on registered components. Particle
    presets already exist: `rusting effect apply` writes one of seven
    `rusting.particle_emitter` presets (dust_motes to fire). Sound presets
    are clip paths: `sfx:<preset> [seed]` in `rusting.sound_cue` or
    `play_sound` plays one of the seven `sfx` generator presets, synthesized
    in memory, with no file, and validation skips them. Verified by
    `sfx_clip_paths_name_a_preset_and_an_optional_seed` (same samples as
    the generator, seed default 1, bad presets, seeds and extra words
    rejected), `builtin_sfx_clips_play_with_no_file` (the offline mixer
    plays `sfx:coin 7` from an empty assets folder, peak above 0.05), and
    the CLI test `friction_fixes_for_docs_new_and_scene_query` (validate
    reports the misplaced `../assets/bell.wav` but not `sfx:coin 7`).
- [x] A built-in placeholder asset pack: CC0 meshes, sprites, fonts, and sounds in one consistent style, so a first build does not look like grey cubes. Its provenance goes through the existing `.rmeta` records.
  - [x] Textures. `rusting asset generate <root> texture "bricks 3"` draws one of six tileable 256 px presets (grid, checker, bricks, planks, tiles, noise) in one shared eight-colour palette, the seed picking the colour, and imports the PNG as CC0 with preset and seed in the `.rmeta` notes, like the `sfx` generator; a project hook named `texture` still wins. Evidence: `texgen::tests::presets_are_seeded_tileable_and_not_flat` (each preset repeats for a seed, changes for another, is not flat, and has no seam at the wrap edge; it fails with the noise lattice unwrapped) and `asset_import::tests::the_builtin_texture_generator_imports_a_seeded_cc0_png`. Through the real binary all six presets imported, and a capture on the GPU showed `bricks 6` on a wall and `planks 5` on a floor through `base_color_texture` with `uv_scale`.
  - [x] Sprites. `rusting asset generate <root> sprite "star 2"` draws one of six 128 px shapes (circle, square, triangle, diamond, star, heart) from signed distance functions, with the texture palette as fill, a lighter top, a dark outline and antialiased edges on a clear background, imported as CC0 like the textures. Evidence: `texgen::tests::sprites_are_shapes_on_a_clear_background` (each shape repeats per seed, changes with it, has a clear border and a solid centre, and covers 20 to 80 % of the image; the border check fails with the heart scaled up, which is how a clipped heart was found) and the sprite half of `the_builtin_texture_generator_imports_a_seeded_cc0_png`. All six were generated through the real binary and checked by eye on a light and a dark background.
  - [x] Meshes. `rusting asset generate <root> mesh "barrel 4"` builds one of five low-poly props (crate, barrel, rock, tree, gem) as closed surfaces of revolution, written as a binary glTF with no normals so the importer shades it flat, coloured from the texture palette (the seed picks the colour and moves the rock's lumps), standing on its origin, imported as CC0 like the textures. Evidence: `meshgen::tests::presets_are_closed_outward_seeded_meshes` (every edge used once each way, positive volume, same bytes per seed, different for another), `generated_meshes_load_as_gltf`, and the mesh half of `the_builtin_texture_generator_imports_a_seeded_cc0_png`. Through the real binary all five were generated, placed with `scene add-model` and captured on the GPU, lit and flat-shaded with no inside-out faces.
  - [x] Fonts in the same style. `rusting asset generate <root> font pixel` (or `bold`) writes a TrueType pixel font of printable ASCII on a 5 x 7 grid, one square-cornered contour per pixel run, with no new dependency, and imports it as CC0 with kind `font` in the `.rmeta`; `asset import` now takes `.ttf` and `.otf`, checked by magic and, with `ui`, by egui laying out text in them. `rusting.hud` has a `font` field (a path under `assets/`) that egui binds from the next pass, with the built-in fonts as fallback; a missing or unreadable file warns once and keeps the built-in font. `rusting capture` now gives the scene its project folder, so HUD fonts resolve there. Sounds are covered by the `sfx` generator item below. Evidence: `fontgen::tests::fonts_are_stable_well_formed_truetype` (same bytes per preset, whole-file checksum `0xB1B0AFBA`, tables in bounds), `outlines_follow_the_bitmaps`, `egui_draws_text_in_the_generated_font` (monospaced with a real advance; it failed on an `hhea` two bytes too long, which read zero horizontal metrics and drew every glyph on one spot), and `runtime::game_feel::tests::hud_fonts_bind_on_the_pass_after_they_load` (unbound on the loading pass, bound on the next, bad and missing files never bind, fallback covers `é`). Through the real binary both presets generated and imported (`pixel 2` fails with `GENERATOR_FAILED`), a glyph sheet of all 95 characters was checked by eye in both weights, and a GPU capture of the `2d` template at tick 5 showed its help text in the pixel font.
- [x] A deterministic procedural sound-effect generator (sfxr style: jump, coin, hit, explosion, from parameters and a seed) with no new dependency. `rusting asset generate <root> sfx "coin 7"` synthesizes one of seven presets (jump, coin, hit, explosion, laser, powerup, blip) with a seeded splitmix64 and imports the WAV as CC0 with its preset and seed in the `.rmeta` notes; a project hook named `sfx` still wins. Verified by `presets_are_seeded_short_and_quiet_enough` (each preset repeats for one seed, changes for another, lasts 0.05 to 1 s, peaks at 0.4 and fades to silence) and `the_builtin_sfx_generator_imports_a_seeded_cc0_wav` (same bytes on a second import, bad presets and seeds fail with `GENERATOR_FAILED`). Run on a sample copy, `coin 7` imported a 207 ms 48 kHz stereo WAV.
- [x] Accessibility checks from `missingFeatures.md` P2: contrast, text size, focus order, colour-only status, and missing captions, with locations and captures.
  - [x] Text size: `LINT_TEXT_SMALL` (see the lint item above).
  - [x] Button text contrast. `LINT_TEXT_CONTRAST` warns when `rusting.hud` button text, blended by its alpha, has a WCAG 2 contrast ratio below 4.5:1 (3:1 from 24 px) against egui's dark button fill. The runtime HUD now always uses the dark theme; it used to follow the desktop theme, so a light desktop put white text on a light fill. Needs the `ui` feature.
    - Verified: `lint_measures_hud_text_that_runs_past_the_view` flags grey, 20 % alpha white and mid-grey 18 px button text and passes white, mid-grey 28 px and dark text with no button; `the_hud_stays_dark_on_a_light_desktop` fails without the theme pin. A card-game template copy with `Card 1` text at 0.35 grey failed `rusting lint` at 1.6:1 through the real binary. All 15 templates, every sample and every RustingGames project lint with no contrast warning.
  - [x] Focus order. Checking it turned up two runtime bugs, fixed instead of linted: egui gives a fixed `Area` `Sense::click()`, so Tab and the first gamepad d-pad press focused the HUD's area (or the captions box) instead of a button, and Enter or South clicked nothing; and HUD elements drew, and so took the focus, in scene-id (UUID) order. HUD and caption areas now sense hover only, their labels are not selectable, and HUD elements draw in reading order of their anchored point (top to bottom, then left to right). No lint is needed: the order always matches the layout.
    - Verified: `hud_buttons_take_focus_in_reading_order` (three centred buttons whose scene ids run opposite to the layout; Tab then Enter presses the top one) fails with either the area sense or the sort removed.
  - [x] Missing captions: a `rusting.sound_cue` takes a `caption` shown for 2 s each time it fires, and captions now stay up for their whole time when the clip is shorter (before, the caption vanished with the clip). No lint warns on an uncaptioned cue: most cues are footsteps and clicks a caption would only clutter. Scenario test `a_sound_cue_caption_shows_for_two_seconds` (fails without the keep-captions fix: the line is gone at tick 60).
  - [x] Plain HUD text contrast against the scene behind it. `rusting capture` (with the HUD) collects each text egui painted, skips text with a filled rect under its centre, renders the same tick again without the UI, and warns `CAPTURE_TEXT_CONTRAST` when the median WCAG contrast of the text colour against the scene pixels inside its box is below 4.5:1 (3:1 from 24 px). Building it turned up a bug: a tick-0 capture, the default, showed no HUD at all, because egui hides a new `Area` for its first frame. Plain HUD text is now painted straight onto a layer (buttons keep their area) and lets the pointer through, like a Godot label. The starter and racing templates failed the check (white or gold text on a pale sky or grass, 1.3 to 2.4:1) and now use dark text.
    - Verified: unit test `captured_text_is_judged_against_the_scene_behind_it`; GPU test `hud_text_is_on_the_first_captured_frame` (one unbacked text after one frame; failed before the layer change, which drew nothing). Through the real binary on an RTX 3060: a racing template capture showed no HUD before and the full HUD after; the warning fired on starter and racing, and all 15 fresh templates capture with no warning after the colour change.
  - [x] Colour-only status. `LINT_COLOR_ONLY_STATUS` warns on the first `set_hud("Name", ...)` call that sets the element's `color` when no call sets its `text` and its scene text has no `{counter}` readout. A change of brightness or alpha alone is not flagged: when every colour set is a literal array with the scene's RGB, the cue is visible without telling hues apart. Mesh colour changes (`set_color`) are not judged; most are hit flashes, not status.
    - Verified: `lint_flags_hud_status_shown_by_colour_alone` (a colour-only change warns at its line; a size change, a colour set with the text in another call, a counter readout and an alpha-only change on the scene's white pass). Through the real binary, a starter template copy given `set_hud("Help", |h| h.color = if hot {..} else {..})` warned at line 10, while the same change on the `Coins {coins}` readout passed. Every sample, RustingGames project and fresh template lints clean; Forever Bear's alpha-only `Aim Dot` warned before the brightness exemption. Limits: a text search, literal names only.

### Exit gate

- Every sample game passes `rusting lint` with no warnings.
- In a blind review of benchmark outputs, games made with the defaults are rated as more finished than the same requests built without them.

## Milestone L7: Recipes, templates, and knowledge

Goal: common game shapes start from tested, editable recipes rather than from an empty scene.

Depends on: Milestones L1 and L2.

- [x] `rusting recipe list` and `rusting recipe apply <name>`: named mechanics (double jump, dash, health and damage, checkpoints, inventory, wave spawner, turret, day timer, pause menu) that write ordinary components, systems, and scenarios into a project.
  - [x] The command and the first recipe, checkpoints. `rusting recipe apply <root> <recipe> [--dry-run]` writes the recipe source as `src/<recipe>.rs`, creates its objects in the main scene as one patch placed from the object named `Player`, and writes a passing scenario as `tests/<recipe>.json`. It never overwrites a file and leaves `src/main.rs` alone; `next` names the `mod` line and the call for `update`.
    - Verified: `checkpoint_recipe_applies_to_the_player_templates_and_its_scenario_passes` applies the recipe to the first-person, third-person and 2D templates (dry run first, then a second apply fails with `PROJECT_EXISTS`). It runs the generated scenario with the recipe source compiled in the engine's tests. Breaking the respawn or the checkpoint store makes the test fail. On a real `rusting new --template first-person` project, `recipe apply` then `rusting lint` reports no warnings, and after adding the `mod` line and the call `rusting test` passes the generated scenario.
  - [x] Health and damage. `health` keeps the counter `health` (3 each round), takes a point when `Player` touches class `hazard`, flashes the player, gives a one-second safe window and restarts the round at 0. It adds a sensor `Hazard 1` 2.5 units along +X from the player.
    - Verified: `health_recipe_applies_to_the_player_templates_and_its_scenario_passes` applies it to the first-person, third-person and 2D templates and runs its scenario (health 2 at tick 30, 1 at tick 90, refilled to 3 with the player back at the start by tick 160). Removing the restart fails at tick 160; removing the safe window fails at tick 30.
  - [x] Double jump. The engine gained `air_jumps` on both player controllers; the `double_jump` recipe sets it to 1 with one `set` patch operation and writes no source. Recipes now build patch operations from the `Player` object, and a player with no controller is refused with `RECIPE_NEEDS_CONTROLLER`.
    - Verified: `air_jumps_allow_that_many_jumps_before_landing` (both controllers: the air jump rises, a third press does nothing, landing refills it). `double_jump_recipe_applies_to_the_player_templates_and_its_scenario_passes` runs the scenario on the first-person, third-person and 2D templates: the player climbs higher than one jump can reach from its start; with `air_jumps` 0 it fails at tick 104. A dry run leaves the scene bytes unchanged.
  - [x] Inventory. Touching an object in class `item` removes it and adds to the counter `inventory_<kind>` (kind from the first word of its name); touching one in class `locked` with a key spends the key and removes it. `inventory::count` and `inventory::take` let game code read and spend items. Recipes share a `sensor` helper for their trigger boxes.
    - Verified: `inventory_recipe_applies_to_the_player_templates_and_its_scenario_passes` on the first-person, third-person and 2D templates: the door stays without the key (tick 10), the key is taken (`inventory_key` 1, `Key 1` gone at tick 20), then the door opens and the key is spent (tick 30). Dropping the key check fails at tick 10.
  - [x] Day timer. The counter `day` starts at 1 and goes up every `DAY_SECONDS` (30); `day_seconds_left` counts down, and the HUD label `Day Clock` shows both.
    - Verified: `day_timer_recipe_applies_to_the_player_templates_and_its_scenario_passes` on the three player templates: day 1 with 28 s left at tick 120; moving `day_started_ms` back 30 s gives day 2 with 29 s left at tick 180. Dropping the day increment fails at tick 180.
  - [x] Pause menu. The action `menu` (Escape, `PadStart`) pauses and shows the hidden HUD objects `Pause Title`, `Pause Resume` and `Pause Quit`; pressing it again or clicking Resume carries on, and Quit closes the game. The engine gained `GameScene::clicked()`, the names of HUD buttons clicked since the last frame.
    - Verified: `clicked_names_the_hud_buttons_pressed_last_frame`. `pause_menu_recipe_applies_to_the_player_templates_and_its_scenario_passes` on the three player templates: after `menu` the title shows and a player lifted 5 m stays there for 24 ticks; a `click` on Resume hides the menu and the player falls. Dropping the pause fails at tick 40; dropping the Resume click fails at tick 45.
  - [x] Wave spawner. When no object in class `enemy` is left (the hidden template `Wave Enemy` aside), `wave` goes up and 2 x wave copies appear in a row from `Wave Spawn`; enemies below y = -10 are removed and `wave_enemies_left` counts the rest.
    - Verified: `wave_spawner_recipe_applies_to_the_player_templates_and_its_scenario_passes` on the three player templates: wave 1 brings two visible enemies; dropping one leaves 1; dropping the other brings wave 2 with four, the first at `Wave Spawn`, and wave 1's are gone. Keeping fallen enemies fails at tick 12; a fixed wave size fails at tick 18.
  - [x] Turret. `Turret` shoots the nearest object in class `enemy` within `RANGE` (12 m) that a ray reaches, every `COOLDOWN_SECONDS` (0.5): the enemy is removed and `turret_kills` goes up. Recipes share `cube` and `enemy` scene helpers.
    - Verified: `turret_recipe_applies_to_the_player_templates_and_its_scenario_passes` on the three player templates: the 4 m target is gone and the 6 m one stays at tick 15, the 6 m one is gone by tick 50, and the 20 m one is still there at tick 90 with 2 kills. Dropping the cooldown, the range, or picking the farthest each fail. The line-of-sight check has no scenario yet.
  - [x] Dash. Both player controllers gained `dash_velocity` and `dash_left`, and `GameScene::dash(name, velocity, seconds)` sets them: the body moves at that velocity with no walking, jumping or gravity, and walls still stop it. The `dash` recipe adds the action `dash` (Q, `PadWest`) and dashes 15 m/s for 0.2 s the way the movement keys point (forward, or a platformer's last running direction, when none is held), with a 0.6 s cooldown.
    - Verified: `dash_recipe_applies_to_the_player_templates_and_its_scenario_passes` on the first-person, third-person and 2D templates: a player lifted 1 m dashes more than 2.5 m along its forward (+X in 2D) by tick 40, and a second press 20 ticks later inside the cooldown leaves it short of 3.5 m at tick 80. In the 2D template an unlifted dash stops 0.7 m out against the brick row, as walls should stop it.
- [x] A template matrix for `rusting new --template`: platformer, top-down action, first-person, third-person, puzzle grid, racing, twin-stick shooter, tower defense, roguelike, card game, rhythm game, and physics sandbox. Each passes its scenarios in CI. Verified: every template in the list ships a `tests/*.json` scenario, and a `project::tests` unit test creates each project and runs it in `cargo test` (sub-items below).
  - [x] Puzzle grid. `rusting new --template puzzle` creates Box Push: walls, boxes and goals on whole X-Z cells from a map, a ball player stepped one cell per press of `up`/`down`/`left`/`right` by `src/main.rs`, a `boxes` counter with a target and a `Solved` HUD, and a passing `tests/solve.json`.
    - Verified: `the_puzzle_template_ships_code_whose_scenario_passes` creates the project, checks `src/main.rs` is the template source compiled into the test, and runs `tests/solve.json` with that `update`: the wall stops the player at tick 10, one box is on a goal at tick 30, both at the end. Dropping the wall check fails at tick 10.
  - [x] Top-down action. `rusting new --template top-down` creates Arena: a walled floor, a ball player moved by `up`/`down`/`left`/`right` from `src/main.rs`, three enemies (class `enemy`) that walk toward the player, `attack` (Space, J, pad south) defeating enemies within 1.8 m, a touch costing a hit with a knockback and camera shake, `kills` and `hits` counters with targets, `You win!`/`Game over` HUD texts, attack to restart, and a passing `tests/fight.json`.
    - Verified: `the_top_down_template_ships_code_whose_scenario_passes` creates the project, checks `src/main.rs` is the template source compiled into the test, and runs `tests/fight.json` with that `update`: an attack at tick 60 defeats the nearest enemy, the second enemy costs one hit by tick 175, moving left for 15 ticks puts the player at x = -1.25, and two more attacks win with one hit taken. Shrinking the attack reach to 1.0 m fails at tick 62. `rusting lint` on a new project: no warnings.
  - [x] Racing. `rusting new --template racing` creates Circuit: four road pieces (class `road`) around a 32 by 20 m rectangle on grass, a car driven by `src/main.rs` (`up` accelerates, `down` brakes and reverses, `left`/`right` steer, speed kept in a `speed` counter in cm/s, capped at 5 m/s off the road), four checkpoints passed in order with the finish line last, a `laps` counter with target 3, a `Finished!` HUD, `restart` (R), and a passing `tests/lap.json`.
    - Verified: `the_racing_template_ships_code_whose_scenario_passes` creates the project, checks `src/main.rs` is the template source compiled into the test, and runs `tests/lap.json`: holding `up` and four 43-tick `left` presses at the corners pass checkpoints 1, 2 and 3 in order and count a lap by tick 430, at full speed. Turning at 1.5 rad/s in place of 2.2 fails at tick 235. `rusting lint` on a new project: no warnings.
  - [x] Twin-stick shooter. `rusting new --template twin-stick` creates Swarm: the arena room, WASD or the left stick moving the player, the arrows or the right stick aiming and firing copies of a hidden `Bullet Template` every 150 ms, twelve copies of a hidden `Enemy Template` coming in from the four sides one a second and walking to the player, a shot defeating the enemy it touches, a touch costing a hit, `kills` (target 12) and `hits` (target 3) counters with win and lose HUD texts, `restart` (R), and a passing `tests/wave.json`. New `GameScene::add_class(name, class)` puts a spawned copy in a class, so templates stay out of the loops over it.
    - Verified: `the_twin_stick_template_ships_code_whose_scenario_passes` creates the project, checks `src/main.rs` is the template source compiled into the test, and runs `tests/wave.json`: nine ticks of `right` and of `left` move the player 0.9 m and back, aiming right, left, up and down for a second each defeats the first four enemies with no hit taken, and the fifth reaches the player for one hit. A shot reach of 0 fails at tick 100. `rusting lint` on a new project: no warnings.
  - [x] Tower defense. `rusting new --template tower-defense` creates Outpost: a road through hidden `Waypoint 1..6` to a base, four build pads, ten copies of a hidden `Enemy Template` setting off every 0.5 s at 2.5 m/s, keys 1 to 4 (or pad buttons) building a copy of `Tower Template` on a pad for 50 of 100 `gold`, each tower defeating the furthest enemy within 3.5 m every 1.5 s for 10 gold, `leaks` (target 5) and `cleared` (target 10) counters with win and lose HUD texts, `restart` (R), and a passing `tests/defend.json`. Enemy positions follow from a `clock_ms` counter, so the code keeps no per-enemy state.
    - Verified: `the_tower_defense_template_ships_code_whose_scenario_passes` creates the project, checks `src/main.rs` is the template source, and runs `tests/defend.json`: two towers spend all the gold, a third build is refused (gold 0, no `Tower 3`), the third is built from bounties at tick 600, and the wave ends with 0 leaks and 10 cleared. Mutation: without the third tower the scenario fails with 1 leak; one tower alone leaks 5 (lose). `rusting lint` on a new project reports no diagnostics.
  - [x] Roguelike. `rusting new --template roguelike` creates Crypt: a walled 9 by 7 grid, a turn per step on WASD, the arrows or the d-pad, each floor laid out from `GameScene::seed()` (rubble copied from a hidden `Rubble Template` only on even-even cells so every other cell stays connected, the `Stairs` far from the start, two enemies plus one a floor copied from `Enemy Template`), walking into an enemy to defeat it, enemies next to the player striking for a wound and the others stepping toward it, `depth` (target 3) and `wounds` (target 5) counters with win and lose HUD texts, `restart` (R), and a passing `tests/descend.json`.
    - Verified: `the_roguelike_template_ships_code_whose_scenario_passes` creates the project, checks `src/main.rs` is the template source, and runs `tests/descend.json` on seed 1: forty steps defeat the first two enemies by tick 15, take the stairs to depth 1 with no wound, depth 2 with one, and depth 3 (win) with two. Mutations: seed 2 fails at tick 15 (`Enemy 2` still exists, a different layout), and enemies that strike for no wound fail at tick 55. `rusting lint` on a new project reports no diagnostics.
  - [x] Card game. `rusting new --template card-game` creates Duel: a hand of three cards from a ten-card deck (Strike, Guard, Heal) shuffled from `GameScene::seed()` and shuffled again each time through, played with keys 1 to 3, the face buttons, or each card's HUD button (`Card 1` to `Card 3`, labelled by the code through `set_hud`), card slots colored by kind, a foe whose next attack shows in the `intent` counter before you pick, `foe_damage` (target 30) and `damage` (target 20) counters with win and lose HUD texts, `restart` (R), and a passing `tests/duel.json`.
    - Verified: `the_card_game_template_ships_code_whose_scenario_passes` creates the project, checks `src/main.rs` is the template source, and runs `tests/duel.json` on seed 1: three strikes reach 18 foe damage by tick 7 with 8 taken, a guard against a 7 lets 1 through, a heal takes back 5, and a `click` on the HUD button `1: Strike 6` lands the winning strike (30, with 13 taken). A HUD click reaches `clicked()` two ticks after the click step. Mutations: a Guard that blocks nothing fails at tick 15 (21 taken), seed 3 fails at tick 7, and ignoring HUD clicks fails at tick 29. `rusting lint` on a new project reports no diagnostics.
  - [x] Rhythm game. `rusting new --template rhythm` creates Beat: three lanes, a hit line with a pad per lane, sixteen copies of a hidden `Note Template` sliding down on the beats of a `CHART` at 120 BPM, `lane_1` to `lane_3` on D, F and J (also the arrows and face buttons), within 60 ms scoring 100 (`PERFECT`) and within 140 ms 50 (`GOOD`), a note let past counting as a miss, a `Judgement` HUD text the code sets through `set_hud`, `score`, `misses` (target 8) and `played` (target 16) counters with win and lose HUD texts, `restart` (R), and a passing `tests/song.json`. Note times follow from a `clock_ms` counter, so the code keeps no per-note state. No music ships with it; the module docs say where to start a track.
    - Verified: `the_rhythm_template_ships_code_whose_scenario_passes` creates the project, checks `src/main.rs` is the template source, and runs `tests/song.json`: thirteen notes tapped on the beat, one six ticks (about 100 ms) late, one let past under a press in the wrong lane, and one let past alone give 500 points and 1 miss at tick 280, then 1350 points, 2 misses and all 16 played (song clear) at tick 580. Mutations: every tap 5 ticks late fails at tick 280 (250 points), and a press that ignores the lane fails at tick 280 (0 misses). `rusting lint` on a new project reports no diagnostics.
  - [x] The platformer, first-person, third-person and physics sandbox templates each ship a `tests/*.json` scenario that passes on a new project, so every template in the matrix is checked the same way.
    - Verified: `the_controller_templates_ship_scenarios_that_pass` creates each project and runs its scenario with no game code. 2d `tests/run.json` holds right and jumps over the first bricks (Player y above 1.5 by tick 30, x past -5 at tick 50); first-person and third-person `tests/walk.json` hold forward (z below 3 at tick 60) and jump (y above 1.5); sandbox `tests/drop.json` checks both balls fall and rest above the floor at tick 300. Mutations: no forward press fails first-person at tick 60 (z stays 4.0); no jump fails 2d at tick 30 (y -0.49). GPU lib run on this machine: 812 passed, 1 ignored.
- [x] The skill file and project `AGENTS.md` are generated from the schema, the API index, and a pitfall catalog, and checked for staleness in CI. Verified: the API table (both guides), the component list and the pitfall list (SKILL.md) are generated, and `cargo test` fails when any is stale (sub-items below).
  - [x] Staleness check against the API index. `agent_guides_cover_every_scene_method_and_name_only_real_ones` fails when a `GameScene` method is named in no guide page or the project AGENTS.md, or when SKILL.md or AGENTS.md writes a call (`name(..)`, `scene.name(..)`) that is no method in the API index. SKILL.md gained the eight methods it missed (material spawns, `set_background_color`, `hit_stop`, and a GPU bodies row).
    - Verified: dropping `hit_stop` from SKILL.md fails with `no guide names ["hit_stop"]`; misspelling `raycast` as `raycastt` fails with "names no API method".
  - [x] Generate the API table in SKILL.md and the project AGENTS.md from the API index and schema instead of writing it by hand.
    - Verified: `docs::api_table` builds the "Need | Call" table from the API index (every `GameScene` method under its need in `API_GROUPS`, call forms taken from the signatures, plus the `GameObject` methods), and both guides hold it between `<!-- api-table:start -->` markers. `agent_guides_hold_their_generated_parts` fails on a hand edit (dropping `hit_stop(seconds)` from SKILL.md gives "the API table is stale") and on a group naming no method (`hit_stopp`); `RUSTING_UPDATE_GOLDEN=1` rewrites both files. Notes the signatures cannot carry stay as prose under the table. The table names all 149 `GameScene` methods.
  - [x] Generate the component list under "Gameplay without code" in SKILL.md from the schema's component summaries, checked the same way.
    - Verified: `docs::component_list` writes each registered component's key and the first sentence of its schema summary between `<!-- components:start -->` markers in SKILL.md, replacing the hand list of six names; all 41 components are listed. `agent_guides_hold_their_generated_parts` fails on a hand edit (changing the `rusting.counter` line gives "the generated components is stale"). `first_sentence` no longer ends a sentence at `..`.
  - [x] List the pitfall catalog's entries in SKILL.md from `docs/pitfalls.md`, checked the same way.
    - Verified: `docs::pitfall_list` writes one line per `docs/pitfalls.md` section with its entries' titles (all 26) between `<!-- pitfalls:start -->` markers in SKILL.md. Renaming the "A scaled player" entry in the catalog fails `agent_guides_hold_their_generated_parts` with "the generated pitfalls is stale".
- [x] A pitfall catalog grown from every logged gap in the "Current focus" list, each with its diagnostic code and fix.
  - Evidence: `docs/pitfalls.md`, served as `rusting docs show guide/pitfalls` and linked from SKILL.md and the project AGENTS.md, has 26 entries from the friction logs and SKILL rules, each with a symptom, `Code:` and `Fix:`. Test `pitfall_catalog_names_only_real_codes_and_every_entry_has_a_fix` checks every named code is in the diagnostics registry (a fake `LINT_ZERO_SCALEX` fails it) and every entry has both lines.
- [x] A task cookbook of runnable snippets ("make an enemy follow the player", "add a level select"), each tested.
  - Evidence: snippets live in `src/cookbook/` and are served as `rusting docs show cookbook/<name>` (header as the page, then the code); `rusting docs search cookbook` lists them. `src/cookbook.rs` compiles each one into the tests and runs it against a template project: `follow_player_walks_a_chaser_to_the_player_and_stops` (first- and third-person: 2.5 to 3.5 m in one second, facing the player, then waiting 1.5 m away; a flipped yaw fails at tick 60) and `level_select_loads_the_clicked_level_and_keeps_the_score` (clicking `Level 2` loads `scenes/level_2.rscene` with the score kept; dropping the score carry fails at tick 12). `every_snippet_has_a_title_and_usage` checks each header. New snippets go in `COOKBOOK` with a test.

### Exit gate

- Every template and recipe passes its scenarios and `rusting lint` in CI.
- Every closed gap in this roadmap either has a diagnostic that catches it or a pitfall entry.

## Milestone L8: Fast logic lane and safe execution

Goal: gameplay logic can change in under a second without a Cargo rebuild, inside a sandbox, and every agent action can be reviewed and reverted.

Depends on: Milestone 9 WASM scripting host and Rust hot reload.

- [ ] A script-first lane: gameplay systems written against the reflected ECS API in WASM (compiled from Rust) swap in a running game without restart, with the same `GameScene` surface as native code where the sandbox allows.
- [ ] A promotion path from a WASM script to native Rust with no behaviour change, checked by state hashes.
- [x] Permission scopes for agent-facing commands: project-root confinement, a `--read-only` mode, `--confirm` for destructive operations, and no network access unless a generator hook asks for it.
  - [x] A `--read-only` mode. Any command takes `--read-only`, and `RUSTING_READ_ONLY=1` (not empty, not `0`) sets it for a session. Only commands that write no file run (`cli::READ_ONLY_COMMANDS`: inspection, query and map, lint, validate, docs, explain, schema, diff, log, provenance, systems, impact and the list commands) plus `scene patch` and `fix` in the exact `--dry-run` shapes the command line parses (a `--dry-run` passed as another flag's value does not count); every other command fails with `READ_ONLY` before it starts, and `write_atomic` refuses every write as a backstop. The daemon sets the mode again for each command, so one read-only request does not stick. `inspect --tick`, `check`, `run` and `test` build into `target/` and `build/`, so they are refused. The catalog lists the commands under `read_only`.
    - Verified: `tests/cli.rs` `read_only_mode_runs_inspection_and_refuses_writes` (through the binary: `scene query`, `lint` and `scene patch --dry-run` run; `scene patch` and `add system` fail with `READ_ONLY`; `RUSTING_READ_ONLY=1` refuses the patch and leaves the scene and `src/main.rs` byte for byte unchanged; `RUSTING_READ_ONLY=0` applies it; `lease claim --as --dry-run` is refused; every allowlisted command, run read-only on a fresh project, leaves every file in it byte for byte unchanged).
  - [x] Project-root confinement. Any command takes `--confine DIR`, and `RUSTING_CONFINE=DIR` sets it for a session. The command fails with `OUTSIDE_CONFINE` before it starts when the working folder, or an argument that exists or holds a path separator, resolves outside DIR; links and `..` are resolved first (`lease::resolve`: the longest existing ancestor canonicalized, a `..` past a missing folder counts as outside). Any other word is at most a path relative to the working folder, which is inside. `write_atomic` refuses writes outside DIR as a backstop.
    - Verified: `tests/cli.rs` `confine_refuses_paths_outside_the_folder_after_resolving_links` (from inside the project, `lint` and a `..` path that comes back inside run; a patch file in the parent folder and a link in the project that points to the parent are refused; a run from outside the project is refused; `RUSTING_CONFINE` stops `rusting new` from creating a project next to it).
  - [x] No network access unless a generator hook asks for it. The engine opens no connection itself (no socket or HTTP code in `src/`); its only network use is cargo fetching crates. Any command takes `--offline`, and `RUSTING_OFFLINE=1` sets it for a session: every cargo run (`check`, `run`, `test`, `export`, and `cargo metadata`) gets `--offline`. Generator hooks are the project's own commands and keep network access. The daemon sets the mode again for each command. The catalog documents it under `offline`.
    - Verified: `tests/cli.rs` `offline_mode_keeps_cargo_off_the_network` (with an empty `CARGO_HOME`, `check --offline` and `RUSTING_OFFLINE=1 check` fail with cargo's offline-mode resolution error instead of downloading). With the normal cargo cache, `rusting check --offline` on a roguelike template copy builds. Full AGENTS.md check green.
  - [x] `--confirm` for destructive operations, met by making every destructive command revertible instead of adding a flag: every write is journaled and `rusting revert <op>` undoes it, and `scene split`/`join` now journal each file of the old form they delete. `revert` deletes before it restores and creates missing parent folders, so a scene goes back from folder to file form and back.
    - Verified: `cli::hint_tests::revert_undoes_scene_split_and_join` (a split reverts to the original file byte for byte; a join reverts to the same folder). Without the deletion-first order the test fails.
- [x] An operation journal: every mutating command records the tool, the command, the diff, and the time. `rusting log` lists it and `rusting revert <op>` undoes one operation when later operations do not depend on it.
  Evidence: every CLI and daemon command runs as one journal operation (`runtime::journal::begin` in `execute`); each project file written through `write_atomic` (scene, prefab and merge writes, and now `add scenario`, `add system`, recipe sources and the AGENTS.md refresh, which moved off `std::fs::write`) appends an entry to `.rusting/journal.jsonl` with op ID, time, tool (`RUSTING_AGENT` or `rusting`), command and before/after content hashes, with contents in `.rusting/blobs/`. `rusting log` lists operations newest first; `rusting revert <op>` restores every file (deleting created ones) as a new operation, or refuses with `REVERT_CONFLICT` naming the later operation. Tests: `runtime::journal::tests::writes_in_an_operation_are_logged_and_revert_until_changed_again` and `cli::hint_tests::log_lists_operations_and_revert_undoes_one`; smoke run of the debug binary: `add system`, `add scenario`, `log`, then two reverts restore both. Full AGENTS.md check green. Limits: the diff is kept as before/after blobs (no line diff in the output); imported asset files and editor saves are not journaled; a revert over a conflicting later change is refused rather than merged.
- [x] A portable provenance report: engine and plugin versions, asset hashes, generator metadata, seeds, and scenario versions (from `missingFeatures.md` P2).
  Evidence: `rusting provenance [root]` reports the tool version, every `rusting*` crate in `Cargo.lock` (version and source; a git source carries the commit), one hash over `src/`, every imported asset's hash, ID and `.rmeta` provenance, the generator hooks, each scene's hash, and each scenario's seed, ticks and hash. `cli::hint_tests::provenance_records_crates_code_assets_scenes_and_scenarios` (a lock with `rusting_engine` and `serde` lists only the engine with its git source; scene and scenario entries; adding a code file changes `code_hash` but not scene hashes). On forever-bear: 2 crates, 20 code files, 1 scene, 61 scenarios with seeds. Full AGENTS.md check green. Limit: `Cargo.lock` is read line by line (no TOML dependency); determinism mode is included, but GPU driver and hardware are not.

### Exit gate

- A logic change in a running sample reaches the game in under one second (hardware gate).
- Every agent change in a benchmark run is listed in the journal and revertible.

## Milestone L9: Agent benchmark and evidence

Goal: prove the "best engine for LLM coding" claim with a repeatable benchmark, the way Milestone 12 proves the physics claim.

Depends on: Milestones L1-L5 for the tools it measures; can start earlier with the tools that exist.

- [x] `benchmarks/agent/` first slice: the task format (request, template, seed and reference edits, hidden scenarios) and three tasks, two bug fixes in seeded broken projects (`rhythm-late-miss`, `duel-guard`) and one feature addition (`rhythm-combo`). Evidence: `tests/agent_benchmark.rs` `every_task_is_well_formed_and_applies_to_its_template` passes in the normal suite; the ignored `hidden_scenarios_fail_when_seeded_and_pass_with_the_reference` passed for all three tasks (each hidden set fails on the seeded project and passes with the reference).
- [x] `benchmarks/agent/` coverage: new-game and performance-fix tasks, and enough tasks per kind for a pass rate to mean something. Partly done: `swarm-shot-leak` is the first performance task (twin-stick shots past the arena wall are never despawned; the hidden `fire.json` holds fire for 600 ticks under a `max_entities: 50` budget, which the reference meets at 35 and the seeded leak breaks at 72, while the template's wave scenario guards gameplay). Evidence: the ignored `hidden_scenarios_fail_when_seeded_and_pass_with_the_reference` passed for all four tasks. Slice 2026-10-09: `clicker-new-game` is the first new-game task, from the empty template; tasks take `reference_patches`, scene patch files the reference applies after its edits, and the hidden `clicks.json` taps `click` twelve times and checks `clicks` and `won` before, at and after the win. Evidence: the ignored validation passed for all five tasks (34 s). Slice 2026-10-09 (2): `tower-sell` is the second feature task, on the tower-defense template: `sell_1` to `sell_4` remove a tower for a 25 gold refund, and the hidden `sell.json` checks the refund, that an empty pad sells nothing and that a sold pad builds again, beside the template's own defend scenario; the reference adds the actions through a scene patch. Evidence: the ignored validation passed for all six tasks (120 s). Slice 2026-10-09 (3): `racing-wide-road` is the third bug-fix task: the seed doubles the road's half-width, so grass beside the track counts as road; the hidden `off.json` sets the car 6 m off Road South and expects the off-road 5 m/s at tick 100 (the seeded project reaches 14 m/s), beside the template's lap scenario. Evidence: the ignored validation passed for all seven tasks (36 s). Slice 2026-10-09 (4): `walker-new-game` is the second new-game task, from the empty template: a `Player` cube steps 1 m per `left` or `right` tap between x = -3 and 3 and wins on the `Goal` at x = 3; the hidden `walk.json` checks one step, the clamp at -3, no win at x = 2, the win at 3, and that taps after the win change nothing. Evidence: the ignored validation passed for all eight tasks (55 s). Slice 2026-10-09 (5): `puzzle-box-stack` is the fourth bug-fix task, on the puzzle template: the seed drops the box check behind a pushed box, so boxes stack; the hidden `stack.json` sets `Box 2` beside `Box 1` and checks that pushing stops at both boxes, beside the template's solve scenario. Evidence: the ignored validation passed for all nine tasks (52 s). Slice 2026-10-09 (6): `arena-cooldown` is the third feature task, on the top-down template: a 0.5 s attack cooldown; the hidden `cooldown.json` sets an enemy in reach before each of three attacks and checks that the one 10 ticks after the first defeats nothing and the one 40 ticks after does, beside the template's fight scenario. Evidence: the ignored validation passed for all ten tasks (61 s). Slice 2026-10-09 (7): `rhythm-spark-leak` is the second performance task, on the rhythm template: the seed adds hit sparks that rise for 300 ms and are then hidden instead of despawned, so every hit leaves an entity the update loop keeps visiting; the hidden `sparks.json` plays the song under a `max_entities: 32` budget, which the reference meets at 28 and the seeded leak breaks at 38, beside the template's song scenario. Evidence: the ignored validation passed for all eleven tasks (59 s). Slice 2026-10-09 (8): `tapper-new-game` is the third new-game task, from the empty template: `time_left` counts down from 10 once a second of fixed ticks, each `tap` adds to `score` while time is left, and `Time up!` ends scoring; the hidden `taps.json` checks the clock at 5, 90, 330, 595 and 650 ticks and that taps at 610 and 630 score nothing. Evidence: the ignored validation passed for all twelve tasks (79 s). Slice 2026-10-09 (9): `swarm-bomb` is the fourth feature task, on the twin-stick template: one bomb a round (`bombs` starts at 1) defeats every enemy within 3 m of the player as kills; the hidden `bomb.json` taps `bomb` at tick 200, when Enemy 1 is about 2.3 m away and Enemy 2 about 4.3 m, checks that only Enemy 1 goes, then taps again at 260 with Enemy 2 in reach and checks that nothing happens, beside the template's wave scenario. Evidence: the ignored validation passed for all thirteen tasks (92 s). Slice 2026-10-09 (10): `crypt-diagonal` is the fifth bug-fix task and the first on the roguelike template: the seed lets an enemy diagonal to the player strike (`gx.abs() <= 1 && gz.abs() <= 1`); the hidden `diagonal.json` (seed 1) sets Enemy 1 so it ends the player's step diagonal to it and expects no wound, then sets it straight beside the player's next cell and expects one, beside the template's descend scenario. On the seeded project `diagonal.json` fails at tick 4 (`wounds` is 1) and descend at tick 15. Evidence: the ignored validation passed for all fourteen tasks (83 s). Slice 2026-10-09 (11): `platformer-goal` is the fifth feature task and the first on the 2D platformer template: touching the gold Goal sets a new `cleared` counter to 1 once and changes the Help text to `Level complete!`; the hidden `goal.json` checks `cleared` is 0 at tick 5, sets the Player onto the Goal at tick 10, expects `cleared` 1 within 20 ticks and still 1 at tick 85 with the new Help text, beside the template's run scenario. The seeded project fails at tick 20. Evidence: the ignored validation passed for all fifteen tasks (96 s). Slice 2026-10-09 (12): `reaction-new-game` is the fourth new-game task, from the empty template: `ready` turns 1 and the screen says `Go!` 120 ticks after the start, a `react` press before that adds to `false_starts` and restarts the wait, and the first press after `Go!` sets `reaction_ms`; the hidden `react.json` false-starts at tick 60, checks `ready` is still 0 at 170 and 1 at 190, reacts at 220 and expects 667 ms within 60, and checks a later press changes nothing. The seeded project fails at tick 60 (`react` has no bindings). Evidence: the ignored validation passed for all sixteen tasks (88 s). Slice 2026-10-09 (13): `racing-lap-times` is the sixth feature task, on the racing template: `lap_ms` holds the last lap and `best_ms` the fastest, in ms of game time from the race start or the previous lap; the hidden `laps.json` sets the Car onto the four checkpoints in turn for laps of 80, 40 and 240 ticks and expects 1333, 667 and 4000 ms (within 50) with the best at 667, beside the template's lap scenario. The seeded project fails at tick 85. Evidence: the ignored validation passed for all seventeen tasks (102 s). Slice 2026-10-09 (14): `tower-reach` is the sixth bug-fix task, on the tower-defense template: the seed checks only the X distance to a target, so a tower hits enemies level with it along X however far off in Z; the hidden `reach.json` builds Tower 1, sets it 14 m from the path at x = -10, level with the first leg, and expects Enemy 1 alive at tick 120, no `cleared` and 50 gold at tick 199, beside the template's defend scenario. Evidence: the ignored validation passed for all eighteen tasks (124 s). Slice 2026-10-09 (15): `puzzle-undo` is the seventh feature task, on the puzzle template: a new `undo` action takes back the last move, with any box it pushed, and the `boxes` counter follows; the hidden `undo.json` checks that undo before any move does nothing, undoes a push of Box 1, then pushes Box 1 onto Goal 1 and checks that undo brings `boxes` back to 0 with the box and player a cell back, beside the template's solve scenario. Evidence: the ignored validation passed for all nineteen tasks (92 s). Slice 2026-10-09 (16): `swarm-diagonal-speed` is the seventh bug-fix task, on the twin-stick template: the seed drops the division by the input's length, so diagonal moves run at 1.4 times PLAYER_SPEED; the hidden `diagonal.json` holds right and down for 30 ticks and expects about 2.05 m on each axis (the seeded project reaches 2.9), then holds left for 30 ticks and expects x near -0.95, beside the template's wave scenario. Evidence: the ignored validation passed for all twenty tasks (109 s). Slice 2026-10-09 (17): `duel-redraw` is the eighth feature task and the first feature on the card-game template: a once-per-duel `redraw` before the first card swaps the hand for the next three cards without a turn, counted in `redraws`; the hidden `redraw.json` (seed 1) checks `drawn` goes from 3 to 6 with `turn` and `damage` still 0, that a second redraw changes nothing, and that the next play still takes a turn and draws one card, beside the template's duel scenario. Evidence: the ignored validation passed for all twenty-one tasks (117 s). Slice 2026-10-09 (18): `arena-knockback` is the eighth bug-fix task and the first bug-fix on the top-down template: the seed flips the knockback's sign, so a hit throws the enemy through the player; the hidden `knockback.json` lets Enemy 1 walk in from +X onto the still player and expects one hit with Enemy 1 near x = 2.75 at tick 95, beside the template's fight scenario. Evidence: the ignored validation passed for all twenty-two tasks (134 s). Slice 2026-10-09 (19): `lock-new-game` is the fifth new-game task, from the empty template: actions `a`, `b` and `c` enter the code a, c, b, c, with `progress`, `mistakes` and `unlocked` counters and an `Unlocked` message; the hidden `lock.json` checks the counters start at 0, that a wrong digit after two right ones resets `progress` and counts a mistake without starting a new attempt, that the full code unlocks only on its last digit, and that presses after that change nothing. Evidence: the ignored validation passed for all twenty-three tasks (137 s). Slice 2026-10-09 (20): `crypt-wait` is the ninth feature task and the first feature on the roguelike template: a `wait` action spends a turn without moving; the hidden `wait.json` (seed 1) sets Enemy 1 two cells below the player and Enemy 2 in the far corner, then checks that the first wait lets Enemy 1 step next to the still player without a wound and the second brings one wound, beside the template's descend scenario. The validation test now takes `RUSTING_BENCH_TASK` to check one task. Evidence: `RUSTING_BENCH_TASK=crypt-wait` validation passed (8 s). Slice 2026-10-09 (21): `platformer-bricks` is the ninth bug-fix task and the first whose bug is in scene data, not code: the seed marks the 2D template's brick tile as not solid in the tile map, so the player falls through the ledges; the hidden `bricks.json` drops the Player over the middle ledge and expects it to rest between y = 2.5 and 4.5 at tick 60, beside the template's run scenario. Evidence: `RUSTING_BENCH_TASK=platformer-bricks` validation passed (12 s). Slice 2026-10-10: `swarm-glass-enemies` is the third performance task and the first about draw calls, not entities: the seed gives the twin-stick `Enemy Template` a blended material, and each blended instance draws alone; the hidden `draws.json` raises the `hits` target so all twelve enemies live and holds the last frame to `max_draws: 8`, which the reference (opaque again) meets at 5 and the seeded project breaks at 17 on the RTX 3060, beside the template's wave scenario. Evidence: `RUSTING_BENCH_TASK=swarm-glass-enemies` validation passed (53 s). Slice 2026-10-10 (2): `jump-new-game` is the sixth new-game task and the first that needs physics: a dynamic `Ball` lands on a fixed `Floor`, `jump` (Space) sets 5 m/s upward only while the ball touches the floor, and `jumps` counts jumps; the hidden `jump.json` expects the ball at rest at y = 0.5 by tick 120, jumps at 150 and expects y > 1 at 165, checks a mid-air press at 170 adds no jump, and expects the ball back at rest by 250. Writing the reference showed that a CPU body that fell asleep on the floor dropped out of `PhysicsWorld::contacts`, so `touching` said a resting ball touched nothing; sleepers now keep the contacts they fell asleep with (unit test `cpu_bodies_sleep_when_still_and_wake_on_touch_or_edit`). Evidence: `RUSTING_BENCH_TASK=jump-new-game` validation passed (37 s). Slice 2026-10-10 (3): `sandbox-reset` is the tenth feature task and the first on the physics sandbox template: a `reset` action (R) puts every box and ball back at its start, at rest, without reloading, and counts `resets`; the hidden `reset.json` moves Box 9 off the pile at tick 100, resets at 110, expects Box 9 back at (1.1, 2.6, 0) and Ball 2 above y = 7 two ticks later with `resets` at 1, and checks the pile has not thrown Box 9 away again by tick 290, beside the template's drop scenario. Evidence: `RUSTING_BENCH_TASK=sandbox-reset` validation passed (57 s). Slice 2026-10-10 (4): `sandbox-hail` is the fourth performance task and the first about triangles: the seed rings the sandbox pile with thirty `spawn_sphere` hailstones at 96 subdivisions; the hidden `hail.json` checks all thirty exist at tick 470 under `max_triangles: 100000`, which the reference (8 subdivisions) meets at 19,792 triangles and 0.13 ms of GPU time and the seeded project breaks at 2,216,272 and 0.69 ms on the RTX 3060, beside the template's drop scenario. Evidence: `RUSTING_BENCH_TASK=sandbox-hail` validation passed (8 s). Slice 2026-10-10 (5): `fps-double-jump` is the tenth bug-fix task and the first on the first-person template: the seed sets the Player's `air_jumps` to 1 in the scene's controller data, so a press in mid-air jumps again; the hidden `air.json` jumps at tick 30, expects the player above y = 1.8 at 50, presses jump again at 55, and expects them below 2.4 at 70 and back on the floor (below 1.1) at 105, beside the template's walk scenario. Evidence: `RUSTING_BENCH_TASK=fps-double-jump` validation passed (5 s). Slice 2026-10-10 (6): `tps-coins` is the eleventh feature task and the first on the third-person template: three coins at given places, each collected within 1 m of the player into a `coins` counter, and `All coins!` on the Help text after the third; the hidden `coins.json` sets the Player onto each coin in turn, checks each disappears and counts once (a return to Coin 1's spot adds nothing), and that the Help text changes only on the third, beside the template's walk scenario. Evidence: `RUSTING_BENCH_TASK=tps-coins` validation passed (7 s). Now 6 new-game, 11 feature, 10 bug-fix and 4 performance tasks. Slice 2026-10-10 (7): `cube-quarter-turn` is the twelfth feature task and the first on the 3d template: a `turn` action (T) starts a 90-degree Y turn of the Cube over 0.25 s, a press mid-turn is ignored, and a `turns` counter counts started turns; the hidden `turn.json` checks the angle mid-turn, that it rests at pi/2 (so a queued press fails), and a second turn to pi. Evidence: `RUSTING_BENCH_TASK=cube-quarter-turn` validation passed (49 s). Now 6 new-game, 12 feature, 10 bug-fix and 4 performance tasks. Slice 2026-10-10 (8): `starter-flag-gate` is the eleventh bug-fix task and the first on the starter template: the seed clears the Flag pickup's `requires: coins`, so touching the flag wins with no coins; the reference restores it. The hidden `gate.json` puts the Player on the flag (no win, flag not collected), sets `coins` to 5, steps off and back on, and expects `won` 1. Evidence: `RUSTING_BENCH_TASK=starter-flag-gate` validation passed (9 s). Now 6 new-game, 12 feature, 11 bug-fix and 4 performance tasks; every template has at least one task. Slice 2026-10-10 (9): `crate-plate-new-game` is the seventh new-game task: a held `push` action (P) slides a dynamic `Crate` at 2 m/s toward a sensor `Plate`, and the crate touching the plate removes the `Door` and sets `opened` to 1. The hidden `plate.json` checks the crate's place after half a second of pushing, that the door stands until the crate arrives, and that it is gone within 35 ticks of release. Evidence: `RUSTING_BENCH_TASK=crate-plate-new-game` validation passed (12 s). Now 7 new-game, 12 feature, 11 bug-fix and 4 performance tasks. Slice 2026-10-10 (10): three more new-game tasks from the empty template. `lift-new-game`: a `Lift` box rises 3 m at 1.5 m/s, comes back down and repeats, and `trips` counts arrivals at the top; the hidden `lift.json` checks the height at ticks 60, 120, 180, 240 and 300 and `trips` before and after each arrival. `seed-planter-new-game`: one copy of a `Seed` template a second, named `Seed n` at x = 2n, five at most, counted in `planted`; the hidden `plant.json` checks the names, places, the count and that no sixth copy appears. `lamp-switch-new-game`: each press of `switch` (L) toggles the point light `Lamp` between intensity 0 and 8 and counts `switches`; the hidden `switch.json` holds the key for 30 ticks (one toggle) and taps it twice more. Evidence: `RUSTING_BENCH_TASK=<task>` validation passed for each (41 s, 6 s, 7 s), and the unignored `agent_benchmark` task check passes. Now 10 new-game, 12 feature, 11 bug-fix and 4 performance tasks. Slice 2026-10-10 (11): `racing-speedo-leak` is the fifth performance task and the first on the racing template: the seed adds a speedometer, `shown_speed`, averaging the last 30 frames, but stores each frame's speed in a new `speed_at_<frame>` counter, so every frame adds an entity (counters are entities) that the game and saves keep; the reference keys them by `frame % SMOOTHING`, a ring of 30. The hidden `speedo.json` sets the Car off road, holds `up` for 600 ticks under a `max_entities: 80` budget, which the reference meets at 54 and the seeded leak breaks at 625, and checks the average still lags (170 at tick 20) and settles at 500, beside the template's lap scenario. Evidence: `RUSTING_BENCH_TASK=racing-speedo-leak` validation passed (156 s), and the unignored `agent_benchmark` task check passes. Now 10 new-game, 12 feature, 11 bug-fix and 5 performance tasks. Slice 2026-10-11: `tower-flash-leak` is the sixth performance task and the first about lights: the seed adds a hidden `Flash Template` point light to the tower-defense scene and lights a copy over each tower that hits, but puts spent flashes out with intensity 0 instead of removing them, and a visible light at intensity 0 still goes to the GPU light list; the reference despawns them. The hidden `flash.json` builds two towers, checks `Flash 1` exists right after the first hit at tick 181, and holds the last frame of 1200 ticks to `max_lights: 3`, which the reference meets at 1 (the sun) and the seeded project breaks at 10, beside the template's defend scenario. Evidence: `RUSTING_BENCH_TASK=tower-flash-leak` validation passed (7 s), and the unignored `agent_benchmark` task check passes. Now 10 new-game, 12 feature, 11 bug-fix and 6 performance tasks. Slice 2026-10-11 (2): `arena-dust-leak` is the seventh performance task and the first on the top-down template: the seed adds a hidden `Dust Template` sphere and kicks up a copy under the moving player each frame, shrinking each puff by 0.9 a frame and hiding it once it is too small instead of despawning it; the reference despawns it. The hidden `dust.json` runs up, down and left for 600 ticks under `max_entities: 90`, which the reference meets at 51 and the seeded project breaks at 174, and checks `Dust 1` exists and is visible at tick 2, beside the template's fight scenario. Evidence: `RUSTING_BENCH_TASK=arena-dust-leak` validation passed (6 s), and the unignored `agent_benchmark` task check passes. Now 10 new-game, 12 feature, 11 bug-fix and 7 performance tasks. Slice 2026-10-11 (3): `crypt-tile-leak` is the eighth performance task and the first on the roguelike template, and its leak is a missed cleanup on level change rather than a hidden object: the seed adds a hidden `Tile Template` and paves every cell with a `Tile <floor> <x> <z>` copy each time a floor is built, but the floor's clear loop removes only rubble and enemies, so each floor stacks 63 more tiles under the new ones; the reference clears class `tile` too. The hidden `tiles.json` replays the descend run on seed 1 under `max_entities: 150`, which the reference meets at 95 and the seeded project breaks at 219, and checks `Tile 0 1 1` exists at tick 1 and `depth` reaches 3, beside the template's descend scenario. Evidence: `RUSTING_BENCH_TASK=crypt-tile-leak` validation passed (9 s), and the unignored `agent_benchmark` task check passes. Now 10 new-game, 12 feature, 11 bug-fix and 8 performance tasks. Slice 2026-10-11 (4): `puzzle-trail-leak` is the ninth performance task and the first on the puzzle template: the seed adds a hidden `Step Template` and drops a `Step n` footprint on the cell the player leaves at each move, keeping a trail of eight by hiding `Step n-8` instead of despawning it; the reference despawns it. The hidden `trail.json` steps up and down 150 times under `max_entities: 80`, which the reference meets at 49 and the seeded project breaks at 191, and checks that `Step 1` shows after the first move and `Step 143` and `Step 150` still show at the end, beside the template's solve scenario. Evidence: `RUSTING_BENCH_TASK=puzzle-trail-leak` validation passed (7 s), and the unignored `agent_benchmark` task check passes. Now 10 new-game, 12 feature, 11 bug-fix and 9 performance tasks. Slice 2026-10-11 (5): `platformer-sparkle-leak` is the tenth performance task and the first on the 2D platformer and the first fixed in scene data: the seed gives the goal's burst emitter a steady `rate` of 12 sparks a second but a `lifetime` of 30 s, so sparks pile up below the level; the reference keeps the rate and restores the 0.8 s lifetime. The hidden `sparkle.json` idles 600 ticks under `max_entities: 120`, which the reference meets at 71 and the seeded project breaks at 181, beside the template's run scenario. The render lane's shadows merge raised the twin-stick baseline from 5 to 11 draws, so `swarm-glass-enemies`' `max_draws` goes from 8 to 16 (reference 11, seeded 23). Evidence: `RUSTING_BENCH_TASK=platformer-sparkle-leak` and `swarm-glass-enemies` validations passed (7 s each), the full ignored validation of all 42 earlier tasks passed (149 s), and the unignored `agent_benchmark` task check passes. Done at 10 new-game, 12 feature, 11 bug-fix and 10 performance tasks.
- [x] A model-agnostic runner that drives any agent CLI through a command hook and records wall time, turns, output size, commands, failed edits, corrective builds, human interventions, and hidden-scenario pass rate. Evidence: `benchmarks/agent/run.py --agent '<command>'` seeds each task, runs the command with the request on stdin, and reports per task; commands, failed commands and corrective builds come from the new `RUSTING_COMMAND_LOG` (test `the_command_log_records_every_command` in `tests/cli.rs`); turns, failed edits and interventions come from an optional agent-written `RUSTING_BENCH_AGENT_REPORT` file. A scripted fake agent on `duel-guard` and `rhythm-combo` gave pass rate 0.5, with 2 commands, 1 failed and 1 corrective build each, as scripted.
- [ ] Baselines for the same tasks in Godot and at least one other engine, run with the same agent and budget.
- [ ] A scheduled job that runs the suite for every release and publishes a report; a regression opens an item in this file.
- [ ] Every failed task is triaged into a gap item in the owning milestone, as the sample games were.

### Exit gate

- The published report shows RustingEngine ahead of the baselines on pass rate and time, and every task where it is not has a follow-up item.

## Milestone L10: Large projects and multiple agents

Goal: agents stay effective on a project with hundreds of scenes and systems, and several agents can work on one project at once.

Depends on: Milestones L1, L3, and L4.

- [x] `rusting project summary [--budget N]`: a budgeted overview of scenes, systems, components, assets, and their dependencies.
  Evidence: `cli::hint_tests::project_summary_lists_scenes_code_and_components_within_budget` (puzzle template plus a code file: scene path, entity count and components; the code file's `GameScene` function and its `spawn_prefab` path; component counts; a 150-token budget cuts items and reports `omitted`); on surveyor, budgets 300/600/5000 give 249/507/997 tokens of data. Scenes and prefabs are every `.rscene` under `scenes/` and `assets/`; a "system" is a game code function whose signature names `GameScene` (a text scan, no ECS system graph yet; that is the next item); code dependencies are literal asset paths in `load_text`, `play_sound*` and `spawn_prefab` calls (`spawn_prefab` paths are now also checked by `CODE_MISSING_ASSET`). Over budget, the longest list anywhere is halved until the JSON fits.
- [x] A system access graph from ECS system parameters: which systems read and write which components and resources, queryable as "who writes `Health`?".
  `rusting systems [--reads TYPE | --writes TYPE]` lists every engine ECS system by stage with the components and resources it reads and writes, read from bevy's access sets (`App::into_system_access`, which re-initializes the systems on a throwaway app); systems that take the whole `World` are `all: true` and always kept by the filters. Evidence: `cli::hint_tests::systems_name_who_writes_a_component` (`--writes Transform` keeps `player_face` in FixedUpdate, drops `draw_hud`, keeps `propagate_transforms` as `all`); full AGENTS.md check green. Game code: `rusting systems [root]` (root defaults to `.`) adds every game code function with stage `Game`, `file` and `line`, from a text scan: bevy system parameters (`Query<&mut Health>` writes `Health`, `Res<T>` reads `T`, a `World` parameter is `all`), a table of `GameScene` calls (`set_position` writes `Transform`, `set_counter` writes `Counter`, ...), `set_field` paths (`/components/game.health/..` writes `game.health`) and `world()`. Evidence: `cli::hint_tests::systems_scan_game_code_for_who_writes_a_component` (a `Query<(Entity, &mut game::Health), With<Enemy>>` system is found by `--writes Health` at its line; a `GameScene` function writes `Counter`, `Transform` and `game.health`; a `World` function is `all`; `Iterator::position(|..|)` is not a `Transform` read; a plain function is not listed). On forever-bear the scan lists 120 game functions and `--writes Counter` names them with file and line; on playplace 26. Full AGENTS.md check green. Limits: a text scan, so calls through helper functions, macros or variables are missed, and a component reached by `set_field` is named by its registered name (`rusting.player_controller`), not its type.
- [x] Scene files that merge well: stable key and entity order.
  Evidence: `runtime::scene_file::tests::saving_is_stable_so_one_edit_changes_one_line` (six entities: save, load, save gives the same bytes; renaming one entity changes exactly one line). Saves already sort entities by ID, components are a `BTreeMap`, and fields follow struct order, so the test pins existing behavior. Full AGENTS.md check green.
- [x] An optional folder form of a scene with one file per entity or prefab, for fewer merge conflicts on large scenes.
  `rusting scene split PATH` turns a scene into a `.rscene` directory: `scene.json` holds everything but the entities and lists their IDs in order, and `entities/<id>.json` holds each entity; `rusting scene join PATH` turns it back. `read_scene_bytes` joins a folder into the canonical single-file JSON, so revisions match across forms, and `write_atomic` on a scene directory splits the bytes and writes only the files whose content changed (removed entities' files are deleted and journaled). The runtime loader, cook, every CLI scene command, patches and the editor's revision check go through them. Evidence: `scene_patch::tests::a_patch_on_a_folder_form_scene_rewrites_only_the_changed_entity` (the folder joins to the same bytes; renaming one entity rewrites only its file, not `scene.json` or the other entity; `revision_after` chains into a second patch that deletes the other entity and removes its file). On a roguelike template copy: split, `scene inspect`, `validate`, `lint` (still finds the misspelled object), a CLI rename patch touching one file out of 21, `rusting test tests/descend.json` passing on the folder scene, and join giving the same JSON as the original. Full AGENTS.md check green. Limits: entity files keep keys sorted (`id` is not first); prefab instances are not split into their own folder, but a prefab file can itself be split.
- [x] `rusting merge`: a semantic three-way merge driver for scenes, registered through `.gitattributes`, that reports real conflicts by entity and field.
  Evidence: `scene_merge::tests` (edits to different entities, fields and `components` entries merge; an added entity is appended; a field both sides changed is a conflict that keeps ours; an entity we deleted but they changed is kept with an `entity` conflict) and `cli::hint_tests::merge_combines_scene_edits_and_names_real_conflicts` (on the puzzle template's main scene: two renames of different entities merge; two renames of one entity give `SCENE_MERGE_CONFLICT` on `name`; `rusting new` writes `.gitattributes`). A real `git merge` with `merge.rusting-scene.driver "rusting merge %O %A %B"`, where one branch rewrote the scene on one line and the other renamed another entity, merged cleanly with both names. Full AGENTS.md check green. Limits: the driver must be set in git config per clone (git does not read drivers from the repository); a conflict keeps ours and writes no conflict markers.
- [x] Scoped leases: an agent can claim scenes, prefabs, or source files through the daemon; conflicting writes are refused with the holder named.
  Evidence: `rusting lease claim|release <path> [--as NAME] [--for SECONDS]` and `rusting lease list` (also through `rusting serve`, which shares the dispatch); leases are in `.rusting/leases.json` (the folder writes its own `.gitignore`), a folder lease covers its files, and every `write_atomic` (scene and prefab saves from the CLI, daemon and editor) is refused with `LEASE_HELD` naming the holder unless `RUSTING_AGENT` is the holder. Tests: `runtime::lease::tests::a_lease_refuses_other_writers_until_released` (overlapping claim and foreign release refused; sibling files and files outside a project are free; release frees the path) and `cli::hint_tests::a_leased_scene_refuses_writes_from_other_agents` (a write to a leased scene fails with `LEASE_HELD`, then succeeds after release). Smoke run of the debug binary: `RUSTING_AGENT=bob rusting merge ...` on a scene alice leased exits 1 with the holder named; as alice it writes. Full AGENTS.md check green. Limits: rusting does not write source files, so a source lease is advisory (visible in `lease list`); leases expire (default 30 minutes) rather than being tied to a live process; a lock file left by a killed process must be deleted by hand.
- [x] Impact reports: before a change to a prefab, component, or asset, list every scene and scenario it affects.
  Evidence: `rusting impact <file-or-component> [root]`; `cli::hint_tests::impact_lists_the_scenes_code_and_scenarios_a_change_touches` (a data file named in game code gives the code line and every scenario; `camera` gives the main scene and every scenario; an unused name gives nothing); on forever-bear `impact models/bear.mesh-0-0.rmesh` lists 1876 entities of the main scene. Full AGENTS.md check green. Limits: prefabs are referenced only from code (`spawn_prefab` literals), so a prefab hit is a code hit; paths built at run time are not found; a scenario is tied to a target only through the main scene, game code, or naming it.

### Exit gate

- Two agents build separate features in one project at the same time, merge with no manual conflict resolution, and all scenarios pass.
- An agent completes a benchmark bug-fix task in a project with at least 100 scenes and 50 systems.

## Dogfood track: PLAYPLACE

Goal: build a real game from `researches/game2/research` (PLAYPLACE, the pick in its VERDICT.md) at `RustingGames/playplace` using only `rusting` commands, and fix every engine gap it exposes. The friction log is `RustingGames/playplace/FRICTION.md`.

- [x] Week 1 slice: pit room, 40,000 code-spawned GPU spheres, `ball_asleep` GPU rule feeding a `sleeping` counter, scenarios `w1_pool` (real hardware, 600 ticks) and `w1_pool_quick` (120 ticks).
  Evidence (2026-10-01, lavapipe because the real GPU is wedged): `rusting check` passes; `rusting test tests/w1_pool.json` before the fix failed at the sleeping check with the counter at 0; with a capture step the same game reported 14,146 sleeping balls at tick 120 (1.2 s per tick in software). Real-GPU frame time, the 60 FPS kill criterion, is NOT measured.
- [x] Fix: scenario `expect` on a game with code-spawned meshes failed with "mesh N has no asset path and cannot be saved". `reflected` now uses `scene_document_lenient`. Verification: the gpu-test `the_gpu_flag_opens_the_device_without_a_capture_step` and the playplace scenario above.
- [x] Scenario `"gpu": true` opens the headless Vulkan device without a capture step and fails with `gpu: no Vulkan device` when none opens. Documented in `rusting schema`, `docs/concepts.md` and tutorial 4. Answers PLAYPLACE research gap #1 (GPU physics does run in `rusting test`).
  Full check, 2026-10-01, lavapipe: fmt clean; clippy plain, `--no-default-features`, `--features gpu-tests` clean; `cargo test --workspace` exit 0, 16 result lines, 0 failed; same with `--features gpu-tests`.
- [x] A `--template empty` (no leftover `Ball`/`Box`/`Wall` names), or a clearer error when code-spawned names collide with the scene. Done in 79501b0 (`rusting new --template empty`, camera only); a name collision panics with "scene object `X` already exists; object names are unique". Evidence: unit tests `the_empty_template_has_no_named_objects_to_clash_with` and `spawn_copy_refuses_a_child_name_that_is_taken`.
- [x] A bulk spawn for scenes (emitter component or a patch grid operation), so large uniform sets need no game code. Done in 18d0738: the `rusting.spawn_grid` component copies its object onto a grid at load. Evidence: unit test `spawn_grids_copy_the_tree_onto_cells_once`.
- [x] A built-in "bodies inside this box" count for GPU classes readable from a scenario, for `escaped` and zone-mass checks. Done in de53851 (class entities with a box count GPU bodies inside it). Evidence: unit test `class_entities_count_members_and_gpu_bodies_in_a_box`.
- [ ] Week 2: noise-attraction force field as a custom GLSL solver; scenario `w2_flow`.
- [ ] Measure 40,000 spheres on real hardware (60 FPS at 1080p, RTX 3060) once the GPU driver is healthy.

## Dogfood track: 40,001

Goal: fix every engine gap that `RustingGames/forty-thousand-and-one/FRICTION.md` (F1-F29) logs.

- [x] F1, F3: `rusting new` writes `skills/rusting-game/SKILL.md`; the agent guide points at `rusting docs show sample/<name>`, not at a `samples/` folder. Verification: `tests/cli.rs` project tests.
- [x] F2: `rusting doctor --probe` opens the device and round-trips a 64-word buffer fill (15 s timeout), reporting `probe.ok` with `selected_device` or `error`. Verification: the doctor CLI test.
- [x] F4, F25, F26, F27: sound output (`play_sound`, `stop_sound`, `set_master_volume`, `sound_cue`), a scenario `audio:` entity with request counts, `sound_clip_diagnostics` in `rusting check`, one project-relative root for asset and clip paths, and the sound API in SKILL.md, the project guide and `rusting docs`. Verification: runtime and CLI unit tests.
- [x] F5: `docs/gpu-condition-shaders.md` documents the hooks, the `PhysicsState` ABI, the free `custom_values`, `emit_event`, sleep and determinism.
- [x] F6, F20: `GpuConditionShader::params` (uniform `vec4`s at binding 14, no recompile) and `events_per_body`; overflow is printed and sent as `GpuPhysicsEventsLost`. Verification: gpu-tests for condition params and event capacity.
- [x] F7, F8, F9, F12: `docs search` reports the total past its limit; `Name::as_str` and `Deref`; `GameObject::entity()`; `scene.set_active_camera(name)`; point light intensity documented in its unit. Verification: unit tests.
- [x] F10: render scale works (see Night Market F10). The post-process hook is still open: it needs the tone-map pass to read HDR from a sampled image instead of a subpass input.
- [x] F11, F13, F14: the GPU contact pass takes the largest push and velocity change per direction instead of their sum, runs 4 rounds per tick (`CONTACT_ROUNDS`), ignores contacts closer than a 2 mm margin, and dynamic bodies sleep after 20 ticks under 0.05 m/s (`angular_velocity.w`; `GpuCondition::sleeping()`). Verification: gpu-test `a_deep_gpu_ball_pit_settles_and_its_walls_hold`: 3,600 balls in a 4 x 4 m pit with sideways gravity of 8 m/s^2, 3,045 at rest and 0 escaped. The old solver gave 646 at rest and 17 escaped through wall corners.
- [x] F15, F16, F19, F22: reading a missing counter warns once, and `set_counter` / `add_to_counter` create it, so game state needs no scene entity per counter; the scenario docs give the order inside one tick; `expect` and invariants take `not_equals`. Verification: runtime and scenario unit tests.
- [x] F17, F18, F21: `preset apply --only`, EPIPE exits quietly, `scene query` returns component values as JSON. Verification: CLI tests.
- [ ] F23: 3D world-space text. Deferred; needs a text atlas in the scene renderer.
- [x] F24: `rusting determinism <root> --gpu <scenario.json>` runs the scenario in a debug and a release build and compares the GPU state hashes of every tick both runs reached (`DETERMINISM_DIVERGED` with the tick, or `DETERMINISM_NO_GPU_STATE`). Scenario reports carry `gpu_state_hashes`; a failing expect step does not stop the comparison (`scenario_passed`). Evidence (2026-10-02, RTX 3060): `rusting determinism . --gpu tests/w6_seed_a.json` on 40,001 compared 29 ticks, all equal (its pinned `night_hash` is stale after the solver change, so `scenario_passed` is false).
- [x] F28: HUD elements are placed from this frame's measured text, so a right or bottom anchor keeps its margin when a `{counter}` value grows; the offset direction is in the schema. Evidence: 40,001 `tests/w4_cctv.json` recapture shows "COUNT: 40000" ending 24 px from the right edge.
- [x] F29: the project guide says a `path` dependency follows a live engine and how to pin a `rev` or `tag`.
  Full check, 2026-10-02, RTX 3060: fmt clean; clippy plain, `--no-default-features`, `--features gpu-tests` clean; `cargo test --workspace` 0 failed; `--features gpu-tests` 573 lib tests passed, 0 failed. The grid test now counts overflow and oversized bodies once per contact round. `rusting check` on 40,001 passes.

## Dogfood track: Lantern Keeper

Goal: fix the engine gaps that `RustingGames/lantern-keeper/FRICTION.md` (F1-F26) logs.

- [ ] F1: skeletal animation and skinned meshes. Deferred (milestone-sized); SKILL.md and the project AGENTS.md now say there is none and to animate child entities from game code.
- [x] F2, F7: a walking player keeps pushing a dynamic body: `move_character` reports the `wall` it slid along and the controller raises the body's horizontal velocity along the push to the walking speed (mass is ignored). `PlayerController` carries runtime `wall` and `velocity`; `touching` includes the player's floor and wall; `scene.player(name)` returns a copy of the controller. Verification: `a_walking_player_keeps_pushing_a_crate_it_did_not_start_in` (crate past z -6 after 180 ticks, player grounded, velocity z about -4).
- [x] F3: scenario `set` reaches `/visible` and the fields of `rigid_body`, `collider`, `physics_body`, `collision_layers` and the three lights. Verification: `set_reaches_built_in_components`.
- [x] F4, F10: `rusting check` runs the debug `cargo build` that `run` and `test` reuse instead of `cargo check`, so no command compiles the engine twice. `rusting new` prints a `Next:` line saying the first build takes minutes. The cold build itself is unchanged.
- [x] F5, F23: plain `rusting test` prints each `log` step as `tick N: <value>`, pass or fail, and a failed scenario message ends with the last 20 lines of game stderr. Verification: `plain_output_lists_scenario_log_values_even_on_failure`.
- [x] F6: no new shape; a dynamic `ConvexMesh` body on the `Cylinder` mesh is the barrel. It slid instead of rolling: `mesh_manifold` picked the body with fewer vertices, so an 8-vertex floor box beat the cylinder hull and the floor's far corners became contact points. It now picks the smaller body by half-extent length. Verification: `a_barrel_with_a_convex_cylinder_collider_rolls` (fails before the fix: no spin, stuck at x 1.33).
- [x] F8: a missing counter warns once (done for 40,001). `counter_value` and `counter_complete` now take `&self` (a read-only `try_query`); the warn-once set is a process-wide static that feeds only warnings.
- [x] F9: SKILL.md puts `parent` inside `entity` of a `create` op.
- [x] F11: `PlayerController::turn_speed` (rad/s, default 0) turns the body's non-`Camera` children toward the horizontal `velocity` in a fixed-step `player_face` system after `player_move`. Verification: `a_player_with_a_turn_speed_faces_its_rig_where_it_walks` (rig yaw -90° walking +X, body yaw unchanged).
- [x] F13: `scene.reparent(name, parent)` keeps the local transform; `scene.set_light(name, color, intensity, range)` changes a point or spot light. Verification: `reparent_and_set_light_move_and_dim_a_lantern`.
- [x] F14, F19: `rusting determinism <root> --scenario <file>` replays a scenario in a debug and then a release build and compares the CPU state hash of every tick, plus GPU hashes when present (`data.divergence.state`). Scenario reports carry `state_hashes`. Each build prints a progress line to stderr. Evidence (2026-10-02): Lantern Keeper `tests/w6_replay.json` compared 520 ticks, all equal, in 66 s.
- [x] F15: `rusting run --record FILE` and `--replay FILE` expose the existing input replay (`RUSTING_REPLAY_OUT`, `RUSTING_REPLAY_PLAY`). Game-code input injection is not added; a recorded run covers the replay need.
- [x] F16: `slide` first casts down from one `bottom` above the body, so a body sunk into a walkable top by less than that is lifted onto it. Verification: `a_player_teleported_into_a_platform_stands_on_top_of_it` (fails without the cast).
- [x] F17: the scenario `log` step already exists; SKILL.md now points to it from the `inspect` note.
- [x] F18: the `counter_value` doc says counters are `i32`.
- [x] F20: `BurstEmitter` gains `rate` (particles/s, every fixed step, fractional carry in `pending`), `area` (world-aligned start box, seeded per tick from a `burst_area` stream) and `stretch` (Y scale factor). Verification: `an_emitter_with_a_rate_rains_streaks_over_its_area`.
- [x] F22: `rusting docs show api/PlayerController` is generated from the struct and field doc comments in `player.rs`; the undocumented fields got docs. Verification: `api_index_lists_documented_gameplay_methods`.
- [x] F24: scenario `tap` step presses and releases before the next tick; SKILL.md explains the `pressed` edge. Verification: the tap case in the scenario press test.
- [x] F25: a `capture` path whose first folder repeats the scenario's folder (`tests/` inside `tests/`) adds a warning with the written path.
- [x] F26: SKILL.md says bodies solve in entity order and scenarios should assert robust values.

## Dogfood track: Night Market

Goal: fix the engine gaps that `RustingGames/night-market/FRICTION.md` (F1-F16) logs.

- [x] F1: a headless egui pass (`rusting test`, `debug`, `RUSTING_HEADLESS_TICKS`) builds its `RawInput` from `RuntimeInput`: screen from the viewport, cursor, mouse button edges and key edges. Verification: `a_headless_pass_clicks_a_button_from_runtime_input` (mouse click, then Tab and Enter click the focused button).
- [x] F2, F14: new `guide/menus-and-ui` page (main menu, pause, quit, keyboard focus, saves, rebinding, menu scenarios); `GameScene::set_paused` and `paused` wrap `TimeControl`. SKILL.md and the project AGENTS.md point to it.
- [x] F3: `docs search` with no match prints "No matches for `<query>`" and the hint to use fewer words.
- [x] F4: `GameScene::cursor`, `viewport_size`, `keys_pressed` and `rebind(action, keys)`.
- [x] F5: a scenario sets the viewport to `capture_size` at tick 0. Verification: `pointer_steps_place_the_cursor_in_the_capture_sized_view`.
- [ ] F6: first build time. Unchanged; `rusting new` already prints the note.
- [x] F7: `GameScene::save_data`, `load_data`, `delete_data` in a per-game user data folder (`project::user_data_folder`, `RUSTING_USER_DATA` override; keys reject absolute paths and `..`), and `GameScene::quit` (window closes, headless run stops).
- [x] F8: scenario `restart` step and top-level `files` (copied into a per-scenario user data folder, `build/test-userdata/<scenario>/`, emptied before each run). A relaunch step is skipped: `restart` plus `files` cover save tests.
- [x] F9: `determinism --scenario` prints a verdict line with the number of ticks compared (the last 1024 the hash history keeps) and the final hash.
- [x] F10: video settings. `GameScene::set_render_scale` (0.25 to 2.0; the scene renders into an offscreen image of that size, then a linear blit stretches it over the window or capture, and UI is painted after at full resolution; skipped when the device or swapchain cannot blit), `set_vsync`, `set_max_fps`, `set_fullscreen` (borderless, current monitor) and `set_window_size`, applied by the window runner after the frame. Verification: `a_half_render_scale_still_fills_the_whole_target` (gpu-tests, lavapipe), `video_settings_write_render_settings_and_the_window_request`, `scaled_extent_rounds_clamps_and_never_reaches_zero`; a 20 s Night Market window run on an RTX 3060 at scale 0.5 with a `set_window_size` and a scale change showed no errors. Fullscreen is real-hardware only and was not run.
- [x] F11: scenario `click` step finds the topmost on-screen text with that label (from egui's output shapes) and clicks it; a miss lists the texts on screen. `rusting inspect --ui` is skipped: the failure lists the labels. Verification: `click_steps_press_egui_buttons_by_label_and_quit_ends_the_run`.
- [x] F12: a failed check names the inputs that ran on its tick. Verification: `a_failed_check_names_inputs_that_ran_on_its_tick`.
- [x] F13: gamepad input through gilrs 0.11 (owner approved; `window` feature). Every pad feeds `RuntimeInput`: `PadButton` buttons, two sticks, and stick directions as buttons (press past 0.5, release below 0.3). `InputBinding::Pad` and `Pad*` input names in `rusting.input_action`; the player controller binds the left stick, d-pad, South (jump) and LeftStick (sprint), and the right stick looks at 3 rad/s. egui menus: the d-pad or left stick focuses and moves the focus, South clicks, East is Escape. `GameScene::stick`, pad names in `keys_pressed`, scenario `left_stick` / `right_stick` steps. Verification: `pad_buttons_and_stick_directions_drive_actions`, `a_headless_pass_clicks_a_button_from_runtime_input` (d-pad then South), `pad_presses_and_stick_steps_reach_runtime_input`, `player_look_needs_captured_cursor_and_clamps_pitch` (right stick). Reading a physical pad is real-hardware only.
- [x] F15: `GameScene::counters()` lists every counter for saves. Bulk creation stays a `once` loop.
- [x] F16: the exported README.txt documents the user data folder, `RUSTING_USER_DATA`, `RUSTING_HEADLESS_TICKS` and `std::env::args()`; `lib.rs` re-exports `serde_json`. `rusting test --build <export>` is deferred.
- [x] Also: `rusting doctor --probe` prints the probe result in text output, not only in JSON.
  Full check, 2026-10-02, RTX 3060: fmt clean; clippy plain, `--no-default-features`, `--features gpu-tests` clean; `cargo test --workspace` 498 lib tests passed, 0 failed; `--features gpu-tests` 577 lib tests passed, 0 failed. All 7 Lantern Keeper scenarios pass with the engine push.

## Dogfood track: Metronome Mines

Goal: fix the engine gaps that `RustingGames/metronome-mines/FRICTION.md` (F1-F19) logs.

- [x] F1: `CLI_OUTDATED` names the newest changed engine file, how much newer it is than the CLI binary, and the command `cargo install --path <engine> --locked`.
- [x] F2, F3, F4: sounds belong to a fixed tick and start one fixed step after it (`Sound::at_tick` for a later tick), so music restarted each bar with `at_tick` stays on the beat; `BeatClock` (`tick_of`, `beat_at`, `is_beat`, `ticks_to_next`, `phase`, `beats_between`). An audio-position query is skipped: the start tick gives it. Verification: `beat_clock_lands_on_whole_ticks`, `the_offline_mix_reports_levels_pan_buses_and_scheduled_starts`.
- [x] F5: SKILL.md says game code sees `time.fixed_tick == N` on scenario tick N.
- [x] F6: `GameScene::press_tick(action)` gives the press time in fractional ticks from the window event time. Verification: `press_tick_is_the_event_time_or_the_frame_tick`.
- [x] F7: scenarios mix every sound through kira's own mixer offline (`OfflineBackend`); `audio:` reports `/level` and `/playing`, and top-level `audio_out` writes the mix as a WAV. Verification: `the_offline_mix_reports_levels_pan_buses_and_scheduled_starts`.
- [x] F8, F9: `play_sound_with(clip, Sound { pan, bus, position, .. })`, `set_sound_volume`, `set_bus_volume` with fades; positional sound pans to the camera's side and falls off as `2 / distance` past 2 m. New `guide/audio` page. Hearing it on speakers is real-hardware only and was not done.
- [x] F10, F11: the HUD draws in PostUpdate, so a capture shows the tick's own text, and it no longer fades in; `GameScene::set_hud`. Verification: `hud_text_shows_the_counters_game_code_set_on_the_same_tick`.
- [x] F12: the GPU hybrid solver damps spin by `5 * dt` per tick while a body touches something, so a spun body sleeps. Verification: `gpu_bodies_rest_on_cpu_colliders_unless_filtered_or_no_collision` (gpu-tests) now spins the box and asserts it sleeps; it fails without the damping.
- [x] F13: `GameScene::load_text`; `rusting validate` reports `CODE_MISSING_ASSET` for a literal `load_text` or `play_sound*` path that is not a file under `assets/`. Verification: `literal_asset_paths_in_game_code_must_be_files`.
- [x] F14: `GpuBodySettings::sync` and `GameScene::gpu_state(name)`; scenarios read the GPU pose under `/gpu_state`. Verification: `gpu_bodies_report_the_gpu_pose_under_gpu_state`.
- [x] F15: patch `upsert` operation replaces the entity with the same `id` or `name` and keeps its id. Verification: `a_patch_of_upserts_runs_twice_and_keeps_ids`.
- [x] F16: scenario reports copy state hashes after every tick, so `determinism --scenario` compares all ticks. Verification: `hashes_are_kept_past_the_world_history`.
- [x] F17: an unknown enum variant lists the valid ones. Verification: the reflect enum test.
- [x] F18: `GpuCondition`, `GpuFieldCondition` and `GpuStateField` are in the API docs.
- [x] F19: `rusting asset import` reports `channels` for a WAV. Verification: the WAV import test asserts `Some(1)`.
  Full check, 2026-10-03, RTX 3060: fmt clean; clippy plain, `--no-default-features`, `--features gpu-tests` clean; `cargo test --workspace` 515 lib tests passed, 0 failed; `--features gpu-tests` 595 lib tests passed, 0 failed. `rusting validate` is clean for 40,001, Lantern Keeper, Metronome Mines and Night Market.

## Dogfood track: Split Signal

Goal: fix the engine gaps that `RustingGames/split-signal/FRICTION.md` (F1-F22) logs.

- [x] F1: `Camera.viewport` (`[x, y, width, height]` fractions); viewport cameras draw over the full-window camera, lower priority first; `GameScene::set_camera(name, active, viewport)`. Verification: `viewport_cameras_split_the_frame`, `viewport_cameras_draw_over_the_full_window_one_in_priority_order` (gpu-tests).
- [x] F2, F3: `PlayerController.mouse_look` (false leaves the mouse free) and `camera_offset` (over-the-shoulder). Verification: `player_look_needs_captured_cursor_and_clamps_pitch`.
- [x] F4: patch errors for a too-short array name the operation and the field path. Verification: `a_bad_field_in_a_created_entity_is_named_by_its_path`.
- [x] F5, F6: patch `delete` takes `missing_ok`; `upsert` takes `merge: true` to keep fields left out. Verification: `delete_can_skip_a_missing_entity_and_upsert_can_merge`.
- [x] F9: render scale is set from game code (video settings) and is no longer listed as a scene resource. Verification: `video_settings_write_render_settings_and_the_window_request`, `a_half_render_scale_still_fills_the_whole_target` (gpu-tests).
- [x] F10, F11: `expect_screen` and `capture` take `camera`; `capture` takes `hud: false` and `golden` with `tolerance`; `rusting test --update-golden`; new `expect_pixels` step (region mean and luma deviation bounds); `rusting capture --no-hud`. Verification: `pixel_checks_goldens_and_named_cameras` (gpu-tests), `capture_steps_take_a_path_or_options`, `region_stats_and_golden_differences`, `screen_checks_see_a_mesh_in_front_of_a_camera_and_behind_a_wall`.
- [x] F12: the perf report has `gpu_ms` and, with viewport cameras, `render.cameras` per camera, without a capture step. Verification: `the_gpu_flag_opens_the_device_without_a_capture_step`, `a_render_budget_reports_render_counters_without_a_capture`.
- [x] F13: does not reproduce. `tools/perf_feeds.py` on 2026-10-03, RTX 3060: 2.37 ms per feed in debug, 2.21 ms with `--release`. No change.
- [x] F14, F15: rotation order, `basis(name)` and `RayHit` fields are documented; `raycast_skipping` passes through chosen classes. Verification: `basis_forward_follows_the_rotation_order`.
- [x] F16: a scenario `set` creates a missing counter, like game code. Verification: `counter_shorthand_sets_checks_and_logs_a_counter_by_name`.
- [x] F17: an unknown expect path suggests the nearest real paths. Verification: `a_missing_path_suggests_the_nearest_real_ones`.
- [x] F18: `rusting docs show` prints whole pages. Verification: `docs_lists_searches_shows_and_briefs_within_a_token_budget`.
- [x] F19: new `guide/cameras` page (viewports, split screen, HUD per camera, camera tests, limits).
- [x] F20: `rusting.hud` element `camera` anchors to that camera's viewport and hides while it is inactive. Verification: `hud_elements_follow_their_camera_viewport`.
- [x] F22: the game AGENTS.md sample uses `time.delta_seconds()`.
- [x] F7 (render to texture), F8 (per-camera post effects), F21 (reuse builds across commands). Done later: F7 in 4688062 (`rusting.camera_screen`), F8 in fb4f83d (grading, size and exposure per feed), F21 re-tested 2026-10-03 (determinism 4.9 s, check 2.5 s); split-signal FRICTION.md marks all three fixed.
  Bug found on the way: `skip_serializing_if` on `SceneCamera.viewport` broke cooked (bincode) scenes; removed, and `source_and_compiled_scene_decode_to_same_document` now cooks a viewport camera.
  Full check, 2026-10-03, RTX 3060: fmt clean; clippy plain, `--no-default-features`, `--features gpu-tests` clean; `cargo test --workspace` 524 lib tests passed, 0 failed; `--features gpu-tests` 606 lib tests passed, 0 failed.

## Dogfood track: update pass

Goal: fix the engine friction the six-game update pass (RustingGames, 2026-10-03) logs.

- [x] Conditions combine with `&` and `|` (`GpuCondition`). Verification: `docs` and `gpu_condition` lib tests.
- [x] Game code: `name_of`, `has_class`, `binding`, `set_pixelated`. Verification: the `in_class("ember")` test in `project_runner`.
- [x] `RenderSettings.pixelated` upscales a low render scale with nearest filtering. Verification: `a_half_render_scale_still_fills_the_whole_target` checks 4x4 blocks at 0.25 (gpu-tests).
- [x] Scenario `click` takes `{text | starts_with, index}`; `index` counts in reading order. Verification: `find_text_where_picks_by_prefix_and_reading_order`.
- [x] New `expect_file` step for save data, evaluated even after the game quits. Verification: the quit test in `scenario`.
- [x] `press`/`tap` take a fractional `at`, recorded as the press tick. Verification: `a_press_at_a_fraction_records_that_press_tick`.
- [x] The `audio:` entity reports `peak` and `clipped`. Verification: the audio mix test asserts both.
- [x] `log` on a missing entity or path says so and suggests near paths; a missing `/gpu_state` names the sync modes that make it.
- [x] Patches accept RGB colors and add alpha 1. Verification: `scene_patch` tests.
- [x] `rusting asset import --dry-run` shows WAV channels. Verification: `asset_import` tests.
- [x] `RUSTING_KEEP_GOING`, and `rusting determinism --scenario` keeps going past failed checks (forty F30).
- [x] `--record` with `--ticks`, `--scenario` or `--replay`, and `--replay` with headless runs, fail with `CLI_USAGE` before the build; `--record` honours `--timeout`. Verification: `record_and_replay_mistakes_fail_before_the_build`.
- [x] `rusting test` and plain output print `log:` lines and step warnings for every scenario.
- [x] `AGENTS_OUTDATED` warns when a project's engine-written AGENTS.md differs from this engine's; `rusting fix` refreshes it and keeps `AGENTS.md.old` (metronome F24, split F28). Verification: `an_old_engine_agents_md_is_flagged_and_refreshed`.
- [x] split F23 (w6_perf slower): does not reproduce. A/B raycast bench, 3600 rays, best of runs: current 4.874/4.904/4.922 ms, HEAD 4.888/4.861/4.882 ms. Slowdown was machine load.
- [ ] Deferred: GPU vertex skinning (CPU skinning is done, item 7 of the asset plan), bulk GPU spawn, GPU body control and waking piles. Render to texture, per-camera post effects and build caching are done (split-signal F7, F8, F21). The counters design (night F15) is answered by `counter_or` (3d894b3) and `scene.counters()`. `click` `right_of` is dropped: night-market F18 was re-tested fixed with `index` and `starts_with`, and no game asks for it. `expect_pixels` `differs_from` was done as item 85 (6e4b0bb). Kill plane done 2026-10-10: `PhysicsSettings::kill_y` (set by `GameScene::set_kill_y`, off by default) despawns loose dynamic CPU bodies below that height with their children and sends `FellOut`, read by `GameScene::fell_out` (lantern-keeper F6). Verification: `the_kill_plane_despawns_fallen_bodies_and_sends_fell_out`. Per-kind rebinding done 2026-10-10: `GameScene::rebind_device` replaces only the inputs on the new inputs' device (keyboard and mouse, or gamepad) and keeps the rest in place, and the `{binding:action}` HUD button uses it, so a new key keeps the pad button (night F17). Verification: `spawn_copy_clones_the_template_tree_and_in_class_lists_it` and `clicking_a_binding_button_rebinds_to_the_next_key`.
  Full check, 2026-10-03, RTX 3060: fmt clean; clippy plain, `--no-default-features`, `--features gpu-tests` clean; `cargo test --workspace` 528 lib tests passed, 0 failed; `--features gpu-tests` 610 lib tests passed, 0 failed (one earlier GPU run failed in the lib tests while the machine was loaded; a rerun and the full run passed).

## Sundering track

Milestones 24-29 build *Sundering* on top of the engine. They consume the determinism, fracture, GPU-scale, navigation, and networking milestones and must not fork parallel versions of those systems; gaps found here become engine items in the owning milestone.

## Milestone 24: Competitive networked authority

Goal: run the authoritative simulation on a server and keep many clients consistent with it under real network conditions.

Depends on: Milestones 8 and 19. Milestone 19 provides the general transport, protocol, RPC, replication, and rollback machinery; this milestone adds the competitive-authority, anti-cheat, and destruction-scale requirements Sundering needs on top of it.

The networking model depends on the Milestone 8 result. Bit-identical determinism permits lockstep and rollback. Without it, the server simulates and clients render, predicting only their own hero.

### Server

- [x] Headless server binary running the fixed-tick simulation with no renderer or window. Evidence 2026-10-10: the game binary itself is the server binary. With `RUSTING_SERVER=1` (`rusting run --server`), `run_project` runs `run_project_server`: no window, Vulkan device or renderer, one fixed tick per fixed step of real time, `net::DedicatedServer` present, until game code quits (Milestone 19 "Client-server and listen-server topologies" item). Evidence: unit test `project_runner::tests::a_dedicated_server_ticks_in_real_time_until_the_game_quits`. GPU-class bodies stay still without a renderer, so server-side simulation uses CPU bodies.
- [x] Tick-scheduled loop with drift correction, overload detection, and reported tick-budget usage. Evidence 2026-10-10: `run_project_server` sleeps to an absolute deadline advanced by one fixed step per tick, so wake-up jitter does not drift; a tick that starts past its deadline counts in `late_ticks`, and after a stall of more than 10 steps the lost ticks are skipped, counted in `dropped_ticks`, and reported on stderr ("server overloaded: skipped N ticks after a stall"). `net::DedicatedServer` carries `ticks`, `last_budget` (the last tick's work time over the fixed step) and `budget` (smoothed over about 20 ticks), so game code can report load to operations tooling. Docs: `docs/networking.md` "Listen servers and dedicated servers". Evidence: unit test `project_runner::tests::a_dedicated_server_ticks_in_real_time_until_the_game_quits` (tick count, a tick overrunning by 3 steps reports `last_budget` above 2.5 and a late tick, nothing dropped, paced run). Full check: fmt, clippy (default, no-default-features, gpu-tests), rustdoc, tests (lib 831, 932 with gpu-tests) green.
- [x] Client lifecycle: join, ready, in-match, drop, reconnect. Evidence 2026-10-10: `net::lobby`. The host's `Lobby` gives each client that sends `lobby::hello` a `PlayerId` that outlives its connection and a random reconnect token (`lobby::welcome_from` on the client); stages `Joined`, `Ready` (`lobby::ready`) and `InMatch` (`start_match` moves every connected ready player in); a disconnect drops the player and keeps its place for a grace time (`expire` turns it into `Left`); a hello with the player id and token from any new connection rejoins it at its old stage with a fresh token. Only dropped players can be rejoined, so a sniffed token cannot steal a connected one; forged tokens, second hellos and unknown messages are errors. Docs: `docs/networking.md` "Players, ready checks and reconnects". Evidence: unit test `net::lobby::tests::players_join_ready_play_drop_and_rejoin` (join order, ready, match start, three refusals, drop, rejoin on a new peer keeps `InMatch` and rotates the token, grace expiry). Full check: fmt, clippy (default, no-default-features, gpu-tests), rustdoc, tests (lib 832, 933 with gpu-tests) green.
- [x] Server-side input validation and rate limiting. The server never trusts a client-reported position, velocity, or terrain state. Evidence 2026-10-10: `net::limit::RateLimit`, a token bucket per peer (`allow(peer, now)`, `forget(peer)`), for dropping client messages over a rate. The authority model is in place: `net::rpc` checks callers on the host, `net::replicate` only takes state from the host, and `net::predict` lets clients send inputs instead of positions. Input validation itself is game code (the engine cannot know a game's ranges); `docs/networking.md` "Trusting nothing a client sends" gives the rules (inputs only, finite and in range, ticks near the host's, drop on failure) and the rate-limit loop. Evidence: unit test `net::limit::tests::each_peer_gets_a_burst_then_its_rate` (burst, separate buckets, refill rate, refill cap, forget). Left open: kicking a client (the relay protocol has no kick message yet). Full check: fmt, clippy (default, no-default-features, gpu-tests), rustdoc, tests (lib 833, 934 with gpu-tests) green.

### Transport and protocol

- [x] Reuse the Milestone 19 transport and versioned protocol; do not fork a second networking stack. Evidence 2026-10-10: the server work so far (dedicated server, `net::lobby`, `net::limit`, `net::predict`) sits on `NetSession` and its `PROTOCOL_VERSION` handshake; lobby, RPC and replication messages share the one session, told apart by a 4-byte prefix (`\0lob`, `\0rpc`, `\0rep`). Keep later Milestone 24 work on the same session.
- [x] Clock synchronization and per-client tick-offset estimation. Evidence 2026-10-10: `net::clock`. `ClockSync` (client) pings the host with its send time; `clock::answer` (host) echoes it with the host tick; on the pong the client takes half the round trip as the one-way delay and keeps the last 8 samples, trusting the one with the shortest round trip. It gives `host_tick(local_tick)`, `round_trip()` and `input_tick(local_tick, margin)`, the tick to stamp an input with so it arrives `margin` ticks early. `TickOffsets` (host) smooths, per peer, how many ticks early each input arrives (`lead`). Docs: `docs/networking.md` "Agreeing on the tick". Evidence: unit test `net::clock::tests::a_client_estimates_the_host_tick_and_input_lead` (estimate over loopback, input tick, a slower sample ignored, foreign messages pass, lead smoothing, forget). Full check: fmt, clippy (default, no-default-features, gpu-tests), rustdoc, tests (lib 834, 935 with gpu-tests) green.
- [x] Input buffering with configurable delay and jitter absorption. Evidence 2026-10-10: `net::input::InputBuffer<I>` on the host stores each peer's inputs by tick; `take()` returns every peer's input for the next tick in peer order and advances, repeating the last input (`fresh: false`) for a late or lost one. The delay is the margin a client passes to `ClockSync::input_tick`; `buffered(peer)` reports the cushion and `late(peer)` the inputs that missed their tick. Inputs for taken ticks or more than `MAX_AHEAD` (120) ticks out are refused, bounding memory. Docs: `docs/networking.md` "Agreeing on the tick". Evidence: unit test `net::input::tests::early_inputs_wait_and_missing_ones_repeat_the_last` (out-of-order early inputs, repeat on a gap, late and far-ahead refusals, peers with no input left out, forget). Full check: fmt, clippy (default, no-default-features, gpu-tests), rustdoc, tests (lib 835, 936 with gpu-tests) green.

### State replication

- [x] Snapshot encoding with delta compression against the last client-acknowledged snapshot.
  Evidence 2026-10-10 (lane tools): `Replication::snapshot` numbers each
  capture and keeps the last `SNAPSHOT_HISTORY` (64); `snapshot_for(peer)`
  encodes the latest as the changes since that peer's last ack (whole when
  none is kept); `Replica::ack`/`Replication::accept` carry acks. The
  client keeps received snapshots, rebuilds each from its base, drops late
  ones and refuses unknown bases, with the replica's existing checks.
  `net::replicate::tests::snapshots_encode_against_the_last_acknowledged_one`
  loses snapshot 2 and still gets its change in 3, smaller than a whole
  snapshot. `docs/networking.md` "Snapshots over unreliable messages".
- [x] Quantize replicated values with documented precision per field.
  Evidence 2026-10-10 (lane tools): `Replication::quantize(pointer, step)`
  now refuses non-positive or non-finite steps (a zero step made values
  null), replaces a path's earlier step, and documents that clients are
  within `step / 2`; `precision()` lists every path and step.
  `net::replicate::tests::quantized_fields_stay_within_half_a_step`.
  `docs/networking.md` has a per-field precision table (position 5 mm,
  rotation 0.0005 rad, scale 0.005, health exact).
- [ ] Interest management: replicate only what a client's team can currently see, using the vision system from Milestone 27.
- [x] Per-client bandwidth budget with measured usage and an enforced cap.
  Evidence 2026-10-10 (lane tools): `net::limit::Bandwidth::new(bytes_per_second,
  burst)` is a byte token bucket per client; `send_unreliable` sends only
  within budget (`Ok(false)` otherwise, error for a message over the
  burst) and `usage(peer)` reports bytes and messages sent, messages
  dropped and bytes in the last whole second.
  `net::limit::tests::bandwidth_caps_and_measures_each_client` checks the
  burst, per-client budgets, refill, the counters and the session's own
  byte count. `docs/networking.md` "Bandwidth per client".
- [ ] Replicate terrain modification events, not individual debris transforms. Clients re-simulate debris locally from the same events. This is the only approach that keeps bandwidth bounded during heavy destruction, and it depends on Milestone 8.

### Client

- [x] Interpolate remote entities with a documented, configurable interpolation delay.
  Evidence 2026-10-10 (lane tools): `net::interpolate::Interpolation::new(delay)`
  records each `NetTransform` an `interpolated()` replica writes, with
  its update time, and `present` sets `Transform` to the state `delay`
  ago, blending positions and scales and turning rotations the short
  way; `set_delay` changes it. `net::interpolate::tests::remote_objects_show_where_they_were_one_delay_ago`
  checks hold-before-first, halfway blend, a rotation across pi and
  hold-at-newest. `docs/networking.md` "Smoothing remote objects".
- [x] Predict only the local hero. Never predict terrain destruction results unless determinism is proven.
  Evidence 2026-10-10 (lane tools): `net::predict::Predicted` marks the
  hero and `predict_only(world, hero)` keeps it the only one;
  `Interpolation::record` skips it, so host updates land in its
  `NetTransform` and the game draws its prediction.
  `net::interpolate::tests::the_predicted_hero_is_not_interpolated`.
  Nothing predicts terrain or other host-simulated results; the rule is in
  the `Predicted` docs and `docs/networking.md` "Predicting the player's
  own object".
- [x] Reconcile prediction error with presentation-only smoothing that never feeds back into simulation state.
  Evidence 2026-10-10 (lane tools): `net::predict::Smoothing::new(half_life,
  snap)` keeps only a drawing offset: `correct(shown, corrected)` records
  a reconcile's jump, `shown(simulated, elapsed)` returns the drawn
  position with the offset halving each half-life, and corrections longer
  than `snap` show at once. It takes the simulated state by value and
  never returns into it.
  `net::predict::tests::smoothing_fades_a_correction_without_touching_the_simulation`.
  `docs/networking.md` "Predicting the player's own object".
- [x] Detect desync by periodic state-hash comparison, report it, and recover through full resynchronization.
  Evidence 2026-10-10 (lane tools): `Replication::checksum` hashes (FNV-1a
  of the JSON) the latest snapshot or sent delta state;
  `Replica::verify` hashes the client's own replicated objects the same
  way and returns `Check::{Match, Desync, Stale}`; `Replica::resync` asks
  the host, whose `accept` returns `FromClient::Resync` and makes the next
  snapshot whole. Whole snapshots and `full()` now replace the client's
  replicated objects (diffed against its world, so stray objects are
  despawned). `net::replicate::tests::a_desync_is_detected_and_repaired_by_a_resync`
  breaks a client in delta and snapshot mode and repairs it.
  `docs/networking.md` "Detecting and repairing a desync".
- [x] Support spectator clients that receive state and send no input.
  Evidence 2026-10-10 (lane tools): `net::lobby::spectate` joins without a
  player; the host's `Lobby` reports `Spectating`/`SpectatorLeft`,
  refuses any other lobby message from a spectator, and
  `accepts_input(peer)` is true only for players in the match.
  `net::lobby::tests::spectators_watch_without_a_player_or_input` checks a
  spectator cannot ready, hello or send input, still receives broadcasts,
  and leaves cleanly. `docs/networking.md` "Spectators".

### Exit gate

- Ten clients complete a full match against a headless server at 200 ms round-trip latency with 2% packet loss, without desync.
- Per-client bandwidth stays within budget with the terrain fully destroyed.
- A client disconnects mid-match, reconnects, and resynchronizes correctly.
- Server tick-budget usage and desync events are observable in operations tooling.

## Milestone 25: Destructible terrain and Scar

Goal: make terrain a first-class simulated, modifiable, and permanently scarred entity rather than authored static geometry.

Depends on: Milestones 8, 11 (fracture, debris, GPU scale), and 13 (terrain rendering).

### Terrain representation

- [ ] Chunked volumetric terrain with an authoring format, streaming, and a documented chunk size.
- [ ] GPU surface extraction with per-chunk LOD and crack-free transitions between levels.
- [ ] Derive terrain collision from the same volume data as rendering. One source of truth, never two.
- [ ] Editor tools to author, sculpt, and paint terrain volumes.
- [ ] Report chunk count, resident memory, extraction time, and streaming state in the profiler.

### Fracture and debris

- [ ] Promote static terrain chunks to dynamic GPU rigid bodies when destroyed.
- [ ] Use deterministic fracture patterns driven by the seeded tick RNG.
- [ ] Enforce a debris budget with a deterministic, documented policy at the cap. Never drop bodies arbitrarily.
- [ ] Settle and re-freeze resting debris back into static terrain, restoring collision and navigation.
- [ ] Merge small settled debris into larger static aggregates so long-running matches stay bounded.

### Structural load

- [ ] Build a support graph over connected terrain chunks.
- [ ] Solve support incrementally on the GPU when the graph changes, within a bounded per-tick cost.
- [ ] Collapse unsupported spans under their own mass.
- [ ] Expose per-chunk stress to gameplay and to debug visualization.

### Scar

- [ ] Make settled debris permanent by default, with an explicit per-region regeneration policy.
- [ ] Ensure settled debris affects collision, navigation, and vision, not only rendering.
- [ ] Persist terrain state in save, replay, and network resynchronization formats as modification history where practical rather than full volumes.

### Terrain commands

- [ ] Extend the CPU-to-GPU command bridge with terrain modification: carve, fracture, displace, freeze.
- [ ] Apply terrain commands at deterministic points in the tick.
- [ ] Account modification cost against a per-team mass-budget resource.

### Exit gate

- A cliff collapses onto a 10,000-body scene deterministically, producing identical hashes on two GPU vendors.
- Settled debris becomes walkable, occluding, static terrain.
- Frame time and memory stay within budget after a one-hour continuous-destruction soak test.
- Terrain state round-trips correctly through save, replay, and network resynchronization.

## Milestone 26: Dynamic navigation and crowds

Goal: let thousands of agents move sensibly over geometry that changes every tick.

Depends on: Milestones 18 (engine navigation) and 25. The engine navigation handles incremental re-baking of ordinary scenes; this milestone handles terrain that changes every tick at crowd scale.

### Navigation representation

- [ ] GPU navigation volume or flow field derived from live terrain collision data.
- [ ] Incremental rebuild limited to changed regions, with a bounded and enforced per-tick cost.
- [ ] Deterministic rebuild results, independent of how many chunks changed in a single tick.
- [ ] Reachability queries answering whether a team can reach a given point at all.
- [ ] Report navigation build time, dirty-region count, and budget overruns in the profiler.

### Agents

- [ ] GPU agent steering with local avoidance at thousands of units.
- [ ] Deterministic agent update order and avoidance resolution.
- [ ] Agents fall, are buried, and are displaced by debris rather than ignoring it.
- [ ] Make path failure an observable gameplay state, never a silent stall.
- [ ] Keep a CPU pathing path for the small number of agents that need immediate answers.

### Exit gate

- 1,000 agents cross a map while it is actively being destroyed, without stalling or tunnelling.
- Navigation rebuild stays within its per-tick budget during a full cliff collapse.
- Agent behaviour is bit-identical across runs and across vendors.
- Blocking every route is detected and reported rather than producing stuck agents.

## Milestone 27: Competitive gameplay framework

Goal: provide the game-specific systems Sundering needs, built entirely on deterministic simulation.

### Match and teams

- [ ] Team membership, match phases, and authoritative match state owned by the server.
- [ ] Objective and structure entities with destruction-aware state.
- [ ] Resource and mass-budget accounting per team.
- [ ] Match start, end, surrender, and result reporting.

### Abilities as forces

- [ ] Express ability definitions as deterministic physical effects: impulses, sustained forces, tethers, mass changes, area displacement.
- [ ] Cooldowns, costs, cast time, interrupts, and targeting resolved on the fixed tick.
- [ ] Area queries against live terrain and bodies, executed deterministically.
- [ ] Require every ability effect to be expressible as commands through the physics bridge.
- [ ] Expose telegraph data to presentation without letting presentation affect simulation.

### Dynamic vision

- [ ] Per-team visibility computed on the GPU from actual terrain geometry, not from authored vision blockers.
- [ ] Recompute visibility when terrain changes, within a bounded per-tick budget.
- [ ] Feed visibility into network interest management from Milestone 24.
- [ ] Derive presentation-side fog rendering from the same data.
- [ ] Keep visibility deterministic and identical between server and every client.

### Bots

- [ ] Bot controllers that use the same input interface as human players.
- [ ] Deterministic bot decisions driven by the seeded tick RNG.
- [ ] Difficulty levels with a documented behaviour set.
- [ ] Bots handle a reshaped map, including blocked routes and newly opened ones.

### Exit gate

- A match can be played to completion against bots with no human opponents.
- Vision is computed from destroyed geometry and matches exactly between server and clients.
- Every ability is expressible as a deterministic physical effect and replays identically.

## Milestone 28: Balance and operations tooling

Goal: make a game whose map differs every match tunable with evidence rather than intuition.

### Headless experimentation

- [ ] Run matches headless, faster than real time.
- [ ] Batch runner executing many matches across varied seeds, configurations, and bot profiles.
- [ ] Deterministic seeding so any interesting match is exactly reproducible from its seed.

### Telemetry

- [ ] Structured per-match telemetry: terrain modified, routes opened and closed, fight locations, resource use, outcome.
- [ ] Aggregate reporting across batch runs.
- [ ] Automatic detection of degenerate states: total lockout, unreachable objectives, runaway debris count, stalled navigation.

### Replay analysis

- [ ] Replay browser with seeking, speed control, and a free camera.
- [ ] Terrain-state timeline showing how the map evolved through the match.
- [ ] Export a match's terrain modification history for offline analysis.

### Server operations

- [ ] Match hosting, lifecycle management, and health reporting.
- [ ] Crash and desync capture producing reproducible artifacts.
- [ ] Per-map performance baselines with regression rejection.

### Exit gate

- One thousand headless bot matches run unattended and produce an aggregate balance report.
- Any reported degenerate state reproduces exactly from its recorded seed.
- A match replay can be inspected tick by tick alongside its terrain timeline.

## Milestone 29: Sundering vertical slice

Goal: prove the whole stack with the smallest piece of the real game. Each slice answers exactly one question and adds nothing beyond it.

### Slice 1: pathing survives destruction

- [ ] One lane, one tower, one destructible cliff.
- [ ] Two heroes, one able to destroy terrain.
- [ ] Creeps pathing from spawn to tower.
- [ ] Acceptance: creeps route correctly after the cliff collapses, deterministically, on two GPU vendors.

### Slice 2: Scar is legible

- [ ] Debris persists and blocks both vision and movement.
- [ ] Per-team dynamic vision active.
- [ ] Acceptance: a spectator can read the map state correctly after sustained destruction.

### Slice 3: structural constraints hold

- [ ] Structural load, mass budget, and debris settling all active.
- [ ] Two players attempt to wall themselves in, and to dig directly to the enemy base.
- [ ] Acceptance: both degenerate strategies fail for physical reasons, not through rule-based prohibitions.

### Slice 4: networked match

- [ ] 3v3 against a mix of bots and humans on a headless server.
- [ ] Full replay recorded and reproduced.
- [ ] Acceptance: a complete match at 200 ms latency with no desync.

### Performance target

- [ ] Define the Sundering benchmark: fixed map, fixed input replay, scripted destruction sequence.
- [ ] Target 1920x1080 at 60 FPS on mid-range hardware with destruction active.
- [ ] Record simulation tick time, navigation rebuild time, debris count, per-client bandwidth, and input latency.
- [ ] Store baselines and reject regressions.

### Exit gate

- A 3v3 match is playable from a clean checkout on Linux, with bots filling empty slots.
- The match replays identically from its recorded input stream.
- Performance baselines are recorded and enforced in CI.

## Engine expansion track

Milestones 30-35 cover areas a general-purpose engine needs that the Godot parity map does not own. They are parallel tracks after Milestone 9, subject to the dependencies at the top of each. Every feature here follows the LLM-native design rules: data-driven components with schema entries, a Rust API in the index, and scenario-testable behaviour. Items that need a new dependency say so and wait for the owner's approval.

## Milestone 30: Gameplay framework

Goal: the systems almost every game rebuilds are built in, inspectable, and deterministic.

Depends on: Milestones 8 and 9.

- [x] Save games: versioned save files for selected components and resources, save slots, autosave, and migration of old saves across game versions, with a round-trip test per registered component.
  - [x] Counter saves: `GameScene::save_counters(key, version, names)` writes the named counters and a save version as JSON to a slot key, and `load_counters(key)` sets them and returns the version, so a newer game migrates by reading old counters. `save_data` now writes atomically, so a crash keeps the old save. Night Market hand-writes this today.
    - Verified (2026-10-09): `counters_round_trip_through_a_versioned_save` round-trips two counters and an unborn one at version 2, leaves unsaved counters out, and rejects a file with a non-integer value or a non-JSON file without setting anything.
  - [x] Save slots and autosave. `GameScene::saved_keys(folder)` lists the saves in a user data folder, sorted, for a load menu; an autosave is a cooldown, as `save_counters` documents. Verified by `saved_keys_list_the_files_of_a_save_folder_in_order` (files only, byte order, an empty list for a missing folder and a key with `..`).
  - [x] Saves of selected registered components and object states, with a round-trip test per registered component. `GameScene::save_objects(key, version, objects, components)` and `load_objects(key)`; each value keeps its component version, so old saves run the component migrations. Verified by `every_registered_component_round_trips_through_an_object_save`, which adds every registered component to one object and compares them after a save and load.
- [x] State machines as data: states, transitions, guards on counters and events, and enter and exit actions, editable in the editor and checked by scenarios.
  - [x] Object states: the registered component `rusting.state` (`state`, `since_tick`), set by `GameScene::set_state(name, state)`, which returns whether it changed so enter actions sit in an `if`, and read with `state` and `state_seconds`. It is in snapshots, the state hash and the editor's component help.
    - Verified (2026-10-09): `set_state_records_the_entry_tick_and_reports_changes` (entry tick, repeated set keeps it, missing object, 0.5 s after 30 ticks); `scenarios_expect_an_object_state` passes an expect on `/components/rusting.state/state`.
  - [x] Transitions and counter guards as data in the scene, run by the engine, with an editor view. `rusting.state` gained `transitions: [{from, to, after_seconds, counter, at_least}]`; `run_state_machines` in `FixedUpdate` takes the first applying edge each tick, and the reflected fields show in the inspector. `set_state` keeps the transitions. Verified by `state_machines_follow_timed_and_counter_guarded_transitions` (timed edge flips on tick 30, a counter guard from any state, no re-entry of the current state).
  - [x] Transition actions as data. A transition's `then_counter` and `then_add` add to a counter (created when missing) when it is taken; guards read the counters from before the tick's transitions and additions apply after all machines, so object order does not matter. Verified by `state_machines_follow_timed_and_counter_guarded_transitions` (the chase edge adds 2 to a new `chases` counter once).
  - [x] Input-action guards on transitions. A transition's `held` names an input action that must be held for it to be taken (empty for none). Verified by `state_transitions_can_wait_for_a_held_input_action` (a door stays shut for 3 ticks and opens on the tick Space, bound to `player.jump`, is held).
  - [x] Contact guards on transitions. A transition's `touching` names an object the machine's object must touch in the last physics step (empty for none). Verified by `state_transitions_can_wait_for_a_contact` (a plate touching a `Wall` stays up for 3 ticks and goes down once a `Crate` touches it).
  - [x] Checked by scenarios. The data-only recipe `alarm` adds a sensor `Alarm Plate` whose transitions alarm it when the `Player` touches it (adding to `alarms`) and calm it 2 s later; its scenario expects `/components/rusting.state/state` and the counter. Counters a transition creates now get a name and `SceneId`, as `GameScene::set_counter` gives them, so scenarios and saves find them. Verified by `alarm_recipe_applies_to_the_player_templates_and_its_scenario_passes` (all three player templates).
  - [x] Enter and exit actions on states. `rusting.state` gained `actions: [{state, exit, counter, add}]`, run by engine transitions (exit actions, then enter actions, after the transition's own `then_add`) and by `GameScene::set_state`. Verified by `state_transitions_run_exit_then_enter_actions` (11 on entering on, 100 more on leaving it 60 ticks later) and `set_state_runs_the_enter_and_exit_actions` (a chasers count up on entering chase and down on leaving it, nothing on setting the same state).
- [ ] Behaviour trees and utility AI as data assets, with a deterministic tick order and a debugger view of the active branch.
- [x] Health, damage, teams, and status effects as optional registered components, built on the typed event bridge. Health and teams are the registered `rusting.health`; statuses are tick-counted counters instead of a component, so `save_counters` and snapshots keep them with no new type.
  - [x] Health and teams: the registered component `rusting.health` (`value`, `max`, `team`); `GameScene::damage(name, amount)` returns what is left, clamped to 0 and healing (negative amounts) to `max`; `health` reads it and `same_team` tells allies apart. It is in snapshots, the state hash and the editor's component help.
    - Verified (2026-10-09): `damage_clamps_heals_to_max_and_teams_mark_allies` (2 damage leaves 1, heal stops at 3, overkill stops at 0, `None` for no health or a missing object, teams only match when non-empty).
  - [x] Damage events on the typed signal bridge. `GameScene::damage` sends a `Damaged {entity, amount, health}` entity event, so a handler registered as `In<Signal<Damaged>>` and connected to the object runs on each hit or heal. Verified by `damage_sends_a_damaged_signal_to_connected_handlers` (connected Orc logs a hit and a clamped heal; unconnected Rock is silent).
  - [x] Status effects with durations, counted in fixed ticks and kept by snapshots and saves. `GameScene::add_status(object, effect, seconds)` (reapplying refreshes, never shortens), `has_status`, `status_left` and `clear_status`; each status is a cooldown in the counter `"<object>/<effect>"`. Verified by `statuses_last_their_duration_and_reapplying_never_shortens` (active on the last tick, gone on the end tick, cleared early).
- [ ] Inventory, items, and loot tables as data resources with seeded rolls.
  - [x] Seeded loot rolls and item spending. `GameScene::roll_loot(table, stream)` picks an `(item, weight)` entry with the run's seeded `random`; `spend(name, amount)` takes from an item or money counter only when it holds enough. Item counts are counters, so `save_counters` saves an inventory. Verified by `loot_rolls_follow_weights_and_spend_needs_enough` (weight boundaries, zero weights never roll, same stream repeats, spend refuses an overdraft).
  - [x] Loot tables as data the editor can edit, read by game code by name. The registered component `rusting.loot_table` (`entries: [{item, weight}]`) sits on a named object and shows in the inspector; `GameScene::roll_loot_table(name, stream)` rolls it like `roll_loot`. It is in snapshots, the editor's component help and the schema. Verified by `loot_rolls_follow_weights_and_spend_needs_enough` (64 table rolls match `roll_loot` on the same entries; an object without a table and a missing object give `None`).
  - [ ] Item definitions (display names, stack limits, prices) as data. Wait for a game that needs more than an item name and a counter.
- [x] Dialogue and quest graphs as data assets, with localization keys and a runtime UI hookup.
  - [x] Dialogue data and stepping. The registered component `rusting.dialogue` (`lines: [{id, speaker, text, next, choices: [{text, next, counter, add}]}]`, `current`) sits on a named object and shows in the inspector; `GameScene::start_dialogue`, `dialogue_line` (speaker, text and choices translated by the locale) and `advance_dialogue(name, choice)` step it, and a choice adds to a counter. `current` is in snapshots and the state hash.
    - Verified (2026-10-09): `dialogues_step_through_lines_and_choices` (translated speaker and text, a missing choice stays, a choice adds 2 to `quest`, unknown and empty `next` end it, restarting, no dialogue, missing object).
  - [x] Runtime UI hookup: a HUD placeholder or menu that shows the running line and its choices and picks them with the mouse or keys, with no game code.
    - Verified (2026-10-09): HUD text `{dialogue:Name}` shows `speaker: text` and a button `{dialogue:Name/n}` shows choice n (`Continue` on a line without choices) and picks it on click; empty HUD elements are not drawn. `runtime::tests::dialogue_placeholders_show_the_line_and_its_choices` (translations, missing choices, ended dialogue) and `project_runner::tests::clicking_a_dialogue_choice_button_steps_the_dialogue` (other buttons ignored, Continue, a choice's counter and an ending `next`). Keyboard and gamepad picks go through egui button focus.
  - [x] Quest graphs: quest steps as data with counter guards, readable by HUD text and scenarios. Check first whether `rusting.state` plus counters already covers a quest; if so, document that pattern instead.
    - Done (2026-10-09) as a pattern: `rusting.state` already has counter guards, actions and scenario expects, so a quest is a state machine whose stages are translation keys. The missing piece was HUD text: `{state:Name}` now shows an object's state, translated. The `rusting.state` schema text documents the quest pattern. Verified by `runtime::tests::state_placeholders_show_the_translated_state`.
- [x] Timers, cooldowns, and a game clock with pause and time scale that never affect the fixed simulation step's determinism.
  - Verified (2026-10-09): `GameScene::cooldown_ready`, `start_cooldown` and `cooldown_left` count fixed ticks in an inspectable counter, so pause (`set_paused`) and the new `set_time_scale` stop or slow them with the simulation; `cooldowns_count_fixed_ticks` checks a 0.5 s cooldown ends on exactly tick 130 from tick 100 and that negative scales clamp to 0. The dash, turret and health recipes now use them, and their recipe scenario tests still pass.
- [ ] Spawners and object pools with capacity reporting (no silent drops).
  - [x] Capped spawner: `GameScene::spawn_numbered(template, position, limit)` names copies `"<template> <n>"` with the lowest free number and, at the limit, spawns nothing, adds to the counter `"<template> refused"` and warns once.
    - Verified (2026-10-09): `spawn_numbered_reuses_free_numbers_and_counts_refusals` spawns Bullet 1 and 2 under a limit of 2, refuses two more with `Bullet refused` at 2, reuses `Bullet 1` after a despawn at the new position, and gives `None` for a missing template.
  - [ ] Object pools that reuse hidden copies instead of spawning, with the same refusal counter. Wait for a game whose spawn cost shows in a profile; ECS spawns are cheap today.
- [ ] Gamepad input with dead zones, rumble, and hot plug. Needs a gamepad backend dependency approved by the owner.

### Exit gate

- A sample RPG slice uses saves, dialogue, a quest, an inventory, and AI enemies with no custom framework code, and its scenarios load a save made by the previous engine version.

## Milestone 31: Terrain, large worlds, and streaming

Goal: build outdoor and open-world games, not only arenas.

Depends on: Milestones 2, 4, and 10; Milestone 13 for foliage rendering.

- [ ] General heightmap terrain with LOD, holes, splat-map materials, and physics collision; sculpt and paint tools in the editor.
- [ ] Foliage and detail scattering by rules (slope, height, mask), linked to the Milestone 13 instancing item.
- [ ] World partition: cells that load and unload by distance, with async asset loading and no hitch above a stated budget.
- [ ] Large coordinates: origin rebasing or 64-bit world positions, documented and tested at 100 km from the origin.
- [ ] Hierarchical LOD and impostors for distant cells.
- [ ] Oceans and rivers built on the existing `rusting.water` buoyancy and waves.
- [ ] Time of day and weather as scene settings driving sun, sky, fog, and wind fields.
- [ ] Splines and paths as a shared component for roads, rails, rivers, and camera tracks.

### Exit gate

- A 16 km² open-world sample streams with no loading screens, keeps frame-time spikes within budget, and stays stable at the far corner (hardware gate for timing).

## Milestone 32: Cinematics and media

Goal: tell stories in the engine with cutscenes, camera work, and video.

Depends on: Milestones 14, 15, and 16.

- [ ] A sequencer timeline asset with tracks for transforms, animation, cameras, audio, events, and properties, played deterministically by fixed tick.
- [ ] Camera rails, look-at targets, blends between cameras, and letterboxing, on the Milestone 31 splines.
- [ ] Subtitles and captions tied to audio cues and localization.
- [ ] Video playback to a texture and to the screen. Needs a decoder dependency approved by the owner.
- [ ] In-game photo mode and trailer capture (fixed-step offline rendering to an image sequence at any resolution).

### Exit gate

- A cutscene plays identically in the editor, the game, and an offline render, and a scenario can skip it and check the state after it.

## Milestone 33: Procedural content

Goal: generate levels and content from seeds, as ordinary engine data that tools and agents can inspect.

Depends on: Milestones 8 and 9.

- [x] Seeded noise, sampling, and random streams on the Milestone 8 stream rules, with results identical across platforms. Done 2026-10-12: `procgen::RandomStream` (`new`, `from_seed(RandomSeed, tick, subsystem, key)` on the `RandomSeed::value`/`stream` rules, `next_u64`, `unit`, `range`, `below`, `chance`, `weighted`, `shuffle`, `in_circle`), gradient noise `noise2`/`noise3` with fractal `fbm2`/`fbm3`, and Bridson Poisson-disk sampling `poisson_disk(seed, size, spacing)`. Only integer hashing and IEEE `+ - * /`, `floor` and constant `sqrt(2)` on `f32`, no transcendental functions, no FMA. Docs: `docs/procedural.md` (also `rusting docs show guide/procedural`). Evidence: unit tests `procgen::tests::a_stream_repeats_for_its_seed_and_stays_in_range` (same seed and tick repeat, the tick changes the stream, ranges hold, `below(6)` within 5% of uniform over 60000 draws, zero weights never picked, shuffle is a permutation), `noise_is_smooth_bounded_and_zero_on_the_lattice` (0 at lattice points, a 0.001 step moves under 0.01, values within [-1, 1] and reaching past ±0.4), `poisson_points_keep_their_spacing_and_fill_the_area` (every pair at least the spacing apart, no spot on a 1-unit grid farther than twice the spacing from a point, same seed same points). Each test pins an FNV digest of the output bits, recorded on x86-64; another platform must match it, which is the cross-platform check (not run on other hardware here). Full check: fmt, clippy (default, no-default-features, gpu-tests), tests (lib 1125 with gpu-tests) green.
- [x] Generators as data assets: a graph or Rust function whose output is a normal scene or prefab, cached by seed and inputs, and regenerated on change. Done 2026-10-12 as Rust functions: `scene_generators` in `project.json` (`command`, `output`, `seed`, `inputs`, `watch`) name a project command, usually a game binary using `procgen::GeneratorRequest::from_env` and `save_scene`; `rusting scene generate <root> [name...] [--force] [--dry-run]` runs it with `RUSTING_SEED`, `RUSTING_INPUTS` and `RUSTING_OUTPUT`, checks the result parses as a scene before it replaces the output through `write_atomic` (journaled, revertible), and caches by an FNV-1a key over command, seed, inputs and the watched files' contents in `.rusting/generated.json`; an unchanged generator reports `cached`, and a scene edited by hand since generation is refused with `GENERATED_SCENE_EDITED` unless `--force`. The output is a normal scene, instanced as a prefab like any other. Docs: `docs/procedural.md` "Generated scenes", the operation catalog and the `project.json` schema. Evidence: unit test `asset_import::tests::scene_generators_cache_by_inputs_and_refuse_bad_output` (dry run runs nothing; seed and inputs reach the command; second run cached; a watched file or seed change reruns; `--force` reruns; a hand edit is refused and kept, then forced over; invalid, missing and failing output leave the last scene and no staged file; unknown name gives `GENERATOR_UNKNOWN`). Smoke run through the debug binary on a puzzle-template copy with a `src/bin/gen_level.rs` using `rooms_and_corridors`: 430 wall cubes generated, second run `cached`, editing the watched binary source regenerates, a hand edit gives `GENERATED_SCENE_EDITED`, `rusting log` lists the write. Full check: fmt, clippy (default, no-default-features, gpu-tests), tests (lib 1129 with gpu-tests) green. Limits: no node graph (a generator is a command); regeneration happens when `scene generate` runs, not automatically inside `rusting run`.
- [x] Tile and grid generators: wave function collapse, rooms and corridors, and cellular automata, for tile maps and 3D grids. Done 2026-10-12 in `procgen`: `cellular_caves(seed, size, fill, steps)` (the 4-5 rule in 2D, 13-14 of 26 neighbours in 3D, edges count as walls), `rooms_and_corridors(seed, size, rooms, room_size)` (non-touching rooms inside a wall border, each joined to the one before by an L-shaped corridor; returns `RoomLayout { floor, rooms }` with `Room::center`), and `wave_function_collapse(seed, size, weights, fits)` (up to 64 tiles, fewest-options-first collapse with propagation along the three axes, retries from 20 streams, `None` when the rules cannot be met). Grids lay cells out `x` fastest (`cell_index`); `tile_rows` turns a 2D grid into `TileMap` rows. Docs: `docs/procedural.md` "Tile and grid generators". Evidence: unit tests `procgen::tests::caves_smooth_into_open_areas_in_2d_and_3d` (repeats per seed, wall share in range, no lone wall cell after smoothing, fill 1 is all wall, a 12x12x12 grid), `rooms_do_not_touch_and_corridors_join_every_floor_cell` (8 rooms within size bounds, no two touching, border all wall, a flood fill from the first room reaches exactly the floor cells, a map too small places none), `collapse_meets_the_rules_in_2d_and_3d` (stripe rules give alternating columns, a 10x10x4 land-coast-sea grid never puts land beside sea along any axis and uses all three tiles, impossible rules and 65 tiles give `None`); each pins the output digest. Full check: fmt, clippy (default, no-default-features, gpu-tests), tests (lib 1128 with gpu-tests) green.
- [x] Mesh generation helpers: extrusion along splines, lofting, and simple constructive solid geometry, with colliders.
  Evidence: `meshgen::spline` (Catmull-Rom points through control points), `extrude` (closed profile swept with parallel-transport frames, capped), `loft` (rings skinned and capped, winding fixed outward by signed volume), and `ProcMesh::union`/`subtract`/`intersect` (csg.js-style BSP); `ProcMesh::glb` writes binary glTF through the placeholder-mesh writer and `ProcMesh::asset` gives a flat-shaded `MeshAsset`. Tests: `extrusion_and_lofting_make_closed_outward_meshes` (closed edges, box volume 6, a pipe along a spline at area times length within 2%, both profile orders, a lofted vase at its analytic volume, identical output on repeat), `solid_operations_cut_and_join_closed_meshes` (overlapping 2 m cubes: union 15, subtract 7, intersect 1; a hole through a slab; disjoint pieces), `generated_meshes_collide_through_mesh_colliders` (a floor with a pit cut by `subtract`, as a `TriangleMesh` collider: a ball rests on the floor at 0.25 m and one falls into the pit to -0.25 m), and `generated_meshes_load_as_gltf` loads a `ProcMesh::glb`. Docs: procedural.md "Meshes". Full check green, lib 1132 passed + 3 ignored with gpu-tests. Limits: no `rusting` CLI command for meshes (a game binary or `scene_generators` command calls them); CSG output can have T-junctions; extrude caps need a star-shaped profile.
- [x] Validation hooks so a generated level can be checked by invariants and the Milestone L5 explorer bot for reachability.
  Evidence: a `scene_generators` entry takes `checks` (scenario files); `rusting scene generate` plays each on the staged scene through the new `RunOptions.scene` (`RUSTING_SCENE_PATH`) before it replaces the old one, and a failure keeps the old scene with `GENERATED_SCENE_CHECK_FAILED` (catalogued, schema and operation summary updated). Unit test `scene_generators_cache_by_inputs_and_refuse_bad_output` covers a rejecting check (the check sees the new scene, the old one stays) and a passing one (`checked` reported). Smoke on a third-person template game with a Python generator placing a `Goal` and `tests/reach.json` (`explore` goals `["Goal"]`, 350 ticks per goal): distance 4 gave `generated` with `checked: ["tests/reach.json"]`; distance 300 gave `GENERATED_SCENE_CHECK_FAILED ... explore: could not reach \`Goal\``, the scene kept the old goal and no staged file was left. Full check green, lib 1129 passed + 3 ignored with gpu-tests. Limits: checks run one by one with a full game build each time the main scene cooks; no automatic retry over seeds.

### Exit gate

- A roguelike sample generates a new, fully reachable level per seed, and the same seed gives the same level on every machine.

## Milestone 34: Modding and user-generated content

Goal: players can extend a shipped game safely.

Depends on: Milestones 9, 21, and L8.

- [ ] Mod packages: data assets, scenes, and WASM scripts in a versioned archive with a manifest, dependencies, and a load order.
  - [x] Data assets and scenes: `src/mods.rs` reads `mod.json` (name, `major.minor.patch` version, `depends` with caret-style minimums) from a mod folder or a `.rmod` archive (the `.rpak` format), loads `mods/` in `load_order.json` order or by name with dependencies first (stable topological sort; unmet, disabled, wrong-version and cyclic mods and their dependents are left out with `MOD_DEPENDENCY_UNMET` / `MOD_DEPENDENCY_CYCLE` / `MOD_MANIFEST_INVALID`), and mounts them over `assets::read_file` / `file_len` as a game starts, last mod winning. `rusting mod pack` and `rusting mod list`; `docs/modding.md`. Evidence: `mods::tests` (versions, load order with cycles and missing or wrong-version dependencies, a packed and a folder mod overriding and adding files through `read_file`); smoke on a `starter` project in `target/tmp`: `mod pack` made `mods/hello.rmod`, `mod list` listed it, and `rusting run --ticks 5` printed `mods: loaded hello 1.0.0` and read the mod's `assets/msg.txt` (`"modded text"`), `"game text"` with the mod removed. lib 1135.
  - [ ] WASM scripts in mods: after L8 gives a WASM runtime.
- [ ] Capability-based sandbox for mod scripts: a game declares which APIs mods may call.
- [x] Mod conflict reporting by asset and component field. `mods::conflicts` lists every `assets/` or `scenes/` file more than one loaded mod holds (mods in load order, last wins) and, for `.rscene` files, each field two mods changed differently from the game's scene, through `scene_merge::merge_scenes` narrowed to component fields (`components/rusting.point_light/intensity`); `rusting mod list` returns them under `conflicts` and games print `mods: FILE is in A, B; B wins` on start. `rusting mod pack` now requires `--out` so `--confine` checks the write. Evidence: `mods::tests::scene_conflicts_name_entity_and_component_fields` (intensity and color conflicts reported per mod pair; different fields of one component and identical moves are not), the asset case in `mounted_mods_replace_game_files_in_load_order`; CLI smoke: two folder mods changing a lamp's intensity gave `{"entity":"lamp","field":"components/rusting.point_light/intensity","mods":["a","b"]}`, and `RUSTING_CONFINE` refused `mod pack a --out ../a.rmod` with `OUTSIDE_CONFINE`. lib 1136.
- [ ] Reusable in-game level editor built from the editor's widgets and command layer, with undo.
- [ ] Workshop and storefront hooks through the Milestone 21 platform services abstraction.

### Exit gate

- A sample game ships a mod that adds a level, an item, and a scripted enemy, and a malicious mod script cannot read files or reach the network.

## Milestone 35: XR and additional input

Goal: record and, where justified, deliver VR and new input devices.

Depends on: Milestones 3, 4, and 21.

- [ ] OpenXR support through Vulkan: stereo rendering, head and hand tracking, and controller actions in the action map. Needs owner approval for the loader dependency; record it as a post-1.0 decision if deferred.
- [ ] Physics-based hand interaction (grabbing, throwing, pushing) built on the typed bridge.
- [ ] Touch and pen input with gestures, shared with the Milestone 21 Android work.
- [ ] Comfort options: snap turning, vignette, and seated mode.

### Exit gate

- A physics sandbox runs in a headset with stable frame timing and hand interaction (hardware gate), or the decision to defer XR past 1.0 is recorded with its blocking reasons.

## Continuous test and CI plan

### Unit tests

- [x] Transform composition and decomposition. Done 2026-10-11: `Transform::to_matrix` builds translation * rotation * scale; `Transform::from_matrix` takes scale from the column lengths and rotation from the normalized columns. Fixed: a mirrored matrix (negative determinant) used to come back as positive scales with a wrong rotation, and a zero scale made the rotation NaN; the mirror now goes into the X scale and a zero-scale column keeps finite angles. Evidence: unit test `transform::tests::transforms_compose_and_decompose_back_to_the_same_matrix` (identity, translation, rotated and non-uniformly scaled, gimbal lock at a 90 degree pitch, mirrored, and flattened transforms decompose back to the same matrix; plain angles come back exactly; a parent times child matrix moves the child's +X offset to the expected world point and decomposes back). Full check: fmt, clippy (default, no-default-features, gpu-tests), rustdoc, tests (lib 890, 1007 with gpu-tests; rusting-core 57) green.
- [x] Hierarchy propagation and cycle rejection. Done 2026-10-11: `rusting_core::hierarchy` tests cover propagation (`propagation_composes_parent_and_child_transforms`, `propagation_uses_transformless_intermediate_entities_as_identity`, `clearing_the_final_parent_resets_an_unchanged_child_global`, `propagation_skips_unchanged_worlds_but_sees_later_edits`, and `moving_part_of_a_hierarchy_matches_a_full_rebuild_bit_for_bit` over 60 entities and 4 ticks) and malformed data (`propagation_diagnoses_a_missing_parent`, `propagation_diagnoses_a_malformed_cycle`, `propagation_diagnoses_a_transformless_cycle_deterministically`). New test `deep_self_and_preexisting_cycles_are_rejected`: `set_parent` rejects a grandparent loop, a direct loop and a self-parent with `HierarchyError::Cycle`, the three-level chain still propagates to the summed position, and parenting beneath a loop written past `set_parent` is rejected without hanging or changing the child. `cycle_rejection_does_not_mutate_existing_hierarchy` checks a rejected edit leaves both sides intact. Full check: fmt, clippy (default, no-default-features, gpu-tests), rustdoc, tests (lib 890, 1007 with gpu-tests; rusting-core 58) green.
- [x] Material default consistency and copying.
- [x] Stable generational asset handles. Done 2026-10-11: a `Handle<T>` is a `Copy` index plus generation; a reused slot gets the next generation, so a stale handle never resolves to the new asset. Fixed: a slot's generation used to wrap from `u32::MAX` back to 1, which after four billion reuses of one slot let a first-use handle resolve again; `Assets::remove` now retires a slot at the last generation instead of reusing it. Evidence: unit tests `assets::tests::stale_handle_does_not_resolve_reused_slot` and `assets::tests::handles_stay_unique_across_reuse_keys_and_exhausted_slots` (99 reuses of one slot never repeat a handle, the key round-trips through serde and `from_key`, a handle past the last slot resolves to nothing, a slot at `u32::MAX` is not reused). Full check: fmt, clippy (default, no-default-features, gpu-tests), rustdoc, tests (lib 889, 1006 with gpu-tests) green.
- [x] Scene round trips and schema migration. Done 2026-10-11 (tests already present, checked and recorded): round trips in `runtime::scene_file::tests` `scene_round_trip_preserves_hierarchy_assets_and_registered_components`, `builtin_primitive_round_trips_without_an_asset_file`, `all_authored_light_types_round_trip_together`, `material_alpha_mode_survives_scene_round_trip`, `tools_keep_unregistered_components_through_a_round_trip`, `source_and_compiled_scene_decode_to_same_document` and `saved_scene_can_be_cooked_and_loaded_by_a_fresh_runtime`. Every older format version (`SCENE_FORMAT_VERSION` is 10) has a migration test: unversioned and newer-is-rejected (`unversioned_text_scene_migrates_but_newer_scene_is_rejected`), and versions one through nine (`version_one_cooked_scene_migrates_without_gpu_watch_data` through `version_eight_and_nine_cooked_scenes_migrate_their_materials`). Component field migrations: `reflect::tests::migrations_rename_and_remove_fields_and_newer_versions_fail`. `cargo test --lib runtime::scene_file::tests`: 29 passed. No code change; the full check of commit dc61f27 covers this tree (lib 889, 1006 with gpu-tests).
- [x] Asset path canonicalization and deduplication. Done 2026-10-11: `Assets` keys each path by `normalize_path` (`std::fs::canonicalize` for existing files, else a lexical absolute path without `.` and `..`). Evidence: unit tests `assets::tests::equivalent_paths_are_deduplicated` (paths that do not exist: `assets/../assets/cube.mesh` and `./assets/cube.mesh` share one handle and load once) and `assets::tests::real_files_dedupe_by_canonical_path_and_reload_after_removal` (real files reached through `..`, `.`, a doubled slash and, on Unix, a symlinked folder share one handle and load once; a second file gets its own; a removed asset loads again into a new handle; an empty path fails with `EmptyPath`). Full check: fmt, clippy (default, no-default-features, gpu-tests), rustdoc, tests (lib 888, 1005 with gpu-tests) green.
- [x] Input action mapping. Done 2026-10-11: `rusting_core::input` tests cover `ActionMap` (`actions_cover_keyboard_mouse_and_edge_clearing`, `pad_buttons_and_stick_directions_drive_actions`), and the new `runtime::actions::tests::scene_actions_follow_their_components_and_keep_code_bindings` covers the scene side: `parse_input` reads key, mouse and pad names and names the bad input; two `InputAction` components binding the same input bind it once; editing a component adds its new input; despawning every component drops the scene's bindings and keeps the one game code bound itself. `project_runner::tests::scene_input_actions_bind_and_report_presses` checks a press through `GameScene::pressed`. Full check: fmt, clippy (default, no-default-features, gpu-tests), rustdoc, tests (lib 890, 1007 with gpu-tests) green.
- [x] Quality-profile selection. Done 2026-10-11: `scene_renderer::resolve_quality` turns `QualityProfile::Auto` into `Eco` on integrated GPUs, `Balanced` below 4 GiB of device-local memory and `High` otherwise; a concrete profile passes through. Evidence: unit test `rendering::scene_renderer::tests::auto_quality_resolves_from_device_class_and_concrete_profiles_pass_through` (now also checks the exact 4 GiB cut: 3 GiB is `Balanced`, 4 GiB is `High`; light budgets rise from `Eco` to `High`), `culling_path_follows_mode_ownership_count_device_and_quality`, `editor::tests::project_area_shows_lod_groups_and_the_resolved_auto_quality` (the Project area shows `Auto` resolved to `Eco` on an integrated GPU) and `cli::hint_tests::lint_names_visible_lights_past_the_quality_budget`; GPU test `quality_profiles_compare_on_one_scene`. Full check: fmt, clippy (default, no-default-features, gpu-tests), rustdoc, tests (lib 890, 1007 with gpu-tests) green.
- [ ] Stable ECS/GPU `PhysicsId` conversion, generation rejection, and entity mapping.

### Layout and shader tests

- [x] Rust physics storage-buffer sizes and offsets.
- [x] Common instance field order across compute shaders.
- [x] Common physics push-constant declaration across compute shaders.
- [ ] Shader reflection compared with every Rust storage, uniform, vertex, and push-constant type.
- [ ] Shader compilation as a dedicated CI job.

### Renderer integration tests

- [ ] Resize, minimize, restore, and zero-sized surface handling.
- [ ] Unsupported present-mode and format fallback.
- [ ] Frames-in-flight reuse and validation-clean shutdown.
- [ ] Asset unload/reload while referenced by submitted frames.
- [ ] Culling enabled/disabled equivalence.
- [ ] Automatic primitive/imported-mesh bounds contain their complete source geometry.
- [ ] GPU-owned objects are culled from the current GPU transform without full-state CPU readback.
- [ ] Objects outside the camera stop producing render work but continue physics simulation.
- [ ] `CullingMode::Auto` skips culling overhead below its tested scene-size threshold.
- [ ] Capability fallback behavior on the low-end baseline.

### Golden-image tests

- [ ] Primitive normals.
- [ ] glTF texture channels and color spaces.
- [ ] Alpha mask and blend modes.
- [ ] Directional shadow depth.
- [ ] PBR reference spheres.
- [ ] Tone mapping.
- [ ] Editor compositing.

### Physics tests

- [x] Triggers and collision events.
  Evidence (2026-10-11, lane physics): CPU: `runtime::tests::cpu_collision_events_follow_touching_pairs_and_stop_when_they_part` (new). A ball dropped 0.5 m sends its first non-sensor `CollisionEvent` with the ground on step 21, at most one per pair per step whatever the substeps, and none from the step after it falls asleep. A kinematic walker crossing a sensor at 3 m/s gets a sensor event on each of the 35 steps it overlaps and none outside, and is never pushed. Events show the frame after their step (`EventQueue` double-buffers). Also `cpu_restitution_bounces_and_sensors_only_report` and `cpu_collision_layers_filter_pairs_and_queries`. GPU: `raw_gpu_events_reach_the_live_ecs_entity`, `gpu_events_are_delivered_in_tick_and_body_order`, `physics_ids_reject_delayed_events_after_slot_reuse`, and in `scene_renderer` (gpu-tests) `physics_events_read_back_from_transient_arenas_every_tick`, `event_buffer_fits_every_step_and_reports_losses_past_budget`, `ten_thousand_gpu_bodies_report_condition_events_without_state_readback`. fmt, all three clippy configurations, `cargo test --workspace` (lib 903; the dedicated-server real-time test flaked once under load and passes alone) and the gpu-tests run (lib 1023) pass.
- [x] Raycasts and filtered queries.
  Evidence (2026-10-11, lane physics): `runtime::tests::cpu_queries_skip_filtered_colliders_and_hit_the_one_behind` (new). Along one line of boxes, `raycast` with each layer mask passes the boxes off the mask and hits the next one at the exact distance (sensors included), `raycast_where` also skips what its predicate rejects, a ray stops at its max distance, and a ray starting inside a box hits the next. `shape_cast` takes the same mask but skips sensors and `exclude`, and stops 0.25 m short for a 0.25 m ball. `overlap_sphere` filters by mask in stable entity order. Also `cpu_collision_layers_filter_pairs_and_queries`, `cpu_shape_casts_stop_before_colliders_and_characters_slide`, `raycast_only_hits_the_named_classes_and_passes_the_rest`, `raycast_visible_passes_through_hidden_objects_and_their_children`, `ray_hits_on_ragdoll_bodies_carry_the_bone_name_and_classes`, `a_fixed_gpu_box_is_solid_to_cpu_queries_but_a_dynamic_one_is_not`, `gpu_events_place_move_and_remove_a_cpu_query_proxy`, and in `cpu_physics` `rays_hit_each_shape_on_its_surface`, `rays_hit_hulls_from_outside_and_triangle_meshes_from_either_side`, `ray_trees_find_the_same_hits_as_scanning_every_item`. fmt, all three clippy configurations, `cargo test --workspace` (lib 904) and the gpu-tests run (lib 1024) pass.
- [x] Stable stacking.
  Evidence (2026-10-11, lane physics): GPU: `scene_renderer::tests::a_twenty_cube_tower_stands_and_sleeps_on_the_gpu` (new, gpu-tests). Twenty unit cubes dropped 1 cm apart, each 2 cm off the one below in turn along x and z, settle onto a GPU ground collider and sleep; every cube ends within 5 cm of where it started across, 3 cm of its stacked height and 0.02 rad of level (measured on the RTX 3060: exact heights, no drift, no tilt). CPU: `cpu_boxes_fall_and_rest_in_a_stack_on_static_ground` (3 boxes within 3 cm, 5 mm and 0.01 rad), `tall_box_stacks_settle_and_sleep_with_cached_contacts` (10 boxes sleep within 2.5 cm across and 1 cm of height), `a_hit_on_the_bottom_of_a_sleeping_stack_wakes_and_topples_it`, and the suite's `StackingStability`. Limit: a 20-cube CPU tower still drifts about 10 cm; that is the open "Solver quality the suite exposed" item in Milestone 12. fmt, all three clippy configurations, `cargo test --workspace` (lib 904) and the gpu-tests run (lib 1025) pass.
- [x] Tunneling/CCD cases.
  Evidence (2026-10-11, lane physics): GPU: `integrate` in `physics.comp` now moves a colliding body in pieces no longer than its inner radius (new `inner_radius` in `physics_shapes.glsl`), at most 64 per tick, and resolves static colliders after each piece. `scene_renderer::tests::fast_gpu_bodies_do_not_tunnel_through_thin_colliders` (new, gpu-tests): a 0.1 m ball at 270 m/s (4.5 m a tick) and a 0.1 m cube at 100 m/s both stop against the front face of a 10 cm wall collider; with the sweep capped at one piece the ball passes through to z = -45. CPU: `fast_cpu_bodies_do_not_tunnel_through_thin_walls`, `a_fast_ball_between_two_dynamic_boxes_pushes_both_back` and the suite's `CcdBullets`. Limit: GPU body-vs-body contacts are not swept, and past 64 inner radii a tick a GPU body can still tunnel. fmt, all three clippy configurations, `cargo test --workspace` (lib 904) and the gpu-tests run (lib 1026) pass.
- [x] Fixed-step independence from render frame rate.
  Evidence (2026-10-11, lane physics): `runtime::tests::cpu_physics_matches_bit_for_bit_whatever_the_render_frame_rate` (new). A tilted box tipping onto the ground and a bouncing ball run 240 fixed ticks with frame deltas of a steady 60 Hz, 144 Hz, 30 Hz and a stutter (0 to 50 ms, several steps in some frames); positions, rotations and velocities match bit for bit across all four. Also `schedules_and_fixed_catch_up_are_deterministic`, `fixed_steps_slower_than_real_time_fall_back_to_slow_motion`, `pause_and_single_step_keep_variable_systems_alive`, and for the GPU `fixed_steps_see_their_own_tick_and_stamp_gpu_commands_for_the_next` (GPU ticks follow the fixed tick, not frames). fmt, all three clippy configurations, `cargo test --workspace` (lib 905) and the gpu-tests run (lib 1027) pass.
- [x] GPU grid overflow and fallback.
  Evidence (2026-10-11, lane physics): existing `scene_renderer::tests::gpu_bodies_collide_with_each_other_through_grid_and_fallback` (gpu-tests), no code change. Twenty pebbles crowd one 0.8 m cell that holds sixteen, beside a body too big for one cell: the overflowing and oversized bodies go to the fallback list and still collide (every pebble ends at rest more than 0.18 m from every other), `CapacityDiagnostics` reports `physics_grid_overflow`, `physics_oversized_bodies` and `physics_fallback_tests` above zero, and a rerun matches bit for bit. With no memory for the hash (one cell for everything, 7 bodies spilling per contact round, as `physics_grid_overflow` reports) the result is bit for bit the same as with a full grid. Also `benchmark_scenes_run_on_the_gpu_and_repeat_exactly`. The gpu-tests run (lib 1027) passes on this tree.
- [x] GPU event overflow and fallback.
  Evidence (2026-10-11, lane physics): `rendering::capture::tests::gpu_events_past_the_buffer_are_counted_and_reported_to_the_world` (new, gpu-tests). In a full `App` through `HeadlessCapture`, 100 GPU bodies run a condition shader that reserves one event per body and tick but sends two; over 8 frames the buffer delivers the reserved half as `GpuPhysicsEvent`s, the rest arrive as `GpuPhysicsEventsLost` counts equal to the delivered number, and `RenderCapacityDiagnostics::physics_events_dropped` keeps the same running total. Also `scene_renderer::tests::event_buffer_fits_every_step_and_reports_losses_past_budget` (a three-tick hitch still fits every firing; a 60-event cap keeps 60 of 100 and reports 40 lost once). The fallback is documented on `GpuPhysicsEventsLost`: `WhileTrue` rules fire again next tick, edge events need a `SelectedState` resync. fmt, all three clippy configurations, `cargo test --workspace` (lib 905) and the gpu-tests run (lib 1028) pass.
- [x] Built-in boolean/range conditions and custom shader conditions emit according to `OnEnter`, `OnExit`, `WhileTrue`, `Once`, and cooldown modes.
  Evidence (2026-10-11, lane physics): `scene_renderer::tests::built_in_conditions_fire_by_mode_and_cooldown` (new, gpu-tests). Five GPU bodies cross x in 0.9..=2.1 on ticks 2 to 4, each with one rule, alternating the range form (`inside`) and the boolean form (`greater_than(..).and(less_than(..))`): `OnEnter` fires on tick 2, `OnExit` on 5, `WhileTrue` on 2, 3 and 4, `Once` on 2, and `WhileTrue` with a 1.5-tick cooldown on 2 and 4, each with its own event ID. Also `conditions_use_any_field_and_see_custom_value_commands`, `rust_condition_builder_compiles_to_postfix_gpu_instructions`, `event_buffer_fits_every_step_and_reports_losses_past_budget` (`WhileTrue` every tick). Custom shader conditions call `emit_event` themselves, so the modes and cooldown do not apply to them; a shader keeps its own edge or cooldown state in `custom_values`, which persists between ticks (`custom_condition_shaders_emit_events_and_skip_broken_sources`). fmt, all three clippy configurations, `cargo test --workspace` (lib 905) and the gpu-tests run (lib 1029) pass.
- [x] Condition events reach the correct ECS entity with their registered event ID and requested payload.
  Evidence (2026-10-11, lane physics): `rendering::capture::tests::gpu_condition_events_reach_their_entity_with_id_and_payload` (new, gpu-tests). In a full `App` through `HeadlessCapture`, after a despawned entity and an unwatched GPU body (so entity, slot and spawn order differ), two falling GPU bodies carry `GpuPhysicsWatch` rules: "fast" (`velocity_y < -3`, `Velocity` payload) arrives first as a `GpuPhysicsEvent` on its own entity with `GpuEventRegistry::id("fast")` and a y velocity in -3.2..-3.0; "low" (`position_y < 1`, `Position` payload) arrives later on the other entity with its ID, its exact x and z, and y in 0.9..1.0. Nothing else arrives. Also `raw_gpu_events_reach_the_live_ecs_entity`, `gpu_events_are_delivered_in_tick_and_body_order`, `physics_ids_reject_delayed_events_after_slot_reuse`, `condition_shaders_reach_the_render_world_with_registered_events`. fmt, all three clippy configurations, `cargo test --workspace` (lib 905) and the gpu-tests run (lib 1030) pass.
- [x] CPU-to-GPU commands reject stale body generations.
  Evidence (2026-10-11, lane physics): `rendering::capture::tests::gpu_commands_for_a_despawned_body_skip_the_one_reusing_its_slot` (new, gpu-tests). In a full `App` through `HeadlessCapture`, a GPU body is despawned and the next one takes its slot with a new generation; a 50 N s sideways `Impulse` pushed with the old `PhysicsId` is rejected (`physics_commands_rejected` rises by exactly 1) and leaves the new body's x position and x velocity at exactly zero change, while the new body's own `Impulse` gives it exactly 2 m/s. Also `scene_renderer::tests::commands_apply_once_before_the_step_and_reject_stale_bodies` (a wrong generation and an unknown slot are both rejected, the rest apply once) and `physics_ids_reject_delayed_events_after_slot_reuse`. fmt, all three clippy configurations, `cargo test --workspace` (lib 905) and the gpu-tests run (lib 1031) pass.
- [x] Asynchronous selected-state readback reports its source tick and frame age.
  Evidence (2026-10-11, lane physics): `scene_renderer::tests::overlapped_state_readback_reports_its_tick_and_frame_age` (new, gpu-tests). Three physics frames of a `SelectedState` body moving 0.1 m a tick are submitted before any is waited on; the samples come back tagged with ticks 1, 2 and 3, each pose matching its own tick, and the next frame's `RenderCounters::physics_readback_latency_frames` reports 2. Also `runtime::tests::sync_mode_controls_rules_and_state_mirror` (a late sample never rewinds `GpuStateMirror`, whose `age_ticks(8)` is 3 for tick 5) and `gpu_bodies_collide_with_each_other_through_grid_and_fallback` (latency 0 when every frame is waited on). fmt, all three clippy configurations, `cargo test --workspace` (lib 905) and the gpu-tests run (lib 1032) pass.
- [x] Explicit CPU/GPU simulation ownership and synchronization modes.
  Evidence (2026-10-11, lane physics): `rendering::capture::tests::each_body_has_one_simulation_owner_and_its_sync_mode` (new, gpu-tests). In one full `App` through `HeadlessCapture`, over 30 frames: `SimulationClass::None` and `Static` bodies stay exactly where placed, the `Cpu` body falls through its own `Transform`, and the four `Gpu` bodies keep their authored `Transform` while `PhysicsSyncMode::None` and `Events` get no `GpuStateMirror`, `SelectedState` gets a falling mirror without custom values, and `FullState` one with them. (A CPU body needs a `Collider` to be simulated.) Also `auto_simulation_picks_cpu_or_gpu_and_stays_overridable` (automatic ownership, never migrating once decided, hard requirements over count), `sync_mode_controls_rules_and_state_mirror` (`None` drops watch rules) and the GPU readback tests above. fmt, all three clippy configurations, `cargo test --workspace` (lib 905) and the gpu-tests run (lib 1033) pass.

### Physics scenario and benchmark suite

- [x] Joint, articulation, character-controller, vehicle, and ragdoll regression scenes with recorded expected results. Evidence: `SuiteScene` (`src/runtime/physics_suite.rs`) gains `ArticulatedChain` (the joint chain hung from a fixed anchor carrying `Articulation`) and `CharacterStairs` (a default `PlayerController` walks up `size` 0.2 m steps; the error is how far its feet end below the top surface); the `physics_suite` example runs both. New test `runtime::physics_suite::tests::regression_scenes_repeat_their_recorded_results` runs the 8-link joint chain (240 steps), the 8-link articulated chain (240), a 4-step stair (180), one vehicle at full lock (240) and one limp ragdoll (120) twice each: both runs are bit-identical, and each error is within 1e-4 of the value recorded on 2026-10-11 (0.2071 m, 1.9e-6 m, -4.1e-4 m, 0.0686 rad, 0.0153 m). Limits: CPU solvers only; each scene records one error number, not a full trajectory, and the 1e-4 tolerance is there for another libm, so a change past it means re-recording after checking the scene. Full check 2026-10-11: fmt and clippy x3 clean; lib 906 tests, 1034 with gpu-tests.
- [ ] Soft-body, cloth, rope, fluid, granular, and fracture scenarios under lavapipe.
- [ ] Two-way coupling scenarios between rigid, deformable, and fluid simulation.
- [ ] 2D physics scenarios mirroring the 3D suite.
- [ ] Comparative benchmark harness against Jolt, PhysX, Rapier, and Godot Physics with stored baselines.

### Engine feature tests

- [x] Reflection round trip for every registered component and data asset. Done 2026-10-11: new unit test `runtime::scene_file::tests::every_registered_component_round_trips_through_source_and_cooked_scenes` adds every component in the default `SceneComponentRegistry` (64) to its own object, changes every number and flag from its default (an ignored change fails), saves the scene, and checks each component's saved value is identical after loading both the JSON source and the cooked bincode form into a fresh app. `project_runner::tests::every_registered_component_round_trips_through_an_object_save` covers the same registry through object saves. The engine registers no built-in data asset type; `assets::data::tests::data_assets_load_share_save_and_reload_through_handles` round-trips a game-registered one through `.rdata` files, and materials (the reflected asset type in `TypeRegistry`) round-trip in `material_alpha_mode_survives_scene_round_trip` and `gltf_materials_survive_save_and_fresh_load_unless_overridden`. Full check: fmt, clippy (default, no-default-features, gpu-tests), rustdoc, tests (lib 891, 1008 with gpu-tests) green.
- [ ] Prefab override propagation, nesting, variants, and cycle rejection.
- [ ] Observers and scene-file event connections fire the registered handlers.
- [ ] Animation sampling, blending, IK, and ragdoll handoff are deterministic.
- [ ] Offline audio render compared against reference buffers.
- [ ] UI layout, text shaping, theming, and localization golden images.
- [ ] 2D sprite, tilemap, and 2D lighting golden images.
- [ ] Navigation baking, re-baking budget, and agent avoidance.
- [ ] Networking RPC, replication, prediction, and rollback over loopback with simulated loss. Partly done 2026-10-11: `net::tests::rpcs_replication_and_prediction_survive_a_lossy_loopback` runs 40 owner RPC moves, 60 replication snapshots with acks, and client prediction over a loopback session with 33% loss, 2 ms latency and 3 ms jitter on both ends; moves arrive once and in order, every reconcile equals the local prediction, the host x matches it with no held messages, and `Replica::verify` matches the host checksum (passes 5/5 runs, 0.7 s). Left open: rollback, blocked on `App::restore` refusing apps that spawned entities after the snapshot.
- [x] Export smoke test for every supported platform preset. Done 2026-10-11: new ignored `tests/cli.rs` `every_platform_preset_exports_a_game_that_runs` creates a starter-template game with one `export_presets` entry per supported platform (Linux and Windows, `docs/getting-started.md`): `linux` (host) and `windows` (`x86_64-pc-windows-gnu`). It exports each with `rusting export --preset`. The Linux export must pass its own headless verification (exit code 0). The Windows export must report `EXPORT_NOT_VERIFIED` and hold a PE `smoke.exe` (`MZ`), which the test runs headless for 60 ticks under Wine when Wine is installed. It passed here with Wine (`cargo test --test cli every_platform_preset_exports_a_game_that_runs -- --ignored`; release builds 226 s for Linux and 184 s for Windows on first run, 19 s cached). Full check: fmt, clippy (default, no-default-features, gpu-tests), rustdoc, tests (lib 896, 1013 with gpu-tests; cli 34, 35 with gpu-tests) green.
- [x] Every demo project and template builds and runs headless. Done 2026-10-11: new ignored `tests/cli.rs` `every_template_and_sample_builds_and_runs_headless` creates every `ProjectTemplate::ALL` (15) with `rusting new --template`, then runs `rusting run <root> --ticks 120 --json` on each template and on the 13 sample games, asserting success and `headless_ticks` 120. It passed here (`cargo test --test cli every_template_and_sample_builds_and_runs_headless -- --ignored`, 171 s with the engine already built). Its first run found a bug: `forever_bear_booth` exited with `UnsavedTexture`, because the end-of-run save of `build/final.rscene` refused a mesh or texture made in code (its render-to-texture screens). That file is only read back by `scene query`, so the save now uses new `runtime::save_scene_state`, which keeps such an object without its `mesh_renderer`, as `scene_document_lenient` does; `save_scene` still refuses it. Evidence: unit test `runtime::scene_file::tests::a_state_save_keeps_objects_whose_mesh_was_made_in_code`. The engine examples under `src/examples` open a window, so they are built by clippy `--all-targets`, not run. Full check: fmt, clippy (default, no-default-features, gpu-tests), rustdoc, tests (lib 896, 1013 with gpu-tests; cli 34, 35 with gpu-tests) green.

### Determinism tests

- [x] Per-tick world-state hashes match between two runs with identical inputs. Done 2026-10-11: `runtime::state_hash::compare_runs` builds the same app twice, steps both one fixed tick at a time, and compares the latest `StateHashes` entry after every tick. In `runtime::tests::compare_runs_reports_the_first_divergent_tick_and_body`, three boxes fall and stack on a ground with `RandomSeed(7)`, and two runs report no divergence over 90 ticks. The same test proves the check is not blind: a 1e-6 m/s nudge, or a different seed, is reported. At the game level, `rusting determinism` compares every tick of separate runs, and all 13 samples matched over 300 ticks (see "Hashes match between debug and release builds."). No code change.
- [x] Hashes match between debug and release builds. Done 2026-10-11: new ignored `tests/cli.rs` `every_sample_hashes_alike_in_debug_release_and_on_one_cpu` runs `rusting determinism samples/<name> --ticks 300` on all 13 sample games. It passes the root as a relative path from the repository and uses `CARGO_TARGET_DIR=target/samples`. Each sample is built in debug and release and run headless, and release is also run under `taskset --cpu-list 0`. Every configuration gave the same state hash on all 300 ticks. The test passed here in 562 s. Writing the test found a bug, now fixed: with a relative root, `rusting determinism` and `rusting bisect` failed with `GAME_FAILED` (`No such file or directory`). The game runs in the project root, so the relative hash-report path was joined twice. `headless_state_hashes` now makes it absolute.
- [x] Hashes match across differing CPU worker-thread counts. Done 2026-10-11: the CPU solver splits the narrow phase and contact islands across rayon workers (`cpu_physics.rs`). Unit test `runtime::tests::a_box_pile_steps_the_same_on_one_and_many_workers` steps 290 boxes for 60 ticks: a 200-box heap and 30 separate three-box towers, one island each. It runs in rayon pools of 1, 4 and 7 threads and gets the same `world_state_hash` each time (passes, 4.9 s). At the game level, the ignored `tests/cli.rs` `every_sample_hashes_alike_in_debug_release_and_on_one_cpu` compares release runs on all 12 cores here with release runs pinned to one CPU by `taskset`, so bevy and rayon start one worker. All 13 samples give the same hash on all 300 ticks. No code change.
- [ ] Hashes match across at least two GPU vendors.
- [x] A recorded replay reproduces its original hash sequence. Done 2026-10-11: unit test `runtime::tests::replays_reproduce_recorded_hashes_and_find_changed_input` records 90 frames of a platformer with uneven frame times (16, 33, 5 and 50 ms), so some frames run no fixed step and some run several. Arrow and jump presses and releases happen during the recording. The replay holds one hash per fixed tick. It round-trips through JSON, and `play_replay` on a fresh scene reproduces every recorded hash (`None`). Dropping one jump press is reported at the first tick it can change, and a replay of an older format is refused with `UnsupportedVersion`. No code change.
- [x] An injected divergence is reported with its first divergent tick and body. Done 2026-10-11: in unit test `runtime::tests::compare_runs_reports_the_first_divergent_tick_and_body`, the second of two runs adds 1e-6 m/s to the "Middle" box during fixed tick 5. `compare_runs` reports tick 6, the first hash that includes the nudge, and names the "Middle" entity. A changed seed is reported at tick 1 with no entity. `divergent_entities_pair_by_scene_id_across_scene_revisions` pairs entities by scene id, so an added entity does not blame the wrong body. `rusting determinism` and `rusting bisect` use the same comparison on game runs. No code change.
- [x] Disabling audio, particles, and all presentation systems does not alter hashes. Done 2026-10-11: new unit test `runtime::tests::presentation_off_leaves_every_tick_hash_unchanged` drops three boxes with `BurstEmitter`s onto a ground and runs 120 fixed ticks twice. In the first run the boxes also have a `SoundCue`, a `SlideSound` and a `ParticleEmitter`, and the scene has a camera, a point light, a `ReverbZone` and a HUD element. The first run makes burst particles, particle systems and sound events. The second run has none of those components and despawns every burst particle and particle system after each tick, so it makes no particle and no sound event. Both runs record the same 120 `StateHashes`. The emitters stay in both runs because their trigger flags are simulation state (`docs/determinism.md`). The windowed runtime plays audio and renders outside the fixed step, so a headless run already has neither. Full check: fmt, clippy (default, no-default-features, gpu-tests), rustdoc, tests (lib 901, 1018 with gpu-tests) green.

### Networking tests

- [x] Full match completes at 200 ms latency with 2% packet loss without desync. Done 2026-10-11: unit test `net::tests::a_lockstep_match_finishes_at_200_ms_and_2_percent_loss_in_sync` plays a whole match on a loopback session. Every end has 200 ms one-way latency, 10 ms jitter and 2% loss. Two clients say hello, get their welcome and ready up in a `Lobby`, which starts the match with players 1 and 2. For 200 ticks, 10 ms apart, clients stamp one input per tick 60 ticks past the last command they ran, and the host feeds an `InputBuffer`. The host sends each tick's commands with `lockstep::send`, and the hash of the state before. Each client runs commands from a `CommandStream`, asserting the host's hash every tick. Both clients reach tick 200 with the host's exact state. About 135 inputs come fresh and about 225 come late, because TCP holds every message behind a lost one. Late inputs repeat on every peer alike, so no tick desyncs. Passes 5/5 runs, 3.2 s.
- [x] Delta snapshot encoding and decoding round-trip correctly. Done 2026-10-11: `net::replicate` encodes each delta against what was last sent and each snapshot against the client's last acknowledged one. Evidence: unit tests `deltas_spawn_update_and_despawn_quantized_objects` and `snapshots_encode_against_the_last_acknowledged_one` (spawn, update, component removal, despawn, a late joiner, a lost and a late snapshot, forged bases and acks), and new `random_edits_round_trip_through_lossy_snapshots_and_deltas`: 300 ticks of seeded random spawns, despawns, health inserts and removals and moves, sent as reliable deltas to one client and as snapshots to another with a third lost, some a tick late and half the acks dropped; after every applied message `Replica::verify` against the host's checksum is `Check::Match`. Full check: fmt, clippy (default, no-default-features, gpu-tests), rustdoc, tests (lib 892, 1009 with gpu-tests) green.
- [ ] Bandwidth stays within the per-client budget during maximum destruction.
- [x] Mid-match disconnect and reconnect resynchronizes correctly. Done 2026-10-11: unit test `net::tests::a_client_that_drops_mid_match_resynchronizes_on_rejoin` runs a TCP host with UDP snapshots at 20% simulated loss. A client syncs for 20 ticks (`Replica::verify` gives `Match`), then drops. The host sees `Disconnected(1)` and calls `Replication::forget`, and the match goes on: it despawns one object, spawns another, and moves everything for 10 ticks while the client's copy drifts locally. The client rejoins as peer 2, keeping its world and `Replica`. Its first snapshot is whole and replaces the stale state, so after 20 ticks the checksum matches again and the client holds exactly A, C and D at the host's positions. Passes 5/5 runs, 0.13 s.
- [x] Protocol version mismatch is rejected at connect time. Done 2026-10-11: every JOIN and HOST_ROOM starts with `PROTOCOL_VERSION`, and `check_version` answers anything else with a REJECT naming both versions before the peer gets an id. Evidence: unit test `net::tests::a_host_refuses_another_protocol_version`, extended: a direct host refuses version 99 and never reports the client as connected; a relay refuses a version-99 HOST_ROOM, a version-99 JOIN to a live room, and a JOIN too short to hold a version (`missing protocol version`), and the room's host never sees a refused client. Full check: fmt, clippy (default, no-default-features, gpu-tests), rustdoc, tests (lib 891, 1008 with gpu-tests) green.
- [x] A client reporting an impossible position or terrain state is rejected by the server. Done 2026-10-11: the host's `Replication::accept` now returns an error for a delta, snapshot, whole state or checksum a client sends; before, it ignored them (`None`). `docs/networking.md` "Trusting nothing a client sends" says so. Unit test `net::tests::the_host_rejects_positions_and_terrain_a_client_claims`: a modified client on a loopback session forges real replication messages of its own world, with the hero at 1e9 m. It also registers the host-only `break_chunk` (terrain) and `teleport` RPCs as open to anyone and calls them, and sends a NaN stick, a stick of 50 and an input 500 ticks ahead. The host runs the loop from the docs, and every forged message is refused with its reason: four "sent state", two "may not call", two out-of-range sticks and one "too far ahead". Only the one valid input reaches the `InputBuffer`, and the host's hero stays where the host put it, with no replication delta.

### Terrain and navigation tests

- [ ] Chunk fracture is deterministic across runs and vendors.
- [ ] Debris settling restores static collision and navigation.
- [ ] The debris budget cap never drops bodies non-deterministically.
- [ ] Unsupported spans collapse; supported spans do not.
- [ ] Navigation rebuild stays within its per-tick budget during a cliff collapse.
- [ ] Agents re-route correctly when a route is closed mid-path.
- [ ] Fully blocking every route is reported rather than producing stuck agents.
- [ ] One-hour destruction soak test holds frame time, memory, and debris count within budget.

### Agent-surface tests

- [x] Every diagnostic code is unique, registered, and has an `explain` entry with a tested example. Done 2026-10-11: new `diagnostics::tests::command_examples_name_a_real_command_and_its_flags` checks that every `rusting ...` example (each `&&` part) names a command from `schema::OPERATIONS` and only uses flags in that command's usage or the global ones; a `--bogus` flag fails it. Evidence: `codes_are_sorted_unique_and_fully_documented` (strict sort means unique; summary, fix and example present), `every_emitted_code_is_registered_and_every_registered_code_is_emitted`, `json_patch_examples_parse_as_patches`, and `explain_describes_a_code_and_suggests_similar_ones_for_a_typo` (explain lists every code). Full check: fmt, clippy (default, no-default-features, gpu-tests), rustdoc, tests (lib 893, 1010 with gpu-tests) green.
- [x] Every example in the schema catalog, API index, skill file, and generated `AGENTS.md` runs and passes. Done 2026-10-11: new `tests/cli.rs` `agent_guide_examples_run_in_a_new_project` applies every JSON patch block of `skills/rusting-game/SKILL.md` and `src/project_agents.md` to a new project's main scene with `scene patch --dry-run`, parses every scenario block as `Scenario` (they name a guide's example game, so they do not run), and runs every placeholder-free `rusting ...` command that writes nothing (36, from `cli::READ_ONLY_COMMANDS` plus `fix --dry-run`) in that project, asserting success; new ignored `agent_guide_games_build` builds each whole game in the guides (`rusting_game!(update)` blocks) as the project's `main.rs` with `rusting check`, passed locally (`cargo test --test cli agent_guide_games_build -- --ignored`, 49 s). Schema catalog: `schema_catalog_examples_parse_and_help_lists_every_operation` runs every operation example and `schema` tests load every component example through the scene parser. The API index has no code examples; `agent_guides_cover_every_scene_method_and_name_only_real_ones` checks the calls the guides make. Full check: fmt, clippy (default, no-default-features, gpu-tests), rustdoc, tests (lib 895, 1012 with gpu-tests; cli 33, 34 with gpu-tests) green.
- [x] The generated skill file and `AGENTS.md` match the current schema and API index. Done 2026-10-11: new `docs::tests::agent_guides_name_only_real_commands_codes_pages_and_schemas` checks every `` `rusting ...` `` span in `skills/rusting-game/SKILL.md` and `src/project_agents.md`: the command is in `schema::OPERATIONS`, its flags are in that command's usage (shared `schema::command_problem`, also used by the diagnostics example test), `explain` names a registered code, `docs show` a real page, and `schema` a catalog entry; a misspelled code, schema name, page id and flag in the AGENTS template each fail it. Evidence: `agent_guides_hold_their_generated_parts` (API table, component list and pitfalls are regenerated and compared), `agent_guides_cover_every_scene_method_and_name_only_real_ones`. Full check: fmt, clippy (default, no-default-features, gpu-tests), rustdoc, tests (lib 894, 1011 with gpu-tests) green.
- [x] JSON Schema files validate every scene, patch, and scenario in the repository. Done 2026-10-11: new `schema::tests::json_schemas_validate_every_scene_patch_scenario_and_project` walks the repository (skipping `target` and dot folders), sorts each `.json`/`.rscene` by its keys into scene (14), scene patch (20 files plus the JSON examples in `diagnostics::CODES`), scenario (93) and `project.json` (14), and checks each against `schema::json_schemas()` with a small in-test validator for the keywords those schemas use (type, const, required, properties, additionalProperties, items, minItems/maxItems, minimum/maximum, oneOf); a known-bad patch and a `"ticks": -90` edit to a benchmark scenario each fail it. No new dependency. Full check: fmt, clippy (default, no-default-features, gpu-tests), rustdoc, tests (lib 895, 1012 with gpu-tests) green.
- [x] Every sample passes `rusting lint`, its invariants, a 100-seed fuzz run, and the scenario strength check. Done 2026-10-11: each of the 13 game samples has a new `tests/invariants.json`. It runs 600 ticks of random moves from the sample's own actions (scenario `fuzz.actions`, which `rusting fuzz` now keeps unless `--action` is given). It checks that the player stays finite and within height bounds and that counters stay in range. New `tests/cli.rs` `every_sample_has_invariants_to_fuzz` checks that every sample has one, that it parses and passes, and that each invariant names a real scene entity (a misspelled entity fails it). It also checks the table of strength scenarios. New ignored `samples_pass_lint_scenarios_fuzz_and_strength_check` runs the whole item per sample. Run here one sample at a time at low priority: `lint` is clean and every scenario passes (`rusting test`) in all 13. `fuzz tests/invariants.json --seeds 100` passes all 13 (39-85 s each). `test <scenario> --without <component>` fails as it must (`SCENARIO_TOO_WEAK` when removing the component does not break the scenario) for 12. forever_bear_booth has no strength check: its game code alone drives what its scenario checks, and removing `camera_screen`, `parent`, `visible` or `mesh_renderer` all left it passing. Fixes found: forever_bear_booth and tower_topple let the player or blocks fall off an open floor edge, so their floor checks became a ceiling check and a finite check. sky_hop `clear_level.json` releases at tick 374 instead of 365: a player standing on both the ledge and the moving Lift is now carried by the Lift (physics behaviour change, reported to the physics lane). Full check: fmt, clippy (default, no-default-features, gpu-tests), rustdoc, tests (lib 895, 1012 with gpu-tests; cli 34, 35 with gpu-tests) green.
- [x] CLI, daemon, and MCP adapter return identical results for the same operation. Done 2026-10-11: new `tests/cli.rs` `cli_serve_and_mcp_give_the_same_envelope_for_each_operation` runs nine operations (validate, lint, project inspect, scene inspect, scene query, a failing scene inspect, explain, schema, asset list) in `samples/putt_course` through `rusting ... --json`, one `rusting serve` session and one `rusting mcp .` session, and asserts the three envelopes are equal and MCP `isError` matches `ok`. Full check: fmt, clippy (default, no-default-features, gpu-tests), rustdoc, tests (lib 893, 1010 with gpu-tests; cli 32, 33 with gpu-tests) green.

### CI matrix

- [ ] Linux software Vulkan runner (lavapipe) running the `gpu-tests` feature on every pull request. This is the primary GPU verification path; hardware runners confirm it, they do not replace it.
- [ ] Linux hardware runner.
- [ ] Windows hardware runner.
- [x] Formatting, strict clippy, unit tests, and docs on Linux and Windows for every pull request.
- [ ] Scheduled validation and performance runs with stored artifacts.
- [ ] Second-vendor GPU runner dedicated to cross-vendor determinism verification.
- [ ] Scheduled soak-test job for long-running destruction and memory growth.
- [ ] Scheduled headless batch-match job producing aggregate balance reports.
- [ ] macOS (MoltenVK) runner.
- [ ] Android build job.
- [ ] Scheduled comparative physics benchmark job publishing its report as an artifact.
- [ ] Scheduled agent benchmark job (Milestone L9) publishing its report as an artifact.

## Cross-cutting engineering rules

- Public fallible operations return structured `Result` values; panics are reserved for internal invariant violations.
- ECS entities and typed asset handles are the only canonical identities exposed to gameplay/editor code.
- GPU resource destruction is deferred until all referencing frames complete.
- No fixed-capacity structure may silently drop work.
- Optional Vulkan features always have a documented fallback or a clear startup error.
- Debug builds enable validation by default when layers are available.
- Runtime and editor code must expose useful diagnostics instead of relying on ad-hoc FPS/debug printing.
- Every milestone must leave the project compiling and its completed acceptance gates automated where practical.
- Simulation state and presentation state are separate. Presentation code may read simulation state and may never write it.
- Anything affecting simulation must be deterministic: no wall-clock time, no frame-rate dependence, no unseeded randomness, no order-dependent accumulation.
- Network code never trusts a client for authoritative state.
- Systems that run for hours need soak tests. Correct for one minute and degrading over one hour is not correct.
- Anything that moves things — animation, particles, characters, vehicles, navigation obstacles, audio occlusion — integrates with physics through the typed bridge rather than a parallel ad-hoc simulation.
- Physics features are judged against the best standalone physics engines, with benchmark evidence. Other features are judged against Godot parity.
- Every user-facing feature ships with documentation and a demo scene in the same change or the next one.
- Every user-facing feature is agent-ready when it lands: a schema entry with defaults, units and an example; a Rust API index entry; diagnostic codes for its errors; and a way to observe it in a scenario, trace, or capture.
- Every error tells the reader what to do next. An error that only says what went wrong is incomplete.
- Nothing in the engine calls an AI model. Agent support is tools, formats, and documentation that work offline and without any account.

## Recommended implementation order

Work on one vertical path at a time rather than creating empty crates for every eventual subsystem.

Foundation (Milestones 0-7):

1. Finish Vulkan/winit modernization and validation.
2. Add the workspace plus `rusting-core`, ECS schedules, components, and compatibility facade.
3. Add typed assets and static scene serialization.
4. Add render extraction and correct frames-in-flight synchronization.
5. Complete the forward PBR pass schedule and profiling.
6. Build the self-written hybrid physics bridge: stable IDs, GPU events, CPU commands, selective readback, and mixed CPU/GPU ownership.
7. Add the minimal egui shell, viewport, hierarchy, and inspector.
8. Build the vertical slice while filling in editor, rendering, asset, and physics gaps.
9. Add hot reload, undo/redo, polish, packaging, and performance gating.

Physics leadership and Godot parity (Milestones 8-23):

10. Establish determinism: simulation math, ordering rules, per-tick state hashing, and the verification harness. Every later solver depends on these rules.
11. Add reflection, prefabs, observers, data resources, and Rust hot reload. Every later editor and animation feature depends on reflection.
12. Complete rigid-body physics: joints, articulations, characters, vehicles, ragdolls, fields, CCD, and 2D physics.
13. Build the physics debugger and the comparative benchmark harness early, so every later solver is measured from its first commit.
14. Add deformables, fluids, granular media, fracture, and million-body GPU scale.
15. In parallel, reach parity in advanced rendering, animation, audio, runtime UI, 2D, and navigation, integrating each with physics as it lands.
16. Add networking with prediction and deterministic rollback.
17. Grow editor parity, platforms/export, and documentation continuously, closing them out at the 1.0 release gate.

LLM-native development (Milestones L1-L10), in parallel with the above and first while the current focus holds:

- Machine-readable surface and diagnostic codes (L1), then observability (L3) and verification (L5), since these remove guesswork from every later task.
- Start the agent benchmark (L9) as soon as L1 lands, with the tools that exist, so later milestones are measured from their first commit.
- Then the generation-friendly API (L2), the daemon and editor bridge (L4), quality lints and recipes (L6, L7), the fast logic lane (L8), and large-project support (L10).

Engine expansion (Milestones 30-35), after Milestone 9: gameplay framework (30) first, since every sample needs it; then terrain and large worlds (31), procedural content (33), cinematics (32), modding (34), and XR (35).

Sundering (Milestones 24-29):

18. Add competitive authority on top of the engine networking, keeping clients as renderers until determinism is proven.
19. Build chunked destructible terrain, structural load, and Scar persistence on the engine fracture system.
20. Add navigation and GPU crowd agents over terrain that changes every tick.
21. Add the competitive gameplay framework: teams, abilities as forces, dynamic vision, and bots.
22. Build the Sundering slices in order, adding balance and operations tooling as each slice requires it.

The next concrete editor task is a Shortcuts settings panel. It must display the central action map, let a user rebind one action at a time, detect duplicate bindings, and persist choices inside project editor settings. Then add focus-selection and interactive translate/rotate/scale handles. Explicit frame contexts and an offscreen scene viewport target remain the next renderer-architecture task.

### Review-fix pass verification

- fmt, clippy (default, `--no-default-features`, `--features rusting_engine/gpu-tests`) clean.
- `cargo test --workspace`: 457 lib tests pass; with `rusting_engine/gpu-tests`: 533 pass, 0 failed.
- New tests: fixed GPU box solid to CPU queries, switched-off fluid surface reaped, scene `InputAction` unbinds, mistyped scene setting errors, huge fluid block cap.

### Look and feel track

Goal: games made by agents look less cheap. Items A to G from the 2026-10-04 audit.

- [x] A. Mipmaps and anisotropic filtering. Already present: textures upload with a full mip chain and linear minification filters anisotropically when the device enables `samplerAnisotropy`. Only the stale comment above `texture_sampler` changed.
- [x] B. Polish loop and art checklist. `docs/look-and-feel.md` (`rusting docs guide/look-and-feel`) covers the capture, look, critique and fix loop, plus a checklist for silhouette, palette, value contrast, warm key against cool fill, materials, shapes, HUD, and motion and feedback. `skills/rusting-game/SKILL.md` gained a Polish step, and the project `AGENTS.md` template mentions the loop.
- [x] C. Color grading and palettes. `rusting.color_grading` (contrast, saturation, shadow and highlight tints, vignette) runs after tone mapping. Presets: `dark_interior` and `bright_stylized` are new, every preset applies color grading, and `preset list --json` prints each preset's five-color palette. The new component is in the schema catalog and the editor's Environment components. Tests: `hdr_values_above_one_survive_until_tone_mapping` (grading assertions, GPU) and the `art_direction` tests.
- [x] D. Mesh kit. New `RoundedCube` and `Capsule` primitives. Cylinder and cone are now smooth shaded, built on a shared lathe. The sphere was inside out and now winds outward, which also fixes lighting on ball pits and other sphere crowds. The guide has a ready humanoid patch (`Hero`), checked by capture. Tests: `primitives_wind_outward_with_unit_normals` (CPU) and `every_primitive_shows_its_outside_to_the_camera` (GPU, Normals debug view).
- [x] E. Free models. The guide lists CC0 sources (Kenney, Quaternius, Poly Pizza, Poly Haven, ambientCG), license rules, and `asset import --license/--author/--url`. New `rusting scene add-model <scene> <model> [--name N] [--dry-run]` places a glTF as one named parent object, with a child per node and the model's materials. The `MODEL_IMPORT` diagnostic covers import failures. A house model CDN is noted as planned and does not exist yet. Tests: `a_gltf_model_lands_in_a_scene_under_one_named_object` and the schema and CLI catalog tests. Checked end to end: import, add-model, validate and capture.
- [x] F. Render to texture. `rusting.camera_screen` (`camera` name, `size`) on a mesh shows that camera's view in place of the material's base color and emissive maps. `SceneRenderer::render_screens` draws each feed with its own child renderer before the frame. `render_game` (game window and captures) calls it. Screen materials batch alone. Tests: `screens_find_their_camera_by_name` (CPU) and `a_screen_material_shows_what_its_camera_sees` (GPU). Checked by a `rusting capture` of a monitor showing a second camera's side view. Limits, also listed in `docs/cameras.md`: one full child renderer per screen, no screens inside feeds, no feeds in the editor viewport, and no egui images.
- [~] G. Small distant objects and noisy crowds. Partial. The sphere winding fix made the `w4_cctv` ball pit lit correctly, and the guide warns against random colors in crowds. LOD groups (`.rlod`) already exist. Deferred: TAA and impostors.
- Full check for this track: fmt and clippy (default, `--no-default-features`, `--features gpu-tests`) clean. `cargo test --workspace` passed, lib 532. `cargo test --workspace --features rusting_engine/gpu-tests` passed: lib 616, cli 25, doctests 17. A first run of that suite was stopped after 30 minutes with no output. Every test binary then passed separately in seconds, and the full rerun took a few minutes, so the stall was not reproduced.

### Effects and animation track

Goal: games feel alive. Particles, effects and animation, each with editor GUI. Items 1 to 7 are from the 2026-10-04 brief.

- [x] 1. Particle emitter v2. `rusting.particle_emitter` (`src/runtime/particles.rs`) has these settings:
  - Emission: rate, bursts, max particles, prewarm, duration and looping.
  - Shape: point, box, sphere, cone or circle, with a size.
  - Random `[min, max]` ranges for lifetime, speed, size, rotation and spin.
  - Motion: direction and spread, gravity, drag, wind and turbulence.
  - Over life: size curve, color and alpha gradient, emissive, fade in and fade out.
  - Rendering: world or local space, billboard or velocity streak, alpha or additive blend, and a soft, disc or square sprite.

  Live particles sit in a `ParticleSystem` component next to the emitter, never one entity per particle. The renderer draws each emitter in one instanced draw. Alpha batches and their particles are sorted far to near; additive batches draw last. Random values come from `RandomSeed` keyed by the emitter's `SceneId` and spawn number and indexed by the fixed tick. Game code uses `scene.particles(name, ParticleCommand::…)`, and `scene.trigger` restarts the emitter. `rusting.burst_emitter` is unchanged. The component is registered in schema, reflect, snapshot and scene_file, and has editor placement help. Docs: `docs/effects.md` (`guide/effects`), `api/ParticleEmitter`, the look-and-feel checklist, SKILL.md and the project AGENTS template.
  - Tests: 9 simulation tests in `runtime::particles::tests` (rate and lifetime, seeding, bursts and max, looping bursts, shapes, spread and gravity, curves and fades, local space, prewarm), plus the GPU test `particles_render_with_their_color_and_size` (coverage, alpha color, additive sum).
  - Full check: fmt and clippy (default, `--no-default-features`, `--features rusting_engine/gpu-tests`) clean. `cargo test --workspace` passes with 541 lib tests; with gpu-tests, 626 lib tests and 25 CLI tests pass.
- [x] 2. Effect presets. `rusting effect list` and `rusting effect apply <scene> <effect> [--on OBJECT | --name N --at X,Y,Z] [--dry-run]` (`src/runtime/effect_presets.rs`) add these presets:
  - Ambient: dust_motes, fireflies, magic_sparkle.
  - Weather: falling_leaves, snow, rain.
  - Fire: fire, embers, smoke.
  - One-shot bursts: sparks, confetti.

  The emitter gained `start_colors`, a per-particle tint picked from a list, for confetti and mixed leaves. The new commands are in the schema catalog, with the `EFFECT_UNKNOWN` diagnostic. Docs: the preset table in `guide/effects`, SKILL.md, the project AGENTS template and look-and-feel.
  - Tuning: each preset was captured with `rusting capture` (640x360, tick 120, or tick 12 to 20 for bursts) over a night or daylight ground scene, and the images were reviewed. Fixes from that review:
    - Emissive was 5 to 8, which clipped additive colors to white. It is now 1 to 2.5, so sparks, embers and fireflies keep their hue.
    - Fireflies, embers and sparkles were too small to read at 7 m, so they are bigger.
    - Leaves are now dark square tumbling sprites, not bright discs.
    - Rain is thinner and more transparent.
    - Fire is wider and softer, with low alpha, so it reads as a flame instead of a white blob.
  - Tests: `presets_have_unique_names_and_round_trip_as_json`, `start_colors_tint_each_particle_with_one_of_them`, CLI `effect_presets_list_and_apply_as_scene_patches`.
  - Full check: fmt and clippy ×3 clean. `cargo test --workspace` passes with 543 lib tests; with gpu-tests, 628 lib tests and 26 CLI tests pass.
- [x] 3. Particle editor GUI. The Inspector draws `rusting.particle_emitter` with its own editor (`src/editor/inspector/particles.rs`), built from the shared inspector widgets and `EditorTheme` colors:
  - A preset dropdown over `EFFECT_PRESETS`, and Play, Pause, Restart and Stop buttons with the live particle count.
  - Collapsible sections: Emission (with a bursts list), Shape, Lifetime, Velocity, Size, Color and Rendering.
  - A min–max range row. Rotation and spin show in degrees.
  - A size-over-life curve plot: click to add a key, drag a dot to move it, and edit or remove keys in the rows below.
  - A color-over-life gradient bar over a checker: click to add a key with the shown color, drag a marker to move it, with color rows below.
  - A start-tint list.

  Setting edits are `ComponentEdit::Set`, so a drag or click closes into one snapshot Undo step. Transport buttons set `EditorState::particle_command`, which is applied to the live emitter and is not a scene edit. Live preview: `update_edit_preview` (called by the editor binary before each runtime update) steps emitters in Edit mode through `runtime::preview_particles`, with its own fixed-step clock capped at four steps a frame. The editor redraws continuously while a preview plays. Starting Play calls `reset_particles`, so the game's emitters start fresh. `update_particles` and the preview share one `advance` function. Deferred work (a standalone particle area, key box selection, a shape gizmo, project presets) is in `docs/editor-overhaul.md`. Docs: "In the editor" in `guide/effects`.
  - Visual check: an offscreen render of the Inspector with the fire preset was reviewed. It used a temporary capture test that was then removed.
  - Tests: `particles::tests` covers sorted key insertion that keeps the curve, presets replacing every setting, and every section and the transport drawing. In `editor::tests`, `particle_inspector_edits_are_one_undo_step_each` clicks "+ Add Burst" through egui input, checks one Undo step, then undoes. `particles_preview_while_stopped_and_transport_is_not_an_edit` checks Edit-mode preview, Pause through the Inspector with no Undo and no dirty scene, and that Play resets the preview and stops it.
  - Full check: fmt and clippy ×3 clean. `cargo test --workspace` passes with 548 lib tests; with gpu-tests, 633 lib tests and 26 CLI tests pass.
- [x] 4. Keyframe animation. `rusting.animation` (`src/runtime/animation.rs`) holds named clips, with `autoplay` and `speed`:
  - A clip has `duration` (0 uses the last key), `repeat` (Once, Loop or PingPong, the `TweenRepeat` enum), tracks and `{time, name}` event markers.
  - A track targets the entity or a child name path (`Arm/Hand`). It writes Position, Rotation, Scale, Color, Emissive, Visible, or `Field {component, path}`, which is any numeric field of a registered component through `set_registered_component_field`.
  - Interpolation is Step, Linear or Smooth (Catmull-Rom).

  An `AnimationPlayer` component is added next to each animation. It holds the current clip, the fading clip, and the material copies it owns. Color and emissive tracks give the target one material copy, then edit it in place only when the value changes, so the renderer re-uploads only on change. The exclusive system `advance_animations` runs on FixedUpdate after `advance_tweens`. It steps by `fixed_delta`, applies writes in (SceneId, entity) order, and sends `AnimationEvent{tick, entity, object, clip, name}` in that order, sorted by time within a tick. Markers fire on half-open windows; a Once clip's end is closed. Crossfade lerps tracks that match on target and property. Game API: `play_animation`, `crossfade`, `stop_animation`, `set_animation_speed`, `is_playing`, `animation_events`. `step_animations`, `apply_preview_pose` and `find_target` are public for the editor timeline. The component is registered in schema (with a round-trip example), reflect, snapshot (component, player and event queue), scene_file, placement help and `api/Animation`. Docs: new `docs/animation.md` (`guide/animation`), SKILL.md, the project AGENTS template and look-and-feel.
  - Tests: 8 in `runtime::animation::tests`: interpolation modes, repeat mapping, autoplay with Once, markers once per pass, crossfade with Stop holding the pose, a color copy with no re-upload when unchanged, child target visibility, and a bitwise-equal pose over two runs. The App test `animations_run_on_the_fixed_step_and_send_marker_events` covers the FixedUpdate chain, a Field track on `rusting.tween /duration`, and event delivery through `EventQueue`.
  - Full check: fmt and clippy ×3 clean. `cargo test --workspace` passes with 557 lib tests; with gpu-tests, 642 lib tests and 26 CLI tests pass.
- [x] 5. Editor animation timeline. `EditorPanel::Timeline` (`src/editor/timeline.rs`) is a Blender-style dope sheet for the selected object's animation, or its nearest animated parent:
  - The header has a clip picker, add, delete and rename, repeat, length (auto), Autoplay, ⏮/Play/Pause/Stop, the time, ● Record, an Insert Key menu (Location, Rotation, Scale, All), Delete Key and + Event.
  - The body has a ruler that scrubs, event markers, one row per track with delete and an interpolation cycle, diamond keys that select and drag (snapped to 1/60 s, re-sorted), and the playhead.
  - Clip edits return as `ComponentEdit::Set` on `rusting.animation`, so they coalesce into one snapshot Undo step. Add Animation is a `ComponentEdit::Add`.
  - `update_timeline` runs after edits in Edit mode while a Timeline area is shown. It poses the subtree with `apply_preview_pose` (transform and visibility only) when the clip, time or clip data changes. The authored pose is kept in the runtime resource `PreviewRestPose`, which `scene_document` writes instead of the posed values, so saves and Undo snapshots never contain a scrub. The rest pose is put back when the selection leaves the animation and before Play. Undo/Redo drop it and re-pose.
  - Record mode compares each posed transform to what the pose wrote. A changed position, rotation or scale keys only that channel at the playhead, at the object's child name path. The gizmo or Inspector edit already took the Undo snapshot, so the key undoes with it. With Record off, the edit updates the rest pose. Playback requests continuous redraw.
  - Deferred items (zoom and pan, marker drag, box select, curve editor, color and field preview, scoped Delete key) are in `docs/editor-overhaul.md`. Docs: "In the editor" in `docs/animation.md`.
  - Tests: in `editor::tests`, `timeline_adds_animation_and_keys_as_one_undo_step_each` clicks + Add Animation and Insert Key › All through egui input, checks two Undo steps, then undoes the keys. `timeline_scrub_poses_but_scene_documents_keep_the_rest_pose` checks a posed x = 1 at 0.5 s, a document x = 0, no Undo step, and restore when the selection is cleared. `timeline_record_mode_keys_transform_edits_at_the_playhead` checks one Position key [1, 3, 0] at 0.5 s, no rotation or scale tracks, and a dirty scene.
  - Full check: fmt and clippy ×3 clean. `cargo test --workspace` passes with 560 lib tests; with gpu-tests, 645 lib tests and 26 CLI tests pass.
- [x] 6. glTF node animation import. `AssetServer::import_gltf_animations(path, &nodes)` (`src/assets/mod.rs`) reads every glTF animation into an `AnimationClip` (Loop, auto length) for a root whose children are the glTF top nodes:
  - Translation and scale channels become Position and Scale tracks. Rotation becomes a track of the new `AnimationProperty::Orientation` (quaternion `[x, y, z, w]`), with the sign of each key flipped to the shortest arc from the previous key. `apply` normalizes the blended quaternion and writes Euler `Rotation`, so imported spins do not flip at ±180°.
  - STEP, LINEAR and CUBICSPLINE map to Step, Linear and Smooth. Cubic spline keys keep the value and drop the tangents. Morph target weight channels are skipped.
  - Tracks target the node name path. `scene add-model` and the editor's `add_model_to_scene` put the clips on the new root as `rusting.animation`, and the first clip autoplays. The editor maps renamed (de-duplicated) node names into the paths.
  - `add_model` now builds its Create operations with `scene_patch::entity_form`, so registered components go in as JSON objects. Before, the JSON strings failed validation; no model had a registered component until now.
  - Sample: `samples/vertical_slice/windmill.gltf` (`generate_windmill.py`, standard library only) is a tower with a Rotor (5 linear quarter-turn rotation keys) and a Flag (3 cubic spline translation keys). Docs: "Imported clips" and the Orientation row in `docs/animation.md`, the add-model step in look-and-feel, the sample README, SKILL.md, the project AGENTS template and the schema summary.
  - Tests: `add_model_keeps_gltf_node_animation_as_a_clip` (`tests/vertical_slice_environment.rs`) runs `cli::add_model` into a saved scene, reloads it, and checks autoplay "spin", a length of 4 s, tracks (`Tower/Rotor` Orientation ×5, `Tower/Flag` Position ×3), Smooth with the cubic value [0.5, 2.0, 0], and the rotor at π/2 after 1 s of fixed ticks. `editor::tests::added_model_clips_follow_renamed_nodes` adds the windmill twice and resolves the second copy's renamed rotor path.
  - Full check: fmt and clippy ×3 clean. `cargo test --workspace` passes with 561 lib tests; with gpu-tests, 646 lib tests, 26 CLI tests and 3 vertical slice tests pass.
- [~] 7. Skeletal skinning. Done on the CPU; GPU vertex skinning is not done (plan below).
  - Import: `import_gltf` reads `JOINTS_0` and `WEIGHTS_0` into `SkinWeights { joints: Vec<[u16; 4]>, weights: Vec<[f32; 4]> }`, cooked with bincode next to the mesh as `.rskin`. When the importer flat-shades a mesh with no normals, the weights are expanded per triangle corner the same way. `import_gltf_scene` reads each node's skin (joint node indices, and inverse bind matrices, identity when missing) into `ImportedGltfNode::skin`.
  - `rusting.skin` (`src/runtime/skinning.rs`) is `Skin { joints, inverse_bind }`. Each joint is a name path from the skinned object, with `..` for the parent (`find_target` now accepts `..`), so it survives saving and reparenting the model root. `gltf_node_skin` builds the paths from the current node names. `spawn_gltf_nodes_in_world` inserts it, and the editor's Add to Scene rebuilds it after de-duplicating names, like the clip paths. Nodes with no shared ancestor meet at the spawn root. Registered in scene_file, reflect, snapshot, schema (with example), placement help and `api/Skin`. The Assets panel hides `.rskin` like other cooked files.
  - Runtime: `update_skins` runs at the start of `extract_render_world`, so it covers the game and the editor (Timeline scrubs of joints included). For each skinned object it computes `inverse(object global) × joint global × inverse bind` per joint. When any matrix changed, it bends the cooked mesh on the CPU with linear blend skinning (weights normalized, normals by the inverse transpose, tangents by the linear part) into a private mesh copy held by the unsaved `SkinnedMesh` component. Render extraction draws that copy in place of `MeshRenderer.mesh`, so saves keep the cooked mesh. Later frames rewrite the copy through `Assets::get_mut`, so the renderer re-uploads it, without re-collecting renderables. A missing `.rskin` draws the mesh unbent.
  - Skeletal clips: joints are glTF nodes, so item 6 already imports their channels as Orientation, Position and Scale tracks on joint paths. No extra code.
  - Sample: `samples/vertical_slice/bending_bar.gltf` (`generate_bending_bar.py`) is a 2 m bar with three vertex rings: Root weights, a 50/50 split, and Tip weights. Its "bend" clip turns Tip a quarter turn around Z in 1 s. Docs: "Skinned meshes" in `docs/animation.md`, look-and-feel, SKILL.md, the project AGENTS template and the sample README.
  - Tests: `runtime::skinning::tests::joints_bend_vertices_and_normals_in_the_mesh_space` checks a `../Bone` path, glTF bind-space placement, weight normalization, normal rotation, and that a vertex with no weight does not move. `add_model_keeps_gltf_skin_and_bends_the_mesh` (`tests/vertical_slice_environment.rs`) runs `cli::add_model`, reloads the scene, and checks the joint paths `../Root` and `../Root/Tip`. It then plays the clip and checks that the drawn copy folds to x ≈ −1 while the cooked mesh stays within ±0.1.
  - Test `editor::tests::added_model_skin_joints_follow_renamed_nodes` adds the bar twice in the editor and checks that the second copy's renamed joint paths resolve.
  - Full check: fmt and clippy ×3 clean. `cargo test --workspace` passes with 563 lib tests. With gpu-tests, 648 lib tests, 26 CLI tests and 4 vertical slice tests pass. One earlier gpu-tests run stalled in `vertical_slice_lighting_combines_shadow_point_sky_and_blend` under parallel lavapipe load. That renderer-only test does not touch skinning. It passed alone in 0.44 s, and two later full runs passed.
  - Follow-up batch (2026-10-04), done:
    - Extra primitives of a skinned node. `insert_gltf_skins` gives every child part the node's skin with each joint path prefixed by `../`; the editor's Add to Scene calls the same function. Test: `assets::tests::every_primitive_of_a_skinned_node_gets_the_skin`.
    - Morph targets. `import_gltf` cooks a primitive's targets to `.rmorph` (positions and normals, expanded per corner on the flat-shaded path). `rusting.morph` is `Morph { weights }`, taken from the node or mesh default weights; child parts read their parent's weights. glTF `weights` channels import as a Field track on `rusting.morph` `/weights`. `update_skins` applies the weighted offsets first (normals renormalized), then skinning. Sample: `samples/vertical_slice/squash_box.gltf` (`generate_squash_box.py`), a box and hat with "squash" and "lean" shapes and a "wobble" clip. Docs: "Blend shapes" in `docs/animation.md`. Test: `add_model_keeps_gltf_morph_weights_and_reshapes_every_part` (both parts move with the weights after 60 ticks).
    - Memory. Deformed copies are tracked in `AssetServer` and freed with `Assets::remove` when the skin and morph go, the renderer mesh changes, or the entity despawns. A `SkinnedMesh` restored from a snapshot without a tracked copy is dropped and rebuilt. Test: `runtime::skinning::tests::copies_are_freed_when_the_skin_goes_or_the_entity_despawns`.
    - Editor skeleton overlay. `overlay::bone_shapes` gives each joint of a visible skin a small cross and a line from its parent joint. The Scene View draws them in front of meshes (cyan, selection colors when selected), and a click near a bone selects the joint before any mesh. Test: `editor::tests::added_model_skin_joints_follow_renamed_nodes` now also checks 4 joints for two bars and a Tip bone line. Not checked visually.
    - Full check: fmt and clippy ×3 clean. `cargo test --workspace` passes (565 lib tests). With gpu-tests: 650 lib, 26 CLI, 5 vertical slice, 17 doc tests pass.
  - Not done, in order:
    1. Done 2026-10-11: GPU skinning. With the `GpuSkinning` resource (inserted by the editor and the windowed game runner; headless renders keep the CPU path as the reference), `update_skins` keeps the copy's cooked vertices and `skin_jobs` lists each copy's joint matrices and shape weights in `RenderWorld::skin_jobs`. `SceneRenderer::prepare_skinning` uploads the cooked vertices, joints, weights and blend shapes once per copy (`rendering::gpu_skinning::SkinInputs`, keyed by copy, rebuilt when the cooked revision changes) and `skinning.comp` writes the morphed then skinned vertices into the copy's prepared vertex buffer (now also a storage buffer) before culling, so forward, prepass, shadow and reflection passes all draw the bent mesh with no shader variants. Verification: `gpu_skinning_draws_what_cpu_skinning_draws` renders a quad with two joints, uneven weights and a blend shape on position and normal (normals shown as color) through the CPU path and the GPU path; at most 8 channels differ by more than 2 while the unbent quad differs in over 100. `gpu_skinning_keeps_the_copy_cooked_and_lists_a_job` checks the copy stays cooked with an unchanged revision and the job list, and that removing the resource returns to CPU bending. Full check: fmt and clippy ×3 clean; `cargo test --workspace` passes (891 lib); with gpu-tests 1033 lib, 32 CLI, 5 vertical slice and 18 doc tests pass. Not checked visually in the editor window.
    2. Skinned bounds. GPU-bent copies are never frustum culled (bounds dropped); a box per joint or a padded bind-pose box would let them cull. Picking still tests the unbent shape.

### FOREVER BEAR track

Goal: the engine features the horror game FOREVER BEAR needs (night-shift booth, six CCTV monitors, about 2,000 shelf bears with pitched voices, a mascot that hunts by sound). Gap list from reading `src/runtime/audio.rs`, `docs/audio.md`, `docs/cameras.md`, `docs/effects.md`, `docs/look-and-feel.md`, CHANGELOG 2.0.3 and this file on 2026-10-06. Audio stays presentation only: no audio state feeds the simulation or the replay hashes.

- [x] 1. Sound rate. `rate` on play, `set_sound_rate(id, rate, fade)`, `/playing` reports the rate. Tests: `half_rate_doubles_the_end_tick` (unit), scenario `rate_pause_resume_and_seek_show_in_playing` (rate 0.5). Docs: "Speed and pitch" in `docs/audio.md`.
- [x] 2. Moving 3D sounds and the listener. `set_sound_position`, sounds attached to an entity, an explicit listener entity. Tests: `spatialize_pans_to_the_side_and_falls_off_past_two_metres`, scenario `an_attached_sound_pans_as_its_entity_moves_and_the_listener_can_move`. Docs: "Moving sounds and the listener".
- [x] 3. Bus effects with kira 0.12 only: `set_bus_effect(bus, LowPass | Reverb | Distortion, fade)`, shown in the audio report. Tests: `bus_voice_limits_clamp_and_inactive_effects_report_off`, scenario `bus_effects_show_change_the_mix_and_leave_state_hashes_alone` (state hashes equal with and without the effects). Docs: "Bus effects".
- [x] 4. Voice limits and `priority: u8`. Scenario `a_thousand_sounds_in_one_tick_stay_within_the_voice_limit`: 1,001 requests, 32 voices, 968 dropped and 1 stolen, no panic. Docs: "Voice limits".
- [x] 5. Pause, resume and seek, with the position in `/playing`. Scenario `rate_pause_resume_and_seek_show_in_playing` pauses at 2.0 s and resumes from 2.0 s. Docs: "Pause, resume and seek".
- [x] 6. Camera screen cost. `rusting.camera_screen` gained `update_every`, `enabled` and `grading`; screens out of every view frustum keep their last image; a new feed always draws once. Root cause of the cost: every feed re-ran GPU physics; feeds now reuse the frame's prepared physics (GPU bodies in feeds lag one frame). GPU test `a_screen_material_shows_what_its_camera_sees` covers update_every 2, enabled false, off-screen, and a wall of six monitors (six on the first frame, then three per frame). Numbers (RTX 3060, `render_bench` balanced 1920x1080, 600 frames, mean / p95): six 320x180 screens 134.91 / 151.07 ms before, 24.86 / 26.48 ms after; base scene 22.18 / 24.54 ms. Docs: "Keeping screens cheap" in `docs/cameras.md`.
- [x] 7. CRT/VHS look. `ColorGrading` gained `grain`, `chromatic_aberration`, `scanlines`, `color_bleed`, `noise_band` and `distortion`, for the scene and per screen. Grain and the band are a hash of pixel and fixed tick, so the same tick gives the same image. Effects that sample neighbours copy the HDR image first. GPU test `a_screen_feed_takes_a_crt_look_with_repeatable_grain` with goldens `screen_crt_off.png` and `screen_crt_on.png`, the same tick identical and the next one different, and the scene grading repeatable. Docs: "Film and CRT/VHS effects" in `docs/look-and-feel.md`.
- [x] 8. Many instanced objects. Scaling holds at 60 fps without a new path, so the per-instance custom value was skipped. `render_bench` gained `--bears`, `--bear-updates`, `--screens`, `--screen-size`, `--screen-every`, `--no-bodies` and `--extent`. Without GPU bodies: 5,000 bears 7.40 / 8.20 ms; 5,000 with 500 moved per frame 8.81 / 9.36 ms; 10,000 with 500 moved 12.37 / 13.38 ms; 5,000 + 500 moved + six screens 16.47 / 18.96 ms, 12.37 / 13.47 ms with screens every other frame. Scenario test `five_hundred_of_five_thousand_bears_move_in_one_tick`. Docs: "Crowds of one object" in `docs/look-and-feel.md`.
- [x] 8a. The benchmark scene's 1,000 GPU bodies cost about 18.6 ms of GPU time per frame on the RTX 3060 (22.18 ms with them, 3.06 ms without). Profile the GPU physics pass. Cause: pass timestamps put 20.83 of 23.27 ms GPU time in Physics, and dropping `CONTACT_ROUNDS` to 0 left 2.6 ms. The contact grid counters were small (about 80,000 fallback tests a frame). Skipping the oversized-body loop alone brought the frame to about 4 ms. The ground body is too big for one cell, so a single GPU thread tested all 1,000 bodies one after another, in each of the four contact rounds. Fix: `physics_contacts.comp` gained `OVERSIZED_PASS`. The grid pass files oversized bodies in their own list; the contact pass writes an indirect dispatch of one workgroup per body (capped at 65,535, the rest loop). Each workgroup splits the bodies across 256 threads and merges max, min and the touch flag in shared memory. Evidence: final poses of `gpu_bodies_collide_with_each_other_through_grid_and_fallback` (a dynamic oversized sphere) and of `benchmark_scenes_run_on_the_gpu_and_repeat_exactly` (Debris, Mixed) are byte-identical to the old shader on the RTX 3060 and on llvmpipe, and the same on both devices. That test now expects `3 * CONTACT_ROUNDS + 2` dispatches. Numbers (RTX 3060, balanced 1920x1080, 600 frames, two runs, mean / p95 frame, then GPU mean): base 5.63-6.27 / 10.83-13.07 ms, GPU 3.93-4.16 ms, Physics 1.60-1.82 ms (was 22.18 / 24.54 ms, Physics 20.83 ms); no bodies 2.57 / 2.80 ms; six screens 8.31-13.14 ms, GPU 3.76-3.91 ms; 5,000 bears 8.57-10.02 ms, GPU 6.11-6.24 ms; 10,000 bears with 500 moved 16.70-19.62 ms, GPU 7.40-7.85 ms; 5,000 + 500 moved + six screens 18.26-20.25 ms, GPU 6.09-6.30 ms. With bears and screens, frame time is now mostly CPU and wait, not GPU, and it varied a lot between runs. Docs: GPU contacts in `docs/determinism.md`. The diagnostic-code scanner now skips the `OVERSIZED_PASS` shader define, as it already skipped `GRID_PASS`. Full check after the fix: fmt clean; clippy clean with default features, `--no-default-features`, `--features gpu-tests` and `--features steam`; `cargo test --workspace` 597 lib; gpu-tests serial on the RTX 3060: 684 lib, 27 CLI, 5 vertical slice, 17 doc tests.
- [x] 9. Text on a mesh. Option B: `text_texture::text_texture(text, TextStyle)` (`ui` feature) lays text out with egui's fonts and copies glyph coverage into a `TextureAsset` on the CPU. Tests: `text_draws_ink_inside_its_padding`, `longer_and_multiline_text_grows_the_texture`, GPU golden `text_reads_on_a_tilted_panel` (`text_panel_tilted.png`, checked by eye). Docs: "Text on meshes" in `docs/menus-and-ui.md`.
- [x] 10. World-space text. Covered by item 9, documented there.
- [x] 11. Captions tied to a sound, with a settings toggle and size. Scenario `captions_show_while_their_sound_plays`. Docs: "Captions".
- [x] 12. Occlusion by a physics raycast lowers volume and adds a low-pass. Scenario `a_wall_between_listener_and_sound_lowers_its_volume` (0.4 to 0.12). Docs: "Occlusion".
- [x] 13. Offline audio render already existed (`audio_out`, RMS and peak checks). New: `audio_reference {path, tolerance}` compares the mix with a stored WAV. Scenario `the_mix_matches_its_stored_reference_within_tolerance`. Docs: "Reference mixes".
- [x] 14. Files over 1 MiB stream. Test `large_files_stream_and_still_mix`. Docs: "Long files".
- [~] 15. Steam. `rusting_engine::steam` (`is_running`, `unlock_achievement`, `set_stat`) compiles in every build and does nothing; the `steam` feature is reserved. Test `calls_do_nothing_without_steam`; clippy clean with `--features steam`.
  - [ ] Blocked: the Steamworks backend needs the owner to approve the `steamworks` crate.
- [~] 16. Windows export. `cargo build --release --target x86_64-pc-windows-gnu --bins` builds. Under Wine 2026-10-06: `game.exe --headless` runs 60 ticks; `game.exe` opens a window, draws its first frame on the RTX 3060 in 1.7 s and opens the WASAPI sound device; the Windows lib test binary passes the 44 `scenario::` tests plus steam, text_texture and waypoints. `rusting.exe doctor --probe` crashed inside Wine's `vkQueueSubmit` (exit 3), while the windowed game rendered, so this looks like a Wine issue.
  - [ ] Check on real Windows: gamepad (Wine stubs Windows.Gaming.Input), the `%APPDATA%\<game>` save folder (scenarios override it with `RUSTING_USER_DATA`), sound output and the GPU probe.
- [ ] 17. Microphone level (`mic_level()`, off by default, never in replay hashes). Blocked: needs the owner to approve `cpal` as a direct dependency.
- [ ] 18. Video on a texture. Blocked: needs the owner to approve a video decoder crate.
- [x] 19. Reverse playback (not for streamed files). Test `reversed_sounds_start_from_the_end`.
- [x] 20. `WaypointGraph`: nodes, two-way edges, `nearest`, shortest `path` (Dijkstra with lowest-ID tie breaks), `disconnect`, `length`. Tests: `shortest_path_avoids_the_detour_and_ties_break_the_same_way`, `nearest_picks_the_closest_node`. Docs: "Waypoint graphs" in `docs/concepts.md`.
- [x] 21. Booth sample `samples/forever_bear_booth`, built with the `rusting` CLI: six CRT monitors on inactive store cameras, 192 shelf bears copied from a template, one bear line at a time at a random rate on a `bears` bus with reverb and a low-pass, captions, "CAM n" plates from `text_texture`, and Big Button walking a `WaypointGraph` route as a pure function of the tick with occluded, top-priority steps. Scenario `tests/night.json` passes: the route at ticks 2, 600 and 1200, 101 bear lines and 41 steps by tick 1200, the reverb on the bus, no dropped voice, a mix WAV whose RMS falls from 0.041 to 0.010 as Big Button walks away, and a booth capture checked by eye. Dogfooding found that `BusEffect`, `Caption`, `WaypointGraph` and `AssetServer` were missing from the prelude although the docs used them with the prelude alone; they are in it now.
- [x] 22. New goldens on lavapipe. `screen_crt_off` differed by up to 185 on llvmpipe: its 8x8 checker was sampled with mipmaps, and the drivers pick different mip levels for it inside the feed. The checker now samples with nearest filtering, and the same goldens pass on llvmpipe and the RTX 3060. `text_panel_tilted` differed by 37 only on glyph edges (mip choice on a minified, tilted texture); that golden now allows 48 per channel, and the test still requires over 200 lit text pixels.
- [x] 23. FOREVER BEAR F13: raycasts and `aim` silently skipped colliders without a `physics_body`. Kept the Godot rule (a shape needs a body) and made it loud: `rusting check`, `validate` and `scene` commands warn `SCENE_COLLIDER_WITHOUT_BODY` with the entity, its `/entities/N/collider` location and the `physics_body` to add; player and platformer controllers are exempt, since their collider is only their own shape. `GameScene::raycast` and "Aiming a camera" in `docs/cameras.md` state the rule. Test: `a_collider_without_a_physics_body_is_reported` (tests/cli.rs). No sample or game scene triggers it. 2026-10-06: fmt clean; clippy clean default, `--no-default-features`, `--features gpu-tests`; `cargo test --workspace` 597 lib, 27 CLI.
- [x] 24. FOREVER BEAR F20: a deadlocked game made `rusting test` wait forever with no output. A scenario run now starts a watchdog thread in the game process. When no tick finishes for 60 s (`RUSTING_TEST_STALL_SECS`, `0` turns it off), it writes a failed report naming the last finished tick and exits with code 1, so `rusting test` reports `SCENARIO_FAILED` ("no tick finished in 60 s; the last finished tick is N ..."). Progress goes through a relaxed atomic that no simulation code reads. Tests: unit `a_stalled_run_reports_its_last_finished_tick`; by hand on 2026-10-06, a copy of `forever_bear_booth` that locks a `Mutex` twice from fixed tick 30 failed in 5.2 s with `RUSTING_TEST_STALL_SECS=5` and "the last finished tick is 29". Docs: `docs/concepts.md`, "Tests you can run without a screen". Not done: no live progress line, since the CLI reads game output only at the end. Full check: fmt clean; clippy clean with default features, `--no-default-features` and `--features gpu-tests`; `cargo test --workspace` passes (598 lib).
- [x] 25. FOREVER BEAR F16: running the game binary (`cargo run`, `target/release/...`) after `scene patch` played the old cooked level with no warning. Loading a project's cooked main scene now compares file times and prints "warning: build/main.rscene.bin is older than scenes/main.rscene ... Run `rusting cook .` first" to stderr. Exported folders (no `Cargo.toml`) are skipped, since copying resets file times. `rusting run` and `rusting test` already cook first. Tests: unit `a_cooked_scene_older_than_its_source_is_reported`; by hand on 2026-10-06, `forever_bear_booth` with a touched scene printed the warning on `RUSTING_HEADLESS_TICKS=5 cargo run`, and was silent after the cooked file was newer. Docs: `docs/getting-started.md`. Full check: fmt clean; clippy clean with default features, `--no-default-features` and `--features gpu-tests`; `cargo test --workspace` passes (599 lib).
- [x] 26. FOREVER BEAR F8: CCTV feeds of dark aisles were near black, and the only fix was lighting the room for the player too. `rusting.camera_screen` gains `exposure` (default 1), which multiplies the scene's tone mapping exposure for that feed only, through `ExtractedScreen` and `SceneRenderOptions::exposure`. Old scenes load unchanged (serde default). Tests: headless Vulkan `a_screen_feed_takes_a_crt_look_with_repeatable_grain` now also checks that exposure 0.25 darkens over 256 channels of the frame and brightens none; all 125 `rendering::` gpu-tests pass serially on the RTX 3060. Docs: `docs/cameras.md`, schema summary and example. Full check: fmt clean; clippy clean with default features, `--no-default-features` and `--features gpu-tests`; `cargo test --workspace` passes (599 lib).
- [x] 27. FOREVER BEAR F9: the `dark_interior` preset's 3000 lux sun made no visible light, and nothing said how lux relates to point lights. The renderer maps 100000 lux to 1 on a white surface facing the light (now `DirectionalLight::LUX_PER_UNIT`, used by the renderer), the same as a point light of 1000 up close or ambient intensity 1; 3000 lux is 0.03. `dark_interior` now uses 30000 lux. Existing scenes keep their values, since presets are patches. Tests: unit `every_preset_sun_lights_a_surface_visibly` (sun / LUX_PER_UNIT x exposure >= 0.25 for every preset; the old value gave 0.042). Docs: `docs/look-and-feel.md` light scale paragraph, `directional_light.illuminance` schema hint. Full check: fmt clean; clippy clean with default features, `--no-default-features` and `--features gpu-tests`; `cargo test --workspace` passes (600 lib).
- [x] 28. FOREVER BEAR F2: `text_texture` was hard to find and game code could not reach it (no `AssetServer` access, no `set_material`). `GameScene` gains `set_text(name, text, TextStyle) -> Option<[u32; 2]>` (draws the text as the object's base color map, returns the texture size, and keeps one texture per distinct text and style), `set_material` and `create_texture`. `docs search` snippets now show the line naming the most query words, so "text texture" shows the `text_texture` line instead of a line with "context". Tests: unit `game_code_puts_text_and_materials_on_existing_objects`, and `search_needs_every_word_and_ranks_head_hits_first` extended; `rusting docs search "text texture"` now lists `GameScene::set_text` first. Docs: `docs/menus-and-ui.md` "Text on meshes", `src/project_agents.md`. Full check: fmt clean; clippy clean with default features, `--no-default-features` and `--features gpu-tests`; `cargo test --workspace` passes (601 lib).
- [x] 29. FOREVER BEAR F1: `asset reimport assets/models/bear.glb` failed with ASSET_NOT_FOUND because models added by `scene add-model` have `.rmesh` files but no `.rmeta`. Reimport by path now registers a supported file under `assets/` that has no metadata: it gets a new ID, the file is revalidated in place (a glTF re-cooks its `.rmesh` files) and the `.rmeta` is written. Unknown paths still fail with ASSET_NOT_FOUND. Test: unit `a_model_without_metadata_is_registered_by_reimport`. Docs: the `asset reimport` schema summary. Full check: fmt clean; clippy clean with default features, `--no-default-features` and `--features gpu-tests`; `cargo test --workspace` passes (602 lib).
- [x] 30. FOREVER BEAR F7: primitive mesh sizes and axes were not written down. `docs/look-and-feel.md` "Mesh kit" now has a table of every built-in primitive with its size at scale 1 and its axis (all centered; `Cylinder`, `Cone`, `Capsule` and `Pyramid` run along Y; `Capsule` is 1 x 2 x 1; `Plane` faces +Y, `Quad` faces +Z), and the `mesh/BuiltinPrimitive` schema field gives the same summary and lists `RoundedCube` and `Capsule`. Test: unit `primitive_sizes_match_the_documented_table` measures each generated mesh against the table. Full check: fmt clean; clippy clean with default features, `--no-default-features` and `--features gpu-tests`; `cargo test --workspace` passes (603 lib).
- [x] 31. FOREVER BEAR F6: the patch docs listed `create` but never showed a whole entity, so where `name`, `parent`, built-in sections and `rusting.*` components go was found by trial. `scene_patch::CREATE_EXAMPLE` is a full patch (a parent, a child that names it, a mesh and a component); `rusting schema --json` shows it as `scene_patch.create_example` next to a new `create_entity` layout note, `rusting explain PATCH_JSON` prints it, and the PATCH_OPERATION fix points there. Test: unit `the_documented_create_example_applies` applies the example to a scene; `rusting explain PATCH_JSON` prints it. Docs: `src/project_agents.md`. Full check: fmt clean; clippy clean with default features, `--no-default-features` and `--features gpu-tests`; `cargo test --workspace` passes (604 lib).
- [x] 32. FOREVER BEAR F24: each `expect_screen` took 15-20 s on FOREVER BEAR's 3,895-entity scene, so six on one tick tripped the 60 s stall watchdog. Every sampled pixel (two passes of up to 64x64) recomputed each mesh's bounds from its vertices and inverted its matrix. `CameraView::pick_rect` now prepares the meshes once per check (bounds once per mesh asset, matrices once per entity) and drops meshes entirely outside the view frustum; `pick` shares the same code. Measured on FOREVER BEAR's `w1_booth` steps with all six `expect_screen` on tick 15 (debug build): 143 s before (about 30 s of it rebuilding), 3 s after against 1 s with no screen checks; the real `w1_booth` now passes in 48 s (was about 1 min 54 s). Tests: the existing `expect_screen` and `pick_rect` scenario tests pass unchanged. Full check: fmt clean; clippy clean with default features, `--no-default-features` and `--features gpu-tests`; `cargo test --workspace` passes (604 lib).
- [x] 33. FOREVER BEAR F10: game code could switch and move cameras but not change a field of view, so a zoom needed a second camera. `GameScene::set_camera_fov(name, radians) -> bool` sets a perspective camera's vertical field of view and `camera_fov(name) -> Option<f32>` reads it; both refuse orthographic cameras and names that are not cameras. Test: unit `aim_hits_what_the_camera_faces_and_despawn_removes_it` extended. Docs: `docs/cameras.md` (with an eased zoom example), the camera schema summary. Full check: fmt clean; clippy clean with default features, `--no-default-features` and `--features gpu-tests`; `cargo test --workspace` passes (604 lib).
- [x] 34. FOREVER BEAR F5: the player controller had a fixed ±89° pitch clamp and no yaw clamp, so a seated view was clamped by hand in game code one frame late. `rusting.player_controller` gains `pitch_limits` (`[low, high]` radians, default ±89°) and `yaw_limits` (`[low, high]` or `null`, the default, to turn freely). Both apply after mouse, stick and `set_look` changes, before the camera transform is written, so the view never shows past them. Test: unit `player_look_needs_captured_cursor_and_clamps_pitch` extended (limits hold against large mouse motion, the camera child already shows the clamped pitch, and resetting the limits turns freely again). Docs: `docs/cameras.md` (a booth seat example), the schema summary and example, reflected field docs. Full check: fmt clean; clippy clean with default features, `--no-default-features` and `--features gpu-tests`; `cargo test --workspace` passes (604 lib).
- [x] 35. FOREVER BEAR F23: `rusting check` printed CLI_OUTDATED on every run while the engine source was being edited, so the warning was noise that hid real ones. The source-newer-than-CLI warning now shows at most once a day per project; the marker is `build/cli-outdated-shown`, and its modified time is the last time it showed. A version mismatch between the CLI and the engine still shows on every check. Test: unit `once_a_day_lets_one_call_through_per_day` (first call passes, second is held, a marker older than a day passes again). Docs: the CLI_OUTDATED explanation. Full check: fmt clean; clippy clean with default features, `--no-default-features` and `--features gpu-tests`; `cargo test --workspace` passes (605 lib).
- [x] 36. FOREVER BEAR F19: the `restart()` docs did not say what happens to playing sounds, so a game could not tell whether a looped hum started in a `once` block would play twice. Restart and `load_scene` stop no sound; the docs now say so and name `stop_all_sounds()` or `stop_sound(id)` as the fix. Test: unit `restart_puts_the_starting_scene_back_and_reruns_setup` extended (a looped sound played in setup is requested twice across a restart and no stop is issued). Docs: `GameScene::restart` and `GameScene::load_scene` reference docs, `docs/audio.md` "Playing sounds". Full check: fmt clean; clippy clean with default features, `--no-default-features` and `--features gpu-tests`; `cargo test --workspace` passes (605 lib).
- [x] 37. FOREVER BEAR F15: a scenario `expect` could not count an array, so "at least three ambience sounds are playing" was checked by indexing `/playing/0`, `/playing/1` and `/playing/3`. `greater_than` and `less_than` on an array now compare its length (`{"entity": "audio:", "path": "/playing", "greater_than": 2}`); invariants get the same rule. No new field. Test: unit `an_attached_sound_pans_as_its_entity_moves_and_the_listener_can_move` extended (`/playing` greater than 0 and less than 2 with one sound). Docs: `docs/audio.md` "Testing sound in scenarios", the schema `expect` summary, the field docs. Full check: fmt clean; clippy clean with default features, `--no-default-features` and `--features gpu-tests`; `cargo test --workspace` passes (605 lib).
- [x] 38. FOREVER BEAR F25: the pan law and the fixed-listener behaviour were undocumented, so a scene clipped on one channel and the cause took reading `spatialize` and kira. `Sound::pan` now documents kira's constant-power law (both sides 1 at pan 0; at pan ±1 the near side √2, +3 dB, the far side silent); `docs/audio.md` warns that a named listener does not turn with the active camera and explains the louder hard-panned side. `audio:/playing` entries gain `gain`: `[left, right]` from volume and pan, before bus and master volume, through the new `audio_output::pan_gains`. Test: unit `pan_gains_match_the_mix` (pan 0 and pan 1 values, and the offline kira mix at pan 1 has a silent left and a right √2 times the centre sample). Full check: fmt clean; clippy clean with default features, `--no-default-features` and `--features gpu-tests`; `cargo test --workspace` passes (606 lib).
- [x] 39. FOREVER BEAR F12: with `mouse_look` on, the first left click captures the cursor, so `pointer_ray()` had no useful cursor and there was no documented way to look and click together. Docs only: `docs/cameras.md` now says the left click captures and Escape releases, and that a first-person view clicks through `scene.aim(distance)` with a centre crosshair, keeping `pointer_ray()` for `mouse_look: false` views; the `pointer_ray` reference doc points to `aim`. The capture behaviour is already covered by unit `player_look_needs_captured_cursor_and_clamps_pitch` and `aim` by `aim_hits_what_the_camera_faces_and_despawn_removes_it`. Full check: fmt clean; clippy clean with default features, `--no-default-features` and `--features gpu-tests`; `cargo test --workspace` passes (606 lib).
- [x] 40. FOREVER BEAR F4: it was unclear whether the player controller's `camera_height` and the camera child's own position add or which wins. In first person (`camera_distance` 0) the child keeps its own local position and `camera_height` is unused; a non-zero `camera_offset` replaces the position. In third person the orbit centre is `camera_height` above the body plus `camera_offset`. Documented on the `camera_height` field and in `docs/cameras.md` "Where the eye sits" with a seated-eye example. The warning when both are set was skipped: `camera_height` defaults to 0.6, so a set value cannot be told from the default. Test: unit `player_look_needs_captured_cursor_and_clamps_pitch` now asserts a first-person child at 0.7 stays at 0.7 with `camera_height` 0.6. Full check: fmt clean; clippy clean with default features, `--no-default-features` and `--features gpu-tests`; `cargo test --workspace` passes (606 lib).
- [x] 41. FOREVER BEAR F3: `rusting schema --json` was one very large document, so finding one component took Python one-liners. `rusting schema NAME` now prints only that part: a component, resource or asset type (`camera_screen` or `rusting.camera_screen`), an operation (`doctor`), or a top-level section (`scenario`). An unknown NAME fails with `CLI_USAGE` and lists the known names, which doubles as the list. New `schema::catalog_entry`. Test: unit `catalog_entry_finds_one_component_or_section`; by hand `rusting schema camera_screen --json` prints its default, example and fields. Docs: the schema operation usage, summary and example, and the generated project AGENTS.md. Full check: fmt clean; clippy clean with default features, `--no-default-features` and `--features gpu-tests`; `cargo test --workspace` passes (607 lib).
- [x] 42. FOREVER BEAR F11: scenario step kinds were spread over several guide pages with no single list. `rusting docs show scenario` now prints the generated page `reference/scenario`: every scenario file field (file, steps, invariants, budgets, files, gpu, audio, counter, tick order) and every step kind with an example, built from the schema catalog's scenario section so it cannot drift from what `rusting test` reads. `rusting schema scenario` (item 41) gives the same as JSON. Test: unit `the_scenario_reference_lists_every_step_kind` (bare `scenario` finds the page; `expect_pixels`, `expect_screen`, `capture`, `log`, `set` and `budgets` are on it). Full check: fmt clean; clippy clean with default features, `--no-default-features` and `--features gpu-tests`; `cargo test --workspace` passes (608 lib).
- [x] 43. FOREVER BEAR F22: the game believed a scenario could not turn the player's view, so clicks on objects off the view centre were untested. Scenario `set` already reaches `/components/rusting.player_controller/yaw` and `/pitch`; the controller applies them, within its limits, the same tick. No new step: the `set` step schema text (and so `rusting docs show scenario`) and `docs/cameras.md` now show turning the view before a click. Test: unit `set_turns_a_player_controllers_look` (set yaw 1.0 and pitch -0.3 at tick 1; at tick 2 the body's Y rotation is 1.0 and the pitch held). Full check: fmt clean; clippy clean with default features, `--no-default-features` and `--features gpu-tests`; `cargo test --workspace` passes (609 lib).
- [x] 44. FOREVER BEAR F21: the `audio:` probe gave one level for the whole mix, so a scenario could not prove a voice bus is heard over the ambience bus. Each bus's kira track now ends with a volume control and a meter effect, and `/buses/<bus>/level` and `/peak` report the bus's RMS and peak over the last tick, after its effects and `set_bus_volume` (named bus volumes moved from the track volume to that control; the main bus `""` keeps its track volume, so its level is the mix before the master volume). Docs: the `AUDIO_ENTITY` rustdoc and `docs/audio.md` probe list, with a voice-over-ambience example. Test: unit `the_offline_mix_reports_levels_pan_buses_and_scheduled_starts` extended (music bus level above 0.1 on the left and silent on the right when panned left, main bus level above 0.1, music bus silent after its volume goes to 0 while the main bus still carries a later sound). Full check: fmt clean; clippy clean with default features, `--no-default-features` and `--features gpu-tests`; `cargo test --workspace` passes (609 lib).
- [x] 45. FOREVER BEAR F17: no trustworthy 1080p frame-time p95. Split:
  - [x] 45a. `RUSTING_PERF` prints frame-time percentiles. The once-a-second `[rusting] perf` line now has `frame p50 / p95 / p99 / max` over that second (nearest rank over each frame's length), and `docs/look-and-feel.md` says how to measure in the real window and that `rusting test` frame times (offscreen plus readback) are a regression guard, not the budget. Test: unit `frame_percentiles_use_the_nearest_rank` (100 frames, six slow: p50 10, p95 20, p99 20, max 40; empty gives zeros). Full check: fmt clean; clippy clean with default features, `--no-default-features` and `--features gpu-tests`; `cargo test --workspace` passes (610 lib).
  - [x] 45b. A window size in `project.json`, so a 1080p run needs no `set_window_size` build. `ProjectManifest.window: Option<[u32; 2]>`; when the game loads from its project folder, `request_project_window` reads it and asks for that size through the same `WindowRequest` as `set_window_size` (applied after the first frame; headless and scenario runs ignore it). The JSON schema lists `window`; `docs/look-and-feel.md` shows it next to `RUSTING_PERF`. Exported builds carry no `project.json`, so they keep calling `set_window_size`. Test: unit `project_json_window_size_is_asked_for` (no project file asks nothing; `"window": [1920, 1080]` asks for 1920x1080) and `json_schemas_describe_every_key_the_engine_writes` covers the new key. Full check: fmt clean; clippy clean with default features, `--no-default-features` and `--features gpu-tests`; `cargo test --workspace` passes (611 lib).
  - [x] 45c. `perf.render.cameras` in the test report lists camera screens. It was filled only for viewport cameras, so a wall of `camera_screen` feeds showed no cost. `SceneRenderer::screen_stats` gives the camera, work counters and GPU time of each feed drawn that frame, and `HeadlessCapture::metadata` appends them after the viewport cameras with `"screen": true`. Docs: `PerfReport.render` rustdoc and `docs/cameras.md`. Test: headless Vulkan `viewport_cameras_split_the_frame` extended with an inactive `Feed` camera shown on the mesh (cameras `Left`, `Right`, `Feed`; `Feed` has `screen: true` and draws > 0). Full check: fmt clean; clippy clean with default features, `--no-default-features` and `--features gpu-tests`; `cargo test --workspace` passes (611 lib), and with `--features gpu-tests --test-threads=1` on the RTX 3060 (698 lib).
  - [x] 45d. `rusting run --bench FRAMES` runs the windowed game for that many frames and prints p50/p95/p99 once at the end. Acceptance: unit test of the summary line; the windowed run itself is real-hardware only. Evidence: `RUSTING_BENCH_FRAMES` makes the windowed runner skip 60 warm-up frames, measure the rest, print a `[rusting] bench` JSON line and exit; `run --bench` reports it as `timings.bench` and a `bench:` text line. Test `project::tests::a_bench_line_round_trips_its_frame_times`; the windowed run is real-hardware only.
- [x] 46. FOREVER BEAR F29: easing hundreds of object colors cost about 25 µs a call. Name lookups were already hashed; the cost was `set_color`/`set_emissive` scanning every material for an equal one, and every eased value adding a material that was never freed. Acceptance: edits find equal materials through a hash index, unused edited materials are removed, and the count stays bounded. Evidence: test `project_runner::tests::easing_many_colors_keeps_the_material_count_bounded` (4,000 objects, 900 eased for 20 ticks): release build 488 µs a call and 18,000 materials kept before, 348 ns and under 4,000 after.
- [x] 47. FOREVER BEAR F27: a scenario `expect` on a counter nobody created yet failed with "no entity", while game code reads it as 0. Acceptance: the scenario reads it as 0 too. Evidence: `scenario::tests::counter_shorthand_sets_checks_and_logs_a_counter_by_name` runs `{"expect": {"counter": "plays", "equals": 0}}` with no such counter and passes; schema scenario docs say so.
- [x] 48. FOREVER BEAR F30: a flag a command does not take gave only "invalid command or arguments". Acceptance: the error names the flag and shows the command's usage; `determinism --release` says determinism always compares debug and release. Evidence: test `a_bad_flag_is_named_with_the_command_usage` in `src/bin/rusting.rs`.
- [x] 49. FOREVER BEAR F28: tuning needed one run per wrong guess because a scenario stops at its first failed check. The scenario field `keep_going` existed but had no CLI flag, and the error showed only the first failure. Acceptance: `rusting test --keep-going` runs every step and the SCENARIO_FAILED message lists every failed step. Evidence: `a_bad_flag_is_named_with_the_command_usage` checks the flag is accepted; `scenario::tests::keep_going_reports_every_failed_check` covers the run; the message joins every failed step from the report.
- [x] 50. FOREVER BEAR F26: which sound loses on a full bus needed a source read. Acceptance: docs/audio.md and `Sound::priority` say priority compares within one bus, quietest means after distance and occlusion, a new sound must rank strictly higher, and ties replace the oldest. Evidence: docs/audio.md "When the bus is full"; `scenario::tests::a_thousand_sounds_in_one_tick_stay_within_the_voice_limit` now also plays a priority 0 sound on another bus while `chorus` is full and expects it to play.
- [x] 51. FOREVER BEAR F18: headless runs gave no render timing. Acceptance: a `rusting test` scenario that renders offscreen reports GPU frame-time percentiles over every frame, not only the last one. Evidence: `perf.render` now has `gpu_frames`, `gpu_ms_p50`, `gpu_ms_p95` and `gpu_ms_max` (nearest rank over each `HeadlessCapture::frame`); `scenario::tests::a_render_budget_reports_render_counters_without_a_capture` (gpu-tests) checks the frame count covers every tick and p95 <= max; docs/look-and-feel.md explains `"gpu": true` with `capture_size`.
- [x] 52. Freddy's F9/F18: game code had no per-tick hook, and `pressed` lasted one frame, so a game stepping per tick inside `update` missed presses at 144 Hz. Acceptance: `rusting_game!(update, tick: tick)` calls `tick` once per fixed tick, and `pressed` there is true on the first tick after a press made on a frame that ran no tick. Evidence: `project_runner::tests::a_tick_function_sees_a_press_from_a_frame_without_a_tick` (a quarter-step frame with the press, then a three-tick frame: one press seen, three ticks run); macro arm compile-checked in a scratch crate; docs/concepts.md "Game code" and "Frames and fixed ticks".
- [x] 53. Freddy's F17: whether `restart` keeps counters and what `set_paused` does to `fixed_tick` was not stated. Acceptance: the `GameScene::restart` and `set_paused` docs say both, with a restart example that carries a value across. Evidence: rustdoc on both; `restart_puts_the_starting_scene_back_and_reruns_setup` now checks a `set_counter` counter is gone after restart; `a_tick_function_sees_a_press_from_a_frame_without_a_tick` checks pause holds `fixed_tick` and tick functions while `frame` counts on.
- [x] 54. Freddy's F13: ray hits on ragdoll bodies had an empty name and ignored the bone's classes. Acceptance: a hit on a ragdoll body reports its bone's name, and `raycast_skipping` skips it by the bone's classes. Evidence: ragdoll bodies carry `RagdollPart { bone }` (snapshot-registered); `runtime::tests::ray_hits_on_ragdoll_bodies_carry_the_bone_name_and_classes` hits an active ragdoll's hips body ("Hips") and skips it by class `animatronic`; docs/animation.md and `GameScene::raycast` rustdoc.
- [x] 55. Freddy's F15: an active ragdoll's hips drifted when a clip keyed only their position, because the whole transform no longer matched what the ragdoll wrote and the rotation target became the last physics pose. Acceptance: unkeyed position, rotation and scale each return to the pose from the handoff. Evidence: `drive` restores per field; `runtime::tests::an_active_ragdoll_returns_an_unkeyed_rotation_to_its_pose` spins the hips body while keying only the hips' position for 90 ticks and expects under 0.2 rad of turn (fails before the fix); docs/animation.md.
- [x] 56. Freddy's F16: teleporting an active ragdoll left its bodies trailing, and muscle 0 collapsed it instead of returning to animation. Acceptance: `GameScene::reset_ragdoll(name)` drops the bodies and restores the bones' animated pose; an active ragdoll respawns its bodies at rest on the bones next tick. Evidence: `runtime::tests::reset_ragdoll_teleports_an_active_ragdoll_with_its_bodies` moves the character 5 m, resets, and finds the hips body 5 m on (within 0.05 m) at under 1 m/s; blending back now also restores unkeyed fields one by one; docs/animation.md.
- [x] 57. Freddy's F14: `AnimationEvent` fields were missing from the docs. Evidence: the struct is in the `api/` index with a doc line per field (`rusting docs show api/AnimationEvent`); `docs.rs` test asserts every field is listed; `docs/animation.md` names the fields.
- [x] 58. Freddy's F1: `guide/look-and-feel` said skinned meshes import as a still pose, against `guide/animation`. Evidence: the stale sentence is replaced by a pointer to `guide/animation` "Skinned meshes"; `grep -rn "still pose" docs` finds nothing; docs tests pass.
- [x] 59. Freddy's F6: `scene add-model` refused models whose node names repeat or clash with the scene. Evidence: node and extra-primitive names already taken get " 2", " 3"; a root name clash fails with "pass --name"; `a_gltf_model_lands_in_a_scene_under_one_named_object` adds a second copy, a root named like its own node, and checks the suffixes and the hint.
- [x] 60. Freddy's F12: exposure and mouse look were reachable only through `world()`. Evidence: `GameScene::set_exposure(f32)` writes or adds `rusting.tone_mapping`, `GameScene::set_mouse_look(name, bool)` toggles the player's mouse look (off frees the cursor); `video_settings_write_render_settings_and_the_window_request` covers both, including ignored negative exposure and an unknown player.
- [x] 61. Freddy's F2: `scene add-model` has no `--license`/`--author` flags and nothing said where provenance lives. Evidence: the `scene add-model` schema summary and `guide/look-and-feel` step 3 say the `.rmeta` written by `asset import` is the record; schema, docs and workspace tests pass.
- [x] 62. Freddy's F8: fog, color grading and player controller fields had no setter from game code. Evidence: `GameScene::set_field(name, path, value)` takes the scenario `set` JSON pointers (`/components/rusting.fog/density`); `video_settings_write_render_settings_and_the_window_request` sets `rusting.player_controller/walk_speed`, and checks the errors for an unknown field and object; the project AGENTS.md names it.
- [x] 63. Freddy's F11: faint egui white came out as a solid grey band. Cause: egui's `from_white_alpha(8)` and `from_rgba_unmultiplied` premultiply in linear light and give grey 50; the painter matches egui's backends. Evidence: GPU test `faint_egui_white_is_premultiplied_in_linear_light` paints both and `from_rgba_premultiplied(8, 8, 8, 8)` (adds 8); `guide/menus-and-ui` says which constructor to use for faint overlays.
- [x] 64. `rusting capture --at X,Y,Z` renders from a camera placed at a point, aimed with `--look-at X,Y,Z` or `--look YAW,PITCH` (Freddy F7). It takes its lens from `--camera` or the active camera.
  Evidence: `capture_without_vulkan_still_reports_camera_and_picks_per_tick` picks the cube at the center from the side and from above and nothing when facing away, and rejects `--look-at` without `--at`; the GPU test `capture_renders_a_png_and_maps_pixels_to_scene_ids` renders from a placed camera and finds the cube. A light on the capture camera is not added.
- [x] 65. Scenario captures and `rusting capture` render only the four ticks before each image step (and through its `until`/`within`), not every tick of the run, unless the scene has GPU bodies or the scenario sets `gpu` or a render budget (FOREVER BEAR F33).
  Evidence: `perf_is_reported_and_budgets_fail_the_run` runs a 40-tick scenario with one capture at tick 40 and gets `gpu_frames` 5 and the PNG; `capture_renders_a_png_and_maps_pixels_to_scene_ids` captures `--tick 300` with `gpu_frames` 5. Every existing capture, pixel and camera-screen test still passes. The wall-clock gain on FOREVER BEAR's 10,000-tick runs is not measured here.
- [x] 66. `rusting docs show api/WaypointGraph` lists its fields and every method (`add_node`, `connect`, `disconnect`, `nearest`, `path`, `length`), read from the source (FOREVER BEAR F31). The struct page reader no longer gives an undocumented type the previous item's doc comment.
  Evidence: `api_index_lists_documented_gameplay_methods` checks the page names `nodes`, `nearest`, `disconnect`, `path` and `length`; every docs test passes.
- [x] 67. `rusting run --stderr` and `rusting test --stderr` print the game's stderr line by line while it runs, and still keep it whole in `game.stderr` (FOREVER BEAR F32).
  Evidence: `echoed_stderr_is_still_kept_whole` runs a child that writes to both streams with echo on and gets its stderr and stdout back intact. No test runs a real game with `--stderr` end to end.
- [x] 68. `rusting docs show guide/lighting` names each light type and whether it casts shadows (only the first shadowed directional light does), the light cap (64 at High, 32 at Balanced, 16 at Eco) and the order lights are dropped in; scenario perf reports `dropped_lights` (Freddy F3, docs half).
  Evidence: `api_index_lists_documented_gameplay_methods` checks the page names the cap, Eco, shadows and `dropped_lights`; `perf_is_reported_and_budgets_fail_the_run` checks `perf.render.dropped_lights` is 0. Drop order was read from `prepare_lights` and `collect_point_lights`/`collect_spot_lights`.
- [x] 69. Shadowed spot lights: `"shadows": true` on a `spot_light` gives it one shadow map, so a flashlight does not light the far side of a wall (Freddy F3, shadow half). Acceptance: a GPU test with a spot light behind a wall leaves the floor past the wall unlit with shadows on and lit with them off. Evidence: GPU test `spot_light_shadow_stops_at_a_wall_only_when_enabled` reads the floor behind a wall at 0 with shadows on and 255 with them off; the directional shadow tests pass unchanged. A frame still has one shadow map: a shadowed directional light wins over spot lights, and point lights cast none.
- [x] 70. `rusting.player_controller` crouches: `crouch_height` above 0 makes the new `player.crouch` action (C, left Ctrl, pad East) shrink a capsule or box body to that height from the top at `crouch_multiplier` speed, drop first-person cameras by the lost height, block jumping, and stand again only when a shape cast up finds room (Freddy F10). Evidence: `player_controller_crouches_under_a_table_and_stands_only_with_room` (a 1.8 m capsule stops at a 0.75 m table, crouched to 0.7 m walks under with feet on the floor and the camera 1.1 m lower, stays crouched under it after release, stands up past it). Sensors still see the standing collider.
- [x] 71. A scenario `audio:` check on `/clips/<clip>` for a clip that never played reads 0 instead of failing with "does not exist", so `"equals": 0` proves a sound never played; `"exists": false` still passes for it (FOREVER BEAR F34). Evidence: `the_audio_entity_counts_sound_cue_requests_per_clip` checks an unplayed `sfx/miss.wav` both ways.
- [x] 72. `asset import --to assets/sounds` (and `asset generate --to`) writes to `assets/sounds`, not `assets/assets/sounds`: a folder starting with `assets/` is read from the project root (playplace F14). Evidence: `audio_imports_and_dry_runs_write_nothing` imports with `assets/music` and gets `assets/music/hit.wav`.
- [x] 73. `rusting docs show api/GpuBodySettings`, `api/GpuConditionShader` and `api/MaterialAsset` list each type's fields with docs; the GPU cube rain tutorial and the `collider` field say friction decides whether a dense GPU pile flows (about 0.05 for plastic balls, 0.3 jams) and that dense piles jam below a threshold push (playplace F13, F15, F17). Evidence: the docs test checks a documented field line on each new page.
- [x] 74. Scenario checks and invariants capture only the entity they read (`scene_entity_lenient`) instead of the whole scene each tick, and `perf.wall_ms_mean` reports the whole run per tick, checks included (playplace F16, first half). Evidence: a throwaway release benchmark on 5,000 entities measured 3.1 ms per whole-scene capture against 35 us for one entity; the existing scenario tests pass unchanged, and `perf` tests assert `wall_ms_mean >= tick_ms_mean`.
- [x] 75. GPU physics steps without drawing a frame: on scenario ticks with GPU bodies and no image step due, submit only the physics compute work, so long GPU scenarios run faster than render speed (playplace F16, second half). Acceptance: a GPU test shows the same `gpu_state_hashes` with and without the skipped draws, and `gpu_frames` counts only image ticks. Evidence: `SceneRenderer::step_physics` records the uploads and physics dispatches of a frame and submits before the culling and draw passes; scenario runs and `rusting capture` use it on ticks with GPU bodies and no image due. GPU test `gpu_bodies_step_without_drawing_and_hash_the_same` gets identical `gpu_state_hashes` from a 30-tick run drawn every tick and one drawn only for its last capture, whose `gpu_frames` is 5. The CPU-side draw preparation still runs on skipped ticks.
- [x] 76. Games keep state in their own components without extra crates: `rusting_game!(update, components: [Night => "game.night"])` registers them (also with `tick:`), and the prelude re-exports `Component`, `Serialize`, `Deserialize`, `bevy_ecs` and `serde` (serde derives take `#[serde(crate = "rusting_engine::serde")]`) (playplace F12). Evidence: unit test `rusting_game_components_are_saved_in_scenes`; ignored CLI test `rusting_game_registers_a_custom_component_without_extra_crates`, run here and passing, builds a generated project that depends on `rusting_engine` alone and passes a scenario that sets and reads `/components/game.night/power`.
- [x] 77. `rusting.post_volume` (`extents`, `blend`, `priority`) limits the `rusting.fog` and `rusting.color_grading` on its object to a box around it: fully inside, fading out over `blend` metres, higher priority blended last; fog and grading without a volume stay global (Freddy F5). Acceptance and evidence: unit test `post_volumes_blend_fog_and_grading_around_the_camera` reads the extracted render world outside the hall (global grade, no fog), inside it (the hall's fog and grade) and half way through the blend (half of each); the editor offers fog and grading on a volume object.
- [x] 78. A game built against a newer or edited engine than the `rusting` CLI no longer fails every run with a bincode error (`InvalidBoolEncoding`) when it cannot decode the cooked scene: in a dev project (with `Cargo.toml`) it prints a warning naming the mismatch and loads the source main scene instead; an exported game still reports the error (forever-bear F35). Evidence: unit test `a_cooked_scene_from_another_engine_build_falls_back_to_the_source` loads an undecodable cooked scene with and without `Cargo.toml`.
- [x] 79. Mesh level of detail is documented and findable: the renderer already picks a level per object from a `.rlod` file beside the `.rmesh` (GPU test `lod_groups_draw_the_level_for_the_camera_distance_on_every_path`), but `rusting docs search lod` found nothing (forever-bear F36). `guide/look-and-feel` gains a "Level of detail (LOD)" section with the file format and both metrics, and SKILL.md points to it. Evidence: unit test `level_of_detail_is_found_by_its_usual_names` (`lod`, `level of detail`, `rlod`).
- [x] 80. The lighting guide says what light brightness means and where lights point: sun `illuminance` in lux next to point and spot `intensity`, the built-in presets' sun, ambient and sky values, lamp ranges, and that directional and spot lights shine along -Z with the rotation that aims one down (forty-thousand-and-one F34; also triaged that game's older items, most already fixed). Evidence: unit test `the_lighting_guide_explains_brightness_and_direction`; the -Z direction read from `light_direction` in the renderer and the values from `src/art_direction.rs`.
- [x] 81. GPU contact-grid overflow is visible outside the editor: `physics_grid_overflow`, `physics_oversized_bodies` and `physics_fallback_tests` are in `rusting test --json` under `perf.render`, game runs (window and headless capture) insert `RenderCapacityDiagnostics` into the world each frame (now in the prelude), and docs/concepts.md "Many GPU bodies" explains the 8-bodies-per-cell limit and warns against clamping many bodies to one plane (forty-thousand-and-one F36). Evidence: GPU test `contact_grid_overflow_is_in_perf_and_the_world` (20 kinematic bodies in one spot: overflow and fallback tests above 0 in perf, overflow above 0 in the world resource); unit test `contact_grid_overflow_is_explained`.
- [x] 82. Many-body costs are documented (forty-thousand-and-one F35): docs/concepts.md "Many bodies: what costs time" gives `4 * subdivisions^2` triangles per `SphereSpawn` sphere (36 at 3, 144 at 6, 1,024 at 16), says `state_hash` is O(class size) and belongs on checkpoint ticks, and points to `perf.tick_ms_p95` and `perf.render.gpu_ms_p95`; the `SphereSpawn::new`, `subdivisions` and `GameScene::state_hash` rustdoc say the same. Evidence: unit test `many_body_costs_are_explained` (docs search finds both topics, and `procedural_sphere_mesh` has exactly `4 * n * n` triangles for 3, 6 and 16).
- [x] 83. A settled GPU pile stays asleep (forty-thousand-and-one F13 re-test: the pile slept by tick 40, then woke with nothing touching it). Cause: sleeping balls still took every tiny contact correction, crept, spread the pile (top rose 0.11 m over 400 ticks) and woke. `physics_contacts.comp` now has sleeping bodies ignore pushes under `SLEEP_SLOP` (0.5 mm) and velocity changes under the rest speed; a larger slop (the 2 mm contact margin) let the sideways-pressed pit squeeze a ball into a wall. Still order-independent (per-body max/min). Evidence: GPU test `a_settled_gpu_pile_stays_asleep` (3,600-ball 9-layer pit: before, 3,032 asleep at tick 200 and 2,181 at 300; now over 3,500 at tick 240, no fewer at 400, top still within 2 mm); `a_deep_gpu_ball_pit_settles_and_its_walls_hold` still holds (3,592 resting, none escape). The 40,000-ball case is for the game to re-test.
- [x] 84. Remaining metronome-mines API doc gaps (F21, F26): `api/BusEffect` page (variants and fields from the source), and the `GameScene::press_tick` doc says a scenario press with `"at": 0.4` gives `tick + 0.4` instead of claiming scenario presses land on whole ticks. The other open metronome-mines items were checked against the current binary and marked: F1, F5, F19, F20, F22 to F25, F27 and F28 already work. Evidence: unit test `bus_effects_and_press_tick_are_documented`; `rusting fix --dry-run` prints `No fixes needed.`; a rerun `upsert` patch reports `0 field changes`; `rusting docs search fixed_tick scenario` finds `reference/scenario`.
- [x] 85. `expect_pixels` can compare a region with an earlier capture (split-signal F24): `"differs_from": "shots/a.png"` with `difference_min` and/or `difference_max` checks the largest of the R, G and B mean differences (0..255) in the region, so a scenario can prove two moments look different or the same. Either half alone is a validation error; the schema line describes it. Evidence: unit test `differs_from_needs_a_difference_limit` (validation and `region_difference` numbers); the GPU test `pixel_checks_goldens_and_named_cameras` now also checks the main frame against `main.png` (difference at most 1) and against the away view `away.png` (difference at least 1).
- [x] 86. Builtin mesh UVs make sense and are documented (split-signal F29, F30): the flat caps of `Cylinder` and `Cone` map a texture as a disc (upright from outside, -Z at the top) instead of radial streaks, and `Plane` shows the whole texture instead of a per-triangle fan. docs/look-and-feel.md "Mesh kit" says how each primitive maps a texture and that a child's transform is relative to its parent, scale included; docs/concepts.md says the same about parents. Evidence: unit test `primitive_uvs_and_child_scale_are_explained` (cap UVs equal the disc mapping for every cap vertex, Plane corner UVs, docs search finds both topics); `primitives_wind_outward_with_unit_normals` still holds for the new Plane.
- [x] 87. A `PATCH_INVALID` error for an object names the operation that last changed it (`operation 1: /transform/position: ...`), and a color array of the wrong length says `a color needs 3 numbers (RGB, alpha 1) or 4 (RGBA), got 2` instead of serde's `expected an array of length 4` (split-signal F4). Evidence: unit test `a_bad_field_in_a_created_entity_is_named_by_its_path` (2-number `base_color` in operation 1, 2-number position in operation 0); `rusting scene patch --dry-run` on a scratch project prints both messages.
- [x] 88. Raycasts are cheaper and measurable (split-signal F23): `GameScene::raycast` no longer looks up each collider's ragdoll owner when no classes are skipped (the per-collider lookup added with `raycast_skipping`), and the ignored timing test `raycast_cost_per_ray` measures one 80x45 feed (3,600 rays, 201 colliders). docs/concepts.md "Many bodies" says ray cost grows with the collider count, gives the measured cost and how to rerun it. Evidence: `cargo test --release --lib raycast_cost_per_ray -- --ignored --nocapture` on a Ryzen 5 7600X: 2.69 us per ray before, 2.08 to 2.12 us after (three runs); `ray_hits_on_ragdoll_bodies_carry_the_bone_name_and_classes` still passes. Timing is hardware-only, so the test is `#[ignore]`.
- [x] 89. `rusting asset reimport` finds an asset by its path relative to `assets/` (`textures/wood.png`), the form `asset import --to textures` takes, as well as by the project-relative path and ID (night-market F21). Also marked night-market F22 (the `Capsule` primitive exists) and F24 (guide/look-and-feel has the lighting and night values) as already fixed, and applied the re-test verdicts in night-market and lantern-keeper. Evidence: unit test `images_import_with_settings_provenance_and_a_stable_id_on_reimport` finds the asset by both `assets/ui/incoming.png` and `ui/incoming.png`; on a scratch project `rusting asset reimport . textures/wood.png --from wood.png --dry-run` now previews the replacement where it failed with `ASSET_NOT_FOUND`.
- [x] 90. `keep_going` already lists every failed check, but the game's own error line in `game stderr` named only the first, which read as one failure (lantern-keeper F31). It now ends with `(N more failed checks in the report)`, and docs/concepts.md says how to turn `keep_going` on, including `RUSTING_KEEP_GOING=1`. Evidence: on a scratch project, three failing `expect` steps at tick 4 with `RUSTING_KEEP_GOING=1 rusting test k.json` list all three failures, and the stderr line ends with `(2 more failed checks in the report)`; unit test `keep_going_and_its_variable_are_documented` finds the variable in manual/concepts; `keep_going_reports_every_failed_check` and `screen_checks_see_a_mesh_in_front_of_a_camera_and_behind_a_wall` cover several failures across ticks and at one tick.
- [x] 91. Primitive side UVs follow height (lantern-keeper F30). `lathe_mesh` set V from the profile row index, which counted the cap rows, so a `Cylinder` side spanned V 0.4 to 0.6 (the middle fifth of the image, stretched) and a `Cone` side V 0.67 to 1; a `Capsule` ran V up, upside down compared with `Sphere`. V is now `(top - y) / (top - bottom)` for every non-cap row, so horizontal bands in a texture become rings. guide/look-and-feel says so. Evidence: unit test `primitive_uvs_and_child_scale_are_explained` checks every side vertex of `Cylinder`, `Cone` and `Capsule` against the height formula, and fails on the old mapping.
- [x] 92. A scenario `click` with `index` counts copies of a text drawn within 4 px of each other once, keeping the topmost, so a label with a drop shadow is one place (night-market F23). `RuntimeUi::find_text_where`, the click schema text and `ClickTarget` say so. Evidence: unit test `find_text_where_picks_by_prefix_and_reading_order` paints a shadow `-` 1 px below the third `-` button and still finds 3 matches; with the merge disabled it fails with `#5 (of 4 matches)`.
- [x] 93. Say when `perf.render` is filled (split-signal F12). A `max_draws` or `max_triangles` budget already draws every tick and fills `perf.render`, with `cameras` splitting GPU time, draws and triangles per camera, but the `budgets` schema text still said draw and triangle limits "need a capture step and Vulkan", which read as `render` being null without one. The schema text and docs/concepts.md now list what fills it (render budget, `"gpu": true`, capture or `expect_pixels`). Evidence: unit test `when_perf_render_is_filled_is_documented` finds the paragraph in manual/concepts and checks the schema text; the existing test `a_render_budget_reports_render_counters_without_a_capture` covers the behaviour.
- [x] 94. Document that bodies solve in `Entity` order, so adding or removing unrelated entities can move a chaotic physics result such as a toppled barrel stack, while the same scene still replays exactly; scenario checks should test robust values (lantern-keeper F26). Evidence: unit test `entity_order_effect_on_physics_is_documented` finds the paragraph in manual/determinism.
- [x] 95. `GameScene::counter_or(name, default)` reads a counter that may not exist yet, giving the default without the missing-counter warning, so a counter-based game no longer creates every counter at 0 on the first tick (night-market F15). The warning names it, and guide/menus-and-ui describes counter creation, `counter_or` and `counters()`. Also marked night-market F11 fixed: its re-test kept pointer fractions only because `click` needed the exact label, which F18 fixed. Evidence: unit test `game_code_reads_and_sets_counters_by_name` reads a missing counter with `counter_or` and checks no warning was recorded and an existing counter wins over the default; `counter_or_is_documented` finds the guide paragraph.
- [x] 96. A paused sound's caption hides until `resume_sound`; before, `/captions` and the HUD kept the line that was showing for as long as the sound stayed paused (forever-bear F37). docs/audio.md says so. Evidence: scenario test `a_paused_sound_hides_its_caption_until_resumed` pauses a captioned tone at tick 20 and resumes it at tick 40, and checks the caption shows at tick 10, is gone at tick 30 and is back at tick 45; without the fix it fails at tick 30.
- [x] 97. Game code counts GPU bodies inside a box (playplace friction 6): `GameScene::count_gpu_bodies_in_box(class, min, max)` counts the members of an object class whose latest `GpuStateMirror` position lies in the box, after `request_gpu_class_snapshot`. Evidence: unit test `gpu_bodies_are_counted_inside_a_box_from_their_mirrors` (two of three balls inside, a crate of another class and a ball with no snapshot are handled). A scenario reads the count through a counter the game sets; there is no scenario key for it.
- [x] 98. Scenes spawn many copies of an object without code (playplace friction 4): the `rusting.spawn_grid` component (`count`, `spacing`) copies its entity and children onto a grid on the first frame of a running game, names the copies `<name>#<n>`, keeps the parent, and removes itself so it expands once; copied physics bodies get fresh IDs. Evidence: unit test `spawn_grids_copy_the_tree_onto_cells_once` (positions, child copy, parent, single expansion), plus a manual `rusting inspect --tick 2` on a sample copy showing `Game Camera#11` at its grid cell.
- [x] 99. Game code moves GPU bodies and `restart` resets them (playplace friction 7): `GameScene::gpu_command(name, GpuBodyCommand)` resolves the body's `PhysicsId` and queues a teleport, velocity, impulse, force or state read for the next GPU tick (`GpuBodyCommand` is in the prelude); `restart` sets `GpuPhysicsCommands::reset_to_authored`, so every GPU body starts again from the scene file with rule edge and cooldown state cleared. Evidence: unit test `gpu_commands_reach_the_named_body_once_it_has_an_id`, the restart test now asserts the extracted reset flag, and the existing GPU test `reset_restarts_from_authored_state_and_restore_resumes_a_snapshot` covers the reset and commands on the device. Bit-for-bit GPU repeat after `restart` is still not promised.
- [x] 100. Scenarios count GPU bodies in a box (playplace friction 6): the `class:<name>` scenario entity reads the class's member count and the count and bounds of their mirrored GPU poses; `class:<name> in minX,minY,minZ maxX,maxY,maxZ` adds `gpu/inside`. Reading it requests a class snapshot, so checks use `within` or `until`. Evidence: unit test `class_entities_count_members_and_gpu_bodies_in_a_box` (members with and without poses, another class, the box count and a bad box).
- [x] 101. `rusting run --replay` matches its windowed recording (forever-bear F39). The state hash fed raw `Entity` bits, but resources are entities too, so the windowed runtime's extra resource shifted every later entity's index and a replay of any recording, even the starter template's, diverged at the first tick after the tilemap spawned its tiles; `PlatformerController::floor` and `PlayerController::floor`/`wall` leaked bits the same way through their `Debug` text. The hash now feeds each entity's place in entity order among the hashed entities (`StateHasher::entity`), for entity keys, solver carry-over, the physics id registry, GPU proxies and controller floors. Evidence: `runtime::tests::state_hash_ignores_resources_that_shift_entity_ids` (a tilemap platformer, a resting box and ground with and without one extra resource hash the same over 60 ticks; fails without the fix). By hand: a starter project recorded for 10 s windowed on the RTX 3060 replays without divergence; before, it diverged at tick 9. Not done: naming the diverging entity in the replay error needs per-entity hashes in the recording.
- [x] 102. Scenario screenshots blend the runtime UI as the window does (forever-bear F41). The window paints egui onto its UNORM swapchain, so egui blends in gamma space as its own backends do. `HeadlessCapture` painted it onto the sRGB offscreen image, so screenshots and goldens blended in linear light and `from_black_alpha(205)` darkened the view only about half. The capture image is now `MUTABLE_FORMAT` and the UI paints through a UNORM view of it. `guide/menus-and-ui` now gives egui's layer order and says a default `CentralPanel` fills the shared background layer with the opaque `panel_fill`, which hides shapes painted there before it. Evidence: `rendering::capture::tests::the_ui_blends_in_gamma_space_as_over_the_window` (half black over grey 200 reads 100; 146 with the old sRGB view). Not reproduced: the game's background-layer shapes not showing; the engine's GPU test `runtime_ui_drawn_by_an_ecs_system_paints_over_the_game_view` shows them, so the layer-order note is the answer until a repro appears.
- [x] 103. Game code lists and pauses the sounds that are playing (forever-bear F40). `scene.playing_sounds()` returns every sound started and not yet ended, oldest first, with `id`, `clip`, `bus`, `looped` and `paused`; the list follows the audio device (finished, stopped or voice-limit-dropped sounds leave it). `scene.pause_sounds(Some(bus))` / `pause_sounds(None)` pause a bus or everything, and `resume_sounds` resumes only what is paused. Per-sound pause, seek and bus volume already existed. The `pause_menu` recipe now pauses and resumes all sounds with the game. Evidence: `runtime::audio::tests::playing_lists_live_sounds_and_pauses_them_by_bus` (pausing a bus sends one `Pause` per sound on it and marks them paused; a device-ended and a stopped sound leave the list); `recipes::tests::pause_menu_recipe_applies_to_the_player_templates_and_its_scenario_passes` still passes.
- [x] 104. Parallel `rusting run`s of one project no longer fail with `SCENE_IO`, and `rusting run --seed N` sets the random seed (forever-bear F42). Every run cooks the main scene, and `write_atomic` wrote through one shared `<path>.tmp`: one run's `rename` moved the file away before another's, which then failed with "No such file or directory (os error 2)". The temporary name now carries the process ID and a per-process counter, and a failed write removes its temporary file. `--seed N` passes `RUSTING_SEED` to the game, which starts `RandomSeed` from it; a scenario's `seed` and a replay's recorded seed still win. Evidence: `runtime::scene_file::tests::parallel_atomic_writes_of_one_file_all_succeed` (8 threads x 50 writes of one file; with the old shared name it fails with the same os error 2), `project_runner::tests::a_run_seed_sets_the_random_seed`, and `--seed` with no value or `-1` is a `CLI_USAGE` error in `tests/cli.rs`. Not done: parallel `--ticks` runs still share `build/final.rscene`, so the last run to finish owns it.
- [x] 105. `rusting test <root> <scenario> --exe <game>` runs a scenario against an already built game, such as an export (forever-bear F48). It skips cooking and building, starts the executable from its own folder with `RUSTING_SCENE_PATH` cleared so it reads the export's scene and assets, and keeps the report and test user data under the project's `build/`. The game side needed nothing new: every game binary already runs `RUSTING_TEST_SCENARIO`. Evidence: unit test `exe_runs_a_scenario_in_a_built_game_from_its_own_folder` (a stand-in executable writes the report and its working folder; the run passes, reads `ticks_run`, ran in the export folder, built nothing; a missing executable fails with FILE_NOT_FOUND). Limit: the export must be built for the machine running the test (FOREVER BEAR's Windows zip needs Wine or Windows).
- [x] 106. `rusting run --bench` says whether the frame is CPU- or GPU-bound (forever-bear F49, second suggestion). `timings.bench` gains `cpu_p50_ms` (update plus extraction, preparation and recording, without GPU waits), `gpu_p50_ms` (summed GPU pass time) and `bound` (`"cpu"`/`"gpu"`, `null` without timestamp queries); the human output prints a `cpu-bound: ...` line. guide/look-and-feel says LOD, fewer triangles and smaller shadow maps only help a GPU-bound frame, and that each `.rlod` level is its own instanced batch. Evidence: unit test `a_bench_line_round_trips_its_frame_times` (CPU-heavy, GPU-heavy and untimed frames). Not run on a window here; the numbers are hardware-only. One batch for all LOD levels is item 126.
- [x] 107. Scenarios report and budget the live entity count (surveyor F8, F13). `perf.entities_max` in `rusting test --json` is the most entities after any tick, not counting engine resources (bevy 0.19 resources are entities); `budgets.max_entities` fails the run with `budget: max_entities: N entities, over the limit of M`. docs/concepts.md and the `budgets` schema text say so, and that `world().entities().len()` counts allocated slots. Evidence: unit test `perf_is_reported_and_budgets_fail_the_run` (count above 0; a limit one below the measured count fails). Not done: a `scene:` expect path for the count.
- [x] 108. Game code edits a player controller (surveyor F9, unclaimed F7): `scene.set_player(name, |pc| ..)` changes `walk_speed`, `sprint_multiplier`, `jump_speed` and the other settings, like `set_hud`; the agent skill lists it. Crouch already exists (`player.crouch`, `crouch_height`). Also marked surveyor F10 (`counter_or`), F16 and F19 (scenario `counter` steps create a missing counter) and F21 (player action names are in manual/concepts and the skill) as already fixed in the engine; the game's 2.0.3 CLI was older. Evidence: unit test `set_player_edits_the_named_controller` (edits the controller; false for an object without one and a missing name).
- [x] 109. `scene.spawn_copy_at_root(template, name, transform)` copies a template tree with no parent and a full world transform, so copies of kit pieces kept under a hidden, offset root neither inherit that root nor need `reparent` and `set_rotation` afterwards (surveyor F4, F6). The agent skill lists it. Evidence: unit test `spawn_copy_clones_the_template_tree_and_in_class_lists_it` now also spawns a root copy and checks it has no `Parent` and exactly the given transform. Not done: a prefab file format.
- [x] 110. Game code draws into a texture it created (surveyor F14): `scene.edit_texture(handle, |texture| ..)` edits a `create_texture` texture and bumps its revision, so the renderer uploads it before the next frame; `TextureAsset`, `TextureColorSpace` and `TextureSampler` are in the prelude, and the agent skill lists both calls. Also marked surveyor F5 (light fields and units are in `rusting schema --json`) and F15 (`spot_light.shadows` exists) as already fixed; the game's CLI was older. Evidence: unit test `game_code_draws_into_a_texture_it_created` (edit applies, revision grows); `gameplay_methods_do_not_return_new_borrows` holds.
- [x] 111. The headless render size is findable (surveyor F12): guide/look-and-feel says `capture_size` is the render size (resolution) of every headless frame, with 1080p and 1440p values and the 1280 x 720 default. Evidence: unit test `the_headless_render_size_is_found_by_its_usual_names` (`resolution`, `render size`, `1080p` find the guide; fails on `resolution` without the paragraph).
- [x] 112. A timing budget says when the machine was busy (surveyor F20): `perf.environment` gains `cpus` and `load_average` (one-minute, Linux `/proc/loadavg`, null elsewhere), and a `max_tick_ms`, `mean_tick_ms` or `p95_tick_ms` failure with load above half the CPUs ends with `(load average L on N CPUs: rerun it alone before treating it as real)`. The budgets schema text says so, and `determinism` help says it builds both profiles and takes no `--release`; `until` and `within` were already documented as absolute ticks. Evidence: unit test `perf_is_reported_and_budgets_fail_the_run` (load 6.5 on 4 CPUs adds the note; load 1.0 does not). Not done: a CPU-time budget.
- [x] 113. Game code reads the run seed (unclaimed F5): `scene.seed()` returns the scenario `seed`, `run --seed` value, or 0, for things generated once such as a layout; the agent skill lists it. Evidence: unit test `game_randomness_repeats_for_a_seed_and_varies_by_tick_and_stream` (0 without a seed resource, 42 with one).
- [x] 114. Scenarios count and budget the lights a frame uses (unclaimed F6): `RenderCapacityDiagnostics::lights` holds the directional, point and spot lights in the latest light upload, `perf.render.lights` reports it, and `budgets.max_lights` fails the run above its limit and, like `max_draws`, renders every tick. Evidence: GPU test `a_render_budget_reports_render_counters_without_a_capture` (one point light gives `lights` 1; RTX 3060, under `flock`); unit test `perf_is_reported_and_budgets_fail_the_run` (33 lights over a limit of 32 gives `max_lights: 33 lights, over the limit of 32`).
- [x] 115. Text on meshes in a game's own font (unclaimed F11): `scene.set_text_in_font(name, text, style, "fonts/x.ttf")` draws with a TTF or OTF file under `assets/`, and `text_texture_in_font` returns `None` for bytes that are not a font instead of panicking. Evidence: unit tests `a_font_file_changes_the_glyphs_and_bad_bytes_give_none` and `game_code_puts_text_and_materials_on_existing_objects` (a font file gives a new texture; a bad or missing file gives `None`).
- [x] 116. A scenario `set` of `/transform/rotation` on a player turns its controller (unclaimed F9): the controller rewrites its body's rotation from `yaw` every tick, so the set copies the rotation's x and y into `pitch` and `yaw`; the project AGENTS.md says so next to "place the player". Evidence: unit test `set_turns_a_player_controllers_look` (a rotation set of `[0.2, 1.5, 0]` reads back as yaw 1.5, pitch 0.2 and body rotation y 1.5 a tick later).
- [x] 117. A scenario walks the player along a route (surveyor F17): `explore.goals` takes `[x, y, z]` points as well as entity names, so a list of points steers the player controller through a maze in order; the report names a point goal by its coordinates. Evidence: unit test `explorer_walks_to_reachable_goals_and_reports_caged_ones` (a two-point route is reached in order; `["Coin", [1, 0, 2.5]]` parses to a name and a point).
- [x] 118. Visibility queries for game code (surveyor F18): `scene.on_screen(point)` gives a world point's place in the active camera's view as fractions, or `None` behind the camera or outside the view, and `scene.raycast_visible` passes through hidden objects and their children (templates), while `raycast` still hits invisible walls. A batch raycast was left out: a loop over `raycast` costs the same. Evidence: unit tests `on_screen_places_points_in_the_view_and_rejects_the_rest` (center, right, top, behind, off the side) and `raycast_visible_passes_through_hidden_objects_and_their_children` (`raycast` hits the hidden template's desk, `raycast_visible` the wall behind it).
- [x] 119. Incremental transform propagation (unclaimed F8): when no entity was reparented, added or removed and the hierarchy is clean, `propagate_transforms` recomputes only entities whose `Transform` changed and their descendants, in (depth, entity) order, instead of the whole hierarchy. Static props then cost nothing per tick. A `static` flag and a per-system `RUSTING_PERF` breakdown were left out. Evidence: unit test `moving_part_of_a_hierarchy_matches_a_full_rebuild_bit_for_bit` (fails when the incremental pass is skipped); a scratch release bench of 3000 roots with 6 children each and one mover went from 5.99 ms to 0.53 ms per tick (load average 10 to 16).
- [x] 120. A scenario capture that names the camera the game already shows alone keeps the HUD (unclaimed F12): the capture is the game's own frame. A different camera or a split view still renders through that camera without the HUD. Evidence: unit test `capturing_through_the_shown_camera_keeps_the_hud` (nothing drawn, the shown camera, another camera, a split view).
- [x] 121. A compact `rusting test` result (unclaimed F10): a single-scenario result leaves out the per-tick `state_hashes` and `gpu_state_hashes`, which were most of its size; `--full` keeps them and build/scenario-report.json always has the whole report. Folder runs were already compact. `trace` and `steps` stay, since they explain a failure. Evidence: unit test `test_results_leave_the_state_hashes_out`; `rusting determinism` still reads the hashes from the untrimmed internal result.
- [x] 122. `CLI_OUTDATED` names what the CLI misses (surveyor F3): when the engine source is newer than the CLI and is a git checkout, the warning lists up to five engine commits made after the CLI was built (`hash subject`, newest first, then "and N more"), so a reader sees whether the change matters. Evidence: unit test `outdated_cli_names_the_engine_commits_it_misses` (a time just before the last commit lists that commit; a future time lists nothing).
- [x] 123. A CPU-time budget (surveyor F20): `perf.cpu_ms_mean` is the CPU time of all game threads per tick over the whole run, from `/proc/self/stat` (Linux; null elsewhere), and the `mean_cpu_ms` budget limits it, so a budget can hold on a shared machine where wall-clock ticks slow 2-3x. Resolution is 10 ms over the whole run. Evidence: unit test `perf_is_reported_and_budgets_fail_the_run` (measured on Linux; an unmeasured value is not checked; 5 ms over a 1 ms limit fails with `mean_cpu_ms`).
- [x] 124. Where tick CPU time goes (unclaimed F8 follow-up): `CpuFrameTimings` gains `update` and `post_update`, the `RUSTING_PERF` line prints `CPU fixed / update / post_update / extract`, and a scenario reports the per-tick mean of each as `perf.stages_ms_mean`, so a game can tell its own systems from engine work such as transform propagation. Per-system timing was left out: `bevy_ecs` schedules have no per-system hook without its tracing feature. Evidence: unit test `perf_is_reported_and_budgets_fail_the_run` (all four stages measured above zero).
- [x] 125. Prefab files from game code (surveyor F4): `scene.spawn_prefab("prefabs/tile.rscene", "Tile 1", transform)` places a scene file under `assets/` as a linked instance with a named root and a full transform, through the existing `AssetServer::load_prefab` and `spawn_prefab`. Evidence: unit test `spawn_prefab_places_a_scene_file_under_a_named_root` (root name and transform, the file's object as its child, a missing file gives `None`).
- [x] 126. One batch for all LOD levels (forever-bear F49, rest). An opaque object in a `.rlod` group is one instance with its finest mesh; the batch draws once per level (`PreparedRenderBatch::first_draw`/`levels`, meshes in `draw_meshes`), and the CPU visibility pass or the GPU cull shader (`CullInstance::lod_ends`) sorts each instance into its level's range of the visible list. Blended objects keep one instance per level. Groups hold at most `MAX_LOD_LEVELS` = 4 levels; the loader refuses more. Evidence: GPU test `lod_groups_draw_the_level_for_the_camera_distance_on_every_path` (one batch, one instance, draws per level on the CPU, frustum and occlusion paths; blended objects still switch); unit tests `compaction_keeps_batches_contiguous_and_slots_blended_instances`, `lod_level_picks_the_range_holding_the_value`, `lod_group_beside_a_mesh_loads_with_it_and_rejects_bad_hand_overs` (5 levels refused). Hardware: ignored test `lod_crowd_cpu_cost`, 2085 swaying objects with a 2-level group, release build, RTX 3060: render CPU 0.298 ms to 0.174 ms a frame (0.127 ms without the group).
- [x] 127. Process memory in scenario reports (forever-bear F47, rest): `perf.rss_mb` is the game process's resident memory at the end of a scenario and `perf.rss_mb_peak` its peak, from `/proc/self/status` (Linux only, null elsewhere), so a soak scenario can show no growth. Docs: "Memory" in `docs/concepts.md`. Evidence: `perf_is_reported_and_budgets_fail_the_run` checks both on Linux; `rusting test --json` on a starter project reported `rss_mb` 23.2 and `rss_mb_peak` 23.2. Full check 2026-10-10: fmt and clippy x3 clean; 789 lib tests, 886 with gpu-tests, 18 doc tests.
- [x] 128. Static props are cheap (unclaimed F8, rest): after the dirty-transform propagation of b6281b6, 20,000 static meshes with a child each (40,000 entities, no physics or scripts) cost 0.58 ms of CPU a tick in release under the full game plugin stack (fixed 0.015, update 0.017, post_update 0.14, extract 0.41 ms), about 14 ns an entity against the 0.5 µs reported, so no `static` flag is needed. Per-stage CPU time is `perf.stages_ms_mean` (8841020). Evidence: `static_props_cost_little_per_tick` (ignored, timing; `cargo test --release --lib static_props_cost -- --ignored`) asserts under 5 ms. Full check 2026-10-10: fmt and clippy x3 clean; workspace tests pass, also with gpu-tests.
- Parallel gpu-tests on the RTX 3060: the clean HEAD (before this track) also fails 5 of 10 parallel runs (a segfault, a hang and failures), so this is not new. A hung run stops inside the NVIDIA driver in `vkDestroyCommandPool` while a test drops its command buffer allocator. The ninth `rusting-core` increment above already records the same SIGSEGV; run gpu-tests with `--test-threads=1` as before. On llvmpipe the parallel run passes.
- Full check (2026-10-06): fmt clean; clippy clean with default features, `--no-default-features`, `--features gpu-tests` and `--features steam`. `cargo test --workspace` passes (597 lib). `cargo test --workspace --features gpu-tests -- --test-threads=1` passes (684 lib, 27 CLI, 5 vertical slice, 17 doc tests). Parallel gpu-tests runs were flaky: one run failed about 30 scene_renderer tests, one hung in `pbr_maps_and_unlit_model_shape_the_lit_color`, and a third run passed all 684. Each failing test passed alone. The failure messages of the first run were not kept. A later serial run had two intermittent failures, `headless_device_creation_succeeds_with_a_vulkan_driver_present` and `devices_enable_anisotropy_when_the_driver_supports_it`, both at device creation (`src/rendering/mod.rs:117`); both passed alone and the next full serial run passed all 684. `rusting test samples/forever_bear_booth` passes after the final formatting.

### Rusting Raft track

Feedback from building Rusting Raft on v2.1.0.

- [x] 1. Built-in networking: `rusting_engine::net::NetSession` hosts (`host(port)`), joins by address (`join`), or goes through a relay by room code (`host_room`, `join_room`), with reliable, ordered, length-prefixed messages over std TCP and a versioned handshake. `rusting relay [address]` runs the relay. Clients talk only to the host. No UDP, replication, prediction or encryption yet; see `guide/networking`. Evidence: unit tests `a_direct_host_and_two_clients_exchange_messages_in_order` (ids, order, broadcast, per-peer send, disconnect, port freed on drop), `a_relay_pairs_a_host_and_a_client_by_room_code` (unknown code refused, codes ignore case, both directions, leave), `a_host_refuses_another_protocol_version`; 30 repeated runs passed.
- [x] 2. Swimming: `PlayerController.swim_speed` above 0 makes the body swim in a `WaterBody` whose surface is above its center: it moves at `swim_speed`, floats with its center `float_depth` below the waves, dives while crouch is held, rises while jump is held, drifts with the current and jumps out from the surface. 0 (the default) keeps the old walk-on-the-bottom behavior. Evidence: unit test `swimming_players_float_at_the_surface_dive_and_rise` (floats at -0.5, dives below -2, rises back, sinks with swimming off).
- [x] 3. Turning platforms: player controllers riding a kinematic floor follow its rotation as well as its translation, and their yaw turns with it (2D platformers stay translation only). Evidence: unit test `a_turning_platform_carries_and_turns_its_rider` (a quarter turn moves a rider at x 1.5 to z -1.5 and turns its yaw by pi/2); `controllers_ride_moving_platforms_and_stop_at_ceilings` still passes.
- [x] 4. Hard shadows: `RenderSettings.hard_shadows` (`scene.set_hard_shadows(true)`, or Hard shadows in the editor's project settings) samples the shadow map once per pixel instead of 3x3 PCF. Evidence: GPU test `hard_shadows_leave_no_soft_edge` (RTX 3060): across one shadow edge, PCF leaves 8 ramp pixels (208, 152) and the hard filter none.
- [x] 5. Flat shading: `MaterialAsset.flat_shading` lights each triangle with its screen-space derivative normal, so it works on any mesh, including generated ones; `WaterBody.flat_shading` turns it on for water. Scene format 10 stores it; version 9 cooked scenes migrate. Evidence: unit test `version_eight_and_nine_cooked_scenes_migrate_their_materials`; schema example round trip `examples_load_through_the_scene_parser_and_registry_unchanged`.
- [x] 6. Window title: `"window_title"` in project.json and `scene.set_window_title(title)`. Evidence: unit test `project_json_window_size_is_asked_for` (reads "Rusting Raft").
- [x] 7. Many moving objects: when only `GlobalTransform`s changed, extraction updates those objects in place instead of hashing, collecting and sorting every renderable, and mesh preparation keys on a new `renderables_layout_revision` that moving does not bump. Measured in a release build with 10,000 objects (CPU extraction including transform propagation, this machine): one object moving 6.4 ms to 0.36 ms a frame, all moving 9.8 ms to 1.6 ms, static 0.5 ms to 0.23 ms. The renderer then patches the moved instances' matrices and bounds in its packed instance list instead of repacking every instance: `prepare_render_instances` with 10,000 objects went from 0.98 ms to 0.15 ms per frame in a release build (it still compares every matrix, marked `ponytail:`). Evidence: unit test `extraction_tracks_changes_removals_and_stable_order` (a move changes one object without a new layout revision); GPU test `a_move_patches_instances_without_repacking`, which fails when the patch skips the model matrix.
- [x] 8. Copies of rigged models animate: path steps in `find_target` and `SceneTree::child` also match a copy's `"<copy>/<child>"` names, so animation tracks, skin joints, ragdolls and retargeting find the copy's bones. Copies of unnamed templates still have unnamed children. Evidence: unit test `spawn_copy_clones_the_template_tree_and_in_class_lists_it` (nested and `..` paths resolve inside the copy; the template still resolves to itself).
- [x] 9. Network passwords: `NetSession::host`/`join` take a password and `host_room`/`join_room`/`run_relay` a relay token (empty means none), checked in the JOIN/HOST_ROOM handshake with a constant-time compare; `rusting relay` reads `RUSTING_RELAY_TOKEN`. `PROTOCOL_VERSION` is 2. Evidence: unit test `a_host_and_a_relay_refuse_the_wrong_secret` (wrong, empty and wrong-case secrets refused, refused clients never reach the host, right ones join); it fails when the compare always passes.
- [x] 10. Fast raycasts: `PhysicsWorld` keeps a ray tree (a median-split bounding-box tree, four items a leaf) over the colliders' bounding spheres, built on the first ray after the bodies change, and each triangle-mesh, heightfield and hull collider keeps one over its triangles. A ray visits only the boxes it crosses and still returns exactly what the old scan of every item returned (ties go to the lowest index). Evidence: unit test `ray_trees_find_the_same_hits_as_scanning_every_item` (a 64x64 bumpy heightfield and 500 bodies, 400 rays, hits equal to a brute-force scan bit for bit). Hardware, `cargo test --release --lib raycast_cost_per_ray -- --ignored --nocapture` on a Ryzen 5 7600X: 0.35 us a ray over 201 colliders (2.1 us before, item 88 of the Signal track), 0.29 us a ray over a 256x256 heightfield of 131,072 triangles. Limit: the collider tree is rebuilt on the first ray after each tick that changed the bodies.
- [x] 11. `scene.raycast_only(origin, direction, max_distance, &["island"])` hits only objects in the named classes and passes through the rest; an empty list hits nothing. Docs: `docs/cameras.md`, the agent skill. Evidence: unit test `raycast_only_hits_the_named_classes_and_passes_the_rest`.
- [x] 12. The over-the-shoulder camera stops at walls: the player controller casts the camera sphere from the orbit center along the yaw-turned `camera_offset` first, shortens the shoulder offset at a wall beside the player, then casts back from that shoulder point. Evidence: unit test `over_the_shoulder_camera_stops_at_walls_beside_and_behind_the_shoulder` (yaw 0 and a quarter turn; a pillar behind the shoulder and a post beside it).
- [x] 13. Vertex colors: `MeshVertex::color` (linear RGBA, white by default) multiplies the instance color in the main vertex shader, so it also reaches alpha testing and the depth prepass; glTF `COLOR_0` is imported. `.rmesh` files now start with an 8-byte `RMESH\0\0\x02` header (`encode_mesh_file`); files without it load as the old layout with white vertices, and the header read as an old vertex count is far larger than any file, so the two cannot be confused. Docs: "Vertex colors" in `docs/look-and-feel.md`, the agent skill. Evidence: unit tests `rmesh_files_cooked_before_vertex_colors_load_white`, `native_mesh_loader_restores_imported_mesh_data` (tinted vertices round-trip), `gltf_import_keeps_vertex_colors_through_the_cooked_file`; GPU test `vertex_colors_tint_the_material_color` (blue vertices on a white unlit material render blue; RTX 3060).
- [x] 14. Flat shading is safe on every driver: the fragment shader takes the derivatives of the world position at the top of `main`, before the alpha-test `discard` (derivatives after a discard or in non-uniform control flow are undefined), and keeps the vertex normal when a sliver triangle gives no face normal instead of normalizing zero into NaN. Evidence: GPU test `flat_shading_lights_a_flat_face_like_its_vertex_normals` (an alpha-tested flat-shaded face renders within 2 levels of the smooth one; RTX 3060). Not run on the drivers that misbehaved; that is hardware-only.
- [x] 15. Relay limits for the open internet: `net::run_relay_with(listener, token, RelayLimits)`; `run_relay` and `rusting relay` use the defaults of 1024 connections in all, 16 open and 60 new a minute per IP address (refused ones count, which slows token guessing), and 1 MiB/s read from each connection by a token bucket that slows a sender instead of cutting it off. A failed write closes that peer, so a half-written frame cannot garble its stream. Docs: "Limits" in `docs/networking.md`, `rusting schema` relay text. Evidence: unit tests `a_relay_limits_connections_per_address` (a third connection from one address and a fifth connect in a minute are refused), `a_throttle_holds_a_reader_to_its_rate`. Not done: `rusting relay` has no flags to change the limits.
- [x] 18. Handshake hardening (security review of item 15): a handshake must arrive whole within 5 s however slowly its bytes trickle (the old timeout was per read, so one byte every 4 s held a join slot forever and 64 such peers blocked every join to a direct host), handshake frames are capped at 64 KiB, every frame buffer grows as bytes arrive instead of reserving the declared length up front (a bare 16 MiB length prefix on each of 1024 relay slots was 16 GiB), and the relay's per-address limits count an IPv6 /64 as one address, since one home connection owns a whole /64. Evidence: unit tests `a_handshake_must_arrive_whole_in_time_and_stay_small` (a peer trickling a byte every 50 ms times out at the deadline; an oversized length is refused at once) and `relay_limits_count_an_ipv6_slash_64_as_one_address`. Full check 2026-10-10: fmt and clippy x3 clean; 820 lib tests, 921 with gpu-tests. Limit: a direct host has no per-address limit, so many addresses connecting nonstop can still fill its 64 handshake slots.
- [x] 16. The relay keeps running after a failed incoming connection: accept errors are skipped after 10 ms instead of ending the accept loop. Evidence: none automated; an accept error is hard to cause in a test. Covered by reading `run_relay_with`.
- [x] 17. One silent client no longer delays other joins: hosts and relays run each handshake on its own thread with a timeout, at most 64 at once. Evidence: unit test `a_silent_connection_does_not_delay_another_join` (a second client joins while the first sends nothing). Full check 2026-10-10: fmt and clippy x3 clean; `cargo test --workspace` 818 lib, 5 vertical slice; `--features gpu-tests -- --test-threads=1` 919 lib, 32 CLI, 18 doc tests.
