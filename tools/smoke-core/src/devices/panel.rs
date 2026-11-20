//! Panel inputs and outputs: PWM levels, key maps, input sweeps and timing.
use super::*;

pub(super) fn run_l6max_channel_signal_pwm(
    session: &Session,
    options: &Options,
    guest_timeout_ms: u32,
    wall_timeout_s: u64,
) -> Result<Value, String> {
    use l6max_diagnostics::gdb::Gdb;
    if session.engine.model() != l6fw::Model::L6max {
        return Err("l6max-channel-signal-pwm requires L6max firmware".into());
    }
    let socket = options.gdb_socket.as_ref().ok_or("GDB is required")?;
    let qmp = &session.engine.management[0];
    let mut debugger = Gdb::connect(socket).map_err(|error| error.to_string())?;
    let inputs = session
        .engine
        .inputs
        .as_ref()
        .ok_or("headless panel input missing")?;
    // SIGNAL 1-4 and 5-8 respond on separate scales, each in five brightness
    // steps. Each stimulus sits well inside one observed step, away from the
    // levels where a lamp changes step.
    let sweeps: [(
        &str,
        std::ops::Range<usize>,
        std::ops::Range<usize>,
        [f32; 5],
    ); 2] = [
        (
            "signal-1-to-4",
            0..4,
            0..4,
            [
                1_600_000.0,
                2_250_000.0,
                3_200_000.0,
                4_550_000.0,
                8_000_000.0,
            ],
        ),
        (
            "signal-5-to-8",
            4..12,
            4..8,
            [
                9_000_000.0,
                12_800_000.0,
                18_000_000.0,
                25_500_000.0,
                40_000_000.0,
            ],
        ),
    ];
    let started_guest = inputs.panel.snapshot().guest_ms;
    let deadline = Instant::now() + Duration::from_secs(wall_timeout_s);
    let mut sweep_evidence = Vec::new();
    let mut failures = Vec::new();

    // The SIGNAL lamps start following these values a second or two after
    // the mixer is ready. Hold the first band high until SIGNAL 1 lights, so
    // the sweep cannot start before the lamps respond.
    let mut warm_up = Vec::with_capacity(12 * 4);
    for channel in 0..12 {
        let value: f32 = if channel < 4 { 8_000_000.0 } else { 0.0 };
        warm_up.extend_from_slice(&value.to_le_bytes());
    }
    let warm_up_start = inputs.panel.snapshot().guest_ms;
    let mut last_seed = None;
    loop {
        let panel = session.poll().map_err(|error| error.to_string())?;
        let guest_ms = inputs.panel.snapshot().guest_ms;
        let (row, column) = l6max_host::indicators::LED_PINS[52];
        if panel.indicator_levels[row][(column ^ 7) as usize] != 0 {
            break;
        }
        if guest_ms.wrapping_sub(warm_up_start) >= 10_000 || Instant::now() >= deadline {
            return Err("SIGNAL 1 did not light within 10 guest seconds".into());
        }
        if last_seed.is_none_or(|seed: u32| guest_ms.wrapping_sub(seed) >= 40) {
            qmp.stop().map_err(|error| error.to_string())?;
            debugger
                .write(0x2001_50e0, &warm_up)
                .map_err(|error| error.to_string())?;
            qmp.resume().map_err(|error| error.to_string())?;
            last_seed = Some(guest_ms);
        }
        thread::sleep(Duration::from_millis(1));
    }

    for (name, accumulator_range, output_range, levels) in sweeps {
        {
            // A lit band takes a moment to fall after its values drop. Drain
            // the first band, lit by the warm-up or the previous sweep, before
            // measuring, so samples cannot contain earlier LEDs.
            let zeroes = vec![0_u8; 12 * 4];
            qmp.stop().map_err(|error| error.to_string())?;
            debugger
                .write(0x2001_50e0, &zeroes)
                .map_err(|error| error.to_string())?;
            qmp.resume().map_err(|error| error.to_string())?;
            let drain_start = inputs.panel.snapshot().guest_ms;
            let mut last_seed = drain_start;
            let mut zero_samples = 0_u8;
            loop {
                let panel = session.poll().map_err(|error| error.to_string())?;
                let guest_ms = inputs.panel.snapshot().guest_ms;
                let prior_band_off = (0..4).all(|channel| {
                    let (row, column) = l6max_host::indicators::LED_PINS[52 + channel];
                    panel.indicator_levels[row][(column ^ 7) as usize] == 0
                });
                zero_samples = if prior_band_off { zero_samples + 1 } else { 0 };
                if zero_samples >= 3 {
                    break;
                }
                let elapsed = guest_ms.wrapping_sub(drain_start);
                if elapsed >= 1_000 {
                    return Err(format!(
                        "previous L6max SIGNAL band did not clear within {elapsed} guest ms"
                    ));
                }
                if guest_ms.wrapping_sub(last_seed) >= 40 {
                    qmp.stop().map_err(|error| error.to_string())?;
                    debugger
                        .write(0x2001_50e0, &zeroes)
                        .map_err(|error| error.to_string())?;
                    qmp.resume().map_err(|error| error.to_string())?;
                    last_seed = guest_ms;
                }
                thread::sleep(Duration::from_millis(1));
            }
        }
        let mut stage_evidence = Vec::new();
        for value in levels {
            let mut bytes = Vec::with_capacity(12 * 4);
            for channel in 0..12 {
                let channel_value = if accumulator_range.contains(&channel) {
                    value
                } else {
                    0.0
                };
                bytes.extend_from_slice(&channel_value.to_le_bytes());
            }
            let mut seed = || -> Result<(), String> {
                qmp.stop().map_err(|error| error.to_string())?;
                debugger
                    .write(0x2001_50e0, &bytes)
                    .map_err(|error| error.to_string())?;
                qmp.resume().map_err(|error| error.to_string())?;
                Ok(())
            };
            seed()?;
            let stage_start = inputs.panel.snapshot().guest_ms;
            let mut last_seed = stage_start;
            let mut samples: Vec<[u8; 8]> = Vec::new();
            loop {
                let panel = session.poll().map_err(|error| error.to_string())?;
                let guest_ms = inputs.panel.snapshot().guest_ms;
                if guest_ms.wrapping_sub(last_seed) >= 40 {
                    seed()?;
                    last_seed = guest_ms;
                }
                let sample = std::array::from_fn(|channel| {
                    let (row, column) = l6max_host::indicators::LED_PINS[52 + channel];
                    panel.indicator_levels[row][(column ^ 7) as usize]
                });
                samples.push(sample);
                if guest_ms.wrapping_sub(stage_start) >= 1_000 {
                    break;
                }
                if Instant::now() >= deadline
                    || guest_ms.wrapping_sub(started_guest) > guest_timeout_ms
                {
                    return Err(format!("timed out sampling {name} PWM"));
                }
                thread::sleep(Duration::from_millis(5));
            }
            let mean: [f64; 8] = std::array::from_fn(|channel| {
                samples
                    .iter()
                    .map(|sample| sample[channel] as u64)
                    .sum::<u64>() as f64
                    / samples.len() as f64
            });
            stage_evidence.push(json!({"input_level":value,"sample_count":samples.len(),"signal_led_mean_levels":mean,"samples":samples}));
        }
        let medians = stage_evidence
            .iter()
            .map(|entry| {
                entry["signal_led_mean_levels"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|value| value.as_f64().unwrap())
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        for channel in output_range.clone() {
            for stage in 0..medians.len() {
                if medians[stage][channel] == 0.0
                    || (stage > 0 && medians[stage][channel] <= medians[stage - 1][channel])
                {
                    let details = medians.iter().map(|row| row[channel]).collect::<Vec<_>>();
                    failures.push(format!("SIGNAL LED {} did not produce five distinct increasing sampled PWM levels in {name}: {details:?}", channel + 1));
                }
            }
        }
        sweep_evidence.push(json!({"group":name,"accumulator_indices":accumulator_range,"output_channels":output_range,"levels":stage_evidence}));
    }
    let evidence =
        json!({"stimulus":"firmware_shared_channel_level_accumulators","sweeps":sweep_evidence});
    fs::write(
        options.logs.join("channel-signal-pwm.json"),
        serde_json::to_vec_pretty(&evidence).map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;
    if !failures.is_empty() {
        return Err(failures.join("; "));
    }
    Ok(evidence)
}

pub(super) fn run_l6_level_meter_pwm(
    session: &Session,
    options: &Options,
    guest_timeout_ms: u32,
    wall_timeout_s: u64,
) -> Result<Value, String> {
    use l6max_diagnostics::gdb::Gdb;
    if session.engine.model() != l6fw::Model::L6 {
        return Err("l6-level-meter-pwm requires L6 firmware".into());
    }
    let socket = options.gdb_socket.as_ref().ok_or("GDB is required")?;
    let qmp = &session.engine.management[0];
    let mut debugger = Gdb::connect(socket).map_err(|error| error.to_string())?;
    // QEMU can report a paused VM after the GDB client attaches even though
    // the scenario was running. Resume before measuring guest-time progress.
    if !qmp
        .query_status()
        .map_err(|error| error.to_string())?
        .running
    {
        qmp.resume().map_err(|error| error.to_string())?;
    }
    let inputs = session
        .engine
        .inputs
        .as_ref()
        .ok_or("headless panel input missing")?;

    // Skip the long boot-time panel output sequence; the PWM probe exercises
    // the task's normal meter loop and panel updates.
    use firmware_api::l6::level_meter_active_gates;
    use firmware_api::l6::level_meter_converted_accumulators;
    use firmware_api::l6::level_meter_task;
    use firmware_api::l6::level_meter_task_phase;
    use firmware_api::l6::level_meter_task_startup_call;
    use firmware_api::l6::level_meter_threshold_builder;
    const THRESHOLD_STAGES: [u32; 6] = [0, 1_500_000, 2_000_000, 3_000_000, 4_000_000, 6_000_000];
    const STAGE_WINDOW_MS: u32 = 500;
    const SETTLE_MS: u32 = 100;

    let started_guest = inputs.panel.snapshot().guest_ms;
    let deadline = Instant::now() + Duration::from_secs(wall_timeout_s);
    let initial_gates = qmp
        .read_memory(0, level_meter_active_gates, 3)
        .map_err(|error| error.to_string())?;
    let initial_phase = u32::from_le_bytes(
        qmp.read_memory(0, level_meter_task_phase, 4)
            .map_err(|error| error.to_string())?
            .try_into()
            .unwrap(),
    );
    qmp.stop().map_err(|error| error.to_string())?;
    debugger
        .breakpoint(level_meter_task, true)
        .map_err(|error| format!("setting LevelMeterLed task-entry breakpoint: {error}"))?;
    if let Err(error) = debugger.continue_until_stop(Duration::from_secs(wall_timeout_s.min(30))) {
        let status = qmp
            .query_status()
            .map(|status| status.status)
            .unwrap_or_else(|read| format!("unavailable: {read}"));
        qmp.resume().map_err(|resume| resume.to_string())?;
        return Err(format!(
            "LevelMeterLed task entry {level_meter_task:#010x} was not reached within {}s (phase {initial_phase}, gates {initial_gates:02x?}, QEMU {status}): {error}; PWM probe was not run",
            wall_timeout_s.min(30)
        ));
    }
    let entry_pc = debugger
        .read_register(15)
        .map_err(|error| error.to_string())?
        & !1;
    if entry_pc != level_meter_task {
        qmp.resume().map_err(|error| error.to_string())?;
        return Err(format!(
            "LevelMeterLed entry breakpoint stopped at {entry_pc:#010x}, expected {level_meter_task:#010x}"
        ));
    }
    debugger
        .breakpoint(level_meter_task, false)
        .map_err(|error| error.to_string())?;
    qmp.stop().map_err(|error| error.to_string())?;
    const NOP_PAIR: [u8; 4] = [0x00, 0xbf, 0x00, 0xbf];
    debugger
        .write(level_meter_task_startup_call, &NOP_PAIR)
        .map_err(|error| format!("skipping the LevelMeterLed startup output sequence: {error}"))?;
    let patched_call = qmp
        .read_memory(0, level_meter_task_startup_call, NOP_PAIR.len())
        .map_err(|error| error.to_string())?;
    if patched_call != NOP_PAIR {
        qmp.resume().map_err(|error| error.to_string())?;
        return Err("could not verify the focused LevelMeterLed startup skip".into());
    }
    debugger
        .write(level_meter_active_gates, &[1, 1])
        .map_err(|error| error.to_string())?;
    debugger
        .breakpoint(level_meter_threshold_builder, true)
        .map_err(|error| format!("setting L6 meter threshold-builder breakpoint: {error}"))?;
    if let Err(error) = debugger.continue_until_stop(Duration::from_secs(5)) {
        let interrupt = debugger
            .interrupt()
            .unwrap_or_else(|interrupt| format!("interrupt failed: {interrupt}"));
        let pc = debugger
            .read_register(15)
            .map(|register| format!("{:#x}", register & !1))
            .unwrap_or_else(|read| format!("unavailable: {read}"));
        let gates = qmp
            .read_memory(0, level_meter_active_gates, 2)
            .map(|bytes| format!("{bytes:02x?}"))
            .unwrap_or_else(|read| format!("unavailable: {read}"));
        let phase = qmp
            .read_memory(0, l6::level_meter_task_phase, 4)
            .map(|bytes| format!("{bytes:02x?}"))
            .unwrap_or_else(|read| format!("unavailable: {read}"));
        qmp.resume().map_err(|resume| resume.to_string())?;
        return Err(format!(
            "waiting for L6 meter threshold builder failed: {error}; stop={interrupt}; PC={pc}; gates={gates}; phase={phase}"
        ));
    }
    if debugger
        .read_register(15)
        .map_err(|error| error.to_string())?
        & !1
        != level_meter_threshold_builder
    {
        return Err("L6 meter threshold-builder breakpoint was missed".into());
    }
    let mut stage_evidence = Vec::new();
    for value in THRESHOLD_STAGES {
        let bytes: Vec<u8> = (0..10).flat_map(|_| value.to_le_bytes()).collect();
        let stage_start = inputs.panel.snapshot().guest_ms;
        let mut sums = [[0_u64; 24]; 8];
        let mut sample_count = 0_u64;
        loop {
            debugger
                .write(level_meter_active_gates, &[1, 1])
                .map_err(|error| error.to_string())?;
            debugger
                .write(level_meter_converted_accumulators, &bytes)
                .map_err(|error| error.to_string())?;
            let injected = qmp
                .read_memory(0, level_meter_converted_accumulators, bytes.len())
                .map_err(|error| error.to_string())?;
            if injected != bytes {
                return Err(format!(
                    "L6 meter accumulator injection failed at {value}: wrote {bytes:02x?}, read {injected:02x?}"
                ));
            }
            debugger
                .continue_until_stop(Duration::from_secs(10))
                .map_err(|error| format!("advancing L6 meter task at {value}: {error}"))?;
            if debugger
                .read_register(15)
                .map_err(|error| error.to_string())?
                & !1
                != level_meter_threshold_builder
            {
                return Err("L6 meter threshold-builder breakpoint was missed".into());
            }
            let panel = session.poll().map_err(|error| error.to_string())?;
            let guest_ms = inputs.panel.snapshot().guest_ms;
            let elapsed = guest_ms.wrapping_sub(stage_start);
            if elapsed >= SETTLE_MS {
                for (row, values) in panel.indicator_levels.iter().enumerate() {
                    for (column, level) in values.iter().enumerate() {
                        sums[row][column] += u64::from(*level);
                    }
                }
                sample_count += 1;
            }
            if elapsed >= STAGE_WINDOW_MS {
                break;
            }
            if Instant::now() >= deadline || guest_ms.wrapping_sub(started_guest) > guest_timeout_ms
            {
                return Err(format!("timed out sampling L6 level-meter PWM at {value}"));
            }
        }
        if sample_count == 0 {
            return Err("L6 level-meter PWM captured no panel samples".into());
        }
        let group_a_records = qmp
            .read_memory(0, l6::level_meter_group_a_records, 12)
            .map_err(|error| error.to_string())?;
        let group_b_records = qmp
            .read_memory(0, l6::level_meter_group_b_records, 12)
            .map_err(|error| error.to_string())?;
        let threshold_masks_and_codes = qmp
            .read_memory(0, l6::level_meter_thresholds, 0x32)
            .map_err(|error| error.to_string())?;
        let mean_levels: [[f64; 24]; 8] = std::array::from_fn(|row| {
            std::array::from_fn(|column| sums[row][column] as f64 / sample_count as f64)
        });
        stage_evidence.push(json!({
            "input_level": value,
            "sample_count": sample_count,
            "group_a_records": group_a_records,
            "group_b_records": group_b_records,
            "threshold_masks_and_codes": threshold_masks_and_codes,
            "mean_levels": mean_levels,
        }));
    }
    // Treat this as a diagnostic only when the controlled input reaches a
    // panel output. A stopped task or an unmapped/uninitialized output must
    // never report a passing PWM result.
    let stage_means = stage_evidence
        .iter()
        .map(|stage| {
            stage["mean_levels"]
                .as_array()
                .unwrap()
                .iter()
                .map(|row| {
                    row.as_array()
                        .unwrap()
                        .iter()
                        .map(|level| level.as_f64().unwrap())
                        .collect::<Vec<_>>()
                })
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    let responsive_cells = (0..8)
        .flat_map(|row| (0..24).map(move |column| (row, column)))
        .filter(|&(row, column)| {
            let levels = stage_means
                .iter()
                .map(|stage| stage[row][column])
                .collect::<Vec<_>>();
            levels.first() == Some(&0.0)
                && levels[1..].iter().all(|level| *level > 0.0)
                && levels.windows(2).all(|pair| pair[1] > pair[0])
        })
        .count();
    debugger
        .breakpoint(level_meter_threshold_builder, false)
        .map_err(|error| error.to_string())?;
    qmp.resume().map_err(|error| error.to_string())?;

    let evidence = json!({
        "stimulus": "l6_level_meter_threshold_builder_inputs",
        "setup": "boot_time_panel_output_sequence_skipped",
        "responsive_cells": responsive_cells,
        "samples_per_stage": stage_evidence,
    });
    fs::write(
        options.logs.join("l6-level-meter-pwm.json"),
        serde_json::to_vec_pretty(&evidence).map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;
    if responsive_cells == 0 {
        return Err("L6 meter stimulus produced no panel cell with five distinct increasing nonzero duty levels; measurements saved to l6-level-meter-pwm.json".into());
    }
    Ok(evidence)
}

pub(super) fn run_l6max_panel_key_map(
    session: &Session,
    wall_timeout_s: u64,
) -> Result<Value, String> {
    if session.engine.model() != l6fw::Model::L6max {
        return Err("l6max-panel-key-map requires L6max firmware".into());
    }
    let inputs = session
        .engine
        .inputs
        .as_ref()
        .ok_or("panel inputs unavailable")?;
    let qmp = &session.engine.management[0];
    use firmware_api::l6max::PANEL_KEY_TABLE;
    use firmware_api::l6max::panel_key_state;
    const KEYS: [(u32, u32); 4] = [(15, 0x19), (20, 0x1a), (25, 0x1b), (30, 0x1c)];

    let read_words = || -> Result<[u32; 6], String> {
        let bytes = qmp
            .read_memory(0, panel_key_state, 24)
            .map_err(|error| error.to_string())?;
        Ok(std::array::from_fn(|index| {
            u32::from_le_bytes(bytes[index * 4..index * 4 + 4].try_into().unwrap())
        }))
    };
    let deadline = Instant::now() + Duration::from_secs(wall_timeout_s);
    let mut observations = Vec::with_capacity(KEYS.len());
    for (target, code) in KEYS {
        let lookup = qmp
            .read_memory(0, PANEL_KEY_TABLE + code * 8, 8)
            .map_err(|error| error.to_string())?;
        // The table stores the key's bit number within its state word.
        let bit = u32::from_le_bytes(lookup[..4].try_into().unwrap());
        let word_index = u32::from_le_bytes(lookup[4..].try_into().unwrap()) as usize;
        let mask = 1u32.checked_shl(bit).unwrap_or(0);
        let expected_mask = 1u32 << (code - 0x18);
        if mask != expected_mask || word_index != 4 {
            return Err(format!(
                "L6max panel key {code:#04x} maps to mask={mask:#010x}, word={word_index}; expected mask={expected_mask:#010x}, word=4"
            ));
        }
        let before = read_words()?;
        if before[word_index] & mask != 0 {
            return Err(format!(
                "L6max panel key target {target} starts active: word {}={:#010x}, mask={mask:#010x}",
                word_index, before[word_index]
            ));
        }

        let applied = inputs.panel.snapshot().applied;
        inputs.down(target, 0).map_err(|error| error.to_string())?;
        wait_l6_input(
            session,
            || inputs.panel.snapshot().applied > applied,
            "sound-pad key press acknowledgement",
        )?;
        wait_guest_duration(session, 100, deadline)?;
        let held = read_words()?;
        if held[word_index] & mask == 0 {
            return Err(format!(
                "L6max panel key target {target} did not set key {code:#04x}: word {} before={:#010x} held={:#010x} mask={mask:#010x}",
                word_index, before[word_index], held[word_index]
            ));
        }

        let applied = inputs.panel.snapshot().applied;
        inputs.up(target).map_err(|error| error.to_string())?;
        wait_l6_input(
            session,
            || inputs.panel.snapshot().applied > applied,
            "sound-pad key release acknowledgement",
        )?;
        wait_guest_duration(session, 100, deadline)?;
        let released = read_words()?;
        if released[word_index] & mask != 0 {
            return Err(format!(
                "L6max panel key target {target} remained active after release: word {}={:#010x}, mask={mask:#010x}",
                word_index, released[word_index]
            ));
        }
        observations.push(json!({
            "target": target,
            "key_code": code,
            "word_index": word_index,
            "mask": format!("{mask:#010x}"),
            "before": format!("{:#010x}", before[word_index]),
            "held": format!("{:#010x}", held[word_index]),
            "released": format!("{:#010x}", released[word_index]),
        }));
    }
    Ok(json!({"sound_pad_keys": observations}))
}

pub(super) fn run_l6_input_sweep(session: &Session, options: &Options) -> Result<Value, String> {
    use std::time::Duration;
    let inputs = session
        .engine
        .inputs
        .as_ref()
        .ok_or("L6 input sweep requires connected inputs")?;
    let trace_path = options.logs.join("engine.trace");
    let count = |pattern: &str| -> Result<usize, String> {
        let trace = fs::read_to_string(&trace_path).map_err(|error| error.to_string())?;
        Ok(trace.matches(pattern).count())
    };
    for target in [0, 5] {
        let before = inputs.main.snapshot().rejected;
        inputs
            .main
            .tap(target, 120)
            .map_err(|error| error.to_string())?;
        wait_l6_input(
            session,
            || inputs.main.snapshot().rejected > before,
            "unsupported MAIN target rejection",
        )?;
    }

    let mut panel_targets: Vec<u32> = (7..31).collect();
    panel_targets.extend([1, 2, 3, 6, 47]);
    let mut verified = Vec::with_capacity(panel_targets.len());
    for target in panel_targets {
        let press_pattern = "l6 panel USART1 TX 91\n";
        let release_pattern = "l6 panel USART1 TX 90\n";
        let presses_before = count(press_pattern)?;
        let releases_before = count(release_pattern)?;
        let panel = inputs.panel.snapshot();
        inputs
            .panel
            .tap(target, 120)
            .map_err(|error| error.to_string())?;
        wait_l6_input(
            session,
            || {
                let now = inputs.panel.snapshot();
                now.applied > panel.applied && now.released > panel.released
            },
            "panel tap application and release",
        )?;
        let deadline = Instant::now() + Duration::from_secs(10);
        while count(release_pattern)? < releases_before + 1 {
            session.poll()?;
            if Instant::now() >= deadline {
                return Err(format!(
                    "timed out waiting for L6 panel input {target} release report"
                ));
            }
            thread::sleep(Duration::from_millis(5));
        }
        let presses = count(press_pattern)?.saturating_sub(presses_before);
        let releases = count(release_pattern)?.saturating_sub(releases_before);
        if (presses, releases) != (1, 1) {
            return Err(format!(
                "L6 panel input {target} emitted {presses} press and {releases} release reports"
            ));
        }
        verified.push(target);
    }

    let encoder_pattern = "l6 panel USART1 TX a1\n";
    let encoder_before = count(encoder_pattern)?;
    let mut encoder_targets = Vec::with_capacity(6);
    for encoder in 0..6 {
        let applied = inputs.panel.snapshot().applied;
        inputs
            .encoder(encoder, 1)
            .map_err(|error| error.to_string())?;
        wait_l6_input(
            session,
            || inputs.panel.snapshot().applied > applied,
            "encoder input application",
        )?;
        encoder_targets.push(encoder);
    }
    let deadline = Instant::now() + Duration::from_secs(10);
    while count(encoder_pattern)?.saturating_sub(encoder_before) < 6 {
        session.poll()?;
        if Instant::now() >= deadline {
            return Err("timed out waiting for six L6 encoder reports".into());
        }
        thread::sleep(Duration::from_millis(5));
    }
    let encoder_reports = count(encoder_pattern)?.saturating_sub(encoder_before);
    if encoder_reports != 6 {
        return Err(format!(
            "expected six L6 encoder reports, got {encoder_reports}"
        ));
    }

    for target in [31] {
        let before = inputs.panel.snapshot().rejected;
        inputs
            .panel
            .down(target, 50)
            .map_err(|error| error.to_string())?;
        wait_l6_input(
            session,
            || inputs.panel.snapshot().rejected > before,
            "unsupported panel target rejection",
        )?;
    }
    let before = inputs.panel.snapshot().rejected;
    inputs.encoder(6, 1).map_err(|error| error.to_string())?;
    wait_l6_input(
        session,
        || inputs.panel.snapshot().rejected > before,
        "unsupported encoder rejection",
    )?;

    let rejected_main = inputs.main.snapshot().rejected;
    let rejected_panel = inputs.panel.snapshot().rejected;
    if (rejected_main, rejected_panel) != (2, 2) {
        return Err(format!(
            "L6 invalid-input checks observed {rejected_main} MAIN and {rejected_panel} panel rejections; expected 2 each"
        ));
    }

    Ok(
        json!({"panel_targets":verified,"encoders":encoder_targets,"encoder_reports":encoder_reports,"unsupported_main_targets_rejected":[0,5],"unsupported_panel_target_rejected":31,"unsupported_encoder_rejected":6,"expected_rejections":{"main":rejected_main,"panel":rejected_panel}}),
    )
}

pub(super) fn run_l6max_input_timing(
    session: &Session,
    options: &Options,
) -> Result<Value, String> {
    use std::time::Duration;
    let inputs = session
        .engine
        .inputs
        .as_ref()
        .ok_or("L6max input timing check requires connected inputs")?;
    let report_count = |pattern: &str| -> Result<usize, String> {
        let trace = fs::read_to_string(options.logs.join("engine.trace"))
            .map_err(|error| error.to_string())?;
        Ok(trace.matches(pattern).count())
    };
    let current = session.poll()?;
    let qmp_clients = &session.engine.management;
    for client in qmp_clients {
        client.stop().map_err(|error| error.to_string())?;
    }
    // Take the baseline once paused: a heartbeat sent just before the pause can
    // still arrive after it, and would read as the clock advancing while paused.
    thread::sleep(Duration::from_millis(100));
    session.poll()?;
    let input_before = inputs.panel.snapshot();
    let presses_before = report_count("l6 panel USART1 TX 91\n")?;
    let releases_before = report_count("l6 panel USART1 TX 90\n")?;
    let send_result = (|| -> Result<(), String> {
        inputs
            .panel
            .down(1, 50)
            .map_err(|error| error.to_string())?;
        inputs.panel.up(1).map_err(|error| error.to_string())?;
        inputs
            .panel
            .down(1, 50)
            .map_err(|error| error.to_string())?;
        inputs.panel.up(1).map_err(|error| error.to_string())?;
        wait_l6_input(
            session,
            || inputs.panel.snapshot().applied >= input_before.applied + 1,
            "first paused L6max input acknowledgement",
        )?;
        thread::sleep(Duration::from_millis(700));
        let paused = inputs.panel.snapshot();
        if paused.applied != input_before.applied + 1 || paused.guest_ms != input_before.guest_ms {
            return Err(format!(
                "L6max input progressed while QEMU was paused: before={input_before:?}, paused={paused:?}"
            ));
        }
        Ok(())
    })();
    let mut resume_error = None;
    for client in qmp_clients {
        if let Err(error) = client.resume() {
            resume_error.get_or_insert_with(|| error.to_string());
        }
    }
    send_result?;
    if let Some(error) = resume_error {
        return Err(format!("could not resume QEMU after paused input: {error}"));
    }
    wait_l6_input(
        session,
        || inputs.panel.snapshot().applied >= input_before.applied + 4,
        "queued L6max input delivery after resume",
    )?;
    let report_deadline = Instant::now() + Duration::from_secs(10);
    while report_count("l6 panel USART1 TX 91\n")? < presses_before + 2
        || report_count("l6 panel USART1 TX 90\n")? < releases_before + 2
    {
        session.poll()?;
        if Instant::now() >= report_deadline {
            return Err("timed out waiting for two L6max press/release reports".into());
        }
        thread::sleep(Duration::from_millis(5));
    }
    let presses = report_count("l6 panel USART1 TX 91\n")? - presses_before;
    let releases = report_count("l6 panel USART1 TX 90\n")? - releases_before;
    if (presses, releases) != (2, 2) {
        return Err(format!(
            "L6max rapid input emitted {presses} press and {releases} release reports"
        ));
    }
    let resumed_guest = inputs.panel.snapshot().guest_ms;
    wait_l6_input(
        session,
        || inputs.panel.snapshot().guest_ms.wrapping_sub(resumed_guest) >= 1500,
        "L6max display settle after rapid input",
    )?;
    let after = session.poll()?;
    if current.display == after.display {
        return Err("L6max display did not change after the two Up inputs".into());
    }
    let encoder_before = report_count("l6 panel USART1 TX a1\n")?;
    let applied = inputs.panel.snapshot().applied;
    inputs.encoder(2, 1).map_err(|error| error.to_string())?;
    wait_l6_input(
        session,
        || inputs.panel.snapshot().applied > applied,
        "L6max encoder input acknowledgement",
    )?;
    let encoder_deadline = Instant::now() + Duration::from_secs(10);
    while report_count("l6 panel USART1 TX a1\n")? < encoder_before + 1 {
        session.poll()?;
        if Instant::now() >= encoder_deadline {
            return Err("timed out waiting for the L6max encoder report".into());
        }
        thread::sleep(Duration::from_millis(5));
    }
    let encoder_reports = report_count("l6 panel USART1 TX a1\n")? - encoder_before;
    if encoder_reports != 1 {
        return Err(format!(
            "expected one L6max encoder report, got {encoder_reports}"
        ));
    }
    Ok(
        json!({"paused_guest_clock":true,"rapid_press_reports":presses,"rapid_release_reports":releases,"display_changed":true,"encoder_channel":3,"encoder_reports":encoder_reports}),
    )
}
