//! Strict scenario schema, composition and semantic model mappings.
use crate::trace;
use l6fw::Model;
pub use l6max_host::controls::{control_target, mode_index};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::{Path, PathBuf},
};

pub mod adaptive;
mod validate;
/// How a case drives the emulator.
#[derive(
    Clone,
    Copy,
    Debug,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    Deserialize,
    schemars::JsonSchema,
    Serialize,
)]
#[serde(rename_all = "lowercase")]
pub enum Backend {
    /// The scenario runner drives a headless engine.
    Headless,
    /// The scenario runs through the GUI.
    Gui,
}

/// Named fields in the ordered, non-atomic dialog restoration snapshot.
#[derive(Clone, Copy, Debug, Deserialize, schemars::JsonSchema, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum RestorationField {
    Window,
    Signature,
    Bitmap,
    BitmapDescriptor,
    NotificationSemaphore,
    TextResources,
    SharedPointer,
    SharedContents,
    TextLengths,
    Widget7,
    Widget33,
    Background10,
    Background37,
    Background48,
    Callback,
}

/// Individual navigation restoration fields in original check order.
#[derive(Clone, Copy, Debug, Deserialize, schemars::JsonSchema, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum NavigationField {
    Window,
    Signature,
    Bitmap,
    BitmapDescriptor,
    NotificationSemaphore,
    SharedContents,
    Widget7,
    Widget33,
    Background10,
    Background37,
    Background48,
    Callback,
}

/// Ordered fields in the retained overlay descriptor snapshot.
#[derive(Clone, Copy, Debug, Deserialize, schemars::JsonSchema, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum OverlayDescriptorField {
    Widget7,
    Widget33,
    Background10,
    Background37,
    Background48,
}

/// A condition that ends a held chord.
#[derive(Clone, Copy, Debug, Deserialize, schemars::JsonSchema, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ChordCondition {
    FactoryMode,
}

/// A deliberate test observation, rather than a readiness delay.
#[derive(Clone, Copy, Debug, Deserialize, schemars::JsonSchema, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ObservationActivity {
    Recording,
    Playback,
    AudioOutput,
}

/// MIDI transport used by a bounded mixer/DIN observation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, schemars::JsonSchema, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum MidiProbePort {
    Mixer,
    Din,
}

/// Case-local synthetic L6max update card variant.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, schemars::JsonSchema, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum UpdateFixture {
    BadChecksum,
    BadChecksumStock,
    L6Stock,
    L6PatchedMidi,
    SyntheticValid,
    PatchedMidi,
}

impl UpdateFixture {
    pub fn is_l6(self) -> bool {
        matches!(self, Self::L6Stock | Self::L6PatchedMidi)
    }

    pub fn is_patched(self) -> bool {
        matches!(self, Self::PatchedMidi | Self::L6PatchedMidi)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, schemars::JsonSchema, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum UpdateGoal {
    Handoff,
    Install,
}

/// One boot generation of a managed update lifecycle.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, schemars::JsonSchema, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum LifecycleBoot {
    Initial,
    Installer,
    Restart,
    Verify,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, schemars::JsonSchema, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum InstallerExpected {
    Installed,
    NoPending,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, schemars::JsonSchema, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum UpdateComparisonField {
    MainPayload,
    BootRegion,
    ProtectedRegion,
    PanelImage,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, schemars::JsonSchema, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum L6UpdateField {
    MainFirmware,
    MainTrailer,
    SecondaryFirmware,
    SecondaryTrailer,
    PackageFooter,
    BootRegion,
    ProtectedRegion,
    ResourceNor,
}

impl Backend {
    pub fn name(self) -> &'static str {
        match self {
            Backend::Headless => "headless",
            Backend::Gui => "gui",
        }
    }
}

impl std::fmt::Display for Backend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

#[derive(Clone, Debug, Deserialize, schemars::JsonSchema, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Scenario {
    pub version: u32,
    pub name: String,
    pub models: Vec<Model>,
    /// Boot from the state spec `tests/emulator/fixtures/states/<model>-<state>.toml`;
    /// [`DEFAULT_STATE`] when the file names none, and no state (first boot)
    /// for `"none"`.
    pub state: Option<String>,
    /// Build the case's card from `tests/emulator/fixtures/cards/<card>.toml`;
    /// [`DEFAULT_CARD`] when the file names none, and no card for `"none"`.
    pub card: Option<String>,
    /// How the test driver runs this scenario as a case; the scenario itself
    /// ignores it.
    #[serde(default, skip_serializing)]
    #[schemars(skip)]
    pub case: Option<toml::Table>,
    /// Firmware patches applied to the configured package.
    #[serde(default)]
    pub patches: Vec<String>,
    /// Patches the engine installs on a device already running the stock firmware.
    #[serde(default)]
    pub installed_patches: Vec<String>,
    /// Patches the engine delivers on the case's card, as `--sd-patch` does: it
    /// installs the Patch loader and writes the hosted modules to `PATCHES/`.
    #[serde(default)]
    pub installed_sd_patches: Vec<String>,
    /// Build the experimental persistent Patch loader and the selected SD bundle.
    /// An empty list installs only the loader for a no-module baseline.
    pub sd_patches: Option<Vec<String>>,
    /// Explicit stock effect slot assignments for the composed SD bundle.
    #[serde(default)]
    pub effect_replacements: std::collections::BTreeMap<String, String>,
    /// SD bundle content: valid (default), absent, or deliberately corrupted.
    pub sd_bundle_mode: Option<String>,
    /// Patches for the update package, per model, applied to the configured package.
    #[serde(default)]
    pub update_patches: std::collections::BTreeMap<Model, Vec<String>>,
    /// Build a case-local synthetic L6max update card before execution.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub update_fixture: Option<UpdateFixture>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub update_goal: Option<UpdateGoal>,
    #[serde(default)]
    pub gdb: bool,
    pub audio_device: Option<String>,
    /// Audio fed to input channels for the whole run.
    #[serde(default)]
    pub audio_input: Vec<AudioInput>,
    #[serde(default = "default_backends")]
    pub backends: Vec<Backend>,
    #[serde(default)]
    pub trace_sd: bool,
    pub step: Vec<Step>,
    /// The case state was built from a state spec: setup input is dropped
    /// and boot readiness does not wait out first-boot setup screens.
    #[serde(skip)]
    pub spec_state: bool,
    /// The scenario file and every fragment it includes, in load order.
    #[serde(skip)]
    pub sources: Vec<PathBuf>,
}
#[derive(Clone, Debug, Deserialize, schemars::JsonSchema, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AudioInput {
    /// One-based input channel. See the audio model for each model's channels.
    pub channel: u8,
    #[serde(flatten)]
    pub content: sd_image::spec::Content,
}
#[derive(Clone, Debug, Deserialize, schemars::JsonSchema, Serialize)]
pub struct Step {
    #[serde(default)]
    pub models: Vec<Model>,
    /// UI input that only brings the device to the scenario's starting
    /// state. Skipped when the case state was built from a state spec.
    #[serde(default)]
    pub setup_input: bool,
    #[serde(default = "guest_timeout")]
    pub guest_timeout_ms: u32,
    #[serde(default = "wall_timeout")]
    pub wall_timeout_s: u64,
    #[serde(flatten)]
    pub action: Action,
}
impl Step {
    /// Whether the step runs on `model`; a step without its own models runs
    /// on every model the scenario supports.
    pub fn applies_to(&self, model: Model) -> bool {
        self.models.is_empty() || self.models.contains(&model)
    }
}
fn default_backends() -> Vec<Backend> {
    vec![Backend::Headless, Backend::Gui]
}
fn default_midi_cable() -> u8 {
    1
}
fn default_true() -> bool {
    true
}
fn ring_settle() -> u32 {
    1000
}
fn default_playback_window_ms() -> u32 {
    6000
}

fn default_tone_tolerance_db() -> f64 {
    1.0
}
fn default_max_residual_db() -> f64 {
    -60.
}
fn is_zero(value: &u32) -> bool {
    *value == 0
}
fn guest_timeout() -> u32 {
    10_000
}
fn wall_timeout() -> u64 {
    180
}
#[derive(Clone, Copy, Debug, Default, Deserialize, schemars::JsonSchema, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Gesture {
    #[default]
    Scroll,
    DragVertical,
    DragHorizontal,
}
#[derive(
    Clone, Copy, Debug, Default, Deserialize, schemars::JsonSchema, Serialize, PartialEq, Eq,
)]
#[serde(rename_all = "kebab-case")]
pub enum PadAssignmentSource {
    #[default]
    Editor,
    SavedState,
}
#[derive(Clone, Debug, Deserialize, schemars::JsonSchema, Serialize)]
#[serde(deny_unknown_fields)]
pub struct UsbSysexRequest {
    pub bytes: Vec<u8>,
    pub expect_prefix: Option<Vec<u8>>,
    #[serde(default)]
    pub reject_prefixes: Vec<Vec<u8>>,
    #[serde(default)]
    pub expect_reply: bool,
    pub min_reply_bytes: Option<usize>,
    pub reply_timeout_ms: Option<u32>,
}
#[derive(Clone, Debug, Deserialize, schemars::JsonSchema, Serialize)]
#[serde(tag = "action", rename_all = "kebab-case", deny_unknown_fields)]
pub enum Action {
    LifecycleStart {
        profile: LifecycleBoot,
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
        field: L6UpdateField,
    },
    ExpectInstallerOutcome {
        expected: InstallerExpected,
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
        field: UpdateComparisonField,
    },
    CaptureInstalledPanel,
    ExpectPanelUnchanged,
    ExpectUpdateGui {
        window: u32,
        progress_mask: u32,
        progress_value: u32,
    },
    Include {
        path: PathBuf,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        include_guest_timeout_ms: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        include_wall_timeout_s: Option<u64>,
    },
    Checkpoint {
        name: String,
    },
    ExpectDisplay {
        checkpoint: String,
        changed: bool,
    },
    ExpectDisplayImage {
        image: PathBuf,
        /// Require the matching image to remain visible for this guest time.
        #[serde(default, skip_serializing_if = "is_zero")]
        settle_ms: u32,
    },
    ExpectTrace {
        contains: Vec<String>,
        #[serde(default)]
        absent: Vec<String>,
    },
    ExpectTraceCount {
        pattern: String,
        count: u64,
        #[serde(default)]
        at_least: bool,
        checkpoint: Option<String>,
    },
    ExpectButtonReport {
        control: String,
    },
    ExpectStartupReport {
        control: String,
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
        port: MidiProbePort,
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
        port: MidiProbePort,
        target: u32,
        hold_ms: u32,
    },
    MidiProbeExpectNoEvents {
        port: MidiProbePort,
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
        #[serde(default)]
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
        checkpoint: String,
        #[serde(default)]
        midi_transaction: bool,
    },

    DirectUsbMidiRoundTrip {
        #[serde(default = "default_midi_cable")]
        cable: u8,
        #[serde(default = "default_true")]
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
        #[serde(flatten)]
        request: UsbSysexRequest,
        #[serde(default)]
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
        #[serde(default)]
        assignment: PadAssignmentSource,
        #[serde(default = "default_true")]
        require_pad_led_change: bool,
        #[serde(default = "default_playback_window_ms")]
        playback_window_ms: u32,
        #[serde(default)]
        require_stereo_meter: bool,
        #[serde(default)]
        expect_source: Option<String>,
        #[serde(default = "default_max_residual_db")]
        max_residual_db: f64,
    },
    /// Wait for L6 pad 1's active SD file assignment.
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
        /// A name declared in `patches/api` for the model the step runs on.
        #[schemars(schema_with = "crate::firmware::symbol_schema")]
        symbol: String,
        bytes: Vec<u8>,
    },
    /// Wait for a declared firmware symbol to match the expected bytes.
    WaitMemory {
        #[schemars(schema_with = "crate::firmware::symbol_schema")]
        symbol: String,
        bytes: Vec<u8>,
    },
    ExpectSdLoader,
    ExpectSdModule {
        loaded: bool,
        #[serde(default)]
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
    /// Wait for effect editor and panel-mode state in the loaded module.
    WaitEffectMode {
        #[serde(default)]
        panel_mode: Option<u8>,
        #[serde(default)]
        edit_mode: Option<bool>,
    },
    EffectReplacementState {
        #[serde(default)]
        expected_effect_type: Option<u8>,
        #[serde(default)]
        expected_processor_type: Option<u8>,
        #[serde(default)]
        expected_panel_mode: Option<u8>,
        #[serde(default)]
        expected_edit_mode: Option<bool>,
        #[serde(default)]
        minimum_parameter_edits: Option<u32>,
        #[serde(default)]
        expected_parameter_id: Option<u8>,
        #[serde(default)]
        expected_parameter_value: Option<f32>,
        #[serde(default)]
        expected_dialog_active: Option<bool>,
        #[serde(default)]
        minimum_exit_ring_refreshes: Option<u32>,
        /// The effect ID the effect loader runs for the selected slot.
        #[serde(default)]
        expected_effect: Option<String>,
        /// The selected slot's replacement has produced nonzero output.
        #[serde(default)]
        require_output: bool,
        #[serde(default)]
        wait_for_slot: bool,
    },
    ExpectHeap {
        minimum_free: u32,
    },
    HoldChord {
        controls: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        duration_ms: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        until: Option<ChordCondition>,
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
    KnobOverlayBegin,
    KnobOverlayCheckWindow,
    KnobOverlaySnapshotDescriptors,
    KnobOverlayCheckDescriptor {
        field: OverlayDescriptorField,
    },
    KnobOverlayFinishDescriptors,
    KnobOverlayCheckResource,
    KnobOverlayCheckCorners,
    KnobOverlayCheckSignature,
    KnobOverlayCheckBitmap,
    KnobOverlayCheckHeap,
    KnobOverlayFinish,
    KnobDialogBitmapZero,
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
        plan: adaptive::Plan,
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
    /// Turn the channel 1 level knob once on the recorder screen and check
    /// that Knob Dialog shows the new value.
    KnobPopupOpen {
        first_steps: i32,
        fallback_steps: i32,
        attempt_guest_ms: u32,
    },
    KnobPopupExpectOpen,
    KnobPopupExpectValue,
    /// Compare a menu's child titles with `items`. `menu` names a stock menu
    /// (`main`, `project`, `sound-pad`, `mixer` or `system`); `path` then
    /// descends through child titles.
    ExpectMenu {
        menu: String,
        #[serde(default)]
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
    ExpectPowered {
        powered: bool,
    },
    ExpectSettings {
        restored: bool,
    },
    Wait {
        until: String,
    },
    Delay {
        guest_ms: u32,
    },
    /// Observe an activity for a test-defined duration of guest time.
    ObserveDuration {
        activity: ObservationActivity,
        guest_ms: u32,
    },
    /// Wait for the display to differ from the preceding press's baseline.
    ExpectDisplayAfterPress,
    Press {
        control: String,
    },
    Hold {
        control: String,
        duration_ms: u32,
    },
    SaveScene {
        scene: usize,
    },
    ExpectScene {
        scene: usize,
        lit: bool,
        checkpoint: Option<String>,
        #[serde(default)]
        changed: bool,
    },
    ExpectButtonLed {
        control: String,
        lit: Option<bool>,
        checkpoint: Option<String>,
        #[serde(default)]
        changed: bool,
        #[serde(default)]
        same: bool,
    },
    ExpectEffect {
        effect: String,
    },
    Switch {
        control: String,
        enabled: bool,
    },
    Encoder {
        channel: usize,
        steps: i32,
        #[serde(default)]
        gesture: Gesture,
    },
    Analog {
        channel: usize,
        steps: i32,
        #[serde(default)]
        gesture: Gesture,
    },
    ExpectAnalog {
        channel: usize,
        value: u16,
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
    L6maxTapHandler,
    /// Wait until an entry appears on the card, written by the firmware.
    /// Optionally check its size, or that its bytes equal a file in the
    /// device state directory (for example `main-nor.bin`).
    ExpectCardEntry {
        path: String,
        #[serde(default)]
        size: Option<u64>,
        #[serde(default)]
        matches_state: Option<String>,
    },
    /// Wait for the newest recording's WAV files to be finalized.
    WaitRecordedFiles {
        files: Vec<String>,
    },
    /// Control input playback: `play`, `pause`, `stop` or `seek` to `position_ms`.
    AudioInputTransport {
        command: String,
        #[serde(default)]
        position_ms: i32,
    },
    /// Map a channel's prepared audio again, or drop it.
    AudioInputLoad {
        channel: u32,
        loaded: bool,
    },
    /// Check the reported input playback state.
    ExpectAudioInput {
        playing: bool,
        min_ms: u32,
        max_ms: u32,
    },
    /// Wait for the reported input transport state and position.
    WaitAudioInput {
        playing: bool,
        min_ms: u32,
        max_ms: u32,
    },
    /// Measure a tone in the last second of captured output. A side without
    /// a level must be below -70 dB.
    ExpectOutputTone {
        hz: f64,
        left_db: Option<f64>,
        right_db: Option<f64>,
        #[serde(default = "default_tone_tolerance_db")]
        tolerance_db: f64,
    },
    /// Measure a tone in a file of the newest recorder project, skipping its
    /// first second. A side without a level must be below -70 dB.
    ExpectRecordedTone {
        file: String,
        hz: f64,
        left_db: Option<f64>,
        right_db: Option<f64>,
        #[serde(default = "default_tone_tolerance_db")]
        tolerance_db: f64,
    },
    L6UsbStartup,
    L6DinEventDispatch {
        group: u8,
        cc: u8,
        value: u8,
    },
    L6InputSweep,
    L6DinSweep,
    L6maxInputTiming,
    L6maxPanelKeyMap,
    L6maxChannelSignalPwm,
    L6LevelMeterPwm,
    ExpectEncoderReports,
    ExpectRing {
        channel: usize,
        mask: Option<u32>,
        same_as_checkpoint: Option<String>,
        #[serde(default)]
        changed: bool,
        #[serde(default)]
        only: bool,
        checkpoint: Option<String>,
        #[serde(default = "ring_settle")]
        settle_ms: u32,
    },
    /// The brightest segment of a ring must read this raw PWM level.
    ExpectRingLevel {
        channel: usize,
        #[serde(default)]
        segment: Option<u8>,
        min: u8,
        max: u8,
    },
    ExpectMute {
        channel: usize,
    },
    SelectMode {
        mode: String,
    },
    ExpectMode {
        mode: String,
    },
}

impl Scenario {
    pub fn parse(text: &str) -> Result<Self, String> {
        let mut value: Self = toml::from_str(text).map_err(|e| e.to_string())?;
        value.apply_defaults();
        value.validate()?;
        Ok(value)
    }
    /// A scenario boots from the completed-setup state with an empty card
    /// unless it names others; `"none"` asks for no state (first boot) or no
    /// card.
    fn apply_defaults(&mut self) {
        for (field, default) in [
            (&mut self.state, DEFAULT_STATE),
            (&mut self.card, DEFAULT_CARD),
        ] {
            match field.as_deref() {
                None => *field = Some(default.into()),
                Some(NONE) => *field = None,
                Some(_) => {}
            }
        }
    }
    /// Load the scenario file at `path`. Its includes and images are paths
    /// from the repository root; see [`spec_path`].
    pub fn load(path: &Path) -> Result<Self, String> {
        let path = fs::canonicalize(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let text = fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        Self::compose(&text, Some(path))
    }
    /// Parse and compose a scenario that is not a file, such as a generated
    /// one. Includes resolve from the repository root as in any scenario.
    pub fn from_text(text: &str) -> Result<Self, String> {
        Self::compose(text, None)
    }
    fn compose(text: &str, path: Option<PathBuf>) -> Result<Self, String> {
        let mut value: Self = toml::from_str(text).map_err(|e| e.to_string())?;
        value.apply_defaults();
        value.sources.extend(path.clone());
        let scope = value.models.clone();
        value.step = expand_steps(
            value.step,
            &scope,
            &mut path.into_iter().collect(),
            &mut value.sources,
        )?;
        value.validate()?;
        Ok(value)
    }
    /// Load a scenario for a case process. When the driver built the case
    /// state from a state spec it sets [`STATE_SPEC_ENV`], and setup-input
    /// steps are dropped because the state already holds their result.
    pub fn load_for_case(path: &Path) -> Result<Self, String> {
        let scenario = Self::load(path)?;
        if std::env::var(STATE_SPEC_ENV).as_deref() == Ok("1") {
            scenario.without_setup_input()
        } else {
            Ok(scenario)
        }
    }
    pub fn without_setup_input(mut self) -> Result<Self, String> {
        self.step.retain(|step| !step.setup_input);
        self.spec_state = true;
        self.validate()?;
        Ok(self)
    }
    /// Whether this scenario needs the native patch builder for its setup or
    /// for a native-versus-WASM patch comparison.
    pub fn needs_patch_builder(&self) -> bool {
        self.sd_patches.is_some()
            || !self.patches.is_empty()
            || !self.installed_patches.is_empty()
            || !self.installed_sd_patches.is_empty()
            || !self.update_patches.is_empty()
            || self
                .step
                .iter()
                .any(|step| matches!(step.action, Action::WasmPatch))
    }
}
/// Resolve a path written in a scenario or fragment (an include or a
/// reference image). Every such path is relative to the repository root,
/// whichever file it appears in; absolute paths and `..` are rejected.
pub fn spec_path(path: &Path) -> Result<PathBuf, String> {
    if path.as_os_str().is_empty()
        || !path
            .components()
            .all(|component| matches!(component, std::path::Component::Normal(_)))
    {
        return Err(format!(
            "{}: scenario paths are relative to the repository root, without `..`",
            path.display()
        ));
    }
    Ok(l6max_host::paths::repository_root().join(path))
}
/// The state a scenario boots from when it names none.
pub const DEFAULT_STATE: &str = "setup-complete";
/// The card a scenario gets when it names none.
pub const DEFAULT_CARD: &str = "empty";
/// The `state` or `card` value that asks for none.
const NONE: &str = "none";
/// Environment variable the driver sets to `1` for cases whose persistent
/// state was built from a state spec.
pub const STATE_SPEC_ENV: &str = "SMOKE_STATE_SPEC";
/// A file of steps that scenarios include.
#[derive(Deserialize, schemars::JsonSchema, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Fragment {
    pub step: Vec<Step>,
}
/// Validate a bounded mode transaction without embedding scene test data in Rust.
/// The final Pan selector is required by the diagnostic's subsequent Pan sweep.
pub(crate) fn valid_knob_mode_steps(steps: &[&Step]) -> bool {
    if steps.is_empty() || steps.len() % 10 != 0 || steps.len() / 10 > 23 {
        return false;
    }
    let valid = steps.chunks_exact(10).all(|group| {
        matches!(&group[0].action, Action::KnobDialogModeBegin { scene } if !scene.is_empty() && scene.len() <= 64 && scene.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'))
        && matches!(group[1].action, Action::EnqueueTap { target, hold_ms } if l6ctl::button_source(target).is_some() && (1..=60_000).contains(&hold_ms))
        && matches!(group[2].action, Action::WaitInputApplied)
        && matches!(group[3].action, Action::WaitKnobParameterMode { mode } if mode <= 9)
        && matches!(group[4].action, Action::EnqueueEncoder { channel, steps, midi_transaction: false } if (1..=8).contains(&channel) && steps != 0 && steps.unsigned_abs() <= 1000)
        && matches!(group[5].action, Action::WaitInputApplied)
        && matches!(&group[6].action, Action::CaptureDisplay { guest_ms, artifact } if *guest_ms > 0 && *guest_ms <= group[6].guest_timeout_ms && !artifact.is_empty() && artifact.len() <= 64 && artifact.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'))
        && matches!(&group[7].action, Action::ExpectCapturedDisplayImage { image } if spec_path(image).is_ok() && image.extension().is_some_and(|extension| extension == "png"))
        && matches!(group[8].action, Action::KnobDialogExpectHeap)
        && matches!(group[9].action, Action::KnobDialogModeEnd)
    });
    valid
        && matches!(
            steps[steps.len() - 7].action,
            Action::WaitKnobParameterMode { mode: 8 }
        )
}

pub(crate) fn valid_invalid_tag_steps(steps: &[&Step]) -> bool {
    matches!(steps, [begin, inject1, wait1, confirm1, inject2, wait2, confirm2]
        if matches!(&begin.action, Action::KnobDialogAdaptive { plan } if plan.validate().is_ok())
        && matches!(inject1.action, Action::KnobDialogInjectInvalidTag { tag } if tag & 0x0f >= 10 && tag >> 4 < 8)
        && matches!(wait1.action, Action::KnobDialogWaitInvalidTag)
        && matches!(confirm1.action, Action::KnobDialogConfirmInvalidTag)
        && matches!(inject2.action, Action::KnobDialogInjectInvalidTag { tag } if tag >> 4 >= 8 && tag & 0x0f <= 9)
        && matches!(wait2.action, Action::KnobDialogWaitInvalidTag)
        && matches!(confirm2.action, Action::KnobDialogConfirmInvalidTag))
}

pub(crate) fn valid_invalid_signature_steps(steps: &[&Step]) -> bool {
    let valid_artifact = |name: &str| {
        !name.is_empty()
            && name.len() <= 64
            && name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    };
    matches!(steps, [before, inject, observe, sample, restore, after]
        if matches!(&before.action, Action::KnobDialogSettleLevel { artifact } if valid_artifact(artifact))
        && matches!(inject.action, Action::KnobDialogInjectInvalidSignature)
        && matches!(observe.action, Action::KnobDialogObserveInvalidSignature { guest_ms } if guest_ms > 0 && guest_ms <= observe.guest_timeout_ms)
        && matches!(sample.action, Action::KnobDialogSampleInvalidSignature)
        && matches!(restore.action, Action::KnobDialogRestoreInvalidSignature)
        && matches!(&after.action, Action::KnobDialogSettleLevel { artifact } if valid_artifact(artifact)))
}

pub(crate) fn valid_knob_notification_steps(steps: &[&Step]) -> bool {
    let valid_artifact = |name: &str| {
        !name.is_empty()
            && name.len() <= 64
            && name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    };
    matches!(steps, [prepare, arm, wait, invoke, frame, check, clear, check_clear]
        if matches!(prepare.action, Action::KnobDialogPrepareNotification)
        && matches!(arm.action, Action::KnobDialogArmNotificationCallback)
        && matches!(wait.action, Action::KnobDialogWaitNotificationCallback)
        && matches!(invoke.action, Action::KnobDialogInvokeNotification { message: 4 })
        && matches!(&frame.action, Action::KnobDialogWaitNotificationFrame { clear: false, artifact, guest_ms, limit_ms } if valid_artifact(artifact) && *guest_ms > 0 && *limit_ms >= *guest_ms && *limit_ms <= frame.guest_timeout_ms)
        && matches!(check.action, Action::KnobDialogExpectNotificationInvariants { clear: false })
        && matches!(&clear.action, Action::KnobDialogWaitNotificationFrame { clear: true, artifact, guest_ms, limit_ms } if valid_artifact(artifact) && *guest_ms > 0 && *limit_ms >= *guest_ms && *limit_ms <= clear.guest_timeout_ms)
        && matches!(check_clear.action, Action::KnobDialogExpectNotificationInvariants { clear: true }))
}

pub(crate) fn valid_knob_expiry_steps(steps: &[&Step]) -> bool {
    matches!(steps, [wait, capture, scene, notification]
        if matches!(wait.action, Action::KnobDialogWaitExpiry { interval_ms, limit_ms } if interval_ms > 0 && limit_ms >= interval_ms && limit_ms <= wait.guest_timeout_ms)
        && matches!(&capture.action, Action::CaptureDisplay { guest_ms, artifact } if *guest_ms > 0 && *guest_ms <= capture.guest_timeout_ms && !artifact.is_empty() && artifact.len() <= 64 && artifact.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'))
        && matches!(scene.action, Action::KnobDialogExpectExpiryFrame { after: true })
        && matches!(notification.action, Action::KnobDialogExpectExpiryFrame { after: false }))
}

pub(crate) fn valid_knob_restoration_steps(steps: &[&Step]) -> bool {
    use RestorationField as F;
    const FIELDS: [F; 15] = [
        F::Window,
        F::Signature,
        F::Bitmap,
        F::BitmapDescriptor,
        F::NotificationSemaphore,
        F::TextResources,
        F::SharedPointer,
        F::SharedContents,
        F::TextLengths,
        F::Widget7,
        F::Widget33,
        F::Background10,
        F::Background37,
        F::Background48,
        F::Callback,
    ];
    steps.len() == 18 && matches!(steps[0].action, Action::KnobDialogSnapshotRestoration)
        && steps[1..16].iter().zip(FIELDS).all(|(step, expected)| matches!(step.action, Action::KnobDialogCheckRestoration { field } if field == expected))
        && matches!(steps[16].action, Action::KnobDialogFinishRestoration)
        && matches!(steps[17].action, Action::KnobDialogExpectRestoredHeap)
}

pub(crate) fn valid_knob_navigation_steps(steps: &[&Step]) -> bool {
    use NavigationField as F;
    const FIELDS: [F; 12] = [
        F::Window,
        F::Signature,
        F::Bitmap,
        F::BitmapDescriptor,
        F::NotificationSemaphore,
        F::SharedContents,
        F::Widget7,
        F::Widget33,
        F::Background10,
        F::Background37,
        F::Background48,
        F::Callback,
    ];
    let artifact = |name: &str| {
        !name.is_empty()
            && name.len() <= 64
            && name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    };
    steps.len() == 19
        && matches!(steps[0].action, Action::EnqueueEncoderNoAck { channel: 1, steps: 3 })
        && matches!(&steps[1].action, Action::CaptureDisplay { guest_ms, artifact: name } if *guest_ms > 0 && *guest_ms <= steps[1].guest_timeout_ms && artifact(name))
        && matches!(steps[2].action, Action::ReleaseCapturedDisplay)
        && matches!(steps[3].action, Action::EnqueueTapNoAck { target: 0, hold_ms } if (1..=60_000).contains(&hold_ms))
        && matches!(&steps[4].action, Action::CaptureDisplay { guest_ms, artifact: name } if *guest_ms > 0 && *guest_ms <= steps[4].guest_timeout_ms && artifact(name))
        && matches!(steps[5].action, Action::ReleaseCapturedDisplay)
        && matches!(steps[6].action, Action::KnobDialogSnapshotNavigation)
        && steps[7..].iter().zip(FIELDS).all(|(step, expected)| matches!(step.action, Action::KnobDialogCheckNavigation { field } if field == expected))
}

/// Expand includes. A fragment is shared, so its steps scoped only to models
/// the scenario does not run (`scope`) are left out.
fn expand_steps(
    steps: Vec<Step>,
    scope: &[Model],
    stack: &mut Vec<PathBuf>,
    sources: &mut Vec<PathBuf>,
) -> Result<Vec<Step>, String> {
    let mut expanded = vec![];
    for step in steps {
        if let Action::Include {
            path,
            include_guest_timeout_ms,
            include_wall_timeout_s,
        } = &step.action
        {
            if stack.len() >= 8 {
                return Err("scenario include depth exceeds eight".into());
            }
            let resolved = spec_path(path)?;
            let path = fs::canonicalize(&resolved)
                .map_err(|e| format!("include {}: {e}", resolved.display()))?;
            if stack.contains(&path) {
                return Err(format!("include {} creates a cycle", path.display()));
            }
            let fragment: Fragment =
                toml::from_str(&fs::read_to_string(&path).map_err(|e| e.to_string())?)
                    .map_err(|e| e.to_string())?;
            stack.push(path.clone());
            if !sources.contains(&path) {
                sources.push(path.clone());
            }
            let mut nested = expand_steps(fragment.step, scope, stack, sources)?;
            stack.pop();
            for child in &mut nested {
                child.setup_input |= step.setup_input;
                if !step.models.is_empty() {
                    if child.models.is_empty() {
                        child.models = step.models.clone();
                    } else {
                        child.models.retain(|m| step.models.contains(m));
                        if child.models.is_empty() {
                            return Err("include and child model scopes do not intersect".into());
                        }
                    }
                }
            }
            nested.retain(|child| {
                child.models.is_empty() || child.models.iter().any(|m| scope.contains(m))
            });
            if let Some(timeout) = include_guest_timeout_ms {
                if *timeout == 0
                    || nested.first().is_some_and(|first| {
                        nested
                            .iter()
                            .any(|child| child.guest_timeout_ms != first.guest_timeout_ms)
                    })
                {
                    return Err("include guest timeout override requires a positive value and uniform child guest deadlines".into());
                }
                for child in &mut nested {
                    child.guest_timeout_ms = *timeout;
                }
            }
            if let Some(timeout) = include_wall_timeout_s {
                if *timeout == 0
                    || nested.first().is_some_and(|first| {
                        nested
                            .iter()
                            .any(|child| child.wall_timeout_s != first.wall_timeout_s)
                    })
                {
                    return Err("include wall timeout override requires a positive value and uniform child wall deadlines".into());
                }
                for child in &mut nested {
                    child.wall_timeout_s = *timeout;
                }
            }
            expanded.extend(nested);
        } else {
            expanded.push(step);
        }
    }
    Ok(expanded)
}
fn validate_models(models: &[Model]) -> Result<(), String> {
    if models.is_empty() {
        return Err("models must contain l6 and/or l6max".into());
    }
    Ok(())
}

pub fn is_switch(model: Model, target: u32) -> bool {
    model == Model::L6max && matches!(target, 8 | 13 | 18 | 23 | 47)
}

pub fn l6_button_led_index(model: Model, control: &str) -> Result<usize, String> {
    if model == Model::L6max {
        return match control {
            // L6max has no SEL lamp; EFX and TAP use outputs 23 and 33.
            "efx-select" => Ok(23),
            "tap-button" => Ok(33),
            _ => Err(format!("{control} has no mapped L6max lamp")),
        };
    }
    if model != Model::L6 {
        return Err("unsupported model for LED assertion".into());
    }
    let target = control_target(model, control)?;
    match target {
        10 => Ok(0),
        13 => Ok(1),
        16 => Ok(2),
        19 => Ok(3),
        22 => Ok(4),
        25 => Ok(5),
        2 => Ok(6),
        3 => Ok(7),
        6 => Ok(8),
        47 => Ok(9),
        12 => Ok(10),
        7 => Ok(11),
        _ => Err(format!("{control} has no L6 button LED")),
    }
}

pub(crate) fn valid_overlay_invariant_steps(steps: &[&Step]) -> bool {
    steps.len() == 15
        && matches!(steps[0].action, Action::KnobOverlayBegin)
        && matches!(steps[1].action, Action::KnobOverlayCheckWindow)
        && matches!(steps[2].action, Action::KnobOverlaySnapshotDescriptors)
        && matches!(
            steps[3].action,
            Action::KnobOverlayCheckDescriptor {
                field: OverlayDescriptorField::Widget7
            }
        )
        && matches!(
            steps[4].action,
            Action::KnobOverlayCheckDescriptor {
                field: OverlayDescriptorField::Widget33
            }
        )
        && matches!(
            steps[5].action,
            Action::KnobOverlayCheckDescriptor {
                field: OverlayDescriptorField::Background10
            }
        )
        && matches!(
            steps[6].action,
            Action::KnobOverlayCheckDescriptor {
                field: OverlayDescriptorField::Background37
            }
        )
        && matches!(
            steps[7].action,
            Action::KnobOverlayCheckDescriptor {
                field: OverlayDescriptorField::Background48
            }
        )
        && matches!(steps[8].action, Action::KnobOverlayFinishDescriptors)
        && matches!(steps[9].action, Action::KnobOverlayCheckResource)
        && matches!(steps[10].action, Action::KnobOverlayCheckCorners)
        && matches!(steps[11].action, Action::KnobOverlayCheckSignature)
        && matches!(steps[12].action, Action::KnobOverlayCheckBitmap)
        && matches!(steps[13].action, Action::KnobOverlayCheckHeap)
        && matches!(steps[14].action, Action::KnobOverlayFinish)
}

#[cfg(test)]
mod overlay_fragment_tests {
    use super::*;
    #[test]
    fn descriptor_order_and_completeness_are_enforced() {
        let mut fragment: crate::Fragment = toml::from_str(include_str!(
            "../../../../patches/knob-dialog/tests/emulator/fragments/overlay-invariants.toml"
        ))
        .unwrap();
        assert!(valid_overlay_invariant_steps(
            &fragment.step.iter().collect::<Vec<_>>()
        ));
        fragment.step.swap(3, 4);
        assert!(!valid_overlay_invariant_steps(
            &fragment.step.iter().collect::<Vec<_>>()
        ));
        fragment.step.swap(3, 4);
        fragment.step.pop();
        assert!(!valid_overlay_invariant_steps(
            &fragment.step.iter().collect::<Vec<_>>()
        ));
    }
}

pub(crate) fn valid_refreshed_overlay_steps(steps: &[&Step]) -> bool {
    matches!(steps, [begin, value, capture, pixels, finish]
        if matches!(begin.action, Action::KnobOverlayRefreshBegin)
        && matches!(value.action, Action::KnobOverlayRefreshValue)
        && matches!(&capture.action, Action::CaptureDisplay { guest_ms: 500, artifact } if artifact == "window-scene-refreshed")
        && matches!(pixels.action, Action::KnobOverlayRefreshPixels)
        && matches!(finish.action, Action::KnobOverlayRefreshFinish))
}

#[cfg(test)]
mod refreshed_overlay_fragment_tests {
    use super::*;
    #[test]
    fn rejects_altered_duration_and_missing_pixel_check() {
        let mut fragment: crate::Fragment = toml::from_str(include_str!(
            "../../../../patches/knob-dialog/tests/emulator/fragments/refreshed-overlay.toml"
        ))
        .unwrap();
        assert!(valid_refreshed_overlay_steps(
            &fragment.step.iter().collect::<Vec<_>>()
        ));
        if let Action::CaptureDisplay { guest_ms, .. } = &mut fragment.step[2].action {
            *guest_ms = 499;
        } else {
            panic!("expected capture");
        }
        assert!(!valid_refreshed_overlay_steps(
            &fragment.step.iter().collect::<Vec<_>>()
        ));
        fragment.step.remove(3);
        assert!(!valid_refreshed_overlay_steps(
            &fragment.step.iter().collect::<Vec<_>>()
        ));
    }
}

pub(crate) fn valid_locked_update_steps(steps: &[&Step]) -> bool {
    matches!(steps, [acquire, trigger, wait, sample, restore, check, capture, finish]
        if matches!(acquire.action, Action::KnobLockAcquire)
        && matches!(trigger.action, Action::KnobLockTrigger { delta: -1 })
        && matches!(wait.action, Action::KnobLockWait)
        && matches!(sample.action, Action::KnobLockSample)
        && matches!(restore.action, Action::KnobLockRestore)
        && matches!(check.action, Action::KnobLockAssert)
        && matches!(&capture.action, Action::CaptureDisplay { guest_ms: 500, artifact } if artifact == "window-scene-after-callback-contention")
        && matches!(finish.action, Action::KnobLockFinish))
}

#[cfg(test)]
mod locked_update_fragment_tests {
    use super::*;
    #[test]
    fn requires_restore_before_assert_and_original_capture() {
        let mut fragment: crate::Fragment = toml::from_str(include_str!(
            "../../../../patches/knob-dialog/tests/emulator/fragments/locked-update.toml"
        ))
        .unwrap();
        assert!(valid_locked_update_steps(
            &fragment.step.iter().collect::<Vec<_>>()
        ));
        fragment.step.swap(4, 5);
        assert!(!valid_locked_update_steps(
            &fragment.step.iter().collect::<Vec<_>>()
        ));
        fragment.step.swap(4, 5);
        if let Action::CaptureDisplay { guest_ms, .. } = &mut fragment.step[6].action {
            *guest_ms = 1;
        } else {
            panic!("expected capture");
        }
        assert!(!valid_locked_update_steps(
            &fragment.step.iter().collect::<Vec<_>>()
        ));
    }
}
