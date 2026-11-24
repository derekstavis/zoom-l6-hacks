//! Building a case's fixture: patched firmware, SD bundle, state and card.
use crate::{
    cache::{self, RunCache},
    config::Config,
    hashing::{files, hash, hash_patch_directory, hash_sparse},
};
use l6fw::Model;
use l6max_host::engine::Options;
use serde_json::{Value, json};
use smoke_core::{Scenario, fixture_layout as layout};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

/// What a case's fixture was built from, written to `inputs.json`. The
/// identity record in `report.json` is read from it.
#[derive(Clone, Debug, serde::Serialize)]
pub(crate) struct Inputs {
    pub(crate) fixture: String,
    pub(crate) model: Model,
    /// SHA-256 of every unpacked firmware file, by name.
    pub(crate) firmware: BTreeMap<String, String>,
    pub(crate) package_sha256: Option<String>,
    pub(crate) patches: Vec<String>,
    pub(crate) sd_patches: Option<Vec<String>>,
    pub(crate) effect_replacements: BTreeMap<String, String>,
    pub(crate) installed_patches: Vec<String>,
    pub(crate) installed_sd_patches: Vec<String>,
    pub(crate) sd_bundle_mode: Option<String>,
    pub(crate) sd_patch_set_sha256: Option<String>,
    pub(crate) update_package_sha256: Option<String>,
    /// The state spec's hash, resolved form and the report of applying it.
    pub(crate) state_spec: Option<Value>,
    /// The card the fixture built, before case options apply.
    pub(crate) sd_sha256: Option<String>,
    pub(crate) audio_device: String,
    pub(crate) qemu_sha256: String,
    /// What the case actually ran with, once its options apply.
    #[serde(flatten)]
    pub(crate) execution: Option<Execution>,
}

/// The card and state a case runs with after `volatile` and `no_sd` apply.
#[derive(Clone, Debug, serde::Serialize)]
pub(crate) struct Execution {
    #[serde(rename = "execution_spec_state")]
    pub(crate) spec_state: bool,
    #[serde(rename = "execution_volatile")]
    pub(crate) volatile: bool,
    #[serde(rename = "execution_no_sd")]
    pub(crate) no_sd: bool,
    #[serde(rename = "execution_sd_sha256")]
    pub(crate) sd_sha256: Option<String>,
    /// SHA-256 of every state file, by path.
    #[serde(rename = "execution_state_sha256")]
    pub(crate) state_sha256: Option<BTreeMap<String, String>>,
}

/// What one case runs from: the configured firmware and its scenario's declarations.
#[derive(Clone)]
pub(crate) struct Fixture {
    pub(crate) name: String,
    pub(crate) model: Model,
    pub(crate) firmware_package: Option<PathBuf>,
    pub(crate) patches: Vec<String>,
    pub(crate) installed_patches: Vec<String>,
    pub(crate) installed_sd_patches: Vec<String>,
    pub(crate) sd_patches: Option<Vec<String>>,
    pub(crate) effect_replacements: BTreeMap<String, String>,
    pub(crate) sd_bundle_mode: Option<String>,
    pub(crate) sd_bundle: Option<PathBuf>,
    pub(crate) update_package: Option<PathBuf>,
    pub(crate) state_spec: Option<PathBuf>,
    pub(crate) card_spec: Option<PathBuf>,
    pub(crate) audio_device: Option<String>,
    pub(crate) audio_input: Vec<smoke_core::AudioInput>,
    pub(crate) gdb: bool,
}
pub(crate) fn audio_device_spec(fixture: &Fixture, logs: &Path) -> String {
    fixture
        .audio_device
        .as_deref()
        .unwrap_or("driver=none,id=l6-audio")
        .replace("{logs}", &logs.display().to_string())
}
pub(crate) fn gdb_socket_path(logs: &Path) -> PathBuf {
    smoke_core::devices::gdb_socket_path(logs, "main")
}
/// The fixture for a case, from the configured firmware and the case's scenario.
/// Cases without a scenario boot from the completed-setup state and an empty card.
pub(crate) fn case_fixture(
    config: &Config,
    model: Model,
    case_id: &str,
    scenario: Option<&Scenario>,
) -> Result<Fixture, String> {
    let firmware = config
        .firmware
        .get(&model)
        .ok_or_else(|| format!("no firmware package configured for {model}"))?;
    let (state, card) = match scenario {
        Some(scenario) => (scenario.state.as_deref(), scenario.card.as_deref()),
        None => (Some("setup-complete"), Some("empty")),
    };
    Ok(Fixture {
        name: case_id.into(),
        model,
        firmware_package: Some(firmware.clone()),
        patches: scenario.map_or_else(Vec::new, |s| s.patches.clone()),
        installed_patches: scenario.map_or_else(Vec::new, |s| s.installed_patches.clone()),
        installed_sd_patches: scenario.map_or_else(Vec::new, |s| s.installed_sd_patches.clone()),
        sd_patches: scenario.and_then(|s| s.sd_patches.clone()),
        effect_replacements: scenario.map_or_else(BTreeMap::new, |s| s.effect_replacements.clone()),
        sd_bundle_mode: scenario.and_then(|s| s.sd_bundle_mode.clone()),
        sd_bundle: None,
        update_package: None,
        state_spec: state.map(|name| crate::catalog::state_spec(model, name)),
        card_spec: card.map(crate::catalog::card_spec),
        audio_device: scenario.and_then(|s| s.audio_device.clone()),
        audio_input: scenario.map_or_else(Vec::new, |s| s.audio_input.clone()),
        gdb: scenario.is_some_and(|s| s.gdb),
    })
}

/// A configured or built package, unpacked once per run. Builders and cases
/// booting stock firmware only read the directory; a case whose firmware
/// directory gets a `patch.json` copies it first.
pub(crate) fn unpacked(cache: &RunCache, package: &Path) -> Result<PathBuf, String> {
    let sha = cache.file_hash(package)?;
    cache.directory("unpacked", &sha, |out| l6fw::unpack(package, out))
}

/// One `l6patch` build per distinct request in a run, copied into `output`.
pub(crate) fn l6patch_build(
    cache: &RunCache,
    package: &Path,
    output: &Path,
    request: l6patch::BuildRequest,
) -> Result<(), String> {
    // Destructured so a new request field cannot be left out of the key.
    let l6patch::BuildRequest {
        source: _,
        output: _,
        patches,
        effect_replacements,
        version,
        mode,
    } = &request;
    let key = json!({
        "package": cache.file_hash(package)?,
        "mode": format!("{mode:?}"),
        "patches": patches,
        "effect_replacements": effect_replacements,
        "version": version,
    })
    .to_string();
    let built = cache.directory("l6patch", &key, |out| {
        let source = unpacked(cache, package)?;
        l6patch::build(l6patch::BuildRequest {
            source,
            output: out.to_path_buf(),
            ..request
        })
        .map_err(|error| format!("l6patch failed: {error}"))
    })?;
    cache::copy_tree(&built, output)
}

/// Patch a package with the browser patcher once per distinct request in a
/// run. Returns the cached patched package and its report.
pub(crate) fn wasm_patch(
    cache: &RunCache,
    package: &Path,
    names: &[String],
) -> Result<(PathBuf, Value), String> {
    let key = json!({"package": cache.file_hash(package)?, "patches": names}).to_string();
    let built = cache.directory("wasm-patch", &key, |out| {
        fs::create_dir_all(out).map_err(|e| e.to_string())?;
        let bytes = fs::read(package).map_err(|e| e.to_string())?;
        let names: Vec<&str> = names.iter().map(String::as_str).collect();
        let (patched, report) = patch_wasm::apply_patches(&bytes, &names)?;
        fs::write(out.join("patched.bin"), patched).map_err(|e| e.to_string())?;
        fs::write(
            out.join("report.json"),
            serde_json::to_vec_pretty(&report).unwrap(),
        )
        .map_err(|e| e.to_string())
    })?;
    let report =
        serde_json::from_slice(&fs::read(built.join("report.json")).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
    Ok((built.join("patched.bin"), report))
}

/// Construct the experimental loader and card bundle from the configured
/// stock package. Both outputs are per-case and recorded with the fixture.
pub(crate) fn prepare_sd_patch(
    fixture: &mut Fixture,
    dir: &Path,
    cache: &RunCache,
    stages: &crate::report::StageRecorder,
) -> Result<(), String> {
    let Some(patches) = fixture.sd_patches.clone() else {
        return Ok(());
    };
    if !fixture.installed_patches.is_empty() {
        return Err("SD patch fixtures cannot combine installed_patches".into());
    }
    if (!patches.is_empty() || !fixture.effect_replacements.is_empty())
        && fixture.card_spec.is_none()
    {
        return Err("SD patches need a card spec".into());
    }
    let package = fixture
        .firmware_package
        .clone()
        .ok_or("SD patches need firmware")?;
    fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    // `smoke-core` reads the stock image from the case's `sd-stock-firmware`
    // to locate the loader's edits, so each SD case keeps its own copy.
    let timer = stages.start(crate::report::Stage::SdStockFirmware);
    cache::copy_tree(
        &unpacked(cache, &package)?,
        &dir.join(layout::SD_STOCK_FIRMWARE),
    )?;
    drop(timer);
    let build = |output: &Path,
                 mode: l6patch::BuildMode,
                 selected: &[String],
                 effect_replacements: &[(String, String)]|
     -> Result<(), String> {
        let result = l6patch_build(
            cache,
            &package,
            output,
            l6patch::BuildRequest {
                source: PathBuf::new(),
                output: PathBuf::new(),
                patches: selected.to_vec(),
                effect_replacements: effect_replacements.to_vec(),
                version: None,
                mode,
            },
        );
        let name = if mode == l6patch::BuildMode::PatchLoader {
            "patch-loader-build.log"
        } else {
            "sd-patches-build.log"
        };
        match result {
            Ok(()) => fs::write(
                dir.join(name),
                format!("l6patch {mode:?} build succeeded\n"),
            )
            .map_err(|e| e.to_string())?,
            Err(error) => {
                fs::write(dir.join(name), &error).map_err(|e| e.to_string())?;
                return Err(error);
            }
        }
        Ok(())
    };
    let loader = dir.join(layout::LOADER_BUILD);
    let timer = stages.start(crate::report::Stage::SdLoaderBuild);
    build(
        &loader,
        l6patch::BuildMode::PatchLoader,
        &fixture.patches,
        &[],
    )?;
    drop(timer);
    // The case boots the loader build; its report identifies that MAIN.
    fs::copy(
        loader.join(layout::PATCH_REPORT),
        dir.join(layout::FIRMWARE_REPORT),
    )
    .map_err(|e| e.to_string())?;
    fixture.firmware_package = Some(loader.join(fixture.model.package_file_name()));
    fixture.patches.clear();
    if !patches.is_empty() || !fixture.effect_replacements.is_empty() {
        let timer = stages.start(crate::report::Stage::SdModuleBuild);
        let bundle = dir.join(layout::SD_BUNDLE_BUILD);
        let effect_replacements = fixture
            .effect_replacements
            .iter()
            .map(|(slot, effect)| (slot.clone(), effect.clone()))
            .collect::<Vec<_>>();
        build(
            &bundle,
            l6patch::BuildMode::Module,
            &patches,
            &effect_replacements,
        )?;
        fixture.sd_bundle = Some(bundle.join("PATCHES"));
        drop(timer);
    }
    Ok(())
}

pub(crate) fn fixture_options(
    config: &Config,
    fixture: &Fixture,
    dir: &Path,
    cache: &RunCache,
    stages: &crate::report::StageRecorder,
) -> Result<(Options, Inputs), String> {
    let timer = stages.start(crate::report::Stage::Firmware);
    fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let mut selected_package = fixture.firmware_package.clone();
    let firmware = if !fixture.patches.is_empty() {
        let package = fixture
            .firmware_package
            .as_ref()
            .ok_or("patches require firmware_package")?;
        let firmware_model = fixture.model;
        // The browser patcher builds what it supports, as the website does.
        // Other firmware patches are built with l6patch.
        let (patched_package, report) = if patch_core::Plan::new(
            firmware_model,
            patch_core::Delivery::Browser,
            &fixture.patches,
            &[],
        )
        .is_ok()
        {
            let (cached, report) = wasm_patch(cache, package, &fixture.patches)?;
            let patched_package = dir.join("patched-firmware.bin");
            fs::copy(&cached, &patched_package).map_err(|e| e.to_string())?;
            (patched_package, report)
        } else {
            let build = dir.join("firmware-build");
            l6patch_build(
                cache,
                package,
                &build,
                l6patch::BuildRequest {
                    source: PathBuf::new(),
                    output: PathBuf::new(),
                    patches: fixture.patches.clone(),
                    effect_replacements: Vec::new(),
                    version: None,
                    mode: l6patch::BuildMode::Firmware,
                },
            )?;
            let report: Value = serde_json::from_slice(
                &fs::read(build.join("patch.json")).map_err(|e| e.to_string())?,
            )
            .map_err(|e| e.to_string())?;
            (build.join(fixture.model.package_file_name()), report)
        };
        fs::write(
            dir.join(layout::FIRMWARE_REPORT),
            serde_json::to_vec_pretty(&report).unwrap(),
        )
        .map_err(|e| e.to_string())?;
        let output = dir.join("firmware");
        cache::copy_tree(&unpacked(cache, &patched_package)?, &output)?;
        fs::write(
            output.join("patch.json"),
            serde_json::to_vec_pretty(&report).unwrap(),
        )
        .map_err(|e| e.to_string())?;
        selected_package = Some(patched_package);
        output
    } else {
        // Stock firmware is booted read-only from the run's shared unpack,
        // as it was from the repository-wide firmware cache before.
        unpacked(cache, fixture.firmware_package.as_ref().unwrap())?
    };
    let main = fs::read(firmware.join("main_firmware.bin")).map_err(|e| e.to_string())?;
    let model = l6fw::model_from_main(&main)?;
    if model != fixture.model {
        return Err("fixture model differs from selected firmware".into());
    }
    if let Some(package) = &fixture.update_package {
        let bytes = fs::read(package).map_err(|e| e.to_string())?;
        if l6fw::model_from_package(&bytes).map_err(|e| e.to_string())? != model {
            return Err("update package belongs to a different model than the fixture".into());
        }
    }
    drop(timer);
    let timer = stages.start(crate::report::Stage::State);
    let state = dir.join("state");
    fs::create_dir_all(&state).map_err(|e| e.to_string())?;
    // A spec edits the disposable copy: seed missing NOR from the selected
    // firmware, commit settings with the firmware's bank protocol, and write
    // the panel RTC/flash it declares.
    let firmware_files = files(&firmware)?;
    let state_spec = fixture
        .state_spec
        .as_ref()
        .map(|path| -> Result<Value, String> {
            // A state depends only on its spec, with what it extends, and the
            // firmware it seeds from.
            let spec = l6nor::StateSpec::load(path)?;
            let resolved =
                crate::hashing::digest(&serde_json::to_value(&spec).map_err(|e| e.to_string())?);
            let key = json!({"spec": resolved, "firmware": firmware_files}).to_string();
            let built = cache.directory("state", &key, |out| {
                fs::create_dir_all(out.join("state")).map_err(|e| e.to_string())?;
                let report = l6nor::state::apply(&spec, Some(&firmware), &out.join("state"))?;
                fs::write(
                    out.join("applied.json"),
                    serde_json::to_vec_pretty(&json!(report)).unwrap(),
                )
                .map_err(|e| e.to_string())
            })?;
            cache::copy_tree(&built.join("state"), &state)?;
            let report: Value = serde_json::from_slice(
                &fs::read(built.join("applied.json")).map_err(|e| e.to_string())?,
            )
            .map_err(|e| e.to_string())?;
            Ok(json!({"sha256": hash(path)?, "resolved": resolved, "applied": report}))
        })
        .transpose()?;
    let audio_device = audio_device_spec(fixture, dir);
    let mut args = vec![
        "--no-auto-setup".into(),
        "--qemu".into(),
        config.qemu.clone().into_os_string(),
        "--firmware-dir".into(),
        firmware.clone().into_os_string(),
        "--state-dir".into(),
        state.into_os_string(),
        "--logs".into(),
        dir.as_os_str().to_owned(),
        "--audio-device".into(),
        audio_device.clone().into(),
    ];
    if fixture.gdb {
        args.extend(["--gdb-socket".into(), gdb_socket_path(dir).into_os_string()]);
    }
    drop(timer);
    let timer = stages.start(crate::report::Stage::Card);
    let sd_hash = if let Some(spec) = &fixture.card_spec {
        let target = dir.join("card.img");
        let mut card = sd_image::spec::CardSpec::load_for(spec, fixture.model.name())?;
        if let Some(bundle_dir) = &fixture.sd_bundle
            && fixture.sd_bundle_mode.as_deref() != Some("absent")
        {
            let mut files = fs::read_dir(bundle_dir)
                .map_err(|e| format!("could not enumerate patch output directory: {e}"))?
                .map(|entry| entry.map_err(|e| e.to_string()))
                .collect::<Result<Vec<_>, _>>()?;
            files.sort_by_key(|entry| entry.file_name());
            if fixture.sd_bundle_mode.as_deref() == Some("ordered-chain") {
                let report: serde_json::Value = serde_json::from_slice(
                    &fs::read(bundle_dir.parent().unwrap().join("patch.json"))
                        .map_err(|e| e.to_string())?,
                )
                .map_err(|e| e.to_string())?;
                let filename = report["patches"][0]["file"]
                    .as_str()
                    .and_then(|path| Path::new(path).file_name())
                    .ok_or("dynamic module report has no output filename")?;
                let original = fs::read(bundle_dir.join(filename)).map_err(|e| e.to_string())?;
                let pass_through = report["patches"][0]["pass_through_entry_offset"]
                    .as_u64()
                    .or_else(|| report["patches"][0]["pass_through_entry_offsets"][0].as_u64())
                    .ok_or("dynamic module report has no pass-through entry")?
                    as u32;
                let pass_through_module = edit_module(&original, |module| {
                    let [hook] = &mut module.hooks[..] else {
                        return Err("ordered-chain fixture expects one dynamic hook".into());
                    };
                    hook.entry_offset = pass_through;
                    // A second package, so the loader chains both copies rather
                    // than keeping one of two identical packages.
                    set_identity(module, Some("chain-first"), None)
                })?;
                let first_path = dir.join(layout::CHAIN_FIRST);
                let second_path = dir.join("sd-chain-second.BIN");
                fs::write(&first_path, pass_through_module).map_err(|e| e.to_string())?;
                fs::write(&second_path, original).map_err(|e| e.to_string())?;
                // Insert in reverse order so the firmware must sort by numeric prefix.
                card.file.push(sd_image::spec::File {
                    path: "PATCHES/10-SECOND.BIN".into(),
                    content: sd_image::spec::Content::Copy(second_path),
                });
                card.file.push(sd_image::spec::File {
                    path: "PATCHES/2-FIRST.BIN".into(),
                    content: sd_image::spec::Content::Copy(first_path),
                });
            } else if let Some(copy) = match fixture.sd_bundle_mode.as_deref() {
                // A second, identical file of the same module; it loads once.
                Some("duplicate-module") => Some(("PATCHES/99-COPY.BIN", None)),
                // An older version that loads first; the newer one wins.
                Some("older-module") => Some(("PATCHES/01-OLD.BIN", Some("0.0.1"))),
                // A multi-package module twice: neither copy loads.
                Some("duplicate-bundle") => Some(("PATCHES/99-COPY.BIN", None)),
                _ => None,
            } {
                let (path, version) = copy;
                let first = files.first().ok_or("empty SD patch directory")?;
                let original = fs::read(first.path()).map_err(|e| e.to_string())?;
                let bytes = if version.is_some() {
                    edit_module(&original, |module| set_identity(module, None, version))?
                } else {
                    original
                };
                let copied = dir.join("sd-duplicate.BIN");
                fs::write(&copied, bytes).map_err(|e| e.to_string())?;
                card.file.push(sd_image::spec::File {
                    path: path.into(),
                    content: sd_image::spec::Content::Copy(copied),
                });
            } else if fixture.sd_bundle_mode.as_deref() == Some("corrupt-first") {
                let first = files.first().ok_or("empty SD patch directory")?;
                let mut bytes = fs::read(first.path()).map_err(|e| e.to_string())?;
                *bytes.last_mut().ok_or("empty SD patch")? ^= 1;
                let corrupt = dir.join("sd-corrupt-first.BIN");
                fs::write(&corrupt, bytes).map_err(|e| e.to_string())?;
                card.file.push(sd_image::spec::File {
                    path: "PATCHES/AAA.BIN".into(),
                    content: sd_image::spec::Content::Copy(corrupt),
                });
            } else if fixture.sd_bundle_mode.as_deref() == Some("composed-module") {
                let report: serde_json::Value = serde_json::from_slice(
                    &fs::read(bundle_dir.parent().unwrap().join("patch.json"))
                        .map_err(|e| e.to_string())?,
                )
                .map_err(|e| e.to_string())?;
                let module = report["patches"]
                    .as_array()
                    .and_then(|patches| patches.first())
                    .ok_or("composed module report has no module")?;
                let members = module["members"]
                    .as_array()
                    .ok_or("composed module report has no members")?;
                if !fixture.effect_replacements.is_empty() {
                    let effect_modules = members
                        .iter()
                        .filter_map(serde_json::Value::as_str)
                        .filter(|member| *member == "efx-plateau" || *member == "efx-velvet")
                        .count();
                    if !members.iter().any(|member| member == "efx-loader")
                        || !members.iter().any(|member| member == "knob-dialog")
                        || effect_modules == 0
                    {
                        return Err("effect replacement bundle is missing the router, knob dialog, or selected effect modules".into());
                    }
                } else if members.is_empty() {
                    return Err("composed module report has no package members".into());
                }
                let file = module["file"]
                    .as_str()
                    .ok_or("composed module has no output path")?;
                card.file.push(sd_image::spec::File {
                    path: file.into(),
                    content: sd_image::spec::Content::Copy(
                        bundle_dir.join(
                            Path::new(file)
                                .file_name()
                                .ok_or("composed module path has no filename")?,
                        ),
                    ),
                });
            }
            for entry in files {
                if !entry.file_type().map_err(|e| e.to_string())?.is_file() {
                    continue;
                }
                if fixture.sd_bundle_mode.as_deref() == Some("ordered-chain") {
                    continue;
                }
                if fixture.sd_bundle_mode.as_deref() == Some("composed-module") {
                    continue;
                }
                let filename = entry.file_name().to_string_lossy().into_owned();
                let source = if fixture.sd_bundle_mode.as_deref() == Some("corrupt") {
                    let mut bytes = fs::read(entry.path()).map_err(|e| e.to_string())?;
                    *bytes.last_mut().ok_or("empty SD patch")? ^= 1;
                    let corrupt = dir.join(format!("sd-corrupt-{filename}"));
                    fs::write(&corrupt, bytes).map_err(|e| e.to_string())?;
                    corrupt
                } else {
                    entry.path()
                };
                card.file.push(sd_image::spec::File {
                    path: format!("PATCHES/{filename}"),
                    content: sd_image::spec::Content::Copy(source),
                });
            }
        }
        // `copy` paths in a card spec are from the repository root.
        let base = l6max_host::paths::repository_root();
        if cache.clones() {
            // A card depends on its resolved spec, with the files the driver
            // added, and the bytes of every file it copies.
            let mut copies = Vec::new();
            let mut key_card = card.clone();
            for file in &mut key_card.file {
                if let sd_image::spec::Content::Copy(path) = &mut file.content {
                    let content_hash = hash(&base.join(&*path))?;
                    copies.push(json!([file.path, content_hash]));
                    // The source path may point into this run's directory;
                    // the card depends on the file's bytes, not that path.
                    *path = PathBuf::from(format!("sha256:{content_hash}"));
                }
            }
            let resolved = serde_json::to_value(&key_card).map_err(|e| e.to_string())?;
            let key =
                json!({"card": crate::hashing::digest(&resolved), "copies": copies}).to_string();
            let built = cache.directory("card", &key, |out| {
                fs::create_dir_all(out).map_err(|e| e.to_string())?;
                card.build(&out.join("card.img"), base)
                    .map_err(|e| e.to_string())
            })?;
            fs::copy(built.join("card.img"), &target).map_err(|e| e.to_string())?;
        } else {
            card.build(&target, base).map_err(|e| e.to_string())?;
        }
        args.extend(["--sd-image".into(), target.clone().into_os_string()]);
        let hash_timer = stages.start(crate::report::Stage::CardHash);
        let hash = hash_sparse(&target)?;
        drop(hash_timer);
        Some(hash)
    } else {
        if fixture.sd_bundle.is_some() {
            return Err("SD patch bundle requires a card spec".into());
        }
        args.push("--no-sd".into());
        None
    };
    drop(timer);
    let timer = stages.start(crate::report::Stage::FixtureInputs);
    if let Some(package) = &selected_package {
        args.extend([
            "--firmware-package".into(),
            package.clone().into_os_string(),
        ]);
    }
    if let Some(package) = &fixture.update_package {
        args.extend(["--update-package".into(), package.clone().into_os_string()]);
    }
    for patch in &fixture.installed_patches {
        args.extend(["--patch".into(), patch.into()]);
    }
    for patch in &fixture.installed_sd_patches {
        args.extend(["--sd-patch".into(), patch.into()]);
    }
    let options = Options::parse_args(args)?;
    let inputs = Inputs {
        fixture: fixture.name.clone(),
        model: fixture.model.clone(),
        firmware: firmware_files,
        package_sha256: selected_package.as_deref().map(hash).transpose()?,
        patches: fixture.patches.clone(),
        sd_patches: fixture.sd_patches.clone(),
        effect_replacements: fixture.effect_replacements.clone(),
        installed_patches: fixture.installed_patches.clone(),
        installed_sd_patches: fixture.installed_sd_patches.clone(),
        sd_bundle_mode: fixture.sd_bundle_mode.clone(),
        sd_patch_set_sha256: fixture
            .sd_bundle
            .as_deref()
            .map(hash_patch_directory)
            .transpose()?,
        update_package_sha256: fixture.update_package.as_deref().map(hash).transpose()?,
        state_spec,
        sd_sha256: sd_hash,
        audio_device,
        qemu_sha256: config.qemu_sha256.clone(),
        execution: None,
    };
    drop(timer);
    Ok((options, inputs))
}
pub(crate) fn engine_args(options: &Options) -> Vec<String> {
    let mut result = vec![
        "--no-auto-setup".into(),
        "--qemu".into(),
        options.qemu.as_ref().unwrap().display().to_string(),
        "--firmware-dir".into(),
        options.firmware_dir.display().to_string(),
        "--logs".into(),
        options.logs.display().to_string(),
        "--audio-device".into(),
        options.audio_device.clone().unwrap(),
    ];
    if let Some(socket) = &options.gdb_socket {
        result.extend(["--gdb-socket".into(), socket.display().to_string()]);
    }
    if let Some(state) = &options.state_dir {
        result.extend(["--state-dir".into(), state.display().to_string()]);
    } else {
        result.push("--volatile".into());
    }
    if let Some(card) = &options.sd_image {
        result.extend(["--sd-image".into(), card.display().to_string()]);
    } else {
        result.push("--no-sd".into());
    }
    if let Some(package) = &options.firmware_package {
        result.extend(["--firmware-package".into(), package.display().to_string()]);
    }
    if let Some(package) = &options.update_package {
        result.extend(["--update-package".into(), package.display().to_string()]);
    }
    for patch in options.patches.iter().flatten() {
        result.extend(["--patch".into(), patch.clone()]);
    }
    for patch in options.sd_patches.iter().flatten() {
        result.extend(["--sd-patch".into(), patch.clone()]);
    }
    if options.update_bootloader {
        result.push("--update-bootloader".into());
    }
    if options.usb_enabled {
        result.push("--usb".into());
    }
    if options.usb_cable_at_start {
        result.push("--usb-cable-at-start".into());
    }
    if options.midi_enabled {
        result.push("--midi".into());
    }
    if options.trace_sd {
        result.push("--trace-sd".into());
    }
    if options.start_paused {
        result.push("--start-paused".into());
    }
    result
}
/// Build persistent state by running a semantic headless scenario on an isolated device.
pub(crate) fn expand(value: &str, options: &Options) -> String {
    let mut text = value
        .replace(
            "{firmware_dir}",
            &options.firmware_dir.display().to_string(),
        )
        .replace("{logs}", &options.logs.display().to_string())
        .replace(
            "{qemu}",
            &options.qemu.as_ref().unwrap().display().to_string(),
        );
    if let Some(package) = &options.firmware_package {
        text = text.replace("{package}", &package.display().to_string());
    }
    text
}

/// The model of a configured package, read once per run.
pub(crate) fn package_model(cache: &RunCache, package: &Path) -> Result<Model, String> {
    let sha = cache.file_hash(package)?;
    cache
        .value(&format!("package-model:{sha}"), || {
            let bytes = fs::read(package).map_err(|e| e.to_string())?;
            let model = l6fw::model_from_package(&bytes).map_err(|e| e.to_string())?;
            Ok(model.name().to_string())
        })?
        .parse()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        env,
        time::{SystemTime, UNIX_EPOCH},
    };
    static NEXT_TEMP: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    fn temp() -> PathBuf {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = env::temp_dir().join(format!(
            "l6test-unit-{}-{stamp}-{}",
            std::process::id(),
            NEXT_TEMP.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    #[ignore = "requires L6_SD_TEST_PACKAGE and L6_SD_TEST_MODEL"]
    fn builds_sd_fixture_from_local_firmware() {
        let model: Model = env::var("L6_SD_TEST_MODEL")
            .expect("set L6_SD_TEST_MODEL")
            .parse()
            .unwrap();
        let package =
            PathBuf::from(env::var_os("L6_SD_TEST_PACKAGE").expect("set L6_SD_TEST_PACKAGE"));
        let selected = env::var("L6_SD_TEST_PATCHES")
            .unwrap_or_else(|_| "midi-sync".into())
            .split(',')
            .map(str::to_owned)
            .collect::<Vec<_>>();
        let static_patches = env::var("L6_SD_TEST_STATIC")
            .ok()
            .map(|s| s.split(',').map(str::to_owned).collect())
            .unwrap_or_default();
        let dir = temp();
        let mut fixture = Fixture {
            name: "sd-fixture".into(),
            model,
            firmware_package: Some(package),
            patches: static_patches,
            installed_patches: vec![],
            installed_sd_patches: vec![],
            sd_patches: Some(selected.clone()),
            effect_replacements: BTreeMap::new(),
            sd_bundle_mode: None,
            sd_bundle: None,
            update_package: None,
            state_spec: None,
            card_spec: Some(crate::catalog::card_spec("empty")),
            audio_device: None,
            audio_input: vec![],
            gdb: false,
        };
        let cache = RunCache::new(dir.join("cache"));
        prepare_sd_patch(
            &mut fixture,
            &dir,
            &cache,
            &crate::report::StageRecorder::default(),
        )
        .unwrap();
        let patch_dir = fixture.sd_bundle.as_ref().unwrap();
        let report: serde_json::Value = serde_json::from_slice(
            &fs::read(patch_dir.parent().unwrap().join("patch.json")).unwrap(),
        )
        .unwrap();
        let filename = report["patches"][0]["file"]
            .as_str()
            .and_then(|path| Path::new(path).file_name())
            .unwrap();
        let patch_file = patch_dir.join(filename);
        let bytes = fs::read(&patch_file).unwrap();
        patch_abi::dynamic::Module::decode(&bytes).unwrap();
        let package = fixture.firmware_package.as_ref().unwrap();
        let unpacked = dir.join("verify-loader");
        l6fw::unpack(package, &unpacked).unwrap();
        let spec = fixture.card_spec.as_ref().unwrap();
        let mut card = sd_image::spec::CardSpec::load_for(spec, fixture.model.name()).unwrap();
        let patch_path = format!("PATCHES/{}", filename.to_string_lossy());
        card.file.push(sd_image::spec::File {
            path: patch_path.clone(),
            content: sd_image::spec::Content::Copy(patch_file),
        });
        let image = dir.join("card.img");
        card.build(&image, spec.parent().unwrap()).unwrap();
        assert!(sd_image::entry_exists(&image, &patch_path).unwrap());
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn fixture_audio_backend_resolves_the_case_artifact_directory() {
        let fixture = Fixture {
            name: "l6-recorder-playback".into(),
            model: Model::L6,
            firmware_package: None,
            patches: vec![],
            installed_patches: vec![],
            installed_sd_patches: vec![],
            sd_patches: None,
            effect_replacements: BTreeMap::new(),
            sd_bundle_mode: None,
            sd_bundle: None,
            update_package: None,
            state_spec: None,
            card_spec: None,
            audio_device: Some("driver=wav,id=l6-audio,path={logs}/audio.wav".into()),
            audio_input: Vec::new(),
            gdb: true,
        };
        assert_eq!(
            audio_device_spec(&fixture, Path::new("/tmp/case-logs")),
            "driver=wav,id=l6-audio,path=/tmp/case-logs/audio.wav"
        );
        let default_fixture = Fixture {
            audio_device: None,
            audio_input: Vec::new(),
            gdb: false,
            ..fixture
        };
        assert_eq!(
            audio_device_spec(&default_fixture, Path::new("/tmp/case-logs")),
            "driver=none,id=l6-audio"
        );
    }
    #[test]
    fn gdb_socket_path_fits_qemus_unix_socket_limit() {
        assert!(
            gdb_socket_path(Path::new("/tmp/case"))
                .as_os_str()
                .as_encoded_bytes()
                .len()
                < 100
        );
    }
    #[test]
    fn a_case_fixture_comes_from_its_scenario() {
        let config = Config {
            firmware: BTreeMap::from([(Model::L6max, PathBuf::from("L6max.bin"))]),
            qemu: PathBuf::new(),
            qemu_sha256: String::new(),
            app: None,
        };
        let catalog = crate::catalog::load(&l6max_host::paths::repository_root()).unwrap();
        let scenario = &catalog.scenarios["host-midi-sync"];
        let fixture =
            case_fixture(&config, Model::L6max, "host-midi-sync", Some(scenario)).unwrap();
        assert_eq!(fixture.patches, ["midi-sync"]);
        assert!(fixture.state_spec.unwrap().is_file());
        assert!(
            case_fixture(&config, Model::L6, "host-midi-sync", Some(scenario))
                .err()
                .unwrap()
                .contains("no firmware package")
        );
    }

    #[test]
    fn every_scenario_card_has_a_valid_spec_for_each_model() {
        let catalog = crate::catalog::load(&l6max_host::paths::repository_root()).unwrap();
        let mut cards = 0;
        for (id, scenario) in &catalog.scenarios {
            for model in &scenario.models {
                let config = Config {
                    firmware: BTreeMap::from([(*model, PathBuf::from("firmware.bin"))]),
                    qemu: PathBuf::new(),
                    qemu_sha256: String::new(),
                    app: None,
                };
                let fixture = case_fixture(&config, *model, id, Some(scenario)).unwrap();
                if let Some(spec_path) = &fixture.card_spec {
                    let spec = sd_image::spec::CardSpec::load_for(spec_path, model.name()).unwrap();
                    assert_eq!(spec.model, model.name(), "{}", spec_path.display());
                    cards += 1;
                }
                if let Some(spec_path) = &fixture.state_spec {
                    let spec = l6nor::StateSpec::load(spec_path).unwrap();
                    assert_eq!(spec.model, model.name(), "{}", spec_path.display());
                }
            }
        }
        assert!(cards > 0);
    }

    #[test]
    fn package_placeholder_uses_the_selected_generated_package() {
        let dir = temp();
        let args = vec![
            "--qemu".into(),
            dir.join("qemu").into_os_string(),
            "--firmware-dir".into(),
            dir.join("firmware").into_os_string(),
            "--firmware-package".into(),
            dir.join("patched.bin").into_os_string(),
        ];
        let options = Options::parse_args(args).unwrap();
        assert_eq!(
            expand("{package}", &options),
            dir.join("patched.bin").display().to_string()
        );
        fs::remove_dir_all(dir).unwrap();
    }
}

/// Decode an SD module, edit it and encode it again.
fn edit_module(
    bytes: &[u8],
    edit: impl FnOnce(&mut patch_abi::dynamic::Module) -> Result<(), String>,
) -> Result<Vec<u8>, String> {
    let mut module = patch_abi::dynamic::Module::decode(bytes)
        .map_err(|e| format!("invalid SD module fixture: {e:?}"))?;
    edit(&mut module)?;
    Ok(module.encode()?)
}

/// Rename the module's first package record or change its version. The
/// module's identity record is at the start of its code; it points at the
/// records by their offset in the code.
fn set_identity(
    module: &mut patch_abi::dynamic::Module,
    name: Option<&str>,
    version: Option<&str>,
) -> Result<(), String> {
    let code = &mut module.code;
    let word = |code: &[u8], offset: usize| {
        code.get(offset..offset + 4)
            .map(|bytes| u32::from_le_bytes(bytes.try_into().unwrap()))
            .ok_or("SD module too short for its identity record")
    };
    if word(code, 0)? != 0x4950_364c {
        return Err("SD module has no identity record".into());
    }
    let record = word(code, 8)? as usize;
    for (field, offset, len) in [(name, 0, 32), (version, 32, 16)] {
        if let Some(text) = field {
            if text.len() >= len {
                return Err(format!("identity text {text} is too long"));
            }
            let target = code
                .get_mut(record + offset..record + offset + len)
                .ok_or("SD module identity record is outside the module")?;
            target.fill(0);
            target[..text.len()].copy_from_slice(text.as_bytes());
        }
    }
    Ok(())
}
