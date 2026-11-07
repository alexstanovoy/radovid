//! Transfer the next writer turn to a checkpoint before the WAL can keep growing.
use super::*;
use std::sync::atomic::{AtomicBool,Ordering};
use tokio::sync::{OwnedRwLockWriteGuard,OwnedSemaphorePermit,RwLock};

const CHECKPOINT_PAGES:u64=4000;

pub(super) struct Wal {
    path:std::path::PathBuf,
    limit:u64,
    pub(super) snapshots:Arc<RwLock<()>>,
    blocked:AtomicBool,
}

impl Wal {
    #[cfg(test)]
    pub(super) fn set_limit_for_test(&mut self,bytes:u64) { self.limit=bytes; }
    pub(super) fn new(path:&Path,connection:&Connection)->rusqlite::Result<Self> {
        let page_size:u32=connection.pragma_query_value(None,"page_size",|row|row.get(0))?;
        let mut wal_path=path.as_os_str().to_os_string();
        wal_path.push("-wal");
        Ok(Self {path:wal_path.into(),limit:32+CHECKPOINT_PAGES*(u64::from(page_size)+24),snapshots:Arc::new(RwLock::new(())),blocked:AtomicBool::new(false)})
    }

    pub(super) fn blocked(&self)->bool { self.blocked.load(Ordering::Acquire) }

    pub(super) fn exceeds_limit(&self)->bool {
        match std::fs::metadata(&self.path) {
            Ok(file)=>file.len()>self.limit,
            Err(error) if error.kind()==std::io::ErrorKind::NotFound=>false,
            Err(error)=> {eprintln!("WARN: measuring database WAL: {error}");true}
        }
    }

    pub(super) fn schedule(self:&Arc<Self>,writer:Arc<Mutex<Connection>>,turn:OwnedSemaphorePermit,runtime:tokio::runtime::Handle) {
        let policy=Arc::clone(self);
        runtime.spawn(async move {
            // Writer-preferring admission prevents new snapshots while active readers drain.
            let pause=Arc::clone(&policy.snapshots).write_owned().await;
            let lease=CheckpointLease {policy:Arc::clone(&policy),finished:false,_pause:pause,_turn:turn};
            let result=tokio::task::spawn_blocking(move || {
                let mut lease=lease;
                let connection=writer.lock().unwrap_or_else(|poisoned|poisoned.into_inner());
                let result=connection.query_row("PRAGMA wal_checkpoint(TRUNCATE)",[],|row|Ok((row.get::<_,i32>(0)?,row.get::<_,i64>(1)?,row.get::<_,i64>(2)?)));
                let success=matches!(result,Ok((0,_,_)));
                lease.policy.blocked.store(!success,Ordering::Release);
                lease.finished=true;
                result
            }).await;
            match result {
                Ok(Ok((0,_,_)))=>{},
                Ok(Ok(_))=>eprintln!("WARN: database WAL checkpoint blocked; writes paused until retry succeeds"),
                Ok(Err(error))=>eprintln!("ERROR: database WAL checkpoint: {error}"),
                Err(error)=>eprintln!("ERROR: database WAL checkpoint task: {error}"),
            }
        });
    }
}

struct CheckpointLease {
    policy:Arc<Wal>,
    finished:bool,
    _pause:OwnedRwLockWriteGuard<()>,
    _turn:OwnedSemaphorePermit,
}

impl Drop for CheckpointLease {
    fn drop(&mut self) {
        if !self.finished { self.policy.blocked.store(true,Ordering::Release); }
    }
}
