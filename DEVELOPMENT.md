# radovid for developers and operators

```
radovid_proto   the wire types, limits, the frame codec, registry/wire.txt; serde + postcard only
radovid_client  Control (the protocol without I/O); Client (blocking) and Background (a thread)
                over tungstenite + rustls (feature native, default); WebClient over the browser's
                WebSocket (feature web, wasm32)
radovid         the service (library + binary): tokio, one SQLite file
```

## Launcher contract

Any game can be launched that honours this, a shell script included:

1. radovid starts the launcher with the ruleset's arguments and environment, placeholders expanded, stdin closed,
   stdout read by radovid, stderr logged (the first 200 lines, cut short): a game prints no secret there.
2. The game prints `READY` once players may connect. Until then radovid tells them nothing; no `READY` within
   `start_timeout_secs` is a crash.
3. When the match is over the game prints one line
   `RESULT winner=<team index>|draw duration_ms=<n> report=<hex> [<key>=<value>]...` and exits 0 within 10 seconds
   (it is killed after that; the match counts as the line says). `report` is up to 64 KiB of the game's own bytes,
   stored and forwarded to clients untouched. The first three fields are fixed and in order; up to 16 `key=value`
   fields follow in any order, each key once:
   - `left=<slot>`: a player left for good (the game's own rule). `winner` must be another team; the match is rated,
     ends as `Left`, and nothing else is held against the leaver.
   - `replay=truncated` (version 2): the game closed its artifact before the match's end; radovid logs it.
   - `v=<n>`: the line's version, absent for 1. A game that only adds fields leaves it alone (radovid skips unknown
     keys); one that changes a field's meaning raises it, and an older radovid treats the line as malformed.
   radovid sets `RADOVID_RESULT_VERSION` (now 2), the newest version it reads; a game prints a version's fields only
   when the variable allows (absent: 1), so either side can be updated first. radovid reports the result and serves
   the match's artifact only after the game process has exited, so a game may finish writing after `RESULT`.
4. Exit code 3 without `RESULT` means abandoned (nobody came, everybody left, or a seat left for good during the picks, which cancels the match); any other exit without `RESULT`, or a
   malformed line, means crashed. Both are null: nothing is rated. A match running past `max_duration_secs`, or stopped
   by an admin or the shutdown, is killed and null.

The result reaches radovid only on the game server's own stdout, which only that process (and root) can write: there
is no network path to forge one.

Placeholders in `args`, `env` values and `public_url`: `{match_id}`, `{port}` (from `game_ports`), `{seats}`
(`ticket_hex:team:name:kind` per slot, comma separated; kind `bot` or `human`, chosen at registration and changed only
by an admin), `{ticket_<slot>}`, `{team_<slot>}`, `{name_<slot>}`, `{kind_<slot>}`; `{{` and `}}` are braces. Slots are
numbered team by team. Tickets may only go into `env` (arguments show in process listings). A ticket is 32 random
bytes; the game checks it in constant time and decides what it allows: a seat, a reconnect, watching.

A `docker` launcher runs `docker run --rm` attached, one container per match, publishing `<publish_address>:{port}` to
`container_port`, always `--read-only`, `--cap-drop ALL`, `no-new-privileges`, labelled `radovid.match`, with the
environment passed by name only. A `local` one runs the binary as a child process (development: bind the game to
loopback).

`version_probe`: arguments that make the game print its wire fingerprint (16 hex digits) and exit. radovid runs it at
start and refuses to serve a ruleset whose servers speak another wire than `game_fingerprint`; a client naming another
fingerprint in `Hello` is refused that ruleset's queue and lobbies (`GameVersionMismatch`).

## Configuration

Required: `listen`, `database`, `tls_cert`, `tls_key`, `game_ports`, and at least one `[[ruleset]]` with its
`[ruleset.launcher]`. Everything else has a default. Unknown keys and out-of-range values fail at start, naming the key.

| Key | Default | What it controls |
|---|---|---|
| `listen` | | address and port of the TLS listener |
| `database` | | the SQLite file (WAL; created `0600`) |
| `tls_cert`, `tls_key` | | PEM chain and key, reread on SIGHUP |
| `game_ports` | | `"first-last"` host ports, one per running match; at least `game_capacity` |
| `web.static_dir` | none | static files served read-only on the listener's origin (`GET`/`HEAD`, no listings, `index.html` at `/`, `.br`/`.gz` siblings), so a web client and the service share one URL |
| `web.send_deadline_secs` | `600` | seconds a page has to be sent whole, TLS close included; a slower peer is cut off. 1 to 3600 (see "Serving a web client") |
| `max_connections` | `2048` | connections at once, logged in or not; at least 16, so that each of the quarters the logins to be and the pages being sent hold is a few connections |
| `server_name` | `radovid` | the name in `Welcome` |
| `password.memory_kib`, `password.iterations` | `65536`, `3` | argon2id cost of new hashes; lower only for tests |
| `password.max_waiting` | `64` | password checks waiting for a hash slot; more are answered `Throttled` |
| `limits.games_per_account` | `{ human = 0, bot = 0 }` | games in flight per account: queue entries, lobbies, unfinished matches; 0 means unlimited (source and service limits still apply) |
| `limits.leaderboard_size` | `100` | leaderboard rows per response, 1 to 1000, besides the requester's own row; served in `Limits` |
| `limits.games_per_source` | `8` | the same for all accounts of an IPv4 address or IPv6 /64; raise for shared NATs, accepting more abuse exposure |
| `limits.games_per_ipv6_48` | `64` | aggregate activities across IPv6 /64 sources in one /48, 1 to 65536 |
| `limits.custom_match_capacity` | `256` | lobby countdowns plus starting, waiting and running lobby/solo matches across all accounts, 1 to 65536; `Limits` reports it |
| `limits.accept_timeout_secs` | `30` | to accept a proposal (1 to 600); silence declines |
| `limits.decline_cooldown_secs` | `120` | out of the queues after a decline or silence (at most a day) |
| `limits.game_capacity` | `8` | games running at once; formed matches wait in order (`MatchQueued`) and the pairer pauses while this many wait |
| `limits.lobby_games_max` | `6` | of those, the most lobby and solo games may hold, so queues always have slots |
| `limits.queue_capacity` | `256` | queue entries across rulesets (1 to 65536); past it `QueueFull` |
| `limits.ranked_window` | `{ initial = 100, widen_per_minute = 100, maximum = 1000 }` | ranked skill window, in rating points |
| `limits.unranked_window` | `{ initial = 600, widen_per_minute = 300, maximum = 3000 }` | unrated skill window; at least as loose as ranked |
| `limits.artifact_retention_days`, `limits.artifact_cap_mib` | `14`, `2048` | artifacts older than this, then the oldest beyond the cap, are deleted hourly |
| `limits.artifact_max_mib` | `64` | the most one artifact may have; a docker game cannot write more, a longer file is not served |
| `lobby_teams` | `[1, 1]` | the slots of every lobby, at most 10 seats; no ruleset caps how many sit on a side |
| `lobby_ruleset` | the first `[[ruleset]]` (a warning at start when there are several) | the ruleset a new lobby is of until its owner chooses another (`RulesetInfo.lobby_default`); set it whenever there is more than one ruleset, e.g. `"duel"` |
| `artifacts.save_replays` | `false` | whether replays are kept and served at all; off, no game is asked to write one |
| `ruleset.id` | | `[a-z0-9_]`, 1 to 32 bytes, never reused for other rules |
| `ruleset.name`, `ruleset.game` | | display name (1 to 64 bytes); the game's label (1 to 32) |
| `ruleset.game_fingerprint` | | 16 hex digits: the wire clients and servers must speak |
| `ruleset.teams` | | team sizes, two or more sides of any size, at most 10 seats |
| `ruleset.rated` | | queued games update this ruleset's ladder; lobbies remain unrated |
| `ruleset.rating_ruleset` | none | an unrated queue's rated sibling; same game, fingerprint and teams, at most one unrated sibling per ladder |
| `ruleset.rating_window` | mode's policy from `limits` | partial overrides of `initial`, `widen_per_minute`, `maximum`; maximum 1 to 10000, other values 0 to 10000, initial ≤ maximum; zero widening requires initial = maximum |
| `ruleset.info` | `""` | hex bytes shown to clients, the game's own meaning |
| `ruleset.max_duration_secs` | `7200` | a longer match is killed (a null) |
| `ruleset.games_per_account` | the `[limits]` value | this ruleset's own per-account limit |
| `ruleset.lobby_min_players` | `1` | players a lobby needs before its host may start it |
| `ruleset.instant_solo` | `false` | one account may start an unrated match alone at once (`StartSolo`) |
| `ruleset.audience` | `everyone` | `everyone`, `humans_only` or `bots_only`; the others aren't told the ruleset exists |
| `ruleset.artifact_access` | `seats` | who fetches the replay: `seats` (who played) or `audience` (anyone the ruleset admits) |
| `ruleset.artifact_max_mib` | `limits.artifact_max_mib` | per-ruleset artifact size limit (1 to 1024) |
| `ruleset.artifacts.<name>` | none | further files per match besides the replay: `path`, `args`, `env`, `max_kib`/`max_mib`, `access`, `retention_days`, `cap_mib`, `kept` |
| `launcher.kind` | | `docker` or `local` |
| `launcher.image`, `launcher.container_port` | | docker: the image and the game's port inside it |
| `launcher.binary` | | local: the executable |
| `launcher.public_url` | | what players connect to; must be `wss://` unless loopback |
| `launcher.insecure_plaintext` | `false` | allow a non-`wss://` `public_url` to another host |
| `launcher.args`, `launcher.env` | `[]`, `{}` | the game's command line and environment; tickets only in `env` |
| `launcher.cheats_args` | `[]` | arguments that start the game with its cheats, added to `args` for a lobby's match whose owner turned them on (`LobbyCheats`, seen in `LobbyState.cheats`), never a queued one; empty: the ruleset has no cheats. It begins with a flag (an element starting with `-` that is no number). Each of its flags with the values after it (`--x=1` is `--x 1`) may not stand, whole and in order, among the arguments of the ruleset's `args` or its artifacts' arguments, or among the words of an env value of `env` or the artifacts' environment: refused at start as written, and a match without cheats whose placeholders expand to one is not launched (an ERROR line; the match ends null) |
| `launcher.version_probe` | `[]` | arguments that print the wire fingerprint |
| `launcher.artifact`, `artifact_args`, `artifact_env` | none | the replay file per match (absolute, `{match_id}` in its name) and how the game is told its path (`{artifact}`); a docker game gets exactly that file, writable at `/artifact` |
| `launcher.start_timeout_secs` | `60` | from launch to `READY` |
| `launcher.memory`, `cpus`, `pids_limit` | `512m`, `1`, `256` | docker limits per container |
| `launcher.network` | docker's bridge | the container's network |
| `launcher.publish_address` | `0.0.0.0` | host address the game port is published on |
| `launcher.mounts` | `[]` | read-only `-v` mounts, e.g. the game's certificate |
| `launcher.docker` | `docker` | the docker client binary |

Artifacts: radovid creates each artifact file empty before the game starts. Keep the directory above it closed to
other users, since the file is world-writable for the container's user. A file that is a link, or over its limit, is
never served. radovid never reads the bytes: a game's durable files (metadata, a PGN, a log) are kept as long as the
operator says. `radovid.example.toml` demonstrates three neutral development fixtures; `examples/tic_tac_toe.toml` shows a single ruleset. A game may additionally opt into `instant_solo` and its own cheats arguments.

Operation: SIGHUP rereads the certificate and key (a bad pair keeps the old one); SIGINT or SIGTERM stops every match
(a null) and exits. After a crash the next start records unfinished matches as null and removes leftover containers.

### Serving a web client

With `[web] static_dir`, the service sends the files of a web client on its own origin. Each page holds a place of a
connection not logged in until it is sent, so `web.send_deadline_secs` bounds how long a slow reader may hold one. Size
it to the largest file a client may fetch uncompressed over the slowest connection you mean to serve: the default, 600
seconds, sends a 125 MB WebAssembly module at ~210 KiB/s. Serve `.br`/`.gz` siblings so most clients
fetch far less. A shorter deadline is stricter with slow readers; past 3600 seconds it is refused at startup.

## Protocol

`/control` speaks `radovid_proto`. One binary WebSocket message is one frame (`u32 LE length | postcard payload`): at
most 64 KiB from a client, 4 MiB from the service.

- The first message is `Hello { proto, game_fingerprint, client }`. The version must lie in
  `MIN_PROTO_VERSION..=PROTO_VERSION` (else `ProtoMismatch`); `Welcome` carries the version and the ruleset catalog.
- Every client frame has a `seq` and gets exactly one `Reply` or `Refused`; `Event`s come on their own. `Rulesets` and
  `Limits` are answered before login.
- A queue entry leads to `MatchProposed` (answer `MatchAccept` or `MatchDecline`), `ProposalUpdate`, then
  `ProposalFailed` (back in the queue) or `MatchQueued` and `MatchFound { handoff }`, and finally `MatchResult`.
  `LoggedIn.activities`, `MyMatches` and `MatchHandoff` return what an account has in flight, for a reconnect or a
  second device.
- Each matchmaking ruleset has humans and everyone queues. `QueueJoinAudiences` can link both places into one
  search; bots may use everyone only. The queue capacity counts raw entries; game limits count linked
  searches once. See [protocol 16](docs/protocol_16.md) for reservation, cancellation and upgrade details.
- `registry/wire.txt` lists every enum variant and field position and only grows. Every wire change bumps
  `PROTO_VERSION`; there is no rolling compatibility yet.
- A position is never reused, like a protobuf field number. A message, variant or field that goes is retired: its line
  stays with `retired <the line>` appended, and a placeholder of the same payload holds the position (a variant
  `Retired<Name>`, which is no message, or an always-empty field `retired_<name>`), so every byte after it stays put.
  Version 11 retired the lobby join requests and the per-ruleset team cap and serves no older client.
  Version 12 added lobby cheats (`LobbyCheats`, `NoCheats`, `LobbyState.cheats`) and serves no older client: a
  client of 11 could not be told that a lobby it sits in plays with cheats.
  Version 13 retires the numeric per-account fields without reusing their positions and appends typed
  `AccountGameLimit::Limited(n)` / `Unlimited` fields. It serves no older client because catalog entries are
  nested on the wire. Old binaries receive `ProtoMismatch`; consumers must rebuild and read the explicit limit.
  Zero remains only a configuration spelling, never an active numeric wire limit.
  Version 14 appends custom-match and IPv6-site capacity fields and refusal variants; it serves version 14 clients.

## Leaderboards and profiles

Each rated mode has Humans and Bots views of its accounts' ratings. The leaderboard returns
the configured top N, literal nickname-prefix search results with absolute ranks, and the
requester's own position even outside those results. Profiles include per-mode ranks. A service
configures at most 64 rated modes. See [protocol 19](docs/protocol_19.md) for the request/reply
contract, deterministic ordering, visibility, rate limits and database-work budget.

## Ratings

Glicko-2 (1500, 350, 0.06, tau 0.5), one rating period per game, with deviation grown for idle days first.
Each account has one rating per ruleset ladder. Concurrent games update that same rating in result-arrival order,
in one SQLite transaction per result. Every member's `before` is read before any member's `after` is written.
A null changes nothing. Custom lobbies remain unrated.

An unrated queue references its mode's rated ladder with `rating_ruleset`, uses a looser
skill window, and never changes ratings. A ruleset without `rated` or `rating_ruleset`
remains available only through lobbies or solo starts. The effective window widens with
waiting time up to its configured maximum; all members must accept the group's full
rating spread. Policy, selection and resource bounds are described in
[protocol 17](docs/protocol_17.md); `examples/tic_tac_toe.toml` shows a ranked/unranked pair.

For two or more sides, each team is represented by its members' mean rating and deviation
`sqrt(sum(member_deviation²)) / team_size`. Each member receives one Glicko-2 period against each opposing team
once, regardless of that team's size. Each opposing-side contribution to Glicko-2 information and
rating gain has weight `1 / (side_count - 1)`: the information budget is one game per match, not one game per
opponent. Volatility is updated once using those weighted totals. The winning side scores 1 against every other side; losing sides score 0
against the winner and ½ against each other. A whole-match draw gives every pairing ½. One-player teams preserve
the original 1v1 formula exactly. Volatility belongs to each member, never to a persistent team record.

Teams are processed in team-index order, with at most ten seats and fewer than 100 member/opponent comparisons.
All arithmetic is deterministic for the same stored ratings and result order; concurrent completion order is
intentionally part of the input. No account may hold two seats in one match. A player's own rating determines its
expected score against the opposing mean: adding low-rated teammates does not amplify that player's update.
Only the launched server supplies outcomes. Deliberate sandbagging or collusion still requires moderation.

The result protocol expresses a single winner or a whole-match draw, not placements, shared first places, or
fractional scores. In FFA, the winner-versus-others decomposition therefore treats non-winning sides as tied.

## Security

- One listener, TLS 1.3 only; no plain-text mode. The service refuses to start when the database or the TLS key is
  readable by every user of the host; it creates the database `0600`, and `dev-cert` writes its key `0600`.
- argon2id, at most four hashes at once and one per source; a full waiting line answers `Throttled`. An unknown login
  costs a hash too. Failed logins grow a delay per source, per (login, source) pair and per login across sources, so
  strangers can't lock an owner out; a right password after the delay always works.
- Session tokens are random, stored as SHA-256, expire after 30 days idle, and a password change revokes them all; an
  account keeps its newest 32 stored, a login past them drops the oldest.
- Bounded: message size and rate, unauthenticated connections per source (16 per IPv4 address and per IPv6 /64, 64
  per IPv6 /48; a page download counts until it is sent, within `web.send_deadline_secs`), handshakes (10 s), sessions
  per account, lobbies, list sizes, registrations per hour per source, game servers' output lines. Connections not logged
  in hold half of `max_connections` at most, in two shares of a quarter each, so the sessions logged in keep the other
  half: new sockets that have made no request yet and control connections not logged in (logins to be), and pages being
  sent (one past theirs is answered `503 Service Unavailable` with `Retry-After`). Pages never take the logins' share,
  nor logins the pages'. Behind a reverse proxy every peer counts as the proxy's address:
  `X-Forwarded-For`, which any peer could forge, is not read.
- Tickets reach game servers only by environment; logs carry match ids and errors, never logins, passwords, tokens or
  tickets.
- Stored: per account login, nickname, password hash, kind, admin and ban flags, bot version, creation time; per session
  a token hash; per match seats, outcome, duration, the game's report, ratings before and after. No e-mail, no IP
  address. `DeleteAccount` removes credentials, sessions and ratings and leaves the match history under `deleted`.

## Tests

`cargo test` runs the end-to-end tests over real WebSockets; the launcher-contract tests use `crates/radovid/tests/fake_game.sh` (its report echoes how it was started).
`tests/games.rs` also launches independent Python 3 game processes and plays them through real socket clients:
rated tic-tac-toe, clock expiry, fixed-tick team and free-for-all games, and a custom lobby. The fixtures bind
loopback only, cap input/connection/work budgets, and validate opaque seat tickets in constant time. `python3` on `PATH` is a required test dependency; a missing interpreter fails the tests and gate. A game's own client
library carries the tests against its real server. `tools/gate.sh` is the merge gate: clippy, the wasm check, the
tests, the docker launcher test against a real docker, the fuzz targets' type check, `cargo deny check` and fmt.

## Population benchmark and WAL regression

`cargo bench -p radovid --bench lookup_scale` is the explicit optimized population
benchmark. It seeds one million accounts in three modes on disk under
`CARGO_TARGET_DIR` (the workspace target by default), verifies and reuses that
fixture, and prints timings. It has no wall-clock pass/fail assertions and does
not run in the lib suite. Remove its generated `lookup_scale/` directory when
finished with the benchmark data.

The ordinary suite checks deterministic SQLite instruction/covering-plan bounds,
per-mode profile fallback, and WAL storage bounds with a small disk-backed fixture.
See [database bounds](docs/database.md) for checkpoint admission and busy behavior.
