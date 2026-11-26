//! Scoped ownership of the optional QEMU MAIN GDB stub.
//!
//! The lease coordinates clients using `l6max-diagnostics`; arbitrary
//! third-party GDB clients that bypass it cannot be coordinated. A session
//! stops MAIN rather than resuming it if debugger state becomes uncertain. The panel is not stopped by this API.
use crate::{midi_config, qmp::QmpClient};
use l6fw::Model;
use l6max_diagnostics::gdb::Gdb;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

const SCRATCH: u32 = 0x841f_f000;
const SCRATCH_LEN: usize = 8;
const MAX_BREAKPOINTS: usize = 4;
const MAX_CONTINUE_MS: u64 = 60_000;
const IDLE_GUARD: &str = "f4870ac4ce0c3cb238f92e437a9f5511912eee645168943b43a0166622b85ba2";
const SENDER_GUARD: &str = "f07c456c512bff15fb7112a0f8ad5993dab518b4fbfd4ed1544de2208681c27d";
const XPSR: u32 = 25;
const SIGNAL_ACCUMULATORS: u32 = 0x2001_50e0;
const SIGNAL_ROUTINE: u32 = 0x8001_0aa8;
const SIGNAL_ROUTINE_SHA256: &str =
    "75aa1f22ed2934b38a18a09cb0c9ec2fe01aa09c1cc1d362473eae39b6216f52";

/// Identity derived from the selected patch report and, for SD, its module.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum L6MidiBinding {
    Firmware,
    Sd {
        dispatcher: u32,
        site_index: u32,
        entry_offset: u32,
    },
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "command", rename_all = "kebab-case", deny_unknown_fields)]
pub enum Command {
    Register {
        index: u32,
    },
    Read {
        address: u32,
        length: usize,
    },
    AddBreakpoint {
        address: u32,
    },
    RemoveBreakpoint {
        address: u32,
    },
    Continue {
        timeout_ms: u64,
    },
    Step,
    L6maxSignalWrite {
        values: [f32; 12],
        timeout_ms: u64,
    },
    L6ScratchRead,
    L6ScratchWrite {
        bytes: [u8; SCRATCH_LEN],
    },
    L6MidiResolveSd {
        dispatcher: u32,
        site_index: u32,
        entry_offset: u32,
        timeout_ms: u64,
    },
    L6MidiInject {
        expected_hook: u32,
        binding: L6MidiBinding,
        timeout_ms: u64,
        leave_stopped: bool,
    },
    L6MidiSender {
        timeout_ms: u64,
    },
    L6MidiHook {
        timeout_ms: u64,
    },
    L6MidiStep {
        timeout_ms: u64,
    },
    L6MidiBreakpoint {
        address: u32,
        enabled: bool,
        timeout_ms: u64,
    },
    L6MidiFinish {
        timeout_ms: u64,
    },
    Close,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum Observation {
    Register {
        index: u32,
        value: u32,
    },
    Memory {
        address: u32,
        bytes: Vec<u8>,
    },
    Breakpoint {
        address: u32,
        installed: bool,
    },
    Stopped {
        reply: String,
    },
    SignalWritten {
        address: u32,
    },
    Scratch {
        address: u32,
        bytes: Vec<u8>,
    },
    L6MidiInjected {
        expected_hook: u32,
        event: Vec<u8>,
        config: midi_config::Config,
        idle_sp: u32,
        idle_xpsr: u32,
        requires_restart: bool,
    },
    L6MidiResolved {
        hook: u32,
    },
    L6MidiSender {
        pc: u32,
        buffer: u32,
        length: u32,
        route: u32,
        return_pc: u32,
        bytes: Option<Vec<u8>>,
    },
    L6MidiHook {
        pc: u32,
        event: Vec<u8>,
    },
    L6MidiFinished {
        requires_restart: bool,
    },
    Closed,
}

fn validate_signal_guard(model: Model, bytes: &[u8]) -> Result<(), String> {
    if model != Model::L6max {
        return Err("SIGNAL accumulators require L6max".into());
    }
    if bytes.len() != 32 || format!("{:x}", Sha256::digest(bytes)) != SIGNAL_ROUTINE_SHA256 {
        return Err("unsupported L6max SIGNAL routine revision".into());
    }
    Ok(())
}

fn l6_probe_deadline(timeout_ms: u64) -> Result<Instant, String> {
    if timeout_ms == 0 || timeout_ms > MAX_CONTINUE_MS {
        return Err("L6 MIDI probe timeout must be 1..60000 ms".into());
    }
    Instant::now()
        .checked_add(Duration::from_millis(timeout_ms))
        .ok_or("L6 MIDI probe deadline overflow".into())
}

fn validate_stop(reply: String) -> Result<String, String> {
    let bytes = reply.as_bytes();
    if bytes.len() < 3
        || !matches!(bytes[0], b'S' | b'T')
        || !bytes[1].is_ascii_hexdigit()
        || !bytes[2].is_ascii_hexdigit()
    {
        return Err(format!("unexpected GDB stop reply: {reply}"));
    }
    Ok(reply)
}

trait Guest: Send + Sync {
    fn running(&self) -> Result<bool, String>;
    fn running_before(&self, deadline: Instant) -> Result<bool, String>;
    fn resume_before(&self, deadline: Instant) -> Result<(), String>;
    fn stop(&self) -> Result<(), String>;
    fn resume(&self) -> Result<(), String>;
    fn guard_l6_scratch(&self) -> Result<(), String>;
    fn guard_l6max_signal(&self, deadline: Instant) -> Result<(), String>;
    fn stop_before(&self, deadline: Instant) -> Result<(), String>;
    fn guard_l6_midi_probe(
        &self,
        deadline: Instant,
        hook: u32,
        binding: &L6MidiBinding,
    ) -> Result<midi_config::Config, String>;
    fn resolve_l6_sd_hook(
        &self,
        deadline: Instant,
        dispatcher: u32,
        site_index: u32,
        entry_offset: u32,
    ) -> Result<u32, String>;
}
struct MainGuest {
    qmp: QmpClient,
    model: Model,
}
impl Guest for MainGuest {
    fn running(&self) -> Result<bool, String> {
        self.qmp
            .query_status()
            .map(|s| s.running)
            .map_err(|e| e.to_string())
    }
    fn running_before(&self, deadline: Instant) -> Result<bool, String> {
        self.qmp.running_before(deadline).map_err(|e| e.to_string())
    }
    fn resume_before(&self, deadline: Instant) -> Result<(), String> {
        self.qmp
            .resume_before(deadline)
            .map_err(|e| e.to_string())?;
        if !self
            .qmp
            .running_before(deadline)
            .map_err(|e| e.to_string())?
        {
            return Err("MAIN still stopped after resume".into());
        }
        Ok(())
    }
    fn stop(&self) -> Result<(), String> {
        self.qmp.stop().map_err(|e| e.to_string())?;
        if self.qmp.query_status().map_err(|e| e.to_string())?.running {
            return Err("MAIN still running after stop".into());
        }
        Ok(())
    }
    fn resume(&self) -> Result<(), String> {
        self.qmp.resume().map_err(|e| e.to_string())?;
        if !self.qmp.query_status().map_err(|e| e.to_string())?.running {
            return Err("MAIN still stopped after resume".into());
        }
        Ok(())
    }
    fn stop_before(&self, deadline: Instant) -> Result<(), String> {
        self.qmp.stop_before(deadline).map_err(|e| e.to_string())
    }
    fn guard_l6max_signal(&self, deadline: Instant) -> Result<(), String> {
        if self.model != Model::L6max {
            return Err("SIGNAL accumulators require L6max".into());
        }
        let bytes = self
            .qmp
            .read_memory_before(0, SIGNAL_ROUTINE, 32, deadline)
            .map_err(|e| e.to_string())?;
        validate_signal_guard(self.model, &bytes)
    }
    fn guard_l6_scratch(&self) -> Result<(), String> {
        midi_config::config(&self.qmp, self.model).map(|_| ())
    }
    fn guard_l6_midi_probe(
        &self,
        deadline: Instant,
        hook: u32,
        binding: &L6MidiBinding,
    ) -> Result<midi_config::Config, String> {
        let config = midi_config::config_before(&self.qmp, self.model, deadline)?;
        validate_l6_probe_guards(self.model, |address, length| {
            self.qmp
                .read_memory_before(0, address, length, deadline)
                .map_err(|e| e.to_string())
        })?;
        validate_l6_hook_binding(hook, binding, |address, length| {
            self.qmp
                .read_memory_before(0, address, length, deadline)
                .map_err(|e| e.to_string())
        })?;
        Ok(config)
    }
    fn resolve_l6_sd_hook(
        &self,
        deadline: Instant,
        dispatcher: u32,
        site_index: u32,
        entry_offset: u32,
    ) -> Result<u32, String> {
        if self.model != Model::L6 {
            return Err("MIDI hook probe requires L6 1.10".into());
        }
        let mut read = |address, length| {
            self.qmp
                .read_memory_before(0, address, length, deadline)
                .map_err(|e| e.to_string())
        };
        let hook = resolve_l6_sd_registry(site_index, entry_offset, &mut read)?;
        validate_l6_hook_binding(
            hook,
            &L6MidiBinding::Sd {
                dispatcher,
                site_index,
                entry_offset,
            },
            read,
        )?;
        Ok(hook)
    }
}
fn resolve_l6_sd_registry(
    site_index: u32,
    entry_offset: u32,
    read: &mut impl FnMut(u32, usize) -> Result<Vec<u8>, String>,
) -> Result<u32, String> {
    use patch_abi::registry;
    let mut word = |address| -> Result<u32, String> {
        Ok(u32::from_le_bytes(
            read(address, 4)?
                .try_into()
                .map_err(|_| "short SD registry read")?,
        ))
    };
    let base = word(registry::L6_POINTER)?;
    if base < 0x8000_0000
        || base
            .checked_add(registry::site_offset(registry::MAX_SITES))
            .is_none_or(|v| v > 0x8420_0000)
    {
        return Err("invalid SD registry address".into());
    }
    if word(base)? != registry::MAGIC
        || site_index >= registry::MAX_SITES
        || site_index >= word(base + registry::SITE_COUNT_OFFSET)?
    {
        return Err("invalid SD registry identity or site".into());
    }
    let module = word(base + registry::MODULE_PTR_OFFSET)?;
    let length = word(base + registry::MODULE_LEN_OFFSET)?;
    let hook = module
        .checked_add(entry_offset)
        .ok_or("SD module entry overflow")?;
    if entry_offset >= length || word(base + registry::site_offset(site_index))? != hook | 1 {
        return Err("SD MIDI hook does not match loaded module and registry".into());
    }
    Ok(hook)
}
fn validate_l6_hook_binding(
    hook: u32,
    binding: &L6MidiBinding,
    mut read: impl FnMut(u32, usize) -> Result<Vec<u8>, String>,
) -> Result<(), String> {
    let target = match binding {
        L6MidiBinding::Firmware => hook,
        L6MidiBinding::Sd { dispatcher, .. } => *dispatcher,
    };
    let displacement = i64::from(target) - i64::from(firmware_api::l6::midi_in + 4);
    if target & 1 != 0 || !(-16_777_216..16_777_216).contains(&displacement) {
        return Err("invalid L6 MIDI branch target".into());
    }
    if read(firmware_api::l6::midi_in, 4)?
        != patch_core::branch(firmware_api::l6::midi_in, target, false)
    {
        return Err("installed L6 MIDI branch does not match selected patch".into());
    }
    if let L6MidiBinding::Sd {
        site_index,
        entry_offset,
        ..
    } = binding
    {
        if resolve_l6_sd_registry(*site_index, *entry_offset, &mut read)? != hook {
            return Err("SD MIDI hook does not match loaded module and registry".into());
        }
    }
    Ok(())
}
fn validate_l6_probe_guards(
    model: Model,
    mut read: impl FnMut(u32, usize) -> Result<Vec<u8>, String>,
) -> Result<(), String> {
    if model != Model::L6 {
        return Err("MIDI hook probe requires L6 1.10".into());
    }
    for (address, expected) in [
        (firmware_api::l6::idle_loop, IDLE_GUARD),
        (firmware_api::l6::midi_send, SENDER_GUARD),
    ] {
        let bytes = read(address, 64)?;
        if bytes.len() != 64 || format!("{:x}", Sha256::digest(&bytes)) != expected {
            return Err(format!(
                "unsupported L6 MIDI probe routine at {address:#010x}"
            ));
        }
    }
    Ok(())
}

trait Transport: Send {
    fn set_timeout(&mut self, timeout: Duration) -> Result<(), String>;
    fn read_register(&mut self, index: u32) -> Result<u32, String>;
    fn write_register(&mut self, index: u32, value: u32) -> Result<(), String>;
    fn read(&mut self, address: u32, length: usize) -> Result<Vec<u8>, String>;
    fn write(&mut self, address: u32, bytes: &[u8]) -> Result<(), String>;
    fn write_before(&mut self, address: u32, bytes: &[u8], deadline: Instant)
    -> Result<(), String>;
    fn breakpoint(&mut self, address: u32, enable: bool) -> Result<(), String>;
    fn resume(&mut self) -> Result<(), String>;
    fn poll_stop(&mut self, wait: Duration) -> Result<Option<String>, String>;
    fn interrupt(&mut self) -> Result<String, String>;
    fn step(&mut self) -> Result<String, String>;
    fn disconnect(&mut self) -> Result<(), String>;
}
impl Transport for Gdb {
    fn set_timeout(&mut self, timeout: Duration) -> Result<(), String> {
        Gdb::set_request_timeout(self, timeout).map_err(|e| e.to_string())
    }
    fn read_register(&mut self, index: u32) -> Result<u32, String> {
        Gdb::read_register(self, index).map_err(|e| e.to_string())
    }
    fn write_register(&mut self, index: u32, value: u32) -> Result<(), String> {
        Gdb::register(self, index, value).map_err(|e| e.to_string())
    }
    fn read(&mut self, address: u32, length: usize) -> Result<Vec<u8>, String> {
        Gdb::read(self, address, length).map_err(|e| e.to_string())
    }
    fn write(&mut self, address: u32, bytes: &[u8]) -> Result<(), String> {
        Gdb::write(self, address, bytes).map_err(|e| e.to_string())
    }
    fn write_before(
        &mut self,
        address: u32,
        bytes: &[u8],
        deadline: Instant,
    ) -> Result<(), String> {
        Gdb::write_before(self, address, bytes, deadline).map_err(|e| e.to_string())
    }
    fn breakpoint(&mut self, address: u32, enable: bool) -> Result<(), String> {
        Gdb::breakpoint(self, address, enable).map_err(|e| e.to_string())
    }
    fn resume(&mut self) -> Result<(), String> {
        Gdb::resume(self).map_err(|e| e.to_string())
    }
    fn poll_stop(&mut self, wait: Duration) -> Result<Option<String>, String> {
        Gdb::poll_stop(self, wait).map_err(|e| e.to_string())
    }
    fn interrupt(&mut self) -> Result<String, String> {
        Gdb::interrupt(self).map_err(|e| e.to_string())
    }
    fn step(&mut self) -> Result<String, String> {
        self.request("s").map_err(|e| e.to_string())
    }
    fn disconnect(&mut self) -> Result<(), String> {
        Gdb::disconnect(self).map_err(|e| e.to_string())
    }
}

#[derive(Clone)]
pub struct Controller {
    path: PathBuf,
    guest: Arc<dyn Guest>,
    unusable: Arc<AtomicBool>,
    retained_lease: Arc<Mutex<Option<l6max_diagnostics::gdb::Lease>>>,
}
impl Controller {
    pub(crate) fn new(path: PathBuf, qmp: QmpClient, model: Model) -> Self {
        Self {
            path,
            guest: Arc::new(MainGuest { qmp, model }),
            unusable: Arc::new(AtomicBool::new(false)),
            retained_lease: Arc::new(Mutex::new(None)),
        }
    }
    /// Acquire and stop MAIN within the caller's shared deadline. Failure
    /// after stopping retains ownership and requires a guest restart.
    pub fn begin_before(&self, deadline: Instant) -> Result<Session, String> {
        if self.unusable.load(Ordering::Acquire) {
            return Err("debugger is unusable; restart the guest".into());
        }
        if Instant::now() >= deadline {
            return Err("debugger acquisition deadline exceeded".into());
        }
        let lease =
            l6max_diagnostics::gdb::Lease::acquire(&self.path).map_err(|e| e.to_string())?;
        if self.unusable.load(Ordering::Acquire) {
            return Err("debugger is unusable; restart the guest".into());
        }
        let connection_lease = lease.try_clone().map_err(|e| e.to_string())?;
        let running = self.guest.running_before(deadline)?;
        if let Err(error) = self.guest.stop_before(deadline) {
            self.unusable.store(true, Ordering::Release);
            *self
                .retained_lease
                .lock()
                .unwrap_or_else(|e| e.into_inner()) = Some(lease);
            let stop = self
                .guest
                .stop_before(Instant::now() + Duration::from_secs(3));
            return Err(match stop {
                Ok(()) => {
                    format!("debugger acquisition failed: {error}; MAIN stopped; restart the guest")
                }
                Err(stop) => format!(
                    "debugger acquisition failed: {error}; could not verify MAIN stopped: {stop}; restart the guest"
                ),
            });
        }
        let gdb = match Gdb::connect_leased_before(connection_lease, deadline) {
            Ok(gdb) => gdb,
            Err(error) => {
                self.unusable.store(true, Ordering::Release);
                *self
                    .retained_lease
                    .lock()
                    .unwrap_or_else(|e| e.into_inner()) = Some(lease);
                let stop = self
                    .guest
                    .stop_before(Instant::now() + Duration::from_secs(3));
                return Err(match stop {
                    Ok(()) => {
                        format!("GDB connection failed: {error}; MAIN stopped; restart the guest")
                    }
                    Err(stop) => format!(
                        "GDB connection failed: {error}; could not verify MAIN stopped: {stop}; restart the guest"
                    ),
                });
            }
        };
        Ok(Session::new(
            Box::new(gdb),
            self.guest.clone(),
            running,
            self.unusable.clone(),
            Some(lease),
            self.retained_lease.clone(),
        ))
    }

    /// Acquire the cross-process GDB lease and pause MAIN. The prior running
    /// state is restored on successful close; failure is fail-closed.
    pub fn begin(&self) -> Result<Session, String> {
        if self.unusable.load(Ordering::Acquire) {
            return Err("debugger is unusable after failed cleanup; restart the guest".into());
        }
        let lease =
            l6max_diagnostics::gdb::Lease::acquire(&self.path).map_err(|e| e.to_string())?;
        if self.unusable.load(Ordering::Acquire) {
            return Err("debugger is unusable after failed cleanup; restart the guest".into());
        }
        let connection_lease = lease.try_clone().map_err(|e| e.to_string())?;
        let running = self.guest.running()?;
        if let Err(error) = self.guest.stop() {
            self.unusable.store(true, Ordering::Release);
            *self
                .retained_lease
                .lock()
                .unwrap_or_else(|e| e.into_inner()) = Some(lease);
            let stopped = self.guest.stop().err();
            return Err(format!(
                "could not verify MAIN stopped: {error}; retry stop: {stopped:?}; restart the guest"
            ));
        }
        let gdb = match Gdb::connect_leased(connection_lease) {
            Ok(gdb) => gdb,
            Err(error) => {
                let restored = if running {
                    self.guest.resume()
                } else {
                    self.guest.stop()
                };
                if let Err(restore) = restored {
                    self.unusable.store(true, Ordering::Release);
                    *self
                        .retained_lease
                        .lock()
                        .unwrap_or_else(|e| e.into_inner()) = Some(lease);
                    let stopped = self.guest.stop().err();
                    return Err(format!(
                        "GDB connection failed: {error}; restore failed: {restore}; stop: {stopped:?}; restart the guest"
                    ));
                }
                return Err(format!("GDB connection failed: {error}"));
            }
        };
        Ok(Session::new(
            Box::new(gdb),
            self.guest.clone(),
            running,
            self.unusable.clone(),
            Some(lease),
            self.retained_lease.clone(),
        ))
    }
}

struct L6MidiContext {
    expected_hook: u32,
    last_return: Option<u32>,
}

pub struct Session {
    transport: Option<Box<dyn Transport>>,
    guest: Arc<dyn Guest>,
    was_running: bool,
    poisoned: bool,
    breakpoints: BTreeSet<u32>,
    scratch: Option<Vec<u8>>,
    l6_midi: Option<L6MidiContext>,
    unusable: Arc<AtomicBool>,
    lease: Option<l6max_diagnostics::gdb::Lease>,
    retained_lease: Arc<Mutex<Option<l6max_diagnostics::gdb::Lease>>>,
}
impl Session {
    fn new(
        transport: Box<dyn Transport>,
        guest: Arc<dyn Guest>,
        was_running: bool,
        unusable: Arc<AtomicBool>,
        lease: Option<l6max_diagnostics::gdb::Lease>,
        retained_lease: Arc<Mutex<Option<l6max_diagnostics::gdb::Lease>>>,
    ) -> Self {
        Self {
            transport: Some(transport),
            guest,
            was_running,
            poisoned: false,
            breakpoints: BTreeSet::new(),
            scratch: None,
            l6_midi: None,
            unusable,
            lease,
            retained_lease,
        }
    }
    fn transact<T>(
        &mut self,
        f: impl FnOnce(&mut dyn Transport) -> Result<T, String>,
    ) -> Result<T, String> {
        if self.poisoned {
            return Err("debug transport is desynchronized".into());
        }
        let result = f(self
            .transport
            .as_deref_mut()
            .ok_or("debug session is closed")?);
        if result.is_err() {
            self.poisoned = true;
        }
        result
    }
    fn timed<T>(
        &mut self,
        deadline: Instant,
        cancelled: &mut impl FnMut() -> bool,
        f: impl FnOnce(&mut dyn Transport) -> Result<T, String>,
    ) -> Result<T, String> {
        if cancelled() {
            return Err("L6 MIDI probe cancelled".into());
        }
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .filter(|d| !d.is_zero())
            .ok_or("L6 MIDI probe deadline exceeded")?;
        let result = self.transact(|g| {
            g.set_timeout(remaining.min(Duration::from_secs(5)))?;
            let result = f(g);
            let reset = g.set_timeout(Duration::from_secs(5));
            match (result, reset) {
                (Ok(value), Ok(())) => Ok(value),
                (Err(error), _) | (_, Err(error)) => Err(error),
            }
        });
        let value = result?;
        if cancelled() {
            return Err("L6 MIDI probe cancelled".into());
        }
        if Instant::now() >= deadline {
            return Err("L6 MIDI probe deadline exceeded".into());
        }
        Ok(value)
    }

    /// Write the guarded SIGNAL accumulators under one caller-owned deadline.
    pub fn signal_write_before(
        &mut self,
        values: [f32; 12],
        deadline: Instant,
        mut cancelled: impl FnMut() -> bool,
    ) -> Result<Observation, String> {
        if self.l6_midi.is_some() {
            return Err("L6 MIDI injection is already active".into());
        }
        if values
            .iter()
            .any(|value| !value.is_finite() || *value < 0.0 || *value > 40_000_000.0)
        {
            return Err("invalid SIGNAL accumulator value".into());
        }
        if cancelled() || Instant::now() >= deadline {
            return Err("SIGNAL write cancelled or deadline exceeded".into());
        }
        self.guest.guard_l6max_signal(deadline)?;
        if cancelled() || Instant::now() >= deadline {
            return Err("SIGNAL write cancelled or deadline exceeded".into());
        }
        let bytes: Vec<u8> = values.into_iter().flat_map(f32::to_le_bytes).collect();
        self.transact(|g| g.write_before(SIGNAL_ACCUMULATORS, &bytes, deadline))?;
        if cancelled() || Instant::now() >= deadline {
            return Err("SIGNAL write cancelled or deadline exceeded".into());
        }
        Ok(Observation::SignalWritten {
            address: SIGNAL_ACCUMULATORS,
        })
    }

    pub fn execute(&mut self, command: Command) -> Result<Observation, String> {
        self.execute_cancellable(command, || false)
    }

    /// Execute one command, checking cancellation while the target runs.
    pub fn execute_cancellable(
        &mut self,
        command: Command,
        mut cancelled: impl FnMut() -> bool,
    ) -> Result<Observation, String> {
        if self.l6_midi.is_some()
            && matches!(
                command,
                Command::L6maxSignalWrite { .. }
                    | Command::L6ScratchRead
                    | Command::L6ScratchWrite { .. }
                    | Command::L6MidiInject { .. }
            )
        {
            return Err("L6 MIDI injection is already active".into());
        }
        match command {
            Command::L6maxSignalWrite { values, timeout_ms } => {
                let deadline = l6_probe_deadline(timeout_ms)?;
                self.signal_write_before(values, deadline, cancelled)
            }
            Command::L6MidiResolveSd {
                dispatcher,
                site_index,
                entry_offset,
                timeout_ms,
            } => {
                let deadline = l6_probe_deadline(timeout_ms)?;
                if cancelled() {
                    return Err("L6 MIDI probe cancelled".into());
                }
                let hook = self.guest.resolve_l6_sd_hook(
                    deadline,
                    dispatcher,
                    site_index,
                    entry_offset,
                )?;
                if cancelled() || Instant::now() >= deadline {
                    return Err("L6 MIDI probe cancelled or deadline exceeded".into());
                }
                Ok(Observation::L6MidiResolved { hook })
            }
            Command::Register { index } => {
                if index > 15 {
                    return Err("register index must be 0..15".into());
                }
                Ok(Observation::Register {
                    index,
                    value: self.transact(|g| g.read_register(index))?,
                })
            }
            Command::Read { address, length } => {
                let end = address
                    .checked_add(u32::try_from(length).map_err(|_| "invalid memory length")?)
                    .ok_or("address overflow")?;
                if length == 0 || length > 4096 || address < 0x8000_0000 || end > 0x8400_0000 {
                    return Err("read outside bounded MAIN SDRAM".into());
                }
                Ok(Observation::Memory {
                    address,
                    bytes: self.transact(|g| g.read(address, length))?,
                })
            }
            Command::AddBreakpoint { address } => {
                if address & 1 != 0 || !(0x8000_0000..0x8420_0000).contains(&address) {
                    return Err("breakpoint must be an aligned MAIN SDRAM address".into());
                }
                if self.breakpoints.contains(&address) {
                    return Err("breakpoint already owned".into());
                }
                if self.breakpoints.len() >= MAX_BREAKPOINTS {
                    return Err("too many debug breakpoints".into());
                }
                self.transact(|g| g.breakpoint(address, true))?;
                self.breakpoints.insert(address);
                Ok(Observation::Breakpoint {
                    address,
                    installed: true,
                })
            }
            Command::RemoveBreakpoint { address } => {
                if !self.breakpoints.contains(&address) {
                    return Err("breakpoint is not owned by this session".into());
                }
                self.transact(|g| g.breakpoint(address, false))?;
                self.breakpoints.remove(&address);
                Ok(Observation::Breakpoint {
                    address,
                    installed: false,
                })
            }
            Command::Continue { timeout_ms } => {
                if timeout_ms == 0 || timeout_ms > MAX_CONTINUE_MS {
                    return Err("continue timeout must be 1..60000 ms".into());
                }
                if self.breakpoints.is_empty() {
                    return Err("continue requires an owned breakpoint".into());
                }
                let deadline = std::time::Instant::now() + Duration::from_millis(timeout_ms);
                self.transact(|g| g.resume())?;
                loop {
                    let now = std::time::Instant::now();
                    if cancelled() || now >= deadline {
                        self.transact(|g| g.interrupt().and_then(validate_stop))?;
                        return Err(if now >= deadline {
                            "debug continue deadline exceeded"
                        } else {
                            "debug session cancelled"
                        }
                        .into());
                    }
                    let wait = deadline
                        .saturating_duration_since(now)
                        .min(Duration::from_millis(50));
                    if let Some(reply) = self.transact(|g| g.poll_stop(wait))? {
                        return Ok(Observation::Stopped {
                            reply: validate_stop(reply).map_err(|error| {
                                self.poisoned = true;
                                error
                            })?,
                        });
                    }
                }
            }
            Command::Step => Ok(Observation::Stopped {
                reply: self.transact(|g| g.step().and_then(validate_stop))?,
            }),
            Command::L6ScratchRead => {
                self.guest.guard_l6_scratch()?;
                Ok(Observation::Scratch {
                    address: SCRATCH,
                    bytes: self.transact(|g| g.read(SCRATCH, SCRATCH_LEN))?,
                })
            }
            Command::L6ScratchWrite { bytes } => {
                self.guest.guard_l6_scratch()?;
                if self.scratch.is_none() {
                    self.scratch = Some(self.transact(|g| g.read(SCRATCH, SCRATCH_LEN))?);
                }
                self.transact(|g| g.write(SCRATCH, &bytes))?;
                Ok(Observation::Scratch {
                    address: SCRATCH,
                    bytes: bytes.to_vec(),
                })
            }
            Command::L6MidiInject {
                expected_hook,
                binding,
                timeout_ms,
                leave_stopped,
            } => {
                let deadline = l6_probe_deadline(timeout_ms)?;
                if !leave_stopped {
                    return Err("L6 MIDI injection requires explicit leave_stopped=true".into());
                }
                if cancelled() {
                    return Err("L6 MIDI probe cancelled".into());
                }
                use firmware_api::l6::{idle_loop, midi_in, midi_send};
                if expected_hook & 1 != 0
                    || !(0x8000_1000..0x8420_0000).contains(&expected_hook)
                    || !self.breakpoints.contains(&idle_loop)
                    || !self.breakpoints.contains(&midi_send)
                    || !self.breakpoints.contains(&expected_hook)
                    || expected_hook == idle_loop
                    || expected_hook == midi_send
                {
                    return Err("L6 MIDI injection requires owned idle, sender and distinct hook breakpoints".into());
                }
                let config = self
                    .guest
                    .guard_l6_midi_probe(deadline, expected_hook, &binding)?;
                if cancelled() || Instant::now() >= deadline {
                    return Err("L6 MIDI probe cancelled or deadline exceeded".into());
                }
                let idle_pc = self.timed(deadline, &mut cancelled, |g| g.read_register(15))? & !1;
                let idle_sp = self.timed(deadline, &mut cancelled, |g| g.read_register(13))?;
                let idle_xpsr = self.timed(deadline, &mut cancelled, |g| g.read_register(XPSR))?;
                if idle_pc != idle_loop || idle_xpsr & 0x1ff != 0 || idle_xpsr & (1 << 24) == 0 {
                    return Err("L6 MIDI injection requires idle task thread mode".into());
                }
                let before =
                    self.timed(deadline, &mut cancelled, |g| g.read(SCRATCH, SCRATCH_LEN))?;
                if before.len() != SCRATCH_LEN {
                    self.poisoned = true;
                    return Err("short scratch read".into());
                }
                if self.scratch.is_none() {
                    self.scratch = Some(before.clone());
                }
                self.l6_midi = Some(L6MidiContext {
                    expected_hook,
                    last_return: None,
                });
                let mut event = [0u8; SCRATCH_LEN];
                event[4..7].copy_from_slice(&[0xb0 | config.channel, 127, 127]);
                self.timed(deadline, &mut cancelled, |g| g.write(SCRATCH, &event))?;
                self.timed(deadline, &mut cancelled, |g| g.write_register(0, SCRATCH))?;
                self.timed(deadline, &mut cancelled, |g| {
                    g.write_register(14, idle_loop | 1)
                })?;
                self.timed(deadline, &mut cancelled, |g| g.write_register(15, midi_in))?;
                Ok(Observation::L6MidiInjected {
                    expected_hook,
                    event: event.to_vec(),
                    config,
                    idle_sp,
                    idle_xpsr,
                    requires_restart: true,
                })
            }
            Command::L6MidiHook { timeout_ms } => {
                let deadline = l6_probe_deadline(timeout_ms)?;
                let expected = self
                    .l6_midi
                    .as_ref()
                    .ok_or("L6 MIDI injection is not active")?
                    .expected_hook;
                let pc = self.timed(deadline, &mut cancelled, |g| g.read_register(15))? & !1;
                if pc != expected {
                    return Err("L6 MIDI hook did not reach the expected target".into());
                }
                let event = self.timed(deadline, &mut cancelled, |g| g.read(SCRATCH, 7))?;
                if event.len() != 7 {
                    return Err("short L6 MIDI hook event".into());
                }
                Ok(Observation::L6MidiHook { pc, event })
            }
            Command::L6MidiStep { timeout_ms } => {
                if self.l6_midi.is_none() {
                    return Err("L6 MIDI injection is not active".into());
                }
                let deadline = l6_probe_deadline(timeout_ms)?;
                let reply = self.timed(deadline, &mut cancelled, |g| {
                    g.step().and_then(validate_stop)
                })?;
                Ok(Observation::Stopped { reply })
            }
            Command::L6MidiBreakpoint {
                address,
                enabled,
                timeout_ms,
            } => {
                if self.l6_midi.is_none() {
                    return Err("L6 MIDI injection is not active".into());
                }
                if address & 1 != 0 || !(0x8000_0000..0x8420_0000).contains(&address) {
                    return Err("breakpoint must be an aligned MAIN SDRAM address".into());
                }
                if enabled == self.breakpoints.contains(&address) {
                    return Err("breakpoint ownership mismatch".into());
                }
                if enabled && self.breakpoints.len() >= MAX_BREAKPOINTS {
                    return Err("too many debug breakpoints".into());
                }
                let deadline = l6_probe_deadline(timeout_ms)?;
                self.timed(deadline, &mut cancelled, |g| g.breakpoint(address, enabled))?;
                if enabled {
                    self.breakpoints.insert(address);
                } else {
                    self.breakpoints.remove(&address);
                }
                Ok(Observation::Breakpoint {
                    address,
                    installed: enabled,
                })
            }
            Command::L6MidiSender { timeout_ms } => {
                let deadline = l6_probe_deadline(timeout_ms)?;
                if self.l6_midi.is_none() {
                    return Err("L6 MIDI injection is not active".into());
                }
                let pc = self.timed(deadline, &mut cancelled, |g| g.read_register(15))? & !1;
                if pc != firmware_api::l6::midi_send {
                    return Err(format!("not stopped at L6 MIDI sender: {pc:#x}"));
                }
                let buffer = self.timed(deadline, &mut cancelled, |g| g.read_register(0))?;
                let length = self.timed(deadline, &mut cancelled, |g| g.read_register(1))?;
                let route = self.timed(deadline, &mut cancelled, |g| g.read_register(2))?;
                let return_pc = self.timed(deadline, &mut cancelled, |g| g.read_register(14))? & !1;
                let bytes = if length == 3 {
                    let end = buffer.checked_add(3).ok_or("MIDI sender buffer overflow")?;
                    if !((buffer >= 0x2000_0000 && end <= 0x2030_0000)
                        || (buffer >= 0x8000_0000 && end <= 0x8420_0000))
                    {
                        return Err("L6 MIDI sender buffer is outside mapped RAM".into());
                    }
                    Some(self.timed(deadline, &mut cancelled, |g| g.read(buffer, 3))?)
                } else {
                    None
                };
                self.l6_midi.as_mut().unwrap().last_return = Some(return_pc);
                Ok(Observation::L6MidiSender {
                    pc,
                    buffer,
                    length,
                    route,
                    return_pc,
                    bytes,
                })
            }
            Command::L6MidiFinish { timeout_ms } => {
                let deadline = l6_probe_deadline(timeout_ms)?;
                let context = self
                    .l6_midi
                    .as_ref()
                    .ok_or("L6 MIDI injection is not active")?;
                let expected_return = context
                    .last_return
                    .ok_or("no L6 MIDI sender was observed")?;
                let scratch = self
                    .scratch
                    .as_ref()
                    .ok_or("missing original debug scratch")?
                    .clone();
                let pc = self.timed(deadline, &mut cancelled, |g| g.read_register(15))? & !1;
                if pc != expected_return {
                    return Err("the last L6 MIDI sender has not returned".into());
                }
                self.timed(deadline, &mut cancelled, |g| g.write(SCRATCH, &scratch))?;
                for address in self.breakpoints.clone() {
                    self.timed(deadline, &mut cancelled, |g| g.breakpoint(address, false))?;
                    self.breakpoints.remove(&address);
                }
                self.timed(deadline, &mut cancelled, |g| g.disconnect())?;
                if cancelled() || Instant::now() >= deadline {
                    return Err("L6 MIDI finish cancelled or deadline exceeded".into());
                }
                self.guest.stop_before(deadline)?;
                if cancelled() || Instant::now() >= deadline {
                    return Err("L6 MIDI finish cancelled or deadline exceeded".into());
                }
                // The injected execution cannot yet be proven to restore all
                // CPU/scheduler state. Keep MAIN paused and reserve the lease
                // until the engine is destroyed, even after a successful probe.
                self.unusable.store(true, Ordering::Release);
                if let Some(lease) = self.lease.take() {
                    *self
                        .retained_lease
                        .lock()
                        .unwrap_or_else(|e| e.into_inner()) = Some(lease);
                }
                self.transport.take();
                self.l6_midi = None;
                Ok(Observation::L6MidiFinished {
                    requires_restart: true,
                })
            }
            Command::Close => {
                self.close()?;
                Ok(Observation::Closed)
            }
        }
    }
    /// Close a bounded, write-only SIGNAL session. No breakpoint or scratch
    /// restoration may be pending. An uncertain restore retains the lease.
    pub fn close_signal_before(&mut self, deadline: Instant) -> Result<(), String> {
        if self.transport.is_none() {
            return Ok(());
        }
        if self.poisoned
            || self.scratch.is_some()
            || !self.breakpoints.is_empty()
            || self.l6_midi.is_some()
        {
            self.unusable.store(true, Ordering::Release);
            if let Some(lease) = self.lease.take() {
                *self
                    .retained_lease
                    .lock()
                    .unwrap_or_else(|e| e.into_inner()) = Some(lease);
            }
            let stop = self
                .guest
                .stop_before(Instant::now() + Duration::from_secs(3));
            self.transport.take();
            return Err(match stop {
                Ok(()) => "debugger state uncertain; MAIN stopped; restart the guest".into(),
                Err(error) => format!(
                    "debugger state uncertain; could not verify MAIN stopped: {error}; restart the guest"
                ),
            });
        }
        let disconnected = self.transport.as_deref_mut().unwrap().disconnect();
        self.transport.take();
        let restore = if disconnected.is_err() {
            Err("GDB disconnect failed".to_owned())
        } else if self.was_running {
            self.guest.resume_before(deadline)
        } else {
            self.guest.stop_before(deadline)
        };
        if let Err(error) = restore {
            self.unusable.store(true, Ordering::Release);
            if let Some(lease) = self.lease.take() {
                *self
                    .retained_lease
                    .lock()
                    .unwrap_or_else(|e| e.into_inner()) = Some(lease);
            }
            let stop = self
                .guest
                .stop_before(Instant::now() + Duration::from_secs(3));
            return Err(match stop {
                Ok(()) => format!(
                    "bounded debugger cleanup failed: {error}; MAIN stopped; restart the guest"
                ),
                Err(stop) => format!(
                    "bounded debugger cleanup failed: {error}; could not verify MAIN stopped: {stop}; restart the guest"
                ),
            });
        }
        self.lease.take();
        Ok(())
    }

    /// Restore leased scratch and remove owned breakpoints before resuming.
    /// If cleanup is uncertain, leave MAIN stopped rather than resume it.
    pub fn close(&mut self) -> Result<(), String> {
        if self.transport.is_none() {
            return Ok(());
        }
        let mut errors = Vec::new();
        if self.poisoned {
            errors.push("debug transport desynchronized".to_owned());
        }
        if self.l6_midi.is_some() {
            errors.push("L6 MIDI injection did not return and restore context".to_owned());
        }
        if !self.poisoned && self.l6_midi.is_none() {
            if let Some(bytes) = self.scratch.take() {
                if let Err(error) = self.transact(|g| g.write(SCRATCH, &bytes)) {
                    errors.push(format!("restore scratch: {error}"));
                }
            }
            if !self.poisoned {
                for address in self.breakpoints.clone() {
                    if let Err(error) = self.transact(|g| g.breakpoint(address, false)) {
                        errors.push(format!("remove breakpoint {address:#x}: {error}"));
                        break;
                    }
                    self.breakpoints.remove(&address);
                }
            }
        }
        // Retain the transport's cross-process lease through detach and QMP
        // restoration, so another client cannot acquire during cleanup.
        if let Some(transport) = self.transport.as_deref_mut() {
            if let Err(error) = transport.disconnect() {
                errors.push(format!("detach GDB: {error}"));
            }
        }
        if errors.is_empty() {
            let restored = if self.was_running {
                self.guest.resume()
            } else {
                self.guest.stop()
            };
            if let Err(error) = restored {
                errors.push(format!("restore MAIN run state: {error}"));
            }
        }
        if !errors.is_empty() {
            self.unusable.store(true, Ordering::Release);
            if let Some(lease) = self.lease.take() {
                *self
                    .retained_lease
                    .lock()
                    .unwrap_or_else(|e| e.into_inner()) = Some(lease);
            }
            if let Err(error) = self.guest.stop() {
                errors.push(format!("could not verify MAIN stopped: {error}"));
            }
            errors.push("debugger unusable; restart the guest before resuming".into());
        }
        self.transport.take();
        self.lease.take();
        if !errors.is_empty() {
            return Err(errors.join("; "));
        }
        Ok(())
    }
}
impl Drop for Session {
    fn drop(&mut self) {
        if let Err(error) = self.close() {
            tracing::error!(%error, "debug session cleanup failed");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[derive(Default)]
    struct FakeState {
        operations: Vec<String>,
        scratch: Vec<u8>,
        registers: [u32; 26],
        fail: Option<&'static str>,
        guarded: bool,
        pending: bool,
        running_after_cont: bool,
        stop_before_calls: usize,
        cancel_on_guard: Option<Arc<AtomicBool>>,
    }
    struct FakeGuest(Arc<Mutex<FakeState>>);
    impl Guest for FakeGuest {
        fn running(&self) -> Result<bool, String> {
            Ok(true)
        }
        fn running_before(&self, _deadline: Instant) -> Result<bool, String> {
            let mut state = self.0.lock().unwrap();
            state.operations.push("running-before".into());
            if state.fail == Some("running-before") {
                Err("timeout".into())
            } else {
                Ok(true)
            }
        }
        fn resume_before(&self, _deadline: Instant) -> Result<(), String> {
            let mut state = self.0.lock().unwrap();
            state.operations.push("resume-before".into());
            state.running_after_cont = true;
            if matches!(state.fail, Some("resume-before" | "resume-and-stop")) {
                Err("timeout".into())
            } else {
                Ok(())
            }
        }
        fn stop(&self) -> Result<(), String> {
            let mut state = self.0.lock().unwrap();
            state.operations.push("stop".into());
            state.running_after_cont = false;
            Ok(())
        }
        fn resume(&self) -> Result<(), String> {
            self.0.lock().unwrap().operations.push("resume".into());
            Ok(())
        }
        fn stop_before(&self, deadline: Instant) -> Result<(), String> {
            let mut state = self.0.lock().unwrap();
            state.stop_before_calls += 1;
            if Instant::now() >= deadline
                || matches!(state.fail, Some("stop-before" | "resume-and-stop"))
                || (state.fail == Some("first-stop") && state.stop_before_calls == 1)
            {
                return Err("timeout".into());
            }
            drop(state);
            self.stop()
        }
        fn guard_l6_midi_probe(
            &self,
            _deadline: Instant,
            _hook: u32,
            _binding: &L6MidiBinding,
        ) -> Result<midi_config::Config, String> {
            self.guard_l6_scratch()?;
            Ok(midi_config::Config {
                revision_guard: "fake".into(),
                atomic: false,
                channel: 0,
                cc_map: vec![0xff; 54],
            })
        }
        fn resolve_l6_sd_hook(&self, _: Instant, _: u32, _: u32, _: u32) -> Result<u32, String> {
            self.guard_l6_scratch()?;
            Ok(0x800b_6b14)
        }
        fn guard_l6max_signal(&self, _deadline: Instant) -> Result<(), String> {
            let state = self.0.lock().unwrap();
            if let Some(flag) = &state.cancel_on_guard {
                flag.store(true, Ordering::Release);
            }
            if state.guarded {
                Ok(())
            } else {
                Err("unsupported revision".into())
            }
        }
        fn guard_l6_scratch(&self) -> Result<(), String> {
            if self.0.lock().unwrap().guarded {
                Ok(())
            } else {
                Err("unsupported revision".into())
            }
        }
    }
    struct FakeTransport(Arc<Mutex<FakeState>>);
    impl FakeTransport {
        fn record(&self, name: &'static str) -> Result<(), String> {
            let mut state = self.0.lock().unwrap();
            state.operations.push(name.into());
            if state.fail == Some(name) {
                Err(format!("{name} failed"))
            } else {
                Ok(())
            }
        }
    }
    impl Transport for FakeTransport {
        fn set_timeout(&mut self, _timeout: Duration) -> Result<(), String> {
            Ok(())
        }
        fn read_register(&mut self, index: u32) -> Result<u32, String> {
            self.record("register")?;
            Ok(self.0.lock().unwrap().registers[index as usize])
        }
        fn write_register(&mut self, index: u32, value: u32) -> Result<(), String> {
            self.record("write-register")?;
            self.0.lock().unwrap().registers[index as usize] = value;
            Ok(())
        }
        fn read(&mut self, address: u32, length: usize) -> Result<Vec<u8>, String> {
            self.record("read")?;
            if address == SCRATCH {
                Ok(self.0.lock().unwrap().scratch[..length].to_vec())
            } else {
                Ok(vec![0; length])
            }
        }
        fn write(&mut self, _: u32, bytes: &[u8]) -> Result<(), String> {
            self.record("write")?;
            self.0.lock().unwrap().scratch = bytes.to_vec();
            Ok(())
        }
        fn write_before(
            &mut self,
            address: u32,
            bytes: &[u8],
            deadline: Instant,
        ) -> Result<(), String> {
            if Instant::now() >= deadline {
                return Err("timeout".into());
            }
            self.write(address, bytes)
        }
        fn breakpoint(&mut self, _: u32, enable: bool) -> Result<(), String> {
            self.record(if enable { "add" } else { "remove" })
        }
        fn resume(&mut self) -> Result<(), String> {
            self.record("continue")
        }
        fn poll_stop(&mut self, _: Duration) -> Result<Option<String>, String> {
            self.record("poll")?;
            if self.0.lock().unwrap().pending {
                std::thread::sleep(Duration::from_millis(5));
                Ok(None)
            } else {
                Ok(Some("S05".into()))
            }
        }
        fn interrupt(&mut self) -> Result<String, String> {
            self.record("interrupt")?;
            Ok("S05".into())
        }
        fn step(&mut self) -> Result<String, String> {
            self.record("step")?;
            Ok("S05".into())
        }
        fn disconnect(&mut self) -> Result<(), String> {
            self.record("disconnect")
        }
    }
    fn fake(running: bool, guarded: bool) -> (Session, Arc<Mutex<FakeState>>) {
        let state = Arc::new(Mutex::new(FakeState {
            scratch: vec![9; SCRATCH_LEN],
            registers: [42; 26],
            guarded,
            ..Default::default()
        }));
        let guest = Arc::new(FakeGuest(state.clone()));
        (
            Session::new(
                Box::new(FakeTransport(state.clone())),
                guest,
                running,
                Arc::new(AtomicBool::new(false)),
                None,
                Arc::new(Mutex::new(None)),
            ),
            state,
        )
    }
    #[test]
    fn bounded_acquisition_fails_closed_when_qmp_stop_stalls() {
        use std::os::unix::net::UnixListener;
        let dir = std::env::temp_dir().join(format!(
            "l6-debug-acquire-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("stub.sock");
        let listener = UnixListener::bind(&path).unwrap();
        let state = Arc::new(Mutex::new(FakeState {
            fail: Some("stop-before"),
            ..Default::default()
        }));
        let unusable = Arc::new(AtomicBool::new(false));
        let retained = Arc::new(Mutex::new(None));
        let controller = Controller {
            path,
            guest: Arc::new(FakeGuest(state.clone())),
            unusable: unusable.clone(),
            retained_lease: retained.clone(),
        };
        let error = match controller.begin_before(Instant::now() + Duration::from_millis(50)) {
            Ok(_) => panic!("unexpected acquisition"),
            Err(error) => error,
        };
        assert!(error.contains("could not verify MAIN stopped"));
        assert_eq!(state.lock().unwrap().stop_before_calls, 2);
        assert!(unusable.load(Ordering::Acquire));
        assert!(retained.lock().unwrap().is_some());
        assert!(
            !state
                .lock()
                .unwrap()
                .operations
                .contains(&"resume".to_owned())
        );
        drop(controller);
        drop(retained);
        drop(listener);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn bounded_acquisition_retries_a_timed_out_stop_with_emergency_budget() {
        use std::os::unix::net::UnixListener;
        let dir = std::env::temp_dir().join(format!(
            "l6-debug-recovery-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("stub.sock");
        let listener = UnixListener::bind(&path).unwrap();
        let state = Arc::new(Mutex::new(FakeState {
            fail: Some("first-stop"),
            running_after_cont: true,
            ..Default::default()
        }));
        let unusable = Arc::new(AtomicBool::new(false));
        let retained = Arc::new(Mutex::new(None));
        let controller = Controller {
            path,
            guest: Arc::new(FakeGuest(state.clone())),
            unusable: unusable.clone(),
            retained_lease: retained.clone(),
        };
        let error = match controller.begin_before(Instant::now() + Duration::from_millis(50)) {
            Ok(_) => panic!("unexpected acquisition"),
            Err(error) => error,
        };
        assert!(error.contains("MAIN stopped"));
        assert!(!state.lock().unwrap().running_after_cont);
        assert!(unusable.load(Ordering::Acquire));
        assert!(retained.lock().unwrap().is_some());
        drop(controller);
        drop(retained);
        drop(listener);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn bounded_signal_close_reports_when_emergency_stop_cannot_be_verified() {
        let (mut session, state) = fake(true, true);
        state.lock().unwrap().fail = Some("resume-and-stop");
        let error = session
            .close_signal_before(Instant::now() + Duration::from_secs(1))
            .unwrap_err();
        assert!(error.contains("could not verify MAIN stopped"));
        assert!(state.lock().unwrap().running_after_cont);
        assert!(session.unusable.load(Ordering::Acquire));
    }

    #[test]
    fn bounded_signal_close_marks_uncertain_restore_unusable_and_stops() {
        let (mut session, state) = fake(true, true);
        state.lock().unwrap().fail = Some("resume-before");
        assert!(
            session
                .close_signal_before(Instant::now() + Duration::from_secs(1))
                .is_err()
        );
        let state = state.lock().unwrap();
        assert!(state.operations.contains(&"resume-before".into()));
        assert!(state.operations.contains(&"stop".into()));
        assert!(!state.running_after_cont);
        assert!(!state.operations.contains(&"resume".into()));
    }
    #[test]
    fn signal_write_does_not_mutate_after_cancellation_during_guard() {
        let (mut session, state) = fake(true, true);
        let cancelled = Arc::new(AtomicBool::new(false));
        state.lock().unwrap().cancel_on_guard = Some(cancelled.clone());
        assert!(
            session
                .signal_write_before([1.0; 12], Instant::now() + Duration::from_secs(1), || {
                    cancelled.load(Ordering::Acquire)
                })
                .is_err()
        );
        assert!(!state.lock().unwrap().operations.contains(&"write".into()));
    }

    #[test]
    fn signal_write_reuses_absolute_deadline_after_delayed_acquisition() {
        let (mut session, state) = fake(true, true);
        let deadline = Instant::now();
        assert!(
            session
                .signal_write_before([1.0; 12], deadline, || false)
                .is_err()
        );
        assert!(!state.lock().unwrap().operations.contains(&"write".into()));
    }

    #[test]
    fn signal_guard_checks_exact_recovered_routine_and_model() {
        let mut code = [
            0x10, 0xb5, 0x04, 0x46, 0x10, 0xf0, 0xec, 0xfc, 0x4e, 0xf2, 0x00, 0x41, 0x46, 0xf6,
            0xe0, 0x42, 0xc2, 0xf2, 0x00, 0x01, 0x88, 0x18, 0x8b, 0x58, 0x23, 0x60, 0x43, 0x68,
            0x63, 0x60, 0x83, 0x68,
        ];
        assert!(validate_signal_guard(Model::L6max, &code).is_ok());
        assert!(validate_signal_guard(Model::L6, &code).is_err());
        code[0] ^= 1;
        assert!(validate_signal_guard(Model::L6max, &code).is_err());
    }
    #[test]
    fn signal_write_is_guarded_bounded_and_fails_closed_on_write_error() {
        let (mut unguarded, state) = fake(true, false);
        assert!(
            unguarded
                .execute(Command::L6maxSignalWrite {
                    values: [1.0; 12],
                    timeout_ms: 1000
                })
                .is_err()
        );
        assert!(
            !state
                .lock()
                .unwrap()
                .operations
                .contains(&"write".to_owned())
        );
        let (mut bad, state) = fake(true, true);
        assert!(
            bad.execute(Command::L6maxSignalWrite {
                values: [f32::NAN; 12],
                timeout_ms: 1000
            })
            .is_err()
        );
        assert!(
            !state
                .lock()
                .unwrap()
                .operations
                .contains(&"write".to_owned())
        );
        let (mut failed, state) = fake(true, true);
        state.lock().unwrap().fail = Some("write");
        assert!(
            failed
                .execute(Command::L6maxSignalWrite {
                    values: [1.0; 12],
                    timeout_ms: 1000
                })
                .is_err()
        );
        assert!(failed.close().is_err());
        assert!(
            !state
                .lock()
                .unwrap()
                .operations
                .contains(&"resume".to_owned())
        );
    }
    fn prepared_midi() -> (Session, Arc<Mutex<FakeState>>, u32) {
        let (mut session, state) = fake(true, true);
        let hook = 0x800b_6b14;
        for address in [
            firmware_api::l6::idle_loop,
            firmware_api::l6::midi_send,
            hook,
        ] {
            session.execute(Command::AddBreakpoint { address }).unwrap();
        }
        {
            let mut state = state.lock().unwrap();
            state.registers[15] = firmware_api::l6::idle_loop;
            state.registers[13] = 0x2000_1000;
            state.registers[XPSR as usize] = 0x0100_0000;
        }
        (session, state, hook)
    }
    #[test]
    fn l6_midi_probe_finishes_stopped_after_sender_returns() {
        let (mut session, state, hook) = prepared_midi();
        assert!(
            session
                .execute(Command::L6MidiInject {
                    expected_hook: hook,
                    binding: L6MidiBinding::Firmware,
                    timeout_ms: 1000,
                    leave_stopped: false,
                })
                .is_err()
        );
        let result = session
            .execute(Command::L6MidiInject {
                expected_hook: hook,
                binding: L6MidiBinding::Firmware,
                timeout_ms: 1000,
                leave_stopped: true,
            })
            .unwrap();
        assert!(matches!(
            result,
            Observation::L6MidiInjected {
                requires_restart: true,
                ..
            }
        ));
        {
            let mut state = state.lock().unwrap();
            assert_eq!(state.scratch, [0, 0, 0, 0, 0xb0, 127, 127, 0]);
            assert_eq!(state.registers[0], SCRATCH);
            assert_eq!(state.registers[14], firmware_api::l6::idle_loop | 1);
            assert_eq!(state.registers[15], firmware_api::l6::midi_in);
            state.registers[15] = hook;
        }
        assert!(
            matches!(session.execute(Command::L6MidiHook { timeout_ms: 1000 }).unwrap(),
            Observation::L6MidiHook { pc, event } if pc == hook && event == [0, 0, 0, 0, 0xb0, 127, 127])
        );
        {
            let mut state = state.lock().unwrap();
            state.registers[15] = firmware_api::l6::midi_send;
            state.registers[0] = 0x2000_1000;
            state.registers[1] = 3;
            state.registers[2] = 1;
            state.registers[14] = 0x8003_1801;
        }
        assert!(matches!(
            session
                .execute(Command::L6MidiSender { timeout_ms: 1000 })
                .unwrap(),
            Observation::L6MidiSender {
                length: 3,
                route: 1,
                return_pc: 0x8003_1800,
                bytes: Some(_),
                ..
            }
        ));
        assert!(
            session
                .execute(Command::L6MidiFinish { timeout_ms: 1000 })
                .is_err()
        );
        state.lock().unwrap().registers[15] = 0x8003_1800;
        assert!(matches!(
            session
                .execute(Command::L6MidiFinish { timeout_ms: 1000 })
                .unwrap(),
            Observation::L6MidiFinished {
                requires_restart: true
            }
        ));
        assert!(session.unusable.load(Ordering::Acquire));
        assert!(session.execute(Command::Close).is_ok());
        let state = state.lock().unwrap();
        assert_eq!(state.scratch, vec![9; SCRATCH_LEN]);
        assert!(state.operations.contains(&"stop".to_owned()));
        assert!(!state.operations.contains(&"resume".to_owned()));
    }
    #[test]
    fn l6_midi_failed_injection_stops_instead_of_resuming() {
        let (mut session, state, hook) = prepared_midi();
        state.lock().unwrap().fail = Some("write-register");
        assert!(
            session
                .execute(Command::L6MidiInject {
                    expected_hook: hook,
                    binding: L6MidiBinding::Firmware,
                    timeout_ms: 1000,
                    leave_stopped: true,
                })
                .is_err()
        );
        assert!(session.execute(Command::Close).is_err());
        assert!(session.unusable.load(Ordering::Acquire));
        let state = state.lock().unwrap();
        assert!(state.operations.contains(&"stop".to_owned()));
        assert!(!state.operations.contains(&"resume".to_owned()));
    }
    #[test]
    fn l6_midi_rejects_exception_mode_before_mutating_guest() {
        let (mut session, state, hook) = prepared_midi();
        state.lock().unwrap().registers[XPSR as usize] = 0x0100_0001;
        assert!(
            session
                .execute(Command::L6MidiInject {
                    expected_hook: hook,
                    binding: L6MidiBinding::Firmware,
                    timeout_ms: 1000,
                    leave_stopped: true,
                })
                .is_err()
        );
        session.execute(Command::Close).unwrap();
        let state = state.lock().unwrap();
        assert!(!state.operations.contains(&"write".to_owned()));
        assert!(state.operations.contains(&"resume".to_owned()));
    }
    #[test]
    fn l6_midi_probe_guards_reject_wrong_model_and_bad_code() {
        assert!(validate_l6_probe_guards(Model::L6max, |_, _| unreachable!()).is_err());
        assert!(validate_l6_probe_guards(Model::L6, |_, n| Ok(vec![0; n])).is_err());
        assert!(validate_l6_probe_guards(Model::L6, |_, _| Ok(vec![])).is_err());
    }
    #[test]
    fn hook_binding_checks_actual_branch_and_sd_registry() {
        use patch_abi::registry;
        let hook = 0x800b_6b14;
        let dispatcher = 0x800b_1000;
        let base = 0x8020_0000;
        let module = 0x800b_6b00;
        let site = 3;
        let branch = patch_core::branch(firmware_api::l6::midi_in, hook, false);
        assert!(
            validate_l6_hook_binding(hook, &L6MidiBinding::Firmware, |_, _| Ok(branch.to_vec()))
                .is_ok()
        );
        assert!(
            validate_l6_hook_binding(hook + 2, &L6MidiBinding::Firmware, |_, _| Ok(
                branch.to_vec()
            ))
            .is_err()
        );
        let binding = L6MidiBinding::Sd {
            dispatcher,
            site_index: site,
            entry_offset: hook - module,
        };
        let read = |address, _| -> Result<Vec<u8>, String> {
            if address == firmware_api::l6::midi_in {
                return Ok(patch_core::branch(address, dispatcher, false).to_vec());
            }
            let value = if address == registry::L6_POINTER {
                base
            } else if address == base {
                registry::MAGIC
            } else if address == base + registry::SITE_COUNT_OFFSET {
                4
            } else if address == base + registry::MODULE_PTR_OFFSET {
                module
            } else if address == base + registry::MODULE_LEN_OFFSET {
                64
            } else if address == base + registry::site_offset(site) {
                hook | 1
            } else {
                return Err("unexpected address".into());
            };
            Ok(value.to_le_bytes().to_vec())
        };
        assert!(validate_l6_hook_binding(hook, &binding, read).is_ok());
        assert!(validate_l6_hook_binding(hook + 2, &binding, read).is_err());
    }
    #[test]
    #[ignore = "requires unpacked L6 1.10 MAIN in L6_MIDI_TEST_MAIN"]
    fn real_l6_midi_probe_guards() {
        let path = std::env::var_os("L6_MIDI_TEST_MAIN").expect("L6_MIDI_TEST_MAIN");
        let image = std::fs::read(path).unwrap();
        validate_l6_probe_guards(Model::L6, |address, length| {
            let offset = (address - 0x8000_1000) as usize;
            image
                .get(offset..offset + length)
                .map(|b| b.to_vec())
                .ok_or_else(|| "short image".into())
        })
        .unwrap();
    }
    #[test]
    #[ignore = "requires patched L6 MAIN in L6_MIDI_PATCHED_MAIN"]
    fn real_l6_firmware_midi_hook() {
        let path = std::env::var_os("L6_MIDI_PATCHED_MAIN").expect("L6_MIDI_PATCHED_MAIN");
        let image = std::fs::read(path).unwrap();
        validate_l6_hook_binding(0x800b_6b14, &L6MidiBinding::Firmware, |address, length| {
            let offset = (address - 0x8000_1000) as usize;
            image
                .get(offset..offset + length)
                .map(|b| b.to_vec())
                .ok_or_else(|| "short image".into())
        })
        .unwrap();
    }
    #[test]
    fn close_restores_scratch_and_owned_breakpoints_before_detach_and_resume() {
        let (mut session, state) = fake(true, true);
        session
            .execute(Command::AddBreakpoint {
                address: 0x80001000,
            })
            .unwrap();
        session
            .execute(Command::L6ScratchWrite {
                bytes: [7; SCRATCH_LEN],
            })
            .unwrap();
        session.execute(Command::Close).unwrap();
        let state = state.lock().unwrap();
        assert_eq!(state.scratch, vec![9; SCRATCH_LEN]);
        assert_eq!(
            state.operations,
            [
                "add",
                "read",
                "write",
                "write",
                "remove",
                "disconnect",
                "resume"
            ]
        );
    }
    #[test]
    fn dropped_or_already_paused_session_restores_state() {
        let (mut session, state) = fake(false, true);
        session
            .execute(Command::AddBreakpoint {
                address: 0x80001000,
            })
            .unwrap();
        drop(session);
        assert_eq!(
            state.lock().unwrap().operations,
            ["add", "remove", "disconnect", "stop"]
        );
    }
    #[test]
    fn transport_failure_or_cleanup_failure_stops_guest_instead_of_resuming() {
        let (mut session, state) = fake(true, true);
        session
            .execute(Command::AddBreakpoint {
                address: 0x80001000,
            })
            .unwrap();
        state.lock().unwrap().fail = Some("continue");
        assert!(
            session
                .execute(Command::Continue { timeout_ms: 10 })
                .is_err()
        );
        assert!(session.close().is_err());
        assert_eq!(
            state.lock().unwrap().operations,
            ["add", "continue", "disconnect", "stop"]
        );
        let (mut session, state) = fake(true, true);
        session
            .execute(Command::AddBreakpoint {
                address: 0x80001000,
            })
            .unwrap();
        state.lock().unwrap().fail = Some("remove");
        assert!(session.close().is_err());
        assert_eq!(
            state.lock().unwrap().operations,
            ["add", "remove", "disconnect", "stop"]
        );
    }
    #[test]
    fn cancellation_interrupts_running_guest_before_cleanup() {
        let (mut session, state) = fake(true, true);
        session
            .execute(Command::AddBreakpoint {
                address: 0x80001000,
            })
            .unwrap();
        assert!(
            session
                .execute_cancellable(Command::Continue { timeout_ms: 1000 }, || true)
                .unwrap_err()
                .contains("cancelled")
        );
        session.close().unwrap();
        assert_eq!(
            state.lock().unwrap().operations,
            [
                "add",
                "continue",
                "interrupt",
                "remove",
                "disconnect",
                "resume"
            ]
        );
    }

    #[test]
    fn socket_disconnect_closes_session_and_restores_guest() {
        use std::{
            io::{BufRead, BufReader, Write},
            os::unix::net::UnixStream,
            thread,
        };
        let (session, state) = fake(true, true);
        let (server, mut client) = UnixStream::pair().unwrap();
        let worker = thread::spawn(move || {
            crate::control::serve_debug(
                server.try_clone().unwrap(),
                BufReader::new(server),
                session,
                &AtomicBool::new(false),
            )
        });
        let mut reader = BufReader::new(client.try_clone().unwrap());
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        assert!(line.contains("started"));
        writeln!(
            client,
            "{{\"command\":\"add-breakpoint\",\"address\":2147487744}}"
        )
        .unwrap();
        line.clear();
        reader.read_line(&mut line).unwrap();
        assert!(line.contains("installed"));
        drop(reader);
        drop(client);
        worker.join().unwrap();
        assert_eq!(
            state.lock().unwrap().operations,
            ["add", "remove", "disconnect", "resume"]
        );
    }

    #[test]
    fn initial_debug_begin_preserves_pipelined_first_command() {
        use std::{
            io::{BufRead, BufReader, Write},
            os::unix::net::UnixStream,
            thread,
        };
        let (session, state) = fake(true, true);
        let (server, mut client) = UnixStream::pair().unwrap();
        let worker = thread::spawn(move || {
            let mut reader = BufReader::new(server.try_clone().unwrap());
            let begin = crate::control::read_control_request(&mut reader).unwrap();
            assert_eq!(begin["command"], "debug-begin");
            crate::control::serve_debug(server, reader, session, &AtomicBool::new(false));
        });
        write!(
            client,
            "{{\"command\":\"debug-begin\"}}\n{{\"command\":\"register\",\"index\":15}}\n"
        )
        .unwrap();
        let mut reader = BufReader::new(client.try_clone().unwrap());
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        assert!(line.contains("started"));
        line.clear();
        reader.read_line(&mut line).unwrap();
        assert!(line.contains("register") && line.contains("42"), "{line}");
        drop(reader);
        drop(client);
        worker.join().unwrap();
        assert_eq!(
            state.lock().unwrap().operations,
            ["register", "disconnect", "resume"]
        );
    }

    #[test]
    fn contended_lease_rejects_before_touching_guest() {
        use std::os::unix::net::UnixListener;
        let dir = std::env::temp_dir().join(format!("l6-debug-contended-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir(&dir).unwrap();
        let path = dir.join("gdb.sock");
        let listener = UnixListener::bind(&path).unwrap();
        let lease = l6max_diagnostics::gdb::Lease::acquire(&path).unwrap();
        let state = Arc::new(Mutex::new(FakeState::default()));
        let controller = Controller {
            path,
            guest: Arc::new(FakeGuest(state.clone())),
            unusable: Arc::new(AtomicBool::new(false)),
            retained_lease: Arc::new(Mutex::new(None)),
        };
        assert!(controller.begin().err().unwrap().contains("leased"));
        assert!(state.lock().unwrap().operations.is_empty());
        drop(lease);
        drop(listener);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn poisoned_session_retains_cross_process_lease_until_owner_drops() {
        use std::os::unix::net::UnixListener;
        let dir = std::env::temp_dir().join(format!("l6-debug-poison-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir(&dir).unwrap();
        let path = dir.join("gdb.sock");
        let _listener = UnixListener::bind(&path).unwrap();
        let lease = l6max_diagnostics::gdb::Lease::acquire(&path).unwrap();
        let state = Arc::new(Mutex::new(FakeState {
            scratch: vec![9; SCRATCH_LEN],
            guarded: true,
            fail: Some("continue"),
            ..Default::default()
        }));
        let holder = Arc::new(Mutex::new(None));
        let unusable = Arc::new(AtomicBool::new(false));
        let mut session = Session::new(
            Box::new(FakeTransport(state.clone())),
            Arc::new(FakeGuest(state)),
            true,
            unusable.clone(),
            Some(lease),
            holder.clone(),
        );
        session
            .execute(Command::AddBreakpoint {
                address: 0x80001000,
            })
            .unwrap();
        assert!(
            session
                .execute(Command::Continue { timeout_ms: 10 })
                .is_err()
        );
        assert!(session.close().is_err());
        assert!(unusable.load(Ordering::Acquire));
        assert!(l6max_diagnostics::gdb::Lease::acquire(&path).is_err());
        drop(session);
        assert!(l6max_diagnostics::gdb::Lease::acquire(&path).is_err());
        drop(holder);
        assert!(l6max_diagnostics::gdb::Lease::acquire(&path).is_ok());
        drop(_listener);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn socket_disconnect_during_continue_interrupts_and_cleans_up() {
        use std::{
            io::{BufRead, BufReader, Write},
            os::unix::net::UnixStream,
            thread,
        };
        let (session, state) = fake(true, true);
        state.lock().unwrap().pending = true;
        let (server, mut client) = UnixStream::pair().unwrap();
        let worker = thread::spawn(move || {
            crate::control::serve_debug(
                server.try_clone().unwrap(),
                BufReader::new(server),
                session,
                &AtomicBool::new(false),
            )
        });
        let mut reader = BufReader::new(client.try_clone().unwrap());
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        writeln!(
            client,
            "{{\"command\":\"add-breakpoint\",\"address\":2147487744}}"
        )
        .unwrap();
        line.clear();
        reader.read_line(&mut line).unwrap();
        writeln!(client, "{{\"command\":\"continue\",\"timeout_ms\":10000}}").unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(1);
        while !state
            .lock()
            .unwrap()
            .operations
            .iter()
            .any(|op| op == "continue")
        {
            assert!(std::time::Instant::now() < deadline);
            thread::yield_now();
        }
        // A queued next request must not hide hangup behind unread bytes.
        writeln!(client, "{{\"command\":\"register\",\"index\":15}}").unwrap();
        drop(reader);
        drop(client);
        worker.join().unwrap();
        let ops = &state.lock().unwrap().operations;
        assert!(ops.contains(&"interrupt".to_owned()), "{ops:?}");
        assert!(
            ops.ends_with(&[
                "interrupt".into(),
                "remove".into(),
                "disconnect".into(),
                "resume".into()
            ]),
            "{ops:?}"
        );
    }

    #[test]
    fn rejects_unbounded_commands_and_unguarded_scratch_without_side_effect() {
        let (mut session, state) = fake(true, false);
        assert!(session.execute(Command::Register { index: 16 }).is_err());
        assert!(
            session
                .execute(Command::Read {
                    address: 0x841ff000,
                    length: 8
                })
                .is_err()
        );
        assert!(
            session
                .execute(Command::AddBreakpoint {
                    address: 0x80000001
                })
                .is_err()
        );
        assert!(
            session
                .execute(Command::Continue { timeout_ms: 60001 })
                .is_err()
        );
        assert!(
            session
                .execute(Command::L6ScratchWrite { bytes: [0; 8] })
                .is_err()
        );
        assert!(state.lock().unwrap().operations.is_empty());
    }
}
