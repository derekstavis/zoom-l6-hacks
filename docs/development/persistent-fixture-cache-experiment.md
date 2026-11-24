# Persistent fixture cache experiment

This branch adds an opt-in `--persistent-cache DIRECTORY` experiment to
`l6test`. Without that option, the existing per-run cache is unchanged.
Use a local, ignored directory outside the checkout; its contents include
copyrighted firmware and generated SD and NOR images. Remove it when no
longer needed. Do not commit or share its contents.

The namespace hashes the running driver, tracked and non-ignored untracked
checkout files (including patch sources, `l6nor`, generated templates and
`Cargo.lock`), `rustc -vV`, and `L6_`/`RUST` environment variables. It is
intentionally conservative: even an unrelated checkout edit invalidates the
namespace. Per-entry keys continue to identify firmware, resolved state,
patch options and card contents. Card keys use copied-file content hashes,
not run-specific source paths. Entries are verified before reuse; processes
serialize publication with per-entry file locks. Cases still receive their own
state and card copies.

This is a prototype, not yet a production-safe cache policy. In particular,
`rustc -vV` and environment paths do not identify every byte of the external
compiler, linker, LLVM utilities or rust-src; cache eviction, disk budgets,
and behavior on filesystems without APFS clones also need design and testing.
The complete checkout fingerprint also costs startup time and over-invalidates.
The usual run assumption that inputs are not edited while a run is active
still applies.

On 2026-11-24 a four-case, four-worker, fixtures-only sample used identical
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
`d33f3853e30115234b1deb9c4b727dfed13a234fcde2fa18ffe05e7d692a62`;
the external QEMU source was not attested.
