# Test readiness experiments

The readiness branch uses positive subsystem signals in place of selected
fixed waits. Recording and negative-observation durations remain part of the
scenarios. The initial Editor experiments, including prepared and fresh state,
are documented in [Editor readiness](editor-readiness-investigation.md).

## Combined sample

On 2026-11-23, a sequential headless comparison used main `ffb935b8` for the
baseline and the readiness branch based on `3af7b54c` with uncommitted changes
for the first after run. Each run used one guest at a time, no retries, and the
same QEMU and per-case firmware, state, SD-card and audio-input identities.
QEMU was the externally supplied, source-unattested executable with SHA-256
`d33f3853e30115234b1deb9c4b727dfed13a234fcdece2fa18ffe05e7d692a62`.
The L6 and L6max firmware package hashes were respectively
`64f1f36b8383176b5d841911fbf3c16205139a313ee88be906f313b1a98a33fb` and
`4e20f92c9c5131ed082c0feec2b2d7d0445fb576296f11f6dace56a4fc0c9b47`.
The baseline and after runners necessarily differed: their hashes were
`2655bd4398f31ebd74e0981df539424edcfbf5692943918e602e48b8c7612e49` and
`929379b27fc1ca78b86f559469371a160280c3c8391bb48a2898e008fb3050c5`.

The 15-case baseline passed. The first after trial passed 13 and failed two;
it is **not** a passing suite. The 13 passing cases took a total of 815.1 s
before and 547.2 s after, summing case wall durations. This is a small single
run, not a full-suite speedup claim. Representative cases:

| Case | Before | Trial | Result |
| --- | ---: | ---: | --- |
| Editor pad assignment, L6 | 102.2 s | 21.2 s | Pass |
| Editor pad assignment, L6max | 106.0 s | 26.5 s | Pass |
| Editor pad playback, L6 | 68.1 s | 26.8 s | Pass |
| UI pad assignment, L6max | 227.2 s | 187.1 s | Pass |
| Storage removal and insertion, L6max | 26.7 s | 15.4 s | Pass against existing display references |
| SD MIDI patch, L6 | 22.5 s | 14.6 s | Pass |

The subsequent Editor and menu changes were tested together in a four-case,
no-retry run with the same QEMU and per-case firmware, card and state hashes.
The runner hash was
`38724436d825ef18599d9e2781f06b3747d9a31215fb127fbee29f790730c772`;
the worktree contained the uncommitted menu and Editor changes. All four
passed: L6 Editor assignment 17.3 s, L6max Editor assignment 18.6 s, L6
Editor playback 24.9 s, and L6max UI assignment 31.3 s. The corresponding
baseline times were 102.2, 106.0, 68.1 and 227.2 s. The UI menu operations
themselves used a total of 14,775 ms of guest time, compared with 174,975 ms
in a separate baseline capture with the original button waits. Each
transition now checks reviewed pixels under `tests/smoke/images/`, then the
existing Editor state query verifies the assignment. Complete and matched
single-response Editor exchanges can finish early; dialog-bearing and other
multi-message exchanges still use a collection window. The focused reports
are `/private/tmp/l6-menu-readiness-probe/report.json` and
`/private/tmp/l6-editor-readiness-final/report.json` on the testing machine.

The same four cases passed again after expressing the menu as normal `press`
and `expect-display-image` scenario steps: 14.9, 19.0, 20.4 and 26.4 s,
respectively. This run used the same QEMU and fixture identities and runner
`f03eaf810427618cd9567e4f78ca191efa177022bcedecb9eec34b42a02f4004`;
its report is `/private/tmp/l6-declarative-menu-trial/report.json`. The
screens and cursor positions were captured with the previous waits still in
place and reviewed before adding the references. The home screen must stay
matched for 500 ms of guest time before opening the menu. Each subsequent
input completes only after the expected rendered screen appears. The
existing Editor readback still checks the selected assignment.

Fresh-state Editor setup was then exercised on both models. The initial
probe waited for boot readiness, captured L6max's Date/Time and Battery Type
screens, and passed the Editor assignment checks. A permanent fresh-state
case using those reviewed screens passed on L6 in 16.7 s and L6max in 23.4 s;
the prepared controls passed in 13.8 s and 16.3 s. Earlier fresh-state checks
with the 40-second setup wait took 60.6 s and 65.4 s. Reports are in
`/private/tmp/l6-editor-fresh-boot-probe/` and
`/private/tmp/l6-editor-fresh-ready-trial/` on the testing machine.

The initial combined 17-case sample passed 16 and failed the fresh L6max DIN
case: its USB identity request received no reply while the Battery Type
confirmation popup was still displayed. The first-boot sequence now waits for
the reviewed recorder-home image after that confirmation. A separate retry
passed fresh DIN, fresh Editor and prepared DIN (`/private/tmp/l6-din-home-trial/`).
The earlier failure remains a failure of that implementation; it is not
silently counted as a clean sample pass.

The failing L6max SD MIDI case did not deliver the expected encoder CC. The
L6max SD Knob Dialog/effect case reached the check before the recorder window
was ready. Both ran without the matrix's eight-second delay, even though the
SD loader had reported a completed claim. Loader completion is therefore not a
readiness signal for the MIDI path or recorder UI. The delay is retained for
L6max at that point; the failure must not be relabeled a flake. With the
delay restored, a focused no-retry rerun passed both L6max cases
(22.0 s for SD MIDI and 25.6 s for Knob Dialog/effect), as well as the L6 SD
MIDI case (15.5 s). Its report is
`/private/tmp/l6-wait-patch-confirm/report.json` on the testing machine.

A later trial replaced the L6max delay with `expect-display-image` for the
reviewed recorder-home screen. Nine no-retry cases passed, covering firmware
and SD MIDI, firmware and SD Knob Dialog, an SD effect, menu-loader, and
installed and hosted SD delivery. The two cases that failed with loader-only
readiness passed with the screen assertion. For SD MIDI, the next patch check
started at approximately 4,473 ms of guest time versus 10,769 ms in the
original baseline. The trial runner hash was
`c7f4918a8bc4a215f23ac7dfd8415a269ab65867e2464dff568e8c76b08d7316`;
the full report and fixture identities are in
`/private/tmp/l6-patch-screen-trial/report.json` on the testing machine.
This is a focused sample, not proof for every generated combination.
The incorporated change then passed eight additional cases covering both
models, mixed firmware and SD patches, dual effects, menu-loader and delivery
paths (`/private/tmp/l6-patch-readiness-combinations/report.json`). Four
further menu-loader cases passed after replacing the pre-menu sleeps with a
stable home screen and the rendered main-menu screen, and removing the delay
directly before the existing Patches-screen assertion
(`/private/tmp/l6-patch-menu-readiness/report.json`). These are focused tests;
the full matrix still needs qualification.

The remaining navigation to System and Patches was then changed to wait for
reviewed main-menu and System-menu images. System has eight, nine or ten items
in the generated matrix; each has its own reference because the scrollbar
changes. Four representative combinations passed, including the simple menu,
an added MIDI item, mixed SD patches and effects, and a ten-item composition.
Their report is `/private/tmp/l6-patch-menu-views/report.json`. These checks
remove the fixed delays along the path into and back from the Patches screen
while preserving the existing Patches rows and menu-action assertions.

The positive SD patch assertion now waits for the loader's completed claim
before retaining its existing module and hook checks. An absence assertion
remains immediate, and scenarios must retain any observation period needed to
establish absence. A separate positive-loading probe passed the all-patches
recording/restart, SD MIDI and dynamic hook-chain cases without their fixed
eight-second waits (`/private/tmp/l6-sd-positive-readiness-probe/report.json`).
The panel-key map polls the model-specific key bit after
input acknowledgement. The L6max storage case waits for the existing reviewed
card-removed and recorder-home display images; L6, which has no display, keeps
its delays. A subsequent confirmation with explicit display-change guards
passed both models: 27.4 s for L6 and 15.9 s for L6max. Its report is
`/private/tmp/l6-wait-storage-confirm/report.json` on the testing machine.

DIN Mixer setup now shares the model-specific first-boot sequence and skips
its former 40-second delay for prepared state. Knob Dialog's callers complete
setup first, and its helper waits for a quiet recorder window rather than
repeating the first-boot prompts and 40-second wait. A probe passed the
all-patches, SD MIDI and static-restart cases; its remaining three cases were
intentionally stopped and are BLOCKED, not passes. The combined sample then
passed all-patches, static restart, standalone Knob Dialog and dynamic SD
Knob Dialog with these changes. Their reports are
`/private/tmp/l6-knob-ready-trial/report.json` and
`/private/tmp/l6-readiness-representative/report.json`.

On the testing machine, complete reports and per-case fixture hashes are in
`/private/tmp/l6-wait-comparison-before/report.json`,
`/private/tmp/l6-wait-supplement-before/report.json`, and
`/private/tmp/l6-wait-comparison-after/report.json`. These local paths are
run artifacts, not committed fixtures. Re-run the affected scenarios and full
qualification after changes; the sample alone does not establish correctness
under every host load or on physical devices.

## L6max updater confirmation

An instrumented `firmware-update` run retained the original 40-second
guest-time wait and all update assertions. The update-dialog window pointer
first matched at 5,378 ms, but the captured screen still showed SD-card
access. A second run sampled the reviewed confirmation image and saw it
at 5,477 ms of guest time. The pointer alone is therefore insufficient to
drive the confirmation button. The reviewed image is
`tests/smoke/images/l6max-firmware-update-dialog.png`.

The valid-update path now uses the regular `expect-display-image` scenario
assertion in its separately booted guest, then preserves the mode, update
flags, installation, panel flash, protected NOR and clean-restart checks.
The corrupt-package path still observes its original window. A focused run
passed the stock L6max update and the patched Knob Dialog update; its report
is `/private/tmp/l6-update-ready-retry/report.json`. The preceding attempt
was blocked by a full disk and could not write a report; the successful run
is not evidence that attempt passed. The updater probes are in
`/private/tmp/l6-update-ready-probe/` and
`/private/tmp/l6-update-screen-probe/`. These runs used the same external
QEMU hash listed above. Because host load varied, their case wall times do
not establish an update-suite speedup; the observed difference is in the
specific guest-time wait.

For the post-install wait, an instrumented stock-update run observed mode 4,
the expected persisted panel image, all 64 writes and the panel-image
verification marker at about 2,500 ms of panel guest time. A patched-update
probe observed mode 4, the expected panel image and a running-panel response
at about 1,023 ms; its later restart and Knob Dialog checks passed. The test
now waits for those positive signals, retaining the 64-write requirement for
the synthetic stock update and all subsequent assertions. The corresponding
probe reports are `/private/tmp/l6-update-install-probe/report.json` and
`/private/tmp/l6-patched-install-probe/report.json`. The combined sample's
stock and patched update cases passed with the new checks; the L6 update is
unchanged. The 40-second clean-restart observation remains because the test
uses that interval to look for unintended panel programming.

A subsequent focused run covering fresh Editor setup on both models, fresh L6
panel input, fresh L6max DIN setup, the L6max patch heap, positive SD patch
loading and hook/duplicate/older-module cases completed with 12 PASS. Its
report is `/private/tmp/l6-readiness-focused-next/report.json`; it is not a
full-suite result. Test-defined recording and playback observation durations
are now labelled separately from readiness delays, and remain guest-time
intervals rather than signals that recording or playback started.

The first combined post-update setup trial (`/private/tmp/l6-readiness-update-final`)
passed the stock update but failed the patched Knob Dialog case: its restart has
no SD card and the card-present recorder reference differed by 200 pixels. The
MIDI update case was interrupted before completing. The post-update setup now
checks the reviewed no-card recorder screen; the prompt sequence itself remains
shared with the regular first-boot setup. A corrected focused run is required
before claiming the patched paths pass.
The corrected focused run passed all five cases, including both patched update
paths and their post-update behavior checks; see
`/private/tmp/l6-readiness-corrected/report.json`.

A separate effect-slot probe replaced the generated one-second per-slot delay
with a bounded wait for the selected slot and processor; replacements also
have to produce a new nonzero output count after the check begins. The final
state assertion still runs. Room, Spring and a dual-replacement case passed in
`/private/tmp/l6-effect-slot-trial/report.json`. The integrated focused run then passed all seven cases, including the three
effect combinations, fresh setup and recordings on both models; see
`/private/tmp/l6-readiness-effect-audio/report.json`.
