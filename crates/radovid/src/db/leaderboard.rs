//! Mode/category ranking and profile lookups.
use super::*;

/// One expensive lookup, including all of a profile's modes, has one work budget.
const LOOKUP_VM_STEPS: u32 = 5_000_000;
const PROGRESS_INTERVAL: i32 = 1000;

pub(super) fn bounded<T>(
    connection: &Connection,
    work: impl FnOnce(&Connection) -> rusqlite::Result<T>,
) -> rusqlite::Result<T> {
    let mut remaining = LOOKUP_VM_STEPS / PROGRESS_INTERVAL as u32;
    connection.progress_handler(
        PROGRESS_INTERVAL,
        Some(move || {
            remaining = remaining.saturating_sub(1);
            remaining == 0
        }),
    )?;
    let mut guard = LookupBudget {
        connection,
        active: true,
    };
    let result = work(connection);
    guard.clear()?;
    result
}

struct LookupBudget<'a> {
    connection: &'a Connection,
    active: bool,
}

impl LookupBudget<'_> {
    fn clear(&mut self) -> rusqlite::Result<()> {
        self.connection.progress_handler(0, None::<fn() -> bool>)?;
        self.active = false;
        Ok(())
    }
}

impl Drop for LookupBudget<'_> {
    fn drop(&mut self) {
        if self.active
            && let Err(error) = self.clear()
        {
            eprintln!("ERROR: clearing database lookup work budget: {error}");
        }
    }
}

/// A single snapshot also excludes changes committed through other connections.
pub(super) fn read_lookup<T>(
    connection: &mut Connection,
    work: impl FnOnce(&Connection) -> rusqlite::Result<T>,
) -> rusqlite::Result<T> {
    let transaction = connection.transaction()?;
    let result = bounded(&transaction, work)?;
    transaction.commit()?;
    Ok(result)
}

// Rank first: prefix filtering must never renumber a search result.
const RANKED: &str = "WITH ranked AS (
    SELECT a.id, a.nickname, a.kind, a.bot_version,
           r.rating, r.deviation, r.games, r.wins, r.draws, r.losses, r.last_played,
           ROW_NUMBER() OVER (ORDER BY r.rating DESC, r.deviation ASC, a.id ASC) AS position
    FROM ratings r JOIN accounts a ON a.id = r.account
    WHERE r.ruleset = ?1 AND a.kind = ?2 AND a.deleted = 0 AND r.games > 0
)";

impl Db {
    pub async fn leaderboard(
        &self,
        ruleset: RulesetId,
        kind: AccountKind,
        prefix: String,
        limit: u16,
        requester: AccountId,
    ) -> DbResult<LeaderboardPage> {
        self.run(move |connection| {
            read_lookup(connection, |connection| {
                let limit = limit.clamp(1, MAX_LEADERBOARD_ROWS);
                let total = connection.query_row(
                    "SELECT COUNT(*) FROM ratings r JOIN accounts a ON a.id=r.account
                 WHERE r.ruleset=?1 AND a.kind=?2 AND a.deleted=0 AND r.games>0
                 AND substr(a.nickname,1,length(?3))=?3 COLLATE NOCASE",
                    params![ruleset.0, kind_code(kind), prefix],
                    |row| row.get(0),
                )?;
                let mut statement = connection.prepare(&format!(
                    "{RANKED}
                SELECT * FROM ranked WHERE substr(nickname,1,length(?3))=?3 COLLATE NOCASE
                ORDER BY position LIMIT ?4"
                ))?;
                let rows = statement
                    .query_map(
                        params![ruleset.0, kind_code(kind), prefix, limit],
                        ladder_row,
                    )?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                let own = own_row(connection, &ruleset, requester)?;
                Ok(LeaderboardPage {
                    ruleset,
                    kind,
                    prefix,
                    limit,
                    total,
                    rows,
                    own,
                })
            })
        })
        .await
    }

    pub async fn profile_ratings(
        &self,
        account: AccountId,
        rulesets: Vec<RulesetId>,
    ) -> DbResult<Vec<(RulesetId, RatingSummary)>> {
        self.run(move |connection| {
            read_lookup(connection, |connection| {
                profile_rows(connection, account, rulesets)
            })
        })
        .await
    }

    pub async fn public_profile(
        &self,
        account: AccountId,
        rulesets: Vec<RulesetId>,
    ) -> DbResult<Option<PublicAccount>> {
        self.run(move |connection| {
            read_lookup(connection, |connection| {
                let row = connection
                    .query_row(
                        &format!("{ACCOUNT_SELECT} WHERE id=?1"),
                        [account.0 as i64],
                        account_row,
                    )
                    .optional()?;
                let Some(row) = row else {
                    return Ok(None);
                };
                let ratings = profile_rows(connection, account, rulesets)?;
                Ok(Some(PublicAccount {
                    id: account,
                    nickname: row.nickname,
                    kind: row.kind,
                    online: false,
                    bot_version: row.bot_version.filter(|_| row.kind == AccountKind::Bot),
                    ratings,
                }))
            })
        })
        .await
    }
}

fn profile_rows(
    connection: &Connection,
    account: AccountId,
    rulesets: Vec<RulesetId>,
) -> rusqlite::Result<Vec<(RulesetId, RatingSummary)>> {
    if rulesets.len() > MAX_RATED_MODES {
        return Err(rusqlite::Error::InvalidParameterName(
            "profile mode limit exceeded".into(),
        ));
    }
    let mut ratings = Vec::new();
    for ruleset in rulesets {
        if let Some(row) = own_row(connection, &ruleset, account)? {
            ratings.push((
                ruleset,
                RatingSummary {
                    rating: row.rating,
                    games: row.games,
                    wins: row.wins,
                    draws: row.draws,
                    losses: row.losses,
                    rank: Some(row.rank),
                },
            ));
        }
    }
    Ok(ratings)
}

fn own_row(
    connection: &Connection,
    ruleset: &RulesetId,
    account: AccountId,
) -> rusqlite::Result<Option<LadderRow>> {
    let kind: Option<i64> = connection
        .query_row(
            "SELECT kind FROM accounts WHERE id=?1 AND deleted=0",
            [account.0 as i64],
            |row| row.get(0),
        )
        .optional()?;
    let Some(kind) = kind else {
        return Ok(None);
    };
    connection
        .query_row(
            &format!("{RANKED} SELECT * FROM ranked WHERE id=?3"),
            params![ruleset.0, kind, account.0 as i64],
            ladder_row,
        )
        .optional()
}

fn ladder_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<LadderRow> {
    Ok(LadderRow {
        account: AccountId(row.get::<_, i64>(0)? as u64),
        nickname: row.get(1)?,
        kind: kind_of(row.get(2)?),
        bot_version: row.get(3)?,
        rating: Rating {
            rating: row.get(4)?,
            deviation: row.get(5)?,
        },
        games: row.get(6)?,
        wins: row.get(7)?,
        draws: row.get(8)?,
        losses: row.get(9)?,
        last_played: Some(UnixMs(row.get::<_, i64>(10)? as u64)),
        rank: row.get(11)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> Db {
        let db = Db::init(Connection::open_in_memory().expect("database")).expect("schema");
        let connection = db.conn.lock().expect("connection");
        for (id, nickname, kind, deleted, rating, games) in [
            (1, "Alpha", 0, 0, 1900, 4),
            (2, "Beta", 0, 0, 2100, 5),
            (3, "BotOne", 1, 0, 2300, 6),
            (4, "Deleted", 0, 1, 2500, 7),
            (5, "Alpine", 0, 0, 1900, 4),
            (6, "Unplayed", 0, 0, 3000, 0),
        ] {
            connection.execute("INSERT INTO accounts(id,login,nickname,hash,kind,deleted,created) VALUES(?1,?2,?2,'fixture',?3,?4,0)", params![id,nickname,kind,deleted]).expect("account");
            connection
                .execute(
                    "INSERT INTO ratings VALUES(?1,'duel',?2,50,0.06,?3,?3,0,0,100)",
                    params![id, rating, games],
                )
                .expect("rating");
        }
        connection
            .execute(
                "INSERT INTO ratings VALUES(1,'teams',2400,50,0.06,2,2,0,0,100)",
                [],
            )
            .expect("second mode");
        drop(connection);
        db
    }

    #[tokio::test]
    async fn category_top_n_keeps_the_requesters_own_rank_outside_the_page() {
        let db = fixture();
        let page = db
            .leaderboard(
                RulesetId("duel".into()),
                AccountKind::Human,
                String::new(),
                1,
                AccountId(5),
            )
            .await
            .expect("board");
        assert_eq!(
            page.total, 3,
            "only humans with rated games, excluding deleted accounts"
        );
        assert_eq!(
            page.rows
                .iter()
                .map(|row| (row.account, row.rank))
                .collect::<Vec<_>>(),
            [(AccountId(2), 1)]
        );
        let own = page.own.expect("own row outside top N");
        assert_eq!(
            (own.account, own.rank, own.kind),
            (AccountId(5), 3, AccountKind::Human)
        );
        let bots = db
            .leaderboard(
                RulesetId("duel".into()),
                AccountKind::Bot,
                String::new(),
                100,
                AccountId(5),
            )
            .await
            .expect("bots");
        assert_eq!(
            bots.rows
                .iter()
                .map(|row| (row.account, row.rank))
                .collect::<Vec<_>>(),
            [(AccountId(3), 1)]
        );
        assert_eq!(bots.own.expect("human own row on bot view").rank, 3);
    }

    #[tokio::test]
    async fn nickname_prefix_search_is_case_insensitive_and_preserves_full_category_ranks() {
        let db = fixture();
        let page = db
            .leaderboard(
                RulesetId("duel".into()),
                AccountKind::Human,
                "aLp".into(),
                100,
                AccountId(2),
            )
            .await
            .expect("search");
        assert_eq!(page.total, 2);
        assert_eq!(
            page.rows
                .iter()
                .map(|row| (row.account, row.rank))
                .collect::<Vec<_>>(),
            [(AccountId(1), 2), (AccountId(5), 3)]
        );
        assert_eq!(page.own.expect("own row outside search").rank, 1);
    }

    #[tokio::test]
    async fn profile_ranks_are_per_mode_and_unplayed_accounts_have_no_position() {
        let db = fixture();
        let ratings = db
            .profile_ratings(
                AccountId(1),
                vec![RulesetId("duel".into()), RulesetId("teams".into())],
            )
            .await
            .expect("profile");
        assert_eq!(
            ratings
                .iter()
                .map(|(mode, rating)| (mode.0.as_str(), rating.rank))
                .collect::<Vec<_>>(),
            [("duel", Some(2)), ("teams", Some(1))]
        );
        let unplayed = db
            .leaderboard(
                RulesetId("duel".into()),
                AccountKind::Human,
                String::new(),
                100,
                AccountId(6),
            )
            .await
            .expect("board");
        assert!(unplayed.own.is_none());
    }

    #[test]
    fn guard_only_lookup_unwind_clears_the_progress_hook() {
        let connection = Connection::open_in_memory().expect("database");
        let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _: rusqlite::Result<()> = bounded(&connection, |_| panic!("test fixture unwinds"));
        }));
        assert!(panic.is_err());
        let total: i64 = connection.query_row("WITH RECURSIVE work(i) AS (VALUES(0) UNION ALL SELECT i+1 FROM work WHERE i<1000000) SELECT sum(i) FROM work", [], |row| row.get(0)).expect("an unbudgeted query must not inherit a previous lookup's hook");
        assert_eq!(total, 500000500000);
    }

    #[test]
    fn a_lookup_keeps_one_snapshot_when_another_connection_reclassifies_an_account() {
        struct Scratch(std::path::PathBuf);
        impl Drop for Scratch {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let directory = Scratch(
            std::env::temp_dir().join(format!("radovid_rank_snapshot_{}", std::process::id())),
        );
        std::fs::create_dir(&directory.0).expect("scratch");
        let db = Db::open(&directory.0.join("ranks.db")).expect("database");
        let mut connection = db.conn.lock().expect("connection");
        connection.execute("INSERT INTO accounts(id,login,nickname,hash,kind,created) VALUES(1,'alpha','Alpha','fixture',0,0)",[]).expect("account");
        connection
            .execute(
                "INSERT INTO ratings VALUES(1,'duel',1900,50,0.06,4,4,0,0,100)",
                [],
            )
            .expect("rating");
        let writer = Connection::open(&directory.0.join("ranks.db")).expect("other connection");
        read_lookup(&mut connection, |snapshot| {
            assert_eq!(
                own_row(snapshot, &RulesetId("duel".into()), AccountId(1))?
                    .expect("own")
                    .kind,
                AccountKind::Human
            );
            writer.execute("UPDATE accounts SET kind=1 WHERE id=1", [])?;
            assert_eq!(
                own_row(snapshot, &RulesetId("duel".into()), AccountId(1))?
                    .expect("same snapshot")
                    .kind,
                AccountKind::Human
            );
            Ok(())
        })
        .expect("consistent lookup");
        assert_eq!(
            own_row(&connection, &RulesetId("duel".into()), AccountId(1))
                .expect("next read")
                .expect("own")
                .kind,
            AccountKind::Bot
        );
    }

    #[test]
    fn every_statement_in_a_lookup_shares_one_work_budget_and_failure_leaves_the_database_usable() {
        let mut connection = Connection::open_in_memory().expect("database");
        let query = "WITH RECURSIVE work(i) AS (VALUES(0) UNION ALL SELECT i+1 FROM work WHERE i<100000) SELECT sum(i) FROM work";
        let mut completed = 0;
        let error = read_lookup(&mut connection, |snapshot| {
            for _ in 0..4 {
                snapshot.query_row(query, [], |row| row.get::<_, i64>(0))?;
                completed += 1;
            }
            Ok(())
        })
        .expect_err("the whole request exceeds its budget");
        assert_eq!(
            error.sqlite_error_code(),
            Some(rusqlite::ErrorCode::OperationInterrupted)
        );
        assert!(
            completed > 0 && completed < 4,
            "the counter spans statements"
        );
        let total: i64 = connection
            .query_row(query, [], |row| row.get(0))
            .expect("next query has no leaked budget");
        assert_eq!(total, 5000050000);
        assert!(
            connection.is_autocommit(),
            "the failed read transaction was rolled back"
        );
    }

    #[tokio::test]
    async fn underscore_is_literal_and_reclassification_and_deletion_move_rankings() {
        let db = fixture();
        db.run(|connection| {
            connection.execute("UPDATE accounts SET nickname='Alpha_one' WHERE id=1", [])?;
            connection.execute("UPDATE accounts SET nickname='AlphaXtwo' WHERE id=5", [])?;
            Ok(())
        })
        .await
        .expect("names");
        let page = db
            .leaderboard(
                RulesetId("duel".into()),
                AccountKind::Human,
                "ALPHA_".into(),
                100,
                AccountId(2),
            )
            .await
            .expect("literal prefix");
        assert_eq!(
            page.rows.iter().map(|row| row.account).collect::<Vec<_>>(),
            [AccountId(1)]
        );
        db.run(|connection| {
            connection
                .execute("UPDATE accounts SET kind=1 WHERE id=2", [])
                .map(|_| ())
        })
        .await
        .expect("reclassify");
        let page = db
            .leaderboard(
                RulesetId("duel".into()),
                AccountKind::Human,
                String::new(),
                100,
                AccountId(2),
            )
            .await
            .expect("reclassified");
        assert_eq!(
            page.rows
                .iter()
                .map(|row| (row.account, row.rank))
                .collect::<Vec<_>>(),
            [(AccountId(1), 1), (AccountId(5), 2)]
        );
        let own = page.own.expect("own bot rank");
        assert_eq!((own.kind, own.rank), (AccountKind::Bot, 2));
        db.delete_account(AccountId(1)).await.expect("delete");
        let page = db
            .leaderboard(
                RulesetId("duel".into()),
                AccountKind::Human,
                String::new(),
                100,
                AccountId(5),
            )
            .await
            .expect("after deletion");
        assert_eq!(page.total, 1);
        assert_eq!(page.own.expect("own").rank, 1);
    }
}
