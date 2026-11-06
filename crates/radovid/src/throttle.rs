//! Bounds on how fast a peer may act: the growing delay after failed logins,
//! and the token bucket on messages.

use std::collections::HashMap;
use std::hash::Hash;
use std::net::{IpAddr, Ipv6Addr};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use radovid_proto::{MAX_UNAUTHENTICATED_PER_IP, valid_login};

/// Failures forgotten after this long without one.
const DECAY: Duration = Duration::from_secs(3600);
/// Failures before the delay starts, for one login from one source.
const FREE_FAILURES: u32 = 3;
/// Failures before the delay starts, for one login counted across all
/// sources: far above what an honest user mistypes, so that a few guessers
/// elsewhere cannot hold the owner's login back, yet a spread-out brute force
/// still slows down.
const LOGIN_FREE_FAILURES: u32 = 30;
/// The longest delay.
const MAX_DELAY_SECS: u64 = 300;
/// Counters kept at most; the oldest are dropped past it.
const MAX_ENTRIES: usize = 100_000;

#[derive(Clone, Copy)]
struct Failures {
    count: u32,
    last: Instant,
}

impl Failures {
    /// How long until the next attempt may be evaluated, once `free`
    /// failures are forgiven.
    fn wait(&self, free: u32, now: Instant) -> Duration {
        if now.duration_since(self.last) >= DECAY || self.count < free {
            return Duration::ZERO;
        }
        let secs = 1u64
            .checked_shl(self.count - free)
            .map_or(MAX_DELAY_SECS, |s| s.min(MAX_DELAY_SECS));
        (self.last + Duration::from_secs(secs)).saturating_duration_since(now)
    }
}

/// The source a per-address limit counts: an IPv4 address (also one mapped
/// into IPv6), or the /64 prefix of an IPv6 address, since one subscriber is
/// handed a whole /64 and could take a fresh address for every attempt.
pub fn source(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V4(v4) => IpAddr::V4(v4),
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => IpAddr::V4(v4),
            None => IpAddr::V6(Ipv6Addr::from_bits(v6.to_bits() & !u128::from(u64::MAX))),
        },
    }
}

/// A site's /48 key, in addition to its individual /64 source keys. Mapped
/// IPv4 addresses keep IPv4 semantics and never count in an IPv6 site.
pub fn ipv6_site(ip: IpAddr) -> Option<Ipv6Addr> {
    match source(ip) {
        IpAddr::V6(ip) => Some(Ipv6Addr::from_bits(ip.to_bits() & (u128::MAX << 80))),
        IpAddr::V4(_) => None,
    }
}

/// Unauthenticated connections one IPv6 /48 may hold: a site, or one host of a provider, holds a
/// whole /48 and could take a fresh /64 for every connection.
pub const MAX_UNAUTHENTICATED_PER_48: usize = 64;

/// A group of addresses a bound on unauthenticated connections counts: its prefix, and how many
/// bits long the prefix is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Group {
    prefix: IpAddr,
    bits: u8,
}

/// The groups an address counts in for the bounds on unauthenticated connections, each with its
/// bound: an IPv4 address (also one mapped into IPv6) alone; an IPv6 address in its /64 and in
/// its /48.
pub fn groups(ip: IpAddr) -> Vec<(Group, usize)> {
    let cut = |v6: Ipv6Addr, bits: u8| Group {
        prefix: IpAddr::V6(Ipv6Addr::from_bits(
            v6.to_bits() & (u128::MAX << (128 - u32::from(bits))),
        )),
        bits,
    };
    match source(ip) {
        IpAddr::V4(v4) => vec![(
            Group {
                prefix: IpAddr::V4(v4),
                bits: 32,
            },
            MAX_UNAUTHENTICATED_PER_IP,
        )],
        IpAddr::V6(v6) => vec![
            (cut(v6, 64), MAX_UNAUTHENTICATED_PER_IP),
            (cut(v6, 48), MAX_UNAUTHENTICATED_PER_48),
        ],
    }
}

/// How many places each group holds of the unauthenticated connections.
#[derive(Clone, Default)]
pub struct Places(Arc<Mutex<HashMap<Group, usize>>>);

impl Places {
    fn lock(&self) -> MutexGuard<'_, HashMap<Group, usize>> {
        self.0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// A place in every group of `ip`, held until the [`Place`] is dropped; none when any of
    /// them holds its bound already.
    pub fn take(&self, ip: IpAddr) -> Option<Place> {
        let groups = groups(ip);
        let mut held = self.lock();
        if groups
            .iter()
            .any(|(group, most)| held.get(group).is_some_and(|count| count >= most))
        {
            return None;
        }
        for (group, _) in &groups {
            *held.entry(*group).or_insert(0) += 1;
        }
        Some(Place {
            places: self.clone(),
            groups: groups.into_iter().map(|(group, _)| group).collect(),
        })
    }
}

/// One connection's places, given back on drop.
pub struct Place {
    places: Places,
    groups: Vec<Group>,
}

impl Drop for Place {
    fn drop(&mut self) {
        let mut held = self.places.lock();
        for group in &self.groups {
            if let Some(count) = held.get_mut(group) {
                *count -= 1;
                if *count == 0 {
                    held.remove(group);
                }
            }
        }
    }
}

/// Places of something held at most `most` at once, all sources together.
pub struct Share {
    held: Arc<std::sync::atomic::AtomicUsize>,
    most: usize,
}

impl Share {
    pub fn new(most: usize) -> Share {
        Share {
            held: Arc::default(),
            most,
        }
    }

    /// One place, held until the [`Shared`] is dropped; none when all are taken.
    pub fn take(&self) -> Option<Shared> {
        use std::sync::atomic::Ordering;
        self.held
            .try_update(Ordering::AcqRel, Ordering::Acquire, |held| {
                (held < self.most).then_some(held + 1)
            })
            .ok()
            .map(|_| Shared(Arc::clone(&self.held)))
    }
}

/// One place of a [`Share`], given back on drop.
pub struct Shared(Arc<std::sync::atomic::AtomicUsize>);

impl Drop for Shared {
    fn drop(&mut self) {
        self.0.fetch_sub(1, std::sync::atomic::Ordering::AcqRel);
    }
}

/// Failure counters per key, with the delay the spec asks for: after `free`
/// failures (three by default) the next attempt waits `min(2^(failures-free),
/// 300)` seconds from the last failure. Nothing locks: a correct password
/// after the delay always works.
pub struct Counters<K> {
    map: HashMap<K, Failures>,
    free: u32,
}

impl<K: Eq + Hash + Clone> Default for Counters<K> {
    fn default() -> Self {
        Self::new()
    }
}

impl<K: Eq + Hash + Clone> Counters<K> {
    pub fn new() -> Self {
        Self::with_free(FREE_FAILURES)
    }

    pub fn with_free(free: u32) -> Self {
        Counters {
            map: HashMap::new(),
            free,
        }
    }

    /// How long `key` must still wait, if at all.
    pub fn wait(&self, key: &K, now: Instant) -> Duration {
        let free = self.free;
        self.map
            .get(key)
            .map_or(Duration::ZERO, |f| f.wait(free, now))
    }

    pub fn fail(&mut self, key: &K, now: Instant) {
        if self.map.len() >= MAX_ENTRIES && !self.map.contains_key(key) {
            self.map.retain(|_, f| now.duration_since(f.last) < DECAY);
            if self.map.len() >= MAX_ENTRIES {
                return;
            }
        }
        let entry = self.map.entry(key.clone()).or_insert(Failures {
            count: 0,
            last: now,
        });
        if now.duration_since(entry.last) >= DECAY {
            entry.count = 0;
        }
        entry.count = entry.count.saturating_add(1);
        entry.last = now;
    }

    pub fn clear(&mut self, key: &K) {
        self.map.remove(key);
    }
}

/// The counters of the brute-force defence. The delay an attempt earns is
/// keyed by its address, by its (login, address) pair, and, at a much higher
/// threshold, by its login alone. The pair is what keeps a stranger's failures
/// from delaying the owner of a login from the owner's own address.
pub struct LoginThrottle {
    by_ip: Counters<IpAddr>,
    by_pair: Counters<(IpAddr, String)>,
    by_login: Counters<String>,
}

impl Default for LoginThrottle {
    fn default() -> Self {
        Self::new()
    }
}

impl LoginThrottle {
    pub fn new() -> Self {
        LoginThrottle {
            by_ip: Counters::new(),
            by_pair: Counters::new(),
            by_login: Counters::with_free(LOGIN_FREE_FAILURES),
        }
    }

    /// How long this attempt must wait, whichever counter says longest.
    pub fn wait(&self, ip: IpAddr, login: &str, now: Instant) -> Duration {
        let ip_wait = self.by_ip.wait(&ip, now);
        if !valid_login(login) {
            return ip_wait;
        }
        let pair = self.by_pair.wait(&(ip, login.to_owned()), now);
        let any_source = self.by_login.wait(&login.to_owned(), now);
        ip_wait.max(pair).max(any_source)
    }

    /// Counts a failure against the address, and against the login when it
    /// is one an account can have: any other is up to 64 KiB of the
    /// attacker's choosing, and a counter for each would fill the memory.
    pub fn failed(&mut self, ip: IpAddr, login: &str, now: Instant) {
        self.by_ip.fail(&ip, now);
        if valid_login(login) {
            self.by_pair.fail(&(ip, login.to_owned()), now);
            self.by_login.fail(&login.to_owned(), now);
        }
    }

    /// A correct password clears the login's counters, not the address's: one
    /// account of one's own must not wash away the guesses at others.
    pub fn succeeded(&mut self, ip: IpAddr, login: &str) {
        self.by_pair.clear(&(ip, login.to_owned()));
        self.by_login.clear(&login.to_owned());
    }
}

/// Events per key within a window, at most `limit`.
pub struct Windowed<K> {
    map: HashMap<K, (u32, Instant)>,
}

impl<K: Eq + Hash + Clone> Default for Windowed<K> {
    fn default() -> Self {
        Self::new()
    }
}

impl<K: Eq + Hash + Clone> Windowed<K> {
    pub fn new() -> Self {
        Windowed {
            map: HashMap::new(),
        }
    }

    /// Counts one event, or says how long until the window of `key` renews.
    pub fn take(
        &mut self,
        key: &K,
        now: Instant,
        limit: u32,
        window: Duration,
    ) -> Result<(), Duration> {
        if self.map.len() >= MAX_ENTRIES && !self.map.contains_key(key) {
            self.map
                .retain(|_, (_, start)| now.duration_since(*start) < window);
        }
        let entry = self.map.entry(key.clone()).or_insert((0, now));
        if now.duration_since(entry.1) >= window {
            *entry = (0, now);
        }
        if entry.0 >= limit {
            return Err(window.saturating_sub(now.duration_since(entry.1)));
        }
        entry.0 += 1;
        Ok(())
    }
}

/// A token bucket: `rate` tokens a second up to `burst`.
pub struct TokenBucket {
    tokens: f64,
    rate: f64,
    burst: f64,
    last: Instant,
}

impl TokenBucket {
    pub fn new(rate: u32, burst: u32, now: Instant) -> Self {
        TokenBucket {
            tokens: f64::from(burst),
            rate: f64::from(rate),
            burst: f64::from(burst),
            last: now,
        }
    }

    /// Takes one token, or says in milliseconds when one will exist.
    pub fn take(&mut self, now: Instant) -> Result<(), u32> {
        let elapsed = now.saturating_duration_since(self.last).as_secs_f64();
        self.tokens = (self.tokens + elapsed * self.rate).min(self.burst);
        self.last = now;
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            Ok(())
        } else {
            Err((((1.0 - self.tokens) / self.rate) * 1000.0).ceil() as u32)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_delay_starts_after_three_failures_and_doubles_to_a_ceiling() {
        let start = Instant::now();
        let mut counters = Counters::new();
        for _ in 0..3 {
            counters.fail(&"a", start);
        }
        assert_eq!(counters.wait(&"a", start), Duration::from_secs(1));
        counters.fail(&"a", start);
        assert_eq!(counters.wait(&"a", start), Duration::from_secs(2));
        for _ in 0..40 {
            counters.fail(&"a", start);
        }
        assert_eq!(counters.wait(&"a", start), Duration::from_secs(300));
        assert_eq!(
            counters.wait(&"a", start + Duration::from_secs(301)),
            Duration::ZERO
        );
        assert_eq!(counters.wait(&"b", start), Duration::ZERO);
    }

    #[test]
    fn unauthenticated_places_are_bounded_per_ipv6_48_too() {
        let places = Places::default();
        let held: Vec<Place> = (0..MAX_UNAUTHENTICATED_PER_48)
            .map(|n| {
                let ip: IpAddr = format!("2001:db8:1:{n:x}::1").parse().expect("address");
                places.take(ip).expect("room in the /48")
            })
            .collect();
        let same_48: IpAddr = "2001:db8:1:ffff::1".parse().expect("address");
        let other_48: IpAddr = "2001:db8:2::1".parse().expect("address");
        assert!(places.take(same_48).is_none(), "the /48 is full");
        assert!(places.take(other_48).is_some(), "another /48 is not");
        drop(held);
        assert!(places.take(same_48).is_some(), "the places are given back");
    }

    #[test]
    fn one_ipv6_subscriber_is_one_source() {
        let a: IpAddr = "2001:db8:1:2:aaaa::1".parse().expect("address");
        let b: IpAddr = "2001:db8:1:2:ffff:1:2:3".parse().expect("address");
        let other: IpAddr = "2001:db8:1:3::1".parse().expect("address");
        assert_eq!(source(a), source(b));
        assert_ne!(source(a), source(other));
        let mapped: IpAddr = "::ffff:192.0.2.7".parse().expect("address");
        assert_eq!(source(mapped), IpAddr::from([192, 0, 2, 7]));
    }

    #[test]
    fn only_logins_an_account_can_have_get_a_counter() {
        let now = Instant::now();
        let mut throttle = LoginThrottle::new();
        let ip = IpAddr::from([192, 0, 2, 1]);
        for i in 0..100 {
            throttle.failed(ip, &format!("{i}{}", "x".repeat(60_000)), now);
        }
        throttle.failed(ip, "carol", now);
        assert_eq!(throttle.by_pair.map.len(), 1);
        assert!(throttle.wait(ip, "anyone", now) > Duration::ZERO);
    }
}
