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
use serde::Deserialize;
use std::collections::BTreeSet;
use std::fmt::Write as _;

/// Suite that runs every generated case: the full product of placements and
/// effect assignments.
pub const SUITE: &str = "patches";
/// Suite with a covering subset in which every pair of factor values (one
/// patch's placement, one slot's effect) appears in some case.
pub const PAIRWISE_SUITE: &str = "patches-pairwise";
/// Suite with each placement and each effect assignment once, with as few
/// other patches as its dependencies allow.
pub const SINGLE_SUITE: &str = "patches-single";
/// Case and scenario-file prefix.
pub const PREFIX: &str = "patches-";

/// The suite's check order, from the repository root.
const SUITE_FILE: &str = "tests/emulator/patches.toml";
/// A patch's checks, from its directory.
const CHECKS_FILE: &str = "tests/emulator/checks.toml";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SuiteFile {
    version: u32,
    check_order: Vec<String>,
}

/// What the suite checks when a patch is installed: a patch's
/// `tests/emulator/checks.toml`. Paths are from the repository root.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PatchChecks {
    version: u32,
    /// A fragment of steps that check the patch once the mixer is ready.
    check: Option<String>,
    /// Items the patch adds to the System menu, in order.
    #[serde(default)]
    system_items: Vec<String>,
    /// Steps that run one of the patch's System menu items and check the result.
    #[serde(default)]
    menu_action: Vec<MenuAction>,
    /// The check walks the System menu (menu-loader): it expects
    /// `stock_system_items`, then Patches and every installed patch's items,
    /// and runs every installed patch's menu actions.
    #[serde(default)]
    walks_menus: bool,
    #[serde(default)]
    stock_system_items: Vec<String>,
    /// The check runs whenever effect slots are reassigned (efx-loader) and
    /// checks each slot's effect.
    #[serde(default)]
    checks_effect_slots: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MenuAction {
    item: String,
    steps: String,
}

/// Every patch's checks, in the order cases run them.
struct Checks {
    order: Vec<(String, PatchChecks)>,
}

impl Checks {
    fn load() -> Result<Self, String> {
        let root = l6max_host::paths::repository_root();
        let read = |path: &std::path::Path| {
            std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))
        };
        let suite: SuiteFile = toml::from_str(&read(&root.join(SUITE_FILE))?)
            .map_err(|e| format!("{SUITE_FILE}: {e}"))?;
        if suite.version != 1 {
            return Err(format!("{SUITE_FILE}: unsupported version"));
        }
        let mut order = Vec::new();
        for name in &suite.check_order {
            let path = root.join("patches").join(name).join(CHECKS_FILE);
            let checks: PatchChecks =
                toml::from_str(&read(&path)?).map_err(|e| format!("{}: {e}", path.display()))?;
            if checks.version != 1 {
                return Err(format!("{}: unsupported version", path.display()));
            }
            order.push((name.clone(), checks));
        }
        for package in patch_catalog() {
            let listed = suite.check_order.contains(&package.name);
            if root
                .join("patches")
                .join(&package.name)
                .join(CHECKS_FILE)
                .is_file()
                != listed
            {
                return Err(format!(
                    "{SUITE_FILE}: check_order must list exactly the patches with a {CHECKS_FILE}; {} differs",
                    package.name
                ));
            }
        }
        Ok(Self { order })
    }

    fn get(&self, patch: &str) -> Option<&PatchChecks> {
        self.order
            .iter()
            .find(|(name, _)| name == patch)
            .map(|(_, checks)| checks)
    }
}

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
    /// The generated suites that run it; every case is in [`SUITE`].
    pub suites: Vec<&'static str>,
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
fn menu_rows(
    combination: &Combination,
    checks: &Checks,
    stock_system_items: &[String],
) -> Result<(Vec<String>, Vec<String>), String> {
    let mut rows = Vec::new();
    // A case with SD modules runs the Patch loader, installed in firmware
    // before every other patch.
    if !combination.sd.is_empty() || !combination.effects.is_empty() {
        let loader = patch_package("patch-loader").ok_or("unknown patch patch-loader")?;
        rows.push(format!("{} {}", loader.name, loader.version));
    }
    let mut items = stock_system_items.to_vec();
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
            if let Some(patch) = checks.get(&name) {
                items.extend(patch.system_items.iter().cloned());
            }
        }
    }
    Ok((rows, items))
}

/// menu-loader's reviewed display images, from the repository root.
const MENU_REFERENCES: &str = "patches/menu-loader/tests/emulator/references";
/// Rows of a menu the display shows at once.
const VISIBLE_ROWS: usize = 3;

/// The reviewed image of System > Patches as it opens, named after the rows
/// it shows (`patch-loader-1.0.0__knob-dialog-1.0.0-sd.png`) and, when the
/// list is longer than the display, how many rows the scroll bar spans
/// (`…__of-4.png`).
fn patches_screen_reference(rows: &[String]) -> String {
    let mut name = rows
        .iter()
        .take(VISIBLE_ROWS)
        .map(|row| row.to_lowercase().replace(" (sd)", "-sd").replace(' ', "-"))
        .collect::<Vec<_>>()
        .join("__");
    if rows.len() > VISIBLE_ROWS {
        name.push_str(&format!("__of-{}", rows.len()));
    }
    format!("{MENU_REFERENCES}/patches-suite/{name}.png")
}

fn include(text: &mut String, path: &str) {
    step(text, &format!("action = \"include\"\npath = {path:?}"));
}

/// The steps that check every patch a combination installs, in check order.
fn checks(text: &mut String, combination: &Combination, all: &Checks) -> Result<(), String> {
    if !combination.sd.is_empty() {
        step(text, "action = \"expect-sd-patch\"\nloaded = true");
    }
    for (name, patch) in &all.order {
        let active = if patch.checks_effect_slots {
            !combination.effects.is_empty()
        } else {
            combination.installs(name)
        };
        if !active {
            continue;
        }
        if let Some(path) = &patch.check {
            include(text, path);
        }
        if patch.checks_effect_slots {
            effect_slot_checks(text, combination);
        }
        if patch.walks_menus {
            menu_checks(text, combination, all, &patch.stock_system_items)?;
        }
    }
    Ok(())
}

/// Each stock effect slot, in EFX mode, runs its assigned effect with output,
/// or its stock effect.
fn effect_slot_checks(text: &mut String, combination: &Combination) {
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

/// Open the System menu, expect its items and Patches rows, and run every
/// installed patch's menu action in menu order.
fn menu_checks(
    text: &mut String,
    combination: &Combination,
    all: &Checks,
    stock_system_items: &[String],
) -> Result<(), String> {
    let (rows, items) = menu_rows(combination, all, stock_system_items)?;
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
    // Open System > Patches and compare its rows as drawn; back in System
    // the cursor stays on Patches. Each item a patch adds comes after
    // Patches, so the actions below move down from there.
    let patches = items
        .iter()
        .position(|item| item == "Patches")
        .ok_or("System menu has no Patches")?;
    press(text, "recorder-function-3", patches);
    delay(text, 500);
    press(text, "recorder-function-4", 1);
    delay(text, 1000);
    step(
        text,
        &format!(
            "action = \"expect-display-image\"\nimage = {:?}",
            patches_screen_reference(&rows)
        ),
    );
    press(text, "recorder-function-1", 1);
    delay(text, 1000);
    // Run each item's action in menu order, moving down from Patches.
    let mut actions: Vec<(usize, &str)> = Vec::new();
    for (name, patch) in &all.order {
        if !combination.installs(name) {
            continue;
        }
        for action in &patch.menu_action {
            let index = items
                .iter()
                .position(|item| *item == action.item)
                .ok_or(format!("System menu has no {}", action.item))?;
            actions.push((index, &action.steps));
        }
    }
    actions.sort();
    let mut cursor = patches;
    for (index, steps) in actions {
        press(text, "recorder-function-3", index - cursor);
        delay(text, 500);
        press(text, "recorder-function-4", 1);
        cursor = index;
        include(text, steps);
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
    all: &Checks,
) -> Result<GeneratedCase, String> {
    let (_, name) = model_names(model);
    let id = combination.id(name);
    for patch in combination.firmware.iter().chain(&combination.sd) {
        if all.get(patch).is_none() {
            return Err(format!(
                "patch {patch} has no patches/{patch}/{CHECKS_FILE} for the `{SUITE}` suite"
            ));
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
    checks(&mut text, combination, all)?;
    Ok(GeneratedCase {
        id,
        model: name,
        scenario: text,
        suites: vec![SUITE],
    })
}

/// One case per patch and delivery method the browser patcher and the
/// website offer.
fn delivery_cases(model: FirmwareModel, all: &Checks) -> Result<Vec<GeneratedCase>, String> {
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
            checks(&mut text, &combination, all)?;
            cases.push(GeneratedCase {
                id,
                model: name,
                scenario: text,
                suites: vec![SUITE, PAIRWISE_SUITE, SINGLE_SUITE],
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
                suites: vec![SUITE, PAIRWISE_SUITE, SINGLE_SUITE],
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
            checks(&mut text, &installed, all)?;
            cases.push(GeneratedCase {
                id,
                model: name,
                scenario: text,
                suites: vec![SUITE, PAIRWISE_SUITE, SINGLE_SUITE],
            });
        }
    }
    Ok(cases)
}

/// A combination as one value per factor: each placeable patch's placement
/// (`-`, `fw` or `sd`), then each stock slot's effect (`-` for stock).
fn factor_values(model: FirmwareModel, combination: &Combination) -> Vec<String> {
    let mut values = Vec::new();
    for (package, _) in placeable(model) {
        let name = &package.name;
        values.push(if combination.firmware.contains(name) {
            "fw".into()
        } else if combination.sd.contains(name) {
            "sd".into()
        } else {
            "-".into()
        });
    }
    for slot in STOCK_EFFECT_SLOTS {
        values.push(
            combination
                .effects
                .iter()
                .find(|(assigned, _)| *assigned == slot)
                .map_or("-".into(), |(_, effect)| effect.clone()),
        );
    }
    values
}

/// The factor-value tuples of `strength` factors (and every single value) a
/// combination covers.
fn tuples(values: &[String], strength: usize) -> Vec<Vec<(usize, &str)>> {
    let mut out: Vec<Vec<(usize, &str)>> = values
        .iter()
        .enumerate()
        .map(|(i, v)| vec![(i, v.as_str())])
        .collect();
    if strength >= 2 {
        for i in 0..values.len() {
            for j in i + 1..values.len() {
                out.push(vec![(i, values[i].as_str()), (j, values[j].as_str())]);
            }
        }
    }
    out
}

/// Indices of a subset of `combinations` that covers every tuple some valid
/// combination covers, chosen greedily (most new tuples first, earliest on a
/// tie), so the subset is stable.
fn covering(combinations: &[Vec<String>], strength: usize) -> BTreeSet<usize> {
    let mut uncovered: BTreeSet<Vec<(usize, &str)>> = combinations
        .iter()
        .flat_map(|values| tuples(values, strength))
        .collect();
    let mut chosen = BTreeSet::new();
    while !uncovered.is_empty() {
        let (best, _) = combinations
            .iter()
            .enumerate()
            .filter(|(index, _)| !chosen.contains(index))
            .map(|(index, values)| {
                let new = tuples(values, strength)
                    .iter()
                    .filter(|tuple| uncovered.contains(*tuple))
                    .count();
                (index, new)
            })
            .fold(
                (0, 0),
                |best, next| if next.1 > best.1 { next } else { best },
            );
        for tuple in tuples(&combinations[best], strength) {
            uncovered.remove(&tuple);
        }
        chosen.insert(best);
    }
    chosen
}

/// For each factor value other than absent or stock, the earliest
/// combination holding it with the fewest other such values.
fn singles(combinations: &[Vec<String>]) -> BTreeSet<usize> {
    let mut chosen = BTreeSet::new();
    let set = |values: &Vec<String>| values.iter().filter(|v| *v != "-").count();
    let factors = combinations.first().map_or(0, Vec::len);
    for factor in 0..factors {
        let mut seen: Vec<&str> = Vec::new();
        for values in combinations {
            let value = values[factor].as_str();
            if value == "-" || seen.contains(&value) {
                continue;
            }
            seen.push(value);
            let best = combinations
                .iter()
                .enumerate()
                .filter(|(_, other)| other[factor] == value)
                .min_by_key(|(index, other)| (set(other), *index))
                .map(|(index, _)| index);
            chosen.extend(best);
        }
    }
    chosen
}

/// Every generated case, for both models.
pub fn generate() -> Result<Vec<GeneratedCase>, String> {
    let all = Checks::load()?;
    let mut cases = Vec::new();
    for model in [FirmwareModel::L6, FirmwareModel::L6max] {
        let combinations = combinations(model)?;
        let values: Vec<Vec<String>> = combinations
            .iter()
            .map(|combination| factor_values(model, combination))
            .collect();
        let pairwise = covering(&values, 2);
        let single = singles(&values);
        for (index, combination) in combinations.iter().enumerate() {
            let mut case = combination_case(model, combination, &all)?;
            if pairwise.contains(&index) {
                case.suites.push(PAIRWISE_SUITE);
            }
            if single.contains(&index) {
                case.suites.push(SINGLE_SUITE);
            }
            cases.push(case);
        }
        cases.extend(delivery_cases(model, &all)?);
    }
    Ok(cases)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pairwise_covers_every_pair_and_single_every_value() {
        for model in [FirmwareModel::L6, FirmwareModel::L6max] {
            let values: Vec<Vec<String>> = combinations(model)
                .unwrap()
                .iter()
                .map(|c| factor_values(model, c))
                .collect();
            let pairs = |set: &mut dyn Iterator<Item = &Vec<String>>| {
                let mut out = BTreeSet::new();
                for v in set {
                    for i in 0..v.len() {
                        out.insert((i, v[i].clone(), i, v[i].clone()));
                        for j in i + 1..v.len() {
                            out.insert((i, v[i].clone(), j, v[j].clone()));
                        }
                    }
                }
                out
            };
            let pairwise = covering(&values, 2);
            assert_eq!(
                pairs(&mut pairwise.iter().map(|&i| &values[i])),
                pairs(&mut values.iter())
            );
            let single = singles(&values);
            for v in &values {
                for (factor, value) in v.iter().enumerate().filter(|(_, v)| *v != "-") {
                    assert!(single.iter().any(|&i| &values[i][factor] == value));
                }
            }
            assert!(single.len() <= pairwise.len() && pairwise.len() <= values.len());
            if model == FirmwareModel::L6max {
                assert!(pairwise.len() * 5 < values.len());
            }
        }
    }

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
