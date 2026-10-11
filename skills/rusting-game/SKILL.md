---
name: rusting-game
description: Build, test and polish a game on RustingEngine from the command line. Use when asked to create or change a Rusting game project, its scenes (.rscene), its game code (rusting_game!, GameScene) or its scenario tests.
---

# Making games with RustingEngine

A Rusting game is a folder with `project.json`, JSON scenes, a small Rust
`update` function and JSON scenario tests. You can build a whole game
without the editor: every step below is a `rusting` command, and every
command takes `--json`.

Thirteen sample games were built this way. Read the closest one before
starting with `rusting docs show sample/<name>` (its README and
`src/main.rs`): `hammer_run` (obstacle course), `sky_hop` (platformer),
`ember_arena` and `night_vault` (third person), `target_range` and
`tower_topple` (first person, shooting), `putt_course` (physics ball, mouse
aim), `core_defense` (top-down, mouse), `lantern_grid` (click puzzle),
`crate_keeper` (grid puzzle), `snake_trail` and `brick_bounce` (2D),
`forever_bear_booth` (horror: camera screens, 3D sound, waypoints).

## Loop

0. Put the CLI on `PATH` once: `cargo install --path <engine checkout>
   --bin rusting`, or call `<engine checkout>/target/debug/rusting`.
1. `rusting new <parent> <name> --template <t>`. Templates: `first-person`,
   `third-person`, `2d` (platformer), `sandbox` (physics pile), `3d`,
   `starter`. The template gives a floor, a player with a camera, lights and
   an `AGENTS.md` with the project's rules. Read that `AGENTS.md`. Its
   `Cargo.toml` builds the engine optimized even in debug builds; keep
   that profile, or a busy physics scene drops to a few frames a second.
2. `rusting schema --json` lists every scene section and component with
   field names, defaults and units. Check field names there before writing
   scene JSON (a directional light's brightness is `illuminance`, not
   `intensity`). `rusting schema --json-schema` prints JSON Schema for
   scene, patch, scenario, `project.json` and `.rmeta` files. `rusting validate` reports a key the scene format does not
   have as `SCENE_UNKNOWN_FIELD`, and `rusting fix` renames a close
   misspelling in place.
3. Change the scene with a patch file, never by hand:
   `rusting scene patch scenes/main.rscene patch.json [--dry-run]`.
   For large layouts, write a short script that generates `patch.json`.
4. Write game code in `src/main.rs`. `rusting check` builds it and
   validates the scene.
5. Write scenarios in `tests/` and run `rusting test` until they pass.
6. Look at your frames. Add `capture` steps to scenarios and open the PNGs.
   A test that passes on a black screen, or with the camera inside a wall,
   is not a finished game.
7. Polish. Run the loop in `rusting docs show guide/look-and-feel`:
   capture, look, critique against its checklist (silhouette, palette,
   warm key and cool fill, materials, shapes, HUD margins, feedback), fix
   the worst item, repeat. Start from `rusting preset apply`.
8. `rusting run` opens the window for a human to play.

Every error carries a code such as `SCENE_CONFLICT`. `rusting explain CODE`
prints what it means, how to fix it and an example; `rusting explain` lists
every code. `rusting docs search <words>` and `rusting docs show <id> [--budget N]` read
the manual, tutorials and every command offline, matched to this binary;
`rusting docs --brief` is a one-screen overview. A diagnostic with a `fix` is certain; `rusting fix --dry-run` lists
them and `rusting fix` applies them.

## Scenes

- +Y is up, -Z is forward. Metres, radians, seconds. At yaw 0 an object
  faces -Z, so facing is `(-sin yaw, -cos yaw)` and the yaw toward
  `(dx, dz)` is `atan2(-dx, -dz)`.
- Objects are found by unique `name`. Give every object game code touches a
  clear name ("Guard A", "Key 1") and group similar ones with `classes`.
- `transform.scale` scales the mesh and its collider together: a unit box
  collider on an object scaled `[6, 2, 0.6]` is a 6 x 2 x 0.6 wall.
- Physics needs `physics_body` (`{"simulation": "Cpu"}`), `rigid_body`
  (`Fixed`, `Dynamic` or `Kinematic`) and `collider`. A `"sensor": true`
  collider only reports touches (pickups, doors, goals, hazards).
- Patch operations: `create` (an `entity` object; `"parent": "<name>"`
  inside `entity` attaches it to an object that exists or was created
  earlier in the same patch), `upsert` (like `create`, but replaces the entity with the same
  `id` or `name` and keeps its id, so a patch can run twice; `"merge": true` keeps the fields you leave out), `set` (`id`, JSON-pointer `path`, `value`), `delete`
  (`"missing_ok": true` skips an entity that is not there),
  `remove`, `reparent`, `duplicate`, `set_scene`.

Create example (a sensor pickup that spins):

```json
{"operations": [
  {"op": "create", "entity": {
    "name": "Key 1", "classes": ["key"],
    "transform": {"position": [-10, 0.8, 8], "rotation": [0, 0, 0], "scale": [0.35, 0.35, 0.35]},
    "mesh_renderer": {"mesh": {"BuiltinPrimitive": "Cube"},
      "material": {"Inline": {"base_color": [1, 0.8, 0.3, 1], "emissive": [3, 2.2, 0.6]}}},
    "physics_body": {"simulation": "Cpu"}, "rigid_body": {"kind": "Fixed"},
    "collider": {"shape": {"Box": {"half_extents": [0.5, 0.5, 0.5]}}, "sensor": true},
    "components": {
      "rusting.pickup": {"counter": "keys"},
      "rusting.tween": {"property": "Rotation", "from": [0, 0, 0], "to": [0, 6.2831853, 0],
        "duration": 2.0, "easing": "Linear", "repeat": "Loop"}}}}
]}
```

### Gameplay without code

Prefer these components over game code when they fit:

- `rusting.counter` `{"name", "value", "target"}`: score, lives, keys,
  win and lose flags. A flag is a counter with target 1.
- `rusting.pickup` `{"counter"}`: the player touching it adds 1 to that
  counter.
- `rusting.hud` `{"anchor", "offset", "font_size", "text", "requires"}`:
  text with `{counter}` placeholders. With `requires` it shows only once
  that counter reaches its target, which makes win and lose screens free.
- `rusting.input_action` `{"action", "inputs"}`: named actions such as
  `{"action": "restart", "inputs": ["KeyR"]}`. Key names are winit
  `KeyCode` names; mouse is `MouseLeft`.
- `rusting.fluid_block` makes a particle fluid: `count_x/y/z` particles of
  `spacing` meters resting in a box of `container_half_extents` centered on
  the entity. Dynamic bodies with sphere colliders float and sink in it.
  It is CPU only and one entity per particle, so keep it to a few thousand
  particles.
- `rusting.soft_block` makes a jelly: `count_x/y/z` cubes of `spacing`
  meters centered on the entity, kept in shape by XPBD. `softness` 0 is
  rigid and 1e-3 wobbly; `tear_strain` above 0 lets it tear. It lands on
  sphere, box and capsule colliders and pushes dynamic bodies back.
- `rusting.cloth` makes a sheet of `count_x` by `count_y` squares in the
  entity's XY plane. `pin_top` hangs its top edge on `holder` (a body) or
  in place when `holder` is null; `wind` blows it and `tear_strain` above
  0 lets it tear. It drapes over the same colliders as soft blocks.
- `rusting.player_controller` walks up ground no steeper than
  `max_slope` (radians, default 45°) and steps onto ledges up to
  `max_step_height` (default 0.3 m); anything taller is a wall. Set
  `push_bodies: false` so the player never moves dynamic bodies, for
  example to keep a physics pile identical every run. `turn_speed`
  (radians per second, default 0 = off) turns the body's non-camera
  children, the visible rig, toward the walking direction.
  `mouse_look: false` stops a left click from capturing the mouse (for a
  second player or a UI). The controller owns the transform of its direct
  camera children; `camera_offset` moves the camera in the body's frame
  (`[0.6, 0, 0]` over the right shoulder). `air_jumps` (default 0) on it
  and on `rusting.platformer_controller` allows that many extra jumps in
  the air; 1 is a double jump.
  `rusting docs show api/PlayerController` lists every field.
- Split screen: give two active cameras a `viewport` (`[x, y, width,
  height]` fractions); `scene.set_camera(name, active, viewport)` changes
  one from code. A `rusting.hud` with `"camera": "<name>"` anchors to that
  camera's viewport. `rusting docs show guide/cameras`.
- `rusting.particle_emitter` is the full particle effect: shape, random
  `[min, max]` ranges, gravity/drag/wind/turbulence, size and color over
  life, additive glow, velocity streaks, one instanced draw per emitter.
  `scene.particles(name, ParticleCommand::Stop)` controls it from code.
  `rusting effect apply <scene> fire --at X,Y,Z` (or `--on OBJECT`) adds a
  preset: dust_motes, falling_leaves, snow, rain, sparks, smoke, fire,
  embers, fireflies, magic_sparkle, confetti.
  `rusting docs show guide/effects`.
- `rusting.burst_emitter` with `rate` (particles per second) emits every
  fixed step with no trigger; `area` (half extents of a world-aligned box)
  spreads the start points and `stretch` makes each particle that many
  times taller. Rain: `rate: 300, area: [10, 0, 10], speed: 0,
  gravity: 20, stretch: 8, on_collision: false`, on an entity above the
  player.
- Round things: give a dynamic body `"collider": {"shape": "ConvexMesh"}`
  and a `Cylinder` (or any) mesh; it collides as the mesh's convex hull, so
  a barrel on its side rolls. There is no cylinder shape of its own.
- Keyframe animation: `rusting.animation` holds named clips; each track
  keys `Position`, `Rotation` (radians), `Scale`, `Color`, `Emissive`,
  `Visible` or a numeric `Field` of another component, on the object or a
  child path such as `Arm/Hand`, with `Step`, `Linear` or `Smooth`
  interpolation and `Once`, `Loop` or `PingPong` repeat. Clip `events`
  markers reach game code through `scene.animation_events()`. Game code
  calls `play_animation`, `crossfade`, `stop_animation`, `is_playing` and
  `set_animation_speed`. `transitions` (from, to, parameter test, fade)
  make a state machine driven by `set_animation_parameter`. Clips with
  `blend` points are 1D or 2D blend spaces; `layers` play clips on top
  (override or additive, masked to child paths); `root_motion` turns a
  root bone's stride into object motion (`take_root_motion` for game code).
  `rusting scene retarget <scene> <from> <clip> <to>` copies a clip onto
  another skeleton (bones matched by name or the `humanoid` map).
  `rusting.ik` (`LookAt`, `TwoBone`, `Foot` or `Chain`) makes a
  joint look at or reach a target object. `rusting.ragdoll` makes a
  character go limp on a hit or `set_ragdoll(name, true)` and blend back
  to its animation afterwards; with `muscle` (Hz) above 0 it is an active
  ragdoll that follows its clips physically and staggers when pushed
  (good for animatronics, zombies and hit reactions). See
  `rusting docs show guide/animation`.
  `scene add-model` keeps a glTF's node animations as clips on the new
  object (the first autoplays).
- Skinned glTF models keep their skin: `scene add-model` adds
  `rusting.skin` (joint paths and inverse bind matrices; weights live in a
  `.rskin` file next to the mesh), and the model's clips move the joints.
  Blend shapes become `rusting.morph` weights, which clips key with a
  `Field` track on `/weights`. Both run on the CPU, so keep to a few
  characters. Without a rigged
  model, build a character from child entities (torso, limbs) and key
  their rotations in a clip, or use `rusting.tween` for simple loops.
- Textures: `mesh_renderer.material.Inline.base_color_texture` (and the
  other slots) is a plain path string relative to the scene file, such as
  `"../assets/textures/crate.png"`. `uv_scale: [8, 4]` repeats the texture
  across each face instead of stretching it; `uv_offset` shifts it. The
  first row of an image is the top of each cube side face.
- Look: `rusting preset apply scenes/main.rscene <preset>` sets sky, sun,
  ambient, fog, bloom, tone mapping and color grading at once (`daylight`,
  `golden_hour`, `night`, `flat_toy`, `dark_interior`,
  `bright_stylized`); `rusting preset list --json` gives each preset's
  five-color palette, so take object colors from it. Tune with
  `rusting.background`, `rusting.sky_light`, `rusting.fog`,
  `rusting.bloom`, `rusting.tone_mapping`, `rusting.color_grading`
  (`contrast`, `saturation`, `shadows`/`highlights` tints, `vignette`),
  `rusting.environment_map`, `directional_light`, `point_light`,
  `spot_light`. Put `rusting.post_volume` (`extents`, `blend`,
  `priority`) on an object with its own fog or grading to use them only
  in that area, such as a foggy hall next to a warm office.
- Shapes: `BuiltinPrimitive` takes `Cube`, `Sphere`, `Cylinder`, `Cone`,
  `Capsule` (diameter 1, height 2), `RoundedCube` (unit box, edges rounded
  by 0.1), `Torus`, `Plane`, `Quad` and a few solids. Use `RoundedCube` and
  `Capsule` for props and characters the player looks at; `guide/look-and-feel`
  has a ready character patch.
- Screens: `rusting.camera_screen` (`camera` name, `size` [w, h]) shows
  another camera's view on a mesh, for CCTV monitors and rear-view screens.
  Give each screen its own material with some `emissive`. See
  `guide/cameras`.
- Models: free CC0 glTF models (Kenney, Quaternius, Poly Pizza) look far
  better than primitives. `rusting asset import . model.glb --to models
  --license CC0-1.0 --author NAME`, then `rusting scene add-model
  scenes/main.rscene assets/models/model.glb --name "Tree 1"` places it as
  one object with its materials. See `guide/look-and-feel`.

Every component, generated from the schema; `rusting schema --json` gives
each one's fields, defaults and an example:

<!-- components:start -->
- `rusting.ambient_light`: Flat light added everywhere.
- `rusting.sky_light`: Hemisphere light: up-facing surfaces see sky_color, down-facing ones ground_color.
- `rusting.tone_mapping`: Exposure and the curve that maps HDR color to the display.
- `rusting.color_grading`: Runs after tone mapping: contrast around mid grey, saturation (0 grey, 1 unchanged), shadows and highlights color tints, vignette (0..1) darkening the corners, and film/CRT effects (0..1, off at 0): grain, chromatic_aberration, scanlines, color_bleed, noise_band, distortion.
- `rusting.camera_screen`: On an object with a mesh: shows what the camera entity named camera sees, at size [w, h] pixels, in place of the material's base color and emissive maps.
- `rusting.environment_map`: Equirectangular (2:1) sky image under assets/ that surfaces reflect and are lit by, replacing the sky_light hemisphere.
- `rusting.color_lut`: Color lookup table under assets/, applied after tone mapping and rusting.color_grading: an image strip of N squares of N x N pixels side by side (256 x 16 or 1024 x 32).
- `rusting.post_shader`: GLSL run on every pixel of the finished image, after tone mapping, rusting.color_grading and rusting.color_lut.
- `rusting.sky_shader`: GLSL that colors the background wherever no surface was drawn.
- `rusting.fog_shader`: GLSL that changes the fog over every surface and the background.
- `rusting.light_cookie`: On an object with a spot_light: projects an image under assets/ through the light's outer cone, like a slide in a projector (window frames, leaves, a logo).
- `rusting.reflection_probe`: Box of half size extents around the object's position.
- `rusting.light_probe_grid`: Grid of resolution light probes spread through a world-aligned box of half size extents around the object.
- `rusting.lightmap`: On an object with a mesh_renderer: bakes the light bouncing onto it into six square faces of resolution texels wrapped around its mesh's box.
- `rusting.foliage`: On an object with a mesh_renderer: draws count copies of its mesh in place of the object, scattered over a disc of radius metres on its local XZ plane, each turned and scaled between scale_min and scale_max at random by seed.
- `rusting.terrain`: On an object with a rusting.heightfield: draws the heights as terrain in square chunks of chunk_cells cells.
- `rusting.fog`: Exponential height fog: the scene fades into color with distance, thinning above height by height_falloff per metre.
- `rusting.procedural_sky`: Daylight sky computed from the sun (the shadow-casting directional light, else the first one): blue overhead, pale at the horizon, orange at sunset, dark at night, with a sun disc of sun_size degrees.
- `rusting.fog_volume`: A box of uniform fog of half size extents around the object, turned and scaled with it: mist in a valley, smoke in a room.
- `rusting.decal`: Paints an image under assets/ onto the surfaces inside a box of half size extents around the object, projected along its forward axis (-Z) with the image's top along +Y: a poster on a wall, a crack, a stain.
- `rusting.post_volume`: Makes the rusting.fog and rusting.color_grading on the same object apply only near it: fully while the active camera is inside the box of half size extents (world axes, metres), fading out over blend metres outside.
- `rusting.bloom`: Glow around pixels brighter than threshold (linear, before exposure), spread over the screen before tone mapping.
- `rusting.auto_exposure`: Eye adaptation: every frame the renderer measures the average brightness of the lit image and moves exposure toward key / average, clamped to min_exposure..max_exposure, at speed per second (the first frame adapts at once).
- `rusting.depth_of_field`: Lens blur: objects nearer or farther than focus_distance metres blur, up to blur (a fraction of the screen height) far behind the focus or at half its distance.
- `rusting.motion_blur`: Camera motion blur: each pixel smears along its screen motion since the last frame, scaled by intensity (0.5 is a 180-degree shutter).
- `rusting.ambient_occlusion`: Screen-space ambient occlusion: darkens ambient, sky and environment light in creases within radius metres.
- `rusting.background`: Clear color behind the scene.
- `rusting.render_bounds`: Render-only visibility bounds in local space, separate from the collider.
- `rusting.physics_sync`: What a GPU body sends back to the CPU.
- `rusting.auto_simulation`: Lets the engine choose CPU or GPU simulation for the body.
- `rusting.player_controller`: First- or third-person walking body.
- `rusting.tween`: Animates one Transform property from `from` to `to` on the fixed step.
- `rusting.camera_shake`: Trauma shake on a camera.
- `rusting.spawn_grid`: Bulk spawn.
- `rusting.squash`: Squash and stretch spring.
- `rusting.material_override`: Per-object material values on an object with a mesh_renderer: base_color (linear RGBA, multiplied into the base color texture), emissive (linear RGB glow), metallic, roughness, uv_scale and uv_offset each replace its material's value; null keeps the material's.
- `rusting.flash`: Hit flash.
- `rusting.sound_cue`: Sends a SoundEvent when the body starts touching something or game code calls trigger().
- `rusting.slide_sound`: Loops clip on this body while it slides or rolls against another collider, louder the faster it moves across the contact (full volume at full_volume_speed m/s, silent under min_speed).
- `rusting.reverb_zone`: Room reverb on a bus while the listener (the active camera, or set_listener) is inside this entity's collider; give it a sensor collider.
- `rusting.burst_emitter`: Spawns particles that fly out, fall, and shrink, when the body starts touching something or game code calls trigger().
- `rusting.particle_emitter`: Particle effects: rate and bursts per cycle, an emission shape (Point, Box, Sphere, Cone, Circle), random [min, max] ranges for lifetime, speed, size, rotation and spin, gravity, drag, wind and turbulence, size and color keys over life, fades, World or Local space, Billboard or Velocity facing, Alpha or Additive blend.
- `rusting.particle_shader`: On an object with a rusting.particle_emitter: GLSL that changes how its particles look.
- `rusting.animation`: Keyframe animation: named clips of tracks keyed over time.
- `rusting.skin`: Skinned mesh: joint paths from this object (child names joined by /, .. for the parent) and one column-major inverse bind matrix per joint.
- `rusting.ik`: Inverse kinematics on the end of a joint chain, solved each fixed step after the animation pose.
- `rusting.rope`: A rope of `segments` (1 to 256) small CPU sphere bodies (radius m, mass kg each) from this object's body at anchor (local m) to target's body at target_anchor (target null: a world point).
- `rusting.ragdoll`: Hands a character's bones from animation to CPU physics and back.
- `rusting.morph`: Blend shape (morph target) weights for the object's mesh, one per shape, usually 0 to 1.
- `rusting.fluid_block`: Particle fluid: a block of count_x by count_y by count_z particles resting on the floor of a box centered on the entity.
- `rusting.soft_block`: XPBD soft body: a block of count_x by count_y by count_z cubes of spacing metres centered on the entity, each cut into six tetrahedra that keep their edge lengths and volumes.
- `rusting.cloth`: XPBD cloth: a sheet of count_x by count_y squares of spacing metres in the entity's XY plane, centered on it, each cut into two triangles.
- `rusting.gravity_volume`: Replaces the scene gravity for dynamic CPU bodies overlapping this object's sensor collider (it needs a CPU physics body, usually Fixed, with sensor true): gravity [0, 0, 0] is a zero-g room, a sideways vector a wind tunnel, and toward_center above 0 pulls toward the object's centre at that many m/s² for a small planet (gravity is then ignored).
- `rusting.mesh_surfaces`: Per-triangle surfaces for a collider with shape "TriangleMesh" on the same object.
- `rusting.polygon`: A 2D outline for a collider with shape "Polygon" or "Chain" on the same object.
- `rusting.fracture`: How the object's mesh breaks: pieces Voronoi cells around points picked by seed, then every piece cut by each of slices (planes [x, y, z, d] in the object's frame, cutting where [x, y, z] · p = d).
- `rusting.heightfield`: A grid of ground heights for a collider with shape "Heightfield" on the same object (static only, like TriangleMesh).
- `rusting.axis_lock`: Freezes a dynamic CPU body's movement along world axes (linear) and its turning around them (angular), like Godot's axis locks.
- `rusting.force_field`: Pushes dynamic CPU bodies overlapping this object's sensor collider (it needs a CPU physics body, usually Fixed, with sensor true) with an acceleration of strength m/s², the same for any mass: Directional along direction, Radial out from the object's centre (negative strength pulls in), Vortex around the direction axis through the centre, Wind along direction in gusts of turbulence times strength, Custom with the Rust function the game registered under the name in function (App::add_force_field_function; CPU bodies only), scaled by strength.
- `rusting.physics_material`: Names the surface of this object's CPU collider and sets how its collider friction and restitution combine with the other side's in a contact: Default (friction geometric mean, restitution the larger), Average, Min, Multiply or Max.
- `rusting.vehicle`: A raycast car on this object's dynamic CPU rigid body (box collider, mass around 1200).
- `rusting.water`: Water for seas, lakes and rivers: a size by size rectangle of animated waves centered on the entity (axis aligned, rotation and scale ignored).
- `rusting.hud`: Text label or button drawn over the game view.
- `rusting.input_action`: Binds a named action to keys and mouse buttons, for game code (GameScene::pressed, held) and scenario press steps.
- `rusting.counter`: Named integer.
- `rusting.health`: Hit points.
- `rusting.dialogue`: A branching conversation on a named object, such as a shopkeeper or quest giver.
- `rusting.loot_table`: A weighted loot table on a named object, such as a chest or an enemy's drops.
- `rusting.state`: The current state of an object's state machine, such as an enemy's patrol or chase.
- `rusting.pickup`: Collected once when a platformer or player body touches its collider (make it a sensor): adds value to the counter, hides the entity, and triggers its sound cue and burst emitter.
- `rusting.tile_map`: Grid of square tiles written as text rows, top row first; the Transform position is the top-left corner.
- `rusting.platformer_controller`: Side-view run and jump in the XY plane.
- `rusting.scene_instance`: Places another scene under this entity when the scene loads, as a prefab.
- `rusting.connections`: Signal connections of the object that emits them: each runs a Rust handler the game registered with App::add_signal_handler, with target as the receiver, when this object sends the handler's event or gains or loses its component.
- `rusting.joint`: Joins this CPU body to target's (null: the world) at anchor and target_anchor.
- `rusting.articulation`: Makes this body the root of a reduced-coordinate joint tree.
<!-- components:end -->

Player actions are `player.forward`, `player.back`, `player.left`,
`player.right`, `player.jump`, `player.sprint`, `player.crouch` (set
`crouch_height` to the crouched body height). The player controller is
kinematic: moving bodies do not push it, so handle hazards in code with
`touching` (it includes the floor the player stands on and the wall or body
it pushes). A player teleported a little into a floor or platform is lifted
onto its top.

## Game code

```rust
use rusting_engine::prelude::*;

fn update(scene: &mut GameScene<'_>, time: &FrameTime) {
    if scene.pressed("restart") {
        scene.restart();
        return;
    }
    if scene.counter_complete("won") || scene.counter_complete("lost") {
        return; // the round is over; the HUD shows the result
    }
    let at_goal = scene.touching("Player").iter().any(|name| name == "Goal");
    if at_goal && scene.counter_complete("keys") {
        scene.set_counter("won", 1);
    }
}

rusting_game!(update);
```

Game state that is more than a few counters can live in your own
component, with no extra crates in `Cargo.toml`:

```rust
#[derive(Component, Clone, Default, Serialize, Deserialize)]
#[serde(crate = "rusting_engine::serde")]
struct Night {
    power: f32,
}

rusting_engine::reflect! {
    struct Night {
        power: f32 { unit: "W", doc: "power left" },
    }
}

rusting_game!(update, components: [Night => "game.night"]);
```

Scenes save it as `game.night`, scenarios read and set it as
`/components/game.night/power`, and code reads it through
`scene.world().query::<&mut Night>()`. `tick: my_tick, components: [...]`
works too.

`update` runs once per rendered frame. The main `GameScene` calls:

<!-- api-table:start -->
| Need | Call |
| --- | --- |
| find, move, parent | `object(name)`, `try_object(name)`, `reparent(name, parent)`, `name_of(entity)`, `world()` |
| contacts | `touching(name)` |
| counters | `counter(name)`, `counter_value(name)`, `counter_or(name, default)`, `set_counter(name, value)`, `add_to_counter(name, amount)`, `spend(name, amount)`, `counter_complete(name)`, `counters()` |
| cooldowns | `cooldown_ready(name)`, `start_cooldown(name, seconds)`, `cooldown_left(name)` |
| state machines | `set_state(name, state)`, `state(name)`, `state_seconds(name)` |
| health | `damage(name, amount)`, `health(name)`, `same_team(a, b)`, `add_status(object, effect, seconds)`, `has_status(object, effect)`, `status_left(object, effect)`, `clear_status(object, effect)` |
| input | `pressed(action)`, `held(action)`, `press_tick(action)`, `stick(stick)`, `clicked()`, `cursor()`, `keys_pressed()`, `binding(action)`, `rebind(action, inputs)`, `window_focused()`, `viewport_size()` |
| rays | `raycast(origin, direction, max_distance)`, `raycast_skipping(origin, direction, max_distance, skip_classes)`, `raycast_only(origin, direction, max_distance, classes)`, `raycast_visible(origin, direction, max_distance)`, `aim(max_distance)`, `camera_ray()`, `pointer_ray()`, `on_screen(point)` |
| cameras | `set_active_camera(name)`, `set_camera(name, active, viewport)`, `set_camera_fov(name, vertical_fov_radians)`, `camera_fov(name)`, `basis(name)`, `set_mouse_look(name, enabled)` |
| physics | `set_body_kind(name, kind)`, `set_linear_velocity(name, velocity)`, `linear_velocity(name)`, `set_angular_velocity(name, velocity)`, `angular_velocity(name)`, `reset_body(name)` |
| player | `set_look(name, yaw, pitch)`, `set_player(name, edit)`, `player(name)`, `dash(name, velocity, seconds)` |
| create, remove | `spawn_cube(name, transform, template)`, `spawn_sphere(name, transform, template)`, `spawn_cube_with_material(name, transform, template, material)`, `spawn_sphere_with_material(name, transform, template, material)`, `spawn_copy(template, name, position)`, `spawn_copy_at_root(template, name, transform)`, `spawn_numbered(template, position, limit)`, `spawn_prefab(path, name, transform)`, `despawn(name)` |
| classes | `in_class(class)`, `has_class(entity, class)`, `add_class(name, class)`, `remove_class(name, class)` |
| sound | `play_sound(clip, volume)`, `play_sound_looped(clip, volume)`, `play_sound_with(clip, sound)`, `play_sound_on(name, clip, sound)`, `set_sound_rate(id, rate, fade)`, `set_sound_position(id, position)`, `set_sound_volume(id, volume, fade)`, `pause_sound(id)`, `resume_sound(id)`, `pause_sounds(bus)`, `resume_sounds(bus)`, `seek_sound(id, seconds)`, `stop_sound(id)`, `stop_all_sounds()`, `playing_sounds()`, `sounds_requested()`, `set_master_volume(volume)`, `set_bus_volume(bus, volume, fade)`, `mute_bus(bus, muted)`, `solo_bus(bus, solo)`, `set_bus_effect(bus, effect, fade)`, `set_bus_voice_limit(bus, limit)`, `set_listener(name)`, `set_captions(enabled, size)` |
| look | `set_hud(name, edit)`, `set_visible(name, visible)`, `color(name)`, `set_color(name, color)`, `set_emissive(name, emissive)`, `create_material(material)`, `set_material(name, material)`, `set_text(name, text, style)`, `set_text_in_font(name, text, style, font)`, `set_light(name, color, intensity, range)`, `set_background_color(color)`, `set_reflections(enabled)`, `set_exposure(exposure)`, `set_hard_shadows(hard)`, `create_texture(texture)`, `edit_texture(handle, edit)` |
| game feel | `flash(name)`, `squash(name, amount)`, `add_trauma(name, amount)`, `hit_stop(seconds)`, `particles(name, command)`, `trigger(name)`, `tween(name, property, to, seconds, easing)` |
| animation | `play_animation(name, clip)`, `crossfade(name, clip, seconds)`, `stop_animation(name)`, `is_playing(name, clip)`, `set_animation_speed(name, speed)`, `set_animation_parameter(name, parameter, value)`, `animation_events()`, `take_root_motion(name)`, `set_ragdoll(name, limp)`, `set_ragdoll_muscle(name, hz)`, `reset_ragdoll(name)`, `is_limp(name)` |
| rounds, levels | `once(key, action)`, `restart()`, `load_scene(path)`, `initial(name)`, `snapshot()`, `restore(snapshot)`, `state_hash(class)`, `seed()`, `random(stream)`, `roll_loot(table, stream)`, `roll_loot_table(name, stream)` |
| menus, saves | `ui()`, `set_text_scale(scale)`, `text_scale()`, `set_paused(paused)`, `paused()`, `set_time_scale(scale)`, `time_scale()`, `quit()`, `save_data(key, text)`, `load_data(key)`, `delete_data(key)`, `saved_keys(folder)`, `save_counters(key, version, names)`, `save_objects(key, version, objects, components)`, `load_objects(key)`, `load_counters(key)`, `load_text(path)` |
| video settings | `set_render_scale(scale)`, `render_scale()`, `set_pixelated(pixelated)`, `set_upscale_sharpness(sharpness)`, `set_vsync(enabled)`, `set_max_fps(fps)`, `set_fullscreen(fullscreen)`, `fullscreen()`, `set_window_size(size)`, `set_window_title(title)` |
| translations | `set_locale(locale)`, `locale()`, `tr(key)`, `tr_count(key, count)` |
| dialogue | `start_dialogue(name)`, `dialogue_line(name)`, `advance_dialogue(name, choice)` |
| tiles, fields | `tile(map, position)`, `set_tile(map, position, character)`, `set_field(name, path, value)` |
| GPU bodies | `apply_gpu_physics_to_class(class, settings)`, `watch_gpu_class(class, rule)`, `watch_gpu_object(name, rule)`, `set_gpu_condition_shaders(shaders)`, `gpu_events(name)`, `gpu_state(name)`, `count_gpu_bodies_in_box(class, min, max)`, `gpu_command(name, command)` |
| other | `rebind_device(action, inputs)`, `generate_ragdoll(name, total_mass)`, `set_kill_y(y)`, `set_physics_substeps(substeps)`, `set_gpu_axis_lock(locks)`, `fell_out()` |
| on `object(name)` | `entity()`, `position()`, `set_position(position)`, `move_by(offset)`, `move_x(distance)`, `move_y(distance)`, `move_z(distance)`, `rotation()`, `scale()`, `set_rotation(rotation)`, `look_at(target)`, `rotate_by(rotation)`, `rotate_x(rotation)`, `rotate_y(rotation)`, `rotate_z(rotation)`, `set_scale(scale)` |
<!-- api-table:end -->

The table is generated from the API index; `rusting docs show
api/GameScene::<name>` prints one call's docs. Notes the signatures do not
say:

- `reparent(name, Some(parent))` keeps the local transform; `None` detaches.
  `touching(name)` names everything touching it, sensors included.
- `pressed` is true on the press frame only, `held` while down.
  `stick(Stick::Left)` reads gamepad tilt; bind `PadSouth`, `PadDpadUp`,
  `PadLeftStickUp`... in `rusting.input_action`. The player controller
  already binds the left stick, the d-pad, South to jump and the right stick
  to look.
- Rays hit sensors, and a ray starting inside a collider passes it.
  `raycast_skipping(origin, dir, max, &["glass"])` skips classes,
  `raycast_only(origin, dir, max, &["island"])` hits only those classes,
  `raycast_visible` passes hidden objects and templates, `on_screen(point)`
  gives view fractions or `None`, and `pointer_ray` goes through the mouse.
- `set_active_camera(name)` makes it the only active camera;
  `set_camera(name, active, Some([x, y, w, h]))` splits the screen.
  `basis(name)` is world `[right, up, forward]`; forward is -Z, and rotation
  applies X, then Y, then Z.
- `set_body_kind` to `Kinematic` or `Fixed` stops the body. `reset_body`
  zeroes velocities, forgets contacts and sleep, and wakes it.
- `set_player(name, |pc| ..)` edits `walk_speed`, `sprint_multiplier`,
  `jump_speed` and the other controller settings. `player(name)` is a copy
  of its controller: `grounded`, `floor`, the `wall` it pushes against and
  the last step's `velocity`. Its `Transform` rotation Y equals `yaw`, so
  set `turn_speed` to face the rig along `velocity`.
- `spawn_copy` copies a hidden template and its children under its parent;
  `spawn_copy_at_root(template, name, transform)` takes no parent and a full
  transform. `add_class(name, class)` puts a spawned copy in a class.
- Sound clip paths are under `assets/`: `play_sound("sfx/hit.wav", volume)`,
  `play_sound_with(clip, Sound { pan, bus, at_tick, position, .. })`.
  `sfx:coin 7` (jump, coin, hit, explosion, laser, powerup, blip, optional
  seed) plays a built-in sound with no file. `BeatClock` and `press_tick`
  time music games; see `guide/audio`.
- `load_text("levels/1.txt")` reads a text file under `assets/`;
  `rusting validate` fails on a literal asset path in game code that is not
  a file.
- `set_hud(name, |hud| ..)` changes a `rusting.hud` text, color or size,
  shown the same tick. `set_light` with `None` keeps a value.
  `create_texture(TextureAsset)` then `edit_texture(handle, |texture| ..)`
  draws pixels while running; the texture re-uploads next frame.
  `hit_stop(seconds)` freezes the picture on a heavy hit; ticks go on.
- `set_animation_parameter` is a state machine input, and
  `animation_events()` returns clip markers; see `guide/animation`.
- `load_scene("scenes/level_2.rscene")` switches level. `initial(name)` is
  the starting transform, color and body kind. `seed()` is the run seed,
  for a layout made once; `random(stream)` draws numbers that repeat for a
  scenario's seed.
- `window_focused()` is false after alt-tab, and `clicked()` lists HUD
  buttons clicked; see `guide/menus-and-ui`.
- `set_render_scale(0.25..=2.0)`; `set_pixelated(true)` upscales with
  nearest filtering for a chunky low scale; `set_upscale_sharpness(0..=1)`
  keeps a low scale crisp instead (FSR 1 style).
- `set_hard_shadows(true)` draws crisp one-tap shadow edges for low-poly
  art; materials with `flat_shading` get faceted normals on any mesh.
- `MeshVertex::color` (linear RGBA, white by default) tints the
  material per vertex; glTF `COLOR_0` is imported.
- `watch_gpu_class` and `watch_gpu_object` are GPU condition rules; see
  `guide/gpu-condition-shaders`.

### Rules that keep tests reliable

- Time gameplay with `time.fixed_tick`, never with frame counts or wall
  time. Make moving things a pure function of the tick where you can (a
  guard's patrol position = f(tick)); scenarios then repeat exactly.
- Randomness comes only from `scene.random(stream)`: it repeats for a
  scenario's `seed` and differs per tick and stream. Never use `rand` or
  the system clock.
- Keep round state in counters, not in Rust statics: `restart` and
  `load_scene` reset the scene, and statics would survive.
- `restart` reloads the current scene and runs `once` blocks again.
  Physics after `restart`, `load_scene` or `restore` repeats bit for bit
  what it did the first time, so a loop or replay game can simply
  restart. `snapshot()` saves the scene mid-game and `restore` puts it back
  with velocities, sleep and solver state; the clock carries on, so time
  loop logic by ticks since the round began.
- Use CPU bodies (`"simulation": "Cpu"`) for anything gameplay reads or
  must repeat. GPU bodies reach game code a few frames late and are not
  covered by `restart` determinism; use them for large decorative piles.
  `restart` puts every GPU body back at its scene-file pose. Move or push
  one from code with `scene.gpu_command("Ball#7",
  GpuBodyCommand::Teleport(Transform::new([0.0, 5.0, 0.0])))` (also
  `SetVelocity`, `Impulse`, `Force`). Their
  `Transform` keeps the spawn pose: read the GPU pose with
  `gpu_state(name)` (set `GpuBodySettings { sync:
  PhysicsSyncMode::SelectedState, .. }`), and in scenarios under
  `/gpu_state/position`.
  `load_scene` switches to another scene file, which then becomes the one
  `restart` returns to. Counters live in the scene, so carry a score to the
  next level by reading it before `load_scene` and setting it after.
- After `restart` or `load_scene`, return from `update`: objects you looked
  up earlier in the frame are gone.
- Game code adds no dependencies beyond `rusting_engine`.
- `rusting docs show guide/pitfalls` lists the mistakes games have made,
  each with the diagnostic code that catches it and the fix.
  Its entries, by symptom:

<!-- pitfalls:start -->
- Game code does not compile: Two `scene` borrows at once; A guessed method name; `f64` or tuples where the scene wants `f32` arrays; Importing from engine modules.
- Objects do not collide, or are hit where they are not drawn: A collider with no body; A collider sized by hand that does not match the mesh; A scaled player; Hiding an object with a zero scale; A pickup or goal placed inside a wall or the floor.
- The view or the HUD is wrong: A camera inside a wall, pillar or other solid; A full-screen egui menu hides the game; HUD text too small to read, anchored off screen, or faint on a button; More lights than the renderer uploads; A light that can never light anything.
- Assets and scene files: An asset path in code that is not a file; A scene that names a missing asset; A misspelled scene key.
- Runs do not repeat: Timing by frames or wall-clock time; `rand` or the system clock; Round state in Rust statics; Using objects after `restart` or `load_scene`; A score lost on `load_scene`; Gameplay reading GPU bodies.
- Tests prove less than they seem: A scenario that passes without the feature it tests; A stale CLI or project guide; A recipe applied to a scene with no `Player`.
<!-- pitfalls:end -->

- `rusting docs search cookbook` lists tested snippets for common tasks
  (an enemy that follows the player, a level select).

## Multiple levels

Create a level by copying `scenes/main.rscene` to `scenes/level_2.rscene`
and patching the copy. Then, in code:

```rust
if scene.counter_complete("keys") && at_exit {
    let score = scene.counter_value("score");
    scene.load_scene("scenes/level_2.rscene").expect("level 2 loads");
    scene.set_counter("score", score);
    return;
}
```

`rusting export` ships only the main scene today, so exported builds cannot
load other levels yet; `rusting run` and `rusting test` can.

## Scenarios

A scenario is a JSON file in `tests/`. `rusting test` runs them all.
`rusting add scenario <name>` writes one that fails until you fill in a real check;
`rusting add system <name>` also appends a documented stub function to
`src/main.rs` (call it from `update`).
`rusting recipe apply <root> checkpoints` (or `health`, `double_jump`, `inventory`, `day_timer`, `pause_menu`, `wave_spawner`, `turret`, `dash`, `alarm`) writes a tested mechanic: its source
as `src/checkpoints.rs`, its objects into the main scene and a passing scenario;
add the `mod` line and call it reports to `src/main.rs` (`rusting recipe list`).

```json
{"name": "Taking every key and reaching the door wins", "ticks": 130, "steps": [
  {"tick": 1, "set": {"entity": "Player", "path": "/transform/position", "value": [-10, 1, 8]}},
  {"tick": 2, "within": 15, "expect": {"counter": "keys", "equals": 1}},
  {"tick": 20, "press": "player.forward"},
  {"tick": 22, "until": 59, "expect": {"entity": "Escaped Counter", "path": "/components/rusting.counter/value", "equals": 0}},
  {"tick": 60, "set": {"counter": "keys", "value": 3}},
  {"tick": 61, "within": 70, "expect": {"entity": "Escaped Counter", "path": "/components/rusting.counter/value", "equals": 1}},
  {"tick": 80, "capture": "shots/escaped.png"},
  {"tick": 90, "release": "player.forward"},
  {"tick": 100, "press": "restart"},
  {"tick": 101, "release": "restart"}
]}
```

- Steps: `press`, `release`, `tap` (action names; `"at": 0.5` presses
  partway through the tick, read by `press_tick`), `click` (an on-screen
  text label: egui buttons, HUD buttons; or `{"text" | "starts_with",
  "index"}`, `index` counting in reading order), `expect_file` (`{"path",
  "exists", "contains"}` in the user data folder, checked even after a
  quit), `restart`, `expect_quit`,
  `left_stick` / `right_stick` (`[x, y]` gamepad tilt), `focus` (`false`
  is alt-tab: held inputs release), `set`, `expect`,
  `expect_events`, `expect_screen` (`on_screen`, `occluded`, `inside`,
  `min_share`, optional `camera`), `expect_pixels` (`region` fractions,
  `mean_min`/`mean_max` `[r, g, b]` 0..255, `stddev_min`/`stddev_max`,
  optional `camera` viewport), `capture` (path relative to the scenario file's folder: `"shots/a.png"` from `tests/`, not `"tests/shots/a.png"`; or
  `{"path", "camera", "hud": false, "golden": "golden/a.png", "tolerance": 1.0}`,
  and `rusting test --update-golden` writes the golden images), `pointer`
  (`[x, y]` as fractions of the view, `[0.5, 0.5]` is the center), `log`
  (`{"entity", "path"}`; records the value, with `until` on every tick,
  and never fails: use it instead of `eprintln!` to debug a scenario;
  plain `rusting test` prints each logged value as `tick N: ...`). A
  failed scenario also prints the last 20 lines of the game's stderr.
- Each scenario runs with an empty user data folder; top-level `files`
  copies save fixtures into it. Menus, saves and rebinding:
  `rusting docs show guide/menus-and-ui`.
- Game code sees `time.fixed_tick == N` on scenario tick N.
- `"entity": "class:ball"` reads `/count` (members of the object class)
  and `/gpu/count`, `/gpu/min`, `/gpu/max` (bounds of their GPU poses);
  `"class:ball in -5,0,-5 5,10,5"` adds `/gpu/inside`, the GPU bodies in
  that box. The poses land 1 to 3 ticks after the read, so check with
  `within` or `until`.
- `"entity": "audio:"` reads the sounds game code and sound cues asked for:
  `/requested` (all plays), `/clips/<clip>` (plays of one clip; write `/`
  in the clip path as `~1`: `/clips/sfx~1hit.wav`; a clip never played reads 0), `/level` (`[l, r]` RMS
  of the offline mix), `/peak` (`[l, r]` max per tick), `/clipped` (samples at full scale so far) and `/playing` (clip, volume, pan, bus, tick of each
  sound still playing). Top-level `"audio_out": "mix.wav"` writes the mix.
  Headless runs have no audio device, so this is how a scenario checks
  sound.
- `rusting serve` keeps one process open: write JSON-RPC lines (`{"jsonrpc":"2.0","id":1,"method":"scene query","params":[...]}`) to stdin, read one
  envelope per line from stdout; `shutdown` ends it.
- `rusting debug` runs the game paused and answers one JSON line per command: `{"id":1,"cmd":"step","ticks":5}`, `get`/`set` (`entity`, `path`, `value`), `press`/`release` (`action`), `capture` (`path`), `tick`, `quit`.
- `rusting mcp [root]` exposes every command as an MCP tool (`scene_query`, args as `{"args":[...]}`); read-only tools carry `readOnlyHint`; paths with `..` or absolute are refused.
- `rusting test --json` also gives each scenario a `perf` report (mean, p95
  and max tick milliseconds; `wall_ms_mean` is the whole run per tick,
  checks included; `render` has draws, triangles and `gpu_ms` of
  the last rendered frame and per viewport camera under `cameras`; and the
  environment). A render budget renders even without a capture step. A scenario `budgets` object (`max_tick_ms`,
  `mean_tick_ms`, `p95_tick_ms`, `max_draws`, `max_triangles`) fails the run
  when a limit is exceeded; timings differ per machine and debug/release.
- `rusting test --json` also gives each scenario a `trace`: inputs, `set` steps and
  collisions in tick order (first 1000), to see what happened without a capture.
- A scenario stops at its first failed check; `"keep_going": true` in the
  scenario runs on and reports every failure. `rusting test --json` lists
  the `log` lines of each scenario; `rusting run --ticks N --json` reports
  `timings.headless_ms_per_tick`.
- `"realtime": true` runs a scenario at one tick per fixed step of real
  time instead of as fast as possible: use it for a client scenario
  against a server started with `rusting run --server`.
- `rusting inspect --tick N [--entity NAME]` runs the game to tick N and prints each
  entity's state after that tick, without writing a scenario.
  It runs without scenario input; to see a value during a scenario, add a
  `log` step.
- `rusting determinism . --scenario tests/run.json` replays a scenario in a
  debug and then a release build and compares the state hash of every
  tick: a scripted full run is the replay test. `rusting run --record
  run.json` saves a played session (it stops at `--timeout`); `rusting run --replay run.json` plays it
  back headless and fails at the first tick that differs.
- `"annotate": true` in a scenario makes each `capture` also write `<name>.annotated.png`
  (a box and a 4-digit short ID over every mesh entity) and `<name>.annotated.json`
  (short ID to entity name, full ID and pixel box). Boxes come from mesh bounds and ignore occlusion.
- `"contact_sheet": "shots/sheet.png"` in a scenario also writes one image with every `capture`
  frame in a grid, each labelled with its tick: one image shows motion.
- `rusting capture` renders the scene file without running game code;
  for a state the code creates, add a `capture` step to a scenario.
  `--at X,Y,Z --look-at X,Y,Z` (or `--look YAW,PITCH`) shoots from any
  point without adding a camera to the scene.
- `expect` takes `equals`, `not_equals`, `greater_than`, `less_than` or
  `exists`, plus `tolerance` for numbers and arrays. It does not take
  `value`.
- `{"counter": "keys"}` in `expect`, `set`, `log` and `invariants` stands for
  the entity and path of the counter named `keys`, so a test does not need
  the counter object's name. `set_counter` and `add_to_counter` create a
  counter that does not exist yet, so plain game state (a clock, a power
  level) needs no scene object; reading a missing counter warns once on
  stderr and returns 0.
- `within` and `until` are absolute ticks, not durations. `within: 70`
  from tick 61 means "true on some tick from 61 to 70"; `until: 59` means
  "true on every tick from 22 to 59". A `set` on a tick applies before that
  tick's checks, so end an `until` before the tick that changes the value.
- `pressed(action)` is true only on the tick an action goes down, so a
  held `press` gives one edge. Press again after a `release`, or use
  `tap`, which presses and releases before the next tick.
- Bodies are solved in entity order, so adding or removing unrelated
  entities can change where a toppling pile ends up, although each run of
  the same scene repeats exactly. Assert robust values (distance moved,
  "fell over") rather than one exact final height.
- Order inside one tick: `set`, `press`, `release`, `tap` and `pointer` first, then
  the game's update and physics, then `expect`, `log`, `capture` and the
  invariants. To check a value before a `set` changes it, check one tick
  earlier.
- A check on tick N sees the state after tick N's update. If the game acts
  on the first tick (a guard sees you at once), do not expect the starting
  value on tick 1.
- HUD objects have no `/visible` value; check the counter the HUD
  `requires` and look at a capture instead.
- Write at least: one scenario for the main success path, one for failure,
  and one that restarts. Test the rule, not the tuning: set the player next
  to the thing under test instead of walking there for 600 ticks.
- Add `tests/shots` to the project's `.gitignore`.

## Performance

- Run the release binary with `RUSTING_PERF=1` to see fps, update and
  render time, GPU pass times and draw counts once a second on stderr.
  Close other GPU programs first: a second game instance halves the numbers.
- Objects with the same mesh and textures share one draw whatever their
  colors, so give many props one texture set instead of many textured ones.
- Level of detail: a `.rlod` file beside a `.rmesh` (same name) lists
  coarser meshes and the distance or screen size where each takes over;
  every object drawing that mesh uses it (`guide/look-and-feel`). glTF
  import writes one with generated levels for static meshes of 512+
  triangles.
- Materials rougher than 0.5 skip screen-space reflections. If no surface
  needs a mirror-like look, call `scene.set_reflections(false)` at startup:
  it removes the scene copy, mip chain and depth pyramid passes.

## Before calling a game done

- `rusting check` and `rusting test` pass.
- You opened every capture and it shows what its scenario claims.
- The frames pass the `guide/look-and-feel` checklist.
- The project `README.md` says how to play, what each file does and what
  each scenario proves.
- If the engine was missing something, report the gap (what you needed,
  what you wrote instead) rather than hiding a workaround in game code.
