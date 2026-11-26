//! Executing operations against a running session.
use super::*;

fn knob_dialog_phase(
    operation: &Operation,
    scope: Option<&KnobModeScope>,
    pending: Option<&PendingInput>,
    next_mode: Option<usize>,
    final_phase: Option<u8>,
    overlay_capture: bool,
    locked_capture: bool,
    active_display: bool,
    probe_index: Option<usize>,
    task_probe: bool,
    task_capture: bool,
    level_phase: Option<u8>,
    bitmap_capture: Option<u8>,
) -> Option<u16> {
    match operation {
        Operation::KnobBitmapReactivate { .. } => return Some(86),
        Operation::KnobBitmapWaitActive => return Some(87),
        _ => {}
    }
    let fixed = match operation {
        Operation::KnobDialogGuard => Some(0),
        Operation::KnobDialogBaseline => Some(1),
        Operation::KnobDialogPreflightBegin { .. } => Some(2),
        Operation::KnobProbePrepare { probe } => u16::try_from(3 + probe.index() * 6).ok(),
        Operation::KnobProbeInject => probe_index.and_then(|i| u16::try_from(4 + i * 6).ok()),
        Operation::KnobProbeTrigger { .. } => {
            probe_index.and_then(|i| u16::try_from(5 + i * 6).ok())
        }
        Operation::KnobProbeSample => probe_index.and_then(|i| u16::try_from(6 + i * 6).ok()),
        Operation::KnobProbeRestore => probe_index.and_then(|i| u16::try_from(7 + i * 6).ok()),
        Operation::KnobProbeAssert => probe_index.and_then(|i| u16::try_from(8 + i * 6).ok()),
        Operation::KnobRaceBegin => Some(33),
        Operation::KnobRaceWaitRegistration => Some(34),
        Operation::KnobRaceArmCallback => Some(35),
        Operation::KnobRaceWaitCallback => Some(36),
        Operation::KnobRaceCapture { .. } => Some(37),
        Operation::KnobRaceSample => Some(38),
        Operation::KnobRaceRestore => Some(39),
        Operation::KnobRaceAssert => Some(40),
        Operation::KnobActivationTrigger => return Some(41),
        Operation::KnobActivationWait => return Some(42),
        Operation::KnobActivationSample => return Some(43),
        Operation::KnobActivationRestore => return Some(44),
        Operation::KnobActivationAssert => return Some(45),
        Operation::KnobActiveBegin => Some(42),
        Operation::EnqueueEncoderNoAck { .. } if active_display => Some(43),
        Operation::CaptureDisplay { .. } if active_display => Some(44),
        Operation::KnobActiveSampleCallbacks => Some(45),
        Operation::KnobActiveCheckCallbacks => Some(46),
        Operation::KnobActiveCheckFrame => Some(47),
        Operation::KnobActiveFinish => Some(48),
        Operation::KnobLevelBegin => Some(49),
        Operation::KnobLevelStart { .. } => Some(50),
        Operation::KnobLevelSweep { sweep } => Some(match sweep.direction {
            crate::scenario::adaptive::LevelDirection::Up => 51,
            crate::scenario::adaptive::LevelDirection::Down => 53,
        }),
        Operation::CaptureDisplay { .. } if scope.is_none() && level_phase == Some(2) => Some(52),
        Operation::KnobLevelExpectZero => Some(54),
        Operation::CaptureDisplay { .. } if scope.is_none() && level_phase == Some(5) => Some(55),
        Operation::KnobLevelFinish => Some(56),
        Operation::KnobTaskBegin => Some(57),
        Operation::KnobTaskArmHook => Some(58),
        Operation::EnqueueEncoderNoAck { .. } if task_probe => Some(59),
        Operation::KnobTaskWaitHook => Some(60),
        Operation::KnobTaskArmCallback => Some(61),
        Operation::KnobTaskWaitCallback => Some(62),
        Operation::KnobTaskAssertDistinct => Some(63),
        Operation::KnobTaskRelease => Some(64),
        Operation::CaptureDisplay { .. } if task_capture => Some(65),
        Operation::KnobTaskFinish => Some(66),
        Operation::KnobOverlayBegin => Some(67),
        Operation::KnobOverlayCheckWindow => Some(68),
        Operation::KnobOverlaySnapshotDescriptors => Some(69),
        Operation::KnobOverlayFinishDescriptors => Some(75),
        Operation::KnobOverlayCheckResource => Some(76),
        Operation::KnobOverlayCheckCorners => Some(77),
        Operation::KnobOverlayCheckSignature => Some(78),
        Operation::KnobOverlayCheckBitmap => Some(79),
        Operation::KnobOverlayCheckHeap => Some(80),
        Operation::KnobOverlayFinish => Some(81),
        Operation::KnobBitmapBegin => Some(82),
        Operation::KnobBitmapCheckBackground { .. } => Some(83),
        Operation::CaptureDisplay { .. } if bitmap_capture == Some(1) => Some(84),
        Operation::KnobBitmapStoreBefore => Some(85),
        Operation::KnobBitmapArm => Some(86),
        Operation::KnobBitmapWait => Some(87),
        Operation::KnobBitmapInvoke { .. } => Some(88),
        Operation::CaptureDisplay { .. } if bitmap_capture == Some(6) => Some(89),
        Operation::KnobBitmapCheckFrame => Some(90),
        Operation::KnobBitmapCheckSignature => Some(91),
        Operation::KnobBitmapCheckDescriptor => Some(92),
        Operation::KnobBitmapFinish => Some(93),
        Operation::KnobOverlayReactivate { .. } => Some(94),
        Operation::KnobOverlayWaitActive => Some(95),
        Operation::KnobOverlayRefreshBegin => Some(96),
        Operation::KnobOverlayRefreshValue => Some(97),
        Operation::CaptureDisplay { .. } if scope.is_none() && overlay_capture => Some(98),
        Operation::KnobOverlayRefreshPixels => Some(99),
        Operation::KnobOverlayRefreshFinish => Some(100),
        Operation::KnobLockAcquire => Some(101),
        Operation::KnobLockTrigger { .. } => Some(102),
        Operation::KnobLockWait => Some(103),
        Operation::KnobLockSample => Some(104),
        Operation::KnobLockRestore => Some(105),
        Operation::KnobLockAssert => Some(106),
        Operation::CaptureDisplay { .. } if scope.is_none() && locked_capture => Some(107),
        Operation::KnobLockFinish => Some(108),
        Operation::KnobOverlayCheckDescriptor { field } => Some(
            70 + match field {
                crate::scenario::OverlayDescriptorField::Widget7 => 0,
                crate::scenario::OverlayDescriptorField::Widget33 => 1,
                crate::scenario::OverlayDescriptorField::Background10 => 2,
                crate::scenario::OverlayDescriptorField::Background37 => 3,
                crate::scenario::OverlayDescriptorField::Background48 => 4,
            },
        ),
        Operation::KnobDialogModeBegin { .. } => {
            next_mode.and_then(|index| u16::try_from(109 + index * 11).ok())
        }
        Operation::KnobDialogAdaptive { .. }
        | Operation::KnobDialogInjectInvalidTag { .. }
        | Operation::KnobDialogWaitInvalidTag
        | Operation::KnobDialogConfirmInvalidTag
        | Operation::KnobDialogSettleLevel { .. }
        | Operation::KnobDialogInjectInvalidSignature
        | Operation::KnobDialogObserveInvalidSignature { .. }
        | Operation::KnobDialogSampleInvalidSignature
        | Operation::KnobDialogRestoreInvalidSignature
        | Operation::KnobDialogPrepareNotification
        | Operation::KnobDialogArmNotificationCallback
        | Operation::KnobDialogWaitNotificationCallback
        | Operation::KnobDialogInvokeNotification { .. }
        | Operation::KnobDialogWaitNotificationFrame { .. }
        | Operation::KnobDialogExpectNotificationInvariants { .. }
        | Operation::KnobDialogWaitExpiry { .. }
        | Operation::KnobDialogExpectExpiryFrame { .. }
        | Operation::KnobDialogSnapshotRestoration
        | Operation::KnobDialogCheckRestoration { .. }
        | Operation::KnobDialogFinishRestoration
        | Operation::KnobDialogExpectRestoredHeap
        | Operation::EnqueueEncoderNoAck { .. }
        | Operation::EnqueueTapNoAck { .. }
        | Operation::ReleaseCapturedDisplay
        | Operation::KnobDialogSnapshotNavigation
        | Operation::KnobDialogCheckNavigation { .. }
        | Operation::KnobDialog => next_mode
            .and_then(|index| u16::try_from(109 + index * 11 + usize::from(final_phase?)).ok()),
        Operation::CaptureDisplay { .. }
            if scope.is_none() && matches!(final_phase, Some(22 | 44 | 47)) =>
        {
            next_mode
                .and_then(|index| u16::try_from(109 + index * 11 + usize::from(final_phase?)).ok())
        }
        _ => None,
    };
    if let Some(phase) = fixed {
        return Some(phase + u16::from(phase >= 42) * 4 + u16::from(phase >= 82) * 2);
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
        Operation::WaitKnobDisplayImage { .. } => 6,
        Operation::CaptureDisplay { .. } => 7,
        Operation::ExpectCapturedDisplayImage { .. } => 8,
        Operation::KnobDialogExpectHeap => 9,
        Operation::KnobDialogModeEnd => 10,
        _ => return None,
    };
    Some(115 + scope.index as u16 * 11 + offset)
}

type KnobDeadline = (Instant, u32, u32, u64, u16);

fn advance_knob_deadline(is_terminal: bool, shared: KnobDeadline) -> Option<KnobDeadline> {
    if is_terminal {
        None
    } else {
        let (start, guest_start, guest_budget, wall_budget, phase) = shared;
        Some((start, guest_start, guest_budget, wall_budget, phase + 1))
    }
}

fn sysex_deadline_expired(
    elapsed: Duration,
    guest_start: u32,
    guest_now: u32,
    guest_budget: u32,
    wall_budget: u64,
) -> bool {
    elapsed > Duration::from_secs(wall_budget) || guest_now.wrapping_sub(guest_start) > guest_budget
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
    l6ctl::validate_complete_wav(bytes)?;
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
        self.menu_snapshot = None;
        self.effect_ring = None;
        self.effect_state = None;
        self.usb_descriptor_budget = None;
        self.direct_midi = false;
        let auxiliary_cleanup = self.abort_auxiliary_scopes();
        self.ui_pad = None;
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
        let debugger_cleanup = self.abort_knob_preflight();
        self.midi = None;
        self.l6_indicator = None;
        self.sysex_deadline = None;
        self.usb_midi = None;
        match (auxiliary_cleanup, debugger_cleanup) {
            (Ok(()), Ok(())) => Ok(()),
            (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
            (Err(error), Err(debugger)) => {
                Err(format!("{error}; debugger cleanup failed: {debugger}"))
            }
        }
    }
    /// Release held inputs and owned transport scopes before the guest exits.
    pub fn abort_auxiliary_scopes(&mut self) -> Result<(), String> {
        let startup = self
            .usb_startup
            .take()
            .map(|mut scope| scope.abort())
            .transpose();
        let dispatch = self
            .l6_dispatch
            .take()
            .map(|mut scope| scope.abort())
            .transpose();
        let gpio = self
            .gpio_event
            .take()
            .map(|mut scope| scope.abort())
            .transpose();
        let tap = self
            .tap_handler
            .take()
            .map(|mut scope| scope.abort())
            .transpose();
        let din_sweep = self
            .l6_din_sweep
            .take()
            .map(|mut scope| scope.abort())
            .transpose();
        let audio = self
            .audio_callbacks
            .take()
            .map(|mut scope| scope.abort())
            .transpose();
        let power = self
            .power
            .take()
            .map(|mut scope| scope.release())
            .transpose();
        let panel_event = self
            .panel_event
            .take()
            .map(|mut scope| scope.abort())
            .transpose();
        let panel = self
            .panel_keys
            .take()
            .map(|mut scope| scope.release())
            .transpose();
        let meter = self
            .l6_meter_pwm
            .take()
            .map(|mut scope| scope.abort())
            .transpose();
        self.din_usb = None;
        self.sd_inspection = None;
        self.effect_slot_deadline = None;
        self.settings = None;
        self.usb_midi = None;
        self.l6_indicator = None;
        self.ui_pad = None;
        self.editor_playback = None;
        self.editor_assignment = None;
        self.signal_pwm = None;
        let mut errors = Vec::new();
        if let Err(error) = startup {
            errors.push(error);
        }
        if let Err(error) = dispatch {
            errors.push(error);
        }
        if let Err(error) = gpio {
            errors.push(error);
        }
        if let Err(error) = tap {
            errors.push(error);
        }
        if let Err(error) = din_sweep {
            errors.push(error);
        }
        if let Err(error) = audio {
            errors.push(error);
        }
        if let Err(error) = power {
            errors.push(error);
        }
        if let Err(error) = panel_event {
            errors.push(error);
        }
        if let Err(error) = panel {
            errors.push(error);
        }
        if let Err(error) = meter {
            errors.push(error);
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors.join("; "))
        }
    }
    pub fn take_deferred_assertion(&mut self) -> Option<String> {
        self.signal_pwm
            .as_mut()
            .and_then(|scope| scope.take_pending_failure())
    }
    pub fn take_deferred_terminal_failure(&mut self) -> bool {
        std::mem::take(&mut self.deferred_terminal_failure)
    }
    pub fn has_signal_pwm(&self) -> bool {
        self.signal_pwm.is_some()
    }
    pub fn has_l6_sweep(&self) -> bool {
        self.l6_input_sweep.is_some()
    }
    pub fn abort_l6_sweep(&mut self, session: &Session) -> Result<(), String> {
        if let Some(mut scope) = self.l6_input_sweep.take() {
            scope.abort(session)?;
        }
        Ok(())
    }
    pub fn has_editor_assignment(&self) -> bool {
        self.editor_assignment.is_some()
    }
    pub fn has_editor_playback(&self) -> bool {
        self.editor_playback.is_some()
    }
    pub fn has_ui_pad(&self) -> bool {
        self.ui_pad.is_some()
    }
    pub fn has_usb_storage(&self) -> bool {
        self.usb_storage_scope.is_some()
    }
    pub fn abort_usb_storage(&mut self) -> Result<(), String> {
        match self.usb_storage_scope.take() {
            Some(mut context) => context.abort(),
            None => Ok(()),
        }
    }
    pub fn has_l6max_timing(&self) -> bool {
        self.l6max_timing.is_some()
    }
    pub fn abort_l6max_timing(&mut self, session: &Session) -> Result<(), String> {
        match self.l6max_timing.take() {
            Some(mut context) => context.abort(session),
            None => Ok(()),
        }
    }
    pub fn has_l6max_recorder(&self) -> bool {
        self.l6max_recorder.is_some()
    }
    pub fn abort_l6max_recorder(&mut self, session: &Session) -> Result<(), String> {
        match self.l6max_recorder.take() {
            Some(mut context) => context.abort(session),
            None => Ok(()),
        }
    }
    pub fn has_l6_recorder(&self) -> bool {
        self.l6_recorder.is_some()
    }
    pub fn abort_l6_recorder(&mut self, session: &Session) -> Result<(), String> {
        match self.l6_recorder.take() {
            Some(mut context) => context.abort(session),
            None => Ok(()),
        }
    }
    pub fn has_sound_pad(&self) -> bool {
        self.sound_pad.is_some()
    }
    pub fn abort_sound_pad(&mut self, session: &Session) -> Result<(), String> {
        match self.sound_pad.take() {
            Some(mut context) => {
                audio::sound_pad::abort(&mut context, session).map_err(|e| e.to_string())
            }
            None => Ok(()),
        }
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
    pub fn abort_usb_midi(&mut self) {
        self.usb_midi = None;
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
        self.overlay_reader = None;
        self.task_reader = None;
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
        if self.effect_slot_deadline.is_some()
            && !matches!(operation, Operation::CaptureEffectState { .. })
        {
            self.effect_slot_deadline = None;
            return Err("effect slot readiness must be followed by its state assertion".into());
        }
        if let Some(scope) = self.signal_pwm.as_ref() {
            let valid = match operation {
                Operation::SignalPwmWarmup {
                    start: 0,
                    end: 4,
                    value,
                } => scope.phase == 0 && value.is_finite() && value > 0.0 && value <= 40_000_000.0,
                Operation::SignalPwmDrain { band } => scope.phase == 1 && band == scope.band,
                Operation::SignalPwmStage {
                    band,
                    start,
                    end,
                    value,
                } => {
                    scope.phase == 2
                        && band == scope.band
                        && (start, end) == if band == 0 { (0, 4) } else { (4, 12) }
                        && value.is_finite()
                        && value > 0.0
                        && value <= 40_000_000.0
                }
                Operation::SignalPwmAssert { band, channel } => {
                    scope.phase == 3
                        && band == scope.band
                        && channel == band * 4 + scope.assertions as u8
                }
                Operation::SignalPwmEnd => scope.phase == 4,
                _ => false,
            };
            if !valid {
                self.signal_pwm = None;
                return Err("SIGNAL PWM interrupted or out of order".into());
            }
            if let Err(error) = scope.check(session, guest_timeout_ms, wall_timeout_s) {
                self.signal_pwm = None;
                return Err(error);
            }
        }
        if let Some(scope) = self.l6_input_sweep.as_ref() {
            let expected = match operation {
                Operation::L6SweepRejectMain { target: 0 } => Some(0),
                Operation::L6SweepRejectMain { target: 5 } => Some(1),
                Operation::L6SweepPanelBaseline { .. } | Operation::L6SweepEncoderBaseline => {
                    Some(2)
                }
                Operation::L6SweepPanelTap => Some(3),
                Operation::L6SweepPanelWait => Some(4),
                Operation::L6SweepPanelTrace => Some(5),
                Operation::L6SweepPanelAssert => Some(6),
                Operation::L6SweepEncoderSend { .. } | Operation::L6SweepEncoderTrace => Some(7),
                Operation::L6SweepEncoderWait => Some(8),
                Operation::L6SweepEncoderAssert => Some(9),
                Operation::L6SweepRejectPanel => Some(10),
                Operation::L6SweepRejectEncoder => Some(11),
                Operation::L6SweepEnd => Some(12),
                _ => None,
            };
            if expected != Some(scope.phase) {
                return Err("L6 input sweep interrupted or out of order".into());
            }
            scope.check(session, guest_timeout_ms, wall_timeout_s)?;
        }
        if let Some(scope) = self.editor_assignment.as_ref() {
            let expected = match operation {
                Operation::EditorAssignIdentity => Some((0, None)),
                Operation::EditorAssignGlobal => Some((1, None)),
                Operation::EditorAssignVersion => Some((2, None)),
                Operation::EditorAssignSdInfo => Some((3, None)),
                Operation::EditorAssignRefresh => Some((4, None)),
                Operation::EditorAssignCount { pad } => Some((5, Some(pad))),
                Operation::EditorAssignFilename { pad } => Some((6, Some(pad))),
                Operation::EditorAssignSend { pad } => Some((7, Some(pad))),
                Operation::EditorAssignDialog { pad } => Some((8, Some(pad))),
                Operation::EditorAssignState { pad } => Some((9, Some(pad))),
                Operation::EditorAssignAssert { pad } => Some((10, Some(pad))),
                Operation::EditorAssignEnd => Some((11, None)),
                _ => None,
            };
            if !expected.is_some_and(|(phase, pad)| {
                phase == scope.phase && pad.is_none_or(|pad| pad == scope.pad)
            }) {
                self.editor_assignment = None;
                return Err("Editor assignment interrupted or wrong pad".into());
            }
            if let Err(error) = scope.check(session, guest_timeout_ms, wall_timeout_s) {
                self.editor_assignment = None;
                return Err(error.to_string());
            }
        }
        if let Some(scope) = self.editor_playback.as_ref() {
            let expected = match operation {
                Operation::EditorPlaybackPlay => Some(0),
                Operation::EditorPlaybackWait => Some(1),
                Operation::EditorPlaybackObserve => Some(2),
                Operation::EditorPlaybackSample => Some(3),
                Operation::EditorPlaybackAssert => Some(4),
                Operation::EditorPlaybackEnd => Some(5),
                _ => None,
            };
            if expected != Some(scope.phase) {
                self.editor_playback = None;
                return Err("Editor playback interrupted by another operation".into());
            }
            if let Err(error) = scope.check(session, guest_timeout_ms, wall_timeout_s) {
                self.editor_playback = None;
                return Err(error.to_string());
            }
        }
        if let Some(scope) = self.ui_pad.as_ref() {
            let expected = match operation {
                Operation::UiPadIdentity => Some(0),
                Operation::UiPadGlobal => Some(1),
                Operation::UiPadKeepAlive => Some(2),
                Operation::UiPadQuery => Some(3),
                Operation::UiPadAssert => Some(4),
                Operation::UiPadEnd => Some(5),
                _ => None,
            };
            if expected != Some(scope.phase) {
                self.ui_pad = None;
                return Err("UI pad verification interrupted by another operation".into());
            }
            if let Err(error) = scope.check(session, guest_timeout_ms, wall_timeout_s) {
                self.ui_pad = None;
                return Err(error.to_string());
            }
        }
        if self.l6_indicator.is_some()
            && !matches!(
                operation,
                Operation::L6RingSample { .. }
                    | Operation::L6MuteSample { .. }
                    | Operation::L6RingCoverage { .. }
                    | Operation::L6IndicatorEnd
            )
        {
            self.l6_indicator = None;
            return Err("L6 indicator campaign interrupted by another operation".into());
        }
        if let Some(scope) = &self.l6_indicator {
            if indicator_deadline_expired(
                scope.start.elapsed(),
                scope.guest_start,
                guest_now(session),
                scope.guest_budget,
                scope.wall_budget,
            ) {
                self.l6_indicator = None;
                return Err("L6 indicator campaign exceeded its shared deadline".into());
            }
        }
        if let Some(context) = self.usb_storage_scope.as_ref() {
            let expected = match operation {
                Operation::UsbStorageInquiry => Some(0),
                Operation::UsbStorageReady => Some(1),
                Operation::UsbStorageCapacity => Some(2),
                Operation::UsbStorageReadBoot => Some(3),
                Operation::UsbStorageAssertBoot => Some(4),
                Operation::UsbStorageLastRead => Some(5),
                Operation::UsbStorageLastWrite => Some(6),
                Operation::UsbStorageLastReadback => Some(7),
                Operation::UsbStorageLastAssert => Some(8),
                Operation::UsbStorageFatOpen => Some(9),
                Operation::UsbStorageCreate { .. } => Some(10),
                Operation::UsbStorageFlushCreated => Some(11),
                Operation::UsbStorageReadFile => Some(12),
                Operation::UsbStorageRemoveFile => Some(13),
                Operation::UsbStorageAssertFile => Some(14),
                Operation::UsbStorageFlushRemoved => Some(15),
                Operation::UsbStorageEnd => Some(16),
                _ => None,
            };
            if expected != Some(context.phase)
                || context
                    .check(session, guest_timeout_ms, wall_timeout_s)
                    .is_err()
            {
                let error = "USB storage interrupted or exceeded shared deadline";
                return match self.abort_usb_storage() {
                    Ok(()) => Err(error.into()),
                    Err(cleanup) => Err(format!("{error}; cleanup failed: {cleanup}")),
                };
            }
        }
        if let Some(context) = self.l6max_timing.as_ref() {
            let expected = match operation {
                Operation::L6maxTimingSettle { .. } => Some(0),
                Operation::L6maxTimingBaseline => Some(1),
                Operation::L6maxTimingEnqueue { .. } => Some(2),
                Operation::L6maxTimingWaitFirst => Some(3),
                Operation::L6maxTimingObservePaused { .. } => Some(4),
                Operation::L6maxTimingCapturePaused => Some(5),
                Operation::L6maxTimingAssertPausedClock => Some(6),
                Operation::L6maxTimingAssertPausedInput => Some(7),
                Operation::L6maxTimingResume => Some(8),
                Operation::L6maxTimingWaitQueued => Some(9),
                Operation::L6maxTimingWaitReports => Some(10),
                Operation::L6maxTimingAssertReports => Some(11),
                Operation::L6maxTimingObserveDisplay { .. } => Some(12),
                Operation::L6maxTimingAssertDisplay => Some(13),
                Operation::L6maxTimingEncoder { .. } => Some(14),
                Operation::L6maxTimingWaitEncoder => Some(15),
                Operation::L6maxTimingWaitEncoderReport => Some(16),
                Operation::L6maxTimingAssertEncoder => Some(17),
                Operation::L6maxTimingEnd => Some(18),
                _ => None,
            };
            if expected != Some(context.phase)
                || context
                    .check(session, guest_timeout_ms, wall_timeout_s)
                    .is_err()
            {
                let error = "L6max timing interrupted or exceeded shared deadline";
                return match self.abort_l6max_timing(session) {
                    Ok(()) => Err(error.into()),
                    Err(cleanup) => Err(format!("{error}; cleanup failed: {cleanup}")),
                };
            }
        }
        if let Some(context) = self.l6max_recorder.as_ref() {
            let expected = match operation {
                Operation::L6maxRecorderPress => Some(0),
                Operation::L6maxRecorderSubmit => Some(1),
                Operation::L6maxRecorderQueue => Some(2),
                Operation::L6maxRecorderDispatch => Some(3),
                Operation::L6maxRecorderScheduler => Some(4),
                Operation::L6maxRecorderCallback => Some(5),
                Operation::L6maxRecorderAction => Some(6),
                Operation::L6maxRecorderStream => Some(7),
                Operation::L6maxRecorderObserve => Some(8),
                Operation::L6maxRecorderSample => Some(9),
                Operation::L6maxRecorderEvidence => Some(10),
                Operation::L6maxRecorderAssertAudio => Some(11),
                Operation::L6maxRecorderEnd => Some(12),
                _ => None,
            };
            if expected != Some(context.phase)
                || context
                    .check(session, guest_timeout_ms, wall_timeout_s)
                    .is_err()
            {
                let error = "L6max recorder interrupted or exceeded its shared deadline";
                return match self.abort_l6max_recorder(session) {
                    Ok(()) => Err(error.into()),
                    Err(cleanup) => Err(format!("{error}; cleanup failed: {cleanup}")),
                };
            }
        }
        if let Some(context) = self.l6_recorder.as_ref() {
            let expected = match operation {
                Operation::L6RecorderPress => Some(0),
                Operation::L6RecorderDispatcher => Some(1),
                Operation::L6RecorderAssertEvent => Some(2),
                Operation::L6RecorderHandler => Some(3),
                Operation::L6RecorderAssertSelection => Some(4),
                Operation::L6RecorderCallback => Some(5),
                Operation::L6RecorderAssertCallback => Some(6),
                Operation::L6RecorderObserve => Some(7),
                Operation::L6RecorderRelease => Some(8),
                Operation::L6RecorderSample => Some(9),
                Operation::L6RecorderEvidence => Some(10),
                Operation::L6RecorderAssertTrace => Some(11),
                Operation::L6RecorderAssertDma => Some(12),
                Operation::L6RecorderEnd => Some(13),
                _ => None,
            };
            if expected != Some(context.phase)
                || context
                    .check(session, guest_timeout_ms, wall_timeout_s)
                    .is_err()
            {
                let error = "recorder transaction interrupted or exceeded its shared deadline";
                return match self.abort_l6_recorder(session) {
                    Ok(()) => Err(error.into()),
                    Err(cleanup) => Err(format!("{error}; cleanup failed: {cleanup}")),
                };
            }
        }
        if self.sound_pad.is_some() {
            let expected = match operation {
                Operation::SoundPadPress => Some(0),
                Operation::SoundPadHold => Some(1),
                Operation::SoundPadRelease => Some(2),
                Operation::SoundPadObserve => Some(3),
                Operation::SoundPadSample => Some(4),
                Operation::SoundPadAssertWaveform => Some(5),
                Operation::SoundPadAssertAudio => Some(6),
                Operation::SoundPadAssertLed => Some(7),
                Operation::SoundPadAssertMeter => Some(8),
                Operation::SoundPadFinish => Some(9),
                _ => None,
            };
            let context = self.sound_pad.as_ref().unwrap();
            if expected != Some(context.phase)
                || (guest_timeout_ms, wall_timeout_s) != (context.guest_budget, context.wall_budget)
            {
                let mut context = self.sound_pad.take().unwrap();
                let cleanup = audio::sound_pad::abort(&mut context, session);
                return Err(match cleanup {
                    Ok(()) => "sound-pad transaction interrupted or changed budget".into(),
                    Err(error) => format!(
                        "sound-pad transaction interrupted or changed budget; release failed: {error}"
                    ),
                });
            }
        }
        if self.sd_inspection.is_some()
            && !matches!(
                operation,
                Operation::CaptureSdModule { .. }
                    | Operation::ExpectSdAllocation { .. }
                    | Operation::ExpectSdChains { .. }
                    | Operation::ExpectSdContinuations { .. }
                    | Operation::ExpectSdModuleImages { .. }
                    | Operation::ExpectSdLatestModule { .. }
            )
        {
            self.sd_inspection = None;
            return Err("SD inspection interrupted before module assertion".into());
        }
        if let Some(context) = &self.sd_inspection {
            if (context.guest_timeout_ms, context.wall_timeout_s)
                != (guest_timeout_ms, wall_timeout_s)
                || Instant::now() >= context.wall_deadline
                || guest_now(session).wrapping_sub(context.guest_start) >= context.guest_timeout_ms
            {
                self.sd_inspection = None;
                return Err("SD inspection exceeded its shared deadline or changed budgets".into());
            }
        }
        let sd_shared = self.sd_inspection.as_ref().map(|context| {
            (
                context.wall_deadline,
                context.guest_start,
                context.guest_timeout_ms,
            )
        });
        if self.sysex_deadline.is_some()
            && !matches!(
                operation,
                Operation::UsbSysexExchange { .. } | Operation::UsbSysexEnd
            )
        {
            self.sysex_deadline = None;
            return Err("USB SysEx campaign interrupted by another operation".into());
        }
        if self.l6_midi_probe.is_some()
            && !matches!(
                operation,
                Operation::L6MidiProbeInject
                    | Operation::L6MidiProbeExpectHook
                    | Operation::L6MidiProbeExpectReplies
                    | Operation::L6MidiProbeFinish
            )
        {
            self.l6_midi_probe.take();
            return Err("L6 MIDI probe interrupted by another operation".into());
        }
        if self.din_usb.is_some()
            && !matches!(
                operation,
                Operation::DinUsbAttach
                    | Operation::DinUsbSend { .. }
                    | Operation::DinUsbExpect { .. }
                    | Operation::DinUsbEnd
            )
        {
            self.din_usb.take();
            return Err("DIN USB forwarding scope interrupted".into());
        }
        if self.panel_keys.is_some()
            && !matches!(
                operation,
                Operation::L6maxPanelKeyCheck { .. }
                    | Operation::L6maxPanelKeyInput { .. }
                    | Operation::L6maxPanelKeyEnd
            )
        {
            self.panel_keys.take();
            return Err("panel key scope interrupted by another operation".into());
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
            self.knob_dialog_preflight.as_ref().map(final_phase),
            self.knob_dialog_preflight
                .as_ref()
                .is_some_and(expecting_overlay_refresh),
            self.knob_dialog_preflight
                .as_ref()
                .is_some_and(expecting_locked_capture),
            self.knob_dialog_preflight
                .as_ref()
                .is_some_and(expecting_active_display),
            self.knob_dialog_preflight
                .as_ref()
                .map(preflight_probe_index),
            self.knob_dialog_preflight
                .as_ref()
                .is_some_and(expecting_task_probe),
            self.knob_dialog_preflight
                .as_ref()
                .is_some_and(expecting_task_capture),
            self.knob_dialog_preflight.as_ref().and_then(level_phase),
            self.knob_dialog_preflight
                .as_ref()
                .and_then(bitmap_capture_phase),
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
        if let Some(context) = self.sound_pad.as_ref() {
            if let Err(error) = audio::sound_pad::check(context, session) {
                let mut context = self.sound_pad.take().unwrap();
                return match audio::sound_pad::abort(&mut context, session) {
                    Ok(()) => Err(error.to_string()),
                    Err(cleanup) => Err(format!("{error}; sound-pad release failed: {cleanup}")),
                };
            }
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
                let mut primary = error.to_string();
                if let Some(mut context) = self.sound_pad.take() {
                    if let Err(cleanup) = audio::sound_pad::abort(&mut context, session) {
                        primary = format!("{primary}; sound-pad release failed: {cleanup}");
                    }
                }
                self.l6_indicator = None;
                self.sysex_deadline = None;
                self.abort_usb_midi();
                return match self.abort_knob_preflight() {
                    Ok(()) => Err(primary),
                    Err(cleanup) => Err(format!("{primary}; debugger cleanup failed: {cleanup}")),
                };
            }
        };
        if let Some(context) = self.sound_pad.as_ref() {
            if let Err(error) = audio::sound_pad::check(context, session) {
                let mut context = self.sound_pad.take().unwrap();
                return match audio::sound_pad::abort(&mut context, session) {
                    Ok(()) => Err(error.to_string()),
                    Err(cleanup) => Err(format!("{error}; sound-pad release failed: {cleanup}")),
                };
            }
        }
        if let Some((wall_deadline, guest_start, guest_budget)) = sd_shared {
            if Instant::now() >= wall_deadline
                || guest_now(session).wrapping_sub(guest_start) >= guest_budget
            {
                return Err("SD inspection exceeded its shared deadline".into());
            }
        }
        let elapsed = match control.input_status(l6ctl::InputSource::Panel) {
            Ok(status) => status.guest_ms.wrapping_sub(guest_start),
            Err(error) => {
                if let Some(mut context) = self.sound_pad.take() {
                    return match audio::sound_pad::abort(&mut context, session) {
                        Ok(()) => Err(error.to_string()),
                        Err(cleanup) => {
                            Err(format!("{error}; sound-pad release failed: {cleanup}"))
                        }
                    };
                }
                return Err(error.to_string());
            }
        };
        if started.elapsed() > Duration::from_secs(wall_timeout_s) || elapsed > guest_timeout_ms {
            let mut error = "device operation exceeded its step deadline".to_owned();
            if let Some(mut context) = self.sound_pad.take() {
                if let Err(cleanup) = audio::sound_pad::abort(&mut context, session) {
                    error = format!("{error}; sound-pad release failed: {cleanup}");
                }
            }
            return match self.abort_knob_preflight() {
                Ok(()) => Err(error),
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
        shared: Option<(Instant, u32, u32, u64, u16)>,
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
                let mode = l6ctl::OwnedControl::new(&session.engine)
                    .read_main_memory(l6max::system_mode, 4)?
                    .bytes;
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
                let actual = l6ctl::OwnedControl::new(&session.engine)
                    .read_symbol(&symbol, bytes.len())?
                    .bytes;
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
                    let actual = l6ctl::OwnedControl::new(&session.engine)
                        .read_symbol_before(&symbol, bytes.len(), deadline)?
                        .bytes;
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
            Operation::EffectPacedTurn { channel, steps } => Ok(effect_paced_turn(
                options,
                session,
                channel,
                steps,
                guest_timeout_ms,
                wall_timeout_s,
                self.paced_progress.clone(),
                self.paced_index,
            )
            .map_err(std::io::Error::other)?),
            Operation::CaptureEffectRing { encoder }
            | Operation::ExpectEffectRingChanged { encoder } => {
                let deadline = Instant::now() + Duration::from_secs(wall_timeout_s);
                let capturing = matches!(operation, Operation::CaptureEffectRing { .. });
                let (reader, baseline) = if capturing {
                    (
                        effect_slot_reader(options, session, deadline, &|| Ok(()))?,
                        None,
                    )
                } else {
                    let (prior_encoder, prior_effect, before, reader) = self
                        .effect_ring
                        .take()
                        .ok_or("effect ring baseline missing")?;
                    if prior_encoder != encoder {
                        return Err("effect ring baseline does not match the encoder".into());
                    }
                    (reader, Some((prior_effect, before)))
                };
                let fields = l6ctl::EffectSlotFields {
                    ring_index: Some(encoder - 1),
                    ..Default::default()
                };
                let snapshot = reader.snapshot_before_checked(&fields, deadline, &|| Ok(()))?;
                if !(1..=2).contains(&snapshot.effect_type) || snapshot.edit_mode == 0 {
                    return Err(
                        "effect ring observation requires an active Room or Spring editor".into(),
                    );
                }
                if encoder > if snapshot.effect_type == 1 { 8 } else { 7 } {
                    return Err("encoder is unavailable for the selected effect".into());
                }
                let value = snapshot
                    .ring_position
                    .ok_or("effect ring position unavailable")?;
                if let Some((effect_type, before)) = baseline {
                    if effect_type != snapshot.effect_type {
                        return Err(
                            "effect ring baseline belongs to a different effect slot".into()
                        );
                    }
                    if value == before {
                        return Err(format!("encoder {encoder} edit left its ring unchanged at Q16.16 position {value}").into());
                    }
                    Ok(json!({"encoder":encoder,"before":before,"after":value}))
                } else {
                    self.effect_ring = Some((encoder, snapshot.effect_type, value, reader));
                    Ok(json!({"encoder":encoder,"position":value}))
                }
            }
            Operation::CaptureEffectState { edits, dialog } => {
                if self.effect_state.is_some() {
                    return Err("effect state snapshot already active".into());
                }
                let prior = self.effect_slot_deadline.take();
                let (started, guest_start, guest_budget, wall_budget) = prior
                    .as_ref()
                    .map(|scope| {
                        (
                            scope.start,
                            scope.guest_start,
                            scope.guest_budget,
                            scope.wall_budget,
                        )
                    })
                    .unwrap_or((
                        operation_start.0,
                        operation_start.1,
                        guest_timeout_ms,
                        wall_timeout_s,
                    ));
                if (guest_budget, wall_budget) != (guest_timeout_ms, wall_timeout_s) {
                    return Err("effect slot assertion changed its shared budget".into());
                }
                let deadline = started + Duration::from_secs(wall_budget);
                let check = || -> Result<(), String> {
                    if Instant::now() >= deadline
                        || guest_now(session).wrapping_sub(guest_start) > guest_budget
                    {
                        Err("effect state snapshot exceeded its deadline".into())
                    } else {
                        Ok(())
                    }
                };
                let reader = match prior {
                    Some(scope) => scope.reader,
                    None => effect_slot_reader(options, session, deadline, &check)?,
                };
                let snapshot = reader.snapshot_before_checked(
                    &l6ctl::EffectSlotFields {
                        applied: edits,
                        dialog_active: dialog,
                        ..Default::default()
                    },
                    deadline,
                    &check,
                )?;
                let evidence = json!({"effect_type":snapshot.effect_type,"processor_type":snapshot.processor_type,"active_effect":"","nonzero_output_blocks":0,"panel_mode":snapshot.panel_mode,"edit_mode":snapshot.edit_mode,"parameter_edits_applied":snapshot.applied.unwrap_or(0),"parameter_id":null,"parameter_value":null,"knob_dialog_active":snapshot.dialog_active.unwrap_or(0),"exit_ring_refreshes":0,"atomic":false,"fixture_module_sha256":snapshot.fixture_module_sha256});
                self.effect_state = Some((
                    snapshot,
                    reader,
                    started,
                    guest_start,
                    guest_budget,
                    wall_budget,
                ));
                Ok(evidence)
            }
            Operation::ExpectEffectName { .. }
            | Operation::ExpectEffectOutput
            | Operation::ExpectEffectEdits { .. }
            | Operation::ExpectEffectDialog { .. }
            | Operation::ExpectEffectType { .. }
            | Operation::ExpectEffectProcessor { .. }
            | Operation::ExpectEffectPanelMode { .. }
            | Operation::ExpectEffectEditMode { .. } => {
                let (snapshot, reader, started, guest_start, guest_budget, wall_budget) = self
                    .effect_state
                    .as_ref()
                    .ok_or("effect state snapshot missing")?;
                if (guest_timeout_ms, wall_timeout_s) != (*guest_budget, *wall_budget)
                    || started.elapsed() >= Duration::from_secs(*wall_budget)
                    || guest_now(session).wrapping_sub(*guest_start) > *guest_budget
                {
                    return Err(
                        "effect state assertion exceeded its shared deadline or changed budget"
                            .into(),
                    );
                }
                match operation {
                    Operation::ExpectEffectName { ref name } => {
                        let deadline = *started + Duration::from_secs(*wall_budget);
                        let check = || -> Result<(), String> {
                            if Instant::now() >= deadline
                                || guest_now(session).wrapping_sub(*guest_start) > *guest_budget
                            {
                                Err("effect state assertion exceeded its shared deadline".into())
                            } else {
                                Ok(())
                            }
                        };
                        let later = reader.snapshot_before_checked(
                            &l6ctl::EffectSlotFields {
                                active_effect: true,
                                ..Default::default()
                            },
                            deadline,
                            &check,
                        )?;
                        let actual = later.active_effect.unwrap_or_default();
                        if &actual != name {
                            return Err(format!(
                                "effect replacement runs {actual:?}, expected {name:?}"
                            )
                            .into());
                        }
                        Ok(json!({"active_effect":actual}))
                    }
                    Operation::ExpectEffectOutput => {
                        let processor = snapshot.processor_type;
                        if !matches!(processor, 1 | 2) {
                            return Err(
                                "effect replacement output check needs a replaced slot selected"
                                    .into(),
                            );
                        }
                        let deadline = *started + Duration::from_secs(*wall_budget);
                        let check = || -> Result<(), String> {
                            if Instant::now() >= deadline
                                || guest_now(session).wrapping_sub(*guest_start) > *guest_budget
                            {
                                Err("effect state assertion exceeded its shared deadline".into())
                            } else {
                                Ok(())
                            }
                        };
                        let later = reader.snapshot_before_checked(
                            &l6ctl::EffectSlotFields {
                                output: true,
                                output_processor: Some(processor as u8),
                                ..Default::default()
                            },
                            deadline,
                            &check,
                        )?;
                        let value = later.output_blocks.unwrap_or(0);
                        if value == 0 {
                            return Err(format!("effect replacement has produced no nonzero output in slot {processor}").into());
                        }
                        Ok(json!({"nonzero_output_blocks":value}))
                    }
                    Operation::ExpectEffectEdits { minimum } => {
                        let applied = snapshot
                            .applied
                            .ok_or("effect edit count was not captured")?;
                        if applied < minimum {
                            return Err(format!("effect replacement applied {applied} parameter edits, expected at least {minimum}").into());
                        }
                        Ok(json!({"parameter_edits_applied":applied}))
                    }
                    Operation::ExpectEffectDialog { active } => {
                        let value = match (snapshot.dialog_active, active) {
                            (Some(value), _) => value,
                            (None, false) => 0,
                            (None, true) => return Err("Knob Dialog active-state symbol is missing from the composed module report".into()),
                        };
                        if value != u8::from(active) {
                            return Err(format!(
                                "Knob Dialog active state is {value}, expected {active}"
                            )
                            .into());
                        }
                        Ok(json!({"dialog_active":value}))
                    }
                    Operation::ExpectEffectType { value } => {
                        if snapshot.effect_type != value {
                            return Err(format!(
                                "effect replacement selected effect is {}, expected {value}",
                                snapshot.effect_type
                            )
                            .into());
                        }
                        Ok(json!({"effect_type":snapshot.effect_type}))
                    }
                    Operation::ExpectEffectProcessor { value } => {
                        if snapshot.processor_type != u32::from(value) {
                            return Err(format!(
                                "effect replacement processor is {}, expected {value}",
                                snapshot.processor_type
                            )
                            .into());
                        }
                        Ok(json!({"processor_type":snapshot.processor_type}))
                    }
                    Operation::ExpectEffectPanelMode { value } => {
                        if snapshot.panel_mode != u32::from(value) {
                            return Err(format!(
                                "effect replacement selected panel mode is {}, expected {value}",
                                snapshot.panel_mode
                            )
                            .into());
                        }
                        Ok(json!({"panel_mode":snapshot.panel_mode}))
                    }
                    Operation::ExpectEffectEditMode { enabled } => {
                        if snapshot.edit_mode != u32::from(enabled) {
                            return Err(format!(
                                "effect replacement parameter edit mode is {}, expected {enabled}",
                                snapshot.edit_mode
                            )
                            .into());
                        }
                        Ok(json!({"edit_mode":snapshot.edit_mode}))
                    }
                    _ => unreachable!(),
                }
            }
            Operation::EffectStateEnd => {
                let (_, _, started, guest_start, guest_budget, wall_budget) = self
                    .effect_state
                    .take()
                    .ok_or("effect state snapshot missing")?;
                if (guest_timeout_ms, wall_timeout_s) != (guest_budget, wall_budget)
                    || started.elapsed() >= Duration::from_secs(wall_budget)
                    || guest_now(session).wrapping_sub(guest_start) > guest_budget
                {
                    return Err(
                        "effect state assertion exceeded its shared deadline or changed budget"
                            .into(),
                    );
                }
                Ok(json!({"checked":true}))
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
            Operation::WaitEffectSlot {
                expected_effect_type,
                expected_processor_type,
                expected_effect,
                require_output,
            } => {
                if self.effect_slot_deadline.is_some() {
                    return Err("effect slot wait already active".into());
                }
                let reader = wait_effect_slot(
                    options,
                    session,
                    Some(expected_effect_type),
                    Some(expected_processor_type),
                    expected_effect.as_deref(),
                    require_output,
                    guest_timeout_ms,
                    wall_timeout_s,
                    operation_start,
                )
                .map_err(std::io::Error::other)?;
                self.effect_slot_deadline = Some(sd::EffectSlotScope {
                    reader,
                    start: operation_start.0,
                    guest_start: operation_start.1,
                    guest_budget: guest_timeout_ms,
                    wall_budget: wall_timeout_s,
                });
                Ok(
                    json!({"effect_type":expected_effect_type,"processor_type":expected_processor_type,"effect":expected_effect,"fresh_output":require_output}),
                )
            }
            Operation::WaitSdPatchLoaded => {
                let registry = wait_sd_patch_loaded(session, guest_timeout_ms, wall_timeout_s)?;
                Ok(json!({"registry": format!("{registry:#010x}")}))
            }
            Operation::WaitSdPatchEmpty => {
                let registry = wait_sd_patch_empty(session, guest_timeout_ms, wall_timeout_s)?;
                Ok(json!({"registry": format!("{registry:#010x}")}))
            }
            Operation::ExpectSdLoader => {
                if self.sd_inspection.is_some() {
                    return Err("SD inspection already active".into());
                }
                let wall_deadline = operation_start
                    .0
                    .checked_add(Duration::from_secs(wall_timeout_s))
                    .ok_or("SD inspection deadline overflow")?;
                let main = selected_main_image(options).map_err(std::io::Error::other)?;
                let fixture = freeze_sd_fixture(options, &main, session.engine.model())
                    .map_err(std::io::Error::other)?;
                let guarded = verify_sd_loader_hooks(
                    session,
                    &main,
                    &fixture.edits.0,
                    wall_deadline,
                    operation_start.1,
                    guest_timeout_ms,
                )
                .map_err(std::io::Error::other)?;
                self.sd_inspection = Some(sd::SdInspectionContext {
                    fixture,
                    snapshot: None,
                    allocation_checked: false,
                    chains: None,
                    continuation_modules: None,
                    module_images_checked: false,
                    wall_deadline,
                    guest_start: operation_start.1,
                    guest_timeout_ms,
                    wall_timeout_s,
                });
                Ok(json!({"guarded_hooks":guarded}))
            }
            Operation::CaptureSdModule { loaded } => {
                let context = self
                    .sd_inspection
                    .as_mut()
                    .ok_or("SD inspection missing loader assertion")?;
                if context.snapshot.is_some() {
                    return Err("SD module snapshot already captured".into());
                }
                let control = l6ctl::OwnedControl::new(&session.engine);
                let reader = control.sd_loader_reader(context.wall_deadline)?;
                let snapshot = if loaded {
                    wait_sd_loader_snapshot(
                        session,
                        &reader,
                        context.wall_deadline,
                        context.guest_start,
                        context.guest_timeout_ms,
                    )?
                } else {
                    reader.snapshot_cancellable(context.wall_deadline, || {
                        guest_now(session).wrapping_sub(context.guest_start)
                            >= context.guest_timeout_ms
                    })?
                };
                let evidence = serde_json::to_value(&snapshot)?;
                context.snapshot = Some((loaded, snapshot));
                Ok(evidence)
            }
            Operation::ExpectSdAllocation { loaded } => {
                let context = self.sd_inspection.as_mut().ok_or("SD inspection missing")?;
                let (captured_loaded, snapshot) = context
                    .snapshot
                    .as_ref()
                    .ok_or("SD module snapshot missing")?;
                if *captured_loaded != loaded || context.allocation_checked {
                    return Err("SD allocation assertion has unexpected state".into());
                }
                let evidence = check_sd_allocation(
                    snapshot,
                    loaded,
                    context
                        .fixture
                        .edits
                        .6
                        .iter()
                        .map(|(bytes, _, _)| bytes.len()),
                )?;
                context.allocation_checked = true;
                Ok(evidence)
            }
            Operation::ExpectSdChains {
                loaded,
                chain_count,
            } => {
                let context = self.sd_inspection.as_mut().ok_or("SD inspection missing")?;
                if !context.allocation_checked || context.chains.is_some() {
                    return Err("SD chain assertion has unexpected state".into());
                }
                let (captured_loaded, snapshot) = context
                    .snapshot
                    .as_ref()
                    .ok_or("SD module snapshot missing")?;
                if *captured_loaded != loaded {
                    return Err("SD chain assertion does not match captured state".into());
                }
                let inspection_deadline = context.wall_deadline;
                let inspection_guest_start = context.guest_start;
                let inspection_guest_budget = context.guest_timeout_ms;
                let (edits, _, _, _, _, module_hooks, _) = &context.fixture.edits;
                let control = l6ctl::OwnedControl::new(&session.engine);
                let reader = control.sd_loader_reader(inspection_deadline)?;
                let read_live =
                    |address: u32, length: usize| -> Result<Vec<u8>, Box<dyn std::error::Error>> {
                        reader
                            .read_chunk_cancellable(address, length, inspection_deadline, || {
                                guest_now(session).wrapping_sub(inspection_guest_start)
                                    >= inspection_guest_budget
                            })
                            .map_err(|e| std::io::Error::other(e).into())
                    };
                let Some(registry) = snapshot.registry else {
                    context.chains = Some(sd::SdChainEvidence {
                        expected_function: None,
                        count: 0,
                        links: Vec::new(),
                        expected_count: chain_count,
                    });
                    return Ok(json!({"loaded":false,"registry":null,"guarded_hooks":edits.len()}));
                };
                let inspect_chain = |record| -> Result<_, Box<dyn std::error::Error>> {
                    sd_typed_site_chain(
                        &reader,
                        session,
                        registry,
                        record,
                        inspection_deadline,
                        inspection_guest_start,
                        inspection_guest_budget,
                    )
                    .map_err(|e| std::io::Error::other(e).into())
                };
                let slot_for = |address| {
                    sd_frozen_handler_slot(&context.fixture.loader_report, registry, address)
                        .map_err(std::io::Error::other)
                };
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
                for module_hook in module_hooks {
                    *expected_chains.entry(module_hook.address).or_default() += 1;
                }
                for (address, module_count) in expected_chains {
                    let record = slot_for(address)?;
                    let (chain, _) = inspect_chain(record)?;
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

                let (registered_count, chain_head, chain_links) =
                    if let Some(address) = expected_function {
                        let record = slot_for(address)?;
                        let (chain, links) = inspect_chain(record)?;
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
                for (address, _, _entry_offset) in edits {
                    // Every edit is one of the module's hooks (sd_fixture_edits).
                    let slot = slot_for(*address)?;
                    let handler = u32::from_le_bytes(read_live(slot, 4)?.try_into().unwrap());
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
                let evidence = json!({"loaded":loaded,"hook_chain_count":registered_count,"hook_chain_orders":chain_links.iter().map(|link|link.2).collect::<Vec<_>>()});
                context.chains = Some(sd::SdChainEvidence {
                    expected_function,
                    count: registered_count,
                    links: chain_links,
                    expected_count: chain_count,
                });
                Ok(evidence)
            }
            Operation::ExpectSdContinuations { loaded } => {
                let context = self.sd_inspection.as_mut().ok_or("SD inspection missing")?;
                let (captured_loaded, snapshot) = context
                    .snapshot
                    .as_ref()
                    .ok_or("SD module snapshot missing")?;
                if *captured_loaded != loaded || context.continuation_modules.is_some() {
                    return Err("SD continuation assertion has unexpected state".into());
                }
                let chains = context
                    .chains
                    .as_ref()
                    .ok_or("SD chain assertion missing")?;
                let mut matched_modules = Vec::new();
                if loaded {
                    if snapshot.registry.is_none() {
                        return Err("SD module registry missing".into());
                    }
                    let expected_function = chains.expected_function;
                    let chain_count = chains.expected_count;
                    let chain_links = &chains.links;
                    let (_, _, _, _, _, module_hooks, module_images) = &context.fixture.edits;
                    let frozen_chain = &context.fixture.chain_first;
                    let frozen_loader = &context.fixture.loader_report;
                    let patch_report = context
                        .fixture
                        .patch_report
                        .as_ref()
                        .ok_or("dynamic report missing")?;
                    let ordered_chain = frozen_chain.is_some();
                    let inspection_deadline = context.wall_deadline;
                    let inspection_guest_start = context.guest_start;
                    let inspection_guest_budget = context.guest_timeout_ms;
                    let control = l6ctl::OwnedControl::new(&session.engine);
                    let reader = control.sd_loader_reader(inspection_deadline)?;
                    let read_live =
                        |address: u32,
                         length: usize|
                         -> Result<Vec<u8>, Box<dyn std::error::Error>> {
                            reader
                                .read_chunk_cancellable(
                                    address,
                                    length,
                                    inspection_deadline,
                                    || {
                                        guest_now(session).wrapping_sub(inspection_guest_start)
                                            >= inspection_guest_budget
                                    },
                                )
                                .map_err(|e| std::io::Error::other(e).into())
                        };
                    let hook = module_hooks
                        .iter()
                        .find(|hook| Some(hook.address) == expected_function)
                        .ok_or("dynamic patch bundle has no hook record")?;
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
                    let loader_report = &frozen_loader;
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
                        let module_index = module_images
                            .iter()
                            .enumerate()
                            .find_map(|(module_index, (_, _, hooks))| {
                                hooks
                                    .iter()
                                    .find(|candidate| {
                                        candidate.address == hook.address
                                            && (*entry == ((*module + candidate.entry_offset) | 1)
                                                || ordered_chain
                                                    && index == 0
                                                    && *entry
                                                        == ((*module
                                                            + pass_through_entry_offset
                                                                .unwrap_or(u32::MAX))
                                                            | 1))
                                            && *next_slot == *module + candidate.next_slot_offset
                                    })
                                    .map(|_| module_index)
                            })
                            .ok_or_else(|| {
                                format!("hook {index} does not match any module's hook metadata")
                            })?;
                        let expected_next =
                            chain_links.get(index + 1).map_or(fallback, |next| next.0);
                        let actual_next =
                            u32::from_le_bytes(read_live(*next_slot, 4)?.try_into().unwrap());
                        if actual_next != expected_next {
                            return Err(format!(
                                "hook {index} continues to {actual_next:#010x}, expected {expected_next:#010x}"
                            )
                            .into());
                        }
                        matched_modules.push(module_index);
                    }
                }
                let count = matched_modules.len();
                context.continuation_modules = Some(matched_modules);
                Ok(json!({"loaded":loaded,"continuations":count}))
            }
            Operation::ExpectSdModuleImages { loaded } => {
                let context = self
                    .sd_inspection
                    .as_mut()
                    .ok_or("SD module-image assertion needs preceding inspection")?;
                let value = sd::verify_module_images(session, context, loaded)?;
                context.module_images_checked = true;
                Ok(value)
            }
            Operation::ExpectSdLatestModule { loaded } => {
                let context = self
                    .sd_inspection
                    .as_ref()
                    .ok_or("SD latest-allocation assertion needs preceding inspection")?;
                if !context.module_images_checked {
                    return Err("SD module-image assertion missing".into());
                }
                let value = sd::verify_latest_module(session, context, loaded)?;
                self.sd_inspection = None;
                Ok(value)
            }
            Operation::ExpectHeap { minimum_free } => {
                let heap = l6ctl::OwnedControl::new(&session.engine).checked_heap()?;
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
            } => chord::hold(
                session,
                targets,
                duration_ms,
                until,
                guest_timeout_ms,
                wall_timeout_s,
            ),
            Operation::PanelEventBegin {
                target,
                column,
                row,
                event,
            } => {
                self.panel_event = Some(panel_event::Scope::begin(
                    session,
                    target,
                    column,
                    row,
                    event,
                    guest_timeout_ms,
                    wall_timeout_s,
                )?);
                Ok(json!({"started":true}))
            }
            Operation::PanelEventPress => Ok(self
                .panel_event
                .as_mut()
                .ok_or("panel event scope missing")?
                .press(session)?),
            Operation::PanelEventCapture => Ok(self
                .panel_event
                .as_mut()
                .ok_or("panel event scope missing")?
                .capture(session)?),
            Operation::PanelEventReport => Ok(self
                .panel_event
                .as_mut()
                .ok_or("panel event scope missing")?
                .assert_report(session)?),
            Operation::PanelEventMapping => Ok(self
                .panel_event
                .as_mut()
                .ok_or("panel event scope missing")?
                .assert_mapping(session, wall_timeout_s)?),
            Operation::PanelEventRelease => Ok(self
                .panel_event
                .as_mut()
                .ok_or("panel event scope missing")?
                .release(session, wall_timeout_s)?),
            Operation::PanelEventSettle => Ok(self
                .panel_event
                .as_ref()
                .ok_or("panel event scope missing")?
                .settle(session)?),
            Operation::PanelEventEnd => {
                let v = self
                    .panel_event
                    .as_ref()
                    .ok_or("panel event scope missing")?
                    .finish(session)?;
                self.panel_event = None;
                Ok(v)
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
                let operation = match command.as_str() {
                    "stop" => l6ctl::AudioTransport::Stop,
                    "play" => l6ctl::AudioTransport::Play,
                    "pause" => l6ctl::AudioTransport::Pause,
                    "seek" => l6ctl::AudioTransport::Seek,
                    other => return Err(format!("unknown audio input command {other}").into()),
                };
                let control = l6ctl::OwnedControl::new(&session.engine);
                let receipt = control.audio_input_transport(operation, position_ms)?;
                Ok(
                    json!({"command": command, "position_ms": position_ms, "sequence": receipt.sequence}),
                )
            }
            Operation::AudioInputLoad { channel, loaded } => {
                let control = l6ctl::OwnedControl::new(&session.engine);
                let before = control.audio_input_status()?.rejected;
                let sequence = control.audio_input_load(channel, loaded)?.sequence;
                let deadline =
                    std::time::Instant::now() + std::time::Duration::from_secs(wall_timeout_s);
                while control.audio_input_status()?.last_ack_sequence != Some(sequence) {
                    if std::time::Instant::now() >= deadline {
                        return Err("audio input load was not acknowledged".into());
                    }
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                if control.audio_input_status()?.rejected != before {
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
                    let status = l6ctl::OwnedControl::new(&session.engine).audio_input_status()?;
                    if status.playing == playing && (min_ms..=max_ms).contains(&status.position_ms)
                    {
                        break Ok(json!({"playing": playing, "position_ms": status.position_ms}));
                    }
                    if Instant::now() >= deadline
                        || guest_now(session).wrapping_sub(start)
                            >= guest_timeout_ms.min(GUEST_TIMEOUT_MS)
                    {
                        break Err(format!("audio input did not reach playing={playing}, {min_ms}..={max_ms} ms: playing={}, position={} ms",status.playing,status.position_ms).into());
                    }
                    thread::sleep(Duration::from_millis(POLL_INTERVAL_MS));
                }
            }
            Operation::ExpectAudioInput {
                playing,
                min_ms,
                max_ms,
            } => {
                let status = l6ctl::OwnedControl::new(&session.engine).audio_input_status()?;
                let evidence = json!({
                    "playing": status.playing,
                    "position_ms": status.position_ms,
                });
                if status.playing != playing || !(min_ms..=max_ms).contains(&status.position_ms) {
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
            Operation::L6maxTapBegin => {
                if self.tap_handler.is_some() {
                    return Err("tap scope already active".into());
                }
                self.tap_handler = Some(tap_handler::Scope::begin(
                    session,
                    guest_timeout_ms,
                    wall_timeout_s,
                )?);
                Ok(json!({"started":true}))
            }
            operation @ (Operation::L6maxTapPress
            | Operation::L6maxTapWait
            | Operation::L6maxTapAssert
            | Operation::L6maxTapRelease
            | Operation::L6maxTapEnd) => {
                let scope = self.tap_handler.as_mut().ok_or("tap scope missing")?;
                let result = match operation {
                    Operation::L6maxTapPress => scope.press(session),
                    Operation::L6maxTapWait => scope.wait(session),
                    Operation::L6maxTapAssert => scope.assert(session),
                    Operation::L6maxTapRelease => scope.release(session),
                    Operation::L6maxTapEnd => scope.finish(session),
                    _ => unreachable!(),
                }?;
                if matches!(operation, Operation::L6maxTapEnd) {
                    self.tap_handler = None;
                }
                Ok(result)
            }
            Operation::L6GpioBegin { target, event } => {
                if self.gpio_event.is_some() {
                    return Err("GPIO event scope already active".into());
                }
                self.gpio_event = Some(gpio_event::Scope::begin(
                    session,
                    target,
                    event,
                    guest_timeout_ms,
                    wall_timeout_s,
                )?);
                Ok(json!({"target":target,"event":event}))
            }
            operation @ (Operation::L6GpioPress
            | Operation::L6GpioSubmit
            | Operation::L6GpioAssert
            | Operation::L6GpioDispatch
            | Operation::L6GpioRelease
            | Operation::L6GpioSettle
            | Operation::L6GpioEnd) => {
                let scope = self.gpio_event.as_mut().ok_or("GPIO event scope missing")?;
                let result = match operation {
                    Operation::L6GpioPress => scope.press(session),
                    Operation::L6GpioSubmit => scope.submit(session),
                    Operation::L6GpioAssert => scope.assert(session),
                    Operation::L6GpioDispatch => scope.dispatch(session),
                    Operation::L6GpioRelease => scope.release(session),
                    Operation::L6GpioSettle => scope.settle(session),
                    Operation::L6GpioEnd => scope.finish(session),
                    _ => unreachable!(),
                }?;
                if matches!(operation, Operation::L6GpioEnd) {
                    self.gpio_event = None;
                }
                Ok(result)
            }
            Operation::L6UsbStartupBegin => {
                if self.usb_startup.is_some() {
                    return Err("USB startup scope already active".into());
                }
                self.usb_startup = Some(usb_startup::Scope::begin(
                    session,
                    options,
                    guest_timeout_ms,
                    wall_timeout_s,
                )?);
                Ok(json!({"started":true}))
            }
            operation @ (Operation::L6UsbStartupCapture
            | Operation::L6UsbStartupAssertHits
            | Operation::L6UsbStartupAssertWrites
            | Operation::L6UsbStartupAssertReplay
            | Operation::L6UsbStartupEnd) => {
                let scope = self
                    .usb_startup
                    .as_mut()
                    .ok_or("USB startup scope missing")?;
                let value = match operation {
                    Operation::L6UsbStartupCapture => scope.capture(session, options),
                    Operation::L6UsbStartupAssertHits => scope.assert_hits(session),
                    Operation::L6UsbStartupAssertWrites => scope.assert_writes(session),
                    Operation::L6UsbStartupAssertReplay => scope.assert_replay(session),
                    Operation::L6UsbStartupEnd => scope.finish(session),
                    _ => unreachable!(),
                }?;
                if matches!(operation, Operation::L6UsbStartupEnd) {
                    self.usb_startup = None;
                }
                Ok(value)
            }
            Operation::L6DispatchBegin { group, cc, value } => {
                if self.l6_dispatch.is_some() {
                    return Err("DIN dispatch already active".into());
                }
                self.l6_dispatch = Some(l6_dispatch::Scope::begin(
                    session,
                    group,
                    cc,
                    value,
                    guest_timeout_ms,
                    wall_timeout_s,
                )?);
                Ok(json!({"group":group,"cc":cc,"value":value}))
            }
            operation @ (Operation::L6DispatchTask
            | Operation::L6DispatchSemaphore
            | Operation::L6DispatchIdle
            | Operation::L6DispatchInject
            | Operation::L6DispatchWake
            | Operation::L6DispatchState
            | Operation::L6DispatchReader
            | Operation::L6DispatchEvent
            | Operation::L6DispatchEq
            | Operation::L6DispatchObserve
            | Operation::L6DispatchSetter { .. }
            | Operation::L6DispatchFinish) => {
                let scope = self
                    .l6_dispatch
                    .as_mut()
                    .ok_or("DIN dispatch scope missing")?;
                let result = match operation {
                    Operation::L6DispatchTask => scope.task(session),
                    Operation::L6DispatchSemaphore => scope.semaphore(session),
                    Operation::L6DispatchIdle => scope.idle(session),
                    Operation::L6DispatchInject => scope.inject(session),
                    Operation::L6DispatchWake => scope.wake(session),
                    Operation::L6DispatchState => scope.state(session),
                    Operation::L6DispatchReader => scope.reader(session),
                    Operation::L6DispatchEvent => scope.event(session),
                    Operation::L6DispatchEq => scope.eq(session),
                    Operation::L6DispatchObserve => scope.observe(session),
                    Operation::L6DispatchSetter { index, value } => {
                        scope.setter(session, index, value)
                    }
                    Operation::L6DispatchFinish => scope.finish(session),
                    _ => unreachable!(),
                }?;
                if matches!(operation, Operation::L6DispatchFinish) {
                    self.l6_dispatch = None;
                }
                Ok(result)
            }
            Operation::L6SweepBegin => {
                self.l6_input_sweep = Some(l6_input_sweep::Scope::begin(
                    session,
                    guest_timeout_ms,
                    wall_timeout_s,
                )?);
                Ok(json!({"started":true}))
            }
            operation @ (Operation::L6SweepRejectMain { .. }
            | Operation::L6SweepPanelBaseline { .. }
            | Operation::L6SweepPanelTap
            | Operation::L6SweepPanelWait
            | Operation::L6SweepPanelTrace
            | Operation::L6SweepPanelAssert
            | Operation::L6SweepEncoderBaseline
            | Operation::L6SweepEncoderSend { .. }
            | Operation::L6SweepEncoderWait
            | Operation::L6SweepEncoderTrace
            | Operation::L6SweepEncoderAssert
            | Operation::L6SweepRejectPanel
            | Operation::L6SweepRejectEncoder
            | Operation::L6SweepEnd) => {
                let scope = self
                    .l6_input_sweep
                    .as_mut()
                    .ok_or("L6 sweep is not active")?;
                let value = match operation {
                    Operation::L6SweepRejectMain { target } => {
                        scope.reject_main(session, target)?
                    }
                    Operation::L6SweepPanelBaseline { target } => {
                        scope.panel_baseline(session, options, target)?
                    }
                    Operation::L6SweepPanelTap => scope.panel_tap(session)?,
                    Operation::L6SweepPanelWait => scope.panel_wait(session)?,
                    Operation::L6SweepPanelTrace => scope.panel_trace(session, options)?,
                    Operation::L6SweepPanelAssert => scope.panel_assert(options)?,
                    Operation::L6SweepEncoderBaseline => scope.encoder_baseline(options)?,
                    Operation::L6SweepEncoderSend { index } => {
                        scope.encoder_send(session, index)?
                    }
                    Operation::L6SweepEncoderWait => scope.encoder_wait(session)?,
                    Operation::L6SweepEncoderTrace => scope.encoder_trace(session, options)?,
                    Operation::L6SweepEncoderAssert => scope.encoder_assert(options)?,
                    Operation::L6SweepRejectPanel => scope.reject_panel(session)?,
                    Operation::L6SweepRejectEncoder => scope.reject_encoder(session)?,
                    Operation::L6SweepEnd => scope.end(session)?,
                    _ => unreachable!(),
                };
                scope.check(session, guest_timeout_ms, wall_timeout_s)?;
                scope.phase = match operation {
                    Operation::L6SweepPanelAssert => 2,
                    Operation::L6SweepEncoderBaseline => 7,
                    Operation::L6SweepEncoderWait => 7,
                    Operation::L6SweepEncoderTrace => 9,
                    _ => scope.phase + 1,
                };
                if matches!(operation, Operation::L6SweepEnd) {
                    self.l6_input_sweep = None;
                }
                Ok(value)
            }
            Operation::L6DinSweepBegin => {
                if self.l6_din_sweep.is_some() {
                    return Err("L6 DIN sweep already active".into());
                }
                self.l6_din_sweep = Some(l6_din_sweep::Scope::begin(
                    session,
                    guest_timeout_ms,
                    wall_timeout_s,
                )?);
                Ok(json!({"started":true}))
            }
            operation @ (Operation::L6DinSweepReady
            | Operation::L6DinSweepMonitor
            | Operation::L6DinSweepBaseline
            | Operation::L6DinSweepSample { .. }
            | Operation::L6DinSweepTrace
            | Operation::L6DinSweepDrain
            | Operation::L6DinSweepStop
            | Operation::L6DinSweepCapture
            | Operation::L6DinSweepAssert { .. }
            | Operation::L6DinSweepEnd) => {
                let scope = self
                    .l6_din_sweep
                    .as_mut()
                    .ok_or("L6 DIN sweep scope missing")?;
                let value = match operation {
                    Operation::L6DinSweepReady => scope.ready(session, options),
                    Operation::L6DinSweepMonitor => scope.monitor(session),
                    Operation::L6DinSweepBaseline => scope.baseline(session),
                    Operation::L6DinSweepSample { cc } => scope.sample(session, cc),
                    Operation::L6DinSweepTrace => scope.trace(session, options),
                    Operation::L6DinSweepDrain => {
                        scope.drain(session, guest_timeout_ms, wall_timeout_s)
                    }
                    Operation::L6DinSweepStop => scope.stop(session),
                    Operation::L6DinSweepCapture => scope.capture(session, options),
                    Operation::L6DinSweepAssert { check } => scope.assert(session, check),
                    Operation::L6DinSweepEnd => scope.finish(),
                    _ => unreachable!(),
                }?;
                if matches!(operation, Operation::L6DinSweepEnd) {
                    self.l6_din_sweep = None;
                }
                Ok(value)
            }
            Operation::L6maxTimingBegin => {
                if self.l6max_timing.is_some() {
                    return Err("L6max timing already active".into());
                }
                self.l6max_timing = Some(timing::Scope::begin(
                    session,
                    guest_timeout_ms,
                    wall_timeout_s,
                )?);
                Ok(json!({"started":true}))
            }
            operation @ (Operation::L6maxTimingSettle { .. }
            | Operation::L6maxTimingBaseline
            | Operation::L6maxTimingEnqueue { .. }
            | Operation::L6maxTimingWaitFirst
            | Operation::L6maxTimingObservePaused { .. }
            | Operation::L6maxTimingCapturePaused
            | Operation::L6maxTimingAssertPausedClock
            | Operation::L6maxTimingAssertPausedInput
            | Operation::L6maxTimingResume
            | Operation::L6maxTimingWaitQueued
            | Operation::L6maxTimingWaitReports
            | Operation::L6maxTimingAssertReports
            | Operation::L6maxTimingObserveDisplay { .. }
            | Operation::L6maxTimingAssertDisplay
            | Operation::L6maxTimingEncoder { .. }
            | Operation::L6maxTimingWaitEncoder
            | Operation::L6maxTimingWaitEncoderReport
            | Operation::L6maxTimingAssertEncoder
            | Operation::L6maxTimingEnd) => {
                let context = self
                    .l6max_timing
                    .as_mut()
                    .ok_or("missing L6max timing scope")?;
                let value = match operation {
                    Operation::L6maxTimingSettle { wall_ms } => context.settle(session, wall_ms),
                    Operation::L6maxTimingBaseline => context.baseline(session, options),
                    Operation::L6maxTimingEnqueue {
                        target,
                        hold_ms,
                        pairs,
                    } => context.enqueue(session, target, hold_ms, pairs),
                    Operation::L6maxTimingWaitFirst => context.wait_first(session),
                    Operation::L6maxTimingObservePaused { wall_ms } => {
                        context.observe_paused(session, wall_ms)
                    }
                    Operation::L6maxTimingCapturePaused => context.capture_paused(session),
                    Operation::L6maxTimingAssertPausedClock => context.assert_paused_clock(),
                    Operation::L6maxTimingAssertPausedInput => context.assert_paused_input(),
                    Operation::L6maxTimingResume => context.resume(session),
                    Operation::L6maxTimingWaitQueued => context.wait_queued(session),
                    Operation::L6maxTimingWaitReports => context.wait_reports(session, options),
                    Operation::L6maxTimingAssertReports => context.assert_reports(session, options),
                    Operation::L6maxTimingObserveDisplay { guest_ms } => {
                        context.observe_display(session, guest_ms)
                    }
                    Operation::L6maxTimingAssertDisplay => context.assert_display(session),
                    Operation::L6maxTimingEncoder { index, steps } => {
                        context.encoder(session, options, index, steps)
                    }
                    Operation::L6maxTimingWaitEncoder => context.wait_encoder(session),
                    Operation::L6maxTimingWaitEncoderReport => {
                        context.wait_encoder_report(session, options)
                    }
                    Operation::L6maxTimingAssertEncoder => context.assert_encoder(options),
                    Operation::L6maxTimingEnd => Ok(json!({"finished":true})),
                    _ => unreachable!(),
                }?;
                context.phase += 1;
                if matches!(operation, Operation::L6maxTimingEnd) {
                    self.l6max_timing = None;
                }
                Ok(value)
            }
            Operation::L6maxPanelKeyBegin => {
                if session.engine.model() != l6fw::Model::L6max || self.panel_keys.is_some() {
                    return Err("panel key scope requires a fresh L6max".into());
                }
                l6ctl::OwnedControl::new(&session.engine)
                    .input_observer()
                    .map_err(|_| "panel inputs unavailable")?;
                self.panel_keys = Some(PanelKeyScope {
                    deadline: Instant::now() + Duration::from_secs(wall_timeout_s),
                    held: None,
                });
                Ok(json!({"started":true}))
            }
            Operation::L6maxPanelKeyInput { target, pressed } => {
                let scope = self.panel_keys.as_mut().ok_or("missing panel key scope")?;
                if Instant::now() >= scope.deadline {
                    return Err("panel key scope deadline exceeded".into());
                }
                if (pressed && scope.held.is_some())
                    || (!pressed && scope.held.as_ref().map(|(target, _)| *target) != Some(target))
                {
                    return Err("panel key held state mismatch".into());
                }
                let control = l6ctl::OwnedControl::new(&session.engine);
                let before = control.input_status(l6ctl::InputSource::Panel)?.applied;
                if pressed {
                    let source = l6ctl::button_source(target).ok_or("invalid held button")?;
                    let button = control.raw_button(source, target)?;
                    scope.held = Some((target, button));
                    scope.held.as_mut().unwrap().1.press(0)?;
                } else {
                    control.up(target)?;
                }
                let deadline = scope.deadline.min(Instant::now() + Duration::from_secs(10));
                loop {
                    session.poll()?;
                    if Instant::now() >= deadline {
                        return Err("panel key input acknowledgement timed out".into());
                    }
                    if control.input_status(l6ctl::InputSource::Panel)?.applied > before {
                        break;
                    }
                    thread::sleep(Duration::from_millis(5));
                }
                if !pressed {
                    scope.held = None;
                }
                Ok(json!({"target":target,"pressed":pressed}))
            }
            Operation::L6maxPanelKeyCheck {
                code,
                bit,
                word_index,
                active,
                wait,
            } => {
                let scope = self.panel_keys.as_ref().ok_or("missing panel key scope")?;
                let control = l6ctl::OwnedControl::new(&session.engine);
                let start = guest_now(session);
                loop {
                    if Instant::now() >= scope.deadline {
                        return Err("panel key scope deadline exceeded".into());
                    }
                    session.poll()?;
                    let snapshot = control.panel_key(code)?;
                    if Instant::now() >= scope.deadline {
                        return Err("panel key scope deadline exceeded".into());
                    }
                    let mask = 1u32.checked_shl(snapshot.bit).unwrap_or(0);
                    if bit >= 32
                        || snapshot.bit != bit
                        || snapshot.word_index != word_index
                        || word_index >= 6
                    {
                        return Err("unexpected panel key lookup".into());
                    }
                    let observed = snapshot.words[word_index as usize] & mask != 0;
                    if observed == active {
                        break Ok(serde_json::to_value(snapshot)?);
                    }
                    if !wait || guest_now(session).wrapping_sub(start) >= 5_000 {
                        return Err(format!("panel key {code:#x} did not become {active}").into());
                    }
                    thread::sleep(Duration::from_millis(5));
                }
            }
            Operation::L6maxPanelKeyEnd => {
                let scope = self.panel_keys.as_ref().ok_or("missing panel key scope")?;
                if scope.held.is_some() || Instant::now() >= scope.deadline {
                    return Err("panel key scope incomplete or expired".into());
                }
                self.panel_keys.take();
                Ok(json!({"complete":true}))
            }
            Operation::SignalPwmBegin => {
                self.signal_pwm = Some(signal_pwm::Scope::begin(
                    session,
                    guest_timeout_ms,
                    wall_timeout_s,
                )?);
                Ok(json!({"started":true}))
            }
            operation @ (Operation::SignalPwmWarmup { .. }
            | Operation::SignalPwmDrain { .. }
            | Operation::SignalPwmStage { .. }
            | Operation::SignalPwmAssert { .. }
            | Operation::SignalPwmEnd) => {
                let scope = self
                    .signal_pwm
                    .as_mut()
                    .ok_or("SIGNAL PWM scope is not active")?;
                let value = match operation {
                    Operation::SignalPwmWarmup { start, end, value } => {
                        scope.warmup(session, start, end, value)?
                    }
                    Operation::SignalPwmDrain { .. } => scope.drain(session)?,
                    Operation::SignalPwmStage {
                        band,
                        start,
                        end,
                        value,
                    } => scope.stage(session, band, start, end, value)?,
                    Operation::SignalPwmAssert { band, channel } => {
                        scope.assert_channel(band, channel)?
                    }
                    Operation::SignalPwmEnd => {
                        let (evidence, failure) = scope.finish(options)?;
                        if let Some(failure) = failure {
                            self.deferred_terminal_failure = true;
                            return Err(failure.into());
                        }
                        evidence
                    }
                    _ => unreachable!(),
                };
                scope.check(session, guest_timeout_ms, wall_timeout_s)?;
                match operation {
                    Operation::SignalPwmWarmup { .. } => scope.phase = 1,
                    Operation::SignalPwmDrain { .. } => {
                        scope.phase = 2;
                        scope.stages = 0;
                    }
                    Operation::SignalPwmStage { .. } => {
                        scope.stages += 1;
                        if scope.stages == 5 {
                            scope.phase = 3;
                            scope.assertions = 0;
                        }
                    }
                    Operation::SignalPwmAssert { .. } => {
                        scope.assertions += 1;
                        if scope.assertions == 4 {
                            if scope.band == 0 {
                                scope.band = 1;
                                scope.phase = 1;
                            } else {
                                scope.phase = 4;
                            }
                        }
                    }
                    Operation::SignalPwmEnd => self.signal_pwm = None,
                    _ => unreachable!(),
                };
                Ok(value)
            }
            Operation::L6MeterBegin => {
                self.l6_meter_pwm = Some(l6_meter_pwm::Scope::begin(
                    session,
                    guest_timeout_ms,
                    wall_timeout_s,
                )?);
                Ok(json!({"started":true}))
            }
            Operation::L6MeterStage { value } => {
                let scope = self.l6_meter_pwm.as_mut().ok_or("L6 meter scope missing")?;
                scope.check(session, guest_timeout_ms, wall_timeout_s)?;
                Ok(scope.stage(session, options, value)?)
            }
            Operation::L6MeterAssert => {
                let scope = self.l6_meter_pwm.as_mut().ok_or("L6 meter scope missing")?;
                scope.check(session, guest_timeout_ms, wall_timeout_s)?;
                Ok(scope.assert(session, options)?)
            }
            Operation::L6MeterEnd => {
                let scope = self.l6_meter_pwm.as_mut().ok_or("L6 meter scope missing")?;
                scope.check(session, guest_timeout_ms, wall_timeout_s)?;
                if !scope.asserted {
                    return Err("L6 meter assertion missing".into());
                }
                scope.finish(session)?;
                self.l6_meter_pwm = None;
                Ok(json!({"requires_restart":true}))
            }
            Operation::Restart => Err("restart is supervised by the session runner".into()),
            Operation::WarmReset => {
                let receipt = l6ctl::OwnedControl::new(&session.engine)
                    .warm_reset(Duration::from_secs(wall_timeout_s))?;
                Ok(serde_json::to_value(receipt)?)
            }
            operation @ (Operation::PowerBegin { .. }
            | Operation::PowerWaitAck
            | Operation::PowerObserveOn { .. }
            | Operation::PowerWaitOff { .. }
            | Operation::PowerObserveStopped { .. }
            | Operation::PowerEnd) => {
                self.run_power(session, operation, guest_timeout_ms, wall_timeout_s)
            }
            Operation::SettingsCapture { restored } => {
                if self.settings.is_some() {
                    return Err("settings observation already active".into());
                }
                self.settings = Some(settings::Scope::capture(
                    session,
                    options,
                    restored,
                    guest_timeout_ms,
                    wall_timeout_s,
                )?);
                Ok(json!({"restored":restored}))
            }
            operation @ (Operation::SettingsVersions
            | Operation::SettingsRtc
            | Operation::SettingsBank
            | Operation::SettingsTrace
            | Operation::SettingsPanel
            | Operation::SettingsReload
            | Operation::SettingsRtcTiming
            | Operation::SettingsEnd) => {
                let scope = self
                    .settings
                    .as_mut()
                    .ok_or("settings observation missing")?;
                let evidence = match operation {
                    Operation::SettingsVersions => scope.versions(session),
                    Operation::SettingsRtc => scope.rtc(session, self.saved.as_ref()),
                    Operation::SettingsBank => scope.bank(session),
                    Operation::SettingsTrace => scope.trace(session, options),
                    Operation::SettingsPanel => scope.panel(session),
                    Operation::SettingsReload => scope.reload(session, self.saved.as_ref()),
                    Operation::SettingsRtcTiming => scope.rtc_timing(session),
                    Operation::SettingsEnd => scope.finish(session),
                    _ => unreachable!(),
                }?;
                if matches!(operation, Operation::SettingsEnd) {
                    let scope = self.settings.take().unwrap();
                    if let Some(saved) = scope.saved_checkpoint()? {
                        self.saved = Some(saved);
                    }
                }
                Ok(evidence)
            }
            Operation::SdCard { inserted } => {
                let path = if inserted {
                    Some(
                        options
                            .sd_image
                            .as_deref()
                            .ok_or("SD insertion requires a fixture card")?,
                    )
                } else {
                    None
                };
                let receipt = l6ctl::OwnedControl::new(&session.engine).sd_card(path)?;
                Ok(serde_json::to_value(receipt)?)
            }
            Operation::UsbControllerReady => {
                if self.usb_descriptor_budget.is_some() {
                    return Err("USB descriptor transaction already active".into());
                }
                let control = l6ctl::OwnedControl::new(&session.engine);
                let budget = control.usb_panel_budget(
                    operation_start.0 + Duration::from_secs(wall_timeout_s),
                    operation_start.1,
                    guest_timeout_ms,
                )?;
                control.usb_attach_before(&budget)?;
                // Legacy readiness starts after attach, independently of the
                // enclosing operation's original guest and wall anchors.
                let ready_start = control.input_status(l6ctl::InputSource::Panel)?.guest_ms;
                let ready_deadline = Instant::now() + Duration::from_secs(wall_timeout_s);
                loop {
                    budget.check()?;
                    if control.usb_controller_before(&budget)? {
                        break;
                    }
                    if Instant::now() >= ready_deadline
                        || control
                            .input_status(l6ctl::InputSource::Panel)?
                            .guest_ms
                            .wrapping_sub(ready_start)
                            > guest_timeout_ms
                    {
                        return Err("USB device controller did not start".into());
                    }
                    session.poll()?;
                    budget.check()?;
                    thread::sleep(Duration::from_millis(25));
                }
                self.usb_descriptor_budget = Some(budget);
                Ok(json!({"usb_controller_running":true}))
            }
            Operation::UsbEnumerate => {
                let budget = self
                    .usb_descriptor_budget
                    .take()
                    .ok_or("missing USB controller readiness")?;
                budget.check()?;
                let (descriptor, config, configuration) =
                    l6ctl::OwnedControl::new(&session.engine).usb_enumerate_before(&budget)?;
                budget.check()?;
                self.midi = configuration.midi;
                budget.check()?;
                fs::write(options.logs.join("usb-device.bin"), &descriptor)?;
                budget.check()?;
                fs::write(options.logs.join("usb-config.bin"), &config)?;
                budget.check()?;
                Ok(
                    json!({"vid":u16::from_le_bytes([descriptor[8],descriptor[9]]),"pid":u16::from_le_bytes([descriptor[10],descriptor[11]]),"configuration_bytes":config.len(),"interfaces":config[4]}),
                )
            }
            operation @ (Operation::UsbMidiOpen
            | Operation::UsbMidiBegin
            | Operation::UsbMidiExpectEncoder
            | Operation::UsbMidiPrepareLevel { .. }
            | Operation::UsbMidiSetAlternate
            | Operation::UsbMidiSetTarget
            | Operation::UsbMidiEnd) => self
                .run_usb_midi_steps(
                    session,
                    options,
                    operation,
                    guest_timeout_ms,
                    wall_timeout_s,
                    operation_start,
                )
                .map_err(Into::into),
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
                shared,
            ),
            operation @ (Operation::DirectMidiConnect { .. }
            | Operation::MidiEncoderAlternate
            | Operation::HostMidiBegin
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
            operation @ (Operation::L6MidiProbeBegin
            | Operation::L6MidiProbeInject
            | Operation::L6MidiProbeExpectHook
            | Operation::L6MidiProbeExpectReplies
            | Operation::L6MidiProbeFinish) => Ok(self.run_l6_midi_probe(
                session,
                options,
                operation,
                guest_timeout_ms,
                wall_timeout_s,
            )?),
            operation @ (Operation::DinUsbBegin
            | Operation::DinUsbAttach
            | Operation::DinUsbSend { .. }
            | Operation::DinUsbExpect { .. }
            | Operation::DinUsbEnd) => self.run_din_usb(session, operation),
            Operation::L6IndicatorBegin => {
                if session.engine.model() != l6fw::Model::L6 {
                    return Err("L6 indicator campaign requires L6".into());
                }
                if self.host_midi_client().is_none() {
                    return Err(
                        "L6 indicator campaign requires a retained direct USB MIDI session".into(),
                    );
                }
                self.l6_indicator = Some(L6IndicatorScope {
                    start: operation_start.0,
                    guest_start: operation_start.1,
                    guest_budget: guest_timeout_ms,
                    wall_budget: wall_timeout_s,
                    changed_segments: 0,
                    last_mute: None,
                });
                Ok(json!({"started":true}))
            }
            Operation::L6RingSample {
                cc,
                value,
                guest_ms,
                ring,
            } => Ok(self.run_l6_indicator_sample(session, cc, value, guest_ms, ring, None, false)?),
            Operation::L6MuteSample {
                cc,
                value,
                guest_ms,
                expected,
                reuse_previous,
            } => Ok(self.run_l6_indicator_sample(
                session,
                cc,
                value,
                guest_ms,
                None,
                Some(expected),
                reuse_previous,
            )?),
            Operation::L6RingCoverage { mask } => {
                let actual = self
                    .l6_indicator
                    .as_ref()
                    .ok_or("L6 indicator campaign is not active")?
                    .changed_segments;
                if actual != mask {
                    return Err(format!(
                        "L6 changed ring segments {actual:#05x}, expected {mask:#05x}"
                    )
                    .into());
                }
                Ok(json!({"changed_segments":actual}))
            }
            Operation::L6IndicatorEnd => {
                self.l6_indicator
                    .take()
                    .ok_or("L6 indicator campaign is not active")?;
                Ok(json!({"finished":true}))
            }
            Operation::UsbSysexBegin => {
                if self.midi.is_none() {
                    return Err("usb-sysex needs a preceding USB MIDI enumeration".into());
                }
                let guest = l6ctl::OwnedControl::new(&session.engine)
                    .input_status(l6ctl::InputSource::Panel)?
                    .guest_ms;
                self.sysex_deadline =
                    Some((Instant::now(), guest, guest_timeout_ms, wall_timeout_s));
                Ok(json!({"started": true}))
            }
            Operation::UsbSysexEnd => {
                let (start, guest, budget, wall) = self
                    .sysex_deadline
                    .take()
                    .ok_or("USB SysEx campaign is not active")?;
                if sysex_deadline_expired(
                    start.elapsed(),
                    guest,
                    l6ctl::OwnedControl::new(&session.engine)
                        .input_status(l6ctl::InputSource::Panel)?
                        .guest_ms,
                    budget,
                    wall,
                ) {
                    return Err("USB SysEx deadline exceeded".into());
                }
                Ok(json!({"finished": true}))
            }
            Operation::UsbSysexExchange {
                request,
                complete_on_match,
            } => {
                let control = l6ctl::OwnedControl::new(&session.engine);
                let endpoints = self
                    .midi
                    .ok_or("usb-sysex needs a preceding USB MIDI enumeration")?;
                let (started, start, guest_timeout_ms, wall_timeout_s) = self
                    .sysex_deadline
                    .ok_or("USB SysEx campaign is not active")?;
                let check = || -> Result<(), Box<dyn std::error::Error>> {
                    let now = l6ctl::OwnedControl::new(&session.engine)
                        .input_status(l6ctl::InputSource::Panel)?
                        .guest_ms;
                    if sysex_deadline_expired(
                        started.elapsed(),
                        start,
                        now,
                        guest_timeout_ms,
                        wall_timeout_s,
                    ) {
                        return Err(format!(
                            "USB SysEx deadline exceeded for request {:02x?}",
                            request.bytes
                        )
                        .into());
                    }
                    Ok(())
                };
                check()?;
                let reply_timeout_ms = request.reply_timeout_ms.unwrap_or(400);
                control
                    .usb_sysex_send(endpoints, 2, &request.bytes)
                    .map_err(|error| error.to_string())?;
                check()?;
                let request_start = l6ctl::OwnedControl::new(&session.engine)
                    .input_status(l6ctl::InputSource::Panel)?
                    .guest_ms;
                let mut decoder = l6ctl::UsbSysexDecoder::default();
                loop {
                    check()?;
                    session.poll()?;
                    check()?;
                    control
                        .usb_sysex_poll(endpoints, 2, &mut decoder)
                        .map_err(|error| error.to_string())?;
                    check()?;
                    let elapsed = l6ctl::OwnedControl::new(&session.engine)
                        .input_status(l6ctl::InputSource::Panel)?
                        .guest_ms
                        .wrapping_sub(request_start);
                    if complete_on_match
                        && sysex_reply_complete(&request, decoder.pending(), decoder.replies())
                    {
                        break;
                    }
                    if elapsed >= reply_timeout_ms {
                        break;
                    }
                    thread::sleep(Duration::from_millis(2));
                }
                let pending = decoder.pending();
                let replies = decoder.replies();
                if !pending.is_empty() {
                    return Err(format!("truncated USB SysEx reply: {pending:02x?}").into());
                }
                if request.expect_reply && replies.is_empty() {
                    return Err(
                        format!("missing SysEx reply for request {:02x?}", request.bytes).into(),
                    );
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
                Ok(json!({"sent":request.bytes,"replies":replies}))
            }
            Operation::EditorAssignBegin => {
                let endpoints = self
                    .midi
                    .ok_or("Editor assignments require USB enumeration")?;
                self.editor_assignment = Some(editor::EditorAssignmentScope::begin(
                    session,
                    endpoints,
                    guest_timeout_ms,
                    wall_timeout_s,
                )?);
                Ok(json!({"started":true}))
            }
            operation @ (Operation::EditorAssignIdentity
            | Operation::EditorAssignGlobal
            | Operation::EditorAssignVersion
            | Operation::EditorAssignSdInfo
            | Operation::EditorAssignRefresh
            | Operation::EditorAssignCount { .. }
            | Operation::EditorAssignFilename { .. }
            | Operation::EditorAssignSend { .. }
            | Operation::EditorAssignDialog { .. }
            | Operation::EditorAssignState { .. }
            | Operation::EditorAssignAssert { .. }
            | Operation::EditorAssignEnd) => {
                let scope = self
                    .editor_assignment
                    .as_mut()
                    .ok_or("Editor assignment is not active")?;
                let value = match operation {
                    Operation::EditorAssignIdentity => scope.identity(session)?,
                    Operation::EditorAssignGlobal => scope.expected(
                        session,
                        &l6ctl::EditorRequest::GlobalSettings.encode(),
                        &[0xf0, 0x52, 0, 0, 0x2a],
                        "global settings",
                    )?,
                    Operation::EditorAssignVersion => scope.expected(
                        session,
                        &l6ctl::EditorRequest::ApplicationVersion.encode(),
                        &[0xf0, 0x52, 0, 0, 0, 0x11],
                        "application version",
                    )?,
                    Operation::EditorAssignSdInfo => scope.expected(
                        session,
                        &l6ctl::EditorRequest::SdInfo.encode(),
                        &[0xf0, 0x52, 0, 0, 0x67, 0x00],
                        "SD card information",
                    )?,
                    Operation::EditorAssignRefresh => scope.initial_refresh(session)?,
                    Operation::EditorAssignCount { pad } => scope.count(session, pad)?,
                    Operation::EditorAssignFilename { pad } => scope.filename(session, pad)?,
                    Operation::EditorAssignSend { pad } => scope.assign(session, pad)?,
                    Operation::EditorAssignDialog { pad } => scope.dialog(session, pad)?,
                    Operation::EditorAssignState { pad } => scope.state(session, pad)?,
                    Operation::EditorAssignAssert { pad } => scope.assert_state(pad)?,
                    Operation::EditorAssignEnd => scope.finish(options)?,
                    _ => unreachable!(),
                };
                scope.check(session, guest_timeout_ms, wall_timeout_s)?;
                if matches!(operation, Operation::EditorAssignAssert { .. }) {
                    scope.pad += 1;
                    scope.phase = if scope.pad == 4 { 11 } else { 5 };
                } else {
                    scope.phase += 1;
                }
                if matches!(operation, Operation::EditorAssignEnd) {
                    self.editor_assignment = None;
                }
                Ok(value)
            }
            Operation::EditorPlaybackBegin { pad } => {
                let endpoints = self
                    .midi
                    .ok_or("Editor playback requires USB enumeration")?;
                self.editor_playback = Some(editor::EditorPlaybackScope::begin(
                    session,
                    endpoints,
                    pad,
                    guest_timeout_ms,
                    wall_timeout_s,
                )?);
                Ok(json!({"pad":pad + 1}))
            }
            operation @ (Operation::EditorPlaybackPlay
            | Operation::EditorPlaybackWait
            | Operation::EditorPlaybackObserve
            | Operation::EditorPlaybackSample
            | Operation::EditorPlaybackAssert
            | Operation::EditorPlaybackEnd) => {
                let scope = self
                    .editor_playback
                    .as_mut()
                    .ok_or("Editor playback is not active")?;
                let value = match operation {
                    Operation::EditorPlaybackPlay => scope.play(session, options)?,
                    Operation::EditorPlaybackWait => scope.wait_playing(session)?,
                    Operation::EditorPlaybackObserve => scope.observe(session, options)?,
                    Operation::EditorPlaybackSample => scope.sample(session, options)?,
                    Operation::EditorPlaybackAssert => scope.assert_audio()?,
                    Operation::EditorPlaybackEnd => json!({"finished":true}),
                    _ => unreachable!(),
                };
                scope.check(session, guest_timeout_ms, wall_timeout_s)?;
                scope.phase += 1;
                if matches!(operation, Operation::EditorPlaybackEnd) {
                    self.editor_playback = None;
                }
                Ok(value)
            }
            Operation::UiPadBegin { pad } => {
                let endpoints = self
                    .midi
                    .ok_or("UI pad verification requires USB enumeration")?;
                self.ui_pad = Some(editor::UiPadScope::begin(
                    session,
                    endpoints,
                    pad,
                    guest_timeout_ms,
                    wall_timeout_s,
                )?);
                Ok(json!({"pad":pad + 1}))
            }
            operation @ (Operation::UiPadIdentity
            | Operation::UiPadGlobal
            | Operation::UiPadKeepAlive
            | Operation::UiPadQuery
            | Operation::UiPadAssert
            | Operation::UiPadEnd) => {
                let scope = self
                    .ui_pad
                    .as_mut()
                    .ok_or("UI pad verification is not active")?;
                let value = match operation {
                    Operation::UiPadIdentity => scope.identity(session)?,
                    Operation::UiPadGlobal => scope.expected(
                        session,
                        &l6ctl::EditorRequest::GlobalSettings.encode(),
                        &[0xf0, 0x52, 0, 0, 0x2a],
                        "global settings",
                    )?,
                    Operation::UiPadKeepAlive => scope.expected(
                        session,
                        &l6ctl::EditorRequest::KeepAlive.encode(),
                        &[0xf0, 0x52, 0, 0, 0, 0x0b],
                        "keep-alive",
                    )?,
                    Operation::UiPadQuery => scope.query(session)?,
                    Operation::UiPadAssert => scope.assert_assignment(session, options)?,
                    Operation::UiPadEnd => json!({"finished":true}),
                    _ => unreachable!(),
                };
                scope.check(session, guest_timeout_ms, wall_timeout_s)?;
                scope.phase += 1;
                if matches!(operation, Operation::UiPadEnd) {
                    self.ui_pad = None;
                }
                Ok(value)
            }
            Operation::WaitL6SoundPadFile { path } => {
                wait_l6_sound_pad_file(session, &path, guest_timeout_ms, wall_timeout_s)
            }
            Operation::SoundPadBegin {
                pad,
                target,
                assignment,
                require_pad_led_change,
                playback_window_ms,
                require_stereo_meter,
                expect_source,
                max_residual_db,
            } => {
                if self.sound_pad.is_some() {
                    return Err("sound-pad transaction already active".into());
                }
                self.sound_pad = Some(audio::sound_pad::begin(
                    session,
                    options,
                    pad,
                    target,
                    assignment,
                    require_pad_led_change,
                    playback_window_ms,
                    require_stereo_meter,
                    expect_source,
                    max_residual_db,
                    guest_timeout_ms,
                    wall_timeout_s,
                    operation_start,
                )?);
                Ok(json!({"pad":pad,"target":target}))
            }
            operation @ (Operation::SoundPadPress
            | Operation::SoundPadHold
            | Operation::SoundPadRelease
            | Operation::SoundPadObserve
            | Operation::SoundPadSample
            | Operation::SoundPadAssertWaveform
            | Operation::SoundPadAssertAudio
            | Operation::SoundPadAssertLed
            | Operation::SoundPadAssertMeter
            | Operation::SoundPadFinish) => {
                let context = self
                    .sound_pad
                    .as_mut()
                    .ok_or("missing sound-pad transaction")?;
                let result = match operation {
                    Operation::SoundPadPress => audio::sound_pad::press(context, session, options),
                    Operation::SoundPadHold => audio::sound_pad::hold(context, session),
                    Operation::SoundPadRelease => audio::sound_pad::release(context, session),
                    Operation::SoundPadObserve => {
                        audio::sound_pad::observe(context, session, options)
                    }
                    Operation::SoundPadSample => {
                        audio::sound_pad::collect(context, session, options)
                    }
                    Operation::SoundPadAssertWaveform => audio::sound_pad::assert_waveform(context),
                    Operation::SoundPadAssertAudio => audio::sound_pad::assert_audio(context),
                    Operation::SoundPadAssertLed => audio::sound_pad::assert_led(context),
                    Operation::SoundPadAssertMeter => audio::sound_pad::assert_meter(context),
                    Operation::SoundPadFinish => Ok(json!({"finished":true})),
                    _ => unreachable!(),
                }?;
                context.phase += 1;
                if matches!(operation, Operation::SoundPadFinish) {
                    self.sound_pad = None;
                }
                Ok(result)
            }
            Operation::L6RecorderBegin { target } => {
                if self.l6_recorder.is_some() {
                    return Err("recorder transaction already active".into());
                }
                self.l6_recorder = Some(audio::recorder::Scope::begin(
                    session,
                    options,
                    target,
                    guest_timeout_ms,
                    wall_timeout_s,
                )?);
                Ok(json!({"started": true}))
            }
            operation @ (Operation::L6RecorderPress
            | Operation::L6RecorderDispatcher
            | Operation::L6RecorderAssertEvent
            | Operation::L6RecorderHandler
            | Operation::L6RecorderAssertSelection
            | Operation::L6RecorderCallback
            | Operation::L6RecorderAssertCallback
            | Operation::L6RecorderObserve
            | Operation::L6RecorderRelease
            | Operation::L6RecorderSample
            | Operation::L6RecorderEvidence
            | Operation::L6RecorderAssertTrace
            | Operation::L6RecorderAssertDma
            | Operation::L6RecorderEnd) => {
                let context = self
                    .l6_recorder
                    .as_mut()
                    .ok_or("missing recorder transaction")?;
                let result = match operation {
                    Operation::L6RecorderPress => context.press(session),
                    Operation::L6RecorderDispatcher => context.dispatcher(session),
                    Operation::L6RecorderAssertEvent => context.assert_event(),
                    Operation::L6RecorderHandler => context.handler(session),
                    Operation::L6RecorderAssertSelection => context.assert_selection(),
                    Operation::L6RecorderCallback => context.callback(session),
                    Operation::L6RecorderAssertCallback => context.assert_callback(),
                    Operation::L6RecorderObserve => context.observe(session),
                    Operation::L6RecorderRelease => context.release(session),
                    Operation::L6RecorderSample => context.sample(session, options),
                    Operation::L6RecorderEvidence => context.save(options),
                    Operation::L6RecorderAssertTrace => context.assert_trace(),
                    Operation::L6RecorderAssertDma => context.assert_dma(),
                    Operation::L6RecorderEnd => Ok(json!({"finished": true})),
                    _ => unreachable!(),
                }?;
                context.phase += 1;
                if matches!(operation, Operation::L6RecorderEnd) {
                    self.l6_recorder = None;
                }
                Ok(result)
            }
            Operation::L6maxRecorderBegin { target } => {
                if self.l6max_recorder.is_some() {
                    return Err("L6max recorder already active".into());
                }
                self.l6max_recorder = Some(audio::max_recorder::Scope::begin(
                    session,
                    options,
                    target,
                    guest_timeout_ms,
                    wall_timeout_s,
                )?);
                Ok(json!({"started":true}))
            }
            operation @ (Operation::L6maxRecorderPress
            | Operation::L6maxRecorderSubmit
            | Operation::L6maxRecorderQueue
            | Operation::L6maxRecorderDispatch
            | Operation::L6maxRecorderScheduler
            | Operation::L6maxRecorderCallback
            | Operation::L6maxRecorderAction
            | Operation::L6maxRecorderStream
            | Operation::L6maxRecorderObserve
            | Operation::L6maxRecorderSample
            | Operation::L6maxRecorderEvidence
            | Operation::L6maxRecorderAssertAudio
            | Operation::L6maxRecorderEnd) => {
                let context = self
                    .l6max_recorder
                    .as_mut()
                    .ok_or("missing L6max recorder")?;
                let result = match operation {
                    Operation::L6maxRecorderPress => context.press(session),
                    Operation::L6maxRecorderSubmit => context.submit(session),
                    Operation::L6maxRecorderQueue => context.queue(session),
                    Operation::L6maxRecorderDispatch => context.dispatch(session),
                    Operation::L6maxRecorderScheduler => context.scheduler(session),
                    Operation::L6maxRecorderCallback => context.callback(session),
                    Operation::L6maxRecorderAction => context.action(session),
                    Operation::L6maxRecorderStream => context.stream(session),
                    Operation::L6maxRecorderObserve => context.observe(session),
                    Operation::L6maxRecorderSample => context.sample(session, options),
                    Operation::L6maxRecorderEvidence => context.save(options),
                    Operation::L6maxRecorderAssertAudio => context.assert_audio(),
                    Operation::L6maxRecorderEnd => Ok(json!({"finished":true})),
                    _ => unreachable!(),
                }?;
                context.phase += 1;
                if matches!(operation, Operation::L6maxRecorderEnd) {
                    self.l6max_recorder = None;
                }
                Ok(result)
            }
            Operation::UsbStorageBegin => {
                if self.usb_storage_scope.is_some() {
                    return Err("USB storage already active".into());
                }
                self.usb_storage_scope = Some(usb_storage_steps::Scope::begin(
                    session,
                    guest_timeout_ms,
                    wall_timeout_s,
                )?);
                Ok(json!({"started":true}))
            }
            operation @ (Operation::UsbStorageInquiry
            | Operation::UsbStorageReady
            | Operation::UsbStorageCapacity
            | Operation::UsbStorageReadBoot
            | Operation::UsbStorageAssertBoot
            | Operation::UsbStorageLastRead
            | Operation::UsbStorageLastWrite
            | Operation::UsbStorageLastReadback
            | Operation::UsbStorageLastAssert
            | Operation::UsbStorageFatOpen
            | Operation::UsbStorageCreate { .. }
            | Operation::UsbStorageFlushCreated
            | Operation::UsbStorageReadFile
            | Operation::UsbStorageRemoveFile
            | Operation::UsbStorageAssertFile
            | Operation::UsbStorageFlushRemoved
            | Operation::UsbStorageEnd) => {
                let context = self
                    .usb_storage_scope
                    .as_mut()
                    .ok_or("missing USB storage scope")?;
                let value = match operation {
                    Operation::UsbStorageInquiry => context.inquiry(),
                    Operation::UsbStorageReady => context.ready(),
                    Operation::UsbStorageCapacity => context.capacity(),
                    Operation::UsbStorageReadBoot => context.read_boot(),
                    Operation::UsbStorageAssertBoot => context.assert_boot(options),
                    Operation::UsbStorageLastRead => context.last_read(),
                    Operation::UsbStorageLastWrite => context.last_write(),
                    Operation::UsbStorageLastReadback => context.last_readback(),
                    Operation::UsbStorageLastAssert => context.last_assert(),
                    Operation::UsbStorageFatOpen => context.fat_open(),
                    Operation::UsbStorageCreate {
                        length,
                        multiplier,
                        increment,
                    } => context.create(length, multiplier, increment),
                    Operation::UsbStorageFlushCreated => context.flush(),
                    Operation::UsbStorageReadFile => context.read(),
                    Operation::UsbStorageRemoveFile => context.remove(),
                    Operation::UsbStorageAssertFile => context.assert_file(),
                    Operation::UsbStorageFlushRemoved => context.flush(),
                    Operation::UsbStorageEnd => context.finish(),
                    _ => unreachable!(),
                }?;
                context.phase += 1;
                if matches!(operation, Operation::UsbStorageEnd) {
                    self.usb_storage_scope = None;
                }
                Ok(value)
            }
            Operation::AudioCallbacksBegin => {
                if self.audio_callbacks.is_some() {
                    return Err("audio callbacks already active".into());
                }
                self.audio_callbacks = Some(audio::callbacks::Scope::begin(
                    session,
                    options,
                    guest_timeout_ms,
                    wall_timeout_s,
                )?);
                Ok(json!({"started":true}))
            }
            operation @ (Operation::AudioCallbacksNext
            | Operation::AudioCallbacksCapture
            | Operation::AudioCallbacksAssert
            | Operation::AudioCallbacksEnd) => {
                let scope = self
                    .audio_callbacks
                    .as_mut()
                    .ok_or("audio callbacks scope missing")?;
                scope.check(session, guest_timeout_ms, wall_timeout_s)?;
                let value = match operation {
                    Operation::AudioCallbacksNext => scope.next(session),
                    Operation::AudioCallbacksCapture => scope.capture(session, options),
                    Operation::AudioCallbacksAssert => scope.assert_trace(options),
                    Operation::AudioCallbacksEnd => scope.finish(),
                    _ => unreachable!(),
                }?;
                scope.check(session, guest_timeout_ms, wall_timeout_s)?;
                if matches!(operation, Operation::AudioCallbacksEnd) {
                    self.audio_callbacks = None;
                }
                Ok(value)
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
            Operation::KnobDialogPreflightBegin { sd_knob } => {
                self.knob_dialog_preflight = Some(begin_preflight(
                    session,
                    options,
                    sd_knob,
                    shared.ok_or("missing shared deadline")?,
                )?);
                let control = l6ctl::OwnedControl::new(&session.engine);
                self.overlay_reader = Some(control.knob_overlay_reader()?);
                self.knob_heap_reader = Some(control.heap_counter_reader()?);
                Ok(json!({"preflight": "begin"}))
            }
            Operation::KnobProbePrepare { probe } => prepare_preflight_probe(
                session,
                self.knob_dialog_preflight
                    .as_mut()
                    .ok_or("missing knob-dialog preflight")?,
                probe,
            ),
            Operation::KnobProbeInject => inject_preflight_probe(
                session,
                self.knob_dialog_preflight
                    .as_mut()
                    .ok_or("missing knob-dialog preflight")?,
                shared.ok_or("missing shared deadline")?,
            ),
            Operation::KnobProbeTrigger { delta } => trigger_preflight_probe(
                session,
                self.knob_dialog_preflight
                    .as_mut()
                    .ok_or("missing knob-dialog preflight")?,
                delta,
                wall_timeout_s,
                shared.ok_or("missing shared deadline")?,
            ),
            Operation::KnobProbeSample => sample_preflight_probe(
                session,
                self.knob_dialog_preflight
                    .as_mut()
                    .ok_or("missing knob-dialog preflight")?,
                self.overlay_reader
                    .as_ref()
                    .ok_or("missing preflight reader")?,
                self.knob_heap_reader
                    .as_ref()
                    .ok_or("missing preflight heap reader")?,
            ),
            Operation::KnobProbeRestore => restore_preflight_probe(
                self.knob_dialog_preflight
                    .as_mut()
                    .ok_or("missing knob-dialog preflight")?,
            ),
            Operation::KnobProbeAssert => {
                let result = assert_preflight_probe(
                    self.knob_dialog_preflight
                        .as_mut()
                        .ok_or("missing knob-dialog preflight")?,
                )?;
                if self
                    .knob_dialog_preflight
                    .as_ref()
                    .is_some_and(|c| preflight_probe_index(c) == 5)
                {
                    self.overlay_reader = None;
                    self.knob_heap_reader = None;
                }
                Ok(result)
            }
            Operation::KnobRaceBegin => begin_callback_race(
                session,
                self.knob_dialog_preflight
                    .as_mut()
                    .ok_or("missing knob-dialog preflight")?,
                shared.ok_or("missing shared deadline")?,
            ),
            Operation::KnobRaceWaitRegistration => wait_race_registration(
                session,
                self.knob_dialog_preflight
                    .as_mut()
                    .ok_or("missing knob-dialog preflight")?,
                shared.ok_or("missing shared deadline")?,
            ),
            Operation::KnobRaceArmCallback => arm_race_callback(
                session,
                self.knob_dialog_preflight
                    .as_mut()
                    .ok_or("missing knob-dialog preflight")?,
                shared.ok_or("missing shared deadline")?,
            ),
            Operation::KnobRaceWaitCallback => wait_race_callback(
                session,
                self.knob_dialog_preflight
                    .as_mut()
                    .ok_or("missing knob-dialog preflight")?,
                shared.ok_or("missing shared deadline")?,
            ),
            Operation::KnobRaceCapture { guest_ms, artifact } => capture_callback_race(
                session,
                options,
                self.knob_dialog_preflight
                    .as_mut()
                    .ok_or("missing knob-dialog preflight")?,
                guest_ms,
                &artifact,
                guest_timeout_ms,
                wall_timeout_s,
                shared.ok_or("missing shared deadline")?,
            ),
            Operation::KnobRaceSample => sample_callback_race(
                session,
                self.knob_dialog_preflight
                    .as_mut()
                    .ok_or("missing knob-dialog preflight")?,
                self.knob_dialog_baseline
                    .as_ref()
                    .ok_or("missing knob-dialog baseline")?,
            ),
            Operation::KnobRaceRestore => restore_callback_race(
                self.knob_dialog_preflight
                    .as_mut()
                    .ok_or("missing knob-dialog preflight")?,
            ),
            Operation::KnobRaceAssert => assert_callback_race(
                session,
                self.knob_dialog_preflight
                    .as_mut()
                    .ok_or("missing knob-dialog preflight")?,
            ),
            Operation::KnobActivationTrigger => activation_trigger(
                session,
                self.knob_dialog_preflight
                    .as_mut()
                    .ok_or("missing knob-dialog preflight")?,
                shared.ok_or("missing shared deadline")?,
            ),
            Operation::KnobActivationWait => activation_wait(
                session,
                self.knob_dialog_preflight
                    .as_mut()
                    .ok_or("missing knob-dialog preflight")?,
                guest_timeout_ms,
                wall_timeout_s,
                shared.ok_or("missing shared deadline")?,
            ),
            Operation::KnobActivationSample => activation_sample(
                session,
                self.knob_dialog_preflight
                    .as_mut()
                    .ok_or("missing knob-dialog preflight")?,
                shared.ok_or("missing shared deadline")?,
            ),
            Operation::KnobActivationRestore => activation_restore(
                session,
                self.knob_dialog_preflight
                    .as_mut()
                    .ok_or("missing knob-dialog preflight")?,
                shared.ok_or("missing shared deadline")?,
            ),
            Operation::KnobActivationAssert => activation_assert(
                self.knob_dialog_preflight
                    .as_mut()
                    .ok_or("missing knob-dialog preflight")?,
            ),
            Operation::KnobActiveBegin => begin_active_display(
                options,
                self.knob_dialog_preflight
                    .as_mut()
                    .ok_or("missing knob-dialog preflight")?,
            ),
            Operation::KnobActiveSampleCallbacks => sample_active_callbacks(
                session,
                self.knob_dialog_preflight
                    .as_mut()
                    .ok_or("missing knob-dialog preflight")?,
            ),
            Operation::KnobActiveCheckCallbacks => check_active_callbacks(
                self.knob_dialog_preflight
                    .as_mut()
                    .ok_or("missing knob-dialog preflight")?,
            ),
            Operation::KnobActiveCheckFrame => {
                let frame = &self
                    .captured_display
                    .as_ref()
                    .ok_or("missing active display capture")?
                    .frame;
                let baseline = self
                    .knob_dialog_baseline
                    .as_ref()
                    .ok_or("missing knob-dialog baseline")?;
                check_active_frame(frame, baseline)
            }
            Operation::KnobActiveFinish => {
                let frame = self
                    .captured_display
                    .take()
                    .ok_or("missing active display capture")?
                    .frame;
                finish_active_display(
                    self.knob_dialog_preflight
                        .as_mut()
                        .ok_or("missing knob-dialog preflight")?,
                    frame,
                )
            }
            Operation::KnobLevelBegin => begin_level_demo(
                session,
                self.knob_dialog_preflight
                    .as_mut()
                    .ok_or("missing knob-dialog preflight")?,
            ),
            Operation::KnobLevelStart { demo, artifact } => run_level_demo_frame(
                session,
                options,
                self.knob_dialog_preflight
                    .as_mut()
                    .ok_or("missing knob-dialog preflight")?,
                &demo,
                &artifact,
                self.adaptive_progress.clone(),
                self.adaptive_index,
                guest_timeout_ms,
                wall_timeout_s,
                shared.ok_or("missing shared deadline")?,
            ),
            Operation::KnobLevelSweep { sweep } => {
                self.captured_display = None;
                run_level_sweep(
                    session,
                    options,
                    self.knob_dialog_preflight
                        .as_mut()
                        .ok_or("missing knob-dialog preflight")?,
                    &sweep,
                    self.adaptive_progress.clone(),
                    self.adaptive_index,
                    guest_timeout_ms,
                    wall_timeout_s,
                    shared.ok_or("missing shared deadline")?,
                )
            }
            Operation::KnobLevelExpectZero => run_level_expect_zero(
                session,
                options,
                self.knob_dialog_preflight
                    .as_mut()
                    .ok_or("missing knob-dialog preflight")?,
                self.adaptive_progress.clone(),
                self.adaptive_index,
                guest_timeout_ms,
                wall_timeout_s,
                shared.ok_or("missing shared deadline")?,
            ),
            Operation::KnobLevelFinish => {
                if self.captured_display.take().is_none() {
                    return Err("missing level zero capture".into());
                }
                finish_level_demo(
                    self.knob_dialog_preflight
                        .as_mut()
                        .ok_or("missing knob-dialog preflight")?,
                )
            }
            Operation::KnobTaskBegin => {
                let result = begin_task_probe(
                    session,
                    self.knob_dialog_preflight
                        .as_mut()
                        .ok_or("missing knob-dialog preflight")?,
                )?;
                self.task_reader =
                    Some(l6ctl::OwnedControl::new(&session.engine).current_task_reader()?);
                Ok(result)
            }
            Operation::KnobTaskArmHook => arm_task_hook(
                session,
                self.knob_dialog_preflight
                    .as_mut()
                    .ok_or("missing knob-dialog preflight")?,
            ),
            Operation::KnobTaskWaitHook => wait_task_hook(
                session,
                self.knob_dialog_preflight
                    .as_mut()
                    .ok_or("missing knob-dialog preflight")?,
                self.task_reader.as_ref().ok_or("missing task reader")?,
                shared.ok_or("missing shared deadline")?,
            ),
            Operation::KnobTaskArmCallback => arm_task_callback(
                self.knob_dialog_preflight
                    .as_mut()
                    .ok_or("missing knob-dialog preflight")?,
            ),
            Operation::KnobTaskWaitCallback => wait_task_callback(
                session,
                self.knob_dialog_preflight
                    .as_mut()
                    .ok_or("missing knob-dialog preflight")?,
                self.task_reader.as_ref().ok_or("missing task reader")?,
                shared.ok_or("missing shared deadline")?,
            ),
            Operation::KnobTaskAssertDistinct => assert_task_distinct(
                self.knob_dialog_preflight
                    .as_mut()
                    .ok_or("missing knob-dialog preflight")?,
            ),
            Operation::KnobTaskRelease => release_task_probe(
                self.knob_dialog_preflight
                    .as_mut()
                    .ok_or("missing knob-dialog preflight")?,
            ),
            Operation::KnobTaskFinish => {
                if self.captured_display.take().is_none() {
                    return Err("missing task probe capture".into());
                }
                let value = finish_task_probe(
                    self.knob_dialog_preflight
                        .as_mut()
                        .ok_or("missing knob-dialog preflight")?,
                )?;
                self.task_reader = None;
                Ok(value)
            }
            Operation::KnobOverlayBegin => {
                begin_overlay_invariants(
                    session,
                    options,
                    self.knob_dialog_preflight
                        .as_mut()
                        .ok_or("missing knob-dialog preflight")?,
                )?;
                self.overlay_reader =
                    Some(l6ctl::OwnedControl::new(&session.engine).knob_overlay_reader()?);
                Ok(json!({"overlay": "opened"}))
            }
            Operation::KnobOverlayCheckWindow => {
                check_overlay_window(
                    self.knob_dialog_preflight
                        .as_mut()
                        .ok_or("missing knob-dialog preflight")?,
                    self.overlay_reader
                        .as_ref()
                        .ok_or("missing overlay reader")?,
                )?;
                Ok(json!({"checked": true}))
            }
            Operation::KnobOverlaySnapshotDescriptors => {
                snapshot_overlay_descriptors(
                    self.knob_dialog_preflight
                        .as_mut()
                        .ok_or("missing knob-dialog preflight")?,
                    self.overlay_reader
                        .as_ref()
                        .ok_or("missing overlay reader")?,
                )?;
                Ok(json!({"checked": true}))
            }
            Operation::KnobOverlayFinishDescriptors => {
                finish_overlay_descriptors(
                    self.knob_dialog_preflight
                        .as_mut()
                        .ok_or("missing knob-dialog preflight")?,
                )?;
                Ok(json!({"checked": true}))
            }
            Operation::KnobOverlayCheckResource => {
                check_overlay_resource(
                    self.knob_dialog_preflight
                        .as_mut()
                        .ok_or("missing knob-dialog preflight")?,
                    self.overlay_reader
                        .as_ref()
                        .ok_or("missing overlay reader")?,
                )?;
                Ok(json!({"checked": true}))
            }
            Operation::KnobOverlayCheckCorners => {
                check_overlay_corners(
                    self.knob_dialog_preflight
                        .as_mut()
                        .ok_or("missing knob-dialog preflight")?,
                )?;
                Ok(json!({"checked": true}))
            }
            Operation::KnobOverlayCheckSignature => {
                check_overlay_signature(
                    self.overlay_reader
                        .as_ref()
                        .ok_or("missing overlay reader")?,
                )?;
                Ok(json!({"checked": true}))
            }
            Operation::KnobOverlayCheckBitmap => {
                check_overlay_bitmap(
                    self.knob_dialog_preflight
                        .as_mut()
                        .ok_or("missing knob-dialog preflight")?,
                    self.overlay_reader
                        .as_ref()
                        .ok_or("missing overlay reader")?,
                )?;
                Ok(json!({"checked": true}))
            }
            Operation::KnobOverlayCheckDescriptor { field } => check_overlay_descriptor(
                self.knob_dialog_preflight
                    .as_mut()
                    .ok_or("missing knob-dialog preflight")?,
                field,
            ),
            Operation::KnobOverlayCheckHeap => check_overlay_heap(
                session,
                self.knob_dialog_preflight
                    .as_mut()
                    .ok_or("missing knob-dialog preflight")?,
            ),
            Operation::KnobOverlayFinish => {
                let value = finish_overlay_invariants(
                    self.knob_dialog_preflight
                        .as_mut()
                        .ok_or("missing knob-dialog preflight")?,
                )?;
                self.overlay_reader = None;
                Ok(value)
            }
            Operation::KnobBitmapReactivate { delta } => {
                let budget = shared.ok_or("missing shared deadline")?;
                let check = || notification_observation_check(session, budget, budget.3);
                self.overlay_reader = Some(
                    l6ctl::OwnedControl::new(&session.engine).knob_overlay_reader_before_checked(
                        budget.0 + Duration::from_secs(budget.3),
                        &check,
                    )?,
                );
                reactivate_zero_overlay(
                    session,
                    self.knob_dialog_preflight
                        .as_mut()
                        .ok_or("missing knob preflight")?,
                    self.overlay_reader
                        .as_ref()
                        .ok_or("missing bitmap overlay reader")?,
                    delta,
                    shared.ok_or("missing shared deadline")?,
                    11,
                )
            }
            Operation::KnobBitmapWaitActive => {
                let value = wait_zero_overlay_active(
                    session,
                    self.knob_dialog_preflight
                        .as_mut()
                        .ok_or("missing knob preflight")?,
                    self.overlay_reader
                        .as_ref()
                        .ok_or("missing bitmap overlay reader")?,
                    shared.ok_or("missing shared deadline")?,
                    15,
                )?;
                self.overlay_reader = None;
                Ok(value)
            }
            Operation::KnobBitmapBegin => {
                let value = begin_bitmap_probe(
                    session,
                    options,
                    self.knob_dialog_preflight
                        .as_mut()
                        .ok_or("missing knob-dialog preflight")?,
                )?;
                let budget = shared.ok_or("missing shared deadline")?;
                let check = || notification_observation_check(session, budget, budget.3);
                self.overlay_reader = Some(
                    l6ctl::OwnedControl::new(&session.engine).knob_overlay_reader_before_checked(
                        budget.0 + Duration::from_secs(budget.3),
                        &check,
                    )?,
                );
                Ok(value)
            }
            Operation::KnobBitmapCheckBackground {
                background,
                expected,
            } => check_bitmap_background(
                session,
                self.knob_dialog_preflight
                    .as_mut()
                    .ok_or("missing knob-dialog preflight")?,
                self.overlay_reader
                    .as_ref()
                    .ok_or("missing bitmap reader")?,
                background,
                expected,
                shared.ok_or("missing shared deadline")?,
            ),
            Operation::KnobBitmapStoreBefore => {
                let frame = self
                    .captured_display
                    .take()
                    .ok_or("missing bitmap before capture")?
                    .frame;
                store_bitmap_before(
                    self.knob_dialog_preflight
                        .as_mut()
                        .ok_or("missing knob-dialog preflight")?,
                    frame,
                )
            }
            Operation::KnobBitmapArm => arm_bitmap_callback(
                session,
                self.knob_dialog_preflight
                    .as_mut()
                    .ok_or("missing knob-dialog preflight")?,
            ),
            Operation::KnobBitmapWait => wait_bitmap_callback(
                session,
                self.knob_dialog_preflight
                    .as_mut()
                    .ok_or("missing knob-dialog preflight")?,
                shared.ok_or("missing shared deadline")?,
            ),
            Operation::KnobBitmapInvoke { background } => invoke_bitmap_probe(
                session,
                self.knob_dialog_preflight
                    .as_mut()
                    .ok_or("missing knob-dialog preflight")?,
                background,
                shared.ok_or("missing shared deadline")?,
            ),
            Operation::KnobBitmapCheckFrame => {
                let frame = &self
                    .captured_display
                    .as_ref()
                    .ok_or("missing bitmap after capture")?
                    .frame;
                check_bitmap_frame(
                    self.knob_dialog_preflight
                        .as_mut()
                        .ok_or("missing knob-dialog preflight")?,
                    frame,
                )
            }
            Operation::KnobBitmapCheckSignature => check_bitmap_signature(
                self.overlay_reader
                    .as_ref()
                    .ok_or("missing bitmap reader")?,
            ),
            Operation::KnobBitmapCheckDescriptor => check_bitmap_descriptor(
                self.knob_dialog_preflight
                    .as_mut()
                    .ok_or("missing knob-dialog preflight")?,
                self.overlay_reader
                    .as_ref()
                    .ok_or("missing bitmap reader")?,
            ),
            Operation::KnobBitmapFinish => {
                self.captured_display = None;
                let value = finish_bitmap_probe(
                    self.knob_dialog_preflight
                        .as_mut()
                        .ok_or("missing knob-dialog preflight")?,
                )?;
                self.overlay_reader = None;
                Ok(value)
            }
            Operation::KnobOverlayReactivate { delta } => {
                self.overlay_reader =
                    Some(l6ctl::OwnedControl::new(&session.engine).knob_overlay_reader()?);
                reactivate_zero_overlay(
                    session,
                    self.knob_dialog_preflight
                        .as_mut()
                        .ok_or("missing knob-dialog preflight")?,
                    self.overlay_reader
                        .as_ref()
                        .ok_or("missing overlay reactivation reader")?,
                    delta,
                    shared.ok_or("missing shared deadline")?,
                    12,
                )
            }
            Operation::KnobOverlayWaitActive => {
                let value = wait_zero_overlay_active(
                    session,
                    self.knob_dialog_preflight
                        .as_mut()
                        .ok_or("missing knob-dialog preflight")?,
                    self.overlay_reader
                        .as_ref()
                        .ok_or("missing overlay reactivation reader")?,
                    shared.ok_or("missing shared deadline")?,
                    1,
                )?;
                self.overlay_reader = None;
                Ok(value)
            }
            Operation::KnobOverlayRefreshBegin => {
                begin_refreshed_overlay(
                    session,
                    options,
                    self.knob_dialog_preflight
                        .as_mut()
                        .ok_or("missing knob-dialog preflight")?,
                )?;
                Ok(json!({"overlay": "opened"}))
            }
            Operation::KnobOverlayRefreshValue => check_refreshed_value(
                session,
                self.knob_dialog_preflight
                    .as_mut()
                    .ok_or("missing knob-dialog preflight")?,
            ),
            Operation::KnobOverlayRefreshPixels => {
                let frame = self
                    .captured_display
                    .as_ref()
                    .ok_or("missing refreshed capture")?
                    .frame;
                check_refreshed_pixels(
                    self.knob_dialog_preflight
                        .as_mut()
                        .ok_or("missing knob-dialog preflight")?,
                    &frame,
                )?;
                Ok(json!({"pixels_match": true}))
            }
            Operation::KnobOverlayRefreshFinish => {
                let capture = self
                    .captured_display
                    .take()
                    .ok_or("missing refreshed capture")?;
                finish_refreshed_overlay(
                    self.knob_dialog_preflight
                        .as_mut()
                        .ok_or("missing knob-dialog preflight")?,
                    capture.frame,
                )
            }
            Operation::KnobLockAcquire => {
                self.overlay_reader =
                    Some(l6ctl::OwnedControl::new(&session.engine).knob_overlay_reader()?);
                acquire_locked_update(
                    session,
                    self.knob_dialog_preflight
                        .as_mut()
                        .ok_or("missing knob-dialog preflight")?,
                    self.overlay_reader
                        .as_ref()
                        .ok_or("missing locked update reader")?,
                    shared.ok_or("missing shared deadline")?,
                )
            }
            Operation::KnobLockTrigger { delta } => trigger_locked_update(
                session,
                self.knob_dialog_preflight
                    .as_mut()
                    .ok_or("missing knob-dialog preflight")?,
                delta,
                wall_timeout_s,
            ),
            Operation::KnobLockWait => wait_locked_update(
                session,
                self.knob_dialog_preflight
                    .as_mut()
                    .ok_or("missing knob-dialog preflight")?,
                self.overlay_reader
                    .as_ref()
                    .ok_or("missing locked update reader")?,
                guest_timeout_ms,
                wall_timeout_s,
            ),
            Operation::KnobLockSample => sample_locked_update(
                session,
                self.knob_dialog_preflight
                    .as_mut()
                    .ok_or("missing knob-dialog preflight")?,
                self.overlay_reader
                    .as_ref()
                    .ok_or("missing locked update reader")?,
            ),
            Operation::KnobLockRestore => restore_locked_update(
                session,
                self.knob_dialog_preflight
                    .as_mut()
                    .ok_or("missing knob-dialog preflight")?,
                shared.ok_or("missing shared deadline")?,
            ),
            Operation::KnobLockAssert => assert_locked_update(
                self.knob_dialog_preflight
                    .as_mut()
                    .ok_or("missing knob-dialog preflight")?,
            ),
            Operation::KnobLockFinish => {
                let result = finish_locked_update(
                    self.knob_dialog_preflight
                        .as_mut()
                        .ok_or("missing knob-dialog preflight")?,
                    self.overlay_reader
                        .as_ref()
                        .ok_or("missing locked update reader")?,
                )?;
                self.overlay_reader = None;
                self.captured_display = None;
                Ok(result)
            }
            operation @ (Operation::KnobDialogModeBegin { .. }
            | Operation::EnqueueTap { .. }
            | Operation::WaitInputApplied
            | Operation::WaitKnobParameterMode { .. }
            | Operation::WaitKnobDisplayImage { .. }
            | Operation::CaptureDisplay { .. }
            | Operation::ExpectCapturedDisplayImage { .. }
            | Operation::KnobDialogExpectHeap
            | Operation::KnobDialogModeEnd) => self.run_mode_primitive(
                session,
                options,
                operation,
                guest_timeout_ms,
                wall_timeout_s,
                shared,
            ),
            Operation::KnobDialogAdaptive { plan } => {
                self.knob_heap_reader = None;
                run_adaptive_demo(
                    &plan,
                    session,
                    options,
                    self.knob_dialog_preflight
                        .as_mut()
                        .ok_or("missing knob preflight")?,
                    self.adaptive_progress.clone(),
                    self.adaptive_index,
                    guest_timeout_ms,
                    wall_timeout_s,
                    shared.ok_or("missing shared deadline")?,
                )
            }
            Operation::KnobDialogInjectInvalidTag { tag } => inject_invalid_tag(
                session,
                self.knob_dialog_preflight
                    .as_mut()
                    .ok_or("missing knob preflight")?,
                tag,
                wall_timeout_s,
                shared.ok_or("missing shared deadline")?,
            ),
            Operation::KnobDialogWaitInvalidTag => wait_invalid_tag(
                session,
                self.knob_dialog_preflight
                    .as_mut()
                    .ok_or("missing knob preflight")?,
                guest_timeout_ms,
                wall_timeout_s,
                shared.ok_or("missing shared deadline")?,
            ),
            Operation::KnobDialogConfirmInvalidTag => confirm_invalid_tag(
                session,
                self.knob_dialog_preflight
                    .as_mut()
                    .ok_or("missing knob preflight")?,
                shared.ok_or("missing shared deadline")?,
            ),
            Operation::KnobDialogSettleLevel { artifact } => settle_level(
                session,
                options,
                self.knob_dialog_preflight
                    .as_mut()
                    .ok_or("missing knob preflight")?,
                &artifact,
                guest_timeout_ms,
                wall_timeout_s,
            ),
            Operation::KnobDialogInjectInvalidSignature => inject_invalid_signature(
                session,
                self.knob_dialog_preflight
                    .as_mut()
                    .ok_or("missing knob preflight")?,
                shared.ok_or("missing shared deadline")?,
            ),
            Operation::KnobDialogObserveInvalidSignature { guest_ms } => observe_invalid_signature(
                session,
                self.knob_dialog_preflight
                    .as_mut()
                    .ok_or("missing knob preflight")?,
                guest_ms,
                guest_timeout_ms,
                wall_timeout_s,
            ),
            Operation::KnobDialogSampleInvalidSignature => sample_invalid_signature(
                session,
                self.knob_dialog_preflight
                    .as_mut()
                    .ok_or("missing knob preflight")?,
            ),
            Operation::KnobDialogRestoreInvalidSignature => restore_invalid_signature(
                session,
                self.knob_dialog_preflight
                    .as_mut()
                    .ok_or("missing knob preflight")?,
                shared.ok_or("missing shared deadline")?,
            ),
            Operation::KnobDialogPrepareNotification => prepare_notification(
                session,
                self.knob_dialog_preflight
                    .as_mut()
                    .ok_or("missing knob preflight")?,
            ),
            Operation::KnobDialogArmNotificationCallback => arm_notification_callback(
                session,
                self.knob_dialog_preflight
                    .as_mut()
                    .ok_or("missing knob preflight")?,
            ),
            Operation::KnobDialogWaitNotificationCallback => wait_notification_callback(
                session,
                self.knob_dialog_preflight
                    .as_mut()
                    .ok_or("missing knob preflight")?,
            ),
            Operation::KnobDialogInvokeNotification { message } => invoke_notification(
                session,
                self.knob_dialog_preflight
                    .as_mut()
                    .ok_or("missing knob preflight")?,
                message,
                shared.ok_or("missing knob-dialog shared deadline")?,
                wall_timeout_s,
            ),
            Operation::KnobDialogWaitNotificationFrame {
                clear,
                artifact,
                guest_ms,
                limit_ms,
            } => wait_notification_frame(
                session,
                options,
                self.knob_dialog_preflight
                    .as_mut()
                    .ok_or("missing knob preflight")?,
                clear,
                &artifact,
                guest_ms,
                limit_ms,
                guest_timeout_ms,
                wall_timeout_s,
                shared.ok_or("missing knob-dialog shared deadline")?,
            ),
            Operation::KnobDialogExpectNotificationInvariants { clear } => {
                expect_notification_invariants(
                    session,
                    self.knob_dialog_preflight
                        .as_mut()
                        .ok_or("missing knob preflight")?,
                    clear,
                )
            }
            Operation::KnobDialogWaitExpiry {
                interval_ms,
                limit_ms,
            } => wait_dialog_expiry(
                session,
                self.knob_dialog_preflight
                    .as_mut()
                    .ok_or("missing knob preflight")?,
                interval_ms,
                limit_ms,
                guest_timeout_ms,
                wall_timeout_s,
            ),
            Operation::KnobDialogExpectExpiryFrame { after } => {
                let capture = self
                    .captured_display
                    .as_ref()
                    .ok_or("missing expiry capture")?;
                let context = self
                    .knob_dialog_preflight
                    .as_mut()
                    .ok_or("missing knob preflight")?;
                expect_expiry_frame(context, &capture.frame, after)?;
                if !after {
                    self.captured_display = None;
                }
                Ok(json!({"after":after}))
            }
            Operation::KnobDialogSnapshotRestoration => snapshot_restoration(
                session,
                self.knob_dialog_preflight
                    .as_mut()
                    .ok_or("missing knob preflight")?,
            ),
            Operation::KnobDialogCheckRestoration { field } => check_restoration_field(
                self.knob_dialog_preflight
                    .as_mut()
                    .ok_or("missing knob preflight")?,
                field,
            ),
            Operation::KnobDialogFinishRestoration => finish_restoration(
                self.knob_dialog_preflight
                    .as_mut()
                    .ok_or("missing knob preflight")?,
            ),
            Operation::KnobDialogExpectRestoredHeap => expect_restored_heap(
                session,
                self.knob_dialog_preflight
                    .as_mut()
                    .ok_or("missing knob preflight")?,
            ),
            Operation::EnqueueEncoderNoAck { channel, steps } => {
                let context = self
                    .knob_dialog_preflight
                    .as_mut()
                    .ok_or("missing knob-dialog preflight")?;
                if expecting_active_display(context) {
                    l6ctl::OwnedControl::new(&session.engine).encoder(channel, steps)?;
                    Ok(json!({"channel":channel,"steps":steps}))
                } else if expecting_task_probe(context) {
                    l6ctl::OwnedControl::new(&session.engine).encoder(channel, steps)?;
                    mark_task_hook_input(context)?;
                    Ok(json!({"channel":channel,"steps":steps}))
                } else {
                    enqueue_navigation_encoder(session, context, channel, steps)
                }
            }
            Operation::EnqueueTapNoAck { target, hold_ms } => enqueue_navigation_tap(
                session,
                self.knob_dialog_preflight
                    .as_mut()
                    .ok_or("missing knob preflight")?,
                target,
                hold_ms,
            ),
            Operation::ReleaseCapturedDisplay => self.release_navigation_capture(options),
            Operation::KnobDialogSnapshotNavigation => snapshot_navigation(
                session,
                self.knob_dialog_preflight
                    .as_mut()
                    .ok_or("missing knob preflight")?,
            ),
            Operation::KnobDialogCheckNavigation { field } => check_navigation_field(
                self.knob_dialog_preflight
                    .as_mut()
                    .ok_or("missing knob preflight")?,
                field,
            ),
            Operation::KnobDialog => {
                self.knob_heap_reader = None;
                let _baseline = self
                    .knob_dialog_baseline
                    .take()
                    .ok_or("knob-dialog requires baseline frame")?;
                finish_knob_dialog(options, &mut self.knob_dialog_preflight)
            }
            operation @ (Operation::KnobPopupOpen { .. }
            | Operation::KnobPopupExpectOpen
            | Operation::KnobPopupExpectDisplay
            | Operation::KnobPopupExpectValue) => {
                self.run_knob_popup(session, operation, guest_timeout_ms, wall_timeout_s)
            }
            Operation::CaptureMenu { menu, path } => {
                let snapshot =
                    capture_menu(session, &menu, &path, guest_timeout_ms, wall_timeout_s)
                        .map_err(std::io::Error::other)?;
                self.menu_snapshot = Some(snapshot.clone());
                Ok(serde_json::to_value(snapshot)?)
            }
            Operation::ExpectMenuItems { items } => {
                let snapshot = self.menu_snapshot.take().ok_or("missing menu snapshot")?;
                if snapshot.items != items {
                    return Err(format!(
                        "menu {} {:?} lists {:?}, expected {items:?}",
                        snapshot.menu, snapshot.path, snapshot.items
                    )
                    .into());
                }
                Ok(json!({"menu":snapshot.menu,"path":snapshot.path,"items":snapshot.items}))
            }
            Operation::PatchParityHostOnly => {
                Err("patch parity must run in the host-only test runner".into())
            }
        }
    }
}

#[cfg(test)]
mod sysex_completion_tests {
    use super::sysex_reply_complete;
    use crate::scenario::UsbSysexRequest;

    #[test]
    fn shared_deadline_handles_guest_wrap_and_wall_expiry() {
        use super::sysex_deadline_expired;
        use std::time::Duration;
        assert!(!sysex_deadline_expired(
            Duration::from_secs(1),
            u32::MAX - 2,
            2,
            5,
            2
        ));
        assert!(sysex_deadline_expired(
            Duration::from_secs(1),
            u32::MAX - 2,
            3,
            5,
            2
        ));
        assert!(sysex_deadline_expired(
            Duration::from_millis(2001),
            0,
            1,
            5,
            2
        ));
    }
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
            Operation::KnobDialogPreflightBegin { sd_knob: false },
            Operation::KnobProbePrepare {
                probe: crate::scenario::PreflightProbe::Descriptor,
            },
            Operation::KnobProbeInject,
            Operation::KnobProbeTrigger { delta: -1 },
            Operation::KnobProbeSample,
            Operation::KnobProbeRestore,
            Operation::KnobProbeAssert,
            Operation::KnobProbePrepare {
                probe: crate::scenario::PreflightProbe::TextAlias,
            },
            Operation::KnobProbeInject,
            Operation::KnobProbeTrigger { delta: -1 },
            Operation::KnobProbeSample,
            Operation::KnobProbeRestore,
            Operation::KnobProbeAssert,
            Operation::KnobProbePrepare {
                probe: crate::scenario::PreflightProbe::LayerCollision,
            },
            Operation::KnobProbeInject,
            Operation::KnobProbeTrigger { delta: -1 },
            Operation::KnobProbeSample,
            Operation::KnobProbeRestore,
            Operation::KnobProbeAssert,
            Operation::KnobProbePrepare {
                probe: crate::scenario::PreflightProbe::WidgetLayer,
            },
            Operation::KnobProbeInject,
            Operation::KnobProbeTrigger { delta: -1 },
            Operation::KnobProbeSample,
            Operation::KnobProbeRestore,
            Operation::KnobProbeAssert,
            Operation::KnobProbePrepare {
                probe: crate::scenario::PreflightProbe::BackgroundLayer,
            },
            Operation::KnobProbeInject,
            Operation::KnobProbeTrigger { delta: -1 },
            Operation::KnobProbeSample,
            Operation::KnobProbeRestore,
            Operation::KnobProbeAssert,
            Operation::KnobRaceBegin,
            Operation::KnobRaceWaitRegistration,
            Operation::KnobRaceArmCallback,
            Operation::KnobRaceWaitCallback,
            Operation::KnobRaceCapture {
                guest_ms: 150,
                artifact: "window-scene-callback-preflight-race".into(),
            },
            Operation::KnobRaceSample,
            Operation::KnobRaceRestore,
            Operation::KnobRaceAssert,
            Operation::KnobActivationTrigger,
            Operation::KnobActivationWait,
            Operation::KnobActivationSample,
            Operation::KnobActivationRestore,
            Operation::KnobActivationAssert,
            Operation::KnobActiveBegin,
            Operation::EnqueueEncoderNoAck {
                channel: 1,
                steps: 3,
            },
            Operation::CaptureDisplay {
                guest_ms: 500,
                artifact: "knob-dialog-active".into(),
            },
            Operation::KnobActiveSampleCallbacks,
            Operation::KnobActiveCheckCallbacks,
            Operation::KnobActiveCheckFrame,
            Operation::KnobActiveFinish,
            Operation::KnobLevelBegin,
            Operation::KnobLevelStart {
                demo: crate::scenario::adaptive::DemoCapture {
                    guest_ms: 100,
                    min_lit: 100,
                    checked_frames: 6,
                },
                artifact: "knob-dialog-demo-level-start".into(),
            },
            Operation::KnobLevelSweep {
                sweep: crate::scenario::adaptive::LevelSweep {
                    direction: crate::scenario::adaptive::LevelDirection::Up,
                    children: vec![],
                    min_percent: 69,
                    max_attempts: 128,
                    artifact_prefix: "up".into(),
                    demo: crate::scenario::adaptive::DemoCapture {
                        guest_ms: 100,
                        min_lit: 100,
                        checked_frames: 6,
                    },
                },
            },
            Operation::CaptureDisplay {
                guest_ms: 500,
                artifact: "knob-dialog-demo-level-peak".into(),
            },
            Operation::KnobLevelSweep {
                sweep: crate::scenario::adaptive::LevelSweep {
                    direction: crate::scenario::adaptive::LevelDirection::Down,
                    children: vec![],
                    min_percent: 0,
                    max_attempts: 128,
                    artifact_prefix: "down".into(),
                    demo: crate::scenario::adaptive::DemoCapture {
                        guest_ms: 100,
                        min_lit: 100,
                        checked_frames: 6,
                    },
                },
            },
            Operation::KnobLevelExpectZero,
            Operation::CaptureDisplay {
                guest_ms: 500,
                artifact: "knob-dialog-demo-level-zero".into(),
            },
            Operation::KnobLevelFinish,
            Operation::KnobTaskBegin,
            Operation::KnobTaskArmHook,
            Operation::EnqueueEncoderNoAck {
                channel: 1,
                steps: 1,
            },
            Operation::KnobTaskWaitHook,
            Operation::KnobTaskArmCallback,
            Operation::KnobTaskWaitCallback,
            Operation::KnobTaskAssertDistinct,
            Operation::KnobTaskRelease,
            Operation::CaptureDisplay {
                guest_ms: 150,
                artifact: "window-scene-task-context-probe".into(),
            },
            Operation::KnobTaskFinish,
            Operation::KnobOverlayBegin,
            Operation::KnobOverlayCheckWindow,
            Operation::KnobOverlaySnapshotDescriptors,
            Operation::KnobOverlayCheckDescriptor {
                field: crate::scenario::OverlayDescriptorField::Widget7,
            },
            Operation::KnobOverlayCheckDescriptor {
                field: crate::scenario::OverlayDescriptorField::Widget33,
            },
            Operation::KnobOverlayCheckDescriptor {
                field: crate::scenario::OverlayDescriptorField::Background10,
            },
            Operation::KnobOverlayCheckDescriptor {
                field: crate::scenario::OverlayDescriptorField::Background37,
            },
            Operation::KnobOverlayCheckDescriptor {
                field: crate::scenario::OverlayDescriptorField::Background48,
            },
            Operation::KnobOverlayFinishDescriptors,
            Operation::KnobOverlayCheckResource,
            Operation::KnobOverlayCheckCorners,
            Operation::KnobOverlayCheckSignature,
            Operation::KnobOverlayCheckBitmap,
            Operation::KnobOverlayCheckHeap,
            Operation::KnobOverlayFinish,
            Operation::KnobBitmapReactivate { delta: -1 },
            Operation::KnobBitmapWaitActive,
            Operation::KnobBitmapBegin,
            Operation::KnobBitmapCheckBackground {
                background: 47,
                expected: 0,
            },
            Operation::CaptureDisplay {
                guest_ms: 50,
                artifact: "window-scene-before-null-bitmap-probe".into(),
            },
            Operation::KnobBitmapStoreBefore,
            Operation::KnobBitmapArm,
            Operation::KnobBitmapWait,
            Operation::KnobBitmapInvoke { background: 47 },
            Operation::CaptureDisplay {
                guest_ms: 100,
                artifact: "window-scene-after-null-bitmap-probe".into(),
            },
            Operation::KnobBitmapCheckFrame,
            Operation::KnobBitmapCheckSignature,
            Operation::KnobBitmapCheckDescriptor,
            Operation::KnobBitmapFinish,
            Operation::KnobOverlayReactivate { delta: -1 },
            Operation::KnobOverlayWaitActive,
            Operation::KnobOverlayRefreshBegin,
            Operation::KnobOverlayRefreshValue,
            Operation::CaptureDisplay {
                guest_ms: 500,
                artifact: "window-scene-refreshed".into(),
            },
            Operation::KnobOverlayRefreshPixels,
            Operation::KnobOverlayRefreshFinish,
            Operation::KnobLockAcquire,
            Operation::KnobLockTrigger { delta: -1 },
            Operation::KnobLockWait,
            Operation::KnobLockSample,
            Operation::KnobLockRestore,
            Operation::KnobLockAssert,
            Operation::CaptureDisplay {
                guest_ms: 500,
                artifact: "window-scene-after-callback-contention".into(),
            },
            Operation::KnobLockFinish,
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
                knob_dialog_phase(
                    operation,
                    scope,
                    pending,
                    next_mode,
                    Some(0),
                    matches!(operation, Operation::CaptureDisplay { .. }) && before.4 == 104,
                    matches!(operation, Operation::CaptureDisplay { .. }) && before.4 == 113,
                    (matches!(operation, Operation::EnqueueEncoderNoAck { .. }) && before.4 == 47)
                        || (matches!(operation, Operation::CaptureDisplay { .. })
                            && before.4 == 48),
                    if (3..33).contains(&before.4) {
                        Some(usize::from((before.4 - 3) / 6))
                    } else {
                        None
                    },
                    matches!(operation, Operation::EnqueueEncoderNoAck { .. }) && before.4 == 63,
                    matches!(operation, Operation::CaptureDisplay { .. }) && before.4 == 69,
                    if matches!(operation, Operation::CaptureDisplay { .. }) && before.4 == 56 {
                        Some(2)
                    } else if matches!(operation, Operation::CaptureDisplay { .. })
                        && before.4 == 59
                    {
                        Some(5)
                    } else {
                        None
                    },
                    if matches!(operation, Operation::CaptureDisplay { .. }) && before.4 == 90 {
                        Some(1)
                    } else if matches!(operation, Operation::CaptureDisplay { .. })
                        && before.4 == 95
                    {
                        Some(6)
                    } else {
                        None
                    }
                ),
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
                    Operation::WaitKnobDisplayImage {
                        image: Path::new(KNOB_DIALOG_REFERENCES).join("pan.png"),
                        main_timeout_ms: 5000,
                    },
                    None,
                ),
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
        for (phase, op) in [
            Operation::KnobDialogAdaptive {
                plan: {
                    let fragment: crate::Fragment = toml::from_str(include_str!(
                        "../../../../patches/knob-dialog/tests/emulator/fragments/invalid-tags.toml"
                    ))
                    .unwrap();
                    match &fragment.step[0].action {
                        crate::Action::KnobDialogAdaptive { plan } => plan.clone(),
                        _ => panic!("missing adaptive plan"),
                    }
                },
            },
            Operation::KnobDialogInjectInvalidTag { tag: 15 },
            Operation::KnobDialogWaitInvalidTag,
            Operation::KnobDialogConfirmInvalidTag,
            Operation::KnobDialogInjectInvalidTag { tag: 240 },
            Operation::KnobDialogWaitInvalidTag,
            Operation::KnobDialogConfirmInvalidTag,
            Operation::KnobDialogSettleLevel {
                artifact: "before".into(),
            },
            Operation::KnobDialogInjectInvalidSignature,
            Operation::KnobDialogObserveInvalidSignature { guest_ms: 150 },
            Operation::KnobDialogSampleInvalidSignature,
            Operation::KnobDialogRestoreInvalidSignature,
            Operation::KnobDialogSettleLevel {
                artifact: "after".into(),
            },
            Operation::KnobDialogPrepareNotification,
            Operation::KnobDialogArmNotificationCallback,
            Operation::KnobDialogWaitNotificationCallback,
            Operation::KnobDialogInvokeNotification { message: 4 },
            Operation::KnobDialogWaitNotificationFrame {
                clear: false,
                artifact: "shown".into(),
                guest_ms: 100,
                limit_ms: 1500,
            },
            Operation::KnobDialogExpectNotificationInvariants { clear: false },
            Operation::KnobDialogWaitNotificationFrame {
                clear: true,
                artifact: "cleared".into(),
                guest_ms: 100,
                limit_ms: 5000,
            },
            Operation::KnobDialogExpectNotificationInvariants { clear: true },
            Operation::KnobDialogWaitExpiry {
                interval_ms: 100,
                limit_ms: 10_000,
            },
            Operation::CaptureDisplay {
                guest_ms: 150,
                artifact: "expired".into(),
            },
            Operation::KnobDialogExpectExpiryFrame { after: true },
            Operation::KnobDialogExpectExpiryFrame { after: false },
            Operation::KnobDialogSnapshotRestoration,
            Operation::KnobDialogCheckRestoration {
                field: crate::scenario::RestorationField::Window,
            },
            Operation::KnobDialogCheckRestoration {
                field: crate::scenario::RestorationField::Signature,
            },
            Operation::KnobDialogCheckRestoration {
                field: crate::scenario::RestorationField::Bitmap,
            },
            Operation::KnobDialogCheckRestoration {
                field: crate::scenario::RestorationField::BitmapDescriptor,
            },
            Operation::KnobDialogCheckRestoration {
                field: crate::scenario::RestorationField::NotificationSemaphore,
            },
            Operation::KnobDialogCheckRestoration {
                field: crate::scenario::RestorationField::TextResources,
            },
            Operation::KnobDialogCheckRestoration {
                field: crate::scenario::RestorationField::SharedPointer,
            },
            Operation::KnobDialogCheckRestoration {
                field: crate::scenario::RestorationField::SharedContents,
            },
            Operation::KnobDialogCheckRestoration {
                field: crate::scenario::RestorationField::TextLengths,
            },
            Operation::KnobDialogCheckRestoration {
                field: crate::scenario::RestorationField::Widget7,
            },
            Operation::KnobDialogCheckRestoration {
                field: crate::scenario::RestorationField::Widget33,
            },
            Operation::KnobDialogCheckRestoration {
                field: crate::scenario::RestorationField::Background10,
            },
            Operation::KnobDialogCheckRestoration {
                field: crate::scenario::RestorationField::Background37,
            },
            Operation::KnobDialogCheckRestoration {
                field: crate::scenario::RestorationField::Background48,
            },
            Operation::KnobDialogCheckRestoration {
                field: crate::scenario::RestorationField::Callback,
            },
            Operation::KnobDialogFinishRestoration,
            Operation::KnobDialogExpectRestoredHeap,
            Operation::EnqueueEncoderNoAck {
                channel: 1,
                steps: 3,
            },
            Operation::CaptureDisplay {
                guest_ms: 400,
                artifact: "before-nav".into(),
            },
            Operation::ReleaseCapturedDisplay,
            Operation::EnqueueTapNoAck {
                target: 0,
                hold_ms: 120,
            },
            Operation::CaptureDisplay {
                guest_ms: 500,
                artifact: "after-nav".into(),
            },
            Operation::ReleaseCapturedDisplay,
            Operation::KnobDialogSnapshotNavigation,
            Operation::KnobDialogCheckNavigation {
                field: crate::scenario::NavigationField::Window,
            },
            Operation::KnobDialogCheckNavigation {
                field: crate::scenario::NavigationField::Signature,
            },
            Operation::KnobDialogCheckNavigation {
                field: crate::scenario::NavigationField::Bitmap,
            },
            Operation::KnobDialogCheckNavigation {
                field: crate::scenario::NavigationField::BitmapDescriptor,
            },
            Operation::KnobDialogCheckNavigation {
                field: crate::scenario::NavigationField::NotificationSemaphore,
            },
            Operation::KnobDialogCheckNavigation {
                field: crate::scenario::NavigationField::SharedContents,
            },
            Operation::KnobDialogCheckNavigation {
                field: crate::scenario::NavigationField::Widget7,
            },
            Operation::KnobDialogCheckNavigation {
                field: crate::scenario::NavigationField::Widget33,
            },
            Operation::KnobDialogCheckNavigation {
                field: crate::scenario::NavigationField::Background10,
            },
            Operation::KnobDialogCheckNavigation {
                field: crate::scenario::NavigationField::Background37,
            },
            Operation::KnobDialogCheckNavigation {
                field: crate::scenario::NavigationField::Background48,
            },
            Operation::KnobDialogCheckNavigation {
                field: crate::scenario::NavigationField::Callback,
            },
            Operation::KnobDialog,
        ]
        .into_iter()
        .enumerate()
        {
            let before = shared.unwrap();
            assert_eq!(
                knob_dialog_phase(
                    &op,
                    None,
                    None,
                    Some(3),
                    Some(phase as u8),
                    false,
                    false,
                    false,
                    None,
                    false,
                    false,
                    None,
                    None
                ),
                Some(before.4)
            );
            shared = advance_knob_deadline(matches!(op, Operation::KnobDialog), before);
        }
        assert!(shared.is_none());
    }
}
