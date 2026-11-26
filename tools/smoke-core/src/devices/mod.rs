//! Typed peripheral operations. Sequencing, timing and artifacts stay in the runner.
use crate::{Scenario, Session, scenario::PadAssignmentSource, waveform};
use firmware_api::{l6, l6max};
use l6max_host::engine::Options;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs,
    path::{Path, PathBuf},
    thread,
    time::{Duration, Instant},
};

mod audio;
mod chord;
mod display;
mod editor;
mod execute;
mod gpio_event;
mod knob_dialog;
mod l6_din_sweep;
mod l6_dispatch;
mod l6_input_sweep;
mod l6_meter_pwm;
mod lifecycle;
mod midi;
mod midi_probe;
mod mode_primitives;
mod panel;
mod panel_event;
mod power;
mod sd;
mod settings;
mod signal_pwm;
mod tap_handler;
mod timing;
mod usb_startup;
mod usb_storage_steps;

use audio::*;
pub use display::*;
pub use knob_dialog::*;
use lifecycle::*;
use midi::*;
use midi_probe::*;
use mode_primitives::wait_parameter_mode_checked;
use sd::*;

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
    UsbControllerReady,
    UsbEnumerate,
    UsbStorageBegin,
    UsbStorageInquiry,
    UsbStorageReady,
    UsbStorageCapacity,
    UsbStorageReadBoot,
    UsbStorageAssertBoot,
    UsbStorageLastRead,
    UsbStorageLastWrite,
    UsbStorageLastAssert,
    UsbStorageFatOpen,
    UsbStorageCreate {
        length: u32,
        multiplier: u32,
        increment: u32,
    },
    UsbStorageFlushCreated,
    UsbStorageReadFile,
    UsbStorageRemoveFile,
    UsbStorageAssertFile,
    UsbStorageFlushRemoved,
    UsbStorageEnd,
    UsbMidiOpen,
    UsbMidiBegin,
    UsbMidiExpectEncoder,
    UsbMidiPrepareLevel {
        value: u8,
    },
    UsbMidiSetAlternate,
    UsbMidiSetTarget,
    UsbMidiEnd,
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
    DirectMidiConnect {
        cable: u8,
    },
    MidiEncoderAlternate,
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
        expected_change: bool,
        observe_wall_ms: Option<u32>,
        midi_transaction: bool,
    },

    L6MidiProbeBegin,
    L6MidiProbeInject,
    L6MidiProbeExpectHook,
    L6MidiProbeExpectReplies,
    L6MidiProbeFinish,
    DinUsbBegin,
    DinUsbAttach,
    DinUsbSend {
        bytes: Vec<u8>,
    },
    DinUsbExpect {
        bytes: Vec<u8>,
        cable: u8,
    },
    DinUsbEnd,
    L6IndicatorBegin,
    L6RingSample {
        cc: u8,
        value: u8,
        guest_ms: u32,
        ring: Option<usize>,
    },
    L6RingCoverage {
        mask: u32,
    },
    L6MuteSample {
        cc: u8,
        value: u8,
        guest_ms: u32,
        expected: [u32; 8],
        reuse_previous: bool,
    },
    L6IndicatorEnd,
    UsbSysexBegin,
    UsbSysexExchange {
        request: crate::scenario::UsbSysexRequest,
        complete_on_match: bool,
    },
    UsbSysexEnd,
    EditorAssignBegin,
    EditorAssignIdentity,
    EditorAssignGlobal,
    EditorAssignVersion,
    EditorAssignSdInfo,
    EditorAssignRefresh,
    EditorAssignCount {
        pad: u8,
    },
    EditorAssignFilename {
        pad: u8,
    },
    EditorAssignSend {
        pad: u8,
    },
    EditorAssignDialog {
        pad: u8,
    },
    EditorAssignState {
        pad: u8,
    },
    EditorAssignAssert {
        pad: u8,
    },
    EditorAssignEnd,
    EditorPlaybackBegin {
        pad: u8,
    },
    EditorPlaybackPlay,
    EditorPlaybackWait,
    EditorPlaybackObserve,
    EditorPlaybackSample,
    EditorPlaybackAssert,
    EditorPlaybackEnd,
    UiPadBegin {
        pad: u8,
    },
    UiPadIdentity,
    UiPadGlobal,
    UiPadKeepAlive,
    UiPadQuery,
    UiPadAssert,
    UiPadEnd,
    SoundPadBegin {
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
    SoundPadPress,
    SoundPadHold,
    SoundPadRelease,
    SoundPadObserve,
    SoundPadSample,
    SoundPadAssertWaveform,
    SoundPadAssertAudio,
    SoundPadAssertLed,
    SoundPadAssertMeter,
    SoundPadFinish,
    L6RecorderBegin {
        target: u32,
    },
    L6RecorderPress,
    L6RecorderDispatcher,
    L6RecorderAssertEvent,
    L6RecorderHandler,
    L6RecorderAssertSelection,
    L6RecorderCallback,
    L6RecorderAssertCallback,
    L6RecorderObserve,
    L6RecorderRelease,
    L6RecorderSample,
    L6RecorderEvidence,
    L6RecorderAssertTrace,
    L6RecorderAssertDma,
    L6RecorderEnd,
    L6maxRecorderBegin {
        target: u32,
    },
    L6maxRecorderPress,
    L6maxRecorderSubmit,
    L6maxRecorderQueue,
    L6maxRecorderDispatch,
    L6maxRecorderScheduler,
    L6maxRecorderCallback,
    L6maxRecorderAction,
    L6maxRecorderStream,
    L6maxRecorderObserve,
    L6maxRecorderSample,
    L6maxRecorderEvidence,
    L6maxRecorderAssertAudio,
    L6maxRecorderEnd,
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
    CaptureSdModule {
        loaded: bool,
    },
    ExpectSdAllocation {
        loaded: bool,
    },
    ExpectSdChains {
        loaded: bool,
        chain_count: Option<u8>,
    },
    ExpectSdContinuations {
        loaded: bool,
    },
    ExpectSdModule {
        loaded: bool,
    },
    EffectPacedTurn {
        channel: usize,
        steps: i32,
    },
    CaptureEffectRing {
        encoder: u8,
    },
    ExpectEffectRingChanged {
        encoder: u8,
    },
    CaptureEffectState {
        edits: bool,
        dialog: bool,
    },
    ExpectEffectName {
        name: String,
    },
    ExpectEffectOutput,
    ExpectEffectEdits {
        minimum: u32,
    },
    ExpectEffectDialog {
        active: bool,
    },
    ExpectEffectType {
        value: u8,
    },
    ExpectEffectProcessor {
        value: u8,
    },
    ExpectEffectPanelMode {
        value: u8,
    },
    ExpectEffectEditMode {
        enabled: bool,
    },
    EffectStateEnd,
    WaitEffectMode {
        panel_mode: Option<u8>,
        edit_mode: Option<bool>,
    },
    WaitEffectSlot {
        expected_effect_type: u8,
        expected_processor_type: u8,
        expected_effect: Option<String>,
        require_output: bool,
    },
    ExpectHeap {
        minimum_free: u32,
    },
    HoldChord {
        targets: Vec<u32>,
        duration_ms: Option<u32>,
        until: Option<crate::scenario::ChordCondition>,
    },
    AudioCallbacksBegin,
    AudioCallbacksNext,
    AudioCallbacksCapture,
    AudioCallbacksAssert,
    AudioCallbacksEnd,
    KnobDialogReady,
    KnobDialogGuard,
    KnobDialogBaseline,
    KnobDialogPreflightBegin {
        sd_knob: bool,
    },
    KnobProbePrepare {
        probe: crate::scenario::PreflightProbe,
    },
    KnobProbeInject,
    KnobProbeTrigger {
        delta: i32,
    },
    KnobProbeSample,
    KnobProbeRestore,
    KnobProbeAssert,
    KnobRaceBegin,
    KnobRaceWaitRegistration,
    KnobRaceArmCallback,
    KnobRaceWaitCallback,
    KnobRaceCapture {
        guest_ms: u32,
        artifact: String,
    },
    KnobRaceSample,
    KnobRaceRestore,
    KnobRaceAssert,
    KnobActivationTrigger,
    KnobActivationWait,
    KnobActivationSample,
    KnobActivationRestore,
    KnobActivationAssert,
    KnobActiveBegin,
    KnobActiveSampleCallbacks,
    KnobActiveCheckCallbacks,
    KnobActiveCheckFrame,
    KnobActiveFinish,
    KnobLevelBegin,
    KnobLevelStart {
        demo: crate::scenario::adaptive::DemoCapture,
        artifact: String,
    },
    KnobLevelSweep {
        sweep: crate::scenario::adaptive::LevelSweep,
    },
    KnobLevelExpectZero,
    KnobLevelFinish,
    KnobTaskBegin,
    KnobTaskArmHook,
    KnobTaskWaitHook,
    KnobTaskArmCallback,
    KnobTaskWaitCallback,
    KnobTaskAssertDistinct,
    KnobTaskRelease,
    KnobTaskFinish,
    KnobOverlayBegin,
    KnobOverlayCheckWindow,
    KnobOverlaySnapshotDescriptors,
    KnobOverlayCheckDescriptor {
        field: crate::scenario::OverlayDescriptorField,
    },
    KnobOverlayFinishDescriptors,
    KnobOverlayCheckResource,
    KnobOverlayCheckCorners,
    KnobOverlayCheckSignature,
    KnobOverlayCheckBitmap,
    KnobOverlayCheckHeap,
    KnobOverlayFinish,
    KnobBitmapReactivate {
        delta: i32,
    },
    KnobBitmapWaitActive,
    KnobBitmapBegin,
    KnobBitmapCheckBackground {
        background: u32,
        expected: u16,
    },
    KnobBitmapStoreBefore,
    KnobBitmapArm,
    KnobBitmapWait,
    KnobBitmapInvoke {
        background: u32,
    },
    KnobBitmapCheckFrame,
    KnobBitmapCheckSignature,
    KnobBitmapCheckDescriptor,
    KnobBitmapFinish,
    KnobOverlayReactivate {
        delta: i32,
    },
    KnobOverlayWaitActive,
    KnobOverlayRefreshBegin,
    KnobOverlayRefreshValue,
    KnobOverlayRefreshPixels,
    KnobOverlayRefreshFinish,
    KnobLockAcquire,
    KnobLockTrigger {
        delta: i32,
    },
    KnobLockWait,
    KnobLockSample,
    KnobLockRestore,
    KnobLockAssert,
    KnobLockFinish,
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
    WaitKnobDisplayImage {
        image: PathBuf,
        main_timeout_ms: u32,
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
    KnobDialogAdaptive {
        plan: crate::scenario::adaptive::Plan,
    },
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
    EnqueueEncoderNoAck {
        channel: u32,
        steps: i32,
    },
    EnqueueTapNoAck {
        target: u32,
        hold_ms: u32,
    },
    ReleaseCapturedDisplay,
    KnobDialogSnapshotNavigation,
    KnobDialogCheckNavigation {
        field: crate::scenario::NavigationField,
    },
    KnobDialog,
    KnobPopupOpen {
        first_steps: i32,
        fallback_steps: i32,
        attempt_guest_ms: u32,
    },
    KnobPopupExpectOpen,
    KnobPopupExpectDisplay,
    KnobPopupExpectValue,
    CaptureMenu {
        menu: String,
        path: Vec<String>,
    },
    ExpectMenuItems {
        items: Vec<String>,
    },
    PatchParityHostOnly,
    WarmReset,
    Restart,
    ExpectRecorder,
    PowerBegin {
        hold_ms: u32,
        powered: bool,
    },
    PowerWaitAck,
    PowerObserveOn {
        guest_ms: u32,
    },
    PowerWaitOff {
        min_cutoff_ms: Option<u32>,
        max_cutoff_ms: Option<u32>,
    },
    PowerObserveStopped {
        settle_wall_ms: u64,
        observe_wall_ms: u64,
    },
    PowerEnd,
    SettingsCapture {
        restored: bool,
    },
    SettingsVersions,
    SettingsRtc,
    SettingsBank,
    SettingsTrace,
    SettingsPanel,
    SettingsReload,
    SettingsRtcTiming,
    SettingsEnd,

    PanelEventBegin {
        target: u32,
        column: u8,
        row: u8,
        event: u8,
    },
    PanelEventPress,
    PanelEventCapture,
    PanelEventReport,
    PanelEventMapping,
    PanelEventRelease,
    PanelEventSettle,
    PanelEventEnd,
    L6GpioBegin {
        target: u32,
        event: u8,
    },
    L6GpioPress,
    L6GpioSubmit,
    L6GpioAssert,
    L6GpioDispatch,
    L6GpioRelease,
    L6GpioSettle,
    L6GpioEnd,

    L6UsbStartupBegin,
    L6UsbStartupCapture,
    L6UsbStartupAssertHits,
    L6UsbStartupAssertWrites,
    L6UsbStartupAssertReplay,
    L6UsbStartupEnd,

    L6DispatchBegin {
        group: u8,
        cc: u8,
        value: u8,
    },
    L6DispatchTask,
    L6DispatchSemaphore,
    L6DispatchIdle,
    L6DispatchInject,
    L6DispatchWake,
    L6DispatchState,
    L6DispatchReader,
    L6DispatchEvent,
    L6DispatchEq,
    L6DispatchObserve,
    L6DispatchSetter {
        index: u8,
        value: u8,
    },
    L6DispatchFinish,

    L6maxTapBegin,
    L6maxTapPress,
    L6maxTapWait,
    L6maxTapAssert,
    L6maxTapRelease,
    L6maxTapEnd,
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
    L6SweepBegin,
    L6SweepRejectMain {
        target: u32,
    },
    L6SweepPanelBaseline {
        target: u32,
    },
    L6SweepPanelTap,
    L6SweepPanelWait,
    L6SweepPanelTrace,
    L6SweepPanelAssert,
    L6SweepEncoderBaseline,
    L6SweepEncoderSend {
        index: u32,
    },
    L6SweepEncoderWait,
    L6SweepEncoderTrace,
    L6SweepEncoderAssert,
    L6SweepRejectPanel,
    L6SweepRejectEncoder,
    L6SweepEnd,
    L6DinSweepBegin,
    L6DinSweepReady,
    L6DinSweepMonitor,
    L6DinSweepBaseline,
    L6DinSweepSample {
        cc: u8,
    },
    L6DinSweepTrace,
    L6DinSweepDrain,
    L6DinSweepStop,
    L6DinSweepCapture,
    L6DinSweepAssert {
        check: crate::scenario::DinSweepCheck,
    },
    L6DinSweepEnd,
    L6maxTimingBegin,
    L6maxTimingSettle {
        wall_ms: u32,
    },
    L6maxTimingBaseline,
    L6maxTimingEnqueue {
        target: u32,
        hold_ms: u32,
        pairs: u32,
    },
    L6maxTimingWaitFirst,
    L6maxTimingObservePaused {
        wall_ms: u32,
    },
    L6maxTimingAssertPaused,
    L6maxTimingResume,
    L6maxTimingWaitQueued,
    L6maxTimingWaitReports,
    L6maxTimingAssertReports,
    L6maxTimingObserveDisplay {
        guest_ms: u32,
    },
    L6maxTimingAssertDisplay,
    L6maxTimingEncoder {
        index: u32,
        steps: i32,
    },
    L6maxTimingWaitEncoder,
    L6maxTimingWaitEncoderReport,
    L6maxTimingAssertEncoder,
    L6maxTimingEnd,
    L6maxPanelKeyBegin,
    L6maxPanelKeyCheck {
        code: u32,
        bit: u32,
        word_index: u32,
        active: bool,
        wait: bool,
    },
    L6maxPanelKeyInput {
        target: u32,
        pressed: bool,
    },
    L6maxPanelKeyEnd,
    SignalPwmBegin,
    SignalPwmWarmup {
        start: u8,
        end: u8,
        value: f32,
    },
    SignalPwmDrain {
        band: u8,
    },
    SignalPwmStage {
        band: u8,
        start: u8,
        end: u8,
        value: f32,
    },
    SignalPwmAssert {
        band: u8,
        channel: u8,
    },
    SignalPwmEnd,
    L6MeterBegin,
    L6MeterStage {
        value: u32,
    },
    L6MeterAssert,
    L6MeterEnd,
}

fn uses_native_host_midi(scenario: &Scenario, model: l6fw::Model) -> bool {
    scenario
        .step
        .iter()
        .any(|step| step.applies_to(model) && matches!(step.action, crate::Action::HostMidiConnect))
}

fn midi_bridge_options(scenario: &Scenario, model: l6fw::Model) -> Option<(bool, bool)> {
    let native = uses_native_host_midi(scenario, model);
    let direct = scenario.step.iter().any(|step| {
        step.applies_to(model) && matches!(step.action, crate::Action::DirectMidiConnect { .. })
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
                crate::Action::DirectMidiConnect { .. }
                    | crate::Action::DinUsbBegin
                    | crate::Action::UsbEnumerate
                    | crate::Action::UsbStorageBegin
                    | crate::Action::UsbMidiOpen
                    | crate::Action::UsbSysexBegin
                    | crate::Action::UsbSysexExchange { .. }
                    | crate::Action::UsbSysexEnd
                    | crate::Action::EditorAssignBegin
                    | crate::Action::EditorPlaybackBegin { .. }
                    | crate::Action::UiPadBegin { .. }
                    | crate::Action::L6DinSweepBegin
                    | crate::Action::L6MidiProbeBegin
                    | crate::Action::L6UsbStartupBegin
                    | crate::Action::L6DispatchBegin { .. }
                    | crate::Action::SignalPwmBegin
                    | crate::Action::L6MeterBegin
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
            crate::Action::SignalPwmBegin | crate::Action::L6MeterBegin
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
            crate::Action::AudioCallbacksBegin
                | crate::Action::SettingsCapture { .. }
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
        .any(|s| matches!(s.action, crate::Action::AudioCallbacksBegin))
    {
        options.start_paused = true;
        options.gdb_socket = Some(gdb_socket_path(&options.logs, "audio"));
    }
    if scenario
        .step
        .iter()
        .any(|s| matches!(s.action, crate::Action::PanelEventBegin { .. }))
    {
        options.gdb_socket = Some(gdb_socket_path(&options.logs, "panel-event-map"));
    }
    if scenario
        .step
        .iter()
        .any(|s| matches!(s.action, crate::Action::L6GpioBegin { .. }))
    {
        options.gdb_socket = Some(gdb_socket_path(&options.logs, "l6-main-gpio-event"));
    }
    if scenario
        .step
        .iter()
        .any(|s| matches!(s.action, crate::Action::L6maxTapBegin))
    {
        options.gdb_socket = Some(gdb_socket_path(&options.logs, "l6max-tap-handler"));
    }
    if scenario
        .step
        .iter()
        .any(|s| s.applies_to(model) && matches!(s.action, crate::Action::L6UsbStartupBegin))
    {
        options.start_paused = true;
        options.usb_enabled = false;
        options.midi_enabled = false;
        options.gdb_socket = Some(gdb_socket_path(&options.logs, "l6-usb-startup"));
    }
    if scenario
        .step
        .iter()
        .any(|s| s.applies_to(model) && matches!(s.action, crate::Action::L6DispatchBegin { .. }))
    {
        options.start_paused = true;
        options.usb_enabled = false;
        options.midi_enabled = false;
        options.gdb_socket = Some(gdb_socket_path(&options.logs, "l6ed"));
    }
    if scenario
        .step
        .iter()
        .any(|s| s.applies_to(model) && matches!(s.action, crate::Action::L6DinSweepBegin))
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
                crate::Action::UsbEnumerate
                    | crate::Action::UsbStorageBegin
                    | crate::Action::UsbMidiOpen
                    | crate::Action::UsbSysexBegin
                    | crate::Action::UsbSysexExchange { .. }
                    | crate::Action::UsbSysexEnd
                    | crate::Action::EditorAssignBegin
                    | crate::Action::EditorPlaybackBegin { .. }
                    | crate::Action::UiPadBegin { .. }
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
    adaptive_progress: crate::progress::CaseProgress,
    adaptive_index: usize,

    din_usb: Option<midi::DinUsbScope>,
    usb_midi: Option<midi::UsbMidiScope>,
    panel_keys: Option<PanelKeyScope>,
    saved: Option<(Vec<u8>, usize)>,
    settings: Option<settings::Scope>,
    usb_descriptor_budget: Option<l6max_host::usb::Budget>,
    menu_snapshot: Option<l6ctl::MenuSnapshot>,
    midi: Option<l6max_host::usb::Endpoints>,
    sysex_deadline: Option<(Instant, u32, u32, u64)>,
    ui_pad: Option<editor::UiPadScope>,
    editor_playback: Option<editor::EditorPlaybackScope>,
    editor_assignment: Option<editor::EditorAssignmentScope>,
    l6_input_sweep: Option<l6_input_sweep::Scope>,
    signal_pwm: Option<signal_pwm::Scope>,
    l6_meter_pwm: Option<l6_meter_pwm::Scope>,
    l6_indicator: Option<L6IndicatorScope>,
    panel_event: Option<panel_event::Scope>,
    host_midi: Option<Box<dyn crate::host_midi::Client>>,
    encoder_turned_up: bool,
    knob_popup: Option<knob_dialog::PopupContext>,
    knob_dialog_baseline: Option<[u8; 1024]>,
    knob_dialog_preflight: Option<knob_dialog::PreflightContext>,
    overlay_reader: Option<l6max_host::knob_popup::OverlayReader>,
    task_reader: Option<l6max_host::freertos::CurrentTaskReader>,
    knob_dialog_deadline: Option<(std::time::Instant, u32, u32, u64, u16)>,
    knob_mode: Option<KnobModeScope>,
    knob_heap_reader: Option<l6max_host::heap::CounterReader>,
    pending_input: Option<PendingInput>,
    power: Option<power::Scope>,
    captured_display: Option<CapturedDisplay>,
    midi_round_trip: Option<MidiRoundTrip>,
    midi_sync: Option<midi::MidiSync>,
    l6_midi_probe: Option<midi::L6MidiProbe>,
    sd_inspection: Option<sd::SdInspectionContext>,
    sound_pad: Option<audio::sound_pad::SoundPadContext>,
    l6_recorder: Option<audio::recorder::Scope>,
    l6max_recorder: Option<audio::max_recorder::Scope>,
    audio_callbacks: Option<audio::callbacks::Scope>,
    l6_din_sweep: Option<l6_din_sweep::Scope>,
    tap_handler: Option<tap_handler::Scope>,
    gpio_event: Option<gpio_event::Scope>,
    l6_dispatch: Option<l6_dispatch::Scope>,
    usb_startup: Option<usb_startup::Scope>,
    effect_slot_deadline: Option<sd::EffectSlotScope>,
    effect_state: Option<(
        l6ctl::EffectSlotSnapshot,
        l6max_host::effect_editor::SlotReader,
        Instant,
        u32,
        u32,
        u64,
    )>,
    effect_ring: Option<(u8, u8, u32, l6max_host::effect_editor::SlotReader)>,
    l6max_timing: Option<timing::Scope>,
    usb_storage_scope: Option<usb_storage_steps::Scope>,
    native_midi: Option<l6ctl::midi::MidiSession>,
    direct_midi: bool,
    midi_probe: Option<MidiProbeContext>,
}

struct PanelKeyScope {
    deadline: Instant,
    held: Option<(u32, l6ctl::RawHeldButton)>,
}
impl PanelKeyScope {
    fn release(&mut self) -> Result<(), String> {
        if let Some((target, mut button)) = self.held.take() {
            button
                .release()
                .map_err(|error| format!("release panel key {target}: {error}"))?;
        }
        Ok(())
    }
}
impl Drop for PanelKeyScope {
    fn drop(&mut self) {
        let _ = self.release();
    }
}

struct L6IndicatorScope {
    start: Instant,
    guest_start: u32,
    guest_budget: u32,
    wall_budget: u64,
    changed_segments: u32,
    last_mute: Option<(u8, [u32; 8])>,
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
    pub fn set_adaptive_progress(&mut self, progress: crate::progress::CaseProgress, index: usize) {
        self.adaptive_progress = progress;
        self.adaptive_index = index;
    }
    pub fn take_host_midi(&mut self) -> Option<Box<dyn crate::host_midi::Client>> {
        self.direct_midi = false;
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

fn guest_now(session: &Session) -> u32 {
    l6ctl::OwnedControl::new(&session.engine)
        .input_status(l6ctl::InputSource::Panel)
        .map_or(0, |status| status.guest_ms)
}

fn wait_guest_duration(
    session: &Session,
    duration_ms: u32,
    deadline: Instant,
) -> Result<(), String> {
    let control = l6ctl::OwnedControl::new(&session.engine);
    let clock = || {
        control
            .input_status(l6ctl::InputSource::Panel)
            .map(|status| status.guest_ms)
            .map_err(|_| "guest wait needs inputs".to_string())
    };
    let start = clock()?;
    while clock()?.wrapping_sub(start) < duration_ms {
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
        let elapsed = l6ctl::OwnedControl::new(&session.engine)
            .input_status(l6ctl::InputSource::Panel)?
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
    fn restarted_releases_held_panel_key_before_input_shutdown() {
        use super::*;
        use std::{os::unix::net::UnixStream, sync::Arc};
        let (main_host, _main_peer) = UnixStream::pair().unwrap();
        let (panel_host, mut panel_peer) = UnixStream::pair().unwrap();
        let inputs = Arc::new(l6max_host::input::InputHub::new(
            l6max_host::input::EngineInput::new(main_host).unwrap(),
            l6max_host::input::EngineInput::new(panel_host).unwrap(),
            l6fw::Model::L6max,
        ));
        let mut options = l6max_host::engine::Options::parse_args(std::iter::empty()).unwrap();
        options.qemu = None;
        options.auto_setup = false;
        let mut engine = l6max_host::engine::Engine::start(&options).unwrap();
        engine.inputs = Some(inputs);
        let mut button = l6ctl::OwnedControl::new(&engine)
            .raw_button(l6ctl::InputSource::Panel, 1)
            .unwrap();
        button.press(0).unwrap();
        let press = l6max_host::protocol::Message::read(&mut panel_peer).unwrap();
        assert_eq!(press.kind, l6max_host::protocol::DOWN);
        let mut state = State::default();
        state.panel_keys = Some(PanelKeyScope {
            deadline: Instant::now(),
            held: Some((1, button)),
        });
        state.restarted().unwrap();
        let message = l6max_host::protocol::Message::read(&mut panel_peer).unwrap();
        assert_eq!(
            (message.kind, message.target),
            (l6max_host::protocol::UP, 1)
        );
        assert!(state.panel_keys.is_none());
    }
    #[test]
    fn scope_cleanup_reports_failed_release_and_clears_scope() {
        use super::*;
        use std::{net::Shutdown, os::unix::net::UnixStream, sync::Arc};
        let (main_host, _main_peer) = UnixStream::pair().unwrap();
        let (panel_host, panel_peer) = UnixStream::pair().unwrap();
        let inputs = Arc::new(l6max_host::input::InputHub::new(
            l6max_host::input::EngineInput::new(main_host).unwrap(),
            l6max_host::input::EngineInput::new(panel_host).unwrap(),
            l6fw::Model::L6max,
        ));
        let mut options = l6max_host::engine::Options::parse_args(std::iter::empty()).unwrap();
        options.qemu = None;
        options.auto_setup = false;
        let mut engine = l6max_host::engine::Engine::start(&options).unwrap();
        engine.inputs = Some(inputs.clone());
        let mut button = l6ctl::OwnedControl::new(&engine)
            .raw_button(l6ctl::InputSource::Panel, 1)
            .unwrap();
        button.press(0).unwrap();
        panel_peer.shutdown(Shutdown::Both).unwrap();
        let until = Instant::now() + std::time::Duration::from_secs(2);
        while inputs.panel.snapshot().connected && Instant::now() < until {
            std::thread::yield_now();
        }
        assert!(
            !inputs.panel.snapshot().connected,
            "input reader did not observe disconnect"
        );
        let mut state = State::default();
        state.panel_keys = Some(PanelKeyScope {
            deadline: Instant::now(),
            held: Some((1, button)),
        });
        assert!(
            state
                .abort_auxiliary_scopes()
                .unwrap_err()
                .contains("release panel key 1")
        );
        assert!(state.panel_keys.is_none());
    }
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
        create_factory_card, display_difference_count, knob_display_matches, sound_pad_led,
        verify_sai_edma_trace,
    };
    use l6ctl::{editor_pad_assignment_matches, editor_pad_file_count, editor_pad_filename_valid};
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
                "[[step]]\naction='direct-midi-connect'\ncable=1\n",
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
            crate::Action::UsbEnumerate,
            crate::Action::UsbStorageBegin,
            crate::Action::UsbMidiOpen,
            crate::Action::UsbSysexBegin,
            crate::Action::DirectMidiConnect { cable: 1 },
            crate::Action::EditorAssignBegin,
            crate::Action::EditorPlaybackBegin { pad: 1 },
            crate::Action::UiPadBegin { pad: 1 },
            crate::Action::L6DinSweepBegin,
            crate::Action::L6MidiProbeBegin,
            crate::Action::DinUsbBegin,
            crate::Action::L6UsbStartupBegin,
            crate::Action::L6DispatchBegin {
                group: 0,
                cc: 1,
                value: 1,
            },
            crate::Action::SignalPwmBegin,
            crate::Action::L6MeterBegin,
        ] {
            let mut step = scenario.step[0].clone();
            step.action = action;
            scenario.step.push(step);
            assert!(validate_native_midi_capabilities(&scenario, l6fw::Model::L6max).is_err());
            scenario.step.pop();
        }
        assert!(validate_native_midi_capabilities(&scenario, l6fw::Model::L6max).is_ok());
        let scoped = Scenario::from_text("version=1\nname='scoped'\nmodels=['l6','l6max']\nbackends=['headless']\n[[step]]\nmodels=['l6']\naction='host-midi-connect'\n[[step]]\nmodels=['l6max']\naction='usb-controller-ready'\n[[step]]\nmodels=['l6max']\naction='usb-enumerate'\n").unwrap();
        assert!(validate_native_midi_capabilities(&scoped, l6fw::Model::L6).is_ok());
        assert!(validate_native_midi_capabilities(&scoped, l6fw::Model::L6max).is_ok());
        assert_eq!(
            midi_bridge_options(&scoped, l6fw::Model::L6),
            Some((true, false))
        );
        assert_eq!(midi_bridge_options(&scoped, l6fw::Model::L6max), None);
    }

    #[test]
    fn native_and_direct_connections_select_the_right_transport() {
        let native = Scenario::from_text("version=1\nname='native'\nmodels=['l6max']\nbackends=['headless']\n[[step]]\naction='host-midi-connect'\n").unwrap();
        let direct = Scenario::from_text("version=1\nname='direct'\nmodels=['l6max']\nbackends=['headless']\n[[step]]\naction='direct-midi-connect'\ncable=1\n").unwrap();
        assert!(uses_native_host_midi(&native, l6fw::Model::L6max));
        assert!(!uses_native_host_midi(&direct, l6fw::Model::L6max));
        assert_eq!(
            midi_bridge_options(&direct, l6fw::Model::L6max),
            Some((false, false))
        );
    }
}
