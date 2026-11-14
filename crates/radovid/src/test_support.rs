//! Bounded request observations for end-to-end consumers; no payloads or identities retained.
use crate::config::{Config, LimitsConfig, PortRange, RulesetConfig};
use crate::password::PasswordCost;
use crate::supervisor::Patience;
use radovid_proto::ClientMsg;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

/// The configuration of a plain test service, with its database and rulesets given. Every field
/// is set here, so a field added to [`Config`] with a default is set once, and the end-to-end
/// tests and the fuzz targets, which start from this, never stop compiling for it. Callers change
/// what their test needs after it.
pub fn config(database: PathBuf, rulesets: Vec<RulesetConfig>) -> Config {
    Config {
        public_identity: None,
        listen: "127.0.0.1:0".parse().expect("address"),
        database,
        tls_cert: PathBuf::new(),
        tls_key: PathBuf::new(),
        game_ports: PortRange { first: 1, last: 1 },
        max_connections: 1024,
        server_name: "radovid-test".to_owned(),
        password: PasswordCost {
            memory_kib: 1024,
            iterations: 1,
            ..PasswordCost::default()
        },
        limits: LimitsConfig::default(),
        artifacts: Default::default(),
        lobby_teams: vec![5, 5],
        lobby_ruleset: None,
        web: None,
        rulesets,
        patience: Patience::DEFAULT,
    }
}

#[derive(Clone, Copy)]
pub enum Read {
    Leaderboard,
    Matches,
    Lobbies,
    Profile,
}
#[derive(Default)]
pub(crate) struct Requests([AtomicU64; 4]);
impl Requests {
    pub(crate) fn record(&self, message: &ClientMsg) {
        let kind = match message {
            ClientMsg::Leaderboard { .. } => Read::Leaderboard,
            ClientMsg::RecentMatches { .. } => Read::Matches,
            ClientMsg::LobbyList | ClientMsg::LobbySearch { .. } => Read::Lobbies,
            ClientMsg::Account { .. } => Read::Profile,
            _ => return,
        };
        self.0[kind as usize].fetch_add(1, Ordering::Relaxed);
    }
    pub(crate) fn count(&self, kind: Read) -> u64 {
        self.0[kind as usize].load(Ordering::Relaxed)
    }
}
