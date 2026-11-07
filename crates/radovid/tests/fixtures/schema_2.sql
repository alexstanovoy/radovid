
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
