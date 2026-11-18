# Protocol 25: avatars of 256 by 256 pixels

The current and minimum control protocol versions are 25. Rebuild service and clients together.
Every existing variant index and field position is preserved. `Reply::Avatars` gains one field,
`answered`, at the end.

## The image

An avatar is a static, non-interlaced PNG of exactly 256 by 256 pixels with 8-bit samples:
grayscale, grayscale with alpha, RGB, RGBA or indexed colour, with the transparency a PNG gives
each (`tRNS` for grayscale, RGB and indexed). It holds `IHDR`, an optional `PLTE`, an optional
`tRNS`, `IDAT` chunks and `IEND` and nothing else: text, colour profiles, animation and every
unknown chunk are refused wherever they sit. It is at most 49152 bytes (`MAX_AVATAR_BYTES`), so
that one upload is one control frame. Its one zlib stream must inflate to exactly the 256
scanlines: a stream that is small but expands further is stopped one byte past the image, so no
avatar can cost the service or a client more than the 196608 to 262144 bytes of the picture.

The service decodes every upload, checks each checksum, and stores the image encoded again from
its pixels with fixed settings (unused palette entries and the deflate encoder's choices are not
kept). A client that sends those bytes gets them back; `radovid_proto::encode_avatar_rgba`
produces them from any 256 by 256 picture, reducing its colours only as far as the limit needs,
and aims at 45056 bytes so that another build of the encoder cannot push the stored form past
the limit. Anything else is `BadAvatar` and spends the account's upload (one change a minute).

Decoding and encoding run beside the hub, so one upload never holds up the other sessions.
Clients decode with the same function (`radovid_proto::decode_avatar`), which allocates the
picture and no more and returns an error for any bytes.

## Batches

`GetAvatars` is unchanged: one to 100 distinct accounts and the versions the caller holds.
`Reply::Avatars { changes, answered }` covers the first `answered` queries. It carries at most
`MAX_AVATAR_REPLY_BYTES` (262144) of images; it stops before the query whose image would pass
that, always answers at least one, and the caller asks again about the queries after
`answered`. A page whose images are all cached costs no image bytes and is answered whole.
Changes that carry no image (an absent or hidden account) cost nothing.

Beyond the lookup quota (four lookups, refilled at four a second, per main account), the images
a main account is sent are repaid at 524288 bytes a second. An avatar read is refused with
`Throttled { retry_after_ms }` while more than 1048576 bytes are owed; a reply is charged after
it is known, so one reply can pass the burst. Other lookups do not wait for it.

A leaderboard page of 100 rows costs the lookup of its rows and, for avatars not cached, up to
49152 bytes each; a client fetches the rows on screen first.

## Stored avatars of 32 by 32 pixels

Database schema 7 makes the avatar table again, without the old images: they are of a size the
format no longer allows, and a picture that small blown up is not worth keeping. An account that
had one is shown without a picture until it sets another, and its upload quota starts afresh.
