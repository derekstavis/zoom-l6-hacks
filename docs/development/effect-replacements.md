# SD effect replacements

Effect replacements are selected at build time and delivered as one SD module.
The selected effect's DSP implementation is kept in its own patch package, so
the composer links only the router and the implementations used by the chosen
stock slots.

## Packages

- `efx-loader` owns the firmware hooks, callback lifecycle, and
  dispatch from a stock effect slot to its configured implementation.
- `efx-plateau` owns the Plateau reverb DSP, parameter editor, and its state
  initialization/sample processing entry points.
- `efx-velvet` owns the Velvet decorrelator DSP, parameter editor, and its state
  initialization/sample processing entry points.
- `dual-efx` remains the original combined patch with its parameter editor.
  Its source now reuses the Plateau and Velvet DSP source files. It is not a
  dependency of the replacement bundles.

The effect packages declare their implementation IDs, compatible stock slots,
and optional controls in `package.toml`. The loader dispatches input according
to the selected effect's control declaration; it does not require every effect
to expose the same controls. The first two effect packages declare encoder
parameter editing and use TAP to toggle the editor. A future tempo effect can
declare a TAP tempo action without exposing the parameter editor.

```toml
[[effects]]
id = "velvet"
title = "Velvet Noise"
stock_slots = ["room", "spring"]
controls = ["encoder-parameters"]
tap_action = "toggle-editor"
```

An effect without encoder controls can leave `controls` empty and use stock TAP
behavior, or declare `controls = ["tap-tempo"]` and `tap_action = "tempo"` to
receive TAP events through its own callback.

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
includes `efx-loader` and each distinct selected effect package. For
example, `room = "velvet", spring = "stock"` links the router and `efx-velvet`
only; it does not link `efx-plateau` or the legacy `dual-efx` package. Assigning
Velvet to both slots still links its code once. The selected-slot registry is generated
by the composer and compiled into the module; there is no runtime plugin ABI.

At runtime, the router observes effect selection and workspace-clear events.
It drains an active custom callback before the firmware reuses the shared DSP
workspace, initializes the selected implementation, then routes its audio
callback in place of the stock one. With no configured replacement for the
selected slot, stock processing continues.

Every effect module provides `EFFECT_STATE_BYTES`, `initialize_effect`, and
`process_sample`. The optional `controls` entries add a small module interface:

- `encoder-parameters` requires `PARAMETER_COUNT`,
  `parameter_ring_position`, `edit_parameter`, and `show_editor_state`.
  `edit_parameter` receives a zero-based encoder and a relative step count, then
  returns the new ring position when it accepts the edit.
- `tap-tempo` requires `tap_tempo`, called from the effect's audio callback.

`tap_action = "toggle-editor"` routes TAP to the shared editor and requires
`encoder-parameters`. `tap_action = "tempo"` routes TAP events to `tap_tempo`.
`tap_action = "stock"` leaves the firmware's TAP behavior in place. An effect
can opt into encoder editing, TAP tempo, or no additional controls. TAP has one
action at a time, so the editor and tempo modes are alternatives in this first
interface. The generated composer only links the control calls that the
selected effect declares.

Parameter values persist across stock workspace clears in each loaded effect
module. Each module owns its encoder map, parameter ranges, display formatting,
and DSP updates; the loader owns editor-mode input routing and ring feedback.
Velvet exposes seven controls because the eighth Dual EFX encoder was a
Dual-only cascade control.

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
