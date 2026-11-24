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

## No-card MIDI startup

The prepared-state `l6-direct-midi-sync` and `l6max-direct-midi-sync` cases
previously waited 40 seconds of guest time before the unchanged MIDI and panel
assertions. A controlled fixture comparison in
`/private/tmp/l6-midi-startup-before/report.json` and
`/private/tmp/l6-midi-startup-probe-run/report.json` passed both models after
replacing that delay with `boot-ready`. The QEMU, patched firmware, package and
state identities match per model across the two reports. Total case wall times
were 102.2 and 108.3 seconds before, and 86.2 and 71.5 seconds after;
host load varied, so these numbers are observations rather than a controlled
performance benchmark.

Separate fresh-state probes in `/private/tmp/l6-midi-fresh-probe-run/report.json`
passed on both models with no card and the original MIDI checks. L6 waits for
boot readiness and retains the two-second interval between its confirmation
presses. L6max checks the reviewed Date/Time and Battery Type prompts, then
waits for the reviewed no-card recorder screen. Prepared-state cases skip the
prompt actions but still wait for boot readiness. The same setup is shared by
the no-card host MIDI, direct MIDI and installed-patch cases; the branch's
integrated run passed all nine cases; its report is
`/private/tmp/l6-midi-readiness-integrated-2/report.json`. An earlier
run was stopped during startup after the worktree changed and did not
produce a case result.

For hosted SD MIDI, a separate probe replaced the 40-second delay with boot
readiness and, on L6max, the reviewed recorder-home screen. Both models passed
(`/private/tmp/l6-hosted-sd-readiness-probe/report.json`), but neither signal
alone proves that the module has finished loading. A second probe additionally
waited for the loader's published successful claim, and both models passed
(`/private/tmp/l6-hosted-sd-claim-probe/report.json`). The reusable
`sd-patch-loaded` condition now uses that claim, with the existing bounded
10-guest-second, 180-wall-second, 10-ms polling policy. The case retains its
actual MIDI sync exchange to verify the expected module's behavior.

The integrated loader-claim run passed all five cases, covering prepared and
fresh hosted-SD MIDI on both models and the pre-existing SD patch assertion;
its report is `/private/tmp/l6-sd-claim-integrated/report.json`.

The L6 panel-event map uses boot readiness in place of its 42-guest-second
startup delay. The isolated probe preserved all 26 event assertions and
passed in 22.7 wall seconds; see `/private/tmp/l6-panel-event-ready-probe/report.json`.

The fresh L6 DIN-to-USB forwarding probe uses the same boot and prompt setup
instead of its 22-guest-second startup wait and five-second post-confirmation
wait. It retained the forwarding assertion and passed; see
`/private/tmp/l6-din-ready-probe/report.json`.
The integrated L6 panel-map and DIN-forwarding cases both passed; see
`/private/tmp/l6-panel-din-integrated/report.json`.

The factory setup probe first failed against the normal no-card recorder
screen: the actual display was the factory SD-card test prompt
(`/private/tmp/l6-factory-setup-probe/report.json`). A reviewed reference for
that prompt was then used as the completion signal, and the full factory
protocol case passed (`/private/tmp/l6-factory-card-probe/report.json`). The
40-guest-second startup chord hold and protocol assertions remain unchanged.
The first integrated factory attempt was blocked before running a case because
its build ran out of disk space. After cleaning rebuildable probe targets, the
integrated retry passed (`/private/tmp/l6-factory-ready-retry/report.json`).

A full-hold factory probe observed `system_mode` transition from 0 to 7 about
0.7 guest seconds after the new session started while the chord was held
(`/private/tmp/l6-factory-mode-transition/report.json`). An initial attempt
to release at that point failed because the button-down command still specified
a 41-second minimum hold; the requested release was not acknowledged
(`/private/tmp/l6-factory-early-release-probe/report.json`). A second probe
used the emulator's normal button minimum, released after mode 7 and input
acknowledgement, and passed the full protocol
(`/private/tmp/l6-factory-short-hold-probe/report.json`). The integrated case
now expresses this as `hold-chord` until `factory-mode`; a separate bounded
`duration_ms` remains available for genuinely timed chords. The condition is
restricted to supported stock L6max firmware and checks that factory mode was
not already active before the chord.
The integrated factory-mode condition passed the complete protocol case with a
990 ms guest-time chord hold; see
`/private/tmp/l6-factory-condition-integrated/report.json`. The earlier
integrated fixed-hold case took 101.2 wall seconds and the condition-based run
57.1 wall seconds, under variable host load; those are not a controlled
benchmark.

The first factory SysEx exchange previously observed 400 guest milliseconds
per request, including requests with a known single positive reply. An isolated
`complete_on_match` probe completed those requests after a matching complete
response, while retaining the full window for the no-reply request. The full
factory protocol passed; both the baseline and probe recorded 25 requests with
24 replies, followed by three requests with three replies. The first exchange
was 12.8 and 0.95 wall seconds respectively in these individual runs under
variable load. See `/private/tmp/l6-factory-condition-integrated/report.json`
and `/private/tmp/l6-factory-sysex-probe/report.json`. Rejected-prefix checks
and explicit reply windows retain their complete observation interval.
The integrated factory case passed both optimizations together in 36.4 wall
seconds with a 567 ms guest-time chord hold. Its two exchanges retained 25/24
and 3/3 request/reply counts; see
`/private/tmp/l6-factory-fast-integrated/report.json`.

The L6 saved-pad meter probe replaced a 16-guest-second startup delay with
boot readiness followed by a bounded wait for the exact active pad-1 UTF-16
file path. The existing play-mode, audio and independent meter checks passed
(`/private/tmp/l6-saved-pad-ready-probe/report.json`), and the matching path was
already present at the first read after boot readiness. The original case also
passed (`/private/tmp/l6-saved-pad-before/report.json`). Their QEMU, firmware
package and state identities match, but their generated SD image hashes differ
and host load varied, so the 115.2/19.1 wall-second observations are not a
controlled before/after performance comparison. The integrated condition also
requires the assignment-pending flag to be clear and is scoped to supported
stock L6 firmware.
The integrated saved-pad run passed all three cases, including CoreAudio,
waveform comparison and the meter check; see
`/private/tmp/l6-saved-pad-integrated/report.json`. The integrated meter case
and the original baseline have matching QEMU, firmware, package, card and state
hashes. Their runner binaries differ because the implementation changed; wall
load varied. The integrated meter case took 28.5 seconds versus 115.2 seconds
in the baseline, so those individual measurements should not be treated as a
controlled speed benchmark.

A first L6max saved-pad probe removed the five-guest-second post-mixer wait
and waited for the active Pad 4 path plus a clear assignment-pending flag.
The path and flag matched, but the playback and stereo meter assertions failed
with no audio (`/private/tmp/l6max-saved-pad-ready-probe/report.json`). Those
fields alone are therefore not sufficient evidence of L6max sampler readiness.
Adding a nonzero sampler file handle and configured buffer size did not fix
playback either (`/private/tmp/l6max-sampler-ready-probe/report.json`).

A later probe waited for existing boot readiness after mixer readiness. The
three audio cases passed (`/private/tmp/l6max-boot-only-probe/report.json`);
the probe had omitted the meter case’s original pad-path assertion, which was
restored and passed separately (`/private/tmp/l6max-boot-assertion-probe/report.json`).
The integrated run kept all original assertions and passed all three cases
(`/private/tmp/l6max-boot-ready-integrated/report.json`). It reused the pinned
runner from the L6 saved-pad integrated run with `--no-build`; the report records
the binary and fixture identities. These observations justify using the existing
boot condition in these scenarios, but do not establish an independent sampler
readiness signal. An earlier combined boot-and-sampler probe passed two cases
and reported the meter case failed before running out of disk while writing its
report (`/private/tmp/l6max-pad-boot-probe/`); it is not counted as a pass.

The GUI power case now waits for boot readiness before L6max power-off and
after power-on, replacing two 40-guest-second delays. The first attempt passed
L6 but failed L6max because the first-boot fragment was skipped for its
prepared state (`/private/tmp/l6-gui-power-readiness-gui/report.json`). An
explicit L6max boot condition then passed the L6max case
(`/private/tmp/l6-gui-power-ready-retry/report.json`). The first invocation
selected headless and ran no cases.

The volatile SD MIDI case now lets its existing positive SD module assertion
poll after reinsertion rather than waiting 20 guest seconds first; both models
passed (`/private/tmp/l6-sd-volatile-ready-retry/report.json`). The preceding
attempt failed both cases during fixture construction because the reused
runner lacked patch-building artifacts (`/private/tmp/l6-sd-volatile-reinsertion/report.json`).
The separate 40-second negative-observation delay remains.

The SD MIDI and menu-loader scenarios now wait for boot, the L6max recorder
home screen where applicable, and the SD loader claim in place of their
eight-guest-second startup delays. All four model/case combinations passed
(`/private/tmp/l6-sd-matrix-ready-next/report.json`).

Card recording on both models, ROM dumping and the knob-dialog menu passed
with boot/recorder-home readiness instead of an eight-guest-second delay
(`/private/tmp/l6-menu-card-ready-next/report.json`).

For negative SD module checks, the patch loader publishes debug stage `0x400`
after a scan loaded no modules and resets its claim to zero. The bounded
`sd-patch-empty` condition waits for both values; a missing registry alone is
not a completed scan. Both models passed absent and corrupt bundle checks
(`/private/tmp/l6-sd-empty-scan-probe/report.json`). Five further L6max cases
passed using the same condition with boot and recorder-home readiness
(`/private/tmp/l6-sd-empty-matrix/report.json`). This does not establish a
scan after a no-card restart, so the volatile restart observation remains.

Both audio-input recording cases and both SD warm-reset cases passed with
boot/recorder-home readiness replacing the eight-guest-second setup delay
(`/private/tmp/l6-record-warm-ready-retry/report.json`). The recording windows
remain unchanged. The initial invocation was blocked during scenario validation
because an L6-only case erroneously contained an L6max-only step; no case ran.

The GUI switch-during-playback scenario passed with boot readiness in place
of its five-guest-second startup delay (`/private/tmp/l6-gui-switch-ready/report.json`).
Its two eight-second playback observation windows remain unchanged.

All four audio-input and transport cases passed after their five-second setup
delay was replaced by boot readiness (`/private/tmp/l6-audio-input-boot-ready/report.json`).

Fresh USB setup now uses the model-specific first-boot sequence followed by
the existing USB descriptor condition instead of a fixed five-second wait.
Both models passed (`/private/tmp/l6-usb-first-boot-ready/report.json`).

An attempt to replace the shared effect-loader startup delay with boot
readiness passed three effect scenarios but failed replacement-map while
waiting for a patched mixer function to return
(`/private/tmp/l6-effect-boot-ready/report.json`). The original delay was
restored and replacement-map then passed (`/private/tmp/l6-effect-map-baseline/report.json`).
The failure is not silently counted as a pass; an effect-specific readiness
signal is still needed before removing the delay.

The direct L6 MIDI sync case passed using boot readiness and shared first-boot
setup rather than a fixed five-second delay (`/private/tmp/l6-direct-midi-ready-next/report.json`).

L6 recorder playback passed without its extra five-second delay after the
existing selected-file assertion (`/private/tmp/l6-recorder-playback-ready/report.json`).

L6 GUI power-on passed without the five-second delay before its existing
startup-report check (`/private/tmp/l6-gui-power-startup-report/report.json`).

The state-spec and L6max recorder playback cases passed without their extra
three-second delays (`/private/tmp/l6-state-recorder-ready-retry/report.json`);
the first invocation selected a nonexistent case ID and ran no cases.
The EQ update and L6 recorder lamp cases now use bounded memory matching
instead of fixed transition sleeps; all four focused cases passed
(`/private/tmp/l6-memory-condition/report.json`). Recording durations remain
explicit observation windows.

The volatile SD case was subsequently checked with the card physically absent
through restart: it waits for the new boot, checks the old module is absent,
then reinserts the card and checks it loads again. Both models passed without
the former 40-guest-second delay (`/private/tmp/l6-sd-volatile-boot-ready/report.json`).
That wait was followed by a single assertion, not continuous observation;
this result does not claim an uninterrupted 40-second absence observation.

The knob-dialog menu scenario passed with reviewed recorder/menu screens and
existing display comparisons replacing its menu transition delays
(`/private/tmp/l6-knob-menu-all-ready/report.json`). The menu checkpoint was
visually reviewed and matches the existing menu-project reference.

GUI power passed both models with one initial boot condition instead of fixed
initial sleeps (`/private/tmp/l6-gui-power-boot-first/report.json`). ROM dump
and SD menu-loader passed with reviewed System-menu screens replacing menu
transition sleeps (`/private/tmp/l6-patch-system-menu-ready/report.json` and
`/private/tmp/l6-dump-rom-menu-ready/report.json`). The new Dump ROM selection
reference was visually reviewed. Three effect cases passed with redundant
pre-assertion sleeps removed (`/private/tmp/l6-effect-output-ready/report.json`).

The effect startup was then revisited with a loader claim and a selected effect
processor condition; the two audio-output cases also wait for output counters.
A trial requiring output from all four cases failed the two UI-focused cases,
which do not guarantee nonzero output (`/private/tmp/l6-effect-output-startup-integrated/report.json`).
With output required only by the two audio-driven scenarios, all four passed
(`/private/tmp/l6-effect-scoped-startup/report.json`). This replaces the
previously restored five-second delay; the earlier failed boot-only trial
remains recorded above.

The effect editor mode-restore case also passed after four sleeps before
positive ring assertions were removed (`/private/tmp/l6-effect-rings-ready/report.json`).
The L6 and L6max audio input transport cases passed with seek and resumed
position checks polling the reported transport state rather than waiting
fixed periods (`/private/tmp/l6-transport-position-ready/report.json`). Their
audio sampling intervals are unchanged.

Four audio input cases passed with existing sample windows labeled as audio
output observations (`/private/tmp/l6-audio-observation-windows/report.json`).
Their durations were not shortened; the label distinguishes sample collection
from readiness delays.

L6 1.10 exposes a volume-A status byte used by the firmware's readiness
check. In diagnostic runs its status was 2 with the card mounted, 1 after
removal, and 2 after reinsertion. The placeholder-value probes intentionally
reported failures; the first removal attempt timed out because its diagnostic
delay had no timeout margin (`/private/tmp/l6-sd-mount-values/report.json` and
`/private/tmp/l6-sd-mount-removed-value/report.json`). The real storage case
now waits for those transitions on L6; both models passed the full case with
its existing trace and display assertions (`/private/tmp/l6-storage-status-ready/report.json`).

The USB storage test initially failed because it entered navigation before
the recorder home screen appeared (`/private/tmp/l6-usb-storage-menu-probe/report.json`).
It then passed with home/menu readiness; the first transfer-screen reference
was wrong because the connection prompt appears only after the host attaches
(`/private/tmp/l6-usb-storage-screens-ready/report.json`). The corrected,
visually reviewed transfer screen and menu selection passed with the complete
USB read/write checks (`/private/tmp/l6-usb-storage-screens-retry/report.json`).

The first persistence experiment was blocked because expanded includes reused
checkpoint names. An atomic press/display-change action then passed the full
RTC/NOR workflow with the repeated date-editor waits removed
(`/private/tmp/l6-persistence-confirm-ready/report.json`). Display changes
are synchronization evidence; the RTC/NOR assertions still check the result.

Removing the post-recording delays outright failed both cases because the WAV
files were not present yet (`/private/tmp/l6-record-finalization-probe/report.json`).
A bounded wait for each expected complete WAV file then passed both models
with the original tone checks (`/private/tmp/l6-record-files-validated/report.json`).
The file validator also has a unit test for truncated containers and chunks.

Both patch heap cases passed with recorded-file completion before playback
(`/private/tmp/l6-patch-heap-recording-ready/report.json`). The L6 case also
passed with encoder-report checks and playback-lamp transitions replacing its
remaining encoder/stop sleeps (`/private/tmp/l6-patch-heap-playback-state/report.json`).
Four effect cases passed when editor/panel state replaced 27 short transition
sleeps (`/private/tmp/l6-effect-mode-transitions/report.json`).

A temporary fresh-state prepared-home case passed with the original prompt
timing (`/private/tmp/l6-prepared-home-fresh-baseline/report.json`). A follow-up
that required the factory SD prompt failed because this fixture had already
reached recorder home (`/private/tmp/l6-prepared-home-fresh-ready/report.json`).
The shared fragment was restored: the SD prompt is conditional, so a mandatory
prompt image is not a safe replacement for its existing waits.

The L6max heap playback probe intentionally used placeholder expectations to
record `rec_mode` as 2 during playback and 0 after stopping
(`/private/tmp/l6max-rec-mode-values/report.json`). The heap case passed with
those transition checks replacing its post-playback delay
(`/private/tmp/l6max-patch-heap-playback-ready/report.json`). Both input
transport cases passed when playback position was checked before unloading a
channel (`/private/tmp/l6-transport-playing-ready/report.json`).

The factory disconnect state checks passed without their fixed delay
(`/private/tmp/l6-factory-disconnect-ready/report.json`), and both effect
slot-switch cases passed with the slot condition
(`/private/tmp/l6-effect-slot-switch-ready/report.json`). The volatile SD
case passed both models with guest removal evidence before checking the
resident module (`/private/tmp/l6-sd-resident-removal-ready/report.json`).
A persistence trial that waited only for edited RTC/NOR values failed the
subsequent restart RTC comparison (`/private/tmp/l6-persistence-commit-ready/report.json`);
the save delay was restored and the full case passed
(`/private/tmp/l6-persistence-save-restored/report.json`).

A representative 13-case sample passed with the driver's default four
workers (`/private/tmp/l6-readiness-sample-parallel/report.json`), covering
both models, recording, persistence, storage, patch heaps, factory mode,
effects, and MIDI. Earlier sequential samples had mixed results: persistence
failed both before and after these changes, and the L6max recording failed
afterwards with a short recorded track during heavy host load. Both passed in
the parallel sample, so these are timing-sensitive outcomes, not clean passes
across all attempts. The host load differed substantially between the before
and after runs, so their wall times are not a reliable speed comparison.

The full run found an unrelated `b0` panel report while the L6 panel-event
case was waiting for a button submission. The action now ignores non-button
reports until a bounded deadline, while still checking the button coordinates
and mapped event. The focused case passed afterward
(`/private/tmp/l6-readiness-panel-fix-run/report.json`); the original full-run
failure remains recorded in `/private/tmp/l6-readiness-full/report.json`.

The full run also failed the knob-dialog guard when a 500 ms guest delay
ended before the menu changed windows. Replacing the menu and return delays
with bounded current-window checks passed the complete knob-dialog case
(`/private/tmp/l6-knob-usb-probe/report.json`); the other two diagnostic
cases in that report deliberately failed with placeholder memory values.

The USB MIDI case now waits for channel-one mixer state to reflect the sent
CC instead of sleeping for one guest second. Placeholder probes observed
`0x12` on L6 and `0x18` on L6max before sending, and `0x20` on both after
the message (`/private/tmp/l6-usb-mixer-before-probe/report.json` and
`/private/tmp/l6-knob-usb-probe/report.json`). If the value is already the
target, the action first sends and observes a different value so a stale value
cannot satisfy the check. Both model cases passed with the new condition
(`/private/tmp/l6-usb-midi-level-ready/report.json`).

The scenario-file inventory does not include delays inside Rust device
helpers. In particular, `knob_dialog.rs` contains additional guest-time
waits and display captures; some test negative behavior or observation
windows, and the remaining transitions still need individual review.

The knob-dialog helper now waits for the display callback to discard invalid
mailbox tags, and checks for popup state and a changed display while allowing
the original bounded fallback window for a knob already at its limit. The
complete dialog case and generated firmware-patch popup case both passed
(`/private/tmp/l6-knob-internal-ready/report.json`). The subsequent default
four-worker regression of panel, knob-dialog, USB, and fresh DIN cases passed
all five cases (`/private/tmp/l6-readiness-regression-parallel/report.json`),
after the full-run failures documented above.

L6max MAIN 1.10's selector stores the encoder parameter mode at
`0x80462d2c`: the getter at `0x8000b4a8` reads it and the selection path calls
the setter at `0x8000bfe8`. The dialog test now waits for each selected mode,
and its negative first-use guards wait for the post-hook return at
`0x80036c78`. An initial lock-contention trial requiring the mixer value to
change failed (`/private/tmp/l6-knob-mode-ready/report.json`); the revised
hook-return check also works at the end of a knob's range. The complete
scenario passed, including all ten reviewed mode images and lock-contention
checks (`/private/tmp/l6-knob-hook-ready/report.json`).
