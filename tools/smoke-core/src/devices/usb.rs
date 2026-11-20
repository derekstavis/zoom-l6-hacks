//! L6 USB startup.
use super::*;

/// Attach the cable and wait, under the step's deadlines, until firmware runs
/// its device controller. Firmware starts it after boot (about 22 guest s on
/// L6; about 4 s on L6max, or 42 s with the cable attached at power-on), and
/// the device model refuses tokens with ENODEV until then.
pub(super) fn wait_usb_controller(
    session: &Session,
    usb: &l6max_host::usb::UsbHost,
    guest_timeout_ms: u32,
    wall_timeout_s: u64,
) -> Result<(), String> {
    usb.connect(true).map_err(|error| error.to_string())?;
    let start = guest_now(session);
    let ceiling = Instant::now() + Duration::from_secs(wall_timeout_s);
    while !usb
        .controller_running()
        .map_err(|error| error.to_string())?
    {
        if Instant::now() >= ceiling || guest_now(session).wrapping_sub(start) > guest_timeout_ms {
            return Err("USB device controller did not start".into());
        }
        session.poll()?;
        thread::sleep(Duration::from_millis(25));
    }
    Ok(())
}

pub(super) fn run_l6_usb_startup(
    session: &Session,
    options: &Options,
    guest_timeout_ms: u32,
    wall_timeout_s: u64,
) -> Result<Value, String> {
    use l6max_diagnostics::gdb;
    const BREAKPOINTS: [(u32, &str); 3] = [
        (l6::usb_start_wrapper, "usb_start_wrapper"),
        (l6::usb_start_initializer, "usb_start_initializer"),
        (l6::usb_phy_gate, "usb_phy_gate"),
    ];
    let qmp = &session.engine.management[0];
    let inputs = session
        .engine
        .inputs
        .as_ref()
        .ok_or("input hub unavailable")?;
    let socket = options
        .gdb_socket
        .as_ref()
        .ok_or("l6-usb-startup requires GDB")?;
    let mut debugger = gdb::Gdb::connect(socket).map_err(|error| error.to_string())?;
    for (address, _) in BREAKPOINTS {
        debugger
            .breakpoint(address, true)
            .map_err(|error| error.to_string())?;
    }
    qmp.resume().map_err(|error| error.to_string())?;

    let mut hits = Vec::new();
    let mut targeted_inputs = 0usize;
    let mut panel_sweep = 0usize;
    let mut panel_ids = vec![1, 2, 3, 6];
    panel_ids.extend(7..=47);
    let deadline = Instant::now() + Duration::from_secs(wall_timeout_s);
    while inputs.panel.snapshot().guest_ms < 60_000 {
        session.poll()?;
        let guest_ms = inputs.panel.snapshot().guest_ms;
        if guest_ms > guest_timeout_ms {
            return Err(format!(
                "L6 USB startup exceeded its guest deadline at {guest_ms} ms"
            ));
        }
        if Instant::now() > deadline {
            return Err(format!(
                "60-second L6 USB probe timed out at {guest_ms} guest ms"
            ));
        }
        if !qmp
            .query_status()
            .map_err(|error| error.to_string())?
            .running
        {
            let pc = debugger
                .read_register(15)
                .map_err(|error| error.to_string())?
                & !1;
            let Some((address, name)) = BREAKPOINTS.iter().find(|(address, _)| *address == pc)
            else {
                return Err(format!(
                    "L6 USB startup stopped at unexpected PC {pc:#010x}"
                ));
            };
            let registers = (0..4)
                .map(|register| debugger.read_register(register))
                .collect::<Result<Vec<_>, _>>()
                .map_err(|error| error.to_string())?;
            hits.push(json!({
                "function": name,
                "address": format!("{address:#010x}"),
                "r0_r3": registers,
                "guest_ms": inputs.panel.snapshot().guest_ms,
            }));
            debugger
                .breakpoint(*address, false)
                .map_err(|error| error.to_string())?;
            qmp.resume().map_err(|error| error.to_string())?;
        } else {
            let guest_ms = inputs.panel.snapshot().guest_ms;
            let target_times = [40_000, 42_000, 44_000, 46_000];
            if targeted_inputs < target_times.len() && guest_ms >= target_times[targeted_inputs] {
                if targeted_inputs < 2 {
                    inputs.panel.tap(3, 50).map_err(|error| error.to_string())?;
                } else {
                    inputs.main.tap(49, 50).map_err(|error| error.to_string())?;
                }
                targeted_inputs += 1;
            } else if targeted_inputs == target_times.len()
                && panel_sweep < panel_ids.len()
                && guest_ms >= 48_000 + panel_sweep as u32 * 250
            {
                inputs
                    .panel
                    .tap(panel_ids[panel_sweep], 50)
                    .map_err(|error| error.to_string())?;
                panel_sweep += 1;
            }
            if Instant::now() > deadline {
                return Err(format!(
                    "60-second L6 USB probe timed out at {} guest ms",
                    inputs.panel.snapshot().guest_ms
                ));
            }
            thread::sleep(Duration::from_millis(5));
        }
    }
    qmp.stop().map_err(|error| error.to_string())?;
    session.poll()?;
    let trace =
        fs::read_to_string(options.logs.join("engine.trace")).map_err(|error| error.to_string())?;
    for (address, name) in BREAKPOINTS {
        if !hits
            .iter()
            .any(|hit| hit["address"] == format!("{address:#010x}"))
        {
            return Err(format!(
                "L6 USB startup did not reach {name} at {address:#010x}"
            ));
        }
    }
    let usb_controller_writes = trace.matches("l6_usb_write offset=").count();
    if usb_controller_writes == 0 {
        return Err(
            "L6 USB startup reached its functions but wrote no controller registers".into(),
        );
    }
    if targeted_inputs != 4 || panel_sweep != panel_ids.len() {
        return Err(format!(
            "input replay incomplete: targeted {targeted_inputs}/4, panel {panel_sweep}/{}",
            panel_ids.len()
        ));
    }
    Ok(json!({
        "model": "L6",
        "guest_ms": inputs.panel.snapshot().guest_ms,
        "targeted_inputs": targeted_inputs,
        "panel_input_ids": panel_ids,
        "breakpoint_hits": hits,
        "usb_controller_writes": usb_controller_writes,
    }))
}
