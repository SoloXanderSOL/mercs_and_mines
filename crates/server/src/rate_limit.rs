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
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use axum::http::HeaderMap;
use dashmap::mapref::entry::Entry;
use dashmap::DashMap;

/// `challenge` + `verify`, one bucket shared by both routes.
pub const SIGN_IN_PER_IP: u32 = 400;
pub const JOIN_PER_IP: u32 = 30;
pub const JOIN_PER_WALLET: u32 = 3;
pub const REPLACEMENT_PER_IP: u32 = 30;
pub const REPLACEMENT_PER_WALLET: u32 = 3;
pub const ABANDON_PER_IP: u32 = 30;
pub const ABANDON_PER_WALLET: u32 = 3;
/// The demo's abandon, per wallet per day (BP-1b, demo-only): bounds one wallet's re-roll
/// loop and the archived campaigns it leaves (the live count is a seq scan under the join
/// lock). A booth demoing on one wallet all day stays under it; restarting `mercs-demo`
/// clears it.
pub const ABANDON_PER_WALLET_DAILY: u32 = 200;
pub const ABANDON_DAILY_WINDOW: Duration = Duration::from_secs(86_400);
pub const RATE_LIMIT_WINDOW: Duration = Duration::from_secs(60);
/// Keys per limiter before a sweep; still full after the sweep → the new key is admitted
/// uncounted (fail open, ODQ-48) and the overflow is journalled.
pub const RATE_LIMIT_MAP_CAP: usize = 100_000;

/// The sector read route (ODQ-57, keeper). Wallets are free to mint, so the per-address
/// limit is the real bound on its ~86 KB body.
pub const SECTOR_READ_PER_IP: u32 = 30;
pub const SECTOR_READ_PER_WALLET: u32 = 10;

/// The roster read route (ODQ-57, keeper): a small body, so a looser per-address limit.
pub const ROSTER_READ_PER_IP: u32 = 120;
pub const ROSTER_READ_PER_WALLET: u32 = 10;

/// The Genesis Token lookup (BP-4, keeper). A lookup spends up to two calls on the
/// provider key and wallets are free, so `GENESIS_ALL_PER_MINUTE` (one key for everyone)
/// is the bound on the key's quota: over it the route answers `503`.
pub const GENESIS_PER_IP: u32 = 20;
pub const GENESIS_PER_WALLET: u32 = 3;
pub const GENESIS_ALL_PER_MINUTE: u32 = 120;

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
/// - The map is capped. A new key arriving at the cap sweeps expired windows, at most once
///   per `SWEEP_MIN_INTERVAL`; if the map is still full, the new key is admitted uncounted
///   (fail open, ODQ-48) and `overflow` journals it. Keys already present keep working. The
///   cap is soft: concurrent new keys can overshoot it by at most the concurrency.
/// - **Never call `len()`, `retain()` or `contains_key()` while an `entry()` guard is
///   alive:** `entry` holds its shard's write lock, so any of them deadlocks on that shard.
pub struct FixedWindow<K> {
    name: &'static str,
    map: DashMap<K, Window>,
    limit: u32,
    window: Duration,
    cap: usize,
    sweep: SweepThrottle,
    overflow: RateWarn,
}

impl<K: Eq + Hash + Clone> FixedWindow<K> {
    pub fn new(name: &'static str, limit: u32, window: Duration, cap: usize) -> Self {
        Self {
            name,
            map: DashMap::new(),
            limit,
            window,
            cap,
            sweep: SweepThrottle::new(),
            overflow: RateWarn::new(),
        }
    }

    /// Counts one request for `key` at `now`. `Err` carries the seconds to wait.
    pub fn check(&self, key: K, now: Instant) -> Result<(), RetryAfter> {
        // Pressure step: no entry guard is alive here.
        if !self.map.contains_key(&key) && self.map.len() >= self.cap {
            if self.sweep.allow(now) {
                self.map.retain(|_, w| now < w.start + self.window);
            }
            if self.map.len() >= self.cap {
                if let Some(n) = self.overflow.hit(now) {
                    tracing::warn!(
                        limiter = self.name,
                        admitted = n,
                        cap = self.cap,
                        "rate-limit map full: admitting new keys uncounted (fail open)"
                    );
                }
                return Ok(());
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

    #[cfg(test)]
    fn contains(&self, key: &K) -> bool {
        self.map.contains_key(key)
    }
}

/// Whole seconds from `now` to `end`, rounded up, minimum 1.
fn retry_secs(end: Instant, now: Instant) -> u64 {
    let left = end.saturating_duration_since(now);
    let secs = left.as_secs() + u64::from(left.subsec_nanos() > 0);
    secs.max(1)
}

/// How often a `RateWarn` journals after its first line.
pub const RATE_WARN_INTERVAL: Duration = Duration::from_secs(3600);
/// The shortest gap between two sweeps of one capped map.
pub const SWEEP_MIN_INTERVAL: Duration = Duration::from_secs(1);

/// Milliseconds from `base` to `now`, plus 1, so that `0` can mean "never". A `now` before
/// `base` saturates.
fn stamp(base: Instant, now: Instant) -> u64 {
    now.saturating_duration_since(base).as_millis() as u64 + 1
}

/// True on the first call, then at most once per `every`. Claimed by `compare_exchange`, so
/// of two concurrent callers only one gets `true`.
fn claim(base: Instant, last: &AtomicU64, now: Instant, every: Duration) -> bool {
    let at = stamp(base, now);
    let prev = last.load(Ordering::Relaxed);
    let due = prev == 0 || at.saturating_sub(prev) >= every.as_millis() as u64;
    due && last.compare_exchange(prev, at, Ordering::Relaxed, Ordering::Relaxed).is_ok()
}

/// Journals a repeating condition without flooding (ODQ-48; Reviewer INF-6c-1 F2): the first
/// hit, then at most once per `RATE_WARN_INTERVAL`, each time with the number of hits since
/// the last line. The caller owns the `warn!` text. There is deliberately no `Default`.
#[derive(Debug)]
pub struct RateWarn {
    base: Instant,
    last_ms: AtomicU64,
    pending: AtomicU64,
}

impl RateWarn {
    pub fn new() -> Self {
        Self { base: Instant::now(), last_ms: AtomicU64::new(0), pending: AtomicU64::new(0) }
    }

    /// Counts one hit at `now`. `Some(n)` means this caller journals: `n` hits since the
    /// last line, this one included.
    pub fn hit(&self, now: Instant) -> Option<u64> {
        self.pending.fetch_add(1, Ordering::Relaxed);
        if claim(self.base, &self.last_ms, now, RATE_WARN_INTERVAL) {
            Some(self.pending.swap(0, Ordering::Relaxed))
        } else {
            None
        }
    }

    #[cfg(test)]
    pub(crate) fn fired(&self) -> bool {
        self.last_ms.load(Ordering::Relaxed) != 0
    }
}

/// At most one sweep per `SWEEP_MIN_INTERVAL`. The first is never refused, so a map's first
/// sweep at its cap always runs.
pub struct SweepThrottle {
    base: Instant,
    last_ms: AtomicU64,
}

impl SweepThrottle {
    pub fn new() -> Self {
        Self { base: Instant::now(), last_ms: AtomicU64::new(0) }
    }

    pub fn allow(&self, now: Instant) -> bool {
        claim(self.base, &self.last_ms, now, SWEEP_MIN_INTERVAL)
    }
}

/// The eight limiters, one set per `AppState`. Per-wallet limiters exist only for the three
/// authenticated routes (GAP-77): a sign-in body's wallet is unverified, so it is never a key.
pub struct RateLimits {
    pub sign_in_ip: FixedWindow<ClientKey>,
    /// The sector read route (ODQ-57): per address and per wallet.
    pub sector_ip: FixedWindow<ClientKey>,
    pub sector_wallet: FixedWindow<String>,
    /// The roster read route (ODQ-57): per address and per wallet.
    pub roster_ip: FixedWindow<ClientKey>,
    pub roster_wallet: FixedWindow<String>,
    /// The Genesis Token lookup (BP-4): per address, per wallet, and one global ceiling.
    pub genesis_ip: FixedWindow<ClientKey>,
    pub genesis_wallet: FixedWindow<String>,
    pub genesis_all: FixedWindow<()>,
    /// Journals refusals at the global ceiling: the first, then at most hourly with a count.
    pub genesis_all_warn: RateWarn,
    pub join_ip: FixedWindow<ClientKey>,
    pub join_wallet: FixedWindow<String>,
    pub replacement_ip: FixedWindow<ClientKey>,
    pub replacement_wallet: FixedWindow<String>,
    pub abandon_ip: FixedWindow<ClientKey>,
    pub abandon_wallet: FixedWindow<String>,
    pub abandon_wallet_daily: FixedWindow<String>,
    /// Journals requests that fall into `ClientKey::Shared`: the first, then at most hourly
    /// with a count.
    pub shared_warn: RateWarn,
}

impl RateLimits {
    pub fn new() -> Self {
        fn w<K: Eq + Hash + Clone>(name: &'static str, limit: u32) -> FixedWindow<K> {
            FixedWindow::new(name, limit, RATE_LIMIT_WINDOW, RATE_LIMIT_MAP_CAP)
        }
        Self {
            sign_in_ip: w("sign_in_ip", SIGN_IN_PER_IP),
            sector_ip: w("sector_ip", SECTOR_READ_PER_IP),
            sector_wallet: w("sector_wallet", SECTOR_READ_PER_WALLET),
            roster_ip: w("roster_ip", ROSTER_READ_PER_IP),
            roster_wallet: w("roster_wallet", ROSTER_READ_PER_WALLET),
            genesis_ip: w("genesis_ip", GENESIS_PER_IP),
            genesis_wallet: w("genesis_wallet", GENESIS_PER_WALLET),
            genesis_all: w("genesis_all", GENESIS_ALL_PER_MINUTE),
            genesis_all_warn: RateWarn::new(),
            join_ip: w("join_ip", JOIN_PER_IP),
            join_wallet: w("join_wallet", JOIN_PER_WALLET),
            replacement_ip: w("replacement_ip", REPLACEMENT_PER_IP),
            replacement_wallet: w("replacement_wallet", REPLACEMENT_PER_WALLET),
            abandon_ip: w("abandon_ip", ABANDON_PER_IP),
            abandon_wallet: w("abandon_wallet", ABANDON_PER_WALLET),
            abandon_wallet_daily: FixedWindow::new("abandon_wallet_daily", ABANDON_PER_WALLET_DAILY, ABANDON_DAILY_WINDOW, RATE_LIMIT_MAP_CAP),
            shared_warn: RateWarn::new(),
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

    #[test]
    fn abandon_wallet_daily_limit_is_200() {
        let limits = RateLimits::new();
        let now = Instant::now();
        for i in 0..200 {
            assert!(limits.abandon_wallet_daily.check("w".to_string(), now).is_ok(), "request {i} is admitted");
        }
        assert!(limits.abandon_wallet_daily.check("w".to_string(), now).is_err(), "the 201st in a day is refused");
    }

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
        let fw = FixedWindow::new("t", 3, Duration::from_secs(60), 100);
        let t0 = Instant::now();
        for _ in 0..3 {
            assert_eq!(fw.check("a", t0), Ok(()));
        }
        assert!(fw.check("a", t0).is_err());
        assert_eq!(fw.check("b", t0), Ok(()), "another key has its own window");
    }

    #[test]
    fn fixed_window_resets_after_the_window() {
        let fw = FixedWindow::new("t", 1, Duration::from_secs(60), 100);
        let t0 = Instant::now();
        assert_eq!(fw.check("a", t0), Ok(()));
        assert!(fw.check("a", t0 + Duration::from_secs(59)).is_err());
        assert_eq!(fw.check("a", t0 + Duration::from_secs(60)), Ok(()));
    }

    #[test]
    fn fixed_window_sweeps_expired_keys_at_the_cap() {
        let fw = FixedWindow::new("t", 5, Duration::from_secs(60), 2);
        let t0 = Instant::now();
        assert_eq!(fw.check("a", t0), Ok(()));
        assert_eq!(fw.check("b", t0), Ok(()));
        assert_eq!(fw.check("c", t0 + Duration::from_secs(61)), Ok(()));
        assert_eq!(fw.len(), 1, "the sweep removed both expired keys");
    }

    /// ODQ-48: still full after the sweep → admitted uncounted (fail open), and journalled.
    #[test]
    fn fixed_window_fails_open_when_still_full() {
        let fw = FixedWindow::new("t", 5, Duration::from_secs(60), 2);
        let t0 = Instant::now();
        assert_eq!(fw.check("a", t0), Ok(()));
        assert_eq!(fw.check("b", t0), Ok(()));
        assert_eq!(fw.check("c", t0 + Duration::from_secs(1)), Ok(()), "admitted, not refused");
        assert_eq!(fw.len(), 2, "admitted uncounted");
        assert!(!fw.contains(&"c"));
        assert!(fw.overflow.fired(), "the overflow is journalled");
        assert_eq!(fw.check("a", t0 + Duration::from_secs(1)), Ok(()), "a key already in the map keeps working");
    }

    /// ODQ-48: at most one sweep a second. `d` finds `b` expired, but the last sweep was
    /// 0.5 s ago, so `d` is admitted uncounted with `b` still present.
    #[test]
    fn fixed_window_sweeps_at_most_once_a_second() {
        let fw = FixedWindow::new("t", 5, Duration::from_secs(60), 2);
        let t0 = Instant::now();
        let ms = Duration::from_millis;
        assert_eq!(fw.check("a", t0), Ok(()));
        assert_eq!(fw.check("b", t0 + ms(500)), Ok(()));
        assert_eq!(fw.check("c", t0 + ms(60_200)), Ok(()));
        assert!(!fw.contains(&"a") && fw.contains(&"b") && fw.contains(&"c"), "the first sweep ran");
        assert_eq!(fw.check("d", t0 + ms(60_700)), Ok(()));
        assert!(fw.contains(&"b") && !fw.contains(&"d"), "throttled: no sweep, d admitted uncounted");
        assert_eq!(fw.check("e", t0 + ms(61_300)), Ok(()));
        assert!(!fw.contains(&"b") && fw.contains(&"e"), "a second later the sweep runs again");
    }

    #[test]
    fn rate_warn_fires_first_then_at_most_hourly_with_the_count() {
        let w = RateWarn::new();
        let t0 = Instant::now();
        assert_eq!(w.hit(t0), Some(1), "the first hit is journalled");
        assert_eq!(w.hit(t0 + Duration::from_secs(3599)), None, "within the hour: counted only");
        assert_eq!(w.hit(t0 + Duration::from_secs(3600)), Some(2), "on the hour: the count since the last line");
    }

    #[test]
    fn retry_after_rounds_up_with_a_minimum_of_one() {
        let fw = FixedWindow::new("t", 1, Duration::from_secs(60), 100);
        let t0 = Instant::now();
        assert_eq!(fw.check("a", t0), Ok(()));
        assert_eq!(fw.check("a", t0 + Duration::from_millis(500)), Err(RetryAfter(60)));
        assert_eq!(fw.check("a", t0 + Duration::from_millis(59_500)), Err(RetryAfter(1)));
    }
}
