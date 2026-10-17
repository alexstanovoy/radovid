//! One task per match: starts the game server, attends it as its referee and
//! reports to the hub what became of it.

use std::sync::Arc;
use std::time::Duration;

use bota_proto::{ClientMsg, FrameReader, Role, ServerMsg, Team, encode_frame_to_vec};
use radovid_proto::{MatchId, MatchStats};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::{Semaphore, mpsc, oneshot};

use crate::backend::{Backend, Container, MatchSpec};
use crate::hub::Cmd;

/// How long the referee keeps trying to connect to a game server that is up.
const REFEREE_CONNECT: Duration = Duration::from_secs(5);
/// How long a game server has to exit after the match is over.
const EXIT_GRACE: Duration = Duration::from_secs(10);

/// How a match ended, as the hub learns it.
#[derive(Debug)]
pub enum End {
    Over {
        winner: Team,
        stats: MatchStats,
    },
    /// The game server died or could not be attended.
    Crashed(String),
    /// An admin stopped it.
    Stopped,
}

pub struct Supervisor {
    pub match_id: MatchId,
    pub spec: MatchSpec,
    pub backend: Arc<dyn Backend>,
    pub hub: mpsc::Sender<Cmd>,
    pub slots: Arc<Semaphore>,
}

impl Supervisor {
    pub async fn run(self, mut stop: oneshot::Receiver<()>) {
        let id = self.match_id;
        let mut container = None;
        let permit = tokio::select! {
            permit = Arc::clone(&self.slots).acquire_owned() => permit.ok(),
            _ = &mut stop => None,
        };
        let end = match permit {
            None => End::Stopped,
            Some(_) => self.attend(&mut stop, &mut container).await,
        };
        if let Some(container) = &container {
            if matches!(end, End::Over { .. }) {
                // Let it exit by itself first; it has nothing left to do.
            } else {
                container.kill();
            }
        }
        let _ = self.hub.send(Cmd::MatchEnded { match_id: id, end }).await;
        if let Some(mut container) = container
            && tokio::time::timeout(EXIT_GRACE, container.exited())
                .await
                .is_err()
        {
            container.kill();
            container.exited().await;
        }
    }

    async fn attend(&self, stop: &mut oneshot::Receiver<()>, slot: &mut Option<Container>) -> End {
        let started = tokio::select! {
            started = self.backend.start(&self.spec) => started,
            _ = &mut *stop => return End::Stopped,
        };
        let container = match started {
            Ok(container) => slot.insert(container),
            Err(error) => return End::Crashed(format!("start: {error}")),
        };
        let addr = container.addr;
        let referee = tokio::select! {
            referee = connect_referee(addr, &self.spec) => referee,
            _ = &mut *stop => return End::Stopped,
            exit = container.exited() => return End::Crashed(format!("exited before the referee attached: {}", exit.description)),
        };
        let mut stream = match referee {
            Ok(stream) => stream,
            Err(error) => return End::Crashed(format!("referee: {error}")),
        };
        let mut reader = FrameReader::new();
        let mut buf = vec![0u8; 16 * 1024];
        loop {
            let message = loop {
                match reader.next_message::<ServerMsg>() {
                    Ok(Some(message)) => break message,
                    Ok(None) => {}
                    Err(error) => return End::Crashed(format!("referee stream: {error}")),
                }
                let read = tokio::select! {
                    read = stream.read(&mut buf) => read,
                    _ = &mut *stop => return End::Stopped,
                    exit = container.exited() => return End::Crashed(format!("exited: {}", exit.description)),
                };
                match read {
                    Ok(0) | Err(_) => {
                        return End::Crashed(
                            "the referee stream ended without a result".to_owned(),
                        );
                    }
                    Ok(n) => reader.push(&buf[..n]),
                }
            };
            match message {
                ServerMsg::Welcome { .. } => {
                    let _ = self
                        .hub
                        .send(Cmd::MatchUp {
                            match_id: self.match_id,
                            addr,
                        })
                        .await;
                }
                ServerMsg::MatchStart { info } => {
                    let picks = info.picks.iter().map(|p| (p.slot, p.hero)).collect();
                    let _ = self
                        .hub
                        .send(Cmd::MatchStarted {
                            match_id: self.match_id,
                            picks,
                        })
                        .await;
                }
                ServerMsg::MatchOver { winner, stats } => return End::Over { winner, stats },
                _ => {}
            }
        }
    }
}

async fn connect_referee(
    addr: std::net::SocketAddr,
    spec: &MatchSpec,
) -> std::io::Result<TcpStream> {
    let deadline = tokio::time::Instant::now() + REFEREE_CONNECT;
    let mut stream = loop {
        match TcpStream::connect(addr).await {
            Ok(stream) => break stream,
            Err(error) if tokio::time::Instant::now() >= deadline => return Err(error),
            Err(_) => tokio::time::sleep(Duration::from_millis(50)).await,
        }
    };
    stream.set_nodelay(true)?;
    let hello = encode_frame_to_vec(&ClientMsg::Hello {
        role: Role::Referee,
        name: "referee".to_owned(),
        key: Some(spec.referee_key),
        wire_fingerprint: bota_proto::WIRE_FINGERPRINT,
    })
    .map_err(std::io::Error::other)?;
    stream.write_all(&hello).await?;
    Ok(stream)
}
