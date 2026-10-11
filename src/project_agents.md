# Agent guide

This is a Rusting game project. You can build the whole game from the command
line: scenes are JSON files, game code is plain Rust, and every `rusting`
command takes `--json` for output a program can read.

## Files

- `project.json`: project manifest; `main_scene` is the scene the game loads.
- `scenes/main.rscene`: the scene, as JSON. Edit it with `rusting scene patch`
  rather than by hand, so every change is validated before it is written.
- `src/main.rs`: game code. `update` runs once per frame.
- `assets/`: imported models, textures and sounds.
- `tests/`: scenario files for `rusting test` (create the folder when needed).
- `rusting add scenario <name>` and `rusting add system <name>` scaffold a failing scenario (and a stub function in `src/main.rs`) to fill in.

## Workflow

1. `rusting schema --json` lists every scene section and component with its
   default, fields, units and an example. Read it before writing scene JSON.
   `rusting schema camera_screen --json` prints one component, operation or
   section (`scenario`) instead of the whole catalog.
2. `rusting scene query scenes/main.rscene --json` lists the entities with
   their IDs. Patches address entities by ID or by unique name.
3. `rusting scene patch scenes/main.rscene patch.json [--dry-run]` applies a
   batch of operations: `create`, `set`, `remove`, `reparent`, `duplicate`,
   `delete` and `set_scene` (scene-level fields). Built-in sections and
   components may be partial; missing fields take their defaults.
   `set`, `remove`, `reparent` and `delete` name their entity with `id`
   (an ID or unique name) or `name`: `{"op": "delete", "name": "Crate 1",
   "missing_ok": true}`, `{"op": "set", "name": "Sun", "path":
   "/directional_light", "value": null}`.
   `rusting explain PATCH_JSON` prints a full patch that creates a parent
   and a child with a mesh and a component.
4. `rusting check` builds the game code and validates the scene. Project
   commands default to the current folder.
5. `rusting test` runs every scenario in `tests/`, and
   `rusting test tests/name.json` runs one. A scenario presses inputs on given
   ticks, sets values, and checks values; `"exists": false` checks that an
   object is gone, and `tolerance` applies to every number in a position or
   other array. Use `set` to place the player or
   fill a counter before a check (`/transform/rotation` on the player
   sets its controller's yaw and pitch), `within` for "eventually by tick N" and
   `until` for "holds every tick through N". Both take an absolute tick,
   not a count. A `pointer` step puts the mouse cursor at a point of the
   view, given as fractions: `{"tick": 1, "pointer": [0.5, 0.5]}` is the
   center.
6. `rusting capture scenes/main.rscene shot.png --tick 60` renders a frame so
   you can look at the result. It runs the scene alone; add `--game` to
   build the game and run its code up to that tick first, so spawned
   objects and HUD text game code sets are in the frame. Scenario files
   can capture frames too.
   A working game is not done until it looks good: run the polish loop and
   checklist in `rusting docs show guide/look-and-feel` (lighting, palette,
   HUD, shapes, free CC0 models with `rusting scene add-model`).
7. `rusting run` opens the game window. `--ticks N` runs it headless and
   saves the end state to `build/final.rscene`; inspect it with
   `rusting scene query build/final.rscene --json`. Game output (`eprintln!`)
   is in the `--json` result under `game.stderr`; `--stderr` on `run` or
   `test` also prints it while the game runs.
8. Every error has a code such as `SCENE_CONFLICT`.
   `rusting explain SCENE_CONFLICT` prints what it means, how to fix it and
   an example; `rusting explain` lists every code. A `--json` diagnostic
   also names where the problem is: `file`, `line` and `column` for text
   that did not parse, `scene_location` (a JSON pointer into `file`, such as
   `/entities/3` or `/operations/1` of a patch) and `entity` (`id`, `name`).
   `rusting docs search <words>` and `rusting docs show <id>` read the
   manual and every command offline, for this engine version.
   `skills/rusting-game/SKILL.md` (also `rusting docs show guide/agent-skill`)
   is the full guide for building a game; read it first.
   `rusting docs show sample/<name>` prints a sample game's README and code.
   A diagnostic with a `fix` (a misspelled scene key) is certain:
   `rusting fix --dry-run` lists the fixes and `rusting fix` applies them.

9. A project that points `rusting_engine` at a local `path` builds against
   whatever is in that folder now. If the engine is being edited while you
   work, a build can fail inside the engine or a GPU state hash can change
   between two runs. Run `rusting --version` and rerun before you report a
   failure as a game bug. To hold one engine version, depend on a git `rev`
   or `tag` instead of a `path`.

## Scene basics

- +Y is up and -Z is forward. Units are metres, radians and seconds.
- A mesh is scaled by `transform.scale`, and so is its collider: a unit box
  collider on an entity scaled `[6, 1, 6]` is a 6 x 1 x 6 slab.
- Physics needs `physics_body`, `rigid_body` (`Fixed`, `Dynamic` or
  `Kinematic`) and `collider`. A `sensor` collider only reports touches.
- Gameplay components need no code: `rusting.counter`, `rusting.pickup`,
  `rusting.hud` (text with `{counter}` placeholders), `rusting.tween`,
  `rusting.sound_cue`, `rusting.slide_sound`, `rusting.reverb_zone`, `rusting.burst_emitter`,
  `rusting.player_controller`, `rusting.joint`, `rusting.camera_shake`
  (shake a camera with `scene.add_trauma("Camera", 0.5)`), `rusting.squash` (wobble a visible
  child with `scene.squash("Body", 0.4)`), `rusting.flash` (tint an object
  and its children for a moment with `scene.flash("Enemy")`),
  `rusting.spawn_grid` (copy an
  object onto a grid at game start, for example `{"count":[10,1,10],
  "spacing":[0.5,0.5,0.5]}`; copies are named `Ball#1`, `Ball#2`, ...).
  Components with `requires` wait for that counter to reach its target.
- Quads are one-sided: a sprite turned more than 90 degrees about Y
  disappears. Animate 2D sprites with a `Scale` tween instead.
- Player and platformer controllers ride moving platforms (kinematic bodies
  moved by `rusting.tween` or game code).
- On a kinematic body the player controller is never pushed, but it pushes
  dynamic bodies it walks into (`push_bodies`). Handle hazards in game code
  with `touching`, as below; it includes the floor and the wall the player
  stands on or pushes. On a `Dynamic` rigid body the solver moves the player,
  so hazards push it, but its walk brakes with up to 40 m/s² times its mass:
  a motorised sweeper needs more force than that at the player to carry it.
- Input actions for the player controller: `player.forward`, `player.back`,
  `player.left`, `player.right`, `player.jump`, `player.sprint`,
  `player.crouch` (needs `crouch_height` above 0; the body shrinks to that
  height, so a 0.7 m crouch fits under a 0.75 m table).
- Add your own actions with `rusting.input_action`, for example
  `{"action": "fire", "inputs": ["MouseLeft", "KeyF"]}`. Key names are winit
  `KeyCode` names; gamepad inputs are `PadSouth` (A), `PadEast`, `PadStart`,
  `PadDpadUp`, `PadLeftStickUp` and so on. Scenarios press the action by name. `pressed` is true
  only on the tick an action goes down; a scenario `tap` step presses and
  releases, so several taps give several edges.
- Menus use egui through `scene.ui()`. A scenario `click` step clicks a
  button by its label. Saves go through `scene.save_data` and
  `scene.load_data`. `rusting docs show guide/menus-and-ui` covers menus,
  pause, quit, saves, rebinding and their tests.
- `rusting docs show guide/pitfalls` lists mistakes games have made, each
  with the diagnostic code that catches it and the fix.
- `rusting docs search cookbook` lists tested snippets for common tasks
  (an enemy that follows the player, a level select).
- `rusting docs show api/PlayerController` lists the controller's fields.
  `turn_speed` turns the body's non-camera children (the visible rig)
  toward the walking direction.
- A `rusting.particle_emitter` is the full particle effect (fire, smoke,
  snow, sparks); `rusting effect list` lists ready presets and
  `rusting docs show guide/effects` covers its fields.
- A `rusting.burst_emitter` with `rate` emits continuously over its `area`
  box; `stretch` makes tall particles such as rain streaks.
- `rusting.vehicle` on a dynamic box body (mass about 1200) makes a car:
  raycast wheels with springs, tire grip and a gearbox. `player_input`
  true drives it with the player actions; game code steers it with
  `set_field(name, "/components/rusting.vehicle/throttle", json!(1.0))`
  (also `brake`, `steer`). `rusting schema vehicle --json`
  lists the wheel fields.
- `rusting.physics_material` names a collider's surface and sets how its
  friction and restitution combine (`Min` on ice keeps it slippery); a
  `rusting.sound_cue` with `with_material` plays only on touches of that
  surface, for per-surface impact sounds.
- Uneven ground: give an object `"shape": "Heightfield"` in its collider
  and a `rusting.heightfield` with `heights[row][column]` and `spacing`.
  It is static. Draw it with a mesh from `Heightfield::mesh()`. Give
  cells their own ground (ice, mud) with `cells[row][column]` indexing
  `surfaces` (`name`, `friction`, `restitution`); a `sound_cue` with
  `with_material` matches the surface name.
- Compound bodies: parent solid collider objects to a Dynamic CPU body and
  they move with it as one body (a table on its legs). Contacts and ray
  hits on a child report the parent; the parent's mass counts for all.
- `rusting.force_field` on a Fixed sensor body pushes dynamic CPU bodies
  inside it: `Directional`, `Radial` (negative `strength` pulls in),
  `Vortex` around `direction`, or `Wind` with `turbulence` gusts. GPU
  bodies inside it are pushed too. For any other shape use `Custom` with
  `"function": "name"` and register
  `app.add_force_field_function("name", |sample| [..])`, which gets the
  body's offset from the centre, velocity and time. Use it for jump pads, fans, whirlpools
  and explosions.
- A dynamic body with a `ConvexMesh` collider collides as the convex hull
  of its mesh: a `Cylinder` mesh makes a rolling barrel.
- `rusting.animation` plays keyframe clips (position, rotation, scale,
  color, emissive, visible, numeric fields) on an object and its named
  children, with `transitions` between clips driven by
  `set_animation_parameter`, 1D/2D blend spaces, masked override or
  additive `layers`, `root_motion` (`scene retarget` copies clips between
  skeletons), `rusting.ik` makes a joint look at or
  reach a target object, and `rusting.ragdoll` hands bones to physics on
  a hit or `set_ragdoll` and blends them back (`muscle` above 0 keeps
  them physical and following the clips: an active ragdoll); `rusting docs show guide/animation` covers it. `scene
  add-model` keeps a glTF's node animations as clips, and a skinned
  glTF's skin as `rusting.skin` and blend shapes as `rusting.morph` (both
  on the CPU; keep to a few characters).
  Without a rigged model, build characters from child entities and key
  them.

## Game code

```rust
use rusting_engine::prelude::*;

fn update(scene: &mut GameScene<'_>, time: &FrameTime) {
    // Objects are found by their scene name.
    let hit = scene.touching("Player").iter().any(|name| name == "Spikes");
    let mut player = scene.object("Player");
    // FrameTime: delta_seconds() and elapsed_seconds() are methods;
    // fixed_tick and frame are fields.
    player.rotate_y(1.5 * time.delta_seconds());
    if hit || player.position()[1] < -5.0 {
        player.set_position([0.0, 1.0, 0.0]);
    }
}

rusting_game!(update);
```

Every `GameScene` call by need, generated from the API index
(`rusting docs show api/GameScene::<name>` prints one):

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

A copied child is named `"<copy>/<child>"`. `restart` reloads the starting
scene, and physics after it repeats the first run exactly. Sound clips are
WAV, Ogg, MP3 or FLAC paths under `assets/`; `rusting asset generate . sfx
"coin 7" --to sounds` makes a placeholder sound from a preset and seed,
and `rusting asset generate . texture "bricks 3" --to textures` a
tileable 256 px texture (grid, checker, bricks, planks, tiles, noise; the
seed picks the colour) for a material's `base_color_texture`;
`sprite "star 2" --to sprites` makes a 128 px outlined shape on a clear
background (circle, square, triangle, diamond, star, heart) for a `Quad`
mesh with `alpha_mode` `Blend`; `mesh "barrel 4" --to models` makes a
flat-shaded low-poly glTF prop (crate, barrel, rock, tree, gem) standing
on its origin, for `rusting scene add-model`; `font pixel --to fonts`
makes a pixel TTF (pixel, bold) for a `rusting.hud` `font`.
`load_scene` takes a path relative to the project folder.
`set_field(name, path, value)` sets any scene field by the JSON pointer
scenario `set` steps use, for example `scene.set_field("Hall",
"/components/rusting.fog/density", serde_json::json!(0.08))`; it reaches
every registered component such as `rusting.color_grading` and
`rusting.player_controller`. `world()` gives the ECS world for anything
else.
Keep game state in your own component, not only counters:
`#[derive(Component, Clone, Default, Serialize, Deserialize)]` with
`#[serde(crate = "rusting_engine::serde")]`, a `rusting_engine::reflect!`
block, and `rusting_game!(update, components: [Night => "game.night"])`.
Scenarios then read `/components/game.night/<field>`; no extra crates.
`cargo doc --open` documents the full API.
