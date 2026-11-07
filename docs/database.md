# SQLite readers, WAL bounds and result recovery

The service owns one writer turn and four read-only connections to one WAL database.
SQLite's database path is resolved before opening readers or measuring its WAL,
including when configuration names a symlink. Reader admission is FIFO, capped at
64 queued/running lookups. Each lookup holds one bounded snapshot.

After an autocommitted operation, a WAL above 4,000 pages (about 16 MiB with
4-KiB pages) schedules a background checkpoint. A dedicated connection runs
`PRAGMA wal_checkpoint(TRUNCATE)` with a 25-ms busy timeout. Reader admission
pauses while active service snapshots drain. No application writer turn is held
while draining, checkpointing or waiting to retry. Foreground auto-checkpointing
is disabled. Failed checkpoints log WARN and retry with a 250-ms minimum backoff
on subsequent database operations or the one-second maintenance tick.

An external reader or concurrent commit can prevent truncation. Normal writes
continue below the 512-MiB hard WAL threshold; only mutations past that threshold
are refused before execution. Reads remain available. Once the blocker releases
its snapshot, a successful retry restores write admission. The hard threshold can
be crossed by one admitted transaction; it does not constrain startup migrations
or writes by other processes. There is no wall-time guarantee for OS scheduling
or filesystem I/O. The soft threshold is a checkpoint trigger, not a size promise.

## Finished results

Before attempting the main transaction, each completed match is published in a
private directory beside the database, `<database>.pending_results`. Immutable,
size-bounded records contain an order, checksum and validated match result. Files
and directory entries are synced before publication is acknowledged. A result
transaction uses SQLite synchronous FULL; only after that durable commit is its
pending record removed. Ordinary database writes retain synchronous NORMAL.

A failed transaction retains its match slot and queues a retry. Ratings apply in
completion order, and clients receive a result only after commit. Restart replays
pending records before marking unfinished matches lost. Replaying an already
finished match cannot rate it twice. Complete temporary publications are recovered;
corrupt, conflicting or unrecognized records stop startup and remain available for
recovery. The directory holds at most262,144 pending identities, with at most ten
seats and64KiB of game report per record. Continued storage failure stops new game
admission as existing slots fill; it cannot create unlimited pending results.

## Backups

Use SQLite's `.backup` command or `VACUUM INTO` for a consistent main-database
copy. Do not copy only a live database file while ignoring its WAL. Avoid leaving
manual `BEGIN` transactions or read cursors open: they hold snapshots and can
prevent WAL restart even after the service's own readers drain.

A full service backup must also preserve pending results. Quiesce or stop the
service, then back up the main database and its pending-result directory together.
Keep all records intact, including temporary records; restore the directory beside
the restored database with the corresponding basename and private permissions.
An independent live copy of the directory and database is not an atomic backup of
both. A SQLite backup alone does not include results still awaiting main commit.

Small disk-backed tests cover sixteen overlapping lookup streams during2,000
transactions, external snapshot recovery, result retry/restart and exactly-once
rating. Population timings belong to the explicit benchmark, not the lib suite.
