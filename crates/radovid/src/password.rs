//! argon2id hashing on the blocking pool: at most a few hashes at once, one
//! per source, and a bounded line of waiting requests.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::{Arc, Mutex, MutexGuard};

use argon2::password_hash::{PasswordHasher, PasswordVerifier};
use argon2::{Algorithm, Argon2, Params, Version};
use radovid_proto::Error;
use serde::Deserialize;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

/// Hashes running at once at most: a bound on memory and a natural rate limit.
const HASHES_IN_FLIGHT: usize = 4;
/// Requests waiting for their turn at most, unless configured; the next one is
/// told to come back.
const MAX_WAITING: usize = 64;
/// What a turned-away request is told to wait.
const BUSY_RETRY_MS: u32 = 1000;

/// What one hash costs.
#[derive(Deserialize, Clone, Copy, Debug)]
#[serde(deny_unknown_fields)]
pub struct PasswordCost {
    /// Memory in KiB.
    pub memory_kib: u32,
    pub iterations: u32,
    /// Hash requests that may wait for their turn; past it, requests are
    /// answered `Throttled`.
    #[serde(default = "default_max_waiting")]
    pub max_waiting: usize,
}

fn default_max_waiting() -> usize {
    MAX_WAITING
}

impl Default for PasswordCost {
    fn default() -> Self {
        PasswordCost {
            memory_kib: 64 * 1024,
            iterations: 3,
            max_waiting: MAX_WAITING,
        }
    }
}

/// Who is in the line.
#[derive(Default)]
struct Line {
    /// The one-permit gate of each source with requests in the line, and how
    /// many they are.
    sources: HashMap<IpAddr, (Arc<Semaphore>, usize)>,
    waiting: usize,
}

/// A request's place in the line, given back on drop (also when the request
/// is cancelled).
struct Place<'a> {
    line: &'a Mutex<Line>,
    source: IpAddr,
    queued: bool,
}

impl Place<'_> {
    /// The request has its turn and no longer waits.
    fn dequeue(&mut self) {
        if std::mem::take(&mut self.queued) {
            lock(self.line).waiting -= 1;
        }
    }
}

impl Drop for Place<'_> {
    fn drop(&mut self) {
        self.dequeue();
        let mut line = lock(self.line);
        if let Some((_, users)) = line.sources.get_mut(&self.source) {
            *users -= 1;
            if *users == 0 {
                line.sources.remove(&self.source);
            }
        }
    }
}

/// The right to hash: the source's turn, and one of the global places.
struct Turn<'a> {
    _own: OwnedSemaphorePermit,
    _global: OwnedSemaphorePermit,
    _place: Place<'a>,
}

fn lock(mutex: &Mutex<Line>) -> MutexGuard<'_, Line> {
    mutex.lock().unwrap_or_else(|p| p.into_inner())
}

/// Hashes and checks passwords.
#[derive(Clone)]
pub struct Passwords {
    cost: PasswordCost,
    gate: Arc<Semaphore>,
    line: Arc<Mutex<Line>>,
    /// Checked when no account exists, so that "no such login" costs what
    /// "wrong password" does.
    decoy: Arc<String>,
}

impl Passwords {
    pub fn new(cost: PasswordCost) -> Result<Passwords, String> {
        let hasher = hasher(cost)?;
        let decoy = hash_with(&hasher, "decoy password for unknown logins")?;
        Ok(Passwords {
            cost,
            gate: Arc::new(Semaphore::new(HASHES_IN_FLIGHT)),
            line: Arc::default(),
            decoy: Arc::new(decoy),
        })
    }

    /// Waits for `source`'s turn, or says the line is full. One source hashes
    /// one password at a time, so a script cannot crowd out the others.
    async fn turn(&self, source: IpAddr) -> Result<Turn<'_>, Error> {
        let (gate, mut place) = {
            let mut line = lock(&self.line);
            if line.waiting >= self.cost.max_waiting {
                return Err(Error::Throttled {
                    retry_after_ms: BUSY_RETRY_MS,
                });
            }
            line.waiting += 1;
            let entry = line
                .sources
                .entry(source)
                .or_insert_with(|| (Arc::new(Semaphore::new(1)), 0));
            entry.1 += 1;
            let gate = Arc::clone(&entry.0);
            let place = Place {
                line: &self.line,
                source,
                queued: true,
            };
            (gate, place)
        };
        let own = gate.acquire_owned().await.map_err(crate::hub::internal)?;
        let global = Arc::clone(&self.gate)
            .acquire_owned()
            .await
            .map_err(crate::hub::internal)?;
        place.dequeue();
        Ok(Turn {
            _own: own,
            _global: global,
            _place: place,
        })
    }

    pub async fn hash(&self, source: IpAddr, password: String) -> Result<String, Error> {
        let cost = self.cost;
        let _turn = self.turn(source).await?;
        tokio::task::spawn_blocking(move || hash_with(&hasher(cost)?, &password))
            .await
            .map_err(crate::hub::internal)?
            .map_err(crate::hub::internal)
    }

    /// Whether `password` matches `stored`; with no stored hash, the decoy is
    /// checked and the answer is no.
    pub async fn verify(
        &self,
        source: IpAddr,
        password: String,
        stored: Option<String>,
    ) -> Result<bool, Error> {
        let known = stored.is_some();
        let stored = stored.unwrap_or_else(|| (*self.decoy).clone());
        let _turn = self.turn(source).await?;
        let matched = tokio::task::spawn_blocking(move || {
            Argon2::default()
                .verify_password(password.as_bytes(), stored.as_str())
                .is_ok()
        })
        .await
        .unwrap_or(false);
        Ok(known && matched)
    }
}

fn hasher(cost: PasswordCost) -> Result<Argon2<'static>, String> {
    let params =
        Params::new(cost.memory_kib, cost.iterations, 1, None).map_err(|e| e.to_string())?;
    Ok(Argon2::new(Algorithm::Argon2id, Version::V0x13, params))
}

fn hash_with(hasher: &Argon2<'_>, password: &str) -> Result<String, String> {
    let mut salt = [0u8; 16];
    getrandom::fill(&mut salt).map_err(|e| e.to_string())?;
    hasher
        .hash_password_with_salt(password.as_bytes(), &salt)
        .map(|hash| hash.to_string())
        .map_err(|e| e.to_string())
}
