//! Bounded, retained MIDI observations used by mixer and DIN scenarios.
use super::*;
use crate::scenario::MidiProbePort;
use l6ctl::midi::{ControlChange, MidiSession};

pub(super) struct MidiProbeContext {
    din: MidiSession,
    direct_mixer: Option<MidiSession>,
    start: Instant,
    guest_start: u32,
    guest_timeout_ms: u32,
    wall_timeout_s: u64,
    last_send: Option<Stimulus>,
    last_ring: Option<(usize, u32, u32)>,
    tap_at: Option<(MidiProbePort, Instant)>,
}

#[derive(Clone, Copy)]
struct Stimulus {
    port: MidiProbePort,
    sent: Instant,
    guest: u32,
    ring_channel: usize,
    before: [u32; 8],
}

#[derive(Debug, PartialEq, Eq)]
enum RingDecision {
    Changed,
    UnexpectedChange,
    Unchanged,
    MissingChange,
    Timeout,
    Waiting,
}

fn ring_decision(
    old: u32,
    current: u32,
    changed: bool,
    elapsed: u32,
    guest_window: Option<u32>,
    wall_expired: bool,
    guest_expired: bool,
) -> RingDecision {
    if current != old {
        return if changed {
            RingDecision::Changed
        } else {
            RingDecision::UnexpectedChange
        };
    }
    if guest_window.is_some_and(|window| elapsed >= window) {
        return if changed {
            RingDecision::MissingChange
        } else {
            RingDecision::Unchanged
        };
    }
    if wall_expired || guest_expired {
        RingDecision::Timeout
    } else {
        RingDecision::Waiting
    }
}

fn observe_no_events(
    deadline: Instant,
    mut pump: impl FnMut() -> Result<(), String>,
    mut poll: impl FnMut() -> Result<Option<[u8; 3]>, String>,
    mut now: impl FnMut() -> Instant,
    mut expired: impl FnMut() -> Result<bool, String>,
    mut sleep: impl FnMut(Duration),
) -> Result<Vec<[u8; 3]>, String> {
    if now() >= deadline {
        return Err("MIDI no-events window elapsed before observation began".into());
    }
    let mut observed = Vec::new();
    while now() < deadline {
        pump()?;
        if let Some(message) = poll()? {
            observed.push(message);
        }
        if expired()? {
            return Err("MIDI probe shared deadline elapsed during no-events observation".into());
        }
        sleep(Duration::from_millis(5));
    }
    // Poll after the final sleep and drain the queued backlog: a message can
    // arrive after the last in-window poll. The backend does not supply event
    // timestamps, so a message arriving just after the boundary is treated
    // conservatively as an observation. Bound a continuously growing queue.
    pump()?;
    for _ in 0..4096 {
        match poll()? {
            Some(message) => observed.push(message),
            None => return Ok(observed),
        }
        if expired()? {
            return Err("MIDI probe shared deadline elapsed during no-events observation".into());
        }
    }
    return Err("MIDI observation queue did not drain at the window boundary".into());
}

fn drain_before_tap(
    mut poll: impl FnMut() -> Result<Option<[u8; 3]>, String>,
    mut expired: impl FnMut() -> Result<bool, String>,
) -> Result<usize, String> {
    let mut drained = 0usize;
    loop {
        if expired()? {
            return Err("MIDI probe deadline elapsed while draining".into());
        }
        let message = poll()?;
        if expired()? {
            return Err("MIDI probe deadline elapsed while draining".into());
        }
        match message {
            Some(_) => drained = drained.saturating_add(1),
            None => return Ok(drained),
        }
    }
}

impl MidiProbeContext {
    fn expired(&self, guest: u32) -> bool {
        self.start.elapsed() > Duration::from_secs(self.wall_timeout_s)
            || guest.wrapping_sub(self.guest_start) > self.guest_timeout_ms
    }
}

impl State {
    fn poll_midi_probe_port(
        &mut self,
        context: &mut MidiProbeContext,
        port: MidiProbePort,
    ) -> Result<Option<[u8; 3]>, String> {
        match port {
            MidiProbePort::Din => context.din.poll_control_observation(),
            MidiProbePort::Mixer => {
                if let Some(mixer) = self.native_midi.as_mut() {
                    mixer.poll_control_observation()
                } else if let Some(mixer) = self.host_midi.as_mut() {
                    mixer.poll()
                } else {
                    context
                        .direct_mixer
                        .as_mut()
                        .ok_or("MIDI mixer session unavailable")?
                        .poll_control_observation()
                }
            }
        }
    }

    pub(super) fn run_midi_probe(
        &mut self,
        session: &Session,
        operation: Operation,
        guest_timeout_ms: u32,
        wall_timeout_s: u64,
        started: (Instant, u32),
    ) -> Result<Value, Box<dyn std::error::Error>> {
        let control = session.control();
        let guest = || {
            control
                .input_status(l6ctl::InputSource::Panel)
                .map(|s| s.guest_ms)
                .map_err(|e| e.to_string())
        };
        let indicators = || {
            control
                .info()
                .map(|s| s.panel_indicators)
                .map_err(|e| e.to_string())
        };
        if matches!(operation, Operation::MidiProbeBegin) {
            if self.midi_probe.is_some() {
                return Err("MIDI probe already active".into());
            }
            if session.model() != l6fw::Model::L6max {
                return Err("MIDI mixer/DIN probe requires L6max".into());
            }
            let endpoint = l6max_host::host_midi::endpoint_name(
                &l6max_host::host_midi::din_port_name_for_model_with_namespace(
                    session.model(),
                    midi_namespace().as_deref(),
                ),
            );
            let connect_deadline = started.0 + Duration::from_secs(10);
            let din = loop {
                if started.0.elapsed() > Duration::from_secs(wall_timeout_s)
                    || guest()?.wrapping_sub(started.1) > guest_timeout_ms
                {
                    return Err("MIDI probe deadline expired during DIN connection".into());
                }
                match MidiSession::connect_native(&endpoint) {
                    Ok(client) => break client,
                    Err(error)
                        if error.contains("missing native MIDI")
                            && Instant::now() < connect_deadline =>
                    {
                        session.poll()?;
                        thread::sleep(Duration::from_millis(25));
                    }
                    Err(error) => return Err(error.into()),
                }
            };
            let direct_mixer = if self.native_midi.is_none() && self.host_midi.is_none() {
                let budget = l6max_host::usb::Budget::new(
                    started.0 + Duration::from_secs(wall_timeout_s),
                    || false,
                );
                Some(
                    session
                        .control()
                        .direct_midi_before(1, &budget)
                        .map_err(std::io::Error::other)?,
                )
            } else {
                None
            };
            if started.0.elapsed() > Duration::from_secs(wall_timeout_s)
                || guest()?.wrapping_sub(started.1) > guest_timeout_ms
            {
                return Err("MIDI probe deadline expired while opening sessions".into());
            }
            let mixer = if direct_mixer.is_some() {
                "direct-usb"
            } else if self.native_midi.is_some() {
                "retained-native"
            } else {
                // The legacy session can itself be native or direct USB.
                "retained-legacy"
            };
            self.midi_probe = Some(MidiProbeContext {
                din,
                direct_mixer,
                start: started.0,
                guest_start: started.1,
                guest_timeout_ms,
                wall_timeout_s,
                last_send: None,
                last_ring: None,
                tap_at: None,
            });
            return Ok(json!({"din_endpoint":endpoint, "mixer_transport":mixer}));
        }
        let mut context = self.midi_probe.take().ok_or("MIDI probe is not active")?;
        if context.expired(guest()?) {
            return Err("MIDI probe shared deadline elapsed".into());
        }
        let result: Result<Value, Box<dyn std::error::Error>> = (|| match operation {
            Operation::MidiProbeSendCc {
                port,
                channel,
                controller,
                value,
                capture_ring_channel,
            } => {
                let cc = ControlChange {
                    channel,
                    controller,
                    value,
                };
                let bytes = cc.to_bytes().map_err(std::io::Error::other)?;
                // The original check sampled the ring immediately before each
                // send; a separate scenario checkpoint would introduce an
                // intervening runner step and could observe unrelated changes.
                let before = indicators()?;
                match port {
                    MidiProbePort::Din => context
                        .din
                        .send_control_change(cc)
                        .map_err(std::io::Error::other)?,
                    MidiProbePort::Mixer => {
                        if let Some(mixer) = self.native_midi.as_mut() {
                            mixer
                                .send_control_change(cc)
                                .map_err(std::io::Error::other)?;
                        } else if let Some(mixer) = self.host_midi.as_mut() {
                            mixer.send(&bytes).map_err(std::io::Error::other)?;
                        } else {
                            context
                                .direct_mixer
                                .as_mut()
                                .ok_or("MIDI mixer session unavailable")?
                                .send_control_change(cc)
                                .map_err(std::io::Error::other)?;
                        }
                    }
                }
                context.last_send = Some(Stimulus {
                    port,
                    sent: Instant::now(),
                    guest: guest()?,
                    ring_channel: capture_ring_channel,
                    before,
                });
                context.last_ring = None;
                Ok(
                    json!({"port":port,"cc":bytes,"ring_channel":capture_ring_channel,"indicators_before":before}),
                )
            }
            Operation::MidiProbeExpectRing {
                channel,
                changed,
                guest_ms,
                wall_ms,
            } => {
                let stimulus = context
                    .last_send
                    .ok_or("MIDI ring observation requires a preceding send")?;
                if stimulus.ring_channel != channel {
                    return Err("ring channel differs from the preceding MIDI stimulus".into());
                }
                let deadline = stimulus.sent + Duration::from_millis(u64::from(wall_ms));
                let index = channel
                    .checked_sub(1)
                    .ok_or("ring channel must be positive")?;
                let before = stimulus.before;
                let old = *l6max_host::indicators::rings(before)
                    .get(index)
                    .ok_or("ring channel exceeds model range")?;
                loop {
                    session.poll()?;
                    let after = indicators()?;
                    let current = *l6max_host::indicators::rings(after)
                        .get(index)
                        .ok_or("ring channel exceeds model range")?;
                    let now = guest()?;
                    let elapsed = now.wrapping_sub(stimulus.guest);
                    match ring_decision(old, current, changed, elapsed, guest_ms,
                        Instant::now() >= deadline, elapsed > context.guest_timeout_ms || context.expired(now)) {
                        RingDecision::UnexpectedChange => {
                            return Err(format!("channel {channel} ring unexpectedly changed: {old:#x} -> {current:#x}; before={before:08x?}, after={after:08x?}").into());
                        }
                        RingDecision::Changed => {
                        context.last_ring = Some((channel, current, now));
                        break Ok(
                            json!({"channel":channel,"expected_change":true,"port":stimulus.port,"before":old,"after":current,"indicators_before":before,"indicators_after":after}),
                        );
                        }
                        RingDecision::MissingChange => return Err(format!("channel {channel} ring did not change within {} guest ms: {old:#x}",guest_ms.unwrap()).into()),
                        RingDecision::Unchanged => {
                        context.last_ring = Some((channel, current, now));
                        break Ok(
                            json!({"channel":channel,"expected_change":false,"port":stimulus.port,"guest_ms":guest_ms,"before":old,"after":current}),
                        );
                        }
                        RingDecision::Timeout => return Err(format!("channel {channel} ring observation timed out; expected_change={changed}, before={old:#x}, last={current:#x}").into()),
                        RingDecision::Waiting => {}
                    }
                    thread::sleep(Duration::from_millis(5));
                }
            }
            Operation::MidiProbeWaitRingStable {
                channel,
                guest_ms,
                wall_ms,
            } => {
                let stimulus = context
                    .last_send
                    .ok_or("ring stability requires a preceding MIDI send")?;
                let deadline = stimulus.sent + Duration::from_millis(u64::from(wall_ms));
                let (saved_channel, mut settled, mut since) = context
                    .last_ring
                    .ok_or("ring stability requires a preceding ring observation")?;
                if saved_channel != channel {
                    return Err("ring stability channel differs from preceding observation".into());
                }
                let index = channel
                    .checked_sub(1)
                    .ok_or("ring channel must be positive")?;
                loop {
                    session.poll()?;
                    let now = guest()?;
                    let current = *l6max_host::indicators::rings(indicators()?)
                        .get(index)
                        .ok_or("ring channel exceeds model range")?;
                    if current != settled {
                        settled = current;
                        since = now;
                    } else if now.wrapping_sub(since) >= guest_ms {
                        break;
                    }
                    if Instant::now() >= deadline
                        || now.wrapping_sub(stimulus.guest) > context.guest_timeout_ms
                        || context.expired(now)
                    {
                        return Err(format!("channel {channel} ring did not remain stable for {guest_ms} guest ms; last={current:#x}").into());
                    }
                    thread::sleep(Duration::from_millis(5));
                }
                context.last_ring = Some((channel, settled, since));
                Ok(json!({"channel":channel,"guest_ms":guest_ms,"ring":settled}))
            }
            Operation::MidiProbeDrainAndTap {
                port,
                target,
                hold_ms,
            } => {
                let (start, guest_start, wall_timeout_s, guest_timeout_ms) = (
                    context.start,
                    context.guest_start,
                    context.wall_timeout_s,
                    context.guest_timeout_ms,
                );
                let drained = drain_before_tap(
                    || self.poll_midi_probe_port(&mut context, port),
                    || {
                        guest().map(|now| {
                            start.elapsed() > Duration::from_secs(wall_timeout_s)
                                || now.wrapping_sub(guest_start) > guest_timeout_ms
                        })
                    },
                )?;
                if context.expired(guest()?) {
                    return Err("MIDI probe deadline elapsed before tap".into());
                }
                let receipt = control.tap(target, hold_ms)?;
                context.tap_at = Some((port, Instant::now()));
                Ok(
                    json!({"port":port,"drained_count":drained,"target":receipt.target,"command":receipt.command,"hold_ms":hold_ms}),
                )
            }
            Operation::MidiProbeExpectNoEvents { port, wall_ms } => {
                let (tapped_port, tap) = context
                    .tap_at
                    .take()
                    .ok_or("MIDI no-events check requires a preceding tap")?;
                if tapped_port != port {
                    return Err("MIDI no-events port differs from the drained port".into());
                }
                let deadline = tap + Duration::from_millis(u64::from(wall_ms));
                let (start, guest_start, wall_timeout_s, guest_timeout_ms) = (
                    context.start,
                    context.guest_start,
                    context.wall_timeout_s,
                    context.guest_timeout_ms,
                );
                let echoed = observe_no_events(
                    deadline,
                    || session.poll().map(|_| ()),
                    || self.poll_midi_probe_port(&mut context, port),
                    Instant::now,
                    || {
                        guest().map(|now| {
                            start.elapsed() > Duration::from_secs(wall_timeout_s)
                                || now.wrapping_sub(guest_start) > guest_timeout_ms
                        })
                    },
                    thread::sleep,
                )?;
                if !echoed.is_empty() {
                    return Err(format!("unexpected DIN MIDI observations: {echoed:02x?}").into());
                }
                Ok(json!({"port":port,"wall_ms":wall_ms,"observed":echoed}))
            }
            Operation::MidiProbeEnd => Ok(json!({"transaction":"ended"})),
            _ => unreachable!(),
        })();
        match result {
            Ok(value) => {
                if context.expired(guest()?) {
                    return Err("MIDI probe shared deadline elapsed".into());
                }
                if !matches!(operation, Operation::MidiProbeEnd) {
                    self.midi_probe = Some(context);
                }
                Ok(value)
            }
            Err(error) => Err(error),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn ring_positive_and_negative_observations_preserve_guest_and_wall_windows() {
        use RingDecision::*;
        assert_eq!(
            ring_decision(1, 1, true, 499, Some(500), false, false),
            Waiting
        );
        assert_eq!(
            ring_decision(1, 2, true, 499, Some(500), false, false),
            Changed
        );
        assert_eq!(
            ring_decision(1, 1, true, 500, Some(500), false, false),
            MissingChange
        );
        assert_eq!(
            ring_decision(1, 1, false, 499, Some(500), false, false),
            Waiting
        );
        assert_eq!(
            ring_decision(1, 2, false, 499, Some(500), false, false),
            UnexpectedChange
        );
        assert_eq!(
            ring_decision(1, 1, false, 500, Some(500), false, false),
            Unchanged
        );
        assert_eq!(
            ring_decision(1, 1, false, 499, Some(500), true, false),
            Timeout
        );
        assert_eq!(ring_decision(1, 1, true, 10001, None, false, true), Timeout);
        // Preserve the original loop's change-before-deadline observation order.
        assert_eq!(
            ring_decision(1, 2, true, 500, Some(500), true, false),
            Changed
        );
    }

    #[test]
    fn no_events_observes_full_wall_window_and_keeps_queued_echo() {
        let start = Instant::now();
        let clock = Cell::new(start);
        let mut polls = 0;
        let result = observe_no_events(
            start + Duration::from_millis(600),
            || Ok(()),
            || {
                polls += 1;
                Ok((polls == 120).then_some([0xb0, 0x51, 1]))
            },
            || clock.get(),
            || Ok(false),
            |d| clock.set(clock.get() + d),
        )
        .unwrap();
        assert_eq!(
            clock.get().duration_since(start),
            Duration::from_millis(600)
        );
        assert_eq!(result, vec![[0xb0, 0x51, 1]]);
        assert_eq!(polls, 121);
        let late = observe_no_events(
            start,
            || Ok(()),
            || Ok(None),
            || start,
            || Ok(false),
            |_| {},
        );
        assert!(late.unwrap_err().contains("before observation began"));
    }

    #[test]
    fn no_events_propagates_transport_and_shared_deadline_errors() {
        let start = Instant::now();
        let error = observe_no_events(
            start + Duration::from_secs(1),
            || Ok(()),
            || Err("disconnected".into()),
            || start,
            || Ok(false),
            |_| {},
        )
        .unwrap_err();
        assert_eq!(error, "disconnected");
        let error = observe_no_events(
            start + Duration::from_secs(1),
            || Ok(()),
            || Ok(None),
            || start,
            || Ok(true),
            |_| {},
        )
        .unwrap_err();
        assert!(error.contains("shared deadline"));
    }

    #[test]
    fn no_events_checks_final_sleep_and_drains_backlog() {
        let start = Instant::now();
        let clock = Cell::new(start);
        let mut backlog = std::collections::VecDeque::from([[0xb0, 81, 1], [0xb0, 81, 2]]);
        let observed = observe_no_events(
            start + Duration::from_millis(10),
            || Ok(()),
            || {
                if clock.get().duration_since(start) >= Duration::from_millis(10) {
                    Ok(backlog.pop_front())
                } else {
                    Ok(None)
                }
            },
            || clock.get(),
            || Ok(false),
            |d| clock.set(clock.get() + d),
        )
        .unwrap();
        assert_eq!(observed, vec![[0xb0, 81, 1], [0xb0, 81, 2]]);
    }

    #[test]
    fn draining_rejects_an_empty_poll_that_crosses_the_deadline() {
        let elapsed = Cell::new(false);
        let error = drain_before_tap(
            || {
                elapsed.set(true);
                Ok(None)
            },
            || Ok(elapsed.get()),
        )
        .unwrap_err();
        assert!(error.contains("deadline elapsed"));
    }
}
