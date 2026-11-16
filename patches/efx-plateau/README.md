# Plateau Plate

This package contains the Plateau Plate Reverb DSP implementation, its
parameter editor, and its effect metadata. The effect composer links it into an
SD bundle only when Plateau is assigned to the stock Room or Spring slot. It
exposes state initialization and sample processing to the shared effect router,
and owns its encoder map, ranges, display values, and DSP updates. It is a
build-time component and is not a separately installed SD file. The package
depends on `knob-dialog` for parameter display.

## DSP tests

Run the host-side DSP unit tests with:

```sh
cargo test --manifest-path patches/efx-plateau/tests/Cargo.toml
```
