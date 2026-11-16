use firmware_patch_core::patch_catalog;
use std::{env, fs, path::PathBuf};

fn main() {
    let manifest_dir = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let repository = manifest_dir.join("../..");
    let mut generated = String::new();
    let mut entries = Vec::new();
    let mut effect_entries = Vec::new();
    for package in patch_catalog() {
        for model in &package.emulator_sd_models {
            let (model_expr, directory) = match model.as_str() {
                "L6" => ("l6fw::Model::L6", "L6"),
                "L6max" => ("l6fw::Model::L6max", "L6max"),
                other => panic!("unsupported model {other} in {}", package.name),
            };
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
    let effects = patch_catalog()
        .iter()
        .flat_map(|package| package.effects.iter())
        .collect::<Vec<_>>();
    let choices = || {
        std::iter::once(None)
            .chain(effects.iter().map(|effect| Some(effect.id.as_str())))
            .collect::<Vec<_>>()
    };
    for room in choices() {
        for spring in choices() {
            if room.is_none() && spring.is_none() {
                continue;
            }
            let replacements = [
                room.map(|effect| ("room".to_owned(), effect.to_owned())),
                spring.map(|effect| ("spring".to_owned(), effect.to_owned())),
            ]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>();
            let file_name = firmware_patch_core::effect_replacement_file_name(&replacements)
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
