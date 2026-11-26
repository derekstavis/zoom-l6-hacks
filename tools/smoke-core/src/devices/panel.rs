//! Panel inputs and outputs: PWM levels, key maps, input sweeps and timing.
use super::*;

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
            // Continuing while still stopped at the entry breakpoint can hit
            // it again without executing the builder. Step past its first
            // instruction, then stop at the next invocation.
            debugger
                .breakpoint(level_meter_threshold_builder, false)
                .map_err(|error| error.to_string())?;
            debugger
                .request("s")
                .map_err(|error| format!("stepping into L6 meter builder: {error}"))?;
            let stepped_pc = debugger
                .read_register(15)
                .map_err(|error| error.to_string())?
                & !1;
            if stepped_pc == level_meter_threshold_builder {
                return Err("L6 meter builder did not advance past its entry".into());
            }
            debugger
                .breakpoint(level_meter_threshold_builder, true)
                .map_err(|error| error.to_string())?;
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

/// Observe guest progress while retaining the caller's overall wall and guest budgets.
pub(super) fn wait_panel_guest_elapsed(
    start_guest: u32,
    duration_ms: u32,
    operation_guest: u32,
    guest_budget: u32,
    wall_deadline: Instant,
    mut sample: impl FnMut() -> Result<(u32, Instant), String>,
    mut pump: impl FnMut() -> Result<(), String>,
) -> Result<(), String> {
    loop {
        let (now, wall_now) = sample()?;
        if wall_now >= wall_deadline || now.wrapping_sub(operation_guest) > guest_budget {
            return Err("L6max display settle exceeded its guest or wall deadline".into());
        }
        if now.wrapping_sub(start_guest) >= duration_ms {
            return Ok(());
        }
        pump()?;
    }
}

#[cfg(test)]
mod panel_guest_wait_tests {
    use super::*;
    use std::{cell::Cell, time::Duration};
    #[test]
    fn slow_guest_keeps_full_1500ms_observation_past_ten_wall_seconds() {
        let base = Instant::now();
        let ticks = Cell::new(0u32);
        let result = wait_panel_guest_elapsed(
            100,
            1500,
            100,
            60_000,
            base + Duration::from_secs(120),
            || {
                Ok((
                    100 + ticks.get() * 100,
                    base + Duration::from_secs(u64::from(ticks.get())),
                ))
            },
            || {
                ticks.set(ticks.get() + 1);
                Ok(())
            },
        );
        assert!(result.is_ok());
        assert_eq!(ticks.get(), 15);
    }
    #[test]
    fn stalled_guest_expires_at_original_wall_deadline() {
        let base = Instant::now();
        let ticks = Cell::new(0u64);
        let result = wait_panel_guest_elapsed(
            100,
            1500,
            100,
            60_000,
            base + Duration::from_secs(12),
            || Ok((100, base + Duration::from_secs(ticks.get()))),
            || {
                ticks.set(ticks.get() + 1);
                Ok(())
            },
        );
        assert!(result.unwrap_err().contains("deadline"));
        assert_eq!(ticks.get(), 12);
    }
    #[test]
    fn guest_wrap_and_overall_guest_budget_are_checked() {
        let base = Instant::now();
        assert!(
            wait_panel_guest_elapsed(
                u32::MAX - 999,
                1500,
                u32::MAX - 999,
                2000,
                base + Duration::from_secs(20),
                || Ok((500, base)),
                || Ok(())
            )
            .is_ok()
        );
        assert!(
            wait_panel_guest_elapsed(
                0,
                1500,
                0,
                1200,
                base + Duration::from_secs(20),
                || Ok((1300, base)),
                || Ok(())
            )
            .is_err()
        );
    }
}
