use l6ctl::{ControlClient, ControlError, Request, RtosKind};
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
