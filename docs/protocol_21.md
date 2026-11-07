# Protocol 21: conditional avatar batches and canonical images

Current version is 21; minimum remains 20. Existing request/reply positions and
single-avatar calls remain supported. The new SDK advertises 21 so it never sends
a batch to an older service.

GetAvatars { accounts: Vec<AvatarQuery> } is request 45.
AvatarQuery { account, version: Option<AvatarVersion> } supplies the hash of bytes
actually cached by the caller. Use None when no image is cached; a row's metadata
hash alone does not mean the bytes are cached. One request accepts 1..100 distinct,
positive account IDs and uses one token from the existing account lookup quota.
Empty, oversized, duplicate or invalid-ID batches return BadAvatarBatch (error 60);
wire vectors above 100 are rejected before allocation.

Reply 18, Avatars { changes: Vec<AvatarChange> }, returns only changes.
AvatarChange { account, avatar: Option<Avatar> } contains a replacement image or
None to clear a cached image that is now absent, hidden, deleted or invalid.
Matching versions and absent images with no cached version are omitted. Hidden
and unknown accounts are indistinguishable from an account without an image.
The reply contains at most 100 entries and 512 image bytes per entry. The client
control decoder applies the same bounded image envelope to every batch entry.

Image preparation and upload byte/dimension limits from protocol 20 still apply.
The service decodes each upload and re-encodes its native eight-bit color type at
32x32 using fixed Balanced compression and Adaptive filtering. Only indexed
images retain PLTE, with unused entries removed and pixel indices remapped in
first-use order. Unused palette alpha entries are removed; valid pixel transparency
is retained, including grayscale/RGB transparent samples. The stored image must
also fit 512 bytes. An image that cannot fit after normalization is BadAvatar and
consumes the upload attempt. Versions hash the stored, re-encoded bytes. The
service does not store the original file or non-pixel suggested palettes.

Existing valid stored images are normalized lazily on fetch, without spending an
upload attempt or resetting its timestamp. A conditional storage update preserves
concurrent uploads. If storage temporarily cannot accept this maintenance write,
the caller still receives safe canonical bytes; a WARN records deferred storage.
Clients cache under the version actually returned, which can differ from an older
profile/board hint. Already canonical records require no maintenance write.

An invalid stored image or hash returns no image and logs WARN. Database I/O
failures still return the ordinary storage error. Metadata is a cache hint, not a
promise that an image remains readable. No raw invalid bytes reach a caller.
Schema 5 and the R4 writer/checkpoint/result durability policies are unchanged.
