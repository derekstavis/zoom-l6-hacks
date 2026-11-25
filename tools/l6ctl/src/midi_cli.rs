use l6ctl::midi::{ControlChange, MidiSession};
use serde_json::json;
use std::{
    io::Write,
    thread,
    time::{Duration, Instant},
};

const USAGE: &str = "Usage: l6ctl midi listen ENDPOINT [--timeout-ms MS] | l6ctl midi send ENDPOINT CHANNEL CONTROLLER VALUE";

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
