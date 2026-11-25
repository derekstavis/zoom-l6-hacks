# Test driver timing investigation

The driver records per-attempt wall durations for fixture preparation,
executable verification, runner startup and shutdown, and scenario steps.
Nested operation times are included in their parent; they must not be added
again. They measure host wall time, including cache waits and I/O, not guest
time. Parallel case durations overlap.

A local, sequential, two-case diagnostic compared debug `l6test` builds
without PGO. The second build optimized `sha2` 0.10.9 in the dev profile;
the SHA-256 identity and verification checks were unchanged. The QEMU binary
was externally supplied and its source was unattested. Its SHA-256 was
`d33f3853e30115234b1deb9c4b727dfed13a234fcdece2fa18ffe05e7d692a62`.

| Case | Stage | Before | Optimized |
| --- | --- | ---: | ---: |
| L6 patch heap | Preflight | 1.360 s | 0.081 s |
| L6 patch heap | Runner hash | 1.564 s | 0.078 s |
| L6 patch heap | Executable verification | 2.760 s | 0.136 s |
| L6max SD effect replacement | Runner hash | 1.555 s | 0.077 s |
| L6max SD effect replacement | Executable verification | 2.762 s | 0.160 s |

Both cases passed in both runs. For each case, the QEMU, firmware package,
firmware, card, state and audio identities matched; only the runner hash
changed as expected for the rebuilt executable. An independent `shasum`
matched the optimized runner and QEMU hashes recorded in the report. The
runner hashes were `534e9444e298c9dbe46b98549211e28489a5099b3e6b2be10f3c4a5caecab3c3`
before and `9bc04c5bf345bc491c2de3639f5a07fba2fe54870dab5ea5d65871e40db8ea92`
after. Full identities and individual durations are in
`/private/tmp/l6-fixture-stages-diagnostic/report.json` and
`/private/tmp/l6-fixture-stages-sha-optimized/report.json`.

These are single samples on a shared host, not a controlled suite benchmark.
They show the measured hash stages became much shorter; they do not establish
an overall suite speedup. SD patch preparation also varies with compiler and
artifact cache warmth, so compare those stages under controlled cache conditions.

With a separate empty Cargo target directory, the driver built only
`patch-hook-macros`, then compiled the native patch and passed the L6max SD
effect replacement case (`/private/tmp/l6-macro-only-cold/report.json`). A
headless boot fixtures-only check with a different empty target skipped the
Cargo build (`/private/tmp/l6-nonpatch-build-cold/report.json`). The latter
did not execute the emulator scenario.
