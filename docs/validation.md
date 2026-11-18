# General-matchmaker validation

The gate script, `tools/gate.sh`, runs every test below; none is a manual-only check. Tests run with
`test-release` (optimized with assertions and overflow checks), product servers with `release`.

## Regression tests

Each test below failed on the code before its fix, as the middle column says, and passes now.

| Problem | What the test showed before the fix | What holds now |
|---|---|---|
| Lobby shape changed silently | `loading_a_ruleset_that_does_not_fit_lobbies_names_the_configuration_error` accepted 5v5 with `[1,1]` lobbies | A specific configuration error names the ruleset, required shape, and actual layout; every shipped example loads. |
| Missing global custom capacity | `custom_matches_have_a_global_cap_before_launch_and_lobby_countdown` accepted a second solo at capacity 1 | Waiting and running solo matches share a cap with lobby starts; stopping a match releases capacity. |
| IPv6 /64 rotation bypassed source limits | `ipv6_site_cap_combines_distinct_subnets_and_releases_cancelled_entries` accepted the third /64 in a /48 capped at 2 | Same-site admission fails, another /48 works, and cancellation releases capacity. |
| Numeric wire sentinel | Version-12 client test succeeded before the typed-limit transition | Older binaries receive `ProtoMismatch` before catalogs; numeric positions are retired, typed fields append. |
| FFA multiplied information | `one_ffa_match_carries_the_same_information_as_one_duel` gave RD 253.4046 for three sides versus 290.3190 for a duel | Equal-rating 3–10-side draws have the same RD and volatility change as one duel. |
| Unseated rated winner | Supervisor guard accepted an absent side; DB guard produced rating changes | Supervisor rejects the result; DB validation returns a specific recoverable error and leaves the transaction usable. |
| Interpreter pinned to a filesystem location | Fixture launcher used an absolute interpreter path | Fixtures resolve `python3` through `PATH`; missing Python fails the game E2Es and gate. |

Additional guard-only tests are labelled in source. The 1v1 golden guard captures fixed rating, deviation and
volatility values from commit `2f21ea7` of this repository, not from the implementation under test. Capture used that
revision's `crates/radovid/src/rating.rs` in a standalone `rustc --edition 2024 -O` harness with players
`1743.258/83.649/0.06` and `1302.115/312.872/0.06`, each win/loss/draw score. The fixed values and tolerances are
in `db::tests::one_on_one_rating_matches_origin_main_goldens`. The maximum-side finite-value and seated-winner
boundary tests are guard-only checks of internal contracts; they do not claim a reachable queue exploit.

## Commands

```sh
cargo test --profile test-release -p radovid --test generality --test capacity --test games
cargo test --profile test-release -p radovid --lib
cargo test --profile test-release -p radovid_proto
cargo clippy --all-targets --all-features -- -D warnings
cargo fmt --check
tools/gate.sh
```

The gate runs the ignored-test audit, fuzz-target compilation, clippy, browser-client compilation, all regular
unit/integration tests, the ignored launcher test against a real Docker engine/image, dependency policy, and
formatting last. A skipped stage fails.

## Clients

Clients are rebuilt with the protocol crate of the service they talk to: a client of another version
(`PROTO_VERSION` in `crates/radovid_proto/src/limits.rs`) receives `ProtoMismatch`. Callers that perform numeric
comparisons must match
`AccountGameLimit::Limited(n)` and `Unlimited`, rather than infer unlimited from a number. Each application
should retain its own bounded worker pool when the service imposes no per-account cap. Retired fields always
stay empty; no existing numeric value is assigned a new wire meaning.

Game-specific consumer tests and configuration examples live in each game's own repository. The service's
tests use only the neutral process fixtures and clients under `tests/`.
