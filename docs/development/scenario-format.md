# Smoke scenario format, version 1

A scenario is a strict TOML sequence, executed by one `smoke-core::Runner` on
both adapters. Unknown actions, misspelled fields, unsupported controls,
model/channel mismatches, unsupported versions and zero timeouts are errors
before boot. Channels are one-based. Electrical IDs belong in Rust model
mappings, and a scenario names a firmware address only by its `patches/api`
symbol.

```toml
version = 1
name = "mute"
models = ["l6", "l6max"]

[[step]]
action = "wait"
until = "boot-ready"
guest_timeout_ms = 120000
wall_timeout_s = 600

[[step]]
action = "press"
control = "channel.1.mute"

[[step]]
action = "expect-mute"
channel = 1
guest_timeout_ms = 3000
```

The example abbreviates prepared setup (SD navigation, mixer restoration and
LEVEL selection); see the checked-in
`tests/emulator/cases/panel/mute.toml` for the prepared sequence.

## Scenario header and common step fields

The scenario document has one header table followed by one or more `[[step]]`
tables. Unknown header fields fail parsing.

| Header field | Required | Meaning |
| --- | --- | --- |
| `version` | yes | Schema version; currently `1`. |
| `name` | yes | Nonempty scenario name used in reports. |
| `models` | yes | One or both of `"l6"` and `"l6max"`. |
| `state` | no, default `setup-complete` | Boot from `tests/emulator/fixtures/states/<model>-<state>.toml`; `"none"` boots without a state (first boot). |
| `card` | no, default `empty` | Card to build for each run, from `tests/emulator/fixtures/cards/<card>.toml`, resolved for the model; `"none"` attaches no card. |
| `patches` | no | Firmware patches applied to the model's package. |
| `installed_patches` | no | Patches the engine installs on a device seeded with the model's stock package, as `--patch` does. |
| `installed_sd_patches` | no | Patches the engine delivers on the case's card, as `--sd-patch` does: it installs the Patch loader and writes the hosted modules to `PATCHES/`. Requires `card`; cannot combine with `sd_patches`. |
| `update_patches` | no | Patches per model for the case's update package, built from the model's package. |
| `gdb` | no | Enable the debugger socket. |
| `audio_device` | no | QEMU audio-device syntax; `{logs}` expands to the case's log directory. |
| `audio_input` | no | Array of tables assigning audio to input channels: `channel` plus one card-file source such as `loop-tone` or `copy`. Nothing plays until an `audio-input-transport` step. See [audio input](../emulator/audio.md#input). |
| `backends` | no | `"headless"` and/or `"gui"`; defaults to both. Typed device/GDB operations require headless. |
| `trace_sd` | no | Enable SD trace instrumentation for the scenario. |

Every `[[step]]` has a required `action` and may also set:

| Common step field | Default | Meaning |
| --- | --- | --- |
| `models` | all scenario models | Restrict this step to a subset of the scenario models. |
| `guest_timeout_ms` | `10000` | Guest-clock deadline for the step. Must be positive. |
| `wall_timeout_s` | `180` | Host wall-clock deadline for the step. Must be positive. |
| `setup_input` | `false` | The step is UI input that only brings the device to the scenario's starting state, such as confirming startup prompts. The driver drops it when the case state was built from a state spec. On an `include`, it applies to every included step. |

Each action table adds its action-specific fields below. For composed scenarios,
`include` takes a path to a TOML fragment containing only `[[step]]` tables.

Every path a scenario or fragment names, an `include` or an `expect-display-image`
reference, is written from the repository root, whichever file it appears in:
`path = "tests/emulator/fragments/prepared-home.toml"`. Absolute paths
and `..` are rejected, so a generated scenario composes the same way from its
text alone. A fragment's steps scoped only to models the scenario does not run
are left out, so a single-model scenario can include a shared fragment.

Mark only input whose result a state spec reproduces with `setup_input`. Input
that is the behavior under test, or that sets runtime-only state (for example
card-reader mode), stays unmarked.

## Step operations

| Action | Fields | Meaning |
| --- | --- | --- |
| `wait` | `until = "boot-ready"`, `"mixer-ready"` or `"panel-settled"` | Boot waits for the full LED sweep, then an L6 mode lamp or L6max pixels; prepared mixer readiness requires nonzero values on every visible ring and, on L6, a lit mode lamp; settling waits for one guest second of unchanged output |
| `delay` | `guest_ms` | Wait on guest time, not host time |
| `include` | `path` | Expand a strict `[[step]]` fragment named from the repository root; cycles and depth (eight) are checked |
| `select-mode` | `mode` | Pass if the mode lamp is lit; otherwise wait for one guest second of stable panel output and press the mode button |
| `switch` | `control`, `enabled` | Set a latching switch using its real GUI toggle or one headless electrical edge |
| `checkpoint` | `name` | Save display/rings and PNG/ASCII evidence under a unique simple name |
| `expect-display` | `checkpoint`, `changed` | Compare retained pixels with the checkpoint; scoped to L6max |
| `expect-display-image` | `image` | Compare all 128×64 L6max display pixels exactly with a PNG named from the repository root |
| `expect-trace` | `contains`, optional `absent` | Require every listed pattern and reject forbidden patterns in the active trace |
| `expect-trace-count` | `pattern`, `count`, optional `at_least`, `checkpoint` | Assert exact or minimum active-trace match count, optionally relative to a checkpoint |
| `expect-button-report` | `control` | Exact L6max press/release UART edges relative to the preceding press/switch, including no spurious switch release |
| `expect-startup-report` | `control` | Check the expected startup report for a mapped control |
| `press` | `control` | Real down/up pair, 50 guest-ms minimum hold; await both applied acknowledgements; UP is acknowledged after release |
| `encoder` | `channel`, `steps`, optional `gesture` | Positive steps raise the knob; use production polarity mapping; await application |
| `expect-ring` | `channel`, `mask`, `checkpoint` and/or `changed = true`, optional `same_as_checkpoint`, `only = true`, `settle_ms` | Verify output and exact encoder firmware report count relative to the preceding action; `same_as_checkpoint` asserts the ring mask matches a named checkpoint |
| `expect-ring-level` | `channel`, `min`, `max`, optional one-based `segment` (1–9) | Check the brightest ring segment, or a specific segment's PWM level |
| `expect-encoder-reports` | — | Exact firmware report count for the preceding encoder command, without assuming its editing mode changes a ring |
| `expect-mode` | `mode` | Verify the model-specific mode lamp before taking ring baselines |
| `expect-mute` | `channel` | Verify the model-specific mute lamp toggled relative to the preceding action |
| `hold` | `control`, `duration_ms` | Keep a semantic control held for a bounded duration |
| `hold-chord` | `controls`, `duration_ms` | Hold multiple semantic controls together |
| `save-scene` | `scene` | Save the current assignment to a scene |
| `expect-scene` | `scene`, `lit`, optional `checkpoint`, `changed` | Check a scene indicator, optionally against a checkpoint |
| `expect-button-led` | `control`, optional `lit`, `checkpoint`, `changed` | Check a mapped button lamp's level or change |
| `expect-effect` | `effect` | Check the current effect indication |
| `analog` | `channel`, `steps`, optional `gesture` | Move a one-based analog control by signed steps |
| `expect-analog` | `channel`, `value` | Check a control's analog value |
| `expect-powered` | `powered` | Check the current power state |
| `expect-memory` | `symbol`, `bytes` | Compare MAIN memory at `symbol`, a name declared in `patches/api` for each model the step runs on; an undeclared name fails when the scenario loads |
| `expect-heap` | `minimum_free` | Require the FreeRTOS heap's lowest free byte count since boot to be at least `minimum_free`, which must fit the heap. Runs on patched images; see [heap headroom](../../patches/README.md#heap-headroom) |
| `panel-event` | `target`, `column`, `row`, `event` | Inject a panel-controller event for focused event-path scenarios |

Boot steps explicitly allow a longer ceiling for slow L6 TCG. Assertion
baselines are captured immediately before an action, never after its output
has already changed.

`gesture` is `scroll` (default), `drag-vertical` or `drag-horizontal`. Headless
execution applies the equivalent logical knob movement. GPUI dispatches the
specified gesture into its actual hit region and handlers. Thus event geometry,
scroll accumulation and redraw bugs remain GUI-adapter coverage. Fractional
motion is additionally covered by production gesture unit tests.

For UI behavior, use `expect-display-image` to compare the rendered 128×64
L6max pixels with a reviewed PNG, named from the repository root (normally
under `tests/emulator/references/`, or a patch's
`tests/emulator/references/` for its own UI). Comparison is exact after converting the reference to
black-and-white pixels. Review the screenshot before updating a reference;
memory and firmware-state assertions may supplement image comparisons but do
not establish that the UI rendered correctly.

Supported semantic controls include each physical channel mute, modes
`eq-high`, `freq`, `mid`, `low`, `aux1`, `aux2`, `efx`, `pan`, `level` and L6max
`sub-mix`. L6max also accepts its canonical `controls::CONTROLS` names and
`recorder.menu/up/down/confirm`. Wiring stays in `control_target`; unsupported
model/control combinations fail early. The L6 USB startup scenario uses `ch4-mono`.

## Device and lifecycle steps

Scenarios default to `backends = ["headless", "gui"]`. Steps that drive emulated
devices require `backends = ["headless"]`; a scenario that names another backend
is rejected when it loads.
`trace_sd = true` enables SD trace instrumentation (also enabled by `sd-card`).

| Action | Fields | Contract |
| --- | --- | --- |
| `sd-card` | `inserted` | QMP removal or insertion of the case's copied card |
| `usb-descriptors` | — | Validate device/configuration descriptors and SET_CONFIGURATION |
| `usb-midi` | `value` | Require the preceding encoder input's model-mapped channel-1 CC on USB MIDI IN, then send that CC with `value` |
| `host-midi-round-trip` | — | Verify host MIDI input/output delivery through the configured host MIDI backend |
| `direct-usb-midi-round-trip` | optional `cable` (0–15), `expect_ring_change` (defaults to true) | Check the encoder report and channel-1 mixer CC on a selected USB MIDI cable; set `expect_ring_change = false` to verify other cables are ignored |
| `l6-din-usb-forwarding` | — | Verify L6 DIN-to-USB forwarding behavior |
| `host-midi-sync` | optional `request` (default `true`) | Send the CC 127 state request on the client of a preceding round trip and collect the model's full Control Change reply. With `request = false`, only collect a reply to a request made another way, such as the **Send MIDI State** menu item |
| `l6-midi-sync` | — | Exercise the L6 MIDI synchronization path |
| `l6-midi-ring-pin-sweep` | `cc` | Sweep L6 MIDI CC values and compare ring-pin output |
| `l6-midi-mute-indicators` | — | Check L6 MIDI mute-indicator responses |
| `usb-sysex` | `requests` with `bytes`, optional `expect_prefix`, `expect_reply`, `min_reply_bytes` | Send cable-2 SysEx requests, reassemble replies, and check presence, length, or prefix as declared |
| `editor-pad-assignments` | — | Initialize the Editor session, enumerate WAV files, assign the first file on all four pads and verify assignment state through SysEx readback; requires a preceding `usb-descriptors` step |
| `editor-pad-playback` | `pad` | L6-only assigned-pad playback check; requires preceding `editor-pad-assignments` |
| `l6max-ui-pad-assignment` | `pad` | L6max UI assignment check; requires preceding `usb-descriptors` and an L6max-scoped scenario |
| `sound-pad-playback` | `pad`, `target`, optional `assignment` (`editor` by default or `saved-state`), optional `require_pad_led_change` (default `true`), optional `require_stereo_meter` (default `false`), optional `playback_window_ms` (default `6000`, range `1000..120000`), optional `expect_source` (WAV path on the card; requires `L6_AUDIO_RAW_CAPTURE`) and `max_residual_db` (default `-60`) | Press one L6 or L6max MAIN pad input and require nonzero SAI1 TX samples; require its model-specific pad LED response unless `require_pad_led_change = false`; `editor` requires a preceding Editor assignment step, while `saved-state` uses the assignment already present in the state; capture indicator PWM levels throughout playback; when `require_stereo_meter = true`, require two distinct levels of at least two bars on each side independently using a stereo isolation WAV; enable `L6_AUDIO_SAMPLE_TRACE=1` in the case environment |
| `wasm-patch` | — | Exercise a generated WebAssembly patch package through the firmware update path |
| `l6-recorder-playback` | `target` | L6 only; GDB verifies event `0x20`, handler acceptance, and the mode-0 callback on MAIN target 4; after five guest seconds it requires a changed SAI1 TX signature and sample data in the channel-14 TCD source. Requires a mode-0 state, valid `MASTER.WAV`, GDB support, and `L6_AUDIO_SAMPLE_TRACE=1` |
| `l6max-recorder-playback` | `target` | L6max only, MAIN target 4; GDB verifies Play event `0x2c`, event dispatch, callback slot 2, and the recorder action, then requires nonzero changed SAI1 TX windows and sample data in channel-14's TCD source. Requires a prepared playable recording, GDB support, and `L6_AUDIO_SAMPLE_TRACE=1` |
| `l6max-din-mixer-control` | `expect_din_ring_change` | With Mixer Control via MIDI on, set by editor SysEx or a state, verify USB mixer control, stock/patched DIN response, and DIN mute-echo suppression. The USB reference uses the client of a preceding round trip when there is one, else USB cable 1 directly |
| `usb-storage` | — | Enumerate storage, compare sector zero to the copied card, round-trip its last sector, then create/read/compare/delete a 1,537-byte FAT file through the USB disk |
| `audio-callbacks` | — | First step only; arm revision-guarded breakpoints before boot and require every model callback |
| `knob-dialog` | — | Patched L6max only; compare all ten parameter scenes with reviewed display images, then verify heap lifetime, busy-aware expiry, display cleanup, widget restoration and exact encoder reports |
| `knob-dialog-popup` | — | L6max with Knob Dialog, on the recorder screen; turn the channel 1 level knob and require the dialog's state, display callback and value text for the new level, then let the window close |
| `expect-menu` | `menu`, optional `path`, `items` | L6max; read a menu's children from guest memory and require exactly the titles in `items`. `menu` is `main`, `project`, `sound-pad`, `mixer` or `system`; `path` descends through child titles. Menu Loader adds its rows when the menu is first entered |
| `effect-replacement-state` | optional `expected_effect_type`, `expected_processor_type`, `expected_effect`, `require_output` and other module fields | L6max effect replacement bundle; compare the selected stock slot and the loader's state with the expectations. `expected_effect` is the effect ID running in the selected slot; `require_output` requires that slot's replacement to have produced nonzero output |
| `firmware-update` | — | L6 and L6max disposable-state update lifecycle, corrupt-checksum rejection, NOR segment install, L6max panel programming, protected-region preservation and clean restart; an `update_patches` package additionally checks model-specific post-install behavior |
| `power-press` | `hold_ms`, `powered` | Check expected power result, model cutoff timing and stopped clock |
| `restart` | — | Retire the session before reopening its isolated persistent state; reset readiness, trace counters and action baselines |
| `expect-recorder` | — | L6max startup mode restored after restart |
| `expect-settings` | `restored` | L6max edited RTC/NOR/version records, or reload and RTC startup timing after restart |
| `l6max-panel-key-map` | — | Verify that L6max sound-pad targets 15, 20, 25 and 30 set their MAIN key-state bits and clear them after release |
| `l6max-channel-signal-pwm` | — | Seed the twelve firmware channel-level accumulators and verify five distinct increasing sampled PWM levels on all eight green SIGNAL lamps; requires L6max MAIN 1.10 and GDB. This qualifies the firmware-to-panel brightness path, not audio-input routing. |
| `l6-level-meter-pwm` | — | Seed ten L6 1.10 level accumulators at the measured brightness thresholds and sample the raw panel matrix; require at least one output cell with five distinct nonzero duty means. Uses GDB and no SD, USB, MIDI, or host audio. |
| `l6-main-gpio-event` | `target`, `event` | L6 GDB check that Tap target 48 submits event `0x19` and Sound Pad targets 49–52 submit `0x15`–`0x18` |
| `audio-input-transport` | `command` (`play`, `pause`, `stop` or `seek`), `position_ms` for `seek` | Control input playback |
| `audio-input-load` | `channel`, `loaded` | Map a channel's prepared audio again, or drop it |
| `expect-audio-input` | `playing`, `min_ms`, `max_ms` | Check the reported input playback state |
| `expect-output-tone` | `hz`, optional `left_db`, `right_db`, `tolerance_db` (default 1) | Measure a tone in the last second of `L6_AUDIO_RAW_CAPTURE`. A side with no level must be below -70 dB |
| `expect-recorded-tone` | `file`, `hz`, optional `left_db`, `right_db`, `tolerance_db` (default 1) | Measure a tone in a file of the newest recorder project, after its first second. A side with no level must be below -70 dB |
| `expect-card-entry` | `path`; optional `size`, `matches_state` | Wait until the firmware has created `path` on the SD card, within `wall_timeout_s`. With `size`, the file must have exactly that many bytes. With `matches_state`, its bytes must equal that file in the device state directory, for example `main-nor.bin` |
| `l6max-tap-handler` | none | L6max GDB check that Tap target 48 reaches the firmware's TAP press handler. |
| `l6-din-event-dispatch` | `group`, `cc`, `value` | L6 check of DIN CC-to-event dispatch |
| `l6-usb-startup` | — | First step only; L6 probe replays the startup USB-enable sequence, verifies three firmware entry points and controller writes |
| `l6-input-sweep` | — | Sweep supported L6 controls and record their input responses |
| `l6-din-sweep` | — | Sweep supported L6 DIN values and record dispatch behavior |
| `l6max-input-timing` | — | Measure L6max input timing and applied acknowledgements |

Peripheral calls have bounded I/O and the step's guest/wall deadline. Callback,
startup and settings operations require the known MAIN revision; existing
installed MAIN bytes must match the selected image. Firmware addresses stay in
Rust. Restart preserves the settings comparison checkpoint but clears stale
per-action observations and boot readiness.

Scenarios do not name firmware hashes. Before a case boots, the runner
identifies its MAIN image: the supported stock 1.10 image of its model, or an
image our patch tools built from it and reported in `patch-report.json`
(`smoke_core::firmware`). Any other image does not run. Each action's
supported models, whether it needs the headless backend, whether it must come
first, and whether it needs the stock image itself are listed once in
`tools/smoke-core/src/requirements.rs`; this page does not repeat them. A step
that needs the stock image fails before boot on a patched build.

A fragment contains only `[[step]]` entries. Parent include model scope is
intersected with child scopes. Use fragments for shared setup or control groups;
there are no arbitrary shell commands, expressions or loop constructs.

## Editor support

[Tombi](https://tombi-toml.github.io/tombi/) is the supported TOML tooling for
case and fragment files. `tombi.toml` applies the JSON Schemas in
`tests/emulator/schemas/`, generated from the Rust types that parse these files.
Through Tombi's language server, in an editor or an agent's LSP client, they
give completion, documentation and errors while a file is written: actions,
fields, `expect-memory` symbols and suite names. `uvx tombi lint <files>` reports
the same errors from the command line.

The Rust loader is the authority, and Tombi only gives earlier feedback.
`cargo l6test list` loads and validates every case and fragment without
firmware. It also checks what the schema cannot: per-model symbol scope, step
order, includes and file references.

Run `cargo l6test schema` after changing a scenario, step or `[case]` type, or
declaring a symbol or suite; `cargo test -p l6test` fails while the committed
schemas are stale.

## Extending the driver

Add a scenario and catalog entry for sequences using existing operations. Add
complex USB, MIDI, storage or debugger assertions as typed Rust operations with
focused tests. Avoid arbitrary shell commands, embedded expressions, loops or
raw firmware addresses in TOML.

A catalog case has either a scenario path or a supervised GUI executable. A case with
`needs_configuration` and no scenario runs the packaged GUI from `--app`, which keeps
native menu and picker interactions in the GUI adapter. These cases boot from the
`setup-complete` state and an `empty` card, and stay BLOCKED without `--app`.

Legacy arguments support `{firmware_dir}`, `{logs}`, `{qemu}` and `{package}`
substitutions; case environments also support `{l6_package}` and `{app_qemu_sha256}`. Remaining braces are an error. Paths in these arguments must be
absolute. The driver strips ambient `L6_*` and `L6MAX_*` variables
from every child, then applies declared case configuration and explicit logging/
case-context settings. This prevents old probe flags silently changing a later run.

Packaged bundle/menu tests run under the driver with the app from `--app`
executable, isolated `L6_APP_DATA` beneath the case artifacts, and an explicit
hash check for the QEMU shipped inside the bundle. The bundle case imports both
model packages and checks their separate persisted state; the menu case
exercises native picker dispatch while the guest is running. Both cases need a
local config with the packaged assets before they can run.

Ring expectations wait for `settle_ms` (default 1000 guest milliseconds) with
unchanged physical ring values and no new encoder reports. Full-ring scenarios
use 3000 ms for delayed L6 feedback; the guest/wall step limits still apply. The `only` assertion checks six physical rings on
L6 and eight on L6max; extra generic decoder slots alias button lamps on L6.
Mapping scenarios use `changed = true` because presets and firmware processing
can change the numerical value associated with detents. Use `mask` when an exact
value is established by the state and firmware. Reverse movement in the
quick scenario must restore channel 2's known initial mask `0x001`.
