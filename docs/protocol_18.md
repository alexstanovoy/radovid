# Protocol 18: category leaderboards and per-mode profile ranks

The API layout below was introduced in18. [Protocol19](protocol_19.md) supersedes
the lookup execution, moderation and rate/work-limit implementation described here.

Rebuild the service and every client together. The minimum and current version are 18:
`RatingSummary` and `Limits` have appended fields nested inside existing replies.
Older clients receive `ProtoMismatch`. Existing field positions and variant numbers keep
their meanings. The legacy `Ladder` request still returns its mixed-category, offset page.

## Leaderboard

An authenticated `Leaderboard { ruleset, kind, prefix }` request selects a rated ruleset
and `AccountKind::Human` or `Bot`. The caller must be admitted to the ruleset's audience.
An unrated sibling uses its `rating_ruleset` parent as the mode's ladder. Select that
parent's ID here; unrated IDs themselves return `NoQueue`.

`Reply::LeaderboardPage { page }` contains:

- `ruleset`, `kind`, `prefix`: the requested filter, echoed so clients can reject stale replies;
- `limit`: the configured response size, independent of the legacy ladder page limit;
- `total`: the number of qualifying accounts matching the prefix, before the response limit;
- `rows`: at most `limit` existing `LadderRow` records;
- `own`: the requester's row in their **own category**, even when another category is selected,
  the nickname search excludes them, or their rank is below the returned page.

Empty prefix lists the category. A prefix is at most 20 ASCII letters, digits or underscores;
matching is case-insensitive and underscores are literal. Other input returns
`BadLeaderboardSearch` before database work. Search covers the entire category, retaining
absolute ranks rather than numbering search results from one.

Each account with rated games has one ordinal position per mode/category. Ordering is stored
rating descending, deviation ascending, then account ID ascending, including ties. Deleted
accounts and zero-game records are excluded. Human/Bot means the account's current
classification: registration declares it, and an administrator can change it. It does not
verify whether a person or a program is operating a client. Reclassification moves the same
stored rating between the category views.

`own = None` means the requester has no qualifying rated record in this mode. Display an
unranked state rather than rank zero. A category-filtered table highlights or pins the own
row only when `own.kind == page.kind`. Its membership check uses the displayed rows if a
client applies a smaller display cap. The independent own metadata does not consume the
service page limit.

## Profiles and configuration

`RatingSummary.rank` is appended. `Account` returns ranks alongside ratings and game counts
for the currently served rated modes visible to the caller. Every returned played-mode
summary has a rank in that account's current category. A catalogued mode without a rating
record has no position; clients can still draw its unranked card from the catalog.

`limits.leaderboard_size` defaults to **100**, accepts 1 to 1000, and is served in
`Limits.leaderboard_size` before login and in per-ruleset limits queries. A service configures
at most 64 rated modes, so a profile reply is bounded. Decoders reject more than 1000 board
rows or 64 profile entries before client caches see them.

## Work and rate bounds

Leaderboard, profile and legacy ladder lookups share a four-per-second, burst-four account
budget across sessions. Recent lookup debt survives a disconnect until its tokens refill;
otherwise-idle account state is then pruned by the clock. The service also has a shared
eight-per-second, burst-eight budget. A rejected account request does not spend global
tokens, and global rejection does not spend an account token.

Rows, counts and the own row use one explicit SQLite read transaction. A profile's metadata
and every mode rank use one snapshot too. A single progress counter permits at most five
million SQLite VM instructions for the whole lookup, including its component statements.
The counter is cleared on success, error and unwind. Budget exhaustion or database contention
returns `Throttled { retry_after_ms }` for the whole request; it never silently drops the own
position. Very large ladders can exceed this work budget. The VM budget bounds database
work, not elapsed time; the existing SQLite busy timeout is five seconds per busy wait.

Clients should coalesce nickname edits, leave at least 300 ms between queries, keep at most
one query in flight, and retry throttling according to its delay. The echoed filter identifies
which response belongs to the current selection.
