# `l6patch`

`l6patch` is the patch tool. It builds patched firmware and SD modules from an
unpacked stock update package (`l6fw unpack`), lists the patch catalog, and
regenerates the prebuilt files. It never edits its input.

```sh
cargo l6patch firmware STOCK_DIR OUTPUT_DIR [--patch NAME]... [--version DIGITS]
cargo l6patch firmware STOCK_DIR OUTPUT_DIR --loader [--patch NAME]... [--qemu-module-arena]
cargo l6patch module STOCK_DIR OUTPUT_DIR [--patch NAME]... [--effect SLOT=ID]...
cargo l6patch catalog [--model l6|l6max] [--format text|json]
cargo l6patch prebuilt [--config PATH | --l6max DIR --l6 DIR] [--check]
```

`cargo l6patch` runs the workspace build offline and locked; outside a
prepared checkout, use `cargo run -p l6patch -- …`. `l6patch STOCK_DIR
OUTPUT_DIR` is `l6patch firmware`, and still accepts the earlier
`--experimental-*` flags.

## Firmware and SD modules

`firmware` writes an update package with the patches applied. With `--loader`
it also installs the experimental Patch loader, which loads SD modules at
startup. `module` writes one composed SD module under `PATCHES/`; `--effect
room=velvet` assigns a custom effect to a stock effect slot. Both write a
`patch.json` report.

`cargo l6patch catalog` lists every package and how it can be installed;
`--format json` prints the catalog the website build reads.
Without `--patch`, the defaults are `knob-dialog` for L6max and `midi-sync` for
L6. Repeat `--patch` to combine compatible patches. L6 currently supports only
`midi-sync`. The output directory must not exist. A requested update version
is exactly four digits; without one the tool selects the version appropriate
to the chosen patch combination. `l6fw repack` preserves the payload checksum
by default; explicitly request `--recompute-checksum` after payload edits.

The tool checks package model and revision guards before writing. Read the
individual patch guide for model support, behavior, and validation commands.

Payloads are compiled `no_std` Rust using the shared firmware bindings and
hook macros. See the [Rust patch payload API guide](../patch-api.md) for the
function attributes and shared hook catalog used by the static builder and the
Patch loader.

## Prebuilt files

The browser patcher's templates (`tools/firmware-patch-wasm/templates/`) and the
SD modules the website and the emulator serve (`web/public/loadable-patches/`)
are committed, generated files. `cargo l6patch prebuilt` regenerates both from
the stock packages in `config.toml` `[firmware]`, which it unpacks itself; pass
`--l6max DIR --l6 DIR` to use folders you already unpacked. Do not edit the
generated files by hand.

Each template is a payload with every byte copied from the firmware removed;
its metadata records which firmware addresses fill them. The browser fills them
from the package the user uploads. For every template, `prebuilt` checks that
filling it from the real firmware rebuilds the native payload byte for byte, so
no Zoom bytes are committed.

Both manifests record a fingerprint of the sources that produce the files: the
patch sources, `rust-toolchain.toml`, and the crates `l6patch` is built from,
with registry crates by exact version. `cargo test -p l6patch` and `cargo
l6patch prebuilt --check` fail while the committed files are stale, without
needing firmware.
