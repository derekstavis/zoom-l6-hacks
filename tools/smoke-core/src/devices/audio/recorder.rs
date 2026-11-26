//! Scoped L6 recorder playback milestones and frozen audio evidence.
use super::*;
use l6ctl::{
    InputSource, L6RecorderBaseline, L6RecorderDma, L6RecorderEvent, L6RecorderSample,
    L6RecorderSelection,
};

pub(in crate::devices) struct Scope {
    started: Instant,
    guest_start: u32,
    guest_budget: u32,
    wall_budget: u64,
    pub phase: u8,
    target: u32,
    held: bool,
    input_before: u64,
    play_guest: u32,
    debugger: Option<l6max_host::debug::Session>,
    reader: l6max_host::l6_recorder::Reader,
    baseline: L6RecorderBaseline,
    event_pc: Option<u32>,
    event: Option<L6RecorderEvent>,
    handler_pc: Option<u32>,
    handler_argument: Option<u32>,
    selection: Option<L6RecorderSelection>,
    callback_pc: Option<u32>,
    samples: Vec<L6RecorderSample>,
    dma: Option<L6RecorderDma>,
    guest_window_ms: u32,
    evidence: Option<Value>,
}
fn expired(
    session: &Session,
    started: Instant,
    guest_start: u32,
    guest_budget: u32,
    wall_budget: u64,
) -> bool {
    if started.elapsed() >= Duration::from_secs(wall_budget) {
        return true;
    }
    l6ctl::OwnedControl::new(&session.engine)
        .input_status(InputSource::Panel)
        .map(|v| v.guest_ms.wrapping_sub(guest_start) > guest_budget)
        .unwrap_or(true)
}
fn combine(primary: String, cleanup: Result<(), String>) -> String {
    match cleanup {
        Ok(()) => primary,
        Err(error) => format!("{primary}; recorder cleanup failed: {error}"),
    }
}
impl Scope {
    pub(in crate::devices) fn begin(
        session: &Session,
        options: &Options,
        target: u32,
        guest_budget: u32,
        wall_budget: u64,
    ) -> Result<Self, String> {
        if session.engine.model() != l6fw::Model::L6 || target != 4 {
            return Err("l6-recorder-playback requires L6 firmware and MAIN target 4".into());
        }
        if std::env::var_os("L6_AUDIO_SAMPLE_TRACE").is_none() {
            return Err("l6-recorder-playback requires L6_AUDIO_SAMPLE_TRACE=1".into());
        }
        if guest_budget < 5000 {
            return Err("l6-recorder-playback guest timeout must be at least 5000 ms".into());
        }
        let started = Instant::now();
        let deadline = started + Duration::from_secs(wall_budget);
        let control = l6ctl::OwnedControl::new(&session.engine);
        let guest_start = control
            .input_status(InputSource::Panel)
            .map_err(|e| e.to_string())?
            .guest_ms;
        let baseline = l6ctl::l6_recorder_baseline(
            &fs::read_to_string(options.logs.join("engine.trace")).map_err(|e| e.to_string())?,
        );
        let mut debugger = control
            .debug_session_before(deadline)
            .map_err(|e| e.to_string())?;
        let setup = (|| {
            let reader = control
                .l6_recorder_reader_before(deadline, || {
                    expired(session, started, guest_start, guest_budget, wall_budget)
                })
                .map_err(|e| e.to_string())?;
            for address in [
                l6::system_event_dispatcher,
                l6::rec_play_handler,
                l6::rec_playback_callback,
            ] {
                if expired(session, started, guest_start, guest_budget, wall_budget) {
                    return Err("recorder shared deadline exceeded".into());
                }
                debugger.breakpoint_before(address, true, deadline)?;
            }
            Ok(reader)
        })();
        let reader = match setup {
            Ok(reader) => reader,
            Err(error) => {
                return Err(combine(
                    error,
                    debugger.close_before(Instant::now() + Duration::from_secs(3)),
                ));
            }
        };
        Ok(Self {
            started,
            guest_start,
            guest_budget,
            wall_budget,
            phase: 0,
            target,
            held: false,
            input_before: 0,
            play_guest: 0,
            debugger: Some(debugger),
            reader,
            baseline,
            event_pc: None,
            event: None,
            handler_pc: None,
            handler_argument: None,
            selection: None,
            callback_pc: None,
            samples: vec![],
            dma: None,
            guest_window_ms: 0,
            evidence: None,
        })
    }
    fn deadline(&self) -> Instant {
        self.started + Duration::from_secs(self.wall_budget)
    }
    fn cancelled(&self, session: &Session) -> bool {
        expired(
            session,
            self.started,
            self.guest_start,
            self.guest_budget,
            self.wall_budget,
        )
    }
    pub(in crate::devices) fn check(
        &self,
        session: &Session,
        guest: u32,
        wall: u64,
    ) -> Result<(), String> {
        if (guest, wall) != (self.guest_budget, self.wall_budget) || self.cancelled(session) {
            Err("recorder shared deadline exceeded or changed".into())
        } else {
            Ok(())
        }
    }
    fn dbg(&mut self) -> Result<&mut l6max_host::debug::Session, String> {
        self.debugger
            .as_mut()
            .ok_or("recorder debugger not active".into())
    }
    pub(in crate::devices) fn press(&mut self, session: &Session) -> Result<Value, String> {
        let c = l6ctl::OwnedControl::new(&session.engine);
        self.play_guest = c
            .input_status(InputSource::Panel)
            .map_err(|e| e.to_string())?
            .guest_ms;
        self.input_before = c
            .input_status(InputSource::Main)
            .map_err(|e| e.to_string())?
            .applied;
        self.held = true;
        let sequence = c
            .down_with_sequence(self.target, 10_000)
            .map_err(|e| e.to_string())?;
        loop {
            self.check(session, self.guest_budget, self.wall_budget)?;
            session.poll()?;
            let status = c
                .input_status(InputSource::Main)
                .map_err(|e| e.to_string())?;
            if !status.connected {
                return Err("MAIN input disconnected during recorder Play".into());
            }
            if status.applied > self.input_before {
                break;
            }
            thread::sleep(Duration::from_millis(5));
        }
        Ok(json!({"target":self.target,"sequence":sequence}))
    }
    fn continue_to_stop(&mut self, session: &Session) -> Result<(), String> {
        let deadline = self
            .deadline()
            .min(Instant::now() + Duration::from_secs(10));
        let (started, guest_start, guest_budget, wall_budget) = (
            self.started,
            self.guest_start,
            self.guest_budget,
            self.wall_budget,
        );
        self.dbg()?
            .continue_before(deadline, || {
                expired(session, started, guest_start, guest_budget, wall_budget)
            })
            .map(|_| ())
    }
    pub(in crate::devices) fn dispatcher(&mut self, session: &Session) -> Result<Value, String> {
        self.continue_to_stop(session)?;
        let deadline = self.deadline();
        let pc = self.dbg()?.register_before(15, deadline)? & !1;
        let pointer = self.dbg()?.register_before(0, deadline)?;
        let event = self
            .reader
            .event(pointer, deadline, || self.cancelled(session))?;
        self.event_pc = Some(pc);
        self.event = Some(event.clone());
        Ok(json!({"pc":pc,"event":event}))
    }
    pub(in crate::devices) fn assert_event(&self) -> Result<Value, String> {
        let pc = self.event_pc.ok_or("missing dispatcher PC")?;
        let event = self.event.as_ref().ok_or("missing event")?;
        if pc != l6::system_event_dispatcher {
            return Err(format!(
                "L6 Play event did not reach class-0 dispatcher (pc={pc:#010x}, event={:02x?})",
                event.bytes
            ));
        }
        if (event.state, event.class, event.code) != (0, 0, 0x20) {
            return Err(format!(
                "L6 class-0 dispatcher received an unexpected event (state={}, class={}, code={:#x}, bytes={:02x?})",
                event.state, event.class, event.code, event.bytes
            ));
        }
        Ok(json!({"state":event.state,"class":event.class,"code":event.code}))
    }
    pub(in crate::devices) fn handler(&mut self, session: &Session) -> Result<Value, String> {
        let deadline = self.deadline();
        self.dbg()?
            .breakpoint_before(l6::system_event_dispatcher, false, deadline)?;
        self.continue_to_stop(session)?;
        let pc = self.dbg()?.register_before(15, deadline)? & !1;
        let argument = self.dbg()?.register_before(0, deadline)?;
        let selection = self
            .reader
            .selection(deadline, || self.cancelled(session))?;
        self.handler_pc = Some(pc);
        self.handler_argument = Some(argument);
        self.selection = Some(selection.clone());
        Ok(json!({"pc":pc,"argument":argument,"selection":selection}))
    }
    pub(in crate::devices) fn assert_selection(&self) -> Result<Value, String> {
        let pc = self.handler_pc.ok_or("missing handler PC")?;
        let argument = self.handler_argument.ok_or("missing handler argument")?;
        let selection = self.selection.as_ref().ok_or("missing selection")?;
        if pc != l6::rec_play_handler
            || argument != 1
            || selection.mode != 0
            || selection.key.iter().all(|byte| *byte == 0)
        {
            return Err(format!(
                "L6 Play handler did not accept the selected mode-0 recording (pc={pc:#010x}, argument={argument}, mode={}, selection={:02x?})",
                selection.mode, selection.key
            ));
        }
        Ok(json!({"mode":selection.mode,"selection_key":selection.key}))
    }
    pub(in crate::devices) fn callback(&mut self, session: &Session) -> Result<Value, String> {
        let deadline = self.deadline();
        self.dbg()?
            .breakpoint_before(l6::rec_play_handler, false, deadline)?;
        self.continue_to_stop(session)?;
        let pc = self.dbg()?.register_before(15, deadline)? & !1;
        self.callback_pc = Some(pc);
        Ok(json!({"pc":pc}))
    }
    pub(in crate::devices) fn assert_callback(&self) -> Result<Value, String> {
        let pc = self.callback_pc.ok_or("missing callback PC")?;
        if pc != l6::rec_playback_callback {
            return Err(format!(
                "L6 recorder playback callback stopped at {pc:#010x}, expected {:#010x}",
                l6::rec_playback_callback
            ));
        }
        Ok(json!({"pc":pc}))
    }
    pub(in crate::devices) fn observe(&mut self, session: &Session) -> Result<Value, String> {
        let deadline = self.deadline();
        self.dbg()?
            .breakpoint_before(l6::rec_playback_callback, false, deadline)?;
        self.check(session, self.guest_budget, self.wall_budget)?;
        let mut debugger = self.debugger.take().ok_or("recorder debugger not active")?;
        debugger.close_before(deadline)?;
        let start = l6ctl::OwnedControl::new(&session.engine)
            .input_status(InputSource::Panel)
            .map_err(|e| e.to_string())?
            .guest_ms;
        loop {
            self.check(session, self.guest_budget, self.wall_budget)?;
            let now = l6ctl::OwnedControl::new(&session.engine)
                .input_status(InputSource::Panel)
                .map_err(|e| e.to_string())?
                .guest_ms;
            if now.wrapping_sub(start) >= 5000 {
                return Ok(json!({"observed_guest_ms":now.wrapping_sub(start)}));
            }
            session.poll()?;
            thread::sleep(Duration::from_millis(5));
        }
    }
    pub(in crate::devices) fn release(&mut self, session: &Session) -> Result<Value, String> {
        let c = l6ctl::OwnedControl::new(&session.engine);
        let before = c
            .input_status(InputSource::Main)
            .map_err(|e| e.to_string())?
            .applied;
        let sequence = c.up_with_sequence(self.target).map_err(|e| e.to_string())?;
        loop {
            self.check(session, self.guest_budget, self.wall_budget)?;
            session.poll()?;
            if c.input_status(InputSource::Main)
                .map_err(|e| e.to_string())?
                .applied
                > before
            {
                self.held = false;
                return Ok(json!({"sequence":sequence}));
            }
            thread::sleep(Duration::from_millis(5));
        }
    }
    pub(in crate::devices) fn sample(
        &mut self,
        session: &Session,
        options: &Options,
    ) -> Result<Value, String> {
        let control = l6ctl::OwnedControl::new(&session.engine);
        let mut debugger = control
            .debug_session_before(self.deadline())
            .map_err(|e| e.to_string())?;
        let capture = (|| {
            let dma = self
                .reader
                .dma(self.deadline(), || self.cancelled(session))?;
            let trace =
                fs::read_to_string(options.logs.join("engine.trace")).map_err(|e| e.to_string())?;
            let samples = l6ctl::l6_recorder_samples(&trace, &self.baseline);
            let guest = control
                .input_status(InputSource::Panel)
                .map_err(|e| e.to_string())?
                .guest_ms;
            Ok::<_, String>((dma, samples, guest.wrapping_sub(self.play_guest)))
        })();
        let cleanup = debugger.close_before(self.deadline());
        let (dma, samples, guest_window) = match capture {
            Ok(v) => {
                cleanup?;
                v
            }
            Err(error) => return Err(combine(error, cleanup)),
        };
        self.dma = Some(dma);
        self.samples = samples;
        self.guest_window_ms = guest_window;
        Ok(json!({"samples":self.samples.len()}))
    }
    pub(in crate::devices) fn save(&mut self, options: &Options) -> Result<Value, String> {
        let event = self.event.as_ref().ok_or("missing event")?;
        let selection = self.selection.as_ref().ok_or("missing selection")?;
        let dma = self.dma.as_ref().ok_or("missing DMA sample")?;
        let changed = l6ctl::l6_recorder_signature_changed(&self.baseline, &self.samples);
        let evidence = json!({"target":self.target,"guest_window_ms":self.guest_window_ms,"dispatched_event_pointer":event.pointer,"dispatched_event":event.bytes,"handler_argument":self.handler_argument.ok_or("missing handler argument")?,"recorder_mode":selection.mode,"selection_key":selection.key,"callback_pc":self.callback_pc.ok_or("missing callback PC")?,"post_press_settle_ms":5000,"baseline_signature":self.baseline.signature,"samples":self.samples,"tx_sample_signature_changed_after_press":changed,"channel_14_tcd_source":dma.source,"channel_14_source_words":dma.source_words,"channel_14_source_has_audio":dma.source_has_audio});
        fs::write(
            options.logs.join("recorder-playback.json"),
            serde_json::to_vec_pretty(&evidence).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
        self.evidence = Some(evidence.clone());
        Ok(evidence)
    }
    pub(in crate::devices) fn assert_trace(&self) -> Result<Value, String> {
        let evidence = self.evidence.as_ref().ok_or("missing recorder evidence")?;
        if evidence["tx_sample_signature_changed_after_press"] != true {
            return Err(format!(
                "L6 recorder playback TX signature did not change: {evidence}"
            ));
        }
        Ok(json!({"changed":true}))
    }
    pub(in crate::devices) fn assert_dma(&self) -> Result<Value, String> {
        let evidence = self.evidence.as_ref().ok_or("missing recorder evidence")?;
        if evidence["channel_14_source_has_audio"] != true {
            return Err(format!(
                "L6 recorder playback DMA source has no audio: {evidence}"
            ));
        }
        Ok(json!({"source_has_audio":true}))
    }
    pub(in crate::devices) fn abort(&mut self, session: &Session) -> Result<(), String> {
        let mut errors = Vec::new();
        if self.held {
            match l6ctl::OwnedControl::new(&session.engine).up_with_sequence(self.target) {
                Ok(_) => self.held = false,
                Err(error) => errors.push(format!("release PLAY: {error}")),
            }
        }
        if let Some(mut debugger) = self.debugger.take() {
            if let Err(error) = debugger.close_before(Instant::now() + Duration::from_secs(3)) {
                errors.push(error);
            }
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors.join("; "))
        }
    }
}
