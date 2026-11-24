//! Strict scenario schema, composition and semantic model mappings.
use crate::trace;
use l6fw::Model;
pub use l6max_host::controls::{control_target, mode_index};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::{Path, PathBuf},
};

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
    Include {
        path: PathBuf,
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
    DirectUsbMidiRoundTrip {
        #[serde(default = "default_midi_cable")]
        cable: u8,
        #[serde(default = "default_true")]
        expect_ring_change: bool,
    },
    L6maxDinMixerControl {
        expect_din_ring_change: bool,
    },
    L6DinUsbForwarding,
    HostMidiSync {
        /// Send the CC 127 state request. When false the step only collects
        /// the reply to a request made another way, such as a menu item.
        #[serde(default = "default_true")]
        request: bool,
    },
    L6MidiSync,
    L6MidiRingPinSweep {
        cc: u8,
    },
    L6MidiMuteIndicators,
    UsbSysex {
        requests: Vec<UsbSysexRequest>,
        /// Complete positive requests on a matching complete reply.
        #[serde(default)]
        complete_on_match: bool,
    },
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
    ExpectSdPatch {
        loaded: bool,
        #[serde(default)]
        chain_count: Option<u8>,
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
    KnobDialog,
    /// Turn the channel 1 level knob once on the recorder screen and check
    /// that Knob Dialog shows the new value.
    KnobDialogPopup,
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
    /// Press an L6max control and wait until the display changes.
    PressDisplayChange {
        control: String,
    },
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
        Ok(if std::env::var(STATE_SPEC_ENV).as_deref() == Ok("1") {
            scenario.without_setup_input()
        } else {
            scenario
        })
    }
    pub fn without_setup_input(mut self) -> Self {
        self.step.retain(|step| !step.setup_input);
        self.spec_state = true;
        self
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
        if let Action::Include { path } = &step.action {
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
