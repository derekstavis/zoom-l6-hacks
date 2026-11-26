# Working in the tooling repository

For a guarded single-read sample of live allocator counters, use
`l6ctl heap-counters` or `OwnedControl::heap_counter_reader()`; `rtos heap`
also traverses and validates the full heap and has different costs and semantics.

This repository owns the simulator, package/patch tools and executable tests.
Keep changes on a feature branch until the user approves a merge.

Read `docs/development/current-status.md`, then `docs/development/testing.md`
for where tests live, suites, and how to add a case or a patch's checks. Use the
suite driver: `cargo l6test list`, `cargo l6test run`. Paths in scenarios,
includes and specs are written from the repository root. Use Tombi's language
server (or `uvx tombi lint`) for fast feedback while editing case and fragment
files; `cargo l6test list` is the authoritative check and needs no firmware.
Write scenario behavior as named, independently reported steps and reuse existing
fragments. Use typed `l6ctl`/host APIs for emulator interactions; keep test
orchestration and assertions in `smoke-core`. Preserve shared deadlines, cleanup,
model scopes, and deliberate observation windows when splitting operations.
See `docs/development/tools/l6test.md#writing-declarative-scenarios`.
For focused runtime observations outside qualification, use
`docs/development/diagnostics.md`: `cargo l6ctl` inspects an already running
emulator started with `--control-socket`, while `l6dbg` starts a disposable
guest for scripted observations. Use `cargo l6ctl rtos` for the running guest's
guarded FreeRTOS snapshots; see `docs/development/tools/l6ctl.md` for the JSON
schema, supported revisions and live-sampling limits. Rust callers can use the
`l6ctl` crate for typed socket requests or borrow an existing host engine;
socket input receipts mean enqueue, not guest acknowledgement.
`cargo l6ctl input status main|panel` exposes aggregate per-channel counters;
they cannot attribute an acknowledgement to a particular caller.
Local firmware package and audio paths belong in ignored `config.toml`; never commit
firmware, SD cards, saved flash, generated analysis artifacts or runtime logs.
Persistent fixture caching is experimental and must not be used for qualification;
see `docs/development/testing.md#fixture-cache` for scope and cleanup.

Validation is tiered. While iterating, and for each PR in a stack, run the
smallest set of cases that covers the change (`--case`, globs, `--rerun`). Run
`--suite stack` (the full suite, the SD matrix and every patch combination;
about two hours) once at the top of a stack, and fix what it finds in the PR
that owns the cause. Run one suite at a time; the default `--jobs` is intended.

When `cargo l6patch prebuilt --check` reports stale prebuilt files (after
changing `patches/`, the patch tool crates or their dependencies), run `cargo
l6patch prebuilt` and commit the result. Do not hand-edit generated files. Run `wasm-patch` for both models and the
`sd-matrix` suite to check browser/native parity and SD loading. See
`docs/development/testing.md#firmware-patch` for commands and interpretation.

Read `report.json`, not the exit code alone: BLOCKED cases did not run, and
FLAKY is a timing flake to report, never a pass. Quote identities from
`manifest.json` and each result's `identity` when claiming a pass.

Before suites, use execution permissions that allow POSIX shared memory and
inherited sockets. Shared Git worktrees need access to their parent Git metadata.
These requirements cannot be granted by this file; request the tool's required
execution permissions directly rather than repeatedly retrying a denied suite.
The user authorizes POSIX shared memory access for this project; this does not
override restrictions imposed by the execution environment.

Both GUI and headless scenarios use `smoke-core`, `PanelFeed` and `PanelData`.
Keep assertions out of GUI adapters. Prefer guest-time readiness/output checks
with bounded wall deadlines. A static OLED is valid. L6 has six rings and no screen;
L6max has eight rings. Do not substitute one model's persistent state for the other.
For UI behavior, compare rendered pixels with reviewed reference images using
`expect-display-image`; memory and state assertions can supplement these checks,
but do not prove that the UI rendered correctly. The 128×64 PNG references live
in `tests/emulator/references/` or a patch's `tests/emulator/references/`.
Record or refresh them with `--update-references`, review every image it lists
in the diff, then rerun without the flag before committing.

Use the reference manual for peripheral behavior. Preserve compatibility
exceptions with explicit model scope and evidence. Scope compatibility
behavior to the corresponding firmware revision. Declare each new firmware
address once, per model and revision, in `patches/api/`, and use it from host
tools and tests through `firmware-api` (`l6::name`, `l6max::name`) instead of a
literal; scenario files name them with `symbol`. `l6dbg` still contains literal
addresses; move one to a declaration when you touch it.

A source commit or QEMU version string does not identify a QEMU executable.
The driver pins external QEMU binaries and explicitly marks their source as
unattested.

Read the existing working-tree status before editing. Preserve unrelated changes.
Never merge or squash history implicitly.

Keep this file to rules and pointers; details belong in `docs/`. When a change
makes a line here stale, update it in the same PR.
