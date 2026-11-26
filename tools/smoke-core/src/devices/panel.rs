//! Panel inputs and outputs: PWM levels, key maps, input sweeps and timing.
use super::*;

/// Observe guest progress while retaining the caller's overall wall and guest budgets.
pub(super) fn wait_panel_guest_elapsed(
    start_guest: u32,
    duration_ms: u32,
    operation_guest: u32,
    guest_budget: u32,
    wall_deadline: Instant,
    mut sample: impl FnMut() -> Result<(u32, Instant), String>,
    mut pump: impl FnMut() -> Result<(), String>,
) -> Result<(), String> {
    loop {
        let (now, wall_now) = sample()?;
        if wall_now >= wall_deadline || now.wrapping_sub(operation_guest) > guest_budget {
            return Err("L6max display settle exceeded its guest or wall deadline".into());
        }
        if now.wrapping_sub(start_guest) >= duration_ms {
            return Ok(());
        }
        pump()?;
    }
}

#[cfg(test)]
mod panel_guest_wait_tests {
    use super::*;
    use std::{cell::Cell, time::Duration};
    #[test]
    fn slow_guest_keeps_full_1500ms_observation_past_ten_wall_seconds() {
        let base = Instant::now();
        let ticks = Cell::new(0u32);
        let result = wait_panel_guest_elapsed(
            100,
            1500,
            100,
            60_000,
            base + Duration::from_secs(120),
            || {
                Ok((
                    100 + ticks.get() * 100,
                    base + Duration::from_secs(u64::from(ticks.get())),
                ))
            },
            || {
                ticks.set(ticks.get() + 1);
                Ok(())
            },
        );
        assert!(result.is_ok());
        assert_eq!(ticks.get(), 15);
    }
    #[test]
    fn stalled_guest_expires_at_original_wall_deadline() {
        let base = Instant::now();
        let ticks = Cell::new(0u64);
        let result = wait_panel_guest_elapsed(
            100,
            1500,
            100,
            60_000,
            base + Duration::from_secs(12),
            || Ok((100, base + Duration::from_secs(ticks.get()))),
            || {
                ticks.set(ticks.get() + 1);
                Ok(())
            },
        );
        assert!(result.unwrap_err().contains("deadline"));
        assert_eq!(ticks.get(), 12);
    }
    #[test]
    fn guest_wrap_and_overall_guest_budget_are_checked() {
        let base = Instant::now();
        assert!(
            wait_panel_guest_elapsed(
                u32::MAX - 999,
                1500,
                u32::MAX - 999,
                2000,
                base + Duration::from_secs(20),
                || Ok((500, base)),
                || Ok(())
            )
            .is_ok()
        );
        assert!(
            wait_panel_guest_elapsed(
                0,
                1500,
                0,
                1200,
                base + Duration::from_secs(20),
                || Ok((1300, base)),
                || Ok(())
            )
            .is_err()
        );
    }
}
