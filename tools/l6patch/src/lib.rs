//! The patch tool: builds patched firmware and SD modules from stock
//! firmware, lists the patch catalog, and regenerates the prebuilt files.
//! Never edits its input.
pub mod prebuilt;
use firmware_patch_compile::{
    FunctionHookSiteMetadata, HookRegistration, compile, compile_l6_patch_loader, compile_sd,
    compile_sd_relocatable_composed, compile_with_continuations, decode_thumb_bl_target,
    function_hook_sites, hook_registrations, symbol,
};
use patch_core::{HookEdit, PatchEdits, apply_patch_with_map, branch, verify_firmware_for};
use sha2::{Digest, Sha256};
use std::{
    fs,
    path::{Path, PathBuf},
};
const USAGE: &str = "\
Usage:
  l6patch firmware STOCK_DIR OUTPUT_DIR [--patch NAME]... [--version DIGITS] [--loader]
  l6patch module STOCK_DIR OUTPUT_DIR [--patch NAME]... [--effect SLOT=ID]...
  l6patch catalog [--model l6|l6max] [--format text|json]
  l6patch prebuilt [--config PATH | --l6max DIR --l6 DIR] [--check]

STOCK_DIR is an unpacked stock update package (`l6fw unpack`). Patch names come
from patches/<name>/package.toml; dependencies are included automatically.
`firmware` builds an update package with the patches applied, and with --loader
also installs the experimental Patch loader. `module` builds one composed SD
module for PATCHES/ on the card. `prebuilt` regenerates the committed browser
templates and SD modules from the config.toml [firmware] packages; --check
only compares them with the current sources. `l6patch STOCK_DIR OUTPUT_DIR`
is `l6patch firmware`.";

struct Options {
    source: PathBuf,
    output: PathBuf,
    /// The patches asked for; a plain firmware build with none gets the
    /// model's default.
    patches: Vec<String>,
    effect_replacements: Vec<(String, String)>,
    version: Option<String>,
    experimental_sd_bundle: bool,
    experimental_patch_loader: bool,
}
/// The original flag form, `l6patch STOCK_DIR OUTPUT_DIR [--experimental-*]`.
fn parse_legacy(
    mut args: impl Iterator<Item = String>,
) -> Result<Options, Box<dyn std::error::Error>> {
    let (Some(source), Some(output)) = (args.next(), args.next()) else {
        return Err(USAGE.into());
    };
    let mut requested = Vec::new();
    let mut effect_replacements = Vec::new();
    let mut version = None;
    let mut experimental_sd_bundle = false;
    let mut experimental_patch_loader = false;
    while let Some(flag) = args.next() {
        if flag == "--experimental-patch-bundle" {
            experimental_sd_bundle = true;
            continue;
        }
        if flag == "--experimental-patch-loader" {
            experimental_patch_loader = true;
            continue;
        }
        let value = args.next().ok_or(USAGE)?;
        match flag.as_str() {
            "--patch" => {
                requested.push(value);
            }
            "--effect-replacement" => {
                let (slot, id) = value
                    .split_once('=')
                    .ok_or("effect replacement must be SLOT=ID")?;
                if slot.is_empty() || id.is_empty() {
                    return Err("effect replacement must be SLOT=ID".into());
                }
                effect_replacements.push((slot.to_owned(), id.to_owned()));
            }
            "--update-version" => version = Some(value),
            _ => return Err(USAGE.into()),
        }
    }
    if let Some(version) = &version {
        parse_version(version)?;
    }
    if experimental_sd_bundle && experimental_patch_loader {
        return Err("build the loader and SD bundle in separate output directories".into());
    }
    Ok(Options {
        source: PathBuf::from(source),
        output: PathBuf::from(output),
        patches: requested,
        effect_replacements,
        version,
        experimental_sd_bundle,
        experimental_patch_loader,
    })
}

#[derive(Debug)]
struct ResolvedHook {
    address: u32,
    symbol: String,
    target: u32,
    length: usize,
    guard_sha256: String,
    continuation_site: Option<String>,
    continuation_handler: Option<String>,
}

struct CompiledPatch {
    name: String,
    base: u32,
    payload: Vec<u8>,
    symbols: String,
    registrations: Vec<HookRegistration>,
    hooks: Vec<ResolvedHook>,
}

fn resolve_compiled_hooks(
    firmware: &[u8],
    firmware_base: u32,
    symbols: &str,
    sites: &[FunctionHookSiteMetadata],
    registrations: &[HookRegistration],
) -> Result<Vec<ResolvedHook>, Box<dyn std::error::Error>> {
    let mut hooks = Vec::new();
    let mut call_dispatch_sites = std::collections::BTreeSet::new();
    for registration in registrations {
        if let HookRegistration::CallPre { function, .. }
        | HookRegistration::CallPost { function, .. } = registration
        {
            if !call_dispatch_sites.insert(function.as_str()) {
                continue;
            }
            let site = sites
                .iter()
                .find(|candidate| candidate.name == *function)
                .ok_or_else(|| {
                    format!("call hook function {function} is absent from define_function_hooks!")
                })?;
            let length = usize::from(site.displaced_len);
            let offset = usize::try_from(
                site.address
                    .checked_sub(firmware_base)
                    .ok_or("hook before firmware")?,
            )?;
            let region = firmware
                .get(offset..offset.checked_add(length).ok_or("hook length overflow")?)
                .ok_or_else(|| format!("hook region outside firmware at {:#x}", site.address))?;
            let guard_sha256 = format!("{:x}", Sha256::digest(region));
            if guard_sha256 != site.guard_sha256 {
                return Err(format!("function hook guard failed: {function}").into());
            }
            hooks.push(ResolvedHook {
                address: site.address,
                symbol: format!("call_dispatch:{function}"),
                target: symbol(symbols, &format!("__l6_call_dispatch_{function}"))?,
                length,
                guard_sha256,
                continuation_site: Some(function.clone()),
                continuation_handler: None,
            });
            continue;
        }
        let (site_name, address, handler, target_symbol) = match registration {
            HookRegistration::Entry { site, handler } => {
                let site = sites
                    .iter()
                    .find(|candidate| candidate.name == *site)
                    .ok_or_else(|| {
                        format!("entry hook {site} is absent from define_function_hooks!")
                    })?;
                (site.name.as_str(), site.address, handler, handler.clone())
            }
            HookRegistration::Offset {
                function,
                offset,
                handler,
                ..
            } => {
                let site = sites
                    .iter()
                    .find(|candidate| candidate.name == *function)
                    .ok_or_else(|| {
                        format!(
                            "offset hook function {function} is absent from define_function_hooks!"
                        )
                    })?;
                let address = site
                    .address
                    .checked_add(*offset)
                    .ok_or("offset hook address overflow")?;
                (
                    site.name.as_str(),
                    address,
                    handler,
                    format!("__l6_offset_veneer_{handler}"),
                )
            }
            HookRegistration::CallPre { .. } | HookRegistration::CallPost { .. } => unreachable!(),
        };
        let site = sites
            .iter()
            .find(|candidate| candidate.name == site_name)
            .ok_or_else(|| {
                format!("hook site {site_name} is absent from define_function_hooks!")
            })?;
        let length = match registration {
            HookRegistration::Offset { displaced_len, .. } => usize::from(*displaced_len),
            _ => usize::from(site.displaced_len),
        };
        let offset = usize::try_from(
            address
                .checked_sub(firmware_base)
                .ok_or("hook before firmware")?,
        )?;
        let region = firmware
            .get(offset..offset.checked_add(length).ok_or("hook length overflow")?)
            .ok_or_else(|| format!("hook region outside firmware at {address:#x}"))?;
        let guard_sha256 = format!("{:x}", Sha256::digest(region));
        match registration {
            HookRegistration::Entry { .. } if guard_sha256 != site.guard_sha256 => {
                return Err(format!("function hook guard failed: {site_name}").into());
            }
            HookRegistration::Offset {
                guard_sha256: expected,
                ..
            } if guard_sha256 != *expected => {
                return Err(
                    format!("offset hook guard failed: {site_name} at {address:#x}").into(),
                );
            }
            _ => {}
        }
        hooks.push(ResolvedHook {
            address,
            symbol: handler.clone(),
            target: symbol(symbols, &target_symbol)?,
            length,
            guard_sha256,
            continuation_site: match registration {
                HookRegistration::Entry { site, .. } => Some(site.clone()),
                HookRegistration::Offset { .. } => None,
                HookRegistration::CallPre { .. } | HookRegistration::CallPost { .. } => {
                    unreachable!()
                }
            },
            continuation_handler: match registration {
                HookRegistration::Entry { handler, .. } => Some(handler.clone()),
                HookRegistration::Offset { .. } => None,
                HookRegistration::CallPre { .. } | HookRegistration::CallPost { .. } => {
                    unreachable!()
                }
            },
        });
    }
    Ok(hooks)
}
// `compile()` and `symbol()` now live in `firmware-patch-compile`, shared
// with the WASM-template prebuild tool so both compile payloads identically.
/// Build target selected for a patch package.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BuildMode {
    Firmware,
    SdBundle,
    PatchLoader,
}

/// Request for the shared patch builder.
pub struct BuildRequest {
    pub source: PathBuf,
    pub output: PathBuf,
    pub patches: Vec<String>,
    pub effect_replacements: Vec<(String, String)>,
    pub version: Option<String>,
    pub mode: BuildMode,
}

/// Build firmware or SD patch artifacts directly, without spawning the CLI.
pub fn build(request: BuildRequest) -> Result<(), Box<dyn std::error::Error>> {
    run_options(Options {
        source: request.source,
        output: request.output,
        patches: request.patches,
        effect_replacements: request.effect_replacements,
        version: request.version,
        experimental_sd_bundle: request.mode == BuildMode::SdBundle,
        experimental_patch_loader: request.mode == BuildMode::PatchLoader,
    })
}

/// Run the command-line interface using process arguments.
pub fn run_cli() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1).peekable();
    match args.peek().map(String::as_str) {
        Some("firmware") | Some("module") => {
            let command = args.next().unwrap();
            build(parse_build(&command, args)?)
        }
        Some("catalog") => {
            args.next();
            catalog(args)
        }
        Some("prebuilt") => {
            args.next();
            prebuilt(args)
        }
        Some("-h" | "--help") | None => {
            println!("{USAGE}");
            Ok(())
        }
        Some(_) => run_options(parse_legacy(args)?),
    }
}

fn parse_build(
    command: &str,
    mut args: impl Iterator<Item = String>,
) -> Result<BuildRequest, Box<dyn std::error::Error>> {
    let (Some(source), Some(output)) = (args.next(), args.next()) else {
        return Err(USAGE.into());
    };
    let module = command == "module";
    let mut request = BuildRequest {
        source: PathBuf::from(source),
        output: PathBuf::from(output),
        patches: Vec::new(),
        effect_replacements: Vec::new(),
        version: None,
        mode: if module {
            BuildMode::SdBundle
        } else {
            BuildMode::Firmware
        },
    };
    while let Some(flag) = args.next() {
        let mut value = || args.next().ok_or_else(|| format!("{flag} needs a value"));
        match flag.as_str() {
            "--patch" => request.patches.push(value()?),
            "--effect" if module => {
                let value = value()?;
                let (slot, id) = value
                    .split_once('=')
                    .filter(|(slot, id)| !slot.is_empty() && !id.is_empty())
                    .ok_or("--effect takes SLOT=ID")?;
                request
                    .effect_replacements
                    .push((slot.to_owned(), id.to_owned()));
            }
            "--version" if !module => request.version = Some(value()?),
            "--loader" if !module => request.mode = BuildMode::PatchLoader,
            _ => return Err(format!("unknown {command} option {flag}\n\n{USAGE}").into()),
        }
    }
    Ok(request)
}

fn catalog(mut args: impl Iterator<Item = String>) -> Result<(), Box<dyn std::error::Error>> {
    let mut model = None;
    let mut json = false;
    while let Some(flag) = args.next() {
        match (flag.as_str(), args.next().as_deref()) {
            ("--model", Some(name)) => model = Some(l6fw::Model::from_name(name).ok_or(USAGE)?),
            ("--format", Some("json")) => json = true,
            ("--format", Some("text")) => json = false,
            _ => return Err(USAGE.into()),
        }
    }
    let packages = patch_core::patch_catalog()
        .iter()
        .filter(|package| model.is_none_or(|model| package.models.contains(&model)));
    if json {
        match model {
            // The website build reads the unfiltered catalog as generated.
            None => print!("{}", patch_core::PATCH_CATALOG_JSON),
            Some(_) => println!(
                "{}",
                serde_json::to_string_pretty(
                    &serde_json::json!({"packages": packages.collect::<Vec<_>>()})
                )?
            ),
        }
        return Ok(());
    }
    let labels = |models: &[l6fw::Model]| {
        models
            .iter()
            .map(|model| model.label())
            .collect::<Vec<_>>()
            .join(",")
    };
    for package in packages {
        let mut delivery = Vec::new();
        if !package.firmware_models.is_empty() {
            delivery.push(format!("firmware {}", labels(&package.firmware_models)));
        }
        if !package.sd_card_models.is_empty() {
            delivery.push(format!("module {}", labels(&package.sd_card_models)));
        }
        let kind = match package.kind {
            patch_core::PackageKind::Patch => "patch",
            patch_core::PackageKind::Loader => "loader",
        };
        println!(
            "{:<20} {:<8} {:<8} {}",
            package.name,
            package.version,
            kind,
            delivery.join("; ")
        );
    }
    Ok(())
}

fn prebuilt(mut args: impl Iterator<Item = String>) -> Result<(), Box<dyn std::error::Error>> {
    let mut config = None;
    let (mut l6max, mut l6) = (None, None);
    let mut check = false;
    while let Some(flag) = args.next() {
        let mut value = || {
            args.next()
                .map(PathBuf::from)
                .ok_or_else(|| format!("{flag} needs a path"))
        };
        match flag.as_str() {
            "--config" => config = Some(value()?),
            "--l6max" => l6max = Some(value()?),
            "--l6" => l6 = Some(value()?),
            "--check" => check = true,
            _ => return Err(format!("unknown prebuilt option {flag}\n\n{USAGE}").into()),
        }
    }
    if check {
        prebuilt::check()?;
        println!("The prebuilt files match the current sources.");
        return Ok(());
    }
    let stock = match (config, l6max, l6) {
        (None, Some(l6max), Some(l6)) => prebuilt::Stock::Unpacked { l6max, l6 },
        (config, None, None) => prebuilt::Stock::Config(
            config.unwrap_or_else(|| prebuilt::repository().join("config.toml")),
        ),
        _ => return Err("pass --config, or both --l6max and --l6".into()),
    };
    prebuilt::generate(stock)
}

fn run_options(options: Options) -> Result<(), Box<dyn std::error::Error>> {
    let source = options.source.canonicalize()?;
    if options.output.exists() {
        return Err("output must be a new directory".into());
    }
    let mut firmware = fs::read(source.join("main_firmware.bin"))?;
    let stock_firmware = firmware.clone();
    let original_length = firmware.len();
    let model = l6fw::model_from_main(&firmware)?;
    let (firmware_base, slot_end) = (model.main_base(), model.slot_end());
    let modules = model.sd_module_region();
    let (module_base, module_end) = (modules.start, modules.end);
    let delivery = if options.experimental_sd_bundle {
        patch_core::Delivery::SdCard
    } else {
        patch_core::Delivery::Firmware
    };
    let default_build = options.patches.is_empty()
        && !options.experimental_patch_loader
        && !options.experimental_sd_bundle;
    let requested = match model {
        l6fw::Model::L6 if default_build => vec!["midi-sync".to_owned()],
        l6fw::Model::L6max if default_build => vec!["knob-dialog".to_owned()],
        _ => options.patches.clone(),
    };
    let plan = patch_core::Plan::new(model, delivery, &requested, &options.effect_replacements)?;
    let patches = plan.patches.clone();
    let version = match options.version {
        Some(version) => parse_version(&version)?,
        None => l6fw::PATCHED_VERSION,
    };
    verify_firmware_for(model, &firmware)?;
    let digest = format!("{:x}", Sha256::digest(&firmware));
    // Validate every hook region before compiling or modifying the output image.
    let roots = patches
        .iter()
        .map(|name| {
            let patch_root = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../patches")
                .join(name);
            let model_root = patch_root.join(model.name());
            let patch_root = if model_root.join("src/payload.rs").is_file() {
                model_root
            } else {
                patch_root
            };
            if !patch_root.join("src/payload.rs").is_file() {
                let model_name = model.label();
                return Err(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    format!("patch `{name}` has no {model_name} implementation"),
                ));
            }
            patch_root.canonicalize()
        })
        .collect::<Result<Vec<_>, _>>()?;
    if options.experimental_sd_bundle {
        fs::create_dir_all(&options.output)?;
        let output = options.output.canonicalize()?;
        let patch_dir = output.join("PATCHES");
        fs::create_dir_all(&patch_dir)?;

        // Dependencies run before their dependents, followed by the user's
        // requested patch order. That order defines same-site hook chains.
        let module_roots = patches.iter().zip(&roots).collect::<Vec<_>>();
        let root_refs = module_roots
            .iter()
            .map(|(name, root)| (name.as_str(), root.as_path()))
            .collect::<Vec<_>>();
        let bundle_name = plan.members().join("-");
        let link_name = "composed-patches";
        let (module, memory_len, symbols, relocations) = compile_sd_relocatable_composed(
            &source,
            &root_refs,
            &options.effect_replacements,
            &output,
            link_name,
            0,
        )?;
        let sites = function_hook_sites(&output.join(format!("{link_name}.payload.elf")))?;
        let registrations = hook_registrations(&symbols)?;
        if registrations.is_empty() {
            return Err("dynamic bundle has no hook registrations".into());
        }
        let firmware_base = model.main_base();
        let image = fs::read(source.join("main_firmware.bin"))?;
        let mut handlers = Vec::with_capacity(registrations.len());
        let mut pass_through = Vec::with_capacity(registrations.len());
        for registration in &registrations {
            let (site_name, handler_symbol, continuation_handler) = match registration {
                HookRegistration::Entry { site, handler } => {
                    (site.as_str(), handler.clone(), handler.clone())
                }
                HookRegistration::CallPre {
                    function,
                    address,
                    handler,
                    ..
                }
                | HookRegistration::CallPost {
                    function,
                    address,
                    handler,
                    ..
                } => {
                    let target = decode_thumb_bl_target(&image, firmware_base, *address)?;
                    let function_site = sites
                        .iter()
                        .find(|site| site.name == *function)
                        .ok_or_else(|| {
                            format!("call hook function {function} is missing from define_function_hooks!")
                        })?;
                    if target != function_site.address {
                        return Err(format!(
                            "dynamic call hook at {address:#x} targets {target:#x}, expected {} at {:#x}",
                            function, function_site.address
                        )
                        .into());
                    }
                    let phase = match registration {
                        HookRegistration::CallPre { .. } => "pre",
                        HookRegistration::CallPost { .. } => "post",
                        _ => unreachable!(),
                    };
                    (
                        function.as_str(),
                        format!("__l6_dynamic_call_{phase}_{handler}"),
                        handler.clone(),
                    )
                }
                HookRegistration::Offset { .. } => {
                    return Err("function-offset hooks are static-only".into());
                }
            };
            let site = sites
                .iter()
                .find(|site| site.name == site_name && site.dynamic_dispatch)
                .ok_or_else(|| format!("dynamic hook {site_name} is not a dynamic catalog site"))?;
            let next_slot = symbol(
                &symbols,
                &format!("dynamic_next_{}_{}_slot", site.name, continuation_handler),
            )?;
            handlers.push((site.address, symbol(&symbols, &handler_symbol)?, next_slot));
            pass_through.push(symbol(
                &symbols,
                &format!("original_{}_{}", site.name, continuation_handler),
            )?);
        }
        let sd_module = patch_abi::dynamic::Module {
            code: module,
            memory_len,
            hooks: handlers
                .iter()
                .map(
                    |&(address, entry_offset, next_slot_offset)| patch_abi::dynamic::Hook {
                        address,
                        entry_offset,
                        next_slot_offset,
                    },
                )
                .collect(),
            relocations,
        };
        let file = sd_module.encode()?;
        let filename = plan.file_name()?;
        fs::write(patch_dir.join(&filename), &file)?;
        let report_patch = serde_json::json!({
            "name": bundle_name,
            "members": patches,
            "effect_replacements": options.effect_replacements.iter().map(|(slot, id)| (slot, id)).collect::<std::collections::BTreeMap<_, _>>(),
            "file": format!("PATCHES/{filename}"),
            "payload_length": sd_module.code.len(),
            "memory_length": memory_len,
            "relocation_count": sd_module.relocations.len(),
            "exports": exported_symbol_offsets(&symbols),
            "writable_data": {
                "offset": symbol(&symbols, "__module_data_start")?,
                "length": symbol(&symbols, "__module_data_end")?
                    .checked_sub(symbol(&symbols, "__module_data_start")?)
                    .ok_or("writable data range underflow")?,
            },
            "pass_through_entry_offsets": pass_through,
            "hooks": handlers.iter().map(|(address, offset, next_offset)| serde_json::json!({"address":format!("{address:#x}"),"entry_offset":offset,"next_slot_offset":next_offset})).collect::<Vec<_>>(),
            "file_length": file.len(),
        });
        let report = serde_json::json!({
            "version": patch_abi::dynamic::FORMAT_VERSION,
            "loader_abi": patch_abi::dynamic::LOADER_ABI,
            "model": model.label(),
            "source_sha256": digest,
            "patches": [report_patch],
        });
        fs::write(
            output.join("patch.json"),
            serde_json::to_vec_pretty(&report)?,
        )?;
        println!("Wrote one composed module under {}", patch_dir.display());
        return Ok(());
    }
    let mut claimed: Vec<(u32, usize)> = Vec::new();
    let mut loader_dispatch_targets = std::collections::BTreeMap::<u32, u32>::new();
    let mut loader_hook_targets = std::collections::BTreeMap::<u32, u32>::new();
    fs::create_dir_all(&options.output)?;
    let output = options.output.canonicalize()?;
    let padding = fs::read(source.join("padding_before_secondary.bin"))?;
    // Payloads are placed back to back, each aligned to 16 bytes.
    let mut end = if options.experimental_sd_bundle {
        module_base
    } else {
        model.first_payload(original_length)
    };
    let initial_payload = end;
    let mut applied = Vec::new();
    // Identity records of every static payload, in placement order.
    let mut static_identities = Vec::new();
    if options.experimental_patch_loader {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../patches/patch-loader")
            .canonicalize()?;
        let (payload, symbols) = if model == l6fw::Model::L6 {
            compile_l6_patch_loader(&source, &root, &output, "patch-loader", end, &[])?
        } else {
            compile_with_continuations(&source, &root, &[], &output, "patch-loader", end)?
        };
        static_identities.push(symbol(&symbols, firmware_patch_compile::PATCH_INFO_SYMBOL)?);
        let catalog = function_hook_sites(&output.join("patch-loader.payload.elf"))?;
        let registrations = hook_registrations(&symbols)?;
        let hooks = resolve_compiled_hooks(
            &stock_firmware,
            firmware_base,
            &symbols,
            &catalog,
            &registrations,
        )?;
        for hook in &hooks {
            loader_hook_targets.insert(hook.address, hook.target | 1);
            let overlap = claimed.iter().find(|&&(address, length)| {
                hook.address < address + length as u32
                    && address < hook.address + hook.length as u32
            });
            if let Some(&(address, length)) = overlap
                && !(address == hook.address
                    && length == hook.length
                    && loader_hook_targets.contains_key(&hook.address))
            {
                return Err(format!("loader hook overlaps another patch: {}", hook.symbol).into());
            }
            claimed.push((hook.address, hook.length));
        }
        let edits = PatchEdits::new(
            hooks
                .iter()
                .map(|hook| HookEdit {
                    address: hook.address,
                    target: hook.target,
                    length: hook.length,
                })
                .collect(),
        );
        let placed = apply_patch_with_map(
            &mut firmware,
            original_length,
            end,
            &payload,
            &edits,
            &padding,
            firmware_base,
            module_base,
        )?;
        let installed_hooks = hooks
            .iter()
            .map(|hook| {
                serde_json::json!({
                    "address": hook.address,
                    "symbol": hook.symbol,
                    "target": hook.target,
                    "displaced_length": hook.length,
                    "displaced_sha256": hook.guard_sha256,
                })
            })
            .collect::<Vec<_>>();
        let mut dispatch_sites = Vec::new();
        for site in &catalog {
            if !site.dynamic_dispatch {
                continue;
            }
            let address = site.address;
            let length = usize::from(site.displaced_len);
            let offset = usize::try_from(
                address
                    .checked_sub(firmware_base)
                    .ok_or("hook before firmware")?,
            )?;
            let region = stock_firmware
                .get(offset..offset.checked_add(length).ok_or("hook length overflow")?)
                .ok_or_else(|| format!("function hook outside firmware at {address:#x}"))?;
            if format!("{:x}", Sha256::digest(region)) != site.guard_sha256 {
                return Err(format!("function hook guard failed: {}", site.name).into());
            }
            let dispatch = symbol(&symbols, &format!("dispatch_{}", site.name))?;
            loader_dispatch_targets.insert(address, dispatch | 1);
            let fallback = symbol(&symbols, &format!("original_{}", site.name))?;
            firmware[offset..offset + 4].copy_from_slice(&branch(address, dispatch, false));
            for chunk in firmware[offset + 4..offset + length].chunks_exact_mut(2) {
                chunk.copy_from_slice(&0xbf00u16.to_le_bytes());
            }
            dispatch_sites.push(serde_json::json!({"address":address,"target":dispatch,"fallback":fallback,"length":length,"function":site.name}));
        }
        applied.push(serde_json::json!({"name":"patch-loader", "payload_address":format!("{end:#x}"), "payload_length":payload.len(), "hooks":installed_hooks, "original_midi_in":format!("{:#x}", symbol(&symbols, "original_midi_in")?)}));
        applied.push(serde_json::json!({"name":"dynamic-dispatch", "sites":dispatch_sites}));
        end = placed.next_base;
    }
    let mut compiled_patches = Vec::new();
    let mut compile_base = end;
    for (name, root) in patches.iter().zip(&roots) {
        let (payload, symbols) = if options.experimental_sd_bundle {
            compile_sd(&source, root, &output, name, compile_base)?
        } else {
            compile(&source, root, &output, name, compile_base)?
        };
        let sites = function_hook_sites(&output.join(format!("{name}.payload.elf")))?;
        let registrations = hook_registrations(&symbols)?;
        let hooks = resolve_compiled_hooks(
            &stock_firmware,
            firmware_base,
            &symbols,
            &sites,
            &registrations,
        )?;
        compile_base = (compile_base + payload.len() as u32 + 15) & !15;
        compiled_patches.push(CompiledPatch {
            name: name.clone(),
            base: compile_base - ((payload.len() as u32 + 15) & !15),
            payload,
            symbols,
            registrations,
            hooks,
        });
    }

    // Compatible entry/call hooks are one chain. Each patch's continuation
    // slot points at the next patch, with the last one retaining stock code.
    let mut hook_owners = std::collections::BTreeMap::<u32, (usize, usize, Option<String>)>::new();
    for patch_index in 0..compiled_patches.len() {
        let patch_hooks = compiled_patches[patch_index].hooks.len();
        for hook_index in 0..patch_hooks {
            let (address, length, target, symbol_name, continuation_site) = {
                let hook = &compiled_patches[patch_index].hooks[hook_index];
                (
                    hook.address,
                    hook.length,
                    hook.target,
                    hook.symbol.clone(),
                    hook.continuation_site.clone(),
                )
            };
            if let Some((first_patch, first_hook, site)) = hook_owners.get(&address).cloned() {
                let first = &compiled_patches[first_patch].hooks[first_hook];
                if first.length != length
                    || site.is_none()
                    || continuation_site.as_ref() != site.as_ref()
                {
                    return Err(format!(
                        "incompatible hooks overlap at {:#x}: {}",
                        address, symbol_name
                    )
                    .into());
                }
                let slot_name = first
                    .continuation_handler
                    .as_ref()
                    .map(|handler| {
                        format!("static_next_{}_{}_slot", site.as_ref().unwrap(), handler)
                    })
                    .unwrap_or_else(|| format!("static_next_{}_slot", site.as_ref().unwrap()));
                let slot = symbol(&compiled_patches[first_patch].symbols, &slot_name)?;
                let slot_offset = usize::try_from(
                    slot.checked_sub(compiled_patches[first_patch].base)
                        .ok_or("chain slot precedes payload")?,
                )?;
                let next_target = target | 1;
                compiled_patches[first_patch].payload[slot_offset..slot_offset + 4]
                    .copy_from_slice(&next_target.to_le_bytes());
                // The first patch's continuation now leads to this patch; for
                // a longer chain, redirect this patch's own continuation next.
                hook_owners.insert(address, (patch_index, hook_index, site));
            } else {
                hook_owners.insert(address, (patch_index, hook_index, continuation_site));
            }
            let is_last_in_chain =
                hook_owners
                    .get(&address)
                    .is_some_and(|(owner_patch, owner_hook, _)| {
                        *owner_patch == patch_index && *owner_hook == hook_index
                    });
            let continuation_target = loader_dispatch_targets
                .get(&address)
                .or_else(|| loader_hook_targets.get(&address))
                .copied();
            if is_last_in_chain
                && let (Some(site), Some(dispatch)) = (
                    compiled_patches[patch_index].hooks[hook_index]
                        .continuation_site
                        .clone(),
                    continuation_target,
                )
            {
                let handler = compiled_patches[patch_index].hooks[hook_index]
                    .continuation_handler
                    .as_ref();
                let slot_name = handler.map_or_else(
                    || format!("static_next_{site}_slot"),
                    |handler| format!("static_next_{site}_{handler}_slot"),
                );
                let slot = symbol(&compiled_patches[patch_index].symbols, &slot_name)?;
                let slot_offset = usize::try_from(
                    slot.checked_sub(compiled_patches[patch_index].base)
                        .ok_or("chain slot precedes payload")?,
                )?;
                compiled_patches[patch_index].payload[slot_offset..slot_offset + 4]
                    .copy_from_slice(&dispatch.to_le_bytes());
            }
        }
    }

    if !options.experimental_sd_bundle {
        for patch in &compiled_patches {
            static_identities.push(symbol(
                &patch.symbols,
                firmware_patch_compile::PATCH_INFO_SYMBOL,
            )?);
        }
    }
    let mut installed_hook_sites = std::collections::BTreeSet::new();
    for patch in compiled_patches {
        let name = patch.name;
        let base = patch.base;
        let payload = patch.payload;
        let registrations = patch.registrations;
        let hooks = patch.hooks;
        let mut unique = std::collections::BTreeSet::new();
        let mut edits_vec = Vec::new();
        let mut installed_hooks = Vec::new();
        for hook in &hooks {
            if !unique.insert(hook.address) {
                continue;
            }
            if installed_hook_sites.contains(&hook.address) {
                continue;
            }
            let prior = claimed.iter().find(|&&(address, length)| {
                hook.address < address + length as u32
                    && address < hook.address + hook.length as u32
            });
            let loader_chain = prior.is_some_and(|&(address, length)| {
                address == hook.address
                    && length == hook.length
                    && loader_hook_targets.contains_key(&hook.address)
            });
            if prior.is_some() && !loader_chain {
                return Err(format!("hook overlaps another patch: {}", hook.symbol).into());
            }
            if prior.is_none() {
                claimed.push((hook.address, hook.length));
            }
            installed_hook_sites.insert(hook.address);
            edits_vec.push(HookEdit {
                address: hook.address,
                target: hook.target,
                length: hook.length,
            });
            installed_hooks.push(hook);
        }
        let edits = PatchEdits::new(edits_vec);
        let applied_patch = apply_patch_with_map(
            &mut firmware,
            original_length,
            base,
            &payload,
            &edits,
            &padding,
            firmware_base,
            slot_end,
        )?;
        end = applied_patch.next_base;
        if options.experimental_patch_loader && end > module_base {
            return Err("installed patch payloads overlap the SD module region".into());
        }
        let mut applied_hooks = Vec::new();
        for (hook_edit, hook) in edits.hooks.iter().zip(installed_hooks) {
            applied_hooks.push(serde_json::json!({"address":hook.address,"symbol":hook.symbol,"target":hook_edit.target,"displaced_length":hook.length,"displaced_sha256":hook.guard_sha256}));
        }
        let call_site_hooks = registrations
            .iter()
            .filter_map(|registration| match registration {
                HookRegistration::CallPre { function, address, handler, .. } => Some(serde_json::json!({"phase":"pre","function":function,"address":format!("{address:#x}"),"handler":handler})),
                HookRegistration::CallPost { function, address, handler, .. } => Some(serde_json::json!({"phase":"post","function":function,"address":format!("{address:#x}"),"handler":handler})),
                _ => None,
            })
            .collect::<Vec<_>>();
        applied.push(serde_json::json!({"name":name,"payload_address":format!("{base:#x}"),"payload_length":applied_patch.payload_len,"hooks":applied_hooks,"call_site_hooks":call_site_hooks}));
    }
    // Link the static identity records into a ring so menu-loader can list
    // every installed payload starting from its own record.
    patch_core::link_identity_ring(&mut firmware, firmware_base, &static_identities)?;
    if options.experimental_patch_loader {
        let reserved_end = (module_end - firmware_base) as usize;
        let consumed = reserved_end - original_length;
        if consumed > padding.len() || padding[..consumed].iter().any(|&byte| byte != 0xff) {
            return Err("SD module reserve overlaps non-erased package data".into());
        }
        firmware.resize(reserved_end, 0xff);
    }
    if options.experimental_sd_bundle {
        let regions = claimed.clone();
        let bundle = patch_abi::bundle::encode(
            &stock_firmware,
            &firmware,
            firmware_base,
            module_base,
            0,
            &regions,
        )?;
        if bundle.len() > 16 * 1024 {
            return Err("experimental SD bundle exceeds the loader's 16 KiB limit".into());
        }
        fs::write(output.join("experimental-patch-bundle.bin"), bundle)?;
    }
    // The output holds the unpacked patched package and the package itself.
    let (mut manifest, mut segments) = l6fw::read_unpacked(&source)?;
    l6fw::replace_main(&mut manifest, &mut segments, firmware.clone(), version)?;
    for (name, bytes) in &segments {
        fs::write(output.join(name), bytes)?;
    }
    fs::write(
        output.join("manifest.json"),
        serde_json::to_string_pretty(&manifest)?,
    )?;
    let packed = l6fw::repack_modified(&manifest, &segments)?;
    let (update_name, trailer_offset) = (model.package_file_name(), model.main_trailer_offset());
    fs::write(output.join(update_name), &packed)?;
    let version_range = trailer_offset + 4..trailer_offset + 8;
    let report = serde_json::json!({"model":model.label(),"source_sha256":digest,"patched_sha256":format!("{:x}",Sha256::digest(&firmware)),"image_length":firmware.len(),"payload_address":format!("{initial_payload:#x}"),"patches":applied,"update_package":update_name,"update_sha256":format!("{:x}",Sha256::digest(&packed)),"version":String::from_utf8_lossy(&packed[version_range]),"usage":"Repacked update for emulator validation; physical bootloader acceptance unverified"});
    fs::write(
        output.join("patch.json"),
        serde_json::to_string_pretty(&report)?,
    )?;
    println!(
        "MAIN image grew by {} bytes ({}); patched firmware: {}",
        firmware.len() - original_length,
        patches.join(" + "),
        output.display()
    );
    Ok(())
}

fn exported_symbol_offsets(symbols: &str) -> serde_json::Value {
    let mut exports = serde_json::Map::new();
    for line in symbols.lines() {
        let fields = line.split_whitespace().collect::<Vec<_>>();
        if fields.len() < 4 || fields[2].len() != 1 {
            continue;
        }
        let Ok(address) = u32::from_str_radix(fields[0], 16) else {
            continue;
        };
        let Some(kind) = fields[2].chars().next() else {
            continue;
        };
        if kind.is_ascii_uppercase() {
            exports.insert(fields[3..].join(" "), serde_json::json!(address & !1));
        }
    }
    serde_json::Value::Object(exports)
}

// Thumb-branch encoding is tested in `patch-core`, which owns `branch()`.

#[cfg(test)]
mod manifest_tests {
    use super::*;

    #[test]
    fn exported_symbol_offsets_keep_global_data_and_clear_thumb_bits() {
        let exports = exported_symbol_offsets(
            "00000011 00000008 T hook_entry\n00000020 00000004 B state_word\n00000030 00000004 b local_state\n",
        );

        assert_eq!(exports["hook_entry"], 0x10);
        assert_eq!(exports["state_word"], 0x20);
        assert!(!exports.as_object().unwrap().contains_key("local_state"));
    }
}

#[cfg(test)]
#[path = "../../../patches/knob-dialog/src/values.rs"]
mod values;

#[cfg(test)]
#[path = "../../../patches/midi-sync/l6max/src/parameters.rs"]
mod parameters;

#[cfg(test)]
#[path = "../../../patches/midi-sync/common/sync_request.rs"]
mod sync_request;

/// An override of the patched version: four ASCII digits, e.g. 0115.
fn parse_version(version: &str) -> Result<[u8; 4], Box<dyn std::error::Error>> {
    version
        .as_bytes()
        .try_into()
        .ok()
        .filter(|digits: &[u8; 4]| digits.iter().all(u8::is_ascii_digit))
        .ok_or_else(|| "update version must be exactly four ASCII digits, e.g. 0115".into())
}
