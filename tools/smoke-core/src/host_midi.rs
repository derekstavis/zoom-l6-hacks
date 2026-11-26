//! Scenario-specific helpers for host MIDI.
use std::time::Duration;

pub use l6max_host::host_midi::consumer::Client;

pub fn connect(endpoint: &str) -> Result<Box<dyn Client>, String> {
    Ok(Box::new(l6ctl::midi::MidiSession::connect_native(
        endpoint,
    )?))
}

pub fn wait_for(
    client: &mut dyn Client,
    timeout: Duration,
    mut idle: impl FnMut() -> Result<(), String>,
    mut accept: impl FnMut([u8; 3]) -> bool,
) -> Result<bool, String> {
    let deadline = std::time::Instant::now() + timeout;
    while std::time::Instant::now() < deadline {
        idle()?;
        if let Some(message) = client.poll()? {
            if accept(message) {
                return Ok(true);
            }
        } else {
            std::thread::sleep(Duration::from_millis(2));
        }
    }
    Ok(false)
}
