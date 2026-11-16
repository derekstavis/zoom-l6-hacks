# Effect Replacements

The common router owns effect-workspace hooks and the stream callback. The
selected effect package supplies the DSP state size, initialization, and sample
processor that the composition compiler connects to it. The composer includes
only the implementation packages selected for Room and Spring, so a
Velvet-only build does not carry Plateau's DSP code or state.

This package is the router implementation, not a standalone user-selected
effect. Select effects through the website's per-slot controls; each resulting
composition is a single `.BIN` file for the SD card. See
[`docs/development/effect-replacements.md`](../../docs/development/effect-replacements.md)
for the composition contract and validation details.
