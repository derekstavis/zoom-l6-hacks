//! The knob-dialog patch: its window and popup.
use super::*;
use l6max_host::debug_journal::{Cleanup as DebugCleanup, Journal, cleanup as cleanup_debugger};
mod adaptive_demo;
pub(super) use adaptive_demo::{
    run as run_adaptive_demo, run_level_demo_frame, run_level_expect_zero, run_level_sweep,
};

/// Reviewed knob-dialog window images, from the repository root. The
/// knob-dialog window check compares the display with these by scene name.
pub const KNOB_DIALOG_REFERENCES: &str = "patches/knob-dialog/tests/emulator/references";

/// Owns the debugger and records intrusive mutations for failure cleanup.
/// Successful runs keep guest-side changes made deliberately by the probes.
struct KnobDebugSession {
    qmp: l6max_host::qmp::QmpClient,
    gdb: l6max_diagnostics::gdb::Gdb,
    journal: Journal,
    finalized: bool,
}

impl KnobDebugSession {
    fn connect(qmp: &l6max_host::qmp::QmpClient, path: &std::path::Path) -> std::io::Result<Self> {
        Ok(Self {
            qmp: qmp.clone(),
            gdb: l6max_diagnostics::gdb::Gdb::connect(path)?,
            journal: Journal::default(),
            finalized: false,
        })
    }
    fn write(&mut self, address: u32, data: &[u8]) -> std::io::Result<()> {
        let before = self.qmp.read_memory(0, address, data.len())?;
        // Journal before sending: a failed request can still have applied.
        let restoring = self.journal.begin_write(address, before, data)?;
        let result = self.gdb.write(address, data);
        if result.is_err() {
            self.journal.synchronized = false;
        }
        result?;
        self.journal.commit_write(restoring);
        Ok(())
    }
    fn write_before(
        &mut self,
        address: u32,
        data: &[u8],
        deadline: Instant,
        mut check: impl FnMut() -> std::io::Result<()>,
    ) -> std::io::Result<()> {
        check()?;
        let before = self
            .qmp
            .read_memory_before(0, address, data.len(), deadline)?;
        check()?;
        let restoring = self.journal.begin_write(address, before, data)?;
        let result = self.gdb.write_before(address, data, deadline);
        if result.is_err() {
            self.journal.synchronized = false;
        }
        result?;
        check()?;
        self.journal.commit_write(restoring);
        Ok(())
    }
    fn read_register_before(&mut self, index: u32, deadline: Instant) -> std::io::Result<u32> {
        let result = self.gdb.read_register_before(index, deadline);
        if result.is_err() {
            self.journal.synchronized = false;
        }
        result
    }
    fn handoff_mailbox(&mut self, address: u32) {
        self.journal.handoff_mailbox(address);
    }
    fn acknowledge_mailbox(&mut self) {
        self.journal.acknowledge_mailbox();
    }
    fn resume_guest(&mut self) -> std::io::Result<()> {
        if !self.journal.synchronized {
            return Err(std::io::Error::other(
                "debugger transport not synchronized; guest left stopped",
            ));
        }
        // Even a failed request may resume, invalidating saved register values.
        self.journal.registers.clear();
        let result = self.qmp.resume();
        if result.is_err() {
            self.journal.synchronized = false;
        }
        result
    }
    fn resume_guest_before(&mut self, deadline: Instant) -> std::io::Result<()> {
        if !self.journal.synchronized {
            return Err(std::io::Error::other(
                "debugger transport not synchronized; guest left stopped",
            ));
        }
        self.journal.registers.clear();
        let result = self.qmp.resume_before(deadline);
        if result.is_err() {
            self.journal.synchronized = false;
        }
        result
    }
    fn breakpoint(&mut self, address: u32, enable: bool) -> std::io::Result<()> {
        if enable && !self.journal.breakpoints.contains(&address) {
            self.journal.breakpoints.push(address);
        }
        let result = self.gdb.breakpoint(address, enable);
        if result.is_err() {
            self.journal.synchronized = false;
        }
        result?;
        if !enable {
            self.journal.breakpoints.retain(|&item| item != address);
        }
        Ok(())
    }
    fn breakpoint_before(
        &mut self,
        address: u32,
        enable: bool,
        deadline: Instant,
    ) -> std::io::Result<()> {
        if enable && !self.journal.breakpoints.contains(&address) {
            self.journal.breakpoints.push(address);
        }
        let result = self.gdb.breakpoint_before(address, enable, deadline);
        if result.is_err() {
            self.journal.synchronized = false;
        }
        result?;
        if !enable {
            self.journal.breakpoints.retain(|&item| item != address);
        }
        Ok(())
    }
    fn read_register(&mut self, index: u32) -> std::io::Result<u32> {
        let result = self.gdb.read_register(index);
        if result.is_err() {
            self.journal.synchronized = false;
        }
        result
    }
    fn continue_until_stop(&mut self, timeout: Duration) -> std::io::Result<String> {
        self.journal.registers.clear();
        let result = self.gdb.continue_until_stop(timeout);
        if result.is_err() {
            self.journal.synchronized = false;
        }
        result
    }
    fn call_thumb(
        &mut self,
        address: u32,
        arguments: &[u32],
        timeout: Duration,
    ) -> std::io::Result<String> {
        self.journal.registers.clear();
        let result = self.gdb.call_thumb(address, arguments, timeout);
        if result.is_err() {
            self.journal.synchronized = false;
        }
        result
    }
    fn finish<T>(
        mut self,
        result: Result<T, Box<dyn std::error::Error>>,
    ) -> Result<T, Box<dyn std::error::Error>> {
        match result {
            Ok(value) if self.journal.clean() => {
                self.finalized = true;
                Ok(value)
            }
            Ok(_) => {
                self.finalized = true;
                let failures = cleanup_debugger(&mut self);
                Err(format!(
                    "debugger session ended with active mutations or breakpoints; cleanup: {}",
                    if failures.is_empty() {
                        "completed".to_owned()
                    } else {
                        failures.join("; ")
                    }
                )
                .into())
            }
            Err(error) => {
                self.finalized = true;
                let failures = cleanup_debugger(&mut self);
                if failures.is_empty() {
                    Err(error)
                } else {
                    Err(format!("{error}; debugger cleanup failed: {}", failures.join("; ")).into())
                }
            }
        }
    }
}

impl Drop for KnobDebugSession {
    fn drop(&mut self) {
        if !self.finalized {
            let errors = cleanup_debugger(self);
            if !errors.is_empty() {
                tracing::error!(
                    "abandoned knob-dialog debugger cleanup failed: {}",
                    errors.join("; ")
                );
            }
        }
    }
}

impl DebugCleanup for KnobDebugSession {
    fn stop(&mut self) -> Result<(), String> {
        self.qmp.stop().map_err(|error| error.to_string())
    }
    fn synchronized(&self) -> bool {
        self.journal.synchronized && !self.journal.mailbox_pending
    }
    fn restore_writes(&mut self) -> Vec<String> {
        let mut failures = Vec::new();
        for (address, before) in self.journal.writes.drain(..).rev() {
            if let Err(error) = self.gdb.write(address, &before) {
                failures.push(format!("restore {address:#010x}: {error}"));
                break;
            }
        }
        failures
    }
    fn clear_breakpoints(&mut self) -> Vec<String> {
        let mut failures = Vec::new();
        for address in self.journal.breakpoints.drain(..).rev() {
            if let Err(error) = self.gdb.breakpoint(address, false) {
                failures.push(format!("remove breakpoint {address:#010x}: {error}"));
                break;
            }
        }
        failures
    }
    fn restore_registers(&mut self) -> Vec<String> {
        let mut failures = Vec::new();
        for (index, before) in self.journal.registers.drain(..).rev() {
            if let Err(error) = self.gdb.register(index, before) {
                failures.push(format!("restore register {index}: {error}"));
                break;
            }
        }
        failures
    }
    fn resume(&mut self) -> Result<(), String> {
        self.qmp.resume().map_err(|error| error.to_string())
    }
}

/// Wait until the MAIN 1.10 mixer call and the knob-dialog post-hook return.
/// At this point a first-use guard has either rejected the turn or published
/// the pending callback, even when the mixer value was already at its limit.
fn remaining_debug_wait(
    shared: (Instant, u32, u32, u64, u16),
    guest_now: u32,
    wall_timeout_s: u64,
) -> Result<Duration, Box<dyn std::error::Error>> {
    let (start, guest_start, guest_budget, wall_budget, _) = shared;
    if super::execute::deadline_exceeded(
        start.elapsed(),
        guest_start,
        guest_now,
        guest_budget,
        wall_budget,
    ) {
        return Err("knob-dialog shared deadline exceeded during debugger operation".into());
    }
    let remaining = Duration::from_secs(wall_budget)
        .checked_sub(start.elapsed())
        .filter(|d| !d.is_zero())
        .ok_or("knob-dialog shared deadline exceeded during debugger operation")?;
    Ok(remaining.min(Duration::from_secs(wall_timeout_s.min(30))))
}

fn turn_and_wait_hook(
    session: &Session,
    debugger: &mut KnobDebugSession,
    delta: i32,
    wall_timeout_s: u64,
) -> Result<(), Box<dyn std::error::Error>> {
    turn_and_wait_hook_bounded(session, debugger, delta, wall_timeout_s, None, None)
}

fn turn_and_wait_hook_bounded(
    session: &Session,
    debugger: &mut KnobDebugSession,
    delta: i32,
    wall_timeout_s: u64,
    shared: Option<(Instant, u32, u32, u64, u16)>,
    expected_mailbox: Option<(&l6max_host::knob_popup::OverlayReader, u8)>,
) -> Result<(), Box<dyn std::error::Error>> {
    const MIXER_HOOK_RETURN: u32 = 0x8003_6c78;
    let qmp = &session.engine.management[0];
    let inputs = session.engine.inputs.as_ref().ok_or("missing inputs")?;
    if let Some(shared) = shared {
        remaining_debug_wait(shared, inputs.panel.snapshot().guest_ms, wall_timeout_s)?;
    }
    qmp.stop()?;
    if let Some((reader, _)) = expected_mailbox {
        let pending = if let Some(shared) = shared {
            let deadline = shared.0 + Duration::from_secs(shared.3);
            let check = || notification_observation_check(session, shared, wall_timeout_s);
            reader.mailbox_before_checked(deadline, &check)?
        } else {
            reader.mailbox()?
        };
        if pending != u8::MAX {
            return Err(
                format!("knob hook mailbox was not empty before input: {pending:#04x}").into(),
            );
        }
    }
    debugger.breakpoint(MIXER_HOOK_RETURN, true)?;
    // Recheck before enqueuing guest input if stopping or arming consumed the budget.
    if let Some(shared) = shared {
        remaining_debug_wait(shared, inputs.panel.snapshot().guest_ms, wall_timeout_s)?;
    }
    inputs.knob(0, delta)?;
    let timeout = match shared {
        Some(shared) => {
            remaining_debug_wait(shared, inputs.panel.snapshot().guest_ms, wall_timeout_s)?
        }
        None => Duration::from_secs(wall_timeout_s.min(30)),
    };
    // GDB continuation is synchronous. Its wall timeout is capped above;
    // guest time cannot be sampled until it returns, so check it immediately
    // afterward and fail closed if the shared guest budget was exceeded.
    debugger.continue_until_stop(timeout)?;
    if let Some(shared) = shared {
        remaining_debug_wait(shared, inputs.panel.snapshot().guest_ms, wall_timeout_s)?;
    }
    let pc = debugger.read_register(15)? & !1;
    debugger.breakpoint(MIXER_HOOK_RETURN, false)?;
    if pc != MIXER_HOOK_RETURN {
        return Err(format!("knob turn stopped at {pc:#x}, not the mixer hook return").into());
    }
    if let Some((reader, expected)) = expected_mailbox {
        let actual = if let Some(shared) = shared {
            let deadline = shared.0 + Duration::from_secs(shared.3);
            let check = || notification_observation_check(session, shared, wall_timeout_s);
            reader.mailbox_before_checked(deadline, &check)?
        } else {
            reader.mailbox()?
        };
        if actual != expected {
            return Err(format!(
                "knob hook published mailbox {actual:#04x}, expected {expected:#04x}"
            )
            .into());
        }
    }
    if let Some(shared) = shared {
        remaining_debug_wait(shared, inputs.panel.snapshot().guest_ms, wall_timeout_s)?;
    }
    debugger.resume_guest()?;
    Ok(())
}

fn recorder_controls_visible(display: &[u8; 1024], reference: &image::GrayImage) -> bool {
    (16..40).all(|y| {
        (0..128).all(|x| {
            let index = ((63 - y) / 8) * 128 + x;
            (display[index] & (1 << ((63 - y) % 8)) != 0)
                == (reference.get_pixel(x as u32, y as u32).0[0] >= 128)
        })
    })
}

pub(super) fn run_knob_dialog_ready(
    session: &Session,
    options: &Options,
    guest_timeout_ms: u32,
    wall_timeout_s: u64,
) -> Result<Value, Box<dyn std::error::Error>> {
    use std::time::Instant;
    let main = selected_main_image(options).map_err(std::io::Error::other)?;
    if main.get(0x40c..0x410) != Some(&0x8007eea9u32.to_le_bytes()) {
        return Err("patched startup hook does not reserve popup RAM".into());
    }
    let inputs = session.engine.inputs.as_ref().unwrap();
    const READY_GUEST_TIMEOUT_MS: u32 = 30_000;
    const READY_WALL_TIMEOUT_S: u64 = 180;
    const READY_POLL_MS: u64 = 5;
    let recorder_reference = image::open(
        l6max_host::paths::repository_root()
            .join("tests/emulator/references/l6max-recorder-home.png"),
    )?
    .to_luma8();
    if recorder_reference.dimensions() != (128, 64) {
        return Err("recorder reference must be 128×64".into());
    }
    let control = l6ctl::OwnedControl::new(&session.engine);
    let reader = control.knob_popup_reader()?;
    let start = inputs.panel.snapshot().guest_ms;
    let deadline = Instant::now() + Duration::from_secs(wall_timeout_s.min(READY_WALL_TIMEOUT_S));
    loop {
        let display = session.poll()?.display;
        let now = inputs.panel.snapshot().guest_ms;
        let observed = reader.recorder_state()?;
        let window = observed.window;
        let busy = observed.notification_busy;
        let active = observed.notification_active;
        if window == l6max::GUI_RECORDER_WINDOW
            && busy == 0
            && active == 0
            && recorder_controls_visible(&display, &recorder_reference)
        {
            break;
        }
        if now.wrapping_sub(start) >= guest_timeout_ms.min(READY_GUEST_TIMEOUT_MS)
            || Instant::now() >= deadline
        {
            return Err("recorder did not settle before knob-dialog checks".into());
        }
        thread::sleep(Duration::from_millis(READY_POLL_MS));
    }

    Ok(serde_json::json!({"recorder_ready": true}))
}

fn guard_rejected(state: &l6max_host::knob_popup::GuardState, window_before: u32) -> bool {
    state.state == 0 && state.callback == 0 && state.window == window_before
}

fn paused_observation<T>(
    stop: impl FnOnce() -> Result<(), String>,
    read: impl FnOnce() -> Result<T, String>,
    resume: impl FnOnce() -> Result<(), String>,
) -> Result<T, String> {
    stop()?;
    let result = read();
    let resumed = resume();
    match (result, resumed) {
        (Ok(value), Ok(())) => Ok(value),
        (Err(error), Ok(())) => Err(error),
        (Ok(_), Err(error)) => Err(format!("failed to resume QEMU: {error}")),
        (Err(error), Err(resume_error)) => {
            Err(format!("{error}; failed to resume QEMU: {resume_error}"))
        }
    }
}

pub(super) fn run_knob_dialog_guard(
    session: &Session,
    options: &Options,
    guest_timeout_ms: u32,
    wall_timeout_s: u64,
) -> Result<Value, Box<dyn std::error::Error>> {
    use std::time::Instant;
    let main = selected_main_image(options).map_err(std::io::Error::other)?;
    if main.get(0x40c..0x410) != Some(&0x8007eea9u32.to_le_bytes()) {
        return Err("patched startup hook does not reserve popup RAM".into());
    }
    let inputs = session.engine.inputs.as_ref().unwrap();
    let qmp = &session.engine.management[0];
    // The project title varies with the selected project. The recorder
    // transport controls in the middle of this reviewed image do not.
    const READY_GUEST_TIMEOUT_MS: u32 = 30_000;
    const READY_WALL_TIMEOUT_S: u64 = 180;
    const READY_POLL_MS: u64 = 5;
    let recorder_reference = image::open(
        l6max_host::paths::repository_root()
            .join("tests/emulator/references/l6max-recorder-home.png"),
    )?
    .to_luma8();
    if recorder_reference.dimensions() != (128, 64) {
        return Err("recorder reference must be 128×64".into());
    }
    let wait_recorder_controls = || -> Result<(), Box<dyn std::error::Error>> {
        let start = inputs.panel.snapshot().guest_ms;
        let deadline =
            Instant::now() + Duration::from_secs(wall_timeout_s.min(READY_WALL_TIMEOUT_S));
        loop {
            let display = session.poll()?.display;
            if recorder_controls_visible(&display, &recorder_reference) {
                return Ok(());
            }
            if inputs.panel.snapshot().guest_ms.wrapping_sub(start)
                >= guest_timeout_ms.min(READY_GUEST_TIMEOUT_MS)
                || Instant::now() >= deadline
            {
                return Err("recorder controls did not render before knob-dialog checks".into());
            }
            thread::sleep(Duration::from_millis(READY_POLL_MS));
        }
    };
    let word = |address: u32| -> Result<u32, Box<dyn std::error::Error>> {
        Ok(u32::from_le_bytes(
            qmp.read_memory(0, address, 4)?.try_into().unwrap(),
        ))
    };
    let wait_window = |recorder: bool| -> Result<u32, Box<dyn std::error::Error>> {
        let start = inputs.panel.snapshot().guest_ms;
        let deadline = Instant::now() + Duration::from_secs(wall_timeout_s.min(120));
        loop {
            session.poll()?;
            let window = word(l6max::current_window)?;
            if (window == l6max::GUI_RECORDER_WINDOW) == recorder {
                return Ok(window);
            }
            if inputs.panel.snapshot().guest_ms.wrapping_sub(start) >= guest_timeout_ms.min(10_000)
                || Instant::now() >= deadline
            {
                return Err(if recorder {
                    "Menu button did not return to recorder before dialog checks"
                } else {
                    "Menu button did not leave the recorder screen for the guard test"
                }
                .into());
            }
            thread::sleep(Duration::from_millis(5));
        }
    };
    let text_pointers = word(l6max::gui_text_pointers_by_language)?;
    let original_resources = qmp.read_memory(0, text_pointers + 229 * 4, 12)?;
    let lengths_table = word(l6max::gui_text_lengths_by_language)?;
    let original_lengths = qmp.read_memory(0, lengths_table + 229 * 2, 6)?;
    let mut original_text = Vec::new();
    for (pointer, capacity) in original_resources
        .chunks_exact(4)
        .zip(original_lengths.chunks_exact(2))
    {
        let address = u32::from_le_bytes(pointer.try_into().unwrap());
        let count = (usize::from(u16::from_le_bytes(capacity.try_into()?)) + 1) * 2;
        original_text.push((address, qmp.read_memory(0, address, count)?));
    }
    let capture = |duration: u32, name: &str| -> Result<[u8; 1024], Box<dyn std::error::Error>> {
        let start = inputs.panel.snapshot().guest_ms;
        let begin = Instant::now();
        let raw = loop {
            let data = session.poll()?;
            let elapsed = inputs.panel.snapshot().guest_ms.wrapping_sub(start);
            if elapsed >= duration {
                break data.display;
            }
            if elapsed > guest_timeout_ms || begin.elapsed() > Duration::from_secs(wall_timeout_s) {
                return Err("knob-dialog display capture exceeded its deadline".into());
            }
            thread::sleep(Duration::from_millis(5));
        };
        let native = image::GrayImage::from_fn(128, 64, |x, y| {
            let index = ((63 - y as usize) / 8) * 128 + x as usize;
            image::Luma([if raw[index] & (1 << ((63 - y as usize) % 8)) != 0 {
                255
            } else {
                0
            }])
        });
        if let Some(scene) = name.strip_prefix("window-scene-")
            && [
                "high", "freq", "mid", "low", "aux1", "aux2", "efx", "sub-mix", "pan", "level",
            ]
            .contains(&scene)
        {
            let path = l6max_host::paths::repository_root()
                .join(KNOB_DIALOG_REFERENCES)
                .join(format!("{scene}.png"));
            check_display_reference(&raw, &path, &options.logs, name)
                .map_err(|error| format!("knob-dialog screen {scene}: {error}"))?;
        }
        image::DynamicImage::ImageLuma8(native)
            .resize(768, 384, image::imageops::FilterType::Nearest)
            .save(options.logs.join(format!("{name}.png")))?;
        Ok(raw)
    };
    let inputs = session.engine.inputs.as_ref().unwrap();
    // Check the first-use guard on an actual non-recorder screen before
    // the dialog has initialized state or claimed its display callback.
    use firmware_api::l6max::GUI_RECORDER_WINDOW;
    if u32::from_le_bytes(
        qmp.read_memory(0, l6max::current_window, 4)?
            .try_into()
            .unwrap(),
    ) != GUI_RECORDER_WINDOW
    {
        return Err("knob-dialog test did not start from the recorder screen".into());
    }
    if qmp.read_memory(0, l6max::gui_bitmap_table_entry_0, 4)? != 0u32.to_le_bytes()
        || qmp.read_memory(0, l6max::gui_display_callback_table + 20, 4)? != 0u32.to_le_bytes()
    {
        return Err(
            "knob-dialog state or callback was already active before the guard test".into(),
        );
    }
    inputs.main.tap(0, 120)?;
    let non_recorder_window = wait_window(false)?;
    capture(100, "knob-dialog-non-recorder-before")?;
    inputs.knob(0, -1)?;
    let non_recorder_knob = inputs.panel.snapshot().guest_ms;
    wait_guest(
        session,
        non_recorder_knob,
        500,
        guest_timeout_ms,
        wall_timeout_s,
    )?;
    let control = l6ctl::OwnedControl::new(&session.engine);
    let reader = control.knob_popup_reader()?;
    let observed = paused_observation(
        || qmp.stop().map_err(|error| error.to_string()),
        || reader.guard_state(),
        || qmp.resume().map_err(|error| error.to_string()),
    )?;
    let non_recorder_no_dialog = guard_rejected(&observed, non_recorder_window);
    if !non_recorder_no_dialog {
        return Err(format!(
            "knob event opened or changed the dialog outside recorder: state={:02x?}, slot5={:02x?}, window={:#010x}",
            observed.state.to_le_bytes(), observed.callback.to_le_bytes(), observed.window
        ).into());
    }
    inputs.main.tap(0, 120)?;
    wait_window(true)?;
    wait_recorder_controls()?;
    let non_recorder_guard_passed = true;

    Ok(json!({"non_recorder_first_use_rejected": non_recorder_guard_passed}))
}

pub(super) fn run_knob_dialog_baseline(
    session: &Session,
    options: &Options,
    guest_timeout_ms: u32,
    wall_timeout_s: u64,
) -> Result<[u8; 1024], Box<dyn std::error::Error>> {
    use std::time::Instant;
    let main = selected_main_image(options).map_err(std::io::Error::other)?;
    if main.get(0x40c..0x410) != Some(&0x8007eea9u32.to_le_bytes()) {
        return Err("patched startup hook does not reserve popup RAM".into());
    }
    let inputs = session.engine.inputs.as_ref().unwrap();
    let control = l6ctl::OwnedControl::new(&session.engine);
    let reader = control.knob_parameter_reader()?;
    let capture = |duration: u32, name: &str| -> Result<[u8; 1024], Box<dyn std::error::Error>> {
        let start = inputs.panel.snapshot().guest_ms;
        let begin = Instant::now();
        let raw = loop {
            let data = session.poll()?;
            let elapsed = inputs.panel.snapshot().guest_ms.wrapping_sub(start);
            if elapsed >= duration {
                break data.display;
            }
            if elapsed > guest_timeout_ms || begin.elapsed() > Duration::from_secs(wall_timeout_s) {
                return Err("knob-dialog display capture exceeded its deadline".into());
            }
            thread::sleep(Duration::from_millis(5));
        };
        let native = image::GrayImage::from_fn(128, 64, |x, y| {
            let index = ((63 - y as usize) / 8) * 128 + x as usize;
            image::Luma([if raw[index] & (1 << ((63 - y as usize) % 8)) != 0 {
                255
            } else {
                0
            }])
        });
        if let Some(scene) = name.strip_prefix("window-scene-")
            && [
                "high", "freq", "mid", "low", "aux1", "aux2", "efx", "sub-mix", "pan", "level",
            ]
            .contains(&scene)
        {
            let path = l6max_host::paths::repository_root()
                .join(KNOB_DIALOG_REFERENCES)
                .join(format!("{scene}.png"));
            check_display_reference(&raw, &path, &options.logs, name)
                .map_err(|error| format!("knob-dialog screen {scene}: {error}"))?;
        }
        image::DynamicImage::ImageLuma8(native)
            .resize(768, 384, image::imageops::FilterType::Nearest)
            .save(options.logs.join(format!("{name}.png")))?;
        Ok(raw)
    };
    let observed = reader.parameter_state()?;
    let [slot0, slot2, display_callback_target] = observed.callback_slots;
    if display_callback_target != 0 || slot0 != 0 || slot2 != 0 {
        return Err(format!("expected an empty slot 5 before the first knob event; slot0={slot0:#010x}, slot2={slot2:#010x}, slot5={display_callback_target:#010x}").into());
    }
    inputs.tap(35, 50)?;
    let start = inputs.panel.snapshot().guest_ms;
    let deadline = Instant::now() + Duration::from_secs(wall_timeout_s.min(180));
    loop {
        session.poll()?;
        let actual = reader.parameter_mode()?;
        if actual == 9 {
            break;
        }
        if inputs.panel.snapshot().guest_ms.wrapping_sub(start) >= guest_timeout_ms.min(10_000)
            || Instant::now() >= deadline
        {
            return Err(format!("parameter mode did not become 9; found {actual}").into());
        }
        thread::sleep(Duration::from_millis(5));
    }
    capture(100, "window-scene-before")
}

/// State retained across the callback first-use race probe. Deadlines are
/// anchored at the operation that resumes the guest, not at a later step.
struct CallbackRace {
    semaphore_count: Vec<u8>,
    callback: Option<u32>,
    registration_deadline: Instant,
    callback_deadline: Option<Instant>,
    frame: Option<[u8; 1024]>,
    snapshot: Option<CallbackRaceSnapshot>,
}

struct CallbackRaceSnapshot {
    state: Vec<u8>,
    slot: Vec<u8>,
    frame_unchanged: bool,
    rejected: bool,
}

pub(super) struct PreflightContext {
    original_window: Vec<u8>,
    original_bitmap: Vec<u8>,
    original_bitmap_descriptor: Vec<u8>,
    original_notification_semaphore: Vec<u8>,
    notification_semaphore: u32,
    text_table: u32,
    original_resources: Vec<u8>,
    shared_resource_pointer: u32,
    original_shared_resource: Vec<u8>,
    original_lengths_table: u32,
    original_lengths: Vec<u8>,
    widget7: u32,
    widget33: u32,
    original_widget7: Vec<u8>,
    original_widget33: Vec<u8>,
    background10: u32,
    background37: u32,
    background48: u32,
    original_background_layers: [Vec<u8>; 3],
    heap_before: Vec<u32>,
    debugger: Option<KnobDebugSession>,
    alias_widget: Option<u32>,
    alias_layer: Option<Vec<u8>>,
    next: usize,
    callback_race: Option<CallbackRace>,
    activation_semaphore_count: Option<Vec<u8>>,
    activation_callback: Option<u32>,
    sd_expected_callback: Option<u32>,
    firmware_callback: Option<l6max_host::knob_binding::FirmwareBinding>,
    activation_phase: u8,
    activation_started: Option<(u32, Instant)>,
    activation_snapshot: Option<FirstActivationSnapshot>,
    active: Option<ActiveSnapshot>,
    opened: Option<OpenedOverlay>,
    task_contexts: Option<TaskContexts>,
    task_probe: Option<TaskProbe>,
    overlay_heap: Option<(Vec<u32>, u32)>,
    mode_index: usize,
    last_mode: Option<u8>,
    final_phase: u8,
    overlay_check: Option<OverlayEvidence>,
    overlay_refresh: bool,
    overlay_reactivation: Option<(u32, Instant)>,
    bitmap_probe: Option<BitmapProbe>,
    active_display: Option<ActiveDisplayEvidence>,
    level_phase: Option<u8>,
    level_peak: Option<u8>,
    pending_preflight_probe: Option<PendingPreflightProbe>,
    locked_update: Option<LockedUpdate>,
    navigation: Option<l6ctl::NavigationSnapshot>,
    restoration: Option<RestorationEvidence>,
    pending_notification: Option<PendingNotification>,
    pending_invalid_signature: Option<PendingInvalidSignature>,
    pending_invalid_tag: Option<PendingInvalidTag>,
}

fn verify_sd_callback(expected: Option<u32>, callback: u32) -> Result<(), String> {
    if expected.is_some_and(|value| value != callback) {
        Err(format!(
            "registered callback {callback:#010x} does not match loaded knob SD module"
        ))
    } else {
        Ok(())
    }
}
fn load_firmware_knob_fixture(
    options: &Options,
) -> Result<l6max_host::knob_binding::FirmwareBinding, String> {
    let image = selected_main_image(options)?;
    let candidates = [
        options.logs.join(crate::fixture_layout::FIRMWARE_REPORT),
        options.fixture_firmware_report.clone().unwrap_or_default(),
        options
            .firmware_dir
            .join(crate::fixture_layout::FIRMWARE_REPORT),
        options
            .firmware_dir
            .join(crate::fixture_layout::PATCH_REPORT),
    ];
    let mut errors = Vec::new();
    for path in candidates {
        if !path.is_file() {
            continue;
        }
        let report = fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        match l6max_host::knob_binding::FirmwareBinding::from_fixture(&report, &image) {
            Ok(binding) => return Ok(binding),
            Err(error) => errors.push(format!("{}: {error}", path.display())),
        }
    }
    Err(format!(
        "no matching knob firmware callback report: {}",
        errors.join("; ")
    ))
}
fn load_sd_knob_fixture(
    options: &Options,
    sd_knob: bool,
) -> Result<Option<(PathBuf, l6max_host::knob_binding::SdBinding)>, Box<dyn std::error::Error>> {
    if !sd_knob {
        return Ok(None);
    }
    let root = sd_fixture_root(options)?;
    let output = root.join(crate::fixture_layout::SD_BUNDLE_BUILD);
    let report_path = output.join(crate::fixture_layout::PATCH_REPORT);
    let report_bytes = fs::read(report_path)?;
    let report: Value = serde_json::from_slice(&report_bytes)?;
    let patches = report["patches"]
        .as_array()
        .ok_or("SD module report missing patches")?;
    if !patches.iter().any(|patch| {
        patch["members"].as_array().is_some_and(|members| {
            members
                .iter()
                .any(|member| member.as_str() == Some("knob-dialog"))
        })
    }) {
        return Err("expected knob-dialog SD module missing from fixture report".into());
    }
    if patches.len() != 1 {
        return Err("knob preflight requires one composed SD module".into());
    }
    let name = patches[0]["file"]
        .as_str()
        .and_then(|name| Path::new(name).file_name())
        .ok_or("knob SD module filename missing")?;
    let module = fs::read(output.join("PATCHES").join(name))?;
    let binding = l6max_host::knob_binding::SdBinding::from_fixture(&report_bytes, &module)?;
    Ok(Some((root.to_path_buf(), binding)))
}
fn verify_sd_knob_fixture(
    session: &Session,
    options: &Options,
    debugger: &mut KnobDebugSession,
    sd_knob: bool,
    shared: (Instant, u32, u32, u64, u16),
) -> Result<Option<u32>, Box<dyn std::error::Error>> {
    let Some((root, binding)) = load_sd_knob_fixture(options, sd_knob)? else {
        return Ok(None);
    };
    let main = selected_main_image(options).map_err(std::io::Error::other)?;
    let loader = fs::read(
        root.join(crate::fixture_layout::LOADER_BUILD)
            .join(crate::fixture_layout::PATCH_REPORT),
    )?;
    let site = l6patch::hook_report::dispatch_site(
        &loader,
        &main,
        "L6max",
        l6max::mixer_apply_channel_parameter,
    )?;
    let inputs = session.engine.inputs.as_ref().ok_or("missing inputs")?;
    let deadline = shared.0 + Duration::from_secs(shared.3);
    remaining_debug_wait(shared, inputs.panel.snapshot().guest_ms, shared.3)?;
    let qmp = &session.engine.management[0];
    qmp.stop_before(deadline)?;
    remaining_debug_wait(shared, inputs.panel.snapshot().guest_ms, shared.3)?;
    let result = (|| -> Result<u32, Box<dyn std::error::Error>> {
        let ctl = l6ctl::OwnedControl::new(&session.engine);
        let reader = ctl.sd_loader_reader_cancellable(deadline, || {
            remaining_debug_wait(shared, inputs.panel.snapshot().guest_ms, shared.3).is_err()
        })?;
        Ok(binding.verify_live(&reader, site.index, deadline, || {
            remaining_debug_wait(shared, inputs.panel.snapshot().guest_ms, shared.3).is_err()
        })?)
    })();
    let resume = remaining_debug_wait(shared, inputs.panel.snapshot().guest_ms, shared.3)
        .map_err(|e| std::io::Error::other(e.to_string()))
        .and_then(|_| debugger.resume_guest_before(deadline));
    match (result, resume) {
        (Ok(callback), Ok(())) => Ok(Some(callback)),
        (Err(error), Ok(())) => Err(error),
        (Ok(_), Err(error)) => Err(format!("SD knob verification resume failed: {error}").into()),
        (Err(error), Err(resume)) => {
            Err(format!("{error}; SD knob verification resume failed: {resume}").into())
        }
    }
}

pub(super) fn begin_preflight(
    session: &Session,
    options: &Options,
    sd_knob: bool,
    shared: (Instant, u32, u32, u64, u16),
) -> Result<PreflightContext, Box<dyn std::error::Error>> {
    let qmp = &session.engine.management[0];
    let heap = || -> Result<Vec<u32>, Box<dyn std::error::Error>> {
        Ok(qmp
            .read_memory(0, l6max::xFreeBytesRemaining, 16)?
            .chunks_exact(4)
            .map(|b| u32::from_le_bytes(b.try_into().unwrap()))
            .collect())
    };
    let original_window = qmp.read_memory(0, l6max::current_window, 4)?;
    let original_bitmap = qmp.read_memory(0, l6max::gui_bitmap_table_entry_0 + 36 * 4, 4)?;
    let original_bitmap_descriptor = qmp.read_memory(0, l6max::GUI_BITMAP_36_DESCRIPTOR, 8)?;
    let original_state_handle = qmp.read_memory(0, l6max::gui_bitmap_table_entry_0, 4)?;
    if u32::from_le_bytes(original_bitmap.clone().try_into().unwrap())
        != l6max::GUI_BITMAP_36_DESCRIPTOR
    {
        return Err(
            "stock bitmap-table entry 36 did not point at its guarded ROM descriptor".into(),
        );
    }
    if original_state_handle != 0u32.to_le_bytes() {
        return Err(format!(
            "bitmap-table slot 0 is not the expected null sentinel: {original_state_handle:02x?}"
        )
        .into());
    }
    let original_notification_semaphore = qmp.read_memory(0, l6max::notification_slot, 4)?;
    let notification_semaphore =
        u32::from_le_bytes(original_notification_semaphore.clone().try_into().unwrap());
    if notification_semaphore == 0 || notification_semaphore & 1 != 0 {
        return Err(format!(
            "expected the stock untagged notification semaphore, got {notification_semaphore:#010x}"
        )
        .into());
    }
    let text_table = u32::from_le_bytes(
        qmp.read_memory(0, l6max::gui_text_pointers_by_language, 4)?
            .try_into()
            .unwrap(),
    );
    let original_resources = qmp.read_memory(0, text_table + 229 * 4, 8)?;
    let shared_resource_pointer = u32::from_le_bytes(
        qmp.read_memory(0, text_table + 179 * 4, 4)?
            .try_into()
            .unwrap(),
    );
    let original_shared_resource = qmp.read_memory(0, shared_resource_pointer, 512)?;
    let original_lengths_table = u32::from_le_bytes(
        qmp.read_memory(0, l6max::gui_text_lengths_by_language, 4)?
            .try_into()
            .unwrap(),
    );
    let original_lengths = qmp.read_memory(0, original_lengths_table + 229 * 2, 4)?;
    let widget_pointer = |index: u32| -> Result<u32, Box<dyn std::error::Error>> {
        Ok(u32::from_le_bytes(
            qmp.read_memory(0, l6max::gui_widget_table + index * 4, 4)?
                .try_into()
                .unwrap(),
        ))
    };
    let widget7 = widget_pointer(7)?;
    let widget33 = widget_pointer(33)?;
    let original_widget7 = qmp.read_memory(0, widget7, 16)?;
    let original_widget33 = qmp.read_memory(0, widget33, 16)?;
    if original_widget7.as_slice() != [1, 0, 0, 0, 10, 0, 1, 0, 1, 0, 126, 0, 1, 0, 1, 2]
        || original_widget33.as_slice() != [1, 0, 1, 0, 40, 0, 30, 0, 33, 0, 67, 0, 1, 0, 1, 0]
    {
        return Err("recorder widgets do not match the guarded firmware defaults".into());
    }
    let original_shared_capacity = qmp.read_memory(0, original_lengths_table + 179 * 2, 2)?;
    if u16::from_le_bytes(original_shared_capacity.try_into().unwrap()) != 255
        || original_shared_resource
            .chunks_exact(2)
            .take(255)
            .any(|unit| unit != b"*\0")
        || original_shared_resource[510..512] != [0, 0]
    {
        return Err("shared GUI resource 179 is not the expected free 255-character buffer".into());
    }
    let background_pointer = |index: u32| -> Result<u32, Box<dyn std::error::Error>> {
        Ok(u32::from_le_bytes(
            qmp.read_memory(0, l6max::gui_background_table + index * 4, 4)?
                .try_into()
                .unwrap(),
        ))
    };
    let background10 = background_pointer(10)?;
    let background37 = background_pointer(37)?;
    let background48 = background_pointer(48)?;
    let original_background_layers = [
        qmp.read_memory(0, background10, 1)?,
        qmp.read_memory(0, background37, 1)?,
        qmp.read_memory(0, background48, 1)?,
    ];
    let heap_before = heap()?;
    let socket = options
        .gdb_socket
        .as_ref()
        .ok_or("knob-dialog lock-contention check needs GDB")?;
    let firmware_callback = if sd_knob {
        None
    } else {
        Some(load_firmware_knob_fixture(options)?)
    };
    let mut debugger = KnobDebugSession::connect(qmp, socket)?;
    let sd_expected_callback =
        verify_sd_knob_fixture(session, options, &mut debugger, sd_knob, shared)?;
    // Freeze the dynamic targets while retaining the existing debugger lease.
    // The host check binds these to the live tables and writable RAM; the
    // legacy debugger transaction remains responsible for all mutations.
    let inputs = session.engine.inputs.as_ref().ok_or("missing inputs")?;
    let deadline = shared.0 + Duration::from_secs(shared.3);
    remaining_debug_wait(shared, inputs.panel.snapshot().guest_ms, shared.3)?;
    qmp.stop_before(deadline)?;
    let targets = l6ctl::KnobPreflightTargets {
        widget7,
        background10,
        notification_semaphore,
    };
    let verified = targets.verify(|address| {
        remaining_debug_wait(shared, inputs.panel.snapshot().guest_ms, shared.3)
            .map_err(|e| e.to_string())?;
        let bytes = qmp
            .read_memory_before(0, address, 4, deadline)
            .map_err(|e| e.to_string())?;
        remaining_debug_wait(shared, inputs.panel.snapshot().guest_ms, shared.3)
            .map_err(|e| e.to_string())?;
        Ok(u32::from_le_bytes(
            bytes.try_into().map_err(|_| "short target pointer")?,
        ))
    });
    let resume = remaining_debug_wait(shared, inputs.panel.snapshot().guest_ms, shared.3)
        .map_err(|e| std::io::Error::other(e.to_string()))
        .and_then(|_| debugger.resume_guest_before(deadline));
    match (verified, resume) {
        (Ok(_), Ok(())) => (),
        (Err(error), Ok(())) => return Err(error.into()),
        (Ok(_), Err(error)) => {
            return Err(format!("knob target verification resume failed: {error}").into());
        }
        (Err(error), Err(resume)) => {
            return Err(
                format!("{error}; knob target verification resume failed: {resume}").into(),
            );
        }
    }
    Ok(PreflightContext {
        original_window,
        original_bitmap,
        original_bitmap_descriptor,
        original_notification_semaphore,
        notification_semaphore,
        text_table,
        original_resources,
        shared_resource_pointer,
        original_shared_resource,
        original_lengths_table,
        original_lengths,
        widget7,
        widget33,
        original_widget7,
        original_widget33,
        background10,
        background37,
        background48,
        original_background_layers,
        heap_before,
        debugger: Some(debugger),
        alias_widget: None,
        alias_layer: None,
        next: 0,
        callback_race: None,
        activation_semaphore_count: None,
        activation_callback: None,
        sd_expected_callback,
        firmware_callback,
        activation_phase: 0,
        activation_started: None,
        activation_snapshot: None,
        active: None,
        opened: None,
        task_contexts: None,
        task_probe: None,
        overlay_heap: None,
        mode_index: 0,
        last_mode: None,
        final_phase: 0,
        overlay_check: None,
        overlay_refresh: false,
        overlay_reactivation: None,
        bitmap_probe: None,
        active_display: None,
        level_phase: None,
        level_peak: None,
        pending_preflight_probe: None,
        locked_update: None,
        navigation: None,
        restoration: None,
        pending_notification: None,
        pending_invalid_signature: None,
        pending_invalid_tag: None,
    })
}

pub(super) fn abort_preflight(mut context: PreflightContext) -> Result<(), String> {
    if let Some(mut debugger) = context.debugger.take() {
        debugger.finalized = true;
        let failures = cleanup_debugger(&mut debugger);
        if !failures.is_empty() {
            return Err(failures.join("; "));
        }
    }
    Ok(())
}

fn preflight_rejected(state: &[u8], callback: &[u8], heap: &[u32], baseline_heap: &[u32]) -> bool {
    state == 0u32.to_le_bytes() && callback == 0u32.to_le_bytes() && heap == baseline_heap
}

struct PendingPreflightProbe {
    kind: crate::scenario::PreflightProbe,
    address: u32,
    original: Vec<u8>,
    replacement: Vec<u8>,
    description: &'static str,
    sample: Option<(Vec<u8>, Vec<u8>, Vec<u32>)>,
}

pub(super) fn preflight_probe_index(context: &PreflightContext) -> usize {
    context.next
}

pub(super) fn prepare_preflight_probe(
    _session: &Session,
    context: &mut PreflightContext,
    probe: crate::scenario::PreflightProbe,
) -> Result<Value, Box<dyn std::error::Error>> {
    use crate::scenario::PreflightProbe as P;
    if context.next != probe.index() || context.pending_preflight_probe.is_some() {
        return Err("knob-dialog preflight probes are out of order".into());
    }
    let (address, original, replacement, description) = match probe {
        P::Descriptor => (
            context.widget7 + 6,
            vec![1, 0],
            vec![2, 0],
            "descriptor drift",
        ),
        P::TextAlias => (
            0,
            vec![],
            179u16.to_le_bytes().to_vec(),
            "shared text resource alias",
        ),
        P::LayerCollision => (
            0,
            vec![],
            4u16.to_le_bytes().to_vec(),
            "scene layer collision",
        ),
        P::WidgetLayer => (context.widget7, vec![], vec![0], "changed widget layer"),
        P::BackgroundLayer => (
            context.background10,
            context.original_background_layers[0].clone(),
            vec![1],
            "changed background layer",
        ),
    };
    context.pending_preflight_probe = Some(PendingPreflightProbe {
        kind: probe,
        address,
        original,
        replacement,
        description,
        sample: None,
    });
    Ok(json!({"probe":description}))
}

pub(super) fn inject_preflight_probe(
    session: &Session,
    context: &mut PreflightContext,
    shared: (Instant, u32, u32, u64, u16),
) -> Result<Value, Box<dyn std::error::Error>> {
    let probe = context
        .pending_preflight_probe
        .as_mut()
        .ok_or("missing preflight probe")?;
    let debugger = context
        .debugger
        .as_mut()
        .ok_or("missing knob-dialog debugger")?;
    let qmp = &session.engine.management[0];
    let inputs = session.engine.inputs.as_ref().ok_or("missing inputs")?;
    let deadline = shared.0 + Duration::from_secs(shared.3);
    let check = || {
        remaining_debug_wait(shared, inputs.panel.snapshot().guest_ms, shared.3)
            .map(|_| ())
            .map_err(|e| std::io::Error::other(e.to_string()))
    };
    check()?;
    qmp.stop_before(deadline)?;
    let read_word = |address| -> Result<u32, String> {
        check().map_err(|e| e.to_string())?;
        let bytes = qmp
            .read_memory_before(0, address, 4, deadline)
            .map_err(|e| e.to_string())?;
        check().map_err(|e| e.to_string())?;
        Ok(u32::from_le_bytes(
            bytes.try_into().map_err(|_| "short target pointer")?,
        ))
    };
    l6ctl::KnobPreflightTargets {
        widget7: context.widget7,
        background10: context.background10,
        notification_semaphore: context.notification_semaphore,
    }
    .verify(read_word)?;
    if matches!(context.next, 1 | 2) {
        let alias = if context.next == 1 {
            read_word(l6max::gui_widget_table + 60 * 4)?
        } else {
            context.alias_widget.ok_or("missing alias widget")?
        };
        l6ctl::verify_preflight_widget60(alias, read_word)?;
        context.alias_widget = Some(alias);
        probe.address = if context.next == 1 {
            alias.checked_add(4).ok_or("widget 60 address overflow")?
        } else {
            alias
        };
        check()?;
        probe.original = qmp.read_memory_before(0, probe.address, 2, deadline)?;
        check()?;
        if context.next == 2 {
            context.alias_layer = Some(probe.original.clone());
        }
    }
    if matches!(probe.kind, crate::scenario::PreflightProbe::WidgetLayer) {
        check()?;
        probe.original = qmp.read_memory_before(0, probe.address, 1, deadline)?;
        check()?;
    }
    debugger.write_before(probe.address, &probe.replacement, deadline, check)?;
    check()?;
    debugger.resume_guest_before(deadline)?;
    Ok(json!({"injected":probe.description}))
}

pub(super) fn trigger_preflight_probe(
    session: &Session,
    context: &mut PreflightContext,
    delta: i32,
    wall_timeout_s: u64,
    shared: (Instant, u32, u32, u64, u16),
) -> Result<Value, Box<dyn std::error::Error>> {
    let debugger = context
        .debugger
        .as_mut()
        .ok_or("missing knob-dialog debugger")?;
    turn_and_wait_hook_bounded(session, debugger, delta, wall_timeout_s, Some(shared), None)?;
    Ok(json!({"delta":delta}))
}

pub(super) fn sample_preflight_probe(
    session: &Session,
    context: &mut PreflightContext,
    reader: &l6max_host::knob_popup::OverlayReader,
    heap_reader: &l6max_host::heap::CounterReader,
) -> Result<Value, Box<dyn std::error::Error>> {
    session.engine.management[0].stop()?;
    let (state, callback) = reader.preflight_state()?;
    let heap = heap_reader.read()?;
    let heap = vec![
        heap.free,
        heap.minimum_ever_free,
        heap.allocations,
        heap.frees,
    ];
    context
        .pending_preflight_probe
        .as_mut()
        .ok_or("missing preflight probe")?
        .sample = Some((state, callback, heap));
    Ok(json!({"sampled":true}))
}

pub(super) fn restore_preflight_probe(
    context: &mut PreflightContext,
) -> Result<Value, Box<dyn std::error::Error>> {
    let probe = context
        .pending_preflight_probe
        .as_ref()
        .ok_or("missing preflight probe")?;
    let debugger = context
        .debugger
        .as_mut()
        .ok_or("missing knob-dialog debugger")?;
    debugger.write(probe.address, &probe.original)?;
    debugger.resume_guest()?;
    Ok(json!({"restored":probe.description}))
}

pub(super) fn assert_preflight_probe(
    context: &mut PreflightContext,
) -> Result<Value, Box<dyn std::error::Error>> {
    let probe = context
        .pending_preflight_probe
        .take()
        .ok_or("missing preflight probe")?;
    let (state, slot5, heap) = probe.sample.ok_or("missing preflight sample")?;
    if !preflight_rejected(&state, &slot5, &heap, &context.heap_before) {
        return Err(format!(
            "{} was not rejected cleanly: state={state:02x?}, slot5={slot5:02x?}, heap={heap:?}",
            probe.description
        )
        .into());
    }
    context.next += 1;
    Ok(
        json!({"preflight":probe.description,"rejected":true,"state":state,"callback":slot5,"heap":heap,"baseline_heap":context.heap_before}),
    )
}

const DISPLAY_CYCLE2_RETURN_GUARD: [u8; 60] = [
    0x4a, 0xf2, 0x68, 0x55, 0x4f, 0xf0, 0x00, 0x08, 0xc8, 0xf2, 0x20, 0x05, 0x01, 0x26, 0x00, 0x27,
    0x05, 0xe0, 0x00, 0xbf, 0x78, 0x1c, 0xc1, 0xb2, 0x8f, 0x1f, 0x18, 0xbf, 0x07, 0x46, 0x28, 0x69,
    0x4f, 0xf0, 0xff, 0x31, 0x0c, 0xf0, 0x18, 0xf8, 0xf8, 0xb2, 0x05, 0xeb, 0x80, 0x04, 0x60, 0x6b,
    0x00, 0x28, 0xef, 0xd0, 0xe6, 0x61, 0x80, 0x47, 0xc4, 0xf8, 0x1c, 0x80,
];
const DISPLAY_CYCLE2_DISPATCH_GUARD: u32 = 0x8008_4ff0;
const DISPLAY_CYCLE2_SLOT5_RUNNING: u32 = 0x8020_a598;

fn supported_display_dispatcher(bytes: &[u8]) -> bool {
    bytes == DISPLAY_CYCLE2_RETURN_GUARD
}

fn callback_first_use_ready(state: &[u8], slot: u32, expected_callback: u32, running: u32) -> bool {
    state.len() == 4
        && u16::from_le_bytes([state[0], state[1]]) == 0x4b4e
        && state[2] != 0
        && state[3] == u8::MAX
        && expected_callback & 1 == 1
        && slot == expected_callback
        && running == 0
}

fn verify_stopped_semaphore(
    session: &Session,
    semaphore: u32,
    shared: (Instant, u32, u32, u64, u16),
) -> Result<(), String> {
    l6ctl::verify_notification_semaphore(semaphore, |address| {
        activation_word(session, address, shared).map_err(|e| e.to_string())
    })
}

pub(super) fn begin_callback_race(
    session: &Session,
    context: &mut PreflightContext,
    shared: (Instant, u32, u32, u64, u16),
) -> Result<Value, Box<dyn std::error::Error>> {
    if context.next != 5 || context.callback_race.is_some() {
        return Err("knob-dialog race step is out of order".into());
    }
    let qmp = &session.engine.management[0];
    let inputs = session.engine.inputs.as_ref().ok_or("missing inputs")?;
    let debugger = context
        .debugger
        .as_mut()
        .ok_or("missing knob-dialog debugger")?;
    remaining_debug_wait(shared, inputs.panel.snapshot().guest_ms, shared.3)?;
    let deadline = shared.0 + Duration::from_secs(shared.3);
    qmp.stop_before(deadline)?;
    verify_stopped_semaphore(session, context.notification_semaphore, shared)?;
    let semaphore_count = activation_read(session, context.notification_semaphore + 56, 4, shared)?;
    if semaphore_count != 1u32.to_le_bytes() {
        remaining_debug_wait(shared, inputs.panel.snapshot().guest_ms, shared.3)?;
        debugger.resume_guest_before(deadline)?;
        return Err("notification semaphore was not available before contention check".into());
    }
    debugger.write_before(
        context.notification_semaphore + 56,
        &0u32.to_le_bytes(),
        deadline,
        || {
            remaining_debug_wait(shared, inputs.panel.snapshot().guest_ms, shared.3)
                .map(|_| ())
                .map_err(|e| std::io::Error::other(e.to_string()))
        },
    )?;
    remaining_debug_wait(shared, inputs.panel.snapshot().guest_ms, shared.3)?;
    debugger.breakpoint_before(
        firmware_api::l6max::gui_display_callback_register,
        true,
        deadline,
    )?;
    remaining_debug_wait(shared, inputs.panel.snapshot().guest_ms, shared.3)?;
    debugger.resume_guest_before(deadline)?;
    inputs.knob(0, -1)?;
    context.callback_race = Some(CallbackRace {
        semaphore_count,
        callback: None,
        registration_deadline: Instant::now() + Duration::from_secs(10),
        callback_deadline: None,
        frame: None,
        snapshot: None,
    });
    Ok(json!({"semaphore_held": true, "delta": -1}))
}

fn race_deadline_expired(now: Instant, deadline: Instant) -> bool {
    now >= deadline
}

pub(super) fn wait_race_registration(
    session: &Session,
    context: &mut PreflightContext,
    shared: (Instant, u32, u32, u64, u16),
) -> Result<Value, Box<dyn std::error::Error>> {
    let race = context
        .callback_race
        .as_mut()
        .ok_or("missing callback race")?;
    if race.callback.is_some() {
        return Err("callback registration already observed".into());
    }
    let qmp = &session.engine.management[0];
    let inputs = session.engine.inputs.as_ref().ok_or("missing inputs")?;
    while qmp.query_status()?.running {
        session.poll()?;
        if race_deadline_expired(Instant::now(), race.registration_deadline) {
            return Err("first-use callback registration breakpoint timed out".into());
        }
        remaining_debug_wait(shared, inputs.panel.snapshot().guest_ms, shared.3)?;
        thread::sleep(Duration::from_millis(5));
    }
    if race_deadline_expired(Instant::now(), race.registration_deadline) {
        return Err("first-use callback registration breakpoint timed out".into());
    }
    let debugger = context
        .debugger
        .as_mut()
        .ok_or("missing knob-dialog debugger")?;
    let pc = debugger.read_register(15)? & !1;
    let slot = debugger.read_register(0)?;
    let callback = debugger.read_register(1)?;
    if pc != firmware_api::l6max::gui_display_callback_register || slot != 5 || callback & 1 == 0 {
        return Err(format!("unexpected display callback registration: pc={pc:#010x}, slot={slot}, callback={callback:#010x}").into());
    }
    verify_sd_callback(context.sd_expected_callback, callback)?;
    if let Some(binding) = &context.firmware_callback {
        let deadline = shared.0 + Duration::from_secs(shared.3);
        let ctl = l6ctl::OwnedControl::new(&session.engine);
        binding.validate_live(callback, |address, length| {
            remaining_debug_wait(shared, inputs.panel.snapshot().guest_ms, shared.3)
                .map_err(|e| e.to_string())?;
            let bytes = ctl
                .read_main_memory_before(address, length, deadline)
                .map_err(|e| e.to_string())?
                .bytes;
            remaining_debug_wait(shared, inputs.panel.snapshot().guest_ms, shared.3)
                .map_err(|e| e.to_string())?;
            Ok(bytes)
        })?;
    } else if context.sd_expected_callback.is_none() {
        return Err("knob callback has no verified firmware or SD binding".into());
    }
    race.callback = Some(callback);
    Ok(json!({"pc": pc, "slot": slot, "callback": callback}))
}

pub(super) fn arm_race_callback(
    session: &Session,
    context: &mut PreflightContext,
    shared: (Instant, u32, u32, u64, u16),
) -> Result<Value, Box<dyn std::error::Error>> {
    let race = context
        .callback_race
        .as_mut()
        .ok_or("missing callback race")?;
    let callback = race.callback.ok_or("missing race callback")?;
    if race.callback_deadline.is_some() {
        return Err("callback already armed".into());
    }
    let debugger = context
        .debugger
        .as_mut()
        .ok_or("missing knob-dialog debugger")?;
    debugger.breakpoint(callback & !1, true)?;
    debugger.breakpoint(firmware_api::l6max::gui_display_callback_register, false)?;
    debugger.write(
        context.alias_widget.ok_or("missing alias widget")?,
        &4u16.to_le_bytes(),
    )?;
    let inputs = session.engine.inputs.as_ref().ok_or("missing inputs")?;
    remaining_debug_wait(shared, inputs.panel.snapshot().guest_ms, shared.3)?;
    debugger.resume_guest()?;
    race.callback_deadline = Some(Instant::now() + Duration::from_secs(10));
    Ok(json!({"callback": callback, "alias_layer": 4}))
}

pub(super) fn wait_race_callback(
    session: &Session,
    context: &mut PreflightContext,
    shared: (Instant, u32, u32, u64, u16),
) -> Result<Value, Box<dyn std::error::Error>> {
    let race = context
        .callback_race
        .as_mut()
        .ok_or("missing callback race")?;
    let callback = race.callback.ok_or("missing race callback")?;
    let deadline = race.callback_deadline.ok_or("callback not armed")?;
    let qmp = &session.engine.management[0];
    let inputs = session.engine.inputs.as_ref().ok_or("missing inputs")?;
    while qmp.query_status()?.running {
        session.poll()?;
        if race_deadline_expired(Instant::now(), deadline) {
            return Err("first DisplayCycle2 callback breakpoint timed out".into());
        }
        remaining_debug_wait(shared, inputs.panel.snapshot().guest_ms, shared.3)?;
        thread::sleep(Duration::from_millis(5));
    }
    if race_deadline_expired(Instant::now(), deadline) {
        return Err("first DisplayCycle2 callback breakpoint timed out".into());
    }
    let debugger = context
        .debugger
        .as_mut()
        .ok_or("missing knob-dialog debugger")?;
    let pc = debugger.read_register(15)? & !1;
    if pc != callback & !1 {
        return Err(format!(
            "first-use race probe stopped at {pc:#010x}, not callback {:#010x}",
            callback & !1
        )
        .into());
    }
    debugger.breakpoint(callback & !1, false)?;
    remaining_debug_wait(shared, inputs.panel.snapshot().guest_ms, shared.3)?;
    debugger.resume_guest()?;
    Ok(json!({"callback_pc": pc}))
}

pub(super) fn capture_callback_race(
    session: &Session,
    options: &Options,
    context: &mut PreflightContext,
    duration: u32,
    artifact: &str,
    guest_timeout_ms: u32,
    wall_timeout_s: u64,
    shared: (Instant, u32, u32, u64, u16),
) -> Result<Value, Box<dyn std::error::Error>> {
    let inputs = session.engine.inputs.as_ref().ok_or("missing inputs")?;
    let race = context
        .callback_race
        .as_mut()
        .ok_or("missing callback race")?;
    if race.frame.is_some() {
        return Err("callback race frame already captured".into());
    }
    let frame = capture_knob_frame_checked(
        session,
        options,
        duration,
        artifact,
        guest_timeout_ms,
        wall_timeout_s,
        || {
            remaining_debug_wait(shared, inputs.panel.snapshot().guest_ms, wall_timeout_s)?;
            Ok(())
        },
    )?;
    race.frame = Some(frame);
    Ok(json!({"guest_ms": duration, "artifact": artifact}))
}

pub(super) fn sample_callback_race(
    session: &Session,
    context: &mut PreflightContext,
    baseline: &[u8; 1024],
) -> Result<Value, Box<dyn std::error::Error>> {
    let race = context
        .callback_race
        .as_mut()
        .ok_or("missing callback race")?;
    if race.snapshot.is_some() {
        return Err("callback race already sampled".into());
    }
    let callback = race.callback.ok_or("missing race callback")?;
    let qmp = &session.engine.management[0];
    qmp.stop()?;
    let state = qmp.read_memory(0, l6max::gui_bitmap_table_entry_0, 4)?;
    let slot = qmp.read_memory(0, l6max::gui_display_callback_table + 20, 4)?;
    let frame_unchanged = race.frame.as_ref().ok_or("callback race frame missing")? == baseline;
    let rejected = frame_unchanged
        && u16::from_le_bytes(state[..2].try_into().unwrap()) == 0x4b4e
        && u32::from_le_bytes(slot.clone().try_into().unwrap()) == callback
        && qmp.read_memory(0, context.alias_widget.ok_or("missing alias widget")?, 1)? == [4]
        && qmp.read_memory(0, context.widget7, 1)? == [1]
        && qmp.read_memory(0, context.background10, 1)? == [0]
        && qmp.read_memory(0, context.shared_resource_pointer, 512)?
            == context.original_shared_resource;
    race.snapshot = Some(CallbackRaceSnapshot {
        state,
        slot,
        frame_unchanged,
        rejected,
    });
    Ok(json!({"sampled": true}))
}

pub(super) fn restore_callback_race(
    context: &mut PreflightContext,
) -> Result<Value, Box<dyn std::error::Error>> {
    let race = context
        .callback_race
        .as_ref()
        .ok_or("missing callback race")?;
    if race.snapshot.is_none() {
        return Err("callback race not sampled".into());
    }
    let debugger = context
        .debugger
        .as_mut()
        .ok_or("missing knob-dialog debugger")?;
    debugger.write(
        context.alias_widget.ok_or("missing alias widget")?,
        context.alias_layer.as_ref().ok_or("missing alias layer")?,
    )?;
    debugger.resume_guest()?;
    Ok(json!({"alias_restored": true}))
}

pub(super) fn assert_callback_race(
    session: &Session,
    context: &mut PreflightContext,
) -> Result<Value, Box<dyn std::error::Error>> {
    let race = context
        .callback_race
        .as_ref()
        .ok_or("missing callback race")?;
    let snapshot = race.snapshot.as_ref().ok_or("callback race not sampled")?;
    let callback = race.callback.ok_or("missing race callback")?;
    if !snapshot.rejected {
        return Err(format!("callback-side scene preflight failed to reject descriptor drift: state={:02x?}, slot5={:02x?}, frame_unchanged={} ", snapshot.state, snapshot.slot, snapshot.frame_unchanged).into());
    }
    if snapshot.state[2] != 0 || snapshot.state[3] != u8::MAX {
        return Err(format!("knob-dialog retry requires zero ticks and empty mailbox, found ticks={} pending={:#04x}", snapshot.state[2], snapshot.state[3]).into());
    }
    let qmp = &session.engine.management[0];
    if !supported_display_dispatcher(&qmp.read_memory(
        0,
        DISPLAY_CYCLE2_DISPATCH_GUARD,
        DISPLAY_CYCLE2_RETURN_GUARD.len(),
    )?) {
        return Err("unsupported L6max DisplayCycle2 dispatcher revision".into());
    }
    let race_state = snapshot.state.clone();
    context.activation_semaphore_count = Some(race.semaphore_count.clone());
    context.activation_callback = Some(callback);
    context.callback_race = None;
    context.next = 6;
    Ok(
        json!({"callback_preflight_race_rejected": true, "callback": callback, "race_state": race_state, "frame_unchanged": true}),
    )
}

fn activation_baseline_ready(state: &[u8], slot: u32, callback: u32, semaphore: &[u8]) -> bool {
    state.len() == 4
        && u16::from_le_bytes([state[0], state[1]]) == 0x4b4e
        && state[2] == 0
        && state[3] == u8::MAX
        && callback & 1 == 1
        && slot == callback
        && semaphore == 0u32.to_le_bytes()
}

#[derive(Clone)]
struct FirstActivationSnapshot {
    state: Vec<u8>,
    slot: u32,
    heap: Vec<u32>,
    held: bool,
    diagnostic: Option<String>,
    failed: bool,
}
type ActivationBudget = (Instant, u32, u32, u64, u16);
const ACTIVATION_KNOB_DELTA: i32 = -1;
fn activation_encoder_steps() -> i32 {
    ACTIVATION_KNOB_DELTA.saturating_neg()
}
fn activation_read(
    session: &Session,
    address: u32,
    length: usize,
    shared: ActivationBudget,
) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let inputs = session.engine.inputs.as_ref().ok_or("missing inputs")?;
    remaining_debug_wait(shared, inputs.panel.snapshot().guest_ms, shared.3)?;
    let deadline = shared.0 + Duration::from_secs(shared.3);
    let bytes = l6ctl::OwnedControl::new(&session.engine)
        .read_main_memory_before(address, length, deadline)?
        .bytes;
    remaining_debug_wait(shared, inputs.panel.snapshot().guest_ms, shared.3)?;
    Ok(bytes)
}
fn activation_word(
    session: &Session,
    address: u32,
    shared: ActivationBudget,
) -> Result<u32, Box<dyn std::error::Error>> {
    Ok(u32::from_le_bytes(
        activation_read(session, address, 4, shared)?
            .try_into()
            .map_err(|_| "short activation word")?,
    ))
}
fn activation_local_expired(
    now: Instant,
    deadline: Instant,
    guest_now: u32,
    start: u32,
    guest: u32,
) -> bool {
    now >= deadline || guest_now.wrapping_sub(start) >= guest.min(10_000)
}
fn activation_local_check(
    session: &Session,
    start: u32,
    guest: u32,
    deadline: Instant,
) -> Result<(), Box<dyn std::error::Error>> {
    if activation_local_expired(
        Instant::now(),
        deadline,
        session
            .engine
            .inputs
            .as_ref()
            .ok_or("missing inputs")?
            .panel
            .snapshot()
            .guest_ms,
        start,
        guest,
    ) {
        Err("knob dialog did not render while the notification lock was held".into())
    } else {
        Ok(())
    }
}
fn activation_read_local(
    session: &Session,
    address: u32,
    length: usize,
    shared: ActivationBudget,
    start: u32,
    guest: u32,
    deadline: Instant,
) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    activation_local_check(session, start, guest, deadline)?;
    let inputs = session.engine.inputs.as_ref().ok_or("missing inputs")?;
    remaining_debug_wait(shared, inputs.panel.snapshot().guest_ms, shared.3)?;
    let bytes = l6ctl::OwnedControl::new(&session.engine)
        .read_main_memory_before(
            address,
            length,
            deadline.min(shared.0 + Duration::from_secs(shared.3)),
        )?
        .bytes;
    remaining_debug_wait(shared, inputs.panel.snapshot().guest_ms, shared.3)?;
    activation_local_check(session, start, guest, deadline)?;
    Ok(bytes)
}
fn activation_word_local(
    session: &Session,
    address: u32,
    shared: ActivationBudget,
    start: u32,
    guest: u32,
    deadline: Instant,
) -> Result<u32, Box<dyn std::error::Error>> {
    Ok(u32::from_le_bytes(
        activation_read_local(session, address, 4, shared, start, guest, deadline)?
            .try_into()
            .map_err(|_| "short activation word")?,
    ))
}
pub(super) fn activation_trigger(
    session: &Session,
    context: &mut PreflightContext,
    shared: ActivationBudget,
) -> Result<Value, Box<dyn std::error::Error>> {
    if context.next != 6 || context.activation_phase != 0 {
        return Err("activation trigger out of order".into());
    }
    let inputs = session.engine.inputs.as_ref().ok_or("missing inputs")?;
    let callback = context
        .activation_callback
        .ok_or("missing callback pointer")?;
    if context.activation_semaphore_count.is_none() {
        return Err("missing saved semaphore".into());
    }
    let deadline = shared.0 + Duration::from_secs(shared.3);
    remaining_debug_wait(shared, inputs.panel.snapshot().guest_ms, shared.3)?;
    session.engine.management[0].stop_before(deadline)?;
    let before = activation_read(session, l6max::gui_bitmap_table_entry_0, 4, shared)?;
    let slot = activation_word(session, l6max::gui_display_callback_table + 20, shared)?;
    let semaphore = activation_read(session, context.notification_semaphore + 56, 4, shared)?;
    if !activation_baseline_ready(&before, slot, callback, &semaphore) {
        return Err(format!("knob-dialog activation baseline changed: state={before:02x?}, slot={slot:#010x}, semaphore={semaphore:02x?}").into());
    }
    remaining_debug_wait(shared, inputs.panel.snapshot().guest_ms, shared.3)?;
    // `InputHub::knob(0, -1)` uses the opposite raw encoder polarity.
    l6ctl::OwnedControl::new(&session.engine).encoder(1, activation_encoder_steps())?;
    remaining_debug_wait(shared, inputs.panel.snapshot().guest_ms, shared.3)?;
    context
        .debugger
        .as_mut()
        .ok_or("missing knob-dialog debugger")?
        .resume_guest_before(deadline)?;
    context.activation_started = Some((inputs.panel.snapshot().guest_ms, Instant::now()));
    context.activation_phase = 1;
    Ok(json!({"baseline":before,"callback":slot,"semaphore":semaphore,"delta":-1}))
}
pub(super) fn activation_wait(
    session: &Session,
    context: &mut PreflightContext,
    guest: u32,
    wall: u64,
    shared: ActivationBudget,
) -> Result<Value, Box<dyn std::error::Error>> {
    if context.activation_phase != 1 {
        return Err("activation wait out of order".into());
    }
    let inputs = session.engine.inputs.as_ref().ok_or("missing inputs")?;
    let (start, wall_start) = context
        .activation_started
        .ok_or("missing activation start")?;
    let deadline = (shared.0 + Duration::from_secs(shared.3))
        .min(wall_start + Duration::from_secs(wall.min(180)));
    let callback = context
        .activation_callback
        .ok_or("missing callback pointer")?;
    loop {
        remaining_debug_wait(shared, inputs.panel.snapshot().guest_ms, shared.3)?;
        if inputs.panel.snapshot().guest_ms.wrapping_sub(start) >= guest.min(10_000)
            || Instant::now() >= deadline
        {
            return Err("knob dialog did not render while the notification lock was held".into());
        }
        session.poll()?;
        activation_local_check(session, start, guest, deadline)?;
        let state = activation_read_local(
            session,
            l6max::gui_bitmap_table_entry_0,
            4,
            shared,
            start,
            guest,
            deadline,
        )?;
        let slot = activation_word_local(
            session,
            l6max::gui_display_callback_table + 20,
            shared,
            start,
            guest,
            deadline,
        )?;
        if state.len() == 4
            && u16::from_le_bytes([state[0], state[1]]) == 0x4b4e
            && state[2] != 0
            && state[3] == u8::MAX
            && slot & 1 == 1
        {
            if Instant::now() >= deadline
                || inputs.panel.snapshot().guest_ms.wrapping_sub(start) >= guest.min(10_000)
            {
                return Err(
                    "knob dialog did not render while the notification lock was held".into(),
                );
            }
            session.engine.management[0].stop_before(deadline)?;
            activation_local_check(session, start, guest, deadline)?;
            let stopped = activation_read_local(
                session,
                l6max::gui_bitmap_table_entry_0,
                4,
                shared,
                start,
                guest,
                deadline,
            )?;
            let stopped_slot = activation_word_local(
                session,
                l6max::gui_display_callback_table + 20,
                shared,
                start,
                guest,
                deadline,
            )?;
            let running = activation_word_local(
                session,
                DISPLAY_CYCLE2_SLOT5_RUNNING,
                shared,
                start,
                guest,
                deadline,
            )?;
            if Instant::now() >= deadline
                || inputs.panel.snapshot().guest_ms.wrapping_sub(start) >= guest.min(10_000)
            {
                return Err(
                    "knob dialog did not render while the notification lock was held".into(),
                );
            }
            if callback_first_use_ready(&stopped, stopped_slot, callback, running) {
                context.activation_phase = 2;
                return Ok(json!({"callback":stopped_slot,"state":stopped,"running":running}));
            }
            context
                .debugger
                .as_mut()
                .ok_or("missing knob-dialog debugger")?
                .resume_guest_before(deadline)?;
            activation_local_check(session, start, guest, deadline)?;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}
pub(super) fn activation_sample(
    session: &Session,
    context: &mut PreflightContext,
    shared: ActivationBudget,
) -> Result<Value, Box<dyn std::error::Error>> {
    if context.activation_phase != 2 {
        return Err("activation sample out of order".into());
    }
    let state = activation_read(session, l6max::gui_bitmap_table_entry_0, 4, shared)?;
    let slot = activation_word(session, l6max::gui_display_callback_table + 20, shared)?;
    let heap = activation_read(session, l6max::xFreeBytesRemaining, 16, shared)?
        .chunks_exact(4)
        .map(|b| u32::from_le_bytes(b.try_into().unwrap()))
        .collect::<Vec<_>>();
    let held = activation_read(session, context.notification_semaphore + 56, 4, shared)?
        == 0u32.to_le_bytes();
    let valid = u16::from_le_bytes(state[..2].try_into().unwrap()) == 0x4b4e
        && slot & 1 == 1
        && activation_read(session, l6max::gui_bitmap_table_entry_0 + 3, 1, shared)?
            == u8::MAX.to_le_bytes()
        && state[2] != 0
        && activation_read(session, context.widget7, 16, shared)? == context.original_widget7
        && activation_read(session, context.background10, 1, shared)?
            == context.original_background_layers[0]
        && held;
    let failed = !valid || heap[2] != context.heap_before[2] || heap[3] != context.heap_before[3];
    let diagnostic = if failed {
        let widget7 =
            activation_read(session, context.widget7, 16, shared).map_err(|e| e.to_string());
        let widget33 =
            activation_read(session, context.widget33, 16, shared).map_err(|e| e.to_string());
        let background =
            activation_read(session, context.background10, 1, shared).map_err(|e| e.to_string());
        let pc = (|| -> Result<u32, String> {
            let inputs = session.engine.inputs.as_ref().ok_or("missing inputs")?;
            remaining_debug_wait(shared, inputs.panel.snapshot().guest_ms, shared.3)
                .map_err(|e| e.to_string())?;
            let pc = context
                .debugger
                .as_mut()
                .ok_or("missing knob-dialog debugger")?
                .read_register_before(15, shared.0 + Duration::from_secs(shared.3))
                .map_err(|e| e.to_string())?;
            remaining_debug_wait(shared, inputs.panel.snapshot().guest_ms, shared.3)
                .map_err(|e| e.to_string())?;
            Ok(pc)
        })();
        Some(format!(
            "actual_widget7={widget7:02x?}, actual_widget33={widget33:02x?}, actual_background10={background:02x?}, stopped_pc={pc:?}"
        ))
    } else {
        None
    };
    context.activation_snapshot = Some(FirstActivationSnapshot {
        state: state.clone(),
        slot,
        heap: heap.clone(),
        held,
        diagnostic,
        failed,
    });
    context.activation_phase = 3;
    Ok(json!({"state":state,"callback":slot,"heap":heap,"notification_lock_still_held":held}))
}
pub(super) fn activation_restore(
    session: &Session,
    context: &mut PreflightContext,
    shared: ActivationBudget,
) -> Result<Value, Box<dyn std::error::Error>> {
    if context.activation_phase != 3 {
        return Err("activation restore out of order".into());
    }
    let inputs = session.engine.inputs.as_ref().ok_or("missing inputs")?;
    let deadline = shared.0 + Duration::from_secs(shared.3);
    remaining_debug_wait(shared, inputs.panel.snapshot().guest_ms, shared.3)?;
    let guard = verify_stopped_semaphore(session, context.notification_semaphore, shared);
    context
        .debugger
        .as_mut()
        .ok_or("missing knob debugger")?
        .journal
        .restoration_guard(guard)?;
    let debugger = context
        .debugger
        .as_mut()
        .ok_or("missing knob-dialog debugger")?;
    debugger.write_before(
        context.notification_semaphore + 56,
        context
            .activation_semaphore_count
            .as_ref()
            .ok_or("missing saved semaphore")?,
        deadline,
        || {
            remaining_debug_wait(shared, inputs.panel.snapshot().guest_ms, shared.3)
                .map(|_| ())
                .map_err(|e| std::io::Error::other(e.to_string()))
        },
    )?;
    remaining_debug_wait(shared, inputs.panel.snapshot().guest_ms, shared.3)?;
    debugger.resume_guest_before(deadline)?;
    context.activation_semaphore_count = None;
    context.activation_phase = 4;
    Ok(json!({"semaphore_restored":true}))
}
pub(super) fn activation_assert(
    context: &mut PreflightContext,
) -> Result<Value, Box<dyn std::error::Error>> {
    if context.activation_phase != 4 {
        return Err("activation assertion out of order".into());
    }
    let sample = context
        .activation_snapshot
        .take()
        .ok_or("missing activation snapshot")?;
    if sample.failed {
        return Err(format!("dialog first use failed: state={:02x?}, slot5={:#010x}, heap={:?}, notification_lock_still_held={}, {}",sample.state,sample.slot,sample.heap,sample.held,sample.diagnostic.as_deref().unwrap_or("diagnostics unavailable")).into());
    }
    context.activation_phase = 0;
    context.activation_started = None;
    context.next = 7;
    Ok(
        json!({"dialog_without_notification_lock":true,"state":sample.state,"callback":sample.slot,"heap":sample.heap,"notification_lock_still_held":sample.held}),
    )
}

#[derive(Clone)]
struct ActiveSnapshot {
    active_slot5: u32,
    heap_live: Vec<u32>,
    state_heap_bytes: u32,
    hook_task: u32,
    callback_task: u32,
    refreshed: [u8; 1024],
}

#[derive(Clone)]
struct OpenedOverlay {
    frame: [u8; 1024],
    callback: u32,
}

fn opened_overlay_matches(state: &[u8], slot: &[u8], expected_callback: u32) -> bool {
    state.len() == 4
        && u16::from_le_bytes([state[0], state[1]]) == 0x4b4e
        && slot == expected_callback.to_le_bytes()
}

fn validate_opened_overlay(
    session: &Session,
    debugger: &mut KnobDebugSession,
    opened: &OpenedOverlay,
) -> Result<(), String> {
    let qmp = &session.engine.management[0];
    paused_observation(
        || qmp.stop().map_err(|error| error.to_string()),
        || {
            let state = qmp
                .read_memory(0, l6max::gui_bitmap_table_entry_0, 4)
                .map_err(|error| error.to_string())?;
            let slot = qmp
                .read_memory(0, l6max::gui_display_callback_table + 20, 4)
                .map_err(|error| error.to_string())?;
            if !opened_overlay_matches(&state, &slot, opened.callback) {
                return Err(format!(
                    "knob-dialog state or callback changed between steps: state={state:02x?}, slot={slot:02x?}, expected={:#010x}",
                    opened.callback
                ));
            }
            Ok(())
        },
        || debugger.resume_guest().map_err(|error| error.to_string()),
    )
}

struct ActiveDisplayEvidence {
    slots: Option<[u32; 6]>,
}

pub(super) fn expecting_active_display(context: &PreflightContext) -> bool {
    context.active_display.is_some()
}

pub(super) fn begin_active_display(
    options: &Options,
    context: &mut PreflightContext,
) -> Result<Value, Box<dyn std::error::Error>> {
    if context.next != 7 || context.active_display.is_some() {
        return Err("knob-dialog active display step is out of order".into());
    }
    let main = selected_main_image(options).map_err(std::io::Error::other)?;
    if main.get(0x40c..0x410) != Some(&0x8007eea9u32.to_le_bytes()) {
        return Err("patched startup hook does not reserve popup RAM".into());
    }
    context.active_display = Some(ActiveDisplayEvidence { slots: None });
    Ok(json!({"guarded":true}))
}

pub(super) fn sample_active_callbacks(
    session: &Session,
    context: &mut PreflightContext,
) -> Result<Value, Box<dyn std::error::Error>> {
    let control = l6ctl::OwnedControl::new(&session.engine);
    let slots = control.knob_popup_reader()?.callback_slots()?;
    context
        .active_display
        .as_mut()
        .ok_or("missing active display scope")?
        .slots = Some(slots);
    Ok(json!({"slots":slots}))
}

pub(super) fn check_active_callbacks(
    context: &PreflightContext,
) -> Result<Value, Box<dyn std::error::Error>> {
    let slots = context
        .active_display
        .as_ref()
        .ok_or("missing active display scope")?
        .slots
        .ok_or("missing callback snapshot")?;
    if slots[5] & 1 == 0 || slots[0] != 0 || slots[2] != 0 {
        return Err(format!("first knob event did not claim only free callback slot 5; slots0/2/5={:#010x}/{:#010x}/{:#010x}",slots[0],slots[2],slots[5]).into());
    }
    Ok(json!({"callback":slots[5]}))
}

pub(super) fn check_active_frame(
    frame: &[u8; 1024],
    baseline: &[u8; 1024],
) -> Result<Value, Box<dyn std::error::Error>> {
    if frame == baseline {
        return Err("custom overlay did not change the LCD".into());
    }
    Ok(json!({"display_changed":true}))
}

pub(super) fn finish_active_display(
    context: &mut PreflightContext,
    frame: [u8; 1024],
) -> Result<Value, Box<dyn std::error::Error>> {
    let slots = context
        .active_display
        .take()
        .ok_or("missing active display scope")?
        .slots
        .ok_or("missing callback snapshot")?;
    context.opened = Some(OpenedOverlay {
        frame,
        callback: slots[5],
    });
    context.next = 8;
    Ok(json!({"display_changed":true,"callback":slots[5]}))
}

pub(super) fn level_phase(context: &PreflightContext) -> Option<u8> {
    context.level_phase
}

pub(super) fn begin_level_demo(
    session: &Session,
    context: &mut PreflightContext,
) -> Result<Value, Box<dyn std::error::Error>> {
    if context.next != 8 || context.level_phase.is_some() {
        return Err("knob-dialog level demo step is out of order".into());
    }
    validate_opened_overlay(
        session,
        context
            .debugger
            .as_mut()
            .ok_or("missing knob-dialog debugger")?,
        context.opened.as_ref().ok_or("missing active overlay")?,
    )?;
    context.level_phase = Some(0);
    Ok(json!({"overlay_validated":true}))
}

pub(super) fn note_level_capture(
    context: &mut PreflightContext,
) -> Result<(), Box<dyn std::error::Error>> {
    context.level_phase = match context.level_phase {
        Some(2) => Some(3),
        Some(5) => Some(6),
        _ => return Err("level capture out of order".into()),
    };
    Ok(())
}

pub(super) fn finish_level_demo(
    context: &mut PreflightContext,
) -> Result<Value, Box<dyn std::error::Error>> {
    if context.level_phase != Some(6) {
        return Err("level demo finish out of order".into());
    }
    context.level_phase = None;
    context.next = 9;
    Ok(
        json!({"level_demo_peak_at_least_percent":context.level_peak.take().ok_or("missing level peak")?,"level_demo_final_percent":0}),
    )
}

#[derive(Clone, Copy)]
struct TaskContexts {
    hook_task: u32,
    callback_task: u32,
}

fn verify_distinct_task_contexts(
    hook_task: u32,
    callback_task: u32,
) -> Result<TaskContexts, String> {
    if hook_task == callback_task {
        return Err("mixer hook and DisplayCycle2 callback unexpectedly share a task; re-evaluate the atomic mailbox".into());
    }
    Ok(TaskContexts {
        hook_task,
        callback_task,
    })
}

struct TaskProbe {
    hook_entry: u32,
    callback_entry: u32,
    deadline: Option<Instant>,
    hook_task: Option<u32>,
    callback_task: Option<u32>,
    capture_ready: bool,
}

pub(super) fn expecting_task_probe(context: &PreflightContext) -> bool {
    context.task_probe.is_some()
}
pub(super) fn expecting_task_capture(context: &PreflightContext) -> bool {
    context
        .task_probe
        .as_ref()
        .is_some_and(|probe| probe.capture_ready)
}

pub(super) fn begin_task_probe(
    session: &Session,
    context: &mut PreflightContext,
) -> Result<Value, Box<dyn std::error::Error>> {
    if context.next != 9 || context.task_probe.is_some() {
        return Err("knob-dialog task contexts step is out of order".into());
    }
    let opened = context.opened.as_ref().ok_or("missing active overlay")?;
    let debugger = context
        .debugger
        .as_mut()
        .ok_or("missing knob-dialog debugger")?;
    validate_opened_overlay(session, debugger, opened)?;
    const KNOB_CALL_SITE: u32 = 0x8003_6c74;
    let call = session.engine.management[0].read_memory(0, KNOB_CALL_SITE, 4)?;
    let hook_entry = thumb_bl_target(
        KNOB_CALL_SITE,
        call.try_into()
            .map_err(|_| "patched mixer call is not four bytes")?,
    )
    .ok_or("patched mixer dispatcher call is not a Thumb BL")?;
    context.task_probe = Some(TaskProbe {
        hook_entry,
        callback_entry: opened.callback & !1,
        deadline: None,
        hook_task: None,
        callback_task: None,
        capture_ready: false,
    });
    Ok(json!({"hook_entry":hook_entry,"callback_entry":opened.callback & !1}))
}

pub(super) fn arm_task_hook(
    session: &Session,
    context: &mut PreflightContext,
) -> Result<Value, Box<dyn std::error::Error>> {
    let entry = context
        .task_probe
        .as_ref()
        .ok_or("missing task probe")?
        .hook_entry;
    let debugger = context
        .debugger
        .as_mut()
        .ok_or("missing knob-dialog debugger")?;
    session.engine.management[0].stop()?;
    debugger.breakpoint(entry, true)?;
    debugger.resume_guest()?;
    Ok(json!({"hook_entry":entry}))
}

pub(super) fn mark_task_hook_input(
    context: &mut PreflightContext,
) -> Result<(), Box<dyn std::error::Error>> {
    context
        .task_probe
        .as_mut()
        .ok_or("missing task probe")?
        .deadline = Some(Instant::now() + Duration::from_secs(10));
    Ok(())
}

fn task_wait_expired(
    now: Instant,
    deadline: Instant,
    guest: u32,
    shared: (Instant, u32, u32, u64, u16),
) -> bool {
    now >= deadline
        || super::execute::deadline_exceeded(
            now.duration_since(shared.0),
            shared.1,
            guest,
            shared.2,
            shared.3,
        )
}

fn wait_task_stop(
    session: &Session,
    deadline: Instant,
    shared: (Instant, u32, u32, u64, u16),
    timeout: &'static str,
) -> Result<(), Box<dyn std::error::Error>> {
    let qmp = &session.engine.management[0];
    while qmp.query_status()?.running {
        session.poll()?;
        let now = Instant::now();
        let guest = session
            .engine
            .inputs
            .as_ref()
            .ok_or("missing inputs")?
            .panel
            .snapshot()
            .guest_ms;
        if task_wait_expired(now, deadline, guest, shared) {
            if now >= deadline {
                return Err(timeout.into());
            }
            return Err("knob-dialog shared deadline exceeded during task probe".into());
        }
        thread::sleep(Duration::from_millis(5));
    }
    // Without a recorded breakpoint stop time, a delayed waiter cannot prove
    // the stop happened before its deadline.
    let now = Instant::now();
    let guest = session
        .engine
        .inputs
        .as_ref()
        .ok_or("missing inputs")?
        .panel
        .snapshot()
        .guest_ms;
    if task_wait_expired(now, deadline, guest, shared) {
        if now >= deadline {
            return Err(timeout.into());
        }
        return Err("knob-dialog shared deadline exceeded during task probe".into());
    }
    Ok(())
}

pub(super) fn wait_task_hook(
    session: &Session,
    context: &mut PreflightContext,
    reader: &l6max_host::freertos::CurrentTaskReader,
    shared: (Instant, u32, u32, u64, u16),
) -> Result<Value, Box<dyn std::error::Error>> {
    let probe = context.task_probe.as_mut().ok_or("missing task probe")?;
    wait_task_stop(
        session,
        probe.deadline.ok_or("missing hook deadline")?,
        shared,
        "knob hook task-context breakpoint timed out",
    )?;
    let pc = context
        .debugger
        .as_mut()
        .ok_or("missing knob-dialog debugger")?
        .read_register(15)?
        & !1;
    if pc != probe.hook_entry {
        return Err(format!(
            "task probe stopped at {pc:#010x}, not knob hook {:#010x}",
            probe.hook_entry
        )
        .into());
    }
    let task = reader.read()?;
    probe.hook_task = Some(task);
    Ok(json!({"hook_task":task}))
}

pub(super) fn arm_task_callback(
    context: &mut PreflightContext,
) -> Result<Value, Box<dyn std::error::Error>> {
    let probe = context.task_probe.as_mut().ok_or("missing task probe")?;
    let debugger = context
        .debugger
        .as_mut()
        .ok_or("missing knob-dialog debugger")?;
    debugger.breakpoint(probe.hook_entry, false)?;
    debugger.breakpoint(probe.callback_entry, true)?;
    debugger.resume_guest()?;
    probe.deadline = Some(Instant::now() + Duration::from_secs(10));
    Ok(json!({"callback_entry":probe.callback_entry}))
}

pub(super) fn wait_task_callback(
    session: &Session,
    context: &mut PreflightContext,
    reader: &l6max_host::freertos::CurrentTaskReader,
    shared: (Instant, u32, u32, u64, u16),
) -> Result<Value, Box<dyn std::error::Error>> {
    let probe = context.task_probe.as_mut().ok_or("missing task probe")?;
    wait_task_stop(
        session,
        probe.deadline.ok_or("missing callback deadline")?,
        shared,
        "DisplayCycle2 task-context breakpoint timed out",
    )?;
    let pc = context
        .debugger
        .as_mut()
        .ok_or("missing knob-dialog debugger")?
        .read_register(15)?
        & !1;
    if pc != probe.callback_entry {
        return Err(format!("task probe stopped at {pc:#010x}, not DisplayCycle2 callback").into());
    }
    let task = reader.read()?;
    probe.callback_task = Some(task);
    Ok(json!({"callback_task":task}))
}

pub(super) fn assert_task_distinct(
    context: &PreflightContext,
) -> Result<Value, Box<dyn std::error::Error>> {
    let probe = context.task_probe.as_ref().ok_or("missing task probe")?;
    let hook = probe.hook_task.ok_or("missing hook task")?;
    let callback = probe.callback_task.ok_or("missing callback task")?;
    verify_distinct_task_contexts(hook, callback)?;
    Ok(json!({"mixer_task":hook,"display_task":callback,"tasks_differ":true}))
}

pub(super) fn release_task_probe(
    context: &mut PreflightContext,
) -> Result<Value, Box<dyn std::error::Error>> {
    let probe = context.task_probe.as_mut().ok_or("missing task probe")?;
    let debugger = context
        .debugger
        .as_mut()
        .ok_or("missing knob-dialog debugger")?;
    debugger.breakpoint(probe.callback_entry, false)?;
    debugger.resume_guest()?;
    probe.capture_ready = true;
    Ok(json!({"released":true}))
}

pub(super) fn finish_task_probe(
    context: &mut PreflightContext,
) -> Result<Value, Box<dyn std::error::Error>> {
    let probe = context.task_probe.take().ok_or("missing task probe")?;
    let hook = probe.hook_task.ok_or("missing hook task")?;
    let callback = probe.callback_task.ok_or("missing callback task")?;
    context.task_contexts = Some(verify_distinct_task_contexts(hook, callback)?);
    context.next = 10;
    Ok(json!({"mixer_task":hook,"display_task":callback,"tasks_differ":true}))
}

struct OverlayEvidence {
    descriptors: Option<l6ctl::OverlayDescriptors>,
    descriptor_mismatch: bool,
}

pub(super) fn begin_overlay_invariants(
    session: &Session,
    options: &Options,
    context: &mut PreflightContext,
) -> Result<(), Box<dyn std::error::Error>> {
    if context.next != 10 {
        return Err("knob-dialog active overlay step is out of order".into());
    }
    let main = selected_main_image(options).map_err(std::io::Error::other)?;
    if main.get(0x40c..0x410) != Some(&0x8007eea9u32.to_le_bytes()) {
        return Err("patched startup hook does not reserve popup RAM".into());
    }
    let opened = context.opened.as_ref().ok_or("missing active overlay")?;
    validate_opened_overlay(
        session,
        context
            .debugger
            .as_mut()
            .ok_or("missing knob-dialog debugger")?,
        opened,
    )?;
    context.task_contexts.ok_or("missing task contexts")?;
    context.overlay_check = Some(OverlayEvidence {
        descriptors: None,
        descriptor_mismatch: false,
    });
    Ok(())
}

pub(super) fn check_overlay_window(
    context: &PreflightContext,
    reader: &l6max_host::knob_popup::OverlayReader,
) -> Result<(), Box<dyn std::error::Error>> {
    if reader.window()? != context.original_window {
        return Err("custom overlay replaced the active firmware window".into());
    }
    Ok(())
}

pub(super) fn snapshot_overlay_descriptors(
    context: &mut PreflightContext,
    reader: &l6max_host::knob_popup::OverlayReader,
) -> Result<(), Box<dyn std::error::Error>> {
    let snapshot = reader.overlay_descriptors(
        context.widget7,
        context.widget33,
        [
            context.background10,
            context.background37,
            context.background48,
        ],
    )?;
    context
        .overlay_check
        .as_mut()
        .ok_or("missing overlay evidence")?
        .descriptors = Some(snapshot);
    Ok(())
}

pub(super) fn check_overlay_descriptor(
    context: &mut PreflightContext,
    field: crate::scenario::OverlayDescriptorField,
) -> Result<Value, Box<dyn std::error::Error>> {
    use crate::scenario::OverlayDescriptorField as F;
    let evidence = context
        .overlay_check
        .as_ref()
        .ok_or("missing overlay evidence")?;
    let shown = evidence
        .descriptors
        .as_ref()
        .ok_or("missing overlay descriptors")?;
    let matches = match field {
        F::Widget7 => shown.widget7 == context.original_widget7,
        F::Widget33 => shown.widget33 == context.original_widget33,
        F::Background10 => shown.backgrounds[0] == context.original_background_layers[0],
        F::Background37 => shown.backgrounds[1] == context.original_background_layers[1],
        F::Background48 => shown.backgrounds[2] == context.original_background_layers[2],
    };
    context.overlay_check.as_mut().unwrap().descriptor_mismatch |= !matches;
    Ok(json!({"field":format!("{field:?}"),"matches":matches}))
}

pub(super) fn finish_overlay_descriptors(
    context: &PreflightContext,
) -> Result<(), Box<dyn std::error::Error>> {
    let evidence = context
        .overlay_check
        .as_ref()
        .ok_or("missing overlay evidence")?;
    let shown = evidence
        .descriptors
        .as_ref()
        .ok_or("missing overlay descriptors")?;
    if evidence.descriptor_mismatch {
        let shown = (&shown.widget7, &shown.widget33, shown.backgrounds.to_vec());
        let stock = (
            &context.original_widget7,
            &context.original_widget33,
            context.original_background_layers.to_vec(),
        );
        return Err(format!("custom overlay kept borrowed descriptors after drawing: widgets and background layers are {shown:02x?}, stock {stock:02x?}").into());
    }
    Ok(())
}

pub(super) fn check_overlay_resource(
    context: &PreflightContext,
    reader: &l6max_host::knob_popup::OverlayReader,
) -> Result<(), Box<dyn std::error::Error>> {
    if reader.shared_resource_pointer(context.text_table)?
        != context.shared_resource_pointer.to_le_bytes()
    {
        return Err("dialog redirected the shared text-resource pointer".into());
    }
    Ok(())
}

fn overlay_corners_black(frame: &[u8; 1024]) -> bool {
    [(0usize, 0usize), (127, 0), (0, 63), (127, 63)]
        .into_iter()
        .all(|(x, y)| {
            let index = ((63 - y) / 8) * 128 + x;
            frame[index] & (1 << ((63 - y) % 8)) == 0
        })
}
pub(super) fn check_overlay_corners(
    context: &PreflightContext,
) -> Result<(), Box<dyn std::error::Error>> {
    if !overlay_corners_black(
        &context
            .opened
            .as_ref()
            .ok_or("missing active overlay")?
            .frame,
    ) {
        return Err("stock bitmap 22 did not paint the overlay background black".into());
    }
    Ok(())
}

pub(super) fn check_overlay_signature(
    reader: &l6max_host::knob_popup::OverlayReader,
) -> Result<(), Box<dyn std::error::Error>> {
    if reader.state_signature()? != 0x4b4eu16.to_le_bytes() {
        return Err("in-place state signature or stock bitmap-36 descriptor is invalid".into());
    }
    Ok(())
}
pub(super) fn check_overlay_bitmap(
    context: &PreflightContext,
    reader: &l6max_host::knob_popup::OverlayReader,
) -> Result<(), Box<dyn std::error::Error>> {
    if reader.bitmap36()? != context.original_bitmap {
        return Err("in-place state signature or stock bitmap-36 descriptor is invalid".into());
    }
    Ok(())
}

pub(super) fn check_overlay_heap(
    session: &Session,
    context: &mut PreflightContext,
) -> Result<Value, Box<dyn std::error::Error>> {
    let control = l6ctl::OwnedControl::new(&session.engine);
    let counters = control.heap_counter_reader()?.read()?;
    let heap_live = vec![
        counters.free,
        counters.minimum_ever_free,
        counters.allocations,
        counters.frees,
    ];
    let before = &context.heap_before;
    if heap_live[0] != before[0] || heap_live[2] != before[2] || heap_live[3] != before[3] {
        return Err(format!(
            "dialog state should not allocate heap: before={before:?}, live={heap_live:?}"
        )
        .into());
    }
    let consumed = before[0]
        .checked_sub(heap_live[0])
        .ok_or("firmware heap free-byte count increased while allocating dialog state")?;
    if consumed != 0 {
        return Err(format!("dialog state should not consume heap bytes, got {consumed}").into());
    }
    context.overlay_heap = Some((heap_live, consumed));
    Ok(json!({"heap_bytes":consumed}))
}

pub(super) fn finish_overlay_invariants(
    context: &mut PreflightContext,
) -> Result<Value, Box<dyn std::error::Error>> {
    if context.overlay_heap.is_none() {
        return Err("missing overlay heap evidence".into());
    }
    context.overlay_check = None;
    context.next = 11;
    Ok(json!({"overlay_invariants":true}))
}

// The dialog lasts 20 callback ticks. Require at least 15 immediately before
// the baseline and at least 10 while stopped before the injected draw.
const BITMAP_BASELINE_MIN_TICKS: u8 = 15;
const BITMAP_DRAW_MIN_TICKS: u8 = 10;

fn require_bitmap_countdown(ticks: u8, minimum: u8) -> Result<(), String> {
    if ticks < minimum {
        Err(format!(
            "bitmap overlay countdown too low: {ticks}, need {minimum}"
        ))
    } else {
        Ok(())
    }
}

struct BitmapProbe {
    before: Option<[u8; 1024]>,
    deadline: Option<Instant>,
    capture_phase: u8,
}

pub(super) fn bitmap_capture_phase(context: &PreflightContext) -> Option<u8> {
    context.bitmap_probe.as_ref().map(|p| p.capture_phase)
}

pub(super) fn begin_bitmap_probe(
    session: &Session,
    options: &Options,
    context: &mut PreflightContext,
) -> Result<Value, Box<dyn std::error::Error>> {
    if context.next != 11 || context.bitmap_probe.is_some() {
        return Err("knob-dialog active overlay step is out of order".into());
    }
    let main = selected_main_image(options).map_err(std::io::Error::other)?;
    if main.get(0x40c..0x410) != Some(&0x8007eea9u32.to_le_bytes()) {
        return Err("patched startup hook does not reserve popup RAM".into());
    }
    let opened = context.opened.as_ref().ok_or("missing active overlay")?;
    validate_opened_overlay(
        session,
        context
            .debugger
            .as_mut()
            .ok_or("missing knob-dialog debugger")?,
        opened,
    )?;
    context.task_contexts.ok_or("missing task contexts")?;
    context.bitmap_probe = Some(BitmapProbe {
        before: None,
        deadline: None,
        capture_phase: 0,
    });
    Ok(json!({"validated":true}))
}

pub(super) fn check_bitmap_background(
    session: &Session,
    context: &mut PreflightContext,
    reader: &l6max_host::knob_popup::OverlayReader,
    background: u32,
    expected: u16,
    shared: (Instant, u32, u32, u64, u16),
) -> Result<Value, Box<dyn std::error::Error>> {
    let deadline = shared.0 + Duration::from_secs(shared.3);
    let check = || notification_observation_check(session, shared, shared.3);
    if reader.background_bitmap_id_before_checked(background, deadline, &check)? != expected {
        return Err(format!("background {background} no longer uses bitmap ID {expected}").into());
    }
    let ticks = reader.countdown_ticks_before_checked(deadline, &check)?;
    require_bitmap_countdown(ticks, BITMAP_BASELINE_MIN_TICKS)?;
    context
        .bitmap_probe
        .as_mut()
        .ok_or("missing bitmap probe")?
        .capture_phase = 1;
    Ok(json!({"background":background,"bitmap_id":expected,"countdown_ticks":ticks}))
}

pub(super) fn note_bitmap_capture(
    context: &mut PreflightContext,
) -> Result<(), Box<dyn std::error::Error>> {
    let probe = context
        .bitmap_probe
        .as_mut()
        .ok_or("missing bitmap probe")?;
    probe.capture_phase = match probe.capture_phase {
        1 => 2,
        6 => 7,
        _ => return Err("bitmap capture out of order".into()),
    };
    Ok(())
}

pub(super) fn store_bitmap_before(
    context: &mut PreflightContext,
    frame: [u8; 1024],
) -> Result<Value, Box<dyn std::error::Error>> {
    let probe = context
        .bitmap_probe
        .as_mut()
        .ok_or("missing bitmap probe")?;
    if probe.capture_phase != 2 {
        return Err("bitmap before frame out of order".into());
    }
    probe.before = Some(frame);
    probe.capture_phase = 3;
    Ok(json!({"before_captured":true}))
}

pub(super) fn arm_bitmap_callback(
    session: &Session,
    context: &mut PreflightContext,
) -> Result<Value, Box<dyn std::error::Error>> {
    let entry = context
        .opened
        .as_ref()
        .ok_or("missing active overlay")?
        .callback
        & !1;
    let debugger = context
        .debugger
        .as_mut()
        .ok_or("missing knob-dialog debugger")?;
    session.engine.management[0].stop()?;
    debugger.breakpoint(entry, true)?;
    debugger.resume_guest()?;
    let probe = context
        .bitmap_probe
        .as_mut()
        .ok_or("missing bitmap probe")?;
    probe.deadline = Some(Instant::now() + Duration::from_secs(15));
    probe.capture_phase = 4;
    Ok(json!({"callback_entry":entry}))
}

pub(super) fn wait_bitmap_callback(
    session: &Session,
    context: &mut PreflightContext,
    shared: (Instant, u32, u32, u64, u16),
) -> Result<Value, Box<dyn std::error::Error>> {
    let probe = context
        .bitmap_probe
        .as_mut()
        .ok_or("missing bitmap probe")?;
    wait_task_stop(
        session,
        probe.deadline.ok_or("missing bitmap deadline")?,
        shared,
        "dialog callback breakpoint was not reached",
    )?;
    probe.capture_phase = 5;
    Ok(json!({"callback_stopped":true}))
}

pub(super) fn invoke_bitmap_probe(
    session: &Session,
    context: &mut PreflightContext,
    background: u32,
    shared: (Instant, u32, u32, u64, u16),
) -> Result<Value, Box<dyn std::error::Error>> {
    let entry = context
        .opened
        .as_ref()
        .ok_or("missing active overlay")?
        .callback
        & !1;
    let debugger = context
        .debugger
        .as_mut()
        .ok_or("missing knob-dialog debugger")?;
    debugger.breakpoint(entry, false)?;
    let inputs = session.engine.inputs.as_ref().ok_or("missing inputs")?;
    remaining_debug_wait(shared, inputs.panel.snapshot().guest_ms, 5)?;
    let deadline = shared.0 + Duration::from_secs(shared.3);
    let check = || notification_observation_check(session, shared, shared.3);
    let reader = l6max_host::knob_popup::overlay_reader_before_checked(
        &session.engine.management[0],
        l6fw::Model::L6max,
        deadline,
        &check,
    )?;
    let ticks = reader.countdown_ticks_before_checked(deadline, &check)?;
    remaining_debug_wait(shared, inputs.panel.snapshot().guest_ms, 5)?;
    require_bitmap_countdown(ticks, BITMAP_DRAW_MIN_TICKS)?;
    let timeout = remaining_debug_wait(shared, inputs.panel.snapshot().guest_ms, 5)?;
    debugger.call_thumb(l6max::gui_draw_background, &[background], timeout)?;
    remaining_debug_wait(shared, inputs.panel.snapshot().guest_ms, 5)?;
    debugger.resume_guest()?;
    context
        .bitmap_probe
        .as_mut()
        .ok_or("missing bitmap probe")?
        .capture_phase = 6;
    Ok(json!({"background":background,"countdown_ticks":ticks}))
}

pub(super) fn check_bitmap_frame(
    context: &PreflightContext,
    after: &[u8; 1024],
) -> Result<Value, Box<dyn std::error::Error>> {
    if context
        .bitmap_probe
        .as_ref()
        .ok_or("missing bitmap probe")?
        .before
        .as_ref()
        .ok_or("missing bitmap before frame")?
        != after
    {
        return Err(
            "bitmap ID 0 did not safely short-circuit while holding the state handle".into(),
        );
    }
    Ok(json!({"frame_unchanged":true}))
}

pub(super) fn check_bitmap_signature(
    reader: &l6max_host::knob_popup::OverlayReader,
) -> Result<Value, Box<dyn std::error::Error>> {
    if reader.state_signature()? != 0x4b4eu16.to_le_bytes() {
        return Err(
            "bitmap ID 0 did not safely short-circuit while holding the state handle".into(),
        );
    }
    Ok(json!({"signature_valid":true}))
}

pub(super) fn check_bitmap_descriptor(
    context: &PreflightContext,
    reader: &l6max_host::knob_popup::OverlayReader,
) -> Result<Value, Box<dyn std::error::Error>> {
    if reader.bitmap36()? != context.original_bitmap {
        return Err(
            "bitmap ID 0 did not safely short-circuit while holding the state handle".into(),
        );
    }
    Ok(json!({"descriptor_unchanged":true}))
}

pub(super) fn finish_bitmap_probe(
    context: &mut PreflightContext,
) -> Result<Value, Box<dyn std::error::Error>> {
    context.bitmap_probe.take().ok_or("missing bitmap probe")?;
    context.next = 12;
    Ok(json!({"bitmap_zero_guard_safe":true}))
}

// The decomposed probes can span the two-second popup lifetime. Re-trigger
// the already-zero endpoint explicitly, then wait for the callback before
// testing the refreshed image. An endpoint guard prevents mixer-value changes.
fn validate_zero_reactivation(before: u8, delta: i32) -> Result<(), String> {
    if delta != -1 {
        return Err("unsupported overlay reactivation direction".into());
    }
    if before != 0 {
        return Err(format!(
            "overlay reactivation requires the zero level endpoint, found {before}"
        ));
    }
    Ok(())
}

fn overlay_reactivation_expired(
    start: u32,
    now: u32,
    elapsed: Duration,
    shared: (Instant, u32, u32, u64, u16),
) -> bool {
    now.wrapping_sub(start) >= 10_000
        || elapsed >= Duration::from_secs(180)
        || super::execute::deadline_exceeded(shared.0.elapsed(), shared.1, now, shared.2, shared.3)
}

pub(super) fn reactivate_zero_overlay(
    session: &Session,
    context: &mut PreflightContext,
    reader: &l6max_host::knob_popup::OverlayReader,
    delta: i32,
    shared: (Instant, u32, u32, u64, u16),
    expected_next: usize,
) -> Result<Value, Box<dyn std::error::Error>> {
    if context.next != expected_next || context.overlay_reactivation.is_some() {
        return Err("overlay reactivation out of order".into());
    }
    let deadline = shared.0 + Duration::from_secs(shared.3);
    let check = || notification_observation_check(session, shared, shared.3);
    let before = reader.level_before_checked(deadline, &check)?;
    validate_zero_reactivation(before, delta)?;
    let debugger = context
        .debugger
        .as_mut()
        .ok_or("missing knob-dialog debugger")?;
    turn_and_wait_hook_bounded(
        session,
        debugger,
        delta,
        shared.3,
        Some(shared),
        Some((reader, 0x09)),
    )?;
    let start = session
        .engine
        .inputs
        .as_ref()
        .ok_or("missing inputs")?
        .panel
        .snapshot()
        .guest_ms;
    context.overlay_reactivation = Some((start, Instant::now()));
    Ok(json!({"channel":1,"delta":delta,"raw_before":before}))
}

fn overlay_reactivation_ready(pending: u8, ticks: u8, minimum_ticks: u8) -> bool {
    pending == u8::MAX && ticks >= minimum_ticks
}

pub(super) fn wait_zero_overlay_active(
    session: &Session,
    context: &mut PreflightContext,
    reader: &l6max_host::knob_popup::OverlayReader,
    shared: (Instant, u32, u32, u64, u16),
    minimum_ticks: u8,
) -> Result<Value, Box<dyn std::error::Error>> {
    let (start, wall) = context
        .overlay_reactivation
        .ok_or("missing overlay reactivation")?;
    let inputs = session.engine.inputs.as_ref().ok_or("missing inputs")?;
    let deadline = (shared.0 + Duration::from_secs(shared.3)).min(wall + Duration::from_secs(180));
    let check = || {
        if overlay_reactivation_expired(
            start,
            inputs.panel.snapshot().guest_ms,
            wall.elapsed(),
            shared,
        ) {
            Err("zero-level overlay did not reactivate before its deadline".to_owned())
        } else {
            Ok(())
        }
    };
    loop {
        let now = inputs.panel.snapshot().guest_ms;
        if overlay_reactivation_expired(start, now, wall.elapsed(), shared) {
            return Err("zero-level overlay did not reactivate before its deadline".into());
        }
        session.poll()?;
        let pending = reader.mailbox_before_checked(deadline, &check)?;
        let ticks = if pending == u8::MAX {
            reader.countdown_ticks_before_checked(deadline, &check)?
        } else {
            0
        };
        if overlay_reactivation_ready(pending, ticks, minimum_ticks) {
            let now = inputs.panel.snapshot().guest_ms;
            if overlay_reactivation_expired(start, now, wall.elapsed(), shared) {
                return Err("zero-level overlay did not reactivate before its deadline".into());
            }
            let after = reader.level_before_checked(deadline, &check)?;
            if after != 0 {
                return Err(format!("overlay reactivation changed zero level to {after}").into());
            }
            context.overlay_reactivation = None;
            return Ok(json!({"level":after,"ticks":ticks,"mailbox":pending}));
        }
        thread::sleep(Duration::from_millis(5));
    }
}

pub(super) fn expecting_overlay_refresh(context: &PreflightContext) -> bool {
    context.overlay_refresh
}

pub(super) fn begin_refreshed_overlay(
    session: &Session,
    options: &Options,
    context: &mut PreflightContext,
) -> Result<(), Box<dyn std::error::Error>> {
    if context.next != 12 {
        return Err("knob-dialog active overlay step is out of order".into());
    }
    let main = selected_main_image(options).map_err(std::io::Error::other)?;
    if main.get(0x40c..0x410) != Some(&0x8007eea9u32.to_le_bytes()) {
        return Err("patched startup hook does not reserve popup RAM".into());
    }
    let opened = context.opened.as_ref().ok_or("missing active overlay")?;
    validate_opened_overlay(
        session,
        context
            .debugger
            .as_mut()
            .ok_or("missing knob-dialog debugger")?,
        opened,
    )?;
    context.task_contexts.ok_or("missing task contexts")?;
    if context.overlay_heap.is_none() {
        return Err("missing overlay invariants".into());
    }
    context.overlay_refresh = true;
    Ok(())
}

pub(super) fn check_refreshed_value(
    session: &Session,
    context: &PreflightContext,
) -> Result<Value, Box<dyn std::error::Error>> {
    let control = l6ctl::OwnedControl::new(&session.engine);
    let reader = control.knob_popup_reader()?;
    let text = reader.text_at_table(context.text_table)?;
    let raw = reader.level()?;
    if !knob_display_matches(9, &locked_level_expected(raw), &text) {
        return Err(format!("custom window value mismatch: {text:?}").into());
    }
    Ok(json!({"value": text, "raw": raw}))
}

fn stable_refreshed_overlay(active: &[u8; 1024], refreshed: &[u8; 1024]) -> bool {
    let pixel = |frame: &[u8; 1024], x: usize, y: usize| {
        frame[((63 - y) / 8) * 128 + x] & (1 << ((63 - y) % 8)) != 0
    };
    (0..64).all(|y| {
        (0..128).all(|x| {
            (30..97).contains(&x) && (33..64).contains(&y)
                || pixel(active, x, y) == pixel(refreshed, x, y)
        })
    })
}

pub(super) fn check_refreshed_pixels(
    context: &PreflightContext,
    refreshed: &[u8; 1024],
) -> Result<(), Box<dyn std::error::Error>> {
    let active = &context
        .opened
        .as_ref()
        .ok_or("missing active overlay")?
        .frame;
    if !stable_refreshed_overlay(active, refreshed) {
        return Err("recorder redraw changed the custom overlay outside its value widget".into());
    }
    Ok(())
}

pub(super) fn finish_refreshed_overlay(
    context: &mut PreflightContext,
    refreshed: [u8; 1024],
) -> Result<Value, Box<dyn std::error::Error>> {
    let (heap_live, state_heap_bytes) = context
        .overlay_heap
        .take()
        .ok_or("missing overlay invariants")?;
    let active_slot5 = context
        .opened
        .as_ref()
        .ok_or("missing active overlay")?
        .callback;
    let TaskContexts {
        hook_task,
        callback_task,
    } = context.task_contexts.ok_or("missing task contexts")?;
    context.active = Some(ActiveSnapshot {
        active_slot5,
        heap_live,
        state_heap_bytes,
        hook_task,
        callback_task,
        refreshed,
    });
    context.next = 13;
    context.overlay_refresh = false;
    Ok(
        json!({"active_overlay":true,"callback":active_slot5,"heap_bytes":state_heap_bytes,"mixer_task":hook_task,"display_task":callback_task}),
    )
}

fn locked_level_expected(raw: u8) -> String {
    format!("{}%", (raw as u32 * 100 + 63) / 127)
}
fn locked_update_ready(pending: &[u8], displayed: Option<&str>, raw: u8) -> bool {
    pending == u8::MAX.to_le_bytes()
        && displayed
            .is_some_and(|value| knob_display_matches(9, &locked_level_expected(raw), value))
}

struct LockedUpdate {
    count: Vec<u8>,
    guest_start: Option<u32>,
    wall_start: Option<Instant>,
    sample: Option<LockedSample>,
    capture_ready: bool,
}
struct LockedSample {
    pending: u8,
    lock_held: bool,
    text: String,
    raw: u8,
}

pub(super) fn expecting_locked_capture(context: &PreflightContext) -> bool {
    context
        .locked_update
        .as_ref()
        .is_some_and(|v| v.capture_ready)
}

pub(super) fn acquire_locked_update(
    session: &Session,
    context: &mut PreflightContext,
    _reader: &l6max_host::knob_popup::OverlayReader,
    shared: (Instant, u32, u32, u64, u16),
) -> Result<Value, Box<dyn std::error::Error>> {
    if context.next != 13 || context.locked_update.is_some() {
        return Err("knob-dialog locked update step is out of order".into());
    }
    let qmp = &session.engine.management[0];
    let debugger = context
        .debugger
        .as_mut()
        .ok_or("missing knob-dialog debugger")?;
    let inputs = session.engine.inputs.as_ref().ok_or("missing inputs")?;
    let deadline = shared.0 + Duration::from_secs(shared.3);
    remaining_debug_wait(shared, inputs.panel.snapshot().guest_ms, shared.3)?;
    qmp.stop_before(deadline)?;
    verify_stopped_semaphore(session, context.notification_semaphore, shared)?;
    let count = activation_read(session, context.notification_semaphore + 56, 4, shared)?;
    if count != 1u32.to_le_bytes() {
        remaining_debug_wait(shared, inputs.panel.snapshot().guest_ms, shared.3)?;
        debugger.resume_guest_before(deadline)?;
        return Err(
            "notification semaphore was not available before callback contention check".into(),
        );
    }
    debugger.write_before(
        context.notification_semaphore + 56,
        &0u32.to_le_bytes(),
        deadline,
        || {
            remaining_debug_wait(shared, inputs.panel.snapshot().guest_ms, shared.3)
                .map(|_| ())
                .map_err(|e| std::io::Error::other(e.to_string()))
        },
    )?;
    remaining_debug_wait(shared, inputs.panel.snapshot().guest_ms, shared.3)?;
    debugger.resume_guest_before(deadline)?;
    context.locked_update = Some(LockedUpdate {
        count,
        guest_start: None,
        wall_start: None,
        sample: None,
        capture_ready: false,
    });
    Ok(json!({"notification_lock_held":true}))
}

pub(super) fn trigger_locked_update(
    session: &Session,
    context: &mut PreflightContext,
    delta: i32,
    wall_timeout_s: u64,
) -> Result<Value, Box<dyn std::error::Error>> {
    let debugger = context
        .debugger
        .as_mut()
        .ok_or("missing knob-dialog debugger")?;
    turn_and_wait_hook(session, debugger, delta, wall_timeout_s)?;
    let guest_start = session
        .engine
        .inputs
        .as_ref()
        .ok_or("missing inputs")?
        .panel
        .snapshot()
        .guest_ms;
    let wall_start = Instant::now();
    let locked = context
        .locked_update
        .as_mut()
        .ok_or("missing locked update")?;
    locked.guest_start = Some(guest_start);
    locked.wall_start = Some(wall_start);
    Ok(json!({"delta":delta}))
}

fn locked_poll_read(
    mut level: impl FnMut() -> Result<u8, String>,
    mut mailbox: impl FnMut() -> Result<u8, String>,
    mut text: impl FnMut() -> Result<String, String>,
) -> Result<(u8, u8, Option<String>), String> {
    let raw = level()?;
    let pending = mailbox()?;
    let displayed = if pending == 255 { Some(text()?) } else { None };
    Ok((raw, pending, displayed))
}

fn locked_wait_expired(
    start: u32,
    now: u32,
    elapsed: Duration,
    guest_timeout_ms: u32,
    wall_timeout_s: u64,
) -> bool {
    now.wrapping_sub(start) >= guest_timeout_ms.min(10_000)
        || elapsed >= Duration::from_secs(wall_timeout_s.min(180))
}

pub(super) fn wait_locked_update(
    session: &Session,
    context: &PreflightContext,
    reader: &l6max_host::knob_popup::OverlayReader,
    guest_timeout_ms: u32,
    wall_timeout_s: u64,
) -> Result<Value, Box<dyn std::error::Error>> {
    let locked = context
        .locked_update
        .as_ref()
        .ok_or("missing locked update")?;
    let start = locked
        .guest_start
        .ok_or("missing locked update guest start")?;
    let wall = locked
        .wall_start
        .ok_or("missing locked update wall start")?;
    let inputs = session.engine.inputs.as_ref().ok_or("missing inputs")?;
    loop {
        session.poll()?;
        let (raw, pending, displayed) = locked_poll_read(
            || reader.level(),
            || reader.mailbox(),
            || reader.text_at_table(context.text_table),
        )?;
        let expected = locked_level_expected(raw);
        if locked_update_ready(&[pending], displayed.as_deref(), raw) {
            return Ok(json!({"raw":raw,"displayed":displayed}));
        }
        if locked_wait_expired(
            start,
            inputs.panel.snapshot().guest_ms,
            wall.elapsed(),
            guest_timeout_ms,
            wall_timeout_s,
        ) {
            return Err(format!("knob dialog did not update while the notification lock was held: pending={:02x?}, raw={raw}, displayed={displayed:?}, expected={expected:?}",[pending]).into());
        }
        thread::sleep(Duration::from_millis(5));
    }
}

pub(super) fn sample_locked_update(
    session: &Session,
    context: &mut PreflightContext,
    reader: &l6max_host::knob_popup::OverlayReader,
) -> Result<Value, Box<dyn std::error::Error>> {
    session.engine.management[0].stop()?;
    let pending = reader.mailbox()?;
    let lock_held =
        reader.notification_count(context.notification_semaphore)? == 0u32.to_le_bytes();
    let text = reader.text_at_table(context.text_table)?;
    let raw = reader.level()?;
    context
        .locked_update
        .as_mut()
        .ok_or("missing locked update")?
        .sample = Some(LockedSample {
        pending,
        lock_held,
        text,
        raw,
    });
    Ok(json!({"sampled":true}))
}

pub(super) fn restore_locked_update(
    session: &Session,
    context: &mut PreflightContext,
    shared: (Instant, u32, u32, u64, u16),
) -> Result<Value, Box<dyn std::error::Error>> {
    let count = &context
        .locked_update
        .as_ref()
        .ok_or("missing locked update")?
        .count;
    let guard = verify_stopped_semaphore(session, context.notification_semaphore, shared);
    context
        .debugger
        .as_mut()
        .ok_or("missing knob debugger")?
        .journal
        .restoration_guard(guard)?;
    let inputs = session.engine.inputs.as_ref().ok_or("missing inputs")?;
    let deadline = shared.0 + Duration::from_secs(shared.3);
    let debugger = context
        .debugger
        .as_mut()
        .ok_or("missing knob-dialog debugger")?;
    debugger.write_before(context.notification_semaphore + 56, count, deadline, || {
        remaining_debug_wait(shared, inputs.panel.snapshot().guest_ms, shared.3)
            .map(|_| ())
            .map_err(|e| std::io::Error::other(e.to_string()))
    })?;
    remaining_debug_wait(shared, inputs.panel.snapshot().guest_ms, shared.3)?;
    debugger.resume_guest_before(deadline)?;
    Ok(json!({"notification_lock_restored":true}))
}

pub(super) fn assert_locked_update(
    context: &mut PreflightContext,
) -> Result<Value, Box<dyn std::error::Error>> {
    let locked = context
        .locked_update
        .as_mut()
        .ok_or("missing locked update")?;
    let sample = locked
        .sample
        .as_ref()
        .ok_or("missing locked update sample")?;
    if sample.pending != 255
        || !sample.lock_held
        || !knob_display_matches(9, &locked_level_expected(sample.raw), &sample.text)
    {
        return Err(format!("dialog did not process the latest update while the notification semaphore was unavailable: pending={:02x?}, value={:?}",[sample.pending],sample.text).into());
    }
    locked.capture_ready = true;
    Ok(
        json!({"pending_during":[sample.pending],"lock_held":sample.lock_held,"raw":sample.raw,"displayed":sample.text}),
    )
}

pub(super) fn finish_locked_update(
    context: &mut PreflightContext,
    reader: &l6max_host::knob_popup::OverlayReader,
) -> Result<Value, Box<dyn std::error::Error>> {
    let pending = reader.mailbox()?;
    if pending != 255 {
        return Err(format!("display callback did not consume the pending update while the lock was held: pending={:02x?}",[pending]).into());
    }
    let locked = context
        .locked_update
        .take()
        .ok_or("missing locked update")?;
    let sample = locked.sample.ok_or("missing locked update sample")?;
    context.next = 14;
    Ok(
        json!({"notification_semaphore_not_required_for_refresh":true,"pending_during":[sample.pending],"pending_after":[pending],"lock_held":sample.lock_held,"raw":sample.raw,"displayed":sample.text}),
    )
}

pub(super) fn begin_knob_mode(context: &PreflightContext) -> Result<usize, String> {
    if context.next != 14 || context.mode_index >= 23 {
        return Err("knob-dialog mode scope is out of order or exceeds 23 modes".into());
    }
    Ok(context.mode_index)
}

pub(super) fn knob_mode_index(context: &PreflightContext) -> usize {
    context.mode_index
}

/// Compare the same allocation/free counters as the original mode walk.
pub(super) fn expect_mode_heap(
    context: &PreflightContext,
    current: &l6ctl::HeapCounters,
) -> Result<(), String> {
    let baseline = &context
        .active
        .as_ref()
        .ok_or("missing active scene baseline")?
        .heap_live;
    if current.allocations != baseline[2] || current.frees != baseline[3] {
        return Err(format!(
            "persistent heap allocation changed: before={baseline:?}, current={current:?}"
        ));
    }
    Ok(())
}

pub(super) fn end_knob_mode(context: &mut PreflightContext, mode: u8) {
    context.mode_index += 1;
    context.last_mode = Some(mode);
}

/// Wait for the distinct channel value to be reflected in the firmware's
/// text. The caller owns both guest and wall deadlines.
fn wait_for_channel_level(
    other: u8,
    mut sample: impl FnMut() -> Result<(u8, String), String>,
    mut expired: impl FnMut() -> bool,
    mut sleep: impl FnMut(),
) -> Result<u8, String> {
    loop {
        if expired() {
            return Err(
                "channel-2 LEVEL did not reach its expected text before the deadline".into(),
            );
        }
        let (raw, displayed) = sample()?;
        if expired() {
            return Err(
                "channel-2 LEVEL did not reach its expected text before the deadline".into(),
            );
        }
        let expected = format!("{}%", (u32::from(raw) * 100 + 63) / 127);
        let other_text = format!("{}%", (u32::from(other) * 100 + 63) / 127);
        if raw != other && expected != other_text && displayed == expected {
            return Ok(raw);
        }
        sleep();
    }
}

fn channel_level_turn_clamped(raw: u8, delta: i32) -> bool {
    (delta < 0 && raw == 0) || (delta > 0 && raw == 127)
}

fn capture_knob_frame(
    session: &Session,
    options: &Options,
    duration: u32,
    name: &str,
    guest_timeout_ms: u32,
    wall_timeout_s: u64,
) -> Result<[u8; 1024], Box<dyn std::error::Error>> {
    capture_knob_frame_checked(
        session,
        options,
        duration,
        name,
        guest_timeout_ms,
        wall_timeout_s,
        || Ok(()),
    )
}

fn capture_knob_frame_checked(
    session: &Session,
    options: &Options,
    duration: u32,
    name: &str,
    guest_timeout_ms: u32,
    wall_timeout_s: u64,
    mut check: impl FnMut() -> Result<(), Box<dyn std::error::Error>>,
) -> Result<[u8; 1024], Box<dyn std::error::Error>> {
    let inputs = session.engine.inputs.as_ref().unwrap();
    let start = inputs.panel.snapshot().guest_ms;
    let begin = Instant::now();
    let raw = loop {
        check()?;
        let data = session.poll()?;
        let elapsed = inputs.panel.snapshot().guest_ms.wrapping_sub(start);
        if elapsed >= duration {
            break data.display;
        }
        if elapsed > guest_timeout_ms || begin.elapsed() > Duration::from_secs(wall_timeout_s) {
            return Err("knob-dialog display capture exceeded its deadline".into());
        }
        thread::sleep(Duration::from_millis(5));
    };
    check()?;
    let native = image::GrayImage::from_fn(128, 64, |x, y| {
        let index = ((63 - y as usize) / 8) * 128 + x as usize;
        image::Luma([if raw[index] & (1 << ((63 - y as usize) % 8)) != 0 {
            255
        } else {
            0
        }])
    });
    if let Some(scene) = name.strip_prefix("window-scene-")
        && [
            "high", "freq", "mid", "low", "aux1", "aux2", "efx", "sub-mix", "pan", "level",
        ]
        .contains(&scene)
    {
        let path = l6max_host::paths::repository_root()
            .join(KNOB_DIALOG_REFERENCES)
            .join(format!("{scene}.png"));
        check_display_reference(&raw, &path, &options.logs, name)
            .map_err(|error| format!("knob-dialog screen {scene}: {error}"))?;
    }
    check()?;
    image::DynamicImage::ImageLuma8(native)
        .resize(768, 384, image::imageops::FilterType::Nearest)
        .save(options.logs.join(format!("{name}.png")))?;
    check()?;
    Ok(raw)
}

pub(super) fn finish_knob_dialog(
    options: &Options,
    context_slot: &mut Option<PreflightContext>,
) -> Result<Value, Box<dyn std::error::Error>> {
    let main = selected_main_image(options).map_err(std::io::Error::other)?;
    if main.get(0x40c..0x410) != Some(&0x8007eea9u32.to_le_bytes()) {
        return Err("patched startup hook does not reserve popup RAM".into());
    }
    let context = context_slot
        .as_ref()
        .ok_or("missing knob-dialog preflight")?;
    if context.next != 14
        || context.mode_index == 0
        || context.last_mode != Some(8)
        || context.final_phase != 62
        || context.alias_widget.is_none()
        || context.alias_layer.is_none()
        || context.debugger.is_none()
        || context.active.is_none()
    {
        return Err("incomplete knob-dialog preflight context".into());
    }
    let mut context = context_slot.take().unwrap();
    let active_snapshot = context.active.take().unwrap();
    let debugger = context.debugger.take().unwrap();
    let widget_layer_guard_rejected = true;
    let background_layer_guard_rejected = true;
    let callback_preflight_race_rejected = true;
    let non_recorder_guard_passed = true;
    let result = (|| -> Result<Value, Box<dyn std::error::Error>> {
        let ActiveSnapshot {
            active_slot5,
            state_heap_bytes,
            hook_task,
            callback_task,
            ..
        } = active_snapshot;
        let invalid_metadata_rejected = true;
        let invalid_tag_discarded = true;
        return Ok(json!({
            "overlay_layer": 4,
            "black_background": true,
            "notification_semaphore_not_required_for_first_use": true,
            "callback_preflight_race_rejected": callback_preflight_race_rejected,
            "notification_semaphore_not_required_for_refresh": true,
            "stock_notification_overlap": true,
            "stock_notification_expiry": true,
            "bitmap_zero_guard_safe": true,
            "timeout_return": true,
            "window_change_restore": true,
            "non_recorder_first_use_rejected": non_recorder_guard_passed,
            "dialog_active_screenshot": "knob-dialog-active.png",
            "dialog_timeout_screenshot": "knob-dialog-timeout-cleared.png",
            "resource_restore": true,
            "shared_resource_reuse": true,
            "shared_resource_alias_rejected": true,
            "scene_layer_collision_rejected": true,
            "changed_widget_layer_rejected": widget_layer_guard_rejected,
            "changed_background_layer_rejected": background_layer_guard_rejected,
            "nonzero_channel_value_matches": true,
            "invalid_mailbox_tag_discarded": invalid_tag_discarded,
            "invalid_state_signature_rejected": invalid_metadata_rejected,
            "stock_notification_bitmap_untouched": true,
            "state_allocation_requested_bytes": 0,
            "state_heap_bytes_consumed": state_heap_bytes,
            "state_retained_for_boot": true,
            "display_callback_slot": 5,
            "display_callback_target": format!("{active_slot5:#010x}"),
            "mixer_hook_task_control_block": format!("{hook_task:#010x}"),
            "display_callback_task_control_block": format!("{callback_task:#010x}"),
            "mixer_and_callback_share_task": hook_task == callback_task,
            "display_callback_remains_registered": true
        }));
    })();
    debugger.finish(result)
}

pub(super) fn enqueue_navigation_encoder(
    session: &Session,
    context: &mut PreflightContext,
    channel: u32,
    steps: i32,
) -> Result<Value, Box<dyn std::error::Error>> {
    if context.final_phase != 43 {
        return Err("navigation encoder out of order".into());
    }
    l6ctl::OwnedControl::new(&session.engine).encoder(channel, steps)?;
    context.final_phase += 1;
    Ok(json!({"channel":channel,"steps":steps}))
}

pub(super) fn enqueue_navigation_tap(
    session: &Session,
    context: &mut PreflightContext,
    target: u32,
    hold_ms: u32,
) -> Result<Value, Box<dyn std::error::Error>> {
    if context.final_phase != 46 {
        return Err("navigation tap out of order".into());
    }
    l6ctl::OwnedControl::new(&session.engine).tap(target, hold_ms)?;
    context.final_phase += 1;
    Ok(json!({"target":target,"hold_ms":hold_ms}))
}

pub(super) fn note_navigation_capture(
    context: &mut PreflightContext,
) -> Result<(), Box<dyn std::error::Error>> {
    if !matches!(context.final_phase, 44 | 47) {
        return Err("navigation capture out of order".into());
    }
    context.final_phase += 1;
    Ok(())
}

pub(super) fn note_navigation_save(
    context: &mut PreflightContext,
) -> Result<(), Box<dyn std::error::Error>> {
    if !matches!(context.final_phase, 45 | 48) {
        return Err("navigation save out of order".into());
    }
    context.final_phase += 1;
    Ok(())
}

pub(super) fn snapshot_navigation(
    session: &Session,
    context: &mut PreflightContext,
) -> Result<Value, Box<dyn std::error::Error>> {
    if context.final_phase != 49 {
        return Err("navigation snapshot out of order".into());
    }
    let pointers = l6ctl::RestorationPointers {
        bitmap_descriptor: u32::from_le_bytes(
            context
                .original_bitmap
                .clone()
                .try_into()
                .map_err(|_| "short bitmap pointer")?,
        ),
        text_table: context.text_table,
        shared_resource: context.shared_resource_pointer,
        lengths_table: context.original_lengths_table,
        widget7: context.widget7,
        widget33: context.widget33,
        backgrounds: [
            context.background10,
            context.background37,
            context.background48,
        ],
    };
    let active = context.active.as_ref().ok_or("missing active snapshot")?;
    let expected = l6ctl::NavigationSnapshot {
        window: context.original_window.clone(),
        signature: 0x4b4eu16.to_le_bytes().to_vec(),
        bitmap: context.original_bitmap.clone(),
        bitmap_descriptor: context.original_bitmap_descriptor.clone(),
        notification_semaphore: context.original_notification_semaphore.clone(),
        shared_contents: context.original_shared_resource.clone(),
        widget7: context.original_widget7.clone(),
        widget33: context.original_widget33.clone(),
        background10: context.original_background_layers[0].clone(),
        background37: context.original_background_layers[1].clone(),
        background48: context.original_background_layers[2].clone(),
        callback: active.active_slot5.to_le_bytes().to_vec(),
    };
    let control = l6ctl::OwnedControl::new(&session.engine);
    context.navigation = Some(
        control
            .knob_popup_reader()?
            .navigation(&pointers, &expected)?,
    );
    context.final_phase += 1;
    Ok(json!({"atomic":false}))
}

pub(super) fn check_navigation_field(
    context: &mut PreflightContext,
    field: crate::scenario::NavigationField,
) -> Result<Value, Box<dyn std::error::Error>> {
    use crate::scenario::NavigationField as F;
    if usize::from(context.final_phase) != 50 + field as usize {
        return Err("navigation field out of order".into());
    }
    let s = context
        .navigation
        .as_ref()
        .ok_or("missing navigation snapshot")?;
    let active = context.active.as_ref().ok_or("missing active snapshot")?;
    let (name, actual, expected): (&str, &[u8], Vec<u8>) = match field {
        F::Window => ("window", &s.window, context.original_window.clone()),
        F::Signature => (
            "state signature",
            &s.signature,
            0x4b4eu16.to_le_bytes().to_vec(),
        ),
        F::Bitmap => ("bitmap table", &s.bitmap, context.original_bitmap.clone()),
        F::BitmapDescriptor => (
            "bitmap descriptor",
            &s.bitmap_descriptor,
            context.original_bitmap_descriptor.clone(),
        ),
        F::NotificationSemaphore => (
            "notification semaphore",
            &s.notification_semaphore,
            context.original_notification_semaphore.clone(),
        ),
        F::SharedContents => (
            "shared resource contents",
            &s.shared_contents,
            context.original_shared_resource.clone(),
        ),
        F::Widget7 => ("widget 7", &s.widget7, context.original_widget7.clone()),
        F::Widget33 => ("widget 33", &s.widget33, context.original_widget33.clone()),
        F::Background10 => (
            "background layer 10",
            &s.background10,
            context.original_background_layers[0].clone(),
        ),
        F::Background37 => (
            "background layer 37",
            &s.background37,
            context.original_background_layers[1].clone(),
        ),
        F::Background48 => (
            "background layer 48",
            &s.background48,
            context.original_background_layers[2].clone(),
        ),
        F::Callback => (
            "display callback slot 5",
            &s.callback,
            active.active_slot5.to_le_bytes().to_vec(),
        ),
    };
    let matches = if field == F::Window {
        actual != expected
    } else {
        actual == expected
    };
    if !matches {
        return Err(format!(
            "window-push cleanup did not restore the dialog's borrowed resources: {name}"
        )
        .into());
    }
    context.final_phase += 1;
    Ok(json!({"field":name}))
}

struct RestorationEvidence {
    snapshot: l6ctl::RestorationSnapshot,
    mismatches: Vec<&'static str>,
}

pub(super) fn snapshot_restoration(
    session: &Session,
    context: &mut PreflightContext,
) -> Result<Value, Box<dyn std::error::Error>> {
    if context.final_phase != 25 {
        return Err("restoration snapshot out of order".into());
    }
    let pointers = l6ctl::RestorationPointers {
        bitmap_descriptor: u32::from_le_bytes(
            context
                .original_bitmap
                .clone()
                .try_into()
                .map_err(|_| "short bitmap pointer")?,
        ),
        text_table: context.text_table,
        shared_resource: context.shared_resource_pointer,
        lengths_table: context.original_lengths_table,
        widget7: context.widget7,
        widget33: context.widget33,
        backgrounds: [
            context.background10,
            context.background37,
            context.background48,
        ],
    };
    let control = l6ctl::OwnedControl::new(&session.engine);
    let snapshot = control.knob_popup_reader()?.restoration(&pointers)?;
    context.restoration = Some(RestorationEvidence {
        snapshot,
        mismatches: Vec::new(),
    });
    context.final_phase += 1;
    Ok(json!({"atomic":false}))
}

fn collect_restoration_mismatch(
    mismatches: &mut Vec<&'static str>,
    name: &'static str,
    actual: &[u8],
    expected: &[u8],
) -> bool {
    let matches = actual == expected;
    if !matches {
        mismatches.push(name);
    }
    matches
}

pub(super) fn check_restoration_field(
    context: &mut PreflightContext,
    field: crate::scenario::RestorationField,
) -> Result<Value, Box<dyn std::error::Error>> {
    use crate::scenario::RestorationField as F;
    if usize::from(context.final_phase) != 26 + field as usize {
        return Err("restoration field out of order".into());
    }
    let evidence = context
        .restoration
        .as_ref()
        .ok_or("missing restoration snapshot")?;
    let s = &evidence.snapshot;
    let active = context.active.as_ref().ok_or("missing active snapshot")?;
    let (name, actual, expected): (&'static str, &[u8], Vec<u8>) = match field {
        F::Window => ("window", &s.window, context.original_window.clone()),
        F::Signature => (
            "state signature",
            &s.signature,
            0x4b4eu16.to_le_bytes().to_vec(),
        ),
        F::Bitmap => ("bitmap table", &s.bitmap, context.original_bitmap.clone()),
        F::BitmapDescriptor => (
            "bitmap descriptor",
            &s.bitmap_descriptor,
            context.original_bitmap_descriptor.clone(),
        ),
        F::NotificationSemaphore => (
            "notification semaphore",
            &s.notification_semaphore,
            context.original_notification_semaphore.clone(),
        ),
        F::TextResources => (
            "text resource table",
            &s.text_resources,
            context.original_resources.clone(),
        ),
        F::SharedPointer => (
            "shared resource pointer",
            &s.shared_pointer,
            context.shared_resource_pointer.to_le_bytes().to_vec(),
        ),
        F::SharedContents => (
            "shared resource contents",
            &s.shared_contents,
            context.original_shared_resource.clone(),
        ),
        F::TextLengths => (
            "text lengths",
            &s.text_lengths,
            context.original_lengths.clone(),
        ),
        F::Widget7 => ("widget 7", &s.widget7, context.original_widget7.clone()),
        F::Widget33 => ("widget 33", &s.widget33, context.original_widget33.clone()),
        F::Background10 => (
            "background layer 10",
            &s.background10,
            context.original_background_layers[0].clone(),
        ),
        F::Background37 => (
            "background layer 37",
            &s.background37,
            context.original_background_layers[1].clone(),
        ),
        F::Background48 => (
            "background layer 48",
            &s.background48,
            context.original_background_layers[2].clone(),
        ),
        F::Callback => (
            "display callback slot 5",
            &s.callback,
            active.active_slot5.to_le_bytes().to_vec(),
        ),
    };
    let actual = actual.to_vec();
    let matches = collect_restoration_mismatch(
        &mut context.restoration.as_mut().unwrap().mismatches,
        name,
        &actual,
        &expected,
    );
    context.final_phase += 1;
    Ok(json!({"field":name,"matches":matches}))
}

pub(super) fn finish_restoration(
    context: &mut PreflightContext,
) -> Result<Value, Box<dyn std::error::Error>> {
    if context.final_phase != 41 {
        return Err("restoration completion out of order".into());
    }
    let evidence = context
        .restoration
        .take()
        .ok_or("missing restoration snapshot")?;
    if !evidence.mismatches.is_empty() {
        return Err(format!(
            "custom overlay did not restore borrowed resources or preserve stock bitmap 36: {:?}",
            evidence.mismatches
        )
        .into());
    }
    context.final_phase += 1;
    Ok(json!({}))
}

pub(super) fn expect_restored_heap(
    session: &Session,
    context: &mut PreflightContext,
) -> Result<Value, Box<dyn std::error::Error>> {
    if context.final_phase != 42 {
        return Err("restoration heap check out of order".into());
    }
    let baseline = &context
        .active
        .as_ref()
        .ok_or("missing active snapshot")?
        .heap_live;
    let control = l6ctl::OwnedControl::new(&session.engine);
    let current = control.heap_counter_reader()?.read()?;
    if current.free != baseline[0]
        || current.allocations != baseline[2]
        || current.frees != baseline[3]
    {
        return Err(format!(
            "in-place scene state caused heap changes: live={baseline:?}, after={current:?}"
        )
        .into());
    }
    context.final_phase += 1;
    Ok(json!({"free":current.free,"allocations":current.allocations,"frees":current.frees}))
}

struct PendingNotification {
    text_before: Vec<u8>,
    breakpoint_deadline: Option<Instant>,
    guest_start: Option<u32>,
    main_tick_start: Option<u32>,
    main_guest_start: Option<u32>,
    main_clock_updates: Option<u64>,
    frame: Option<[u8; 1024]>,
    after: Option<[u8; 1024]>,
    timeout_start: Option<u32>,
}

pub(super) fn prepare_notification(
    session: &Session,
    context: &mut PreflightContext,
) -> Result<Value, Box<dyn std::error::Error>> {
    if context.final_phase != 13 {
        return Err("notification preparation out of order".into());
    }
    let refreshed = &context
        .active
        .as_ref()
        .ok_or("missing active snapshot")?
        .refreshed;
    let lit = |y0: usize, y1: usize| {
        (y0..y1)
            .flat_map(|y| {
                (0..128).map(move |x| {
                    let index = ((63 - y) / 8) * 128 + x;
                    refreshed[index] & (1 << ((63 - y) % 8)) != 0
                })
            })
            .filter(|lit| *lit)
            .count()
    };
    if lit(0, 17) < 10 || lit(30, 48) < 5 {
        return Err("shared text resource did not render both dialog lines".into());
    }
    let text_before =
        session.engine.management[0].read_memory(0, context.shared_resource_pointer, 512)?;
    context.pending_notification = Some(PendingNotification {
        text_before,
        breakpoint_deadline: None,
        guest_start: None,
        main_tick_start: None,
        main_guest_start: None,
        main_clock_updates: None,
        frame: None,
        after: None,
        timeout_start: None,
    });
    context.final_phase += 1;
    Ok(json!({}))
}

pub(super) fn arm_notification_callback(
    session: &Session,
    context: &mut PreflightContext,
) -> Result<Value, Box<dyn std::error::Error>> {
    if context.final_phase != 14 {
        return Err("notification breakpoint out of order".into());
    }
    let callback = context
        .active
        .as_ref()
        .ok_or("missing active snapshot")?
        .active_slot5
        & !1;
    let debugger = context.debugger.as_mut().ok_or("missing knob debugger")?;
    session.engine.management[0].stop()?;
    debugger.breakpoint(callback, true)?;
    debugger.resume_guest()?;
    context
        .pending_notification
        .as_mut()
        .ok_or("missing notification context")?
        .breakpoint_deadline = Some(Instant::now() + Duration::from_secs(15));
    context.final_phase += 1;
    Ok(json!({}))
}

pub(super) fn wait_notification_callback(
    session: &Session,
    context: &mut PreflightContext,
) -> Result<Value, Box<dyn std::error::Error>> {
    if context.final_phase != 15 {
        return Err("notification callback wait out of order".into());
    }
    let callback = context
        .active
        .as_ref()
        .ok_or("missing active snapshot")?
        .active_slot5
        & !1;
    let deadline = context
        .pending_notification
        .as_ref()
        .ok_or("missing notification context")?
        .breakpoint_deadline
        .ok_or("missing breakpoint deadline")?;
    let qmp = &session.engine.management[0];
    while qmp.query_status()?.running {
        session.poll()?;
        if Instant::now() >= deadline {
            return Err("dialog callback breakpoint was not reached".into());
        }
        thread::sleep(Duration::from_millis(5));
    }
    context
        .debugger
        .as_mut()
        .ok_or("missing knob debugger")?
        .breakpoint(callback, false)?;
    context.final_phase += 1;
    Ok(json!({}))
}

pub(super) fn notification_observation_check(
    session: &Session,
    shared: (Instant, u32, u32, u64, u16),
    wall_timeout_s: u64,
) -> Result<(), String> {
    let now = session
        .engine
        .inputs
        .as_ref()
        .ok_or("missing inputs")?
        .panel
        .snapshot()
        .guest_ms;
    remaining_debug_wait(shared, now, wall_timeout_s)
        .map(|_| ())
        .map_err(|e| e.to_string())
}

pub(super) fn invoke_notification(
    session: &Session,
    context: &mut PreflightContext,
    message: u32,
    shared: (Instant, u32, u32, u64, u16),
    wall_timeout_s: u64,
) -> Result<Value, Box<dyn std::error::Error>> {
    if context.final_phase != 16 {
        return Err("notification invocation out of order".into());
    }
    let deadline = (Instant::now() + Duration::from_secs(wall_timeout_s))
        .min(shared.0 + Duration::from_secs(shared.3));
    let check = || notification_observation_check(session, shared, wall_timeout_s);
    let qmp = &session.engine.management[0];
    let tick =
        l6max_host::freertos::ticks_before_checked(qmp, l6fw::Model::L6max, deadline, &check)?;
    l6max_host::knob_popup::notification_state_before_checked(
        qmp,
        l6fw::Model::L6max,
        deadline,
        &check,
    )?;
    let main = session
        .engine
        .inputs
        .as_ref()
        .ok_or("missing inputs")?
        .main
        .snapshot();
    if !main.connected || main.clock_updates == 0 {
        return Err("MAIN virtual clock unavailable".into());
    }
    let debugger = context.debugger.as_mut().ok_or("missing knob debugger")?;
    for (index, value) in [(0, message), (15, l6max::gui_notification_show)] {
        check()?;
        debugger
            .gdb
            .set_request_timeout(deadline.saturating_duration_since(Instant::now()))?;
        let before = debugger.read_register(index)?;
        check()?;
        debugger.journal.registers.push((index, before));
        debugger
            .gdb
            .set_request_timeout(deadline.saturating_duration_since(Instant::now()))?;
        if let Err(error) = debugger.gdb.register(index, value) {
            debugger.journal.synchronized = false;
            return Err(error.into());
        }
    }
    // Keep later debugger operations' default request budget.
    debugger.gdb.set_request_timeout(Duration::from_secs(5))?;
    check()?;
    if Instant::now() >= deadline {
        return Err("notification invocation exceeded its wall deadline".into());
    }
    debugger.resume_guest_before(deadline)?;
    let now = session
        .engine
        .inputs
        .as_ref()
        .ok_or("missing inputs")?
        .panel
        .snapshot()
        .guest_ms;
    context
        .pending_notification
        .as_mut()
        .ok_or("missing notification context")?
        .guest_start = Some(now);
    context
        .pending_notification
        .as_mut()
        .unwrap()
        .main_tick_start = Some(tick);
    context
        .pending_notification
        .as_mut()
        .unwrap()
        .main_guest_start = Some(main.guest_ms);
    context
        .pending_notification
        .as_mut()
        .unwrap()
        .main_clock_updates = Some(main.clock_updates);
    context.final_phase += 1;
    Ok(json!({"message":message}))
}

#[derive(Debug, PartialEq, Eq)]
enum NotificationFrameWait {
    Changed,
    Pending,
    Expired,
}

fn notification_frame_wait(
    changed: bool,
    start: u32,
    now: u32,
    limit_ms: u32,
) -> NotificationFrameWait {
    if changed {
        NotificationFrameWait::Changed
    } else if now.wrapping_sub(start) >= limit_ms {
        NotificationFrameWait::Expired
    } else {
        NotificationFrameWait::Pending
    }
}

fn notification_visible(changed: bool, active: u8) -> bool {
    changed && active != 0
}

fn notification_cleared(
    changed: bool,
    active: u8,
    invocation_ticks: u32,
    clear_start: u32,
    now: u32,
) -> bool {
    changed
        && active == 0
        && now.wrapping_sub(invocation_ticks) != 0
        && now.wrapping_sub(clear_start) != 0
}

fn notification_clear_decision(
    ready: bool,
    start_ms: u32,
    now_ms: u32,
    limit_ms: u32,
    wall_expired: bool,
) -> NotificationFrameWait {
    if wall_expired || now_ms.wrapping_sub(start_ms) >= limit_ms {
        NotificationFrameWait::Expired
    } else if ready {
        NotificationFrameWait::Changed
    } else {
        NotificationFrameWait::Pending
    }
}

pub(super) fn wait_notification_frame(
    session: &Session,
    options: &Options,
    context: &mut PreflightContext,
    clear: bool,
    artifact: &str,
    guest_ms: u32,
    limit_ms: u32,
    guest_timeout_ms: u32,
    wall_timeout_s: u64,
    shared: (Instant, u32, u32, u64, u16),
) -> Result<Value, Box<dyn std::error::Error>> {
    if context.final_phase != if clear { 19 } else { 17 } {
        return Err("notification frame wait out of order".into());
    }
    let pending = context
        .pending_notification
        .as_ref()
        .ok_or("missing notification context")?;
    let start = pending.guest_start.ok_or("missing notification start")?;
    let main_start = pending
        .main_tick_start
        .ok_or("missing MAIN tick baseline")?;
    let main_guest_start = pending
        .main_guest_start
        .ok_or("missing MAIN clock baseline")?;
    let main_clock_updates = pending
        .main_clock_updates
        .ok_or("missing MAIN clock generation")?;
    let baseline = if clear {
        pending.frame.ok_or("missing notification frame")?
    } else {
        context
            .active
            .as_ref()
            .ok_or("missing active snapshot")?
            .refreshed
    };
    let deadline = (Instant::now() + Duration::from_secs(wall_timeout_s))
        .min(shared.0 + Duration::from_secs(shared.3));
    let check = || notification_observation_check(session, shared, wall_timeout_s);
    let qmp = &session.engine.management[0];
    let clear_start = if clear {
        Some(l6max_host::freertos::ticks_before_checked(
            qmp,
            l6fw::Model::L6max,
            deadline,
            &check,
        )?)
    } else {
        None
    };
    let clear_clock_updates = session
        .engine
        .inputs
        .as_ref()
        .ok_or("missing inputs")?
        .main
        .snapshot()
        .clock_updates;
    let frame = loop {
        let frame = capture_knob_frame_checked(
            session,
            options,
            guest_ms,
            artifact,
            guest_timeout_ms,
            wall_timeout_s,
            || {
                let panel_now = session
                    .engine
                    .inputs
                    .as_ref()
                    .ok_or("missing inputs")?
                    .panel
                    .snapshot()
                    .guest_ms;
                remaining_debug_wait(shared, panel_now, wall_timeout_s)?;
                if Instant::now() >= deadline {
                    Err("stock notification wait exceeded its wall deadline".into())
                } else {
                    Ok(())
                }
            },
        )?;
        let (active, _timer) = l6max_host::knob_popup::notification_state_before_checked(
            qmp,
            l6fw::Model::L6max,
            deadline,
            &check,
        )?;
        remaining_debug_wait(
            shared,
            session
                .engine
                .inputs
                .as_ref()
                .ok_or("missing inputs")?
                .panel
                .snapshot()
                .guest_ms,
            wall_timeout_s,
        )?;
        if clear {
            let ticks = l6max_host::freertos::ticks_before_checked(
                qmp,
                l6fw::Model::L6max,
                deadline,
                &check,
            )?;
            let main = session
                .engine
                .inputs
                .as_ref()
                .ok_or("missing inputs")?
                .main
                .snapshot();
            if !main.connected {
                return Err("MAIN virtual clock disconnected".into());
            }
            remaining_debug_wait(
                shared,
                session
                    .engine
                    .inputs
                    .as_ref()
                    .ok_or("missing inputs")?
                    .panel
                    .snapshot()
                    .guest_ms,
                wall_timeout_s,
            )?;
            if Instant::now() >= deadline {
                return Err("stock notification wait exceeded its wall deadline".into());
            }
            match notification_clear_decision(
                main.clock_updates > main_clock_updates
                    && main.clock_updates > clear_clock_updates
                    && notification_cleared(
                        frame != baseline,
                        active,
                        main_start,
                        clear_start.unwrap(),
                        ticks,
                    ),
                main_guest_start,
                main.guest_ms,
                limit_ms,
                Instant::now() >= deadline,
            ) {
                NotificationFrameWait::Changed => break frame,
                NotificationFrameWait::Expired => {
                    return Err(
                        "stock notification did not clear after its MAIN guest-time timeout".into(),
                    );
                }
                NotificationFrameWait::Pending => continue,
            }
        }
        if notification_visible(frame != baseline, active) {
            break frame;
        }
        let now = session
            .engine
            .inputs
            .as_ref()
            .ok_or("missing inputs")?
            .panel
            .snapshot()
            .guest_ms;
        if notification_frame_wait(false, start, now, limit_ms) == NotificationFrameWait::Expired {
            return Err(if clear {
                "stock notification did not clear after its timeout"
            } else {
                "stock notification did not render its frame over the custom scene"
            }
            .into());
        }
    };
    let pending = context.pending_notification.as_mut().unwrap();
    if clear {
        pending.after = Some(frame);
    } else {
        pending.frame = Some(frame);
    }
    context.final_phase += 1;
    Ok(json!({"artifact":artifact}))
}

pub(super) fn expect_notification_invariants(
    session: &Session,
    context: &mut PreflightContext,
    clear: bool,
) -> Result<Value, Box<dyn std::error::Error>> {
    if context.final_phase != if clear { 20 } else { 18 } {
        return Err("notification assertion out of order".into());
    }
    let pending = context
        .pending_notification
        .as_ref()
        .ok_or("missing notification context")?;
    let active_slot5 = context
        .active
        .as_ref()
        .ok_or("missing active snapshot")?
        .active_slot5;
    let qmp = &session.engine.management[0];
    let callback = qmp.read_memory(0, l6max::gui_display_callback_table + 20, 4)?;
    let pointer = qmp.read_memory(0, context.text_table + 179 * 4, 4)?;
    let resource = qmp.read_memory(0, context.shared_resource_pointer, 512)?;
    if callback != active_slot5.to_le_bytes()
        || pointer != context.shared_resource_pointer.to_le_bytes()
        || resource != pending.text_before
    {
        if !clear {
            return Err(format!("stock notification changed scene state: callback {callback:02x?} vs {active_slot5:08x}, resource pointer {pointer:02x?} vs {:08x}, resource modified={}", context.shared_resource_pointer, resource != pending.text_before).into());
        }
        let utf16 = |bytes: &[u8]| -> String {
            bytes
                .chunks(2)
                .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
                .take_while(|unit| *unit != 0)
                .take(24)
                .map(|unit| char::from_u32(unit.into()).unwrap_or('?'))
                .collect()
        };
        let start = pending.guest_start.ok_or("missing notification start")?;
        let now = session
            .engine
            .inputs
            .as_ref()
            .ok_or("missing inputs")?
            .panel
            .snapshot()
            .guest_ms;
        return Err(format!("stock notification expiry damaged or prematurely removed the custom scene: callback {callback:02x?} vs {active_slot5:08x}, resource pointer {pointer:02x?} vs {:08x}, resource {:?} vs {:?}, ticks {}, window {:02x?}, notification active {} busy {}, guest ms since notification {}", context.shared_resource_pointer, utf16(&resource), utf16(&pending.text_before), qmp.read_memory(0, l6max::gui_bitmap_table_entry_0 + 2, 1)?[0], qmp.read_memory(0, l6max::current_window, 4)?, qmp.read_memory(0, l6max::notification_active, 1)?[0], qmp.read_memory(0, l6max::notification_busy, 1)?[0], now.wrapping_sub(start)).into());
    }
    if clear {
        let now = session
            .engine
            .inputs
            .as_ref()
            .ok_or("missing inputs")?
            .panel
            .snapshot()
            .guest_ms;
        context
            .pending_notification
            .as_mut()
            .ok_or("missing notification context")?
            .timeout_start = Some(now);
    }
    context.final_phase += 1;
    Ok(json!({"clear":clear}))
}

fn expiry_timed_out(start: u32, now: u32, limit_ms: u32) -> bool {
    now.wrapping_sub(start) >= limit_ms
}

pub(super) fn wait_dialog_expiry(
    session: &Session,
    context: &mut PreflightContext,
    interval_ms: u32,
    limit_ms: u32,
    guest_timeout_ms: u32,
    wall_timeout_s: u64,
) -> Result<Value, Box<dyn std::error::Error>> {
    if context.final_phase != 21 {
        return Err("dialog expiry wait out of order".into());
    }
    let start = context
        .pending_notification
        .as_ref()
        .ok_or("missing notification context")?
        .timeout_start
        .ok_or("missing expiry start")?;
    let control = l6ctl::OwnedControl::new(&session.engine);
    let reader = control.knob_popup_reader()?;
    loop {
        let ticks = reader.countdown_ticks()?;
        if ticks == 0 {
            break;
        }
        let now = control.input_status(l6ctl::InputSource::Panel)?.guest_ms;
        if expiry_timed_out(start, now, limit_ms) {
            return Err(format!("custom scene timeout did not finish after notification expiry; {ticks} ticks remain").into());
        }
        wait_guest(session, now, interval_ms, guest_timeout_ms, wall_timeout_s)?;
    }
    context.final_phase += 1;
    Ok(json!({}))
}

pub(super) fn note_expiry_capture(
    context: &mut PreflightContext,
) -> Result<(), Box<dyn std::error::Error>> {
    if context.final_phase != 22 {
        return Err("dialog expiry capture out of order".into());
    }
    context.final_phase += 1;
    Ok(())
}

fn validate_expiry_frame(
    frame: &[u8; 1024],
    expected: &[u8; 1024],
    after: bool,
) -> Result<(), &'static str> {
    if frame == expected {
        Err(if after {
            "knob dialog timeout screenshot still shows the custom scene"
        } else {
            "knob dialog timeout screenshot still shows the notification"
        })
    } else {
        Ok(())
    }
}

pub(super) fn expect_expiry_frame(
    context: &mut PreflightContext,
    frame: &[u8; 1024],
    after: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    if context.final_phase != if after { 23 } else { 24 } {
        return Err("dialog expiry frame assertion out of order".into());
    }
    let pending = context
        .pending_notification
        .as_ref()
        .ok_or("missing notification context")?;
    let expected = if after {
        pending.after.ok_or("missing cleared notification frame")?
    } else {
        pending.frame.ok_or("missing notification frame")?
    };
    validate_expiry_frame(frame, &expected, after)?;
    context.final_phase += 1;
    Ok(())
}

struct PendingInvalidSignature {
    original: Vec<u8>,
    invalid: u32,
    guest_start: u32,
    wall_start: Instant,
    rejected: Option<bool>,
}

#[derive(Debug, PartialEq, Eq)]
enum SignatureObservation {
    Complete,
    Waiting,
    Expired,
}

impl PendingInvalidSignature {
    fn observation(
        &self,
        guest_now: u32,
        now: Instant,
        guest_ms: u32,
        guest_timeout_ms: u32,
        wall_timeout_s: u64,
    ) -> SignatureObservation {
        let elapsed = guest_now.wrapping_sub(self.guest_start);
        if elapsed >= guest_ms {
            SignatureObservation::Complete
        } else if elapsed > guest_timeout_ms
            || now.duration_since(self.wall_start) > Duration::from_secs(wall_timeout_s)
        {
            SignatureObservation::Expired
        } else {
            SignatureObservation::Waiting
        }
    }
}

fn settle_level_encoder_steps(before: u8) -> i32 {
    if before == 0 { -1 } else { 1 }
}

fn settled_level_matches(
    before: u8,
    level: u8,
    mut mailbox: impl FnMut() -> Result<u8, String>,
    mut text: impl FnMut() -> Result<String, String>,
) -> Result<bool, String> {
    let expected = format!("{}%", (u32::from(level) * 100 + 63) / 127);
    Ok(level != before && mailbox()? == u8::MAX && knob_display_matches(9, &expected, &text()?))
}

pub(super) fn settle_level(
    session: &Session,
    options: &Options,
    context: &mut PreflightContext,
    artifact: &str,
    guest_timeout_ms: u32,
    wall_timeout_s: u64,
) -> Result<Value, Box<dyn std::error::Error>> {
    if !matches!(context.final_phase, 7 | 12) {
        return Err("level settle out of order".into());
    }
    let control = l6ctl::OwnedControl::new(&session.engine);
    let reader = control.knob_popup_reader()?;
    let before = reader.level()?;
    control.encoder(1, settle_level_encoder_steps(before))?;
    let start = control.input_status(l6ctl::InputSource::Panel)?.guest_ms;
    loop {
        capture_knob_frame(
            session,
            options,
            50,
            artifact,
            guest_timeout_ms,
            wall_timeout_s,
        )?;
        let level = reader.level()?;
        if settled_level_matches(
            before,
            level,
            || reader.mailbox(),
            || reader.text_at_table(context.text_table),
        )? {
            break;
        }
        if control
            .input_status(l6ctl::InputSource::Panel)?
            .guest_ms
            .wrapping_sub(start)
            >= 2_000
        {
            return Err(format!(
                "channel-1 LEVEL turn was not displayed: before={before}, now={}",
                level
            )
            .into());
        }
    }
    context.final_phase += 1;
    Ok(json!({"artifact":artifact}))
}

pub(super) fn inject_invalid_signature(
    session: &Session,
    context: &mut PreflightContext,
    shared: (Instant, u32, u32, u64, u16),
) -> Result<Value, Box<dyn std::error::Error>> {
    if context.final_phase != 8 {
        return Err("signature injection out of order".into());
    }
    let qmp = &session.engine.management[0];
    let debugger = context.debugger.as_mut().ok_or("missing knob debugger")?;
    let inputs = session.engine.inputs.as_ref().ok_or("missing inputs")?;
    let deadline = shared.0 + Duration::from_secs(shared.3);
    remaining_debug_wait(shared, inputs.panel.snapshot().guest_ms, shared.3)?;
    qmp.stop_before(deadline)?;
    let address = l6max::gui_bitmap_table_entry_0;
    let original = activation_read(session, address, 4, shared)?;
    l6ctl::verify_bitmap_probe_state(
        original
            .clone()
            .try_into()
            .map_err(|_| "short bitmap state")?,
        false,
    )?;
    let invalid = u32::from_le_bytes(
        original
            .clone()
            .try_into()
            .map_err(|_| "short signature read")?,
    ) ^ 1;
    debugger.write_before(address, &invalid.to_le_bytes(), deadline, || {
        remaining_debug_wait(shared, inputs.panel.snapshot().guest_ms, shared.3)
            .map(|_| ())
            .map_err(|e| std::io::Error::other(e.to_string()))
    })?;
    remaining_debug_wait(shared, inputs.panel.snapshot().guest_ms, shared.3)?;
    debugger.resume_guest_before(deadline)?;
    let guest_start = session
        .engine
        .inputs
        .as_ref()
        .ok_or("missing inputs")?
        .panel
        .snapshot()
        .guest_ms;
    context.pending_invalid_signature = Some(PendingInvalidSignature {
        original,
        invalid,
        guest_start,
        wall_start: Instant::now(),
        rejected: None,
    });
    context.final_phase += 1;
    Ok(json!({}))
}

pub(super) fn observe_invalid_signature(
    session: &Session,
    context: &mut PreflightContext,
    guest_ms: u32,
    guest_timeout_ms: u32,
    wall_timeout_s: u64,
) -> Result<Value, Box<dyn std::error::Error>> {
    if context.final_phase != 9 {
        return Err("signature observation out of order".into());
    }
    let pending = context
        .pending_invalid_signature
        .as_ref()
        .ok_or("missing signature probe")?;
    loop {
        session.poll()?;
        let guest_now = session
            .engine
            .inputs
            .as_ref()
            .ok_or("missing inputs")?
            .panel
            .snapshot()
            .guest_ms;
        match pending.observation(
            guest_now,
            Instant::now(),
            guest_ms,
            guest_timeout_ms,
            wall_timeout_s,
        ) {
            SignatureObservation::Complete => break,
            SignatureObservation::Expired => {
                return Err("knob-dialog setup exceeded its deadline".into());
            }
            SignatureObservation::Waiting => {}
        }
        thread::sleep(Duration::from_millis(5));
    }
    context.final_phase += 1;
    Ok(json!({"guest_ms":guest_ms}))
}

pub(super) fn sample_invalid_signature(
    session: &Session,
    context: &mut PreflightContext,
) -> Result<Value, Box<dyn std::error::Error>> {
    if context.final_phase != 10 {
        return Err("signature sample out of order".into());
    }
    let pending = context
        .pending_invalid_signature
        .as_mut()
        .ok_or("missing signature probe")?;
    let qmp = &session.engine.management[0];
    qmp.stop()?;
    let state = l6max::gui_bitmap_table_entry_0;
    pending.rejected = Some(
        qmp.read_memory(0, state, 4)? == pending.invalid.to_le_bytes()
            && qmp.read_memory(0, state + 36 * 4, 4)? == context.original_bitmap,
    );
    context.final_phase += 1;
    Ok(json!({}))
}

pub(super) fn restore_invalid_signature(
    session: &Session,
    context: &mut PreflightContext,
    shared: (Instant, u32, u32, u64, u16),
) -> Result<Value, Box<dyn std::error::Error>> {
    if context.final_phase != 11 {
        return Err("signature restoration out of order".into());
    }
    let pending = context
        .pending_invalid_signature
        .as_ref()
        .ok_or("missing signature probe")?;
    let rejected = pending.rejected.ok_or("missing signature sample")?;
    let current = activation_read(session, l6max::gui_bitmap_table_entry_0, 4, shared);
    let guard = match current {
        Ok(bytes) if bytes == pending.invalid.to_le_bytes() => Ok(()),
        Ok(_) => Err("invalid bitmap signature changed before restoration".to_owned()),
        Err(error) => Err(format!("bitmap restoration guard failed: {error}")),
    };
    context
        .debugger
        .as_mut()
        .ok_or("missing knob debugger")?
        .journal
        .restoration_guard(guard)?;
    let inputs = session.engine.inputs.as_ref().ok_or("missing inputs")?;
    let deadline = shared.0 + Duration::from_secs(shared.3);
    let debugger = context.debugger.as_mut().ok_or("missing knob debugger")?;
    debugger.write_before(
        l6max::gui_bitmap_table_entry_0,
        &pending.original,
        deadline,
        || {
            remaining_debug_wait(shared, inputs.panel.snapshot().guest_ms, shared.3)
                .map(|_| ())
                .map_err(|e| std::io::Error::other(e.to_string()))
        },
    )?;
    remaining_debug_wait(shared, inputs.panel.snapshot().guest_ms, shared.3)?;
    debugger.resume_guest_before(deadline)?;
    if !rejected {
        return Err("the payload accepted an invalid state signature".into());
    }
    context.pending_invalid_signature = None;
    context.final_phase += 1;
    Ok(json!({"rejected":true}))
}

struct PendingInvalidTag {
    tag: u8,
    guest_start: u32,
    wall_deadline: Instant,
}

impl PendingInvalidTag {
    fn expired(&self, guest: u32, now: Instant, guest_timeout_ms: u32) -> bool {
        guest.wrapping_sub(self.guest_start) >= guest_timeout_ms.min(10_000)
            || now >= self.wall_deadline
    }
    fn description(&self) -> String {
        if self.tag & 0x0f >= 10 {
            format!("invalid mode {}", self.tag & 0x0f)
        } else {
            format!("invalid channel {}", self.tag >> 4)
        }
    }
}

fn read_invalid_tag_if_live<T>(
    pending: &PendingInvalidTag,
    guest: u32,
    now: Instant,
    guest_timeout_ms: u32,
    read: impl FnOnce() -> Result<T, Box<dyn std::error::Error>>,
) -> Result<T, Box<dyn std::error::Error>> {
    if pending.expired(guest, now, guest_timeout_ms) {
        return Err(format!(
            "display callback did not safely discard {}",
            pending.description()
        )
        .into());
    }
    read()
}

pub(super) fn final_phase(context: &PreflightContext) -> u8 {
    context.final_phase
}

pub(super) fn inject_invalid_tag(
    session: &Session,
    context: &mut PreflightContext,
    tag: u8,
    wall_timeout_s: u64,
    shared: (Instant, u32, u32, u64, u16),
) -> Result<Value, Box<dyn std::error::Error>> {
    if !matches!(context.final_phase, 1 | 4) {
        return Err("invalid tag injection out of order".into());
    }
    let qmp = &session.engine.management[0];
    let debugger = context.debugger.as_mut().ok_or("missing knob debugger")?;
    let inputs = session.engine.inputs.as_ref().ok_or("missing inputs")?;
    let deadline = shared.0 + Duration::from_secs(shared.3);
    remaining_debug_wait(shared, inputs.panel.snapshot().guest_ms, shared.3)?;
    qmp.stop_before(deadline)?;
    let state = activation_read(session, l6max::gui_bitmap_table_entry_0, 4, shared)?;
    l6ctl::verify_bitmap_probe_state(state.try_into().map_err(|_| "short bitmap state")?, true)?;
    let address = l6max::gui_bitmap_table_entry_0 + 3;
    debugger.write_before(address, &[tag], deadline, || {
        remaining_debug_wait(shared, inputs.panel.snapshot().guest_ms, shared.3)
            .map(|_| ())
            .map_err(|e| std::io::Error::other(e.to_string()))
    })?;
    debugger.handoff_mailbox(address);
    remaining_debug_wait(shared, inputs.panel.snapshot().guest_ms, shared.3)?;
    debugger.resume_guest_before(deadline)?;
    let guest_start = session
        .engine
        .inputs
        .as_ref()
        .ok_or("missing inputs")?
        .panel
        .snapshot()
        .guest_ms;
    context.pending_invalid_tag = Some(PendingInvalidTag {
        tag,
        guest_start,
        wall_deadline: Instant::now() + Duration::from_secs(wall_timeout_s.min(180)),
    });
    context.final_phase += 1;
    Ok(json!({"tag":tag}))
}

pub(super) fn wait_invalid_tag(
    session: &Session,
    context: &mut PreflightContext,
    guest_timeout_ms: u32,
    _wall_timeout_s: u64,
    shared: (Instant, u32, u32, u64, u16),
) -> Result<Value, Box<dyn std::error::Error>> {
    if !matches!(context.final_phase, 2 | 5) {
        return Err("invalid tag wait out of order".into());
    }
    let inputs = session.engine.inputs.as_ref().ok_or("missing inputs")?;
    let pending = context
        .pending_invalid_tag
        .as_ref()
        .ok_or("missing pending invalid tag")?;
    loop {
        remaining_debug_wait(shared, inputs.panel.snapshot().guest_ms, shared.3)?;
        session.poll()?;
        remaining_debug_wait(shared, inputs.panel.snapshot().guest_ms, shared.3)?;
        let deadline = (shared.0 + Duration::from_secs(shared.3)).min(pending.wall_deadline);
        let discarded = read_invalid_tag_if_live(
            pending,
            inputs.panel.snapshot().guest_ms,
            Instant::now(),
            guest_timeout_ms,
            || {
                Ok(session.engine.management[0].read_memory_before(
                    0,
                    l6max::gui_bitmap_table_entry_0 + 3,
                    1,
                    deadline,
                )? == [0xff])
            },
        )?;
        remaining_debug_wait(shared, inputs.panel.snapshot().guest_ms, shared.3)?;
        if pending.expired(
            inputs.panel.snapshot().guest_ms,
            Instant::now(),
            guest_timeout_ms,
        ) {
            return Err(format!(
                "display callback did not safely discard {}",
                pending.description()
            )
            .into());
        }
        if discarded {
            break;
        }
        thread::sleep(Duration::from_millis(5));
    }
    context.final_phase += 1;
    Ok(json!({}))
}

pub(super) fn confirm_invalid_tag(
    session: &Session,
    context: &mut PreflightContext,
    shared: (Instant, u32, u32, u64, u16),
) -> Result<Value, Box<dyn std::error::Error>> {
    if !matches!(context.final_phase, 3 | 6) {
        return Err("invalid tag confirmation out of order".into());
    }
    let description = context
        .pending_invalid_tag
        .as_ref()
        .ok_or("missing pending invalid tag")?
        .description();
    let qmp = &session.engine.management[0];
    let inputs = session.engine.inputs.as_ref().ok_or("missing inputs")?;
    let deadline = shared.0 + Duration::from_secs(shared.3);
    let debugger = context.debugger.as_mut().ok_or("missing knob debugger")?;
    remaining_debug_wait(shared, inputs.panel.snapshot().guest_ms, shared.3)?;
    qmp.stop_before(deadline)?;
    let discarded =
        activation_read(session, l6max::gui_bitmap_table_entry_0 + 3, 1, shared)? == [0xff];
    remaining_debug_wait(shared, inputs.panel.snapshot().guest_ms, shared.3)?;
    debugger.resume_guest_before(deadline)?;
    if !discarded {
        return Err(format!("display callback did not safely discard {description}").into());
    }
    debugger.acknowledge_mailbox();
    context.pending_invalid_tag = None;
    context.final_phase += 1;
    Ok(json!({}))
}

pub(super) fn knob_display_matches(mode: u32, expected: &str, displayed: &str) -> bool {
    if expected == displayed {
        return true;
    }
    if mode != 1 {
        return false;
    }
    let Some((expected_number, expected_unit)) = expected.rsplit_once(' ') else {
        return false;
    };
    let Some((displayed_number, displayed_unit)) = displayed.rsplit_once(' ') else {
        return false;
    };
    if expected_unit != displayed_unit {
        return false;
    }
    let (Ok(expected), Ok(displayed)) = (
        expected_number.parse::<f32>(),
        displayed_number.parse::<f32>(),
    ) else {
        return false;
    };
    let tolerance = match expected_unit {
        "Hz" => 1.0,
        "kHz" => 0.01,
        _ => return false,
    };
    (expected - displayed).abs() <= tolerance
}

/// Evidence and original overall deadlines for the popup's declarative steps.
pub(super) struct PopupContext {
    before: [u8; 1024],
    level_before: u8,
    turns: Vec<i32>,
    guest_start: u32,
    wall_deadline: Instant,
    guest_timeout_ms: u32,
}

impl PopupContext {
    fn expired(&self, now: Instant, guest: u32) -> bool {
        now > self.wall_deadline || guest.wrapping_sub(self.guest_start) > self.guest_timeout_ms
    }
}

fn popup_active(snapshot: &l6ctl::KnobPopupSnapshot) -> bool {
    u16::from_le_bytes([snapshot.state[0], snapshot.state[1]]) == 0x4b4e
        && snapshot.callback & 1 == 1
}

fn popup_opened(
    snapshot: &l6ctl::KnobPopupSnapshot,
    before: &[u8; 1024],
    after: &[u8; 1024],
    level_before: u8,
) -> bool {
    popup_active(snapshot) && after != before && snapshot.level != level_before
}

fn popup_value_matches(snapshot: &l6ctl::KnobPopupSnapshot, level_before: u8) -> bool {
    let expected = format!("{}%", (u32::from(snapshot.level) * 100 + 63) / 127);
    snapshot.state[3] == u8::MAX
        && snapshot.level != level_before
        && snapshot.text.as_deref() == Some(expected.as_str())
}

fn attempt_popup_turns(
    first_steps: i32,
    fallback_steps: i32,
    mut attempt: impl FnMut(i32) -> Result<bool, String>,
) -> Result<Vec<i32>, String> {
    let mut turns = Vec::new();
    for delta in [first_steps, fallback_steps] {
        turns.push(delta);
        if attempt(delta)? {
            break;
        }
    }
    Ok(turns)
}

impl State {
    pub(super) fn run_knob_popup(
        &mut self,
        session: &Session,
        operation: Operation,
        guest_timeout_ms: u32,
        wall_timeout_s: u64,
    ) -> Result<Value, Box<dyn std::error::Error>> {
        let inputs = session
            .engine
            .inputs
            .as_ref()
            .ok_or("knob popup needs inputs")?;
        let step_started = Instant::now();
        let step_guest = inputs.panel.snapshot().guest_ms;
        let control = l6ctl::OwnedControl::new(&session.engine);
        let reader = control.knob_popup_reader()?;
        if let Operation::KnobPopupOpen {
            first_steps,
            fallback_steps,
            attempt_guest_ms,
        } = operation
        {
            let guest_start = step_guest;
            let wall_deadline = step_started + Duration::from_secs(wall_timeout_s);
            let initial = reader.snapshot(false).map_err(std::io::Error::other)?;
            if initial.window != l6max::GUI_RECORDER_WINDOW {
                return Err(format!(
                    "knob-dialog-popup needs the recorder screen, found window {:#010x}",
                    initial.window
                )
                .into());
            }
            let before = session.poll()?.display;
            let level_before = reader.snapshot(false).map_err(std::io::Error::other)?.level;
            let mut context = PopupContext {
                before,
                level_before,
                turns: Vec::new(),
                guest_start,
                wall_deadline,
                guest_timeout_ms,
            };
            // InputHub::knob reverses panel encoder polarity; preserve that
            // exact convention while using l6ctl's one-based encoder API.
            context.turns = attempt_popup_turns(first_steps, fallback_steps, |delta| {
                if context.expired(Instant::now(), inputs.panel.snapshot().guest_ms) {
                    return Err(
                        "knob-dialog popup exceeded its shared deadline before encoder input"
                            .into(),
                    );
                }
                control
                    .encoder(1, -delta)
                    .map_err(|error| error.to_string())?;
                let attempt_start = inputs.panel.snapshot().guest_ms;
                let attempt_deadline = Instant::now() + Duration::from_secs(wall_timeout_s);
                loop {
                    let display = session.poll()?.display;
                    let snapshot = reader.snapshot(false)?;
                    if popup_opened(&snapshot, &context.before, &display, context.level_before) {
                        return Ok(true);
                    }
                    let guest = inputs.panel.snapshot().guest_ms;
                    if guest.wrapping_sub(attempt_start) >= attempt_guest_ms {
                        return Ok(false);
                    }
                    if Instant::now() >= attempt_deadline
                        || guest.wrapping_sub(attempt_start) >= guest_timeout_ms
                        || context.expired(Instant::now(), guest)
                    {
                        return Err("knob-dialog popup observation exceeded its deadline".into());
                    }
                    thread::sleep(Duration::from_millis(5));
                }
            })?;
            let turns = context.turns.clone();
            self.knob_popup = Some(context);
            return Ok(json!({"turns": turns, "level_before": level_before}));
        }
        let context = self
            .knob_popup
            .as_ref()
            .ok_or("knob popup open step has not run")?;
        if context.expired(Instant::now(), inputs.panel.snapshot().guest_ms) {
            return Err("knob-dialog popup exceeded its shared deadline".into());
        }
        match operation {
            Operation::KnobPopupExpectOpen => {
                let snapshot = reader.snapshot(false).map_err(std::io::Error::other)?;
                if !popup_active(&snapshot) {
                    return Err(format!("Knob Dialog did not open after turning channel 1 level {:?}: state={:02x?}, slot5={:#010x}", context.turns, &snapshot.state[..2], snapshot.callback).into());
                }
                if context.expired(Instant::now(), inputs.panel.snapshot().guest_ms) {
                    return Err("knob-dialog popup exceeded its shared deadline".into());
                }
                Ok(json!({"state": snapshot.state, "callback": snapshot.callback}))
            }
            Operation::KnobPopupExpectDisplay => {
                if session.poll()?.display == context.before {
                    return Err("Knob Dialog opened but the display did not change".into());
                }
                if context.expired(Instant::now(), inputs.panel.snapshot().guest_ms) {
                    return Err("knob-dialog popup exceeded its shared deadline".into());
                }
                Ok(json!({"display_changed": true}))
            }
            Operation::KnobPopupExpectValue => {
                let start = inputs.panel.snapshot().guest_ms;
                let deadline = (Instant::now() + Duration::from_secs(wall_timeout_s.min(180)))
                    .min(context.wall_deadline);
                loop {
                    session.poll()?;
                    let snapshot = reader.snapshot(true).map_err(std::io::Error::other)?;
                    let displayed = snapshot.text.as_deref().unwrap_or_default();
                    let raw = snapshot.level;
                    let expected = format!("{}%", (u32::from(raw) * 100 + 63) / 127);
                    if popup_value_matches(&snapshot, context.level_before) {
                        if context.expired(Instant::now(), inputs.panel.snapshot().guest_ms) {
                            return Err("knob-dialog popup exceeded its shared deadline".into());
                        }
                        let result =
                            json!({"turns":context.turns,"level_raw":raw,"displayed":displayed});
                        self.knob_popup = None;
                        return Ok(result);
                    }
                    let guest = inputs.panel.snapshot().guest_ms;
                    if guest.wrapping_sub(start) >= guest_timeout_ms.min(10_000)
                        || Instant::now() >= deadline
                        || context.expired(Instant::now(), guest)
                    {
                        return Err(format!("Knob Dialog shows {displayed:?} for channel 1 level {raw}, expected {expected:?}").into());
                    }
                    thread::sleep(Duration::from_millis(5));
                }
            }
            _ => unreachable!(),
        }
    }
}

#[cfg(test)]
mod popup_step_tests {
    use super::*;
    #[test]
    fn tries_reverse_only_after_first_attempt_fails() {
        let mut directions = Vec::new();
        let turns = attempt_popup_turns(-2, 4, |delta| {
            directions.push(delta);
            Ok(true)
        })
        .unwrap();
        assert_eq!(turns, [-2]);
        assert_eq!(directions, [-2]);
        let mut directions = Vec::new();
        let turns = attempt_popup_turns(-2, 4, |delta| {
            directions.push(delta);
            Ok(delta == 4)
        })
        .unwrap();
        assert_eq!(turns, [-2, 4]);
        assert_eq!(directions, [-2, 4]);
        assert!(
            attempt_popup_turns(-2, 4, |_| Err("timeout".into()))
                .unwrap_err()
                .contains("timeout")
        );
    }

    #[test]
    fn open_requires_state_callback_display_and_level() {
        let mut snapshot = l6ctl::KnobPopupSnapshot {
            revision_guard: "test".into(),
            atomic: false,
            window: 0,
            state: [0x4e, 0x4b, 1, 0xff],
            callback: 1,
            level: 33,
            text: None,
        };
        let before = [0; 1024];
        let mut after = before;
        after[0] = 1;
        assert!(popup_opened(&snapshot, &before, &after, 32));
        assert!(!popup_opened(&snapshot, &before, &before, 32));
        assert!(!popup_opened(&snapshot, &before, &after, 33));
        snapshot.callback = 0;
        assert!(!popup_opened(&snapshot, &before, &after, 32));
        snapshot.callback = 1;
        snapshot.state[0] = 0;
        assert!(!popup_opened(&snapshot, &before, &after, 32));
    }
    #[test]
    fn rendered_value_requires_ready_changed_level_and_exact_text() {
        let mut snapshot = l6ctl::KnobPopupSnapshot {
            revision_guard: "test".into(),
            atomic: false,
            window: 0,
            state: [0x4e, 0x4b, 1, 0xff],
            callback: 1,
            level: 64,
            text: Some("50%".into()),
        };
        assert!(popup_value_matches(&snapshot, 63));
        assert!(!popup_value_matches(&snapshot, 64));
        snapshot.text = Some("49%".into());
        assert!(!popup_value_matches(&snapshot, 63));
        snapshot.text = Some("50%".into());
        snapshot.state[3] = 0;
        assert!(!popup_value_matches(&snapshot, 63));
    }

    #[test]
    fn popup_shared_deadline_is_bounded_by_wall_and_guest_time() {
        let now = Instant::now();
        let context = PopupContext {
            before: [0; 1024],
            level_before: 0,
            turns: vec![],
            guest_start: u32::MAX - 4,
            wall_deadline: now + Duration::from_secs(2),
            guest_timeout_ms: 10,
        };
        assert!(!context.expired(now + Duration::from_secs(1), 5));
        assert!(context.expired(now + Duration::from_secs(3), 5));
        assert!(context.expired(now + Duration::from_secs(1), 6));
    }
}

#[cfg(test)]
mod recorder_ready_tests {
    use super::recorder_controls_visible;
    #[test]
    fn compares_only_the_reviewed_transport_crop() {
        let reference = image::GrayImage::from_pixel(128, 64, image::Luma([0]));
        let mut display = [0; 1024];
        assert!(recorder_controls_visible(&display, &reference));
        display[((63 - 4) / 8) * 128] ^= 1 << ((63 - 4) % 8);
        assert!(recorder_controls_visible(&display, &reference));
        display[((63 - 20) / 8) * 128] ^= 1 << ((63 - 20) % 8);
        assert!(!recorder_controls_visible(&display, &reference));
    }
}

#[cfg(test)]
mod paused_guard_tests {
    use super::{guard_rejected, paused_observation};
    use l6max_host::knob_popup::GuardState;
    use std::cell::Cell;
    #[test]
    fn guard_requires_unchanged_window_empty_state_and_callback() {
        let mut state = GuardState {
            window: 7,
            state: 0,
            callback: 0,
        };
        assert!(guard_rejected(&state, 7));
        state.window = 8;
        assert!(!guard_rejected(&state, 7));
        state.window = 7;
        state.state = 1;
        assert!(!guard_rejected(&state, 7));
        state.state = 0;
        state.callback = 1;
        assert!(!guard_rejected(&state, 7));
    }
    #[test]
    fn resumes_after_success_or_read_failure_and_reports_resume_failure() {
        let resumed = Cell::new(false);
        let read = paused_observation(
            || Ok(()),
            || Err::<(), _>("read failed".to_owned()),
            || {
                resumed.set(true);
                Ok(())
            },
        );
        assert_eq!(read.unwrap_err(), "read failed");
        assert!(resumed.get());
        let both = paused_observation(
            || Ok(()),
            || Err::<(), _>("read".to_owned()),
            || Err("resume".to_owned()),
        );
        assert!(
            both.unwrap_err()
                .contains("read; failed to resume QEMU: resume")
        );
        assert_eq!(
            paused_observation(|| Ok(()), || Ok(42), || Ok(())).unwrap(),
            42
        );
        let called = Cell::new(false);
        assert!(
            paused_observation(
                || Err("stop".to_owned()),
                || Ok(()),
                || {
                    called.set(true);
                    Ok(())
                }
            )
            .is_err()
        );
        assert!(!called.get());
    }
}

#[cfg(test)]
mod debug_cleanup_tests {
    use super::{DebugCleanup, cleanup_debugger};
    use l6max_host::debug_journal::Journal;
    #[derive(Default)]
    struct Fake {
        calls: Vec<&'static str>,
        stop_fails: bool,
        unsynchronized: bool,
        mailbox_pending: bool,
        restore_fails: bool,
        breakpoint_fails: bool,
        register_fails: bool,
        resume_fails: bool,
    }
    impl DebugCleanup for Fake {
        fn stop(&mut self) -> Result<(), String> {
            self.calls.push("stop");
            if self.stop_fails {
                Err("stop failure".into())
            } else {
                Ok(())
            }
        }
        fn synchronized(&self) -> bool {
            !self.unsynchronized && !self.mailbox_pending
        }
        fn restore_writes(&mut self) -> Vec<String> {
            self.calls.push("writes");
            if self.restore_fails {
                vec!["write failure".into()]
            } else {
                vec![]
            }
        }
        fn clear_breakpoints(&mut self) -> Vec<String> {
            self.calls.push("breakpoints");
            if self.breakpoint_fails {
                vec!["breakpoint failure".into()]
            } else {
                vec![]
            }
        }
        fn restore_registers(&mut self) -> Vec<String> {
            self.calls.push("registers");
            if self.register_fails {
                vec!["register failure".into()]
            } else {
                vec![]
            }
        }
        fn resume(&mut self) -> Result<(), String> {
            self.calls.push("resume");
            if self.resume_fails {
                Err("resume failure".into())
            } else {
                Ok(())
            }
        }
    }
    #[test]
    fn successful_restoration_retires_only_the_matching_outstanding_write() {
        let mut journal = Journal::default();
        let first = journal.begin_write(0x100, vec![1], &[2]).unwrap();
        journal.commit_write(first);
        let other = journal.begin_write(0x200, vec![3], &[4]).unwrap();
        journal.commit_write(other);
        let restore = journal.begin_write(0x100, vec![2], &[1]).unwrap();
        journal.commit_write(restore);
        assert_eq!(journal.writes, vec![(0x200, vec![3])]);
    }
    #[test]
    fn failed_restore_keeps_original_and_attempt_for_reverse_rollback() {
        let mut journal = Journal::default();
        journal.begin_write(0x100, vec![1], &[2]).unwrap();
        journal.begin_write(0x100, vec![2], &[1]).unwrap();
        assert_eq!(journal.writes, vec![(0x100, vec![1]), (0x100, vec![2])]);
    }
    #[test]
    fn overlapping_regions_are_rejected_except_exact_restoration() {
        let mut journal = Journal::default();
        journal
            .begin_write(0x100, vec![1, 2, 3, 4], &[9, 9, 9, 9])
            .unwrap();
        assert!(journal.begin_write(0x102, vec![0], &[3]).is_err());
        assert!(
            journal
                .begin_write(0x100, vec![0; 4], &[8, 8, 8, 8])
                .is_err()
        );
        assert_eq!(journal.writes.len(), 1);
        assert!(
            journal
                .begin_write(0x100, vec![9; 4], &[1, 2, 3, 4])
                .is_ok()
        );
    }
    #[test]
    fn unsynchronized_transport_stops_without_further_debugger_requests() {
        let mut fake = Fake {
            unsynchronized: true,
            ..Default::default()
        };
        let errors = cleanup_debugger(&mut fake);
        assert_eq!(fake.calls, ["stop"]);
        assert!(errors.join(" ").contains("guest left stopped"));
    }
    #[test]
    fn pending_mailbox_handoff_keeps_guest_stopped() {
        let mut fake = Fake {
            mailbox_pending: true,
            ..Default::default()
        };
        let errors = cleanup_debugger(&mut fake);
        assert_eq!(fake.calls, ["stop"]);
        assert!(errors.join(" ").contains("guest left stopped"));
    }
    #[test]
    fn clean_cleanup_restores_and_resumes_in_order() {
        let mut fake = Fake::default();
        assert!(cleanup_debugger(&mut fake).is_empty());
        assert_eq!(
            fake.calls,
            ["stop", "writes", "breakpoints", "registers", "resume"]
        );
    }
    #[test]
    fn failed_restoration_leaves_guest_stopped() {
        let mut fake = Fake {
            restore_fails: true,
            breakpoint_fails: true,
            register_fails: true,
            resume_fails: true,
            ..Default::default()
        };
        let errors = cleanup_debugger(&mut fake);
        assert_eq!(fake.calls, ["stop", "writes"]);
        assert!(errors.join(" ").contains("write failure"));
        assert!(errors.join(" ").contains("guest left stopped"));
    }
    #[test]
    fn failed_breakpoint_cleanup_never_restores_registers_or_resumes() {
        let mut fake = Fake {
            breakpoint_fails: true,
            ..Default::default()
        };
        let errors = cleanup_debugger(&mut fake);
        assert_eq!(fake.calls, ["stop", "writes", "breakpoints"]);
        assert!(errors.join(" ").contains("guest left stopped"));
    }
    #[test]
    fn failed_register_cleanup_never_resumes() {
        let mut fake = Fake {
            register_fails: true,
            ..Default::default()
        };
        let errors = cleanup_debugger(&mut fake);
        assert_eq!(fake.calls, ["stop", "writes", "breakpoints", "registers"]);
        assert!(errors.join(" ").contains("guest left stopped"));
    }
    #[test]
    fn failed_stop_skips_unsafe_mutations_and_resume() {
        let mut fake = Fake {
            stop_fails: true,
            ..Default::default()
        };
        let errors = cleanup_debugger(&mut fake);
        assert_eq!(fake.calls, ["stop"]);
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("stop failure"));
    }
}

#[cfg(test)]
mod preflight_tests {
    use super::preflight_rejected;
    #[test]
    fn rejection_requires_empty_state_and_callback_and_unchanged_heap() {
        let heap = [100, 90, 5, 2];
        assert!(preflight_rejected(&[0; 4], &[0; 4], &heap, &heap));
        assert!(!preflight_rejected(&[1, 0, 0, 0], &[0; 4], &heap, &heap));
        assert!(!preflight_rejected(&[0; 4], &[1, 0, 0, 0], &heap, &heap));
        assert!(!preflight_rejected(
            &[0; 4],
            &[0; 4],
            &[100, 90, 6, 2],
            &heap
        ));
    }
}

#[cfg(test)]
mod first_use_callback_tests {
    use super::{
        DISPLAY_CYCLE2_RETURN_GUARD, callback_first_use_ready, supported_display_dispatcher,
    };
    #[test]
    fn dispatcher_guard_rejects_any_changed_or_truncated_instruction() {
        assert!(supported_display_dispatcher(&DISPLAY_CYCLE2_RETURN_GUARD));
        assert!(!supported_display_dispatcher(
            &DISPLAY_CYCLE2_RETURN_GUARD[..59]
        ));
        for index in 0..DISPLAY_CYCLE2_RETURN_GUARD.len() {
            let mut changed = DISPLAY_CYCLE2_RETURN_GUARD;
            changed[index] ^= 1;
            assert!(!supported_display_dispatcher(&changed));
        }
    }
    #[test]
    fn cleared_mailbox_and_ticks_are_not_enough_until_callback_returns() {
        let ready = [0x4e, 0x4b, 20, 0xff];
        assert!(!callback_first_use_ready(&ready, 1, 1, 1));
        assert!(callback_first_use_ready(&ready, 1, 1, 0));
        assert!(!callback_first_use_ready(&[0x4e, 0x4b, 0, 0xff], 1, 1, 0));
        assert!(!callback_first_use_ready(&[0x4e, 0x4b, 20, 9], 1, 1, 0));
        assert!(!callback_first_use_ready(&ready, 0, 1, 0));
        assert!(!callback_first_use_ready(&ready, 3, 1, 0));
        assert!(!callback_first_use_ready(&[0; 3], 1, 1, 0));
    }
}

#[cfg(test)]
mod activation_baseline_tests {
    use super::activation_baseline_ready;
    #[test]
    fn retry_requires_unchanged_empty_baseline_exact_callback_and_held_lock() {
        let state = [0x4e, 0x4b, 0, 0xff];
        assert!(activation_baseline_ready(&state, 3, 3, &[0; 4]));
        assert!(!activation_baseline_ready(
            &[0x4e, 0x4b, 1, 0xff],
            3,
            3,
            &[0; 4]
        ));
        assert!(!activation_baseline_ready(
            &[0x4e, 0x4b, 0, 9],
            3,
            3,
            &[0; 4]
        ));
        assert!(!activation_baseline_ready(&state, 5, 3, &[0; 4]));
        assert!(!activation_baseline_ready(&state, 3, 3, &[1, 0, 0, 0]));
    }
}

#[cfg(test)]
mod locked_update_tests {
    use super::{locked_level_expected, locked_update_ready};
    #[test]
    fn mailbox_and_current_rounded_value_must_both_match() {
        assert_eq!(locked_level_expected(64), "50%");
        assert!(locked_update_ready(&[0xff], Some("50%"), 64));
        assert!(!locked_update_ready(&[9], Some("50%"), 64));
        assert!(!locked_update_ready(&[0xff], None, 64));
        assert!(!locked_update_ready(&[0xff], Some("49%"), 64));
    }
}

#[cfg(test)]
mod opened_overlay_tests {
    use super::opened_overlay_matches;
    #[test]
    fn boundary_requires_signature_and_same_callback_but_allows_expiry() {
        assert!(opened_overlay_matches(
            &[0x4e, 0x4b, 1, 0xff],
            &3u32.to_le_bytes(),
            3
        ));
        assert!(opened_overlay_matches(
            &[0x4e, 0x4b, 0, 0xff],
            &3u32.to_le_bytes(),
            3
        ));
        assert!(!opened_overlay_matches(&[0; 4], &3u32.to_le_bytes(), 3));
        assert!(!opened_overlay_matches(
            &[0x4e, 0x4b, 1, 0xff],
            &5u32.to_le_bytes(),
            3
        ));
    }
}

#[cfg(test)]
mod task_context_tests {
    use super::verify_distinct_task_contexts;
    #[test]
    fn task_contexts_preserve_ids_and_reject_same_task() {
        let tasks = verify_distinct_task_contexts(0x1000, 0x2000).unwrap();
        assert_eq!((tasks.hook_task, tasks.callback_task), (0x1000, 0x2000));
        assert!(verify_distinct_task_contexts(0x1000, 0x1000).is_err());
    }
}

#[cfg(test)]
mod channel_level_readiness_tests {
    use super::{channel_level_turn_clamped, wait_for_channel_level};
    use std::cell::Cell;
    #[test]
    fn waits_for_fresh_text_and_fails_wrong_channel_at_deadline() {
        let time = Cell::new(0);
        let raw = wait_for_channel_level(
            0,
            || Ok((1, if time.get() < 2 { "0%" } else { "1%" }.to_owned())),
            || time.get() >= 5,
            || time.set(time.get() + 1),
        )
        .unwrap();
        assert_eq!(raw, 1);
        assert_eq!(time.get(), 2);
        let time = Cell::new(0);
        assert!(
            wait_for_channel_level(
                0,
                || Ok((1, "0%".into())),
                || time.get() >= 3,
                || time.set(time.get() + 1)
            )
            .is_err()
        );
        let time = Cell::new(0);
        assert!(
            wait_for_channel_level(
                0,
                || Ok((0, "0%".into())),
                || time.get() >= 3,
                || time.set(time.get() + 1)
            )
            .is_err()
        );
        let time = Cell::new(0);
        assert!(
            wait_for_channel_level(
                2,
                || Ok((3, "2%".into())),
                || time.get() >= 3,
                || time.set(time.get() + 1)
            )
            .is_err()
        );
    }
    #[test]
    fn clamped_no_change_does_not_require_an_impossible_update() {
        assert!(channel_level_turn_clamped(0, -1));
        assert!(channel_level_turn_clamped(127, 1));
        assert!(!channel_level_turn_clamped(0, 1));
        assert!(!channel_level_turn_clamped(1, -1));
    }
}

#[cfg(test)]
mod invalid_tag_deadline_tests {
    use super::*;
    #[test]
    fn timer_includes_inter_step_delay_and_guest_wrap() {
        let start = Instant::now();
        let tag = PendingInvalidTag {
            tag: 15,
            guest_start: u32::MAX - 4,
            wall_deadline: start + Duration::from_secs(180),
        };
        assert!(!tag.expired(4, start + Duration::from_secs(1), 10_000));
        assert!(tag.expired(9995, start + Duration::from_secs(1), 10_000));
        assert!(tag.expired(4, start + Duration::from_secs(180), 10_000));
        assert_eq!(tag.description(), "invalid mode 15");
        assert_eq!(
            PendingInvalidTag { tag: 240, ..tag }.description(),
            "invalid channel 15"
        );
    }
    #[test]
    fn poll_crossing_guest_deadline_does_not_read_mailbox() {
        let now = Instant::now();
        let pending = PendingInvalidTag {
            tag: 15,
            guest_start: 100,
            wall_deadline: now + Duration::from_secs(180),
        };
        let mut reads = 0;
        assert!(
            read_invalid_tag_if_live(&pending, 10_100, now, 10_000, || {
                reads += 1;
                Ok(())
            })
            .is_err()
        );
        assert_eq!(reads, 0);
    }
}

#[cfg(test)]
mod invalid_signature_observation_tests {
    use super::*;
    #[test]
    fn anchors_guest_time_at_resume_and_handles_wrap_and_deadline() {
        let start = Instant::now();
        let pending = PendingInvalidSignature {
            original: vec![0; 4],
            invalid: 1,
            guest_start: u32::MAX - 49,
            wall_start: start,
            rejected: None,
        };
        assert_eq!(
            pending.observation(49, start + Duration::from_secs(1), 150, 180_000, 600),
            SignatureObservation::Waiting
        );
        assert_eq!(
            pending.observation(100, start + Duration::from_secs(1), 150, 180_000, 600),
            SignatureObservation::Complete
        );
        assert_eq!(
            pending.observation(49, start + Duration::from_secs(601), 150, 180_000, 600),
            SignatureObservation::Expired
        );
        assert_eq!(
            pending.observation(100, start + Duration::from_secs(601), 150, 180_000, 600),
            SignatureObservation::Complete
        );
    }
}

#[cfg(test)]
mod settle_level_polarity_tests {
    use super::settle_level_encoder_steps;
    #[test]
    fn uses_input_hub_equivalent_polarity_at_both_endpoints() {
        assert_eq!(settle_level_encoder_steps(0), -1);
        assert_eq!(settle_level_encoder_steps(127), 1);
    }
}

#[cfg(test)]
mod settle_level_observation_tests {
    use super::settled_level_matches;
    #[test]
    fn does_not_read_mailbox_or_text_before_level_changes() {
        assert!(
            !settled_level_matches(4, 4, || panic!("mailbox read"), || panic!("text read"))
                .unwrap()
        );
    }
    #[test]
    fn does_not_read_text_before_mailbox_clears() {
        assert!(!settled_level_matches(4, 5, || Ok(0), || panic!("text read")).unwrap());
        assert!(settled_level_matches(4, 5, || Ok(255), || Ok("4%".into())).unwrap());
    }
}

#[cfg(test)]
mod notification_frame_wait_tests {
    use super::*;
    #[test]
    fn clear_uses_main_progress_not_panel_time_and_requires_changed_pixels() {
        // Panel can advance arbitrarily faster; only MAIN's guarded counter matters.
        assert!(!notification_visible(true, 0));
        assert!(!notification_visible(false, 1));
        assert!(notification_visible(true, 1));
        // Diverging panel time is deliberately absent; MAIN ms and wall bound decide.
        assert_eq!(
            notification_clear_decision(false, 100, 5099, 5000, false),
            NotificationFrameWait::Pending
        );
        assert_eq!(
            notification_clear_decision(false, 100, 5100, 5000, false),
            NotificationFrameWait::Expired
        );
        assert_eq!(
            notification_clear_decision(false, 100, 100, 5000, true),
            NotificationFrameWait::Expired
        );
        assert_eq!(
            notification_clear_decision(true, 100, 200, 5000, true),
            NotificationFrameWait::Expired
        );
        assert_eq!(
            notification_clear_decision(true, 100, 5100, 5000, false),
            NotificationFrameWait::Expired
        );
        assert_eq!(
            notification_clear_decision(true, u32::MAX - 10, 0, 5000, false),
            NotificationFrameWait::Changed
        );
        assert!(!notification_cleared(true, 1, 100, 110, 120));
        assert!(!notification_cleared(false, 0, 100, 110, 120));
        assert!(!notification_cleared(true, 0, 100, 110, 110));
        assert!(notification_cleared(true, 0, 100, 110, 120));
        assert!(notification_cleared(true, 0, u32::MAX - 3, u32::MAX - 1, 1));
    }
    #[test]
    fn late_notification_entry_honors_shared_wall_and_panel_guest_budgets() {
        let now = Instant::now();
        assert!(
            remaining_debug_wait((now - Duration::from_secs(11), 100, 1000, 10, 0), 101, 600)
                .is_err()
        );
        assert!(remaining_debug_wait((now, 100, 1000, 10, 0), 1101, 600).is_err());
        assert!(remaining_debug_wait((now, 100, 1000, 10, 0), 1099, 600).is_ok());
    }
    #[test]
    fn uses_original_guest_start_across_wrap_and_accepts_changed_frame_first() {
        let start = u32::MAX - 499;
        assert_eq!(
            notification_frame_wait(false, start, 999, 1500),
            NotificationFrameWait::Pending
        );
        assert_eq!(
            notification_frame_wait(false, start, 1000, 1500),
            NotificationFrameWait::Expired
        );
        assert_eq!(
            notification_frame_wait(false, start, 4499, 5000),
            NotificationFrameWait::Pending
        );
        assert_eq!(
            notification_frame_wait(false, start, 4500, 5000),
            NotificationFrameWait::Expired
        );
        assert_eq!(
            notification_frame_wait(true, start, 4500, 5000),
            NotificationFrameWait::Changed
        );
    }
}

#[cfg(test)]
mod expiry_tests {
    use super::*;
    #[test]
    fn expiry_grace_uses_original_start_across_guest_wrap() {
        let start = u32::MAX - 499;
        assert!(!expiry_timed_out(start, 9499, 10_000));
        assert!(expiry_timed_out(start, 9500, 10_000));
    }
    #[test]
    fn both_saved_frames_are_independently_rejected() {
        let scene = [1; 1024];
        let notification = [2; 1024];
        let expired = [3; 1024];
        assert!(validate_expiry_frame(&expired, &scene, true).is_ok());
        assert!(validate_expiry_frame(&expired, &notification, false).is_ok());
        assert!(
            validate_expiry_frame(&scene, &scene, true)
                .unwrap_err()
                .contains("custom scene")
        );
        assert!(
            validate_expiry_frame(&notification, &notification, false)
                .unwrap_err()
                .contains("notification")
        );
    }
}

#[cfg(test)]
mod restoration_mismatch_tests {
    use super::collect_restoration_mismatch;
    #[test]
    fn aggregates_all_mismatches_in_comparison_order() {
        let mut names = Vec::new();
        assert!(!collect_restoration_mismatch(
            &mut names,
            "window",
            &[0],
            &[1]
        ));
        assert!(collect_restoration_mismatch(
            &mut names,
            "bitmap",
            &[2],
            &[2]
        ));
        assert!(!collect_restoration_mismatch(
            &mut names,
            "widget",
            &[3],
            &[4]
        ));
        assert_eq!(names, ["window", "widget"]);
    }
}

#[cfg(test)]
mod overlay_invariant_tests {
    use super::overlay_corners_black;
    #[test]
    fn detects_each_lit_corner_of_saved_frame() {
        let mut frame = [0; 1024];
        assert!(overlay_corners_black(&frame));
        for (x, y) in [(0usize, 0usize), (127, 0), (0, 63), (127, 63)] {
            let index = ((63 - y) / 8) * 128 + x;
            frame[index] = 1 << ((63 - y) % 8);
            assert!(!overlay_corners_black(&frame));
            frame[index] = 0;
        }
    }
}

#[cfg(test)]
mod refreshed_overlay_tests {
    use super::stable_refreshed_overlay;

    #[test]
    fn only_value_widget_pixels_may_change() {
        let active = [0; 1024];
        for (x, y, allowed) in [
            (30, 33, true),
            (96, 63, true),
            (29, 33, false),
            (97, 33, false),
            (30, 32, false),
            (127, 63, false),
        ] {
            let mut refreshed = active;
            refreshed[((63 - y) / 8) * 128 + x] |= 1 << ((63 - y) % 8);
            assert_eq!(
                stable_refreshed_overlay(&active, &refreshed),
                allowed,
                "pixel ({x},{y})"
            );
        }
    }
}

#[cfg(test)]
mod locked_update_decomposition_tests {
    use super::*;
    #[test]
    fn reads_level_then_mailbox_and_decodes_text_only_when_cleared() {
        let reads = std::cell::RefCell::new(Vec::new());
        let result = locked_poll_read(
            || {
                reads.borrow_mut().push("level");
                Ok(7)
            },
            || {
                reads.borrow_mut().push("mailbox");
                Ok(0)
            },
            || panic!("text read before mailbox cleared"),
        )
        .unwrap();
        assert_eq!(result, (7, 0, None));
        assert_eq!(*reads.borrow(), ["level", "mailbox"]);
        reads.borrow_mut().clear();
        let result = locked_poll_read(
            || {
                reads.borrow_mut().push("level");
                Ok(7)
            },
            || {
                reads.borrow_mut().push("mailbox");
                Ok(255)
            },
            || {
                reads.borrow_mut().push("text");
                Ok("6%".into())
            },
        )
        .unwrap();
        assert_eq!(result, (7, 255, Some("6%".into())));
        assert_eq!(*reads.borrow(), ["level", "mailbox", "text"]);
    }
    #[test]
    fn original_wait_anchor_enforces_guest_wrap_and_wall_cap() {
        let start = u32::MAX - 499;
        assert!(!locked_wait_expired(
            start,
            9499,
            Duration::from_secs(179),
            180_000,
            600
        ));
        assert!(locked_wait_expired(
            start,
            9500,
            Duration::from_secs(1),
            180_000,
            600
        ));
        assert!(locked_wait_expired(
            start,
            start,
            Duration::from_secs(180),
            180_000,
            600
        ));
    }
}

#[cfg(test)]
mod preflight_assertion_tests {
    use super::preflight_rejected;
    #[test]
    fn rejects_nonzero_state_callback_and_every_heap_counter_change() {
        let zero = [0; 4];
        let baseline = [100, 90, 7, 6];
        assert!(preflight_rejected(&zero, &zero, &baseline, &baseline));
        assert!(!preflight_rejected(
            &[1, 0, 0, 0],
            &zero,
            &baseline,
            &baseline
        ));
        assert!(!preflight_rejected(
            &zero,
            &[1, 0, 0, 0],
            &baseline,
            &baseline
        ));
        for i in 0..4 {
            let mut changed = baseline;
            changed[i] += 1;
            assert!(!preflight_rejected(&zero, &zero, &changed, &baseline));
        }
    }
}

#[cfg(test)]
mod preflight_budget_tests {
    use super::*;
    #[test]
    fn caps_blocking_wait_to_remaining_wall_and_checks_guest_wrap() {
        let start = Instant::now() - Duration::from_millis(9500);
        let shared = (start, u32::MAX - 499, 10_000, 10, 0);
        let remaining = remaining_debug_wait(shared, 1000, 600).unwrap();
        assert!(remaining <= Duration::from_millis(500));
        assert!(remaining_debug_wait(shared, 9501, 600).is_err());
        let expired = (Instant::now() - Duration::from_secs(11), 0, 10_000, 10, 0);
        assert!(remaining_debug_wait(expired, 0, 600).is_err());
    }
}

#[cfg(test)]
mod task_probe_deadline_tests {
    use super::*;
    #[test]
    fn uses_anchored_wait_deadline_and_shared_guest_wrap() {
        let start = Instant::now();
        let shared = (start, u32::MAX - 499, 10_000, 100, 0);
        assert!(!task_wait_expired(
            start + Duration::from_secs(9),
            start + Duration::from_secs(10),
            1000,
            shared
        ));
        // Applies equally when the first query observes an already-stopped guest.
        assert!(task_wait_expired(
            start + Duration::from_secs(10),
            start + Duration::from_secs(10),
            1000,
            shared
        ));
        assert!(task_wait_expired(
            start + Duration::from_secs(1),
            start + Duration::from_secs(10),
            9501,
            shared
        ));
    }
}

#[cfg(test)]
mod overlay_reactivation_tests {
    use super::{
        BITMAP_DRAW_MIN_TICKS, overlay_reactivation_expired, overlay_reactivation_ready,
        require_bitmap_countdown, validate_zero_reactivation,
    };
    use std::time::{Duration, Instant};
    #[test]
    fn requires_zero_endpoint_and_decreasing_input() {
        assert!(validate_zero_reactivation(0, -1).is_ok());
        assert!(validate_zero_reactivation(1, -1).is_err());
        assert!(validate_zero_reactivation(0, 1).is_err());
    }
    #[test]
    fn readiness_timeout_handles_guest_wrap_and_wall_limit() {
        let shared = (Instant::now(), u32::MAX - 499, 50_000, 600, 0);
        assert!(!overlay_reactivation_expired(
            u32::MAX - 499,
            9499,
            Duration::from_secs(1),
            shared
        ));
        assert!(overlay_reactivation_expired(
            u32::MAX - 499,
            9500,
            Duration::from_secs(1),
            shared
        ));
        assert!(overlay_reactivation_expired(
            0,
            0,
            Duration::from_secs(180),
            shared
        ));
    }
    #[test]
    fn delayed_final_level_read_cannot_succeed_after_local_guest_deadline() {
        let shared = (Instant::now(), 0, 50_000, 600, 0);
        let before = overlay_reactivation_expired(100, 10_099, Duration::from_secs(1), shared);
        let after = overlay_reactivation_expired(100, 10_100, Duration::from_secs(1), shared);
        assert!(!before);
        assert!(after);
    }
    #[test]
    fn requires_consumed_mailbox_and_active_countdown() {
        assert!(!overlay_reactivation_ready(0, 20, 15));
        assert!(!overlay_reactivation_ready(255, 0, 15));
        assert!(!overlay_reactivation_ready(255, 14, 15));
        assert!(overlay_reactivation_ready(255, 15, 15));
        assert!(require_bitmap_countdown(9, BITMAP_DRAW_MIN_TICKS).is_err());
        assert!(require_bitmap_countdown(10, BITMAP_DRAW_MIN_TICKS).is_ok());
        assert!(overlay_reactivation_ready(255, 20, 15));
    }
}

#[cfg(test)]
mod callback_race_deadline_tests {
    use super::*;

    #[test]
    fn delayed_wait_and_already_stopped_observation_keep_original_anchor() {
        let start = Instant::now();
        let deadline = start + Duration::from_secs(10);
        assert!(!race_deadline_expired(
            deadline - Duration::from_nanos(1),
            deadline
        ));
        assert!(race_deadline_expired(deadline, deadline));
        assert!(race_deadline_expired(
            deadline + Duration::from_secs(1),
            deadline
        ));
    }
}

#[cfg(test)]
mod activation_deadline_tests {
    use super::*;
    #[test]
    fn typed_encoder_matches_legacy_knob_polarity() {
        use std::os::unix::net::UnixStream;
        let (main, _main_peer) = UnixStream::pair().unwrap();
        let (panel, mut panel_peer) = UnixStream::pair().unwrap();
        let inputs = l6max_host::input::InputHub::new(
            l6max_host::input::EngineInput::new(main).unwrap(),
            l6max_host::input::EngineInput::new(panel).unwrap(),
            l6fw::Model::L6max,
        );
        inputs.knob(0, ACTIVATION_KNOB_DELTA).unwrap();
        let message = l6max_host::protocol::Message::read(&mut panel_peer).unwrap();
        assert_eq!(
            (message.kind, message.target, message.value),
            (
                l6max_host::protocol::ENCODER_STEP,
                0,
                activation_encoder_steps()
            )
        );
    }
    #[test]
    fn local_deadline_rejects_late_read_and_handles_guest_wrap() {
        let now = Instant::now();
        let deadline = now + Duration::from_secs(1);
        assert!(!activation_local_expired(
            now,
            deadline,
            2,
            u32::MAX - 5,
            10
        ));
        assert!(activation_local_expired(now, deadline, 4, u32::MAX - 5, 10));
        assert!(activation_local_expired(deadline, deadline, 0, 0, 10_000));
    }
}

#[cfg(test)]
mod sd_knob_binding_tests {
    use super::*;
    #[test]
    fn sd_registration_requires_the_exact_relocated_callback() {
        assert!(verify_sd_callback(Some(0x81001001), 0x81001001).is_ok());
        assert!(verify_sd_callback(Some(0x81001001), 0x81001003).is_err());
        assert!(verify_sd_callback(None, 0x80101001).is_ok());
    }
    #[test]
    fn sd_fixture_is_required_only_when_the_plan_selects_sd_knob() {
        let root = std::env::temp_dir().join(format!(
            "knob-sd-binding-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        fs::create_dir_all(root.join("sd-stock-firmware")).unwrap();
        fs::write(root.join("sd-stock-firmware/main_firmware.bin"), []).unwrap();
        let mut options = Options::parse_args(std::iter::empty()).unwrap();
        options.logs = root.clone();
        assert!(load_sd_knob_fixture(&options, false).unwrap().is_none());
        assert!(load_sd_knob_fixture(&options, true).is_err());
        fs::create_dir_all(root.join("sd-patches-build")).unwrap();
        fs::write(
            root.join("sd-patches-build/patch.json"),
            br#"{"patches":[{"members":["midi-sync"],"file":"PATCHES/midi.bin"}]}"#,
        )
        .unwrap();
        assert!(load_sd_knob_fixture(&options, true).is_err());
        fs::write(
            root.join("sd-patches-build/patch.json"),
            br#"{"patches":[{"members":["knob-dialog"],"file":"PATCHES/knob.bin"}]}"#,
        )
        .unwrap();
        assert!(load_sd_knob_fixture(&options, true).is_err());
        fs::remove_dir_all(root).unwrap();
    }
}

#[cfg(test)]
mod firmware_knob_binding_tests {
    use super::*;
    use sha2::{Digest, Sha256};

    #[test]
    fn retained_report_binds_after_restart_and_rejects_missing_or_wrong_image() {
        let root = std::env::temp_dir().join(format!(
            "knob-restart-binding-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let firmware = root.join("firmware");
        fs::create_dir_all(&firmware).unwrap();
        fs::create_dir_all(root.join("restart")).unwrap();
        let base = l6fw::Model::L6max.main_base();
        let image = vec![0u8; 0x100000];
        let payload = base + 0xe0000;
        let report = serde_json::json!({"model":"L6max","image_length":image.len(),"patched_sha256":format!("{:x}",Sha256::digest(&image)),"patches":[{"name":"knob-dialog","payload_address":format!("{payload:#x}"),"payload_length":1024,"knob_callback":payload + 0x100}]});
        let report_path = root.join("patch-report.json");
        fs::write(firmware.join("main_firmware.bin"), &image).unwrap();
        fs::write(&report_path, serde_json::to_vec(&report).unwrap()).unwrap();
        let mut options = Options::parse_args(std::iter::empty()).unwrap();
        options.firmware_dir = firmware.clone();
        options.state_dir = None;
        options.logs = root.join("restart");
        options.fixture_firmware_report = Some(report_path.clone());
        assert!(load_firmware_knob_fixture(&options).is_ok());
        let mut wrong = image;
        wrong[0] ^= 1;
        fs::write(firmware.join("main_firmware.bin"), wrong).unwrap();
        assert!(load_firmware_knob_fixture(&options).is_err());
        fs::remove_file(report_path).unwrap();
        assert!(load_firmware_knob_fixture(&options).is_err());
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    #[ignore = "requires L6_KNOB_FIRMWARE_REPORT and L6_KNOB_FIRMWARE_MAIN"]
    fn direct_and_installed_nor_are_bound_to_the_same_report() {
        let report = fs::read(std::env::var("L6_KNOB_FIRMWARE_REPORT").unwrap()).unwrap();
        let image = fs::read(std::env::var("L6_KNOB_FIRMWARE_MAIN").unwrap()).unwrap();
        let root = std::env::temp_dir().join(format!(
            "knob-firmware-binding-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let firmware = root.join("firmware");
        let logs = root.join("logs");
        let state = root.join("state");
        fs::create_dir_all(&firmware).unwrap();
        fs::create_dir_all(&logs).unwrap();
        fs::create_dir_all(&state).unwrap();
        fs::write(firmware.join("main_firmware.bin"), &image).unwrap();
        fs::write(logs.join("patch-report.json"), &report).unwrap();
        let mut options = Options::parse_args(std::iter::empty()).unwrap();
        options.firmware_dir = firmware.clone();
        options.logs = logs.clone();
        let direct = load_firmware_knob_fixture(&options).unwrap();
        fs::remove_file(logs.join("patch-report.json")).unwrap();
        fs::write(firmware.join("patch.json"), &report).unwrap();
        let mut nor = vec![0u8; 0x50000 + image.len()];
        nor[0x50000..].copy_from_slice(&image);
        fs::write(state.join("main-nor.bin"), &nor).unwrap();
        options.state_dir = Some(state.clone());
        let installed = load_firmware_knob_fixture(&options).unwrap();
        assert_eq!(direct.callback, installed.callback);
        nor[0x50000] ^= 1;
        fs::write(state.join("main-nor.bin"), nor).unwrap();
        assert!(load_firmware_knob_fixture(&options).is_err());
        fs::remove_dir_all(root).unwrap();
    }
}
