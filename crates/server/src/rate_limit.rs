//! Client IP keys and the per-route rate limits (INF-6c-1).
//!
//! The client IP comes from the TCP peer. `X-Real-IP` is trusted only when the peer is
//! loopback, i.e. nginx on the same host, which sets it with
//! `proxy_set_header X-Real-IP $remote_addr` (Security_Mandate §2i). That nginx line is
//! load-bearing: without it a client's own `X-Real-IP` would pass through and be trusted.
//! `X-Forwarded-For` is never read, not even as a fallback.
//!
//! The limits are consts, never config: nothing in `ServerConfig` can loosen them.

use std::hash::Hash;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use axum::http::HeaderMap;
use dashmap::mapref::entry::Entry;
use dashmap::DashMap;

/// `challenge` + `verify`, one bucket shared by both routes.
pub const SIGN_IN_PER_IP: u32 = 20;
pub const JOIN_PER_IP: u32 = 10;
pub const JOIN_PER_WALLET: u32 = 3;
pub const REPLACEMENT_PER_IP: u32 = 10;
pub const REPLACEMENT_PER_WALLET: u32 = 3;
pub const RATE_LIMIT_WINDOW: Duration = Duration::from_secs(60);
/// Keys per limiter before a sweep; still full after the sweep → refused (fail closed).
pub const RATE_LIMIT_MAP_CAP: usize = 10_000;

/// What a request is counted against. IPv4 whole; IPv6 by its /64 (`V6Net` holds the top
/// 64 bits), so one host can't mint fresh buckets from its own prefix. `Shared` is the one
/// bucket for a loopback peer that sent no usable `X-Real-IP`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ClientKey {
    V4(Ipv4Addr),
    V6Net(u64),
    Shared,
}

fn key_of(ip: IpAddr) -> ClientKey {
    match ip.to_canonical() {
        IpAddr::V4(v4) => ClientKey::V4(v4),
        IpAddr::V6(v6) => {
            let o = v6.octets();
            let mut top = [0u8; 8];
            top.copy_from_slice(&o[..8]);
            ClientKey::V6Net(u64::from_be_bytes(top))
        }
    }
}

/// The key for a request. Pure, no logging.
///
/// - A non-loopback peer (after `::ffff:a.b.c.d` canonicalisation) is the key; headers are
///   not read.
/// - A loopback peer: exactly one `X-Real-IP` value that parses as an IP is the key.
///   Zero values, two or more, or an unparseable one → `Shared`.
pub fn client_key(peer: SocketAddr, headers: &HeaderMap) -> ClientKey {
    let peer_ip = peer.ip().to_canonical();
    if !peer_ip.is_loopback() {
        return key_of(peer_ip);
    }
    let mut values = headers.get_all("x-real-ip").iter();
    match (values.next(), values.next()) {
        (Some(v), None) => v
            .to_str()
            .ok()
            .and_then(|s| s.trim().parse::<IpAddr>().ok())
            .map(key_of)
            .unwrap_or(ClientKey::Shared),
        _ => ClientKey::Shared,
    }
}

/// Whole seconds until the caller's window ends; the `Retry-After` value.
#[derive(Debug, PartialEq, Eq)]
pub struct RetryAfter(pub u64);

struct Window {
    start: Instant,
    count: u32,
}

/// A keyed fixed-window limiter.
///
/// - The window is anchored at a key's first hit, not at the wall clock.
/// - The map is capped. A new key arriving at the cap sweeps expired windows once; if the
///   map is still full, the new key is refused (fail closed). Keys already present keep
///   working. The cap is soft: concurrent new keys can overshoot it by at most the
///   concurrency.
/// - **Never call `len()`, `retain()` or `contains_key()` while an `entry()` guard is
///   alive:** `entry` holds its shard's write lock, so any of them deadlocks on that shard.
pub struct FixedWindow<K> {
    map: DashMap<K, Window>,
    limit: u32,
    window: Duration,
    cap: usize,
}

impl<K: Eq + Hash + Clone> FixedWindow<K> {
    pub fn new(limit: u32, window: Duration, cap: usize) -> Self {
        Self { map: DashMap::new(), limit, window, cap }
    }

    /// Counts one request for `key` at `now`. `Err` carries the seconds to wait.
    pub fn check(&self, key: K, now: Instant) -> Result<(), RetryAfter> {
        // Pressure step: no entry guard is alive here.
        if !self.map.contains_key(&key) && self.map.len() >= self.cap {
            self.map.retain(|_, w| now < w.start + self.window);
            if self.map.len() >= self.cap {
                return Err(RetryAfter(self.window.as_secs()));
            }
        }
        match self.map.entry(key) {
            Entry::Occupied(mut e) => {
                let w = e.get_mut();
                if now >= w.start + self.window {
                    w.start = now;
                    w.count = 0;
                }
                if w.count >= self.limit {
                    return Err(RetryAfter(retry_secs(w.start + self.window, now)));
                }
                w.count += 1;
                Ok(())
            }
            Entry::Vacant(e) => {
                e.insert(Window { start: now, count: 1 });
                Ok(())
            }
        }
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.map.len()
    }
}

/// Whole seconds from `now` to `end`, rounded up, minimum 1.
fn retry_secs(end: Instant, now: Instant) -> u64 {
    let left = end.saturating_duration_since(now);
    let secs = left.as_secs() + u64::from(left.subsec_nanos() > 0);
    secs.max(1)
}

/// The five limiters, one set per `AppState`. Per-wallet limiters exist only for the two
/// authenticated routes (GAP-77): a sign-in body's wallet is unverified, so it is never a key.
pub struct RateLimits {
    pub sign_in_ip: FixedWindow<ClientKey>,
    pub join_ip: FixedWindow<ClientKey>,
    pub join_wallet: FixedWindow<String>,
    pub replacement_ip: FixedWindow<ClientKey>,
    pub replacement_wallet: FixedWindow<String>,
    /// Set by the first request that falls into `ClientKey::Shared`, so its warning is
    /// journalled once per `AppState` (once per process).
    pub shared_warned: AtomicBool,
}

impl RateLimits {
    pub fn new() -> Self {
        fn w<K: Eq + Hash + Clone>(limit: u32) -> FixedWindow<K> {
            FixedWindow::new(limit, RATE_LIMIT_WINDOW, RATE_LIMIT_MAP_CAP)
        }
        Self {
            sign_in_ip: w(SIGN_IN_PER_IP),
            join_ip: w(JOIN_PER_IP),
            join_wallet: w(JOIN_PER_WALLET),
            replacement_ip: w(REPLACEMENT_PER_IP),
            replacement_wallet: w(REPLACEMENT_PER_WALLET),
            shared_warned: AtomicBool::new(false),
        }
    }
}

impl Default for RateLimits {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    fn peer(s: &str) -> SocketAddr {
        SocketAddr::new(s.parse().unwrap(), 1)
    }

    fn real_ip(values: &[&str]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for v in values {
            h.append("x-real-ip", HeaderValue::from_str(v).unwrap());
        }
        h
    }

    fn v4(s: &str) -> ClientKey {
        ClientKey::V4(s.parse().unwrap())
    }

    #[test]
    fn client_key_ignores_x_real_ip_from_a_non_loopback_peer() {
        assert_eq!(client_key(peer("192.0.2.1"), &real_ip(&["198.51.100.7"])), v4("192.0.2.1"));
    }

    #[test]
    fn client_key_reads_x_real_ip_from_a_loopback_peer() {
        assert_eq!(client_key(peer("127.0.0.1"), &real_ip(&["198.51.100.7"])), v4("198.51.100.7"));
    }

    #[test]
    fn client_key_is_shared_when_the_header_is_missing_unparseable_or_repeated() {
        let lo = peer("127.0.0.1");
        assert_eq!(client_key(lo, &HeaderMap::new()), ClientKey::Shared);
        assert_eq!(client_key(lo, &real_ip(&["not-an-ip"])), ClientKey::Shared);
        assert_eq!(client_key(lo, &real_ip(&["198.51.100.7", "198.51.100.8"])), ClientKey::Shared);
    }

    #[test]
    fn client_key_masks_ipv6_to_its_64() {
        let none = HeaderMap::new();
        let a = client_key(peer("2001:db8:1:2::a"), &none);
        let b = client_key(peer("2001:db8:1:2::b"), &none);
        let c = client_key(peer("2001:db8:1:3::a"), &none);
        assert_eq!(a, b, "two addresses in one /64 share a key");
        assert_ne!(a, c, "another /64 has its own key");
    }

    #[test]
    fn client_key_keys_ipv4_whole_and_canonicalises_mapped() {
        let none = HeaderMap::new();
        assert_ne!(client_key(peer("192.0.2.1"), &none), client_key(peer("192.0.2.2"), &none));
        assert_eq!(client_key(peer("::ffff:192.0.2.1"), &none), v4("192.0.2.1"));
        assert_eq!(client_key(peer("127.0.0.1"), &real_ip(&["::ffff:198.51.100.7"])), v4("198.51.100.7"));
    }

    #[test]
    fn fixed_window_allows_the_limit_then_refuses() {
        let fw = FixedWindow::new(3, Duration::from_secs(60), 100);
        let t0 = Instant::now();
        for _ in 0..3 {
            assert_eq!(fw.check("a", t0), Ok(()));
        }
        assert!(fw.check("a", t0).is_err());
        assert_eq!(fw.check("b", t0), Ok(()), "another key has its own window");
    }

    #[test]
    fn fixed_window_resets_after_the_window() {
        let fw = FixedWindow::new(1, Duration::from_secs(60), 100);
        let t0 = Instant::now();
        assert_eq!(fw.check("a", t0), Ok(()));
        assert!(fw.check("a", t0 + Duration::from_secs(59)).is_err());
        assert_eq!(fw.check("a", t0 + Duration::from_secs(60)), Ok(()));
    }

    #[test]
    fn fixed_window_sweeps_expired_keys_at_the_cap() {
        let fw = FixedWindow::new(5, Duration::from_secs(60), 2);
        let t0 = Instant::now();
        assert_eq!(fw.check("a", t0), Ok(()));
        assert_eq!(fw.check("b", t0), Ok(()));
        assert_eq!(fw.check("c", t0 + Duration::from_secs(61)), Ok(()));
        assert_eq!(fw.len(), 1, "the sweep removed both expired keys");
    }

    #[test]
    fn fixed_window_fails_closed_when_still_full() {
        let fw = FixedWindow::new(5, Duration::from_secs(60), 2);
        let t0 = Instant::now();
        assert_eq!(fw.check("a", t0), Ok(()));
        assert_eq!(fw.check("b", t0), Ok(()));
        assert_eq!(fw.check("c", t0 + Duration::from_secs(1)), Err(RetryAfter(60)));
        assert_eq!(fw.check("a", t0 + Duration::from_secs(1)), Ok(()), "a key already in the map keeps working");
    }

    #[test]
    fn retry_after_rounds_up_with_a_minimum_of_one() {
        let fw = FixedWindow::new(1, Duration::from_secs(60), 100);
        let t0 = Instant::now();
        assert_eq!(fw.check("a", t0), Ok(()));
        assert_eq!(fw.check("a", t0 + Duration::from_millis(500)), Err(RetryAfter(60)));
        assert_eq!(fw.check("a", t0 + Duration::from_millis(59_500)), Err(RetryAfter(1)));
    }
}
