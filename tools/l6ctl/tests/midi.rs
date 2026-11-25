use std::process::Command;

#[test]
fn midi_command_does_not_require_control_socket() {
    let output = Command::new(env!("CARGO_BIN_EXE_l6ctl"))
        .env_remove("L6_CONTROL_SOCKET")
        .args(["midi", "send", "unused", "16", "1", "2"])
        .output()
        .expect("run l6ctl");
    assert_eq!(output.status.code(), Some(2));
    let error = String::from_utf8(output.stderr).unwrap();
    let details: serde_json::Value = serde_json::from_str(&error).unwrap();
    assert_eq!(details["kind"], "invalid-argument");
    assert!(error.contains("invalid MIDI control change"), "{error}");
    assert!(!error.contains("control socket"), "{error}");
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
#[ignore = "requires a running CoreMIDI or ALSA sequencer service"]
fn native_listener_subscribes_before_stimulus_and_cleans_up() {
    use l6max_host::host_midi::{HostMidi, NativeMidi, consumer, endpoint_name};
    use std::{
        io::{BufRead, BufReader},
        process::{Child, Stdio},
        sync::mpsc,
        thread,
        time::{Duration, Instant, SystemTime, UNIX_EPOCH},
    };

    struct Guard(Child);
    impl Drop for Guard {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    fn start(endpoint: &str, timeout: &str) -> (Guard, mpsc::Receiver<String>) {
        let mut child = Command::new(env!("CARGO_BIN_EXE_l6ctl"))
            .env_remove("L6_CONTROL_SOCKET")
            .args(["midi", "listen", endpoint, "--timeout-ms", timeout])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let stdout = child.stdout.take().unwrap();
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                if tx.send(line.unwrap()).is_err() {
                    break;
                }
            }
        });
        (Guard(child), rx)
    }
    fn event(rx: &mpsc::Receiver<String>) -> serde_json::Value {
        serde_json::from_str(
            &rx.recv_timeout(Duration::from_secs(5))
                .expect("listener event"),
        )
        .unwrap()
    }
    fn wait(guard: &mut Guard) -> std::process::ExitStatus {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(status) = guard.0.try_wait().unwrap() {
                return status;
            }
            assert!(Instant::now() < deadline, "listener did not exit");
            thread::sleep(Duration::from_millis(5));
        }
    }

    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let name = format!("l6ctl-test-{}-{nonce}", std::process::id());
    let endpoint = endpoint_name(&name);
    let mut ports = NativeMidi::open(&[&name]).expect("native MIDI service is unavailable");

    // A fresh subprocess subscribes before every stimulus, including on reuse.
    for value in [42, 43] {
        let (mut child, rx) = start(&endpoint, "3000");
        assert_eq!(event(&rx)["event"], "ready");
        #[cfg(target_os = "linux")]
        {
            ports.write(0, &[0xc0, 12]).unwrap();
            ports.write(0, &[0xe0, 0, 64]).unwrap();
        }
        ports.write(0, &[0xb0, 81, value]).unwrap();
        let message = event(&rx);
        assert_eq!(message["event"], "control-change");
        assert_eq!(message["channel"], 0);
        assert_eq!(message["controller"], 81);
        assert_eq!(message["value"], value);
        assert!(wait(&mut child).success());
    }

    // A single sync command keeps its subscription through drain, request,
    // and a coalesced/running-status response plus a separate message.
    let mut child = Command::new(env!("CARGO_BIN_EXE_l6ctl"))
        .env_remove("L6_CONTROL_SOCKET")
        .args([
            "midi",
            "sync",
            &endpoint,
            "--count",
            "4",
            "--quiet-ms",
            "10",
            "--timeout-ms",
            "3000",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let stdout = child.stdout.take().unwrap();
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            if tx.send(line.unwrap()).is_err() {
                break;
            }
        }
    });
    let mut child = Guard(child);
    assert_eq!(event(&rx)["event"], "ready");
    let deadline = Instant::now() + Duration::from_secs(2);
    let mut device_stream = l6max_host::usb_midi::Encoder::default();
    loop {
        let messages = ports
            .read(16)
            .unwrap()
            .into_iter()
            .flat_map(|(_, bytes)| {
                l6max_host::usb_midi::decode(&device_stream.push(0, &bytes)).unwrap()
            })
            .collect::<Vec<_>>();
        if messages.iter().any(|(_, bytes)| bytes == &[0xb0, 127, 127]) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "sync request did not reach endpoint"
        );
        thread::sleep(Duration::from_millis(5));
    }
    ports.write(0, &[0xb0, 81, 45, 81, 46, 81, 47]).unwrap();
    ports.write(0, &[0xb0, 81, 48]).unwrap();
    for expected in [45, 46, 47, 48] {
        let line = match rx.recv_timeout(Duration::from_secs(5)) {
            Ok(line) => line,
            Err(error) => {
                use std::io::Read;
                let mut stderr = String::new();
                child
                    .0
                    .stderr
                    .take()
                    .unwrap()
                    .read_to_string(&mut stderr)
                    .unwrap();
                panic!("event {expected}: {error}; stderr={stderr}");
            }
        };
        let value: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(value["value"], expected);
    }
    assert!(wait(&mut child).success());

    // An interrupted subscriber must release its native resources as well.
    let (mut interrupted, rx) = start(&endpoint, "3000");
    assert_eq!(event(&rx)["event"], "ready");
    interrupted.0.kill().unwrap();
    assert!(!wait(&mut interrupted).success());
    let (mut restarted, rx) = start(&endpoint, "3000");
    assert_eq!(event(&rx)["event"], "ready");
    ports.write(0, &[0xb0, 81, 44]).unwrap();
    assert_eq!(event(&rx)["value"], 44);
    assert!(wait(&mut restarted).success());

    // Releasing a session removes its subscriptions without losing the
    // ability to observe whether the endpoint still exists.
    let mut session = l6ctl::midi::MidiSession::connect_native(&endpoint).unwrap();
    session.release();
    assert_eq!(session.endpoint_present(), Some(true));
    ports.write(0, &[0xb0, 81, 99]).unwrap();
    thread::sleep(Duration::from_millis(50));
    assert_eq!(session.poll_control_change().unwrap(), None);
    drop(session);

    // A live CLI send reaches the virtual device side of the same endpoint.
    let output = Command::new(env!("CARGO_BIN_EXE_l6ctl"))
        .env_remove("L6_CONTROL_SOCKET")
        .args(["midi", "send", &endpoint, "0", "81", "55"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let sent: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(sent["event"], "sent");
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let messages = ports
            .read(16)
            .unwrap()
            .into_iter()
            .flat_map(|(_, bytes)| {
                l6max_host::usb_midi::decode(&device_stream.push(0, &bytes)).unwrap()
            })
            .collect::<Vec<_>>();
        if messages.iter().any(|(_, bytes)| bytes == &[0xb0, 81, 55]) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "virtual endpoint did not receive CLI send"
        );
        thread::sleep(Duration::from_millis(5));
    }

    let (mut child, rx) = start(&endpoint, "100");
    assert_eq!(event(&rx)["event"], "ready");
    assert!(!wait(&mut child).success());
    let stderr = child.0.stderr.take().unwrap();
    let error: serde_json::Value = serde_json::from_reader(stderr).unwrap();
    assert_eq!(error["kind"], "timeout");

    drop(ports);
    assert!(
        consumer::connect(&endpoint).is_err(),
        "virtual endpoint persisted after drop"
    );
    let output = Command::new(env!("CARGO_BIN_EXE_l6ctl"))
        .env_remove("L6_CONTROL_SOCKET")
        .args(["midi", "listen", &endpoint])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let error: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(error["kind"], "endpoint-unavailable");
}
