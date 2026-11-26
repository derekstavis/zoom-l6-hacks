//! Scoped L6max recorder milestones with one held debugger lease.
use super::*;
use l6ctl::InputSource;

pub(in crate::devices) struct Scope {
    start: Instant,
    guest_start: u32,
    guest_budget: u32,
    wall_budget: u64,
    pub phase: u8,
    target: u32,
    held: bool,
    debugger: Option<l6max_host::debug::Session>,
    reader: l6max_host::l6max_recorder::Reader,
    baseline: l6max_host::audio_trace::L6RecorderBaseline,
    fields: Vec<u32>,
    callback: u32,
    action: u32,
    stream: bool,
    evidence: Option<Value>,
}
fn cancelled(session: &Session, start: Instant, guest_start: u32, guest: u32, wall: u64) -> bool {
    start.elapsed() >= Duration::from_secs(wall)
        || l6ctl::OwnedControl::new(&session.engine)
            .input_status(InputSource::Panel)
            .map(|s| s.guest_ms.wrapping_sub(guest_start) > guest)
            .unwrap_or(true)
}
fn bounded_observation<T>(
    deadline: Instant,
    mut now: impl FnMut() -> Instant,
    operation: impl FnOnce() -> Result<T, String>,
) -> Result<T, String> {
    if now() >= deadline {
        return Err("scheduler observation deadline exceeded".into());
    }
    let value = operation()?;
    if now() >= deadline {
        return Err("scheduler observation deadline exceeded".into());
    }
    Ok(value)
}
fn merge(error: String, cleanup: Result<(), String>) -> String {
    match cleanup {
        Ok(()) => error,
        Err(e) => format!("{error}; recorder cleanup failed: {e}"),
    }
}
impl Scope {
    pub(in crate::devices) fn begin(
        session: &Session,
        options: &Options,
        target: u32,
        guest: u32,
        wall: u64,
    ) -> Result<Self, String> {
        if session.engine.model() != l6fw::Model::L6max || target != 4 {
            return Err("L6max recorder requires MAIN target 4".into());
        }
        if std::env::var_os("L6_AUDIO_SAMPLE_TRACE").is_none() || guest < 10000 {
            return Err("L6max recorder requires audio tracing and at least 10000 guest ms".into());
        }
        let start = Instant::now();
        let deadline = start + Duration::from_secs(wall);
        let c = l6ctl::OwnedControl::new(&session.engine);
        let guest_start = c
            .input_status(InputSource::Panel)
            .map_err(|e| e.to_string())?
            .guest_ms;
        let trace =
            fs::read_to_string(options.logs.join("engine.trace")).map_err(|e| e.to_string())?;
        let baseline = l6max_host::audio_trace::l6_recorder_baseline(&trace);
        let mut debugger = c
            .debug_session_before(deadline)
            .map_err(|e| e.to_string())?;
        let setup = (|| {
            let reader = c.l6max_recorder_reader_before(deadline, || {
                cancelled(session, start, guest_start, guest, wall)
            })?;
            debugger.breakpoint_before(l6max::play_event_submit, true, deadline)?;
            Ok::<_, String>(reader)
        })();
        let reader = match setup {
            Ok(r) => r,
            Err(e) => {
                return Err(merge(
                    e,
                    debugger.close_before(Instant::now() + Duration::from_secs(3)),
                ));
            }
        };
        Ok(Self {
            start,
            guest_start,
            guest_budget: guest,
            wall_budget: wall,
            phase: 0,
            target,
            held: false,
            debugger: Some(debugger),
            reader,
            baseline,
            fields: vec![],
            callback: 0,
            action: 0,
            stream: false,
            evidence: None,
        })
    }
    fn deadline(&self) -> Instant {
        self.start + Duration::from_secs(self.wall_budget)
    }
    fn cancelled(&self, s: &Session) -> bool {
        cancelled(
            s,
            self.start,
            self.guest_start,
            self.guest_budget,
            self.wall_budget,
        )
    }
    pub(in crate::devices) fn check(&self, s: &Session, g: u32, w: u64) -> Result<(), String> {
        if (g, w) != (self.guest_budget, self.wall_budget) || self.cancelled(s) {
            Err("L6max recorder shared deadline exceeded or changed".into())
        } else {
            Ok(())
        }
    }
    fn dbg(&mut self) -> Result<&mut l6max_host::debug::Session, String> {
        self.debugger
            .as_mut()
            .ok_or("recorder debugger missing".into())
    }
    fn resume(&mut self, s: &Session, seconds: u64, optional: bool) -> Result<bool, String> {
        let deadline = self
            .deadline()
            .min(Instant::now() + Duration::from_secs(seconds));
        let (start, guest_start, guest, wall) = (
            self.start,
            self.guest_start,
            self.guest_budget,
            self.wall_budget,
        );
        let stop = self
            .dbg()?
            .continue_optional_before(deadline, || cancelled(s, start, guest_start, guest, wall))?;
        if stop.is_none() && !optional {
            return Err("recorder breakpoint timeout".into());
        }
        Ok(stop.is_some())
    }
    fn switch(&mut self, old: u32, new: u32) -> Result<(), String> {
        let d = self.deadline();
        self.dbg()?.breakpoint_before(old, false, d)?;
        self.dbg()?.breakpoint_before(new, true, d)
    }
    fn pc(&mut self) -> Result<u32, String> {
        let d = self.deadline();
        Ok(self.dbg()?.register_before(15, d)? & !1)
    }
    pub(in crate::devices) fn press(&mut self, s: &Session) -> Result<Value, String> {
        let c = l6ctl::OwnedControl::new(&s.engine);
        let before = c
            .input_status(InputSource::Main)
            .map_err(|e| e.to_string())?
            .applied;
        self.held = true;
        let seq = c
            .down_with_sequence(self.target, 10000)
            .map_err(|e| e.to_string())?;
        let end = Instant::now() + Duration::from_secs(self.wall_budget.min(30));
        loop {
            self.check(s, self.guest_budget, self.wall_budget)?;
            if Instant::now() >= end {
                return Err("L6max Play press not acknowledged".into());
            };
            s.poll()?;
            let x = c
                .input_status(InputSource::Main)
                .map_err(|e| e.to_string())?;
            if !x.connected {
                return Err("MAIN input disconnected".into());
            }
            if x.applied > before {
                break;
            }
            thread::sleep(Duration::from_millis(5));
        }
        Ok(json!({"sequence":seq}))
    }
    pub(in crate::devices) fn submit(&mut self, s: &Session) -> Result<Value, String> {
        self.resume(s, 10, false)?;
        let pc = self.pc()?;
        let d = self.deadline();
        let mut args = vec![];
        for i in 0..4 {
            args.push(self.dbg()?.register_before(i, d)?)
        }
        if pc != l6max::play_event_submit || args[2] != 0x2c {
            return Err(format!(
                "unexpected Play submission pc={pc:#x} args={args:?}"
            ));
        };
        Ok(json!({"pc":pc,"args":args}))
    }
    pub(in crate::devices) fn queue(&mut self, s: &Session) -> Result<Value, String> {
        self.switch(l6max::play_event_submit, l6max::play_event_send_return)?;
        self.resume(s, 5, false)?;
        let pc = self.pc()?;
        let d = self.deadline();
        let result = self.dbg()?.register_before(0, d)?;
        if pc != l6max::play_event_send_return || result != 0 {
            return Err(format!(
                "Play queue submission failed pc={pc:#x} result={result:#x}"
            ));
        };
        Ok(json!({"pc":pc,"result":result}))
    }
    pub(in crate::devices) fn dispatch(&mut self, s: &Session) -> Result<Value, String> {
        self.switch(
            l6max::play_event_send_return,
            l6max::panel_handle_main_button_event,
        )?;
        self.resume(s, 10, false)?;
        let pc = self.pc()?;
        let d = self.deadline();
        let ptr = self.dbg()?.register_before(0, d)?;
        if pc != l6max::panel_handle_main_button_event {
            return Err(format!("unexpected event handler {pc:#x}"));
        }
        let fields = self.reader.event(ptr, d, || self.cancelled(s))?;
        if fields.first() != Some(&0) || fields.get(2) != Some(&0x2c) {
            return Err(format!("unexpected Play event {fields:08x?}"));
        }
        self.fields = fields.clone();
        Ok(json!({"event":fields}))
    }
    pub(in crate::devices) fn scheduler(&mut self, s: &Session) -> Result<Value, String> {
        self.switch(
            l6max::panel_handle_main_button_event,
            l6max::gui_callback_schedule,
        )?;
        let deadline = self
            .deadline()
            .min(Instant::now() + Duration::from_secs(10));
        loop {
            self.check(s, self.guest_budget, self.wall_budget)?;
            if Instant::now() >= deadline {
                return Err("Play did not schedule callback slot 2".into());
            }
            let (start, gs, g, w) = (
                self.start,
                self.guest_start,
                self.guest_budget,
                self.wall_budget,
            );
            self.dbg()?
                .continue_before(deadline, || cancelled(s, start, gs, g, w))?;
            let pc = bounded_observation(deadline, Instant::now, || {
                self.dbg()?.register_before(15, deadline)
            })? & !1;
            if pc != l6max::gui_callback_schedule {
                return Err(format!("unexpected scheduler stop {pc:#x}"));
            }
            let slot = bounded_observation(deadline, Instant::now, || {
                self.dbg()?.register_before(0, deadline)
            })?;
            let caller = bounded_observation(deadline, Instant::now, || {
                self.dbg()?.register_before(14, deadline)
            })? & !1;
            if slot == 2
                && (l6max::panel_handle_main_button_event
                    ..l6max::panel_handle_main_button_event_end)
                    .contains(&caller)
            {
                self.callback = bounded_observation(deadline, Instant::now, || {
                    self.reader.callback_target(deadline, || self.cancelled(s))
                })?;
                break;
            }
            self.check(s, self.guest_budget, self.wall_budget)?;
            self.dbg()?
                .breakpoint_before(l6max::gui_callback_schedule, false, deadline)?;
            self.dbg()?
                .step_before(deadline, || cancelled(s, start, gs, g, w))?;
            self.dbg()?
                .breakpoint_before(l6max::gui_callback_schedule, true, deadline)?;
        }
        Ok(json!({"callback_target":self.callback}))
    }
    pub(in crate::devices) fn callback(&mut self, s: &Session) -> Result<Value, String> {
        self.switch(l6max::gui_callback_schedule, self.callback)?;
        self.resume(s, 5, false)?;
        let pc = self.pc()?;
        if pc != self.callback {
            return Err(format!("unexpected callback {pc:#x}"));
        }
        Ok(json!({"pc":pc}))
    }
    pub(in crate::devices) fn action(&mut self, s: &Session) -> Result<Value, String> {
        self.switch(self.callback, l6max::rec_play_action)?;
        self.resume(s, 5, false)?;
        let pc = self.pc()?;
        if pc != l6max::rec_play_action {
            return Err(format!("unexpected recorder action {pc:#x}"));
        }
        self.action = pc;
        Ok(json!({"pc":pc}))
    }
    pub(in crate::devices) fn stream(&mut self, s: &Session) -> Result<Value, String> {
        self.switch(l6max::rec_play_action, l6max::rec_stream_read)?;
        self.stream = self.resume(s, 10, true)?;
        if self.stream {
            let pc = self.pc()?;
            if pc != l6max::rec_stream_read {
                return Err(format!("unexpected recorder stream {pc:#x}"));
            }
        }
        Ok(json!({"stream_read_reached":self.stream}))
    }
    pub(in crate::devices) fn observe(&mut self, s: &Session) -> Result<Value, String> {
        let d = self.deadline();
        self.dbg()?
            .breakpoint_before(l6max::rec_stream_read, false, d)?;
        self.check(s, self.guest_budget, self.wall_budget)?;
        self.debugger
            .take()
            .ok_or("debugger missing")?
            .close_before(d)?;
        let start = l6ctl::OwnedControl::new(&s.engine)
            .input_status(InputSource::Panel)
            .map_err(|e| e.to_string())?
            .guest_ms;
        loop {
            self.check(s, self.guest_budget, self.wall_budget)?;
            let now = l6ctl::OwnedControl::new(&s.engine)
                .input_status(InputSource::Panel)
                .map_err(|e| e.to_string())?
                .guest_ms;
            if now.wrapping_sub(start) >= 10000 {
                return Ok(json!({"observed_guest_ms":now.wrapping_sub(start)}));
            }
            s.poll()?;
            thread::sleep(Duration::from_millis(5));
        }
    }
    pub(in crate::devices) fn sample(
        &mut self,
        s: &Session,
        options: &Options,
    ) -> Result<Value, String> {
        let trace =
            fs::read_to_string(options.logs.join("engine.trace")).map_err(|e| e.to_string())?;
        let summary = l6max_host::audio_trace::l6max_recorder_summary(&trace, &self.baseline);
        let c = l6ctl::OwnedControl::new(&s.engine);
        let mut debug = c
            .debug_session_before(self.deadline())
            .map_err(|e| e.to_string())?;
        let sample = (|| {
            let (source, words) = self.reader.dma(self.deadline(), || self.cancelled(s))?;
            let guest = c
                .input_status(InputSource::Panel)
                .map_err(|e| e.to_string())?
                .guest_ms;
            Ok::<_, String>((source, words, guest))
        })();
        let cleanup = debug.close_before(self.deadline());
        let (source, words, guest) = match sample {
            Ok(v) => {
                cleanup?;
                v
            }
            Err(e) => return Err(merge(e, cleanup)),
        };
        let audio = words.iter().any(|v| *v != 0 && *v != u32::MAX);
        self.evidence = Some(
            json!({"model":"l6max","target":self.target,"event":0x2c,"event_fields":self.fields,"callback_target":self.callback,"recorder_action":self.action,"stream_read_reached":self.stream,"guest_ms":guest,"tx_sample_windows":summary.windows,"nonzero_sample_windows":summary.nonzero_windows,"tx_signature_changed":summary.signature_changed,"channel14_source":source,"channel14_source_words":words,"source_has_audio":audio}),
        );
        Ok(json!({"sampled":true}))
    }
    pub(in crate::devices) fn save(&self, options: &Options) -> Result<Value, String> {
        let e = self.evidence.as_ref().ok_or("recorder evidence missing")?;
        fs::write(
            options.logs.join("l6max-recorder-playback.json"),
            serde_json::to_vec_pretty(e).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
        Ok(e.clone())
    }
    pub(in crate::devices) fn assert_audio(&self) -> Result<Value, String> {
        let e = self.evidence.as_ref().ok_or("recorder evidence missing")?;
        if e["stream_read_reached"] != true
            || e["nonzero_sample_windows"].as_u64().unwrap_or(0) == 0
            || e["tx_signature_changed"] != true
            || e["source_has_audio"] != true
        {
            return Err(format!("L6max recorder produced no verified audio: {e}"));
        }
        Ok(json!({"verified":true}))
    }
    pub(in crate::devices) fn abort(&mut self, s: &Session) -> Result<(), String> {
        let mut errors = vec![];
        if self.held {
            match l6ctl::OwnedControl::new(&s.engine).up_with_sequence(self.target) {
                Ok(_) => self.held = false,
                Err(e) => errors.push(format!("release PLAY: {e}")),
            }
        }
        if let Some(mut d) = self.debugger.take() {
            if let Err(e) = d.close_before(Instant::now() + Duration::from_secs(3)) {
                errors.push(e)
            }
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors.join("; "))
        }
    }
}

#[cfg(test)]
mod deadline_tests {
    use super::*;
    #[test]
    fn delayed_read_rejects_result_and_skips_next_operation() {
        let start = Instant::now();
        let end = start + Duration::from_millis(10);
        let mut clock = [start, end].into_iter();
        let mut calls = 0;
        assert!(
            bounded_observation(
                end,
                || clock.next().unwrap(),
                || {
                    calls += 1;
                    Ok(())
                }
            )
            .is_err()
        );
        assert!(
            bounded_observation(
                end,
                || end,
                || {
                    calls += 1;
                    Ok(())
                }
            )
            .is_err()
        );
        assert_eq!(calls, 1);
    }
}
