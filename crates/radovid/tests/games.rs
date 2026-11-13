//! Real launcher processes and game clients over sockets, with radovid_client
//! carrying accounts, queue proposals, handoffs, results and ladder queries.
mod common;

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::time::Duration;

use common::*;
use radovid::config::{ArtifactConfig, RulesetConfig};
use radovid_client::Client;
use radovid_client::proto::*;

fn fingerprint(game: &str) -> u64 {
    match game {
        "ttt" => 0x7474740000000001,
        "clock" => 0x636c6f636b000001,
        "grid" => 0x6772696400000001,
        _ => panic!("fixture game"),
    }
}

fn fixture(game: &str, teams: &[u8], clock_ms: u32) -> RulesetConfig {
    let script = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/game_server.py");
    let mut rules = ruleset(game, true, "", "");
    rules.game = game.into();
    rules.game_fingerprint = fingerprint(game);
    rules.teams = teams.to_vec();
    rules.info = format!("{game}:clock={clock_ms}").into_bytes();
    rules.launcher.binary = Some("python3".into());
    rules.launcher.public_url = "tcp://127.0.0.1:{port}".into();
    rules.launcher.args = vec![
        script.display().to_string(),
        "--game".into(),
        game.into(),
        "--port".into(),
        "{port}".into(),
        "--clock-ms".into(),
        clock_ms.to_string(),
    ];
    rules.launcher.version_probe = vec![
        script.display().to_string(),
        "--game".into(),
        game.into(),
        "--probe".into(),
    ];
    rules
}

fn service(rules: RulesetConfig) -> FixtureEnv {
    let teams = rules.teams.clone();
    service_with_lobby(rules, teams)
}

struct FixtureEnv {
    env: Env,
    _port: PortLease,
}

impl std::ops::Deref for FixtureEnv {
    type Target = Env;
    fn deref(&self) -> &Env {
        &self.env
    }
}

static PORTS: std::sync::Mutex<Vec<u16>> = std::sync::Mutex::new(Vec::new());

struct PortLease(u16);

impl Drop for PortLease {
    fn drop(&mut self) {
        PORTS
            .lock()
            .expect("port ownership")
            .retain(|&port| port != self.0);
    }
}

fn reserve_port() -> (TcpListener, PortLease) {
    for _ in 0..128 {
        let listener = TcpListener::bind("127.0.0.1:0").expect("reserve port");
        let port = listener.local_addr().expect("address").port();
        let mut ports = PORTS.lock().expect("port ownership");
        if !ports.contains(&port) {
            ports.push(port);
            return (listener, PortLease(port));
        }
    }
    panic!("fixture port reservation budget")
}

fn service_with_lobby(rules: RulesetConfig, teams: Vec<u8>) -> FixtureEnv {
    service_catalog(vec![rules], teams)
}

fn service_catalog(rules: Vec<RulesetConfig>, teams: Vec<u8>) -> FixtureEnv {
    // The launcher needs the socket released, but parallel fixtures retain
    // ownership of their selected port until the service has stopped.
    let (reservation, lease) = reserve_port();
    let port = lease.0;
    let env = start_configured(rules, move |config| {
        config.lobby_teams = teams;
        config.game_ports = radovid::config::PortRange {
            first: port,
            last: port,
        };
        config.limits.game_capacity = 1;
        config.limits.lobby_games_max = 1;
        let path = config
            .database
            .parent()
            .expect("test directory")
            .join("record-{match_id}.txt");
        let record: ArtifactConfig = toml::from_str(&format!(
            "path = {:?}\nmax_kib = 4\nenv = {{ GAME_RECORD = \"{{artifact}}\" }}",
            path.display().to_string()
        ))
        .expect("artifact config");
        for rules in &mut config.rulesets {
            let mut record = record.clone();
            record.path = record.path.replace("record-", &format!("{}-", rules.id));
            rules.artifacts.insert("record".into(), record);
        }
        drop(reservation);
    });
    FixtureEnv { env, _port: lease }
}

fn account(env: &Env, game: &str, name: &str) -> Account {
    let mut client = Client::connect(&env.url, &env.tls, fingerprint(game)).expect("control");
    let logged = client
        .register(name, PASSWORD, AccountKind::Human, ClientInfo::default())
        .expect("account");
    Account {
        client,
        me: logged.me,
        token: logged.session,
    }
}

struct GameClient(BufReader<TcpStream>);

impl GameClient {
    fn connect(handoff: &Handoff) -> Self {
        let mut client = Self::open(&handoff.game_url);
        client.send(&handoff.ticket.to_hex());
        assert_eq!(
            client.line(),
            format!("SEAT {} {}", handoff.slot, handoff.team)
        );
        client
    }

    fn open(url: &str) -> Self {
        let stream = TcpStream::connect(url.strip_prefix("tcp://").expect("game scheme"))
            .expect("game connection");
        stream
            .set_read_timeout(Some(Duration::from_secs(60)))
            .expect("read guard");
        stream
            .set_write_timeout(Some(Duration::from_secs(60)))
            .expect("write guard");
        Self(BufReader::new(stream))
    }

    fn send(&mut self, line: &str) {
        writeln!(self.0.get_mut(), "{line}").expect("game input");
    }

    fn line(&mut self) -> String {
        let mut line = String::new();
        let count = self
            .0
            .by_ref()
            .take(513)
            .read_line(&mut line)
            .expect("game message");
        assert!((1..=512).contains(&count), "bounded game message");
        line.trim_end().to_owned()
    }
}

fn launch(accounts: &mut [Account], game: &str) -> Vec<Handoff> {
    for account in accounts.iter_mut() {
        account.queue(&RulesetId(game.into()));
    }
    for account in accounts.iter_mut() {
        account.accept();
    }
    accounts.iter_mut().map(Account::wait_match_found).collect()
}

/// The service meters lookups per account in real time; wait out the advertised delay.
fn ask_lookup(account: &mut Account, message: ClientMsg) -> Reply {
    for _ in 0..8 {
        match account.ask(message.clone()) {
            Err(Error::Throttled { retry_after_ms }) => {
                std::thread::sleep(Duration::from_millis(retry_after_ms.into()))
            }
            other => return other.expect("lookup"),
        }
    }
    panic!("lookup stayed throttled");
}

fn finished(accounts: &mut [Account], expected: Outcome, game: &str) -> MatchResult {
    let result = accounts[0].wait_result();
    assert!(result.rated);
    assert_eq!(result.outcome, expected);
    for account in &mut accounts[1..] {
        assert_eq!(account.wait_result(), result);
    }
    for seat in &result.seats {
        let before = seat.before.expect("before");
        let after = seat.after.expect("after");
        match expected {
            Outcome::Winner { team } if seat.team == team => assert!(after.rating > before.rating),
            Outcome::Winner { .. } => assert!(after.rating < before.rating),
            Outcome::Draw => assert!(after.deviation < before.deviation),
            Outcome::Null => panic!("not a played match"),
        }
    }
    let Reply::LadderPage { rows, total } = accounts[0]
        .ask(ClientMsg::Ladder {
            ruleset: RulesetId(game.into()),
            offset: 0,
            limit: 10,
        })
        .expect("ladder")
    else {
        panic!("ladder reply")
    };
    assert_eq!(total as usize, accounts.len());
    for seat in &result.seats {
        let row = rows
            .iter()
            .find(|r| r.account == seat.account)
            .expect("one position per account");
        assert_eq!(Some(row.rating), seat.after);
        let owner = accounts
            .iter()
            .position(|account| account.me.id == seat.account)
            .expect("participant");
        let Reply::Matches { list } = ask_lookup(
            &mut accounts[owner],
            ClientMsg::RecentMatches {
                account: seat.account,
                limit: 1,
            },
        ) else {
            panic!("own matches")
        };
        assert_eq!(
            list.as_slice(),
            std::slice::from_ref(&result),
            "each participant retains the complete report on their own list"
        );
        let viewer = (owner + 1) % accounts.len();
        assert_ne!(accounts[viewer].me.id, seat.account);
        let Reply::Matches { list } = ask_lookup(
            &mut accounts[viewer],
            ClientMsg::RecentMatches {
                account: seat.account,
                limit: 1,
            },
        ) else {
            panic!("public matches")
        };
        let mut public = result.clone();
        public.game_report.clear();
        assert_eq!(
            list.as_slice(),
            std::slice::from_ref(&public),
            "public history retains all public result facts and omits the private report"
        );
    }
    let Reply::ArtifactNamed {
        data: bytes, total, ..
    } = accounts[0]
        .ask(ClientMsg::MatchArtifactNamed {
            match_id: result.match_id,
            name: "record".into(),
            offset: 0,
        })
        .expect("game record")
    else {
        panic!("record reply")
    };
    assert_eq!(total as usize, bytes.len());
    assert_eq!(bytes, result.game_report);
    let report = String::from_utf8(bytes).expect("report");
    assert!(report.contains(&format!("game={game};")));
    assert!(
        !report.contains(&format!("pid={};", std::process::id())),
        "separate server process"
    );
    result
}

#[test]
fn tic_tac_toe_clients_play_a_rated_draw_and_win() {
    let env = service(fixture("ttt", &[1, 1], 200));
    let mut accounts = [account(&env, "ttt", "alice"), account(&env, "ttt", "bob")];
    for (moves, outcome) in [
        (vec![0, 1, 2, 4, 3, 5, 7, 6, 8], Outcome::Draw),
        (vec![0, 3, 1, 4, 2], Outcome::Winner { team: 0 }),
    ] {
        let handoffs = launch(&mut accounts, "ttt");
        let mut clients: Vec<_> = handoffs.iter().map(GameClient::connect).collect();
        let mut intruder = GameClient::open(&handoffs[0].game_url);
        intruder.send(&"00".repeat(32));
        assert_eq!(intruder.line(), "REFUSED");
        // Another simultaneous connection cannot steal an occupied seat.
        let mut duplicate = GameClient::open(&handoffs[0].game_url);
        duplicate.send(&handoffs[0].ticket.to_hex());
        assert_eq!(duplicate.line(), "REFUSED");
        clients[1].send("MOVE 0");
        assert_eq!(clients[1].line(), "INVALID");
        for (turn, cell) in moves.iter().enumerate() {
            clients[turn % 2].send(&format!("MOVE {cell}"));
            assert_eq!(clients[turn % 2].line(), "OK");
        }
        for client in &mut clients {
            assert!(client.line().starts_with("END "));
        }
        let result = finished(&mut accounts, outcome, "ttt");
        assert!(
            String::from_utf8_lossy(&result.game_report)
                .contains(&format!("moves={};", moves.len()))
        );
    }
}

#[test]
fn game_clock_parameters_pass_untouched_and_timeout_is_a_rated_loss() {
    for clock_ms in [30, 90] {
        let env = service(fixture("clock", &[1, 1], clock_ms));
        let mut accounts = [
            account(&env, "clock", "alice"),
            account(&env, "clock", "bob"),
        ];
        let Reply::Rulesets { list: rulesets } =
            accounts[0].ask(ClientMsg::Rulesets).expect("catalog")
        else {
            panic!("catalog reply")
        };
        assert_eq!(
            rulesets[0].info,
            format!("clock:clock={clock_ms}").as_bytes()
        );
        let handoffs = launch(&mut accounts, "clock");
        let mut clients: Vec<_> = handoffs.iter().map(GameClient::connect).collect();
        for client in &mut clients {
            assert_eq!(client.line(), "END 1");
        }
        let result = finished(&mut accounts, Outcome::Winner { team: 1 }, "clock");
        assert!(
            String::from_utf8_lossy(&result.game_report)
                .contains(&format!("clock_ms={clock_ms};moves=0;"))
        );
    }
}

#[test]
fn fixed_tick_clients_play_rated_teams_and_ffa_and_reconnect() {
    for teams in [vec![2, 2], vec![1, 1, 1], vec![1, 2, 1]] {
        let env = service(fixture("grid", &teams, 200));
        let count: usize = teams.iter().map(|&size| usize::from(size)).sum();
        let mut accounts: Vec<_> = (0..count)
            .map(|i| account(&env, "grid", &format!("player{i}")))
            .collect();
        let handoffs = launch(&mut accounts, "grid");
        play_grid(&handoffs, Some(1));
        let result = finished(&mut accounts, Outcome::Winner { team: 1 }, "grid");
        let report = String::from_utf8_lossy(&result.game_report);
        assert!(report.contains("reconnects=1;"));
        assert!(!report.contains("ticks=0;"));
        let distances = handoffs
            .iter()
            .map(|h| if h.team == 1 { "16" } else { "8" })
            .collect::<Vec<_>>()
            .join(",");
        assert!(report.contains(&format!("distances={distances};")));
        assert!(report.contains(&format!("moves={};", count * 8)));
    }
}

fn play_grid(handoffs: &[Handoff], winner: Option<u8>) {
    let clients: Vec<_> = handoffs.iter().map(GameClient::connect).collect();
    std::thread::scope(|scope| {
        let threads: Vec<_> = clients
            .into_iter()
            .zip(handoffs)
            .map(|(mut client, handoff)| {
                scope.spawn(move || {
                    let mut reconnected = false;
                    let mut sent = 0;
                    for _ in 0..20000 {
                        let message = client.line();
                        if message.starts_with("END ") {
                            return;
                        }
                        assert!(message.starts_with("TICK "));
                        if handoff.slot == 0 && !reconnected {
                            client.send("DROP");
                            // Tick messages may already be buffered ahead of BYE.
                            let mut goodbye = false;
                            for _ in 0..20000 {
                                if client.line() == "BYE" {
                                    goodbye = true;
                                    break;
                                }
                            }
                            assert!(goodbye, "disconnect barrier");
                            client = GameClient::connect(handoff);
                            reconnected = true;
                        }
                        if sent < 8 {
                            client.send(if Some(handoff.team) == winner {
                                "INPUT 2"
                            } else {
                                "INPUT 1"
                            });
                            sent += 1;
                        }
                    }
                    panic!("game never ended")
                })
            })
            .collect();
        for thread in threads {
            thread.join().expect("game client");
        }
    });
}

#[test]
fn rated_team_and_ffa_draws_update_each_accounts_single_ladder_position() {
    for teams in [vec![2, 2], vec![1, 1, 1]] {
        let env = service(fixture("grid", &teams, 200));
        let count: usize = teams.iter().map(|&n| usize::from(n)).sum();
        let mut accounts: Vec<_> = (0..count)
            .map(|i| account(&env, "grid", &format!("player{i}")))
            .collect();
        let handoffs = launch(&mut accounts, "grid");
        play_grid(&handoffs, None);
        let result = finished(&mut accounts, Outcome::Draw, "grid");
        for seat in result.seats {
            assert_eq!(seat.after.expect("rated draw").rating, 1500.0);
        }
    }
}

#[test]
fn unrelated_games_coexist_on_one_service_without_protocol_dependencies() {
    let env = service_catalog(
        vec![
            fixture("ttt", &[1, 1], 200),
            fixture("clock", &[1, 1], 40),
            fixture("grid", &[1, 1, 1], 200),
        ],
        vec![1, 1, 1],
    );
    for game in ["ttt", "clock", "grid"] {
        let count = if game == "grid" { 3 } else { 2 };
        let mut accounts: Vec<_> = (0..count)
            .map(|i| account(&env, game, &format!("{game}{i}")))
            .collect();
        let Reply::Rulesets { list } = accounts[0].ask(ClientMsg::Rulesets).expect("catalog")
        else {
            panic!("catalog")
        };
        assert_eq!(list.len(), 3);
        let other = if game == "ttt" { "grid" } else { "ttt" };
        assert_eq!(
            accounts[0].ask(ClientMsg::QueueJoin {
                ruleset: RulesetId(other.into())
            }),
            Err(Error::GameVersionMismatch {
                server: fingerprint(other)
            })
        );
        let handoffs = launch(&mut accounts, game);
        let outcome = match game {
            "grid" => {
                play_grid(&handoffs, Some(1));
                Outcome::Winner { team: 1 }
            }
            "clock" => {
                let mut clients: Vec<_> = handoffs.iter().map(GameClient::connect).collect();
                for client in &mut clients {
                    assert_eq!(client.line(), "END 1");
                }
                Outcome::Winner { team: 1 }
            }
            _ => {
                let mut clients: Vec<_> = handoffs.iter().map(GameClient::connect).collect();
                for (turn, cell) in [0, 3, 1, 4, 2].iter().enumerate() {
                    clients[turn % 2].send(&format!("MOVE {cell}"));
                    assert_eq!(clients[turn % 2].line(), "OK");
                }
                for client in &mut clients {
                    assert_eq!(client.line(), "END 0");
                }
                Outcome::Winner { team: 0 }
            }
        };
        finished(&mut accounts, outcome, game);
    }
}

#[test]
fn a_lobby_result_uses_its_actual_sides_instead_of_the_queue_layout() {
    let mut rules = fixture("grid", &[1, 1], 200);
    rules.lobby_min_players = Some(3);
    let env = service_with_lobby(rules, vec![1, 1, 1]);
    let mut accounts: Vec<_> = (0..3)
        .map(|i| account(&env, "grid", &format!("player{i}")))
        .collect();
    let Reply::Lobby { state } = accounts[0]
        .ask(ClientMsg::LobbyCreate {
            ruleset: RulesetId("grid".into()),
            name: "Three sides".into(),
            audience: Audience::Everyone,
            password: None,
        })
        .expect("create")
    else {
        panic!("lobby")
    };
    for account in &mut accounts[1..] {
        assert!(matches!(
            account.ask(ClientMsg::LobbyJoin {
                lobby: state.id,
                password: None
            }),
            Ok(Reply::Lobby { .. })
        ));
        account.expect_done(ClientMsg::LobbyReady {
            lobby: state.id,
            ready: true,
        });
    }
    accounts[0].expect_done(ClientMsg::LobbyReady {
        lobby: state.id,
        ready: true,
    });
    let handoffs: Vec<_> = accounts.iter_mut().map(Account::wait_match_found).collect();
    assert_eq!(
        handoffs.iter().map(|h| h.team).collect::<Vec<_>>(),
        [0, 1, 2]
    );
    play_grid(&handoffs, Some(2));
    let result = accounts[0].wait_result();
    assert_eq!(result.outcome, Outcome::Winner { team: 2 });
    assert!(!result.rated, "custom lobbies remain unrated");
    assert!(
        result
            .seats
            .iter()
            .all(|s| s.before.is_none() && s.after.is_none())
    );
    for account in &mut accounts[1..] {
        assert_eq!(account.wait_result(), result);
    }
}

#[test]
fn fixture_interpreter_is_resolved_through_path() {
    let rules = fixture("ttt", &[1, 1], 200);
    assert_eq!(
        rules.launcher.binary.as_deref(),
        Some(std::path::Path::new("python3"))
    );
}
