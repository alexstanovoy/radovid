//! Custom games: names, search and passwords, joining at once, kicks,
//! readiness, the host-controlled countdown, the lobby closing with its
//! owner, and several lobbies per account.

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

/// `member` sits in the open `lobby` at once.
fn join(member: &mut Account, lobby: LobbyId) {
    let state = match member.ask(ClientMsg::LobbyJoin {
        lobby,
        password: None,
    }) {
        Ok(Reply::Lobby { state }) => state,
        other => panic!("join: {other:?}"),
    };
    assert!(state.members.iter().any(|m| m.account == member.me.id));
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

    // Listed while it has a free seat; whoever may sit is in at once.
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
        guest.ask(ClientMsg::LobbyJoin {
            lobby: LobbyId(999),
            password: None
        }),
        Err(Error::UnknownLobby)
    );
    join(&mut guest, lobby);
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
        guest.ask(ClientMsg::LobbyKick {
            lobby,
            account: owner.me.id
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
    join(&mut guest, lobby);
    guest.expect_done(ClientMsg::LobbyReady { lobby, ready: true });
    let started = Instant::now();
    owner.expect_done(ClientMsg::LobbyReady { lobby, ready: true });
    let in_ms = owner.wait_for(10, |event| match event {
        Event::LobbyStarting { lobby: l, in_ms } if l == lobby => Some(in_ms),
        _ => None,
    });
    assert_eq!(in_ms, LOBBY_COUNTDOWN_MS);

    // Guests cannot cancel it, and leaving is blocked until the start ends.
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
    let lobby = state.id;
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

    // A locked lobby takes its password and nothing else: no request for the owner to decide.
    assert_eq!(
        stranger.ask(ClientMsg::LobbyJoin {
            lobby,
            password: None
        }),
        Err(Error::LobbyPasswordRequired)
    );
    assert_eq!(
        stranger.ask(ClientMsg::LobbyJoin {
            lobby,
            password: Some(Secret::new("guess"))
        }),
        Err(Error::BadLobbyPassword)
    );
    let Ok(Reply::Lobby { state }) = friend.ask(ClientMsg::LobbyJoin {
        lobby,
        password: Some(Secret::new("open sesame")),
    }) else {
        panic!("join with the password");
    };
    assert_eq!(state.members[1].account, friend.me.id);
    let seen = owner.wait_for(10, |event| match event {
        Event::LobbyState { state } => Some(state.members.len()),
        _ => None,
    });
    assert_eq!(seen, 2);
    assert_eq!(
        stranger.ask(ClientMsg::LobbyJoin {
            lobby,
            password: Some(Secret::new("open sesame"))
        }),
        Err(Error::LobbyFull)
    );
}

/// The owner's password is for the owner to give: nothing the service tells a member or anyone
/// else holds it, before or after the owner changes it, and no retired join request is listed.
#[test]
fn no_member_or_stranger_can_read_the_lobby_password() {
    let env = start(rules());
    let mut owner = env.account("olga", AccountKind::Human);
    let mut friend = env.account("fritz", AccountKind::Human);
    let mut stranger = env.account("sam", AccountKind::Human);
    let lobby = create(&mut owner, "Friday Night", Some("open sesame")).id;
    let mut told: Vec<Vec<u8>> = Vec::new();
    let encoded = |state: &LobbyState| encode_frame_to_vec(state).expect("encoded");
    let Ok(Reply::Lobby { state }) = friend.ask(ClientMsg::LobbyJoin {
        lobby,
        password: Some(Secret::new("open sesame")),
    }) else {
        panic!("join with the password");
    };
    assert!(state.locked && state.retired_pending.is_empty());
    told.push(encoded(&state));
    owner.expect_done(ClientMsg::LobbyConfigure {
        lobby,
        ruleset: fun(),
        audience: Audience::Everyone,
        password: Some(Secret::new("new sesame")),
    });
    let state = state_until(&mut friend, lobby, |s| s.locked && s.members.len() == 2);
    told.push(encoded(&state));
    for asker in [&mut friend, &mut stranger] {
        for msg in [
            ClientMsg::LobbyList,
            ClientMsg::LobbySearch {
                query: "sesame".to_owned(),
            },
        ] {
            let Ok(Reply::Lobbies { list }) = asker.ask(msg) else {
                panic!("list")
            };
            told.push(encode_frame_to_vec(&list).expect("encoded"));
        }
    }
    for secret in [&b"open sesame"[..], b"new sesame"] {
        assert!(
            told.iter()
                .all(|bytes| !bytes.windows(secret.len()).any(|w| w == secret)),
            "a lobby's password reached someone who is not its owner"
        );
    }
}

/// The service names the ruleset a new lobby is of: its `lobby_ruleset`, else its first, and no
/// other; a `lobby_ruleset` that names no ruleset is refused at start.
#[test]
fn the_lobby_ruleset_is_the_default_of_a_new_lobby() {
    let defaults = |env: &Env| match env.human("lena").ask(ClientMsg::Rulesets) {
        Ok(Reply::Rulesets { list }) => list
            .into_iter()
            .filter(|r| r.lobby_default)
            .map(|r| r.id.0)
            .collect::<Vec<_>>(),
        other => panic!("rulesets: {other:?}"),
    };
    assert_eq!(
        defaults(&start(rules())),
        ["solo"],
        "the first unless named"
    );
    let named = start_configured(rules(), |c| c.lobby_ruleset = Some("fun".to_owned()));
    assert_eq!(defaults(&named), ["fun"]);
    let refused = try_start(rules(), |c| c.lobby_ruleset = Some("nowhere".to_owned()));
    assert!(
        refused.is_err_and(|e| e.contains("lobby_ruleset \"nowhere\" names no [[ruleset]]")),
        "a lobby_ruleset of no ruleset is refused"
    );
}

/// A search lists the open lobbies whose name or owner holds the text, case folded in any script;
/// an empty one lists them all, and one no name could hold (too long, a control character) is
/// refused.
#[test]
fn a_search_finds_lobbies_by_name_and_owner() {
    let env = start(rules());
    let mut olga = env.account("olga", AccountKind::Human);
    let mut fritz = env.account("fritz", AccountKind::Human);
    let mut seeker = env.account("sam", AccountKind::Human);
    create(&mut olga, "Friday Night", Some("open sesame"));
    create(&mut fritz, "Practice", None);
    create(&mut seeker, "ΟΔΟΣ Straße Ночь", None);
    let mut search = |query: &str| match seeker.ask(ClientMsg::LobbySearch {
        query: query.to_owned(),
    }) {
        Ok(Reply::Lobbies { list }) => list.into_iter().map(|l| l.name).collect::<Vec<_>>(),
        other => panic!("search {query:?}: {other:?}"),
    };
    assert_eq!(search("FRIDAY"), ["Friday Night"], "by name, case ignored");
    assert_eq!(search("frit"), ["Practice"], "by the owner's nickname");
    for query in ["НОЧЬ", "STRASSE", "οδος"] {
        assert_eq!(
            search(query),
            ["ΟΔΟΣ Straße Ночь"],
            "{query}: any script's case"
        );
    }
    assert_eq!(
        search(""),
        ["ΟΔΟΣ Straße Ночь", "Practice", "Friday Night"],
        "all, newest first"
    );
    assert!(search("saturday").is_empty());
    assert!(
        search(&"я".repeat(MAX_LOBBY_NAME_LEN)).is_empty(),
        "as long as a name may be, in characters"
    );
    for refused in ["x".repeat(MAX_LOBBY_NAME_LEN + 1), "fri\u{7}day".to_owned()] {
        assert_eq!(
            seeker.ask(ClientMsg::LobbySearch { query: refused }),
            Err(Error::BadLobbyName),
            "nothing a lobby name may not hold"
        );
    }
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
    join(&mut guest, lobby);
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
        bot.ask(ClientMsg::LobbyJoin {
            lobby: state.id,
            password: None
        }),
        Err(Error::AudienceMismatch)
    );
    join(&mut human, state.id);
    assert_eq!(
        asker.ask(ClientMsg::LobbyJoin {
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

    // A vanished owner closes it too.
    let state = create(&mut owner, "again", None);
    join(&mut asker, state.id);
    drop(owner);
    let closed = asker.wait_for(10, |event| match event {
        Event::LobbyClosed { lobby } => Some(lobby),
        _ => None,
    });
    assert_eq!(closed, state.id);
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

/// A ruleset that starts for one account alone: StartSolo makes the match at once, with no lobby
/// and no countdown, and the one seat held; a ruleset that does not start so refuses it.
#[test]
fn an_instant_solo_ruleset_starts_for_one_account_at_once() {
    let mut demo = ruleset("demo", false, "team0", "0.3");
    demo.instant_solo = true;
    let env = start(vec![demo, ruleset("fun", false, "team0", "0.3")]);
    let mut player = env.human("olga");
    assert_eq!(
        player.ask(ClientMsg::StartSolo { ruleset: fun() }),
        Err(Error::NoSoloStart)
    );
    player.expect_done(ClientMsg::StartSolo {
        ruleset: RulesetId("demo".to_owned()),
    });
    let handoff = player.wait_match_found();
    assert_eq!((handoff.slot, handoff.seats.len()), (0, 1));
    let result = player.wait_result();
    assert!(!result.rated, "a solo start is unrated");
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
    join(&mut guest, lobby);
    join(&mut watcher, lobby);

    // Slots 0 to 4 belong to side 0, and 5 to 9 to side 1: the guest, who came
    // to side 1's first slot, goes to its second.
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
    join(&mut guest, lobby);
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

#[test]
fn lobby_ruleset_changes_preserve_members_game_version_and_state() {
    let mut foreign = ruleset("foreign", false, "team0", "0.3");
    foreign.game_fingerprint = FINGERPRINT ^ 1;
    foreign.launcher.version_probe[1] = format!("{:016x}", foreign.game_fingerprint);
    let env = start(vec![
        ruleset("fun", false, "team0", "0.3"),
        ruleset("compatible", false, "team0", "0.3"),
        foreign,
    ]);
    let (mut owner, mut guest) = (env.human("host"), env.bot("guest"));
    let lobby = create(&mut owner, "versioned lobby", None).id;
    join(&mut guest, lobby);
    guest.expect_done(ClientMsg::LobbyReady { lobby, ready: true });
    let configure = |ruleset: &str| ClientMsg::LobbyConfigure {
        lobby,
        ruleset: RulesetId(ruleset.to_owned()),
        audience: Audience::Everyone,
        password: Some(Secret::new("replacement password")),
    };
    assert_eq!(
        owner.ask(configure("foreign")),
        Err(Error::GameVersionMismatch {
            server: FINGERPRINT ^ 1
        })
    );
    let Ok(Reply::Lobby { state }) = owner.ask(ClientMsg::LobbyJoin {
        lobby,
        password: None,
    }) else {
        panic!("the existing lobby can still be read");
    };
    assert_eq!(
        state.ruleset,
        fun(),
        "a refusal leaves the ruleset unchanged"
    );
    assert!(!state.locked, "a refusal leaves the password unchanged");
    assert_eq!(state.members.len(), 2, "both members remain seated");
    assert!(
        state
            .members
            .iter()
            .find(|m| m.account == guest.me.id)
            .expect("the guest")
            .ready,
        "a refusal preserves readiness"
    );
    owner.expect_done(configure("compatible"));
    let state = state_until(&mut guest, lobby, |s| s.ruleset.0 == "compatible");
    assert!(
        state.locked,
        "a compatible mode change applies the settings"
    );
    assert!(
        state.members.iter().all(|m| !m.ready),
        "a successful change unreadies members"
    );
}

/// Every lobby has the service's ten slots, whatever its ruleset: a ruleset chosen later changes
/// the rules and nothing else. A ruleset whose own games are one a side (the duel) still starts
/// with as many on a side as are seated there: a mode caps nobody.
#[test]
fn a_lobby_keeps_its_ten_slots_under_any_ruleset() {
    let mut full = ruleset("full", false, "team0", "0.3");
    full.teams = vec![5, 5];
    let duel = ruleset("duel", false, "team0", "0.3");
    let env = start(vec![full, duel]);
    let (mut owner, mut guest) = (env.human("olga"), env.bot("gus"));

    let state = lobby_in(&mut owner, "full", "ten");
    let lobby = state.id;
    assert_eq!(state.teams, [5, 5], "the default lobby is five a side");
    join(&mut guest, lobby);
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

    // Two on one team, more than the duel's own games seat: it starts all the same, and nothing
    // was added to fill the other slots.
    guest.expect_done(ClientMsg::LobbySlot {
        lobby,
        account: None,
        slot: 1,
    });
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

/// A lobby's owner turns the game's cheats on for a ruleset that has them (`cheats_args`): every
/// member sees it and is unreadied, and the match is started with them. Nobody else can turn
/// them on, a ruleset without them refuses, a change to such a ruleset turns them off, and a
/// queued match of the very ruleset never has them.
#[test]
fn a_lobby_owner_turns_cheats_on_and_only_that_lobby_has_them() {
    let mut cheaty = ruleset("cheaty", true, "team0", "0.3");
    cheaty.launcher.cheats_args = vec!["--cheats".to_owned()];
    let plain = ruleset("plain", false, "team0", "0.3");
    let env = start(vec![cheaty, plain]);
    let (mut owner, mut guest) = (env.human("olga"), env.human("gus"));
    let lobby = lobby_in(&mut owner, "cheaty", "sandbox");
    assert_eq!(
        lobby.cheats,
        Some(false),
        "a ruleset with cheats, off at first"
    );
    let lobby = lobby.id;
    join(&mut guest, lobby);
    let cheats = |on| ClientMsg::LobbyCheats { lobby, on };
    assert_eq!(guest.ask(cheats(true)), Err(Error::NotLobbyOwner));
    guest.expect_done(ClientMsg::LobbyReady { lobby, ready: true });
    owner.expect_done(cheats(true));
    let seen = state_until(&mut guest, lobby, |s| s.cheats == Some(true));
    assert!(
        seen.members.iter().all(|m| !m.ready),
        "cheats unready everyone"
    );

    // Rules without cheats turn them off; they refuse to have them.
    let configure = |ruleset: &str| ClientMsg::LobbyConfigure {
        lobby,
        ruleset: RulesetId(ruleset.to_owned()),
        audience: Audience::Everyone,
        password: None,
    };
    owner.expect_done(configure("plain"));
    state_until(&mut guest, lobby, |s| {
        s.ruleset.0 == "plain" && s.cheats.is_none()
    });
    assert_eq!(owner.ask(cheats(true)), Err(Error::NoCheats));
    owner.expect_done(configure("cheaty"));
    let back = state_until(&mut guest, lobby, |s| s.ruleset.0 == "cheaty");
    assert_eq!(back.cheats, Some(false), "asked for again");

    owner.expect_done(cheats(true));
    guest.expect_done(ClientMsg::LobbyReady { lobby, ready: true });
    owner.expect_done(ClientMsg::LobbyReady { lobby, ready: true });
    owner.wait_match_found();
    let result = owner.wait_result();
    assert!(!result.rated);
    assert_eq!(started_args(&result.game_report), "--cheats");

    // The same ruleset queued: rated, and never with cheats.
    let (mut a, mut b) = (env.bot("ann"), env.bot("bob"));
    a.queue(&RulesetId("cheaty".to_owned()));
    b.queue(&RulesetId("cheaty".to_owned()));
    a.accept();
    b.accept();
    let result = a.wait_result();
    assert!(result.rated);
    assert_eq!(
        started_args(&result.game_report),
        "",
        "a queued match has no cheats"
    );
}

/// An argument of `cheats_args` that also stands in what every match of the ruleset is started
/// with (its `args`, `env`, the artifacts' arguments and environment) would reach queued, rated
/// matches too: the service refuses to start with it, naming where it is.
#[test]
fn a_cheats_argument_that_every_match_gets_is_refused_at_start() {
    let refused = |change: fn(&mut radovid::config::RulesetConfig)| {
        let mut rules = ruleset("cheaty", true, "team0", "0.3");
        rules.launcher.cheats_args = vec!["--lobby-cheats".to_owned()];
        change(&mut rules);
        match try_start(vec![rules], |_| {}) {
            Ok(_) => panic!("a ruleset sharing its cheats was taken"),
            Err(error) => error,
        }
    };
    let shared = |error: &str, place: &str| {
        error.contains(&format!(
            "ruleset cheaty: cheats_args \"--lobby-cheats\" is also in {place}, where it would \
             reach every match"
        ))
    };
    let error = refused(|r| r.launcher.args.push("--lobby-cheats".to_owned()));
    assert!(shared(&error, "args"), "{error}");
    let error = refused(|r| {
        r.launcher
            .env
            .insert("GAME_FLAGS".to_owned(), "-x --lobby-cheats".to_owned());
    });
    assert!(shared(&error, "env GAME_FLAGS"), "{error}");
    let error = refused(|r| {
        r.launcher.artifact = Some("/tmp/radovid-never/replay-{match_id}".to_owned());
        r.launcher.artifact_args = vec!["--lobby-cheats".to_owned()];
    });
    assert!(shared(&error, "artifact_args"), "{error}");
    let error = refused(|r| r.launcher.cheats_args.push(" ".to_owned()));
    assert!(
        error.contains("cheats_args holds an empty argument"),
        "{error}"
    );
    // Kept apart, the same arguments start.
    let mut rules = ruleset("cheaty", true, "team0", "0.3");
    rules.launcher.cheats_args = vec!["--lobby-cheats".to_owned()];
    assert!(try_start(vec![rules], |_| {}).is_ok());
}

/// A cheats flag is compared whole, with the values that follow it: a value of its own may stand
/// anywhere, a flag that merely begins like it is another flag, and the same flag with another
/// value is another setting. Each of these rulesets starts; the same flag and value in an env
/// value does not.
#[test]
fn a_cheats_argument_is_compared_whole_and_its_values_are_free() {
    let start = |cheats: &[&str], args: &[&str], env: Option<&str>| {
        let mut rules = ruleset("cheaty", true, "team0", "0.3");
        rules.launcher.cheats_args = cheats.iter().map(|a| (*a).to_owned()).collect();
        rules
            .launcher
            .args
            .extend(args.iter().map(|a| (*a).to_owned()));
        if let Some(value) = env {
            rules
                .launcher
                .env
                .insert("GAME_FLAGS".to_owned(), value.to_owned());
        }
        try_start(vec![rules], |_| {}).map(drop)
    };
    assert_eq!(
        start(&["--cheat-level", "1"], &["--players", "10"], None),
        Ok(())
    );
    assert_eq!(
        start(&["--cheat-level", "1"], &["--level", "1"], None),
        Ok(()),
        "a value is no marker"
    );
    assert_eq!(
        start(&["--cheats"], &["--cheats-log"], Some("--cheats-log=1")),
        Ok(())
    );
    assert_eq!(
        start(&["--sandbox"], &["--mode", "ranked"], Some("-v --verbose")),
        Ok(())
    );
    assert_eq!(
        start(&["--mode", "sandbox"], &["--mode", "ranked"], None),
        Ok(()),
        "another value of the flag is another setting"
    );
    assert_eq!(
        start(
            &["--mode", "sandbox"],
            &["--port", "1", "--mode", "sandbox"],
            None
        ),
        Err(
            "ruleset cheaty: cheats_args \"--mode sandbox\" is also in args, where it would reach \
             every match, queued and rated ones too: keep it in cheats_args alone"
                .to_owned()
        ),
        "the flag with its value"
    );
    assert!(
        start(&["--cheats"], &[], Some("-v --cheats"))
            .is_err_and(|e| e.contains("is also in env GAME_FLAGS")),
        "a word of an env value"
    );
    // A flag joined to its value is the flag and its value.
    assert!(
        start(&["--cheats"], &["--cheats=1"], None)
            .is_err_and(|e| e.contains("cheats_args \"--cheats\" is also in args")),
        "--cheats=1 holds --cheats"
    );
    assert!(
        start(&["--mode", "sandbox"], &["--mode=sandbox"], None)
            .is_err_and(|e| e.contains("cheats_args \"--mode sandbox\" is also in args")),
    );
    assert_eq!(start(&["--mode=sandbox"], &["--mode=ranked"], None), Ok(()));
    // A negative number is a value: the marker is the flag with it.
    assert_eq!(
        start(&["--cheat-level", "-1"], &["--cheat-level", "0"], None),
        Ok(())
    );
    // cheats_args begins with the flag that turns cheats on: a lone value marks nothing.
    assert!(
        start(&["on"], &["on"], None).is_err_and(|e| e.contains("cheats_args begins with \"on\"")),
        "a value first is refused"
    );
}

/// The check at start reads placeholders as written: one that expands, for a queued match, to a
/// marker of the ruleset's cheats is caught at launch. The match is not started and ends with
/// nothing (a crash: null, unrated), and the service goes on.
#[test]
fn a_queued_match_that_placeholders_would_give_cheats_is_not_started() {
    let mut cheaty = ruleset("cheaty", true, "team0", "0.3");
    cheaty.launcher.cheats_args = vec!["--cheat-team".to_owned(), "1".to_owned()];
    // Slot 1 plays for team 1: expanded, these are the cheats' own words.
    cheaty
        .launcher
        .args
        .extend(["--cheat-team".to_owned(), "{team_1}".to_owned()]);
    let env = start(vec![cheaty]);
    let (mut a, mut b) = (env.bot("ann"), env.bot("bob"));
    a.queue(&RulesetId("cheaty".to_owned()));
    b.queue(&RulesetId("cheaty".to_owned()));
    a.accept();
    b.accept();
    let result = a.wait_result();
    assert_eq!(
        (result.outcome, result.reason, result.game_report.is_empty()),
        (Outcome::Null, EndReason::Crashed, true),
        "refused at launch: no game ran"
    );
}
