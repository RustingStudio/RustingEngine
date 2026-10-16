# Coming from Godot

This page maps Godot's ideas to RustingEngine's, so you can carry over what
you know. The short version: a Godot scene tree becomes a scene file of
named objects with components, GDScript becomes Rust, and the editor and
the `rusting` command line edit the same files.

## The big differences

- **Objects, not node types.** A Godot node *is* its type (`RigidBody3D`,
  `Camera3D`). A RustingEngine object is a name, a transform, an optional
  parent and a set of components. One object can be a mesh, a body and a
  light at once, so there are fewer objects than nodes.
- **Rust, not GDScript.** Game code is one Rust crate per project. Small
  games use the `rusting_game!` macro and a `GameScene` that finds objects
  by name; larger ones add ECS systems (the engine uses `bevy_ecs`).
- **Fixed ticks are the simulation.** Physics and game rules step at a
  fixed rate, and runs repeat exactly from the same seed and inputs, so a
  scenario file can test a game without a screen. Godot has no such
  guarantee.
- **Text files all the way down.** Scenes are JSON. `rusting scene patch`
  edits them as atomic batches, so an agent or a script changes a level the
  same way the editor does.

## Nodes

| Godot | RustingEngine |
| --- | --- |
| `Node3D` | Any object: every object has a `transform` |
| `MeshInstance3D` | `mesh_renderer` with a mesh and a `rusting.material` |
| `Camera3D`, `current = true` | `camera`; `scene.set_active_camera(name)` |
| `DirectionalLight3D`, `OmniLight3D` | `directional_light`, `rusting.point_light` |
| `WorldEnvironment` | `rusting.render_settings`, `rusting.sky_light`, `rusting.fog`, `rusting.tone_mapping` and the other post components |
| `StaticBody3D` | `rigid_body` `Fixed` plus `collider` (and `physics_body`) |
| `RigidBody3D` | `rigid_body` `Dynamic` |
| `AnimatableBody3D` | `rigid_body` `Kinematic` |
| `CollisionShape3D` | `collider` on the same object; `transform.scale` scales it with the mesh |
| `Area3D` | A collider with `"sensor": true`; `scene.touching(name)` lists what is inside |
| `CharacterBody3D` with `move_and_slide` | `rusting.player_controller`: walking, slopes, steps, jumping and the camera are built in |
| `CharacterBody2D` platformer | `rusting.platformer_controller` |
| `TileMap` | `rusting.tile_map` (text rows); `scene.tile`, `scene.set_tile` |
| `RayCast3D`, `intersect_ray` | `scene.raycast(origin, direction, max_distance)` |
| `AnimationPlayer` | `rusting.animation`; `scene.play_animation(name, clip)` |
| `AnimationTree` | The state machine, blend spaces and layers in `rusting.animation`; see [Animation](animation.md) |
| `GPUParticles3D` | `rusting.particle_emitter`; `scene.particles(name, command)` |
| `AudioStreamPlayer` | `scene.play_sound(clip, volume)`, or `rusting.sound_cue` on an object |
| `AudioStreamPlayer3D` | `scene.play_sound_on(name, clip, sound)` |
| `Label`, `Button` in a HUD | `rusting.hud` (`"button": true` for a button); `scene.set_hud`, `scene.clicked()` |
| `Timer` | `scene.start_cooldown(name, seconds)`, `scene.cooldown_ready(name)` |
| `NavigationAgent3D` | `WaypointGraph` for hand-placed routes; there is no navmesh |
| `Marker3D` | An object with only a transform |

`rusting schema` lists every component and field, with defaults, units and
valid ranges.

## Scenes and the tree

| Godot | RustingEngine |
| --- | --- |
| `.tscn` scene | `.rscene` JSON file; `rusting scene split` stores one file per object for clean git merges |
| Instancing a scene in the editor | An object with `rusting.scene_instance` (`{"source": "prefabs/coin.rscene"}`) |
| Inherited scene | A variant: **New Variant** in the Assets menu |
| Editable children, overrides | `rusting.instance_overrides`, saved for you |
| Make Local | **Unpack Completely** in the Hierarchy |
| `PackedScene.instantiate()` | `scene.spawn_prefab(path, name, transform)` |
| `duplicate()` | `scene.spawn_copy(template, name, position)` |
| `queue_free()` | `scene.despawn(name)` |
| `get_node("Arm/Hand")` | `scene.object(name)` by unique name; `SceneTree::find_path(root, "Arm/Hand")` in a system |
| `get_parent()`, `get_children()` | `scene.children(name)`; `SceneTree::parent`, `SceneTree::children` in a system |
| `reparent()` | `scene.reparent(name, Some(parent))` |
| `change_scene_to_file()` | `scene.load_scene("scenes/level_2.rscene")` |
| `reload_current_scene()` | `scene.restart()` |
| `get_tree().quit()` | `scene.quit()` |
| `get_tree().paused`, `Engine.time_scale` | `scene.set_paused(paused)`, `scene.set_time_scale(scale)` |

Object names must be unique in a scene, because game code finds objects
by name. Name every object your code touches ("Guard A", "Key 1").

## Scripts

GDScript callbacks map to the two functions `rusting_game!` takes:

```rust
use rusting_engine::prelude::*;

fn tick(scene: &mut GameScene<'_>, _time: &FrameTime) {
    // _physics_process: once per fixed tick. Step game rules here.
    if scene.pressed("player.jump") {
        scene.add_to_counter("jumps", 1);
    }
}

fn update(scene: &mut GameScene<'_>, _time: &FrameTime) {
    // _ready: `once` runs its block once per round.
    scene.once("setup", |scene| {
        scene.set_counter("jumps", 0);
    });
    // _process: once per rendered frame. Draw the HUD and effects here.
    let jumps = scene.counter_value("jumps");
    scene.set_hud("Score", |hud| hud.text = format!("Jumps: {jumps}"));
}

rusting_game!(update, tick: tick);
```

| Godot | RustingEngine |
| --- | --- |
| `_ready()` | `scene.once(key, action)`; it runs again after `restart()` |
| `_process(delta)` | `update(scene, time)` |
| `_physics_process(delta)` | `tick(scene, time)` |
| `Input.is_action_just_pressed` | `scene.pressed(action)` |
| `Input.is_action_pressed` | `scene.held(action)` |
| Input Map | `rusting.input_action` objects; the `player.*` actions are already bound |
| `position`, `translate()` | `scene.object(name).position()`, `.move_by(offset)` |
| `rotation`, `look_at()` | `.rotation()`, `.look_at(target)` |
| `create_tween().tween_property()` | `scene.tween(name, property, to, seconds, easing)` |
| `randi()`, `randf()` | `scene.random(stream)`: it repeats for a scenario's seed |
| `tr()` | `scene.tr(key)` with `assets/locales/<locale>.json` |
| `@export var` | A component described with `reflect!`; the Inspector, scenes and the schema read it |
| `MultiplayerSynchronizer`, `MultiplayerSpawner` | `net::replicate`: `Replication` on the host, `Replica` on clients; see [Networking](networking.md) |
| `@rpc` functions, `rpc()` | `net::rpc::Rpcs`: `register`, `call`, `accept`; see [Networking](networking.md) |
| A script's member variables | Your own component, or a `rusting.counter` for a number the HUD shows |
| `@tool` scripts, editor plugins | Not yet |

Large games use a `Plugin` with ECS systems instead of `rusting_game!`; see
[Tutorial 3](tutorials/03-gameplay-plugin.md). For modding and designer
logic, an optional WebAssembly host runs sandboxed scripts; see
[WebAssembly scripts](scripting.md).

## Signals and groups

| Godot | RustingEngine |
| --- | --- |
| `signal` and `emit_signal` | An `EntityEvent` and `commands.trigger(event)` |
| `connect()` | `app.add_signal_handler(name, system)` once, then `app.connect(source, handler, target)` or the `rusting.connections` component |
| Built-in signals such as `body_entered` | Read `scene.touching(name)` each tick, or handle `Signal<Added<T>>` |
| Groups, `add_to_group()` | Classes: `classes` in the scene, `scene.add_class(name, class)` |
| `get_tree().get_nodes_in_group()` | `scene.in_class(class)`, or `ClassIndex::members` in a system |

See "Signals" and "Classes" in [Core concepts](concepts.md).

## Resources and autoloads

| Godot | RustingEngine |
| --- | --- |
| `Resource` saved as `.tres` | A data asset: a `reflect!` type saved as `.rdata` JSON |
| `load("res://...")` | `AssetServer::load_data::<T>(path)` returns a `Handle` |
| Make Unique, `duplicate()` | `{"$data": ...}` in the scene, `AssetServer::duplicate_data(handle)` |
| `res://` | The project folder; runtime files live under `assets/` |
| `user://` saves | `scene.save_data(key, text)`, `scene.load_data(key)`, `scene.save_counters`, `scene.save_objects` |
| Autoload singleton | A `bevy_ecs` resource: `scene.world().insert_resource(value)` |
| Import dock, `.import` files | `.rmeta` files beside each asset; `rusting asset list` shows them |

Data assets reload while the game runs when their file changes. See "Data
assets" in [Core concepts](concepts.md).

## Project, editor and export

| Godot | RustingEngine |
| --- | --- |
| Project Manager, new project | `rusting new <parent-directory> <project-name> --template 2d` (or `3d`, `first-person`, `starter` and more) |
| `project.godot` | `project.json` |
| Run (F5) | **Play** in the editor, or `rusting run <project>` |
| Export | `rusting export <project>` |
| Remote debugger | `rusting inspect`, `rusting serve`; see `rusting docs` |
| GUT or other test addons | Scenario files run by `rusting test <project>`: press actions, wait ticks, check values |
| Godot docs in the editor | `rusting docs search <query>` and `rusting docs show <item>`, offline and matched to your version |

## Not there yet

Some Godot features have no counterpart today: a navmesh, editor plugins
and `@tool` scripts, and a visual shader graph.
[Networking](networking.md) has messages, RPCs, replicated objects and
client-side prediction, but no rollback. The [roadmap](../roadmap.md) tracks each of them.
