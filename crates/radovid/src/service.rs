//! Starting and stopping the whole service.

use std::net::SocketAddr;
use std::sync::Arc;

use tokio::net::TcpListener;
use tokio::sync::watch;
use tokio_rustls::TlsAcceptor;

use bota_proto::WIRE_FINGERPRINT;

use crate::backend::{self, Backend};
use crate::config::Config;
use crate::db::Db;
use crate::hub::{self, Cmd};
use crate::password::Passwords;
use crate::server::{self, Shared};
use crate::tls::{self, Identity};

/// A running service.
pub struct Service {
    /// Where it listens.
    pub addr: SocketAddr,
    stop: watch::Sender<bool>,
    hub_tx: tokio::sync::mpsc::Sender<Cmd>,
    hub_task: tokio::task::JoinHandle<()>,
    listener_task: tokio::task::JoinHandle<()>,
}

impl Service {
    /// Opens the database, cleans up after an earlier run and starts listening
    /// on `config.listen`. `{port}` in the public game URL is the port bound.
    pub async fn start(mut config: Config, identity: &Identity) -> Result<Service, String> {
        config.validate()?;
        let backend = backend::from_config(&config.backend);
        check_wire(&*backend).await?;
        let listener = TcpListener::bind(config.listen)
            .await
            .map_err(|e| format!("cannot listen on {}: {e}", config.listen))?;
        let addr = listener.local_addr().map_err(|e| e.to_string())?;
        config.public_game_url = config
            .public_game_url
            .replace("{port}", &addr.port().to_string());
        let acceptor = TlsAcceptor::from(tls::server_config(identity)?);
        let db = Db::open(&config.database).map_err(|e| e.to_string())?;
        backend.clean_up_orphans().await;
        let lost = db
            .fail_unfinished_matches()
            .await
            .map_err(|e| e.to_string())?;
        if lost > 0 {
            eprintln!("radovid: {lost} matches of an earlier run recorded as lost");
        }
        let passwords = Passwords::new(config.password)?;
        let config = Arc::new(config);
        let (hub, hub_task) = hub::spawn(Arc::clone(&config), db.clone(), backend);
        let shared = Arc::new(Shared::new(config, db, hub.clone(), passwords));
        let (stop, stopped) = watch::channel(false);
        let listener_task = tokio::spawn(server::serve(listener, acceptor, shared, stopped));
        Ok(Service {
            addr,
            stop,
            hub_tx: hub.tx,
            hub_task,
            listener_task,
        })
    }

    /// Stops listening, stops every running match and waits for the hub.
    pub async fn shutdown(self) {
        let _ = self.stop.send(true);
        let _ = self.listener_task.await;
        let _ = self.hub_tx.send(Cmd::Shutdown).await;
        let _ = self.hub_task.await;
    }
}

/// Refuses game servers of another bota wire: each would turn the referee away
/// at its Hello, and every match would be lost.
async fn check_wire(backend: &dyn Backend) -> Result<(), String> {
    let theirs = backend
        .wire_fingerprint()
        .await
        .map_err(|e| format!("cannot learn the game server's wire fingerprint: {e}"))?;
    if theirs != WIRE_FINGERPRINT {
        return Err(format!(
            "the game server speaks bota wire {theirs:016x} and radovid {WIRE_FINGERPRINT:016x}: \
             build both from one bota commit"
        ));
    }
    eprintln!("radovid: the game server speaks bota wire {theirs:016x}, as radovid does");
    Ok(())
}
