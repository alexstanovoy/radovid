
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


CREATE INDEX ratings_kind_ladder ON ratings(ruleset,kind,rating DESC,deviation,account) WHERE games>0;
CREATE INDEX ratings_global_ladder ON ratings(ruleset,rating DESC,deviation,account) WHERE games>0;
CREATE INDEX accounts_nickname_key ON accounts(lower(nickname),id) WHERE deleted=0;
CREATE TRIGGER ratings_account_kind AFTER INSERT ON ratings
WHEN NEW.kind != (SELECT kind FROM accounts WHERE id=NEW.account)
BEGIN UPDATE ratings SET kind=(SELECT kind FROM accounts WHERE id=NEW.account)
      WHERE account=NEW.account AND ruleset=NEW.ruleset; END;
CREATE TRIGGER accounts_ratings_kind AFTER UPDATE OF kind ON accounts
BEGIN UPDATE ratings SET kind=NEW.kind WHERE account=NEW.id; END;

PRAGMA user_version=3;

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

PRAGMA user_version=4;
