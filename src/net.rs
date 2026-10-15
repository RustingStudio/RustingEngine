//! Reliable multiplayer messages over TCP: a listen-server host, clients
//! that join it by address, and a relay that pairs hosts and clients by a
//! short room code so neither side needs an open port.
//!
//! Every peer talks only to the host (a star). The host has [`HOST`] as its
//! id; clients get ids from 1 in join order. Messages are byte strings that
//! arrive whole and in order; put JSON or bincode in them.
//! [`NetSession::send_unreliable`] sends per-tick state over UDP instead.
//!
//! ```no_run
//! use rusting_engine::net::{NetEvent, NetSession, HOST};
//! let host = NetSession::host(7777, "")?;
//! let client = NetSession::join("127.0.0.1:7777", "")?;
//! client.send(HOST, b"hello")?;
//! for event in host.poll() {
//!     if let NetEvent::Message { from, bytes } = event {
//!         host.send(from, &bytes)?;
//!     }
//! }
//! # Ok::<(), std::io::Error>(())
//! ```
//!
//! Store the session as a resource from game code with
//! `scene.world().insert_resource(session)` and read it back each frame
//! with `scene.world().get_resource::<NetSession>()`.
//!
//! A host can require a password and a relay a token; an empty string
//! means none. Both travel in plain text, so they keep strangers out of a
//! game but do not hide traffic from someone on the path.

use bevy_ecs::prelude::Resource;
use std::cell::Cell;
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::io::{self, Read, Write};
use std::net::{
    IpAddr, Ipv6Addr, Shutdown, SocketAddr, TcpListener, TcpStream,
    ToSocketAddrs, UdpSocket,
};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub mod clock;
pub mod http;
pub mod input;
pub mod interpolate;
pub mod limit;
pub mod lobby;
pub mod lockstep;
pub mod predict;
pub mod replicate;
pub mod rpc;

/// A peer in a session. The host is [`HOST`]; clients count up from 1.
pub type PeerId = u32;
/// The host's id.
pub const HOST: PeerId = 0;
/// Wire protocol version. A host or relay refuses other versions.
pub const PROTOCOL_VERSION: u16 = 4;
/// Largest message, in bytes. Larger frames close the connection.
pub const MAX_MESSAGE: usize = 16 << 20;
/// Largest unreliable message, in bytes, so a datagram fits one packet.
pub const MAX_UNRELIABLE: usize = 1200;

const EVERYONE: PeerId = u32::MAX;
// Frame kinds.
const DATA: u8 = 0;
const JOINED: u8 = 1;
const LEFT: u8 = 2;
const HOST_ROOM: u8 = 3;
const ROOM: u8 = 4;
const JOIN: u8 = 5;
const WELCOME: u8 = 6;
const REJECT: u8 = 7;
/// Host to relay: the room's world seed, a u64, for later WELCOMEs.
const SEED: u8 = 8;
/// Longest a whole handshake may take, however slowly its bytes arrive.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
/// Largest handshake frame. A JOIN holds a version, a room code and a
/// password or token, so a bigger one is not a real join.
const MAX_HANDSHAKE: usize = 64 << 10;
/// Joins a direct host checks at once; more connections are dropped.
const MAX_PENDING_JOINS: usize = 64;

/// Something that happened since the last [`NetSession::poll`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NetEvent {
    /// A client joined (host only).
    Connected(PeerId),
    /// A client left (host only).
    Disconnected(PeerId),
    Message {
        from: PeerId,
        bytes: Vec<u8>,
    },
    /// The host or relay connection closed; the session is over.
    Closed,
}

/// Network conditions a session pretends to have, for testing a game
/// against a bad connection on one machine. Set with
/// [`NetSession::simulate`]; the default is a perfect connection.
///
/// They act on what this end receives: each incoming event waits
/// `latency` plus up to `jitter` before [`NetSession::poll`] returns it.
/// Set them on both ends for a round trip of twice `latency`. Messages
/// stay reliable and in order, as on TCP: a message `loss` picks waits one
/// more round trip (at least 200 ms, Linux's shortest retransmit), and
/// the messages behind it wait with it.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct NetConditions {
    /// One-way delay added to every incoming event.
    pub latency: Duration,
    /// Extra random delay, up to this much.
    pub jitter: Duration,
    /// Share of messages lost and resent, from 0 to 1.
    pub loss: f32,
    /// Seed for jitter and loss, so a test repeats.
    pub seed: u64,
}

/// Traffic counters since the session started, from [`NetSession::stats`].
/// Bytes are wire bytes: each message's payload plus its 9-byte frame
/// header. A host's broadcast counts once per client it reached.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NetStats {
    pub messages_sent: u64,
    pub bytes_sent: u64,
    pub messages_received: u64,
    pub bytes_received: u64,
    /// Events [`NetConditions`] hold back right now.
    pub held: usize,
}

/// Wire bytes a frame adds to its payload.
const FRAME_HEADER: u64 = 9;
/// Bytes a datagram adds to its payload: the receiver's token and the
/// sender's id.
const DATAGRAM_HEADER: usize = 12;

thread_local! {
    /// The time [`NetSession::poll`] reads instead of the wall clock.
    static PINNED_NOW: Cell<Option<Instant>> = const { Cell::new(None) };
}

/// Makes [`NetSession::poll`] on this thread take `now` as the current time
/// when it times [`NetSession::simulate`] delays; `None` returns to the wall
/// clock. A realtime scenario pins each tick's planned time, so a slow
/// capture frame does not change which tick a held message arrives on.
pub(crate) fn pin_clock(now: Option<Instant>) {
    PINNED_NOW.with(|pinned| pinned.set(now));
}

/// Simulated conditions, the events they hold back, and the counters.
#[derive(Default)]
struct Lab {
    conditions: NetConditions,
    rng: u64,
    held: VecDeque<(Instant, NetEvent)>,
    /// Unreliable messages held back; they may pass each other.
    loose: Vec<(Instant, NetEvent)>,
    stats: NetStats,
}

impl Lab {
    /// Holds `arrived` as the conditions say and returns what is due at
    /// `now`, in arrival order.
    // ponytail: delay starts when poll sees an event, not when it arrived,
    // so it runs up to one poll interval long; timestamp in read_frames
    // if tests need finer timing.
    fn deliver(
        &mut self,
        arrived: Vec<NetEvent>,
        now: Instant,
    ) -> Vec<NetEvent> {
        for event in arrived {
            let mut at = now + self.delay();
            // In order, as on TCP: nothing passes an event held before it.
            if let Some((last, _)) = self.held.back() {
                at = at.max(*last);
            }
            self.held.push_back((at, event));
        }
        let mut due = Vec::new();
        while self.held.front().is_some_and(|(at, _)| *at <= now) {
            let (_, event) = self.held.pop_front().unwrap();
            if let NetEvent::Message { bytes, .. } = &event {
                self.stats.messages_received += 1;
                self.stats.bytes_received += bytes.len() as u64 + FRAME_HEADER;
            }
            due.push(event);
        }
        due
    }

    /// Like [`Lab::deliver`] for unreliable messages: `loss` drops them
    /// instead of resending, and they keep no order.
    fn deliver_loose(
        &mut self,
        arrived: Vec<NetEvent>,
        now: Instant,
    ) -> Vec<NetEvent> {
        for event in arrived {
            let NetConditions {
                latency,
                jitter,
                loss,
                ..
            } = self.conditions;
            let at = now + latency + jitter.mul_f64(self.random());
            if self.random() >= f64::from(loss) {
                self.loose.push((at, event));
            }
        }
        self.loose.sort_by_key(|(at, _)| *at);
        let due = self.loose.partition_point(|(at, _)| *at <= now);
        self.loose
            .drain(..due)
            .map(|(_, event)| {
                if let NetEvent::Message { bytes, .. } = &event {
                    self.stats.messages_received += 1;
                    self.stats.bytes_received +=
                        (bytes.len() + DATAGRAM_HEADER) as u64;
                }
                event
            })
            .collect()
    }

    fn delay(&mut self) -> Duration {
        let NetConditions {
            latency,
            jitter,
            loss,
            ..
        } = self.conditions;
        let mut delay = latency + jitter.mul_f64(self.random());
        if self.random() < f64::from(loss) {
            delay += (latency * 2).max(Duration::from_millis(200));
        }
        delay
    }

    /// SplitMix64, as a number in [0, 1).
    fn random(&mut self) -> f64 {
        self.rng = self.rng.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.rng;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        (z ^ (z >> 31)) as f64 / 2f64.powi(64)
    }

    fn sent(&mut self, bytes: &[u8]) {
        self.stats.messages_sent += 1;
        self.stats.bytes_sent += bytes.len() as u64 + FRAME_HEADER;
    }
}

/// Present while the game runs as a dedicated server
/// ([`crate::project::SERVER_ENV`], `rusting run --server`): there is no
/// window or local player, so game code should host and not join. Its
/// fields report the tick loop's load, as of the tick before this one.
#[derive(Resource, Clone, Copy, Debug, Default, PartialEq)]
pub struct DedicatedServer {
    /// Ticks run.
    pub ticks: u64,
    /// Share of the fixed step the last tick's work took: above 1.0 the
    /// server cannot keep up.
    pub last_budget: f32,
    /// `last_budget` smoothed over about the last 20 ticks.
    pub budget: f32,
    /// Ticks that started after their deadline.
    pub late_ticks: u64,
    /// Ticks skipped to catch up after a stall of more than 10 steps.
    pub dropped_ticks: u64,
}

/// One end of a multiplayer session. See the [module docs](self).
#[derive(Resource)]
pub struct NetSession {
    me: PeerId,
    room: Option<String>,
    route: Route,
    events: Mutex<Receiver<NetEvent>>,
    lab: Mutex<Lab>,
    /// The unreliable channel of a direct host or client.
    datagrams: Option<Datagrams>,
    /// A direct host's listening address and its stop flag.
    listener: Option<(SocketAddr, Arc<AtomicBool>)>,
    /// The host's world seed: set on the host, received in WELCOME on a
    /// client. A direct host's join threads read it.
    seed: Arc<Mutex<Option<u64>>>,
}

/// A UDP socket beside a direct session's TCP streams. A datagram is the
/// receiver's token (u64), the sender's id (u32), both little-endian, and
/// the payload; the token, sent in WELCOME, keeps strangers from injecting
/// messages.
/// By peer: the token its datagrams carry, the IP address they must come
/// from (the peer's TCP one), and, once one arrived, where to send.
type UdpPeers = BTreeMap<PeerId, (u64, IpAddr, Option<SocketAddr>)>;

struct Datagrams {
    socket: UdpSocket,
    /// A client holds only [`HOST`], with its own token.
    peers: Arc<Mutex<UdpPeers>>,
    events: Mutex<Receiver<NetEvent>>,
    stop: Arc<AtomicBool>,
}

impl Datagrams {
    fn open(socket: UdpSocket, peers: UdpPeers) -> io::Result<Self> {
        // The reader wakes this often to see whether the session ended.
        socket.set_read_timeout(Some(Duration::from_millis(100)))?;
        let peers = Arc::new(Mutex::new(peers));
        let stop = Arc::new(AtomicBool::new(false));
        let (sender, events) = channel();
        let (reader, known, stopped) =
            (socket.try_clone()?, Arc::clone(&peers), Arc::clone(&stop));
        std::thread::spawn(move || {
            let mut buffer = [0; MAX_UNRELIABLE + DATAGRAM_HEADER];
            while !stopped.load(Ordering::Relaxed) {
                let Ok((length, from)) = reader.recv_from(&mut buffer) else {
                    continue;
                };
                let Some((head, bytes)) =
                    buffer[..length].split_at_checked(DATAGRAM_HEADER)
                else {
                    continue;
                };
                let token = u64::from_le_bytes(head[..8].try_into().unwrap());
                let peer = PeerId::from_le_bytes(head[8..].try_into().unwrap());
                let mut known = known.lock().unwrap();
                // Pinning the IP keeps someone who saw the token from
                // turning the host's datagrams on another machine.
                let Some(entry) = known
                    .get_mut(&peer)
                    .filter(|(want, ip, _)| *want == token && *ip == from.ip())
                else {
                    continue;
                };
                entry.2 = Some(from);
                let message = NetEvent::Message {
                    from: peer,
                    bytes: bytes.to_vec(),
                };
                // An empty datagram only says where the peer is.
                if !bytes.is_empty() && sender.send(message).is_err() {
                    return;
                }
            }
        });
        Ok(Self {
            socket,
            peers,
            events: Mutex::new(events),
            stop,
        })
    }

    /// Sends `bytes` to `to` and returns true, or returns false when no
    /// datagram from `to` has arrived yet.
    fn send(&self, me: PeerId, to: PeerId, bytes: &[u8]) -> io::Result<bool> {
        let Some((token, address)) = self
            .peers
            .lock()
            .unwrap()
            .get(&to)
            .and_then(|(token, _, address)| Some((*token, (*address)?)))
        else {
            return Ok(false);
        };
        let mut datagram = token.to_le_bytes().to_vec();
        datagram.extend(me.to_le_bytes());
        datagram.extend(bytes);
        self.socket.send_to(&datagram, address)?;
        Ok(true)
    }
}

pub(crate) fn token() -> u64 {
    u64::from_le_bytes(uuid::Uuid::new_v4().as_bytes()[..8].try_into().unwrap())
}

enum Route {
    /// Streams by peer: a direct host's clients, or a client's host.
    Direct(Arc<Mutex<BTreeMap<PeerId, TcpStream>>>),
    /// One stream to a relay; frames name the other end.
    Relay(Mutex<TcpStream>),
    /// In-process sessions made by [`NetSession::loopback`]: every member's
    /// event queue, host included, by peer.
    Loopback(Arc<Mutex<BTreeMap<PeerId, Sender<NetEvent>>>>),
}

impl NetSession {
    /// Hosts on `port` of every interface. Clients must join with
    /// `password`; an empty one lets anyone join.
    pub fn host(port: u16, password: &str) -> io::Result<Self> {
        Self::host_on(TcpListener::bind(("0.0.0.0", port))?, password)
    }

    /// Hosts on an already bound listener, such as one on port 0 for a
    /// free port.
    pub fn host_on(listener: TcpListener, password: &str) -> io::Result<Self> {
        let address = listener.local_addr()?;
        // A free TCP port can have its UDP twin taken. Then the host runs
        // without UDP and unreliable sends go reliably, as over a relay.
        let datagrams = match UdpSocket::bind(address) {
            Ok(socket) => Some(Datagrams::open(socket, BTreeMap::new())?),
            Err(_) => None,
        };
        let tokens = datagrams.as_ref().map(|d| Arc::clone(&d.peers));
        let (sender, events) = channel();
        let peers = Arc::new(Mutex::new(BTreeMap::new()));
        let accepted = Arc::clone(&peers);
        let closed = Arc::new(AtomicBool::new(false));
        let stop = Arc::clone(&closed);
        let password: Arc<str> = password.into();
        let next = Arc::new(AtomicU32::new(HOST + 1));
        let pending = Arc::new(AtomicUsize::new(0));
        let seed = Arc::new(Mutex::new(None));
        let shared_seed = Arc::clone(&seed);
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                // Dropping the session sets `stop` and connects once to
                // wake this loop, which frees the port.
                if stop.load(Ordering::Relaxed) {
                    return;
                }
                let Ok(stream) = stream else { continue };
                if pending.load(Ordering::Relaxed) >= MAX_PENDING_JOINS {
                    continue;
                }
                pending.fetch_add(1, Ordering::Relaxed);
                let (peers, tokens, next, pending, password, seed, sender) = (
                    Arc::clone(&accepted),
                    tokens.clone(),
                    Arc::clone(&next),
                    Arc::clone(&pending),
                    Arc::clone(&password),
                    Arc::clone(&shared_seed),
                    sender.clone(),
                );
                // Each join shakes hands on its own thread, so a silent
                // client delays nobody else.
                std::thread::spawn(move || {
                    let joined = admit_client(
                        &stream,
                        &peers,
                        tokens.as_deref(),
                        &next,
                        &password,
                        &seed,
                        &sender,
                    );
                    pending.fetch_sub(1, Ordering::Relaxed);
                    let Some(peer) = joined else { return };
                    read_frames(
                        stream,
                        |kind, _, bytes| {
                            (kind == DATA).then_some(NetEvent::Message {
                                from: peer,
                                bytes,
                            })
                        },
                        &sender,
                    );
                    peers.lock().unwrap().remove(&peer);
                    if let Some(tokens) = &tokens {
                        tokens.lock().unwrap().remove(&peer);
                    }
                    let _ = sender.send(NetEvent::Disconnected(peer));
                });
            }
        });
        Ok(Self {
            me: HOST,
            room: None,
            route: Route::Direct(peers),
            events: Mutex::new(events),
            lab: Mutex::default(),
            datagrams,
            listener: Some((address, closed)),
            seed,
        })
    }

    /// Joins a host at `address`, such as `"192.168.1.20:7777"`, with the
    /// host's password (empty if it has none).
    pub fn join(
        address: impl ToSocketAddrs,
        password: &str,
    ) -> io::Result<Self> {
        let stream = TcpStream::connect(address)?;
        let (me, token, seed) = request_join(&stream, "", password)?;
        let host = stream.peer_addr()?;
        let datagrams = match token {
            Some(token) => {
                let any: SocketAddr = if host.is_ipv4() {
                    ([0, 0, 0, 0], 0).into()
                } else {
                    (Ipv6Addr::UNSPECIFIED, 0).into()
                };
                let datagrams = Datagrams::open(
                    UdpSocket::bind(any)?,
                    BTreeMap::from([(HOST, (token, host.ip(), Some(host)))]),
                )?;
                // Tells the host where to send. Repeated in case one is
                // lost; until one arrives the host sends reliably.
                for _ in 0..3 {
                    datagrams.send(me, HOST, &[])?;
                }
                Some(datagrams)
            }
            None => None,
        };
        let (sender, events) = channel();
        let reader = stream.try_clone()?;
        std::thread::spawn(move || {
            read_frames(reader, data_from_frame, &sender);
            let _ = sender.send(NetEvent::Closed);
        });
        Ok(Self {
            me,
            room: None,
            route: Route::Direct(Arc::new(Mutex::new(BTreeMap::from([(
                HOST, stream,
            )])))),
            events: Mutex::new(events),
            lab: Mutex::default(),
            datagrams,
            listener: None,
            seed: Arc::new(Mutex::new(seed)),
        })
    }

    /// Hosts through the relay at `relay` (see [`run_relay`]). The relay
    /// hands out a room code, [`NetSession::room_code`], for clients to
    /// join with. `token` is the relay's token (empty if it has none).
    pub fn host_room(
        relay: impl ToSocketAddrs,
        token: &str,
    ) -> io::Result<Self> {
        let mut stream = TcpStream::connect(relay)?;
        stream.set_nodelay(true)?;
        let mut payload = PROTOCOL_VERSION.to_le_bytes().to_vec();
        payload.extend(token.as_bytes());
        write_frame(&mut stream, HOST_ROOM, HOST, &payload)?;
        let code = match read_handshake(&stream)? {
            (ROOM, _, code) => String::from_utf8_lossy(&code).into_owned(),
            (REJECT, _, reason) => return Err(rejected(&reason)),
            _ => return Err(invalid("the relay did not answer with a room")),
        };
        let (sender, events) = channel();
        let reader = stream.try_clone()?;
        std::thread::spawn(move || {
            read_frames(
                reader,
                |kind, peer, bytes| match kind {
                    DATA => Some(NetEvent::Message { from: peer, bytes }),
                    JOINED => Some(NetEvent::Connected(peer)),
                    LEFT => Some(NetEvent::Disconnected(peer)),
                    _ => None,
                },
                &sender,
            );
            let _ = sender.send(NetEvent::Closed);
        });
        Ok(Self {
            me: HOST,
            room: Some(code),
            route: Route::Relay(Mutex::new(stream)),
            events: Mutex::new(events),
            lab: Mutex::default(),
            datagrams: None,
            listener: None,
            seed: Arc::default(),
        })
    }

    /// Joins the room `code` on the relay at `relay`. Codes ignore case.
    /// `token` is the relay's token (empty if it has none).
    pub fn join_room(
        relay: impl ToSocketAddrs,
        code: &str,
        token: &str,
    ) -> io::Result<Self> {
        let stream = TcpStream::connect(relay)?;
        let code = code.trim().to_ascii_uppercase();
        let (me, _, seed) = request_join(&stream, &code, token)?;
        let (sender, events) = channel();
        let reader = stream.try_clone()?;
        std::thread::spawn(move || {
            read_frames(reader, data_from_frame, &sender);
            let _ = sender.send(NetEvent::Closed);
        });
        Ok(Self {
            me,
            room: Some(code),
            route: Route::Relay(Mutex::new(stream)),
            events: Mutex::new(events),
            lab: Mutex::default(),
            datagrams: None,
            listener: None,
            seed: Arc::new(Mutex::new(seed)),
        })
    }

    /// A host and `clients` clients joined to it in this process, with no
    /// sockets, for tests. They behave like a direct host and its clients:
    /// the host's first [`NetSession::poll`] reports each client as
    /// `Connected`, dropping a client reports it `Disconnected`, and
    /// dropping the host closes every client.
    #[must_use]
    pub fn loopback(clients: usize) -> (Self, Vec<Self>) {
        let group = Arc::new(Mutex::new(BTreeMap::new()));
        // Every member shares the host's seed, as if each joined after the
        // host set it.
        let seed = Arc::new(Mutex::new(None));
        let member = |me: PeerId| {
            let (sender, events) = channel();
            group.lock().unwrap().insert(me, sender);
            Self {
                me,
                room: None,
                route: Route::Loopback(Arc::clone(&group)),
                events: Mutex::new(events),
                lab: Mutex::default(),
                datagrams: None,
                listener: None,
                seed: Arc::clone(&seed),
            }
        };
        let host = member(HOST);
        let clients = (1..=clients as PeerId)
            .map(|peer| {
                let client = member(peer);
                let group = group.lock().unwrap();
                let _ = group[&HOST].send(NetEvent::Connected(peer));
                client
            })
            .collect();
        (host, clients)
    }

    /// This end's id: [`HOST`] for the host.
    #[must_use]
    pub fn id(&self) -> PeerId {
        self.me
    }

    /// The relay room code, if the session goes through a relay.
    #[must_use]
    pub fn room_code(&self) -> Option<&str> {
        self.room.as_deref()
    }

    /// Sets the world seed that clients receive when they join (host
    /// only), usually `scene.seed()`. Call it right after hosting: a
    /// client that joined before gets no seed.
    pub fn set_seed(&self, seed: u64) -> io::Result<()> {
        if self.me != HOST {
            return Err(invalid("only the host sets the seed"));
        }
        if let Route::Relay(stream) = &self.route {
            let bytes = seed.to_le_bytes();
            write_frame(&mut *stream.lock().unwrap(), SEED, HOST, &bytes)?;
        }
        *self.seed.lock().unwrap() = Some(seed);
        Ok(())
    }

    /// The host's world seed: on the host, the one given to
    /// [`NetSession::set_seed`]; on a client, the one the host had set
    /// when this client joined. `None` when the host set none. Give it to
    /// `scene.set_seed` so the client builds the same world.
    #[must_use]
    pub fn seed(&self) -> Option<u64> {
        *self.seed.lock().unwrap()
    }

    /// Clients connected now, in id order (host only; empty on a client).
    #[must_use]
    pub fn peers(&self) -> Vec<PeerId> {
        match &self.route {
            Route::Direct(peers) if self.me == HOST => {
                peers.lock().unwrap().keys().copied().collect()
            }
            Route::Loopback(group) if self.me == HOST => group
                .lock()
                .unwrap()
                .keys()
                .copied()
                .filter(|&peer| peer != HOST)
                .collect(),
            _ => Vec::new(),
        }
    }

    /// Sends `bytes` to `to`. A client can send only to [`HOST`].
    pub fn send(&self, to: PeerId, bytes: &[u8]) -> io::Result<()> {
        if self.me != HOST && to != HOST {
            return Err(invalid("clients send only to the host"));
        }
        match &self.route {
            Route::Direct(peers) => {
                let mut peers = peers.lock().unwrap();
                let stream = peers
                    .get_mut(&to)
                    .ok_or_else(|| invalid("no such peer"))?;
                write_frame(stream, DATA, self.me, bytes)?;
            }
            Route::Relay(stream) => {
                write_frame(&mut *stream.lock().unwrap(), DATA, to, bytes)?;
            }
            Route::Loopback(group) => {
                let message = NetEvent::Message {
                    from: self.me,
                    bytes: bytes.to_vec(),
                };
                group
                    .lock()
                    .unwrap()
                    .get(&to)
                    .and_then(|peer| peer.send(message).ok())
                    .ok_or_else(|| invalid("no such peer"))?;
            }
        }
        self.lab.lock().unwrap().sent(bytes);
        Ok(())
    }

    /// Sends `bytes` to every client (host), or to the host (client).
    pub fn broadcast(&self, bytes: &[u8]) -> io::Result<()> {
        if self.me != HOST {
            return self.send(HOST, bytes);
        }
        match &self.route {
            // A client that fails to take the message is dropped by its
            // reader thread, which reports Disconnected.
            Route::Direct(peers) => {
                for stream in peers.lock().unwrap().values_mut() {
                    if write_frame(stream, DATA, HOST, bytes).is_ok() {
                        self.lab.lock().unwrap().sent(bytes);
                    }
                }
                Ok(())
            }
            Route::Relay(stream) => {
                write_frame(
                    &mut *stream.lock().unwrap(),
                    DATA,
                    EVERYONE,
                    bytes,
                )?;
                self.lab.lock().unwrap().sent(bytes);
                Ok(())
            }
            Route::Loopback(_) => {
                for peer in self.peers() {
                    self.send(peer, bytes)?;
                }
                Ok(())
            }
        }
    }

    /// Sends `bytes` to `to` over UDP: faster than [`NetSession::send`]
    /// under loss, but it may be lost, duplicated or overtaken. Use it for
    /// state sent every tick, such as positions. At most
    /// [`MAX_UNRELIABLE`] bytes. It goes reliably instead where there is
    /// no UDP route: through a relay, in a loopback session, and to a
    /// client whose first datagram has not reached the host yet.
    pub fn send_unreliable(&self, to: PeerId, bytes: &[u8]) -> io::Result<()> {
        if bytes.len() > MAX_UNRELIABLE {
            return Err(invalid(
                "an unreliable message holds at most 1200 bytes",
            ));
        }
        if self.me != HOST && to != HOST {
            return Err(invalid("clients send only to the host"));
        }
        match &self.datagrams {
            Some(datagrams) if datagrams.send(self.me, to, bytes)? => {
                let mut lab = self.lab.lock().unwrap();
                lab.stats.messages_sent += 1;
                lab.stats.bytes_sent += (bytes.len() + DATAGRAM_HEADER) as u64;
                Ok(())
            }
            _ => self.send(to, bytes),
        }
    }

    /// [`NetSession::send_unreliable`] to every client (host), or to the
    /// host (client).
    pub fn broadcast_unreliable(&self, bytes: &[u8]) -> io::Result<()> {
        if self.me != HOST {
            return self.send_unreliable(HOST, bytes);
        }
        for peer in self.peers() {
            self.send_unreliable(peer, bytes)?;
        }
        Ok(())
    }

    /// Events since the last call, in arrival order. Never blocks. Under
    /// [`NetSession::simulate`], events come back once their delay passes.
    pub fn poll(&self) -> Vec<NetEvent> {
        let now = PINNED_NOW.with(Cell::get).unwrap_or_else(Instant::now);
        let arrived = self.events.lock().unwrap().try_iter().collect();
        let mut lab = self.lab.lock().unwrap();
        let mut events = lab.deliver(arrived, now);
        if let Some(datagrams) = &self.datagrams {
            let arrived = datagrams.events.lock().unwrap().try_iter().collect();
            events.extend(lab.deliver_loose(arrived, now));
        }
        events
    }

    /// Pretends this end has a slow or lossy connection from now on; see
    /// [`NetConditions`]. `NetConditions::default()` turns it off, and
    /// events already held still wait out their delay.
    pub fn simulate(&self, conditions: NetConditions) {
        let mut lab = self.lab.lock().unwrap();
        lab.conditions = conditions;
        lab.rng = conditions.seed;
    }

    /// Messages and bytes sent and received so far.
    #[must_use]
    pub fn stats(&self) -> NetStats {
        let lab = self.lab.lock().unwrap();
        NetStats {
            held: lab.held.len() + lab.loose.len(),
            ..lab.stats
        }
    }
}

impl Drop for NetSession {
    fn drop(&mut self) {
        if let Some((address, closed)) = &self.listener {
            closed.store(true, Ordering::Relaxed);
            let wake = if address.ip().is_unspecified() {
                SocketAddr::from(([127, 0, 0, 1], address.port()))
            } else {
                *address
            };
            let _ = TcpStream::connect_timeout(&wake, HANDSHAKE_TIMEOUT);
        }
        if let Some(datagrams) = &self.datagrams {
            datagrams.stop.store(true, Ordering::Relaxed);
        }
        // Shutting the sockets down ends the reader threads.
        match &self.route {
            Route::Direct(peers) => {
                for stream in peers.lock().unwrap().values() {
                    let _ = stream.shutdown(Shutdown::Both);
                }
            }
            Route::Relay(stream) => {
                let _ = stream.lock().unwrap().shutdown(Shutdown::Both);
            }
            Route::Loopback(group) => {
                let mut group = group.lock().unwrap();
                group.remove(&self.me);
                if self.me == HOST {
                    for peer in group.values() {
                        let _ = peer.send(NetEvent::Closed);
                    }
                } else if let Some(host) = group.get(&HOST) {
                    let _ = host.send(NetEvent::Disconnected(self.me));
                }
            }
        }
    }
}

/// Runs a relay on `listener` with [`RelayLimits::default`]: hosts ask it
/// for a room code with [`NetSession::host_room`] and clients join that code
/// with [`NetSession::join_room`]. The relay forwards host messages to
/// clients and client messages to the host. Hosts and clients must give
/// `token`; an empty one lets anyone in. `rusting relay` runs one. It runs
/// until the process ends; a failed incoming connection is skipped.
// ponytail: one lock over every room serialises room bookkeeping and
// queueing (not socket writes); per-room locks if one relay serves many busy
// rooms.
pub fn run_relay(listener: TcpListener, token: &str) -> io::Result<()> {
    run_relay_with(listener, token, RelayLimits::default())
}

/// What a relay lets one address, and everyone together, use. A relay on
/// the open internet needs them; the defaults suit a small game's relay.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RelayLimits {
    /// Open connections from everyone; more are closed at once.
    pub max_connections: usize,
    /// Open connections from one IP address.
    pub max_connections_per_address: usize,
    /// New connections one IP address may open per minute, failed ones
    /// included, which also slows token guessing.
    pub connects_per_minute: u32,
    /// Bytes per second one connection may send, with one second's worth
    /// as a burst; a faster sender is cut off. 0 means no cap.
    pub bytes_per_second: u64,
    /// Rooms open at once; another host is refused with "relay is full".
    pub max_rooms: usize,
    /// Clients in one room; another join is refused with "room is full".
    pub max_clients_per_room: usize,
    /// Room opens and join attempts one IP address may make per minute,
    /// refused ones included, which also slows room code guessing.
    pub room_attempts_per_minute: u32,
    /// A connection that sends nothing for this long after its handshake is
    /// closed. Zero means never.
    pub idle_timeout: Duration,
    /// Bytes waiting to be written to one connection. A peer that stops
    /// reading is cut off when its queue is full, so it never holds up
    /// anyone else. A host's queue gives each client an equal share
    /// (this over `max_clients_per_room`); a client over its share is cut
    /// off, not the host or other clients. One message larger than a
    /// queue or share still goes when nothing of it is waiting. A message
    /// sent to many peers is held once.
    pub send_queue: usize,
}

impl Default for RelayLimits {
    fn default() -> Self {
        Self {
            max_connections: 1024,
            max_connections_per_address: 16,
            connects_per_minute: 60,
            bytes_per_second: 1 << 20,
            max_rooms: 256,
            max_clients_per_room: 64,
            room_attempts_per_minute: 30,
            idle_timeout: Duration::from_secs(600),
            send_queue: 1 << 20,
        }
    }
}

/// [`run_relay`] with chosen [`RelayLimits`].
pub fn run_relay_with(
    listener: TcpListener,
    token: &str,
    limits: RelayLimits,
) -> io::Result<()> {
    let rooms = Arc::new(Mutex::new(HashMap::<String, RelayRoom>::new()));
    let token: Arc<str> = token.into();
    let admission = Arc::new(Mutex::new(Admission::default()));
    for stream in listener.incoming() {
        // A connection that failed before accept (reset, or out of file
        // handles) is skipped; the pause keeps a persistent error from
        // spinning the loop.
        let stream = match stream {
            Ok(stream) => stream,
            Err(_) => {
                std::thread::sleep(Duration::from_millis(10));
                continue;
            }
        };
        let Ok(address) = stream.peer_addr() else {
            continue;
        };
        let Some(slot) =
            Admission::admit(&admission, limit_key(address.ip()), &limits)
        else {
            continue;
        };
        let rooms = Arc::clone(&rooms);
        let token = Arc::clone(&token);
        std::thread::spawn(move || {
            relay_connection(stream, &rooms, &token, &limits, &slot);
            drop(slot);
        });
    }
    Ok(())
}

/// Open connections and recent connects, overall and per address.
#[derive(Default)]
struct Admission {
    open: usize,
    /// Per address: open connections, start of the minute counted, and
    /// connects and room attempts in that minute.
    addresses: HashMap<IpAddr, (usize, Instant, u32, u32)>,
}

impl Admission {
    /// Counts a new connection from `address`, or refuses it with `None`.
    /// The slot gives the connection back when dropped.
    fn admit(
        this: &Arc<Mutex<Self>>,
        address: IpAddr,
        limits: &RelayLimits,
    ) -> Option<AdmissionSlot> {
        let mut admission = this.lock().unwrap();
        let now = Instant::now();
        let minute = Duration::from_secs(60);
        if admission.addresses.len() > 4 * limits.max_connections {
            admission.addresses.retain(|_, (open, start, ..)| {
                *open > 0 || now.duration_since(*start) < minute
            });
        }
        if admission.open >= limits.max_connections {
            return None;
        }
        let (open, start, connects, attempts) =
            admission.addresses.entry(address).or_insert((0, now, 0, 0));
        if now.duration_since(*start) >= minute {
            (*start, *connects, *attempts) = (now, 0, 0);
        }
        // Refused connects count too, so hammering never resets the count.
        *connects = connects.saturating_add(1);
        if *open >= limits.max_connections_per_address
            || *connects > limits.connects_per_minute
        {
            return None;
        }
        *open += 1;
        admission.open += 1;
        Some(AdmissionSlot {
            admission: Arc::clone(this),
            address,
        })
    }
}

struct AdmissionSlot {
    admission: Arc<Mutex<Admission>>,
    address: IpAddr,
}

impl AdmissionSlot {
    /// Counts a room open or join from this connection's address; false
    /// when the address is over `limit` this minute.
    fn room_attempt(&self, limit: u32) -> bool {
        let mut admission = self.admission.lock().unwrap();
        let Some((.., attempts)) = admission.addresses.get_mut(&self.address)
        else {
            return false;
        };
        *attempts = attempts.saturating_add(1);
        *attempts <= limit
    }
}

impl Drop for AdmissionSlot {
    fn drop(&mut self) {
        let mut admission = self.admission.lock().unwrap();
        admission.open -= 1;
        if let Some((open, ..)) = admission.addresses.get_mut(&self.address) {
            *open -= 1;
        }
    }
}

/// Holds a reader to a byte rate, allowing one second's worth in a burst.
struct Throttle {
    rate: f64,
    allowance: f64,
    last: Instant,
}

impl Throttle {
    fn new(bytes_per_second: u64) -> Self {
        let rate = bytes_per_second as f64;
        Self {
            rate,
            allowance: rate,
            last: Instant::now(),
        }
    }

    /// The largest message one burst allows, so a message the rate would
    /// refuse is refused before the relay buffers it.
    fn largest(&self) -> usize {
        if self.rate <= 0.0 {
            MAX_MESSAGE
        } else {
            MAX_MESSAGE.min((self.rate as usize).saturating_sub(9))
        }
    }

    /// Spends `bytes`; false when the rate does not allow them.
    fn spend(&mut self, bytes: usize) -> bool {
        if self.rate <= 0.0 {
            return true;
        }
        let now = Instant::now();
        let earned = now.duration_since(self.last).as_secs_f64() * self.rate;
        self.allowance = (self.allowance + earned).min(self.rate);
        self.last = now;
        self.allowance -= bytes as f64;
        self.allowance >= 0.0
    }
}

/// A frame waiting in a [`RelayPeer`] queue and the client share it
/// counts against, if any.
type Queued = (Arc<[u8]>, Option<Arc<AtomicUsize>>);

/// One relay connection's write side: frames wait in a queue capped in
/// bytes that a thread of its own writes out. A frame sent to several peers
/// is shared, not copied. A host's queue is split into one share per
/// client instead, counted by the client (see [`Self::offer`]).
struct RelayPeer {
    queue: Sender<Queued>,
    /// Bytes queued and not yet written.
    queued: Arc<AtomicUsize>,
    limit: usize,
    stream: TcpStream,
}

impl RelayPeer {
    fn new(stream: &TcpStream, limit: usize) -> io::Result<Self> {
        let (queue, frames) = channel::<Queued>();
        let queued = Arc::new(AtomicUsize::new(0));
        let mut writer = stream.try_clone()?;
        let written = Arc::clone(&queued);
        std::thread::spawn(move || {
            for (frame, owner) in frames {
                if writer.write_all(&frame).is_err() {
                    let _ = writer.shutdown(Shutdown::Both);
                    break;
                }
                written.fetch_sub(frame.len(), Ordering::Relaxed);
                if let Some(owner) = owner {
                    owner.fetch_sub(frame.len(), Ordering::Relaxed);
                }
            }
        });
        Ok(Self {
            queue,
            queued,
            limit,
            stream: stream.try_clone()?,
        })
    }

    /// Queues a frame; a peer whose queue is full is cut off, since a
    /// dropped frame would garble everything after it.
    fn send(&self, frame: &Arc<[u8]>) {
        if !(fits(&self.queued, frame.len(), self.limit)
            && self.push(frame, None))
        {
            let _ = self.stream.shutdown(Shutdown::Both);
        }
    }

    /// Queues a client's frame for this host if it fits the client's
    /// `share` of the queue (`held` counts the client's bytes not yet
    /// written), so a client that floods a slow host overflows only its
    /// own share. False when it does not fit; the caller then drops the
    /// client.
    fn offer(
        &self,
        frame: &Arc<[u8]>,
        held: &Arc<AtomicUsize>,
        share: usize,
    ) -> bool {
        fits(held, frame.len(), share) && self.push(frame, Some(held))
    }

    /// Queues a frame without a size check. Callers hold the rooms lock,
    /// so only the writer runs alongside.
    fn push(
        &self,
        frame: &Arc<[u8]>,
        owner: Option<&Arc<AtomicUsize>>,
    ) -> bool {
        self.queued.fetch_add(frame.len(), Ordering::Relaxed);
        if let Some(owner) = owner {
            owner.fetch_add(frame.len(), Ordering::Relaxed);
        }
        self.queue.send((Arc::clone(frame), owner.cloned())).is_ok()
    }

    fn forward(&self, kind: u8, peer: PeerId, bytes: &[u8]) {
        self.send(&relay_frame(kind, peer, bytes));
    }
}

/// Whether `bytes` more fit beside `held` under `limit`. One frame larger
/// than the limit still fits when nothing is held.
fn fits(held: &AtomicUsize, bytes: usize, limit: usize) -> bool {
    let held = held.load(Ordering::Relaxed);
    held == 0 || held + bytes <= limit
}

/// A frame for [`RelayPeer::send`]. Relayed payloads are read at most
/// `MAX_MESSAGE` long, so encoding cannot fail.
fn relay_frame(kind: u8, peer: PeerId, bytes: &[u8]) -> Arc<[u8]> {
    encode_frame(kind, peer, bytes).unwrap_or_default().into()
}

struct RelayRoom {
    host: RelayPeer,
    clients: BTreeMap<PeerId, RelayPeer>,
    next: PeerId,
    /// The host's world seed, sent in each WELCOME.
    seed: Option<u64>,
}

type Rooms = Mutex<HashMap<String, RelayRoom>>;

fn relay_connection(
    mut stream: TcpStream,
    rooms: &Rooms,
    token: &str,
    limits: &RelayLimits,
    slot: &AdmissionSlot,
) {
    // Only the writer thread blocks on a peer that stops reading, and only
    // this long.
    let _ = stream.set_write_timeout(Some(HANDSHAKE_TIMEOUT));
    let _ = stream.set_nodelay(true);
    let Ok((kind, _, payload)) = read_handshake(&stream) else {
        return;
    };
    let request = match kind {
        HOST_ROOM => check_version(&payload).map(|()| (None, &payload[2..])),
        JOIN => parse_join(&payload).map(|(code, secret)| (Some(code), secret)),
        _ => return,
    }
    .and_then(|(code, secret)| {
        if same_secret(secret, token.as_bytes()) {
            Ok(code)
        } else {
            Err("wrong relay token".to_owned())
        }
    })
    .and_then(|code| {
        if slot.room_attempt(limits.room_attempts_per_minute) {
            Ok(code)
        } else {
            Err("too many rooms opened or joined; wait a minute".to_owned())
        }
    });
    let code = match request {
        Ok(code) => code,
        Err(reason) => {
            reject(&mut stream, &reason);
            return;
        }
    };
    if !limits.idle_timeout.is_zero() {
        let _ = stream.set_read_timeout(Some(limits.idle_timeout));
    }
    let Ok(peer) = RelayPeer::new(&stream, limits.send_queue) else {
        return;
    };
    let throttle = Throttle::new(limits.bytes_per_second);
    match code {
        None => relay_host(stream, peer, rooms, limits, throttle),
        Some(code) => relay_client(stream, peer, rooms, limits, code, throttle),
    }
}

fn reject(stream: &mut TcpStream, reason: &str) {
    let _ = write_frame(stream, REJECT, HOST, reason.as_bytes());
}

fn relay_host(
    mut stream: TcpStream,
    peer: RelayPeer,
    rooms: &Rooms,
    limits: &RelayLimits,
    mut throttle: Throttle,
) {
    let code = {
        let mut rooms = rooms.lock().unwrap();
        if rooms.len() >= limits.max_rooms {
            drop(rooms);
            reject(&mut stream, "relay is full");
            return;
        }
        let code = loop {
            let code = room_code();
            if !rooms.contains_key(&code) {
                break code;
            }
        };
        peer.forward(ROOM, HOST, code.as_bytes());
        rooms.insert(
            code.clone(),
            RelayRoom {
                host: peer,
                clients: BTreeMap::new(),
                next: HOST + 1,
                seed: None,
            },
        );
        code
    };
    while let Ok((kind, to, bytes)) =
        read_frame(&mut stream, throttle.largest())
    {
        if !throttle.spend(9 + bytes.len()) {
            break;
        }
        if kind != DATA && kind != SEED {
            continue;
        }
        let mut rooms = rooms.lock().unwrap();
        let Some(room) = rooms.get_mut(&code) else {
            break;
        };
        if kind == SEED {
            room.seed = bytes.try_into().ok().map(u64::from_le_bytes);
            continue;
        }
        let frame = relay_frame(DATA, HOST, &bytes);
        for (&peer, client) in &room.clients {
            if to == EVERYONE || to == peer {
                client.send(&frame);
            }
        }
    }
    // The room ends with its host.
    if let Some(room) = rooms.lock().unwrap().remove(&code) {
        for client in room.clients.values() {
            let _ = client.stream.shutdown(Shutdown::Both);
        }
    }
    let _ = stream.shutdown(Shutdown::Both);
}

fn relay_client(
    mut stream: TcpStream,
    me: RelayPeer,
    rooms: &Rooms,
    limits: &RelayLimits,
    code: String,
    mut throttle: Throttle,
) {
    // This client's bytes waiting in the host's queue, and its share.
    let held = Arc::new(AtomicUsize::new(0));
    let share = limits.send_queue / limits.max_clients_per_room.max(1);
    let peer = {
        let mut rooms = rooms.lock().unwrap();
        let refused = match rooms.get(&code) {
            None => Some(format!("no room {code}")),
            Some(room) if room.clients.len() >= limits.max_clients_per_room => {
                Some("room is full".to_owned())
            }
            Some(_) => None,
        };
        if let Some(reason) = refused {
            drop(rooms);
            reject(&mut stream, &reason);
            return;
        }
        let room = rooms.get_mut(&code).unwrap();
        let peer = room.next;
        room.next += 1;
        me.forward(WELCOME, peer, &welcome_payload(None, room.seed));
        room.host.push(&relay_frame(JOINED, peer, &[]), None);
        room.clients.insert(peer, me);
        peer
    };
    while let Ok((kind, _, bytes)) = read_frame(&mut stream, throttle.largest())
    {
        if !throttle.spend(9 + bytes.len()) {
            break;
        }
        if kind != DATA {
            continue;
        }
        let mut rooms = rooms.lock().unwrap();
        let Some(room) = rooms.get_mut(&code) else {
            break;
        };
        // A host behind on reading drops the sender, not the room.
        if !room
            .host
            .offer(&relay_frame(DATA, peer, &bytes), &held, share)
        {
            break;
        }
    }
    let _ = stream.shutdown(Shutdown::Both);
    let mut rooms = rooms.lock().unwrap();
    if let Some(room) = rooms.get_mut(&code) {
        room.clients.remove(&peer);
        room.host.push(&relay_frame(LEFT, peer, &[]), None);
    }
}

/// Six letters and digits without look-alikes (no I, O, 0 or 1).
fn room_code() -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";
    uuid::Uuid::new_v4().as_bytes()[..6]
        .iter()
        .map(|byte| char::from(ALPHABET[usize::from(*byte) % ALPHABET.len()]))
        .collect()
}

/// Host side of a direct join: reads JOIN, answers WELCOME or REJECT, and
/// on WELCOME adds the client to `peers` and reports it. Returns its id.
fn admit_client(
    stream: &TcpStream,
    peers: &Mutex<BTreeMap<PeerId, TcpStream>>,
    tokens: Option<&Mutex<UdpPeers>>,
    next: &AtomicU32,
    password: &str,
    seed: &Mutex<Option<u64>>,
    sender: &Sender<NetEvent>,
) -> Option<PeerId> {
    let mut stream = stream;
    stream.set_nodelay(true).ok()?;
    let (kind, _, payload) = read_handshake(stream).ok()?;
    if kind != JOIN {
        return None;
    }
    let checked = parse_join(&payload).and_then(|(_, secret)| {
        if same_secret(secret, password.as_bytes()) {
            Ok(())
        } else {
            Err("wrong password".to_owned())
        }
    });
    if let Err(reason) = checked {
        let _ = write_frame(&mut stream, REJECT, HOST, reason.as_bytes());
        return None;
    }
    let writer = stream.try_clone().ok()?;
    let ip = stream.peer_addr().ok()?.ip();
    // Ids, WELCOMEs and Connected events follow one order under the lock.
    let mut peers = peers.lock().unwrap();
    let peer = next.fetch_add(1, Ordering::Relaxed);
    // With no token in WELCOME the client sends everything reliably.
    let token = tokens.map(|tokens| (tokens, token()));
    let seed = *seed.lock().unwrap();
    let welcome = welcome_payload(token.map(|(_, token)| token), seed);
    write_frame(&mut stream, WELCOME, peer, &welcome).ok()?;
    if let Some((tokens, token)) = token {
        tokens.lock().unwrap().insert(peer, (token, ip, None));
    }
    if sender.send(NetEvent::Connected(peer)).is_err() {
        // The session is gone.
        let _ = stream.shutdown(Shutdown::Both);
        return None;
    }
    peers.insert(peer, writer);
    Some(peer)
}

/// Client side of a join; returns the id the host or relay assigned, a
/// direct host's datagram token and the host's world seed (see
/// [`welcome_payload`]). A JOIN carries the version, the room code's
/// length as one byte, the code and then the password or relay token.
fn request_join(
    stream: &TcpStream,
    code: &str,
    secret: &str,
) -> io::Result<(PeerId, Option<u64>, Option<u64>)> {
    let mut stream = stream;
    stream.set_nodelay(true)?;
    let length =
        u8::try_from(code.len()).map_err(|_| invalid("room code too long"))?;
    let mut payload = PROTOCOL_VERSION.to_le_bytes().to_vec();
    payload.push(length);
    payload.extend(code.as_bytes());
    payload.extend(secret.as_bytes());
    write_frame(&mut stream, JOIN, HOST, &payload)?;
    match read_handshake(stream)? {
        (WELCOME, me, payload) => {
            let (token, seed) = parse_welcome(&payload)
                .ok_or_else(|| invalid("malformed welcome"))?;
            Ok((me, token, seed))
        }
        (REJECT, _, reason) => Err(rejected(&reason)),
        _ => Err(invalid("the host did not answer the join")),
    }
}

/// A WELCOME payload: a flags byte (1: a datagram token follows, 2: a
/// world seed follows), then each present value as a little-endian u64.
/// With no token the client sends everything reliably.
fn welcome_payload(token: Option<u64>, seed: Option<u64>) -> Vec<u8> {
    let flags = u8::from(token.is_some()) | u8::from(seed.is_some()) << 1;
    let mut payload = vec![flags];
    for value in [token, seed].into_iter().flatten() {
        payload.extend(value.to_le_bytes());
    }
    payload
}

/// The token and seed of a [`welcome_payload`], or `None` if malformed.
fn parse_welcome(payload: &[u8]) -> Option<(Option<u64>, Option<u64>)> {
    let (&flags, mut rest) = payload.split_first()?;
    let mut take = |bit: u8| -> Option<Option<u64>> {
        if flags & bit == 0 {
            return Some(None);
        }
        let (value, tail) = rest.split_first_chunk::<8>()?;
        rest = tail;
        Some(Some(u64::from_le_bytes(*value)))
    };
    let token = take(1)?;
    let seed = take(2)?;
    rest.is_empty().then_some((token, seed))
}

fn check_version(payload: &[u8]) -> Result<(), String> {
    match payload {
        [low, high, ..]
            if u16::from_le_bytes([*low, *high]) == PROTOCOL_VERSION =>
        {
            Ok(())
        }
        [low, high, ..] => Err(format!(
            "protocol version {} is not {PROTOCOL_VERSION}",
            u16::from_le_bytes([*low, *high])
        )),
        _ => Err("missing protocol version".into()),
    }
}

/// Splits a JOIN payload into its room code and secret.
fn parse_join(payload: &[u8]) -> Result<(String, &[u8]), String> {
    check_version(payload)?;
    let rest = &payload[2..];
    let length = usize::from(*rest.first().ok_or("missing room code")?);
    let code = rest.get(1..=length).ok_or("short room code")?;
    Ok((
        String::from_utf8_lossy(code).into_owned(),
        &rest[1 + length..],
    ))
}

/// Compares secrets in time that does not depend on where they differ.
fn same_secret(given: &[u8], expected: &[u8]) -> bool {
    given.len() == expected.len()
        && given
            .iter()
            .zip(expected)
            .fold(0, |difference, (a, b)| difference | (a ^ b))
            == 0
}

fn data_from_frame(kind: u8, peer: PeerId, bytes: Vec<u8>) -> Option<NetEvent> {
    (kind == DATA).then_some(NetEvent::Message { from: peer, bytes })
}

/// Reads frames until the stream ends, sending the events `event` makes.
fn read_frames(
    mut stream: TcpStream,
    event: impl Fn(u8, PeerId, Vec<u8>) -> Option<NetEvent>,
    sender: &Sender<NetEvent>,
) {
    while let Ok((kind, peer, bytes)) = read_frame(&mut stream, MAX_MESSAGE) {
        if let Some(event) = event(kind, peer, bytes) {
            if sender.send(event).is_err() {
                break;
            }
        }
    }
    let _ = stream.shutdown(Shutdown::Both);
}

/// A frame is a little-endian u32 length of the rest, a kind byte, a
/// little-endian u32 peer id and the payload.
fn write_frame(
    stream: &mut impl Write,
    kind: u8,
    peer: PeerId,
    payload: &[u8],
) -> io::Result<()> {
    stream.write_all(&encode_frame(kind, peer, payload)?)
}

fn encode_frame(kind: u8, peer: PeerId, payload: &[u8]) -> io::Result<Vec<u8>> {
    if payload.len() > MAX_MESSAGE {
        return Err(invalid("message is larger than MAX_MESSAGE"));
    }
    let mut frame = Vec::with_capacity(9 + payload.len());
    frame.extend(u32::try_from(5 + payload.len()).unwrap().to_le_bytes());
    frame.push(kind);
    frame.extend(peer.to_le_bytes());
    frame.extend(payload);
    Ok(frame)
}

fn read_frame(
    stream: &mut impl Read,
    max: usize,
) -> io::Result<(u8, PeerId, Vec<u8>)> {
    let mut length = [0; 4];
    stream.read_exact(&mut length)?;
    let length = u32::from_le_bytes(length) as usize;
    if !(5..=max + 5).contains(&length) {
        return Err(invalid("bad frame length"));
    }
    // Grown as bytes arrive, so a length prefix alone cannot reserve
    // megabytes.
    let mut frame = Vec::new();
    stream.take(length as u64).read_to_end(&mut frame)?;
    if frame.len() < length {
        return Err(io::ErrorKind::UnexpectedEof.into());
    }
    let peer = PeerId::from_le_bytes(frame[1..5].try_into().unwrap());
    Ok((frame[0], peer, frame.split_off(5)))
}

/// Reads one handshake frame, small and whole within `HANDSHAKE_TIMEOUT`,
/// so a peer trickling bytes cannot hold a join slot. Clears the read
/// timeout after.
fn read_handshake(stream: &TcpStream) -> io::Result<(u8, PeerId, Vec<u8>)> {
    let mut reader = Deadline {
        stream,
        until: Instant::now() + HANDSHAKE_TIMEOUT,
    };
    let frame = read_frame(&mut reader, MAX_HANDSHAKE);
    stream.set_read_timeout(None)?;
    frame
}

/// A stream whose reads fail once `until` has passed.
struct Deadline<'a> {
    stream: &'a TcpStream,
    until: Instant,
}

impl Read for Deadline<'_> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let left = self.until.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(io::ErrorKind::TimedOut.into());
        }
        self.stream.set_read_timeout(Some(left))?;
        let mut stream = self.stream;
        // Unix reports a read timeout as WouldBlock.
        stream.read(buffer).map_err(|error| match error.kind() {
            io::ErrorKind::WouldBlock => io::ErrorKind::TimedOut.into(),
            _ => error,
        })
    }
}

/// What the relay's per-address limits count: an IPv4 address, or an IPv6
/// /64, since one home connection gets a whole /64.
fn limit_key(address: IpAddr) -> IpAddr {
    match address {
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => IpAddr::V4(v4),
            None => {
                IpAddr::V6(Ipv6Addr::from(u128::from(v6) & (u128::MAX << 64)))
            }
        },
        v4 => v4,
    }
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.to_owned())
}

fn rejected(reason: &[u8]) -> io::Error {
    io::Error::new(
        io::ErrorKind::ConnectionRefused,
        String::from_utf8_lossy(reason).into_owned(),
    )
}

#[cfg(test)]
mod tests {
    #[test]
    fn loopback_sessions_act_like_a_host_and_its_clients() {
        let (host, mut clients) = NetSession::loopback(2);
        assert_eq!(host.peers(), vec![1, 2]);
        assert_eq!(
            host.poll(),
            vec![NetEvent::Connected(1), NetEvent::Connected(2)]
        );
        clients[1].send(HOST, b"hi").unwrap();
        assert!(clients[1].send(1, b"no").is_err());
        assert_eq!(
            host.poll(),
            vec![NetEvent::Message {
                from: 2,
                bytes: b"hi".to_vec()
            }]
        );
        host.broadcast(b"all").unwrap();
        for client in &clients {
            let all = NetEvent::Message {
                from: HOST,
                bytes: b"all".to_vec(),
            };
            assert_eq!(client.poll(), vec![all]);
        }
        drop(clients.remove(0));
        assert_eq!(host.poll(), vec![NetEvent::Disconnected(1)]);
        assert_eq!(host.peers(), vec![2]);
        assert!(host.send(1, b"gone").is_err());
        drop(host);
        assert_eq!(clients[0].poll(), vec![NetEvent::Closed]);
    }

    use super::*;
    use std::time::Instant;

    /// Polls until `count` events arrive or two seconds pass.
    fn wait(session: &NetSession, count: usize) -> Vec<NetEvent> {
        let start = Instant::now();
        let mut events = Vec::new();
        while events.len() < count && start.elapsed() < Duration::from_secs(2) {
            events.extend(session.poll());
            std::thread::sleep(Duration::from_millis(5));
        }
        events
    }

    fn message(from: PeerId, text: &str) -> NetEvent {
        NetEvent::Message {
            from,
            bytes: text.as_bytes().to_vec(),
        }
    }

    #[test]
    fn unreliable_messages_go_over_udp_and_loss_drops_them() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let host = NetSession::host_on(listener, "").unwrap();
        let client = NetSession::join(address, "").unwrap();
        assert_eq!(wait(&host, 1), [NetEvent::Connected(1)]);

        client.send_unreliable(HOST, b"pos").unwrap();
        assert_eq!(wait(&host, 1), [message(1, "pos")]);
        // The datagram taught the host the client's address.
        host.broadcast_unreliable(b"tick").unwrap();
        assert_eq!(wait(&client, 1), [message(HOST, "tick")]);
        assert_eq!(host.stats().bytes_sent, 4 + DATAGRAM_HEADER as u64);
        assert!(host.send_unreliable(1, &[0; MAX_UNRELIABLE + 1]).is_err());

        host.simulate(NetConditions {
            loss: 1.0,
            ..NetConditions::default()
        });
        client.send_unreliable(HOST, b"lost").unwrap();
        std::thread::sleep(Duration::from_millis(100));
        assert!(host.poll().is_empty());
        assert_eq!(host.stats().held, 0, "lost, not resent");

        // The right token from another IP address is ignored.
        let datagrams = host.datagrams.as_ref().unwrap();
        let (token, ..) = datagrams.peers.lock().unwrap()[&1];
        if let Ok(stranger) = UdpSocket::bind("127.0.0.2:0") {
            let mut datagram = token.to_le_bytes().to_vec();
            datagram.extend(1u32.to_le_bytes());
            datagram.extend(b"evil");
            stranger.send_to(&datagram, address).unwrap();
            std::thread::sleep(Duration::from_millis(100));
            let peers = datagrams.peers.lock().unwrap();
            assert_eq!(peers[&1].2.unwrap().ip(), address.ip());
        }

        // No UDP route in a loopback session: it goes reliably.
        let (host, clients) = NetSession::loopback(1);
        host.poll();
        clients[0].send_unreliable(HOST, b"pos").unwrap();
        assert_eq!(host.poll(), [message(1, "pos")]);
    }

    #[test]
    fn simulated_conditions_delay_events_in_order() {
        let ms = Duration::from_millis;
        let mut lab = Lab {
            conditions: NetConditions {
                latency: ms(100),
                jitter: ms(50),
                loss: 0.0,
                seed: 7,
            },
            ..Lab::default()
        };
        let start = Instant::now();
        let arrived = vec![message(1, "a"), message(1, "b")];
        assert!(lab.deliver(arrived, start).is_empty());
        assert!(lab.deliver(Vec::new(), start + ms(99)).is_empty());
        assert_eq!(
            lab.deliver(Vec::new(), start + ms(150)),
            [message(1, "a"), message(1, "b")]
        );
        assert_eq!(lab.stats.messages_received, 2);
        assert_eq!(lab.stats.bytes_received, 2 * 10);

        // Every message lost: each waits one more round trip, and none
        // passes the one before it.
        lab.conditions.loss = 1.0;
        let later = start + ms(1000);
        let arrived = (0..20).map(|i| message(1, &i.to_string())).collect();
        assert!(lab.deliver(arrived, later).is_empty());
        assert!(lab.deliver(Vec::new(), later + ms(299)).is_empty());
        let due = lab.deliver(Vec::new(), later + ms(350));
        let expected: Vec<_> =
            (0..20).map(|i| message(1, &i.to_string())).collect();
        assert_eq!(due, expected);

        // The same seed gives the same delays.
        let delays = |seed| {
            let mut lab = Lab {
                rng: seed,
                ..Lab::default()
            };
            lab.conditions.jitter = ms(50);
            lab.conditions.loss = 0.5;
            (0..8).map(|_| lab.delay()).collect::<Vec<_>>()
        };
        assert_eq!(delays(3), delays(3));
        assert_ne!(delays(3), delays(4));
    }

    #[test]
    fn a_pinned_clock_times_simulated_delays() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let host = NetSession::host_on(listener, "").unwrap();
        let client = NetSession::join(address, "").unwrap();
        assert_eq!(wait(&host, 1), [NetEvent::Connected(1)]);
        host.simulate(NetConditions {
            latency: Duration::from_millis(1000),
            ..NetConditions::default()
        });

        let start = Instant::now();
        pin_clock(Some(start));
        client.send(HOST, b"late").unwrap();
        while host.stats().held == 0 && start.elapsed() < Duration::from_secs(2)
        {
            assert!(host.poll().is_empty());
        }
        pin_clock(Some(start + Duration::from_millis(500)));
        assert!(host.poll().is_empty());
        // Only the pinned clock moved past the delay, not the wall clock.
        pin_clock(Some(start + Duration::from_millis(1500)));
        let due = host.poll();
        pin_clock(None);
        assert!(start.elapsed() < Duration::from_secs(1), "too slow");
        assert_eq!(due, [message(1, "late")]);
    }

    #[test]
    fn a_session_counts_traffic_and_holds_messages_under_latency() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let host = NetSession::host_on(listener, "").unwrap();
        let client = NetSession::join(address, "").unwrap();
        assert_eq!(wait(&host, 1), [NetEvent::Connected(1)]);

        host.simulate(NetConditions {
            latency: Duration::from_millis(300),
            ..NetConditions::default()
        });
        client.send(HOST, b"hello").unwrap();
        std::thread::sleep(Duration::from_millis(50));
        assert!(host.poll().is_empty(), "the message waits out its latency");
        assert_eq!(host.stats().held, 1);
        assert_eq!(wait(&host, 1), [message(1, "hello")]);

        host.broadcast(b"tick").unwrap();
        assert_eq!(wait(&client, 1), [message(HOST, "tick")]);
        let sent = NetStats {
            messages_sent: 1,
            bytes_sent: 14,
            ..NetStats::default()
        };
        assert_eq!(
            client.stats(),
            NetStats {
                messages_received: 1,
                bytes_received: 13,
                ..sent
            }
        );
        assert_eq!(
            host.stats(),
            NetStats {
                messages_sent: 1,
                bytes_sent: 13,
                messages_received: 1,
                bytes_received: 14,
                held: 0,
            }
        );
    }

    #[test]
    fn a_direct_host_and_two_clients_exchange_messages_in_order() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let host = NetSession::host_on(listener, "").unwrap();
        let first = NetSession::join(address, "").unwrap();
        assert_eq!(wait(&host, 1), [NetEvent::Connected(1)]);
        let second = NetSession::join(address, "").unwrap();
        assert_eq!(wait(&host, 1), [NetEvent::Connected(2)]);
        assert_eq!((first.id(), second.id()), (1, 2));
        assert_eq!(host.peers(), [1, 2]);

        first.send(HOST, b"a").unwrap();
        first.send(HOST, b"b").unwrap();
        assert_eq!(wait(&host, 2), [message(1, "a"), message(1, "b")]);
        assert!(first.send(2, b"x").is_err(), "clients reach only the host");

        host.broadcast(b"tick").unwrap();
        host.send(2, b"only you").unwrap();
        assert_eq!(wait(&first, 1), [message(HOST, "tick")]);
        assert_eq!(
            wait(&second, 2),
            [message(HOST, "tick"), message(HOST, "only you")]
        );

        drop(first);
        assert_eq!(wait(&host, 1), [NetEvent::Disconnected(1)]);
        drop(host);
        assert_eq!(wait(&second, 1), [NetEvent::Closed]);
        // The port is free again for the next match.
        let start = Instant::now();
        while TcpListener::bind(address).is_err() {
            assert!(start.elapsed() < Duration::from_secs(2), "port held");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn a_relay_pairs_a_host_and_a_client_by_room_code() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let relay = listener.local_addr().unwrap();
        std::thread::spawn(move || run_relay(listener, ""));
        let host = NetSession::host_room(relay, "").unwrap();
        let code = host.room_code().unwrap().to_owned();
        assert_eq!(code.len(), 6);

        let error = NetSession::join_room(relay, "NOROOM", "").err().unwrap();
        assert_eq!(error.kind(), io::ErrorKind::ConnectionRefused);
        let client =
            NetSession::join_room(relay, &code.to_lowercase(), "").unwrap();
        assert_eq!(client.id(), 1);
        assert_eq!(wait(&host, 1), [NetEvent::Connected(1)]);

        client.send(HOST, b"ping").unwrap();
        assert_eq!(wait(&host, 1), [message(1, "ping")]);
        host.send(1, b"pong").unwrap();
        host.broadcast(b"all").unwrap();
        assert_eq!(
            wait(&client, 2),
            [message(HOST, "pong"), message(HOST, "all")]
        );

        drop(client);
        assert_eq!(wait(&host, 1), [NetEvent::Disconnected(1)]);
    }

    #[test]
    fn a_joining_client_receives_the_hosts_world_seed() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let host = NetSession::host_on(listener, "").unwrap();
        let early = NetSession::join(address, "").unwrap();
        assert_eq!(early.seed(), None, "joined before the host set one");
        host.set_seed(0xDEAD_BEEF_0042).unwrap();
        let client = NetSession::join(address, "").unwrap();
        assert_eq!(client.seed(), Some(0xDEAD_BEEF_0042));
        assert!(client.datagrams.is_some(), "the token still arrives");
        assert!(client.set_seed(1).is_err(), "only the host sets it");
        assert_eq!(host.seed(), Some(0xDEAD_BEEF_0042));

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let relay = listener.local_addr().unwrap();
        std::thread::spawn(move || run_relay(listener, ""));
        let host = NetSession::host_room(relay, "").unwrap();
        host.set_seed(7).unwrap();
        // The relay reads the seed on the host's thread; joins race it.
        let start = Instant::now();
        let client = loop {
            let code = host.room_code().unwrap();
            let client = NetSession::join_room(relay, code, "").unwrap();
            if client.seed().is_some() || start.elapsed().as_secs() > 2 {
                break client;
            }
        };
        assert_eq!(client.seed(), Some(7));

        let (host, clients) = NetSession::loopback(1);
        host.set_seed(9).unwrap();
        assert_eq!(clients[0].seed(), Some(9));

        for (token, seed) in [(None, None), (Some(3), None), (None, Some(4))] {
            let payload = welcome_payload(token, seed);
            assert_eq!(parse_welcome(&payload), Some((token, seed)));
        }
        assert_eq!(parse_welcome(&[2, 1, 2, 3]), None, "short seed");
        assert_eq!(parse_welcome(&[]), None);
    }

    #[test]
    fn a_host_whose_udp_port_is_taken_sends_unreliable_messages_reliably() {
        // Find a port free for TCP whose UDP twin we can hold.
        let (listener, _udp) = loop {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            if let Ok(udp) = UdpSocket::bind(listener.local_addr().unwrap()) {
                break (listener, udp);
            }
        };
        let address = listener.local_addr().unwrap();
        let host = NetSession::host_on(listener, "").unwrap();
        assert!(host.datagrams.is_none());
        let client = NetSession::join(address, "").unwrap();
        assert!(client.datagrams.is_none());
        assert_eq!(wait(&host, 1), [NetEvent::Connected(1)]);
        client.send_unreliable(HOST, b"up").unwrap();
        let up = NetEvent::Message {
            from: 1,
            bytes: b"up".to_vec(),
        };
        assert_eq!(wait(&host, 1), [up]);
        host.send_unreliable(1, b"down").unwrap();
        let down = NetEvent::Message {
            from: HOST,
            bytes: b"down".to_vec(),
        };
        assert_eq!(wait(&client, 1), [down]);
    }

    #[test]
    fn a_host_refuses_another_protocol_version() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let direct = NetSession::host_on(listener, "").unwrap();
        let mut stream = TcpStream::connect(address).unwrap();
        write_frame(&mut stream, JOIN, HOST, &99u16.to_le_bytes()).unwrap();
        let (kind, _, reason) = read_frame(&mut stream, MAX_MESSAGE).unwrap();
        assert_eq!(kind, REJECT);
        assert!(String::from_utf8_lossy(&reason).contains("99"));
        assert!(direct.poll().is_empty(), "a refused client never joins");

        // A relay refuses an old host and an old client the same way, and
        // a payload too short to hold a version.
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let relay = listener.local_addr().unwrap();
        std::thread::spawn(move || run_relay(listener, ""));
        let host = NetSession::host_room(relay, "").unwrap();
        let code = host.room_code().unwrap().to_owned();
        let mut old_join = 99u16.to_le_bytes().to_vec();
        old_join.push(u8::try_from(code.len()).unwrap());
        old_join.extend(code.as_bytes());
        for (kind, payload, expected) in [
            (HOST_ROOM, 99u16.to_le_bytes().to_vec(), "99"),
            (JOIN, old_join, "99"),
            (JOIN, vec![1], "missing protocol version"),
        ] {
            let mut stream = TcpStream::connect(relay).unwrap();
            write_frame(&mut stream, kind, HOST, &payload).unwrap();
            let (answer, _, reason) =
                read_frame(&mut stream, MAX_MESSAGE).unwrap();
            assert_eq!(answer, REJECT);
            let reason = String::from_utf8_lossy(&reason);
            assert!(reason.contains(expected), "{reason}");
        }
        assert!(host.poll().is_empty(), "a refused client never joins");
    }

    #[test]
    fn a_host_and_a_relay_refuse_the_wrong_secret() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let host = NetSession::host_on(listener, "raft").unwrap();
        for wrong in ["", "rafts", "Raft"] {
            let error = NetSession::join(address, wrong).err().unwrap();
            assert_eq!(error.kind(), io::ErrorKind::ConnectionRefused);
            assert_eq!(error.to_string(), "wrong password");
        }
        let client = NetSession::join(address, "raft").unwrap();
        assert_eq!(client.id(), 1);
        assert_eq!(wait(&host, 1), [NetEvent::Connected(1)]);

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let relay = listener.local_addr().unwrap();
        std::thread::spawn(move || run_relay(listener, "s3cret"));
        let error = NetSession::host_room(relay, "").err().unwrap();
        assert_eq!(error.to_string(), "wrong relay token");
        let host = NetSession::host_room(relay, "s3cret").unwrap();
        let code = host.room_code().unwrap().to_owned();
        let error = NetSession::join_room(relay, &code, "nope").err().unwrap();
        assert_eq!(error.to_string(), "wrong relay token");
        assert!(host.poll().is_empty(), "a refused client never joins");
        let client = NetSession::join_room(relay, &code, "s3cret").unwrap();
        assert_eq!(client.id(), 1);
        assert_eq!(wait(&host, 1), [NetEvent::Connected(1)]);
    }

    #[test]
    fn a_silent_connection_does_not_delay_another_join() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let host = NetSession::host_on(listener, "").unwrap();
        let _silent = TcpStream::connect(address).unwrap();
        std::thread::sleep(Duration::from_millis(50));
        let start = Instant::now();
        let client = NetSession::join(address, "").unwrap();
        assert!(start.elapsed() < Duration::from_secs(1));
        assert_eq!(client.id(), 1);
        assert_eq!(wait(&host, 1), [NetEvent::Connected(1)]);
    }

    #[test]
    fn a_handshake_must_arrive_whole_in_time_and_stay_small() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        // One byte every 50 ms never trips a per-read timeout; the
        // deadline still ends the handshake.
        std::thread::spawn(move || {
            let mut stream = TcpStream::connect(address).unwrap();
            for byte in 64u32.to_le_bytes().into_iter().cycle() {
                if stream.write_all(&[byte]).is_err() {
                    return;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        });
        let (stream, _) = listener.accept().unwrap();
        let start = Instant::now();
        let mut reader = Deadline {
            stream: &stream,
            until: start + Duration::from_millis(400),
        };
        let error = read_frame(&mut reader, MAX_HANDSHAKE).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert!(start.elapsed() < Duration::from_secs(1));

        // A frame longer than a handshake is refused from its length.
        let mut sender = TcpStream::connect(address).unwrap();
        let (stream, _) = listener.accept().unwrap();
        sender
            .write_all(&(MAX_HANDSHAKE as u32 + 6).to_le_bytes())
            .unwrap();
        let start = Instant::now();
        let error = read_handshake(&stream).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert!(start.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn relay_limits_count_an_ipv6_slash_64_as_one_address() {
        let key = |text: &str| limit_key(text.parse().unwrap());
        assert_eq!(key("2001:db8:1:2::1"), key("2001:db8:1:2:ffff::9"));
        assert_ne!(key("2001:db8:1:2::1"), key("2001:db8:1:3::1"));
        assert_eq!(key("::ffff:203.0.113.7"), key("203.0.113.7"));
        assert_ne!(key("203.0.113.7"), key("203.0.113.8"));
    }

    #[test]
    fn a_relay_limits_connections_per_address() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let relay = listener.local_addr().unwrap();
        let limits = RelayLimits {
            max_connections_per_address: 2,
            connects_per_minute: 4,
            ..RelayLimits::default()
        };
        std::thread::spawn(move || run_relay_with(listener, "", limits));
        let host = NetSession::host_room(relay, "").unwrap();
        let code = host.room_code().unwrap().to_owned();
        let client = NetSession::join_room(relay, &code, "").unwrap();
        // A third open connection from the same address is closed.
        assert!(NetSession::join_room(relay, &code, "").is_err());
        drop(client);
        assert_eq!(
            wait(&host, 2),
            [NetEvent::Connected(1), NetEvent::Disconnected(1)]
        );
        // A slot is free again, but this is the address's fourth connect
        // this minute and the fifth is over the rate.
        let client = NetSession::join_room(relay, &code, "").unwrap();
        drop(client);
        assert_eq!(
            wait(&host, 2),
            [NetEvent::Connected(2), NetEvent::Disconnected(2)]
        );
        assert!(NetSession::join_room(relay, &code, "").is_err());
    }

    #[test]
    fn a_throttle_allows_one_seconds_burst_then_refuses() {
        let mut throttle = Throttle::new(100_000);
        assert!(throttle.spend(100_000));
        assert!(!throttle.spend(30_000));
        // No cap at all.
        assert!(Throttle::new(0).spend(usize::MAX));
    }

    #[test]
    fn a_relay_drops_a_sender_over_its_rate() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let relay = listener.local_addr().unwrap();
        let limits = RelayLimits {
            bytes_per_second: 2000,
            ..RelayLimits::default()
        };
        std::thread::spawn(move || run_relay_with(listener, "", limits));
        let host = NetSession::host_room(relay, "").unwrap();
        let code = host.room_code().unwrap().to_owned();
        let client = NetSession::join_room(relay, &code, "").unwrap();
        assert_eq!(wait(&host, 1), [NetEvent::Connected(1)]);
        client.send(HOST, &[7; 1000]).unwrap();
        client.send(HOST, &[7; 1500]).unwrap();
        let events = wait(&host, 2);
        assert_eq!(events.len(), 2, "{events:?}");
        assert_eq!(events[1], NetEvent::Disconnected(1));
        // A message larger than one burst is refused before it is buffered.
        let client = NetSession::join_room(relay, &code, "").unwrap();
        assert_eq!(wait(&host, 1), [NetEvent::Connected(2)]);
        client.send(HOST, &[7; 4000]).unwrap();
        assert_eq!(wait(&host, 1), [NetEvent::Disconnected(2)]);
    }

    #[test]
    fn a_relay_caps_rooms_clients_and_room_attempts() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let relay = listener.local_addr().unwrap();
        let limits = RelayLimits {
            max_rooms: 1,
            max_clients_per_room: 1,
            room_attempts_per_minute: 4,
            ..RelayLimits::default()
        };
        std::thread::spawn(move || run_relay_with(listener, "", limits));
        let host = NetSession::host_room(relay, "").unwrap();
        let code = host.room_code().unwrap().to_owned();
        let error = NetSession::host_room(relay, "").err().unwrap();
        assert!(error.to_string().contains("full"), "{error}");
        let _client = NetSession::join_room(relay, &code, "").unwrap();
        let error = NetSession::join_room(relay, &code, "").err().unwrap();
        assert!(error.to_string().contains("full"), "{error}");
        // Four room opens and joins this minute were used; the fifth is
        // refused before it reaches a room.
        let error = NetSession::join_room(relay, "NOROOM", "").err().unwrap();
        assert!(!error.to_string().contains("no room"), "{error}");
    }

    #[test]
    fn a_relay_closes_a_connection_idle_after_its_handshake() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let relay = listener.local_addr().unwrap();
        let limits = RelayLimits {
            idle_timeout: Duration::from_millis(200),
            ..RelayLimits::default()
        };
        std::thread::spawn(move || run_relay_with(listener, "", limits));
        let host = NetSession::host_room(relay, "").unwrap();
        assert_eq!(wait(&host, 1), [NetEvent::Closed]);
    }

    #[test]
    fn a_peer_that_stops_reading_stalls_only_itself() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let relay = listener.local_addr().unwrap();
        let limits = RelayLimits {
            bytes_per_second: 0,
            send_queue: 256 << 10,
            ..RelayLimits::default()
        };
        std::thread::spawn(move || run_relay_with(listener, "", limits));
        // Room A: a client that joins and then never reads.
        let flooder = NetSession::host_room(relay, "").unwrap();
        let code = flooder.room_code().unwrap().to_owned();
        let stuck = TcpStream::connect(relay).unwrap();
        request_join(&stuck, &code, "").unwrap();
        assert_eq!(wait(&flooder, 1), [NetEvent::Connected(1)]);
        // Room B: an ordinary pair.
        let host = NetSession::host_room(relay, "").unwrap();
        let code = host.room_code().unwrap().to_owned();
        let client = NetSession::join_room(relay, &code, "").unwrap();
        assert_eq!(wait(&host, 1), [NetEvent::Connected(1)]);
        // Far more than the stuck client's socket buffers hold.
        let flood = std::thread::spawn(move || {
            for _ in 0..400 {
                if flooder.send(EVERYONE, &[1; 64 << 10]).is_err() {
                    break;
                }
            }
            flooder
        });
        std::thread::sleep(Duration::from_millis(300));
        let start = Instant::now();
        client.send(HOST, b"hi").unwrap();
        assert_eq!(wait(&host, 1), [message(1, "hi")]);
        assert!(start.elapsed() < Duration::from_millis(500));
        // The stuck client was cut off once its queue filled.
        let flooder = flood.join().unwrap();
        let events = wait(&flooder, 1);
        assert_eq!(events, [NetEvent::Disconnected(1)]);
        drop(stuck);
    }

    #[test]
    fn a_client_flooding_a_slow_host_is_cut_off_not_the_room_or_others() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let relay = listener.local_addr().unwrap();
        let limits = RelayLimits {
            bytes_per_second: 0,
            send_queue: 256 << 10,
            max_clients_per_room: 4,
            ..RelayLimits::default()
        };
        std::thread::spawn(move || run_relay_with(listener, "", limits));
        // A host that opens a room and then never reads.
        let mut host = TcpStream::connect(relay).unwrap();
        write_frame(
            &mut host,
            HOST_ROOM,
            HOST,
            &PROTOCOL_VERSION.to_le_bytes(),
        )
        .unwrap();
        let (kind, _, code) = read_handshake(&host).unwrap();
        assert_eq!(kind, ROOM);
        let code = String::from_utf8(code).unwrap();
        let honest = NetSession::join_room(relay, &code, "").unwrap();
        let flooder = NetSession::join_room(relay, &code, "").unwrap();
        for _ in 0..400 {
            if flooder.send(HOST, &[1; 64 << 10]).is_err() {
                break;
            }
        }
        assert_eq!(wait(&flooder, 1), [NetEvent::Closed]);
        // A client within its share of the host's queue stays, and the
        // room is still open.
        honest.send(HOST, &[2; 64 << 10]).unwrap();
        std::thread::sleep(Duration::from_millis(200));
        assert_eq!(honest.poll(), []);
        NetSession::join_room(relay, &code, "").unwrap();
    }

    #[test]
    fn rpcs_replication_and_prediction_survive_a_lossy_loopback() {
        use crate::runtime::{SceneComponentRegistry, SceneId};
        use crate::Transform;
        use bevy_ecs::prelude::World;
        use predict::Prediction;
        use replicate::{Check, Replica, Replicated, Replication, TRANSFORM};
        use rpc::{Authority, Reliability, Rpcs};

        let (host, clients) = NetSession::loopback(1);
        let client = &clients[0];
        host.poll();
        // Both ends lose a third of their messages, which come back one
        // round trip later, with jitter on top.
        for (end, seed) in [(&host, 1), (client, 2)] {
            end.simulate(NetConditions {
                latency: Duration::from_millis(2),
                jitter: Duration::from_millis(3),
                loss: 0.33,
                seed,
            });
        }
        let mut rpcs = Rpcs::new();
        rpcs.register("move", Authority::Owner, Reliability::Reliable);
        rpcs.set_owner("Hero", 1);
        let mut replication = Replication::new();
        replication.component(TRANSFORM);
        let mut replica = Replica::new(&replication);
        let world = || {
            let mut world = World::new();
            world.insert_resource(SceneComponentRegistry::default());
            world
        };
        let (mut host_world, mut client_world) = (world(), world());
        let hero = host_world
            .spawn((Replicated, SceneId::new(), Transform::default()))
            .id();

        // The client predicts its hero's x from its own moves; the host
        // applies the same moves and answers with the tick and x.
        let step = |x: &mut f32, dx: &f32| *x += dx;
        let mut prediction = Prediction::new();
        let (mut predicted, mut host_x, mut moves) = (0.0_f32, 0.0_f32, 0);
        let mut reconciled = 0.0_f32;
        for tick in 1..=60_u64 {
            if tick <= 40 {
                let dx = (tick % 3) as f32 - 0.5;
                prediction.push(tick, dx);
                step(&mut predicted, &dx);
                let args =
                    [tick.to_le_bytes(), u64::from(dx.to_bits()).to_le_bytes()]
                        .concat();
                rpcs.call(client, "Hero", "move", &args).unwrap();
            }
            host_world.get_mut::<Transform>(hero).unwrap().position[0] += 0.25;
            replication.snapshot(&mut host_world).unwrap();
            host.send_unreliable(1, &replication.snapshot_for(1).unwrap())
                .unwrap();
            for event in host.poll() {
                let NetEvent::Message { from, bytes } = event else {
                    continue;
                };
                if let Some(ack) = replication.accept(from, &bytes) {
                    ack.unwrap();
                    continue;
                }
                let call = rpcs.accept(from, &bytes).unwrap().unwrap();
                let (move_tick, dx) = call.args.split_at(8);
                let move_tick =
                    u64::from_le_bytes(move_tick.try_into().unwrap());
                let dx = f32::from_bits(u64::from_le_bytes(
                    dx.try_into().unwrap(),
                ) as u32);
                moves += 1;
                assert_eq!(move_tick, moves, "moves arrive once and in order");
                step(&mut host_x, &dx);
                let state = [
                    move_tick.to_le_bytes(),
                    u64::from(host_x.to_bits()).to_le_bytes(),
                ]
                .concat();
                host.send(1, &state).unwrap();
            }
            for event in client.poll() {
                let NetEvent::Message { from, bytes } = event else {
                    continue;
                };
                if let Some(applied) =
                    replica.apply(&mut client_world, from, &bytes)
                {
                    applied.unwrap();
                    client.send(HOST, &replica.ack().unwrap()).unwrap();
                    continue;
                }
                let acked = u64::from_le_bytes(bytes[..8].try_into().unwrap());
                let x = f32::from_bits(u64::from_le_bytes(
                    bytes[8..].try_into().unwrap(),
                ) as u32);
                reconciled = prediction.reconcile(acked, x, step);
                assert_eq!(reconciled, predicted, "tick {tick}");
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        // Let the held messages arrive, then compare.
        let start = Instant::now();
        while (moves < 40 || prediction.pending().next().is_some())
            && start.elapsed() < Duration::from_secs(5)
        {
            for event in host.poll() {
                let NetEvent::Message { from, bytes } = event else {
                    continue;
                };
                if replication.accept(from, &bytes).is_some() {
                    continue;
                }
                let call = rpcs.accept(from, &bytes).unwrap().unwrap();
                let dx = f32::from_bits(u64::from_le_bytes(
                    call.args[8..].try_into().unwrap(),
                ) as u32);
                moves += 1;
                step(&mut host_x, &dx);
                let state = [
                    moves.to_le_bytes(),
                    u64::from(host_x.to_bits()).to_le_bytes(),
                ]
                .concat();
                host.send(1, &state).unwrap();
            }
            for event in client.poll() {
                let NetEvent::Message { from, bytes } = event else {
                    continue;
                };
                if replica.apply(&mut client_world, from, &bytes).is_some() {
                    continue;
                }
                let acked = u64::from_le_bytes(bytes[..8].try_into().unwrap());
                let x = f32::from_bits(u64::from_le_bytes(
                    bytes[8..].try_into().unwrap(),
                ) as u32);
                reconciled = prediction.reconcile(acked, x, step);
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(moves, 40);
        assert_eq!((reconciled, host_x), (predicted, predicted));
        let stats = host.stats();
        assert_eq!(stats.held, 0);

        // The last snapshot gets through once the host sends it again.
        let checksum = replication.checksum();
        host.send(1, &replication.snapshot_for(1).unwrap()).unwrap();
        host.send(1, &checksum).unwrap();
        let events = wait(client, 2);
        let mut verified = None;
        for event in events {
            let NetEvent::Message { from, bytes } = event else {
                continue;
            };
            if let Some(applied) =
                replica.apply(&mut client_world, from, &bytes)
            {
                applied.unwrap();
            } else {
                verified = replica.verify(&mut client_world, from, &bytes);
            }
        }
        assert_eq!(verified, Some(Ok(Check::Match)));
    }

    #[test]
    fn a_client_that_drops_mid_match_resynchronizes_on_rejoin() {
        use crate::runtime::{Name, SceneComponentRegistry, SceneId};
        use crate::Transform;
        use bevy_ecs::prelude::{Entity, With, World};
        use replicate::{
            Check, Replica, Replicated, Replication, NAME, TRANSFORM,
        };

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let host = NetSession::host_on(listener, "").unwrap();
        host.simulate(NetConditions {
            loss: 0.2,
            seed: 3,
            ..NetConditions::default()
        });
        let mut replication = Replication::new();
        replication.component(TRANSFORM).component(NAME);
        let world = || {
            let mut world = World::new();
            world.insert_resource(SceneComponentRegistry::default());
            world
        };
        let (mut host_world, mut client_world) = (world(), world());
        let spawn = |world: &mut World, name: &str| {
            world
                .spawn((
                    Replicated,
                    SceneId::new(),
                    Name(name.into()),
                    Transform::default(),
                ))
                .id()
        };
        let a = spawn(&mut host_world, "A");
        let b = spawn(&mut host_world, "B");
        spawn(&mut host_world, "C");

        // One host tick: move everything, send each client its snapshot,
        // and take acknowledgements and departures.
        let tick = |replication: &mut Replication, world: &mut World| {
            let mut query =
                world.query_filtered::<&mut Transform, With<Replicated>>();
            for mut transform in query.iter_mut(world) {
                transform.position[0] += 1.0;
            }
            replication.snapshot(world).unwrap();
            for peer in host.peers() {
                let bytes = replication.snapshot_for(peer).unwrap();
                host.send_unreliable(peer, &bytes).unwrap();
            }
            let mut left = Vec::new();
            for event in host.poll() {
                match event {
                    NetEvent::Message { from, bytes } => {
                        replication.accept(from, &bytes).unwrap().unwrap();
                    }
                    NetEvent::Disconnected(peer) => {
                        replication.forget(peer);
                        left.push(peer);
                    }
                    _ => {}
                }
            }
            std::thread::sleep(Duration::from_millis(2));
            left
        };
        let pump =
            |client: &NetSession, replica: &mut Replica, world: &mut World| {
                for event in client.poll() {
                    if let NetEvent::Message { from, bytes } = event {
                        replica.apply(world, from, &bytes).unwrap().unwrap();
                        client
                            .send_unreliable(HOST, &replica.ack().unwrap())
                            .unwrap();
                    }
                }
            };
        // Sends the latest snapshot and a checksum reliably, then checks.
        let settle = |replication: &mut Replication,
                      client: &NetSession,
                      replica: &mut Replica,
                      world: &mut World| {
            let peer = client.id();
            host.send(peer, &replication.snapshot_for(peer).unwrap())
                .unwrap();
            host.send(peer, &replication.checksum()).unwrap();
            let start = Instant::now();
            loop {
                assert!(
                    start.elapsed() < Duration::from_secs(2),
                    "no checksum"
                );
                for event in client.poll() {
                    let NetEvent::Message { from, bytes } = event else {
                        continue;
                    };
                    if let Some(check) = replica.verify(world, from, &bytes) {
                        return check.unwrap();
                    }
                    replica.apply(world, from, &bytes).unwrap().unwrap();
                }
                std::thread::sleep(Duration::from_millis(5));
            }
        };
        let names = |world: &mut World| {
            let mut query =
                world.query_filtered::<(&Name, &Transform), With<Replicated>>();
            let mut names: Vec<_> = query
                .iter(world)
                .map(|(name, transform)| {
                    (name.0.clone(), transform.position[0])
                })
                .collect();
            names.sort_by(|x, y| x.0.cmp(&y.0));
            names
        };

        let client = NetSession::join(address, "").unwrap();
        assert_eq!(wait(&host, 1), [NetEvent::Connected(1)]);
        let mut replica = Replica::new(&replication);
        for _ in 0..20 {
            tick(&mut replication, &mut host_world);
            pump(&client, &mut replica, &mut client_world);
        }
        assert_eq!(
            settle(&mut replication, &client, &mut replica, &mut client_world),
            Check::Match
        );

        // The client drops; the match goes on without it.
        drop(client);
        let start = Instant::now();
        while tick(&mut replication, &mut host_world) != [1] {
            assert!(start.elapsed() < Duration::from_secs(2), "no departure");
        }
        host_world.despawn(b);
        spawn(&mut host_world, "D");
        for _ in 0..10 {
            tick(&mut replication, &mut host_world);
        }
        // Meanwhile the client's own copy drifts.
        let stale: Vec<Entity> = client_world
            .query_filtered::<Entity, With<Replicated>>()
            .iter(&client_world)
            .collect();
        client_world
            .get_mut::<Transform>(stale[0])
            .unwrap()
            .position[1] = 9.0;

        // It rejoins as a new peer with the world and replica it kept; the
        // first snapshot it gets is whole and replaces what it had.
        let client = NetSession::join(address, "").unwrap();
        assert_eq!(client.id(), 2);
        for _ in 0..20 {
            tick(&mut replication, &mut host_world);
            pump(&client, &mut replica, &mut client_world);
        }
        assert_eq!(
            settle(&mut replication, &client, &mut replica, &mut client_world),
            Check::Match
        );
        let synced = names(&mut client_world);
        let kept: Vec<_> =
            synced.iter().map(|(name, _)| name.as_str()).collect();
        assert_eq!(kept, ["A", "C", "D"]);
        assert_eq!(
            synced[0].1,
            host_world.get::<Transform>(a).unwrap().position[0]
        );
        assert_eq!(names(&mut client_world), names(&mut host_world));
    }

    #[test]
    fn a_lockstep_match_finishes_at_200_ms_and_2_percent_loss_in_sync() {
        use input::InputBuffer;
        use lobby::{Lobby, LobbyEvent, Rejoin};
        use lockstep::CommandStream;
        use std::collections::BTreeMap;

        type State = BTreeMap<lobby::PlayerId, i64>;
        type Commands = Vec<(lobby::PlayerId, i64)>;
        const TICKS: u64 = 200;
        const INPUT: &[u8] = b"\0inp";
        let hash = |state: &State| {
            state
                .iter()
                .fold(0xcbf2_9ce4_8422_2325_u64, |hash, (id, x)| {
                    (hash ^ u64::from(*id) ^ (*x as u64).rotate_left(32))
                        .wrapping_mul(0x100_0000_01b3)
                })
        };
        let run = |state: &mut State, commands: &Commands| {
            for (player, dx) in commands {
                *state.entry(*player).or_default() += dx;
            }
        };
        // 200 ms each way, and 2% of messages lost and resent.
        let (host, clients) = NetSession::loopback(2);
        for (seed, end) in
            [&host, &clients[0], &clients[1]].into_iter().enumerate()
        {
            end.simulate(NetConditions {
                latency: Duration::from_millis(200),
                jitter: Duration::from_millis(10),
                loss: 0.02,
                seed: seed as u64,
            });
        }
        host.poll();

        // Lobby: both say hello, wait for their welcome, and get ready.
        let mut lobby = Lobby::new(Duration::from_secs(5));
        let mut welcomed: Vec<Option<Rejoin>> = vec![None; 2];
        let mut ready = 0;
        for client in &clients {
            lobby::hello(client, None).unwrap();
        }
        let start = Instant::now();
        while ready < 2 {
            assert!(start.elapsed() < Duration::from_secs(5), "lobby stuck");
            for event in host.poll() {
                if let Some(Ok(LobbyEvent::Ready(_))) =
                    lobby.handle(&host, &event)
                {
                    ready += 1;
                }
            }
            for (client, welcome) in clients.iter().zip(&mut welcomed) {
                for event in client.poll() {
                    if let NetEvent::Message { from, bytes } = event {
                        *welcome = lobby::welcome_from(from, &bytes);
                        lobby::ready(client).unwrap();
                    }
                }
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(lobby.start_match(), [1, 2]);

        // The match: clients stamp inputs 60 ticks past the last tick they
        // ran; the host runs a tick every 10 ms on whatever arrived and
        // sends its commands, which every client runs, checking the
        // host's hash first.
        let mut inputs = InputBuffer::new(1);
        let mut host_state = State::new();
        let mut streams =
            vec![CommandStream::<(lobby::PlayerId, i64)>::new(1); 2];
        let mut states = vec![State::new(); 2];
        let mut sent = [0_u64; 2];
        let (mut fresh, mut late) = (0, 0);
        let client_step =
            |streams: &mut [CommandStream<(lobby::PlayerId, i64)>],
             states: &mut [State],
             sent: &mut [u64; 2]| {
                for (index, client) in clients.iter().enumerate() {
                    for event in client.poll() {
                        if let NetEvent::Message { from, bytes } = event {
                            streams[index]
                                .accept(from, &bytes)
                                .unwrap()
                                .unwrap();
                        }
                    }
                    while let Some(tick) = streams[index].ready() {
                        assert_eq!(
                            tick.hash,
                            hash(&states[index]),
                            "desync at {}",
                            tick.tick
                        );
                        run(&mut states[index], &tick.commands);
                    }
                    // One input per tick, also for ticks a burst of
                    // commands skipped past.
                    let last = (streams[index].tick() + 60).min(TICKS);
                    for stamp in sent[index] + 1..=last {
                        let dx = (stamp + index as u64) % 3;
                        let bytes =
                            [INPUT, &stamp.to_le_bytes(), &dx.to_le_bytes()]
                                .concat();
                        client.send(HOST, &bytes).unwrap();
                    }
                    sent[index] = sent[index].max(last);
                }
            };
        for _ in 1..=TICKS {
            for event in host.poll() {
                let NetEvent::Message { from, bytes } = event else {
                    continue;
                };
                assert!(lobby.accepts_input(from));
                let body = bytes.strip_prefix(INPUT).unwrap();
                let stamp = u64::from_le_bytes(body[..8].try_into().unwrap());
                let dx = u64::from_le_bytes(body[8..].try_into().unwrap())
                    as i64
                    - 1;
                if inputs.push(from, stamp, dx).is_err() {
                    late += 1;
                }
            }
            let before = hash(&host_state);
            let (tick, taken) = inputs.take();
            fresh += taken.iter().filter(|input| input.fresh).count();
            let commands: Commands = taken
                .iter()
                .map(|input| (lobby.player(input.peer).unwrap(), input.input))
                .collect();
            lockstep::send(&host, tick, before, &commands).unwrap();
            run(&mut host_state, &commands);
            client_step(&mut streams, &mut states, &mut sent);
            std::thread::sleep(Duration::from_millis(10));
        }
        let start = Instant::now();
        while streams.iter().any(|stream| stream.tick() <= TICKS) {
            assert!(start.elapsed() < Duration::from_secs(5), "match stuck");
            client_step(&mut streams, &mut states, &mut sent);
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(states, [host_state.clone(), host_state.clone()]);
        assert_eq!(host_state.len(), 2, "both players moved");
        assert!(welcomed.iter().all(Option::is_some));
        // TCP holds every message behind a lost one for a round trip, so
        // some inputs miss their tick; the host repeats that player's last
        // input, and every peer runs the same repeat.
        assert!(fresh > 0 && late > 0, "{fresh} fresh inputs, {late} late");
    }

    #[test]
    fn the_host_rejects_positions_and_terrain_a_client_claims() {
        use crate::runtime::{Name, SceneComponentRegistry, SceneId};
        use crate::Transform;
        use bevy_ecs::prelude::World;
        use input::InputBuffer;
        use replicate::{Replicated, Replication, TRANSFORM};
        use rpc::{Authority, Reliability, Rpcs};

        let (host, clients) = NetSession::loopback(1);
        let cheat = &clients[0];
        host.poll();
        let world = || {
            let mut world = World::new();
            world.insert_resource(SceneComponentRegistry::default());
            world
        };
        let id = SceneId::new();
        let hero = |world: &mut World| {
            world
                .spawn((
                    Replicated,
                    id,
                    Name("Hero".into()),
                    Transform::default(),
                ))
                .id()
        };
        let mut host_world = world();
        let host_hero = hero(&mut host_world);
        let mut replication = Replication::new();
        replication.component(TRANSFORM);
        replication.delta(&mut host_world).unwrap();
        // Terrain changes and teleports are the host's own calls.
        let mut rpcs = Rpcs::new();
        rpcs.register("break_chunk", Authority::Host, Reliability::Reliable)
            .register("teleport", Authority::Host, Reliability::Reliable);
        let mut inputs = InputBuffer::new(1);

        // A modified client builds real replication messages of its own
        // world, with its hero far away, registers the host's methods as
        // open to anyone, and sends inputs out of range and far ahead.
        let mut forged = world();
        let fake = hero(&mut forged);
        forged.get_mut::<Transform>(fake).unwrap().position = [1e9, 0.0, 0.0];
        let mut theirs = Replication::new();
        theirs.component(TRANSFORM);
        let delta = theirs.delta(&mut forged).unwrap().unwrap();
        theirs.snapshot(&mut forged).unwrap();
        for bytes in [
            delta,
            theirs.full(),
            theirs.snapshot_for(HOST).unwrap(),
            theirs.checksum(),
        ] {
            cheat.send(HOST, &bytes).unwrap();
        }
        let mut open = Rpcs::new();
        open.register("break_chunk", Authority::Anyone, Reliability::Reliable)
            .register("teleport", Authority::Anyone, Reliability::Reliable);
        open.call(cheat, "Terrain", "break_chunk", b"0,0,0")
            .unwrap();
        open.call(cheat, "Hero", "teleport", b"1e9,0,0").unwrap();
        for (tick, stick) in
            [(1_u64, f32::NAN), (2, 50.0), (500, 1.0), (3, 0.5)]
        {
            let bytes =
                [tick.to_le_bytes().as_slice(), &stick.to_le_bytes()].concat();
            cheat.send(HOST, &bytes).unwrap();
        }

        // The host's loop, as docs/networking.md has it: replication and
        // RPCs check the sender, the game checks each input's range, and
        // the input buffer checks its tick.
        let mut refused = Vec::new();
        for event in wait(&host, 10) {
            let NetEvent::Message { from, bytes } = event else {
                continue;
            };
            if let Some(result) = replication.accept(from, &bytes) {
                refused.push(result.unwrap_err());
            } else if let Some(result) = rpcs.accept(from, &bytes) {
                refused.push(result.unwrap_err());
            } else {
                let tick = u64::from_le_bytes(bytes[..8].try_into().unwrap());
                let stick = f32::from_le_bytes(bytes[8..].try_into().unwrap());
                if !(stick.is_finite() && stick.abs() <= 1.0) {
                    refused.push(format!("peer {from}: stick {stick}"));
                } else if let Err(reason) = inputs.push(from, tick, stick) {
                    refused.push(reason);
                }
            }
        }
        assert_eq!(
            refused,
            [
                "peer 1 sent state; only the host may",
                "peer 1 sent state; only the host may",
                "peer 1 sent state; only the host may",
                "peer 1 sent state; only the host may",
                "peer 1 may not call \"break_chunk\" on \"Terrain\"",
                "peer 1 may not call \"teleport\" on \"Hero\"",
                "peer 1: stick NaN",
                "peer 1: stick 50",
                "peer 1: input for tick 500 is too far ahead",
            ]
        );
        // Only the one valid input gets through, and the host's state is
        // its own.
        inputs.take();
        inputs.take();
        let (_, third) = inputs.take();
        assert_eq!(third.len(), 1);
        assert_eq!(third[0].input, 0.5);
        assert_eq!(
            host_world.get::<Transform>(host_hero).unwrap().position,
            [0.0; 3]
        );
        assert_eq!(replication.delta(&mut host_world).unwrap(), None);
    }
}
