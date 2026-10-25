//! The service's configuration file. Everything radovid knows about a game is
//! here: its rulesets, and how to launch one game server per match.

use std::collections::BTreeMap;
use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};

use radovid_proto::*;
use serde::Deserialize;

use crate::password::PasswordCost;
use crate::template;

#[derive(Deserialize, Clone, Debug)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// Where the one TLS listener binds.
    pub listen: SocketAddr,
    /// The SQLite file.
    pub database: PathBuf,
    /// PEM certificate chain and private key of the listener; reread on SIGHUP.
    pub tls_cert: PathBuf,
    pub tls_key: PathBuf,
    /// Host ports handed to game servers, one per launched match: `"20000-20100"`.
    pub game_ports: PortRange,
    /// Connections held at once at most, logged in or not; one past it is
    /// closed as soon as it is accepted.
    #[serde(default = "default_max_connections")]
    pub max_connections: usize,
    /// What the server calls itself in `Welcome`.
    #[serde(default = "default_server_name")]
    pub server_name: String,
    #[serde(default)]
    pub password: PasswordCost,
    #[serde(default)]
    pub limits: LimitsConfig,
    #[serde(rename = "ruleset")]
    pub rulesets: Vec<RulesetConfig>,
}

fn default_max_connections() -> usize {
    2048
}

fn default_server_name() -> String {
    "radovid".to_owned()
}

/// An inclusive range of ports, written `"first-last"`.
#[derive(Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(try_from = "String")]
pub struct PortRange {
    pub first: u16,
    pub last: u16,
}

impl TryFrom<String> for PortRange {
    type Error = String;

    fn try_from(text: String) -> Result<Self, Self::Error> {
        let bad = || format!("game_ports {text:?} is not \"first-last\" with 1 <= first <= last");
        let (first, last) = text.split_once('-').ok_or_else(bad)?;
        let first: u16 = first.trim().parse().map_err(|_| bad())?;
        let last: u16 = last.trim().parse().map_err(|_| bad())?;
        if first == 0 || first > last {
            return Err(bad());
        }
        Ok(PortRange { first, last })
    }
}

impl PortRange {
    pub fn ports(&self) -> impl Iterator<Item = u16> {
        self.first..=self.last
    }

    pub fn count(&self) -> usize {
        usize::from(self.last - self.first) + 1
    }
}

/// One number per account kind.
#[derive(Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PerKind {
    pub human: u8,
    pub bot: u8,
}

impl PerKind {
    pub fn of(&self, kind: AccountKind) -> u8 {
        match kind {
            AccountKind::Human => self.human,
            AccountKind::Bot => self.bot,
        }
    }
}

/// A per-kind number, or one number for both kinds.
#[derive(Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(untagged)]
pub enum PerKindOrBoth {
    Both(u8),
    PerKind(PerKind),
}

impl PerKindOrBoth {
    fn per_kind(self) -> PerKind {
        match self {
            PerKindOrBoth::Both(n) => PerKind { human: n, bot: n },
            PerKindOrBoth::PerKind(per_kind) => per_kind,
        }
    }
}

/// How much play the service takes on, and what an account may hold.
#[derive(Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields, default)]
pub struct LimitsConfig {
    /// Games in flight per account: queue entries, lobbies, join requests and
    /// unfinished matches.
    pub games_per_account: PerKind,
    /// Games in flight of all accounts of one source (an IPv4 address or an
    /// IPv6 /64).
    pub games_per_source: u8,
    /// Seconds to accept a proposal; silence counts as a decline.
    pub accept_timeout_secs: u32,
    /// Seconds out of the queues after a decline or a silence.
    pub decline_cooldown_secs: u32,
    /// Games launched at once; accepted matches wait for a slot in order.
    pub game_capacity: u32,
    /// Of those, the most lobby games may hold, so that queued matches always
    /// have slots of their own.
    pub lobby_games_max: u32,
    /// Queue entries across every ruleset; past it `QueueFull`.
    pub queue_capacity: u32,
    /// Days a match's artifact is kept, and the most bytes (MiB) all of a ruleset's artifacts may
    /// take: older and then the oldest go first.
    pub artifact_retention_days: u32,
    pub artifact_cap_mib: u32,
}

impl Default for LimitsConfig {
    fn default() -> Self {
        LimitsConfig {
            games_per_account: PerKind { human: 2, bot: 8 },
            games_per_source: 8,
            accept_timeout_secs: 30,
            decline_cooldown_secs: 120,
            game_capacity: 8,
            lobby_games_max: 6,
            queue_capacity: 256,
            artifact_retention_days: 14,
            artifact_cap_mib: 2048,
        }
    }
}

/// One entry of the ruleset catalog.
#[derive(Deserialize, Clone, Debug)]
#[serde(deny_unknown_fields)]
pub struct RulesetConfig {
    /// `[a-z0-9_]`, never reused for other rules.
    pub id: String,
    pub name: String,
    /// The game's label for clients; radovid never interprets it.
    pub game: String,
    /// 16 hex digits: the game wire a client must name in its Hello.
    #[serde(deserialize_with = "hex_u64")]
    pub game_fingerprint: u64,
    /// Team sizes; slots are numbered team by team.
    pub teams: Vec<u8>,
    /// Has a queue and a ladder; its queued matches are rated.
    pub rated: bool,
    /// Hex bytes shown to clients as `RulesetInfo.info`.
    #[serde(default, deserialize_with = "hex_bytes")]
    pub info: Vec<u8>,
    /// A match still running after this long is stopped (a null).
    #[serde(default = "default_max_duration")]
    pub max_duration_secs: u64,
    /// Overrides `[limits] games_per_account` for this ruleset.
    pub games_per_account: Option<PerKindOrBoth>,
    /// Who may queue and sit in lobbies here; the others are not even told of the ruleset.
    #[serde(default)]
    pub audience: AudienceConfig,
    /// Players a lobby needs before its host may start it; the seats when absent. Fewer lets a
    /// host start alone: the game's own launcher arguments fill the empty seats.
    pub lobby_min_players: Option<u8>,
    pub launcher: LauncherConfig,
}

/// A ruleset's audience as the configuration spells it.
#[derive(Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AudienceConfig {
    #[default]
    Everyone,
    HumansOnly,
    BotsOnly,
}

impl From<AudienceConfig> for Audience {
    fn from(audience: AudienceConfig) -> Audience {
        match audience {
            AudienceConfig::Everyone => Audience::Everyone,
            AudienceConfig::HumansOnly => Audience::HumansOnly,
            AudienceConfig::BotsOnly => Audience::BotsOnly,
        }
    }
}

fn default_max_duration() -> u64 {
    1800
}

/// How a ruleset's game servers are started: a local process or a docker
/// container, with arguments and environment in which radovid expands
/// `{match_id}`, `{port}`, `{seats}`, `{ticket_<slot>}`, `{team_<slot>}` and
/// `{name_<slot>}` (`{{` and `}}` are braces).
#[derive(Deserialize, Clone, Debug)]
#[serde(deny_unknown_fields)]
pub struct LauncherConfig {
    pub kind: LauncherKind,
    /// local: the executable.
    pub binary: Option<PathBuf>,
    /// docker: the image, and the port the game listens on inside it.
    pub image: Option<String>,
    pub container_port: Option<u16>,
    /// docker: the client binary (default `docker`), the network (default
    /// docker's own), the limits of the container (512m, 1 CPU, 256 pids),
    /// the host address the game port is published on (default 0.0.0.0) and
    /// read-only `-v` mounts.
    pub docker: Option<String>,
    pub network: Option<String>,
    pub memory: Option<String>,
    pub cpus: Option<String>,
    pub pids_limit: Option<u32>,
    pub publish_address: Option<IpAddr>,
    #[serde(default)]
    pub mounts: Vec<String>,
    /// What players are told to connect to, e.g. `wss://game.example.org:{port}`.
    /// Anything but `wss://` must name a loopback host, unless `insecure_plaintext`.
    pub public_url: String,
    /// Allow a `public_url` that is not `wss://` to name another host: the tickets
    /// then cross the network in clear.
    #[serde(default)]
    pub insecure_plaintext: bool,
    /// Arguments that make the game print its wire fingerprint (16 hex
    /// digits) and exit; checked against `game_fingerprint` at start.
    #[serde(default)]
    pub version_probe: Vec<String>,
    #[serde(default)]
    pub args: Vec<String>,
    /// Environment; the only place tickets may go (`{seats}`, `{ticket_<slot>}`).
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// Seconds from the start to the game's `READY` line at most.
    #[serde(default = "default_start_timeout")]
    pub start_timeout_secs: u64,
    /// Where the game leaves the match's replay, as seen by radovid: an absolute path whose file
    /// name holds `{match_id}` (a docker game's mounted directory). Seats of the match fetch it.
    pub artifact: Option<String>,
}

fn default_start_timeout() -> u64 {
    60
}

#[derive(Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LauncherKind {
    Local,
    Docker,
}

fn hex_u64<'de, D: serde::Deserializer<'de>>(de: D) -> Result<u64, D::Error> {
    let text = String::deserialize(de)?;
    parse_fingerprint(&text)
        .ok_or_else(|| serde::de::Error::custom(format!("{text:?} is not 16 hex digits")))
}

/// A wire fingerprint as games print it: 16 hex digits.
pub fn parse_fingerprint(text: &str) -> Option<u64> {
    let text = text.trim();
    (text.len() == 16 && text.bytes().all(|b| b.is_ascii_hexdigit()))
        .then(|| u64::from_str_radix(text, 16).ok())
        .flatten()
}

fn hex_bytes<'de, D: serde::Deserializer<'de>>(de: D) -> Result<Vec<u8>, D::Error> {
    let text = String::deserialize(de)?;
    decode_hex(&text).ok_or_else(|| serde::de::Error::custom("info is not hex bytes"))
}

/// Pairs of hex digits as bytes.
pub fn decode_hex(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) {
        return None;
    }
    text.as_bytes()
        .chunks(2)
        .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).ok()?, 16).ok())
        .collect()
}

impl RulesetConfig {
    pub fn ruleset_id(&self) -> RulesetId {
        RulesetId(self.id.clone())
    }

    pub fn seats(&self) -> usize {
        self.teams.iter().map(|&n| usize::from(n)).sum()
    }

    /// The team of a slot: slots are numbered team by team.
    pub fn team_of(&self, slot: usize) -> u8 {
        let mut first = 0;
        for (team, &size) in self.teams.iter().enumerate() {
            first += usize::from(size);
            if slot < first {
                return team as u8;
            }
        }
        0
    }

    pub fn games_per_account(&self, limits: &LimitsConfig, kind: AccountKind) -> u8 {
        self.games_per_account
            .map_or(limits.games_per_account, PerKindOrBoth::per_kind)
            .of(kind)
    }

    pub fn info(&self, limits: &LimitsConfig, kind: AccountKind) -> RulesetInfo {
        RulesetInfo {
            id: self.ruleset_id(),
            name: self.name.clone(),
            game: self.game.clone(),
            game_fingerprint: self.game_fingerprint,
            teams: self.teams.clone(),
            rated: self.rated,
            info: self.info.clone(),
            games_per_account: self.games_per_account(limits, kind),
            audience: self.audience.into(),
            lobby_min_players: self.lobby_min_players(),
        }
    }

    /// Players a lobby of this ruleset needs before its host starts it.
    pub fn lobby_min_players(&self) -> u8 {
        self.lobby_min_players
            .unwrap_or_else(|| u8::try_from(self.seats()).unwrap_or(u8::MAX))
    }

    /// Whether an account of `kind` may queue and sit in lobbies here.
    pub fn admits(&self, kind: AccountKind) -> bool {
        Audience::from(self.audience).admits(kind)
    }
}

impl Config {
    pub fn load(path: &Path) -> Result<Config, String> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        let config: Config =
            toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.rulesets.is_empty() {
            return Err("no ruleset is configured".to_owned());
        }
        if self.max_connections == 0 {
            return Err("max_connections must be at least 1".to_owned());
        }
        self.validate_limits()?;
        for (i, ruleset) in self.rulesets.iter().enumerate() {
            if self.rulesets[..i].iter().any(|r| r.id == ruleset.id) {
                return Err(format!("ruleset {} is configured twice", ruleset.id));
            }
            validate_ruleset(ruleset).map_err(|e| format!("ruleset {}: {e}", ruleset.id))?;
        }
        Ok(())
    }

    fn validate_limits(&self) -> Result<(), String> {
        let limits = &self.limits;
        if limits.games_per_account.human == 0 || limits.games_per_account.bot == 0 {
            return Err("limits.games_per_account must be at least 1".to_owned());
        }
        if limits.games_per_source == 0 || limits.queue_capacity == 0 {
            return Err("limits.games_per_source and queue_capacity must be at least 1".to_owned());
        }
        if !(1..=600).contains(&limits.accept_timeout_secs) {
            return Err("limits.accept_timeout_secs must be 1 to 600".to_owned());
        }
        if limits.decline_cooldown_secs > 86_400 {
            return Err("limits.decline_cooldown_secs must be at most a day".to_owned());
        }
        if limits.game_capacity == 0 || limits.lobby_games_max > limits.game_capacity {
            return Err(
                "limits.game_capacity must be at least 1 and at least lobby_games_max".to_owned(),
            );
        }
        if !(1..=365).contains(&limits.artifact_retention_days) || limits.artifact_cap_mib == 0 {
            return Err(
                "limits.artifact_retention_days must be 1 to 365 and artifact_cap_mib at least 1"
                    .to_owned(),
            );
        }
        if self.game_ports.count() < limits.game_capacity as usize {
            return Err(format!(
                "game_ports holds {} ports for a game_capacity of {}",
                self.game_ports.count(),
                limits.game_capacity
            ));
        }
        Ok(())
    }

    pub fn ruleset(&self, id: &RulesetId) -> Option<&RulesetConfig> {
        self.rulesets.iter().find(|r| r.id == id.0)
    }

    /// The limits as they apply to an account of `kind`, for `ruleset` if named.
    pub fn limits_for(&self, kind: AccountKind, ruleset: Option<&RulesetConfig>) -> Limits {
        let limits = &self.limits;
        Limits {
            games_per_account: ruleset.map_or(limits.games_per_account.of(kind), |r| {
                r.games_per_account(limits, kind)
            }),
            accept_timeout_ms: limits.accept_timeout_secs * 1000,
            decline_cooldown_ms: limits.decline_cooldown_secs * 1000,
            queue_capacity: limits.queue_capacity,
            game_capacity: limits.game_capacity,
            lobby_games_max: limits.lobby_games_max,
            games_per_source: limits.games_per_source,
        }
    }

    pub fn ruleset_infos(&self, kind: AccountKind) -> Vec<RulesetInfo> {
        self.rulesets
            .iter()
            .filter(|r| r.admits(kind))
            .map(|r| r.info(&self.limits, kind))
            .collect()
    }
}

fn validate_ruleset(ruleset: &RulesetConfig) -> Result<(), String> {
    if !valid_ruleset_id(&ruleset.id) {
        return Err("the id is not [a-z0-9_] of 1 to 32 bytes".to_owned());
    }
    if ruleset.name.is_empty() || ruleset.name.len() > 64 {
        return Err("needs a name of 1 to 64 bytes".to_owned());
    }
    if ruleset.game.is_empty()
        || ruleset.game.len() > 32
        || ruleset.game.chars().any(char::is_control)
    {
        return Err("needs a game label of 1 to 32 bytes".to_owned());
    }
    if ruleset.teams.len() < 2 || ruleset.teams.contains(&0) || ruleset.seats() > MAX_SEATS {
        return Err(format!(
            "needs two or more teams of at least one, {MAX_SEATS} seats at most"
        ));
    }
    if ruleset
        .lobby_min_players
        .is_some_and(|n| n == 0 || usize::from(n) > ruleset.seats())
    {
        return Err("lobby_min_players is 1 to the number of seats".to_owned());
    }
    if ruleset.rated && ruleset.teams != [1, 1] {
        return Err("a rated ruleset is two teams of one: teams = [1, 1]".to_owned());
    }
    if ruleset.info.len() > MAX_RULESET_INFO_LEN {
        return Err(format!("info is over {MAX_RULESET_INFO_LEN} bytes"));
    }
    if ruleset.max_duration_secs == 0 {
        return Err("max_duration_secs must be at least 1".to_owned());
    }
    if let Some(per) = ruleset.games_per_account.map(PerKindOrBoth::per_kind)
        && (per.human == 0 || per.bot == 0)
    {
        return Err("games_per_account must be at least 1".to_owned());
    }
    validate_launcher(&ruleset.launcher, ruleset.seats())
}

fn validate_launcher(launcher: &LauncherConfig, seats: usize) -> Result<(), String> {
    let docker_only = launcher.image.is_some()
        || launcher.container_port.is_some()
        || launcher.docker.is_some()
        || launcher.network.is_some()
        || launcher.memory.is_some()
        || launcher.cpus.is_some()
        || launcher.pids_limit.is_some()
        || launcher.publish_address.is_some()
        || !launcher.mounts.is_empty();
    match launcher.kind {
        LauncherKind::Local if launcher.binary.is_none() || docker_only => {
            return Err("a local launcher has a binary and no docker settings".to_owned());
        }
        LauncherKind::Docker
            if launcher.binary.is_some()
                || launcher.image.is_none()
                || launcher.container_port.is_none() =>
        {
            return Err(
                "a docker launcher has an image and a container_port, no binary".to_owned(),
            );
        }
        _ => {}
    }
    if launcher.start_timeout_secs == 0 {
        return Err("start_timeout_secs must be at least 1".to_owned());
    }
    if let Some(artifact) = &launcher.artifact {
        crate::artifacts::Template::parse(artifact)?;
    }
    template::check(&launcher.public_url, seats, false).map_err(|e| format!("public_url: {e}"))?;
    if !launcher.insecure_plaintext && !stays_private(&launcher.public_url) {
        return Err(format!(
            "public_url {:?} would send tickets across the network in clear: use wss://, a \
loopback host, or set insecure_plaintext = true for a network you trust",
            launcher.public_url
        ));
    }
    let artifact = launcher.artifact.is_some();
    for arg in &launcher.args {
        template::check_with(arg, seats, false, artifact).map_err(|e| format!("args: {e}"))?;
    }
    for (name, value) in &launcher.env {
        if name.is_empty() || !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
            return Err(format!("env name {name:?} is not [A-Za-z0-9_]"));
        }
        template::check_with(value, seats, true, artifact)
            .map_err(|e| format!("env {name}: {e}"))?;
    }
    Ok(())
}

/// Whether a game URL keeps its traffic out of other hands: TLS (`wss://`),
/// or a loopback host.
fn stays_private(url: &str) -> bool {
    if url.starts_with("wss://") {
        return true;
    }
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    let authority = rest.split('/').next().unwrap_or_default();
    let host = match authority.strip_prefix('[') {
        Some(v6) => v6.split(']').next().unwrap_or_default(),
        None => authority
            .rsplit_once(':')
            .map_or(authority, |(host, _port)| host),
    };
    host.eq_ignore_ascii_case("localhost")
        || host.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_example_configuration_loads() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../radovid.example.toml");
        let config = Config::load(&path).expect("radovid.example.toml");
        assert_eq!(config.limits, LimitsConfig::default());
    }
}
