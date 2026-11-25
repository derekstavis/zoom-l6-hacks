mod midi_cli;

use l6ctl::{ControlClient, InputSource, Request, RtosKind};
use l6fw::Model;
use l6max_host::controls::control_target;
use serde_json::{Value, json};
use std::{env, path::PathBuf};

fn target(model: Model, name: &str) -> Result<u32, String> {
    if let Ok(target) = name.parse::<u32>() {
        return (target < 54)
            .then_some(target)
            .ok_or_else(|| "numeric button target must be 0..53".into());
    }
    control_target(model, name)
}

fn parse_int<T: std::str::FromStr>(value: Option<&String>, name: &str) -> Result<T, String> {
    value
        .ok_or_else(|| format!("missing {name}"))?
        .parse()
        .map_err(|_| format!("invalid {name}"))
}

fn run() -> Result<(), String> {
    let mut args = env::args().skip(1).collect::<Vec<_>>();
    let mut socket = env::var_os("L6_CONTROL_SOCKET").map(PathBuf::from);
    let mut index = 0;
    while index < args.len() {
        if args[index] == "--socket" {
            if index + 1 >= args.len() {
                return Err("--socket requires a path".into());
            }
            socket = Some(PathBuf::from(args.remove(index + 1)));
            args.remove(index);
        } else {
            index += 1;
        }
    }
    let command = args.first().map(String::as_str).unwrap_or("help");
    if matches!(command, "help" | "--help" | "-h") {
        println!(
            "Usage: l6ctl [--socket PATH] info\n       l6ctl [--socket PATH] input status main|panel\n       l6ctl [--socket PATH] heap-counters\n       l6ctl [--socket PATH] memory-read ADDRESS LENGTH\n       l6ctl [--socket PATH] midi-config\n       l6ctl [--socket PATH] update-state\n       l6ctl [--socket PATH] installer-outcome\n       l6ctl [--socket PATH] usb attach|release|controller\n       l6ctl [--socket PATH] update-gui-state\n       l6ctl [--socket PATH] knob-popup [--text]\n       l6ctl [--socket PATH] recorder-state\n       l6ctl [--socket PATH] knob-parameter-state\n       l6ctl [--socket PATH] rtos heap|tasks|scheduler|objects|all [--pretty]\n       l6ctl [--socket PATH] button NAME|TARGET [--hold-ms MS]\n       l6ctl [--socket PATH] down NAME|TARGET DURATION_MS\n       l6ctl [--socket PATH] up NAME|TARGET\n       l6ctl [--socket PATH] encoder CHANNEL STEPS\n       l6ctl [--socket PATH] analog CHANNEL STEPS\n       l6ctl [--socket PATH] set-analog CHANNEL VALUE\n       l6ctl midi listen ENDPOINT [--timeout-ms MS]\n       l6ctl midi observe ENDPOINT --wall-ms MS\n       l6ctl midi send ENDPOINT CHANNEL CONTROLLER VALUE\n       l6ctl midi sync ENDPOINT --count N [--quiet-ms MS] [--timeout-ms MS]\n\nConnects to a running emulator started with --control-socket PATH. Button names use the semantic names documented by l6test; numeric targets 0..53 are also accepted. Encoder channels are one-based. Analog channels are 1..5."
        );
        return Ok(());
    }
    if command == "midi" {
        return midi_cli::run(&args[1..]).map_err(|error| error.json());
    }
    let socket = socket.ok_or("provide --socket PATH or set L6_CONTROL_SOCKET")?;
    let client = ControlClient::new(&socket);
    if command == "input" {
        let source = match args.as_slice() {
            [_, subcommand, source] if subcommand == "status" && source == "main" => {
                InputSource::Main
            }
            [_, subcommand, source] if subcommand == "status" && source == "panel" => {
                InputSource::Panel
            }
            _ => return Err("Usage: l6ctl input status main|panel".into()),
        };
        let status = client
            .input_status(source)
            .map_err(|error| error.to_string())?;
        println!(
            "{}",
            serde_json::to_string(&status).map_err(|error| error.to_string())?
        );
        return Ok(());
    }
    if command == "heap-counters" {
        if args.len() != 1 {
            return Err("Usage: l6ctl heap-counters".into());
        }
        let counters = client.heap_counters().map_err(|error| error.to_string())?;
        println!(
            "{}",
            serde_json::to_string(&counters).map_err(|error| error.to_string())?
        );
        return Ok(());
    }
    if command == "memory-read" {
        if args.len() != 3 {
            return Err("Usage: l6ctl memory-read ADDRESS LENGTH".into());
        }
        let address = if let Some(hex) = args[1].strip_prefix("0x") {
            u32::from_str_radix(hex, 16)
        } else {
            args[1].parse()
        }
        .map_err(|_| "invalid address")?;
        let length: usize = args[2].parse().map_err(|_| "invalid length")?;
        let sample = client
            .read_main_memory(address, length)
            .map_err(|e| e.to_string())?;
        println!(
            "{}",
            serde_json::to_string(&sample).map_err(|e| e.to_string())?
        );
        return Ok(());
    }
    if command == "midi-config" {
        if args.len() != 1 {
            return Err("Usage: l6ctl midi-config".into());
        }
        let config = client.midi_config().map_err(|error| error.to_string())?;
        println!(
            "{}",
            serde_json::to_string(&config).map_err(|error| error.to_string())?
        );
        return Ok(());
    }
    if command == "update-state" {
        if args.len() != 1 {
            return Err("Usage: l6ctl update-state".into());
        }
        let state = client.update_state().map_err(|error| error.to_string())?;
        println!(
            "{}",
            serde_json::to_string(&state).map_err(|error| error.to_string())?
        );
        return Ok(());
    }
    if command == "update-gui-state" {
        if args.len() != 1 {
            return Err("Usage: l6ctl update-gui-state".into());
        }
        let state = client
            .update_gui_state()
            .map_err(|error| error.to_string())?;
        println!(
            "{}",
            serde_json::to_string(&state).map_err(|error| error.to_string())?
        );
        return Ok(());
    }
    if command == "installer-outcome" {
        if args.len() != 1 {
            return Err("Usage: l6ctl installer-outcome".into());
        }
        let outcome = client
            .installer_outcome()
            .map_err(|error| error.to_string())?;
        println!(
            "{}",
            serde_json::to_string(&outcome).map_err(|error| error.to_string())?
        );
        return Ok(());
    }
    if command == "usb" {
        let value = match args.as_slice() {
            [_, sub] if sub == "attach" => {
                serde_json::to_value(client.usb_attach().map_err(|e| e.to_string())?)
            }
            [_, sub] if sub == "release" => {
                serde_json::to_value(client.usb_release().map_err(|e| e.to_string())?)
            }
            [_, sub] if sub == "controller" => {
                serde_json::to_value(client.usb_controller().map_err(|e| e.to_string())?)
            }
            _ => return Err("Usage: l6ctl usb attach|release|controller".into()),
        }
        .map_err(|e| e.to_string())?;
        println!(
            "{}",
            serde_json::to_string(&value).map_err(|e| e.to_string())?
        );
        return Ok(());
    }
    if command == "knob-parameter-state" {
        if args.len() != 1 {
            return Err("Usage: l6ctl knob-parameter-state".into());
        }
        let state = client
            .knob_parameter_state()
            .map_err(|error| error.to_string())?;
        println!(
            "{}",
            serde_json::to_string(&state).map_err(|error| error.to_string())?
        );
        return Ok(());
    }
    if command == "recorder-state" {
        if args.len() != 1 {
            return Err("Usage: l6ctl recorder-state".into());
        }
        let state = client.recorder_state().map_err(|error| error.to_string())?;
        println!(
            "{}",
            serde_json::to_string(&state).map_err(|error| error.to_string())?
        );
        return Ok(());
    }
    if command == "knob-popup" {
        if !(args.len() == 1 || (args.len() == 2 && args[1] == "--text")) {
            return Err("Usage: l6ctl knob-popup [--text]".into());
        }
        let snapshot = client
            .knob_popup(args.len() == 2)
            .map_err(|error| error.to_string())?;
        println!(
            "{}",
            serde_json::to_string(&snapshot).map_err(|error| error.to_string())?
        );
        return Ok(());
    }
    if command == "rtos" {
        let kind = args
            .get(1)
            .map(String::as_str)
            .ok_or("Usage: l6ctl rtos heap|tasks|scheduler|objects|all [--pretty]")?;
        if !["heap", "tasks", "scheduler", "objects", "all"].contains(&kind)
            || !(args.len() == 2 || (args.len() == 3 && args[2] == "--pretty"))
        {
            return Err("Usage: l6ctl rtos heap|tasks|scheduler|objects|all [--pretty]".into());
        }
        let kind = match kind {
            "heap" => RtosKind::Heap,
            "tasks" => RtosKind::Tasks,
            "scheduler" => RtosKind::Scheduler,
            "objects" => RtosKind::Objects,
            "all" => RtosKind::All,
            _ => unreachable!(),
        };
        let result = client.rtos(kind).map_err(|e| e.to_string())?;
        println!(
            "{}",
            if args.len() == 3 {
                serde_json::to_string_pretty(&result)
            } else {
                serde_json::to_string(&result)
            }
            .map_err(|e| e.to_string())?
        );
        return Ok(());
    }
    if command == "info" {
        println!(
            "{}",
            serde_json::to_string_pretty(
                &client.request(&Request::Info).map_err(|e| e.to_string())?
            )
            .unwrap()
        );
        return Ok(());
    }
    let info = client.request(&Request::Info).map_err(|e| e.to_string())?;
    let model = match info.get("model").and_then(Value::as_str) {
        Some("l6") => Model::L6,
        Some("l6max") => Model::L6max,
        Some(other) => return Err(format!("emulator returned unknown model {other:?}")),
        None => return Err("emulator info omitted model".into()),
    };
    let message = match command {
        "button" | "tap" | "down" | "up" => {
            let target = target(
                model,
                args.get(1).ok_or("button command needs a name or target")?,
            )?;
            match command {
                "button" | "tap" => {
                    let mut hold_ms = 50u32;
                    if args.len() == 4 && args[2] == "--hold-ms" {
                        hold_ms = parse_int(args.get(3), "hold duration")?;
                    } else if args.len() != 2 {
                        return Err("Usage: l6ctl button NAME|TARGET [--hold-ms MS]".into());
                    }
                    Request::Tap { target, hold_ms }
                }
                "down" => {
                    if args.len() != 3 {
                        return Err("Usage: l6ctl down NAME|TARGET DURATION_MS".into());
                    }
                    let duration_ms: u32 = parse_int(args.get(2), "duration")?;
                    Request::Down {
                        target,
                        duration_ms,
                    }
                }
                "up" => {
                    if args.len() != 2 {
                        return Err("Usage: l6ctl up NAME|TARGET".into());
                    }
                    Request::Up { target }
                }
                _ => unreachable!(),
            }
        }
        "encoder" => {
            if args.len() != 3 {
                return Err("Usage: l6ctl encoder CHANNEL STEPS".into());
            }
            let channel: u32 = parse_int(args.get(1), "channel")?;
            let steps: i32 = parse_int(args.get(2), "steps")?;
            Request::Encoder { channel, steps }
        }
        "analog" => {
            if args.len() != 3 {
                return Err("Usage: l6ctl analog CHANNEL STEPS".into());
            }
            let channel: u32 = parse_int(args.get(1), "channel")?;
            let steps: i32 = parse_int(args.get(2), "steps")?;
            Request::Analog { channel, steps }
        }
        "set-analog" => {
            if args.len() != 3 {
                return Err("Usage: l6ctl set-analog CHANNEL VALUE".into());
            }
            let channel: u32 = parse_int(args.get(1), "channel")?;
            let value: u16 = parse_int(args.get(2), "value")?;
            Request::SetAnalog { channel, value }
        }
        other => return Err(format!("unknown command {other:?}; use `l6ctl --help`")),
    };
    let response = match message {
        Request::Tap { target, hold_ms } => {
            serde_json::to_value(client.tap(target, hold_ms).map_err(|e| e.to_string())?)
        }
        Request::Down {
            target,
            duration_ms,
        } => serde_json::to_value(
            client
                .down(target, duration_ms)
                .map_err(|e| e.to_string())?,
        ),
        Request::Up { target } => {
            serde_json::to_value(client.up(target).map_err(|e| e.to_string())?)
        }
        Request::Encoder { channel, steps } => {
            serde_json::to_value(client.encoder(channel, steps).map_err(|e| e.to_string())?)
        }
        Request::Analog { channel, steps } => {
            serde_json::to_value(client.analog(channel, steps).map_err(|e| e.to_string())?)
        }
        Request::SetAnalog { channel, value } => serde_json::to_value(
            client
                .set_analog(channel, value)
                .map_err(|e| e.to_string())?,
        ),
        Request::Info
        | Request::InputStatus { .. }
        | Request::HeapCounters
        | Request::MemoryRead { .. }
        | Request::MidiConfig
        | Request::UpdateState
        | Request::UpdateGuiState
        | Request::InstallerOutcome
        | Request::UsbAttach
        | Request::UsbRelease
        | Request::UsbController
        | Request::RecorderState
        | Request::KnobParameterState
        | Request::KnobPopup { .. }
        | Request::Rtos { .. } => unreachable!(),
    }
    .map_err(|e| e.to_string())?;
    println!("{}", serde_json::to_string(&response).unwrap());
    Ok(())
}

fn main() {
    let mut arguments = env::args().skip(1);
    let mut rtos = false;
    let mut midi = false;
    while let Some(arg) = arguments.next() {
        if arg == "--socket" {
            arguments.next();
        } else {
            rtos = arg == "rtos";
            midi = arg == "midi";
            break;
        }
    }
    if let Err(error) = run() {
        if midi {
            eprintln!("{error}");
        } else if rtos {
            eprintln!("{}", json!({"schema":1,"error":error}));
        } else {
            eprintln!("l6ctl: {error}");
        }
        std::process::exit(2);
    }
}
