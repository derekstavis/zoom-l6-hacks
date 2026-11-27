//! SD patch loading: fixture edits, the loader registry and effect replacement.
use super::*;

/// Verify the generated per-patch module before inspecting its live registry.
pub(super) type SdEdits = (
    Vec<(u32, Vec<u8>, u32)>,
    u32,
    Vec<u8>,
    Option<u32>,
    Vec<patch_abi::relocation::Relocation>,
    Vec<patch_abi::dynamic::Hook>,
    Vec<(
        Vec<u8>,
        Vec<patch_abi::relocation::Relocation>,
        Vec<patch_abi::dynamic::Hook>,
    )>,
);

pub(super) struct SdInspectionContext {
    pub fixture: FrozenSdFixture,
    pub snapshot: Option<(bool, l6ctl::SdLoaderSnapshot)>,
    pub allocation_checked: bool,
    pub chains: Option<SdChainEvidence>,
    pub continuation_modules: Option<Vec<usize>>,
    pub module_images_checked: bool,
    pub wall_deadline: Instant,
    pub guest_start: u32,
    pub guest_timeout_ms: u32,
    pub wall_timeout_s: u64,
}

pub(super) struct SdChainEvidence {
    pub expected_function: Option<u32>,
    pub count: u32,
    pub links: Vec<(u32, u32, u32, u32)>,
    pub expected_count: Option<u8>,
}

fn mutable_module_ranges(report: &Value, size: usize) -> Vec<(usize, usize)> {
    let patch = report["patches"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|patch| patch["memory_length"].as_u64() == Some(size as u64));
    let mut ranges = patch
        .and_then(|patch| patch["writable_data"].as_object())
        .and_then(|data| {
            Some((
                usize::try_from(data["offset"].as_u64()?).ok()?,
                usize::try_from(data["length"].as_u64()?).ok()?,
            ))
        })
        .filter(|(_, length)| *length > 0)
        .into_iter()
        .collect::<Vec<_>>();
    if let Some(code_len) = patch
        .and_then(|patch| patch["payload_length"].as_u64())
        .and_then(|length| usize::try_from(length).ok())
        && code_len < size
    {
        // The loader zeroes this NOBITS tail before module initialization.
        // It is writable module state after startup, not immutable code.
        ranges.push((code_len, size - code_len));
    }
    ranges
}

/// Check the active hook-chain module images against the frozen fixture.
pub(super) fn verify_module_images(
    session: &Session,
    context: &SdInspectionContext,
    loaded: bool,
) -> Result<Value, String> {
    if !context.allocation_checked {
        return Err("SD allocation assertion missing".into());
    }
    let (captured_loaded, snapshot) = context
        .snapshot
        .as_ref()
        .ok_or("SD module snapshot missing")?;
    if *captured_loaded != loaded {
        return Err("SD module-image assertion does not match captured state".into());
    }
    let chains = context
        .chains
        .as_ref()
        .ok_or("SD chain assertion missing")?;
    let chain_links = &chains.links;
    let continuation_modules = context
        .continuation_modules
        .as_ref()
        .ok_or("SD continuation assertion missing")?;
    let (edits, link_base, payload, _, _, _, module_images) = &context.fixture.edits;
    if snapshot.registry.is_none() {
        return Ok(json!({"loaded":false,"registry":null,"guarded_hooks":edits.len()}));
    }
    if !loaded {
        return Ok(
            json!({"loaded":false,"guarded_hooks":edits.len(),"module_bytes":0,"hook_chain_count":chains.count,"hook_chain_orders":chains.links.iter().map(|link|link.2).collect::<Vec<_>>()}),
        );
    }
    let patch_report = context
        .fixture
        .patch_report
        .as_ref()
        .ok_or("dynamic report missing")?;
    let ordered_chain = context.fixture.chain_first.is_some();
    let control = session.control();
    let reader = control
        .sd_loader_reader(context.wall_deadline)
        .map_err(|error| error.to_string())?;
    let read_live = |address: u32, length: usize| -> Result<Vec<u8>, String> {
        reader
            .read_chunk_cancellable(address, length, context.wall_deadline, || {
                guest_now(session).wrapping_sub(context.guest_start) >= context.guest_timeout_ms
            })
            .map_err(|error| error.to_string())
    };
    for (index, (entry, _, _, module)) in chain_links.iter().enumerate() {
        let module_index = *continuation_modules
            .get(index)
            .ok_or("SD continuation evidence missing")?;
        let (module_payload, relocations, hooks) = module_images
            .get(module_index)
            .ok_or("SD continuation module missing")?;
        let mutable_ranges = mutable_module_ranges(patch_report, module_payload.len());
        let mut expected = module_payload.clone();
        if ordered_chain && index == 0 {
            let file = context
                .fixture
                .chain_first
                .as_ref()
                .ok_or("ordered-chain fixture missing")?;
            let start = patch_abi::dynamic::HEADER_LEN_V1;
            expected = file
                .get(start..start + expected.len())
                .ok_or("ordered-chain fixture is shorter than its module")?
                .to_vec();
        }
        patch_abi::relocation::relocate(
            &mut expected,
            relocations.clone().into_iter(),
            *link_base,
            *module,
        )
        .map_err(|error| format!("invalid fixture relocations: {error:?}"))?;
        for hook in hooks {
            if hook.next_slot_offset == u32::MAX {
                continue;
            }
            let slot = *module + hook.next_slot_offset;
            let target = read_live(slot, 4)?;
            let offset = hook.next_slot_offset as usize;
            expected[offset..offset + 4].copy_from_slice(&target);
        }
        for (chunk_index, chunk) in expected.chunks(4096).enumerate() {
            let address = *module + (chunk_index * 4096) as u32;
            let actual = read_live(address, chunk.len())?;
            if let Some((offset, (actual_byte, expected_byte))) = actual
                .iter()
                .zip(chunk)
                .enumerate()
                .find(|(index, (actual, expected))| {
                    let module_offset = chunk_index * 4096 + index;
                    actual != expected
                        && !mutable_ranges.iter().any(|(start, length)| {
                            module_offset >= *start && module_offset < *start + *length
                        })
                })
            {
                return Err(format!(
                    "allocated SD module {index} differs from its relocated bundle at {:#010x} (expected {:#04x}, got {:#04x})",
                    address + offset as u32,
                    expected_byte,
                    actual_byte,
                ));
            }
        }
        if (*entry & !1) < *module {
            return Err(format!("hook {index} entry precedes its module"));
        }
    }
    Ok(
        json!({"loaded":loaded,"guarded_hooks":edits.len(),"module_bytes":payload.len(),"hook_chain_count":chains.count,"hook_chain_orders":chain_links.iter().map(|link|link.2).collect::<Vec<_>>()}),
    )
}

/// Check the separately allocated latest module image against the same frozen
/// fixture and deadline used by the hook-chain image check.
pub(super) fn verify_latest_module(
    session: &Session,
    context: &SdInspectionContext,
    loaded: bool,
) -> Result<Value, String> {
    let (captured_loaded, snapshot) = context
        .snapshot
        .as_ref()
        .ok_or("SD module snapshot missing")?;
    if *captured_loaded != loaded {
        return Err("latest SD allocation assertion does not match captured state".into());
    }
    if !context.module_images_checked {
        return Err("SD module-image assertion missing".into());
    }
    if !loaded || snapshot.registry.is_none() {
        return Ok(json!({"loaded":false,"latest_allocation":null}));
    }
    let loaded_base = snapshot.module_base.ok_or("SD module base missing")?;
    let loaded_len = snapshot.module_len.ok_or("SD module length missing")?;
    let (_, link_base, _, _, _, _, module_images) = &context.fixture.edits;
    let patch_report = context
        .fixture
        .patch_report
        .as_ref()
        .ok_or("dynamic report missing")?;
    let control = session.control();
    let reader = control
        .sd_loader_reader(context.wall_deadline)
        .map_err(|error| error.to_string())?;
    let read_live = |address: u32, length: usize| -> Result<Vec<u8>, String> {
        reader
            .read_chunk_cancellable(address, length, context.wall_deadline, || {
                guest_now(session).wrapping_sub(context.guest_start) >= context.guest_timeout_ms
            })
            .map_err(|error| error.to_string())
    };
    let Some((payload, relocations, hooks)) = module_images
        .iter()
        .find(|(payload, _, _)| payload.len() == loaded_len as usize)
    else {
        return Ok(json!({"loaded":true,"latest_allocation":null}));
    };
    let mutable_ranges = mutable_module_ranges(patch_report, payload.len());
    let mut expected = payload.clone();
    patch_abi::relocation::relocate(
        &mut expected,
        relocations.clone().into_iter(),
        *link_base,
        loaded_base,
    )
    .map_err(|error| format!("invalid latest module relocations: {error:?}"))?;
    for hook in hooks {
        if hook.next_slot_offset == u32::MAX {
            continue;
        }
        let slot = loaded_base + hook.next_slot_offset;
        let target = read_live(slot, 4)?;
        let offset = hook.next_slot_offset as usize;
        expected[offset..offset + 4].copy_from_slice(&target);
    }
    for (chunk_index, chunk) in expected.chunks(4096).enumerate() {
        let address = loaded_base + (chunk_index * 4096) as u32;
        let actual = read_live(address, chunk.len())?;
        if let Some((offset, (actual_byte, expected_byte))) = actual
            .iter()
            .zip(chunk)
            .enumerate()
            .find(|(index, (actual, expected))| {
                let module_offset = chunk_index * 4096 + index;
                actual != expected
                    && !mutable_ranges.iter().any(|(start, length)| {
                        module_offset >= *start && module_offset < *start + *length
                    })
            })
        {
            return Err(format!(
                "latest allocated SD module at {loaded_base:#010x} differs from its relocated bundle at {:#010x} (expected {:#04x}, got {:#04x})",
                address + offset as u32,
                expected_byte,
                actual_byte,
            ));
        }
    }
    Ok(json!({"loaded":true,"latest_allocation":loaded_base,"module_bytes":loaded_len}))
}

/// Validate the captured allocation and claim against the frozen module sizes.
pub(super) fn check_sd_allocation(
    snapshot: &l6ctl::SdLoaderSnapshot,
    loaded: bool,
    module_sizes: impl Iterator<Item = usize>,
) -> Result<Value, String> {
    if snapshot.registry.is_none() {
        if loaded {
            return Err("the Patch loader has not allocated its registry".into());
        }
        return Ok(json!({"loaded":false,"registry":null}));
    }
    let base = snapshot.module_base.ok_or("SD module base missing")?;
    let len = snapshot.module_len.ok_or("SD module length missing")?;
    let known_size = module_sizes.into_iter().any(|size| size == len as usize);
    if loaded != (base != 0) || (loaded && !known_size) {
        let stage = snapshot.stage.unwrap_or_default();
        return Err(format!(
            "SD allocation state loaded={loaded}, base={base:#010x}, len={len}, loader stage={stage:#x}"
        ));
    }
    let claim = snapshot.claim.ok_or("SD claim missing")?;
    if (claim == 1) != loaded {
        let stage = snapshot.stage.unwrap_or_default();
        return Err(format!(
            "SD module loaded={loaded}, claim={claim:#x}, loader stage={stage:#x}"
        ));
    }
    Ok(json!({"loaded":loaded,"module_base":base,"module_len":len,"claim":claim}))
}

/// Compare the pinned loader image's persistent stubs with running MAIN.
pub(super) fn verify_sd_loader_hooks(
    session: &Session,
    main: &[u8],
    edits: &[(u32, Vec<u8>, u32)],
    deadline: Instant,
    guest_start: u32,
    guest_timeout_ms: u32,
) -> Result<usize, String> {
    let model = session.model();
    let image_base = model.main_base();
    let control = session.control();
    let check = || -> Result<(), String> {
        if Instant::now() >= deadline
            || guest_now(session).wrapping_sub(guest_start) >= guest_timeout_ms
        {
            Err("SD inspection exceeded its shared deadline".into())
        } else {
            Ok(())
        }
    };
    let loader_calls: &[u32] = match model {
        l6fw::Model::L6 => &[0x8000_9a60, 0x8000_9aa0],
        l6fw::Model::L6max => &[
            l6max::sys_main_startup_initialize,
            0x8000_e9fa,
            0x8000_ea24,
            0x8001_0118,
            0x8002_bc7e,
            0x8000_ebac,
        ],
    };
    for &address in loader_calls {
        check()?;
        let offset = address
            .checked_sub(image_base)
            .ok_or("loader address precedes image")? as usize;
        let expected = main
            .get(offset..offset + 4)
            .ok_or("Patch loader image too short")?;
        if control
            .read_main_memory_before(address, 4, deadline)
            .map_err(|e| e.to_string())?
            .bytes
            != expected
        {
            return Err(format!("persistent loader hook missing at {address:#010x}"));
        }
    }
    for (address, original, _) in edits {
        check()?;
        let offset = address
            .checked_sub(image_base)
            .ok_or("dispatch address precedes image")? as usize;
        let expected = main
            .get(offset..offset + original.len())
            .ok_or("Patch loader image too short")?;
        if control
            .read_main_memory_before(*address, expected.len(), deadline)
            .map_err(|e| e.to_string())?
            .bytes
            != expected
        {
            return Err(format!(
                "persistent SD dispatch stub missing at {address:#010x}"
            ));
        }
    }
    check()?;
    Ok(edits.len())
}
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

pub(super) struct EffectSlotScope {
    pub reader: l6max_host::effect_editor::SlotReader,
    pub start: Instant,
    pub guest_start: u32,
    pub guest_budget: u32,
    pub wall_budget: u64,
}

fn effect_slot_ready(
    snapshot: &l6ctl::EffectSlotSnapshot,
    selected: u8,
    processor: u8,
    expected_effect: Option<&str>,
    baseline: Option<u32>,
    require_output: bool,
) -> bool {
    snapshot.effect_type == selected
        && snapshot.processor_type == u32::from(processor)
        && expected_effect.is_none_or(|expected| {
            snapshot.active_effect_bytes.as_deref() == Some(expected.as_bytes())
        })
        && (!require_output
            || snapshot
                .output_blocks
                .is_some_and(|value| value != 0 && Some(value) != baseline))
}

pub(super) fn wait_effect_slot(
    options: &Options,
    session: &Session,
    expected_effect_type: Option<u8>,
    expected_processor_type: Option<u8>,
    expected_effect: Option<&str>,
    require_output: bool,
    guest_timeout_ms: u32,
    wall_timeout_s: u64,
    outer_start: (Instant, u32),
) -> Result<l6max_host::effect_editor::SlotReader, String> {
    const GUEST_TIMEOUT_MS: u32 = 10_000;
    const WALL_TIMEOUT_S: u64 = 180;
    const POLL_INTERVAL_MS: u64 = 10;
    if session.model() != l6fw::Model::L6max {
        return Err("effect slot readiness requires L6max".into());
    }
    let selected =
        expected_effect_type.ok_or("effect slot readiness needs expected_effect_type")?;
    let processor =
        expected_processor_type.ok_or("effect slot readiness needs expected_processor_type")?;
    if require_output && !matches!(processor, 1 | 2) {
        return Err("output requires a replacement processor".into());
    }
    let outer_deadline = outer_start.0 + Duration::from_secs(wall_timeout_s);
    let outer_check = || {
        if Instant::now() >= outer_deadline
            || guest_now(session).wrapping_sub(outer_start.1) > guest_timeout_ms
        {
            Err("effect slot exceeded its shared deadline".to_owned())
        } else {
            Ok(())
        }
    };
    let reader = effect_slot_reader(options, session, outer_deadline, &outer_check)?;
    let output_fields = l6ctl::EffectSlotFields {
        output: require_output,
        output_processor: Some(processor),
        ..Default::default()
    };
    let baseline = if require_output {
        reader
            .snapshot_before_checked(&output_fields, outer_deadline, &outer_check)?
            .output_blocks
    } else {
        None
    };
    let start = guest_now(session);
    let deadline = (Instant::now() + Duration::from_secs(wall_timeout_s.min(WALL_TIMEOUT_S)))
        .min(outer_deadline);
    let check = || {
        outer_check()?;
        if Instant::now() >= deadline
            || guest_now(session).wrapping_sub(start) >= guest_timeout_ms.min(GUEST_TIMEOUT_MS)
        {
            Err("effect slot readiness deadline exceeded".to_owned())
        } else {
            Ok(())
        }
    };
    let fields = l6ctl::EffectSlotFields {
        active_effect: expected_effect.is_some(),
        ..output_fields
    };
    loop {
        check()?;
        session.poll()?;
        let snapshot = reader.snapshot_before_checked(&fields, deadline, &check)?;
        let active_matches = expected_effect.is_none_or(|expected| {
            snapshot.active_effect_bytes.as_deref() == Some(expected.as_bytes())
        });
        let output_ready = !require_output
            || snapshot
                .output_blocks
                .is_some_and(|value| value != 0 && Some(value) != baseline);
        if effect_slot_ready(
            &snapshot,
            selected,
            processor,
            expected_effect,
            baseline,
            require_output,
        ) {
            return Ok(reader);
        }
        check().map_err(|error| format!("{error}: selected={}, processor={}, active_matches={active_matches}, output_ready={output_ready}", snapshot.effect_type, snapshot.processor_type))?;
        thread::sleep(Duration::from_millis(POLL_INTERVAL_MS));
    }
}

/// Wait for the loaded effect module to publish the requested editor state.
pub(super) fn wait_effect_mode(
    options: &Options,
    session: &Session,
    panel_mode: Option<u8>,
    edit_mode: Option<bool>,
    guest_timeout_ms: u32,
    wall_timeout_s: u64,
) -> Result<(), String> {
    const GUEST_TIMEOUT_MS: u32 = 10_000;
    const WALL_TIMEOUT_S: u64 = 180;
    const POLL_INTERVAL_MS: u64 = 10;
    // Bind the live module to the frozen fixture before polling, as other
    // effect-slot observations do. Keep the wait's own clocks anchored after
    // setup, matching the original readiness condition.
    let setup_deadline = Instant::now() + Duration::from_secs(wall_timeout_s.min(WALL_TIMEOUT_S));
    let reader = effect_slot_reader(options, session, setup_deadline, &|| Ok(()))?;
    let start = guest_now(session);
    let deadline = Instant::now() + Duration::from_secs(wall_timeout_s.min(WALL_TIMEOUT_S));
    loop {
        session.poll()?;
        let state = reader.snapshot_before_checked(
            &l6ctl::EffectSlotFields::default(),
            deadline,
            &|| Ok(()),
        )?;
        let panel = state.panel_mode;
        let edit = state.edit_mode;
        if panel_mode.is_none_or(|v| panel == u32::from(v))
            && edit_mode.is_none_or(|v| edit == u32::from(v))
        {
            return Ok(());
        }
        if Instant::now() >= deadline
            || guest_now(session).wrapping_sub(start) >= guest_timeout_ms.min(GUEST_TIMEOUT_MS)
        {
            return Err(format!(
                "effect editor did not reach panel={panel_mode:?}, edit={edit_mode:?}: panel={panel}, edit={edit}"
            ));
        }
        thread::sleep(Duration::from_millis(POLL_INTERVAL_MS));
    }
}

pub(super) struct FrozenSdFixture {
    pub edits: SdEdits,
    pub loader_report: Value,
    pub patch_report: Option<Value>,
    pub chain_first: Option<Vec<u8>>,
}

pub(super) fn sd_fixture_edits(
    options: &Options,
    main: &[u8],
    model: l6fw::Model,
) -> Result<SdEdits, String> {
    Ok(freeze_sd_fixture(options, main, model)?.edits)
}

pub(super) fn freeze_sd_fixture(
    options: &Options,
    main: &[u8],
    model: l6fw::Model,
) -> Result<FrozenSdFixture, String> {
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
    let chain_first = match fs::read(root.join(crate::fixture_layout::CHAIN_FIRST)) {
        Ok(bytes) => Some(bytes),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error.to_string()),
    };
    if !patch_report_path.exists() {
        return Ok(FrozenSdFixture {
            edits: (
                Vec::new(),
                0,
                Vec::new(),
                None,
                Vec::new(),
                Vec::new(),
                Vec::new(),
            ),
            loader_report: loader,
            patch_report: None,
            chain_first,
        });
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
            Vec<patch_abi::relocation::Relocation>,
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
    Ok(FrozenSdFixture {
        edits: (
            all_edits,
            0,
            payload,
            init,
            relocations,
            all_hooks,
            module_images,
        ),
        loader_report: loader,
        patch_report: Some(patch_report),
        chain_first,
    })
}

/// The Patch loader's heap-allocated registry, read through its pointer word.
/// A successful claim is published after scanning and installing SD hooks.
pub(super) fn wait_sd_patch_loaded(
    session: &Session,
    guest_timeout_ms: u32,
    wall_timeout_s: u64,
) -> Result<u32, Box<dyn std::error::Error>> {
    wait_sd_patch_result(session, guest_timeout_ms, wall_timeout_s, true)
}

pub(super) fn wait_sd_loader_snapshot(
    session: &Session,
    reader: &l6max_host::sd_loader::Reader<'_>,
    shared_wall: Instant,
    shared_guest: u32,
    shared_guest_budget: u32,
) -> Result<l6max_host::sd_loader::Snapshot, String> {
    let start = guest_now(session);
    let deadline = shared_wall.min(Instant::now() + Duration::from_secs(180));
    let mut last = String::from("registry not allocated");
    loop {
        if Instant::now() >= deadline
            || guest_now(session).wrapping_sub(start) >= 10_000
            || guest_now(session).wrapping_sub(shared_guest) >= shared_guest_budget
        {
            return Err(format!(
                "SD patch loader did not finish within its deadline ({last})"
            ));
        }
        session.poll().map_err(|e| e.to_string())?;
        let state = reader.readiness_cancellable(deadline, || {
            guest_now(session).wrapping_sub(start) >= 10_000
                || guest_now(session).wrapping_sub(shared_guest) >= shared_guest_budget
        })?;
        if Instant::now() >= deadline
            || guest_now(session).wrapping_sub(start) >= 10_000
            || guest_now(session).wrapping_sub(shared_guest) >= shared_guest_budget
        {
            return Err("SD patch loader readiness exceeded its deadline".into());
        }
        if state.registry.is_some() && state.claim == Some(1) {
            return reader.snapshot_cancellable(deadline, || {
                guest_now(session).wrapping_sub(start) >= 10_000
                    || guest_now(session).wrapping_sub(shared_guest) >= shared_guest_budget
            });
        }
        last = format!("stage={:?}, claim={:?}", state.stage, state.claim);
        thread::sleep(Duration::from_millis(10));
    }
}

/// Wait for a completed scan that installed no module. This requires a card
/// mount and a completed scan; an unallocated registry is not sufficient.
pub(super) fn wait_sd_patch_empty(
    session: &Session,
    guest_timeout_ms: u32,
    wall_timeout_s: u64,
) -> Result<u32, Box<dyn std::error::Error>> {
    wait_sd_patch_result(session, guest_timeout_ms, wall_timeout_s, false)
}

fn wait_sd_patch_result(
    session: &Session,
    guest_timeout_ms: u32,
    wall_timeout_s: u64,
    loaded: bool,
) -> Result<u32, Box<dyn std::error::Error>> {
    const GUEST_TIMEOUT_MS: u32 = 10_000;
    const WALL_TIMEOUT_S: u64 = 180;
    const POLL_INTERVAL_MS: u64 = 10;
    let start = guest_now(session);
    let deadline = Instant::now() + Duration::from_secs(wall_timeout_s.min(WALL_TIMEOUT_S));
    let control = session.control();
    let reader = control.sd_loader_reader(deadline)?;
    let mut last_state = String::from("registry not allocated");
    loop {
        session.poll()?;
        let state = reader.readiness_cancellable(deadline, || false)?;
        if let (Some(base), Some(claim), Some(stage)) = (state.registry, state.claim, state.stage) {
            if (loaded && claim == 1) || (!loaded && claim == 0 && stage == 0x400) {
                return Ok(base);
            }
            last_state = format!("stage={stage:#x}, claim={claim:#x}");
        }
        if Instant::now() >= deadline
            || guest_now(session).wrapping_sub(start) >= guest_timeout_ms.min(GUEST_TIMEOUT_MS)
        {
            return Err(format!(
                "SD patch loader did not finish within its deadline ({last_state})"
            )
            .into());
        }
        thread::sleep(Duration::from_millis(POLL_INTERVAL_MS));
    }
}

pub(super) fn sd_frozen_handler_slot(
    report: &Value,
    registry: u32,
    function_address: u32,
) -> Result<u32, String> {
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
/// Use the guarded host interpretation while retaining the existing assertion types.
pub(super) fn sd_typed_site_chain(
    reader: &l6max_host::sd_loader::Reader<'_>,
    session: &Session,
    registry: u32,
    record: u32,
    deadline: Instant,
    guest_start: u32,
    guest_budget: u32,
) -> Result<
    (
        patch_abi::dynamic::SiteChain,
        Vec<patch_abi::dynamic::HookLink>,
    ),
    String,
> {
    let offset = record
        .checked_sub(registry)
        .and_then(|n| n.checked_sub(patch_abi::registry::SITES_OFFSET))
        .ok_or("SD site record precedes registry")?;
    if offset % patch_abi::registry::SITE_RECORD_LEN != 0 {
        return Err("misaligned SD site record".into());
    }
    let site = reader.site_cancellable(
        offset / patch_abi::registry::SITE_RECORD_LEN,
        deadline,
        || guest_now(session).wrapping_sub(guest_start) >= guest_budget,
    )?;
    if site.registry != registry || site.record != record {
        return Err("SD registry changed during inspection".into());
    }
    let chain = patch_abi::dynamic::SiteChain {
        handler: site.handler,
        first: site.first,
        last: site.last,
        count: site.count,
    };
    let links = site
        .links
        .into_iter()
        .map(|v| patch_abi::dynamic::HookLink {
            entry: v.entry,
            next_slot: v.next_slot,
            order: v.order,
            module_base: v.module_base,
            next: v.next,
            site: v.site,
        })
        .collect();
    Ok((chain, links))
}

/// Bind effect-slot observations to the frozen composed module and loader fixture.
pub(super) fn effect_slot_reader(
    options: &Options,
    session: &Session,
    deadline: Instant,
    check: &dyn Fn() -> Result<(), String>,
) -> Result<l6max_host::effect_editor::SlotReader, String> {
    check()?;
    let main = selected_main_image(options)?;
    sd_fixture_edits(options, &main, session.model())?;
    check()?;
    let output = sd_fixture_root(options)?.join(crate::fixture_layout::SD_BUNDLE_BUILD);
    let report_bytes =
        fs::read(output.join(crate::fixture_layout::PATCH_REPORT)).map_err(|e| e.to_string())?;
    let report: Value = serde_json::from_slice(&report_bytes).map_err(|e| e.to_string())?;
    let filename = report["patches"]
        .as_array()
        .and_then(|patches| (patches.len() == 1).then_some(&patches[0]))
        .and_then(|patch| patch["file"].as_str())
        .and_then(|name| Path::new(name).file_name())
        .ok_or("effect module file missing")?;
    let bytes = fs::read(output.join("PATCHES").join(filename)).map_err(|e| e.to_string())?;
    check()?;
    let binding = l6ctl::EffectSlotBinding::from_fixture(&report_bytes, &bytes)?;
    check()?;
    session
        .control()
        .effect_slot_reader_before_checked(binding, deadline, check)
        .map_err(|e| e.to_string())
}

/// Fixture-scoped control of the effect editor's single-slot mailbox.
pub(super) fn effect_editor_reader(
    options: &Options,
    session: &Session,
    deadline: Instant,
    check: &dyn Fn() -> Result<(), String>,
) -> Result<l6max_host::effect_editor::Reader, String> {
    let main = selected_main_image(options)?;
    sd_fixture_edits(options, &main, session.model())?;
    let root = sd_fixture_root(options)?;
    let output = root.join(crate::fixture_layout::SD_BUNDLE_BUILD);
    let report_bytes =
        fs::read(output.join(crate::fixture_layout::PATCH_REPORT)).map_err(|e| e.to_string())?;
    let report: Value = serde_json::from_slice(&report_bytes).map_err(|e| e.to_string())?;
    let patches = report["patches"]
        .as_array()
        .ok_or("effect module report missing")?;
    if patches.len() != 1 {
        return Err("paced effect edits require one composed module".into());
    }
    let filename = patches[0]["file"]
        .as_str()
        .and_then(|v| Path::new(v).file_name())
        .ok_or("effect module file missing")?;
    let bytes = fs::read(output.join("PATCHES").join(filename)).map_err(|e| e.to_string())?;
    let binding = l6ctl::EffectEditorBinding::from_fixture(&report_bytes, &bytes)?;
    session
        .control()
        .effect_editor_reader_before_checked(binding, deadline, check)
        .map_err(|e| e.to_string())
}

/// Interpret a non-atomic sample against the counters preceding one input.
fn paced_effect_state(
    before: &l6ctl::EffectEditorSnapshot,
    now: &l6ctl::EffectEditorSnapshot,
) -> Result<bool, String> {
    if now.dropped != before.dropped {
        return Err("effect editor dropped a paced turn".into());
    }
    match now.applied.wrapping_sub(before.applied) {
        0 => Ok(false),
        1 => Ok(now.pending == 0 && now.gate == 0),
        count => Err(format!(
            "effect editor applied {count} turns while awaiting one"
        )),
    }
}

fn paced_acknowledged(state: l6ctl::InputAckState) -> Result<bool, String> {
    match state {
        l6ctl::InputAckState::Applied => Ok(true),
        l6ctl::InputAckState::Pending => Ok(false),
        l6ctl::InputAckState::Rejected => Err("paced effect encoder was rejected".into()),
        l6ctl::InputAckState::Unknown => {
            Err("paced effect encoder acknowledgement unavailable".into())
        }
    }
}

pub(super) fn effect_paced_turn(
    options: &Options,
    session: &Session,
    channel: usize,
    steps: i32,
    guest_timeout_ms: u32,
    wall_timeout_s: u64,
    progress: crate::progress::CaseProgress,
    index: usize,
) -> Result<Value, String> {
    if session.model() != l6fw::Model::L6max
        || !(1..=8).contains(&channel)
        || steps == 0
        || steps.unsigned_abs() > 128
    {
        return Err("invalid paced effect turn".into());
    }
    let start = Instant::now();
    let start_guest = guest_now(session);
    let check = || -> Result<(), String> {
        if start.elapsed() >= Duration::from_secs(wall_timeout_s)
            || guest_now(session).wrapping_sub(start_guest) >= guest_timeout_ms
        {
            Err("paced effect turn exceeded its shared deadline".into())
        } else {
            Ok(())
        }
    };
    check()?;
    let deadline = start
        .checked_add(Duration::from_secs(wall_timeout_s))
        .ok_or("paced effect deadline overflow")?;
    let reader = effect_editor_reader(options, session, deadline, &check)?;
    let control = session.control();
    let total = steps.unsigned_abs();
    let mut prior = None;
    let mut before = loop {
        check()?;
        session.poll()?;
        let now = reader.read_before_checked(deadline, &check)?;
        check()?;
        if now.pending == 0 && now.gate == 0 && prior.as_ref() == Some(&now) {
            break now;
        }
        prior = (now.pending == 0 && now.gate == 0).then_some(now);
        thread::sleep(Duration::from_millis(5));
    };
    progress.emit(crate::progress::CaseEvent::StepProgress {
        index,
        completed: 0,
        total,
    });
    for completed in 1..=total {
        check()?;
        // Gesture polarity is the inverse of the wire-level encoder command.
        let receipt = control
            .encoder(channel as u32, -steps.signum())
            .map_err(|e| e.to_string())?;
        let mut acknowledged = false;
        loop {
            check()?;
            session.poll()?;
            let ack = control
                .input_ack(l6ctl::InputSource::Panel, receipt.sequence)
                .map_err(|e| e.to_string())?
                .state;
            acknowledged |= paced_acknowledged(ack)?;
            let now = reader.read_before_checked(deadline, &check)?;
            check()?;
            let applied = paced_effect_state(&before, &now)?;
            if acknowledged && applied {
                before = now;
                break;
            }
            thread::sleep(Duration::from_millis(5));
        }
        progress.emit(crate::progress::CaseEvent::StepProgress {
            index,
            completed,
            total,
        });
    }
    Ok(
        json!({"channel":channel,"steps":steps,"applied":total,"dropped_counter":before.dropped,"fixture_module_sha256":before.fixture_module_sha256}),
    )
}

#[cfg(test)]
mod paced_effect_tests {
    use super::*;
    fn sample(applied: u32, dropped: u32, pending: u32) -> l6ctl::EffectEditorSnapshot {
        l6ctl::EffectEditorSnapshot {
            revision_guard: "test".into(),
            fixture_module_sha256: "test".into(),
            atomic: false,
            applied,
            dropped,
            pending,
            gate: 0,
        }
    }
    #[test]
    fn acknowledgement_fails_closed() {
        assert!(!paced_acknowledged(l6ctl::InputAckState::Pending).unwrap());
        assert!(paced_acknowledged(l6ctl::InputAckState::Applied).unwrap());
        assert!(paced_acknowledged(l6ctl::InputAckState::Rejected).is_err());
        assert!(paced_acknowledged(l6ctl::InputAckState::Unknown).is_err());
    }
    #[test]
    fn requires_one_consumed_edit_and_rejects_drops_or_jumps_even_on_wrap() {
        let before = sample(u32::MAX, 9, 0);
        assert!(!paced_effect_state(&before, &sample(u32::MAX, 9, 0)).unwrap());
        assert!(!paced_effect_state(&before, &sample(0, 9, 1)).unwrap());
        let mut busy = sample(0, 9, 0);
        busy.gate = 1;
        assert!(!paced_effect_state(&before, &busy).unwrap());
        assert!(paced_effect_state(&before, &sample(0, 9, 0)).unwrap());
        assert!(paced_effect_state(&before, &sample(1, 9, 0)).is_err());
        assert!(paced_effect_state(&before, &sample(0, 10, 0)).is_err());
    }
}

#[cfg(test)]
mod effect_slot_readiness_tests {
    use super::*;
    #[test]
    fn output_must_be_nonzero_and_different_from_baseline() {
        let mut sample = l6ctl::EffectSlotSnapshot {
            revision_guard: String::new(),
            fixture_module_sha256: String::new(),
            atomic: false,
            effect_type: 1,
            processor_type: 1,
            panel_mode: 0,
            edit_mode: 0,
            applied: None,
            exit_refreshes: None,
            active_effect: Some("velvet".into()),
            active_effect_bytes: Some(b"velvet".to_vec()),
            output_blocks: Some(0),
            parameter_bits: None,
            dialog_active: None,
            ring_position: None,
        };
        assert!(!effect_slot_ready(
            &sample,
            1,
            1,
            Some("velvet"),
            Some(0),
            true
        ));
        sample.output_blocks = Some(42);
        assert!(!effect_slot_ready(
            &sample,
            1,
            1,
            Some("velvet"),
            Some(42),
            true
        ));
        assert!(effect_slot_ready(
            &sample,
            1,
            1,
            Some("velvet"),
            Some(u32::MAX),
            true
        ));
        assert!(!effect_slot_ready(
            &sample,
            2,
            1,
            Some("velvet"),
            Some(0),
            true
        ));
        assert!(!effect_slot_ready(
            &sample,
            1,
            1,
            Some("plateau"),
            Some(0),
            true
        ));
        assert!(effect_slot_ready(
            &sample,
            1,
            1,
            Some("velvet"),
            None,
            false
        ));
    }
}

#[cfg(test)]
mod allocation_tests {
    use super::*;
    fn sample() -> l6ctl::SdLoaderSnapshot {
        l6ctl::SdLoaderSnapshot {
            revision_guard: "test".into(),
            atomic: false,
            registry: Some(0x80200000),
            stage: Some(0x400),
            claim: Some(1),
            module_base: Some(0x80201000),
            module_len: Some(64),
            site_count: Some(1),
        }
    }
    #[test]
    fn distinguishes_absence_invalid_size_and_unclaimed_module() {
        let mut value = sample();
        assert!(check_sd_allocation(&value, true, [64].into_iter()).is_ok());
        assert!(check_sd_allocation(&value, true, [32].into_iter()).is_err());
        value.claim = Some(0);
        assert!(check_sd_allocation(&value, true, [64].into_iter()).is_err());
        value.registry = None;
        assert!(check_sd_allocation(&value, true, [64].into_iter()).is_err());
        assert!(check_sd_allocation(&value, false, [].into_iter()).is_ok());
        value.registry = Some(0x80200000);
        value.module_base = Some(0);
        value.module_len = Some(0);
        assert!(check_sd_allocation(&value, false, [].into_iter()).is_ok());
    }
}
