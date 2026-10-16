# Changelog

## [Unreleased]

### Added

- `GameScene::scroll_delta()` returns the mouse wheel lines turned since the last frame, or in a tick function since the last tick, for hotbars and zoom.
- `rusting test` on a scenario folder cooks and builds the game once, then runs every scenario with that build, instead of running `cargo build` before each one. The result gives the build time as `build_ms`.
- `rusting.water` and `WaterShading` take `crest_foam`: the crests of steep Gerstner waves turn white where the waves pinch the surface together (the Jacobian of their sideways sway).
- Water and ocean waves now move in the scene vertex shader. The surface grid uploads once instead of every tick, so large seas cost less CPU time.
- `GameScene::set_max_texture_size(px)` and `RenderSettings::max_texture_size`: textures larger than the cap upload at half size until they fit, so a Low preset loads 2K textures at 1K (RAFT).
- `ShadowQuality` sets the sun's cascade count: 2 on Low, 3 on Medium and 4 on High, so low settings render fewer shadow views (RAFT).
- `GameScene::resolved_quality()`: the quality profile the renderer draws with, `Auto` resolved from the GPU, so game content can scale with it (RAFT).
- `rusting.render_layer` (`RenderLayer::Viewmodel`) on a mesh and `rusting.viewmodel_camera` (`ViewmodelCamera { vertical_fov_radians }`) on the active camera: held tools draw over the world with their own field of view and never poke into walls (RAFT).
- `ColorGrading::lens_droplets` (0 to 1): rain drops on the camera lens that bend the image behind them and fade after a few seconds, for first-person views in storms.
- `GameScene::set_wetness(0..1)` and `MaterialAsset::wettable`: wettable materials darken and turn glossy as the global wetness rises, so decks, sails and rocks get wet in the rain, not only terrain.
- `rusting.particle_blocker` boxes and the emitter option `blockers`: CPU particles die inside the boxes and below `rusting.heightfield` ground, so rain stops at roofs (RAFT).
- `rusting.procedural_sky` draws clouds: `cloud_cover` (0..1), `cloud_darkness` (0..1), `cloud_speed` (m/s) and `wind_direction` (world x, z). They glow at their edges toward the sun and hide its disc; lighting still sees the clear sky (RAFT).
- `rusting.water` `ocean: true` makes an endless sea: bodies float and players swim everywhere, and the surface grid follows the active camera with cells that grow away from it (RAFT).
- `rusting.water` takes `waves`, a list of Gerstner `OceanWave`s (`direction`, `length`, `height`, `steepness`) with sharp crests and analytic normals in place of the three sines, and `time`, a game clock for the waves in place of the fixed tick. `WaterBody::wave` returns the swayed surface and takes its time as `f64`, so waves keep their shape hours in; `WaterBody` is no longer `Copy` (RAFT).
- Scenario step `{"scroll": [dx, dy]}` turns the mouse wheel over the cursor, so a scenario can scroll an egui `ScrollArea` and `click` rows below the fold. Game code reads the wheel with `RuntimeInput::scroll()`, in lines, from the window too (RAFT).
- Scenario step `{"key": "SuperLeft"}` taps one key by its `KeyCode` name, for keys no action uses (RAFT).
- `GameScene::set_cursor_captured(bool)` grabs or frees the cursor; called on the frame Esc went down, it keeps the player controller from freeing the cursor for an Esc the game used. `set_mouse_look(.., true)` now grabs the cursor at once while the window has focus, and losing focus frees it (RAFT).

### Changed

- The relay (`rusting relay`, `net::run_relay`) is safe to expose for `NetSession::host_room` / `join_room`: each connection writes through its own bounded queue, so a peer that stops reading is cut off instead of stalling every room; `RelayLimits` gains `max_rooms` (256), `max_clients_per_room` (64), `room_attempts_per_minute` (30, per address), `idle_timeout` (10 minutes after the handshake) and `send_queue` (1 MiB, split into equal shares per client on a host's connection); a sender over `bytes_per_second`, or a message larger than one second of it, is now dropped instead of slowed (RAFT).

### Fixed

- `Orientation` keys and crossfades blend the short way round, so a key and its negated twin no longer collapse or spin the bone, and a crossfade eases a transform track only the old clip keys back to its rest value instead of leaving it where the old clip stopped (RAFT).
- `GameScene::raycast` and its variants hit a scene's colliders from the first fixed tick: loading a project, `load_scene` and `restart` build the physics bodies at once, with child colliders at their propagated poses, instead of after the first physics step (RAFT).
- `FrameTime::real_delta` reads the real frame time again while an overloaded app runs in slow motion; only the game clock is clamped to one fixed step. Replays record the real delta beside the clamped one (`ReplayFrame::real_nanos`) (RAFT).

## [2.5.6] - 2026-10-14

### Added

- `GameScene::exiting()`: game code gets one last update, with no tick and `exiting()` true, when the window closes, after `scene.quit()`, and when a dedicated server gets Ctrl+C or SIGTERM (a console close on Windows), so it can write its save. `App::run_exit_update` runs it for custom loops. `save_data` already writes through a temporary file and a rename (WILDROOT F25).
- `rusting run --bench --json` reports `passes_p50_ms`, and `rusting test --json` reports `perf.render.passes_ms`: GPU milliseconds per pass, per shadow cascade, and per opaque, terrain, foliage and sky part of the scene pass. The plain `bench:` output names the three costliest passes (WILDROOT F15).
- `rusting.terrain` layers take optional `normal` (OpenGL-style normal map), `roughness` (red channel) and `tiling` (metres per repeat; 0 uses the terrain's) fields. Layers without them look as before (WILDROOT F3).
- The net join handshake carries the host's world seed: the host calls `NetSession::set_seed(scene.seed())`, a client reads `NetSession::seed()` after joining, directly or through a relay, and applies it with the new `GameScene::set_seed`. The wire protocol is now version 4, so hosts, clients and relays must all be updated together (WILDROOT F21).
- `rusting.terrain` takes a terrain-wide `tint` (linear rgb, default white) multiplied into every layer and a `roughness` (default 1) that scales every layer's roughness. Changing only these two updates the terrain material without rebuilding chunk meshes, LOD groups or layer arrays, and `GameScene::set_terrain_look(name, tint, roughness)` sets them without a trip through JSON. A material color on a terrain now tints the blended layers instead of skewing the splat weights (WILDROOT F23).
- `net::interpolate::Track` blends a remote object's plain `(tick, position)` samples for games that send their own snapshots, without a replica or `NetTransform`: `push(tick, position)` in any order, `at(tick)` with a fractional tick, `newest()` (WILDROOT F18).
- `rusting asset import --linear` (and `reimport --linear`) marks an image as data and cooks `<name>-linear.rtexture` beside it, which the `reference` field points at. A plain image in a material's normal, metallic-roughness or occlusion slot now loads as linear data, from scene files and from the Inspector. `AssetServer::load_linear_texture` and `assets::cook_linear_texture` expose both paths (WILDROOT F24).
- `rusting.foliage` takes `conform` (default false), which sets each copy on the heightfield below it so a scatter follows hills, and `layers` (default empty), terrain layer indices a copy must stand on, such as grass but not sand. Copies are placed again when the object, heightfield or terrain changes (WILDROOT F12).
- Materials take a `sway` block (`MaterialSway`: `amplitude`, `frequency`, `height`) that bends every mesh using them in the `rusting.force_field` winds at its origin, in the vertex shader, so tree crowns sway for no CPU cost. `frequency` adds gusts phased by position and `height` keeps the base rooted while the crown curves. The editor material inspector shows it, and cooked scenes move to format version 11; version 10 scenes load without sway (WILDROOT F13).
- `rusting.water` takes optional `deep_color`, `depth_fade`, `shore_foam`, `ripples` and `ripple_length`: the surface fades to the deep color by the metres of water the view crosses, draws white foam where the ground nears the surface, and lights small scrolling waves. They read the opaque scene depth, which the renderer now copies for water even with reflections off. The defaults keep the plain one-color surface (WILDROOT F7).
- glTF import accepts `KHR_texture_transform`: the offset, rotation and scale the material's textures share are folded into `TEXCOORD_0`, and textures with different transforms fail with an error naming the material instead of `Unsupported extension` (WILDROOT F8).
- `PlayerController::step(physics, body, shape, position, input, water, floor_pose, dt)` runs one fixed tick of player movement from a `PlayerInput` (`forward`, `right`, `jump`, `jump_held`, `sprint`, `crouch`) instead of the global `player.*` actions, so a host can move every peer's body, and a client can replay its own inputs for prediction, with the same code as `rusting.player_controller`. It returns the dynamic body walked into as a `PlayerShove` (WILDROOT F16).
- `rusting asset bake-foliage <model> [--cards N] [--impostor] [--node NAME] [--tint RRGGBB] [--atlas PIXELS] [--out PATH]` bakes a tree's `MASK` or `BLEND` foliage into crossed alpha-tested cards with one RGBA atlas (4 triangles per card), and with `--impostor` a crossed-billboard impostor of the whole tree for the last LOD. It prints the source, foliage and card triangle counts. `foliage_bake::bake_foliage` exposes it to code (WILDROOT F9).
- `rusting asset import` and `asset reimport` record every file they cook beside a model (its `.rmesh` meshes, `.lodN.rmesh` levels, `.rlod` groups and `.rtexture` textures) in the new optional `.rmeta` field `cooked`, and the report's new `meshes` list gives each mesh's triangle count and its LOD triangle counts, so a 4-million-triangle tree shows up at import time instead of in a frame-time capture (WILDROOT F1).
- `rusting docs show api/net`, `api/net::predict`, `api/net::input`, `api/net::interpolate` and `api/procgen` list each module's public items, and every type, function, constant and type alias there has its own `api/<name>` page with its signature and docs, such as `api/PeerId`, `api/NetSession`, `api/fbm2` and `api/RandomStream`. `Smoothing::correct` now says which position to pass as `shown` (WILDROOT F17, F18, F2).
- `rusting lint` warns `LINT_TERRAIN_CHUNKS` when a terrain draws more than 300 chunks (WILDROOT F11).

### Changed

- Cooldowns have their own namespace: `start_cooldown(name, ..)` keeps its end tick in the counter `cooldown:<name>`, so a counter called `name` is a separate value and a cooldown no longer overwrites it. Saves and scenarios load as before; a game that saved a cooldown through `save_counters` passes `cooldown:<name>` instead. Statuses keep their `<object>/<effect>` counters (WILDROOT F26).
- Terrain layers are kept in a mipmapped texture array at full size up to 2048 px (was a 2x2 atlas of tiles cut to 512 px) and sampled with anisotropic filtering, so distant terrain shows no grid lines or moiré at layer repeats (WILDROOT F5, F6).

### Fixed

- A `"realtime": true` scenario times `NetSession::simulate` delays from each tick's planned time instead of the wall clock, so a slow `capture` frame no longer changes the tick a held message arrives on, and a copy of the scenario without its captures sees the same arrivals (WILDROOT F22).
- `rusting asset reimport` of a glTF recooks its `.rtexture` files next to the project copy, not only its `.rmesh` files, so a changed source texture shows. Reimport without `--from` keeps a project copy that was edited after the last import instead of overwriting it with the recorded original, warns `ASSET_EDITED_COPY`, and records the copy as the new original (WILDROOT F24).
- The engine builds for Windows again: a zip permission literal had no integer type there.
- Fixed-tick code (a game's `tick` function and every `FixedUpdate` system) sees `FrameTime::delta` equal to `fixed_delta`, so code that steps movement by `delta_seconds()` in a tick moves at the same speed whatever the frame rate. `Update` still sees the frame's time (WILDROOT F19).
- Terrain: the one-pixel vertical seam at the screen centre is gone. The atlas cropped each layer to 94% of its image, so every texture repeat drew a seam, and a camera at a round coordinate looking down an axis put one at the screen centre (WILDROOT F14).
- Terrain: layer `texture`, `normal` and `roughness` paths are relative to the scene file, like every other scene asset path. Paths relative to the project root still load, with a deprecation warning. `rusting lint` and `validate` report a missing layer image (`SCENE_MISSING_ASSET`) and a project-root path (`SCENE_TERRAIN_ROOT_PATH`), and a run logs once per path when a layer image fails to load instead of painting it white silently (WILDROOT F4).

### Performance

- Foliage: wind lean of `rusting.foliage` copies runs in the scene and shadow vertex shaders instead of on the CPU, so a wind field no longer re-uploads every copy's transform each frame. `Custom` force fields no longer lean foliage (WILDROOT F13).
- Shadows: each cascade draws only the casters inside it, at the finest LOD level any of them shows on screen, instead of every caster at full detail. WILDROOT's densest forest went from 13.2 ms to 2.4 ms of shadow GPU time, and from 25.8 ms to 17.1 ms per frame (WILDROOT F15).
- Terrain: `rusting.terrain` `chunk_cells` 0, the new default, picks the chunk size from the heightfield so a terrain makes at most about 150 chunks (a 1501 x 1501 island draws 144 instead of 2,209); an explicit size still wins (WILDROOT F11).
- CPU physics: `TriangleMesh` and `Heightfield` colliders test only the triangles under the other body's bounds through the mesh's box tree; one ball on a 1501 x 1501 heightfield steps in 0.05 ms instead of 40 ms (WILDROOT F10).

## [2.5.0] - 2026-10-12

Concave dynamic colliders, terrain destruction and debris budgets, decals and physics-driven particles, 2D rendering in the scene, world-space text, golden-image tests, packed asset archives, mods, and procedural generation. Needs `rusting-core` 0.2.1.

### Added

- `ConvexDecomposition` (`rusting.convex_decomposition`) makes a `ConvexMesh` collider follow a concave mesh as decomposed hulls on the CPU solver, so concave props can be dynamic; scene-file settings are clamped to 128 voxels per axis and 256 hulls.
- `runtime::decompose::decompose` splits a concave closed mesh into convex hulls (V-HACD-class approximate convex decomposition) for `ConvexMesh` colliders.
- Physics suite scenes `BuoyancySort`, `SheetCatch` and `JellyRaft` check two-way coupling between rigid bodies, cloth, soft bodies and fluid
- `SuiteScene::run_2d` runs 2D forms of the stacking, joint chain and CCD bullet suite scenes on plane-locked polygon bodies
- `SlideSound::by_material` picks a slide or roll loop by the physics material or surface the body slides on
- `TerrainFracture` digs a struck heightfield chunk out as a dynamic GPU slab
- `TerrainFracture::pieces` cracks dug-out slabs into Voronoi pieces seeded by the scene's `RandomSeed` at the tick
- GPU debris put to sleep by the GPU solver now counts as asleep, so the `DebrisBudget` can free dug terrain slabs at the cap
- `TerrainFracture::refreeze` freezes debris that rests asleep on the terrain back into its heights and sends a `Refrozen` event
- `DebrisAggregation` merges small sleeping debris into one static body per grid cell
- Rendering: `Sprite::in_scene` draws a sprite inside the 3D scene, hidden by nearer meshes, instead of over it
- Rendering: `rusting.camera_2d` follows a target with smoothing, limits, and a pixel-perfect mode
- Rendering: impact and settle particles of 2D bodies draw in the 2D pass
- Rendering: `rusting.particles_2d` draws particle emitters with the sprites in 2D canvas order
- Rendering: sprites take a normal map that 2D lights shade
- Rendering: 2D light occluder polygons cast shadows on sprites
- Rendering: round 2D lights and a 2D ambient color light sprites in the sprite pass
- Rendering: `rusting.sprite_animation` plays grid sprite sheets on a sprite, frame-stepped on the fixed tick
- Rendering: 2D sprite pass draws `rusting.sprite` in canvas order over the frame, one instanced draw per image batch
- Rendering: `rusting.sprite` component with canvas-ordered extraction and image batching for the 2D renderer
- Rendering: the `[rusting] perf` line reports press-to-present input latency
- Rendering: the FPS limiter keeps its cadence through late wakes and uneven frames instead of drifting slower
- Rendering: `FrameGraph` schedules a frame's enabled passes, their barriers, and transient resource lifetimes
- Tests: editor compositing golden image: the offscreen scene view drawn through egui under panels and overlays
- Tests: `assert_shader_layout!` checks render upload types field by field against the shader-reflected blocks
- Tests: a `shaders` CI job compiles every shader file with `glslc` through `scripts/check_shaders.sh`
- Tests: resize, minimize to a zero-sized surface, and restore redraw the scene correctly at each size
- Rendering: windows pick an RGBA swapchain on surfaces that prefer it instead of failing on a forced BGRA format
- Tests: the renderer shuts down validation-clean with frames in flight; `rendering::validation_error_count` counts validation-layer errors
- Tests: unloaded meshes and textures free their GPU data only after the frames that read them complete
- Tests: the low-end capability baseline falls back to Eco, CPU culling, no MSAA and the Eco light budget, and draws the Eco image
- Rendering: `CullingMode::Auto` draws scenes under 128 instances with no culling (was 64), the break-even measured on an RTX 3060
- Tests: GPU bodies are culled where the GPU moved them, with no state read back
- Rendering: `rusting.text` draws text in the world (signs, labels) with the built-in pixel font
- Fluids: Granular fluid blocks draw an opaque sand surface instead of translucent water
- Rendering: decals take a `roughness` painted with their coverage, so a dark decal can be a wet stain or a puddle that catches highlights
- Tests: a golden-image test of every primitive's normals
- Tests: a golden-image test of glTF texture channels and color spaces
- Tests: a golden-image test of alpha mask and blend modes
- Tests: a golden-image test of directional shadow depth
- Tests: a golden-image test of PBR reference spheres
- Tests: golden-image tests of the Linear, Reinhard and ACES tone mappers
- Tests: every culling mode and path draws the same image as no culling
- Tests: automatic culling bounds contain every vertex of the primitives and imported sample meshes
- Tests: a culled CPU rigid body keeps falling off view and is drawn once it enters the view
- Decals: `survive_fracture` moves a decal onto the nearest debris piece when its parent breaks, or removes it with the object
- Particles: `on_fracture` bursts `impact_count` particles at the break point when an emitter's parent object fractures
- Fracture: debris pieces keep the broken object's normals and texture coordinates on their outer faces
- Soft blocks: `color` and `texture`, laid once on each side of the block and fixed to the particles
- Release zips: an export preset with `"zip": true` zips the finished, signed and verified export folder into `<folder>.zip` beside it (stored, Unix modes kept, same bytes for the same folder); the export JSON reports `zip`.
- Workshop and store hooks: `PlatformServices` gains `workshop_items`, `publish_workshop_item`, `owns_content` and `open_store_page`, each with a default (no workshop, all content owned, no store page), with matching `platform::` functions; games load subscribed workshop items with their `mods/` folder (`mods::load_mods_with`). Mod loading no longer follows linked folders inside a mod (a link loop hung the game), compares scenes field by field only in `rusting mod list` and only up to 4 MiB, and prints mod file names with control characters replaced
- Mod conflicts: `rusting mod list` reports under `conflicts` each file more than one mod holds, and for scenes each entity and component field two mods changed differently; games print the conflicting files as they mount mods. `rusting mod pack` now needs `--out`, so `--confine` checks where it writes The scene comparison skips files that are not regular files of known size, so a mod scene linked to `/dev/zero` cannot hang it.
- Mods: folders or `.rmod` archives with a `mod.json` (name, version, dependencies) in a game's `mods/` folder replace or add its assets and scenes, in a load order from `mods/load_order.json` or by name with dependencies first; `rusting mod pack` packs one and `rusting mod list` shows the order and the mods left out
- Mesh generation: `meshgen::spline` (Catmull-Rom points), `extrude` (a profile swept along a path without twisting), `loft` (rings skinned into a closed mesh), and `ProcMesh::union`, `subtract` and `intersect`; `ProcMesh::glb` saves a mesh for a scene and `ProcMesh::asset` gives a flat-shaded asset that `TriangleMesh` and `ConvexMesh` colliders use
- Generated level checks: a `scene_generators` entry's `checks` scenarios (such as an `explore` run that must reach every goal, or `invariants`) play on each new scene before it replaces the old one; a failure keeps the old scene and gives `GENERATED_SCENE_CHECK_FAILED`. `RunOptions.scene` runs a game on any scene
- Scene generators: `scene_generators` in `project.json` name a project command that writes a scene from a seed and inputs; `rusting scene generate` reruns it only when the command, seed, inputs or watched files changed, checks the scene loads, journals the write, and refuses to overwrite hand edits without `--force`
- The API audit test covers every type in the prelude: a new public method returning a borrow, or a prelude type gaining a lifetime parameter, fails the build's tests
- Packed asset archives: export preset `pack: true` packs `assets/` and `scenes/` into `.rpak` files the game streams files from, with loose files still winning
- `procgen`: seeded random streams, gradient noise with fractal layers, and Poisson-disk point scattering that give the same bits on every machine
- `procgen` grid generators for tile maps and 3D grids: `cellular_caves`, `rooms_and_corridors` and `wave_function_collapse`, with `tile_rows` to fill a `TileMap`

### Fixed

- `rusting-core` 0.2.1: transforms with mirrored or flattened scale decompose without losing their matrix, and hierarchy cycle rejection is tested for deep, self and pre-existing cycles.
- A body that loses its `PhysicsBody` component no longer keeps a stale `PhysicsId`, and a `PhysicsId` slot is retired at its last generation instead of wrapping, so old GPU events can never reach a newer body

## [2.4.1] - 2026-10-10

Fixes 2.4.0 builds: without the Vulkan SDK the cloth shader failed to compile, and the crates.io package needed a newer `rusting-core` than the one published.

### Fixed

- `rusting_engine` builds from crates.io again: it depends on `rusting-core` 0.2.0, which carries the 2D, light and render settings types 2.4.0 uses (crates.io still had 0.1.8)
- The engine builds again without the Vulkan SDK: the cloth shader used a constant the bundled shader compiler cannot work out (`cloth.comp:23: global const initializers must be constant`)
- `chacha20` is updated from the yanked 0.10.0 to 0.10.2

---

## [2.4.0] - 2026-10-10

Rendering, deformable physics and multiplayer building blocks for larger games: modern shadows and post effects, soft bodies and cloth on the CPU and GPU, and replication, prediction and dedicated servers.

### Added

- Rendering: cascaded shadow maps for the directional light, so shadows stay sharp near the camera and still reach far away
- Rendering: point lights can cast shadows (`PointLight::shadows`), and point and spot lights take a `radius` for softer, wider highlights from a glowing sphere
- Rendering: clustered lighting for point and spot lights; a frame can now light up to 1024 local lights instead of 64
- Rendering: `rusting.light_cookie` projects an image through a spot light's cone
- Rendering: `rusting.auto_exposure` adapts exposure to the scene's measured brightness, like an eye adjusting
- Rendering: `rusting.color_lut` applies a color grading lookup strip, `rusting.depth_of_field` adds lens blur and `rusting.motion_blur` adds camera motion blur
- Rendering: `Antialiasing::Fxaa` and `Antialiasing::Taa` (temporal anti-aliasing with a jittered camera)
- Rendering: `RenderSettings::upscale_sharpness` gives a `render_scale` below 1 an edge-preserving, sharpened upscale like FSR 1
- Rendering: meshes carry vertex colors that tint the material; glTF `COLOR_0` is imported
- Physics: XPBD soft bodies (`rusting.soft_block`) that keep their volume, push out of colliders, attach to rigid bodies both ways, tear when overstretched and draw with a skin of their outer faces
- Physics: XPBD cloth (`rusting.cloth`) with wind, self-collision, pinning and tearing, drawn two-sided
- Physics: `GpuCloth` and `GpuSoftBody` step cloth and soft bodies on the GPU with the same results bit for bit as the CPU reference; cloth floor contacts arrive as physics events
- Physics: raycasts walk bounding-volume trees instead of testing every collider and triangle, with the same hits as before; `scene.raycast_only` hits only the named classes
- Networking: `send_unreliable` and `broadcast_unreliable` send per-tick state over UDP beside the TCP connection, falling back to reliable sends where UDP cannot get through
- Networking: remote procedure calls on scene objects (`net::rpc`), with a host, owner or anyone authority checked on the host
- Networking: replicated spawning and component sync (`net::replicate`): marked objects send compact per-component changes against the last snapshot each client acknowledged, with chosen fields quantized
- Networking: client-side prediction and reconciliation, clock synchronization, input tick offsets and a host-side input buffer that absorbs jitter and late inputs
- Networking: a player lobby with ready checks and reconnects within a grace time
- Networking: dedicated servers: `rusting run --server` (or `RUSTING_SERVER`) runs a game with no window or renderer and reports tick load
- Networking: `NetSession::simulate` adds latency, jitter and loss for testing, `NetSession::stats` counts traffic, and loopback sessions run in one process for tests
- Networking: per-peer rate limits (`RateLimit`) for client messages
- Docs & tools: a migration guide for Godot users, a plugin authoring guide, generated reference pages for every reflected type, and a release notes process in `RELEASE.md`
- `rusting.rope` hangs a rope of small bodies between two bodies, or a body and a world point; it goes slack, never stretches past its length, and tows what it is tied to.
- `rusting.mesh_surfaces` gives each triangle of a TriangleMesh collider its own friction, restitution and sound material, like heightfield cells.
- `GameScene::set_gpu_axis_lock` (`PhysicsSettings::gpu_locks`) locks every GPU body to chosen axes, so a 2D game's GPU bodies stay in its plane.
- 2D joints and queries are covered by a test: a ball socket on an axis-locked body acts as a 2D pin joint, and rays and overlaps hit `Polygon` and `Chain` colliders.
- `ColliderShape::Polygon` and `ColliderShape::Chain` collide with a `rusting.polygon` 2D outline extruded along Z: a convex polygon any body may use, or a static segment chain for 2D ground.
- `rusting.axis_lock` locks a CPU body's movement along or rotation around chosen world axes, Godot style; `AxisLock::PLANE_XY` keeps a 2D body in the XY plane exactly.
- `FieldKind::Custom` force fields call a plain Rust function the game registers by name with `App::add_force_field_function`; it gets a `FieldSample` (offset, velocity, time, direction) and returns the push per unit strength
- `rusting.force_field` now pushes GPU bodies too: the CPU step's field sensors upload each frame and `physics.comp` adds their push next to gravity, with wind gusts from a new bit-exact `sim_sin_cos`
- The CPU contact solver solves independent islands on all cores, bit-identical to one worker
- Heightfield `cells` and `surfaces`: each grid cell can have its own friction, restitution and sound material name (an ice patch on grass); `Contact::surface` says which one a contact touched
- Compound bodies: solid collider children of a dynamic CPU body move with it as one body; contacts and ray hits on them report the parent (they were fixed in place before)
- The CPU physics narrow phase runs on all cores; results stay bit-identical for any worker count
- `ColliderShape::Heightfield` with a `rusting.heightfield` height grid: static uneven ground for CPU bodies and the player; `Heightfield::mesh()` draws the same surface
- `rusting.force_field`: directional, radial, vortex and turbulent wind fields that push dynamic CPU bodies inside a sensor collider
- `rusting.physics_material`: a named collider surface with friction and restitution combine modes (Default, Average, Min, Multiply, Max); `SoundCue::with_material` plays a cue only on touches of that surface
- `rusting.vehicle`: raycast cars with spring-damper suspension, a slip-curve tire model, an open differential, an engine with flywheel inertia and an automatic gearbox; `player_input` drives one with the player actions
- Regression tests for ten-box stacks settling with cached contacts for a hit waking a whole sleeping stack, and for bullets against thin box, hull, triangle and dynamic walls.
- `GameScene::generate_ragdoll(name, total_mass)` and `runtime::ragdoll_bones` build ragdoll bones from a character's skinned joints.
- A `PlayerController` on a dynamic `RigidBody` is moved by the solver: walking steers it with a bounded force, jumps set its vertical speed, and it never tips over.
- Ground snapping for `PlayerController`: a walking player stays on the ground down ramps and steps no taller than `max_step_height` instead of hopping off them, so it can jump all the way down. `PhysicsWorld::snap_to_floor` gives the same snap to custom character code.
- `PhysicsSettings::substeps` and `ProjectRunner::set_physics_substeps` split each CPU physics step into equal substeps. Eight substeps hold a 20-link joint chain with a 50:1 tip within 2 cm, where one stretched it by metres; four rest 100 kg on 1 kg. Trade-off table in docs/concepts.md.
- `rusting.gravity_volume`: a sensor body that replaces gravity for CPU bodies inside it (zero-g rooms, wind tunnels, `toward_center` planets), with `priority` for overlaps
- Scenario reports give the game process's memory: `perf.rss_mb` at the end and `perf.rss_mb_peak` (Linux)
- Agent benchmark new-game tasks `lift-new-game` (a lift moving up and down that counts trips), `seed-planter-new-game` (numbered copies of a template, one a second) and `lamp-switch-new-game` (a key toggles a point light)
- Agent benchmark task `crate-plate-new-game`: a new-game task where pushing a crate onto a pressure plate opens a door
- Agent benchmark task `starter-flag-gate`: a bug-fix task on the starter template where the flag wins before the coins are collected
- Agent benchmark task `cube-quarter-turn`: a feature task on the 3d template where a key press turns the cube a quarter turn over 0.25 s
- Kill plane: `scene.set_kill_y(Some(y))` despawns loose dynamic CPU bodies that fall below `y`, with their children, and `scene.fell_out()` names them
- Agent benchmark task `swarm-glass-enemies`: a performance task where a blended enemy material makes every enemy its own draw call, held to a `max_draws` budget
- Agent benchmark task `jump-new-game`: a new-game task with a ball that jumps only while it rests on the floor
- Agent benchmark task `sandbox-reset`: a feature task that puts the physics sandbox's pile back at its start without reloading
- Agent benchmark task `sandbox-hail`: a performance task where over-detailed spheres break a `max_triangles` budget
- Agent benchmark task `fps-double-jump`: a bug-fix task where the first-person player can jump again in mid-air
- Agent benchmark task `tps-coins`: a feature task that adds three collectable coins to the third-person template
- `GameScene::rebind_device` rebinds an action on one device (keyboard and mouse, or gamepad) and keeps its other inputs; the `{binding:action}` HUD button now keeps the gamepad button when the player picks a new key

### Changed

- The network protocol version is 3. 2.3.0 games cannot join 2.4.0 hosts or relays, or the other way round
- `.rmesh` files have a version header; older files still load, with white vertex colors
- `PointLight` gains `shadows` and `radius`, `SpotLight` gains `radius`, and `RenderSettings` gains `upscale_sharpness`; struct literals need the new fields (or `..Default::default()`)
- `Antialiasing` has two new variants, `Fxaa` and `Taa`; an exhaustive `match` on it needs arms for them
- `AxisLock` moved to `rusting_core::components`; `rusting_engine::runtime::AxisLock` still works. A struct literal of `PhysicsSettings` needs the new `gpu_locks` field (or `..PhysicsSettings::default()`).
- `ColliderShape` has two new variants, `Polygon` and `Chain`; an exhaustive `match` on it needs arms for them.
- `ForceField` is no longer `Copy` (it gained `function: String`); write `..field.clone()` where you spread one field into another
- A `.rlod` group may have at most 4 levels; loading one with more fails

### Fixed

- A CPU body asleep on the floor no longer drops out of `PhysicsWorld::contacts` and `GameScene::touching`; sleepers keep the contacts they fell asleep with
- The over-the-shoulder camera stops at walls beside the shoulder instead of looking through them
- Flat shading no longer breaks on alpha-tested materials or on degenerate triangles
- Network handshakes have a deadline and a size cap, so a silent or oversized hello cannot hold up a host
- The relay limits connections per address (per IPv6 /64), connection rate, total connections and read rate, keeps running after accept errors, and handles each handshake on its own thread
- The host accepts a client's UDP datagrams only from the IP address of its TCP connection
- Replication refuses messages from anyone but the host, for objects or components it does not replicate, or naming one object twice

### Performance

- Measured static props: 40,000 static entities cost 0.58 ms of CPU a tick in release (ignored test `static_props_cost_little_per_tick`), so no static flag is needed
- LOD groups no longer double CPU render cost: an opaque object in a `.rlod` group is one instance, and its batch picks the level per object (2085 moving objects with two levels: 0.298 ms to 0.174 ms of render CPU a frame; 0.127 ms without LOD)

### Known limits

- Point lights cast shadows only in frames where no directional or spot light casts shadows
- A rope has at most 256 beads

---

## [2.3.0] - 2026-10-10

Follow-up to 2.2.0 from Rusting Raft: animated copies, cheaper moving objects and passwords for networking.

### Added

- Networking passwords: a host can require a password from joining clients, and `RUSTING_RELAY_TOKEN` makes `rusting relay` refuse hosts and clients without that token, so a relay can face the internet

### Changed

- `NetSession::host`, `host_on` and `join` take a password, and `host_room`, `join_room` and `run_relay` a relay token, as a new last argument. Pass `""` for none
- The network protocol version is 2. 2.2.0 games cannot join 2.3.0 hosts or relays, or the other way round

### Fixed

- Copies of rigged or animated models (`spawn_copy`, spawn grids) now animate: animation tracks, skin joints, ragdolls and retargeting find the copy's own bones instead of standing in bind pose. Copies of unnamed templates still have unnamed children and cannot be found by path

### Performance

- The renderer patches moved instances in place instead of repacking every instance: with 10,000 objects, 0.15 ms instead of 0.98 ms a frame for instance preparation

### Known limits

- Network passwords and relay tokens travel in plain text, and the relay does not rate-limit wrong tokens

---

## [2.2.0] - 2026-10-10

Engine features asked for while building the multiplayer raft game Rusting Raft.

### Added

- Networking: `rusting_engine::net::NetSession` hosts, joins by address, or joins through a relay by room code, with reliable, ordered messages over TCP. `rusting relay` runs the relay. See `guide/networking`
- Swimming: a player controller with `swim_speed` above 0 swims in water, floats at the surface, dives while crouch is held and rises while jump is held
- Turning platforms: players riding a kinematic floor turn with it, not only move with it
- Hard shadows: `scene.set_hard_shadows(true)`, or Hard shadows in the editor's project settings, gives crisp, stair-stepped shadow edges
- Flat shading on any mesh: `flat_shading` on a material, and on `rusting.water` for low-poly waves
- Window title: `"window_title"` in `project.json`, or `scene.set_window_title`

### Changed

- New public fields: `MaterialAsset::flat_shading`, `WaterBody::flat_shading`, `RenderSettings::hard_shadows`, and `PlayerController::swim_speed`, `float_depth`, `swimming` and `floor_rotation`. Add `..Default::default()` to struct literals
- Scene format 10 stores material flat shading. Version 9 cooked scenes still load; older engine builds cannot read the new files

### Fixed

- The scenario stall watchdog no longer ends a unit-test run during long GPU tests

### Performance

- Scenes with many moving objects: when objects only move, the renderable list updates them in place instead of being rebuilt. With 10,000 objects, one moving object costs 0.36 ms instead of 6.4 ms a frame, and all moving cost 1.6 ms instead of 9.8 ms

### Known limits

- Networking is TCP only, with no unreliable channel, replication, prediction or encryption
- The renderer still repacks every instance when one object moves
- 2D platforms carry riders by translation only

---

## [2.1.0] - 2026-10-09

Engine features for the horror game FOREVER BEAR, and a large batch of tools for building games with an agent.

### Added

- Gameplay without code: state machines with timed, held-input and touch transitions, health and damage, status effects, cooldowns, loot tables, dialogue, capped spawners and spawn grids
- Gameplay recipes: `rusting recipe apply` adds checkpoints, health, double jump, dash, inventory, day timer, pause menu, wave spawner or turret, each with a passing test
- Nine new game templates: twin-stick, tower defense, roguelike, card game, rhythm, racing, top-down, puzzle, empty, plus tests in every existing template
- Saves: versioned save slots for counters and objects, a save-slot list, and crash-safe writes
- Localization: locale files, plural rules, translated HUD text and a missing-translation lint
- HUD: themes from `assets/ui/theme.json`, scaling to any window, elements pinned over objects, dialogue boxes and rebindable controls with no code, and larger text for players who need it
- Audio: bus effects (low-pass, reverb, distortion, compressor, EQ), reverb zones, Doppler, falloff curves, walls that muffle sound, streaming music, pitch, seek, reverse, voice limits, mute and solo, sliding and rolling sounds, and captions
- Look: CRT and VHS screens, film grain, fog and color grading inside boxes, shadowed spot lights, text on 3D objects, camera shake, squash and stretch, hit flash and hit stop
- Gameplay helpers: tweens, `look_at`, slow motion, waypoint paths, crouching, air jumps, camera zoom and view limits
- Placeholder content: `rusting asset generate` makes meshes, textures, sprites and sound effects from a seed
- Testing: an explorer bot, fuzzing, recording a play session as a test, bisecting two builds, coverage reports and weak-test detection
- Many new `rusting lint` checks: missing objects, actions, files and translations, unreachable goals, text contrast and size, light budget, rounds with no ending
- Agent tooling: an operation journal with `rusting log` and `rusting revert`, leases for parallel agents, scene merge for git, folder-form scenes, `--offline`, `--confine` and `--read-only`, and an agent benchmark
- Performance reporting: CPU and GPU frame times, `rusting run --bench`, and an on-screen fps report under `RUSTING_PERF` (F3 toggles it)
- Editor: the Agent area shows test results and can hold outside scene edits for review, entity by entity, with undo
- Steam achievement and stat calls that do nothing yet, so games can call them today
- Sample game `forever_bear_booth`; new guides on lighting, pitfalls and the cookbook

### Changed

- `runtime::spatialize` takes a fourth `Falloff` argument; pass `Falloff::default()` for the old curve
- `runtime::hud_text` takes a third `Translations` argument; pass `None` for the old behaviour
- `ObjectState` has a `transitions` field and no longer implements `Eq`; `CpuFrameTimings` has `update` and `post_update` fields. Add `..Default::default()` to struct literals
- `Explore::goals` is `Vec<ExploreGoal>`; `"Coin".into()` still works
- `resource_state_hash` takes `&mut World`
- `rusting test` with one scenario leaves out state hashes; pass `--full` to keep them
- Commands that write project files create a `.rusting/` folder, which git ignores on its own
- Textured cylinders, cones and capsules map the full image height; a textured capsule appears flipped compared with earlier builds

### Fixed

- HUD buttons work from the keyboard and gamepad, and the HUD keeps its dark theme on a light desktop
- Exported games find their assets and load scenes cooked by another engine build
- Replays no longer diverge at tick 9 on every windowed recording
- Screenshots blend UI the same way as the game window; goldens with translucent UI may need regenerating
- Captions stay up for their whole time and hide while their sound is paused
- Long soak tests no longer grow without limit, and a stalled test fails instead of hanging
- Parallel runs of one project no longer fail at random while cooking the scene

### Performance

- Camera screens reuse the frame's physics: six screens cost about 3 ms instead of 113 ms
- Large GPU bodies no longer stall the physics pass: 20.8 ms down to under 2 ms of GPU time in the benchmark, with identical results
- Faster scenario checks, captures and color changes on large scenes

### Known limits

- Windows was checked under Wine only; gamepads, the save folder and real Windows hardware are not checked yet
- Microphone input, video on textures and the real Steam backend are not done yet

---

## [2.0.3] - 2026-10-05

### Added

- Animation: keyframe clips that move, turn, scale, recolor or hide objects, with smooth blending between clips
- Timeline in the editor: play, scrub and edit keys, with a record mode and full undo
- Animated and skinned glTF models: bones, blend shapes and their clips now import and play
- Animation state machine and blend spaces: switch and mix walk, run and other clips from game code
- Animation layers: play a clip on top of another, like waving while walking
- Root motion: walk clips move the character instead of sliding in place
- Copy animations between differently built skeletons
- IK: heads look at targets, hands and feet reach for things, feet stay on slopes and stairs
- Ragdolls: characters go limp when hit hard and get back up, or stay physical and stagger when pushed
- Particles: a new emitter with eleven ready presets (dust, leaves, snow, rain, sparks, smoke, fire, embers, fireflies, sparkle, confetti)
- Particle editor in the Inspector with a live preview
- Color grading and vignette, plus two new art presets: dark interior and bright stylized
- Camera screens: show what another camera sees on a mesh, like CCTV monitors
- Rounded cubes and capsules, and a ready-made simple character
- Add a downloaded glTF model to a scene with one command
- Skeletons show in the editor; click a bone to select it
- New guides: Animation, Effects and Look and feel

### Changed

- Cylinders and cones look smooth

### Fixed

- Spheres were drawn inside out and looked badly lit
- Adding a model to a scene no longer fails in some cases

---

## [2.0.2] - 2026-10-03

### Added

- Much better sound: panning, 3D sounds that follow objects, fades, music ducking and separate volume groups
- Menus: buttons, settings screens and pause menus made with egui, usable with mouse, keyboard or gamepad
- Gamepad support for moving, looking and menus
- Saves: games can save and load files, pause and quit
- Rebindable keys
- Split screen and picture-in-picture, with HUD for each player's view
- Video settings: resolution scale, pixelated look, vsync, FPS cap, fullscreen and window size
- Rain and other particles that run on their own, no code needed
- Player can push crates, turn to face where it walks and use an over-the-shoulder camera
- More control over GPU physics from game code: see where bodies are, feed values to your own physics shaders and combine checks
- Rhythm helpers: turn a tempo into game ticks and know exactly when a key was pressed
- Game tests can click menu buttons, check screenshots and colors, listen to the sound mix, check save files and restart the game
- Record a play session and replay it to check nothing changed
- Same game, same result: easier checks for GPU physics and for whole test runs
- New guides: Audio, Menus and UI, Cameras and custom GPU physics shaders

### Changed

- Tests show more useful output when something fails
- Scene edits from the command line can run twice safely and update only some fields
- Projects get warned when their agent guide is out of date, and `rusting fix` updates it

### Fixed

- Piles of GPU balls now settle and sleep instead of shaking forever, and no longer squeeze through wall corners
- Spinning GPU bodies come to rest after landing
- Barrels and other round objects roll properly
- Player no longer falls through a platform it starts slightly inside
- HUD text no longer fades in or slides off the screen edge
- Clearer error messages and fuller docs pages
- The command-line tool no longer crashes when its output is cut short
- No more extra full engine rebuild between check and run

---

## [2.0.1] - 2026-10-01

### Added

- Sound! Games can now play, loop and stop sounds and change the volume
- AI agents can now work with the engine directly: run commands, attach to a running game, step it and look inside
- New Agent panel in the editor that shows what an agent changed in your scene, and you can still undo it
- Much stronger game tests: check what is on screen, catch broken values every tick, measure performance and get screenshots with labels
- New helper commands: compare two scenes, see a scene as a map, jump the game to any moment and inspect it, create a test or system in one command
- Built-in offline docs and clear explanations for every error, with hints on how to fix common code mistakes

---

## [2.0.0] - 2026-09-31

### Added

- Water for seas, lakes and rivers with waves and currents, objects float in it
- Small splashing fluids drawn as one smooth surface
- Glass that bends and blurs what is behind it
- Reflections on smooth surfaces, sky images for lighting and reflection probes
- Fog, bloom (glow) and ambient occlusion (soft shadows in corners)
- Editor got a big visual refresh: new Add Component picker with descriptions, new Project Settings page, nicer Hierarchy, Assets and Console
- Tile Painter for drawing levels on a grid, with rectangle, line and fill tools
- Blender-like camera views on Numpad, orthographic view and snap settings
- Every shortcut can have a second key
- Physics joints (hinges, sliders, springs and more), joints that can break and robot-like linked bodies
- Scenes inside scenes (like prefabs), scene variants and overrides
- Reload Code while the game is running, without restarting the scene
- Optional scripting with WebAssembly
- New project templates: first-person, third-person and physics sandbox
- Many new sample games: platformer, brick breaker, snake, mini golf, stealth, shooter and more
- A lot of new simple functions for game code (input, raycasts, counters, colors, random, level loading and more)
- Players ride moving platforms, and the third-person camera no longer goes through walls
- Materials have names and textures can repeat on long floors

### Changed

- Rendering is much faster: fewer draw calls and higher FPS on Wayland
- Transparent materials now work as clear glass
- When the game can't keep up, it slows down instead of freezing

### Fixed

- A lot of physics fixes: fast objects, players on ledges, sleeping bodies, exact replays
- Textures no longer shimmer in the distance
- Textures on cube sides were upside down. If you flipped your images to fix it, flip them back
- Hidden objects and lights no longer show or light the scene
- HUD text no longer jumps to a new line when it gets longer
- Game no longer crashes when closing on Wayland

---

## [1.4.0] - 2026-09-27

### Added

- New `rusting` command-line tool: create, build, run, test and export games without opening the editor
- Edit scenes, take screenshots and write game tests from the command line
- Same game, same result every time: replays, fixed random seeds and checks that physics runs the same
- Play the scene right inside the editor with Pause and Step, no build needed
- Profiler panel with CPU and GPU graphs, plus Render Settings and Physics panels
- Better Scene View camera (orbit, pan, zoom, focus with F) and gizmo snapping
- Image and glTF import with reimport and import settings
- First-person player controller
- Game-feel kit: smooth animations, sounds and particles on events, pickups, score counters and in-game HUD with buttons
- Simple 2D: sprites, tile maps and a 2D starter scene
- Starter game template and a full sample level
- Choose which GPU to use
- Getting-started guide, concepts guide and four tutorials

### Changed

- Inspector now shows proper fields, drop-downs and pickers for every component instead of raw JSON
- Selected objects stay outlined even behind other objects
- GPU physics no longer drops objects when there are too many
- Old renderer code removed and editor guide rewritten

### Fixed

- Physics no longer skips or mixes up steps when the game lags
- Particle bursts no longer all look the same
- Scenes with renamed or old fields no longer lose data silently

---

## [1.3.0] - 2026-09-26

### Added

- Real Console panel with filters and Clear button
- Image previews in Assets, and images can be dragged onto objects
- Editor text size setting, saved automatically
- Fully customizable keyboard shortcuts for almost every editor action
- Engine can pick CPU or GPU physics for each object by itself
- Much better physics: real rotation, rolling spheres, stable box stacks, friction and bounce
- GPU objects now collide with each other, with CPU objects and with the level
- Accurate mesh colliders for level geometry
- Basic character movement that slides along walls and detects floor
- Raycasts and shape casts for checking what is in the way
- Custom GPU physics rules with your own shaders
- Fast objects no longer fly through thin walls
- Windows export from Linux and macOS
- MSAA anti-aliasing and sharper textures at an angle
- Detailed performance stats in the editor

### Changed

- Assets panel is now a file tree like in Godot, models can be double-clicked or dragged into the scene
- glTF import brings the whole model with meshes, materials, cameras and lights
- Dragging many selected objects in Hierarchy moves them all together
- Menus and popups now show correctly over the Scene View
- Physics modes renamed to simply CPU and GPU

### Fixed

- Stripes and triangles on lit surfaces (shadow acne)
- Camera movement no longer clicks or types into editor UI
- GPU stacks no longer slowly sink into each other
- Big editor slowdown when selecting huge meshes
- Wrong lighting on some glTF models
- GPU physics now gives the same result every run

---

## [1.2.0] - 2026-09-25

### Added

- Added a much more complete PBR renderer.
- Materials now support base color, normal maps, metallic/roughness, ambient occlusion and emissive textures.
- Added transparent and cutout materials.
- Added HDR rendering and tone mapping.
- Added environment lighting and support for multiple point lights.
- Added directional light shadows with quality settings.
- Added automatic visibility culling to improve performance in large scenes.
- The engine can choose between CPU and GPU culling depending on scene size and hardware.
- Added occlusion culling so objects hidden behind other objects can be skipped.
- Added LOD support for using simpler models at long distances.
- Added a clearer rendering pipeline with separate rendering stages.
- Added scene Quality and Culling settings to the editor.
- Added a full Material section to the Inspector.
- Materials can now be edited directly from the editor.
- Added Blender-style camera and light icons inside the Scene View.
- Cameras and lights can now be selected directly from their viewport icons.
- Added editable render bounds for controlling object culling.
- Added culling statistics to the editor.
- The editor now remembers the camera position and selected objects for every scene.
- Asset reload success and errors are now shown in the Console.

### Changed

- Material shader settings were simplified into PBR and Unlit material types.
- Scenes now save their rendering quality and culling settings.
- Older scenes still load correctly.
- The editor rendering system was reworked to give the engine more control over the UI.
- File dialogs now run separately, so the editor no longer looks frozen while they are open.

### Known issues

- Image files manually added as normal, metallic or occlusion textures may use the wrong color space. Textures imported through glTF work correctly.

---

## [1.1.3] - 2026-09-24

### Added

- Added asynchronous asset loading.
- Assets can now load without freezing the main engine thread.
- Added automatic asset hot reload when project files change.
- Broken asset reloads keep the previous working version instead of breaking the scene.
- glTF import now supports full model hierarchies.
- glTF cameras and lights are now imported.
- Added better glTF material and texture support.
- Added tangent generation for models that need it.
- Added transparent glTF materials.
- Scene files now save hierarchy, transforms, renderers, lights, physics and editor data.
- Renderer resource management was heavily improved for smoother frame rendering.
- Added automatic GPU buffer growth for larger scenes.
- Added GPU capability detection and rendering quality profiles.
- Major editor redesign inspired by Blender and Godot.
- Added a new dark editor theme.
- Added a better Inspector with easier editing of positions, rotations, colors and custom components.
- Added Hierarchy search.
- Added object type icons, visibility controls, inline rename, multi-selection and context menus.
- Added adjustable editor UI scale.
- Added Blender-style Scene View orbit, pan, zoom and focus controls.
- Added new editor design and roadmap documentation.

### Changed

- Core engine systems such as time, input, hierarchy and events were moved into the shared core module.
- glTF support can now be disabled when it is not needed.
- Engine camera and scene handling was simplified internally.
- Engine architecture and roadmap were rewritten around the new editor and physics direction.

---

## [1.1.2] - 2026-09-15

### Added

- Added Vulkan GPU selection with `RUSTING_VULKAN_DEVICE`.
- Engine startup now shows which GPU is being used.
- Added clearer errors when a requested GPU cannot be found.
- Added headless Vulkan support for rendering and compute without opening a window.
- Added offscreen rendering support.
- Added GPU image and buffer readback tools.
- Added headless game execution for automated tests and servers.
- Added optional GPU tests.
- Added reusable GPU testing tools.
- Added automatic rendered-image comparison tests.
- Added better Vulkan debugging information.
- Started moving shared engine types into the new `rusting-core` crate.

### Changed

- Loading broken or missing glTF models now returns a proper error instead of crashing.
- Loading broken or missing textures now returns a proper error instead of crashing.
- GPU layout tests now detect shader changes automatically instead of relying on manually written values.

---

## [1.1.1] - 2026-09-12

### Added

- Added gameplay click events for selecting objects with the mouse.
- Added collision events.
- Added runtime keyboard, mouse and cursor input.
- Added named input actions, making gameplay controls easier to manage.
- Input system is prepared for future gamepad support.
- Added shared object picking for gameplay.
- Added scene unloading without needing to immediately load another scene.
- Improved glTF material import.
- glTF models now correctly import base color, normal, metallic/roughness, occlusion and emissive textures.
- glTF textures now use the correct color space.

---

## [1.1.0] - 2026-08-31

### Added

- Added new shapes and light to "Add Object"
- Now creating multiple light sources is possible

### Reworked

- Massive GUI update
- Custom icons
- Smaller everything to get more space
- Floating buttons on scene view
- Moved scene view setting to dropdown
- Recreated "Add Object", now its a modal window with categories
- Now every Object in Hierarchy is draggable to drag to other object and make it child of it
- And few other little changes

### Removed

- Frame count from Header

---

## [1.0.2] - 2026-08-30

### Added

- Interactive axis arrows for every transform, so you can move/scale/rotate object just by holding and moving mouse like in blender and other game engines
- Also shortcuts for transform, "G" for Move, "S" for scale and "R" for rotate.
- Axes choice, e.g. When you use Move, Scale or Rotate you can press "X" to use Transformation only for "X" axis, also you can press "Y" to transform only in X and Y axes in same time, no translate will allied to "Z".
- Added transform cancelation of current transform on "Secondary button"(Mostly right click) and "Escape"
- Some GUI buttons for Transform modes

### Changed

- Now fly mode active just by holding "Secondary button"(Mostly right click)

---

## [1.0.1] - 2026-08-27

### Added

- Scene view overlay
- Bound box for selected object in Scene view
- Axis arrows for selected object
- Shortcut system
- Move camera to selected object on "F" in Scene view
- FPS like camera fly on "Num 0" in Scene view

---

## [1.0.0] - 2026-08-26

### Added

- Added Project Manager with project creation, folder picker and recent projects
- Added scene New, Open, Save As, object creation, duplicate, rename, delete and parenting
- Added scene dirty state, safe confirmation and Undo/Redo
- Added Assets browser with file import, texture loading and glTF mesh assignment
- Added real Cargo Check, release Build and compiler output in Code Editor
- Added portable game export with executable, cooked scene, assets and license
- Added Linux and Windows CI and automatic GitHub release archives
- Added public contribution guidelines and a reproducible 10,000-body benchmark summary
- Added reusable responsive GUI elements with CSS-like style values
- Added modern editor theme with reusable hover, active, border, and shadow styles
- Added Debug and Release choices beside the editor Play button
- Added a reusable CSS-style ComboBox with a toolbar-aligned preset
- Added compact File, Edit, and View menus while keeping Play directly visible
- Added generation-checked physics IDs shared by ECS and future GPU readback
- Added typed Rust GPU-condition builders with comparisons, ranges, boolean logic, timers, collisions, sleeping state and custom values
- Added serializable GPU physics watch rules, event modes, payload selection and cooldown settings
- Added a fixed 48-byte GPU physics event ABI with safe routing back to live ECS entities
- Connected GPU Dynamic bodies to native-game compute gravity and GPU-owned render transforms
- Connected compiled GPU conditions to asynchronous fence-polled Rust gameplay events
- Added `GameScene::watch_gpu_object` and `GameScene::gpu_events` for concise game code
- Added multi-class object tagging and `GameScene::watch_gpu_class` so shared GPU rules affect only explicitly selected object classes
- Added unique scene-name validation for reliable single-object lookup
- Added `GameScene::once`, reusable cube spawning, and class-based GPU physics assignment for procedural 10,000-body scenes
- Added a directly runnable `hybrid_10k` native game example
- Added visible vertical and horizontal scroll areas to Code, Inspector, Hierarchy, Project, Console, and Assets panels
- Moved Cargo compiler and native game output from Code Editor into the Console panel so source editing keeps its full height
- Added migration support for version 1 cooked scenes created before GPU watch rules
- Added cached procedural `SphereSpawn` and `GameScene::spawn_sphere` for native Rust games
- Reduced high-instance runtime overhead by using ECS change detection for physics IDs, revision-based render extraction, cached swapchain frame resources, and fixed-tick-only GPU event readback

### Fixed

- Shaderc source builds now configure correctly with CMake 4 on GitHub's Windows runners
- Inspector values no longer leak from the previous object into a newly selected object
- Native ECS rendering now batches equal mesh/material objects into indexed instanced draws instead of recording one Vulkan draw per object
- The 10,000-body example aggregates event logs instead of printing thousands of terminal lines per frame
- Editor areas showing the same panel now use separate Egui IDs
- Custom ComboBoxes now keep separate popup IDs in repeated dock areas
- New Project now requires an explicitly selected parent folder
- Project switching now clears stale code and uses project-relative source and scene paths
- Project and scene save actions now have separate, unambiguous controls
- Toolbar dropdowns now open wider styled panels with aligned full-width actions
- Replaced editor icon-font symbols with portable text to prevent missing-glyph squares
- Grouped Hierarchy object creation into one styled Add Object popup
- Hierarchy now renders a real parent-first indented tree instead of grouping rows only by depth
- Hierarchy Rename, Duplicate, and Delete actions now live in each object's right-click menu
- Rename now edits the selected tree row inline with Apply, Cancel, Enter, and Escape controls
- Split the large editor module into view, dock, project, test, and GUI files
- Dock selection now follows clicks anywhere inside an area
- Dock content now keeps safe spacing from borders and neighboring areas
- Cargo Output now streams native game stdout, stderr, panics, and exit status
- Editor Play now cooks, compiles, and runs the real Rust game instead of only changing preview mode
- Scene loading now validates object IDs and hierarchy before replacing current scene
- Saved and cooked asset paths are now portable between project and export folders
- Old unversioned projects and scenes are now migrated safely

---

## [0.1.47] - 2026-08-25

### Added

- More settings in gui like: Game/Scene/Code modes
- Adding native Rust game projects that can be edited and built from GUI
- Adding simple Rust scene API to move and edit objects without ECS boilerplate
- Adding Blender-style editor areas that can be split, resized and changed to another panel type

### Fixed

- Gui design a bit improved

---

## [0.1.46] - 2026-08-24

### Added

- More settings and features in Editor GUI like FPS limit controls
- Little custom compiler from GUI scene to optimized no GUI game/simulation

### Fixed

- Rewriting architecture a bit to prepare for scaling

---

## [0.1.45] - 2026-08-24

### Added

- Added first version of ECS runtime with schedules, hierarchy and stable entity identities
- Added typed asset handles, render extraction and revision-aware GPU mesh cache
- Added first version of egui editor with hierarchy, inspector, camera settings, play controls and live 3D Vulkan viewport
- Added roadmap and architecture documentation for future engine implementation
- Added more tests for runtime, assets, GPU layouts and perspective projection

---

## [0.1.44] - 2026-08-24

### Added

- A lot of tests

### Fixed

- A lot of different fixes to improve stability before big implementation

---

## [0.1.43] - 2026-04-11

### Added

- Textures can be applied on engine build in shapes

### Fixed

- Now textures can be reused to save VRAM

---

## [0.1.42] - 2026-04-11

### Added

- A lot of docs for better user experience( description of functions/values on hover )

### Fixed

- Culling is now working much better, without bugs.
- Fixing multi gltf model import, now each texture and model render good even if there are 10k gltf models. But each texture is separate, so you cant reuse texture without vram loss, I will fix it so fast as possible.

---

## [0.1.41] - 2026-04-6

### Added

- Added gltf models import, already with Materials, Textures and everything that needed

---

## [0.1.4] - 2026-04-5

### Added

- Better code, now no warnings( before was like 60 )
- Added new very heavy fragment shader with noise and other things
- Culling toggle on 'C'

### Fixed

- Culling is working well and give insane performance boost on big scenes where fragment/vertex shader is heavy. But it uses object center, so some object might disappear earlier as needed. I will fix it in next patch

---

## [0.1.32] - 2026-04-5

### Added

- Added culling( when mesh is not in view => dont render ), but its beta, so its working bad

---

## [0.1.31] - 2026-04-4

### Added

- Just made code cleaner and only fixed some problems in shaders

### Fixed

- Grid collision shader

---

## [0.1.3] - 2026-04-3

### Added

- Optimizing physic shaders and fragment shaders render. Collision check was **O(n²)**, now it splitted on grid, so its **O(n*k) + O(n*j)** where k is objects count in cell and j is big objects count.
- adding more physic settings like **_friction_**, **_gravity direction_** and **_bounciness_**.

### Fixed

- Object collapse on stacking.

---

## [0.1.1] - 2026-04-1

### Added

- Collision types as enum (Sphere, Box).
- Optional apply one physic/visual shader for all object on scene

### Fixed

- Better performance( main loop refactoring )

---

## [0.1.0] - 2026-03-31

### Added

- Initial release!
- Different fragment shader support.
- Different physic shader support.
- Physics engine with per-object collision types (Box/Sphere).
- Same speed as pure Vulkano+winit
