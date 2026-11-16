# Effect Replacements

The common router owns effect-workspace hooks, the stream callback, TAP routing,
and parameter-ring feedback. The selected effect package supplies its DSP state
size, initialization, sample processor, and any optional controls declared in
`package.toml`. The composer includes only the implementations and dependencies
selected for Room and Spring, so a Velvet-only build does not carry Plateau's
DSP code or state.

For parameter editing, the effect formats and submits the parameter label and
value through the `knob-dialog` API. The shared dialog draws that text; the
router handles editor visibility, encoder routing, and ring feedback.

This package is the router implementation, not a standalone user-selected
effect. Select effects through the website's per-slot controls; each resulting
composition is a single `.BIN` file for the SD card. See
[`docs/development/effect-replacements.md`](../../docs/development/effect-replacements.md)
for the composition contract and validation details.
