# Networking

`rusting_engine::net` sends reliable, ordered messages between one host and
its clients over TCP, and unreliable ones over UDP. It uses only the
standard library. Every client talks
to the host, and the host forwards what other clients need (a star).

- The host has id `HOST` (0). Clients get ids from 1 in join order.
- A message is a byte string. It arrives whole and in order. Put JSON
  (`serde_json::to_vec`) or bincode in it.
- `poll()` never blocks. Call it once per frame or tick.

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

## Listen servers and dedicated servers

A listen server is a player's game that also hosts: call
`NetSession::host` in it, and the host plays like any client. A dedicated
server runs the same game with no window or player:

```sh
rusting run my_game --server            # from the project
RUSTING_SERVER=1 ./my_game              # an exported game
```

- It ticks once per fixed step of real time, renders nothing, and runs
  until game code calls `scene.quit()` (or `--timeout` stops it).
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
  `net::replicate` only moves state from the host to clients.
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
  spawned. To update an object both sides loaded from the same scene in
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
- A correction snaps. Ease the drawn position toward the result for a
  few frames if it shows.

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
  caps everyone together at 1024 connections, and reads at most 1 MiB/s
  from each connection. Still use a long random token. To change the
  limits, run the relay from Rust with `net::run_relay_with(listener,
  token, RelayLimits { .. })`.
- One player who connects and then sends nothing does not hold up anyone
  else's join; each handshake runs on its own thread and must finish
  within 5 seconds. The relay counts an IPv6 /64 as one address.
- Passwords and relay tokens must fit in a 64 KiB handshake.
- The wire protocol has a version (`PROTOCOL_VERSION`); a host or relay
  refuses clients built with another one.
