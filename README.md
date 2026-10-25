# radovid

radovid is a matchmaking service for any game whose servers can be started from a command line:
accounts, queues with two-phase match acceptance, custom lobbies, Glicko-2 ratings, a ladder,
admin bans, and the launching of one game server per match. It knows no game. A ruleset names its
game and how to start a server; players get the server's address and a ticket for their seat, and
what they say to the server there is the game's business. radovid never carries game traffic.

It is one process (tokio) with one SQLite file. Clients speak WebSocket over TLS on one port.

```
radovid_proto   the wire types, limits, the frame codec, registry/wire.txt; serde + postcard only
radovid_client  Control (the protocol without I/O); Client (blocking) and Background (a thread)
                over tungstenite + rustls (feature native, default); WebClient over the browser's
                WebSocket (feature web, wasm32)
radovid         the service (library + binary)
```

## What it does

- **Accounts.** A login and a password (no e-mail, no recovery), human or bot. Another account is
  always shown by nickname and id (`nickname#id`); the login is a credential and only its owner
  sees it. `DeleteAccount` removes the credentials, sessions and ratings and leaves the match
  history under the name `deleted`.
- **Several games at once.** An account has games in flight (queue entries, lobby seats, join
  requests, unfinished matches) up to its limit (`[limits] games_per_account`, per kind, per
  ruleset overridable, readable with `Limits`); all accounts of one source (an IPv4 address or an
  IPv6 /64) together up to `games_per_source`. One rating per account per rated ruleset.
- **Queues and proposals.** A rated ruleset has a queue. The pairer takes the oldest free entries,
  never one account twice, and two from one source only when no other entry fits. A pairing is a
  proposal that names no opponent: every player accepts within `accept_timeout_secs`. A decline
  or silence ends it: the decliner's entry is gone and its account may not queue for
  `decline_cooldown_secs`; the others keep their entries and places. Leaving a match costs nothing beyond the lost
  match: there is no cooldown for it.
- **Game slots.** At most `game_capacity` games run at once, lobby games at most
  `lobby_games_max` of them, so queued matches always have slots of their own. Formed matches wait
  in the order they were formed and are told how many wait before them (`MatchQueued`); the
  pairer pauses while `game_capacity` matches are being formed or waiting, and entries see
  `overloaded` in their `QueueStatus`. The queues take `queue_capacity` entries; past that
  `QueueFull`.
- **Lobbies.** Unrated games with an owner and a unique name. Ask the owner, or give the lobby's
  password and be in at once; press ready; the owner's ready starts a 5 s countdown nobody can
  cancel; the match then waits for a slot like any other. The owner leaving closes the lobby.
- **Ratings.** Glicko-2 (1500, 350, 0.06, tau 0.5), one rating period per game, the deviation
  grown for idle days first; a draw is half a win; a null changes nothing.
- **Admin.** Ban (with reason and expiry), unban, change an account's kind, stop a match (a null).
  An admin is made on the host: `radovid admin --config radovid.toml <login>`.

## Ratings of concurrent matches

Glicko-2 depends on the order of results, and one account may finish several matches at about the
same time. The rule: results are applied one at a time, in the order radovid reads their `RESULT`
lines; each is one SQLite transaction that reads both seats' ratings as stored at that moment
(not at the match start), grows their deviations for the time since each one's last stored game,
updates both, and writes them back. `before` and `after` in a `MatchResult` are the values around
that transaction. Two matches finishing within a second thus apply in arrival order and the second
sees the first's result; the outcome differs from the other order only by Glicko-2's own
non-commutativity.

## Launcher contract

Any game can be launched that honours this, a shell script included:

1. radovid starts the launcher with the ruleset's arguments and environment, placeholders
   expanded, stdin closed, stdout read by radovid, stderr logged (the first 200 lines, cut short):
   a game prints no secret there.
2. The game prints a line `READY` once players may connect. Until then radovid tells them nothing;
   no `READY` within `start_timeout_secs` is a crash.
3. When the match is over the game prints one line
   `RESULT winner=<team index>|draw duration_ms=<n> report=<hex> [left=<slot>]` and exits 0. `report` is up to
   64 KiB of the game's own bytes, stored and forwarded to clients untouched. `left=<slot>` names a
   player who left the match for good (the game's own rule): `winner` must then be another team's;
   the match is rated, ends as `Left`, and nothing else is held against the leaver.
4. Exit code 3 without `RESULT` means abandoned (nobody came, or everybody left); any other exit
   without `RESULT`, or a malformed one, means crashed. Both are null: nothing is rated. A match
   running past `max_duration_secs`, or stopped by an admin or the shutdown, is killed: stopped,
   null.

The result reaches radovid only on the game server's own stdout, which only that process (and the
host's root) can write: clients never report outcomes, and there is no network path to forge one.

Placeholders in `args`, `env` values and `public_url`: `{match_id}`, `{port}` (taken from
`game_ports`, the host port of this match), `{seats}` (`ticket_hex:team:name:kind` per slot in slot
order, comma separated; kind is `bot` or `human`: the account's own, chosen at registration and changed
only by an administrator), `{ticket_<slot>}`, `{team_<slot>}`, `{name_<slot>}`, `{kind_<slot>}`; `{{` and `}}` are
braces. Tickets may only go into `env`: an argument shows in every process listing. Slots are
numbered team by team. A ticket is 32 random bytes; the game checks it (in constant time) and
decides what it allows: a seat, reconnecting to it, watching it.

A `docker` launcher runs `docker run --rm` attached, one container per match, publishing
`<publish_address>:{port}` to `container_port`; a `local` one runs the binary as a child
process (development: bind the game to loopback).

`version_probe`: arguments that make the game print its wire fingerprint (16 hex digits) and
exit. radovid runs it at start and refuses to serve a ruleset whose game servers speak another
wire than `game_fingerprint`. A client names its game wire in `Hello`; a ruleset of another
fingerprint refuses its queue and lobby requests (`GameVersionMismatch`).

## Run it

```sh
cargo build --release -p radovid
target/release/radovid dev-cert dev/          # self-signed cert.pem and key.pem (mode 0600) for localhost, 127.0.0.1 and ::1; development only
cp radovid.example.toml radovid.toml          # tls_cert/tls_key: yours; image and public_url: your game's
target/release/radovid serve --config radovid.toml
target/release/radovid admin --config radovid.toml <login>   # make an existing account an admin
```

SIGHUP rereads `tls_cert` and `tls_key` (connections already up keep theirs; a bad pair keeps
the old one); SIGINT or SIGTERM stops every match (a null) and exits. After a crash the next
start records unfinished matches as lost (null) and removes the containers left behind.
Backups, if wanted: `sqlite3 radovid.db ".backup copy.db"` takes a consistent copy while the
service runs.

### A development setup on one machine (no docker, no domain)

Build the game server with its WebSocket listener (bota: `cargo build --release -p bota_server` in the bota
checkout; the `websocket` feature is on by default) and make a certificate:

```sh
cargo build --release -p radovid
mkdir dev && target/release/radovid dev-cert dev/
GAME=../bota/target/release/bota_server
$GAME --wire-fingerprint                      # 16 hex digits for game_fingerprint below
```

`dev/radovid.toml` (a `local` launcher runs the game server as a child process on loopback; the
game's address is `ws://127.0.0.1:{port}`, which a non-`wss://` `public_url` may be only because
it is loopback):

```toml
listen = "127.0.0.1:4460"
database = "dev/radovid.db"
tls_cert = "dev/cert.pem"
tls_key = "dev/key.pem"
game_ports = "20000-20010"

[[ruleset]]
id = "solo_mid"
name = "1x1 Mid Only"
game = "bota"
game_fingerprint = "<the digits above>"
teams = [1, 1]
rated = true
info = "0100"

[ruleset.launcher]
kind = "local"
binary = "<absolute path of bota_server>"
public_url = "ws://127.0.0.1:{port}"
version_probe = ["--wire-fingerprint"]
args = ["--map", "2", "--match-id", "{match_id}", "--bind", "127.0.0.1", "--port", "0",
        "--ws-port", "{port}", "--heroes", "1", "--start-after-secs", "60",
        "--abandon-unjoined", "--empty-after-secs", "120",
        "--replay", "{artifact}"]
env = { BOTA_SEATS = "{seats}" }
artifact = "/abs/dev/replays/replay-{match_id}.brp"    # the game writes it ({artifact}); seats fetch it after the match
```

```sh
target/release/radovid serve --config dev/radovid.toml
```

Clients name the service `wss://localhost:4460` (the certificate names `localhost`, `127.0.0.1`
and `::1`) and trust `dev/cert.pem` (dbota: `--trust-cert dev/cert.pem`). A bot client needs an
account of its own: it registers one with kind `bot` on first login. A game server built without
the listener (`--no-default-features`) refuses `--ws-port`: every match then ends as a crash,
nothing is rated; start the service once and look at its stderr lines (`radovid: match N: ...`)
when matches are lost.

### Safe by default

- One listener, TLS 1.3 only; no plain-text mode exists.
- The service refuses to start when a secret file is open to every user of the host (the
  database, the TLS key: `chmod o-rwx`); it creates the database `0600`, and `dev-cert` writes
  its key `0600`.
- A `public_url` that is not `wss://` must name a loopback host, or tickets would cross the
  network in clear; `insecure_plaintext = true` allows it on a network you trust.
- Every limit has a default (table below); unknown keys and out-of-range values fail at start
  with the offending key named.
- Tickets go to game servers by environment only (a `{ticket_*}` or `{seats}` placeholder in
  `args` is refused); logs carry match ids and errors, never logins, passwords, tokens or tickets.

### Configuration

Required: `listen`, `database`, `tls_cert`, `tls_key`, `game_ports`, and at least one
`[[ruleset]]` with its `[ruleset.launcher]`. Everything else has a default.

| Key | Default | What it controls | Change it when |
|---|---|---|---|
| `listen` | | address and port of the TLS listener | |
| `database` | | the SQLite file (WAL; created `0600`) | |
| `tls_cert`, `tls_key` | | PEM chain and key, reread on SIGHUP | a renewal |
| `game_ports` | | `"first-last"` host ports, one per running match; at least `game_capacity` | |
| `web.static_dir` | none | a directory of static files served read-only on the listener's own origin (`GET`/`HEAD`, no listings, nothing outside the directory, `index.html` at `/`, a `.br` or `.gz` sibling for clients that take it), so a page and the service share one URL | a client is a web page |
| `max_connections` | `2048` | connections at once, logged in or not | the host has fewer file descriptors or more memory |
| `server_name` | `radovid` | the name in `Welcome` | |
| `password.memory_kib`, `password.iterations` | `65536`, `3` | argon2id cost of new hashes (stored hashes keep theirs) | lower only for tests |
| `password.max_waiting` | `64` | password checks waiting for a hash slot (one hash per source at a time); more are answered `Throttled` | |
| `limits.games_per_account` | `{ human = 2, bot = 8 }` | games in flight per account: queue entries, lobbies, join requests, unfinished matches | |
| `limits.games_per_source` | `8` | the same for all accounts of an IPv4 address or IPv6 /64 | players behind one NAT |
| `limits.accept_timeout_secs` | `30` | to accept a proposal (1 to 600); silence declines | |
| `limits.decline_cooldown_secs` | `120` | out of the queues after a decline or silence (at most a day) | |
| `limits.game_capacity` | `8` | games running at once; formed matches wait in order | the host takes more games |
| `limits.lobby_games_max` | `6` | of those, the most lobby games may hold | |
| `limits.queue_capacity` | `256` | queue entries across rulesets; past it `QueueFull` | |
| `ruleset.id` | | `[a-z0-9_]`, 1 to 32 bytes, never reused for other rules | |
| `ruleset.name`, `ruleset.game` | | display name (1 to 64 bytes); the game's label for clients (1 to 32) | |
| `ruleset.game_fingerprint` | | 16 hex digits: the game wire clients and servers must speak | each game release |
| `ruleset.teams` | | team sizes; slots are numbered team by team | |
| `ruleset.rated` | | queue and ladder; a rated ruleset is `teams = [1, 1]` | |
| `ruleset.info` | `""` | hex bytes shown to clients (the game's own meaning) | |
| `ruleset.max_duration_secs` | `1800` | a match running longer is killed (a null) | longer games |
| `ruleset.games_per_account` | the `[limits]` value | this ruleset's own per-account limit, one number or per kind | |
| `ruleset.lobby_min_players` | the seats | players a lobby needs before its host may start it (`LobbyReady`); fewer lets a host start alone, and the game's own launcher arguments fill the empty seats (a ruleset for it is a launcher that does) | |
| `ruleset.audience` | `everyone` | `everyone`, `humans_only` or `bots_only`: who may queue and sit in lobbies here (`AudienceMismatch` otherwise); the others are not told the ruleset exists | |
| `launcher.kind` | | `docker` (one container per match) or `local` (a child process, development) | |
| `launcher.image`, `launcher.container_port` | | docker: the image, and the game's port inside it | |
| `launcher.binary` | | local: the executable | |
| `launcher.public_url` | | what players connect to; placeholders below | |
| `launcher.insecure_plaintext` | `false` | allow a non-`wss://` `public_url` to another host | a trusted network without TLS |
| `launcher.args`, `launcher.env` | `[]`, `{}` | the game's command line and environment; tickets only in `env` | |
| `launcher.version_probe` | `[]` | arguments that make the game print its wire fingerprint and exit; checked at start | always, when the game has one |
| `launcher.artifact` | none | an absolute path whose file name holds `{match_id}`: the replay the game leaves for a match. radovid creates the empty file before the game starts and passes its path as `{artifact}` (in `args` and `env` values); a docker container gets exactly that file, writable at `/artifact` (at most 64 MiB, `--ulimit fsize`), and the rest of it stays read-only. Keep the directory above it closed to other users: the file is world-writable so the container's user can open it. The seats of the finished match fetch it in chunks (`MatchArtifact`) | the game records matches |
| `limits.artifact_retention_days`, `limits.artifact_cap_mib` | `14`, `2048` | artifacts older than this, then the oldest beyond the size cap, are deleted (hourly) | the disk is smaller or replays matter longer |
| `launcher.start_timeout_secs` | `60` | from launch to the game's `READY` | slow-starting games |
| `launcher.memory`, `cpus`, `pids_limit` | `512m`, `1`, `256` | docker limits per container | heavier games |
| `launcher.network` | docker's bridge | the container's network | a network without egress |
| `launcher.publish_address` | `0.0.0.0` | host address the game port is published on | a host with several addresses |
| `launcher.mounts` | `[]` | read-only `-v` mounts, e.g. the game's certificate | |
| `launcher.docker` | `docker` | the docker client binary | |

Docker containers always run `--read-only`, `--cap-drop ALL`, `no-new-privileges`, labelled
`radovid.match`, environment passed by name only (`--env NAME`).

## Protocol

`/control` speaks `radovid_proto`. One binary WebSocket message is one frame
(`u32 LE length | postcard payload`): at most 64 KiB from a client, 4 MiB from the service.

- The first message is `Hello { proto, game_fingerprint, client }`. The protocol version must match
  exactly (`Error::ProtoMismatch`); the answer `Welcome` carries the ruleset catalog.
- Every client frame has a `seq`; the service answers it with exactly one `Reply` or `Refused`, and
  sends `Event`s on its own. `Rulesets` and `Limits` are answered before login too.
- A queue entry leads to `MatchProposed` (answer `MatchAccept` or `MatchDecline`),
  `ProposalUpdate`, then `ProposalFailed` (the entry is back at its place) or `MatchQueued`
  (waiting for a slot) and `MatchFound { handoff }` (connect), and in the end `MatchResult`.
  `LoggedIn.activities` lists what an account has in flight; `MyMatches` and `MatchHandoff`
  return the handoffs of its running matches, for a reconnect or a second device.
- `registry/wire.txt` lists every enum variant and field position; it only grows. Protocol
  version 2 restarted it (version 1 never shipped). Every wire change bumps `PROTO_VERSION`; there
  is no rolling compatibility for now.

## Security notes

- argon2id, at most four hashes at once and one per source; the line of waiting checks is
  bounded and a full line answers `Throttled`. An unknown login costs a hash too. Failed logins
  grow a delay per source and per (login, source) pair (after 3: 1, 2, 4 ... 300 s) and per
  login across all sources (after 30), so strangers cannot hold a login back from its owner; a
  right password after the delay always works; a resume is never delayed (its token cannot be
  guessed).
- Session tokens are random, stored as SHA-256, expire after 30 days idle and are revoked by a
  password change, which also ends the other tokens' live sessions.
- Also bounded: a client's message size (64 KiB) and rate, unauthenticated connections per
  source, TLS and WebSocket handshakes (10 s), sessions per account, lobbies, list sizes,
  registrations per hour per source, the game servers' output lines.
- Stored: per account login, nickname, password hash, kind, admin and ban flags, bot version,
  creation time; per session a token hash; per match seats, outcome, duration, the game's
  report, ratings before and after. No e-mail, no IP address.

## Tests

`cargo test` runs the end-to-end tests over real WebSockets; the game servers are
`crates/radovid/tests/fake_game.sh`, which honours the launcher contract and plays nothing (its
report echoes what it was started with). A game's own client library carries the tests against
its real server.
