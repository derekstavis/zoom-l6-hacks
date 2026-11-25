//! L6 DIN MIDI: sweeps and event dispatch into the mixer.
use super::*;

/// Retries connecting to a just-enabled native MIDI endpoint. The host MIDI
/// bridge opens its CoreMIDI/ALSA port on its own worker thread sometime
/// after the engine starts, so a caller that connects right away can lose
/// that race; `crate::host_midi::connect` makes exactly one attempt and
/// returns "missing native MIDI ..." if the port isn't there yet. Polls the
/// session between attempts so the guest keeps running while this waits.
pub(super) fn connect_native_midi(
    session: &Session,
    endpoint: &str,
) -> Result<Box<dyn crate::host_midi::Client>, String> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match crate::host_midi::connect(endpoint) {
            Ok(client) => return Ok(client),
            Err(error) if error.contains("missing native MIDI") && Instant::now() < deadline => {
                session.poll()?;
                thread::sleep(Duration::from_millis(25));
            }
            Err(error) => return Err(error),
        }
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
pub(super) fn run_l6_din_sweep(session: &Session, options: &Options) -> Result<Value, String> {
    use l6max_host::host_midi::{din_port_name_for_model_with_namespace, endpoint_name};
    let endpoint = endpoint_name(&din_port_name_for_model_with_namespace(
        l6fw::Model::L6,
        midi_namespace().as_deref(),
    ));
    let mut client = connect_native_midi(session, &endpoint)?;
    run_l6_din_sweep_inner(session, options, client.as_mut())
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
pub(super) fn start_l6_din_dispatch_monitor(
    socket: &std::path::Path,
) -> Result<
    (
        std::sync::Arc<std::sync::atomic::AtomicBool>,
        [std::sync::Arc<std::sync::atomic::AtomicUsize>; 4],
        thread::JoinHandle<()>,
    ),
    String,
> {
    use l6max_diagnostics::gdb::Gdb;
    use std::sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    };

    const BREAKPOINTS: [u32; 6] = [
        l6::midi_in,                 // DIN MIDI CC handler
        l6::main_event_submit,       // Main event submitter
        l6::main_event_queue_reader, // Main event queue reader
        l6::mixer_setter,            // mixer setter
        l6::mixer_din_setter_6,      // mixer setter
        l6::mixer_set_eq,            // mixer setter
    ];
    let mut gdb = Gdb::connect(socket).map_err(|error| error.to_string())?;
    for address in BREAKPOINTS {
        gdb.breakpoint(address, true)
            .map_err(|error| error.to_string())?;
    }
    let running = Arc::new(AtomicBool::new(true));
    let counts: [Arc<AtomicUsize>; 4] = std::array::from_fn(|_| Arc::new(AtomicUsize::new(0)));
    let worker_running = running.clone();
    let worker_counts = counts.clone();
    let worker = thread::spawn(move || {
        while worker_running.load(Ordering::Relaxed) {
            if gdb.continue_until_stop(Duration::from_secs(120)).is_err() {
                break;
            }
            if !worker_running.load(Ordering::Relaxed) {
                break;
            }
            let Ok(pc) = gdb.read_register(15) else {
                break;
            };
            let address = pc & !1;
            let counter = match address {
                l6::midi_in => Some(&worker_counts[0]),
                l6::main_event_submit => Some(&worker_counts[1]),
                l6::main_event_queue_reader => Some(&worker_counts[2]),
                l6::mixer_setter | l6::mixer_din_setter_6 | l6::mixer_set_eq => {
                    Some(&worker_counts[3])
                }
                _ => None,
            };
            if let Some(counter) = counter {
                counter.fetch_add(1, Ordering::Relaxed);
            }
            if gdb.breakpoint(address, false).is_err()
                || gdb.request("s").is_err()
                || gdb.breakpoint(address, true).is_err()
            {
                break;
            }
        }
    });
    Ok((running, counts, worker))
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub(super) fn run_l6_din_sweep(_session: &Session, _options: &Options) -> Result<Value, String> {
    Err("l6-din-sweep requires CoreMIDI or ALSA".into())
}

pub(super) fn run_l6_din_sweep_inner(
    session: &Session,
    options: &Options,
    client: &mut dyn crate::host_midi::Client,
) -> Result<Value, String> {
    let inputs = session
        .engine
        .inputs
        .as_ref()
        .ok_or("L6 DIN sweep has no inputs")?;
    let mixer_snapshot = || -> Result<Vec<u8>, String> {
        let qmp = &session.engine.management[0];
        let mut snapshot = qmp
            .read_memory(0, l6::mixer_state, 0x7e)
            .map_err(|error| error.to_string())?;
        snapshot.extend(
            qmp.read_memory(0, l6::mixer_state_tail, 6)
                .map_err(|error| error.to_string())?,
        );
        Ok(snapshot)
    };
    let trace_path = options.logs.join("engine.trace");
    let ready_deadline = Instant::now() + Duration::from_secs(120);
    let main_queue_pending = || -> Result<u32, String> {
        let bytes = session.engine.management[0]
            .read_memory(0, l6::main_event_queue_pending, 4)
            .map_err(|e| e.to_string())?;
        Ok(u32::from_le_bytes(bytes.try_into().unwrap()))
    };
    // Main must be draining its queue, or the sweep only fills it.
    loop {
        session.poll()?;
        let trace = fs::read_to_string(&trace_path).map_err(|e| e.to_string())?;
        if trace.contains("LPUART2 (MIDI jacks) CTRL 002c0000") && main_queue_pending()? == 0 {
            break;
        }
        if Instant::now() >= ready_deadline {
            return Err("timed out waiting for L6 DIN UART receive and interrupt enable".into());
        }
        thread::sleep(Duration::from_millis(5));
    }
    let socket = options
        .gdb_socket
        .as_deref()
        .ok_or("L6 DIN sweep has no GDB socket")?;
    let (dispatch_running, dispatch_counts, dispatch_monitor) =
        start_l6_din_dispatch_monitor(socket)?;
    let before = l6max_host::indicators::rings(inputs.panel.snapshot().indicators)[0];
    let queue_before = session.engine.management[0]
        .read_memory(0, l6::main_event_queue_pending, 4)
        .map_err(|e| e.to_string())?;
    let queue_before = u32::from_le_bytes(queue_before.try_into().unwrap());
    let mixer_before = mixer_snapshot()?;
    let mut previous = before;
    let mut changes = Vec::new();
    for cc in 0..=127u8 {
        client.send(&[0xb0, cc, 0x7f])?;
        let deadline = Instant::now() + Duration::from_millis(50);
        while Instant::now() < deadline {
            session.poll()?;
            thread::sleep(Duration::from_millis(2));
        }
        let ring = l6max_host::indicators::rings(inputs.panel.snapshot().indicators)[0];
        if ring != previous {
            changes.push((cc, previous, ring));
            previous = ring;
        }
    }
    let trace = fs::read_to_string(&trace_path).map_err(|e| e.to_string())?;
    let accepted_bytes = trace
        .lines()
        .filter(|line| {
            line.contains("LPUART2 (MIDI jacks) host RX ") && line.contains(" RE 1 RIE 1")
        })
        .filter_map(|line| {
            line.split("host RX ")
                .nth(1)?
                .split(" bytes CTRL")
                .next()?
                .parse::<usize>()
                .ok()
        })
        .sum::<usize>();
    let expected = (0..=127u8)
        .flat_map(|cc| [0xb0, cc, 0x7f])
        .collect::<Vec<_>>();
    let drain_deadline = Instant::now() + Duration::from_secs(10);
    let (din_ring, ring_state) = loop {
        session.poll()?;
        let state = session.engine.management[0]
            .read_memory(0, l6::midi_din_ring_state, 0x20)
            .map_err(|e| e.to_string())?;
        let write_index = u32::from_le_bytes(state[4..8].try_into().unwrap());
        let read_index = u32::from_le_bytes(state[12..16].try_into().unwrap());
        if write_index >= expected.len() as u32 && read_index >= expected.len() as u32 {
            let bytes = session.engine.management[0]
                .read_memory(0, l6::midi_din_ring, expected.len())
                .map_err(|e| e.to_string())?;
            break (bytes, state);
        }
        if Instant::now() >= drain_deadline {
            return Err(format!(
                "L6 DIN ring did not drain: write index {write_index}, read index {read_index}"
            ));
        }
        thread::sleep(Duration::from_millis(5));
    };
    session.engine.management[0]
        .stop()
        .map_err(|e| format!("cannot stop L6 after DIN dispatch sweep: {e}"))?;
    dispatch_running.store(false, std::sync::atomic::Ordering::Relaxed);
    dispatch_monitor
        .join()
        .map_err(|_| "L6 DIN dispatch monitor thread panicked".to_owned())?;
    let dispatch_hits = dispatch_counts
        .each_ref()
        .map(|count| count.load(std::sync::atomic::Ordering::Relaxed));
    let matching = din_ring
        .iter()
        .zip(&expected)
        .filter(|(a, b)| a == b)
        .count();
    let write_index = u32::from_le_bytes(ring_state[4..8].try_into().unwrap());
    let read_index = u32::from_le_bytes(ring_state[12..16].try_into().unwrap());
    let queue_after = session.engine.management[0]
        .read_memory(0, l6::main_event_queue_pending, 4)
        .map_err(|e| e.to_string())?;
    let queue_after = u32::from_le_bytes(queue_after.try_into().unwrap());
    let mixer_after = mixer_snapshot()?;
    let mixer_changed_bytes = mixer_before
        .iter()
        .zip(&mixer_after)
        .filter(|(a, b)| a != b)
        .count();
    let dispatch_map = session.engine.management[0]
        .read_memory(0, l6::midi_cc_dispatch_map, 128)
        .map_err(|e| e.to_string())?;
    let mapped_cc_count = dispatch_map.iter().filter(|cc| **cc != 0xff).count();
    fs::write(
        options.logs.join("l6-din-sweep.json"),
        serde_json::to_vec_pretty(&json!({"sent_messages":128,"uart_bytes":accepted_bytes,"ring_bytes":matching,"write_index":write_index,"read_index":read_index,"indicator_before":before,"indicator_after":previous,"indicator_changes":changes,"main_queue_items_before":queue_before,"main_queue_items_after":queue_after,"mapped_cc_count":mapped_cc_count,"mixer_state_changed_bytes":mixer_changed_bytes})).unwrap(),
    )
    .map_err(|e| e.to_string())?;
    if accepted_bytes != expected.len()
        || matching != expected.len()
        || write_index != expected.len() as u32
        || read_index != write_index
        || changes.is_empty()
        || dispatch_hits[0] != 128
        || dispatch_hits[1] < mapped_cc_count
        || dispatch_hits[2] < dispatch_hits[1]
        || dispatch_hits[3] == 0
        || mixer_changed_bytes == 0
    {
        return Err(format!(
            "L6 DIN sweep failed: UART accepted {accepted_bytes}/{} bytes, ring {matching}/{}, indices {write_index}/{read_index}, indicator changes {}, Main queue {queue_before}->{queue_after}, mapped CCs {mapped_cc_count}, dispatch hits handler/submit/reader/setter={dispatch_hits:?}, mixer bytes changed {mixer_changed_bytes}",
            expected.len(),
            expected.len(),
            changes.len()
        ));
    }
    Ok(
        json!({"sent_messages":128,"uart_bytes":accepted_bytes,"ring_bytes":matching,"write_index":write_index,"read_index":read_index,"indicator_changes":changes.len(),"dispatch_hits":{"midi_handler":dispatch_hits[0],"event_submitter":dispatch_hits[1],"main_queue_reader":dispatch_hits[2],"mixer_setters":dispatch_hits[3]},"mixer_changed_bytes":mixer_changed_bytes}),
    )
}

pub(super) fn run_l6_din_event_dispatch(
    session: &Session,
    options: &Options,
    group: u8,
    cc: u8,
    value: u8,
) -> Result<Value, Box<dyn std::error::Error>> {
    use l6max_diagnostics::gdb::Gdb;
    use std::io;

    use firmware_api::l6::idle_loop;
    use firmware_api::l6::main_class2_event_handler;
    use firmware_api::l6::main_event_queue_reader;
    use firmware_api::l6::main_event_submit;
    use firmware_api::l6::main_state_return;
    use firmware_api::l6::main_task;
    use firmware_api::l6::main_wait_call;
    use firmware_api::l6::main_wait_give;
    use firmware_api::l6::main_wait_return;
    use firmware_api::l6::mixer_set_eq;
    const CHANNEL_SETTERS: [u32; 9] = [
        l6::mixer_set_eq,
        l6::mixer_din_setter_1,
        l6::mixer_set_eq,
        l6::mixer_set_eq,
        l6::mixer_din_setter_4,
        l6::mixer_din_setter_4,
        l6::mixer_din_setter_6,
        l6::mixer_din_setter_7,
        l6::mixer_din_setter_8,
    ];
    let qmp = &session.engine.management[0];
    let socket = options
        .gdb_socket
        .as_ref()
        .ok_or("l6-din-event-dispatch requires a GDB socket")?;
    let mut gdb = Gdb::connect(socket)?;
    tracing::info!(
        event = "l6_event_dispatch_stage",
        stage = "gdb_connected",
        "L6 event-dispatch debugger connected"
    );
    let read_state = || -> Result<(Vec<u8>, Vec<u8>), Box<dyn std::error::Error>> {
        Ok((
            qmp.read_memory(0, l6::mixer_state, 0x7e)?,
            qmp.read_memory(0, l6::mixer_state_tail, 6)?,
        ))
    };
    let changed_bytes = |before: &(Vec<u8>, Vec<u8>), after: &(Vec<u8>, Vec<u8>)| {
        before
            .0
            .iter()
            .zip(&after.0)
            .chain(before.1.iter().zip(&after.1))
            .filter(|(old, new)| old != new)
            .count()
    };
    let pc = |gdb: &mut Gdb| -> Result<u32, Box<dyn std::error::Error>> {
        Ok(gdb.read_register(15)? & !1)
    };

    gdb.breakpoint(main_task, true)?;
    tracing::info!(
        event = "l6_event_dispatch_stage",
        stage = "wait_main_task",
        "Waiting for L6 Main task entry"
    );
    gdb.continue_until_stop(Duration::from_secs(60))?;
    if pc(&mut gdb)? != main_task {
        return Err(io::Error::other("L6 Main task entry breakpoint was missed").into());
    }
    gdb.breakpoint(main_task, false)?;
    gdb.breakpoint(main_wait_call, true)?;
    tracing::info!(
        event = "l6_event_dispatch_stage",
        stage = "wait_main_semaphore",
        "Waiting for L6 Main semaphore call"
    );
    if let Err(error) = gdb.continue_until_stop(Duration::from_secs(30)) {
        let stop = gdb
            .interrupt()
            .unwrap_or_else(|interrupt| format!("interrupt failed: {interrupt}"));
        let stopped_pc = gdb
            .read_register(15)
            .map(|register| format!("{:#x}", register & !1))
            .unwrap_or_else(|read| format!("unavailable: {read}"));
        return Err(io::Error::other(format!(
            "waiting for Main semaphore call failed: {error}; interrupt reply {stop}; PC {stopped_pc}"
        ))
        .into());
    }
    if pc(&mut gdb)? != main_wait_call || gdb.read_register(1)? != u32::MAX {
        return Err(io::Error::other("L6 Main did not enter its infinite semaphore wait").into());
    }
    let main_wait = gdb.read_register(0)?;
    gdb.breakpoint(main_wait_call, false)?;
    gdb.breakpoint(main_wait_return, true)?;
    gdb.breakpoint(idle_loop, true)?;
    tracing::info!(
        event = "l6_event_dispatch_stage",
        stage = "wait_main_block",
        "Waiting for Main to block before event injection"
    );
    gdb.continue_until_stop(Duration::from_secs(10))?;
    if pc(&mut gdb)? != idle_loop {
        return Err(io::Error::other("L6 Main did not block before DIN injection").into());
    }
    let before = read_state()?;
    tracing::info!(
        event = "l6_event_dispatch_stage",
        stage = "inject_din_event",
        "Injecting L6 DIN event"
    );

    let stack = gdb.read_register(13)?;
    gdb.write(stack, &0u32.to_le_bytes())?;
    gdb.register(0, 2)?;
    gdb.register(1, u32::from(group))?;
    gdb.register(2, u32::from(cc))?;
    gdb.register(3, u32::from(value))?;
    gdb.register(14, idle_loop | 1)?;
    gdb.register(15, main_event_submit)?;
    gdb.continue_until_stop(Duration::from_secs(10))?;
    if pc(&mut gdb)? != idle_loop {
        return Err(io::Error::other("L6 DIN event producer did not return to Idle").into());
    }

    gdb.register(0, main_wait)?;
    gdb.register(1, 0)?;
    gdb.register(2, 0)?;
    gdb.register(3, 0)?;
    gdb.register(14, idle_loop | 1)?;
    gdb.register(15, main_wait_give)?;
    gdb.continue_until_stop(Duration::from_secs(10))?;
    if pc(&mut gdb)? != main_wait_return {
        return Err(io::Error::other("L6 Main did not wake from its state wait").into());
    }
    gdb.breakpoint(main_wait_return, false)?;
    gdb.breakpoint(main_state_return, true)?;
    gdb.continue_until_stop(Duration::from_secs(10))?;
    if pc(&mut gdb)? != main_state_return {
        return Err(io::Error::other("L6 Main state-selector return was missed").into());
    }
    let state = gdb.read_register(0)?;
    gdb.breakpoint(main_state_return, false)?;
    gdb.breakpoint(idle_loop, false)?;
    gdb.breakpoint(main_event_queue_reader, true)?;
    tracing::info!(
        event = "l6_event_dispatch_stage",
        stage = "wait_main_queue_reader",
        "Waiting for Main event queue reader"
    );
    for _ in 0..8 {
        gdb.continue_until_stop(Duration::from_secs(30))?;
        let stopped = pc(&mut gdb)?;
        if stopped == main_event_queue_reader {
            break;
        }
        if stopped == idle_loop {
            gdb.breakpoint(idle_loop, false)?;
        } else {
            return Err(io::Error::other(format!(
                "L6 Main initialization stopped at {stopped:#x}"
            ))
            .into());
        }
    }
    if pc(&mut gdb)? != main_event_queue_reader {
        return Err(io::Error::other("L6 Main did not reach its queue reader").into());
    }
    gdb.breakpoint(main_event_queue_reader, false)?;
    gdb.breakpoint(main_class2_event_handler, true)?;
    tracing::info!(
        event = "l6_event_dispatch_stage",
        stage = "wait_class2_handler",
        "Waiting for class-2 event handler"
    );
    let mut observed_event = false;
    for _ in 0..8 {
        gdb.continue_until_stop(Duration::from_secs(30))?;
        let stopped = pc(&mut gdb)?;
        if stopped == main_class2_event_handler {
            let address = gdb.read_register(0)?;
            let bytes = qmp.read_memory(0, address, 20)?;
            let words = bytes
                .chunks_exact(4)
                .map(|word| u32::from_le_bytes(word.try_into().unwrap()))
                .collect::<Vec<_>>();
            if words == [2, u32::from(group), u32::from(cc), u32::from(value), 0] {
                observed_event = true;
                break;
            }
        } else if stopped != main_event_queue_reader {
            return Err(io::Error::other(format!("L6 event queue stopped at {stopped:#x}")).into());
        }
    }
    if !observed_event {
        return Err(io::Error::other("L6 Main did not dispatch the injected DIN event").into());
    }
    gdb.breakpoint(main_class2_event_handler, false)?;
    gdb.breakpoint(mixer_set_eq, true)?;
    gdb.continue_until_stop(Duration::from_secs(10))?;
    if pc(&mut gdb)? != mixer_set_eq {
        return Err(io::Error::other("queued DIN CC did not reach the EQ setter").into());
    }
    let stack = gdb.read_register(13)?;
    let setter_value = u32::from_le_bytes(qmp.read_memory(0, stack, 4)?.try_into().unwrap());
    gdb.breakpoint(mixer_set_eq, false)?;
    gdb.breakpoint(idle_loop, true)?;
    gdb.continue_until_stop(Duration::from_secs(10))?;
    let after_event = read_state()?;
    let event_changed = changed_bytes(&before, &after_event);
    if setter_value != u32::from(value) || event_changed == 0 {
        return Err(io::Error::other(format!(
            "queued DIN CC expected setter value {value:#x} and a mixer-state change, got {setter_value:#x} and {event_changed} changed bytes"
        ))
        .into());
    }

    let mut setters = Vec::new();
    for (index, setter) in CHANNEL_SETTERS.into_iter().enumerate() {
        let before = read_state()?;
        let direct_value = 0x20 + index as u32;
        let stack = gdb.read_register(13)?;
        gdb.write(stack, &direct_value.to_le_bytes())?;
        gdb.register(0, 0)?;
        gdb.register(1, index as u32)?;
        gdb.register(2, 1)?;
        gdb.register(3, 0)?;
        gdb.register(14, idle_loop | 1)?;
        gdb.register(15, setter)?;
        gdb.continue_until_stop(Duration::from_secs(10))?;
        if pc(&mut gdb)? != idle_loop {
            return Err(io::Error::other(format!(
                "L6 setter for group {index} did not return to Idle"
            ))
            .into());
        }
        let changed = changed_bytes(&before, &read_state()?);
        if changed == 0 {
            return Err(io::Error::other(format!(
                "L6 setter for group {index} did not change mixer RAM"
            ))
            .into());
        }
        setters.push(json!({"group":index,"value":direct_value,"changed_bytes":changed}));
    }
    tracing::info!(
        event = "l6_din_event_dispatched",
        group,
        cc,
        value,
        state,
        event_changed,
        setter_value,
        "L6 DIN event reached the class-2 handler and mixer setter"
    );
    Ok(
        json!({"group":group,"cc":cc,"value":value,"main_state":state,"event_changed_bytes":event_changed,"setter_value":setter_value,"channel_setters":setters}),
    )
}

impl State {
    pub(super) fn run_host_midi_round_trip(
        &mut self,
        session: &Session,
        guest_timeout_ms: u32,
        wall_timeout_s: u64,
        direct_cable: Option<u8>,
        expect_ring_change: bool,
    ) -> Result<Value, Box<dyn std::error::Error>> {
        let model = session.engine.model();
        let endpoint = l6max_host::host_midi::endpoint_name(
            &l6max_host::host_midi::port_names_for_model_with_namespace(
                model,
                midi_namespace().as_deref(),
            )[1],
        );
        let mut client: Box<dyn crate::host_midi::Client> = if let Some(cable) = direct_cable {
            // A previous round trip deliberately retained its session for any
            // following sync. Replace it before acquiring the exclusive USB
            // lease for a new round trip; its queued events belong to that
            // earlier session and must not leak into the new one.
            self.release_retained_host_midi();
            Box::new(l6ctl::midi::MidiSession::connect_direct(
                session.engine.usb.clone().ok_or("USB host unavailable")?,
                cable,
            )?)
        } else {
            let endpoint_deadline = Instant::now() + Duration::from_secs(10);
            loop {
                match crate::host_midi::connect(&endpoint) {
                    Ok(client) => break client,
                    Err(error)
                        if error.contains("missing native MIDI")
                            && Instant::now() < endpoint_deadline =>
                    {
                        session.poll()?;
                        thread::sleep(Duration::from_millis(25));
                    }
                    Err(error) => {
                        let bridge = session
                            .engine
                            .midi
                            .as_ref()
                            .map(|midi| midi.status.lock().unwrap().clone())
                            .unwrap_or_else(|| "USB MIDI bridge unavailable".into());
                        return Err(std::io::Error::other(format!(
                            "{error}; bridge status: {bridge}"
                        ))
                        .into());
                    }
                }
            }
        };
        let inputs = session
            .engine
            .inputs
            .as_ref()
            .ok_or("MIDI test needs inputs")?;
        let expected_cc = if model == l6fw::Model::L6 {
            [0xb0, 0x53]
        } else {
            [0xb0, 0x51]
        };
        let start_guest = inputs.panel.snapshot().guest_ms;
        let deadline = Instant::now() + Duration::from_secs(wall_timeout_s.min(30));
        // Alternate direction: repeated turns one way saturate and send no CC.
        self.encoder_turned_up = !self.encoder_turned_up;
        inputs.encoder(0, if self.encoder_turned_up { -4 } else { 4 })?;
        let mut knob_message = None;
        while Instant::now() < deadline {
            session.poll()?;
            if let Some(message) = client.poll().map_err(std::io::Error::other)? {
                if message[..2] == expected_cc {
                    knob_message = Some(message);
                    break;
                }
            }
            let elapsed = inputs.panel.snapshot().guest_ms.wrapping_sub(start_guest);
            if elapsed > guest_timeout_ms {
                break;
            }
            thread::sleep(Duration::from_millis(5));
        }
        let knob_message = knob_message.ok_or_else(|| {
            std::io::Error::other(format!(
                "model-specific encoder CC {expected_cc:02x?} did not reach the host MIDI endpoint"
            ))
        })?;
        // Let the encoder's remaining reports land before taking the baseline.
        wait_guest_duration(session, 1000, deadline)?;
        let before = inputs.panel.snapshot().indicators;
        let mixer_cc = if model == l6fw::Model::L6 { 0x53 } else { 0x51 };
        client
            .send(&[0xb0, mixer_cc, 100])
            .map_err(std::io::Error::other)?;
        let indicator_deadline =
            Instant::now() + Duration::from_secs(3).min(Duration::from_secs(wall_timeout_s));
        let after = loop {
            session.poll()?;
            let after = inputs.panel.snapshot().indicators;
            let changed =
                l6max_host::indicators::rings(after)[0] != l6max_host::indicators::rings(before)[0];
            if changed {
                if expect_ring_change {
                    break after;
                }
                return Err(std::io::Error::other(format!(
                    "direct USB MIDI cable {} unexpectedly changed the channel-1 ring",
                    direct_cable.unwrap_or(1)
                ))
                .into());
            }
            if Instant::now() >= indicator_deadline {
                if !expect_ring_change {
                    break before;
                }
                return Err(std::io::Error::other(format!(
                    "host MIDI CC {mixer_cc:#04x}=100 did not change the model's firmware indicator"
                ))
                .into());
            }
            thread::sleep(Duration::from_millis(5));
        };
        self.host_midi = Some(client);
        let endpoint_description = if let Some(cable) = direct_cable {
            format!("direct-usb-cable-{cable}")
        } else {
            endpoint
        };
        Ok(
            json!({"endpoint":endpoint_description,"encoder_cc":knob_message,"mixer_cc":[0xb0,mixer_cc,100],"expect_ring_change":expect_ring_change,"indicator_before":before,"indicator_after":after}),
        )
    }

    pub(super) fn run_l6_midi_ring_pin_sweep(
        &mut self,
        session: &Session,
        cc: u8,
        wall_timeout_s: u64,
    ) -> Result<Value, Box<dyn std::error::Error>> {
        if session.engine.model() != l6fw::Model::L6 || !(0x53..=0x58).contains(&cc) {
            return Err(std::io::Error::other(
                "L6 ring-pin sweep requires L6 firmware and CC 83 through 88",
            )
            .into());
        }
        let client = self
            .host_midi_client()
            .ok_or("L6 ring-pin sweep requires a preceding direct USB MIDI operation")?;
        let deadline = Instant::now() + Duration::from_secs(wall_timeout_s);
        client.send(&[0xb0, cc, 0]).map_err(std::io::Error::other)?;
        wait_guest_duration(session, 1000, deadline).map_err(std::io::Error::other)?;
        let expected_ring = usize::from(cc - 0x53);
        let mut changed_segments = 0u32;
        let mut observations = Vec::new();
        for value in [16, 32, 48, 64, 80, 96, 100, 112, 127, 0] {
            let before = session
                .engine
                .inputs
                .as_ref()
                .ok_or("L6 ring-pin sweep needs panel inputs")?
                .panel
                .snapshot()
                .indicators;
            client
                .send(&[0xb0, cc, value])
                .map_err(std::io::Error::other)?;
            wait_guest_duration(session, 500, deadline).map_err(std::io::Error::other)?;
            let after = session
                .engine
                .inputs
                .as_ref()
                .unwrap()
                .panel
                .snapshot()
                .indicators;
            let before_rings = l6max_host::indicators::rings(before);
            let after_rings = l6max_host::indicators::rings(after);
            let changes =
                std::array::from_fn::<_, 8, _>(|ring| before_rings[ring] ^ after_rings[ring]);
            if changes
                .iter()
                .enumerate()
                .any(|(ring, changed)| ring != expected_ring && *changed != 0)
            {
                return Err(std::io::Error::other(format!(
                    "L6 CC {cc} unexpectedly changed decoded ring masks {changes:03x?}"
                ))
                .into());
            }
            changed_segments |= changes[expected_ring];
            observations.push(
                json!({"value":value,"before":before_rings,"after":after_rings,"changed":changes}),
            );
        }
        if changed_segments != 0x1fe {
            return Err(std::io::Error::other(format!(
                "L6 CC {cc} changed ring {} segments {changed_segments:#05x}, expected 0x1fe",
                expected_ring + 1
            ))
            .into());
        }
        Ok(
            json!({"cc":cc,"ring":expected_ring + 1,"changed_segments":changed_segments,"observations":observations}),
        )
    }

    pub(super) fn run_l6_midi_mute_indicators(
        &mut self,
        session: &Session,
        wall_timeout_s: u64,
    ) -> Result<Value, Box<dyn std::error::Error>> {
        if session.engine.model() != l6fw::Model::L6 {
            return Err(
                std::io::Error::other("L6 mute-indicator probe requires L6 firmware").into(),
            );
        }
        let client = self
            .host_midi_client()
            .ok_or("L6 mute-indicator probe requires a preceding direct USB MIDI operation")?;
        let expected = [
            [0, 0, 0, 0, 1 << 11, 0, 0, 0],
            [0, 0, 0, 0, 1 << 10, 0, 0, 0],
            [0, 0, 0, 0, 0, 1 << 11, 0, 0],
            [0, 0, 0, 0, 0, 1 << 10, 0, 0],
            [0, 0, 0, 0, 0, 0, 1 << 11, 0],
            [0, 0, 0, 0, 0, 0, 1 << 10, 0],
        ];
        let deadline = Instant::now() + Duration::from_secs(wall_timeout_s);
        let mut observations = Vec::new();
        for (channel, cc) in [0x5d, 0x5e, 0x5f, 0x66, 0x67, 0x68].into_iter().enumerate() {
            let before = session
                .engine
                .inputs
                .as_ref()
                .ok_or("L6 mute-indicator probe needs panel inputs")?
                .panel
                .snapshot()
                .indicators;
            client
                .send(&[0xb0, cc, 127])
                .map_err(std::io::Error::other)?;
            wait_guest_duration(session, 500, deadline).map_err(std::io::Error::other)?;
            let active = session
                .engine
                .inputs
                .as_ref()
                .unwrap()
                .panel
                .snapshot()
                .indicators;
            client.send(&[0xb0, cc, 0]).map_err(std::io::Error::other)?;
            wait_guest_duration(session, 500, deadline).map_err(std::io::Error::other)?;
            let released = session
                .engine
                .inputs
                .as_ref()
                .unwrap()
                .panel
                .snapshot()
                .indicators;
            let on = std::array::from_fn::<_, 8, _>(|row| before[row] ^ active[row]);
            let off = std::array::from_fn::<_, 8, _>(|row| active[row] ^ released[row]);
            if on != expected[channel] || off != expected[channel] {
                return Err(std::io::Error::other(format!(
                    "L6 channel-{} mute CC {cc:#04x} indicator mismatch: on={on:?}, off={off:?}",
                    channel + 1
                ))
                .into());
            }
            observations.push(json!({"channel":channel + 1,"cc":cc,"on":on,"off":off}));
        }
        Ok(json!({"channels":observations}))
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
    } else if l6max_host::indicators::rings(after)[index]
        != l6max_host::indicators::rings(before)[index]
    {
        RingObservation::Changed
    } else {
        RingObservation::Waiting
    }
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
        let inputs = session
            .engine
            .inputs
            .as_ref()
            .ok_or("MIDI test needs inputs")?;
        if matches!(operation, Operation::HostMidiConnect) {
            if self.host_midi.is_some() || self.native_midi.is_some() {
                return Err("host MIDI client already connected".into());
            }
            let started = Instant::now();
            let guest_start = inputs.panel.snapshot().guest_ms;
            let endpoint = l6max_host::host_midi::endpoint_name(
                &l6max_host::host_midi::port_names_for_model_with_namespace(
                    session.engine.model(),
                    midi_namespace().as_deref(),
                )[1],
            );
            // This retries exactly the same endpoint race as existing MIDI consumers.
            let client = retry_native_midi(
                || MidiSession::connect_native(&endpoint),
                || session.poll().map(|_| ()),
                || inputs.panel.snapshot().guest_ms,
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
                    .engine
                    .midi
                    .as_ref()
                    .map(|midi| midi.status.lock().unwrap().clone())
                    .unwrap_or_else(|| "USB MIDI bridge unavailable".into());
                std::io::Error::other(format!("{error}; bridge status: {bridge}"))
            })?;
            self.native_midi = Some(client);
            self.midi_round_trip = Some(MidiRoundTrip {
                guest_start,
                receive_start: inputs.panel.snapshot().guest_ms,
                deadline: Instant::now() + Duration::from_secs(wall_timeout_s.min(30)),
                overall_deadline: started + Duration::from_secs(wall_timeout_s),
                guest_timeout_ms,
                midi_sent_at: None,
            });
            return Ok(json!({"endpoint": endpoint}));
        }
        if matches!(operation, Operation::HostMidiSyncBegin) {
            let overall_deadline = Instant::now() + Duration::from_secs(wall_timeout_s);
            if self.midi_sync.is_some() || self.midi_round_trip.is_some() {
                return Err("host MIDI transaction is already active".into());
            }
            if self.host_midi_client().is_none() {
                return Err("MIDI sync requires a connected MIDI session".into());
            }
            let guest_start = inputs.panel.snapshot().guest_ms;
            let mut remaining = Vec::new();
            let (expected_count, expected_status) = if session.engine.model() == l6fw::Model::L6 {
                let config = l6ctl::OwnedControl::new(&session.engine).midi_config()?;
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
            let transport = if self.native_midi.is_some() {
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
                || inputs
                    .panel
                    .snapshot()
                    .guest_ms
                    .wrapping_sub(scope.guest_start)
                    > scope.guest_timeout_ms
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
                            || inputs.panel.snapshot().guest_ms.wrapping_sub(start) > guest_budget
                    },
                    thread::sleep,
                )?;
                if Instant::now() >= deadline
                    || inputs.panel.snapshot().guest_ms.wrapping_sub(start) > guest_budget
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
                    || inputs
                        .panel
                        .snapshot()
                        .guest_ms
                        .wrapping_sub(scope.guest_start)
                        > scope.guest_timeout_ms
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
                    || inputs.panel.snapshot().guest_ms,
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
            let guest = inputs.panel.snapshot().guest_ms;
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
            if !independent_operation
                && context.exceeded(Instant::now(), inputs.panel.snapshot().guest_ms)
            {
                return Err("device operation exceeded its shared deadline".into());
            }
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
                let receipt =
                    l6ctl::OwnedControl::new(&session.engine).encoder(channel as u32, steps)?;
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
                    || context.exceeded(Instant::now(), inputs.panel.snapshot().guest_ms),
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
                            || context.exceeded(Instant::now(), inputs.panel.snapshot().guest_ms)
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
                while !context.receive_expired(Instant::now(), inputs.panel.snapshot().guest_ms) {
                    session.poll()?;
                    if context.receive_expired(Instant::now(), inputs.panel.snapshot().guest_ms) {
                        break;
                    }
                    if let Some(message) = poll_timed_control_change(
                        context,
                        || client.poll_control_change(),
                        Instant::now,
                        || inputs.panel.snapshot().guest_ms,
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
                let start = inputs.panel.snapshot().guest_ms;
                while inputs.panel.snapshot().guest_ms.wrapping_sub(start) < guest_ms {
                    session.poll()?;
                    let now = inputs.panel.snapshot().guest_ms;
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
                midi_transaction,
            } => {
                let deadline = if midi_transaction {
                    let context = self
                        .midi_round_trip
                        .as_ref()
                        .ok_or("ring assertion requires an active host MIDI transaction")?;
                    let sent = context
                        .midi_sent_at
                        .ok_or("ring assertion requires a host MIDI CC sent in this transaction")?;
                    (sent + Duration::from_secs(wall_timeout_s)).min(context.overall_deadline)
                } else {
                    Instant::now() + Duration::from_secs(wall_timeout_s)
                };
                let index = channel
                    .checked_sub(1)
                    .ok_or("ring channel must be positive")?;
                let after = loop {
                    session.poll()?;
                    let after = inputs.panel.snapshot().indicators;
                    let observation =
                        ring_observation(before, after, index, Instant::now(), deadline);
                    if observation == RingObservation::Expired
                        || midi_transaction
                            && self.midi_round_trip.as_ref().is_some_and(|context| {
                                context.exceeded(Instant::now(), inputs.panel.snapshot().guest_ms)
                            })
                    {
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
            if !independent_operation
                && context.exceeded(Instant::now(), inputs.panel.snapshot().guest_ms)
            {
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
            midi_transaction: false
        }));
        assert!(uses_midi_budget(&Operation::ExpectRingChanged {
            channel: 1,
            before: [0; 8],
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
            if self.l6_midi_probe.is_some() || session.engine.model() != l6fw::Model::L6 {
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
            let debug = l6ctl::OwnedControl::new(&session.engine)
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
                scope.advance(session, 10_000)?;
                let mut index = 0;
                let mut replies = Vec::new();
                for _ in 0..scope.expected.len() + 64 {
                    let timeout_ms = scope.remaining_ms(session, 60_000)?;
                    let O::L6MidiSender {
                        length,
                        route,
                        return_pc,
                        bytes: Some(bytes),
                        ..
                    } = scope.command(session, C::L6MidiSender { timeout_ms })?
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
                        let timeout_ms = scope.remaining_ms(session, 60_000)?;
                        scope.command(
                            session,
                            C::L6MidiBreakpoint {
                                address: midi_send,
                                enabled: false,
                                timeout_ms,
                            },
                        )?;
                        let timeout_ms = scope.remaining_ms(session, 60_000)?;
                        scope.command(
                            session,
                            C::L6MidiBreakpoint {
                                address: return_pc,
                                enabled: true,
                                timeout_ms,
                            },
                        )?;
                        scope.advance(session, 10_000)?;
                        let timeout_ms = scope.remaining_ms(session, 60_000)?;
                        let O::L6MidiFinished {
                            requires_restart: true,
                        } = scope.command(session, C::L6MidiFinish { timeout_ms })?
                        else {
                            return Err("L6 MIDI probe did not finish stopped".into());
                        };
                        self.l6_midi_probe.take();
                        return Ok(json!({"replies":replies,"requires_restart":true}));
                    }
                    let timeout_ms = scope.remaining_ms(session, 60_000)?;
                    scope.command(session, C::L6MidiStep { timeout_ms })?;
                    scope.advance(session, 10_000)?;
                }
                Err(format!(
                    "L6 MIDI sync matched {index}/{} expected CCs",
                    scope.expected.len()
                ))
            }
            _ => Err("L6 MIDI probe actions must execute in order".into()),
        }
    }
}

#[cfg(test)]
mod l6_hook_probe_tests {
    use super::*;
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
            if self.din_usb.is_some() || session.engine.model() != l6fw::Model::L6 {
                return Err("DIN USB forwarding requires a fresh L6 scope".into());
            }
            l6ctl::OwnedControl::new(&session.engine).connect_din()?;
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
                let mut usb = l6ctl::OwnedControl::new(&session.engine).raw_usb_midi()?;
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
