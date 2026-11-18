# radovid

A matchmaking service for any game whose servers start from a command line: accounts, ranked and unranked queues,
lobbies, ratings and a ladder, and one game server launched per match, in a single process with a single SQLite file.

radovid knows nothing about your game. A ruleset says which program to start and how; radovid starts it when a match
is formed, hands each player the server's address and a ticket for their seat, and reads the result from the server's
own output. It never carries game traffic, and clients can't report outcomes, so there is nothing for them to forge.

**State:** young, built from source only; the protocol changes, and clients and the service update together.

**What you need:** a Unix system (developed and tested on Linux; the service uses Unix signals and file permissions,
so it does not build on Windows), Rust 1.99 or newer, git and a C compiler (SQLite is built in). The example games
below also need `python3`.

## Try it

A development setup on one machine, with the [example configuration](radovid.example.toml): three small test games
(tic-tac-toe, a turn clock and a grid race), on loopback only:

```sh
git clone https://github.com/alexstanovoy/radovid
cd radovid
cargo build --release -p radovid
mkdir -p dev
target/release/radovid dev-cert dev/
cp radovid.example.toml dev/radovid.toml
target/release/radovid serve --config dev/radovid.toml
```

Clients connect to `wss://localhost:4460` and trust `dev/cert.pem`. The example games' end-to-end tests
([crates/radovid/tests/games.rs](crates/radovid/tests/games.rs)) start a service of their own and show the whole path,
from registration and queueing to acceptance, direct game connections, results and ratings:

```sh
cargo test --profile test-release -p radovid --test games
```

For your own game, configure its executable or container image, the fingerprint of its wire protocol, its arguments
and public address as the [launcher contract](DEVELOPMENT.md#launcher-contract) says. The game keeps its rules,
network protocol and client library; the service never imports them. [examples/tic_tac_toe.toml](examples/tic_tac_toe.toml) pairs a ranked and an
unranked queue on one ladder.

## What works today

- **Accounts:** login and password (no e-mail, no recovery), human or bot. Players see each other as `nickname#id`;
  the login stays private.
- **Ranked and unranked queues** with rating windows that widen with waiting time. A found match must be accepted by
  everyone within a time limit; a decline or silence sends that player to a short cooldown and the others back to
  their places.
- **Lobbies:** listed, searched by name or owner, and joined at once; a locked one asks for its password, which only
  its owner sees. Slots by team, spectators, and a game mode the owner chooses.
- **Glicko-2 ratings** per mode, for two or more sides of any size up to ten seats, free-for-all and draws included (a
  draw is half a win; a crashed or abandoned match changes nothing); Humans and Bots leaderboards, nickname search and
  your own rank. An unranked queue can match by a ranked ladder without changing it.
- **Limits the operator sets** on games in flight per account and per network address, and on how many game servers
  run at once; formed matches wait their turn in order.
- **Replays and other match files,** kept for a set time and fetched by the players who sat in the match (or
  everyone, if you choose).
- **A practice ruleset** one player can start alone at once, e.g. a demo with cheats.
- **Admin:** bans with reason and expiry, account kinds, stopping a match.
- **Safe by default:** TLS 1.3 only, argon2id passwords, every limit bounded, tickets passed to game servers by
  environment only, no IP addresses or e-mails stored. Docker game servers run read-only with all capabilities
  dropped.

Clients speak WebSocket over TLS on one port; `radovid_client` is the Rust client library (blocking, a background
thread, or the browser's WebSocket). The service can also serve a web client's static files on the same origin.

## What is missing

Password recovery and e-mail; placements or partial scores in ratings; a ladder across several hosts (one process, one
SQLite file); clients and services of different protocol versions talking to each other.

## How it is built

[DEVELOPMENT.md](DEVELOPMENT.md) has the launcher contract a game must honour, every configuration key, the protocol,
how concurrent results are rated, the security model and the tests. [docs/](docs/) holds the notes of each protocol
revision, the database layout, and an audit of what the service assumes about a game
([docs/generality.md](docs/generality.md)).

## License

MIT, see [LICENSE](LICENSE).
