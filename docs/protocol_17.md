# Protocol 17: unrated queues and rating windows

Rebuild the service and every client together. The minimum and current version are 17:
the catalog and limits contain appended fields nested in existing replies. Older clients
receive `ProtoMismatch`. Existing variant numbers and field positions retain their meanings.

`RulesetInfo.rating_ruleset` names the rated sibling used by an unrated queue for skill
matching. `RulesetInfo.rating_window` is the effective policy, or `None` for a ruleset
without a queue. The existing `rated` field still determines whether a queued game changes
ratings. Queue admission and handoff requests are unchanged: select the desired ruleset ID.

`Limits.ranked_window` and `unranked_window` report the default policies. A limits query
for either sibling reports both resolved policies for that mode, including ruleset overrides.
Each policy is a `RatingWindow { initial, widen_per_minute, maximum }`, measured in rating
points. After `waiting_ms`, its width is:

```text
min(maximum, initial + floor(widen_per_minute * waiting_ms / 60000))
```

Every member must accept the group's full rating spread, including team and free-for-all
games. An older member's wider window cannot override a newcomer’s narrower one. A clock
tick retries matching without requiring another join. A failed proposal retains the original
waiting time. Matchmaking uses the rated sibling's stored rating, or 1500 for a newcomer;
a concurrent rated result refreshes every waiting search for that account and ladder.

Ranked waiting time has no upper bound. If the available players remain farther apart
than their configured maximum windows, waiting longer cannot make them compatible.
Unranked is the configured fallback with a looser window; the service never silently
widens ranked beyond its cap or switches a ranked search into unranked play.

`QueueEntry.window_capped` reports that this search's own window has reached its
maximum, including constant windows. A transition is sent through `QueueStatus` even
when position and queue length are unchanged. This is not an opponent-distance estimate
or a guarantee that a match will be found. A client can suggest the configured unranked
sibling for a capped ranked search; changing modes remains the player's choice.

Only a rated result updates the ladder. An unrated handoff/result has `rated = false`, and
its result seats have no `before`/`after` rating changes. Its handoff can show existing ratings
from the sibling's ladder. An unrated ruleset does not acquire a separate ladder.

Searches remain independent: an account can enter multiple simultaneous games, but it
cannot occupy two seats of one game. The oldest eligible search is preferred; distinct
network sources are preferred among compatible partners. This is greedy matching, not
a maximum-cardinality optimization or protection against collusion.

The service refills a shared budget of 65,536 candidate inspections on every clock tick
across pairing requests and retries. It rebuilds the rating index after selecting each group, counting
each rebuild against that budget. Queue capacity cannot exceed 65,536. Each
(ruleset, audience) queue rotates when
work is deferred; the next tick resumes it. At the hard queue limit, this intentionally permits
only one full-queue selection per tick. The default capacity is 256. Each selection uses
linear storage and bounded-neighbor rating searches; there are at most ten seats per group.
