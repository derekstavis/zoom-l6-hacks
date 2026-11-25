//! The knob-dialog patch: its window and popup.
use super::*;

/// Reviewed knob-dialog window images, from the repository root. The
/// knob-dialog window check compares the display with these by scene name.
pub const KNOB_DIALOG_REFERENCES: &str = "patches/knob-dialog/tests/emulator/references";

/// Owns the debugger and records intrusive mutations for failure cleanup.
/// Successful runs keep guest-side changes made deliberately by the probes.
struct KnobDebugSession {
    qmp: l6max_host::qmp::QmpClient,
    gdb: l6max_diagnostics::gdb::Gdb,
    writes: Vec<(u32, Vec<u8>)>,
    breakpoints: Vec<u32>,
    registers: Vec<(u32, u32)>,
    synchronized: bool,
    mailbox_pending: bool,
    finalized: bool,
}

fn begin_temporary_write(
    writes: &mut Vec<(u32, Vec<u8>)>,
    address: u32,
    before: Vec<u8>,
    data: &[u8],
) -> std::io::Result<Option<usize>> {
    let end = u64::from(address) + data.len() as u64;
    let mut restoring = None;
    for (index, (at, original)) in writes.iter().enumerate() {
        let other_end = u64::from(*at) + original.len() as u64;
        if u64::from(address) < other_end && u64::from(*at) < end {
            if *at == address && original == data && restoring.is_none() {
                restoring = Some(index);
            } else {
                return Err(std::io::Error::other(
                    "overlapping temporary debugger writes",
                ));
            }
        }
    }
    writes.push((address, before));
    Ok(restoring)
}

fn finish_temporary_write(writes: &mut Vec<(u32, Vec<u8>)>, restoring: Option<usize>) {
    if let Some(index) = restoring {
        writes.pop();
        writes.remove(index);
    }
}

impl KnobDebugSession {
    fn connect(qmp: &l6max_host::qmp::QmpClient, path: &std::path::Path) -> std::io::Result<Self> {
        Ok(Self {
            qmp: qmp.clone(),
            gdb: l6max_diagnostics::gdb::Gdb::connect(path)?,
            writes: Vec::new(),
            breakpoints: Vec::new(),
            registers: Vec::new(),
            synchronized: true,
            mailbox_pending: false,
            finalized: false,
        })
    }
    fn write(&mut self, address: u32, data: &[u8]) -> std::io::Result<()> {
        let before = self.qmp.read_memory(0, address, data.len())?;
        // Journal before sending: a failed request can still have applied.
        let restoring = begin_temporary_write(&mut self.writes, address, before, data)?;
        let result = self.gdb.write(address, data);
        if result.is_err() {
            self.synchronized = false;
        }
        result?;
        finish_temporary_write(&mut self.writes, restoring);
        Ok(())
    }
    fn handoff_mailbox(&mut self, address: u32) {
        self.writes.retain(|(at, _)| *at != address);
        self.mailbox_pending = true;
    }
    fn acknowledge_mailbox(&mut self) {
        self.mailbox_pending = false;
    }
    fn resume_guest(&mut self) -> std::io::Result<()> {
        if !self.synchronized {
            return Err(std::io::Error::other(
                "debugger transport not synchronized; guest left stopped",
            ));
        }
        // Even a failed request may resume, invalidating saved register values.
        self.registers.clear();
        let result = self.qmp.resume();
        if result.is_err() {
            self.synchronized = false;
        }
        result
    }
    fn breakpoint(&mut self, address: u32, enable: bool) -> std::io::Result<()> {
        if enable && !self.breakpoints.contains(&address) {
            self.breakpoints.push(address);
        }
        let result = self.gdb.breakpoint(address, enable);
        if result.is_err() {
            self.synchronized = false;
        }
        result?;
        if !enable {
            self.breakpoints.retain(|&item| item != address);
        }
        Ok(())
    }
    fn read_register(&mut self, index: u32) -> std::io::Result<u32> {
        let result = self.gdb.read_register(index);
        if result.is_err() {
            self.synchronized = false;
        }
        result
    }
    fn register(&mut self, index: u32, value: u32) -> std::io::Result<()> {
        let before = self.read_register(index)?;
        self.registers.push((index, before));
        let result = self.gdb.register(index, value);
        if result.is_err() {
            self.synchronized = false;
        }
        result
    }
    fn continue_until_stop(&mut self, timeout: Duration) -> std::io::Result<String> {
        self.registers.clear();
        let result = self.gdb.continue_until_stop(timeout);
        if result.is_err() {
            self.synchronized = false;
        }
        result
    }
    fn call_thumb(
        &mut self,
        address: u32,
        arguments: &[u32],
        timeout: Duration,
    ) -> std::io::Result<String> {
        self.registers.clear();
        let result = self.gdb.call_thumb(address, arguments, timeout);
        if result.is_err() {
            self.synchronized = false;
        }
        result
    }
    fn finish<T>(
        mut self,
        result: Result<T, Box<dyn std::error::Error>>,
    ) -> Result<T, Box<dyn std::error::Error>> {
        match result {
            Ok(value)
                if self.writes.is_empty()
                    && self.breakpoints.is_empty()
                    && self.registers.is_empty()
                    && self.synchronized
                    && !self.mailbox_pending =>
            {
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

trait DebugCleanup {
    fn stop(&mut self) -> Result<(), String>;
    fn synchronized(&self) -> bool;
    fn restore_writes(&mut self) -> Vec<String>;
    fn clear_breakpoints(&mut self) -> Vec<String>;
    fn restore_registers(&mut self) -> Vec<String>;
    fn resume(&mut self) -> Result<(), String>;
}

fn cleanup_debugger(backend: &mut impl DebugCleanup) -> Vec<String> {
    let mut failures = Vec::new();
    if let Err(error) = backend.stop() {
        return vec![format!("stop: {error}; guest state unknown")];
    }
    if !backend.synchronized() {
        return vec![
            "debugger transport or mailbox handoff not synchronized; guest left stopped".into(),
        ];
    }
    failures.extend(backend.restore_writes());
    if failures.is_empty() {
        failures.extend(backend.clear_breakpoints());
    }
    if failures.is_empty() {
        failures.extend(backend.restore_registers());
    }
    if !failures.is_empty() {
        failures.push("guest left stopped".into());
        return failures;
    }
    if let Err(error) = backend.resume() {
        failures.push(format!("resume: {error}"));
    }
    failures
}

impl DebugCleanup for KnobDebugSession {
    fn stop(&mut self) -> Result<(), String> {
        self.qmp.stop().map_err(|error| error.to_string())
    }
    fn synchronized(&self) -> bool {
        self.synchronized && !self.mailbox_pending
    }
    fn restore_writes(&mut self) -> Vec<String> {
        let mut failures = Vec::new();
        for (address, before) in self.writes.drain(..).rev() {
            if let Err(error) = self.gdb.write(address, &before) {
                failures.push(format!("restore {address:#010x}: {error}"));
                break;
            }
        }
        failures
    }
    fn clear_breakpoints(&mut self) -> Vec<String> {
        let mut failures = Vec::new();
        for address in self.breakpoints.drain(..).rev() {
            if let Err(error) = self.gdb.breakpoint(address, false) {
                failures.push(format!("remove breakpoint {address:#010x}: {error}"));
                break;
            }
        }
        failures
    }
    fn restore_registers(&mut self) -> Vec<String> {
        let mut failures = Vec::new();
        for (index, before) in self.registers.drain(..).rev() {
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
fn turn_and_wait_hook(
    session: &Session,
    debugger: &mut KnobDebugSession,
    delta: i32,
    wall_timeout_s: u64,
) -> Result<(), Box<dyn std::error::Error>> {
    const MIXER_HOOK_RETURN: u32 = 0x8003_6c78;
    let qmp = &session.engine.management[0];
    qmp.stop()?;
    debugger.breakpoint(MIXER_HOOK_RETURN, true)?;
    session.engine.inputs.as_ref().unwrap().knob(0, delta)?;
    debugger.continue_until_stop(Duration::from_secs(wall_timeout_s.min(30)))?;
    let pc = debugger.read_register(15)? & !1;
    debugger.breakpoint(MIXER_HOOK_RETURN, false)?;
    if pc != MIXER_HOOK_RETURN {
        return Err(format!("knob turn stopped at {pc:#x}, not the mixer hook return").into());
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
    activation_semaphore_count: Option<Vec<u8>>,
    activation_callback: Option<u32>,
    active: Option<ActiveSnapshot>,
    opened: Option<OpenedOverlay>,
    task_contexts: Option<TaskContexts>,
    overlay_heap: Option<(Vec<u32>, u32)>,
    mode_index: usize,
    last_mode: Option<u8>,
    final_phase: u8,
    pending_notification: Option<PendingNotification>,
    pending_invalid_signature: Option<PendingInvalidSignature>,
    pending_invalid_tag: Option<PendingInvalidTag>,
}

pub(super) fn begin_preflight(
    session: &Session,
    options: &Options,
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
    let debugger = KnobDebugSession::connect(qmp, socket)?;
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
        activation_semaphore_count: None,
        activation_callback: None,
        active: None,
        opened: None,
        task_contexts: None,
        overlay_heap: None,
        mode_index: 0,
        last_mode: None,
        final_phase: 0,
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

pub(super) fn run_preflight_probe(
    session: &Session,
    context: &mut PreflightContext,
    probe: usize,
    wall_timeout_s: u64,
) -> Result<Value, Box<dyn std::error::Error>> {
    if context.next != probe {
        return Err("knob-dialog preflight probes are out of order".into());
    }
    let qmp = &session.engine.management[0];
    let (address, original, replacement, description) = match probe {
        0 => (
            context.widget7 + 6,
            vec![1, 0],
            vec![2, 0],
            "descriptor drift",
        ),
        1 => {
            let alias = u32::from_le_bytes(
                qmp.read_memory(0, l6max::gui_widget_table + 60 * 4, 4)?
                    .try_into()
                    .unwrap(),
            );
            context.alias_widget = Some(alias);
            (
                alias + 4,
                qmp.read_memory(0, alias + 4, 2)?,
                179u16.to_le_bytes().to_vec(),
                "shared text resource alias",
            )
        }
        2 => {
            let alias = context.alias_widget.ok_or("missing alias widget")?;
            let original = qmp.read_memory(0, alias, 2)?;
            context.alias_layer = Some(original.clone());
            (
                alias,
                original,
                4u16.to_le_bytes().to_vec(),
                "scene layer collision",
            )
        }
        3 => (
            context.widget7,
            qmp.read_memory(0, context.widget7, 1)?,
            vec![0],
            "changed widget layer",
        ),
        4 => (
            context.background10,
            context.original_background_layers[0].clone(),
            vec![1],
            "changed background layer",
        ),
        _ => return Err("invalid knob-dialog preflight probe".into()),
    };
    let debugger = context
        .debugger
        .as_mut()
        .ok_or("missing knob-dialog debugger")?;
    qmp.stop()?;
    debugger.write(address, &replacement)?;
    debugger.resume_guest()?;
    turn_and_wait_hook(session, debugger, -1, wall_timeout_s)?;
    qmp.stop()?;
    let state = qmp.read_memory(0, l6max::gui_bitmap_table_entry_0, 4)?;
    let slot5 = qmp.read_memory(0, l6max::gui_display_callback_table + 20, 4)?;
    let heap = qmp
        .read_memory(0, l6max::xFreeBytesRemaining, 16)?
        .chunks_exact(4)
        .map(|b| u32::from_le_bytes(b.try_into().unwrap()))
        .collect::<Vec<_>>();
    debugger.write(address, &original)?;
    debugger.resume_guest()?;
    if !preflight_rejected(&state, &slot5, &heap, &context.heap_before) {
        return Err(format!("{description} was not rejected cleanly: state={state:02x?}, slot5={slot5:02x?}, heap={heap:?}").into());
    }
    context.next += 1;
    Ok(
        json!({"preflight": description, "rejected": true, "state":state, "callback":slot5, "heap":heap, "baseline_heap":context.heap_before}),
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

pub(super) fn run_callback_race(
    session: &Session,
    options: &Options,
    context: &mut PreflightContext,
    baseline: &[u8; 1024],
    guest_timeout_ms: u32,
    wall_timeout_s: u64,
) -> Result<Value, Box<dyn std::error::Error>> {
    use std::time::Instant;
    if context.next != 5 {
        return Err("knob-dialog race step is out of order".into());
    }
    let qmp = &session.engine.management[0];
    let inputs = session.engine.inputs.as_ref().unwrap();
    let notification_semaphore = context.notification_semaphore;
    let widget7 = context.widget7;
    let background10 = context.background10;
    let shared_resource_pointer = context.shared_resource_pointer;
    let original_shared_resource = &context.original_shared_resource;
    let alias_widget = context.alias_widget.ok_or("missing alias widget")?;
    let original_alias_layer = context.alias_layer.as_ref().ok_or("missing alias layer")?;
    let debugger = context
        .debugger
        .as_mut()
        .ok_or("missing knob-dialog debugger")?;
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
    // Simulate the notification manager owning its semaphore during first
    // use. The scene must not depend on that unrelated lock.
    qmp.stop()?;
    let semaphore_count = qmp.read_memory(0, notification_semaphore + 56, 4)?;
    if semaphore_count != 1u32.to_le_bytes() {
        debugger.resume_guest()?;
        return Err("notification semaphore was not available before contention check".into());
    }
    debugger.write(notification_semaphore + 56, &0u32.to_le_bytes())?;
    // Stop at callback registration after the mixer-side scene preflight
    // has passed. Change a borrowed descriptor before DisplayCycle2 can
    // render so the callback-side preflight gets exercised deterministically.
    use firmware_api::l6max::gui_display_callback_register;
    debugger.breakpoint(gui_display_callback_register, true)?;
    debugger.resume_guest()?;
    inputs.knob(0, -1)?;
    let registration_deadline = Instant::now() + Duration::from_secs(10);
    while qmp.query_status()?.running {
        session.poll()?;
        if Instant::now() >= registration_deadline {
            let _ = debugger.breakpoint(gui_display_callback_register, false);
            let _ = debugger.resume_guest();
            return Err("first-use callback registration breakpoint timed out".into());
        }
        thread::sleep(Duration::from_millis(5));
    }
    let registration_pc = debugger.read_register(15)? & !1;
    let callback_slot = debugger.read_register(0)?;
    let callback_pointer = debugger.read_register(1)?;
    if registration_pc != gui_display_callback_register
        || callback_slot != 5
        || callback_pointer & 1 == 0
    {
        let _ = debugger.breakpoint(gui_display_callback_register, false);
        let _ = debugger.resume_guest();
        return Err(format!(
        "unexpected display callback registration: pc={registration_pc:#010x}, slot={callback_slot}, callback={callback_pointer:#010x}"
    )
    .into());
    }
    let callback_entry = callback_pointer & !1;
    debugger.breakpoint(callback_entry, true)?;
    debugger.breakpoint(gui_display_callback_register, false)?;
    debugger.write(alias_widget, &4u16.to_le_bytes())?;
    debugger.resume_guest()?;
    let callback_deadline = Instant::now() + Duration::from_secs(10);
    while qmp.query_status()?.running {
        session.poll()?;
        if Instant::now() >= callback_deadline {
            let _ = debugger.breakpoint(callback_entry, false);
            let _ = debugger.resume_guest();
            return Err("first DisplayCycle2 callback breakpoint timed out".into());
        }
        thread::sleep(Duration::from_millis(5));
    }
    let callback_pc = debugger.read_register(15)? & !1;
    if callback_pc != callback_entry {
        let _ = debugger.breakpoint(callback_entry, false);
        let _ = debugger.resume_guest();
        return Err(format!(
        "first-use race probe stopped at {callback_pc:#010x}, not callback {callback_entry:#010x}"
    )
    .into());
    }
    debugger.breakpoint(callback_entry, false)?;
    debugger.resume_guest()?;
    let race_frame = capture(150, "window-scene-callback-preflight-race")?;
    qmp.stop()?;
    let race_state = qmp.read_memory(0, l6max::gui_bitmap_table_entry_0, 4)?;
    let race_slot5 = qmp.read_memory(0, l6max::gui_display_callback_table + 20, 4)?;
    let callback_preflight_race_rejected = race_frame == *baseline
        && u16::from_le_bytes(race_state[..2].try_into().unwrap()) == 0x4b4e
        && u32::from_le_bytes(race_slot5.clone().try_into().unwrap()) == callback_pointer
        && qmp.read_memory(0, alias_widget, 1)? == [4]
        && qmp.read_memory(0, widget7, 1)? == [1]
        && qmp.read_memory(0, background10, 1)? == [0]
        && qmp.read_memory(0, shared_resource_pointer, 512)? == *original_shared_resource;
    debugger.write(alias_widget, &original_alias_layer)?;
    debugger.resume_guest()?;
    if !callback_preflight_race_rejected {
        return Err(format!(
        "callback-side scene preflight failed to reject descriptor drift: state={race_state:02x?}, slot5={race_slot5:02x?}, frame_unchanged={} ",
        race_frame == *baseline
    )
    .into());
    }

    // Only DisplayCycle2 sets ticks; require an empty mailbox and zero
    // ticks so neither a previous scene nor an old event can satisfy the
    // subsequent callback-return check.
    if race_state[2] != 0 || race_state[3] != u8::MAX {
        return Err(format!(
        "knob-dialog retry requires zero ticks and empty mailbox, found ticks={} pending={:#04x}",
        race_state[2], race_state[3]
    ).into());
    }
    if !supported_display_dispatcher(&qmp.read_memory(
        0,
        DISPLAY_CYCLE2_DISPATCH_GUARD,
        DISPLAY_CYCLE2_RETURN_GUARD.len(),
    )?) {
        return Err("unsupported L6max DisplayCycle2 dispatcher revision".into());
    }

    context.activation_semaphore_count = Some(semaphore_count);
    context.activation_callback = Some(callback_pointer);
    context.next = 6;
    Ok(
        json!({"callback_preflight_race_rejected": true, "callback":callback_pointer, "race_state":race_state, "frame_unchanged":true}),
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

pub(super) fn run_first_activation(
    session: &Session,
    context: &mut PreflightContext,
    guest_timeout_ms: u32,
    wall_timeout_s: u64,
    shared: (std::time::Instant, u32, u32, u64, u16),
) -> Result<Value, Box<dyn std::error::Error>> {
    use std::time::Instant;
    if context.next != 6 {
        return Err("knob-dialog activation step is out of order".into());
    }
    let qmp = &session.engine.management[0];
    let inputs = session.engine.inputs.as_ref().unwrap();
    let notification_semaphore = context.notification_semaphore;
    let widget7 = context.widget7;
    let widget33 = context.widget33;
    let background10 = context.background10;
    let original_widget7 = &context.original_widget7;
    let original_background_layers = &context.original_background_layers;
    let heap_before = &context.heap_before;
    let semaphore_count = context
        .activation_semaphore_count
        .as_ref()
        .ok_or("missing saved semaphore")?;
    let callback_pointer = context
        .activation_callback
        .ok_or("missing callback pointer")?;
    let debugger = context
        .debugger
        .as_mut()
        .ok_or("missing knob-dialog debugger")?;
    let word = |address: u32| -> Result<u32, Box<dyn std::error::Error>> {
        Ok(u32::from_le_bytes(
            qmp.read_memory(0, address, 4)?.try_into().unwrap(),
        ))
    };
    let heap = || -> Result<Vec<u32>, Box<dyn std::error::Error>> {
        Ok(qmp
            .read_memory(0, l6max::xFreeBytesRemaining, 16)?
            .chunks_exact(4)
            .map(|b| u32::from_le_bytes(b.try_into().unwrap()))
            .collect())
    };
    // Recheck the cross-step baseline while stopped: no previous event or
    // callback can satisfy the return observation for this retry.
    qmp.stop()?;
    let before = qmp.read_memory(0, l6max::gui_bitmap_table_entry_0, 4)?;
    let before_slot = word(l6max::gui_display_callback_table + 20)?;
    let before_semaphore = qmp.read_memory(0, notification_semaphore + 56, 4)?;
    if !activation_baseline_ready(&before, before_slot, callback_pointer, &before_semaphore) {
        return Err(format!("knob-dialog activation baseline changed: state={before:02x?}, slot={before_slot:#010x}, semaphore={before_semaphore:02x?}").into());
    }
    if super::execute::deadline_exceeded(
        shared.0.elapsed(),
        shared.1,
        inputs.panel.snapshot().guest_ms,
        shared.2,
        shared.3,
    ) {
        return Err("knob-dialog activation exceeded its shared deadline before retry".into());
    }
    // Enqueue while MAIN is stopped, then resume it. Panel/UART work may
    // still be in flight independently.
    inputs.knob(0, -1)?;
    debugger.resume_guest()?;
    let contention_start = inputs.panel.snapshot().guest_ms;
    let contention_deadline = Instant::now() + Duration::from_secs(wall_timeout_s.min(180));
    loop {
        session.poll()?;
        let state = qmp.read_memory(0, l6max::gui_bitmap_table_entry_0, 4)?;
        let slot = word(l6max::gui_display_callback_table + 20)?;
        if state.len() == 4
            && u16::from_le_bytes([state[0], state[1]]) == 0x4b4e
            && state[2] != 0
            && state[3] == u8::MAX
            && slot & 1 == 1
        {
            // Coherently sample the running word after stopping QEMU. A
            // zero here proves the callback that set ticks returned.
            qmp.stop()?;
            let stopped_state = qmp.read_memory(0, l6max::gui_bitmap_table_entry_0, 4)?;
            let stopped_slot = word(l6max::gui_display_callback_table + 20)?;
            let running = word(DISPLAY_CYCLE2_SLOT5_RUNNING)?;
            if callback_first_use_ready(&stopped_state, stopped_slot, callback_pointer, running) {
                break;
            }
            debugger.resume_guest()?;
        }
        if inputs
            .panel
            .snapshot()
            .guest_ms
            .wrapping_sub(contention_start)
            >= guest_timeout_ms.min(10_000)
            || Instant::now() >= contention_deadline
        {
            return Err("knob dialog did not render while the notification lock was held".into());
        }
        thread::sleep(Duration::from_millis(5));
    }
    let first_state = qmp.read_memory(0, l6max::gui_bitmap_table_entry_0, 4)?;
    let first_slot5 = u32::from_le_bytes(
        qmp.read_memory(0, l6max::gui_display_callback_table + 20, 4)?
            .try_into()
            .unwrap(),
    );
    let heap_while_locked = heap()?;
    let notification_lock_still_held =
        qmp.read_memory(0, notification_semaphore + 56, 4)? == 0u32.to_le_bytes();
    let dialog_without_notification_lock = u16::from_le_bytes(first_state[..2].try_into().unwrap())
== 0x4b4e
&& first_slot5 & 1 == 1
&& qmp.read_memory(0, l6max::gui_bitmap_table_entry_0 + 3, 1)? == u8::MAX.to_le_bytes()
// A nonzero timeout means a scene is showing; its descriptors are
// already back on their stock layers.
&& first_state[2] != 0
&& qmp.read_memory(0, widget7, 16)? == *original_widget7
&& qmp.read_memory(0, background10, 1)? == original_background_layers[0]
&& notification_lock_still_held;
    let first_use_failed = !dialog_without_notification_lock
        || heap_while_locked[2] != heap_before[2]
        || heap_while_locked[3] != heap_before[3];
    // Preserve the stopped snapshot before releasing the lock: ticks and
    // mailbox acknowledgement can become visible before rendering returns.
    let diagnostic = if first_use_failed {
        let widget7_read = qmp
            .read_memory(0, widget7, 16)
            .map_err(|error| error.to_string());
        let widget33_read = qmp
            .read_memory(0, widget33, 16)
            .map_err(|error| error.to_string());
        let background10_read = qmp
            .read_memory(0, background10, 1)
            .map_err(|error| error.to_string());
        let pc = match debugger.read_register(15) {
        Ok(pc) => pc,
        Err(error) => return Err(format!(
            "dialog first use failed: state={first_state:02x?}, slot5={first_slot5:#010x}, heap={heap_while_locked:?}, notification_lock_still_held={notification_lock_still_held}, actual_widget7={widget7_read:02x?}, actual_widget33={widget33_read:02x?}, actual_background10={background10_read:02x?}, stopped_pc_error={error}"
        ).into()),
    };
        Some((widget7_read, widget33_read, background10_read, pc))
    } else {
        None
    };
    debugger.write(notification_semaphore + 56, semaphore_count)?;
    debugger.resume_guest()?;
    if first_use_failed {
        return Err(format!(
        "dialog first use failed: state={first_state:02x?}, slot5={first_slot5:#010x}, heap={heap_while_locked:?}, notification_lock_still_held={notification_lock_still_held}, actual_widget7_widget33_background10_pc={diagnostic:02x?}"
    ).into());
    }

    context.activation_semaphore_count = None;
    context.next = 7;
    Ok(
        json!({"dialog_without_notification_lock": true, "state": first_state, "callback":first_slot5, "heap":heap_while_locked, "notification_lock_still_held":notification_lock_still_held}),
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

pub(super) fn run_active_display(
    session: &Session,
    options: &Options,
    context: &mut PreflightContext,
    baseline: &[u8; 1024],
    guest_timeout_ms: u32,
    wall_timeout_s: u64,
) -> Result<Value, Box<dyn std::error::Error>> {
    use std::time::Instant;
    if context.next != 7 {
        return Err("knob-dialog active display step is out of order".into());
    }
    let main = selected_main_image(options).map_err(std::io::Error::other)?;
    if main.get(0x40c..0x410) != Some(&0x8007eea9u32.to_le_bytes()) {
        return Err("patched startup hook does not reserve popup RAM".into());
    }
    let inputs = session.engine.inputs.as_ref().unwrap();
    let qmp = &session.engine.management[0];
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
    inputs.knob(0, -3)?;
    let active = capture(500, "knob-dialog-active")?;
    let active_callbacks = qmp.read_memory(0, l6max::gui_display_callback_table, 24)?;
    let active_slot5 = u32::from_le_bytes(active_callbacks[20..24].try_into().unwrap());
    if active_slot5 & 1 == 0
        || u32::from_le_bytes(active_callbacks[0..4].try_into().unwrap()) != 0
        || u32::from_le_bytes(active_callbacks[8..12].try_into().unwrap()) != 0
    {
        return Err(format!(
        "first knob event did not claim only free callback slot 5; slots0/2/5={:#010x}/{:#010x}/{active_slot5:#010x}",
        u32::from_le_bytes(active_callbacks[0..4].try_into().unwrap()),
        u32::from_le_bytes(active_callbacks[8..12].try_into().unwrap()),
    )
    .into());
    }
    if active == *baseline {
        return Err("custom overlay did not change the LCD".into());
    }

    context.opened = Some(OpenedOverlay {
        frame: active,
        callback: active_slot5,
    });
    context.next = 8;
    Ok(json!({"display_changed":true, "callback":active_slot5}))
}

pub(super) fn run_level_demo(
    session: &Session,
    options: &Options,
    context: &mut PreflightContext,
    guest_timeout_ms: u32,
    wall_timeout_s: u64,
) -> Result<Value, Box<dyn std::error::Error>> {
    use std::time::Instant;
    if context.next != 8 {
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
    let inputs = session.engine.inputs.as_ref().unwrap();
    let qmp = &session.engine.management[0];
    let text_table = context.text_table;
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
    let decode_text = |address: u32| -> Result<String, Box<dyn std::error::Error>> {
        let pointer = u32::from_le_bytes(qmp.read_memory(0, address, 4)?.try_into().unwrap());
        let bytes = qmp.read_memory(0, pointer, 48)?;
        let units = bytes
            .chunks_exact(2)
            .map(|b| u16::from_le_bytes([b[0], b[1]]))
            .take_while(|c| *c != 0)
            .collect::<Vec<_>>();
        Ok(String::from_utf16(&units)?)
    };
    let capture_demo = |name: &str| -> Result<[u8; 1024], Box<dyn std::error::Error>> {
        let mut frame = capture(100, name)?;
        for _ in 0..6 {
            if frame.iter().map(|byte| byte.count_ones()).sum::<u32>() >= 100 {
                return Ok(frame);
            }
            frame = capture(100, name)?;
        }
        Err(format!("demo frame {name} stayed blank").into())
    };
    // Capture emulator frames while the web demo values move in both
    // directions. The web animation plays these captures at 20 fps.
    capture_demo("knob-dialog-demo-level-start")?;
    let mut level_peak_reached = false;
    // Encoder samples can coalesce while the guest is busy. Keep the target
    // value assertion, but allow enough bounded attempts to reach it.
    for index in 0..128 {
        let displayed = decode_text(text_table + 179 * 4)?;
        let value = displayed
            .strip_suffix('%')
            .and_then(|value| value.parse::<u8>().ok())
            .unwrap_or(0);
        if value >= 69 {
            level_peak_reached = true;
            break;
        }
        inputs.knob(0, 1)?;
        capture_demo(&format!("knob-dialog-demo-level-up-{index:02}"))?;
    }
    if !level_peak_reached {
        return Err("demo level sweep did not reach 69%".into());
    }
    capture(500, "knob-dialog-demo-level-peak")?;
    // The displayed percentage is rounded from a 0..127 firmware value,
    // so 60 encoder steps are not always enough to return to zero.
    for index in 0..128 {
        if decode_text(text_table + 179 * 4)? == "0%" {
            break;
        }
        inputs.knob(0, -1)?;
        capture_demo(&format!("knob-dialog-demo-level-down-{index:02}"))?;
    }
    if decode_text(text_table + 179 * 4)? != "0%" {
        return Err("demo level sweep did not return to zero".into());
    }
    capture(500, "knob-dialog-demo-level-zero")?;

    context.next = 9;
    Ok(json!({"level_demo_peak_at_least_percent":69, "level_demo_final_percent":0}))
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

pub(super) fn run_task_contexts(
    session: &Session,
    options: &Options,
    context: &mut PreflightContext,
    guest_timeout_ms: u32,
    wall_timeout_s: u64,
) -> Result<Value, Box<dyn std::error::Error>> {
    use std::time::Instant;
    if context.next != 9 {
        return Err("knob-dialog task contexts step is out of order".into());
    }
    let inputs = session.engine.inputs.as_ref().unwrap();
    let qmp = &session.engine.management[0];
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
    let debugger = context
        .debugger
        .as_mut()
        .ok_or("missing knob-dialog debugger")?;
    let opened = context.opened.as_ref().ok_or("missing active overlay")?;
    validate_opened_overlay(session, debugger, opened)?;
    let active_slot5 = opened.callback;
    // Observe the producer and consumer task contexts directly. The knob
    // hook runs from the mixer update path; slot 5 runs from DisplayCycle2.
    // This determines whether the atomic mailbox can be replaced safely.
    use firmware_api::l6max::pxCurrentTCB;
    // knob-dialog's call-site hook into `mixer_apply_channel_parameter`
    // (patches/knob-dialog).
    const KNOB_CALL_SITE: u32 = 0x8003_6c74;
    let knob_hook_entry = thumb_bl_target(
        KNOB_CALL_SITE,
        qmp.read_memory(0, KNOB_CALL_SITE, 4)?
            .try_into()
            .map_err(|_| "patched mixer call is not four bytes")?,
    )
    .ok_or("patched mixer dispatcher call is not a Thumb BL")?;
    let callback_probe_entry = active_slot5 & !1;
    qmp.stop()?;
    debugger.breakpoint(knob_hook_entry, true)?;
    debugger.resume_guest()?;
    inputs.knob(0, -1)?;
    let task_probe_deadline = Instant::now() + Duration::from_secs(10);
    while qmp.query_status()?.running {
        session.poll()?;
        if Instant::now() >= task_probe_deadline {
            return Err("knob hook task-context breakpoint timed out".into());
        }
        thread::sleep(Duration::from_millis(5));
    }
    let hook_pc = debugger.read_register(15)? & !1;
    if hook_pc != knob_hook_entry {
        return Err(format!(
            "task probe stopped at {hook_pc:#010x}, not knob hook {knob_hook_entry:#010x}"
        )
        .into());
    }
    let hook_task = u32::from_le_bytes(qmp.read_memory(0, pxCurrentTCB, 4)?.try_into().unwrap());
    debugger.breakpoint(knob_hook_entry, false)?;
    debugger.breakpoint(callback_probe_entry, true)?;
    debugger.resume_guest()?;

    let task_probe_deadline = Instant::now() + Duration::from_secs(10);
    while qmp.query_status()?.running {
        session.poll()?;
        if Instant::now() >= task_probe_deadline {
            let _ = debugger.breakpoint(callback_probe_entry, false);
            let _ = debugger.resume_guest();
            return Err("DisplayCycle2 task-context breakpoint timed out".into());
        }
        thread::sleep(Duration::from_millis(5));
    }
    let callback_pc = debugger.read_register(15)? & !1;
    if callback_pc != callback_probe_entry {
        return Err(format!(
            "task probe stopped at {callback_pc:#010x}, not DisplayCycle2 callback"
        )
        .into());
    }
    let callback_task =
        u32::from_le_bytes(qmp.read_memory(0, pxCurrentTCB, 4)?.try_into().unwrap());
    let tasks = verify_distinct_task_contexts(hook_task, callback_task)?;
    debugger.breakpoint(callback_probe_entry, false)?;
    debugger.resume_guest()?;
    capture(150, "window-scene-task-context-probe")?;

    context.task_contexts = Some(tasks);
    context.next = 10;
    Ok(json!({"mixer_task":hook_task,"display_task":callback_task,"tasks_differ":true}))
}

#[derive(Clone, Copy)]
pub(super) enum OverlayPhase {
    Invariants,
    BitmapZero,
    Refreshed,
}

pub(super) fn run_active_overlay(
    session: &Session,
    options: &Options,
    context: &mut PreflightContext,
    phase: OverlayPhase,
    guest_timeout_ms: u32,
    wall_timeout_s: u64,
) -> Result<Value, Box<dyn std::error::Error>> {
    use std::time::Instant;
    let expected = match phase {
        OverlayPhase::Invariants => 10,
        OverlayPhase::BitmapZero => 11,
        OverlayPhase::Refreshed => 12,
    };
    if context.next != expected {
        return Err("knob-dialog active overlay step is out of order".into());
    }
    let main = selected_main_image(options).map_err(std::io::Error::other)?;
    if main.get(0x40c..0x410) != Some(&0x8007eea9u32.to_le_bytes()) {
        return Err("patched startup hook does not reserve popup RAM".into());
    }
    let inputs = session.engine.inputs.as_ref().unwrap();
    let qmp = &session.engine.management[0];
    let heap = || -> Result<Vec<u32>, Box<dyn std::error::Error>> {
        Ok(qmp
            .read_memory(0, l6max::xFreeBytesRemaining, 16)?
            .chunks_exact(4)
            .map(|b| u32::from_le_bytes(b.try_into().unwrap()))
            .collect())
    };
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
    let expected_value = |mode: u32, raw: u8| -> String {
        let table = |offset: usize| {
            f32::from_le_bytes(
                main[offset + raw as usize * 4..offset + raw as usize * 4 + 4]
                    .try_into()
                    .unwrap(),
            )
        };
        match mode {
            0 | 2 | 3 => {
                let db = table(0xa5138);
                if db > 0.0 {
                    format!("+{db:.2} dB")
                } else {
                    format!("{db:.2} dB")
                }
            }
            1 => {
                let hz = table(0xa5738);
                if hz < 1000.0 {
                    format!("{hz:.0} Hz")
                } else {
                    format!("{:.2} kHz", hz / 1000.0)
                }
            }
            8 => match raw {
                63 | 64 => "CENTER".into(),
                0..=62 => format!("L {}%", ((63 - raw as u32) * 100 + 31) / 63),
                _ => format!("R {}%", ((raw as u32 - 64) * 100 + 31) / 63),
            },
            _ => format!("{}%", (raw as u32 * 100 + 63) / 127),
        }
    };
    let decode_text = |address: u32| -> Result<String, Box<dyn std::error::Error>> {
        let pointer = u32::from_le_bytes(qmp.read_memory(0, address, 4)?.try_into().unwrap());
        let bytes = qmp.read_memory(0, pointer, 48)?;
        let units = bytes
            .chunks_exact(2)
            .map(|b| u16::from_le_bytes([b[0], b[1]]))
            .take_while(|c| *c != 0)
            .collect::<Vec<_>>();
        Ok(String::from_utf16(&units)?)
    };
    let background_pointer = |index: u32| -> Result<u32, Box<dyn std::error::Error>> {
        Ok(u32::from_le_bytes(
            qmp.read_memory(0, l6max::gui_background_table + index * 4, 4)?
                .try_into()
                .unwrap(),
        ))
    };
    let original_window = &context.original_window;
    let original_bitmap = &context.original_bitmap;
    let text_table = context.text_table;
    let shared_resource_pointer = context.shared_resource_pointer;
    let widget7 = context.widget7;
    let widget33 = context.widget33;
    let original_widget7 = &context.original_widget7;
    let original_widget33 = &context.original_widget33;
    let background10 = context.background10;
    let background37 = context.background37;
    let background48 = context.background48;
    let original_background_layers = &context.original_background_layers;
    let heap_before = &context.heap_before;
    let debugger = context
        .debugger
        .as_mut()
        .ok_or("missing knob-dialog debugger")?;
    validate_opened_overlay(
        session,
        debugger,
        context.opened.as_ref().ok_or("missing active overlay")?,
    )?;
    let opened = context.opened.as_ref().unwrap();
    let active = opened.frame;
    let active_slot5 = opened.callback;
    let TaskContexts {
        hook_task,
        callback_task,
    } = context.task_contexts.ok_or("missing task contexts")?;
    if matches!(phase, OverlayPhase::Invariants) {
        if u32::from_le_bytes(
            qmp.read_memory(0, l6max::current_window, 4)?
                .try_into()
                .unwrap(),
        ) != u32::from_le_bytes(original_window.clone().try_into().unwrap())
        {
            return Err("custom overlay replaced the active firmware window".into());
        }
        // The scene borrows the descriptors only while it draws, so a window that
        // opens over it draws its own title on the stock layers.
        let shown = (
            qmp.read_memory(0, widget7, 16)?,
            qmp.read_memory(0, widget33, 16)?,
            [background10, background37, background48]
                .into_iter()
                .map(|background| qmp.read_memory(0, background, 1))
                .collect::<Result<Vec<_>, _>>()?,
        );
        let stock = (
            original_widget7.clone(),
            original_widget33.clone(),
            original_background_layers.to_vec(),
        );
        if shown != stock {
            return Err(format!(
    "custom overlay kept borrowed descriptors after drawing: widgets and background layers are {shown:02x?}, stock {stock:02x?}"
)
.into());
        }
        if u32::from_le_bytes(
            qmp.read_memory(0, text_table + 179 * 4, 4)?
                .try_into()
                .unwrap(),
        ) != shared_resource_pointer
        {
            return Err("dialog redirected the shared text-resource pointer".into());
        }
        let black_pixel = |x: usize, y: usize| {
            let index = ((63 - y) / 8) * 128 + x;
            active[index] & (1 << ((63 - y) % 8)) == 0
        };
        if ![(0, 0), (127, 0), (0, 63), (127, 63)]
            .into_iter()
            .all(|(x, y)| black_pixel(x, y))
        {
            return Err("stock bitmap 22 did not paint the overlay background black".into());
        }
        let state = l6max::gui_bitmap_table_entry_0;
        if u16::from_le_bytes(qmp.read_memory(0, state, 2)?.try_into().unwrap()) != 0x4b4e
            || qmp.read_memory(0, l6max::gui_bitmap_table_entry_0 + 36 * 4, 4)? != *original_bitmap
        {
            return Err("in-place state signature or stock bitmap-36 descriptor is invalid".into());
        }
        let heap_live = heap()?;
        if heap_live[0] != heap_before[0]
            || heap_live[2] != heap_before[2]
            || heap_live[3] != heap_before[3]
        {
            return Err(format!(
                "dialog state should not allocate heap: before={heap_before:?}, live={heap_live:?}"
            )
            .into());
        }
        let state_heap_bytes = heap_before[0]
            .checked_sub(heap_live[0])
            .ok_or("firmware heap free-byte count increased while allocating dialog state")?;
        if state_heap_bytes != 0 {
            return Err(format!(
                "dialog state should not consume heap bytes, got {state_heap_bytes}"
            )
            .into());
        }
        context.overlay_heap = Some((heap_live, state_heap_bytes));
        context.next = 11;
        return Ok(json!({"overlay_invariants": true}));
    }
    if matches!(phase, OverlayPhase::BitmapZero) {
        let state = l6max::gui_bitmap_table_entry_0;
        let null_bitmap_background = background_pointer(47)?;
        if u16::from_le_bytes(
            qmp.read_memory(0, null_bitmap_background + 2, 2)?
                .try_into()
                .unwrap(),
        ) != 0
        {
            return Err("background 47 no longer uses bitmap ID 0".into());
        }
        let before_null_bitmap_draw = capture(50, "window-scene-before-null-bitmap-probe")?;
        // Call the draw routine from the display task, at the dialog's own
        // callback entry. An arbitrary stop can land in the idle task or an
        // exception handler, where the routine can block past the deadline.
        qmp.stop()?;
        debugger.breakpoint(active_slot5 & !1, true)?;
        debugger.resume_guest()?;
        let display_task_deadline = Instant::now() + Duration::from_secs(15);
        while qmp.query_status()?.running {
            session.poll()?;
            if Instant::now() >= display_task_deadline {
                return Err("dialog callback breakpoint was not reached".into());
            }
            thread::sleep(Duration::from_millis(5));
        }
        debugger.breakpoint(active_slot5 & !1, false)?;
        debugger.call_thumb(l6max::gui_draw_background, &[47], Duration::from_secs(5))?;
        debugger.resume_guest()?;
        let after_null_bitmap_draw = capture(100, "window-scene-after-null-bitmap-probe")?;
        if before_null_bitmap_draw != after_null_bitmap_draw
            || u16::from_le_bytes(qmp.read_memory(0, state, 2)?.try_into().unwrap()) != 0x4b4e
            || qmp.read_memory(0, l6max::gui_bitmap_table_entry_0 + 36 * 4, 4)? != *original_bitmap
        {
            return Err(
                "bitmap ID 0 did not safely short-circuit while holding the state handle".into(),
            );
        }
        context.next = 12;
        return Ok(json!({"bitmap_zero_guard_safe": true}));
    }
    let (heap_live, state_heap_bytes) = context
        .overlay_heap
        .take()
        .ok_or("missing overlay invariants")?;
    let value_text = decode_text(text_table + 179 * 4)?;
    let raw = qmp.read_memory(0, l6max::mixer_channel_levels, 1)?[0];
    if !knob_display_matches(9, &expected_value(9, raw), &value_text) {
        return Err(format!("custom window value mismatch: {value_text:?}").into());
    }
    let refreshed = capture(500, "window-scene-refreshed")?;
    let pixel = |frame: &[u8; 1024], x: usize, y: usize| {
        frame[((63 - y) / 8) * 128 + x] & (1 << ((63 - y) % 8)) != 0
    };
    // The value has intentionally changed during the level demonstration.
    // Compare the rest of the LCD, including the title and black backing,
    // to make sure normal recorder redraws did not replace the overlay.
    let stable_overlay = (0..64).all(|y| {
        (0..128).all(|x| {
            (30..97).contains(&x) && (33..64).contains(&y)
                || pixel(&active, x, y) == pixel(&refreshed, x, y)
        })
    });
    if !stable_overlay {
        return Err("recorder redraw changed the custom overlay outside its value widget".into());
    }

    context.active = Some(ActiveSnapshot {
        active_slot5,
        heap_live,
        state_heap_bytes,
        hook_task,
        callback_task,
        refreshed,
    });
    context.next = 13;
    Ok(
        json!({"active_overlay":true, "callback":active_slot5, "heap_bytes":state_heap_bytes, "mixer_task":hook_task, "display_task":callback_task}),
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

pub(super) fn run_locked_value_update(
    session: &Session,
    options: &Options,
    context: &mut PreflightContext,
    guest_timeout_ms: u32,
    wall_timeout_s: u64,
) -> Result<Value, Box<dyn std::error::Error>> {
    use std::time::Instant;
    if context.next != 13 {
        return Err("knob-dialog locked update step is out of order".into());
    }
    let qmp = &session.engine.management[0];
    let inputs = session.engine.inputs.as_ref().unwrap();
    let notification_semaphore = context.notification_semaphore;
    let text_table = context.text_table;
    let state = l6max::gui_bitmap_table_entry_0;
    let debugger = context
        .debugger
        .as_mut()
        .ok_or("missing knob-dialog debugger")?;
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
    let decode_text = |address: u32| -> Result<String, Box<dyn std::error::Error>> {
        let pointer = u32::from_le_bytes(qmp.read_memory(0, address, 4)?.try_into().unwrap());
        let bytes = qmp.read_memory(0, pointer, 48)?;
        let units = bytes
            .chunks_exact(2)
            .map(|b| u16::from_le_bytes([b[0], b[1]]))
            .take_while(|c| *c != 0)
            .collect::<Vec<_>>();
        Ok(String::from_utf16(&units)?)
    };
    // Hold the notification semaphore while a second value is rendered.
    // The patch's dedicated layer/widget resources and persistent bitmap
    // descriptor must remain usable without taking this unrelated lock.
    qmp.stop()?;
    let semaphore_count = qmp.read_memory(0, notification_semaphore + 56, 4)?;
    if semaphore_count != 1u32.to_le_bytes() {
        debugger.resume_guest()?;
        return Err(
            "notification semaphore was not available before callback contention check".into(),
        );
    }
    debugger.write(notification_semaphore + 56, &0u32.to_le_bytes())?;
    debugger.resume_guest()?;
    turn_and_wait_hook(session, debugger, -1, wall_timeout_s)?;
    let callback_busy_start = inputs.panel.snapshot().guest_ms;
    let callback_deadline = Instant::now() + Duration::from_secs(wall_timeout_s.min(180));
    loop {
        session.poll()?;
        let raw = qmp.read_memory(0, l6max::mixer_channel_levels, 1)?[0];
        let pending = qmp.read_memory(0, state + 3, 1)?;
        let displayed = if pending == u8::MAX.to_le_bytes() {
            Some(decode_text(text_table + 179 * 4)?)
        } else {
            None
        };
        let expected = locked_level_expected(raw);
        if locked_update_ready(&pending, displayed.as_deref(), raw) {
            break;
        }
        if inputs
            .panel
            .snapshot()
            .guest_ms
            .wrapping_sub(callback_busy_start)
            >= guest_timeout_ms.min(10_000)
            || Instant::now() >= callback_deadline
        {
            return Err(
            format!("knob dialog did not update while the notification lock was held: pending={pending:02x?}, raw={raw}, displayed={displayed:?}, expected={expected:?}").into(),
        );
        }
        thread::sleep(Duration::from_millis(5));
    }
    qmp.stop()?;
    let pending_during_contention = qmp.read_memory(0, state + 3, 1)?;
    let lock_held_during_render =
        qmp.read_memory(0, notification_semaphore + 56, 4)? == 0u32.to_le_bytes();
    let dialog_value_during_lock = decode_text(text_table + 179 * 4)?;
    let raw_during_lock = qmp.read_memory(0, l6max::mixer_channel_levels, 1)?[0];
    debugger.write(notification_semaphore + 56, &semaphore_count)?;
    debugger.resume_guest()?;
    if pending_during_contention != u8::MAX.to_le_bytes()
        || !lock_held_during_render
        || !knob_display_matches(
            9,
            &locked_level_expected(raw_during_lock),
            &dialog_value_during_lock,
        )
    {
        return Err(format!(
        "dialog did not process the latest update while the notification semaphore was unavailable: pending={pending_during_contention:02x?}, value={dialog_value_during_lock:?}"
    )
    .into());
    }
    capture(500, "window-scene-after-callback-contention")?;
    let pending_after_contention = qmp.read_memory(0, state + 3, 1)?;
    if pending_after_contention != u8::MAX.to_le_bytes() {
        return Err(format!(
        "display callback did not consume the pending update while the lock was held: pending={pending_after_contention:02x?}"
    )
    .into());
    }

    context.next = 14;
    Ok(
        json!({"notification_semaphore_not_required_for_refresh": true,
        "pending_during": pending_during_contention, "pending_after":pending_after_contention,
        "lock_held":lock_held_during_render, "raw":raw_during_lock, "displayed":dialog_value_during_lock}),
    )
}

/// Start one declared mode without transferring the live debugger out of State.
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
    let inputs = session.engine.inputs.as_ref().unwrap();
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
}

pub(super) fn run_knob_dialog(
    prefix: bool,
    session: &Session,
    options: &Options,
    context_slot: &mut Option<PreflightContext>,
    guest_timeout_ms: u32,
    wall_timeout_s: u64,
    shared: Option<(Instant, u32, u32, u64, u16)>,
    operation_start: (Instant, u32),
) -> Result<Value, Box<dyn std::error::Error>> {
    use std::time::Instant;

    let main = selected_main_image(options).map_err(std::io::Error::other)?;
    if main.get(0x40c..0x410) != Some(&0x8007eea9u32.to_le_bytes()) {
        return Err("patched startup hook does not reserve popup RAM".into());
    }
    let inputs = session.engine.inputs.as_ref().unwrap();
    let qmp = &session.engine.management[0];
    let input_control = l6ctl::OwnedControl::new(&session.engine);
    let check_deadline = || -> Result<(), Box<dyn std::error::Error>> {
        let guest_now = inputs.panel.snapshot().guest_ms;
        if operation_start.0.elapsed() > Duration::from_secs(wall_timeout_s)
            || guest_now.wrapping_sub(operation_start.1) > guest_timeout_ms
            || shared.is_some_and(|(start, guest, budget, wall, _)| {
                start.elapsed() > Duration::from_secs(wall)
                    || guest_now.wrapping_sub(guest) > budget
            })
        {
            return Err("knob-dialog shared deadline exceeded while awaiting channel value".into());
        }
        Ok(())
    };
    // MAIN 1.10's parameter selector writes this byte before returning;
    // the corresponding getter at 0x8000b4a8 reads it.
    const SELECTED_PARAMETER_MODE: u32 = 0x8046_2d2c;
    let wait_parameter_mode = |expected: u8| -> Result<(), Box<dyn std::error::Error>> {
        let start = inputs.panel.snapshot().guest_ms;
        let deadline = Instant::now() + Duration::from_secs(wall_timeout_s.min(180));
        loop {
            session.poll()?;
            let actual = qmp.read_memory(0, SELECTED_PARAMETER_MODE, 1)?[0];
            if actual == expected {
                return Ok(());
            }
            if inputs.panel.snapshot().guest_ms.wrapping_sub(start) >= guest_timeout_ms.min(10_000)
                || Instant::now() >= deadline
            {
                return Err(
                    format!("parameter mode did not become {expected}; found {actual}").into(),
                );
            }
            thread::sleep(Duration::from_millis(5));
        }
    };
    let heap = || -> Result<Vec<u32>, Box<dyn std::error::Error>> {
        Ok(qmp
            .read_memory(0, l6max::xFreeBytesRemaining, 16)?
            .chunks_exact(4)
            .map(|b| u32::from_le_bytes(b.try_into().unwrap()))
            .collect())
    };
    let capture = |duration: u32, name: &str| {
        capture_knob_frame(
            session,
            options,
            duration,
            name,
            guest_timeout_ms,
            wall_timeout_s,
        )
    };
    let expected_value = |mode: u32, raw: u8| -> String {
        let table = |offset: usize| {
            f32::from_le_bytes(
                main[offset + raw as usize * 4..offset + raw as usize * 4 + 4]
                    .try_into()
                    .unwrap(),
            )
        };
        match mode {
            0 | 2 | 3 => {
                let db = table(0xa5138);
                if db > 0.0 {
                    format!("+{db:.2} dB")
                } else {
                    format!("{db:.2} dB")
                }
            }
            1 => {
                let hz = table(0xa5738);
                if hz < 1000.0 {
                    format!("{hz:.0} Hz")
                } else {
                    format!("{:.2} kHz", hz / 1000.0)
                }
            }
            8 => match raw {
                63 | 64 => "CENTER".into(),
                0..=62 => format!("L {}%", ((63 - raw as u32) * 100 + 31) / 63),
                _ => format!("R {}%", ((raw as u32 - 64) * 100 + 31) / 63),
            },
            _ => format!("{}%", (raw as u32 * 100 + 63) / 127),
        }
    };
    let non_recorder_guard_passed = true;
    let decode_text = |address: u32| -> Result<String, Box<dyn std::error::Error>> {
        let pointer = u32::from_le_bytes(qmp.read_memory(0, address, 4)?.try_into().unwrap());
        let bytes = qmp.read_memory(0, pointer, 48)?;
        let units = bytes
            .chunks_exact(2)
            .map(|b| u16::from_le_bytes([b[0], b[1]]))
            .take_while(|c| *c != 0)
            .collect::<Vec<_>>();
        Ok(String::from_utf16(&units)?)
    };
    let capture_demo = |name: &str| -> Result<[u8; 1024], Box<dyn std::error::Error>> {
        let mut frame = capture(100, name)?;
        for _ in 0..6 {
            if frame.iter().map(|byte| byte.count_ones()).sum::<u32>() >= 100 {
                return Ok(frame);
            }
            frame = capture(100, name)?;
        }
        Err(format!("demo frame {name} stayed blank").into())
    };
    if context_slot.as_ref().is_none_or(|context| {
        context.next != 14 || context.mode_index == 0 || context.last_mode != Some(8)
    }) {
        return Err("knob-dialog preflight incomplete".into());
    }
    let context = context_slot.as_ref().unwrap();
    if context.final_phase != if prefix { 0 } else { 21 } {
        return Err("knob-dialog final phases out of order".into());
    }
    if context.alias_widget.is_none()
        || context.alias_layer.is_none()
        || context.debugger.is_none()
        || context.active.is_none()
    {
        return Err("incomplete knob-dialog preflight context".into());
    }
    let mut context = context_slot.take().unwrap();

    let active_snapshot = context.active.as_ref().unwrap().clone();
    let debugger = context.debugger.take().unwrap();
    let original_window = context.original_window.clone();
    let original_bitmap = context.original_bitmap.clone();
    let original_bitmap_descriptor = context.original_bitmap_descriptor.clone();
    let original_notification_semaphore = context.original_notification_semaphore.clone();
    let text_table = context.text_table;
    let original_resources = context.original_resources.clone();
    let shared_resource_pointer = context.shared_resource_pointer;
    let original_shared_resource = context.original_shared_resource.clone();
    let original_lengths_table = context.original_lengths_table;
    let original_lengths = context.original_lengths.clone();
    let widget7 = context.widget7;
    let widget33 = context.widget33;
    let original_widget7 = context.original_widget7.clone();
    let original_widget33 = context.original_widget33.clone();
    let background10 = context.background10;
    let background37 = context.background37;
    let background48 = context.background48;
    let original_background_layers = context.original_background_layers.clone();
    let widget_layer_guard_rejected = true;
    let background_layer_guard_rejected = true;
    let callback_preflight_race_rejected = true;
    let result = (|| -> Result<Value, Box<dyn std::error::Error>> {
        let ActiveSnapshot {
            active_slot5,
            heap_live,
            state_heap_bytes,
            hook_task,
            callback_task,
            refreshed: _,
        } = active_snapshot;
        let state = l6max::gui_bitmap_table_entry_0;
        if prefix {
            // The preceding mode walk ends on Pan with a verified live scene.
            // Sweep from center through left 50% to right 50%, capturing at 20 fps.
            capture_demo("knob-dialog-demo-pan-center")?;
            if decode_text(text_table + 179 * 4)? != "CENTER" {
                return Err("demo pan sweep did not start at center".into());
            }
            use firmware_api::l6max::mixer_first_channel_pan;
            let pan_start = qmp.read_memory(0, mixer_first_channel_pan, 1)?[0];
            for name in ["left", "right"] {
                let mut reached_target = false;
                for index in 0..100 {
                    let raw = qmp.read_memory(0, mixer_first_channel_pan, 1)?[0];
                    let displayed = decode_text(text_table + 179 * 4)?;
                    let target_reached = if name == "left" {
                        displayed
                            .strip_prefix("L ")
                            .and_then(|value| value.strip_suffix('%'))
                            .and_then(|value| value.parse::<u8>().ok())
                            .is_some_and(|value| value >= 48)
                    } else {
                        displayed
                            .strip_prefix("R ")
                            .and_then(|value| value.strip_suffix('%'))
                            .and_then(|value| value.parse::<u8>().ok())
                            .is_some_and(|value| value >= 48)
                    };
                    if target_reached {
                        reached_target = true;
                        break;
                    }
                    let target = if name == "left" { 32 } else { 95 };
                    inputs.knob(0, if raw < target { 1 } else { -1 })?;
                    capture_demo(&format!("knob-dialog-demo-pan-{name}-{index:02}"))?;
                }
                if !reached_target {
                    return Err(format!("demo pan sweep did not reach {name} 50%").into());
                }
                capture(500, &format!("knob-dialog-demo-pan-{name}-end"))?;
            }
            for _ in 0..100 {
                let raw = qmp.read_memory(0, mixer_first_channel_pan, 1)?[0];
                if raw == pan_start {
                    break;
                }
                inputs.knob(0, if raw < pan_start { 1 } else { -1 })?;
                capture(50, "knob-dialog-demo-pan-reset")?;
            }
            inputs.tap(35, 50)?;
            wait_parameter_mode(9)?;
            let channel0_level = l6max::mixer_channel_levels;
            let channel1_level = l6max::mixer_channel_levels + 1;
            let channel0_raw = qmp.read_memory(0, channel0_level, 1)?[0];
            let mut channel1_raw = qmp.read_memory(0, channel1_level, 1)?[0];
        let mut observed_change = false;
        for delta in [-1, 1, -2, 2] {
            check_deadline()?;
            let before = channel1_raw;
            // Gesture polarity is the inverse of the encoder's wire steps.
            let receipt = input_control.encoder(2, -delta)?;
            loop {
                session.poll()?;
                check_deadline()?;
                match input_control
                    .input_ack(l6ctl::InputSource::Panel, receipt.sequence)?
                    .state
                {
                    l6ctl::InputAckState::Applied => break,
                    l6ctl::InputAckState::Pending => thread::sleep(Duration::from_millis(5)),
                    l6ctl::InputAckState::Rejected => {
                        return Err("channel-2 encoder input rejected".into());
                    }
                    l6ctl::InputAckState::Unknown => {
                        return Err("channel-2 encoder acknowledgement unavailable".into());
                    }
                }
            }
            // A clamped negative (or positive at the top) is an intentional
            // no-change probe: wait for its receipt but do not require a new value.
            let clamped = channel_level_turn_clamped(before, delta);
            if !clamped {
                loop {
                    session.poll()?;
                    check_deadline()?;
                    channel1_raw = qmp.read_memory(0, channel1_level, 1)?[0];
                    if channel1_raw != before {
                        observed_change = true;
                        break;
                    }
                    thread::sleep(Duration::from_millis(5));
                }
            }
            capture(250, "window-scene-channel-2-level")?;
            channel1_raw = qmp.read_memory(0, channel1_level, 1)?[0];
            if clamped {
                continue;
            }
            // Different raw values can round to the same displayed text.
            if expected_value(9, channel1_raw) == expected_value(9, channel0_raw) {
                continue;
            }
            if channel1_raw != channel0_raw {
                channel1_raw = wait_for_channel_level(
                    channel0_raw,
                    || {
                        session.poll().map_err(|e| e.to_string())?;
                        let raw = qmp
                            .read_memory(0, channel1_level, 1)
                            .map_err(|e| e.to_string())?[0];
                        let text = decode_text(text_table + 179 * 4).map_err(|e| e.to_string())?;
                        Ok((raw, text))
                    },
                    || check_deadline().is_err(),
                    || thread::sleep(Duration::from_millis(5)),
                )?;
                break;
            }
        }
        let channel1_display = decode_text(text_table + 179 * 4)?;
        if !observed_change
            || channel1_raw == channel0_raw
            || expected_value(9, channel1_raw) == expected_value(9, channel0_raw)
            || !knob_display_matches(9, &expected_value(9, channel1_raw), &channel1_display)
        {
            return Err(format!(
                "channel-2 event did not display its own LEVEL value: channel0={channel0_raw}, channel1={channel1_raw}, displayed={channel1_display:?}"
            )
            .into());
            }
            return Ok(json!({"pan_and_level_checked": true}));
        }
        let invalid_metadata_rejected = true;
        let invalid_tag_discarded = true;
        let notification = context
            .pending_notification
            .as_ref()
            .ok_or("missing notification context")?;
        let notification_frame = notification.frame.ok_or("missing notification frame")?;
        let after_notification = notification
            .after
            .ok_or("missing cleared notification frame")?;
        let timeout_start = inputs.panel.snapshot().guest_ms;
        loop {
            let ticks = qmp.read_memory(0, state + 2, 1)?[0];
            if ticks == 0 {
                break;
            }
            let now = inputs.panel.snapshot().guest_ms;
            // The guest countdown can land on its final tick while host-side
            // QEMU work is contending with other parallel cases. Keep a full
            // grace interval for that last tick before treating it as stuck.
            if now.wrapping_sub(timeout_start) >= 10_000 {
                return Err(format!(
                    "custom scene timeout did not finish after notification expiry; {ticks} ticks remain"
                )
                .into());
            }
            wait_guest(session, now, 100, guest_timeout_ms, wall_timeout_s)?;
        }
        let timeout_cleared = capture(150, "knob-dialog-timeout-cleared")?;
        if timeout_cleared == after_notification {
            return Err("knob dialog timeout screenshot still shows the custom scene".into());
        }
        if timeout_cleared == notification_frame {
            return Err("knob dialog timeout screenshot still shows the notification".into());
        }
        let restore_checks = [
            (
                "window",
                qmp.read_memory(0, l6max::current_window, 4)? == original_window,
            ),
            (
                "state signature",
                u16::from_le_bytes(qmp.read_memory(0, state, 2)?.try_into().unwrap()) == 0x4b4e,
            ),
            (
                "bitmap table",
                qmp.read_memory(0, l6max::gui_bitmap_table_entry_0 + 36 * 4, 4)? == original_bitmap,
            ),
            (
                "bitmap descriptor",
                qmp.read_memory(
                    0,
                    u32::from_le_bytes(original_bitmap.clone().try_into().unwrap()),
                    8,
                )? == original_bitmap_descriptor,
            ),
            (
                "notification semaphore",
                qmp.read_memory(0, l6max::notification_slot, 4)? == original_notification_semaphore,
            ),
            (
                "text resource table",
                qmp.read_memory(0, text_table + 229 * 4, 8)? == original_resources,
            ),
            (
                "shared resource pointer",
                qmp.read_memory(0, text_table + 179 * 4, 4)?
                    == shared_resource_pointer.to_le_bytes(),
            ),
            (
                "shared resource contents",
                qmp.read_memory(0, shared_resource_pointer, 512)? == original_shared_resource,
            ),
            (
                "text lengths",
                qmp.read_memory(0, original_lengths_table + 229 * 2, 4)? == original_lengths,
            ),
            (
                "widget 7",
                qmp.read_memory(0, widget7, 16)? == original_widget7,
            ),
            (
                "widget 33",
                qmp.read_memory(0, widget33, 16)? == original_widget33,
            ),
            (
                "background layer 10",
                qmp.read_memory(0, background10, 1)? == original_background_layers[0],
            ),
            (
                "background layer 37",
                qmp.read_memory(0, background37, 1)? == original_background_layers[1],
            ),
            (
                "background layer 48",
                qmp.read_memory(0, background48, 1)? == original_background_layers[2],
            ),
            (
                "display callback slot 5",
                u32::from_le_bytes(
                    qmp.read_memory(0, l6max::gui_display_callback_table + 20, 4)?
                        .try_into()
                        .unwrap(),
                ) == active_slot5,
            ),
        ];
        let mismatches = restore_checks
            .iter()
            .filter_map(|(name, matches)| (!matches).then_some(*name))
            .collect::<Vec<_>>();
        if !mismatches.is_empty() {
            return Err(format!(
                "custom overlay did not restore borrowed resources or preserve stock bitmap 36: {mismatches:?}"
            )
            .into());
        }
        let heap_after = heap()?;
        if heap_after[0] != heap_live[0]
            || heap_after[2] != heap_live[2]
            || heap_after[3] != heap_live[3]
        {
            return Err(format!(
                "in-place scene state caused heap changes: live={heap_live:?}, after={heap_after:?}"
            )
            .into());
        }
        // Prove that the DisplayCycle2 callback restores borrowed global
        // descriptors on navigation, rather than relying on expiry cleanup.
        inputs.knob(0, -3)?;
        capture(400, "window-scene-before-navigation")?;
        inputs.main.tap(0, 120)?;
        capture(500, "window-scene-navigation-returned")?;
        let navigated_window = qmp.read_memory(0, l6max::current_window, 4)?;
        if navigated_window == original_window
            || u16::from_le_bytes(qmp.read_memory(0, state, 2)?.try_into().unwrap()) != 0x4b4e
            || qmp.read_memory(0, l6max::gui_bitmap_table_entry_0 + 36 * 4, 4)? != original_bitmap
            || qmp.read_memory(
                0,
                u32::from_le_bytes(original_bitmap.clone().try_into().unwrap()),
                8,
            )? != original_bitmap_descriptor
            || qmp.read_memory(0, l6max::notification_slot, 4)? != original_notification_semaphore
            || qmp.read_memory(0, shared_resource_pointer, 512)? != original_shared_resource
            || qmp.read_memory(0, widget7, 16)? != original_widget7
            || qmp.read_memory(0, widget33, 16)? != original_widget33
            || qmp.read_memory(0, background10, 1)? != original_background_layers[0]
            || qmp.read_memory(0, background37, 1)? != original_background_layers[1]
            || qmp.read_memory(0, background48, 1)? != original_background_layers[2]
            || u32::from_le_bytes(
                qmp.read_memory(0, l6max::gui_display_callback_table + 20, 4)?
                    .try_into()
                    .unwrap(),
            ) != active_slot5
        {
            return Err(
                "window-push cleanup did not restore the dialog's borrowed resources".into(),
            );
        }
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
    if prefix && result.is_ok() {
        context.final_phase = 1;
        context.debugger = Some(debugger);
        *context_slot = Some(context);
        result
    } else {
        debugger.finish(result)
    }
}

struct PendingNotification {
    text_before: Vec<u8>,
    breakpoint_deadline: Option<Instant>,
    guest_start: Option<u32>,
    frame: Option<[u8; 1024]>,
    after: Option<[u8; 1024]>,
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
        frame: None,
        after: None,
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

pub(super) fn invoke_notification(
    session: &Session,
    context: &mut PreflightContext,
    message: u32,
) -> Result<Value, Box<dyn std::error::Error>> {
    if context.final_phase != 16 {
        return Err("notification invocation out of order".into());
    }
    let debugger = context.debugger.as_mut().ok_or("missing knob debugger")?;
    debugger.register(0, message)?;
    debugger.register(15, l6max::gui_notification_show)?;
    debugger.resume_guest()?;
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
) -> Result<Value, Box<dyn std::error::Error>> {
    if context.final_phase != if clear { 19 } else { 17 } {
        return Err("notification frame wait out of order".into());
    }
    let pending = context
        .pending_notification
        .as_ref()
        .ok_or("missing notification context")?;
    let start = pending.guest_start.ok_or("missing notification start")?;
    let baseline = if clear {
        pending.frame.ok_or("missing notification frame")?
    } else {
        context
            .active
            .as_ref()
            .ok_or("missing active snapshot")?
            .refreshed
    };
    let frame = loop {
        let frame = capture_knob_frame(
            session,
            options,
            guest_ms,
            artifact,
            guest_timeout_ms,
            wall_timeout_s,
        )?;
        if notification_frame_wait(frame != baseline, start, start, limit_ms)
            == NotificationFrameWait::Changed
        {
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
    context.final_phase += 1;
    Ok(json!({"clear":clear}))
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
) -> Result<Value, Box<dyn std::error::Error>> {
    if context.final_phase != 8 {
        return Err("signature injection out of order".into());
    }
    let qmp = &session.engine.management[0];
    let debugger = context.debugger.as_mut().ok_or("missing knob debugger")?;
    qmp.stop()?;
    let address = l6max::gui_bitmap_table_entry_0;
    let original = qmp.read_memory(0, address, 4)?;
    let invalid = u32::from_le_bytes(
        original
            .clone()
            .try_into()
            .map_err(|_| "short signature read")?,
    ) ^ 1;
    debugger.write(address, &invalid.to_le_bytes())?;
    debugger.resume_guest()?;
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
    context: &mut PreflightContext,
) -> Result<Value, Box<dyn std::error::Error>> {
    if context.final_phase != 11 {
        return Err("signature restoration out of order".into());
    }
    let pending = context
        .pending_invalid_signature
        .as_ref()
        .ok_or("missing signature probe")?;
    let rejected = pending.rejected.ok_or("missing signature sample")?;
    let debugger = context.debugger.as_mut().ok_or("missing knob debugger")?;
    debugger.write(l6max::gui_bitmap_table_entry_0, &pending.original)?;
    debugger.resume_guest()?;
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

pub(super) fn final_phase(context: &PreflightContext) -> u8 {
    context.final_phase
}

pub(super) fn inject_invalid_tag(
    session: &Session,
    context: &mut PreflightContext,
    tag: u8,
    wall_timeout_s: u64,
) -> Result<Value, Box<dyn std::error::Error>> {
    if !matches!(context.final_phase, 1 | 4) {
        return Err("invalid tag injection out of order".into());
    }
    let qmp = &session.engine.management[0];
    let debugger = context.debugger.as_mut().ok_or("missing knob debugger")?;
    qmp.stop()?;
    let address = l6max::gui_bitmap_table_entry_0 + 3;
    debugger.write(address, &[tag])?;
    debugger.handoff_mailbox(address);
    debugger.resume_guest()?;
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
) -> Result<Value, Box<dyn std::error::Error>> {
    if !matches!(context.final_phase, 2 | 5) {
        return Err("invalid tag wait out of order".into());
    }
    let qmp = &session.engine.management[0];
    let inputs = session.engine.inputs.as_ref().ok_or("missing inputs")?;
    let pending = context
        .pending_invalid_tag
        .as_ref()
        .ok_or("missing pending invalid tag")?;
    loop {
        session.poll()?;
        if qmp.read_memory(0, l6max::gui_bitmap_table_entry_0 + 3, 1)? == [0xff] {
            break;
        }
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
        thread::sleep(Duration::from_millis(5));
    }
    context.final_phase += 1;
    Ok(json!({}))
}

pub(super) fn confirm_invalid_tag(
    session: &Session,
    context: &mut PreflightContext,
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
    let debugger = context.debugger.as_mut().ok_or("missing knob debugger")?;
    qmp.stop()?;
    let discarded = qmp.read_memory(0, l6max::gui_bitmap_table_entry_0 + 3, 1)? == [0xff];
    debugger.resume_guest()?;
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
                if session.poll()?.display == context.before {
                    return Err("Knob Dialog opened but the display did not change".into());
                }
                if context.expired(Instant::now(), inputs.panel.snapshot().guest_ms) {
                    return Err("knob-dialog popup exceeded its shared deadline".into());
                }
                Ok(
                    json!({"state":snapshot.state,"callback":snapshot.callback,"display_changed":true}),
                )
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
    use super::{DebugCleanup, begin_temporary_write, cleanup_debugger, finish_temporary_write};
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
        let mut journal = Vec::new();
        let first = begin_temporary_write(&mut journal, 0x100, vec![1], &[2]).unwrap();
        finish_temporary_write(&mut journal, first);
        let other = begin_temporary_write(&mut journal, 0x200, vec![3], &[4]).unwrap();
        finish_temporary_write(&mut journal, other);
        let restore = begin_temporary_write(&mut journal, 0x100, vec![2], &[1]).unwrap();
        finish_temporary_write(&mut journal, restore);
        assert_eq!(journal, vec![(0x200, vec![3])]);
    }
    #[test]
    fn failed_restore_keeps_original_and_attempt_for_reverse_rollback() {
        let mut journal = Vec::new();
        begin_temporary_write(&mut journal, 0x100, vec![1], &[2]).unwrap();
        begin_temporary_write(&mut journal, 0x100, vec![2], &[1]).unwrap();
        assert_eq!(journal, vec![(0x100, vec![1]), (0x100, vec![2])]);
    }
    #[test]
    fn overlapping_regions_are_rejected_except_exact_restoration() {
        let mut journal = Vec::new();
        begin_temporary_write(&mut journal, 0x100, vec![1, 2, 3, 4], &[9, 9, 9, 9]).unwrap();
        assert!(begin_temporary_write(&mut journal, 0x102, vec![0], &[3]).is_err());
        assert!(begin_temporary_write(&mut journal, 0x100, vec![0; 4], &[8, 8, 8, 8]).is_err());
        assert_eq!(journal.len(), 1);
        assert!(begin_temporary_write(&mut journal, 0x100, vec![9; 4], &[1, 2, 3, 4]).is_ok());
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
