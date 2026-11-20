# Working in the tooling repository

This repository owns the simulator, package/patch tools and executable tests.
Keep changes on a feature branch until the user approves a merge.

Read `docs/development/current-status.md`, then `docs/development/testing.md`
for where tests live, suites, and how to add a case or a patch's checks. Use the
suite driver: `cargo l6test list`, `cargo l6test run`. Paths in scenarios,
includes and specs are written from the repository root.
For focused runtime observations outside qualification, use
`docs/development/diagnostics.md` and the `l6dbg` tool documentation.
Local firmware package and audio paths belong in ignored `config.toml`; never commit
firmware, SD cards, saved flash, generated analysis artifacts or runtime logs.

Validation is tiered. While iterating, and for each PR in a stack, run the
smallest set of cases that covers the change (`--case`, globs, `--rerun`). Run
`--suite stack` (the full suite, the SD matrix and every patch combination;
about two hours) once at the top of a stack, and fix what it finds in the PR
that owns the cause. Run one suite at a time; the default `--jobs` is intended.

When changing anything under `patches/` (including `patches/api`), the patch
tools, `Cargo.toml` or `Cargo.lock`, regenerate the committed browser templates
and web SD modules with both `firmware-patch-wasm-prebuild` and
`tools/build-web-loadable-patches.sh`, using supported unpacked L6max and L6
firmware; `cargo test -p firmware-patch-wasm-prebuild` fails while either is
stale. Do not hand-edit generated files. Run `wasm-patch` for both models and the
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
literal. Scenario files and `l6dbg` still contain literal addresses; move one
to a declaration when you touch it.

A source commit or QEMU version string does not identify a QEMU executable.
The driver pins external QEMU binaries and explicitly marks their source as
unattested.

Read the existing working-tree status before editing. Preserve unrelated changes.
Never merge or squash history implicitly.

Keep this file to rules and pointers; details belong in `docs/`. When a change
makes a line here stale, update it in the same PR.
