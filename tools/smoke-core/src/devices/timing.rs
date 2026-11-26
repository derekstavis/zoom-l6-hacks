//! L6max paused-input and report timing transaction.
use super::*;
use l6ctl::{InputCounters, InputSource};

pub(super) struct Scope {
    start: Instant,
    guest_start: u32,
    guest_budget: u32,
    wall_budget: u64,
    pub phase: u8,
    pause: Option<l6max_host::pause_scope::PauseScope>,
    held: Option<u32>,
    paused_at: Instant,
    before_display: [u8; 1024],
    before: Option<InputCounters>,
    presses: usize,
    releases: usize,
    pressed: usize,
    released: usize,
    encoder_before: usize,
    encoder_applied: u64,
    encoder_reports: usize,
    wait_deadline: Option<Instant>,
    paused_ack_at: Option<Instant>,
    display_start: Option<u32>,
}
fn deadline(start: Instant, wall: u64) -> Result<Instant, String> {
    start
        .checked_add(Duration::from_secs(wall))
        .ok_or("timing deadline overflow".into())
}
fn wall_wait_until(deadline: Instant, until: Instant) -> Result<(), String> {
    loop {
        let now = Instant::now();
        if now >= deadline {
            return Err("input timing exceeded wall deadline".into());
        }
        if now >= until {
            return Ok(());
        }
        thread::sleep((until - now).min(Duration::from_millis(5)));
    }
}
fn report(options: &Options, byte: u8) -> Result<usize, String> {
    let trace = fs::read_to_string(options.logs.join("engine.trace")).map_err(|e| e.to_string())?;
    Ok(l6ctl::panel_uart_count(&trace, byte))
}
fn bounded_action<T>(
    end: Instant,
    mut now: impl FnMut() -> Instant,
    action: impl FnOnce() -> Result<T, String>,
) -> Result<T, String> {
    if now() >= end {
        return Err("input readiness deadline exceeded".into());
    }
    let value = action()?;
    if now() >= end {
        return Err("input readiness deadline exceeded".into());
    }
    Ok(value)
}
fn release_held(
    held: &mut Option<u32>,
    mut release: impl FnMut(u32) -> Result<(), String>,
) -> Result<(), String> {
    if let Some(target) = *held {
        release(target)?;
        *held = None;
    }
    Ok(())
}
impl Scope {
    pub(super) fn begin(s: &Session, guest: u32, wall: u64) -> Result<Self, String> {
        if s.engine.model() != l6fw::Model::L6max {
            return Err("input timing requires L6max".into());
        }
        let start = Instant::now();
        let d = deadline(start, wall)?;
        let control = l6ctl::OwnedControl::new(&s.engine);
        let guest_start = control
            .input_status(InputSource::Panel)
            .map_err(|e| e.to_string())?
            .guest_ms;
        let before_display = s.poll()?.display;
        let pause = control.pause_all_before(d, || {
            control
                .input_status(InputSource::Panel)
                .map(|status| status.guest_ms.wrapping_sub(guest_start) > guest)
                .unwrap_or(true)
        })?;
        let paused_at = Instant::now();
        Ok(Self {
            start,
            guest_start,
            guest_budget: guest,
            wall_budget: wall,
            phase: 0,
            pause: Some(pause),
            held: None,
            paused_at,
            before_display,
            before: None,
            presses: 0,
            releases: 0,
            pressed: 0,
            released: 0,
            encoder_before: 0,
            encoder_applied: 0,
            encoder_reports: 0,
            wait_deadline: None,
            paused_ack_at: None,
            display_start: None,
        })
    }
    fn deadline(&self) -> Result<Instant, String> {
        deadline(self.start, self.wall_budget)
    }
    pub(super) fn check(&self, s: &Session, g: u32, w: u64) -> Result<(), String> {
        let now = l6ctl::OwnedControl::new(&s.engine)
            .input_status(InputSource::Panel)
            .map_err(|e| e.to_string())?
            .guest_ms;
        if (g, w) != (self.guest_budget, self.wall_budget)
            || Instant::now() >= self.deadline()?
            || now.wrapping_sub(self.guest_start) > g
        {
            Err("input timing shared deadline exceeded or changed".into())
        } else {
            Ok(())
        }
    }
    fn input(s: &Session) -> Result<InputCounters, String> {
        l6ctl::OwnedControl::new(&s.engine)
            .input_status(InputSource::Panel)
            .map_err(|e| e.to_string())
    }
    fn wait(
        &self,
        s: &Session,
        mut ready: impl FnMut() -> Result<bool, String>,
        what: &str,
    ) -> Result<(), String> {
        let end = self
            .wait_deadline
            .ok_or("missing input wait deadline")?
            .min(self.deadline()?);
        loop {
            bounded_action(end, Instant::now, || {
                self.check(s, self.guest_budget, self.wall_budget)?;
                s.poll()?;
                self.check(s, self.guest_budget, self.wall_budget)
            })
            .map_err(|error| format!("waiting for {what}: {error}"))?;
            let is_ready = bounded_action(end, Instant::now, || {
                let result = ready()?;
                self.check(s, self.guest_budget, self.wall_budget)?;
                Ok(result)
            })
            .map_err(|error| format!("waiting for {what}: {error}"))?;
            if is_ready {
                return Ok(());
            }
            thread::sleep(Duration::from_millis(5));
        }
    }
    pub(super) fn settle(&self, s: &Session, ms: u32) -> Result<Value, String> {
        self.check(s, self.guest_budget, self.wall_budget)?;
        wall_wait_until(
            self.deadline()?,
            self.paused_at + Duration::from_millis(u64::from(ms)),
        )?;
        s.poll()?;
        Ok(json!({"settle_wall_ms":ms}))
    }
    pub(super) fn baseline(&mut self, s: &Session, o: &Options) -> Result<Value, String> {
        self.before = Some(Self::input(s)?);
        self.presses = report(o, 0x91)?;
        self.releases = report(o, 0x90)?;
        Ok(json!({"press_reports":self.presses,"release_reports":self.releases}))
    }
    pub(super) fn enqueue(
        &mut self,
        s: &Session,
        target: u32,
        hold_ms: u32,
        pairs: u32,
    ) -> Result<Value, String> {
        if target != 1 || pairs != 2 || hold_ms != 50 {
            return Err("input timing requires the verified two Up pairs".into());
        }
        let c = l6ctl::OwnedControl::new(&s.engine);
        for _ in 0..pairs {
            self.check(s, self.guest_budget, self.wall_budget)?;
            self.held = Some(target);
            c.down_with_sequence(target, hold_ms)
                .map_err(|e| e.to_string())?;
            c.up_with_sequence(target).map_err(|e| e.to_string())?;
            self.held = None;
        }
        self.wait_deadline = Some(Instant::now() + Duration::from_secs(10));
        Ok(json!({"target":target,"pairs":pairs}))
    }
    pub(super) fn wait_first(&mut self, s: &Session) -> Result<Value, String> {
        let before = self
            .before
            .as_ref()
            .ok_or("missing input baseline")?
            .applied;
        self.wait(
            s,
            || Ok(Self::input(s)?.applied >= before + 1),
            "first paused input",
        )?;
        self.paused_ack_at = Some(Instant::now());
        Ok(json!({"applied":true}))
    }
    pub(super) fn observe_paused(&self, s: &Session, ms: u32) -> Result<Value, String> {
        self.check(s, self.guest_budget, self.wall_budget)?;
        wall_wait_until(
            self.deadline()?,
            self.paused_ack_at.ok_or("missing paused acknowledgement")?
                + Duration::from_millis(u64::from(ms)),
        )?;
        Ok(json!({"observed_wall_ms":ms}))
    }
    pub(super) fn assert_paused(&self, s: &Session) -> Result<Value, String> {
        let before = self.before.as_ref().ok_or("missing input baseline")?;
        let now = Self::input(s)?;
        if now.applied != before.applied + 1 || now.guest_ms != before.guest_ms {
            return Err(format!(
                "input progressed while paused: before={before:?}, after={now:?}"
            ));
        }
        Ok(json!({"guest_clock_stopped":true,"applied":1}))
    }
    pub(super) fn resume(&mut self, s: &Session) -> Result<Value, String> {
        self.check(s, self.guest_budget, self.wall_budget)?;
        let mut pause = self.pause.take().ok_or("missing pause scope")?;
        pause.close_before(self.deadline()?)?;
        self.wait_deadline = Some(Instant::now() + Duration::from_secs(10));
        Ok(json!({"resumed":true}))
    }
    pub(super) fn wait_queued(&mut self, s: &Session) -> Result<Value, String> {
        let before = self
            .before
            .as_ref()
            .ok_or("missing input baseline")?
            .applied;
        self.wait(
            s,
            || Ok(Self::input(s)?.applied >= before + 4),
            "queued input delivery",
        )?;
        self.wait_deadline = Some(Instant::now() + Duration::from_secs(10));
        Ok(json!({"applied":4}))
    }
    pub(super) fn wait_reports(&self, s: &Session, o: &Options) -> Result<Value, String> {
        self.wait(
            s,
            || Ok(report(o, 0x91)? >= self.presses + 2 && report(o, 0x90)? >= self.releases + 2),
            "two press and release reports",
        )?;
        Ok(json!({"received":true}))
    }
    pub(super) fn assert_reports(&mut self, s: &Session, o: &Options) -> Result<Value, String> {
        self.pressed = report(o, 0x91)?
            .checked_sub(self.presses)
            .ok_or("press trace shrank")?;
        self.released = report(o, 0x90)?
            .checked_sub(self.releases)
            .ok_or("release trace shrank")?;
        if (self.pressed, self.released) != (2, 2) {
            return Err(format!(
                "expected 2 press/release reports, got {}/{}",
                self.pressed, self.released
            ));
        }
        self.display_start = Some(Self::input(s)?.guest_ms);
        Ok(json!({"presses":self.pressed,"releases":self.released}))
    }
    pub(super) fn observe_display(&self, s: &Session, ms: u32) -> Result<Value, String> {
        let start = self
            .display_start
            .ok_or("missing display observation anchor")?;
        super::panel::wait_panel_guest_elapsed(
            start,
            ms,
            self.guest_start,
            self.guest_budget,
            self.deadline()?,
            || Ok((Self::input(s)?.guest_ms, Instant::now())),
            || {
                s.poll()?;
                thread::sleep(Duration::from_millis(5));
                Ok(())
            },
        )?;
        Ok(json!({"guest_ms": Self::input(s)?.guest_ms.wrapping_sub(start)}))
    }
    pub(super) fn assert_display(&self, s: &Session) -> Result<Value, String> {
        if s.poll()?.display == self.before_display {
            return Err("display did not change after Up inputs".into());
        }
        Ok(json!({"changed":true}))
    }
    pub(super) fn encoder(
        &mut self,
        s: &Session,
        o: &Options,
        index: u32,
        steps: i32,
    ) -> Result<Value, String> {
        if index != 2 || steps != 1 {
            return Err("input timing requires verified channel 3 encoder input".into());
        }
        self.encoder_before = report(o, 0xa1)?;
        self.encoder_applied = Self::input(s)?.applied;
        let seq = l6ctl::OwnedControl::new(&s.engine)
            .encoder(index + 1, steps)
            .map_err(|e| e.to_string())?;
        self.wait_deadline = Some(Instant::now() + Duration::from_secs(10));
        Ok(json!({"receipt":seq}))
    }
    pub(super) fn wait_encoder(&mut self, s: &Session) -> Result<Value, String> {
        self.wait(
            s,
            || Ok(Self::input(s)?.applied > self.encoder_applied),
            "encoder acknowledgement",
        )?;
        self.wait_deadline = Some(Instant::now() + Duration::from_secs(10));
        Ok(json!({"applied":true}))
    }
    pub(super) fn wait_encoder_report(&self, s: &Session, o: &Options) -> Result<Value, String> {
        self.wait(
            s,
            || Ok(report(o, 0xa1)? >= self.encoder_before + 1),
            "encoder report",
        )?;
        Ok(json!({"received":true}))
    }
    pub(super) fn assert_encoder(&mut self, o: &Options) -> Result<Value, String> {
        self.encoder_reports = report(o, 0xa1)?
            .checked_sub(self.encoder_before)
            .ok_or("encoder trace shrank")?;
        if self.encoder_reports != 1 {
            return Err(format!(
                "expected one encoder report, got {}",
                self.encoder_reports
            ));
        }
        Ok(json!({"encoder_channel":3,"encoder_reports":self.encoder_reports}))
    }
    pub(super) fn abort(&mut self, s: &Session) -> Result<(), String> {
        let mut errors = Vec::new();
        if let Err(error) = release_held(&mut self.held, |target| {
            l6ctl::OwnedControl::new(&s.engine)
                .up_with_sequence(target)
                .map(|_| ())
                .map_err(|e| e.to_string())
        }) {
            errors.push(format!("release held input: {error}"));
        }
        if let Some(mut p) = self.pause.take() {
            if let Err(e) = p.close_before(Instant::now() + Duration::from_secs(3)) {
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
mod tests {
    use super::*;
    #[test]
    fn delayed_poll_or_ready_read_cannot_be_accepted() {
        let start = Instant::now();
        let end = start + Duration::from_millis(5);
        let mut clock = [start, end].into_iter();
        let mut reads = 0;
        assert!(
            bounded_action(
                end,
                || clock.next().unwrap(),
                || {
                    reads += 1;
                    Ok(true)
                }
            )
            .is_err()
        );
        assert!(
            bounded_action(
                end,
                || end,
                || {
                    reads += 1;
                    Ok(true)
                }
            )
            .is_err()
        );
        assert_eq!(reads, 1);
    }
    #[test]
    fn held_input_release_is_attempted_and_failure_preserves_state() {
        let mut held = Some(1);
        assert!(release_held(&mut held, |_| Err("injected".into())).is_err());
        assert_eq!(held, Some(1));
        let mut calls = 0;
        release_held(&mut held, |target| {
            calls += 1;
            assert_eq!(target, 1);
            Ok(())
        })
        .unwrap();
        release_held(&mut held, |_| {
            calls += 1;
            Ok(())
        })
        .unwrap();
        assert_eq!(calls, 1);
        assert_eq!(held, None);
    }
}
