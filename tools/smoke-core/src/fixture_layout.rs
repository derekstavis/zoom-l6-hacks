//! Names of the files the suite driver builds into a case's artifact
//! directory and the scenario runner reads back. Both sides use these
//! constants, and the driver checks [`required`] before a case runs.
use crate::Scenario;

/// The stock firmware the SD loader was built from, unpacked.
pub const SD_STOCK_FIRMWARE: &str = "sd-stock-firmware";
/// The Patch loader firmware build, with its `patch.json` report.
pub const LOADER_BUILD: &str = "patch-loader-firmware";
/// The SD bundle build, with its `patch.json` report and `PATCHES/`.
pub const SD_BUNDLE_BUILD: &str = "sd-patches-build";
/// The patch tools' report for the MAIN image the case boots, when they
/// built it; [`crate::firmware`] identifies the image by it.
pub const FIRMWARE_REPORT: &str = "patch-report.json";
/// The report an `l6patch` build writes beside its output.
pub const PATCH_REPORT: &str = "patch.json";
/// Present when the card holds a pass-through module ahead of the real one.
pub const CHAIN_FIRST: &str = "sd-chain-first.BIN";
/// A patched update package; its report sits beside it as
/// `patched-update.json`.
pub const PATCHED_UPDATE: &str = "patched-update.bin";

/// Files under the case directory that `scenario` reads for `model`, relative
/// to that directory.
pub fn required(scenario: &Scenario, model: l6fw::Model) -> Vec<String> {
    let mut files = Vec::new();
    if let Some(patches) = &scenario.sd_patches {
        files.push(format!("{SD_STOCK_FIRMWARE}/main_firmware.bin"));
        files.push(format!("{LOADER_BUILD}/{PATCH_REPORT}"));
        if !patches.is_empty() || !scenario.effect_replacements.is_empty() {
            files.push(format!("{SD_BUNDLE_BUILD}/{PATCH_REPORT}"));
        }
        if scenario.sd_bundle_mode.as_deref() == Some("ordered-chain") {
            files.push(CHAIN_FIRST.into());
        }
    }
    if scenario
        .update_patches
        .get(&model)
        .is_some_and(|patches| !patches.is_empty())
    {
        files.push(PATCHED_UPDATE.into());
        files.push(PATCHED_UPDATE.replace(".bin", ".json"));
    }
    files
}
