# Protocol 19: indexed lookups outside matchmaking

The current and minimum control versions are19. Rebuild service and clients together.
All leaderboard page fields and SDK calls from protocol18 retain their meanings.
Error variant58, LookupTooExpensive, is appended: an unchanged request that exceeds the
work budget fails permanently rather than inviting an endless throttling retry.

## Indexed ranking

Ratings store the account kind. A played-row covering index on
(ruleset, kind, rating DESC, deviation, account) serves the top-N walk and the three
index-range counts for an account's absolute position. Account reclassification updates
its rating categories in the same database statement. Nickname search starts with the
normalised-nickname range index; an ordered rating-index pass assigns the selected
search hits their absolute ranks. It does not sort/materialise every account to find
a player's position.

Database schema2 upgrades atomically to3: the category is backfilled from accounts,
indexes and synchronisation triggers are installed, and existing accounts, credentials,
sessions and rating values are retained. An older service cannot open schema3.

## Independent bounded readers

Leaderboard, profile and legacy ladder work runs outside the matchmaking hub on four
read-only connections. Each connection has a FIFO turn; at most64 reads may be pending.
A caller has at most one lookup in flight and its own four-per-second, burst-four budget,
shared by sessions and retained across reconnects until refill. There is no shared
service-wide request-rate bucket for another account to exhaust.

The hub validates/adopts each request and continues processing queues, lobbies and game
events while a reader waits. Completion releases the account's in-flight state before
its response is delivered. Read-only connections cannot acquire the account/match writer's
application mutex. A snapshot transaction covers each reply.

One50-million-instruction SQLite work counter spans a lookup, including every profile
mode; the hook is cleared on success, error and unwind. If this final safety bound trips,
LookupTooExpensive has no retry delay. Throttled remains temporary: account quota,
an already-running lookup, full read capacity or database contention. Reader busy waits
are bounded to50ms; the writer retains its existing5s busy timeout.

Response limits remain operator-configurable1..1000 (default100), with at most64
rated modes in a profile. Menu clients can cap visible rows to100. Highlight/pin own
metadata only for the selected kind and test membership against the displayed rows.
