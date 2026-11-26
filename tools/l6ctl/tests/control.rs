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
    let panel = json!({"source":"panel", "connected":true, "guest_ms":42, "clock_updates":2, "powered_off":false, "powered_off_guest_ms":null,
        "sent":7, "applied":6, "released":3, "rejected":1, "error":null});
    let main = json!({"source":"main", "connected":false, "guest_ms":0, "clock_updates":0, "powered_off":false, "powered_off_guest_ms":null,
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
fn sd_loader_status_and_site_use_typed_socket_and_cli() {
    let path = socket_path();
    let status = json!({"revision_guard":"l6_1_10","atomic":false,"registry":null,"stage":null,"claim":null,"module_base":null,"module_len":null,"site_count":null});
    let site = json!({"atomic":false,"registry":2155876352u64,"index":1,"record":2155876624u64,"handler":0,"first":0,"last":0,"count":0,"links":[]});
    let server = server(
        &path,
        vec![
            (
                json!({"command":"sd-loader-status"}),
                json!({"ok":true,"result":status}),
            ),
            (
                json!({"command":"sd-loader-site","index":1}),
                json!({"ok":true,"result":site}),
            ),
        ],
    );
    let actual = ControlClient::new(&path).sd_loader_status().unwrap();
    assert_eq!(actual.registry, None);
    assert!(!actual.atomic);
    let output = Command::new(env!("CARGO_BIN_EXE_l6ctl"))
        .arg("--socket")
        .arg(&path)
        .args(["sd-loader", "site", "1"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&output.stdout).unwrap(),
        site
    );
    server.join().unwrap();
    std::fs::remove_file(path).unwrap();
}

#[test]
fn input_ack_client_and_cli_preserve_sequence_and_state() {
    let path = socket_path();
    let response =
        json!({"source":"panel", "sequence":17, "state":"applied", "applied_guest_ms":73});
    let exchange = (
        json!({"command":"input-ack", "source":"panel", "sequence":17}),
        json!({"ok":true,"result":response}),
    );
    let server = server(&path, vec![exchange.clone(), exchange]);
    let ack = ControlClient::new(&path)
        .input_ack(InputSource::Panel, 17)
        .unwrap();
    assert_eq!(ack.state, l6ctl::InputAckState::Applied);
    assert_eq!(ack.applied_guest_ms, Some(73));
    let output = Command::new(env!("CARGO_BIN_EXE_l6ctl"))
        .arg("--socket")
        .arg(&path)
        .args(["input", "ack", "panel", "17"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&output.stdout).unwrap(),
        response
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
                json!({"ok":true,"result":{"channel":1,"steps":-4,"sequence":17}}),
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
fn mixer_level_client_and_cli_preserve_guard_and_value() {
    let path = socket_path();
    let sample = json!({"model":"l6","channel":1,"value":32,"getter_guard":"l6_1_10_mixer_getter","atomic":false});
    let server = server(
        &path,
        vec![
            (
                json!({"command":"mixer-level"}),
                json!({"ok":true,"result":sample}),
            ),
            (
                json!({"command":"mixer-level"}),
                json!({"ok":true,"result":sample}),
            ),
            (
                json!({"command":"mixer-level"}),
                json!({"ok":false,"error":"unsupported getter"}),
            ),
        ],
    );
    let client = ControlClient::new(&path);
    assert_eq!(client.mixer_level().unwrap().value, 32);
    let output = Command::new(env!("CARGO_BIN_EXE_l6ctl"))
        .arg("--socket")
        .arg(&path)
        .arg("mixer-level")
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
        matches!(client.mixer_level(), Err(ControlError::Rejected(message)) if message == "unsupported getter")
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
                json!({"ok":true,"result":{"channel":1,"steps":-4,"sequence":17}}),
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
        json!({"channel":1,"steps":-4,"sequence":17})
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

#[test]
fn debugger_uses_one_persistent_typed_connection_for_api_and_cli() {
    use l6ctl::{DebugCommand, DebugObservation};
    use std::process::Stdio;
    let path = socket_path();
    let listener = UnixListener::bind(&path).unwrap();
    let server = thread::spawn(move || {
        for _ in 0..2 {
            let (mut stream, _) = listener.accept().unwrap();
            let mut input = BufReader::new(stream.try_clone().unwrap());
            let mut line = String::new();
            input.read_line(&mut line).unwrap();
            assert_eq!(
                serde_json::from_str::<Value>(&line).unwrap(),
                json!({"command":"debug-begin"})
            );
            writeln!(stream, "{}", json!({"ok":true,"result":{"started":true}})).unwrap();
            line.clear();
            input.read_line(&mut line).unwrap();
            assert_eq!(
                serde_json::from_str::<Value>(&line).unwrap(),
                json!({"command":"register","index":15})
            );
            writeln!(
                stream,
                "{}",
                json!({"ok":true,"result":{"kind":"register","index":15,"value":42}})
            )
            .unwrap();
            line.clear();
            input.read_line(&mut line).unwrap();
            assert_eq!(
                serde_json::from_str::<Value>(&line).unwrap(),
                json!({"command":"close"})
            );
            writeln!(stream, "{}", json!({"ok":true,"result":{"kind":"closed"}})).unwrap();
        }
    });
    let mut session = ControlClient::new(&path).debug_session().unwrap();
    assert!(matches!(
        session
            .execute(DebugCommand::Register { index: 15 })
            .unwrap(),
        DebugObservation::Register {
            index: 15,
            value: 42
        }
    ));
    assert!(matches!(
        session.execute(DebugCommand::Close).unwrap(),
        DebugObservation::Closed
    ));
    assert!(
        session
            .execute(DebugCommand::Register { index: 15 })
            .is_err()
    );
    let mut child = Command::new(env!("CARGO_BIN_EXE_l6ctl"))
        .arg("--socket")
        .arg(&path)
        .arg("debug-session")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    writeln!(stdin, "{{\"command\":\"register\",\"index\":15}}").unwrap();
    drop(stdin);
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let lines = String::from_utf8(output.stdout).unwrap();
    assert!(
        lines.contains("\"value\":42") && lines.contains("\"kind\":\"closed\""),
        "{lines}"
    );
    server.join().unwrap();
    std::fs::remove_file(path).unwrap();
}

#[test]
fn effect_editor_client_uses_bounded_typed_wire_contract() {
    let path = socket_path();
    let binding = l6ctl::EffectEditorBinding {
        module_len: 512,
        code_len: 256,
        state_offset: 128,
        hook_offset: 0,
        hook_guard: [3; 32],
        immutable_sha256: "b".repeat(64),
        masked_words: vec![],
        applied_offset: 256,
        dropped_offset: 260,
        pending_offset: 264,
        gate_offset: 128,
        fixture_module_sha256: "a".repeat(64),
    };
    let response = json!({"revision_guard":"l6max_1_10_fixture_hook","fixture_module_sha256":"a".repeat(64),"atomic":false,"applied":7,"dropped":1,"pending":0,"gate":0});
    let server = server(
        &path,
        vec![(
            json!({"command":"effect-editor","binding":binding}),
            json!({"ok":true,"result":response}),
        )],
    );
    let sample = ControlClient::new(&path).effect_editor(binding).unwrap();
    assert_eq!(
        (sample.applied, sample.dropped, sample.pending, sample.gate),
        (7, 1, 0, 0)
    );
    assert!(!sample.atomic);
    server.join().unwrap();
    std::fs::remove_file(path).unwrap();
}

#[test]
fn effect_slot_client_uses_typed_non_atomic_wire_contract() {
    let path = socket_path();
    let identity = l6max_host::effect_editor::SlotIdentity {
        module_len: 512,
        code_len: 256,
        state_offset: 128,
        guard_offset: 0,
        guard: [3; 32],
        immutable_sha256: "b".repeat(64),
        masked_words: vec![],
        fixture_module_sha256: "a".repeat(64),
    };
    let binding = l6ctl::EffectSlotBinding {
        identity,
        processor: 268,
        panel_mode: 272,
        edit_mode: 276,
        applied: Some(256),
        exit_refreshes: None,
        active_pointer: None,
        active_length: None,
        room_output: None,
        spring_output: None,
        parameters: Some(280),
        dialog_active: None,
    };
    let fields = l6ctl::EffectSlotFields {
        parameter_id: Some(2),
        ..Default::default()
    };
    let response = json!({"revision_guard":"l6max_1_10_fixture_hook","fixture_module_sha256":"a".repeat(64),"atomic":false,
        "effect_type":1,"processor_type":1,"panel_mode":2,"edit_mode":1,"applied":null,"exit_refreshes":null,
        "active_effect":null,"active_effect_bytes":null,"output_blocks":null,"parameter_bits":2143289344u32,"dialog_active":null});
    let server = server(
        &path,
        vec![(
            json!({"command":"effect-slot","binding":binding,"fields":fields}),
            json!({"ok":true,"result":response}),
        )],
    );
    let sample = ControlClient::new(&path)
        .effect_slot(binding, fields)
        .unwrap();
    assert!(!sample.atomic);
    assert!(f32::from_bits(sample.parameter_bits.unwrap()).is_nan());
    server.join().unwrap();
    std::fs::remove_file(path).unwrap();
}

#[test]
fn panel_key_client_and_cli_use_typed_wire_contract() {
    let path = socket_path();
    let result = json!({"revision_guard":"l6max_1_10_panel_handler_and_gui","atomic":false,"key_code":25,"bit":1,"word_index":4,"words":[0,0,0,0,2,0]});
    let server = server(
        &path,
        vec![
            (
                json!({"command":"panel-key","code":25}),
                json!({"ok":true,"result":result}),
            ),
            (
                json!({"command":"panel-key","code":25}),
                json!({"ok":true,"result":result}),
            ),
        ],
    );
    let sample = ControlClient::new(&path).panel_key(25).unwrap();
    assert_eq!(
        (
            sample.key_code,
            sample.bit,
            sample.word_index,
            sample.words[4]
        ),
        (25, 1, 4, 2)
    );
    let output = Command::new(env!("CARGO_BIN_EXE_l6ctl"))
        .arg("--socket")
        .arg(&path)
        .args(["panel-key", "25"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&output.stdout).unwrap(),
        result
    );
    server.join().unwrap();
    std::fs::remove_file(path).unwrap();
}

#[test]
fn power_receipt_and_qmp_status_are_available_to_socket_and_cli() {
    let path = socket_path();
    let receipt = json!({"target":53,"command":"tap","sequence":41});
    let status = json!({"running":false,"status":"shutdown"});
    let server = server(
        &path,
        vec![
            (
                json!({"command":"tap","target":53,"hold_ms":100}),
                json!({"ok":true,"result":receipt}),
            ),
            (
                json!({"command":"qmp-status"}),
                json!({"ok":true,"result":status}),
            ),
        ],
    );
    let result = ControlClient::new(&path).tap(53, 100).unwrap();
    assert_eq!(result.sequence, 41);
    let output = Command::new(env!("CARGO_BIN_EXE_l6ctl"))
        .arg("--socket")
        .arg(&path)
        .arg("qmp-status")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&output.stdout).unwrap(),
        status
    );
    server.join().unwrap();
    std::fs::remove_file(path).unwrap();
}
