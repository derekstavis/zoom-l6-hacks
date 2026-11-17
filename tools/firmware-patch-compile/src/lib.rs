//! Native-only: compiles one patch's `payload.rs` to a flat ARM Thumb binary
//! via `rustc --target=thumbv7em-none-eabihf` + `lld` + `llvm-objcopy`. Shared
//! by the `l6patch` CLI and the WASM-template prebuild tool, so both
//! ever compile a payload through exactly this one code path — the whole
//! premise of shipping precompiled templates depends on this being the same
//! compile every time, not two paths that could silently drift apart.
//!
//! Never built for `wasm32-unknown-unknown`: it shells out to `rustc`/`lld`/
//! `llvm-objcopy`, which don't exist in a browser.
use sha2::{Digest, Sha256};
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

fn run(command: &mut Command) -> Result<String, Box<dyn std::error::Error>> {
    let output = command.output()?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).into_owned().into());
    }
    Ok(String::from_utf8(output.stdout)?)
}

const HOMEBREW_LLD: &str = "/opt/homebrew/bin/ld.lld";
const HOMEBREW_NM: &str = "/opt/homebrew/opt/llvm/bin/llvm-nm";
const HOMEBREW_OBJCOPY: &str = "/opt/homebrew/opt/llvm/bin/llvm-objcopy";
const HOMEBREW_READELF: &str = "/opt/homebrew/opt/llvm/bin/llvm-readelf";

/// Load-time fixups for addresses that refer back into a linked patch image.
/// PC-relative calls and references need no fixup when the complete image moves.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Relocation {
    Abs32 { offset: u32 },
    ThumbMov32 { low_offset: u32, high_offset: u32 },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FunctionHookSiteMetadata {
    pub address: u32,
    pub displaced_len: u16,
    pub continuation: u8,
    pub dynamic_dispatch: bool,
    pub name: String,
    pub guard_sha256: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HookRegistration {
    Entry {
        site: String,
        handler: String,
    },
    Offset {
        function: String,
        offset: u32,
        displaced_len: u16,
        guard_sha256: String,
        handler: String,
    },
    CallPre {
        function: String,
        address: u32,
        argument_count: usize,
        handler: String,
    },
    CallPost {
        function: String,
        address: u32,
        argument_count: usize,
        handler: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HookWindow {
    pub address: u32,
    pub target_symbol: String,
    pub name: String,
    pub length: usize,
    pub continuation: u8,
}

/// The active toolchain's `bin` directory, which holds `rust-lld` and, with the
/// `llvm-tools` component (listed in `rust-toolchain.toml`), `llvm-nm` and
/// `llvm-objcopy`.
fn rustc_executable() -> Option<PathBuf> {
    let sysroot = run(Command::new("rustc").args(["--print", "sysroot"])).ok()?;
    let rustc = PathBuf::from(sysroot.trim()).join("bin/rustc");
    rustc.is_file().then_some(rustc)
}

/// Target-library flags for the immediate-abort sysroot. Changing them
/// changes the cache key in [`immediate_abort_sysroot`].
const IMMEDIATE_ABORT_LIBRARY_FLAGS: &[&str] = &[
    "--target=thumbv7em-none-eabihf",
    "--edition=2024",
    "-Copt-level=z",
    "-Cdebuginfo=0",
    "-Zunstable-options",
    "-Cpanic=immediate-abort",
    "-Zforce-unstable-if-unmarked",
    "-Zlocation-detail=none",
];

/// Build `core` and `compiler_builtins` with the immediate-abort panic
/// strategy into a sysroot, once per toolchain, and return its path.
///
/// Every payload uses it so that a panic compiles to a single trap
/// instruction. The prebuilt `core` formats a panic message and passes source
/// locations to the panic handler: about 1.4 KB of formatting code plus the
/// message and file-name strings, and in a relocatable module one relocation
/// per location, all of which the handler ignores. With
/// `-Zlocation-detail=none`, a location that a `#[track_caller]` call still
/// passes names no file, line or column. Both crates come from the `rust-src`
/// component and have no crates.io dependencies, so the build needs no
/// network access.
fn immediate_abort_sysroot(rustc: &Path) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let toolchain = run(Command::new(rustc).args(["--print", "sysroot"]))?;
    let library = Path::new(toolchain.trim()).join("lib/rustlib/src/rust/library");
    if !library.join("core/src/lib.rs").is_file() {
        return Err(format!(
            "the rust-src component is missing from {}; it is listed in rust-toolchain.toml",
            toolchain.trim()
        )
        .into());
    }
    let mut key = Sha256::new();
    key.update(run(Command::new(rustc).arg("-vV"))?);
    key.update(IMMEDIATE_ABORT_LIBRARY_FLAGS.join(" "));
    let key = format!("{:x}", key.finalize());
    let cache =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/l6-immediate-abort-sysroot");
    let root = cache.join(&key[..16]);
    let target_lib = |root: &Path| root.join("lib/rustlib/thumbv7em-none-eabihf/lib");
    if target_lib(&root)
        .join("libcompiler_builtins.rlib")
        .is_file()
    {
        return Ok(root);
    }

    // Build in a private directory and rename it into place, so concurrent
    // builders never see a partial sysroot.
    let staging = cache.join(format!("{}.{}.tmp", &key[..16], std::process::id()));
    let _ = fs::remove_dir_all(&staging);
    let lib = target_lib(&staging);
    fs::create_dir_all(&lib)?;
    run(Command::new(rustc)
        .args(IMMEDIATE_ABORT_LIBRARY_FLAGS)
        .args(["--crate-name", "core", "--crate-type", "rlib"])
        .arg(library.join("core/src/lib.rs"))
        .arg("--out-dir")
        .arg(&lib))?;
    // These configuration values are what compiler-builtins' build script
    // selects for thumbv7em-none-eabihf as a dependency of the standard
    // library: memory intrinsics, unmangled symbol names and Thumb-2 code.
    run(Command::new(rustc)
        .args(IMMEDIATE_ABORT_LIBRARY_FLAGS)
        .args(["--crate-name", "compiler_builtins", "--crate-type", "rlib"])
        .args(["-Ccodegen-units=10000", "--sysroot"])
        .arg(&staging)
        .args([
            "--cfg",
            "feature=\"compiler-builtins\"",
            "--cfg",
            "feature=\"mem\"",
            "--cfg",
            "feature=\"unmangled-names\"",
            "--cfg",
            "feature=\"arch\"",
            "--cfg",
            "thumb",
            "--cfg",
            "intrinsics_enabled",
            "--cfg",
            "optimizations_enabled",
        ])
        .arg(library.join("compiler-builtins/compiler-builtins/src/lib.rs"))
        .arg("--out-dir")
        .arg(&lib))?;
    if fs::rename(&staging, &root).is_err() {
        // Another builder finished first; use its sysroot.
        let _ = fs::remove_dir_all(&staging);
        if !target_lib(&root)
            .join("libcompiler_builtins.rlib")
            .is_file()
        {
            return Err(format!("could not install the sysroot at {}", root.display()).into());
        }
    }
    Ok(root)
}

fn toolchain_bin() -> Option<PathBuf> {
    let sysroot = run(Command::new("rustc").args(["--print", "sysroot"])).ok()?;
    fs::read_dir(PathBuf::from(sysroot.trim()).join("lib/rustlib"))
        .ok()?
        .flatten()
        .map(|host| host.path().join("bin"))
        .find(|bin| bin.join("rust-lld").is_file())
}

/// An explicit `variable` override, then a Homebrew LLVM install, then the Rust
/// toolchain's own copy.
fn llvm_tool(
    variable: &str,
    homebrew: &str,
    bundled: &str,
) -> Result<PathBuf, Box<dyn std::error::Error>> {
    if let Some(path) = std::env::var_os(variable) {
        return Ok(PathBuf::from(path));
    }
    if Path::new(homebrew).exists() {
        return Ok(PathBuf::from(homebrew));
    }
    toolchain_bin()
        .map(|bin| bin.join(bundled))
        .filter(|path| path.is_file())
        .ok_or_else(|| {
            format!("{bundled} not found; run `rustup component add llvm-tools` or set {variable}")
                .into()
        })
}

/// Compiles one patch's payload at `base` against `main_firmware.bin` under
/// `source`. Continuations and hook veneers are generated from hook attributes
/// and the shared function catalog. Returns the flat binary and symbols.
pub fn compile(
    source: &Path,
    root: &Path,
    output: &Path,
    name: &str,
    base: u32,
) -> Result<(Vec<u8>, String), Box<dyn std::error::Error>> {
    compile_inner(
        source,
        root,
        output,
        name,
        base,
        None,
        false,
        false,
        true,
        &[],
    )
}

/// Compile a deterministic sentinel-firmware variant for browser template
/// window discovery. Guard validation is performed against the real image
/// before this variant is built.
pub fn compile_sentinel(
    source: &Path,
    root: &Path,
    output: &Path,
    name: &str,
    base: u32,
) -> Result<(Vec<u8>, String), Box<dyn std::error::Error>> {
    compile_inner(
        source,
        root,
        output,
        name,
        base,
        None,
        false,
        false,
        false,
        &[],
    )
}

/// Compile with continuation metadata collected from additional patch roots.
/// Used by the loader, whose resident dispatcher also needs stock continuations
/// for patches it can dispatch dynamically.
pub fn compile_with_continuations(
    source: &Path,
    root: &Path,
    continuation_roots: &[&Path],
    output: &Path,
    name: &str,
    base: u32,
) -> Result<(Vec<u8>, String), Box<dyn std::error::Error>> {
    compile_with_continuations_cfg(source, root, continuation_roots, output, name, base, &[])
}

/// Compile with named additional Rust cfgs. Used for emulator-only loader
/// experiments that must not be enabled in normal firmware builds.
pub fn compile_with_continuations_cfg(
    source: &Path,
    root: &Path,
    continuation_roots: &[&Path],
    output: &Path,
    name: &str,
    base: u32,
    extra_cfgs: &[&str],
) -> Result<(Vec<u8>, String), Box<dyn std::error::Error>> {
    let _ = continuation_roots;
    compile_inner(
        source, root, output, name, base, None, false, true, true, extra_cfgs,
    )
}

/// Compile a deterministic sentinel-firmware variant of a loader template.
/// The loader still needs every dynamic dispatcher and continuation emitted,
/// but the sentinel firmware intentionally differs at guarded hook windows.
pub fn compile_sentinel_with_continuations(
    source: &Path,
    root: &Path,
    continuation_roots: &[&Path],
    output: &Path,
    name: &str,
    base: u32,
) -> Result<(Vec<u8>, String), Box<dyn std::error::Error>> {
    let _ = continuation_roots;
    compile_inner(
        source,
        root,
        output,
        name,
        base,
        None,
        false,
        true,
        false,
        &[],
    )
}

/// Compile a payload with its optional SD-specific initialization entry.
pub fn compile_sd(
    source: &Path,
    root: &Path,
    output: &Path,
    name: &str,
    base: u32,
) -> Result<(Vec<u8>, String), Box<dyn std::error::Error>> {
    compile_inner(
        source,
        root,
        output,
        name,
        base,
        Some("sd_dynamic"),
        false,
        false,
        true,
        &[],
    )
}

/// Compile an SD module and retain the small set of absolute relocations whose
/// targets are within the module. The relocation records are extracted from
/// the linked ELF; the flat image remains the same compact static codegen.
pub fn compile_sd_relocatable(
    source: &Path,
    root: &Path,
    output: &Path,
    name: &str,
    base: u32,
) -> Result<(Vec<u8>, usize, String, Vec<Relocation>), Box<dyn std::error::Error>> {
    let (image, symbols) = compile_inner(
        source,
        root,
        output,
        name,
        base,
        Some("sd_dynamic"),
        true,
        false,
        true,
        &[],
    )?;
    let elf = output.join(format!("{name}.payload.elf"));
    let memory_len = symbols
        .lines()
        .find(|line| line.split_whitespace().last() == Some("__module_memory_end"))
        .and_then(|line| u32::from_str_radix(line.split_whitespace().next()?, 16).ok())
        .and_then(|end| end.checked_sub(base))
        .map(|length| length as usize)
        .unwrap_or(image.len())
        .max(image.len());
    let relocations = parse_relocations(&elf, base, image.len(), memory_len)?;
    Ok((image, memory_len, symbols, relocations))
}

/// Compile several patch payload roots into one relocatable SD module. Each
/// source remains namespaced as a Rust module, while the shared patch runtime
/// and any dependency modules are linked exactly once.
pub fn compile_sd_relocatable_composed(
    source: &Path,
    roots: &[(&str, &Path)],
    effect_replacements: &[(String, String)],
    output: &Path,
    name: &str,
    base: u32,
) -> Result<(Vec<u8>, usize, String, Vec<Relocation>), Box<dyn std::error::Error>> {
    if roots.is_empty() {
        return Err("composed module has no patch roots".into());
    }
    let mut source_text = String::from("#![no_std]\n#![no_main]\n#[path = \"");
    let firmware = fs::read(source.join("main_firmware.bin"))?;
    let model_dir = if firmware.get(4..8) == Some(&0x8000_1415u32.to_le_bytes()) {
        "l6"
    } else {
        "l6max"
    };
    let patchrt = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../patches/api/patchrt.rs")
        .canonicalize()?;
    source_text.push_str(&patchrt.display().to_string());
    source_text.push_str("\"]\n#[macro_use]\npub mod patch_api;\n");
    let api_root =
        Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("../../patches/api/{model_dir}"));
    source_text.push_str(&format!(
        "#[path = {:?}]\nmod firmware_bindings;\n#[path = {:?}]\nmod hook_catalog;\n",
        api_root
            .join("mod.rs")
            .canonicalize()?
            .display()
            .to_string(),
        api_root
            .join("hooks.rs")
            .canonicalize()?
            .display()
            .to_string(),
    ));
    for (patch_name, root) in roots {
        let module_name = patch_name.replace('-', "_");
        let payload = root.join("src/payload.rs").canonicalize()?;
        source_text.push_str(&format!(
            "#[path = {:?}]\npub mod {module_name};\n",
            payload.display().to_string()
        ));
    }
    let assignments = effect_replacements
        .iter()
        .map(|(slot, id)| (slot.as_str(), id.as_str()))
        .collect::<std::collections::BTreeMap<_, _>>();
    let selected_effects = assignments
        .iter()
        .map(|(slot, id)| {
            let package = firmware_patch_core::patch_catalog()
                .iter()
                .find(|package| package.effects.iter().any(|effect| effect.id == **id))
                .ok_or_else(|| format!("unknown selected effect `{id}`"))?;
            let effect = package
                .effects
                .iter()
                .find(|effect| effect.id == **id)
                .expect("effect package was found");
            Ok((
                *slot,
                *id,
                package.name.replace('-', "_"),
                effect.title.clone(),
                effect.controls.clone(),
                effect.tap_action.clone(),
            ))
        })
        .collect::<Result<Vec<_>, String>>()?;
    source_text.push_str("pub mod effect_replacement_config {\n");
    source_text.push_str(&format!(
        "pub const EXPLICIT: bool = {};\n",
        !assignments.is_empty()
    ));
    source_text.push_str(
        "pub fn effect_for_stock_slot(slot: u32) -> Option<&'static str> { match slot {\n",
    );
    for (slot, effect_id, _, _, _, _) in &selected_effects {
        let stock_type = match *slot {
            "room" => 1,
            "spring" => 2,
            _ => unreachable!("effect slots are validated by firmware-patch-core"),
        };
        source_text.push_str(&format!("{stock_type} => Some({effect_id:?}),\n"));
    }
    source_text.push_str("_ => None, } }\n");
    source_text.push_str("pub fn state_size(slot: u32) -> Option<usize> { match slot {\n");
    for (slot, _, module_name, _, _, _) in &selected_effects {
        let stock_type = if *slot == "room" { 1 } else { 2 };
        source_text.push_str(&format!(
            "{stock_type} => Some(crate::{module_name}::EFFECT_STATE_BYTES),\n"
        ));
    }
    source_text.push_str("_ => None, } }\n");
    let mut state_sizes = selected_effects
        .iter()
        .map(|(_, _, module_name, _, _, _)| format!("crate::{module_name}::EFFECT_STATE_BYTES"));
    let maximum_state_size = state_sizes
        .next()
        .map(|first| {
            state_sizes.fold(first, |largest, next| {
                format!("if {largest} > {next} {{ {largest} }} else {{ {next} }}")
            })
        })
        .unwrap_or_else(|| "0".to_owned());
    source_text.push_str(&format!(
        "pub const MAX_EFFECT_STATE_BYTES: usize = {maximum_state_size};\n"
    ));
    source_text
        .push_str("pub unsafe fn initialize(slot: u32, state: *mut u8) -> bool { match slot {\n");
    for (slot, _, module_name, _, _, _) in &selected_effects {
        let stock_type = if *slot == "room" { 1 } else { 2 };
        source_text.push_str(&format!(
            "{stock_type} => {{ unsafe {{ crate::{module_name}::initialize_effect(state) }}; true }},\n"
        ));
    }
    source_text.push_str("_ => false, } }\n");
    source_text.push_str(
        "pub unsafe fn process_sample(slot: u32, state: *mut u8, input: f32) -> Option<(f32, f32)> { match slot {\n",
    );
    for (slot, _, module_name, _, _, _) in &selected_effects {
        let stock_type = if *slot == "room" { 1 } else { 2 };
        source_text.push_str(&format!(
            "{stock_type} => Some(unsafe {{ crate::{module_name}::process_sample(state, input) }}),\n"
        ));
    }
    source_text.push_str("_ => None, } }\n");
    source_text.push_str("pub fn tap_action(slot: u32) -> u32 { match slot {\n");
    for (slot, _, _, _, _, tap_action) in &selected_effects {
        let stock_type = if *slot == "room" { 1 } else { 2 };
        let action = match tap_action.as_str() {
            "toggle-editor" => 1,
            "tempo" => 2,
            _ => 0,
        };
        source_text.push_str(&format!("{stock_type} => {action},\n"));
    }
    source_text.push_str("_ => 0, } }\n");
    source_text.push_str("pub fn parameter_count(slot: u32) -> usize { match slot {\n");
    for (slot, _, module_name, _, controls, _) in &selected_effects {
        if controls
            .iter()
            .any(|control| control == "encoder-parameters")
        {
            let stock_type = if *slot == "room" { 1 } else { 2 };
            source_text.push_str(&format!(
                "{stock_type} => crate::{module_name}::PARAMETER_COUNT,\n"
            ));
        }
    }
    source_text.push_str("_ => 0, } }\n");
    source_text.push_str("pub unsafe fn parameter_position(slot: u32, state: *mut u8, encoder: usize) -> Option<u32> { match slot {\n");
    for (slot, _, module_name, _, controls, _) in &selected_effects {
        if controls
            .iter()
            .any(|control| control == "encoder-parameters")
        {
            let stock_type = if *slot == "room" { 1 } else { 2 };
            source_text.push_str(&format!(
                "{stock_type} => unsafe {{ crate::{module_name}::parameter_ring_position(state, encoder) }},\n"
            ));
        }
    }
    source_text.push_str("_ => None, } }\n");
    source_text.push_str("pub unsafe fn preview_parameter_position(slot: u32, state: *mut u8, encoder: usize, delta: i32) -> Option<u32> { match slot {\n");
    for (slot, _, module_name, _, controls, _) in &selected_effects {
        if controls
            .iter()
            .any(|control| control == "encoder-parameters")
        {
            let stock_type = if *slot == "room" { 1 } else { 2 };
            source_text.push_str(&format!(
                "{stock_type} => unsafe {{ crate::{module_name}::preview_parameter_ring_position(state, encoder, delta) }},\n"
            ));
        }
    }
    source_text.push_str("_ => None, } }\n");
    source_text.push_str("pub unsafe fn edit_parameter(slot: u32, state: *mut u8, encoder: usize, delta: i32) -> Option<u32> { match slot {\n");
    for (slot, _, module_name, _, controls, _) in &selected_effects {
        if controls
            .iter()
            .any(|control| control == "encoder-parameters")
        {
            let stock_type = if *slot == "room" { 1 } else { 2 };
            source_text.push_str(&format!(
                "{stock_type} => unsafe {{ crate::{module_name}::edit_parameter(state, encoder, delta) }},\n"
            ));
        }
    }
    source_text.push_str("_ => None, } }\n");
    source_text
        .push_str("pub unsafe fn show_editor_state(slot: u32, enabled: bool) { match slot {\n");
    for (slot, _, module_name, _, controls, _) in &selected_effects {
        if controls
            .iter()
            .any(|control| control == "encoder-parameters")
        {
            let stock_type = if *slot == "room" { 1 } else { 2 };
            source_text.push_str(&format!(
                "{stock_type} => unsafe {{ crate::{module_name}::show_editor_state(enabled) }},\n"
            ));
        }
    }
    source_text.push_str("_ => {}, } }\n");
    source_text.push_str("pub unsafe fn tap_tempo(slot: u32, state: *mut u8) { match slot {\n");
    for (slot, _, module_name, _, controls, _) in &selected_effects {
        if controls.iter().any(|control| control == "tap-tempo") {
            let stock_type = if *slot == "room" { 1 } else { 2 };
            source_text.push_str(&format!(
                "{stock_type} => unsafe {{ crate::{module_name}::tap_tempo(state) }},\n"
            ));
        }
    }
    source_text.push_str("_ => {}, } }\n}\n");
    let composed_root = output.join(format!("{}_composed.rs", name.replace('-', "_")));
    fs::write(&composed_root, source_text)?;
    let link_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../patches/api/composed");
    let identity = roots
        .iter()
        .map(|(patch_name, _)| patch_identity(patch_name))
        .collect::<Vec<_>>();
    let (image, symbols) = compile_inner_with_payload(
        source,
        &link_root,
        output,
        name,
        base,
        Some("sd_dynamic"),
        true,
        false,
        true,
        &["sd_composed"],
        &composed_root,
        &identity,
    )?;
    let elf = output.join(format!("{name}.payload.elf"));
    let memory_len = symbols
        .lines()
        .find(|line| line.split_whitespace().last() == Some("__module_memory_end"))
        .and_then(|line| u32::from_str_radix(line.split_whitespace().next()?, 16).ok())
        .and_then(|end| end.checked_sub(base))
        .map(|length| length as usize)
        .unwrap_or(image.len())
        .max(image.len());
    let relocations = parse_relocations(&elf, base, image.len(), memory_len)?;
    Ok((image, memory_len, symbols, relocations))
}

/// Compile the Patch loader against the separate L6 firmware bindings.
pub fn compile_l6_patch_loader(
    source: &Path,
    root: &Path,
    output: &Path,
    name: &str,
    base: u32,
    continuation_roots: &[&Path],
) -> Result<(Vec<u8>, String), Box<dyn std::error::Error>> {
    let _ = continuation_roots;
    let (image, symbols) = compile_inner(
        source,
        root,
        output,
        name,
        base,
        Some("sd_l6"),
        false,
        true,
        true,
        &[],
    )?;
    Ok((image, symbols))
}

/// Sentinel variant for deriving L6 browser loader templates.
pub fn compile_sentinel_l6_patch_loader(
    source: &Path,
    root: &Path,
    output: &Path,
    name: &str,
    base: u32,
    continuation_roots: &[&Path],
) -> Result<(Vec<u8>, String), Box<dyn std::error::Error>> {
    let _ = continuation_roots;
    compile_inner(
        source,
        root,
        output,
        name,
        base,
        Some("sd_l6"),
        false,
        true,
        false,
        &[],
    )
}

fn compile_inner(
    source: &Path,
    root: &Path,
    output: &Path,
    name: &str,
    base: u32,
    cfg: Option<&str>,
    emit_relocations: bool,
    generate_dynamic_sites: bool,
    validate_guards: bool,
    extra_cfgs: &[&str],
) -> Result<(Vec<u8>, String), Box<dyn std::error::Error>> {
    compile_inner_with_payload(
        source,
        root,
        output,
        name,
        base,
        cfg,
        emit_relocations,
        generate_dynamic_sites,
        validate_guards,
        extra_cfgs,
        &root.join("src/payload.rs"),
        &[patch_identity(name)],
    )
}

/// The catalog name and version recorded in a payload's identity record.
fn patch_identity(name: &str) -> (String, String) {
    let version = firmware_patch_core::patch_catalog()
        .iter()
        .find(|package| package.name == name)
        .map(|package| package.version.clone())
        .unwrap_or_default();
    (name.to_owned(), version)
}

/// Bytes in one `patchrt::MenuItem`.
pub const MENU_ITEM_LEN: u32 = 36;
/// Prefix of the symbol `menu_items!` exports for each patch module.
pub const MENU_ITEMS_SYMBOL_PREFIX: &str = "__l6_menu_items_";
/// Symbol of the identity record in every payload.
pub const PATCH_INFO_SYMBOL: &str = "__l6_patch_info";
/// Offset of `PatchInfo::next`, which the static builder fills.
pub const PATCH_INFO_NEXT_OFFSET: u32 = 20;

fn rust_string(text: &str) -> String {
    format!("{text:?}")
}

/// Generate `patch_identity.rs` for `patchrt::__identity`: the payload's patch
/// records and one menu group per `menu_items!` symbol found by `nm`.
fn identity_source(
    identity: &[(String, String)],
    symbols: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    let mut groups = Vec::new();
    for line in symbols.lines() {
        let fields = line.split_whitespace().collect::<Vec<_>>();
        if fields.len() != 4 || !fields[3].starts_with(MENU_ITEMS_SYMBOL_PREFIX) {
            continue;
        }
        let size = u32::from_str_radix(fields[1], 16)?;
        if size == 0 || size % MENU_ITEM_LEN != 0 {
            return Err(format!("{} has size {size}, not a menu item array", fields[3]).into());
        }
        groups.push((fields[3].to_owned(), size / MENU_ITEM_LEN));
    }
    groups.sort();
    groups.dedup();
    let mut text = String::new();
    text.push_str(&format!(
        "static __L6_PATCH_RECORDS: [PatchRecord; {}] = [\n",
        identity.len()
    ));
    for (name, version) in identity {
        text.push_str(&format!(
            "    PatchRecord {{ name: super::fixed_text({}), version: super::fixed_text({}) }},\n",
            rust_string(name),
            rust_string(version)
        ));
    }
    text.push_str("];\n");
    for (index, (symbol, _)) in groups.iter().enumerate() {
        text.push_str(&format!(
            "unsafe extern \"C\" {{\n    #[link_name = {}]\n    static __L6_MENU_GROUP_{index}: MenuItem;\n}}\n",
            rust_string(symbol)
        ));
    }
    text.push_str(&format!(
        "static __L6_MENU_GROUPS: [MenuGroup; {}] = [\n",
        groups.len()
    ));
    for (index, (_, count)) in groups.iter().enumerate() {
        text.push_str(&format!(
            "    MenuGroup {{ items: &raw const __L6_MENU_GROUP_{index}, count: {count} }},\n"
        ));
    }
    text.push_str("];\n");
    text.push_str(&format!(
        "#[used]\n#[allow(non_upper_case_globals)]\n#[unsafe(no_mangle)]\n#[unsafe(link_section = \".rodata.l6_patch_info\")]\n\
pub static {PATCH_INFO_SYMBOL}: PatchInfo = PatchInfo {{\n    magic: super::PATCH_INFO_MAGIC,\n    layout: super::PATCH_INFO_LAYOUT,\n    patch_count: {},\n    patches: __L6_PATCH_RECORDS.as_ptr(),\n    menu_group_count: {},\n    menu_groups: {},\n    next: 0,\n}};\n",
        identity.len(),
        groups.len(),
        if groups.is_empty() { "core::ptr::null()" } else { "__L6_MENU_GROUPS.as_ptr()" },
    ));
    Ok(text)
}

fn compile_inner_with_payload(
    source: &Path,
    root: &Path,
    output: &Path,
    name: &str,
    base: u32,
    cfg: Option<&str>,
    emit_relocations: bool,
    generate_dynamic_sites: bool,
    validate_guards: bool,
    extra_cfgs: &[&str],
    payload: &Path,
    identity: &[(String, String)],
) -> Result<(Vec<u8>, String), Box<dyn std::error::Error>> {
    let generated = output.join(format!("{name}.generated"));
    fs::create_dir_all(&generated)?;
    let generated = generated.canonicalize()?;
    let firmware = fs::read(source.join("main_firmware.bin"))?;
    let elf = output.join(format!("{name}.payload.elf"));
    let blob = output.join(format!("{name}.payload.bin"));
    let hook_macro = patch_hook_macro_library()?;
    fs::write(
        generated.join("continuations.rs"),
        "pub fn address(_: &str) -> usize { 0 }\n",
    )?;
    fs::write(
        generated.join("patch_identity.rs"),
        identity_source(identity, "")?,
    )?;
    let _ = compile_payload_pass(
        source,
        root,
        name,
        base,
        cfg,
        emit_relocations,
        extra_cfgs,
        &generated,
        &elf,
        &blob,
        &hook_macro,
        payload,
        true,
    )?;

    let first_symbols = run(
        Command::new(llvm_tool("L6_LLVM_NM", HOMEBREW_NM, "llvm-nm")?)
            .arg("-S")
            .arg(&elf),
    )?;
    let sites = function_hook_sites(&elf)?;
    let registrations = hook_registrations(&first_symbols)?;
    let continuation_rs = generate_continuations_from_metadata(
        &sites,
        &registrations,
        &firmware,
        emit_relocations,
        generate_dynamic_sites,
        validate_guards,
    )?;
    fs::write(generated.join("continuations.rs"), continuation_rs)?;
    fs::write(
        generated.join("patch_identity.rs"),
        identity_source(identity, &first_symbols)?,
    )?;
    let (bytes, symbols) = compile_payload_pass(
        source,
        root,
        name,
        base,
        cfg,
        emit_relocations,
        extra_cfgs,
        &generated,
        &elf,
        &blob,
        &hook_macro,
        payload,
        false,
    )?;
    if let Some(text) = diagnostic_text(&bytes) {
        return Err(format!(
            "the {name} payload contains panic diagnostics, {text:?}; payloads carry \
             only the text they display or write"
        )
        .into());
    }
    Ok((bytes, symbols))
}

/// Text that only panic diagnostics put in a payload: `core`'s messages and
/// the file name of a panic location.
const DIAGNOSTIC_TEXT: &[&str] = &[
    "<redacted>",
    "index out of bounds",
    "out of range for slice",
    "slice index starts at",
    "copy_from_slice",
    "unwrap()",
    "panicked",
    "attempt to ",
    "called `",
];

/// The first panic diagnostic in a built payload: a [`DIAGNOSTIC_TEXT`] entry
/// or a Rust source path. The hardware never reads either, so a payload with
/// one carries them in flash and module memory for nothing.
pub fn diagnostic_text(payload: &[u8]) -> Option<String> {
    if let Some(text) = DIAGNOSTIC_TEXT.iter().find(|text| {
        payload
            .windows(text.len())
            .any(|window| window == text.as_bytes())
    }) {
        return Some((*text).to_owned());
    }
    let path_byte = |byte: u8| byte.is_ascii_alphanumeric() || b"_./-".contains(&byte);
    (0..payload.len().saturating_sub(2)).find_map(|end| {
        let suffix_end = end + 3;
        if &payload[end..suffix_end] != b".rs"
            || payload
                .get(suffix_end)
                .is_some_and(|byte| byte.is_ascii_alphanumeric() || *byte == b'_')
        {
            return None;
        }
        let start = payload[..end]
            .iter()
            .rposition(|byte| !path_byte(*byte))
            .map_or(0, |index| index + 1);
        let path = &payload[start..suffix_end];
        (path.contains(&b'/') && path.len() > 5).then(|| String::from_utf8_lossy(path).into_owned())
    })
}

#[allow(clippy::too_many_arguments)]
fn compile_payload_pass(
    source: &Path,
    root: &Path,
    name: &str,
    base: u32,
    cfg: Option<&str>,
    emit_relocations: bool,
    extra_cfgs: &[&str],
    generated: &Path,
    elf: &Path,
    blob: &Path,
    hook_macro: &Path,
    payload: &Path,
    discovery: bool,
) -> Result<(Vec<u8>, String), Box<dyn std::error::Error>> {
    let firmware = fs::read(source.join("main_firmware.bin"))?;
    let dynamic_linker = root.join("linker/link-dynamic.ld");
    let static_state_linker = root.join("linker/link-static.ld");
    let linker_script = if cfg == Some("sd_dynamic") && emit_relocations && dynamic_linker.is_file()
    {
        dynamic_linker
    } else if cfg.is_none() && !emit_relocations && static_state_linker.is_file() {
        static_state_linker
    } else {
        root.join("linker/link.ld")
    };
    let firmware_is_l6 = firmware.get(4..8) == Some(&0x8000_1415u32.to_le_bytes());
    // Resolve rustc before changing into the extracted firmware directory.
    // Rustup selects toolchains from the current directory, and that directory
    // does not contain this workspace's rust-toolchain.toml.
    let rustc = rustc_executable().ok_or("could not locate the active rustc executable")?;
    let mut command = Command::new(&rustc);
    command
        .current_dir(source)
        .env("OUT_DIR", &generated)
        .arg(payload)
        .arg("--extern")
        .arg(format!("patch_hook_macros={}", hook_macro.display()))
        // Use a checkout-independent source path in compiler-generated data.
        .arg(format!(
            "--remap-path-prefix={}=patches/{name}",
            root.join("src").display(),
        ))
        .args([
            "--edition=2024",
            "--target=thumbv7em-none-eabihf",
            "-C",
            "opt-level=z",
            "-C",
        ])
        .arg(format!(
            "linker={}",
            llvm_tool("L6_LD_LLD", HOMEBREW_LLD, "rust-lld")?.display()
        ))
        .arg("-C")
        .arg(format!("link-arg=--defsym=PAYLOAD_BASE={base:#x}"))
        .arg("-C")
        .arg(format!("link-arg=-T{}", linker_script.display()))
        // Keep the identity record and every menu item array through
        // section garbage collection; nothing in the payload references them.
        .arg("-C")
        .arg(format!("link-arg=--undefined={PATCH_INFO_SYMBOL}"))
        .arg("-C")
        .arg(format!(
            "link-arg=--undefined-glob={MENU_ITEMS_SYMBOL_PREFIX}*"
        ))
        .arg("-o")
        .arg(&elf);
    if discovery {
        // The discovery pass only reports hook metadata. Hook handlers call
        // their `original_<site>_<handler>` continuation stubs directly, and
        // those stubs are generated from this pass's output, so they cannot
        // resolve yet. The output of this pass is never installed.
        command.args(["-C", "link-arg=--unresolved-symbols=ignore-all"]);
    }
    if emit_relocations {
        // SD modules are linked at base zero and moved by the loader. With
        // position-independent code, references within the module are
        // PC-relative and need no load-time fixup. Every symbol a module uses
        // is linked into it or is a fixed firmware address, so rustc may
        // address external statics directly instead of through a GOT, which
        // the loader does not support.
        command.args([
            "-C",
            "relocation-model=pie",
            "-Z",
            "direct-access-external-data=yes",
        ]);
    } else {
        command.args(["-C", "relocation-model=static"]);
    }
    // A panic traps instead of formatting a message the panic handler would
    // ignore, and panic locations carry no file, line or column. See
    // `immediate_abort_sysroot`.
    command
        .arg("--sysroot")
        .arg(immediate_abort_sysroot(&rustc)?)
        .args([
            "-Zunstable-options",
            "-Cpanic=immediate-abort",
            "-Zlocation-detail=none",
        ]);
    if emit_relocations {
        command
            .arg("--cfg")
            .arg("sd_dynamic_module")
            .arg("-C")
            .arg("link-arg=--emit-relocs");
    }
    if let Some(cfg) = cfg {
        command.args(["--cfg", cfg]);
    }
    for cfg in extra_cfgs {
        command.args(["--cfg", cfg]);
    }
    if firmware_is_l6 && cfg != Some("sd_l6") {
        command.args(["--cfg", "sd_l6"]);
    }
    run(&mut command)?;
    let symbols = run(
        Command::new(llvm_tool("L6_LLVM_NM", HOMEBREW_NM, "llvm-nm")?)
            .arg("-S")
            .arg(&elf),
    )?;
    run(Command::new(llvm_tool(
        "L6_LLVM_OBJCOPY",
        HOMEBREW_OBJCOPY,
        "llvm-objcopy",
    )?)
    .args(["--remove-section=.patch_hooks", "-O", "binary"])
    .arg(&elf)
    .arg(&blob))?;
    Ok((fs::read(blob)?, symbols))
}

fn generate_continuations_from_metadata(
    sites: &[FunctionHookSiteMetadata],
    registrations: &[HookRegistration],
    firmware: &[u8],
    dynamic: bool,
    generate_dynamic_sites: bool,
    validate_guards: bool,
) -> Result<String, Box<dyn std::error::Error>> {
    let main_base = match firmware.get(4..8) {
        Some(bytes) if bytes == 0x8000_0415u32.to_le_bytes() => 0x8000_0000u32,
        Some(bytes) if bytes == 0x8000_1415u32.to_le_bytes() => 0x8000_1000u32,
        _ => return Err("unsupported firmware reset vector for continuation generation".into()),
    };
    let mut needed = std::collections::BTreeSet::new();
    let mut call_groups =
        std::collections::BTreeMap::<String, Vec<(bool, u32, String, usize)>>::new();
    for site in sites.iter().filter(|site| site.dynamic_dispatch) {
        if dynamic || generate_dynamic_sites {
            needed.insert(site.name.as_str());
        }
    }
    for registration in registrations {
        match registration {
            HookRegistration::Entry { site, .. } => {
                needed.insert(site.as_str());
            }
            HookRegistration::CallPre {
                function,
                address,
                argument_count,
                handler,
            } => {
                needed.insert(function.as_str());
                call_groups.entry(function.clone()).or_default().push((
                    false,
                    *address,
                    handler.clone(),
                    *argument_count,
                ));
            }
            HookRegistration::CallPost {
                function,
                address,
                argument_count,
                handler,
            } => {
                needed.insert(function.as_str());
                call_groups.entry(function.clone()).or_default().push((
                    true,
                    *address,
                    handler.clone(),
                    *argument_count,
                ));
            }
            HookRegistration::Offset { .. } => {}
        }
    }

    // Continuation stubs load their slot word with a PC-relative literal
    // load (`ldr.w r12, slot`), so a relocated module needs no fixup for them.
    let mut assembly = String::from(".thumb\n");
    let mut address_arms = Vec::new();
    let mut externs = Vec::new();
    let mut wrappers = Vec::new();
    for name in needed {
        let site = sites
            .iter()
            .find(|site| site.name == name)
            .ok_or_else(|| format!("hook site {name} is missing from define_function_hooks!"))?;
        let offset = usize::try_from(
            site.address
                .checked_sub(main_base)
                .ok_or("hook before firmware")?,
        )?;
        let end = offset
            .checked_add(usize::from(site.displaced_len))
            .ok_or("hook length overflow")?;
        let displaced = firmware
            .get(offset..end)
            .ok_or_else(|| format!("continuation bytes outside firmware at {:#x}", site.address))?;
        let actual_guard = format!("{:x}", Sha256::digest(displaced));
        if validate_guards && actual_guard != site.guard_sha256 {
            return Err(format!(
                "function hook guard failed: {name} at {:#x} (base {main_base:#x}, offset {offset:#x}, bytes {:02x?}): expected {}, got {}",
                site.address, displaced, site.guard_sha256, actual_guard
            )
            .into());
        }

        let original = format!("original_{name}");
        externs.push(format!("unsafe extern \"C\" {{ fn {original}(); }}"));
        if dynamic && site.dynamic_dispatch {
            let slot = format!("dynamic_next_{name}_slot");
            assembly.push_str(&format!(
                ".section .text.continuation_{name}, \"ax\"\n.global {original}\n.thumb_func\n{original}:\n    ldr.w r12, {slot}\n    bx r12\n.align 2\n.global {slot}\n{slot}:\n    .word 0\n"
            ));
        } else if generate_dynamic_sites {
            // The resident loader's dispatch fallback must stay a direct
            // stock continuation. Patch modules use the slot-based path below
            // so statically selected modules can be chained.
            assembly.push_str(&format!(
                ".section .text.continuation_{name}, \"ax\"\n.global {original}\n.thumb_func\n{original}:\n"
            ));
            let resume = site
                .address
                .checked_add(u32::from(site.displaced_len))
                .ok_or("continuation address overflow")?
                | 1;
            match site.continuation {
                0 => {
                    emit_bytes(&mut assembly, displaced);
                    assembly.push_str(&format!("    ldr pc, ={resume:#x}\n.ltorg\n"));
                }
                1 => emit_cbz_continuation(&mut assembly, site.address, displaced, resume)?,
                2 => emit_relocated_bl_continuation(
                    &mut assembly,
                    site.address,
                    displaced,
                    firmware,
                    main_base,
                    resume,
                )?,
                3 => emit_push_cbz_continuation(&mut assembly, site.address, displaced, resume)?,
                value => return Err(format!("unsupported continuation strategy {value}").into()),
            }
        } else {
            let slot = format!("static_next_{name}_slot");
            let stock = format!("static_stock_{name}");
            assembly.push_str(&format!(
                ".section .text.continuation_{name}, \"ax\"\n.global {original}\n.thumb_func\n{original}:\n    ldr.w r12, {slot}\n    bx r12\n.align 2\n.global {slot}\n{slot}:\n    .word {stock}\n.thumb_func\n{stock}:\n"
            ));
            let resume = site
                .address
                .checked_add(u32::from(site.displaced_len))
                .ok_or("continuation address overflow")?
                | 1;
            match site.continuation {
                0 => {
                    emit_bytes(&mut assembly, displaced);
                    assembly.push_str(&format!("    ldr pc, ={resume:#x}\n.ltorg\n"));
                }
                1 => emit_cbz_continuation(&mut assembly, site.address, displaced, resume)?,
                2 => emit_relocated_bl_continuation(
                    &mut assembly,
                    site.address,
                    displaced,
                    firmware,
                    main_base,
                    resume,
                )?,
                3 => emit_push_cbz_continuation(&mut assembly, site.address, displaced, resume)?,
                value => return Err(format!("unsupported continuation strategy {value}").into()),
            }
        }
        address_arms.push(format!(
            "{} => {original} as *const () as usize,",
            format!("\"{name}\"")
        ));
    }

    for registration in registrations {
        let (site_name, handler_name) = match registration {
            HookRegistration::Entry { site, handler } => (site, handler),
            HookRegistration::CallPre {
                function, handler, ..
            }
            | HookRegistration::CallPost {
                function, handler, ..
            } => (function, handler),
            HookRegistration::Offset { .. } => continue,
        };
        let site = sites
            .iter()
            .find(|site| site.name == *site_name)
            .ok_or_else(|| {
                format!("hook site {site_name} is missing from define_function_hooks!")
            })?;
        // The hook macro calls this stub by name, so it needs no lookup.
        let original = format!("original_{site_name}_{handler_name}");
        validate_asm_symbol(&original)?;

        if dynamic && site.dynamic_dispatch {
            let slot = format!("dynamic_next_{site_name}_{handler_name}_slot");
            validate_asm_symbol(&slot)?;
            assembly.push_str(&format!(
                ".section .text.continuation_{site_name}_{handler_name}, \"ax\"\n.global {original}\n.thumb_func\n{original}:\n    ldr.w r12, {slot}\n    bx r12\n.align 2\n.global {slot}\n{slot}:\n    .word 0\n"
            ));
        } else {
            let slot = format!("static_next_{site_name}_{handler_name}_slot");
            let target = if generate_dynamic_sites {
                format!("original_{site_name}")
            } else {
                format!("static_stock_{site_name}")
            };
            validate_asm_symbol(&slot)?;
            validate_asm_symbol(&target)?;
            assembly.push_str(&format!(
                ".section .text.continuation_{site_name}_{handler_name}, \"ax\"\n.global {original}\n.thumb_func\n{original}:\n    ldr.w r12, {slot}\n    bx r12\n.align 2\n.global {slot}\n{slot}:\n    .word {target}\n"
            ));
        }
    }

    // The resident loader patches every catalog entry marked for dynamic
    // dispatch to one of these ABI-transparent tail-call stubs. The handler
    // table lives in the loader's heap-allocated registry, which the stub finds
    // through the registry pointer word; without a registry it takes the stock
    // path. Each module handler is a normal typed Rust hook and owns its
    // continuation slot; the stub only selects the current chain head or stock
    // continuation, and restores r0 and r1 before branching.
    if generate_dynamic_sites && !dynamic {
        use patch_loader_core::registry;
        let pointer = registry::pointer_for_main_base(main_base);
        let mut dynamic_index = 0usize;
        for site in sites.iter().filter(|site| site.dynamic_dispatch) {
            let dispatcher = format!("dispatch_{}", site.name);
            let original = format!("original_{}", site.name);
            validate_asm_symbol(&dispatcher)?;
            // The handler is the first word of the site's registry record.
            let index = u32::try_from(dynamic_index)?;
            if index >= registry::MAX_SITES {
                return Err(format!(
                    "{} dynamic dispatch sites exceed the registry's {} sites",
                    index + 1,
                    registry::MAX_SITES
                )
                .into());
            }
            let slot_offset = registry::site_offset(index);
            assembly.push_str(&format!(
                ".section .text.{dispatcher}, \"ax\"\n.global {dispatcher}\n.thumb_func\n{dispatcher}:\n    push {{r0, r1}}\n    ldr r0, ={pointer:#x}\n    ldr r0, [r0]\n    cbz r0, 1f\n    ldr r1, [r0]\n    ldr r12, ={magic:#x}\n    cmp r1, r12\n    bne 1f\n    ldr.w r12, [r0, #{slot_offset}]\n    cmp r12, #0\n    bne 2f\n1:  ldr r12, ={original}\n2:  pop {{r0, r1}}\n    bx r12\n.ltorg\n",
                magic = registry::MAGIC,
            ));
            wrappers.push(format!(
                "unsafe extern \"C\" {{ fn {dispatcher}(); }}\n#[used]\nstatic __L6_KEEP_DYNAMIC_DISPATCH_{}: unsafe extern \"C\" fn() = {dispatcher};",
                site.name
            ));
            dynamic_index += 1;
        }
    }

    let mut offset_wrappers = Vec::new();
    for registration in registrations {
        let HookRegistration::Offset {
            function,
            offset: instruction_offset,
            displaced_len,
            guard_sha256,
            handler,
        } = registration
        else {
            continue;
        };
        let site = sites
            .iter()
            .find(|site| site.name == function.as_str())
            .ok_or_else(|| {
                format!("offset hook function {function} is missing from define_function_hooks!")
            })?;
        let address = site
            .address
            .checked_add(*instruction_offset)
            .ok_or("offset hook address overflow")?;
        let start = usize::try_from(
            address
                .checked_sub(main_base)
                .ok_or("offset hook before firmware")?,
        )?;
        let len = usize::from(*displaced_len);
        let displaced = firmware
            .get(
                start
                    ..start
                        .checked_add(len)
                        .ok_or("offset hook length overflow")?,
            )
            .ok_or_else(|| format!("offset hook bytes outside firmware at {address:#x}"))?;
        let actual_guard = format!("{:x}", Sha256::digest(displaced));
        if validate_guards && actual_guard != *guard_sha256 {
            return Err(format!("offset hook guard failed: {function} at {address:#x}").into());
        }
        let veneer = format!("__l6_offset_veneer_{handler}");
        validate_asm_symbol(handler)?;
        validate_asm_symbol(&veneer)?;
        assembly.push_str(&format!(
            ".section .text.generated_hook_{handler}, \"ax\"\n.global {veneer}\n.thumb_func\n{veneer}:\n    push {{r0-r3, r4-r12, lr}}\n    bl {handler}\n    cmp r0, #1\n    beq 1f\n    pop {{r0-r3, r4-r12, lr}}\n"
        ));
        emit_bytes(&mut assembly, displaced);
        let resume = address
            .checked_add(u32::try_from(len)?)
            .ok_or("offset hook continuation address overflow")?
            | 1;
        assembly.push_str(&format!(
            "    ldr pc, ={resume:#x}\n1:\n    pop {{r0-r3, r4-r12, lr}}\n    ldr pc, ={resume:#x}\n.ltorg\n"
        ));
        offset_wrappers.push(format!(
            "unsafe extern \"C\" {{ fn {veneer}(); }}\n#[used]\nstatic __L6_KEEP_OFFSET_{handler}: unsafe extern \"C\" fn() = {veneer};"
        ));
    }

    if !dynamic {
        for (function, handlers) in call_groups {
            let site = sites
                .iter()
                .find(|site| site.name == function)
                .ok_or_else(|| {
                    format!("call hook function {function} is missing from define_function_hooks!")
                })?;
            for (_, call_address, _, _) in &handlers {
                let actual = decode_thumb_bl_target(firmware, main_base, *call_address)?;
                if actual != site.address {
                    return Err(format!(
                        "call hook at {call_address:#x} targets {actual:#x}, expected {} at {:#x}",
                        function, site.address
                    )
                    .into());
                }
            }
            let argument_count = handlers[0].3;
            if handlers.iter().any(|handler| handler.3 != argument_count) {
                return Err(format!(
                    "call hook handlers for {function} disagree on argument count"
                )
                .into());
            }
            let wrapper = format!("__l6_call_dispatch_{function}");
            let original = format!("original_{function}");
            let matched_label = format!(".Lcall_hook_match_{function}");
            validate_asm_symbol(&wrapper)?;
            assembly.push_str(&format!(
                ".section .text.call_dispatch_{function}, \"ax\"\n.global {wrapper}\n.thumb_func\n{wrapper}:\n"
            ));
            let mut call_returns = std::collections::BTreeSet::new();
            for (_, call_address, _, _) in &handlers {
                call_returns.insert(
                    call_address
                        .checked_add(4)
                        .ok_or("call return address overflow")?
                        | 1,
                );
            }
            for caller_return in call_returns {
                assembly.push_str(&format!(
                    "    ldr r12, ={caller_return:#x}\n    cmp lr, r12\n    beq {matched_label}\n"
                ));
            }
            assembly.push_str(&format!(
                "    ldr pc, ={original}\n{matched_label}:\n    push {{r0-r4, lr}}\n    sub sp, #8\n    mov r4, sp\n"
            ));
            for (_, call_address, handler, _) in handlers.iter().filter(|(post, _, _, _)| !post) {
                let caller_return = call_address
                    .checked_add(4)
                    .ok_or("call return address overflow")?
                    | 1;
                emit_call_hook_dispatch(&mut assembly, caller_return, handler, argument_count)?;
            }
            emit_call_arguments(&mut assembly, argument_count)?;
            assembly.push_str(&format!(
                "    bl {original}\n    mov sp, r4\n    str r0, [r4, #0]\n    str r1, [r4, #4]\n"
            ));
            // Call wrappers unwind after the callee. Reverse the direct static
            // calls so their observable order matches filename-ordered dynamic
            // wrapper chains.
            for (_, call_address, handler, _) in
                handlers.iter().rev().filter(|(post, _, _, _)| *post)
            {
                let caller_return = call_address
                    .checked_add(4)
                    .ok_or("call return address overflow")?
                    | 1;
                emit_call_hook_dispatch(&mut assembly, caller_return, handler, argument_count)?;
            }
            assembly.push_str(
                "    mov sp, r4\n    add sp, #8\n    pop {r0-r4, lr}\n    sub r12, sp, #32\n    ldr r0, [r12, #0]\n    ldr r1, [r12, #4]\n    bx lr\n.ltorg\n",
            );
            wrappers.push(format!(
                "unsafe extern \"C\" {{ fn {wrapper}(); }}\n#[used]\nstatic __L6_KEEP_CALL_DISPATCH_{function}: unsafe extern \"C\" fn() = {wrapper};"
            ));
        }
    }

    if !assembly.trim().eq(".thumb") {
        Ok(format!(
            "use core::arch::global_asm;\nglobal_asm!(r####\"{assembly}\"####, options(raw));\n{}\npub fn address(name: &str) -> usize {{ match name {{ {} _ => 0 }} }}\n{}\n{}\n",
            externs.join("\n"),
            address_arms.join(" "),
            offset_wrappers.join("\n"),
            wrappers.join("\n"),
        ))
    } else {
        Ok("pub fn address(_: &str) -> usize { 0 }\n".to_owned())
    }
}

pub fn decode_thumb_bl_target(
    firmware: &[u8],
    firmware_base: u32,
    address: u32,
) -> Result<u32, Box<dyn std::error::Error>> {
    let offset = usize::try_from(
        address
            .checked_sub(firmware_base)
            .ok_or("call site before firmware")?,
    )?;
    let bytes = firmware
        .get(
            offset
                ..offset
                    .checked_add(4)
                    .ok_or("call instruction offset overflow")?,
        )
        .ok_or_else(|| format!("call instruction outside firmware at {address:#x}"))?;
    let first = u16::from_le_bytes(bytes[..2].try_into()?);
    let second = u16::from_le_bytes(bytes[2..].try_into()?);
    if first & 0xf800 != 0xf000 || second & 0xd000 != 0xd000 {
        return Err(format!("expected Thumb BL at {address:#x}").into());
    }
    let sign = ((first >> 10) & 1) as u32;
    let j1 = ((second >> 13) & 1) as u32;
    let j2 = ((second >> 11) & 1) as u32;
    let i1 = (!(j1 ^ sign)) & 1;
    let i2 = (!(j2 ^ sign)) & 1;
    let encoded = (sign << 24)
        | (i1 << 23)
        | (i2 << 22)
        | (((first & 0x03ff) as u32) << 12)
        | (((second & 0x07ff) as u32) << 1);
    let signed_offset = ((encoded << 7) as i32) >> 7;
    Ok(address.wrapping_add(4).wrapping_add(signed_offset as u32) & !1)
}

fn emit_call_hook_dispatch(
    assembly: &mut String,
    caller_return: u32,
    handler: &str,
    argument_count: usize,
) -> Result<(), Box<dyn std::error::Error>> {
    validate_asm_symbol(handler)?;
    assembly.push_str(&format!(
        "    ldr r0, [r4, #28]\n    ldr r12, ={caller_return:#x}\n    cmp r0, r12\n    bne 1f\n"
    ));
    emit_call_arguments(assembly, argument_count)?;
    assembly.push_str(&format!("    bl {handler}\n    mov sp, r4\n1:\n"));
    Ok(())
}

fn emit_call_arguments(
    assembly: &mut String,
    argument_count: usize,
) -> Result<(), Box<dyn std::error::Error>> {
    assembly.push_str(
        "    ldr r0, [r4, #8]\n    ldr r1, [r4, #12]\n    ldr r2, [r4, #16]\n    ldr r3, [r4, #20]\n",
    );
    let stack_arguments = argument_count.saturating_sub(4);
    let outgoing_bytes = stack_arguments
        .checked_mul(4)
        .ok_or("too many stack arguments")?
        .checked_add(7)
        .ok_or("stack argument area overflow")?
        & !7;
    if outgoing_bytes == 0 {
        assembly.push_str("    mov sp, r4\n");
    } else {
        if outgoing_bytes > 508 {
            return Err("call hook has too many stack arguments".into());
        }
        assembly.push_str(&format!("    mov sp, r4\n    sub sp, #{outgoing_bytes}\n"));
        for index in 0..stack_arguments {
            let source_offset = 32usize
                .checked_add(
                    index
                        .checked_mul(4)
                        .ok_or("stack argument offset overflow")?,
                )
                .ok_or("stack argument offset overflow")?;
            let target_offset = index * 4;
            assembly.push_str(&format!(
                "    ldr r12, [r4, #{source_offset}]\n    str r12, [sp, #{target_offset}]\n"
            ));
        }
    }
    Ok(())
}

fn patch_hook_macro_library() -> Result<PathBuf, Box<dyn std::error::Error>> {
    let executable = std::env::current_exe()?;
    let executable_dir = executable
        .parent()
        .ok_or("compiler executable has no parent directory")?;
    let profile_dir = if let Some(directory) = std::env::var_os("L6_PATCH_ARTIFACT_DIR") {
        PathBuf::from(directory)
    } else if executable_dir
        .file_name()
        .is_some_and(|name| name == "deps")
    {
        executable_dir
            .parent()
            .ok_or("dependency executable has no profile directory")?
            .to_path_buf()
    } else {
        executable_dir.to_path_buf()
    };
    let expected_extension = std::env::consts::DLL_SUFFIX.trim_start_matches('.');
    let mut pending = vec![profile_dir.join("deps"), profile_dir.join("build")];
    let mut candidates = Vec::new();
    while let Some(directory) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                pending.push(path);
            } else if path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| {
                    (name.starts_with("libpatch_hook_macros-")
                        || name.starts_with("patch_hook_macros-"))
                        && path
                            .extension()
                            .is_some_and(|extension| extension == expected_extension)
                })
            {
                candidates.push(path);
            }
        }
    }
    candidates.sort_by_key(|path| {
        std::fs::metadata(path)
            .and_then(|metadata| metadata.modified())
            .ok()
    });
    candidates.pop().ok_or_else(|| {
        format!(
            "patch-hook-macros artifact not found under {}",
            profile_dir.display()
        )
        .into()
    })
}

fn validate_asm_symbol(symbol: &str) -> Result<(), Box<dyn std::error::Error>> {
    if symbol.is_empty()
        || !symbol
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_')
        || symbol.as_bytes().first().is_some_and(u8::is_ascii_digit)
    {
        return Err(format!("invalid generated assembly symbol {symbol:?}").into());
    }
    Ok(())
}

fn emit_bytes(assembly: &mut String, bytes: &[u8]) {
    for chunk in bytes.chunks(8) {
        assembly.push_str("    .byte ");
        for (index, byte) in chunk.iter().enumerate() {
            if index != 0 {
                assembly.push_str(", ");
            }
            assembly.push_str(&format!("0x{byte:02x}"));
        }
        assembly.push('\n');
    }
}

/// Emit a guarded continuation that relocates Thumb BL instructions while
/// copying other instructions verbatim. This strategy is explicit in the
/// function catalog and does not claim to relocate arbitrary PC-relative
/// Thumb instructions.
fn emit_relocated_bl_continuation(
    assembly: &mut String,
    address: u32,
    displaced: &[u8],
    firmware: &[u8],
    firmware_base: u32,
    resume: u32,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut offset = 0usize;
    let mut copied_start = 0usize;
    let mut calls = 0usize;
    while offset + 4 <= displaced.len() {
        let first = u16::from_le_bytes(displaced[offset..offset + 2].try_into()?);
        let second = u16::from_le_bytes(displaced[offset + 2..offset + 4].try_into()?);
        if first & 0xf800 == 0xf000 && second & 0xd000 == 0xd000 {
            emit_bytes(assembly, &displaced[copied_start..offset]);
            let target =
                decode_thumb_bl_target(firmware, firmware_base, address + u32::try_from(offset)?)?
                    | 1;
            assembly.push_str(&format!("    ldr r12, ={target:#x}\n    blx r12\n"));
            offset += 4;
            copied_start = offset;
            calls += 1;
        } else {
            offset += 2;
        }
    }
    if calls == 0 {
        return Err(
            format!("RelocatedBl continuation at {address:#x} contains no Thumb BL").into(),
        );
    }
    emit_bytes(assembly, &displaced[copied_start..]);
    assembly.push_str(&format!("    ldr pc, ={resume:#x}\n.ltorg\n"));
    Ok(())
}

fn emit_cbz_continuation(
    assembly: &mut String,
    address: u32,
    displaced: &[u8],
    resume: u32,
) -> Result<(), Box<dyn std::error::Error>> {
    if displaced.len() < 2 || displaced.len() % 2 != 0 {
        return Err("CBZ continuation requires an even displaced instruction span".into());
    }
    let instruction = u16::from_le_bytes(displaced[..2].try_into()?);
    if instruction & 0xf500 != 0xb100 {
        return Err(format!("expected CBZ/CBNZ at {address:#x}").into());
    }
    let branch_offset =
        (((instruction & 0x0200) as u32) >> 3) | (((instruction & 0x00f8) as u32) >> 2);
    let target = address
        .checked_add(4)
        .and_then(|pc| pc.checked_add(branch_offset))
        .ok_or("CBZ target overflow")?;
    if (address..address + displaced.len() as u32).contains(&target) {
        return Err(format!("CBZ target lands inside displaced bytes at {target:#x}").into());
    }
    // CBZ's architectural PC is instruction address + 4. The local label
    // follows the replayed span and the 32-bit literal load that resumes it.
    let local_offset = u32::try_from(displaced.len())?;
    if local_offset > 0x7e {
        return Err("displaced CBZ span is too long for generated local branch".into());
    }
    let relocated = (instruction & !0x02f8)
        | (((local_offset & 0x40) << 3) as u16)
        | (((local_offset & 0x3e) << 2) as u16);
    assembly.push_str(&format!("    .hword 0x{relocated:04x}\n"));
    emit_bytes(assembly, &displaced[2..]);
    assembly.push_str(&format!(
        "    ldr pc, ={resume:#x}\n1: ldr pc, ={:#x}\n.ltorg\n",
        target | 1
    ));
    Ok(())
}

/// Relocate a CBZ/CBNZ that follows an initial 16-bit PUSH. This is used by
/// firmware functions whose four-byte hook window starts with a register-save
/// prologue followed by a short PC-relative branch.
fn emit_push_cbz_continuation(
    assembly: &mut String,
    address: u32,
    displaced: &[u8],
    resume: u32,
) -> Result<(), Box<dyn std::error::Error>> {
    if displaced.len() < 4 || displaced.len() % 2 != 0 {
        return Err("PUSH+CBZ continuation requires at least four even bytes".into());
    }
    let push = u16::from_le_bytes(displaced[..2].try_into()?);
    if push & 0xfe00 != 0xb400 {
        return Err(format!("expected Thumb PUSH at {address:#x}").into());
    }
    let branch_offset = 2usize;
    let instruction = u16::from_le_bytes(displaced[branch_offset..branch_offset + 2].try_into()?);
    if instruction & 0xf500 != 0xb100 {
        return Err(format!("expected CBZ/CBNZ at {:#x}", address + 2).into());
    }
    let encoded_offset =
        (((instruction & 0x0200) as u32) >> 3) | (((instruction & 0x00f8) as u32) >> 2);
    let target = address
        .checked_add(branch_offset as u32 + 4)
        .and_then(|pc| pc.checked_add(encoded_offset))
        .ok_or("CBZ target overflow")?;
    if (address..address + displaced.len() as u32).contains(&target) {
        return Err(format!("CBZ target lands inside displaced bytes at {target:#x}").into());
    }
    // The local label follows the relocated span and the resume literal load.
    // CBZ's PC is the relocated instruction address plus four bytes.
    let local_offset = displaced.len() - branch_offset;
    if local_offset > 0x7e {
        return Err("displaced PUSH+CBZ span is too long for generated local branch".into());
    }
    let local_offset = local_offset as u32;
    let relocated = (instruction & !0x02f8)
        | (((local_offset & 0x40) << 3) as u16)
        | (((local_offset & 0x3e) << 2) as u16);
    emit_bytes(assembly, &displaced[..branch_offset]);
    assembly.push_str(&format!("    .hword 0x{relocated:04x}\n"));
    emit_bytes(assembly, &displaced[branch_offset + 2..]);
    assembly.push_str(&format!(
        "    ldr pc, ={resume:#x}\n1: ldr pc, ={:#x}\n.ltorg\n",
        target | 1
    ));
    Ok(())
}

fn parse_relocations(
    elf: &Path,
    base: u32,
    code_len: usize,
    memory_len: usize,
) -> Result<Vec<Relocation>, Box<dyn std::error::Error>> {
    let output = run(Command::new(llvm_tool(
        "L6_LLVM_READELF",
        HOMEBREW_READELF,
        "llvm-readelf",
    )?)
    .args(["-r"])
    .arg(elf))?;
    let end = base
        .checked_add(u32::try_from(memory_len).map_err(|_| "module too large")?)
        .ok_or("payload address overflow")?;
    let mut in_loaded = false;
    let mut records = Vec::new();
    let mut movw = Vec::<(u32, u32, String)>::new();
    let mut movt = Vec::<(u32, u32, String)>::new();

    for line in output.lines() {
        if let Some(rest) = line.strip_prefix("Relocation section '") {
            let section = rest.split('\'').next().unwrap_or_default();
            in_loaded = match section {
                // Code, read-only data and writable data are all part of the
                // loaded image, so their absolute references move with it.
                ".rel.text" | ".rela.text" | ".rel.data" | ".rela.data" => true,
                // Hook metadata is stripped from the image, and debug and
                // attribute sections are never loaded.
                ".rel.patch_hooks" | ".rela.patch_hooks" => false,
                name if name.starts_with(".rel.debug_")
                    || name.starts_with(".rela.debug_")
                    || name.starts_with(".rel.ARM.")
                    || name.starts_with(".rela.ARM.") =>
                {
                    false
                }
                // A relocation section this function does not know could
                // hold pointers that the loader must fix. Refuse the module
                // instead of producing one with broken pointers.
                name => return Err(format!("unsupported relocation section {name}").into()),
            };
            continue;
        }
        if !in_loaded {
            continue;
        }
        let columns = line.split_whitespace().collect::<Vec<_>>();
        if columns.len() < 5 || !columns[2].starts_with("R_ARM_") {
            continue;
        }
        let offset = u32::from_str_radix(columns[0], 16)?;
        let kind = columns[2];
        let symbol_value = u32::from_str_radix(columns[3], 16)?;
        if !(base..end).contains(&symbol_value) {
            continue;
        }
        let relative = offset
            .checked_sub(base)
            .ok_or("relocation before payload")?;
        let symbol = columns[4..].join(" ");
        match kind {
            "R_ARM_ABS32" => {
                if (relative as usize)
                    .checked_add(4)
                    .is_none_or(|x| x > code_len)
                {
                    return Err("ABS32 relocation outside payload".into());
                }
                records.push(Relocation::Abs32 { offset: relative });
            }
            "R_ARM_THM_MOVW_ABS_NC" => movw.push((relative, symbol_value, symbol)),
            "R_ARM_THM_MOVT_ABS" => movt.push((relative, symbol_value, symbol)),
            // These relocations encode PC-relative displacements, which do not
            // change when the whole module is moved by one load delta.
            "R_ARM_REL32" | "R_ARM_THM_CALL" | "R_ARM_THM_JUMP24" | "R_ARM_THM_JUMP19"
            | "R_ARM_THM_PC8" | "R_ARM_THM_PC12" | "R_ARM_THM_PC22" => {}
            _ => return Err(format!("unsupported internal relocation {kind}").into()),
        }
    }

    for (low_offset, low_value, low_symbol) in movw {
        let Some((index, (high_offset, _, _))) = movt
            .iter()
            .enumerate()
            .filter(|(_, (_, value, symbol))| *value == low_value && *symbol == low_symbol)
            .min_by_key(|(_, (offset, _, _))| offset.abs_diff(low_offset))
        else {
            return Err(format!("unpaired MOVW relocation for {low_symbol}").into());
        };
        if high_offset.abs_diff(low_offset) > 16 {
            return Err(format!("distant MOVW/MOVT relocations for {low_symbol}").into());
        }
        if (*high_offset as usize)
            .checked_add(4)
            .is_none_or(|x| x > code_len)
        {
            return Err("MOVT relocation outside payload".into());
        }
        records.push(Relocation::ThumbMov32 {
            low_offset,
            high_offset: *high_offset,
        });
        movt.remove(index);
    }
    if !movt.is_empty() {
        return Err("unpaired MOVT relocation".into());
    }
    records.sort_by_key(|record| match record {
        Relocation::Abs32 { offset } => *offset,
        Relocation::ThumbMov32 { low_offset, .. } => *low_offset,
    });
    Ok(records)
}

/// Looks up a symbol's address in `compile()`'s `llvm-nm` output, clearing the
/// Thumb bit (bit 0, set on every Thumb function symbol).
pub fn symbol(symbols: &str, name: &str) -> Result<u32, Box<dyn std::error::Error>> {
    let line = symbols
        .lines()
        .find(|line| line.split_whitespace().last() == Some(name))
        .ok_or_else(|| format!("missing patch entry: {name}"))?;
    Ok(u32::from_str_radix(line.split_whitespace().next().unwrap(), 16)? & !1)
}

/// Read catalog records emitted in the ELF's non-runtime `.patch_hooks`
/// section. The section is removed from the flat payload binary.
pub fn function_hook_sites(
    elf: &Path,
) -> Result<Vec<FunctionHookSiteMetadata>, Box<dyn std::error::Error>> {
    let readelf = llvm_tool("L6_LLVM_READELF", HOMEBREW_READELF, "llvm-readelf")?;
    let sections = run(Command::new(readelf).args(["-S"]).arg(elf))?;
    if !sections.contains(".patch_hooks") {
        return Ok(Vec::new());
    }

    let objcopy = llvm_tool("L6_LLVM_OBJCOPY", HOMEBREW_OBJCOPY, "llvm-objcopy")?;
    let metadata_path = elf.with_extension("patch-hooks.tmp");
    let section_arg = format!("--dump-section=.patch_hooks={}", metadata_path.display());
    run(Command::new(objcopy).arg(section_arg).arg(elf))?;
    let bytes = fs::read(&metadata_path)?;
    fs::remove_file(metadata_path)?;

    const RECORD_LEN: usize = 124;
    const MAGIC: u32 = 0x484f_4f4b;
    let mut records = Vec::new();
    let mut offset = 0;
    while offset + RECORD_LEN <= bytes.len() {
        if u32::from_le_bytes(bytes[offset..offset + 4].try_into()?) != MAGIC {
            offset += 4;
            continue;
        }
        let address = u32::from_le_bytes(bytes[offset + 4..offset + 8].try_into()?);
        let displaced_len = u16::from_le_bytes(bytes[offset + 8..offset + 10].try_into()?);
        let continuation = bytes[offset + 10];
        let dynamic_dispatch = bytes[offset + 11];
        let name_bytes = &bytes[offset + 12..offset + 60];
        let guard_bytes = &bytes[offset + 60..offset + 124];
        let name_end = name_bytes.iter().position(|byte| *byte == 0).unwrap_or(48);
        let name = std::str::from_utf8(&name_bytes[..name_end])?.to_owned();
        let guard_sha256 = std::str::from_utf8(guard_bytes)?.to_owned();
        if address & 1 != 0
            || displaced_len < 2
            || !matches!(continuation, 0 | 1 | 2 | 3)
            || !matches!(dynamic_dispatch, 0 | 1)
            || name.is_empty()
            || guard_sha256.len() != 64
            || !guard_sha256.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            return Err(
                format!("invalid function hook metadata at section offset {offset:#x}").into(),
            );
        }
        records.push(FunctionHookSiteMetadata {
            address,
            displaced_len,
            continuation,
            dynamic_dispatch: dynamic_dispatch != 0,
            name,
            guard_sha256,
        });
        offset += RECORD_LEN;
    }
    Ok(records)
}

/// Decode hook-attribute registration symbols from `llvm-nm` output.
pub fn hook_registrations(
    symbols: &str,
) -> Result<Vec<HookRegistration>, Box<dyn std::error::Error>> {
    let mut registrations = Vec::new();
    for symbol in symbols
        .lines()
        .filter_map(|line| line.split_whitespace().last())
    {
        let Some(metadata) = symbol.strip_prefix("__l6_patch_hook_") else {
            continue;
        };
        let fields = metadata.split("__").collect::<Vec<_>>();
        let invalid = || format!("invalid hook metadata symbol {symbol}");
        match fields.as_slice() {
            ["entry", site, handler] if !site.is_empty() && !handler.is_empty() => {
                registrations.push(HookRegistration::Entry {
                    site: (*site).to_owned(),
                    handler: (*handler).to_owned(),
                });
            }
            [
                "offset",
                function,
                offset,
                displaced_len,
                guard_sha256,
                handler,
            ] if !function.is_empty() && !handler.is_empty() => {
                let displaced_len = displaced_len.parse::<u16>().map_err(|_| invalid())?;
                if displaced_len < 4
                    || displaced_len & 1 != 0
                    || guard_sha256.len() != 64
                    || !guard_sha256.bytes().all(|byte| byte.is_ascii_hexdigit())
                {
                    return Err(invalid().into());
                }
                registrations.push(HookRegistration::Offset {
                    function: (*function).to_owned(),
                    offset: u32::from_str_radix(offset, 16).map_err(|_| invalid())?,
                    displaced_len,
                    guard_sha256: (*guard_sha256).to_owned(),
                    handler: (*handler).to_owned(),
                });
            }
            ["pre", function, address, argument_count, handler]
            | ["post", function, address, argument_count, handler]
                if !function.is_empty() && !handler.is_empty() =>
            {
                let address = u32::from_str_radix(address, 16).map_err(|_| invalid())?;
                let argument_count = argument_count.parse::<usize>().map_err(|_| invalid())?;
                let registration = if fields[0] == "pre" {
                    HookRegistration::CallPre {
                        function: (*function).to_owned(),
                        address,
                        argument_count,
                        handler: (*handler).to_owned(),
                    }
                } else {
                    HookRegistration::CallPost {
                        function: (*function).to_owned(),
                        address,
                        argument_count,
                        handler: (*handler).to_owned(),
                    }
                };
                registrations.push(registration);
            }
            _ => return Err(invalid().into()),
        }
    }
    Ok(registrations)
}

/// Resolve static hook sites from emitted attributes and the shared catalog.
/// Call-site hooks produce a function-entry dispatcher window; offset hooks
/// produce a veneer at the declared instruction offset.
pub fn hook_windows(
    sites: &[FunctionHookSiteMetadata],
    registrations: &[HookRegistration],
) -> Result<Vec<HookWindow>, Box<dyn std::error::Error>> {
    let mut windows = Vec::new();
    let mut call_dispatches = std::collections::BTreeSet::new();
    for registration in registrations {
        match registration {
            HookRegistration::Entry { site, handler } => {
                let site = sites
                    .iter()
                    .find(|candidate| candidate.name == *site)
                    .ok_or_else(|| {
                        format!("entry hook {site} is absent from define_function_hooks!")
                    })?;
                windows.push(HookWindow {
                    address: site.address,
                    target_symbol: handler.clone(),
                    name: handler.clone(),
                    length: usize::from(site.displaced_len),
                    continuation: site.continuation,
                });
            }
            HookRegistration::Offset {
                function,
                offset,
                displaced_len,
                handler,
                ..
            } => {
                let site = sites
                    .iter()
                    .find(|candidate| candidate.name == *function)
                    .ok_or_else(|| {
                        format!("offset hook {function} is absent from define_function_hooks!")
                    })?;
                windows.push(HookWindow {
                    address: site
                        .address
                        .checked_add(*offset)
                        .ok_or("offset hook address overflow")?,
                    target_symbol: format!("__l6_offset_veneer_{handler}"),
                    name: handler.clone(),
                    length: usize::from(*displaced_len),
                    continuation: 0,
                });
            }
            HookRegistration::CallPre { function, .. }
            | HookRegistration::CallPost { function, .. } => {
                if !call_dispatches.insert(function.as_str()) {
                    continue;
                }
                let site = sites
                    .iter()
                    .find(|candidate| candidate.name == *function)
                    .ok_or_else(|| {
                        format!(
                            "call hook function {function} is absent from define_function_hooks!"
                        )
                    })?;
                windows.push(HookWindow {
                    address: site.address,
                    target_symbol: format!("__l6_call_dispatch_{function}"),
                    name: format!("call_dispatch:{function}"),
                    length: usize::from(site.displaced_len),
                    continuation: site.continuation,
                });
            }
        }
    }
    Ok(windows)
}

#[cfg(test)]
mod tests {
    use super::*;

    const NM: &str = "\
00001200 00000024 T __l6_menu_items_composed::midi_sync
00001224 00000048 T __l6_menu_items_composed::knob_dialog
00001300 00000018 T __l6_patch_info
00001400 00000010 T unrelated_symbol
";

    #[test]
    fn identity_lists_patches_and_menu_groups() {
        let identity = [
            ("knob-dialog".to_owned(), "1.0.0".to_owned()),
            ("midi-sync".to_owned(), "1.0.0".to_owned()),
        ];
        let source = identity_source(&identity, NM).unwrap();
        assert!(source.contains("[PatchRecord; 2]"));
        assert!(source.contains("super::fixed_text(\"midi-sync\")"));
        assert!(source.contains("#[link_name = \"__l6_menu_items_composed::knob_dialog\"]"));
        // 0x48 bytes are two 36-byte items; groups are sorted by symbol.
        assert!(source.contains("items: &raw const __L6_MENU_GROUP_0, count: 2"));
        assert!(source.contains("items: &raw const __L6_MENU_GROUP_1, count: 1"));
        assert!(source.contains("menu_group_count: 2"));
        assert!(source.contains("menu_groups: __L6_MENU_GROUPS.as_ptr()"));
    }

    #[test]
    fn identity_without_menu_items_has_no_groups() {
        let source =
            identity_source(&[("menu-loader".to_owned(), "1.0.0".to_owned())], "").unwrap();
        assert!(source.contains("menu_group_count: 0"));
        assert!(source.contains("menu_groups: core::ptr::null()"));
        assert!(source.contains(&format!("pub static {PATCH_INFO_SYMBOL}: PatchInfo")));
    }

    #[test]
    fn identity_rejects_a_partial_menu_item_array() {
        let nm = "00001200 00000020 T __l6_menu_items_patch\n";
        assert!(identity_source(&[], nm).is_err());
    }

    #[test]
    fn identity_versions_come_from_the_catalog() {
        assert_eq!(patch_identity("menu-loader").1, "1.0.0");
        assert_eq!(patch_identity("not-a-patch").1, "");
    }

    #[test]
    fn diagnostic_text_finds_panic_messages_and_source_paths() {
        assert_eq!(
            diagnostic_text(b"\x01\x02PLATEAU PREDELAY\x00dump-rom"),
            None
        );
        assert_eq!(
            diagnostic_text(b"\x00\x00<redacted>\x00"),
            Some("<redacted>".into())
        );
        assert_eq!(
            diagnostic_text(b"x index out of bounds: the len is "),
            Some("index out of bounds".into())
        );
        assert_eq!(
            diagnostic_text(b"\xffpatches/knob-dialog/src/values.rs\x1c"),
            Some("patches/knob-dialog/src/values.rs".into())
        );
        // An ".rs" without a path is not a source location.
        assert_eq!(diagnostic_text(b"\x00a.rs\x00errs\x00"), None);
    }
}
