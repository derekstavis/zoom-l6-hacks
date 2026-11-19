//! The generated `patches` suite.
//!
//! Every valid combination of patch placements and effect slot assignments,
//! derived from the package declarations, plus one case per patch and
//! delivery method. Each case boots once and runs a short functional check
//! for every patch it installs.
//!
//! - **Placements:** each patch is absent, installed in firmware, or loaded
//!   from the SD card, wherever its package allows.
//! - **Effect assignments:** each stock effect slot keeps its stock effect or
//!   runs one of the declared replacement effects.
//! - **Valid:** the firmware set and the SD set each include their own
//!   dependencies, and no patch is in both. A combination that relies on
//!   the builder adding a dependency equals another combination that names
//!   it, so it is not generated twice.
//! - **Delivery:** besides `l6patch` firmware and engine-built SD modules,
//!   patches the browser patcher supports are installed by the engine and
//!   through a firmware update, and hosted SD modules are loaded as the
//!   website delivers them.

use firmware_patch_core::{
    EFFECT_REPLACEMENT_ROUTER_PACKAGE, FirmwareModel, PatchCategory, PatchPackage,
    STOCK_EFFECT_SLOTS, patch_catalog, patch_package, resolve_patch_dependencies,
};
use std::collections::BTreeSet;
use std::fmt::Write as _;

/// Suite that runs the generated cases.
pub const SUITE: &str = "patches";
/// Case and scenario-file prefix.
pub const PREFIX: &str = "patches-";

/// The stock System menu, before patches add to it.
const STOCK_SYSTEM_MENU: [&str; 7] = [
    "Date/Time",
    "Battery Type",
    "Auto Power Off",
    "Display Brightness",
    "Power Saving",
    "Firmware",
    "Reset All Settings",
];

/// What the matrix checks for a patch, and the System menu items it adds.
struct Check {
    patch: &'static str,
    system_items: &'static [&'static str],
}

/// Every placeable patch needs an entry; generation fails for one without.
const CHECKS: [Check; 5] = [
    Check {
        patch: "menu-loader",
        system_items: &[],
    },
    Check {
        patch: "dump-rom",
        system_items: &["Dump ROM"],
    },
    Check {
        patch: "knob-dialog",
        system_items: &[],
    },
    Check {
        patch: "midi-sync",
        system_items: &["Send MIDI State"],
    },
    Check {
        patch: "din-mixer-control",
        system_items: &[],
    },
];

/// Files the dump writes, with their sizes.
/// The manifest comes first: the dump writes it last, so once it exists every
/// other file is complete.
const DUMP_FILES: [(&str, u64); 7] = [
    ("L6MAX_DUMP.TXT", 0),
    ("L6MAX_NOR.BIN", 0x20_0000),
    ("L6MAX_BOOTROM.BIN", 0x2_0000),
    ("L6MAX_ITCM.BIN", 0x8_0000),
    ("L6MAX_DTCM.BIN", 0x8_0000),
    ("L6MAX_OCRAM.BIN", 0x10_0000),
    ("L6MAX_SDRAM.BIN", 0x400_0000),
];

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
enum Placement {
    Firmware,
    Sd,
}

/// One generated case.
pub struct GeneratedCase {
    pub id: String,
    pub model: &'static str,
    pub scenario: String,
}

fn model_names(model: FirmwareModel) -> (&'static str, &'static str) {
    match model {
        FirmwareModel::L6 => ("L6", "l6"),
        FirmwareModel::L6max => ("L6max", "l6max"),
    }
}

fn declares(models: &[String], model: FirmwareModel) -> bool {
    models.iter().any(|name| name == model_names(model).0)
}

fn is_effect_package(package: &PatchPackage) -> bool {
    package.category == Some(PatchCategory::Effects)
}

/// Patches a case can place, in package order, with their placements.
fn placeable(model: FirmwareModel) -> Vec<(&'static PatchPackage, Vec<Placement>)> {
    patch_catalog()
        .iter()
        .filter(|package| package.kind == "patch" && !is_effect_package(package))
        .filter_map(|package| {
            let mut placements = Vec::new();
            if declares(&package.firmware_models, model) {
                placements.push(Placement::Firmware);
            }
            if declares(&package.sd_card_models, model) {
                placements.push(Placement::Sd);
            }
            (!placements.is_empty()).then_some((package, placements))
        })
        .collect()
}

/// Every slot assignment, the first being all stock.
fn effect_assignments(model: FirmwareModel) -> Vec<Vec<(&'static str, String)>> {
    let effects: Vec<String> = patch_catalog()
        .iter()
        .filter(|package| declares(&package.sd_card_models, model))
        .flat_map(|package| package.effects.iter().map(|effect| effect.id.clone()))
        .collect();
    let mut assignments = vec![Vec::new()];
    if effects.is_empty() {
        return assignments;
    }
    for slot in STOCK_EFFECT_SLOTS {
        let mut next = Vec::new();
        for assignment in &assignments {
            next.push(assignment.clone());
            for effect in &effects {
                let mut extended = assignment.clone();
                extended.push((slot, effect.clone()));
                next.push(extended);
            }
        }
        assignments = next;
    }
    assignments
}

/// Packages an effect assignment adds to the SD build: the router and the
/// packages that provide the chosen effects.
fn effect_packages(assignment: &[(&str, String)]) -> BTreeSet<String> {
    let mut packages = BTreeSet::new();
    if assignment.is_empty() {
        return packages;
    }
    packages.insert(EFFECT_REPLACEMENT_ROUTER_PACKAGE.to_owned());
    for (_, effect) in assignment {
        if let Some(package) = patch_catalog().iter().find(|package| {
            package
                .effects
                .iter()
                .any(|candidate| &candidate.id == effect)
        }) {
            packages.insert(package.name.clone());
        }
    }
    packages
}

fn closure(names: &[String]) -> Result<BTreeSet<String>, String> {
    Ok(resolve_patch_dependencies(names)?.into_iter().collect())
}

fn in_package_order(names: &BTreeSet<String>) -> Vec<String> {
    let mut ordered: Vec<String> = names.iter().cloned().collect();
    ordered.sort_by_key(|name| patch_package(name).map_or(u32::MAX, |package| package.order));
    ordered
}

/// One combination of placements and effect slots.
#[derive(Debug)]
struct Combination {
    firmware: Vec<String>,
    sd: Vec<String>,
    effects: Vec<(&'static str, String)>,
}

impl Combination {
    fn installs(&self, patch: &str) -> bool {
        self.firmware
            .iter()
            .chain(&self.sd)
            .any(|name| name == patch)
    }
    fn id(&self, model: &str) -> String {
        let mut parts = vec![model.to_owned()];
        let mut placed: Vec<(&String, &str)> = self
            .firmware
            .iter()
            .map(|name| (name, "fw"))
            .chain(self.sd.iter().map(|name| (name, "sd")))
            .collect();
        placed.sort_by_key(|(name, _)| patch_package(name).map_or(u32::MAX, |p| p.order));
        parts.extend(placed.iter().map(|(name, at)| format!("{name}.{at}")));
        parts.extend(
            self.effects
                .iter()
                .map(|(slot, effect)| format!("{slot}.{effect}")),
        );
        format!("{PREFIX}{}", parts.join("_"))
    }
}

/// Every valid combination for a model, in a stable order.
fn combinations(model: FirmwareModel) -> Result<Vec<Combination>, String> {
    let patches = placeable(model);
    let mut choices: Vec<Vec<Option<Placement>>> = vec![Vec::new()];
    for (_, placements) in &patches {
        let mut next = Vec::new();
        for prefix in &choices {
            for choice in std::iter::once(None).chain(placements.iter().copied().map(Some)) {
                let mut extended = prefix.clone();
                extended.push(choice);
                next.push(extended);
            }
        }
        choices = next;
    }
    let mut result = Vec::new();
    for choice in &choices {
        let pick = |placement: Placement| -> Vec<String> {
            patches
                .iter()
                .zip(choice)
                .filter(|(_, chosen)| **chosen == Some(placement))
                .map(|((package, _), _)| package.name.clone())
                .collect()
        };
        let firmware = pick(Placement::Firmware);
        let sd = pick(Placement::Sd);
        if closure(&firmware)? != firmware.iter().cloned().collect() {
            continue;
        }
        for effects in effect_assignments(model) {
            if firmware.is_empty() && sd.is_empty() && effects.is_empty() {
                continue;
            }
            let added = effect_packages(&effects);
            let mut requested = sd.clone();
            requested.extend(added.iter().cloned());
            let sd_closure = closure(&requested)?;
            let named: BTreeSet<String> = sd.iter().cloned().chain(added).collect();
            if sd_closure != named || sd_closure.iter().any(|name| firmware.contains(name)) {
                continue;
            }
            result.push(Combination {
                firmware: firmware.clone(),
                sd: sd.clone(),
                effects,
            });
        }
    }
    Ok(result)
}

fn quoted_list(names: &[String]) -> String {
    let items: Vec<String> = names.iter().map(|name| format!("{name:?}")).collect();
    format!("[{}]", items.join(", "))
}

fn step(text: &mut String, body: &str) {
    text.push_str("\n[[step]]\n");
    text.push_str(body.trim());
    text.push('\n');
}

fn press(text: &mut String, control: &str, times: usize) {
    for _ in 0..times {
        step(text, &format!("action = \"press\"\ncontrol = {control:?}"));
    }
}

fn delay(text: &mut String, guest_ms: u32) {
    step(text, &format!("action = \"delay\"\nguest_ms = {guest_ms}"));
}

/// Patches rows and System items menu-loader shows for a combination.
fn menu_rows(combination: &Combination) -> Result<(Vec<String>, Vec<String>), String> {
    let mut rows = Vec::new();
    // A case with SD modules runs the Patch loader, installed in firmware
    // before every other patch.
    if !combination.sd.is_empty() || !combination.effects.is_empty() {
        let loader = patch_package("patch-loader").ok_or("unknown patch patch-loader")?;
        rows.push(format!("{} {}", loader.name, loader.version));
    }
    let mut items: Vec<String> = STOCK_SYSTEM_MENU.iter().map(|s| (*s).to_owned()).collect();
    items.push("Patches".into());
    let mut sd_members = combination.sd.clone();
    sd_members.extend(effect_packages(&combination.effects));
    let sd_members = in_package_order(&sd_members.into_iter().collect());
    for (names, suffix) in [(&combination.firmware, ""), (&sd_members, " (SD)")] {
        for name in in_package_order(&names.iter().cloned().collect()) {
            let package = patch_package(&name).ok_or(format!("unknown patch {name}"))?;
            rows.push(format!("{} {}{suffix}", package.name, package.version));
        }
        for name in in_package_order(&names.iter().cloned().collect()) {
            if let Some(check) = CHECKS.iter().find(|check| check.patch == name) {
                items.extend(check.system_items.iter().map(|item| (*item).to_owned()));
            }
        }
    }
    Ok((rows, items))
}

/// The steps that check every installed patch on L6max.
fn l6max_checks(text: &mut String, combination: &Combination) -> Result<(), String> {
    let installs = |patch: &str| combination.installs(patch);
    let midi = installs("midi-sync");
    if !combination.sd.is_empty() {
        step(text, "action = \"expect-sd-patch\"\nloaded = true");
    }
    if installs("knob-dialog") {
        step(
            text,
            "action = \"knob-dialog-popup\"\nguest_timeout_ms = 15000\nwall_timeout_s = 120",
        );
    }
    if !combination.effects.is_empty() {
        step(text, "action = \"select-mode\"\nmode = \"efx\"");
        for (index, slot) in STOCK_EFFECT_SLOTS.iter().enumerate() {
            if index > 0 {
                press(text, "efx-select", 1);
            }
            delay(text, 1000);
            let effect = combination
                .effects
                .iter()
                .find(|(assigned, _)| assigned == slot)
                .map(|(_, effect)| effect);
            let mut body = format!(
                "action = \"effect-replacement-state\"\nexpected_effect_type = {}\nexpected_processor_type = {}",
                index + 1,
                if effect.is_some() { index + 1 } else { 0 }
            );
            if let Some(effect) = effect {
                let _ = write!(
                    body,
                    "\nexpected_effect = {effect:?}\nrequire_output = true"
                );
            }
            step(text, &body);
        }
        step(text, "action = \"select-mode\"\nmode = \"level\"");
    }
    if midi {
        step(
            text,
            "action = \"direct-usb-midi-round-trip\"\nguest_timeout_ms = 60000\nwall_timeout_s = 180",
        );
        step(
            text,
            "action = \"host-midi-sync\"\nguest_timeout_ms = 30000\nwall_timeout_s = 120",
        );
    }
    if installs("din-mixer-control") {
        step(
            text,
            "action = \"l6max-din-mixer-control\"\nexpect_din_ring_change = true\nguest_timeout_ms = 10000\nwall_timeout_s = 120",
        );
    }
    if installs("menu-loader") {
        let (rows, items) = menu_rows(combination)?;
        // Let earlier input finish: a Knob Dialog window opened by an
        // encoder turn, or a just-tapped button, would absorb the press.
        delay(text, 3000);
        // Menu, then System at main-menu index 7.
        press(text, "recorder-function-1", 1);
        delay(text, 1500);
        step(
            text,
            &format!(
                "action = \"expect-menu\"\nmenu = \"system\"\nitems = {}",
                quoted_list(&items)
            ),
        );
        step(
            text,
            &format!(
                "action = \"expect-menu\"\nmenu = \"system\"\npath = [\"Patches\"]\nitems = {}",
                quoted_list(&rows)
            ),
        );
        press(text, "recorder-function-3", 7);
        delay(text, 500);
        press(text, "recorder-function-4", 1);
        delay(text, 1000);
        // Run each item's action in menu order, moving down from the top.
        let mut actions: Vec<(usize, &str)> = Vec::new();
        for (patch, title) in [("midi-sync", "Send MIDI State"), ("dump-rom", "Dump ROM")] {
            if installs(patch) && (patch != "midi-sync" || midi) {
                let index = items
                    .iter()
                    .position(|item| item == title)
                    .ok_or(format!("System menu has no {title}"))?;
                actions.push((index, title));
            }
        }
        actions.sort();
        let mut cursor = 0;
        for (index, title) in actions {
            press(text, "recorder-function-3", index - cursor);
            delay(text, 500);
            press(text, "recorder-function-4", 1);
            cursor = index;
            match title {
                "Send MIDI State" => step(
                    text,
                    "action = \"host-midi-sync\"\nrequest = false\nguest_timeout_ms = 30000\nwall_timeout_s = 120",
                ),
                _ => {
                    for (file, size) in DUMP_FILES {
                        let mut body = format!(
                            "action = \"expect-card-entry\"\npath = {file:?}\nguest_timeout_ms = 900000\nwall_timeout_s = 900"
                        );
                        if file == "L6MAX_NOR.BIN" {
                            body.push_str("\nmatches_state = \"main-nor.bin\"");
                        } else if size > 0 {
                            let _ = write!(body, "\nsize = {size}");
                        }
                        step(text, &body);
                    }
                }
            }
        }
    }
    Ok(())
}

fn header(text: &mut String, id: &str, model: &str, state: &str, card: bool) {
    let _ = writeln!(text, "# Generated by l6test for the `{SUITE}` suite.");
    let _ = writeln!(
        text,
        "version = 1\nname = {id:?}\nmodels = [{model:?}]\nbackends = [\"headless\"]\nstate = {state:?}"
    );
    // The empty card is the default.
    if !card {
        text.push_str("card = \"none\"\n");
    }
}

fn combination_case(
    model: FirmwareModel,
    combination: &Combination,
) -> Result<GeneratedCase, String> {
    let (_, name) = model_names(model);
    let id = combination.id(name);
    for patch in combination.firmware.iter().chain(&combination.sd) {
        if !CHECKS.iter().any(|check| check.patch == patch) {
            return Err(format!("patch {patch} has no check in the patch matrix"));
        }
    }
    let mut text = String::new();
    let state = if model == FirmwareModel::L6max {
        "patch-matrix"
    } else {
        "setup-complete"
    };
    header(&mut text, &id, name, state, true);
    if !combination.firmware.is_empty() {
        let _ = writeln!(text, "patches = {}", quoted_list(&combination.firmware));
    }
    if !combination.sd.is_empty() || !combination.effects.is_empty() {
        let _ = writeln!(text, "sd_patches = {}", quoted_list(&combination.sd));
    }
    if !combination.effects.is_empty() {
        let slots: Vec<String> = combination
            .effects
            .iter()
            .map(|(slot, effect)| format!("{slot} = {effect:?}"))
            .collect();
        let _ = writeln!(
            text,
            "sd_bundle_mode = \"composed-module\"\neffect_replacements = {{ {} }}",
            slots.join(", ")
        );
        text.push_str(
            "\n[[audio_input]]\nchannel = 1\nloop-tone = { hz = 440.0, seconds = 1.0, amplitude = 0.1 }\n",
        );
        step(
            &mut text,
            "action = \"audio-input-transport\"\ncommand = \"play\"",
        );
    }
    step(
        &mut text,
        "action = \"wait\"\nuntil = \"mixer-ready\"\nguest_timeout_ms = 60000\nwall_timeout_s = 300",
    );
    step(
        &mut text,
        "action = \"delay\"\nguest_ms = 8000\nguest_timeout_ms = 20000\nwall_timeout_s = 300",
    );
    match model {
        FirmwareModel::L6max => l6max_checks(&mut text, combination)?,
        FirmwareModel::L6 => l6_checks(&mut text, combination),
    }
    Ok(GeneratedCase {
        id,
        model: name,
        scenario: text,
    })
}

fn l6_checks(text: &mut String, combination: &Combination) {
    if !combination.sd.is_empty() {
        step(text, "action = \"expect-sd-patch\"\nloaded = true");
    }
    if combination.installs("midi-sync") {
        step(
            text,
            "action = \"direct-usb-midi-round-trip\"\nguest_timeout_ms = 60000\nwall_timeout_s = 180",
        );
        step(
            text,
            "action = \"host-midi-sync\"\nguest_timeout_ms = 30000\nwall_timeout_s = 120",
        );
    }
}

/// One case per patch and delivery method the browser patcher and the
/// website offer.
fn delivery_cases(model: FirmwareModel) -> Result<Vec<GeneratedCase>, String> {
    let (_, name) = model_names(model);
    let mut cases = Vec::new();
    for package in patch_catalog().iter().filter(|p| p.kind == "patch") {
        let patch = package.name.as_str();
        let closure = in_package_order(&closure(&[package.name.clone()])?);
        let combination = Combination {
            firmware: closure.clone(),
            sd: Vec::new(),
            effects: Vec::new(),
        };
        if declares(&package.wasm_models, model) {
            // Installed by the engine on a device running the stock firmware.
            let id = format!("{PREFIX}{name}_installed_{patch}");
            let mut text = String::new();
            header(
                &mut text,
                &id,
                name,
                if model == FirmwareModel::L6max {
                    "patch-matrix"
                } else {
                    "setup-complete"
                },
                true,
            );
            let _ = writeln!(text, "installed_patches = {}", quoted_list(&closure));
            step(
                &mut text,
                "action = \"wait\"\nuntil = \"mixer-ready\"\nguest_timeout_ms = 60000\nwall_timeout_s = 300",
            );
            step(
                &mut text,
                "action = \"delay\"\nguest_ms = 8000\nguest_timeout_ms = 20000\nwall_timeout_s = 300",
            );
            match model {
                FirmwareModel::L6max => l6max_checks(&mut text, &combination)?,
                FirmwareModel::L6 => l6_checks(&mut text, &combination),
            }
            cases.push(GeneratedCase {
                id,
                model: name,
                scenario: text,
            });
            // Installed by a firmware update built with the browser patcher.
            let id = format!("{PREFIX}{name}_update_{patch}");
            let mut text = String::new();
            let _ = writeln!(text, "# Generated by l6test for the `{SUITE}` suite.");
            let _ = writeln!(
                text,
                "version = 1\nname = {id:?}\nmodels = [{name:?}]\nbackends = [\"headless\"]\nstate = \"none\"\ncard = \"none\"\nupdate_patches = {{ {name} = {} }}",
                quoted_list(&closure)
            );
            step(
                &mut text,
                "action = \"firmware-update\"\nguest_timeout_ms = 300000\nwall_timeout_s = 1200",
            );
            cases.push(GeneratedCase {
                id,
                model: name,
                scenario: text,
            });
        }
        if declares(&package.website_sd_models, model) {
            // The hosted module on the card, with the loader the engine installs.
            let id = format!("{PREFIX}{name}_hosted-sd_{patch}");
            let mut text = String::new();
            header(
                &mut text,
                &id,
                name,
                if model == FirmwareModel::L6max {
                    "patch-matrix"
                } else {
                    "setup-complete"
                },
                true,
            );
            let _ = writeln!(text, "installed_sd_patches = [{patch:?}]");
            step(
                &mut text,
                "action = \"wait\"\nuntil = \"mixer-ready\"\nguest_timeout_ms = 60000\nwall_timeout_s = 300",
            );
            step(
                &mut text,
                "action = \"delay\"\nguest_ms = 8000\nguest_timeout_ms = 20000\nwall_timeout_s = 300",
            );
            let hosted = Combination {
                firmware: Vec::new(),
                sd: Vec::new(),
                effects: Vec::new(),
            };
            // The hosted module is not an engine-built SD bundle, so the
            // loader-state check does not apply; the patch checks do.
            let installed = Combination {
                firmware: vec![package.name.clone()],
                ..hosted
            };
            match model {
                FirmwareModel::L6max => l6max_checks(&mut text, &installed)?,
                FirmwareModel::L6 => l6_checks(&mut text, &installed),
            }
            cases.push(GeneratedCase {
                id,
                model: name,
                scenario: text,
            });
        }
    }
    Ok(cases)
}

/// Every generated case, for both models.
pub fn generate() -> Result<Vec<GeneratedCase>, String> {
    let mut cases = Vec::new();
    for model in [FirmwareModel::L6, FirmwareModel::L6max] {
        for combination in combinations(model)? {
            cases.push(combination_case(model, &combination)?);
        }
        cases.extend(delivery_cases(model)?);
    }
    Ok(cases)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn combinations_cover_every_placement_and_effect_assignment_once() {
        let cases = generate().unwrap();
        let ids: BTreeSet<&str> = cases.iter().map(|case| case.id.as_str()).collect();
        assert_eq!(ids.len(), cases.len(), "case IDs are unique");
        let l6max = combinations(FirmwareModel::L6max).unwrap();
        // knob-dialog and midi-sync are absent, in firmware or on SD, and
        // din-mixer-control absent or in firmware: 18 sets, 6 of them with
        // knob-dialog on SD. menu-loader and dump-rom go in firmware only, and
        // dump-rom needs menu-loader: 3 choices. The empty set is not a case.
        // Two effect slots each take stock, Plateau or Velvet, and a
        // replacement needs knob-dialog on SD: 8 replacement assignments per
        // such set.
        assert_eq!(l6max.len(), (18 + 8 * 6) * 3 - 1);
        assert_eq!(combinations(FirmwareModel::L6).unwrap().len(), 2);
        assert!(
            l6max
                .iter()
                .all(|c| !(c.firmware.contains(&"dump-rom".into())
                    && !c.firmware.contains(&"menu-loader".into())))
        );
        assert!(
            l6max
                .iter()
                .all(|c| c.effects.is_empty() || c.sd.contains(&"knob-dialog".into()))
        );
    }

    #[test]
    fn every_generated_scenario_loads() {
        for case in generate().unwrap() {
            smoke_core::Scenario::from_text(&case.scenario)
                .unwrap_or_else(|error| panic!("{}: {error}\n{}", case.id, case.scenario));
        }
    }
}
