//! Production panel session and the common headless execution adapter.
use crate::{Gesture, Progress, Runner, Scenario, devices};
use l6max_host::{
    engine::{Engine, Options},
    panel::{PanelData, PanelFeed},
};
use serde_json::json;
use std::{
    cell::Cell,
    time::{Duration, Instant},
};
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
    pub fn headless(&self, engine: &Engine) -> Result<(), String> {
        let inputs = engine.inputs.as_ref().ok_or("input hub unavailable")?;
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
                l6ctl::OwnedControl::new(engine)
                    .down(target, duration_ms)
                    .map_err(|e| e.to_string())?;
            }
            Self::Release { target } => {
                l6ctl::OwnedControl::new(engine)
                    .up(target)
                    .map_err(|e| e.to_string())?;
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
    pub(crate) supervised_din: bool,
}
impl Session {
    pub fn start(options: &Options) -> Result<Self, String> {
        let engine = Engine::start(options)?;
        let feed = PanelFeed::new(&engine)?;
        Ok(Self {
            engine,
            feed,
            switches: Cell::new([false; 54]),
            supervised_din: false,
        })
    }
    pub fn dispatch(&self, command: &Command) -> Result<(), String> {
        command.headless(&self.engine)?;
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

/// Wait for an absolute panel-guest uptime while tolerating the panel being
/// held in reset as long as MAIN advances, as the updater does.
struct DualClockWatch {
    last: (u32, u32),
    deadline: Instant,
}
impl DualClockWatch {
    fn new(now: Instant) -> Self {
        Self {
            last: (0, 0),
            deadline: now + Duration::from_secs(60),
        }
    }
    fn stalled(&mut self, panel: u32, main: u32, now: Instant) -> bool {
        if (panel, main) != self.last {
            self.last = (panel, main);
            self.deadline = now + Duration::from_secs(60);
        }
        now >= self.deadline
    }
}

fn wait_panel_uptime(session: &Session, target: u32, wall_timeout_s: u64) -> Result<u32, String> {
    let control = l6ctl::OwnedControl::new(&session.engine);
    let deadline = Instant::now() + Duration::from_secs(wall_timeout_s);
    let mut watch = DualClockWatch::new(Instant::now());
    loop {
        if !session.poll()?.powered {
            return Err("guest powered off before panel uptime".into());
        }
        let main = control
            .input_status(l6ctl::InputSource::Main)
            .map_err(|e| e.to_string())?;
        let panel = control
            .input_status(l6ctl::InputSource::Panel)
            .map_err(|e| e.to_string())?;
        if panel.guest_ms >= target {
            return Ok(panel.guest_ms);
        }
        let now = Instant::now();
        if now >= deadline {
            return Err("panel uptime exceeded wall deadline".into());
        }
        if watch.stalled(panel.guest_ms, main.guest_ms, now) {
            return Err(format!(
                "guest clocks stalled at panel {} ms, MAIN {} ms",
                panel.guest_ms, main.guest_ms
            ));
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn panel_update_ready(
    mode: u32,
    image_matches: bool,
    rtc_response: bool,
    writes: usize,
    image_verified: bool,
    require_programming: bool,
) -> bool {
    mode == 4
        && image_matches
        && rtc_response
        && (!require_programming || (writes == 64 && image_verified))
}

fn verify_frozen_snapshot(path: &std::path::Path, expected: &[u8]) -> Result<(), String> {
    let actual = std::fs::read(path).map_err(|e| e.to_string())?;
    if actual != expected {
        return Err(format!(
            "snapshot changed after capture: {}",
            path.display()
        ));
    }
    Ok(())
}

fn wait_update_panel_ready(
    session: &Session,
    state: &std::path::Path,
    package: &[u8],
    trace_path: &std::path::Path,
    anchor: (Instant, u32),
    guest_timeout_ms: u32,
    wall_timeout_s: u64,
    require_programming: bool,
) -> Result<(), String> {
    let control = l6ctl::OwnedControl::new(&session.engine);
    let (started, start) = anchor;
    let deadline = started + Duration::from_secs(wall_timeout_s);
    loop {
        if !session.poll()?.powered {
            return Err("guest powered off before panel image matched".into());
        }
        let image_matches = match std::fs::read(state.join("panel-flash.bin")) {
            Ok(panel) => l6nor::update::panel_image_matches(&panel, package)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
            Err(error) => return Err(error.to_string()),
        };
        let trace = match std::fs::read_to_string(trace_path) {
            Ok(trace) => trace,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(error) => return Err(error.to_string()),
        };
        let mode = control.update_state().map_err(|e| e.to_string())?.mode;
        if panel_update_ready(
            mode,
            image_matches,
            trace.contains("l6 panel RTC response start"),
            trace
                .lines()
                .filter(|line| line.starts_with("l6 panel ROM write 080"))
                .count(),
            trace.contains("l6 panel ROM 16 KiB image match: yes"),
            require_programming,
        ) {
            return Ok(());
        }
        let guest = control
            .input_status(l6ctl::InputSource::Panel)
            .map_err(|e| e.to_string())?
            .guest_ms;
        if guest.wrapping_sub(start) >= guest_timeout_ms || Instant::now() >= deadline {
            return Err("panel update readiness did not converge before deadline".into());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[derive(Clone, Copy)]
struct WallBudget {
    deadline: Instant,
}
impl WallBudget {
    fn new(start: Instant, duration_s: u64) -> Result<Self, String> {
        if duration_s == 0 {
            return Err("shared wall deadline must be positive".into());
        }
        let deadline = start
            .checked_add(Duration::from_secs(duration_s))
            .ok_or("shared wall deadline overflow")?;
        Ok(Self { deadline })
    }
    fn check(&self, now: Instant) -> Result<(), String> {
        if now >= self.deadline {
            Err("shared wall deadline exceeded".into())
        } else {
            Ok(())
        }
    }
}

fn verify_patched_artifacts(
    logs: &std::path::Path,
    inputs: &serde_json::Value,
) -> Result<(), String> {
    use sha2::{Digest, Sha256};
    let verify = logs.join("verify-firmware");
    let expected = inputs["verify_firmware"]
        .as_object()
        .ok_or("missing frozen verify firmware identity")?;
    let mut actual = std::collections::BTreeMap::new();
    fn visit(
        root: &std::path::Path,
        dir: &std::path::Path,
        files: &mut std::collections::BTreeMap<String, String>,
    ) -> Result<(), String> {
        for entry in std::fs::read_dir(dir).map_err(|e| e.to_string())? {
            let path = entry.map_err(|e| e.to_string())?.path();
            if path.is_symlink() {
                return Err("symlink in verify firmware".into());
            }
            if path.is_dir() {
                visit(root, &path, files)?;
            } else {
                let bytes = std::fs::read(&path).map_err(|e| e.to_string())?;
                files.insert(
                    path.strip_prefix(root)
                        .unwrap()
                        .to_string_lossy()
                        .into_owned(),
                    format!("{:x}", Sha256::digest(&bytes)),
                );
            }
        }
        Ok(())
    }
    visit(&verify, &verify, &mut actual)?;
    if serde_json::to_value(&actual).map_err(|e| e.to_string())?
        != serde_json::Value::Object(expected.clone())
    {
        return Err("verify firmware changed after staging".into());
    }
    let report = std::fs::read(logs.join("patched-update.json")).map_err(|e| e.to_string())?;
    if inputs["update_report_sha256"].as_str()
        != Some(format!("{:x}", Sha256::digest(&report)).as_str())
        || std::fs::read(verify.join(crate::fixture_layout::FIRMWARE_REPORT))
            .map_err(|e| e.to_string())?
            != report
    {
        return Err("update report changed after staging".into());
    }
    Ok(())
}

fn device_options<'a>(
    managed: bool,
    case: &'a Options,
    boot: Option<&'a Options>,
) -> Result<&'a Options, String> {
    if managed {
        boot.ok_or_else(|| "device action needs active lifecycle boot options".into())
    } else {
        Ok(case)
    }
}

/// Execute a scenario without GPUI, with the production panel feed and shared assertions.
pub fn run(scenario: Scenario, options: Options) -> Result<(), String> {
    run_with_context(scenario, options, false)
}

/// Run a direct headless DIN case whose caller owns this fresh process group.
pub fn run_supervised_din(scenario: Scenario, options: Options) -> Result<(), String> {
    if unsafe { libc::getpgrp() } != unsafe { libc::getpid() } {
        return Err("DIN case is not a fresh process-group leader".into());
    }
    run_with_context(scenario, options, true)
}

fn run_with_context(
    scenario: Scenario,
    mut options: Options,
    supervised_din: bool,
) -> Result<(), String> {
    if !scenario.backends.contains(&crate::Backend::Headless) {
        return Err("scenario does not support headless execution".into());
    }
    if scenario
        .step
        .iter()
        .any(|step| matches!(step.action, crate::Action::PatchParityPrepare))
    {
        return Err("patch parity is a host-only l6test scenario".into());
    }
    let progress = crate::progress::CaseProgress::from_env();
    progress.emit(crate::progress::CaseEvent::PhaseChanged {
        phase: crate::progress::CasePhase::PreparingScenario,
        started_unix_ms: crate::progress::unix_ms(),
    });
    devices::prepare(&scenario, &mut options)?;
    progress.emit(crate::progress::CaseEvent::PhaseChanged {
        phase: crate::progress::CasePhase::StartingEmulator,
        started_unix_ms: crate::progress::unix_ms(),
    });
    let managed = scenario.update_fixture.is_some();
    let mut managed_inputs = None;
    let managed_package = if managed {
        use sha2::{Digest, Sha256};
        let package =
            std::fs::read(options.logs.join("staged-update.bin")).map_err(|e| e.to_string())?;
        let inputs: serde_json::Value = serde_json::from_slice(
            &std::fs::read(options.logs.join("inputs.json")).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
        let digest = format!("{:x}", Sha256::digest(&package));
        if inputs["update_card_package_sha256"].as_str() != Some(digest.as_str()) {
            return Err("staged update package identity changed before execution".into());
        }
        if scenario
            .update_fixture
            .is_some_and(crate::scenario::UpdateFixture::is_patched)
        {
            verify_patched_artifacts(&options.logs, &inputs)?;
        }
        managed_inputs = Some(inputs);
        Some(package)
    } else {
        None
    };
    let mut session = if managed {
        None
    } else {
        Some(Session::start(&options)?)
    };
    if let Some(session) = session.as_mut() {
        session.supervised_din = supervised_din;
    }
    progress.emit(crate::progress::CaseEvent::PhaseChanged {
        phase: crate::progress::CasePhase::PreparingScenario,
        started_unix_ms: crate::progress::unix_ms(),
    });
    let mut runner = Runner::with_progress(
        scenario,
        if let Some(session) = &session {
            session.engine.model()
        } else {
            let main = std::fs::read(options.firmware_dir.join("main_firmware.bin"))
                .map_err(|error| error.to_string())?;
            l6fw::model_from_main(&main).map_err(|error| error.to_string())?
        },
        options.logs.clone(),
        progress.clone(),
    )?;
    runner.trace_file(options.logs.join(if options.trace_sd {
        "sdhci.trace"
    } else {
        "engine.trace"
    }));
    let case_progress = progress.clone();
    let mut device_state = devices::State::default();
    let mut lifecycle_last_boot = None;
    let mut lifecycle_boot_options: Option<Options> = None;
    let mut installer_anchor = None;
    let mut shared_wall_budget: Option<WallBudget> = None;
    let mut pending_snapshot: Option<Vec<u8>> = None;
    let mut installed_panel_snapshot: Option<Vec<u8>> = None;
    let mut card_inserted = options.sd_image.is_some() && !options.no_sd;
    let mut result: Result<(), String> = (|| {
        loop {
            if let Some(budget) = shared_wall_budget {
                budget.check(Instant::now())?;
            }
            if runner.current_step() == runner.scenario.step.len()
                && device_state.has_knob_preflight()
            {
                return Err("knob-dialog preflight ended before diagnostic completion".into());
            }
            let progress = {
                if let Some(current) = &session {
                    let data = current.poll()?;
                    runner.tick(data, current.engine.inputs.as_ref().unwrap())?
                } else {
                    runner.tick_without_input(PanelData {
                        powered: true,
                        ..Default::default()
                    })?
                }
            };
            if let Some(budget) = shared_wall_budget {
                budget.check(Instant::now())?;
            }
            match progress {
                Progress::Complete => {
                    if shared_wall_budget.is_some() {
                        return Err("shared wall deadline was not ended".into());
                    }
                    if let Some(error) = runner.deferred_assertion_error() {
                        runner.mark_deferred_terminal();
                        return Err(error);
                    }
                    return Ok(());
                }
                Progress::Device(devices::Operation::BeginWallDeadline { duration_s }) => {
                    if shared_wall_budget.is_some() {
                        return Err("shared wall deadline is already active".into());
                    }
                    shared_wall_budget = Some(WallBudget::new(Instant::now(), duration_s)?);
                    runner.device_finished(json!({"duration_s":duration_s}));
                }
                Progress::Device(devices::Operation::EndWallDeadline) => {
                    let budget = shared_wall_budget
                        .take()
                        .ok_or("no active shared wall deadline")?;
                    budget.check(Instant::now())?;
                    runner.device_finished(json!({"deadline":"completed"}));
                }
                Progress::Dispatch(command) => session.as_ref().unwrap().dispatch(&command)?,
                Progress::Device(devices::Operation::LifecycleStart { profile }) => {
                    if session.is_some() || options.state_dir.is_none() {
                        return Err(
                            "lifecycle start requires an inactive guest and isolated state".into(),
                        );
                    }
                    use crate::scenario::LifecycleBoot;
                    let expected_previous = match profile {
                        LifecycleBoot::Initial => None,
                        LifecycleBoot::Installer => Some(LifecycleBoot::Initial),
                        LifecycleBoot::Restart => Some(LifecycleBoot::Installer),
                        LifecycleBoot::Verify => Some(
                            if runner.scenario.update_fixture
                                == Some(crate::scenario::UpdateFixture::L6PatchedMidi)
                            {
                                LifecycleBoot::Installer
                            } else {
                                LifecycleBoot::Restart
                            },
                        ),
                    };
                    if lifecycle_last_boot != expected_previous {
                        return Err("lifecycle boot profile is out of order".into());
                    }
                    let mut boot = options.clone();
                    match profile {
                        LifecycleBoot::Initial
                            if runner
                                .scenario
                                .update_fixture
                                .is_some_and(crate::scenario::UpdateFixture::is_l6) =>
                        {
                            boot.sd_image = None;
                            boot.no_sd = true;
                        }
                        LifecycleBoot::Initial | LifecycleBoot::Installer => {
                            if boot.sd_image.is_none() || boot.no_sd {
                                return Err("update boot needs its staged card".into());
                            }
                            let package = managed_package
                                .as_deref()
                                .ok_or("missing frozen update package")?;
                            let card_package = sd_image::read_file(
                                boot.sd_image.as_ref().unwrap(),
                                if runner
                                    .scenario
                                    .update_fixture
                                    .is_some_and(crate::scenario::UpdateFixture::is_l6)
                                {
                                    "L6.BIN"
                                } else {
                                    "L6max.BIN"
                                },
                                package.len() as u64 + 1,
                            )
                            .map_err(|e| e.to_string())?;
                            if card_package != package {
                                return Err("update card package changed after staging".into());
                            }
                        }
                        LifecycleBoot::Restart | LifecycleBoot::Verify => {
                            boot.sd_image = None;
                            boot.no_sd = true;
                        }
                    }
                    if profile == LifecycleBoot::Installer
                        && runner
                            .scenario
                            .update_fixture
                            .is_some_and(crate::scenario::UpdateFixture::is_l6)
                    {
                        let frozen = pending_snapshot
                            .as_deref()
                            .ok_or("missing pending L6 snapshot")?;
                        verify_frozen_snapshot(&options.logs.join("pending-main-nor.bin"), frozen)?;
                        verify_frozen_snapshot(
                            &boot
                                .state_dir
                                .as_ref()
                                .ok_or("missing L6 state")?
                                .join("main-nor.bin"),
                            frozen,
                        )?;
                    }
                    boot.update_bootloader =
                        matches!(profile, LifecycleBoot::Installer | LifecycleBoot::Restart);
                    if profile == LifecycleBoot::Verify {
                        if !runner
                            .scenario
                            .update_fixture
                            .is_some_and(crate::scenario::UpdateFixture::is_patched)
                        {
                            return Err("verify boot requires patched update".into());
                        }
                        verify_patched_artifacts(
                            &options.logs,
                            managed_inputs.as_ref().ok_or("missing managed identity")?,
                        )?;
                        boot.firmware_dir = options.logs.join("verify-firmware");
                        let l6_verify = runner.scenario.update_fixture
                            == Some(crate::scenario::UpdateFixture::L6PatchedMidi);
                        if l6_verify {
                            // The legacy L6 probe boots the unpacked patched image in a fresh disposable session.
                            boot.state_dir = None;
                        } else if matches!(
                            runner.scenario.update_fixture,
                            Some(
                                crate::scenario::UpdateFixture::PatchedMidi
                                    | crate::scenario::UpdateFixture::PatchedKnobMidi
                            )
                        ) {
                            boot.usb_cable_at_start = true;
                            boot.usb_enabled = false;
                            boot.midi_enabled = false;
                        }
                        if matches!(
                            runner.scenario.update_fixture,
                            Some(
                                crate::scenario::UpdateFixture::PatchedKnob
                                    | crate::scenario::UpdateFixture::PatchedKnobMidi
                            )
                        ) {
                            boot.gdb_socket = Some(crate::devices::gdb_socket_path(
                                &options.logs,
                                "post-update-knob",
                            ));
                        }
                        let main = std::fs::read(boot.firmware_dir.join("main_firmware.bin"))
                            .map_err(|e| e.to_string())?;
                        if !l6_verify && crate::devices::selected_main_image(&boot)? != main {
                            return Err("installed MAIN differs from frozen verify firmware".into());
                        }
                        let mut verify_scenario = runner.scenario.clone();
                        let start = verify_scenario
                            .step
                            .iter()
                            .position(|step| {
                                matches!(
                                    step.action,
                                    crate::Action::LifecycleStart {
                                        profile: LifecycleBoot::Verify
                                    }
                                )
                            })
                            .ok_or("missing verify profile")?;
                        verify_scenario.step.drain(..=start);
                        crate::requirements::check_firmware(
                            &verify_scenario,
                            if l6_verify {
                                l6fw::Model::L6
                            } else {
                                l6fw::Model::L6max
                            },
                            &crate::firmware::identify(&main, &boot.firmware_dir),
                        )?;
                        if l6_verify {
                            verify_scenario.update_fixture = None;
                            verify_scenario.update_goal = None;
                            let report_directory = boot.firmware_dir.clone();
                            crate::devices::prepare_with_report(
                                &verify_scenario,
                                &mut boot,
                                Some(&report_directory),
                            )?;
                        }
                    }
                    let label = match profile {
                        LifecycleBoot::Initial => "initial",
                        LifecycleBoot::Installer => "installer",
                        LifecycleBoot::Restart => "restart",
                        LifecycleBoot::Verify => "verify",
                    };
                    boot.logs = options.logs.join(format!("boot-{label}"));
                    session = Some(Session::start(&boot)?);
                    lifecycle_boot_options = Some(boot.clone());
                    installer_anchor = if profile == LifecycleBoot::Installer {
                        let started = Instant::now();
                        let current = session.as_ref().unwrap();
                        let guest = l6ctl::OwnedControl::new(&current.engine)
                            .input_status(l6ctl::InputSource::Panel)
                            .map_err(|e| e.to_string())?
                            .guest_ms;
                        Some((started, guest))
                    } else {
                        None
                    };
                    lifecycle_last_boot = Some(profile);
                    runner.restarted(&boot.logs);
                    runner.device_finished(json!({"started":true,"profile":label,"card": !boot.no_sd && boot.sd_image.is_some(),"replacement_installer":boot.update_bootloader}));
                }
                Progress::Device(devices::Operation::LifecycleStop) => {
                    if device_state.has_sound_pad()
                        || device_state.has_l6_recorder()
                        || device_state.has_l6max_recorder()
                        || device_state.has_l6max_timing()
                        || device_state.has_usb_storage()
                        || device_state.has_ui_pad()
                        || device_state.has_editor_playback()
                        || device_state.has_editor_assignment()
                        || device_state.has_l6_sweep()
                        || device_state.has_signal_pwm()
                    {
                        return Err("device transaction interrupted by guest shutdown".into());
                    }
                    device_state.restarted()?;
                    let mut current = session
                        .take()
                        .ok_or("lifecycle stop requires an active guest")?;
                    current.engine.shutdown_checked()?;
                    drop(current);
                    lifecycle_boot_options = None;
                    runner.device_finished(json!({"stopped":true,"clean_shutdown":true}));
                }
                Progress::Device(devices::Operation::ExpectUpdateMarkerAbsent) => {
                    if session.is_some() {
                        return Err("update marker requires a stopped guest".into());
                    }
                    let state = options
                        .state_dir
                        .as_ref()
                        .ok_or("update marker requires persistent state")?;
                    let nor = l6nor::MainNor::read(&state.join("main-nor.bin"))?;
                    let marker = nor.update_marker();
                    if marker != l6nor::nor::UpdateMarker::None {
                        return Err(format!(
                            "corrupt package triggered update marker: {marker:?}"
                        ));
                    }
                    use sha2::{Digest, Sha256};
                    runner.device_finished(json!({"update_marker":"none", "main_nor_sha256":format!("{:x}", Sha256::digest(nor.bytes()))}));
                }
                Progress::Device(devices::Operation::ExpectLiveUpdateMarkerAbsent) => {
                    if session.is_none() {
                        return Err("live update marker requires an active guest".into());
                    }
                    let state = options
                        .state_dir
                        .as_ref()
                        .ok_or("update marker requires persistent state")?;
                    let nor = l6nor::MainNor::read(&state.join("main-nor.bin"))?;
                    if nor.update_marker() != l6nor::nor::UpdateMarker::None {
                        return Err("update marker set before power handoff".into());
                    }
                    runner.device_finished(json!({"update_marker":"none", "atomic":false}));
                }
                Progress::Device(devices::Operation::MarkL6UpdatePending) => {
                    if session.is_some()
                        || !runner
                            .scenario
                            .update_fixture
                            .is_some_and(crate::scenario::UpdateFixture::is_l6)
                    {
                        return Err(
                            "L6 marker mutation requires the stopped L6 stock lifecycle".into()
                        );
                    }
                    let path = options
                        .state_dir
                        .as_ref()
                        .ok_or("marker requires persistent state")?
                        .join("main-nor.bin");
                    let mut nor = l6nor::MainNor::read(&path)?;
                    l6nor::update::mark_l6_pending(&mut nor)?;
                    l6nor::state::write_device_file(&path, nor.bytes())?;
                    if l6nor::MainNor::read(&path)?.update_marker()
                        != l6nor::nor::UpdateMarker::Pending
                    {
                        return Err("pending marker was not persisted".into());
                    }
                    runner.device_finished(
                        json!({"update_marker":"pending","source":"explicit-offline-mutation"}),
                    );
                }
                Progress::Device(devices::Operation::ExpectL6UpdateResetVector) => {
                    let current = session
                        .as_ref()
                        .ok_or("reset vector check needs active guest")?;
                    if current.engine.model() != l6fw::Model::L6 {
                        return Err("reset vector check requires L6".into());
                    }
                    let package = managed_package
                        .as_deref()
                        .ok_or("missing frozen update package")?;
                    let (_, segments) = l6fw::unpack_bytes(package).map_err(|e| e.to_string())?;
                    let main = segments
                        .get("main_firmware.bin")
                        .ok_or("package lacks MAIN")?;
                    let expected = main.get(4..8).ok_or("package MAIN lacks reset vector")?;
                    let address = l6fw::Model::L6.main_base() + 4;
                    let sample = l6ctl::OwnedControl::new(&current.engine)
                        .read_main_memory(address, 4)
                        .map_err(|e| e.to_string())?;
                    if sample.bytes != expected {
                        return Err(format!(
                            "installed reset vector differs from package: {:02x?} != {expected:02x?}",
                            sample.bytes
                        ));
                    }
                    runner.device_finished(
                        json!({"address":address,"bytes":sample.bytes,"atomic":sample.atomic}),
                    );
                }
                Progress::Device(devices::Operation::ExpectL6UpdateComparison { field }) => {
                    if session.is_some()
                        || !runner
                            .scenario
                            .update_fixture
                            .is_some_and(crate::scenario::UpdateFixture::is_l6)
                    {
                        return Err("L6 comparison requires a stopped L6 stock lifecycle".into());
                    }
                    let state = options
                        .state_dir
                        .as_ref()
                        .ok_or("L6 comparison needs persistent state")?;
                    let frozen = pending_snapshot
                        .as_deref()
                        .ok_or("missing frozen pending NOR snapshot")?;
                    verify_frozen_snapshot(&options.logs.join("pending-main-nor.bin"), frozen)?;
                    let pending = l6nor::MainNor::from_bytes(frozen.to_vec())?;
                    let installed = l6nor::MainNor::read(&state.join("main-nor.bin"))?;
                    let resource =
                        std::fs::read(state.join("resource-nor.bin")).map_err(|e| e.to_string())?;
                    let comparison = l6nor::update::compare_l6(
                        &pending,
                        &installed,
                        &resource,
                        managed_package
                            .as_deref()
                            .ok_or("missing frozen update package")?,
                    )?;
                    use crate::scenario::L6UpdateField::*;
                    let matched = match field {
                        MainFirmware => comparison.segment_matches["main_firmware.bin"],
                        MainTrailer => comparison.segment_matches["main_trailer.bin"],
                        SecondaryFirmware => comparison.segment_matches["secondary_firmware.bin"],
                        SecondaryTrailer => comparison.segment_matches["secondary_trailer.bin"],
                        PackageFooter => comparison.segment_matches["package_footer.bin"],
                        BootRegion => comparison.boot_region_preserved,
                        ProtectedRegion => comparison.protected_region_preserved,
                        ResourceNor => comparison.resource_nor_matches,
                    };
                    if !matched {
                        return Err(format!(
                            "L6 installed update comparison failed for {field:?}"
                        ));
                    }
                    runner.device_finished(json!({"field":format!("{field:?}"),"matches":true}));
                }
                Progress::Device(devices::Operation::CaptureUpdateMarkerPending) => {
                    if session.is_some() {
                        return Err("pending marker requires a stopped guest".into());
                    }
                    let state = options
                        .state_dir
                        .as_ref()
                        .ok_or("update marker requires persistent state")?;
                    let nor = l6nor::MainNor::read(&state.join("main-nor.bin"))?;
                    let marker = nor.update_marker();
                    if marker == l6nor::nor::UpdateMarker::None
                        || (runner
                            .scenario
                            .update_fixture
                            .is_some_and(crate::scenario::UpdateFixture::is_l6)
                            && marker != l6nor::nor::UpdateMarker::Pending)
                    {
                        return Err("power handoff did not save a valid update marker".into());
                    }
                    let path = options.logs.join("pending-main-nor.bin");
                    use std::io::Write;
                    let mut file = std::fs::OpenOptions::new()
                        .write(true)
                        .create_new(true)
                        .open(&path)
                        .map_err(|e| e.to_string())?;
                    file.write_all(nor.bytes())
                        .and_then(|()| file.sync_all())
                        .map_err(|e| e.to_string())?;
                    pending_snapshot = Some(nor.bytes().to_vec());
                    use sha2::{Digest, Sha256};
                    runner.device_finished(json!({"update_marker":format!("{marker:?}"), "main_nor_sha256":format!("{:x}", Sha256::digest(nor.bytes())), "snapshot":"pending-main-nor.bin"}));
                }
                Progress::Device(devices::Operation::WaitPanelUptime { guest_ms }) => {
                    let current = session
                        .as_ref()
                        .ok_or("panel uptime requires an active guest")?;
                    let step = &runner.scenario.step[runner.current_step()];
                    let observed = wait_panel_uptime(current, guest_ms, step.wall_timeout_s)?;
                    runner.device_finished(
                        json!({"panel_uptime_ms":observed, "required_panel_uptime_ms":guest_ms}),
                    );
                }
                Progress::Device(devices::Operation::ExpectUpdateState {
                    mode,
                    flags_mask,
                    flags_value,
                }) => {
                    let current = session
                        .as_ref()
                        .ok_or("update state requires an active guest")?;
                    let sample = l6ctl::OwnedControl::new(&current.engine)
                        .update_state()
                        .map_err(|e| e.to_string())?;
                    if sample.mode != mode || sample.flags & flags_mask != flags_value {
                        return Err(format!(
                            "update state mismatch: mode={}, flags={:#x}; expected mode={mode}, flags & {flags_mask:#x}={flags_value:#x}",
                            sample.mode, sample.flags
                        ));
                    }
                    runner.device_finished(json!({"mode":sample.mode,"flags":sample.flags,"revision_guard":sample.revision_guard,"atomic":sample.atomic}));
                }
                Progress::Device(devices::Operation::ExpectUpdateGui {
                    window,
                    progress_mask,
                    progress_value,
                }) => {
                    let current = session
                        .as_ref()
                        .ok_or("update GUI requires an active guest")?;
                    let sample = l6ctl::OwnedControl::new(&current.engine)
                        .update_gui_state()
                        .map_err(|e| e.to_string())?;
                    if sample.current_window != window
                        || sample.progress_flags & progress_mask != progress_value
                    {
                        return Err(format!(
                            "update GUI mismatch: window={:#x}, progress={:#x}",
                            sample.current_window, sample.progress_flags
                        ));
                    }
                    runner.device_finished(json!({"window":sample.current_window,"progress_flags":sample.progress_flags,"atomic":sample.atomic,"revision_guard":sample.revision_guard}));
                }
                Progress::Device(devices::Operation::ExpectInstallerOutcome { expected }) => {
                    let current = session
                        .as_ref()
                        .ok_or("installer outcome requires an active guest")?;
                    let actual = l6ctl::OwnedControl::new(&current.engine)
                        .installer_outcome()
                        .map_err(|e| e.to_string())?;
                    let matches = matches!(
                        (expected, &actual),
                        (
                            crate::scenario::InstallerExpected::Installed,
                            l6ctl::InstallerOutcome::Installed
                        ) | (
                            crate::scenario::InstallerExpected::NoPending,
                            l6ctl::InstallerOutcome::NoPending
                        )
                    );
                    if !matches {
                        return Err(format!(
                            "replacement installer outcome mismatch: expected {expected:?}, got {actual:?}"
                        ));
                    }
                    runner.device_finished(
                        json!({"replacement_installer_outcome":format!("{actual:?}")}),
                    );
                }
                Progress::Device(devices::Operation::WaitUsbController) => {
                    let current = session
                        .as_ref()
                        .ok_or("USB readiness requires an active guest")?;
                    let control = l6ctl::OwnedControl::new(&current.engine);
                    control.usb_attach().map_err(|e| e.to_string())?;
                    let step = &runner.scenario.step[runner.current_step()];
                    let start = control
                        .input_status(l6ctl::InputSource::Panel)
                        .map_err(|e| e.to_string())?
                        .guest_ms;
                    let deadline = Instant::now() + Duration::from_secs(step.wall_timeout_s);
                    loop {
                        if control.usb_controller().map_err(|e| e.to_string())?.running {
                            break;
                        }
                        let guest = control
                            .input_status(l6ctl::InputSource::Panel)
                            .map_err(|e| e.to_string())?
                            .guest_ms;
                        if guest.wrapping_sub(start) >= step.guest_timeout_ms
                            || Instant::now() >= deadline
                        {
                            return Err("USB device controller did not start".into());
                        }
                        current.poll()?;
                        std::thread::sleep(Duration::from_millis(25));
                    }
                    runner.device_finished(json!({"attached":true,"running":true}));
                }
                Progress::Device(devices::Operation::WaitUpdatePanelReady {
                    require_programming,
                }) => {
                    let current = session
                        .as_ref()
                        .ok_or("panel image wait requires an active guest")?;
                    let state = options
                        .state_dir
                        .as_deref()
                        .ok_or("panel image needs persistent state")?;
                    let package = managed_package
                        .as_deref()
                        .ok_or("missing frozen update package")?;
                    let step = &runner.scenario.step[runner.current_step()];
                    wait_update_panel_ready(
                        current,
                        state,
                        package,
                        &options.logs.join("boot-installer/engine.trace"),
                        installer_anchor.ok_or("panel readiness requires an installer boot")?,
                        step.guest_timeout_ms,
                        step.wall_timeout_s,
                        require_programming,
                    )?;
                    runner.device_finished(json!({"panel_image_matches":true,"require_programming":require_programming,"atomic":false}));
                }
                Progress::Device(devices::Operation::ExpectUpdateMode { mode }) => {
                    let current = session
                        .as_ref()
                        .ok_or("update mode requires an active guest")?;
                    let sample = l6ctl::OwnedControl::new(&current.engine)
                        .update_state()
                        .map_err(|e| e.to_string())?;
                    if sample.mode != mode {
                        return Err(format!(
                            "update mode mismatch: expected {mode}, got {}",
                            sample.mode
                        ));
                    }
                    runner.device_finished(json!({"mode":sample.mode,"revision_guard":sample.revision_guard,"atomic":sample.atomic}));
                }
                Progress::Device(devices::Operation::ExpectMainMemory { address, bytes }) => {
                    let current = session
                        .as_ref()
                        .ok_or("memory observation requires an active guest")?;
                    let sample = l6ctl::OwnedControl::new(&current.engine)
                        .read_main_memory(address, bytes.len())
                        .map_err(|e| e.to_string())?;
                    if sample.bytes != bytes {
                        return Err(format!(
                            "MAIN memory mismatch at {address:#x}: expected {bytes:02x?}, got {:02x?}",
                            sample.bytes
                        ));
                    }
                    runner.device_finished(
                        json!({"address":address,"bytes":sample.bytes,"atomic":sample.atomic}),
                    );
                }
                Progress::Device(devices::Operation::ExpectUpdateComparison { field }) => {
                    if session.is_some() {
                        return Err("installed update comparison requires stopped guest".into());
                    }
                    let state = options
                        .state_dir
                        .as_ref()
                        .ok_or("installed update needs persistent state")?;
                    let frozen = pending_snapshot
                        .as_deref()
                        .ok_or("missing frozen pending NOR snapshot")?;
                    verify_frozen_snapshot(&options.logs.join("pending-main-nor.bin"), frozen)?;
                    let pending = l6nor::MainNor::from_bytes(frozen.to_vec())?;
                    let installed = l6nor::MainNor::read(&state.join("main-nor.bin"))?;
                    let panel =
                        std::fs::read(state.join("panel-flash.bin")).map_err(|e| e.to_string())?;
                    let comparison = l6nor::update::compare_l6max(
                        &pending,
                        &installed,
                        &panel,
                        managed_package
                            .as_deref()
                            .ok_or("missing frozen update package")?,
                    )?;
                    let matched = match field {
                        crate::scenario::UpdateComparisonField::MainPayload => {
                            comparison.main_payload_matches
                        }
                        crate::scenario::UpdateComparisonField::BootRegion => {
                            comparison.boot_region_preserved
                        }
                        crate::scenario::UpdateComparisonField::ProtectedRegion => {
                            comparison.protected_region_preserved
                        }
                        crate::scenario::UpdateComparisonField::PanelImage => {
                            comparison.panel_image_matches
                        }
                    };
                    if !matched {
                        return Err(format!("installed update comparison failed for {field:?}"));
                    }
                    runner.device_finished(json!({"field":format!("{field:?}"),"matches":true}));
                }
                Progress::Device(devices::Operation::CaptureInstalledPanel) => {
                    if session.is_some() {
                        return Err("panel snapshot requires stopped guest".into());
                    }
                    let state = options
                        .state_dir
                        .as_ref()
                        .ok_or("panel snapshot needs persistent state")?;
                    let panel =
                        std::fs::read(state.join("panel-flash.bin")).map_err(|e| e.to_string())?;
                    let mut file = std::fs::OpenOptions::new()
                        .write(true)
                        .create_new(true)
                        .open(options.logs.join("installed-panel-flash.bin"))
                        .map_err(|e| e.to_string())?;
                    use std::io::Write;
                    file.write_all(&panel)
                        .and_then(|()| file.sync_all())
                        .map_err(|e| e.to_string())?;
                    installed_panel_snapshot = Some(panel.clone());
                    use sha2::{Digest, Sha256};
                    runner.device_finished(json!({"panel_sha256":format!("{:x}",Sha256::digest(&panel)),"snapshot":"installed-panel-flash.bin"}));
                }
                Progress::Device(devices::Operation::ExpectPanelUnchanged) => {
                    if session.is_some() {
                        return Err("panel comparison requires stopped guest".into());
                    }
                    let state = options
                        .state_dir
                        .as_ref()
                        .ok_or("panel comparison needs persistent state")?;
                    let before = installed_panel_snapshot
                        .as_deref()
                        .ok_or("missing frozen installed panel snapshot")?;
                    verify_frozen_snapshot(
                        &options.logs.join("installed-panel-flash.bin"),
                        before,
                    )?;
                    let after =
                        std::fs::read(state.join("panel-flash.bin")).map_err(|e| e.to_string())?;
                    if before != after.as_slice() {
                        return Err("panel flash changed across restart".into());
                    }
                    runner.device_finished(json!({"panel_unchanged":true}));
                }
                Progress::Device(devices::Operation::Restart) => {
                    if options.state_dir.is_none() {
                        return Err("restart requires isolated persistent state".into());
                    }
                    if device_state.has_sound_pad()
                        || device_state.has_l6_recorder()
                        || device_state.has_l6max_recorder()
                        || device_state.has_l6max_timing()
                        || device_state.has_usb_storage()
                        || device_state.has_ui_pad()
                        || device_state.has_editor_playback()
                        || device_state.has_editor_assignment()
                        || device_state.has_l6_sweep()
                        || device_state.has_signal_pwm()
                    {
                        return Err("device transaction interrupted by guest shutdown".into());
                    }
                    device_state.restarted()?;
                    drop(session.take());
                    if options.fixture_firmware_report.is_none() {
                        options.fixture_firmware_report =
                            Some(options.logs.join(crate::fixture_layout::FIRMWARE_REPORT));
                    }
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
                    let warm_reset_panel =
                        matches!(operation, devices::Operation::WarmReset).then(|| {
                            session
                                .as_ref()
                                .unwrap()
                                .engine
                                .inputs
                                .as_ref()
                                .unwrap()
                                .panel
                                .snapshot()
                        });
                    let card_change = match &operation {
                        devices::Operation::SdCard { inserted } => Some(*inserted),
                        _ => None,
                    };
                    device_state
                        .set_paced_progress(case_progress.clone(), runner.current_display_index());
                    let step = &runner.scenario.step[runner.current_step()];
                    device_state.set_adaptive_progress(
                        case_progress.clone(),
                        runner.current_progress_index(),
                    );
                    let evidence = match device_state.execute(
                        session.as_ref().unwrap(),
                        device_options(managed, &options, lifecycle_boot_options.as_ref())?,
                        operation,
                        step.guest_timeout_ms,
                        step.wall_timeout_s,
                    ) {
                        Ok(evidence) => evidence,
                        Err(error) => {
                            if device_state.take_deferred_terminal_failure() {
                                runner.mark_deferred_terminal();
                            }
                            return Err(error);
                        }
                    };
                    if let Some(error) = device_state.take_deferred_assertion() {
                        runner.defer_assertion_failure(error);
                    }
                    if let Some(inserted) = card_change {
                        card_inserted = inserted;
                    }
                    if let Some(panel) = warm_reset_panel {
                        let barrier = session
                            .as_ref()
                            .unwrap()
                            .engine
                            .inputs
                            .as_ref()
                            .unwrap()
                            .panel
                            .snapshot();
                        runner.warm_reset(panel.indicators, barrier.indicator_updates);
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
    if result.is_ok() && device_state.has_signal_pwm() {
        result = Err("SIGNAL PWM ended before completion".into());
    }
    if result.is_ok() && device_state.has_l6_sweep() {
        result = Err("L6 input sweep ended before completion".into());
    }
    if device_state.has_l6_sweep() {
        let cleanup = match session.as_ref() {
            Some(current) => device_state.abort_l6_sweep(current),
            None => Err("no active guest to release L6 panel input".to_owned()),
        };
        if let Err(cleanup) = cleanup {
            result = Err(match result {
                Ok(()) => format!("L6 sweep cleanup failed: {cleanup}"),
                Err(error) => format!("{error}; L6 sweep cleanup failed: {cleanup}"),
            });
        }
    }
    if result.is_ok() && device_state.has_editor_assignment() {
        result = Err("Editor assignment ended before completion".into());
    }
    if result.is_ok() && device_state.has_editor_playback() {
        result = Err("Editor playback ended before completion".into());
    }
    if result.is_ok() && device_state.has_ui_pad() {
        result = Err("UI pad verification ended before completion".into());
    }
    if result.is_ok() && device_state.has_usb_storage() {
        result = Err("USB storage ended before completion".into());
    }
    if device_state.has_usb_storage() {
        if let Err(cleanup) = device_state.abort_usb_storage() {
            result = Err(match result {
                Ok(()) => format!("USB storage cleanup failed: {cleanup}"),
                Err(e) => format!("{e}; USB storage cleanup failed: {cleanup}"),
            })
        }
    }
    if result.is_ok() && device_state.has_l6max_timing() {
        result = Err("L6max timing ended before completion".into());
    }
    if device_state.has_l6max_timing() {
        let timing_cleanup = match session.as_ref() {
            Some(current) => device_state.abort_l6max_timing(current),
            None => Err("no active guest for timing cleanup".into()),
        };
        if let Err(cleanup) = timing_cleanup {
            result = Err(match result {
                Ok(()) => format!("timing cleanup failed: {cleanup}"),
                Err(e) => format!("{e}; timing cleanup failed: {cleanup}"),
            })
        }
    }
    if result.is_ok() && device_state.has_l6max_recorder() {
        result = Err("L6max recorder transaction ended before completion".into());
    }
    if device_state.has_l6max_recorder() {
        let cleanup = match session.as_ref() {
            Some(current) => device_state.abort_l6max_recorder(current),
            None => Err("no active guest for recorder cleanup".into()),
        };
        if let Err(cleanup) = cleanup {
            result = Err(match result {
                Ok(()) => format!("L6max recorder cleanup failed: {cleanup}"),
                Err(e) => format!("{e}; L6max recorder cleanup failed: {cleanup}"),
            })
        }
    }
    if result.is_ok() && device_state.has_l6_recorder() {
        result = Err("recorder transaction ended before completion".into());
    }
    if device_state.has_l6_recorder() {
        let cleanup = match session.as_ref() {
            Some(current) => device_state.abort_l6_recorder(current),
            None => Err("no active guest for recorder cleanup".into()),
        };
        if let Err(cleanup) = cleanup {
            result = Err(match result {
                Ok(()) => format!("recorder cleanup failed: {cleanup}"),
                Err(error) => format!("{error}; recorder cleanup failed: {cleanup}"),
            });
        }
    }
    if result.is_ok() && device_state.has_sound_pad() {
        result = Err("sound-pad transaction ended before completion".into());
    }
    if device_state.has_sound_pad() {
        let cleanup = match session.as_ref() {
            Some(current) => device_state.abort_sound_pad(current),
            None => Err("no active guest to release sound pad".to_owned()),
        };
        if let Err(cleanup) = cleanup {
            result = Err(match result {
                Ok(()) => format!("sound-pad cleanup failed: {cleanup}"),
                Err(error) => format!("{error}; sound-pad cleanup failed: {cleanup}"),
            });
        }
    }
    if let Some(command) = runner.take_held_release() {
        let cleanup = session
            .as_ref()
            .ok_or("no active guest for momentary release".to_owned())
            .and_then(|guest| guest.dispatch(&command));
        if let Err(cleanup) = cleanup {
            result = Err(match result {
                Ok(()) => format!("momentary cleanup failed: {cleanup}"),
                Err(error) => format!("{error}; momentary cleanup failed: {cleanup}"),
            });
        }
    }
    if let Err(cleanup) = device_state.abort_auxiliary_scopes() {
        result = Err(match result {
            Ok(()) => format!("device scope cleanup failed: {cleanup}"),
            Err(error) => format!("{error}; device scope cleanup failed: {cleanup}"),
        });
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

#[cfg(test)]
mod lifecycle_clock_tests {
    use super::*;
    #[test]
    fn lifecycle_device_actions_use_active_boot_card_and_firmware() {
        let mut case = Options::parse_args(std::iter::empty::<std::ffi::OsString>()).unwrap();
        case.sd_image = Some("update-card.img".into());
        case.firmware_dir = "stock".into();
        let mut verify = case.clone();
        verify.sd_image = None;
        verify.no_sd = true;
        verify.firmware_dir = "verify-firmware".into();
        let selected = device_options(true, &case, Some(&verify)).unwrap();
        assert!(selected.sd_image.is_none());
        assert_eq!(
            selected.firmware_dir,
            std::path::Path::new("verify-firmware")
        );
        assert!(device_options(true, &case, None).is_err());
        assert_eq!(
            device_options(false, &case, None).unwrap().sd_image,
            case.sd_image
        );
    }
    #[test]
    fn main_progress_keeps_a_held_panel_alive_but_two_stopped_clocks_fail() {
        let start = Instant::now();
        let mut watch = DualClockWatch::new(start);
        assert!(!watch.stalled(0, 100, start + Duration::from_secs(59)));
        assert!(!watch.stalled(0, 200, start + Duration::from_secs(118)));
        assert!(!watch.stalled(0, 200, start + Duration::from_secs(177)));
        assert!(watch.stalled(0, 200, start + Duration::from_secs(178)));
    }

    #[test]
    fn shared_wall_deadline_spans_several_steps() {
        let start = Instant::now();
        let budget = WallBudget::new(start, 300).unwrap();
        assert!(budget.check(start + Duration::from_secs(100)).is_ok());
        assert!(budget.check(start + Duration::from_secs(200)).is_ok());
        assert!(budget.check(start + Duration::from_secs(300)).is_err());
        let mut dispatched = false;
        let mut delayed_tick = || -> Result<(), String> {
            budget.check(start + Duration::from_secs(301))?;
            dispatched = true;
            Ok(())
        };
        assert!(delayed_tick().is_err());
        assert!(!dispatched);
        assert!(WallBudget::new(start, 0).is_err());
    }

    #[test]
    fn panel_readiness_requires_all_simultaneous_conditions() {
        assert!(panel_update_ready(4, true, true, 64, true, true));
        assert!(!panel_update_ready(3, true, true, 64, true, true));
        assert!(!panel_update_ready(4, false, true, 64, true, true));
        assert!(!panel_update_ready(4, true, false, 64, true, true));
        assert!(!panel_update_ready(4, true, true, 63, true, true));
        assert!(!panel_update_ready(4, true, true, 65, true, true));
        assert!(!panel_update_ready(4, true, true, 64, false, true));
        assert!(panel_update_ready(4, true, true, 0, false, false));
        assert!(!panel_update_ready(3, true, true, 0, false, false));
        assert!(!panel_update_ready(4, false, true, 0, false, false));
        assert!(!panel_update_ready(4, true, false, 0, false, false));
    }

    #[test]
    fn patched_artifacts_reject_changed_report_and_firmware() {
        use sha2::{Digest, Sha256};
        let root =
            std::env::temp_dir().join(format!("l6-patched-artifacts-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let verify = root.join("verify-firmware");
        std::fs::create_dir_all(&verify).unwrap();
        let report = b"report";
        let main = b"main";
        std::fs::write(root.join("patched-update.json"), report).unwrap();
        std::fs::write(verify.join(crate::fixture_layout::FIRMWARE_REPORT), report).unwrap();
        std::fs::write(verify.join("main_firmware.bin"), main).unwrap();
        let hash = |bytes: &[u8]| format!("{:x}", Sha256::digest(bytes));
        let inputs = json!({"update_report_sha256":hash(report),"verify_firmware":{
            crate::fixture_layout::FIRMWARE_REPORT:hash(report), "main_firmware.bin":hash(main)
        }});
        assert!(verify_patched_artifacts(&root, &inputs).is_ok());
        std::fs::write(verify.join("main_firmware.bin"), b"different").unwrap();
        assert!(verify_patched_artifacts(&root, &inputs).is_err());
        std::fs::write(verify.join("main_firmware.bin"), main).unwrap();
        std::fs::write(root.join("patched-update.json"), b"different").unwrap();
        assert!(verify_patched_artifacts(&root, &inputs).is_err());
        std::fs::write(root.join("patched-update.json"), report).unwrap();
        std::fs::write(verify.join("unexpected.bin"), b"extra").unwrap();
        assert!(verify_patched_artifacts(&root, &inputs).is_err());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn frozen_snapshot_rejects_changed_or_missing_artifact() {
        let path = std::env::temp_dir().join(format!("l6-frozen-snapshot-{}", std::process::id()));
        let _ = std::fs::remove_file(&path);
        assert!(verify_frozen_snapshot(&path, b"before").is_err());
        std::fs::write(&path, b"before").unwrap();
        assert!(verify_frozen_snapshot(&path, b"before").is_ok());
        std::fs::write(&path, b"changed").unwrap();
        assert!(verify_frozen_snapshot(&path, b"before").is_err());
        std::fs::remove_file(path).unwrap();
    }
}
