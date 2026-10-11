# Physics guide

This guide answers four questions for every moving object in a game:

1. **Ownership**: which solver owns the body, the CPU or the GPU?
2. **Synchronization**: how do game code and the other solver learn what
   the body did, and how old is that information?
3. **Determinism**: what makes the same inputs give the same simulation?
4. **Solver choice**: which solver or physics feature fits the job?

For the component fields, run `rusting docs show component/<name>`, for
example `rusting docs show component/physics_body`. For a first physics
game, work through [Tutorial 5](tutorials/05-physics-sandbox.md) and
[Tutorial 4](tutorials/04-gpu-cube-rain.md).

## Ownership: where a body lives

A body needs three components: `physics_body` (where it simulates),
`rigid_body` (its kind, mass and velocities) and `collider` (its shape).
`physics_body.simulation` picks the owner:

| Class | Owner | Newest state is in | Use it for |
| --- | --- | --- | --- |
| `Cpu` (default) | CPU solver, every fixed tick | the ECS `Transform` and `RigidBody` | the player, doors, vehicles, anything gameplay reads or changes every tick |
| `Gpu` | Vulkan compute, every fixed tick | GPU memory; the CPU sees events and requested copies | debris, swarms, rain, thousands of similar bodies |
| `Static` | nobody; it never moves | the ECS | floors and walls; both solvers collide with it |
| `None` | nobody | the ECS | visual-only objects |

Exactly one solver owns a body at a time. The owner writes its state; the
other side only reads a copy.

`rigid_body.kind` says how the owner moves it: `Dynamic` (forces, gravity
and contacts), `Kinematic` (your code sets its velocity or pose; contacts
do not push it) or `Fixed`.

### Letting the engine choose

Add `rusting.auto_simulation` (`{}`) to a body and the engine picks `Cpu`
or `Gpu` once, at the end of the first frame that has the body, and
records why in `AutoSimulation::decision`. Hard requirements come first:

| Reason | Class |
| --- | --- |
| The body is `None` or `Static` | unchanged |
| No GPU physics backend (no Vulkan device, a headless run) | `Cpu` |
| `solver` is `Custom` | `Gpu` |
| `rigid_body.kind` is `Kinematic` | `Cpu` |
| A sensor, or a `ConvexMesh`, `TriangleMesh`, `Heightfield`, `Polygon` or `Chain` collider | `Cpu` |
| `rusting.physics_sync` reads state back every tick | `Cpu` |

The other bodies are "flexible". They all go to the GPU when there are at
least `AutoAllocationPolicy::gpu_min_bodies` (256) of them, or when the
last frame's CPU physics took longer than `cpu_physics_budget` (4 ms).
Otherwise they stay on the CPU. A decision sticks: a body never moves
between solvers by itself, because that needs a handoff of live state.

## Synchronization: who sees what, and when

### CPU bodies: same tick

The CPU solver runs in `FixedUpdate` and writes `Transform` and
`RigidBody` straight back into the ECS. Game code in the same tick reads
the result, and `scene.raycast`, `scene.touching` and the
`PhysicsWorld` queries answer against the poses of the last step. A
velocity or pose you set applies on the next step.

### GPU bodies: commands down, events up

The CPU never waits for the GPU. The renderer reads GPU results only after
the frame that produced them has finished, so they reach game code one to
three rendered frames after the fixed tick they describe. Every result
carries that tick.

What travels from the CPU to the GPU:

- **Scene edits.** Spawning, despawning, and changing a body's solver,
  collider or watch rules need no command. The next frame rebuilds the GPU
  tables and keeps every surviving body's simulated state. Editing a GPU
  body's `Transform` restarts the body from that pose.
- **Commands.** `scene.gpu_command(name, command)` teleports a body, sets
  its velocity, applies an impulse or a one-tick force, sets its four
  custom values, or asks for its state once (`GpuBodyCommand::ReadState`).
  A command pushed during fixed tick `n` applies before GPU tick `n`,
  however many ticks a frame runs. It returns `false` until the engine has
  given a new body its GPU id, the frame after it spawns.
- **CPU colliders.** Every solid `Cpu` and `Static` collider of the last
  CPU step (box, sphere or capsule) is uploaded each frame, with the body's
  velocity, so GPU bodies collide with them. The coupling is one way: a
  GPU body bounces off a CPU body, but the CPU body does not feel it.
- **Force fields.** `rusting.force_field` sensors push GPU bodies as well
  as CPU bodies.

What travels from the GPU to the CPU, chosen per body by
`rusting.physics_sync`:

| `rusting.physics_sync` | Read back per fixed tick | Game code reads |
| --- | --- | --- |
| `None` | nothing; watch rules are not evaluated | nothing |
| `Events` (default) | only the events that fired | `scene.gpu_events(name)` |
| `SelectedState` | events, plus 144 bytes of pose and velocities | `scene.gpu_state(name)` every frame |
| `FullState` | as `SelectedState`, plus the custom values | `scene.gpu_state(name)` with `custom_values` |

Events come from watch rules (`gpu_physics_watch` in a scene, or
`scene.watch_gpu_object` and `scene.watch_gpu_class` in code). A rule is a
condition the GPU checks every tick, such as "position Y below -20" or
"colliding", and an event mode: `OnEnter`, `OnExit`, `WhileTrue` or
`Once`. An event can carry four floats (position, velocity, angular
velocity, contact or custom values) so the CPU needs no other readback.
[GPU condition shaders](gpu-condition-shaders.md) add custom conditions.

`scene.gpu_state(name)` returns a `GpuStateMirror`. The object's own
`Transform` keeps its spawn pose, because writing the late GPU pose into
it would restart the body there. Check the state's age before using it
for a same-tick decision:

```rust
if let Some(state) = scene.gpu_state("Boulder") {
    let tick = scene.world().resource::<FrameTime>().fixed_tick;
    if state.age_ticks(tick) <= 3 {
        // state.transform, state.linear_velocity
    }
}
```

When gameplay needs a whole group's state now and then, call
`request_gpu_class_snapshot(scene.world(), class)` instead of paying
`SelectedState` every tick. Every member's state arrives together, with
one tick.

When CPU queries must see a GPU body (a raycast, a CPU body landing on
it), add a `GpuQueryProxy` in Rust. The engine then keeps a `Static` CPU
collider at the position the body's `place_on` event reports. The proxy is
as old as that event.

### Limits

- The GPU event buffer has a fixed size. Events past it are lost, counted
  in `GpuPhysicsEventsLost` and logged. Lost `OnEnter` and `OnExit` edges
  do not come back, so resynchronize from state if gameplay depends on
  them.
- GPU bodies need a renderer. In `rusting run --server`, `--ticks` runs and
  scenarios without `"gpu": true`, they stay where they spawned.
- GPU bodies have no joints, no sensors and no mesh colliders. Use CPU
  bodies for those.

## Determinism

The engine runs physics at a fixed tick (1/60 s by default) and the
result does not depend on frame rate. With the same scene, seed and
inputs, both solvers give the same bits on the same machine and build:

- The CPU solver sorts bodies by entity, solves contacts in that order and
  runs a fixed 8 velocity passes per step.
- GPU contacts keep, for each body, the largest push each way per axis, so
  the order in which threads find contacts does not change the result.
- Commands apply on the tick they name, not the frame they arrive in.
- Randomness comes from `RandomSeed`, keyed by subsystem and fixed tick.

What you must do in game code:

- Change simulation state in the fixed tick (the `tick:` function of
  `rusting_game!`, or `FixedUpdate`), and use `FrameTime::fixed_delta`.
- Take random values from the scene seed, never from a global generator,
  and do not read the wall clock.
- Do not let an overflowing GPU event buffer decide gameplay.

Check it with `rusting determinism`, which runs a scenario in a debug and
a release build and compares a hash of the whole state every tick. The
engine is deterministic on one machine (`Local`), not yet across
different machines. [Determinism](determinism.md) has the full rules, the
state hash and replays.

## Solver choice

Start from the job, not the solver:

| Job | Use | Owner |
| --- | --- | --- |
| Player character | `rusting.player_controller` (walks with `move_character`, slides along colliders) | CPU |
| Props the player pushes, picks up or stands on | `Dynamic` body, `Cpu` | CPU |
| Moving platforms and doors | `Kinematic` body, `Cpu`, or a tween | CPU |
| Thousands of falling, piling or swarming bodies | `Gpu` with watch rules | GPU |
| Bodies that ignore each other (rain, sparks with weight) | `Gpu` with solver `NoCollision` | GPU |
| Bodies pulled to a point (orbits, a black hole) | `Gpu` with solver `Space` | GPU |
| Your own integration rule | `Gpu` with solver `Custom` and a GLSL `solve` | GPU |
| Hinges, sliders, springs, breakable links | `rusting.joint` | CPU |
| Chains, robot arms, ragdoll limbs that must not stretch | `rusting.articulation` on the root | CPU |
| Ropes | `rusting.rope` (beads joined by distance joints) | CPU |
| Cars | `rusting.vehicle` (raycast wheels, tire curve, gearbox) | CPU |
| Characters going limp | `rusting.ragdoll` | CPU |
| Flags, curtains, capes | `rusting.cloth` (XPBD) | CPU |
| Squishy blocks | `rusting.soft_block` (XPBD tetrahedra) | CPU |
| A splash in a tank, a few thousand particles | `rusting.fluid_block` (position based fluids) | CPU |
| Seas, lakes and rivers that bodies float on | `rusting.water` (analytic waves) | CPU, cheap |
| Zero-g rooms, planets, wind tunnels | `rusting.gravity_volume`, `rusting.force_field` | CPU (force fields also push GPU bodies) |

### The CPU rigid-body solver

Sequential impulses with 8 velocity passes per step and sleeping after a
few quiet ticks. Long joint chains and heavy bodies on light ones need
more substeps: `substeps` in the `rusting.physics_settings` resource, or
`scene.set_physics_substeps(n)` in code. Cost grows about linearly with
substeps. For a chain that must never stretch, use an articulation, which
holds exactly at any mass ratio. [Core concepts](concepts.md) has the
measurements, the joint kinds, and what costs time with many bodies.

### The GPU solvers

`physics_body.solver` picks a GPU profile. It does nothing for `Cpu`
bodies.

| Solver | Gravity | Collides with CPU and Static colliders | Collides with other GPU bodies |
| --- | --- | --- | --- |
| `Full` (default) | scene gravity | yes | yes |
| `Simplified` | same as `Full` today | yes | yes |
| `NoCollision` | scene gravity | no | no |
| `Space` | pull toward the world origin instead | yes | yes |
| `Custom` | your `solve` function moves the body | no | no |

GPU contacts use a grid sized by the largest GPU body; keep GPU bodies of
a similar size and under 8 per cell (see "Many GPU bodies" in
[Core concepts](concepts.md)). `Custom` shaders are described in
[GPU condition shaders](gpu-condition-shaders.md).

### Cloth and soft bodies on the GPU

`rusting.cloth` and `rusting.soft_block` run on the CPU. In Rust, a
`GpuCloth` component steps the same volume on the GPU, bit for bit equal
to the CPU reference, and shows it one to three frames late. Wind,
self-collision, tearing, attachments and collider obstacles stay CPU-only.

## Debugging

- `rusting test --json` reports `perf.tick_ms_p95` (CPU tick time) and,
  when the scenario renders, `perf.render` with the GPU contact grid
  counters `physics_grid_overflow`, `physics_oversized_bodies` and
  `physics_fallback_tests`.
- `scene.state_hash(class)` hashes a class's bodies; a scenario can check
  it.
- `rusting validate` refuses a `CrossPlatform` determinism project with
  physics bodies, because no solver meets that level yet.
