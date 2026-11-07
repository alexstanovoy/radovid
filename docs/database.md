# SQLite readers and WAL bounds

The service owns one writer turn and four read-only connections to one WAL database.
Reader admission is FIFO, capped at 64 queued/running lookups. Each lookup holds
one snapshot. Read queries have fixed instruction budgets, with profile ranks
budgeted separately per mode.

After an autocommitted operation, the writer checks the WAL file size. Beyond
4,000 pages (including the WAL frame headers: about 16 MiB at the default 4-KiB
page size), it transfers its next turn to a background checkpoint. It returns
the already committed result without waiting for the checkpoint.

The checkpoint first closes reader admission and drains active snapshots. Waiting
readers stay within the same 64-job bound. The writer turn remains reserved, so
new commits cannot grow the log while snapshots drain. The blocking worker then
runs `PRAGMA wal_checkpoint(TRUNCATE)` with the writer's five-second busy timeout.
On success it releases admission and the next writer turn. This creates a gap
in overlapping snapshots, allowing SQLite to restart and truncate the log.

The steady-state bound is the threshold plus the one transaction crossing it;
transaction sizes and query work are bounded by their service operations. It is
not a hard quota on existing database files, startup migrations or writes from
other processes. There is no claim of a wall-time deadline for OS scheduling or
filesystem I/O.

An external SQLite connection can prevent truncation. If a checkpoint is busy or
fails, subsequent writer operations fail before executing and schedule another
background attempt. The service does not keep accepting mutations while an
unresettable log grows. Already committed mutations remain successful; a blocked
next mutation is a recoverable storage error. Read-only lookup work can continue.
Once the external reader releases its snapshot and a retry succeeds, writes resume.
A checkpoint panic also leaves the writer policy blocked rather than silently
resuming unrestricted growth.

Tests use a small disk-backed fixture to keep sixteen overlapping lookup streams
active during 2,000 transactions, assert a storage bound, check reader/writer
admission during snapshot drain, and verify failure/recovery with an external
checkpoint blocker. Timing diagnostics belong to the explicit population
benchmark, not the lib suite.
