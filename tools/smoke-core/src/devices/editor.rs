//! Editor pad assignment and playback over USB SysEx.
use super::*;

fn panel_guest(session: &Session) -> Result<u32, Box<dyn std::error::Error>> {
    Ok(session
        .control()
        .input_status(l6ctl::InputSource::Panel)?
        .guest_ms)
}

#[allow(clippy::too_many_arguments)]
pub(super) fn editor_exchange(
    session: &Session,
    usb: &l6ctl::OwnedControl<'_>,
    endpoints: l6max_host::usb::Endpoints,
    message: &[u8],
    timeout_ms: u32,
    start_guest: u32,
    guest_timeout_ms: u32,
    wall_deadline: Instant,
) -> Result<Vec<Vec<u8>>, Box<dyn std::error::Error>> {
    usb.usb_sysex_send(endpoints, 2, message)?;
    collect_editor_sysex(
        session,
        usb,
        endpoints,
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
    usb: &l6ctl::OwnedControl<'_>,
    endpoints: l6max_host::usb::Endpoints,
    message: &[u8],
    expected: &[u8],
    start_guest: u32,
    guest_timeout_ms: u32,
    wall_deadline: Instant,
) -> Result<Vec<Vec<u8>>, Box<dyn std::error::Error>> {
    const RESPONSE_GUEST_TIMEOUT_MS: u32 = 2_000;
    const RESPONSE_WALL_TIMEOUT_S: u64 = 30;
    usb.usb_sysex_send(endpoints, 2, message)?;
    let matches = |reply: &[u8]| reply.starts_with(expected);
    collect_editor_sysex_inner(
        session,
        usb,
        endpoints,
        RESPONSE_GUEST_TIMEOUT_MS,
        start_guest,
        guest_timeout_ms,
        wall_deadline.min(Instant::now() + Duration::from_secs(RESPONSE_WALL_TIMEOUT_S)),
        Some(&matches),
    )
}

pub(super) fn refresh_editor_session(
    session: &Session,
    usb: &l6ctl::OwnedControl<'_>,
    endpoints: l6max_host::usb::Endpoints,
    start_guest: u32,
    guest_timeout_ms: u32,
    wall_deadline: Instant,
) -> Result<(), Box<dyn std::error::Error>> {
    if session.model() == l6fw::Model::L6 {
        return Ok(());
    }
    let replies = editor_exchange_expected(
        session,
        usb,
        endpoints,
        &l6ctl::EditorRequest::KeepAlive.encode(),
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
    usb: &l6ctl::OwnedControl<'_>,
    endpoints: l6max_host::usb::Endpoints,
    timeout_ms: u32,
    start_guest: u32,
    guest_timeout_ms: u32,
    wall_deadline: Instant,
) -> Result<Vec<Vec<u8>>, Box<dyn std::error::Error>> {
    collect_editor_sysex_inner(
        session,
        usb,
        endpoints,
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
    usb: &l6ctl::OwnedControl<'_>,
    endpoints: l6max_host::usb::Endpoints,
    timeout_ms: u32,
    start_guest: u32,
    guest_timeout_ms: u32,
    wall_deadline: Instant,
    expected: Option<&dyn Fn(&[u8]) -> bool>,
) -> Result<Vec<Vec<u8>>, Box<dyn std::error::Error>> {
    let started = panel_guest(session)?;
    let mut decoder = l6ctl::UsbSysexDecoder::default();
    loop {
        session.poll().map_err(std::io::Error::other)?;
        usb.usb_sysex_poll(endpoints, 2, &mut decoder)?;
        if decoder.pending().is_empty()
            && expected.is_some_and(|matches| decoder.replies().iter().any(|reply| matches(reply)))
        {
            return Ok(decoder.replies().to_vec());
        }
        let now = panel_guest(session)?;
        if now.wrapping_sub(started) >= timeout_ms {
            if !decoder.pending().is_empty() {
                return Err(
                    format!("truncated Editor SysEx reply: {:02x?}", decoder.pending()).into(),
                );
            }
            return Ok(decoder.replies().to_vec());
        }
        if Instant::now() >= wall_deadline || now.wrapping_sub(start_guest) > guest_timeout_ms {
            return Err("Editor pad assignment exceeded its guest or wall deadline".into());
        }
        thread::sleep(Duration::from_millis(2));
    }
}

pub(super) fn answer_editor_pad_dialog(
    usb: &l6ctl::OwnedControl<'_>,
    endpoints: l6max_host::usb::Endpoints,
    replies: &[Vec<u8>],
) -> Result<bool, Box<dyn std::error::Error>> {
    let pending = l6ctl::editor_pad_dialog_pending(replies);
    if !pending {
        return Ok(false);
    }
    usb.usb_sysex_send(
        endpoints,
        2,
        &l6ctl::EditorRequest::AcceptPadDialog.encode(),
    )?;
    Ok(true)
}

/// One Editor verification retains the original endpoint and clock
/// across separately reported scenario steps.
pub(super) struct UiPadScope {
    pad: u8,
    endpoints: l6max_host::usb::Endpoints,
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
        if session.model() != l6fw::Model::L6max || pad > 3 {
            return Err("UI pad verification requires L6max and pad 0..3".into());
        }
        let start_guest = panel_guest(session)?;
        Ok(Self {
            pad,
            endpoints,
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
        let guest = panel_guest(session)?;
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
        let control = session.control();
        let usb = &control;
        usb.usb_sysex_send(self.endpoints, 2, &l6ctl::EditorRequest::Identity.encode())?;
        self.identity = collect_editor_sysex(
            session,
            usb,
            self.endpoints,
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
        let control = session.control();
        let usb = &control;
        let replies = editor_exchange_expected(
            session,
            usb,
            self.endpoints,
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
        let control = session.control();
        let usb = &control;
        for _ in 0..8 {
            self.check(session, self.guest_budget, self.wall_budget)?;
            self.assignments = editor_exchange(
                session,
                usb,
                self.endpoints,
                &l6ctl::EditorRequest::PadAssignment { pad: self.pad }.encode(),
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
        let guest = panel_guest(session)?;
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

/// Editor-controlled playback retains its USB endpoint and deadline.
pub(super) struct EditorPlaybackScope {
    pad: u8,
    endpoints: l6max_host::usb::Endpoints,
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
        if session.model() != l6fw::Model::L6 || pad > 3 {
            return Err("Editor playback requires L6 and pad 0..3".into());
        }
        if std::env::var_os("L6_AUDIO_SAMPLE_TRACE").is_none() {
            return Err("Editor playback requires L6_AUDIO_SAMPLE_TRACE=1".into());
        }
        let start_guest = panel_guest(session)?;
        Ok(Self {
            pad,
            endpoints,
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
        let now = panel_guest(session)?;
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
        let control = session.control();
        let usb = &control;
        self.play = editor_exchange(
            session,
            usb,
            self.endpoints,
            &l6ctl::EditorRequest::PlayPad { pad: self.pad }.encode(),
            100,
            self.start_guest,
            self.guest_budget,
            self.deadline,
        )?;
        self.check(session, self.guest_budget, self.wall_budget)?;
        self.dialog_answered = answer_editor_pad_dialog(usb, self.endpoints, &self.play)?;
        Ok(json!({"play_replies":self.play,"assignment_dialog_answered":self.dialog_answered}))
    }
    pub(super) fn wait_playing(
        &mut self,
        session: &Session,
    ) -> Result<Value, Box<dyn std::error::Error>> {
        let control = session.control();
        let usb = &control;
        for _ in 0..12 {
            self.check(session, self.guest_budget, self.wall_budget)?;
            self.state = editor_exchange(
                session,
                usb,
                self.endpoints,
                &l6ctl::EditorRequest::PadPlaybackState { pad: self.pad }.encode(),
                250,
                self.start_guest,
                self.guest_budget,
                self.deadline,
            )?;
            self.check(session, self.guest_budget, self.wall_budget)?;
            if l6ctl::editor_pad_playing(&self.state, self.pad) {
                return Ok(json!({"state_replies":self.state,"playing_state":true}));
            }
            if answer_editor_pad_dialog(usb, self.endpoints, &self.state)? {
                let _ = collect_editor_sysex(
                    session,
                    usb,
                    self.endpoints,
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
        let start = panel_guest(session)?;
        loop {
            self.check(session, self.guest_budget, self.wall_budget)?;
            let elapsed = panel_guest(session)?.wrapping_sub(start);
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
        let guest = panel_guest(session)?;
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
/// this scope retains the USB endpoint, exact reply data and shared deadline.
pub(super) struct EditorAssignmentScope {
    endpoints: l6max_host::usb::Endpoints,
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
    state_replies: Option<Vec<Vec<u8>>>,
    assignments: Vec<Value>,
}
impl EditorAssignmentScope {
    pub(super) fn begin(
        session: &Session,
        endpoints: l6max_host::usb::Endpoints,
        guest_budget: u32,
        wall_budget: u64,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let start_guest = panel_guest(session)?;
        Ok(Self {
            endpoints,
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
            state_replies: None,
            assignments: vec![],
        })
    }
    pub(super) fn check(
        &self,
        session: &Session,
        guest: u32,
        wall: u64,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let now = panel_guest(session)?;
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
        let control = session.control();
        let usb = &control;
        usb.usb_sysex_send(self.endpoints, 2, &l6ctl::EditorRequest::Identity.encode())?;
        let replies = collect_editor_sysex(
            session,
            usb,
            self.endpoints,
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
        let control = session.control();
        let usb = &control;
        let replies = editor_exchange_expected(
            session,
            usb,
            self.endpoints,
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
        let control = session.control();
        let usb = &control;
        refresh_editor_session(
            session,
            usb,
            self.endpoints,
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
        Ok(json!({"model":format!("{:?}",session.model())}))
    }
    pub(super) fn count(
        &mut self,
        session: &Session,
        pad: u8,
    ) -> Result<Value, Box<dyn std::error::Error>> {
        self.refresh(session)?;
        self.check(session, self.guest_budget, self.wall_budget)?;
        let control = session.control();
        let usb = &control;
        usb.usb_sysex_send(
            self.endpoints,
            2,
            &l6ctl::EditorRequest::PadFileCount { pad }.encode(),
        )?;
        let replies = collect_editor_sysex_inner(
            session,
            usb,
            self.endpoints,
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
            let control = session.control();
            let usb = &control;
            usb.usb_sysex_send(
                self.endpoints,
                2,
                &l6ctl::EditorRequest::PadFilename { pad }.encode(),
            )?;
            let replies = collect_editor_sysex_inner(
                session,
                usb,
                self.endpoints,
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
        let request = l6ctl::EditorRequest::AssignPad { args: args.clone() }.encode();
        let control = session.control();
        let usb = &control;
        usb.usb_sysex_send(self.endpoints, 2, &request)?;
        self.assignment_replies = collect_editor_sysex(
            session,
            usb,
            self.endpoints,
            300,
            self.start_guest,
            self.guest_budget,
            self.deadline,
        )?;
        Ok(json!({"pad":pad+1,"assignment_replies":self.assignment_replies}))
    }
    pub(super) fn dialog(
        &mut self,
        session: &Session,
        pad: u8,
    ) -> Result<Value, Box<dyn std::error::Error>> {
        let control = session.control();
        let usb = &control;
        let answered = answer_editor_pad_dialog(usb, self.endpoints, &self.assignment_replies)?;
        if answered {
            self.assignment_replies.extend(collect_editor_sysex(
                session,
                usb,
                self.endpoints,
                300,
                self.start_guest,
                self.guest_budget,
                self.deadline,
            )?);
        }
        Ok(
            json!({"pad":pad+1,"dialog_answered":answered,"assignment_replies":self.assignment_replies}),
        )
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
            let control = session.control();
            let usb = &control;
            usb.usb_sysex_send(
                self.endpoints,
                2,
                &l6ctl::EditorRequest::PadAssignment { pad }.encode(),
            )?;
            replies = collect_editor_sysex(
                session,
                usb,
                self.endpoints,
                250,
                self.start_guest,
                self.guest_budget,
                self.deadline,
            )?;
            self.check(session, self.guest_budget, self.wall_budget)?;
            if answer_editor_pad_dialog(usb, self.endpoints, &replies)? {
                replies.extend(collect_editor_sysex(
                    session,
                    usb,
                    self.endpoints,
                    300,
                    self.start_guest,
                    self.guest_budget,
                    self.deadline,
                )?);
            }
            if l6ctl::editor_pad_assignment_matches(&replies, pad, &args) {
                break;
            }
        }
        let matched = l6ctl::editor_pad_assignment_matches(&replies, pad, &args);
        self.state_replies = Some(replies.clone());
        Ok(json!({"pad":pad+1,"matched":matched,"assignment_state":replies}))
    }
    pub(super) fn assert_state(&mut self, pad: u8) -> Result<Value, Box<dyn std::error::Error>> {
        let args = self
            .assignment_args
            .as_ref()
            .ok_or("missing selected filename")?;
        let replies = self
            .state_replies
            .take()
            .ok_or("missing Editor assignment state")?;
        verify_assignment_state(&replies, pad, args)?;
        let evidence = json!({"pad":pad+1,"file_count":self.file_count,"filename_reply":self.filename,"assignment_replies":self.assignment_replies,"assignment_state":replies});
        self.assignments.push(evidence.clone());
        self.file_count = None;
        self.filename = None;
        self.assignment_args = None;
        self.assignment_replies.clear();
        Ok(evidence)
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

fn verify_assignment_state(replies: &[Vec<u8>], pad: u8, args: &[u8]) -> Result<(), String> {
    if l6ctl::editor_pad_assignment_matches(replies, pad, args) {
        Ok(())
    } else {
        Err(format!(
            "Editor assignment query did not match the selected file for Sound Pad {}: {replies:02x?}",
            pad + 1
        ))
    }
}

#[cfg(test)]
mod assignment_assertion_tests {
    use super::verify_assignment_state;
    #[test]
    fn mismatch_reports_pad_and_received_reply() {
        let reply = vec![0xf0, 0x52, 0, 0, 0x45, 0x02, 1, 2, 3, 0xf7];
        assert!(verify_assignment_state(std::slice::from_ref(&reply), 1, &[1, 2, 3]).is_ok());
        let failure = verify_assignment_state(&[reply], 1, &[1, 2, 4]).unwrap_err();
        assert!(failure.contains("Sound Pad 2"));
        assert!(failure.contains("45, 02, 01, 02, 03"));
    }
}
