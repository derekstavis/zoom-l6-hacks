# `l6fw`

`l6fw` unpacks and repacks L6 and L6max firmware update packages. The model is
detected from the package header.

```sh
cargo run --offline --locked -p l6fw -- unpack PACKAGE OUTPUT_DIRECTORY
cargo run --offline --locked -p l6fw -- repack INPUT_DIRECTORY OUTPUT_PACKAGE
cargo run --offline --locked -p l6fw -- repack INPUT_DIRECTORY OUTPUT_PACKAGE --recompute-checksum
cargo run --offline --locked -p l6fw -- update-checksum INPUT_PACKAGE OUTPUT_PACKAGE
cargo run --offline --locked -p l6fw -- update-images PACKAGE
```

Unpack into a new directory before editing. Repack writes a new package and
validates the expected firmware files. `--recompute-checksum` explicitly
regenerates the package checksum; otherwise the original checksum policy is
preserved. Keep firmware packages and extracted firmware out of Git.
`update-checksum` recalculates the checksum on an L6max MAIN-only update
package and validates the replacement installer's supported layout before
writing the output.

`l6fw` handles package structure. To apply supported firmware modifications,
see [`l6patch`](l6patch.md) and the patch-specific guides.

`update-images` validates an L6max MAIN-only package and prints sizes and SHA-256 hashes of its installer payload and panel image.
