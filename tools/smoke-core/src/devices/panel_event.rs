//! L6 panel event observation with simultaneous dispatcher and submit breakpoints.
use super::*;
const DISPATCH: u32 = l6::panel_event_dispatcher;
const SUBMIT: u32 = l6::main_event_submit;
const GUARDS: [(u32, &str); 2] = [
    (
        DISPATCH,
        "973c4167670544ceb2cbd322ebe633dc3a0fedabbeac7631e93cafe77a0f0d8e",
    ),
    (
        SUBMIT,
        "aeac5eb8ff0f16e2d3ecc5d5cc41af4bc3334d08940d5fdb58cb267babf2744b",
    ),
];
fn cancel_shared(
    session: &Session,
    started: Instant,
    guest_start: u32,
    guest: u32,
    wall: u64,
) -> bool {
    started.elapsed() > Duration::from_secs(wall)
        || guest_now(session).wrapping_sub(guest_start) > guest
}
fn settled(elapsed: u32, expired: bool) -> Result<bool, String> {
    if expired {
        Err("panel event release timed out".into())
    } else {
        Ok(elapsed >= 250)
    }
}
fn button_report(bytes: &[u8]) -> Result<bool, String> {
    if bytes.len() != 3 {
        return Err("short panel report".into());
    }
    Ok(matches!(bytes[0], 0x90 | 0x91))
}
#[derive(Clone, Debug)]
struct Evidence {
    report: Vec<u8>,
    state: u32,
    event: u32,
    skipped: u32,
}
pub(super) struct Scope {
    debugger: Option<l6max_host::debug::Session>,
    inputs: l6ctl::InputObserver,
    button: l6ctl::RawHeldButton,
    target: u32,
    column: u8,
    row: u8,
    event: u8,
    dispatcher: bool,
    evidence: Option<Evidence>,
    report_ok: bool,
    mapping_ok: bool,
    released: bool,
    capture_deadline: Option<Instant>,
    release_start: u32,
    release_deadline: Option<Instant>,
    started: Instant,
    guest_start: u32,
    guest_budget: u32,
    wall_budget: u64,
}
impl Scope {
    fn check(&self, session: &Session) -> Result<(), String> {
        if self.started.elapsed() > Duration::from_secs(self.wall_budget)
            || guest_now(session).wrapping_sub(self.guest_start) > self.guest_budget
        {
            Err("panel event shared deadline exceeded".into())
        } else {
            Ok(())
        }
    }
    pub(super) fn begin(
        session: &Session,
        target: u32,
        column: u8,
        row: u8,
        event: u8,
        guest: u32,
        wall: u64,
    ) -> Result<Self, String> {
        if session.engine.model() != l6fw::Model::L6
            || target >= 54
            || column > 3
            || row > 7
            || event > 31
        {
            return Err("invalid L6 panel event".into());
        }
        let ctl = l6ctl::OwnedControl::new(&session.engine);
        let inputs = ctl
            .input_observer()
            .map_err(|_| "panel input unavailable")?;
        let button = ctl
            .raw_button(l6ctl::InputSource::Panel, target)
            .map_err(|_| "panel input unavailable")?;
        let started = Instant::now();
        let guest_start = guest_now(session);
        let deadline = started
            .checked_add(Duration::from_secs(wall))
            .ok_or("panel event deadline overflow")?;
        let mut debugger = ctl
            .debug_session_before(deadline)
            .map_err(|e| e.to_string())?;
        let setup = (|| -> Result<(), String> {
            for (address, expected) in GUARDS {
                if cancel_shared(session, started, guest_start, guest, wall) {
                    return Err("panel event setup timed out".into());
                }
                let bytes = ctl
                    .read_main_memory_before(address, 64, deadline)
                    .map_err(|e| e.to_string())?
                    .bytes;
                if format!("{:x}", Sha256::digest(bytes)) != expected {
                    return Err(format!("panel event code guard mismatch at {address:#x}"));
                }
                if cancel_shared(session, started, guest_start, guest, wall) {
                    return Err("panel event setup timed out".into());
                }
            }
            debugger.breakpoint_before(DISPATCH, true, deadline)?;
            if cancel_shared(session, started, guest_start, guest, wall) {
                return Err("panel event setup timed out".into());
            }
            debugger.breakpoint_before(SUBMIT, true, deadline)?;
            if cancel_shared(session, started, guest_start, guest, wall) {
                return Err("panel event setup timed out".into());
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
            debugger: Some(debugger),
            inputs,
            button,
            target,
            column,
            row,
            event,
            dispatcher: true,
            evidence: None,
            report_ok: false,
            mapping_ok: false,
            released: false,
            capture_deadline: None,
            release_start: 0,
            release_deadline: None,
            started,
            guest_start,
            guest_budget: guest,
            wall_budget: wall,
        })
    }
    pub(super) fn press(&mut self, session: &Session) -> Result<Value, String> {
        self.check(session)?;
        // A failed write may have delivered a partial message; attempt release
        // on every failure once a press has been attempted.
        let receipt = self.button.press(0).map_err(|e| e.to_string())?;
        self.capture_deadline = Some(Instant::now() + Duration::from_secs(15));
        self.check(session)?;
        Ok(json!({"target":self.target,"sequence":receipt}))
    }
    pub(super) fn capture(&mut self, session: &Session) -> Result<Value, String> {
        if !self.button.is_held() || self.evidence.is_some() {
            return Err("panel event capture out of order".into());
        }
        let deadline = self
            .capture_deadline
            .ok_or("panel press missing")?
            .min(self.started + Duration::from_secs(self.wall_budget));
        let ctl = l6ctl::OwnedControl::new(&session.engine);
        self.check(session)?;
        let debug = self
            .debugger
            .as_mut()
            .ok_or("panel event debugger missing")?;
        let mut skipped = 0u32;
        loop {
            if cancel_shared(
                session,
                self.started,
                self.guest_start,
                self.guest_budget,
                self.wall_budget,
            ) {
                return Err("panel event shared deadline exceeded".into());
            }
            if Instant::now() >= deadline {
                return Err(format!(
                    "panel target {} did not submit a button report after {skipped} unrelated submissions",
                    self.target
                ));
            }
            debug.continue_before(deadline, || {
                Instant::now() >= deadline
                    || cancel_shared(
                        session,
                        self.started,
                        self.guest_start,
                        self.guest_budget,
                        self.wall_budget,
                    )
            })?;
            if Instant::now() >= deadline
                || cancel_shared(
                    session,
                    self.started,
                    self.guest_start,
                    self.guest_budget,
                    self.wall_budget,
                )
            {
                return Err("panel event capture timed out".into());
            }
            let pc = debug.register_before(15, deadline)? & !1;
            if cancel_shared(
                session,
                self.started,
                self.guest_start,
                self.guest_budget,
                self.wall_budget,
            ) {
                return Err("panel event shared deadline exceeded".into());
            }
            if pc == DISPATCH {
                debug.breakpoint_before(DISPATCH, false, deadline)?;
                self.dispatcher = false;
                continue;
            }
            if pc != SUBMIT {
                return Err(format!(
                    "panel target {} stopped at {pc:#x}, not dispatcher or event submit",
                    self.target
                ));
            }
            let report = ctl
                .read_main_memory_before(l6::panel_report, 3, deadline)
                .map_err(|e| e.to_string())?
                .bytes;
            if cancel_shared(
                session,
                self.started,
                self.guest_start,
                self.guest_budget,
                self.wall_budget,
            ) {
                return Err("panel event shared deadline exceeded".into());
            }
            if !button_report(&report)? {
                skipped = skipped
                    .checked_add(1)
                    .ok_or("panel report count overflow")?;
                debug.breakpoint_before(SUBMIT, false, deadline)?;
                if cancel_shared(
                    session,
                    self.started,
                    self.guest_start,
                    self.guest_budget,
                    self.wall_budget,
                ) {
                    return Err("panel event shared deadline exceeded".into());
                }
                debug.step_before(deadline, || {
                    Instant::now() >= deadline
                        || cancel_shared(
                            session,
                            self.started,
                            self.guest_start,
                            self.guest_budget,
                            self.wall_budget,
                        )
                })?;
                if cancel_shared(
                    session,
                    self.started,
                    self.guest_start,
                    self.guest_budget,
                    self.wall_budget,
                ) {
                    return Err("panel event shared deadline exceeded".into());
                }
                debug.breakpoint_before(SUBMIT, true, deadline)?;
                continue;
            }
            let state = debug.register_before(1, deadline)?;
            let event = debug.register_before(2, deadline)?;
            if Instant::now() >= deadline
                || cancel_shared(
                    session,
                    self.started,
                    self.guest_start,
                    self.guest_budget,
                    self.wall_budget,
                )
            {
                return Err("panel event capture timed out".into());
            }
            self.evidence = Some(Evidence {
                report: report.clone(),
                state,
                event,
                skipped,
            });
            return Ok(
                json!({"target":self.target,"report":report,"state":state,"event":event,"skipped_reports":skipped}),
            );
        }
    }
    pub(super) fn assert_report(&mut self, session: &Session) -> Result<Value, String> {
        self.check(session)?;
        let evidence = self.evidence.as_ref().ok_or("panel report missing")?;
        if (evidence.report[1], evidence.report[2]) != (self.column, self.row) {
            return Err(format!(
                "panel target {} report {:02x?}, expected ({},{})",
                self.target, evidence.report, self.column, self.row
            ));
        }
        if evidence.state != 0 || evidence.event != u32::from(self.event) {
            return Err(format!(
                "panel target {} submitted state {} event {}, expected press-state 0 event {}",
                self.target, evidence.state, evidence.event, self.event
            ));
        }
        self.report_ok = true;
        Ok(
            json!({"target":self.target,"column":self.column,"row":self.row,"event":self.event,"report":evidence.report,"skipped_reports":evidence.skipped}),
        )
    }
    pub(super) fn assert_mapping(&mut self, session: &Session, wall: u64) -> Result<Value, String> {
        self.check(session)?;
        let entry = if self.column == 0 {
            l6::PANEL_COLUMN0_EVENT_TABLE + u32::from(self.row) * 0x14
        } else {
            l6::PANEL_EVENT_TABLE + u32::from(self.column) * 0xa0 + u32::from(self.row) * 0x14
        };
        let bytes = l6ctl::OwnedControl::new(&session.engine)
            .read_main_memory_before(
                entry + 8,
                4,
                (Instant::now() + Duration::from_secs(wall))
                    .min(self.started + Duration::from_secs(self.wall_budget)),
            )
            .map_err(|e| e.to_string())?
            .bytes;
        let value = u32::from_le_bytes(bytes.try_into().map_err(|_| "short panel map")?);
        self.check(session)?;
        if value != u32::from(self.event) {
            return Err(format!(
                "MAIN map ({},{}) is {value}, expected {}",
                self.column, self.row, self.event
            ));
        }
        self.mapping_ok = true;
        Ok(json!({"mapped_event":value}))
    }
    pub(super) fn release(&mut self, session: &Session, wall: u64) -> Result<Value, String> {
        self.check(session)?;
        if !self.report_ok || !self.mapping_ok {
            return Err("panel event assertions missing".into());
        }
        let deadline = (Instant::now() + Duration::from_secs(wall))
            .min(self.started + Duration::from_secs(self.wall_budget));
        let debug = self
            .debugger
            .as_mut()
            .ok_or("panel event debugger missing")?;
        if self.dispatcher {
            debug.breakpoint_before(DISPATCH, false, deadline)?;
            self.dispatcher = false;
            if cancel_shared(
                session,
                self.started,
                self.guest_start,
                self.guest_budget,
                self.wall_budget,
            ) {
                return Err("panel event shared deadline exceeded".into());
            }
        }
        debug.breakpoint_before(SUBMIT, false, deadline)?;
        if cancel_shared(
            session,
            self.started,
            self.guest_start,
            self.guest_budget,
            self.wall_budget,
        ) {
            return Err("panel event shared deadline exceeded".into());
        }
        debug.close_resuming_before(deadline)?;
        self.debugger = None;
        self.check(session)?;
        let receipt = self
            .button
            .release()
            .map_err(|e| e.to_string())?
            .ok_or("panel button was not held")?;
        self.released = true;
        self.release_start = self.inputs.status(l6ctl::InputSource::Panel).guest_ms;
        self.release_deadline = Some(Instant::now() + Duration::from_secs(wall));
        self.check(session)?;
        Ok(json!({"sequence":receipt}))
    }
    pub(super) fn settle(&self, session: &Session) -> Result<Value, String> {
        let deadline = self.release_deadline.ok_or("panel release missing")?;
        loop {
            session.poll()?;
            self.check(session)?;
            let elapsed = self
                .inputs
                .status(l6ctl::InputSource::Panel)
                .guest_ms
                .wrapping_sub(self.release_start);
            if settled(elapsed, Instant::now() > deadline)? {
                return Ok(json!({"panel_guest_ms":250}));
            }
            if Instant::now() > deadline {
                return Err("panel event release timed out".into());
            }
            thread::sleep(Duration::from_millis(5));
        }
    }
    pub(super) fn finish(&self, session: &Session) -> Result<Value, String> {
        self.check(session)?;
        if !self.released {
            return Err("panel event release missing".into());
        }
        Ok(json!({"target":self.target,"column":self.column,"row":self.row,"event":self.event}))
    }
    pub(super) fn abort(&mut self) -> Result<(), String> {
        let up = self.button.release().map(|_| ()).map_err(|e| e.to_string());
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
    fn release_window_rejects_completion_after_deadline() {
        assert!(!settled(249, false).unwrap());
        assert!(settled(250, false).unwrap());
        assert!(settled(250, true).is_err());
    }
    #[test]
    fn both_button_report_types_are_accepted_other_reports_are_skipped() {
        assert!(button_report(&[0x90, 1, 2]).unwrap());
        assert!(button_report(&[0x91, 1, 2]).unwrap());
        assert!(!button_report(&[0x80, 1, 2]).unwrap());
        assert!(button_report(&[0x90, 1]).is_err());
    }
    #[test]
    #[ignore = "requires L6_PANEL_EVENT_STOCK_MAIN"]
    fn stock_l6_matches_dispatch_and_submit_guards() {
        let image = fs::read(std::env::var("L6_PANEL_EVENT_STOCK_MAIN").unwrap()).unwrap();
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
