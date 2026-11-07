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

Database schema2/3 upgrades atomically to4: the category/visibility keys are backfilled from accounts,
indexes and synchronisation triggers are installed, and existing accounts, credentials,
sessions and rating values are retained. An older service cannot open schema4.

## Independent bounded readers

Leaderboard, profile and legacy ladder work runs outside the matchmaking hub on four
read-only connections. One shared FIFO queue feeds free connections; at most64 reads may be pending.
A caller has at most one lookup in flight and its own four-per-second, burst-four budget,
shared by sessions and retained across reconnects until refill. There is no shared
service-wide request-rate bucket for another account to exhaust.

The hub validates/adopts each request and continues processing queues, lobbies and game
events while a reader waits. Completion releases the account's in-flight state before
its response is delivered. Read-only connections cannot acquire the account/match writer's
application mutex. A snapshot transaction covers each reply.

One five-million-instruction SQLite work counter spans a lookup, including every profile
mode; the hook is cleared on success, error and unwind. If this final safety bound trips,
LookupTooExpensive has no retry delay. Throttled remains temporary: account quota,
an already-running lookup, full read capacity or database contention. Reader busy waits
are bounded to50ms; the writer retains its existing5s busy timeout.

Response limits remain operator-configurable1..1000 (default100), with at most64
rated modes in a profile. Menu clients can cap visible rows to100. Highlight/pin own
metadata only for the selected kind and test membership against the displayed rows.

## Moderation visibility

Accounts with a currently active ban are excluded from board/search rows, counts and
rank positions, including the legacy ladder. Indexed visibility is read consistently throughout each snapshot. A bounded tick sweep
restores expired bans to the same stored rating (see the correction below). Other callers cannot fetch a banned account's public profile;
the requester's own profile can show metadata without a ranked position. Human rows
omit bot-version metadata in both ladder APIs, including after reclassification.

## Ranking visibility and reader scheduling correction

Schema 4 materialises `ratings.hidden` and a normalized nickname key. Visible,
played rows have category/global order indexes and a prefix index. The eligibility
columns also appear at the end of these indexes so SQLite can prove the walks are
covering. Counts and rank walks never join every entry to `accounts`. Only the
bounded selected page loads account metadata. Legacy offset paging selects account
keys before loading metadata. Own/search ranks remain exact ordinal positions in
the snapshot; a shared ordered pass ranks search hits and the requester's own row.

Per-mode/category totals are maintained by database triggers in the same statement
as rating insertion, updates, deletion, account reclassification and visibility
changes. Board totals are primary-key reads, including the two categories of the
legacy board. Prefix totals use a covering prefix-range count. Nickname changes
update every stored rating key in the account's transaction.

Ban, unban and deletion change indexed visibility atomically. Expiring timed bans
are restored by the hub's one-second tick, in batches of at most 256 accounts. Only
one sweep may be outstanding. A large simultaneous expiry backlog is drained over
subsequent ticks; an expired account can remain unranked until its batch runs. The
profile's active-ban check still uses the current clock. Permanent bans and wrapped
negative timestamps are never expired by the sweep. Schema 2/3 upgrades to 4 in one
transaction, preserving account/rating data and backfilling visibility and totals.

All four readers draw from one FIFO admission queue, not a queue per connection.
An idle connection immediately serves the oldest waiting lookup. Queued/running
work remains capped at 64, with one pending lookup per account. A reply has one
**five-million-instruction** budget, shared across all its statements and profile
modes. The deterministic instruction bound limits work without classifying a slow
machine as a permanently invalid request. Exhaustion remains `LookupTooExpensive`,
not an invitation to retry unchanged. Very large searches or multi-mode profiles
can reach that final bound; the service does not hold unbounded read snapshots.
The response shapes, protocol minimum 19 and SDK calls are unchanged.
