# Protocol 15: queue audiences

Rebuild the service and control clients together: `MIN_PROTO_VERSION = PROTO_VERSION = 15`.
Older binaries receive `ProtoMismatch` before catalogs. All existing wire positions and
payload types remain reserved; the new request, reply, reason and record fields are appended.

Each rated ruleset has two FIFO queues: **humans** (`Audience::HumansOnly`) and **everyone**
(`Audience::Everyone`). Bots can enter everyone only. Ruleset access and game fingerprints
still apply to either queue. Lobbies retain their independent audience settings, including
`BotsOnly`; there is no bots-only queue.

`QueueJoinAudiences { ruleset, audiences }` starts one search in one or two distinct queues.
The service rejects an empty, repeated, oversized or bots-only selection. It admits all
entries together or none: global `queue_capacity` counts the actual entries, while account
and source game limits count one linked search as one potential game.

`QueuesJoined` returns both entries. Each entry and proposal names its audience and its
`search`, the first entry's ID. Existing `QueueJoin` retains its original meaning: one
independent entry in everyone. Several searches, including several for the same ruleset,
remain independent and may lead to concurrent games.
Decoders reject empty or oversized batches before allocation, and reject inconsistent
search/ruleset identifiers, repeated entries or audiences, and unsupported audiences.

A proposal reserves every entry of its search, preventing another queue from claiming the
same search. The service considers humans first when both queues can fill at once. If the
proposal fails, accepters retain both original queue places. Cancellation, decline, timeout
or loss of the account's last session removes every entry of the affected search. A decline
or timeout keeps the normal configured cooldown. Another live session keeps the search alive.
An administrative account-kind change cancels its outstanding searches without a decline
penalty, so an existing human-only proposal cannot acquire a newly classified bot.

Acceptance consumes the entire search in one hub operation. Every consumed entry receives
`QueueLeft { reason: Matched, cooldown_ms: 0 }` before game handoff. Clients must remove those
exact entries; a `MatchQueued` standing update must never guess which concurrent search ended.
No launcher, game-wire, artifact or rating formula changes are involved.

Clients already receive `Limits.accept_timeout_ms` and `decline_cooldown_ms`; proposal deadlines
and decline countdowns continue to use those service settings.
