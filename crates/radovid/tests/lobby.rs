//! Custom games: names and passwords, join requests, kicks, readiness, the
//! countdown nobody can cancel, the lobby closing with its owner, and several
//! lobbies per account.

mod common;

use std::time::{Duration, Instant};

use common::*;
use radovid_client::proto::*;

fn rules() -> Vec<radovid::config::RulesetConfig> {
    vec![
        ruleset("solo", true, "team1", "0.3"),
        ruleset("fun", false, "team0", "0.3"),
    ]
}

fn fun() -> RulesetId {
    RulesetId("fun".to_owned())
}

fn create(owner: &mut Account, name: &str, password: Option<&str>) -> LobbyState {
    match owner.ask(ClientMsg::LobbyCreate {
        ruleset: fun(),
        name: name.to_owned(),
        audience: Audience::Everyone,
        password: password.map(Secret::new),
    }) {
        Ok(Reply::Lobby { state }) => state,
        other => panic!("lobby: {other:?}"),
    }
}

/// `member` asks into `lobby`; the owner lets it in.
fn join(owner: &mut Account, member: &mut Account, lobby: LobbyId) {
    member.expect_done(ClientMsg::LobbyAsk {
        lobby,
        password: None,
    });
    owner.wait_for(10, |event| match event {
        Event::LobbyState { state } if state.pending.iter().any(|m| m.account == member.me.id) => {
            Some(())
        }
        _ => None,
    });
    owner.expect_done(ClientMsg::LobbyDecide {
        lobby,
        account: member.me.id,
        allow: true,
    });
    member.wait_for(10, |event| {
        matches!(event, Event::LobbyJoined { .. }).then_some(())
    });
}

#[test]
fn a_lobby_fills_kicks_counts_down_and_starts_an_unrated_match() {
    let env = start(rules());
    let mut owner = env.account("olga", AccountKind::Human);
    let mut guest = env.bot("gus");
    let state = create(&mut owner, "casual game", None);
    let lobby = state.id;
    assert_eq!(
        (
            state.members.len(),
            state.members[0].account,
            state.rated,
            state.locked
        ),
        (1, owner.me.id, false, false)
    );
    assert_eq!((state.members[0].slot, state.members[0].team), (0, 0));

    // Listed while it has a free seat; a request waits for the owner.
    let Ok(Reply::Lobbies { list }) = guest.ask(ClientMsg::LobbyList) else {
        panic!("list")
    };
    assert_eq!(
        (
            list.len(),
            list[0].owner_nickname.as_str(),
            list[0].members,
            list[0].seats
        ),
        (1, "olga", 1, 2)
    );
    assert_eq!(
        guest.ask(ClientMsg::LobbyAsk {
            lobby: LobbyId(999),
            password: None
        }),
        Err(Error::UnknownLobby)
    );
    join(&mut owner, &mut guest, lobby);
    let joined = owner.wait_for(10, |event| match event {
        Event::LobbyState { state } if state.members.len() == 2 => Some(state),
        _ => None,
    });
    assert_eq!(
        (
            joined.members[1].nickname.as_str(),
            joined.members[1].team,
            joined.members[1].bot_version.as_deref()
        ),
        ("gus", 1, Some("test-1"))
    );
    assert_eq!(
        guest.ask(ClientMsg::LobbyDecide {
            lobby,
            account: owner.me.id,
            allow: true
        }),
        Err(Error::NotLobbyOwner)
    );
    // A lobby is one game in flight among others: the guest may still queue.
    let entry = guest.queue(&solo());
    guest.expect_done(ClientMsg::QueueLeave { entry: entry.entry });

    // The owner cannot start before everyone is ready; a kick frees the seat.
    assert_eq!(
        owner.ask(ClientMsg::LobbyReady { lobby, ready: true }),
        Err(Error::NotAllReady)
    );
    assert_eq!(
        owner.ask(ClientMsg::LobbyKick {
            lobby,
            account: owner.me.id
        }),
        Err(Error::NoSuchMember)
    );
    owner.expect_done(ClientMsg::LobbyKick {
        lobby,
        account: guest.me.id,
    });
    guest.wait_for(10, |event| {
        matches!(event, Event::LobbyKicked { .. }).then_some(())
    });
    assert_eq!(
        owner.ask(ClientMsg::LobbyReady { lobby, ready: true }),
        Err(Error::LobbyNotFull)
    );
    assert_eq!(
        guest.ask(ClientMsg::LobbyLeave { lobby }),
        Err(Error::NotInLobby { lobby })
    );

    // Back in, ready; the owner's ready starts the count.
    join(&mut owner, &mut guest, lobby);
    guest.expect_done(ClientMsg::LobbyReady { lobby, ready: true });
    let started = Instant::now();
    owner.expect_done(ClientMsg::LobbyReady { lobby, ready: true });
    let in_ms = owner.wait_for(10, |event| match event {
        Event::LobbyStarting { lobby: l, in_ms } if l == lobby => Some(in_ms),
        _ => None,
    });
    assert_eq!(in_ms, LOBBY_COUNTDOWN_MS);

    // Nobody can cancel it.
    assert_eq!(
        guest.ask(ClientMsg::LobbyLeave { lobby }),
        Err(Error::Starting)
    );
    assert_eq!(
        guest.ask(ClientMsg::LobbyReady {
            lobby,
            ready: false
        }),
        Err(Error::Starting)
    );
    assert_eq!(
        owner.ask(ClientMsg::LobbyLeave { lobby }),
        Err(Error::Starting)
    );
    let handoff = guest.wait_match_found();
    assert!(
        started.elapsed() >= Duration::from_millis(4500),
        "{:?}",
        started.elapsed()
    );
    assert!(!handoff.rated);
    let owner_handoff = owner.wait_match_found();
    assert_eq!((owner_handoff.slot, handoff.slot), (0, 1));

    let result = owner.wait_result();
    assert_eq!(
        (result.rated, result.outcome),
        (false, Outcome::Winner { team: 0 })
    );
    assert!(
        result
            .seats
            .iter()
            .all(|s| s.before.is_none() && s.after.is_none())
    );
    let Ok(Reply::Account { account }) = owner.ask(ClientMsg::Account {
        account: owner.me.id,
    }) else {
        panic!("account");
    };
    assert!(account.ratings.is_empty(), "lobbies are unrated");
}

#[test]
fn a_password_lets_in_at_once_and_names_are_unique() {
    let env = start(rules());
    let mut owner = env.account("olga", AccountKind::Human);
    let mut friend = env.account("fritz", AccountKind::Human);
    let mut stranger = env.account("sam", AccountKind::Human);
    let state = create(&mut owner, "Friday Night", Some("open sesame"));
    assert!(state.locked);
    let Ok(Reply::Lobbies { list }) = stranger.ask(ClientMsg::LobbyList) else {
        panic!("list")
    };
    assert!(list[0].locked);
    let taken = ClientMsg::LobbyCreate {
        ruleset: fun(),
        name: "friday night".to_owned(),
        audience: Audience::Everyone,
        password: None,
    };
    assert_eq!(friend.ask(taken), Err(Error::LobbyNameTaken));

    assert_eq!(
        stranger.ask(ClientMsg::LobbyJoinByName {
            name: "Friday Night".to_owned(),
            password: Some(Secret::new("guess"))
        }),
        Err(Error::BadLobbyPassword)
    );
    let Ok(Reply::Lobby { state }) = friend.ask(ClientMsg::LobbyJoinByName {
        name: "Friday Night".to_owned(),
        password: Some(Secret::new("open sesame")),
    }) else {
        panic!("join by name");
    };
    assert_eq!(state.members[1].account, friend.me.id);
    let seen = owner.wait_for(10, |event| match event {
        Event::LobbyState { state } => Some(state.members.len()),
        _ => None,
    });
    assert_eq!(seen, 2);
    // Without the password, a request is the owner's to decide, as anywhere.
    assert_eq!(
        stranger.ask(ClientMsg::LobbyAsk {
            lobby: state.id,
            password: None
        }),
        Err(Error::LobbyFull)
    );
}

#[test]
fn an_account_sits_in_several_lobbies_up_to_its_limit() {
    let env = start(rules());
    let mut owner = env.account("olga", AccountKind::Human);
    let first = create(&mut owner, "first", None);
    let second = create(&mut owner, "second", None);
    assert_ne!(first.id, second.id);
    let third = ClientMsg::LobbyCreate {
        ruleset: fun(),
        name: "third".to_owned(),
        audience: Audience::Everyone,
        password: None,
    };
    assert_eq!(
        owner.ask(third.clone()),
        Err(Error::TooManyGames { limit: 2 })
    );
    owner.expect_done(ClientMsg::LobbyLeave { lobby: second.id });
    assert!(matches!(owner.ask(third), Ok(Reply::Lobby { .. })));
    let mut back = env.connect();
    let logged = back
        .login("olga", PASSWORD, ClientInfo::default())
        .expect("login");
    assert_eq!(logged.activities.lobbies.len(), 2);
}

#[test]
fn a_member_who_drops_during_the_countdown_is_still_in_the_match() {
    let env = start(rules());
    let mut owner = env.account("olga", AccountKind::Human);
    let mut guest = env.bot("gus");
    let lobby = create(&mut owner, "casual game", None).id;
    join(&mut owner, &mut guest, lobby);
    guest.expect_done(ClientMsg::LobbyReady { lobby, ready: true });
    owner.expect_done(ClientMsg::LobbyReady { lobby, ready: true });
    owner.wait_for(10, |event| {
        matches!(event, Event::LobbyStarting { .. }).then_some(())
    });
    drop(owner);

    let handoff = guest.wait_match_found();
    assert_eq!(handoff.seats.len(), 2);
    // The owner comes back later and is told where its match is.
    let logged = env
        .connect()
        .login("olga", PASSWORD, ClientInfo::default())
        .expect("login");
    assert_eq!(logged.activities.matches, vec![handoff.match_id]);
}

#[test]
fn the_owner_leaving_closes_the_lobby_and_audiences_are_kept() {
    let env = start(rules());
    let mut owner = env.account("olga", AccountKind::Human);
    let mut bot = env.bot("gus");
    let mut human = env.account("hugo", AccountKind::Human);
    let mut asker = env.account("anna", AccountKind::Human);

    let bots_only = ClientMsg::LobbyCreate {
        ruleset: fun(),
        name: "bots".to_owned(),
        audience: Audience::BotsOnly,
        password: None,
    };
    assert_eq!(owner.ask(bots_only), Err(Error::AudienceMismatch));
    assert_eq!(
        owner.ask(ClientMsg::LobbyCreate {
            ruleset: RulesetId("nope".to_owned()),
            name: "x".to_owned(),
            audience: Audience::Everyone,
            password: None,
        }),
        Err(Error::UnknownRuleset)
    );
    assert_eq!(
        owner.ask(ClientMsg::LobbyCreate {
            ruleset: fun(),
            name: String::new(),
            audience: Audience::Everyone,
            password: None,
        }),
        Err(Error::BadLobbyName)
    );

    let state = match owner.ask(ClientMsg::LobbyCreate {
        ruleset: fun(),
        name: "humans".to_owned(),
        audience: Audience::HumansOnly,
        password: None,
    }) {
        Ok(Reply::Lobby { state }) => state,
        other => panic!("{other:?}"),
    };
    assert_eq!(
        bot.ask(ClientMsg::LobbyAsk {
            lobby: state.id,
            password: None
        }),
        Err(Error::AudienceMismatch)
    );
    join(&mut owner, &mut human, state.id);
    assert_eq!(
        asker.ask(ClientMsg::LobbyAsk {
            lobby: state.id,
            password: None
        }),
        Err(Error::LobbyFull)
    );

    owner.expect_done(ClientMsg::LobbyLeave { lobby: state.id });
    let closed = human.wait_for(10, |event| match event {
        Event::LobbyClosed { lobby } => Some(lobby),
        _ => None,
    });
    assert_eq!(closed, state.id);
    let Ok(Reply::Lobbies { list }) = asker.ask(ClientMsg::LobbyList) else {
        panic!("list")
    };
    assert!(list.is_empty());

    // A pending asker is denied when the owner goes; a vanished owner closes it too.
    let state = create(&mut owner, "again", None);
    let ask = ClientMsg::LobbyAsk {
        lobby: state.id,
        password: None,
    };
    asker.expect_done(ask.clone());
    asker.expect_done(ClientMsg::LobbyAskCancel { lobby: state.id });
    assert_eq!(
        asker.ask(ClientMsg::LobbyAskCancel { lobby: state.id }),
        Err(Error::NoPendingRequest)
    );
    asker.expect_done(ask);
    drop(owner);
    asker.wait_for(10, |event| {
        matches!(event, Event::LobbyDenied { .. }).then_some(())
    });
    let logged = env
        .connect()
        .login("olga", PASSWORD, ClientInfo::default())
        .expect("login");
    assert_eq!(logged.activities, Activities::default());
}
