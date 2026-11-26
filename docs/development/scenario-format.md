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
| `update_fixture` | no | Build a case-local update card: stock L6 `l6-stock` or `l6-matching` (a supported MAIN with a matching L6 package), synthetic L6max `bad-checksum` or `synthetic-valid`, or browser-built L6max `patched-midi`, `patched-knob`, `patched-knob-midi`, or `patched-din`. Managed update scenarios require an isolated headless lifecycle. |
| `update_goal` | no | Select the complete valid-update `handoff` or `install` lifecycle. |
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

An include may set `include_guest_timeout_ms` and/or `include_wall_timeout_s`
to replace the respective outer deadline of each included step. Overrides must
be positive and all applicable steps must have the same original deadline;
mixed deadlines are rejected so a shorter intentional budget is not silently
extended. Nested includes expand inside out. These overrides do not change
observation durations or other timing parameters inside an action.

Mark only input whose result a state spec reproduces with `setup_input`. Input
that is the behavior under test, or that sets runtime-only state (for example
card-reader mode), stays unmarked.

## Step operations

| Action | Fields | Meaning |
| --- | --- | --- |
| `wait` | `until = "boot-ready"`, `"mixer-ready"`, `"panel-settled"`, `"sd-patch-loaded"` or `"sd-patch-empty"` | Boot waits for the full LED sweep, then an L6 mode lamp or L6max pixels; with a prepared state L6max also requires a fresh MAIN clock and one MAIN guest second without a new display frame. Prepared mixer readiness requires nonzero values on every visible ring and, on L6, a lit mode lamp; settling waits for one guest second of unchanged output; SD readiness waits for either the loader’s successful claim after installation or its completed scan without a module. Both use 10 guest seconds, 180 wall seconds and 10 ms wall-time polling. A separate assertion must check the expected module state or behavior |
| `expect-sd-loader` | — | Freeze and validate the loader fixture, then check persistent loader and dispatch bytes in running MAIN |
| `expect-sd-module` | `loaded`, optional `chain_count` | Immediately follow `expect-sd-loader` with matching deadlines; assert the registry, hook chains and relocated immutable module bytes using that fixture. Loaded modules wait for the loader claim; absent modules are inspected immediately. |
| `expect-display-after-press` | — | Wait for the L6max display to change from the preceding press's baseline, within that press's original guest and wall deadlines. Use a reviewed image assertion when verifying a specific screen |
| `warm-reset` | | Reset MAIN in place and wait for QEMU's reset event. The panel remains running; subsequent `boot-ready` and `mixer-ready` waits require a fresh ring transition and a completed boot sweep before accepting panel state |
| `wait-effect-mode` | `panel_mode`, `edit_mode` (at least one) | Wait for the loaded L6max effect module’s published editor state, bounded by 10 guest seconds and 180 wall seconds with 10 ms wall-time polling |
| `wait-recorded-files` | `files` | Wait for each named WAV in the newest recorder project to have a complete RIFF container and at least two seconds of audio (20 guest seconds maximum, 180 wall seconds maximum, 100 ms wall-time polling). Check the audio contents separately |
| `wait-audio-input` | `playing`, `min_ms`, `max_ms` | Wait for the input transport’s reported state and position, bounded by 10 guest seconds and 180 wall seconds with 10 ms wall-time polling |
| `wait-memory` | `symbol`, `bytes` | Poll a model-declared firmware symbol until its bytes match, bounded by 10 guest seconds and 180 wall seconds with 10 ms wall-time polling. `expect-memory` remains an immediate assertion |
| `delay` | `guest_ms` | Wait on guest time, not host time |
| `observe-duration` | `activity` (`recording`, `playback` or `audio-output`), `guest_ms` | Observe an already-started activity for a test-defined duration in guest time; start and verify the activity separately |
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
| `hold-chord` | `controls` and either `duration_ms` or `until = "factory-mode"` | Hold multiple semantic controls together for the specified guest time or until L6max factory mode is entered. Factory-mode readiness is polled every 10 ms of wall time, bounded by 10 guest seconds and 180 wall seconds |
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
under `tests/smoke/images/`, or an existing `tests/emulator/references/` or patch's
`tests/emulator/references/` for its own UI). Comparison is exact after converting the reference to
black-and-white pixels. Review the screenshot before updating a reference;
memory and firmware-state assertions may supplement image comparisons but do
not establish that the UI rendered correctly. Set `settle_ms` when the
matching image must remain visible for a guest-time interval before the next
step; it defaults to zero. This is a stability requirement, not a polling
interval or wall-time timeout.

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

MIDI steps retain one native or direct USB session and its receive queue across steps.
`host-midi-connect` and `direct-midi-connect` start the first bounded transaction.
Starting another direct transaction releases the previous direct session before reacquiring
USB, while sync between transactions consumes the retained queue. A ring assertion with `midi_transaction = true`
uses the last CC send as its deadline anchor; a plain ring assertion is
independent. `host-midi-end` completes the transaction without closing the
session, and `host-midi-begin` starts another transaction on that session.
Receive, settle, and send steps require an active transaction. Encoder input channels are one-based; MIDI channels use the zero-based wire convention,
matching `l6ctl midi`. `enqueue-encoder` reports enqueueing without waiting for
the normal encoder acknowledgment, and `expect-ring-changed` compares a named
checkpoint without requiring encoder reports. A native MIDI session cannot share
USB endpoints with direct USB operations in the same scenario. The existing
L6max DIN mixer setup uses direct USB descriptors and SysEx, so it cannot be
combined with the native bridge in one scenario under this policy.
`midi-encoder-alternate` alternates -4 and +4 turns so repeated checks avoid saturation.
The negative cable check sends its CC and observes the ring in one operation; it
samples immediately after sending and requires the entire declared wall window
from that sample. Like the original polling check, it observes sampled states;
a transient between samples is not guaranteed to be detected. Typed MIDI receive filters non-CC ALSA events. Connection
retries honor the shared guest and wall limits while retaining the independent
10-second discovery cap. Ring observations at or after the declared wall deadline
fail even if the ring changed by that observation.

The L6max mixer/DIN cases use a separate `midi-probe-begin` / `midi-probe-end`
transaction. It owns a namespaced native DIN session and either reuses the
preceding native mixer session or owns direct USB cable 1. All probe steps use
the same guest/wall timeouts as the begin step; the total deadline starts before
endpoint discovery. No unrelated device operation may interrupt the sequence.
The probe releases its own sessions on completion, failure or restart, without
discarding an earlier mixer session. DIN receive uses the existing legacy
control-observation behavior, including its documented ALSA limitations.

The L6 firmware and SD MIDI hook checks use the shared
`patches/midi-sync/tests/emulator/fragments/l6-hook-probe.toml` fragment.
Its four actions resolve and verify the selected hook, inject a request, check
the hook input, and compare sender output against the configured CC map in
channel-major order, including duplicates. They share one guest/wall deadline.
The injected execution cannot yet be fully restored, so the final action leaves
MAIN stopped and the disposable emulator must be restarted. It must be the last
L6 action; mixed-model scenarios can scope the include to L6.

`midi-probe-send-cc` captures the declared ring channel immediately before the
stimulus. `midi-probe-expect-ring` compares against that capture, with its wall
window anchored to the send; a negative result requires the whole declared
guest window. For parity with the former mixer/DIN action, a ring change is
considered before that phase's deadline; the enclosing transaction deadline
still applies. `midi-probe-wait-ring-stable` anchors its wall limit to the same
send and restarts its guest-time stability clock whenever the ring changes.
`midi-probe-expect-no-events` observes until the wall deadline anchored to the
preceding `midi-probe-drain-and-tap`; it fails if that interval already elapsed before
the observation starts. It also drains the receive queue at the boundary so
messages from the final polling gap cannot be missed. Native MIDI observations
have no event timestamps, so the boundary drain may conservatively count a
message delivered just after the interval.

| Action | Fields | Contract |
| --- | --- | --- |
| `sd-card` | `inserted` | QMP removal or insertion of the case's copied card |
| `usb-descriptors` | — | Validate device/configuration descriptors and SET_CONFIGURATION |
| `usb-midi-open`, `usb-midi-begin`, `usb-midi-expect-encoder`, `usb-midi-prepare-level`, `usb-midi-set-alternate`, `usb-midi-set-target`, `usb-midi-end` | `value` on prepare | Retain the previously enumerated endpoints before the encoder; observe its model-mapped CC on any USB cable, then set and verify channel 1 on cable 1. The transaction keeps a single guest/wall budget; an alternate value is sent first when the target is already present. |
| `host-midi-connect` | — | Connect the model's native mixer endpoint and start the first MIDI transaction |
| `direct-midi-connect` | `cable` | Start a bounded direct USB MIDI transaction on cable 0–15 |
| `midi-encoder-alternate` | — | Enqueue the next alternating channel-1 encoder turn |
| `host-midi-send-and-expect-ring-unchanged` | `channel`, `controller`, `value`, `ring`, `checkpoint`, `observe_wall_ms` | Send a CC and observe the specified ring for the full bounded wall interval in one operation |
| `host-midi-drain` | `quiet_wall_ms` | Drain the retained native session until no legacy control observation arrives for the declared wall interval, within the transaction budget |
| `host-midi-expect-count` | `count` | Collect that many legacy control observations on the retained native session; preserve their order and remaining queue |
| `host-midi-begin` | — | Start a new bounded transaction on the retained session |
| `host-midi-end` | — | End the active transaction while retaining the session and its queue |
| `enqueue-encoder` | `channel`, `steps`, optional `midi_transaction` | Enqueue an encoder input without waiting for guest acknowledgment; optionally enforce the active MIDI budget |
| `host-midi-expect-cc` | `channel`, `controller` | Wait for a matching CC on the retained session |
| `host-midi-settle` | `guest_ms` | Allow guest time to pass within the shared MIDI deadline |
| `host-midi-send-cc` | `channel`, `controller`, `value` | Send a CC through the retained session |
| `expect-ring-changed` | `channel`, `checkpoint`, optional `midi_transaction` | Wait for a ring to differ from a named checkpoint; when `midi_transaction` is true, anchor its deadline at the MIDI send |
| `l6-din-usb-forwarding` | — | Verify L6 DIN-to-USB forwarding behavior |
| `host-midi-sync-begin` | — | Capture the L6 MIDI map and configured channel, or the L6max reply count, using a retained MIDI session. Begin the shared guest deadline. |
| `host-midi-expect-sync` | — | Assert the captured L6 channel and CC multiset, including duplicate mappings, or 94 L6max Control Change replies. Follow with `host-midi-end`. |
| `l6-midi-probe-begin` | — | Verify fixture identity, acquire a scoped debugger, and wait for L6 idle |
| `l6-midi-probe-inject` | — | Inject the guarded sync request; the guest must be restarted afterwards |
| `l6-midi-probe-expect-hook` | — | Verify the request reaches the selected firmware or SD hook unchanged |
| `l6-midi-probe-expect-replies` | — | Check the ordered configured CC replies, wait for the last sender return, and leave MAIN stopped |
| `l6-indicator-begin`, `l6-indicator-end` | Begin carries shared guest/wall deadline | Bound an L6 MIDI indicator campaign using the retained direct MIDI session. |
| `l6-ring-sample`, `l6-ring-coverage` | `cc`, `value`, `guest_ms`, optional `ring`; coverage `mask` | Send a CC, observe for the declared guest duration, reject unrelated ring changes, and check the accumulated segment mask. |
| `l6-mute-sample` | `cc`, `value`, `guest_ms`, `expected`, optional `reuse_previous` | Compare all eight panel indicator rows; `reuse_previous` uses the preceding sample as the baseline for a release edge. |
| `usb-sysex-begin`, `usb-sysex-exchange`, `usb-sysex-end` | Begin carries the shared guest/wall timeout; exchanges carry `bytes` and optional `expect_prefix`, `expect_reply`, `min_reply_bytes`, `reject_prefixes`, `reply_timeout_ms`, and `complete_on_match` | Send cable-2 requests over the previously enumerated USB connection. Each exchange is reported separately; no-reply requests, rejected-prefix checks and explicit reply windows observe the full interval. |
| `editor-assign-*` | `pad` for per-pad steps | Initialize Editor, select and assign a WAV to pads 1–4 and verify exact readback. Steps must be contiguous, use one shared deadline and follow `usb-descriptors`. |
| `editor-playback-begin`, `editor-playback-play`, `editor-playback-wait`, `editor-playback-observe`, `editor-playback-sample`, `editor-playback-assert`, `editor-playback-end` | `pad` on begin | L6 Editor playback check. Steps must be contiguous, share deadlines and follow a completed `editor-assign-*` sequence; observe 1200 ms guest audio and write evidence before checking it. |
| `ui-pad-begin`, `ui-pad-identity`, `ui-pad-global`, `ui-pad-keep-alive`, `ui-pad-query`, `ui-pad-assert`, `ui-pad-end` | `pad` on begin | Verify an L6max UI pad assignment through the Editor protocol. Steps must be contiguous, share deadlines, and follow `usb-descriptors`. The query checks up to eight replies and the assertion writes the per-pad evidence. |
| `wait-l6-sound-pad-file` | `path` (relative SD card path) | On supported stock L6 firmware, wait until pad 1's active UTF-16 path matches the file and its assignment is no longer pending; bounded by 10 guest seconds and 180 wall seconds, polled every 10 ms of wall time |
| `sound-pad-begin` followed by `sound-pad-press`, `sound-pad-hold`, `sound-pad-release`, `sound-pad-observe`, `sound-pad-sample`, `sound-pad-assert-waveform`, `sound-pad-assert-audio`, `sound-pad-assert-led`, `sound-pad-assert-meter` and `sound-pad-finish` | `pad`, `target`, optional `assignment` (`editor` by default or `saved-state`), optional `require_pad_led_change` (default `true`), optional `require_stereo_meter` (default `false`), optional `playback_window_ms` (default `6000`, range `1000..120000`), optional `expect_source` (WAV path on the card; requires `L6_AUDIO_RAW_CAPTURE`) and `max_residual_db` (default `-60`) | Press one L6 or L6max MAIN pad input and require nonzero SAI1 TX samples; require its model-specific pad LED response unless `require_pad_led_change = false`; `editor` requires a preceding Editor assignment step, while `saved-state` uses the assignment already present in the state; capture indicator PWM levels throughout playback; when `require_stereo_meter = true`, require two distinct levels of at least two bars on each side independently using a stereo isolation WAV; enable `L6_AUDIO_SAMPLE_TRACE=1` in the case environment |
| `patch-parity-prepare`, `patch-parity-firmware`, `patch-parity-loader`, `patch-parity-unsupported`, `patch-parity-finish` | Profile steps carry explicit patch lists or a rejected patch name. | Compare native and browser-template patch builders for the complete, model-scoped profile matrix. This host-only scenario uses the browser builder's Rust library; it does not execute WebAssembly or boot a guest. |
| `l6-recorder-begin`, `l6-recorder-press`, `l6-recorder-dispatcher`, `l6-recorder-assert-event`, `l6-recorder-handler`, `l6-recorder-assert-selection`, `l6-recorder-callback`, `l6-recorder-assert-callback`, `l6-recorder-observe`, `l6-recorder-release`, `l6-recorder-sample`, `l6-recorder-evidence`, `l6-recorder-assert-trace`, `l6-recorder-assert-dma`, `l6-recorder-end` | Begin: `target` | L6 only; execute in this order with the same guest and wall budgets. The scoped debugger verifies event `0x20`, handler acceptance and mode-0 callback on MAIN target 4. After five guest seconds, evidence must show a changed SAI1 TX signature and sample data in the channel-14 TCD source. Requires a mode-0 state, valid `MASTER.WAV`, GDB support and `L6_AUDIO_SAMPLE_TRACE=1`. Cleanup releases PLAY and restores the debugger on failure. |
| `l6max-recorder-begin`, `l6max-recorder-press`, `l6max-recorder-submit`, `l6max-recorder-queue`, `l6max-recorder-dispatch`, `l6max-recorder-scheduler`, `l6max-recorder-callback`, `l6max-recorder-action`, `l6max-recorder-stream`, `l6max-recorder-observe`, `l6max-recorder-sample`, `l6max-recorder-evidence`, `l6max-recorder-assert-audio`, `l6max-recorder-end` | Begin: `target` | L6max only, MAIN target 4; ordered steps share deadlines and a debugger scope. Verify Play event `0x2c`, queue success, event dispatch, callback slot 2, recorder action and stream read. After ten guest seconds require nonzero changed SAI1 TX windows and sample data in channel-14’s TCD source. Requires a prepared playable recording, GDB support and `L6_AUDIO_SAMPLE_TRACE=1`. |
| `midi-probe-begin` / `midi-probe-end` | — | Open/close a bounded L6max mixer/DIN observation; retain any previously opened native mixer session |
| `midi-probe-send-cc` | `port`, `channel`, `controller`, `value`, `capture_ring_channel` | Capture the ring immediately before sending on `mixer` or `din` |
| `midi-probe-expect-ring` | `channel`, `changed`, optional `guest_ms`, `wall_ms` | Observe change or a full unchanged guest window, anchored to the preceding send |
| `midi-probe-wait-ring-stable` | `channel`, `guest_ms`, `wall_ms` | Require a ring to remain unchanged for guest time, bounded by wall time since the send |
| `midi-probe-drain-and-tap` | `port`, `target`, `hold_ms` | Drain queued legacy control observations and immediately enqueue a tap to anchor the following wall-time observation |
| `midi-probe-expect-no-events` | `port`, `wall_ms` | Observe the selected session for the complete wall-time interval after the tap |
| `usb-storage-*` | `length`, `multiplier`, `increment` on create | Ordered, shared-deadline USB storage scope: enumerate and check readiness and geometry, compare sector zero with the copied card, round-trip the last sector, then create/read/compare/delete a 1,537-byte FAT file. Cleanup attempts to remove any owned file on failure. |
| `audio-callbacks` | — | First step only; arm revision-guarded breakpoints before boot and require every model callback |
| `knob-dialog-ready` | — | Wait for the L6max recorder window, idle notification state and reviewed transport-control pixels before `knob-dialog-guard`; all steps use matching deadlines shared across the sequence |
| `knob-dialog-guard` | — | Verify first-use rejection outside the recorder screen and return to the recorder; retain a screenshot and read state while QEMU is stopped |
| `knob-dialog-baseline` | — | Select level mode and capture the baseline frame for the later race check |
| `knob-probe-*` | probe for prepare, delta for trigger | Inject a guarded descriptor conflict, sample rejection, restore it and assert the saved state and heap |
| `knob-dialog-preflight-begin` | — | Capture the original descriptors, resources and heap, and open the debugger for the following five probes |
| `knob-race-*` | capture duration and artifact | Observe registration and callback breakpoints, capture and sample rejected descriptor drift, then restore the descriptor before asserting while retaining the notification semaphore for first activation |
| `knob-dialog-first-activation` | — | Wait for the display callback to return, then verify first activation and restore the notification semaphore |
| `knob-active-*` | — | Turn the knob, capture the active display and preserve its frame and callback pointer |
| `knob-level-*` | — | Capture the bounded level sweep to at least 69% and back to 0% |
| `knob-task-*` | — | Verify the input hook and display callback run in different FreeRTOS tasks, retaining their task IDs as evidence |
| `knob-overlay-*` | field for descriptor checks | Check the active window, saved descriptor snapshot, resource, background pixels, bitmap and heap against the baseline using the shared overlay fragment |
| `knob-bitmap-*` | — | Reactivate the unchanged zero-level overlay, require a fresh consumed mailbox and countdown of at least 15 ticks before the baseline, then require at least 10 ticks while stopped before checking bitmap ID 0 with the original frame, signature and descriptor assertions |
| `knob-overlay-reactivate`, `knob-overlay-wait-active` | delta for reactivation | Reopen the zero-level overlay without changing its value, then wait for its callback |
| `knob-overlay-refresh-*` | — | Check the rendered value and stable overlay pixels after the bitmap probe |
| `knob-lock-*` | — | Verify the dialog consumes and displays a level update while the notification semaphore is held, then restore the lock and check the mailbox |
| `knob-dialog-mode-begin` / `knob-dialog-mode-end` | `scene` on begin | Scope each declared recorder scene inside the owned debugger transaction and its overall deadline. The subsequent Pan sweep requires the final declared parameter mode to be 8. |
| `enqueue-tap` / `wait-input-applied` | `target`, `hold_ms` on enqueue | Enqueue a tap and wait for one aggregate APPLIED counter increment on the routed input channel, with a 10-second wall deadline. An increment is not proof of which client caused it. |
| `wait-knob-parameter-mode` | `mode` | Wait for the guarded parameter selector with its 10,000-guest-ms / 180-second wall caps. |
| `wait-knob-display-image` | `image`, `main_timeout_ms` | Within a knob scene, poll for exact reviewed pixels using a fresh MAIN clock, bounded by MAIN time and the shared panel/wall deadlines. The following capture and exact assertion remain independent; reference-update mode waits for a fresh MAIN clock without requiring an old image. |
| `capture-display` / `expect-captured-display-image` | `guest_ms`, `artifact` / `image` | Capture a frame after the requested guest duration, then compare that exact frame once with a reviewed PNG and write its named artifact. |
| `knob-dialog-expect-heap` | — | Compare allocation and free counters with the active-dialog baseline. The bounded mode walk validates the 1.10 allocator once; images replacing that allocator are rejected. |
| `knob-dialog-adaptive` | `plan` | Execute the bounded, typed Pan and channel-2 attempts, reporting individual child operations while retaining the debugger context. |
| `knob-dialog-inject-invalid-tag` / `knob-dialog-wait-invalid-tag` / `knob-dialog-confirm-invalid-tag` | `tag` on injection | Hand a tag to the callback while the debugger owns the guest, wait for the cleared mailbox with a deadline starting at handoff, and recheck while stopped. An interrupted handoff leaves the guest stopped. |
| `knob-dialog-settle-level` | `artifact` | Turn channel 1 and wait until the changed level is displayed and the mailbox is clear; capture 50 ms guest-time frames under the bounded transaction. |
| `knob-dialog-inject-invalid-signature` / `knob-dialog-observe-invalid-signature` / `knob-dialog-sample-invalid-signature` / `knob-dialog-restore-invalid-signature` | `guest_ms` on observation | Journal and inject the invalid state word, observe from guest resume, sample state and bitmap while stopped, then restore and assert rejection. Debugger cleanup restores outstanding writes on abort. |
| `knob-dialog-prepare-notification` | — | Verify both text lines are visible and capture the borrowed text-resource baseline. |
| `knob-dialog-arm-notification-callback` / `knob-dialog-wait-notification-callback` / `knob-dialog-invoke-notification` | `message` on invocation | Break on the active display callback, then invoke the stock notification with the retained debugger. The callback deadline starts when MAIN resumes. |
| `knob-dialog-wait-notification-frame` / `knob-dialog-expect-notification-invariants` | `clear`, `artifact`, `guest_ms`, `limit_ms` on wait; `clear` on assertion | Capture until the notification appears or clears. Appearance is bounded by panel guest time from invocation; clearance uses MAIN virtual milliseconds from invocation and requires changed pixels, an inactive guarded notification, and MAIN scheduler progress, with an independent wall deadline. The MAIN clock is sampled at 100 ms intervals and may lag during transport. Check the callback and borrowed text pointer and contents during overlap and after expiry. |
| `knob-dialog-wait-expiry` | `interval_ms`, `limit_ms` | Wait for the dialog countdown to reach zero using guest-time polling and a grace limit anchored after notification expiry. |
| `knob-dialog-expect-expiry-frame` | `after` | Compare the retained expiry capture against the saved post-notification scene (`true`) or notification (`false`) frame. Both comparisons use the same capture. |
| `knob-dialog-snapshot-restoration` / `knob-dialog-check-restoration` / `knob-dialog-finish-restoration` | `field` on check | Take one ordered, non-atomic restoration snapshot; compare its individual fields with the saved preflight evidence and report all mismatches together. |
| `knob-dialog-expect-restored-heap` | — | Check free bytes, allocation count and free count against the active-dialog heap baseline using the guarded allocator reader. |
| `enqueue-encoder-no-ack` / `enqueue-tap-no-ack` | `channel`, `steps` / `target`, `hold_ms` | Enqueue navigation inputs without waiting for guest acknowledgement, preserving the diagnostic's original input timing. |
| `release-captured-display` | — | Release a retained navigation capture after its artifact has been written, so the next capture can begin. |
| `knob-dialog-snapshot-navigation` / `knob-dialog-check-navigation` | `field` on check | Capture the ordered, non-atomic navigation state, stopping at the first failed check, then assert each recorded field against the saved baseline. The window must have changed. |
| `knob-dialog` | — | Finish the L6max diagnostic: check heap lifetime, notification expiry, display cleanup, widget restoration and exact encoder reports. The preceding mode walk checks reviewed display images. |
| `knob-popup-open` | `first_steps`, `fallback_steps`, `attempt_guest_ms` | On the L6max recorder screen, try the channel 1 level knob by `first_steps`, then by `fallback_steps` if the popup does not open within `attempt_guest_ms`; retain the initial display and level as evidence |
| `knob-popup-expect-open` | — | Require the dialog signature, callback and a changed display under the shared popup deadline |
| `knob-popup-expect-value` | — | Require the rendered channel 1 level text and changed mixer value under the shared popup deadline; unlike the original monolithic action, fail promptly when its overall step deadline expires |
| `expect-menu` | `menu`, optional `path`, `items` | L6max; read a menu's children from guest memory and require exactly the titles in `items`. `menu` is `main`, `project`, `sound-pad`, `mixer` or `system`; `path` descends through child titles. Menu Loader adds its rows when the menu is first entered |
| `effect-replacement-state` | optional `expected_effect_type`, `expected_processor_type`, `expected_effect`, `require_output`, `wait_for_slot` and other module fields | L6max effect replacement bundle; compare the selected stock slot and the loader's state with the expectations. `expected_effect` is the effect ID running in the selected slot; `require_output` requires that slot's replacement to have produced nonzero output; `wait_for_slot` waits for the selected slot and processor and, when output is required, a fresh nonzero output count |
| `lifecycle-start` | `profile` | Start `initial`, `installer`, cardless `restart`, or cardless `verify` on the same case-local state with fresh clocks and logs. The verify profile uses the frozen patched MAIN and report, and attaches USB at power-on. |
| `wait-panel-uptime` | `guest_ms` | Wait for absolute panel guest uptime, with a wall deadline and a 60-second stall deadline reset by progress on either guest clock. |
| `expect-update-state` | `mode`, `flags_mask`, `flags_value` | Require the guarded L6max update mode and masked package flags. |
| `lifecycle-stop` | — | Request QMP shutdown and require successful child exits before reading persistent state. |
| `expect-update-marker-absent` | — | With the guest stopped, use `l6nor` to assert that neither a pending nor failed update marker exists. |
| `expect-live-update-marker-absent` | — | Assert the marker is absent from the live state file before power handoff; this sample is non-atomic. |
| `expect-update-gui` | `window`, `progress_mask`, `progress_value` | Assert the guarded current update window and masked progress flags. |
| `capture-update-marker-pending` | — | Require the stopped guest's pending marker and save/hash its case-local MAIN NOR snapshot. |
| `mark-l6-update-pending` | — | Explicitly set and flush the replacement-installer marker in a stopped L6 fixture; this does not emulate a firmware handoff. |
| `expect-l6-update-reset-vector` | — | Compare the live L6 MAIN reset vector against the frozen package. |
| `expect-l6-update-comparison` | `field` | Compare one of the five installed L6 package segments, boot/protected NOR, or entire resource NOR against frozen package and pending-state data. |
| `expect-installer-outcome` | `expected` | Assert the typed emulator replacement-installer result for this boot. |
| `wait-update-panel-ready` | `require_programming` | Under one guest/wall deadline, wait until the expected panel image, mode and RTC response agree simultaneously. When `require_programming` is true, require 64 writes and the image-verification trace too. |
| `wait-usb-controller` | — | Attach raw USB and wait for controller readiness within the step’s guest and wall deadlines. |
| `begin-wall-deadline`, `end-wall-deadline` | `duration_s` on begin | Enclose sequential managed lifecycle steps in one shared wall-clock budget; it remains active across all enclosed steps and nesting is rejected. |
| `expect-update-mode` | `mode` | Assert the guarded update mode. |
| `expect-main-memory` | `address`, `bytes` | Compare a bounded non-atomic MAIN SDRAM read with scenario data. |
| `expect-update-comparison` | `field` | Assert one stopped-state package, protected NOR or panel comparison. |
| `capture-installed-panel` | — | Save/hash the stopped installed panel flash before restart. |
| `expect-panel-unchanged` | — | Compare the stopped panel flash against the captured install snapshot. |
| `power-begin`, `power-wait-ack`, `power-observe-on`, `power-wait-off`, `power-observe-stopped`, `power-end` | A contiguous transaction with matching step budgets. | Observe an exact button receipt, power result and stopped panel clock. MAIN acknowledgement and power-off timestamps use the MAIN clock; the powered-on window uses the panel clock. |
| `restart` | — | Retire the session before reopening its isolated persistent state; reset readiness, trace counters and action baselines |
| `expect-recorder` | — | L6max startup mode restored after restart |
| `expect-settings` | `restored` | L6max edited RTC/NOR/version records, or reload and RTC startup timing after restart |
| `l6max-panel-key-map` | — | Verify that L6max sound-pad targets 15, 20, 25 and 30 set their MAIN key-state bits and clear them after release |
| `signal-pwm-*` | `band`, `start`, `end`, `value`, `channel` on selected steps | Seed L6max 1.10 channel accumulators and verify five increasing PWM levels on eight SIGNAL lamps. A contiguous scope shares deadlines and requires GDB. This tests firmware-to-panel brightness, not audio-input routing. |
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
| `l6-sweep-*` | `target`, `index` on selected steps | Exercise all L6 panel targets and encoders, compare UART reports, and check unsupported inputs are rejected. The contiguous L6-only scope shares a deadline. |
| `l6-din-sweep` | — | Sweep supported L6 DIN values and record dispatch behavior |
| `l6max-timing-*` | `wall_ms`, `guest_ms`, `target`, `hold_ms`, `pairs`, `index` and `steps` on selected steps | L6max: an ordered, shared-deadline scope pauses MAIN and panel, verifies queued input and clock behavior, resumes, checks UART reports and display change, then checks one encoder report. Wall observations and guest-time observations are separate. |

The shared indicator fragment budgets 10,000 guest ms and 90 wall seconds per
campaign. Its fixed 6,000 guest ms of observations remain unchanged; the
remaining budget covers runner work between samples and slower guests under
parallel load. Both ceilings remain hard failures.

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

The MIDI state request fragment begins a sync scope, drains until 500 ms of wall-time quiet, sends CC 127/127, asserts the reply, then ends the scope. The System menu fragment begins and asserts without a drain or request so a response queued by the menu action is preserved. Guest time is measured from scope start; the reply wall deadline begins after the send, or after preparation for a menu response. The shared guest and wall deadlines also bound preparation, the quiet drain and request.

`effect-paced-turn` applies a signed number of one-step L6max effect-editor turns. Each turn waits for its exact panel acknowledgement and one consumed effect edit, with an empty mailbox and idle callback gate. A dropped edit, unexpected counter jump, lost acknowledgement, or the step's shared guest/wall deadline fails the step. The watcher shows completed turns; subsequent ring assertions retain their original baseline and report-count checks.

The `l6max-panel-key-begin`, `l6max-panel-key-check`, `l6max-panel-key-input`, and `l6max-panel-key-end` actions describe the bounded sound-pad lookup and held-key sequence. The begin step owns the shared wall deadline, and pressed controls are released on scope cleanup.

The `din-usb-begin`, `din-usb-attach`, `din-usb-send`, `din-usb-expect`, and `din-usb-end` actions keep a native DIN sender and an exclusive direct USB receiver alive across a forwarding test. Send and expect actions must pair the same bounded byte sequence; each expectation has a three-second wall deadline anchored after the corresponding send. The initial `delay` states the guest-time bridge settling period.

A case can instead declare an ordered, model-scoped campaign using `models` and
`[[case.campaign]]` entries with `name`, `model`, and `scenario`. Every declared
model must have at least one compatible headless scenario. Selecting the case
runs that model's members in order under one wall deadline, each with isolated
state and card; the first failure stops the campaign. Nested campaigns and
case-level volatile or SD overrides are not supported.
