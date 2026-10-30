//! A service on a loopback port whose game servers are `fake_game.sh`, and
//! accounts to talk to it.
#![allow(dead_code)]

use rustls::pki_types::pem::PemObject;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use radovid::config::{
    Config, LauncherConfig, LauncherKind, LimitsConfig, PerKind, PortRange, RulesetConfig,
};
use radovid::password::PasswordCost;
use radovid::service::Service;
use radovid::tls;
use radovid_client::proto::*;
use radovid_client::{Client, ClientError, Incoming, TlsConfig};

pub const PASSWORD: &str = "correct horse battery";
/// The game wire the test rulesets and clients name.
pub const FINGERPRINT: u64 = 0x1e88_417a_0b3c_5d6f;

static NEXT: AtomicU32 = AtomicU32::new(0);

/// A running service, stopped when dropped.
pub struct Env {
    runtime: tokio::runtime::Runtime,
    service: Option<Service>,
    pub url: String,
    pub tls: TlsConfig,
    pub dir: PathBuf,
    pub cert_pem: String,
}

impl Env {
    pub fn db_path(&self) -> PathBuf {
        self.dir.join("radovid.db")
    }

    /// One raw HTTPS request on the service's port (`request` is the whole head and what follows),
    /// and everything the service sent back until it closed.
    pub fn https(&self, request: &[u8]) -> Vec<u8> {
        use std::io::{BufReader, Read, Write};
        use std::sync::Arc;
        let addr = self.url.trim_start_matches("wss://");
        let mut roots = rustls::RootCertStore::empty();
        for cert in rustls::pki_types::CertificateDer::pem_reader_iter(&mut BufReader::new(
            self.cert_pem.as_bytes(),
        )) {
            roots.add(cert.expect("a certificate")).expect("a root");
        }
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let config = rustls::ClientConfig::builder_with_provider(provider)
            .with_protocol_versions(&[&rustls::version::TLS13])
            .expect("versions")
            .with_root_certificates(roots)
            .with_no_client_auth();
        let name = rustls::pki_types::ServerName::try_from("localhost").expect("a name");
        let connection = rustls::ClientConnection::new(Arc::new(config), name).expect("tls");
        let tcp = std::net::TcpStream::connect(addr).expect("connect");
        tcp.set_read_timeout(Some(Duration::from_secs(10)))
            .expect("timeout");
        let mut stream = rustls::StreamOwned::new(connection, tcp);
        let _ = stream.write_all(request);
        let mut out = Vec::new();
        let _ = stream.read_to_end(&mut out);
        out
    }

    pub fn connect(&self) -> Client {
        Client::connect(&self.url, &self.tls, FINGERPRINT).expect("connect")
    }

    /// A connection from another source than `connect`'s: over IPv6 loopback,
    /// to a service listening on both families (`listen_on_both_families`).
    pub fn connect_from_second_source(&self) -> Client {
        let url = self.url.replace("127.0.0.1", "[::1]");
        Client::connect(&url, &self.tls, FINGERPRINT).expect("connect from the second source")
    }

    /// A registered, logged-in account.
    pub fn account(&self, login: &str, kind: AccountKind) -> Account {
        let mut client = self.connect();
        let info = ClientInfo {
            bot_version: (kind == AccountKind::Bot).then(|| "test-1".to_owned()),
        };
        let logged = client
            .register(login, PASSWORD, kind, info)
            .expect("register");
        Account {
            client,
            me: logged.me,
            token: logged.session,
        }
    }

    pub fn human(&self, login: &str) -> Account {
        self.account(login, AccountKind::Human)
    }

    pub fn bot(&self, login: &str) -> Account {
        self.account(login, AccountKind::Bot)
    }

    /// Another session of an existing account.
    pub fn login(&self, login: &str) -> Account {
        let mut client = self.connect();
        let logged = client
            .login(login, PASSWORD, ClientInfo::default())
            .expect("login");
        Account {
            client,
            me: logged.me,
            token: logged.session,
        }
    }

    /// Makes an account an admin, the way an operator would on the host.
    pub fn grant_admin(&self, login: &str) {
        self.runtime.block_on(async {
            let db = radovid::db::Db::open(&self.db_path()).expect("open");
            assert!(db.grant_admin(login.to_owned()).await.expect("grant"));
        });
    }

    pub fn reload_tls(&self, identity: &tls::Identity) {
        self.service
            .as_ref()
            .expect("running")
            .reload_tls(identity)
            .expect("reload");
    }

    pub fn stop(mut self) {
        if let Some(service) = self.service.take() {
            self.runtime.block_on(service.shutdown());
        }
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        if let Some(service) = self.service.take() {
            self.runtime.block_on(service.shutdown());
        }
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

pub fn fake_game() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fake_game.sh")
}

/// A ruleset of two teams of one whose game does `behaviour` (see
/// fake_game.sh) after `seconds`.
pub fn ruleset(id: &str, rated: bool, behaviour: &str, seconds: &str) -> RulesetConfig {
    RulesetConfig {
        id: id.to_owned(),
        name: format!("Test {id}"),
        game: "testgame".to_owned(),
        game_fingerprint: FINGERPRINT,
        teams: vec![1, 1],
        rated,
        info: vec![1, 2, 3],
        max_duration_secs: 600,
        games_per_account: None,
        audience: Default::default(),
        lobby_min_players: None,
        artifact_access: Default::default(),
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
            version_probe: vec!["--probe".to_owned(), format!("{FINGERPRINT:016x}")],
            args: [behaviour, seconds, "{match_id}", "{port}"]
                .map(str::to_owned)
                .to_vec(),
            env: [("GAME_SEATS".to_owned(), "{seats}".to_owned())].into(),
            start_timeout_secs: 30,
            artifact: None,
            artifact_args: Vec::new(),
            artifact_env: Default::default(),
        },
    }
}

/// The default test limits: the per-source one out of the way, since every
/// test client comes from 127.0.0.1.
pub fn test_limits() -> LimitsConfig {
    LimitsConfig {
        games_per_account: PerKind { human: 2, bot: 8 },
        games_per_source: 200,
        accept_timeout_secs: 30,
        decline_cooldown_secs: 120,
        game_capacity: 8,
        lobby_games_max: 6,
        queue_capacity: 256,
        artifact_retention_days: 14,
        artifact_cap_mib: 2048,
        artifact_max_mib: 64,
    }
}

/// Makes the service listen on both IP families, so that a second source exists.
pub fn listen_on_both_families(config: &mut Config) {
    config.listen = "[::]:0".parse().expect("address");
}

pub fn start(rulesets: Vec<RulesetConfig>) -> Env {
    try_start(rulesets, |_| {}).expect("service")
}

pub fn start_configured(rulesets: Vec<RulesetConfig>, adjust: impl FnOnce(&mut Config)) -> Env {
    try_start(rulesets, adjust).expect("service")
}

/// A fresh directory for one service.
pub fn test_dir() -> PathBuf {
    let base = if std::path::Path::new("/dev/shm").is_dir() {
        PathBuf::from("/dev/shm")
    } else {
        std::env::temp_dir()
    };
    let dir = base.join(format!(
        "radovid-test-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).expect("test dir");
    dir
}

/// A service whose configuration `adjust` changes first.
pub fn try_start(
    rulesets: Vec<RulesetConfig>,
    adjust: impl FnOnce(&mut Config),
) -> Result<Env, String> {
    let dir = test_dir();
    let identity = self_signed();
    let mut config = Config {
        web: None,
        listen: "127.0.0.1:0".parse().expect("address"),
        database: dir.join("radovid.db"),
        tls_cert: PathBuf::new(),
        tls_key: PathBuf::new(),
        game_ports: PortRange {
            first: 40000,
            last: 40063,
        },
        max_connections: 1024,
        server_name: "radovid-test".to_owned(),
        password: PasswordCost {
            memory_kib: 1024,
            iterations: 1,
            ..PasswordCost::default()
        },
        limits: test_limits(),
        artifacts: Default::default(),
        rulesets,
    };
    adjust(&mut config);
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()
        .expect("runtime");
    let service = match runtime.block_on(Service::start(config, &identity)) {
        Ok(service) => service,
        Err(error) => {
            let _ = std::fs::remove_dir_all(&dir);
            return Err(error);
        }
    };
    Ok(Env {
        url: format!("wss://127.0.0.1:{}", service.addr.port()),
        tls: TlsConfig::trust_pem(&identity.cert_pem).expect("trust"),
        cert_pem: identity.cert_pem.clone(),
        runtime,
        service: Some(service),
        dir,
    })
}

pub fn self_signed() -> tls::Identity {
    tls::self_signed(vec![
        "127.0.0.1".to_owned(),
        "::1".to_owned(),
        "localhost".to_owned(),
    ])
    .expect("certificate")
}

pub fn solo() -> RulesetId {
    RulesetId("solo".to_owned())
}

/// A logged-in account and its control connection.
pub struct Account {
    pub client: Client,
    pub me: AccountInfo,
    pub token: SessionToken,
}

impl Account {
    pub fn ask(&mut self, msg: ClientMsg) -> Result<Reply, Error> {
        match self.client.request(msg) {
            Ok(reply) => Ok(reply),
            Err(ClientError::Refused(error)) => Err(error),
            Err(other) => panic!("request failed: {other}"),
        }
    }

    pub fn expect_done(&mut self, msg: ClientMsg) {
        assert_eq!(self.ask(msg).expect("accepted"), Reply::Done);
    }

    /// The next event of the kind `pick` selects, within `secs` seconds.
    pub fn wait_for<T>(&mut self, secs: u64, mut pick: impl FnMut(Event) -> Option<T>) -> T {
        let deadline = Instant::now() + Duration::from_secs(secs);
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.client.next_event(left).expect("connection") {
                Some(event) => {
                    if let Some(found) = pick(event) {
                        return found;
                    }
                }
                None => panic!("the awaited event never came"),
            }
        }
    }

    /// No event `pick` selects arrives within `millis`.
    pub fn quiet(&mut self, millis: u64, mut pick: impl FnMut(&Event) -> bool) {
        let deadline = Instant::now() + Duration::from_millis(millis);
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.client.next_event(left).expect("connection") {
                Some(event) => assert!(!pick(&event), "unexpected {event:?}"),
                None => return,
            }
        }
    }

    pub fn queue(&mut self, ruleset: &RulesetId) -> QueueEntry {
        match self.ask(ClientMsg::QueueJoin {
            ruleset: ruleset.clone(),
        }) {
            Ok(Reply::Queued { entry }) => entry,
            other => panic!("queue: {other:?}"),
        }
    }

    pub fn wait_proposal(&mut self) -> Proposal {
        self.wait_for(10, |event| match event {
            Event::MatchProposed { proposal } => Some(proposal),
            _ => None,
        })
    }

    /// Accepts the next proposal.
    pub fn accept(&mut self) -> Proposal {
        let proposal = self.wait_proposal();
        self.expect_done(ClientMsg::MatchAccept {
            proposal: proposal.proposal,
        });
        proposal
    }

    pub fn wait_match_found(&mut self) -> Handoff {
        self.wait_for(30, |event| match event {
            Event::MatchFound { handoff } => Some(handoff),
            _ => None,
        })
    }

    pub fn wait_result(&mut self) -> MatchResult {
        self.wait_for(60, |event| match event {
            Event::MatchResult { result } => Some(result),
            _ => None,
        })
    }
}

/// Two accounts queue for `ruleset`, accept, and get their handoffs, the
/// longer waiting one first.
pub fn pair_up(
    first: &mut Account,
    second: &mut Account,
    ruleset: &RulesetId,
) -> (Handoff, Handoff) {
    first.queue(ruleset);
    second.queue(ruleset);
    first.accept();
    second.accept();
    (first.wait_match_found(), second.wait_match_found())
}

/// One seat as the fake game was started with it: ticket, team, name, kind.
pub type StartedSeat = (String, String, String, String);

/// What the fake game was started with, from its report:
/// `(match_id, port, seats)` with each seat `(ticket_hex, team, name, kind)`.
pub fn started_with(report: &[u8]) -> (String, String, Vec<StartedSeat>) {
    let text = String::from_utf8(report.to_vec()).expect("utf-8 report");
    let mut parts = text.splitn(3, '|');
    let (match_id, port, seats) = (
        parts.next().unwrap_or_default().to_owned(),
        parts.next().unwrap_or_default().to_owned(),
        parts.next().unwrap_or_default(),
    );
    let seats = seats
        .split(',')
        .map(|seat| {
            let mut fields = seat.split(':').map(str::to_owned);
            (
                fields.next().unwrap_or_default(),
                fields.next().unwrap_or_default(),
                fields.next().unwrap_or_default(),
                fields.next().unwrap_or_default(),
            )
        })
        .collect();
    (match_id, port, seats)
}

/// Reads one raw message, for tests of hostile input.
pub fn read_raw(client: &mut Client, secs: u64) -> Result<Option<Incoming>, ClientError> {
    client.read_raw(Duration::from_secs(secs))
}
