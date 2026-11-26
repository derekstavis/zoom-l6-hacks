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

use l6fw::Model;
use patch_core::{
    patch_catalog, patch_package, resolve_patch_dependencies, PackageKind, PatchCategory,
    PatchPackage, EFFECT_REPLACEMENT_ROUTER_PACKAGE, STOCK_EFFECT_SLOTS,
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
    pub campaign: Vec<crate::catalog::CampaignMember>,
    /// The generated suites that run it; every case is in [`SUITE`].
    pub suites: Vec<&'static str>,
}

fn is_effect_package(package: &PatchPackage) -> bool {
    package.category == Some(PatchCategory::Effects)
}

/// Patches a case can place, in package order, with their placements.
fn placeable(model: Model) -> Vec<(&'static PatchPackage, Vec<Placement>)> {
    patch_catalog()
        .iter()
        .filter(|package| package.kind == PackageKind::Patch && !is_effect_package(package))
        .filter_map(|package| {
            let mut placements = Vec::new();
            if package.firmware_models.contains(&model) {
                placements.push(Placement::Firmware);
            }
            if package.sd_card_models.contains(&model) {
                placements.push(Placement::Sd);
            }
            (!placements.is_empty()).then_some((package, placements))
        })
        .collect()
}

/// Every slot assignment, the first being all stock.
fn effect_assignments(model: Model) -> Vec<Vec<(&'static str, String)>> {
    let effects: Vec<String> = patch_catalog()
        .iter()
        .filter(|package| package.sd_card_models.contains(&model))
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
fn combinations(model: Model) -> Result<Vec<Combination>, String> {
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

/// L6max must finish rendering the recorder before patch inputs can work.
/// L6 has no display, so its other delivery paths retain their startup wait.
fn startup_readiness(text: &mut String, model: Model) {
    let path = if model == Model::L6max {
        "tests/emulator/fragments/patch-matrix-l6max-home.toml"
    } else {
        "tests/emulator/fragments/patch-matrix-l6-startup.toml"
    };
    include(text, path);
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
        step(text, "action = \"expect-sd-loader\"");
        step(text, "action = \"expect-sd-module\"\nloaded = true");
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

/// Expected state for one physical stock slot after selecting EFX mode.
#[derive(Debug, PartialEq, Eq)]
struct EffectSlotPlan<'a> {
    slot_index: usize,
    expected_effect_type: usize,
    expected_processor_type: usize,
    effect: Option<&'a str>,
    require_output: bool,
}

fn effect_slot_plan(combination: &Combination) -> Vec<EffectSlotPlan<'_>> {
    STOCK_EFFECT_SLOTS
        .iter()
        .enumerate()
        .map(|(index, slot)| {
            let effect = combination
                .effects
                .iter()
                .find(|(assigned, _)| assigned == slot)
                .map(|(_, effect)| effect.as_str());
            EffectSlotPlan {
                slot_index: index,
                expected_effect_type: index + 1,
                expected_processor_type: if effect.is_some() { index + 1 } else { 0 },
                effect,
                require_output: effect.is_some(),
            }
        })
        .collect()
}

/// Each stock effect slot, in EFX mode, runs its assigned effect with output,
/// or its stock effect.
fn effect_slot_checks(text: &mut String, combination: &Combination) {
    step(text, "action = \"select-mode\"\nmode = \"efx\"");
    for slot in effect_slot_plan(combination) {
        if slot.slot_index > 0 {
            press(text, "efx-select", 1);
        }
        let mut body = format!(
            "expected_effect_type = {}\nexpected_processor_type = {}",
            slot.expected_effect_type, slot.expected_processor_type
        );
        if let Some(effect) = slot.effect {
            let _ = write!(
                body,
                "\nexpected_effect = {effect:?}\nrequire_output = {}",
                slot.require_output
            );
        }
        step(text, &format!("action = \"wait-effect-slot\"\n{body}"));
        step(
            text,
            &format!("action = \"effect-replacement-state\"\n{body}"),
        );
    }
    step(text, "action = \"select-mode\"\nmode = \"level\"");
}

/// The menu items and ordered patch-specific actions for a combination.
#[derive(Debug, PartialEq, Eq)]
struct MenuPlan<'a> {
    rows: Vec<String>,
    items: Vec<String>,
    patches_index: usize,
    actions: Vec<MenuActionPlan<'a>>,
}

#[derive(Debug, PartialEq, Eq)]
struct MenuActionPlan<'a> {
    index: usize,
    steps: &'a str,
    send_midi_with_dump_rom: bool,
    wait_for_dump_notification: bool,
}

fn menu_plan<'a>(
    combination: &Combination,
    all: &'a Checks,
    stock_system_items: &[String],
) -> Result<MenuPlan<'a>, String> {
    let (rows, items) = menu_rows(combination, all, stock_system_items)?;
    let patches_index = items
        .iter()
        .position(|item| item == "Patches")
        .ok_or("System menu has no Patches")?;
    let mut actions = Vec::new();
    for (name, patch) in &all.order {
        if !combination.installs(name) {
            continue;
        }
        for action in &patch.menu_action {
            let index = items
                .iter()
                .position(|item| *item == action.item)
                .ok_or(format!("System menu has no {}", action.item))?;
            actions.push(MenuActionPlan {
                index,
                steps: &action.steps,
                send_midi_with_dump_rom: items[index] == "Send MIDI State"
                    && combination.installs("dump-rom"),
                wait_for_dump_notification: items[index] == "Dump ROM",
            });
        }
    }
    // Match the previous tuple sort, including its fragment-path tiebreaker.
    actions.sort_by(|a, b| (a.index, a.steps).cmp(&(b.index, b.steps)));
    Ok(MenuPlan {
        rows,
        items,
        patches_index,
        actions,
    })
}

/// Open the System menu, expect its items and Patches rows, and run every
/// installed patch's menu action in menu order.
fn menu_checks(
    text: &mut String,
    combination: &Combination,
    all: &Checks,
    stock_system_items: &[String],
) -> Result<(), String> {
    let plan = menu_plan(combination, all, stock_system_items)?;
    let rows = &plan.rows;
    let items = &plan.items;
    // Open the menu from a reviewed, settled recorder screen.
    include(text, "tests/emulator/fragments/patch-matrix-menu-open.toml");
    step(text, "action = \"capture-menu\"\nmenu = \"system\"");
    step(
        text,
        &format!(
            "action = \"expect-menu-items\"\nitems = {}",
            quoted_list(&items)
        ),
    );
    step(
        text,
        "action = \"capture-menu\"\nmenu = \"system\"\npath = [\"Patches\"]",
    );
    step(
        text,
        &format!(
            "action = \"expect-menu-items\"\nitems = {}",
            quoted_list(&rows)
        ),
    );
    press(text, "recorder-function-3", 7);
    step(
        text,
        "action = \"expect-display-image\"\nimage = \"tests/smoke/images/l6max-patch-menu/main-system.png\"\nguest_timeout_ms = 30000\nwall_timeout_s = 300",
    );
    press(text, "recorder-function-4", 1);
    step(
        text,
        &format!(
            "action = \"expect-display-image\"\nimage = \"tests/smoke/images/l6max-patch-menu/system-first-{}.png\"\nguest_timeout_ms = 30000\nwall_timeout_s = 300",
            items.len()
        ),
    );
    // Open System > Patches and compare its rows as drawn; back in System
    // the cursor stays on Patches. Each item a patch adds comes after
    // Patches, so the actions below move down from there.
    let patches = plan.patches_index;
    press(text, "recorder-function-3", patches);
    step(
        text,
        &format!(
            "action = \"expect-display-image\"\nimage = \"tests/smoke/images/l6max-patch-menu/system-patches-{}.png\"\nguest_timeout_ms = 30000\nwall_timeout_s = 300",
            items.len()
        ),
    );
    press(text, "recorder-function-4", 1);
    step(
        text,
        &format!(
            "action = \"expect-display-image\"\nimage = {:?}",
            patches_screen_reference(&rows)
        ),
    );
    press(text, "recorder-function-1", 1);
    step(
        text,
        &format!(
            "action = \"expect-display-image\"\nimage = \"tests/smoke/images/l6max-patch-menu/system-patches-{}.png\"\nguest_timeout_ms = 30000\nwall_timeout_s = 300",
            items.len()
        ),
    );
    // Run each item's action in menu order, moving down from Patches.
    let mut cursor = patches;
    for action in plan.actions {
        let index = action.index;
        press(text, "recorder-function-3", index - cursor);
        delay(text, 500);
        if action.send_midi_with_dump_rom {
            step(
                text,
                "action = \"expect-display-image\"\nimage = \"tests/smoke/images/l6max-patch-menu/system-send-midi-state-10.png\"\nguest_timeout_ms = 30000\nwall_timeout_s = 120",
            );
        }
        press(text, "recorder-function-4", 1);
        cursor = index;
        include(text, action.steps);
        if action.wait_for_dump_notification {
            // The file appears before the completion notification clears.
            include(
                text,
                "tests/emulator/fragments/patch-matrix-dump-notification-clear.toml",
            );
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
    model: Model,
    combination: &Combination,
    all: &Checks,
) -> Result<GeneratedCase, String> {
    let name = model.name();
    let id = combination.id(name);
    for patch in combination.firmware.iter().chain(&combination.sd) {
        if all.get(patch).is_none() {
            return Err(format!(
                "patch {patch} has no patches/{patch}/{CHECKS_FILE} for the `{SUITE}` suite"
            ));
        }
    }
    let mut text = String::new();
    let state = if model == Model::L6max {
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
    include(
        &mut text,
        "tests/emulator/fragments/patch-matrix-mixer-ready.toml",
    );
    // The L6 SD loader has its own completion signal in `checks`. L6max
    // also needs its rendered recorder screen before patch input checks.
    if model == Model::L6max || combination.sd.is_empty() {
        startup_readiness(&mut text, model);
    }
    checks(&mut text, combination, all)?;
    Ok(GeneratedCase {
        id,
        model: name,
        scenario: text,
        campaign: Vec::new(),
        suites: vec![SUITE],
    })
}

/// One case per patch and delivery method the browser patcher and the
/// website offer.
fn delivery_cases(model: Model, all: &Checks) -> Result<Vec<GeneratedCase>, String> {
    let name = model.name();
    let mut cases = Vec::new();
    for package in patch_catalog()
        .iter()
        .filter(|p| p.kind == PackageKind::Patch)
    {
        let patch = package.name.as_str();
        let closure = in_package_order(&closure(&[package.name.clone()])?);
        let combination = Combination {
            firmware: closure.clone(),
            sd: Vec::new(),
            effects: Vec::new(),
        };
        if package.wasm_models.contains(&model) {
            // Installed by the engine on a device running the stock firmware.
            let id = format!("{PREFIX}{name}_installed_{patch}");
            let mut text = String::new();
            header(
                &mut text,
                &id,
                name,
                if model == Model::L6max {
                    "patch-matrix"
                } else {
                    "setup-complete"
                },
                true,
            );
            let _ = writeln!(text, "installed_patches = {}", quoted_list(&closure));
            include(
                &mut text,
                "tests/emulator/fragments/patch-matrix-mixer-ready.toml",
            );
            startup_readiness(&mut text, model);
            checks(&mut text, &combination, all)?;
            cases.push(GeneratedCase {
                id,
                model: name,
                scenario: text,
                campaign: Vec::new(),
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
            let mut campaign = Vec::new();
            if model == Model::L6max
                && matches!(patch, "midi-sync" | "knob-dialog" | "din-mixer-control")
            {
                let (scenario, name, path) = match patch {
                    "midi-sync" => (
                        include_str!(
                            "../../../tests/emulator/cases/boot/l6max-update-patched-midi.toml"
                        ),
                        "patched-midi-install",
                        "tests/emulator/cases/boot/l6max-update-patched-midi.toml",
                    ),
                    "knob-dialog" => (
                        include_str!(
                            "../../../tests/emulator/cases/boot/l6max-update-patched-knob.toml"
                        ),
                        "patched-knob-install",
                        "tests/emulator/cases/boot/l6max-update-patched-knob.toml",
                    ),
                    "din-mixer-control" => (
                        include_str!(
                            "../../../tests/emulator/scenarios/l6max-update-patched-din.toml"
                        ),
                        "patched-din-install",
                        "tests/emulator/scenarios/l6max-update-patched-din.toml",
                    ),
                    _ => unreachable!(),
                };
                text = scenario.to_owned();
                campaign = vec![
                    crate::catalog::CampaignMember {
                        name: "stock-checksum-rejection".into(),
                        model,
                        scenario: "tests/emulator/cases/boot/l6max-update-stock-bad-checksum.toml"
                            .into(),
                        fingerprint: String::new(),
                    },
                    crate::catalog::CampaignMember {
                        name: name.into(),
                        model,
                        scenario: path.into(),
                        fingerprint: String::new(),
                    },
                ];
            } else if model == Model::L6 && patch == "midi-sync" {
                text.push_str("update_fixture = \"l6-patched-midi\"\nupdate_goal = \"install\"\n");
                include(&mut text, "tests/emulator/fragments/l6-update-install.toml");
                step(
                    &mut text,
                    "action = \"lifecycle-start\"\nprofile = \"verify\"",
                );
                step(
                    &mut text,
                    "action = \"delay\"\nguest_ms = 5000\nguest_timeout_ms = 120000\nwall_timeout_s = 180",
                );
                step(
                    &mut text,
                    "action = \"include\"\npath = \"patches/midi-sync/tests/emulator/fragments/l6-hook-probe.toml\"\ninclude_guest_timeout_ms = 120000\ninclude_wall_timeout_s = 300",
                );
                step(&mut text, "action = \"lifecycle-stop\"");
            } else {
                return Err(format!(
                    "no managed update campaign for {name} patch {patch}"
                ));
            }
            cases.push(GeneratedCase {
                id,
                model: name,
                scenario: text,
                campaign,
                suites: vec![SUITE, PAIRWISE_SUITE, SINGLE_SUITE],
            });
        }
        if package.website_sd_models.contains(&model) {
            // The hosted module on the card, with the loader the engine installs.
            let id = format!("{PREFIX}{name}_hosted-sd_{patch}");
            let mut text = String::new();
            header(
                &mut text,
                &id,
                name,
                if model == Model::L6max {
                    "patch-matrix"
                } else {
                    "setup-complete"
                },
                true,
            );
            let _ = writeln!(text, "installed_sd_patches = [{patch:?}]");
            include(
                &mut text,
                "tests/emulator/fragments/patch-matrix-mixer-ready.toml",
            );
            startup_readiness(&mut text, model);
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
                campaign: Vec::new(),
                suites: vec![SUITE, PAIRWISE_SUITE, SINGLE_SUITE],
            });
        }
    }
    Ok(cases)
}

/// A combination as one value per factor: each placeable patch's placement
/// (`-`, `fw` or `sd`), then each stock slot's effect (`-` for stock).
fn factor_values(model: Model, combination: &Combination) -> Vec<String> {
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
    for model in [Model::L6, Model::L6max] {
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
    fn matrix_menu_fragments_preserve_original_expanded_steps() {
        let header =
            "version = 1\nname = \"test\"\nmodels = [\"l6max\"]\nbackends = [\"headless\"]\n";
        let cases = [
            (
                "tests/emulator/fragments/patch-matrix-menu-open.toml",
                r#"
[[step]]
action = "expect-display-image"
image = "tests/emulator/references/l6max-recorder-home.png"
settle_ms = 500
guest_timeout_ms = 30000
wall_timeout_s = 300
[[step]]
action = "press"
control = "recorder-function-1"
[[step]]
action = "expect-display-image"
image = "tests/smoke/images/l6max-sound-assign/menu-project.png"
guest_timeout_ms = 30000
wall_timeout_s = 300
"#,
            ),
            (
                "tests/emulator/fragments/patch-matrix-dump-notification-clear.toml",
                r#"
[[step]]
action = "wait-memory"
symbol = "notification_busy"
bytes = [0]
guest_timeout_ms = 30000
wall_timeout_s = 120
[[step]]
action = "wait-memory"
symbol = "notification_active"
bytes = [0]
guest_timeout_ms = 30000
wall_timeout_s = 120
"#,
            ),
        ];
        for (path, original) in cases {
            let previous = smoke_core::Scenario::from_text(&format!("{header}{original}")).unwrap();
            let included = smoke_core::Scenario::from_text(&format!(
                "{header}\n[[step]]\naction = \"include\"\npath = {path:?}\n"
            ))
            .unwrap();
            assert_eq!(
                serde_json::to_value(previous.step).unwrap(),
                serde_json::to_value(included.step).unwrap(),
                "{path}"
            );
        }
    }

    #[test]
    fn effect_slot_plan_represents_stock_and_assigned_slots() {
        let combination = Combination {
            firmware: vec![],
            sd: vec![],
            effects: vec![("spring", "plateau".into())],
        };
        assert_eq!(
            effect_slot_plan(&combination),
            vec![
                EffectSlotPlan {
                    slot_index: 0,
                    expected_effect_type: 1,
                    expected_processor_type: 0,
                    effect: None,
                    require_output: false
                },
                EffectSlotPlan {
                    slot_index: 1,
                    expected_effect_type: 2,
                    expected_processor_type: 2,
                    effect: Some("plateau"),
                    require_output: true
                },
            ]
        );
    }

    #[test]
    fn menu_plan_orders_mixed_placements_and_special_actions() {
        let checks = Checks::load().unwrap();
        let menu_checks = checks.get("menu-loader").unwrap();
        let combination = Combination {
            firmware: vec!["menu-loader".into(), "dump-rom".into()],
            sd: vec!["knob-dialog".into(), "midi-sync".into()],
            effects: vec![("spring", "plateau".into())],
        };
        let plan = menu_plan(&combination, &checks, &menu_checks.stock_system_items).unwrap();
        assert!(plan.rows[0].starts_with("patch-loader "));
        assert_eq!(plan.items[plan.patches_index], "Patches");
        assert!(plan
            .rows
            .iter()
            .any(|row| row.starts_with("dump-rom ") && !row.ends_with(" (SD)")));
        assert!(plan
            .rows
            .iter()
            .any(|row| row.starts_with("midi-sync ") && row.ends_with(" (SD)")));
        assert_eq!(plan.actions.len(), 2);
        let dump = &plan.actions[0];
        let midi = &plan.actions[1];
        assert_eq!(plan.items[dump.index], "Dump ROM");
        assert!(dump.wait_for_dump_notification);
        assert!(!dump.send_midi_with_dump_rom);
        assert_eq!(plan.items[midi.index], "Send MIDI State");
        assert!(midi.send_midi_with_dump_rom);
        assert!(!midi.wait_for_dump_notification);
        assert_eq!(
            dump.steps,
            "patches/dump-rom/tests/emulator/fragments/dump-files.toml"
        );
        assert_eq!(
            midi.steps,
            "patches/midi-sync/tests/emulator/fragments/send-midi-state.toml"
        );
    }

    #[test]
    fn pairwise_covers_every_pair_and_single_every_value() {
        for model in [Model::L6, Model::L6max] {
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
            if model == Model::L6max {
                assert!(pairwise.len() * 5 < values.len());
            }
        }
    }

    #[test]
    fn midi_update_campaign_keeps_rejection_then_install() {
        let cases = generate().unwrap();
        let case = cases
            .iter()
            .find(|case| case.id == "patches-l6max_update_midi-sync")
            .unwrap();
        assert_eq!(
            case.campaign
                .iter()
                .map(|member| member.name.as_str())
                .collect::<Vec<_>>(),
            ["stock-checksum-rejection", "patched-midi-install"]
        );
        let first =
            smoke_core::Scenario::load(&crate::catalog::scenario_file(&case.campaign[0].scenario))
                .unwrap();
        let second =
            smoke_core::Scenario::load(&crate::catalog::scenario_file(&case.campaign[1].scenario))
                .unwrap();
        assert_eq!(
            first.update_fixture,
            Some(smoke_core::UpdateFixture::BadChecksumStock)
        );
        assert_eq!(
            second.update_fixture,
            Some(smoke_core::UpdateFixture::PatchedMidi)
        );
        for (id, expected) in [
            (
                "patches-l6max_update_knob-dialog",
                smoke_core::UpdateFixture::PatchedKnob,
            ),
            (
                "patches-l6max_update_din-mixer-control",
                smoke_core::UpdateFixture::PatchedDin,
            ),
        ] {
            let other = cases.iter().find(|case| case.id == id).unwrap();
            assert_eq!(other.campaign.len(), 2);
            assert_eq!(other.campaign[0].name, "stock-checksum-rejection");
            let second = smoke_core::Scenario::load(&crate::catalog::scenario_file(
                &other.campaign[1].scenario,
            ))
            .unwrap();
            assert_eq!(second.update_fixture, Some(expected));
        }
        assert_eq!(
            cases
                .iter()
                .filter(|case| !case.campaign.is_empty())
                .count(),
            3
        );
    }

    #[test]
    fn combinations_cover_every_placement_and_effect_assignment_once() {
        let cases = generate().unwrap();
        let ids: BTreeSet<&str> = cases.iter().map(|case| case.id.as_str()).collect();
        assert_eq!(ids.len(), cases.len(), "case IDs are unique");
        let l6max = combinations(Model::L6max).unwrap();
        // knob-dialog and midi-sync are absent, in firmware or on SD, and
        // din-mixer-control absent or in firmware: 18 sets, 6 of them with
        // knob-dialog on SD. menu-loader and dump-rom go in firmware only, and
        // dump-rom needs menu-loader: 3 choices. The empty set is not a case.
        // Two effect slots each take stock, Plateau or Velvet, and a
        // replacement needs knob-dialog on SD: 8 replacement assignments per
        // such set.
        assert_eq!(l6max.len(), (18 + 8 * 6) * 3 - 1);
        assert_eq!(combinations(Model::L6).unwrap().len(), 2);
        assert!(l6max
            .iter()
            .all(|c| !(c.firmware.contains(&"dump-rom".into())
                && !c.firmware.contains(&"menu-loader".into()))));
        assert!(l6max
            .iter()
            .all(|c| c.effects.is_empty() || c.sd.contains(&"knob-dialog".into())));
    }

    #[test]
    fn expanded_matrix_preserves_model_specific_startup() {
        use smoke_core::Action;
        for case in generate().unwrap() {
            if case.id.contains("_update_") {
                continue;
            }
            let scenario = smoke_core::Scenario::from_text(&case.scenario).unwrap();
            let mixer = scenario
                .step
                .iter()
                .position(
                    |step| matches!(&step.action, Action::Wait { until } if until == "mixer-ready"),
                )
                .unwrap_or_else(|| panic!("{}: missing mixer readiness", case.id));
            assert_eq!(
                (
                    scenario.step[mixer].guest_timeout_ms,
                    scenario.step[mixer].wall_timeout_s
                ),
                (60_000, 300),
                "{}",
                case.id
            );
            let next = &scenario.step[mixer + 1];
            if case.model == "l6max" {
                assert!(
                    matches!(&next.action, Action::ExpectDisplayImage { image, settle_ms: 0 } if image == std::path::Path::new("tests/emulator/references/l6max-recorder-home.png")),
                    "{}",
                    case.id
                );
                assert_eq!(
                    (next.guest_timeout_ms, next.wall_timeout_s),
                    (30_000, 300),
                    "{}",
                    case.id
                );
            } else if scenario
                .sd_patches
                .as_ref()
                .is_some_and(|patches| !patches.is_empty())
            {
                assert!(matches!(next.action, Action::ExpectSdLoader), "{}", case.id);
                assert!(
                    matches!(
                        scenario.step[mixer + 2].action,
                        Action::ExpectSdModule { loaded: true, .. }
                    ),
                    "{}",
                    case.id
                );
            } else {
                assert!(
                    matches!(next.action, Action::Delay { guest_ms: 8_000 }),
                    "{}",
                    case.id
                );
                assert_eq!(
                    (next.guest_timeout_ms, next.wall_timeout_s),
                    (20_000, 300),
                    "{}",
                    case.id
                );
            }
        }
    }

    #[test]
    fn every_generated_scenario_loads() {
        for case in generate().unwrap() {
            smoke_core::Scenario::from_text(&case.scenario)
                .unwrap_or_else(|error| panic!("{}: {error}\n{}", case.id, case.scenario));
        }
    }

    #[test]
    fn generated_sd_combinations_check_loader_before_module() {
        let mut checked = 0;
        for case in generate().unwrap() {
            let doc: toml::Value = toml::from_str(&case.scenario).unwrap();
            let has_sd = doc
                .get("sd_patches")
                .and_then(toml::Value::as_array)
                .is_some_and(|patches| !patches.is_empty());
            let steps = doc.get("step").and_then(toml::Value::as_array).unwrap();
            let loaders: Vec<_> = steps
                .iter()
                .enumerate()
                .filter(|(_, step)| {
                    step.get("action").and_then(toml::Value::as_str) == Some("expect-sd-loader")
                })
                .collect();
            assert_eq!(loaders.len(), usize::from(has_sd), "{}", case.id);
            if let Some((index, _)) = loaders.first() {
                let module = &steps[index + 1];
                assert_eq!(
                    module.get("action").and_then(toml::Value::as_str),
                    Some("expect-sd-module"),
                    "{}",
                    case.id
                );
                assert_eq!(
                    module.get("loaded").and_then(toml::Value::as_bool),
                    Some(true),
                    "{}",
                    case.id
                );
                checked += 1;
            }
        }
        assert!(checked > 0);
    }
}
