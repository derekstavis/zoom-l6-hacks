# Current development status

Use [`l6test`](testing.md) for repeatable emulator scenarios and
[`l6dbg`](diagnostics.md) for focused diagnostics. Results apply to the
firmware, QEMU binary, fixture, and backend used in each run; inspect the
artifacts for the configuration and result.

## Panel and audio

The L6 and L6max MASTER meters use model-specific panel outputs. The stereo
sound-pad scenarios check that each meter responds independently at multiple
levels, using a saved one-shot assignment and no USB setup. Encoder rings use
sampled PWM duty, normalized to their observed full-scale endpoints. The panel
sampler integrates pin activity over the last brightness cycle of the LED scan
(32 frames) and sends changed levels every 5 ms of guest time.

The L6 and L6max green SIGNAL indicators have a graded PWM path, and the GUI
shows them at their sampled brightness; red SIGNAL is a binary clip state. See
[controls and indicators](../emulator/controls.md#indicator-matrix). Sound-pad
and TAP lamps can blink; scan-window samples of a binary lamp do not establish
a separate brightness control. Physical LED brightness, meter calibration,
analog audio behavior, and physical jack routing remain unverified.

## Known limits

- The full L6 panel scenario and exact encoder reversal need further
  qualification on both headless and GPUI backends.
- L6 DIN-to-USB forwarding and USB startup scenarios need further qualification.
- L6 scene-save GUI coverage and startup-key parity need further validation.
- Capella Linux qualification on 2026-11-26 completed the `stack` suite with
  314 PASS and 2 BLOCKED when no app bundle was supplied. The two packaged-app
  cases were then run against the Linux AppDir: `gui-bundle` failed because
  the L6 window aspect ratio was 2.589 instead of 2.027, and `gui-menu`
  requires macOS. A macOS packaged-app run is still needed to qualify the menu;
  the Linux bundle sizing failure remains unresolved. These runs pinned QEMU
  SHA-256 `73f770d3129dbd50e8d352a0f6c33f4c8d242d96a5afabe3c3d2805a9fb3b7b4`
  (external binary; build source unattested).
- Sound-pad playback and MASTER meter response are exercised for both models
  using saved assignments. The waveform cases compare SAI output with the
  source WAV. Cards must meet the firmware's
  [layout requirements](../emulator/usb.md#card-layout). The Editor-assigned
  pad scenarios need further qualification. Recorder playback requires prepared media and state.
  Input channels play assigned files into the mix and record to their tracks on both models; there is no live input.
- L6max sound-pad key-bit assertions and DIN/Editor scenarios need live
  qualification. Physical control wiring is not established by emulator tests.

## Test setup and execution

Each [case file](testing.md#case-files) under `tests/emulator/cases/` or a
patch's `tests/emulator/cases/` specifies its models, backends, and suites. Each scenario declares its state, card, and patches, and the local
configuration supplies the firmware packages. A state spec creates disposable settings
and panel state without running setup interactions, and a card spec builds the case's
SD card with its pad assignments. Scenario steps marked `setup_input` are skipped when
a state spec supplies that setup. See the
[testing workflow](testing.md) for configuration, permissions, suites, and reports.
The driver records the binary hash; an externally supplied binary's build
source is not attested by that hash.
