//! The game link: bota's protocol over the service's `/game` WebSocket.

use std::time::Duration;

use bota_client_core::{ClientSession, IssuedOrder, Outcome, PlayError, Seated, Transport};
use bota_proto::{ClientMsg, FrameReader, HeroId, Order, Role, ServerMsg, encode_frame_to_vec};
use radovid_proto::Handoff;
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
        let ws = connect(
            &handoff.game_url,
            tls,
            bota_proto::MAX_PAYLOAD_LEN + bota_proto::LEN_PREFIX,
            IO_TIMEOUT,
        )?;
        Ok(GameLink {
            ws,
            reader: FrameReader::new(),
        })
    }
}

impl Transport for GameLink {
    fn recv(&mut self) -> std::io::Result<Option<ServerMsg>> {
        loop {
            match self.reader.next_message::<ServerMsg>() {
                Ok(Some(message)) => return Ok(Some(message)),
                Ok(None) => {}
                Err(error) => {
                    return Err(std::io::Error::other(format!(
                        "undecodable server message: {error}"
                    )));
                }
            }
            match self.ws.read() {
                Ok(Message::Binary(bytes)) => self.reader.push(&bytes),
                Ok(Message::Close(_)) => return Ok(None),
                Ok(_) => {}
                Err(tungstenite::Error::ConnectionClosed | tungstenite::Error::AlreadyClosed) => {
                    return Ok(None);
                }
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
pub fn take_seat(
    handoff: &Handoff,
    hero: HeroId,
    tls: &TlsConfig,
) -> Result<(GameLink, Seated), ClientError> {
    let mut link = GameLink::connect(handoff, tls)?;
    let name = handoff
        .seats
        .iter()
        .find(|seat| seat.slot == handoff.slot)
        .map_or("bot", |seat| seat.nickname.as_str());
    let seated = bota_client_core::join(&mut link, name, hero, Some(handoff.seat_key))?;
    Ok((link, seated))
}

/// Opens the link as a spectator locked to the seat's view (the same account's
/// own match) and waits for the welcome.
pub fn spectate(handoff: &Handoff, tls: &TlsConfig) -> Result<GameLink, ClientError> {
    let mut link = GameLink::connect(handoff, tls)?;
    link.send(&ClientMsg::Hello {
        role: Role::Spectator,
        name: "spectator".to_owned(),
        key: Some(handoff.seat_key),
        wire_fingerprint: bota_proto::WIRE_FINGERPRINT,
    })?;
    for _ in 0..MAX_MESSAGES_BEFORE_WELCOME {
        match link.recv()? {
            Some(ServerMsg::Welcome { .. }) => return Ok(link),
            Some(_) => {}
            None => return Err(ClientError::Closed),
        }
    }
    Err(ClientError::Protocol("no welcome".to_owned()))
}

/// Plays a match on a taken seat to its end with `bota_client_core::play`:
/// once per tick `decide` is shown the session and returns an order or none;
/// ticks are acknowledged. The link's end before the match is over is an error,
/// after which the caller may take the seat again (`take_seat` with the same
/// handoff).
pub fn play(
    link: &mut GameLink,
    seated: &Seated,
    mut decide: impl FnMut(&ClientSession) -> Option<Order>,
) -> Result<Outcome, ClientError> {
    bota_client_core::play(link, *seated, &mut (), |session| {
        decide(session).map(|order| IssuedOrder { unit: None, order })
    })
    .map_err(|error| match error {
        PlayError::Closed => ClientError::Closed,
        other => ClientError::Protocol(other.to_string()),
    })
}
