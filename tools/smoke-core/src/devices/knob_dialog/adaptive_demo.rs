//! Execution of the bounded knob-dialog demo plan.
use super::*;
use crate::progress::{CaseEvent, CaseProgress, Outcome, unix_ms};
use crate::scenario::adaptive::{Block, Child, DemoCapture, Direction, Plan};

struct SharedDeadline {
    start: Instant,
    guest_start: u32,
    guest_budget: u32,
    wall_budget: u64,
}
impl SharedDeadline {
    fn expired(&self, now: Instant, guest: u32) -> bool {
        super::super::execute::deadline_exceeded(
            now.duration_since(self.start),
            self.guest_start,
            guest,
            self.guest_budget,
            self.wall_budget,
        )
    }
}
type DeadlineCheck<'a> = dyn Fn() -> Result<(), Box<dyn std::error::Error>> + 'a;
struct Reporter<'a> {
    progress: CaseProgress,
    index: usize,
    check: Option<&'a DeadlineCheck<'a>>,
}
impl Reporter<'_> {
    fn child<T>(
        &self,
        block: &str,
        attempt: u32,
        max: u32,
        child: &str,
        f: impl FnOnce() -> Result<T, Box<dyn std::error::Error>>,
    ) -> Result<T, Box<dyn std::error::Error>> {
        self.progress.emit(CaseEvent::AdaptiveChildStarted {
            index: self.index,
            block: block.into(),
            attempt,
            max_attempts: max,
            child: child.into(),
            started_unix_ms: unix_ms(),
        });
        let result: Result<T, Box<dyn std::error::Error>> = (|| {
            if let Some(check) = self.check {
                check()?;
            }
            let value = f()?;
            if let Some(check) = self.check {
                check()?;
            }
            Ok(value)
        })();
        self.progress.emit(CaseEvent::AdaptiveChildFinished {
            index: self.index,
            block: block.into(),
            attempt,
            child: child.into(),
            outcome: if result.is_ok() {
                Outcome::Pass
            } else {
                Outcome::Fail
            },
            finished_unix_ms: unix_ms(),
        });
        result.map_err(|error| format!("{block}, attempt {attempt}/{max}, {child}: {error}").into())
    }
    fn scope<T>(
        &self,
        block: &str,
        attempt: Option<u32>,
        max: u32,
        f: impl FnOnce() -> Result<T, Box<dyn std::error::Error>>,
    ) -> Result<T, Box<dyn std::error::Error>> {
        self.progress.emit(CaseEvent::AdaptiveScopeStarted {
            index: self.index,
            block: block.into(),
            attempt,
            max_attempts: max,
            started_unix_ms: unix_ms(),
        });
        let result = f();
        self.progress.emit(CaseEvent::AdaptiveScopeFinished {
            index: self.index,
            block: block.into(),
            attempt,
            outcome: if result.is_ok() {
                Outcome::Pass
            } else {
                Outcome::Fail
            },
            matched: None,
            finished_unix_ms: unix_ms(),
        });
        result
    }
    fn attempt(
        &self,
        block: &str,
        attempt: u32,
        max: u32,
        f: impl FnOnce() -> Result<bool, Box<dyn std::error::Error>>,
    ) -> Result<bool, Box<dyn std::error::Error>> {
        self.progress.emit(CaseEvent::AdaptiveScopeStarted {
            index: self.index,
            block: block.into(),
            attempt: Some(attempt),
            max_attempts: max,
            started_unix_ms: unix_ms(),
        });
        let result = f();
        self.progress.emit(CaseEvent::AdaptiveScopeFinished {
            index: self.index,
            block: block.into(),
            attempt: Some(attempt),
            outcome: if result.is_ok() {
                Outcome::Pass
            } else {
                Outcome::Fail
            },
            matched: result.as_ref().ok().copied(),
            finished_unix_ms: unix_ms(),
        });
        result
    }
}

fn check_demo_frames(
    config: &DemoCapture,
    mut capture: impl FnMut(u32) -> Result<[u8; 1024], Box<dyn std::error::Error>>,
    mut check: impl FnMut(u32, &[u8; 1024]) -> Result<bool, Box<dyn std::error::Error>>,
) -> Result<bool, Box<dyn std::error::Error>> {
    let mut frame = capture(1)?;
    for index in 0..config.checked_frames {
        if check(index + 1, &frame)? {
            return Ok(true);
        }
        frame = capture(index + 2)?;
    }
    Ok(false)
}

fn channel_ack_ready(state: l6ctl::InputAckState) -> Result<bool, &'static str> {
    match state {
        l6ctl::InputAckState::Applied => Ok(true),
        l6ctl::InputAckState::Pending => Ok(false),
        l6ctl::InputAckState::Rejected => Err("channel-2 encoder input rejected"),
        l6ctl::InputAckState::Unknown => Err("channel-2 encoder acknowledgement unavailable"),
    }
}

fn channel_level_percent(raw: u8) -> u32 {
    (u32::from(raw) * 100 + 63) / 127
}

trait AdaptiveHost {
    fn capture(
        &mut self,
        guest_ms: u32,
        artifact: &str,
    ) -> Result<[u8; 1024], Box<dyn std::error::Error>>;
    fn pan(&mut self) -> Result<u8, Box<dyn std::error::Error>>;
    fn text(&mut self) -> Result<String, Box<dyn std::error::Error>>;
    fn channel_level(&mut self, channel: u32) -> Result<u8, Box<dyn std::error::Error>>;
    fn encoder(&mut self, channel: u32, steps: i32) -> Result<(), Box<dyn std::error::Error>>;
    fn channel_turn(&mut self, delta: i32, before: u8) -> Result<bool, Box<dyn std::error::Error>>;
    fn channel_ready(
        &mut self,
        other: u8,
        current: u8,
    ) -> Result<(u8, String), Box<dyn std::error::Error>>;
    fn tap(&mut self, target: u32, hold_ms: u32) -> Result<(), Box<dyn std::error::Error>>;
    fn wait_mode(&mut self, mode: u8) -> Result<(), Box<dyn std::error::Error>>;
}

struct LiveHost<'a, 'b> {
    session: &'a Session,
    options: &'a Options,
    control: &'a l6ctl::OwnedControl<'b>,
    reader: &'a l6max_host::knob_popup::Reader<'a>,
    text_table: u32,
    guest_timeout_ms: u32,
    wall_timeout_s: u64,
    check: &'a DeadlineCheck<'a>,
}
impl AdaptiveHost for LiveHost<'_, '_> {
    fn capture(
        &mut self,
        guest_ms: u32,
        artifact: &str,
    ) -> Result<[u8; 1024], Box<dyn std::error::Error>> {
        capture_knob_frame_checked(
            self.session,
            self.options,
            guest_ms,
            artifact,
            self.guest_timeout_ms,
            self.wall_timeout_s,
            self.check,
        )
    }
    fn pan(&mut self) -> Result<u8, Box<dyn std::error::Error>> {
        Ok(self.reader.pan()?)
    }
    fn text(&mut self) -> Result<String, Box<dyn std::error::Error>> {
        Ok(self.reader.text_at_table(self.text_table)?)
    }
    fn channel_level(&mut self, channel: u32) -> Result<u8, Box<dyn std::error::Error>> {
        Ok(self.reader.channel_level(channel)?)
    }
    fn encoder(&mut self, channel: u32, steps: i32) -> Result<(), Box<dyn std::error::Error>> {
        self.control.encoder(channel, steps)?;
        Ok(())
    }
    fn channel_turn(&mut self, delta: i32, before: u8) -> Result<bool, Box<dyn std::error::Error>> {
        (self.check)()?;
        let receipt = self.control.encoder(2, delta.saturating_neg())?;
        loop {
            self.session.poll()?;
            (self.check)()?;
            if channel_ack_ready(
                self.control
                    .input_ack(l6ctl::InputSource::Panel, receipt.sequence)?
                    .state,
            )? {
                break;
            }
            thread::sleep(Duration::from_millis(5));
        }
        if channel_level_turn_clamped(before, delta) {
            return Ok(false);
        }
        loop {
            self.session.poll()?;
            (self.check)()?;
            if self.reader.channel_level(2)? != before {
                return Ok(true);
            }
            thread::sleep(Duration::from_millis(5));
        }
    }
    fn channel_ready(
        &mut self,
        other: u8,
        _current: u8,
    ) -> Result<(u8, String), Box<dyn std::error::Error>> {
        let raw = wait_for_channel_level(
            other,
            || {
                self.session.poll().map_err(|e| e.to_string())?;
                Ok((
                    self.reader.channel_level(2)?,
                    self.reader.text_at_table(self.text_table)?,
                ))
            },
            || (self.check)().is_err(),
            || thread::sleep(Duration::from_millis(5)),
        )?;
        Ok((raw, self.reader.text_at_table(self.text_table)?))
    }
    fn tap(&mut self, target: u32, hold_ms: u32) -> Result<(), Box<dyn std::error::Error>> {
        self.control.tap(target, hold_ms)?;
        Ok(())
    }
    fn wait_mode(&mut self, mode: u8) -> Result<(), Box<dyn std::error::Error>> {
        wait_parameter_mode_checked(
            self.session,
            self.control,
            mode,
            self.guest_timeout_ms,
            self.wall_timeout_s,
            self.check,
        )
    }
}

fn demo_frame(
    host: &mut impl AdaptiveHost,
    reporter: &Reporter,
    block: &str,
    attempt: u32,
    max: u32,
    config: &DemoCapture,
    artifact: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let passed = check_demo_frames(
        config,
        |number| {
            reporter.child(
                block,
                attempt,
                max,
                &format!("capture demo frame {number}"),
                || host.capture(config.guest_ms, artifact),
            )
        },
        |number, frame| {
            reporter.child(
                block,
                attempt,
                max,
                &format!("check demo frame {number}"),
                || Ok(frame.iter().map(|byte| byte.count_ones()).sum::<u32>() >= config.min_lit),
            )
        },
    )?;
    if passed {
        Ok(())
    } else {
        reporter.child(block, attempt, max, "assert demo frame bright", || {
            Err(format!("demo frame {artifact} stayed blank").into())
        })
    }
}

fn pan_encoder_steps(raw: u8, target: u8) -> i32 {
    if raw < target { -1 } else { 1 }
}

fn reached(direction: Direction, min: u8, text: &str) -> bool {
    let prefix = match direction {
        Direction::Left => "L ",
        Direction::Right => "R ",
    };
    text.strip_prefix(prefix)
        .and_then(|s| s.strip_suffix('%'))
        .and_then(|s| s.parse::<u8>().ok())
        .is_some_and(|value| value >= min)
}

pub(in crate::devices) fn run(
    plan: &Plan,
    session: &Session,
    options: &Options,
    context: &mut PreflightContext,
    progress: CaseProgress,
    index: usize,
    guest_timeout_ms: u32,
    wall_timeout_s: u64,
    shared: (Instant, u32, u32, u64, u16),
) -> Result<Value, Box<dyn std::error::Error>> {
    plan.validate()?;
    if context.next != 14
        || context.mode_index == 0
        || context.last_mode != Some(8)
        || context.final_phase != 0
        || context.alias_widget.is_none()
        || context.alias_layer.is_none()
        || context.debugger.is_none()
        || context.active.is_none()
    {
        return Err("incomplete knob-dialog preflight context".into());
    }
    let main = selected_main_image(options).map_err(std::io::Error::other)?;
    if main.get(0x40c..0x410) != Some(&0x8007eea9u32.to_le_bytes()) {
        return Err("patched startup hook does not reserve popup RAM".into());
    }
    let control = l6ctl::OwnedControl::new(&session.engine);
    let deadline = SharedDeadline {
        start: shared.0,
        guest_start: shared.1,
        guest_budget: shared.2,
        wall_budget: shared.3,
    };
    let check = || -> Result<(), Box<dyn std::error::Error>> {
        let guest = control.input_status(l6ctl::InputSource::Panel)?.guest_ms;
        if deadline.expired(Instant::now(), guest) {
            Err("knob-dialog shared deadline exceeded".into())
        } else {
            Ok(())
        }
    };
    check()?;
    let reader = control.knob_popup_reader()?;
    check()?;
    let reporter = Reporter {
        progress,
        index,
        check: Some(&check),
    };
    let mut host = LiveHost {
        session,
        options,
        control: &control,
        reader: &reader,
        text_table: context.text_table,
        guest_timeout_ms,
        wall_timeout_s,
        check: &check,
    };
    execute_plan(plan, &mut host, &reporter)?;
    context.final_phase = 1;
    Ok(json!({"pan_and_level_checked":true}))
}

fn with_level_host<T>(
    session: &Session,
    options: &Options,
    context: &PreflightContext,
    progress: CaseProgress,
    index: usize,
    guest_timeout_ms: u32,
    wall_timeout_s: u64,
    shared: (Instant, u32, u32, u64, u16),
    f: impl FnOnce(&mut LiveHost<'_, '_>, &Reporter<'_>) -> Result<T, Box<dyn std::error::Error>>,
) -> Result<T, Box<dyn std::error::Error>> {
    let control = l6ctl::OwnedControl::new(&session.engine);
    let deadline = SharedDeadline {
        start: shared.0,
        guest_start: shared.1,
        guest_budget: shared.2,
        wall_budget: shared.3,
    };
    let check = || -> Result<(), Box<dyn std::error::Error>> {
        let guest = control.input_status(l6ctl::InputSource::Panel)?.guest_ms;
        if deadline.expired(Instant::now(), guest) {
            Err("knob-dialog shared deadline exceeded".into())
        } else {
            Ok(())
        }
    };
    check()?;
    let reader = control.knob_popup_reader()?;
    check()?;
    let reporter = Reporter {
        progress,
        index,
        check: Some(&check),
    };
    let mut host = LiveHost {
        session,
        options,
        control: &control,
        reader: &reader,
        text_table: context.text_table,
        guest_timeout_ms,
        wall_timeout_s,
        check: &check,
    };
    f(&mut host, &reporter)
}

pub(in crate::devices) fn run_level_demo_frame(
    session: &Session,
    options: &Options,
    context: &mut PreflightContext,
    demo: &DemoCapture,
    artifact: &str,
    progress: CaseProgress,
    index: usize,
    guest_timeout_ms: u32,
    wall_timeout_s: u64,
    shared: (Instant, u32, u32, u64, u16),
) -> Result<Value, Box<dyn std::error::Error>> {
    if context.level_phase != Some(0)
        || !crate::scenario::adaptive::demo(demo)
        || !crate::scenario::adaptive::artifact(artifact)
    {
        return Err("level demo start out of order or invalid".into());
    }
    with_level_host(
        session,
        options,
        context,
        progress,
        index,
        guest_timeout_ms,
        wall_timeout_s,
        shared,
        |host, reporter| {
            reporter.scope("level start", None, 1, || {
                demo_frame(host, reporter, "level start", 1, 1, demo, artifact)
            })
        },
    )?;
    context.level_phase = Some(1);
    Ok(json!({"artifact":artifact}))
}

fn level_target(
    direction: crate::scenario::adaptive::LevelDirection,
    min_percent: u8,
    text: &str,
) -> bool {
    use crate::scenario::adaptive::LevelDirection as D;
    match direction {
        D::Up => {
            text.strip_suffix('%')
                .and_then(|value| value.parse::<u8>().ok())
                .unwrap_or(0)
                >= min_percent
        }
        D::Down => text == "0%",
    }
}

fn execute_level_sweep(
    sweep: &crate::scenario::adaptive::LevelSweep,
    host: &mut impl AdaptiveHost,
    reporter: &Reporter,
) -> Result<(), Box<dyn std::error::Error>> {
    use crate::scenario::adaptive::{LevelChild as C, LevelDirection as D};
    sweep.validate()?;
    let label = match sweep.direction {
        D::Up => "level up",
        D::Down => "level down",
    };
    reporter.scope(label, None, sweep.max_attempts, || {
        for attempt in 1..=sweep.max_attempts {
            let matched = reporter.attempt(label, attempt, sweep.max_attempts, || {
                let mut text = String::new();
                for child in &sweep.children {
                    match child {
                        C::ReadText => {
                            text = reporter.child(
                                label,
                                attempt,
                                sweep.max_attempts,
                                "read level text",
                                || host.text(),
                            )?
                        }
                        C::CheckTarget => {
                            if reporter.child(
                                label,
                                attempt,
                                sweep.max_attempts,
                                "check level target",
                                || Ok(level_target(sweep.direction, sweep.min_percent, &text)),
                            )? {
                                return Ok(true);
                            }
                        }
                        C::Turn => reporter.child(
                            label,
                            attempt,
                            sweep.max_attempts,
                            "turn level",
                            || {
                                host.encoder(
                                    1,
                                    match sweep.direction {
                                        D::Up => -1,
                                        D::Down => 1,
                                    },
                                )
                            },
                        )?,
                        C::CaptureDemo => demo_frame(
                            host,
                            reporter,
                            label,
                            attempt,
                            sweep.max_attempts,
                            &sweep.demo,
                            &format!("{}-{:02}", sweep.artifact_prefix, attempt - 1),
                        )?,
                    }
                }
                Ok(false)
            })?;
            if matched {
                return Ok(());
            }
        }
        match sweep.direction {
            D::Up => Err(format!("demo level sweep did not reach {}%", sweep.min_percent).into()),
            D::Down => Ok(()),
        }
    })
}

pub(in crate::devices) fn run_level_sweep(
    session: &Session,
    options: &Options,
    context: &mut PreflightContext,
    sweep: &crate::scenario::adaptive::LevelSweep,
    progress: CaseProgress,
    index: usize,
    guest_timeout_ms: u32,
    wall_timeout_s: u64,
    shared: (Instant, u32, u32, u64, u16),
) -> Result<Value, Box<dyn std::error::Error>> {
    use crate::scenario::adaptive::LevelDirection as D;
    let expected = match sweep.direction {
        D::Up => 1,
        D::Down => 3,
    };
    if context.level_phase != Some(expected) {
        return Err("level sweep out of order".into());
    }
    with_level_host(
        session,
        options,
        context,
        progress,
        index,
        guest_timeout_ms,
        wall_timeout_s,
        shared,
        |host, reporter| execute_level_sweep(sweep, host, reporter),
    )?;
    if sweep.direction == D::Up {
        context.level_peak = Some(sweep.min_percent);
        context.level_phase = Some(2);
    } else {
        context.level_phase = Some(4);
    }
    Ok(json!({"direction":format!("{:?}",sweep.direction)}))
}

pub(in crate::devices) fn run_level_expect_zero(
    session: &Session,
    options: &Options,
    context: &mut PreflightContext,
    progress: CaseProgress,
    index: usize,
    guest_timeout_ms: u32,
    wall_timeout_s: u64,
    shared: (Instant, u32, u32, u64, u16),
) -> Result<Value, Box<dyn std::error::Error>> {
    if context.level_phase != Some(4) {
        return Err("level zero assertion out of order".into());
    }
    let text = with_level_host(
        session,
        options,
        context,
        progress,
        index,
        guest_timeout_ms,
        wall_timeout_s,
        shared,
        |host, reporter| reporter.child("level zero", 1, 1, "read final level", || host.text()),
    )?;
    if text != "0%" {
        return Err("demo level sweep did not return to zero".into());
    }
    context.level_phase = Some(5);
    Ok(json!({"displayed":text}))
}

fn execute_plan(
    plan: &Plan,
    host: &mut impl AdaptiveHost,
    reporter: &Reporter,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut pan_start = 0u8;
    let mut pan = 0u8;
    let mut text = String::new();
    let mut level1 = 0u8;
    let mut level2 = 0u8;
    for block in &plan.blocks {
        match block {
            Block::Center { children, artifact, demo } => reporter.scope("pan center",None,1,|| {
                for child in children {
                    match child {
                        Child::CaptureDemo => demo_frame(host,reporter,"pan center",1,1,demo,artifact)?,
                        Child::AssertCenter => reporter.child("pan center",1,1,"check CENTER",|| { if host.text()? != "CENTER" { Err("demo pan sweep did not start at center".into()) } else { Ok(()) } })?,
                        Child::SavePan => pan_start = reporter.child("pan center",1,1,"save pan",|| Ok(host.pan()?))?,
                        _ => return Err("invalid center child".into()),
                    }
                }
                Ok(())
            })?,
            Block::PanSweep { children, after, direction, target, min_percent, max_attempts, artifact_prefix, endpoint_artifact, endpoint_guest_ms, demo, .. } => {
                let label = match direction { Direction::Left => "pan left", Direction::Right => "pan right" };
                reporter.scope(label,None,*max_attempts,|| {
                    let mut done = false;
                    for attempt in 1..=*max_attempts {
                        let reached_target = reporter.attempt(label,attempt,*max_attempts,|| {
                            for child in children {
                                match child {
                                    Child::ReadPan => pan = reporter.child(label,attempt,*max_attempts,"read pan",|| Ok(host.pan()?))?,
                                    Child::ReadText => text = reporter.child(label,attempt,*max_attempts,"read text",|| Ok(host.text()?))?,
                                    Child::CheckTarget => if reporter.child(label,attempt,*max_attempts,"check target",|| Ok(reached(*direction,*min_percent,&text)))? { return Ok(true); },
                                    Child::TurnPan => reporter.child(label,attempt,*max_attempts,"turn pan",|| { host.encoder(1,pan_encoder_steps(pan, *target))?; Ok(()) })?,
                                    Child::CaptureDemo => demo_frame(host,reporter,label,attempt,*max_attempts,demo,&format!("{artifact_prefix}-{:02}",attempt-1))?,
                                    _ => return Err("invalid pan child".into()),
                                }
                            }
                            Ok(false)
                        })?;
                        if reached_target { done = true; break; }
                    }
                    if !done { return Err(format!("demo pan sweep did not reach {} 50%", match direction { Direction::Left => "left", Direction::Right => "right" }).into()); }
                    for child in after {
                        match child {
                            Child::Capture => { reporter.child(label,1,1,"capture endpoint",|| host.capture(*endpoint_guest_ms,endpoint_artifact))?; },
                            _ => return Err("invalid pan endpoint child".into()),
                        }
                    }
                    Ok(())
                })?;
            }
            Block::PanReset { children, max_attempts, artifact, guest_ms, .. } => reporter.scope("pan reset",None,*max_attempts,|| {
                for attempt in 1..=*max_attempts {
                    let done = reporter.attempt("pan reset",attempt,*max_attempts,|| {
                        for child in children {
                            match child {
                                Child::ReadPan => pan = reporter.child("pan reset",attempt,*max_attempts,"read pan",|| Ok(host.pan()?))?,
                                Child::CheckTarget => if reporter.child("pan reset",attempt,*max_attempts,"check start",|| Ok(pan == pan_start))? { return Ok(true); },
                                Child::TurnPan => reporter.child("pan reset",attempt,*max_attempts,"turn pan",|| { host.encoder(1,pan_encoder_steps(pan, pan_start))?; Ok(()) })?,
                                Child::Capture => { reporter.child("pan reset",attempt,*max_attempts,"capture",|| host.capture(*guest_ms,artifact))?; },
                                _ => return Err("invalid reset child".into()),
                            }
                        }
                        Ok(false)
                    })?;
                    if done { break; }
                }
                Ok(())
            })?,
            Block::SelectLevel { children, target, hold_ms, mode } => reporter.scope("select level",None,1,|| {
                for child in children {
                    match child {
                        Child::Tap => reporter.child("select level",1,1,"tap",|| { host.tap(*target,*hold_ms)?; Ok(()) })?,
                        Child::WaitMode => reporter.child("select level",1,1,"wait for mode",|| {
                            host.wait_mode(*mode)
                        })?,
                        _ => return Err("invalid level child".into()),
                    }
                }
                Ok(())
            })?,
            Block::Channel2 { before, attempt: attempt_children, after, deltas, artifact, guest_ms } => reporter.scope("channel 2",None,deltas.len() as u32,|| {
                for child in before {
                    match child {
                        Child::ReadChannel1 => level1 = reporter.child("channel 2",1,1,"read channel 1",|| Ok(host.channel_level(1)?))?,
                        Child::ReadChannel2 => level2 = reporter.child("channel 2",1,1,"read channel 2",|| Ok(host.channel_level(2)?))?,
                        _ => return Err("invalid channel baseline child".into()),
                    }
                }
                let mut observed_change = false;
                for (index, delta) in deltas.iter().enumerate() {
                    let attempt = index as u32 + 1;
                    let mut changed = false;
                    let done = reporter.attempt("channel 2",attempt,deltas.len() as u32,|| {
                        for child in attempt_children {
                            match child {
                                Child::TurnChannel2 => { changed = reporter.child("channel 2",attempt,deltas.len() as u32,"turn channel 2 and wait for input",|| host.channel_turn(*delta, level2))?; observed_change |= changed; },
                                Child::Capture => { reporter.child("channel 2",attempt,deltas.len() as u32,"capture",|| host.capture(*guest_ms,artifact))?; },
                                Child::ReadChannel2 => level2 = reporter.child("channel 2",attempt,deltas.len() as u32,"read channel 2",|| Ok(host.channel_level(2)?))?,
                                Child::CheckChannel2 => if reporter.child("channel 2",attempt,deltas.len() as u32,"check changed",|| Ok(changed && level2 != level1 && channel_level_percent(level2) != channel_level_percent(level1)))? { return Ok(true); },
                                _ => return Err("invalid channel attempt child".into()),
                            }
                        }
                        Ok(false)
                    })?;
                    if done { break; }
                }
                for child in after {
                    match child {
                        Child::ReadText => { let (raw, ready_text) = reporter.child("channel 2",1,1,"wait for channel 2 text",|| host.channel_ready(level1, level2))?; level2 = raw; text = ready_text; },
                        Child::AssertLevel => reporter.child("channel 2",1,1,"assert channel 2 value",|| {
                            let expected = format!("{}%",(u32::from(level2)*100+63)/127);
                            if !observed_change || level2 == level1 || channel_level_percent(level2) == channel_level_percent(level1) || !knob_display_matches(9,&expected,&text) { Err(format!("channel-2 event did not display its own LEVEL value: channel0={level1}, channel1={level2}, displayed={text:?}").into()) } else { Ok(()) }
                        })?,
                        _ => return Err("invalid channel assertion child".into()),
                    }
                }
                Ok(())
            })?,
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn shared_deadline_wrap_and_wall_are_enforced() {
        let start = Instant::now();
        let deadline = SharedDeadline {
            start,
            guest_start: u32::MAX - 49,
            guest_budget: 150,
            wall_budget: 3,
        };
        assert!(!deadline.expired(start + Duration::from_secs(1), 100));
        assert!(deadline.expired(start + Duration::from_secs(1), 101));
        assert!(deadline.expired(start + Duration::from_secs(4), 0));
    }
    #[test]
    fn expired_child_does_not_execute_side_effect() {
        let called = std::cell::Cell::new(false);
        let check = || -> Result<(), Box<dyn std::error::Error>> { Err("expired".into()) };
        let reporter = Reporter {
            progress: CaseProgress::default(),
            index: 1,
            check: Some(&check),
        };
        let result = reporter.child("pan", 1, 100, "turn", || {
            called.set(true);
            Ok(())
        });
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("pan, attempt 1/100, turn: expired")
        );
        assert!(!called.get());
    }
    struct FakeHost {
        calls: Vec<String>,
        pans: std::collections::VecDeque<u8>,
        texts: std::collections::VecDeque<String>,
        levels: std::collections::VecDeque<(u32, u8)>,
    }
    impl AdaptiveHost for FakeHost {
        fn capture(
            &mut self,
            ms: u32,
            artifact: &str,
        ) -> Result<[u8; 1024], Box<dyn std::error::Error>> {
            self.calls.push(format!("capture:{ms}:{artifact}"));
            Ok([255; 1024])
        }
        fn pan(&mut self) -> Result<u8, Box<dyn std::error::Error>> {
            self.calls.push("pan".into());
            self.pans.pop_front().ok_or_else(|| "missing pan".into())
        }
        fn text(&mut self) -> Result<String, Box<dyn std::error::Error>> {
            self.calls.push("text".into());
            self.texts.pop_front().ok_or_else(|| "missing text".into())
        }
        fn channel_level(&mut self, channel: u32) -> Result<u8, Box<dyn std::error::Error>> {
            self.calls.push(format!("level:{channel}"));
            let (expected, value) = self.levels.pop_front().ok_or("missing level")?;
            if channel != expected {
                return Err("wrong level channel".into());
            }
            Ok(value)
        }
        fn encoder(&mut self, channel: u32, steps: i32) -> Result<(), Box<dyn std::error::Error>> {
            self.calls.push(format!("encoder:{channel}:{steps}"));
            Ok(())
        }
        fn channel_turn(
            &mut self,
            delta: i32,
            before: u8,
        ) -> Result<bool, Box<dyn std::error::Error>> {
            self.encoder(2, delta.saturating_neg())?;
            Ok(!channel_level_turn_clamped(before, delta))
        }
        fn channel_ready(
            &mut self,
            _other: u8,
            current: u8,
        ) -> Result<(u8, String), Box<dyn std::error::Error>> {
            Ok((current, self.text()?))
        }
        fn tap(&mut self, target: u32, hold_ms: u32) -> Result<(), Box<dyn std::error::Error>> {
            self.calls.push(format!("tap:{target}:{hold_ms}"));
            Ok(())
        }
        fn wait_mode(&mut self, mode: u8) -> Result<(), Box<dyn std::error::Error>> {
            self.calls.push(format!("mode:{mode}"));
            Ok(())
        }
    }
    fn canonical_plan() -> Plan {
        let fragment: crate::Fragment = toml::from_str(include_str!(
            "../../../../../patches/knob-dialog/tests/emulator/fragments/invalid-tags.toml"
        ))
        .unwrap();
        match &fragment.step[0].action {
            crate::Action::KnobDialogAdaptive { plan } => plan.clone(),
            _ => panic!("missing plan"),
        }
    }
    #[test]
    fn full_plan_uses_initial_channel_read_and_ordered_fallback() {
        let plan = canonical_plan();
        plan.validate().unwrap();
        let mut host = FakeHost {
            calls: Vec::new(),
            pans: [64, 64, 64, 64].into(),
            texts: [
                "CENTER".into(),
                "L 48%".into(),
                "R 48%".into(),
                "32%".into(),
            ]
            .into(),
            levels: [(1, 40), (2, 41), (2, 40), (2, 41)].into(),
        };
        let reporter = Reporter {
            progress: CaseProgress::default(),
            index: 1,
            check: None,
        };
        execute_plan(&plan, &mut host, &reporter).unwrap();
        let baseline = host.calls.iter().position(|v| v == "level:2").unwrap();
        let first_turn = host.calls.iter().position(|v| v == "encoder:2:1").unwrap();
        let second_turn = host.calls.iter().position(|v| v == "encoder:2:-1").unwrap();
        assert!(baseline < first_turn && first_turn < second_turn);
        assert_eq!(
            host.calls
                .iter()
                .filter(|v| v.starts_with("encoder:2:"))
                .count(),
            2
        );
        assert!(
            host.calls
                .contains(&"capture:250:window-scene-channel-2-level".into())
        );
    }
    #[test]
    fn equal_target_still_turns_and_first_artifact_is_zero_indexed() {
        let plan = canonical_plan();
        let mut host = FakeHost {
            calls: Vec::new(),
            pans: [64, 32, 31, 64, 64].into(),
            texts: [
                "CENTER".into(),
                "L 0%".into(),
                "L 48%".into(),
                "R 48%".into(),
                "32%".into(),
            ]
            .into(),
            levels: [(1, 40), (2, 40), (2, 41)].into(),
        };
        let reporter = Reporter {
            progress: CaseProgress::default(),
            index: 1,
            check: None,
        };
        execute_plan(&plan, &mut host, &reporter).unwrap();
        assert!(host.calls.contains(&"encoder:1:1".into()));
        assert!(
            host.calls
                .contains(&"capture:100:knob-dialog-demo-pan-left-00".into())
        );
    }
    #[test]
    fn exhausted_reset_continues_to_level_selection() {
        let mut plan = canonical_plan();
        if let Block::PanReset { max_attempts, .. } = &mut plan.blocks[3] {
            *max_attempts = 2;
        }
        let mut host = FakeHost {
            calls: Vec::new(),
            pans: [64, 64, 64, 60, 60].into(),
            texts: [
                "CENTER".into(),
                "L 48%".into(),
                "R 48%".into(),
                "32%".into(),
            ]
            .into(),
            levels: [(1, 40), (2, 40), (2, 41)].into(),
        };
        let reporter = Reporter {
            progress: CaseProgress::default(),
            index: 1,
            check: None,
        };
        execute_plan(&plan, &mut host, &reporter).unwrap();
        assert_eq!(
            host.calls
                .iter()
                .filter(|v| *v == "capture:50:knob-dialog-demo-pan-reset")
                .count(),
            2
        );
        assert!(host.calls.contains(&"tap:35:50".into()));
    }
    #[test]
    fn six_checks_capture_the_seventh_before_failure() {
        let demo = DemoCapture {
            guest_ms: 100,
            min_lit: 100,
            checked_frames: 6,
        };
        let mut captures = Vec::new();
        let mut checks = Vec::new();
        let result = check_demo_frames(
            &demo,
            |index| {
                captures.push(index);
                Ok([0; 1024])
            },
            |index, _| {
                checks.push(index);
                Ok(false)
            },
        )
        .unwrap();
        assert!(!result);
        assert_eq!(captures, [1, 2, 3, 4, 5, 6, 7]);
        assert_eq!(checks, [1, 2, 3, 4, 5, 6]);
    }
    #[test]
    fn pan_target_and_threshold_match_original_rules() {
        assert_eq!(pan_encoder_steps(31, 32), -1);
        assert_eq!(pan_encoder_steps(32, 32), 1);
        assert!(reached(Direction::Left, 48, "L 48%"));
        assert!(!reached(Direction::Left, 48, "R 48%"));
        assert!(!reached(Direction::Right, 48, "R 47%"));
    }
    fn level_sweep(
        direction: crate::scenario::adaptive::LevelDirection,
        max_attempts: u32,
    ) -> crate::scenario::adaptive::LevelSweep {
        use crate::scenario::adaptive::{LevelChild as C, LevelDirection as D};
        crate::scenario::adaptive::LevelSweep {
            direction,
            children: vec![C::ReadText, C::CheckTarget, C::Turn, C::CaptureDemo],
            min_percent: if direction == D::Up { 69 } else { 0 },
            max_attempts,
            artifact_prefix: "level".into(),
            demo: DemoCapture {
                guest_ms: 100,
                min_lit: 100,
                checked_frames: 6,
            },
        }
    }
    fn level_host(texts: &[&str]) -> FakeHost {
        FakeHost {
            calls: Vec::new(),
            pans: Default::default(),
            texts: texts.iter().map(|s| (*s).to_owned()).collect(),
            levels: Default::default(),
        }
    }
    #[test]
    fn level_up_parsing_polarity_and_zero_based_artifacts_match_original() {
        use crate::scenario::adaptive::LevelDirection as D;
        let mut host = level_host(&["unparseable", "68%", "69%"]);
        let reporter = Reporter {
            progress: CaseProgress::default(),
            index: 1,
            check: None,
        };
        execute_level_sweep(&level_sweep(D::Up, 3), &mut host, &reporter).unwrap();
        assert_eq!(
            host.calls,
            [
                "text",
                "encoder:1:-1",
                "capture:100:level-00",
                "text",
                "encoder:1:-1",
                "capture:100:level-01",
                "text"
            ]
        );
    }
    #[test]
    fn up_exhaustion_does_not_reread_but_down_leaves_final_zero_read() {
        use crate::scenario::adaptive::LevelDirection as D;
        let reporter = Reporter {
            progress: CaseProgress::default(),
            index: 1,
            check: None,
        };
        let mut up = level_host(&["68%", "69%"]);
        assert!(execute_level_sweep(&level_sweep(D::Up, 1), &mut up, &reporter).is_err());
        assert_eq!(up.texts.len(), 1);
        let mut down = level_host(&["1%", "0%", "0%"]);
        execute_level_sweep(&level_sweep(D::Down, 2), &mut down, &reporter).unwrap();
        assert_eq!(
            down.calls,
            ["text", "encoder:1:1", "capture:100:level-00", "text"]
        );
        assert_eq!(down.text().unwrap(), "0%");
    }
}

#[cfg(test)]
mod integrated_readiness_tests {
    use super::*;

    #[test]
    fn channel_ack_is_exact_and_rejects_missing_or_failed_receipts() {
        use l6ctl::InputAckState::*;
        assert_eq!(channel_ack_ready(Pending), Ok(false));
        assert_eq!(channel_ack_ready(Applied), Ok(true));
        assert!(channel_ack_ready(Rejected).is_err());
        assert!(channel_ack_ready(Unknown).is_err());
    }

    #[test]
    fn raw_values_that_round_to_same_percent_are_not_distinguishable() {
        assert_ne!(40, 41);
        assert_eq!(channel_level_percent(2), channel_level_percent(3));
        assert_ne!(channel_level_percent(40), channel_level_percent(41));
    }
}
