use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{fs, io::Write, path::Path};

/// The MAIN version stock packages carry (1.10), in the MAIN trailer and the
/// package footer.
pub const STOCK_VERSION: [u8; 4] = *b"0110";
/// The panel version stock packages carry (1.00).
pub const STOCK_PANEL_VERSION: [u8; 4] = *b"0100";
/// The MAIN version every patched package carries (1.15), whatever patches
/// it holds. The mixer only shows the version and offers a newer one at
/// startup; menu-loader lists the installed patches and their versions.
pub const PATCHED_VERSION: [u8; 4] = *b"0115";

const L6MAX_FORMAT: &str = "l6max-observed-v1";
const L6_FORMAT: &str = "l6-observed-v1";

#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(rename_all = "lowercase")]
pub enum Model {
    L6,
    L6max,
}

impl Model {
    pub const ALL: [Model; 2] = [Model::L6, Model::L6max];

    /// The name scenarios, cases and reports use: `l6` or `l6max`.
    pub fn name(self) -> &'static str {
        match self {
            Model::L6 => "l6",
            Model::L6max => "l6max",
        }
    }

    /// The name packages, the patch catalog and update reports use: `L6` or
    /// `L6max`.
    pub fn label(self) -> &'static str {
        match self {
            Model::L6 => "L6",
            Model::L6max => "L6max",
        }
    }

    pub fn from_name(name: &str) -> Option<Model> {
        Model::ALL.into_iter().find(|model| model.name() == name)
    }

    pub fn from_label(label: &str) -> Option<Model> {
        Model::ALL.into_iter().find(|model| model.label() == label)
    }

    /// The update package's file name on the SD card.
    pub fn package_file_name(self) -> &'static str {
        match self {
            Model::L6 => "L6.BIN",
            Model::L6max => "L6max.bin",
        }
    }

    /// Guest address the MAIN image loads at.
    pub fn main_base(self) -> u32 {
        profile(self).main_base
    }

    /// Package offset of the MAIN trailer: MAIN length, then the four version
    /// digits.
    pub fn main_trailer_offset(self) -> usize {
        profile(self).main_trailer
    }

    /// End of the MAIN slot: the guest address of the MAIN trailer, which a
    /// grown MAIN image must not reach.
    pub fn slot_end(self) -> u32 {
        let profile = profile(self);
        profile.main_base + (profile.main_trailer - 0x200) as u32
    }

    /// SHA-256 of the supported stock MAIN image (1.10).
    pub fn stock_main_sha256(self) -> &'static str {
        match self {
            Model::L6 => "827c7c5cb2a43cc3a011d4556462b8cb9539242f817ebeed7727d66a36c03799",
            Model::L6max => "980d92272c1f84bed200b4d2f83575202f6a00082c928d99b9e8b53af8d44461",
        }
    }

    /// The stock MAIN image's reset vector, which tells the models apart.
    pub fn stock_reset_vector(self) -> u32 {
        match self {
            Model::L6 => 0x8000_1415,
            Model::L6max => 0x8000_0415,
        }
    }

    pub fn from_stock_reset_vector(reset_vector: u32) -> Option<Model> {
        Model::ALL
            .into_iter()
            .find(|model| model.stock_reset_vector() == reset_vector)
    }

    /// Where the first firmware patch payload goes, for a MAIN image of
    /// `main_length` bytes. L6max patches start at a fixed address inside its
    /// MAIN slot; L6 patches follow the MAIN image, 16-byte aligned.
    pub fn first_payload(self, main_length: usize) -> u32 {
        match self {
            Model::L6 => (self.main_base() + main_length as u32 + 15) & !15,
            Model::L6max => 0x8010_8000,
        }
    }

    /// The RAM the Patch loader reserves for SD modules, as `start..end`.
    pub fn sd_module_region(self) -> core::ops::Range<u32> {
        match self {
            Model::L6 => 0x800c_0000..0x800c_8000,
            Model::L6max => 0x8011_0000..0x8011_8000,
        }
    }
}

#[derive(Clone, Copy)]
struct Profile {
    model: Model,
    label: &'static [u8],
    format: &'static str,
    total_size: usize,
    board_mask: u32,
    main_base: u32,
    main_trailer: usize,
    panel_start: usize,
    panel_trailer: usize,
}

const L6: Profile = Profile {
    model: Model::L6,
    label: b"L6",
    format: L6_FORMAT,
    total_size: 0x395200,
    board_mask: 1,
    main_base: 0x80001000,
    main_trailer: 0x2851f8,
    panel_start: 0x285200,
    panel_trailer: 0x2951f8,
};
const L6MAX: Profile = Profile {
    model: Model::L6max,
    label: b"L6max",
    format: L6MAX_FORMAT,
    total_size: 0x1a8200,
    board_mask: 0x40,
    main_base: 0x80000000,
    main_trailer: 0x1a31f8,
    panel_start: 0x1a3200,
    panel_trailer: 0x1a71f8,
};

fn segments_for(profile: Profile, main_length: usize) -> Vec<(&'static str, usize, usize)> {
    let (main_trailer, panel_start, panel_trailer, total_size) = (
        profile.main_trailer,
        profile.panel_start,
        profile.panel_trailer,
        profile.total_size,
    );
    let panel_end = panel_trailer + 8;
    let package_footer = total_size - 0x40;
    if profile.model == Model::L6max {
        vec![
            ("system_header.bin", 0x000000, 0x000100),
            ("main_header.bin", 0x000100, 0x000200),
            ("main_firmware.bin", 0x000200, 0x200 + main_length),
            (
                "padding_before_secondary.bin",
                0x200 + main_length,
                main_trailer,
            ),
            ("main_trailer.bin", main_trailer, panel_start),
            ("secondary_firmware.bin", panel_start, 0x1a6cd8),
            ("padding_after_secondary.bin", 0x1a6cd8, panel_trailer),
            ("secondary_trailer.bin", panel_trailer, panel_end),
            ("final_padding.bin", panel_end, package_footer),
            ("package_footer.bin", package_footer, total_size),
        ]
    } else {
        vec![
            ("system_header.bin", 0, 0x100),
            ("main_header.bin", 0x100, 0x200),
            ("main_firmware.bin", 0x200, 0x200 + main_length),
            (
                "padding_before_secondary.bin",
                0x200 + main_length,
                main_trailer,
            ),
            ("main_trailer.bin", main_trailer, panel_start),
            ("secondary_firmware.bin", panel_start, 0x288680),
            ("padding_after_secondary.bin", 0x288680, panel_trailer),
            ("secondary_trailer.bin", panel_trailer, panel_end),
            ("resource_region.bin", panel_end, package_footer),
            ("package_footer.bin", package_footer, total_size),
        ]
    }
}

/// The package's segment layout for a given main-image length: offsets are
/// fixed, only `main_firmware.bin`'s end (and everything after it, up to the
/// trailer) shifts.
pub fn layout_for(model: Model, main_length: usize) -> Vec<(&'static str, usize, usize)> {
    segments_for(profile(model), main_length)
}

fn profile(model: Model) -> Profile {
    match model {
        Model::L6 => L6,
        Model::L6max => L6MAX,
    }
}

fn detect(data: &[u8]) -> Result<Profile, String> {
    if data.starts_with(b"L6max System Data") {
        Ok(L6MAX)
    } else if data.starts_with(b"L6 System Data") {
        Ok(L6)
    } else {
        Err("unknown system header (expected L6 or L6max)".into())
    }
}

/// Identify a complete update package from its system header, after checking
/// its layout and the MAIN payload byte-sum checksum.
pub fn model_from_package(data: &[u8]) -> Result<Model, String> {
    check_layout(data)?;
    let actual = data[0x200..]
        .iter()
        .fold(0u32, |sum, &byte| sum.wrapping_add(u32::from(byte)));
    if u32::from_be_bytes(data[0x1fc..0x200].try_into().unwrap()) != actual {
        return Err("MAIN payload byte-sum checksum mismatch".into());
    }
    Ok(detect(data)?.model)
}

/// Identify an extracted MAIN image from its reset vector. This is also the
/// discriminator used by QEMU at boot: L6 starts at 0x80001000 and L6max at
/// 0x80000000.
pub fn model_from_main(data: &[u8]) -> Result<Model, String> {
    if data.len() < 8 {
        return Err("MAIN image is too short to contain a vector table".into());
    }
    let reset = u32le(data, 4);
    let stack = u32le(data, 0);
    if !(0x20000000..0x40000000).contains(&stack) || reset & 1 == 0 {
        return Err(format!(
            "invalid MAIN vector table ({stack:#010x}, {reset:#010x})"
        ));
    }
    if (0x80001000..0x80200000).contains(&(reset & !1)) {
        Ok(Model::L6)
    } else if (0x80000000..0x80001000).contains(&(reset & !1)) {
        Ok(Model::L6max)
    } else {
        Err(format!("unknown MAIN reset vector {reset:#010x}"))
    }
}

/// Identify the firmware image installed in the simulator's 2 MiB MAIN NOR.
/// The application image starts at offset 0x50000 in the device flash; this
/// is the image QEMU executes when persistent device state is enabled.
pub fn model_from_main_nor(data: &[u8]) -> Result<Model, String> {
    const MAIN_IMAGE_OFFSET: usize = 0x50000;
    let main = data
        .get(MAIN_IMAGE_OFFSET..)
        .ok_or("MAIN NOR is too short to contain the installed firmware")?;
    model_from_main(main)
}

fn profile_for_manifest(manifest: &Value) -> Result<Profile, String> {
    match manifest["format"].as_str() {
        Some(L6_FORMAT) => Ok(L6),
        Some(L6MAX_FORMAT) => Ok(L6MAX),
        _ => Err("unsupported manifest format".into()),
    }
}

fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn u32le(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
}

pub fn check_layout(data: &[u8]) -> Result<(), String> {
    let p = detect(data)?;
    if data.len() != p.total_size {
        return Err(format!(
            "{}: expected {} bytes, got {}",
            String::from_utf8_lossy(p.label),
            p.total_size,
            data.len()
        ));
    }
    let mut main_label = p.label.to_vec();
    main_label.extend_from_slice(b" Main Data");
    if !data[0x100..].starts_with(&main_label) {
        return Err("missing MAIN header".into());
    }
    if u32le(data, 0x60) != 2
        || u32le(data, 0x64) != p.board_mask
        || &data[0x68..0x6c] != b"BOOT"
        || &data[0x74..0x78] != b"MAIN"
        || u32le(data, 0x6c) != 0
        || u32le(data, 0x70) != 0
        || u32le(data, 0x78) != 0x100
        || u32le(data, 0x7c) != (p.total_size - 0x100) as u32
    {
        return Err("unexpected package table".into());
    }
    let main_length = u32le(data, p.main_trailer) as usize;
    if !(8..=p.main_trailer - 0x200).contains(&main_length)
        || u32le(data, p.total_size - 8) as usize != main_length
    {
        return Err("main length exceeds its slot or disagrees with footer".into());
    }
    let panel_length = u32le(data, p.panel_trailer) as usize;
    if !(8..=p.panel_trailer - p.panel_start).contains(&panel_length) {
        return Err("panel image exceeds its reserved slot".into());
    }
    for (start, end, trailer, base) in [
        (0x200, 0x200 + main_length, p.main_trailer, p.main_base),
        (
            p.panel_start,
            p.panel_start + panel_length,
            p.panel_trailer,
            0x08000000u32,
        ),
    ] {
        if u32le(data, trailer) != (end - start) as u32 {
            return Err(format!("image length at 0x{trailer:x} does not match"));
        }
        let stack = u32le(data, start);
        let reset = u32le(data, start + 4);
        if !(0x20000000..0x40000000).contains(&stack)
            || reset <= base
            || reset >= base + (end - start) as u32
            || reset & 1 == 0
        {
            return Err(format!("no plausible Cortex-M vector table at 0x{start:x}"));
        }
    }
    if data[p.main_trailer + 4..p.main_trailer + 8] != data[p.total_size - 4..p.total_size]
        || !data[p.panel_trailer + 4..p.panel_trailer + 8]
            .iter()
            .all(u8::is_ascii_digit)
    {
        return Err("main version markers disagree".into());
    }
    Ok(())
}

/// Splits a validated package into its named segments and a fresh manifest,
/// without touching the filesystem. Shared by `unpack` (writes files) and
/// `unpack_bytes` (keeps everything in memory, for the in-browser patcher).
fn unpack_core(data: &[u8]) -> Result<(Value, Vec<(&'static str, Vec<u8>)>), String> {
    check_layout(data)?;
    let p = detect(data)?;
    let mut entries = Vec::new();
    let mut parts = Vec::new();
    for (name, start, end) in segments_for(p, u32le(data, p.main_trailer) as usize) {
        let part = data[start..end].to_vec();
        entries.push(
            json!({"file":name,"offset":start,"size":end-start,"original_sha256":sha256(&part)}),
        );
        parts.push((name, part));
    }
    let manifest = json!({"format":p.format,"model":if p.model == Model::L6 {"L6"} else {"L6max"},"original_sha256":sha256(data),"segments":entries});
    Ok((manifest, parts))
}

pub fn unpack(source: &Path, out: &Path) -> Result<(), String> {
    let data = fs::read(source).map_err(|e| e.to_string())?;
    let (manifest, parts) = unpack_core(&data)?;
    fs::create_dir(out).map_err(|e| e.to_string())?;
    for (name, part) in &parts {
        fs::write(out.join(name), part).map_err(|e| e.to_string())?;
    }
    fs::write(
        out.join("manifest.json"),
        format!(
            "{}\n",
            serde_json::to_string_pretty(&manifest).map_err(|e| e.to_string())?
        ),
    )
    .map_err(|e| e.to_string())?;
    println!("Unpacked {} into {}", source.display(), out.display());
    let p = detect(&data)?;
    println!(
        "Model: {}",
        if p.model == Model::L6 { "L6" } else { "L6max" }
    );
    println!(
        "Main version: {}",
        String::from_utf8_lossy(&data[p.main_trailer + 4..p.main_trailer + 8])
    );
    println!(
        "Panel version: {}",
        String::from_utf8_lossy(
            &data[detect(&data)?.panel_trailer + 4..detect(&data)?.panel_trailer + 8]
        )
    );
    Ok(())
}

/// In-memory equivalent of `unpack`: validates `data` and returns its fresh
/// manifest plus a name → bytes map of every segment, with nothing written to
/// disk. Used by the WASM patcher, which never has a filesystem.
pub fn unpack_bytes(
    data: &[u8],
) -> Result<(Value, std::collections::BTreeMap<String, Vec<u8>>), String> {
    let (manifest, parts) = unpack_core(data)?;
    Ok((
        manifest,
        parts.into_iter().map(|(n, b)| (n.to_string(), b)).collect(),
    ))
}

/// Assembles a package from its manifest and segments, reading each named
/// segment through `read`. Shared by `repack` (reads files) and
/// `repack_bytes` (reads from an in-memory map). Returns the assembled bytes
/// and the list of segments that changed since the manifest's own
/// `original_sha256` was recorded.
fn repack_core(
    manifest: &Value,
    read: impl Fn(&str) -> Result<Vec<u8>, String>,
    recompute_checksum: bool,
) -> Result<(Vec<u8>, Vec<&'static str>), String> {
    let p = profile_for_manifest(manifest)?;
    if manifest["model"] != if p.model == Model::L6 { "L6" } else { "L6max" } {
        return Err("manifest model and format disagree".into());
    }
    let entries = manifest["segments"]
        .as_array()
        .ok_or("manifest has no segment list")?;
    let trailer = read("main_trailer.bin")?;
    if trailer.len() != 8 {
        return Err("main trailer must have eight bytes".into());
    }
    let main_length = u32le(&trailer, 0) as usize;
    if !(8..=p.main_trailer - 0x200).contains(&main_length) {
        return Err("main image exceeds its reserved slot".into());
    }
    let layout = segments_for(p, main_length);
    if entries.len() != layout.len() {
        return Err("manifest segment list does not match this layout".into());
    }
    let mut assembled = Vec::with_capacity(p.total_size);
    let mut changed = Vec::new();
    for (entry, (name, start, end)) in entries.iter().zip(layout) {
        if entry["file"] != name
            || entry["offset"].as_u64() != Some(start as u64)
            || entry["size"].as_u64() != Some((end - start) as u64)
        {
            return Err(format!("manifest mismatch for {name}"));
        }
        let part = read(name)?;
        if part.len() != end - start {
            return Err(format!(
                "{name}: expected {} bytes, got {}",
                end - start,
                part.len()
            ));
        }
        if entry["original_sha256"].as_str() != Some(&sha256(&part)) {
            changed.push(name);
        }
        assembled.extend_from_slice(&part);
    }
    check_layout(&assembled)?;
    if recompute_checksum {
        let sum = assembled[0x200..]
            .iter()
            .fold(0u32, |sum, &byte| sum.wrapping_add(byte as u32));
        let checksum = sum.to_be_bytes();
        if assembled[0x1fc..0x200] != checksum && !changed.contains(&"main_header.bin") {
            changed.push("main_header.bin");
        }
        assembled[0x1fc..0x200].copy_from_slice(&checksum);
    }
    let digest = sha256(&assembled);
    if changed.is_empty() && manifest["original_sha256"].as_str() != Some(&digest) {
        return Err("assembled hash disagrees with the manifest".into());
    }
    Ok((assembled, changed))
}

pub fn repack(input: &Path, output: &Path, recompute_checksum: bool) -> Result<(), String> {
    let manifest: Value =
        serde_json::from_slice(&fs::read(input.join("manifest.json")).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
    let (assembled, changed) = repack_core(
        &manifest,
        |name| fs::read(input.join(name)).map_err(|e| e.to_string()),
        recompute_checksum,
    )?;
    let digest = sha256(&assembled);
    if output.exists() {
        return Err(format!("refusing to overwrite {}", output.display()));
    }
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(output)
        .map_err(|e| e.to_string())?;
    file.write_all(&assembled).map_err(|e| e.to_string())?;
    file.sync_all().map_err(|e| e.to_string())?;
    println!(
        "Wrote {} ({} bytes; SHA-256 {digest})",
        output.display(),
        assembled.len()
    );
    if changed.is_empty() {
        println!("Byte-identical to the original package.");
    } else {
        println!("Changed segments: {}", changed.join(", "));
        if recompute_checksum {
            println!("MAIN byte-sum checksum regenerated. Device acceptance is unverified.");
        } else {
            println!(
                "Payload checksum is preserved; use --recompute-checksum after payload edits. Device acceptance is unverified."
            );
        }
    }
    Ok(())
}

/// In-memory equivalent of `repack`: assembles the final package bytes from a
/// manifest and a name → bytes map, with nothing read from or written to
/// disk. Used by the WASM patcher.
pub fn repack_bytes(
    manifest: &Value,
    segments: &std::collections::BTreeMap<String, Vec<u8>>,
    recompute_checksum: bool,
) -> Result<Vec<u8>, String> {
    let (assembled, _changed) = repack_core(
        manifest,
        |name| {
            segments
                .get(name)
                .cloned()
                .ok_or_else(|| format!("missing segment {name}"))
        },
        recompute_checksum,
    )?;
    Ok(assembled)
}

/// Segment name to bytes, as `unpack_bytes` returns them.
pub type Segments = std::collections::BTreeMap<String, Vec<u8>>;

/// Read a directory `unpack` wrote: its manifest and every segment it lists.
pub fn read_unpacked(dir: &Path) -> Result<(Value, Segments), String> {
    let manifest: Value =
        serde_json::from_slice(&fs::read(dir.join("manifest.json")).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
    let mut segments = Segments::new();
    for entry in manifest["segments"]
        .as_array()
        .ok_or("manifest has no segment list")?
    {
        let name = entry["file"]
            .as_str()
            .ok_or("manifest segment has no file")?;
        let bytes = fs::read(dir.join(name)).map_err(|e| format!("{name}: {e}"))?;
        segments.insert(name.to_owned(), bytes);
    }
    Ok((manifest, segments))
}

/// Replace MAIN with `main`, which may only grow into the erased padding
/// that follows it, and give the package `version`. The padding, the MAIN
/// trailer, the package footer and the manifest follow the new length.
pub fn replace_main(
    manifest: &mut Value,
    segments: &mut Segments,
    main: Vec<u8>,
    version: [u8; 4],
) -> Result<(), String> {
    let model = profile_for_manifest(manifest)?.model;
    let segment = |segments: &Segments, name: &str| {
        segments
            .get(name)
            .cloned()
            .ok_or_else(|| format!("package has no {name} segment"))
    };
    let original_length = segment(segments, "main_firmware.bin")?.len();
    let padding = segment(segments, "padding_before_secondary.bin")?;
    let grown = main
        .len()
        .checked_sub(original_length)
        .ok_or("the patched MAIN image is shorter than the stock one")?;
    if grown > padding.len() || padding[..grown].iter().any(|&byte| byte != 0xff) {
        return Err("the patched MAIN image overlaps non-erased package data".into());
    }
    let length = u32::try_from(main.len()).map_err(|_| "MAIN image too long")?;
    let mut trailer = segment(segments, "main_trailer.bin")?;
    let mut footer = segment(segments, "package_footer.bin")?;
    if trailer.len() < 8 || footer.len() < 0x40 {
        return Err("short MAIN trailer or package footer".into());
    }
    trailer[..4].copy_from_slice(&length.to_le_bytes());
    trailer[4..8].copy_from_slice(&version);
    footer[0x38..0x3c].copy_from_slice(&length.to_le_bytes());
    footer[0x3c..0x40].copy_from_slice(&version);
    segments.insert("main_firmware.bin".into(), main);
    segments.insert(
        "padding_before_secondary.bin".into(),
        padding[grown..].to_vec(),
    );
    segments.insert("main_trailer.bin".into(), trailer);
    segments.insert("package_footer.bin".into(), footer);
    let entries = manifest["segments"]
        .as_array_mut()
        .ok_or("manifest has no segment list")?;
    for (name, start, end) in layout_for(model, length as usize) {
        if let Some(entry) = entries.iter_mut().find(|entry| entry["file"] == name) {
            entry["offset"] = json!(start);
            entry["size"] = json!(end - start);
        }
    }
    Ok(())
}

/// Assemble a modified package, regenerating the MAIN checksum, and check
/// that the result is still a package for the same model.
pub fn repack_modified(manifest: &Value, segments: &Segments) -> Result<Vec<u8>, String> {
    let model = profile_for_manifest(manifest)?.model;
    let packed = repack_bytes(manifest, segments, true)?;
    if model == Model::L6max {
        update::validate(&packed).map_err(|e| e.to_string())?;
    } else if model_from_package(&packed)? != model {
        return Err("the repacked package's model changed".into());
    }
    Ok(packed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn models_have_one_memory_map() {
        assert_eq!(Model::L6max.slot_end(), 0x801a_2ff8);
        assert_eq!(Model::L6.slot_end(), 0x8028_5ff8);
        assert_eq!(Model::L6max.first_payload(0x1081e8), 0x8010_8000);
        assert_eq!(Model::L6.first_payload(0x21), 0x8000_1030);
        for model in Model::ALL {
            assert_eq!(Model::from_label(model.label()), Some(model));
            assert_eq!(Model::from_name(model.name()), Some(model));
            assert_eq!(
                Model::from_stock_reset_vector(model.stock_reset_vector()),
                Some(model)
            );
            let modules = model.sd_module_region();
            assert!(model.first_payload(0) < modules.start && modules.end <= model.slot_end());
        }
    }

    fn fixture(model: Model, main_length: usize) -> Vec<u8> {
        let p = profile(model);
        let mut data = vec![0xff; p.total_size];
        let mut system = p.label.to_vec();
        system.extend_from_slice(b" System Data");
        let mut main = p.label.to_vec();
        main.extend_from_slice(b" Main Data");
        data[..system.len()].copy_from_slice(&system);
        data[0x100..0x100 + main.len()].copy_from_slice(&main);
        for (offset, value) in [
            (0x60, 2u32),
            (0x64, p.board_mask),
            (0x6c, 0),
            (0x70, 0),
            (0x78, 0x100),
            (0x7c, (p.total_size - 0x100) as u32),
            (0x200, 0x20220000),
            (0x204, p.main_base + 9),
            (p.main_trailer, main_length as u32),
            (p.panel_start, 0x20002000),
            (p.panel_start + 4, 0x08000009),
            (p.panel_trailer, 0x40),
            (p.total_size - 8, main_length as u32),
        ] {
            data[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
        }
        data[0x68..0x6c].copy_from_slice(b"BOOT");
        data[0x74..0x78].copy_from_slice(b"MAIN");
        data[p.main_trailer + 4..p.main_trailer + 8].copy_from_slice(&STOCK_VERSION);
        data[p.panel_trailer + 4..p.panel_trailer + 8].copy_from_slice(&STOCK_PANEL_VERSION);
        data[p.total_size - 4..].copy_from_slice(&STOCK_VERSION);
        if model == Model::L6 {
            data[p.panel_trailer + 8..p.panel_trailer + 16].copy_from_slice(b"RESOURCE");
        }
        let sum = data[0x200..]
            .iter()
            .fold(0u32, |s, &b| s.wrapping_add(b as u32));
        data[0x1fc..0x200].copy_from_slice(&sum.to_be_bytes());
        data
    }

    #[test]
    fn both_models_roundtrip_in_memory_and_keep_model_metadata() {
        for model in [Model::L6, Model::L6max] {
            let data = fixture(model, 0x40);
            check_layout(&data).unwrap();
            assert_eq!(model_from_package(&data).unwrap(), model);
            let (manifest, parts) = unpack_bytes(&data).unwrap();
            assert_eq!(
                manifest["model"],
                if model == Model::L6 { "L6" } else { "L6max" }
            );
            assert_eq!(repack_bytes(&manifest, &parts, true).unwrap(), data);
            if model == Model::L6 {
                assert_eq!(&parts["resource_region.bin"][..8], b"RESOURCE");
            }
        }
    }

    #[test]
    fn both_models_roundtrip_on_disk() {
        for model in [Model::L6, Model::L6max] {
            let id = if model == Model::L6 { "l6" } else { "l6max" };
            let root = std::env::temp_dir().join(format!("l6fw-{id}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&root);
            fs::create_dir(&root).unwrap();
            let source = root.join("source.bin");
            let unpacked = root.join("parts");
            let packed = root.join("roundtrip.bin");
            let bytes = fixture(model, 0x40);
            fs::write(&source, &bytes).unwrap();
            unpack(&source, &unpacked).unwrap();
            repack(&unpacked, &packed, true).unwrap();
            assert_eq!(fs::read(&packed).unwrap(), bytes);
            assert!(repack(&unpacked, &packed, true).is_err());
            fs::remove_dir_all(root).unwrap();
        }
    }

    #[test]
    fn a_longer_main_moves_the_padding_trailer_and_footer() {
        for model in Model::ALL {
            let original = fixture(model, 0x40);
            let (manifest, segments) = unpack_bytes(&original).unwrap();
            let mut main = segments["main_firmware.bin"].clone();
            main.extend_from_slice(&[0xaa; 16]);
            let replace = |main: Vec<u8>| {
                let (mut manifest, mut segments) = (manifest.clone(), segments.clone());
                replace_main(&mut manifest, &mut segments, main, PATCHED_VERSION)
                    .map(|()| repack_bytes(&manifest, &segments, true).unwrap())
            };
            let assembled = replace(main.clone()).unwrap();
            let trailer = profile(model).main_trailer;
            assert_eq!(u32le(&assembled, trailer), 0x50);
            assert_eq!(&assembled[trailer + 4..trailer + 8], &PATCHED_VERSION);
            check_layout(&assembled).unwrap();
            assert!(replace(main[..0x30].to_vec()).is_err());
            let mut padding = segments.clone();
            padding.get_mut("padding_before_secondary.bin").unwrap()[4] = 0;
            assert!(
                replace_main(&mut manifest.clone(), &mut padding, main, PATCHED_VERSION).is_err()
            );
        }
    }

    #[test]
    fn rejects_wrong_masks_lengths_and_vector_addresses() {
        for model in [Model::L6, Model::L6max] {
            let p = profile(model);
            let good = fixture(model, 0x40);
            assert!(check_layout(&good).is_ok());
            let mut bad = good.clone();
            bad[0x64..0x68].copy_from_slice(&0u32.to_le_bytes());
            assert!(check_layout(&bad).is_err());
            let mut bad = good.clone();
            bad[0x204..0x208].copy_from_slice(&0x80000008u32.to_le_bytes());
            assert!(check_layout(&bad).is_err());
            let mut bad = good.clone();
            bad[p.total_size - 8..p.total_size - 4].copy_from_slice(&0x99u32.to_le_bytes());
            assert!(check_layout(&bad).is_err());
            assert!(check_layout(&good[..good.len() - 1]).is_err());
        }
    }

    #[test]
    fn identifies_model_from_extracted_main_reset_vector() {
        for (model, reset) in [(Model::L6, 0x80001415u32), (Model::L6max, 0x80000415u32)] {
            let mut image = vec![0; 8];
            image[..4].copy_from_slice(&0x20220000u32.to_le_bytes());
            image[4..8].copy_from_slice(&reset.to_le_bytes());
            assert_eq!(model_from_main(&image).unwrap(), model);
        }
    }

    #[test]
    fn identifies_model_from_installed_main_nor_image() {
        let mut nor = vec![0xff; 0x200000];
        for (model, reset) in [(Model::L6, 0x80001415u32), (Model::L6max, 0x80000415u32)] {
            nor[0x50000..0x50004].copy_from_slice(&0x20220000u32.to_le_bytes());
            nor[0x50004..0x50008].copy_from_slice(&reset.to_le_bytes());
            assert_eq!(model_from_main_nor(&nor).unwrap(), model);
        }
        assert!(model_from_main_nor(&nor[..0x50007]).is_err());
    }

    #[test]
    fn main_model_detection_enforces_vector_boundaries() {
        for (model, reset) in [
            (Model::L6max, 0x80000fffu32),
            (Model::L6, 0x80001001u32),
            (Model::L6, 0x801fffffu32),
        ] {
            let mut image = vec![0; 8];
            image[..4].copy_from_slice(&0x20220000u32.to_le_bytes());
            image[4..8].copy_from_slice(&reset.to_le_bytes());
            assert_eq!(model_from_main(&image).unwrap(), model);
        }
    }

    #[test]
    fn main_model_detection_rejects_invalid_vectors() {
        let mut image = vec![0; 8];
        image[..4].copy_from_slice(&0x20220000u32.to_le_bytes());
        image[4..8].copy_from_slice(&0x80001415u32.to_le_bytes());
        assert!(model_from_main(&image[..7]).is_err());

        for (stack, reset) in [
            (0x1fffffffu32, 0x80001415u32),
            (0x40000000, 0x80001415),
            (0x20220000, 0x80001410),
            (0x20220000, 0x7ffffffd),
            (0x20220000, 0x80200001),
        ] {
            image[..4].copy_from_slice(&stack.to_le_bytes());
            image[4..8].copy_from_slice(&reset.to_le_bytes());
            assert!(
                model_from_main(&image).is_err(),
                "{stack:#010x} {reset:#010x}"
            );
        }
    }
}

pub mod update;

impl std::fmt::Display for Model {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

impl std::str::FromStr for Model {
    type Err = String;

    fn from_str(name: &str) -> Result<Self, Self::Err> {
        Model::ALL
            .into_iter()
            .find(|model| model.name() == name)
            .ok_or_else(|| format!("unknown model {name:?}; expected l6 or l6max"))
    }
}
