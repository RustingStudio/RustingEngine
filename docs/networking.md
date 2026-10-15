# Networking

`rusting_engine::net` sends reliable, ordered messages between one host and
its clients over TCP, and unreliable ones over UDP. It uses only the
standard library. Every client talks
to the host, and the host forwards what other clients need (a star).

- The host has id `HOST` (0). Clients get ids from 1 in join order. A
  `PeerId` is a `u32`.
- A message is a byte string. It arrives whole and in order. Put JSON
  (`serde_json::to_vec`) or bincode in it.
- `poll()` never blocks. Call it once per frame or tick.
- `rusting docs show api/net` lists every type and function with its
  signature; `api/net::predict`, `api/net::input` and
  `api/net::interpolate` list the helpers below, and `api/<name>` (such as
  `api/NetSession` or `api/Smoothing::correct`) shows one.

## Host and join by address

For a LAN game or a host with an open port:

```rust
use rusting_engine::net::{NetEvent, NetSession, HOST};
use rusting_engine::prelude::*;

fn start_host(scene: &mut GameScene<'_>) {
    match NetSession::host(7777, "") {
        Ok(session) => scene.world().insert_resource(session),
        Err(error) => eprintln!("host failed: {error}"),
    }
}

fn start_client(scene: &mut GameScene<'_>, address: &str) {
    // A join waits up to 5 seconds for the host's answer.
    if let Ok(session) = NetSession::join(address, "") {
        scene.world().insert_resource(session);
    }
}

fn update(scene: &mut GameScene<'_>, _time: &FrameTime) {
    let Some(session) = scene.world().get_resource::<NetSession>() else {
        return;
    };
    for event in session.poll() {
        match event {
            NetEvent::Connected(peer) => { /* send the new player the world */ }
            NetEvent::Disconnected(peer) => { /* remove that player's raft */ }
            NetEvent::Message { from, bytes } => { /* apply it */ }
            NetEvent::Closed => { /* the host left: back to the menu */ }
        }
    }
}
```

- `send(peer, bytes)` sends to one peer. A client can send only to `HOST`.
- `broadcast(bytes)` sends to every client from the host, or to the host
  from a client.
- `peers()` lists the host's connected clients.
- Pass a password to `host` and the same one to `join` to keep strangers
  out: `NetSession::host(7777, "raft")`. A wrong one fails the join with
  `ConnectionRefused` and the message "wrong password". `""` means none.
- Remove the resource (`scene.world().remove_resource::<NetSession>()`) to
  leave. Dropping a host session closes every client and frees the port.

## Room codes through a relay

When players are behind home routers, run a relay on a machine both can
reach:

```sh
rusting relay 0.0.0.0:7777
```

To expose a relay on the internet, give it a token. Hosts and clients
without it are refused:

```sh
RUSTING_RELAY_TOKEN=long-random-string rusting relay 0.0.0.0:7777
```

The host asks the relay for a room, and players join with the code:

```rust
let host = NetSession::host_room("relay.example.com:7777", TOKEN)?;
let code = host.room_code().unwrap(); // six characters, such as "K7QXRM"

let client = NetSession::join_room("relay.example.com:7777", "k7qxrm", TOKEN)?;
```

Codes ignore case and leave out the look-alikes I, O, 0 and 1. The room
closes when its host leaves. Game code is the same as for direct
sessions; only the constructor differs.

## Sharing the world seed

A procedural world must use the same seed on every peer. The host puts its
seed in the session, and each joining client receives it in the join
handshake, direct or through a relay:

```rust
// Host, right after NetSession::host or host_room.
session.set_seed(scene.seed())?;

// Client, right after NetSession::join or join_room.
if let Some(seed) = session.seed() {
    scene.set_seed(seed);
}
```

- `seed()` is `None` when the host set no seed, or when the client joined
  before the host called `set_seed`. Call it before telling players the
  address or room code.
- A client cannot call `set_seed`; it returns an error.
- A dedicated server takes its seed from `rusting run --server --seed N`,
  or 0 without one, so clients no longer need `--seed` too.

## Listen servers and dedicated servers

A listen server is a player's game that also hosts: call
`NetSession::host` in it, and the host plays like any client. A dedicated
server runs the same game with no window or player:

```sh
rusting run my_game --server            # from the project
RUSTING_SERVER=1 ./my_game              # an exported game
```

- It ticks once per fixed step of real time, renders nothing, and runs
  until game code calls `scene.quit()` (or `--timeout` stops it). Ctrl+C
  and SIGTERM stop it too; either way game code gets one last update with
  `scene.exiting()` true to save in.
- Game code sees the `net::DedicatedServer` resource there. Host instead
  of showing a menu:

```rust
use rusting_engine::net::DedicatedServer;

fn update(scene: &mut GameScene<'_>, _time: &FrameTime) {
    let world = scene.world();
    if world.contains_resource::<DedicatedServer>()
        && world.get_resource::<NetSession>().is_none()
    {
        if let Ok(session) = NetSession::host(7777, "") {
            world.insert_resource(session);
        }
    }
}
```

- The `DedicatedServer` resource also reports the loop's load: `budget`
  (the share of the fixed step a tick's work takes, smoothed; above 1.0
  the server cannot keep up), `last_budget`, `late_ticks`, and
  `dropped_ticks` (skipped after a stall of over 10 steps, with a
  warning on stderr). Send them to your monitoring or print them.
- GPU physics bodies stay still without a renderer, as in `--ticks` runs;
  use CPU bodies for server-side simulation.

## Players, ready checks and reconnects

A `PeerId` belongs to one connection. `net::lobby::Lobby` gives the host
players that outlive it, so a player who loses the connection can come
back to the same place in the match:

```rust
use rusting_engine::net::lobby::{self, Lobby, LobbyEvent, Rejoin};
use std::time::{Duration, Instant};

// Host: keep a dropped player's place for 60 seconds.
let mut players = Lobby::new(Duration::from_secs(60));
for event in session.poll() {
    match players.handle(&session, &event) {
        Some(Ok(LobbyEvent::Joined(player))) => { /* add a raft */ }
        Some(Ok(LobbyEvent::Rejoined(player))) => { /* send it the world */ }
        Some(Ok(LobbyEvent::Ready(_))) => { /* start when all are ready */ }
        Some(Ok(LobbyEvent::Dropped(player))) => { /* pause its raft */ }
        Some(Ok(LobbyEvent::Left(_))) | None => { /* game messages: event */ }
        Some(Err(reason)) => eprintln!("lobby: {reason}"),
    }
}
for left in players.expire(Instant::now()) { /* remove its raft */ }
let playing = players.start_match(); // every connected ready player

// Client: after joining, and again after reconnecting.
lobby::hello(&session, saved_rejoin)?;          // None the first time
// on each message from the host:
if let Some(rejoin) = lobby::welcome_from(from, &bytes) {
    saved_rejoin = Some(rejoin);                // keep it to come back
}
lobby::ready(&session)?;
```

- Stages are `Joined`, `Ready` and `InMatch` (`stage(player)`); a
  rejoined player gets its stage back. `peer(player)` and
  `player(peer)` map between players and connections.
- The host answers each hello with a player id and a secret token, and
  a new token on every rejoin. Only a dropped player can be taken over,
  so a token seen on the wire cannot steal a connected player.

### Spectators

A client that only watches calls `lobby::spectate(&session)` instead of
`hello`. The host's lobby reports `LobbyEvent::Spectating(peer)` and,
when it leaves, `SpectatorLeft(peer)`. A spectator has no player, cannot
ready up or join a match, and receives everything the host broadcasts
or sends each peer, replication included. Take game input only from
peers the lobby accepts:

```rust
NetEvent::Message { from, bytes } if players.accepts_input(from) => {
    // validate, then buffer the input
}
```

`accepts_input` is true only for a player in the match. `spectators()`
lists who is watching.

## Agreeing on the tick

Predicted inputs carry a tick, and the host should get each one before
it runs that tick. `net::clock` gives the client the host's tick:

```rust
use rusting_engine::net::clock::{self, ClockSync, TickOffsets};

// Client: once, then ping every second or so.
let mut clock = ClockSync::new(time.fixed_delta);
clock.ping(&session)?;
// on each message from the host (local_tick as f64 with any fraction):
if clock.accept(from, &bytes, local_tick) { continue; }
// stamp inputs so they arrive 2 ticks early:
let tick = clock.input_tick(local_tick, 2.0);

// Host: answer pings, and measure how early inputs arrive.
if clock::answer(&session, from, &bytes, host_tick) { continue; }
offsets.observe(from, input.tick, host_tick); // lead(peer) in ticks
```

- `host_tick(local_tick)` and `round_trip()` are `None` until the first
  answer. Of the last 8 samples, the one with the shortest round trip
  counts, so one delayed packet does not move the estimate.
- A client whose `TickOffsets::lead` falls below zero sends too late;
  tell it to raise its margin.

On the host, `net::input::InputBuffer` holds inputs until their tick, so
uneven arrival (jitter) does not stall anyone:

```rust
use rusting_engine::net::input::InputBuffer;

let mut inputs = InputBuffer::new(first_tick);
// on each input message:
if let Err(reason) = inputs.push(from, input.tick, input.steer) { /* log */ }
// each host tick:
let (tick, this_tick) = inputs.take();
for entry in this_tick { /* apply entry.input for entry.peer */ }
```

- The margin passed to `input_tick` is the input delay: more ticks
  absorb more jitter but add lag. `buffered(peer)` shows a client's
  current cushion and `late(peer)` how many inputs missed their tick.
- A late or lost input repeats the peer's last one (`fresh: false`).
- Inputs more than `MAX_AHEAD` (120) ticks ahead are refused.

## Trusting nothing a client sends

On a public server, assume some clients are modified. Keep the host the
authority:

- Clients send inputs (stick, buttons, aim), never positions,
  velocities or world state. The host runs the movement itself;
  `net::predict` lets the client still feel instant.
- Check every input before using it: numbers finite and in range
  (`x.is_finite() && x.abs() <= 1.0`), ticks near the host's, names
  ones you registered. Drop the message on any failure.
- `net::rpc` already checks who may call a method, and
  `net::replicate` only moves state from the host to clients: the
  host's `Replication::accept` returns an error for a delta, snapshot,
  whole state or checksum a client sends. Drop the message, or the
  client.
- Limit how often each client may send with `net::limit::RateLimit`:

```rust
use rusting_engine::net::limit::RateLimit;
use std::time::Instant;

let mut limit = RateLimit::new(120.0, 30); // 120 a second, bursts of 30
for event in session.poll() {
    match event {
        NetEvent::Message { from, bytes } => {
            if !limit.allow(from, Instant::now()) {
                continue; // over its rate: drop it
            }
            // validate, then apply
        }
        NetEvent::Disconnected(peer) => limit.forget(peer),
        _ => {}
    }
}
```

## Remote procedure calls

`net::rpc::Rpcs` calls a named method on a scene object across the
session. Register the same methods on every peer, with who may call each
and how it travels:

```rust
use rusting_engine::net::rpc::{Authority, Reliability, Rpcs};

let mut rpcs = Rpcs::new();
rpcs.register("jump", Authority::Owner, Reliability::Reliable)
    .register("aim", Authority::Owner, Reliability::Unreliable)
    .register("explode", Authority::Host, Reliability::Reliable);
rpcs.set_owner("Raft 2", 2); // on the host: client 2 steers it

rpcs.call(&session, "Raft 2", "jump", b"")?; // client: runs on the host

for event in session.poll() {
    if let NetEvent::Message { from, bytes } = event {
        match rpcs.accept(from, &bytes) {
            Some(Ok(call)) => { /* apply call.method to call.object */ }
            Some(Err(reason)) => eprintln!("refused: {reason}"),
            None => { /* a plain game message */ }
        }
    }
}
```

- `Authority::Host`: only the host calls it, and it runs on every client.
  `Owner`: the host or the object's owner (the host by default).
  `Anyone`: any peer. The host may call every method.
- A client's call goes to the host, and a host's call goes to every
  client. The host checks each client call against its own owners, so a
  modified client cannot pass a forged one. Forward a call to the other
  clients yourself when they should see it.
- `Reliability::Unreliable` sends with `send_unreliable`, so its
  arguments must fit in 1200 bytes with the names.

## Replicated objects

`net::replicate` copies objects from the host to its clients. The host
marks objects with `Replicated` and lists the components to copy, by the
names `rusting schema` shows; `transform` and `name` cover the object's
transform and name:

```rust
use rusting_engine::net::replicate::{Replica, Replicated, Replication, NAME, TRANSFORM};

// Host, once:
let mut replication = Replication::new();
replication
    .component(TRANSFORM)
    .component(NAME)
    .component("rusting.health")
    .quantize("/transform/position", 0.01)  // centimetres
    .quantize("/transform/rotation", 0.001);

// Host, every tick (or a few times a second):
if let Some(delta) = replication.delta(world)? {
    session.broadcast(&delta)?;
}
// Host, on NetEvent::Connected(peer):
session.send(peer, &replication.full())?;

// Client, once, from the same Replication built the same way:
let mut replica = Replica::new(&replication);
// Client, for each NetEvent::Message { from, bytes }:
match replica.apply(world, from, &bytes) {
    Some(Ok(())) => {}
    Some(Err(reason)) => eprintln!("bad replication message: {reason}"),
    None => { /* a plain game message or an RPC */ }
}
```

- A delta holds only what changed since the last one: new objects whole,
  changed components, removed components and despawned objects. Nothing
  changed means no message.
- `quantize` rounds every number at a field path to a step on the host, so
  changes smaller than the step send nothing. A client's value is within
  half a step of the host's. Steps must be positive; setting a path again
  replaces its step, and `precision()` lists every path and step.
  Starting points:

  | Field | Step | Client error at most |
  |---|---|---|
  | `/transform/position` | 0.01 | 5 mm |
  | `/transform/rotation` | 0.001 | 0.0005 rad (0.03°) per axis |
  | `/transform/scale` | 0.01 | 0.005 |
  | `/rusting.health/value` | 1 | exact for whole numbers |

  Fields without a step travel at full `f64` precision.
- Objects are matched by their scene id; ones the client lacks are
  spawned (with a `Transform` only if the host sends one). To update an object both sides loaded from the same scene in
  place, mark it `Replicated` on the client too.
- A replica takes messages only from the host, only for the components
  its `Replication` lists, and only for objects marked `Replicated`, and
  tracks at most `MAX_OBJECTS` (16384). It refuses anything else whole,
  so a host cannot delete or rewrite the client's own objects (menus,
  cameras).
- Send deltas with `broadcast`, not `broadcast_unreliable`: each builds on
  the one before. Send `full()` to a client that joins later, before the
  next delta.
- A component that refers to other objects or to assets by handle is
  copied as saved in a scene; its references are not remapped on the
  client yet. The messages are JSON.

### Snapshots over unreliable messages

To send state every tick without waiting on lost packets, number each
capture and encode it, per client, against the last snapshot that client
acknowledged:

```rust
// Host, every tick:
replication.snapshot(world)?;
for peer in session.peers() {
    if let Some(bytes) = replication.snapshot_for(peer) {
        session.send_unreliable(peer, &bytes)?;
    }
}
// Host, for each message: acknowledgements.
if let Some(Err(reason)) = replication.accept(from, &bytes) {
    eprintln!("{reason}");
}
// Host, on NetEvent::Disconnected(peer):
replication.forget(peer);

// Client, after replica.apply(...) returns Some(Ok(())):
if let Some(ack) = replica.ack() {
    session.broadcast_unreliable(&ack)?;
}
```

- A snapshot holds only what changed since the acknowledged one, so a
  lost snapshot costs nothing but a bigger next one. A client with no
  acknowledgement, or one older than `SNAPSHOT_HISTORY` (64) snapshots,
  gets a whole snapshot.
- A snapshot that arrives after a newer one is dropped. One built on a
  snapshot the client never got is refused; the next one, built on the
  client's real acknowledgement, fixes it.
- Use snapshots or deltas for a set of objects, not both.

### Detecting and repairing a desync

A client's copy can drift from the host's: a bug, a local script that
moves a replicated object, a lost message. Compare state hashes now and
then, and resynchronize on a mismatch:

```rust
use rusting_engine::net::replicate::{Check, FromClient};

// Host, about once a second, after the delta or snapshot it goes with:
session.broadcast(&replication.checksum())?;
// Host, for each client message:
match replication.accept(from, &bytes) {
    Some(Ok(FromClient::Resync)) => session.send(from, &replication.full())?, // deltas only
    Some(Ok(FromClient::Ack)) | None => {}
    Some(Err(reason)) => eprintln!("{reason}"),
}

// Client, for each message from the host:
match replica.verify(world, from, &bytes) {
    Some(Ok(Check::Desync)) => {
        eprintln!("desync: resynchronizing");
        session.send(HOST, &replica.resync())?;
    }
    Some(Ok(Check::Match | Check::Stale)) | None => {}
    Some(Err(reason)) => eprintln!("{reason}"),
}
```

- The hash covers every replicated component, quantized the same way
  on both sides, of every object marked `Replicated`. An interpolated
  client hashes `NetTransform`, not the drawn `Transform`.
- With snapshots the hash names its snapshot; a client that has not
  applied that one answers `Stale` and waits for the next hash. With
  deltas, send the hash reliably right after the delta.
- Each checksum lets a client resynchronize once; `accept` refuses a
  second request before the next checksum, so a client cannot make the
  host send the whole state over and over.
- After a resync request the next snapshot for that client is whole.
  A whole snapshot, and `full()`, replace the client's replicated
  objects: changed values are set back, and replicated objects the host
  does not have are despawned.

### Commands instead of bodies

A deterministic simulation, such as physics bodies, does not need its
state sent at all: send what happened each tick and let every peer run
it. Mark those objects `net::replicate::Simulated` on every peer, and
send each tick's commands with `net::lockstep`:

```rust
use rusting_engine::net::lockstep::{self, CommandStream};
use rusting_engine::runtime::world_state_hash;

// Host, every tick, before running it:
lockstep::send(&session, tick, world_state_hash(world), &commands)?;
run_tick(world, &commands);

// Client: buffer the host's commands, then run every tick that arrived.
let mut stream = CommandStream::<MyCommand>::new(start_tick);
if let Some(Err(reason)) = stream.accept(from, &bytes) {
    eprintln!("{reason}");
}
while let Some(tick) = stream.ready() {
    if world_state_hash(world) != tick.hash {
        eprintln!("tick {}: simulation drifted", tick.tick);
    }
    run_tick(world, &tick.commands);
}
```

- A `Simulated` object's transform is never replicated or hashed by
  `Replication`; its other listed components still are. Spawn it on
  every peer, from the shared scene or by a command, since a replicated
  spawn carries no transform for it.
- Send every tick, in order, with no commands if nothing happened; a
  client waits for each tick before running it. Commands for a tick more
  than `input::MAX_AHEAD` ahead, repeated, or not from the host are
  refused.
- The hash is whatever the game computes of the state before the tick.
  `world_state_hash` fits when every peer built its world the same way.
  A mismatch means the simulation is not deterministic there: remove
  `Simulated` from those objects on every peer and their transforms are
  replicated again.

### Bandwidth per client

Cap what the host sends each client with `net::limit::Bandwidth`, a byte
budget per client that also measures what each one got:

```rust
use rusting_engine::net::limit::Bandwidth;

let mut budget = Bandwidth::new(16_000.0, 4_000); // 16 KB/s, bursts of 4 KB
// Host, every tick, instead of session.send_unreliable:
for peer in session.peers() {
    if let Some(bytes) = replication.snapshot_for(peer) {
        budget.send_unreliable(&session, peer, &bytes, Instant::now())?;
    }
}
let usage = budget.usage(peer); // bytes_sent, bytes_per_second, messages_dropped
// On NetEvent::Disconnected(peer):
budget.forget(peer);
```

- A message over the budget is not sent and counts in
  `messages_dropped`. Snapshots suit this: the next one, encoded against
  the client's last acknowledgement, carries what the dropped one had.
  Do not send deltas or other messages that build on each other through
  it.
- The burst must hold the largest message; a bigger one is an error.
- Budgets count payload bytes; `NetSession::stats` adds the headers.

## Smoothing remote objects

Updates arrive a few times a second, each late by a different amount.
`net::interpolate::Interpolation` shows every remote object where it was
a fixed delay ago, blending between the two updates around that time:

```rust
use rusting_engine::net::interpolate::Interpolation;
use std::time::Duration;

let mut replica = Replica::new(&replication).interpolated();
let mut smooth = Interpolation::new(Duration::from_millis(100));

// After each replica.apply(...) that returns Some(Ok(())):
smooth.record(world, host_time); // e.g. clock.host_tick(tick) * step
// Every frame, before drawing:
smooth.present(world, client_time);
```

- An `interpolated()` replica writes the host's transform into
  `NetTransform`; `present` sets `Transform`, which is what the client
  draws. Positions and scales blend linearly; rotations take the short
  way around.
- The delay is the trade: longer hides more jitter and loss, shorter
  shows the host's state sooner. A bit more than two update intervals
  hides one lost update (100 ms at 20 Hz). Change it with `set_delay`.
- Past the newest update an object holds still rather than guessing
  ahead. Use the same clock for `record` and `present`.
- `record` skips the object marked `Predicted` (the local hero), so
  `present` never moves it; the game sets its `Transform` from the
  prediction.

A game that sends its own snapshots, with no replica, keeps one
`net::interpolate::Track` per remote object and pushes plain
`(tick, position)` samples into it:

```rust
use rusting_engine::net::interpolate::Track;
use std::collections::BTreeMap;

let mut tracks: BTreeMap<PeerId, Track> = BTreeMap::new();
// For each snapshot received:
tracks.entry(peer).or_default().push(tick, position);
// Every frame, draw each peer two and a half ticks behind its newest:
for track in tracks.values() {
    let tick = track.newest().unwrap() as f64 - 2.5;
    let position = track.at(tick).unwrap();
}
```

Samples may arrive in any order; a repeated tick replaces the old one, and
a track keeps the newest 64 ticks. `at` takes a fractional tick, holds the
oldest sample before it and the newest after it.

## Predicting the player's own object

Waiting a round trip for the host makes a player's own raft feel slow.
`net::predict::Prediction` lets the client move it at once and fix it
when the host disagrees:

```rust
use rusting_engine::net::predict::Prediction;

// Shared by host and client: one tick of input.
fn steer(position: &mut [f32; 3], input: &Steer) { /* ... */ }

// Client, each tick:
prediction.push(tick, input);
steer(&mut position, &input);
session.send_unreliable(HOST, &encode(tick, &input))?;

// Host: apply each input with `steer`, then send back the state and the
// tick of the last input it applied.

// Client, on the host's reply:
position = prediction.reconcile(acked_tick, host_position, steer);
```

- `reconcile` forgets inputs up to `acked_tick` and replays the rest on
  the host's state, so a right guess changes nothing.
- `steer` must be the same code on both sides, with the same fixed step.
- It keeps at most `MAX_PENDING` (240) inputs. Inputs over UDP can be
  lost: send the last few ticks' inputs in each message.
- A correction would snap. `net::predict::Smoothing` hides it in the
  drawing only:

  ```rust
  use rusting_engine::net::predict::Smoothing;

  let mut smoothing = Smoothing::new(Duration::from_millis(100), 2.0);
  // On the host's reply. `before` is where the prediction is drawn now,
  // not last frame's `drawn`, which is a step behind while moving.
  let before = smoothing.shown(position, Duration::ZERO);
  position = prediction.reconcile(acked_tick, host_position, steer);
  smoothing.correct(before, position);
  // Every frame:
  drawn = smoothing.shown(position, frame_time);
  transform.position = drawn;
  ```

  The simulation keeps the reconciled `position`; only the drawn one
  lags, halving its error every half-life (100 ms here). A correction
  longer than the snap distance (2 m here: a teleport or respawn) shows
  at once. Never feed `drawn` back into `steer` or the host.
- A walking player needs no hand-written `steer`.
  `PlayerController::step` runs one tick of the same movement as
  `rusting.player_controller` from a `PlayerInput` instead of the local
  keyboard. The host steps each peer's body with that peer's input, and
  the client replays its pending inputs through it:

  ```rust
  use rusting_engine::{PlayerController, PlayerInput};

  let input = PlayerInput { forward: 1.0, jump: pressed, jump_held: held, ..Default::default() };
  let shove = controller.step(physics, body, shape, &mut position, input, None, |_| None, dt);
  ```

  Set `controller.yaw` from the input first. `water` is
  `Some((surface_y, current))` when water lies over the body, and
  `floor_pose` returns the position and rotation of a floor entity so
  a moving platform carries the player. Keep the `PlayerController` itself in the predicted state:
  it holds the velocity, the jump buffer and the dash cooldown.
- Predict only the local hero. Mark it with
  `net::predict::predict_only(world, hero)`, which removes `Predicted`
  from any other object, and use an `interpolated()` replica so the
  host's updates land in its `NetTransform`, not its `Transform`. Show
  everything else from host state. Never predict terrain destruction or
  other host-simulated results: that needs a simulation proven to match
  the host's exactly, which the engine does not promise across machines
  yet.

## Testing a bad connection

`simulate` makes one end act as if its connection were slow or lossy, so a
game can be tried against lag on one machine:

```rust
use rusting_engine::net::NetConditions;
use std::time::Duration;

session.simulate(NetConditions {
    latency: Duration::from_millis(75), // each way: set it on both ends
    jitter: Duration::from_millis(20),
    loss: 0.02,
    seed: 1,
});
```

- It delays what `poll` returns on this end. Set it on the host and on a
  client for a 150 ms round trip.
- Messages stay reliable and in order. A lost message is resent after one
  more round trip (at least 200 ms), and the messages behind it wait too,
  as on real TCP.
- The same seed gives the same delays. `NetConditions::default()` turns
  it off.
- In a `"realtime": true` scenario the delays count from each tick's
  planned time, not the wall clock, so a slow `capture` frame does not
  change the tick a message arrives on. A copy of the scenario without
  its captures sees the same arrivals.

`stats()` returns `NetStats`: messages and wire bytes sent and received
(payload plus a 9-byte frame header each), and `held`, the events the
simulation is holding back. Show it on a debug HUD to see what a game
sends per second.

## Unreliable messages

`send_unreliable(peer, bytes)` and `broadcast_unreliable(bytes)` send over
UDP beside the TCP connection. A lost one is gone instead of holding up
the messages behind it, so use them for state sent every tick, such as
positions. They arrive as the same `NetEvent::Message`.

- A message may be lost, duplicated or arrive out of order. Send whole
  state, not changes, and drop old ones (put a tick number in each).
- At most `MAX_UNRELIABLE` (1200) bytes, so one fits in a packet.
- The host takes datagrams for a client only from that client's TCP IP
  address. A client whose UDP leaves from another public address (rare,
  some carrier NATs) gets no unreliable messages through.
- The host listens for UDP on its TCP port: open both in the firewall.
- They go reliably where there is no UDP route: through a relay, in a
  loopback session, to a client whose first datagram has not reached
  the host yet, and from a host whose UDP port was already taken.
- `simulate` drops them at the loss rate instead of resending them.

## Tests without sockets

`NetSession::loopback(clients)` returns a host and that many clients joined
to it inside one process, with no ports or threads. They take the same
calls as real sessions: the host's first `poll` reports each client as
`Connected`, a dropped client is `Disconnected`, and a dropped host sends
`Closed` to every client. Unit-test game networking code with them.

```rust
use rusting_engine::net::{NetEvent, NetSession, HOST};

let (host, clients) = NetSession::loopback(2);
clients[0].send(HOST, b"ready")?;
for event in host.poll() {
    if let NetEvent::Message { from, bytes } = event {
        host.send(from, &bytes)?; // echo back
    }
}
```

## HTTP requests

`net::http` talks to web services and downloads files over plain HTTP/1.1,
using only the standard library.

- `http::get(url)`, `http::post(url, content_type, body)` and
  `http::request(method, url, headers, body)` block until the whole
  response is in and return its `status`, `headers` (names in lower case)
  and `body`. Any status is a response, so check `status`. Bodies over
  `MAX_BODY` (64 MiB) fail.
- `http::download(url, path)` streams the body into a file. It writes
  `<path>.part` and renames it only when the body is whole, so a failed
  download never leaves a cut-off file. Any status but 2xx fails.
- `http::fetch(method, url, headers, body)` runs `request` on its own
  thread. Poll the returned `PendingHttp` once a frame, so a slow server
  never stalls the game:

```rust
use rusting_engine::net::http;

let mut scores = http::fetch("GET", "http://scores.example.com/top", &[], &[]);
// Each frame:
if let Some(result) = scores.poll() {
    let top = result?.text();
}
```

Redirects to another `http://` URL are followed, up to five. Headers you
pass, such as an `Authorization` token, go only to the first URL's host and
are left off once a redirect points anywhere else. A URL with a space or a
control character fails; %-encode them. A connect or a wait for the next
bytes gives up after 30 seconds.

## Limits

- Reliable messages ride TCP, so a lost packet delays the messages behind
  it. Send per-tick state with `send_unreliable` instead.
- No rollback is built in. Replicated objects other than the player's
  own predicted one show the host's state one trip late.
- No encryption. The password and relay token travel in plain text, so
  they keep strangers out but do not hide traffic from someone on the
  path. Do not send other secrets.
- The relay limits each IP address to 16 open connections and 60 new
  connections a minute (failed ones count, which slows token guessing),
  caps everyone together at 1024 connections, and cuts off a connection
  that sends more than 1 MiB/s (one second's worth may come in a burst).
  It holds at most 256 rooms and 64 clients in a room, lets one address
  open or join 30 rooms a minute (refused attempts count, which slows room
  code guessing), and closes a connection that sends nothing for 10
  minutes after its handshake. Each connection has its own send queue of
  1 MiB; a peer that stops reading is cut off when its queue fills, and
  never slows anyone else. A host's queue is split into equal
  shares, one per client slot; a client that sends faster than its host
  reads is cut off when its share fills, and the host and other clients
  stay. A message larger than one second's rate is
  refused, so raise `bytes_per_second` to relay bigger ones. Still use a long random token. To
  change the limits, run the relay from Rust with
  `net::run_relay_with(listener, token, RelayLimits { .. })`.
- One player who connects and then sends nothing does not hold up anyone
  else's join; each handshake runs on its own thread and must finish
  within 5 seconds. The relay counts an IPv6 /64 as one address.
- `net::http` has no TLS, so `https://` URLs fail with
  `ErrorKind::Unsupported`. Most public services need HTTPS; reach them
  through a proxy you run, or wait for TLS support.
- Passwords and relay tokens must fit in a 64 KiB handshake.
- The wire protocol has a version (`PROTOCOL_VERSION`); a host or relay
  refuses clients built with another one.
