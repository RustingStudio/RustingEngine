//! Commands by tick, sent by the host so every peer runs the same
//! simulation instead of receiving its result.
//!
//! For deterministic objects, such as physics bodies marked
//! [`Simulated`](super::replicate::Simulated), the host sends what happened
//! each tick (inputs, impulses, spawns, events) rather than every body's
//! state. The host runs tick `t` with its commands and calls [`send`];
//! each client feeds messages to [`CommandStream::accept`] and runs a tick
//! only once [`CommandStream::ready`] hands it that tick's commands, so it
//! steps the same ticks with the same commands in the same order.
//!
//! Each tick carries a hash the game computes of the state the commands
//! apply to (before the tick), so a client can tell when its simulation
//! drifted.

use super::input::MAX_AHEAD;
use super::{NetSession, PeerId, HOST};
use serde::de::DeserializeOwned;
use serde::Serialize;
use std::collections::BTreeMap;
use std::io;

/// Marks a command message, so game messages pass by.
const MAGIC: &[u8; 4] = b"\0cmd";

/// One tick's commands, as [`CommandStream::ready`] returns them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TickCommands<C> {
    pub tick: u64,
    /// The host's hash of the state before this tick. Compare it with
    /// the same hash of this world before running the tick.
    pub hash: u64,
    pub commands: Vec<C>,
}

/// Sends tick `tick`'s commands and the hash of the state before it to
/// every client, reliably. Call it on the host once per tick, in tick
/// order, including ticks with no commands.
pub fn send<C: Serialize>(
    session: &NetSession,
    tick: u64,
    hash: u64,
    commands: &[C],
) -> io::Result<()> {
    let bytes = [
        MAGIC.as_slice(),
        &tick.to_le_bytes(),
        &hash.to_le_bytes(),
        &serde_json::to_vec(commands)?,
    ]
    .concat();
    session.broadcast(&bytes)
}

/// A client's buffer of the host's commands by tick.
#[derive(Clone, Debug)]
pub struct CommandStream<C> {
    waiting: BTreeMap<u64, (u64, Vec<C>)>,
    /// The next tick [`CommandStream::ready`] serves.
    next: u64,
}

impl<C: DeserializeOwned> CommandStream<C> {
    /// A stream whose first tick is `tick`: the host's tick when this
    /// client got the state it starts from.
    pub fn new(tick: u64) -> Self {
        Self {
            waiting: BTreeMap::new(),
            next: tick,
        }
    }

    /// Reads a message `from` a peer. Returns `None` for one that is not
    /// commands, and an error for one not from the host, for a tick
    /// already received or more than [`MAX_AHEAD`] ticks out, or that does
    /// not parse.
    pub fn accept(
        &mut self,
        from: PeerId,
        bytes: &[u8],
    ) -> Option<Result<(), String>> {
        let body = bytes.strip_prefix(MAGIC)?;
        if from != HOST {
            return Some(Err(format!("peer {from} sent commands")));
        }
        let Some((head, json)) = body.split_at_checked(16) else {
            return Some(Err("short commands".into()));
        };
        let read = |b: &[u8]| u64::from_le_bytes(b.try_into().unwrap());
        let (tick, hash) = (read(&head[..8]), read(&head[8..]));
        if tick < self.next || self.waiting.contains_key(&tick) {
            return Some(Err(format!("commands for tick {tick} came twice")));
        }
        if tick - self.next > MAX_AHEAD {
            return Some(Err(format!(
                "commands for tick {tick} are too early"
            )));
        }
        Some(
            serde_json::from_slice(json)
                .map(|commands| {
                    self.waiting.insert(tick, (hash, commands));
                })
                .map_err(|error| format!("bad commands: {error}")),
        )
    }

    /// The commands for the next tick, or `None` while they have not
    /// arrived: then do not run the tick yet.
    pub fn ready(&mut self) -> Option<TickCommands<C>> {
        let (hash, commands) = self.waiting.remove(&self.next)?;
        self.next += 1;
        Some(TickCommands {
            tick: self.next - 1,
            hash,
            commands,
        })
    }

    /// The tick [`ready`](Self::ready) serves.
    pub fn tick(&self) -> u64 {
        self.next
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::replicate::{Replication, Simulated, TRANSFORM};
    use crate::net::NetEvent;
    use crate::runtime::{SceneComponentRegistry, SceneId};
    use crate::Transform;
    use bevy_ecs::prelude::*;
    use serde::Deserialize;

    #[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
    struct Push(f32);

    fn step(world: &mut World, commands: &[Push]) {
        for Push(dx) in commands {
            for mut transform in world.query::<&mut Transform>().iter_mut(world)
            {
                transform.position[0] += dx;
            }
        }
    }

    fn state(world: &mut World) -> u64 {
        let mut query = world.query::<&Transform>();
        query
            .iter(world)
            .map(|t| u64::from(t.position[0].to_bits()))
            .sum()
    }

    #[test]
    fn clients_run_the_hosts_commands_instead_of_receiving_bodies() {
        let (host, clients) = NetSession::loopback(1);
        host.poll();
        let id = SceneId::new();
        let mut worlds = [World::new(), World::new()];
        for world in &mut worlds {
            world.insert_resource(SceneComponentRegistry::default());
            world.spawn((
                crate::net::replicate::Replicated,
                Simulated,
                id,
                Transform::default(),
            ));
        }
        let mut replication = Replication::new();
        replication.component(TRANSFORM);
        let first = replication.delta(&mut worlds[0]).unwrap();

        // The host steps and sends each tick; the body never travels.
        let ticks = [vec![Push(1.0)], vec![], vec![Push(2.5), Push(-0.5)]];
        for (tick, commands) in (5..).zip(&ticks) {
            send(&host, tick, state(&mut worlds[0]), commands).unwrap();
            step(&mut worlds[0], commands);
        }
        assert!(replication.delta(&mut worlds[0]).unwrap().is_none());

        // The client steps each tick once it has it, and agrees.
        let mut stream = CommandStream::<Push>::new(5);
        assert_eq!(stream.ready(), None);
        for event in clients[0].poll() {
            let NetEvent::Message { from, bytes } = event else {
                panic!("expected a message")
            };
            assert_eq!(stream.accept(from, &bytes), Some(Ok(())));
        }
        while let Some(tick) = stream.ready() {
            assert_eq!(tick.hash, state(&mut worlds[1]));
            step(&mut worlds[1], &tick.commands);
        }
        assert_eq!(stream.tick(), 8);
        assert_eq!(state(&mut worlds[0]), state(&mut worlds[1]));
        assert!(first.is_some_and(
            |bytes| !String::from_utf8_lossy(&bytes).contains(TRANSFORM)
        ));

        // Repeats, far-future ticks, clients and junk are refused.
        let message = |tick: u64| {
            [MAGIC.as_slice(), &tick.to_le_bytes(), &[0; 8], b"[]"].concat()
        };
        assert!(matches!(stream.accept(HOST, &message(7)), Some(Err(_))));
        assert!(matches!(
            stream.accept(HOST, &message(8 + MAX_AHEAD + 1)),
            Some(Err(_))
        ));
        assert!(matches!(stream.accept(1, &message(8)), Some(Err(_))));
        assert!(matches!(stream.accept(HOST, b"\0cmd\x01"), Some(Err(_))));
        assert_eq!(stream.accept(HOST, b"game bytes"), None);
        assert_eq!(stream.accept(HOST, &message(8)), Some(Ok(())));
        assert_eq!(stream.ready().map(|tick| tick.tick), Some(8));
    }
}
