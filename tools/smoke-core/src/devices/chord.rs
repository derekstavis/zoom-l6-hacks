//! Atomic held chord, including cleanup of partial or failed holds.
use super::*;

fn release_held(
    held: &mut Vec<u32>,
    mut release: impl FnMut(u32) -> Result<(), String>,
) -> Result<(), String> {
    let mut failures = Vec::new();
    while let Some(target) = held.pop() {
        if let Err(error) = release(target) {
            failures.push(format!("target {target}: {error}"));
        }
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(format!("chord release failed: {}", failures.join("; ")))
    }
}

pub(super) fn hold(
    session: &Session,
    targets: Vec<u32>,
    duration_ms: Option<u32>,
    until: Option<crate::scenario::ChordCondition>,
    guest_timeout_ms: u32,
    wall_timeout_s: u64,
) -> Result<Value, Box<dyn std::error::Error>> {
    let control = l6ctl::OwnedControl::new(&session.engine);
    let start = control.input_status(l6ctl::InputSource::Panel)?.guest_ms;
    let factory = matches!(until, Some(crate::scenario::ChordCondition::FactoryMode));
    if factory && control.read_main_memory(l6max::system_mode, 4)?.bytes == 7u32.to_le_bytes() {
        return Err("factory mode was already active before the chord".into());
    }
    let hold = duration_ms.map_or(0, |duration| duration.saturating_add(1000).min(60_000));
    let mut held = Vec::new();
    let mut deadline = Instant::now();
    let result = (|| -> Result<(), Box<dyn std::error::Error>> {
        for target in &targets {
            control.down(*target, hold)?;
            held.push(*target);
        }
        deadline = Instant::now()
            + Duration::from_secs(if factory {
                wall_timeout_s.min(180)
            } else {
                wall_timeout_s
            });
        let guest_limit = if factory {
            guest_timeout_ms.min(10_000)
        } else {
            guest_timeout_ms
        };
        let mut next_mode_poll = Instant::now();
        let mut reached = false;
        loop {
            session.poll()?;
            let main = control.input_status(l6ctl::InputSource::Main)?;
            let panel = control.input_status(l6ctl::InputSource::Panel)?;
            if main.powered_off || panel.powered_off {
                return Err("guest powered off during held chord".into());
            }
            let elapsed = panel.guest_ms.wrapping_sub(start);
            if factory && !reached && Instant::now() >= next_mode_poll {
                reached = control
                    .read_main_memory_before(l6max::system_mode, 4, deadline)?
                    .bytes
                    == 7u32.to_le_bytes();
                next_mode_poll = Instant::now() + Duration::from_millis(10);
            }
            if main.applied >= main.sent
                && panel.applied >= panel.sent
                && (reached || duration_ms.is_some_and(|duration| elapsed >= duration))
            {
                return Ok(());
            }
            if Instant::now() > deadline || elapsed > guest_limit {
                return Err("held chord was not applied within its deadline".into());
            }
            thread::sleep(Duration::from_millis(5));
        }
    })();
    let release = release_held(&mut held, |target| {
        control.up(target).map(|_| ()).map_err(|e| e.to_string())
    });
    match (result, release) {
        (Err(error), Err(cleanup)) => return Err(format!("{error}; {cleanup}").into()),
        (Err(error), _) => return Err(error),
        (Ok(()), Err(cleanup)) => return Err(cleanup.into()),
        (Ok(()), Ok(())) => {}
    }
    loop {
        session.poll()?;
        let main = control.input_status(l6ctl::InputSource::Main)?;
        let panel = control.input_status(l6ctl::InputSource::Panel)?;
        if main.applied >= main.sent && panel.applied >= panel.sent {
            break;
        }
        if Instant::now() > deadline {
            return Err("held chord release was not acknowledged".into());
        }
        thread::sleep(Duration::from_millis(5));
    }
    Ok(
        json!({"targets":targets,"held_guest_ms":control.input_status(l6ctl::InputSource::Panel)?.guest_ms.wrapping_sub(start)}),
    )
}

#[cfg(test)]
mod tests {
    use super::release_held;
    #[test]
    fn releases_partial_chord_in_reverse_even_if_one_release_fails() {
        let mut held = vec![1, 2, 3];
        let mut attempted = Vec::new();
        let error = release_held(&mut held, |target| {
            attempted.push(target);
            if target == 2 {
                Err("disconnected".into())
            } else {
                Ok(())
            }
        })
        .unwrap_err();
        assert_eq!(attempted, [3, 2, 1]);
        assert!(held.is_empty());
        assert!(error.contains("target 2: disconnected"));
    }
}
