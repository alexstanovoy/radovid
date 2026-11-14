//! Immutable playable bot identities, owned by an authenticated account.
use super::*;

pub(super) const SCHEMA: &str = "
ALTER TABLE accounts RENAME COLUMN bot_version TO retired_version;
ALTER TABLE accounts ADD COLUMN main_account INTEGER REFERENCES accounts(id);
ALTER TABLE accounts ADD COLUMN version TEXT;
CREATE UNIQUE INDEX accounts_version_name ON accounts(main_account,version) WHERE main_account IS NOT NULL;
CREATE INDEX accounts_versions ON accounts(main_account,id) WHERE main_account IS NOT NULL;
CREATE TRIGGER versions_owner_name AFTER UPDATE OF nickname ON accounts
WHEN NEW.main_account IS NULL
BEGIN UPDATE accounts SET nickname=NEW.nickname WHERE main_account=NEW.id; END;
CREATE TRIGGER versions_owner_ban AFTER UPDATE OF banned,ban_reason,ban_until ON accounts
WHEN NEW.main_account IS NULL
BEGIN UPDATE accounts SET banned=NEW.banned,ban_reason=NEW.ban_reason,ban_until=NEW.ban_until
WHERE main_account=NEW.id; END;
";

/// Schema 5 to 6: bot accounts become main accounts whose versions hold the ratings, and every
/// version starts from zero, so the ratings the bot accounts earned before are removed. This runs
/// once, inside the migration transaction. Returns how many rating rows were removed.
pub(super) fn migrate(transaction: &Transaction<'_>) -> rusqlite::Result<usize> {
    transaction.execute_batch(SCHEMA)?;
    transaction.execute(
        "DELETE FROM ratings WHERE account IN (SELECT id FROM accounts WHERE kind=1)",
        [],
    )
}

impl Db {
    /// Selection and creation share the writer's transaction, including the owner's bound.
    pub async fn select_version(
        &self,
        owner: AccountId,
        name: String,
    ) -> DbResult<Result<AccountRow, Error>> {
        self.run(move |connection| {
            let transaction = connection.transaction()?;
            let main = transaction
                .query_row(
                    &format!("{ACCOUNT_SELECT} WHERE id=?1 AND deleted=0"),
                    [owner.0 as i64],
                    account_row,
                )
                .optional()?;
            let Some(main) = main else {
                return Ok(Err(Error::BadSession));
            };
            if let Some(ban) = main.active_ban(now_ms()) {
                return Ok(Err(Error::Banned {
                    reason: ban.reason.clone(),
                    until: ban.until,
                }));
            }
            if main.kind != AccountKind::Bot || main.main_account.is_some() {
                return Ok(Err(Error::VersionNotAllowed));
            }
            if !valid_version(&name) {
                return Ok(Err(Error::BadVersion));
            }
            let existing = transaction
                .query_row(
                    &format!("{ACCOUNT_SELECT} WHERE main_account=?1 AND version=?2"),
                    params![owner.0 as i64, name],
                    account_row,
                )
                .optional()?;
            let mut row = match existing {
                Some(row) => row,
                None => {
                    let count: u32 = transaction.query_row(
                        "SELECT COUNT(*) FROM accounts WHERE main_account=?1",
                        [owner.0 as i64],
                        |row| row.get(0),
                    )?;
                    if count >= MAX_VERSIONS_PER_ACCOUNT as u32 {
                        return Ok(Err(Error::VersionLimit));
                    }
                    create_version(&transaction, &main, &name, count)?
                }
            };
            row.login = main.login;
            row.admin = main.admin;
            transaction.commit()?;
            Ok(Ok(row))
        })
        .await
    }
}

fn create_version(
    transaction: &Transaction<'_>,
    main: &AccountRow,
    name: &str,
    count: u32,
) -> rusqlite::Result<AccountRow> {
    transaction.execute("INSERT INTO accounts(login,nickname,hash,kind,created,main_account,version,banned,ban_reason,ban_until)
        VALUES(?1,?2,'',1,?3,?4,?5,?6,?7,?8)", params![format!("version#{}#{count}", main.id.0), main.nickname, now_ms().0 as i64, main.id.0 as i64, name,
        main.ban.is_some(), main.ban.as_ref().map_or("", |ban| ban.reason.as_str()), main.ban.as_ref().and_then(|ban| ban.until).map(|until| until.0 as i64)])?;
    transaction.query_row(
        &format!("{ACCOUNT_SELECT} WHERE id=?1"),
        [transaction.last_insert_rowid()],
        account_row,
    )
}
