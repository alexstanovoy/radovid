//! The game link: bota's protocol over the service's `/game` WebSocket.

use std::time::Duration;

use bota_client_core::{
    AckPolicy, ClientSession, Ingested, IssuedOrder, Perspective, Seated, SessionOptions, Transport,
};
use bota_proto::{ClientMsg, FrameReader, HeroId, Order, Role, ServerMsg, Team, TickMode, encode_frame_to_vec};
use radovid_proto::{Handoff, MatchStats};
use tungstenite::Message;

use crate::url::{Ws, connect};
use crate::{ClientError, TlsConfig};

const IO_TIMEOUT: Duration = Duration::from_secs(300);
const MAX_MESSAGES_BEFORE_WELCOME: usize = 64;

/// A connection to the game path; a [`Transport`] for `bota_client_core`.
pub struct GameLink {
    ws: Ws,
    reader: FrameReader,
}

impl GameLink {
    /// Opens the handoff's game URL. Nothing is said yet.
    pub fn connect(handoff: &Handoff, tls: &TlsConfig) -> Result<GameLink, ClientError> {
        let ws = connect(&handoff.game_url, tls, bota_proto::MAX_PAYLOAD_LEN + bota_proto::LEN_PREFIX, IO_TIMEOUT)?;
        Ok(GameLink { ws, reader: FrameReader::new() })
    }
}

impl Transport for GameLink {
    fn recv(&mut self) -> std::io::Result<Option<ServerMsg>> {
        loop {
            match self.reader.next_message::<ServerMsg>() {
                Ok(Some(message)) => return Ok(Some(message)),
                Ok(None) => {}
                Err(error) => return Err(std::io::Error::other(format!("undecodable server message: {error}"))),
            }
            match self.ws.read() {
                Ok(Message::Binary(bytes)) => self.reader.push(&bytes),
                Ok(Message::Close(_)) => return Ok(None),
                Ok(_) => {}
                Err(tungstenite::Error::ConnectionClosed | tungstenite::Error::AlreadyClosed) => return Ok(None),
                Err(error) => return Err(std::io::Error::other(error.to_string())),
            }
        }
    }

    fn send(&mut self, msg: &ClientMsg) -> std::io::Result<()> {
        let frame = encode_frame_to_vec(msg).map_err(std::io::Error::other)?;
        self.ws
            .send(Message::Binary(frame.into()))
            .map_err(|e| std::io::Error::other(e.to_string()))
    }
}

/// Opens the link and takes the seat of the handoff: picks `hero` and says
/// ready. Calling it again with the same handoff is a reconnect: the service
/// replaces the old connection.
pub fn take_seat(handoff: &Handoff, hero: HeroId, tls: &TlsConfig) -> Result<(GameLink, Seated), ClientError> {
    let mut link = GameLink::connect(handoff, tls)?;
    let name = handoff
        .seats
        .iter()
        .find(|seat| seat.slot == handoff.slot)
        .map_or("bot", |seat| seat.nickname.as_str());
    let seated = bota_client_core::join(&mut link, name, Some(hero), Some(handoff.seat_key))?;
    Ok((link, seated))
}

/// Opens the link as a spectator locked to the seat's view (the same account's
/// own match) and waits for the welcome.
pub fn spectate(handoff: &Handoff, tls: &TlsConfig) -> Result<GameLink, ClientError> {
    let mut link = GameLink::connect(handoff, tls)?;
    link.send(&ClientMsg::Hello { role: Role::Spectator, name: "spectator".to_owned(), key: Some(handoff.seat_key) })?;
    for _ in 0..MAX_MESSAGES_BEFORE_WELCOME {
        match link.recv()? {
            Some(ServerMsg::Welcome { .. }) => return Ok(link),
            Some(_) => {}
            None => return Err(ClientError::Closed),
        }
    }
    Err(ClientError::Protocol("no welcome".to_owned()))
}

/// Plays a match on a taken seat to its end: folds the stream, asks `decide`
/// for an order once per tick and acknowledges ticks where the match is
/// lockstep. Returns the winner (`Team::Neutral` for a draw) and the final
/// numbers; the link's end before that is [`ClientError::Closed`], after which
/// the caller may take the seat again (`take_seat` with the same handoff).
pub fn play(
    link: &mut GameLink,
    seated: &Seated,
    hero: HeroId,
    mut decide: impl FnMut(&ClientSession) -> Option<Order>,
) -> Result<(Team, MatchStats), ClientError> {
    let options = SessionOptions {
        perspective: Perspective::Seat(seated.slot),
        hero: Some(hero),
        ack: if seated.mode == TickMode::Lockstep { AckPolicy::AfterTurn } else { AckPolicy::Never },
        forget_after_ticks: 480,
    };
    let mut session: Option<ClientSession> = None;
    loop {
        let message = link.recv()?.ok_or(ClientError::Closed)?;
        if let ServerMsg::MatchStart { info } = &message {
            let started = ClientSession::start(seated, info, options).map_err(|e| ClientError::Protocol(e.to_string()))?;
            session = Some(started);
            continue;
        }
        let Some(session) = session.as_mut() else {
            continue; // the lobby is still being told
        };
        match session.ingest(message, &mut ()).map_err(|e| ClientError::Protocol(e.to_string()))? {
            Ingested::TickComplete { .. } => {
                if let Some(order) = decide(session)
                    && let Ok(given) = session.issue(IssuedOrder { unit: None, order })
                {
                    link.send(&given.message())?;
                }
                if let Some(tick) = session.end_turn() {
                    link.send(&ClientMsg::Ack { tick })?;
                }
            }
            Ingested::Over { winner, stats } => return Ok((winner, stats)),
            _ => {}
        }
    }
}
