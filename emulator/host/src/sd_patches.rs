//! Patches the Patch loader reads from the SD card. The modules are the ones the
//! web patcher hosts; the `patch-loader-core` tests check them against the loader.
use std::{collections::BTreeMap, path::Path};

/// The card folder the loader scans.
const FOLDER: &str = "PATCHES";

struct Module {
    patch: &'static str,
    model: l6fw::Model,
    bytes: &'static [u8],
}

struct EffectBundle {
    room: Option<&'static str>,
    spring: Option<&'static str>,
    file_name: &'static str,
    bytes: &'static [u8],
}

include!(concat!(env!("OUT_DIR"), "/sd_modules.rs"));

fn card_file_name(module: &Module) -> String {
    let package = firmware_patch_core::patch_package(module.patch)
        .expect("embedded SD module must have a package entry");
    format!("{:02}-{}", package.order, package.file_name)
}

/// The patches `model` can load from its SD card, in catalog order.
pub fn available(model: l6fw::Model) -> Vec<&'static str> {
    let model_name = crate::config::model_name(model);
    firmware_patch_core::patch_catalog()
        .iter()
        .filter(|package| {
            package.kind == "patch"
                && package
                    .emulator_sd_models
                    .iter()
                    .any(|name| name.eq_ignore_ascii_case(model_name))
        })
        .filter(|package| module(model, &package.name).is_some())
        .map(|package| package.name.as_str())
        .collect()
}

fn module(model: l6fw::Model, patch: &str) -> Option<&'static Module> {
    MODULES
        .iter()
        .find(|module| module.model == model && module.patch == patch)
}

fn effect_bundle(
    model: l6fw::Model,
    replacements: &BTreeMap<String, String>,
) -> Result<Option<&'static EffectBundle>, String> {
    if replacements.is_empty() {
        return Ok(None);
    }
    if model != l6fw::Model::L6max {
        return Err("custom effect replacements are supported on L6max only".into());
    }
    let entries = replacements
        .iter()
        .map(|(slot, effect)| (slot.clone(), effect.clone()))
        .collect::<Vec<_>>();
    let file_name = firmware_patch_core::effect_replacement_file_name(&entries)?;
    EFFECT_BUNDLES
        .iter()
        .find(|bundle| {
            bundle.file_name == file_name
                && bundle.room == replacements.get("room").map(String::as_str)
                && bundle.spring == replacements.get("spring").map(String::as_str)
        })
        .map(Some)
        .ok_or_else(|| format!("effect replacement bundle {file_name} is not embedded"))
}

/// Validate an effect map and ensure its generated SD module is available.
pub fn check_effect_replacements(
    model: l6fw::Model,
    replacements: &BTreeMap<String, String>,
) -> Result<(), String> {
    if replacements.is_empty() {
        return Ok(());
    }
    let entries = replacements
        .iter()
        .map(|(slot, effect)| (slot.clone(), effect.clone()))
        .collect::<Vec<_>>();
    firmware_patch_core::effect_replacement_packages(
        &entries,
        firmware_patch_core::patch_catalog(),
    )?;
    effect_bundle(model, replacements)?;
    Ok(())
}

fn effect_card_file_name(bundle: &EffectBundle) -> String {
    let order = firmware_patch_core::patch_package("efx-loader")
        .expect("effect replacement package is in the catalog")
        .order;
    format!("{order:02}-{}", bundle.file_name)
}

/// Check whether a card contains the exact hosted bundle for this selection.
pub fn contains_effect_bundle(
    card: &Path,
    model: l6fw::Model,
    replacements: &BTreeMap<String, String>,
) -> Result<bool, String> {
    let Some(bundle) = effect_bundle(model, replacements)? else {
        return Ok(true);
    };
    let mut volume =
        sd_image::open(card).map_err(|error| format!("SD card {}: {error}", card.display()))?;
    let path = format!("{FOLDER}/{}", effect_card_file_name(bundle));
    Ok(volume.read(&path).is_ok_and(|bytes| bytes == bundle.bytes))
}

/// Check that each patch in `selected` loads from `model`'s card.
pub fn check(model: l6fw::Model, selected: &[String]) -> Result<(), String> {
    for patch in selected {
        if module(model, patch).is_none() {
            return Err(format!(
                "{patch} cannot load from the {} SD card; it can only be installed in the firmware",
                crate::config::model_name(model)
            ));
        }
    }
    Ok(())
}

/// Bring the card's `PATCHES` folder to `selected`. Only files that are
/// byte-for-byte one of these modules are removed, so a card's own patch files
/// stay. Returns whether the card changed.
pub fn install(
    card: &Path,
    model: l6fw::Model,
    selected: &[String],
    effect_replacements: &BTreeMap<String, String>,
) -> Result<bool, String> {
    check(model, selected)?;
    check_effect_replacements(model, effect_replacements)?;
    let error = |e: std::io::Error| format!("SD card {}: {e}", card.display());
    let mut volume = sd_image::open(card).map_err(error)?;
    let wanted: Vec<&Module> = selected
        .iter()
        .filter_map(|patch| module(model, patch))
        .collect();
    let selected_effect = effect_bundle(model, effect_replacements)?;
    let has_wanted = !wanted.is_empty() || selected_effect.is_some();
    let mut changed = false;
    if volume.exists(FOLDER) {
        for entry in volume.entries(FOLDER).map_err(error)? {
            if entry.directory {
                continue;
            }
            let path = format!("{FOLDER}/{}", entry.name);
            let known_length = MODULES
                .iter()
                .any(|module| module.bytes.len() as u64 == entry.length)
                || EFFECT_BUNDLES
                    .iter()
                    .any(|bundle| bundle.bytes.len() as u64 == entry.length);
            if !known_length {
                continue;
            }
            let bytes = volume.read(&path).map_err(error)?;
            let hosted = MODULES.iter().any(|module| module.bytes == bytes)
                || EFFECT_BUNDLES.iter().any(|bundle| bundle.bytes == bytes);
            if !hosted {
                continue;
            }
            let keep_patch = wanted.iter().any(|module| {
                card_file_name(module).eq_ignore_ascii_case(&entry.name) && module.bytes == bytes
            });
            let keep_effect = selected_effect.is_some_and(|bundle| {
                effect_card_file_name(bundle).eq_ignore_ascii_case(&entry.name)
                    && bundle.bytes == bytes
            });
            if !keep_patch && !keep_effect {
                volume.remove(&path).map_err(error)?;
                changed = true;
            }
        }
    } else if has_wanted {
        volume.create_dir(FOLDER).map_err(error)?;
        changed = true;
    }
    for module in wanted {
        let path = format!("{FOLDER}/{}", card_file_name(module));
        if volume.read(&path).is_ok_and(|bytes| bytes == module.bytes) {
            continue;
        }
        volume.write(&path, module.bytes).map_err(error)?;
        changed = true;
    }
    if let Some(bundle) = selected_effect {
        let path = format!("{FOLDER}/{}", effect_card_file_name(bundle));
        if !volume.read(&path).is_ok_and(|bytes| bytes == bundle.bytes) {
            volume.write(&path, bundle.bytes).map_err(error)?;
            changed = true;
        }
    }
    if changed {
        volume.flush().map_err(error)?;
    }
    Ok(changed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn no_effects() -> BTreeMap<String, String> {
        BTreeMap::new()
    }

    fn card(name: &str) -> std::path::PathBuf {
        let path =
            std::env::temp_dir().join(format!("l6-sd-patches-{name}-{}.img", std::process::id()));
        let _ = std::fs::remove_file(&path);
        sd_image::create_empty(&path, "TEST").unwrap();
        path
    }

    fn files(card: &Path) -> Vec<String> {
        let mut volume = sd_image::open(card).unwrap();
        if !volume.exists(FOLDER) {
            return Vec::new();
        }
        let mut names: Vec<String> = volume
            .entries(FOLDER)
            .unwrap()
            .into_iter()
            .map(|entry| entry.name.to_ascii_uppercase())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn offers_the_hosted_modules_per_model() {
        assert_eq!(available(l6fw::Model::L6max), ["knob-dialog", "midi-sync"]);
        assert_eq!(available(l6fw::Model::L6), ["midi-sync"]);
        assert!(check(l6fw::Model::L6, &["knob-dialog".into()]).is_err());
        assert!(check(l6fw::Model::L6max, &["din-mixer-control".into()]).is_err());
    }

    #[test]
    fn writes_selected_modules_and_removes_only_its_own() {
        let path = card("install");
        let both = ["knob-dialog".to_owned(), "midi-sync".to_owned()];
        assert!(install(&path, l6fw::Model::L6max, &both, &no_effects()).unwrap());
        assert_eq!(
            files(&path),
            ["10-KNOB-DIALOG-1.0.0.BIN", "30-MIDI-SYNC-1.0.0.BIN"]
        );
        let mut volume = sd_image::open(&path).unwrap();
        assert_eq!(
            volume.read("PATCHES/30-midi-sync-1.0.0.bin").unwrap(),
            module(l6fw::Model::L6max, "midi-sync").unwrap().bytes
        );
        volume.write("PATCHES/02-MINE.BIN", b"user patch").unwrap();
        volume.flush().unwrap();
        drop(volume);
        assert!(!install(&path, l6fw::Model::L6max, &both, &no_effects()).unwrap());
        assert!(
            install(
                &path,
                l6fw::Model::L6max,
                &["midi-sync".into()],
                &no_effects()
            )
            .unwrap()
        );
        assert_eq!(files(&path), ["02-MINE.BIN", "30-MIDI-SYNC-1.0.0.BIN"]);
        assert!(install(&path, l6fw::Model::L6max, &[], &no_effects()).unwrap());
        assert_eq!(files(&path), ["02-MINE.BIN"]);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn an_unselected_card_stays_untouched() {
        let path = card("untouched");
        assert!(!install(&path, l6fw::Model::L6, &[], &no_effects()).unwrap());
        assert!(files(&path).is_empty());
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn installs_and_removes_the_selected_effect_bundle() {
        let path = card("effect-bundle");
        let effects = [("room".to_owned(), "velvet".to_owned())]
            .into_iter()
            .collect::<BTreeMap<_, _>>();
        assert!(install(&path, l6fw::Model::L6max, &[], &effects).unwrap());
        assert!(contains_effect_bundle(&path, l6fw::Model::L6max, &effects).unwrap());
        assert_eq!(files(&path).len(), 1);
        assert!(install(&path, l6fw::Model::L6max, &[], &no_effects()).unwrap());
        assert!(files(&path).is_empty());
        std::fs::remove_file(path).unwrap();
    }
}
