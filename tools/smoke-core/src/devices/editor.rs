//! Editor pad assignment and playback over USB SysEx.
use super::*;

#[allow(clippy::too_many_arguments)]
pub(super) fn editor_exchange(
    session: &Session,
    usb: &l6max_host::usb::UsbHost,
    endpoints: l6max_host::usb::Endpoints,
    encoder: &mut l6max_host::usb_midi::Encoder,
    message: &[u8],
    timeout_ms: u32,
    start_guest: u32,
    guest_timeout_ms: u32,
    wall_deadline: Instant,
) -> Result<Vec<Vec<u8>>, Box<dyn std::error::Error>> {
    let wire = encoder.push(2, message);
    usb.send(endpoints.output, &wire)?;
    collect_editor_sysex(
        session,
        usb,
        endpoints.input,
        timeout_ms,
        start_guest,
        guest_timeout_ms,
        wall_deadline,
    )
}

/// An exchange whose caller expects one specific complete response. Unlike
/// dialog and multi-message exchanges, it does not need a collection window.
#[allow(clippy::too_many_arguments)]
fn editor_exchange_expected(
    session: &Session,
    usb: &l6max_host::usb::UsbHost,
    endpoints: l6max_host::usb::Endpoints,
    encoder: &mut l6max_host::usb_midi::Encoder,
    message: &[u8],
    expected: &[u8],
    start_guest: u32,
    guest_timeout_ms: u32,
    wall_deadline: Instant,
) -> Result<Vec<Vec<u8>>, Box<dyn std::error::Error>> {
    const RESPONSE_GUEST_TIMEOUT_MS: u32 = 2_000;
    const RESPONSE_WALL_TIMEOUT_S: u64 = 30;
    let wire = encoder.push(2, message);
    usb.send(endpoints.output, &wire)?;
    let matches = |reply: &[u8]| reply.starts_with(expected);
    collect_editor_sysex_inner(
        session,
        usb,
        endpoints.input,
        RESPONSE_GUEST_TIMEOUT_MS,
        start_guest,
        guest_timeout_ms,
        wall_deadline.min(Instant::now() + Duration::from_secs(RESPONSE_WALL_TIMEOUT_S)),
        Some(&matches),
    )
}

pub(super) fn refresh_editor_session(
    session: &Session,
    usb: &l6max_host::usb::UsbHost,
    endpoints: l6max_host::usb::Endpoints,
    encoder: &mut l6max_host::usb_midi::Encoder,
    start_guest: u32,
    guest_timeout_ms: u32,
    wall_deadline: Instant,
) -> Result<(), Box<dyn std::error::Error>> {
    if session.engine.model() == l6fw::Model::L6 {
        return Ok(());
    }
    let replies = editor_exchange_expected(
        session,
        usb,
        endpoints,
        encoder,
        &[0xf0, 0x52, 0, 0, 0x31, 0x0b, 0xf7],
        &[0xf0, 0x52, 0, 0, 0, 0x0b],
        start_guest,
        guest_timeout_ms,
        wall_deadline,
    )?;
    require_editor_reply(&replies, &[0xf0, 0x52, 0, 0, 0, 0x0b], "keep-alive")?;
    Ok(())
}

pub(super) fn require_editor_reply(
    replies: &[Vec<u8>],
    prefix: &[u8],
    operation: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    if replies.iter().any(|reply| reply.starts_with(prefix)) {
        Ok(())
    } else {
        Err(format!("Editor {operation} reply missing: {replies:02x?}").into())
    }
}

pub(super) fn collect_editor_sysex(
    session: &Session,
    usb: &l6max_host::usb::UsbHost,
    input: u8,
    timeout_ms: u32,
    start_guest: u32,
    guest_timeout_ms: u32,
    wall_deadline: Instant,
) -> Result<Vec<Vec<u8>>, Box<dyn std::error::Error>> {
    collect_editor_sysex_inner(
        session,
        usb,
        input,
        timeout_ms,
        start_guest,
        guest_timeout_ms,
        wall_deadline,
        None,
    )
}

#[allow(clippy::too_many_arguments)]
fn collect_editor_sysex_inner(
    session: &Session,
    usb: &l6max_host::usb::UsbHost,
    input: u8,
    timeout_ms: u32,
    start_guest: u32,
    guest_timeout_ms: u32,
    wall_deadline: Instant,
    expected: Option<&dyn Fn(&[u8]) -> bool>,
) -> Result<Vec<Vec<u8>>, Box<dyn std::error::Error>> {
    let started = session
        .engine
        .inputs
        .as_ref()
        .unwrap()
        .panel
        .snapshot()
        .guest_ms;
    let mut pending = Vec::new();
    let mut replies = Vec::new();
    loop {
        session.poll().map_err(std::io::Error::other)?;
        match usb.try_receive(input, 16_384) {
            Ok(data) => {
                for (cable, fragment) in l6max_host::usb_midi::decode(&data)? {
                    if cable != 2 {
                        continue;
                    }
                    for byte in fragment {
                        if byte == 0xf0 {
                            pending.clear();
                        }
                        if byte == 0xf0 || !pending.is_empty() {
                            pending.push(byte);
                            if byte == 0xf7 {
                                replies.push(std::mem::take(&mut pending));
                            }
                        }
                    }
                }
            }
            Err(error) if error.raw_os_error() == Some(libc::EAGAIN) => {}
            Err(error) => return Err(error.into()),
        }
        if pending.is_empty()
            && expected.is_some_and(|matches| replies.iter().any(|reply| matches(reply)))
        {
            return Ok(replies);
        }
        let now = session
            .engine
            .inputs
            .as_ref()
            .unwrap()
            .panel
            .snapshot()
            .guest_ms;
        if now.wrapping_sub(started) >= timeout_ms {
            if !pending.is_empty() {
                return Err(format!("truncated Editor SysEx reply: {pending:02x?}").into());
            }
            return Ok(replies);
        }
        if Instant::now() >= wall_deadline || now.wrapping_sub(start_guest) > guest_timeout_ms {
            return Err("Editor pad assignment exceeded its guest or wall deadline".into());
        }
        thread::sleep(Duration::from_millis(2));
    }
}

pub(super) fn answer_editor_pad_dialog(
    usb: &l6max_host::usb::UsbHost,
    output: u8,
    encoder: &mut l6max_host::usb_midi::Encoder,
    replies: &[Vec<u8>],
) -> Result<bool, Box<dyn std::error::Error>> {
    let pending = replies.iter().any(|reply| {
        reply.get(4..6) == Some(&[0x45, 0x06]) || reply.get(4..7) == Some(&[0x31, 0x10, 0x06])
    });
    if !pending {
        return Ok(false);
    }
    let answer = encoder.push(2, &[0xf0, 0x52, 0, 0, 0x31, 0x12, 0x06, 0x01, 0xf7]);
    usb.send(output, &answer)?;
    Ok(true)
}

/// One Editor verification retains the original endpoint, encoder and clock
/// across separately reported scenario steps.
pub(super) struct UiPadScope {
    pad: u8,
    endpoints: l6max_host::usb::Endpoints,
    encoder: l6max_host::usb_midi::Encoder,
    start_guest: u32,
    deadline: Instant,
    guest_budget: u32,
    wall_budget: u64,
    pub phase: u8,
    identity: Vec<Vec<u8>>,
    assignments: Vec<Vec<u8>>,
}

impl UiPadScope {
    pub(super) fn begin(
        session: &Session,
        endpoints: l6max_host::usb::Endpoints,
        pad: u8,
        guest_budget: u32,
        wall_budget: u64,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        if session.engine.model() != l6fw::Model::L6max || pad > 3 {
            return Err("UI pad verification requires L6max and pad 0..3".into());
        }
        let start_guest = session
            .engine
            .inputs
            .as_ref()
            .ok_or("panel input unavailable")?
            .panel
            .snapshot()
            .guest_ms;
        Ok(Self {
            pad,
            endpoints,
            encoder: Default::default(),
            start_guest,
            deadline: Instant::now() + Duration::from_secs(wall_budget),
            guest_budget,
            wall_budget,
            phase: 0,
            identity: vec![],
            assignments: vec![],
        })
    }

    pub(super) fn check(
        &self,
        session: &Session,
        guest_budget: u32,
        wall_budget: u64,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let guest = session
            .engine
            .inputs
            .as_ref()
            .ok_or("panel input unavailable")?
            .panel
            .snapshot()
            .guest_ms;
        if (guest_budget, wall_budget) != (self.guest_budget, self.wall_budget)
            || Instant::now() >= self.deadline
            || guest.wrapping_sub(self.start_guest) > self.guest_budget
        {
            return Err(
                "UI pad verification exceeded its shared deadline or changed budget".into(),
            );
        }
        Ok(())
    }

    pub(super) fn identity(
        &mut self,
        session: &Session,
    ) -> Result<Value, Box<dyn std::error::Error>> {
        let usb = session
            .engine
            .usb
            .as_ref()
            .ok_or("USB backend unavailable")?;
        let wire = self.encoder.push(2, &[0xf0, 0x7e, 0, 0x06, 0x01, 0xf7]);
        usb.send(self.endpoints.output, &wire)?;
        self.identity = collect_editor_sysex(
            session,
            usb,
            self.endpoints.input,
            500,
            self.start_guest,
            self.guest_budget,
            self.deadline,
        )?;
        if !self
            .identity
            .iter()
            .any(|r| r.starts_with(&[0xf0, 0x7e, 0, 0x06, 0x02, 0x52, 0x72, 0]))
        {
            return Err(format!(
                "L6max Editor identity reply missing after UI assignment: {:02x?}",
                self.identity
            )
            .into());
        }
        Ok(json!({"identity_replies": self.identity}))
    }

    pub(super) fn expected(
        &mut self,
        session: &Session,
        request: &[u8],
        expected: &[u8],
        label: &str,
    ) -> Result<Value, Box<dyn std::error::Error>> {
        let usb = session
            .engine
            .usb
            .as_ref()
            .ok_or("USB backend unavailable")?;
        let replies = editor_exchange_expected(
            session,
            usb,
            self.endpoints,
            &mut self.encoder,
            request,
            expected,
            self.start_guest,
            self.guest_budget,
            self.deadline,
        )?;
        require_editor_reply(&replies, expected, label)?;
        Ok(json!({"replies": replies}))
    }

    pub(super) fn query(&mut self, session: &Session) -> Result<Value, Box<dyn std::error::Error>> {
        let usb = session
            .engine
            .usb
            .as_ref()
            .ok_or("USB backend unavailable")?;
        for _ in 0..8 {
            self.check(session, self.guest_budget, self.wall_budget)?;
            self.assignments = editor_exchange(
                session,
                usb,
                self.endpoints,
                &mut self.encoder,
                &[0xf0, 0x52, 0, 0, 0x46, 0x02, self.pad, 0xf7],
                300,
                self.start_guest,
                self.guest_budget,
                self.deadline,
            )?;
            if self
                .assignments
                .iter()
                .any(|reply| l6ctl::editor_pad_assignment_present(reply, self.pad))
            {
                break;
            }
        }
        Ok(json!({"assignment_replies": self.assignments}))
    }

    pub(super) fn assert_assignment(
        &self,
        session: &Session,
        options: &Options,
    ) -> Result<Value, Box<dyn std::error::Error>> {
        let assigned = self
            .assignments
            .iter()
            .any(|reply| l6ctl::editor_pad_assignment_present(reply, self.pad));
        let guest = session
            .engine
            .inputs
            .as_ref()
            .ok_or("panel input unavailable")?
            .panel
            .snapshot()
            .guest_ms;
        let evidence = json!({"pad": self.pad + 1, "identity_replies": self.identity, "assignment_replies": self.assignments, "assigned": assigned, "guest_elapsed_ms": guest.wrapping_sub(self.start_guest)});
        fs::write(
            options
                .logs
                .join(format!("l6max-ui-pad-verify-{}.json", self.pad + 1)),
            serde_json::to_vec_pretty(&evidence)?,
        )?;
        if !assigned {
            return Err(format!(
                "L6max UI did not assign a WAV to Sound Pad {}: {evidence}",
                self.pad + 1
            )
            .into());
        }
        Ok(evidence)
    }
}

/// Editor-controlled playback retains one USB encoder, endpoint and deadline.
pub(super) struct EditorPlaybackScope {
    pad: u8,
    endpoints: l6max_host::usb::Endpoints,
    encoder: l6max_host::usb_midi::Encoder,
    start_guest: u32,
    deadline: Instant,
    guest_budget: u32,
    wall_budget: u64,
    pub phase: u8,
    trace_baseline: usize,
    play: Vec<Vec<u8>>,
    state: Vec<Vec<u8>>,
    dialog_answered: bool,
    samples: Vec<u64>,
    trace_snapshot: Option<String>,
    evidence: Option<Value>,
}
impl EditorPlaybackScope {
    pub(super) fn begin(
        session: &Session,
        endpoints: l6max_host::usb::Endpoints,
        pad: u8,
        guest_budget: u32,
        wall_budget: u64,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        if session.engine.model() != l6fw::Model::L6 || pad > 3 {
            return Err("Editor playback requires L6 and pad 0..3".into());
        }
        if std::env::var_os("L6_AUDIO_SAMPLE_TRACE").is_none() {
            return Err("Editor playback requires L6_AUDIO_SAMPLE_TRACE=1".into());
        }
        let start_guest = session
            .engine
            .inputs
            .as_ref()
            .ok_or("panel input unavailable")?
            .panel
            .snapshot()
            .guest_ms;
        Ok(Self {
            pad,
            endpoints,
            encoder: Default::default(),
            start_guest,
            deadline: Instant::now() + Duration::from_secs(wall_budget),
            guest_budget,
            wall_budget,
            phase: 0,
            trace_baseline: 0,
            play: vec![],
            state: vec![],
            dialog_answered: false,
            samples: vec![],
            trace_snapshot: None,
            evidence: None,
        })
    }
    pub(super) fn check(
        &self,
        session: &Session,
        guest_budget: u32,
        wall_budget: u64,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let now = session
            .engine
            .inputs
            .as_ref()
            .ok_or("panel input unavailable")?
            .panel
            .snapshot()
            .guest_ms;
        if (guest_budget, wall_budget) != (self.guest_budget, self.wall_budget)
            || Instant::now() >= self.deadline
            || now.wrapping_sub(self.start_guest) > self.guest_budget
        {
            return Err("Editor playback exceeded its shared deadline or changed budget".into());
        }
        Ok(())
    }
    pub(super) fn play(
        &mut self,
        session: &Session,
        options: &Options,
    ) -> Result<Value, Box<dyn std::error::Error>> {
        let trace = fs::read_to_string(options.logs.join("engine.trace"))?;
        self.trace_baseline = trace
            .lines()
            .filter(|line| line.starts_with(l6ctl::SAI1_TX_PREFIX))
            .count();
        self.check(session, self.guest_budget, self.wall_budget)?;
        let usb = session
            .engine
            .usb
            .as_ref()
            .ok_or("USB backend unavailable")?;
        self.play = editor_exchange(
            session,
            usb,
            self.endpoints,
            &mut self.encoder,
            &[0xf0, 0x52, 0, 0, 0x31, 0x08, self.pad, 1, 0xf7],
            100,
            self.start_guest,
            self.guest_budget,
            self.deadline,
        )?;
        self.check(session, self.guest_budget, self.wall_budget)?;
        self.dialog_answered =
            answer_editor_pad_dialog(usb, self.endpoints.output, &mut self.encoder, &self.play)?;
        Ok(json!({"play_replies":self.play,"assignment_dialog_answered":self.dialog_answered}))
    }
    pub(super) fn wait_playing(
        &mut self,
        session: &Session,
    ) -> Result<Value, Box<dyn std::error::Error>> {
        let usb = session
            .engine
            .usb
            .as_ref()
            .ok_or("USB backend unavailable")?;
        for _ in 0..12 {
            self.check(session, self.guest_budget, self.wall_budget)?;
            self.state = editor_exchange(
                session,
                usb,
                self.endpoints,
                &mut self.encoder,
                &[0xf0, 0x52, 0, 0, 0x46, 0x03, self.pad, 0xf7],
                250,
                self.start_guest,
                self.guest_budget,
                self.deadline,
            )?;
            self.check(session, self.guest_budget, self.wall_budget)?;
            if self
                .state
                .iter()
                .any(|reply| reply.get(4..8) == Some(&[0x45, 0x03, self.pad, 0x01]))
            {
                return Ok(json!({"state_replies":self.state,"playing_state":true}));
            }
            if answer_editor_pad_dialog(usb, self.endpoints.output, &mut self.encoder, &self.state)?
            {
                let _ = collect_editor_sysex(
                    session,
                    usb,
                    self.endpoints.input,
                    300,
                    self.start_guest,
                    self.guest_budget,
                    self.deadline,
                )?;
            }
        }
        Err(format!(
            "Editor Sound Pad {} did not enter playing state: {:02x?}",
            self.pad + 1,
            self.state
        )
        .into())
    }
    pub(super) fn observe(
        &mut self,
        session: &Session,
        options: &Options,
    ) -> Result<Value, Box<dyn std::error::Error>> {
        let inputs = session
            .engine
            .inputs
            .as_ref()
            .ok_or("panel input unavailable")?;
        let start = inputs.panel.snapshot().guest_ms;
        loop {
            self.check(session, self.guest_budget, self.wall_budget)?;
            let elapsed = inputs.panel.snapshot().guest_ms.wrapping_sub(start);
            if elapsed >= 1200 {
                self.trace_snapshot = Some(fs::read_to_string(options.logs.join("engine.trace"))?);
                return Ok(json!({"observed_guest_ms": elapsed}));
            }
            session.poll()?;
            thread::sleep(Duration::from_millis(5));
        }
    }
    pub(super) fn sample(
        &mut self,
        session: &Session,
        options: &Options,
    ) -> Result<Value, Box<dyn std::error::Error>> {
        let trace = self
            .trace_snapshot
            .as_deref()
            .ok_or("Editor playback trace is not captured")?;
        self.samples = l6ctl::sai1_tx_nonzero_counts(trace, self.trace_baseline);
        let guest = session
            .engine
            .inputs
            .as_ref()
            .ok_or("panel input unavailable")?
            .panel
            .snapshot()
            .guest_ms;
        let evidence = json!({"pad":self.pad+1,"play_replies":self.play,"state_replies":self.state,"assignment_dialog_answered":self.dialog_answered,"playing_state":true,"nonzero_audio_samples":self.samples,"guest_elapsed_ms":guest.wrapping_sub(self.start_guest)});
        fs::write(
            options
                .logs
                .join(format!("editor-pad-playback-{}.json", self.pad + 1)),
            serde_json::to_vec_pretty(&evidence)?,
        )?;
        self.evidence = Some(evidence.clone());
        Ok(evidence)
    }
    pub(super) fn assert_audio(&self) -> Result<Value, Box<dyn std::error::Error>> {
        if !self.samples.iter().any(|count| *count > 0) {
            return Err(format!("Editor Sound Pad {} entered playing state but produced no nonzero SAI1 TX samples: {}", self.pad + 1, self.evidence.as_ref().ok_or("Editor playback evidence unavailable")?).into());
        }
        Ok(json!({"nonzero_audio_samples":self.samples}))
    }
}

/// Editor assignment transaction. The scenario chooses each pad and phase;
/// this scope owns the USB encoder, exact reply data and shared deadline.
pub(super) struct EditorAssignmentScope {
    endpoints: l6max_host::usb::Endpoints,
    encoder: l6max_host::usb_midi::Encoder,
    start_guest: u32,
    deadline: Instant,
    guest_budget: u32,
    wall_budget: u64,
    pub phase: u8,
    pub pad: u8,
    file_count: Option<u16>,
    filename: Option<Vec<u8>>,
    assignment_args: Option<Vec<u8>>,
    assignment_replies: Vec<Vec<u8>>,
    assignments: Vec<Value>,
}
impl EditorAssignmentScope {
    pub(super) fn begin(
        session: &Session,
        endpoints: l6max_host::usb::Endpoints,
        guest_budget: u32,
        wall_budget: u64,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let start_guest = session
            .engine
            .inputs
            .as_ref()
            .ok_or("editor assignments need panel inputs")?
            .panel
            .snapshot()
            .guest_ms;
        Ok(Self {
            endpoints,
            encoder: Default::default(),
            start_guest,
            deadline: Instant::now() + Duration::from_secs(wall_budget),
            guest_budget,
            wall_budget,
            phase: 0,
            pad: 0,
            file_count: None,
            filename: None,
            assignment_args: None,
            assignment_replies: vec![],
            assignments: vec![],
        })
    }
    pub(super) fn check(
        &self,
        session: &Session,
        guest: u32,
        wall: u64,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let now = session
            .engine
            .inputs
            .as_ref()
            .ok_or("panel input unavailable")?
            .panel
            .snapshot()
            .guest_ms;
        if (guest, wall) != (self.guest_budget, self.wall_budget)
            || Instant::now() >= self.deadline
            || now.wrapping_sub(self.start_guest) > self.guest_budget
        {
            return Err("Editor assignment exceeded its shared deadline or changed budget".into());
        }
        Ok(())
    }
    pub(super) fn identity(
        &mut self,
        session: &Session,
    ) -> Result<Value, Box<dyn std::error::Error>> {
        let usb = session
            .engine
            .usb
            .as_ref()
            .ok_or("USB backend unavailable")?;
        let wire = self.encoder.push(2, &[0xf0, 0x7e, 0, 0x06, 0x01, 0xf7]);
        usb.send(self.endpoints.output, &wire)?;
        let replies = collect_editor_sysex(
            session,
            usb,
            self.endpoints.input,
            500,
            self.start_guest,
            self.guest_budget,
            self.deadline,
        )?;
        if !replies
            .iter()
            .any(|reply| reply.starts_with(&[0xf0, 0x7e, 0, 0x06, 0x02, 0x52, 0x72, 0]))
        {
            return Err(format!("Editor identity reply missing: {replies:02x?}").into());
        }
        Ok(json!({"identity_replies":replies}))
    }
    pub(super) fn expected(
        &mut self,
        session: &Session,
        request: &[u8],
        expected: &[u8],
        label: &str,
    ) -> Result<Value, Box<dyn std::error::Error>> {
        let usb = session
            .engine
            .usb
            .as_ref()
            .ok_or("USB backend unavailable")?;
        let replies = editor_exchange_expected(
            session,
            usb,
            self.endpoints,
            &mut self.encoder,
            request,
            expected,
            self.start_guest,
            self.guest_budget,
            self.deadline,
        )?;
        require_editor_reply(&replies, expected, label)?;
        Ok(json!({"replies":replies}))
    }
    fn refresh(&mut self, session: &Session) -> Result<(), Box<dyn std::error::Error>> {
        self.check(session, self.guest_budget, self.wall_budget)?;
        let usb = session
            .engine
            .usb
            .as_ref()
            .ok_or("USB backend unavailable")?;
        refresh_editor_session(
            session,
            usb,
            self.endpoints,
            &mut self.encoder,
            self.start_guest,
            self.guest_budget,
            self.deadline,
        )
    }
    pub(super) fn initial_refresh(
        &mut self,
        session: &Session,
    ) -> Result<Value, Box<dyn std::error::Error>> {
        self.refresh(session)?;
        Ok(json!({"model":format!("{:?}",session.engine.model())}))
    }
    pub(super) fn count(
        &mut self,
        session: &Session,
        pad: u8,
    ) -> Result<Value, Box<dyn std::error::Error>> {
        self.refresh(session)?;
        self.check(session, self.guest_budget, self.wall_budget)?;
        let usb = session
            .engine
            .usb
            .as_ref()
            .ok_or("USB backend unavailable")?;
        let wire = self
            .encoder
            .push(2, &[0xf0, 0x52, 0, 0, 0x46, 0, pad, 0xf7]);
        usb.send(self.endpoints.output, &wire)?;
        let replies = collect_editor_sysex_inner(
            session,
            usb,
            self.endpoints.input,
            500,
            self.start_guest,
            self.guest_budget,
            self.deadline,
            Some(&|reply| reply.get(4..7) == Some(&[0x45, 0, pad]) && reply.len() >= 10),
        )?;
        let count = l6ctl::editor_pad_file_count(&replies, pad).ok_or_else(|| {
            format!(
                "Editor returned no file count for Sound Pad {}: {replies:02x?}",
                pad + 1
            )
        })?;
        if count == 0 {
            return Err(format!("Editor lists no WAV files for Sound Pad {}", pad + 1).into());
        }
        self.file_count = Some(count);
        Ok(json!({"pad":pad+1,"file_count":count,"replies":replies}))
    }
    pub(super) fn filename(
        &mut self,
        session: &Session,
        pad: u8,
    ) -> Result<Value, Box<dyn std::error::Error>> {
        for _ in 0..4 {
            self.refresh(session)?;
            self.check(session, self.guest_budget, self.wall_budget)?;
            let usb = session
                .engine
                .usb
                .as_ref()
                .ok_or("USB backend unavailable")?;
            let wire = self
                .encoder
                .push(2, &[0xf0, 0x52, 0, 0, 0x46, 1, pad, 0, 0, 0xf7]);
            usb.send(self.endpoints.output, &wire)?;
            let replies = collect_editor_sysex_inner(
                session,
                usb,
                self.endpoints.input,
                750,
                self.start_guest,
                self.guest_budget,
                self.deadline,
                Some(&|reply| l6ctl::editor_pad_filename_valid(reply, pad)),
            )?;
            if let Some(reply) = replies
                .into_iter()
                .find(|reply| l6ctl::editor_pad_filename_valid(reply, pad))
            {
                self.assignment_args = Some(reply[6..reply.len() - 1].to_vec());
                self.filename = Some(reply.clone());
                return Ok(json!({"pad":pad+1,"filename_reply":reply}));
            }
        }
        Err(format!(
            "Editor returned no valid filename for Sound Pad {}",
            pad + 1
        )
        .into())
    }
    pub(super) fn assign(
        &mut self,
        session: &Session,
        pad: u8,
    ) -> Result<Value, Box<dyn std::error::Error>> {
        let args = self
            .assignment_args
            .as_ref()
            .ok_or("missing selected filename")?;
        let mut request = vec![0xf0, 0x52, 0, 0, 0x31, 0x05];
        request.extend_from_slice(args);
        request.push(0xf7);
        let usb = session
            .engine
            .usb
            .as_ref()
            .ok_or("USB backend unavailable")?;
        let wire = self.encoder.push(2, &request);
        usb.send(self.endpoints.output, &wire)?;
        self.assignment_replies = collect_editor_sysex(
            session,
            usb,
            self.endpoints.input,
            300,
            self.start_guest,
            self.guest_budget,
            self.deadline,
        )?;
        self.check(session, self.guest_budget, self.wall_budget)?;
        if answer_editor_pad_dialog(
            usb,
            self.endpoints.output,
            &mut self.encoder,
            &self.assignment_replies,
        )? {
            self.assignment_replies.extend(collect_editor_sysex(
                session,
                usb,
                self.endpoints.input,
                300,
                self.start_guest,
                self.guest_budget,
                self.deadline,
            )?);
        }
        Ok(json!({"pad":pad+1,"assignment_replies":self.assignment_replies}))
    }
    pub(super) fn state(
        &mut self,
        session: &Session,
        pad: u8,
    ) -> Result<Value, Box<dyn std::error::Error>> {
        let args = self
            .assignment_args
            .clone()
            .ok_or("missing selected filename")?;
        let mut replies = Vec::new();
        for _ in 0..8 {
            self.refresh(session)?;
            self.check(session, self.guest_budget, self.wall_budget)?;
            let usb = session
                .engine
                .usb
                .as_ref()
                .ok_or("USB backend unavailable")?;
            let wire = self
                .encoder
                .push(2, &[0xf0, 0x52, 0, 0, 0x46, 2, pad, 0xf7]);
            usb.send(self.endpoints.output, &wire)?;
            replies = collect_editor_sysex(
                session,
                usb,
                self.endpoints.input,
                250,
                self.start_guest,
                self.guest_budget,
                self.deadline,
            )?;
            self.check(session, self.guest_budget, self.wall_budget)?;
            if answer_editor_pad_dialog(usb, self.endpoints.output, &mut self.encoder, &replies)? {
                replies.extend(collect_editor_sysex(
                    session,
                    usb,
                    self.endpoints.input,
                    300,
                    self.start_guest,
                    self.guest_budget,
                    self.deadline,
                )?);
            }
            if l6ctl::editor_pad_assignment_matches(&replies, pad, &args) {
                let evidence = json!({"pad":pad+1,"file_count":self.file_count,"filename_reply":self.filename,"assignment_replies":self.assignment_replies,"assignment_state":replies});
                self.assignments.push(evidence.clone());
                self.file_count = None;
                self.filename = None;
                self.assignment_args = None;
                self.assignment_replies.clear();
                return Ok(evidence);
            }
        }
        Err(format!("Editor assignment query did not match the selected file for Sound Pad {}: {replies:02x?}",pad+1).into())
    }
    pub(super) fn finish(&self, options: &Options) -> Result<Value, Box<dyn std::error::Error>> {
        let evidence = json!({"assignments":self.assignments});
        fs::write(
            options.logs.join("editor-pad-assignments.json"),
            serde_json::to_vec_pretty(&evidence)?,
        )?;
        Ok(evidence)
    }
}
