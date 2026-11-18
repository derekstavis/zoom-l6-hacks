//! Shared firmware patch catalog and pure firmware-splicing logic for the
//! native `l6patch`
//! CLI (which also compiles payloads via `rustc`/`lld`/`llvm-objcopy`, a step
//! that stays native-only) and the browser-based WASM patcher (which applies
//! payloads precompiled ahead of time). Every function here only moves bytes
//! around in memory; none of it touches the filesystem or shells out.
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::sync::OnceLock;

include!(concat!(env!("OUT_DIR"), "/generated_catalog.rs"));

/// One patch or loader package, generated from its `package.toml`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PatchPackage {
    pub name: String,
    pub version: String,
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<PatchCategory>,
    pub order: u32,
    pub title: String,
    pub author: String,
    pub summary: String,
    pub description: String,
    pub models: Vec<String>,
    pub firmware_models: Vec<String>,
    pub sd_card_models: Vec<String>,
    pub wasm_models: Vec<String>,
    pub website: bool,
    pub website_order: u32,
    pub website_sd_models: Vec<String>,
    pub emulator_gui: bool,
    pub emulator_order: u32,
    pub emulator_sd_models: Vec<String>,
    pub readme: String,
    pub demo_image: Option<String>,
    pub request_command: Option<String>,
    pub requires: Vec<PatchDependency>,
    pub file_name: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub effects: Vec<EffectPackage>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct PatchDependency {
    pub name: String,
    pub version: String,
}

/// User-facing category used to organize selectable patches.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum PatchCategory {
    Display,
    Midi,
    Effects,
}

/// Build-time registration metadata for a custom effect implementation.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct EffectPackage {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub based_on: Option<EffectLink>,
    #[serde(default)]
    pub learn_more: Option<EffectLink>,
    #[serde(default)]
    pub controls: Vec<String>,
    #[serde(default = "default_tap_action")]
    pub tap_action: String,
}

/// Optional attribution or background link for an effect package.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct EffectLink {
    pub title: String,
    pub url: String,
}

fn default_tap_action() -> String {
    "stock".into()
}

/// Stable identifiers for L6max MAIN 1.10 stock effect slots exposed to
/// build-time replacement. Names are also used in package metadata.
pub const STOCK_EFFECT_SLOTS: [&str; 2] = ["room", "spring"];
pub const EFFECT_REPLACEMENT_ROUTER_PACKAGE: &str = "efx-loader";

/// Every package's metadata in package order. This is generated at build time
/// from the checked-in `patches/*/package.toml` files and is available to
/// native tools and the browser catalog exporter without runtime filesystem
/// access.
pub fn patch_catalog() -> &'static [PatchPackage] {
    static CATALOG: OnceLock<Vec<PatchPackage>> = OnceLock::new();
    CATALOG.get_or_init(|| {
        let value: serde_json::Value =
            serde_json::from_str(PATCH_CATALOG_JSON).expect("generated package catalog is valid");
        serde_json::from_value(value["packages"].clone())
            .expect("generated package catalog matches PatchPackage")
    })
}

pub fn patch_package(name: &str) -> Option<&'static PatchPackage> {
    patch_catalog().iter().find(|package| package.name == name)
}

fn model_name(model: FirmwareModel) -> &'static str {
    match model {
        FirmwareModel::L6 => "L6",
        FirmwareModel::L6max => "L6max",
    }
}

pub fn supports_firmware_patch(name: &str, model: FirmwareModel) -> bool {
    patch_package(name).is_some_and(|package| {
        package.kind == "patch"
            && package
                .firmware_models
                .iter()
                .any(|value| value == model_name(model))
    })
}

pub fn supports_sd_patch(name: &str, model: FirmwareModel) -> bool {
    patch_package(name).is_some_and(|package| {
        package.kind == "patch"
            && package
                .sd_card_models
                .iter()
                .any(|value| value == model_name(model))
    })
}

pub fn supports_wasm_patch(name: &str, model: FirmwareModel) -> bool {
    patch_package(name).is_some_and(|package| {
        package.kind == "patch"
            && package
                .wasm_models
                .iter()
                .any(|value| value == model_name(model))
    })
}

/// Resolve selected patch dependencies once, ordering all ready patches by
/// their package order while placing dependencies before their consumers.
pub fn resolve_patch_dependencies(requested: &[String]) -> Result<Vec<String>, String> {
    fn visit(
        name: &str,
        active: &mut std::collections::BTreeSet<String>,
        complete: &mut std::collections::BTreeSet<String>,
        ordered: &mut Vec<String>,
    ) -> Result<(), String> {
        if complete.contains(name) {
            return Ok(());
        }
        let package = patch_package(name)
            .filter(|package| package.kind == "patch")
            .ok_or_else(|| format!("patch `{name}` has no patches/{name}/package.toml"))?;
        if !active.insert(name.to_owned()) {
            return Err(format!("patch dependency cycle at `{name}`"));
        }
        for dependency in &package.requires {
            visit(&dependency.name, active, complete, ordered)?;
        }
        active.remove(name);
        complete.insert(name.to_owned());
        ordered.push(name.to_owned());
        Ok(())
    }

    let mut active = std::collections::BTreeSet::new();
    let mut complete = std::collections::BTreeSet::new();
    let mut ordered = Vec::new();
    for name in requested {
        visit(name, &mut active, &mut complete, &mut ordered)?;
    }

    let mut remaining = ordered
        .into_iter()
        .collect::<std::collections::BTreeSet<_>>();
    let mut result = Vec::with_capacity(remaining.len());
    while !remaining.is_empty() {
        let next = remaining
            .iter()
            .filter(|name| {
                patch_package(name).is_some_and(|package| {
                    package
                        .requires
                        .iter()
                        .all(|dependency| !remaining.contains(&dependency.name))
                })
            })
            .min_by_key(|name| {
                patch_package(name)
                    .map(|package| (package.order, package.name.clone()))
                    .unwrap_or((u32::MAX, (*name).clone()))
            })
            .cloned()
            .ok_or("patch dependency cycle")?;
        remaining.remove(&next);
        result.push(next);
    }
    Ok(result)
}

/// Resolve slot-to-effect assignments into the package set for one composed
/// replacement bundle. The `replacements` pairs are `(stock_slot, effect_id)`;
/// an effect assigned to multiple slots is included only once.
pub fn resolve_effect_replacements(
    replacements: &[(String, String)],
) -> Result<Vec<String>, String> {
    let requested = effect_replacement_packages(replacements, patch_catalog())?;
    resolve_patch_dependencies(&requested)
}

/// Versioned filename for a composed effect-replacement map. Stock slots are
/// written explicitly so each supported mapping has a unique SD asset name.
pub fn effect_replacement_file_name(replacements: &[(String, String)]) -> Result<String, String> {
    let requested = effect_replacement_packages(replacements, patch_catalog())?;
    if requested.is_empty() {
        return Err("effect replacement bundle needs at least one assigned effect".into());
    }
    let resolved = resolve_patch_dependencies(&requested)?;
    let package_names = resolved
        .iter()
        .filter(|name| name.as_str() != EFFECT_REPLACEMENT_ROUTER_PACKAGE)
        .map(|name| {
            let package = patch_package(name).ok_or("missing effect implementation package")?;
            Ok(format!("{}-{}", package.name, package.version))
        })
        .collect::<Result<Vec<_>, String>>()?
        .join("-");
    let assignment = |slot: &str| {
        replacements
            .iter()
            .find(|(selected_slot, _)| selected_slot == slot)
            .map(|(_, id)| id.as_str())
            .unwrap_or("stock")
    };
    Ok(format!(
        "efx-loader-{}-room-{}-spring-{}.bin",
        package_names,
        assignment("room"),
        assignment("spring")
    ))
}

/// Validate a slot-to-effect map against a package catalog and return the
/// distinct selected effect package names in first-assignment order.
pub fn effect_replacement_packages(
    replacements: &[(String, String)],
    catalog: &[PatchPackage],
) -> Result<Vec<String>, String> {
    let mut slots = std::collections::BTreeSet::new();
    let mut packages = Vec::new();
    if !replacements.is_empty() {
        if !catalog
            .iter()
            .any(|package| package.name == EFFECT_REPLACEMENT_ROUTER_PACKAGE)
        {
            return Err(format!(
                "missing effect replacement router package `{EFFECT_REPLACEMENT_ROUTER_PACKAGE}`"
            ));
        }
        packages.push(EFFECT_REPLACEMENT_ROUTER_PACKAGE.to_owned());
    }
    for (slot, effect_id) in replacements {
        if !STOCK_EFFECT_SLOTS.contains(&slot.as_str()) {
            return Err(format!("unknown stock effect slot `{slot}`"));
        }
        if !slots.insert(slot) {
            return Err(format!(
                "stock effect slot `{slot}` has multiple replacements"
            ));
        }
        let package = catalog
            .iter()
            .find(|package| package.effects.iter().any(|effect| effect.id == *effect_id))
            .ok_or_else(|| format!("unknown effect implementation `{effect_id}`"))?;
        if !packages.contains(&package.name) {
            packages.push(package.name.clone());
        }
    }
    Ok(packages)
}

/// Return a versioned filename for a composed module. Requested packages are
/// named first; dependency-only packages follow in their resolved order.
pub fn composed_patch_file_name(requested: &[String]) -> Result<String, String> {
    let ordered = resolve_patch_dependencies(requested)?;
    let mut members = Vec::<&PatchPackage>::new();
    for name in requested.iter().chain(ordered.iter()) {
        let package = patch_package(name).ok_or_else(|| format!("unknown patch `{name}`"))?;
        if !members.iter().any(|member| member.name == package.name) {
            members.push(package);
        }
    }
    if members.is_empty() {
        return Err("composed bundle needs at least one patch name".into());
    }
    Ok(format!(
        "{}.bin",
        members
            .iter()
            .map(|package| format!("{}-{}", package.name, package.version))
            .collect::<Vec<_>>()
            .join("-")
    ))
}
pub const FIRST_PAYLOAD: u32 = 0x80108000;
pub const SLOT_END: u32 = 0x801a2ff8;
/// SHA-256 of the supported stock L6max MAIN firmware image (1.10).
pub const EXPECTED_FIRMWARE_SHA256: &str =
    "980d92272c1f84bed200b4d2f83575202f6a00082c928d99b9e8b53af8d44461";
/// SHA-256 of the supported stock L6 MAIN firmware image (1.10).
pub const L6_EXPECTED_FIRMWARE_SHA256: &str =
    "827c7c5cb2a43cc3a011d4556462b8cb9539242f817ebeed7727d66a36c03799";
pub const L6_MAIN_BASE: u32 = 0x80001000;
/// End of the observed L6 MAIN slot, immediately before its trailer area.
pub const L6_SLOT_END: u32 = 0x80285ff8;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FirmwareModel {
    L6,
    L6max,
}

/// The L6 payload starts immediately after its exact supported MAIN image,
/// aligned for Thumb payload placement. Its large erased package padding
/// supplies the bytes consumed when the MAIN segment grows.
pub fn first_payload_for(model: FirmwareModel, main_length: usize) -> u32 {
    match model {
        FirmwareModel::L6 => {
            let end = L6_MAIN_BASE + main_length as u32;
            (end + 15) & !15
        }
        FirmwareModel::L6max => FIRST_PAYLOAD,
    }
}

/// Encodes a Thumb-2 BL (`link: true`) or B.W (`link: false`) branch from `from` to `to`.
pub fn branch(from: u32, to: u32, link: bool) -> [u8; 4] {
    let displacement = to.wrapping_sub(from + 4);
    assert!(displacement as i32 >= -16_777_216 && (displacement as i32) < 16_777_216);
    assert_eq!(displacement & 1, 0);
    let s = (displacement >> 24) & 1;
    let j1 = (!(displacement >> 23) ^ s) & 1;
    let j2 = (!(displacement >> 22) ^ s) & 1;
    let first = 0xf000 | (s << 10) | ((displacement >> 12) & 0x3ff);
    let second = (if link { 0xd000 } else { 0x9000 })
        | (j1 << 13)
        | (j2 << 11)
        | ((displacement >> 1) & 0x7ff);
    let mut bytes = [0; 4];
    bytes[..2].copy_from_slice(&(first as u16).to_le_bytes());
    bytes[2..].copy_from_slice(&(second as u16).to_le_bytes());
    bytes
}

/// Detects the model from the MAIN reset vector.
pub fn detect_firmware_model(firmware: &[u8]) -> Result<FirmwareModel, String> {
    let reset_vector = firmware
        .get(4..8)
        .ok_or("firmware is too short to contain a reset vector")?;
    match u32::from_le_bytes(reset_vector.try_into().unwrap()) {
        0x80001415 => Ok(FirmwareModel::L6),
        0x80000415 => Ok(FirmwareModel::L6max),
        value => Err(format!("unsupported firmware reset vector: {value:#010x}")),
    }
}

/// Confirms `firmware` is byte-identical to the supported stock image for its
/// detected model, returning a descriptive error instead of panicking (the
/// inputs — user uploads in the browser or local files for the CLI — are
/// untrusted enough to deserve a clean error, not a crash).
pub fn verify_firmware(firmware: &[u8]) -> Result<(), String> {
    verify_firmware_for(detect_firmware_model(firmware)?, firmware)
}

pub fn verify_firmware_for(model: FirmwareModel, firmware: &[u8]) -> Result<(), String> {
    let digest = format!("{:x}", Sha256::digest(firmware));
    let (expected, reset_vector, min_len) = match model {
        FirmwareModel::L6 => (L6_EXPECTED_FIRMWARE_SHA256, 0x80001415u32, 8),
        FirmwareModel::L6max => (EXPECTED_FIRMWARE_SHA256, 0x80000415u32, 0x410),
    };
    if digest != expected {
        return Err(format!(
            "unsupported main firmware SHA-256: expected {expected}, got {digest}"
        ));
    }
    if firmware.len() < min_len {
        return Err("firmware is too short to be the expected image".into());
    }
    if u32::from_le_bytes(firmware[4..8].try_into().unwrap()) != reset_vector {
        return Err("firmware does not match the expected reset vector".into());
    }
    if model == FirmwareModel::L6 {
        return Ok(());
    }
    if firmware[0x40c..0x410] != 0x8007eea9u32.to_le_bytes() {
        return Err("firmware does not match the expected reset support vector".into());
    }
    if firmware.len() > 0x108000 {
        return Err("payload would overwrite original firmware".into());
    }
    Ok(())
}

/// One hooked instruction: a branch overwrites the first 4 bytes at `address`,
/// and any remaining bytes up to `length` are filled with Thumb NOPs
/// (`0xbf00`), matching how `l6patch` neutralizes a wider displaced
/// region than the branch itself occupies.
pub struct HookEdit {
    pub address: u32,
    pub target: u32,
    pub length: usize,
}

/// Everything needed to splice one already-compiled patch payload into a
/// firmware image.
pub struct PatchEdits {
    pub hooks: Vec<HookEdit>,
}

impl PatchEdits {
    pub fn new(hooks: Vec<HookEdit>) -> Self {
        Self { hooks }
    }
}

/// Result of successfully applying one patch: where its payload landed, and
/// the next 16-byte-aligned base address for whatever patch comes after it.
pub struct AppliedPatch {
    pub base: u32,
    pub payload_len: usize,
    pub next_base: u32,
}

/// Places one already-compiled, position-dependent payload at `base` in
/// `firmware` (growing it with `0xff` filler as needed) and applies its hook
/// and call-site edits. `original_length` is the firmware's length before any
/// patch in this run was applied; `padding` is the untouched
/// `padding_before_secondary.bin` content read before any patch grew the
/// image. Mirrors `l6patch`'s per-patch loop exactly, minus compiling.
pub fn apply_patch(
    firmware: &mut Vec<u8>,
    original_length: usize,
    base: u32,
    payload: &[u8],
    edits: &PatchEdits,
    padding: &[u8],
) -> Result<AppliedPatch, String> {
    apply_patch_with_map(
        firmware,
        original_length,
        base,
        payload,
        edits,
        padding,
        0x80000000,
        SLOT_END,
    )
}

pub fn apply_patch_with_map(
    firmware: &mut Vec<u8>,
    original_length: usize,
    base: u32,
    payload: &[u8],
    edits: &PatchEdits,
    padding: &[u8],
    firmware_base: u32,
    slot_end: u32,
) -> Result<AppliedPatch, String> {
    if base + payload.len() as u32 > slot_end {
        return Err("patch exceeds observed application slot".into());
    }
    if base < firmware_base {
        return Err("payload address precedes the loaded MAIN image".into());
    }
    let offset = (base - firmware_base) as usize;
    let consumed = offset + payload.len() - original_length;
    if consumed > padding.len() || padding[..consumed].iter().any(|&b| b != 0xff) {
        return Err("payload would overwrite non-erased reserved bytes".into());
    }
    firmware.resize(offset, 0xff);
    firmware.extend_from_slice(payload);
    for hook in &edits.hooks {
        let off = (hook.address - firmware_base) as usize;
        firmware[off..off + 4].copy_from_slice(&branch(hook.address, hook.target, false));
        for chunk in firmware[off + 4..off + hook.length].chunks_exact_mut(2) {
            chunk.copy_from_slice(&[0, 0xbf]);
        }
    }
    let next_base = (base + payload.len() as u32 + 15) & !15;
    Ok(AppliedPatch {
        base,
        payload_len: payload.len(),
        next_base,
    })
}

/// Writes the new main image length and update-version marker into a copy of
/// `main_trailer.bin` (8 bytes: little-endian length, then 4 ASCII digits)
/// and `package_footer.bin` (same two fields at its own fixed offsets).
pub fn write_image_length_and_version(
    trailer: &mut [u8],
    footer: &mut [u8],
    length: u32,
    version: &str,
) {
    trailer[..4].copy_from_slice(&length.to_le_bytes());
    trailer[4..8].copy_from_slice(version.as_bytes());
    footer[0x38..0x3c].copy_from_slice(&length.to_le_bytes());
    footer[0x3c..0x40].copy_from_slice(version.as_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;

    fn effect_package(name: &str, id: &str) -> PatchPackage {
        PatchPackage {
            name: name.into(),
            version: "1.0.0".into(),
            kind: "patch".into(),
            category: Some(PatchCategory::Effects),
            order: 0,
            title: name.into(),
            author: "derekstavis".into(),
            summary: String::new(),
            description: String::new(),
            models: vec!["L6max".into()],
            firmware_models: Vec::new(),
            sd_card_models: vec!["L6max".into()],
            wasm_models: Vec::new(),
            website: false,
            website_order: 0,
            website_sd_models: Vec::new(),
            emulator_gui: false,
            emulator_order: 0,
            emulator_sd_models: Vec::new(),
            readme: "README.md".into(),
            demo_image: None,
            request_command: None,
            requires: Vec::new(),
            file_name: format!("{name}-1.0.0.bin"),
            effects: vec![EffectPackage {
                id: id.into(),
                title: id.into(),
                based_on: None,
                learn_more: None,
                controls: Vec::new(),
                tap_action: "stock".into(),
            }],
        }
    }

    #[test]
    fn package_catalog_is_generated_from_package_files() {
        let catalog = patch_catalog();
        let firmware_patches = catalog
            .iter()
            .filter(|package| package.kind == "patch" && !package.firmware_models.is_empty())
            .map(|package| package.name.as_str())
            .collect::<Vec<_>>();
        let wasm_patches = catalog
            .iter()
            .filter(|package| package.kind == "patch" && !package.wasm_models.is_empty())
            .map(|package| package.name.as_str())
            .collect::<Vec<_>>();
        assert_eq!(PATCHES.as_slice(), firmware_patches);
        assert_eq!(WASM_PATCHES.as_slice(), wasm_patches);
        let knob = patch_package("knob-dialog").unwrap();
        assert_eq!(knob.version, "1.0.0");
        assert_eq!(knob.title, "Show knob values on display");
        assert_eq!(knob.author, "derekstavis");
        assert!(supports_firmware_patch("knob-dialog", FirmwareModel::L6max));
        assert!(!supports_firmware_patch("knob-dialog", FirmwareModel::L6));
        assert!(supports_sd_patch("midi-sync", FirmwareModel::L6));
        assert!(patch_package("patch-loader").is_some_and(|package| package.kind == "loader"));
        for (package_name, effect_id) in [("efx-plateau", "plateau"), ("efx-velvet", "velvet")] {
            let package = patch_package(package_name).unwrap();
            assert_eq!(package.author, "VegaDeftwing");
            assert!(
                package
                    .requires
                    .iter()
                    .any(|dependency| dependency.name == "knob-dialog")
            );
            let effect = package
                .effects
                .iter()
                .find(|effect| effect.id == effect_id)
                .unwrap();
            assert_eq!(effect.controls, ["encoder-parameters"]);
            assert_eq!(effect.tap_action, "toggle-editor");
            for slot in STOCK_EFFECT_SLOTS {
                assert!(
                    effect_replacement_packages(
                        &[(slot.to_owned(), effect_id.to_owned())],
                        catalog,
                    )
                    .is_ok()
                );
            }
        }
        assert_eq!(
            resolve_effect_replacements(&[("room".into(), "velvet".into())]).unwrap(),
            ["knob-dialog", "efx-loader", "efx-velvet"]
        );
        assert_eq!(
            effect_replacement_file_name(&[("room".into(), "velvet".into())]).unwrap(),
            "efx-loader-knob-dialog-1.0.0-efx-velvet-1.0.0-room-velvet-spring-stock.bin"
        );
    }

    #[test]
    fn package_filename_includes_id_and_version() {
        assert_eq!(
            composed_patch_file_name(&["midi-sync".to_owned()]).unwrap(),
            "midi-sync-1.0.0.bin"
        );
        assert_eq!(
            patch_package("midi-sync").unwrap().file_name,
            "midi-sync-1.0.0.bin"
        );
    }

    #[test]
    fn dependency_resolution_uses_package_order_and_includes_each_patch_once() {
        let requested = ["midi-sync".to_owned(), "midi-control".to_owned()];
        assert_eq!(
            resolve_patch_dependencies(&requested).unwrap(),
            ["midi-control".to_owned(), "midi-sync".to_owned(),]
        );
    }

    #[test]
    fn effect_replacement_resolution_accepts_empty_map_and_rejects_unknown_effects() {
        assert!(resolve_effect_replacements(&[]).unwrap().is_empty());
        assert_eq!(
            resolve_effect_replacements(&[("room".into(), "missing-effect".into())]).unwrap_err(),
            "unknown effect implementation `missing-effect`"
        );
    }

    #[test]
    fn effect_slot_map_accepts_any_known_slot_and_deduplicates_packages() {
        let catalog = [
            effect_package("efx-loader", "unused-router-effect"),
            effect_package("efx-velvet", "velvet"),
        ];
        assert_eq!(
            effect_replacement_packages(
                &[
                    ("room".into(), "velvet".into()),
                    ("spring".into(), "velvet".into())
                ],
                &catalog,
            )
            .unwrap(),
            ["efx-loader", "efx-velvet"]
        );
        assert!(
            effect_replacement_packages(&[("ainr".into(), "velvet".into())], &catalog)
                .unwrap_err()
                .contains("unknown stock effect slot")
        );
        assert!(
            effect_replacement_packages(
                &[
                    ("room".into(), "velvet".into()),
                    ("room".into(), "velvet".into())
                ],
                &catalog
            )
            .unwrap_err()
            .contains("multiple replacements")
        );
    }

    #[test]
    fn dependency_resolution_uses_package_order_and_includes_shared_dependency_once() {
        let requested = ["efx-plateau".to_owned(), "midi-control".to_owned()];
        assert_eq!(
            resolve_patch_dependencies(&requested).unwrap(),
            [
                "knob-dialog".to_owned(),
                "midi-control".to_owned(),
                "efx-plateau".to_owned(),
            ]
        );
    }

    #[test]
    fn generic_firmware_verification_detects_both_models_from_reset_vector() {
        for (reset_vector, expected) in [
            (0x80001415u32, FirmwareModel::L6),
            (0x80000415u32, FirmwareModel::L6max),
        ] {
            let mut firmware = vec![0; 8];
            firmware[4..8].copy_from_slice(&reset_vector.to_le_bytes());
            assert_eq!(detect_firmware_model(&firmware), Ok(expected));
            let error = verify_firmware(&firmware).unwrap_err();
            let expected_digest = match expected {
                FirmwareModel::L6 => L6_EXPECTED_FIRMWARE_SHA256,
                FirmwareModel::L6max => EXPECTED_FIRMWARE_SHA256,
            };
            assert!(error.contains(expected_digest), "{error}");
        }
    }

    #[test]
    fn generic_firmware_verification_rejects_unknown_and_short_images() {
        assert!(
            detect_firmware_model(&[0; 7])
                .unwrap_err()
                .contains("too short")
        );
        let mut firmware = vec![0; 8];
        firmware[4..8].copy_from_slice(&0x12345679u32.to_le_bytes());
        assert!(
            detect_firmware_model(&firmware)
                .unwrap_err()
                .contains("unsupported firmware reset vector")
        );
    }

    #[test]
    fn thumb_branches_encode_synthetic_targets() {
        assert_eq!(branch(0x1000, 0x1004, true), [0x00, 0xf0, 0x00, 0xf8]);
        assert_eq!(branch(0x1000, 0x1004, false), [0x00, 0xf0, 0x00, 0xb8]);
    }

    #[test]
    fn apply_patch_grows_firmware_and_writes_a_hook_branch() {
        // Just under FIRST_PAYLOAD's offset (0x108000), so growth is small.
        let mut firmware = vec![0u8; 0x107ff0];
        let original_length = firmware.len();
        let padding = vec![0xffu8; 0x10000];
        let edits = PatchEdits::new(vec![HookEdit {
            address: 0x80000100,
            target: 0x80108010,
            length: 6,
        }]);
        let payload = vec![0xaa; 16];
        let applied = apply_patch(
            &mut firmware,
            original_length,
            FIRST_PAYLOAD,
            &payload,
            &edits,
            &padding,
        )
        .unwrap();
        assert_eq!(applied.base, FIRST_PAYLOAD);
        assert_eq!(applied.next_base, 0x80108010);
        assert_eq!(
            &firmware[0x100..0x104],
            &branch(0x80000100, 0x80108010, false)
        );
        assert_eq!(&firmware[0x104..0x106], &[0, 0xbf]);
        assert_eq!(&firmware[0x108000..], &payload[..]);
    }

    #[test]
    fn l6_payload_and_hook_offsets_use_the_l6_loaded_base() {
        let mut firmware = vec![0u8; 0x20];
        let padding = vec![0xff; 0x100];
        assert_eq!(
            first_payload_for(FirmwareModel::L6max, 0x1081e8),
            FIRST_PAYLOAD
        );
        let base = first_payload_for(FirmwareModel::L6, firmware.len());
        assert_eq!(base, 0x80001020);
        let target = base;
        let edits = PatchEdits::new(vec![HookEdit {
            address: 0x80001010,
            target,
            length: 4,
        }]);
        let payload = [0xaa; 16];
        let applied = apply_patch_with_map(
            &mut firmware,
            0x20,
            base,
            &payload,
            &edits,
            &padding,
            L6_MAIN_BASE,
            L6_SLOT_END,
        )
        .unwrap();
        assert_eq!(applied.base, base);
        assert_eq!(applied.next_base, 0x80001030);
        assert_eq!(&firmware[0x10..0x14], &branch(0x80001010, target, false));
        assert_eq!(&firmware[0x20..], &payload);
    }

    #[test]
    fn apply_patch_rejects_non_erased_padding() {
        let mut firmware = vec![0u8; 0x107ff0];
        let original_length = firmware.len();
        let mut padding = vec![0xffu8; 0x10000];
        padding[5] = 0x00; // falls within the small region this patch would consume
        let edits = PatchEdits::new(Vec::new());
        let payload = vec![0xaa; 16];
        assert!(
            apply_patch(
                &mut firmware,
                original_length,
                FIRST_PAYLOAD,
                &payload,
                &edits,
                &padding
            )
            .is_err()
        );
    }
}
