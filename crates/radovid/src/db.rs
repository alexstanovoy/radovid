//! The database: one SQLite file in WAL mode behind one connection.

use std::path::Path;
use std::sync::{Arc, Mutex};

use radovid_proto::*;
use rusqlite::{Connection, OptionalExtension, Transaction, params};

use crate::rating::Glicko;

/// The schema this build writes. A file with another version is refused; there
/// is no migration.
const SCHEMA_VERSION: i64 = 1;

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
    created INTEGER NOT NULL
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
    PRIMARY KEY(account, ruleset)
);
CREATE INDEX ratings_ladder ON ratings(ruleset, rating DESC, deviation ASC);
CREATE TABLE matches(
    id INTEGER PRIMARY KEY,
    ruleset TEXT NOT NULL,
    rated INTEGER NOT NULL,
    finished INTEGER NOT NULL DEFAULT 0,
    outcome INTEGER,
    reason INTEGER,
    duration INTEGER,
    stats BLOB,
    created INTEGER NOT NULL,
    finished_at INTEGER
);
CREATE TABLE match_seats(
    match_id INTEGER NOT NULL REFERENCES matches(id),
    slot INTEGER NOT NULL,
    team INTEGER NOT NULL,
    account INTEGER NOT NULL REFERENCES accounts(id),
    hero INTEGER,
    before_rating REAL,
    before_deviation REAL,
    after_rating REAL,
    after_deviation REAL,
    PRIMARY KEY(match_id, slot)
);
CREATE INDEX match_seats_account ON match_seats(account, match_id);
";

/// Sessions unused this long are gone.
const SESSION_TTL_MS: u64 = 30 * 24 * 3600 * 1000;

#[derive(Debug)]
pub enum DbError {
    /// The login is taken.
    Taken,
    Sqlite(rusqlite::Error),
    Join,
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
        }
    }
}

/// One seat of a match being created.
#[derive(Clone, Copy, Debug)]
pub struct NewSeat {
    pub account: AccountId,
    pub slot: SlotId,
    pub team: Team,
}

/// The rating change of one seat of a rated match.
#[derive(Clone, Copy, Debug)]
pub struct RatingChange {
    pub account: AccountId,
    pub before: Glicko,
    pub after: Glicko,
    /// The seat's score: 1, 0.5 or 0.
    pub score: f64,
}

/// What a finished match leaves behind.
pub struct Finish {
    pub match_id: MatchId,
    pub ruleset: RulesetId,
    pub outcome: Outcome,
    pub reason: EndReason,
    pub duration_ticks: u32,
    pub stats: Option<MatchStats>,
    pub heroes: Vec<(SlotId, Option<HeroId>)>,
    pub changes: Vec<RatingChange>,
    pub finished_at: UnixMs,
}

#[derive(Clone)]
pub struct Db {
    conn: Arc<Mutex<Connection>>,
}

pub fn now_ms() -> UnixMs {
    let since = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    UnixMs(since.as_millis() as u64)
}

impl Db {
    pub fn open(path: &Path) -> DbResult<Db> {
        let conn = Connection::open(path)?;
        Self::init(conn)
    }

    fn init(conn: Connection) -> DbResult<Db> {
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        let version: i64 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
        match version {
            0 => {
                conn.execute_batch(SCHEMA)?;
                conn.pragma_update(None, "user_version", SCHEMA_VERSION)?;
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
        })
    }

    /// Runs `work` on the blocking pool with the connection.
    async fn run<T: Send + 'static>(
        &self,
        work: impl FnOnce(&mut Connection) -> rusqlite::Result<T> + Send + 'static,
    ) -> DbResult<T> {
        let conn = Arc::clone(&self.conn);
        tokio::task::spawn_blocking(move || {
            let mut conn = conn.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            work(&mut conn)
        })
        .await
        .map_err(|_| DbError::Join)?
        .map_err(DbError::from)
    }

    pub async fn account_by_login(&self, login: String) -> DbResult<Option<AccountRow>> {
        self.run(move |c| {
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
        self.run(move |c| {
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

    pub async fn create_session(&self, account: AccountId, token_hash: [u8; 32]) -> DbResult<()> {
        let now = now_ms().0 as i64;
        self.run(move |c| {
            c.execute(
                "INSERT INTO sessions(token_hash, account, last_used) VALUES(?1, ?2, ?3)",
                params![token_hash.as_slice(), account.0 as i64, now],
            )
            .map(|_| ())
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
        self.run(move |c| {
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
        self.run(move |c| {
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
                    params![id, seat.slot.0, team_code(seat.team), seat.account.0 as i64],
                )?;
            }
            tx.commit()?;
            Ok(MatchId(id as u64))
        })
        .await
    }

    /// Records the end of a match and its rating changes in one transaction.
    pub async fn finish_match(&self, finish: Finish) -> DbResult<()> {
        self.run(move |c| {
            let tx = c.transaction()?;
            write_finish(&tx, &finish)?;
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
                params![
                    outcome_code(Outcome::Null),
                    reason_code(EndReason::Crashed),
                    now
                ],
            )
        })
        .await
    }

    pub async fn recent_matches(
        &self,
        account: AccountId,
        limit: u8,
    ) -> DbResult<Vec<MatchResult>> {
        self.run(move |c| {
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

    pub async fn ladder(
        &self,
        ruleset: RulesetId,
        offset: u32,
        limit: u8,
    ) -> DbResult<(u32, Vec<LadderRow>)> {
        self.run(move |c| {
            let total: u32 = c.query_row(
                "SELECT COUNT(*) FROM ratings WHERE ruleset = ?1 AND games > 0",
                [&ruleset.0],
                |row| row.get(0),
            )?;
            let mut statement = c.prepare(
                "SELECT a.id, a.login, a.nickname, a.kind, a.bot_version,
                        r.rating, r.deviation, r.games, r.wins, r.draws, r.losses, r.last_played
                 FROM ratings r JOIN accounts a ON a.id = r.account
                 WHERE r.ruleset = ?1 AND r.games > 0
                 ORDER BY r.rating DESC, r.deviation ASC, a.id ASC LIMIT ?2 OFFSET ?3",
            )?;
            let rows = statement
                .query_map(params![ruleset.0, limit, offset], |row| {
                    Ok(LadderRow {
                        rank: 0,
                        account: AccountId(row.get::<_, i64>(0)? as u64),
                        login: row.get(1)?,
                        nickname: row.get(2)?,
                        kind: kind_of(row.get(3)?),
                        bot_version: row.get(4)?,
                        rating: Rating {
                            rating: row.get(5)?,
                            deviation: row.get(6)?,
                        },
                        games: row.get(7)?,
                        wins: row.get(8)?,
                        draws: row.get(9)?,
                        losses: row.get(10)?,
                        last_played: Some(UnixMs(row.get::<_, i64>(11)? as u64)),
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
        .await
    }
}

const ACCOUNT_SELECT: &str = "SELECT id, login, nickname, hash, kind, admin, banned, ban_reason,
    ban_until, bot_version FROM accounts";

fn account_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<AccountRow> {
    let banned: bool = row.get(6)?;
    Ok(AccountRow {
        id: AccountId(row.get::<_, i64>(0)? as u64),
        login: row.get(1)?,
        nickname: row.get(2)?,
        hash: row.get(3)?,
        kind: kind_of(row.get(4)?),
        admin: row.get(5)?,
        ban: banned.then(|| Ban {
            reason: row.get(7).unwrap_or_default(),
            until: row
                .get::<_, Option<i64>>(8)
                .ok()
                .flatten()
                .map(|u| UnixMs(u as u64)),
        }),
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

fn write_finish(tx: &Transaction<'_>, finish: &Finish) -> rusqlite::Result<()> {
    let stats = finish
        .stats
        .as_ref()
        .and_then(|stats| postcard::to_allocvec(stats).ok());
    let id = finish.match_id.0 as i64;
    tx.execute(
        "UPDATE matches SET finished = 1, outcome = ?2, reason = ?3, duration = ?4, stats = ?5,
         finished_at = ?6 WHERE id = ?1",
        params![
            id,
            outcome_code(finish.outcome),
            reason_code(finish.reason),
            finish.duration_ticks,
            stats,
            finish.finished_at.0 as i64
        ],
    )?;
    for (slot, hero) in &finish.heroes {
        tx.execute(
            "UPDATE match_seats SET hero = ?3 WHERE match_id = ?1 AND slot = ?2",
            params![id, slot.0, hero.map(|h| h.0)],
        )?;
    }
    for change in &finish.changes {
        let (wins, draws, losses) = match change.score {
            s if s > 0.75 => (1, 0, 0),
            s if s > 0.25 => (0, 1, 0),
            _ => (0, 0, 1),
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
    let (ruleset, rated, outcome, reason, duration, stats, finished_at) = c.query_row(
        "SELECT ruleset, rated, outcome, reason, duration, stats, finished_at FROM matches WHERE id = ?1",
        [id],
        |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, bool>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, Option<u32>>(4)?,
                row.get::<_, Option<Vec<u8>>>(5)?,
                row.get::<_, i64>(6)?,
            ))
        },
    )?;
    let mut statement = c.prepare(
        "SELECT s.slot, s.team, s.account, a.login, a.nickname, a.kind, s.hero,
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
                slot: SlotId(row.get(0)?),
                team: team_of(row.get(1)?),
                account: AccountId(row.get::<_, i64>(2)? as u64),
                login: row.get(3)?,
                nickname: row.get(4)?,
                kind: kind_of(row.get(5)?),
                hero: row.get::<_, Option<u16>>(6)?.map(HeroId),
                before: rating(7)?,
                after: rating(9)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(MatchResult {
        match_id: MatchId(id as u64),
        ruleset: RulesetId(ruleset),
        rated,
        outcome: outcome_of(outcome),
        reason: reason_of(reason),
        duration_ticks: duration.unwrap_or(0),
        stats: stats.and_then(|bytes| postcard::from_bytes(&bytes).ok()),
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

fn team_code(team: Team) -> i64 {
    match team {
        Team::Radiant => 0,
        Team::Dire => 1,
        Team::Neutral => 2,
    }
}

fn team_of(code: i64) -> Team {
    match code {
        0 => Team::Radiant,
        1 => Team::Dire,
        _ => Team::Neutral,
    }
}

fn outcome_code(outcome: Outcome) -> i64 {
    match outcome {
        Outcome::Radiant => 0,
        Outcome::Dire => 1,
        Outcome::Draw => 2,
        Outcome::Null => 3,
    }
}

fn outcome_of(code: i64) -> Outcome {
    match code {
        0 => Outcome::Radiant,
        1 => Outcome::Dire,
        2 => Outcome::Draw,
        _ => Outcome::Null,
    }
}

fn reason_code(reason: EndReason) -> i64 {
    match reason {
        EndReason::Decided => 0,
        EndReason::TimeCap => 1,
        EndReason::Crashed => 2,
        EndReason::Stopped => 3,
    }
}

fn reason_of(code: i64) -> EndReason {
    match code {
        0 => EndReason::Decided,
        1 => EndReason::TimeCap,
        2 => EndReason::Crashed,
        _ => EndReason::Stopped,
    }
}
