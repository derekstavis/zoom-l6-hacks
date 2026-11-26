//! Production panel session and the common headless execution adapter.
use crate::{Gesture, Progress, Runner, Scenario, devices};
use l6max_host::{
    engine::{Engine, Options},
    input::InputHub,
    panel::{PanelData, PanelFeed},
};
use serde_json::json;
use std::{cell::Cell, time::Duration};
#[derive(Clone, Debug)]
pub enum Command {
    Switch {
        target: u32,
        enabled: bool,
    },
    Press {
        target: u32,
    },
    HoldDown {
        target: u32,
        duration_ms: u32,
    },
    Release {
        target: u32,
    },
    Encoder {
        channel: usize,
        steps: i32,
        gesture: Gesture,
    },
    Analog {
        channel: usize,
        steps: i32,
        gesture: Gesture,
    },
}
impl Command {
    pub fn headless(&self, inputs: &InputHub) -> Result<(), String> {
        match *self {
            Self::Switch { target, enabled } => {
                if enabled {
                    inputs.down(target, 0)
                } else {
                    inputs.up(target)
                }
                .map_err(|e| e.to_string())?;
            }
            Self::Press { target } => {
                inputs.down(target, 100).map_err(|e| e.to_string())?;
                inputs.up(target).map_err(|e| e.to_string())?;
            }
            Self::HoldDown {
                target,
                duration_ms,
            } => {
                inputs
                    .down(target, duration_ms)
                    .map_err(|e| e.to_string())?;
            }
            Self::Release { target } => {
                inputs.up(target).map_err(|e| e.to_string())?;
            }
            Self::Encoder { channel, steps, .. } => {
                inputs.knob(channel - 1, steps).map_err(|e| e.to_string())?;
            }
            Self::Analog { channel, steps, .. } => {
                inputs
                    .knob(8 + channel - 1, steps)
                    .map_err(|e| e.to_string())?;
            }
        }
        Ok(())
    }
}

/// Owns the same engine, display consumer and snapshot builder used by GPUI.
pub struct Session {
    pub engine: Engine,
    feed: PanelFeed,
    switches: Cell<[bool; 54]>,
}
impl Session {
    pub fn start(options: &Options) -> Result<Self, String> {
        let engine = Engine::start(options)?;
        let feed = PanelFeed::new(&engine)?;
        Ok(Self {
            engine,
            feed,
            switches: Cell::new([false; 54]),
        })
    }
    pub fn dispatch(&self, command: &Command) -> Result<(), String> {
        command.headless(self.engine.inputs.as_ref().unwrap())?;
        if let Command::Switch { target, enabled } = command {
            let mut switches = self.switches.get();
            switches[*target as usize] = *enabled;
            self.switches.set(switches);
        }
        Ok(())
    }
    pub fn poll(&self) -> Result<PanelData, String> {
        self.feed.sample().map_err(|e| e.to_string())?;
        let powered = !self
            .engine
            .inputs
            .as_ref()
            .unwrap()
            .main
            .snapshot()
            .powered_off;
        Ok(self.feed.data(self.switches.get(), powered))
    }
}

/// Execute a scenario without GPUI, with the production panel feed and shared assertions.
pub fn run(scenario: Scenario, mut options: Options) -> Result<(), String> {
    if !scenario.backends.contains(&crate::Backend::Headless) {
        return Err("scenario does not support headless execution".into());
    }
    let progress = crate::progress::CaseProgress::from_env();
    if scenario.step.len() == 1 && matches!(scenario.step[0].action, crate::Action::FirmwareUpdate)
    {
        progress.emit(crate::progress::CaseEvent::ScenarioStarted { total_steps: 1 });
        progress.emit(crate::progress::CaseEvent::StepStarted {
            index: 1,
            label: crate::progress::step_label(&scenario.step[0].action),
            started_unix_ms: crate::progress::unix_ms(),
        });
        devices::prepare(&scenario, &mut options)?;
        let started = std::time::Instant::now();
        let result = devices::run_firmware_update(&options);
        let model = std::fs::read(options.firmware_dir.join("main_firmware.bin"))
            .map_err(|error| error.to_string())
            .and_then(|main| l6fw::model_from_main(&main).map_err(|error| error.to_string()))?;
        let report = json!({
            "schema": 1,
            "scenario": scenario.name,
            "model": model.name(),
            "outcome": if result.is_ok() { "PASS" } else { "FAIL" },
            "failed_step": if result.is_err() { Some(1) } else { None },
            "error": result.as_ref().err(),
            "wall_seconds": started.elapsed().as_secs_f64(),
            "completed_steps": if result.is_ok() { vec![json!({"step":1,"device":true})] } else { vec![] }
        });
        std::fs::write(
            options.logs.join("scenario.json"),
            serde_json::to_vec_pretty(&report).map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
        progress.emit(crate::progress::CaseEvent::ScenarioFinished {
            finished_unix_ms: crate::progress::unix_ms(),
            outcome: if result.is_ok() {
                crate::progress::Outcome::Pass
            } else {
                crate::progress::Outcome::Fail
            },
        });
        return result;
    }
    if scenario.step.len() == 1 && matches!(scenario.step[0].action, crate::Action::WasmPatch) {
        progress.emit(crate::progress::CaseEvent::ScenarioStarted { total_steps: 1 });
        progress.emit(crate::progress::CaseEvent::StepStarted {
            index: 1,
            label: crate::progress::step_label(&scenario.step[0].action),
            started_unix_ms: crate::progress::unix_ms(),
        });
        devices::prepare(&scenario, &mut options)?;
        let started = std::time::Instant::now();
        let result = devices::run_wasm_patch(&options);
        let model = std::fs::read(options.firmware_dir.join("main_firmware.bin"))
            .map_err(|error| error.to_string())
            .and_then(|main| l6fw::model_from_main(&main).map_err(|error| error.to_string()))?;
        let report = json!({
            "schema": 1,
            "scenario": scenario.name,
            "model": model.name(),
            "outcome": if result.is_ok() { "PASS" } else { "FAIL" },
            "failed_step": if result.is_err() { Some(1) } else { None },
            "error": result.as_ref().err(),
            "wall_seconds": started.elapsed().as_secs_f64(),
            "completed_steps": if result.is_ok() { vec![json!({"step":1,"device":true})] } else { vec![] }
        });
        std::fs::write(
            options.logs.join("scenario.json"),
            serde_json::to_vec_pretty(&report).map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
        progress.emit(crate::progress::CaseEvent::ScenarioFinished {
            finished_unix_ms: crate::progress::unix_ms(),
            outcome: if result.is_ok() {
                crate::progress::Outcome::Pass
            } else {
                crate::progress::Outcome::Fail
            },
        });
        return result;
    }
    progress.emit(crate::progress::CaseEvent::PhaseChanged {
        phase: crate::progress::CasePhase::PreparingScenario,
        started_unix_ms: crate::progress::unix_ms(),
    });
    devices::prepare(&scenario, &mut options)?;
    progress.emit(crate::progress::CaseEvent::PhaseChanged {
        phase: crate::progress::CasePhase::StartingEmulator,
        started_unix_ms: crate::progress::unix_ms(),
    });
    let mut session = Some(Session::start(&options)?);
    progress.emit(crate::progress::CaseEvent::PhaseChanged {
        phase: crate::progress::CasePhase::PreparingScenario,
        started_unix_ms: crate::progress::unix_ms(),
    });
    let mut runner = Runner::with_progress(
        scenario,
        session.as_ref().unwrap().engine.model(),
        options.logs.clone(),
        progress,
    )?;
    runner.trace_file(options.logs.join(if options.trace_sd {
        "sdhci.trace"
    } else {
        "engine.trace"
    }));
    let mut device_state = devices::State::default();
    let mut card_inserted = options.sd_image.is_some() && !options.no_sd;
    let mut result: Result<(), String> = (|| {
        loop {
            if runner.current_step() == runner.scenario.step.len()
                && device_state.has_knob_preflight()
            {
                return Err("knob-dialog preflight ended before diagnostic completion".into());
            }
            let progress = {
                let current = session.as_ref().unwrap();
                let data = current.poll()?;
                runner.tick(data, current.engine.inputs.as_ref().unwrap())?
            };
            match progress {
                Progress::Complete => return Ok(()),
                Progress::Dispatch(command) => session.as_ref().unwrap().dispatch(&command)?,
                Progress::Device(devices::Operation::Restart) => {
                    if options.state_dir.is_none() {
                        return Err("restart requires isolated persistent state".into());
                    }
                    device_state.restarted()?;
                    drop(session.take());
                    options.logs = options.logs.join("restart");
                    let mut restart_options = options.clone();
                    if !card_inserted {
                        restart_options.sd_image = None;
                        restart_options.no_sd = true;
                    }
                    session = Some(Session::start(&restart_options)?);
                    runner.restarted(&options.logs);
                    runner.trace_file(options.logs.join(if options.trace_sd {
                        "sdhci.trace"
                    } else {
                        "engine.trace"
                    }));
                    runner.device_finished(json!({"restarted":true,"logs":options.logs}));
                }
                Progress::Device(operation) => {
                    let card_change = match &operation {
                        devices::Operation::SdCard { inserted } => Some(*inserted),
                        _ => None,
                    };
                    let step = &runner.scenario.step[runner.current_step()];
                    let evidence = device_state.execute(
                        session.as_ref().unwrap(),
                        &options,
                        operation,
                        step.guest_timeout_ms,
                        step.wall_timeout_s,
                    )?;
                    if let Some(inserted) = card_change {
                        card_inserted = inserted;
                    }
                    runner.device_finished(evidence);
                }
                Progress::Waiting => {}
            }
            std::thread::sleep(Duration::from_millis(16));
        }
    })();
    if result.is_ok() && device_state.has_knob_preflight() {
        result = Err("knob-dialog preflight ended before diagnostic completion".into());
    }
    if result.is_ok() && device_state.has_midi_probe() {
        result = Err("MIDI probe transaction ended before completion".into());
    }
    if result.is_ok() && device_state.has_midi_sync() {
        result = Err("MIDI sync transaction ended before completion".into());
    }
    device_state.abort_midi_sync();
    device_state.abort_midi_probe();
    if let Err(cleanup) = device_state.abort_knob_preflight() {
        result = Err(match result {
            Ok(()) => format!("debugger cleanup failed: {cleanup}"),
            Err(error) => format!("{error}; debugger cleanup failed: {cleanup}"),
        });
    }
    if let Err(error) = &result {
        runner.finish("FAIL", Some(error))?;
    }
    let mut host_midi = device_state.take_host_midi();
    if let Some(client) = host_midi.as_mut() {
        client.release();
    }
    drop(session);
    if result.is_ok()
        && host_midi
            .as_mut()
            .is_some_and(|client| client.endpoint_present() == Some(true))
    {
        let error = "host MIDI endpoint remained after engine shutdown".to_owned();
        runner.finish("FAIL", Some(&error))?;
        return Err(error);
    }
    if result.is_ok() {
        runner.finish("PASS", None)?;
    }
    result
}
