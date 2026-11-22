//! SD patch loading: fixture edits, the loader registry and effect replacement.
use super::*;

/// Verify the generated per-patch module before inspecting its live registry.
pub(super) type SdEdits = (
    Vec<(u32, Vec<u8>, u32)>,
    u32,
    Vec<u8>,
    Option<u32>,
    Vec<patch_abi::bundle::Relocation>,
    Vec<patch_abi::dynamic::Hook>,
    Vec<(
        Vec<u8>,
        Vec<patch_abi::bundle::Relocation>,
        Vec<patch_abi::dynamic::Hook>,
    )>,
);
pub(super) fn sd_fixture_root(options: &Options) -> Result<&std::path::Path, String> {
    options
        .logs
        .ancestors()
        .find(|dir| {
            dir.join(crate::fixture_layout::SD_STOCK_FIRMWARE)
                .join("main_firmware.bin")
                .is_file()
        })
        .ok_or_else(|| "missing generated SD fixture".into())
}

pub(super) fn composed_module_report(report: &Value) -> Option<&Value> {
    report["patches"].as_array()?.first()
}

pub(super) fn composed_export_offset(report: &Value, name: &str) -> Option<u32> {
    u32::try_from(composed_module_report(report)?["exports"][name].as_u64()?).ok()
}

pub(super) fn effect_parameter_edit(
    options: &Options,
    session: &Session,
    encoder: usize,
    steps: i32,
) -> Result<Value, String> {
    if session.engine.model() != l6fw::Model::L6max {
        return Err("L6max effect parameter editing requires L6max".into());
    }
    let socket = options
        .gdb_socket
        .as_ref()
        .ok_or("L6max effect parameter editing requires GDB")?;
    let root = sd_fixture_root(options)?;
    let report: Value = serde_json::from_slice(
        &fs::read(
            root.join(crate::fixture_layout::SD_BUNDLE_BUILD)
                .join(crate::fixture_layout::PATCH_REPORT),
        )
        .map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;
    let symbol_offset = composed_export_offset(&report, "DUAL_EFFECT_EDIT_APPLIED")
        .ok_or("Effect editor counter is missing from its module report")?;
    let event_offset = composed_export_offset(&report, "DUAL_EFFECT_EDIT_EVENT")
        .ok_or("Effect editor event is missing from its module report")?;
    let callback_hits_offset = composed_export_offset(&report, "DUAL_EFFECT_CALLBACK_HITS")
        .ok_or("Effect callback counter is missing from its module report")?;
    let qmp = &session.engine.management[0];
    use firmware_api::l6max::current_window;
    use firmware_api::l6max::mixer_apply_channel_parameter;
    use firmware_api::l6max::mixer_efx_type;
    let effect_type = qmp
        .read_memory(0, mixer_efx_type, 1)
        .map_err(|error| error.to_string())?
        .first()
        .copied()
        .ok_or("missing selected effect type")?;
    if effect_type != 1 && effect_type != 2 {
        return Err(format!(
            "Effect parameter edit requires Room or Spring, selected type is {effect_type}"
        ));
    }
    let registry = u32::from_le_bytes(
        qmp.read_memory(0, patch_abi::registry::L6MAX_POINTER as u32, 4)
            .map_err(|error| error.to_string())?
            .try_into()
            .unwrap(),
    );
    let module_base = u32::from_le_bytes(
        qmp.read_memory(0, registry + patch_abi::registry::MODULE_PTR_OFFSET, 4)
            .map_err(|error| error.to_string())?
            .try_into()
            .unwrap(),
    );
    if module_base == 0 {
        return Err("Effect replacement module is not loaded".into());
    }
    let edit_mode_offset = composed_export_offset(&report, "DUAL_EFFECT_PARAMETER_EDIT_MODE")
        .ok_or("Effect editor mode is missing from its module report")?;
    let ring_positions_offset = composed_export_offset(&report, "PARAMETER_RING_POSITIONS")
        .ok_or("Effect ring state is missing from its module report")?;
    let edit_mode = u32::from_le_bytes(
        qmp.read_memory(0, module_base + edit_mode_offset, 4)
            .map_err(|error| error.to_string())?
            .try_into()
            .unwrap(),
    );
    if edit_mode == 0 {
        return Err(
            "Enable the selected effect's editor with TAP before turning an encoder".into(),
        );
    }
    let parameter_count = if effect_type == 1 { 8 } else { 7 };
    if encoder == 0 || encoder > parameter_count {
        return Err(format!(
            "selected effect has {parameter_count} editable encoders"
        ));
    }
    let ring_address = module_base + ring_positions_offset + ((encoder - 1) as u32) * 4;
    let ring_before = u32::from_le_bytes(
        qmp.read_memory(0, ring_address, 4)
            .map_err(|error| error.to_string())?
            .try_into()
            .unwrap(),
    );
    let edit_counter = module_base
        .checked_add(symbol_offset)
        .ok_or("effect replacement edit counter address overflow")?;
    let before = u32::from_le_bytes(
        qmp.read_memory(0, edit_counter, 4)
            .map_err(|error| error.to_string())?
            .try_into()
            .unwrap(),
    );
    let previous_window = qmp
        .read_memory(0, current_window, 4)
        .map_err(|error| error.to_string())?;

    let mut debugger = l6max_diagnostics::gdb::Gdb::connect(socket)
        .map_err(|error| format!("connecting to emulator GDB: {error}"))?;
    qmp.stop()
        .map_err(|error| format!("stopping QEMU for effect parameter edit: {error}"))?;
    let inject = (|| {
        debugger
            .write(current_window, &l6max::GUI_RECORDER_WINDOW.to_le_bytes())
            .map_err(|error| format!("selecting parameter-edit context: {error}"))?;
        let stop = debugger
            .call_thumb(
                mixer_apply_channel_parameter,
                &[(encoder - 1) as u32, 6, 0, steps as u32, 0],
                Duration::from_secs(3),
            )
            .map_err(|error| format!("calling patched mixer parameter function: {error}"))?;
        let event = u32::from_le_bytes(
            debugger
                .read(module_base + event_offset, 4)
                .map_err(|error| format!("reading queued parameter event: {error}"))?
                .try_into()
                .unwrap(),
        );
        Ok::<_, String>((stop, event))
    })();
    let restore_window = debugger
        .write(current_window, &previous_window)
        .map_err(|error| format!("restoring current window: {error}"));
    let resume = qmp
        .resume()
        .map_err(|error| format!("resuming QEMU after effect parameter edit: {error}"));
    restore_window?;
    resume?;
    let (stop_reply, event_after_hook) = inject?;

    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let applied = u32::from_le_bytes(
            qmp.read_memory(0, edit_counter, 4)
                .map_err(|error| error.to_string())?
                .try_into()
                .unwrap(),
        );
        if applied > before {
            let ring_after = u32::from_le_bytes(
                qmp.read_memory(0, ring_address, 4)
                    .map_err(|error| error.to_string())?
                    .try_into()
                    .unwrap(),
            );
            if ring_after == ring_before {
                return Err(format!(
                    "encoder {encoder} edit left its ring unchanged at Q16.16 position {ring_after}"
                ));
            }
            qmp.stop()
                .map_err(|error| format!("stopping QEMU after effect parameter edit: {error}"))?;
            debugger
                .write(current_window, &previous_window)
                .map_err(|error| format!("restoring current window: {error}"))?;
            qmp.resume().map_err(|error| {
                format!("resuming QEMU after restoring current window: {error}")
            })?;
            return Ok(json!({
                "encoder": encoder,
                "steps": steps,
                "effect_type": effect_type,
                "edit_counter_before": before,
                "edit_counter_after": applied,
                "ring_position_before": ring_before,
                "ring_position_after": ring_after,
                "input": "patched mixer_apply_channel_parameter call",
            }));
        }
        if Instant::now() >= deadline {
            let event = u32::from_le_bytes(
                qmp.read_memory(0, module_base + event_offset, 4)
                    .map_err(|error| error.to_string())?
                    .try_into()
                    .unwrap(),
            );
            let callback_hits = u32::from_le_bytes(
                qmp.read_memory(0, module_base + callback_hits_offset, 4)
                    .map_err(|error| error.to_string())?
                    .try_into()
                    .unwrap(),
            );
            let active_callback = u32::from_le_bytes(
                qmp.read_memory(0, 0x202a_6f20, 4)
                    .map_err(|error| error.to_string())?
                    .try_into()
                    .unwrap(),
            );
            let window = u32::from_le_bytes(
                qmp.read_memory(0, current_window, 4)
                    .map_err(|error| error.to_string())?
                    .try_into()
                    .unwrap(),
            );
            qmp.stop().map_err(|error| {
                format!("stopping QEMU after effect replacement edit timeout: {error}")
            })?;
            debugger
                .write(current_window, &previous_window)
                .map_err(|error| format!("restoring current window after edit timeout: {error}"))?;
            qmp.resume()
                .map_err(|error| format!("resuming QEMU after edit timeout: {error}"))?;
            return Err(format!(
                "effect audio callback did not apply the parameter edit (counter {before}->{applied}, event={event:#010x}, event_after_hook={event_after_hook:#010x}, callbacks={callback_hits}, effect_type={effect_type}, active_callback={active_callback:#010x}, current_window={window:#010x}, module_base={module_base:#010x}, gdb_stop={stop_reply})"
            ));
        }
        thread::sleep(Duration::from_millis(10));
    }
}

pub(super) fn effect_replacement_state(
    options: &Options,
    session: &Session,
    expected_effect_type: Option<u8>,
    expected_processor_type: Option<u8>,
    expected_panel_mode: Option<u8>,
    expected_edit_mode: Option<bool>,
    minimum_parameter_edits: Option<u32>,
    expected_parameter_id: Option<u8>,
    expected_parameter_value: Option<f32>,
    expected_dialog_active: Option<bool>,
    minimum_exit_ring_refreshes: Option<u32>,
    expected_effect: Option<&str>,
    require_output: bool,
) -> Result<Value, String> {
    if session.engine.model() != l6fw::Model::L6max {
        return Err("effect replacement state checks require L6max".into());
    }
    let root = sd_fixture_root(options)?;
    let report: Value = serde_json::from_slice(
        &fs::read(
            root.join(crate::fixture_layout::SD_BUNDLE_BUILD)
                .join(crate::fixture_layout::PATCH_REPORT),
        )
        .map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;
    let symbols = composed_module_report(&report)
        .and_then(|patch| patch["exports"].as_object())
        .ok_or("effect replacement debug symbols are missing from its module report")?;
    let offset = |name: &str| -> Result<u32, String> {
        symbols
            .get(name)
            .ok_or_else(|| format!("effect replacement symbol {name} is missing"))?
            .as_u64()
            .and_then(|value| u32::try_from(value).ok())
            .ok_or_else(|| format!("effect replacement symbol {name} is missing"))
    };
    let qmp = &session.engine.management[0];
    let registry = u32::from_le_bytes(
        qmp.read_memory(0, patch_abi::registry::L6MAX_POINTER as u32, 4)
            .map_err(|error| error.to_string())?
            .try_into()
            .unwrap(),
    );
    let module_base = u32::from_le_bytes(
        qmp.read_memory(0, registry + patch_abi::registry::MODULE_PTR_OFFSET, 4)
            .map_err(|error| error.to_string())?
            .try_into()
            .unwrap(),
    );
    if module_base == 0 {
        return Err("effect replacement module is not loaded".into());
    }
    let effect_type = qmp
        .read_memory(0, l6max::mixer_efx_type, 1)
        .map_err(|error| error.to_string())?
        .first()
        .copied();
    let edit_mode = u32::from_le_bytes(
        qmp.read_memory(
            0,
            module_base + offset("DUAL_EFFECT_PARAMETER_EDIT_MODE")?,
            4,
        )
        .map_err(|error| error.to_string())?
        .try_into()
        .unwrap(),
    );
    let processor_type = u32::from_le_bytes(
        qmp.read_memory(0, module_base + offset("DUAL_EFFECT_PROCESSOR_TYPE")?, 4)
            .map_err(|error| error.to_string())?
            .try_into()
            .unwrap(),
    );
    let panel_mode = u32::from_le_bytes(
        qmp.read_memory(0, module_base + offset("DUAL_EFFECT_PANEL_MODE")?, 4)
            .map_err(|error| error.to_string())?
            .try_into()
            .unwrap(),
    );
    let exit_ring_refreshes = if minimum_exit_ring_refreshes.is_some() {
        u32::from_le_bytes(
            qmp.read_memory(
                0,
                module_base + offset("DUAL_EFFECT_EXIT_RING_REFRESH_HITS")?,
                4,
            )
            .map_err(|error| error.to_string())?
            .try_into()
            .unwrap(),
        )
    } else {
        0
    };
    let applied = if minimum_parameter_edits.is_some() {
        u32::from_le_bytes(
            qmp.read_memory(0, module_base + offset("DUAL_EFFECT_EDIT_APPLIED")?, 4)
                .map_err(|error| error.to_string())?
                .try_into()
                .unwrap(),
        )
    } else {
        0
    };
    let check_dialog = expected_dialog_active.is_some()
        || (expected_edit_mode == Some(true) && minimum_parameter_edits.is_some());
    let dialog_active = if check_dialog {
        if let Some(dialog_active_offset) =
            composed_export_offset(&report, "KNOB_DIALOG_EXTERNAL_TEXT_ACTIVE")
        {
            qmp.read_memory(0, module_base + dialog_active_offset, 1)
                .map_err(|error| error.to_string())?
                .first()
                .copied()
                .ok_or("Knob Dialog active-state byte is missing")?
        } else if expected_dialog_active == Some(false) {
            0
        } else {
            return Err(
                "Knob Dialog active-state symbol is missing from the composed module report".into(),
            );
        }
    } else {
        0
    };
    let parameter = if let Some(parameter_id) = expected_parameter_id {
        if parameter_id >= 16 {
            return Err(format!(
                "effect replacement parameter id {parameter_id} is out of range"
            ));
        }
        let handoff_offset = offset("PARAMETER_HANDOFF")? + u32::from(parameter_id) * 4;
        Some(f32::from_bits(u32::from_le_bytes(
            qmp.read_memory(0, module_base + handoff_offset, 4)
                .map_err(|error| error.to_string())?
                .try_into()
                .unwrap(),
        )))
    } else {
        None
    };
    if expected_effect_type.is_some_and(|expected| effect_type != Some(expected)) {
        return Err(format!(
            "effect replacement selected effect is {effect_type:?}, expected {expected_effect_type:?}"
        ));
    }
    if expected_processor_type.is_some_and(|expected| processor_type != u32::from(expected)) {
        return Err(format!(
            "effect replacement replacement processor has type {processor_type}, expected {expected_processor_type:?}"
        ));
    }
    if expected_panel_mode.is_some_and(|expected| panel_mode != u32::from(expected)) {
        return Err(format!(
            "effect replacement selected panel mode is {panel_mode}, expected {expected_panel_mode:?}"
        ));
    }
    if expected_edit_mode.is_some_and(|expected| edit_mode != u32::from(expected)) {
        return Err(format!(
            "effect replacement parameter edit mode is {edit_mode}, expected {expected_edit_mode:?}"
        ));
    }
    if minimum_exit_ring_refreshes.is_some_and(|minimum| exit_ring_refreshes < minimum) {
        return Err(format!(
            "effect replacement stock exit refresh called the panel ring routine {exit_ring_refreshes} times, expected at least {minimum_exit_ring_refreshes:?}"
        ));
    }
    if expected_dialog_active.is_some_and(|expected| dialog_active != u8::from(expected)) {
        return Err(format!(
            "Knob Dialog active state is {dialog_active}, expected {expected_dialog_active:?}"
        ));
    }
    if minimum_parameter_edits.is_some_and(|minimum| applied < minimum) {
        return Err(format!(
            "effect replacement applied {applied} parameter edits, expected at least {minimum_parameter_edits:?}"
        ));
    }
    if expected_edit_mode == Some(true) && minimum_parameter_edits.is_some() && dialog_active != 1 {
        return Err(format!(
            "effect replacement has applied {applied} parameter edits but Knob Dialog is not active ({dialog_active})"
        ));
    }
    let read_u32 = |address: u32| -> Result<u32, String> {
        Ok(u32::from_le_bytes(
            qmp.read_memory(0, address, 4)
                .map_err(|error| error.to_string())?
                .try_into()
                .unwrap(),
        ))
    };
    let active_effect = if expected_effect.is_some() {
        let pointer = read_u32(module_base + offset("DUAL_EFFECT_ACTIVE_EFFECT")?)?;
        let length = read_u32(module_base + offset("DUAL_EFFECT_ACTIVE_EFFECT_LEN")?)?;
        if pointer == 0 || length == 0 {
            String::new()
        } else if length > 64 {
            return Err(format!(
                "effect replacement active effect ID claims {length} bytes"
            ));
        } else {
            String::from_utf8_lossy(
                &qmp.read_memory(0, pointer, length as usize)
                    .map_err(|error| error.to_string())?,
            )
            .into_owned()
        }
    } else {
        String::new()
    };
    if let Some(expected) = expected_effect
        && active_effect != expected
    {
        return Err(format!(
            "effect replacement runs {active_effect:?} in the selected slot, expected {expected:?}"
        ));
    }
    let output_counter = match processor_type {
        1 => Some("DUAL_EFFECT_ROOM_OUTPUT_NONZERO"),
        2 => Some("DUAL_EFFECT_SPRING_OUTPUT_NONZERO"),
        _ => None,
    };
    let output_blocks = match output_counter {
        Some(name) if require_output => read_u32(module_base + offset(name)?)?,
        _ => 0,
    };
    if require_output {
        if output_counter.is_none() {
            return Err("effect replacement output check needs a replaced slot selected".into());
        }
        if output_blocks == 0 {
            return Err(format!(
                "effect replacement {active_effect:?} has produced no nonzero output in slot {processor_type}"
            ));
        }
    }
    if expected_parameter_value.is_some() && parameter.is_none() {
        return Err("effect replacement parameter value requires expected_parameter_id".into());
    }
    if let (Some(expected), Some(actual)) = (expected_parameter_value, parameter) {
        let tolerance = 0.01_f32.max(expected.abs() * 0.0001);
        if !actual.is_finite() || (actual - expected).abs() > tolerance {
            return Err(format!(
                "effect replacement parameter {expected_parameter_id:?} is {actual}, expected {expected}"
            ));
        }
    }
    Ok(
        json!({"effect_type":effect_type,"processor_type":processor_type,"active_effect":active_effect,"nonzero_output_blocks":output_blocks,"panel_mode":panel_mode,"edit_mode":edit_mode,"parameter_edits_applied":applied,"parameter_id":expected_parameter_id,"parameter_value":parameter,"knob_dialog_active":dialog_active,"exit_ring_refreshes":exit_ring_refreshes}),
    )
}

pub(super) fn sd_fixture_edits(
    options: &Options,
    main: &[u8],
    model: l6fw::Model,
) -> Result<SdEdits, String> {
    use sha2::{Digest, Sha256};
    let root = sd_fixture_root(options)?;
    let stock = fs::read(
        root.join(crate::fixture_layout::SD_STOCK_FIRMWARE)
            .join("main_firmware.bin"),
    )
    .map_err(|e| e.to_string())?;
    let image_base = model.main_base();
    patch_core::verify_firmware_for(model, &stock)?;
    let loader: Value = serde_json::from_slice(
        &fs::read(
            root.join(crate::fixture_layout::LOADER_BUILD)
                .join(crate::fixture_layout::PATCH_REPORT),
        )
        .map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    if loader["patched_sha256"].as_str() != Some(format!("{:x}", Sha256::digest(main)).as_str()) {
        return Err("Patch loader image does not match its generated report".into());
    }
    let patch_output = root.join(crate::fixture_layout::SD_BUNDLE_BUILD);
    let patch_report_path = patch_output.join("patch.json");
    if !patch_report_path.exists() {
        return Ok((
            Vec::new(),
            0,
            Vec::new(),
            None,
            Vec::new(),
            Vec::new(),
            Vec::new(),
        ));
    }
    let patch_report: Value =
        serde_json::from_slice(&fs::read(&patch_report_path).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
    fn parse_module(
        bytes: &[u8],
        stock: &[u8],
        image_base: u32,
    ) -> Result<
        (
            Vec<(u32, Vec<u8>, u32)>,
            Vec<u8>,
            Option<u32>,
            Vec<patch_abi::bundle::Relocation>,
            Vec<patch_abi::dynamic::Hook>,
        ),
        String,
    > {
        let patch_abi::dynamic::Module {
            mut code,
            memory_len,
            hooks,
            relocations,
        } = patch_abi::dynamic::Module::decode(bytes)
            .map_err(|e| format!("invalid SD patch file: {e:?}"))?;
        code.resize(memory_len, 0);
        let module_hooks = hooks.clone();
        let mut edits = Vec::with_capacity(hooks.len());
        for hook in hooks {
            let offset = hook
                .address
                .checked_sub(image_base)
                .ok_or("hook precedes firmware image")? as usize;
            let end = offset.checked_add(4).ok_or("hook range overflow")?;
            let original = stock
                .get(offset..end)
                .ok_or("hook exceeds firmware image")?
                .to_vec();
            edits.push((hook.address, original, hook.entry_offset));
        }
        Ok((edits, code, None, relocations, module_hooks))
    }
    let mut all_edits = Vec::new();
    let mut all_hooks = Vec::new();
    let mut module_images = Vec::new();
    let mut last = None;
    for patch in patch_report["patches"]
        .as_array()
        .ok_or("dynamic report has no patch list")?
    {
        let filename = patch["file"]
            .as_str()
            .and_then(|file| std::path::Path::new(file).file_name())
            .ok_or("dynamic patch report has no module filename")?;
        let bytes =
            fs::read(patch_output.join("PATCHES").join(filename)).map_err(|e| e.to_string())?;
        let (edits, payload, init, relocations, hooks) = parse_module(&bytes, &stock, image_base)?;
        all_edits.extend(edits);
        all_hooks.extend(hooks.iter().copied());
        module_images.push((payload.clone(), relocations.clone(), hooks));
        last = Some((payload, init, relocations));
    }
    let (payload, init, relocations) = last.ok_or("dynamic report contains no modules")?;
    Ok((
        all_edits,
        0,
        payload,
        init,
        relocations,
        all_hooks,
        module_images,
    ))
}

/// The Patch loader's heap-allocated registry, read through its pointer word.
pub(super) fn sd_registry(
    qmp: &l6max_host::qmp::QmpClient,
    model: l6fw::Model,
) -> Result<u32, Box<dyn std::error::Error>> {
    sd_registry_if_allocated(qmp, model)?
        .ok_or_else(|| "the Patch loader has not allocated its registry".into())
}

/// The registry, or `None` while the root word still holds its stock value.
pub(super) fn sd_registry_if_allocated(
    qmp: &l6max_host::qmp::QmpClient,
    model: l6fw::Model,
) -> Result<Option<u32>, Box<dyn std::error::Error>> {
    use patch_abi::registry;
    let (pointer, stock) = match model {
        l6fw::Model::L6 => (registry::L6_POINTER, registry::L6_POINTER_STOCK),
        l6fw::Model::L6max => (registry::L6MAX_POINTER, registry::L6MAX_POINTER_STOCK),
    };
    let base = u32::from_le_bytes(qmp.read_memory(0, pointer, 4)?.try_into().unwrap());
    if base == stock {
        return Ok(None);
    }
    let magic = u32::from_le_bytes(qmp.read_memory(0, base, 4)?.try_into().unwrap());
    if magic != registry::MAGIC {
        return Err(
            format!("Patch loader registry at {base:#010x} has magic {magic:#010x}").into(),
        );
    }
    Ok(Some(base))
}

pub(super) fn sd_dynamic_handler_slot(
    options: &Options,
    _model: l6fw::Model,
    registry: u32,
    function_address: u32,
) -> Result<u32, String> {
    let root = sd_fixture_root(options)?;
    let report: Value = serde_json::from_slice(
        &fs::read(
            root.join(crate::fixture_layout::LOADER_BUILD)
                .join(crate::fixture_layout::PATCH_REPORT),
        )
        .map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    let sites = report["patches"]
        .as_array()
        .and_then(|patches| {
            patches
                .iter()
                .find(|patch| patch["name"] == "dynamic-dispatch")
        })
        .and_then(|patch| patch["sites"].as_array())
        .ok_or("Patch loader report has no dynamic dispatch site table")?;
    let index = sites
        .iter()
        .position(|site| site["address"].as_u64() == Some(u64::from(function_address)))
        .ok_or_else(|| format!("function {function_address:#010x} has no SD dispatch slot"))?;
    let index = u32::try_from(index).map_err(|e| e.to_string())?;
    registry
        .checked_add(patch_abi::registry::site_offset(index))
        .ok_or_else(|| "dynamic handler slot address overflow".into())
}

/// A dispatch site's registry record, at the record's address, and the links
/// it chains, read from guest memory.
pub(super) fn sd_site_chain(
    qmp: &l6max_host::qmp::QmpClient,
    record: u32,
) -> Result<
    (
        patch_abi::dynamic::SiteChain,
        Vec<patch_abi::dynamic::HookLink>,
    ),
    Box<dyn std::error::Error>,
> {
    let words = |address: u32, count: usize| -> Result<Vec<u32>, Box<dyn std::error::Error>> {
        Ok(qmp
            .read_memory(0, address, count * 4)?
            .chunks_exact(4)
            .map(|word| u32::from_le_bytes(word.try_into().unwrap()))
            .collect())
    };
    let value = words(record, 4)?;
    let chain = patch_abi::dynamic::SiteChain {
        handler: value[0],
        first: value[1],
        last: value[2],
        count: value[3],
    };
    let mut links = Vec::new();
    let mut next = chain.first;
    while next != 0 {
        if links.len() >= chain.count as usize {
            return Err(format!(
                "site record {record:#010x} chains more than {} links",
                chain.count
            )
            .into());
        }
        let value = words(next, 6)?;
        links.push(patch_abi::dynamic::HookLink {
            entry: value[0],
            next_slot: value[1],
            order: value[2],
            module_base: value[3],
            next: value[4],
            site: value[5],
        });
        next = value[4];
    }
    if links.len() != chain.count as usize
        || links.last().is_some_and(|_| chain.last == 0)
        || (chain.count != 0 && chain.handler != links[0].entry)
    {
        return Err(format!(
            "site record {record:#010x} is inconsistent: {chain:?}, {} links",
            links.len()
        )
        .into());
    }
    Ok((chain, links))
}
