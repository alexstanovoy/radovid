//! One WAL database, one account/match writer and a bounded read-only pool.

mod results;
mod leaderboard;
mod reader;
#[cfg(test)]
mod schema_tests;
mod visibility;
mod wal;
#[cfg(test)]
mod wal_tests;
#[cfg(test)]
mod results_tests;

use std::path::Path;
use std::sync::{Arc, Mutex};

use radovid_proto::*;
use rusqlite::{Connection, OptionalExtension, Transaction, params};

use crate::rating::Glicko;

/// Schema 2 gains the denormalised category and indexes in one transaction.
const SCHEMA_VERSION: i64 = 4;

const SCHEMA: &str = "
CREATE TABLE accounts(
    id INTEGER PRIMARY KEY,
    login TEXT NOT NULL UNIQUE,
    nickname TEXT NOT NULL,
    hash TEXT NOT NULL,
    kind INTEGER NOT NULL,
    admin INTEGER NOT NULL DEFAULT 0,
    banned INTEGER NOT NULL DEFAULT 0,
    ban_reason TEXT NOT NULL DEFAULT '',
    ban_until INTEGER,
    bot_version TEXT,
    created INTEGER NOT NULL,
    deleted INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE sessions(
    token_hash BLOB PRIMARY KEY,
    account INTEGER NOT NULL REFERENCES accounts(id),
    last_used INTEGER NOT NULL
);
CREATE INDEX sessions_account ON sessions(account);
CREATE TABLE ratings(
    account INTEGER NOT NULL REFERENCES accounts(id),
    ruleset TEXT NOT NULL,
    rating REAL NOT NULL,
    deviation REAL NOT NULL,
    volatility REAL NOT NULL,
    games INTEGER NOT NULL,
    wins INTEGER NOT NULL,
    draws INTEGER NOT NULL,
    losses INTEGER NOT NULL,
    last_played INTEGER NOT NULL,
    kind INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY(account, ruleset)
);
CREATE INDEX ratings_ladder ON ratings(ruleset, rating DESC, deviation ASC);
CREATE TABLE matches(
    id INTEGER PRIMARY KEY,
    ruleset TEXT NOT NULL,
    rated INTEGER NOT NULL,
    finished INTEGER NOT NULL DEFAULT 0,
    outcome INTEGER,
    winner INTEGER,
    reason INTEGER,
    duration_ms INTEGER,
    report BLOB,
    created INTEGER NOT NULL,
    finished_at INTEGER
);
CREATE TABLE match_seats(
    match_id INTEGER NOT NULL REFERENCES matches(id),
    slot INTEGER NOT NULL,
    team INTEGER NOT NULL,
    account INTEGER NOT NULL REFERENCES accounts(id),
    before_rating REAL,
    before_deviation REAL,
    after_rating REAL,
    after_deviation REAL,
    PRIMARY KEY(match_id, slot)
);
CREATE INDEX match_seats_account ON match_seats(account, match_id);
";

const RANKING_SCHEMA: &str = "
CREATE INDEX ratings_kind_ladder ON ratings(ruleset,kind,rating DESC,deviation,account) WHERE games>0;
CREATE INDEX ratings_global_ladder ON ratings(ruleset,rating DESC,deviation,account) WHERE games>0;
CREATE INDEX accounts_nickname_key ON accounts(lower(nickname),id) WHERE deleted=0;
CREATE TRIGGER ratings_account_kind AFTER INSERT ON ratings
WHEN NEW.kind != (SELECT kind FROM accounts WHERE id=NEW.account)
BEGIN UPDATE ratings SET kind=(SELECT kind FROM accounts WHERE id=NEW.account)
      WHERE account=NEW.account AND ruleset=NEW.ruleset; END;
CREATE TRIGGER accounts_ratings_kind AFTER UPDATE OF kind ON accounts
BEGIN UPDATE ratings SET kind=NEW.kind WHERE account=NEW.id; END;
";

/// What a deleted account is shown as.
pub const DELETED_NICKNAME: &str = "deleted";

/// Session tokens one account keeps stored at most; a login past it drops the oldest.
pub const MAX_STORED_SESSIONS: usize = 32;

/// Sessions unused this long are gone.
const SESSION_TTL_MS: u64 = 30 * 24 * 3600 * 1000;

#[derive(Debug)]
pub enum DbError {
    /// The login is taken.
    Taken,
    Sqlite(rusqlite::Error),
    Join,
    ReadBusy,
    WalLimit,
    Io(std::io::Error),
}

impl From<rusqlite::Error> for DbError {
    fn from(error: rusqlite::Error) -> Self {
        DbError::Sqlite(error)
    }
}

impl std::fmt::Display for DbError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DbError::Taken => write!(f, "the login is taken"),
            DbError::Sqlite(error) => write!(f, "database: {error}"),
            DbError::Join => write!(f, "database task failed"),
            DbError::ReadBusy => write!(f, "database read queue is full"),
            DbError::WalLimit => {
                write!(f, "database WAL hard limit reached; write not executed")
            }
            DbError::Io(error) => write!(f, "database file: {error}"),
        }
    }
}

impl std::error::Error for DbError {}

pub type DbResult<T> = Result<T, DbError>;

#[derive(Clone, Debug)]
pub struct Ban {
    pub reason: String,
    pub until: Option<UnixMs>,
}

#[derive(Clone, Debug)]
pub struct AccountRow {
    pub id: AccountId,
    pub login: String,
    pub nickname: String,
    pub hash: String,
    pub kind: AccountKind,
    pub admin: bool,
    pub ban: Option<Ban>,
    pub bot_version: Option<String>,
}

impl AccountRow {
    pub fn info(&self) -> AccountInfo {
        AccountInfo {
            id: self.id,
            login: self.login.clone(),
            nickname: self.nickname.clone(),
            kind: self.kind,
            admin: self.admin,
        }
    }

    /// The ban in force at `now`, if any.
    pub fn active_ban(&self, now: UnixMs) -> Option<&Ban> {
        self.ban
            .as_ref()
            .filter(|ban| ban.until.is_none_or(|until| until > now))
    }
}

#[derive(Clone, Copy, Debug)]
pub struct RatingRow {
    pub glicko: Glicko,
    pub games: u32,
    pub wins: u32,
    pub draws: u32,
    pub losses: u32,
    pub last_played: UnixMs,
}

impl RatingRow {
    pub fn summary(&self) -> RatingSummary {
        RatingSummary {
            rating: self.glicko.public(),
            games: self.games,
            wins: self.wins,
            draws: self.draws,
            losses: self.losses,
            rank: None,
        }
    }
}

/// One seat of a match being created.
#[derive(Clone, Copy, Debug, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct NewSeat {
    pub account: AccountId,
    pub slot: u8,
    pub team: u8,
}

/// The rating change of one seat of a rated match.
#[derive(Clone, Copy, Debug)]
pub struct RatingChange {
    pub account: AccountId,
    pub before: Glicko,
    pub after: Glicko,
}

/// What a finished match leaves behind.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct Finish {
    pub match_id: MatchId,
    pub ruleset: RulesetId,
    /// Rate the seats: a rated match with a winner or a draw.
    pub rate: bool,
    pub outcome: Outcome,
    pub reason: EndReason,
    pub duration_ms: u32,
    #[serde(deserialize_with="results::report")]
    pub report: Vec<u8>,
    /// Slot by slot.
    #[serde(deserialize_with="results::seats")]
    pub seats: Vec<NewSeat>,
    pub finished_at: UnixMs,
}

#[derive(Clone)]
pub struct Db {
    conn: Arc<Mutex<Connection>>,
    readers: Option<Arc<reader::ReadPool>>,
    writer_turn: Arc<tokio::sync::Semaphore>,
    wal: Option<Arc<wal::Wal>>,
    results:Option<Arc<results::Results>>,
}

/// Validates a bounded durable-result record without opening files.
pub fn decode_pending_result(bytes:&[u8])->DbResult<(u64,Finish)> {results::decode_record(bytes)}

pub fn now_ms() -> UnixMs {
    let since = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    UnixMs(since.as_millis() as u64)
}

impl Db {
    pub(crate) async fn recover_results(&self)->DbResult<usize> {
        let Some(journal)=self.results.clone() else {return Ok(0);};
        let mut recovered=0;
        for _ in 0..=results::MAX_PENDING {
            let reader=Arc::clone(&journal);
            let next=tokio::task::spawn_blocking(move||reader.first()).await.map_err(|_|DbError::Join)??;
            let Some(finish)=next else {return Ok(recovered);};
            if recovered>=results::MAX_PENDING {return Err(DbError::Io(std::io::Error::other("too many pending results")));}
            let id=finish.match_id;
            self.commit_finish(finish).await?;
            self.ack_finish(id).await?;
            recovered+=1;
        }
        Ok(recovered)
    }

    pub(crate) async fn queue_finish(&self,finish:Finish)->DbResult<()> {
        let Some(journal)=self.results.clone() else {return Ok(());};
        tokio::task::spawn_blocking(move||journal.save(&finish)).await.map_err(|_|DbError::Join)?
    }

    async fn ack_finish(&self,id:MatchId)->DbResult<()> {
        let Some(journal)=self.results.clone() else {return Ok(());};
        tokio::task::spawn_blocking(move||journal.remove(id)).await.map_err(|_|DbError::Join)?
    }

    pub(crate) async fn completed_result(&self,id:MatchId)->DbResult<MatchResult> {
        self.inspect(move|connection|load_result(connection,id.0 as i64)).await
    }
    #[cfg(test)]
    fn lookup_pending_for_test(&self) -> usize {
        self.readers
            .as_ref()
            .map_or(0, |pool| pool.pending_for_test())
    }
    #[cfg(test)]
    pub(crate) fn lookup_connection_for_test(&self) -> &Mutex<Connection> {
        self.readers
            .as_ref()
            .map_or(self.conn.as_ref(), |readers| readers.first_connection())
    }

    pub fn open(path: &Path) -> DbResult<Db> {
        let conn = Connection::open(path)?;
        let mut db = Self::init(conn)?;
        let (resolved, wal) = {
            let connection = db
                .conn
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let actual = connection
                .path()
                .filter(|path| !path.is_empty())
                .ok_or_else(|| DbError::Sqlite(rusqlite::Error::InvalidPath(path.to_owned())))?;
            let resolved = std::fs::canonicalize(actual).map_err(DbError::Io)?;
            let wal = Arc::new(wal::Wal::new(&resolved, &connection)?);
            (resolved, wal)
        };
        db.readers = Some(Arc::new(reader::ReadPool::open(
            &resolved,
            Arc::clone(&wal.snapshots),
        )?));
        db.results=Some(Arc::new(results::Results::open(&resolved)?));
        db.wal = Some(wal);
        Ok(db)
    }

    fn init(mut conn: Connection) -> DbResult<Db> {
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        // Checkpoints use the dedicated short-timeout connection, never a foreground commit.
        conn.pragma_update(None,"wal_autocheckpoint",0)?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        let version: i64 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
        match version {
            0 => {
                let transaction = conn.transaction()?;
                transaction.execute_batch(SCHEMA)?;
                transaction.execute_batch(RANKING_SCHEMA)?;
                transaction.execute_batch(visibility::SCHEMA)?;
                transaction.pragma_update(None, "user_version", SCHEMA_VERSION)?;
                transaction.commit()?;
            }
            2 => {
                let transaction = conn.transaction()?;
                transaction.execute_batch(
                    "ALTER TABLE ratings ADD COLUMN kind INTEGER NOT NULL DEFAULT 0;
                    UPDATE ratings SET kind=(SELECT kind FROM accounts WHERE id=ratings.account);",
                )?;
                transaction.execute_batch(RANKING_SCHEMA)?;
                transaction.execute_batch(visibility::SCHEMA)?;
                transaction.pragma_update(None, "user_version", SCHEMA_VERSION)?;
                transaction.commit()?;
            }
            3 => {
                let transaction = conn.transaction()?;
                transaction.execute_batch(visibility::SCHEMA)?;
                transaction.pragma_update(None, "user_version", SCHEMA_VERSION)?;
                transaction.commit()?;
            }
            SCHEMA_VERSION => {}
            other => {
                return Err(DbError::Sqlite(rusqlite::Error::InvalidParameterName(
                    format!("database schema version {other}, this build writes {SCHEMA_VERSION}"),
                )));
            }
        }
        Ok(Db {
            conn: Arc::new(Mutex::new(conn)),
            readers: None,
            writer_turn: Arc::new(tokio::sync::Semaphore::new(1)),
            wal: None,
            results:None,
        })
    }

    /// Runs `work` on the blocking pool with the connection.
    async fn run<T: Send + 'static>(
        &self,
        work: impl FnOnce(&mut Connection) -> rusqlite::Result<T> + Send + 'static,
    ) -> DbResult<T> {
        self.run_inner(work,true).await
    }

    async fn inspect<T:Send+'static>(&self,work:impl FnOnce(&mut Connection)->rusqlite::Result<T>+Send+'static)->DbResult<T> {self.run_inner(work,false).await}

    async fn run_inner<T:Send+'static>(&self,work:impl FnOnce(&mut Connection)->rusqlite::Result<T>+Send+'static,mutation:bool)->DbResult<T> {
        let conn = Arc::clone(&self.conn);
        let _turn = Arc::clone(&self.writer_turn)
            .acquire_owned()
            .await
            .map_err(|_| DbError::Join)?;
        let wal = self.wal.clone();
        let runtime = tokio::runtime::Handle::current();
        tokio::task::spawn_blocking(move || {
            let _turn=_turn;
            if mutation && let Some(policy) = &wal && policy.exceeds_hard_limit() {
                policy.schedule(runtime);
                return Err(DbError::WalLimit);
            }
            let (result, checkpoint) = {
                let mut connection = conn.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
                let result = work(&mut connection);
                let checkpoint = connection.is_autocommit()
                    && wal.as_ref().is_some_and(|policy| policy.exceeds_limit());
                (result, checkpoint)
            };
            if checkpoint && let Some(policy) = wal {
                policy.schedule(runtime);
            }
            result.map_err(DbError::from)
        })
        .await
        .map_err(|_| DbError::Join)?
    }

    async fn read<T: Send + 'static>(
        &self,
        work: impl FnOnce(&mut Connection) -> rusqlite::Result<T> + Send + 'static,
    ) -> DbResult<T> {
        match &self.readers {
            Some(readers) => readers.run(work).await,
            None => self.inspect(work).await,
        }
    }

    pub async fn account_by_login(&self, login: String) -> DbResult<Option<AccountRow>> {
        self.inspect(move |c| {
            c.query_row(
                &format!("{ACCOUNT_SELECT} WHERE login = ?1"),
                [&login],
                account_row,
            )
            .optional()
        })
        .await
    }

    pub async fn account_by_id(&self, id: AccountId) -> DbResult<Option<AccountRow>> {
        self.inspect(move |c| {
            c.query_row(
                &format!("{ACCOUNT_SELECT} WHERE id = ?1"),
                [id.0 as i64],
                account_row,
            )
            .optional()
        })
        .await
    }

    pub async fn create_account(
        &self,
        login: String,
        hash: String,
        kind: AccountKind,
        bot_version: Option<String>,
    ) -> DbResult<AccountId> {
        let created = now_ms().0 as i64;
        let result = self
            .run(move |c| {
                c.execute(
                    "INSERT INTO accounts(login, nickname, hash, kind, bot_version, created)
                     VALUES(?1, ?1, ?2, ?3, ?4, ?5)",
                    params![login, hash, kind_code(kind), bot_version, created],
                )?;
                Ok(AccountId(c.last_insert_rowid() as u64))
            })
            .await;
        match result {
            Err(DbError::Sqlite(rusqlite::Error::SqliteFailure(e, _)))
                if e.code == rusqlite::ErrorCode::ConstraintViolation =>
            {
                Err(DbError::Taken)
            }
            other => other,
        }
    }

    pub async fn set_nickname(&self, id: AccountId, nickname: String) -> DbResult<()> {
        self.exec(
            "UPDATE accounts SET nickname = ?2 WHERE id = ?1",
            move |s| s.execute(params![id.0 as i64, nickname]),
        )
        .await
    }

    pub async fn set_password(&self, id: AccountId, hash: String) -> DbResult<()> {
        self.exec("UPDATE accounts SET hash = ?2 WHERE id = ?1", move |s| {
            s.execute(params![id.0 as i64, hash])
        })
        .await
    }

    pub async fn set_kind(&self, id: AccountId, kind: AccountKind) -> DbResult<()> {
        self.exec("UPDATE accounts SET kind = ?2 WHERE id = ?1", move |s| {
            s.execute(params![id.0 as i64, kind_code(kind)])
        })
        .await
    }

    pub async fn set_bot_version(&self, id: AccountId, version: Option<String>) -> DbResult<()> {
        self.exec(
            "UPDATE accounts SET bot_version = ?2 WHERE id = ?1",
            move |s| s.execute(params![id.0 as i64, version]),
        )
        .await
    }

    pub async fn set_ban(&self, id: AccountId, ban: Option<Ban>) -> DbResult<()> {
        self.exec(
            "UPDATE accounts SET banned = ?2, ban_reason = ?3, ban_until = ?4 WHERE id = ?1",
            move |s| {
                let (banned, reason, until) = match ban {
                    Some(ban) => (1, ban.reason, ban.until.map(|u| u.0 as i64)),
                    None => (0, String::new(), None),
                };
                s.execute(params![id.0 as i64, banned, reason, until])
            },
        )
        .await
    }

    /// Makes the account with this login an admin; whether there was one.
    pub async fn grant_admin(&self, login: String) -> DbResult<bool> {
        self.run(move |c| {
            c.execute("UPDATE accounts SET admin = 1 WHERE login = ?1", [login])
                .map(|n| n == 1)
        })
        .await
    }

    async fn exec(
        &self,
        sql: &'static str,
        bind: impl FnOnce(&mut rusqlite::Statement<'_>) -> rusqlite::Result<usize> + Send + 'static,
    ) -> DbResult<()> {
        self.run(move |c| {
            let mut statement = c.prepare_cached(sql)?;
            bind(&mut statement).map(|_| ())
        })
        .await
    }

    /// Stores a fresh session of the account, and lets go of its oldest past
    /// [`MAX_STORED_SESSIONS`] and of its expired ones: every login stores one, so without this
    /// a script that logs in again and again would grow the file without end.
    pub async fn create_session(&self, account: AccountId, token_hash: [u8; 32]) -> DbResult<()> {
        let now = now_ms().0;
        let expired_before = now.saturating_sub(SESSION_TTL_MS) as i64;
        self.run(move |c| {
            let tx = c.transaction()?;
            tx.execute(
                "INSERT INTO sessions(token_hash, account, last_used) VALUES(?1, ?2, ?3)",
                params![token_hash.as_slice(), account.0 as i64, now as i64],
            )?;
            tx.execute(
                "DELETE FROM sessions WHERE account = ?1 AND (last_used < ?2 OR rowid NOT IN
                     (SELECT rowid FROM sessions WHERE account = ?1
                      ORDER BY last_used DESC, rowid DESC LIMIT ?3))",
                params![account.0 as i64, expired_before, MAX_STORED_SESSIONS as i64],
            )?;
            tx.commit()
        })
        .await
    }

    /// The account a token belongs to, if the token is known and fresh; a use
    /// refreshes it.
    pub async fn resume_session(&self, token_hash: [u8; 32]) -> DbResult<Option<AccountId>> {
        let now = now_ms().0;
        self.run(move |c| {
            let tx = c.transaction()?;
            let row: Option<(i64, i64)> = tx
                .query_row(
                    "SELECT account, last_used FROM sessions WHERE token_hash = ?1",
                    [token_hash.as_slice()],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()?;
            let found = match row {
                Some((account, last_used))
                    if now.saturating_sub(last_used as u64) < SESSION_TTL_MS =>
                {
                    tx.execute(
                        "UPDATE sessions SET last_used = ?2 WHERE token_hash = ?1",
                        params![token_hash.as_slice(), now as i64],
                    )?;
                    Some(AccountId(account as u64))
                }
                Some(_) => {
                    tx.execute(
                        "DELETE FROM sessions WHERE token_hash = ?1",
                        [token_hash.as_slice()],
                    )?;
                    None
                }
                None => None,
            };
            tx.commit()?;
            Ok(found)
        })
        .await
    }

    pub async fn revoke_session(&self, token_hash: [u8; 32]) -> DbResult<()> {
        self.run(move |c| {
            c.execute(
                "DELETE FROM sessions WHERE token_hash = ?1",
                [token_hash.as_slice()],
            )
            .map(|_| ())
        })
        .await
    }

    /// Revokes every token of the account but one.
    pub async fn revoke_other_sessions(&self, account: AccountId, keep: [u8; 32]) -> DbResult<()> {
        self.run(move |c| {
            c.execute(
                "DELETE FROM sessions WHERE account = ?1 AND token_hash != ?2",
                params![account.0 as i64, keep.as_slice()],
            )
            .map(|_| ())
        })
        .await
    }

    pub async fn rating(
        &self,
        account: AccountId,
        ruleset: RulesetId,
    ) -> DbResult<Option<RatingRow>> {
        self.inspect(move |c| {
            c.query_row(
                &format!(
                    "SELECT {RATING_COLUMNS} FROM ratings WHERE account = ?1 AND ruleset = ?2"
                ),
                params![account.0 as i64, ruleset.0],
                |row| rating_row_at(row, 0),
            )
            .optional()
        })
        .await
    }

    pub async fn ratings_of(&self, account: AccountId) -> DbResult<Vec<(RulesetId, RatingRow)>> {
        self.inspect(move |c| {
            let mut statement = c.prepare(&format!(
                "SELECT ruleset, {RATING_COLUMNS} FROM ratings WHERE account = ?1 ORDER BY ruleset"
            ))?;
            statement
                .query_map([account.0 as i64], |row| {
                    Ok((RulesetId(row.get(0)?), rating_row_at(row, 1)?))
                })?
                .collect()
        })
        .await
    }

    /// A pending match with its seats.
    pub async fn create_match(
        &self,
        ruleset: RulesetId,
        rated: bool,
        seats: Vec<NewSeat>,
    ) -> DbResult<MatchId> {
        let now = now_ms().0 as i64;
        self.run(move |c| {
            let tx = c.transaction()?;
            tx.execute(
                "INSERT INTO matches(ruleset, rated, created) VALUES(?1, ?2, ?3)",
                params![ruleset.0, rated, now],
            )?;
            let id = tx.last_insert_rowid();
            for seat in &seats {
                tx.execute(
                    "INSERT INTO match_seats(match_id, slot, team, account) VALUES(?1, ?2, ?3, ?4)",
                    params![id, seat.slot, seat.team, seat.account.0 as i64],
                )?;
            }
            tx.commit()?;
            Ok(MatchId(id as u64))
        })
        .await
    }

    /// Records the end of a match and, for a rated one, the Glicko-2 update
    /// of every member against the opposing teams as stored now, in one
    /// transaction (DEVELOPMENT.md "Ratings"). Returns the rating changes.
    pub async fn finish_match(&self,finish:Finish)->DbResult<Vec<RatingChange>> {
        let id=finish.match_id;
        self.queue_finish(finish.clone()).await?;
        let changes=self.commit_finish(finish).await?;
        if let Err(error)=self.ack_finish(id).await {eprintln!("WARN: match {} committed; result-inbox cleanup will retry: {error}",id.0);}
        Ok(changes)
    }

    async fn commit_finish(&self,finish:Finish)->DbResult<Vec<RatingChange>> {
        self.run(move |connection| {
            // Pending records may be removed only after the main WAL reaches stable storage.
            connection.pragma_update(None,"synchronous","FULL")?;
            let result=commit_finish_once(connection,&finish);
            let restored=connection.pragma_update(None,"synchronous","NORMAL");
            match result {Ok(changes)=>{restored?;Ok(changes)},Err(error)=>Err(error)}
        }).await
    }

    /// Deletes the credentials, sessions and ratings of an account; its
    /// matches stay, under an anonymous name.
    pub async fn delete_account(&self, id: AccountId) -> DbResult<()> {
        self.run(move |c| {
            let tx = c.transaction()?;
            let id = id.0 as i64;
            tx.execute("DELETE FROM sessions WHERE account = ?1", [id])?;
            tx.execute("DELETE FROM ratings WHERE account = ?1", [id])?;
            tx.execute(
                "UPDATE accounts SET login = 'deleted#' || id, nickname = ?2, hash = '',
                 bot_version = NULL, admin = 0, deleted = 1 WHERE id = ?1",
                params![id, DELETED_NICKNAME],
            )?;
            tx.commit()
        })
        .await
    }

    /// Marks every match left unfinished by a stopped service as lost.
    /// Returns how many.
    pub async fn fail_unfinished_matches(&self) -> DbResult<usize> {
        let now = now_ms().0 as i64;
        self.run(move |c| {
            c.execute(
                "UPDATE matches SET finished = 1, outcome = ?1, reason = ?2, finished_at = ?3
                 WHERE finished = 0",
                params![OUTCOME_NULL, reason_code(EndReason::Crashed), now],
            )
        })
        .await
    }

    pub async fn recent_matches(
        &self,
        account: AccountId,
        limit: u8,
    ) -> DbResult<Vec<MatchResult>> {
        self.inspect(move |c| {
            let mut ids = c.prepare(
                "SELECT m.id FROM matches m JOIN match_seats s ON s.match_id = m.id
                 WHERE s.account = ?1 AND m.finished = 1 ORDER BY m.id DESC LIMIT ?2",
            )?;
            let ids: Vec<i64> = ids
                .query_map(params![account.0 as i64, limit], |row| row.get(0))?
                .collect::<Result<_, _>>()?;
            ids.into_iter().map(|id| load_result(c, id)).collect()
        })
        .await
    }

    /// The ruleset and the accounts of a finished match.
    pub async fn finished_match_seats(
        &self,
        match_id: MatchId,
    ) -> DbResult<Option<(RulesetId, Vec<AccountId>)>> {
        self.inspect(move |c| {
            let ruleset: Option<String> = c
                .query_row(
                    "SELECT ruleset FROM matches WHERE id = ?1 AND finished = 1",
                    [match_id.0 as i64],
                    |row| row.get(0),
                )
                .optional()?;
            let Some(ruleset) = ruleset else {
                return Ok(None);
            };
            let mut seats = c.prepare("SELECT account FROM match_seats WHERE match_id = ?1")?;
            let accounts = seats
                .query_map([match_id.0 as i64], |row| {
                    Ok(AccountId(row.get::<_, i64>(0)? as u64))
                })?
                .collect::<Result<_, _>>()?;
            Ok(Some((RulesetId(ruleset), accounts)))
        })
        .await
    }

    pub async fn ladder(
        &self,
        ruleset: RulesetId,
        offset: u32,
        limit: u8,
    ) -> DbResult<(u32, Vec<LadderRow>)> {
        self.read(move |connection| {
            leaderboard::read_lookup(connection, |c| {
                let visible = leaderboard::VISIBLE;
                let humans = visibility::total(c, &ruleset, AccountKind::Human)?;
                let bots = visibility::total(c, &ruleset, AccountKind::Bot)?;
                let total = humans
                    .checked_add(bots)
                    .ok_or(rusqlite::Error::InvalidQuery)?;
                let mut statement = c.prepare(&format!(
                    "WITH picked AS MATERIALIZED (
                        SELECT account FROM ratings r INDEXED BY ratings_global_ladder
                        WHERE r.ruleset=?1 AND {visible}
                        ORDER BY r.rating DESC,r.deviation,r.account LIMIT ?2 OFFSET ?3)
                    SELECT a.id,a.nickname,a.kind,a.bot_version,r.rating,r.deviation,
                        r.games,r.wins,r.draws,r.losses,r.last_played
                    FROM picked p CROSS JOIN ratings r ON r.account=p.account AND r.ruleset=?1
                    CROSS JOIN accounts a ON a.id=r.account
                    ORDER BY r.rating DESC,r.deviation,r.account"
                ))?;
                let rows = statement
                    .query_map(params![ruleset.0, limit, offset], |row| {
                        let kind = kind_of(row.get(2)?);
                        Ok(LadderRow {
                            rank: 0,
                            account: AccountId(row.get::<_, i64>(0)? as u64),
                            nickname: row.get(1)?,
                            kind,
                            bot_version: if kind == AccountKind::Bot {
                                row.get(3)?
                            } else {
                                None
                            },
                            rating: Rating {
                                rating: row.get(4)?,
                                deviation: row.get(5)?,
                            },
                            games: row.get(6)?,
                            wins: row.get(7)?,
                            draws: row.get(8)?,
                            losses: row.get(9)?,
                            last_played: Some(UnixMs(row.get::<_, i64>(10)? as u64)),
                        })
                    })?
                    .collect::<Result<Vec<_>, _>>()?;
                let rows = rows
                    .into_iter()
                    .enumerate()
                    .map(|(i, mut row)| {
                        row.rank = offset + i as u32 + 1;
                        row
                    })
                    .collect();
                Ok((total, rows))
            })
        })
        .await
    }
}

const ACCOUNT_SELECT: &str = "SELECT id, login, nickname, hash, kind, admin, banned, ban_reason,
    ban_until, bot_version FROM accounts";

fn account_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<AccountRow> {
    let ban = if row.get::<_, bool>(6)? {
        Some(Ban {
            reason: row.get(7)?,
            until: row.get::<_, Option<i64>>(8)?.map(|u| UnixMs(u as u64)),
        })
    } else {
        None
    };
    Ok(AccountRow {
        id: AccountId(row.get::<_, i64>(0)? as u64),
        login: row.get(1)?,
        nickname: row.get(2)?,
        hash: row.get(3)?,
        kind: kind_of(row.get(4)?),
        admin: row.get(5)?,
        ban,
        bot_version: row.get(9)?,
    })
}

const RATING_COLUMNS: &str =
    "rating, deviation, volatility, games, wins, draws, losses, last_played";

fn rating_row_at(row: &rusqlite::Row<'_>, at: usize) -> rusqlite::Result<RatingRow> {
    Ok(RatingRow {
        glicko: Glicko {
            rating: row.get(at)?,
            deviation: row.get(at + 1)?,
            volatility: row.get(at + 2)?,
        },
        games: row.get(at + 3)?,
        wins: row.get(at + 4)?,
        draws: row.get(at + 5)?,
        losses: row.get(at + 6)?,
        last_played: UnixMs(row.get::<_, i64>(at + 7)? as u64),
    })
}

/// Snapshot every member before calculating any update: concurrent games use
/// arrival order, while teammates in this transaction see the same opponents.
fn rate(tx: &Transaction<'_>, finish: &Finish) -> rusqlite::Result<Vec<RatingChange>> {
    if finish.outcome == Outcome::Null {
        return Ok(Vec::new());
    }
    validate_rated_match(finish, finish.seats.len())?;
    let mut standing = Vec::new();
    for seat in &finish.seats {
        let row: Option<RatingRow> = tx
            .query_row(
                &format!(
                    "SELECT {RATING_COLUMNS} FROM ratings WHERE account = ?1 AND ruleset = ?2"
                ),
                params![seat.account.0 as i64, finish.ruleset.0],
                |row| rating_row_at(row, 0),
            )
            .optional()?;
        standing.push(row.map_or(Glicko::NEW, |row| {
            let idle_ms = finish.finished_at.0.saturating_sub(row.last_played.0);
            row.glicko.after_idle(idle_ms as f64 / 86_400_000.0)
        }));
    }
    rate_teams(finish, &standing)
}

fn rate_teams(finish: &Finish, standing: &[Glicko]) -> rusqlite::Result<Vec<RatingChange>> {
    validate_rated_match(finish, standing.len())?;
    let mut teams = std::collections::BTreeMap::<u8, Vec<Glicko>>::new();
    for (seat, rating) in finish.seats.iter().zip(standing) {
        teams.entry(seat.team).or_default().push(*rating);
    }
    let opponents: Vec<_> = teams
        .into_iter()
        .map(|(team, members)| {
            if members.len() == 1 {
                return (team, members[0]);
            }
            let count = members.len() as f64;
            (
                team,
                Glicko {
                    rating: members.iter().map(|r| r.rating).sum::<f64>() / count,
                    deviation: members
                        .iter()
                        .map(|r| r.deviation.powi(2))
                        .sum::<f64>()
                        .sqrt()
                        / count,
                    volatility: crate::rating::INITIAL_VOLATILITY,
                },
            )
        })
        .collect();
    finish
        .seats
        .iter()
        .zip(standing)
        .map(|(seat, &before)| {
            let games: Vec<_> = opponents
                .iter()
                .filter(|(team, _)| *team != seat.team)
                .map(|&(team, rating)| {
                    let score = match finish.outcome {
                        Outcome::Winner { team: winner } if winner == seat.team => 1.0,
                        Outcome::Winner { team: winner } if winner == team => 0.0,
                        _ => 0.5,
                    };
                    (rating, score)
                })
                .collect();
            Ok(RatingChange {
                account: seat.account,
                before,
                after: before.updated_match(&games),
            })
        })
        .collect()
}

fn validate_rated_match(finish: &Finish, standing_count: usize) -> rusqlite::Result<()> {
    let fail = |message: &str| Err(rusqlite::Error::InvalidParameterName(message.into()));
    if !(2..=MAX_SEATS).contains(&finish.seats.len()) || finish.seats.len() != standing_count {
        return fail("rated match needs 2 to 10 matching seats and ratings");
    }
    if let Outcome::Winner { team } = finish.outcome
        && !finish.seats.iter().any(|seat| seat.team == team)
    {
        return fail("rated match winner has no seat");
    }
    let mut accounts = std::collections::BTreeSet::new();
    let mut teams = std::collections::BTreeSet::new();
    for seat in &finish.seats {
        if !accounts.insert(seat.account) {
            return fail("rated match repeats an account");
        }
        teams.insert(seat.team);
    }
    if teams.len() < 2 {
        return fail("rated match needs at least two seated sides");
    }
    Ok(())
}

fn write_finish(
    tx: &Transaction<'_>,
    finish: &Finish,
    changes: &[RatingChange],
) -> rusqlite::Result<()> {
    let id = finish.match_id.0 as i64;
    let (outcome, winner) = outcome_code(finish.outcome);
    tx.execute(
        "UPDATE matches SET finished = 1, outcome = ?2, winner = ?3, reason = ?4,
         duration_ms = ?5, report = ?6, finished_at = ?7 WHERE id = ?1",
        params![
            id,
            outcome,
            winner,
            reason_code(finish.reason),
            finish.duration_ms,
            finish.report,
            finish.finished_at.0 as i64
        ],
    )?;
    for change in changes {
        let seat_team = finish
            .seats
            .iter()
            .find(|s| s.account == change.account)
            .map(|s| s.team);
        let (wins, draws, losses) = match finish.outcome {
            Outcome::Winner { team } if Some(team) == seat_team => (1, 0, 0),
            Outcome::Winner { .. } => (0, 0, 1),
            _ => (0, 1, 0),
        };
        tx.execute(
            "INSERT INTO ratings(account, ruleset, rating, deviation, volatility, games, wins,
                                 draws, losses, last_played)
             VALUES(?1, ?2, ?3, ?4, ?5, 1, ?6, ?7, ?8, ?9)
             ON CONFLICT(account, ruleset) DO UPDATE SET
                rating = ?3, deviation = ?4, volatility = ?5, games = games + 1,
                wins = wins + ?6, draws = draws + ?7, losses = losses + ?8, last_played = ?9",
            params![
                change.account.0 as i64,
                finish.ruleset.0,
                change.after.rating,
                change.after.deviation,
                change.after.volatility,
                wins,
                draws,
                losses,
                finish.finished_at.0 as i64
            ],
        )?;
        tx.execute(
            "UPDATE match_seats SET before_rating = ?3, before_deviation = ?4, after_rating = ?5,
             after_deviation = ?6 WHERE match_id = ?1 AND account = ?2",
            params![
                id,
                change.account.0 as i64,
                change.before.rating,
                change.before.deviation,
                change.after.rating,
                change.after.deviation
            ],
        )?;
    }
    Ok(())
}

fn load_result(c: &Connection, id: i64) -> rusqlite::Result<MatchResult> {
    let (ruleset, rated, outcome, winner, reason, duration, report, finished_at) = c.query_row(
        "SELECT ruleset, rated, outcome, winner, reason, duration_ms, report, finished_at
         FROM matches WHERE id = ?1",
        [id],
        |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, bool>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, Option<u8>>(3)?,
                row.get::<_, i64>(4)?,
                row.get::<_, Option<u32>>(5)?,
                row.get::<_, Option<Vec<u8>>>(6)?,
                row.get::<_, i64>(7)?,
            ))
        },
    )?;
    let mut statement = c.prepare(
        "SELECT s.slot, s.team, s.account, a.nickname, a.kind,
                s.before_rating, s.before_deviation, s.after_rating, s.after_deviation
         FROM match_seats s JOIN accounts a ON a.id = s.account WHERE s.match_id = ?1 ORDER BY s.slot",
    )?;
    let seats = statement
        .query_map([id], |row| {
            let rating = |at: usize| -> rusqlite::Result<Option<Rating>> {
                let rating: Option<f64> = row.get(at)?;
                let deviation: Option<f64> = row.get(at + 1)?;
                Ok(rating
                    .zip(deviation)
                    .map(|(rating, deviation)| Rating { rating, deviation }))
            };
            Ok(SeatResult {
                slot: row.get(0)?,
                team: row.get(1)?,
                account: AccountId(row.get::<_, i64>(2)? as u64),
                nickname: row.get(3)?,
                kind: kind_of(row.get(4)?),
                before: rating(5)?,
                after: rating(7)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(MatchResult {
        match_id: MatchId(id as u64),
        ruleset: RulesetId(ruleset),
        rated,
        outcome: outcome_of(outcome, winner),
        reason: reason_of(reason),
        duration_ms: duration.unwrap_or(0),
        game_report: report.unwrap_or_default(),
        seats,
        finished_at: UnixMs(finished_at as u64),
    })
}

fn kind_code(kind: AccountKind) -> i64 {
    match kind {
        AccountKind::Human => 0,
        AccountKind::Bot => 1,
    }
}

fn kind_of(code: i64) -> AccountKind {
    if code == 1 {
        AccountKind::Bot
    } else {
        AccountKind::Human
    }
}

const OUTCOME_WINNER: i64 = 0;
const OUTCOME_DRAW: i64 = 1;
const OUTCOME_NULL: i64 = 2;

/// The outcome column and the winner column.
fn outcome_code(outcome: Outcome) -> (i64, Option<u8>) {
    match outcome {
        Outcome::Winner { team } => (OUTCOME_WINNER, Some(team)),
        Outcome::Draw => (OUTCOME_DRAW, None),
        Outcome::Null => (OUTCOME_NULL, None),
    }
}

fn outcome_of(code: i64, winner: Option<u8>) -> Outcome {
    match (code, winner) {
        (OUTCOME_WINNER, Some(team)) => Outcome::Winner { team },
        (OUTCOME_DRAW, _) => Outcome::Draw,
        _ => Outcome::Null,
    }
}

fn reason_code(reason: EndReason) -> i64 {
    match reason {
        EndReason::Decided => 0,
        EndReason::Draw => 1,
        EndReason::Abandoned => 2,
        EndReason::Crashed => 3,
        EndReason::Stopped => 4,
        EndReason::Left => 5,
    }
}

fn reason_of(code: i64) -> EndReason {
    match code {
        0 => EndReason::Decided,
        1 => EndReason::Draw,
        2 => EndReason::Abandoned,
        3 => EndReason::Crashed,
        5 => EndReason::Left,
        _ => EndReason::Stopped,
    }
}


fn commit_finish_once(connection:&mut Connection,finish:&Finish)->rusqlite::Result<Vec<RatingChange>> {
            let transaction=connection.transaction()?;
            let (finished,ruleset,rated):(bool,String,bool)=transaction.query_row("SELECT finished,ruleset,rated FROM matches WHERE id=?1",[finish.match_id.0 as i64],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?)))?;
            if finished {return Ok(Vec::new());}
            if ruleset!=finish.ruleset.0 || (finish.rate && !rated) {return Err(rusqlite::Error::InvalidQuery);}
            let reserved=transaction.prepare("SELECT account,slot,team FROM match_seats WHERE match_id=?1 ORDER BY slot")?.query_map([finish.match_id.0 as i64],|row|Ok(NewSeat{account:AccountId(row.get::<_,i64>(0)? as u64),slot:row.get(1)?,team:row.get(2)?}))?.collect::<rusqlite::Result<Vec<_>>>()?;
            let mut seats=finish.seats.clone();seats.sort_by_key(|seat|seat.slot);
            if seats!=reserved {return Err(rusqlite::Error::InvalidQuery);}
            let changes=if finish.rate {rate(&transaction,finish)?}else{Vec::new()};
            write_finish(&transaction,finish,&changes)?;
            transaction.commit()?;
            Ok(changes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn standing(rating: f64, deviation: f64) -> Glicko {
        Glicko {
            rating,
            deviation,
            ..Glicko::NEW
        }
    }

    fn finish(teams: &[u8], outcome: Outcome) -> Finish {
        Finish {
            match_id: MatchId(1),
            ruleset: RulesetId("test".into()),
            rate: true,
            outcome,
            reason: EndReason::Decided,
            duration_ms: 1,
            report: Vec::new(),
            seats: teams
                .iter()
                .enumerate()
                .map(|(slot, &team)| NewSeat {
                    account: AccountId(slot as u64 + 1),
                    slot: slot as u8,
                    team,
                })
                .collect(),
            finished_at: UnixMs(0),
        }
    }

    #[test]
    fn unequal_teams_rate_each_member_against_each_opposing_mean_once() {
        // These form means of 1600/RD50, 1500/RD100 and 1800/RD200.
        // In particular, the two losing sides draw each other, never lose twice.
        let before = [
            standing(2200.0, 60.0),
            standing(1000.0, 80.0),
            standing(1400.0, 120.0),
            standing(1600.0, 160.0),
            standing(1800.0, 200.0),
        ];
        let a = standing(1600.0, 50.0);
        let b = standing(1500.0, 100.0);
        let c = standing(1800.0, 200.0);
        for (outcome, scores) in [
            (
                Outcome::Winner { team: 1 },
                [(0.0, 0.5), (1.0, 1.0), (0.5, 0.0)],
            ),
            (Outcome::Draw, [(0.5, 0.5); 3]),
        ] {
            let finish = finish(&[0, 0, 1, 1, 2], outcome);
            let changes = rate_teams(&finish, &before).expect("valid match");
            let expected = [
                before[0].updated_match(&[(b, scores[0].0), (c, scores[0].1)]),
                before[1].updated_match(&[(b, scores[0].0), (c, scores[0].1)]),
                before[2].updated_match(&[(a, scores[1].0), (c, scores[1].1)]),
                before[3].updated_match(&[(a, scores[1].0), (c, scores[1].1)]),
                before[4].updated_match(&[(a, scores[2].0), (b, scores[2].1)]),
            ];
            for (change, expected) in changes.iter().zip(expected) {
                assert_eq!(change.after, expected);
            }
        }
    }

    /// Guard-only: captured from origin/main 2f21ea7's rating.rs using a standalone
    /// release-optimized harness. These fixed values do not call the expected-value implementation.
    #[test]
    fn one_on_one_rating_matches_origin_main_goldens() {
        let before = [standing(1743.258, 83.649), standing(1302.115, 312.872)];
        let cases = [
            (
                Outcome::Winner { team: 0 },
                [
                    [1747.3099515716492, 83.69490829308198, 0.0599993179215],
                    [1266.7860551195884, 283.3002328334886, 0.05999930046076021],
                ],
            ),
            (
                Outcome::Winner { team: 1 },
                [
                    [1718.6966703287535, 83.69500785664277, 0.06000405353881476],
                    [1713.3264260384049, 283.3002633672034, 0.0600064225681048],
                ],
            ),
            (
                Outcome::Draw,
                [
                    [1733.003336479686, 83.69492334580887, 0.06000003390938952],
                    [1490.0562044810486, 283.30023903024943, 0.060000745947484],
                ],
            ),
        ];
        for (outcome, expected) in cases {
            let changes = rate_teams(&finish(&[0, 1], outcome), &before).expect("valid match");
            for (change, golden) in changes.iter().zip(expected) {
                assert!((change.after.rating - golden[0]).abs() < 1e-10);
                assert!((change.after.deviation - golden[1]).abs() < 1e-10);
                assert!((change.after.volatility - golden[2]).abs() < 1e-14);
            }
        }
    }

    /// Guard-only: verifies the maximum supported shape, rather than reproducing an incident.
    #[test]
    fn ten_sides_produce_one_finite_update_per_account() {
        let teams: Vec<_> = (0..MAX_SEATS as u8).collect();
        let changes = rate_teams(&finish(&teams, Outcome::Draw), &[Glicko::NEW; MAX_SEATS])
            .expect("valid match");
        assert_eq!(changes.len(), MAX_SEATS);
        for change in changes {
            assert_eq!(change.after.rating, 1500.0);
            assert!(change.after.deviation.is_finite());
            assert!(change.after.volatility.is_finite());
        }
    }
    #[test]
    fn malformed_rated_finish_returns_an_error_without_unwinding_the_transaction() {
        let mut connection = Connection::open_in_memory().expect("database");
        connection.execute_batch(SCHEMA).expect("schema");
        let tx = connection.transaction().expect("transaction");
        let invalid = finish(&[0, 1], Outcome::Winner { team: 2 });
        let error = rate(&tx, &invalid).expect_err("unseated winner must be rejected");
        assert_eq!(
            error.to_string(),
            "Invalid parameter name: rated match winner has no seat"
        );
        assert_eq!(
            tx.query_row("SELECT 1", [], |row| row.get::<_, i64>(0))
                .expect("transaction remains usable"),
            1
        );
    }

    #[test]
    fn one_ffa_match_carries_the_same_information_as_one_duel() {
        let duel = rate_teams(&finish(&[0, 1], Outcome::Draw), &[Glicko::NEW; 2]).expect("duel");
        for sides in 3..=MAX_SEATS {
            let teams: Vec<_> = (0..sides as u8).collect();
            let ffa =
                rate_teams(&finish(&teams, Outcome::Draw), &vec![Glicko::NEW; sides]).expect("FFA");
            assert!(
                (ffa[0].after.deviation - duel[0].after.deviation).abs() < 1e-10,
                "{sides}-side FFA RD {} versus duel {}",
                ffa[0].after.deviation,
                duel[0].after.deviation
            );
            assert!((ffa[0].after.volatility - duel[0].after.volatility).abs() < 1e-12);
        }
    }
}
