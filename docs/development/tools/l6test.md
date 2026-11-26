# `l6test`

`l6test` is the suite driver for repeatable emulator scenarios. It validates
local configuration, prepares isolated guest state, supervises the requested
headless or GUI cases, and writes a structured report with case artifacts.
Use [diagnostic tools](l6dbg.md) for observation without pass/fail assertions.

## Commands

```sh
cargo l6test list
cargo l6test check --config config.toml
cargo l6test run --config config.toml --suite quick
cargo l6test run --config config.toml --case boot --model l6
cargo l6test run --config config.toml --suite quick --backend both
cargo l6test watch /tmp/l6smk-RUN
cargo l6test run --case boot --persistent-cache target/l6test-fixtures
cargo l6test cache clean --persistent-cache target/l6test-fixtures
```

`cargo l6test` is an alias for `cargo run --offline --locked -p l6test --`.

- `list` prints catalog cases, model scope, suites, and cases that need a
  packaged application. `list --format json` prints every case, model and
  backend; see [case matrix](../testing.md#case-matrix).
- `check` validates `config.toml` without starting a guest.
- `watch RUN` shows live progress for a run in another terminal. It replays and tails
  the run and attempt `progress.jsonl` streams and shows startup phases,
  current steps, and failed step names and errors, including after late attachment.
  Ctrl+C closes the view without stopping the suite. Non-terminal output is a
  single plain-text snapshot. Only runs started by a version that writes
  `progress.jsonl` can be watched; a different progress format version is rejected.
- `run` selects a `--suite NAME`, or `--case` with comma-separated ids or
  globs (`--case 'patches-l6max_*'`), optionally filtered by
  `--model l6|l6max|all` and `--backend headless|gui|both`.
- `--rerun RUN` reruns an earlier run's FAIL, XPASS, BLOCKED and FLAKY results;
  `--only OUTCOMES` names others. `--shard I/N` runs one of N stable parts.
- `--retries N` retries execution failures; a failure that later passes is
  FLAKY, and `--strict` makes FLAKY fail the run.
- `--update-references` records the display into any reference image that
  is missing or differs, for review in the diff, instead of failing. See
  [reference images](../testing.md#reference-images).
- `--fixtures-only` builds each case's fixture and identity without booting;
  `--no-cache` builds fixtures per case instead of once per run.
- `--persistent-cache DIRECTORY` opts into experimental reuse across runs in
  the same checkout. `cache clean --persistent-cache DIRECTORY` removes only
  that checkout's entries and refuses while a run is using them. Keep this
  cache local and ignored; see [fixture cache](../testing.md#fixture-cache)
  for its current tool-identity limitations.
- `--output NEW_DIRECTORY` chooses an artifact directory. It must not already
  exist. By default the driver creates a unique directory under `/tmp`.
- `--no-build` uses existing emulator binaries instead of building them.
- `--jobs N` runs up to N cases at once. The default is a quarter of the host's
  cores, between 1 and 4. Each case has its own process group, state, card,
  MIDI namespace, sockets and artifact directory. GUI cases run alone.
  Shareable cases run first. `--jobs 1` restores strictly sequential execution.

The [results reference](../testing.md#results-and-evidence) describes
`report.json`, outcomes and phases.

Copy the [example file](../../../config.example.toml) to
`config.toml` and set your stock L6 and L6max package paths. Firmware, device
state, and generated reports stay
local. See the
[testing workflow](../testing.md).

## GUI screen probe

The exploratory `probe gui` command captures the panel while pressing GPIO
targets. It is for visual/behavioral investigation and does not qualify a test.
The window observation resolves `current_window` for the selected model; the
symbol lookup does not verify the running firmware revision.

```sh
cargo run --offline --locked -p l6test -- probe gui \
  --keys 4,5,6 --volatile --qemu /path/to/qemu-system-arm \
  --firmware-dir /path/to/unpacked-firmware --logs /tmp/gui-capture
```

Use `--help` after `probe gui` for the short usage. Engine options are the same
as the GUI/headless host; `--volatile` avoids persistent device state.

## Capture the L6max screen as a GIF

`capture screen` boots L6max into the recorder screen and records its LCD as a
looping GIF. Add timed taps and encoder turns to demonstrate a screen change:

```sh
cargo run --offline --locked -p l6test -- capture screen \
  --out /tmp/knob-dialog.gif --duration-ms 9000 --fps 5 \
  --tap 35:0:50 \
  --encoder 1:800:-4 --encoder 1:1400:-4 --encoder 1:2000:-4 \
  --encoder 1:2600:-4 --encoder 1:3200:-3 \
  --volatile --qemu /path/to/qemu-system-arm \
  --firmware-dir /path/to/patched-l6max
```

`--out FILE.gif` is required and existing files are never overwritten.
`--duration-ms` defaults to 12000 and accepts 1000..60000; `--fps` defaults to
5 and accepts 1..15. Repeat `--tap TARGET:AT_MS:HOLD_MS` and
`--encoder CHANNEL:AT_MS:STEPS`; event times are relative to the start of
capture. Capture requires L6max firmware and isolated state (`--volatile` or a
disposable `--state-dir`). The GIF contains the emulator LCD framebuffer,
scaled with nearest-neighbor pixels, and does not require ffmpeg.

## Capture the live emulator GUI without a display server

`capture gui` boots an isolated L6max guest, samples its live panel indicators
and LCD, then renders the full emulator panel with GPUI's offscreen renderer.
It works without a window server and retains the sampled panel data next to the
PNG for inspection. Use a state directory built from a state spec when a
specific mixer value or indicator state is needed:

```sh
cargo run --offline --locked -p l6test -- capture gui \
  --out /tmp/l6max-panel.png --state-dir /tmp/prepared-state \
  --wait-guest-ms 45000 --qemu /path/to/qemu-system-arm \
  --firmware-dir /path/to/unpacked-l6max
```

Use `--volatile` instead of `--state-dir` for a fresh guest. The command
refuses to overwrite the PNG or its `.panel.json` data file. The JSON includes
the sampled indicator levels and masks, analog positions and LCD framebuffer.

## TOML files

There are three related TOML inputs:

1. `config.toml` supplies the firmware packages; `--qemu` and `--app` choose an external QEMU or packaged application. See
   [local configuration and case files](../testing.md#local-configuration-and-case-files).
2. Each case file under `tests/emulator/cases/` or a patch's
   `tests/emulator/cases/` is a scenario with a `[case]` table giving its suites,
   environment and timeout; see [case files](../testing.md#case-files).
3. The scenario part describes an ordered sequence of steps.
   The complete list of header, per-step, and
   action blocks is in the [scenario format reference](../scenario-format.md).
   Its `state` header declares the starting device directly; see
   [`l6nor` state specs](l6nor.md#state-specs).

Local configuration, catalog entries, scenario headers, and action-specific
fields are strict. Paths in local configuration are resolved relative to the
TOML file. Scenario and fragment paths are relative to the repository root;
absolute paths and `..` are rejected.

### Writing declarative scenarios

A scenario should show its behavioral sequence: setup, stimulus, observation,
and assertions are separately named steps when each represents meaningful
work. Reuse existing fragments before introducing a new one. A generated patch
matrix may choose combinations in Rust, but its behavior should compose the
same steps and fragments as ordinary scenarios. Use typed `l6ctl` operations
for guest control and host-owned domain interpretation; the runner owns test
sequencing, expected values, artifacts and pass/fail decisions.

When decomposing an existing operation, compare the expanded steps, model and
backend scope, fixtures, assertions, and cleanup behavior with the original.
Preserve each shared guest and wall deadline: includes propagate model scope
and `setup_input`, while explicit `include_guest_timeout_ms` and
`include_wall_timeout_s` overrides require uniform child deadlines. A sampling
window, recording duration, and negative observation are intentional durations;
do not replace them with readiness polling or turn concurrent capture into
sequential observations. Keep a bounded transaction together if splitting it
would lose an event or release a resource prematurely.

Use `cargo l6test list` to validate catalog composition, then run focused cases
for the affected models and backends with the normal worker count. Watch the
run from another terminal with `cargo l6test watch RUN_DIRECTORY`. Review
`report.json` and the individual `scenario.json` and progress streams: a
BLOCKED case did not run, and a retry that passes after failure is FLAKY. An
`expected_failure.step` identifies the one-based index in the runtime scenario
after include expansion and any `setup_input` removal, not the displayed count
after model filtering; revisit it when inserting steps.
For user-visible behavior, check rendered pixels against reviewed references.

### L6max stereo level-meter sweep

The `l6max-volume-meter-sweep` case uses the Pad 4 assignment already saved
on the SD card, presses pad 4, and records indicator PWM levels through a short
sweep and decay. It does not enumerate USB or assign the pad during the test.
Its card, [`l6max-meter-levels.toml`](../../../tests/emulator/fixtures/cards/l6max-meter-levels.toml),
holds a stereo WAV with successive left-only and right-only level stages, assigned to
Pad 4. The scenario uses no host audio backend, and its state spec sets the first-run
settings and panel clock needed for a ready guest. It allows the audio engine to start
after mixer readiness, verifies the active Pad 4 assignment, and turns the analog output
controls to maximum. It then presses the pad and captures the indicator PWM levels during
playback and decay. The default `sound-pad-playback` window is six seconds;
`playback_window_ms` can extend it to 120 seconds for long cards.

```sh
cargo run --offline --locked -p l6test -- run \
  --config config.toml --case l6max-volume-meter-sweep \
  --backend headless
```

Inspect `sound-pad-playback.json` in the case logs.
For an offscreen screenshot, use the `capture gui` command described above.
To render a recorded indicator sample, place its `indicator_levels` in a full
panel snapshot JSON containing `indicators`, `switches`, `analog`, `powered`,
and the 1024-byte `display` as shown in a `capture gui` artifact, then pass it
to `l6max-gui --render-panel l6max --data panel.json --output meter.png`.
The scenario requires at least two distinct left meter levels of two or more
bars with the right side off, and the same for the right with the left off.
The JSON records decoded meter levels alongside the raw panel output. This checks channel isolation and does
not establish physical meter calibration.

### L6 stereo level-meter sweep

The `l6-volume-meter-playback` case uses a similar stereo isolation WAV at
`SOUND_PAD/PAD1/TONE220.WAV`, from
[`l6-meter-levels.toml`](../../../tests/emulator/fixtures/cards/l6-meter-levels.toml). The scenario checks the active
assignment and one-shot mode, sets the four analog output controls to maximum,
plays the pad, and requires independent left and right meter responses without
USB setup. The report contains the per-sample
meter levels and panel output.

### L6 thresholded meter PWM

The `l6-level-meter-pwm` case drives the ten L6 1.10 meter accumulators at the
measured threshold values and samples all 192 raw panel matrix cells. It uses
a dedicated fresh state, no SD image, no USB or MIDI setup, and no host audio.
It needs L6 1.10 firmware. Run it with:

```sh
cargo run --offline --locked -p l6test -- run \
  --config config.toml --case l6-level-meter-pwm \
  --backend headless
```

The case requires at least one panel cell to show five distinct nonzero mean
duty readings. Inspect `l6-level-meter-pwm.json` for all cell means at each
input level. Each 500 panel-ms stage excludes its first 100 ms; completed stages and partial failure evidence are written incrementally. This terminal debugger probe restores journalled bytes when possible but cannot restore task or panel progression, so MAIN stays stopped and the guest must be restarted. The bounded mutation API is in-process; no equivalent control-socket sampling command is provided. This tests the firmware-to-panel path with controlled accumulator input; it does not qualify physical meter wiring or audio-input routing.
