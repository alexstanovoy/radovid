//! Custom games: names and passwords, join requests, kicks, readiness, the
//! countdown nobody can cancel, the lobby closing with its owner, and several
//! lobbies per account.

mod common;

use std::time::{Duration, Instant};

use common::*;
use radovid_client::proto::*;

fn rules() -> Vec<radovid::config::RulesetConfig> {
    let mut fun = ruleset("fun", false, "team0", "0.3");
    fun.lobby_min_players = Some(2);
    vec![ruleset("solo", true, "team1", "0.3"), fun]
}

/// A service whose lobbies have two slots, one for each side, so that they fill up.
fn start_small() -> Env {
    start_configured(rules(), |config| config.lobby_teams = vec![1, 1])
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
        (1, "olga", 1, 10)
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
    let env = start_small();
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
    let env = start_small();
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

/// A lobby starts with the players it has unless its ruleset asks for more: its host starts
/// alone, the match has the one seat held and the others stay empty, nothing fills them.
#[test]
fn a_host_starts_a_lobby_alone_unless_the_ruleset_asks_for_more() {
    let alone = ruleset("alone", false, "team0", "0.3");
    let mut crowd = ruleset("fun", false, "team0", "0.3");
    crowd.lobby_min_players = Some(2);
    let env = start(vec![alone, crowd]);
    let mut owner = env.human("olga");
    let Ok(Reply::Rulesets { list }) = owner.ask(ClientMsg::Rulesets) else {
        panic!("rulesets");
    };
    let minimum: Vec<_> = list
        .iter()
        .map(|r| (r.id.0.as_str(), r.lobby_min_players))
        .collect();
    assert_eq!(minimum, [("alone", 1), ("fun", 2)]);

    let Ok(Reply::Lobby { state }) = owner.ask(ClientMsg::LobbyCreate {
        ruleset: fun(),
        name: "full only".to_owned(),
        audience: Audience::Everyone,
        password: None,
    }) else {
        panic!("lobby");
    };
    assert_eq!(
        owner.ask(ClientMsg::LobbyReady {
            lobby: state.id,
            ready: true
        }),
        Err(Error::LobbyNotFull)
    );
    owner.expect_done(ClientMsg::LobbyLeave { lobby: state.id });

    let Ok(Reply::Lobby { state }) = owner.ask(ClientMsg::LobbyCreate {
        ruleset: RulesetId("alone".to_owned()),
        name: "by myself".to_owned(),
        audience: Audience::Everyone,
        password: None,
    }) else {
        panic!("lobby");
    };
    owner.expect_done(ClientMsg::LobbyReady {
        lobby: state.id,
        ready: true,
    });
    let handoff = owner.wait_match_found();
    assert_eq!(handoff.slot, 0);
    let result = owner.wait_result();
    assert_eq!(result.seats.len(), 1, "one seat held");
}

fn lobby_in(owner: &mut Account, ruleset: &str, name: &str) -> LobbyState {
    match owner.ask(ClientMsg::LobbyCreate {
        ruleset: RulesetId(ruleset.to_owned()),
        name: name.to_owned(),
        audience: Audience::Everyone,
        password: None,
    }) {
        Ok(Reply::Lobby { state }) => state,
        other => panic!("lobby: {other:?}"),
    }
}

/// The first state of `lobby` the account is told that `holds`.
fn state_until(
    account: &mut Account,
    lobby: LobbyId,
    holds: impl Fn(&LobbyState) -> bool,
) -> LobbyState {
    account.wait_for(10, |event| match event {
        Event::LobbyState { state } if state.id == lobby && holds(&state) => Some(state),
        _ => None,
    })
}

/// Members choose their slots: a free one at once, a taken one never, none at all as a spectator;
/// the owner moves the others; a move unreadies; a spectator is left out of the match.
#[test]
fn members_choose_their_slots_and_spectators_stay_out_of_the_match() {
    let mut big = ruleset("big", false, "team0", "0.3");
    big.teams = vec![2, 2];
    big.lobby_min_players = Some(2);
    let env = start(vec![big]);
    let (mut owner, mut guest, mut watcher) =
        (env.human("olga"), env.bot("gus"), env.human("walt"));
    let lobby = lobby_in(&mut owner, "big", "slots").id;
    join(&mut owner, &mut guest, lobby);
    join(&mut owner, &mut watcher, lobby);

    // Slots 0 to 4 are Radiant's, 5 to 9 Dire's, whatever the ruleset: the guest, who came
    // to Dire's first slot, goes to its second.
    guest.expect_done(ClientMsg::LobbySlot {
        lobby,
        account: None,
        slot: 6,
    });
    let state = state_until(&mut owner, lobby, |s| s.members.iter().any(|m| m.slot == 6));
    let at = |state: &LobbyState, who: &Account| {
        state
            .members
            .iter()
            .find(|m| m.account == who.me.id)
            .map(|m| (m.slot, m.team))
            .expect("a member")
    };
    assert_eq!(at(&state, &guest), (6, 1));
    // A taken slot, one the ruleset lacks, and the owner's say over another's.
    assert_eq!(
        watcher.ask(ClientMsg::LobbySlot {
            lobby,
            account: None,
            slot: 6
        }),
        Err(Error::SlotTaken)
    );
    assert_eq!(
        watcher.ask(ClientMsg::LobbySlot {
            lobby,
            account: None,
            slot: 10
        }),
        Err(Error::NoSuchSlot)
    );
    assert_eq!(
        watcher.ask(ClientMsg::LobbySlot {
            lobby,
            account: Some(guest.me.id),
            slot: 1
        }),
        Err(Error::NotLobbyOwner)
    );
    owner.expect_done(ClientMsg::LobbySlot {
        lobby,
        account: Some(guest.me.id),
        slot: 2,
    });
    let seen = state_until(&mut watcher, lobby, |s| {
        s.members
            .iter()
            .any(|m| m.account == guest.me.id && m.slot == 2)
    });
    assert_eq!(at(&seen, &guest), (2, 0));
    // A spectator holds no slot and no team.
    watcher.expect_done(ClientMsg::LobbySlot {
        lobby,
        account: None,
        slot: LOBBY_NO_SLOT,
    });
    let state = state_until(&mut owner, lobby, |s| {
        s.members.iter().any(|m| m.slot == LOBBY_NO_SLOT)
    });
    assert_eq!(at(&state, &watcher), (LOBBY_NO_SLOT, LOBBY_NO_SLOT));

    // Moving unreadies; the owner starts with the seated, the spectator is left out.
    guest.expect_done(ClientMsg::LobbyReady { lobby, ready: true });
    guest.expect_done(ClientMsg::LobbySlot {
        lobby,
        account: None,
        slot: 6,
    });
    state_until(&mut owner, lobby, |s| {
        s.members.iter().any(|m| m.slot == 6) && s.members.iter().all(|m| !m.ready)
    });
    guest.expect_done(ClientMsg::LobbyReady { lobby, ready: true });
    owner.expect_done(ClientMsg::LobbyReady { lobby, ready: true });
    let handoff = guest.wait_match_found();
    assert_eq!(handoff.slot, 1, "the seats are numbered in order");
    watcher.wait_for(10, |event| {
        matches!(event, Event::LobbyClosed { .. }).then_some(())
    });
    let result = owner.wait_result();
    assert_eq!(result.seats.len(), 2, "the spectator played no seat");
    let teams: Vec<u8> = result.seats.iter().map(|s| s.team).collect();
    assert_eq!(teams, [0, 1], "each on the side chosen");
}

/// The owner changes the settings: the ruleset (the rules only: every member keeps the slot),
/// the password and the audience; the others cannot, and everyone is unreadied.
#[test]
fn the_owner_changes_a_lobbys_settings_and_nobody_else() {
    let mut big = ruleset("big", false, "team0", "0.3");
    big.teams = vec![2, 2];
    let env = start(vec![ruleset("fun", false, "team0", "0.3"), big]);
    let (mut owner, mut guest) = (env.human("olga"), env.bot("gus"));
    let lobby = lobby_in(&mut owner, "fun", "settings").id;
    join(&mut owner, &mut guest, lobby);
    let configure = |ruleset: &str, audience, password: Option<&str>| ClientMsg::LobbyConfigure {
        lobby,
        ruleset: RulesetId(ruleset.to_owned()),
        audience,
        password: password.map(Secret::new),
    };
    assert_eq!(
        guest.ask(configure("big", Audience::Everyone, None)),
        Err(Error::NotLobbyOwner)
    );
    guest.expect_done(ClientMsg::LobbyReady { lobby, ready: true });
    assert_eq!(
        owner.ask(configure("nowhere", Audience::Everyone, None)),
        Err(Error::UnknownRuleset)
    );
    assert_eq!(
        owner.ask(configure("big", Audience::BotsOnly, None)),
        Err(Error::AudienceMismatch),
        "the human owner sits in it"
    );
    owner.expect_done(configure("big", Audience::Everyone, Some("secret")));
    let state = state_until(&mut guest, lobby, |s| s.ruleset.0 == "big");
    assert_eq!(
        (state.ruleset.0.as_str(), state.locked, state.members.len()),
        ("big", true, 2)
    );
    assert!(state.members.iter().all(|m| !m.ready), "settings unready");
    // Back to the smaller ruleset: the slots stay, the member keeps hers.
    owner.expect_done(ClientMsg::LobbySlot {
        lobby,
        account: Some(guest.me.id),
        slot: 3,
    });
    owner.expect_done(configure("fun", Audience::Everyone, None));
    let state = state_until(&mut guest, lobby, |s| s.ruleset.0 == "fun");
    assert!(!state.locked);
    assert_eq!(state.teams, [5, 5], "the slots are the service's");
    let theirs = state
        .members
        .iter()
        .find(|m| m.account == guest.me.id)
        .expect("a member");
    assert_eq!(theirs.slot, 3);
}

/// Every lobby has the service's ten slots, whatever its ruleset: a ruleset chosen later changes
/// the rules and nothing else, and one that takes fewer players on a team than are seated there
/// refuses the start (`TeamTooLarge`) where one that takes more starts, with the seated alone.
#[test]
fn a_lobby_keeps_its_ten_slots_under_any_ruleset() {
    let mut full = ruleset("full", false, "team0", "0.3");
    full.teams = vec![5, 5];
    let duel = ruleset("duel", false, "team0", "0.3");
    let mut mid = ruleset("mid", false, "team0", "0.3");
    mid.lobby_max_per_team = Some(5);
    let env = start(vec![full, duel, mid]);
    let (mut owner, mut guest) = (env.human("olga"), env.bot("gus"));

    let state = lobby_in(&mut owner, "full", "ten");
    let lobby = state.id;
    assert_eq!(state.teams, [5, 5], "the default lobby is five a side");
    join(&mut owner, &mut guest, lobby);
    let joined = state_until(&mut owner, lobby, |s| s.members.len() == 2);
    let teams: Vec<u8> = joined.members.iter().map(|m| m.team).collect();
    assert_eq!(teams, [0, 1], "two players meet across the sides");

    // Choosing the duel changes the rules: the slots stay, and so does everybody's.
    let configure = |ruleset: &str| ClientMsg::LobbyConfigure {
        lobby,
        ruleset: RulesetId(ruleset.to_owned()),
        audience: Audience::Everyone,
        password: None,
    };
    owner.expect_done(configure("duel"));
    let state = state_until(&mut guest, lobby, |s| s.ruleset.0 == "duel");
    assert_eq!(state.teams, [5, 5]);
    let slots: Vec<u8> = state.members.iter().map(|m| m.slot).collect();
    assert_eq!(slots, [0, 5]);

    // Two on one team: past what the duel takes. Nothing was added to fill the other slots.
    guest.expect_done(ClientMsg::LobbySlot {
        lobby,
        account: None,
        slot: 1,
    });
    guest.expect_done(ClientMsg::LobbyReady { lobby, ready: true });
    assert_eq!(
        owner.ask(ClientMsg::LobbyReady { lobby, ready: true }),
        Err(Error::TeamTooLarge)
    );
    // A ruleset that plays more a side takes them.
    owner.expect_done(configure("mid"));
    guest.expect_done(ClientMsg::LobbyReady { lobby, ready: true });
    owner.expect_done(ClientMsg::LobbyReady { lobby, ready: true });
    owner.wait_match_found();
    let result = owner.wait_result();
    let teams: Vec<u8> = result.seats.iter().map(|s| s.team).collect();
    assert_eq!(
        teams,
        [0, 0],
        "exactly the two seated, both on the first side"
    );
}
