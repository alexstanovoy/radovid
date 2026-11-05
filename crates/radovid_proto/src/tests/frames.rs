//! Frozen frames of version 12: the bytes a client of 12 reads for a lobby's state, in a reply
//! and in an event, with each value of its cheats. A change of their layout fails here, whatever
//! the registry says.

use crate::*;

/// A lobby of one member, with `cheats`.
fn lobby(cheats: Option<bool>) -> LobbyState {
    LobbyState {
        id: LobbyId(7),
        name: "sandbox".to_owned(),
        ruleset: RulesetId("full".to_owned()),
        owner: AccountId(3),
        audience: Audience::Everyone,
        rated: false,
        locked: true,
        members: vec![LobbyMember {
            account: AccountId(3),
            nickname: "olga".to_owned(),
            kind: AccountKind::Human,
            bot_version: None,
            rating: None,
            slot: 0,
            team: 0,
            ready: false,
        }],
        retired_pending: Vec::new(),
        starting_in_ms: None,
        teams: vec![5, 5],
        cheats,
    }
}

/// Each frame as the service sends it and its bytes in hex.
fn frames() -> Vec<(ServerMsg, &'static str)> {
    let mut frames = Vec::new();
    for (cheats, reply, event) in [
        (None, REPLY_NONE, EVENT_NONE),
        (Some(false), REPLY_OFF, EVENT_OFF),
        (Some(true), REPLY_ON, EVENT_ON),
    ] {
        let state = lobby(cheats);
        frames.push((
            ServerMsg::Reply {
                seq: 9,
                reply: Reply::Lobby {
                    state: state.clone(),
                },
            },
            reply,
        ));
        frames.push((ServerMsg::Event(Event::LobbyState { state }), event));
    }
    frames
}

const REPLY_NONE: &str =
    "28000000000907070773616e64626f780466756c6c030000010103046f6c6761000000000000000002050500";
const REPLY_OFF: &str =
    "29000000000907070773616e64626f780466756c6c030000010103046f6c676100000000000000000205050100";
const REPLY_ON: &str =
    "29000000000907070773616e64626f780466756c6c030000010103046f6c676100000000000000000205050101";
const EVENT_NONE: &str =
    "270000000206070773616e64626f780466756c6c030000010103046f6c6761000000000000000002050500";
const EVENT_OFF: &str =
    "280000000206070773616e64626f780466756c6c030000010103046f6c676100000000000000000205050100";
const EVENT_ON: &str =
    "280000000206070773616e64626f780466756c6c030000010103046f6c676100000000000000000205050101";

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex(text: &str) -> Vec<u8> {
    (0..text.len())
        .step_by(2)
        .map(|at| u8::from_str_radix(&text[at..at + 2], 16).expect("hex"))
        .collect()
}

#[test]
fn a_lobby_state_of_version_12_keeps_its_bytes_with_every_cheats_value() {
    for (msg, frozen) in frames() {
        let bytes = encode_frame_to_vec(&msg).expect("encoded");
        assert_eq!(hex(&bytes), frozen, "the frame of {msg:?} changed");
        let frozen = unhex(frozen);
        let payload = whole_frame(&frozen, MAX_SERVER_PAYLOAD_LEN).expect("one frame");
        let read: ServerMsg = decode_payload(payload).expect("decoded");
        assert_eq!(read, msg, "the frozen bytes read back");
    }
}
