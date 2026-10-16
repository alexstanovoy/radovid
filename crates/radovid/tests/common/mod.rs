//! A service on a loopback port with a scripted or a real game server, and
//! bots to talk to it.
#![allow(dead_code)]

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use bota_client_core::Transport;
use bota_proto::{HeroId, ServerMsg, Team};
use radovid::config::{BackendConfig, Config, RulesetConfig};
use radovid::password::PasswordCost;
use radovid::service::Service;
use radovid::tls;
use radovid_client::proto::*;
use radovid_client::{Client, ClientError, GameLink, TlsConfig, take_seat};

pub const PASSWORD: &str = "correct horse battery";

static NEXT: AtomicU32 = AtomicU32::new(0);

/// A running service, stopped when dropped.
pub struct Env {
    runtime: tokio::runtime::Runtime,
    service: Option<Service>,
    pub url: String,
    pub tls: TlsConfig,
    pub dir: PathBuf,
}

impl Env {
    pub fn db_path(&self) -> PathBuf {
        self.dir.join("radovid.db")
    }

    pub fn connect(&self) -> Client {
        Client::connect(&self.url, &self.tls).expect("connect")
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

    pub fn bot(&self, login: &str) -> Account {
        self.account(login, AccountKind::Bot)
    }

    /// Makes an account an admin, the way an operator would on the host.
    pub fn grant_admin(&self, login: &str) {
        self.runtime.block_on(async {
            let db = radovid::db::Db::open(&self.db_path()).expect("open");
            assert!(db.grant_admin(login.to_owned()).await.expect("grant"));
        });
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

/// One ruleset of the catalog; `bota_args` script the fake game server.
pub fn ruleset(id: &str, queue: bool, bota_args: &[&str]) -> RulesetConfig {
    RulesetConfig {
        id: id.to_owned(),
        name: format!("Test {id}"),
        seats: 2,
        heroes: vec![0, 1],
        queue,
        bota_args: bota_args.iter().map(|a| (*a).to_owned()).collect(),
        start_after_secs: 30,
    }
}

/// The scripted game server built beside the tests.
pub fn fake_game_server() -> PathBuf {
    let exe = std::env::current_exe().expect("test executable");
    let target = exe
        .parent()
        .and_then(|deps| deps.parent())
        .expect("target dir");
    target.join("examples").join("fake_bota_server")
}

pub fn start(rulesets: Vec<RulesetConfig>) -> Env {
    start_with(
        BackendConfig::Local {
            bota_server: fake_game_server(),
        },
        rulesets,
    )
}

pub fn start_with(backend: BackendConfig, rulesets: Vec<RulesetConfig>) -> Env {
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
    let identity = tls::self_signed(vec!["127.0.0.1".to_owned(), "localhost".to_owned()])
        .expect("certificate");
    let config = Config {
        listen: "127.0.0.1:0".parse().expect("address"),
        database: dir.join("radovid.db"),
        tls_cert: PathBuf::new(),
        tls_key: PathBuf::new(),
        public_game_url: "wss://127.0.0.1:{port}/game".to_owned(),
        max_matches: 8,
        server_name: "radovid-test".to_owned(),
        game_messages_per_sec: 1_000_000,
        password: PasswordCost {
            memory_kib: 1024,
            iterations: 1,
        },
        backend,
        rulesets,
    };
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()
        .expect("runtime");
    let service = runtime
        .block_on(Service::start(config, &identity))
        .expect("service");
    Env {
        url: format!("wss://127.0.0.1:{}", service.addr.port()),
        tls: TlsConfig::trust_pem(&identity.cert_pem).expect("trust"),
        runtime,
        service: Some(service),
        dir,
    }
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
        let deadline = std::time::Instant::now() + Duration::from_secs(secs);
        loop {
            let left = deadline.saturating_duration_since(std::time::Instant::now());
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

/// Takes the seat and plays the (scripted) match to its end on a thread.
pub fn play(handoff: Handoff, tls: TlsConfig) -> std::thread::JoinHandle<Option<Team>> {
    std::thread::spawn(move || {
        let (mut link, _) = take_seat(&handoff, HeroId::SHADOW_FIEND, &tls).expect("seat");
        winner_of(&mut link)
    })
}

/// Reads the stream until the match is over.
pub fn winner_of(link: &mut GameLink) -> Option<Team> {
    loop {
        match link.recv().expect("readable") {
            Some(ServerMsg::MatchOver { winner, .. }) => return Some(winner),
            Some(_) => {}
            None => return None,
        }
    }
}

/// Two accounts queue for `ruleset` and are matched: their handoffs, the longer
/// waiting one first.
pub fn pair_up(first: &mut Account, second: &mut Account, ruleset: &str) -> (Handoff, Handoff) {
    let ruleset = RulesetId(ruleset.to_owned());
    assert_eq!(
        first.ask(ClientMsg::QueueJoin {
            ruleset: ruleset.clone()
        }),
        Ok(Reply::Queued { waiting: 1 })
    );
    assert_eq!(
        second.ask(ClientMsg::QueueJoin { ruleset }),
        Ok(Reply::Queued { waiting: 2 })
    );
    (first.wait_match_found(), second.wait_match_found())
}

/// Where an account is, as a fresh login tells.
pub fn presence_of(env: &Env, login: &str) -> Presence {
    let mut client = env.connect();
    client
        .login(login, PASSWORD, ClientInfo::default())
        .expect("login")
        .presence
}
