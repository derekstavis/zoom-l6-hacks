//! Explicit power-button transaction using host-owned input receipts.
use super::*;

pub(super) struct Scope {
    sequence: u32,
    deadline: Instant,
    hold_ms: u32,
    powered: bool,
    held: Option<l6ctl::HeldButton>,
    applied_main_ms: Option<u32>,
    panel_at_ack_ms: Option<u32>,
    complete: bool,
}
impl Scope {
    pub(super) fn release(&mut self) -> Result<(), String> {
        if let Some(held) = &mut self.held {
            held.release_unless_main_powered_off()
                .map_err(|e| format!("power button release failed: {e}"))?;
        }
        self.held = None;
        Ok(())
    }
    fn check(&self) -> Result<(), String> {
        if Instant::now() >= self.deadline {
            Err("power transition timed out".into())
        } else {
            Ok(())
        }
    }
}

fn cutoff(start: u32, off: u32, minimum: Option<u32>, maximum: Option<u32>) -> Result<u32, String> {
    let elapsed = off.wrapping_sub(start);
    if minimum.is_some_and(|v| elapsed < v) || maximum.is_some_and(|v| elapsed > v) {
        Err(format!("power cutoff took {elapsed} guest ms"))
    } else {
        Ok(elapsed)
    }
}

fn observe_on(
    powered_off: bool,
    main_connected: bool,
    panel_connected: bool,
    panel_start: u32,
    panel_now: u32,
    required_ms: u32,
    guest_budget_ms: u32,
) -> Result<bool, String> {
    if powered_off {
        return Err("short power press cut power".into());
    }
    if !main_connected || !panel_connected {
        return Err("input disconnected during powered observation".into());
    }
    let elapsed = panel_now.wrapping_sub(panel_start);
    if elapsed >= required_ms {
        return Ok(true);
    }
    if elapsed > guest_budget_ms {
        return Err("power transition timed out".into());
    }
    Ok(false)
}

impl State {
    pub(super) fn run_power(
        &mut self,
        session: &Session,
        operation: Operation,
        guest_timeout_ms: u32,
        wall_timeout_s: u64,
    ) -> Result<Value, Box<dyn std::error::Error>> {
        let control = l6ctl::OwnedControl::new(&session.engine);
        match operation {
            Operation::PowerBegin { hold_ms, powered } => {
                if self.power.is_some() {
                    return Err("power transaction already active".into());
                }
                let deadline = Instant::now()
                    .checked_add(Duration::from_secs(wall_timeout_s))
                    .ok_or("power deadline overflow")?;
                let (receipt, held) = if powered {
                    (control.tap(53, hold_ms)?, None)
                } else {
                    let (receipt, held) = control.hold_button(53, hold_ms)?;
                    (receipt, Some(held))
                };
                self.power = Some(Scope {
                    sequence: receipt.sequence,
                    deadline,
                    hold_ms,
                    powered,
                    held,
                    applied_main_ms: None,
                    panel_at_ack_ms: None,
                    complete: false,
                });
                Ok(json!({"sequence":receipt.sequence,"hold_ms":hold_ms,"powered":powered}))
            }
            Operation::PowerWaitAck => {
                let scope = self.power.as_mut().ok_or("missing power transaction")?;
                loop {
                    scope.check()?;
                    session.poll()?;
                    scope.check()?;
                    let ack = control.input_ack(l6ctl::InputSource::Main, scope.sequence)?;
                    match ack.state {
                        l6ctl::InputAckState::Applied => {
                            let applied = ack
                                .applied_guest_ms
                                .ok_or("power acknowledgement has no guest timestamp")?;
                            let panel = control.input_status(l6ctl::InputSource::Panel)?;
                            if !panel.connected || panel.clock_updates == 0 {
                                return Err(
                                    "panel clock unavailable at power acknowledgement".into()
                                );
                            }
                            scope.applied_main_ms = Some(applied);
                            scope.panel_at_ack_ms = Some(panel.guest_ms);
                            return Ok(
                                json!({"main_applied_guest_ms":applied,"panel_guest_ms_at_ack":panel.guest_ms}),
                            );
                        }
                        l6ctl::InputAckState::Rejected => {
                            return Err("power press was rejected by the emulator".into());
                        }
                        l6ctl::InputAckState::Unknown => {
                            return Err("power input acknowledgement unavailable".into());
                        }
                        l6ctl::InputAckState::Pending => {}
                    }
                    thread::sleep(Duration::from_millis(5));
                }
            }
            Operation::PowerObserveOn { guest_ms } => {
                let scope = self.power.as_mut().ok_or("missing power transaction")?;
                if !scope.powered || guest_ms != scope.hold_ms.saturating_add(1000) {
                    return Err("invalid powered observation".into());
                }
                let start = scope
                    .panel_at_ack_ms
                    .ok_or("power press not acknowledged")?;
                loop {
                    scope.check()?;
                    session.poll()?;
                    scope.check()?;
                    let main = control.input_status(l6ctl::InputSource::Main)?;
                    let panel = control.input_status(l6ctl::InputSource::Panel)?;
                    if observe_on(
                        main.powered_off,
                        main.connected,
                        panel.connected,
                        start,
                        panel.guest_ms,
                        guest_ms,
                        guest_timeout_ms,
                    )? {
                        let elapsed = panel.guest_ms.wrapping_sub(start);
                        scope.complete = true;
                        return Ok(json!({"powered":true,"panel_guest_ms":elapsed}));
                    }
                    thread::sleep(Duration::from_millis(5));
                }
            }
            Operation::PowerWaitOff {
                min_cutoff_ms,
                max_cutoff_ms,
            } => {
                let scope = self.power.as_mut().ok_or("missing power transaction")?;
                if scope.powered {
                    return Err("power-off observation requires held input".into());
                }
                let start = scope
                    .applied_main_ms
                    .ok_or("power press not acknowledged")?;
                loop {
                    scope.check()?;
                    session.poll()?;
                    scope.check()?;
                    let main = control.input_status(l6ctl::InputSource::Main)?;
                    if main.powered_off {
                        scope.held = None;
                        let off = main
                            .powered_off_guest_ms
                            .ok_or("power-off timestamp unavailable")?;
                        let elapsed = cutoff(start, off, min_cutoff_ms, max_cutoff_ms)?;
                        scope.check()?;
                        let status = control.qmp_status_before(scope.deadline)?;
                        scope.check()?;
                        if status.status != "shutdown" {
                            return Err("power-off did not shut down QEMU".into());
                        }
                        return Ok(
                            json!({"powered":false,"held_guest_ms":elapsed,"qmp_status":status.status}),
                        );
                    }
                    if !main.connected {
                        return Err("MAIN disconnected before power-off".into());
                    }
                    thread::sleep(Duration::from_millis(5));
                }
            }
            Operation::PowerObserveStopped {
                settle_wall_ms,
                observe_wall_ms,
            } => {
                let scope = self.power.as_mut().ok_or("missing power transaction")?;
                if scope.powered || scope.held.is_some() {
                    return Err("clock observation requires power-off".into());
                }
                scope.check()?;
                thread::sleep(Duration::from_millis(settle_wall_ms));
                scope.check()?;
                let before = control.input_status(l6ctl::InputSource::Panel)?.guest_ms;
                thread::sleep(Duration::from_millis(observe_wall_ms));
                scope.check()?;
                let after = control.input_status(l6ctl::InputSource::Panel)?.guest_ms;
                if before != after {
                    return Err("guest clock advanced while powered off".into());
                }
                scope.complete = true;
                Ok(json!({"clock_stopped":true,"panel_guest_ms":after}))
            }
            Operation::PowerEnd => {
                let mut scope = self.power.take().ok_or("missing power transaction")?;
                if !scope.complete {
                    self.power = Some(scope);
                    return Err("power transaction is incomplete".into());
                }
                scope.release()?;
                Ok(json!({"power_transaction":"complete"}))
            }
            _ => Err("invalid power operation".into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::cutoff;
    #[test]
    fn powered_observation_uses_panel_elapsed_and_rejects_power_loss() {
        assert!(!super::observe_on(false, true, true, u32::MAX - 99, 500, 1100, 5000).unwrap());
        assert!(super::observe_on(false, true, true, u32::MAX - 99, 1000, 1100, 5000).unwrap());
        assert!(super::observe_on(true, true, true, 0, 1100, 1100, 5000).is_err());
        assert!(super::observe_on(false, false, true, 0, 1, 1100, 5000).is_err());
    }

    #[test]
    fn cutoff_checks_main_clock_wrap_and_exact_bounds() {
        assert_eq!(
            cutoff(u32::MAX - 99, 400, Some(300), Some(800)).unwrap(),
            500
        );
        assert!(cutoff(0, 299, Some(300), Some(800)).is_err());
        assert!(cutoff(0, 801, Some(300), Some(800)).is_err());
        assert!(cutoff(0, 300, Some(300), Some(800)).is_ok());
        assert!(cutoff(0, 800, Some(300), Some(800)).is_ok());
    }
}
