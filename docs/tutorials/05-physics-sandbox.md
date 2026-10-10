# Tutorial 5: Physics sandbox

You will throw balls into a pile of crates. Starting from the sandbox
template, a pile of crates and balls in a walled pen, you make one ball
bouncy and add a key that throws balls from the camera. Each change is checked by a scenario test, and the last
step proves the simulation gives the same result in debug and release
builds.

In the commands below, `rusting` means `./target/debug/rusting` from the
engine checkout (build it with `cargo build --bin rusting`). Add `--json` to
any command for machine-readable output.

## 1. Create the game

```bash
mkdir -p ~/Code/games && cd ~/Code/games
rusting new . "Crate Drop" --template sandbox
```

The scene holds three kinds of objects:

- **Floor** and **Wall 1** to **Wall 4**: `Fixed` rigid bodies with box
  colliders. Fixed bodies never move; other bodies collide with them.
- **Box 1** to **Box 9** and **Ball 1**, **Ball 2**: `Dynamic` rigid bodies.
  Gravity pulls them down and they push each other.
- **Sun**, **Game Camera**, and a help HUD.

Every body has a `rigid_body` (its kind, mass, and velocities) and a
`collider` (its shape, friction, and restitution). Look at one:

```bash
rusting scene query "Crate Drop/scenes/main.rscene" --name "Ball 2" --fields collider --json
```

```json
"collider": {
    "friction": 0.5,
    "restitution": 0.0,
    "sensor": false,
    "shape": {"Sphere": {"radius": 0.5}}
}
```

## 2. Run the test that comes with it

The template ships `tests/drop.json`, a scenario that runs 300 fixed ticks
(5 seconds) without a window and checks that both balls fall but stay above
the floor:

```bash
rusting test "Crate Drop" "Crate Drop/tests/drop.json"
```

```text
Game exit code: 0, timed out: false
Scenario "drop: the balls fall and land on the pile, not through the floor": passed after 300 ticks, seed 0
```

The first run builds the engine and takes a few minutes.

## 3. Make a ball bouncy

Restitution is how much speed a body keeps when it bounces: 0 stops dead,
1 bounces back as fast as it came. Save this patch as `bounce.json`:

```json
{
  "operations": [
    {"op": "set", "name": "Ball 2", "path": "/collider/restitution", "expected": 0.0, "value": 0.8}
  ]
}
```

```bash
rusting scene patch "Crate Drop/scenes/main.rscene" bounce.json
```

```text
Patched 23b18ed3b4b3b2f0 -> a47d66c5957c001a: 0 created, 0 deleted, 1 field changes
  Ball 2/collider/restitution: 0.0 -> 0.800000011920929
```

`expected` makes the patch safe to run twice: the second time, the value is
no longer 0.0, so the patch refuses instead of overwriting someone else's
change. Object IDs and revisions are random per project, so yours differ:

```text
error [SCENE_CONFLICT]: operation 0: 6129d249-.../collider/restitution is 0.800000011920929, patch expected 0.0 (bounce.json at /operations/0, object `Ball 2` 6129d249-...)
```

## 4. Add a throw action

Actions name an input, so code and tests say `throw` instead of a key. Save
this patch as `throw-input.json`. It creates an object that binds Space and
the left mouse button to `throw`:

```json
{
  "operations": [
    {"op": "create", "entity": {
      "name": "Controls",
      "components": {
        "rusting.input_action": {"action": "throw", "inputs": ["Space", "MouseLeft"]}
      }
    }}
  ]
}
```

```bash
rusting scene patch "Crate Drop/scenes/main.rscene" throw-input.json
```

```text
Patched a47d66c5957c001a -> 21f3fc2d87140fa1: 1 created, 0 deleted, 0 field changes
```

## 5. Write the test first

Save this as `Crate Drop/tests/throw.json`. It taps `throw` at tick 30, then
checks that exactly one ball was thrown, that it starts in front of the
camera (the camera is at z = 10), and that two seconds later it has flown
into the pen:

```json
{
  "name": "throw: Space throws a ball from the camera into the pile",
  "ticks": 120,
  "steps": [
    {"tick": 30, "tap": "throw"},
    {"tick": 30, "expect": {"counter": "throws", "equals": 1}},
    {"tick": 30, "expect": {"entity": "Throw 1", "path": "/transform/position/2", "less_than": 9.5}},
    {"tick": 120, "expect": {"entity": "Throw 1", "path": "/transform/position/2", "less_than": 3.0}},
    {"tick": 120, "expect": {"entity": "Throw 2", "exists": false}}
  ]
}
```

Run it. Nothing throws yet, so it fails at the first check:

```bash
rusting test "Crate Drop" "Crate Drop/tests/throw.json"
```

```text
error [SCENARIO_FAILED]: tick 30 step 1: `counter:throws` /components/rusting.counter/value is 0, expected 1 ± 0 (inputs at this tick ran before the check: {"tap":"throw"})
```

## 6. Throw balls from the camera

Replace `Crate Drop/src/main.rs`:

```rust
use rusting_engine::prelude::*;

const THROW_SPEED: f32 = 15.0;

fn update(scene: &mut GameScene<'_>, _time: &FrameTime) {
    if !scene.pressed("throw") {
        return;
    }
    let Some((origin, forward)) = scene.camera_ray() else {
        return;
    };
    let n = scene.add_to_counter("throws", 1);
    let name = format!("Throw {n}");
    // Start one metre in front of the camera, so the ball is not inside it.
    let start = [
        origin[0] + forward[0],
        origin[1] + forward[1],
        origin[2] + forward[2],
    ];
    scene.spawn_copy("Ball 1", name.as_str(), start);
    scene.set_linear_velocity(&name, forward.map(|v| v * THROW_SPEED));
}

rusting_game!(update);
```

- `pressed("throw")` is true on the one tick the action goes down, so
  holding Space throws one ball, not one per frame.
- `camera_ray` gives the active camera's position and the unit direction it
  looks along.
- `add_to_counter` creates the `throws` counter on first use and returns the
  new value, which gives every ball a unique name.
- `spawn_copy` clones `Ball 1`, with its mesh, collider, and rigid body, at
  the start point. `set_linear_velocity` then launches it.

Run both tests by naming the folder:

```bash
rusting test "Crate Drop" "Crate Drop/tests"
```

```text
PASS Crate Drop/tests/drop.json: "drop: the balls fall and land on the pile, not through the floor" after 300 ticks
PASS Crate Drop/tests/throw.json: "throw: Space throws a ball from the camera into the pile" after 120 ticks
```

Play it with `rusting run "Crate Drop"` and press Space or click.

## 7. Check determinism

The engine steps physics at a fixed rate, in a fixed body order, so the same
inputs give the same simulation. `determinism` runs a scenario in a debug and
a release build and compares a hash of the whole state at every tick:

```bash
rusting determinism "Crate Drop" --scenario "Crate Drop/tests/throw.json"
```

```text
determinism: building and running the debug build...
determinism: building and running the release build...
deterministic: debug == release on all 120 ticks (the hash at each tick covers the whole state), final hash 0xe3c8e9c6af7d175b
```

Keep it that way: take randomness from the scene's seed, not a global random
generator, and do not read the wall clock in gameplay code. See
[Determinism](../determinism.md).

## What you learned

- `Fixed` and `Dynamic` rigid bodies, colliders, and restitution.
- `scene patch` with `expected` values that refuse a stale edit.
- Input actions from `rusting.input_action`, and `pressed` in code.
- Spawning and launching bodies with `spawn_copy`, `camera_ray`, and
  `set_linear_velocity`.
- Test-first scenarios with `tap`, `counter`, and `exists` checks.
- `rusting determinism` to prove debug and release builds agree.
