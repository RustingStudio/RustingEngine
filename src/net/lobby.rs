//! Player lifecycle on the host: join, ready, in match, dropped, and
//! reconnect.
//!
//! A [`PlayerId`] outlives the connection: a client that loses its
//! connection gets a fresh [`PeerId`] when it comes back, but keeps its
//! player, and its place in the match, by sending the reconnect token the
//! host gave it on joining.
//!
//! A spectator says [`spectate`] instead of [`hello`]: it gets no player,
//! receives state like everyone else, and the host takes no input from it
//! ([`Lobby::accepts_input`]).

use super::{NetEvent, NetSession, PeerId, HOST};
use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::time::{Duration, Instant};

/// Marks a lobby message, so game messages on the same session pass by.
const MAGIC: &[u8; 4] = b"\0lob";
const HELLO: u8 = 0;
const READY: u8 = 1;
const WELCOME: u8 = 2;
const SPECTATE: u8 = 3;

/// A player, stable across reconnects. Numbered from 1 in join order.
pub type PlayerId = u32;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    /// Connected, not ready yet.
    Joined,
    /// Ready for the match to start.
    Ready,
    /// Playing.
    InMatch,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LobbyEvent {
    /// A new player joined.
    Joined(PlayerId),
    /// A dropped player came back, at the stage it had.
    Rejoined(PlayerId),
    Ready(PlayerId),
    /// A player's connection closed. It can rejoin until the grace time
    /// runs out.
    Dropped(PlayerId),
    /// A dropped player's grace time ran out; it is gone.
    Left(PlayerId),
    /// A peer joined to watch.
    Spectating(PeerId),
    /// A spectator's connection closed.
    SpectatorLeft(PeerId),
}

#[derive(Clone, Debug)]
struct Player {
    /// `None` while dropped.
    peer: Option<PeerId>,
    token: u64,
    stage: Stage,
    dropped_at: Option<Instant>,
}

/// The host's record of players. Feed it every [`NetEvent`].
#[derive(Clone, Debug)]
pub struct Lobby {
    players: BTreeMap<PlayerId, Player>,
    spectators: BTreeSet<PeerId>,
    next: PlayerId,
    grace: Duration,
}

impl Lobby {
    /// A lobby that keeps a dropped player's place for `grace`.
    pub fn new(grace: Duration) -> Self {
        Self {
            players: BTreeMap::new(),
            spectators: BTreeSet::new(),
            next: 1,
            grace,
        }
    }

    /// Reads one session event on the host. Returns `None` for an event
    /// the lobby has no part in (game messages, peers that never said
    /// hello), and an error for a bad lobby message.
    pub fn handle(
        &mut self,
        session: &NetSession,
        event: &NetEvent,
    ) -> Option<Result<LobbyEvent, String>> {
        match event {
            NetEvent::Disconnected(peer) => {
                if self.spectators.remove(peer) {
                    return Some(Ok(LobbyEvent::SpectatorLeft(*peer)));
                }
                let (&id, player) = self
                    .players
                    .iter_mut()
                    .find(|(_, player)| player.peer == Some(*peer))?;
                player.peer = None;
                player.dropped_at = Some(Instant::now());
                Some(Ok(LobbyEvent::Dropped(id)))
            }
            NetEvent::Message { from, bytes } => {
                let body = bytes.strip_prefix(MAGIC)?;
                Some(self.message(session, *from, body))
            }
            _ => None,
        }
    }

    fn message(
        &mut self,
        session: &NetSession,
        from: PeerId,
        body: &[u8],
    ) -> Result<LobbyEvent, String> {
        if self.spectators.contains(&from) {
            return Err(format!("peer {from}: spectators only watch"));
        }
        let current = self.player(from);
        match (body, current) {
            ([SPECTATE], None) => {
                self.spectators.insert(from);
                Ok(LobbyEvent::Spectating(from))
            }
            ([READY], Some(id)) => {
                let player = self.players.get_mut(&id).expect("listed");
                if player.stage == Stage::Joined {
                    player.stage = Stage::Ready;
                }
                Ok(LobbyEvent::Ready(id))
            }
            ([HELLO], None) => {
                let id = self.next;
                self.next += 1;
                let token = super::token();
                self.players.insert(
                    id,
                    Player {
                        peer: Some(from),
                        token,
                        stage: Stage::Joined,
                        dropped_at: None,
                    },
                );
                welcome(session, from, id, token)?;
                Ok(LobbyEvent::Joined(id))
            }
            ([HELLO, rest @ ..], None) if rest.len() == 12 => {
                let id = u32::from_le_bytes(rest[..4].try_into().unwrap());
                let token = u64::from_le_bytes(rest[4..].try_into().unwrap());
                // Only a dropped player can be taken over, so a leaked
                // token cannot steal a connected one.
                let player = self
                    .players
                    .get_mut(&id)
                    .filter(|player| player.peer.is_none())
                    .filter(|player| player.token == token)
                    .ok_or_else(|| format!("peer {from}: no such player"))?;
                player.peer = Some(from);
                player.dropped_at = None;
                player.token = super::token();
                let token = player.token;
                welcome(session, from, id, token)?;
                Ok(LobbyEvent::Rejoined(id))
            }
            _ => Err(format!("peer {from}: unexpected lobby message")),
        }
    }

    /// Moves every connected ready player into the match and returns them.
    pub fn start_match(&mut self) -> Vec<PlayerId> {
        self.players
            .iter_mut()
            .filter(|(_, player)| {
                player.peer.is_some() && player.stage == Stage::Ready
            })
            .map(|(&id, player)| {
                player.stage = Stage::InMatch;
                id
            })
            .collect()
    }

    /// Removes dropped players whose grace time has run out by `now`.
    /// Call it once a second or so.
    pub fn expire(&mut self, now: Instant) -> Vec<LobbyEvent> {
        let grace = self.grace;
        let gone: Vec<PlayerId> = self
            .players
            .iter()
            .filter(|(_, player)| {
                player.dropped_at.is_some_and(|at| {
                    now.saturating_duration_since(at) >= grace
                })
            })
            .map(|(&id, _)| id)
            .collect();
        for id in &gone {
            self.players.remove(id);
        }
        gone.into_iter().map(LobbyEvent::Left).collect()
    }

    /// The player on `peer`, if it said hello.
    pub fn player(&self, peer: PeerId) -> Option<PlayerId> {
        self.players
            .iter()
            .find(|(_, player)| player.peer == Some(peer))
            .map(|(&id, _)| id)
    }

    /// The connection of `player`, or `None` while it is dropped.
    pub fn peer(&self, player: PlayerId) -> Option<PeerId> {
        self.players.get(&player)?.peer
    }

    pub fn stage(&self, player: PlayerId) -> Option<Stage> {
        Some(self.players.get(&player)?.stage)
    }

    /// Whether to take game input from `peer`: only from a player in the
    /// match, never from a spectator or a peer that has not said hello.
    pub fn accepts_input(&self, peer: PeerId) -> bool {
        self.player(peer)
            .is_some_and(|id| self.stage(id) == Some(Stage::InMatch))
    }

    /// Connected spectators, in peer order.
    pub fn spectators(&self) -> impl Iterator<Item = PeerId> + '_ {
        self.spectators.iter().copied()
    }

    /// Every player, dropped ones included, in id order.
    pub fn players(&self) -> impl Iterator<Item = PlayerId> + '_ {
        self.players.keys().copied()
    }
}

fn welcome(
    session: &NetSession,
    to: PeerId,
    id: PlayerId,
    token: u64,
) -> Result<(), String> {
    let mut bytes = MAGIC.to_vec();
    bytes.push(WELCOME);
    bytes.extend(id.to_le_bytes());
    bytes.extend(token.to_le_bytes());
    session.send(to, &bytes).map_err(|error| error.to_string())
}

/// What a client keeps to rejoin as the same player: from [`welcome_from`],
/// to pass to [`hello`] after reconnecting.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rejoin {
    pub player: PlayerId,
    pub token: u64,
}

/// Client: introduces this client to the host, as a new player or, with
/// a [`Rejoin`], as the one it was before losing the connection.
pub fn hello(session: &NetSession, rejoin: Option<Rejoin>) -> io::Result<()> {
    let mut bytes = MAGIC.to_vec();
    bytes.push(HELLO);
    if let Some(rejoin) = rejoin {
        bytes.extend(rejoin.player.to_le_bytes());
        bytes.extend(rejoin.token.to_le_bytes());
    }
    session.send(HOST, &bytes)
}

/// Client: joins to watch, instead of [`hello`]. The host sends state
/// and takes no input from this client.
pub fn spectate(session: &NetSession) -> io::Result<()> {
    session.send(HOST, &[MAGIC.as_slice(), &[SPECTATE]].concat())
}

/// Client: tells the host this player is ready.
pub fn ready(session: &NetSession) -> io::Result<()> {
    let mut bytes = MAGIC.to_vec();
    bytes.push(READY);
    session.send(HOST, &bytes)
}

/// Client: reads the host's answer to [`hello`]. Keep it to rejoin; each
/// rejoin hands out a new token.
pub fn welcome_from(from: PeerId, bytes: &[u8]) -> Option<Rejoin> {
    let body = bytes.strip_prefix(MAGIC)?.strip_prefix(&[WELCOME])?;
    (from == HOST && body.len() == 12).then(|| Rejoin {
        player: u32::from_le_bytes(body[..4].try_into().unwrap()),
        token: u64::from_le_bytes(body[4..].try_into().unwrap()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn welcomed(client: &NetSession) -> Rejoin {
        match client.poll().remove(0) {
            NetEvent::Message { from, bytes } => {
                welcome_from(from, &bytes).unwrap()
            }
            other => panic!("expected a welcome, got {other:?}"),
        }
    }

    #[test]
    fn players_join_ready_play_drop_and_rejoin() {
        let (host, mut clients) = NetSession::loopback(3);
        let mut lobby = Lobby::new(Duration::from_secs(30));
        let step = |lobby: &mut Lobby| -> Vec<_> {
            host.poll()
                .iter()
                .filter_map(|event| lobby.handle(&host, event))
                .collect()
        };
        assert!(step(&mut lobby).is_empty()); // Connected: no hello yet

        hello(&clients[0], None).unwrap();
        hello(&clients[1], None).unwrap();
        assert_eq!(
            step(&mut lobby),
            [Ok(LobbyEvent::Joined(1)), Ok(LobbyEvent::Joined(2))]
        );
        let rejoin = welcomed(&clients[0]);
        assert_eq!(rejoin.player, 1);
        ready(&clients[0]).unwrap();
        assert_eq!(step(&mut lobby), [Ok(LobbyEvent::Ready(1))]);
        assert_eq!(lobby.start_match(), [1]);
        assert_eq!(lobby.stage(2), Some(Stage::Joined));

        // A second hello, a forged rejoin and junk are refused.
        hello(&clients[1], None).unwrap();
        hello(
            &clients[2],
            Some(Rejoin {
                player: 1,
                token: 0,
            }),
        )
        .unwrap();
        hello(
            &clients[2],
            Some(Rejoin {
                player: 1,
                ..rejoin
            }),
        )
        .unwrap();
        let refused = step(&mut lobby);
        assert_eq!(refused.len(), 3);
        assert!(refused.iter().all(Result::is_err), "{refused:?}");

        // Player 1 drops and comes back on another connection.
        drop(clients.remove(0));
        assert_eq!(step(&mut lobby), [Ok(LobbyEvent::Dropped(1))]);
        assert_eq!(lobby.peer(1), None);
        hello(&clients[1], Some(rejoin)).unwrap();
        assert_eq!(step(&mut lobby), [Ok(LobbyEvent::Rejoined(1))]);
        assert_eq!(lobby.peer(1), Some(3));
        assert_eq!(lobby.stage(1), Some(Stage::InMatch));
        let next = welcomed(&clients[1]);
        assert_eq!(next.player, 1);
        assert_ne!(next.token, rejoin.token);

        // A dropped player past its grace time is gone.
        drop(clients.remove(0));
        assert_eq!(step(&mut lobby), [Ok(LobbyEvent::Dropped(2))]);
        assert!(lobby.expire(Instant::now()).is_empty());
        let later = Instant::now() + Duration::from_secs(31);
        assert_eq!(lobby.expire(later), [LobbyEvent::Left(2)]);
        assert_eq!(lobby.players().collect::<Vec<_>>(), [1]);
        assert_eq!(welcome_from(2, b"\0lob\x02"), None);
    }

    #[test]
    fn spectators_watch_without_a_player_or_input() {
        let (host, clients) = NetSession::loopback(2);
        host.poll();
        let mut lobby = Lobby::new(Duration::from_secs(30));
        let handle = |lobby: &mut Lobby| {
            host.poll()
                .iter()
                .filter_map(|event| lobby.handle(&host, event))
                .collect::<Vec<_>>()
        };
        hello(&clients[0], None).unwrap();
        spectate(&clients[1]).unwrap();
        assert_eq!(
            handle(&mut lobby),
            [Ok(LobbyEvent::Joined(1)), Ok(LobbyEvent::Spectating(2))]
        );
        ready(&clients[0]).unwrap();
        ready(&clients[1]).unwrap();
        hello(&clients[1], None).unwrap();
        let events = handle(&mut lobby);
        assert_eq!(events[0], Ok(LobbyEvent::Ready(1)));
        assert!(events[1..].iter().all(Result::is_err), "{events:?}");
        assert_eq!(lobby.start_match(), [1]);
        assert!(lobby.accepts_input(1));
        assert!(!lobby.accepts_input(2), "a spectator sends no input");
        assert_eq!(lobby.player(2), None);
        assert_eq!(lobby.spectators().collect::<Vec<_>>(), [2]);
        // Spectators still receive what the host broadcasts.
        host.broadcast(b"state").unwrap();
        assert!(matches!(
            clients[1].poll().last(),
            Some(NetEvent::Message { bytes, .. }) if bytes == b"state"
        ));
        assert_eq!(
            lobby.handle(&host, &NetEvent::Disconnected(2)),
            Some(Ok(LobbyEvent::SpectatorLeft(2)))
        );
        assert_eq!(lobby.spectators().count(), 0);
    }
}
