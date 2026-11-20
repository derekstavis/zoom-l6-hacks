# Testing workflow

Run commands from the tooling repository root. Start with [current status](current-status.md).
This guide covers executable checks for the emulator and its tools.

## One entry point

```sh
cargo l6test list
cp config.example.toml config.toml
# Edit config.toml: set the paths to your stock L6 and L6max update packages.
cargo l6test check --config config.toml
cargo l6test run --suite quick --backend both
```

`cargo l6test` is an alias for `cargo run --offline --locked -p l6test --`,
defined in `.cargo/config.toml`; the longer form works the same.

`check` validates the configuration and checks shared memory without launching
firmware. `run` builds selected binaries once, including QEMU unless `--no-build` is
given, copies those executables into the run so their hashes identify what ran, starts
each case with fresh state and card, and supervises every process group. Cases read
their scenarios from the repository in place; only generated scenarios are written,
into the run's `generated/` directory. Run suites from a worktree that does not change
during the run. A supplied output directory must not exist. Paths in the local file
are relative to that file.

Each scenario declares what it runs from, so the local file only supplies firmware:

| Scenario key | Purpose |
| --- | --- |
| `state` | Boot from [`tests/emulator/fixtures/states`](../../tests/emulator/fixtures/states)`/<model>-<state>.toml`, an [`l6nor` state spec](tools/l6nor.md#state-specs). By default `setup-complete`; `"none"` starts from first boot. |
| `card` | Build [`tests/emulator/fixtures/cards`](../../tests/emulator/fixtures/cards)`/<card>.toml` for the case's model each run; `empty` by default, and `"none"` attaches no card. |
| `patches` | Apply named guarded patches to the model's package, using the same implementation as the browser patcher. |
| `installed_patches` | Have the engine install named patches on a device seeded with the stock firmware, as `--patch` does. |
| `installed_sd_patches` | Have the engine install the Patch loader and write the named patches' hosted modules to the case's card, as `--sd-patch` does. |
| `update_patches` | Build the case's update package from the model's package, per model, for example `{ l6 = ["midi-sync"] }`. |
| `gdb` | Enable the case's debugger socket. |
| `audio_device` | QEMU audio-device syntax, with `{logs}` as the case's log directory. |
| `audio_input` | Audio assigned to input channels; see [audio input](../emulator/audio.md#input). |

A state spec seeds MAIN NOR from the case's firmware and commits the declared settings
with the firmware's bank protocol. It also writes the declared panel RTC calendar and,
for L6max, a pre-programmed panel. It boots without the startup prompts, so scenario
steps marked `setup_input` are skipped. No extra guest runs, and the spec hash and the
applied report are recorded in `inputs.json`.

Use saved pad assignments from a card spec unless the Editor or menu is what the case
tests. Use an empty card for cases that compare the home screen across reinsertion: a
card holding a recorder project shows a new project named from the guest clock.

The driver bypasses GUI demo-media and installer defaults. Headless and GUI runs receive
the same explicit firmware, card, state and audio backend. Inputs are never
modified. The factory-protocol scenario creates its own disposable jig card and state
under the case artifact directory, and their hashes are recorded in `inputs.json`.

## Local configuration and case files

`config.toml` is private machine configuration, shared with the emulator; the example is
[`config.example.toml`](../../config.example.toml). The driver reads only its `[firmware]`
table, with the stock update package of each model. Cases for a model without one are
BLOCKED. Input audio, the SD card and device state come from each case's scenario.

Options choose what a run tests, without editing files:

| Option | Purpose |
| --- | --- |
| `--config FILE` | Use another configuration instead of `config.toml` in the working directory. |
| `--qemu BINARY` | Test another QEMU. Without it, the driver uses the repository's, building it unless `--no-build` is given. The report records the hash of the binary used, and a run fails if it changes. |
| `--app PATH` | The macOS `.app` or Linux AppDir for the packaged application cases. Without it, those cases are BLOCKED. |

Card spec details:

- A card spec is a TOML file declaring a card's contents, like the ones in
  [`tests/emulator/fixtures/cards`](../../tests/emulator/fixtures/cards). It names `version = 1`,
  with optional `directory` (a list of paths). A spec without `model` serves both
  models; its `[l6]` and `[l6max]` tables replace top-level fields for one model,
  so `[[l6.sound_pad]]` gives the L6 its own pads. A spec naming `model` serves only
  that model.
  `system_file` defaults to `true`; `false` leaves the file out (see
  [card layout](../emulator/usb.md#card-layout)). `[[sound_pad]]` blocks take `pad` (1–4), `file` (a path on the card),
  optional `mode` (`one-shot`, `loop`, or `hold`; default `one-shot`), and optional
  `level` (0–59; default 49).
  Each `[[file]]` has a `path` from the card root, with parent directories created as
  needed, and one source: `tone` (`hz`, `seconds`, `amplitude`), `loop-tone` (`hz`, `seconds`, `amplitude`; no fade, so it loops cleanly when each loop holds whole cycles), `sweep` (`low_hz`,
  `high_hz`, `seconds`, `amplitude`), `levels` (`hz` and `steps`, each with `seconds`,
  `left`, and `right`), `copy` (a host path from the repository root), or `text`. Generated
  audio is 48 kHz stereo 32-bit float. On L6max, each `[[project]]` (`name`, `hz`,
  `seconds`, `amplitude`, and optional `level`, the mixer level of every channel,
  default 0, which plays silently) writes a recorder project under `RECORDER`, with its settings
  file, eight tracks, and a master.

  ```toml
  version = 1
  model = "l6max"

  [[file]]
  path = "SOUND_PAD/PAD4/SWEEP.WAV"
  sweep = { low_hz = 100.0, high_hz = 8000.0, seconds = 6.0, amplitude = 0.7 }

  [[sound_pad]]
  pad = 4
  file = "SOUND_PAD/PAD4/SWEEP.WAV"
  level = 30
  ```

### Where tests live

| Path | Holds |
| --- | --- |
| `tests/emulator/cases/<area>/` | Device behavior cases, by area: `boot`, `panel`, `audio`, `midi`, `usb`, `storage`, `app`, and `patching` for cases that combine patches or exercise patch infrastructure. |
| `patches/<name>/tests/emulator/cases/` | A patch's own cases; the Patch loader's cases live in `patches/patch-loader/`. |
| `tests/emulator/fragments/`, `patches/<name>/tests/emulator/fragments/` | Step fragments shared by several cases, next to the cases that include them. |
| `tests/emulator/references/`, `patches/<name>/tests/emulator/references/` | Reviewed 128×64 display images, next to the cases that compare against them. |
| `tests/emulator/fixtures/states/`, `tests/emulator/fixtures/cards/` | State and card specs, shared by every case. |

One tree holds both models: a case's scenario names its models, so per-model
folders would split cases that run on both.

### Case files

Every TOML file under a `cases/` directory is a case, and its file name is the
case id. A case file is a scenario (see [the format](scenario-format.md)) with
a `[case]` table saying how the driver runs it:

```toml
version = 1
name = "audio-path"
models = ["l6", "l6max"]
state = "setup-complete"
card = "empty"

[case]
suites = ["devices", "regression", "full"]
volatile = true
no_sd = true
environment = { L6_AUDIO_TRACE = "1" }

[[step]]
action = "audio-callbacks"
```

A file with only a `[case]` table either runs another case's scenario
differently (`scenario`, such as `ui-input-l6-gui` running `ui-input-l6` on the
GUI backend) or, naming no scenario, drives the packaged application. Both
need `models`; a case with its own scenario takes them from the scenario.

| `[case]` field | Default | Purpose |
| --- | --- | --- |
| `suites` | `[]` | Suite names that include this case; an empty array means select it only with `--case`. |
| `scenario` | the file itself | Another case's scenario file, from the repository root, for a file without its own. |
| `models` | the scenario's | Models of a case without its own scenario (`l6`, `l6max`). |
| `environment` | none | Environment for the case process. `{logs}`, `{package}`, `{l6_package}` and `{app_qemu_sha256}` expand. |
| `gui` | `false` | Run on the GUI backend only. |
| `volatile` | `false` | Discard persistent-state templates for this case and run without persistent state, unless the scenario declares a `state`, which is rebuilt for the case. |
| `no_sd` | `false` | Do not attach the scenario's SD card. |
| `expected_failure` | none | Array of tables with `model`, `step`, `contains` and `reason`. Match a known failure only at the exact scenario step and when its error contains the specified text. A matching failure is XFAIL; an unexpected pass is XPASS. |
| `blocked` | none | Human-readable reason to report this case as BLOCKED. |
| `wall_timeout_s` | `900` | Per-case host wall limit. |

A case without a scenario runs the packaged application and is BLOCKED
without `--app`. Case ids must be unique across every `cases/` directory.

The local configuration, `[case]` tables, and scenario headers reject unknown
fields. Scenario action blocks are tagged by the required `action` value and
their action-specific fields are strict; use the documented common step fields
and action schema. Every `scenario`, `include` and reference image path is written from the
repository root.
`l6test`'s focused [command reference](tools/l6test.md) summarizes selection
flags and links to the full scenario schema.

Use the required execution permissions before starting suites: QEMU needs
POSIX shared memory and inherited sockets. Git operations on a worktree need
access to its parent repository's metadata. Repository instructions cannot
grant sandbox permissions; the calling environment must provide them.

## Suites and isolation

[`tests/emulator/suites.toml`](../../tests/emulator/suites.toml) declares every
suite with a description of what it covers; this page does not repeat them. A
case naming an undeclared suite fails to load. A suite's `includes` lists other
suites whose cases it also runs; `stack` runs `full`, `sd-matrix` and `patches`
for the top of a stack. `l6test list` shows each case's suites. The generated
`patches` suites are described under [Patch combinations](#patch-combinations).

Select single cases with `--case`, for example `--case boot` for the shared
sweep/readiness gate only, `--case encoder` or `--case mute`.

Validation is tiered. While iterating, and for each PR in a stack, run the
smallest set of cases that covers the change (`--case` with ids or globs,
`--rerun`, or a focused suite such as `patches-pairwise`). Run `--suite stack`
once at the top of a stack (about two hours), and fix what it finds in the PR
that owns the cause.

Cases run concurrently, up to `--jobs N` at once (by default a quarter of
the host's cores, between 1 and 4). Scenario waits and the headless boot
waits count guest time and fail only when the guest stops advancing, so
concurrency lengthens wall time. Many device operations that run the guest
under GDB still have fixed wall-clock limits; keep N well below the core count,
since an oversubscribed host can exceed them. GUI cases run alone. Native MIDI
endpoints are namespaced per guest; patch builds call the `l6patch` library API
and write into each case's artifact directory. Debugger sockets and other
scenario-generated files are isolated by case. Shared firmware-cache
publication uses unique staging directories and atomic rename. `--jobs 1` runs
cases sequentially.

Add `--model l6` or `--model l6max` to focus a run. `--backend both` runs
identical shared scenarios through headless and GPUI adapters on separate
copies of the same state and card. Legacy cases execute their declared backend only;
a low-level diagnostic pass does not qualify GUI parity.

`--no-build` explicitly reuses installed debug executables. Their hashes are
still recorded. It is useful for repeated diagnostic runs and binary comparisons,
but does not attest that an executable was built from the current sources.
The driver pins QEMU by binary hash and records it for every run. A source checkout
hash alone never identifies that binary. An external binary is never rebuilt or
replaced.

Boot readiness requires the model's six or eight rings to reach
all-lit and settle for one guest second; L6 also needs a lit editing-mode
lamp, and L6max needs display pixels. On template and fresh state, L6max also
waits for a 12 guest-second floor: its first-boot Date/Time editor appears at
about 9 s, after a card popup, with no lamp change. Spec-built state boots
straight to its home screen, so it skips the floor; L6max then also requires one
guest second without a new display frame. Both sweeps end within 5 guest
seconds. Shared control scenarios then wait until every visible ring holds a
restored value (on L6, also a lit mode lamp), select LEVEL unless it is already
lit, verify its lamp, and wait for stable panel outputs before capturing
per-control baselines. A static OLED
frame is valid and must not be treated as a stopped guest clock.

Steps marked `setup_input` confirm the L6max startup SD prompt or the startup prompts,
for cases that boot without a state spec. A case with a `state` boots straight to the
recorder, so the driver skips them and records `execution_spec_state` in `inputs.json`.

## Results and evidence

The printed artifact directory contains:

- `report.json` and, for completed suites, `summary.md`: matrix and outcome.
  See [Report](#report) below.
- `manifest.json`: source revision, dirty diff/untracked hashes, scenario,
  generated-scenario and board/patch hashes, driver binary and pinned QEMU
  identity.
- `generated/`: the generated scenarios this run wrote, such as the `patches`
  suite's.
- `cache/`: fixtures built once for the run and copied into each case. See
  [Fixture cache](#fixture-cache).
- Per-case `inputs.json` and `invocation.json`: selected fixture content hashes,
  execution options, executable hash, exact arguments and declared environment.
- `scenario.json`: shared assertions, failed step, last state and bounded events.
- `stdout.log`, `stderr.log`, QEMU trace, LCD PNG and ASCII preview; named checkpoints also retain PNG/ASCII.
  SD tracing selects `sdhci.trace`; other runs use `engine.trace`. Restart logs
  live under the case's `restart/` directory.

Mapping assertions wait for quiet visible rings and UART reports before advancing
(one guest second by default, three in the full-ring fragment); this prevents intermediate values being mistaken for
completed actions. L6 compares only its six physical rings.

Default artifact directories are short paths under `/tmp` so Unix socket paths
fit platform limits. Retain useful runs outside temporary storage before cleanup.
`--output /short/new/path` selects a retained location. Completed cases are written
incrementally. Exit 0 means PASS, explicitly matched XFAIL, or FLAKY
without `--strict`; 1 means FAIL or XPASS (or FLAKY with `--strict`); 2 means
BLOCKED. Read the outcome rather than inferring a clean pass from exit 0. Keep a
baseline with XFAIL labeled as incomplete.

Expected failures are optional catalog entries on a case, matching an exact
case/model/step and error substring, with a reason. A different failure remains
FAIL; an unexpected pass becomes XPASS and requires review. No current device
failure is hidden by a checked-in expected-failure rule.

The process supervisor bounds wall time, stops descendant processes on failure,
interruption or completion, and leaves the system mouse untouched. Scenario
waits and button timing use guest time, with independent wall-clock ceilings.

### Report

`report.json` (schema 2) has the run `outcome`, `counts` per outcome, fixture
`cache` statistics and one result per case, model and backend. Each result
keeps the schema 1 fields (`case`, `model`, `backend`, `outcome`, `detail`,
`artifacts`) and adds:

| Field | Meaning |
| --- | --- |
| `phase` | Where the case stopped: `preflight`, `fixture` or `execute` |
| `duration_s`, `timings` | Wall seconds in total, building fixtures and executing |
| `identity` | Hashes of what the case ran on: QEMU, the runner executable, the firmware package and unpacked firmware, the card, the state and audio input |
| `attempts` | Every attempt, when the case was retried |

Outcomes:

| Outcome | Meaning |
| --- | --- |
| PASS | Ran and passed |
| FAIL | Its fixture failed to build, the image our patch tools built is not the one they reported or a step needs the stock image the patches replaced, or it ran and failed |
| XFAIL, XPASS | Failed as expected; passed despite an expected failure |
| BLOCKED | Did not run, or its result is not valid: missing configuration or `--app`, unsupported backend, the configured package is for another model or not the supported stock image, the run was interrupted, or QEMU or the executable changed during the case |
| FLAKY | Failed, then passed on a retry. Never counted as a pass |

A broken patch build, or a patched image that differs from its build report,
is a FAIL in the `fixture` phase, not BLOCKED. Before a case runs, the driver
also checks that its fixture holds every file the scenario reads from the case
directory (`smoke_core::fixture_layout`); a missing one is a fixture FAIL.

### Selecting and rerunning cases

- `--case` takes comma-separated ids or globs: `--case 'patches-l6max_*'`.
- `--shard I/N` runs the I-th of N parts. A case's shard depends only on its
  case, model and backend, so N machines can split a suite and each run
  `--jobs 1`.
- `--rerun RUN` reruns the results of an earlier run (its directory or
  `report.json`) that were FAIL, XPASS, BLOCKED or FLAKY. `--only PASS,FAIL`
  names other outcomes.
- `--retries N` reruns a case that failed while executing up to N more times.
  Each attempt keeps its artifacts (`<case>.attempt-2/`, ...). A failure followed
  by a pass is FLAKY, listed separately in `summary.md`. `--strict` makes a
  FLAKY case fail the run. BLOCKED cases and fixture failures are not retried.
  A failure followed by anything but a pass stays FAIL, so a later attempt
  never hides an earlier failure.
- A `--case` pattern that matches no case is an error.

### Reference images

`expect-display-image` waits, within the step's timeout, for the display to
match its reference; on a timeout the last frame is saved next to the case
logs as `<reference>.actual.png`. `--update-references` instead records the
display into the reference once it has held still for half a second, and
lists every rewritten path in `summary.md`, so the new images show in the
diff for review before they are committed. When two cases in the run record
different screens for the same reference, the later one fails rather than
overwriting it. Rerun without the flag to confirm the recorded images pass.

### Fixture cache

Within a run, each distinct `l6patch` build, browser-patched package, unpacked
package, state and card image is built once under `cache/` and copied into
every case that uses it; stock firmware is booted read-only from the shared
unpack. When the run directory's volume clones files (APFS), copies cost no
data; otherwise card images are built per case. Keys name every input of a
build except repository sources, which a run assumes do not change while it
runs. The cache lives only as long as its run. `summary.md` lists hits and
builds per kind.

`--fixtures-only` builds every selected case's fixture and records its
identity without booting a guest; results are PASS in the `fixture` phase. `--no-cache` builds every fixture per case. Comparing the
identities of a cached and an uncached `--fixtures-only` run shows the cache
does not change what cases boot.

## Scenario format and extension

See [the version-1 format](scenario-format.md). New behavior belongs in a shared
scenario or a typed operation in `smoke-core`, not another one-off binary.
Each case file's `[case]` table gives its suites, environment and time limit. WASM patch parity runs as a typed host-side driver operation.
The focused panel cases compose reusable fragments rather
than copying setup or asserting results separately.

Packaged application tests use the same supervisor and report format. Pass the packaged
application with `--app`; the catalog gives each test an isolated `L6_APP_DATA`, and the
driver checks the QEMU inside it. Run them on macOS with:

```sh
cargo run --offline --locked -p l6test -- run --case gui-bundle --model l6max --backend gui --app "/path/to/L6max Emulator.app"
cargo run --offline --locked -p l6test -- run --case gui-menu --model l6max --backend gui --app "/path/to/L6max Emulator.app"
```

`gui-bundle` imports L6 then L6max firmware and checks isolated persisted state;
`gui-menu` checks both native picker actions while an L6max guest is running.

## Structured runtime logs

The driver, shared runner and GPUI scenario adapter
use `tracing`. Smoke execution writes one JSON event per line to stderr by
default. `list` and help retain ordinary stdout. Case child stderr is captured
in `stderr.log`; QEMU's device trace is a separate artifact.

```sh
RUST_LOG=warn,l6test=info,smoke_core=debug cargo run --offline --locked -p l6test -- run --suite quick
L6_LOG_FORMAT=text cargo run --offline --locked -p l6test -- run --case boot
```

`L6_LOG_FORMAT=json|text` chooses JSON or compact text. `RUST_LOG` accepts normal
`tracing_subscriber::EnvFilter` directives. Invalid settings fail before boot.
The driver records and forwards the resolved logging settings. JSON events carry
levels, targets, timestamps, case/model/backend span context and numeric step/
guest-time fields. Debug events contain detailed runner observations; the default
filter limits output to progress, step boundaries and results. Report and manifest
JSON files remain the stable test-result interface.

The shared QMP client emits command names, elapsed milliseconds and outcomes at
DEBUG level, failed commands at WARN, and asynchronous event names at DEBUG. It
does not log command arguments or memory/register contents. Add
`l6max_host::qmp=debug` to `RUST_LOG` when investigating QEMU management traffic.

The regular GUI defaults to compact text and honors the same settings. Remaining
legacy diagnostic binaries may still print their own historical text output.

## Unit and build checks

```sh
cargo fmt --all --check
cargo check --offline --locked --workspace --all-targets
cargo test --offline --locked -p smoke-core -p l6test -p l6max-host -p l6max-gui
```

These firmware-free checks verify parsing, sequencing, deadlines, snapshots,
transport, gesture handling and process cleanup. They do not establish guest
firmware behavior. See [CI](ci.md) for hosted checks.

The panel register check uses the built QEMU test accelerator and a synthetic
image; it needs neither firmware nor an SD card:

```sh
python3 emulator/qemu/tests/panel-qtest.py emulator/qemu/build-source/build/qemu-system-arm
```

It exercises both panel profiles, flash boundaries and existing 64 KiB saved
flash, plus GPIO and timer registers, pending interrupts and timer overflows.
It does not establish behavior on physical hardware. The smaller C tests in
`emulator/qemu/tests/panel-gpio.c` and `panel-timer.c` cover shared register
logic independently of QEMU.

### Case matrix

`cargo l6test list --format json` prints every case, model and backend the
catalog can run, sorted, with suite membership and a `definition` digest of
what the case runs on that model: the scenario's steps for the model after
includes, its reference images by content, its state and card specs as data,
and the behavioral catalog fields. Moving files, rewriting includes or merging
per-model scenarios into one keeps the digest; changing what runs changes it.
`tools/l6test/snapshots/cases.json` holds the reviewed copy, and
`cargo test -p l6test` fails when the catalog no longer matches it. A change
that adds, removes or moves cases, or changes a generated scenario, must update
it deliberately:

```sh
L6TEST_UPDATE_SNAPSHOT=1 cargo test --offline --locked -p l6test case_matrix
```

Review the diff of `cases.json` with the change.

## Input, display, and LEDs

Use the shared `panel`, `l6-input-sweep`, and `l6max-input-smoke` scenarios.

## Integration setup

Use the local configuration above and each case file's `[case]` table for prerequisites.

## Patch combinations

The `patches` suite is generated by `l6test` from the package declarations in
`patches/*/package.toml` and each patch's checks; it has no case files. A run
writes each case's scenario into the run's `generated/`, and `l6test list`
shows every case.

- **Combinations:** each patch is absent, installed in firmware, or loaded
  from the SD card, wherever its package declares that model. Each stock
  effect slot (Room, Spring) keeps its effect or runs a declared replacement.
- **Valid:** the firmware set and the SD set each include their own
  dependencies, and none is in both. Effect replacements therefore need
  knob-dialog on the card.
- **Builds:** firmware-only cases use the browser patcher when it supports
  every patch, and `l6patch` otherwise. Cases with SD modules build the Patch
  loader with the firmware patches, and the SD module with `l6patch`.
- **Checks:** one boot per case, then a short check per installed patch. Each
  patch declares its check in `patches/<name>/tests/emulator/checks.toml`:
  `check`, a fragment run once the mixer is ready; `system_items`, the System
  menu items it adds; and `menu_action`, a fragment run after selecting one of
  them. Cases run the checks in the order `tests/emulator/patches.toml` lists,
  and include the fragments by path, so a generated scenario is ordinary
  composed steps:

  | Patch | Check |
  | --- | --- |
  | Any SD module | `expect-sd-patch` |
  | knob-dialog | `knob-dialog-popup` |
  | Effect replacements | `expect-heap`, then for each slot `effect-replacement-state` with the assigned effect ID and nonzero output, or no replacement |
  | midi-sync | `direct-usb-midi-round-trip`, then `host-midi-sync` |
  | din-mixer-control | `l6max-din-mixer-control` |
  | menu-loader | `expect-menu` for System and System > Patches, then System > Patches as drawn against `patches/menu-loader/tests/emulator/references/patches-suite/` (skipped with knob-dialog; see below) |
  | midi-sync with menu-loader | **Send MIDI State**, then `host-midi-sync` with `request = false` |
  | dump-rom | **Dump ROM**, then every file's size and the NOR file's contents |

  L6max cases boot from the `patch-matrix` state, which turns on Mixer
  Control via MIDI and selects Room with channel 1 sent to it.
- **Delivery:** each patch the browser patcher supports is also installed by
  the engine on stock firmware (`installed_patches`, with the same checks)
  and through a firmware update (`update_patches`). After an update,
  `firmware-update` checks the installed image and runs knob-dialog's full
  check or the MIDI state request for the patches the update installs. Each
  hosted SD module is loaded as the website delivers it
  (`installed_sd_patches`).

A new placeable patch fails generation until it has a
`tests/emulator/checks.toml` and a place in `tests/emulator/patches.toml`.
The effect-slot checks and menu-loader's menu walk depend on the combination,
so the generator writes those steps itself. A Patches-screen reference is
named after the rows it shows and, when the list scrolls, its length
(`…__of-5.png`), so each name maps to one screen. `l6test list` shows the case count,
which grows with each placeable patch. Adding, removing or changing a
generated case changes the reviewed case matrix in
`tools/l6test/snapshots/cases.json`; see [Case matrix](#case-matrix). Run one case at a time: the heaviest
takes under two minutes, and the suite takes hours:

```sh
cargo run --offline --locked -p l6test -- run --config config.toml \
  --suite patches --model all --backend headless --jobs 1
```

## Firmware patch

Patch, factory, and update cases need explicit local configuration and matching
firmware. Use `knob-dialog`, `patched-update`,
`firmware-update`, and `wasm-patch` as appropriate. `l6-patch-heap` and
`l6max-patch-heap` check heap headroom with every patch for the model applied.

The committed browser templates and the SD modules the web page and emulator
serve are generated, and their manifests record a fingerprint of the sources
that produced them: everything under `patches/` except `tests/`,
`rust-toolchain.toml`, and the crates `l6patch` is built from. A change to any
of these, including a new `patches/api` declaration, makes them stale, and
`cargo l6patch prebuilt --check` and `cargo test -p l6patch` fail until they
are regenerated from the stock packages in `config.toml`:

```sh
cargo l6patch prebuilt
```

Review the generated changes under `tools/firmware-patch-wasm/templates/` and
`web/public/loadable-patches/` and commit them with the source change. Do not edit the generated `.bin` or `.json`
files by hand. Run the browser/native parity case for both models and the SD
loading matrix:

```sh
cargo run --offline --locked -p l6test -- run --config config.toml \
  --case wasm-patch --model all --backend headless
cargo run --offline --locked -p l6test -- run --config config.toml \
  --suite sd-matrix --model all --backend headless
```

Then run the full suite at the final stack tip. On macOS, build a local app
bundle and pass it with `--app` so packaged GUI cases execute instead of being
reported BLOCKED:

```sh
cargo run --offline --locked -p macos-package -- \
  emulator/qemu/build-source/build/qemu-system-arm \
  --debug --output /tmp/l6-app-package --version 0.1.0
cargo run --offline --locked -p l6test -- run --config config.toml \
  --suite full --model all --backend both \
  --app "/tmp/l6-app-package/L6max Emulator.app"
```

Configure the supported stock 1.10 packages. Another revision is BLOCKED
coverage, not a passing test. Read `report.json` and distinguish PASS,
FAIL and BLOCKED totals. Retry timing-sensitive failures with `--retries` or
`--rerun`: a failure that passes on retry is FLAKY, and both attempts are kept.

## Power, storage, and USB

Use the `lifecycle` or `devices` suite and the relevant individual cases.

## Editor

Use the `editor-pad-assignments` case for the emulator's editor protocol path.
