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

The failing L6max SD MIDI case did not deliver the expected encoder CC. The
L6max SD Knob Dialog/effect case reached the check before the recorder window
was ready. Both ran without the matrix's eight-second delay, even though the
SD loader had reported a completed claim. Loader completion is therefore not a
readiness signal for the MIDI path or recorder UI. The delay is retained for
L6max pending separate evidence; the failure must not be relabeled a flake.
With that delay restored, a focused no-retry rerun passed both L6max cases
(22.0 s for SD MIDI and 25.6 s for Knob Dialog/effect), as well as the L6 SD
MIDI case (15.5 s). Its report is
`/private/tmp/l6-wait-patch-confirm/report.json` on the testing machine.

The positive SD patch assertion now waits for the loader's completed claim
before retaining its existing module and hook checks. An absence assertion
remains immediate, and scenarios must retain any observation period needed to
establish absence. The panel-key map polls the model-specific key bit after
input acknowledgement. The L6max storage case waits for the existing reviewed
card-removed and recorder-home display images; L6, which has no display, keeps
its delays. A subsequent confirmation with explicit display-change guards
passed both models: 27.4 s for L6 and 15.9 s for L6max. Its report is
`/private/tmp/l6-wait-storage-confirm/report.json` on the testing machine.

On the testing machine, complete reports and per-case fixture hashes are in
`/private/tmp/l6-wait-comparison-before/report.json`,
`/private/tmp/l6-wait-supplement-before/report.json`, and
`/private/tmp/l6-wait-comparison-after/report.json`. These local paths are
run artifacts, not committed fixtures. Re-run the affected scenarios and full
qualification after changes; the sample alone does not establish correctness
under every host load or on physical devices.
