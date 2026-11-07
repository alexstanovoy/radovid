# General matchmaker audit

Audit baseline: `2f21ea7`. Findings were posted before implementation.

| Priority | Finding at baseline | Evidence | Resolution |
|---|---|---|---|
| High | Humans defaulted to 2 concurrent activities, bots to 8; unlimited was rejected | `config.rs:199,722,938`, `hub/limits.rs:46` | Config zero means unlimited; appended typed wire limits distinguish `Limited(n)` and `Unlimited`. Positive caps still work. Global custom and IPv6-site admission are bounded as described below. |
| High | Rated rulesets required exactly two players | `config.rs:923`, `db.rs:714` | Up to ten seats on two or more sides, unequal teams and FFA included; one Glicko-2 rating per account per ruleset. |
| Medium | Default ten-seat lobbies and primary config assumed a particular game | `config.rs:88`, `radovid.example.toml:9` | Default two single-player sides; primary config and examples use neutral fixtures; game-specific integrations belong to their own repositories. |
| Medium | Lobby result validation used queue sides instead of actual launch seats | `supervisor.rs:110,138` | Additional lobby sides and compacted leaver slots are validated against reserved seats. Configured unoccupied opponents remain valid for solo games. |
| Boundary | Lobby layout is service-wide, not per ruleset | `config.rs:40,696` | Queues use each ruleset's shape; custom lobbies use the shared layout. Config loading rejects ruleset sides that do not fit it; shipped examples set their layout explicitly. |
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
one row per account. Numerical tests protect distinct team means/deviations, loser-versus-loser draws and a single-match information
budget: opposing sides divide both information and gain by `side_count - 1`. A 3–10-side equal-rating FFA has
the same RD change as a duel. Golden 1v1 values captured from the audit baseline protect the original formula.
The ten-side finite-value test is guard-only; it did not reproduce a reported bug. Existing matchmaking tests cover
concurrent result-arrival transactions and null results.

`tests/generality.rs` covers queryable unlimited defaults, explicit caps, default lobby shape, compacted leaver
slots and solo opponents without a reserved player. All tests run in the ordinary test stage of `tools/gate.sh`.
The Python fixture is never compiled into, imported by, or invoked from the production binary. It limits peers,
line lengths, operation count and runtime, and never prints credentials in reports or diagnostics.

## Service bounds and shared networks

Unlimited per account removes only the account cap. `queue_capacity` (default 256) bounds queue entries;
the hard ceiling is 65,536. Ranked and explicitly paired unrated queues use mutual widening
rating windows; an unrated result never changes the referenced rated ladder. Pairing shares
a 65,536-candidate work budget per second and resumes deferred work on ticks, rotating queues.
Each selected group rebuilds its index under that same budget; at the hard queue ceiling
this allows one full-queue selection per tick. See [protocol 17](protocol_17.md).
`game_capacity` (8) bounds running servers, and pairing pauses when pending proposals and queued matches reach
that capacity. `custom_match_capacity` (256) independently bounds all lobby countdowns plus starting, waiting
and running lobby/solo matches across all sources, before a launch slot is available. A finished or stopped
match releases its admission capacity. Open lobbies remain bounded separately by `MAX_LOBBIES` (256), with at
most ten seats each. Unlimited accounts cannot create an unbounded backlog of solo matches.

`games_per_source` (8) counts all in-flight activities for an IPv4 address or IPv6 /64.
`games_per_ipv6_48` (64) additionally aggregates the /64 sources of one IPv6 /48. IPv4-mapped IPv6 addresses
keep IPv4 semantics. Both source policies are operator choices: shared NATs and large IPv6 sites can contain
many legitimate players, so tighter caps trade access for abuse resistance. Raise the configurable caps for
the intended population; global custom, queue and running-server bounds still apply. The global custom and
IPv6-site capacities must be between 1 and 65536 and are queryable through `Limits`.

The capacity tests exercise real-service solo/lobby admission, including waiting matches, countdown
reservations and released capacity. A component E2E injects authenticated source keys at the real hub boundary
for distinct IPv6 /64s in a shared /48: the host network and forwarding headers are never changed or trusted.

## Compatibility

Protocol version 13 retired the original numeric account-limit fields, retaining their reserved positions
and payload types, and appended `AccountGameLimit::Limited(n)` / `Unlimited`. Version 14 appends custom and
IPv6-site capacity fields and refusal variants. These nested record additions require version 14 clients;
older binaries receive `ProtoMismatch` before receiving catalogs. No numeric value has acquired a new wire
meaning. Zero is only the configuration spelling for unlimited, or an empty retired placeholder.

Clients must rebuild and read the explicit limit enum. Existing positive config values and launcher/result
syntax remain valid. Per-ruleset ladders and single-account ratings across concurrent matches are preserved.

See [validation.md](validation.md) for commands, fail-first evidence and the read-only consumer check.
