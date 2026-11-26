//! Optional local Unix-socket control endpoint for a running emulator.
use crate::{
    freertos,
    input::{InputHub, InputSource},
    qmp::QmpClient,
};
use l6fw::Model;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    fs,
    io::{BufRead, BufReader, Write},
    os::fd::AsRawFd,
    os::unix::{
        fs::PermissionsExt,
        net::{UnixListener, UnixStream},
    },
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

/// Basic state shared by in-process callers and the control socket.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct Info {
    pub model: Model,
    pub guest_ms: u32,
    pub powered_off: bool,
    pub main_indicators: [u32; 8],
    pub panel_indicators: [u32; 8],
}

pub fn info(inputs: &InputHub, model: Model) -> Info {
    let main = inputs.main.snapshot();
    let panel = inputs.panel.snapshot();
    Info {
        model,
        guest_ms: panel.guest_ms,
        powered_off: main.powered_off,
        main_indicators: main.indicators,
        panel_indicators: panel.indicators,
    }
}

/// Validate a one-based channel and enqueue a panel encoder input.
/// Success reports enqueueing only; the guest may not have applied it yet.
pub fn enqueue_encoder(
    inputs: &InputHub,
    model: Model,
    channel: u32,
    steps: i32,
) -> Result<u32, String> {
    enqueue_encoder_with(model, channel, steps, |index, steps| {
        inputs
            .encoder(index, steps)
            .map_err(|error| error.to_string())
    })
}

/// Enqueue a tap. This confirms enqueueing, not guest acknowledgement.
pub fn enqueue_tap(inputs: &InputHub, target: u32, hold_ms: u32) -> Result<u32, String> {
    if target >= crate::controls::INPUT_COUNT as u32 {
        return Err(format!(
            "button target must be less than {}",
            crate::controls::INPUT_COUNT
        ));
    }
    if hold_ms == 0 || hold_ms > 60_000 {
        return Err("hold_ms must be between 1 and 60000".into());
    }
    inputs
        .tap(target, hold_ms)
        .map_err(|error| error.to_string())
}

fn enqueue_encoder_with<T>(
    model: Model,
    channel: u32,
    steps: i32,
    enqueue: impl FnOnce(u32, i32) -> Result<T, String>,
) -> Result<T, String> {
    let count = model.channels() as u32;
    if channel == 0 || channel > count {
        return Err(format!(
            "encoder channel must be 1..{count} for {}",
            model.name()
        ));
    }
    if steps == 0 || steps.unsigned_abs() > 1000 {
        return Err("steps must be between -1000 and 1000, excluding zero".into());
    }
    enqueue(channel - 1, steps)
}

pub struct ControlServer {
    path: PathBuf,
    stopping: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl ControlServer {
    pub fn start(
        path: PathBuf,
        inputs: Arc<InputHub>,
        model: Model,
        qmp: QmpClient,
        installer_outcome: Option<crate::update_bootloader::Outcome>,
        usb_controller: Option<crate::usb::Controller>,
        debugger: Option<crate::debug::Controller>,
    ) -> Result<Self, String> {
        if fs::symlink_metadata(&path).is_ok() {
            return Err(format!(
                "control socket path already exists: {}",
                path.display()
            ));
        }
        let listener = UnixListener::bind(&path)
            .map_err(|error| format!("bind control socket {}: {error}", path.display()))?;
        if let Err(error) = fs::set_permissions(&path, fs::Permissions::from_mode(0o600)) {
            let _ = fs::remove_file(&path);
            return Err(format!("secure control socket {}: {error}", path.display()));
        }
        if let Err(error) = listener.set_nonblocking(true) {
            let _ = fs::remove_file(&path);
            return Err(format!("configure control socket: {error}"));
        }
        let stopping = Arc::new(AtomicBool::new(false));
        let thread_stopping = stopping.clone();
        let thread = match thread::Builder::new()
            .name("l6-control-socket".into())
            .spawn(move || {
                let mut workers: Vec<JoinHandle<()>> = Vec::new();
                while !thread_stopping.load(Ordering::Relaxed) {
                    let mut index = 0;
                    while index < workers.len() {
                        if workers[index].is_finished() {
                            let _ = workers.swap_remove(index).join();
                        } else {
                            index += 1;
                        }
                    }
                    match listener.accept() {
                        Ok((mut stream, _)) if workers.len() >= 8 => {
                            let _ = writeln!(
                                stream,
                                "{}",
                                json!({"ok":false,"error":"control server is busy"})
                            );
                        }
                        Ok((stream, _)) => {
                            let inputs = inputs.clone();
                            let qmp = qmp.clone();
                            let installer_outcome = installer_outcome.clone();
                            let usb_controller = usb_controller.clone();
                            let debugger = debugger.clone();
                            let stopping = thread_stopping.clone();
                            workers.push(thread::spawn(move || {
                                handle(
                                    stream,
                                    &inputs,
                                    model,
                                    &qmp,
                                    installer_outcome.as_ref(),
                                    usb_controller.as_ref(),
                                    debugger.as_ref(),
                                    &stopping,
                                )
                            }));
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            thread::sleep(Duration::from_millis(10));
                        }
                        Err(error) => {
                            tracing::warn!(%error, "control socket accept failed");
                            thread::sleep(Duration::from_millis(50));
                        }
                    }
                }
                for worker in workers {
                    let _ = worker.join();
                }
            }) {
            Ok(thread) => thread,
            Err(error) => {
                let _ = fs::remove_file(&path);
                return Err(format!("start control socket thread: {error}"));
            }
        };
        tracing::info!(socket=%path.display(), "local emulator control enabled");
        Ok(Self {
            path,
            stopping,
            thread: Some(thread),
        })
    }
}

impl Drop for ControlServer {
    fn drop(&mut self) {
        self.stopping.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        let _ = fs::remove_file(&self.path);
    }
}

fn number(request: &Value, key: &str) -> Result<u64, String> {
    request
        .get(key)
        .and_then(Value::as_u64)
        .ok_or_else(|| format!("{key} must be a nonnegative integer"))
}

fn signed(request: &Value, key: &str) -> Result<i32, String> {
    request
        .get(key)
        .and_then(Value::as_i64)
        .and_then(|value| i32::try_from(value).ok())
        .ok_or_else(|| format!("{key} must be a signed 32-bit integer"))
}

fn rtos_sample(
    kind: &str,
    inputs: &InputHub,
    model: Model,
    qmp: &QmpClient,
) -> Result<Value, String> {
    let data = match kind {
        "heap" => {
            let h = freertos::heap(qmp, model)?;
            let free_blocks: Vec<_> = h.blocks.iter().filter(|b| !b.allocated).collect();
            let largest = free_blocks.iter().map(|b| b.size).max().unwrap_or(0);
            let fragmentation = (h.counters.free != 0)
                .then(|| (h.counters.free - largest) as f64 * 100.0 / h.counters.free as f64);
            json!({"total_bytes":h.counters.size,"free_bytes":h.counters.free,
                "minimum_free_bytes":h.counters.minimum_ever_free,"allocations":h.counters.allocations,
                "frees":h.counters.frees,"free_block_count":free_blocks.len(),
                "largest_free_block_bytes":largest,"fragmentation_percent":fragmentation,
                "block_sizes_include_headers":true,
                "blocks":h.blocks.iter().map(|b| json!({"address":format!("0x{:08x}",b.address),"size_bytes":b.size,"allocated":b.allocated,"owner":b.owner})).collect::<Vec<_>>()})
        }
        "tasks" => {
            let snapshot = freertos::tasks(qmp, model)?;
            json!({"stack_headroom_is_estimate":true,"tasks":snapshot.tasks.iter().map(|t| {
                let state = match t.state.as_str() {"Current"=>"current","Ready"=>"ready","Blocked"=>"blocked","Timed wait"=>"blocked_timed","Delay / wait"=>"delayed_or_waiting","Wait / susp."=>"waiting_or_suspended",_=>"other"};
                json!({"name":t.name,"state":state,"current":t.current,"tcb_address":format!("0x{:08x}",t.tcb),"priority":t.priority,"base_priority":t.base_priority,"creation_number":t.creation_number,"mutexes_held":t.mutexes_held,"event_list_address":t.event_list.map(|a|format!("0x{a:08x}")),"stack_base_address":format!("0x{:08x}",t.stack_start),"stack_size_bytes":t.stack_size,"stack_free_bytes_estimate":t.stack_remaining,"stack_free_percent_estimate":if t.stack_size == 0 { None } else { Some(t.stack_remaining as f64 * 100.0 / t.stack_size as f64) }})
            }).collect::<Vec<_>>()})
        }
        "scheduler" => {
            let s = freertos::scheduler(qmp, model)?;
            json!({"task_count":s.task_count,"running":s.running,"suspension_depth":s.suspended,
                "ticks":s.ticks,"pending_ticks":s.pending_ticks,"current_tcb_address":format!("0x{:08x}",s.current_tcb),
                "yield_pending":s.yield_pending,"tick_overflows":s.tick_overflows,"next_wake_tick":s.next_wake,"critical_nesting":s.critical_nesting})
        }
        "objects" => {
            let objects = freertos::objects(qmp, model)?;
            json!({"complete":false,"objects":objects.iter().map(|o|json!({"name":o.name,"type":"counting_semaphore","address":format!("0x{:08x}",o.address),"count":o.count,"capacity":o.capacity,"send_waiters":o.send_waiters,"receive_waiters":o.receive_waiters})).collect::<Vec<_>>()})
        }
        _ => {
            return Err(format!(
                "unknown RTOS category {kind:?}; use heap, tasks, scheduler, objects, or all"
            ));
        }
    };
    Ok(
        json!({"schema":1,"kind":kind,"model":model.name(),"panel_guest_ms":inputs.panel.snapshot().guest_ms,
        "atomic":false,"revision_guard":freertos::revision_guard(model),"data":data}),
    )
}

fn execute(
    request: &Value,
    inputs: &InputHub,
    model: Model,
    qmp: &QmpClient,
) -> Result<Value, String> {
    let command = request
        .get("command")
        .and_then(Value::as_str)
        .ok_or("request needs a command")?;
    match command {
        "sd-loader-status" => {
            serde_json::to_value(crate::sd_loader::snapshot(qmp, model)?).map_err(|e| e.to_string())
        }
        "sd-loader-site" => {
            let index =
                u32::try_from(number(request, "index")?).map_err(|_| "invalid SD site index")?;
            serde_json::to_value(crate::sd_loader::site(qmp, model, index)?)
                .map_err(|e| e.to_string())
        }
        "qmp-status" => serde_json::to_value(qmp.query_status().map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string()),
        "input-status" => {
            let source = match request.get("source").and_then(Value::as_str) {
                Some("main") => InputSource::Main,
                Some("panel") => InputSource::Panel,
                _ => return Err("input-status source must be main or panel".into()),
            };
            serde_json::to_value(inputs.counters(source)).map_err(|error| error.to_string())
        }
        "input-ack" => {
            let source = match request.get("source").and_then(Value::as_str) {
                Some("main") => InputSource::Main,
                Some("panel") => InputSource::Panel,
                _ => return Err("input-ack source must be main or panel".into()),
            };
            let sequence = u32::try_from(number(request, "sequence")?)
                .map_err(|_| "invalid input sequence")?;
            serde_json::to_value(inputs.acknowledgement(source, sequence))
                .map_err(|error| error.to_string())
        }
        "heap-counters" => {
            let counters = crate::heap::counters(qmp, model)?;
            serde_json::to_value(counters).map_err(|error| error.to_string())
        }
        "memory-read" => {
            let address = request
                .get("address")
                .and_then(Value::as_u64)
                .and_then(|v| u32::try_from(v).ok())
                .ok_or("memory-read needs a u32 address")?;
            let length = request
                .get("length")
                .and_then(Value::as_u64)
                .and_then(|v| usize::try_from(v).ok())
                .ok_or("memory-read needs a length")?;
            serde_json::to_value(crate::memory::read_main(qmp, address, length)?)
                .map_err(|e| e.to_string())
        }
        "update-gui-state" => {
            serde_json::to_value(crate::update_state::update_gui_state(qmp, model)?)
                .map_err(|error| error.to_string())
        }
        "update-state" => serde_json::to_value(crate::update_state::update_state(qmp, model)?)
            .map_err(|error| error.to_string()),
        "l6-din-state" => {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
            let reader =
                crate::l6_din::Reader::new_before_checked(qmp, model, deadline, &|| Ok(()))?;
            serde_json::to_value(reader.snapshot(deadline, &|| Ok(()))?).map_err(|e| e.to_string())
        }
        "effect-slot" => {
            let binding: crate::effect_editor::SlotBinding = serde_json::from_value(
                request
                    .get("binding")
                    .cloned()
                    .ok_or("effect-slot needs binding")?,
            )
            .map_err(|e| e.to_string())?;
            let fields: crate::effect_editor::SlotFields = serde_json::from_value(
                request
                    .get("fields")
                    .cloned()
                    .ok_or("effect-slot needs fields")?,
            )
            .map_err(|e| e.to_string())?;
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
            let reader = crate::effect_editor::SlotReader::new_before_checked(
                qmp,
                model,
                binding,
                deadline,
                &|| Ok(()),
            )?;
            serde_json::to_value(reader.snapshot_before_checked(&fields, deadline, &|| Ok(()))?)
                .map_err(|e| e.to_string())
        }
        "effect-editor" => {
            let binding: crate::effect_editor::Binding = serde_json::from_value(
                request
                    .get("binding")
                    .cloned()
                    .ok_or("effect-editor needs binding")?,
            )
            .map_err(|e| e.to_string())?;
            serde_json::to_value(crate::effect_editor::Reader::new(qmp, model, binding)?.read()?)
                .map_err(|e| e.to_string())
        }
        "panel-key" => {
            let code = request
                .get("code")
                .and_then(Value::as_u64)
                .and_then(|v| u32::try_from(v).ok())
                .ok_or("panel-key needs a code")?;
            serde_json::to_value(crate::panel_keys::snapshot(qmp, model, code)?)
                .map_err(|e| e.to_string())
        }
        "midi-config" => serde_json::to_value(crate::midi_config::config(qmp, model)?)
            .map_err(|error| error.to_string()),
        "mixer-level" => serde_json::to_value(crate::mixer_level::first_channel(qmp, model)?)
            .map_err(|error| error.to_string()),
        "rtos" => {
            let kind = request
                .get("kind")
                .and_then(Value::as_str)
                .ok_or("rtos request needs a kind")?;
            let sample = |kind| rtos_sample(kind, inputs, model, qmp);
            if kind == "all" {
                Ok(
                    json!({"schema":1,"kind":"all","model":model.name(),"atomic":false,
                    "heap":sample("heap")?,"tasks":sample("tasks")?,"scheduler":sample("scheduler")?,"objects":sample("objects")?}),
                )
            } else {
                sample(kind)
            }
        }
        "knob-parameter-state" => {
            let state = crate::knob_popup::parameter_reader(qmp, model)?.parameter_state()?;
            serde_json::to_value(state).map_err(|error| error.to_string())
        }
        "recorder-state" => {
            let state = crate::knob_popup::reader(qmp, model)?.recorder_state()?;
            serde_json::to_value(state).map_err(|error| error.to_string())
        }
        "knob-popup" => {
            let with_text = request
                .get("with_text")
                .and_then(Value::as_bool)
                .ok_or("knob-popup requires with_text")?;
            let snapshot = crate::knob_popup::snapshot(qmp, model, with_text)?;
            serde_json::to_value(snapshot).map_err(|error| error.to_string())
        }
        "info" => serde_json::to_value(info(inputs, model)).map_err(|error| error.to_string()),
        "tap" | "down" | "up" => {
            let target = number(request, "target")?;
            if target >= crate::controls::INPUT_COUNT as u64 {
                return Err(format!(
                    "button target must be less than {}",
                    crate::controls::INPUT_COUNT
                ));
            }
            let target = target as u32;
            let sequence = match command {
                "tap" => {
                    let hold = number(request, "hold_ms")?;
                    let hold =
                        u32::try_from(hold).map_err(|_| "hold_ms must be between 1 and 60000")?;
                    enqueue_tap(inputs, target, hold)?
                }
                "down" => {
                    let duration = number(request, "duration_ms")?;
                    if duration == 0 || duration > 60_000 {
                        return Err("duration_ms must be between 1 and 60000".into());
                    }
                    inputs
                        .down(target, duration as u32)
                        .map_err(|error| error.to_string())?
                }
                "up" => inputs.up(target).map_err(|error| error.to_string())?,
                _ => unreachable!(),
            };
            Ok(json!({"target":target,"command":command,"sequence":sequence}))
        }
        "encoder" => {
            let channel = number(request, "channel")?;
            let steps = signed(request, "steps")?;
            let channel = u32::try_from(channel).map_err(|_| "encoder channel is too large")?;
            let sequence = enqueue_encoder(inputs, model, channel, steps)?;
            Ok(json!({"channel":channel,"steps":steps,"sequence":sequence}))
        }
        "analog" => {
            let channel = number(request, "channel")?;
            if channel == 0 || channel > 5 {
                return Err("analog channel must be 1..5".into());
            }
            let steps = signed(request, "steps")?;
            if steps == 0 || steps.unsigned_abs() > 1000 {
                return Err("steps must be between -1000 and 1000, excluding zero".into());
            }
            let index = 8 + channel as usize - 1;
            let sequence = inputs
                .knob(index, steps)
                .map_err(|error| error.to_string())?;
            Ok(json!({"channel":channel,"steps":steps,"sequence":sequence}))
        }
        "set-analog" => {
            let channel = number(request, "channel")?;
            let value = number(request, "value")?;
            if channel == 0 || channel > 5 || value > 1023 {
                return Err("channel must be 1..5 and value must be 0..1023".into());
            }
            inputs
                .set_analog_position(channel as usize - 1, value as u16)
                .map_err(|error| error.to_string())?;
            Ok(json!({"channel":channel,"value":value}))
        }
        _ => Err(format!("unknown control command {command:?}")),
    }
}

fn installer_observation(
    outcome: Option<&crate::update_bootloader::Outcome>,
) -> Result<Value, String> {
    let outcome = outcome.ok_or("replacement installer was not run for this engine start")?;
    serde_json::to_value(outcome).map_err(|error| error.to_string())
}

fn handle(
    mut stream: UnixStream,
    inputs: &InputHub,
    model: Model,
    qmp: &QmpClient,
    installer_outcome: Option<&crate::update_bootloader::Outcome>,
    usb_controller: Option<&crate::usb::Controller>,
    debugger: Option<&crate::debug::Controller>,
    stopping: &AtomicBool,
) {
    if let Err(error) = configure_client(&stream) {
        tracing::warn!(%error, "cannot configure control client socket");
        return;
    }
    let cloned = match stream.try_clone() {
        Ok(cloned) => cloned,
        Err(error) => {
            tracing::warn!(%error, "cannot clone control socket");
            return;
        }
    };
    let mut reader = BufReader::new(cloned);
    let request = read_control_request(&mut reader);
    if request
        .as_ref()
        .ok()
        .and_then(|r| r.get("command"))
        .and_then(Value::as_str)
        == Some("debug-begin")
    {
        match debugger
            .ok_or_else(|| "debugger unavailable".to_owned())
            .and_then(|controller| controller.begin())
        {
            Ok(session) => serve_debug(stream, reader, session, stopping),
            Err(error) => {
                let _ = send_response(&mut stream, &json!({"ok":false,"error":error}));
            }
        }
        return;
    }
    let outcome = request.and_then(|request| {
        if request.get("command").and_then(Value::as_str) == Some("usb-attach") {
            usb_controller
                .ok_or("USB controller unavailable")?
                .attach()?;
            return Ok(json!({"attached":true}));
        }
        if request.get("command").and_then(Value::as_str) == Some("usb-release") {
            usb_controller
                .ok_or("USB controller unavailable")?
                .release()?;
            return Ok(json!({"attached":false}));
        }
        if request.get("command").and_then(Value::as_str) == Some("usb-controller") {
            let running = usb_controller
                .ok_or("USB controller unavailable")?
                .running()?;
            return Ok(json!({"running":running}));
        }
        if request.get("command").and_then(Value::as_str) == Some("installer-outcome") {
            return installer_observation(installer_outcome);
        }
        execute(&request, inputs, model, qmp)
    });
    let reply = match outcome {
        Ok(result) => json!({"ok":true,"result":result}),
        Err(error) => json!({"ok":false,"error":error}),
    };
    if let Err(error) = send_response(&mut stream, &reply) {
        tracing::warn!(%error, "cannot send control response");
    }
}

enum ControlLine {
    Idle,
    Eof,
    Value(String),
}

/// Keep the initial and session readers bounded even when a peer slowly drips
/// a partial line; retain any pipelined bytes in the same `BufReader`.
fn read_control_line(
    reader: &mut BufReader<UnixStream>,
    overall: std::time::Instant,
    packet_timeout: Duration,
) -> Result<ControlLine, String> {
    let mut bytes = Vec::new();
    let mut packet_deadline = None;
    loop {
        let now = std::time::Instant::now();
        if now >= overall {
            return Err("control session deadline exceeded".into());
        }
        if packet_deadline.is_some_and(|deadline| now >= deadline) {
            return Err("control request deadline exceeded".into());
        }
        let end = packet_deadline.unwrap_or(overall).min(overall);
        let wait = end
            .saturating_duration_since(now)
            .min(Duration::from_secs(1));
        reader
            .get_ref()
            .set_read_timeout(Some(wait))
            .map_err(|error| error.to_string())?;
        let available = match reader.fill_buf() {
            Ok(available) => available,
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
                ) =>
            {
                if bytes.is_empty() {
                    return Ok(ControlLine::Idle);
                }
                continue;
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error.to_string()),
        };
        if available.is_empty() {
            return if bytes.is_empty() {
                Ok(ControlLine::Eof)
            } else {
                Err("control request ended before newline".into())
            };
        }
        if packet_deadline.is_none() {
            packet_deadline = Some(std::time::Instant::now() + packet_timeout);
        }
        let count = available
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(available.len(), |index| index + 1);
        if bytes.len() + count > 4096 {
            return Err("control request exceeds 4096 bytes".into());
        }
        let finished = available[count - 1] == b'\n';
        bytes.extend_from_slice(&available[..count]);
        reader.consume(count);
        if finished {
            return String::from_utf8(bytes)
                .map(ControlLine::Value)
                .map_err(|error| error.to_string());
        }
    }
}

pub(crate) fn read_control_request(reader: &mut BufReader<UnixStream>) -> Result<Value, String> {
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    loop {
        match read_control_line(reader, deadline, Duration::from_secs(2))? {
            ControlLine::Idle => continue,
            ControlLine::Eof => return Err("control request ended before newline".into()),
            ControlLine::Value(line) => {
                return serde_json::from_str(&line).map_err(|error| error.to_string());
            }
        }
    }
}

/// One persistent connection owns one debugger session. EOF, malformed
/// requests, idle timeout, or server shutdown all close the scoped session.
pub(crate) fn serve_debug(
    mut stream: UnixStream,
    mut reader: BufReader<UnixStream>,
    mut session: crate::debug::Session,
    stopping: &AtomicBool,
) {
    if send_response(&mut stream, &json!({"ok":true,"result":{"started":true}})).is_err() {
        return;
    }
    let deadline = std::time::Instant::now() + Duration::from_secs(300);
    loop {
        if stopping.load(Ordering::Relaxed) || std::time::Instant::now() >= deadline {
            break;
        }
        let line = match read_control_line(&mut reader, deadline, Duration::from_secs(2)) {
            Ok(ControlLine::Idle) => continue,
            Ok(ControlLine::Eof) => break,
            Ok(ControlLine::Value(line)) => line,
            Err(error) => {
                let error = match session.close() {
                    Ok(()) => error,
                    Err(cleanup) => format!("{error}; debugger cleanup failed: {cleanup}"),
                };
                let _ = send_response(&mut stream, &json!({"ok":false,"error":error}));
                break;
            }
        };
        let command =
            serde_json::from_str::<crate::debug::Command>(&line).map_err(|e| e.to_string());
        let close = matches!(
            command,
            Ok(crate::debug::Command::Close
                | crate::debug::Command::L6MidiFinish { .. }
                | crate::debug::Command::L6DinFinish { .. })
        );
        let fd = stream.as_raw_fd();
        let result = command.and_then(|command| {
            session.execute_cancellable(command, || {
                if stopping.load(Ordering::Relaxed) || std::time::Instant::now() >= deadline {
                    return true;
                }
                // EOF can sit behind unread pipelined bytes, so peeking at
                // data alone does not establish that the peer is still alive.
                let mut descriptor = libc::pollfd {
                    fd,
                    events: libc::POLLIN,
                    revents: 0,
                };
                let ready = unsafe { libc::poll(&mut descriptor, 1, 0) };
                if ready < 0 {
                    return std::io::Error::last_os_error().kind()
                        != std::io::ErrorKind::Interrupted;
                }
                if descriptor.revents & (libc::POLLHUP | libc::POLLERR | libc::POLLNVAL) != 0 {
                    return true;
                }
                let mut byte = 0u8;
                let received = unsafe {
                    libc::recv(
                        fd,
                        (&mut byte as *mut u8).cast(),
                        1,
                        libc::MSG_PEEK | libc::MSG_DONTWAIT,
                    )
                };
                if received == 0 {
                    return true;
                }
                if received < 0 {
                    let error = std::io::Error::last_os_error();
                    return !matches!(
                        error.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                    );
                }
                false
            })
        });
        let failed = result.is_err();
        let reply = match result {
            Ok(value) => json!({"ok":true,"result":value}),
            Err(error) => {
                let error = match session.close() {
                    Ok(()) => error,
                    Err(cleanup) => format!("{error}; debugger cleanup failed: {cleanup}"),
                };
                json!({"ok":false,"error":error})
            }
        };
        if send_response(&mut stream, &reply).is_err() || close || failed {
            break;
        }
    }
    if let Err(error) = session.close() {
        tracing::error!(%error, "debug session cleanup failed");
    }
}

fn configure_client(stream: &UnixStream) -> std::io::Result<()> {
    stream.set_nonblocking(false)?;
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    stream.set_write_timeout(Some(Duration::from_secs(2)))
}

fn send_response(stream: &mut UnixStream, reply: &Value) -> std::io::Result<()> {
    let mut bytes = serde_json::to_vec(reply)?;
    bytes.push(b'\n');
    stream.write_all(&bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slow_partial_control_line_cannot_extend_its_deadline() {
        let (server, mut client) = UnixStream::pair().unwrap();
        let writer = thread::spawn(move || {
            for _ in 0..20 {
                if client.write_all(b"x").is_err() {
                    break;
                }
                thread::sleep(Duration::from_millis(10));
            }
        });
        let mut reader = BufReader::new(server);
        let started = std::time::Instant::now();
        let result = read_control_line(
            &mut reader,
            started + Duration::from_secs(1),
            Duration::from_millis(50),
        );
        assert!(matches!(result, Err(error) if error.contains("deadline")));
        assert!(started.elapsed() < Duration::from_millis(300));
        drop(reader);
        writer.join().unwrap();
    }

    #[test]
    fn installer_observation_reports_actual_result_or_unavailable() {
        use crate::update_bootloader::Outcome;
        assert!(installer_observation(None).unwrap_err().contains("not run"));
        for (outcome, expected) in [
            (Outcome::Installed, json!({"status":"installed"})),
            (Outcome::NoPending, json!({"status":"no-pending"})),
            (
                Outcome::PreviousFailure,
                json!({"status":"previous-failure"}),
            ),
            (
                Outcome::Failed("bad card".into()),
                json!({"status":"failed","error":"bad card"}),
            ),
        ] {
            assert_eq!(installer_observation(Some(&outcome)).unwrap(), expected);
            assert_eq!(
                serde_json::from_value::<Outcome>(expected).unwrap(),
                outcome
            );
        }
    }

    #[test]
    fn input_status_request_dispatches_and_rejects_invalid_sources() {
        use crate::input::EngineInput;
        let (main, _main_peer) = EngineInput::pair().unwrap();
        let (panel, _panel_peer) = EngineInput::pair().unwrap();
        let inputs = InputHub::new(main, panel, Model::L6max);
        let (stream, mut peer) = UnixStream::pair().unwrap();
        let server = thread::spawn(move || {
            peer.write_all(b"{\"QMP\":{\"version\":{\"qemu\":{\"major\":10,\"minor\":0,\"micro\":0},\"package\":\"\"},\"capabilities\":[]}}\r\n").unwrap();
            let mut line = String::new();
            BufReader::new(&mut peer).read_line(&mut line).unwrap();
            assert_eq!(
                serde_json::from_str::<Value>(&line).unwrap()["execute"],
                "qmp_capabilities"
            );
            peer.write_all(b"{\"return\":{}}\r\n").unwrap();
        });
        let qmp = QmpClient::connect(stream).unwrap();
        server.join().unwrap();
        for (source, expected) in [("main", InputSource::Main), ("panel", InputSource::Panel)] {
            let result = execute(
                &json!({"command":"input-status", "source":source}),
                &inputs,
                Model::L6max,
                &qmp,
            )
            .unwrap();
            assert_eq!(
                result,
                serde_json::to_value(inputs.counters(expected)).unwrap()
            );
        }
        assert!(
            execute(
                &json!({"command":"input-status", "source":"other"}),
                &inputs,
                Model::L6max,
                &qmp
            )
            .is_err()
        );
        assert!(
            execute(
                &json!({"command":"input-status"}),
                &inputs,
                Model::L6max,
                &qmp
            )
            .is_err()
        );
    }

    #[test]
    fn input_status_samples_channel_counters_without_consuming_acknowledgements() {
        use crate::{
            input::EngineInput,
            protocol::{self, Message},
        };
        let (main, _main_peer) = EngineInput::pair().unwrap();
        let (panel, mut panel_peer) = EngineInput::pair().unwrap();
        let inputs = InputHub::new(main, panel, Model::L6max);
        let sent = inputs.tap(35, 50).unwrap();
        let command = Message::read(&mut panel_peer).unwrap();
        assert_eq!(command.sequence, sent);
        Message {
            kind: protocol::APPLIED,
            sequence: sent,
            target: 35,
            value: 0,
            duration_ms: 0,
        }
        .write(&mut panel_peer)
        .unwrap();
        Message {
            kind: protocol::CLOCK,
            sequence: 1,
            target: 0,
            value: 0,
            duration_ms: 4321,
        }
        .write(&mut panel_peer)
        .unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        loop {
            let panel = inputs.counters(InputSource::Panel);
            if panel.applied == 1 && panel.guest_ms == 4321 {
                assert_eq!((panel.sent, panel.released, panel.rejected), (1, 0, 0));
                assert!(panel.connected);
                assert_eq!(panel.error, None);
                assert_eq!(inputs.counters(InputSource::Panel), panel);
                let main = inputs.counters(InputSource::Main);
                assert_eq!((main.sent, main.applied, main.guest_ms), (0, 0, 0));
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "input status did not observe messages: {panel:?}"
            );
            thread::sleep(Duration::from_millis(1));
        }
    }

    #[test]
    fn large_unicode_response_is_complete_json() {
        let (mut client, mut server) = UnixStream::pair().unwrap();
        server.set_nonblocking(true).unwrap();
        configure_client(&server).unwrap();
        let value = json!({"ok":true,"result": {"data":"stack · task".repeat(10000)}});
        let expected = value.clone();
        let writer = thread::spawn(move || send_response(&mut server, &value).unwrap());
        let mut line = String::new();
        BufReader::new(&mut client).read_line(&mut line).unwrap();
        assert!(line.ends_with('\n'));
        assert_eq!(serde_json::from_str::<Value>(&line).unwrap(), expected);
        writer.join().unwrap();
    }
}

#[cfg(test)]
mod encoder_enqueue_tests {
    use super::*;
    #[test]
    fn one_based_channels_are_validated_and_enqueued_without_waiting() {
        let mut observed = None;
        enqueue_encoder_with(Model::L6, 1, -4, |index, steps| {
            observed = Some((index, steps));
            Ok(())
        })
        .unwrap();
        assert_eq!(observed, Some((0, -4)));
        enqueue_encoder_with(Model::L6max, 8, 1000, |index, steps| {
            assert_eq!((index, steps), (7, 1000));
            Ok(())
        })
        .unwrap();
        for (model, channel, steps) in [
            (Model::L6, 0, 1),
            (Model::L6, 7, 1),
            (Model::L6max, 9, 1),
            (Model::L6, 1, 0),
            (Model::L6, 1, 1001),
        ] {
            assert!(
                enqueue_encoder_with::<()>(model, channel, steps, |_, _| panic!(
                    "invalid input was enqueued"
                ))
                .is_err()
            );
        }
        assert_eq!(
            enqueue_encoder_with::<()>(Model::L6, 1, 1, |_, _| Err("enqueue failed".into()))
                .unwrap_err(),
            "enqueue failed"
        );
    }
}
