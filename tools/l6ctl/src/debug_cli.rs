//! Interactive JSONL protocol for one scoped debugger lease.
use l6ctl::{ControlClient, DebugCommand};
use serde_json::json;
use std::io::{self, BufRead, Read, Write};

pub fn run(client: &ControlClient) -> Result<(), String> {
    let mut session = client.debug_session().map_err(|error| error.to_string())?;
    let stdin = io::stdin();
    let mut input = stdin.lock();
    let stdout = io::stdout();
    let mut output = stdout.lock();
    loop {
        let mut line = String::new();
        let bytes = (&mut input)
            .take(4097)
            .read_line(&mut line)
            .map_err(|error| error.to_string())?;
        let command = if bytes == 0 {
            DebugCommand::Close
        } else {
            if bytes > 4096 || !line.ends_with('\n') {
                return Err("debug request must be one JSON line of at most 4096 bytes".into());
            }
            serde_json::from_str::<DebugCommand>(&line).map_err(|error| error.to_string())?
        };
        let close = matches!(
            command,
            DebugCommand::Close
                | DebugCommand::L6MidiFinish { .. }
                | DebugCommand::L6DinFinish { .. }
                | DebugCommand::L6MeterFinish { .. }
        );
        match session.execute(command) {
            Ok(result) => writeln!(output, "{}", json!({"ok":true,"result":result}))
                .map_err(|error| error.to_string())?,
            Err(error) => {
                writeln!(output, "{}", json!({"ok":false,"error":error.to_string()}))
                    .map_err(|error| error.to_string())?;
                output.flush().map_err(|error| error.to_string())?;
                return Err("debug session failed".into());
            }
        }
        output.flush().map_err(|error| error.to_string())?;
        if close {
            return Ok(());
        }
    }
}
