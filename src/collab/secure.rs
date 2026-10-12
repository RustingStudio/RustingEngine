//! End-to-end encryption of collaboration traffic. Each guest and the host
//! run a Noise `NNpsk0` handshake with the session key as the pre-shared
//! key, so a relay or anyone on the network sees only ciphertext, and a
//! peer without the key cannot join or read anything. Each session gets
//! fresh keys from the handshake, so a session key read later does not
//! decrypt recorded traffic.

use std::collections::BTreeMap;
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use snow::{HandshakeState, TransportState};

use crate::net::{NetEvent, NetSession, PeerId, HOST};

const PARAMS: &str = "Noise_NNpsk0_25519_ChaChaPoly_BLAKE2s";
const HANDSHAKE: u8 = 0;
const DATA: u8 = 1;
/// The host's answer to a handshake made with another key.
const REFUSED: u8 = 2;
/// Noise's largest message.
const NOISE_MESSAGE: usize = 65535;
/// Plaintext per Noise message: the largest message less its tag.
const PIECE: usize = NOISE_MESSAGE - 16;

/// A new random session key: 32 hexadecimal digits (122 random bits).
#[must_use]
pub fn new_key() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

/// The pre-shared key for `text`, ignoring case, spaces and dashes, or
/// `None` when it is not 32 hexadecimal digits.
fn parse_key(text: &str) -> Option<[u8; 32]> {
    let clean: String = text
        .chars()
        .filter(|c| !c.is_whitespace() && *c != '-')
        .collect::<String>()
        .to_ascii_lowercase();
    let bytes = clean.as_bytes();
    if !bytes.iter().all(u8::is_ascii_hexdigit) {
        return None;
    }
    bytes.try_into().ok()
}

fn noise(error: snow::Error) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, error.to_string())
}

fn builder(key: &[u8; 32]) -> io::Result<snow::Builder<'_>> {
    let params = PARAMS.parse().map_err(noise)?;
    snow::Builder::new(params).psk(0, key).map_err(noise)
}

fn framed(kind: u8, bytes: &[u8]) -> Vec<u8> {
    let mut frame = Vec::with_capacity(bytes.len() + 1);
    frame.push(kind);
    frame.extend_from_slice(bytes);
    frame
}

/// A `DATA` frame: each piece of `plain` as a 2-byte length and a Noise
/// message.
fn seal(transport: &mut TransportState, plain: &[u8]) -> io::Result<Vec<u8>> {
    let mut frame = vec![DATA];
    let mut buffer = vec![0; NOISE_MESSAGE];
    for piece in plain.chunks(PIECE) {
        let length =
            transport.write_message(piece, &mut buffer).map_err(noise)?;
        frame.extend_from_slice(&(length as u16).to_le_bytes());
        frame.extend_from_slice(&buffer[..length]);
    }
    Ok(frame)
}

/// The plaintext of a `DATA` frame's body, or `None` when any piece is cut
/// short or fails to decrypt.
fn open(transport: &mut TransportState, mut body: &[u8]) -> Option<Vec<u8>> {
    let mut plain = Vec::with_capacity(body.len());
    let mut buffer = vec![0; NOISE_MESSAGE];
    while !body.is_empty() {
        let (length, rest) = body.split_first_chunk::<2>()?;
        let length = usize::from(u16::from_le_bytes(*length));
        let message = rest.get(..length)?;
        let size = transport.read_message(message, &mut buffer).ok()?;
        plain.extend_from_slice(&buffer[..size]);
        body = &rest[length..];
    }
    Some(plain)
}

enum Peer {
    /// The handshake has not finished; sends wait in the queue.
    Waiting(Box<HandshakeState>, Vec<Vec<u8>>),
    Ready(TransportState),
}

/// A [`NetSession`] whose messages are encrypted end to end. The host
/// talks only to guests that finished the handshake with its key.
pub struct SecureNet {
    net: NetSession,
    key: [u8; 32],
    // ponytail: one lock over every peer, held while a message is written,
    // like `NetSession`'s own stream lock. Per-peer locks if a slow guest
    // stalls the others.
    peers: Mutex<BTreeMap<PeerId, Peer>>,
    refused: AtomicBool,
}

impl SecureNet {
    /// Wraps `net` with the session `key`. A guest starts its handshake
    /// with the host at once; what it sends waits until the host answers.
    pub fn new(net: NetSession, key: &str) -> io::Result<Self> {
        let key = parse_key(key).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "the session key is 32 letters and digits, from the host",
            )
        })?;
        let mut peers = BTreeMap::new();
        if net.id() != HOST {
            let mut handshake =
                builder(&key)?.build_initiator().map_err(noise)?;
            let mut buffer = vec![0; NOISE_MESSAGE];
            let length =
                handshake.write_message(&[], &mut buffer).map_err(noise)?;
            net.send(HOST, &framed(HANDSHAKE, &buffer[..length]))?;
            peers.insert(HOST, Peer::Waiting(Box::new(handshake), Vec::new()));
        }
        Ok(Self {
            net,
            key,
            peers: Mutex::new(peers),
            refused: AtomicBool::new(false),
        })
    }

    #[must_use]
    pub fn id(&self) -> PeerId {
        self.net.id()
    }

    #[must_use]
    pub fn room_code(&self) -> Option<&str> {
        self.net.room_code()
    }

    /// True once the host refused this guest's session key.
    #[must_use]
    pub fn refused(&self) -> bool {
        self.refused.load(Ordering::Relaxed)
    }

    fn send_to(
        &self,
        peers: &mut BTreeMap<PeerId, Peer>,
        to: PeerId,
        bytes: &[u8],
    ) -> io::Result<()> {
        match peers.get_mut(&to) {
            Some(Peer::Ready(transport)) => {
                self.net.send(to, &seal(transport, bytes)?)
            }
            Some(Peer::Waiting(_, queue)) => {
                queue.push(bytes.to_vec());
                Ok(())
            }
            None => Err(io::Error::new(
                io::ErrorKind::NotConnected,
                "no encrypted connection to that peer",
            )),
        }
    }

    pub fn send(&self, to: PeerId, bytes: &[u8]) -> io::Result<()> {
        self.send_to(&mut self.peers.lock().unwrap(), to, bytes)
    }

    /// Sends to the host, or to every guest when this is the host.
    pub fn broadcast(&self, bytes: &[u8]) -> io::Result<()> {
        if self.id() != HOST {
            return self.send(HOST, bytes);
        }
        let mut peers = self.peers.lock().unwrap();
        let mut result = Ok(());
        for (peer, state) in peers.iter_mut() {
            if let Peer::Ready(transport) = state {
                if let Err(error) = seal(transport, bytes)
                    .and_then(|frame| self.net.send(*peer, &frame))
                {
                    result = Err(error);
                }
            }
        }
        result
    }

    /// The session's events with messages decrypted. Handshakes are handled
    /// here, and messages that fail to decrypt are dropped.
    pub fn poll(&self) -> Vec<NetEvent> {
        let mut events = Vec::new();
        for event in self.net.poll() {
            match event {
                NetEvent::Message { from, bytes } => {
                    let Some((&kind, body)) = bytes.split_first() else {
                        continue;
                    };
                    match kind {
                        HANDSHAKE => {
                            // A guest whose handshake fails cannot go on.
                            let failed = self.handshake(from, body).is_err();
                            events.extend(
                                (failed && self.id() != HOST)
                                    .then_some(NetEvent::Closed),
                            );
                        }
                        DATA => events.extend(self.decrypt(from, body)),
                        REFUSED if from == HOST && self.id() != HOST => {
                            self.refused.store(true, Ordering::Relaxed);
                            events.push(NetEvent::Closed);
                        }
                        _ => {}
                    }
                }
                NetEvent::Disconnected(peer) => {
                    self.peers.lock().unwrap().remove(&peer);
                    events.push(NetEvent::Disconnected(peer));
                }
                other => events.push(other),
            }
        }
        events
    }

    /// A message from a ready peer. A message that fails to decrypt was
    /// changed on the way, and every later one would fail too: the guest
    /// closes, the host stops listening to that guest.
    fn decrypt(&self, from: PeerId, body: &[u8]) -> Option<NetEvent> {
        let mut peers = self.peers.lock().unwrap();
        let Some(Peer::Ready(transport)) = peers.get_mut(&from) else {
            return None;
        };
        if let Some(bytes) = open(transport, body) {
            return Some(NetEvent::Message { from, bytes });
        }
        peers.remove(&from);
        Some(if self.id() == HOST {
            NetEvent::Disconnected(from)
        } else {
            NetEvent::Closed
        })
    }

    /// The host answers a guest's first handshake message; a guest reads
    /// the host's answer and sends what waited for it.
    fn handshake(&self, from: PeerId, message: &[u8]) -> io::Result<()> {
        let mut buffer = vec![0; NOISE_MESSAGE];
        let mut peers = self.peers.lock().unwrap();
        if self.id() == HOST {
            if peers.contains_key(&from) {
                return Ok(());
            }
            let mut handshake =
                builder(&self.key)?.build_responder().map_err(noise)?;
            if let Err(error) = handshake.read_message(message, &mut buffer) {
                let _ = self.net.send(from, &[REFUSED]);
                return Err(noise(error));
            }
            let length =
                handshake.write_message(&[], &mut buffer).map_err(noise)?;
            self.net.send(from, &framed(HANDSHAKE, &buffer[..length]))?;
            let transport = handshake.into_transport_mode().map_err(noise)?;
            peers.insert(from, Peer::Ready(transport));
            return Ok(());
        }
        if from != HOST {
            return Ok(());
        }
        let Some(Peer::Waiting(mut handshake, queue)) = peers.remove(&HOST)
        else {
            return Ok(());
        };
        handshake
            .read_message(message, &mut buffer)
            .map_err(noise)?;
        let transport = (*handshake).into_transport_mode().map_err(noise)?;
        peers.insert(HOST, Peer::Ready(transport));
        for bytes in queue {
            self.send_to(&mut peers, HOST, &bytes)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A host and one guest over loopback, handshake done.
    fn pair(host_key: &str, guest_key: &str) -> (SecureNet, SecureNet) {
        let (host, mut guests) = NetSession::loopback(1);
        let host = SecureNet::new(host, host_key).unwrap();
        let guest = SecureNet::new(guests.remove(0), guest_key).unwrap();
        (host, guest)
    }

    fn messages(net: &SecureNet) -> Vec<Vec<u8>> {
        net.poll()
            .into_iter()
            .filter_map(|event| match event {
                NetEvent::Message { bytes, .. } => Some(bytes),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn keys_parse_loosely_and_reject_garbage() {
        let key = new_key();
        assert_eq!(key.len(), 32);
        assert!(parse_key(&key).is_some());
        let spaced = format!(" {}-{} ", &key[..16], key[16..].to_uppercase());
        assert_eq!(parse_key(&spaced), parse_key(&key));
        assert!(parse_key("").is_none());
        assert!(parse_key(&key[..31]).is_none());
        assert!(parse_key(&"z".repeat(32)).is_none());
    }

    #[test]
    fn messages_travel_both_ways_once_the_handshake_ends() {
        let key = new_key();
        let (host, guest) = pair(&key, &key);
        // Sent before the host answered: it waits for the handshake.
        guest.send(HOST, b"hello").unwrap();
        assert!(messages(&host).is_empty());
        assert!(messages(&guest).is_empty());
        assert_eq!(messages(&host), [b"hello".to_vec()]);
        let big: Vec<u8> = (0..5 << 20).map(|i| (i % 253) as u8).collect();
        host.broadcast(&big).unwrap();
        assert_eq!(messages(&guest), [big]);
    }

    /// Polls `net` until a message arrives, for real sockets.
    fn wait_message(net: &SecureNet, other: &SecureNet) -> Vec<u8> {
        let deadline =
            std::time::Instant::now() + std::time::Duration::from_secs(5);
        while std::time::Instant::now() < deadline {
            let _ = other.poll();
            if let Some(bytes) = messages(net).pop() {
                return bytes;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        panic!("no message");
    }

    #[test]
    fn works_over_tcp() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let key = new_key();
        let host =
            SecureNet::new(NetSession::host_on(listener, "").unwrap(), &key)
                .unwrap();
        let guest =
            SecureNet::new(NetSession::join(address, "").unwrap(), &key)
                .unwrap();
        guest.send(HOST, b"over the wire").unwrap();
        assert_eq!(wait_message(&host, &guest), b"over the wire");
        host.broadcast(b"back").unwrap();
        assert_eq!(wait_message(&guest, &host), b"back");
    }

    #[test]
    fn wrong_key_is_refused() {
        let (host, guest) = pair(&new_key(), &new_key());
        guest.send(HOST, b"hello").unwrap();
        assert!(messages(&host).is_empty());
        assert_eq!(guest.poll(), [NetEvent::Closed]);
        assert!(guest.refused());
        assert!(host.send(1, b"x").is_err(), "the host never trusts it");
    }

    #[test]
    fn traffic_is_ciphertext_and_tampering_is_dropped() {
        let key = parse_key(&new_key()).unwrap();
        let mut initiator = builder(&key).unwrap().build_initiator().unwrap();
        let mut responder = builder(&key).unwrap().build_responder().unwrap();
        let (mut a, mut b) = (vec![0; NOISE_MESSAGE], vec![0; NOISE_MESSAGE]);
        let n = initiator.write_message(&[], &mut a).unwrap();
        responder.read_message(&a[..n], &mut b).unwrap();
        let n = responder.write_message(&[], &mut a).unwrap();
        initiator.read_message(&a[..n], &mut b).unwrap();
        let mut sender = initiator.into_transport_mode().unwrap();
        let mut receiver = responder.into_transport_mode().unwrap();

        let plain = b"scenes/secret.rscene holds the boss fight";
        let frame = seal(&mut sender, plain).unwrap();
        assert!(!frame.windows(6).any(|w| w == b"secret"));
        let mut tampered = frame.clone();
        *tampered.last_mut().unwrap() ^= 1;
        assert_eq!(open(&mut receiver, &tampered[1..]), None);
        assert_eq!(open(&mut receiver, &frame[1..]).unwrap(), plain);
    }
}
