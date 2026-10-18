//! Starting one game server per match from a ruleset's launcher: a local
//! process or a docker container, its stdout piped to radovid (the launcher
//! contract, README "Launcher contract").

mod lines;

use std::process::Stdio;
use std::time::Duration;

use radovid_proto::MatchId;
use tokio::process::{Child, ChildStdout, Command};

use crate::config::{LauncherConfig, LauncherKind, RulesetConfig, parse_fingerprint};
use crate::template::{self, SeatSpec, Vars};

pub use lines::{GameResult, Line, LineReader};

/// The label every container carries, for cleaning up after a crashed service.
const LABEL: &str = "radovid.match";
/// How long a version probe may take.
const PROBE_TIMEOUT: Duration = Duration::from_secs(60);
/// Lines of a game server's stderr logged at most, and their length.
const STDERR_LINES: usize = 200;
const STDERR_LINE_LEN: usize = 300;

/// Everything one launch is made of.
pub struct LaunchSpec {
    pub match_id: MatchId,
    pub port: u16,
    pub seats: Vec<SeatSpec>,
}

/// A running game server.
pub struct GameProcess {
    child: Child,
    pub stdout: LineReader<ChildStdout>,
    /// docker: the container's name and the client that kills it.
    container: Option<(String, String)>,
}

impl GameProcess {
    /// Kills the game server; idempotent.
    pub async fn kill(&mut self) {
        if let Some((docker, name)) = &self.container {
            let _ = Command::new(docker)
                .args(["kill", name])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .await;
        }
        let _ = self.child.start_kill();
    }

    /// Waits for the exit; its code, if it exited by itself.
    pub async fn exited(&mut self) -> Option<i32> {
        self.child
            .wait()
            .await
            .ok()
            .and_then(|status| status.code())
    }
}

/// Starts the game server of one match.
pub fn spawn(ruleset: &RulesetConfig, spec: &LaunchSpec) -> std::io::Result<GameProcess> {
    let launcher = &ruleset.launcher;
    let vars = Vars {
        match_id: spec.match_id,
        port: spec.port,
        seats: &spec.seats,
    };
    let args: Vec<String> = launcher
        .args
        .iter()
        .map(|a| template::expand(a, &vars))
        .collect();
    let env: Vec<(&String, String)> = launcher
        .env
        .iter()
        .map(|(name, value)| (name, template::expand(value, &vars)))
        .collect();
    let (mut command, container) = match launcher.kind {
        LauncherKind::Local => {
            let mut command = Command::new(launcher.binary.as_deref().unwrap_or("".as_ref()));
            command.args(&args);
            (command, None)
        }
        LauncherKind::Docker => {
            let name = format!("radovid-match-{}", spec.match_id.0);
            let docker = docker_binary(launcher);
            let mut command = Command::new(&docker);
            command.args(docker_run_args(launcher, spec, &name, &args));
            (command, Some((docker, name)))
        }
    };
    // Values travel in the environment only: `docker run -e NAME` passes on
    // the client's own variable without putting the value on a command line.
    command.envs(env.iter().map(|(name, value)| (name.as_str(), value)));
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| std::io::Error::other("no stdout"))?;
    if let Some(stderr) = child.stderr.take() {
        tokio::spawn(log_stderr(spec.match_id, stderr));
    }
    Ok(GameProcess {
        child,
        stdout: LineReader::new(stdout),
        container,
    })
}

fn docker_binary(launcher: &LauncherConfig) -> String {
    launcher
        .docker
        .clone()
        .unwrap_or_else(|| "docker".to_owned())
}

/// `docker run` attached (its stdout is the container's), removed on exit,
/// read-only, without capabilities, bounded, the game port published.
fn docker_run_args(
    launcher: &LauncherConfig,
    spec: &LaunchSpec,
    name: &str,
    args: &[String],
) -> Vec<String> {
    let publish = match launcher.publish_address {
        Some(std::net::IpAddr::V6(v6)) => format!("[{v6}]"),
        Some(v4) => v4.to_string(),
        None => "0.0.0.0".to_owned(),
    };
    let mut run: Vec<String> = [
        "run",
        "--rm",
        "--name",
        name,
        "--label",
        &format!("{LABEL}={}", spec.match_id.0),
        "--read-only",
        "--cap-drop",
        "ALL",
        "--security-opt",
        "no-new-privileges",
        "--pids-limit",
        &launcher.pids_limit.unwrap_or(256).to_string(),
        "--memory",
        launcher.memory.as_deref().unwrap_or("512m"),
        "--cpus",
        launcher.cpus.as_deref().unwrap_or("1"),
        "--publish",
        &format!(
            "{publish}:{}:{}",
            spec.port,
            launcher.container_port.unwrap_or(spec.port)
        ),
    ]
    .map(str::to_owned)
    .to_vec();
    if let Some(network) = &launcher.network {
        run.extend(["--network".to_owned(), network.clone()]);
    }
    for mount in &launcher.mounts {
        run.extend(["--volume".to_owned(), mount.clone()]);
    }
    for name in launcher.env.keys() {
        run.extend(["--env".to_owned(), name.clone()]);
    }
    run.push(launcher.image.clone().unwrap_or_default());
    run.extend(args.iter().cloned());
    run
}

/// Logs the first lines of a game server's stderr, cut short: a game prints
/// no secret there (README "Launcher contract"). Reads to the end, a line at
/// a time within the bound and whatever its bytes, so the game never blocks
/// on a full pipe nor fails to write to a closed one.
async fn log_stderr(match_id: MatchId, stderr: tokio::process::ChildStderr) {
    let mut lines = LineReader::new(stderr);
    let mut logged = 0;
    while let Some(line) = lines.next_line().await {
        if logged < STDERR_LINES {
            let line = String::from_utf8_lossy(&line);
            let cut: String = line.chars().take(STDERR_LINE_LEN).collect();
            eprintln!("radovid: match {}: {cut}", match_id.0);
            logged += 1;
        }
    }
}

/// Runs the launcher's version probe and compares what it prints with the
/// ruleset's fingerprint; a ruleset without a probe passes.
pub async fn probe(ruleset: &RulesetConfig) -> Result<(), String> {
    let launcher = &ruleset.launcher;
    if launcher.version_probe.is_empty() {
        return Ok(());
    }
    let mut command = match launcher.kind {
        LauncherKind::Local => Command::new(launcher.binary.as_deref().unwrap_or("".as_ref())),
        LauncherKind::Docker => {
            let mut command = Command::new(docker_binary(launcher));
            command.args([
                "run",
                "--rm",
                "--network",
                "none",
                "--read-only",
                "--cap-drop",
                "ALL",
                "--security-opt",
                "no-new-privileges",
                launcher.image.as_deref().unwrap_or_default(),
            ]);
            command
        }
    };
    command
        .args(&launcher.version_probe)
        .stdin(Stdio::null())
        .kill_on_drop(true);
    let output = tokio::time::timeout(PROBE_TIMEOUT, command.output())
        .await
        .map_err(|_| "the version probe did not finish in time".to_owned())?
        .map_err(|e| format!("the version probe did not run: {e}"))?;
    let printed = String::from_utf8_lossy(&output.stdout);
    match parse_fingerprint(&printed) {
        Some(theirs) if theirs == ruleset.game_fingerprint => Ok(()),
        Some(theirs) => Err(format!(
            "the game server speaks wire {theirs:016x} and the ruleset names {:016x}",
            ruleset.game_fingerprint
        )),
        None => Err(format!(
            "the version probe printed no fingerprint ({}): {:?}",
            output.status,
            printed.trim()
        )),
    }
}

/// Removes the containers an earlier run of the service left behind.
pub async fn clean_up_orphans(rulesets: &[RulesetConfig]) {
    let mut dockers: Vec<String> = rulesets
        .iter()
        .filter(|r| r.launcher.kind == LauncherKind::Docker)
        .map(|r| docker_binary(&r.launcher))
        .collect();
    dockers.sort();
    dockers.dedup();
    for docker in dockers {
        let filter = format!("label={LABEL}");
        let listed = tokio::time::timeout(
            PROBE_TIMEOUT,
            Command::new(&docker)
                .args(["ps", "-aq", "--filter", &filter])
                .stdin(Stdio::null())
                .output(),
        )
        .await;
        let Ok(Ok(listed)) = listed else {
            continue;
        };
        for id in String::from_utf8_lossy(&listed.stdout).split_whitespace() {
            let _ = Command::new(&docker)
                .args(["rm", "-f", id])
                .stdin(Stdio::null())
                .output()
                .await;
        }
    }
}
