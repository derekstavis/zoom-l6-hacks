//! Terminal L6 DIN dispatch observation using scoped typed debugger commands.
use super::*;
use firmware_api::l6;
use l6max_host::debug::{Command, Observation};

pub(super) struct Scope {
    debugger: Option<l6max_host::debug::Session>,
    reader: l6max_host::l6_din::Reader,
    inputs: l6ctl::InputObserver,
    started: Instant,
    panel_start: u32,
    guest: u32,
    deadline: Instant,
    group: u8,
    cc: u8,
    value: u8,
    before: Option<Vec<u8>>,
    state: Option<u32>,
    setter_value: Option<u32>,
    next_setter: u8,
}
impl Scope {
    fn check(&self, session: &Session) -> Result<(), String> {
        if Instant::now() >= self.deadline
            || guest_now(session).wrapping_sub(self.panel_start) > self.guest
        {
            Err("DIN dispatch shared deadline exceeded".into())
        } else {
            Ok(())
        }
    }
    fn timeout_ms(&self) -> Result<u64, String> {
        let ms = self
            .deadline
            .saturating_duration_since(Instant::now())
            .as_millis()
            .min(60_000) as u64;
        if ms == 0 {
            Err("DIN dispatch deadline exceeded".into())
        } else {
            Ok(ms)
        }
    }
    fn command(&mut self, session: &Session, command: Command) -> Result<Observation, String> {
        self.check(session)?;
        let inputs = self.inputs.clone();
        let start = self.panel_start;
        let guest = self.guest;
        let deadline = self.deadline;
        let result = self
            .debugger
            .as_mut()
            .ok_or("DIN debugger missing")?
            .execute_cancellable(command, || {
                Instant::now() >= deadline
                    || inputs
                        .status(l6ctl::InputSource::Panel)
                        .guest_ms
                        .wrapping_sub(start)
                        > guest
            })?;
        self.check(session)?;
        Ok(result)
    }
    fn breakpoint(&mut self, session: &Session, address: u32, enabled: bool) -> Result<(), String> {
        self.check(session)?;
        self.debugger
            .as_mut()
            .ok_or("DIN debugger missing")?
            .breakpoint_before(address, enabled, self.deadline)?;
        self.check(session)
    }
    fn hit(&mut self, session: &Session, expected: u32, seconds: u64) -> Result<(), String> {
        let pc = self.stop(session, seconds)?;
        if pc != expected {
            return Err(format!("DIN stopped at {pc:#x}, expected {expected:#x}"));
        }
        Ok(())
    }
    fn stop(&mut self, session: &Session, seconds: u64) -> Result<u32, String> {
        self.check(session)?;
        let local = self
            .deadline
            .min(Instant::now() + Duration::from_secs(seconds));
        let inputs = self.inputs.clone();
        let start = self.panel_start;
        let guest = self.guest;
        self.debugger
            .as_mut()
            .ok_or("DIN debugger missing")?
            .continue_before(local, || {
                Instant::now() >= local
                    || inputs
                        .status(l6ctl::InputSource::Panel)
                        .guest_ms
                        .wrapping_sub(start)
                        > guest
            })?;
        self.check(session)?;
        if Instant::now() >= local {
            return Err("DIN continuation timed out".into());
        }
        let pc = self
            .debugger
            .as_mut()
            .unwrap()
            .register_before(15, self.deadline)?
            & !1;
        self.check(session)?;
        Ok(pc)
    }
    fn mixer(&self, session: &Session) -> Result<Vec<u8>, String> {
        self.reader.mixer(self.deadline, &|| self.check(session))
    }
    pub(super) fn begin(
        session: &Session,
        group: u8,
        cc: u8,
        value: u8,
        guest: u32,
        wall: u64,
    ) -> Result<Self, String> {
        if session.model() != l6fw::Model::L6 || group > 15 || cc > 127 || value > 127 {
            return Err("unsupported L6 DIN event".into());
        }
        let started = Instant::now();
        let deadline = started
            .checked_add(Duration::from_secs(wall))
            .ok_or("DIN deadline overflow")?;
        let panel_start = guest_now(session);
        let inputs = session
            .control()
            .input_observer()
            .map_err(|_| "DIN inputs missing")?;
        let check = || {
            if Instant::now() >= deadline || guest_now(session).wrapping_sub(panel_start) > guest {
                Err("DIN setup timed out".into())
            } else {
                Ok(())
            }
        };
        let control = session.control();
        let mut debugger = control
            .debug_session_before(deadline)
            .map_err(|e| e.to_string())?;
        let setup = (|| -> Result<_, String> {
            control
                .verify_l6_din_dispatch_before_checked(deadline, &check)
                .map_err(|e| e.to_string())?;
            let reader = control
                .l6_din_reader_before_checked(deadline, &check)
                .map_err(|e| e.to_string())?;
            check()?;
            debugger.breakpoint_before(l6::main_task, true, deadline)?;
            check()?;
            Ok(reader)
        })();
        let reader = match setup {
            Ok(reader) => reader,
            Err(error) => {
                return match debugger.close_before(Instant::now() + Duration::from_secs(3)) {
                    Ok(()) => Err(error),
                    Err(cleanup) => Err(format!("{error}; debugger cleanup: {cleanup}")),
                };
            }
        };
        Ok(Self {
            debugger: Some(debugger),
            reader,
            inputs,
            started,
            panel_start,
            guest,
            deadline,
            group,
            cc,
            value,
            before: None,
            state: None,
            setter_value: None,
            next_setter: 0,
        })
    }
    pub(super) fn task(&mut self, session: &Session) -> Result<Value, String> {
        self.hit(session, l6::main_task, 60)?;
        self.breakpoint(session, l6::main_task, false)?;
        self.breakpoint(session, l6::main_wait_call, true)?;
        Ok(json!({"pc":l6::main_task}))
    }
    pub(super) fn semaphore(&mut self, session: &Session) -> Result<Value, String> {
        self.hit(session, l6::main_wait_call, 30)?;
        let timeout_ms = self.timeout_ms()?;
        let evidence = self.command(session, Command::L6DinCaptureSemaphore { timeout_ms })?;
        self.breakpoint(session, l6::main_wait_call, false)?;
        self.breakpoint(session, l6::main_wait_return, true)?;
        self.breakpoint(session, l6::idle_loop, true)?;
        Ok(json!({"semaphore":evidence}))
    }
    pub(super) fn idle(&mut self, session: &Session) -> Result<Value, String> {
        self.hit(session, l6::idle_loop, 10)?;
        self.before = Some(self.mixer(session)?);
        Ok(json!({"pc":l6::idle_loop}))
    }
    pub(super) fn inject(&mut self, session: &Session) -> Result<Value, String> {
        let timeout_ms = self.timeout_ms()?;
        let evidence = self.command(
            session,
            Command::L6DinInject {
                group: self.group,
                cc: self.cc,
                value: self.value,
                timeout_ms,
                leave_stopped: true,
            },
        )?;
        self.hit(session, l6::idle_loop, 10)?;
        Ok(json!({"injection":evidence}))
    }
    pub(super) fn wake(&mut self, session: &Session) -> Result<Value, String> {
        let timeout_ms = self.timeout_ms()?;
        self.command(session, Command::L6DinWake { timeout_ms })?;
        self.hit(session, l6::main_wait_return, 10)?;
        Ok(json!({"pc":l6::main_wait_return}))
    }
    pub(super) fn state(&mut self, session: &Session) -> Result<Value, String> {
        self.breakpoint(session, l6::main_wait_return, false)?;
        self.breakpoint(session, l6::main_state_return, true)?;
        self.hit(session, l6::main_state_return, 10)?;
        let value = self
            .debugger
            .as_mut()
            .unwrap()
            .register_before(0, self.deadline)?;
        self.check(session)?;
        self.state = Some(value);
        self.breakpoint(session, l6::main_state_return, false)?;
        self.breakpoint(session, l6::idle_loop, false)?;
        self.breakpoint(session, l6::main_event_queue_reader, true)?;
        Ok(json!({"state":value}))
    }
    pub(super) fn reader(&mut self, session: &Session) -> Result<Value, String> {
        let mut reached = false;
        for _ in 0..8 {
            let pc = self.stop(session, 30)?;
            if pc == l6::main_event_queue_reader {
                reached = true;
                break;
            }
            if pc == l6::idle_loop {
                continue;
            } else {
                return Err(format!("L6 Main initialization stopped at {pc:#x}"));
            }
        }
        if !reached {
            return Err("L6 Main did not reach its queue reader".into());
        }
        self.breakpoint(session, l6::main_event_queue_reader, false)?;
        self.breakpoint(session, l6::main_class2_event_handler, true)?;
        Ok(json!({"pc":l6::main_event_queue_reader}))
    }
    pub(super) fn event(&mut self, session: &Session) -> Result<Value, String> {
        for _ in 0..8 {
            let pc = self.stop(session, 30)?;
            if pc == l6::main_event_queue_reader {
                continue;
            }
            if pc != l6::main_class2_event_handler {
                return Err(format!("L6 event queue stopped at {pc:#x}"));
            }
            let timeout_ms = self.timeout_ms()?;
            let Observation::L6DinEvent { words } =
                self.command(session, Command::L6DinEvent { timeout_ms })?
            else {
                return Err("DIN event evidence missing".into());
            };
            if words
                == [
                    2,
                    u32::from(self.group),
                    u32::from(self.cc),
                    u32::from(self.value),
                    0,
                ]
            {
                self.breakpoint(session, l6::main_class2_event_handler, false)?;
                self.breakpoint(session, l6::mixer_set_eq, true)?;
                return Ok(json!({"words":words}));
            }
        }
        Err("L6 Main did not dispatch the injected DIN event".into())
    }
    pub(super) fn eq(&mut self, session: &Session) -> Result<Value, String> {
        self.hit(session, l6::mixer_set_eq, 10)?;
        let timeout_ms = self.timeout_ms()?;
        let Observation::L6DinSetterValue { value } =
            self.command(session, Command::L6DinSetterValue { timeout_ms })?
        else {
            return Err("DIN setter evidence missing".into());
        };
        self.setter_value = Some(value);
        self.breakpoint(session, l6::mixer_set_eq, false)?;
        self.breakpoint(session, l6::idle_loop, true)?;
        Ok(json!({"setter_value":value}))
    }
    pub(super) fn observe(&mut self, session: &Session) -> Result<Value, String> {
        self.hit(session, l6::idle_loop, 10)?;
        let after = self.mixer(session)?;
        let before = self.before.as_ref().ok_or("DIN baseline missing")?;
        let changed = before.iter().zip(&after).filter(|(a, b)| a != b).count();
        let setter = self.setter_value.ok_or("DIN setter value missing")?;
        if setter != u32::from(self.value) || changed == 0 {
            return Err(format!(
                "queued DIN CC expected value {} and mixer change, got {setter} and {changed} bytes",
                self.value
            ));
        }
        Ok(json!({"setter_value":setter,"event_changed_bytes":changed,"main_state":self.state}))
    }
    pub(super) fn setter(
        &mut self,
        session: &Session,
        index: u8,
        value: u8,
    ) -> Result<Value, String> {
        if index != self.next_setter {
            return Err(format!("expected DIN setter {}", self.next_setter));
        }
        let before = self.mixer(session)?;
        let timeout_ms = self.timeout_ms()?;
        self.command(
            session,
            Command::L6DinSetter {
                index,
                value,
                timeout_ms,
            },
        )?;
        self.hit(session, l6::idle_loop, 10)?;
        let after = self.mixer(session)?;
        let changed = before.iter().zip(&after).filter(|(a, b)| a != b).count();
        if changed == 0 {
            return Err(format!("DIN setter group {index} did not change mixer RAM"));
        }
        self.next_setter += 1;
        Ok(json!({"group":index,"value":value,"changed_bytes":changed}))
    }
    pub(super) fn finish(&mut self, session: &Session) -> Result<Value, String> {
        if self.next_setter != 9 {
            return Err("DIN setter observations incomplete".into());
        }
        let timeout_ms = self.timeout_ms()?;
        let result = self.command(session, Command::L6DinFinish { timeout_ms })?;
        self.debugger = None;
        Ok(json!({"finish":result,"wall_ms":self.started.elapsed().as_millis()}))
    }
    pub(super) fn abort(&mut self) -> Result<(), String> {
        if let Some(d) = self.debugger.as_mut() {
            d.close_before(Instant::now() + Duration::from_secs(3))?;
        }
        self.debugger = None;
        Ok(())
    }
}
