//! One task per launched match: starts the game server, reads its stdout
//! (DEVELOPMENT.md "Launcher contract") and reports to the hub what became of it.

use std::time::Duration;

use radovid_proto::MatchId;
use tokio::sync::{mpsc, oneshot};
use tokio::time::{Instant, sleep_until, timeout_at};

use crate::config::RulesetConfig;
use crate::hub::Cmd;
use crate::launcher::{self, Aborted, GameProcess, GameResult, LaunchSpec, Line};

/// How long a supervisor waits on a game server at each step; one value for production
/// ([`Patience::DEFAULT`]), small ones for tests.
#[derive(Clone, Copy, Debug)]
pub struct Patience {
    /// How long a game server has to exit after its result, or after its stdout closed, before it
    /// is killed. The result is told, and the artifact served, after the exit. A match that is
    /// asked to finish has this long, counted once from the request, to print its `RESULT` and
    /// exit.
    pub exit: Duration,
    /// How long a killed game server has to be gone. Past it the supervisor lets go of the match
    /// (the child is killed once more when dropped) and says so: a process that survives
    /// `SIGKILL` is stuck in the kernel, and nothing waits on it forever.
    pub kill: Duration,
    /// How long one second of a ruleset's `max_duration_secs` and `start_timeout_secs` is: a second, and a few
    /// milliseconds in the tests, which then do not wait for a limit. (The game is told the
    /// seconds of the ruleset all the same.)
    pub second: Duration,
}

impl Default for Patience {
    fn default() -> Self {
        Self::DEFAULT
    }
}

impl Patience {
    pub const DEFAULT: Self = Self {
        exit: Duration::from_secs(10),
        kill: Duration::from_secs(2),
        second: Duration::from_secs(1),
    };

    /// A span a ruleset gives in seconds (its time limit, its start timeout), in these seconds.
    fn limit(self, max_duration_secs: u64) -> Duration {
        self.second
            .saturating_mul(u32::try_from(max_duration_secs).unwrap_or(u32::MAX))
    }

    /// How long after the limit a game has to end the match at it by itself before the service
    /// asks: a twentieth of the limit, between half a second and five. The game counts the limit
    /// from before it says `READY`, so its own end comes first; the margin is the time it takes
    /// to notice. Should the service ask first, the match still ends as the limit's, but the
    /// players would be told it was stopped.
    fn limit_margin(self, max_duration: Duration) -> Duration {
        (max_duration / 20).clamp(self.second / 2, self.second * 5)
    }
}

/// The most a supervisor takes from a request to finish to the moment it lets go of the match,
/// whatever the game does, exactly: the answer and the exit until `exit` after the request, the
/// kill (a command that is itself bounded within it) and the wait for the process to be gone
/// until `kill` more. The hub's shutdown waits for this and a little more.
pub const STOP_TOTAL: Duration = Patience::DEFAULT
    .exit
    .saturating_add(Patience::DEFAULT.kill);
/// The exit code of a game nobody played: nobody came, or everybody left.
const EXIT_ABANDONED: i32 = 3;

/// Why the service ended a match that was not played out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ending {
    /// An admin, or the service's shutdown.
    Stopped,
    /// The time limit ran out.
    TimeLimit,
}

/// How a match ended, as the hub learns it.
#[derive(Debug)]
pub enum End {
    /// The game's `RESULT` line.
    Result(GameResult),
    /// Exit code 3 without a result.
    Abandoned,
    /// No start, no `READY` in time, a bad or unauthorised `RESULT`, or an exit without a result.
    Crashed(String),
    /// An admin or the service's shutdown, and the game did not answer the request to finish
    /// in time (or never got that far).
    Stopped,
    /// The time limit, and the game neither ended the match nor answered the request to.
    TimedOut,
    /// The game ended the match without playing it out (`RESULT ... aborted=<why>`), at the
    /// limit it was told or because the service asked it to finish, and the service had reason
    /// to hear it. It counts for nothing, and ends as the service's own reason says.
    Aborted(GameResult, Ending),
}

/// What the service has asked of one game and when it may hear what, so that an `aborted=` it
/// reads is believed only when the service has a reason to expect it. The game controls its own
/// stdout, and so its winner and exit code; what this keeps is that a game cannot end matches
/// with a word nothing called for.
#[derive(Clone, Copy, Debug)]
pub struct Ledger {
    /// The earliest the game's own limit can fall due: it counts from after the spawn.
    limit_due: Instant,
    asked: Option<Ending>,
}

impl Ledger {
    pub fn new(spawned: Instant, max_duration: Duration) -> Self {
        Self {
            limit_due: spawned + max_duration,
            asked: None,
        }
    }

    /// The service has asked the game to finish, for `ending`. The first ask stands.
    pub fn ask(&mut self, ending: Ending) {
        self.asked.get_or_insert(ending);
    }

    /// How the service ends a match whose game said `aborted=<why>` at `now`: `None` when
    /// nothing called for it. A game that says `time_limit` must have reached its limit; one that
    /// says `stopped` must have been asked. An ask for the limit is the limit's end whatever the
    /// game calls it, and the limit's own word is the limit's end even when nobody asked.
    pub fn believe(&self, why: Aborted, now: Instant) -> Option<Ending> {
        match (why, self.asked) {
            (Aborted::TimeLimit, _) if now >= self.limit_due => Some(Ending::TimeLimit),
            (Aborted::Stopped, Some(ending)) => Some(ending),
            _ => None,
        }
    }
}

pub struct Supervisor {
    pub patience: Patience,
    pub match_id: MatchId,
    pub ruleset: RulesetConfig,
    pub rated: bool,
    pub spec: LaunchSpec,
    pub hub: mpsc::Sender<Cmd>,
}

impl Supervisor {
    pub(crate) async fn hide_private_profile_bindings(&mut self, database: &crate::db::Db) {
        let accounts = self.spec.seats.iter().map(|seat| seat.account).collect();
        let result = tokio::time::timeout(
            Duration::from_secs(5),
            database.published_accounts(accounts),
        )
        .await;
        let bindings = match result {
            Ok(Ok(bindings)) => bindings,
            _ => {
                eprintln!("WARN: public profile visibility unavailable; omitting launch bindings");
                vec![None; self.spec.seats.len()]
            }
        };
        assert_eq!(
            bindings.len(),
            self.spec.seats.len(),
            "one visibility decision per seat"
        );
        for (seat, binding) in self.spec.seats.iter_mut().zip(bindings) {
            seat.account = binding.unwrap_or(radovid_proto::AccountId(0));
        }
    }

    pub async fn run(self, mut stop: oneshot::Receiver<()>) {
        let match_id = self.match_id;
        match launcher::spawn(&self.ruleset, &self.spec) {
            Err(error) => {
                if error
                    .get_ref()
                    .is_some_and(|e| e.is::<launcher::CheatsRefused>())
                {
                    // A configuration that would give a match cheats it may not have.
                    eprintln!("radovid: ERROR: match {}: {error}", match_id.0);
                }
                let end = End::Crashed(format!("start: {error}"));
                let _ = self.hub.send(Cmd::MatchEnded { match_id, end }).await;
            }
            Ok(mut game) => {
                let mut ledger = Ledger::new(
                    Instant::now(),
                    self.patience.limit(self.ruleset.max_duration_secs),
                );
                let mut exit_by = None;
                let end = self
                    .attend(&mut game, &mut stop, &mut ledger, &mut exit_by)
                    .await;
                let played_out = matches!(end, End::Result(_) | End::Aborted(..));
                // The hub hears of the end, and the seats may fetch the artifact, only once the
                // process is gone and so its files are closed. A stop has one deadline for all of
                // it, `Patience::exit` + `Patience::kill` from the request: the answer and the
                // exit until the first, the kill and the wait for it until the second. A game
                // that ended some other way is waited for if it played out, else killed now.
                let now = Instant::now();
                let exit_by = exit_by.unwrap_or(if played_out {
                    now + self.patience.exit
                } else {
                    now
                });
                if !gone_by(&mut game, exit_by, exit_by + self.patience.kill).await {
                    eprintln!(
                        "radovid: ERROR: match {}: the game server is still there after the kill; \
                         the match is let go",
                        match_id.0
                    );
                }
                let _ = self.hub.send(Cmd::MatchEnded { match_id, end }).await;
            }
        }
        // The port is free only once the game server is gone.
        let port = self.spec.port;
        let _ = self.hub.send(Cmd::PortFree { port }).await;
    }

    /// Reads the game until it ends the match. `exit_by` is set to when the process has to be
    /// gone if the end was asked for: the one deadline of the whole stop.
    async fn attend(
        &self,
        game: &mut GameProcess,
        stop: &mut oneshot::Receiver<()>,
        ledger: &mut Ledger,
        exit_by: &mut Option<Instant>,
    ) -> End {
        let start_by = Instant::now()
            + self
                .patience
                .limit(self.ruleset.launcher.start_timeout_secs);
        tokio::select! {
            line = game.stdout.next() => match line {
                Some(Line::Ready) => {}
                Some(line) => return self.end_of(line, ledger),
                None => return exit_end(game, self.patience.exit).await,
            },
            _ = &mut *stop => return End::Stopped,
            _ = sleep_until(start_by) => return End::Crashed("no READY in time".to_owned()),
        }
        let match_id = self.match_id;
        let _ = self.hub.send(Cmd::MatchReady { match_id }).await;
        // The game counts its limit from before it said READY, so its own end comes first.
        let max_duration = self.patience.limit(self.ruleset.max_duration_secs);
        let ask_at = Instant::now() + max_duration + self.patience.limit_margin(max_duration);
        loop {
            tokio::select! {
                line = game.stdout.next() => match line {
                    Some(Line::Ready) => {}
                    Some(line) => return self.end_of(line, ledger),
                    None => return exit_end(game, self.patience.exit).await,
                },
                _ = &mut *stop => {
                    return self.wind_down(game, Ending::Stopped, ledger, exit_by).await;
                }
                _ = sleep_until(ask_at) => {
                    return self.wind_down(game, Ending::TimeLimit, ledger, exit_by).await;
                }
            }
        }
    }

    /// The match is to end now (`ending`): the game is asked to finish it, so that its players are
    /// told, its artifacts are closed and its word is the match's end. All of it (the ask, the
    /// `RESULT`, the exit) is bounded by one deadline, [`Patience::exit`] from now. A game that
    /// answers with a `RESULT` in time, aborted or plain (if it was just done), ends the match as
    /// the service's reason says, unrated; any other is stopped.
    async fn wind_down(
        &self,
        game: &mut GameProcess,
        ending: Ending,
        ledger: &mut Ledger,
        exit_by: &mut Option<Instant>,
    ) -> End {
        ledger.ask(ending);
        let answer_by = Instant::now() + self.patience.exit;
        *exit_by = Some(answer_by);
        let answer = timeout_at(answer_by, async {
            game.request_stop().await;
            loop {
                match game.stdout.next().await {
                    Some(Line::Ready) => {}
                    other => return other,
                }
            }
        })
        .await;
        let unanswered = || match ending {
            Ending::Stopped => End::Stopped,
            Ending::TimeLimit => End::TimedOut,
        };
        match answer {
            // An aborted answer is judged as it stands: an unauthorised one is a crash.
            Ok(Some(Line::Result(result))) if result.aborted.is_some() => {
                self.end_of(Line::Result(result), ledger)
            }
            // An end by the limit or a stop is never rated: once the service has asked, a game
            // that answers with a winner (it had just finished; a rare race, accepted) has its
            // account kept, and the match ends as the service's reason says, for nobody.
            Ok(Some(Line::Result(result))) => End::Aborted(result, ending),
            // Any other garbled answer to the request is no answer.
            Ok(Some(line)) => match self.end_of(line, ledger) {
                End::Crashed(_) => unanswered(),
                end => end,
            },
            // The game is gone without a word (a lobby the game closed, as it does for a match
            // that never began, exits 3): the match ended as the service asked.
            Ok(None) | Err(_) => unanswered(),
        }
    }

    /// A lobby may add sides beyond the queue layout; an unoccupied configured
    /// side may still win (for instance, the computer opponent in solo practice).
    fn end_of(&self, line: Line, ledger: &Ledger) -> End {
        match line {
            // Nothing is rated, so the winner it names is not held to the seats.
            Line::Result(result) if result.aborted.is_some() => {
                let Some(why) = result.aborted else {
                    unreachable!("matched on it")
                };
                let Some(ending) = ledger.believe(why, Instant::now()) else {
                    return End::Crashed(format!(
                        "unauthorised aborted RESULT ({why:?}): nothing asked the game to stop \
                         and its limit was not due"
                    ));
                };
                if result.artifact_truncated {
                    eprintln!(
                        "radovid: match {} left a truncated artifact (the game closed it short)",
                        self.match_id.0
                    );
                }
                End::Aborted(result, ending)
            }
            Line::Result(result)
                if result.winner.is_none_or(|team| {
                    (!self.rated && usize::from(team) < self.ruleset.teams.len())
                        || self.spec.seats.iter().any(|seat| seat.team == team)
                }) && self.leaver_lost(&result) =>
            {
                if result.artifact_truncated {
                    eprintln!(
                        "radovid: match {} left a truncated artifact (the game closed it short)",
                        self.match_id.0
                    );
                }
                End::Result(result)
            }
            Line::Result(result) => End::Crashed(format!(
                "RESULT names an absent winner or invalid leaver (winner {:?}, {} seats)",
                result.winner,
                self.spec.seats.len()
            )),
            Line::Ready | Line::BadResult => End::Crashed("a malformed RESULT line".to_owned()),
        }
    }
}

impl Supervisor {
    /// A result that names a leaver names a seat of the ruleset, and a winner that is not the
    /// leaver's team.
    fn leaver_lost(&self, result: &launcher::GameResult) -> bool {
        let Some(slot) = result.left else {
            return true;
        };
        self.spec
            .seats
            .get(usize::from(slot))
            .is_some_and(|seat| result.winner.is_some_and(|winner| winner != seat.team))
    }
}

/// Waits for the game server to exit until `exit_by`, then kills it and waits until `kill_by`
/// for it to be gone (the kill command is part of that wait). False when it is still there: the supervisor lets go of it then (it is killed once more
/// when dropped), so that the match, its port and its seats are never held by a stuck process.
async fn gone_by(game: &mut GameProcess, exit_by: Instant, kill_by: Instant) -> bool {
    if timeout_at(exit_by, game.exited()).await.is_ok() {
        return true;
    }
    timeout_at(kill_by, async {
        game.kill().await;
        game.exited().await
    })
    .await
    .is_ok()
}

/// The stdout closed without a result: what the exit code says.
async fn exit_end(game: &mut GameProcess, exit: Duration) -> End {
    let code = match tokio::time::timeout(exit, game.exited()).await {
        Ok(code) => code,
        Err(_) => {
            game.kill().await;
            return End::Crashed("stdout closed, the process lived on".to_owned());
        }
    };
    match code {
        Some(EXIT_ABANDONED) => End::Abandoned,
        code => End::Crashed(format!("exited ({code:?}) without a result")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::template::SeatSpec;
    use radovid_proto::{AccountKind, Ticket};

    /// Guard-only: queue formation currently fills every side, but the result
    /// boundary must remain safe if that internal guarantee changes.
    #[test]
    fn a_rated_result_cannot_name_a_side_without_a_reserved_seat() {
        let ruleset: RulesetConfig = toml::from_str(
            r#"
            id = "guard"
            name = "Guard"
            game = "test"
            game_fingerprint = "0123456789abcdef"
            teams = [1, 1]
            rated = true
            [launcher]
            kind = "local"
            binary = "/bin/true"
            public_url = "tcp://127.0.0.1:{port}"
        "#,
        )
        .expect("rules");
        let spec = LaunchSpec {
            public_identity: None,
            match_id: MatchId(1),
            port: 1,
            cheats: false,
            seats: vec![SeatSpec {
                account: radovid_proto::AccountId(42),
                ticket: Ticket([0; 32]),
                team: 0,
                name: "seat".into(),
                kind: AccountKind::Human,
            }],
            artifacts: Vec::new(),
        };
        let supervisor = Supervisor {
            patience: Patience::DEFAULT,
            match_id: MatchId(1),
            ruleset,
            rated: true,
            spec,
            hub: mpsc::channel(1).0,
        };
        let result = GameResult {
            winner: Some(1),
            duration_ms: 1,
            report: Vec::new(),
            left: None,
            artifact_truncated: false,
            aborted: None,
        };
        assert!(matches!(
            supervisor.end_of(
                Line::Result(result),
                &Ledger::new(Instant::now(), Duration::from_secs(1))
            ),
            End::Crashed(_)
        ));
    }

    /// A game server that says READY and then ignores SIGTERM.
    fn deaf_ruleset(max_duration_secs: u64) -> RulesetConfig {
        toml::from_str(&format!(
            r#"
            id = "deaf"
            name = "Deaf"
            game = "test"
            game_fingerprint = "0123456789abcdef"
            teams = [1, 1]
            rated = false
            max_duration_secs = {max_duration_secs}
            [launcher]
            kind = "local"
            binary = "/bin/sh"
            args = ["-c", "echo READY; trap '' TERM; exec sleep 60"]
            public_url = "tcp://127.0.0.1:{{port}}"
        "#
        ))
        .expect("rules")
    }

    fn deaf_supervisor(
        ruleset: RulesetConfig,
        patience: Patience,
    ) -> (Supervisor, mpsc::Receiver<Cmd>) {
        let (hub, heard) = mpsc::channel(16);
        let spec = LaunchSpec {
            match_id: MatchId(7),
            port: 7,
            cheats: false,
            seats: Vec::new(),
            artifacts: Vec::new(),
            public_identity: None,
        };
        let supervisor = Supervisor {
            patience,
            match_id: MatchId(7),
            ruleset,
            rated: false,
            spec,
            hub,
        };
        (supervisor, heard)
    }

    const QUICK: Patience = Patience {
        exit: Duration::from_millis(100),
        kill: Duration::from_millis(100),
        second: Duration::from_millis(20),
    };

    /// The whole stop of a game that ignores the request, from the request to the port being
    /// free, fits one deadline: the answer's wait, the kill and the wait for the exit are not
    /// added up one after another without an end.
    #[tokio::test]
    async fn a_stop_the_game_ignores_ends_within_the_answer_and_the_kill_and_frees_the_port() {
        let (supervisor, mut heard) = deaf_supervisor(deaf_ruleset(3600), QUICK);
        let (stop, stopped) = oneshot::channel();
        let task = tokio::spawn(supervisor.run(stopped));
        assert!(matches!(heard.recv().await, Some(Cmd::MatchReady { .. })));
        let asked = Instant::now();
        stop.send(()).expect("the supervisor listens");
        let mut ended = None;
        let mut port_freed = false;
        while !port_freed {
            match tokio::time::timeout(Duration::from_secs(5), heard.recv())
                .await
                .expect("the supervisor lets go within its bound")
            {
                Some(Cmd::MatchEnded { end, .. }) => ended = Some(end),
                Some(Cmd::PortFree { .. }) => port_freed = true,
                _ => panic!("unexpected command to the hub"),
            }
        }
        assert!(matches!(ended, Some(End::Stopped)), "{ended:?}");
        let took = asked.elapsed();
        assert!(
            took >= QUICK.exit && took < QUICK.exit + QUICK.kill + Duration::from_secs(1),
            "answer wait, then kill: {took:?}"
        );
        task.await.expect("the supervisor task");
    }

    /// The limit asked by the service ends the match as the limit's even though the game that
    /// was asked does not answer.
    #[tokio::test]
    async fn a_limit_the_game_does_not_end_by_itself_or_answer_ends_as_the_time_limit() {
        let (supervisor, mut heard) = deaf_supervisor(deaf_ruleset(1), QUICK);
        let (_stop, stopped) = oneshot::channel();
        let task = tokio::spawn(supervisor.run(stopped));
        assert!(matches!(heard.recv().await, Some(Cmd::MatchReady { .. })));
        let ended = match tokio::time::timeout(Duration::from_secs(10), heard.recv())
            .await
            .expect("the limit is enforced")
        {
            Some(Cmd::MatchEnded { end, .. }) => end,
            _ => panic!("unexpected command to the hub"),
        };
        assert!(matches!(ended, End::TimedOut), "{ended:?}");
        task.await.expect("the supervisor task");
    }

    /// A killed game is waited for within `kill_grace` after the deadline, and is gone.
    #[tokio::test]
    async fn a_game_that_ignores_the_request_is_killed_at_the_deadline_and_gone() {
        let (supervisor, _heard) = deaf_supervisor(deaf_ruleset(3600), QUICK);
        let mut game = launcher::spawn(&supervisor.ruleset, &supervisor.spec).expect("a game");
        game.request_stop().await;
        let started = Instant::now();
        assert!(
            gone_by(
                &mut game,
                started + QUICK.exit,
                started + QUICK.exit + QUICK.kill
            )
            .await
        );
        assert!(started.elapsed() >= QUICK.exit, "the deadline was waited");
    }

    #[test]
    fn an_aborted_word_is_believed_only_when_something_called_for_it() {
        let spawned = Instant::now();
        let limit = Duration::from_secs(100);
        let at = |secs: u64| spawned + Duration::from_secs(secs);
        let fresh = Ledger::new(spawned, limit);
        assert_eq!(fresh.believe(Aborted::Stopped, at(5)), None, "nobody asked");
        assert_eq!(
            fresh.believe(Aborted::TimeLimit, at(99)),
            None,
            "the limit is not due"
        );
        assert_eq!(
            fresh.believe(Aborted::TimeLimit, at(100)),
            Some(Ending::TimeLimit),
            "the game's own end at the limit, unasked"
        );
        let mut stopped = fresh;
        stopped.ask(Ending::Stopped);
        assert_eq!(
            stopped.believe(Aborted::Stopped, at(5)),
            Some(Ending::Stopped)
        );
        assert_eq!(
            stopped.believe(Aborted::TimeLimit, at(5)),
            None,
            "asked to stop is no reason to claim the limit"
        );
        let mut limited = fresh;
        limited.ask(Ending::TimeLimit);
        assert_eq!(
            limited.believe(Aborted::Stopped, at(105)),
            Some(Ending::TimeLimit),
            "the service's reason is the limit, whatever the game calls its answer"
        );
        limited.ask(Ending::Stopped);
        assert_eq!(
            limited.believe(Aborted::Stopped, at(105)),
            Some(Ending::TimeLimit),
            "the first ask stands"
        );
    }
}
