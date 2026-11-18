# Protocol 22: bot versions

> History: what protocol version 22 changed. The version a build speaks is `PROTO_VERSION` in
> `crates/radovid_proto/src/limits.rs`; later versions keep every wire position described here.

The current and minimum control protocol versions were 22; service and clients were rebuilt together.
Every existing variant index and field position is preserved; mutable bot metadata positions
are reserved. The new `version` fields are appended to their records.

`ClientInfo.version` selects an immutable named identity on registration, login and resume.
It is mandatory for a bot and forbidden for a human. Credentials and session tokens belong
to the main account; a token can select any of that account's versions. Names are opaque,
case-sensitive, 1 to 32 printable ASCII bytes, without leading or trailing whitespace.
An exact existing name selects the existing version. Padded aliases are refused, not normalized.
An account can create at most 128 versions; existing names still work at the limit.

A selected version receives its own stable `AccountId`. `AccountInfo.main_account` and
`PublicAccount.main_account` identify its credential owner. `nickname` is the main account's
current display name; `version` is the version's exact name. Clients display them side by side.
The main bot account owns no rating. Each version starts independently at rating 1500 and
deviation 350 per rated mode, and owns its profile, history and leaderboard position.
Existing main-account bot ratings are discarded during the schema 6 upgrade, never inherited
(a match result still waiting in the pending-results journal when the service is upgraded is
credited to the main account it was recorded for, as before).

Matchmaking groups contain at most one participant of each main account, in ranked and
unranked queues, including repeated searches of one version. Manual lobbies stay unrestricted.
Limits on sessions, concurrent activities and decline cooldowns remain main-account limits.
Changing the main nickname updates each version's displayed main name, leaving version names
unchanged. Bans, password changes and account deletion apply to the credential owner and its
live versions. A main account with versions cannot be reclassified as human.

Specific refusals: `VersionRequired`, `VersionNotAllowed`, `BadVersion`, and `VersionLimit`.
There are no version renames, deletion, rating transfers or lifecycle operations.

Launchers can use `{seats_with_versions}` in their private environment to carry each
authenticated version as a separate hex-encoded field. `{seats}` retains its existing form,
but the names in `{seats}` and `{name_<slot>}` are no longer unique: two versions of one bot in
a manual lobby carry the bot's name twice (their seats, tickets and slots still differ), while
two different accounts with one nickname get the slot suffix.
A game must treat the launch reservation as authoritative over any client claim.

An actual account-kind change is refused while a match is pending, running, or waiting for
its result to commit. A same-kind request does nothing. A queued-only kind change keeps
its existing cancellation behavior, without a decline cooldown; conversion to bot removes the
former human rating. A changed account's sessions end (`BadSession`), and with them its
lobbies and searches.
