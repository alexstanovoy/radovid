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
  `decline_cooldown_secs`; the others keep their entries and places. Two abandoned matches within
  an hour bar an account from queues and lobbies for an hour.
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
   `RESULT winner=<team index>|draw duration_ms=<n> report=<hex>` and exits 0. `report` is up to
   64 KiB of the game's own bytes, stored and forwarded to clients untouched.
4. Exit code 3 without `RESULT` means abandoned (nobody came, or everybody left); any other exit
   without `RESULT`, or a malformed one, means crashed. Both are null: nothing is rated. A match
   running past `max_duration_secs`, or stopped by an admin or the shutdown, is killed: stopped,
   null.

The result reaches radovid only on the game server's own stdout, which only that process (and the
host's root) can write: clients never report outcomes, and there is no network path to forge one.

Placeholders in `args`, `env` values and `public_url`: `{match_id}`, `{port}` (taken from
`game_ports`, the host port of this match), `{seats}` (`ticket_hex:team:name` per slot in slot
order, comma separated), `{ticket_<slot>}`, `{team_<slot>}`, `{name_<slot>}`; `{{` and `}}` are
braces. Tickets may only go into `env`: an argument shows in every process listing. Slots are
numbered team by team. A ticket is 32 random bytes; the game checks it (in constant time) and
decides what it allows: a seat, reconnecting to it, watching it.

Launcher kinds:

- `local`: a child process of the service (development, tests). Bind the game to 127.0.0.1
  unless players come from elsewhere.
- `docker`: `docker run --rm` attached, one container per match: read-only root, all capabilities
  dropped, no new privileges, memory/CPU/pid limits, `--publish <publish_address>:{port}:<container_port>`,
  read-only `mounts` (a certificate), labelled `radovid.match` (removed at the next start if the
  service died). Environment values pass by name (`--env NAME`), never on a command line. On the
  host: `userland-proxy: false` in the daemon configuration, and container egress blocked in
  the `DOCKER-USER` chain (not `--internal`, which drops published ports).

`version_probe`: arguments that make the game print its wire fingerprint (16 hex digits) and
exit. radovid runs it at start and refuses to serve a ruleset whose game servers speak another
wire than `game_fingerprint`. A client names its game wire in `Hello`; a ruleset of another
fingerprint refuses its queue and lobby requests (`GameVersionMismatch`).

## Run it

```sh
cargo run --release -p radovid -- dev-cert dev/          # a self-signed certificate for development
cp radovid.example.toml radovid.toml                      # point tls_cert/tls_key and the launcher at yours
cargo run --release -p radovid -- serve --config radovid.toml
```

Production: one host with docker; certificates from Let's Encrypt for the service's name and for
the game servers' name, renewed by certbot with a deploy hook that sends the service SIGHUP (it
rereads its certificate; connections already up keep theirs) and refreshes the game servers'
read-only mount (a match outlives no renewal). Native clients trust the web PKI
(`TlsConfig::webpki()`) or a development certificate (`TlsConfig::trust_pem`).

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

- TLS 1.3 only (rustls). Tickets travel only inside TLS and in a game server's environment.
- argon2id (64 MiB, t=3 by default), at most four hashes at once; an unknown login costs a hash too.
- Failed logins grow a delay per source and per login (after 3: 1, 2, 4 ... 300 s); an attempt
  inside the delay is not evaluated, and a right password after it always works. Wrong lobby
  passwords count against the source the same way. Counters live in memory, only for logins an
  account can have. A resume is never delayed: its token cannot be guessed.
- Session tokens are random, stored as SHA-256, expire after 30 days idle and are revoked by a
  password change, which also ends the live sessions of the other tokens. Passwords travel as
  `Secret`, whose `Debug` shows nothing.
- Everything is bounded: connections at once (`max_connections`), a client's message size,
  messages per second (token bucket), unauthenticated connections per source, sessions per
  account, games in flight per account and per source, queue entries, launched games, lobbies,
  list sizes, registrations per hour per source, the game servers' stdout lines. Hostile bytes
  close their own connection and nothing else.
- Stored per account: login, nickname, password hash, kind, admin and ban flags, bot version,
  creation time; per session a token hash and its last use; per match the seats, outcome, reason,
  duration, the game's report and the ratings before and after. No e-mail, no IP address. Logs
  carry match ids and errors, never logins, tokens or tickets.

## Tests

`cargo test` runs the end-to-end tests over real WebSockets; the game servers are
`crates/radovid/tests/fake_game.sh`, which honours the launcher contract and plays nothing (its
report echoes what it was started with). A game's own client library carries the tests against
its real server.
