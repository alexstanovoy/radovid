//! Custom games: join requests, kicks, readiness, the countdown nobody can
//! cancel, and the lobby closing with its owner.

mod common;

use std::time::{Duration, Instant};

use bota_proto::Team;
use common::*;
use radovid_client::proto::*;

fn rules() -> Vec<radovid::config::RulesetConfig> {
    vec![
        ruleset("solo_mid", true, &["--outcome", "dire", "--after-ms", "300"]),
        ruleset("fun", false, &["--outcome", "radiant", "--after-ms", "300"]),
    ]
}

fn create(owner: &mut Account, audience: Audience) -> LobbyState {
    let Ok(Reply::Lobby { state }) = owner.ask(ClientMsg::LobbyCreate {
        ruleset: RulesetId("fun".to_owned()),
        name: "casual game".to_owned(),
        audience,
    }) else {
        panic!("lobby");
    };
    state
}

/// `member` asks into `lobby`; the owner lets it in.
fn join(owner: &mut Account, member: &mut Account, lobby: LobbyId) {
    member.expect_done(ClientMsg::LobbyAsk { lobby });
    owner.wait_for(10, |event| match event {
        Event::LobbyState { state } if state.pending.iter().any(|m| m.account == member.me.id) => Some(()),
        _ => None,
    });
    owner.expect_done(ClientMsg::LobbyDecide { account: member.me.id, allow: true });
    member.wait_for(10, |event| matches!(event, Event::LobbyJoined { .. }).then_some(()));
}

#[test]
fn a_lobby_fills_kicks_counts_down_and_starts_an_unrated_match() {
    let env = start(rules());
    let mut owner = env.account("olga", AccountKind::Human);
    let mut guest = env.bot("gus");
    let state = create(&mut owner, Audience::Everyone);
    assert_eq!((state.members.len(), state.members[0].account, state.rated), (1, owner.me.id, false));
    assert_eq!((state.members[0].slot.0, state.members[0].team), (0, Team::Radiant));

    // Listed while it has a free seat; a request waits for the owner.
    let Ok(Reply::Lobbies { list }) = guest.ask(ClientMsg::LobbyList) else { panic!("list") };
    assert_eq!((list.len(), list[0].owner_login.as_str(), list[0].members, list[0].seats), (1, "olga", 1, 2));
    assert_eq!(guest.ask(ClientMsg::LobbyAsk { lobby: LobbyId(999) }), Err(Error::UnknownLobby));
    join(&mut owner, &mut guest, state.id);
    let joined = owner.wait_for(10, |event| match event {
        Event::LobbyState { state } if state.members.len() == 2 => Some(state),
        _ => None,
    });
    assert_eq!((joined.members[1].login.as_str(), joined.members[1].team, joined.members[1].bot_version.as_deref()), ("gus", Team::Dire, Some("test-1")));
    assert_eq!(guest.ask(ClientMsg::LobbyDecide { account: owner.me.id, allow: true }), Err(Error::NotLobbyOwner));
    assert_eq!(
        guest.ask(ClientMsg::QueueJoin { ruleset: RulesetId("solo_mid".to_owned()) }),
        Err(Error::Busy { presence: Presence::InLobby { lobby: state.id } })
    );

    // The owner cannot start before everyone is ready; a kick frees the seat.
    assert_eq!(owner.ask(ClientMsg::LobbyReady { ready: true }), Err(Error::NotAllReady));
    assert_eq!(owner.ask(ClientMsg::LobbyKick { account: owner.me.id }), Err(Error::NoSuchMember));
    owner.expect_done(ClientMsg::LobbyKick { account: guest.me.id });
    guest.wait_for(10, |event| matches!(event, Event::LobbyKicked { .. }).then_some(()));
    assert_eq!(owner.ask(ClientMsg::LobbyReady { ready: true }), Err(Error::LobbyNotFull));
    assert_eq!(presence_of(&env, "gus"), Presence::Idle);

    // Back in, ready; the owner's ready starts the count.
    join(&mut owner, &mut guest, state.id);
    guest.expect_done(ClientMsg::LobbyReady { ready: true });
    let started = Instant::now();
    owner.expect_done(ClientMsg::LobbyReady { ready: true });
    let in_ms = owner.wait_for(10, |event| match event {
        Event::LobbyStarting { in_ms } => Some(in_ms),
        _ => None,
    });
    assert_eq!(in_ms, LOBBY_COUNTDOWN_MS);

    // Nobody can cancel it.
    assert_eq!(guest.ask(ClientMsg::LobbyLeave), Err(Error::Starting));
    assert_eq!(guest.ask(ClientMsg::LobbyReady { ready: false }), Err(Error::Starting));
    assert_eq!(owner.ask(ClientMsg::LobbyLeave), Err(Error::Starting));
    assert_eq!(owner.ask(ClientMsg::LobbyKick { account: guest.me.id }), Err(Error::Starting));
    let handoff = guest.wait_match_found();
    assert!(started.elapsed() >= Duration::from_millis(4500), "{:?}", started.elapsed());
    assert!(!handoff.rated);
    let owner_handoff = owner.wait_match_found();
    assert_eq!((owner_handoff.slot.0, handoff.slot.0), (0, 1));

    let games = [play(owner_handoff, env.tls.clone()), play(handoff, env.tls.clone())];
    for game in games {
        assert_eq!(game.join().expect("game"), Some(Team::Radiant));
    }
    let result = owner.wait_result();
    assert_eq!((result.rated, result.outcome), (false, Outcome::Radiant));
    assert!(result.seats.iter().all(|s| s.before.is_none() && s.after.is_none()));
    let Ok(Reply::Account { account }) = owner.ask(ClientMsg::Account { account: owner.me.id }) else {
        panic!("account");
    };
    assert!(account.ratings.is_empty(), "lobbies are unrated");
}

#[test]
fn a_member_who_drops_during_the_countdown_is_still_in_the_match() {
    let env = start(rules());
    let mut owner = env.account("olga", AccountKind::Human);
    let mut guest = env.bot("gus");
    let state = create(&mut owner, Audience::Everyone);
    join(&mut owner, &mut guest, state.id);
    guest.expect_done(ClientMsg::LobbyReady { ready: true });
    owner.expect_done(ClientMsg::LobbyReady { ready: true });
    owner.wait_for(10, |event| matches!(event, Event::LobbyStarting { .. }).then_some(()));
    drop(owner);

    let handoff = guest.wait_match_found();
    assert_eq!(handoff.seats.len(), 2);
    // The owner comes back later and is told where its match is.
    let logged = env.connect().login("olga", PASSWORD, ClientInfo::default()).expect("login");
    assert_eq!(logged.presence, Presence::InMatch { match_id: handoff.match_id });
}

#[test]
fn the_owner_leaving_closes_the_lobby_and_audiences_are_kept() {
    let env = start(rules());
    let mut owner = env.account("olga", AccountKind::Human);
    let mut bot = env.bot("gus");
    let mut human = env.account("hugo", AccountKind::Human);
    let mut asker = env.account("anna", AccountKind::Human);

    let bots_only = ClientMsg::LobbyCreate {
        ruleset: RulesetId("fun".to_owned()),
        name: "bots".to_owned(),
        audience: Audience::BotsOnly,
    };
    assert_eq!(owner.ask(bots_only), Err(Error::AudienceMismatch));
    assert_eq!(
        owner.ask(ClientMsg::LobbyCreate { ruleset: RulesetId("nope".to_owned()), name: "x".to_owned(), audience: Audience::Everyone }),
        Err(Error::UnknownRuleset)
    );
    assert_eq!(
        owner.ask(ClientMsg::LobbyCreate { ruleset: RulesetId("fun".to_owned()), name: String::new(), audience: Audience::Everyone }),
        Err(Error::BadLobbyName)
    );

    let state = create(&mut owner, Audience::HumansOnly);
    assert_eq!(bot.ask(ClientMsg::LobbyAsk { lobby: state.id }), Err(Error::AudienceMismatch));
    join(&mut owner, &mut human, state.id);
    // Full: a further request is refused; a pending one cannot exist, so ask before it fills elsewhere.
    assert_eq!(asker.ask(ClientMsg::LobbyAsk { lobby: state.id }), Err(Error::LobbyFull));

    owner.expect_done(ClientMsg::LobbyLeave);
    let closed = human.wait_for(10, |event| match event {
        Event::LobbyClosed { lobby } => Some(lobby),
        _ => None,
    });
    assert_eq!(closed, state.id);
    assert_eq!(human.ask(ClientMsg::LobbyLeave), Err(Error::NotInLobby));
    let Ok(Reply::Lobbies { list }) = asker.ask(ClientMsg::LobbyList) else { panic!("list") };
    assert!(list.is_empty());

    // A pending asker is denied when the owner goes; a vanished owner closes it too.
    let state = create(&mut owner, Audience::Everyone);
    asker.expect_done(ClientMsg::LobbyAsk { lobby: state.id });
    asker.expect_done(ClientMsg::LobbyAskCancel);
    assert_eq!(asker.ask(ClientMsg::LobbyAskCancel), Err(Error::NoPendingRequest));
    asker.expect_done(ClientMsg::LobbyAsk { lobby: state.id });
    drop(owner);
    asker.wait_for(10, |event| matches!(event, Event::LobbyDenied { .. }).then_some(()));
    assert_eq!(presence_of(&env, "olga"), Presence::Idle);
}
