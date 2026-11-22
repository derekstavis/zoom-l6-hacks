//! Source fingerprint for the prebuilt files. Each generated manifest records
//! it, so CI can find stale prebuilt files without Zoom firmware.
//!
//! The inputs are what can change a prebuilt file: the patch sources, the
//! pinned toolchain, and the crates `l6patch` is built from. Local crates
//! contribute their sources and registry crates their exact versions, so a
//! dependency change elsewhere in the workspace leaves the prebuilt files
//! fresh.
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    process::Command,
};

/// The manifests that record the fingerprint, relative to the repository.
pub(crate) const MANIFESTS: [&str; 2] = [
    "tools/firmware-patch-wasm/templates/manifest.json",
    "web/public/loadable-patches/manifest.json",
];

pub(crate) fn source_fingerprint(root: &Path) -> Result<String, String> {
    let mut files = vec![root.join("rust-toolchain.toml")];
    collect_sources(&root.join("patches"), &mut files)?;
    let mut versions = BTreeSet::new();
    for package in generator_closure(root)? {
        match package {
            Package::Local(directory) => collect_sources(&directory, &mut files)?,
            Package::Registry(version) => {
                versions.insert(version);
            }
        }
    }
    files.sort();
    files.dedup();

    let mut hash = Sha256::new();
    for path in files {
        let relative = path.strip_prefix(root).unwrap_or(&path).to_string_lossy();
        let contents = fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        hash.update(relative.as_bytes());
        hash.update([0]);
        hash.update((contents.len() as u64).to_le_bytes());
        hash.update(contents);
    }
    for version in versions {
        hash.update(version.as_bytes());
        hash.update([0]);
    }
    Ok(format!("{:x}", hash.finalize()))
}

/// Check every committed manifest against the current sources and the
/// supported stock firmware.
pub(crate) fn check(root: &Path) -> Result<(), String> {
    let expected = source_fingerprint(root)?;
    for relative in MANIFESTS {
        let manifest: Value = serde_json::from_slice(
            &fs::read(root.join(relative)).map_err(|e| format!("{relative}: {e}"))?,
        )
        .map_err(|e| format!("{relative}: {e}"))?;
        if manifest["generator_source_sha256"].as_str() != Some(expected.as_str()) {
            return Err(format!(
                "{relative} is stale; regenerate it with `cargo l6patch prebuilt`"
            ));
        }
        for (key, supported) in [
            (
                "source_firmware_sha256",
                l6fw::Model::L6max.stock_main_sha256(),
            ),
            (
                "l6_source_firmware_sha256",
                l6fw::Model::L6.stock_main_sha256(),
            ),
        ] {
            if manifest[key].as_str() != Some(supported) {
                return Err(format!(
                    "{relative} was generated from unsupported stock firmware ({key})"
                ));
            }
        }
    }
    Ok(())
}

enum Package {
    Local(PathBuf),
    Registry(String),
}

/// `l6patch` and its normal and build dependencies, from `cargo metadata`.
fn generator_closure(root: &Path) -> Result<Vec<Package>, String> {
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let output = Command::new(cargo)
        .args(["metadata", "--format-version", "1", "--offline", "--locked"])
        .arg("--manifest-path")
        .arg(root.join("Cargo.toml"))
        .output()
        .map_err(|e| format!("cargo metadata: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "cargo metadata failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    let metadata: Value =
        serde_json::from_slice(&output.stdout).map_err(|e| format!("cargo metadata: {e}"))?;
    let packages = metadata["packages"]
        .as_array()
        .ok_or("cargo metadata has no packages")?
        .iter()
        .map(|package| (package["id"].as_str().unwrap_or_default(), package))
        .collect::<BTreeMap<_, _>>();
    let nodes = metadata["resolve"]["nodes"]
        .as_array()
        .ok_or("cargo metadata has no resolve graph")?
        .iter()
        .map(|node| (node["id"].as_str().unwrap_or_default(), node))
        .collect::<BTreeMap<_, _>>();
    let start = packages
        .iter()
        .find(|(_, package)| package["name"] == "l6patch" && package["source"].is_null())
        .map(|(id, _)| *id)
        .ok_or("cargo metadata has no l6patch package")?;

    let mut seen = BTreeSet::from([start]);
    let mut pending = vec![start];
    while let Some(id) = pending.pop() {
        let node = nodes.get(id).ok_or("cargo metadata node missing")?;
        for dependency in node["deps"].as_array().into_iter().flatten() {
            let used = dependency["dep_kinds"]
                .as_array()
                .into_iter()
                .flatten()
                .any(|kind| kind["kind"].is_null() || kind["kind"] == "build");
            let target = dependency["pkg"].as_str().unwrap_or_default();
            if used && seen.insert(target) {
                pending.push(target);
            }
        }
    }
    seen.into_iter()
        .map(|id| {
            let package = packages.get(id).ok_or("cargo metadata package missing")?;
            Ok(if package["source"].is_null() {
                let manifest = package["manifest_path"]
                    .as_str()
                    .ok_or("package without manifest path")?;
                Package::Local(Path::new(manifest).parent().unwrap().to_path_buf())
            } else {
                Package::Registry(format!(
                    "{} {} {}",
                    package["name"].as_str().unwrap_or_default(),
                    package["version"].as_str().unwrap_or_default(),
                    package["source"].as_str().unwrap_or_default()
                ))
            })
        })
        .collect()
}

fn collect_sources(directory: &Path, files: &mut Vec<PathBuf>) -> Result<(), String> {
    let entries = fs::read_dir(directory).map_err(|e| format!("{}: {e}", directory.display()))?;
    for entry in entries {
        let path = entry.map_err(|e| e.to_string())?.path();
        if path.is_dir() {
            // Host test harnesses and build output never reach a payload.
            if path
                .file_name()
                .is_some_and(|name| name == "tests" || name == "target")
            {
                continue;
            }
            collect_sources(&path, files)?;
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
    #[test]
    fn committed_prebuilt_files_match_current_sources() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .canonicalize()
            .unwrap();
        if let Err(error) = super::check(&root) {
            panic!("{error}");
        }
    }
}
