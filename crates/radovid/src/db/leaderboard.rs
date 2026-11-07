//! Mode/category ranking and profile lookups.
use super::*;

/// A board or legacy ladder walk has one deterministic work budget.
const LOOKUP_VM_STEPS: u32 = 5_000_000;
const PROGRESS_INTERVAL: i32 = 1000;
const PROFILE_RANK_VM_STEPS:u32=500_000;
const PROFILE_METADATA_VM_STEPS:u32=10_000;

pub(super) fn bounded<T>(
    connection: &Connection,
    work: impl FnOnce(&Connection) -> rusqlite::Result<T>,
) -> rusqlite::Result<T> {
    bounded_with_limit(connection,LOOKUP_VM_STEPS,work)
}

fn bounded_with_limit<T>(connection:&Connection,steps:u32,work:impl FnOnce(&Connection)->rusqlite::Result<T>)->rusqlite::Result<T> {
    let mut remaining = steps / PROGRESS_INTERVAL as u32;
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
    read_snapshot(connection,|snapshot|bounded(snapshot,work))
}

fn read_snapshot<T>(connection:&mut Connection,work:impl FnOnce(&Connection)->rusqlite::Result<T>)->rusqlite::Result<T> {
    let transaction=connection.transaction()?;
    let result=work(&transaction)?;
    transaction.commit()?;
    Ok(result)
}

const ROW: &str = "r.account,a.nickname,r.kind,a.bot_version,r.rating,r.deviation,r.games,r.wins,r.draws,r.losses,r.last_played";
pub(super) const VISIBLE: &str = "r.games>0 AND r.hidden=0";

impl Db {
    pub async fn leaderboard(
        &self,
        ruleset: RulesetId,
        kind: AccountKind,
        prefix: String,
        limit: u16,
        requester: AccountId,
    ) -> DbResult<LeaderboardPage> {
        if !valid_nickname_prefix(&prefix) {
            return Err(DbError::Sqlite(rusqlite::Error::InvalidParameterName(
                "invalid nickname prefix".into(),
            )));
        }
        self.read(move |connection| {
            read_lookup(connection, |connection| {
                let limit = limit.clamp(1, MAX_LEADERBOARD_ROWS);
                let visible = VISIBLE;
                let (total, mut rows) = if prefix.is_empty() {
                    category_top(connection, &ruleset, kind, limit, visible)?
                } else {
                    prefix_top(connection, &ruleset, kind, &prefix, limit, visible)?
                };
                let mut own = find_own_row(connection, &ruleset, requester, visible)?;
                if let Some(row) = &mut own
                    && (prefix.is_empty() || rows.is_empty() || row.kind != kind)
                {
                    row.rank = rank_of(connection, &ruleset, row, visible)?;
                }
                if !prefix.is_empty() {
                    rank_selected(connection, &ruleset, kind, &mut rows, &mut own, visible)?;
                }
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

    pub async fn public_profile(
        &self,
        account: AccountId,
        rulesets: Vec<RulesetId>,
        requester: AccountId,
    ) -> DbResult<Option<PublicAccount>> {
        self.read(move |connection| {
            read_snapshot(connection, |connection| {
                let now = now_ms();
                let row = bounded_with_limit(connection, PROFILE_METADATA_VM_STEPS, |connection| connection
                    .query_row(
                        &format!("{ACCOUNT_SELECT} WHERE id=?1"),
                        [account.0 as i64],
                        account_row,
                    )
                    .optional())?;
                let Some(row) = row else {
                    return Ok(None);
                };
                if requester != account && row.active_ban(now).is_some() {
                    return Ok(None);
                }
                let ratings = profile_rows(connection, account, rulesets, VISIBLE, PROFILE_RANK_VM_STEPS)?;
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
    visible: &str,
    rank_budget:u32,
) -> rusqlite::Result<Vec<(RulesetId, RatingSummary)>> {
    if rulesets.len() > MAX_RATED_MODES {
        return Err(rusqlite::Error::InvalidParameterName(
            "profile mode limit exceeded".into(),
        ));
    }
    let mut ratings = Vec::new();
    for ruleset in rulesets {
        if let Some(row) = bounded_with_limit(connection,PROFILE_METADATA_VM_STEPS,|connection|find_own_row(connection,&ruleset,account,visible))? {
            let rank = match bounded_with_limit(connection,rank_budget,|connection|rank_of(connection,&ruleset,&row,visible)) {
                Ok(rank)=>Some(rank),
                Err(error) if error.sqlite_error_code()==Some(rusqlite::ErrorCode::OperationInterrupted)=>None,
                Err(error)=>return Err(error),
            };
            ratings.push((
                ruleset,
                RatingSummary {
                    rating: row.rating,
                    games: row.games,
                    wins: row.wins,
                    draws: row.draws,
                    losses: row.losses,
                    rank,
                },
            ));
        }
    }
    Ok(ratings)
}

fn category_top(
    connection: &Connection,
    ruleset: &RulesetId,
    kind: AccountKind,
    limit: u16,
    visible: &str,
) -> rusqlite::Result<(u32, Vec<LadderRow>)> {
    let total = super::visibility::total(connection, ruleset, kind)?;
    let mut statement=connection.prepare(&format!("SELECT {ROW},0 FROM ratings r INDEXED BY ratings_kind_ladder CROSS JOIN accounts a ON a.id=r.account WHERE r.ruleset=?1 AND r.kind=?2 AND {visible} ORDER BY r.rating DESC,r.deviation,r.account LIMIT ?3"))?;
    let mut rows = statement
        .query_map(params![ruleset.0, kind_code(kind), limit], ladder_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for (index, row) in rows.iter_mut().enumerate() {
        row.rank = index as u32 + 1;
    }
    Ok((total, rows))
}

fn prefix_top(
    connection: &Connection,
    ruleset: &RulesetId,
    kind: AccountKind,
    prefix: &str,
    limit: u16,
    visible: &str,
) -> rusqlite::Result<(u32, Vec<LadderRow>)> {
    let lower = prefix.to_ascii_lowercase();
    let mut upper = lower.as_bytes().to_vec();
    if let Some(last) = upper.last_mut() {
        *last += 1;
    }
    let upper = String::from_utf8(upper).map_err(|_| rusqlite::Error::InvalidQuery)?;
    let from = format!(
        "FROM ratings r INDEXED BY ratings_nickname WHERE r.ruleset=?1 AND r.kind=?2 AND {visible} AND r.nickname_key>=?3 AND r.nickname_key<?4"
    );
    let total = connection.query_row(
        &format!("SELECT COUNT(*) {from}"),
        params![ruleset.0, kind_code(kind), lower, upper],
        |row| row.get(0),
    )?;
    let mut statement = connection.prepare(&format!(
        "WITH picked AS MATERIALIZED (SELECT r.account {from} ORDER BY r.rating DESC,r.deviation,r.account LIMIT ?5)
        SELECT {ROW},0 FROM picked p CROSS JOIN ratings r ON r.account=p.account AND r.ruleset=?1
        CROSS JOIN accounts a ON a.id=r.account ORDER BY r.rating DESC,r.deviation,r.account"
    ))?;
    let rows = statement
        .query_map(
            params![ruleset.0, kind_code(kind), lower, upper, limit],
            ladder_row,
        )?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok((total, rows))
}

// One ordered index pass ranks selected search rows; counting separately per row is quadratic.
fn rank_selected(
    connection: &Connection,
    ruleset: &RulesetId,
    kind: AccountKind,
    picked: &mut [LadderRow],
    own: &mut Option<LadderRow>,
    visible: &str,
) -> rusqlite::Result<()> {
    let mut remaining: std::collections::HashMap<_, _> = picked
        .iter()
        .enumerate()
        .map(|(index, row)| (row.account, index))
        .collect();
    if remaining.is_empty() {
        return Ok(());
    }
    let mut own_pending = own
        .as_ref()
        .is_some_and(|row| row.kind == kind && row.rank == 0);
    let mut statement=connection.prepare(&format!("SELECT r.account FROM ratings r INDEXED BY ratings_kind_ladder WHERE r.ruleset=?1 AND r.kind=?2 AND {visible} ORDER BY r.rating DESC,r.deviation,r.account"))?;
    let mut cursor = statement.query(params![ruleset.0, kind_code(kind)])?;
    let mut rank = 0u32;
    while let Some(row) = cursor.next()? {
        rank = rank.checked_add(1).ok_or(rusqlite::Error::InvalidQuery)?;
        let account = AccountId(row.get::<_, i64>(0)? as u64);
        if let Some(index) = remaining.remove(&account) {
            picked[index].rank = rank;
        }
        if own_pending && let Some(row) = own.as_mut().filter(|row| row.account == account) {
            row.rank = rank;
            own_pending = false;
        }
        if remaining.is_empty() && !own_pending {
            break;
        }
    }
    Ok(())
}

fn rank_of(
    connection: &Connection,
    ruleset: &RulesetId,
    row: &LadderRow,
    visible: &str,
) -> rusqlite::Result<u32> {
    let mut rank = 1u32;
    for range in [
        "r.rating>?3",
        "r.rating=?3 AND r.deviation<?4",
        "r.rating=?3 AND r.deviation=?4 AND r.account<?5",
    ] {
        let sql = format!(
            "SELECT COUNT(*) FROM ratings r INDEXED BY ratings_kind_ladder WHERE r.ruleset=?1 AND r.kind=?2 AND {visible} AND {range} AND ?4 IS NOT NULL AND ?5 IS NOT NULL"
        );
        let count: u32 = connection.query_row(
            &sql,
            params![
                ruleset.0,
                kind_code(row.kind),
                row.rating.rating,
                row.rating.deviation,
                row.account.0 as i64
            ],
            |row| row.get(0),
        )?;
        rank = rank
            .checked_add(count)
            .ok_or(rusqlite::Error::InvalidQuery)?;
    }
    Ok(rank)
}

fn find_own_row(
    connection: &Connection,
    ruleset: &RulesetId,
    account: AccountId,
    visible: &str,
) -> rusqlite::Result<Option<LadderRow>> {
    connection.query_row(&format!("SELECT {ROW},0 FROM ratings r CROSS JOIN accounts a ON a.id=r.account WHERE r.ruleset=?1 AND r.account=?2 AND {visible}"),params![ruleset.0,account.0 as i64],ladder_row).optional()
}

fn ladder_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<LadderRow> {
    let kind = kind_of(row.get(2)?);
    Ok(LadderRow {
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
        rank: row.get(11)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_keeps_each_modes_summary_when_its_rank_budget_is_exhausted() {
        let db=Db::init(Connection::open_in_memory().expect("database")).expect("schema");
        let mut connection=db.conn.lock().expect("writer");
        {
            let transaction=connection.transaction().expect("seed");
            for id in 1..=4096i64 {
                transaction.execute("INSERT INTO accounts(id,login,nickname,hash,kind,created) VALUES(?1,'l'||?1,'a'||?1,'fixture',0,0)",[id]).expect("account");
                for mode in ["deep","other"] {
                    transaction.execute("INSERT INTO ratings(account,ruleset,rating,deviation,volatility,games,wins,draws,losses,last_played) VALUES(?1,?2,?3,50,0.06,2,1,0,1,1)",params![id,mode,1000+id]).expect("rating");
                }
            }
            transaction.execute("INSERT INTO ratings(account,ruleset,rating,deviation,volatility,games,wins,draws,losses,last_played) VALUES(1,'tiny',1800,50,0.06,2,1,0,1,1)",[]).expect("cheap rank");
            transaction.commit().expect("commit");
        }
        let modes=["deep","other","tiny"].map(|name|RulesetId(name.into())).to_vec();
        let ratings=read_snapshot(&mut connection,|snapshot|profile_rows(snapshot,AccountId(1),modes,VISIBLE,1000)).expect("profile renders");
        assert_eq!(ratings.len(),3);
        assert_eq!(ratings.iter().map(|(_,summary)|summary.rank).collect::<Vec<_>>(),[None,None,Some(1)]);
        assert!(ratings.iter().all(|(_,summary)|summary.games==2 && summary.wins==1 && summary.losses==1));
    }

    #[test]
    fn guard_only_board_walks_have_at_most_five_million_sqlite_instructions() {
        const {
            assert!(
                LOOKUP_VM_STEPS <= 5_000_000,
                "work limit must not be enlarged to mask a linear join"
            );
        }
    }

    #[tokio::test]
    async fn banned_profiles_are_hidden_from_others_and_expired_bans_restore_rank() {
        let db = fixture();
        db.set_ban(
            AccountId(2),
            Some(Ban {
                reason: "fixture".into(),
                until: None,
            }),
        )
        .await
        .expect("ban");
        let profile = db
            .public_profile(AccountId(2), vec![RulesetId("duel".into())], AccountId(1))
            .await
            .expect("profile lookup");
        assert!(
            profile.is_none(),
            "other accounts cannot see a banned profile"
        );
        let own = db
            .public_profile(AccountId(2), vec![RulesetId("duel".into())], AccountId(2))
            .await
            .expect("own profile")
            .expect("own metadata");
        assert!(own.ratings.is_empty(), "a banned requester is not ranked");
        let (_, legacy) = db
            .ladder(RulesetId("duel".into()), 0, 100)
            .await
            .expect("legacy board");
        assert!(
            legacy.iter().all(|row| row.account != AccountId(2)),
            "legacy views hide the same ban"
        );
        let page = db
            .leaderboard(
                RulesetId("duel".into()),
                AccountKind::Human,
                "Be".into(),
                100,
                AccountId(2),
            )
            .await
            .expect("search");
        assert_eq!(page.total, 0);
        assert!(page.rows.is_empty());
        assert!(page.own.is_none(), "a banned requester has no ranked row");
        db.set_ban(
            AccountId(2),
            Some(Ban {
                reason: "future".into(),
                until: Some(UnixMs(u64::MAX)),
            }),
        )
        .await
        .expect("future ban");
        let future = db
            .leaderboard(
                RulesetId("duel".into()),
                AccountKind::Human,
                String::new(),
                100,
                AccountId(2),
            )
            .await
            .expect("future ban lookup");
        assert!(future.own.is_none());
        db.set_ban(
            AccountId(2),
            Some(Ban {
                reason: "expired".into(),
                until: Some(UnixMs(1)),
            }),
        )
        .await
        .expect("expired ban");
        let page = db
            .leaderboard(
                RulesetId("duel".into()),
                AccountKind::Human,
                String::new(),
                100,
                AccountId(2),
            )
            .await
            .expect("expired");
        assert_eq!(page.total, 3);
        assert_eq!(page.own.expect("rank restored").rank, 1);
    }

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
                    "INSERT INTO ratings(account,ruleset,rating,deviation,volatility,games,wins,draws,losses,last_played) VALUES(?1,'duel',?2,50,0.06,?3,?3,0,0,100)",
                    params![id, rating, games],
                )
                .expect("rating");
        }
        connection
            .execute(
                "INSERT INTO ratings(account,ruleset,rating,deviation,volatility,games,wins,draws,losses,last_played) VALUES(1,'teams',2400,50,0.06,2,2,0,0,100)",
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
            .public_profile(
                AccountId(1),
                vec![RulesetId("duel".into()), RulesetId("teams".into())],
                AccountId(1),
            )
            .await
            .expect("profile")
            .expect("account")
            .ratings;
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
        let total: i64 = connection.query_row("WITH RECURSIVE work(i) AS (VALUES(0) UNION ALL SELECT i+1 FROM work WHERE i<4000000) SELECT sum(i) FROM work", [], |row| row.get(0)).expect("an unbudgeted query must not inherit a previous lookup's hook");
        assert_eq!(total, 8000002000000);
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
                "INSERT INTO ratings(account,ruleset,rating,deviation,volatility,games,wins,draws,losses,last_played) VALUES(1,'duel',1900,50,0.06,4,4,0,0,100)",
                [],
            )
            .expect("rating");
        let writer = Connection::open(directory.0.join("ranks.db")).expect("other connection");
        read_lookup(&mut connection, |snapshot| {
            assert_eq!(
                find_own_row(snapshot, &RulesetId("duel".into()), AccountId(1), VISIBLE)?
                    .expect("own")
                    .kind,
                AccountKind::Human
            );
            writer.execute("UPDATE accounts SET kind=1 WHERE id=1", [])?;
            assert_eq!(
                find_own_row(snapshot, &RulesetId("duel".into()), AccountId(1), VISIBLE)?
                    .expect("same snapshot")
                    .kind,
                AccountKind::Human
            );
            Ok(())
        })
        .expect("consistent lookup");
        assert_eq!(
            find_own_row(
                &connection,
                &RulesetId("duel".into()),
                AccountId(1),
                VISIBLE
            )
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

#[cfg(test)]
mod scale_tests {
    use super::*;

    fn populate(db: &Db, modes: &[&str], accounts: i64) {
        let mut connection = db.conn.lock().expect("connection");
        let transaction = connection.transaction().expect("tx");
        for id in 1..=accounts {
            transaction
                .execute(
                    "INSERT INTO accounts(id,login,nickname,hash,kind,created) VALUES(?1,'l'||?1,'n'||?1,'h',0,0)",
                    [id],
                )
                .expect("account");
            for mode in modes {
                transaction
                    .execute(
                        "INSERT INTO ratings(account,ruleset,rating,deviation,volatility,games,wins,draws,losses,last_played) VALUES(?1,?2,?3,50,0.06,1,1,0,0,100)",
                        params![id, mode, 1000 + (id * 7919) % 2000],
                    )
                    .expect("rating");
            }
        }
        transaction.commit().expect("commit");
    }

    /// Red: a category of a realistic public size must still serve its top-N board.
    #[tokio::test]
    async fn probe_board_still_served_at_100k_rated_humans() {
        for accounts in [10_000i64, 20_000, 50_000, 100_000] {
            let db = Db::init(Connection::open_in_memory().expect("db")).expect("schema");
            populate(&db, &["duel"], accounts);
            let started = std::time::Instant::now();
            let result = db
                .leaderboard(
                    RulesetId("duel".into()),
                    AccountKind::Human,
                    String::new(),
                    100,
                    AccountId(1),
                )
                .await;
            eprintln!(
                "PROBE board accounts={accounts} ok={} elapsed={:?}",
                result.is_ok(),
                started.elapsed()
            );
            assert!(
                result.is_ok(),
                "top board refused at {accounts}: {:?}",
                result.err()
            );
            assert!(
                started.elapsed() < std::time::Duration::from_secs(2),
                "indexed board exceeds its latency bound"
            );
            let started = std::time::Instant::now();
            let result = db
                .leaderboard(
                    RulesetId("duel".into()),
                    AccountKind::Human,
                    "n1".into(),
                    100,
                    AccountId(1),
                )
                .await;
            eprintln!(
                "PROBE search accounts={accounts} ok={} elapsed={:?}",
                result.is_ok(),
                started.elapsed()
            );
            assert!(
                result.is_ok(),
                "search refused at {accounts} accounts: {:?}",
                result.err()
            );
            assert!(
                started.elapsed() < std::time::Duration::from_secs(2),
                "indexed search exceeds its latency bound"
            );
        }
    }

    /// Red: a profile with several played modes of moderate size.
    #[tokio::test]
    async fn probe_profile_with_8_modes_at_10k_accounts() {
        let modes = ["m0", "m1", "m2", "m3", "m4", "m5", "m6", "m7"];
        let db = Db::init(Connection::open_in_memory().expect("db")).expect("schema");
        populate(&db, &modes, 10_000);
        let started = std::time::Instant::now();
        let result = db
            .public_profile(
                AccountId(1),
                modes.iter().map(|m| RulesetId((*m).into())).collect(),
                AccountId(1),
            )
            .await;
        eprintln!(
            "PROBE profile ok={} elapsed={:?}",
            result.is_ok(),
            started.elapsed()
        );
        assert!(result.is_ok(), "profile refused: {:?}", result.err());
    }

    /// Red if banned accounts should be hidden: a banned account is ranked and listed.
    #[tokio::test]
    async fn probe_banned_account_is_not_listed() {
        let db = Db::init(Connection::open_in_memory().expect("db")).expect("schema");
        populate(&db, &["duel"], 3);
        db.run(|c| {
            c.execute(
                "UPDATE accounts SET banned=1, ban_reason='cheat' WHERE id=2",
                [],
            )
            .map(|_| ())
        })
        .await
        .expect("ban");
        let page = db
            .leaderboard(
                RulesetId("duel".into()),
                AccountKind::Human,
                String::new(),
                100,
                AccountId(1),
            )
            .await
            .expect("board");
        assert!(
            page.rows.iter().all(|row| row.account != AccountId(2)),
            "banned account listed: {:?}",
            page.rows
                .iter()
                .map(|r| (r.account, r.rank))
                .collect::<Vec<_>>()
        );
    }

    /// Red: a human row must not carry a bot version (profile already filters it).
    #[tokio::test]
    async fn probe_reclassified_human_row_has_no_bot_version() {
        let db = Db::init(Connection::open_in_memory().expect("db")).expect("schema");
        populate(&db, &["duel"], 2);
        db.run(|c| {
            c.execute(
                "UPDATE accounts SET bot_version='secret-build-7' WHERE id=2",
                [],
            )
            .map(|_| ())
        })
        .await
        .expect("ver");
        let page = db
            .leaderboard(
                RulesetId("duel".into()),
                AccountKind::Human,
                String::new(),
                100,
                AccountId(1),
            )
            .await
            .expect("board");
        assert!(
            page.rows.iter().all(|row| row.bot_version.is_none()),
            "human row leaks bot_version"
        );
        let (_, rows) = db
            .ladder(RulesetId("duel".into()), 0, 100)
            .await
            .expect("legacy ladder");
        assert!(
            rows.iter()
                .filter(|row| row.kind == AccountKind::Human)
                .all(|row| row.bot_version.is_none()),
            "legacy human row leaks bot_version"
        );
    }
}
