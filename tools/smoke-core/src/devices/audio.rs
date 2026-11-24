//! Audio callbacks, sound pads and recorder playback.
use super::*;

// Revision-specific catalog names retained from the qualified audio-path probe.
pub(super) fn callbacks(model: l6fw::Model) -> &'static [(u32, &'static str)] {
    match model {
        l6fw::Model::L6 => &[
            (l6::sai1_rx_dma_callback, "l6_sai1_rx_dma_callback"),
            (
                l6::audio_input_block_semaphore_callback,
                "l6_audio_input_block_semaphore_callback",
            ),
            (
                l6::audio_staging_ring_producer,
                "l6_audio_staging_ring_producer",
            ),
            (
                l6::audio_process_block_consumer,
                "l6_audio_process_block_consumer",
            ),
            (l6::dsp_stream_callback, "l6_dsp_stream_callback"),
        ],
        l6fw::Model::L6max => &[
            (l6max::audio_ring_consumer, "audio_ring_consumer"),
            (l6max::dsp_stream_callback, "dsp_stream_callback"),
        ],
    }
}

pub(super) fn verify_sai_edma_trace(
    model: l6fw::Model,
    trace: &str,
) -> Result<Value, Box<dyn std::error::Error>> {
    let paths: &[(u32, u32, u32)] = match model {
        l6fw::Model::L6 => &[(0x4038_4000, 14, 15), (0x4038_c000, 12, 13)],
        l6fw::Model::L6max => &[(0x4038_4000, 14, 15)],
    };
    let timestamp = |line: &str| {
        line.split("at=")
            .nth(1)
            .and_then(|value| value.strip_suffix("ms"))
            .and_then(|value| value.parse::<u64>().ok())
    };
    let lines = trace.lines().collect::<Vec<_>>();
    let mut enable_times = Vec::new();
    for (base, _, _) in paths {
        for (direction, offset) in [("TX", 0x08), ("RX", 0x88)] {
            let address = base + offset;
            let enabled_at = lines
                .iter()
                .filter(|line| {
                    line.contains(&format!("write addr={address:08x}"))
                        && line.contains("value=80040001")
                })
                .filter_map(|line| timestamp(line))
                .max()
                .ok_or_else(|| {
                    format!(
                        "{} SAI at {base:#010x} did not enable {direction} FIFO DMA requests",
                        model.name()
                    )
                })?;
            enable_times.push(enabled_at);
        }
    }
    let latest_enable = enable_times.into_iter().max().unwrap_or_default();
    let active = lines
        .iter()
        .copied()
        .filter(|line| timestamp(line).is_some_and(|time| time >= latest_enable))
        .collect::<Vec<_>>();
    let mut streams = Vec::new();
    for (base, tx_channel, rx_channel) in paths {
        for (direction, fifo_offset, channel) in
            [("TX", 0x20, tx_channel), ("RX", 0xa0, rx_channel)]
        {
            let fifo = base + fifo_offset;
            if !active
                .iter()
                .any(|line| line.contains(&format!("addr={fifo:08x}")))
            {
                return Err(format!(
                    "{} SAI at {base:#010x} had no post-enable {direction} FIFO traffic",
                    model.name()
                )
                .into());
            }
            let tcd = 0x400e_9000 + channel * 0x20;
            if !active
                .iter()
                .any(|line| line.contains(&format!("addr={tcd:08x}")))
            {
                return Err(format!(
                    "{} SAI {direction} at {base:#010x} had no eDMA channel {channel} TCD activity",
                    model.name()
                )
                .into());
            }
            streams.push(json!({
                "sai_base": format!("{base:#010x}"),
                "direction": direction,
                "fifo_address": format!("{fifo:#010x}"),
                "edma_channel": channel,
                "tcd_address": format!("{tcd:#010x}"),
            }));
        }
    }
    Ok(json!({"latest_enable_ms":latest_enable,"streams":streams}))
}

/// Wait for the exact L6 pad-1 file path and its assignment-pending flag.
pub(super) fn wait_l6_sound_pad_file(
    session: &Session,
    path: &str,
    guest_timeout_ms: u32,
    wall_timeout_s: u64,
) -> Result<Value, Box<dyn std::error::Error>> {
    const GUEST_TIMEOUT_MS: u32 = 10_000;
    const WALL_TIMEOUT_S: u64 = 180;
    const POLL_INTERVAL_MS: u64 = 10;
    if session.engine.model() != l6fw::Model::L6 {
        return Err("saved pad-file readiness requires L6".into());
    }
    let expected: Vec<u8> = format!("A:\\{}", path.replace('/', "\\"))
        .encode_utf16()
        .chain(std::iter::once(0))
        .flat_map(u16::to_le_bytes)
        .collect();
    let qmp = &session.engine.management[0];
    let start = guest_now(session);
    let deadline = Instant::now() + Duration::from_secs(wall_timeout_s.min(WALL_TIMEOUT_S));
    loop {
        session.poll()?;
        let assigned = qmp.read_memory(0, l6::sound_pad1_path, expected.len())?;
        let pending = qmp.read_memory(0, l6::sound_pad_assignments, 1)?[0];
        if assigned == expected && pending == 0 {
            return Ok(json!({"path": path, "pending": pending}));
        }
        if guest_now(session).wrapping_sub(start) >= guest_timeout_ms.min(GUEST_TIMEOUT_MS)
            || Instant::now() >= deadline
        {
            return Err(format!(
                "L6 pad 1 file {path} was not assigned before the deadline (pending={pending:#04x})"
            )
            .into());
        }
        thread::sleep(Duration::from_millis(POLL_INTERVAL_MS));
    }
}

pub(super) fn run_sound_pad_playback(
    session: &Session,
    options: &Options,
    pad: u8,
    target: u32,
    assignment: PadAssignmentSource,
    require_pad_led_change: bool,
    playback_window_ms: u32,
    require_stereo_meter: bool,
    expect_source: Option<&str>,
    max_residual_db: f64,
    guest_timeout_ms: u32,
    wall_timeout_s: u64,
) -> Result<Value, Box<dyn std::error::Error>> {
    let model = session.engine.model();
    let capture_path = std::env::var_os("L6_AUDIO_RAW_CAPTURE");
    if expect_source.is_some() && capture_path.is_none() {
        return Err(std::io::Error::other(
            "sound-pad-playback expect_source requires L6_AUDIO_RAW_CAPTURE in the case environment",
        )
        .into());
    }
    let capture_baseline = match &capture_path {
        Some(path) => fs::metadata(path)?.len() as usize,
        None => 0,
    };
    if pad > 3 || target != 49 + u32::from(pad) {
        return Err(std::io::Error::other(
            "sound-pad-playback target must match pad (pad 0..3 maps to target 49..52)",
        )
        .into());
    }
    if std::env::var_os("L6_AUDIO_SAMPLE_TRACE").is_none() {
        return Err(std::io::Error::other(
            "sound-pad-playback requires L6_AUDIO_SAMPLE_TRACE=1 in the case environment",
        )
        .into());
    }
    let inputs = session
        .engine
        .inputs
        .as_ref()
        .ok_or_else(|| std::io::Error::other("sound-pad-playback has no panel inputs"))?;
    let assignment_ready = if model == l6fw::Model::L6 {
        let address = l6::sound_pad_assignments + u32::from(pad);
        let start_guest = inputs.panel.snapshot().guest_ms;
        let deadline = Instant::now() + Duration::from_secs(wall_timeout_s.min(30));
        loop {
            let pending = session.engine.management[0].read_memory(0, address, 1)?[0];
            if pending == 0 {
                break json!({"address":format!("{address:#010x}"),"pending":pending});
            }
            let elapsed_guest = inputs.panel.snapshot().guest_ms.wrapping_sub(start_guest);
            if elapsed_guest >= guest_timeout_ms || Instant::now() >= deadline {
                return Err(std::io::Error::other(format!(
                    "L6 saved pad {} assignment is still pending ({pending:#04x}) at {address:#010x}; refusing to press before the file assignment completes",
                    pad + 1
                ))
                .into());
            }
            wait_guest_duration(session, 100, deadline).map_err(std::io::Error::other)?;
        }
    } else {
        json!({"model":"l6max","source":format!("{assignment:?}")})
    };
    let trace_path = options.logs.join("engine.trace");
    let audio_prefix = "l6 audio TX samples sai=00384000";
    let trace_before = fs::read_to_string(&trace_path)?;
    let baseline_lines = trace_before
        .lines()
        .filter(|line| line.starts_with(audio_prefix))
        .count();
    let start_guest = inputs.panel.snapshot().guest_ms;
    let mut last_indicator_sample_ms = start_guest;
    let initial_panel = session.poll().map_err(std::io::Error::other)?;
    let mut indicator_samples = vec![json!({
        "elapsed_guest_ms": 0,
        "master_meter": master_meter_levels(model, initial_panel.indicator_levels),
        "indicator_levels": initial_panel.indicator_levels,
    })];
    let wall_deadline = Instant::now() + Duration::from_secs(wall_timeout_s);
    let guest_deadline_ms = guest_timeout_ms;
    let led_before = require_pad_led_change
        .then(|| sound_pad_led(model, inputs.panel.snapshot().indicators, pad));
    let applied_before = inputs.main.snapshot().applied;
    inputs
        .main
        .down(target, 500)
        .map_err(|error| std::io::Error::other(error.to_string()))?;
    let mut pad_led_changed = false;
    let observe_until_guest_ms = 400u32;
    loop {
        let data = session.poll().map_err(std::io::Error::other)?;
        let panel = inputs.panel.snapshot();
        if panel.guest_ms != last_indicator_sample_ms {
            last_indicator_sample_ms = panel.guest_ms;
            indicator_samples.push(json!({
                "elapsed_guest_ms": panel.guest_ms.wrapping_sub(start_guest),
                "master_meter": master_meter_levels(model, data.indicator_levels),
                "indicator_levels": data.indicator_levels,
            }));
        }
        let main = inputs.main.snapshot();
        if !main.connected {
            return Err(std::io::Error::other(format!(
                "MAIN input disconnected during pad press: {main:?}"
            ))
            .into());
        }
        if main.applied > applied_before {
            if let Some(led_before) = led_before {
                pad_led_changed |= sound_pad_led(model, data.indicators, pad) != led_before;
            }
            let elapsed = inputs.panel.snapshot().guest_ms.wrapping_sub(start_guest);
            if elapsed >= observe_until_guest_ms {
                break;
            }
        }
        let elapsed = inputs.panel.snapshot().guest_ms.wrapping_sub(start_guest);
        if Instant::now() >= wall_deadline || elapsed >= guest_deadline_ms {
            return Err(std::io::Error::other(
                "sound-pad press did not settle before its deadline",
            )
            .into());
        }
        thread::sleep(Duration::from_millis(5));
    }
    let released_before = inputs.main.snapshot().applied;
    inputs
        .main
        .up(target)
        .map_err(|error| std::io::Error::other(error.to_string()))?;
    loop {
        let data = session.poll().map_err(std::io::Error::other)?;
        let panel = inputs.panel.snapshot();
        if panel.guest_ms != last_indicator_sample_ms {
            last_indicator_sample_ms = panel.guest_ms;
            indicator_samples.push(json!({
                "elapsed_guest_ms": panel.guest_ms.wrapping_sub(start_guest),
                "master_meter": master_meter_levels(model, data.indicator_levels),
                "indicator_levels": data.indicator_levels,
            }));
        }
        let main = inputs.main.snapshot();
        if !main.connected {
            return Err(std::io::Error::other(format!(
                "MAIN input disconnected during pad playback: {main:?}"
            ))
            .into());
        }
        if let Some(led_before) = led_before {
            pad_led_changed |= sound_pad_led(model, data.indicators, pad) != led_before;
        }
        let elapsed = inputs.panel.snapshot().guest_ms.wrapping_sub(start_guest);
        if elapsed >= playback_window_ms && main.applied > released_before {
            break;
        }
        if Instant::now() >= wall_deadline || elapsed >= guest_deadline_ms {
            return Err(std::io::Error::other(format!(
                "sound-pad playback timed out at guest {elapsed} ms (limit {guest_deadline_ms} ms)"
            ))
            .into());
        }
        thread::sleep(Duration::from_millis(5));
    }
    let trace = fs::read_to_string(&trace_path)?;
    let samples = trace
        .lines()
        .filter(|line| line.starts_with(audio_prefix))
        .skip(baseline_lines)
        .filter_map(|line| {
            let field = |name: &str| {
                line.split_whitespace()
                    .find_map(|part| part.strip_prefix(&format!("{name}=")))?
                    .parse::<u64>()
                    .ok()
            };
            let at_ms = line
                .split_whitespace()
                .find_map(|part| part.strip_prefix("at="))?
                .strip_suffix("ms")?
                .parse::<u64>()
                .ok()?;
            Some(json!({
                "words": field("words")?,
                "nonzero": field("nonzero")?,
                "underruns": field("underruns")?,
                "first": line.split_whitespace().find_map(|part| part.strip_prefix("first="))?,
                "last": line.split_whitespace().find_map(|part| part.strip_prefix("last="))?,
                "at_ms": at_ms,
            }))
        })
        .collect::<Vec<_>>();
    let nonzero_audio = samples.iter().any(|sample| {
        let nonzero = sample["nonzero"].as_u64().unwrap_or(0);
        let words = sample["words"].as_u64().unwrap_or(0);
        let first = sample["first"].as_str().unwrap_or_default();
        let last = sample["last"].as_str().unwrap_or_default();
        nonzero > 0 && words > 0 && first != "ffffffff" && last != "ffffffff"
    });
    let isolated_levels = std::array::from_fn::<_, 2, _>(|side| {
        let mut seen = [false; 7];
        for sample in &indicator_samples {
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
    let waveform = expect_source
        .map(|source| {
            let capture = fs::read(capture_path.as_ref().unwrap())?;
            compare_pad_waveform(
                &options.logs.join("card.img"),
                source,
                &capture[capture_baseline.min(capture.len())..],
            )
        })
        .transpose()?;
    let evidence = json!({
        "waveform": waveform,
        "stereo_meter": {"required": require_stereo_meter, "left_only": left_only, "right_only": right_only, "isolated_levels": isolated_levels},
        "pad": pad + 1,
        "target": target,
        "assignment_source": format!("{assignment:?}"),
        "assignment_ready": assignment_ready,
        "led": if model == l6fw::Model::L6 {
            usize::from(pad)
        } else {
            27 + usize::from(pad)
        },
        "pad_led_check_enabled": require_pad_led_change,
        "pad_led_changed_from_baseline": pad_led_changed,
        "indicator_samples": indicator_samples,
        "audio_samples": samples,
        "nonzero_audio_after_press": nonzero_audio,
        "guest_window_ms": inputs.panel.snapshot().guest_ms.wrapping_sub(start_guest),
    });
    fs::write(
        options.logs.join("sound-pad-playback.json"),
        serde_json::to_vec_pretty(&evidence)?,
    )?;
    if let Some(waveform) = &waveform {
        let residual = waveform["residual_db"].as_f64().unwrap_or(f64::INFINITY);
        if residual > max_residual_db {
            return Err(std::io::Error::other(format!(
                "sound-pad waveform residual {residual:.1} dB exceeds {max_residual_db:.1} dB: {waveform}"
            ))
            .into());
        }
    }
    if !nonzero_audio
        || (require_pad_led_change && !pad_led_changed)
        || (require_stereo_meter && !(left_only && right_only))
    {
        return Err(std::io::Error::other(format!(
            "sound-pad playback assertion failed: audio={nonzero_audio}, pad LED changed={pad_led_changed}, pad LED required={require_pad_led_change}, stereo meter required={require_stereo_meter}, left only={left_only}, right only={right_only}; details saved to sound-pad-playback.json"
        )).into());
    }
    Ok(evidence)
}

pub(super) fn compare_pad_waveform(
    card: &std::path::Path,
    source: &str,
    capture: &[u8],
) -> Result<Value, Box<dyn std::error::Error>> {
    let wav = sd_image::read_file(card, source, u64::MAX)?;
    let source = waveform::decode_wav(&wav).map_err(std::io::Error::other)?;
    waveform::compare(&source, &waveform::decode_capture(capture))
        .map_err(|error| std::io::Error::other(error).into())
}

pub(super) fn master_meter_levels(model: l6fw::Model, levels: [[u8; 24]; 8]) -> [u8; 2] {
    match model {
        l6fw::Model::L6 => l6max_host::indicators::l6_master_meter_levels(levels),
        l6fw::Model::L6max => l6max_host::indicators::l6max_master_meter_levels(levels),
    }
}

pub(super) fn run_l6_recorder_playback(
    session: &Session,
    options: &Options,
    target: u32,
    guest_timeout_ms: u32,
    wall_timeout_s: u64,
) -> Result<Value, Box<dyn std::error::Error>> {
    if session.engine.model() != l6fw::Model::L6 || target != 4 {
        return Err(std::io::Error::other(
            "l6-recorder-playback requires L6 firmware and MAIN target 4",
        )
        .into());
    }
    if std::env::var_os("L6_AUDIO_SAMPLE_TRACE").is_none() {
        return Err(std::io::Error::other(
            "l6-recorder-playback requires L6_AUDIO_SAMPLE_TRACE=1 in the case environment",
        )
        .into());
    }
    const PREFIX: &str = "l6 audio TX samples sai=00384000";
    const POST_PRESS_SETTLE_MS: u32 = 5000;
    let total_guest_ms = POST_PRESS_SETTLE_MS;
    if guest_timeout_ms < total_guest_ms {
        return Err(std::io::Error::other(
            "l6-recorder-playback guest timeout must be at least 5000 ms",
        )
        .into());
    }
    let inputs = session
        .engine
        .inputs
        .as_ref()
        .ok_or_else(|| std::io::Error::other("l6-recorder-playback has no panel inputs"))?;
    let trace_path = options.logs.join("engine.trace");
    let trace_before = fs::read_to_string(&trace_path)?;
    let baseline_samples = trace_before
        .lines()
        .filter(|line| line.starts_with(PREFIX))
        .collect::<Vec<_>>();
    let baseline_signature = baseline_samples
        .last()
        .and_then(|line| tx_sample_signature(line));
    let qmp = &session.engine.management[0];
    let gdb_socket = options
        .gdb_socket
        .as_ref()
        .ok_or_else(|| std::io::Error::other("l6-recorder-playback requires GDB"))?;
    let mut debugger = l6max_diagnostics::gdb::Gdb::connect(gdb_socket).map_err(|error| {
        std::io::Error::other(format!("connecting recorder GDB socket: {error}"))
    })?;
    qmp.stop().map_err(|error| {
        std::io::Error::other(format!(
            "stopping QEMU before recorder breakpoint setup: {error}"
        ))
    })?;
    debugger
        .breakpoint(l6::system_event_dispatcher, true)
        .map_err(|error| {
            std::io::Error::other(format!(
                "setting L6 system-event dispatcher breakpoint: {error}"
            ))
        })?;
    debugger
        .breakpoint(l6::rec_play_handler, true)
        .map_err(|error| {
            std::io::Error::other(format!("setting L6 Play handler breakpoint: {error}"))
        })?;
    debugger
        .breakpoint(l6::rec_playback_callback, true)
        .map_err(|error| {
            std::io::Error::other(format!("setting L6 recorder callback breakpoint: {error}"))
        })?;
    let start_guest_ms = inputs.panel.snapshot().guest_ms;
    let before_applied = inputs.main.snapshot().applied;
    inputs
        .main
        .down(target, 10_000)
        .map_err(|error| std::io::Error::other(error.to_string()))?;
    let wall_deadline = Instant::now() + Duration::from_secs(wall_timeout_s);
    loop {
        session.poll().map_err(std::io::Error::other)?;
        let main = inputs.main.snapshot();
        if !main.connected {
            return Err(std::io::Error::other(format!(
                "MAIN input disconnected during recorder Play: {main:?}"
            ))
            .into());
        }
        if main.applied > before_applied {
            break;
        }
        if Instant::now() >= wall_deadline {
            return Err(
                std::io::Error::other("L6 recorder Play press was not acknowledged").into(),
            );
        }
        thread::sleep(Duration::from_millis(5));
    }
    debugger
        .continue_until_stop(Duration::from_secs(10))
        .map_err(|error| {
            std::io::Error::other(format!("waiting for L6 system-event dispatcher: {error}"))
        })?;
    let dispatcher_pc = debugger.read_register(15).map_err(std::io::Error::other)? & !1;
    let event_pointer = debugger.read_register(0).map_err(std::io::Error::other)?;
    let event = qmp
        .read_memory(0, event_pointer, 20)
        .map_err(|error| std::io::Error::other(format!("reading dispatched L6 event: {error}")))?;
    let event_state = u32::from_le_bytes(event[0..4].try_into().unwrap());
    let event_class = u32::from_le_bytes(event[4..8].try_into().unwrap());
    let event_code = u32::from_le_bytes(event[8..12].try_into().unwrap());
    if dispatcher_pc != l6::system_event_dispatcher {
        return Err(std::io::Error::other(format!(
            "L6 Play event did not reach class-0 dispatcher (pc={dispatcher_pc:#010x}, event={event:02x?})"
        )).into());
    }
    if event_state != 0 || event_class != 0 || event_code != 0x20 {
        return Err(std::io::Error::other(format!(
            "L6 class-0 dispatcher received an unexpected event (state={event_state}, class={event_class}, code={event_code:#x}, bytes={event:02x?})"
        )).into());
    }
    // A breakpoint left at the current PC would stop here again.
    debugger
        .breakpoint(l6::system_event_dispatcher, false)
        .map_err(|error| std::io::Error::other(format!("clearing dispatcher: {error}")))?;
    debugger
        .continue_until_stop(Duration::from_secs(10))
        .map_err(|error| std::io::Error::other(format!("waiting for L6 Play handler: {error}")))?;
    let handler_pc = debugger.read_register(15).map_err(std::io::Error::other)? & !1;
    let handler_argument = debugger.read_register(0).map_err(std::io::Error::other)?;
    let mode = u32::from_le_bytes(
        qmp.read_memory(0, l6::rec_mode, 4)
            .map_err(|error| std::io::Error::other(format!("reading L6 recorder mode: {error}")))?
            .try_into()
            .unwrap(),
    );
    let selection_key = qmp
        .read_memory(0, l6::rec_selected_key, 8)
        .map_err(|error| {
            std::io::Error::other(format!("reading selected L6 recording key: {error}"))
        })?;
    if handler_pc != l6::rec_play_handler
        || handler_argument != 1
        || mode != 0
        || selection_key.iter().all(|byte| *byte == 0)
    {
        return Err(std::io::Error::other(format!(
            "L6 Play handler did not accept the selected mode-0 recording (pc={handler_pc:#010x}, argument={handler_argument}, mode={mode}, selection={selection_key:02x?})"
        ))
        .into());
    }
    debugger
        .breakpoint(l6::rec_play_handler, false)
        .map_err(|error| std::io::Error::other(format!("clearing Play handler: {error}")))?;
    debugger
        .continue_until_stop(Duration::from_secs(10))
        .map_err(|error| {
            std::io::Error::other(format!("waiting for L6 recorder callback: {error}"))
        })?;
    let callback_pc = debugger.read_register(15).map_err(std::io::Error::other)? & !1;
    if callback_pc != l6::rec_playback_callback {
        return Err(std::io::Error::other(format!(
            "L6 recorder playback callback stopped at {callback_pc:#010x}, expected {:#010x}",
            l6::rec_playback_callback
        ))
        .into());
    }
    debugger
        .breakpoint(l6::rec_playback_callback, false)
        .map_err(std::io::Error::other)?;
    qmp.resume().map_err(|error| {
        std::io::Error::other(format!("resuming QEMU after L6 recorder callback: {error}"))
    })?;
    wait_guest_duration(session, POST_PRESS_SETTLE_MS, wall_deadline)
        .map_err(std::io::Error::other)?;
    let release_before = inputs.main.snapshot().applied;
    inputs
        .main
        .up(target)
        .map_err(|error| std::io::Error::other(error.to_string()))?;
    loop {
        session.poll().map_err(std::io::Error::other)?;
        let main = inputs.main.snapshot();
        if main.applied > release_before {
            break;
        }
        if Instant::now() >= wall_deadline {
            return Err(
                std::io::Error::other("L6 recorder Play release was not acknowledged").into(),
            );
        }
        thread::sleep(Duration::from_millis(5));
    }
    qmp.stop().map_err(|error| {
        std::io::Error::other(format!("stopping QEMU for L6 SAI DMA inspection: {error}"))
    })?;
    let tcd = qmp.read_memory(0, 0x400e_91c0, 32).map_err(|error| {
        std::io::Error::other(format!("reading L6 SAI channel-14 TCD: {error}"))
    })?;
    let source = u32::from_le_bytes(tcd[0..4].try_into().unwrap());
    let source_data = qmp.read_memory(0, source, 16).map_err(|error| {
        std::io::Error::other(format!("reading L6 SAI channel-14 DMA source: {error}"))
    })?;
    let source_words = source_data
        .chunks_exact(4)
        .map(|word| u32::from_le_bytes(word.try_into().unwrap()))
        .collect::<Vec<_>>();
    let source_has_audio = source_words
        .iter()
        .any(|word| *word != 0 && *word != u32::MAX);
    let trace = fs::read_to_string(&trace_path)?;
    let samples = trace
        .lines()
        .filter(|line| line.starts_with(PREFIX))
        .skip(baseline_samples.len())
        .filter_map(|line| {
            Some(json!({
                "at_ms": trace_u64_field(line, "at")?,
                "first": trace_hex_field(line, "first")?,
                "last": trace_hex_field(line, "last")?,
            }))
        })
        .collect::<Vec<_>>();
    let tx_changed = samples.iter().any(|sample| {
        let signature = (sample["first"].as_str(), sample["last"].as_str());
        baseline_signature.as_ref().is_some_and(|baseline| {
            signature != (Some(baseline.0.as_str()), Some(baseline.1.as_str()))
        })
    });
    let evidence = json!({
        "target": target,
        "guest_window_ms": inputs.panel.snapshot().guest_ms.wrapping_sub(start_guest_ms),
        "dispatched_event_pointer": event_pointer,
        "dispatched_event": event,
        "handler_argument": handler_argument,
        "recorder_mode": mode,
        "selection_key": selection_key,
        "callback_pc": callback_pc,
        "post_press_settle_ms": POST_PRESS_SETTLE_MS,
        "baseline_signature": baseline_signature,
        "samples": samples,
        "tx_sample_signature_changed_after_press": tx_changed,
        "channel_14_tcd_source": source,
        "channel_14_source_words": source_words,
        "channel_14_source_has_audio": source_has_audio,
    });
    fs::write(
        options.logs.join("recorder-playback.json"),
        serde_json::to_vec_pretty(&evidence)?,
    )?;
    if !tx_changed || !source_has_audio {
        return Err(std::io::Error::other(format!(
            "L6 recorder playback audio assertion failed (TX changed={tx_changed}, channel-14 source has audio={source_has_audio}): {evidence}"
        ))
        .into());
    }
    Ok(evidence)
}

pub(super) fn tx_sample_signature(line: &str) -> Option<(String, String)> {
    Some((
        trace_hex_field(line, "first")?,
        trace_hex_field(line, "last")?,
    ))
}

pub(super) fn trace_hex_field(line: &str, field: &str) -> Option<String> {
    let value = line
        .split_whitespace()
        .find_map(|part| part.strip_prefix(&format!("{field}=")))?;
    Some(value.to_ascii_lowercase())
}

pub(super) fn trace_u64_field(line: &str, field: &str) -> Option<u64> {
    let value = line
        .split_whitespace()
        .find_map(|part| part.strip_prefix(&format!("{field}=")))?;
    value
        .strip_suffix("ms")
        .unwrap_or(value)
        .parse::<u64>()
        .ok()
}

pub(super) fn sound_pad_led(model: l6fw::Model, indicators: [u32; 8], pad: u8) -> bool {
    match model {
        l6fw::Model::L6 => l6max_host::indicators::l6_sound_pad_leds(indicators)[pad as usize],
        l6fw::Model::L6max => l6max_host::indicators::leds(indicators)[27 + pad as usize],
    }
}

impl State {
    pub(super) fn run_l6max_recorder_playback(
        &mut self,
        session: &Session,
        options: &Options,
        target: u32,
        guest_timeout_ms: u32,
        wall_timeout_s: u64,
    ) -> Result<Value, Box<dyn std::error::Error>> {
        if session.engine.model() != l6fw::Model::L6max || target != 4 {
            return Err(std::io::Error::other(
                "l6max-recorder-playback requires L6max firmware and MAIN target 4",
            )
            .into());
        }
        if std::env::var_os("L6_AUDIO_SAMPLE_TRACE").is_none() {
            return Err(std::io::Error::other(
                "l6max-recorder-playback requires L6_AUDIO_SAMPLE_TRACE=1",
            )
            .into());
        }
        if guest_timeout_ms < 10_000 {
            return Err(std::io::Error::other(
                "l6max-recorder-playback guest timeout must be at least 10000 ms",
            )
            .into());
        }
        let inputs = session
            .engine
            .inputs
            .as_ref()
            .ok_or("L6max recorder playback has no MAIN input")?;
        let qmp = &session.engine.management[0];
        let socket = options
            .gdb_socket
            .as_ref()
            .ok_or("l6max-recorder-playback requires GDB")?;
        let mut debugger =
            l6max_diagnostics::gdb::Gdb::connect(socket).map_err(std::io::Error::other)?;
        let trace_path = options.logs.join("engine.trace");
        const PREFIX: &str = "l6 audio TX samples sai=00384000";
        let baseline = fs::read_to_string(&trace_path)?;
        let baseline_lines = baseline
            .lines()
            .filter(|line| line.starts_with(PREFIX))
            .collect::<Vec<_>>();
        let baseline_signature = baseline_lines
            .last()
            .and_then(|line| tx_sample_signature(line));

        qmp.stop()?;
        use firmware_api::l6max::play_event_submit;
        debugger.breakpoint(play_event_submit, true)?;
        let before_applied = inputs.main.snapshot().applied;
        inputs.main.down(target, 10_000)?;
        let input_deadline = Instant::now() + Duration::from_secs(wall_timeout_s.min(30));
        loop {
            session.poll().map_err(std::io::Error::other)?;
            let status = inputs.main.snapshot();
            if !status.connected {
                return Err(std::io::Error::other(format!(
                    "MAIN input disconnected during L6max Play: {status:?}"
                ))
                .into());
            }
            if status.applied > before_applied {
                break;
            }
            if Instant::now() >= input_deadline {
                return Err(std::io::Error::other("L6max Play press was not acknowledged").into());
            }
            thread::sleep(Duration::from_millis(5));
        }
        debugger.continue_until_stop(Duration::from_secs(10))?;
        let submit_pc = debugger.read_register(15)? & !1;
        let submit_args = (0..4)
            .map(|register| debugger.read_register(register))
            .collect::<Result<Vec<_>, _>>()?;
        if submit_pc != play_event_submit || submit_args.get(2) != Some(&0x2c) {
            return Err(std::io::Error::other(format!(
                "L6max target 4 did not submit Play event 0x2c at {play_event_submit:#010x}: pc={submit_pc:#010x}, args={submit_args:?}"
            ))
            .into());
        }
        debugger.breakpoint(play_event_submit, false)?;
        use firmware_api::l6max::play_event_send_return;
        debugger.breakpoint(play_event_send_return, true)?;
        debugger.continue_until_stop(Duration::from_secs(5))?;
        let send_pc = debugger.read_register(15)? & !1;
        let send_result = debugger.read_register(0)?;
        if send_pc != play_event_send_return || send_result != 0 {
            return Err(std::io::Error::other(format!(
                "L6max Play queue submission failed: pc={send_pc:#010x}, result={send_result:#010x}"
            ))
            .into());
        }
        debugger.breakpoint(play_event_send_return, false)?;

        use firmware_api::l6max::panel_handle_main_button_event;
        debugger.breakpoint(panel_handle_main_button_event, true)?;
        debugger.continue_until_stop(Duration::from_secs(10))?;
        let handler_pc = debugger.read_register(15)? & !1;
        let event_pointer = debugger.read_register(0)?;
        if handler_pc != panel_handle_main_button_event {
            return Err(std::io::Error::other(format!(
                "L6max Play dispatch stopped at {handler_pc:#010x}, expected {panel_handle_main_button_event:#010x}"
            ))
            .into());
        }
        let event = qmp.read_memory(0, event_pointer, 20)?;
        let event_fields = event
            .chunks_exact(4)
            .map(|word| u32::from_le_bytes(word.try_into().unwrap()))
            .collect::<Vec<_>>();
        if event_fields.first() != Some(&0) || event_fields.get(2) != Some(&0x2c) {
            return Err(std::io::Error::other(format!(
                "L6max main-button handler received unexpected event {event_fields:08x?}"
            ))
            .into());
        }
        debugger.breakpoint(panel_handle_main_button_event, false)?;

        use firmware_api::l6max::gui_callback_schedule;
        debugger.breakpoint(gui_callback_schedule, true)?;
        let callback_deadline = Instant::now() + Duration::from_secs(10);
        let callback_target = loop {
            if Instant::now() >= callback_deadline {
                return Err(std::io::Error::other(
                    "L6max Play handler did not schedule callback slot 2",
                )
                .into());
            }
            debugger
                .continue_until_stop(callback_deadline.saturating_duration_since(Instant::now()))?;
            let pc = debugger.read_register(15)? & !1;
            if pc != gui_callback_schedule {
                return Err(std::io::Error::other(format!(
                    "L6max callback scheduler stopped at {pc:#010x}"
                ))
                .into());
            }
            let callback = debugger.read_register(0)?;
            let caller = debugger.read_register(14)? & !1;
            if callback == 2
                && (l6max::panel_handle_main_button_event
                    ..l6max::panel_handle_main_button_event_end)
                    .contains(&caller)
            {
                let window = u32::from_le_bytes(
                    qmp.read_memory(0, l6max::current_window, 4)?
                        .try_into()
                        .unwrap(),
                );
                let table =
                    u32::from_le_bytes(qmp.read_memory(0, window + 24, 4)?.try_into().unwrap());
                let target =
                    u32::from_le_bytes(qmp.read_memory(0, table + 8, 4)?.try_into().unwrap()) & !1;
                if !(0x8000_0000..0x8100_0000).contains(&target) {
                    return Err(std::io::Error::other(format!(
                        "L6max Play callback target is invalid: {target:#010x}"
                    ))
                    .into());
                }
                break target;
            }
            debugger.breakpoint(gui_callback_schedule, false)?;
            debugger.request("s")?;
            debugger.breakpoint(gui_callback_schedule, true)?;
        };
        debugger.breakpoint(gui_callback_schedule, false)?;
        debugger.breakpoint(callback_target, true)?;
        debugger.continue_until_stop(Duration::from_secs(5))?;
        let callback_pc = debugger.read_register(15)? & !1;
        if callback_pc != callback_target {
            return Err(std::io::Error::other(format!(
                "L6max Play callback stopped at {callback_pc:#010x}, expected {callback_target:#010x}"
            ))
            .into());
        }
        debugger.breakpoint(callback_target, false)?;
        use firmware_api::l6max::rec_play_action;
        debugger.breakpoint(rec_play_action, true)?;
        debugger.continue_until_stop(Duration::from_secs(5))?;
        let action_pc = debugger.read_register(15)? & !1;
        if action_pc != rec_play_action {
            return Err(std::io::Error::other(format!(
                "L6max Play callback did not reach recorder action: stopped at {action_pc:#010x}"
            ))
            .into());
        }
        debugger.breakpoint(rec_play_action, false)?;
        use firmware_api::l6max::rec_stream_read;
        debugger.breakpoint(rec_stream_read, true)?;
        let stream_read_hit = match debugger.continue_until_stop(Duration::from_secs(10)) {
            Ok(_) => {
                let pc = debugger.read_register(15)? & !1;
                if pc != rec_stream_read {
                    return Err(std::io::Error::other(format!(
                        "L6max recorder stream-read probe stopped at {pc:#010x}, expected {rec_stream_read:#010x}"
                    ))
                    .into());
                }
                true
            }
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                debugger.interrupt()?;
                false
            }
            Err(error) => return Err(error.into()),
        };
        debugger.breakpoint(rec_stream_read, false)?;
        qmp.resume()?;

        let sample_deadline = Instant::now() + Duration::from_secs(wall_timeout_s);
        wait_guest_duration(session, 10_000, sample_deadline).map_err(std::io::Error::other)?;
        let after = fs::read_to_string(&trace_path)?;
        let samples = after
            .lines()
            .filter(|line| line.starts_with(PREFIX))
            .skip(baseline_lines.len())
            .collect::<Vec<_>>();
        let nonzero_samples = samples
            .iter()
            .filter(|line| trace_u64_field(line, "nonzero").unwrap_or(0) > 0)
            .count();
        let tx_changed = samples.iter().any(|line| {
            let signature = tx_sample_signature(line);
            signature.is_some() && signature != baseline_signature
        });
        qmp.stop()?;
        let tcd = qmp.read_memory(0, 0x400e_91c0, 32)?;
        let source = u32::from_le_bytes(tcd[0..4].try_into().unwrap());
        let source_words = if (0x2000_0000..0x2008_0000).contains(&source) {
            qmp.read_memory(0, source, 16)?
                .chunks_exact(4)
                .map(|word| u32::from_le_bytes(word.try_into().unwrap()))
                .collect::<Vec<_>>()
        } else {
            vec![]
        };
        let source_has_audio = source_words
            .iter()
            .any(|word| *word != 0 && *word != u32::MAX);
        let evidence = json!({
            "model":"l6max", "target":target, "event":0x2c,
            "event_fields":event_fields, "callback_target":callback_target,
            "recorder_action":action_pc, "stream_read_reached":stream_read_hit,
            "guest_ms":inputs.panel.snapshot().guest_ms,
            "tx_sample_windows":samples.len(), "nonzero_sample_windows":nonzero_samples,
            "tx_signature_changed":tx_changed, "channel14_source":source,
            "channel14_source_words":source_words, "source_has_audio":source_has_audio,
        });
        fs::write(
            options.logs.join("l6max-recorder-playback.json"),
            serde_json::to_vec_pretty(&evidence)?,
        )?;
        if !stream_read_hit || nonzero_samples == 0 || !tx_changed || !source_has_audio {
            return Err(std::io::Error::other(format!(
                "L6max recorder playback entered the firmware action but produced no verified audio: {evidence}"
            ))
            .into());
        }
        Ok(evidence)
    }
}
