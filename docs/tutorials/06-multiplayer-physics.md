# Tutorial 6: Multiplayer physics

You will turn Crate Drop from [Tutorial 5](05-physics-sandbox.md) into a
networked game. One dedicated server simulates every crate and ball. Players
join it, see the same pile, and throw balls into it. A scenario test plays a
client against a running server.

The model is host-authoritative: only the server runs physics, and clients
draw the transforms it sends. A client cannot move a crate or fake a throw,
because it only asks the server to throw. See [Networking](../networking.md)
for the other models, such as lockstep and client prediction.

In the commands below, `rusting` means `./target/debug/rusting` from the
engine checkout. Start from the finished Tutorial 5 project.

## 1. Choose a role at start

The same executable is the server and the client:

- `rusting run --server` runs the game with no window and inserts the
  `DedicatedServer` resource. The game hosts on port 7777.
- With the `CRATE_DROP_JOIN` environment variable set to an address, the
  game joins that host.
- Otherwise it plays alone, exactly as in Tutorial 5, so the Tutorial 5
  tests still pass.

## 2. Write the networked game

Replace `Crate Drop/src/main.rs`:

```rust
use rusting_engine::bevy_ecs::prelude::{Mut, Resource, Without};
use rusting_engine::net::replicate::{Replica, Replicated, Replication, NAME, TRANSFORM};
use rusting_engine::net::{DedicatedServer, NetEvent, NetSession, HOST};
use rusting_engine::prelude::*;
use rusting_engine::runtime::MeshRenderer;

const THROW_SPEED: f32 = 15.0;
const PORT: u16 = 7777;

/// The host simulates every body and sends their transforms to clients.
#[derive(Resource)]
struct Host(Replication);

/// A client draws the host's bodies and asks the host to throw.
#[derive(Resource)]
struct Client(Replica);

/// What the host sends: each object's transform and name, rounded to a
/// centimetre and a thousandth of a radian. Both sides build the same one.
fn replication() -> Replication {
    let mut replication = Replication::new();
    replication
        .component(TRANSFORM)
        .component(NAME)
        .quantize("/transform/position", 0.01)
        .quantize("/transform/rotation", 0.001);
    replication
}

fn update(scene: &mut GameScene<'_>, _time: &FrameTime) {
    scene.once("network", start_network);
    if scene.world().contains_resource::<Host>() {
        host_update(scene);
    } else if scene.world().contains_resource::<Client>() {
        client_update(scene);
    } else if scene.pressed("throw") {
        throw(scene);
    }
}

/// A dedicated server hosts; `CRATE_DROP_JOIN=address` joins a host; any
/// other run plays alone, as in Tutorial 5.
fn start_network(scene: &mut GameScene<'_>) {
    let world = scene.world();
    let hosting = world.contains_resource::<DedicatedServer>();
    if hosting {
        let session = NetSession::host(PORT, "").expect("host on port 7777");
        world.insert_resource(session);
        world.insert_resource(Host(replication()));
    } else if let Ok(address) = std::env::var("CRATE_DROP_JOIN") {
        let session = NetSession::join(address.as_str(), "").expect("join the host");
        world.insert_resource(session);
        world.insert_resource(Client(Replica::new(&replication())));
    } else {
        return;
    }
    // Both sides loaded the same scene, so its bodies share their ids:
    // mark them so the host's transforms update the client's copies.
    let mut bodies = world.query::<(Entity, &Name, &RigidBody)>();
    let dynamic: Vec<(Entity, String)> = bodies
        .iter(world)
        .filter(|(_, _, body)| body.kind == RigidBodyKind::Dynamic)
        .map(|(entity, name, _)| (entity, name.0.clone()))
        .collect();
    for (entity, name) in dynamic {
        scene.world().entity_mut(entity).insert(Replicated);
        if !hosting {
            // The host simulates; the client only shows where bodies are.
            scene.set_body_kind(&name, RigidBodyKind::Kinematic);
        }
    }
}

fn host_update(scene: &mut GameScene<'_>) {
    let events = scene.world().resource::<NetSession>().poll();
    for event in events {
        match event {
            NetEvent::Connected(peer) => {
                let full = scene.world().resource::<Host>().0.full();
                let _ = scene.world().resource::<NetSession>().send(peer, &full);
            }
            // A client asks for a throw; the host decides where it starts
            // and how fast it goes, so a client cannot cheat on either.
            NetEvent::Message { bytes, .. } if bytes == b"throw" => throw(scene),
            _ => {}
        }
    }
    let delta = scene
        .world()
        .resource_scope(|world, mut host: Mut<Host>| host.0.delta(world));
    if let Ok(Some(delta)) = delta {
        let _ = scene.world().resource::<NetSession>().broadcast(&delta);
    }
}

fn client_update(scene: &mut GameScene<'_>) {
    if scene.pressed("throw") {
        let _ = scene.world().resource::<NetSession>().send(HOST, b"throw");
    }
    let events = scene.world().resource::<NetSession>().poll();
    for event in events {
        match event {
            NetEvent::Message { from, bytes } => {
                let applied = scene
                    .world()
                    .resource_scope(|world, mut client: Mut<Client>| {
                        client.0.apply(world, from, &bytes)
                    });
                if let Some(Err(reason)) = applied {
                    eprintln!("bad message from the host: {reason}");
                }
            }
            NetEvent::Closed => {
                eprintln!("the host left");
                scene.quit();
            }
            _ => {}
        }
    }
    // Replication sends transforms and names, not meshes: give each ball
    // the host threw the look of Ball 1.
    let world = scene.world();
    let Some(look) = world
        .query::<(&Name, &MeshRenderer)>()
        .iter(world)
        .find(|(name, _)| name.0 == "Ball 1")
        .map(|(_, mesh)| *mesh)
    else {
        return;
    };
    let unseen: Vec<Entity> = world
        .query_filtered::<(Entity, &Name), Without<MeshRenderer>>()
        .iter(world)
        .filter(|(_, name)| name.0.starts_with("Throw "))
        .map(|(entity, _)| entity)
        .collect();
    for entity in unseen {
        world.entity_mut(entity).insert(look);
    }
}

fn throw(scene: &mut GameScene<'_>) {
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
    if let Some(ball) = scene.spawn_copy("Ball 1", name.as_str(), start) {
        scene.world().entity_mut(ball).insert(Replicated);
    }
    scene.set_linear_velocity(&name, forward.map(|v| v * THROW_SPEED));
}

rusting_game!(update);
```

How it works:

- **Shared ids.** The server and every client load the same scene, so each
  crate has the same scene id everywhere. Marking the crates `Replicated` on
  both sides makes the server's updates move the client's own copies.
- **The client does not simulate.** `set_body_kind(.., Kinematic)` turns the
  client's crates into bodies that only follow their transforms, so local
  physics never fights the server's positions.
- **Updates.** `Replication::full()` sends the whole state to a player who
  just connected. After that, `delta` sends only what changed since the last
  tick: transforms rounded to 1 cm and 0.001 rad, so resting crates send
  nothing.
- **Throws.** A client sends the bytes `throw`. The server spawns the ball
  with `spawn_copy`, marks it `Replicated`, and launches it. The client
  receives a new object with a name and a transform, and gives it Ball 1's
  mesh, since replication does not send meshes.

A real game would also limit how often each client may throw. See
"Trusting nothing a client sends" in [Networking](../networking.md).

## 3. Play it

Start the server in one terminal. It runs until you stop it with Ctrl+C:

```bash
rusting run "Crate Drop" --server
```

Join it from two more terminals, one window each:

```bash
CRATE_DROP_JOIN=127.0.0.1:7777 rusting run "Crate Drop"
```

Press Space or click in either window. The ball appears in both, and the
crates it knocks over land in the same places in both, because only the
server decides where they go. On another machine, use the server's LAN
address and open TCP and UDP port 7777 in its firewall.

## 4. Test a client against the server

A normal scenario runs ticks as fast as it can. A client that talks to a
server must keep the server's pace, so `"realtime": true` runs one tick per
fixed step of real time. Save this as `Crate Drop/net-tests/online.json`,
outside `tests/` so the offline test run does not pick it up:

```json
{
  "name": "online: the host throws for the client and sends the ball back",
  "ticks": 240,
  "realtime": true,
  "steps": [
    {"tick": 30, "tap": "throw"},
    {"tick": 240, "within": 240, "expect": {"entity": "Throw 1", "path": "/transform/position/2", "less_than": 3.0}},
    {"tick": 240, "expect": {"entity": "Throw 1", "path": "/mesh_renderer/mesh", "equals": "BuiltinSphere"}},
    {"tick": 240, "expect": {"counter": "throws", "exists": false}}
  ]
}
```

The client taps `throw` at tick 30, and the ball must reach the pile
(z < 3) within the next 240 ticks. It must also look like a ball, and the
client must have no `throws` counter of its own, which proves the server
did the throw.

With the server running in another terminal:

```bash
CRATE_DROP_JOIN=127.0.0.1:7777 rusting test "Crate Drop" "Crate Drop/net-tests/online.json"
```

```text
Game exit code: 0, timed out: false
Scenario "online: the host throws for the client and sends the ball back": passed after 240 ticks, seed 0
```

Without `CRATE_DROP_JOIN` the game plays alone, throws the ball itself, and
the test fails, as it should:

```text
error [SCENARIO_FAILED]: tick 240 step 3: `counter:throws` /components/rusting.counter/value still exists
```

The offline tests from Tutorial 5 still pass:

```bash
rusting test "Crate Drop" "Crate Drop/tests"
```

```text
PASS Crate Drop/tests/drop.json: "drop: the balls fall and land on the pile, not through the floor" after 300 ticks
PASS Crate Drop/tests/throw.json: "throw: Space throws a ball from the camera into the pile" after 120 ticks
```

## What you learned

- One executable as dedicated server, client, and single-player game.
- Host-authoritative physics: `Replication` on the server, `Replica` on the
  client, and kinematic copies that follow the server.
- Marking scene objects and spawned objects `Replicated`.
- Clients that send requests, not results.
- `"realtime": true` scenarios that test a client against a live server.
