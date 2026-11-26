//! Scoped L6 input transport and panel UART observations.
use super::*;
use l6ctl::InputSource;

fn ensure_wait_open(deadline: Instant, now: Instant, what: &str) -> Result<(), String> {
    if now >= deadline {
        Err(format!("timed out waiting for L6 {what}"))
    } else {
        Ok(())
    }
}

fn guarded_poll(
    deadline: Instant,
    now: Instant,
    what: &str,
    poll: impl FnOnce() -> Result<bool, String>,
) -> Result<bool, String> {
    ensure_wait_open(deadline, now, what)?;
    poll()
}

pub(super) struct Scope {
    started: Instant,
    guest_start: u32,
    guest_budget: u32,
    wall_budget: u64,
    pub phase: u8,
    pub target: Option<u32>,
    held: Option<u32>,
    panel_applied: u64,
    panel_released: u64,
    presses: usize,
    releases: usize,
    encoder_before: usize,
    panel_counts: Option<(usize, usize)>,
    encoder_count: Option<usize>,
    encoder_applied: u64,
    wait_deadline: Option<Instant>,
    encoders: Vec<u32>,
    verified: Vec<u32>,
}
impl Scope {
    pub(super) fn begin(
        session: &Session,
        guest_budget: u32,
        wall_budget: u64,
    ) -> Result<Self, String> {
        if session.engine.model() != l6fw::Model::L6 {
            return Err("L6 input sweep requires L6".into());
        }
        let guest_start = l6ctl::OwnedControl::new(&session.engine)
            .input_status(InputSource::Panel)
            .map_err(|e| e.to_string())?
            .guest_ms;
        Ok(Self {
            started: Instant::now(),
            guest_start,
            guest_budget,
            wall_budget,
            phase: 0,
            target: None,
            held: None,
            panel_applied: 0,
            panel_released: 0,
            presses: 0,
            releases: 0,
            encoder_before: 0,
            panel_counts: None,
            encoder_count: None,
            encoder_applied: 0,
            wait_deadline: None,
            encoders: vec![],
            verified: vec![],
        })
    }
    pub(super) fn check(&self, session: &Session, guest: u32, wall: u64) -> Result<(), String> {
        let now = l6ctl::OwnedControl::new(&session.engine)
            .input_status(InputSource::Panel)
            .map_err(|e| e.to_string())?
            .guest_ms;
        if (guest, wall) != (self.guest_budget, self.wall_budget)
            || self.started.elapsed() >= Duration::from_secs(self.wall_budget)
            || now.wrapping_sub(self.guest_start) > self.guest_budget
        {
            return Err("L6 input sweep shared deadline exceeded or changed".into());
        }
        Ok(())
    }
    fn count(options: &Options, byte: u8) -> Result<usize, String> {
        let trace =
            fs::read_to_string(options.logs.join("engine.trace")).map_err(|e| e.to_string())?;
        Ok(l6ctl::panel_uart_count(&trace, byte))
    }
    fn wait(
        &self,
        session: &Session,
        what: &str,
        ready: impl FnMut() -> Result<bool, String>,
    ) -> Result<(), String> {
        self.wait_at(
            session,
            what,
            Instant::now() + Duration::from_secs(10),
            ready,
        )
    }
    fn wait_at(
        &self,
        session: &Session,
        what: &str,
        deadline: Instant,
        mut ready: impl FnMut() -> Result<bool, String>,
    ) -> Result<(), String> {
        ensure_wait_open(deadline, Instant::now(), what)?;
        loop {
            if guarded_poll(deadline, Instant::now(), what, || {
                self.check(session, self.guest_budget, self.wall_budget)?;
                session.poll()?;
                ready()
            })? {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(format!("timed out waiting for L6 {what}"));
            }
            thread::sleep(Duration::from_millis(5));
        }
    }
    pub(super) fn reject_main(&self, session: &Session, target: u32) -> Result<Value, String> {
        if !matches!(target, 0 | 5) {
            return Err("unsupported MAIN test target must be 0 or 5".into());
        }
        let c = l6ctl::OwnedControl::new(&session.engine);
        let before = c
            .input_status(InputSource::Main)
            .map_err(|e| e.to_string())?
            .rejected;
        if before != if target == 0 { 0 } else { 1 } {
            return Err("unexpected MAIN rejection count before input".into());
        }
        c.raw_tap(InputSource::Main, target, 120)
            .map_err(|e| e.to_string())?;
        self.wait(session, "unsupported MAIN target rejection", || {
            Ok(c.input_status(InputSource::Main)
                .map_err(|e| e.to_string())?
                .rejected
                > before)
        })?;
        let main = c
            .input_status(InputSource::Main)
            .map_err(|e| e.to_string())?
            .rejected;
        let panel = c
            .input_status(InputSource::Panel)
            .map_err(|e| e.to_string())?
            .rejected;
        if (main, panel) != (before + 1, 0) {
            return Err("unexpected rejection count after MAIN input".into());
        }
        Ok(json!({"target":target,"rejections":{"main":main,"panel":panel}}))
    }
    pub(super) fn panel_baseline(
        &mut self,
        session: &Session,
        options: &Options,
        target: u32,
    ) -> Result<Value, String> {
        self.presses = Self::count(options, 0x91)?;
        self.releases = Self::count(options, 0x90)?;
        let p = l6ctl::OwnedControl::new(&session.engine)
            .input_status(InputSource::Panel)
            .map_err(|e| e.to_string())?;
        self.panel_applied = p.applied;
        self.panel_released = p.released;
        self.target = Some(target);
        Ok(json!({"target":target}))
    }
    pub(super) fn panel_tap(&mut self, session: &Session) -> Result<Value, String> {
        let target = self.target.ok_or("missing panel target")?;
        self.held = Some(target);
        let sequence = l6ctl::OwnedControl::new(&session.engine)
            .raw_tap(InputSource::Panel, target, 120)
            .map_err(|e| e.to_string())?;
        self.wait_deadline = Some(Instant::now() + Duration::from_secs(10));
        Ok(json!({"target":target,"sequence":sequence}))
    }
    pub(super) fn panel_wait(&mut self, session: &Session) -> Result<Value, String> {
        let c = l6ctl::OwnedControl::new(&session.engine);
        self.wait_at(
            session,
            "panel tap application and release",
            self.wait_deadline.ok_or("missing panel wait deadline")?,
            || {
                let now = c
                    .input_status(InputSource::Panel)
                    .map_err(|e| e.to_string())?;
                Ok(now.applied > self.panel_applied && now.released > self.panel_released)
            },
        )?;
        self.held = None;
        self.wait_deadline = Some(Instant::now() + Duration::from_secs(10));
        Ok(json!({"applied":true,"released":true}))
    }
    pub(super) fn panel_trace(
        &mut self,
        session: &Session,
        options: &Options,
    ) -> Result<Value, String> {
        let target = self.target.ok_or("missing panel target")?;
        self.wait_at(
            session,
            &format!("panel input {target} release report"),
            self.wait_deadline.ok_or("missing panel trace deadline")?,
            || Ok(Self::count(options, 0x90)? >= self.releases + 1),
        )?;
        let presses = Self::count(options, 0x91)?.saturating_sub(self.presses);
        let releases = Self::count(options, 0x90)?.saturating_sub(self.releases);
        self.panel_counts = Some((presses, releases));
        Ok(json!({"release_seen":true}))
    }
    pub(super) fn panel_assert(&mut self, _options: &Options) -> Result<Value, String> {
        let target = self.target.take().ok_or("missing panel target")?;
        let (presses, releases) = self
            .panel_counts
            .take()
            .ok_or("missing panel trace sample")?;
        if (presses, releases) != (1, 1) {
            return Err(format!(
                "L6 panel input {target} emitted {presses} press and {releases} release reports"
            ));
        }
        self.verified.push(target);
        Ok(json!({"target":target,"presses":presses,"releases":releases}))
    }
    pub(super) fn encoder_baseline(&mut self, options: &Options) -> Result<Value, String> {
        self.encoder_before = Self::count(options, 0xa1)?;
        Ok(json!({"started":true}))
    }
    pub(super) fn encoder_send(&mut self, session: &Session, index: u32) -> Result<Value, String> {
        if index >= 6 {
            return Err("encoder test index must be 0..5".into());
        }
        let c = l6ctl::OwnedControl::new(&session.engine);
        self.encoder_applied = c
            .input_status(InputSource::Panel)
            .map_err(|e| e.to_string())?
            .applied;
        let seq = c.raw_panel_encoder(index, 1).map_err(|e| e.to_string())?;
        self.target = Some(index);
        self.wait_deadline = Some(Instant::now() + Duration::from_secs(10));
        Ok(json!({"encoder":index,"sequence":seq}))
    }
    pub(super) fn encoder_wait(&mut self, session: &Session) -> Result<Value, String> {
        let c = l6ctl::OwnedControl::new(&session.engine);
        self.wait_at(
            session,
            "encoder input application",
            self.wait_deadline.ok_or("missing encoder wait deadline")?,
            || {
                Ok(c.input_status(InputSource::Panel)
                    .map_err(|e| e.to_string())?
                    .applied
                    > self.encoder_applied)
            },
        )?;
        let index = self.target.take().ok_or("missing encoder")?;
        self.encoders.push(index);
        if self.encoders.len() == 6 {
            self.wait_deadline = Some(Instant::now() + Duration::from_secs(10));
        }
        Ok(json!({"encoder":index}))
    }
    pub(super) fn encoder_trace(
        &mut self,
        session: &Session,
        options: &Options,
    ) -> Result<Value, String> {
        self.wait_at(
            session,
            "six L6 encoder reports",
            self.wait_deadline.ok_or("missing encoder trace deadline")?,
            || Ok(Self::count(options, 0xa1)?.saturating_sub(self.encoder_before) >= 6),
        )?;
        self.encoder_count = Some(Self::count(options, 0xa1)?.saturating_sub(self.encoder_before));
        Ok(json!({"reports_ready":true}))
    }
    pub(super) fn encoder_assert(&self, _options: &Options) -> Result<Value, String> {
        let count = self.encoder_count.ok_or("missing encoder trace sample")?;
        if count != 6 {
            return Err(format!("expected six L6 encoder reports, got {count}"));
        }
        Ok(json!({"encoder_reports":count}))
    }
    pub(super) fn reject_panel(&self, session: &Session) -> Result<Value, String> {
        let c = l6ctl::OwnedControl::new(&session.engine);
        let before = c
            .input_status(InputSource::Panel)
            .map_err(|e| e.to_string())?
            .rejected;
        if before != 0 {
            return Err("unexpected panel rejection count before input".into());
        }
        c.raw_down(InputSource::Panel, 31, 50)
            .map_err(|e| e.to_string())?;
        self.wait(session, "unsupported panel target rejection", || {
            Ok(c.input_status(InputSource::Panel)
                .map_err(|e| e.to_string())?
                .rejected
                > before)
        })?;
        let main = c
            .input_status(InputSource::Main)
            .map_err(|e| e.to_string())?
            .rejected;
        let panel = c
            .input_status(InputSource::Panel)
            .map_err(|e| e.to_string())?
            .rejected;
        if (main, panel) != (2, 1) {
            return Err("unexpected rejection count after panel input".into());
        }
        Ok(json!({"target":31,"rejections":{"main":main,"panel":panel}}))
    }
    pub(super) fn reject_encoder(&self, session: &Session) -> Result<Value, String> {
        let c = l6ctl::OwnedControl::new(&session.engine);
        let before = c
            .input_status(InputSource::Panel)
            .map_err(|e| e.to_string())?
            .rejected;
        if before != 1 {
            return Err("unexpected panel rejection count before encoder".into());
        }
        c.raw_panel_encoder(6, 1).map_err(|e| e.to_string())?;
        self.wait(session, "unsupported encoder rejection", || {
            Ok(c.input_status(InputSource::Panel)
                .map_err(|e| e.to_string())?
                .rejected
                > before)
        })?;
        let main = c
            .input_status(InputSource::Main)
            .map_err(|e| e.to_string())?
            .rejected;
        let panel = c
            .input_status(InputSource::Panel)
            .map_err(|e| e.to_string())?
            .rejected;
        if (main, panel) != (2, 2) {
            return Err("unexpected rejection count after encoder".into());
        }
        Ok(json!({"encoder":6,"rejections":{"main":main,"panel":panel}}))
    }
    pub(super) fn end(&self, session: &Session) -> Result<Value, String> {
        let c = l6ctl::OwnedControl::new(&session.engine);
        let main = c
            .input_status(InputSource::Main)
            .map_err(|e| e.to_string())?
            .rejected;
        let panel = c
            .input_status(InputSource::Panel)
            .map_err(|e| e.to_string())?
            .rejected;
        if (main, panel) != (2, 2) {
            return Err(format!(
                "L6 invalid-input checks observed {main} MAIN and {panel} panel rejections; expected 2 each"
            ));
        }
        Ok(
            json!({"panel_targets":self.verified,"encoders":self.encoders,"encoder_reports":6,"unsupported_main_targets_rejected":[0,5],"unsupported_panel_target_rejected":31,"unsupported_encoder_rejected":6,"rejections":{"main":main,"panel":panel}}),
        )
    }
    pub(super) fn abort(&mut self, session: &Session) -> Result<(), String> {
        if let Some(target) = self.held.take() {
            l6ctl::OwnedControl::new(&session.engine)
                .raw_up(InputSource::Panel, target)
                .map_err(|e| e.to_string())?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn wait_does_not_poll_again_after_sleep_crosses_deadline() {
        let start = Instant::now();
        let deadline = start + Duration::from_millis(10);
        let mut polls = 0;
        assert!(
            !guarded_poll(deadline, start, "panel", || {
                polls += 1;
                Ok(false)
            })
            .unwrap()
        );
        assert!(
            guarded_poll(
                deadline,
                deadline + Duration::from_millis(1),
                "panel",
                || {
                    polls += 1;
                    Ok(true)
                }
            )
            .is_err()
        );
        assert_eq!(polls, 1);
    }
    #[test]
    fn delayed_handoff_cannot_extend_the_original_wait() {
        let start = Instant::now();
        let deadline = start + Duration::from_secs(10);
        assert!(ensure_wait_open(deadline, start + Duration::from_secs(9), "panel").is_ok());
        assert!(ensure_wait_open(deadline, deadline, "panel").is_err());
        assert!(ensure_wait_open(deadline, deadline + Duration::from_secs(1), "panel").is_err());
    }
}
