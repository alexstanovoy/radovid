# radovid

radovid is the service around [bota](../bota): accounts, a matchmaking queue, custom lobbies,
Glicko-2 ratings, a ladder, admin bans, and the orchestration of one `bota_server` per match.
It is one process (tokio) with one SQLite file. Clients are native bots (`radovid_client`)
or a GUI; everything speaks WebSocket over TLS on one port.

```
radovid_proto   the wire types, limits, registry/wire.txt
radovid         the service (library + binary), the local and docker backends
radovid_client  a synchronous client: Client (control), GameLink (game), play()
```

`bota_proto <- radovid_proto <- {radovid, radovid_client}`; bota knows nothing of radovid.

## What it does

- **Accounts.** Register with a login and a password (no e-mail, no recovery), human or bot. The
  nickname is free-form, not unique, and changeable. Passwords are argon2id.
- **Queue and ladder.** One FIFO per queueable ruleset pairs the two oldest accounts. Queued
  matches are rated with Glicko-2 (1500, 350, 0.06, tau 0.5); a draw is half a win; a lost game
  server (`null`) changes nothing. The ladder shows whether a row is a bot or a human.
- **Lobbies.** Custom, unrated games: ask the owner, get let in, press ready; the owner's ready
  starts a 5 s countdown that nobody can cancel. The owner leaving closes the lobby.
- **One activity.** An account is in at most one of queue, lobby and match.
- **Matches.** One game server per match with reserved seats. The service attends it as a
  *referee* and records the result; the players connect through the service's `/game` path. A
  player may reconnect at any time (the new connection replaces the old one); the same account
  may add spectators that see only that seat's view.
- **Admin.** Ban (with reason and expiry), unban, change an account's kind, stop a match (a
  `null`). An admin is made on the host: `radovid admin --config radovid.toml <login>`.

## Run it (development)

```sh
# 1. the game server, from the bota checkout this tree builds against
cargo build --release -p bota_server        # in the bota checkout the path dependencies point at
# 2. a self-signed certificate and the service
cargo run --release -p radovid -- dev-cert dev/
cp radovid.example.toml radovid.toml        # point tls_cert/tls_key at dev/, backend at the binary
cargo run --release -p radovid -- serve --config radovid.toml
# 3. a bot, in another terminal
cargo run --release -p radovid_client --example solo_mid_bot -- wss://127.0.0.1:4460 bot_a 'a long password' dev/cert.pem
```

Start the example twice with different logins and the two bots are matched and play.
Browsers and `radovid_client::TlsConfig::webpki()` need a real certificate (Let's Encrypt on a
VPS); `TlsConfig::trust_pem` trusts a development certificate.

## Run it (docker)

The backend starts one container per match: read-only root, no volume, all capabilities dropped,
memory, CPU and pid limits, on a private network, labelled `radovid.match`, secrets in the
environment (not the command line). Only the service is reachable from outside.

```sh
rustup target add x86_64-unknown-linux-musl
cargo build --release --target x86_64-unknown-linux-musl -p bota_server --bin bota_server   # in bota
cp <bota>/target/x86_64-unknown-linux-musl/release/bota_server docker/
docker build -f docker/Dockerfile.bota_server -t bota_server:local docker/
docker network create radovid
# radovid.toml:  [backend] kind = "docker", image = "bota_server:local", network = "radovid"
```

The service reaches a container by its address on the network. A rootless docker daemon does not
route container addresses to the host; set `publish_loopback = true` there and the service uses a
loopback-only published port instead. At start the service kills containers an earlier run left
behind and records their matches as lost. Build the image from the bota commit the service is built
against: `bota_server --wire-fingerprint` and the service's `Welcome` print the same number when they
match.

## Protocol

`/control` speaks `radovid_proto`; `/game` speaks bota's protocol, bridged to the container. One binary
WebSocket message is exactly one frame (`u32 LE length | postcard`), the same bytes as on bota's
sockets and in replay files.

- The first message is `Hello { proto, bota_wire, client }`. The protocol version must match exactly
  (`Error::ProtoMismatch`); `bota_wire` is the fingerprint of bota's wire registry, and a client with
  another one can use accounts but not queue, create or ask into lobbies.
- Every client frame has a `seq`; the service answers it with exactly one `Reply` or `Refused`, and
  sends `Event`s on its own (`MatchFound`, `MatchResult`, lobby changes, `Bye`).
- A match is announced by `Event::MatchFound { handoff }`: the game URL and a `SeatKey` for the seat.
  The key goes into bota's `Hello`; the service looks the key up, connects to the container and
  copies frames. The key is also what a reconnect and the account's spectators use.
- `registry/wire.txt` lists every enum variant and field position; it only grows. Every wire change
  bumps `PROTO_VERSION`; there is no rolling compatibility for now.

## Security notes

- TLS 1.3 only (rustls); the game path is TLS too, because a sniffed seat key would take the seat.
- argon2id (64 MiB, t=3 by default), at most four hashes at once; an unknown login costs a hash too.
- Failed logins grow a delay per address and per login (after 3: 1, 2, 4 ... 300 s); an attempt
  inside the delay is not evaluated, and a right password after it always works, so nobody can lock
  anybody out. Counters live in memory.
- Session tokens are random, stored as SHA-256, expire after 30 days idle and are revoked by a
  password change. Seat keys are random, live in memory and in the container's environment only, and
  are compared in constant time.
- Everything is bounded: frame size, messages per second (token bucket), unauthenticated connections
  per address, sessions per account, list sizes, lobbies, registrations per hour per address, matches
  at once, connections per seat key. Hostile bytes close their own connection and nothing else.
- Game servers never learn the seed from the service: it is drawn from the OS inside the container, and
  the public match id is a separate label.

## Tests

`cargo test` runs the end-to-end tests over real WebSockets: a scripted stand-in game server
(`examples/fake_bota_server.rs`) for results, ratings, lobbies, reconnects and bans, and the real
`bota_server` for the referee path and two bots playing a whole match.
