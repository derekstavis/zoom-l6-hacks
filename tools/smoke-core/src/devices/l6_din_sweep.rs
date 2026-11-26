//! Scoped L6 DIN sweep with concurrent, exclusively owned debugger monitor.
use super::*;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

pub(super) struct Scope {
    started: Instant,
    guest_start: u32,
    guest_budget: u32,
    wall_budget: u64,
    client: l6ctl::midi::MidiSession,
    reader: l6max_host::l6_din::Reader,
    cancel: Arc<AtomicBool>,
    monitor: Option<thread::JoinHandle<Result<[u32; 4], String>>>,
    emergency: Option<l6max_host::debug::CancellationHandle>,
    main_stop: l6ctl::MainStopHandle,
    before_ring: u32,
    previous_ring: u32,
    changes: Vec<(u8, u32, u32)>,
    before_queue: u32,
    before_mixer: Vec<u8>,
    next_cc: u16,
    accepted_bytes: Option<usize>,
    uart_messages: Option<Vec<[u8; 3]>>,
    ring_state: Option<Vec<u8>>,
    ring: Option<Vec<u8>>,
    hits: Option<[u32; 4]>,
    finished: bool,
    evidence: Option<Evidence>,
}

struct Evidence {
    accepted: usize,
    uart_messages: usize,
    uart_matching: usize,
    ring_messages: usize,
    matching: usize,
    write: u32,
    read: u32,
    changes: usize,
    hits: [u32; 4],
    mapped: usize,
    changed: usize,
}

impl Evidence {
    fn passes(&self, check: crate::scenario::DinSweepCheck) -> bool {
        use crate::scenario::DinSweepCheck::*;
        match check {
            Uart => self.uart_messages == 128 && self.uart_matching == 128,
            RingBytes => self.ring_messages == 128 && self.matching == 128,
            WriteIndex => self.write as usize == self.accepted,
            ReadIndex => self.read == self.write,
            Indicator => self.changes > 0,
            Handler => self.hits[0] == 128,
            Submitter => self.hits[1] >= self.mapped as u32,
            Reader => self.hits[2] >= self.hits[1],
            Setter => self.hits[3] > 0,
            Mixer => self.changed > 0,
        }
    }
}

fn decode_control_changes(bytes: &[u8]) -> Vec<[u8; 3]> {
    let mut status = None;
    let mut data = [0; 2];
    let mut data_len = 0;
    let mut messages = Vec::new();
    for &byte in bytes {
        if byte & 0x80 != 0 {
            if byte >= 0xf8 {
                continue;
            }
            status = (byte & 0xf0 == 0xb0).then_some(byte);
            data_len = 0;
        } else if let Some(status) = status {
            data[data_len] = byte;
            data_len += 1;
            if data_len == 2 {
                messages.push([status, data[0], data[1]]);
                data_len = 0;
            }
        }
    }
    messages
}

fn matching_sweep_messages(messages: &[[u8; 3]]) -> usize {
    messages
        .iter()
        .zip((0..=127u8).map(|cc| [0xb0, cc, 0x7f]))
        .filter(|(actual, expected)| *actual == expected)
        .count()
}

fn uart_data_reads(trace: &str) -> Vec<u8> {
    trace
        .lines()
        .filter_map(|line| {
            line.split_once("LPUART2 (MIDI jacks) DATA read ")?
                .1
                .split_whitespace()
                .next()
                .and_then(|byte| u8::from_str_radix(byte, 16).ok())
        })
        .collect()
}

fn expired(session: &Session, started: Instant, guest_start: u32, guest: u32, wall: u64) -> bool {
    started.elapsed() > Duration::from_secs(wall)
        || guest_now(session).wrapping_sub(guest_start) > guest
}
type MonitorWorker = thread::JoinHandle<Result<[u32; 4], String>>;
fn wait_worker(worker: &MonitorWorker, deadline: Instant) -> bool {
    while !worker.is_finished() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(5));
    }
    worker.is_finished()
}

fn category(pc: u32) -> Option<usize> {
    let bp = l6max_host::l6_din::BREAKPOINTS;
    if pc == bp[0] {
        Some(0)
    } else if pc == bp[1] {
        Some(1)
    } else if pc == bp[2] {
        Some(2)
    } else if bp[3..].contains(&pc) {
        Some(3)
    } else {
        None
    }
}

impl Scope {
    pub(super) fn begin(session: &Session, guest: u32, wall: u64) -> Result<Self, String> {
        if !session.supervised_din {
            return Err("L6 DIN sweep requires a direct supervised headless case".into());
        }
        if session.engine.model() != l6fw::Model::L6 {
            return Err("L6 DIN sweep requires L6".into());
        }
        let started = Instant::now();
        let guest_start = guest_now(session);
        let deadline = started + Duration::from_secs(wall);
        let check = || {
            if expired(session, started, guest_start, guest, wall) {
                Err("L6 DIN sweep shared deadline exceeded".to_owned())
            } else {
                Ok(())
            }
        };
        let endpoint = l6max_host::host_midi::endpoint_name(
            &l6max_host::host_midi::din_port_name_for_model_with_namespace(
                l6fw::Model::L6,
                midi_namespace().as_deref(),
            ),
        );
        let connect_deadline = (Instant::now() + Duration::from_secs(10)).min(deadline);
        let client = loop {
            check()?;
            match l6ctl::midi::MidiSession::connect_native(&endpoint) {
                Ok(client) => break client,
                Err(error)
                    if error.contains("missing native MIDI")
                        && Instant::now() < connect_deadline =>
                {
                    session.poll()?;
                    thread::sleep(Duration::from_millis(25));
                }
                Err(error) => return Err(error),
            }
        };
        let reader = l6ctl::OwnedControl::new(&session.engine)
            .l6_din_reader_before_checked(deadline, &check)
            .map_err(|e| e.to_string())?;
        Ok(Self {
            started,
            guest_start,
            guest_budget: guest,
            wall_budget: wall,
            client,
            reader,
            cancel: Arc::new(AtomicBool::new(false)),
            monitor: None,
            emergency: None,
            main_stop: l6ctl::OwnedControl::new(&session.engine)
                .main_stop_handle()
                .map_err(|e| e.to_string())?,
            before_ring: 0,
            previous_ring: 0,
            changes: Vec::new(),
            before_queue: 0,
            before_mixer: Vec::new(),
            next_cc: 0,
            accepted_bytes: None,
            uart_messages: None,
            ring_state: None,
            ring: None,
            hits: None,
            finished: false,
            evidence: None,
        })
    }
    fn deadline(&self) -> Instant {
        self.started + Duration::from_secs(self.wall_budget)
    }
    fn check(&self, session: &Session) -> Result<(), String> {
        if expired(
            session,
            self.started,
            self.guest_start,
            self.guest_budget,
            self.wall_budget,
        ) {
            return Err("L6 DIN sweep shared deadline exceeded".into());
        }
        if self
            .monitor
            .as_ref()
            .is_some_and(|worker| worker.is_finished())
        {
            return Err("L6 DIN dispatch monitor exited before sweep finished".into());
        }
        Ok(())
    }
    pub(super) fn ready(&self, session: &Session, options: &Options) -> Result<Value, String> {
        let deadline = self
            .deadline()
            .min(Instant::now() + Duration::from_secs(120));
        loop {
            self.check(session)?;
            if Instant::now() >= deadline {
                return Err(
                    "timed out waiting for L6 DIN UART receive and interrupt enable".into(),
                );
            }
            session.poll()?;
            let trace =
                fs::read_to_string(options.logs.join("engine.trace")).map_err(|e| e.to_string())?;
            let check = || self.check(session);
            if trace.contains("LPUART2 (MIDI jacks) CTRL 002c0000")
                && self.reader.queue(deadline, &check)? == 0
            {
                return Ok(json!({"uart_ready":true,"queue_pending":0}));
            }
            thread::sleep(Duration::from_millis(5));
        }
    }
    pub(super) fn monitor(&mut self, session: &Session) -> Result<Value, String> {
        self.check(session)?;
        if self.monitor.is_some() {
            return Err("L6 DIN monitor already active".into());
        }
        let deadline = self.deadline();
        let control = l6ctl::OwnedControl::new(&session.engine);
        let mut debugger = control
            .debug_session_before(deadline)
            .map_err(|e| e.to_string())?;
        let setup = (|| {
            self.reader
                .verify_before_checked(deadline, &|| self.check(session))?;
            for address in l6max_host::l6_din::BREAKPOINTS {
                self.check(session)?;
                debugger.breakpoint_before(
                    address,
                    true,
                    deadline.min(Instant::now() + Duration::from_secs(1)),
                )?;
                self.check(session)?;
            }
            debugger.cancellation_handle()
        })();
        self.emergency = Some(match setup {
            Ok(handle) => handle,
            Err(error) => {
                return match debugger.close_before(Instant::now() + Duration::from_secs(3)) {
                    Ok(()) => Err(error),
                    Err(cleanup) => Err(format!("{error}; DIN debugger cleanup failed: {cleanup}")),
                };
            }
        });
        let cancel = self.cancel.clone();
        let panel = l6ctl::OwnedControl::new(&session.engine)
            .input_observer()
            .map_err(|_| "DIN sweep needs panel inputs")?;
        let (started, guest_start, guest, wall) = (
            self.started,
            self.guest_start,
            self.guest_budget,
            self.wall_budget,
        );
        self.monitor = Some(thread::spawn(move || {
            let cancelled = || {
                cancel.load(Ordering::Acquire)
                    || started.elapsed() > Duration::from_secs(wall)
                    || panel
                        .status(l6ctl::InputSource::Panel)
                        .guest_ms
                        .wrapping_sub(guest_start)
                        > guest
            };
            let mut hits = [0u32; 4];
            let result = loop {
                if cancelled() {
                    break Ok(());
                }
                match debugger.continue_before(
                    deadline.min(Instant::now() + Duration::from_secs(120)),
                    cancelled,
                ) {
                    Ok(_) => {}
                    Err(error)
                        if cancel.load(Ordering::Acquire)
                            && error == "debug execution cancelled" =>
                    {
                        break Ok(());
                    }
                    Err(error) => break Err(error),
                }
                if cancelled() {
                    break Ok(());
                }
                let step = (|| -> Result<(), String> {
                    let check = || {
                        if cancelled() {
                            Err("DIN monitor cancelled".to_owned())
                        } else {
                            Ok(())
                        }
                    };
                    let short = || deadline.min(Instant::now() + Duration::from_secs(1));
                    check()?;
                    let pc = debugger.register_before(15, short())? & !1;
                    check()?;
                    let index = category(pc)
                        .ok_or_else(|| format!("unexpected DIN monitor stop at {pc:#x}"))?;
                    hits[index] = hits[index].checked_add(1).ok_or("DIN hit count overflow")?;
                    check()?;
                    debugger.breakpoint_before(pc, false, short())?;
                    check()?;
                    debugger.step_before(deadline, cancelled)?;
                    check()?;
                    debugger.breakpoint_before(pc, true, short())?;
                    check()?;
                    Ok(())
                })();
                if let Err(error) = step {
                    if cancel.load(Ordering::Acquire)
                        && (error == "DIN monitor cancelled"
                            || error == "debug execution cancelled")
                    {
                        break Ok(());
                    }
                    break Err(error);
                }
            };
            let cleanup = debugger.close_stopped_before(Instant::now() + Duration::from_secs(3));
            match (result, cleanup) {
                (Ok(()), Ok(())) => Ok(hits),
                (Err(e), Ok(())) | (Ok(()), Err(e)) => Err(e),
                (Err(e), Err(c)) => Err(format!("{e}; DIN monitor cleanup failed: {c}")),
            }
        }));
        Ok(json!({"breakpoints":6}))
    }
    pub(super) fn baseline(&mut self, session: &Session) -> Result<Value, String> {
        self.check(session)?;
        let deadline = self.deadline();
        let before_ring = l6max_host::indicators::rings(
            l6ctl::OwnedControl::new(&session.engine)
                .info()
                .map_err(|_| "DIN sweep needs inputs")?
                .panel_indicators,
        )[0];
        let check = || self.check(session);
        let queue = self.reader.queue(deadline, &check)?;
        let mixer = self.reader.mixer(deadline, &check)?;
        self.before_queue = queue;
        self.before_mixer = mixer;
        self.before_ring = before_ring;
        self.previous_ring = self.before_ring;
        Ok(json!({"queue_pending":self.before_queue,"ring":self.before_ring}))
    }
    pub(super) fn sample(&mut self, session: &Session, cc: u8) -> Result<Value, String> {
        self.check(session)?;
        if u16::from(cc) != self.next_cc {
            return Err(format!("DIN sweep expected CC {}, got {cc}", self.next_cc));
        }
        self.client
            .send_control_change(l6ctl::midi::ControlChange {
                channel: 0,
                controller: cc,
                value: 127,
            })?;
        let end = Instant::now() + Duration::from_millis(50);
        while Instant::now() < end {
            self.check(session)?;
            session.poll()?;
            thread::sleep(Duration::from_millis(2));
        }
        self.check(session)?;
        let ring = l6max_host::indicators::rings(
            l6ctl::OwnedControl::new(&session.engine)
                .info()
                .map_err(|_| "DIN sweep needs inputs")?
                .panel_indicators,
        )[0];
        if ring != self.previous_ring {
            self.changes.push((cc, self.previous_ring, ring));
            self.previous_ring = ring;
        }
        self.next_cc += 1;
        Ok(json!({"cc":cc,"ring":ring}))
    }
    pub(super) fn trace(&mut self, session: &Session, options: &Options) -> Result<Value, String> {
        self.check(session)?;
        if self.next_cc != 128 {
            return Err("DIN sweep did not send all CCs".into());
        }
        let trace =
            fs::read_to_string(options.logs.join("engine.trace")).map_err(|e| e.to_string())?;
        let accepted = trace
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
            .sum();
        let uart_messages = decode_control_changes(&uart_data_reads(&trace));
        let uart_matching = matching_sweep_messages(&uart_messages);
        self.accepted_bytes = Some(accepted);
        self.uart_messages = Some(uart_messages);
        Ok(json!({
            "uart_bytes":accepted,
            "uart_messages":self.uart_messages.as_ref().unwrap().len(),
            "matching_messages":uart_matching
        }))
    }
    pub(super) fn drain(
        &mut self,
        session: &Session,
        guest_timeout_ms: u32,
        wall_timeout_s: u64,
    ) -> Result<Value, String> {
        let started = Instant::now();
        let guest_start = guest_now(session);
        let deadline = self
            .deadline()
            .min(started + Duration::from_secs(wall_timeout_s));
        loop {
            if let Err(error) = self.check(session) {
                let state = self.reader.ring_state(self.deadline(), &|| Ok(())).ok();
                return match state {
                    Some(state) => {
                        let write = u32::from_le_bytes(state[4..8].try_into().unwrap());
                        let read = u32::from_le_bytes(state[12..16].try_into().unwrap());
                        Err(format!(
                            "{error}; DIN ring at timeout: write={write}, read={read}"
                        ))
                    }
                    None => Err(error),
                };
            }
            if Instant::now() >= deadline
                || guest_now(session).wrapping_sub(guest_start) > guest_timeout_ms
            {
                return Err("L6 DIN ring did not drain".into());
            }
            session.poll()?;
            let check = || self.check(session);
            let state = self.reader.ring_state(deadline, &check)?;
            let write = u32::from_le_bytes(state[4..8].try_into().unwrap());
            let read = u32::from_le_bytes(state[12..16].try_into().unwrap());
            if self.accepted_bytes == Some(write as usize) && read == write {
                self.ring = Some(self.reader.ring(deadline, &check)?);
                self.ring_state = Some(state);
                return Ok(json!({"write":write,"read":read}));
            }
            thread::sleep(Duration::from_millis(5));
        }
    }
    pub(super) fn stop(&mut self, session: &Session) -> Result<Value, String> {
        self.check(session)?;
        let hits = self.join_monitor()?;
        self.hits = Some(hits);
        Ok(json!({"dispatch_hits":hits}))
    }
    pub(super) fn capture(
        &mut self,
        session: &Session,
        options: &Options,
    ) -> Result<Value, String> {
        self.check(session)?;
        let deadline = self.deadline();
        let check = || self.check(session);
        let queue_after = self.reader.queue(deadline, &check)?;
        let mixer_after = self.reader.mixer(deadline, &check)?;
        let map = self.reader.map(deadline, &check)?;
        let mapped = map.iter().filter(|cc| **cc != 0xff).count();
        let changed = self
            .before_mixer
            .iter()
            .zip(&mixer_after)
            .filter(|(a, b)| a != b)
            .count();
        let state = self.ring_state.as_ref().ok_or("DIN ring state missing")?;
        let ring = self.ring.as_ref().ok_or("DIN ring bytes missing")?;
        let write = u32::from_le_bytes(state[4..8].try_into().unwrap());
        let read = u32::from_le_bytes(state[12..16].try_into().unwrap());
        let accepted = self.accepted_bytes.ok_or("DIN UART trace missing")?;
        let uart_messages = self
            .uart_messages
            .as_ref()
            .ok_or("DIN UART trace missing")?;
        let ring_messages = decode_control_changes(
            ring.get(..read as usize)
                .ok_or("DIN ring read index exceeds captured bytes")?,
        );
        let uart_matching = matching_sweep_messages(uart_messages);
        let matching = matching_sweep_messages(&ring_messages);
        let hits = self.hits.ok_or("DIN monitor counts missing")?;
        let evidence = json!({"sent_messages":128,"uart_bytes":accepted,"uart_messages":uart_messages.len(),"uart_matching_messages":uart_matching,"ring_messages":ring_messages.len(),"ring_matching_messages":matching,"write_index":write,"read_index":read,"indicator_before":self.before_ring,"indicator_after":self.previous_ring,"indicator_changes":self.changes,"main_queue_items_before":self.before_queue,"main_queue_items_after":queue_after,"mapped_cc_count":mapped,"mixer_state_changed_bytes":changed});
        fs::write(
            options.logs.join("l6-din-sweep.json"),
            serde_json::to_vec_pretty(&evidence).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
        self.evidence = Some(Evidence {
            accepted,
            uart_messages: uart_messages.len(),
            uart_matching,
            ring_messages: ring_messages.len(),
            matching,
            write,
            read,
            changes: self.changes.len(),
            hits,
            mapped,
            changed,
        });
        Ok(evidence)
    }
    pub(super) fn assert(
        &mut self,
        session: &Session,
        check: crate::scenario::DinSweepCheck,
    ) -> Result<Value, String> {
        self.check(session)?;
        let e = self.evidence.as_ref().ok_or("DIN sweep evidence missing")?;
        use crate::scenario::DinSweepCheck::*;
        let pass = e.passes(check);
        if !pass {
            return Err(format!(
                "L6 DIN sweep {check:?} assertion failed: UART {}/128 messages ({} matching), ring {}/128 messages ({} matching), {} bytes at indices {}/{}, changes {}, mapped {}, dispatch {:?}, mixer changed {}",
                e.uart_messages,
                e.uart_matching,
                e.ring_messages,
                e.matching,
                e.accepted,
                e.write,
                e.read,
                e.changes,
                e.mapped,
                e.hits,
                e.changed
            ));
        }
        if check == Mixer {
            self.finished = true;
        }
        Ok(json!({"check":check,"passed":true}))
    }
    pub(super) fn finish(&self) -> Result<Value, String> {
        if !self.finished {
            return Err("DIN sweep assertion missing".into());
        }
        Ok(json!({"finished":true}))
    }
    fn join_monitor(&mut self) -> Result<[u32; 4], String> {
        self.cancel.store(true, Ordering::Release);
        let worker = self.monitor.take().ok_or("DIN monitor missing")?;
        let emergency = !wait_worker(&worker, Instant::now() + Duration::from_secs(5));
        let mut emergency_error = None;
        if emergency {
            if let Some(handle) = self.emergency.take() {
                if let Err(error) = handle.poison_and_shutdown() {
                    emergency_error = Some(format!("GDB shutdown failed: {error}"));
                }
            }
            if let Err(error) = self
                .main_stop
                .stop_before(Instant::now() + Duration::from_secs(3))
            {
                emergency_error = Some(match emergency_error {
                    Some(prior) => format!("{prior}; MAIN stop failed: {error}"),
                    None => format!("MAIN stop failed: {error}"),
                });
            }
            // Keep ownership until the worker exits. If it cannot exit, l6test's
            // 900-second case supervisor terminates this process group and QEMU.
            // This join is deliberately not advertised as locally bounded.
        }
        let result = worker
            .join()
            .map_err(|_| "DIN monitor thread panicked".to_owned())?;
        self.emergency = None;
        if emergency {
            let primary = result
                .err()
                .unwrap_or_else(|| "DIN monitor exceeded cooperative shutdown deadline".into());
            return Err(match emergency_error {
                Some(error) => {
                    format!("{primary}; {error}; guest state uncertain; restart required")
                }
                None => {
                    format!("{primary}; debugger lease retained and MAIN stopped; restart required")
                }
            });
        }
        result
    }
    pub(super) fn abort(&mut self) -> Result<(), String> {
        if self.monitor.is_none() {
            return Ok(());
        }
        self.join_monitor().map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn each_named_check_rejects_its_own_bad_evidence() {
        use crate::scenario::DinSweepCheck::*;
        let good = || Evidence {
            accepted: 257,
            uart_messages: 128,
            uart_matching: 128,
            ring_messages: 128,
            matching: 128,
            write: 257,
            read: 257,
            changes: 1,
            hits: [128, 4, 4, 1],
            mapped: 4,
            changed: 1,
        };
        let checks = [
            Uart, RingBytes, WriteIndex, ReadIndex, Indicator, Handler, Submitter, Reader, Setter,
            Mixer,
        ];
        assert!(checks.into_iter().all(|check| good().passes(check)));
        for check in checks {
            let mut e = good();
            match check {
                Uart => e.uart_matching = 127,
                RingBytes => e.matching = 127,
                WriteIndex => e.write = 256,
                ReadIndex => e.read = 256,
                Indicator => e.changes = 0,
                Handler => e.hits[0] = 127,
                Submitter => e.hits[1] = 3,
                Reader => e.hits[2] = 3,
                Setter => e.hits[3] = 0,
                Mixer => e.changed = 0,
            }
            assert!(!e.passes(check), "{check:?}");
        }
    }
    #[test]
    fn decodes_repeated_status_bytes() {
        let bytes: Vec<_> = (0..=127u8).flat_map(|cc| [0xb0, cc, 0x7f]).collect();
        let messages = decode_control_changes(&bytes);
        assert_eq!(messages.len(), 128);
        assert_eq!(matching_sweep_messages(&messages), 128);
    }
    #[test]
    fn decodes_running_status_bytes_and_realtime_interleaving() {
        let mut bytes = vec![0xb0];
        for cc in 0..=127u8 {
            bytes.extend([cc, 0x7f]);
            if cc == 63 {
                bytes.push(0xf8);
            }
        }
        let messages = decode_control_changes(&bytes);
        assert_eq!(messages.len(), 128);
        assert_eq!(matching_sweep_messages(&messages), 128);
    }
    #[test]
    fn delayed_non_io_worker_is_detected_before_join() {
        let (release, held) = std::sync::mpsc::channel();
        let worker: MonitorWorker = thread::spawn(move || {
            held.recv().unwrap();
            Ok([0; 4])
        });
        let start = Instant::now();
        assert!(!wait_worker(&worker, start + Duration::from_millis(20)));
        assert!(start.elapsed() < Duration::from_secs(1));
        release.send(()).unwrap();
        assert_eq!(worker.join().unwrap().unwrap(), [0; 4]);
    }
    #[test]
    fn monitor_categories_cover_all_six_sites() {
        let bp = l6max_host::l6_din::BREAKPOINTS;
        assert_eq!(
            bp.map(category),
            [Some(0), Some(1), Some(2), Some(3), Some(3), Some(3)]
        );
        assert_eq!(category(0x8000_0000), None);
    }
}
