//! Prebuilt SD modules for `l6patch prebuilt`: every module the website or
//! the emulator offers, and every L6max effect replacement bundle. Firmware is
//! read locally and never copied into `web/public`.
use crate::{BuildMode, BuildRequest};
use firmware_patch_core::{STOCK_EFFECT_SLOTS, patch_catalog};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
};

/// Where the modules are committed, relative to the repository.
pub(crate) const DIR: &str = "web/public/loadable-patches";

pub(crate) fn generate(
    root: &Path,
    l6max_source: &Path,
    l6_source: &Path,
    scratch: &Path,
    fingerprint: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let directory = root.join(DIR);
    let mut modules = Vec::new();
    for package in patch_catalog() {
        let models = package
            .website_sd_models
            .iter()
            .chain(&package.emulator_sd_models)
            .collect::<BTreeSet<_>>();
        for model in models {
            let source = if model == "L6" {
                l6_source
            } else {
                l6max_source
            };
            let file = build(
                source,
                &scratch.join(format!("{model}-{}", package.name)),
                vec![package.name.clone()],
                vec![],
            )?;
            if file.file_name().unwrap().to_string_lossy() != package.file_name {
                return Err(format!(
                    "{} {model}: the catalog expects {} but l6patch wrote {}",
                    package.name,
                    package.file_name,
                    file.display()
                )
                .into());
            }
            let mut entry = install(&file, &directory, model)?;
            entry["id"] = json!(package.name);
            entry["model"] = json!(model);
            modules.push(entry);
        }
    }

    let mut effects = patch_catalog()
        .iter()
        .flat_map(|package| package.effects.iter().map(|effect| effect.id.as_str()))
        .collect::<Vec<_>>();
    effects.sort();
    let choices = std::iter::once("stock").chain(effects).collect::<Vec<_>>();
    let mut replacements = Vec::new();
    for room in &choices {
        for spring in &choices {
            if *room == "stock" && *spring == "stock" {
                continue;
            }
            let assignments = STOCK_EFFECT_SLOTS
                .into_iter()
                .zip([room, spring])
                .filter(|(_, effect)| **effect != "stock")
                .map(|(slot, effect)| (slot.to_owned(), (*effect).to_owned()))
                .collect();
            let file = build(
                l6max_source,
                &scratch.join(format!("L6max-effects-{room}-{spring}")),
                vec![],
                assignments,
            )?;
            let mut entry = install(&file, &directory, "L6max")?;
            entry["room"] = json!(room);
            entry["spring"] = json!(spring);
            replacements.push(entry);
        }
    }

    let expected = modules
        .iter()
        .chain(&replacements)
        .map(|entry| entry["file"].as_str().unwrap().to_owned())
        .collect::<BTreeSet<_>>();
    for model in fs::read_dir(&directory)? {
        let model = model?.path();
        if !model.is_dir() {
            continue;
        }
        for file in fs::read_dir(&model)? {
            let file = file?.path();
            let relative = format!(
                "{}/{}",
                model.file_name().unwrap().to_string_lossy(),
                file.file_name().unwrap().to_string_lossy()
            );
            if file.extension().is_some_and(|ext| ext == "bin") && !expected.contains(&relative) {
                fs::remove_file(file)?;
            }
        }
    }

    let digest = |source: &Path| -> Result<String, std::io::Error> {
        Ok(format!(
            "{:x}",
            Sha256::digest(fs::read(source.join("main_firmware.bin"))?)
        ))
    };
    let manifest = json!({
        "effect_replacements": replacements,
        "generator_source_sha256": fingerprint,
        "l6_source_firmware_sha256": digest(l6_source)?,
        "modules": modules,
        "source_firmware_sha256": digest(l6max_source)?,
    });
    let mut text = serde_json::to_string_pretty(&manifest)?;
    text.push('\n');
    fs::write(directory.join("manifest.json"), text)?;
    println!(
        "Wrote {} SD modules and {} effect replacement bundles to {}",
        modules.len(),
        replacements.len(),
        directory.display()
    );
    Ok(())
}

/// Build one composed SD module and return the path of its module file.
fn build(
    source: &Path,
    output: &Path,
    patches: Vec<String>,
    effect_replacements: Vec<(String, String)>,
) -> Result<PathBuf, Box<dyn std::error::Error>> {
    crate::build(BuildRequest {
        source: source.to_path_buf(),
        output: output.to_path_buf(),
        patches,
        effect_replacements,
        version: None,
        mode: BuildMode::SdBundle,
        experimental_qemu_module_arena: false,
    })?;
    let report: Value = serde_json::from_slice(&fs::read(output.join("patch.json"))?)?;
    let file = report["patches"][0]["file"]
        .as_str()
        .ok_or("patch.json names no module file")?;
    Ok(output.join(file))
}

/// Copy a module into the committed folder and describe it for the manifest.
fn install(
    file: &Path,
    directory: &Path,
    model: &str,
) -> Result<Value, Box<dyn std::error::Error>> {
    let data = fs::read(file)?;
    if data.get(..4) != Some(b"L6SB".as_slice()) || data.len() < 8 {
        return Err(format!("{}: not an SD patch module", file.display()).into());
    }
    let name = file.file_name().unwrap().to_string_lossy();
    fs::create_dir_all(directory.join(model))?;
    fs::write(directory.join(model).join(name.as_ref()), &data)?;
    Ok(json!({
        "file": format!("{model}/{name}"),
        "format_version": u16::from_le_bytes([data[4], data[5]]),
        "loader_abi": u16::from_le_bytes([data[6], data[7]]),
        "sha256": format!("{:x}", Sha256::digest(&data)),
    }))
}
