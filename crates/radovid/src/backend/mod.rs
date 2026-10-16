//! Where game servers run: a trait with a local-process and a docker backend.

mod docker;
mod local;

use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;

use bota_proto::SeatKey;
use radovid_proto::MatchId;
use tokio::sync::{mpsc, watch};

pub use docker::DockerBackend;
pub use local::LocalBackend;

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Everything a game server is started with.
pub struct MatchSpec {
    pub match_id: MatchId,
    /// One key and display name per seat, in slot order.
    pub seats: Vec<(SeatKey, String)>,
    /// What radovid's own connection presents.
    pub referee_key: SeatKey,
    /// The ruleset's game server arguments.
    pub rules_args: Vec<String>,
    pub start_after_secs: u64,
}

/// Connections a game server holds at most: two players, their spectators and
/// the referee.
pub const MAX_CONNECTIONS: usize = 24;

impl MatchSpec {
    /// The command line shared by every backend (the secrets go by environment).
    pub fn args(&self, port: u16) -> Vec<String> {
        let mut args = self.rules_args.clone();
        args.extend([
            "--port".to_owned(),
            port.to_string(),
            "--match-id".to_owned(),
            self.match_id.0.to_string(),
            "--start-after-secs".to_owned(),
            self.start_after_secs.to_string(),
            "--max-connections".to_owned(),
            MAX_CONNECTIONS.to_string(),
        ]);
        args
    }

    /// `BOTA_SEATS`: `key_hex:name`, comma separated.
    pub fn seats_env(&self) -> String {
        self.seats
            .iter()
            .map(|(key, name)| format!("{}:{}", hex(&key.0), name.replace([',', ':'], "_")))
            .collect::<Vec<_>>()
            .join(",")
    }

    pub fn referee_env(&self) -> String {
        hex(&self.referee_key.0)
    }
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// How a game server ended.
#[derive(Clone, Debug)]
pub struct Exit {
    pub description: String,
}

/// A running game server.
pub struct Container {
    /// Where its plain TCP port is reachable.
    pub addr: SocketAddr,
    exited: watch::Receiver<Option<Exit>>,
    kill: mpsc::UnboundedSender<()>,
}

impl Container {
    /// The handles a backend's watcher task holds.
    pub fn new(addr: SocketAddr) -> (Container, watch::Sender<Option<Exit>>, mpsc::UnboundedReceiver<()>) {
        let (exit_tx, exited) = watch::channel(None);
        let (kill, kill_rx) = mpsc::unbounded_channel();
        (Container { addr, exited, kill }, exit_tx, kill_rx)
    }

    /// Asks the backend to kill the server. Idempotent.
    pub fn kill(&self) {
        let _ = self.kill.send(());
    }

    /// Resolves when the server is gone.
    pub async fn exited(&mut self) -> Exit {
        loop {
            if let Some(exit) = self.exited.borrow().clone() {
                return exit;
            }
            if self.exited.changed().await.is_err() {
                return Exit {
                    description: "the backend stopped watching".to_owned(),
                };
            }
        }
    }
}

/// Starts game servers.
pub trait Backend: Send + Sync + 'static {
    fn start<'a>(&'a self, spec: &'a MatchSpec) -> BoxFuture<'a, std::io::Result<Container>>;

    /// Kills whatever an earlier run of the service left behind.
    fn clean_up_orphans(&self) -> BoxFuture<'_, ()>;
}

pub fn from_config(config: &crate::config::BackendConfig) -> Arc<dyn Backend> {
    use crate::config::BackendConfig;
    match config {
        BackendConfig::Local { bota_server } => Arc::new(LocalBackend::new(bota_server.clone())),
        BackendConfig::Docker {
            image,
            network,
            docker,
            memory,
            cpus,
            publish_loopback,
        } => Arc::new(DockerBackend::new(
            docker.clone(),
            image.clone(),
            network.clone(),
            memory.clone(),
            cpus.clone(),
            *publish_loopback,
        )),
    }
}
