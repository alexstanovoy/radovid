# radovid

A matchmaking service for any game whose servers start from a command line. Accounts, rated queues, lobbies,
ratings and a ladder, and one game server launched per match, in a single process with a single SQLite file.

radovid knows nothing about your game. A ruleset says which program to start and how; radovid starts it when a match
is formed, hands each player the server's address and a ticket for their seat, and reads the result from the server's
own output. It never carries game traffic, and clients can't report outcomes, so there is nothing for them to forge.

## What it does today

- **Accounts:** login and password (no e-mail, no recovery), human or bot. Players see each other as `nickname#id`;
  the login stays private.
- **Rated queues** with two-phase acceptance: a found match must be accepted by everyone within a time limit; a
  decline or silence sends that player to a short cooldown and the others back to their places.
- **Lobbies:** found in the list of open lobbies or by searching names and owners, and joined at once: an open lobby
  needs nothing, a locked one its password, which the service shows to nobody but the owner who set it. No approval
  step. Slots by team,
  spectators, and a game mode the owner chooses, which changes the rules but never how many may play.
- **Glicko-2 ratings** and a ladder; a draw is half a win, a crashed or abandoned match changes nothing.
- **Several games per account** at once (queues, lobbies, running matches), within per-account and per-address limits.
- **Game capacity:** a fixed number of servers run at once, and formed matches wait their turn in order.
- **Replays and other match files** kept for a set time and fetched by the players who sat in the match (or everyone,
  if you choose).
- **A practice ruleset** one player can start alone at once, e.g. a demo with cheats.
- **Admin:** bans with reason and expiry, account kinds, stopping a match.
- **Safe by default:** TLS 1.3 only, argon2id passwords, every limit bounded, tickets passed to game servers by
  environment only, no IP addresses or e-mails stored. Docker game servers run read-only with all capabilities
  dropped.

Clients speak WebSocket over TLS on one port; `radovid_client` is the Rust client library (blocking, a background
thread, or the browser's WebSocket). It can also serve a web client's static files on the same origin.

What is missing: password recovery and e-mail, rated matches with more than one player a side (rated rulesets are
1v1), a ladder across several hosts (one process, one SQLite file), and rolling protocol compatibility (clients and
service update together).

## Get started: a development setup on one machine

No docker and no domain needed. This uses bota's game server as the game; any game that honours the
[launcher contract](DEVELOPMENT.md#launcher-contract) works the same way.

1. Build radovid and the game server (in the bota checkout, `cargo build --release -p bota_server`), make a
   development certificate, and ask the game for its wire fingerprint:

   ```sh
   cargo build --release -p radovid
   mkdir dev && target/release/radovid dev-cert dev/        # cert.pem and key.pem for localhost; development only
   GAME=../bota/target/release/bota_server
   $GAME --wire-fingerprint                                  # 16 hex digits for game_fingerprint below
   ```

2. Write `dev/radovid.toml`. A `local` launcher runs the game as a child process on loopback:

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

   [ruleset.launcher]
   kind = "local"
   binary = "<absolute path of bota_server>"
   public_url = "ws://127.0.0.1:{port}"
   version_probe = ["--wire-fingerprint"]
   args = ["--map", "2", "--match-id", "{match_id}", "--bind", "127.0.0.1", "--port", "0",
           "--ws-port", "{port}", "--heroes", "1", "--start-after-secs", "60",
           "--abandon-unjoined", "--empty-after-secs", "120"]
   env = { BOTA_SEATS = "{seats}" }
   artifact = "/abs/dev/replays/replay-{match_id}.brp"    # the replay the game writes; players fetch it afterwards
   artifact_args = ["--replay", "{artifact}"]             # used only with [artifacts] save_replays = true
   ```

3. Run it:

   ```sh
   target/release/radovid serve --config dev/radovid.toml
   target/release/radovid admin --config dev/radovid.toml <login>   # make an existing account an admin
   ```

Clients connect to `wss://localhost:4460` and trust `dev/cert.pem` (dbota: `--trust-cert dev/cert.pem`). A bot
registers its own account of kind `bot` on first login. If matches end as crashes, look at the service's stderr
(`radovid: match N: ...`).

For production, start from `radovid.example.toml`: a `docker` launcher runs one container per match, `public_url`
must be `wss://` for anything but loopback, `lobby_ruleset` names the rules a new lobby starts with when there are
several, and SIGHUP reloads the certificate. `sqlite3 radovid.db ".backup copy.db"`
takes a consistent backup while the service runs.

## For developers

[DEVELOPMENT.md](DEVELOPMENT.md) has the launcher contract a game must honour, every configuration key, the protocol,
how concurrent results are rated, the security model and the tests.

## License

MIT, see [LICENSE](LICENSE).
