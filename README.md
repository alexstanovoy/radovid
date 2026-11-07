# radovid

A matchmaking service for any game whose servers start from a command line. Accounts, ranked and unranked queues, lobbies,
ratings and a ladder, and one game server launched per match, in a single process with a single SQLite file.

radovid knows nothing about your game. A ruleset says which program to start and how; radovid starts it when a match
is formed, hands each player the server's address and a ticket for their seat, and reads the result from the server's
own output. It never carries game traffic, and clients can't report outcomes, so there is nothing for them to forge.

## What it does today

- **Accounts:** login and password (no e-mail, no recovery), human or bot. Players see each other as `nickname#id`;
  the login stays private.
- **Ranked and unranked queues** with configurable rating windows that widen with waiting time. A found match must be accepted by everyone within a time limit; a
  decline or silence sends that player to a short cooldown and the others back to their places.
- **Lobbies:** found in the list of open lobbies or by searching names and owners, and joined at once: an open lobby
  needs nothing, a locked one its password, which the service shows to nobody but the owner who set it. No approval
  step. Slots by team,
  spectators, and a game mode the owner chooses, which changes the rules but never how many may play.
- **Glicko-2 ratings** and a ladder; a draw is half a win, a crashed or abandoned match changes nothing.
- **Several games per account** at once (queues, lobbies, running matches), with no per-account cap by default; operators may set account and source limits.
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

Rated queues support two or more sides of any size, up to ten seats, including free-for-all and draws. Each
account has one rating and ladder position per rated ruleset, shared across its concurrent matches.
An unrated sibling uses that ladder for skill matching with a looser window and leaves it unchanged.

What is missing: password recovery and e-mail, ranked placements or partial scores, a ladder across several hosts (one process, one SQLite file), and rolling protocol compatibility (clients and
service update together).

## Get started: a development setup on one machine

The [example configuration](radovid.example.toml) runs neutral test fixtures: tic-tac-toe, a turn clock, and a
fixed-tick grid race. They require `python3` on `PATH`, bind only to loopback, and remain test fixtures under
`tests/`; none is part of the service binary. Run these commands from the repository root:

```sh
cargo build --release -p radovid
mkdir -p dev
target/release/radovid dev-cert dev/
cp radovid.example.toml dev/radovid.toml
target/release/radovid serve --config dev/radovid.toml
```

Clients connect to `wss://localhost:4460` and trust `dev/cert.pem`. The fixture clients in
[the game E2Es](crates/radovid/tests/games.rs) demonstrate registration, queueing, acceptance, direct game
connections, result reporting and ratings:

```sh
cargo test --profile test-release -p radovid --test games
```

For a ranked/unranked pair, use [the single-game example](examples/tic_tac_toe.toml): its two
rulesets share one ladder, and its unrated queue explicitly references the rated sibling.

For your own game, configure its executable or container image, wire fingerprint, arguments and public address
according to the [launcher contract](DEVELOPMENT.md#launcher-contract). The game owns its rules, network protocol
and client library; the service never imports them. The [generality audit](docs/generality.md) records guarantees
and remaining boundaries. [Protocol 16 coordination](docs/protocol_16.md) describes the client upgrade boundary
and the linked humans/everyone queues.

## For developers

[DEVELOPMENT.md](DEVELOPMENT.md) has the launcher contract a game must honour, every configuration key, the protocol,
how concurrent results are rated, the security model and the tests.

## License

MIT, see [LICENSE](LICENSE).
