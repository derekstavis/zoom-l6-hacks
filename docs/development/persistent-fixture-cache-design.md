# Persistent fixture cache design

## Checkout ownership

Each canonical checkout directory owns a separate cache. Two agents in the
same checkout may share that cache; agents in different worktrees must never
share entries, locks, accounting or eviction decisions, even when their
source contents and requested cache parent are identical. Symlinks to the
same checkout resolve to the same owner. Moving a checkout creates a new
owner; the old cache is eligible for explicit cleanup. The prototype uses a
SHA-256 of the canonical checkout path to partition an explicitly supplied
parent. A future default can place the parent under that checkout's ignored
`target/`, which makes ownership and cleanup visible.

Cache contents include firmware, NOR images and SD images. They must remain
local and ignored, with normal user-only access. Checkout isolation does not
provide filesystem security against another process with the same user ID.
Do not place the cache on a shared network filesystem without verifying its
lock and atomic-rename guarantees.

## Inputs and identity

A cache hit is correct only when every input that could change the generated
bytes is identified. This includes stock package bytes, resolved NOR/state and
card specs, all copied file bytes, patch options and sources, generated patch
templates, code and dependencies of `l6fw`, `l6nor`, `sd-image`, `l6patch`
and the compiler pipeline. Fixture types should have versioned, typed keys
that identify only their actual dependencies. Hash contents, not paths, except
for paths whose spelling changes output. A run still records and verifies the
identities of the copies it actually uses.

For native patch builds the key also needs the **actual resolved** compiler,
linker and LLVM tools, plus rust-src and target libraries used to compile the
payload. Version strings and paths alone are insufficient: a tool can be
replaced at the same path. Resolve tools once using the same logic as the
builder, hash the relevant executable and input bytes, and pass those exact
resolved paths to the build. Detect replacement between hashing and execution
or fail closed. Environment options that affect output belong in the key;
avoid putting unrelated or secret environment values in persisted metadata.
The prototype additionally fingerprints resolved tool bytes and their dynamic
dependencies, and uses a metadata-checked snapshot to avoid rehashing them on
every run. It verifies selections around builds, but does not yet pass one
pinned set of tool paths through the entire builder; loader behavior and
cross-platform qualification remain open work. The checkout hash is also
conservative and can invalidate unrelated fixtures.

## Publication, use and eviction

A cached entry is immutable after publication. Build into a unique staging
area, verify its contents and manifest, then publish atomically while holding
an entry lock. An incomplete or corrupt entry is never returned as a hit.
State and card images given to a test are independent copies or verified
copy-on-write clones. Immutable firmware paths may remain in use by the
emulator for the entire run; copying only the mutable files is not sufficient
reason to delete an entry immediately after lookup.

Eviction therefore needs a **run-duration lease**, not just a lock during
lookup. A practical first implementation is a shared lock held for the life
of a run on every namespace the run uses, with a separate stable lock file
outside the namespace directory. Eviction takes a nonblocking exclusive lease
before examining or removing a namespace and skips it if any run holds a
shared lease. Kernel-released locks handle crashes; do not infer liveness from
PID files or delete lock files while another process could open them. Entry
builders remain protected by per-entry locks. Define lock order (checkout
maintenance, namespace lease, entry) and never hold a maintenance lock while
waiting for a live run to finish.

A separate per-checkout maintenance operation can remove least-recently-used
inactive namespaces, using an explicitly recorded last-use timestamp rather
than filesystem access time. Candidate initial policy: a configurable size
budget and age limit, plus an explicit inspect/clear command; choose defaults
after measuring real cache sizes. Account for sparse files and APFS clones:
logical image length is misleading and filesystem allocated-block counts may
still overcount shared extents. Report this as an estimate. Recheck accounting
and lease status under the maintenance lock, remove only inside the owning
checkout's cache root, and use a soft budget if all candidates are active.
A cache miss or failed eviction must not turn an otherwise valid test into a
false pass. Cleanup of abandoned staging directories must observe the same
locks and not race active builders.

The prototype holds a shared **checkout-wide** lease for each run.
`cargo l6test cache clean --persistent-cache DIRECTORY` takes the matching
exclusive lease without waiting and removes only that checkout’s entries;
it refuses while any run holds a shared lease. Its stable lock file is left
in the parent. Automatic eviction and finer-grained namespace leases are not
implemented. Do not manually delete the cache while a run is active.

## Validation needed before making this the default

- Identical inputs produce identical fixture identities with caching disabled,
  cold, warm and after corruption; changed NOR code, patch code, templates,
  firmware and tool bytes cause the appropriate misses.
- Simultaneous processes in one checkout share a build safely; identical
  checkouts in different worktrees never share cache state or eviction.
- Forced interruption during build, publication and eviction leaves no partial
  entry usable as a hit. Eviction while a guest is running cannot remove its
  firmware or any leased namespace.
- Exercise non-cloning filesystems and concurrent disk limits, then compare
  cold and warm representative runs with fixed executable and fixture hashes.
  Report wall time and disk usage separately, including cache-validation and
  fingerprint overhead.
