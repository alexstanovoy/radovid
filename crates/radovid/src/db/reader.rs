//! A fixed pool of read-only connections, separate from account and match writes.
use super::*;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

const READ_CONNECTIONS: usize = 4;
const MAX_READS: usize = 64;

struct ReturnReader {
    index: usize,
    free: Arc<Mutex<Vec<usize>>>,
    _permit: OwnedSemaphorePermit,
}

impl Drop for ReturnReader {
    fn drop(&mut self) {
        self.free
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(self.index);
    }
}

pub(super) struct ReadPool {
    connections: Vec<Arc<Mutex<Connection>>>,
    free: Arc<Mutex<Vec<usize>>>,
    available: Arc<Semaphore>,
    pending: Arc<Semaphore>,
}

impl ReadPool {
    #[cfg(test)]
    pub(super) fn pending_for_test(&self) -> usize {
        MAX_READS - self.pending.available_permits()
    }
    pub(super) fn open(path: &Path) -> DbResult<Self> {
        let mut connections = Vec::with_capacity(READ_CONNECTIONS);
        for _ in 0..READ_CONNECTIONS {
            let connection =
                Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
            connection.pragma_update(None, "query_only", true)?;
            connection.busy_timeout(std::time::Duration::from_millis(50))?;
            let schema: i64 =
                connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
            if schema != SCHEMA_VERSION {
                return Err(DbError::Sqlite(rusqlite::Error::InvalidParameterName(
                    "lookup database must be file-backed and have the current schema".into(),
                )));
            }
            connections.push(Arc::new(Mutex::new(connection)));
        }
        Ok(Self {
            connections,
            free: Arc::new(Mutex::new((0..READ_CONNECTIONS).rev().collect())),
            available: Arc::new(Semaphore::new(READ_CONNECTIONS)),
            pending: Arc::new(Semaphore::new(MAX_READS)),
        })
    }

    pub(super) async fn run<T: Send + 'static>(
        &self,
        work: impl FnOnce(&mut Connection) -> rusqlite::Result<T> + Send + 'static,
    ) -> DbResult<T> {
        let pending = Arc::clone(&self.pending)
            .try_acquire_owned()
            .map_err(|_| DbError::ReadBusy)?;
        // One FIFO semaphore queues all readers; no request is assigned a busy connection.
        let permit = Arc::clone(&self.available)
            .acquire_owned()
            .await
            .map_err(|_| DbError::Join)?;
        let index = self
            .free
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .pop()
            .expect("an available permit has a free connection");
        let checkout = ReturnReader {
            index,
            free: Arc::clone(&self.free),
            _permit: permit,
        };
        let connection = Arc::clone(&self.connections[index]);
        tokio::task::spawn_blocking(move || {
            let _permits = (pending, checkout);
            let mut connection = connection
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            work(&mut connection)
        })
        .await
        .map_err(|_| DbError::Join)?
        .map_err(DbError::from)
    }

    #[cfg(test)]
    pub(super) fn first_connection(&self) -> &Mutex<Connection> {
        &self.connections[0]
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(flavor = "multi_thread")]
    async fn a_busy_connection_does_not_pin_a_new_read_while_other_connections_are_free() {
        struct Scratch(std::path::PathBuf);
        impl Drop for Scratch {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let scratch = Scratch(
            std::env::temp_dir().join(format!("radovid_read_queue_{}", std::process::id())),
        );
        std::fs::create_dir(&scratch.0).expect("scratch");
        let db = Db::open(&scratch.0.join("queue.db")).expect("database");
        let (began, started) = tokio::sync::oneshot::channel();
        let (release, released) = std::sync::mpsc::sync_channel(1);
        let blocked = db.clone();
        let holding = tokio::spawn(async move {
            blocked
                .read(move |_| {
                    let _ = began.send(());
                    released.recv().expect("release");
                    Ok(())
                })
                .await
        });
        started.await.expect("one reader occupied");
        for _ in 0..3 {
            db.read(|_| Ok(()))
                .await
                .expect("other readers remain free");
        }
        let progress =
            tokio::time::timeout(std::time::Duration::from_secs(1), db.read(|_| Ok(()))).await;
        release.send(()).expect("release reader");
        holding.await.expect("task").expect("reader");
        assert!(
            matches!(progress, Ok(Ok(()))),
            "a shared queue must use an available reader"
        );
    }
}
