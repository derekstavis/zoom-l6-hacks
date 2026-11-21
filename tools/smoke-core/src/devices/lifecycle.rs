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

/// Run the firmware updater's full disposable-state lifecycle. This action owns
/// its guest restarts because each boot changes the NOR and SD fixture.
pub fn run_firmware_update(options: &Options) -> Result<(), String> {
    let main =
        fs::read(options.firmware_dir.join("main_firmware.bin")).map_err(|e| e.to_string())?;
    let model = l6fw::model_from_main(&main).map_err(|e| e.to_string())?;
    if model == l6fw::Model::L6 {
        run_l6_update(options, &main)
    } else {
        run_l6max_update(options, &main)
    }
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
    let wasm_model = firmware_patch_wasm::detect_firmware_model(&stock)
        .map_err(|error| format!("WASM firmware model detection failed: {error:?}"))?;
    let expected_wasm_model = match model {
        l6fw::Model::L6 => "L6",
        l6fw::Model::L6max => "L6max",
    };
    if wasm_model != expected_wasm_model {
        return Err(format!(
            "WASM detected {wasm_model}, expected {expected_wasm_model}"
        ));
    }

    let mut results = Vec::new();
    let profiles: Vec<Vec<&str>> = match model {
        l6fw::Model::L6max => (1u16..(1u16 << firmware_patch_core::WASM_PATCHES.len()))
            .map(|mask| {
                firmware_patch_core::WASM_PATCHES
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
            Some("0111"),
            l6patch::BuildMode::Firmware,
        )
        .map_err(|error| format!("native patch build failed for {selected:?}: {error}"))?;
        let filename = match model {
            l6fw::Model::L6 => "L6.BIN",
            l6fw::Model::L6max => "L6max.bin",
        };
        let native = fs::read(native_out.join(filename)).map_err(|error| error.to_string())?;
        let (wasm, report) = firmware_patch_wasm::apply_patches(&stock, selected)
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
        for mask in 0u16..(1u16 << firmware_patch_core::WASM_PATCHES.len()) {
            let selected: Vec<&str> = firmware_patch_core::WASM_PATCHES
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
            let wasm_result =
                firmware_patch_wasm::apply_patches_with_loader(&stock, &selected, true);
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
            match firmware_patch_wasm::apply_patches(&stock, &[unsupported]) {
                Err(message)
                    if message.contains("only the midi-sync patch is supported")
                        || message.contains("is not available for L6 firmware") => {}
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
                !message.contains("only the midi-sync patch is supported")
                    && !message.contains("not supported for firmware installation on L6")
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

pub(super) fn update_word(guest: &Headless, address: u32) -> Result<u32, String> {
    Ok(u32::from_le_bytes(
        guest.engine.management[0]
            .read_memory(0, address, 4)
            .map_err(|e| e.to_string())?
            .try_into()
            .unwrap(),
    ))
}

pub(super) fn run_l6_update(options: &Options, main_image: &[u8]) -> Result<(), String> {
    let package_path = options
        .update_package
        .clone()
        .or_else(|| options.firmware_package.clone())
        .unwrap_or_else(|| options.logs.join("L6-repacked.BIN"));
    if options.update_package.is_none() && options.firmware_package.is_none() {
        l6fw::repack(&options.firmware_dir, &package_path, false).map_err(|e| e.to_string())?;
    }
    let package_path = package_path.canonicalize().map_err(|e| e.to_string())?;
    let package = fs::read(&package_path).map_err(|e| e.to_string())?;
    if l6fw::model_from_package(&package).map_err(|e| e.to_string())? != l6fw::Model::L6 {
        return Err("L6 firmware-update requires an L6 package".into());
    }
    let (_, segments) = l6fw::unpack_bytes(&package).map_err(|e| e.to_string())?;
    let packaged_main = segments
        .get("main_firmware.bin")
        .ok_or("L6 package missing MAIN segment")?;
    if options.update_package.is_none() {
        if packaged_main.as_slice() != main_image {
            return Err("L6 update package MAIN differs from the selected firmware".into());
        }
    } else {
        if !crate::firmware::is_stock(main_image, l6fw::Model::L6)
            || packaged_main.as_slice() == main_image
        {
            return Err(
                "patched L6 update requires stock 1.10 source and a changed MAIN image".into(),
            );
        }
    }
    fs::create_dir_all(&options.logs).map_err(|e| e.to_string())?;
    let root = options.logs.join("l6-update-fixture");
    fs::create_dir(&root).map_err(|e| e.to_string())?;
    let state = root.join("state");
    fs::create_dir_all(&state).map_err(|e| e.to_string())?;
    let mut initial_options = options.clone();
    initial_options.state_dir = Some(state.clone());
    initial_options.sd_image = None;
    initial_options.no_sd = true;
    initial_options.update_bootloader = false;
    initial_options.logs = root.join("before-update");
    let initial = Headless::start(&initial_options).map_err(|e| e.to_string())?;
    if initial.engine.model() != l6fw::Model::L6 {
        return Err("L6 firmware directory booted as another model".into());
    }
    initial.until(40_000).map_err(|e| e.to_string())?;
    drop(initial);
    let initial_nor = fs::read(state.join("main-nor.bin")).map_err(|e| e.to_string())?;
    let card = root.join("update-card.img");
    l6max_host::demo_sd::create(&card, &package_path).map_err(|e| e.to_string())?;
    let nor_path = state.join("main-nor.bin");
    let mut nor = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&nor_path)
        .map_err(|e| e.to_string())?;
    nor.seek(SeekFrom::Start(0x1ff000))
        .map_err(|e| e.to_string())?;
    nor.write_all(b"FW UPDT\0").map_err(|e| e.to_string())?;
    nor.sync_all().map_err(|e| e.to_string())?;
    drop(nor);
    let mut update_options = initial_options;
    update_options.sd_image = Some(card);
    update_options.no_sd = false;
    update_options.update_bootloader = true;
    update_options.logs = root.join("after-update");
    let installed = Headless::start(&update_options).map_err(|e| e.to_string())?;
    if installed.engine.model() != l6fw::Model::L6
        || fs::read_to_string(update_options.logs.join("update-bootloader.log"))
            .map_err(|e| e.to_string())?
            != "Installed\n"
    {
        return Err("L6 replacement installer did not report Installed".into());
    }
    let installed_nor = fs::read(&nor_path).map_err(|e| e.to_string())?;
    for (name, start) in [
        ("main_firmware.bin", 0x50000),
        ("main_trailer.bin", 0x1f2ff8),
        ("secondary_firmware.bin", 0x1f3000),
        ("secondary_trailer.bin", 0x1f6ff8),
        ("package_footer.bin", 0x1f7fc0),
    ] {
        let image = segments
            .get(name)
            .ok_or_else(|| format!("L6 package missing segment {name}"))?;
        if installed_nor.get(start..start + image.len()) != Some(image.as_slice()) {
            return Err(format!("installed L6 segment differs: {name}"));
        }
    }
    if installed_nor.get(0x1ff000..0x1ff008) == Some(b"FW UPDT\0")
        || installed_nor.get(..0x50000) != initial_nor.get(..0x50000)
        || installed_nor.get(0x1f8000..0x1ff000) != initial_nor.get(0x1f8000..0x1ff000)
    {
        return Err("L6 update changed protected NOR or left pending marker".into());
    }
    let resource = segments
        .get("resource_region.bin")
        .ok_or("L6 package has no resource region")?;
    let mut expected_resource = vec![0xff; 0x400000];
    expected_resource[..resource.len()].copy_from_slice(resource);
    if fs::read(state.join("resource-nor.bin")).map_err(|e| e.to_string())? != expected_resource {
        return Err("installed L6 resource NOR differs from package".into());
    }
    let reset_vector = u32::from_le_bytes(packaged_main[4..8].try_into().unwrap());
    if update_word(&installed, l6fw::Model::L6.main_base() + 4)? != reset_vector {
        return Err("installed L6 MAIN reset vector does not match package".into());
    }
    installed.until(40_000).map_err(|e| e.to_string())?;
    drop(installed);
    if options.update_package.is_some() {
        let mut verify = options.clone();
        verify.sd_image = None;
        verify.no_sd = true;
        verify.update_bootloader = false;
        let package = options.update_package.as_ref().unwrap();
        let report = package.with_extension("json");
        verify.firmware_dir = if report.is_file() {
            // A patched package keeps its patch report beside it; the MIDI
            // probe reads the installed hook targets from that report.
            let dir = options.logs.join("post-update-firmware");
            let _ = fs::remove_dir_all(&dir);
            l6fw::unpack(package, &dir)?;
            fs::copy(&report, dir.join("patch.json")).map_err(|e| e.to_string())?;
            dir
        } else {
            l6max_host::setup::unpack_cached(
                package,
                &l6max_host::paths::repository_root().join("emulator/state/firmware"),
            )?
        };
        verify.gdb_socket = Some(gdb_socket_path(&options.logs, "post-update-midi"));
        let session = crate::Session::start(&verify)?;
        let start = session
            .engine
            .inputs
            .as_ref()
            .unwrap()
            .panel
            .snapshot()
            .guest_ms;
        let deadline = Instant::now() + Duration::from_secs(180);
        while session
            .engine
            .inputs
            .as_ref()
            .unwrap()
            .panel
            .snapshot()
            .guest_ms
            .wrapping_sub(start)
            < 5_000
        {
            session.poll()?;
            if Instant::now() >= deadline {
                return Err("patched L6 did not reach its post-update MIDI probe point".into());
            }
            thread::sleep(Duration::from_millis(5));
        }
        State::default()
            .execute(&session, &verify, Operation::L6MidiSync, 120_000, 300)
            .map_err(|error| format!("installed L6 midi-sync behavior failed: {error}"))?;
    }
    fs::write(
        options.logs.join("firmware-update.json"),
        serde_json::to_vec_pretty(&json!({"model":"l6","outcome":"PASS","segments_installed":5,"resource_region":true,"pending_marker_cleared":true,"protected_regions_preserved":true,"installed_firmware_booted":true})).unwrap(),
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

pub(super) fn update_checksum(package: &[u8]) -> u32 {
    package[0x200..0x1a8200]
        .iter()
        .fold(0u32, |sum, &byte| sum.wrapping_add(u32::from(byte)))
}

pub(super) fn run_l6max_update(options: &Options, main: &[u8]) -> Result<(), String> {
    use sha2::{Digest, Sha256};
    let source = options
        .firmware_package
        .as_ref()
        .ok_or("L6max firmware-update requires its original package in the fixture")?;
    let target_package = options
        .update_package
        .as_ref()
        .map(fs::read)
        .transpose()
        .map_err(|e| e.to_string())?;
    let original = fs::read(source).map_err(|e| e.to_string())?;
    if format!("{:x}", Sha256::digest(&original)) != crate::firmware::STOCK_L6MAX_PACKAGE_SHA256
        || !crate::firmware::is_stock(main, l6fw::Model::L6max)
    {
        return Err(
            "L6max firmware-update requires the hash-pinned stock 1.10 package and MAIN".into(),
        );
    }
    if update_checksum(&original) != u32::from_be_bytes(original[0x1fc..0x200].try_into().unwrap())
    {
        return Err("original L6max package checksum mismatch".into());
    }
    fs::create_dir_all(&options.logs).map_err(|e| e.to_string())?;
    let root = options.logs.join("l6max-update-fixtures");
    fs::create_dir(&root).map_err(|e| e.to_string())?;
    for valid in [false, true] {
        let label = if valid { "valid" } else { "bad-checksum" };
        let directory = root.join(label);
        fs::create_dir(&directory).map_err(|e| e.to_string())?;
        let mut package = if valid {
            target_package.clone().unwrap_or_else(|| original.clone())
        } else {
            original.clone()
        };
        if target_package.is_none() {
            for offset in [0x1a31fc, 0x1a81fc] {
                package[offset..offset + 4].copy_from_slice(b"0111");
            }
            package[0x1a71fc..0x1a7200].copy_from_slice(b"0101");
            package[0x21c..0x220].copy_from_slice(&0x13579bdfu32.to_le_bytes());
            package[0x1a321c..0x1a3220].copy_from_slice(&0x2468ace0u32.to_le_bytes());
            if valid {
                let checksum = update_checksum(&package).to_be_bytes();
                package[0x1fc..0x200].copy_from_slice(&checksum);
            }
        } else if valid {
            l6max_host::package::check_layout(&package).map_err(|e| e.to_string())?;
            l6max_host::update_bootloader::validate(&package).map_err(|e| e.to_string())?;
            if package[0x1a31fc..0x1a3200].iter().cmp(b"0110".iter()) != std::cmp::Ordering::Greater
            {
                return Err(
                    "patched L6max update package must carry a version newer than 1.10".into(),
                );
            }
        } else {
            package[0x1fc] ^= 1;
        }
        let card = directory.join("card.img");
        let mut volume = sd_image::create_empty(&card, "L6MAX").map_err(|e| e.to_string())?;
        volume
            .write("L6max.BIN", &package)
            .and_then(|()| volume.flush())
            .map_err(|e| e.to_string())?;
        drop(volume);
        let mut local = options.clone();
        local.sd_image = Some(card);
        local.no_sd = false;
        local.state_dir = Some(directory.join("state"));
        local.update_bootloader = false;
        local.logs = directory.join("boot");
        let guest = Headless::start(&local).map_err(|e| e.to_string())?;
        guest.until(40_000).map_err(|e| e.to_string())?;
        let mode = update_word(&guest, l6max::system_mode)?;
        let flags = guest.engine.management[0]
            .read_memory(0, l6max::update_package_flags, 1)
            .map_err(|e| e.to_string())?[0];
        if flags & 9 != 0 || mode != if valid { 6 } else { 1 } {
            return Err(format!(
                "{label}: firmware did not gate package as expected; mode={mode}, flags={flags:#x}"
            ));
        }
        let nor_path = directory.join("state/main-nor.bin");
        if valid {
            if update_word(&guest, l6max::current_window)? != l6max::GUI_UPDATE_DIALOG_WINDOW {
                return Err("valid L6max package did not open the update dialog".into());
            }
            guest.tap(2, 50, 1000).map_err(|e| e.to_string())?;
            guest.tap(3, 50, 1000).map_err(|e| e.to_string())?;
            if update_word(&guest, l6max::current_window)? != l6max::GUI_UPDATE_PROGRESS_WINDOW
                || update_word(&guest, l6max::GUI_UPDATE_PROGRESS_WINDOW)? & 0x100 == 0
            {
                return Err("valid L6max update preparation did not complete".into());
            }
            let before = fs::read(&nor_path).map_err(|e| e.to_string())?;
            if before.get(0x1ff000..0x1ff008) == Some(b"FW UPDT\0") {
                return Err("update marker set before power handoff".into());
            }
            guest
                .engine
                .inputs
                .as_ref()
                .unwrap()
                .tap(53, 3500)
                .map_err(|e| e.to_string())?;
            let deadline = Instant::now() + Duration::from_secs(30);
            while !guest
                .engine
                .inputs
                .as_ref()
                .unwrap()
                .main
                .snapshot()
                .powered_off
            {
                guest.display.sample().map_err(|e| e.to_string())?;
                if Instant::now() > deadline {
                    return Err("L6max firmware did not cut power".into());
                }
                thread::sleep(Duration::from_millis(5));
            }
            drop(guest);
            let pending = fs::read(&nor_path).map_err(|e| e.to_string())?;
            if pending.get(0x1ff000..0x1ff008) != Some(b"FW UPDT\0") {
                return Err("power handoff did not save update marker".into());
            }
            local.update_bootloader = true;
            local.logs = directory.join("installed");
            let installed = Headless::start(&local).map_err(|e| e.to_string())?;
            installed.until(40_000).map_err(|e| e.to_string())?;
            if fs::read_to_string(local.logs.join("update-bootloader.log"))
                .map_err(|e| e.to_string())?
                != "Installed\n"
                || update_word(&installed, l6max::system_mode)? != 4
                || (target_package.is_none()
                    && update_word(&installed, l6fw::Model::L6max.main_base() + 0x1c)?
                        != 0x13579bdf)
            {
                return Err("L6max update install or application handoff failed".into());
            }
            let programmed =
                fs::read_to_string(local.logs.join("engine.trace")).map_err(|e| e.to_string())?;
            if target_package.is_none()
                && (!programmed.contains("l6 panel ROM 16 KiB image match: yes")
                    || programmed
                        .lines()
                        .filter(|line| line.starts_with("l6 panel ROM write 080"))
                        .count()
                        != 64)
            {
                return Err("L6max panel ROM programming did not verify 64 writes".into());
            }
            drop(installed);
            let after = fs::read(&nor_path).map_err(|e| e.to_string())?;
            if after.get(..0x50000) != pending.get(..0x50000)
                || after.get(0x50000..0x1f8000) != package.get(0x200..)
                || after.get(0x1f8000..0x1ff000) != pending.get(0x1f8000..0x1ff000)
                || after.get(0x1ff000..0x1ff008) == Some(b"FW UPDT\0")
            {
                return Err("L6max update changed protected NOR or left update marker".into());
            }
            let panel =
                fs::read(directory.join("state/panel-flash.bin")).map_err(|e| e.to_string())?;
            if panel.get(..0x3ffc) != package.get(0x1a3200..0x1a71fc)
                || panel.get(0x3ffc..0x4000) != package.get(0x1a71fc..0x1a7200)
            {
                return Err("installed L6max panel firmware differs from update package".into());
            }
            local.sd_image = None;
            local.no_sd = true;
            local.logs = directory.join("restart");
            let restarted = Headless::start(&local).map_err(|e| e.to_string())?;
            restarted.until(40_000).map_err(|e| e.to_string())?;
            if (target_package.is_none()
                && update_word(&restarted, l6fw::Model::L6max.main_base() + 0x1c)? != 0x13579bdf)
                || fs::read_to_string(local.logs.join("update-bootloader.log"))
                    .map_err(|e| e.to_string())?
                    != "NoPending\n"
                || fs::read(directory.join("state/panel-flash.bin")).map_err(|e| e.to_string())?
                    != panel
            {
                return Err("L6max installed image did not survive clean restart".into());
            }
            let trace =
                fs::read_to_string(local.logs.join("engine.trace")).map_err(|e| e.to_string())?;
            if trace.contains("l6 panel ROM erase") || trace.contains("l6 panel ROM mass erase") {
                return Err("L6max unexpectedly reinstalled panel firmware on restart".into());
            }
            if target_package.is_some() {
                drop(restarted);
                let mut verify = local.clone();
                verify.sd_image = None;
                verify.no_sd = true;
                verify.update_bootloader = false;
                verify.firmware_dir = l6max_host::setup::unpack_cached(
                    options.update_package.as_ref().unwrap(),
                    &l6max_host::paths::repository_root().join("emulator/state/firmware"),
                )?;
                verify.gdb_socket = Some(gdb_socket_path(&options.logs, "post-update-knob"));
                let session = crate::Session::start(&verify)?;
                let start = session
                    .engine
                    .inputs
                    .as_ref()
                    .unwrap()
                    .panel
                    .snapshot()
                    .guest_ms;
                let deadline = Instant::now() + Duration::from_secs(180);
                while session
                    .engine
                    .inputs
                    .as_ref()
                    .unwrap()
                    .panel
                    .snapshot()
                    .guest_ms
                    .wrapping_sub(start)
                    < 40_000
                {
                    session.poll()?;
                    if Instant::now() >= deadline {
                        return Err("patched L6max did not reach its post-update UI state".into());
                    }
                    thread::sleep(Duration::from_millis(5));
                }
                // Check the behavior of each patch the update installs. The
                // installed-patch cases check the same browser-built image
                // in full; din-mixer-control needs a DIN port and Mixer
                // Control via MIDI, which this boot does not set up.
                let report: Value = serde_json::from_slice(
                    &fs::read(
                        options
                            .update_package
                            .as_ref()
                            .unwrap()
                            .with_extension("json"),
                    )
                    .map_err(|e| e.to_string())?,
                )
                .map_err(|e| e.to_string())?;
                let patches: Vec<String> = report["patches"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|patch| patch.as_str().map(str::to_owned))
                    .collect();
                let mut state = State::default();
                let mut run = |operation: Operation, patch: &str| {
                    state
                        .execute(&session, &verify, operation, 180_000, 300)
                        .map(|_| ())
                        .map_err(|error| {
                            format!("installed L6max {patch} behavior failed: {error}")
                        })
                };
                if patches.iter().any(|patch| patch == "knob-dialog") {
                    run(Operation::KnobDialog, "knob-dialog")?;
                }
                if let Some(patch) = patches.iter().find(|patch| *patch == "midi-sync") {
                    run(
                        Operation::DirectUsbMidiRoundTrip {
                            cable: 1,
                            expect_ring_change: true,
                        },
                        patch,
                    )?;
                    run(Operation::HostMidiSync { request: true }, patch)?;
                }
            }
        } else {
            drop(guest);
            let nor = fs::read(&nor_path).map_err(|e| e.to_string())?;
            if nor.get(0x1ff000..0x1ff008) == Some(b"FW UPDT\0") {
                return Err("corrupt L6max package triggered an update".into());
            }
        }
    }
    fs::write(
        options.logs.join("firmware-update.json"),
        serde_json::to_vec_pretty(&json!({"model":"l6max","outcome":"PASS","valid_package_installed":true,"corrupt_package_rejected":true,"panel_flash_verified":true,"protected_storage_preserved":true,"clean_restart_verified":true})).unwrap(),
    )
    .map_err(|e| e.to_string())?;
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
