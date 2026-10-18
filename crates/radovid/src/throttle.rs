//! Bounds on how fast a peer may act: the growing delay after failed logins,
//! and the token bucket on messages.

use std::collections::HashMap;
use std::hash::Hash;
use std::net::IpAddr;
use std::time::{Duration, Instant};

use radovid_proto::valid_login;

/// Failures forgotten after this long without one.
const DECAY: Duration = Duration::from_secs(3600);
/// Failures before the delay starts.
const FREE_FAILURES: u32 = 3;
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
    /// How long until the next attempt may be evaluated.
    fn wait(&self, now: Instant) -> Duration {
        if now.duration_since(self.last) >= DECAY || self.count < FREE_FAILURES {
            return Duration::ZERO;
        }
        let secs = 1u64
            .checked_shl(self.count - FREE_FAILURES)
            .map_or(MAX_DELAY_SECS, |s| s.min(MAX_DELAY_SECS));
        (self.last + Duration::from_secs(secs)).saturating_duration_since(now)
    }
}

/// Failure counters per key, with the delay the spec asks for: after three
/// failures the next attempt waits `min(2^(failures-3), 300)` seconds from the
/// last failure. Nothing locks: a correct password after the delay always works.
pub struct Counters<K> {
    map: HashMap<K, Failures>,
}

impl<K: Eq + Hash + Clone> Default for Counters<K> {
    fn default() -> Self {
        Self::new()
    }
}

impl<K: Eq + Hash + Clone> Counters<K> {
    pub fn new() -> Self {
        Counters {
            map: HashMap::new(),
        }
    }

    /// How long `key` must still wait, if at all.
    pub fn wait(&self, key: &K, now: Instant) -> Duration {
        self.map.get(key).map_or(Duration::ZERO, |f| f.wait(now))
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

/// The two counters of the brute-force defence, per source address and per login.
pub struct LoginThrottle {
    by_ip: Counters<IpAddr>,
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
            by_login: Counters::new(),
        }
    }

    /// How long this attempt must wait, whichever counter says longer.
    pub fn wait(&self, ip: IpAddr, login: &str, now: Instant) -> Duration {
        let by_login = match valid_login(login) {
            true => self.by_login.wait(&login.to_owned(), now),
            false => Duration::ZERO,
        };
        self.by_ip.wait(&ip, now).max(by_login)
    }

    /// Counts a failure against the address, and against the login when it
    /// is one an account can have: any other is up to 64 KiB of the
    /// attacker's choosing, and a counter for each would fill the memory.
    pub fn failed(&mut self, ip: IpAddr, login: &str, now: Instant) {
        self.by_ip.fail(&ip, now);
        if valid_login(login) {
            self.by_login.fail(&login.to_owned(), now);
        }
    }

    /// A correct password clears the login's counter, not the address's: one
    /// account of one's own must not wash away the guesses at others.
    pub fn succeeded(&mut self, login: &str) {
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
    fn only_logins_an_account_can_have_get_a_counter() {
        let now = Instant::now();
        let mut throttle = LoginThrottle::new();
        let ip = IpAddr::from([192, 0, 2, 1]);
        for i in 0..100 {
            throttle.failed(ip, &format!("{i}{}", "x".repeat(60_000)), now);
        }
        throttle.failed(ip, "carol", now);
        assert_eq!(throttle.by_login.map.len(), 1);
        assert!(throttle.wait(ip, "anyone", now) > Duration::ZERO);
    }
}
