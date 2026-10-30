//! One task per launched match: starts the game server, reads its stdout
//! (README "Launcher contract") and reports to the hub what became of it.

use std::time::Duration;

use radovid_proto::MatchId;
use tokio::sync::{mpsc, oneshot};
use tokio::time::{Instant, sleep_until};

use crate::config::RulesetConfig;
use crate::hub::Cmd;
use crate::launcher::{self, GameProcess, GameResult, LaunchSpec, Line};

/// How long a game server has to exit after its result, or after its stdout
/// closed, before it is killed. The result is told, and the artifact served, after the exit.
const EXIT_GRACE: Duration = Duration::from_secs(10);
/// The exit code of a game nobody played: nobody came, or everybody left.
const EXIT_ABANDONED: i32 = 3;

/// How a match ended, as the hub learns it.
#[derive(Debug)]
pub enum End {
    /// The game's `RESULT` line.
    Result(GameResult),
    /// Exit code 3 without a result.
    Abandoned,
    /// No start, no `READY` in time, a bad `RESULT`, or an exit without a result.
    Crashed(String),
    /// An admin, the time limit or the service's shutdown.
    Stopped,
}

pub struct Supervisor {
    pub match_id: MatchId,
    pub ruleset: RulesetConfig,
    pub spec: LaunchSpec,
    pub hub: mpsc::Sender<Cmd>,
}

impl Supervisor {
    pub async fn run(self, mut stop: oneshot::Receiver<()>) {
        let match_id = self.match_id;
        match launcher::spawn(&self.ruleset, &self.spec) {
            Err(error) => {
                let end = End::Crashed(format!("start: {error}"));
                let _ = self.hub.send(Cmd::MatchEnded { match_id, end }).await;
            }
            Ok(mut game) => {
                let end = self.attend(&mut game, &mut stop).await;
                if !matches!(end, End::Result(_)) {
                    game.kill().await;
                }
                // The hub hears of the end, and the seats may fetch the artifact, only once the
                // process is gone and so its files are closed.
                if tokio::time::timeout(EXIT_GRACE, game.exited())
                    .await
                    .is_err()
                {
                    game.kill().await;
                    game.exited().await;
                }
                let _ = self.hub.send(Cmd::MatchEnded { match_id, end }).await;
            }
        }
        // The port is free only once the game server is gone.
        let port = self.spec.port;
        let _ = self.hub.send(Cmd::PortFree { port }).await;
    }

    async fn attend(&self, game: &mut GameProcess, stop: &mut oneshot::Receiver<()>) -> End {
        let start_by =
            Instant::now() + Duration::from_secs(self.ruleset.launcher.start_timeout_secs);
        tokio::select! {
            line = game.stdout.next() => match line {
                Some(Line::Ready) => {}
                Some(line) => return self.end_of(line),
                None => return exit_end(game).await,
            },
            _ = &mut *stop => return End::Stopped,
            _ = sleep_until(start_by) => return End::Crashed("no READY in time".to_owned()),
        }
        let match_id = self.match_id;
        let _ = self.hub.send(Cmd::MatchReady { match_id }).await;
        let end_by = Instant::now() + Duration::from_secs(self.ruleset.max_duration_secs);
        loop {
            tokio::select! {
                line = game.stdout.next() => match line {
                    Some(Line::Ready) => {}
                    Some(line) => return self.end_of(line),
                    None => return exit_end(game).await,
                },
                _ = &mut *stop => return End::Stopped,
                _ = sleep_until(end_by) => return End::Stopped,
            }
        }
    }

    /// A `RESULT` line, checked against the ruleset's teams.
    fn end_of(&self, line: Line) -> End {
        match line {
            Line::Result(result)
                if result
                    .winner
                    .is_none_or(|team| usize::from(team) < self.ruleset.teams.len())
                    && self.leaver_lost(&result) =>
            {
                End::Result(result)
            }
            Line::Result(result) => End::Crashed(format!(
                "RESULT names team {:?} of {}",
                result.winner,
                self.ruleset.teams.len()
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
        let mut first = 0usize;
        for (team, size) in self.ruleset.teams.iter().enumerate() {
            let end = first + usize::from(*size);
            if (first..end).contains(&usize::from(slot)) {
                return result
                    .winner
                    .is_some_and(|winner| usize::from(winner) != team);
            }
            first = end;
        }
        false
    }
}

/// The stdout closed without a result: what the exit code says.
async fn exit_end(game: &mut GameProcess) -> End {
    let code = match tokio::time::timeout(EXIT_GRACE, game.exited()).await {
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
