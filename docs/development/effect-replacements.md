# SD effect replacements

Effect replacements are selected at build time and delivered as one SD module.
The selected effect's DSP implementation is kept in its own patch package, so
the composer links only the router and the implementations used by the chosen
stock slots.

## Packages

- `effect-replacements` owns the firmware hooks, callback lifecycle, and
  dispatch from a stock effect slot to its configured implementation.
- `efx-plateau` owns the Plateau reverb DSP and its state initialization/sample
  processing entry points.
- `efx-velvet` owns the Velvet decorrelator DSP and its state initialization/sample
  processing entry points.
- `dual-efx` remains the original combined patch with its parameter editor.
  Its source now reuses the Plateau and Velvet DSP source files. It is not a
  dependency of the replacement bundles.

The effect packages declare their implementation IDs and compatible stock
slots in `package.toml`. For example:

```toml
[[effects]]
id = "velvet"
title = "Velvet Noise"
stock_slots = ["room", "spring"]
```

The composition request chooses one implementation per slot:

```toml
[effect_replacements]
room = "plateau"
spring = "velvet"
```

The stock mixer still selects and runs only one effect at a time. A slot left
as stock keeps its original implementation. The web patcher builds/selects the
matching bundle; removing its SD file disables the replacements.

The emulator's **File → Firmware Patches…** window has separate Room and Spring
selectors on L6max. Choose Stock, Plateau Plate, or Velvet Noise for each slot,
then apply and restart. The emulator writes the matching combined bundle to the
configured SD card and saves the assignments in `config.toml` under
`[emulator.l6max.effect_replacements]`. These assignments require an SD card and
cannot be combined with the legacy Dual EFX patch.

## Composition and runtime

The resolver validates that each effect supports its assigned slot, then
includes `effect-replacements` and each distinct selected effect package. For
example, `room = "velvet", spring = "stock"` links the router and `efx-velvet`
only; it does not link `efx-plateau` or the legacy `dual-efx` package. Assigning
Velvet to both slots still links its code once. The selected-slot registry is generated
by the composer and compiled into the module; there is no runtime plugin ABI.

At runtime, the router observes effect selection and workspace-clear events.
It drains an active custom callback before the firmware reuses the shared DSP
workspace, initializes the selected implementation, then routes its audio
callback in place of the stock one. With no configured replacement for the
selected slot, stock processing continues.

The effect packages currently expose DSP state size, initialization, and
sample processing. A shared parameter-editing contract is not part of this
first composition step; the existing parameter editor remains in `dual-efx`.
The plugin boundary can grow parameter metadata and edit callbacks after that
API is designed.

## Validation

- Catalog tests validate effect IDs, stock-slot compatibility, package
  deduplication, and generated filenames.
- The `sd-effect-replacement-map` smoke case composes and loads a Velvet-only
  bundle, checks the router and selected implementation members, and observes
  custom DSP output while the mixer selects the matching slot.
- The packaged `gui-menu` case selects a Room and Spring implementation in the
  emulator workflow, checks the saved assignments and generated SD bundle, then
  removes the effect selection and verifies that the card and firmware are restored.
- The `sd-dual-efx-emulator` case continues to cover the original combined
  patch and parameter editor.

Memory ownership, callback timing, signal mapping, and audio quality are based
on L6max MAIN 1.10 emulator observations and still need hardware qualification.
