//! The public match list uses the same visibility snapshot and bounded readers as profiles.
use super::*;
use std::collections::{BTreeMap, btree_map::Entry};

pub(super) fn account_visible(
    connection: &Connection,
    account: AccountId,
    requester: AccountId,
    now: UnixMs,
) -> rusqlite::Result<bool> {
    connection
        .query_row(
            "SELECT 1 FROM accounts a WHERE a.id=?1 AND a.deleted=0 AND
         (?1=?2 OR ((a.banned=0 OR a.ban_until BETWEEN 0 AND ?3) AND
           (NOT EXISTS(SELECT 1 FROM ratings r WHERE r.account=a.id AND r.games>0) OR
            EXISTS(SELECT 1 FROM ratings r WHERE r.account=a.id AND r.games>0 AND r.hidden=0))))",
            params![account.0 as i64, requester.0 as i64, now.0 as i64],
            |_| Ok(true),
        )
        .optional()
        .map(|value| value.unwrap_or(false))
}

impl Db {
    pub(crate) async fn published_accounts(
        &self,
        accounts: Vec<AccountId>,
    ) -> DbResult<Vec<Option<AccountId>>> {
        if accounts.len() > MAX_SEATS {
            return Err(DbError::Sqlite(rusqlite::Error::InvalidQuery));
        }
        self.read(move |connection| {
            super::leaderboard::read_lookup(connection, |connection| {
                let now = now_ms();
                accounts
                    .into_iter()
                    .map(|account| {
                        account_visible(connection, account, AccountId(0), now)
                            .map(|visible| visible.then_some(account))
                    })
                    .collect()
            })
        })
        .await
    }

    pub(crate) async fn public_recent_matches(
        &self,
        account: AccountId,
        requester: AccountId,
        limit: u8,
        modes: Vec<RulesetId>,
    ) -> DbResult<Option<Vec<MatchResult>>> {
        self.read(move |connection| {
            super::leaderboard::read_lookup(connection, |connection| {
                public_matches(connection, account, requester, limit, &modes)
            })
        })
        .await
    }
}

fn public_matches(
    connection: &Connection,
    account: AccountId,
    requester: AccountId,
    limit: u8,
    modes: &[RulesetId],
) -> rusqlite::Result<Option<Vec<MatchResult>>> {
    let now = now_ms();
    if !account_visible(connection, account, requester, now)? {
        return Ok(None);
    }
    let mut statement=connection.prepare("SELECT m.id FROM matches m JOIN match_seats s ON s.match_id=m.id WHERE s.account=?1 AND m.finished=1 ORDER BY m.id DESC LIMIT ?2")?;
    let ids = statement
        .query_map(
            params![account.0 as i64, limit.min(MAX_RECENT_MATCHES)],
            |row| row.get::<_, i64>(0),
        )?
        .collect::<Result<Vec<_>, _>>()?;
    let mut visibility = BTreeMap::new();
    let mut matches = Vec::with_capacity(ids.len());
    for id in ids {
        // A participant keeps their own opaque report; everyone else gets the list alone.
        let mut result = load_result_projection(connection, id, account == requester)?;
        for seat in &mut result.seats {
            if seat.account == requester {
                continue;
            }
            let key = (seat.account.0, result.ruleset.0.clone());
            let (identity_visible, rating_visible) = match visibility.entry(key) {
                Entry::Occupied(entry) => *entry.get(),
                Entry::Vacant(entry) => {
                    let identity = account_visible(connection, seat.account, AccountId(0), now)?;
                    let rating = identity
                        && visible_rating(connection, seat.account, &result.ruleset, modes)?;
                    *entry.insert((identity, rating))
                }
            };
            if !identity_visible {
                seat.account = AccountId(0);
                seat.nickname.clear();
            }
            if !rating_visible {
                seat.before = None;
                seat.after = None;
            }
        }
        matches.push(result);
    }
    Ok(Some(matches))
}

fn visible_rating(
    connection: &Connection,
    account: AccountId,
    mode: &RulesetId,
    modes: &[RulesetId],
) -> rusqlite::Result<bool> {
    if !modes.contains(mode) {
        return Ok(false);
    }
    connection
        .query_row(
            "SELECT 1 FROM ratings WHERE account=?1 AND ruleset=?2 AND hidden=0",
            params![account.0 as i64, mode.0],
            |_| Ok(true),
        )
        .optional()
        .map(|value| value.unwrap_or(false))
}
