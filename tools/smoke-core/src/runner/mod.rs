//! Backend-independent scenario state machine and assertions.
use crate::{
    Action, Command, Scenario, control_target, devices,
    scenario::mode_index,
    trace::{self, TraceCursor},
};
use l6fw::Model;
use l6max_host::{
    indicators::{l6_button_leds, l6_mode_leds, leds, rings},
    input::{InputHub, InputStatus},
    panel::PanelData,
    readiness::BootReadiness,
};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, VecDeque},
    fs,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
#[derive(Clone)]
struct Observation {
    data: PanelData,
    main: InputStatus,
    panel: InputStatus,
    reports: u64,
    traces: BTreeMap<String, u64>,
}
#[derive(Clone)]
struct SceneSaveState {
    scene: usize,
    target: u32,
    press_pattern: String,
    release_pattern: String,
    press_count: u64,
    release_count: u64,
}

fn panel_led_state(model: Model, data: PanelData, index: usize) -> bool {
    if model == Model::L6 {
        return l6_button_leds(data.indicators)[index];
    }
    // Match the L6max GUI's sampled PWM threshold so this assertion checks
    // the lamp state the user actually sees, including firmware blink phases.
    let (row, column) = l6max_host::indicators::LED_PINS[index];
    let full_scale = [41u16, 42, 41, 41, 41, 41, 40, 41][row];
    let measured = u16::from(data.indicator_levels[row][(column ^ 7) as usize]);
    ((measured * 255 + full_scale / 2) / full_scale).min(255) >= 128
}

impl Observation {
    fn summary(&self) -> Value {
        json!({"guest_ms": self.panel.guest_ms, "rings": rings(self.data.indicators), "rows": self.data.indicators,
            "ring_levels": l6max_host::indicators::ring_levels(self.data.indicator_levels),
            "main": {"sent":self.main.sent,"applied":self.main.applied,"released":self.main.released,"frames":self.main.frame_ready,"frame_guest_ms":self.main.frame_guest_ms},
            "panel": {"sent":self.panel.sent,"applied":self.panel.applied,"released":self.panel.released}, "encoder_reports":self.reports,"trace_counts":self.traces})
    }
}
#[derive(Debug)]
pub enum Progress {
    Waiting,
    Dispatch(Command),
    Device(devices::Operation),
    Complete,
}
pub struct Runner {
    pub scenario: Scenario,
    model: Model,
    logs: PathBuf,
    step: usize,
    entered: Option<(Instant, u32)>,
    /// For a display check recording a reference: the display and the guest
    /// time it has stayed unchanged since.
    settling: Option<([u8; 1024], u32)>,
    dispatched: bool,
    baseline: Option<Observation>,
    last_command: Option<Command>,
    /// The previous press's start and budgets, shared with its display check.
    last_press_deadline: Option<(Instant, u32, u64, u32)>,
    startup: BootReadiness,
    trace: TraceCursor,
    trace_path: PathBuf,
    history: VecDeque<Value>,
    completed: Vec<Value>,
    last: Option<Observation>,
    started: Instant,
    last_progress: Instant,
    panel_quiet: Option<([u32; 8], u32)>,
    ring_quiet: Option<([u32; 8], u32)>,
    last_report_guest: u32,
    checkpoints: BTreeMap<String, Observation>,
    scene_save: Option<SceneSaveState>,
    allowed_rejections: [u64; 2],
    progress: crate::progress::CaseProgress,
}
impl Runner {
    pub fn new(scenario: Scenario, model: Model, logs: PathBuf) -> Result<Self, String> {
        Self::with_progress(
            scenario,
            model,
            logs,
            crate::progress::CaseProgress::from_env(),
        )
    }
    pub fn with_progress(
        scenario: Scenario,
        model: Model,
        logs: PathBuf,
        progress: crate::progress::CaseProgress,
    ) -> Result<Self, String> {
        if !scenario.models.contains(&model) {
            return Err("scenario does not support this model".into());
        }
        if scenario
            .step
            .iter()
            .any(|s| matches!(s.action, Action::Include { .. }))
        {
            return Err("load includes with Scenario::load before running".into());
        }
        let mut trace = TraceCursor::default();
        for step in &scenario.step {
            if !step.applies_to(model) {
                continue;
            }
            match &step.action {
                Action::ExpectTrace { contains, absent } => {
                    for pattern in contains.iter().chain(absent) {
                        trace.watch(pattern)?;
                    }
                }
                Action::ExpectTraceCount { pattern, .. } => trace.watch(pattern)?,
                Action::ExpectButtonReport { control } => {
                    for pattern in trace::button_packets(model, control)? {
                        trace.watch(&pattern)?;
                    }
                }
                Action::ExpectStartupReport { control } => {
                    for pattern in trace::button_packets(model, control)? {
                        trace.watch(&pattern)?;
                    }
                }
                Action::SaveScene { scene } => {
                    for pattern in trace::button_packets(
                        model,
                        &format!("scene-{}", ['a', 'b', 'c'][scene - 1]),
                    )? {
                        trace.watch(&pattern)?;
                    }
                }
                _ => {}
            }
        }
        fs::create_dir_all(&logs).map_err(|e| e.to_string())?;
        let no_setup_screens = scenario.spec_state;
        progress.emit(crate::progress::CaseEvent::ScenarioStarted {
            total_steps: scenario.step.iter().filter(|s| s.applies_to(model)).count(),
        });
        Ok(Self {
            scenario,
            model,
            trace_path: logs.join("engine.trace"),
            logs,
            step: 0,
            entered: None,
            settling: None,
            dispatched: false,
            baseline: None,
            last_command: None,
            last_press_deadline: None,
            startup: BootReadiness::new(no_setup_screens),
            trace,
            history: Default::default(),
            completed: vec![],
            last: None,
            started: Instant::now(),
            last_progress: Instant::now(),
            panel_quiet: None,
            ring_quiet: None,
            last_report_guest: 0,
            checkpoints: Default::default(),
            scene_save: None,
            allowed_rejections: [0, 0],
            progress,
        })
    }
    /// Advance the scenario. After `Complete`, the adapter must call `finish`
    /// only after its own cleanup and result checks succeed.
    pub fn tick(&mut self, data: PanelData, inputs: &InputHub) -> Result<Progress, String> {
        self.tick_observation(data, inputs.main.snapshot(), inputs.panel.snapshot())
    }
    /// Advance without input; completion has the same explicit `finish` contract as `tick`.
    pub fn tick_without_input(&mut self, data: PanelData) -> Result<Progress, String> {
        let guest_ms = self.last.as_ref().map_or(0, |last| last.panel.guest_ms);
        let status = l6max_host::input::InputStatus {
            connected: true,
            powered_off: !data.powered,
            guest_ms,
            ..Default::default()
        };
        self.tick_observation(data, status.clone(), status)
    }
    fn tick_observation(
        &mut self,
        data: PanelData,
        main: InputStatus,
        panel: InputStatus,
    ) -> Result<Progress, String> {
        let result = self.trace.sample(&self.trace_path).and_then(|reports| {
            self.advance(
                Observation {
                    data,
                    main,
                    panel,
                    reports,
                    traces: self.trace.counts.clone(),
                },
                Instant::now(),
            )
        });
        if let Err(error) = &result {
            self.finish("FAIL", Some(error))?;
        }
        result
    }
    fn advance(&mut self, obs: Observation, now: Instant) -> Result<Progress, String> {
        let mut visible = rings(obs.data.indicators);
        if self.model == Model::L6 {
            visible[6..].fill(0);
        }
        if self.ring_quiet.is_none_or(|(old, _)| old != visible) {
            self.ring_quiet = Some((visible, obs.panel.guest_ms));
        }
        if self
            .last
            .as_ref()
            .is_some_and(|last| last.reports != obs.reports)
        {
            self.last_report_guest = obs.panel.guest_ms;
        }
        let quiet_rows = settling_rows(self.model, obs.data.indicators);
        if self.panel_quiet.is_none_or(|(rows, _)| rows != quiet_rows) {
            self.panel_quiet = Some((quiet_rows, obs.panel.guest_ms));
        }
        let ready = self
            .startup
            .observe(self.model, obs.panel.guest_ms, obs.data.indicators);
        if self.last.as_ref().is_none_or(|last| {
            obs.panel.guest_ms.wrapping_sub(last.panel.guest_ms) >= 1000
                || obs.data.indicators != last.data.indicators
                || obs.panel.applied != last.panel.applied
                || obs.main.applied != last.main.applied
        }) {
            self.record(json!({"event":"observation","step":self.step+1,"state":obs.summary()}));
        }
        if now.saturating_duration_since(self.last_progress) >= Duration::from_secs(15) {
            tracing::info!(event="scenario_progress",scenario=%self.scenario.name,model=self.model.name(),step=self.step+1,guest_ms=obs.panel.guest_ms,rings=?rings(obs.data.indicators),sweep_seen=self.startup.all_lit_seen,"scenario running");
            self.last_progress = now;
        }
        self.last = Some(obs.clone());
        let powered_off_action = self.scenario.step[self.step..]
            .iter()
            .find(|step| step.applies_to(self.model))
            .is_some_and(|step| match &step.action {
                Action::ExpectPowered { powered: false } => true,
                Action::LifecycleStop => true,
                Action::Press { control } if control == "power" => true,
                Action::Hold { control, .. } if control == "power" => true,
                Action::Press { control } if self.model == Model::L6 && control == "rec-record" => {
                    true
                }
                _ => false,
            });
        if !obs.main.connected
            || !obs.panel.connected
            || obs.main.rejected != self.allowed_rejections[0]
            || obs.panel.rejected != self.allowed_rejections[1]
            || (!obs.data.powered
                && !powered_off_action
                && !matches!(
                    self.scenario.step[self.step..]
                        .iter()
                        .find(|s| s.applies_to(self.model))
                        .map(|s| &s.action),
                    Some(Action::Restart)
                ))
        {
            return Err(format!(
                "device input/power failed: main={:?}, panel={:?}",
                obs.main, obs.panel
            ));
        }
        while self.step < self.scenario.step.len() {
            let step = self.scenario.step[self.step].clone();
            if !step.applies_to(self.model) {
                self.step += 1;
                continue;
            }
            let (entered, guest_start) = *self.entered.get_or_insert_with(|| {
                let index = self.scenario.step[..=self.step].iter().filter(|s| s.applies_to(self.model)).count();
                let label = crate::progress::step_label(&step.action);
                self.progress.emit(crate::progress::CaseEvent::StepStarted { index, label, started_unix_ms: crate::progress::unix_ms() });
                tracing::info!(event="step_started",scenario=%self.scenario.name,model=self.model.name(),step=self.step+1,action=?step.action,guest_ms=obs.panel.guest_ms,"step started");
                (now, obs.panel.guest_ms)
            });
            let elapsed = obs.panel.guest_ms.wrapping_sub(guest_start);
            if !matches!(step.action, Action::ExpectDisplayAfterPress)
                && (now.duration_since(entered) > Duration::from_secs(step.wall_timeout_s)
                    || elapsed > step.guest_timeout_ms)
            {
                if let Action::ExpectDisplayImage { image, .. } = &step.action {
                    let path = crate::scenario::spec_path(image)?;
                    return Err(format!(
                        "step {}: {}",
                        self.step + 1,
                        devices::display_mismatch(
                            &obs.data.display,
                            &path,
                            &self.logs,
                            &reference_label(&path)
                        )
                    ));
                }
                return Err(format!(
                    "step {} timed out: {:?}; observed {}; startup={:?}",
                    self.step + 1,
                    step.action,
                    obs.summary(),
                    self.startup
                ));
            }
            let done = match step.action {
                Action::Include { .. } => return Err("unresolved include".into()),
                Action::Checkpoint { name } => {
                    save_display(&obs.data, &self.logs, &name)?;
                    self.record(json!({
                        "event": "checkpoint",
                        "step": self.step + 1,
                        "name": name,
                        "trace_counts": &obs.traces,
                    }));
                    self.checkpoints.insert(name, obs.clone());
                    true
                }
                Action::ExpectDisplay {
                    checkpoint,
                    changed,
                } => {
                    if self.model == Model::L6 {
                        return Err("L6 has no display".into());
                    }
                    let base = self
                        .checkpoints
                        .get(&checkpoint)
                        .ok_or("checkpoint not executed for this model")?;
                    (base.data.display != obs.data.display) == changed
                }
                Action::ExpectDisplayImage { image, settle_ms } => {
                    if self.model == Model::L6 {
                        return Err("L6 has no display".into());
                    }
                    // The display may still be redrawing, so wait, within the
                    // step's timeout, for it to match. A reference is recorded
                    // only once the display has held still for half a second.
                    let path = crate::scenario::spec_path(&image)?;
                    let display = obs.data.display;
                    if devices::reference_differences(&display, &path) == Some(0) {
                        if settle_ms == 0 {
                            true
                        } else {
                            let guest = obs.panel.guest_ms;
                            match self.settling {
                                Some((frame, since)) if frame == display => {
                                    guest.wrapping_sub(since) >= settle_ms
                                }
                                _ => {
                                    self.settling = Some((display, guest));
                                    false
                                }
                            }
                        }
                    } else if devices::updating_references() {
                        let guest = obs.panel.guest_ms;
                        match self.settling {
                            Some((frame, since))
                                if frame == display && guest.wrapping_sub(since) >= 500 =>
                            {
                                devices::record_display_reference(
                                    &display,
                                    &path,
                                    &self.logs,
                                    &reference_label(&path),
                                )?;
                                true
                            }
                            Some((frame, _)) if frame == display => false,
                            _ => {
                                self.settling = Some((display, guest));
                                false
                            }
                        }
                    } else {
                        self.settling = None;
                        false
                    }
                }
                Action::ExpectTrace { contains, absent } => {
                    if absent
                        .iter()
                        .any(|p| obs.traces.get(p).copied().unwrap_or(0) > 0)
                    {
                        return Err("forbidden trace observed".into());
                    }
                    contains
                        .iter()
                        .all(|p| obs.traces.get(p).copied().unwrap_or(0) > 0)
                }
                Action::ExpectTraceCount {
                    pattern,
                    count,
                    at_least,
                    checkpoint,
                } => {
                    let current = obs.traces.get(&pattern).copied().unwrap_or(0);
                    let baseline = checkpoint
                        .as_ref()
                        .and_then(|name| self.checkpoints.get(name))
                        .and_then(|observation| observation.traces.get(&pattern))
                        .copied()
                        .unwrap_or(0);
                    let observed = current.saturating_sub(baseline);
                    if at_least {
                        observed >= count
                    } else if observed > count {
                        return Err(format!(
                            "trace {pattern:?} occurred {observed} times, expected exactly {count}"
                        ));
                    } else {
                        observed == count
                    }
                }
                Action::ExpectButtonReport { control } => {
                    if !matches!(self.last_command, Some(Command::Press { target } | Command::Switch { target, .. }) if target == control_target(self.model, &control)?)
                    {
                        return Err(
                            "button report assertion needs the matching preceding press".into()
                        );
                    }
                    let base = self.baseline.as_ref().unwrap();
                    let mut complete = true;
                    for (index, pattern) in trace::button_packets(self.model, &control)?
                        .into_iter()
                        .enumerate()
                    {
                        let delta = obs
                            .traces
                            .get(&pattern)
                            .copied()
                            .unwrap_or(0)
                            .saturating_sub(base.traces.get(&pattern).copied().unwrap_or(0));
                        let expected = match self.last_command {
                            Some(Command::Switch { enabled, .. }) => {
                                u64::from(enabled == (index == 0))
                            }
                            _ => 1,
                        };
                        if delta > expected {
                            return Err(format!(
                                "duplicate or unexpected button report for {control}"
                            ));
                        }
                        complete &= delta == expected;
                    }
                    complete && elapsed >= 1000
                }
                Action::ExpectStartupReport { control } => {
                    let press = &trace::button_packets(self.model, &control)?[0];
                    obs.traces.get(press).copied().unwrap_or(0) > 0
                }
                Action::SdCard { inserted } => {
                    return Ok(Progress::Device(devices::Operation::SdCard { inserted }));
                }
                Action::UsbDescriptors => {
                    return Ok(Progress::Device(devices::Operation::UsbDescriptors));
                }
                Action::UsbMidi { value } => {
                    return Ok(Progress::Device(devices::Operation::UsbMidi { value }));
                }
                Action::HostMidiConnect => {
                    return Ok(Progress::Device(devices::Operation::HostMidiConnect));
                }
                Action::HostMidiDrain { quiet_wall_ms } => {
                    return Ok(Progress::Device(devices::Operation::HostMidiDrain {
                        quiet_wall_ms,
                    }));
                }
                Action::HostMidiExpectCount { count } => {
                    return Ok(Progress::Device(devices::Operation::HostMidiExpectCount {
                        count,
                    }));
                }
                Action::HostMidiBegin => {
                    return Ok(Progress::Device(devices::Operation::HostMidiBegin));
                }
                Action::HostMidiEnd => {
                    return Ok(Progress::Device(devices::Operation::HostMidiEnd));
                }
                Action::EnqueueEncoder {
                    channel,
                    steps,
                    midi_transaction,
                } => {
                    return Ok(Progress::Device(devices::Operation::EnqueueEncoder {
                        channel,
                        steps,
                        midi_transaction,
                    }));
                }
                Action::HostMidiExpectCc {
                    channel,
                    controller,
                } => {
                    return Ok(Progress::Device(devices::Operation::HostMidiExpectCc {
                        channel,
                        controller,
                    }));
                }
                Action::HostMidiSettle { guest_ms } => {
                    return Ok(Progress::Device(devices::Operation::HostMidiSettle {
                        guest_ms,
                    }));
                }
                Action::HostMidiSendCc {
                    channel,
                    controller,
                    value,
                } => {
                    return Ok(Progress::Device(devices::Operation::HostMidiSendCc {
                        channel,
                        controller,
                        value,
                    }));
                }
                Action::ExpectRingChanged {
                    channel,
                    checkpoint,
                    midi_transaction,
                } => {
                    let before = self
                        .checkpoints
                        .get(&checkpoint)
                        .ok_or("checkpoint not executed for this model")?
                        .data
                        .indicators;
                    return Ok(Progress::Device(devices::Operation::ExpectRingChanged {
                        channel,
                        before,
                        midi_transaction,
                    }));
                }
                Action::HostMidiRoundTrip => {
                    return Ok(Progress::Device(devices::Operation::HostMidiRoundTrip));
                }
                Action::MidiProbeBegin => {
                    return Ok(Progress::Device(devices::Operation::MidiProbeBegin));
                }
                Action::MidiProbeSendCc {
                    port,
                    channel,
                    controller,
                    value,
                    capture_ring_channel,
                } => {
                    return Ok(Progress::Device(devices::Operation::MidiProbeSendCc {
                        port,
                        channel,
                        controller,
                        value,
                        capture_ring_channel,
                    }));
                }
                Action::MidiProbeExpectRing {
                    channel,
                    changed,
                    guest_ms,
                    wall_ms,
                } => {
                    return Ok(Progress::Device(devices::Operation::MidiProbeExpectRing {
                        channel,
                        changed,
                        guest_ms,
                        wall_ms,
                    }));
                }
                Action::MidiProbeWaitRingStable {
                    channel,
                    guest_ms,
                    wall_ms,
                } => {
                    return Ok(Progress::Device(
                        devices::Operation::MidiProbeWaitRingStable {
                            channel,
                            guest_ms,
                            wall_ms,
                        },
                    ));
                }
                Action::MidiProbeDrainAndTap {
                    port,
                    target,
                    hold_ms,
                } => {
                    return Ok(Progress::Device(devices::Operation::MidiProbeDrainAndTap {
                        port,
                        target,
                        hold_ms,
                    }));
                }
                Action::MidiProbeExpectNoEvents { port, wall_ms } => {
                    return Ok(Progress::Device(
                        devices::Operation::MidiProbeExpectNoEvents { port, wall_ms },
                    ));
                }
                Action::MidiProbeEnd => {
                    return Ok(Progress::Device(devices::Operation::MidiProbeEnd));
                }
                Action::DirectUsbMidiRoundTrip {
                    cable,
                    expect_ring_change,
                } => {
                    return Ok(Progress::Device(
                        devices::Operation::DirectUsbMidiRoundTrip {
                            cable,
                            expect_ring_change,
                        },
                    ));
                }
                Action::L6DinUsbForwarding => {
                    return Ok(Progress::Device(devices::Operation::L6DinUsbForwarding));
                }
                Action::HostMidiSyncBegin => {
                    return Ok(Progress::Device(devices::Operation::HostMidiSyncBegin));
                }
                Action::HostMidiExpectSync => {
                    return Ok(Progress::Device(devices::Operation::HostMidiExpectSync));
                }
                Action::L6MidiProbeBegin => {
                    return Ok(Progress::Device(devices::Operation::L6MidiProbeBegin));
                }
                Action::L6MidiProbeInject => {
                    return Ok(Progress::Device(devices::Operation::L6MidiProbeInject));
                }
                Action::L6MidiProbeExpectHook => {
                    return Ok(Progress::Device(devices::Operation::L6MidiProbeExpectHook));
                }
                Action::L6MidiProbeExpectReplies => {
                    return Ok(Progress::Device(
                        devices::Operation::L6MidiProbeExpectReplies,
                    ));
                }
                Action::L6MidiRingPinSweep { cc } => {
                    return Ok(Progress::Device(devices::Operation::L6MidiRingPinSweep {
                        cc,
                    }));
                }
                Action::L6MidiMuteIndicators => {
                    return Ok(Progress::Device(devices::Operation::L6MidiMuteIndicators));
                }
                Action::UsbSysex {
                    requests,
                    complete_on_match,
                } => {
                    return Ok(Progress::Device(devices::Operation::UsbSysex {
                        requests,
                        complete_on_match,
                    }));
                }
                Action::EditorPadAssignments => {
                    return Ok(Progress::Device(devices::Operation::EditorPadAssignments));
                }
                Action::EditorPadPlayback { pad } => {
                    return Ok(Progress::Device(devices::Operation::EditorPadPlayback {
                        pad,
                    }));
                }
                Action::L6maxUiPadVerify { pad } => {
                    return Ok(Progress::Device(devices::Operation::L6maxUiPadVerify {
                        pad,
                    }));
                }
                Action::SoundPadPlayback {
                    pad,
                    target,
                    assignment,
                    require_pad_led_change,
                    playback_window_ms,
                    require_stereo_meter,
                    expect_source,
                    max_residual_db,
                } => {
                    return Ok(Progress::Device(devices::Operation::SoundPadPlayback {
                        pad,
                        target,
                        assignment,
                        require_pad_led_change,
                        playback_window_ms,
                        require_stereo_meter,
                        expect_source,
                        max_residual_db,
                    }));
                }
                Action::WaitL6SoundPadFile { path } => {
                    return Ok(Progress::Device(devices::Operation::WaitL6SoundPadFile {
                        path,
                    }));
                }
                Action::L6RecorderPlayback { target } => {
                    return Ok(Progress::Device(devices::Operation::L6RecorderPlayback {
                        target,
                    }));
                }
                Action::L6maxRecorderPlayback { target } => {
                    return Ok(Progress::Device(
                        devices::Operation::L6maxRecorderPlayback { target },
                    ));
                }
                Action::ExpectMemory { symbol, bytes } => {
                    let address = crate::firmware::symbol(self.model, &symbol)
                        .ok_or_else(|| format!("{symbol} is not declared for this model"))?;
                    return Ok(Progress::Device(devices::Operation::ExpectMemory {
                        symbol,
                        address,
                        bytes,
                    }));
                }
                Action::WaitMemory { symbol, bytes } => {
                    let address = crate::firmware::symbol(self.model, &symbol)
                        .ok_or_else(|| format!("{symbol} is not declared for this model"))?;
                    return Ok(Progress::Device(devices::Operation::WaitMemory {
                        symbol,
                        address,
                        bytes,
                    }));
                }
                Action::ExpectSdLoader => {
                    return Ok(Progress::Device(devices::Operation::ExpectSdLoader));
                }
                Action::ExpectSdModule {
                    loaded,
                    chain_count,
                } => {
                    return Ok(Progress::Device(devices::Operation::ExpectSdModule {
                        loaded,
                        chain_count,
                    }));
                }
                Action::ExpectSdPatch {
                    loaded,
                    chain_count,
                } => {
                    return Ok(Progress::Device(devices::Operation::ExpectSdPatch {
                        loaded,
                        chain_count,
                    }));
                }
                Action::EffectParameterEdit { encoder, steps } => {
                    return Ok(Progress::Device(devices::Operation::EffectParameterEdit {
                        encoder,
                        steps,
                    }));
                }
                Action::WaitEffectMode {
                    panel_mode,
                    edit_mode,
                } => {
                    return Ok(Progress::Device(devices::Operation::WaitEffectMode {
                        panel_mode,
                        edit_mode,
                    }));
                }
                Action::EffectReplacementState {
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
                    return Ok(Progress::Device(
                        devices::Operation::EffectReplacementState {
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
                        },
                    ));
                }
                Action::ExpectHeap { minimum_free } => {
                    return Ok(Progress::Device(devices::Operation::ExpectHeap {
                        minimum_free,
                    }));
                }
                Action::HoldChord {
                    controls,
                    duration_ms,
                    until,
                } => {
                    let targets = controls
                        .iter()
                        .map(|control| control_target(self.model, control))
                        .collect::<Result<Vec<_>, _>>()?;
                    return Ok(Progress::Device(devices::Operation::HoldChord {
                        targets,
                        duration_ms,
                        until,
                    }));
                }
                Action::UsbStorage => return Ok(Progress::Device(devices::Operation::UsbStorage)),
                Action::ExpectRecorder => {
                    return Ok(Progress::Device(devices::Operation::ExpectRecorder));
                }
                Action::Restart => return Ok(Progress::Device(devices::Operation::Restart)),
                Action::WarmReset => return Ok(Progress::Device(devices::Operation::WarmReset)),
                Action::PowerPress { hold_ms, powered } => {
                    return Ok(Progress::Device(devices::Operation::PowerPress {
                        hold_ms,
                        powered,
                    }));
                }
                Action::ExpectPowered { powered } => obs.data.powered == powered,
                Action::ExpectSettings { restored } => {
                    return Ok(Progress::Device(devices::Operation::ExpectSettings {
                        restored,
                    }));
                }
                Action::AudioCallbacks => {
                    return Ok(Progress::Device(devices::Operation::AudioCallbacks));
                }
                Action::KnobDialogReady => {
                    return Ok(Progress::Device(devices::Operation::KnobDialogReady));
                }
                Action::KnobDialogGuard => {
                    return Ok(Progress::Device(devices::Operation::KnobDialogGuard));
                }
                Action::KnobDialogBaseline => {
                    return Ok(Progress::Device(devices::Operation::KnobDialogBaseline));
                }
                Action::KnobDialogPreflightBegin => {
                    return Ok(Progress::Device(
                        devices::Operation::KnobDialogPreflightBegin,
                    ));
                }
                Action::KnobDialogPreflightDescriptor => {
                    return Ok(Progress::Device(
                        devices::Operation::KnobDialogPreflightDescriptor,
                    ));
                }
                Action::KnobDialogPreflightTextAlias => {
                    return Ok(Progress::Device(
                        devices::Operation::KnobDialogPreflightTextAlias,
                    ));
                }
                Action::KnobDialogPreflightLayerCollision => {
                    return Ok(Progress::Device(
                        devices::Operation::KnobDialogPreflightLayerCollision,
                    ));
                }
                Action::KnobDialogPreflightWidgetLayer => {
                    return Ok(Progress::Device(
                        devices::Operation::KnobDialogPreflightWidgetLayer,
                    ));
                }
                Action::KnobDialogPreflightBackgroundLayer => {
                    return Ok(Progress::Device(
                        devices::Operation::KnobDialogPreflightBackgroundLayer,
                    ));
                }
                Action::KnobDialogCallbackRace => {
                    return Ok(Progress::Device(devices::Operation::KnobDialogCallbackRace));
                }
                Action::KnobDialogFirstActivation => {
                    return Ok(Progress::Device(
                        devices::Operation::KnobDialogFirstActivation,
                    ));
                }
                Action::KnobDialogActiveDisplay => {
                    return Ok(Progress::Device(
                        devices::Operation::KnobDialogActiveDisplay,
                    ));
                }
                Action::KnobDialogLevelDemo => {
                    return Ok(Progress::Device(devices::Operation::KnobDialogLevelDemo));
                }
                Action::KnobDialogTaskContexts => {
                    return Ok(Progress::Device(devices::Operation::KnobDialogTaskContexts));
                }
                Action::KnobDialogOverlayInvariants => {
                    return Ok(Progress::Device(
                        devices::Operation::KnobDialogOverlayInvariants,
                    ));
                }
                Action::KnobDialogBitmapZero => {
                    return Ok(Progress::Device(devices::Operation::KnobDialogBitmapZero));
                }
                Action::KnobDialogActiveOverlay => {
                    return Ok(Progress::Device(
                        devices::Operation::KnobDialogActiveOverlay,
                    ));
                }
                Action::KnobDialogLockedUpdate => {
                    return Ok(Progress::Device(devices::Operation::KnobDialogLockedUpdate));
                }
                Action::KnobDialogModeBegin { scene } => {
                    return Ok(Progress::Device(devices::Operation::KnobDialogModeBegin {
                        scene: scene.clone(),
                    }));
                }
                Action::EnqueueTap { target, hold_ms } => {
                    return Ok(Progress::Device(devices::Operation::EnqueueTap {
                        target,
                        hold_ms,
                    }));
                }
                Action::WaitInputApplied => {
                    return Ok(Progress::Device(devices::Operation::WaitInputApplied));
                }
                Action::WaitKnobParameterMode { mode } => {
                    return Ok(Progress::Device(
                        devices::Operation::WaitKnobParameterMode { mode },
                    ));
                }
                Action::CaptureDisplay { guest_ms, artifact } => {
                    return Ok(Progress::Device(devices::Operation::CaptureDisplay {
                        guest_ms,
                        artifact: artifact.clone(),
                    }));
                }
                Action::ExpectCapturedDisplayImage { image } => {
                    return Ok(Progress::Device(
                        devices::Operation::ExpectCapturedDisplayImage {
                            image: image.clone(),
                        },
                    ));
                }
                Action::KnobDialogExpectHeap => {
                    return Ok(Progress::Device(devices::Operation::KnobDialogExpectHeap));
                }
                Action::KnobDialogModeEnd => {
                    return Ok(Progress::Device(devices::Operation::KnobDialogModeEnd));
                }
                Action::KnobDialog => {
                    return Ok(Progress::Device(devices::Operation::KnobDialog));
                }
                Action::KnobPopupOpen {
                    first_steps,
                    fallback_steps,
                    attempt_guest_ms,
                } => {
                    return Ok(Progress::Device(devices::Operation::KnobPopupOpen {
                        first_steps,
                        fallback_steps,
                        attempt_guest_ms,
                    }));
                }
                Action::KnobPopupExpectOpen => {
                    return Ok(Progress::Device(devices::Operation::KnobPopupExpectOpen));
                }
                Action::KnobPopupExpectValue => {
                    return Ok(Progress::Device(devices::Operation::KnobPopupExpectValue));
                }
                Action::ExpectMenu { menu, path, items } => {
                    return Ok(Progress::Device(devices::Operation::ExpectMenu {
                        menu,
                        path,
                        items,
                    }));
                }
                Action::LifecycleStart { profile } => {
                    return Ok(Progress::Device(devices::Operation::LifecycleStart {
                        profile,
                    }));
                }
                Action::LifecycleStop => {
                    return Ok(Progress::Device(devices::Operation::LifecycleStop));
                }
                Action::ExpectLiveUpdateMarkerAbsent => {
                    return Ok(Progress::Device(
                        devices::Operation::ExpectLiveUpdateMarkerAbsent,
                    ));
                }
                Action::MarkL6UpdatePending => {
                    return Ok(Progress::Device(devices::Operation::MarkL6UpdatePending));
                }
                Action::ExpectL6UpdateResetVector => {
                    return Ok(Progress::Device(
                        devices::Operation::ExpectL6UpdateResetVector,
                    ));
                }
                Action::ExpectL6UpdateComparison { field } => {
                    return Ok(Progress::Device(
                        devices::Operation::ExpectL6UpdateComparison { field },
                    ));
                }
                Action::CaptureUpdateMarkerPending => {
                    return Ok(Progress::Device(
                        devices::Operation::CaptureUpdateMarkerPending,
                    ));
                }
                Action::ExpectInstallerOutcome { expected } => {
                    return Ok(Progress::Device(
                        devices::Operation::ExpectInstallerOutcome { expected },
                    ));
                }
                Action::BeginWallDeadline { duration_s } => {
                    return Ok(Progress::Device(devices::Operation::BeginWallDeadline {
                        duration_s,
                    }));
                }
                Action::EndWallDeadline => {
                    return Ok(Progress::Device(devices::Operation::EndWallDeadline));
                }
                Action::WaitUsbController => {
                    return Ok(Progress::Device(devices::Operation::WaitUsbController));
                }
                Action::WaitUpdatePanelReady {
                    require_programming,
                } => {
                    return Ok(Progress::Device(devices::Operation::WaitUpdatePanelReady {
                        require_programming,
                    }));
                }
                Action::ExpectUpdateMode { mode } => {
                    return Ok(Progress::Device(devices::Operation::ExpectUpdateMode {
                        mode,
                    }));
                }
                Action::ExpectMainMemory { address, bytes } => {
                    return Ok(Progress::Device(devices::Operation::ExpectMainMemory {
                        address,
                        bytes,
                    }));
                }
                Action::ExpectUpdateComparison { field } => {
                    return Ok(Progress::Device(
                        devices::Operation::ExpectUpdateComparison { field },
                    ));
                }
                Action::CaptureInstalledPanel => {
                    return Ok(Progress::Device(devices::Operation::CaptureInstalledPanel));
                }
                Action::ExpectPanelUnchanged => {
                    return Ok(Progress::Device(devices::Operation::ExpectPanelUnchanged));
                }
                Action::ExpectUpdateGui {
                    window,
                    progress_mask,
                    progress_value,
                } => {
                    return Ok(Progress::Device(devices::Operation::ExpectUpdateGui {
                        window,
                        progress_mask,
                        progress_value,
                    }));
                }
                Action::WaitPanelUptime { guest_ms } => {
                    return Ok(Progress::Device(devices::Operation::WaitPanelUptime {
                        guest_ms,
                    }));
                }
                Action::ExpectUpdateState {
                    mode,
                    flags_mask,
                    flags_value,
                } => {
                    return Ok(Progress::Device(devices::Operation::ExpectUpdateState {
                        mode,
                        flags_mask,
                        flags_value,
                    }));
                }
                Action::ExpectUpdateMarkerAbsent => {
                    return Ok(Progress::Device(
                        devices::Operation::ExpectUpdateMarkerAbsent,
                    ));
                }
                Action::FirmwareUpdate => {
                    return Ok(Progress::Device(devices::Operation::FirmwareUpdate));
                }
                Action::WasmPatch => {
                    return Ok(Progress::Device(devices::Operation::WasmPatch));
                }
                Action::PanelEvent {
                    target,
                    column,
                    row,
                    event,
                } => {
                    return Ok(Progress::Device(devices::Operation::PanelEvent {
                        target,
                        column,
                        row,
                        event,
                    }));
                }
                Action::L6MainGpioEvent { target, event } => {
                    return Ok(Progress::Device(devices::Operation::L6MainGpioEvent {
                        target,
                        event,
                    }));
                }
                Action::ExpectCardEntry {
                    path,
                    size,
                    matches_state,
                } => {
                    return Ok(Progress::Device(devices::Operation::ExpectCardEntry {
                        path: path.clone(),
                        size,
                        matches_state,
                    }));
                }
                Action::WaitRecordedFiles { files } => {
                    return Ok(Progress::Device(devices::Operation::WaitRecordedFiles {
                        files,
                    }));
                }
                Action::AudioInputTransport {
                    command,
                    position_ms,
                } => {
                    return Ok(Progress::Device(devices::Operation::AudioInputTransport {
                        command: command.clone(),
                        position_ms,
                    }));
                }
                Action::AudioInputLoad { channel, loaded } => {
                    return Ok(Progress::Device(devices::Operation::AudioInputLoad {
                        channel,
                        loaded,
                    }));
                }
                Action::WaitAudioInput {
                    playing,
                    min_ms,
                    max_ms,
                } => {
                    return Ok(Progress::Device(devices::Operation::WaitAudioInput {
                        playing,
                        min_ms,
                        max_ms,
                    }));
                }
                Action::ExpectAudioInput {
                    playing,
                    min_ms,
                    max_ms,
                } => {
                    return Ok(Progress::Device(devices::Operation::ExpectAudioInput {
                        playing,
                        min_ms,
                        max_ms,
                    }));
                }
                Action::ExpectOutputTone {
                    hz,
                    left_db,
                    right_db,
                    tolerance_db,
                } => {
                    return Ok(Progress::Device(devices::Operation::ExpectTone {
                        file: None,
                        hz,
                        levels: [left_db, right_db],
                        tolerance_db,
                    }));
                }
                Action::ExpectRecordedTone {
                    file,
                    hz,
                    left_db,
                    right_db,
                    tolerance_db,
                } => {
                    return Ok(Progress::Device(devices::Operation::ExpectTone {
                        file: Some(file.clone()),
                        hz,
                        levels: [left_db, right_db],
                        tolerance_db,
                    }));
                }
                Action::L6maxTapHandler => {
                    return Ok(Progress::Device(devices::Operation::L6maxTapHandler));
                }
                Action::L6UsbStartup => {
                    return Ok(Progress::Device(devices::Operation::L6UsbStartup));
                }
                Action::L6DinEventDispatch { group, cc, value } => {
                    return Ok(Progress::Device(devices::Operation::L6DinEventDispatch {
                        group,
                        cc,
                        value,
                    }));
                }
                Action::L6InputSweep => {
                    return Ok(Progress::Device(devices::Operation::L6InputSweep));
                }
                Action::L6DinSweep => {
                    return Ok(Progress::Device(devices::Operation::L6DinSweep));
                }
                Action::L6maxInputTiming => {
                    return Ok(Progress::Device(devices::Operation::L6maxInputTiming));
                }
                Action::L6maxPanelKeyMap => {
                    return Ok(Progress::Device(devices::Operation::L6maxPanelKeyMap));
                }
                Action::L6maxChannelSignalPwm => {
                    return Ok(Progress::Device(devices::Operation::L6maxChannelSignalPwm));
                }
                Action::L6LevelMeterPwm => {
                    return Ok(Progress::Device(devices::Operation::L6LevelMeterPwm));
                }
                Action::Wait { until } => {
                    if until == "sd-patch-loaded" {
                        return Ok(Progress::Device(devices::Operation::WaitSdPatchLoaded));
                    }
                    if until == "sd-patch-empty" {
                        return Ok(Progress::Device(devices::Operation::WaitSdPatchEmpty));
                    }
                    if until == "boot-ready" {
                        // Without the setup floor, L6max also needs its screen
                        // to have held one frame for a guest second.
                        let display_settled = !self.scenario.spec_state
                            || obs.panel.guest_ms.wrapping_sub(obs.main.frame_guest_ms) >= 1000;
                        ready
                            && (self.model == Model::L6
                                || (obs.data.display.iter().any(|b| *b != 0) && display_settled))
                    } else if until == "mixer-ready" {
                        // MAIN restores every ring at once after the sweep;
                        // L6 then lights its editing-mode lamp.
                        rings(obs.data.indicators)[..self.model.channels()]
                            .iter()
                            .all(|mask| *mask != 0)
                            && (self.model != Model::L6
                                || l6_mode_leds(obs.data.indicators).contains(&true))
                    } else {
                        self.panel_quiet.is_some_and(|(_, since)| {
                            obs.panel.guest_ms.wrapping_sub(since) >= 1000
                        })
                    }
                }
                Action::SelectMode { mode } => {
                    let index = mode_index(self.model, &mode)?;
                    let lit = if self.model == Model::L6 {
                        l6_mode_leds(obs.data.indicators)[index]
                    } else {
                        leds(obs.data.indicators)[17 + index]
                    };
                    if !self.dispatched && !lit {
                        let settled = self.panel_quiet.is_some_and(|(_, since)| {
                            obs.panel.guest_ms.wrapping_sub(since) >= 1000
                        });
                        if !settled {
                            return Ok(Progress::Waiting);
                        }
                        return self.dispatch(
                            Command::Press {
                                target: control_target(self.model, &format!("mode.{mode}"))?,
                            },
                            obs,
                        );
                    }
                    lit && (!self.dispatched || self.acknowledged(&obs)?)
                }
                Action::ExpectMode { mode } => {
                    let index = mode_index(self.model, &mode)?;
                    if self.model == Model::L6 {
                        l6_mode_leds(obs.data.indicators)[index]
                    } else {
                        leds(obs.data.indicators)[17 + index]
                    }
                }
                Action::Delay { guest_ms } | Action::ObserveDuration { guest_ms, .. } => {
                    elapsed >= guest_ms
                }
                Action::Switch { control, enabled } => {
                    if !self.dispatched {
                        return self.dispatch(
                            Command::Switch {
                                target: control_target(self.model, &control)?,
                                enabled,
                            },
                            obs,
                        );
                    }
                    self.acknowledged(&obs)?
                        && obs.data.switches[control_target(self.model, &control)? as usize]
                            == enabled
                }
                Action::ExpectDisplayAfterPress => {
                    let (press_started, press_guest, wall_timeout, guest_timeout) = self
                        .last_press_deadline
                        .ok_or("display change requires a preceding press")?;
                    if !matches!(self.last_command, Some(Command::Press { .. })) {
                        return Err("display change requires a preceding press".into());
                    }
                    if now.duration_since(press_started) > Duration::from_secs(wall_timeout)
                        || obs.panel.guest_ms.wrapping_sub(press_guest) > guest_timeout
                    {
                        return Err(format!(
                            "step {} timed out: display did not change before the press deadline; observed {}; startup={:?}",
                            self.step + 1,
                            obs.summary(),
                            self.startup
                        ));
                    }
                    self.acknowledged(&obs)?
                        && self
                            .baseline
                            .as_ref()
                            .is_some_and(|baseline| baseline.data.display != obs.data.display)
                }
                Action::Press { control } => {
                    if !self.dispatched {
                        return self.dispatch(
                            Command::Press {
                                target: control_target(self.model, &control)?,
                            },
                            obs,
                        );
                    }
                    let target = control_target(self.model, &control)?;
                    if control == "power"
                        && self
                            .baseline
                            .as_ref()
                            .is_some_and(|baseline| baseline.data.powered != obs.data.powered)
                    {
                        true
                    } else if !obs.data.powered && self.model == Model::L6 && target != 53 {
                        obs.data.switches[target as usize]
                    } else {
                        self.acknowledged(&obs)?
                    }
                }
                Action::Hold {
                    control,
                    duration_ms,
                } => {
                    let target = control_target(self.model, &control)?;
                    if !self.dispatched {
                        return self.dispatch(
                            Command::HoldDown {
                                target,
                                duration_ms,
                            },
                            obs,
                        );
                    }
                    if control == "power" && !obs.data.powered {
                        if elapsed < duration_ms {
                            return Err("power shut down before the required hold duration".into());
                        }
                        true
                    } else {
                        match self.last_command.as_ref() {
                            Some(Command::HoldDown { .. }) if elapsed < duration_ms => false,
                            Some(Command::HoldDown { .. }) => {
                                return self.dispatch(Command::Release { target }, obs);
                            }
                            Some(Command::Release { .. }) => self.acknowledged(&obs)?,
                            _ => return Err("hold action lost its press state".into()),
                        }
                    }
                }
                Action::SaveScene { scene } => {
                    if self.model != Model::L6 {
                        return Err("save-scene currently supports L6 only".into());
                    }
                    let control = format!("scene-{}", ['a', 'b', 'c'][scene - 1]);
                    let target = control_target(self.model, &control)?;
                    if !self.dispatched {
                        let [press_pattern, release_pattern] =
                            trace::button_packets(self.model, &control)?;
                        let state = SceneSaveState {
                            scene,
                            target,
                            press_count: obs.traces.get(&press_pattern).copied().unwrap_or(0),
                            release_count: obs.traces.get(&release_pattern).copied().unwrap_or(0),
                            press_pattern,
                            release_pattern,
                        };
                        self.scene_save = Some(state);
                        return self.dispatch(
                            Command::HoldDown {
                                target,
                                // Keep the panel key down until the scene lamp responds.
                                // This is the same 50 ms minimum used by a normal click.
                                duration_ms: 50,
                            },
                            obs,
                        );
                    }
                    let state = self
                        .scene_save
                        .as_ref()
                        .ok_or("save-scene lost its press baseline")?
                        .clone();
                    if state.scene != scene || state.target != target {
                        return Err("save-scene state does not match the current step".into());
                    }
                    match self.last_command.as_ref() {
                        Some(Command::HoldDown { target: held, .. }) if *held == target => {
                            let press_delta = obs
                                .traces
                                .get(&state.press_pattern)
                                .copied()
                                .unwrap_or(0)
                                .saturating_sub(state.press_count);
                            if press_delta > 1 {
                                return Err(format!("duplicate L6 scene {scene} press report"));
                            }
                            // Only this scene's lamp: another just saved can still be
                            // flashing, so the others are not a stable baseline.
                            if self.acknowledged(&obs)?
                                && press_delta == 1
                                && l6max_host::indicators::l6_scene_leds(obs.data.indicators)
                                    [scene - 1]
                            {
                                return self.dispatch(Command::Release { target }, obs);
                            }
                            false
                        }
                        Some(Command::Release { target: released }) if *released == target => {
                            let release_delta = obs
                                .traces
                                .get(&state.release_pattern)
                                .copied()
                                .unwrap_or(0)
                                .saturating_sub(state.release_count);
                            if release_delta > 1 {
                                return Err(format!("duplicate L6 scene {scene} release report"));
                            }
                            self.acknowledged(&obs)? && release_delta == 1
                        }
                        _ => return Err("save-scene lost its input state".into()),
                    }
                }
                Action::ExpectScene {
                    scene,
                    lit,
                    checkpoint,
                    changed,
                } => {
                    if self.model != Model::L6 {
                        return Err("scene lamp assertions currently support L6 only".into());
                    }
                    let current = l6max_host::indicators::l6_scene_leds(obs.data.indicators);
                    let previous = checkpoint
                        .as_ref()
                        .map(|name| {
                            self.checkpoints
                                .get(name)
                                .ok_or("checkpoint not executed for this model")
                                .map(|state| {
                                    l6max_host::indicators::l6_scene_leds(state.data.indicators)
                                })
                        })
                        .transpose()?;
                    current[scene - 1] == lit
                        && (!changed
                            || previous
                                .is_some_and(|previous| current[scene - 1] != previous[scene - 1]))
                }
                Action::ExpectButtonLed {
                    control,
                    lit,
                    checkpoint,
                    changed,
                    same,
                } => {
                    let index = crate::scenario::l6_button_led_index(self.model, &control)?;
                    let current = panel_led_state(self.model, obs.data, index);
                    let previous = checkpoint
                        .as_ref()
                        .map(|name| {
                            self.checkpoints
                                .get(name)
                                .ok_or("checkpoint not executed for this model")
                                .map(|state| panel_led_state(self.model, state.data, index))
                        })
                        .transpose()?;
                    lit.is_none_or(|lit| current == lit)
                        && (!changed || previous.is_some_and(|previous| current != previous))
                        && (!same || previous.is_some_and(|previous| current == previous))
                }
                Action::ExpectEffect { effect } => {
                    if self.model != Model::L6 {
                        return Err("effect assertions currently support L6 only".into());
                    }
                    let effects = ["hall", "room", "spring", "delay", "echo"];
                    let index = effects
                        .iter()
                        .position(|value| *value == effect)
                        .ok_or("unknown effect")?;
                    l6max_host::indicators::l6_effect_leds(obs.data.indicators)
                        == std::array::from_fn(|i| i == index)
                }
                Action::Encoder {
                    channel,
                    steps,
                    gesture,
                } => {
                    if !self.dispatched {
                        return self.dispatch(
                            Command::Encoder {
                                channel,
                                steps,
                                gesture,
                            },
                            obs,
                        );
                    }
                    self.acknowledged(&obs)?
                }
                Action::Analog {
                    channel,
                    steps,
                    gesture,
                } => {
                    if !self.dispatched {
                        return self.dispatch(
                            Command::Analog {
                                channel,
                                steps,
                                gesture,
                            },
                            obs,
                        );
                    }
                    self.acknowledged(&obs)?
                }
                Action::ExpectAnalog { channel, value } => {
                    let Some(Command::Analog {
                        channel: previous, ..
                    }) = self.last_command.as_ref()
                    else {
                        return Err("analog assertion needs a preceding analog action".into());
                    };
                    if *previous != channel {
                        return Err("analog assertion channel differs from preceding action".into());
                    }
                    let adc_index = (l6max_host::controls::analog_channels(self.model)[channel - 1]
                        - 3) as usize;
                    obs.data.analog[channel - 1] == value
                        && obs.main.adc_samples[adc_index] == value
                }
                Action::ExpectEncoderReports => {
                    if !matches!(self.last_command, Some(Command::Encoder { .. })) {
                        return Err(
                            "encoder report assertion needs a preceding encoder action".into()
                        );
                    }
                    self.encoder_reports_ready(&obs)?
                        && obs.panel.guest_ms.wrapping_sub(self.last_report_guest) >= 1000
                }
                Action::ExpectRing {
                    channel,
                    mask,
                    same_as_checkpoint,
                    changed,
                    only,
                    checkpoint,
                    settle_ms,
                } => {
                    let base = self
                        .baseline
                        .as_ref()
                        .ok_or("ring assertion needs a preceding action")?;
                    let before = rings(base.data.indicators);
                    let after = rings(obs.data.indicators);
                    let same_as_checkpoint = same_as_checkpoint.as_ref().is_none_or(|name| {
                        self.checkpoints.get(name).is_some_and(|checkpoint| {
                            rings(checkpoint.data.indicators)[channel - 1] == after[channel - 1]
                        })
                    });
                    let reports_ready = self.encoder_reports_ready(&obs)?;
                    if only
                        && after
                            .iter()
                            .take(self.model.channels())
                            .zip(before)
                            .enumerate()
                            .any(|(i, (a, b))| i != channel - 1 && *a != b)
                    {
                        return Err("encoder changed an unrelated ring".into());
                    }
                    let settled = self.ring_quiet.is_some_and(|(_, since)| {
                        obs.panel.guest_ms.wrapping_sub(since) >= settle_ms
                    }) && obs.panel.guest_ms.wrapping_sub(self.last_report_guest)
                        >= settle_ms;
                    reports_ready
                        && settled
                        // Firmware accelerates encoders by report spacing, so
                        // assert the direction of a turn and not its exact mask.
                        && checkpoint.as_ref().is_none_or(|_| {
                            let moved = after[channel - 1].count_ones() as i64
                                - before[channel - 1].count_ones() as i64;
                            match self.last_command {
                                Some(Command::Encoder { steps, .. }) => {
                                    moved.signum() == i64::from(steps.signum())
                                }
                                _ => moved == 0,
                            }
                        })
                        && mask.is_none_or(|v| after[channel - 1] == v)
                        && same_as_checkpoint
                        && (!changed || before[channel - 1] != after[channel - 1])
                }
                Action::ExpectRingLevel {
                    channel,
                    segment,
                    min,
                    max,
                } => {
                    let levels =
                        l6max_host::indicators::ring_levels(obs.data.indicator_levels)[channel - 1];
                    let level = segment.map_or_else(
                        || levels.into_iter().max().unwrap_or(0),
                        |segment| levels[usize::from(segment - 1)],
                    );
                    (min..=max).contains(&level)
                }
                Action::ExpectMute { channel } => {
                    let base = self
                        .baseline
                        .as_ref()
                        .ok_or("mute assertion needs a preceding action")?;
                    let decode = |data: PanelData| {
                        if self.model == Model::L6 {
                            l6_button_leds(data.indicators)[channel - 1]
                        } else {
                            leds(data.indicators)[channel - 1]
                        }
                    };
                    decode(obs.data) != decode(base.data)
                        && self.panel_quiet.is_some_and(|(_, since)| {
                            obs.panel.guest_ms.wrapping_sub(since) >= 1000
                        })
                }
            };
            if !done {
                return Ok(Progress::Waiting);
            }
            self.record(
                json!({"event":"step-passed","step":self.step+1,"observation":obs.summary()}),
            );
            self.completed
                .push(json!({"step":self.step+1,"guest_ms":obs.panel.guest_ms}));
            self.step += 1;
            self.entered = None;
            self.settling = None;
            self.dispatched = false;
            self.scene_save = None;
        }
        Ok(Progress::Complete)
    }
    fn encoder_reports_ready(&self, obs: &Observation) -> Result<bool, String> {
        let Some(Command::Encoder { steps, .. }) = self.last_command else {
            return Ok(true);
        };
        let base = self
            .baseline
            .as_ref()
            .ok_or("encoder assertion needs an action baseline")?;
        let delta = obs.reports.saturating_sub(base.reports);
        if delta > u64::from(steps.unsigned_abs()) {
            return Err(format!("duplicate encoder reports: {delta}"));
        }
        Ok(delta == u64::from(steps.unsigned_abs()))
    }
    pub(crate) fn trace_file(&mut self, path: PathBuf) {
        self.trace_path = path;
    }
    pub(crate) fn restarted(&mut self, logs: &Path) {
        self.guest_restarted(logs);
        self.baseline = None;
        self.last_command = None;
    }
    pub fn guest_restarted(&mut self, logs: &Path) {
        self.trace.restart();
        self.trace_path = logs.join("engine.trace");
        self.startup = BootReadiness::new(self.scenario.spec_state);
        self.entered = None;
        self.settling = None;
        self.last_press_deadline = None;
        self.panel_quiet = None;
        self.ring_quiet = None;
        self.last_report_guest = 0;
        self.last = None;
        self.allowed_rejections = [0, 0];
    }
    pub fn current_step(&self) -> usize {
        self.step
    }

    /// Complete a typed engine operation; its assertion evidence joins the same report.
    pub fn device_finished(&mut self, evidence: Value) {
        if let Some(expected) = evidence.get("expected_rejections") {
            if let (Some(main), Some(panel)) = (
                expected.get("main").and_then(Value::as_u64),
                expected.get("panel").and_then(Value::as_u64),
            ) {
                self.allowed_rejections = [main, panel];
            }
        }
        self.record(json!({"event":"device-result","step":self.step+1,"evidence":evidence}));
        self.completed
            .push(json!({"step":self.step+1,"device":true}));
        self.step += 1;
        self.entered = None;
        self.settling = None;
        self.dispatched = false;
    }
    fn dispatch(&mut self, command: Command, obs: Observation) -> Result<Progress, String> {
        self.last_press_deadline = match (
            &command,
            &self.scenario.step[self.step].action,
            self.entered,
        ) {
            (Command::Press { .. }, Action::Press { .. }, Some((started, guest))) => {
                let step = &self.scenario.step[self.step];
                Some((started, guest, step.wall_timeout_s, step.guest_timeout_ms))
            }
            _ => None,
        };
        self.record(json!({"event":"dispatch","step":self.step+1,"command":format!("{command:?}"),"observation":obs.summary()}));
        self.baseline = Some(obs);
        self.last_command = Some(command.clone());
        self.dispatched = true;
        Ok(Progress::Dispatch(command))
    }
    fn acknowledged(&self, obs: &Observation) -> Result<bool, String> {
        let base = self.baseline.as_ref().unwrap();
        let (current, old, count) = match self.last_command.as_ref().unwrap() {
            Command::Encoder { .. } | Command::Switch { .. } => (&obs.panel, &base.panel, 1),
            Command::Analog { .. } => (&obs.main, &base.main, 1),
            Command::HoldDown { target, .. } | Command::Release { target } => {
                if matches!(target, 0 | 4 | 5 | 48..=53) {
                    (&obs.main, &base.main, 1)
                } else {
                    (&obs.panel, &base.panel, 1)
                }
            }
            Command::Press { target } if matches!(target, 0 | 4 | 5 | 48..=53) => {
                (&obs.main, &base.main, 2)
            }
            Command::Press { .. } => (&obs.panel, &base.panel, 2),
        };
        if current.sent > old.sent + count {
            return Err("unexpected concurrent input; leave the test window untouched".into());
        }
        // DOWN and explicit UP each return APPLIED; UP is acknowledged after
        // the minimum hold expires. RELEASED belongs to automatic TAP only.
        Ok(current.applied >= old.applied + count)
    }

    fn record(&mut self, event: Value) {
        tracing::debug!(event=event["event"].as_str().unwrap_or("observation"),scenario=%self.scenario.name,model=self.model.name(),step=self.step+1,detail=%event,"scenario event");
        if self.history.len() == 128 {
            self.history.pop_front();
        }
        self.history.push_back(event);
    }
    pub fn finish(&self, outcome: &str, error: Option<&str>) -> Result<(), String> {
        self.progress
            .emit(crate::progress::CaseEvent::ScenarioFinished {
                finished_unix_ms: crate::progress::unix_ms(),
                outcome: if outcome == "PASS" {
                    crate::progress::Outcome::Pass
                } else {
                    crate::progress::Outcome::Fail
                },
            });
        if let Some(error) = error {
            tracing::error!(event="scenario_finished",scenario=%self.scenario.name,model=self.model.name(),step=self.step+1,outcome,%error,"scenario failed");
        } else {
            tracing::info!(event="scenario_finished",scenario=%self.scenario.name,model=self.model.name(),outcome,"scenario completed");
        }
        let report = json!({"schema":1,"scenario":self.scenario.name,"model":self.model.name(),"outcome":outcome,
            "failed_step": if error.is_some(){Some(self.step+1)}else{None}, "error":error,"wall_seconds":self.started.elapsed().as_secs_f64(),
            "trace_file":self.trace_path,"completed_steps":self.completed,"last_observation":self.last.as_ref().map(Observation::summary),"events":self.history});
        fs::write(
            self.logs.join("scenario.json"),
            serde_json::to_vec_pretty(&report).unwrap(),
        )
        .map_err(|e| e.to_string())?;
        if let Some(last) = &self.last {
            save_display(&last.data, &self.logs, "display")?;
        }
        Ok(())
    }
}

fn save_display(data: &PanelData, logs: &Path, name: &str) -> Result<(), String> {
    fs::write(logs.join(format!("{name}.txt")), data.display_ascii()).map_err(|e| e.to_string())?;
    image::GrayImage::from_fn(128, 64, |x, y| {
        let index = ((63 - y as usize) / 8) * 128 + x as usize;
        image::Luma(
            [if data.display[index] >> ((63 - y as usize) % 8) & 1 != 0 {
                255
            } else {
                0
            }],
        )
    })
    .save(logs.join(format!("{name}.png")))
    .map_err(|e| e.to_string())
}

/// L6max non-ring lamp index of TAP in `indicators::LED_PINS`.
const L6MAX_TAP_LED: usize = 33;
/// Indicator rows used to decide that panel output has settled. The L6max
/// TAP lamp flashes at the effect tempo (about every 500 guest ms at the
/// default 120 BPM) for as long as the device runs, so it never settles.
/// It is excluded; every other lamp and ring still has to hold still.
fn settling_rows(model: Model, mut rows: [u32; 8]) -> [u32; 8] {
    if model == Model::L6max {
        let (row, column) = l6max_host::indicators::LED_PINS[L6MAX_TAP_LED];
        rows[row] &= !(1 << (column ^ 7));
    }
    rows
}

#[cfg(test)]
mod tests;

/// The name a display check's saved frame takes after its reference.
fn reference_label(path: &Path) -> String {
    path.file_stem()
        .map_or("display".into(), |stem| stem.to_string_lossy().into_owned())
}
