//! Opportunistic short checkpoints, with a separate emergency WAL size limit.
use super::*;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use tokio::sync::{Notify, RwLock};

const CHECKPOINT_PAGES: u64 = 4000;
pub(super) const HARD_LIMIT_BYTES: u64 = 512 * 1024 * 1024;
const CHECKPOINT_BUSY: Duration = Duration::from_millis(25);
const RETRY_BACKOFF: Duration = Duration::from_millis(250);

pub(super) struct Wal {
    path: std::path::PathBuf,
    limit: u64,
    hard_limit: u64,
    checkpoint: Arc<Mutex<Connection>>,
    pub(super) snapshots: Arc<RwLock<()>>,
    pending: AtomicBool,
    retry_at: Mutex<Instant>,
    finished: Notify,
}

impl Wal {
    #[cfg(test)]
    pub(super) fn set_limit_for_test(&mut self, bytes: u64) {
        self.limit = bytes;
    }
    #[cfg(test)]
    pub(super) fn set_hard_limit_for_test(&mut self, bytes: u64) {
        self.hard_limit = bytes;
    }
    #[cfg(test)]
    pub(super) fn reset_retry_for_test(&self) {
        *self.retry_at.lock().expect("retry clock") = Instant::now();
    }
    #[cfg(test)]
    pub(super) async fn wait_for_test(&self) {
        loop {
            let notified = self.finished.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if !self.pending.load(Ordering::Acquire) {
                return;
            }
            notified.await;
        }
    }

    pub(super) fn new(database: &Path, connection: &Connection) -> rusqlite::Result<Self> {
        let page_size: u32 = connection.pragma_query_value(None, "page_size", |row| row.get(0))?;
        let checkpoint =
            Connection::open_with_flags(database, rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE)?;
        checkpoint.busy_timeout(CHECKPOINT_BUSY)?;
        let mut path = database.as_os_str().to_os_string();
        path.push("-wal");
        Ok(Self {
            path: path.into(),
            limit: 32 + CHECKPOINT_PAGES * (u64::from(page_size) + 24),
            hard_limit: HARD_LIMIT_BYTES,
            checkpoint: Arc::new(Mutex::new(checkpoint)),
            snapshots: Arc::new(RwLock::new(())),
            pending: AtomicBool::new(false),
            retry_at: Mutex::new(Instant::now()),
            finished: Notify::new(),
        })
    }

    fn bytes(&self) -> Option<u64> {
        match std::fs::metadata(&self.path) {
            Ok(file) => Some(file.len()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Some(0),
            Err(error) => {
                eprintln!("WARN: measuring database WAL: {error}");
                None
            }
        }
    }
    pub(super) fn exceeds_limit(&self) -> bool {
        self.bytes().is_none_or(|size| size > self.limit)
    }
    pub(super) fn exceeds_hard_limit(&self) -> bool {
        self.bytes().is_none_or(|size| size > self.hard_limit)
    }

    pub(super) fn schedule(self: &Arc<Self>, runtime: tokio::runtime::Handle) {
        if Instant::now()
            < *self
                .retry_at
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
            || self
                .pending
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_err()
        {
            return;
        }
        let attempt = Attempt(Arc::clone(self));
        runtime.spawn(async move {
            let pause=Arc::clone(&attempt.0.snapshots).write_owned().await;
            let checkpoint=Arc::clone(&attempt.0.checkpoint);
            let result=tokio::task::spawn_blocking(move || {
                // Retain admission/attempt ownership even if the awaiting future is cancelled.
                let _lease=(pause,attempt);
                let connection=checkpoint.lock().unwrap_or_else(|poisoned|poisoned.into_inner());
                connection.query_row("PRAGMA wal_checkpoint(TRUNCATE)",[],|row|row.get::<_,i32>(0))
            }).await;
            match result {
                Ok(Ok(0))=>{},
                Ok(Ok(_))=>eprintln!("WARN: database WAL checkpoint busy; normal writes continue below the hard limit"),
                Ok(Err(error))=>eprintln!("WARN: database WAL checkpoint: {error}"),
                Err(error)=>eprintln!("WARN: database WAL checkpoint task: {error}"),
            }
        });
    }
}

struct Attempt(Arc<Wal>);
impl Drop for Attempt {
    fn drop(&mut self) {
        *self
            .0
            .retry_at
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Instant::now() + RETRY_BACKOFF;
        self.0.pending.store(false, Ordering::Release);
        self.0.finished.notify_waiters();
    }
}
