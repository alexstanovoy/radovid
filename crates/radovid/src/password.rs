//! argon2id hashing on the blocking pool, at most a few hashes at once.

use std::sync::Arc;

use argon2::password_hash::{PasswordHasher, PasswordVerifier};
use argon2::{Algorithm, Argon2, Params, Version};
use serde::Deserialize;
use tokio::sync::Semaphore;

/// Hashes running at once at most: a bound on memory and a natural rate limit.
const HASHES_IN_FLIGHT: usize = 4;

/// What one hash costs.
#[derive(Deserialize, Clone, Copy, Debug)]
#[serde(deny_unknown_fields)]
pub struct PasswordCost {
    /// Memory in KiB.
    pub memory_kib: u32,
    pub iterations: u32,
}

impl Default for PasswordCost {
    fn default() -> Self {
        PasswordCost {
            memory_kib: 64 * 1024,
            iterations: 3,
        }
    }
}

/// Hashes and checks passwords.
#[derive(Clone)]
pub struct Passwords {
    cost: PasswordCost,
    gate: Arc<Semaphore>,
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
            decoy: Arc::new(decoy),
        })
    }

    pub async fn hash(&self, password: String) -> Result<String, String> {
        let cost = self.cost;
        let _permit = self.gate.acquire().await.map_err(|e| e.to_string())?;
        tokio::task::spawn_blocking(move || hash_with(&hasher(cost)?, &password))
            .await
            .map_err(|e| e.to_string())?
    }

    /// Whether `password` matches `stored`; with no stored hash, the decoy is
    /// checked and the answer is no.
    pub async fn verify(&self, password: String, stored: Option<String>) -> bool {
        let known = stored.is_some();
        let stored = stored.unwrap_or_else(|| (*self.decoy).clone());
        let Ok(_permit) = self.gate.acquire().await else {
            return false;
        };
        let matched = tokio::task::spawn_blocking(move || {
            Argon2::default().verify_password(password.as_bytes(), stored.as_str()).is_ok()
        })
        .await
        .unwrap_or(false);
        known && matched
    }
}

fn hasher(cost: PasswordCost) -> Result<Argon2<'static>, String> {
    let params = Params::new(cost.memory_kib, cost.iterations, 1, None).map_err(|e| e.to_string())?;
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
