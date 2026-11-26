//! Concurrent audio callback breakpoints with a retained debugger lease.
use super::*;

pub(in crate::devices) struct Scope {
    model: l6fw::Model,
    started: Instant,
    guest_start: u32,
    guest_budget: u32,
    wall_budget: u64,
    debugger: Option<l6max_host::debug::Session>,
    hits: Vec<Value>,
    trace: Option<String>,
    asserted: bool,
}

fn budget_expired(
    started: Instant,
    now: Instant,
    guest_start: u32,
    guest_now: u32,
    guest: u32,
    wall: u64,
) -> bool {
    now.duration_since(started) >= Duration::from_secs(wall)
        || guest_now.wrapping_sub(guest_start) > guest
}
fn expired(session: &Session, started: Instant, guest_start: u32, guest: u32, wall: u64) -> bool {
    session
        .control()
        .input_status(l6ctl::InputSource::Panel)
        .map_or(true, |status| {
            budget_expired(
                started,
                Instant::now(),
                guest_start,
                status.guest_ms,
                guest,
                wall,
            )
        })
}
fn checked_debug_op<T>(
    mut cancelled: impl FnMut() -> bool,
    op: impl FnOnce() -> Result<T, String>,
) -> Result<T, String> {
    if cancelled() {
        return Err("audio callbacks exceeded their shared deadline".into());
    }
    let value = op()?;
    if cancelled() {
        return Err("audio callbacks exceeded their shared deadline".into());
    }
    Ok(value)
}
fn guard_site(model: l6fw::Model, stock: &[u8], address: u32, actual: &[u8]) -> Result<(), String> {
    let offset = address
        .checked_sub(model.main_base())
        .ok_or("audio callback address outside stock MAIN")? as usize;
    let expected = stock
        .get(offset..offset + 16)
        .ok_or("audio callback site outside stock MAIN")?;
    if actual != expected {
        return Err(format!(
            "audio callback site {address:#x} does not match supported stock MAIN"
        ));
    }
    Ok(())
}

fn callback_name(model: l6fw::Model, hits: &[Value], pc: u32) -> Result<&'static str, String> {
    let (_, name) = callbacks(model)
        .iter()
        .find(|(address, _)| *address == pc)
        .ok_or_else(|| format!("unexpected audio stop at {pc:#x}"))?;
    if hits.iter().any(|hit| hit["callback"] == *name) {
        return Err(format!("duplicate audio callback {name}"));
    }
    Ok(name)
}

impl Scope {
    pub(in crate::devices) fn begin(
        session: &Session,
        options: &Options,
        guest: u32,
        wall: u64,
    ) -> Result<Self, String> {
        let started = Instant::now();
        let deadline = started + Duration::from_secs(wall);
        let guest_start = session
            .control()
            .input_status(l6ctl::InputSource::Panel)
            .map_err(|_| "audio callbacks need inputs")?
            .guest_ms;
        let model = session.model();
        let stock =
            fs::read(options.firmware_dir.join("main_firmware.bin")).map_err(|e| e.to_string())?;
        if !crate::firmware::is_stock(&stock, model) {
            return Err("audio callbacks require supported stock MAIN".into());
        }
        let cancelled = || expired(session, started, guest_start, guest, wall);
        if cancelled() {
            return Err("audio callback setup exceeded shared deadline".into());
        }
        let control = session.control();
        let mut debugger = control
            .debug_session_before(deadline)
            .map_err(|e| e.to_string())?;
        let setup = (|| {
            for (address, _) in callbacks(model) {
                if cancelled() {
                    return Err("audio callback setup exceeded shared deadline".into());
                }
                let actual = control
                    .read_main_memory_before(*address, 16, deadline)
                    .map_err(|e| e.to_string())?;
                if cancelled() {
                    return Err("audio callback setup exceeded shared deadline".into());
                }
                guard_site(model, &stock, *address, &actual.bytes)?;
            }
            for (address, _) in callbacks(model) {
                if cancelled() {
                    return Err("audio callback setup exceeded shared deadline".into());
                }
                debugger.breakpoint_before(*address, true, deadline)?;
                if cancelled() {
                    return Err("audio callback setup exceeded shared deadline".into());
                }
            }
            Ok::<_, String>(())
        })();
        if let Err(error) = setup {
            return match debugger.close_before(Instant::now() + Duration::from_secs(3)) {
                Ok(()) => Err(error),
                Err(cleanup) => Err(format!("{error}; audio debugger cleanup failed: {cleanup}")),
            };
        }
        Ok(Self {
            model,
            started,
            guest_start,
            guest_budget: guest,
            wall_budget: wall,
            debugger: Some(debugger),
            hits: Vec::new(),
            trace: None,
            asserted: false,
        })
    }
    fn deadline(&self) -> Instant {
        self.started + Duration::from_secs(self.wall_budget)
    }
    pub(in crate::devices) fn check(
        &self,
        session: &Session,
        guest: u32,
        wall: u64,
    ) -> Result<(), String> {
        if (guest, wall) != (self.guest_budget, self.wall_budget)
            || expired(session, self.started, self.guest_start, guest, wall)
        {
            return Err("audio callbacks exceeded their shared deadline or changed budgets".into());
        }
        Ok(())
    }
    pub(in crate::devices) fn next(&mut self, session: &Session) -> Result<Value, String> {
        self.check(session, self.guest_budget, self.wall_budget)?;
        if self.hits.len() >= callbacks(self.model).len() {
            return Err("all audio callbacks already collected".into());
        }
        let deadline = self.deadline();
        let (started, guest_start, guest, wall) = (
            self.started,
            self.guest_start,
            self.guest_budget,
            self.wall_budget,
        );
        let missing: Vec<_> = callbacks(self.model)
            .iter()
            .map(|(_, name)| *name)
            .filter(|name| !self.hits.iter().any(|hit| hit["callback"] == *name))
            .collect();
        let debugger = self.debugger.as_mut().ok_or("audio debugger missing")?;
        debugger
            .continue_before(deadline, || {
                expired(session, started, guest_start, guest, wall)
            })
            .map_err(|error| format!("{error}; audio callbacks not reached: {missing:?}"))?;
        let pc = checked_debug_op(
            || expired(session, started, guest_start, guest, wall),
            || debugger.register_before(15, deadline),
        )? & !1;
        let name = callback_name(self.model, &self.hits, pc)?;
        let registers = (0..4)
            .map(|index| {
                checked_debug_op(
                    || expired(session, started, guest_start, guest, wall),
                    || debugger.register_before(index, deadline),
                )
            })
            .collect::<Result<Vec<_>, _>>()?;
        let lr = checked_debug_op(
            || expired(session, started, guest_start, guest, wall),
            || debugger.register_before(14, deadline),
        )?;
        let guest_ms = session
            .control()
            .input_status(l6ctl::InputSource::Panel)
            .map_err(|_| "audio callbacks need inputs")?
            .guest_ms;
        checked_debug_op(
            || expired(session, started, guest_start, guest, wall),
            || debugger.breakpoint_before(pc, false, deadline),
        )?;
        let hit = json!({"callback": name, "address":format!("{pc:#010x}"),"r0_r3":registers,"lr":lr,"guest_ms":guest_ms});
        self.hits.push(hit.clone());
        Ok(hit)
    }
    pub(in crate::devices) fn capture(
        &mut self,
        session: &Session,
        options: &Options,
    ) -> Result<Value, String> {
        self.check(session, self.guest_budget, self.wall_budget)?;
        if self.hits.len() != callbacks(self.model).len() {
            return Err("audio callbacks incomplete".into());
        }
        let deadline = self.deadline();
        let mut debugger = self.debugger.take().ok_or("audio debugger missing")?;
        debugger.close_resuming_before(deadline)?;
        let trace =
            fs::read_to_string(options.logs.join("engine.trace")).map_err(|e| e.to_string())?;
        self.trace = Some(trace);
        Ok(json!({"callbacks":self.hits.len(),"trace":"frozen"}))
    }
    pub(in crate::devices) fn assert_trace(&mut self, options: &Options) -> Result<Value, String> {
        let trace = self.trace.as_ref().ok_or("audio trace not frozen")?;
        let dma = verify_sai_edma_trace(self.model, trace).map_err(|e| e.to_string())?;
        let evidence = json!({"breakpoint_hits":self.hits,"sai_edma":dma});
        fs::write(
            options.logs.join("audio-path-smoke.json"),
            serde_json::to_vec_pretty(&evidence).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
        self.asserted = true;
        Ok(evidence)
    }
    pub(in crate::devices) fn finish(&self) -> Result<Value, String> {
        if !self.asserted || self.debugger.is_some() {
            return Err("audio callbacks incomplete".into());
        }
        Ok(json!({"finished":true}))
    }
    pub(in crate::devices) fn abort(&mut self) -> Result<(), String> {
        match self.debugger.take() {
            Some(mut debugger) => debugger.close_before(Instant::now() + Duration::from_secs(3)),
            None => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    #[ignore = "requires L6_AUDIO_STOCK_L6 and L6_AUDIO_STOCK_L6MAX stock MAIN files"]
    fn supported_stock_images_cover_every_guarded_site() {
        for (model, env) in [
            (l6fw::Model::L6, "L6_AUDIO_STOCK_L6"),
            (l6fw::Model::L6max, "L6_AUDIO_STOCK_L6MAX"),
        ] {
            let bytes = fs::read(std::env::var(env).expect("missing stock MAIN path")).unwrap();
            assert!(crate::firmware::is_stock(&bytes, model));
            for (address, _) in callbacks(model) {
                let offset = (*address - model.main_base()) as usize;
                let site = bytes.get(offset..offset + 16).expect("site outside MAIN");
                guard_site(model, &bytes, *address, site).unwrap();
                let mut changed = site.to_vec();
                changed[0] ^= 1;
                assert!(guard_site(model, &bytes, *address, &changed).is_err());
            }
        }
    }

    #[test]
    fn callback_hits_accept_any_order_and_reject_duplicates_and_unknown_stops() {
        for model in [l6fw::Model::L6, l6fw::Model::L6max] {
            let points = callbacks(model);
            let (last_pc, last_name) = points[points.len() - 1];
            let (first_pc, first_name) = points[0];
            assert_eq!(callback_name(model, &[], last_pc).unwrap(), last_name);
            let hits = vec![json!({"callback":last_name})];
            assert_eq!(callback_name(model, &hits, first_pc).unwrap(), first_name);
            assert!(
                callback_name(model, &hits, last_pc)
                    .unwrap_err()
                    .contains("duplicate")
            );
            assert!(
                callback_name(model, &hits, 0x8000_0000)
                    .unwrap_err()
                    .contains("unexpected")
            );
        }
    }

    #[test]
    fn callback_guard_uses_model_base_and_rejects_mismatches() {
        for model in [l6fw::Model::L6, l6fw::Model::L6max] {
            let mut stock = vec![0; 64];
            stock[16..32].fill(7);
            assert!(guard_site(model, &stock, model.main_base() + 16, &[7; 16]).is_ok());
            assert!(guard_site(model, &stock, model.main_base() + 16, &[8; 16]).is_err());
            assert!(guard_site(model, &stock, model.main_base() - 2, &[7; 16]).is_err());
        }
    }
    #[test]
    fn expiry_during_register_read_prevents_later_mutation() {
        use std::cell::Cell;
        let expired = Cell::new(false);
        let removed = Cell::new(false);
        let read = checked_debug_op(
            || expired.get(),
            || {
                expired.set(true);
                Ok(42)
            },
        );
        assert!(read.is_err());
        if read.is_ok() {
            let _ = checked_debug_op(
                || expired.get(),
                || {
                    removed.set(true);
                    Ok(())
                },
            );
        }
        assert!(!removed.get());
    }

    #[test]
    fn shared_budget_includes_setup_and_wraps_panel_clock() {
        let start = Instant::now();
        assert!(!budget_expired(
            start,
            start + Duration::from_secs(299),
            u32::MAX - 10,
            10,
            30,
            300
        ));
        assert!(budget_expired(
            start,
            start + Duration::from_secs(300),
            u32::MAX - 10,
            10,
            30,
            300
        ));
        assert!(budget_expired(
            start,
            start + Duration::from_secs(1),
            u32::MAX - 10,
            25,
            30,
            300
        ));
    }
}
