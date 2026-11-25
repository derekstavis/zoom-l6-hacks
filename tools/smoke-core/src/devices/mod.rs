//! Typed peripheral operations. Sequencing, timing and artifacts stay in the runner.
use crate::{Scenario, Session, scenario::PadAssignmentSource, waveform};
use firmware_api::{l6, l6max};
use l6max_host::{engine::Options, headless::Headless};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    thread,
    time::{Duration, Instant},
};

mod audio;
mod display;
mod editor;
mod execute;
mod knob_dialog;
mod lifecycle;
mod midi;
mod midi_probe;
mod mode_primitives;
mod panel;
mod sd;
mod usb;

use audio::*;
pub use display::*;
use editor::*;
pub use knob_dialog::*;
pub use lifecycle::*;
use midi::*;
use midi_probe::*;
use mode_primitives::{
    embedded_expiry_operations,
    embedded_invalid_signature_operations,
    embedded_invalid_tag_operations,
    embedded_midi_sync_operations,
    embedded_mode_operations,
    embedded_notification_operations,
    embedded_restoration_operations,
};
use panel::*;
use sd::*;
use usb::*;

/// Return a short, per-case GDB socket path. Darwin's QEMU GDB stub rejects
/// UNIX socket paths longer than 104 bytes, while artifact directories can be
/// arbitrarily deep. Hashing the case path preserves isolation without tying
/// the socket name length to the artifact directory.
pub fn gdb_socket_path(logs: &Path, purpose: &str) -> PathBuf {
    let digest = Sha256::digest(logs.as_os_str().as_encoded_bytes());
    let case = digest[..8]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    Path::new("/tmp").join(format!("l6g{case}-{purpose}.sock"))
}

#[cfg(test)]
mod gdb_socket_tests {
    use super::gdb_socket_path;
    use std::path::Path;

    #[test]
    fn paths_are_short_and_isolated_even_with_long_artifact_paths() {
        let long = Path::new(
            "/private/tmp/very-long-output-name/case-with-a-very-long-name-l6max-headless",
        );
        let main = gdb_socket_path(long, "main");
        let panel = gdb_socket_path(long, "panel");
        assert!(main.as_os_str().as_encoded_bytes().len() < 104);
        assert_ne!(main, panel);
        assert_ne!(main, gdb_socket_path(Path::new("/tmp/other-case"), "main"));
    }
}

fn midi_namespace() -> Option<String> {
    std::env::var("L6_MIDI_NAMESPACE")
        .ok()
        .filter(|namespace| !namespace.is_empty())
}

#[derive(Clone, Debug)]
pub enum Operation {
    LifecycleStart {
        profile: crate::scenario::LifecycleBoot,
    },
    LifecycleStop,
    WaitPanelUptime {
        guest_ms: u32,
    },
    ExpectUpdateState {
        mode: u32,
        flags_mask: u8,
        flags_value: u8,
    },
    ExpectUpdateMarkerAbsent,
    ExpectLiveUpdateMarkerAbsent,
    CaptureUpdateMarkerPending,
    MarkL6UpdatePending,
    ExpectL6UpdateResetVector,
    ExpectL6UpdateComparison {
        field: crate::scenario::L6UpdateField,
    },
    ExpectInstallerOutcome {
        expected: crate::scenario::InstallerExpected,
    },
    WaitUpdatePanelReady {
        require_programming: bool,
    },
    WaitUsbController,
    BeginWallDeadline {
        duration_s: u64,
    },
    EndWallDeadline,
    ExpectUpdateMode {
        mode: u32,
    },
    ExpectMainMemory {
        address: u32,
        bytes: Vec<u8>,
    },
    ExpectUpdateComparison {
        field: crate::scenario::UpdateComparisonField,
    },
    CaptureInstalledPanel,
    ExpectPanelUnchanged,
    ExpectUpdateGui {
        window: u32,
        progress_mask: u32,
        progress_value: u32,
    },
    SdCard {
        inserted: bool,
    },
    UsbDescriptors,
    UsbStorage,
    UsbMidi {
        value: u8,
    },
    HostMidiRoundTrip,
    MidiProbeBegin,
    MidiProbeSendCc {
        port: crate::scenario::MidiProbePort,
        channel: u8,
        controller: u8,
        value: u8,
        capture_ring_channel: usize,
    },
    MidiProbeExpectRing {
        channel: usize,
        changed: bool,
        guest_ms: Option<u32>,
        wall_ms: u32,
    },
    MidiProbeWaitRingStable {
        channel: usize,
        guest_ms: u32,
        wall_ms: u32,
    },
    MidiProbeDrainAndTap {
        port: crate::scenario::MidiProbePort,
        target: u32,
        hold_ms: u32,
    },
    MidiProbeExpectNoEvents {
        port: crate::scenario::MidiProbePort,
        wall_ms: u32,
    },
    MidiProbeEnd,
    HostMidiConnect,
    HostMidiBegin,
    HostMidiSyncBegin,
    HostMidiExpectSync,
    HostMidiDrain {
        quiet_wall_ms: u32,
    },
    HostMidiExpectCount {
        count: usize,
    },
    HostMidiEnd,
    EnqueueEncoder {
        channel: usize,
        steps: i32,
        midi_transaction: bool,
    },
    HostMidiExpectCc {
        channel: u8,
        controller: u8,
    },
    HostMidiSettle {
        guest_ms: u32,
    },
    HostMidiSendCc {
        channel: u8,
        controller: u8,
        value: u8,
    },
    ExpectRingChanged {
        channel: usize,
        before: [u32; 8],
        midi_transaction: bool,
    },

    DirectUsbMidiRoundTrip {
        cable: u8,
        expect_ring_change: bool,
    },
    L6DinUsbForwarding,
    L6MidiProbeBegin,
    L6MidiProbeInject,
    L6MidiProbeExpectHook,
    L6MidiProbeExpectReplies,
    L6MidiRingPinSweep {
        cc: u8,
    },
    L6MidiMuteIndicators,
    UsbSysexBegin,
    UsbSysexExchange {
        request: crate::scenario::UsbSysexRequest,
        complete_on_match: bool,
    },
    UsbSysexEnd,
    EditorPadAssignments,
    EditorPadPlayback {
        pad: u8,
    },
    L6maxUiPadVerify {
        pad: u8,
    },
    SoundPadPlayback {
        pad: u8,
        target: u32,
        assignment: PadAssignmentSource,
        require_pad_led_change: bool,
        playback_window_ms: u32,
        require_stereo_meter: bool,
        expect_source: Option<String>,
        max_residual_db: f64,
    },
    WaitL6SoundPadFile {
        path: String,
    },
    L6RecorderPlayback {
        target: u32,
    },
    L6maxRecorderPlayback {
        target: u32,
    },
    ExpectMemory {
        symbol: String,
        address: u32,
        bytes: Vec<u8>,
    },
    WaitMemory {
        symbol: String,
        address: u32,
        bytes: Vec<u8>,
    },
    WaitSdPatchLoaded,
    WaitSdPatchEmpty,
    ExpectSdLoader,
    ExpectSdModule {
        loaded: bool,
        chain_count: Option<u8>,
    },
    EffectPacedTurn {
        channel: usize,
        steps: i32,
    },
    EffectParameterEdit {
        encoder: usize,
        steps: i32,
    },
    WaitEffectMode {
        panel_mode: Option<u8>,
        edit_mode: Option<bool>,
    },
    EffectReplacementState {
        expected_effect_type: Option<u8>,
        expected_processor_type: Option<u8>,
        expected_panel_mode: Option<u8>,
        expected_edit_mode: Option<bool>,
        minimum_parameter_edits: Option<u32>,
        expected_parameter_id: Option<u8>,
        expected_parameter_value: Option<f32>,
        expected_dialog_active: Option<bool>,
        minimum_exit_ring_refreshes: Option<u32>,
        expected_effect: Option<String>,
        require_output: bool,
        wait_for_slot: bool,
    },
    ExpectHeap {
        minimum_free: u32,
    },
    HoldChord {
        targets: Vec<u32>,
        duration_ms: Option<u32>,
        until: Option<crate::scenario::ChordCondition>,
    },
    AudioCallbacks,
    KnobDialogReady,
    KnobDialogGuard,
    KnobDialogBaseline,
    KnobDialogPreflightBegin,
    KnobDialogPreflightDescriptor,
    KnobDialogPreflightTextAlias,
    KnobDialogPreflightLayerCollision,
    KnobDialogPreflightWidgetLayer,
    KnobDialogPreflightBackgroundLayer,
    KnobDialogCallbackRace,
    KnobDialogFirstActivation,
    KnobDialogActiveDisplay,
    KnobDialogLevelDemo,
    KnobDialogTaskContexts,
    KnobDialogOverlayInvariants,
    KnobDialogBitmapZero,
    KnobDialogActiveOverlay,
    KnobDialogLockedUpdate,
    KnobDialogModeBegin {
        scene: String,
    },
    EnqueueTap {
        target: u32,
        hold_ms: u32,
    },
    WaitInputApplied,
    WaitKnobParameterMode {
        mode: u8,
    },
    CaptureDisplay {
        guest_ms: u32,
        artifact: String,
    },
    ExpectCapturedDisplayImage {
        image: PathBuf,
    },
    KnobDialogExpectHeap,
    KnobDialogModeEnd,
    KnobDialogFinalBegin,
    KnobDialogInjectInvalidTag {
        tag: u8,
    },
    KnobDialogWaitInvalidTag,
    KnobDialogConfirmInvalidTag,
    KnobDialogSettleLevel {
        artifact: String,
    },
    KnobDialogInjectInvalidSignature,
    KnobDialogObserveInvalidSignature {
        guest_ms: u32,
    },
    KnobDialogSampleInvalidSignature,
    KnobDialogRestoreInvalidSignature,
    KnobDialogPrepareNotification,
    KnobDialogArmNotificationCallback,
    KnobDialogWaitNotificationCallback,
    KnobDialogInvokeNotification {
        message: u32,
    },
    KnobDialogWaitNotificationFrame {
        clear: bool,
        artifact: String,
        guest_ms: u32,
        limit_ms: u32,
    },
    KnobDialogExpectNotificationInvariants {
        clear: bool,
    },
    KnobDialogWaitExpiry {
        interval_ms: u32,
        limit_ms: u32,
    },
    KnobDialogExpectExpiryFrame {
        after: bool,
    },
    KnobDialogSnapshotRestoration,
    KnobDialogCheckRestoration {
        field: crate::scenario::RestorationField,
    },
    KnobDialogFinishRestoration,
    KnobDialogExpectRestoredHeap,
    KnobDialog,
    KnobPopupOpen {
        first_steps: i32,
        fallback_steps: i32,
        attempt_guest_ms: u32,
    },
    KnobPopupExpectOpen,
    KnobPopupExpectValue,
    ExpectMenu {
        menu: String,
        path: Vec<String>,
        items: Vec<String>,
    },
    FirmwareUpdate,
    WasmPatch,
    WarmReset,
    Restart,
    ExpectRecorder,
    PowerPress {
        hold_ms: u32,
        powered: bool,
    },
    ExpectSettings {
        restored: bool,
    },
    PanelEvent {
        target: u32,
        column: u8,
        row: u8,
        event: u8,
    },
    L6MainGpioEvent {
        target: u32,
        event: u8,
    },
    L6UsbStartup,
    L6DinEventDispatch {
        group: u8,
        cc: u8,
        value: u8,
    },
    L6maxTapHandler,
    WaitRecordedFiles {
        files: Vec<String>,
    },
    AudioInputTransport {
        command: String,
        position_ms: i32,
    },
    AudioInputLoad {
        channel: u32,
        loaded: bool,
    },
    WaitAudioInput {
        playing: bool,
        min_ms: u32,
        max_ms: u32,
    },
    ExpectAudioInput {
        playing: bool,
        min_ms: u32,
        max_ms: u32,
    },
    ExpectTone {
        file: Option<String>,
        hz: f64,
        levels: [Option<f64>; 2],
        tolerance_db: f64,
    },
    ExpectCardEntry {
        path: String,
        size: Option<u64>,
        matches_state: Option<String>,
    },
    L6InputSweep,
    L6DinSweep,
    L6maxInputTiming,
    L6maxPanelKeyMap,
    L6maxChannelSignalPwm,
    L6LevelMeterPwm,
}

fn uses_native_host_midi(scenario: &Scenario, model: l6fw::Model) -> bool {
    scenario.step.iter().any(|step| {
        step.applies_to(model)
            && matches!(
                step.action,
                crate::Action::HostMidiRoundTrip | crate::Action::HostMidiConnect
            )
    })
}

fn midi_bridge_options(scenario: &Scenario, model: l6fw::Model) -> Option<(bool, bool)> {
    let native = uses_native_host_midi(scenario, model);
    let direct = scenario.step.iter().any(|step| {
        step.applies_to(model)
            && matches!(step.action, crate::Action::DirectUsbMidiRoundTrip { .. })
    });
    let din = scenario
        .step
        .iter()
        .any(|step| step.applies_to(model) && matches!(step.action, crate::Action::MidiProbeBegin));
    (native || direct || din).then_some((native, din))
}

fn validate_native_midi_capabilities(
    scenario: &Scenario,
    model: l6fw::Model,
) -> Result<(), String> {
    let native = scenario.step.iter().any(|step| {
        step.applies_to(model) && matches!(step.action, crate::Action::HostMidiConnect)
    });
    let direct = scenario.step.iter().any(|step| {
        step.applies_to(model)
            && matches!(
                step.action,
                crate::Action::DirectUsbMidiRoundTrip { .. }
                    | crate::Action::HostMidiRoundTrip
                    | crate::Action::UsbDescriptors
                    | crate::Action::UsbStorage
                    | crate::Action::UsbMidi { .. }
                    | crate::Action::UsbSysexBegin
                    | crate::Action::UsbSysexExchange { .. }
                    | crate::Action::UsbSysexEnd
                    | crate::Action::EditorPadAssignments
                    | crate::Action::EditorPadPlayback { .. }
                    | crate::Action::L6maxUiPadVerify { .. }
                    | crate::Action::L6DinSweep
                    | crate::Action::L6MidiProbeBegin
                    | crate::Action::L6UsbStartup
                    | crate::Action::L6DinEventDispatch { .. }
                    | crate::Action::L6maxChannelSignalPwm
                    | crate::Action::L6LevelMeterPwm
            )
    });
    if native && direct {
        return Err("host-midi-connect conflicts with another MIDI consumer or incompatible device instrumentation in this scenario".into());
    }
    Ok(())
}

/// Configure only the hardware instrumentation explicitly requested by a scenario.
pub fn prepare(scenario: &Scenario, options: &mut Options) -> Result<(), String> {
    prepare_with_report(scenario, options, None)
}

pub(crate) fn prepare_with_report(
    scenario: &Scenario,
    options: &mut Options,
    report_directory: Option<&Path>,
) -> Result<(), String> {
    // Managed patched updates boot stock before the verification generation.
    // Apply hardware requirements and firmware guards to that phase only;
    // the verify generation is checked against its own frozen build at startup.
    let mut initial_phase = None;
    if scenario
        .update_fixture
        .is_some_and(crate::scenario::UpdateFixture::is_patched)
    {
        let mut initial = scenario.clone();
        let boundary = initial
            .step
            .iter()
            .position(|step| {
                matches!(
                    step.action,
                    crate::Action::LifecycleStart {
                        profile: crate::scenario::LifecycleBoot::Verify
                    }
                )
            })
            .ok_or("patched update has no verify boot")?;
        initial.step.truncate(boundary);
        initial_phase = Some(initial);
    }
    let scenario = initial_phase.as_ref().unwrap_or(scenario);
    if scenario.step.iter().any(|step| {
        matches!(
            step.action,
            crate::Action::L6maxChannelSignalPwm | crate::Action::L6LevelMeterPwm
        )
    }) {
        options.usb_enabled = false;
        options.midi_enabled = false;
        options.gdb_socket = Some(gdb_socket_path(&options.logs, "l6sp"));
    }
    if scenario.name == "factory-protocol" {
        prepare_factory_fixture(options)?;
    }
    let main = selected_main_image(options)?;
    let model = l6fw::model_from_main(&main).map_err(|e| e.to_string())?;
    validate_native_midi_capabilities(scenario, model)?;
    crate::requirements::check_firmware(
        scenario,
        model,
        &crate::firmware::identify(&main, report_directory.unwrap_or(&options.logs)),
    )?;
    if scenario.step.iter().any(|s| {
        matches!(
            s.action,
            crate::Action::AudioCallbacks
                | crate::Action::ExpectSettings { .. }
                | crate::Action::ExpectRecorder
        )
    }) {
        let main =
            fs::read(options.firmware_dir.join("main_firmware.bin")).map_err(|e| e.to_string())?;
        if let Some(path) = options
            .state_dir
            .as_ref()
            .map(|p| p.join("main-nor.bin"))
            .filter(|p| p.exists())
        {
            let nor = fs::read(path).map_err(|e| e.to_string())?;
            let offset = 0x50000; // l6fw::model_from_main_nor and the modeled installer.
            if nor.get(offset..offset + main.len()) != Some(main.as_slice()) {
                return Err("installed MAIN differs from selected firmware revision".into());
            }
        }
    }
    if scenario
        .step
        .iter()
        .any(|s| matches!(s.action, crate::Action::AudioCallbacks))
    {
        options.start_paused = true;
        options.gdb_socket = Some(gdb_socket_path(&options.logs, "audio"));
    }
    if scenario
        .step
        .iter()
        .any(|s| matches!(s.action, crate::Action::PanelEvent { .. }))
    {
        options.gdb_socket = Some(gdb_socket_path(&options.logs, "panel-event-map"));
    }
    if scenario
        .step
        .iter()
        .any(|s| matches!(s.action, crate::Action::L6MainGpioEvent { .. }))
    {
        options.gdb_socket = Some(gdb_socket_path(&options.logs, "l6-main-gpio-event"));
    }
    if scenario
        .step
        .iter()
        .any(|s| matches!(s.action, crate::Action::L6maxTapHandler))
    {
        options.gdb_socket = Some(gdb_socket_path(&options.logs, "l6max-tap-handler"));
    }
    if scenario
        .step
        .iter()
        .any(|s| s.applies_to(model) && matches!(s.action, crate::Action::L6UsbStartup))
    {
        options.start_paused = true;
        options.usb_enabled = false;
        options.midi_enabled = false;
        options.gdb_socket = Some(gdb_socket_path(&options.logs, "l6-usb-startup"));
    }
    if scenario.step.iter().any(|s| {
        s.applies_to(model) && matches!(s.action, crate::Action::L6DinEventDispatch { .. })
    }) {
        options.start_paused = true;
        options.usb_enabled = false;
        options.midi_enabled = false;
        options.gdb_socket = Some(gdb_socket_path(&options.logs, "l6ed"));
    }
    if scenario
        .step
        .iter()
        .any(|s| s.applies_to(model) && matches!(s.action, crate::Action::L6DinSweep))
    {
        options.usb_enabled = false;
        options.midi_enabled = true;
        options.gdb_socket = Some(gdb_socket_path(&options.logs, "l6-din-sweep"));
    }
    if let Some((usb, din)) = midi_bridge_options(scenario, model) {
        // A native mixer subscription and DIN check share the USB bridge;
        // without a native subscription, the DIN check opens USB directly.
        options.usb_enabled = usb;
        options.midi_enabled = din;
    }
    if scenario
        .step
        .iter()
        .any(|s| s.applies_to(model) && matches!(s.action, crate::Action::L6MidiProbeBegin))
    {
        options.usb_enabled = false;
        options.midi_enabled = false;
        options.gdb_socket = Some(gdb_socket_path(&options.logs, "l6-midi-sync"));
    }
    options.trace_sd |= scenario.trace_sd
        || scenario
            .step
            .iter()
            .any(|s| matches!(s.action, crate::Action::SdCard { .. }));
    // Typed USB operations own the host tokens; present VBUS from boot so the
    // firmware initializes its USB class tasks, without starting the native
    // MIDI bridge which would compete for those same endpoints.
    if scenario.step.iter().any(|s| {
        s.applies_to(model)
            && matches!(
                s.action,
                crate::Action::UsbDescriptors
                    | crate::Action::UsbStorage
                    | crate::Action::UsbMidi { .. }
                    | crate::Action::UsbSysexBegin
                    | crate::Action::UsbSysexExchange { .. }
                    | crate::Action::UsbSysexEnd
                    | crate::Action::EditorPadAssignments
                    | crate::Action::EditorPadPlayback { .. }
                    | crate::Action::L6maxUiPadVerify { .. }
            )
    }) {
        options.usb_enabled = false;
        options.usb_cable_at_start = true;
        if !scenario.step.iter().any(|step| {
            step.applies_to(model) && matches!(step.action, crate::Action::MidiProbeBegin)
        }) {
            options.midi_enabled = false;
        }
    }
    Ok(())
}

pub(crate) fn selected_main_image(options: &Options) -> Result<Vec<u8>, String> {
    let source = fs::read(options.firmware_dir.join("main_firmware.bin"))
        .map_err(|error| error.to_string())?;
    let Some(nor_path) = options
        .state_dir
        .as_ref()
        .map(|state| state.join("main-nor.bin"))
        .filter(|path| path.is_file())
    else {
        return Ok(source);
    };
    let nor = fs::read(nor_path).map_err(|error| error.to_string())?;
    let start = 0x50000usize;
    let end = start
        .checked_add(source.len())
        .ok_or_else(|| "installed MAIN range overflow".to_string())?;
    let installed = nor
        .get(start..end)
        .ok_or_else(|| "persistent NOR does not contain a complete MAIN image".to_string())?;
    Ok(installed.to_vec())
}

fn thumb_bl_target(address: u32, instruction: [u8; 4]) -> Option<u32> {
    let first = u16::from_le_bytes([instruction[0], instruction[1]]);
    let second = u16::from_le_bytes([instruction[2], instruction[3]]);
    if first & 0xf800 != 0xf000 || second & 0xd000 != 0xd000 {
        return None;
    }
    let sign = ((first >> 10) & 1) as u32;
    let j1 = ((second >> 13) & 1) as u32;
    let j2 = ((second >> 11) & 1) as u32;
    let i1 = (!(j1 ^ sign)) & 1;
    let i2 = (!(j2 ^ sign)) & 1;
    let immediate = (sign << 24)
        | (i1 << 23)
        | (i2 << 22)
        | (((first & 0x03ff) as u32) << 12)
        | (((second & 0x07ff) as u32) << 1);
    let displacement = ((immediate as i32) << 7) >> 7;
    Some(address.wrapping_add(4).wrapping_add(displacement as u32))
}

#[derive(Default)]
pub struct State {
    paced_progress: crate::progress::CaseProgress,
    paced_index: usize,
    saved: Option<(Vec<u8>, usize)>,
    midi: Option<l6max_host::usb::Endpoints>,
    sysex_deadline: Option<(Instant, u32, u32, u64)>,
    gdb: Option<l6max_diagnostics::gdb::Gdb>,
    host_midi: Option<Box<dyn crate::host_midi::Client>>,
    encoder_turned_up: bool,
    knob_popup: Option<knob_dialog::PopupContext>,
    knob_dialog_baseline: Option<[u8; 1024]>,
    knob_dialog_preflight: Option<knob_dialog::PreflightContext>,
    knob_dialog_deadline: Option<(std::time::Instant, u32, u32, u64, u16)>,
    knob_mode: Option<KnobModeScope>,
    knob_heap_reader: Option<l6max_host::heap::CounterReader>,
    pending_input: Option<PendingInput>,
    captured_display: Option<CapturedDisplay>,
    midi_round_trip: Option<MidiRoundTrip>,
    midi_sync: Option<midi::MidiSync>,
    l6_midi_probe: Option<midi::L6MidiProbe>,
    sd_inspection: Option<sd::SdInspectionContext>,
    native_midi: Option<l6ctl::midi::MidiSession>,
    midi_probe: Option<MidiProbeContext>,
}

struct KnobModeScope {
    index: usize,
    scene: String,
    selected_mode: Option<u8>,
}

struct PendingInput {
    source: l6ctl::InputSource,
    applied: u64,
    deadline: Instant,
    kind: PendingInputKind,
}

#[derive(Clone, Copy)]
enum PendingInputKind {
    Tap,
    Encoder,
}

struct CapturedDisplay {
    frame: [u8; 1024],
    artifact: String,
}

impl State {
    pub fn set_paced_progress(&mut self, progress: crate::progress::CaseProgress, index: usize) {
        self.paced_progress = progress;
        self.paced_index = index;
    }
    pub fn take_host_midi(&mut self) -> Option<Box<dyn crate::host_midi::Client>> {
        self.native_midi
            .take()
            .map(|client| Box::new(client) as Box<dyn crate::host_midi::Client>)
            .or_else(|| self.host_midi.take())
    }

    fn host_midi_client(&mut self) -> Option<&mut (dyn crate::host_midi::Client + '_)> {
        if let Some(client) = self.native_midi.as_mut() {
            return Some(client);
        }
        match self.host_midi.as_mut() {
            Some(client) => Some(client.as_mut()),
            None => None,
        }
    }
}

fn wait_l6_input(
    session: &Session,
    mut ready: impl FnMut() -> bool,
    what: &str,
) -> Result<(), String> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        session.poll()?;
        if ready() {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(format!("timed out waiting for L6 {what}"));
        }
        thread::sleep(Duration::from_millis(5));
    }
}

/// Let the guest run under `gdb` until it stops, and return the stop reply.
/// It may run for `guest_budget_ms` of its own time; a loaded host stretches
/// wall time, so the guest budget decides, and `wall_cap` only ends a guest
/// that no longer advances.
fn continue_within_guest(
    session: &Session,
    gdb: &mut l6max_diagnostics::gdb::Gdb,
    guest_budget_ms: u32,
    wall_cap: Duration,
) -> Result<String, String> {
    let main = &session
        .engine
        .inputs
        .as_ref()
        .ok_or("input hub unavailable")?
        .main;
    let start = main.snapshot().guest_ms;
    let deadline = Instant::now() + wall_cap;
    gdb.resume().map_err(|error| error.to_string())?;
    loop {
        if let Some(reply) = gdb
            .poll_stop(Duration::from_millis(200))
            .map_err(|error| error.to_string())?
        {
            return Ok(reply);
        }
        session.poll()?;
        let ran = main.snapshot().guest_ms.wrapping_sub(start);
        if ran > guest_budget_ms || Instant::now() >= deadline {
            let _ = gdb.interrupt();
            return Err(format!(
                "the guest ran {ran} ms without stopping (budget {guest_budget_ms} ms)"
            ));
        }
    }
}

fn guest_now(session: &Session) -> u32 {
    session
        .engine
        .inputs
        .as_ref()
        .map_or(0, |inputs| inputs.panel.snapshot().guest_ms)
}

fn wait_guest_duration(
    session: &Session,
    duration_ms: u32,
    deadline: Instant,
) -> Result<(), String> {
    let inputs = session
        .engine
        .inputs
        .as_ref()
        .ok_or("guest wait needs inputs")?;
    let start = inputs.panel.snapshot().guest_ms;
    while inputs.panel.snapshot().guest_ms.wrapping_sub(start) < duration_ms {
        session.poll()?;
        if Instant::now() >= deadline {
            return Err(format!(
                "guest did not advance {duration_ms} ms before deadline"
            ));
        }
        thread::sleep(Duration::from_millis(5));
    }
    Ok(())
}

fn wait_guest(
    session: &Session,
    start: u32,
    duration: u32,
    guest_timeout_ms: u32,
    wall_timeout_s: u64,
) -> Result<(), Box<dyn std::error::Error>> {
    let wall = Instant::now();
    loop {
        session.poll()?;
        let elapsed = session
            .engine
            .inputs
            .as_ref()
            .unwrap()
            .panel
            .snapshot()
            .guest_ms
            .wrapping_sub(start);
        if elapsed >= duration {
            return Ok(());
        }
        if elapsed > guest_timeout_ms || wall.elapsed() > Duration::from_secs(wall_timeout_s) {
            return Err("knob-dialog setup exceeded its deadline".into());
        }
        thread::sleep(Duration::from_millis(5));
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn preparation_can_use_a_separately_staged_firmware_report() {
        use super::{Options, Scenario, prepare, prepare_with_report};
        use sha2::{Digest, Sha256};
        let root = std::env::temp_dir().join(format!("l6-verify-report-{}", std::process::id()));
        let firmware = root.join("firmware");
        let logs = root.join("logs");
        std::fs::create_dir_all(&firmware).unwrap();
        std::fs::create_dir_all(&logs).unwrap();
        let mut main = vec![0; 8];
        main[..4].copy_from_slice(&0x20220000u32.to_le_bytes());
        main[4..].copy_from_slice(&0x80001415u32.to_le_bytes());
        std::fs::write(firmware.join("main_firmware.bin"), &main).unwrap();
        let hash = format!("{:x}", Sha256::digest(&main));
        std::fs::write(
            firmware.join(crate::fixture_layout::FIRMWARE_REPORT),
            format!(r#"{{"patched_sha256":"{hash}"}}"#),
        )
        .unwrap();
        let scenario = Scenario::from_text("version=1\nname='verify'\nmodels=['l6']\nbackends=['headless']\n[[step]]\naction='delay'\nguest_ms=1\n").unwrap();
        let mut options = Options::parse_args(std::iter::empty::<std::ffi::OsString>()).unwrap();
        options.firmware_dir = firmware.clone();
        options.logs = logs;
        options.state_dir = None;
        assert!(prepare(&scenario, &mut options).is_err());
        assert!(prepare_with_report(&scenario, &mut options, Some(&firmware)).is_ok());
        std::fs::remove_dir_all(root).unwrap();
    }
    use super::{
        create_factory_card, display_difference_count, editor_pad_assignment_matches,
        editor_pad_file_count, editor_pad_filename_valid, knob_display_matches, sound_pad_led,
        verify_sai_edma_trace,
    };
    use l6fw::Model;
    use std::{
        fs,
        time::{SystemTime, UNIX_EPOCH},
    };

    #[test]
    fn sound_pad_playback_uses_the_selected_models_led_wiring() {
        let mut l6_rows = [0; 8];
        l6_rows[3] = 1 << 15;
        assert!(sound_pad_led(Model::L6, l6_rows, 0));
        assert!(!sound_pad_led(Model::L6max, l6_rows, 0));

        let mut l6max_rows = [0; 8];
        l6max_rows[1] = 1 << 21;
        assert!(sound_pad_led(Model::L6max, l6max_rows, 0));
        assert!(!sound_pad_led(Model::L6, l6max_rows, 0));
    }

    #[test]
    fn factory_card_contains_the_jig_marker_file() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path =
            std::env::temp_dir().join(format!("factory-jig-{}-{nonce}.img", std::process::id()));
        create_factory_card(&path).unwrap();
        let contents = sd_image::open(&path)
            .unwrap()
            .read("D445_JIG_000.txt")
            .unwrap();
        assert_eq!(contents, b"emulator fixture; no calibration parameters\n");
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn sai_edma_trace_checks_model_specific_fifo_and_descriptor_paths() {
        let trace = concat!(
            "write addr=40384008 value=80040001 at=10ms\n",
            "write addr=40384088 value=80040001 at=11ms\n",
            "write addr=40384020 value=00000001 at=12ms\n",
            "write addr=400e91c0 value=00000001 at=13ms\n",
            "write addr=403840a0 value=00000001 at=14ms\n",
            "write addr=400e91e0 value=00000001 at=15ms\n",
            "write addr=4038c008 value=80040001 at=16ms\n",
            "write addr=4038c088 value=80040001 at=17ms\n",
            "write addr=4038c020 value=00000001 at=18ms\n",
            "write addr=400e9180 value=00000001 at=19ms\n",
            "write addr=4038c0a0 value=00000001 at=20ms\n",
            "write addr=400e91a0 value=00000001 at=21ms\n",
            "write addr=40384020 value=00000001 at=22ms\n",
            "write addr=400e91c0 value=00000001 at=23ms\n",
            "write addr=403840a0 value=00000001 at=24ms\n",
            "write addr=400e91e0 value=00000001 at=25ms\n",
        );
        assert!(verify_sai_edma_trace(Model::L6, trace).is_ok());
        assert!(verify_sai_edma_trace(Model::L6max, trace).is_ok());
        let without_sai2 = trace
            .lines()
            .filter(|line| !line.contains("4038c"))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(verify_sai_edma_trace(Model::L6max, &without_sai2).is_ok());
        assert!(verify_sai_edma_trace(Model::L6, &without_sai2).is_err());
    }

    #[test]
    fn editor_pad_reply_parser_distinguishes_empty_and_confirmed_assignments() {
        let count = vec![240, 82, 0, 0, 69, 0, 2, 0, 4, 247];
        assert_eq!(editor_pad_file_count(&[count], 2), Some(4));
        let none_assigned = vec![240, 82, 0, 0, 69, 1, 2, 127, 127, 0, 0, 247];
        assert!(!editor_pad_filename_valid(&none_assigned, 2));
        let filename = vec![240, 82, 0, 0, 69, 1, 2, 0, 0, 4, 0, 1, 2, 3, 4, 247];
        assert!(editor_pad_filename_valid(&filename, 2));
        let args = filename[6..filename.len() - 1].to_vec();
        let assigned = vec![240, 82, 0, 0, 69, 2]
            .into_iter()
            .chain(args.iter().copied())
            .chain([247])
            .collect::<Vec<_>>();
        assert!(editor_pad_assignment_matches(&[assigned.clone()], 2, &args));
        assert!(!editor_pad_assignment_matches(&[assigned], 1, &args));
    }

    #[test]
    fn frequency_dialog_accepts_one_unit_of_display_rounding() {
        assert!(knob_display_matches(1, "646 Hz", "647 Hz"));
        assert!(knob_display_matches(1, "1.23 kHz", "1.24 kHz"));
        assert!(!knob_display_matches(1, "646 Hz", "648 Hz"));
        assert!(!knob_display_matches(0, "646 Hz", "647 Hz"));
    }

    #[test]
    fn display_image_comparison_counts_native_lcd_pixel_differences() {
        let mut display = [0; 1024];
        let mut reference = image::GrayImage::from_pixel(128, 64, image::Luma([0]));
        assert_eq!(display_difference_count(&display, &reference), 0);

        display[0] |= 1;
        assert_eq!(display_difference_count(&display, &reference), 1);

        reference.put_pixel(0, 63, image::Luma([255]));
        assert_eq!(display_difference_count(&display, &reference), 0);
    }
}

#[cfg(test)]
mod native_midi_capability_tests {
    use super::*;

    #[test]
    fn native_direct_and_din_scenarios_select_the_expected_bridges() {
        for (steps, expected) in [
            (
                "[[step]]\naction='host-midi-connect'\n",
                Some((true, false)),
            ),
            (
                "[[step]]\naction='direct-usb-midi-round-trip'\n",
                Some((false, false)),
            ),
            (
                "[[step]]\naction='midi-probe-begin'\n[[step]]\naction='midi-probe-end'\n",
                Some((false, true)),
            ),
            (
                "[[step]]\naction='host-midi-connect'\n[[step]]\naction='midi-probe-begin'\n[[step]]\naction='midi-probe-end'\n",
                Some((true, true)),
            ),
            ("[[step]]\naction='delay'\nguest_ms=1\n", None),
        ] {
            let scenario = Scenario::from_text(&format!(
                "version=1\nname='midi'\nmodels=['l6max']\nbackends=['headless']\n{steps}"
            ))
            .unwrap();
            assert_eq!(
                midi_bridge_options(&scenario, l6fw::Model::L6max),
                expected,
                "{steps}"
            );
        }
    }

    #[test]
    fn native_midi_rejects_direct_usb_endpoint_conflicts() {
        let mut scenario = Scenario::from_text("version=1\nname='midi'\nmodels=['l6max']\nbackends=['headless']\n[[step]]\naction='host-midi-connect'\n").unwrap();
        for action in [
            crate::Action::HostMidiRoundTrip,
            crate::Action::UsbDescriptors,
            crate::Action::UsbStorage,
            crate::Action::UsbMidi { value: 1 },
            crate::Action::UsbSysexBegin,
            crate::Action::DirectUsbMidiRoundTrip {
                cable: 1,
                expect_ring_change: true,
            },
            crate::Action::EditorPadAssignments,
            crate::Action::EditorPadPlayback { pad: 1 },
            crate::Action::L6maxUiPadVerify { pad: 1 },
            crate::Action::L6DinSweep,
            crate::Action::L6MidiProbeBegin,
            crate::Action::L6UsbStartup,
            crate::Action::L6DinEventDispatch {
                group: 0,
                cc: 1,
                value: 1,
            },
            crate::Action::L6maxChannelSignalPwm,
            crate::Action::L6LevelMeterPwm,
        ] {
            let mut step = scenario.step[0].clone();
            step.action = action;
            scenario.step.push(step);
            assert!(validate_native_midi_capabilities(&scenario, l6fw::Model::L6max).is_err());
            scenario.step.pop();
        }
        assert!(validate_native_midi_capabilities(&scenario, l6fw::Model::L6max).is_ok());
        let scoped = Scenario::from_text("version=1\nname='scoped'\nmodels=['l6','l6max']\nbackends=['headless']\n[[step]]\nmodels=['l6']\naction='host-midi-connect'\n[[step]]\nmodels=['l6max']\naction='usb-descriptors'\n").unwrap();
        assert!(validate_native_midi_capabilities(&scoped, l6fw::Model::L6).is_ok());
        assert!(validate_native_midi_capabilities(&scoped, l6fw::Model::L6max).is_ok());
        assert_eq!(
            midi_bridge_options(&scoped, l6fw::Model::L6),
            Some((true, false))
        );
        assert_eq!(midi_bridge_options(&scoped, l6fw::Model::L6max), None);
    }

    #[test]
    fn native_round_trip_and_connection_enable_native_midi() {
        for (action, expected) in [
            ("host-midi-round-trip", true),
            ("host-midi-connect", true),
            ("direct-usb-midi-round-trip", false),
        ] {
            let scenario = Scenario::from_text(&format!(
                "version=1\nname='midi'\nmodels=['l6','l6max']\nbackends=['headless']\n[[step]]\naction='{action}'\n"
            )).unwrap();
            assert_eq!(
                uses_native_host_midi(&scenario, l6fw::Model::L6max),
                expected,
                "{action}"
            );
        }
    }
}
