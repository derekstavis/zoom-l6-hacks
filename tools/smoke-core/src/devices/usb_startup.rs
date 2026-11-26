//! Paused-start L6 USB probe: capture remains atomic because breakpoint hits
//! and guest-timed input replay must be interleaved in the same observation.
use super::*;

const BREAKPOINTS: [(u32, &str); 3] = [
    (l6::usb_start_wrapper, "usb_start_wrapper"),
    (l6::usb_start_initializer, "usb_start_initializer"),
    (l6::usb_phy_gate, "usb_phy_gate"),
];
#[derive(Debug, PartialEq, Eq)]
enum Replay {
    Target(u32),
    Panel(u32),
}
fn next_replay(
    ms: u32,
    target_count: usize,
    panel_count: usize,
    panel_ids: &[u32],
) -> Option<Replay> {
    let times = [40_000, 42_000, 44_000, 46_000];
    if target_count < times.len() && ms >= times[target_count] {
        Some(Replay::Target(if target_count < 2 { 3 } else { 49 }))
    } else if target_count == times.len()
        && panel_count < panel_ids.len()
        && ms >= 48_000 + panel_count as u32 * 250
    {
        Some(Replay::Panel(panel_ids[panel_count]))
    } else {
        None
    }
}
fn capture_deadline(resumed: Instant, wall_budget: u64) -> Result<Instant, String> {
    resumed
        .checked_add(Duration::from_secs(wall_budget))
        .ok_or("USB capture deadline overflow".into())
}
fn panel_ms(session: &Session) -> Result<u32, String> {
    Ok(l6ctl::OwnedControl::new(&session.engine)
        .input_status(l6ctl::InputSource::Panel)
        .map_err(|e| e.to_string())?
        .guest_ms)
}
pub(super) struct Scope {
    observer: Option<l6max_host::debug::Observer>,
    deadline: Instant,
    wall_budget: u64,
    guest_budget: u32,
    hits: Vec<Value>,
    target_count: usize,
    panel_count: usize,
    panel_ids: Vec<u32>,
    trace: Option<String>,
    captured_guest: Option<u32>,
    hits_ok: bool,
    writes_ok: bool,
    replay_ok: bool,
}
impl Scope {
    fn check(&self, session: &Session) -> Result<(), String> {
        let ms = panel_ms(session)?;
        if Instant::now() >= self.deadline || ms > self.guest_budget {
            Err(format!(
                "L6 USB startup deadline exceeded at {ms} panel guest ms"
            ))
        } else {
            Ok(())
        }
    }
    pub(super) fn begin(
        session: &Session,
        options: &Options,
        guest: u32,
        wall: u64,
    ) -> Result<Self, String> {
        if session.engine.model() != l6fw::Model::L6 {
            return Err("L6 USB startup requires L6".into());
        }
        let deadline = Instant::now()
            .checked_add(Duration::from_secs(wall))
            .ok_or("USB startup deadline overflow")?;
        let stock =
            fs::read(options.firmware_dir.join("main_firmware.bin")).map_err(|e| e.to_string())?;
        if !crate::firmware::is_stock(&stock, l6fw::Model::L6) {
            return Err("L6 USB startup requires stock MAIN".into());
        }
        let ctl = l6ctl::OwnedControl::new(&session.engine);
        let mut observer = ctl
            .debug_observer_before(deadline)
            .map_err(|e| e.to_string())?;
        let setup = (|| -> Result<(), String> {
            for (address, _) in BREAKPOINTS {
                if Instant::now() >= deadline || panel_ms(session)? > guest {
                    return Err("USB startup setup timed out".into());
                }
                let offset = (address - l6fw::Model::L6.main_base()) as usize;
                let expected = stock
                    .get(offset..offset + 16)
                    .ok_or("USB code outside stock MAIN")?;
                let actual = ctl
                    .read_main_memory_before(address, 16, deadline)
                    .map_err(|e| e.to_string())?;
                if actual.bytes != expected {
                    return Err(format!("USB startup code mismatch at {address:#x}"));
                }
                if Instant::now() >= deadline || panel_ms(session)? > guest {
                    return Err("USB startup setup timed out".into());
                }
            }
            for (address, _) in BREAKPOINTS {
                if Instant::now() >= deadline || panel_ms(session)? > guest {
                    return Err("USB startup setup timed out".into());
                }
                observer.breakpoint_before(address, true, deadline)?;
                if Instant::now() >= deadline || panel_ms(session)? > guest {
                    return Err("USB startup setup timed out".into());
                }
            }
            Ok(())
        })();
        if let Err(error) = setup {
            return match observer.abort() {
                Ok(()) => Err(error),
                Err(cleanup) => Err(format!("{error}; USB debugger cleanup: {cleanup}")),
            };
        }
        let mut panel_ids = vec![1, 2, 3, 6];
        panel_ids.extend(7..=30);
        panel_ids.push(47);
        Ok(Self {
            observer: Some(observer),
            deadline,
            wall_budget: wall,
            guest_budget: guest,
            hits: Vec::new(),
            target_count: 0,
            panel_count: 0,
            panel_ids,
            trace: None,
            captured_guest: None,
            hits_ok: false,
            writes_ok: false,
            replay_ok: false,
        })
    }
    pub(super) fn capture(
        &mut self,
        session: &Session,
        options: &Options,
    ) -> Result<Value, String> {
        self.check(session)?;
        self.observer
            .as_ref()
            .ok_or("USB observer missing")?
            .resume_panel_before(self.deadline)?;
        self.check(session)?;
        self.observer
            .as_mut()
            .ok_or("USB observer missing")?
            .resume_main_before(self.deadline)?;
        self.check(session)?;
        // Legacy capture starts its wall clock after both guests resume.
        // Setup is independently bounded by the initial deadline above.
        self.deadline = capture_deadline(Instant::now(), self.wall_budget)?;
        let ctl = l6ctl::OwnedControl::new(&session.engine);
        while panel_ms(session)? < 60_000 {
            self.check(session)?;
            session.poll()?;
            self.check(session)?;
            let running = self
                .observer
                .as_ref()
                .ok_or("USB observer missing")?
                .main_running_before(self.deadline)?;
            self.check(session)?;
            if !running {
                let pc = self
                    .observer
                    .as_mut()
                    .unwrap()
                    .register_before(15, self.deadline)?
                    & !1;
                self.check(session)?;
                let Some((address, name)) = BREAKPOINTS.iter().find(|(address, _)| *address == pc)
                else {
                    return Err(format!(
                        "L6 USB startup stopped at unexpected PC {pc:#010x}"
                    ));
                };
                if self
                    .hits
                    .iter()
                    .any(|hit| hit["address"] == format!("{address:#010x}"))
                {
                    return Err(format!("duplicate USB startup breakpoint {name}"));
                }
                let mut registers = Vec::new();
                for register in 0..4 {
                    self.check(session)?;
                    registers.push(
                        self.observer
                            .as_mut()
                            .unwrap()
                            .register_before(register, self.deadline)?,
                    );
                    self.check(session)?;
                }
                self.hits.push(json!({"function":name,"address":format!("{address:#010x}"),"r0_r3":registers,"guest_ms":panel_ms(session)?}));
                self.observer.as_mut().unwrap().breakpoint_before(
                    *address,
                    false,
                    self.deadline,
                )?;
                self.check(session)?;
                self.observer
                    .as_mut()
                    .unwrap()
                    .resume_main_before(self.deadline)?;
                self.check(session)?;
            } else {
                let ms = panel_ms(session)?;
                match next_replay(ms, self.target_count, self.panel_count, &self.panel_ids) {
                    Some(Replay::Target(target)) => {
                        self.check(session)?;
                        ctl.tap(target, 50).map_err(|e| e.to_string())?;
                        self.target_count += 1;
                    }
                    Some(Replay::Panel(target)) => {
                        self.check(session)?;
                        ctl.tap(target, 50).map_err(|e| e.to_string())?;
                        self.panel_count += 1;
                    }
                    None => {}
                }
                self.check(session)?;
                thread::sleep(Duration::from_millis(5));
            }
        }
        self.check(session)?;
        self.observer
            .as_ref()
            .ok_or("USB observer missing")?
            .stop_main_before(self.deadline)?;
        self.check(session)?;
        session.poll()?;
        self.check(session)?;
        let trace =
            fs::read_to_string(options.logs.join("engine.trace")).map_err(|e| e.to_string())?;
        self.check(session)?;
        self.trace = Some(trace);
        self.captured_guest = Some(panel_ms(session)?);
        Ok(
            json!({"guest_ms":self.captured_guest,"targeted_inputs":self.target_count,"panel_inputs":self.panel_count,"breakpoint_hits":self.hits}),
        )
    }
    pub(super) fn assert_hits(&mut self, session: &Session) -> Result<Value, String> {
        self.check(session)?;
        if self.trace.is_none() {
            return Err("USB capture missing".into());
        }
        for (address, name) in BREAKPOINTS {
            if !self
                .hits
                .iter()
                .any(|hit| hit["address"] == format!("{address:#010x}"))
            {
                return Err(format!(
                    "L6 USB startup did not reach {name} at {address:#010x}"
                ));
            }
        }
        self.hits_ok = true;
        Ok(json!({"breakpoint_hits":self.hits}))
    }
    pub(super) fn assert_writes(&mut self, session: &Session) -> Result<Value, String> {
        self.check(session)?;
        let count = self
            .trace
            .as_ref()
            .ok_or("USB capture missing")?
            .matches("l6_usb_write offset=")
            .count();
        if count == 0 {
            return Err(
                "L6 USB startup reached its functions but wrote no controller registers".into(),
            );
        }
        self.writes_ok = true;
        Ok(json!({"usb_controller_writes":count}))
    }
    pub(super) fn assert_replay(&mut self, session: &Session) -> Result<Value, String> {
        self.check(session)?;
        if self.target_count != 4 || self.panel_count != self.panel_ids.len() {
            return Err(format!(
                "input replay incomplete: targeted {}/4, panel {}/{}",
                self.target_count,
                self.panel_count,
                self.panel_ids.len()
            ));
        }
        self.replay_ok = true;
        Ok(json!({"targeted_inputs":self.target_count,"panel_input_ids":self.panel_ids}))
    }
    pub(super) fn finish(&mut self, session: &Session) -> Result<Value, String> {
        self.check(session)?;
        if !self.hits_ok || !self.writes_ok || !self.replay_ok {
            return Err("USB startup assertions incomplete".into());
        }
        self.observer
            .as_mut()
            .ok_or("USB debugger missing")?
            .close_stopped_before(self.deadline)?;
        self.observer = None;
        Ok(
            json!({"model":"L6","guest_ms":self.captured_guest,"breakpoint_hits":self.hits,"targeted_inputs":self.target_count,"panel_input_ids":self.panel_ids,"usb_controller_writes":self.trace.as_ref().unwrap().matches("l6_usb_write offset=").count()}),
        )
    }
    pub(super) fn abort(&mut self) -> Result<(), String> {
        if let Some(observer) = self.observer.as_mut() {
            observer.abort()?;
        }
        self.observer = None;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn replay_preserves_boundaries_order_and_panel_sweep() {
        let mut ids = vec![1, 2, 3, 6];
        ids.extend(7..=30);
        ids.push(47);
        assert_eq!(next_replay(39_999, 0, 0, &ids), None);
        assert_eq!(next_replay(40_000, 0, 0, &ids), Some(Replay::Target(3)));
        assert_eq!(next_replay(42_000, 1, 0, &ids), Some(Replay::Target(3)));
        assert_eq!(next_replay(44_000, 2, 0, &ids), Some(Replay::Target(49)));
        assert_eq!(next_replay(46_000, 3, 0, &ids), Some(Replay::Target(49)));
        assert_eq!(next_replay(48_249, 4, 1, &ids), None);
        assert_eq!(next_replay(48_250, 4, 1, &ids), Some(Replay::Panel(2)));
        assert_eq!(
            next_replay(60_000, 4, ids.len() - 1, &ids),
            Some(Replay::Panel(47))
        );
        assert_eq!(next_replay(60_000, 4, ids.len(), &ids), None);
    }
    #[test]
    fn capture_wall_budget_starts_after_setup_and_resume() {
        let started = Instant::now();
        let resumed = started + Duration::from_secs(12);
        let deadline = capture_deadline(resumed, 300).unwrap();
        assert_eq!(deadline.duration_since(resumed), Duration::from_secs(300));
        assert_eq!(deadline.duration_since(started), Duration::from_secs(312));
    }
}
