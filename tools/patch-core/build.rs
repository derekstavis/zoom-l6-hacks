//! Reads every `patches/*/package.toml` into the catalog the library embeds.
#[allow(dead_code)]
#[path = "src/catalog.rs"]
mod catalog;

use std::{env, fs, path::PathBuf};

fn main() {
    let manifest_dir = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let patch_root = manifest_dir.join("../../patches");
    println!("cargo:rerun-if-changed={}", patch_root.display());
    println!("cargo:rerun-if-changed=src/catalog.rs");

    let mut packages = Vec::new();
    for entry in fs::read_dir(&patch_root).expect("read patches directory") {
        let entry = entry.expect("read patch directory entry");
        let path = entry.path().join("package.toml");
        if !path.is_file() {
            continue;
        }
        println!("cargo:rerun-if-changed={}", path.display());
        let text = fs::read_to_string(&path).expect("read package.toml");
        let file: catalog::PackageFile =
            toml::from_str(&text).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
        assert_eq!(
            entry.file_name().to_string_lossy(),
            file.package.name,
            "package directory and package name differ"
        );
        packages.push(file.into_package());
    }
    let packages = catalog::finish(packages).unwrap_or_else(|error| panic!("{error}"));

    let names = |supported: fn(&catalog::PatchPackage) -> bool| {
        packages
            .iter()
            .filter(|package| package.kind == catalog::PackageKind::Patch && supported(package))
            .map(|package| format!("{:?},", package.name))
            .collect::<Vec<_>>()
    };
    let mut generated = String::new();
    for (constant, list) in [
        (
            "PATCHES",
            names(|package| !package.firmware_models.is_empty()),
        ),
        (
            "WASM_PATCHES",
            names(|package| !package.wasm_models.is_empty()),
        ),
    ] {
        generated.push_str(&format!(
            "pub const {constant}: [&str; {}] = [{}];\n",
            list.len(),
            list.concat()
        ));
    }

    let out_dir = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    let catalog_path = out_dir.join("patch_catalog.json");
    fs::write(
        &catalog_path,
        serde_json::to_vec_pretty(&serde_json::json!({ "packages": packages }))
            .expect("serialize the catalog"),
    )
    .expect("write package catalog JSON");
    generated.push_str(&format!(
        "pub const PATCH_CATALOG_JSON: &str = include_str!({catalog_path:?});\n"
    ));
    fs::write(out_dir.join("generated_catalog.rs"), generated)
        .expect("write generated Rust package catalog");
}
