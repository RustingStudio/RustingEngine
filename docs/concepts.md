# Core concepts

A short tour of the ideas the tutorials rely on.

## Projects

A project is a Cargo package plus a `project.json` manifest. The manifest
names the main scene, the cooked scene the game loads, the binary name, and
the determinism level. The editor and the `rusting` CLI read and write the
same files, so you can switch between them at any time.

## Scenes

A scene (`scenes/main.rscene`) is JSON: a list of entities, each with a
persistent ID (a UUID), a name, an optional parent, and components. A
child's transform is relative to its parent, scale included. Built-in
components have their own fields (`transform`, `mesh_renderer`, `camera`,
`rigid_body`, `collider`, lights, ...). Everything else, built-in gameplay
components such as `rusting.pickup` and your own, lives under `components`
by registered name.

- Entity names must be unique in a scene, except objects placed by an
  instance (see below). Game code finds objects by name,
  tools address them by ID.
- `rusting scene query` prints entities, `rusting scene patch` changes them
  as one atomic batch, and the editor edits them with undo.
- `rusting scene split` turns a large scene into folder form, a `.rscene`
  directory with one file per entity, so edits to different objects do not
  conflict in git; `rusting scene join` turns it back. Every tool reads and
  writes both forms.
- `rusting schema` lists every component and field, with defaults, units,
  and valid ranges.
- Registered components are described with `reflect!`. The description
  drives saving, the Inspector's widgets, the schema, and field paths
  (`set_registered_component_field`). A saved field the type does not have
  fails the load; a renamed or removed field needs a `FieldMigration`.

A scene can hold other scenes, like a prefab. An object with the
`rusting.scene_instance` component (`{"source": "prefabs/coin.rscene"}`)
gets that scene's objects as children when the scene loads. Their IDs are
the same on every load. The scene saves the link plus what you changed
on the placed objects (`rusting.instance_overrides`), so a change to
`coin.rscene` shows up everywhere it is used, except in fields an instance
changed itself. Instances can nest; a scene that contains itself fails to
load. A *variant* (Godot's inherited scene) is a scene whose one object is an
instance of a base scene: its own changes save as overrides, and it still
follows every other edit to the base. Create one with **New Variant** in the
Assets menu of a `.rscene` file. Right-click an instance in the Hierarchy to
revert your changes, write them into the source scene (**Apply to Source**),
or turn the instance into ordinary objects (**Unpack Completely**).

Game code can place a scene while the game runs. Load it once with
`AssetServer::load_prefab("assets/prefabs/coin.rscene")`, keep the returned
`Handle<Prefab>`, and place it from a system with
`commands.queue(move |world: &mut World| spawn_prefab(world, coin, transform).map(drop))`.
Each placement gets a root named `coin #1`, `coin #2`, and so on, with IDs
that are the same when the game replays the same placements in the same
order. Paths are relative to the project folder, where games run. Keep
runtime prefabs under `assets/` so that export includes them.
From a `GameScene`, `scene.spawn_prefab("prefabs/coin.rscene", "Coin 3",
transform)` does the same in one call, with the path relative to `assets/`
and the root named `Coin 3`.

**Cooking** turns the JSON scene into a compact binary
(`build/main.rscene.bin`) that the game loads. Play, `rusting run`, `test`,
and `export` cook for you. A cooked scene includes the objects of its
instances, so the game does not need the source scenes.

## Game code

A game's `main.rs` picks one of two entry points:

- **`rusting_game!(update)`**: one function called every frame with a
  `GameScene`. Find objects by name, move and rotate them, spawn cubes and
  spheres, turn on GPU physics for a class, and read GPU events. Good for
  small games and procedural setup. See [Tutorial 1](tutorials/01-hello-cube.md).
- **`rusting_game!(update, tick: tick)`**: the same, plus `tick` called
  once per fixed tick before the frame's `update`. Step game state (timers,
  AI, rules) in `tick` and draw the HUD in `update`. Inside `tick`,
  `scene.pressed(action)` is true on the first tick after the press, even
  at 144 Hz when most frames run no tick; later ticks of the same frame see
  no press.
- **A `Plugin` passed to `run_project`**: normal ECS systems (the engine uses
  `bevy_ecs`) with queries, resources, input actions, and your own
  components saved in scenes. See [Tutorial 3](tutorials/03-gameplay-plugin.md).

Both run the same engine: the same scene loader, physics, renderer, and
built-in gameplay systems.

### Signals

Signals are the ECS form of Godot's signals. Register a named handler once,
in your plugin: a system whose input says which signal it answers.

```rust
#[derive(EntityEvent, Clone)]
struct Collected { entity: Entity }

fn add_score(In(signal): In<Signal<Collected>>, mut score: ResMut<Score>) {
    score.0 += 1; // signal.source collected, signal.target receives
}

app.add_signal_handler("add_score", add_score)?;
app.connect(coin, "add_score", player)?; // or insert `Connections`
```

A system then sends `commands.trigger(Collected { entity: coin })`, and
every handler connected on the coin that takes `Signal<Collected>` runs, in
connection order, after the system's commands apply. `Signal<Added<T>>` and
`Signal<Removed<T>>` handlers answer component `T` being added to or removed
from the connected entity; a despawn counts as a removal. Connections are
data on the emitting entity, so snapshots and replays keep them, and
restoring a snapshot fires no signals. Register handlers during setup, like
systems.

Scenes save connections as the `rusting.connections` component, with each
target stored by object ID, so you can also set them up in the editor's
Inspector or a scene patch:

```json
"rusting.connections": {"list": [{"handler": "add_score", "target": "<player's id>"}]}
```

A game refuses to load a scene whose connections name a handler it did not
register or an object that is not in the scene, and the error names the
object. The editor and other tools load such scenes without checking.

### Classes (groups and tags)

A class is a name shared by any number of objects, and one object can have
several (`ObjectClasses`). Assign classes in the Inspector's Classes section;
scenes save them with each object. To find the members, read the class index
instead of scanning every object:

```rust
fn hurt_enemies(index: Res<ClassIndex>, mut bodies: Query<&mut Health>) {
    for enemy in index.members("enemy") {
        if let Ok(mut health) = bodies.get_mut(enemy) {
            health.0 -= 1;
        }
    }
}
```

`app.class_members("enemy")` does the same outside a system. Members come in
ascending entity order. The index follows every insert, removal and despawn
of `ObjectClasses`, so change an object's classes by inserting a new value,
not by editing it through `Mut<ObjectClasses>`.

### Finding objects in the scene tree

Add `SceneTree` to a system's parameters for the common lookups. It only
returns entities, so the components you read or change stay in your own
queries:

```rust
fn open_door(tree: SceneTree, mut doors: Query<&mut Transform>) {
    let Some(house) = tree.find_by_name("House") else { return };
    if let Some(door) = tree.find_path(house, "Front/Door") {
        if let Ok(mut transform) = doors.get_mut(door) {
            transform.position[1] += 3.0;
        }
    }
}
```

| Method | Returns |
| --- | --- |
| `find_by_name(name)` | The object with that name; with several, the lowest entity index. It scans every named object. |
| `child(parent, name)` | The first direct child with that name. |
| `find_path(root, "Arm/Hand")` | The object at a `/`-separated path of child names, like Godot's `get_node`. `..` steps up to the parent. |
| `parent(entity)`, `children(entity)`, `descendants(root)` | The hierarchy, in hierarchy order (`descendants` goes depth first). |
| `name(entity)` | The object's name. |
| `in_class(class)`, `is_in_class(entity, class)` | Class members from `ClassIndex`. |

## Build times

`rusting check`, `run`, `test` and `export` build game code with Cargo.
The budget is 3 seconds from saving a game file to its diagnostic once
the engine is built. Measured on forever-bear (3,700 lines of game code,
Ryzen 5 7600X, 2026-10-07): a type error is reported in 0.9 s and the
fixed build is green in 2.1 s.

The first build of a project compiles the engine and takes about 2
minutes, and every project keeps its own multi-gigabyte `target/`. To
build the engine once for all your games, point them at one shared
target directory:

```sh
export CARGO_TARGET_DIR=~/.cache/rusting-target
```

With it, a second project's first `rusting check` took 3.1 s instead of
122 s. Games that pick different engine features (for example
`default-features = false`) still compile their own copy of the engine
once. Each game needs its own package name, or their binaries overwrite
each other. `rusting export` finds the binary in the shared directory.

## Data assets

A data asset is a file of typed values that several objects share, like a
Godot `Resource` saved as `.tres`. Enemy stats, loot tables and dialogue
settings are typical. Describe the type with `reflect!`, give it a name, and
register it:

```rust
#[derive(Default, Serialize, Deserialize)]
struct EnemyStats {
    health: u32,
    speed: f32,
    base: Option<Handle<EnemyStats>>,
}

rusting_engine::reflect! {
    struct EnemyStats {
        health: u32,
        speed: f32 { unit: "m/s" },
        base: Option<Handle<EnemyStats>>,
    }
}

impl DataAsset for EnemyStats {
    const NAME: &'static str = "my_game.enemy_stats";
}

app.register_data_asset::<EnemyStats>();
```

The file is JSON with the type name and the value. References to other
assets are paths relative to the file:

```json
{
  "type": "my_game.enemy_stats",
  "data": { "health": 40, "speed": 3.5, "base": { "$asset": "monster.rdata" } }
}
```

- `AssetServer::load_data::<EnemyStats>(path)` returns a `Handle`, and
  `assets.data.get(handle)` reads the value. Loading the same file again
  returns the same handle, so every user shares one copy.
- A component field of type `Handle<EnemyStats>` saves in the scene as the
  file's path and loads the file with the scene.
- `save_data` writes a value, and `reload_data` reads the file again into
  the existing handle.
- Loading fails with the reason when the file holds another type, has a
  field the type does not have, or refers back to itself. Data assets
  cannot refer to scene objects. Renamed or removed fields are not
  migrated yet.

### Shared and unique data

A handle to a file is shared: every object that loads `goblin.rdata` reads
the same values, and an edit to the file reaches all of them. To give one
object its own values, make the reference unique, like Godot's Make
Unique. A unique value has no file. It is saved inside the object that
refers to it as `{"$data": value}`:

```json
"my_game.spawner": { "stats": { "$data": { "health": 80, "speed": 3.5, "base": null } } }
```

- Each `{"$data": ...}` loads as its own copy. Two objects, or two
  instances of the same prefab, never share a unique value, and reloading
  a file does not change copies made from it.
- `AssetServer::duplicate_data(handle)` makes a unique copy in game code,
  like `Resource.duplicate()`. Handles inside the copy still point at the
  same assets.
- A data asset file can hold unique values in its own fields too.

### Changing data while the game runs

A running game checks its loaded `.rdata` files for changes twice a
second. Save a file, from the editor's Inspector or any text editor, and
the game reads it again into the same handle, so every object that uses
it sees the new values on the next frame. Nothing restarts and no object
state is lost.

- To react to a change, compare
  `assets.data.store::<T>().unwrap().revision(handle)` with the value you
  saw last time. It goes up on every reload.
- A file that fails to load keeps its last values. The game prints
  `hot reload failed:` with the reason, and the editor's Console shows it.
- Unique copies have no file, so reloads never change them.

## Waypoint graphs

`WaypointGraph` moves a monster between hand-placed points, such as the
aisles and doors a mascot patrols. It is plain data: positions and two-way
edges. A node's ID is its index.

```rust
use rusting_engine::runtime::WaypointGraph;

let mut graph = WaypointGraph::default();
let booth = graph.add_node([0.0, 0.0, 0.0]);
let hall = graph.add_node([0.0, 0.0, 8.0]);
let aisle = graph.add_node([6.0, 0.0, 8.0]);
graph.connect(booth, hall);
graph.connect(hall, aisle);

let start = graph.nearest(monster_position).unwrap();
let route = graph.path(start, booth); // Some(vec![aisle, hall, booth])
```

- `path` returns the shortest route by straight-line edge length, both ends
  included, or `None` when nothing joins them. `length` sums a route.
- `disconnect` removes an edge, for a door that closes, and `connect` adds it
  back.
- Ties resolve the same way every run: `nearest` picks the lowest ID among
  equal distances, and `path` prefers lower IDs among equal-cost routes. A
  path chosen in a fixed tick stays in step with replays.
- The graph serializes with serde, so it can be a field of a data asset or a
  component.
- There is no navmesh, no automatic graph building and no obstacle
  avoidance between nodes. Place nodes where a straight walk between
  neighbors is clear.

## Steam achievements

`rusting_engine::steam` holds the calls a game makes to Steam:
`unlock_achievement("night_5")`, `set_stat("nights_survived", 5)` and
`is_running()`. Call them from game code now. Today they do nothing and
return `false`, in every build, so the game runs the same with or without
Steam installed.

- The optional `steam` cargo feature is reserved for the real Steamworks
  backend. That backend needs the `steamworks` crate, which waits on the
  owner's approval, so the feature adds nothing yet.
- Unlocks are presentation, like audio. Never read their result back into
  simulation state, or replays stop matching.

## Frames and fixed ticks

The engine separates **frames** from **fixed ticks**:

- A frame is one rendered image. Frame length varies with the machine.
- A fixed tick is one simulation step of exactly `fixed_delta` (1/60 s by
  default). Each frame runs as many ticks as real time requires, up to 8.

Systems run in stages: `Startup`, `FixedUpdate` (once per tick), `Update`
(once per frame), `PostUpdate`, and `RenderExtract`. Physics and anything
that must replay identically runs in `FixedUpdate` and reads
`FrameTime::fixed_delta`. Visual effects and per-frame input handling run in
`Update` and read `FrameTime::delta`.

`scene.pressed` in a plain `update` is true for one frame. A frame can
run no tick or several, so game code that steps state per tick inside
`update` misses presses on fast displays; use the `tick:` function of
`rusting_game!` instead. Scenarios run one tick per frame and do not show
the problem.

`FrameTime` also holds `elapsed` game time, `fixed_tick` (ticks completed),
and `frame`. `TimeControl` pauses, steps, and scales time.

## Physics: CPU and GPU

Every body has a `physics_body.simulation` class:

| Class | Where it runs | Use it for |
| --- | --- | --- |
| `Cpu` (default) | CPU solver | the player, doors, anything gameplay reads every frame |
| `Gpu` | Vulkan compute | debris, swarms, crowds, thousands of bodies |
| `Static` | nowhere, never moves | floors and walls; both solvers collide with it |
| `None` | nowhere | visual-only objects |

CPU bodies are ordinary ECS components: read and write them directly. GPU
bodies stay on the GPU. The CPU does not download their transforms every
frame. Instead you attach **rules** (`GpuPhysicsRule`: "position Y below
-20", "colliding", "timer elapsed") to an object or a whole **class**, and the
GPU sends back an event only when a rule matches. See
[Tutorial 4](tutorials/04-gpu-cube-rain.md), and the
[Physics guide](physics.md) for ownership, readback latency and solver
choice.

`rigid_body.kind` is `Dynamic` (moved by forces), `Kinematic` (moved by your
code or a controller), or `Fixed`.

### Gravity volumes

A `rusting.gravity_volume` on a `Fixed` CPU body with a sensor collider
replaces the scene gravity for dynamic CPU bodies that overlap it, like a
Godot `Area3D` gravity override. `gravity: [0, 0, 0]` makes a zero-g room and
a sideways vector a wind tunnel. `toward_center` above 0 pulls toward the
object's centre at that many m/s², so a sphere sensor becomes a small planet.
Where volumes overlap, the highest `priority` wins. `gravity_scale` still
applies. GPU bodies and the player controller keep the scene gravity.

### Substeps: long joint chains and heavy-on-light stacks

The CPU solver runs each fixed step as `PhysicsSettings::substeps` equal
substeps (default 1; `ProjectRunner::set_physics_substeps(n)` in a game).
More substeps hold joints and stacks tighter, at about that many times the
solver cost. Measured on a 20-ball joint chain whose tip is 50 times heavier
than each link, 300 steps in release:

| Substeps | Largest link stretch | Stretch at rest | Time |
| --- | --- | --- | --- |
| 1 | 6.6 m | 0.41 m | 29 ms |
| 4 | 0.024 m | 0.001 m | 74 ms |
| 8 | 0.009 m | 0.0004 m | 124 ms |

A 100 kg box on a 1 kg box sinks 6 cm into it with 1 substep and under 1 cm
with 4. For a chain or ragdoll that must never stretch, put
`rusting.articulation` on its root instead: the joints then hold exactly at
any mass ratio, at a cost that grows with the cube of the link count.
Substeps are a scene setting, not part of the render quality profile, so
the simulation is the same on every machine.

### Many GPU bodies: keep the contact grid uncrowded

GPU contacts use a grid. Each cell is as wide as the largest GPU body and
holds 8 bodies. A body that finds its cell full goes to a fallback list, and
every GPU body tests that whole list in each contact round. So more than 8
bodies per cell costs every body, not only the crowded ones. Do not clamp
many bodies to one plane, such as a hard ceiling or floor height in a
shader; give each body a slightly different height instead. One very large
GPU body makes every cell large, so keep GPU bodies of a similar size.

The latest physics frame's counts are in the `RenderCapacityDiagnostics`
resource: `physics_grid_overflow` (bodies that found their cell full),
`physics_oversized_bodies` and `physics_fallback_tests` (pair tests made
through the fallback list). `rusting test --json` reports the same fields
under `perf.render`. Game code reads them like this:

```rust
let overflow = scene
    .world()
    .get_resource::<RenderCapacityDiagnostics>()
    .map_or(0, |counts| counts.physics_grid_overflow);
```

### Many bodies: what costs time

With thousands of bodies, a few per-body costs decide the frame rate:

- Triangles. A `SphereSpawn` sphere has `4 * subdivisions * subdivisions`
  triangles: 36 at 3, 144 at 6, 1,024 at the default 16. Forty thousand
  balls at 6 are 5.8 million triangles a frame; at 3 they are 1.4 million
  and look the same when small or pixelated. `perf.render.triangles` in
  `rusting test --json` adds up every pass that draws a mesh, so it can be
  a multiple of the mesh count. For models, use level of detail
  (`rusting docs show guide/look-and-feel`).
- `scene.state_hash(class)` reads every object in the class: about 5 ms
  for 40,000 bodies. Call it on checkpoint ticks (every 30 ticks, or where
  a scenario checks it), not in every `update`.
- The GPU contact grid: keep cells under 8 bodies, as above.
- `scene.raycast` walks a box tree over the CPU colliders, and another
  over each mesh or heightfield collider's triangles, so a ray tests only
  what lies along it: about 0.35 microseconds per ray against 200 colliders
  and 0.3 against a 256 x 256 heightfield (131,072 triangles) in a release
  build (Ryzen 5 7600X). The collider tree is rebuilt by the first ray after
  each physics tick, so batch rays within a tick. `cargo test --release
  --lib raycast_cost_per_ray -- --ignored --nocapture` in the engine
  measures it.
- Compare runs with `rusting test --json`: `perf.tick_ms_p95` is the CPU
  side and `perf.render.gpu_ms_p95` the GPU side. `perf.render.cameras`
  splits the GPU time, draws and triangles per camera and camera screen.
  `perf.render` is filled when the scenario renders: a `max_draws` or
  `max_triangles` budget, `"gpu": true`, or a capture or `expect_pixels`
  step. Without one it is null.
- Entity count. `perf.entities_max` in `rusting test --json` is the most
  live entities after any tick: scene objects, spawned copies and counters,
  not engine resources. A `"budgets": {"max_entities": 3000}` limit fails
  the run when a scenario goes over it. Through `scene.world()`,
  `entities().len()` counts allocated slots, not live entities.
- Memory. `perf.rss_mb` is the game process's resident memory in MiB at
  the end of the scenario and `perf.rss_mb_peak` its peak since start
  (Linux only, null elsewhere). Run a long scenario twice at different
  lengths: a game that does not leak ends near the same `rss_mb`.

A `rusting.joint` component joins a CPU body to another CPU body, its
`target`, or to the world when `target` is null. `anchor` and `frame` place
the joint on this body; `target_anchor` and `target_frame` place it on the
target (in world space when there is no target). The frame's X axis is the
hinge, slider, and twist axis. `kind` picks the joint:

| Kind | Allows |
| --- | --- |
| `Fixed` | nothing; the bodies move as one |
| `Hinge` | turning about X, with an optional limit, spring, and motor |
| `Slider` | sliding along X, with an optional limit, spring, and motor |
| `BallSocket` | turning freely about the anchor |
| `ConeTwist` | swinging within a cone around X and twisting within a range |
| `Distance` | anything that keeps the anchors between `min` and `max` apart |
| `Spring` | anything, pulled toward `rest_length` |
| `Generic` | each of the six axes `Locked`, `Free`, or `Limited`, with its own spring and motor |

Jointed bodies do not collide with each other unless `collide_connected` is
true. Set `break_force` (N) or `break_torque` (N·m) above 0 to make a joint
breakable: when a fixed step's load passes it, the engine removes the
`rusting.joint` component and sends a `JointBroken` event naming the joint,
its target, and the load. GPU bodies do not support joints yet.

For chains, robots, and ragdolls, add a `rusting.articulation` component
(`{}`) to the root body. Every `Fixed`, `Hinge`, `Slider`, or `BallSocket`
joint that hangs from the root, directly or through other bodies in the
tree, is then solved in joint space (reduced coordinates). Its bodies are
placed from their parents through their joints every step, so they never
drift apart, and hinge and slider limits hold exactly. Contacts and other
joints on any link move the whole tree. A `Dynamic` root floats; a `Fixed`
or `Kinematic` root is the tree's base. Other joint kinds, and joints that
would close a loop, stay regular joints. Reduced joints do not break,
ball sockets in a tree have no cone limit, and articulations never sleep.

## Input

Game code reads **actions**, not keys. The `ActionMap` resource binds action
names to keys and mouse buttons; `RuntimeInput` holds this frame's raw
state. The engine binds `player.forward`, `player.back`, `player.left`,
`player.right` (WASD and arrows), `player.jump` (Space), and
`player.sprint` (Shift), and `player.crouch` (C or left Ctrl; the
controller crouches only with `crouch_height` above 0). Scenario tests press and release actions by name,
so a game that uses actions is testable without a keyboard.

## Tests you can run without a screen

- `rusting run <project> --ticks N` runs the game headless for N ticks.
- `rusting test <project> <scenario.json>` presses actions at fixed ticks and
  checks scene values, collision events, and screenshots.
  `rusting test <project>` runs every scenario in `tests/` and saves the
  results to `build/test-results.json`, which the editor's Agent area shows
  in its Results tab: pass or fail, the failure message, and each scenario's
  tick time (mean, p95, max) with draws and triangles when it rendered.
- `rusting determinism <project>` checks that debug, release, and one-CPU
  builds produce the same simulation, tick by tick.
- `rusting bisect <project> <other-copy> --ticks N` runs two copies of a
  game, such as two git worktrees with different code or scenes, and names
  the first tick and entity whose state differs. Entities pair by scene ID,
  so a revision that adds or removes entities still lines up.

GPU bodies do not move in `rusting run --ticks`, because no renderer runs
their compute shaders. A headless run warns when the loaded scene has GPU
bodies; bodies that game code moves to the GPU later are not counted.
`rusting test` runs them when the scenario sets `"gpu": true` or has a
`capture` step, because either opens the headless Vulkan device; software
Vulkan (lavapipe) works but is slow. Without both, a scenario's GPU bodies
stay where they spawned.

A scenario fails when no tick finishes for 60 seconds, for example after a
deadlock in game code. The failure names the last tick that finished. Set
`RUSTING_TEST_STALL_SECS` to change the limit, or to `0` to turn it off.

A scenario stops at its first failed check. To list every failed check,
including several at the same tick, set `"keep_going": true` in the
scenario, pass `--keep-going` to `rusting test`, or set the environment
variable `RUSTING_KEEP_GOING=1`.

To prove a scenario checks a mechanic, run it with that mechanic's
component switched off: `rusting test <project> tests/jump.json --without
rusting.player_controller`. Every scene the game loads leaves the component
out (a built-in section such as `collider` or `rigid_body`, or a registered
component name; repeat `--without` for more), and the result inverts: the
run passes when the scenario fails or the game crashes, and fails with
`SCENARIO_TOO_WEAK` when the scenario still passes. A `--without` run does
not update `build/test-results.json`.

To run a scenario against an exported build, pass the exported executable:
`rusting test <project> tests/smoke.json --exe exports/my_game/my_game`.
Nothing is cooked or built; the game runs from its own folder, so it reads
the export's scene and assets, while the scenario, report and test data stay
in the project. The export must be built for the machine that runs the test.

To find input that breaks a game, give a scenario `invariants` and fuzz it:
`rusting fuzz <project> tests/invariants.json --seeds 50`. Each seed runs the
scenario with seeded random presses and releases of the scene's actions (or
the `--action` names; name actions that game code binds this way), each
flipping with chance 0.1 per tick. The first seed that fails a step or an
invariant, or crashes the game, is written to `build/fuzz/seed-N.json` with
the presses as ordinary steps up to the failing tick, so `rusting test`
replays it and the file can become a regression test. A scenario can also
carry the random input itself as `"fuzz": {"seed": 3, "actions": [], "rate":
0.1}`; its report then lists the presses as `fuzz_steps`.

Every scenario report also carries `coverage`: the scene before tick 0
compared with the scene at the end. It lists the entities that changed and
which sections changed in each (`transform`, `components/rusting.counter`),
the entities added and removed, how many stayed the same, the sections and
components no entity changed (`sections_untouched`, the mechanics this
scenario does not exercise), the actions pressed, and the trace events by
kind. A value that changed and came back by the end counts as unchanged.

## Determinism

`determinism` in `project.json` is `Off`, `Local` (reproduces on one
machine), or `CrossPlatform`. The CPU simulation is deterministic today, and
recorded replays reproduce exactly. See [Determinism](determinism.md).
