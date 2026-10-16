//! The service's configuration file.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use radovid_proto::{HeroId, RulesetId, RulesetInfo, valid_ruleset_id};
use serde::Deserialize;

use crate::password::PasswordCost;

#[derive(Deserialize, Clone, Debug)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// Where the one TLS listener binds.
    pub listen: SocketAddr,
    /// The SQLite file.
    pub database: PathBuf,
    /// PEM certificate chain and private key of the listener.
    pub tls_cert: PathBuf,
    pub tls_key: PathBuf,
    /// The `/game` URL clients are told to connect to.
    pub public_game_url: String,
    /// Matches running at once at most; a formed match waits for a slot.
    #[serde(default = "default_max_matches")]
    pub max_matches: usize,
    /// What the server calls itself in `Welcome`.
    #[serde(default = "default_server_name")]
    pub server_name: String,
    #[serde(default)]
    pub password: PasswordCost,
    pub backend: BackendConfig,
    #[serde(rename = "ruleset")]
    pub rulesets: Vec<RulesetConfig>,
}

fn default_max_matches() -> usize {
    8
}

fn default_server_name() -> String {
    "radovid".to_owned()
}

/// Where game servers run.
#[derive(Deserialize, Clone, Debug)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum BackendConfig {
    /// A child process of the service, for development and tests.
    Local {
        /// The `bota_server` executable.
        bota_server: PathBuf,
    },
    /// One docker container per match on a private network.
    Docker {
        image: String,
        network: String,
        #[serde(default = "default_docker")]
        docker: String,
        #[serde(default = "default_memory")]
        memory: String,
        #[serde(default = "default_cpus")]
        cpus: String,
        /// Reach the container on a loopback port the docker daemon publishes,
        /// not on its address in the network: for a rootless daemon, whose
        /// container addresses the host cannot reach.
        #[serde(default)]
        publish_loopback: bool,
    },
}

fn default_docker() -> String {
    "docker".to_owned()
}

fn default_memory() -> String {
    "512m".to_owned()
}

fn default_cpus() -> String {
    "1".to_owned()
}

/// One entry of the ruleset catalog.
#[derive(Deserialize, Clone, Debug)]
#[serde(deny_unknown_fields)]
pub struct RulesetConfig {
    pub id: String,
    pub name: String,
    pub seats: u8,
    /// Hero ids the ruleset allows, for display ahead of a match.
    #[serde(default)]
    pub heroes: Vec<u16>,
    /// Has a queue and a ladder.
    pub queue: bool,
    /// Arguments for the game server that make these rules (`--map 2`, ...).
    #[serde(default)]
    pub bota_args: Vec<String>,
    /// Seconds after the game server came up at which the hero select ends.
    #[serde(default = "default_start_after")]
    pub start_after_secs: u64,
}

fn default_start_after() -> u64 {
    60
}

impl RulesetConfig {
    pub fn info(&self) -> RulesetInfo {
        RulesetInfo {
            id: RulesetId(self.id.clone()),
            name: self.name.clone(),
            seats: self.seats,
            heroes: self.heroes.iter().copied().map(HeroId).collect(),
            queue: self.queue,
        }
    }
}

impl Config {
    pub fn load(path: &Path) -> Result<Config, String> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        let config: Config = toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.rulesets.is_empty() {
            return Err("no ruleset is configured".to_owned());
        }
        if self.max_matches == 0 {
            return Err("max_matches must be at least 1".to_owned());
        }
        for (i, ruleset) in self.rulesets.iter().enumerate() {
            if !valid_ruleset_id(&ruleset.id) {
                return Err(format!("ruleset id {:?} is not [a-z0-9_] of 1 to 32 bytes", ruleset.id));
            }
            if self.rulesets[..i].iter().any(|r| r.id == ruleset.id) {
                return Err(format!("ruleset {} is configured twice", ruleset.id));
            }
            if ruleset.seats < 2 || ruleset.seats % 2 != 0 || ruleset.seats > 10 {
                return Err(format!("ruleset {} needs an even number of seats from 2 to 10", ruleset.id));
            }
            if ruleset.queue && ruleset.seats != 2 {
                return Err(format!("ruleset {}: a queue pairs two accounts", ruleset.id));
            }
            if ruleset.name.is_empty() || ruleset.name.len() > 64 {
                return Err(format!("ruleset {} needs a name of 1 to 64 bytes", ruleset.id));
            }
        }
        Ok(())
    }

    pub fn ruleset(&self, id: &RulesetId) -> Option<&RulesetConfig> {
        self.rulesets.iter().find(|r| r.id == id.0)
    }
}
