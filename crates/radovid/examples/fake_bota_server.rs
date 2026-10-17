//! A stand-in for `bota_server` for the end-to-end tests: it speaks just enough
//! of bota's protocol (reserved seats, a referee, a scripted end) to exercise
//! the service quickly and deterministically. It plays no game.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use bota_proto::{
    ClientMsg, FrameReader, HeroId, MapId, MatchInfo, MatchStats, PlayerId, Role, SeatKey,
    ServerMsg, SlotId, SlotStats, Team, TickMode, Vec2, encode_frame_to_vec,
};

struct Seat {
    key: SeatKey,
    conn: Option<(PlayerId, Arc<TcpStream>)>,
    hero: Option<HeroId>,
    ready: bool,
}

struct State {
    seats: Vec<Seat>,
    referee_key: SeatKey,
    others: Vec<(PlayerId, Arc<TcpStream>)>,
    started: bool,
    next_player: u32,
    outcome: String,
    after_ms: u64,
    match_id: u64,
}

fn main() {
    let mut args = std::env::args().skip(1);
    let (mut port, mut outcome, mut after_ms, mut match_id) =
        (0u16, "draw".to_owned(), 300u64, 0u64);
    while let Some(arg) = args.next() {
        let mut value = || args.next().unwrap_or_default();
        match arg.as_str() {
            "--port" => port = value().parse().unwrap_or(0),
            "--outcome" => outcome = value(),
            "--after-ms" => after_ms = value().parse().unwrap_or(300),
            "--match-id" => match_id = value().parse().unwrap_or(0),
            "--wire-fingerprint" => {
                println!(
                    "{}",
                    bota_proto::fingerprint_to_hex(bota_proto::WIRE_FINGERPRINT)
                );
                return;
            }
            _ => {}
        }
    }
    let seats = std::env::var("BOTA_SEATS")
        .unwrap_or_default()
        .split(',')
        .filter(|entry| !entry.is_empty())
        .map(|entry| Seat {
            key: SeatKey::from_hex(entry.split(':').next().unwrap_or(""))
                .expect("a 64-digit seat key"),
            conn: None,
            hero: None,
            ready: false,
        })
        .collect();
    let state = Arc::new(Mutex::new(State {
        seats,
        referee_key: SeatKey::from_hex(&std::env::var("BOTA_REFEREE_KEY").unwrap_or_default())
            .expect("a 64-digit referee key"),
        others: Vec::new(),
        started: false,
        next_player: 1,
        outcome,
        after_ms,
        match_id,
    }));
    let listener = TcpListener::bind(("127.0.0.1", port)).expect("bind");
    println!(
        "bota_server listening on {}",
        listener.local_addr().expect("addr")
    );
    for stream in listener.incoming().flatten() {
        let state = Arc::clone(&state);
        thread::spawn(move || connection(stream, state));
    }
}

fn send(stream: &TcpStream, msg: &ServerMsg) {
    let frame = encode_frame_to_vec(msg).expect("encode");
    let _ = (&*stream).write_all(&frame);
}

fn connection(stream: TcpStream, state: Arc<Mutex<State>>) {
    let stream = Arc::new(stream);
    let mut reader = FrameReader::new();
    let mut buf = [0u8; 4096];
    let mut me: Option<(PlayerId, Option<usize>)> = None;
    loop {
        let n = match (&*stream).read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        reader.push(&buf[..n]);
        loop {
            match reader.next_message::<ClientMsg>() {
                Ok(Some(msg)) => {
                    if !handle(&stream, &state, &mut me, msg) {
                        return;
                    }
                }
                Ok(None) => break,
                Err(_) => return,
            }
        }
    }
    let mut st = state.lock().expect("state");
    if let Some((id, seat)) = me {
        st.others.retain(|(p, _)| *p != id);
        let slot = seat.filter(|&i| st.seats[i].conn.as_ref().is_some_and(|(p, _)| *p == id));
        if let Some(i) = slot {
            st.seats[i].conn = None;
        }
        let left = ServerMsg::ParticipantLeft {
            player_id: id,
            slot: slot.map(|i| SlotId(i as u8)),
        };
        for (_, s) in everyone(&st) {
            send(&s, &left);
        }
    }
}

fn everyone(st: &State) -> Vec<(PlayerId, Arc<TcpStream>)> {
    st.seats
        .iter()
        .filter_map(|s| s.conn.clone())
        .chain(st.others.iter().cloned())
        .collect()
}

/// One message; false ends the connection.
fn handle(
    stream: &Arc<TcpStream>,
    state: &Arc<Mutex<State>>,
    me: &mut Option<(PlayerId, Option<usize>)>,
    msg: ClientMsg,
) -> bool {
    let mut st = state.lock().expect("state");
    match (&*me, msg) {
        (
            None,
            ClientMsg::Hello {
                role,
                key: Some(key),
                ..
            },
        ) => {
            let id = PlayerId(st.next_player);
            st.next_player += 1;
            let seat = st.seats.iter().position(|s| s.key == key);
            let (slot, seat_index) = match (role, seat) {
                (Role::Referee, _) if key == st.referee_key => {
                    st.others.push((id, Arc::clone(stream)));
                    (None, None)
                }
                (Role::Player | Role::Bot, Some(i)) => {
                    // As bota: a seat never changes hands while its connection lives.
                    if st.seats[i].conn.is_some() {
                        send(
                            stream,
                            &ServerMsg::Refused {
                                reason: bota_proto::RejectReason::SeatHeld,
                            },
                        );
                        return false;
                    }
                    st.seats[i].conn = Some((id, Arc::clone(stream)));
                    st.seats[i].ready = false;
                    (Some(SlotId(i as u8)), Some(i))
                }
                (Role::Spectator, Some(_)) => {
                    st.others.push((id, Arc::clone(stream)));
                    (None, None)
                }
                _ => return false,
            };
            *me = Some((id, seat_index));
            send(
                stream,
                &ServerMsg::Welcome {
                    player_id: id,
                    slot,
                    tick_rate: 30,
                    mode: TickMode::Realtime,
                },
            );
            if st.started {
                send(stream, &ServerMsg::MatchStart { info: info(&st) });
            }
            true
        }
        (Some((_, Some(i))), ClientMsg::PickHero { hero }) => {
            st.seats[*i].hero = Some(hero);
            true
        }
        (Some((_, Some(i))), ClientMsg::SetReady(ready)) => {
            st.seats[*i].ready = ready;
            maybe_start(&mut st, state);
            true
        }
        (Some(_), _) => true,
        (None, _) => false,
    }
}

fn info(st: &State) -> MatchInfo {
    MatchInfo {
        match_id: st.match_id,
        map: MapId(2),
        tick_rate: 30,
        pregame_ticks: 0,
        trees: Vec::new(),
        terrain_cells: 0,
        terrain_rle: Vec::new(),
        opaque_cells: Vec::new(),
        mode: TickMode::Realtime,
        picks: st
            .seats
            .iter()
            .enumerate()
            .map(|(i, s)| bota_proto::Pick {
                slot: SlotId(i as u8),
                team: if i % 2 == 0 {
                    Team::Radiant
                } else {
                    Team::Dire
                },
                hero: s.hero.unwrap_or(HeroId::SHADOW_FIEND),
            })
            .collect(),
        shop: Vec::new(),
        fountains: [Vec2::from_ints(0, 0), Vec2::from_ints(1, 1)],
        shop_range: 0,
    }
}

fn maybe_start(st: &mut State, state: &Arc<Mutex<State>>) {
    if st.started || !st.seats.iter().all(|s| s.conn.is_some() && s.ready) {
        return;
    }
    st.started = true;
    let start = ServerMsg::MatchStart { info: info(st) };
    for (_, s) in everyone(st) {
        send(&s, &start);
    }
    let state = Arc::clone(state);
    thread::spawn(move || {
        let (outcome, after_ms) = {
            let st = state.lock().expect("state");
            (st.outcome.clone(), st.after_ms)
        };
        thread::sleep(Duration::from_millis(after_ms));
        let winner = match outcome.as_str() {
            "radiant" => Team::Radiant,
            "dire" => Team::Dire,
            "draw" => Team::Neutral,
            "crash" => std::process::exit(3),
            _ => loop {
                thread::park();
            },
        };
        let st = state.lock().expect("state");
        let over = ServerMsg::MatchOver {
            winner,
            stats: MatchStats {
                duration: 27_000,
                slots: (0..st.seats.len() as u8)
                    .map(|i| SlotStats {
                        slot: SlotId(i),
                        kills: 0,
                        deaths: 0,
                        assists: 0,
                        last_hits: 0,
                        denies: 0,
                        net_worth: 0,
                        hero_damage: 0,
                        structure_damage: 0,
                    })
                    .collect(),
            },
        };
        for (_, s) in everyone(&st) {
            send(&s, &over);
        }
        drop(st);
        thread::sleep(Duration::from_millis(300));
        std::process::exit(0);
    });
}
