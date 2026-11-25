use l6ctl::{ControlClient, ControlError, InputSource, Request, RtosKind};
use l6fw::Model;
use serde_json::{Value, json};
use std::{
    io::{BufRead, BufReader, Write},
    os::unix::net::UnixListener,
    path::PathBuf,
    process::Command,
    sync::atomic::{AtomicUsize, Ordering},
    thread,
};

static NEXT: AtomicUsize = AtomicUsize::new(0);

fn socket_path() -> PathBuf {
    std::env::temp_dir().join(format!(
        "l6ctl-test-{}-{}.sock",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ))
}

fn server(path: &PathBuf, exchanges: Vec<(Value, Value)>) -> thread::JoinHandle<()> {
    let listener = UnixListener::bind(path).unwrap();
    thread::spawn(move || {
        for (expected, response) in exchanges {
            let (mut stream, _) = listener.accept().unwrap();
            let mut line = String::new();
            BufReader::new(&stream).read_line(&mut line).unwrap();
            assert_eq!(serde_json::from_str::<Value>(&line).unwrap(), expected);
            writeln!(stream, "{response}").unwrap();
        }
    })
}

fn info() -> Value {
    json!({"model":"l6max","guest_ms":42,"powered_off":false,
        "main_indicators":vec![0;8],"panel_indicators":vec![0;8]})
}

#[test]
fn input_status_client_and_cli_use_the_same_typed_wire_contract() {
    let path = socket_path();
    let panel = json!({"source":"panel", "connected":true, "guest_ms":42,
        "sent":7, "applied":6, "released":3, "rejected":1, "error":null});
    let main = json!({"source":"main", "connected":false, "guest_ms":0,
        "sent":2, "applied":1, "released":0, "rejected":0, "error":"disconnected"});
    let server = server(
        &path,
        vec![
            (
                json!({"command":"input-status", "source":"panel"}),
                json!({"ok":true,"result":panel}),
            ),
            (
                json!({"command":"input-status", "source":"main"}),
                json!({"ok":true,"result":main}),
            ),
        ],
    );
    let status = ControlClient::new(&path)
        .input_status(InputSource::Panel)
        .unwrap();
    assert_eq!(
        (
            status.guest_ms,
            status.sent,
            status.applied,
            status.released,
            status.rejected
        ),
        (42, 7, 6, 3, 1)
    );
    let output = Command::new(env!("CARGO_BIN_EXE_l6ctl"))
        .arg("--socket")
        .arg(&path)
        .args(["input", "status", "main"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&output.stdout).unwrap(),
        main
    );
    server.join().unwrap();
    std::fs::remove_file(path).unwrap();
}

#[test]
fn input_status_cli_rejects_unknown_channel_before_connecting() {
    let output = Command::new(env!("CARGO_BIN_EXE_l6ctl"))
        .arg("--socket")
        .arg(socket_path())
        .args(["input", "status", "bad"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("input status main|panel"));
}

#[test]
fn typed_client_uses_existing_wire_format_and_surfaces_server_errors() {
    let path = socket_path();
    let server = server(
        &path,
        vec![
            (
                json!({"command":"info"}),
                json!({"ok":true,"result":info()}),
            ),
            (
                json!({"command":"encoder","channel":1,"steps":-4}),
                json!({"ok":true,"result":{"channel":1,"steps":-4}}),
            ),
            (
                json!({"command":"rtos","kind":"tasks"}),
                json!({"ok":true,"result":{"schema":1,"kind":"tasks","data":{"tasks":[]}}}),
            ),
            (
                json!({"command":"rtos","kind":"tasks"}),
                json!({"ok":false,"error":"unsupported firmware"}),
            ),
        ],
    );
    let client = ControlClient::new(&path);
    let state = client.info().unwrap();
    assert_eq!(state.model, Model::L6max);
    assert_eq!(state.guest_ms, 42);
    let receipt = client.encoder(1, -4).unwrap();
    assert_eq!((receipt.channel, receipt.steps), (1, -4));
    assert_eq!(
        client
            .request(&Request::Rtos {
                kind: RtosKind::Tasks
            })
            .unwrap(),
        json!({"schema":1,"kind":"tasks","data":{"tasks":[]}})
    );
    assert!(
        matches!(client.request(&Request::Rtos { kind: RtosKind::Tasks }), Err(ControlError::Rejected(message)) if message == "unsupported firmware")
    );
    server.join().unwrap();
    std::fs::remove_file(path).unwrap();
}

#[test]
fn missing_socket_reports_connection_failure() {
    let path = socket_path();
    let error = ControlClient::new(&path).info().unwrap_err();
    assert!(matches!(error, ControlError::Connect { .. }));
    assert!(error.to_string().contains("--control-socket"));
}

#[test]
fn memory_read_cli_and_client_use_bounded_read_wire_contract() {
    let path = socket_path();
    let sample = json!({"address":2147483676u32,"bytes":[223,155,87,19],"atomic":false});
    let server = server(
        &path,
        vec![
            (
                json!({"command":"memory-read","address":2147483676u32,"length":4}),
                json!({"ok":true,"result":sample}),
            ),
            (
                json!({"command":"memory-read","address":2147483676u32,"length":4}),
                json!({"ok":true,"result":sample}),
            ),
        ],
    );
    let client = ControlClient::new(&path);
    assert_eq!(
        client.read_main_memory(0x8000001c, 4).unwrap().bytes,
        [223, 155, 87, 19]
    );
    let output = Command::new(env!("CARGO_BIN_EXE_l6ctl"))
        .arg("--socket")
        .arg(&path)
        .args(["memory-read", "0x8000001c", "4"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&output.stdout).unwrap(),
        sample
    );
    server.join().unwrap();
    std::fs::remove_file(path).unwrap();
}

#[test]
fn heap_counters_cli_and_client_use_typed_guarded_snapshot() {
    let path = socket_path();
    let sample = json!({"model":"l6max","atomic":false,"revision_guard":"1.10_pvPortMalloc_vPortFree","free":100,"minimum_ever_free":200,"allocations":5,"frees":3});
    let server = server(
        &path,
        vec![
            (
                json!({"command":"heap-counters"}),
                json!({"ok":true,"result":sample}),
            ),
            (
                json!({"command":"heap-counters"}),
                json!({"ok":true,"result":sample}),
            ),
            (
                json!({"command":"heap-counters"}),
                json!({"ok":false,"error":"allocator differs from 1.10"}),
            ),
        ],
    );
    let client = ControlClient::new(&path);
    let counters = client.heap_counters().unwrap();
    assert_eq!(
        (
            counters.free,
            counters.minimum_ever_free,
            counters.allocations,
            counters.frees
        ),
        (100, 200, 5, 3)
    );
    let output = Command::new(env!("CARGO_BIN_EXE_l6ctl"))
        .arg("--socket")
        .arg(&path)
        .arg("heap-counters")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&output.stdout).unwrap(),
        sample
    );
    assert!(
        matches!(client.heap_counters(), Err(ControlError::Rejected(message)) if message == "allocator differs from 1.10")
    );
    server.join().unwrap();
    std::fs::remove_file(path).unwrap();
}

#[test]
fn midi_config_cli_and_client_use_typed_snapshot() {
    let path = socket_path();
    let sample = json!({"atomic":false,"revision_guard":"l6_1_10_midi_routines","channel":2,"cc_map":vec![255; 54]});
    let server = server(
        &path,
        vec![
            (
                json!({"command":"midi-config"}),
                json!({"ok":true,"result":sample}),
            ),
            (
                json!({"command":"midi-config"}),
                json!({"ok":true,"result":sample}),
            ),
            (
                json!({"command":"midi-config"}),
                json!({"ok":false,"error":"unsupported L6 MIDI revision"}),
            ),
        ],
    );
    let client = ControlClient::new(&path);
    let config = client.midi_config().unwrap();
    assert_eq!(config.channel, 2);
    assert_eq!(config.cc_map, vec![255; 54]);
    let output = Command::new(env!("CARGO_BIN_EXE_l6ctl"))
        .arg("--socket")
        .arg(&path)
        .arg("midi-config")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&output.stdout).unwrap(),
        sample
    );
    assert!(
        matches!(client.midi_config(), Err(ControlError::Rejected(message)) if message == "unsupported L6 MIDI revision")
    );
    server.join().unwrap();
    std::fs::remove_file(path).unwrap();
}

#[test]
fn update_state_cli_and_client_preserve_raw_snapshot() {
    let path = socket_path();
    let sample =
        json!({"atomic":false,"revision_guard":"l6max_1_10_update_routines","mode":1,"flags":169});
    let server = server(
        &path,
        vec![
            (
                json!({"command":"update-state"}),
                json!({"ok":true,"result":sample}),
            ),
            (
                json!({"command":"update-state"}),
                json!({"ok":true,"result":sample}),
            ),
            (
                json!({"command":"update-state"}),
                json!({"ok":false,"error":"unsupported update revision"}),
            ),
        ],
    );
    let client = ControlClient::new(&path);
    let state = client.update_state().unwrap();
    assert_eq!((state.mode, state.flags, state.atomic), (1, 169, false));
    let output = Command::new(env!("CARGO_BIN_EXE_l6ctl"))
        .arg("--socket")
        .arg(&path)
        .arg("update-state")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&output.stdout).unwrap(),
        sample
    );
    assert!(
        matches!(client.update_state(), Err(ControlError::Rejected(message)) if message == "unsupported update revision")
    );
    server.join().unwrap();
    std::fs::remove_file(path).unwrap();
}

#[test]
fn update_gui_state_cli_and_client_preserve_raw_snapshot() {
    let path = socket_path();
    let sample = json!({"atomic":false,"revision_guard":"l6max_1_10_update_gui_routines","current_window":2149581748u32,"progress_flags":256});
    let server = server(
        &path,
        vec![
            (
                json!({"command":"update-gui-state"}),
                json!({"ok":true,"result":sample}),
            ),
            (
                json!({"command":"update-gui-state"}),
                json!({"ok":true,"result":sample}),
            ),
            (
                json!({"command":"update-gui-state"}),
                json!({"ok":false,"error":"unsupported update GUI revision"}),
            ),
        ],
    );
    let client = ControlClient::new(&path);
    let state = client.update_gui_state().unwrap();
    assert_eq!(
        (state.current_window, state.progress_flags, state.atomic),
        (2149581748, 256, false)
    );
    let output = Command::new(env!("CARGO_BIN_EXE_l6ctl"))
        .arg("--socket")
        .arg(&path)
        .arg("update-gui-state")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&output.stdout).unwrap(),
        sample
    );
    assert!(
        matches!(client.update_gui_state(), Err(ControlError::Rejected(message)) if message == "unsupported update GUI revision")
    );
    server.join().unwrap();
    std::fs::remove_file(path).unwrap();
}

#[test]
fn installer_outcome_cli_and_client_preserve_result_and_unavailable() {
    let path = socket_path();
    let sample = json!({"status":"installed"});
    let server = server(
        &path,
        vec![
            (
                json!({"command":"installer-outcome"}),
                json!({"ok":true,"result":sample}),
            ),
            (
                json!({"command":"installer-outcome"}),
                json!({"ok":true,"result":sample}),
            ),
            (
                json!({"command":"installer-outcome"}),
                json!({"ok":true,"result":{"status":"failed","error":"bad card"}}),
            ),
            (
                json!({"command":"installer-outcome"}),
                json!({"ok":false,"error":"replacement installer was not run for this engine start"}),
            ),
        ],
    );
    let client = ControlClient::new(&path);
    assert_eq!(
        client.installer_outcome().unwrap(),
        l6ctl::InstallerOutcome::Installed
    );
    let output = Command::new(env!("CARGO_BIN_EXE_l6ctl"))
        .arg("--socket")
        .arg(&path)
        .arg("installer-outcome")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&output.stdout).unwrap(),
        sample
    );
    assert_eq!(
        client.installer_outcome().unwrap(),
        l6ctl::InstallerOutcome::Failed("bad card".into())
    );
    assert!(
        matches!(client.installer_outcome(), Err(ControlError::Rejected(message)) if message.contains("not run"))
    );
    server.join().unwrap();
    std::fs::remove_file(path).unwrap();
}

#[test]
fn usb_controller_cli_and_client_distinguish_attach_and_running() {
    let path = socket_path();
    let server = server(
        &path,
        vec![
            (
                json!({"command":"usb-attach"}),
                json!({"ok":true,"result":{"attached":true}}),
            ),
            (
                json!({"command":"usb-controller"}),
                json!({"ok":true,"result":{"running":false}}),
            ),
            (
                json!({"command":"usb-controller"}),
                json!({"ok":true,"result":{"running":true}}),
            ),
            (
                json!({"command":"usb-attach"}),
                json!({"ok":false,"error":"USB is owned by the native MIDI bridge"}),
            ),
        ],
    );
    let client = ControlClient::new(&path);
    assert!(client.usb_attach().unwrap().attached);
    assert!(!client.usb_controller().unwrap().running);
    let output = Command::new(env!("CARGO_BIN_EXE_l6ctl"))
        .arg("--socket")
        .arg(&path)
        .args(["usb", "controller"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&output.stdout).unwrap(),
        json!({"running":true})
    );
    assert!(
        matches!(client.usb_attach(), Err(ControlError::Rejected(message)) if message.contains("bridge"))
    );
    server.join().unwrap();
    std::fs::remove_file(path).unwrap();
}

#[test]
fn cli_keeps_existing_one_based_encoder_and_output_contract() {
    let path = socket_path();
    let server = server(
        &path,
        vec![
            (
                json!({"command":"info"}),
                json!({"ok":true,"result":info()}),
            ),
            (
                json!({"command":"encoder","channel":1,"steps":-4}),
                json!({"ok":true,"result":{"channel":1,"steps":-4}}),
            ),
        ],
    );
    let output = Command::new(env!("CARGO_BIN_EXE_l6ctl"))
        .arg("--socket")
        .arg(&path)
        .args(["encoder", "1", "-4"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&output.stdout).unwrap(),
        json!({"channel":1,"steps":-4})
    );
    server.join().unwrap();
    std::fs::remove_file(path).unwrap();
}

#[test]
fn cli_preserves_rtos_error_json() {
    let path = socket_path();
    let server = server(
        &path,
        vec![(
            json!({"command":"rtos","kind":"tasks"}),
            json!({"ok":false,"error":"unsupported firmware"}),
        )],
    );
    let output = Command::new(env!("CARGO_BIN_EXE_l6ctl"))
        .arg("--socket")
        .arg(&path)
        .args(["rtos", "tasks"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert_eq!(
        serde_json::from_slice::<Value>(&output.stderr).unwrap(),
        json!({"schema":1,"error":"unsupported firmware"})
    );
    server.join().unwrap();
    std::fs::remove_file(path).unwrap();
}

#[test]
fn knob_popup_cli_requests_optional_text_and_returns_the_snapshot() {
    let path = socket_path();
    let sample = json!({"revision_guard":"l6max_1_10_gui_routines","atomic":false,"window":2149581832u32,"state":[78,75,1,255],"callback":2147483649u32,"level":64,"text":"50%"});
    let server = server(
        &path,
        vec![(
            json!({"command":"knob-popup","with_text":true}),
            json!({"ok":true,"result":sample}),
        )],
    );
    let output = Command::new(env!("CARGO_BIN_EXE_l6ctl"))
        .arg("--socket")
        .arg(&path)
        .args(["knob-popup", "--text"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["text"], "50%");
    assert_eq!(result["atomic"], false);
    server.join().unwrap();
    std::fs::remove_file(path).unwrap();
}

#[test]
fn help_lists_knob_popup() {
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_l6ctl"))
        .arg("--help")
        .output()
        .expect("run l6ctl help");
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("knob-popup [--text]"));
}

#[test]
fn recorder_state_cli_reports_window_and_notification_flags() {
    let path = socket_path();
    let sample = json!({"revision_guard":"l6max_1_10_gui_routines","atomic":false,"window":2149581832u32,"notification_busy":0,"notification_active":1});
    let server = server(
        &path,
        vec![(
            json!({"command":"recorder-state"}),
            json!({"ok":true,"result":sample}),
        )],
    );
    let output = Command::new(env!("CARGO_BIN_EXE_l6ctl"))
        .arg("--socket")
        .arg(&path)
        .arg("recorder-state")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["notification_active"], 1);
    assert_eq!(result["atomic"], false);
    server.join().unwrap();
    std::fs::remove_file(path).unwrap();
}

#[test]
fn knob_parameter_state_cli_reports_mode_and_slots() {
    let path = socket_path();
    let sample = json!({"revision_guard":"l6max_1_10_gui_and_parameter_getter","atomic":false,"mode":9,"callback_slots":[0,0,0]});
    let server = server(
        &path,
        vec![(
            json!({"command":"knob-parameter-state"}),
            json!({"ok":true,"result":sample}),
        )],
    );
    let output = Command::new(env!("CARGO_BIN_EXE_l6ctl"))
        .arg("--socket")
        .arg(&path)
        .arg("knob-parameter-state")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["mode"], 9);
    assert_eq!(result["callback_slots"], json!([0, 0, 0]));
    assert_eq!(result["atomic"], false);
    server.join().unwrap();
    std::fs::remove_file(path).unwrap();
}
