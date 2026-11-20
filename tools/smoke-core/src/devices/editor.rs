//! Editor pad assignment and playback over USB SysEx.
use super::*;

pub(super) fn run_editor_pad_assignments(
    session: &Session,
    options: &Options,
    endpoints: l6max_host::usb::Endpoints,
    guest_timeout_ms: u32,
    wall_timeout_s: u64,
) -> Result<Value, Box<dyn std::error::Error>> {
    let usb = session
        .engine
        .usb
        .as_ref()
        .ok_or("USB backend unavailable")?
        .clone();
    let start_guest = session
        .engine
        .inputs
        .as_ref()
        .ok_or("editor-pad-assignments needs panel inputs")?
        .panel
        .snapshot()
        .guest_ms;
    let deadline = Instant::now() + Duration::from_secs(wall_timeout_s);
    let mut encoder = l6max_host::usb_midi::Encoder::default();
    let identity = encoder.push(2, &[0xf0, 0x7e, 0, 0x06, 0x01, 0xf7]);
    usb.send(endpoints.output, &identity)?;
    let identity_replies = collect_editor_sysex(
        session,
        &usb,
        endpoints.input,
        500,
        start_guest,
        guest_timeout_ms,
        deadline,
    )?;
    if !identity_replies
        .iter()
        .any(|reply| reply.starts_with(&[0xf0, 0x7e, 0, 0x06, 0x02, 0x52, 0x72, 0]))
    {
        return Err(format!("Editor identity reply missing: {identity_replies:02x?}").into());
    }

    // The firmware discards Editor commands until 2B advances its session
    // from identity-only state 1 to command state 2. Match the legacy
    // DeviceInitializationService sequence before querying pad file lists.
    let global = editor_exchange(
        session,
        &usb,
        endpoints,
        &mut encoder,
        &[0xf0, 0x52, 0, 0, 0x2b, 0xf7],
        500,
        start_guest,
        guest_timeout_ms,
        deadline,
    )?;
    require_editor_reply(&global, &[0xf0, 0x52, 0, 0, 0x2a], "global settings")?;
    let app_version = editor_exchange(
        session,
        &usb,
        endpoints,
        &mut encoder,
        &[
            0xf0, 0x52, 0, 0, 0x45, 0x05, 0x02, 0, 0, 0, 0, 0, 0x27, 0, 0xf7,
        ],
        500,
        start_guest,
        guest_timeout_ms,
        deadline,
    )?;
    require_editor_reply(
        &app_version,
        &[0xf0, 0x52, 0, 0, 0, 0x11],
        "application version",
    )?;
    let sd_info = editor_exchange(
        session,
        &usb,
        endpoints,
        &mut encoder,
        &[0xf0, 0x52, 0, 0, 0x67, 0x01, 0xf7],
        500,
        start_guest,
        guest_timeout_ms,
        deadline,
    )?;
    require_editor_reply(
        &sd_info,
        &[0xf0, 0x52, 0, 0, 0x67, 0x00],
        "SD card information",
    )?;

    // Start keep-alive after initialization, matching the Editor's sequence.
    // The inspected L6 1.10 fixture does not reply to 31 0B; L6max does.
    if session.engine.model() == l6fw::Model::L6max {
        let keep_alive = editor_exchange(
            session,
            &usb,
            endpoints,
            &mut encoder,
            &[0xf0, 0x52, 0, 0, 0x31, 0x0b, 0xf7],
            500,
            start_guest,
            guest_timeout_ms,
            deadline,
        )?;
        require_editor_reply(&keep_alive, &[0xf0, 0x52, 0, 0, 0, 0x0b], "keep-alive")?;
    }

    let mut assignments = Vec::new();
    for pad in 0u8..4 {
        refresh_editor_session(
            session,
            &usb,
            endpoints,
            &mut encoder,
            start_guest,
            guest_timeout_ms,
            deadline,
        )?;
        let count_request = encoder.push(2, &[0xf0, 0x52, 0, 0, 0x46, 0, pad, 0xf7]);
        usb.send(endpoints.output, &count_request)?;
        let count_replies = collect_editor_sysex(
            session,
            &usb,
            endpoints.input,
            500,
            start_guest,
            guest_timeout_ms,
            deadline,
        )?;
        let file_count = editor_pad_file_count(&count_replies, pad);
        let file_count = file_count.ok_or_else(|| {
            format!(
                "Editor returned no file count for Sound Pad {}: {count_replies:02x?}",
                pad + 1
            )
        })?;
        if file_count == 0 {
            return Err(format!("Editor lists no WAV files for Sound Pad {}", pad + 1).into());
        }

        let mut filename_reply = None;
        for _ in 0..4 {
            refresh_editor_session(
                session,
                &usb,
                endpoints,
                &mut encoder,
                start_guest,
                guest_timeout_ms,
                deadline,
            )?;
            let request = encoder.push(2, &[0xf0, 0x52, 0, 0, 0x46, 1, pad, 0, 0, 0xf7]);
            usb.send(endpoints.output, &request)?;
            let replies = collect_editor_sysex(
                session,
                &usb,
                endpoints.input,
                750,
                start_guest,
                guest_timeout_ms,
                deadline,
            )?;
            filename_reply = replies
                .into_iter()
                .find(|reply| editor_pad_filename_valid(reply, pad));
            if filename_reply.is_some() {
                break;
            }
        }
        let filename_reply = filename_reply.ok_or_else(|| {
            format!(
                "Editor returned no valid filename for Sound Pad {}",
                pad + 1
            )
        })?;
        let assignment_args = filename_reply[6..filename_reply.len() - 1].to_vec();
        let mut assignment = vec![0xf0, 0x52, 0, 0, 0x31, 0x05];
        assignment.extend_from_slice(&assignment_args);
        assignment.push(0xf7);
        let wire = encoder.push(2, &assignment);
        usb.send(endpoints.output, &wire)?;
        let mut assignment_replies = collect_editor_sysex(
            session,
            &usb,
            endpoints.input,
            300,
            start_guest,
            guest_timeout_ms,
            deadline,
        )?;
        if answer_editor_pad_dialog(&usb, endpoints.output, &mut encoder, &assignment_replies)? {
            assignment_replies.extend(collect_editor_sysex(
                session,
                &usb,
                endpoints.input,
                300,
                start_guest,
                guest_timeout_ms,
                deadline,
            )?);
        }

        let mut state_replies = Vec::new();
        let mut assigned = false;
        for _ in 0..8 {
            refresh_editor_session(
                session,
                &usb,
                endpoints,
                &mut encoder,
                start_guest,
                guest_timeout_ms,
                deadline,
            )?;
            let request = encoder.push(2, &[0xf0, 0x52, 0, 0, 0x46, 2, pad, 0xf7]);
            usb.send(endpoints.output, &request)?;
            state_replies = collect_editor_sysex(
                session,
                &usb,
                endpoints.input,
                250,
                start_guest,
                guest_timeout_ms,
                deadline,
            )?;
            if answer_editor_pad_dialog(&usb, endpoints.output, &mut encoder, &state_replies)? {
                state_replies.extend(collect_editor_sysex(
                    session,
                    &usb,
                    endpoints.input,
                    300,
                    start_guest,
                    guest_timeout_ms,
                    deadline,
                )?);
            }
            assigned = editor_pad_assignment_matches(&state_replies, pad, &assignment_args);
            if assigned {
                break;
            }
        }
        if !assigned {
            return Err(format!(
                "Editor assignment query did not match the selected file for Sound Pad {}: {state_replies:02x?}",
                pad + 1
            )
            .into());
        }
        assignments.push(json!({
            "pad": pad + 1,
            "file_count": file_count,
            "filename_reply": filename_reply,
            "assignment_replies": assignment_replies,
            "assignment_state": state_replies,
        }));
    }
    let evidence = json!({"assignments":assignments});
    fs::write(
        options.logs.join("editor-pad-assignments.json"),
        serde_json::to_vec_pretty(&evidence)?,
    )?;
    Ok(evidence)
}

pub(super) fn run_editor_pad_playback(
    session: &Session,
    options: &Options,
    endpoints: l6max_host::usb::Endpoints,
    pad: u8,
    guest_timeout_ms: u32,
    wall_timeout_s: u64,
) -> Result<Value, Box<dyn std::error::Error>> {
    if session.engine.model() != l6fw::Model::L6 || pad > 3 {
        return Err(std::io::Error::other("editor-pad-playback requires L6 and pad 0..3").into());
    }
    if std::env::var_os("L6_AUDIO_SAMPLE_TRACE").is_none() {
        return Err(std::io::Error::other(
            "editor-pad-playback requires L6_AUDIO_SAMPLE_TRACE=1 in the case environment",
        )
        .into());
    }
    let usb = session
        .engine
        .usb
        .as_ref()
        .ok_or("USB backend unavailable")?
        .clone();
    let inputs = session
        .engine
        .inputs
        .as_ref()
        .ok_or("editor-pad-playback needs panel inputs")?;
    let start_guest = inputs.panel.snapshot().guest_ms;
    let deadline = Instant::now() + Duration::from_secs(wall_timeout_s);
    let trace_path = options.logs.join("engine.trace");
    let audio_prefix = "l6 audio TX samples sai=00384000";
    let audio_before = fs::read_to_string(&trace_path)?
        .lines()
        .filter(|line| line.starts_with(audio_prefix))
        .count();
    let mut encoder = l6max_host::usb_midi::Encoder::default();
    let play = editor_exchange(
        session,
        &usb,
        endpoints,
        &mut encoder,
        &[0xf0, 0x52, 0, 0, 0x31, 0x08, pad, 1, 0xf7],
        100,
        start_guest,
        guest_timeout_ms,
        deadline,
    )?;
    let dialog_answered = answer_editor_pad_dialog(&usb, endpoints.output, &mut encoder, &play)?;
    let mut state_replies = Vec::new();
    for _ in 0..12 {
        state_replies = editor_exchange(
            session,
            &usb,
            endpoints,
            &mut encoder,
            &[0xf0, 0x52, 0, 0, 0x46, 0x03, pad, 0xf7],
            250,
            start_guest,
            guest_timeout_ms,
            deadline,
        )?;
        if state_replies
            .iter()
            .any(|reply| reply.get(4..8) == Some(&[0x45, 0x03, pad, 0x01]))
        {
            break;
        }
        if answer_editor_pad_dialog(&usb, endpoints.output, &mut encoder, &state_replies)? {
            let _ = collect_editor_sysex(
                session,
                &usb,
                endpoints.input,
                300,
                start_guest,
                guest_timeout_ms,
                deadline,
            )?;
        }
    }
    let playing = state_replies
        .iter()
        .any(|reply| reply.get(4..8) == Some(&[0x45, 0x03, pad, 0x01]));
    if !playing {
        return Err(format!(
            "Editor Sound Pad {} did not enter playing state: {state_replies:02x?}",
            pad + 1
        )
        .into());
    }
    wait_guest_duration(session, 1_200, deadline).map_err(std::io::Error::other)?;
    let trace = fs::read_to_string(&trace_path)?;
    let samples = trace
        .lines()
        .filter(|line| line.starts_with(audio_prefix))
        .skip(audio_before)
        .filter_map(|line| {
            line.split_whitespace()
                .find_map(|field| field.strip_prefix("nonzero="))
                .and_then(|value| value.parse::<u64>().ok())
        })
        .collect::<Vec<_>>();
    let nonzero_audio = samples.iter().any(|count| *count > 0);
    let evidence = json!({
        "pad": pad + 1,
        "play_replies": play,
        "state_replies": state_replies,
        "assignment_dialog_answered": dialog_answered,
        "playing_state": playing,
        "nonzero_audio_samples": samples,
        "guest_elapsed_ms": inputs.panel.snapshot().guest_ms.wrapping_sub(start_guest),
    });
    fs::write(
        options
            .logs
            .join(format!("editor-pad-playback-{}.json", pad + 1)),
        serde_json::to_vec_pretty(&evidence)?,
    )?;
    if !nonzero_audio {
        return Err(format!("Editor Sound Pad {} entered playing state but produced no nonzero SAI1 TX samples: {evidence}", pad + 1).into());
    }
    Ok(evidence)
}

pub(super) fn run_l6max_ui_pad_assignment(
    session: &Session,
    options: &Options,
    endpoints: l6max_host::usb::Endpoints,
    pad: u8,
    guest_timeout_ms: u32,
    wall_timeout_s: u64,
) -> Result<Value, Box<dyn std::error::Error>> {
    if session.engine.model() != l6fw::Model::L6max || pad > 3 {
        return Err(
            std::io::Error::other("l6max-ui-pad-assignment requires L6max and pad 0..3").into(),
        );
    }
    let usb = session
        .engine
        .usb
        .as_ref()
        .ok_or("USB backend unavailable")?
        .clone();
    let inputs = session
        .engine
        .inputs
        .as_ref()
        .ok_or("L6max UI pad assignment needs panel inputs")?;
    let start_guest = inputs.panel.snapshot().guest_ms;
    let deadline = Instant::now() + Duration::from_secs(wall_timeout_s);
    let mut key_evidence = Vec::new();
    let tap = |target: u32,
               main: bool,
               key_evidence: &mut Vec<Value>|
     -> Result<(), Box<dyn std::error::Error>> {
        let before = if main {
            inputs.main.snapshot().applied
        } else {
            inputs.panel.snapshot().applied
        };
        let release_before = if main {
            inputs.main.snapshot().released
        } else {
            inputs.panel.snapshot().released
        };
        if main {
            inputs.main.tap(target, 120)?;
        } else {
            inputs.panel.tap(target, 120)?;
        }
        loop {
            session.poll().map_err(std::io::Error::other)?;
            let (applied, released) = if main {
                let snapshot = inputs.main.snapshot();
                (snapshot.applied, snapshot.released)
            } else {
                let snapshot = inputs.panel.snapshot();
                (snapshot.applied, snapshot.released)
            };
            if applied > before && released > release_before {
                key_evidence.push(
                    json!({"target":target,"main":main,"applied":applied,"released":released}),
                );
                break;
            }
            let elapsed = inputs.panel.snapshot().guest_ms.wrapping_sub(start_guest);
            if Instant::now() >= deadline || elapsed >= guest_timeout_ms {
                return Err(
                    format!("L6max UI key {target} did not complete before its deadline").into(),
                );
            }
            thread::sleep(Duration::from_millis(3));
        }
        wait_guest_duration(session, 1_500, deadline).map_err(std::io::Error::other)?;
        Ok(())
    };

    // A "Data OK." popup can still be up, and it would swallow the Menu press.
    wait_guest_duration(session, 5000, deadline)?;

    // Sound Assign navigation: Menu, Confirm into Sound Pad, Confirm into Sound
    // Assign, select the pad, Confirm into its file list, Down past its first row,
    // None Assigned, and Confirm the first WAV. Menus keep their cursors between
    // visits, so only the first pad moves down to Sound Pad, and each later pad moves
    // one row from the last. A stray Down here reaches SD Card and its Format.
    tap(0, true, &mut key_evidence)?;
    let to_sound_pad: &[u32] = if pad == 0 { &[2, 2] } else { &[] };
    let to_pad: &[u32] = if pad == 0 { &[] } else { &[2] };
    for &button in to_sound_pad
        .iter()
        .chain(&[3, 3])
        .chain(to_pad)
        .chain(&[3, 2, 3])
    {
        // Each menu loads after its key, and the file list drops keys until then.
        wait_guest_duration(session, 2000, deadline)?;
        tap(button, false, &mut key_evidence)?;
    }
    // Back out through Sound Pad and Menu to the home screen, which saves the
    // assignment and leaves the next visit starting from the same place.
    for _ in 0..3 {
        wait_guest_duration(session, 2000, deadline)?;
        tap(0, true, &mut key_evidence)?;
    }
    wait_guest_duration(session, 2000, deadline)?;

    // Verify the UI assignment through the same Editor state query used by
    // the original probe, after opening an Editor session on cable 2.
    let mut encoder = l6max_host::usb_midi::Encoder::default();
    let identity = encoder.push(2, &[0xf0, 0x7e, 0, 0x06, 0x01, 0xf7]);
    usb.send(endpoints.output, &identity)?;
    let identity_replies = collect_editor_sysex(
        session,
        &usb,
        endpoints.input,
        500,
        start_guest,
        guest_timeout_ms,
        deadline,
    )?;
    if !identity_replies
        .iter()
        .any(|reply| reply.starts_with(&[0xf0, 0x7e, 0, 0x06, 0x02, 0x52, 0x72, 0]))
    {
        return Err(format!(
            "L6max Editor identity reply missing after UI assignment: {identity_replies:02x?}"
        )
        .into());
    }
    let global = editor_exchange(
        session,
        &usb,
        endpoints,
        &mut encoder,
        &[0xf0, 0x52, 0, 0, 0x2b, 0xf7],
        500,
        start_guest,
        guest_timeout_ms,
        deadline,
    )?;
    require_editor_reply(&global, &[0xf0, 0x52, 0, 0, 0x2a], "global settings")?;
    let keep_alive = editor_exchange(
        session,
        &usb,
        endpoints,
        &mut encoder,
        &[0xf0, 0x52, 0, 0, 0x31, 0x0b, 0xf7],
        250,
        start_guest,
        guest_timeout_ms,
        deadline,
    )?;
    require_editor_reply(&keep_alive, &[0xf0, 0x52, 0, 0, 0, 0x0b], "keep-alive")?;
    let mut assignment_replies = Vec::new();
    for _ in 0..8 {
        assignment_replies = editor_exchange(
            session,
            &usb,
            endpoints,
            &mut encoder,
            &[0xf0, 0x52, 0, 0, 0x46, 0x02, pad, 0xf7],
            300,
            start_guest,
            guest_timeout_ms,
            deadline,
        )?;
        if assignment_replies.iter().any(|reply| {
            reply.get(4..7) == Some(&[0x45, 0x02, pad])
                && reply.len() >= 12
                && reply.get(7..9) != Some(&[0x7f, 0x7f])
                && reply.get(9..11) != Some(&[0, 0])
        }) {
            break;
        }
    }
    let assigned = assignment_replies.iter().any(|reply| {
        reply.get(4..7) == Some(&[0x45, 0x02, pad])
            && reply.len() >= 12
            && reply.get(7..9) != Some(&[0x7f, 0x7f])
            && reply.get(9..11) != Some(&[0, 0])
    });
    let evidence = json!({
        "pad": pad + 1,
        "ui_keys": key_evidence,
        "identity_replies": identity_replies,
        "assignment_replies": assignment_replies,
        "assigned": assigned,
        "guest_elapsed_ms": inputs.panel.snapshot().guest_ms.wrapping_sub(start_guest),
    });
    fs::write(
        options
            .logs
            .join(format!("l6max-ui-pad-assignment-{}.json", pad + 1)),
        serde_json::to_vec_pretty(&evidence)?,
    )?;
    if !assigned {
        return Err(format!(
            "L6max UI did not assign a WAV to Sound Pad {}: {evidence}",
            pad + 1
        )
        .into());
    }
    Ok(evidence)
}

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
    let replies = editor_exchange(
        session,
        usb,
        endpoints,
        encoder,
        &[0xf0, 0x52, 0, 0, 0x31, 0x0b, 0xf7],
        250,
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

pub(super) fn editor_pad_file_count(replies: &[Vec<u8>], pad: u8) -> Option<u16> {
    replies.iter().find_map(|reply| {
        (reply.get(4..7) == Some(&[0x45, 0x00, pad]) && reply.len() >= 10)
            .then(|| ((reply[7] as u16) << 7) | reply[8] as u16)
    })
}

pub(super) fn editor_pad_filename_valid(reply: &[u8], pad: u8) -> bool {
    reply.get(4..7) == Some(&[0x45, 0x01, pad])
        && reply.len() >= 12
        && reply.get(7..9) != Some(&[0x7f, 0x7f])
        && reply.get(9..11) != Some(&[0, 0])
}

pub(super) fn editor_pad_assignment_matches(replies: &[Vec<u8>], pad: u8, args: &[u8]) -> bool {
    replies.iter().any(|reply| {
        reply.get(4..7) == Some(&[0x45, 0x02, pad])
            && reply.get(6..reply.len().saturating_sub(1)) == Some(args)
    })
}
