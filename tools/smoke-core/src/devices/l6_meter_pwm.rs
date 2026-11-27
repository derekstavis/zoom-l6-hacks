//! L6 level-meter diagnostic: one retained debugger and independently reported windows.
use super::*;

fn panel_ms(session: &Session) -> Result<u32, String> {
    Ok(session
        .control()
        .input_status(l6ctl::InputSource::Panel)
        .map_err(|e| e.to_string())?
        .guest_ms)
}
fn check(session: &Session, start: u32, guest: u32, deadline: Instant) -> Result<(), String> {
    if Instant::now() >= deadline || panel_ms(session)?.wrapping_sub(start) > guest {
        Err("L6 level-meter shared deadline exceeded".into())
    } else {
        Ok(())
    }
}
fn local(deadline: Instant, seconds: u64) -> Instant {
    deadline.min(Instant::now() + Duration::from_secs(seconds))
}
fn window_decision(
    elapsed: u32,
    check: impl FnOnce() -> Result<(), String>,
) -> Result<(bool, bool), String> {
    check()?;
    Ok((elapsed >= 100, elapsed >= 500))
}
fn responsive_cell(values: &[f64]) -> bool {
    values.len() == 6
        && values[0] == 0.0
        && values[1..].iter().all(|v| *v > 0.0)
        && values.windows(2).all(|pair| pair[1] > pair[0])
}

pub(super) struct Scope {
    debug: Option<l6max_host::debug::Session>,
    start: u32,
    guest: u32,
    wall: u64,
    deadline: Instant,
    pub stages: Vec<Value>,
    pub asserted: bool,
}
impl Scope {
    pub(super) fn begin(session: &Session, guest: u32, wall: u64) -> Result<Self, String> {
        if session.model() != l6fw::Model::L6 {
            return Err("L6 level-meter requires L6".into());
        }
        let start = panel_ms(session)?;
        let deadline = Instant::now()
            .checked_add(Duration::from_secs(wall))
            .ok_or("level-meter deadline overflow")?;
        let control = session.control();
        let mut debug = control
            .debug_session_before(deadline)
            .map_err(|e| e.to_string())?;
        let cancel = || check(session, start, guest, deadline).is_err();
        let setup = (|| -> Result<(), String> {
            debug.l6_meter_guard_before(deadline, cancel)?;
            let initial_gates = control
                .read_main_memory_before(l6::level_meter_active_gates, 3, deadline)
                .map_err(|e| e.to_string())?
                .bytes;
            check(session, start, guest, deadline)?;
            let initial_phase = control
                .read_main_memory_before(l6::level_meter_task_phase, 4, deadline)
                .map_err(|e| e.to_string())?
                .bytes;
            check(session, start, guest, deadline)?;
            debug.breakpoint_before(l6::level_meter_task, true, deadline)?;
            check(session, start, guest, deadline)?;
            debug.continue_before(local(deadline, wall.min(30)), cancel).map_err(|e| format!("LevelMeterLed task entry was not reached (phase {initial_phase:02x?}, gates {initial_gates:02x?}): {e}; PWM probe was not run"))?;
            check(session, start, guest, deadline)?;
            if debug.register_before(15, deadline)? & !1 != l6::level_meter_task {
                return Err("LevelMeterLed entry breakpoint was missed".into());
            }
            check(session, start, guest, deadline)?;
            debug.breakpoint_before(l6::level_meter_task, false, deadline)?;
            debug.l6_meter_skip_startup_before(deadline, cancel)?;
            debug.l6_meter_activate_before(deadline, cancel)?;
            check(session, start, guest, deadline)?;
            debug.breakpoint_before(l6::level_meter_threshold_builder, true, deadline)?;
            check(session, start, guest, deadline)?;
            if let Err(error) = debug.continue_before(local(deadline, 5), cancel) {
                if cancel() {
                    return Err(format!(
                        "waiting for L6 meter threshold builder failed: {error}; shared deadline expired"
                    ));
                }
                let pc = debug
                    .register_before(15, deadline)
                    .map(|v| format!("{:#x}", v & !1))
                    .unwrap_or_else(|e| format!("unavailable: {e}"));
                let gates = control
                    .read_main_memory_before(l6::level_meter_active_gates, 2, deadline)
                    .map(|v| format!("{:02x?}", v.bytes))
                    .unwrap_or_else(|e| format!("unavailable: {e}"));
                let phase = control
                    .read_main_memory_before(l6::level_meter_task_phase, 4, deadline)
                    .map(|v| format!("{:02x?}", v.bytes))
                    .unwrap_or_else(|e| format!("unavailable: {e}"));
                return Err(format!(
                    "waiting for L6 meter threshold builder failed: {error}; PC={pc}; gates={gates}; phase={phase}"
                ));
            }
            check(session, start, guest, deadline)?;
            if debug.register_before(15, deadline)? & !1 != l6::level_meter_threshold_builder {
                return Err("L6 meter threshold-builder breakpoint was missed".into());
            }
            Ok(())
        })();
        if let Err(error) = setup {
            return match debug.l6_meter_abort_before(Instant::now() + Duration::from_secs(3)) {
                Ok(()) => Err(error),
                Err(cleanup) => Err(format!("{error}; L6 meter debugger cleanup: {cleanup}")),
            };
        }
        Ok(Self {
            debug: Some(debug),
            start,
            guest,
            wall,
            deadline,
            stages: Vec::new(),
            asserted: false,
        })
    }
    pub(super) fn check(&self, session: &Session, guest: u32, wall: u64) -> Result<(), String> {
        if (guest, wall) != (self.guest, self.wall) {
            return Err("L6 meter shared budget changed".into());
        }
        check(session, self.start, self.guest, self.deadline)
    }
    pub(super) fn stage(
        &mut self,
        session: &Session,
        options: &Options,
        value: u32,
    ) -> Result<Value, String> {
        let stage_start = panel_ms(session)?;
        let mut sums = [[0_u64; 24]; 8];
        let mut count = 0_u64;
        let cancel = || check(session, self.start, self.guest, self.deadline).is_err();
        let debug = self.debug.as_mut().ok_or("L6 meter debugger missing")?;
        let capture = (|| -> Result<(), String> {
            loop {
                check(session, self.start, self.guest, self.deadline)?;
                debug.l6_meter_seed_before(value, self.deadline, cancel)?;
                check(session, self.start, self.guest, self.deadline)?;
                debug.breakpoint_before(l6::level_meter_threshold_builder, false, self.deadline)?;
                check(session, self.start, self.guest, self.deadline)?;
                debug.step_before(self.deadline, cancel)?;
                check(session, self.start, self.guest, self.deadline)?;
                if debug.register_before(15, self.deadline)? & !1
                    == l6::level_meter_threshold_builder
                {
                    return Err("L6 meter builder did not advance past its entry".into());
                }
                check(session, self.start, self.guest, self.deadline)?;
                debug.breakpoint_before(l6::level_meter_threshold_builder, true, self.deadline)?;
                check(session, self.start, self.guest, self.deadline)?;
                debug
                    .continue_before(local(self.deadline, 10), cancel)
                    .map_err(|e| format!("advancing L6 meter task at {value}: {e}"))?;
                check(session, self.start, self.guest, self.deadline)?;
                if debug.register_before(15, self.deadline)? & !1
                    != l6::level_meter_threshold_builder
                {
                    return Err("L6 meter threshold-builder breakpoint was missed".into());
                }
                let panel = session.poll()?;
                let elapsed = panel_ms(session)?.wrapping_sub(stage_start);
                let (sample, done) = window_decision(elapsed, || {
                    check(session, self.start, self.guest, self.deadline)
                })?;
                if sample {
                    for (row, values) in panel.indicator_levels.iter().enumerate() {
                        for (column, level) in values.iter().enumerate() {
                            sums[row][column] += u64::from(*level);
                        }
                    }
                    count += 1;
                }
                if done {
                    break;
                }
            }
            Ok(())
        })();
        if let Err(error) = capture {
            let partial =
                json!({"input_level":value,"sample_count":count,"partial_sums":sums,"error":error});
            self.stages.push(partial);
            return match self.save(options, None) {
                Ok(()) => Err(error),
                Err(save) => Err(format!(
                    "{error}; could not save partial L6 meter evidence: {save}"
                )),
            };
        }
        if count == 0 {
            return Err("L6 level-meter PWM captured no panel samples".into());
        }
        let ctl = session.control();
        let read = |address, length| -> Result<Vec<u8>, String> {
            check(session, self.start, self.guest, self.deadline)?;
            let bytes = ctl
                .read_main_memory_before(address, length, self.deadline)
                .map_err(|e| e.to_string())?
                .bytes;
            check(session, self.start, self.guest, self.deadline)?;
            Ok(bytes)
        };
        let means: [[f64; 24]; 8] =
            std::array::from_fn(|r| std::array::from_fn(|c| sums[r][c] as f64 / count as f64));
        let records = (|| -> Result<_, String> {
            Ok((
                read(l6::level_meter_group_a_records, 12)?,
                read(l6::level_meter_group_b_records, 12)?,
                read(l6::level_meter_thresholds, 0x32)?,
            ))
        })();
        let (a, b, thresholds) = match records {
            Ok(records) => records,
            Err(error) => {
                self.stages.push(json!({"input_level":value,"sample_count":count,"mean_levels":means,"error":error}));
                return match self.save(options, None) {
                    Ok(()) => Err(error),
                    Err(save) => Err(format!(
                        "{error}; could not save partial L6 meter evidence: {save}"
                    )),
                };
            }
        };
        let evidence = json!({"input_level":value,"sample_count":count,"group_a_records":a,"group_b_records":b,"threshold_masks_and_codes":thresholds,"mean_levels":means});
        self.stages.push(evidence.clone());
        check(session, self.start, self.guest, self.deadline)?;
        self.save(options, None)?;
        check(session, self.start, self.guest, self.deadline)?;
        Ok(evidence)
    }
    fn save(&self, options: &Options, responsive: Option<usize>) -> Result<(), String> {
        let evidence = json!({"stimulus":"l6_level_meter_threshold_builder_inputs","setup":"boot_time_panel_output_sequence_skipped","responsive_cells":responsive,"samples_per_stage":self.stages});
        fs::write(
            options.logs.join("l6-level-meter-pwm.json"),
            serde_json::to_vec_pretty(&evidence).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())
    }
    pub(super) fn assert(&mut self, session: &Session, options: &Options) -> Result<Value, String> {
        check(session, self.start, self.guest, self.deadline)?;
        if self.stages.len() != 6 {
            return Err("L6 meter requires six stages".into());
        }
        let responsive = (0..8)
            .flat_map(|r| (0..24).map(move |c| (r, c)))
            .filter(|&(r, c)| {
                let values: Vec<f64> = self
                    .stages
                    .iter()
                    .map(|s| s["mean_levels"][r][c].as_f64().unwrap())
                    .collect();
                responsive_cell(&values)
            })
            .count();
        self.save(options, Some(responsive))?;
        check(session, self.start, self.guest, self.deadline)?;
        if responsive == 0 {
            return Err("L6 meter stimulus produced no panel cell with five distinct increasing nonzero duty levels; measurements saved to l6-level-meter-pwm.json".into());
        }
        self.asserted = true;
        Ok(json!({"responsive_cells":responsive}))
    }
    pub(super) fn finish(&mut self, session: &Session) -> Result<(), String> {
        let cancel = || check(session, self.start, self.guest, self.deadline).is_err();
        let debug = self.debug.as_mut().ok_or("L6 meter debugger missing")?;
        check(session, self.start, self.guest, self.deadline)?;
        debug.breakpoint_before(l6::level_meter_threshold_builder, false, self.deadline)?;
        check(session, self.start, self.guest, self.deadline)?;
        debug.l6_meter_finish_before(self.deadline, cancel)?;
        self.debug = None;
        Ok(())
    }
    pub(super) fn abort(&mut self) -> Result<(), String> {
        if let Some(debug) = self.debug.as_mut() {
            debug.l6_meter_abort_before(Instant::now() + Duration::from_secs(3))?;
        }
        self.debug = None;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn window_requires_budget_even_if_poll_completed_after_window() {
        assert!(window_decision(500, || Err("expired".into())).is_err());
        assert_eq!(window_decision(99, || Ok(())).unwrap(), (false, false));
        assert_eq!(window_decision(100, || Ok(())).unwrap(), (true, false));
        assert_eq!(window_decision(500, || Ok(())).unwrap(), (true, true));
    }
    #[test]
    fn responsive_cell_requires_zero_then_five_strictly_increasing_nonzero_means() {
        assert!(responsive_cell(&[0.0, 1.0, 2.0, 3.0, 4.0, 5.0]));
        assert!(!responsive_cell(&[1.0, 2.0, 3.0, 4.0, 5.0, 6.0]));
        assert!(!responsive_cell(&[0.0, 1.0, 2.0, 2.0, 4.0, 5.0]));
    }
}
