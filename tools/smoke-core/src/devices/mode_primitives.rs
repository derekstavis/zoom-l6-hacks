//! Reusable input, capture and observation primitives for bounded scenarios.
use super::*;

// Embedded at build time so the staged binary's hash identifies the lifecycle
// sequence even if the checkout changes or disappears during the run.
const EMBEDDED_MODE_WALK: &str =
    include_str!("../../../../patches/knob-dialog/tests/emulator/fragments/mode-walk.toml");

const EMBEDDED_MIDI_SYNC: &str =
    include_str!("../../../../patches/midi-sync/tests/emulator/fragments/request-state.toml");

const EMBEDDED_L6_HOOK_PROBE: &str =
    include_str!("../../../../patches/midi-sync/tests/emulator/fragments/l6-hook-probe.toml");

pub(super) fn embedded_l6_hook_operations() -> Result<Vec<Operation>, String> {
    let source = format!(
        "version=1\nname='embedded-l6-hook'\nmodels=['l6']\nbackends=['headless']\n{EMBEDDED_L6_HOOK_PROBE}"
    );
    let scenario = crate::Scenario::parse(&source)
        .map_err(|e| format!("invalid embedded L6 hook fragment: {e}"))?;
    let operations: Vec<_> = scenario
        .step
        .iter()
        .map(|step| match step.action {
            crate::Action::L6MidiProbeBegin => Ok(Operation::L6MidiProbeBegin),
            crate::Action::L6MidiProbeInject => Ok(Operation::L6MidiProbeInject),
            crate::Action::L6MidiProbeExpectHook => Ok(Operation::L6MidiProbeExpectHook),
            crate::Action::L6MidiProbeExpectReplies => Ok(Operation::L6MidiProbeExpectReplies),
            _ => Err("invalid embedded L6 hook action".to_owned()),
        })
        .collect::<Result<_, _>>()?;
    if operations.len() != 4 {
        return Err("embedded L6 hook probe requires a complete transaction".into());
    }
    Ok(operations)
}

pub(super) fn embedded_midi_sync_operations() -> Result<Vec<(String, Operation)>, String> {
    midi_sync_operations(EMBEDDED_MIDI_SYNC)
}

fn midi_sync_operations(fragment: &str) -> Result<Vec<(String, Operation)>, String> {
    // Use the normal scenario parser and semantic validation, including the
    // sync transaction state machine and scalar checks.
    let source = format!(
        "version=1\nname='embedded-midi-sync'\nmodels=['l6max']\nbackends=['headless']\n{fragment}"
    );
    let scenario = crate::Scenario::parse(&source)
        .map_err(|error| format!("invalid embedded MIDI sync fragment: {error}"))?;
    let actions = scenario
        .step
        .iter()
        .map(|step| &step.action)
        .collect::<Vec<_>>();
    if !matches!(
        actions.as_slice(),
        [
            crate::Action::HostMidiSyncBegin,
            crate::Action::HostMidiDrain { .. },
            crate::Action::HostMidiSendCc {
                channel: 0,
                controller: 127,
                value: 127
            },
            crate::Action::HostMidiExpectSync,
            crate::Action::HostMidiEnd
        ]
    ) {
        return Err("embedded MIDI sync must request and assert one complete state reply".into());
    }
    let mut operations = Vec::new();
    for step in &scenario.step {
        if !step.models.is_empty() || step.setup_input {
            return Err("embedded MIDI sync has invalid scope".into());
        }
        let operation = match step.action {
            crate::Action::HostMidiSyncBegin => Operation::HostMidiSyncBegin,
            crate::Action::HostMidiDrain { quiet_wall_ms } => {
                Operation::HostMidiDrain { quiet_wall_ms }
            }
            crate::Action::HostMidiSendCc {
                channel,
                controller,
                value,
            } => Operation::HostMidiSendCc {
                channel,
                controller,
                value,
            },
            crate::Action::HostMidiExpectSync => Operation::HostMidiExpectSync,
            crate::Action::HostMidiEnd => Operation::HostMidiEnd,
            _ => return Err("unsupported embedded MIDI sync action".into()),
        };
        operations.push((crate::progress::step_label(&step.action), operation));
    }
    Ok(operations)
}

pub(super) fn embedded_mode_operations() -> Result<Vec<(String, Operation)>, String> {
    let fragment: crate::Fragment = toml::from_str(EMBEDDED_MODE_WALK)
        .map_err(|error| format!("invalid embedded knob mode fragment: {error}"))?;
    let steps = fragment.step.iter().collect::<Vec<_>>();
    if !crate::scenario::valid_knob_mode_steps(&steps)
        || steps
            .iter()
            .any(|step| !step.models.is_empty() || step.setup_input)
    {
        return Err("embedded knob mode fragment has an invalid sequence or scope".into());
    }
    steps
        .into_iter()
        .map(|step| {
            let operation =
                mode_step_operation(&step.action).ok_or("unsupported embedded knob mode action")?;
            Ok((crate::progress::step_label(&step.action), operation))
        })
        .collect()
}

pub(super) fn embedded_invalid_tag_operations() -> Result<Vec<(String, Operation)>, String> {
    let fragment: crate::Fragment = toml::from_str(include_str!(
        "../../../../patches/knob-dialog/tests/emulator/fragments/invalid-tags.toml"
    ))
    .map_err(|error| format!("invalid embedded invalid-tag fragment: {error}"))?;
    let steps = fragment.step.iter().collect::<Vec<_>>();
    if !crate::scenario::valid_invalid_tag_steps(&steps)
        || steps
            .iter()
            .any(|step| !step.models.is_empty() || step.setup_input)
    {
        return Err("embedded invalid-tag fragment has an invalid sequence or scope".into());
    }
    steps
        .into_iter()
        .map(|step| {
            let operation = match step.action {
                crate::Action::KnobDialogFinalBegin => Operation::KnobDialogFinalBegin,
                crate::Action::KnobDialogInjectInvalidTag { tag } => {
                    Operation::KnobDialogInjectInvalidTag { tag }
                }
                crate::Action::KnobDialogWaitInvalidTag => Operation::KnobDialogWaitInvalidTag,
                crate::Action::KnobDialogConfirmInvalidTag => {
                    Operation::KnobDialogConfirmInvalidTag
                }
                _ => return Err("unsupported embedded invalid-tag action".into()),
            };
            Ok((crate::progress::step_label(&step.action), operation))
        })
        .collect()
}

pub(super) fn embedded_invalid_signature_operations() -> Result<Vec<(String, Operation)>, String> {
    let fragment: crate::Fragment = toml::from_str(include_str!(
        "../../../../patches/knob-dialog/tests/emulator/fragments/invalid-signature.toml"
    ))
    .map_err(|error| format!("invalid embedded signature fragment: {error}"))?;
    let steps = fragment.step.iter().collect::<Vec<_>>();
    if !crate::scenario::valid_invalid_signature_steps(&steps)
        || steps
            .iter()
            .any(|step| !step.models.is_empty() || step.setup_input)
    {
        return Err("embedded signature fragment has invalid sequence or scope".into());
    }
    steps
        .into_iter()
        .map(|step| {
            let operation = match &step.action {
                crate::Action::KnobDialogSettleLevel { artifact } => {
                    Operation::KnobDialogSettleLevel {
                        artifact: artifact.clone(),
                    }
                }
                crate::Action::KnobDialogInjectInvalidSignature => {
                    Operation::KnobDialogInjectInvalidSignature
                }
                crate::Action::KnobDialogObserveInvalidSignature { guest_ms } => {
                    Operation::KnobDialogObserveInvalidSignature {
                        guest_ms: *guest_ms,
                    }
                }
                crate::Action::KnobDialogSampleInvalidSignature => {
                    Operation::KnobDialogSampleInvalidSignature
                }
                crate::Action::KnobDialogRestoreInvalidSignature => {
                    Operation::KnobDialogRestoreInvalidSignature
                }
                _ => return Err("unsupported embedded signature action".into()),
            };
            Ok((crate::progress::step_label(&step.action), operation))
        })
        .collect()
}

pub(super) fn embedded_notification_operations() -> Result<Vec<(String, Operation)>, String> {
    let fragment: crate::Fragment = toml::from_str(include_str!(
        "../../../../patches/knob-dialog/tests/emulator/fragments/notification.toml"
    ))
    .map_err(|error| format!("invalid embedded notification fragment: {error}"))?;
    let steps = fragment.step.iter().collect::<Vec<_>>();
    if !crate::scenario::valid_knob_notification_steps(&steps)
        || steps
            .iter()
            .any(|step| !step.models.is_empty() || step.setup_input)
    {
        return Err("embedded notification fragment has invalid sequence or scope".into());
    }
    steps
        .into_iter()
        .map(|step| {
            let operation = match &step.action {
                crate::Action::KnobDialogPrepareNotification => {
                    Operation::KnobDialogPrepareNotification
                }
                crate::Action::KnobDialogArmNotificationCallback => {
                    Operation::KnobDialogArmNotificationCallback
                }
                crate::Action::KnobDialogWaitNotificationCallback => {
                    Operation::KnobDialogWaitNotificationCallback
                }
                crate::Action::KnobDialogInvokeNotification { message } => {
                    Operation::KnobDialogInvokeNotification { message: *message }
                }
                crate::Action::KnobDialogWaitNotificationFrame {
                    clear,
                    artifact,
                    guest_ms,
                    limit_ms,
                } => Operation::KnobDialogWaitNotificationFrame {
                    clear: *clear,
                    artifact: artifact.clone(),
                    guest_ms: *guest_ms,
                    limit_ms: *limit_ms,
                },
                crate::Action::KnobDialogExpectNotificationInvariants { clear } => {
                    Operation::KnobDialogExpectNotificationInvariants { clear: *clear }
                }
                _ => return Err("unsupported embedded notification action".into()),
            };
            Ok((crate::progress::step_label(&step.action), operation))
        })
        .collect()
}

pub(super) fn embedded_expiry_operations() -> Result<Vec<(String, Operation)>, String> {
    let fragment: crate::Fragment = toml::from_str(include_str!(
        "../../../../patches/knob-dialog/tests/emulator/fragments/expiry.toml"
    ))
    .map_err(|error| format!("invalid embedded expiry fragment: {error}"))?;
    let steps = fragment.step.iter().collect::<Vec<_>>();
    if !crate::scenario::valid_knob_expiry_steps(&steps)
        || steps
            .iter()
            .any(|step| !step.models.is_empty() || step.setup_input)
    {
        return Err("embedded expiry fragment has invalid sequence or scope".into());
    }
    steps
        .into_iter()
        .map(|step| {
            let operation = match &step.action {
                crate::Action::KnobDialogWaitExpiry {
                    interval_ms,
                    limit_ms,
                } => Operation::KnobDialogWaitExpiry {
                    interval_ms: *interval_ms,
                    limit_ms: *limit_ms,
                },
                crate::Action::CaptureDisplay { guest_ms, artifact } => Operation::CaptureDisplay {
                    guest_ms: *guest_ms,
                    artifact: artifact.clone(),
                },
                crate::Action::KnobDialogExpectExpiryFrame { after } => {
                    Operation::KnobDialogExpectExpiryFrame { after: *after }
                }
                _ => return Err("unsupported embedded expiry action".into()),
            };
            Ok((crate::progress::step_label(&step.action), operation))
        })
        .collect()
}

pub(super) fn embedded_restoration_operations() -> Result<Vec<(String, Operation)>, String> {
    let fragment: crate::Fragment = toml::from_str(include_str!(
        "../../../../patches/knob-dialog/tests/emulator/fragments/restoration.toml"
    ))
    .map_err(|error| format!("invalid embedded restoration fragment: {error}"))?;
    let steps = fragment.step.iter().collect::<Vec<_>>();
    if !crate::scenario::valid_knob_restoration_steps(&steps)
        || steps
            .iter()
            .any(|step| !step.models.is_empty() || step.setup_input)
    {
        return Err("embedded restoration fragment has invalid sequence or scope".into());
    }
    steps
        .into_iter()
        .map(|step| {
            let operation = match step.action {
                crate::Action::KnobDialogSnapshotRestoration => {
                    Operation::KnobDialogSnapshotRestoration
                }
                crate::Action::KnobDialogCheckRestoration { field } => {
                    Operation::KnobDialogCheckRestoration { field }
                }
                crate::Action::KnobDialogFinishRestoration => {
                    Operation::KnobDialogFinishRestoration
                }
                crate::Action::KnobDialogExpectRestoredHeap => {
                    Operation::KnobDialogExpectRestoredHeap
                }
                _ => return Err("unsupported embedded restoration action".into()),
            };
            Ok((crate::progress::step_label(&step.action), operation))
        })
        .collect()
}

pub(super) fn embedded_navigation_operations() -> Result<Vec<(String, Operation)>, String> {
    let fragment: crate::Fragment = toml::from_str(include_str!(
        "../../../../patches/knob-dialog/tests/emulator/fragments/navigation.toml"
    ))
    .map_err(|error| format!("invalid embedded navigation fragment: {error}"))?;
    let steps = fragment.step.iter().collect::<Vec<_>>();
    if !crate::scenario::valid_knob_navigation_steps(&steps)
        || steps
            .iter()
            .any(|step| !step.models.is_empty() || step.setup_input)
    {
        return Err("embedded navigation fragment has invalid sequence or scope".into());
    }
    steps
        .into_iter()
        .map(|step| {
            let operation = match &step.action {
                crate::Action::EnqueueEncoderNoAck { channel, steps } => {
                    Operation::EnqueueEncoderNoAck {
                        channel: *channel,
                        steps: *steps,
                    }
                }
                crate::Action::EnqueueTapNoAck { target, hold_ms } => Operation::EnqueueTapNoAck {
                    target: *target,
                    hold_ms: *hold_ms,
                },
                crate::Action::CaptureDisplay { guest_ms, artifact } => Operation::CaptureDisplay {
                    guest_ms: *guest_ms,
                    artifact: artifact.clone(),
                },
                crate::Action::ReleaseCapturedDisplay => Operation::ReleaseCapturedDisplay,
                crate::Action::KnobDialogSnapshotNavigation => {
                    Operation::KnobDialogSnapshotNavigation
                }
                crate::Action::KnobDialogCheckNavigation { field } => {
                    Operation::KnobDialogCheckNavigation { field: *field }
                }
                _ => return Err("unsupported embedded navigation action".into()),
            };
            Ok((crate::progress::step_label(&step.action), operation))
        })
        .collect()
}

/// Convert a mode-fragment action for the post-update lifecycle. Scenario
/// loading validates the complete sequence before this conversion.
pub(super) fn mode_step_operation(action: &crate::Action) -> Option<Operation> {
    Some(match action {
        crate::Action::KnobDialogModeBegin { scene } => Operation::KnobDialogModeBegin {
            scene: scene.clone(),
        },
        crate::Action::EnqueueTap { target, hold_ms } => Operation::EnqueueTap {
            target: *target,
            hold_ms: *hold_ms,
        },
        crate::Action::WaitInputApplied => Operation::WaitInputApplied,
        crate::Action::WaitKnobParameterMode { mode } => {
            Operation::WaitKnobParameterMode { mode: *mode }
        }
        crate::Action::EnqueueEncoder {
            channel,
            steps,
            midi_transaction,
        } => Operation::EnqueueEncoder {
            channel: *channel,
            steps: *steps,
            midi_transaction: *midi_transaction,
        },
        crate::Action::CaptureDisplay { guest_ms, artifact } => Operation::CaptureDisplay {
            guest_ms: *guest_ms,
            artifact: artifact.clone(),
        },
        crate::Action::ExpectCapturedDisplayImage { image } => {
            Operation::ExpectCapturedDisplayImage {
                image: image.clone(),
            }
        }
        crate::Action::KnobDialogExpectHeap => Operation::KnobDialogExpectHeap,
        crate::Action::KnobDialogModeEnd => Operation::KnobDialogModeEnd,
        _ => return None,
    })
}

fn valid_artifact(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

#[derive(Debug, PartialEq, Eq)]
enum InputWait {
    Applied,
    Pending,
    Expired,
}

fn input_wait(applied_before: u64, applied_now: u64, expired: bool) -> InputWait {
    if applied_now > applied_before {
        InputWait::Applied
    } else if expired {
        InputWait::Expired
    } else {
        InputWait::Pending
    }
}

fn mode_operation_matches(operation: &Operation) -> bool {
    matches!(
        operation,
        Operation::EnqueueTap { .. }
            | Operation::WaitInputApplied
            | Operation::WaitKnobParameterMode { .. }
            | Operation::EnqueueEncoder {
                midi_transaction: false,
                ..
            }
            | Operation::CaptureDisplay { .. }
            | Operation::ExpectCapturedDisplayImage { .. }
            | Operation::KnobDialogExpectHeap
            | Operation::KnobDialogModeEnd
    )
}

fn save_captured_display(
    capture: &CapturedDisplay,
    logs: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let native = image::GrayImage::from_fn(128, 64, |x, y| {
        let index = ((63 - y as usize) / 8) * 128 + x as usize;
        image::Luma([
            if capture.frame[index] & (1 << ((63 - y as usize) % 8)) != 0 {
                255
            } else {
                0
            },
        ])
    });
    image::DynamicImage::ImageLuma8(native)
        .resize(768, 384, image::imageops::FilterType::Nearest)
        .save(logs.join(format!("{}.png", capture.artifact)))?;
    Ok(())
}

impl State {
    pub(super) fn release_navigation_capture(
        &mut self,
        _options: &Options,
    ) -> Result<Value, Box<dyn std::error::Error>> {
        let capture = self
            .captured_display
            .as_ref()
            .ok_or("no display capture to save")?;
        let context = self
            .knob_dialog_preflight
            .as_mut()
            .ok_or("missing knob preflight")?;
        if !matches!(final_phase(context), 45 | 48) {
            return Err("navigation save out of order".into());
        }
        let artifact = capture.artifact.clone();
        note_navigation_save(context)?;
        self.captured_display = None;
        Ok(json!({"artifact":artifact}))
    }

    pub(super) fn run_mode_primitive(
        &mut self,
        session: &Session,
        options: &Options,
        operation: Operation,
        guest_timeout_ms: u32,
        wall_timeout_s: u64,
    ) -> Result<Value, Box<dyn std::error::Error>> {
        if let Some(scope) = &self.knob_mode {
            if !mode_operation_matches(&operation) {
                return Err(format!(
                    "unexpected operation in knob-dialog mode {}: {operation:?}",
                    scope.scene
                )
                .into());
            }
        }
        let control = l6ctl::OwnedControl::new(&session.engine);
        match operation {
            Operation::KnobDialogModeBegin { scene } => {
                if self.knob_mode.is_some()
                    || self.pending_input.is_some()
                    || self.captured_display.is_some()
                {
                    return Err("a mode or input/display observation is already active".into());
                }
                if !valid_artifact(&scene) {
                    return Err("invalid mode scene name".into());
                }
                let context = self
                    .knob_dialog_preflight
                    .as_ref()
                    .ok_or("missing knob-dialog preflight")?;
                let index = begin_knob_mode(context)?;
                if index == 0 {
                    self.knob_heap_reader = Some(control.heap_counter_reader()?);
                } else if self.knob_heap_reader.is_none() {
                    return Err("missing guarded heap reader".into());
                }
                self.knob_mode = Some(KnobModeScope {
                    index,
                    scene: scene.clone(),
                    selected_mode: None,
                });
                Ok(json!({"scene":scene}))
            }
            Operation::EnqueueTap { target, hold_ms } => {
                if self.pending_input.is_some() {
                    return Err("input acknowledgement already pending".into());
                }
                let source = l6ctl::button_source(target).ok_or("invalid button target")?;
                let before = control.input_status(source)?;
                control.tap(target, hold_ms)?;
                self.pending_input = Some(PendingInput {
                    source,
                    applied: before.applied,
                    deadline: Instant::now() + Duration::from_secs(10),
                    kind: PendingInputKind::Tap,
                });
                Ok(json!({"target":target, "hold_ms":hold_ms, "applied_before":before.applied}))
            }
            Operation::EnqueueEncoder {
                channel,
                steps,
                midi_transaction: false,
            } => {
                if self.pending_input.is_some() {
                    return Err("input acknowledgement already pending".into());
                }
                let source = l6ctl::InputSource::Panel;
                let before = control.input_status(source)?;
                control.encoder(channel as u32, steps)?;
                self.pending_input = Some(PendingInput {
                    source,
                    applied: before.applied,
                    deadline: Instant::now() + Duration::from_secs(10),
                    kind: PendingInputKind::Encoder,
                });
                Ok(json!({"channel":channel, "steps":steps, "applied_before":before.applied}))
            }
            Operation::WaitInputApplied => {
                let pending = self
                    .pending_input
                    .as_ref()
                    .ok_or("no input acknowledgement pending")?;
                loop {
                    session.poll()?;
                    let status = control.input_status(pending.source)?;
                    match input_wait(pending.applied, status.applied, Instant::now() >= pending.deadline) {
                        InputWait::Applied => {
                            let evidence = json!({"source":pending.source, "applied_before":pending.applied, "applied_after":status.applied});
                            self.pending_input = None;
                            return Ok(evidence);
                        }
                        InputWait::Expired => return Err(format!("timed out waiting for input applied: source={:?}, before={}, observed={}", pending.source, pending.applied, status.applied).into()),
                        InputWait::Pending => {}
                    }
                    thread::sleep(Duration::from_millis(5));
                }
            }
            Operation::WaitKnobParameterMode { mode } => {
                let reader = control.knob_parameter_reader()?;
                let start = control.input_status(l6ctl::InputSource::Panel)?.guest_ms;
                let deadline = Instant::now() + Duration::from_secs(wall_timeout_s.min(180));
                loop {
                    session.poll()?;
                    let actual = reader.parameter_mode()?;
                    if actual == mode {
                        if let Some(scope) = self.knob_mode.as_mut() {
                            scope.selected_mode = Some(mode);
                        }
                        return Ok(json!({"mode":mode}));
                    }
                    let now = control.input_status(l6ctl::InputSource::Panel)?.guest_ms;
                    if now.wrapping_sub(start) >= guest_timeout_ms.min(10_000)
                        || Instant::now() >= deadline
                    {
                        return Err(format!(
                            "parameter mode did not become {mode}; found {actual}"
                        )
                        .into());
                    }
                    thread::sleep(Duration::from_millis(5));
                }
            }
            Operation::CaptureDisplay { guest_ms, artifact } => {
                if guest_ms == 0 || !valid_artifact(&artifact) {
                    return Err("invalid display capture duration or artifact name".into());
                }
                if self.captured_display.is_some() {
                    return Err("captured display has not been checked".into());
                }
                let start = control.input_status(l6ctl::InputSource::Panel)?.guest_ms;
                let wall = Instant::now();
                let frame = loop {
                    let data = session.poll()?;
                    let elapsed = control
                        .input_status(l6ctl::InputSource::Panel)?
                        .guest_ms
                        .wrapping_sub(start);
                    if elapsed >= guest_ms {
                        break data.display;
                    }
                    if elapsed > guest_timeout_ms
                        || wall.elapsed() > Duration::from_secs(wall_timeout_s)
                    {
                        return Err("display capture exceeded its deadline".into());
                    }
                    thread::sleep(Duration::from_millis(5));
                };
                let capture = CapturedDisplay {
                    frame,
                    artifact: artifact.clone(),
                };
                if self.knob_mode.is_none()
                    && self
                        .knob_dialog_preflight
                        .as_ref()
                        .is_some_and(|context| final_phase(context) == 22)
                {
                    save_captured_display(&capture, &options.logs)?;
                    note_expiry_capture(self.knob_dialog_preflight.as_mut().unwrap())?;
                } else if self.knob_mode.is_none()
                    && self
                        .knob_dialog_preflight
                        .as_ref()
                        .is_some_and(|context| matches!(final_phase(context), 44 | 47))
                {
                    save_captured_display(&capture, &options.logs)?;
                    note_navigation_capture(self.knob_dialog_preflight.as_mut().unwrap())?;
                }
                self.captured_display = Some(capture);
                Ok(json!({"guest_ms":guest_ms,"artifact":artifact}))
            }
            Operation::ExpectCapturedDisplayImage { image } => {
                let capture = self
                    .captured_display
                    .as_ref()
                    .ok_or("no display capture to check")?;
                let path = crate::scenario::spec_path(&image)?;
                check_display_reference(&capture.frame, &path, &options.logs, &capture.artifact)?;
                save_captured_display(capture, &options.logs)?;
                let artifact = capture.artifact.clone();
                self.captured_display = None;
                Ok(json!({"image":image,"artifact":artifact}))
            }
            Operation::KnobDialogExpectHeap => {
                let context = self
                    .knob_dialog_preflight
                    .as_ref()
                    .ok_or("missing knob-dialog preflight")?;
                let counters = self
                    .knob_heap_reader
                    .as_ref()
                    .ok_or("missing guarded heap reader")?
                    .read()?;
                expect_mode_heap(context, &counters)?;
                Ok(json!({"allocations":counters.allocations,"frees":counters.frees}))
            }
            Operation::KnobDialogModeEnd => {
                if self.pending_input.is_some() || self.captured_display.is_some() {
                    return Err("mode has unfinished observations".into());
                }
                let scope = self.knob_mode.take().ok_or("no knob-dialog mode active")?;
                let selected = scope
                    .selected_mode
                    .ok_or("mode selector was not observed")?;
                let context = self
                    .knob_dialog_preflight
                    .as_mut()
                    .ok_or("missing knob-dialog preflight")?;
                end_knob_mode(context, selected);
                Ok(json!({"mode":selected,"scene":scope.scene}))
            }
            _ => Err("not a mode observation primitive".into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_midi_sync_reuses_scenario_validation() {
        assert_eq!(midi_sync_operations(EMBEDDED_MIDI_SYNC).unwrap().len(), 5);
        assert!(midi_sync_operations("").is_err());
        assert!(midi_sync_operations("[[step]]\naction='host-midi-end'\n").is_err());
        assert!(
            midi_sync_operations(&EMBEDDED_MIDI_SYNC.replace(
                "action = \"host-midi-end\"",
                "action = \"host-midi-expect-sync\""
            ))
            .is_err()
        );
        assert!(
            midi_sync_operations(
                &EMBEDDED_MIDI_SYNC.replace("quiet_wall_ms = 500", "quiet_wall_ms = 0")
            )
            .is_err()
        );
        assert!(
            midi_sync_operations(
                &EMBEDDED_MIDI_SYNC.replace("controller = 127", "controller = 255")
            )
            .is_err()
        );
    }

    #[test]
    fn embedded_lifecycle_modes_do_not_read_a_source_file() {
        let operations = embedded_mode_operations().unwrap();
        assert_eq!(operations.len(), 100);
        assert!(
            matches!(&operations[0].1, Operation::KnobDialogModeBegin { scene } if scene == "level")
        );
        assert!(matches!(
            &operations[93].1,
            Operation::WaitKnobParameterMode { mode: 8 }
        ));
        assert!(matches!(&operations[99].1, Operation::KnobDialogModeEnd));
    }

    #[test]
    fn acknowledgement_uses_one_applied_event_and_is_bounded() {
        assert_eq!(input_wait(4, 4, false), InputWait::Pending);
        assert_eq!(input_wait(4, 5, false), InputWait::Applied);
        assert_eq!(input_wait(4, 4, true), InputWait::Expired);
        // Preserve the original completion-before-timeout ordering.
        assert_eq!(input_wait(4, 5, true), InputWait::Applied);
    }

    #[test]
    fn scope_admits_only_mode_primitives() {
        assert!(mode_operation_matches(&Operation::EnqueueTap {
            target: 35,
            hold_ms: 50
        }));
        assert!(!mode_operation_matches(&Operation::HostMidiConnect));
        assert!(!valid_artifact("../escape"));
    }

    #[test]
    fn comparison_uses_the_captured_frame_even_if_the_live_frame_changes() {
        let root = std::env::temp_dir().join(format!("l6-captured-image-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let reference = root.join("reference.png");
        image::GrayImage::new(128, 64).save(&reference).unwrap();
        let mut live = [0; 1024];
        let capture = CapturedDisplay {
            frame: live,
            artifact: "captured".into(),
        };
        live[0] = 1;
        assert!(
            check_display_reference(&capture.frame, &reference, &root, &capture.artifact).is_ok()
        );
        assert!(check_display_reference(&live, &reference, &root, "changed").is_err());
        std::fs::remove_dir_all(root).unwrap();
    }
}
