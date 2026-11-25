use l6ctl::midi::{ControlChange, MidiSession};
use serde_json::json;
use std::{
    io::Write,
    thread,
    time::{Duration, Instant},
};

const USAGE: &str = "Usage: l6ctl midi listen ENDPOINT [--timeout-ms MS] | l6ctl midi send ENDPOINT CHANNEL CONTROLLER VALUE | l6ctl midi sync ENDPOINT --count N [--quiet-ms MS] [--timeout-ms MS]";

#[derive(Debug)]
pub struct MidiCliError {
    kind: &'static str,
    message: String,
}

impl MidiCliError {
    fn new(kind: &'static str, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    fn connection(message: String) -> Self {
        let kind = match message.as_str() {
            "missing native MIDI source"
            | "missing native MIDI destination"
            | "missing native MIDI port" => "endpoint-unavailable",
            "host MIDI smoke requires CoreMIDI or ALSA" => "unsupported",
            _ => "transport",
        };
        Self::new(kind, message)
    }

    pub fn json(&self) -> String {
        json!({"event":"error", "kind":self.kind, "message":self.message}).to_string()
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Command {
    Listen {
        endpoint: String,
        timeout: Duration,
    },
    Send {
        endpoint: String,
        message: ControlChange,
    },
    Sync {
        endpoint: String,
        count: usize,
        quiet: Duration,
        timeout: Duration,
    },
}

fn parse(args: &[String]) -> Result<Command, String> {
    match args.first().map(String::as_str) {
        Some("listen") if args.len() == 2 || (args.len() == 4 && args[2] == "--timeout-ms") => {
            let ms = if args.len() == 4 {
                args[3]
                    .parse()
                    .map_err(|_| "invalid timeout in milliseconds")?
            } else {
                30_000
            };
            Ok(Command::Listen {
                endpoint: args[1].clone(),
                timeout: Duration::from_millis(ms),
            })
        }
        Some("sync") if args.len() >= 2 => {
            let mut count = None;
            let mut quiet = 500u64;
            let mut timeout = 30_000u64;
            let mut seen_quiet = false;
            let mut seen_timeout = false;
            let mut i = 2;
            while i < args.len() {
                let value = args.get(i + 1).ok_or("missing sync option value")?;
                match args[i].as_str() {
                    "--count" if count.is_none() => {
                        let parsed: usize = value.parse().map_err(|_| "invalid sync count")?;
                        if !(1..=16384).contains(&parsed) {
                            return Err("sync count must be 1..16384".into());
                        }
                        count = Some(parsed);
                    }
                    "--quiet-ms" if !seen_quiet => {
                        quiet = value.parse().map_err(|_| "invalid quiet interval")?;
                        seen_quiet = true;
                    }
                    "--timeout-ms" if !seen_timeout => {
                        timeout = value
                            .parse()
                            .map_err(|_| "invalid timeout in milliseconds")?;
                        seen_timeout = true;
                    }
                    _ => return Err(USAGE.into()),
                }
                i += 2;
            }
            if quiet == 0 || quiet >= timeout {
                return Err("quiet interval must be positive and shorter than timeout".into());
            }
            Ok(Command::Sync {
                endpoint: args[1].clone(),
                count: count.ok_or("sync requires --count")?,
                quiet: Duration::from_millis(quiet),
                timeout: Duration::from_millis(timeout),
            })
        }
        Some("send") if args.len() == 5 => {
            let message = ControlChange {
                channel: args[2].parse().map_err(|_| "invalid channel")?,
                controller: args[3].parse().map_err(|_| "invalid controller")?,
                value: args[4].parse().map_err(|_| "invalid value")?,
            };
            message.to_bytes()?;
            Ok(Command::Send {
                endpoint: args[1].clone(),
                message,
            })
        }
        _ => Err(USAGE.into()),
    }
}

fn listen(
    endpoint: &str,
    timeout: Duration,
    mut poll: impl FnMut() -> Result<Option<ControlChange>, String>,
    output: &mut impl Write,
) -> Result<(), MidiCliError> {
    let deadline = Instant::now()
        .checked_add(timeout)
        .ok_or_else(|| MidiCliError::new("invalid-argument", "timeout is too large"))?;
    writeln!(output, "{}", json!({"event":"ready", "endpoint":endpoint}))
        .and_then(|()| output.flush())
        .map_err(|e| MidiCliError::new("output", e.to_string()))?;
    while Instant::now() < deadline {
        if let Some(message) = poll().map_err(|e| MidiCliError::new("transport", e))? {
            writeln!(output, "{}", json!({"event":"control-change", "channel":message.channel, "controller":message.controller, "value":message.value}))
                .and_then(|()| output.flush())
                .map_err(|e| MidiCliError::new("output", e.to_string()))?;
            return Ok(());
        }
        thread::sleep(
            Duration::from_millis(5).min(deadline.saturating_duration_since(Instant::now())),
        );
    }
    Err(MidiCliError::new(
        "timeout",
        "timed out waiting for a MIDI control change",
    ))
}

trait MidiIo {
    fn poll(&mut self) -> Result<Option<ControlChange>, String>;
    fn send(&mut self, message: ControlChange) -> Result<(), String>;
}
impl MidiIo for MidiSession {
    fn poll(&mut self) -> Result<Option<ControlChange>, String> {
        self.poll_control_change()
    }
    fn send(&mut self, message: ControlChange) -> Result<(), String> {
        self.send_control_change(message)
    }
}

fn wait_for_quiet(
    client: &mut impl MidiIo,
    quiet: Duration,
    deadline: Instant,
    mut now: impl FnMut() -> Instant,
    mut sleep: impl FnMut(Duration),
) -> Result<(), MidiCliError> {
    let mut quiet_since = now();
    loop {
        let instant = now();
        if instant >= deadline {
            return Err(MidiCliError::new(
                "timeout",
                "timed out waiting for MIDI quiet",
            ));
        }
        // Poll even at the quiet boundary: a message may have arrived during
        // the final sleep and must not be counted as a response to our request.
        if client
            .poll()
            .map_err(|e| MidiCliError::new("transport", e))?
            .is_some()
        {
            quiet_since = now();
        } else {
            let instant = now();
            if instant >= deadline {
                return Err(MidiCliError::new(
                    "timeout",
                    "timed out waiting for MIDI quiet",
                ));
            }
            let remaining = quiet.saturating_sub(instant.duration_since(quiet_since));
            if remaining.is_zero() {
                return Ok(());
            }
            sleep(
                Duration::from_millis(5)
                    .min(remaining)
                    .min(deadline.saturating_duration_since(instant)),
            );
        }
    }
}

/// Observe a quiet interval, then send the state request and stream replies
/// over one subscription. Both phases share one wall-clock deadline.
fn sync(
    endpoint: &str,
    count: usize,
    quiet: Duration,
    timeout: Duration,
    client: &mut impl MidiIo,
    output: &mut impl Write,
) -> Result<(), MidiCliError> {
    let deadline = Instant::now()
        .checked_add(timeout)
        .ok_or_else(|| MidiCliError::new("invalid-argument", "timeout is too large"))?;
    wait_for_quiet(client, quiet, deadline, Instant::now, thread::sleep)?;
    writeln!(output, "{}", json!({"event":"ready", "endpoint":endpoint}))
        .and_then(|()| output.flush())
        .map_err(|e| MidiCliError::new("output", e.to_string()))?;
    if Instant::now() >= deadline {
        return Err(MidiCliError::new(
            "timeout",
            "timed out before sending the MIDI request",
        ));
    }
    let request = ControlChange {
        channel: 0,
        controller: 127,
        value: 127,
    };
    client
        .send(request)
        .map_err(|e| MidiCliError::new("transport", e))?;
    let mut received = 0;
    while received < count {
        if Instant::now() >= deadline {
            return Err(MidiCliError::new(
                "timeout",
                format!("timed out after {received}/{count} MIDI control changes"),
            ));
        }
        if let Some(message) = client
            .poll()
            .map_err(|e| MidiCliError::new("transport", e))?
        {
            if Instant::now() >= deadline {
                return Err(MidiCliError::new(
                    "timeout",
                    format!("timed out after {received}/{count} MIDI control changes"),
                ));
            }
            writeln!(output, "{}", json!({"event":"control-change", "channel":message.channel, "controller":message.controller, "value":message.value}))
                .and_then(|()| output.flush()).map_err(|e| MidiCliError::new("output", e.to_string()))?;
            received += 1;
        } else {
            thread::sleep(
                Duration::from_millis(5).min(deadline.saturating_duration_since(Instant::now())),
            );
        }
    }
    Ok(())
}

pub fn run(args: &[String]) -> Result<(), MidiCliError> {
    match parse(args).map_err(|e| MidiCliError::new("invalid-argument", e))? {
        Command::Listen { endpoint, timeout } => {
            let mut session =
                MidiSession::connect_native(&endpoint).map_err(MidiCliError::connection)?;
            listen(
                &endpoint,
                timeout,
                || session.poll_control_change(),
                &mut std::io::stdout().lock(),
            )
        }
        Command::Sync {
            endpoint,
            count,
            quiet,
            timeout,
        } => {
            let mut session =
                MidiSession::connect_native(&endpoint).map_err(MidiCliError::connection)?;
            sync(
                &endpoint,
                count,
                quiet,
                timeout,
                &mut session,
                &mut std::io::stdout().lock(),
            )
        }
        Command::Send { endpoint, message } => {
            let mut session =
                MidiSession::connect_native(&endpoint).map_err(MidiCliError::connection)?;
            session
                .send_control_change(message)
                .map_err(|e| MidiCliError::new("transport", e))?;
            println!(
                "{}",
                json!({"event":"sent", "endpoint":endpoint, "channel":message.channel, "controller":message.controller, "value":message.value})
            );
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn command_validation() {
        assert_eq!(
            parse(&args(&["listen", "port", "--timeout-ms", "25"])),
            Ok(Command::Listen {
                endpoint: "port".into(),
                timeout: Duration::from_millis(25)
            })
        );
        assert!(parse(&args(&["send", "port", "16", "1", "2"])).is_err());
        assert!(parse(&args(&["listen", "port", "--timeout-ms", "abc"])).is_err());
    }

    #[test]
    fn sync_drains_before_request_and_collects_all_replies() {
        use std::collections::VecDeque;
        struct Fake {
            queue: VecDeque<ControlChange>,
            sent: Vec<ControlChange>,
        }
        impl MidiIo for Fake {
            fn poll(&mut self) -> Result<Option<ControlChange>, String> {
                Ok(self.queue.pop_front())
            }
            fn send(&mut self, message: ControlChange) -> Result<(), String> {
                self.sent.push(message);
                self.queue.extend([
                    ControlChange {
                        channel: 0,
                        controller: 1,
                        value: 2,
                    },
                    ControlChange {
                        channel: 0,
                        controller: 3,
                        value: 4,
                    },
                ]);
                Ok(())
            }
        }
        let mut fake = Fake {
            queue: VecDeque::from([ControlChange {
                channel: 0,
                controller: 99,
                value: 99,
            }]),
            sent: vec![],
        };
        let mut output = Vec::new();
        sync(
            "port",
            2,
            Duration::from_millis(1),
            Duration::from_secs(1),
            &mut fake,
            &mut output,
        )
        .unwrap();
        assert_eq!(
            fake.sent,
            vec![ControlChange {
                channel: 0,
                controller: 127,
                value: 127
            }]
        );
        let events: Vec<serde_json::Value> = String::from_utf8(output)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(events.len(), 3);
        assert_eq!(events[0]["event"], "ready");
        assert_eq!(events[1]["controller"], 1);
        assert_eq!(events[2]["controller"], 3);
        assert!(parse(&args(&["sync", "port", "--count", "0"])).is_err());
        assert!(parse(&args(&["sync", "port"])).is_err());
    }

    #[test]
    fn sync_does_not_send_after_ready_flush_exceeds_deadline() {
        struct Client {
            sent: bool,
        }
        impl MidiIo for Client {
            fn poll(&mut self) -> Result<Option<ControlChange>, String> {
                Ok(None)
            }
            fn send(&mut self, _: ControlChange) -> Result<(), String> {
                self.sent = true;
                Ok(())
            }
        }
        struct SlowOutput;
        impl Write for SlowOutput {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                thread::sleep(Duration::from_millis(20));
                Ok(())
            }
        }
        let mut client = Client { sent: false };
        let error = sync(
            "port",
            1,
            Duration::ZERO,
            Duration::from_millis(5),
            &mut client,
            &mut SlowOutput,
        )
        .unwrap_err();
        assert_eq!(error.kind, "timeout");
        assert!(!client.sent);
    }

    #[test]
    fn quiet_boundary_polls_before_accepting_silence() {
        use std::cell::Cell;
        struct Client<'a> {
            now: &'a Cell<Instant>,
            arrival: Instant,
            delivered: bool,
        }
        impl MidiIo for Client<'_> {
            fn poll(&mut self) -> Result<Option<ControlChange>, String> {
                if !self.delivered && self.now.get() >= self.arrival {
                    self.delivered = true;
                    Ok(Some(ControlChange {
                        channel: 0,
                        controller: 1,
                        value: 2,
                    }))
                } else {
                    Ok(None)
                }
            }
            fn send(&mut self, _: ControlChange) -> Result<(), String> {
                Ok(())
            }
        }
        let start = Instant::now();
        let now = Cell::new(start);
        let mut client = Client {
            now: &now,
            arrival: start + Duration::from_millis(5),
            delivered: false,
        };
        wait_for_quiet(
            &mut client,
            Duration::from_millis(5),
            start + Duration::from_millis(100),
            || now.get(),
            |duration| now.set(now.get() + duration),
        )
        .unwrap();
        assert!(client.delivered);
        assert_eq!(now.get(), start + Duration::from_millis(10));
        assert!(parse(&args(&["sync", "port", "--count", "1", "--quiet-ms", "0"])).is_err());
    }

    #[test]
    fn listener_timeout_is_distinct() {
        let mut output = Vec::new();
        let error = listen("port", Duration::ZERO, || Ok(None), &mut output).unwrap_err();
        assert_eq!(error.kind, "timeout");
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&output).unwrap()["event"],
            "ready"
        );
    }

    #[test]
    fn listener_reports_ready_then_first_message() {
        let mut output = Vec::new();
        let mut next = Some(ControlChange {
            channel: 0,
            controller: 81,
            value: 42,
        });
        listen(
            "port",
            Duration::from_millis(50),
            || Ok(next.take()),
            &mut output,
        )
        .unwrap();
        let events: Vec<serde_json::Value> = String::from_utf8(output)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(events[0]["event"], "ready");
        assert_eq!(events[1]["event"], "control-change");
        assert_eq!(events[1]["controller"], 81);
        assert_eq!(events.len(), 2);
    }
}
