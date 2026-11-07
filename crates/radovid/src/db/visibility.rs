//! Materialised ranking eligibility and counts, changed with the source rows.
use super::*;

pub(super) const SCHEMA: &str = "
ALTER TABLE ratings ADD COLUMN hidden INTEGER NOT NULL DEFAULT 1 CHECK(hidden IN (0,1));
ALTER TABLE ratings ADD COLUMN nickname_key TEXT NOT NULL DEFAULT '';
UPDATE ratings SET nickname_key=(SELECT lower(nickname) FROM accounts WHERE id=ratings.account);
UPDATE ratings SET hidden=(SELECT CASE WHEN deleted!=0 OR
    (banned!=0 AND (ban_until IS NULL OR ban_until<0 OR ban_until>CAST(unixepoch('subsec')*1000 AS INTEGER)))
    THEN 1 ELSE 0 END FROM accounts WHERE id=ratings.account);
DROP TRIGGER ratings_account_kind;
DROP TRIGGER accounts_ratings_kind;
DROP INDEX ratings_kind_ladder;
DROP INDEX ratings_global_ladder;
CREATE INDEX ratings_kind_ladder ON ratings(ruleset,kind,rating DESC,deviation,account,games,hidden) WHERE games>0 AND hidden=0;
CREATE INDEX ratings_global_ladder ON ratings(ruleset,rating DESC,deviation,account,games,hidden) WHERE games>0 AND hidden=0;
CREATE INDEX ratings_nickname ON ratings(ruleset,kind,nickname_key,rating DESC,deviation,account,games,hidden) WHERE games>0 AND hidden=0;
CREATE INDEX accounts_expiring_bans ON accounts(ban_until,id) WHERE banned!=0 AND ban_until>=0;
CREATE TABLE ladder_totals(
    ruleset TEXT NOT NULL,
    kind INTEGER NOT NULL,
    total INTEGER NOT NULL CHECK(total>=0),
    PRIMARY KEY(ruleset,kind)
) WITHOUT ROWID;
INSERT INTO ladder_totals SELECT ruleset,kind,COUNT(*) FROM ratings WHERE games>0 AND hidden=0 GROUP BY ruleset,kind;
CREATE TRIGGER ratings_account_visibility AFTER INSERT ON ratings
BEGIN
    UPDATE ratings SET kind=(SELECT kind FROM accounts WHERE id=NEW.account),
        nickname_key=(SELECT lower(nickname) FROM accounts WHERE id=NEW.account),
        hidden=(SELECT CASE WHEN deleted!=0 OR (banned!=0 AND
            (ban_until IS NULL OR ban_until<0 OR ban_until>CAST(unixepoch('subsec')*1000 AS INTEGER)))
            THEN 1 ELSE 0 END FROM accounts WHERE id=NEW.account)
    WHERE account=NEW.account AND ruleset=NEW.ruleset;
END;
CREATE TRIGGER accounts_ratings_visibility AFTER UPDATE OF kind,deleted,banned,ban_until,nickname ON accounts
BEGIN
    UPDATE ratings SET kind=NEW.kind,nickname_key=lower(NEW.nickname),hidden=CASE WHEN NEW.deleted!=0 OR
        (NEW.banned!=0 AND (NEW.ban_until IS NULL OR NEW.ban_until<0 OR
            NEW.ban_until>CAST(unixepoch('subsec')*1000 AS INTEGER))) THEN 1 ELSE 0 END
    WHERE account=NEW.id;
END;
CREATE TRIGGER ratings_update_totals AFTER UPDATE OF ruleset,kind,hidden,games ON ratings
BEGIN
    UPDATE ladder_totals SET total=total-1 WHERE ruleset=OLD.ruleset AND kind=OLD.kind AND OLD.games>0 AND OLD.hidden=0;
    INSERT INTO ladder_totals SELECT NEW.ruleset,NEW.kind,1 WHERE NEW.games>0 AND NEW.hidden=0
    ON CONFLICT(ruleset,kind) DO UPDATE SET total=total+1;
END;
CREATE TRIGGER ratings_delete_totals AFTER DELETE ON ratings
BEGIN
    UPDATE ladder_totals SET total=total-1 WHERE ruleset=OLD.ruleset AND kind=OLD.kind AND OLD.games>0 AND OLD.hidden=0;
END;
";

impl Db {
    /// One tick's finite batch; permanent bans and negative stored timestamps are excluded.
    pub(crate) async fn expire_bans(&self, now: UnixMs) -> DbResult<usize> {
        self.run(move |connection| {
            connection.execute("UPDATE accounts SET banned=0 WHERE id IN (
                SELECT id FROM accounts INDEXED BY accounts_expiring_bans
                WHERE banned!=0 AND ban_until>=0 AND ban_until<=?1
                ORDER BY ban_until,id LIMIT 256)", [now.0 as i64])
        }).await
    }
}

pub(super) fn total(connection: &Connection, ruleset: &RulesetId, kind: AccountKind) -> rusqlite::Result<u32> {
    connection.query_row("SELECT total FROM ladder_totals WHERE ruleset=?1 AND kind=?2",params![ruleset.0,kind_code(kind)],|row|row.get(0)).optional().map(|count|count.unwrap_or(0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn timed_ban_sweep_restores_indexes_and_totals_in_a_bounded_batch() {
        let db=Db::init(Connection::open_in_memory().expect("database")).expect("schema");
        let now=now_ms();
        db.run(move |connection| {
            let transaction=connection.transaction()?;
            for id in 1..=260i64 {
                transaction.execute("INSERT INTO accounts(id,login,nickname,hash,kind,created,banned,ban_until) VALUES(?1,'l'||?1,'n'||?1,'hash',0,0,1,?2)",params![id,(now.0+60_000) as i64])?;
                transaction.execute("INSERT INTO ratings(account,ruleset,rating,deviation,volatility,games,wins,draws,losses,last_played) VALUES(?1,'duel',1500,350,0.06,1,1,0,0,1)",[id])?;
            }
            transaction.commit()
        }).await.expect("banned fixtures");
        assert_eq!(db.expire_bans(now).await.expect("not expired"),0);
        assert_eq!(db.expire_bans(UnixMs(now.0+60_000)).await.expect("first batch"),256);
        let rule=RulesetId("duel".into());
        let board=db.leaderboard(rule.clone(),AccountKind::Human,String::new(),100,AccountId(260)).await.expect("board");
        assert_eq!(board.total,256);
        assert!(board.own.is_none());
        assert_eq!(db.expire_bans(UnixMs(now.0+60_000)).await.expect("second batch"),4);
        let board=db.leaderboard(rule,AccountKind::Human,String::new(),100,AccountId(260)).await.expect("restored");
        assert_eq!(board.total,260);
        assert_eq!(board.own.expect("restored rank").rank,260);
    }
}
