//! The running service fed what hostile accounts may send: one service for
//! the whole run (its hub keeps its state from one input to the next, as it
//! would live), four accounts (an admin, two humans and a bot) on control
//! connections of their own.
//!
//! An input is pieces: the first byte picks the account (low two bits) and
//! whether the rest goes out as the raw bytes of a WebSocket message (bit 2)
//! rather than as a request. After the input every connection is dropped and
//! the hub must still let the admin back in: a hub that died or stalls fails
//! the run.
#![no_main]

use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use libfuzzer_sys::fuzz_target;
use radovid::config::{
    Config, LauncherConfig, LauncherKind, LimitsConfig, PerKind, PortRange, RulesetConfig,
};
use radovid::db::Db;
use radovid::password::{PasswordCost, Passwords};
use radovid::service::Service;
use radovid_client::{Client, ClientError, TlsConfig};
use radovid_proto::{
    AccountId, AccountKind, ClientInfo, ClientMsg, Error, SessionToken, decode_payload,
};
use sha2::{Digest, Sha256};
use tokio::signal::unix::{SignalKind, signal};

const FINGERPRINT: u64 = 0x5eed_f00d_0000_0001;
const PASSWORD: &str = "correct horse battery";
/// Pieces one input may send: a few more than a connection's burst.
const MAX_PIECES: usize = 64;
/// How long a raw message's answer is waited for.
const RAW_WAIT: Duration = Duration::from_millis(20);
/// How long the hub has to let the admin back in after an input.
const PROBE_TRIES: usize = 50;

struct Actor {
    kind: AccountKind,
    id: AccountId,
    token: SessionToken,
}

struct Harness {
    runtime: tokio::runtime::Runtime,
    /// Held so that SIGPIPE stays handled: libFuzzer's `main` is not Rust's,
    /// which ignores it, and a write to a connection the service closed must
    /// fail rather than end the run.
    _sigpipe: tokio::signal::unix::Signal,
    _service: Service,
    db: Db,
    passwords: Passwords,
    url: String,
    tls: TlsConfig,
    actors: Vec<Actor>,
    created: u32,
}

fn fake_game() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../crates/radovid/tests/fake_game.sh")
}

fn ruleset(id: &str, rated: bool, teams: Vec<u8>, behaviour: &str, max_secs: u64) -> RulesetConfig {
    RulesetConfig {
        id: id.to_owned(),
        name: id.to_owned(),
        game: "fuzzgame".to_owned(),
        game_fingerprint: FINGERPRINT,
        teams,
        rated,
        info: vec![1],
        max_duration_secs: max_secs,
        games_per_account: None,
        launcher: LauncherConfig {
            kind: LauncherKind::Local,
            binary: Some(fake_game()),
            image: None,
            container_port: None,
            docker: None,
            network: None,
            memory: None,
            cpus: None,
            pids_limit: None,
            publish_address: None,
            mounts: Vec::new(),
            public_url: "wss://game.example.org:{port}/m{match_id}".to_owned(),
            insecure_plaintext: false,
            version_probe: Vec::new(),
            args: [behaviour, "1", "{match_id}", "{port}"]
                .map(str::to_owned)
                .to_vec(),
            env: [("GAME_SEATS".to_owned(), "{seats}".to_owned())].into(),
            start_timeout_secs: 5,
        },
    }
}

fn config(dir: &std::path::Path) -> Config {
    Config {
        listen: "127.0.0.1:0".parse().expect("address"),
        database: dir.join("radovid.db"),
        tls_cert: PathBuf::new(),
        tls_key: PathBuf::new(),
        game_ports: PortRange {
            first: 41000,
            last: 41015,
        },
        max_connections: 256,
        server_name: "fuzz".to_owned(),
        password: PasswordCost {
            memory_kib: 64,
            iterations: 1,
        },
        limits: LimitsConfig {
            games_per_account: PerKind { human: 2, bot: 4 },
            games_per_source: 200,
            accept_timeout_secs: 2,
            decline_cooldown_secs: 1,
            game_capacity: 4,
            lobby_games_max: 3,
            queue_capacity: 32,
        },
        rulesets: vec![
            ruleset("solo", true, vec![1, 1], "team0", 30),
            ruleset("duel", false, vec![1, 1], "draw", 30),
            ruleset("trio", false, vec![2, 1], "crash", 30),
            ruleset("hang", false, vec![1, 1], "hang", 3),
        ],
    }
}

/// A directory of this process's own for the service's database, on disk
/// beside the build; those of runs that are gone are removed first, since a
/// fuzzing process never gets to clean up after itself.
fn scratch() -> PathBuf {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/hub_session");
    if let Ok(entries) = std::fs::read_dir(&root) {
        for entry in entries.flatten() {
            let gone = entry
                .file_name()
                .to_str()
                .and_then(|pid| pid.parse::<u32>().ok())
                .is_some_and(|pid| !std::path::Path::new(&format!("/proc/{pid}")).exists());
            if gone {
                let _ = std::fs::remove_dir_all(entry.path());
            }
        }
    }
    let dir = root.join(std::process::id().to_string());
    std::fs::create_dir_all(&dir).expect("scratch directory");
    dir
}

fn token_hash(token: &SessionToken) -> [u8; 32] {
    Sha256::digest(token.0).into()
}

impl Harness {
    fn start() -> Harness {
        let dir = scratch();
        let identity = radovid::tls::self_signed(vec!["127.0.0.1".to_owned()]).expect("cert");
        let config = config(&dir);
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("runtime");
        let sigpipe = runtime
            .block_on(async { signal(SignalKind::pipe()) })
            .expect("SIGPIPE handler");
        let passwords = Passwords::new(config.password).expect("cost");
        // The service creates its database private; the harness opens it after.
        let database = config.database.clone();
        let service = runtime
            .block_on(Service::start(config, &identity))
            .expect("service");
        let db = Db::open(&database).expect("database");
        let mut harness = Harness {
            url: format!("wss://127.0.0.1:{}", service.addr.port()),
            tls: TlsConfig::trust_pem(&identity.cert_pem).expect("trust"),
            runtime,
            _sigpipe: sigpipe,
            _service: service,
            db,
            passwords,
            actors: Vec::new(),
            created: 0,
        };
        for kind in [
            AccountKind::Human,
            AccountKind::Human,
            AccountKind::Human,
            AccountKind::Bot,
        ] {
            let actor = harness.new_account(kind);
            harness.actors.push(actor);
        }
        harness.grant_admin(0);
        harness
    }

    /// A fresh account of `kind` with a session token, made the way the
    /// service would store them.
    fn new_account(&mut self, kind: AccountKind) -> Actor {
        self.created += 1;
        let login = format!("fuzz_{}", self.created);
        let hash = self
            .runtime
            .block_on(self.passwords.hash(PASSWORD.to_owned()))
            .expect("hash");
        let id = self
            .runtime
            .block_on(self.db.create_account(login, hash, kind, None))
            .expect("account");
        let mut actor = Actor {
            kind,
            id,
            token: SessionToken([0; 32]),
        };
        self.new_token(&mut actor);
        actor
    }

    fn new_token(&self, actor: &mut Actor) {
        let mut bytes = [0u8; 32];
        bytes[..8].copy_from_slice(&actor.id.0.to_le_bytes());
        bytes[8..12].copy_from_slice(&self.created.to_le_bytes());
        bytes[12..20].copy_from_slice(&std::process::id().to_le_bytes().repeat(2));
        bytes[20..28].copy_from_slice(
            &std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
                .to_le_bytes()[..8],
        );
        actor.token = SessionToken(bytes);
        self.runtime
            .block_on(self.db.create_session(actor.id, token_hash(&actor.token)))
            .expect("session");
    }

    fn grant_admin(&self, at: usize) {
        let id = self.actors[at].id;
        let row = self
            .runtime
            .block_on(self.db.account_by_id(id))
            .expect("read")
            .expect("account");
        self.runtime
            .block_on(self.db.grant_admin(row.login))
            .expect("grant");
    }

    /// A logged-in control connection of an actor, its account or token made
    /// anew when a request of an earlier input deleted or revoked them.
    fn connect(&mut self, at: usize) -> Option<Client> {
        for _ in 0..PROBE_TRIES {
            let mut client = Client::connect(&self.url, &self.tls, FINGERPRINT).ok()?;
            let info = ClientInfo {
                bot_version: (self.actors[at].kind == AccountKind::Bot).then(|| "fuzz".to_owned()),
            };
            match client.resume(self.actors[at].token.clone(), info) {
                Ok(_) => return Some(client),
                Err(ClientError::Refused(Error::BadSession)) => self.renew(at),
                Err(ClientError::Refused(Error::TooManySessions | Error::Banned { .. })) => {
                    std::thread::sleep(Duration::from_millis(20));
                    if self.banned(at) {
                        self.unban(at);
                    }
                }
                Err(_) => std::thread::sleep(Duration::from_millis(20)),
            }
        }
        None
    }

    fn banned(&self, at: usize) -> bool {
        let row = self
            .runtime
            .block_on(self.db.account_by_id(self.actors[at].id));
        matches!(row, Ok(Some(row)) if row.active_ban(radovid::db::now_ms()).is_some())
    }

    fn unban(&self, at: usize) {
        let _ = self
            .runtime
            .block_on(self.db.set_ban(self.actors[at].id, None));
    }

    /// A revoked token is made again; a deleted account is replaced.
    fn renew(&mut self, at: usize) {
        let id = self.actors[at].id;
        let row = self.runtime.block_on(self.db.account_by_id(id));
        match row {
            Ok(Some(row)) if !row.login.starts_with("deleted_") => {
                let mut actor = std::mem::replace(
                    &mut self.actors[at],
                    Actor {
                        kind: AccountKind::Human,
                        id,
                        token: SessionToken([0; 32]),
                    },
                );
                actor.kind = row.kind;
                self.new_token(&mut actor);
                self.actors[at] = actor;
            }
            _ => {
                let kind = self.actors[at].kind;
                self.actors[at] = self.new_account(kind);
                if at == 0 {
                    self.grant_admin(0);
                }
            }
        }
    }
}

/// Sends one piece; false when the connection is over.
fn send(client: &mut Client, raw: bool, body: &[u8]) -> bool {
    if raw {
        if client.send_raw(body.to_vec()).is_err() {
            return false;
        }
        return !matches!(client.read_raw(RAW_WAIT), Err(_));
    }
    let Ok(msg) = decode_payload::<ClientMsg>(body) else {
        return true;
    };
    match client.request(msg) {
        Ok(_) | Err(ClientError::Refused(_)) => true,
        Err(_) => false,
    }
}

fuzz_target!(|data: &[u8]| {
    static HARNESS: OnceLock<Mutex<Harness>> = OnceLock::new();
    let mut harness = HARNESS
        .get_or_init(|| Mutex::new(Harness::start()))
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let mut clients: Vec<Option<Client>> = (0..4).map(|_| None).collect();
    for piece in radovid_fuzz::frames(data).take(MAX_PIECES) {
        let Some((&head, body)) = piece.split_first() else {
            continue;
        };
        let at = usize::from(head & 3);
        if clients[at].is_none() {
            clients[at] = harness.connect(at);
        }
        let Some(client) = clients[at].as_mut() else {
            continue;
        };
        if !send(client, head & 4 != 0, body) {
            clients[at] = None;
        }
    }
    drop(clients);
    let admin = harness.connect(0);
    assert!(admin.is_some(), "the hub let nobody back in");
});
