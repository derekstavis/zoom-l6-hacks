# All-catalog failure investigation

On 2026-11-24, the all-catalog run at `b7dd8589` recorded 326 PASS and 46
FAIL (`/private/tmp/l6-cache-all-cases/report.json`). It used the run-local
cache. The 46 failures fell into three groups:

- Forty-four generated combinations installed menu-loader, dump-rom and
  midi-sync. The dump's final file appeared before its completion notification
  cleared, so the following Down press could dismiss the notification instead
  of selecting Send MIDI State. The menu-triggered MIDI check then waited for
  94 replies and received zero or one. The direct-request MIDI check had
  completed earlier. The generator now waits for the notification to clear and
  checks the reviewed selected-menu image before invoking MIDI.
- `l6-usb-startup` started both emulated processors paused but resumed only
  MAIN, leaving panel-derived guest time at zero. Resuming both exposed a
  second issue: the input sweep included unsupported panel targets 31–46.
  The probe now sweeps the supported L6 panel targets; its original three
  breakpoint and controller-write assertions remain.
- `l6-level-meter-pwm` repeatedly continued from the threshold-builder entry
  breakpoint without stepping past it. The panel ran, but the injected data
  was never processed by the builder. Stepping past the breakpoint allowed
  the probe to observe increasing meter records and sampled panel levels.

The four-worker rerun of exactly the original 46 failures passed all 46
(`/private/tmp/l6-original-46-rerun/report.json`). Every recorded firmware,
state, card, audio and QEMU identity matched its original case; the rebuilt
runner differed, as expected. The original failures remain failures: this
rerun does not claim the original all-catalog run passed, nor does it replace
a fresh all-catalog qualification on the final stack tip. The rerun's driver
SHA-256 was `719faf936b02e2aad2516f6072baedf78fc51ce2e93fc8ecfe0fbd8ebe75b140`;
the externally supplied QEMU was source-unattested and had SHA-256
`d33f3853e30115234b1deb9c4b727dfed13a234fcdece2fa18ffe05e7d692a62`.

Focused evidence also includes four previously failing patch combinations
(`/private/tmp/l6-dump-midi-reviewed-parallel/report.json`), a dump-only
control (`/private/tmp/l6-dump-only-ready/report.json`), the USB probe
(`/private/tmp/l6-usb-supported-targets-probe/report.json`), and the L6
injection and tone-driven meter checks together with the L6max signal-meter
check (`/private/tmp/l6-meter-step-over-parallel/report.json`). The Rust
workspace tests passed; one firmware-dependent fixture unit test was ignored
without its explicit environment variables.
