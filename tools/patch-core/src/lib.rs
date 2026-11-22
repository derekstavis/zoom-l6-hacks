//! Shared firmware patch catalog and pure firmware-splicing logic for the
//! native `l6patch`
//! CLI (which also compiles payloads via `rustc`/`lld`/`llvm-objcopy`, a step
//! that stays native-only) and the browser-based WASM patcher (which applies
//! payloads precompiled ahead of time). Every function here only moves bytes
//! around in memory; none of it touches the filesystem or shells out.
use l6fw::Model;
use sha2::{Digest, Sha256};
use std::sync::OnceLock;

include!(concat!(env!("OUT_DIR"), "/generated_catalog.rs"));

pub mod catalog;
pub mod plan;
pub use catalog::{
    EFFECT_REPLACEMENT_ROUTER_PACKAGE, EffectControl, EffectLink, EffectPackage, PackageKind,
    PatchCategory, PatchDependency, PatchPackage, STOCK_EFFECT_SLOTS, TapAction,
    effect_replacement_packages,
};
pub use plan::{Delivery, Plan};

/// Every package in package order, generated at build time from the
/// checked-in `patches/*/package.toml` files, so native tools and the browser
/// catalog exporter need no filesystem access.
pub fn patch_catalog() -> &'static [PatchPackage] {
    static CATALOG: OnceLock<Vec<PatchPackage>> = OnceLock::new();
    CATALOG.get_or_init(|| {
        #[derive(serde::Deserialize)]
        struct Generated {
            packages: Vec<PatchPackage>,
        }
        serde_json::from_str::<Generated>(PATCH_CATALOG_JSON)
            .expect("generated package catalog is valid")
            .packages
    })
}

pub fn patch_package(name: &str) -> Option<&'static PatchPackage> {
    patch_catalog().iter().find(|package| package.name == name)
}

/// Whether the browser patcher can apply `name` to `model`.
pub fn supports_wasm_patch(name: &str, model: Model) -> bool {
    patch_package(name).is_some_and(|package| {
        package.kind == PackageKind::Patch && package.wasm_models.contains(&model)
    })
}

/// The requested patches and everything they require, in package order with
/// every dependency before the patches that need it.
pub fn resolve_patch_dependencies(requested: &[String]) -> Result<Vec<String>, String> {
    if let Some(name) = requested
        .iter()
        .find(|name| patch_package(name).is_some_and(|package| package.kind != PackageKind::Patch))
    {
        return Err(format!("`{name}` is not a patch"));
    }
    catalog::resolve_dependencies(patch_catalog(), requested)
}

/// Every effect-replacement map the catalog's effects allow; see
/// [`catalog::effect_assignments`].
pub fn effect_assignments() -> Vec<Vec<(String, String)>> {
    catalog::effect_assignments(patch_catalog())
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
pub fn detect_firmware_model(firmware: &[u8]) -> Result<Model, String> {
    let reset_vector = firmware
        .get(4..8)
        .ok_or("firmware is too short to contain a reset vector")?;
    let value = u32::from_le_bytes(reset_vector.try_into().unwrap());
    Model::from_stock_reset_vector(value)
        .ok_or_else(|| format!("unsupported firmware reset vector: {value:#010x}"))
}

/// Confirms `firmware` is byte-identical to the supported stock image for its
/// detected model, returning a descriptive error instead of panicking (the
/// inputs — user uploads in the browser or local files for the CLI — are
/// untrusted enough to deserve a clean error, not a crash).
pub fn verify_firmware(firmware: &[u8]) -> Result<(), String> {
    verify_firmware_for(detect_firmware_model(firmware)?, firmware)
}

pub fn verify_firmware_for(model: Model, firmware: &[u8]) -> Result<(), String> {
    let digest = format!("{:x}", Sha256::digest(firmware));
    let expected = model.stock_main_sha256();
    let reset_vector = model.stock_reset_vector();
    let min_len = match model {
        Model::L6 => 8,
        Model::L6max => 0x410,
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
    if model == Model::L6 {
        return Ok(());
    }
    if firmware[0x40c..0x410] != 0x8007eea9u32.to_le_bytes() {
        return Err("firmware does not match the expected reset support vector".into());
    }
    if firmware.len() > (model.first_payload(firmware.len()) - model.main_base()) as usize {
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
/// image. `firmware_base` is where MAIN loads and nothing may reach
/// `slot_end`. Mirrors `l6patch`'s per-patch loop exactly, minus compiling.
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

/// Offset of `patchrt::PatchInfo::next` within a payload's identity record.
pub const PATCH_INFO_NEXT_OFFSET: u32 = 20;

/// Link the identity records of every static payload into a ring, in the
/// order given, by writing each record's `next` word. menu-loader starts from
/// its own record and follows the ring to list every installed payload. The
/// native and browser patchers both call this, so they produce identical
/// images.
pub fn link_identity_ring(
    firmware: &mut [u8],
    firmware_base: u32,
    records: &[u32],
) -> Result<(), String> {
    for (index, &record) in records.iter().enumerate() {
        let next = records[(index + 1) % records.len()];
        let offset = record
            .checked_add(PATCH_INFO_NEXT_OFFSET)
            .and_then(|address| address.checked_sub(firmware_base))
            .ok_or("identity record outside the MAIN image")? as usize;
        let word = firmware
            .get_mut(offset..offset + 4)
            .ok_or("identity record outside the MAIN image")?;
        if word != [0; 4] {
            return Err(format!("identity record at {record:#x} is already linked"));
        }
        word.copy_from_slice(&next.to_le_bytes());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIRST_PAYLOAD: u32 = 0x8010_8000;

    fn effect_package(name: &str, id: &str) -> PatchPackage {
        PatchPackage {
            name: name.into(),
            version: "1.0.0".into(),
            kind: PackageKind::Patch,
            category: Some(PatchCategory::Effects),
            order: 0,
            title: name.into(),
            author: "derekstavis".into(),
            summary: String::new(),
            description: String::new(),
            models: vec![Model::L6max],
            firmware_models: Vec::new(),
            sd_card_models: vec![Model::L6max],
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
                tap_action: TapAction::Stock,
            }],
        }
    }

    #[test]
    fn package_catalog_is_generated_from_package_files() {
        let catalog = patch_catalog();
        let firmware_patches = catalog
            .iter()
            .filter(|package| {
                package.kind == PackageKind::Patch && !package.firmware_models.is_empty()
            })
            .map(|package| package.name.as_str())
            .collect::<Vec<_>>();
        let wasm_patches = catalog
            .iter()
            .filter(|package| package.kind == PackageKind::Patch && !package.wasm_models.is_empty())
            .map(|package| package.name.as_str())
            .collect::<Vec<_>>();
        assert_eq!(PATCHES.as_slice(), firmware_patches);
        assert_eq!(WASM_PATCHES.as_slice(), wasm_patches);
        let knob = patch_package("knob-dialog").unwrap();
        assert_eq!(knob.version, "1.0.0");
        assert_eq!(knob.title, "Show knob values on display");
        assert_eq!(knob.author, "derekstavis");
        assert!(supports_wasm_patch("knob-dialog", Model::L6max));
        assert!(!supports_wasm_patch("knob-dialog", Model::L6));
        assert!(
            patch_package("patch-loader")
                .is_some_and(|package| package.kind == PackageKind::Loader)
        );
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
            assert_eq!(effect.controls, [EffectControl::EncoderParameters]);
            assert_eq!(effect.tap_action, TapAction::ToggleEditor);
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
        let plan = Plan::new(
            Model::L6max,
            Delivery::SdCard,
            &[],
            &[("room".into(), "velvet".into())],
        )
        .unwrap();
        assert_eq!(plan.patches, ["knob-dialog", "efx-loader", "efx-velvet"]);
        assert_eq!(
            plan.file_name().unwrap(),
            "efx-loader-1.0.1-knob-dialog-1.0.0-efx-velvet-1.0.1-room-velvet-spring-stock.bin"
        );
    }

    #[test]
    fn the_catalog_rejects_inconsistent_packages() {
        let catalog = patch_catalog().to_vec();
        assert_eq!(catalog::validate(&catalog), Ok(()));
        let index = |name: &str| catalog.iter().position(|p| p.name == name).unwrap();
        let broken = |edit: &dyn Fn(&mut Vec<PatchPackage>)| {
            let mut packages = catalog.clone();
            edit(&mut packages);
            catalog::validate(&packages).unwrap_err()
        };
        let dump = index("dump-rom");
        let velvet = index("efx-velvet");
        assert!(broken(&|p| p[dump].requires[0].version = "9.9.9".into()).contains("version"));
        assert!(broken(&|p| p[dump].requires[0].name = "missing".into()).contains("missing"));
        assert!(broken(&|p| p[dump].wasm_models.push(Model::L6)).contains("subset"));
        assert!(broken(&|p| p[dump].author = "-nobody".into()).contains("GitHub"));
        assert!(broken(&|p| p[dump].category = None).contains("category"));
        assert!(broken(&|p| p[velvet].effects[0].controls.clear()).contains("tap action"));
        assert!(broken(&|p| p[velvet].effects[0].id = "plateau".into()).contains("duplicate"));
        let menu = index("menu-loader");
        assert!(
            broken(&|p| p[menu].requires.push(PatchDependency {
                name: "dump-rom".into(),
                version: "1.0.0".into()
            }))
            .contains("cycle")
        );
    }

    #[test]
    fn package_filename_includes_id_and_version() {
        let plan = Plan::new(Model::L6, Delivery::SdCard, &["midi-sync".into()], &[]).unwrap();
        assert_eq!(plan.file_name().unwrap(), "midi-sync-1.0.0.bin");
        assert_eq!(
            patch_package("midi-sync").unwrap().file_name,
            "midi-sync-1.0.0.bin"
        );
    }

    #[test]
    fn dependency_resolution_uses_package_order_and_includes_each_patch_once() {
        let requested = ["midi-sync".to_owned(), "knob-dialog".to_owned()];
        assert_eq!(
            resolve_patch_dependencies(&requested).unwrap(),
            ["knob-dialog".to_owned(), "midi-sync".to_owned(),]
        );
    }

    #[test]
    fn a_plan_checks_the_model_the_delivery_and_the_effects() {
        let plan = |model, delivery, patches: &[&str], effects: &[(&str, &str)]| {
            let patches = patches.iter().map(|&name| name.into()).collect::<Vec<_>>();
            let effects = effects
                .iter()
                .map(|&(slot, id)| (slot.into(), id.into()))
                .collect::<Vec<_>>();
            Plan::new(model, delivery, &patches, &effects)
        };
        let room = [("room", "velvet")];
        let midi = plan(Model::L6max, Delivery::SdCard, &["midi-sync"], &room).unwrap();
        assert_eq!(midi.requested, ["midi-sync", "efx-loader", "efx-velvet"]);
        assert_eq!(
            midi.members(),
            ["midi-sync", "efx-loader", "efx-velvet", "knob-dialog"]
        );
        assert_eq!(
            plan(Model::L6, Delivery::Firmware, &["knob-dialog"], &[]).unwrap_err(),
            "patch `knob-dialog` is not available for L6 firmware"
        );
        assert!(plan(Model::L6max, Delivery::Browser, &["dump-rom"], &[]).is_err());
        assert!(plan(Model::L6, Delivery::SdCard, &[], &room).is_err());
        assert!(plan(Model::L6max, Delivery::Firmware, &[], &room).is_err());
        assert!(plan(Model::L6max, Delivery::SdCard, &[], &[]).is_err());
        assert!(plan(Model::L6max, Delivery::Browser, &["patch-loader"], &[]).is_err());
        assert_eq!(
            plan(Model::L6max, Delivery::SdCard, &[], &[("room", "missing")]).unwrap_err(),
            "unknown effect implementation `missing`"
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
        let requested = ["efx-plateau".to_owned(), "midi-sync".to_owned()];
        assert_eq!(
            resolve_patch_dependencies(&requested).unwrap(),
            [
                "knob-dialog".to_owned(),
                "midi-sync".to_owned(),
                "efx-plateau".to_owned(),
            ]
        );
    }

    #[test]
    fn generic_firmware_verification_detects_both_models_from_reset_vector() {
        for (reset_vector, expected) in [(0x80001415u32, Model::L6), (0x80000415u32, Model::L6max)]
        {
            let mut firmware = vec![0; 8];
            firmware[4..8].copy_from_slice(&reset_vector.to_le_bytes());
            assert_eq!(detect_firmware_model(&firmware), Ok(expected));
            let error = verify_firmware(&firmware).unwrap_err();
            let expected_digest = expected.stock_main_sha256();
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
        // Just under the first payload's offset (0x108000), so growth is small.
        let mut firmware = vec![0u8; 0x107ff0];
        let original_length = firmware.len();
        let padding = vec![0xffu8; 0x10000];
        let edits = PatchEdits::new(vec![HookEdit {
            address: 0x80000100,
            target: 0x80108010,
            length: 6,
        }]);
        let payload = vec![0xaa; 16];
        let applied = apply_patch_with_map(
            &mut firmware,
            original_length,
            FIRST_PAYLOAD,
            &payload,
            &edits,
            &padding,
            Model::L6max.main_base(),
            Model::L6max.slot_end(),
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
        assert_eq!(Model::L6max.first_payload(0x1081e8), FIRST_PAYLOAD);
        let base = Model::L6.first_payload(firmware.len());
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
            Model::L6.main_base(),
            Model::L6.slot_end(),
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
            apply_patch_with_map(
                &mut firmware,
                original_length,
                FIRST_PAYLOAD,
                &payload,
                &edits,
                &padding,
                Model::L6max.main_base(),
                Model::L6max.slot_end(),
            )
            .is_err()
        );
    }
}
