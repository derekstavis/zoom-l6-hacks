//! Executing operations against a running session.
use super::*;

fn knob_dialog_phase(
    operation: &Operation,
    scope: Option<&KnobModeScope>,
    pending: Option<&PendingInput>,
    next_mode: Option<usize>,
) -> Option<u8> {
    let fixed = match operation {
        Operation::KnobDialogGuard => Some(0),
        Operation::KnobDialogBaseline => Some(1),
        Operation::KnobDialogPreflightBegin => Some(2),
        Operation::KnobDialogPreflightDescriptor => Some(3),
        Operation::KnobDialogPreflightTextAlias => Some(4),
        Operation::KnobDialogPreflightLayerCollision => Some(5),
        Operation::KnobDialogPreflightWidgetLayer => Some(6),
        Operation::KnobDialogPreflightBackgroundLayer => Some(7),
        Operation::KnobDialogCallbackRace => Some(8),
        Operation::KnobDialogFirstActivation => Some(9),
        Operation::KnobDialogActiveDisplay => Some(10),
        Operation::KnobDialogLevelDemo => Some(11),
        Operation::KnobDialogTaskContexts => Some(12),
        Operation::KnobDialogOverlayInvariants => Some(13),
        Operation::KnobDialogBitmapZero => Some(14),
        Operation::KnobDialogActiveOverlay => Some(15),
        Operation::KnobDialogLockedUpdate => Some(16),
        Operation::KnobDialogModeBegin { .. } | Operation::KnobDialog => {
            next_mode.and_then(|index| u8::try_from(17 + index * 10).ok())
        }
        _ => None,
    };
    if fixed.is_some() {
        return fixed;
    }
    let scope = scope?;
    let offset = match operation {
        Operation::EnqueueTap { .. } => 1,
        Operation::WaitInputApplied => match pending?.kind {
            PendingInputKind::Tap => 2,
            PendingInputKind::Encoder => 5,
        },
        Operation::WaitKnobParameterMode { .. } => 3,
        Operation::EnqueueEncoder {
            midi_transaction: false,
            ..
        } => 4,
        Operation::CaptureDisplay { .. } => 6,
        Operation::ExpectCapturedDisplayImage { .. } => 7,
        Operation::KnobDialogExpectHeap => 8,
        Operation::KnobDialogModeEnd => 9,
        _ => return None,
    };
    Some(17 + scope.index as u8 * 10 + offset)
}

type KnobDeadline = (Instant, u32, u32, u64, u8);

fn advance_knob_deadline(is_terminal: bool, shared: KnobDeadline) -> Option<KnobDeadline> {
    if is_terminal {
        None
    } else {
        let (start, guest_start, guest_budget, wall_budget, phase) = shared;
        Some((start, guest_start, guest_budget, wall_budget, phase + 1))
    }
}

/// Only positive, single-reply checks can end early. An explicit reply window
/// or a rejected prefix needs the whole observation interval.
fn sysex_reply_complete(
    request: &crate::scenario::UsbSysexRequest,
    pending: &[u8],
    replies: &[Vec<u8>],
) -> bool {
    pending.is_empty()
        && request.reject_prefixes.is_empty()
        && request.reply_timeout_ms.is_none()
        && request.expect_prefix.as_ref().is_some_and(|prefix| {
            replies.iter().any(|reply| {
                reply.starts_with(prefix)
                    && reply.last() == Some(&0xf7)
                    && request
                        .min_reply_bytes
                        .is_none_or(|minimum| reply.len() >= minimum)
            })
        })
}

/// Require a complete RIFF container and enough audio for the tone checks.
fn recorded_wav_ready(bytes: &[u8]) -> Result<(), String> {
    if bytes.len() < 12 || &bytes[..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err("missing WAV header".into());
    }
    let declared = u32::from_le_bytes(bytes[4..8].try_into().unwrap()) as usize;
    if declared.saturating_add(8) != bytes.len() {
        return Err(format!(
            "length is {}, RIFF declares {}",
            bytes.len(),
            declared.saturating_add(8)
        ));
    }
    let mut offset = 12usize;
    while offset + 8 <= bytes.len() {
        let size = u32::from_le_bytes(bytes[offset + 4..offset + 8].try_into().unwrap()) as usize;
        let end = offset
            .checked_add(8)
            .and_then(|start| start.checked_add(size))
            .ok_or("WAV chunk length overflow")?;
        if end > bytes.len() {
            return Err("truncated WAV chunk".into());
        }
        offset = end
            .checked_add(size & 1)
            .ok_or("WAV chunk length overflow")?;
    }
    if offset != bytes.len() {
        return Err("incomplete WAV chunk padding".into());
    }
    let frames = waveform::decode_wav(bytes)?;
    if frames.len() < 96_000 {
        return Err(format!("only {} frames", frames.len()));
    }
    Ok(())
}

#[cfg(test)]
mod recorded_wav_tests {
    use super::recorded_wav_ready;

    #[test]
    fn requires_complete_container_and_two_seconds_of_audio() {
        let mut wav = Vec::new();
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&(36u32 + 192_000).to_le_bytes());
        wav.extend_from_slice(b"WAVEfmt ");
        wav.extend_from_slice(&16u32.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes());
        wav.extend_from_slice(&48_000u32.to_le_bytes());
        wav.extend_from_slice(&96_000u32.to_le_bytes());
        wav.extend_from_slice(&2u16.to_le_bytes());
        wav.extend_from_slice(&16u16.to_le_bytes());
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&192_000u32.to_le_bytes());
        wav.resize(wav.len() + 192_000, 0);
        assert!(recorded_wav_ready(&wav).is_ok());
        let mut truncated = wav.clone();
        truncated.pop();
        assert!(recorded_wav_ready(&truncated).is_err());
        wav[40..44].copy_from_slice(&192_001u32.to_le_bytes());
        assert!(recorded_wav_ready(&wav).is_err());
    }
}

pub(super) fn deadline_exceeded(
    wall_elapsed: Duration,
    guest_start: u32,
    guest_now: u32,
    guest_budget: u32,
    wall_budget: u64,
) -> bool {
    wall_elapsed > Duration::from_secs(wall_budget)
        || guest_now.wrapping_sub(guest_start) > guest_budget
}

#[cfg(test)]
mod carried_deadline_tests {
    use super::deadline_exceeded;
    use std::time::Duration;

    #[test]
    fn total_elapsed_is_checked_across_both_steps_and_guest_wrap() {
        let start = u32::MAX - 100;
        assert!(!deadline_exceeded(
            Duration::from_secs(120),
            start,
            49,
            150,
            120
        ));
        assert!(deadline_exceeded(
            Duration::from_secs(121),
            start,
            49,
            150,
            120
        ));
        assert!(deadline_exceeded(
            Duration::from_secs(1),
            start,
            50,
            150,
            120
        ));
    }
}

impl State {
    pub(super) fn release_retained_host_midi(&mut self) {
        if let Some(mut client) = self.host_midi.take() {
            client.release();
        }
    }

    pub fn restarted(&mut self) -> Result<(), String> {
        self.abort_midi_probe();
        if let Some(mut client) = self.native_midi.take() {
            client.release();
        }
        self.release_retained_host_midi();
        self.midi_round_trip = None;
        self.midi_sync = None;
        self.knob_popup = None;
        self.knob_dialog_deadline = None;
        self.knob_dialog_baseline = None;
        self.abort_knob_preflight()?;
        self.midi = None;
        Ok(())
    }
    pub fn has_knob_preflight(&self) -> bool {
        self.knob_dialog_preflight.is_some()
    }
    pub fn has_midi_probe(&self) -> bool {
        self.midi_probe.is_some()
    }
    pub fn has_midi_sync(&self) -> bool {
        self.midi_sync.is_some()
    }
    pub fn abort_midi_sync(&mut self) {
        self.midi_sync = None;
    }
    pub fn abort_midi_probe(&mut self) {
        // Drop only sessions opened by the probe; a previously connected
        // native mixer session remains owned by the scenario.
        self.midi_probe = None;
    }
    pub fn abort_knob_preflight(&mut self) -> Result<(), String> {
        self.knob_mode = None;
        self.knob_heap_reader = None;
        self.pending_input = None;
        self.captured_display = None;
        match self.knob_dialog_preflight.take() {
            Some(context) => super::knob_dialog::abort_preflight(context),
            None => Ok(()),
        }
    }
    pub fn execute(
        &mut self,
        session: &Session,
        options: &Options,
        operation: Operation,
        guest_timeout_ms: u32,
        wall_timeout_s: u64,
    ) -> Result<Value, String> {
        if self.l6_midi_probe.is_some()
            && !matches!(
                operation,
                Operation::L6MidiProbeInject
                    | Operation::L6MidiProbeExpectHook
                    | Operation::L6MidiProbeExpectReplies
            )
        {
            self.l6_midi_probe.take();
            return Err("L6 MIDI probe interrupted by another operation".into());
        }
        if self.midi_sync.is_some()
            && !matches!(
                operation,
                Operation::HostMidiDrain { .. }
                    | Operation::HostMidiSendCc { .. }
                    | Operation::HostMidiExpectSync
                    | Operation::HostMidiEnd
            )
        {
            self.abort_midi_sync();
            return Err("MIDI sync transaction interrupted by another operation".into());
        }
        if self.midi_probe.is_some()
            && !matches!(
                operation,
                Operation::MidiProbeSendCc { .. }
                    | Operation::MidiProbeExpectRing { .. }
                    | Operation::MidiProbeWaitRingStable { .. }
                    | Operation::MidiProbeDrainAndTap { .. }
                    | Operation::MidiProbeExpectNoEvents { .. }
                    | Operation::MidiProbeEnd
            )
        {
            self.abort_midi_probe();
            return Err("MIDI probe transaction interrupted by another operation".into());
        }
        let started = Instant::now();
        let control = l6ctl::OwnedControl::new(&session.engine);
        let guest_start = control
            .input_status(l6ctl::InputSource::Panel)
            .map_err(|e| e.to_string())?
            .guest_ms;
        let is_ready = matches!(operation, Operation::KnobDialogReady);
        let expected = knob_dialog_phase(
            &operation,
            self.knob_mode.as_ref(),
            self.pending_input.as_ref(),
            self.knob_dialog_preflight.as_ref().map(knob_mode_index),
        );
        let deadline_operation = expected.map(|_| matches!(operation, Operation::KnobDialog));
        if expected.is_none() && self.has_knob_preflight() {
            let error = "knob-dialog preflight interrupted by another operation";
            return match self.abort_knob_preflight() {
                Ok(()) => Err(error.into()),
                Err(cleanup) => Err(format!("{error}; debugger cleanup failed: {cleanup}")),
            };
        }
        let shared = if let Some(expected) = expected {
            let context = match self.knob_dialog_deadline.take() {
                Some(context) => context,
                None => {
                    let error = "knob-dialog requires preceding readiness step";
                    return match self.abort_knob_preflight() {
                        Ok(()) => Err(error.into()),
                        Err(cleanup) => Err(format!("{error}; debugger cleanup failed: {cleanup}")),
                    };
                }
            };
            if context.4 != expected {
                let error = "knob-dialog steps are out of order";
                return match self.abort_knob_preflight() {
                    Ok(()) => Err(error.into()),
                    Err(cleanup) => Err(format!("{error}; debugger cleanup failed: {cleanup}")),
                };
            }
            Some(context)
        } else {
            None
        };
        if let Some((start, begin_guest, guest_budget, wall_budget, _)) = shared {
            if deadline_exceeded(
                start.elapsed(),
                begin_guest,
                guest_start,
                guest_budget,
                wall_budget,
            ) {
                let error = "knob-dialog shared deadline exceeded before step";
                return match self.abort_knob_preflight() {
                    Ok(()) => Err(error.into()),
                    Err(cleanup) => Err(format!("{error}; debugger cleanup failed: {cleanup}")),
                };
            }
        }
        if is_ready {
            self.knob_dialog_deadline = None;
            self.knob_dialog_baseline = None;
        }
        let result = self.execute_inner(
            session,
            options,
            operation,
            guest_timeout_ms,
            wall_timeout_s,
            shared,
            (started, guest_start),
        );
        let result = match result {
            Ok(value) => value,
            Err(error) => {
                let primary = error.to_string();
                return match self.abort_knob_preflight() {
                    Ok(()) => Err(primary),
                    Err(cleanup) => Err(format!("{primary}; debugger cleanup failed: {cleanup}")),
                };
            }
        };
        let elapsed = control
            .input_status(l6ctl::InputSource::Panel)
            .map_err(|e| e.to_string())?
            .guest_ms
            .wrapping_sub(guest_start);
        if started.elapsed() > Duration::from_secs(wall_timeout_s) || elapsed > guest_timeout_ms {
            let error = "device operation exceeded its step deadline";
            return match self.abort_knob_preflight() {
                Ok(()) => Err(error.into()),
                Err(cleanup) => Err(format!("{error}; debugger cleanup failed: {cleanup}")),
            };
        }
        if let Some((start, guest_start, guest_budget, wall_budget, _)) = shared {
            let now = control
                .input_status(l6ctl::InputSource::Panel)
                .map_err(|e| e.to_string())?
                .guest_ms;
            if deadline_exceeded(start.elapsed(), guest_start, now, guest_budget, wall_budget) {
                let error = "device operation exceeded its step deadline";
                return match self.abort_knob_preflight() {
                    Ok(()) => Err(error.into()),
                    Err(cleanup) => Err(format!("{error}; debugger cleanup failed: {cleanup}")),
                };
            }
        }
        if is_ready {
            self.knob_dialog_deadline =
                Some((started, guest_start, guest_timeout_ms, wall_timeout_s, 0));
        } else if let Some(is_terminal) = deadline_operation {
            self.knob_dialog_deadline = advance_knob_deadline(is_terminal, shared.unwrap());
        }
        Ok(result)
    }

    fn execute_inner(
        &mut self,
        session: &Session,
        options: &Options,
        operation: Operation,
        guest_timeout_ms: u32,
        wall_timeout_s: u64,
        shared: Option<(Instant, u32, u32, u64, u8)>,
        operation_start: (Instant, u32),
    ) -> Result<Value, Box<dyn std::error::Error>> {
        match operation {
            Operation::LifecycleStart { .. }
            | Operation::LifecycleStop
            | Operation::WaitPanelUptime { .. }
            | Operation::ExpectUpdateState { .. }
            | Operation::ExpectUpdateMarkerAbsent
            | Operation::ExpectLiveUpdateMarkerAbsent
            | Operation::CaptureUpdateMarkerPending
            | Operation::MarkL6UpdatePending
            | Operation::ExpectL6UpdateResetVector
            | Operation::ExpectL6UpdateComparison { .. }
            | Operation::ExpectUpdateGui { .. }
            | Operation::ExpectInstallerOutcome { .. }
            | Operation::WaitUpdatePanelReady { .. }
            | Operation::WaitUsbController
            | Operation::BeginWallDeadline { .. }
            | Operation::EndWallDeadline
            | Operation::ExpectUpdateMode { .. }
            | Operation::ExpectMainMemory { .. }
            | Operation::ExpectUpdateComparison { .. }
            | Operation::CaptureInstalledPanel
            | Operation::ExpectPanelUnchanged => {
                Err("managed lifecycle operation must run at the session boundary".into())
            }
            Operation::ExpectRecorder => {
                let mode = session.engine.management[0].read_memory(0, l6max::system_mode, 4)?;
                if mode != 0u32.to_le_bytes() {
                    return Err(
                        format!("restart did not restore recorder mode: {mode:02x?}").into(),
                    );
                }
                Ok(json!({"startup_mode":"recorder"}))
            }
            Operation::ExpectMemory {
                symbol,
                address,
                bytes,
            } => {
                let actual = session.engine.management[0].read_memory(0, address, bytes.len())?;
                if actual != bytes {
                    return Err(format!(
                        "{symbol} ({address:#010x}) is {actual:02x?}, expected {bytes:02x?}"
                    )
                    .into());
                }
                Ok(json!({"symbol":symbol,"address":format!("{address:#010x}"),"bytes":actual}))
            }
            Operation::WaitMemory {
                symbol,
                address,
                bytes,
            } => {
                const GUEST_TIMEOUT_MS: u32 = 10_000;
                const WALL_TIMEOUT_S: u64 = 180;
                const POLL_INTERVAL_MS: u64 = 10;
                let start = guest_now(session);
                let deadline =
                    Instant::now() + Duration::from_secs(wall_timeout_s.min(WALL_TIMEOUT_S));
                loop {
                    session.poll()?;
                    let actual =
                        session.engine.management[0].read_memory(0, address, bytes.len())?;
                    if actual == bytes {
                        break Ok(
                            json!({"symbol":symbol,"address":format!("{address:#010x}"),"bytes":actual}),
                        );
                    }
                    if Instant::now() >= deadline
                        || guest_now(session).wrapping_sub(start)
                            >= guest_timeout_ms.min(GUEST_TIMEOUT_MS)
                    {
                        break Err(format!("{symbol} ({address:#010x}) did not match: {actual:02x?}, expected {bytes:02x?}").into());
                    }
                    thread::sleep(Duration::from_millis(POLL_INTERVAL_MS));
                }
            }
            Operation::EffectParameterEdit { encoder, steps } => {
                Ok(effect_parameter_edit(options, session, encoder, steps)
                    .map_err(std::io::Error::other)?)
            }
            Operation::WaitEffectMode {
                panel_mode,
                edit_mode,
            } => {
                wait_effect_mode(
                    options,
                    session,
                    panel_mode,
                    edit_mode,
                    guest_timeout_ms,
                    wall_timeout_s,
                )
                .map_err(std::io::Error::other)?;
                Ok(json!({"panel_mode": panel_mode, "edit_mode": edit_mode}))
            }
            Operation::EffectReplacementState {
                expected_effect_type,
                expected_processor_type,
                expected_panel_mode,
                expected_edit_mode,
                minimum_parameter_edits,
                expected_parameter_id,
                expected_parameter_value,
                expected_dialog_active,
                minimum_exit_ring_refreshes,
                expected_effect,
                require_output,
                wait_for_slot,
            } => {
                if wait_for_slot {
                    wait_effect_slot(
                        options,
                        session,
                        expected_effect_type,
                        expected_processor_type,
                        expected_effect.as_deref(),
                        require_output,
                        guest_timeout_ms,
                        wall_timeout_s,
                    )
                    .map_err(std::io::Error::other)?;
                }
                Ok(effect_replacement_state(
                    options,
                    session,
                    expected_effect_type,
                    expected_processor_type,
                    expected_panel_mode,
                    expected_edit_mode,
                    minimum_parameter_edits,
                    expected_parameter_id,
                    expected_parameter_value,
                    expected_dialog_active,
                    minimum_exit_ring_refreshes,
                    expected_effect.as_deref(),
                    require_output,
                )
                .map_err(std::io::Error::other)?)
            }
            Operation::WaitSdPatchLoaded => {
                let registry = wait_sd_patch_loaded(session, guest_timeout_ms, wall_timeout_s)?;
                Ok(json!({"registry": format!("{registry:#010x}")}))
            }
            Operation::WaitSdPatchEmpty => {
                let registry = wait_sd_patch_empty(session, guest_timeout_ms, wall_timeout_s)?;
                Ok(json!({"registry": format!("{registry:#010x}")}))
            }
            Operation::ExpectSdPatch {
                loaded,
                chain_count,
            } => {
                let main = selected_main_image(options).map_err(std::io::Error::other)?;
                let (edits, link_base, payload, _, _relocations, module_hooks, module_images) =
                    sd_fixture_edits(options, &main, session.engine.model())
                        .map_err(std::io::Error::other)?;
                let qmp = &session.engine.management[0];
                let image_base = session.engine.model().main_base();
                // The patch loader's own hook sites (patches/patch-loader),
                // which its build report does not list.
                let loader_calls: &[u32] = match session.engine.model() {
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
                    let offset = (address - image_base) as usize;
                    let expected = main
                        .get(offset..offset + 4)
                        .ok_or("Patch loader image too short")?;
                    if qmp.read_memory(0, address, 4)? != expected {
                        return Err(
                            format!("persistent loader hook missing at {address:#010x}").into()
                        );
                    }
                }
                for (address, original, _entry_offset) in &edits {
                    let offset = (*address - image_base) as usize;
                    let expected = &main[offset..offset + original.len()];
                    let actual = qmp.read_memory(0, *address, expected.len())?;
                    if actual != expected {
                        return Err(format!(
                            "persistent SD dispatch stub missing at {address:#010x}"
                        )
                        .into());
                    }
                }
                // Without a card mount since the last reset the loader has no
                // registry, so every dispatch stub takes the stock path.
                // A successful loader claim is published only after the SD
                // scan and hook installation have finished. Wait for that
                // signal when a module is expected; absence checks remain
                // immediate and require their own observation window.
                let registry = if loaded {
                    Some(wait_sd_patch_loaded(
                        session,
                        guest_timeout_ms,
                        wall_timeout_s,
                    )?)
                } else {
                    sd_registry_if_allocated(qmp, session.engine.model())?
                };
                let Some(registry) = registry else {
                    if loaded {
                        return Err("the Patch loader has not allocated its registry".into());
                    }
                    return Ok(json!({"loaded":false,"registry":null,"guarded_hooks":edits.len()}));
                };
                let loaded_base =
                    u32::from_le_bytes(qmp.read_memory(0, registry + 160, 4)?.try_into().unwrap());
                let loaded_len =
                    u32::from_le_bytes(qmp.read_memory(0, registry + 164, 4)?.try_into().unwrap());
                let known_module_size = module_images
                    .iter()
                    .any(|(module_payload, _, _)| module_payload.len() == loaded_len as usize);
                if loaded != (loaded_base != 0) || (loaded && !known_module_size) {
                    let stage = u32::from_le_bytes(
                        qmp.read_memory(0, registry + 4, 4)?.try_into().unwrap(),
                    );
                    return Err(format!(
                        "SD allocation state loaded={loaded}, base={loaded_base:#010x}, len={loaded_len}, loader stage={stage:#x}"
                    )
                    .into());
                }
                let claim =
                    u32::from_le_bytes(qmp.read_memory(0, registry + 8, 4)?.try_into().unwrap());
                if (claim == 1) != loaded {
                    let stage = u32::from_le_bytes(
                        qmp.read_memory(0, registry + 4, 4)?.try_into().unwrap(),
                    );
                    return Err(format!(
                        "SD module loaded={loaded}, claim={claim:#x}, loader stage={stage:#x}"
                    )
                    .into());
                }
                let expected_function = if chain_count.is_some() {
                    module_hooks
                        .iter()
                        .find(|hook| {
                            module_hooks
                                .iter()
                                .filter(|other| other.address == hook.address)
                                .count()
                                >= chain_count.unwrap_or(1) as usize
                        })
                        .or_else(|| module_hooks.first())
                        .map(|hook| hook.address)
                } else {
                    module_hooks.first().map(|hook| hook.address)
                };
                let mut expected_chains = std::collections::BTreeMap::<u32, u32>::new();
                for module_hook in &module_hooks {
                    *expected_chains.entry(module_hook.address).or_default() += 1;
                }
                for (address, module_count) in expected_chains {
                    let record =
                        sd_dynamic_handler_slot(options, session.engine.model(), registry, address)
                            .map_err(std::io::Error::other)?;
                    let (chain, _) = sd_site_chain(qmp, record)?;
                    let expected_count = if !loaded {
                        0
                    } else if Some(address) == expected_function {
                        chain_count.map_or(module_count, u32::from)
                    } else {
                        module_count
                    };
                    if chain.count != expected_count {
                        return Err(format!(
                            "SD hook site {address:#010x} has {} registered hooks, expected {expected_count}",
                            chain.count
                        )
                        .into());
                    }
                }

                let (registered_count, chain_head, chain_links) = if let Some(address) =
                    expected_function
                {
                    let record =
                        sd_dynamic_handler_slot(options, session.engine.model(), registry, address)
                            .map_err(std::io::Error::other)?;
                    let (chain, links) = sd_site_chain(qmp, record)?;
                    (chain.count, chain.handler, links)
                } else {
                    (0, 0, Vec::new())
                };
                if loaded != (registered_count != 0)
                    || chain_count.is_some_and(|expected| registered_count != u32::from(expected))
                {
                    return Err(format!(
                        "SD hook chain state loaded={loaded}, count={registered_count}, expected function={expected_function:?}, expected count={chain_count:?}"
                    )
                    .into());
                }
                let mut prior_order = None;
                for (index, link) in chain_links.iter().enumerate() {
                    if link.entry & 1 == 0
                        || link.module_base == 0
                        || prior_order.is_some_and(|prior| link.order < prior)
                    {
                        return Err(format!(
                            "invalid SD hook link {index}: entry={:#010x}, module={:#010x}, order={}",
                            link.entry, link.module_base, link.order
                        )
                        .into());
                    }
                    prior_order = Some(link.order);
                }
                let chain_links: Vec<(u32, u32, u32, u32)> = chain_links
                    .iter()
                    .map(|link| (link.entry, link.next_slot, link.order, link.module_base))
                    .collect();
                for (address, _, _entry_offset) in &edits {
                    // Every edit is one of the module's hooks (sd_fixture_edits).
                    let slot = sd_dynamic_handler_slot(
                        options,
                        session.engine.model(),
                        registry,
                        *address,
                    )
                    .map_err(std::io::Error::other)?;
                    let handler =
                        u32::from_le_bytes(qmp.read_memory(0, slot, 4)?.try_into().unwrap());
                    let valid = if !loaded {
                        handler == 0
                    } else if Some(*address) == expected_function {
                        handler == chain_head
                    } else {
                        handler != 0 && handler & 1 != 0
                    };
                    if !valid {
                        return Err(format!(
                            "SD handler at {slot:#010x} is invalid: {handler:#010x}"
                        )
                        .into());
                    }
                }
                if loaded {
                    let hook = module_hooks
                        .iter()
                        .find(|hook| Some(hook.address) == expected_function)
                        .ok_or("dynamic patch bundle has no hook record")?;
                    let root = sd_fixture_root(options).map_err(std::io::Error::other)?;
                    let ordered_chain = root.join(crate::fixture_layout::CHAIN_FIRST).is_file();
                    let patch_report: Value = serde_json::from_slice(&fs::read(
                        root.join(crate::fixture_layout::SD_BUNDLE_BUILD)
                            .join(crate::fixture_layout::PATCH_REPORT),
                    )?)?;
                    let patch_for_size = |size: usize| {
                        patch_report["patches"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .find(|patch| patch["memory_length"].as_u64() == Some(size as u64))
                    };
                    let mutable_ranges_for_size = |size: usize| {
                        let patch = patch_for_size(size);
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
                            // The loader zeroes this NOBITS tail before module
                            // initialization. Once running, it is module-owned
                            // writable state, so runtime byte comparison must
                            // not mistake legitimate initialization for code
                            // corruption.
                            ranges.push((code_len, size - code_len));
                        }
                        ranges
                    };
                    let pass_through_entry_offset = if ordered_chain {
                        Some(
                            patch_report["patches"][0]["pass_through_entry_offsets"][0]
                                .as_u64()
                                .ok_or("ordered-chain module has no pass-through entry")?
                                as u32,
                        )
                    } else {
                        None
                    };
                    let loader_report: Value = serde_json::from_slice(&fs::read(
                        root.join(crate::fixture_layout::LOADER_BUILD)
                            .join(crate::fixture_layout::PATCH_REPORT),
                    )?)?;
                    let _loader_patch = loader_report["patches"]
                        .as_array()
                        .and_then(|patches| {
                            patches.iter().find(|patch| patch["name"] == "patch-loader")
                        })
                        .ok_or("loader report has no patch-loader record")?;
                    let dispatch_site = loader_report["patches"]
                        .as_array()
                        .and_then(|patches| {
                            patches
                                .iter()
                                .find(|patch| patch["name"] == "dynamic-dispatch")
                        })
                        .and_then(|patch| patch["sites"].as_array())
                        .and_then(|sites| {
                            sites.iter().find(|site| {
                                site["address"].as_u64() == Some(u64::from(hook.address))
                            })
                        })
                        .ok_or("loader report has no dispatcher for the registered function")?;
                    let fallback = u32::try_from(
                        dispatch_site["fallback"]
                            .as_u64()
                            .ok_or("loader report has no function continuation")?,
                    )? | 1;
                    if chain_count == Some(2) {
                        let first = chain_links.first().ok_or("empty expected hook chain")?;
                        let second = chain_links.get(1).ok_or("short expected hook chain")?;
                        if first.2 >= second.2 {
                            return Err("module hook order was not preserved in the chain".into());
                        }
                    }
                    for (index, (entry, next_slot, _order, module)) in
                        chain_links.iter().enumerate()
                    {
                        let (module_payload, module_relocations, module_payload_hooks) =
                            module_images
                                .iter()
                                .find_map(|(payload, relocations, hooks)| {
                                    hooks
                                        .iter()
                                        .find(|candidate| {
                                            candidate.address == hook.address
                                                && (*entry
                                                    == ((*module + candidate.entry_offset) | 1)
                                                    || ordered_chain
                                                        && index == 0
                                                        && *entry
                                                            == ((*module
                                                                + pass_through_entry_offset
                                                                    .unwrap_or(u32::MAX))
                                                                | 1))
                                                && *next_slot
                                                    == *module + candidate.next_slot_offset
                                        })
                                        .map(|_| (payload, relocations, hooks))
                                })
                                .ok_or_else(|| {
                                    format!(
                                        "hook {index} does not match any module's hook metadata"
                                    )
                                })?;
                        let mutable_symbol_ranges = mutable_ranges_for_size(module_payload.len());
                        let expected_next =
                            chain_links.get(index + 1).map_or(fallback, |next| next.0);
                        let actual_next = u32::from_le_bytes(
                            qmp.read_memory(0, *next_slot, 4)?.try_into().unwrap(),
                        );
                        if actual_next != expected_next {
                            return Err(format!(
                                "hook {index} continues to {actual_next:#010x}, expected {expected_next:#010x}"
                            )
                            .into());
                        }
                        let mut expected = module_payload.clone();
                        if ordered_chain && index == 0 {
                            // The first copy names its own package, so its
                            // code is the fixture file's, not the bundle's.
                            let file = fs::read(root.join(crate::fixture_layout::CHAIN_FIRST))?;
                            let start = patch_abi::dynamic::HEADER_LEN_V1;
                            expected = file
                                .get(start..start + expected.len())
                                .ok_or("ordered-chain fixture is shorter than its module")?
                                .to_vec();
                        }
                        patch_abi::relocation::relocate(
                            &mut expected,
                            module_relocations.clone().into_iter(),
                            link_base,
                            *module,
                        )
                        .map_err(|error| format!("invalid fixture relocations: {error:?}"))?;
                        for payload_hook in module_payload_hooks {
                            if payload_hook.next_slot_offset == u32::MAX {
                                continue;
                            }
                            let slot = *module + payload_hook.next_slot_offset;
                            let target = qmp.read_memory(0, slot, 4)?;
                            let slot_offset = payload_hook.next_slot_offset as usize;
                            expected[slot_offset..slot_offset + 4].copy_from_slice(&target);
                        }
                        for (chunk_index, chunk) in expected.chunks(4096).enumerate() {
                            let address = *module + (chunk_index * 4096) as u32;
                            let actual = qmp.read_memory(0, address, chunk.len())?;
                            let mismatch = actual.iter().zip(chunk).enumerate().find(
                                |(index, (actual, expected))| {
                                    let module_offset = chunk_index * 4096 + index;
                                    actual != expected
                                        && !mutable_symbol_ranges.iter().any(|(start, length)| {
                                            module_offset >= *start
                                                && module_offset < *start + *length
                                        })
                                },
                            );
                            if let Some((offset, (actual_byte, expected_byte))) = mismatch {
                                return Err(format!(
                                    "allocated SD module {index} differs from its relocated bundle at {:#010x} (expected {:#04x}, got {:#04x})",
                                    address + offset as u32,
                                    expected_byte,
                                    actual_byte,
                                )
                                .into());
                            }
                        }
                        if (*entry & !1) < *module {
                            return Err(format!("hook {index} entry precedes its module").into());
                        }
                    }
                    if let Some((latest_payload, latest_relocations, latest_hooks)) = module_images
                        .iter()
                        .find(|(module_payload, _, _)| module_payload.len() == loaded_len as usize)
                    {
                        let mutable_symbol_ranges = mutable_ranges_for_size(latest_payload.len());
                        let mut expected = latest_payload.clone();
                        patch_abi::relocation::relocate(
                            &mut expected,
                            latest_relocations.clone().into_iter(),
                            link_base,
                            loaded_base,
                        )
                        .map_err(|error| format!("invalid latest module relocations: {error:?}"))?;
                        for latest_hook in latest_hooks {
                            if latest_hook.next_slot_offset == u32::MAX {
                                continue;
                            }
                            let slot = loaded_base + latest_hook.next_slot_offset;
                            let target = qmp.read_memory(0, slot, 4)?;
                            let offset = latest_hook.next_slot_offset as usize;
                            expected[offset..offset + 4].copy_from_slice(&target);
                        }
                        for (chunk_index, chunk) in expected.chunks(4096).enumerate() {
                            let address = loaded_base + (chunk_index * 4096) as u32;
                            let actual = qmp.read_memory(0, address, chunk.len())?;
                            let mismatch = actual.iter().zip(chunk).enumerate().any(
                                |(index, (actual, expected))| {
                                    let module_offset = chunk_index * 4096 + index;
                                    actual != expected
                                        && !mutable_symbol_ranges.iter().any(|(start, length)| {
                                            module_offset >= *start
                                                && module_offset < *start + *length
                                        })
                                },
                            );
                            if mismatch {
                                return Err(
                                    "latest allocated SD module does not match its relocated bundle"
                                        .into(),
                                );
                            }
                        }
                    }
                }
                Ok(
                    json!({"loaded":loaded,"guarded_hooks":edits.len(),"module_bytes":if loaded { payload.len() } else { 0 },"hook_chain_count":registered_count,"hook_chain_orders":chain_links.iter().map(|link|link.2).collect::<Vec<_>>()}),
                )
            }
            Operation::ExpectHeap { minimum_free } => {
                let heap =
                    l6max_host::heap::read(&session.engine.management[0], session.engine.model())?;
                if heap.minimum_ever_free < minimum_free {
                    return Err(format!(
                        "heap fell to {} free bytes, below the required {minimum_free}: {heap:?}",
                        heap.minimum_ever_free
                    )
                    .into());
                }
                Ok(json!({"heap":heap,"peak_used":heap.peak_used()}))
            }
            Operation::HoldChord {
                targets,
                duration_ms,
                until,
            } => {
                let inputs = session.engine.inputs.as_ref().unwrap();
                let start = inputs.panel.snapshot().guest_ms;
                let factory_mode =
                    matches!(until, Some(crate::scenario::ChordCondition::FactoryMode));
                if factory_mode {
                    let initial =
                        session.engine.management[0].read_memory(0, l6max::system_mode, 4)?;
                    if initial == 7u32.to_le_bytes() {
                        return Err("factory mode was already active before the chord".into());
                    }
                }
                // DOWN duration is a minimum hold in the emulator; a
                // condition-based chord requests the normal button minimum.
                let hold =
                    duration_ms.map_or(0, |duration| duration.saturating_add(1000).min(60_000));
                for target in &targets {
                    inputs.down(*target, hold)?;
                }
                let deadline = Instant::now()
                    + Duration::from_secs(if factory_mode {
                        wall_timeout_s.min(180)
                    } else {
                        wall_timeout_s
                    });
                let guest_limit = if factory_mode {
                    guest_timeout_ms.min(10_000)
                } else {
                    guest_timeout_ms
                };
                let mut next_mode_poll = Instant::now();
                let mut factory_reached = false;
                loop {
                    session.poll()?;
                    let main = inputs.main.snapshot();
                    let panel = inputs.panel.snapshot();
                    if main.powered_off || panel.powered_off {
                        return Err("guest powered off during held chord".into());
                    }
                    let elapsed = panel.guest_ms.wrapping_sub(start);
                    if factory_mode && !factory_reached && Instant::now() >= next_mode_poll {
                        factory_reached =
                            session.engine.management[0].read_memory(0, l6max::system_mode, 4)?
                                == 7u32.to_le_bytes();
                        next_mode_poll = Instant::now() + Duration::from_millis(10);
                    }
                    if main.applied >= main.sent
                        && panel.applied >= panel.sent
                        && (factory_reached
                            || duration_ms.is_some_and(|duration| elapsed >= duration))
                    {
                        break;
                    }
                    if Instant::now() > deadline || elapsed > guest_limit {
                        return Err("held chord was not applied within its deadline".into());
                    }
                    thread::sleep(Duration::from_millis(5));
                }
                for target in targets.iter().rev() {
                    inputs.up(*target)?;
                }
                loop {
                    session.poll()?;
                    let main = inputs.main.snapshot();
                    let panel = inputs.panel.snapshot();
                    if main.applied >= main.sent && panel.applied >= panel.sent {
                        break;
                    }
                    if Instant::now() > deadline {
                        return Err("held chord release was not acknowledged".into());
                    }
                    thread::sleep(Duration::from_millis(5));
                }
                Ok(
                    json!({"targets":targets,"held_guest_ms":inputs.panel.snapshot().guest_ms.wrapping_sub(start)}),
                )
            }
            Operation::PanelEvent {
                target,
                column,
                row,
                event,
            } => {
                use firmware_api::l6::main_event_submit;
                use firmware_api::l6::panel_event_dispatcher;
                use firmware_api::l6::panel_report;
                let inputs = &session.engine.inputs.as_ref().unwrap().panel;
                let qmp = &session.engine.management[0];
                let socket = options
                    .gdb_socket
                    .as_ref()
                    .ok_or("panel-event requires a configured GDB socket")?;
                if self.gdb.is_none() {
                    self.gdb = Some(l6max_diagnostics::gdb::Gdb::connect(socket)?);
                }
                let gdb = self.gdb.as_mut().unwrap();
                qmp.stop()?;
                gdb.breakpoint(panel_event_dispatcher, true)?;
                let mut dispatcher_armed = true;
                gdb.breakpoint(main_event_submit, true)?;
                inputs.down(target, 0)?;
                let deadline = Instant::now() + Duration::from_secs(15);
                let mut skipped_reports = 0u32;
                let bytes = loop {
                    let remaining = deadline.saturating_duration_since(Instant::now());
                    if remaining.is_zero() {
                        return Err(format!(
                            "panel target {target} did not submit a button report after {skipped_reports} unrelated submissions"
                        )
                        .into());
                    }
                    gdb.continue_until_stop(remaining)?;
                    let pc = gdb.read_register(15)? & !1;
                    if pc == panel_event_dispatcher {
                        gdb.breakpoint(panel_event_dispatcher, false)?;
                        dispatcher_armed = false;
                        continue;
                    }
                    if pc != main_event_submit {
                        return Err(format!(
                            "panel target {target} stopped at {pc:#x}, not dispatcher or event submit"
                        )
                        .into());
                    }
                    let response = gdb.request(&format!("m{panel_report:x},3"))?;
                    if response.len() != 6 {
                        return Err(format!("unexpected panel report bytes: {response}").into());
                    }
                    let bytes = (0..3)
                        .map(|index| u8::from_str_radix(&response[index * 2..index * 2 + 2], 16))
                        .collect::<Result<Vec<_>, _>>()?;
                    // Other panel activity can submit events between input
                    // injection and the button report, especially at boot.
                    if !matches!(bytes[0], 0x90 | 0x91) {
                        skipped_reports += 1;
                        // Continuing from a software breakpoint can stop on
                        // the same instruction again. Step over the unrelated
                        // submission before waiting for the next one.
                        gdb.breakpoint(main_event_submit, false)?;
                        gdb.request("s")?;
                        gdb.breakpoint(main_event_submit, true)?;
                        continue;
                    }
                    break bytes;
                };
                if (bytes[1], bytes[2]) != (column, row) {
                    return Err(format!(
                        "panel target {target} report {bytes:02x?}, expected ({column},{row})"
                    )
                    .into());
                }
                let state = gdb.read_register(1)?;
                let observed_event = gdb.read_register(2)?;
                if state != 0 || observed_event != u32::from(event) {
                    return Err(format!("panel target {target} submitted state {state} event {observed_event}, expected press-state 0 event {event}").into());
                }
                let entry = if column == 0 {
                    l6::PANEL_COLUMN0_EVENT_TABLE + u32::from(row) * 0x14
                } else {
                    l6::PANEL_EVENT_TABLE + u32::from(column) * 0xa0 + u32::from(row) * 0x14
                };
                let mapping = qmp.read_memory(0, entry + 8, 4)?;
                let mapped_event = u32::from_le_bytes(mapping.try_into().unwrap());
                if mapped_event != u32::from(event) {
                    return Err(format!(
                        "MAIN map ({column},{row}) is {mapped_event}, expected {event}"
                    )
                    .into());
                }
                if dispatcher_armed {
                    gdb.breakpoint(panel_event_dispatcher, false)?;
                }
                gdb.breakpoint(main_event_submit, false)?;
                qmp.resume()?;
                inputs.up(target)?;
                let started = inputs.snapshot().guest_ms;
                let deadline = Instant::now() + Duration::from_secs(wall_timeout_s);
                loop {
                    session.poll()?;
                    if inputs.snapshot().guest_ms.wrapping_sub(started) >= 250 {
                        break;
                    }
                    if Instant::now() > deadline {
                        return Err("panel event release timed out".into());
                    }
                    thread::sleep(Duration::from_millis(5));
                }
                Ok(
                    json!({"target":target,"column":column,"row":row,"event":event,"report":bytes,"skipped_reports":skipped_reports}),
                )
            }
            Operation::WaitRecordedFiles { files } => {
                const GUEST_TIMEOUT_MS: u32 = 20_000;
                const WALL_TIMEOUT_S: u64 = 180;
                const POLL_INTERVAL_MS: u64 = 100;
                let card = options
                    .sd_image
                    .as_ref()
                    .ok_or("wait-recorded-files requires an SD card")?;
                let start = guest_now(session);
                let deadline =
                    Instant::now() + Duration::from_secs(wall_timeout_s.min(WALL_TIMEOUT_S));
                loop {
                    session.poll()?;
                    let status = (|| -> Result<String, String> {
                        let mut volume = sd_image::open(card).map_err(|e| e.to_string())?;
                        let project = volume
                            .entries("RECORDER")
                            .map_err(|e| e.to_string())?
                            .into_iter()
                            .filter(|entry| entry.directory && !entry.name.starts_with('.'))
                            .map(|entry| entry.name)
                            .max()
                            .ok_or("the card has no recorder project")?;
                        for file in &files {
                            let bytes = volume
                                .read(&format!("RECORDER/{project}/{file}"))
                                .map_err(|e| e.to_string())?;
                            recorded_wav_ready(&bytes)
                                .map_err(|error| format!("{file}: {error}"))?;
                        }
                        Ok(project)
                    })();
                    match status {
                        Ok(project) => break Ok(json!({"project": project, "files": files})),
                        Err(error)
                            if Instant::now() >= deadline
                                || guest_now(session).wrapping_sub(start)
                                    >= guest_timeout_ms.min(GUEST_TIMEOUT_MS) =>
                        {
                            break Err(format!("recorded files did not finalize: {error}").into());
                        }
                        Err(_) => thread::sleep(Duration::from_millis(POLL_INTERVAL_MS)),
                    }
                }
            }
            Operation::AudioInputTransport {
                command,
                position_ms,
            } => {
                use l6max_host::protocol;
                let target = match command.as_str() {
                    "stop" => protocol::AUDIO_INPUT_STOP,
                    "play" => protocol::AUDIO_INPUT_PLAY,
                    "pause" => protocol::AUDIO_INPUT_PAUSE,
                    "seek" => protocol::AUDIO_INPUT_SEEK,
                    other => return Err(format!("unknown audio input command {other}").into()),
                };
                let main = &session.engine.inputs.as_ref().unwrap().main;
                let sequence = main.audio_input_transport(target, position_ms)?;
                Ok(json!({"command": command, "position_ms": position_ms, "sequence": sequence}))
            }
            Operation::AudioInputLoad { channel, loaded } => {
                let main = &session.engine.inputs.as_ref().unwrap().main;
                let before = main.snapshot().rejected;
                let sequence = main.audio_input_load(channel - 1, loaded)?;
                let deadline =
                    std::time::Instant::now() + std::time::Duration::from_secs(wall_timeout_s);
                while main
                    .snapshot()
                    .last_ack
                    .is_none_or(|ack| ack.sequence != sequence)
                {
                    if std::time::Instant::now() >= deadline {
                        return Err("audio input load was not acknowledged".into());
                    }
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                if main.snapshot().rejected != before {
                    return Err(format!("channel {channel} has no prepared audio").into());
                }
                Ok(json!({"channel": channel, "loaded": loaded}))
            }
            Operation::WaitAudioInput {
                playing,
                min_ms,
                max_ms,
            } => {
                const GUEST_TIMEOUT_MS: u32 = 10_000;
                const WALL_TIMEOUT_S: u64 = 180;
                const POLL_INTERVAL_MS: u64 = 10;
                let start = guest_now(session);
                let deadline =
                    Instant::now() + Duration::from_secs(wall_timeout_s.min(WALL_TIMEOUT_S));
                loop {
                    session.poll()?;
                    let status = session.engine.inputs.as_ref().unwrap().main.snapshot();
                    if status.audio_input_playing == playing
                        && (min_ms..=max_ms).contains(&status.audio_input_position_ms)
                    {
                        break Ok(
                            json!({"playing": playing, "position_ms": status.audio_input_position_ms}),
                        );
                    }
                    if Instant::now() >= deadline
                        || guest_now(session).wrapping_sub(start)
                            >= guest_timeout_ms.min(GUEST_TIMEOUT_MS)
                    {
                        break Err(format!("audio input did not reach playing={playing}, {min_ms}..={max_ms} ms: playing={}, position={} ms",status.audio_input_playing,status.audio_input_position_ms).into());
                    }
                    thread::sleep(Duration::from_millis(POLL_INTERVAL_MS));
                }
            }
            Operation::ExpectAudioInput {
                playing,
                min_ms,
                max_ms,
            } => {
                let status = session.engine.inputs.as_ref().unwrap().main.snapshot();
                let evidence = json!({
                    "playing": status.audio_input_playing,
                    "position_ms": status.audio_input_position_ms,
                });
                if status.audio_input_playing != playing
                    || !(min_ms..=max_ms).contains(&status.audio_input_position_ms)
                {
                    return Err(format!(
                        "audio input state {evidence} is not playing={playing}, {min_ms}..={max_ms} ms"
                    )
                    .into());
                }
                Ok(evidence)
            }
            Operation::ExpectTone {
                file,
                hz,
                levels,
                tolerance_db,
            } => {
                const RATE: usize = 48000;
                let frames = match &file {
                    None => {
                        let path = std::env::var_os("L6_AUDIO_RAW_CAPTURE")
                            .ok_or("expect-output-tone requires L6_AUDIO_RAW_CAPTURE")?;
                        let frames = waveform::decode_capture(&fs::read(path)?);
                        frames[frames.len().saturating_sub(RATE)..].to_vec()
                    }
                    Some(file) => {
                        let card = options
                            .sd_image
                            .as_ref()
                            .ok_or("expect-recorded-tone requires an SD card")?;
                        let mut volume = sd_image::open(card)?;
                        let project = volume
                            .entries("RECORDER")?
                            .into_iter()
                            .filter(|entry| entry.directory && !entry.name.starts_with('.'))
                            .map(|entry| entry.name)
                            .max()
                            .ok_or("the card has no recorder project")?;
                        let bytes = volume.read(&format!("RECORDER/{project}/{file}"))?;
                        let frames = waveform::decode_wav(&bytes)?;
                        frames[RATE.min(frames.len())..].to_vec()
                    }
                };
                if frames.len() < RATE {
                    return Err(format!("only {} frames to measure", frames.len()).into());
                }
                let measured = waveform::tone_levels_db(&frames[..RATE], hz, RATE as f64);
                let evidence =
                    json!({"file": file, "hz": hz, "measured_db": measured, "expected_db": levels});
                for (side, (measured, expected)) in
                    ["left", "right"].iter().zip(measured.iter().zip(levels))
                {
                    let matches = match expected {
                        Some(expected) => (measured - expected).abs() <= tolerance_db,
                        None => *measured < -70.,
                    };
                    if !matches {
                        return Err(format!("{side} tone mismatch: {evidence}").into());
                    }
                }
                Ok(evidence)
            }
            Operation::ExpectCardEntry {
                path,
                size,
                matches_state,
            } => {
                let card = options
                    .sd_image
                    .as_ref()
                    .ok_or("expect-card-entry requires an SD card")?;
                let deadline =
                    std::time::Instant::now() + std::time::Duration::from_secs(wall_timeout_s);
                while !sd_image::entry_exists(card, &path)? {
                    if std::time::Instant::now() >= deadline {
                        return Err(format!("{path} did not appear on the SD card").into());
                    }
                    std::thread::sleep(std::time::Duration::from_millis(250));
                }
                let mut evidence = json!({ "card_entry": path });
                let expected = match &matches_state {
                    Some(name) => {
                        let state = options
                            .state_dir
                            .as_ref()
                            .ok_or("expect-card-entry matches_state requires a state directory")?;
                        Some(fs::read(state.join(name))?)
                    }
                    None => None,
                };
                let limit = size
                    .or(expected.as_ref().map(|bytes| bytes.len() as u64))
                    .map(|length| length + 1);
                if let Some(limit) = limit {
                    let actual = sd_image::read_file(card, &path, limit)?;
                    if let Some(size) = size
                        && actual.len() as u64 != size
                    {
                        return Err(
                            format!("{path} is {} bytes, expected {size}", actual.len()).into()
                        );
                    }
                    if let (Some(expected), Some(name)) = (&expected, &matches_state) {
                        if actual.len() != expected.len() {
                            return Err(format!(
                                "{path} is {} bytes, {name} is {}",
                                actual.len(),
                                expected.len()
                            )
                            .into());
                        }
                        if let Some(offset) = actual.iter().zip(expected).position(|(a, b)| a != b)
                        {
                            return Err(format!(
                                "{path} differs from {name} at offset {offset:#x}"
                            )
                            .into());
                        }
                        evidence["matches_state"] = json!(name);
                    }
                    evidence["size"] = json!(actual.len());
                    evidence["sha256"] = json!(format!("{:x}", Sha256::digest(&actual)));
                }
                Ok(evidence)
            }
            Operation::L6maxTapHandler => {
                const TAP_TARGET: u32 = 48;
                use firmware_api::l6max::l6max_tap_press_handler;
                let inputs = &session.engine.inputs.as_ref().unwrap().main;
                let qmp = &session.engine.management[0];
                let socket = options
                    .gdb_socket
                    .as_ref()
                    .ok_or("l6max-tap-handler requires a configured GDB socket")?;
                if self.gdb.is_none() {
                    self.gdb = Some(l6max_diagnostics::gdb::Gdb::connect(socket)?);
                }
                let gdb = self.gdb.as_mut().unwrap();
                qmp.stop()?;
                gdb.breakpoint(l6max_tap_press_handler, true)?;
                let applied = inputs.snapshot().applied;
                inputs.down(TAP_TARGET, 0)?;
                wait_l6_input(
                    session,
                    || inputs.snapshot().applied > applied,
                    "TAP press acknowledgement",
                )?;
                let stopped =
                    continue_within_guest(session, gdb, 5_000, Duration::from_secs(wall_timeout_s));
                if let Err(error) = stopped {
                    let _ = gdb.breakpoint(l6max_tap_press_handler, false);
                    let _ = qmp.resume();
                    return Err(format!("TAP press did not reach its handler: {error}").into());
                }
                let pc = gdb.read_register(15)? & !1;
                gdb.breakpoint(l6max_tap_press_handler, false)?;
                qmp.resume()?;
                if pc != l6max_tap_press_handler {
                    return Err(format!(
                        "TAP press stopped at {pc:#010x}, expected {l6max_tap_press_handler:#010x}"
                    )
                    .into());
                }
                let applied = inputs.snapshot().applied;
                inputs.up(TAP_TARGET)?;
                wait_l6_input(
                    session,
                    || inputs.snapshot().applied > applied,
                    "TAP release acknowledgement",
                )?;
                Ok(json!({"target":TAP_TARGET,"handler":l6max_tap_press_handler}))
            }
            Operation::L6MainGpioEvent { target, event } => {
                use firmware_api::l6::main_event_submit;
                use firmware_api::l6::sound_pad_event_handler;
                let inputs = &session.engine.inputs.as_ref().unwrap().main;
                let qmp = &session.engine.management[0];
                let socket = options
                    .gdb_socket
                    .as_ref()
                    .ok_or("l6-main-gpio-event requires a configured GDB socket")?;
                if self.gdb.is_none() {
                    self.gdb = Some(l6max_diagnostics::gdb::Gdb::connect(socket)?);
                }
                let gdb = self.gdb.as_mut().unwrap();
                qmp.stop()?;
                gdb.breakpoint(main_event_submit, true)?;
                let applied = inputs.snapshot().applied;
                inputs.down(target, 0)?;
                wait_l6_input(
                    session,
                    || inputs.snapshot().applied > applied,
                    "MAIN GPIO press acknowledgement",
                )?;
                gdb.continue_until_stop(Duration::from_secs(15))?;
                let pc = gdb.read_register(15)? & !1;
                if pc != main_event_submit {
                    return Err(format!(
                        "L6 MAIN target {target} stopped at {pc:#010x}, expected event submit {main_event_submit:#010x}"
                    )
                    .into());
                }
                let args = (0..4)
                    .map(|register| gdb.read_register(register))
                    .collect::<Result<Vec<_>, _>>()?;
                if args[2] != u32::from(event) {
                    return Err(format!(
                        "L6 MAIN target {target} submitted event {:#04x}, expected {event:#04x}",
                        args[2]
                    )
                    .into());
                }
                let dispatch_arguments = if (0x15..=0x18).contains(&event) {
                    gdb.breakpoint(main_event_submit, false)?;
                    gdb.breakpoint(sound_pad_event_handler, true)?;
                    match gdb.continue_until_stop(Duration::from_secs(15)) {
                        Ok(_) => {
                            let dispatch_pc = gdb.read_register(15)? & !1;
                            if dispatch_pc != sound_pad_event_handler {
                                return Err(format!(
                                    "L6 MAIN target {target} stopped at {dispatch_pc:#010x}, expected sound-pad event handler {sound_pad_event_handler:#010x}"
                                )
                                .into());
                            }
                            let dispatch_args = (0..3)
                                .map(|register| gdb.read_register(register))
                                .collect::<Result<Vec<_>, _>>()?;
                            gdb.breakpoint(sound_pad_event_handler, false)?;
                            Some(dispatch_args)
                        }
                        Err(error) => {
                            let _ = gdb.interrupt();
                            let _ = gdb.breakpoint(sound_pad_event_handler, false);
                            let _ = qmp.resume();
                            return Err(format!(
                                "L6 MAIN target {target} submitted event {event:#04x} but did not reach the sound-pad event handler: {error}"
                            )
                            .into());
                        }
                    }
                } else {
                    gdb.breakpoint(main_event_submit, false)?;
                    None
                };
                qmp.resume()?;
                let applied = inputs.snapshot().applied;
                inputs.up(target)?;
                wait_l6_input(
                    session,
                    || inputs.snapshot().applied > applied,
                    "MAIN GPIO release acknowledgement",
                )?;
                wait_guest_duration(
                    session,
                    100,
                    Instant::now() + Duration::from_secs(wall_timeout_s),
                )?;
                Ok(
                    json!({"target":target,"event":event,"arguments":args,"dispatch_arguments":dispatch_arguments}),
                )
            }
            Operation::L6UsbStartup => {
                Ok(
                    run_l6_usb_startup(session, options, guest_timeout_ms, wall_timeout_s)
                        .map_err(std::io::Error::other)?,
                )
            }
            Operation::L6DinEventDispatch { group, cc, value } => {
                run_l6_din_event_dispatch(session, options, group, cc, value)
            }
            Operation::L6InputSweep => {
                Ok(run_l6_input_sweep(session, options).map_err(std::io::Error::other)?)
            }
            Operation::L6DinSweep => {
                Ok(run_l6_din_sweep(session, options).map_err(std::io::Error::other)?)
            }
            Operation::L6maxInputTiming => {
                Ok(run_l6max_input_timing(session, options).map_err(std::io::Error::other)?)
            }
            Operation::L6maxPanelKeyMap => {
                Ok(run_l6max_panel_key_map(session, wall_timeout_s)
                    .map_err(std::io::Error::other)?)
            }
            Operation::L6maxChannelSignalPwm => Ok(run_l6max_channel_signal_pwm(
                session,
                options,
                guest_timeout_ms,
                wall_timeout_s,
            )
            .map_err(std::io::Error::other)?),
            Operation::L6LevelMeterPwm => {
                Ok(
                    run_l6_level_meter_pwm(session, options, guest_timeout_ms, wall_timeout_s)
                        .map_err(std::io::Error::other)?,
                )
            }
            Operation::Restart => Err("restart is supervised by the session runner".into()),
            Operation::WarmReset => {
                session.engine.management[0].system_reset()?;
                Ok(json!({"reset":"qmp-system-reset"}))
            }
            Operation::PowerPress { hold_ms, powered } => {
                let inputs = session.engine.inputs.as_ref().unwrap();
                // Power-button events are routed to MAIN even when sent as a
                // tap. Its acknowledgement gives the exact guest timestamp
                // at which the press was applied. The panel publishes the
                // continuing guest clock used to bound short presses.
                let input = &inputs.main;
                let sequence = if powered {
                    inputs.tap(53, hold_ms)?
                } else {
                    inputs.main.down(53, hold_ms)?
                };
                let mut started_guest_ms = None;
                let deadline = Instant::now() + Duration::from_secs(wall_timeout_s);
                loop {
                    session.poll()?;
                    let input_status = input.snapshot();
                    if let Some(ack) = input_status.last_ack.filter(|ack| ack.sequence == sequence)
                    {
                        if !matches!(
                            ack.kind,
                            l6max_host::protocol::APPLIED | l6max_host::protocol::RELEASED
                        ) {
                            return Err("power press was rejected by the emulator".into());
                        }
                        started_guest_ms.get_or_insert(ack.duration_ms);
                    }
                    let main_status = inputs.main.snapshot();
                    let off = main_status.powered_off;
                    let elapsed = started_guest_ms
                        .zip(main_status.powered_off_guest_ms)
                        .map(|(started, stopped)| stopped.wrapping_sub(started));
                    let guest_now = if powered {
                        inputs.panel.snapshot().guest_ms
                    } else {
                        input_status.guest_ms
                    };
                    let guest_elapsed =
                        started_guest_ms.map(|started| guest_now.wrapping_sub(started));
                    if powered && off {
                        return Err("short power press cut power".into());
                    }
                    if !powered && off {
                        if session.engine.management[0].query_status()?.status != "shutdown" {
                            return Err("power-off did not shut down QEMU".into());
                        }
                        // The input and panel guest clocks can differ by one
                        // update interval while the main core shuts down.
                        let elapsed = elapsed.ok_or(
                            "power-off occurred before its input timestamps were observed",
                        )?;
                        if session.engine.model() == l6fw::Model::L6
                            && !(300..=800).contains(&elapsed)
                        {
                            return Err(format!("L6 cutoff took {elapsed} guest ms").into());
                        }
                        thread::sleep(Duration::from_millis(100));
                        let stopped = inputs.panel.snapshot().guest_ms;
                        thread::sleep(Duration::from_millis(200));
                        if inputs.panel.snapshot().guest_ms != stopped {
                            return Err("guest clock advanced while powered off".into());
                        }
                        return Ok(
                            json!({"powered":false,"held_guest_ms":elapsed,"clock_stopped":true}),
                        );
                    }
                    if powered && guest_elapsed.is_some_and(|elapsed| elapsed >= hold_ms + 1000) {
                        return Ok(json!({"powered":true,"held_guest_ms":hold_ms}));
                    }
                    if Instant::now() > deadline
                        || powered
                            && guest_elapsed.is_some_and(|elapsed| elapsed > guest_timeout_ms)
                    {
                        return Err("power transition timed out".into());
                    }
                    thread::sleep(Duration::from_millis(5));
                }
            }
            Operation::ExpectSettings { restored } => {
                let state = options
                    .state_dir
                    .as_ref()
                    .ok_or("settings assertion requires persistent state")?;
                let saved = fs::read(state.join("panel-rtc.bin"))?;
                let panel = fs::read(state.join("panel-flash.bin"))?;
                let nor = fs::read(state.join("main-nor.bin"))?;
                if session.engine.model() != l6fw::Model::L6max {
                    return Err("settings layout currently supports L6max 1.10 only".into());
                }
                if panel.get(0x3ffc..0x4000) != Some(&l6fw::STOCK_PANEL_VERSION)
                    || nor.get(0x1f7ffc..0x1f8000) != Some(&l6fw::STOCK_VERSION)
                    || nor.get(0x1f6ffc..0x1f7000) != Some(&l6fw::STOCK_PANEL_VERSION)
                {
                    return Err("persisted firmware version records do not match".into());
                }
                let time = u32::from_le_bytes(saved.get(..4).ok_or("short RTC file")?.try_into()?);
                let calendar =
                    u32::from_le_bytes(saved.get(4..8).ok_or("short RTC file")?.try_into()?);
                if restored {
                    let (before, bank) = self
                        .saved
                        .as_ref()
                        .ok_or("restore assertion needs saved-settings checkpoint")?;
                    if before[4..8] != saved[4..8]
                        || u32::from_le_bytes(before[..4].try_into()?) & 0x003f7f00
                            != time & 0x003f7f00
                    {
                        return Err("RTC fields changed across restart".into());
                    }
                    let mut trace = fs::read_to_string(options.logs.join("engine.trace"))?;
                    if let Ok(panel) = fs::read_to_string(options.logs.join("engine-panel.trace")) {
                        trace.push_str(&panel);
                    }
                    if trace.contains("l6 panel ROM write") {
                        return Err("panel reprogrammed on restart".into());
                    }
                    if !trace.contains(&format!("addr={:08x} ipcr1=000017e0", bank + 4)) {
                        return Err("settings bank was not reloaded".into());
                    }
                    let timestamp = |prefix: &str| -> Option<u32> {
                        trace.lines().find_map(|line| {
                            line.strip_prefix(prefix)?
                                .split_whitespace()
                                .next()?
                                .parse()
                                .ok()
                        })
                    };
                    let request =
                        timestamp("l6 main RTC request at ").ok_or("missing RTC request")?;
                    let response = timestamp("l6 panel RTC response start at ")
                        .ok_or("missing RTC response")?;
                    if response < request || response - request >= 2000 {
                        return Err("RTC response missed startup deadline".into());
                    }
                } else {
                    if calendar & 0x00ff1f3f != 0x00260202 || time & 0x003f7f00 != 0x00010100 {
                        return Err("date editor did not save all five fields".into());
                    }
                    let bank = [0x1f9000usize, 0x1fc000]
                        .into_iter()
                        .find(|&base| {
                            nor.get(base..base + 4).is_some_and(|b| {
                                (1..=3).contains(&u32::from_le_bytes(b.try_into().unwrap()))
                            })
                        })
                        .ok_or("settings bank not committed")?;
                    self.saved = Some((saved.clone(), bank));
                }
                Ok(json!({"restored":restored,"rtc":saved}))
            }
            Operation::SdCard { inserted } => {
                if inserted {
                    session.engine.management[0].insert_sd(
                        options
                            .sd_image
                            .as_ref()
                            .ok_or("SD insertion requires a fixture card")?,
                    )?;
                } else {
                    session.engine.management[0].eject_sd()?;
                }
                Ok(json!({"inserted":inserted}))
            }
            Operation::UsbDescriptors => {
                let usb = session
                    .engine
                    .usb
                    .as_ref()
                    .ok_or("USB backend unavailable")?;
                wait_usb_controller(session, usb, guest_timeout_ms, wall_timeout_s)?;
                let (descriptor, config, configuration) = usb.enumerate_with_descriptors()?;
                self.midi = configuration.midi;
                fs::write(options.logs.join("usb-device.bin"), &descriptor)?;
                fs::write(options.logs.join("usb-config.bin"), &config)?;
                Ok(
                    json!({"vid":u16::from_le_bytes([descriptor[8],descriptor[9]]),"pid":u16::from_le_bytes([descriptor[10],descriptor[11]]),"configuration_bytes":config.len(),"interfaces":config[4]}),
                )
            }
            Operation::UsbMidi { value } => {
                let usb = session
                    .engine
                    .usb
                    .as_ref()
                    .ok_or("USB backend unavailable")?;
                let endpoints = self
                    .midi
                    .ok_or("usb-midi needs a preceding USB MIDI enumeration")?;
                let cc = if session.engine.model() == l6fw::Model::L6 {
                    0x53
                } else {
                    0x51
                };
                let deadline = Instant::now() + Duration::from_secs(wall_timeout_s);
                let start = session
                    .engine
                    .inputs
                    .as_ref()
                    .unwrap()
                    .panel
                    .snapshot()
                    .guest_ms;
                let mut midi = Vec::new();
                loop {
                    session.poll()?;
                    match usb.try_receive(endpoints.input, 16384) {
                        Ok(bytes) => {
                            // The preceding encoder step's channel-1 CC.
                            let matching = l6max_host::usb_midi::decode(&bytes)?
                                .iter()
                                .any(|(_, msg)| msg.starts_with(&[0xb0, cc]));
                            midi.extend_from_slice(&bytes);
                            fs::write(options.logs.join("usb-midi.bin"), &midi)?;
                            if matching {
                                break;
                            }
                        }
                        Err(e) if e.raw_os_error() == Some(libc::EAGAIN) => {}
                        Err(e) => return Err(e.into()),
                    }
                    if Instant::now() > deadline
                        || session
                            .engine
                            .inputs
                            .as_ref()
                            .unwrap()
                            .panel
                            .snapshot()
                            .guest_ms
                            .wrapping_sub(start)
                            > guest_timeout_ms
                    {
                        return Err("USB MIDI receive deadline exceeded".into());
                    }
                    thread::sleep(Duration::from_millis(5));
                }
                fs::write(options.logs.join("usb-midi.bin"), &midi)?;
                // Confirm that MAIN consumed the incoming CC. If the mixer is
                // already at the requested value, first send a different one
                // so the final check cannot succeed on stale state.
                let level_address = match session.engine.model() {
                    l6fw::Model::L6 => l6::mixer_state_tail,
                    l6fw::Model::L6max => l6max::mixer_channel_levels,
                };
                let qmp = &session.engine.management[0];
                let send_and_wait = |level: u8| -> Result<(), Box<dyn std::error::Error>> {
                    const GUEST_TIMEOUT_MS: u32 = 10_000;
                    const WALL_TIMEOUT_S: u64 = 180;
                    const POLL_INTERVAL_MS: u64 = 10;
                    let start = guest_now(session);
                    let deadline =
                        Instant::now() + Duration::from_secs(wall_timeout_s.min(WALL_TIMEOUT_S));
                    usb.send(endpoints.output, &[0x1b, 0xb0, cc, level])?;
                    loop {
                        session.poll()?;
                        if qmp.read_memory(0, level_address, 1)? == [level] {
                            return Ok(());
                        }
                        if guest_now(session).wrapping_sub(start)
                            >= guest_timeout_ms.min(GUEST_TIMEOUT_MS)
                            || Instant::now() >= deadline
                        {
                            return Err(
                                format!("USB MIDI channel level did not become {level}").into()
                            );
                        }
                        thread::sleep(Duration::from_millis(POLL_INTERVAL_MS));
                    }
                };
                if qmp.read_memory(0, level_address, 1)? == [value] {
                    send_and_wait(if value == 0 { 127 } else { 0 })?;
                }
                send_and_wait(value)?;
                Ok(json!({"received":midi,"sent":[0x1b,0xb0,cc,value]}))
            }
            Operation::HostMidiConnect => self.run_declarative_midi(
                session,
                Operation::HostMidiConnect,
                guest_timeout_ms,
                wall_timeout_s,
            ),
            operation @ Operation::EnqueueEncoder {
                midi_transaction: false,
                ..
            } if self.knob_mode.is_some() => self.run_mode_primitive(
                session,
                options,
                operation,
                guest_timeout_ms,
                wall_timeout_s,
            ),
            operation @ (Operation::HostMidiBegin
            | Operation::HostMidiSyncBegin
            | Operation::HostMidiExpectSync
            | Operation::HostMidiDrain { .. }
            | Operation::HostMidiExpectCount { .. }
            | Operation::HostMidiEnd
            | Operation::EnqueueEncoder { .. }
            | Operation::HostMidiExpectCc { .. }
            | Operation::HostMidiSettle { .. }
            | Operation::HostMidiSendCc { .. }
            | Operation::ExpectRingChanged { .. }) => {
                self.run_declarative_midi(session, operation, guest_timeout_ms, wall_timeout_s)
            }
            Operation::HostMidiRoundTrip => {
                self.run_host_midi_round_trip(session, guest_timeout_ms, wall_timeout_s, None, true)
            }
            operation @ (Operation::MidiProbeBegin
            | Operation::MidiProbeSendCc { .. }
            | Operation::MidiProbeExpectRing { .. }
            | Operation::MidiProbeWaitRingStable { .. }
            | Operation::MidiProbeDrainAndTap { .. }
            | Operation::MidiProbeExpectNoEvents { .. }
            | Operation::MidiProbeEnd) => self.run_midi_probe(
                session,
                operation,
                guest_timeout_ms,
                wall_timeout_s,
                operation_start,
            ),
            Operation::DirectUsbMidiRoundTrip {
                cable,
                expect_ring_change,
            } => self.run_host_midi_round_trip(
                session,
                guest_timeout_ms,
                wall_timeout_s,
                Some(cable),
                expect_ring_change,
            ),
            Operation::L6DinUsbForwarding => {
                self.run_l6_din_usb_forwarding(session, wall_timeout_s)
            }
            operation @ (Operation::L6MidiProbeBegin
            | Operation::L6MidiProbeInject
            | Operation::L6MidiProbeExpectHook
            | Operation::L6MidiProbeExpectReplies) => Ok(self.run_l6_midi_probe(
                session,
                options,
                operation,
                guest_timeout_ms,
                wall_timeout_s,
            )?),
            Operation::L6MidiRingPinSweep { cc } => {
                self.run_l6_midi_ring_pin_sweep(session, cc, wall_timeout_s)
            }
            Operation::L6MidiMuteIndicators => {
                self.run_l6_midi_mute_indicators(session, wall_timeout_s)
            }
            Operation::UsbSysex {
                requests,
                complete_on_match,
            } => {
                let usb = session
                    .engine
                    .usb
                    .as_ref()
                    .ok_or("USB backend unavailable")?;
                let endpoints = self
                    .midi
                    .ok_or("usb-sysex needs a preceding USB MIDI enumeration")?;
                let start = session
                    .engine
                    .inputs
                    .as_ref()
                    .unwrap()
                    .panel
                    .snapshot()
                    .guest_ms;
                let deadline = Instant::now() + Duration::from_secs(wall_timeout_s);
                let mut evidence = Vec::new();
                for request in requests {
                    let reply_timeout_ms = request.reply_timeout_ms.unwrap_or(400);
                    let wire = l6max_host::usb_midi::Encoder::default().push(2, &request.bytes);
                    usb.send(endpoints.output, &wire)?;
                    let request_start = session
                        .engine
                        .inputs
                        .as_ref()
                        .unwrap()
                        .panel
                        .snapshot()
                        .guest_ms;
                    let mut pending = Vec::new();
                    let mut replies: Vec<Vec<u8>> = Vec::new();
                    loop {
                        session.poll()?;
                        match usb.try_receive(endpoints.input, 16_384) {
                            Ok(data) => {
                                for (cable, fragment) in l6max_host::usb_midi::decode(&data)? {
                                    if cable != 2 {
                                        continue;
                                    }
                                    for byte in fragment {
                                        if byte == 0xf0 {
                                            pending.clear();
                                        }
                                        if byte == 0xf0 || !pending.is_empty() {
                                            pending.push(byte);
                                            if byte == 0xf7 {
                                                replies.push(std::mem::take(&mut pending));
                                            }
                                        }
                                    }
                                }
                            }
                            Err(e) if e.raw_os_error() == Some(libc::EAGAIN) => {}
                            Err(e) => return Err(e.into()),
                        }
                        let elapsed = session
                            .engine
                            .inputs
                            .as_ref()
                            .unwrap()
                            .panel
                            .snapshot()
                            .guest_ms
                            .wrapping_sub(request_start);
                        if complete_on_match && sysex_reply_complete(&request, &pending, &replies) {
                            break;
                        }
                        if elapsed >= reply_timeout_ms {
                            break;
                        }
                        if Instant::now() > deadline
                            || session
                                .engine
                                .inputs
                                .as_ref()
                                .unwrap()
                                .panel
                                .snapshot()
                                .guest_ms
                                .wrapping_sub(start)
                                > guest_timeout_ms
                        {
                            return Err(format!(
                                "USB SysEx deadline exceeded for request {:02x?}",
                                request.bytes
                            )
                            .into());
                        }
                        thread::sleep(Duration::from_millis(2));
                    }
                    if !pending.is_empty() {
                        return Err(format!("truncated USB SysEx reply: {pending:02x?}").into());
                    }
                    if request.expect_reply && replies.is_empty() {
                        return Err(format!(
                            "missing SysEx reply for request {:02x?}",
                            request.bytes
                        )
                        .into());
                    }
                    if let Some(minimum) = request.min_reply_bytes {
                        if !replies.iter().any(|reply| reply.len() >= minimum) {
                            return Err(format!(
                                "SysEx reply for {:02x?} was shorter than {minimum} bytes: {replies:02x?}",
                                request.bytes
                            )
                            .into());
                        }
                    }
                    if let Some(prefix) = &request.expect_prefix {
                        if !replies
                            .iter()
                            .any(|reply| reply.starts_with(prefix) && reply.last() == Some(&0xf7))
                        {
                            return Err(format!(
                                "missing SysEx reply {prefix:02x?}; got {replies:02x?}"
                            )
                            .into());
                        }
                    } else if !request.expect_reply && !replies.is_empty() {
                        return Err(format!("unexpected SysEx reply: {replies:02x?}").into());
                    }
                    for prefix in &request.reject_prefixes {
                        if replies.iter().any(|reply| reply.starts_with(prefix)) {
                            return Err(format!(
                                "SysEx reply matched rejected prefix {prefix:02x?}: {replies:02x?}"
                            )
                            .into());
                        }
                    }
                    evidence.push(json!({"sent":request.bytes,"replies":replies}));
                }
                Ok(json!({"exchanges":evidence}))
            }
            Operation::EditorPadAssignments => {
                let endpoints = self
                    .midi
                    .ok_or("editor-pad-assignments needs a preceding USB MIDI enumeration")?;
                run_editor_pad_assignments(
                    session,
                    options,
                    endpoints,
                    guest_timeout_ms,
                    wall_timeout_s,
                )
            }
            Operation::EditorPadPlayback { pad } => {
                let endpoints = self
                    .midi
                    .ok_or("editor-pad-playback needs preceding USB MIDI enumeration")?;
                run_editor_pad_playback(
                    session,
                    options,
                    endpoints,
                    pad,
                    guest_timeout_ms,
                    wall_timeout_s,
                )
            }
            Operation::L6maxUiPadVerify { pad } => {
                let endpoints = self
                    .midi
                    .ok_or("l6max-ui-pad-verify needs preceding USB MIDI enumeration")?;
                run_l6max_ui_pad_verify(
                    session,
                    options,
                    endpoints,
                    pad,
                    guest_timeout_ms,
                    wall_timeout_s,
                )
            }
            Operation::WaitL6SoundPadFile { path } => {
                wait_l6_sound_pad_file(session, &path, guest_timeout_ms, wall_timeout_s)
            }
            Operation::SoundPadPlayback {
                pad,
                target,
                assignment,
                require_pad_led_change,
                playback_window_ms,
                require_stereo_meter,
                expect_source,
                max_residual_db,
            } => run_sound_pad_playback(
                session,
                options,
                pad,
                target,
                assignment,
                require_pad_led_change,
                playback_window_ms,
                require_stereo_meter,
                expect_source.as_deref(),
                max_residual_db,
                guest_timeout_ms,
                wall_timeout_s,
            ),
            Operation::L6RecorderPlayback { target } => {
                run_l6_recorder_playback(session, options, target, guest_timeout_ms, wall_timeout_s)
            }
            Operation::L6maxRecorderPlayback { target } => self.run_l6max_recorder_playback(
                session,
                options,
                target,
                guest_timeout_ms,
                wall_timeout_s,
            ),
            Operation::UsbStorage => {
                let usb = session
                    .engine
                    .usb
                    .as_ref()
                    .ok_or("USB backend unavailable")?;
                let configuration = usb.enumerate()?;
                let mut disk = l6max_host::usb_storage::MassStorage::new(
                    usb,
                    configuration
                        .storage
                        .ok_or("firmware did not expose mass storage")?,
                );
                let inquiry = disk.inquiry()?;
                disk.ready()?;
                let (blocks, size) = disk.capacity()?;
                if size != 512 || blocks == 0 {
                    return Err("invalid sector geometry".into());
                }
                let boot = disk.read_sector(0)?;
                use std::io::Read;
                let mut expected = [0; 512];
                fs::File::open(
                    options
                        .sd_image
                        .as_ref()
                        .ok_or("USB storage requires an SD card")?,
                )?
                .read_exact(&mut expected)?;
                if boot != expected {
                    return Err("USB sector did not match attached SD image".into());
                }
                let original = disk.read_sector((blocks - 1) as u32)?;
                disk.write_sector((blocks - 1) as u32, &original)?;
                if disk.read_sector((blocks - 1) as u32)? != original {
                    return Err("USB sector write/read mismatch".into());
                }
                let disk = l6max_host::usb_storage::UsbDisk::new(disk)?;
                let mut volume = sd_image::Volume::open(disk)?;
                let filename = format!("L6T{:05X}.TXT", std::process::id() & 0xfffff);
                let expected = (0..1537)
                    .map(|index| ((index * 37 + 11) % 256) as u8)
                    .collect::<Vec<_>>();
                if volume.exists(&filename) {
                    return Err("USB FAT roundtrip filename already exists".into());
                }
                volume.write(&filename, &expected)?;
                volume.flush()?;
                let actual = volume.read(&filename)?;
                volume.remove(&filename)?;
                if expected != actual {
                    return Err(format!(
                        "USB FAT file roundtrip mismatch: wrote {} bytes, read {} bytes",
                        expected.len(),
                        actual.len()
                    )
                    .into());
                }
                volume.flush()?;
                Ok(
                    json!({"inquiry":String::from_utf8_lossy(&inquiry[8..36]),"blocks":blocks,"sector_size":size,"sector_round_trip":true,"fat_file_round_trip_bytes":expected.len()}),
                )
            }
            Operation::AudioCallbacks => {
                let points = callbacks(session.engine.model());
                let model = session.engine.model();
                let qmp = &session.engine.management[0];
                let mut debugger = l6max_diagnostics::gdb::Gdb::connect(
                    options
                        .gdb_socket
                        .as_ref()
                        .ok_or("missing audio debugger socket")?,
                )?;
                qmp.stop()?;
                for (address, _) in points {
                    debugger.breakpoint(*address, true)?;
                }
                let deadline = Instant::now() + Duration::from_secs(wall_timeout_s);
                let start = session
                    .engine
                    .inputs
                    .as_ref()
                    .unwrap()
                    .panel
                    .snapshot()
                    .guest_ms;
                qmp.resume()?;
                let mut hits: Vec<Value> = Vec::new();
                while hits.len() < points.len() {
                    session.poll()?;
                    if Instant::now() > deadline
                        || session
                            .engine
                            .inputs
                            .as_ref()
                            .unwrap()
                            .panel
                            .snapshot()
                            .guest_ms
                            .wrapping_sub(start)
                            > guest_timeout_ms
                    {
                        let missing: Vec<_> = points
                            .iter()
                            .map(|(_, name)| *name)
                            .filter(|name| !hits.iter().any(|hit| hit["callback"] == *name))
                            .collect();
                        return Err(format!(
                            "audio callback deadline exceeded; not reached: {missing:?}"
                        )
                        .into());
                    }
                    if !qmp.query_status()?.running {
                        let pc = debugger.read_register(15)? & !1;
                        let (_, name) = points
                            .iter()
                            .find(|(a, _)| *a == pc)
                            .ok_or_else(|| format!("unexpected audio stop at {pc:#x}"))?;
                        hits.push(json!({"callback":name,"address":format!("{pc:#010x}"),"r0_r3":(0..4).map(|i| debugger.read_register(i)).collect::<Result<Vec<_>,_>>()?,"lr":debugger.read_register(14)?,"guest_ms":session.engine.inputs.as_ref().unwrap().panel.snapshot().guest_ms}));
                        debugger.breakpoint(pc, false)?;
                        qmp.resume()?;
                    } else {
                        thread::sleep(Duration::from_millis(5));
                    }
                }
                let trace = fs::read_to_string(options.logs.join("engine.trace"))?;
                let dma = verify_sai_edma_trace(model, &trace)?;
                let evidence = json!({"breakpoint_hits":hits,"sai_edma":dma});
                fs::write(
                    options.logs.join("audio-path-smoke.json"),
                    serde_json::to_vec_pretty(&evidence)?,
                )?;
                Ok(evidence)
            }
            Operation::KnobDialogReady => {
                run_knob_dialog_ready(session, options, guest_timeout_ms, wall_timeout_s)
            }
            Operation::KnobDialogGuard => {
                run_knob_dialog_guard(session, options, guest_timeout_ms, wall_timeout_s)
            }
            Operation::KnobDialogBaseline => {
                let frame =
                    run_knob_dialog_baseline(session, options, guest_timeout_ms, wall_timeout_s)?;
                self.knob_dialog_baseline = Some(frame);
                Ok(json!({"baseline_captured": true}))
            }
            Operation::KnobDialogPreflightBegin => {
                self.knob_dialog_preflight = Some(begin_preflight(session, options)?);
                Ok(json!({"preflight": "begin"}))
            }
            operation @ (Operation::KnobDialogPreflightDescriptor
            | Operation::KnobDialogPreflightTextAlias
            | Operation::KnobDialogPreflightLayerCollision
            | Operation::KnobDialogPreflightWidgetLayer
            | Operation::KnobDialogPreflightBackgroundLayer) => {
                let index = match operation {
                    Operation::KnobDialogPreflightDescriptor => 0,
                    Operation::KnobDialogPreflightTextAlias => 1,
                    Operation::KnobDialogPreflightLayerCollision => 2,
                    Operation::KnobDialogPreflightWidgetLayer => 3,
                    _ => 4,
                };
                let context = self
                    .knob_dialog_preflight
                    .as_mut()
                    .ok_or("missing knob-dialog preflight")?;
                run_preflight_probe(session, context, index, wall_timeout_s)
            }
            Operation::KnobDialogCallbackRace => {
                let baseline = self
                    .knob_dialog_baseline
                    .as_ref()
                    .ok_or("missing knob-dialog baseline")?;
                let context = self
                    .knob_dialog_preflight
                    .as_mut()
                    .ok_or("missing knob-dialog preflight")?;
                run_callback_race(
                    session,
                    options,
                    context,
                    baseline,
                    guest_timeout_ms,
                    wall_timeout_s,
                )
            }
            Operation::KnobDialogFirstActivation => {
                let context = self
                    .knob_dialog_preflight
                    .as_mut()
                    .ok_or("missing knob-dialog preflight")?;
                run_first_activation(
                    session,
                    context,
                    guest_timeout_ms,
                    wall_timeout_s,
                    shared.ok_or("missing shared deadline")?,
                )
            }
            Operation::KnobDialogActiveDisplay => {
                let baseline = self
                    .knob_dialog_baseline
                    .as_ref()
                    .ok_or("missing knob-dialog baseline")?;
                let context = self
                    .knob_dialog_preflight
                    .as_mut()
                    .ok_or("missing knob-dialog preflight")?;
                run_active_display(
                    session,
                    options,
                    context,
                    baseline,
                    guest_timeout_ms,
                    wall_timeout_s,
                )
            }
            Operation::KnobDialogLevelDemo => {
                let context = self
                    .knob_dialog_preflight
                    .as_mut()
                    .ok_or("missing knob-dialog preflight")?;
                run_level_demo(session, options, context, guest_timeout_ms, wall_timeout_s)
            }
            Operation::KnobDialogTaskContexts => {
                let context = self
                    .knob_dialog_preflight
                    .as_mut()
                    .ok_or("missing knob-dialog preflight")?;
                run_task_contexts(session, options, context, guest_timeout_ms, wall_timeout_s)
            }
            operation @ (Operation::KnobDialogOverlayInvariants
            | Operation::KnobDialogBitmapZero
            | Operation::KnobDialogActiveOverlay) => {
                let phase = match operation {
                    Operation::KnobDialogOverlayInvariants => OverlayPhase::Invariants,
                    Operation::KnobDialogBitmapZero => OverlayPhase::BitmapZero,
                    Operation::KnobDialogActiveOverlay => OverlayPhase::Refreshed,
                    _ => unreachable!(),
                };
                let context = self
                    .knob_dialog_preflight
                    .as_mut()
                    .ok_or("missing knob-dialog preflight")?;
                run_active_overlay(
                    session,
                    options,
                    context,
                    phase,
                    guest_timeout_ms,
                    wall_timeout_s,
                )
            }
            Operation::KnobDialogLockedUpdate => {
                let context = self
                    .knob_dialog_preflight
                    .as_mut()
                    .ok_or("missing knob-dialog preflight")?;
                run_locked_value_update(session, options, context, guest_timeout_ms, wall_timeout_s)
            }
            operation @ (Operation::KnobDialogModeBegin { .. }
            | Operation::EnqueueTap { .. }
            | Operation::WaitInputApplied
            | Operation::WaitKnobParameterMode { .. }
            | Operation::CaptureDisplay { .. }
            | Operation::ExpectCapturedDisplayImage { .. }
            | Operation::KnobDialogExpectHeap
            | Operation::KnobDialogModeEnd) => self.run_mode_primitive(
                session,
                options,
                operation,
                guest_timeout_ms,
                wall_timeout_s,
            ),
            Operation::KnobDialog => {
                self.knob_heap_reader = None;
                let _baseline = self
                    .knob_dialog_baseline
                    .take()
                    .ok_or("knob-dialog requires baseline frame")?;
                run_knob_dialog(
                    session,
                    options,
                    &mut self.knob_dialog_preflight,
                    guest_timeout_ms,
                    wall_timeout_s,
                    shared,
                    operation_start,
                )
            }
            operation @ (Operation::KnobPopupOpen { .. }
            | Operation::KnobPopupExpectOpen
            | Operation::KnobPopupExpectValue) => {
                self.run_knob_popup(session, operation, guest_timeout_ms, wall_timeout_s)
            }
            Operation::ExpectMenu { menu, path, items } => {
                Ok(expect_menu(session, &menu, &path, &items).map_err(std::io::Error::other)?)
            }
            Operation::FirmwareUpdate => {
                Err("firmware-update scenario must run at the host lifecycle boundary".into())
            }
            Operation::WasmPatch => {
                Err("wasm-patch scenario must run at the host lifecycle boundary".into())
            }
        }
    }
}

#[cfg(test)]
mod sysex_completion_tests {
    use super::sysex_reply_complete;
    use crate::scenario::UsbSysexRequest;

    #[test]
    fn early_completion_requires_a_complete_positive_reply_without_observation_constraints() {
        let mut request = UsbSysexRequest {
            bytes: vec![0xf0, 0xf7],
            expect_prefix: Some(vec![0xf0, 0x52]),
            reject_prefixes: vec![],
            expect_reply: true,
            min_reply_bytes: Some(3),
            reply_timeout_ms: None,
        };
        let reply = vec![vec![0xf0, 0x52, 0xf7]];
        assert!(sysex_reply_complete(&request, &[], &reply));
        assert!(!sysex_reply_complete(&request, &[0xf0], &reply));
        assert!(!sysex_reply_complete(&request, &[], &[vec![0xf0, 0x52]]));
        request.min_reply_bytes = Some(4);
        assert!(!sysex_reply_complete(&request, &[], &reply));
        request.min_reply_bytes = None;
        request.reject_prefixes.push(vec![0xf0, 0x53]);
        assert!(!sysex_reply_complete(&request, &[], &reply));
        request.reject_prefixes.clear();
        request.reply_timeout_ms = Some(500);
        assert!(!sysex_reply_complete(&request, &[], &reply));
        request.reply_timeout_ms = None;
        request.expect_prefix = None;
        assert!(!sysex_reply_complete(&request, &[], &reply));
    }
}

#[cfg(test)]
mod knob_phase_tests {
    use super::*;
    #[test]
    fn shared_budget_carries_through_every_intermediate_phase() {
        let phases = vec![
            Operation::KnobDialogGuard,
            Operation::KnobDialogBaseline,
            Operation::KnobDialogPreflightBegin,
            Operation::KnobDialogPreflightDescriptor,
            Operation::KnobDialogPreflightTextAlias,
            Operation::KnobDialogPreflightLayerCollision,
            Operation::KnobDialogPreflightWidgetLayer,
            Operation::KnobDialogPreflightBackgroundLayer,
            Operation::KnobDialogCallbackRace,
            Operation::KnobDialogFirstActivation,
            Operation::KnobDialogActiveDisplay,
            Operation::KnobDialogLevelDemo,
            Operation::KnobDialogTaskContexts,
            Operation::KnobDialogOverlayInvariants,
            Operation::KnobDialogBitmapZero,
            Operation::KnobDialogActiveOverlay,
            Operation::KnobDialogLockedUpdate,
        ];
        let start = Instant::now();
        let mut shared = Some((start, 17, 180_000, 600, 0));
        let check = |operation: &Operation,
                     scope: Option<&KnobModeScope>,
                     pending: Option<&PendingInput>,
                     next_mode: Option<usize>,
                     shared: &mut Option<KnobDeadline>| {
            let before = shared.expect("deadline lost before the final operation");
            assert_eq!(
                knob_dialog_phase(operation, scope, pending, next_mode),
                Some(before.4)
            );
            *shared = advance_knob_deadline(matches!(operation, Operation::KnobDialog), before);
        };
        for op in &phases {
            check(op, None, None, None, &mut shared);
        }
        for index in 0..3 {
            let begin = Operation::KnobDialogModeBegin {
                scene: format!("scene-{index}"),
            };
            check(&begin, None, None, Some(index), &mut shared);
            let scope = KnobModeScope {
                index,
                scene: format!("scene-{index}"),
                selected_mode: None,
            };
            let tap = PendingInput {
                source: l6ctl::InputSource::Panel,
                applied: 0,
                deadline: start,
                kind: PendingInputKind::Tap,
            };
            let encoder = PendingInput {
                source: l6ctl::InputSource::Panel,
                applied: 0,
                deadline: start,
                kind: PendingInputKind::Encoder,
            };
            let ops = [
                (
                    Operation::EnqueueTap {
                        target: 35,
                        hold_ms: 50,
                    },
                    None,
                ),
                (Operation::WaitInputApplied, Some(&tap)),
                (Operation::WaitKnobParameterMode { mode: 8 }, None),
                (
                    Operation::EnqueueEncoder {
                        channel: 1,
                        steps: 1,
                        midi_transaction: false,
                    },
                    None,
                ),
                (Operation::WaitInputApplied, Some(&encoder)),
                (
                    Operation::CaptureDisplay {
                        guest_ms: 400,
                        artifact: "screen".into(),
                    },
                    None,
                ),
                (
                    Operation::ExpectCapturedDisplayImage {
                        image: Path::new(KNOB_DIALOG_REFERENCES).join("pan.png"),
                    },
                    None,
                ),
                (Operation::KnobDialogExpectHeap, None),
                (Operation::KnobDialogModeEnd, None),
            ];
            for (op, pending) in &ops {
                check(op, Some(&scope), *pending, Some(index), &mut shared);
            }
        }
        check(&Operation::KnobDialog, None, None, Some(3), &mut shared);
        assert!(shared.is_none());
    }
}
