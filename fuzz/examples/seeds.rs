//! Writes seed inputs for the targets whose inputs are encoded messages:
//! `cargo run --manifest-path fuzz/Cargo.toml --example seeds`.

use std::path::Path;

use radovid_proto::{
    AccountId, AccountKind, Audience, ClientFrame, ClientInfo, ClientMsg, LobbyId, MatchId,
    ProposalId, QueueEntryId, RulesetId, Secret, UnixMs, encode_frame_to_vec,
};

fn payload(msg: &ClientMsg) -> Vec<u8> {
    encode_frame_to_vec(msg).expect("encodes")[4..].to_vec()
}

/// One piece of a `hub_session` input.
fn piece(actor: u8, msg: &ClientMsg) -> Vec<u8> {
    let body = payload(msg);
    let mut out = ((body.len() + 1) as u16).to_le_bytes().to_vec();
    out.push(actor);
    out.extend_from_slice(&body);
    out
}

fn write(target: &str, name: &str, bytes: &[u8]) {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("corpus")
        .join(target);
    std::fs::create_dir_all(&dir).expect("corpus");
    std::fs::write(dir.join(format!("seed-{name}")), bytes).expect("seed");
}

fn ruleset(id: &str) -> RulesetId {
    RulesetId(id.to_owned())
}

fn main() {
    let lobby = ClientMsg::LobbyCreate {
        ruleset: ruleset("duel"),
        name: "room".to_owned(),
        audience: Audience::Everyone,
        password: Some(Secret::new("pw")),
    };
    let scripts: Vec<(&str, Vec<(u8, ClientMsg)>)> = vec![
        (
            "queue",
            vec![
                (
                    1,
                    ClientMsg::QueueJoin {
                        ruleset: ruleset("solo"),
                    },
                ),
                (
                    2,
                    ClientMsg::QueueJoin {
                        ruleset: ruleset("solo"),
                    },
                ),
                (
                    1,
                    ClientMsg::MatchAccept {
                        proposal: ProposalId(1),
                    },
                ),
                (
                    2,
                    ClientMsg::MatchAccept {
                        proposal: ProposalId(1),
                    },
                ),
                (1, ClientMsg::MyMatches),
                (
                    1,
                    ClientMsg::MatchHandoff {
                        match_id: MatchId(1),
                    },
                ),
                (
                    0,
                    ClientMsg::AdminStopMatch {
                        match_id: MatchId(1),
                    },
                ),
                (
                    2,
                    ClientMsg::QueueLeave {
                        entry: QueueEntryId(2),
                    },
                ),
            ],
        ),
        (
            "lobby",
            vec![
                (1, lobby.clone()),
                (2, ClientMsg::LobbyList),
                (
                    2,
                    ClientMsg::LobbyAsk {
                        lobby: LobbyId(1),
                        password: Some(Secret::new("pw")),
                    },
                ),
                (
                    1,
                    ClientMsg::LobbyDecide {
                        lobby: LobbyId(1),
                        account: AccountId(3),
                        allow: true,
                    },
                ),
                (
                    2,
                    ClientMsg::LobbyReady {
                        lobby: LobbyId(1),
                        ready: true,
                    },
                ),
                (
                    1,
                    ClientMsg::LobbyReady {
                        lobby: LobbyId(1),
                        ready: true,
                    },
                ),
                (
                    3,
                    ClientMsg::LobbyJoinByName {
                        name: "room".to_owned(),
                        password: None,
                    },
                ),
                (
                    1,
                    ClientMsg::LobbyKick {
                        lobby: LobbyId(1),
                        account: AccountId(3),
                    },
                ),
                (2, ClientMsg::LobbyLeave { lobby: LobbyId(1) }),
            ],
        ),
        (
            "account",
            vec![
                (
                    1,
                    ClientMsg::ChangeNickname {
                        nickname: "newname".to_owned(),
                    },
                ),
                (
                    1,
                    ClientMsg::Account {
                        account: AccountId(2),
                    },
                ),
                (
                    2,
                    ClientMsg::RecentMatches {
                        account: AccountId(2),
                        limit: 10,
                    },
                ),
                (
                    2,
                    ClientMsg::Ladder {
                        ruleset: ruleset("solo"),
                        offset: 0,
                        limit: 10,
                    },
                ),
                (
                    0,
                    ClientMsg::AdminBan {
                        account: AccountId(3),
                        reason: "x".to_owned(),
                        until: Some(UnixMs(1)),
                    },
                ),
                (
                    0,
                    ClientMsg::AdminUnban {
                        account: AccountId(3),
                    },
                ),
                (
                    0,
                    ClientMsg::AdminSetKind {
                        account: AccountId(4),
                        kind: AccountKind::Bot,
                    },
                ),
                (
                    2,
                    ClientMsg::ChangePassword {
                        current: Secret::new("correct horse battery"),
                        new: Secret::new("another password"),
                    },
                ),
                (
                    3,
                    ClientMsg::Limits {
                        ruleset: Some(ruleset("trio")),
                    },
                ),
                (2, ClientMsg::Logout),
            ],
        ),
    ];
    for (name, script) in &scripts {
        let input: Vec<u8> = script
            .iter()
            .flat_map(|(actor, msg)| piece(*actor, msg))
            .collect();
        write("hub_session", name, &input);
        for (at, (_, msg)) in script.iter().enumerate() {
            let frame = ClientFrame {
                seq: at as u32,
                msg: msg.clone(),
            };
            write(
                "proto_decode",
                &format!("{name}-{at}"),
                &encode_frame_to_vec(&frame).expect("encodes")[4..],
            );
        }
    }
    let hello = ClientMsg::Register {
        login: "someone".to_owned(),
        password: Secret::new("long enough"),
        kind: AccountKind::Bot,
        client: ClientInfo {
            bot_version: Some("v1".to_owned()),
        },
    };
    write("proto_decode", "register", &payload(&hello));
    write(
        "launcher_lines",
        "result",
        b"noise\nREADY\nRESULT winner=1 duration_ms=5 report=00ff\n",
    );
    write(
        "launcher_lines",
        "draw",
        b"READY\r\nRESULT winner=draw duration_ms=0 report=\n",
    );
    let example =
        std::fs::read(Path::new(env!("CARGO_MANIFEST_DIR")).join("../radovid.example.toml"))
            .expect("the example configuration");
    write("config_file", "example", &example);
}
