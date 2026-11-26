//! L6 MAIN GPIO event transaction, retaining the debugger and pressed input.
use super::*;
const SUBMIT: u32 = firmware_api::l6::main_event_submit;
const HANDLER: u32 = firmware_api::l6::sound_pad_event_handler;
const GUARDS: [(u32, &str); 2] = [
    (
        SUBMIT,
        "aeac5eb8ff0f16e2d3ecc5d5cc41af4bc3334d08940d5fdb58cb267babf2744b",
    ),
    (
        HANDLER,
        "e1cfc7de7d14fe02ac408fa85dc2b76fe197d3f787261e687748ea4658a8aff5",
    ),
];
pub(super) struct Scope {
    inputs: l6ctl::InputObserver,
    debugger: Option<l6max_host::debug::Session>,
    target: u32,
    event: u8,
    panel_start: u32,
    guest: u32,
    deadline: Instant,
    held: Option<l6ctl::HeldButton>,
    args: Option<[u32; 4]>,
    asserted: bool,
    dispatched: bool,
    released: bool,
}
impl Scope {
    fn check(&self, session: &Session) -> Result<(), String> {
        if Instant::now() >= self.deadline
            || guest_now(session).wrapping_sub(self.panel_start) > self.guest
        {
            Err("GPIO event shared deadline exceeded".into())
        } else {
            Ok(())
        }
    }
    pub(super) fn begin(
        session: &Session,
        target: u32,
        event: u8,
        guest: u32,
        wall: u64,
    ) -> Result<Self, String> {
        if session.engine.model() != l6fw::Model::L6
            || !(48..=52).contains(&target)
            || !(21..=25).contains(&event)
        {
            return Err("unsupported L6 GPIO target/event".into());
        }
        let deadline = Instant::now()
            .checked_add(Duration::from_secs(wall))
            .ok_or("GPIO deadline overflow")?;
        let panel_start = guest_now(session);
        let ctl = l6ctl::OwnedControl::new(&session.engine);
        let inputs = ctl.input_observer().map_err(|_| "GPIO input unavailable")?;
        let mut debugger = ctl
            .debug_session_before(deadline)
            .map_err(|e| e.to_string())?;
        let setup = (|| -> Result<(), String> {
            for (address, expected) in GUARDS {
                if Instant::now() >= deadline
                    || guest_now(session).wrapping_sub(panel_start) > guest
                {
                    return Err("GPIO setup timed out".into());
                }
                let code = ctl
                    .read_main_memory_before(address, 64, deadline)
                    .map_err(|e| e.to_string())?;
                if format!("{:x}", Sha256::digest(&code.bytes)) != expected {
                    return Err(format!("GPIO code guard mismatch at {address:#x}"));
                }
                if Instant::now() >= deadline
                    || guest_now(session).wrapping_sub(panel_start) > guest
                {
                    return Err("GPIO setup timed out".into());
                }
            }
            debugger.breakpoint_before(SUBMIT, true, deadline)?;
            if Instant::now() >= deadline || guest_now(session).wrapping_sub(panel_start) > guest {
                return Err("GPIO setup timed out".into());
            }
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
            target,
            event,
            panel_start,
            guest,
            deadline,
            held: None,
            args: None,
            asserted: false,
            dispatched: false,
            released: false,
        })
    }
    fn ack(&self, session: &Session, seq: u32) -> Result<(), String> {
        let deadline = self.deadline.min(Instant::now() + Duration::from_secs(10));
        let ctl = l6ctl::OwnedControl::new(&session.engine);
        loop {
            self.check(session)?;
            if Instant::now() >= deadline {
                return Err("GPIO ACK timed out".into());
            }
            session.poll()?;
            self.check(session)?;
            if Instant::now() >= deadline {
                return Err("GPIO ACK timed out".into());
            }
            let ack = ctl
                .input_ack(l6ctl::InputSource::Main, seq)
                .map_err(|e| e.to_string())?;
            self.check(session)?;
            if Instant::now() >= deadline {
                return Err("GPIO ACK timed out".into());
            }
            match ack.state {
                l6ctl::InputAckState::Applied => return Ok(()),
                l6ctl::InputAckState::Pending => thread::sleep(Duration::from_millis(5)),
                _ => return Err("GPIO ACK rejected or unavailable".into()),
            }
        }
    }
    pub(super) fn press(&mut self, session: &Session) -> Result<Value, String> {
        self.check(session)?;
        let (r, held) = l6ctl::OwnedControl::new(&session.engine)
            .hold_button(self.target, 0)
            .map_err(|e| e.to_string())?;
        self.held = Some(held);
        self.ack(session, r.sequence)?;
        Ok(json!({"sequence":r.sequence}))
    }
    fn hit(&mut self, session: &Session, expected: u32, count: u32) -> Result<Vec<u32>, String> {
        self.check(session)?;
        let deadline = self.deadline.min(Instant::now() + Duration::from_secs(15));
        let inputs = self.inputs.clone();
        let start = self.panel_start;
        let guest = self.guest;
        self.debugger
            .as_mut()
            .ok_or("debugger missing")?
            .continue_before(deadline, || {
                Instant::now() >= deadline
                    || inputs
                        .status(l6ctl::InputSource::Panel)
                        .guest_ms
                        .wrapping_sub(start)
                        > guest
            })?;
        self.check(session)?;
        if Instant::now() >= deadline {
            return Err("GPIO continuation timed out".into());
        }
        let pc = self
            .debugger
            .as_mut()
            .unwrap()
            .register_before(15, self.deadline)?
            & !1;
        self.check(session)?;
        if pc != expected {
            return Err(format!("GPIO stopped at {pc:#x}, expected {expected:#x}"));
        }
        let mut args = Vec::new();
        for reg in 0..count {
            self.check(session)?;
            args.push(
                self.debugger
                    .as_mut()
                    .unwrap()
                    .register_before(reg, self.deadline)?,
            );
            self.check(session)?;
        }
        Ok(args)
    }
    pub(super) fn submit(&mut self, session: &Session) -> Result<Value, String> {
        let args = self.hit(session, SUBMIT, 4)?;
        self.args = Some(args.clone().try_into().unwrap());
        Ok(json!({"arguments":args}))
    }
    pub(super) fn assert(&mut self, session: &Session) -> Result<Value, String> {
        self.check(session)?;
        let args = self.args.ok_or("submit evidence missing")?;
        if args[2] != u32::from(self.event) {
            return Err(format!("GPIO event {} != {}", args[2], self.event));
        }
        self.asserted = true;
        Ok(json!({"event":self.event,"arguments":args}))
    }
    pub(super) fn dispatch(&mut self, session: &Session) -> Result<Value, String> {
        self.check(session)?;
        if !self.asserted || !(21..=24).contains(&self.event) {
            return Err("unexpected GPIO handler phase".into());
        }
        self.check(session)?;
        self.debugger
            .as_mut()
            .ok_or("debugger missing")?
            .breakpoint_before(SUBMIT, false, self.deadline)?;
        self.check(session)?;
        self.debugger
            .as_mut()
            .unwrap()
            .breakpoint_before(HANDLER, true, self.deadline)?;
        self.check(session)?;
        let args = self.hit(session, HANDLER, 3)?;
        self.check(session)?;
        self.debugger
            .as_mut()
            .unwrap()
            .breakpoint_before(HANDLER, false, self.deadline)?;
        self.check(session)?;
        self.dispatched = true;
        Ok(json!({"dispatch_arguments":args}))
    }
    pub(super) fn release(&mut self, session: &Session) -> Result<Value, String> {
        self.check(session)?;
        if !self.asserted || ((21..=24).contains(&self.event) && !self.dispatched) {
            return Err("GPIO observations incomplete".into());
        }
        if self.event == 25 {
            self.debugger
                .as_mut()
                .ok_or("debugger missing")?
                .breakpoint_before(SUBMIT, false, self.deadline)?;
        }
        self.check(session)?;
        self.debugger
            .as_mut()
            .unwrap()
            .close_resuming_before(self.deadline)?;
        self.debugger = None;
        self.check(session)?;
        let r = l6ctl::OwnedControl::new(&session.engine)
            .up(self.target)
            .map_err(|e| e.to_string())?;
        self.ack(session, r.sequence)?;
        self.held = None;
        self.released = true;
        Ok(json!({"sequence":r.sequence}))
    }
    pub(super) fn settle(&mut self, session: &Session) -> Result<Value, String> {
        if !self.released {
            return Err("GPIO release missing".into());
        }
        let start = guest_now(session);
        loop {
            self.check(session)?;
            if guest_now(session).wrapping_sub(start) >= 100 {
                return Ok(json!({"guest_ms":100}));
            }
            session.poll()?;
            thread::sleep(Duration::from_millis(5));
        }
    }
    pub(super) fn finish(&self, session: &Session) -> Result<Value, String> {
        self.check(session)?;
        if !self.released {
            return Err("GPIO release missing".into());
        }
        Ok(json!({"target":self.target,"event":self.event}))
    }
    pub(super) fn abort(&mut self) -> Result<(), String> {
        let up = self
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
        match (up, close) {
            (Ok(()), Ok(_)) => Ok(()),
            (Err(e), Ok(_)) | (Ok(()), Err(e)) => Err(e),
            (Err(e), Err(c)) => Err(format!("{e}; debugger cleanup: {c}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "requires L6_GPIO_STOCK_MAIN"]
    fn supported_stock_main_matches_both_guards() {
        let image = fs::read(std::env::var("L6_GPIO_STOCK_MAIN").unwrap()).unwrap();
        assert_eq!(
            format!("{:x}", Sha256::digest(&image)),
            "827c7c5cb2a43cc3a011d4556462b8cb9539242f817ebeed7727d66a36c03799"
        );
        for (address, expected) in GUARDS {
            let offset = (address - l6fw::Model::L6.main_base()) as usize;
            assert_eq!(
                format!("{:x}", Sha256::digest(&image[offset..offset + 64])),
                expected
            );
        }
    }
}
