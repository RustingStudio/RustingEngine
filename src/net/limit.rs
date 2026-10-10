//! Per-peer rate limits for what clients send the host, and bandwidth
//! budgets for what the host sends each client.

use super::{invalid, NetSession, PeerId};
use std::collections::BTreeMap;
use std::io;
use std::time::{Duration, Instant};

/// A token bucket per peer: each peer may send `burst` messages at once,
/// then `per_second` on average. Ask it before handling a client message
/// and drop (or count against the sender) the ones it refuses.
#[derive(Clone, Debug)]
pub struct RateLimit {
    per_second: f64,
    burst: f64,
    buckets: BTreeMap<PeerId, (f64, Instant)>,
}

impl RateLimit {
    pub fn new(per_second: f64, burst: u32) -> Self {
        Self {
            per_second,
            burst: f64::from(burst),
            buckets: BTreeMap::new(),
        }
    }

    /// Takes one message from `peer` at `now`; `false` when it is over its
    /// rate.
    pub fn allow(&mut self, peer: PeerId, now: Instant) -> bool {
        self.take(peer, 1.0, now)
    }

    fn take(&mut self, peer: PeerId, cost: f64, now: Instant) -> bool {
        let (tokens, last) =
            self.buckets.entry(peer).or_insert((self.burst, now));
        let earned = now.saturating_duration_since(*last).as_secs_f64();
        *tokens = (*tokens + earned * self.per_second).min(self.burst);
        *last = now;
        if *tokens >= cost {
            *tokens -= cost;
            true
        } else {
            false
        }
    }

    /// Forgets `peer`, on `NetEvent::Disconnected`.
    pub fn forget(&mut self, peer: PeerId) {
        self.buckets.remove(&peer);
    }
}

/// What a [`Bandwidth`] measured for one client. Bytes are payload bytes.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Usage {
    pub bytes_sent: u64,
    pub messages_sent: u64,
    /// Messages refused because the client was over its budget.
    pub messages_dropped: u64,
    /// Bytes sent during the last whole second.
    pub bytes_per_second: u64,
    window: Option<(Instant, u64)>,
}

/// A bandwidth budget per client: the host may send each one `burst`
/// bytes at once, then `bytes_per_second` on average, and the budget
/// measures what each one got. Send per-tick state through it; a refused
/// message is simply not sent, so use it for messages that a later one
/// replaces, such as [`snapshot_for`](super::replicate::Replication::snapshot_for).
#[derive(Clone, Debug)]
pub struct Bandwidth {
    cap: RateLimit,
    usage: BTreeMap<PeerId, Usage>,
}

impl Bandwidth {
    pub fn new(bytes_per_second: f64, burst: u32) -> Self {
        Self {
            cap: RateLimit::new(bytes_per_second, burst),
            usage: BTreeMap::new(),
        }
    }

    /// Sends `bytes` to `to` unreliably if its budget allows at `now`.
    /// `Ok(false)` when it does not. Fails for a message bigger than the
    /// burst, which could never be sent.
    pub fn send_unreliable(
        &mut self,
        session: &NetSession,
        to: PeerId,
        bytes: &[u8],
        now: Instant,
    ) -> io::Result<bool> {
        let size = bytes.len() as f64;
        if size > self.cap.burst {
            return Err(invalid("message is bigger than the bandwidth burst"));
        }
        let allowed = self.cap.take(to, size, now);
        if allowed {
            session.send_unreliable(to, bytes)?;
        }
        let usage = self.usage.entry(to).or_default();
        let (start, bytes_in_window) = usage.window.get_or_insert((now, 0));
        if now.saturating_duration_since(*start) >= Duration::from_secs(1) {
            usage.bytes_per_second = *bytes_in_window;
            *start = now;
            *bytes_in_window = 0;
        }
        if allowed {
            *bytes_in_window += bytes.len() as u64;
            usage.bytes_sent += bytes.len() as u64;
            usage.messages_sent += 1;
        } else {
            usage.messages_dropped += 1;
        }
        Ok(allowed)
    }

    /// What `peer` was sent so far.
    pub fn usage(&self, peer: PeerId) -> Usage {
        self.usage.get(&peer).copied().unwrap_or_default()
    }

    /// Forgets `peer`, on `NetEvent::Disconnected`.
    pub fn forget(&mut self, peer: PeerId) {
        self.cap.forget(peer);
        self.usage.remove(&peer);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn each_peer_gets_a_burst_then_its_rate() {
        let mut limit = RateLimit::new(10.0, 3);
        let start = Instant::now();
        let allowed = |limit: &mut RateLimit, peer, at| {
            (0..5).filter(|_| limit.allow(peer, at)).count()
        };
        assert_eq!(allowed(&mut limit, 1, start), 3);
        assert_eq!(allowed(&mut limit, 2, start), 3); // its own bucket
        let later = start + Duration::from_millis(200);
        assert_eq!(allowed(&mut limit, 1, later), 2);
        // A long pause refills only up to the burst.
        let much_later = start + Duration::from_secs(60);
        assert_eq!(allowed(&mut limit, 1, much_later), 3);
        limit.forget(1);
        assert_eq!(allowed(&mut limit, 1, much_later), 3);
    }

    #[test]
    fn bandwidth_caps_and_measures_each_client() {
        let (host, _clients) = NetSession::loopback(2);
        host.poll();
        let mut budget = Bandwidth::new(1000.0, 400);
        let start = Instant::now();
        let message = [0u8; 100];
        let sent = |budget: &mut Bandwidth, peer, at| {
            (0..10)
                .filter(|_| {
                    budget.send_unreliable(&host, peer, &message, at).unwrap()
                })
                .count()
        };
        assert_eq!(sent(&mut budget, 1, start), 4, "the burst");
        assert_eq!(sent(&mut budget, 2, start), 4, "its own budget");
        let later = start + Duration::from_secs(1);
        assert_eq!(sent(&mut budget, 1, later), 4, "refilled to the burst");
        let usage = budget.usage(1);
        assert_eq!((usage.bytes_sent, usage.messages_sent), (800, 8));
        assert_eq!(usage.messages_dropped, 12);
        assert_eq!(usage.bytes_per_second, 400, "the first second");
        assert_eq!(host.stats().bytes_sent, 1200 + 12 * 9);
        assert!(budget.send_unreliable(&host, 1, &[0; 401], later).is_err());
        budget.forget(1);
        assert_eq!(budget.usage(1), Usage::default());
    }
}
