# `l6ctl`

`l6ctl` sends input commands to an already running emulator. Start the emulator
with an explicit socket path:

```sh
cargo run -p l6max-gui -- --control-socket /tmp/l6-control.sock
```

Then run commands in another terminal:

```sh
cargo run -p l6ctl -- --socket /tmp/l6-control.sock info
cargo run -p l6ctl -- --socket /tmp/l6-control.sock button channel.1.mute
cargo run -p l6ctl -- --socket /tmp/l6-control.sock down play 500
cargo run -p l6ctl -- --socket /tmp/l6-control.sock up play
cargo run -p l6ctl -- --socket /tmp/l6-control.sock encoder 1 -2
cargo run -p l6ctl -- --socket /tmp/l6-control.sock analog 1 3
cargo run -p l6ctl -- --socket /tmp/l6-control.sock set-analog 1 512
cargo run -p l6ctl -- --socket /tmp/l6-control.sock rtos tasks --pretty
```

`--socket PATH` can be omitted when `L6_CONTROL_SOCKET` is set. The socket is
created with current-user-only permissions and is removed when the emulator
exits. `info` reports model, guest time, power state, and indicator snapshots.

`button` (alias `tap`) presses and releases after 50 ms by default; use
`--hold-ms MS` to change it. `down NAME|TARGET DURATION_MS` presses for a
bounded duration, while `up NAME|TARGET` releases it. Names are semantic names
from the shared host control map (for example `channel.1.mute`, `mode.level`,
or `recorder.menu`); numeric GPIO targets `0..53` are also accepted.
`encoder CHANNEL STEPS` uses one-based channels and signed detents.
`analog CHANNEL STEPS` moves one of five knobs by signed steps; `set-analog`
sets a knob position from `0..1023`.

Use `cargo run -p l6ctl -- --help` for the syntax summary. The endpoint is
opt-in: an emulator started without `--control-socket` exposes no control
socket.

## FreeRTOS snapshots

`l6ctl rtos heap|tasks|scheduler|objects|all` reads the running guest through
the same guarded decoder as the Debug window. The emulator must be started
with `--control-socket`; no firmware code runs and no guest heap is allocated.
The client prints one compact JSON object on stdout; `--pretty` formats it for
reading. Errors are JSON on stderr and exit nonzero.

Each snapshot has `schema: 1`, `kind`, `model`, `panel_guest_ms`, `atomic:
false`, `revision_guard`, and a category-specific `data` object. The guard
identifies checked L6max 1.10 kernel routines; it is not a whole-image hash.
Other models and unsupported or changing layouts return an error. A guest may
continue running during reads, so values are observations rather than an atomic
snapshot. `all` performs four sequential snapshots, each with its own timestamp;
it reports an error rather than silently omitting a failed category.

Heap data includes block addresses as hexadecimal strings, sizes in bytes,
allocator counters and the percentage of free block bytes outside the largest
block. Block sizes include allocator headers; fragmentation is null when free
bytes are zero. Task data includes effective/base priority, task state, TCB,
event-list and stack addresses, creation number, held-mutex count and estimated
stack headroom. Task state labels are machine-readable and ambiguous waits stay
ambiguous. Scheduler data includes tick and suspension information. The Objects
response always has `complete: false`: currently it contains only the verified
notification semaphore, if present. It is not an inventory of all queues.

Only the requested category is collected. Heap and Tasks read the full heap
arena and are heavier than Scheduler or Objects; scripts should avoid rapid
polling. Read-only diagnostics run independently of control requests, but
share the emulator's serialized QEMU management connection.
