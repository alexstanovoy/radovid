# General matchmaker audit

Audit baseline: `2f21ea7`. Findings were posted before implementation.

| Priority | Finding at baseline | Evidence | Resolution |
|---|---|---|---|
| High | Humans defaulted to 2 concurrent activities, bots to 8; unlimited was rejected | `config.rs:199,722,938`, `hub/limits.rs:46` | Zero means unlimited, advertised by `Limits` and `RulesetInfo`; positive caps still work. Source and service capacity remain bounded. |
| High | Rated rulesets required exactly two players | `config.rs:923`, `db.rs:714` | Up to ten seats on two or more sides, unequal teams and FFA included; one Glicko-2 rating per account per ruleset. |
| Medium | Default ten-seat lobbies and primary config assumed a particular game | `config.rs:88`, `radovid.example.toml:9` | Default two single-player sides; primary config is generic, bota is an optional example under `examples/`. |
| Medium | Lobby result validation used queue sides instead of actual launch seats | `supervisor.rs:110,138` | Additional lobby sides and compacted leaver slots are validated against reserved seats. Configured unoccupied opponents remain valid for solo games. |
| Boundary | Lobby layout is service-wide, not per ruleset | `config.rs:40,696` | Retained explicitly: queues use each ruleset's shape; custom lobbies use the configured shared layout. |
| Boundary | Outcome is one winning side, draw, or null; no placements or arbitrary numeric scores | `radovid_proto/src/model.rs:38`, `launcher/lines.rs:119` | Retained explicitly. A report can carry arbitrary game scores; rating uses winner-versus-other sides, with losing sides tied. |
| Boundary | Legacy artifact keys and API use “replay” | `config.rs:271`, `radovid_proto/src/message.rs:166` | Kept for compatibility; arbitrary named artifacts already work. Tests fetch a game record with no replay semantics. |
| Boundary | Startup/match deadlines are configurable; exit grace is 10 seconds, lobby countdown 5 seconds | `supervisor.rs:16,80,94`, `radovid_proto/src/limits.rs` | Operational bounds retained. Game clocks are opaque parameters owned and enforced by each game server. |

Paths in the evidence column are relative to `crates/radovid/src/` unless another crate is named. Line numbers
refer to the audit baseline; edited files have moved. No tower, map, tick-rate, hero, or game-protocol verdict
exists in the matchmaking core. Dependency direction remains game → radovid client/protocol.

## Executable proof

`crates/radovid/tests/games.rs` starts the real TLS/WebSocket service and uses `radovid_client` for registration,
queues, acceptance, handoffs, results, history, artifacts and ladders. Each game server is a separate Python 3
process launched by the production launcher. It receives reserved seats through environment variables, binds the
assigned loopback port, authenticates opaque tickets using constant-time comparison, and reports only through its
own stdout. Game traffic goes directly between socket clients and the game process.

- Tic-tac-toe: legal alternating moves produce a rated win and draw. Out-of-turn moves, invalid credentials and
  simultaneous seat theft are refused.
- Clock game: two configured time controls flow through launcher arguments and catalog bytes untouched. When
  the active player's clock runs out, the other player wins and both ratings update. Tests wait for the game
  result rather than asserting elapsed wall time.
- Fixed-tick grid race: rated 2v2, three-way FFA, unequal `[1,2,1]` sides, and team/FFA draws. Clients send inputs
  throughout play; one disconnects and reconnects with its original credentials. Eight accepted moves per player
  decide distance, so scheduler speed does not pick the winner. State advances on a 10 ms tick.
- A three-side custom lobby with a two-side queue ruleset reports the actual third side's win.
- Three unrelated game fingerprints coexist in a single service, each independently launched, played and rated.
  A client claiming the wrong game fingerprint is refused admission.

Every rated result is checked against all clients, persisted history, named-artifact bytes and the ladder, with
one row per account. Pure numerical tests additionally protect distinct team means/deviations, loser-versus-loser
draws, exact preservation of the old 1v1 formula, and the ten-side bound. Existing matchmaking tests cover
concurrent result-arrival transactions and null results.

`tests/generality.rs` covers queryable unlimited defaults, explicit caps, default lobby shape, compacted leaver
slots and solo opponents without a reserved player. All tests run in the ordinary test stage of `tools/gate.sh`.
The Python fixture is never compiled into, imported by, or invoked from the production binary. It limits peers,
line lengths, operation count and runtime, and never prints credentials in reports or diagnostics.

## Compatibility

No wire positions, fields, or variants changed. Existing positive account caps and launcher/result syntax remain
valid. Existing clients may display zero as unlimited; clients that disable actions based on a positive-only limit
need to adopt that documented meaning. The game client libraries own that presentation.
