//! L6max SIGNAL PWM observation with scoped, revision-guarded stimulation.
use super::*;

pub(super) struct Scope {
    started: Instant,
    guest_start: u32,
    guest_budget: u32,
    wall_budget: u64,
    pub phase: u8,
    pub band: u8,
    pub stages: usize,
    pub assertions: usize,
    samples: [Vec<Value>; 2],
    means: [Vec<[f64; 8]>; 2],
    failures: Vec<String>,
    pending_failure: Option<String>,
}
impl Scope {
    pub(super) fn begin(
        session: &Session,
        guest_budget: u32,
        wall_budget: u64,
    ) -> Result<Self, String> {
        if session.model() != l6fw::Model::L6max {
            return Err("SIGNAL PWM requires L6max".into());
        }
        let guest_start = session
            .control()
            .input_status(l6ctl::InputSource::Panel)
            .map_err(|e| e.to_string())?
            .guest_ms;
        Ok(Self {
            started: Instant::now(),
            guest_start,
            guest_budget,
            wall_budget,
            phase: 0,
            band: 0,
            stages: 0,
            assertions: 0,
            samples: [vec![], vec![]],
            means: [vec![], vec![]],
            failures: vec![],
            pending_failure: None,
        })
    }
    fn guest(session: &Session) -> Result<u32, String> {
        Ok(session
            .control()
            .input_status(l6ctl::InputSource::Panel)
            .map_err(|e| e.to_string())?
            .guest_ms)
    }
    pub(super) fn check(&self, session: &Session, guest: u32, wall: u64) -> Result<(), String> {
        if (guest, wall) != (self.guest_budget, self.wall_budget)
            || self.started.elapsed() >= Duration::from_secs(self.wall_budget)
            || Self::guest(session)?.wrapping_sub(self.guest_start) > self.guest_budget
        {
            return Err("SIGNAL PWM shared deadline exceeded or changed".into());
        }
        Ok(())
    }
    fn seed(&self, session: &Session, values: [f32; 12]) -> Result<(), String> {
        self.check(session, self.guest_budget, self.wall_budget)?;
        let control = session.control();
        let deadline = self.started + Duration::from_secs(self.wall_budget);
        let mut debugger = control
            .debug_session_before(deadline)
            .map_err(|e| e.to_string())?;
        let result = debugger
            .signal_write_before(values, deadline, || {
                self.check(session, self.guest_budget, self.wall_budget)
                    .is_err()
            })
            .map(|_| ());
        let cleanup = debugger.close_signal_before(deadline);
        match (result, cleanup) {
            (Ok(()), Ok(())) => self.check(session, self.guest_budget, self.wall_budget),
            (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
            (Err(error), Err(cleanup)) => {
                Err(format!("{error}; debugger cleanup failed: {cleanup}"))
            }
        }
    }
    fn levels(session: &Session) -> Result<[u8; 8], String> {
        let panel = session.poll()?;
        Ok(std::array::from_fn(|channel| {
            let (row, column) = l6max_host::indicators::LED_PINS[52 + channel];
            panel.indicator_levels[row][(column ^ 7) as usize]
        }))
    }
    pub(super) fn warmup(
        &self,
        session: &Session,
        start: u8,
        end: u8,
        value: f32,
    ) -> Result<Value, String> {
        let values = std::array::from_fn(|i| {
            if (start as usize..end as usize).contains(&i) {
                value
            } else {
                0.0
            }
        });
        let begin = Self::guest(session)?;
        let mut last = None;
        loop {
            self.check(session, self.guest_budget, self.wall_budget)?;
            let panel = Self::levels(session)?;
            let guest = Self::guest(session)?;
            if panel[0] != 0 {
                return Ok(json!({"signal_1":panel[0]}));
            }
            if guest.wrapping_sub(begin) >= 10_000 {
                return Err("SIGNAL 1 did not light within 10 guest seconds".into());
            }
            if last.is_none_or(|last: u32| guest.wrapping_sub(last) >= 40) {
                self.seed(session, values)?;
                last = Some(guest);
            }
            thread::sleep(Duration::from_millis(1));
        }
    }
    pub(super) fn drain(&self, session: &Session) -> Result<Value, String> {
        self.seed(session, [0.0; 12])?;
        let start = Self::guest(session)?;
        let mut last = start;
        let mut zero = 0u8;
        loop {
            self.check(session, self.guest_budget, self.wall_budget)?;
            let panel = Self::levels(session)?;
            let guest = Self::guest(session)?;
            zero = if panel[..4].iter().all(|v| *v == 0) {
                zero + 1
            } else {
                0
            };
            if zero >= 3 {
                return Ok(json!({"clear_samples":zero}));
            }
            let elapsed = guest.wrapping_sub(start);
            if elapsed >= 1000 {
                return Err(format!(
                    "previous L6max SIGNAL band did not clear within {elapsed} guest ms"
                ));
            }
            if guest.wrapping_sub(last) >= 40 {
                self.seed(session, [0.0; 12])?;
                last = guest;
            }
            thread::sleep(Duration::from_millis(1));
        }
    }
    pub(super) fn stage(
        &mut self,
        session: &Session,
        band: u8,
        start: u8,
        end: u8,
        value: f32,
    ) -> Result<Value, String> {
        let values = std::array::from_fn(|i| {
            if (start as usize..end as usize).contains(&i) {
                value
            } else {
                0.0
            }
        });
        self.seed(session, values)?;
        let stage_start = Self::guest(session)?;
        let mut last = stage_start;
        let mut samples = Vec::new();
        loop {
            self.check(session, self.guest_budget, self.wall_budget)?;
            let sample = Self::levels(session)?;
            let guest = Self::guest(session)?;
            if guest.wrapping_sub(last) >= 40 {
                self.seed(session, values)?;
                last = guest;
            }
            samples.push(sample);
            if guest.wrapping_sub(stage_start) >= 1000 {
                break;
            }
            thread::sleep(Duration::from_millis(5));
        }
        let mean: [f64; 8] = std::array::from_fn(|channel| {
            samples
                .iter()
                .map(|sample| u64::from(sample[channel]))
                .sum::<u64>() as f64
                / samples.len() as f64
        });
        self.means[band as usize].push(mean);
        let evidence = json!({"input_level":value,"sample_count":samples.len(),"signal_led_mean_levels":mean,"samples":samples});
        self.samples[band as usize].push(evidence.clone());
        Ok(evidence)
    }
    pub(super) fn assert_channel(&mut self, band: u8, channel: u8) -> Result<Value, String> {
        let means = &self.means[band as usize];
        let details = means
            .iter()
            .map(|row| row[channel as usize])
            .collect::<Vec<_>>();
        let failure_start = self.failures.len();
        for stage in 0..means.len() {
            if means[stage][channel as usize] == 0.0
                || (stage > 0
                    && means[stage][channel as usize] <= means[stage - 1][channel as usize])
            {
                let name = if band == 0 {
                    "signal-1-to-4"
                } else {
                    "signal-5-to-8"
                };
                self.failures.push(format!("SIGNAL LED {} did not produce five distinct increasing sampled PWM levels in {name}: {details:?}",channel+1));
            }
        }
        let failures = &self.failures[failure_start..];
        // Each failed stage has the same channel-level diagnosis and full
        // measurements. Emit it once while retaining all stage evidence.
        self.pending_failure = failures.first().cloned();
        Ok(
            json!({"channel":channel+1,"means":details,"passed":failures.is_empty(),"failures":failures}),
        )
    }
    pub(super) fn take_pending_failure(&mut self) -> Option<String> {
        self.pending_failure.take()
    }
    pub(super) fn finish(&self, options: &Options) -> Result<(Value, Option<String>), String> {
        let evidence = json!({"stimulus":"firmware_shared_channel_level_accumulators","sweeps":[{"group":"signal-1-to-4","accumulator_indices":{"start":0,"end":4},"output_channels":{"start":0,"end":4},"levels":self.samples[0]},{"group":"signal-5-to-8","accumulator_indices":{"start":4,"end":12},"output_channels":{"start":4,"end":8},"levels":self.samples[1]}]});
        fs::write(
            options.logs.join("channel-signal-pwm.json"),
            serde_json::to_vec_pretty(&evidence).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
        let failure = (!self.failures.is_empty()).then(|| self.failures.join("; "));
        Ok((evidence, failure))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scope() -> Scope {
        Scope {
            started: Instant::now(),
            guest_start: 0,
            guest_budget: 60_000,
            wall_budget: 120,
            phase: 3,
            band: 0,
            stages: 5,
            assertions: 0,
            samples: [vec![], vec![]],
            means: [vec![], vec![]],
            failures: vec![],
            pending_failure: None,
        }
    }

    #[test]
    fn channel_assertions_accumulate_nonzero_and_monotonic_failures() {
        let mut scope = scope();
        scope.means[0] = vec![[1.0; 8], [2.0; 8], [3.0; 8], [4.0; 8], [5.0; 8]];
        let passing = scope.assert_channel(0, 0).unwrap();
        assert_eq!(passing["passed"], true);
        assert_eq!(passing["failures"], json!([]));
        assert!(scope.failures.is_empty());
        scope.means[0][0][1] = 0.0;
        scope.means[0][3][1] = scope.means[0][2][1];
        let failing = scope.assert_channel(0, 1).unwrap();
        assert_eq!(failing["passed"], false);
        assert_eq!(failing["channel"], 2);
        assert_eq!(failing["failures"].as_array().unwrap().len(), 2);
        assert_eq!(
            scope.take_pending_failure().as_deref(),
            Some(scope.failures[0].as_str())
        );
        assert_eq!(scope.failures.len(), 2);
        scope.means[0][4][2] = scope.means[0][3][2];
        scope.assert_channel(0, 2).unwrap();
        assert_eq!(scope.failures.len(), 3);
    }
}
