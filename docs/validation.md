# General-matchmaker validation

The ordinary `tools/gate.sh` covers these tests; none is an unnoticed manual-only check. Tests run with
`test-release` (optimized with assertions and overflow checks), product servers with `release`.

## Fail-first evidence

| Finding | Reproduction before enforcement | Protected result |
|---|---|---|
| Lobby shape changed silently | `loading_a_ruleset_that_does_not_fit_lobbies_names_the_configuration_error` accepted 5v5 with `[1,1]` lobbies | A specific configuration error names the ruleset, required shape, and actual layout; every shipped example loads. |
| Missing global custom capacity | `custom_matches_have_a_global_cap_before_launch_and_lobby_countdown` accepted a second solo at capacity 1 | Waiting and running solo matches share a cap with lobby starts; stopping a match releases capacity. |
| IPv6 /64 rotation bypassed source limits | `ipv6_site_cap_combines_distinct_subnets_and_releases_cancelled_entries` accepted the third /64 in a /48 capped at 2 | Same-site admission fails, another /48 works, and cancellation releases capacity. |
| Numeric wire sentinel | Version-12 client test succeeded before the typed-limit transition | Older binaries receive `ProtoMismatch` before catalogs; numeric positions are retired, typed fields append. |
| FFA multiplied information | `one_ffa_match_carries_the_same_information_as_one_duel` gave RD 253.4046 for three sides versus 290.3190 for a duel | Equal-rating 3–10-side draws have the same RD and volatility change as one duel. |
| Unseated rated winner | Supervisor guard accepted an absent side; DB guard produced rating changes | Supervisor rejects the result; DB validation returns a specific recoverable error and leaves the transaction usable. |
| Interpreter pinned to a filesystem location | Fixture launcher used an absolute interpreter path | Fixtures resolve `python3` through `PATH`; missing Python fails the game E2Es and gate. |

Additional guard-only tests are labelled in source. The 1v1 golden guard captures fixed rating, deviation and
volatility values from `origin/main` commit `2f21ea7`, not from the implementation under test. Capture used that
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
cargo machete --fix
# Format after the implementation checks, then commit and run the final-head gate.
cargo fmt
tools/gate.sh
```

The gate runs the ignored-test audit, fuzz-target compilation, clippy, browser-client compilation, all regular
unit/integration tests, the ignored launcher test against a real Docker engine/image, dependency policy, and
formatting last. A skipped stage fails. Independent model review found no remaining production blockers.

## Read-only reference consumer

The optional bota example remains a consumer, never a dependency. Check its unmodified `bota_radovid` crate
against these local client/protocol crates with the `e2e` feature enabled. Its production client requests limits,
caches the `Limits` record and returns it (`client.rs`); it performs no numeric-cap queue filtering. Rebuilding
therefore picks up the typed field without a production client logic change. Its old integer-default assertion
must become `AccountGameLimit::Unlimited`; the test cannot compare the typed value to integer `8`.

Protocol version/minimum 14 explicitly refuses older binaries. Callers that perform numeric comparisons must
match `Limited(n)` and `Unlimited` rather than infer unlimited from a number. Retired fields always stay empty;
no existing numeric value is assigned a new wire meaning.

The read-only check at bota revision `be6cafc` passed `cargo check --release -p bota_radovid --features e2e --lib`
and its one library test with `test-release`. The all-targets test build stops at the owner-maintained assertion
in `tests/matchmaking.rs:39`: expected `(AccountGameLimit, u32)`, found integer tuple `(8, 120_000)`. The required
change is `(AccountGameLimit::Unlimited, 120_000)`. No bota source or tracked file was edited; the integration
suite is reported as not run, rather than passed.
