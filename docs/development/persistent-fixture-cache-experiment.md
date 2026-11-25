# Persistent fixture cache experiment

This branch adds an opt-in `--persistent-cache DIRECTORY` experiment to
`l6test`. Without that option, the existing per-run cache is unchanged.
Use a local, ignored directory outside the checkout; its contents include
copyrighted firmware and generated SD and NOR images. Remove it when no
longer needed. Do not commit or share its contents. To remove only this
checkout’s entries, run:

```sh
cargo l6test cache clean --persistent-cache DIRECTORY
```

The command refuses to clean while a run holds the checkout cache lease.
It leaves other checkouts and the stable lock file untouched.

Each canonical checkout receives a separate cache root even if a parent is
shared. The namespace hashes the running driver, tracked and non-ignored untracked
checkout files (including patch sources, `l6nor`, generated templates and
`Cargo.lock`), `rustc -vV`, and `L6_`, `RUST`, and loader environment variables. It is
intentionally conservative: even an unrelated checkout edit invalidates the
namespace. Per-entry keys continue to identify firmware, resolved state,
patch options and card contents. Card keys use copied-file content hashes,
not run-specific source paths. Entries are verified before reuse; processes
serialize publication with per-entry file locks. Cases still receive their own
state and card copies.

Native patch entries additionally identify the selected compiler, LLVM tools,
procedural macro, compiler libraries, rust-src, generated target libraries and
resolved dynamic dependencies. The first run hashes their contents; a locked
snapshot allows later runs to reuse the digest after checking file identities,
change times, search directories and the loader environment. Tool replacement
or changed inputs invalidate the snapshot. Unsupported loader overrides fail
closed. This uses filesystem metadata to validate the snapshot, and assumes
the cache and filesystem metadata have not been deliberately tampered with.

In a release-build, two-case fixtures-only sample, uncached preparation took
2.36 s, the first persistent run took 17.94 s, and the repeated warm run took
0.40 s. The initial run paid for hashing the toolchain and macOS shared cache;
the warm run built no entries. Firmware, state, card, audio, driver and QEMU
identities matched across all three runs. These are single samples on a shared
machine, not full-suite timings. Reports are `/private/tmp/l6-snapshot2-uncached`,
`/private/tmp/l6-snapshot2-cold`, and `/private/tmp/l6-snapshot2-warm`.
On Linux, a native toolchain identity probe reused the same digest on a second
run (5.76 s cold, 46 ms warm). Changing the bytes of a temporary copy of
`llvm-nm` at the same path caused the next probe to produce a different digest.

See [the cache design](persistent-fixture-cache-design.md) for checkout isolation,
leases, eviction and remaining validation. This is a prototype, not yet a
production-safe cache policy. Cache eviction, disk budgets, dynamic loader
resolution on additional environments, and behavior on filesystems without APFS clones also need design and testing.
The complete checkout fingerprint also costs startup time and over-invalidates.
The usual run assumption that inputs are not edited while a run is active
still applies.

Before native toolchain identity was added, a four-case, four-worker,
fixtures-only sample on 2026-11-24 used identical
firmware, state, card, audio, driver and externally supplied QEMU identities.
With an already-warm compiler sysroot, cold case preparation took 2.10 s wall
and the immediately repeated warm case preparation took 0.25 s. All four
fixture identities matched, and the warm run built no entries. The SD effect
replacement case's own fixture time fell from 2.03 s to 0.18 s. These are
single samples on a shared machine, not a full-suite speedup; the runs did
not boot firmware. Reports: `/private/tmp/l6-persistent-cold-v2/report.json`
and `/private/tmp/l6-persistent-warm-v2/report.json`.

A subsequent four-case run did boot the warm fixtures: three passed and the
SD effect case failed at a firmware return breakpoint. A focused retry passed
with the same identities; report both as a timing-sensitive failure and retry,
not a clean first run. Reports: `/private/tmp/l6-persistent-executed/report.json`
and `/private/tmp/l6-persistent-effect-retry/report.json`.

Two simultaneous fixtures-only processes using an empty cache both passed
with identical identities and shared the patch, state and card builds
(`/private/tmp/l6-persistent-race-v2-a/report.json` and
`/private/tmp/l6-persistent-race-v2-b/report.json`). Unit checks also exercise
corruption recovery and cache namespace changes after edits to NOR and patch
sources. QEMU SHA-256 for these trials was
`d33f3853e30115234b1deb9c4b727dfed13a234fcdece2fa18ffe05e7d692a62`;
the external QEMU source was not attested.

## All-catalog validation

On 2026-11-24, `--suite stack --case '*' --backend both --model all --app …`
selected all 372 catalog case/model/backend combinations, including generated
patch combinations, named suites, unscheduled cases and the packaged GUI. It
used the run-local cache, not the experimental persistent cache. The report
(`/private/tmp/l6-cache-all-cases/report.json`) records 326 PASS and 46 FAIL;
there were no BLOCKED cases. The checkout was clean at commit
`b7dd8589cd018022afd825051d1d7b536ec1bf63`. Driver SHA-256 was
`2bcaf760b4437734d42168652594a65de33f7723fe4d4d953552959a07d89cae`;
QEMU was the externally pinned, source-unattested executable identified above.

Forty-four generated L6max combinations containing menu-loader, dump-rom and
midi-sync failed waiting for the menu-triggered state message: each received
zero or one of 94 expected Control Change replies. The unscheduled
`l6-level-meter-pwm` failed its increasing PWM-duty assertion, and the
unscheduled `l6-usb-startup` timed out with its guest clock at 0 ms. These are
failures, not passes or flakes. A focused MIDI case failed again on this branch
(`/private/tmp/l6-cache-midi-focused-retry/report.json`). The same case and
both unscheduled cases failed on the parent `73697362`, with matching
firmware, state, card, audio and QEMU identities; the runner necessarily
changed. Parent reports are `/private/tmp/l6-midi-parent-comparison-fixed/report.json`
and `/private/tmp/l6-unscheduled-parent-comparison/report.json`. One
representative parent case does not establish the cause of every generated
failure. An earlier attempt to run the parent MIDI case stopped during fixture
preparation because the staged driver could not find the macro artifact; it
is recorded separately in `/private/tmp/l6-midi-parent-comparison/report.json`.

`cargo test --offline --locked --workspace` passed on the cache branch. The
firmware-dependent `l6test` fixture unit test remained ignored without its
explicit environment variables.

## Qualification after native toolchain identity

On 2026-11-24 the complete 372-case catalog ran with four workers, both models,
both backends, and the packaged app, at commit
`d810f5a11e6c0badd7a11326fa15e98f047b3f09`. It used the run-local cache,
not the experimental persistent cache. The report
`/private/tmp/l6-toolchain-final-all/report.json` recorded 369 PASS and three
FAIL, with no BLOCKED cases. `sd-knob-midi` and `sd-knob-dialog-dynamic` on
L6max failed the combined dialog first-use assertion; `sd-warm-reset` on L6
observed a loaded registry when it expected no loaded module. Both dialog
cases had passed the separately run 23-case SD matrix with matching identities.

All three passed a focused rerun (`/private/tmp/l6-toolchain-final-rerun/report.json`)
with the same firmware, card, state, audio, runner and QEMU identities. Treat
them as timing flakes, not a clean first run. The failed warm-reset trace shows
the mixer-readiness step completing immediately after the QEMU reset request;
it may have observed the previous boot's state. The dialog failure combines
several checks and did not identify the failing subcondition. Both need
investigation before claiming their causes are fixed. The QEMU executable was
externally supplied and its source was not attested.
