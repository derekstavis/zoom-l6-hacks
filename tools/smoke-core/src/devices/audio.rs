//! Audio callbacks, sound pads and recorder playback.
use super::*;
pub(super) mod callbacks;
pub(super) mod max_recorder;
pub(super) mod recorder;
pub(super) mod sound_pad;

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
    if session.model() != l6fw::Model::L6 {
        return Err("saved pad-file readiness requires L6".into());
    }
    let start = guest_now(session);
    let deadline = Instant::now() + Duration::from_secs(wall_timeout_s.min(WALL_TIMEOUT_S));
    let control = session.control();
    let reader = control.sound_pad_reader_before(deadline)?;
    loop {
        session.poll()?;
        let sample = reader.sample_before(0, Some(path), deadline)?;
        let pending = sample.pending;
        if sample.path_matches == Some(true) && pending == 0 {
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

pub(super) fn compare_pad_waveform(
    card: &std::path::Path,
    source: &str,
    capture: &[u8],
) -> Result<Value, Box<dyn std::error::Error>> {
    let wav = sd_image::read_file(card, source, u64::MAX)?;
    let source = waveform::decode_wav(&wav).map_err(std::io::Error::other)?;
    let comparison = waveform::compare(&source, &waveform::decode_capture(capture))
        .map_err(std::io::Error::other)?;
    Ok(serde_json::to_value(comparison)?)
}

pub(super) fn master_meter_levels(model: l6fw::Model, levels: [[u8; 24]; 8]) -> [u8; 2] {
    match model {
        l6fw::Model::L6 => l6max_host::indicators::l6_master_meter_levels(levels),
        l6fw::Model::L6max => l6max_host::indicators::l6max_master_meter_levels(levels),
    }
}

pub(super) fn sound_pad_led(model: l6fw::Model, indicators: [u32; 8], pad: u8) -> bool {
    match model {
        l6fw::Model::L6 => l6max_host::indicators::l6_sound_pad_leds(indicators)[pad as usize],
        l6fw::Model::L6max => l6max_host::indicators::leds(indicators)[27 + pad as usize],
    }
}
