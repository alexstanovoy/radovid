//! The `/game` path: a WebSocket on the outside, bota's frames on the inside,
//! bridged to the game server's plain TCP port.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bota_proto::{ClientMsg, FrameReader, MAX_PAYLOAD_LEN, Role, SeatKey, decode_payload};
use futures_util::{SinkExt, StreamExt};
use radovid_proto::*;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::oneshot;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::protocol::CloseFrame;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;

use crate::hub::Cmd;
use crate::server::{Shared, Socket, Unauthenticated};
use crate::throttle::TokenBucket;

/// Seconds to reach the game server.
const CONNECT_SECS: u64 = 5;
const SEND_TIMEOUT: Duration = Duration::from_secs(30);

/// A game connection's opening message and what it asks for.
struct Entry {
    first: tokio_tungstenite::tungstenite::Bytes,
    key: SeatKey,
    spectator: bool,
}

pub async fn run(ws: Socket, shared: Arc<Shared>, mut guard: Unauthenticated) {
    let (mut sink, mut stream) = ws.split();
    let Some(Entry {
        first,
        key,
        spectator,
    }) = read_entry(&mut stream).await
    else {
        deny(&mut sink).await;
        return;
    };
    let (reply, answer) = oneshot::channel();
    let asked = shared
        .hub
        .tx
        .send(Cmd::GameRoute {
            key,
            spectator,
            reply,
        })
        .await;
    if asked.is_err() {
        return;
    }
    let Ok(Some(route)) = answer.await else {
        deny(&mut sink).await;
        return;
    };
    let tcp = tokio::time::timeout(
        Duration::from_secs(CONNECT_SECS),
        TcpStream::connect(route.addr),
    )
    .await;
    if let Ok(Ok(tcp)) = tcp {
        guard.release();
        let _ = bridge(
            &mut sink,
            &mut stream,
            tcp,
            &first,
            shared.config.game_messages_per_sec,
        )
        .await;
    }
    let _ = sink.close().await;
    let _ = shared.hub.tx.send(Cmd::GameClosed { key, spectator }).await;
}

async fn deny(sink: &mut Sink) {
    let denied = CloseFrame {
        code: CloseCode::Policy,
        reason: "".into(),
    };
    let _ = sink.send(Message::Close(Some(denied))).await;
}

/// Waits for the player's or spectator's `Hello` with a seat key. A referee, a
/// keyless or malformed one, or none in time is no entry.
async fn read_entry(stream: &mut Stream) -> Option<Entry> {
    let first = tokio::time::timeout(Duration::from_secs(GAME_HELLO_DEADLINE_SECS), async {
        loop {
            match stream.next().await {
                Some(Ok(Message::Binary(bytes))) => return Some(bytes),
                Some(Ok(Message::Ping(_) | Message::Pong(_))) => {}
                _ => return None,
            }
        }
    })
    .await
    .ok()??;
    let payload = whole_frame(&first, MAX_PAYLOAD_LEN)?;
    let ClientMsg::Hello {
        role,
        key: Some(key),
        ..
    } = decode_payload(payload).ok()?
    else {
        return None;
    };
    let spectator = match role {
        Role::Spectator => true,
        Role::Player | Role::Bot => false,
        Role::Referee => return None,
    };
    Some(Entry {
        first,
        key,
        spectator,
    })
}

type Sink = futures_util::stream::SplitSink<Socket, Message>;
type Stream = futures_util::stream::SplitStream<Socket>;

async fn bridge(
    sink: &mut Sink,
    stream: &mut Stream,
    tcp: TcpStream,
    first: &[u8],
    rate: u32,
) -> Result<(), ()> {
    tcp.set_nodelay(true).map_err(|_| ())?;
    let socket = socket2::SockRef::from(&tcp);
    let keepalive = socket2::TcpKeepalive::new()
        .with_time(Duration::from_secs(30))
        .with_interval(Duration::from_secs(10));
    let _ = socket.set_tcp_keepalive(&keepalive);
    let (mut read, mut write) = tcp.into_split();
    write.write_all(first).await.map_err(|_| ())?;
    let mut bucket = TokenBucket::new(rate, rate.saturating_mul(2), Instant::now());
    let mut frames = FrameReader::new();
    let mut buf = vec![0u8; 32 * 1024];
    let mut ping = tokio::time::interval(Duration::from_secs(PING_INTERVAL_SECS));
    ping.tick().await;
    let mut last_heard = Instant::now();
    loop {
        tokio::select! {
            message = stream.next() => {
                let message = message.ok_or(())?.map_err(|_| ())?;
                last_heard = Instant::now();
                match message {
                    Message::Binary(bytes) => {
                        if whole_frame(&bytes, MAX_PAYLOAD_LEN).is_none() || bucket.take(Instant::now()).is_err() {
                            return Err(());
                        }
                        write.write_all(&bytes).await.map_err(|_| ())?;
                    }
                    Message::Ping(_) | Message::Pong(_) => {}
                    _ => return Err(()),
                }
            }
            got = read.read(&mut buf) => {
                let n = got.map_err(|_| ())?;
                if n == 0 {
                    return Ok(());
                }
                frames.push(&buf[..n]);
                while let Some(frame) = frames.next_frame().map_err(|_| ())? {
                    match tokio::time::timeout(SEND_TIMEOUT, sink.send(Message::Binary(frame.into()))).await {
                        Ok(Ok(())) => {}
                        _ => return Err(()),
                    }
                }
            }
            _ = ping.tick() => {
                if last_heard.elapsed() > Duration::from_secs(PONG_DEADLINE_SECS) {
                    return Err(());
                }
                sink.send(Message::Ping(Vec::new().into())).await.map_err(|_| ())?;
            }
        }
    }
}
