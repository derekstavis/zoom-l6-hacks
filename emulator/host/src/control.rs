//! Optional local Unix-socket control endpoint for a running emulator.
use crate::{freertos, input::InputHub, qmp::QmpClient};
use l6fw::Model;
use serde_json::{Value, json};
use std::{
    fs,
    io::{BufRead, BufReader, Read, Write},
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
                            workers
                                .push(thread::spawn(move || handle(stream, &inputs, model, &qmp)));
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
        "atomic":false,"revision_guard":"l6max_1_10_kernel_routines","data":data}),
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
        "info" => {
            let main = inputs.main.snapshot();
            let panel = inputs.panel.snapshot();
            Ok(json!({
                "model": model.name(),
                "guest_ms": panel.guest_ms,
                "powered_off": main.powered_off,
                "main_indicators": main.indicators,
                "panel_indicators": panel.indicators,
            }))
        }
        "tap" | "down" | "up" => {
            let target = number(request, "target")?;
            if target >= crate::controls::INPUT_COUNT as u64 {
                return Err(format!(
                    "button target must be less than {}",
                    crate::controls::INPUT_COUNT
                ));
            }
            let target = target as u32;
            match command {
                "tap" => {
                    let hold = number(request, "hold_ms")?;
                    if hold == 0 || hold > 60_000 {
                        return Err("hold_ms must be between 1 and 60000".into());
                    }
                    inputs
                        .tap(target, hold as u32)
                        .map_err(|error| error.to_string())?;
                }
                "down" => {
                    let duration = number(request, "duration_ms")?;
                    if duration == 0 || duration > 60_000 {
                        return Err("duration_ms must be between 1 and 60000".into());
                    }
                    inputs
                        .down(target, duration as u32)
                        .map_err(|error| error.to_string())?;
                }
                "up" => {
                    inputs.up(target).map_err(|error| error.to_string())?;
                }
                _ => unreachable!(),
            }
            Ok(json!({"target":target,"command":command}))
        }
        "encoder" => {
            let channel = number(request, "channel")?;
            let count = if model == Model::L6 { 6 } else { 8 };
            if channel == 0 || channel > count {
                return Err(format!(
                    "encoder channel must be 1..{count} for {}",
                    model.name()
                ));
            }
            let steps = signed(request, "steps")?;
            if steps == 0 || steps.unsigned_abs() > 1000 {
                return Err("steps must be between -1000 and 1000, excluding zero".into());
            }
            inputs
                .encoder(channel as u32 - 1, steps)
                .map_err(|error| error.to_string())?;
            Ok(json!({"channel":channel,"steps":steps}))
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
            inputs
                .knob(index, steps)
                .map_err(|error| error.to_string())?;
            Ok(json!({"channel":channel,"steps":steps}))
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

fn handle(mut stream: UnixStream, inputs: &InputHub, model: Model, qmp: &QmpClient) {
    // Accepted sockets can inherit O_NONBLOCK from the listener (notably on
    // macOS). Use bounded blocking I/O so large JSON replies are complete.
    if let Err(error) = configure_client(&stream) {
        tracing::warn!(%error, "cannot configure control client socket");
        return;
    }
    let mut line = String::new();
    let outcome = BufReader::new(&mut stream)
        .take(4097)
        .read_line(&mut line)
        .map_err(|error| error.to_string())
        .and_then(|_| {
            if line.len() > 4096 {
                return Err("request exceeds 4096 bytes".into());
            }
            let request: Value = serde_json::from_str(&line).map_err(|error| error.to_string())?;
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
