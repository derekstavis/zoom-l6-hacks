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

## FreeRTOS snapshots

`l6ctl rtos heap|tasks|scheduler|objects|all` reads the running guest through
the same guarded decoder as the Debug window. The emulator must be started
with `--control-socket`; no firmware code runs and no guest heap is allocated.
The client prints one compact JSON object on stdout; `--pretty` formats it for
reading. Errors are JSON on stderr and exit nonzero.

Each single-category snapshot has `schema: 1`, `kind`, `model`,
`panel_guest_ms`, `atomic: false`, `revision_guard`, and a category-specific
`data` object. `panel_guest_ms` is the panel clock sampled after the read, not
an exact timestamp for every value. The guard
identifies checked L6max 1.10 kernel routines; it is not a whole-image hash.
Other models and unsupported or changing layouts return an error. A guest may
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
