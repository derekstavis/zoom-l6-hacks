use sha2::{Digest, Sha256};
use std::{
    fs, io,
    path::{Path, PathBuf},
};

const INPUT_DIRS: &[&str] = &[
    "patches",
    "tools/firmware-patch-core/src",
    "tools/firmware-patch-compile/src",
    "tools/l6patch/src",
    "tools/patch-hook-macros/src",
    "tools/firmware-patch-wasm-prebuild/src",
];

const INPUT_FILES: &[&str] = &[
    "Cargo.toml",
    "Cargo.lock",
    "tools/build-web-loadable-patches.sh",
    "tools/firmware-patch-core/Cargo.toml",
    "tools/firmware-patch-compile/Cargo.toml",
    "tools/l6patch/Cargo.toml",
    "tools/patch-hook-macros/Cargo.toml",
    "tools/firmware-patch-wasm-prebuild/Cargo.toml",
];

/// Fingerprint source and build inputs that affect generated browser patches.
/// The generated manifests carry this value so normal CI can detect stale
/// assets without needing Zoom firmware files.
pub fn source_fingerprint(root: &Path) -> io::Result<String> {
    let mut files = Vec::new();
    for relative in INPUT_FILES {
        let path = root.join(relative);
        if path.is_file() {
            files.push(path);
        }
    }
    for relative in INPUT_DIRS {
        let path = root.join(relative);
        if path.is_dir() {
            collect_inputs(&path, &mut files)?;
        }
    }
    files.sort();
    files.dedup();

    let mut hash = Sha256::new();
    for path in files {
        let relative = path.strip_prefix(root).unwrap_or(&path);
        let relative = relative.to_string_lossy();
        let contents = fs::read(&path)?;
        hash.update(relative.as_bytes());
        hash.update([0]);
        hash.update((contents.len() as u64).to_le_bytes());
        hash.update(contents);
    }
    Ok(format!("{:x}", hash.finalize()))
}

fn collect_inputs(directory: &Path, files: &mut Vec<PathBuf>) -> io::Result<()> {
    for entry in fs::read_dir(directory)? {
        let path = entry?.path();
        if path.is_dir() {
            collect_inputs(&path, files)?;
        } else if matches!(
            path.extension().and_then(|extension| extension.to_str()),
            Some("rs" | "toml" | "ld" | "s")
        ) {
            files.push(path);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::source_fingerprint;
    use serde_json::Value;
    use std::{fs, path::Path};

    fn assert_manifest_fingerprint(root: &Path, manifest_path: &Path, expected: &str) {
        let manifest: Value = serde_json::from_slice(
            &fs::read(manifest_path).expect("read generated patch manifest"),
        )
        .expect("parse generated patch manifest");
        assert_eq!(
            manifest["generator_source_sha256"].as_str(),
            Some(expected),
            "{} is stale; regenerate browser patch assets with the firmware-patch-wasm-prebuild and build-web-loadable-patches tools",
            manifest_path
                .strip_prefix(root)
                .unwrap_or(manifest_path)
                .display()
        );
        assert_eq!(
            manifest["source_firmware_sha256"].as_str(),
            Some(firmware_patch_core::EXPECTED_FIRMWARE_SHA256),
            "{} was generated from an unsupported L6max firmware image",
            manifest_path
                .strip_prefix(root)
                .unwrap_or(manifest_path)
                .display()
        );
        assert_eq!(
            manifest["l6_source_firmware_sha256"].as_str(),
            Some(firmware_patch_core::L6_EXPECTED_FIRMWARE_SHA256),
            "{} was generated from an unsupported L6 firmware image",
            manifest_path
                .strip_prefix(root)
                .unwrap_or(manifest_path)
                .display()
        );
    }

    #[test]
    fn checked_in_browser_patch_assets_match_current_sources() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .canonicalize()
            .expect("repository root");
        let expected = source_fingerprint(&root).expect("hash patch source inputs");

        let templates_manifest = root.join("tools/firmware-patch-wasm/templates/manifest.json");
        assert_manifest_fingerprint(&root, &templates_manifest, &expected);

        let web_manifest = root.join("web/public/loadable-patches/manifest.json");
        if web_manifest.is_file() {
            assert_manifest_fingerprint(&root, &web_manifest, &expected);
        }
    }
}
