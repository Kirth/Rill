# Streams (parked)

The runtime has a `Value::Stream` variant and a `Stream.tail` method sketch for
incremental processing of long-running command output (think `tail -f` piped
into Rill values without buffering the whole output).

The feature is neither finished nor removed: it is not reachable from the
parser, has no type-checker support, and is not documented in the reference.
It stays in the tree so the design can be picked up later without re-deriving
the plumbing. Until then, consume process output with `| .lines()`, which waits
for the process to finish, or `spawn()`/`await()` for background jobs.
