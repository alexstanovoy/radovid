//! The docker launcher against a real docker: a game gets one writable file, its match's
//! artifact, and nothing else of the container is writable.
//!
//! Needs a docker that can run `DOCKER_TEST_IMAGE` (any image with `/bin/sh`, e.g. an ubuntu
//! one that is already pulled); run with `cargo test --test docker -- --ignored`.

mod common;

use common::*;
use radovid::config::LauncherKind;
use radovid_client::proto::*;

#[test]
#[ignore = "needs docker and DOCKER_TEST_IMAGE"]
fn a_docker_game_writes_its_artifact_and_nothing_else() {
    let image = std::env::var("DOCKER_TEST_IMAGE").expect("DOCKER_TEST_IMAGE names an image");
    let dir = std::env::temp_dir().join(format!("radovid-docker-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("dir");
    // The game: writes into the artifact, tries to write elsewhere, reports what happened.
    let script = dir.join("game.sh");
    std::fs::write(
        &script,
        r#"echo READY
echo "replay bytes" > "$1"
if echo x > /elsewhere 2>/dev/null; then seen=writable; else seen=readonly; fi
report=$(printf '%s' "$seen" | od -An -v -tx1 | tr -d ' \n')
echo "RESULT winner=0 duration_ms=1 report=$report"
"#,
    )
    .expect("script");
    let mut rules = vec![ruleset("solo", true, "team0", "0")];
    let launcher = &mut rules[0].launcher;
    launcher.kind = LauncherKind::Docker;
    launcher.binary = None;
    launcher.version_probe = Vec::new();
    launcher.image = Some(image);
    launcher.container_port = Some(4456);
    launcher.mounts = vec![format!("{}:/game.sh:ro", script.display())];
    launcher.env.clear();
    launcher.args = ["/bin/sh", "/game.sh", "{artifact}"]
        .map(str::to_owned)
        .to_vec();
    launcher.artifact = Some(format!("{}/replays/replay-{{match_id}}.brp", dir.display()));
    let env = start(rules);
    let (mut a, mut b) = (env.bot("alpha"), env.bot("beta"));
    pair_up(&mut a, &mut b, &solo());
    let result = a.wait_result();
    assert_eq!(result.outcome, Outcome::Winner { team: 0 }, "{result:?}");
    assert_eq!(
        result.game_report, b"readonly",
        "the rest of the container is read-only"
    );
    let file = dir.join(format!("replays/replay-{}.brp", result.match_id.0));
    assert_eq!(
        std::fs::read(&file).expect("the artifact"),
        b"replay bytes\n"
    );
    let Ok(Reply::Artifact { total, data, .. }) = a.ask(ClientMsg::MatchArtifact {
        match_id: result.match_id,
        offset: 0,
    }) else {
        panic!("the artifact is served");
    };
    assert_eq!((total, data), (13, b"replay bytes\n".to_vec()));
    env.stop();
    let _ = std::fs::remove_dir_all(&dir);
}
