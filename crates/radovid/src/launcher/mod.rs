//! Starting one game server per match from a ruleset's launcher: a local
//! process or a docker container, its stdout piped to radovid (the launcher
//! contract, DEVELOPMENT.md "Launcher contract").

mod lines;

use std::process::Stdio;
use std::time::Duration;

use radovid_proto::MatchId;
use tokio::process::{Child, ChildStdout, Command};

use crate::config::{Artifact, LauncherConfig, LauncherKind, RulesetConfig, parse_fingerprint};
use crate::template::{self, SeatSpec, Vars};

pub use lines::{Aborted, GameResult, Line, LineReader};
use lines::{MAX_DURATION_VAR, RESULT_VERSION, RESULT_VERSION_VAR};

/// The label every container carries, for cleaning up after a crashed service.
const LABEL: &str = "radovid.match";
/// How long a version probe may take.
const PROBE_TIMEOUT: Duration = Duration::from_secs(60);
/// How long a command that controls a running game (`docker kill`, `kill`) may take: a docker
/// client that hangs must not hold up the end of a match. The command is killed with it.
const CONTROL_TIMEOUT: Duration = Duration::from_secs(2);
/// Where a docker game sees the artifact called `replay`; the others are under
/// [`ARTIFACTS_IN_CONTAINER`] by name.
const REPLAY_IN_CONTAINER: &str = "/artifact";
const ARTIFACTS_IN_CONTAINER: &str = "/artifacts";
/// Lines of a game server's stderr logged at most, and their length.
const STDERR_LINES: usize = 200;
const STDERR_LINE_LEN: usize = 300;

/// Everything one launch is made of.
pub struct LaunchSpec {
    pub public_identity: Option<String>,
    pub match_id: MatchId,
    pub port: u16,
    /// The ruleset's `cheats_args` are added: a lobby's match whose owner turned them on.
    pub cheats: bool,
    pub seats: Vec<SeatSpec>,
    /// The artifacts the game is asked to leave: those of its ruleset that are kept.
    pub artifacts: Vec<Artifact>,
}

/// An artifact file made ready for a game.
struct Placed {
    /// The file as radovid sees it.
    host: std::path::PathBuf,
    /// The path the game is told: the file itself, or where a container sees it.
    seen: String,
    max_bytes: u64,
}

/// A running game server.
pub struct GameProcess {
    child: Child,
    pub stdout: LineReader<ChildStdout>,
    /// docker: the container's name and the client that kills it.
    container: Option<(String, String)>,
}

impl GameProcess {
    /// Kills the game server; idempotent. Bounded by [`CONTROL_TIMEOUT`].
    pub async fn kill(&mut self) {
        if let Some((docker, name)) = &self.container {
            control(Command::new(docker).args(["kill", name])).await;
        }
        let _ = self.child.start_kill();
    }

    /// Asks the game server to finish the match now (SIGTERM, to the container for a docker
    /// game): it tells its players, closes its artifacts, prints a `RESULT` with `aborted=...`
    /// and exits. A game that does nothing of the kind is killed by whoever asked, after a grace.
    /// Bounded by [`CONTROL_TIMEOUT`].
    pub async fn request_stop(&mut self) {
        if let Some((docker, name)) = &self.container {
            control(Command::new(docker).args(["kill", "--signal", "TERM", name])).await;
        } else if let Some(pid) = self.child.id() {
            control(Command::new("kill").args(["-TERM", &pid.to_string()])).await;
        }
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

/// Runs a command that controls a game, silently and for [`CONTROL_TIMEOUT`] at the most; the
/// command is killed when it is late.
async fn control(command: &mut Command) {
    let run = command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .status();
    let _ = tokio::time::timeout(CONTROL_TIMEOUT, run).await;
}

/// A match without cheats whose arguments or environment, as its placeholders expanded them,
/// hold a marker of the ruleset's cheats (`config::cheats_markers`) is not started: the check at
/// start reads placeholders as written, and one may expand to what turns cheats on. Names where,
/// never the values (the environment holds tickets).
fn refuse_cheats(
    ruleset: &RulesetConfig,
    args: &[String],
    env: &[(String, String)],
) -> std::io::Result<()> {
    let markers = crate::config::cheats_markers(&ruleset.launcher.cheats_args);
    if markers.is_empty() {
        return Ok(());
    }
    let lists = std::iter::once((
        "its arguments".to_owned(),
        args.iter().map(String::as_str).collect(),
    ))
    .chain(env.iter().map(|(name, value)| {
        (
            format!("its env {name}"),
            value.split_whitespace().collect(),
        )
    }));
    match crate::config::cheats_marker_in(&markers, lists) {
        Some((place, marker)) => Err(std::io::Error::other(CheatsRefused(format!(
            "refused: a match without cheats would get cheats_args {marker:?} in {place}, as \
             placeholders expanded them"
        )))),
        None => Ok(()),
    }
}

/// A launch refused because a match without cheats would get them ([`refuse_cheats`]).
#[derive(Debug)]
pub struct CheatsRefused(pub String);

impl std::fmt::Display for CheatsRefused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for CheatsRefused {}

/// Starts the game server of one match.
pub fn spawn(ruleset: &RulesetConfig, spec: &LaunchSpec) -> std::io::Result<GameProcess> {
    let launcher = &ruleset.launcher;
    let mut placed = Vec::new();
    for artifact in &spec.artifacts {
        let host = prepare_file(launcher.kind, artifact, spec.match_id).map_err(|error| {
            std::io::Error::other(format!("the artifact {}: {error}", artifact.name))
        })?;
        let seen = match launcher.kind {
            LauncherKind::Local => host.to_string_lossy().into_owned(),
            LauncherKind::Docker => inside(&artifact.name),
        };
        placed.push(Placed {
            host,
            seen,
            max_bytes: artifact.max_bytes,
        });
    }
    fn vars<'a>(spec: &'a LaunchSpec, artifact: Option<&'a str>) -> Vars<'a> {
        Vars {
            match_id: spec.match_id,
            port: spec.port,
            seats: &spec.seats,
            artifact,
        }
    }
    // What an artifact needs the game to be told is added only while it is kept, each told its
    // own path.
    let mut args: Vec<String> = launcher
        .args
        .iter()
        .map(|a| template::expand(a, &vars(spec, None)))
        .collect();
    if spec.cheats {
        args.extend(
            launcher
                .cheats_args
                .iter()
                .map(|a| template::expand(a, &vars(spec, None))),
        );
    }
    let mut env: Vec<(String, String)> = launcher
        .env
        .iter()
        .map(|(name, value)| (name.clone(), template::expand(value, &vars(spec, None))))
        .collect();
    for (artifact, placed) in spec.artifacts.iter().zip(&placed) {
        let vars = vars(spec, Some(&placed.seen));
        args.extend(artifact.args.iter().map(|a| template::expand(a, &vars)));
        env.extend(
            artifact
                .env
                .iter()
                .map(|(name, value)| (name.clone(), template::expand(value, &vars))),
        );
    }
    if !spec.cheats {
        refuse_cheats(ruleset, &args, &env)?;
    }
    // Last, so a ruleset's own variables cannot overrule what this radovid reads and says.
    env.push((RESULT_VERSION_VAR.to_owned(), RESULT_VERSION.to_string()));
    env.push((
        "RADOVID_PUBLIC_IDENTITY".into(),
        spec.public_identity.clone().unwrap_or_default(),
    ));
    env.push((
        MAX_DURATION_VAR.to_owned(),
        ruleset.max_duration_secs.to_string(),
    ));
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
            command.args(docker_run_args(launcher, spec, &name, &args, &placed, &env));
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

/// Where a docker game sees an artifact.
fn inside(name: &str) -> String {
    if name == crate::config::REPLAY_ARTIFACT {
        REPLAY_IN_CONTAINER.to_owned()
    } else {
        format!("{ARTIFACTS_IN_CONTAINER}/{name}")
    }
}

/// Makes an artifact's file for the match empty before the game starts: a docker container is
/// given this one file to write and nothing else, which the container's user must be able to
/// open, so it is world-writable and the directory above it is the operator's to keep closed.
fn prepare_file(
    kind: LauncherKind,
    artifact: &Artifact,
    match_id: MatchId,
) -> std::io::Result<std::path::PathBuf> {
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    let template =
        crate::artifacts::Template::parse(&artifact.path).map_err(std::io::Error::other)?;
    let path = template.path(match_id);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mode = if kind == LauncherKind::Docker {
        0o666
    } else {
        0o600
    };
    std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(mode)
        .open(&path)?
        .set_permissions(std::fs::Permissions::from_mode(mode))?;
    Ok(path)
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
    placed: &[Placed],
    env: &[(String, String)],
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
    // The only writable things: this match's artifact files, bounded in size (a process has one
    // limit, the largest); the rest of the container stays read-only.
    for file in placed {
        run.extend([
            "--volume".to_owned(),
            format!("{}:{}:rw", file.host.display(), file.seen),
        ]);
    }
    if let Some(largest) = placed.iter().map(|file| file.max_bytes).max() {
        run.extend(["--ulimit".to_owned(), format!("fsize={largest}:{largest}")]);
    }
    for (name, _) in env {
        run.extend(["--env".to_owned(), name.clone()]);
    }
    run.push(launcher.image.clone().unwrap_or_default());
    run.extend(args.iter().cloned());
    run
}

/// Logs the first lines of a game server's stderr, cut short: a game prints
/// no secret there (DEVELOPMENT.md "Launcher contract"). Reads to the end, a line at
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
