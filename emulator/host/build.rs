use patch_core::patch_catalog;
use std::{env, fs, path::PathBuf};

fn main() {
    let manifest_dir = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let repository = manifest_dir.join("../..");
    let mut generated = String::new();
    let mut entries = Vec::new();
    let mut effect_entries = Vec::new();
    for package in patch_catalog() {
        for model in &package.emulator_sd_models {
            let model_expr = format!("l6fw::Model::{model:?}");
            let directory = model.label();
            let asset = repository
                .join("web/public/loadable-patches")
                .join(directory)
                .join(&package.file_name);
            println!("cargo:rerun-if-changed={}", asset.display());
            assert!(
                asset.is_file(),
                "missing emulator SD artifact {}",
                asset.display()
            );
            entries.push(format!(
                "Module {{ patch: {:?}, model: {model_expr}, bytes: include_bytes!({:?}) }},",
                package.name, asset
            ));
        }
    }
    for replacements in patch_core::effect_assignments() {
        let effect = |slot: &str| {
            replacements
                .iter()
                .find(|(selected, _)| selected == slot)
                .map(|(_, id)| id.clone())
        };
        let (room, spring) = (effect("room"), effect("spring"));
        let file_name = patch_core::Plan::new(
            l6fw::Model::L6max,
            patch_core::Delivery::SdCard,
            &[],
            &replacements,
        )
        .and_then(|plan| plan.file_name())
        .expect("catalog effect map has a versioned bundle filename");
        let asset = repository
            .join("web/public/loadable-patches/L6max")
            .join(&file_name);
        println!("cargo:rerun-if-changed={}", asset.display());
        assert!(
            asset.is_file(),
            "missing emulator effect bundle {}",
            asset.display()
        );
        effect_entries.push(format!(
                "EffectBundle {{ room: {:?}, spring: {:?}, file_name: {:?}, bytes: include_bytes!({:?}) }},",
                room, spring, file_name, asset
            ));
    }
    generated.push_str(&format!("const MODULES: [Module; {}] = [", entries.len()));
    generated.push_str(&entries.join("\n"));
    generated.push_str("];\n");
    generated.push_str(&format!(
        "const EFFECT_BUNDLES: [EffectBundle; {}] = [",
        effect_entries.len()
    ));
    generated.push_str(&effect_entries.join("\n"));
    generated.push_str("];\n");
    let output = PathBuf::from(env::var_os("OUT_DIR").unwrap()).join("sd_modules.rs");
    fs::write(output, generated).expect("write embedded SD module list");
}
