# Protocol 14 coordination

This records the version-14 upgrade. The current queue-audience boundary is
[protocol 16](protocol_16.md).

The service and every control client must use protocol 14 (`MIN_PROTO_VERSION = PROTO_VERSION = 14`). Older
binaries receive `ProtoMismatch` during `Hello`, before any catalog or account record. There is no rolling
compatibility window: nested records gained appended fields that older decoders cannot safely interpret.

Build and test the coordinated source series before changing a running service. Merge the service/protocol
change first, then each game's client adapter and configuration, then applications using those adapters. Keep
the running service and rebuilt clients on the same protocol boundary when resuming control traffic. The
source merge order does not require running a new service against old clients in the meantime.

## Consumer changes

- Numeric account-limit positions remain retired placeholders. The appended field is
  `AccountGameLimit::Limited(n)` or `Unlimited`; zero has no new active wire meaning. An unlimited service
  allowance does not remove an application's own worker/resource cap.
- `Limits` additionally reports global custom-match and IPv6-site capacities. Admission refusals are explicit.
  Match results update each account's one rating in its ruleset ladder, including concurrent team/FFA matches.
- Lobby clients must speak the selected ruleset's game fingerprint. A login or token resume is refused while
  that account holds a lobby of another game wire. Leave the incompatible lobby using its matching client
  before switching game clients.
- Set `lobby_teams` explicitly when the neutral `[1,1]` default cannot fit any configured ruleset. Configuration
  loading names mismatches rather than silently shrinking a declared game's sides.

## State and game-server boundary

The SQLite schema is unchanged. Accounts, ratings, history and session credentials remain in the same database;
control-session authentication still requires the rebuilt client. Lobbies are live service state. Finish or
explicitly stop active matches before a coordinated restart if their results must be kept: interrupted matches
follow the normal null-result/recovery rules.

This control-protocol change does not change any game's network protocol, replay format or launcher result
contract. Keep each game's server/client fingerprint matched independently. Game rules, server arguments and
integration guides stay with that game's client adapter; the service repository contains only neutral test
fixtures. No game-specific dependency is introduced into the service.
