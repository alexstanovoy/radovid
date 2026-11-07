//! A fixed pool of read-only connections, separate from account and match writes.
use super::*;
use std::sync::atomic::{AtomicUsize,Ordering};
use tokio::sync::Semaphore;

const READ_CONNECTIONS:usize=4;
const MAX_READS:usize=64;

struct Reader {
    connection:Arc<Mutex<Connection>>,
    turn:Arc<Semaphore>,
}

pub(super) struct ReadPool {
    connections:Vec<Reader>,
    next:AtomicUsize,
    pending:Arc<Semaphore>,
}

impl ReadPool {
    pub(super) fn open(path:&Path)->DbResult<Self> {
        let mut connections=Vec::with_capacity(READ_CONNECTIONS);
        for _ in 0..READ_CONNECTIONS {
            let connection=Connection::open_with_flags(path,rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
            connection.pragma_update(None,"query_only",true)?;
            connection.busy_timeout(std::time::Duration::from_millis(50))?;
            let schema:i64=connection.pragma_query_value(None,"user_version",|row| row.get(0))?;
            if schema!=SCHEMA_VERSION { return Err(DbError::Sqlite(rusqlite::Error::InvalidParameterName("lookup database must be file-backed and have the current schema".into()))); }
            connections.push(Reader { connection:Arc::new(Mutex::new(connection)),turn:Arc::new(Semaphore::new(1)) });
        }
        Ok(Self { connections,next:AtomicUsize::new(0),pending:Arc::new(Semaphore::new(MAX_READS)) })
    }

    pub(super) async fn run<T:Send+'static>(&self,work:impl FnOnce(&mut Connection)->rusqlite::Result<T>+Send+'static)->DbResult<T> {
        let pending=Arc::clone(&self.pending).try_acquire_owned().map_err(|_| DbError::ReadBusy)?;
        let reader=&self.connections[self.next.fetch_add(1,Ordering::Relaxed)%READ_CONNECTIONS];
        let turn=Arc::clone(&reader.turn).acquire_owned().await.map_err(|_| DbError::Join)?;
        let connection=Arc::clone(&reader.connection);
        tokio::task::spawn_blocking(move || {
            let _permits=(pending,turn);
            let mut connection=connection.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            work(&mut connection)
        }).await.map_err(|_| DbError::Join)?.map_err(DbError::from)
    }

    #[cfg(test)]
    pub(super) fn first_connection(&self)->&Mutex<Connection> { &self.connections[0].connection }
}
