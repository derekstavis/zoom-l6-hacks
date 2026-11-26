# `l6ctl`

`cargo l6ctl` is the offline, locked alias for
`cargo run --offline --locked -p l6ctl --`. It sends commands to an already
running emulator. Start the emulator
with an explicit socket path:

```sh
cargo run -p l6max-gui -- --control-socket /tmp/l6-control.sock
```

Then run commands in another terminal:

```sh
cargo l6ctl --socket /tmp/l6-control.sock info
cargo l6ctl --socket /tmp/l6-control.sock input status panel
cargo l6ctl --socket /tmp/l6-control.sock heap-counters
cargo l6ctl --socket /tmp/l6-control.sock midi-config
cargo l6ctl --socket /tmp/l6-control.sock mixer-level
cargo l6ctl --socket /tmp/l6-control.sock usb attach
cargo l6ctl --socket /tmp/l6-control.sock usb release
cargo l6ctl --socket /tmp/l6-control.sock usb controller
cargo l6ctl --socket /tmp/l6-control.sock installer-outcome
cargo l6ctl --socket /tmp/l6-control.sock update-state
cargo l6ctl --socket /tmp/l6-control.sock update-gui-state
cargo l6ctl --socket /tmp/l6-control.sock button channel.1.mute
cargo l6ctl --socket /tmp/l6-control.sock down play 500
cargo l6ctl --socket /tmp/l6-control.sock up play
cargo l6ctl --socket /tmp/l6-control.sock encoder 1 -2
cargo l6ctl --socket /tmp/l6-control.sock analog 1 3
cargo l6ctl --socket /tmp/l6-control.sock set-analog 1 512
cargo l6ctl --socket /tmp/l6-control.sock rtos tasks --pretty
```

`--socket PATH` can be omitted when `L6_CONTROL_SOCKET` is set. The socket is
created with current-user-only permissions and is removed when the emulator
exits. `info` reports model, guest time, power state, and indicator snapshots.

`sd-loader status` reads the model-scoped L6/L6max 1.10 loader root and,
when allocated, its stage, claim, latest module and site count. `sd-loader
site INDEX` inspects one indexed dispatch chain, including its links. The
site index comes from the loader build's dynamic-dispatch report. These reads
are bounded and revision-guarded, but non-atomic: a concurrent loader update
can make a snapshot inconsistent or cause an inspection to fail. Retry only
when a caller's own readiness condition and deadline allow it.

`input status main|panel` returns a compact JSON observation of one input
channel: its `source`, `connected` state, `guest_ms`, `sent`, `applied`,
`released`, `rejected`, and optional `error`. The counters aggregate all
clients on that channel and live for that host connection. They are not a
per-command acknowledgement: another input can advance them, and a socket
command's receipt still means enqueueing only. The 32-bit guest clock wraps;
values can lag guest execution. Status fields are copied under one host lock,
not sampled atomically with the running firmware or the other channel. The
reply reader can observe an applied event before the sender increments `sent`,
so `applied` can transiently exceed `sent`.

Encoder and analog responses include the enqueued input `sequence`. Use
`input ack panel SEQUENCE` for an encoder, or `input ack main SEQUENCE` for
an analog input, to query its individual `pending`, `applied`, or `rejected`
state. The host tracks the most recent 256 inputs per channel; `unknown` means
the sequence was never tracked or has been evicted, and does not imply that it
was applied. Acknowledgement confirms guest input handling, not that a later
firmware action or display update has completed.

`button` (alias `tap`) presses and releases after 50 ms by default; use
`--hold-ms MS` to change it. `down NAME|TARGET DURATION_MS` presses for a
bounded duration, while `up NAME|TARGET` releases it. Names are semantic names
from the shared host control map (for example `channel.1.mute`, `mode.level`,
or `recorder.menu`); numeric GPIO targets `0..53` are also accepted.
`encoder CHANNEL STEPS` uses one-based channels and signed detents.
`analog CHANNEL STEPS` moves one of five knobs by signed steps; `set-analog`
sets a knob position from `0..1023`.

Use `cargo l6ctl --help` for the syntax summary. The endpoint is
opt-in: an emulator started without `--control-socket` exposes no control
socket.

## Rust API

The `l6ctl` crate also exposes typed control requests for Rust callers.
`ControlClient::new(path)` attaches to the existing optional socket;
`info`, `input_status`, `input_ack`, `tap`, `down`, `up`, `encoder`, `analog`, and `set_analog` return
typed responses. Encoder and analog channels use the same one-based numbering
as the CLI. Socket input receipts indicate that the request was enqueued,
not that the guest applied it. `request` and `RtosKind` expose the existing
versioned RTOS JSON until its socket responses have shared typed schemas.

Code that already owns an `l6max_host::engine::Engine` can borrow it with
`OwnedControl::new(&engine)`. Its `info`, `input_status` and `rtos_*` methods use the host's
existing snapshots directly; it does not create a second guest or open a
control socket. RTOS decoding and model/revision guards remain in
`l6max-host`, and the two transports do not yet expose identical capabilities.

The in-process `OwnedControl::raw_tap`, `raw_down`, `raw_up`, and
`raw_panel_encoder` methods target a specified input channel without the
normal model or target routing checks. They support protocol rejection tests;
they are not exposed by the control socket or CLI. `panel_uart_count` counts
exact L6 panel trace markers in supplied text.

`ControlClient::heap_counters()` returns one guarded, non-atomic sample of the
four allocator counters. In-process callers can create
`OwnedControl::heap_counter_reader()` once for a bounded observation, then
call `read()` for each sample; recreate the reader after guest restart or a
firmware change. The reader verifies both model-specific 1.10 allocator
routines before trusting the addresses. Patches that replace either routine
are rejected, even if some other guest state still appears compatible.

`l6ctl heap-counters` prints the same typed JSON snapshot (`model`, `atomic`,
`revision_guard`, `free`, `minimum_ever_free`, `allocations`, and `frees`).
The counter sample is one live guest-memory read; it neither traverses the heap
nor validates a stable or plausible heap snapshot. Successful allocation and
free counters are 32-bit unsigned values and may wrap. Use `rtos heap` when
the heavier, consistency-checked block inventory is needed.

## FreeRTOS snapshots

`l6ctl rtos heap|tasks|scheduler|objects|all` reads the running guest through
the same guarded decoder as the Debug window. The emulator must be started
with `--control-socket`; no firmware code runs and no guest heap is allocated.
The client prints one compact JSON object on stdout; `--pretty` formats it for
reading. Errors are JSON on stderr and exit nonzero.

Each single-category snapshot has `schema: 1`, `kind`, `model`,
`panel_guest_ms`, `atomic: false`, `revision_guard`, and a category-specific
`data` object. `panel_guest_ms` is the panel clock sampled after the read, not
an exact timestamp for every value. The guard,
`l6_1_10_kernel_routines` or `l6max_1_10_kernel_routines`, identifies the checked
kernel routines of that model's 1.10 MAIN; it is not a whole-image hash.
Unsupported or changing layouts return an error. L6 has no screen, so its
`objects` list is empty. A guest may
continue running during reads, so values are observations rather than an atomic
snapshot. `all` returns a top-level `schema`, `kind`, `model` and `atomic`, plus
`heap`, `tasks`, `scheduler` and `objects` fields containing four separate
single-category snapshots. Each has its own timestamp. `all` reports an error
rather than silently omitting a failed category.

For example, with `jq` installed:

```sh
# Sort tasks by estimated percentage of stack still untouched.
cargo l6ctl --socket /tmp/l6-control.sock rtos tasks |
  jq '.data.tasks | sort_by(.stack_free_percent_estimate)'

# Find the largest currently allocated heap blocks, including their headers.
cargo l6ctl --socket /tmp/l6-control.sock rtos heap |
  jq '[.data.blocks[] | select(.allocated)] | sort_by(-.size_bytes) | .[:10]'
```

In automation, check the command's exit status before consuming stdout; when
using a shell pipeline, enable `pipefail` so an error is not hidden by `jq`.
An RTOS error is a JSON object with `schema: 1` and an `error` string on stderr;
the process exits with status 2. Unknown categories, unsupported firmware,
disconnected sockets and inconsistent samples are errors, not empty snapshots.

| Category | `data` fields |
| --- | --- |
| `heap` | `total_bytes`, `free_bytes`, `minimum_free_bytes`, `allocations`, `frees`, `free_block_count`, `largest_free_block_bytes`, `fragmentation_percent`, `block_sizes_include_headers`, and `blocks` with `address`, `size_bytes`, `allocated`, `owner`. |
| `tasks` | `stack_headroom_is_estimate` and `tasks` with `name`, `state`, `current`, `tcb_address`, `priority`, `base_priority`, `creation_number`, `mutexes_held`, `event_list_address`, `stack_base_address`, `stack_size_bytes`, `stack_free_bytes_estimate`, `stack_free_percent_estimate`. |
| `scheduler` | `task_count`, `running`, `suspension_depth`, `ticks`, `pending_ticks`, `current_tcb_address`, `yield_pending`, `tick_overflows`, `next_wake_tick`, `critical_nesting`. |
| `objects` | `complete` and `objects` with `name`, `type`, `address`, `count`, `capacity`, `send_waiters`, `receive_waiters`. |

Heap data includes block addresses as hexadecimal strings, sizes in bytes,
allocator counters and the percentage of free block bytes outside the largest
block. Block sizes include allocator headers; fragmentation is null when free
bytes are zero. Task data includes effective/base priority, task state, TCB,
event-list and stack addresses, creation number, held-mutex count and estimated
stack headroom. Task state labels are machine-readable and ambiguous waits stay
ambiguous. Scheduler data includes tick and suspension information. The Objects
response always has `complete: false`: currently it contains only the verified
notification semaphore, if present. It is not an inventory of all queues.
Task `state` is one of `current`, `ready`, `blocked`, `blocked_timed`,
`delayed_or_waiting`, `waiting_or_suspended`, or `other`; a null
`event_list_address` means no event-list link was validated in that sample.

Only the requested category is collected. Heap and Tasks read the full heap
arena and are heavier than Scheduler or Objects; scripts should avoid rapid
polling. Read-only diagnostics run independently of control requests, but
share the emulator's serialized QEMU management connection.

## MIDI sessions in Rust

`l6ctl::midi::MidiSession` connects to a native CoreMIDI or ALSA endpoint by
its full name, or to an in-process USB host and cable. Connect before sending
the input whose MIDI output you want to observe, then retain the session to
preserve its subscription and pending messages across operations.
`send_control_change` and `poll_control_change` use a typed control change
with zero-based MIDI channels; raw byte methods are also available. Native
MIDI sessions do not require an emulator control socket. Native receive is
intended for the emulator's endpoints: on CoreMIDI the control-change poller
accepts complete three-byte messages in one packet and is not a general MIDI
stream decoder. The direct USB
transport receives mixer-control replies on cable 1, including when it sends
on another cable. `release` disconnects subscriptions while preserving the
endpoint presence observer, when available. The native transport is also available from the CLI without a control socket:

```sh
cargo l6ctl midi listen "L6max Mixer Control Port (Emulator)" --timeout-ms 30000
cargo l6ctl midi observe "L6max Mixer Control Port (Emulator)" --wall-ms 600
cargo l6ctl midi send "L6max Mixer Control Port (Emulator)" 0 81 100
```

MIDI channels are zero-based. The listener writes JSON objects, one per line.
It flushes a `ready` event after subscribing, so a script can wait for that
event before stimulating the emulator; subsequent `control-change` events
include `channel`, `controller`, and `value`. The listener exits successfully after the first control change. If no message
arrives before the wall-clock timeout (default 30 seconds), it exits nonzero.
Errors are JSON on stderr with `event: "error"`, a `kind` (`timeout`,
`endpoint-unavailable`, `unsupported`, `transport`, `output`, or
`invalid-argument`), and a `message`. `send` emits a `sent` event after the native
backend accepts the message.

`observe` retains one native subscription for the entire requested wall-time
window (1–60000 ms), even after receiving messages. It emits `ready` after
subscribing, strict `control-change` events, and a `complete` event with the
count; an empty interval succeeds with count zero. A final queue drain catches
messages from the last polling gap. Since observations have no event timestamp,
the drain can include a message delivered just after the window. This command
does not expose direct USB tokens.

The native MIDI integration test requires an available CoreMIDI or ALSA
sequencer service and is explicitly ignored by the default test run. Run it
on a host with that service using:

```sh
cargo test --offline --locked -p l6ctl --test midi -- --ignored
```

The CLI listener reports strict Control Change messages. The Rust session also
exposes `poll_control_observation` for the existing MIDI-sync scenario's
legacy counting semantics: on ALSA it can synthesize control-shaped bytes for
other control events and can hide ALSA input errors. It is an observation of
the legacy client, not a raw MIDI byte stream.

For a MIDI-sync state response, use one retained native session:

```sh
cargo l6ctl midi sync "L6max Mixer Control Port (Emulator)" --count 94 --quiet-ms 500 --timeout-ms 30000
```

`sync` drains strict Control Changes until the declared wall-time quiet
interval, emits `ready`, sends channel-0 CC 127 with value 127, then emits one
JSON `control-change` per reply until the requested count. The timeout is a
wall-clock bound across both phases. A caller supplies the expected count;
this command does not infer L6 firmware's configurable channel or CC map.
A session must choose control observations or raw byte reception; attempting to
mix them returns an error. Sending bytes is independent of that choice.
On CoreMIDI both strict and legacy control polling now decode coalesced
messages, so legacy counting can observe CCs that it previously dropped.
On CoreMIDI the strict listener handles coalesced messages, running status,
and messages fragmented across delivered byte chunks. CoreMIDI may itself
omit bytes sent as incomplete independent packets; omitted bytes cannot be
recovered by the decoder.

## Knob Dialog observation

`cargo l6ctl --socket /tmp/l6-control.sock knob-popup [--text]` reads the
L6max 1.10 recorder window, four-byte dialog state, callback slot 5 and
channel-1 level. `--text` also reads the bounded UTF-16 GUI resource 179.
The response reports the GUI-routine fingerprint guard and `atomic: false`:
the guest can change these values between QMP reads. Unsupported models,
GUI revisions and out-of-range text pointers return an error.

`l6ctl --socket PATH recorder-state` reads the revision-guarded L6max 1.10
recorder window and notification flags. This non-atomic observation does not
include display pixels and does not by itself prove that the screen rendered.

The first-use knob guard uses an in-process snapshot while its test owns the
paused guest. The CLI does not offer an atomic version: `knob-popup` and
`recorder-state` are non-atomic observations and do not acquire pause ownership.

`l6ctl --socket PATH midi-config` reads the L6 1.10 MIDI channel and its 54
raw CC mappings. Entries retain their order and duplicates; `255` means
unmapped. The channel and map are separate live reads and are not atomic.

The host checks the stock MIDI channel and mapped-CC sender routines before
reading; other models and revisions are rejected, as are patches that alter
either guard routine. The command requires a running guest with its MAIN
firmware loaded.

The production guards have been checked against the unpacked L6 1.10 MAIN
SHA-256 `827c7c5cb2a43cc3a011d4556462b8cb9539242f817ebeed7727d66a36c03799`
and a midi-sync-patched MAIN SHA-256
`57b716759a62be814cc0df4c8b38df1a08985545584f4bfad2420d34895addee`.
The stock image was also verified against a live headless guest through the
control socket. A focused patched L6 emulator case verified all 54 configured
CC replies. The first static check used the wrong image base and was corrected
after a live L6 observation exposed the error. To repeat the static check with an unpacked
MAIN, set `L6_MIDI_TEST_MAIN` to its path and run
`cargo test --offline --locked -p l6max-host compatible_firmware_passes_production_guards -- --ignored`.

`l6ctl --socket PATH mixer-level` samples channel 1 on L6 or L6max. It checks
the complete model-specific 1.10 mixer getter before reading its level byte.
The getter match is not a whole-image identity, and the live snapshot is not
atomic with its guard read.


`l6ctl --socket PATH knob-parameter-state` reads the L6max 1.10 parameter
selector and callback slots 0, 2 and 5. The sample is non-atomic.

### L6max update state

`l6ctl --socket PATH update-state` reads the L6max 1.10 startup mode and
SD status flags as raw `mode` (u32) and `flags` (u8). It checks fingerprints
of the inspected 1.10 startup, update and SD routines before reading; these
fingerprints do not identify the whole image or rule out unrelated patches.
The two values are separate live reads (`atomic: false`). The equivalent Rust
methods are `ControlClient::update_state` and `OwnedControl::update_state`.
Mode 1 and flags bits 0 and 3 clear are the existing rejection check; this
observation alone does not prove that a particular package was rejected.

`l6ctl --socket PATH update-gui-state` reads the raw 32-bit current-window
pointer and update-progress descriptor flags in L6max 1.10. The equivalent
Rust methods are `ControlClient::update_gui_state` and
`OwnedControl::update_gui_state`. The separate reads are non-atomic, and
revision fingerprints identify inspected startup and confirmation routines,
not the entire image or unrelated patches. The raw flag bit 0x100 is used by
the update preparation scenario; pixels are still needed to verify rendering.

### Replacement installer outcome

`l6ctl --socket PATH installer-outcome` reports the result of the emulator's
replacement installer for this engine start: `installed`, `no-pending`,
`failed` (with the error), or `previous-failure`. If the replacement
installer was not enabled, the command reports an unavailable error. This is
emulator behavior, not an observation of the hardware boot ROM. The matching
Rust methods are `ControlClient::installer_outcome` and
`OwnedControl::installer_outcome`; a new engine start has its own result.

`cargo l6ctl --socket PATH memory-read ADDRESS LENGTH` reads 1–4096 bytes of MAIN SDRAM (0x80000000–0x83ffffff) as non-atomic raw data. It does not select a firmware revision or interpret the bytes.

### USB controller

`l6ctl --socket PATH usb attach` attaches the emulated raw USB cable; it
does not start the native MIDI bridge or wait for the guest. It reserves the USB host until `usb release` or engine shutdown, and fails while
the native MIDI bridge or a direct MIDI session owns the USB host. A raw
reservation prevents the bridge from starting; release detaches the cable.
The guard coordinates these APIs and direct MIDI sessions; legacy in-process
callers using `UsbHost` directly bypass it and must coordinate separately. `l6ctl --socket PATH usb controller`
probes whether an attached guest controller answers tokens and prints
`running`; this is not USB enumeration or MIDI readiness. Both operations
support the L6 and L6max emulator models. Their Rust equivalents are
`ControlClient::usb_attach`, `ControlClient::usb_release`, `ControlClient::usb_controller`, and the same
methods on `OwnedControl`.

## Scoped MAIN debugger

A local emulator can opt into the QEMU MAIN GDB stub as well as the control
socket. `l6ctl debug-session` keeps one control connection open, pauses MAIN,
and accepts one JSON command per line, returning one JSON response per line:

```sh
printf '%s\n' '{"command":"register","index":15}' '{"command":"close"}' |
  cargo l6ctl --socket /tmp/l6-control.sock debug-session
```

Start the emulator with both `--control-socket /tmp/l6-control.sock` and
`--gdb-socket /tmp/l6-main-gdb.sock`. MAIN stays paused between operations;
the panel continues independently. Commands are `register` (indices 0–15),
`read` (`address`, `length` up to 4096 bytes in bounded MAIN SDRAM),
`add-breakpoint` and `remove-breakpoint` (aligned MAIN SDRAM `address`),
`continue` (`timeout_ms` from 1 to 60000, with an owned breakpoint), `step`,
`l6-scratch-read`, `l6-scratch-write` (exactly eight `bytes`), and `close`.
`l6max-signal-write` accepts twelve finite `values` between 0 and
40,000,000 and `timeout_ms`; it checks the L6max 1.10 SIGNAL snapshot
routine before writing the channel accumulators. These values are transient
firmware state and are not restored. In-process callers can use
`OwnedControl::debug_session_before` and `signal_write_before` with one
absolute deadline, then `close_signal_before`. If bounded cleanup cannot
verify MAIN is stopped, it reports that uncertainty, retains the lease and
requires a guest restart. A failed cleanup may spend up to three additional
seconds attempting an emergency stop.

For in-process L6 recorder diagnostics, `l6_recorder_reader_before` checks
three immutable L6 1.10 code spans and provides typed event, selection and
SAI DMA observations. The caller must keep MAIN stopped, supply an absolute
deadline and cancellation check, and recreate the reader after guest restart
or firmware changes. Pointer reads require an entire non-wrapping span in a
mapped MAIN RAM region; observations are separate reads and are not atomic.
The reader and bounded recorder breakpoint methods are not socket or CLI APIs.

`l6max_recorder_reader_before` likewise guards six L6max 1.10 code spans
and reads the event, current-window callback target and recorder DMA source
while MAIN is stopped. It checks entire pointer spans and rejects wrapping or
unmapped reads; these reads are non-atomic. The caller owns the deadline and
cancellation and must recreate the reader after restarting or changing firmware.
This reader and bounded single-step/optional-stop helpers are in-process APIs.

`OwnedControl::pause_all_before` creates an in-process scope that pauses both
QEMU processors under one wall deadline and cancellation check. Explicitly
close it to restore their previous run states and inspect errors. A failed
restore is reported as an uncertain state requiring a guest restart; dropping
the scope makes a bounded best-effort restore. It is not exposed on the CLI.

`OwnedControl::usb_storage_before` acquires exclusive in-process USB ownership
and creates a mass-storage session with an absolute wall deadline and panel
guest-time budget. Enumeration, BOT commands and FAT sector I/O use the same
budget; failed partial wire transfers close the transport. Call `abort` for
bounded best-effort file cleanup after failure. This session is not exposed on
the control socket or CLI.

The L6 scratch operations verify the supported L6 1.10 MIDI code before using
the legacy emulator probe page at `0x841ff000`. This is a scoped diagnostic
buffer, not evidence that physical firmware reserves that address.

The L6 1.10 MIDI hook probe additionally provides `l6-midi-inject`
(`expected_hook`, a firmware or SD `binding`, `timeout_ms`, and explicit
`leave_stopped: true`),
`l6-midi-sender` (`timeout_ms`), and `l6-midi-finish` (`timeout_ms`). The
caller must first own breakpoints at the idle routine, MIDI sender, and the
hook target verified from the matching patch report. Injection checks the
supported firmware routines, installed hook branch, and for SD the registry and
module entry; it requires the idle task in thread mode. The
sender command returns a bounded three-byte observation, and finish requires
the observed sender to have returned. The caller remains responsible for
checking the hook and interpreting the ordered MIDI output.

For a firmware patch the binding is `{"kind":"firmware"}`. For an SD patch
it is `{"kind":"sd","dispatcher":ADDRESS,"site_index":INDEX,"entry_offset":OFFSET}`;
derive those values from the matching loader report and module, and verify
the staged fixture identities. All addresses and offsets are JSON numbers.
`l6-midi-resolve-sd` takes those three fields and `timeout_ms`, and returns
the current relocated `hook` after checking the installed branch and registry.
Injection checks the binding again before writing guest state.
`l6-midi-hook` observes the hook PC and seven event bytes. `l6-midi-step` and
`l6-midi-breakpoint` (`address`, `enabled`, `timeout_ms`) provide bounded
stepping and owned breakpoint changes during the injected transaction.

Injection changes CPU execution state that this API cannot fully restore.
Finish restores scratch and removes breakpoints but deliberately leaves MAIN
stopped, closes the debug session, and reserves the debugger until the engine
is destroyed. Failed or cancelled injection also attempts to stop MAIN and
requires a restart; a failed stop leaves its state unknown. Use this probe on a
disposable emulator, and explicitly shut it down after collecting results.

The session owns at most four breakpoints. Closing or disconnecting restores
the scratch bytes, removes owned breakpoints, and restores MAIN's prior run
state. A socket session expires after 300 seconds. A disconnect during
`continue` interrupts the target before cleanup. If the debugger stream is
desynchronized or cleanup cannot be verified, the host attempts to stop MAIN,
marks its debugger unusable, and reports that the guest must be restarted.
It does not terminate the emulator. If the stop itself fails, the error says
so: MAIN's state is then unknown. The existing GDB transport uses an advisory
cross-process lease shared by repository clients; third-party clients that
connect directly to the QEMU stub do not honor that lease. The lease cannot
prevent external QMP clients from changing run state, and cleanup cannot run
if the host process itself is killed. In either situation, inspect or restart
the disposable guest before relying on its state.

Rust callers use `OwnedControl::debug_session()` for an owned engine or
`ControlClient::debug_session()` for the persistent socket. Both accept
`DebugCommand` and return `DebugObservation`. A caller should explicitly send
`Close` and check its result; dropping a session also attempts cleanup.

`l6ctl effect-editor REPORT MODULE` inspects a running L6max effect editor using a composed SD build report and its matching module file. The response reports a non-atomic mailbox/counter snapshot and the supplied fixture module hash; it checks the loaded module's bounds and a hash of immutable module code with relocation and continuation words masked; writable state is not hashed. This read-only operation is also available to owned guests through `EffectEditorBinding` and `effect_editor_reader_before`.

The in-process `OwnedControl` API offers USB SysEx send and poll against an already enumerated endpoint. The caller owns its connection, receive queue and deadlines. These operations are not exposed through the control socket or CLI.

`l6ctl panel-key CODE` reads the L6max 1.10 sound-pad key lookup and six state words for codes 25–28. The revision guard is checked before reading. The table and state reads are non-atomic; the returned input state is not evidence of an OLED rendering.

`l6ctl midi send-bytes ENDPOINT HEX_BYTE...` sends raw MIDI to a native endpoint; bytes are two hexadecimal digits. In-process callers can also reserve USB with `l6ctl::midi::RawUsbReceiver`, enumerate it with their own timeout policy, and poll complete USB transfers with cable and packet boundaries preserved. The raw USB receiver is unavailable through the control socket.
