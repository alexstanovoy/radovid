# Protocol 20: small account avatars

Current and minimum versions are 20. Rebuild clients and service together. Nested
profile and ladder records gain a trailing avatar-version field, so version 19
cannot decode the new records. Every earlier wire position remains reserved.

`GetAvatar { account }` returns `Avatar { account, avatar: Option<Avatar> }`.
`SetAvatar { png }` changes only the authenticated account; `RemoveAvatar` clears
it. Both return the same reply with the resulting image or `None`.
`Avatar { version: AvatarVersion([u8;32]), png: Vec<u8> }` holds a SHA-256 content
hash and at most 512 PNG bytes. `PublicAccount.avatar` and `LadderRow.avatar`
carry only `Option<AvatarVersion>`, including own rows and legacy ladder pages.
Read the own profile with `Account { account: me.id }`.

Cache images by service identity, account and version. A new version replaces the
old cache reference; `None` means no image. Bytes are fetched separately, never
expanded into a leaderboard response. A concurrent change can make a fetch return
a newer version: cache under the version actually returned. There is no presence
service or avatar-change broadcast. Refresh metadata when refreshing the profile
or board.

## Image contract

The client prepares a static, non-interlaced 32×32 PNG, eight bits per sample, at
most 512 bytes including every header and checksum. Grayscale, RGB, indexed,
grayscale-alpha and RGBA are accepted. Only IHDR, optional PLTE/tRNS, contiguous
IDAT and IEND chunks are allowed. Strip text, color profiles and all other metadata;
animation, SVG, URLs, filesystem paths and trailing bytes are refused.

The service checks byte size, chunk framing and dimensions before invoking the
pixel decoder. One zlib stream must consume all IDAT data and produce exactly the
expected scanlines into a fixed 4,129-byte buffer, with its checksum verified.
Pixel decoding verifies PNG CRCs, uses a 4,096-byte output buffer and a 64-KiB
intermediate allocation budget. No file is opened and no URL is fetched. Stored
images are checked again before being served. A corrupt stored image causes a
recoverable internal error, never unvalidated bytes sent to clients.

`BadAvatar` means the format or image is invalid. The bounded protocol decoder
rejects upload/download byte vectors above 512 before allocating their claimed
size; an oversized raw wire upload is malformed, while SDK preflight returns
`BadAvatar` without sending it.

## Limits and visibility

Uploads and removals share one attempt per 60 seconds per account. Invalid images
and no-op removals consume the attempt. The timestamp is persisted, shared by all
sessions, and survives reconnects and service restarts. Early attempts receive
`Throttled { retry_after_ms }`. A failed database operation does not claim success.
There is no shared account upload bucket.

Reads share the existing four-per-second, burst-four account lookup budget with
profiles and boards. They use the bounded read-only pool off the hub actor.
Unknown, deleted and currently banned targets are hidden from other accounts as
`UnknownAccount`; an existing visible account without an image returns `None`.
A requester's own banned profile may be unranked. Deleting an account deletes its
avatar and quota row. There is at most one 512-byte image and one timestamp per
account; replacing an image keeps no history. Schema 4 upgrades atomically to 5;
schema 2/3 upgrades directly to 5 without losing account or rating data.
