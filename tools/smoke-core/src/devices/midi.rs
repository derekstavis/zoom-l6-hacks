//! L6 DIN MIDI: sweeps and event dispatch into the mixer.
use super::execute::deadline_exceeded;
use super::*;

impl State {
    pub(super) fn run_l6_indicator_sample(
        &mut self,
        session: &Session,
        cc: u8,
        value: u8,
        guest_ms: u32,
        ring: Option<usize>,
        expected: Option<[u32; 8]>,
        reuse_previous: bool,
    ) -> Result<Value, String> {
        let scope = self
            .l6_indicator
            .as_ref()
            .ok_or("L6 indicator campaign is not active")?;
        let (started, guest_start, guest_budget, wall_budget) = (
            scope.start,
            scope.guest_start,
            scope.guest_budget,
            scope.wall_budget,
        );
        let check = || -> Result<(), String> {
            if indicator_deadline_expired(
                started.elapsed(),
                guest_start,
                guest_now(session),
                guest_budget,
                wall_budget,
            ) {
                Err("L6 indicator campaign exceeded its shared deadline".into())
            } else {
                Ok(())
            }
        };
        check()?;
        let control = session.control();
        let before = if reuse_previous {
            let (previous_cc, rows) = self
                .l6_indicator
                .as_ref()
                .and_then(|scope| scope.last_mute)
                .ok_or("missing previous L6 mute observation")?;
            if previous_cc != cc {
                return Err("L6 mute observation changed controller".into());
            }
            rows
        } else {
            control.info().map_err(|e| e.to_string())?.panel_indicators
        };
        check()?;
        self.host_midi_client()
            .ok_or("L6 indicator campaign requires a retained direct USB MIDI session")?
            .send(&[0xb0, cc, value])?;
        check()?;
        let start = guest_now(session);
        while guest_now(session).wrapping_sub(start) < guest_ms {
            check()?;
            session.poll()?;
            check()?;
            thread::sleep(Duration::from_millis(5));
        }
        check()?;
        let after = control.info().map_err(|e| e.to_string())?.panel_indicators;
        check()?;
        if expected.is_some() {
            self.l6_indicator.as_mut().unwrap().last_mute = Some((cc, after));
        }
        let rows = std::array::from_fn::<_, 8, _>(|i| before[i] ^ after[i]);
        if let Some(expected) = expected {
            if rows != expected {
                return Err(format!(
                    "L6 mute CC {cc:#04x} value {value} indicator mismatch: got {rows:?}, expected {expected:?}"
                ));
            }
        }
        let before_rings = l6ctl::decode_indicator_rings(before);
        let after_rings = l6ctl::decode_indicator_rings(after);
        let changes = std::array::from_fn::<_, 8, _>(|i| before_rings[i] ^ after_rings[i]);
        if let Some(ring) = ring {
            let (_, changed) = checked_ring_change(before, after, ring)
                .map_err(|error| format!("L6 CC {cc}: {error}"))?;
            self.l6_indicator.as_mut().unwrap().changed_segments |= changed;
        }
        Ok(
            json!({"cc":cc,"value":value,"before":before,"after":after,"changed_rows":rows,"changed_rings":changes}),
        )
    }
}

pub(super) fn indicator_deadline_expired(
    elapsed: Duration,
    start: u32,
    now: u32,
    guest_budget: u32,
    wall_budget: u64,
) -> bool {
    elapsed > Duration::from_secs(wall_budget) || now.wrapping_sub(start) > guest_budget
}

fn checked_ring_change(
    before: [u32; 8],
    after: [u32; 8],
    ring: usize,
) -> Result<([u32; 8], u32), String> {
    if !(1..=6).contains(&ring) {
        return Err("L6 ring must be in 1..=6".into());
    }
    let previous = l6ctl::decode_indicator_rings(before);
    let current = l6ctl::decode_indicator_rings(after);
    let changes = std::array::from_fn(|i| previous[i] ^ current[i]);
    if changes
        .iter()
        .enumerate()
        .any(|(i, value)| i != ring - 1 && *value != 0)
    {
        return Err(format!("unexpected decoded ring changes {changes:03x?}"));
    }
    Ok((changes, changes[ring - 1]))
}

#[cfg(test)]
mod l6_indicator_tests {
    use super::*;
    #[test]
    fn deadline_carries_guest_wrap_and_wall_budget() {
        assert!(!indicator_deadline_expired(
            Duration::from_secs(1),
            u32::MAX - 2,
            2,
            5,
            2
        ));
        assert!(indicator_deadline_expired(
            Duration::from_secs(1),
            u32::MAX - 2,
            3,
            5,
            2
        ));
        assert!(indicator_deadline_expired(
            Duration::from_millis(2001),
            0,
            1,
            5,
            2
        ));
        // The campaign may spend time between its 6,000 ms of observations,
        // but neither the guest nor wall ceiling may be exceeded.
        assert!(!indicator_deadline_expired(
            Duration::from_secs(89),
            0,
            9_999,
            10_000,
            90
        ));
        assert!(indicator_deadline_expired(
            Duration::from_secs(1),
            0,
            10_001,
            10_000,
            90
        ));
        assert!(indicator_deadline_expired(
            Duration::from_millis(90_001),
            0,
            1,
            10_000,
            90
        ));
    }
    #[test]
    fn ring_sample_rejects_other_ring_changes_and_invalid_ring() {
        let before = [0; 8];
        let mut after = before;
        let (row, bit) = l6max_host::indicators::RING_PINS[0][0];
        after[row] |= 1 << (bit ^ 7);
        assert_eq!(checked_ring_change(before, after, 1).unwrap().1, 1);
        assert!(checked_ring_change(before, after, 2).is_err());
        assert!(checked_ring_change(before, after, 0).is_err());
        assert!(checked_ring_change(before, after, 7).is_err());
    }
}

/// Retry endpoint discovery without letting the independent ten-second cap
/// exceed the scenario's overall wall or guest budget.
struct ConnectBudget {
    guest_start: u32,
    guest_timeout_ms: u32,
    overall_deadline: Instant,
}

fn retry_native_midi<T>(
    mut connect: impl FnMut() -> Result<T, String>,
    mut pump: impl FnMut() -> Result<(), String>,
    mut guest_now: impl FnMut() -> u32,
    mut now: impl FnMut() -> Instant,
    mut sleep: impl FnMut(Duration),
    budget: ConnectBudget,
) -> Result<T, String> {
    let deadline = (now() + Duration::from_secs(10)).min(budget.overall_deadline);
    loop {
        if now() >= deadline
            || guest_now().wrapping_sub(budget.guest_start) > budget.guest_timeout_ms
        {
            return Err(
                "native MIDI endpoint did not appear before the connection deadline".into(),
            );
        }
        match connect() {
            Ok(client) => {
                if now() >= deadline
                    || guest_now().wrapping_sub(budget.guest_start) > budget.guest_timeout_ms
                {
                    return Err(
                        "native MIDI endpoint appeared after the connection deadline".into(),
                    );
                }
                return Ok(client);
            }
            Err(error) if error.contains("missing native MIDI") => {
                pump()?;
                if guest_now().wrapping_sub(budget.guest_start) > budget.guest_timeout_ms {
                    return Err(
                        "native MIDI endpoint did not appear before the guest timeout".into(),
                    );
                }
                let remaining = deadline.saturating_duration_since(now());
                if remaining.is_zero() {
                    return Err(
                        "native MIDI endpoint did not appear before the connection deadline".into(),
                    );
                }
                sleep(Duration::from_millis(25).min(remaining));
            }
            Err(error) => return Err(error),
        }
    }
}

/// Consume legacy control observations until they have been quiet for the
/// declared wall interval. A continuous stream cannot exceed the budget.
fn drain_control_until_quiet(
    quiet: Duration,
    mut poll: impl FnMut() -> Result<Option<[u8; 3]>, String>,
    mut pump: impl FnMut() -> Result<(), String>,
    mut now: impl FnMut() -> Instant,
    mut expired: impl FnMut() -> bool,
    mut sleep: impl FnMut(Duration),
) -> Result<usize, String> {
    let mut quiet_since = now();
    let mut drained = 0;
    while now().duration_since(quiet_since) < quiet {
        pump()?;
        while poll()?.is_some() {
            quiet_since = now();
            drained += 1;
            if expired() {
                return Err("MIDI input did not become quiet before the shared deadline".into());
            }
        }
        if expired() {
            return Err("MIDI input did not become quiet before the shared deadline".into());
        }
        sleep(Duration::from_millis(5));
    }
    Ok(drained)
}

fn midi_collection_deadline(sent: Instant, wall_timeout: Duration, overall: Instant) -> Instant {
    (sent + wall_timeout).min(overall)
}

fn collect_control_observations(
    count: usize,
    mut poll: impl FnMut() -> Result<Option<[u8; 3]>, String>,
    mut pump: impl FnMut() -> Result<(), String>,
    mut expired: impl FnMut() -> bool,
    mut sleep: impl FnMut(Duration),
) -> Result<Vec<[u8; 3]>, String> {
    let mut replies = Vec::with_capacity(count);
    loop {
        pump()?;
        while let Some(message) = poll()? {
            replies.push(message);
            if replies.len() == count {
                return Ok(replies);
            }
        }
        if expired() {
            return Err(format!(
                "midi-sync returned {}/{} expected Control Change replies before deadline; observed {replies:02x?}",
                replies.len(),
                count
            ));
        }
        sleep(Duration::from_millis(5));
    }
}

/// One state-sync observation; the mixer session remains owned by the scenario.
pub(super) struct MidiSync {
    guest_start: u32,
    guest_timeout_ms: u32,
    wall_timeout_s: u64,
    overall_deadline: Instant,
    reply_started: Instant,
    expected_status: Option<u8>,
    remaining: Vec<(u8, usize)>,
    expected_count: usize,
    completed: bool,
    phase: SyncPhase,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SyncPhase {
    Prepared,
    Drained,
    Sent,
    Complete,
}

fn accept_sync_reply(status: Option<u8>, remaining: &mut [(u8, usize)], message: [u8; 3]) -> bool {
    match status {
        None => true,
        Some(status) if message[0] == status => {
            if let Some((_, count)) = remaining
                .iter_mut()
                .find(|(mapped, count)| *mapped == message[1] && *count > 0)
            {
                *count -= 1;
                true
            } else {
                false
            }
        }
        Some(_) => false,
    }
}

fn collect_sync_replies(
    scope: &mut MidiSync,
    mut poll: impl FnMut() -> Result<Option<[u8; 3]>, String>,
    mut pump: impl FnMut() -> Result<(), String>,
    mut now: impl FnMut() -> Instant,
    mut guest: impl FnMut() -> u32,
    mut sleep: impl FnMut(Duration),
) -> Result<Vec<[u8; 3]>, String> {
    let deadline = (scope.reply_started + Duration::from_secs(scope.wall_timeout_s))
        .min(scope.overall_deadline);
    let mut replies = Vec::with_capacity(scope.expected_count);
    loop {
        pump()?;
        while let Some(message) = poll()? {
            if now() >= deadline || guest().wrapping_sub(scope.guest_start) > scope.guest_timeout_ms
            {
                return Err(
                    "MIDI sync exceeded its shared deadline while collecting replies".into(),
                );
            }
            if accept_sync_reply(scope.expected_status, &mut scope.remaining, message) {
                replies.push(message);
            }
            if replies.len() == scope.expected_count {
                return Ok(replies);
            }
        }
        if now() >= deadline || guest().wrapping_sub(scope.guest_start) > scope.guest_timeout_ms {
            return Err(format!(
                "midi-sync returned {}/{} expected Control Change replies before deadline",
                replies.len(),
                scope.expected_count
            ));
        }
        sleep(Duration::from_millis(5));
    }
}

/// Timing shared by the native host MIDI scenario's individual steps.
pub(super) struct MidiRoundTrip {
    guest_start: u32,
    receive_start: u32,
    deadline: Instant,
    overall_deadline: Instant,
    guest_timeout_ms: u32,
    midi_sent_at: Option<Instant>,
}

impl MidiRoundTrip {
    fn receive_expired(&self, now: Instant, guest_now: u32) -> bool {
        now >= self.deadline.min(self.overall_deadline)
            || guest_now.wrapping_sub(self.receive_start) > self.guest_timeout_ms
            || self.exceeded(now, guest_now)
    }

    fn exceeded(&self, now: Instant, guest_now: u32) -> bool {
        now > self.overall_deadline
            || guest_now.wrapping_sub(self.guest_start) > self.guest_timeout_ms
    }
}

fn poll_timed_control_change(
    context: &MidiRoundTrip,
    mut poll: impl FnMut() -> Result<Option<l6ctl::midi::ControlChange>, String>,
    mut now: impl FnMut() -> Instant,
    mut guest_now: impl FnMut() -> u32,
) -> Result<Option<l6ctl::midi::ControlChange>, String> {
    let message = poll()?;
    if context.receive_expired(now(), guest_now()) {
        Ok(None)
    } else {
        Ok(message)
    }
}

#[derive(Debug, PartialEq, Eq)]
enum RingObservation {
    Changed,
    Waiting,
    Expired,
}

fn ring_observation(
    before: [u32; 8],
    after: [u32; 8],
    index: usize,
    now: Instant,
    deadline: Instant,
) -> RingObservation {
    if now >= deadline {
        RingObservation::Expired
    } else if l6ctl::decode_indicator_rings(after)[index]
        != l6ctl::decode_indicator_rings(before)[index]
    {
        RingObservation::Changed
    } else {
        RingObservation::Waiting
    }
}

#[derive(Debug, PartialEq, Eq)]
enum NegativeRingObservation {
    Changed,
    Waiting,
    Complete,
    BudgetExpired,
}

fn negative_ring_observation(
    before: [u32; 8],
    after: [u32; 8],
    index: usize,
    now: Instant,
    observation_end: Instant,
    overall_end: Instant,
    guest_expired: bool,
) -> NegativeRingObservation {
    if l6ctl::decode_indicator_rings(after)[index] != l6ctl::decode_indicator_rings(before)[index] {
        NegativeRingObservation::Changed
    } else if guest_expired || (now >= overall_end && overall_end < observation_end) {
        NegativeRingObservation::BudgetExpired
    } else if now >= observation_end {
        NegativeRingObservation::Complete
    } else {
        NegativeRingObservation::Waiting
    }
}

fn next_midi_encoder_steps(turned_up: &mut bool) -> i32 {
    *turned_up = !*turned_up;
    if *turned_up { -4 } else { 4 }
}

fn uses_midi_budget(operation: &Operation) -> bool {
    !matches!(
        operation,
        Operation::ExpectRingChanged {
            midi_transaction: false,
            ..
        } | Operation::EnqueueEncoder {
            midi_transaction: false,
            ..
        }
    )
}

impl State {
    fn end_midi_transaction(&mut self) -> Result<(), String> {
        self.midi_round_trip
            .take()
            .map(|_| ())
            .ok_or_else(|| "host MIDI transaction is not active".into())
    }

    pub(super) fn run_declarative_midi(
        &mut self,
        session: &Session,
        operation: Operation,
        guest_timeout_ms: u32,
        wall_timeout_s: u64,
    ) -> Result<Value, Box<dyn std::error::Error>> {
        use l6ctl::midi::{ControlChange, MidiSession};
        let control = session.control();
        control
            .input_status(l6ctl::InputSource::Panel)
            .map_err(|_| "MIDI test needs inputs")?;
        if matches!(operation, Operation::HostMidiConnect) {
            if self.host_midi.is_some() || self.native_midi.is_some() {
                return Err("host MIDI client already connected".into());
            }
            let started = Instant::now();
            let guest_start = guest_now(session);
            let endpoint = l6max_host::host_midi::endpoint_name(
                &l6max_host::host_midi::port_names_for_model_with_namespace(
                    session.model(),
                    midi_namespace().as_deref(),
                )[1],
            );
            // This retries exactly the same endpoint race as existing MIDI consumers.
            let client = retry_native_midi(
                || MidiSession::connect_native(&endpoint),
                || session.poll().map(|_| ()),
                || guest_now(session),
                Instant::now,
                thread::sleep,
                ConnectBudget {
                    guest_start,
                    guest_timeout_ms,
                    overall_deadline: started + Duration::from_secs(wall_timeout_s),
                },
            )
            .map_err(|error| {
                let bridge = session
                    .control()
                    .midi_bridge_status()
                    .unwrap_or_else(|| "USB MIDI bridge unavailable".into());
                std::io::Error::other(format!("{error}; bridge status: {bridge}"))
            })?;
            self.native_midi = Some(client);
            self.midi_round_trip = Some(MidiRoundTrip {
                guest_start,
                receive_start: guest_now(session),
                deadline: Instant::now() + Duration::from_secs(wall_timeout_s.min(30)),
                overall_deadline: started + Duration::from_secs(wall_timeout_s),
                guest_timeout_ms,
                midi_sent_at: None,
            });
            return Ok(json!({"endpoint": endpoint}));
        }
        if let Operation::DirectMidiConnect { cable } = operation {
            if cable > 15 {
                return Err("direct MIDI cable must be 0..15".into());
            }
            if self.midi_round_trip.is_some() || self.midi_sync.is_some() {
                return Err("cannot replace a MIDI session during a transaction".into());
            }
            if self.native_midi.is_some() && !self.direct_midi {
                return Err("cannot replace a retained native MIDI session with direct USB".into());
            }
            if self.direct_midi {
                if let Some(mut previous) = self.native_midi.take() {
                    previous.release();
                }
                self.direct_midi = false;
            }
            self.release_retained_host_midi();
            let started = Instant::now();
            let guest_start = guest_now(session);
            let control = session.control();
            let budget = control.usb_panel_budget(
                started + Duration::from_secs(wall_timeout_s),
                guest_start,
                guest_timeout_ms,
            )?;
            let client = control.direct_midi_before(cable, &budget)?;
            budget.check()?;
            let now = Instant::now();
            self.native_midi = Some(client);
            self.direct_midi = true;
            self.midi_round_trip = Some(MidiRoundTrip {
                guest_start,
                receive_start: guest_now(session),
                deadline: now + Duration::from_secs(wall_timeout_s.min(30)),
                overall_deadline: started + Duration::from_secs(wall_timeout_s),
                guest_timeout_ms,
                midi_sent_at: None,
            });
            return Ok(json!({"endpoint":format!("direct-usb-cable-{cable}")}));
        }
        if matches!(operation, Operation::HostMidiSyncBegin) {
            let overall_deadline = Instant::now() + Duration::from_secs(wall_timeout_s);
            if self.midi_sync.is_some() || self.midi_round_trip.is_some() {
                return Err("host MIDI transaction is already active".into());
            }
            if self.host_midi_client().is_none() {
                return Err("MIDI sync requires a connected MIDI session".into());
            }
            let guest_start = guest_now(session);
            let mut remaining = Vec::new();
            let (expected_count, expected_status) = if session.model() == l6fw::Model::L6 {
                let config = session.control().midi_config()?;
                for cc in config.cc_map.into_iter().filter(|cc| *cc != 0xff) {
                    if let Some((_, count)) = remaining.iter_mut().find(|(mapped, _)| *mapped == cc)
                    {
                        *count += 1;
                    } else {
                        remaining.push((cc, 1usize));
                    }
                }
                let count = remaining.iter().map(|(_, count)| count).sum::<usize>();
                if count == 0 {
                    return Err("L6 MIDI CC map has no enabled entries".into());
                }
                (count, Some(0xb0 | config.channel))
            } else {
                (94, None)
            };
            let transport = if self.native_midi.is_some() && !self.direct_midi {
                "retained-native"
            } else {
                "retained-legacy"
            };
            self.midi_sync = Some(MidiSync {
                guest_start,
                guest_timeout_ms,
                wall_timeout_s,
                overall_deadline,
                reply_started: Instant::now(),
                expected_status,
                remaining,
                expected_count,
                completed: false,
                phase: SyncPhase::Prepared,
            });
            return Ok(
                json!({"expected_replies": expected_count, "expected_status": expected_status, "transport": transport}),
            );
        }
        if self.midi_sync.is_some() {
            let scope = self.midi_sync.as_ref().unwrap();
            if Instant::now() >= scope.overall_deadline
                || guest_now(session).wrapping_sub(scope.guest_start) > scope.guest_timeout_ms
            {
                self.midi_sync = None;
                return Err("MIDI sync exceeded its shared deadline".into());
            }
            if matches!(operation, Operation::HostMidiEnd) {
                let scope = self.midi_sync.take().unwrap();
                if !scope.completed {
                    return Err("MIDI sync ended before its reply assertion".into());
                }
                return Ok(json!({"transaction":"ended"}));
            }
            if matches!(operation, Operation::HostMidiDrain { .. }) {
                if self.midi_sync.as_ref().unwrap().phase != SyncPhase::Prepared {
                    return Err("MIDI sync drain is out of sequence".into());
                }
                let quiet = match operation {
                    Operation::HostMidiDrain { quiet_wall_ms } => quiet_wall_ms,
                    _ => unreachable!(),
                };
                let deadline = self.midi_sync.as_ref().unwrap().overall_deadline;
                let start = self.midi_sync.as_ref().unwrap().guest_start;
                let guest_budget = self.midi_sync.as_ref().unwrap().guest_timeout_ms;
                let client = self
                    .host_midi_client()
                    .ok_or("host MIDI client unavailable")?;
                let drained = drain_control_until_quiet(
                    Duration::from_millis(u64::from(quiet)),
                    || client.poll(),
                    || session.poll().map(|_| ()),
                    Instant::now,
                    || {
                        Instant::now() >= deadline
                            || guest_now(session).wrapping_sub(start) > guest_budget
                    },
                    thread::sleep,
                )?;
                if Instant::now() >= deadline
                    || guest_now(session).wrapping_sub(start) > guest_budget
                {
                    return Err("MIDI sync exceeded its shared deadline".into());
                }
                self.midi_sync.as_mut().unwrap().phase = SyncPhase::Drained;
                return Ok(json!({"drained":drained,"quiet_wall_ms":quiet}));
            }
            if let Operation::HostMidiSendCc {
                channel,
                controller,
                value,
            } = operation
            {
                if self.midi_sync.as_ref().unwrap().phase != SyncPhase::Drained {
                    return Err("MIDI sync send must follow a quiet drain".into());
                }
                let message = ControlChange {
                    channel,
                    controller,
                    value,
                }
                .to_bytes()
                .map_err(std::io::Error::other)?;
                let scope = self.midi_sync.as_ref().unwrap();
                if Instant::now() >= scope.overall_deadline
                    || guest_now(session).wrapping_sub(scope.guest_start) > scope.guest_timeout_ms
                {
                    return Err("MIDI sync exceeded its shared deadline before send".into());
                }
                self.host_midi_client()
                    .ok_or("host MIDI client unavailable")?
                    .send(&message)
                    .map_err(std::io::Error::other)?;
                let scope = self.midi_sync.as_mut().unwrap();
                scope.reply_started = Instant::now();
                scope.phase = SyncPhase::Sent;
                return Ok(json!({"midi_cc":message}));
            }
            if matches!(operation, Operation::HostMidiExpectSync) {
                let mut scope = self.midi_sync.take().unwrap();
                if !matches!(scope.phase, SyncPhase::Prepared | SyncPhase::Sent) {
                    return Err("MIDI sync reply assertion is out of sequence".into());
                }
                let client = self
                    .host_midi_client()
                    .ok_or("host MIDI client unavailable")?;
                let replies = collect_sync_replies(
                    &mut scope,
                    || client.poll(),
                    || session.poll().map(|_| ()),
                    Instant::now,
                    || guest_now(session),
                    thread::sleep,
                )?;
                scope.completed = true;
                scope.phase = SyncPhase::Complete;
                let expected = scope.expected_count;
                self.midi_sync = Some(scope);
                return Ok(
                    json!({"expected_replies":expected,"reply_count":replies.len(),"replies":replies}),
                );
            }

            self.midi_sync = None;
            return Err("MIDI sync transaction interrupted by another operation".into());
        }
        if matches!(operation, Operation::HostMidiExpectSync) {
            return Err("MIDI sync reply assertion requires an active sync transaction".into());
        }
        if matches!(operation, Operation::HostMidiEnd) {
            self.end_midi_transaction()?;
            return Ok(json!({"transaction": "ended"}));
        }
        if matches!(operation, Operation::HostMidiBegin) {
            if self.native_midi.is_none() {
                return Err("host-midi-begin requires a connected native MIDI session".into());
            }
            if self.midi_round_trip.is_some() {
                return Err("host MIDI transaction is already active".into());
            }
            let now = Instant::now();
            let guest = guest_now(session);
            self.midi_round_trip = Some(MidiRoundTrip {
                guest_start: guest,
                receive_start: guest,
                deadline: now + Duration::from_secs(wall_timeout_s.min(30)),
                overall_deadline: now + Duration::from_secs(wall_timeout_s),
                guest_timeout_ms,
                midi_sent_at: None,
            });
            return Ok(json!({"transaction": "started"}));
        }
        let independent_operation = !uses_midi_budget(&operation);
        if matches!(
            operation,
            Operation::EnqueueEncoder {
                midi_transaction: true,
                ..
            }
        ) && self.midi_round_trip.is_none()
        {
            return Err("encoder input requires an active host MIDI transaction".into());
        }
        if let Some(context) = &self.midi_round_trip {
            if !independent_operation && context.exceeded(Instant::now(), guest_now(session)) {
                return Err("device operation exceeded its shared deadline".into());
            }
        }
        if matches!(operation, Operation::MidiEncoderAlternate) {
            let context = self
                .midi_round_trip
                .as_ref()
                .ok_or("encoder input requires an active MIDI transaction")?;
            if context.receive_expired(Instant::now(), guest_now(session)) {
                return Err("host MIDI receive deadline elapsed before encoder input".into());
            }
            let steps = next_midi_encoder_steps(&mut self.encoder_turned_up);
            let receipt = session.control().encoder(1, steps)?;
            return Ok(json!({"channel":receipt.channel,"steps":receipt.steps}));
        }
        let result = match operation {
            Operation::EnqueueEncoder {
                channel,
                steps,
                midi_transaction,
            } => {
                if midi_transaction
                    && self
                        .midi_round_trip
                        .as_ref()
                        .is_some_and(|context| Instant::now() >= context.deadline)
                {
                    return Err("host MIDI receive deadline elapsed before encoder input".into());
                }
                let receipt = session.control().encoder(channel as u32, steps)?;
                json!({"channel": receipt.channel, "steps": receipt.steps})
            }
            Operation::HostMidiDrain { quiet_wall_ms } => {
                let context = self
                    .midi_round_trip
                    .as_ref()
                    .ok_or("MIDI drain requires an active transaction")?;
                let client = self
                    .native_midi
                    .as_mut()
                    .ok_or("host MIDI client unavailable")?;
                let drained = drain_control_until_quiet(
                    Duration::from_millis(u64::from(quiet_wall_ms)),
                    || client.poll_control_observation(),
                    || session.poll().map(|_| ()),
                    Instant::now,
                    || context.exceeded(Instant::now(), guest_now(session)),
                    thread::sleep,
                )?;
                json!({"drained": drained, "quiet_wall_ms": quiet_wall_ms})
            }
            Operation::HostMidiExpectCount { count } => {
                let context = self
                    .midi_round_trip
                    .as_ref()
                    .ok_or("MIDI collection requires an active transaction")?;
                let client = self
                    .native_midi
                    .as_mut()
                    .ok_or("host MIDI client unavailable")?;
                let deadline = midi_collection_deadline(
                    context.midi_sent_at.unwrap_or_else(Instant::now),
                    Duration::from_secs(wall_timeout_s),
                    context.overall_deadline,
                );
                let replies = collect_control_observations(
                    count,
                    || client.poll_control_observation(),
                    || session.poll().map(|_| ()),
                    || {
                        Instant::now() >= deadline
                            || context.exceeded(Instant::now(), guest_now(session))
                    },
                    thread::sleep,
                )?;
                json!({"expected_replies": count, "reply_count": replies.len(), "replies": replies})
            }
            Operation::HostMidiExpectCc {
                channel,
                controller,
            } => {
                let context = self.midi_round_trip.as_ref().ok_or(
                    "host MIDI transaction is not active; run host-midi-connect or host-midi-begin",
                )?;
                let client = self
                    .native_midi
                    .as_mut()
                    .ok_or("host MIDI client unavailable")?;
                let mut found = None;
                let mut observed = std::collections::VecDeque::new();
                while !context.receive_expired(Instant::now(), guest_now(session)) {
                    session.poll()?;
                    if context.receive_expired(Instant::now(), guest_now(session)) {
                        break;
                    }
                    if let Some(message) = poll_timed_control_change(
                        context,
                        || client.poll_control_change(),
                        Instant::now,
                        || guest_now(session),
                    )
                    .map_err(std::io::Error::other)?
                    {
                        let bytes = message.to_bytes().map_err(std::io::Error::other)?;
                        if message.channel == channel && message.controller == controller {
                            found = Some(bytes);
                            break;
                        }
                        if observed.len() == 8 {
                            observed.pop_front();
                        }
                        observed.push_back(bytes);
                    }
                    thread::sleep(Duration::from_millis(5));
                }
                let message = found.ok_or_else(|| std::io::Error::other(format!(
                    "expected host MIDI CC channel {channel}, controller {controller}; observed {observed:02x?}"
                )))?;
                json!({"encoder_cc": message})
            }
            Operation::HostMidiSettle { guest_ms } => {
                let context = self.midi_round_trip.as_ref().ok_or(
                    "host MIDI transaction is not active; run host-midi-connect or host-midi-begin",
                )?;
                let start = guest_now(session);
                while guest_now(session).wrapping_sub(start) < guest_ms {
                    session.poll()?;
                    let now = guest_now(session);
                    if Instant::now() >= context.deadline.min(context.overall_deadline)
                        || context.exceeded(Instant::now(), now)
                    {
                        return Err(format!(
                            "guest did not advance {guest_ms} ms before the shared MIDI deadline"
                        )
                        .into());
                    }
                    thread::sleep(Duration::from_millis(5));
                }
                json!({"guest_ms": guest_ms})
            }
            Operation::HostMidiSendCc {
                channel,
                controller,
                value,
            } => {
                let context = self.midi_round_trip.as_mut().ok_or(
                    "host MIDI transaction is not active; run host-midi-connect or host-midi-begin",
                )?;
                let message = ControlChange {
                    channel,
                    controller,
                    value,
                };
                self.native_midi
                    .as_mut()
                    .ok_or("host MIDI client unavailable")?
                    .send_control_change(message)
                    .map_err(std::io::Error::other)?;
                context.midi_sent_at = Some(Instant::now());
                json!({"midi_cc": message.to_bytes().map_err(std::io::Error::other)?})
            }
            Operation::ExpectRingChanged {
                channel,
                before,
                expected_change,
                observe_wall_ms,
                midi_transaction,
            } => {
                let duration = observe_wall_ms
                    .map(|ms| Duration::from_millis(u64::from(ms)))
                    .unwrap_or_else(|| Duration::from_secs(wall_timeout_s));
                let deadline = if midi_transaction {
                    let context = self
                        .midi_round_trip
                        .as_ref()
                        .ok_or("ring assertion requires an active host MIDI transaction")?;
                    let sent = context
                        .midi_sent_at
                        .ok_or("ring assertion requires a host MIDI CC sent in this transaction")?;
                    (sent + duration).min(context.overall_deadline)
                } else {
                    Instant::now() + duration
                };
                let index = channel
                    .checked_sub(1)
                    .ok_or("ring channel must be positive")?;
                let mut negative_start = None;
                let after = loop {
                    session.poll()?;
                    let after = control.info()?.panel_indicators;
                    let observation =
                        ring_observation(before, after, index, Instant::now(), deadline);
                    if !expected_change {
                        let context = self
                            .midi_round_trip
                            .as_ref()
                            .ok_or("negative ring observation requires a MIDI transaction")?;
                        context
                            .midi_sent_at
                            .ok_or("negative ring observation requires a sent CC")?;
                        let sampled_at = Instant::now();
                        let started = match negative_start {
                            Some(started) => started,
                            None => {
                                negative_start = Some(sampled_at);
                                sampled_at
                            }
                        };
                        match negative_ring_observation(before, after, index, sampled_at, started + duration,
                            context.overall_deadline,
                            guest_now(session).wrapping_sub(context.guest_start) > context.guest_timeout_ms) {
                            NegativeRingObservation::Changed => return Err(format!("channel {channel} ring unexpectedly changed during negative observation").into()),
                            NegativeRingObservation::BudgetExpired => return Err("shared MIDI deadline ended before negative observation completed".into()),
                            NegativeRingObservation::Complete => break after,
                            NegativeRingObservation::Waiting => { thread::sleep(Duration::from_millis(5)); continue; }
                        }
                    }
                    if midi_transaction
                        && self.midi_round_trip.as_ref().is_some_and(|context| {
                            context.exceeded(Instant::now(), guest_now(session))
                        })
                    {
                        return Err("device operation exceeded its shared deadline".into());
                    }
                    if observation == RingObservation::Expired {
                        return Err(std::io::Error::other(format!("channel {channel} ring did not change; expected different ring from baseline; before={before:08x?}, last_after={after:08x?}")).into());
                    }
                    if observation == RingObservation::Changed {
                        break after;
                    }
                    thread::sleep(Duration::from_millis(5));
                };
                json!({"channel": channel, "indicator_before": before, "indicator_after": after})
            }
            _ => unreachable!(),
        };
        if let Some(context) = &self.midi_round_trip {
            if !independent_operation && context.exceeded(Instant::now(), guest_now(session)) {
                return Err("device operation exceeded its shared deadline".into());
            }
        }
        Ok(result)
    }
}

#[cfg(test)]
mod declarative_midi_tests {
    use super::*;

    #[test]
    fn sync_reply_filter_preserves_l6_multiset_and_l6max_count_semantics() {
        let mut remaining = [(83, 2), (84, 1)];
        assert!(!accept_sync_reply(
            Some(0xb2),
            &mut remaining,
            [0xb1, 83, 4]
        ));
        assert!(accept_sync_reply(Some(0xb2), &mut remaining, [0xb2, 83, 4]));
        assert!(accept_sync_reply(Some(0xb2), &mut remaining, [0xb2, 83, 4]));
        assert!(!accept_sync_reply(
            Some(0xb2),
            &mut remaining,
            [0xb2, 83, 4]
        ));
        assert!(accept_sync_reply(Some(0xb2), &mut remaining, [0xb2, 84, 5]));
        assert!(accept_sync_reply(None, &mut [], [0xb9, 127, 0]));
    }

    #[test]
    fn sync_stops_during_continuous_unmatched_traffic() {
        let start = Instant::now();
        let mut scope = MidiSync {
            guest_start: 0,
            guest_timeout_ms: 100,
            wall_timeout_s: 2,
            overall_deadline: start + Duration::from_secs(2),
            reply_started: start,
            expected_status: Some(0xb0),
            remaining: vec![(83, 1)],
            expected_count: 1,
            completed: false,
            phase: SyncPhase::Sent,
        };
        let polls = std::cell::Cell::new(0);
        let result = collect_sync_replies(
            &mut scope,
            || {
                let count = polls.get() + 1;
                assert!(count < 10, "unmatched backlog was not bounded");
                polls.set(count);
                Ok(Some([0xb1, 83, 0]))
            },
            || Ok(()),
            || start + Duration::from_secs(polls.get() as u64),
            || 0,
            |_| {},
        );
        assert!(result.unwrap_err().contains("deadline"));
        assert_eq!(polls.get(), 2);
    }
    use std::{
        collections::VecDeque,
        sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        },
    };

    struct FakeClient {
        queue: VecDeque<[u8; 3]>,
        released: Arc<AtomicBool>,
    }
    impl crate::host_midi::Client for FakeClient {
        fn send(&mut self, _: &[u8]) -> Result<(), String> {
            Ok(())
        }
        fn poll(&mut self) -> Result<Option<[u8; 3]>, String> {
            Ok(self.queue.pop_front())
        }
        fn release(&mut self) {
            self.released.store(true, Ordering::SeqCst);
        }
    }

    #[test]
    fn drain_uses_wall_quiet_since_the_last_observation() {
        use std::cell::{Cell, RefCell};
        let start = Instant::now();
        let now = Cell::new(start);
        let queue = RefCell::new(VecDeque::from([
            Some([0xb0, 1, 1]),
            None,
            Some([0xb0, 2, 2]),
            None,
            None,
            None,
        ]));
        let count = drain_control_until_quiet(
            Duration::from_millis(10),
            || Ok(queue.borrow_mut().pop_front().flatten()),
            || Ok(()),
            || now.get(),
            || false,
            |duration| now.set(now.get() + duration),
        )
        .unwrap();
        assert_eq!(count, 2);
        assert_eq!(now.get(), start + Duration::from_millis(15));
    }

    #[test]
    fn collector_preserves_order_and_unread_queue_and_reports_partial_timeout() {
        use std::cell::{Cell, RefCell};
        let queue = RefCell::new(VecDeque::from([[0xb0, 1, 1], [0xb0, 2, 2], [0xb0, 3, 3]]));
        let received = collect_control_observations(
            2,
            || Ok(queue.borrow_mut().pop_front()),
            || Ok(()),
            || false,
            |_| {},
        )
        .unwrap();
        assert_eq!(received, vec![[0xb0, 1, 1], [0xb0, 2, 2]]);
        assert_eq!(queue.borrow_mut().pop_front(), Some([0xb0, 3, 3]));
        let attempts = Cell::new(0);
        let error = collect_control_observations(
            2,
            || {
                let n = attempts.get();
                attempts.set(n + 1);
                Ok((n == 0).then_some([0xb0, 4, 4]))
            },
            || Ok(()),
            || attempts.get() >= 2,
            |_| {},
        )
        .unwrap_err();
        assert!(error.contains("1/2"));
        assert!(error.contains("b0"));
    }

    #[test]
    fn collector_retains_legacy_completion_order_and_overall_deadline() {
        let start = Instant::now();
        let sent = start + Duration::from_millis(500);
        assert_eq!(
            midi_collection_deadline(
                sent,
                Duration::from_secs(120),
                start + Duration::from_secs(120)
            ),
            start + Duration::from_secs(120)
        );
        assert_eq!(
            midi_collection_deadline(
                sent,
                Duration::from_secs(3),
                start + Duration::from_secs(120)
            ),
            sent + Duration::from_secs(3)
        );
        // The prior collector checks a complete response before checking its
        // timeout. Preserve that observed order in this extraction.
        let result =
            collect_control_observations(1, || Ok(Some([0xb0, 1, 2])), || Ok(()), || true, |_| {})
                .unwrap();
        assert_eq!(result, vec![[0xb0, 1, 2]]);
    }

    #[test]
    fn drain_stops_continuous_traffic_when_budget_expires() {
        use std::cell::Cell;
        let n = Cell::new(0);
        let error = drain_control_until_quiet(
            Duration::from_millis(500),
            || {
                n.set(n.get() + 1);
                Ok(Some([0xb0, 1, 1]))
            },
            || Ok(()),
            Instant::now,
            || n.get() == 3,
            |_| {},
        )
        .unwrap_err();
        assert!(error.contains("shared deadline"));
        assert_eq!(n.get(), 3);
    }

    #[test]
    fn repeated_round_trips_alternate_encoder_direction() {
        let mut turned_up = false;
        assert_eq!(
            [
                next_midi_encoder_steps(&mut turned_up),
                next_midi_encoder_steps(&mut turned_up),
                next_midi_encoder_steps(&mut turned_up)
            ],
            [-4, 4, -4]
        );
    }

    #[test]
    fn negative_ring_requires_full_window_and_rejects_changes_or_budget_expiry() {
        let start = Instant::now();
        let end = start + Duration::from_secs(3);
        let budget = start + Duration::from_secs(10);
        let before = [0; 8];
        let changed = [u32::MAX; 8];
        assert_eq!(
            negative_ring_observation(before, before, 0, start, end, budget, false),
            NegativeRingObservation::Waiting
        );
        assert_eq!(
            negative_ring_observation(before, before, 0, end, end, budget, false),
            NegativeRingObservation::Complete
        );
        assert_eq!(
            negative_ring_observation(before, changed, 0, end, end, budget, false),
            NegativeRingObservation::Changed
        );
        assert_eq!(
            negative_ring_observation(
                before,
                before,
                0,
                start + Duration::from_secs(1),
                end,
                start + Duration::from_secs(1),
                false
            ),
            NegativeRingObservation::BudgetExpired
        );
        assert_eq!(
            negative_ring_observation(before, before, 0, start, end, budget, true),
            NegativeRingObservation::BudgetExpired
        );
    }

    #[test]
    fn consecutive_direct_round_trips_release_the_previous_session_before_reopening() {
        let first_released = Arc::new(AtomicBool::new(false));
        let mut state = State::default();
        state.host_midi = Some(Box::new(FakeClient {
            queue: VecDeque::from([[0xb0, 83, 1]]),
            released: first_released.clone(),
        }));
        state.release_retained_host_midi();
        assert!(first_released.load(Ordering::SeqCst));
        assert!(state.host_midi.is_none());
        state.host_midi = Some(Box::new(FakeClient {
            queue: VecDeque::from([[0xb0, 81, 2]]),
            released: Arc::new(AtomicBool::new(false)),
        }));
        assert_eq!(
            state.host_midi.as_mut().unwrap().poll().unwrap(),
            Some([0xb0, 81, 2])
        );
        assert_eq!(state.host_midi.as_mut().unwrap().poll().unwrap(), None);
    }

    #[test]
    fn restart_releases_subscription_and_discards_queued_messages_and_deadline() {
        let released = Arc::new(AtomicBool::new(false));
        let client = FakeClient {
            queue: VecDeque::from([[0xb0, 83, 1]]),
            released: released.clone(),
        };
        let start = Instant::now();
        let mut state = State::default();
        state.host_midi = Some(Box::new(client));
        state.midi_round_trip = Some(MidiRoundTrip {
            guest_start: 0,
            receive_start: 0,
            deadline: start,
            overall_deadline: start,
            guest_timeout_ms: 1,
            midi_sent_at: None,
        });
        state.restarted().unwrap();
        assert!(released.load(Ordering::SeqCst));
        assert!(state.native_midi.is_none());
        assert!(state.midi_round_trip.is_none());
        let replacement = FakeClient {
            queue: VecDeque::from([[0xb0, 81, 2]]),
            released: Arc::new(AtomicBool::new(false)),
        };
        state.host_midi = Some(Box::new(replacement));
        assert_eq!(
            state.host_midi.as_mut().unwrap().poll().unwrap(),
            Some([0xb0, 81, 2])
        );
        assert!(state.host_midi.as_mut().unwrap().poll().unwrap().is_none());
    }

    #[test]
    fn endpoint_retry_respects_short_wall_and_guest_budgets() {
        use std::cell::Cell;
        let start = Instant::now();
        let now = Cell::new(start);
        let attempts = Cell::new(0);
        let result: Result<(), String> = retry_native_midi(
            || {
                attempts.set(attempts.get() + 1);
                Err("missing native MIDI source".into())
            },
            || Ok(()),
            || 0,
            || now.get(),
            |duration| now.set(now.get() + duration),
            ConnectBudget {
                guest_start: 0,
                guest_timeout_ms: 1000,
                overall_deadline: start + Duration::from_millis(4),
            },
        );
        assert!(result.unwrap_err().contains("connection deadline"));
        assert_eq!(attempts.get(), 1);
        assert_eq!(now.get(), start + Duration::from_millis(4));

        let now = Cell::new(start);
        let result: Result<(), String> = retry_native_midi(
            || Err("missing native MIDI source".into()),
            || Ok(()),
            || 0,
            || now.get(),
            |duration| now.set(now.get() + duration),
            ConnectBudget {
                guest_start: 0,
                guest_timeout_ms: 1000,
                overall_deadline: start + Duration::from_secs(120),
            },
        );
        assert!(result.unwrap_err().contains("connection deadline"));
        assert_eq!(now.get(), start + Duration::from_secs(10));

        let guest = Cell::new(0);
        let result: Result<(), String> = retry_native_midi(
            || Err("missing native MIDI source".into()),
            || {
                guest.set(2);
                Ok(())
            },
            || guest.get(),
            || start,
            |_| panic!("guest deadline should stop before sleep"),
            ConnectBudget {
                guest_start: 0,
                guest_timeout_ms: 1,
                overall_deadline: start + Duration::from_secs(120),
            },
        );
        assert!(result.unwrap_err().contains("guest timeout"));
    }

    #[test]
    fn unrelated_encoder_and_ring_actions_do_not_use_midi_budget() {
        assert!(!uses_midi_budget(&Operation::EnqueueEncoder {
            channel: 1,
            steps: 1,
            midi_transaction: false
        }));
        assert!(uses_midi_budget(&Operation::EnqueueEncoder {
            channel: 1,
            steps: 1,
            midi_transaction: true
        }));
        assert!(!uses_midi_budget(&Operation::ExpectRingChanged {
            channel: 1,
            before: [0; 8],
            expected_change: true,
            observe_wall_ms: None,
            midi_transaction: false
        }));
        assert!(uses_midi_budget(&Operation::ExpectRingChanged {
            channel: 1,
            before: [0; 8],
            expected_change: true,
            observe_wall_ms: None,
            midi_transaction: true
        }));
    }

    #[test]
    fn matching_cc_returned_after_poll_deadline_is_not_accepted() {
        use std::cell::Cell;
        let start = Instant::now();
        let now = Cell::new(start);
        let context = MidiRoundTrip {
            guest_start: 0,
            receive_start: 0,
            deadline: start + Duration::from_secs(3),
            overall_deadline: start + Duration::from_secs(10),
            guest_timeout_ms: 1000,
            midi_sent_at: None,
        };
        let message = l6ctl::midi::ControlChange {
            channel: 0,
            controller: 81,
            value: 7,
        };
        let received = poll_timed_control_change(
            &context,
            || {
                now.set(start + Duration::from_secs(3));
                Ok(Some(message))
            },
            || now.get(),
            || 0,
        )
        .unwrap();
        assert!(received.is_none());
        let received =
            poll_timed_control_change(&context, || Ok(Some(message)), || start, || 0).unwrap();
        assert_eq!(received, Some(message));
    }

    #[test]
    fn ring_changes_must_arrive_before_the_declared_deadline() {
        let start = Instant::now();
        let deadline = start + Duration::from_secs(3);
        let before = [0; 8];
        let mut changed = before;
        changed[0] = 1 << (2 ^ 7);
        assert_eq!(
            ring_observation(before, before, 0, start, deadline),
            RingObservation::Waiting
        );
        assert_eq!(
            ring_observation(before, changed, 0, start, deadline),
            RingObservation::Changed
        );
        assert_eq!(
            ring_observation(before, changed, 0, deadline, deadline),
            RingObservation::Expired
        );
    }

    #[test]
    fn finishing_transaction_drops_budget_but_retains_pending_queue() {
        let start = Instant::now();
        let released = Arc::new(AtomicBool::new(false));
        let mut state = State::default();
        state.host_midi = Some(Box::new(FakeClient {
            queue: VecDeque::from([[0xb0, 81, 7]]),
            released: released.clone(),
        }));
        state.midi_round_trip = Some(MidiRoundTrip {
            guest_start: 0,
            receive_start: 0,
            deadline: start,
            overall_deadline: start,
            guest_timeout_ms: 1,
            midi_sent_at: Some(start),
        });
        state.end_midi_transaction().unwrap();
        assert!(state.midi_round_trip.is_none());
        assert!(
            state
                .end_midi_transaction()
                .unwrap_err()
                .contains("not active")
        );
        assert!(!released.load(Ordering::SeqCst));
        assert_eq!(
            state.host_midi.as_mut().unwrap().poll().unwrap(),
            Some([0xb0, 81, 7])
        );
    }

    #[test]
    fn shared_overall_budget_includes_guest_wraparound() {
        let start = Instant::now();
        let context = MidiRoundTrip {
            guest_start: u32::MAX - 5,
            receive_start: 10,
            deadline: start + Duration::from_secs(30),
            overall_deadline: start + Duration::from_secs(120),
            guest_timeout_ms: 20,
            midi_sent_at: None,
        };
        assert!(!context.receive_expired(start + Duration::from_secs(29), 14));
        assert!(context.receive_expired(start + Duration::from_secs(30), 14));
        assert!(context.receive_expired(start + Duration::from_secs(1), 31));
        assert!(!context.exceeded(start + Duration::from_secs(119), 14));
        assert!(context.exceeded(start + Duration::from_secs(121), 14));
        assert!(context.exceeded(start + Duration::from_secs(1), 15));
    }
}

/// One disposable guest's injected L6 MIDI transaction. Each scenario action
/// exposes a distinct observation while preserving one shared time budget.
pub(super) struct L6MidiProbe {
    debug: l6max_host::debug::Session,
    target: u32,
    binding: l6max_host::debug::L6MidiBinding,
    wall_deadline: Instant,
    guest_start: u32,
    guest_timeout: u32,
    event: Vec<u8>,
    expected: Vec<u8>,
    sender_return: Option<(u32, usize)>,
    phase: u8,
}
fn l6_midi_expected_ccs(map: &[u8]) -> Result<Vec<u8>, String> {
    if map.len() != 54 {
        return Err("L6 MIDI CC map has the wrong size".into());
    }
    let result: Vec<u8> = (0..6)
        .flat_map(|channel| (0..9).map(move |group| group * 6 + channel))
        .map(|index| map[index])
        .filter(|value| *value != 0xff)
        .collect();
    if result.is_empty() {
        return Err("L6 MIDI CC map has no enabled entries".into());
    }
    Ok(result)
}
fn l6_accept_ordered_cc(expected: &[u8], index: &mut usize, cc: u8) -> bool {
    if expected.get(*index) == Some(&cc) {
        *index += 1;
    }
    *index == expected.len()
}
fn l6_reply_error(
    attempt: usize,
    matched: usize,
    total: usize,
    operation: &str,
    error: &str,
) -> String {
    format!("L6 MIDI reply attempt {attempt} (matched {matched}/{total}), {operation}: {error}")
}
impl L6MidiProbe {
    fn remaining_ms(&self, session: &Session, cap_ms: u64) -> Result<u64, String> {
        if guest_now(session).wrapping_sub(self.guest_start) > self.guest_timeout {
            return Err("L6 MIDI probe exceeded its guest deadline".into());
        }
        let left = self
            .wall_deadline
            .checked_duration_since(Instant::now())
            .ok_or("L6 MIDI probe exceeded its wall deadline")?;
        let ms = left.as_millis().min(u128::from(cap_ms)) as u64;
        if ms == 0 {
            return Err("L6 MIDI probe exceeded its wall deadline".into());
        }
        Ok(ms)
    }
    fn command(
        &mut self,
        session: &Session,
        command: l6max_host::debug::Command,
    ) -> Result<l6max_host::debug::Observation, String> {
        self.remaining_ms(session, 60_000)?;
        let deadline = self.wall_deadline;
        let start = self.guest_start;
        let timeout = self.guest_timeout;
        let value = self.debug.execute_cancellable(command, || {
            Instant::now() >= deadline || guest_now(session).wrapping_sub(start) > timeout
        })?;
        self.remaining_ms(session, 60_000)?;
        Ok(value)
    }
    fn advance(&mut self, session: &Session, cap_ms: u64) -> Result<(), String> {
        let timeout_ms = self.remaining_ms(session, cap_ms)?;
        self.command(session, l6max_host::debug::Command::Continue { timeout_ms })?;
        Ok(())
    }
}

impl State {
    pub(super) fn run_l6_midi_probe(
        &mut self,
        session: &Session,
        options: &Options,
        operation: Operation,
        guest_timeout_ms: u32,
        wall_timeout_s: u64,
    ) -> Result<Value, String> {
        use firmware_api::l6::{idle_loop, midi_in, midi_send};
        use l6max_host::debug::{Command as C, L6MidiBinding as B, Observation as O};
        if matches!(operation, Operation::L6MidiProbeBegin) {
            if self.l6_midi_probe.is_some() || session.model() != l6fw::Model::L6 {
                return Err("L6 MIDI probe requires a fresh L6 guest".into());
            }
            let started = Instant::now();
            let wall_deadline = started
                .checked_add(Duration::from_secs(wall_timeout_s))
                .ok_or("L6 MIDI probe deadline overflow")?;
            let guest_start = guest_now(session);
            let main = selected_main_image(options)?;
            let (binding, direct_hook) = if options.sd_image.is_some() {
                let root = sd_fixture_root(options)?;
                // Preserve stock revision and staged module validation from the existing SD fixtures.
                sd_fixture_edits(options, &main, l6fw::Model::L6)?;
                let loader = fs::read(
                    root.join(crate::fixture_layout::LOADER_BUILD)
                        .join(crate::fixture_layout::PATCH_REPORT),
                )
                .map_err(|e| e.to_string())?;
                let site = l6patch::hook_report::dispatch_site(&loader, &main, "L6", midi_in)?;
                let output = root.join(crate::fixture_layout::SD_BUNDLE_BUILD);
                let report: Value = serde_json::from_slice(
                    &fs::read(output.join("patch.json")).map_err(|e| e.to_string())?,
                )
                .map_err(|e| e.to_string())?;
                let mut matches = Vec::new();
                for patch in report["patches"]
                    .as_array()
                    .ok_or("SD report has no modules")?
                {
                    let filename = patch["file"]
                        .as_str()
                        .and_then(|s| std::path::Path::new(s).file_name())
                        .ok_or("SD report has no module file")?;
                    let bytes = fs::read(output.join("PATCHES").join(filename))
                        .map_err(|e| e.to_string())?;
                    if let Ok(offset) = l6patch::hook_report::module_hook_offset(&bytes, midi_in) {
                        matches.push(offset);
                    }
                }
                if matches.len() != 1 {
                    return Err("expected exactly one SD MIDI module hook".into());
                }
                (
                    B::Sd {
                        dispatcher: site.target,
                        site_index: site.index,
                        entry_offset: matches[0],
                    },
                    None,
                )
            } else {
                let report =
                    fs::read(options.firmware_dir.join("patch.json")).map_err(|e| e.to_string())?;
                let hook = l6patch::hook_report::firmware_hook(
                    &report,
                    &main,
                    "L6",
                    "midi_in_hook",
                    midi_in,
                )?;
                (B::Firmware, Some(hook.target))
            };
            let debug = session
                .control()
                .debug_session()
                .map_err(|e| e.to_string())?;
            let mut scope = L6MidiProbe {
                debug,
                target: 0,
                binding,
                wall_deadline,
                guest_start,
                guest_timeout: guest_timeout_ms,
                event: vec![],
                expected: vec![],
                sender_return: None,
                phase: 0,
            };
            scope.command(session, C::AddBreakpoint { address: idle_loop })?;
            scope.advance(session, 60_000)?;
            match scope.command(session, C::Register { index: 15 })? {
                O::Register { value, .. } if value & !1 == idle_loop => (),
                _ => return Err("L6 did not stop at idle".into()),
            }
            scope.target = if let Some(hook) = direct_hook {
                hook
            } else {
                let B::Sd {
                    dispatcher,
                    site_index,
                    entry_offset,
                } = scope.binding
                else {
                    unreachable!()
                };
                let timeout_ms = scope.remaining_ms(session, 60_000)?;
                match scope.command(
                    session,
                    C::L6MidiResolveSd {
                        dispatcher,
                        site_index,
                        entry_offset,
                        timeout_ms,
                    },
                )? {
                    O::L6MidiResolved { hook } => hook,
                    _ => return Err("unexpected SD hook observation".into()),
                }
            };
            scope.command(session, C::AddBreakpoint { address: midi_send })?;
            scope.command(
                session,
                C::AddBreakpoint {
                    address: scope.target,
                },
            )?;
            scope.phase = 1;
            let target = scope.target;
            self.l6_midi_probe = Some(scope);
            return Ok(json!({"hook":target}));
        }
        let scope = self
            .l6_midi_probe
            .as_mut()
            .ok_or("L6 MIDI probe has not begun")?;
        match operation {
            Operation::L6MidiProbeInject if scope.phase == 1 => {
                let timeout_ms = scope.remaining_ms(session, 60_000)?;
                let result = scope.command(
                    session,
                    C::L6MidiInject {
                        expected_hook: scope.target,
                        binding: scope.binding.clone(),
                        timeout_ms,
                        leave_stopped: true,
                    },
                )?;
                let O::L6MidiInjected {
                    event,
                    config,
                    requires_restart: true,
                    ..
                } = result
                else {
                    return Err("unexpected L6 injection observation".into());
                };
                scope.expected = l6_midi_expected_ccs(&config.cc_map)?;
                scope.event = event;
                scope.phase = 2;
                Ok(json!({"channel":config.channel,"expected_ccs":scope.expected}))
            }
            Operation::L6MidiProbeExpectHook if scope.phase == 2 => {
                scope.advance(session, 10_000)?;
                let timeout_ms = scope.remaining_ms(session, 60_000)?;
                let O::L6MidiHook { pc, event } =
                    scope.command(session, C::L6MidiHook { timeout_ms })?
                else {
                    return Err("unexpected hook observation".into());
                };
                if pc != scope.target || event != scope.event[..7] {
                    return Err("L6 MIDI event changed before hook entry".into());
                }
                let timeout_ms = scope.remaining_ms(session, 60_000)?;
                scope.command(
                    session,
                    C::L6MidiBreakpoint {
                        address: scope.target,
                        enabled: false,
                        timeout_ms,
                    },
                )?;
                scope.phase = 3;
                Ok(json!({"hook":pc,"event":event}))
            }
            Operation::L6MidiProbeExpectReplies if scope.phase == 3 => {
                let mut index = 0;
                let mut replies = Vec::new();
                let total = scope.expected.len();
                scope
                    .advance(session, 10_000)
                    .map_err(|e| l6_reply_error(1, index, total, "advance to sender", &e))?;
                for attempt in 1..=total + 64 {
                    let timeout_ms = scope.remaining_ms(session, 60_000)?;
                    let O::L6MidiSender {
                        length,
                        route,
                        return_pc,
                        bytes: Some(bytes),
                        ..
                    } = scope
                        .command(session, C::L6MidiSender { timeout_ms })
                        .map_err(|e| l6_reply_error(attempt, index, total, "read sender", &e))?
                    else {
                        return Err("expected a three-byte L6 sender observation".into());
                    };
                    if length != 3
                        || route != 1
                        || bytes.len() != 3
                        || bytes[0] != scope.event[4]
                        || bytes[1] == 0xff
                        || bytes[2] > 127
                    {
                        return Err(format!("invalid L6 MIDI sender event: {bytes:?}"));
                    }
                    let complete = l6_accept_ordered_cc(&scope.expected, &mut index, bytes[1]);
                    replies.push(bytes);
                    if complete {
                        scope.sender_return = Some((return_pc, attempt));
                        scope.phase = 4;
                        return Ok(json!({"replies": replies}));
                    }
                    let timeout_ms = scope.remaining_ms(session, 60_000)?;
                    scope
                        .command(session, C::L6MidiStep { timeout_ms })
                        .map_err(|e| {
                            l6_reply_error(attempt, index, total, "step past sender", &e)
                        })?;
                    scope.advance(session, 10_000).map_err(|e| {
                        l6_reply_error(attempt, index, total, "advance to next sender", &e)
                    })?;
                }
                Err(format!(
                    "L6 MIDI sync matched {index}/{} expected CCs",
                    scope.expected.len()
                ))
            }
            Operation::L6MidiProbeFinish if scope.phase == 4 => {
                let (return_pc, attempt) =
                    scope.sender_return.ok_or("missing L6 MIDI sender return")?;
                let total = scope.expected.len();
                let index = total;
                let timeout_ms = scope.remaining_ms(session, 60_000)?;
                scope
                    .command(
                        session,
                        C::L6MidiBreakpoint {
                            address: midi_send,
                            enabled: false,
                            timeout_ms,
                        },
                    )
                    .map_err(|e| {
                        l6_reply_error(attempt, index, total, "remove sender breakpoint", &e)
                    })?;
                let timeout_ms = scope.remaining_ms(session, 60_000)?;
                scope
                    .command(
                        session,
                        C::L6MidiBreakpoint {
                            address: return_pc,
                            enabled: true,
                            timeout_ms,
                        },
                    )
                    .map_err(|e| {
                        l6_reply_error(attempt, index, total, "arm return breakpoint", &e)
                    })?;
                scope
                    .advance(session, 10_000)
                    .map_err(|e| l6_reply_error(attempt, index, total, "advance to return", &e))?;
                let timeout_ms = scope.remaining_ms(session, 60_000)?;
                let O::L6MidiFinished {
                    requires_restart: true,
                } = scope
                    .command(session, C::L6MidiFinish { timeout_ms })
                    .map_err(|e| l6_reply_error(attempt, index, total, "finish probe", &e))?
                else {
                    return Err("L6 MIDI probe did not finish stopped".into());
                };
                self.l6_midi_probe.take();
                Ok(json!({"requires_restart": true}))
            }
            _ => Err("L6 MIDI probe actions must execute in order".into()),
        }
    }
}

#[cfg(test)]
mod l6_hook_probe_tests {
    use super::*;
    #[test]
    fn reply_error_keeps_operation_progress_and_underlying_error() {
        assert_eq!(
            l6_reply_error(4, 2, 54, "step past sender", "GDB packet deadline exceeded"),
            "L6 MIDI reply attempt 4 (matched 2/54), step past sender: GDB packet deadline exceeded"
        );
    }
    #[test]
    fn map_is_channel_major_and_replies_are_ordered_with_duplicates() {
        let mut map = [0xff; 54];
        map[0] = 7;
        map[6] = 9;
        map[12] = 7;
        map[1] = 8;
        let expected = l6_midi_expected_ccs(&map).unwrap();
        assert_eq!(expected, [7, 9, 7, 8]);
        let mut index = 0;
        for cc in [9, 7, 8, 9, 7] {
            assert!(!l6_accept_ordered_cc(&expected, &mut index, cc));
        }
        assert!(l6_accept_ordered_cc(&expected, &mut index, 8));
        assert!(l6_midi_expected_ccs(&[0xff; 54]).is_err());
        assert!(l6_midi_expected_ccs(&[0; 53]).is_err());
    }
}

fn match_usb_forward(
    expected: &[u8],
    cable: u8,
    accumulated: &mut Vec<u8>,
    packets: Vec<(u8, Vec<u8>)>,
) -> bool {
    for (received_cable, data) in packets {
        if received_cable != cable || (accumulated.is_empty() && data.first() != expected.first()) {
            continue;
        }
        accumulated.extend(data);
        if accumulated == expected {
            return true;
        }
        if !expected.starts_with(accumulated) {
            accumulated.clear();
        }
    }
    false
}

/// DIN and direct USB are held together for the entire forwarding transaction.
pub(super) struct DinUsbScope {
    din: l6ctl::midi::MidiSession,
    usb: Option<l6ctl::midi::RawUsbReceiver>,
    pending: Option<(Vec<u8>, Instant)>,
}
impl Drop for DinUsbScope {
    fn drop(&mut self) {
        self.din.release();
    }
}

impl State {
    pub(super) fn run_din_usb(
        &mut self,
        session: &Session,
        operation: Operation,
    ) -> Result<Value, Box<dyn std::error::Error>> {
        if matches!(operation, Operation::DinUsbBegin) {
            if self.din_usb.is_some() || session.model() != l6fw::Model::L6 {
                return Err("DIN USB forwarding requires a fresh L6 scope".into());
            }
            session.control().connect_din()?;
            let endpoint = l6max_host::host_midi::endpoint_name(
                &l6max_host::host_midi::din_port_name_for_model_with_namespace(
                    l6fw::Model::L6,
                    midi_namespace().as_deref(),
                ),
            );
            let deadline = Instant::now() + Duration::from_secs(10);
            let din = loop {
                match l6ctl::midi::MidiSession::connect_native(&endpoint) {
                    Ok(value) => break value,
                    Err(error)
                        if error.contains("missing native MIDI") && Instant::now() < deadline =>
                    {
                        session.poll()?;
                        thread::sleep(Duration::from_millis(25));
                    }
                    Err(error) => return Err(error.into()),
                }
            };
            self.din_usb = Some(DinUsbScope {
                din,
                usb: None,
                pending: None,
            });
            return Ok(json!({"endpoint":endpoint}));
        }
        let scope = self.din_usb.as_mut().ok_or("missing DIN USB scope")?;
        match operation {
            Operation::DinUsbAttach if scope.usb.is_none() => {
                let mut usb = session.control().raw_usb_midi()?;
                let deadline = Instant::now() + Duration::from_secs(15);
                loop {
                    match usb.enumerate() {
                        Ok(()) => break,
                        Err(error)
                            if (error.raw_os_error() == Some(libc::ENODEV)
                                || error.raw_os_error() == Some(libc::EAGAIN)
                                || error.kind() == std::io::ErrorKind::InvalidData)
                                && Instant::now() < deadline =>
                        {
                            session.poll()?;
                            thread::sleep(Duration::from_millis(250));
                        }
                        Err(error) => {
                            return Err(format!("cannot enumerate L6 USB MIDI: {error}").into());
                        }
                    }
                }
                let _ = usb.poll();
                scope.usb = Some(usb);
                Ok(json!({"enumerated":true}))
            }
            Operation::DinUsbSend { bytes } if scope.usb.is_some() && scope.pending.is_none() => {
                if bytes.is_empty() || bytes.len() > 256 {
                    return Err("MIDI bytes must have length 1..256".into());
                }
                scope
                    .din
                    .send_bytes(&bytes)
                    .map_err(std::io::Error::other)?;
                scope.pending = Some((bytes.clone(), Instant::now() + Duration::from_secs(3)));
                Ok(json!({"sent":bytes}))
            }
            Operation::DinUsbExpect { bytes, cable } if scope.usb.is_some() => {
                let (expected, deadline) = scope
                    .pending
                    .as_ref()
                    .ok_or("missing pending MIDI message")?;
                if *expected != bytes || cable > 15 {
                    return Err("MIDI expectation does not match pending message".into());
                }
                let mut forwarded = Vec::new();
                while Instant::now() < *deadline {
                    session.poll()?;
                    if let Some(packets) = scope.usb.as_mut().unwrap().poll()? {
                        if match_usb_forward(&bytes, cable, &mut forwarded, packets) {
                            scope.pending = None;
                            return Ok(json!({"cable":cable,"bytes":bytes}));
                        }
                    }
                    thread::sleep(Duration::from_millis(5));
                }
                Err(format!(
                    "DIN to USB forwarding mismatch for {bytes:02x?}: received {forwarded:02x?}"
                )
                .into())
            }
            Operation::DinUsbEnd if scope.usb.is_some() && scope.pending.is_none() => {
                self.din_usb.take();
                Ok(json!({"released":true}))
            }
            _ => Err("DIN USB forwarding actions are out of order".into()),
        }
    }
}

#[cfg(test)]
mod din_usb_forward_tests {
    use super::match_usb_forward;
    #[test]
    fn preserves_cable_and_fragment_matching() {
        let mut seen = Vec::new();
        let expected = [0xf0, 0x7d, 1, 0xf7];
        assert!(!match_usb_forward(
            &expected,
            0,
            &mut seen,
            vec![
                (1, expected.to_vec()),
                (0, vec![0x90, 1, 2]),
                (0, vec![0xf0, 0x7d])
            ]
        ));
        assert_eq!(seen, [0xf0, 0x7d]);
        assert!(match_usb_forward(
            &expected,
            0,
            &mut seen,
            vec![(0, vec![1, 0xf7])]
        ));
        seen.clear();
        assert!(!match_usb_forward(
            &expected,
            0,
            &mut seen,
            vec![(0, vec![0xf0, 0x7d]), (0, vec![9])]
        ));
        assert!(seen.is_empty());
        assert!(match_usb_forward(
            &[0xf8],
            0,
            &mut seen,
            vec![(0, vec![0xf8])]
        ));
    }
}

/// One exclusive USB MIDI session; the budget begins only after the encoder
/// action, matching the original receive-and-send operation.
pub(super) struct UsbMidiScope {
    receiver: l6ctl::midi::RawUsbReceiver,
    started: Option<(Instant, u32, u32, u64)>,
    phase: u8,
    target: Option<u8>,
    alternate: Option<u8>,
}

fn usb_encoder_cc(messages: &[(u8, Vec<u8>)], controller: u8) -> bool {
    messages
        .iter()
        .any(|(_, message)| message.starts_with(&[0xb0, controller]))
}

fn usb_alternate_level(current: u8, target: u8) -> Option<u8> {
    (current == target).then_some(if target == 0 { 127 } else { 0 })
}

fn usb_receive_after_poll<T>(
    poll: impl FnOnce() -> Result<(), String>,
    check: impl FnOnce() -> Result<(), String>,
    receive: impl FnOnce() -> Result<T, String>,
) -> Result<T, String> {
    poll()?;
    check()?;
    receive()
}

impl UsbMidiScope {
    fn check(&self, session: &Session) -> Result<(), String> {
        let (started, guest_start, guest_budget, wall_budget) =
            self.started.ok_or("USB MIDI transaction not started")?;
        if deadline_exceeded(
            started.elapsed(),
            guest_start,
            guest_now(session),
            guest_budget,
            wall_budget,
        ) {
            return Err("USB MIDI transaction exceeded its shared deadline".into());
        }
        Ok(())
    }
    fn wall_deadline(&self) -> Result<Instant, String> {
        let (started, _, _, wall) = self.started.ok_or("USB MIDI transaction not started")?;
        started
            .checked_add(Duration::from_secs(wall))
            .ok_or_else(|| "USB MIDI deadline overflow".into())
    }
}

impl State {
    pub(super) fn run_usb_midi_steps(
        &mut self,
        session: &Session,
        options: &Options,
        operation: Operation,
        guest_timeout_ms: u32,
        wall_timeout_s: u64,
        operation_start: (Instant, u32),
    ) -> Result<Value, String> {
        use Operation::*;
        if matches!(operation, UsbMidiOpen) {
            if self.usb_midi.is_some() {
                return Err("USB MIDI session already open".into());
            }
            let endpoints = self
                .midi
                .ok_or("USB MIDI needs prior descriptor enumeration")?;
            let receiver = session
                .control()
                .raw_usb_midi_enumerated(endpoints)
                .map_err(|e| e.to_string())?;
            self.usb_midi = Some(UsbMidiScope {
                receiver,
                started: None,
                phase: 0,
                target: None,
                alternate: None,
            });
            return Ok(json!({"connected":true}));
        }
        let scope = self.usb_midi.as_mut().ok_or("USB MIDI session not open")?;
        if matches!(operation, UsbMidiBegin) && scope.phase == 0 {
            scope.started = Some((
                operation_start.0,
                operation_start.1,
                guest_timeout_ms,
                wall_timeout_s,
            ));
            scope.phase = 1;
            return Ok(json!({"started":true}));
        }
        scope.check(session)?;
        let cc = if session.model() == l6fw::Model::L6 {
            0x53
        } else {
            0x51
        };
        match operation {
            UsbMidiExpectEncoder if scope.phase == 1 => {
                let mut captured = Vec::new();
                loop {
                    scope.check(session)?;
                    let (started, guest_start, guest_budget, wall_budget) = scope.started.unwrap();
                    let transfer = usb_receive_after_poll(
                        || session.poll().map(|_| ()).map_err(|e| e.to_string()),
                        || {
                            if deadline_exceeded(
                                started.elapsed(),
                                guest_start,
                                guest_now(session),
                                guest_budget,
                                wall_budget,
                            ) {
                                Err("USB MIDI transaction exceeded its shared deadline".into())
                            } else {
                                Ok(())
                            }
                        },
                        || scope.receiver.poll_transfer().map_err(|e| e.to_string()),
                    )?;
                    if let Some((raw, messages)) = transfer {
                        scope.check(session)?;
                        captured.extend_from_slice(&raw);
                        fs::write(options.logs.join("usb-midi.bin"), &captured)
                            .map_err(|e| e.to_string())?;
                        scope.check(session)?;
                        if usb_encoder_cc(&messages, cc) {
                            break;
                        }
                    }
                    scope.check(session)?;
                    thread::sleep(Duration::from_millis(5));
                }
                scope.phase = 2;
                Ok(json!({"received":captured, "controller":cc, "cable":"any"}))
            }
            UsbMidiPrepareLevel { value } if scope.phase == 2 && value <= 127 => {
                let sample = session
                    .control()
                    .mixer_level_cancellable(scope.wall_deadline()?, || {
                        scope.check(session).is_err()
                    })
                    .map_err(|e| e.to_string())?;
                scope.check(session)?;
                scope.target = Some(value);
                scope.alternate = usb_alternate_level(sample.value, value);
                scope.phase = 3;
                Ok(json!({"baseline":sample, "alternate":scope.alternate, "target":value}))
            }
            UsbMidiSetAlternate | UsbMidiSetTarget
                if (matches!(operation, UsbMidiSetAlternate) && scope.phase == 3)
                    || (matches!(operation, UsbMidiSetTarget) && scope.phase == 4) =>
            {
                let alternate = matches!(operation, UsbMidiSetAlternate);
                let level = if alternate {
                    scope.alternate
                } else {
                    scope.target
                };
                if let Some(level) = level {
                    let control = session.control();
                    let local_start = guest_now(session);
                    let (_, _, outer_guest, outer_wall) = scope.started.unwrap();
                    let local_guest = outer_guest.min(10_000);
                    let local_deadline = Instant::now()
                        .checked_add(Duration::from_secs(outer_wall.min(180)))
                        .ok_or("USB MIDI deadline overflow")?
                        .min(scope.wall_deadline()?);
                    scope.check(session)?;
                    scope
                        .receiver
                        .send_control_change(1, 0, cc, level)
                        .map_err(|e| e.to_string())?;
                    loop {
                        scope.check(session)?;
                        if Instant::now() >= local_deadline
                            || guest_now(session).wrapping_sub(local_start) >= local_guest
                        {
                            return Err(format!("USB MIDI channel level did not become {level}"));
                        }
                        session.poll().map_err(|e| e.to_string())?;
                        let sample = control
                            .mixer_level_cancellable(local_deadline, || {
                                scope.check(session).is_err()
                                    || guest_now(session).wrapping_sub(local_start) >= local_guest
                            })
                            .map_err(|e| e.to_string())?;
                        scope.check(session)?;
                        if Instant::now() >= local_deadline
                            || guest_now(session).wrapping_sub(local_start) >= local_guest
                        {
                            return Err(format!("USB MIDI channel level did not become {level}"));
                        }
                        if sample.value == level {
                            break;
                        }
                        thread::sleep(Duration::from_millis(10));
                    }
                }
                scope.phase = if alternate { 4 } else { 5 };
                Ok(json!({"level":level,"skipped":level.is_none(),"cable":1}))
            }
            UsbMidiEnd if scope.phase == 5 => {
                self.usb_midi.take();
                Ok(json!({"released":true}))
            }
            _ => Err("USB MIDI actions are out of order".into()),
        }
    }
}

#[cfg(test)]
mod usb_midi_transaction_tests {
    use super::*;
    #[test]
    fn delayed_guest_poll_cannot_consume_or_write_after_expiry() {
        use std::cell::Cell;
        let expired = Cell::new(false);
        let received = Cell::new(false);
        let result = usb_receive_after_poll(
            || {
                expired.set(true);
                Ok(())
            },
            || {
                if expired.get() {
                    Err("expired".into())
                } else {
                    Ok(())
                }
            },
            || {
                received.set(true);
                Ok(())
            },
        );
        assert!(result.is_err());
        assert!(!received.get());
    }
    #[test]
    fn receives_encoder_cc_on_any_cable_but_not_wrong_message() {
        assert!(usb_encoder_cc(&[(0, vec![0xb0, 0x53, 42])], 0x53));
        assert!(usb_encoder_cc(&[(7, vec![0xb0, 0x51, 42])], 0x51));
        assert!(!usb_encoder_cc(
            &[(1, vec![0xb1, 0x51, 42]), (0, vec![0xb0, 0x53, 42])],
            0x51
        ));
    }
    #[test]
    fn alternate_forces_a_fresh_target_transition() {
        assert_eq!(usb_alternate_level(32, 32), Some(0));
        assert_eq!(usb_alternate_level(0, 0), Some(127));
        assert_eq!(usb_alternate_level(31, 32), None);
    }
}
