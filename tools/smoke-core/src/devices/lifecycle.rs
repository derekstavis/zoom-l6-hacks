//! Firmware lifecycle: factory fixtures, updates and the browser patch builder.
use super::*;

pub(super) fn prepare_factory_fixture(options: &mut Options) -> Result<(), String> {
    let state = options.logs.join("factory-state");
    let card = options.logs.join("factory-card.img");
    if options.state_dir.as_ref() == Some(&state)
        && options.sd_image.as_ref() == Some(&card)
        && state.is_dir()
        && card.is_file()
    {
        return Ok(());
    }
    if options.state_dir.is_some() || options.sd_image.is_some() {
        return Err("factory-protocol owns its disposable state and SD fixture".into());
    }
    let main = fs::read(options.firmware_dir.join("main_firmware.bin"))
        .map_err(|error| error.to_string())?;
    if !crate::firmware::is_stock(&main, l6fw::Model::L6max) {
        return Err("factory-protocol requires the hash-verified L6max MAIN image".into());
    }
    fs::create_dir_all(&options.logs).map_err(|error| error.to_string())?;
    fs::create_dir(&state).map_err(|error| error.to_string())?;
    create_factory_card(&card)?;
    options.sd_image = Some(card);
    options.state_dir = Some(state);
    options.no_sd = false;
    Ok(())
}

/// Compare the browser patcher's full model/profile matrix with the native
/// l6patch library API. The native builder is the reference implementation;
/// each supported output must match byte-for-byte, and unsupported L6 profiles
/// must be rejected by both implementations.
pub(super) fn build_native_patch(
    source: &Path,
    output: &Path,
    patches: &[&str],
    version: Option<&str>,
    mode: l6patch::BuildMode,
) -> Result<(), String> {
    l6patch::build(l6patch::BuildRequest {
        source: source.to_path_buf(),
        output: output.to_path_buf(),
        patches: patches.iter().map(|patch| (*patch).to_owned()).collect(),
        effect_replacements: Vec::new(),
        version: version.map(str::to_owned),
        mode,
    })
    .map_err(|error| error.to_string())
}

pub fn run_wasm_patch(options: &Options) -> Result<(), String> {
    use sha2::{Digest, Sha256};

    let main = fs::read(options.firmware_dir.join("main_firmware.bin"))
        .map_err(|error| error.to_string())?;
    let model = l6fw::model_from_main(&main).map_err(|error| error.to_string())?;
    let label = model.name();
    let root = options.logs.join("wasm-patch-lifecycle");
    fs::create_dir_all(&root).map_err(|error| error.to_string())?;
    let stock_path = root.join("stock.bin");
    l6fw::repack(&options.firmware_dir, &stock_path, false).map_err(|error| error.to_string())?;
    let stock = fs::read(&stock_path).map_err(|error| error.to_string())?;
    if l6fw::model_from_package(&stock).map_err(|error| error.to_string())? != model {
        return Err(format!(
            "repacked firmware package does not detect as {label}"
        ));
    }
    let wasm_model = patch_wasm::detect_firmware_model(&stock)
        .map_err(|error| format!("WASM firmware model detection failed: {error:?}"))?;
    let expected_wasm_model = model.label();
    if wasm_model != expected_wasm_model {
        return Err(format!(
            "WASM detected {wasm_model}, expected {expected_wasm_model}"
        ));
    }

    let mut results = Vec::new();
    let profiles: Vec<Vec<&str>> = match model {
        l6fw::Model::L6max => (1u16..(1u16 << patch_core::WASM_PATCHES.len()))
            .map(|mask| {
                patch_core::WASM_PATCHES
                    .iter()
                    .enumerate()
                    .filter(|(index, _)| mask & (1u16 << index) != 0)
                    .map(|(_, patch)| *patch)
                    .collect()
            })
            .collect(),
        l6fw::Model::L6 => vec![vec!["midi-sync"]],
    };

    for (index, selected) in profiles.iter().enumerate() {
        let native_out = root.join(format!("native-{index}"));
        build_native_patch(
            &options.firmware_dir,
            &native_out,
            selected,
            None,
            l6patch::BuildMode::Firmware,
        )
        .map_err(|error| format!("native patch build failed for {selected:?}: {error}"))?;
        let native = fs::read(native_out.join(model.package_file_name()))
            .map_err(|error| error.to_string())?;
        let (wasm, report) = patch_wasm::apply_patches(&stock, selected)
            .map_err(|message| format!("WASM patching failed for {selected:?}: {message}"))?;
        if wasm != native {
            return Err(format!(
                "{label} {selected:?} mismatch: WASM {} bytes SHA-256 {:x}, native {} bytes SHA-256 {:x}",
                wasm.len(),
                Sha256::digest(&wasm),
                native.len(),
                Sha256::digest(&native)
            ));
        }
        results.push(json!({
            "patches": selected,
            "bytes": wasm.len(),
            "sha256": format!("{:x}", Sha256::digest(&wasm)),
            "usage": report["usage"].as_str().unwrap_or("")
        }));
    }

    // The persistent loader can coexist with any static L6max patch subset.
    // Include the loader-only profile: users may install the loader first and
    // add SD patches later without rebuilding the firmware update.
    let mut loader_results = Vec::new();
    if model == l6fw::Model::L6max {
        for mask in 0u16..(1u16 << patch_core::WASM_PATCHES.len()) {
            let selected: Vec<&str> = patch_core::WASM_PATCHES
                .iter()
                .enumerate()
                .filter(|(index, _)| mask & (1u16 << index) != 0)
                .map(|(_, patch)| *patch)
                .collect();
            let native_out = root.join(format!("native-loader-{mask:03}"));
            let native_result = build_native_patch(
                &options.firmware_dir,
                &native_out,
                &selected,
                None,
                l6patch::BuildMode::PatchLoader,
            );
            let wasm_result = patch_wasm::apply_patches_with_loader(&stock, &selected, true);
            let native = match native_result {
                Ok(()) => {
                    fs::read(native_out.join("L6max.bin")).map_err(|error| error.to_string())?
                }
                Err(native_error)
                    if native_error
                        .contains("installed patch payloads overlap the SD module region") =>
                {
                    match wasm_result {
                        Err(wasm_error)
                            if wasm_error.contains("overlap the Patch loader module reserve") =>
                        {
                            // Both builders reject a profile that cannot fit
                            // beside the loader's reserved SD module area.
                            continue;
                        }
                        Err(wasm_error) => {
                            return Err(format!(
                                "native and WASM reject {selected:?} for different reasons: {native_error} / {wasm_error}"
                            ));
                        }
                        Ok(_) => {
                            return Err(format!(
                                "WASM accepted {selected:?}, which native rejects because its static payload overlaps the SD module region"
                            ));
                        }
                    }
                }
                Err(error) => {
                    return Err(format!(
                        "native patch-loader build failed for {selected:?}: {error}"
                    ));
                }
            };
            let (wasm, report) = wasm_result.map_err(|message| {
                format!("WASM patch-loader build failed for {selected:?}: {message}")
            })?;
            if wasm != native {
                fs::write(root.join(format!("wasm-loader-{mask:03}.bin")), &wasm)
                    .map_err(|error| error.to_string())?;
                let differences = wasm
                    .iter()
                    .zip(&native)
                    .enumerate()
                    .filter_map(|(offset, (left, right))| {
                        (left != right).then_some((offset, *left, *right))
                    })
                    .take(16)
                    .collect::<Vec<_>>();
                return Err(format!(
                    "L6max loader {selected:?} mismatch at {differences:?}: WASM {} bytes SHA-256 {:x}, native {} bytes SHA-256 {:x}",
                    wasm.len(),
                    Sha256::digest(&wasm),
                    native.len(),
                    Sha256::digest(&native)
                ));
            }
            loader_results.push(json!({
                "patches": selected,
                "bytes": wasm.len(),
                "sha256": format!("{:x}", Sha256::digest(&wasm)),
                "usage": report["usage"].as_str().unwrap_or("")
            }));
        }
    }

    if model == l6fw::Model::L6 {
        for unsupported in ["knob-dialog", "din-mixer-control"] {
            match patch_wasm::apply_patches(&stock, &[unsupported]) {
                Err(message) if message.contains("is not available for L6 in the browser") => {}
                Err(message) => {
                    return Err(format!(
                        "WASM rejected unsupported L6 profile {unsupported} for an unexpected reason: {message}"
                    ));
                }
                _ => {
                    return Err(format!(
                        "WASM accepted unsupported L6 profile {unsupported}"
                    ));
                }
            }
            let native_out = root.join(format!("native-unsupported-{unsupported}"));
            let output = build_native_patch(
                &options.firmware_dir,
                &native_out,
                &[unsupported],
                None,
                l6patch::BuildMode::Firmware,
            );
            if output.is_ok() {
                return Err(format!(
                    "native patcher accepted unsupported L6 profile {unsupported}"
                ));
            } else if {
                let message = output.unwrap_err().to_string();
                !message.contains("is not available for L6 firmware")
            } {
                return Err(format!(
                    "native patcher rejected unsupported L6 profile {unsupported} for an unexpected reason"
                ));
            }
        }
    }

    fs::write(
        options.logs.join("wasm-patch.json"),
        serde_json::to_vec_pretty(&json!({"model":label,"stock_sha256":format!("{:x}", Sha256::digest(&stock)),"profiles":results,"patch_loader_profiles":loader_results}))
            .map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;
    Ok(())
}

pub(super) fn create_factory_card(card: &std::path::Path) -> Result<(), String> {
    let mut volume = sd_image::create_empty(card, "L6MAX").map_err(|e| e.to_string())?;
    volume
        .write(
            "D445_JIG_000.txt",
            b"emulator fixture; no calibration parameters\n",
        )
        .and_then(|()| volume.flush())
        .map_err(|error| error.to_string())
}
