//! Scoped sound-pad playback: all state and deadlines span declarative steps.
use super::*;

pub(in crate::devices) struct SoundPadContext {
    pub phase: u8,
    pub pad: u8,
    pub target: u32,
    pub assignment: PadAssignmentSource,
    pub require_led: bool,
    pub require_stereo: bool,
    pub window_ms: u32,
    pub source: Option<String>,
    pub max_residual_db: f64,
    pub assignment_ready: Value,
    pub capture_path: Option<std::ffi::OsString>,
    pub capture_baseline: usize,
    pub trace_baseline: usize,
    pub overall_started: Instant,
    pub overall_guest_start: u32,
    pub pressed: bool,
    pub started: Instant,
    pub guest_start: u32,
    pub guest_budget: u32,
    pub wall_budget: u64,
    pub last_sample: u32,
    pub indicators: Vec<Value>,
    pub led_before: Option<bool>,
    pub led_changed: bool,
    pub held: bool,
    pub down_sequence: Option<u32>,
    pub up_sequence: Option<u32>,
    pub trace_snapshot: Option<String>,
    pub capture_snapshot: Option<Vec<u8>>,
    pub evidence: Option<Value>,
}

fn deadline_expired(
    overall_wall: Duration,
    overall_guest: u32,
    playback: Option<(Duration, u32)>,
    now: u32,
    guest_budget: u32,
    wall_budget: u64,
) -> bool {
    overall_wall > Duration::from_secs(wall_budget)
        || now.wrapping_sub(overall_guest) > guest_budget
        || playback.is_some_and(|(wall, guest)| {
            wall > Duration::from_secs(wall_budget) || now.wrapping_sub(guest) > guest_budget
        })
}

pub(in crate::devices) fn check(
    context: &SoundPadContext,
    session: &Session,
) -> Result<(), Box<dyn std::error::Error>> {
    let now = l6ctl::OwnedControl::new(&session.engine)
        .input_status(l6ctl::InputSource::Panel)?
        .guest_ms;
    if deadline_expired(
        context.overall_started.elapsed(),
        context.overall_guest_start,
        context
            .pressed
            .then(|| (context.started.elapsed(), context.guest_start)),
        now,
        context.guest_budget,
        context.wall_budget,
    ) {
        return Err("sound-pad playback exceeded its shared deadline".into());
    }
    Ok(())
}

fn sample(
    context: &mut SoundPadContext,
    session: &Session,
) -> Result<bool, Box<dyn std::error::Error>> {
    let data = session.poll()?;
    let control = l6ctl::OwnedControl::new(&session.engine);
    let panel = control.input_status(l6ctl::InputSource::Panel)?;
    if panel.guest_ms != context.last_sample {
        context.last_sample = panel.guest_ms;
        context.indicators.push(json!({"elapsed_guest_ms": panel.guest_ms.wrapping_sub(context.guest_start), "master_meter": master_meter_levels(session.engine.model(), data.indicator_levels), "indicator_levels": data.indicator_levels}));
    }
    if !control.input_status(l6ctl::InputSource::Main)?.connected {
        return Err("MAIN input disconnected during pad playback".into());
    }
    Ok(context.led_before.is_some_and(|before| {
        sound_pad_led(session.engine.model(), data.indicators, context.pad) != before
    }))
}

fn acknowledged(
    control: &l6ctl::OwnedControl<'_>,
    sequence: u32,
) -> Result<bool, Box<dyn std::error::Error>> {
    match control.input_ack(l6ctl::InputSource::Main, sequence)?.state {
        l6ctl::InputAckState::Applied => Ok(true),
        l6ctl::InputAckState::Pending => Ok(false),
        l6ctl::InputAckState::Rejected => Err("sound-pad input was rejected".into()),
        l6ctl::InputAckState::Unknown => Err("sound-pad input acknowledgement unavailable".into()),
    }
}

#[allow(clippy::too_many_arguments)]
pub(in crate::devices) fn begin(
    session: &Session,
    _options: &Options,
    pad: u8,
    target: u32,
    assignment: PadAssignmentSource,
    require_led: bool,
    window_ms: u32,
    require_stereo: bool,
    source: Option<String>,
    max_residual_db: f64,
    guest_budget: u32,
    wall_budget: u64,
    overall: (Instant, u32),
) -> Result<SoundPadContext, Box<dyn std::error::Error>> {
    let model = session.engine.model();
    let capture_path = std::env::var_os("L6_AUDIO_RAW_CAPTURE");
    if source.is_some() && capture_path.is_none() {
        return Err("sound-pad-playback expect_source requires L6_AUDIO_RAW_CAPTURE in the case environment".into());
    }
    let capture_baseline = match &capture_path {
        Some(path) => fs::metadata(path)?.len() as usize,
        None => 0,
    };
    if pad > 3 || target != 49 + u32::from(pad) {
        return Err(
            "sound-pad-playback target must match pad (pad 0..3 maps to target 49..52)".into(),
        );
    }
    if std::env::var_os("L6_AUDIO_SAMPLE_TRACE").is_none() {
        return Err(
            "sound-pad-playback requires L6_AUDIO_SAMPLE_TRACE=1 in the case environment".into(),
        );
    }
    let control = l6ctl::OwnedControl::new(&session.engine);
    let assignment_ready = if model == l6fw::Model::L6 {
        let start_guest = control.input_status(l6ctl::InputSource::Panel)?.guest_ms;
        let deadline = (Instant::now() + Duration::from_secs(30))
            .min(overall.0 + Duration::from_secs(wall_budget));
        let reader = control.sound_pad_reader_before(deadline)?;
        let check_outer = || -> Result<(), Box<dyn std::error::Error>> {
            let guest = control.input_status(l6ctl::InputSource::Panel)?.guest_ms;
            if Instant::now() >= deadline || guest.wrapping_sub(overall.1) > guest_budget {
                return Err("sound-pad assignment exceeded its shared deadline".into());
            }
            Ok(())
        };
        loop {
            check_outer()?;
            let pending = reader.sample_before(pad, None, deadline)?.pending;
            check_outer()?;
            if pending == 0 {
                break json!({"address":format!("{:#010x}", l6::sound_pad_assignments + u32::from(pad)),"pending":pending});
            }
            if control
                .input_status(l6ctl::InputSource::Panel)?
                .guest_ms
                .wrapping_sub(start_guest)
                >= guest_budget
                || Instant::now() >= deadline
            {
                return Err(format!("L6 saved pad {} assignment is still pending ({pending:#04x}) at {:#010x}; refusing to press before the file assignment completes",pad+1, l6::sound_pad_assignments + u32::from(pad)).into());
            }
            wait_guest_duration(session, 100, deadline).map_err(std::io::Error::other)?;
        }
    } else {
        json!({"model":"l6max","source":format!("{assignment:?}")})
    };
    Ok(SoundPadContext {
        phase: 0,
        pad,
        target,
        assignment,
        require_led,
        require_stereo,
        window_ms,
        source,
        max_residual_db,
        assignment_ready,
        capture_path,
        capture_baseline,
        trace_baseline: 0,
        overall_started: overall.0,
        overall_guest_start: overall.1,
        pressed: false,
        started: Instant::now(),
        guest_start: 0,
        guest_budget,
        wall_budget,
        last_sample: 0,
        indicators: Vec::new(),
        led_before: None,
        led_changed: false,
        held: false,
        down_sequence: None,
        up_sequence: None,
        trace_snapshot: None,
        capture_snapshot: None,
        evidence: None,
    })
}

pub(in crate::devices) fn press(
    context: &mut SoundPadContext,
    session: &Session,
    options: &Options,
) -> Result<Value, Box<dyn std::error::Error>> {
    check(context, session)?;
    let trace = fs::read_to_string(options.logs.join("engine.trace"))?;
    context.trace_baseline = trace
        .lines()
        .filter(|line| line.starts_with(l6ctl::SAI1_TX_PREFIX))
        .count();
    check(context, session)?;
    let control = l6ctl::OwnedControl::new(&session.engine);
    let initial = session.poll()?;
    check(context, session)?;
    let guest = control.input_status(l6ctl::InputSource::Panel)?.guest_ms;
    context.guest_start = guest;
    context.last_sample = guest;
    context.indicators = vec![
        json!({"elapsed_guest_ms":0,"master_meter":master_meter_levels(session.engine.model(), initial.indicator_levels),"indicator_levels":initial.indicator_levels}),
    ];
    context.led_before = context
        .require_led
        .then(|| sound_pad_led(session.engine.model(), initial.indicators, context.pad));
    context.started = Instant::now();
    context.pressed = true;
    context.held = true;
    context.down_sequence =
        Some(l6ctl::OwnedControl::new(&session.engine).down_with_sequence(context.target, 500)?);
    Ok(json!({"target":context.target,"sequence":context.down_sequence}))
}

pub(in crate::devices) fn hold(
    context: &mut SoundPadContext,
    session: &Session,
) -> Result<Value, Box<dyn std::error::Error>> {
    let control = l6ctl::OwnedControl::new(&session.engine);
    loop {
        let led_changed = sample(context, session)?;
        let applied = acknowledged(&control, context.down_sequence.ok_or("missing pad press")?)?;
        if applied {
            context.led_changed |= led_changed;
        }
        let elapsed = control
            .input_status(l6ctl::InputSource::Panel)?
            .guest_ms
            .wrapping_sub(context.guest_start);
        if applied && elapsed >= 400 {
            return Ok(json!({"guest_ms":elapsed,"acknowledged":true}));
        }
        if elapsed >= context.guest_budget
            || context.started.elapsed() >= Duration::from_secs(context.wall_budget)
        {
            return Err("sound-pad press did not settle before its deadline".into());
        }
        check(context, session)?;
        thread::sleep(Duration::from_millis(5));
    }
}

pub(in crate::devices) fn release(
    context: &mut SoundPadContext,
    session: &Session,
) -> Result<Value, Box<dyn std::error::Error>> {
    check(context, session)?;
    let sequence = l6ctl::OwnedControl::new(&session.engine).up_with_sequence(context.target)?;
    context.up_sequence = Some(sequence);
    Ok(json!({"sequence":sequence}))
}

pub(in crate::devices) fn observe(
    context: &mut SoundPadContext,
    session: &Session,
    options: &Options,
) -> Result<Value, Box<dyn std::error::Error>> {
    let control = l6ctl::OwnedControl::new(&session.engine);
    loop {
        context.led_changed |= sample(context, session)?;
        let applied = acknowledged(&control, context.up_sequence.ok_or("missing pad release")?)?;
        let elapsed = control
            .input_status(l6ctl::InputSource::Panel)?
            .guest_ms
            .wrapping_sub(context.guest_start);
        if applied && elapsed >= context.window_ms {
            context.held = false;
            context.trace_snapshot = Some(fs::read_to_string(options.logs.join("engine.trace"))?);
            if context.source.is_some() {
                context.capture_snapshot = Some(fs::read(
                    context
                        .capture_path
                        .as_ref()
                        .ok_or("missing audio capture")?,
                )?);
            }
            check(context, session)?;
            return Ok(json!({"guest_ms":elapsed,"acknowledged":true}));
        }
        if elapsed >= context.guest_budget
            || context.started.elapsed() >= Duration::from_secs(context.wall_budget)
        {
            return Err(format!(
                "sound-pad playback timed out at guest {elapsed} ms (limit {} ms)",
                context.guest_budget
            )
            .into());
        }
        check(context, session)?;
        thread::sleep(Duration::from_millis(5));
    }
}

fn release_if_held(
    held: &mut bool,
    release: impl FnOnce() -> Result<(), String>,
) -> Result<(), String> {
    if *held {
        release()?;
        *held = false;
    }
    Ok(())
}

pub(in crate::devices) fn abort(
    context: &mut SoundPadContext,
    session: &Session,
) -> Result<(), String> {
    release_if_held(&mut context.held, || {
        l6ctl::OwnedControl::new(&session.engine)
            .up_with_sequence(context.target)
            .map(|_| ())
            .map_err(|error| error.to_string())
    })
}

pub(in crate::devices) fn collect(
    context: &mut SoundPadContext,
    session: &Session,
    options: &Options,
) -> Result<Value, Box<dyn std::error::Error>> {
    check(context, session)?;
    let trace = context
        .trace_snapshot
        .as_ref()
        .ok_or("missing sound-pad trace snapshot")?;
    let samples = l6ctl::sai1_tx_samples(trace, context.trace_baseline);
    let nonzero_audio = l6ctl::sai1_tx_nonzero(&samples);
    let isolated_levels = std::array::from_fn::<_, 2, _>(|side| {
        let mut seen = [false; 7];
        for sample in &context.indicators {
            let meter = &sample["master_meter"];
            let level = meter[side].as_u64().unwrap_or(0) as usize;
            if meter[1 - side].as_u64() == Some(0) && (2..=6).contains(&level) {
                seen[level] = true;
            }
        }
        (2..=6).filter(|&level| seen[level]).collect::<Vec<_>>()
    });
    let left_only = isolated_levels[0].len() >= 2;
    let right_only = isolated_levels[1].len() >= 2;
    let waveform = context
        .source
        .as_ref()
        .map(|source| {
            let capture = context
                .capture_snapshot
                .as_ref()
                .ok_or("missing audio capture snapshot")?;
            compare_pad_waveform(
                &options.logs.join("card.img"),
                source,
                &capture[context.capture_baseline.min(capture.len())..],
            )
        })
        .transpose()?;
    let model = session.engine.model();
    let elapsed = l6ctl::OwnedControl::new(&session.engine)
        .input_status(l6ctl::InputSource::Panel)?
        .guest_ms
        .wrapping_sub(context.guest_start);
    let evidence = json!({"waveform":waveform,"stereo_meter":{"required":context.require_stereo,"left_only":left_only,"right_only":right_only,"isolated_levels":isolated_levels},"pad":context.pad+1,"target":context.target,"assignment_source":format!("{:?}",context.assignment),"assignment_ready":context.assignment_ready,"led":if model == l6fw::Model::L6 { usize::from(context.pad) } else { 27+usize::from(context.pad) },"pad_led_check_enabled":context.require_led,"pad_led_changed_from_baseline":context.led_changed,"indicator_samples":context.indicators,"audio_samples":samples,"nonzero_audio_after_press":nonzero_audio,"guest_window_ms":elapsed});
    check(context, session)?;
    fs::write(
        options.logs.join("sound-pad-playback.json"),
        serde_json::to_vec_pretty(&evidence)?,
    )?;
    check(context, session)?;
    context.evidence = Some(evidence.clone());
    Ok(evidence)
}

pub(in crate::devices) fn assert_waveform(
    context: &SoundPadContext,
) -> Result<Value, Box<dyn std::error::Error>> {
    let evidence = context
        .evidence
        .as_ref()
        .ok_or("missing sound-pad evidence")?;
    if !evidence["waveform"].is_null() {
        let residual = evidence["waveform"]["residual_db"]
            .as_f64()
            .unwrap_or(f64::INFINITY);
        if residual > context.max_residual_db {
            return Err(format!(
                "sound-pad waveform residual {residual:.1} dB exceeds {:.1} dB: {}",
                context.max_residual_db, evidence["waveform"]
            )
            .into());
        }
    }
    Ok(json!({"waveform":evidence["waveform"]}))
}

pub(in crate::devices) fn assert_audio(
    context: &SoundPadContext,
) -> Result<Value, Box<dyn std::error::Error>> {
    let evidence = context
        .evidence
        .as_ref()
        .ok_or("missing sound-pad evidence")?;
    if evidence["nonzero_audio_after_press"] != true {
        return Err("sound-pad playback produced no valid nonzero SAI1 TX samples; details saved to sound-pad-playback.json".into());
    }
    Ok(json!({"nonzero_audio":true}))
}

pub(in crate::devices) fn assert_led(
    context: &SoundPadContext,
) -> Result<Value, Box<dyn std::error::Error>> {
    if context.require_led && !context.led_changed {
        return Err(
            "sound-pad LED did not change; details saved to sound-pad-playback.json".into(),
        );
    }
    Ok(json!({"required":context.require_led,"changed":context.led_changed}))
}

pub(in crate::devices) fn assert_meter(
    context: &SoundPadContext,
) -> Result<Value, Box<dyn std::error::Error>> {
    let evidence = context
        .evidence
        .as_ref()
        .ok_or("missing sound-pad evidence")?;
    let meter = &evidence["stereo_meter"];
    if context.require_stereo && (meter["left_only"] != true || meter["right_only"] != true) {
        return Err("sound-pad stereo meters did not show two isolated levels per side; details saved to sound-pad-playback.json".into());
    }
    Ok(meter.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cleanup_releases_only_a_held_pad_and_retains_ownership_on_failure() {
        let mut held = true;
        assert!(release_if_held(&mut held, || Err("disconnected".into())).is_err());
        assert!(held);
        let mut releases = 0;
        release_if_held(&mut held, || {
            releases += 1;
            Ok(())
        })
        .unwrap();
        assert!(!held);
        release_if_held(&mut held, || {
            releases += 1;
            Ok(())
        })
        .unwrap();
        assert_eq!(releases, 1);
    }
    #[test]
    fn prepress_trace_does_not_count_as_playback() {
        let prepress = "l6 audio TX samples sai=00384000 words=9 nonzero=3 underruns=0 first=00000001 last=00000002 at=4ms\n";
        let baseline = prepress
            .lines()
            .filter(|line| line.starts_with(l6ctl::SAI1_TX_PREFIX))
            .count();
        assert!(!l6ctl::sai1_tx_nonzero(&l6ctl::sai1_tx_samples(
            prepress, baseline
        )));
        let after = format!(
            "{prepress}l6 audio TX samples sai=00384000 words=9 nonzero=2 underruns=0 first=00000001 last=00000002 at=5ms\n"
        );
        assert!(l6ctl::sai1_tx_nonzero(&l6ctl::sai1_tx_samples(
            &after, baseline
        )));
    }

    #[test]
    fn delayed_press_starts_a_fresh_playback_window_but_keeps_outer_budget() {
        let before_press = 800;
        let after_press = 1199;
        assert!(!deadline_expired(
            Duration::from_secs(1),
            0,
            None,
            before_press,
            2000,
            30
        ));
        assert!(!deadline_expired(
            Duration::from_secs(2),
            0,
            Some((Duration::from_secs(1), before_press)),
            after_press,
            2000,
            30
        ));
        assert_eq!(after_press - before_press, 399);
        assert!(deadline_expired(
            Duration::from_secs(2),
            0,
            Some((Duration::from_secs(1), before_press)),
            2001,
            2000,
            30
        ));
    }

    #[test]
    fn shared_deadline_preserves_outer_and_playback_anchors_and_guest_wrap() {
        let wall = Duration::from_secs(1);
        assert!(!deadline_expired(
            wall,
            u32::MAX - 4,
            Some((wall, 0)),
            4,
            10,
            30
        ));
        assert!(deadline_expired(
            wall,
            u32::MAX - 4,
            Some((wall, 0)),
            6,
            10,
            30
        ));
        assert!(deadline_expired(
            Duration::from_secs(31),
            0,
            Some((wall, 2)),
            4,
            10,
            30
        ));
        assert!(!deadline_expired(
            wall,
            0,
            Some((Duration::from_secs(30), 2)),
            4,
            10,
            30
        ));
        assert!(deadline_expired(
            wall,
            0,
            Some((Duration::from_secs(31), 2)),
            4,
            10,
            30
        ));
    }
}
