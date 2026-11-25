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
`info`, `input_status`, `tap`, `down`, `up`, `encoder`, `analog`, and `set_analog` return
typed responses. Encoder and analog channels use the same one-based numbering
as the CLI. Socket input receipts indicate that the request was enqueued,
not that the guest applied it. `request` and `RtosKind` expose the existing
versioned RTOS JSON until its socket responses have shared typed schemas.

Code that already owns an `l6max_host::engine::Engine` can borrow it with
`OwnedControl::new(&engine)`. Its `info`, `input_status` and `rtos_*` methods use the host's
existing snapshots directly; it does not create a second guest or open a
control socket. RTOS decoding and model/revision guards remain in
`l6max-host`, and the two transports do not yet expose identical capabilities.

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
