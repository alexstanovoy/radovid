//! Starting and stopping the whole service.

#[cfg(test)]
mod leaderboard_tests;

use std::net::SocketAddr;
use std::sync::{Arc, RwLock};

use tokio::net::TcpListener;
use tokio::sync::watch;

use crate::config::Config;
use crate::db::Db;
use crate::hub::{self, Cmd};
use crate::launcher;
use crate::password::Passwords;
use crate::server::{self, Certificates, Shared};
use crate::tls::{self, Identity};

/// A running service.
pub struct Service {
    #[cfg(feature = "test_support")]
    requests: Arc<crate::test_support::Requests>,
    #[cfg(feature = "test_support")]
    db: Db,
    /// Where it listens.
    pub addr: SocketAddr,
    certificates: Certificates,
    stop: watch::Sender<bool>,
    hub_tx: tokio::sync::mpsc::Sender<Cmd>,
    hub_task: tokio::task::JoinHandle<()>,
    listener_task: tokio::task::JoinHandle<()>,
}

impl Service {
    #[cfg(feature = "test_support")]
    #[must_use]
    pub fn request_count(&self, kind: crate::test_support::Read) -> u64 {
        self.requests.count(kind)
    }

    /// Serves every later read as of `now`, so a test can assert exact time-dependent values.
    #[cfg(feature = "test_support")]
    pub fn freeze_now(&self, now: radovid_proto::UnixMs) {
        self.db.freeze_now(now);
    }

    #[cfg(test)]
    pub(crate) async fn freeze_lookup_clock_for_test(&self, now: std::time::Instant) {
        let (done, ready) = tokio::sync::oneshot::channel();
        self.hub_tx
            .send(Cmd::FreezeLookupClock { now, done })
            .await
            .expect("hub");
        ready.await.expect("clock installed");
    }

    /// Checks every ruleset's game servers (`version_probe`), opens the
    /// database, cleans up after an earlier run and starts listening.
    pub async fn start(config: Config, identity: &Identity) -> Result<Service, String> {
        config.validate()?;
        for ruleset in &config.rulesets {
            launcher::probe(ruleset)
                .await
                .map_err(|e| format!("ruleset {}: {e}", ruleset.id))?;
        }
        let listener = TcpListener::bind(config.listen)
            .await
            .map_err(|e| format!("cannot listen on {}: {e}", config.listen))?;
        let addr = listener.local_addr().map_err(|e| e.to_string())?;
        let certificates = Arc::new(RwLock::new(tls::server_config(identity)?));
        crate::private::database(&config.database)?;
        let db = Db::open(&config.database).map_err(|e| e.to_string())?;
        launcher::clean_up_orphans(&config.rulesets).await;
        let recovered = db
            .recover_results()
            .await
            .map_err(|error| format!("cannot replay pending match results: {error}"))?;
        if recovered > 0 {
            eprintln!("radovid: recovered {recovered} pending match results");
        }
        let lost = db
            .fail_unfinished_matches()
            .await
            .map_err(|e| e.to_string())?;
        if lost > 0 {
            eprintln!("radovid: {lost} matches of an earlier run recorded as lost");
        }
        let passwords = Passwords::new(config.password)?;
        let config = Arc::new(config);
        let (hub, hub_task) = hub::spawn(Arc::clone(&config), db.clone());
        #[cfg(feature = "test_support")]
        let frozen_db = db.clone();
        let shared = Arc::new(Shared::new(config, db, hub.clone(), passwords));
        #[cfg(feature = "test_support")]
        let requests = Arc::clone(&shared.requests);
        let (stop, stopped) = watch::channel(false);
        let listener_task = tokio::spawn(server::serve(
            listener,
            Arc::clone(&certificates),
            shared,
            stopped,
        ));
        Ok(Service {
            #[cfg(feature = "test_support")]
            requests,
            #[cfg(feature = "test_support")]
            db: frozen_db,
            addr,
            certificates,
            stop,
            hub_tx: hub.tx,
            hub_task,
            listener_task,
        })
    }

    /// Serves new connections with another certificate (a renewal); the
    /// connections already up keep theirs.
    pub fn reload_tls(&self, identity: &Identity) -> Result<(), String> {
        let config = tls::server_config(identity)?;
        *self.certificates.write().unwrap_or_else(|p| p.into_inner()) = config;
        Ok(())
    }

    /// Stops listening, stops every match and waits for the hub.
    pub async fn shutdown(self) {
        let _ = self.stop.send(true);
        let _ = self.listener_task.await;
        let _ = self.hub_tx.send(Cmd::Shutdown).await;
        let _ = self.hub_task.await;
    }
}
