//! L6max TAP handler observation with an owned debugger and held input.
use super::*;

const TARGET: u32 = 48;
const ADDRESS: u32 = firmware_api::l6max::l6max_tap_press_handler;
const GUARD: &str = "987c6f18635be467650c37677f2cfea39b534454a7e2941b556ce987443b3262";

fn main_deadline_expired(start: u32, now: u32) -> bool {
    now.wrapping_sub(start) > 5000
}
fn acknowledgement_applied(state: l6ctl::InputAckState, expired: bool) -> Result<bool, String> {
    if expired {
        return Err("TAP acknowledgement timed out".into());
    }
    match state {
        l6ctl::InputAckState::Applied => Ok(true),
        l6ctl::InputAckState::Rejected => Err("TAP input rejected".into()),
        l6ctl::InputAckState::Unknown => Err("TAP input acknowledgement unavailable".into()),
        l6ctl::InputAckState::Pending => Ok(false),
    }
}

pub(super) struct Scope {
    inputs: l6ctl::InputObserver,
    debugger: Option<l6max_host::debug::Session>,
    started: Instant,
    panel_start: u32,
    guest: u32,
    deadline: Instant,
    held: Option<l6ctl::HeldButton>,
    pc: Option<u32>,
    asserted: bool,
    released: bool,
}
impl Scope {
    fn check(&self, session: &Session) -> Result<(), String> {
        if Instant::now() >= self.deadline
            || guest_now(session).wrapping_sub(self.panel_start) > self.guest
        {
            return Err("TAP handler shared deadline exceeded".into());
        }
        Ok(())
    }
    pub(super) fn begin(session: &Session, guest: u32, wall: u64) -> Result<Self, String> {
        if session.model() != l6fw::Model::L6max {
            return Err("TAP handler requires L6max".into());
        }
        let started = Instant::now();
        let deadline = started
            .checked_add(Duration::from_secs(wall))
            .ok_or("TAP deadline overflow")?;
        let panel_start = guest_now(session);
        let control = session.control();
        let inputs = control
            .input_observer()
            .map_err(|_| "TAP input unavailable")?;
        let mut debugger = control
            .debug_session_before(deadline)
            .map_err(|e| e.to_string())?;
        let setup = (|| -> Result<(), String> {
            let code = control
                .read_main_memory_before(ADDRESS, 64, deadline)
                .map_err(|e| e.to_string())?;
            if format!("{:x}", Sha256::digest(&code.bytes)) != GUARD {
                return Err("TAP handler does not match supported stock firmware".into());
            }
            if Instant::now() >= deadline || guest_now(session).wrapping_sub(panel_start) > guest {
                return Err("TAP handler setup timed out".into());
            }
            debugger.breakpoint_before(ADDRESS, true, deadline)?;
            Ok(())
        })();
        if let Err(error) = setup {
            return match debugger.close_before(Instant::now() + Duration::from_secs(3)) {
                Ok(()) => Err(error),
                Err(cleanup) => Err(format!("{error}; debugger cleanup: {cleanup}")),
            };
        }
        Ok(Self {
            inputs,
            debugger: Some(debugger),
            started,
            panel_start,
            guest,
            deadline,
            held: None,
            pc: None,
            asserted: false,
            released: false,
        })
    }
    fn ack(&self, session: &Session, sequence: u32) -> Result<(), String> {
        let deadline = (Instant::now() + Duration::from_secs(10)).min(self.deadline);
        let control = session.control();
        loop {
            self.check(session)?;
            if Instant::now() >= deadline {
                return Err("TAP acknowledgement timed out".into());
            }
            session.poll()?;
            self.check(session)?;
            if Instant::now() >= deadline {
                return Err("TAP acknowledgement timed out".into());
            }
            let ack = control
                .input_ack(l6ctl::InputSource::Main, sequence)
                .map_err(|e| e.to_string())?;
            self.check(session)?;
            if Instant::now() >= deadline {
                return Err("TAP acknowledgement timed out".into());
            }
            if acknowledgement_applied(ack.state, Instant::now() >= deadline)? {
                return Ok(());
            }
            thread::sleep(Duration::from_millis(5));
        }
    }
    pub(super) fn press(&mut self, session: &Session) -> Result<Value, String> {
        self.check(session)?;
        let (receipt, held) = session
            .control()
            .hold_button(TARGET, 0)
            .map_err(|e| e.to_string())?;
        self.held = Some(held);
        self.ack(session, receipt.sequence)?;
        Ok(json!({"target":TARGET,"sequence":receipt.sequence}))
    }
    pub(super) fn wait(&mut self, session: &Session) -> Result<Value, String> {
        self.check(session)?;
        let clock = self.inputs.status(l6ctl::InputSource::Main);
        if !clock.connected || clock.clock_updates == 0 {
            return Err("MAIN clock unavailable".into());
        }
        let start = clock.guest_ms;
        let panel_start = self.panel_start;
        let guest = self.guest;
        let deadline = self.deadline;
        let inputs = self.inputs.clone();
        self.debugger
            .as_mut()
            .ok_or("debugger missing")?
            .continue_before(deadline, || {
                let main = inputs.status(l6ctl::InputSource::Main);
                Instant::now() >= deadline
                    || main_deadline_expired(start, main.guest_ms)
                    || inputs
                        .status(l6ctl::InputSource::Panel)
                        .guest_ms
                        .wrapping_sub(panel_start)
                        > guest
            })?;
        self.check(session)?;
        let main = self.inputs.status(l6ctl::InputSource::Main);
        if main_deadline_expired(start, main.guest_ms) {
            return Err("TAP MAIN clock exceeded deadline".into());
        }
        let pc = self
            .debugger
            .as_mut()
            .ok_or("debugger missing")?
            .register_before(15, self.deadline)?
            & !1;
        self.check(session)?;
        self.pc = Some(pc);
        Ok(json!({"pc":pc}))
    }
    pub(super) fn assert(&mut self, session: &Session) -> Result<Value, String> {
        self.check(session)?;
        if self.pc != Some(ADDRESS) {
            return Err(format!(
                "TAP handler stopped at {:?}, expected {ADDRESS:#x}",
                self.pc
            ));
        }
        self.asserted = true;
        Ok(json!({"handler":ADDRESS}))
    }
    pub(super) fn release(&mut self, session: &Session) -> Result<Value, String> {
        self.check(session)?;
        if !self.asserted {
            return Err("TAP handler not asserted".into());
        }
        let debugger = self.debugger.as_mut().ok_or("debugger missing")?;
        debugger.breakpoint_before(ADDRESS, false, self.deadline)?;
        self.check(session)?;
        self.debugger
            .as_mut()
            .unwrap()
            .close_before(self.deadline)?;
        self.debugger = None;
        self.check(session)?;
        let receipt = session.control().up(TARGET).map_err(|e| e.to_string())?;
        self.ack(session, receipt.sequence)?;
        self.held = None;
        self.released = true;
        Ok(json!({"sequence":receipt.sequence}))
    }
    pub(super) fn finish(&self, session: &Session) -> Result<Value, String> {
        self.check(session)?;
        if !self.released {
            return Err("TAP release not acknowledged".into());
        }
        Ok(json!({"target":TARGET,"handler":ADDRESS,"wall_ms":self.started.elapsed().as_millis()}))
    }
    pub(super) fn abort(&mut self) -> Result<(), String> {
        let release = self
            .held
            .as_mut()
            .map(|held| held.release().map_err(|e| e.to_string()))
            .transpose()
            .map(|_| ());
        let close = self
            .debugger
            .as_mut()
            .map(|d| d.close_before(Instant::now() + Duration::from_secs(3)))
            .transpose();
        match (release, close) {
            (Ok(()), Ok(_)) => Ok(()),
            (Err(e), Ok(_)) | (Ok(()), Err(e)) => Err(e),
            (Err(e), Err(c)) => Err(format!(
                "TAP release failed: {e}; debugger cleanup failed: {c}"
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn immediate_hit_and_wrapped_main_clock_keep_original_budget() {
        assert!(!main_deadline_expired(100, 100));
        assert!(!main_deadline_expired(u32::MAX - 50, 4949));
        assert!(main_deadline_expired(u32::MAX - 50, 4950));
    }
    #[test]
    fn late_or_rejected_release_does_not_complete_acknowledgement() {
        use l6ctl::InputAckState::*;
        assert!(acknowledgement_applied(Applied, true).is_err());
        assert!(acknowledgement_applied(Rejected, false).is_err());
        assert!(acknowledgement_applied(Unknown, false).is_err());
        assert!(!acknowledgement_applied(Pending, false).unwrap());
        assert!(acknowledgement_applied(Applied, false).unwrap());
    }
}
