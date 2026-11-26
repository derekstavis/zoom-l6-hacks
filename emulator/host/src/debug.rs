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
const MAX_BREAKPOINTS: usize = 6;
const MAX_CONTINUE_MS: u64 = 60_000;
const IDLE_GUARD: &str = "f4870ac4ce0c3cb238f92e437a9f5511912eee645168943b43a0166622b85ba2";
const SENDER_GUARD: &str = "f07c456c512bff15fb7112a0f8ad5993dab518b4fbfd4ed1544de2208681c27d";
const XPSR: u32 = 25;
const SIGNAL_ACCUMULATORS: u32 = 0x2001_50e0;
const SIGNAL_ROUTINE: u32 = 0x8001_0aa8;
const SIGNAL_ROUTINE_SHA256: &str =
    "75aa1f22ed2934b38a18a09cb0c9ec2fe01aa09c1cc1d362473eae39b6216f52";
// L6 1.10 stock task and threshold builder; the startup-call bytes are the
// four bytes that the focused PWM probe temporarily replaces with Thumb NOPs.
const L6_METER_TASK: u32 = 0x8002_b140;
const L6_METER_CALL: u32 = 0x8002_b150;
const L6_METER_BUILDER: u32 = 0x8002_6f88;
const L6_METER_GATES: u32 = 0x801f_8e74;
const L6_METER_ACCUMULATORS: u32 = 0x801f_90b8;
const L6_METER_TASK_BYTES: [u8; 16] = [
    0x48, 0xf6, 0x74, 0x6b, 0x41, 0xf2, 0x10, 0x00, 0xc8, 0xf2, 0x1f, 0x0b, 0xab, 0xf8, 0x03, 0x00,
];
const L6_METER_BUILDER_BYTES: [u8; 16] = [
    0x2d, 0xe9, 0xf0, 0x4f, 0x82, 0xb0, 0x48, 0xf6, 0x74, 0x6e, 0x4d, 0xf6, 0x3b, 0x45, 0xc8, 0xf2,
];
const L6_METER_CALL_BYTES: [u8; 4] = [0xdc, 0xf7, 0x6a, 0xf8];

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
    L6MeterGuard {
        timeout_ms: u64,
    },
    L6MeterSkipStartup {
        timeout_ms: u64,
    },
    L6MeterActivate {
        timeout_ms: u64,
    },
    L6MeterSeed {
        value: u32,
        timeout_ms: u64,
    },
    L6MeterFinish {
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
    L6DinIdleEvidence {
        timeout_ms: u64,
    },
    L6DinCaptureSemaphore {
        timeout_ms: u64,
    },
    L6DinInject {
        group: u8,
        cc: u8,
        value: u8,
        timeout_ms: u64,
        leave_stopped: bool,
    },
    L6DinWake {
        timeout_ms: u64,
    },
    L6DinSetter {
        index: u8,
        value: u8,
        timeout_ms: u64,
    },
    L6DinEvent {
        timeout_ms: u64,
    },
    L6DinSetterValue {
        timeout_ms: u64,
    },
    L6DinFinish {
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
    L6DinIdleEvidence {
        pc: u32,
        sp: u32,
        xpsr: u32,
        stack_start: u32,
        stack_end: u32,
        stack_available: u32,
    },
    L6DinSemaphore {
        address: u32,
    },
    L6DinInjected {
        stack: u32,
        stack_available: u32,
        requires_restart: bool,
    },
    L6DinCallPrepared {
        pc: u32,
        stack: u32,
    },
    L6DinEvent {
        words: [u32; 5],
    },
    L6DinSetterValue {
        value: u32,
    },
    L6DinFinished {
        requires_restart: bool,
    },
    L6MeterGuarded,
    L6MeterStartupSkipped {
        requires_restart: bool,
    },
    L6MeterActivated,
    L6MeterSeeded {
        value: u32,
    },
    L6MeterFinished {
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

fn l6_readable_ram(address: u32, length: u32) -> bool {
    address.checked_add(length).is_some_and(|end| {
        (address >= 0x2000_0000 && end <= 0x2030_0000)
            || (address >= 0x8020_0000 && end <= 0x8420_0000)
    })
}

fn l6_readable_main_memory(address: u32, length: u32) -> bool {
    address.checked_add(length).is_some_and(|end| {
        (address >= 0x2000_0000 && end <= 0x2030_0000)
            || (address >= 0x8000_0000 && end <= 0x8420_0000)
    })
}

trait Guest: Send + Sync {
    fn running(&self) -> Result<bool, String>;
    fn running_before(&self, deadline: Instant) -> Result<bool, String>;
    fn resume_before(&self, deadline: Instant) -> Result<(), String>;
    fn resume_for_breakpoint_before(&self, deadline: Instant) -> Result<(), String>;
    fn stop(&self) -> Result<(), String>;
    fn resume(&self) -> Result<(), String>;
    fn guard_l6_scratch(&self) -> Result<(), String>;
    fn guard_l6max_signal(&self, deadline: Instant) -> Result<(), String>;
    fn guard_l6_meter(
        &self,
        deadline: Instant,
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<(), String>;
    fn l6_din_idle_tasks(
        &self,
        deadline: Instant,
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<crate::freertos::TasksSnapshot, String>;
    fn guard_l6_din_dispatch(
        &self,
        deadline: Instant,
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<(), String>;
    fn stop_before(&self, deadline: Instant) -> Result<(), String>;
    fn read_memory_before(
        &self,
        address: u32,
        length: usize,
        deadline: Instant,
    ) -> Result<Vec<u8>, String>;
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
    fn guard_l6_meter(
        &self,
        deadline: Instant,
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<(), String> {
        if self.model != Model::L6 {
            return Err("level-meter probe requires L6".into());
        }
        for (address, expected) in [
            (L6_METER_TASK, &L6_METER_TASK_BYTES[..]),
            (L6_METER_CALL, &L6_METER_CALL_BYTES[..]),
            (L6_METER_BUILDER, &L6_METER_BUILDER_BYTES[..]),
        ] {
            check()?;
            let actual = self
                .qmp
                .read_memory_before(0, address, expected.len(), deadline)
                .map_err(|e| e.to_string())?;
            check()?;
            if actual != expected {
                return Err(format!(
                    "unsupported L6 level-meter code at {address:#010x}"
                ));
            }
        }
        Ok(())
    }
    fn resume_for_breakpoint_before(&self, deadline: Instant) -> Result<(), String> {
        self.qmp.resume_before(deadline).map_err(|e| e.to_string())
    }
    fn guard_l6_din_dispatch(
        &self,
        deadline: Instant,
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<(), String> {
        crate::l6_din_dispatch::verify_before_checked(&self.qmp, self.model, deadline, check)
    }
    fn l6_din_idle_tasks(
        &self,
        deadline: Instant,
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<crate::freertos::TasksSnapshot, String> {
        crate::l6_din_dispatch::verify_before_checked(&self.qmp, self.model, deadline, check)?;
        crate::freertos::tasks_before_checked(&self.qmp, self.model, deadline, check)
    }
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
    fn read_memory_before(
        &self,
        address: u32,
        length: usize,
        deadline: Instant,
    ) -> Result<Vec<u8>, String> {
        if length == 0 || length > 0x1000 || !l6_readable_main_memory(address, length as u32) {
            return Err("debug observation outside bounded MAIN memory".into());
        }
        self.qmp
            .read_memory_before(0, address, length, deadline)
            .map_err(|error| error.to_string())
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
    fn cancellation_handle(&self) -> Result<std::os::unix::net::UnixStream, String> {
        Err("debug transport cannot be interrupted independently".into())
    }
    fn set_timeout(&mut self, timeout: Duration) -> Result<(), String>;
    fn read_register(&mut self, index: u32) -> Result<u32, String>;
    fn write_register(&mut self, index: u32, value: u32) -> Result<(), String>;
    fn read(&mut self, address: u32, length: usize) -> Result<Vec<u8>, String>;
    fn write(&mut self, address: u32, bytes: &[u8]) -> Result<(), String>;
    fn write_before(&mut self, address: u32, bytes: &[u8], deadline: Instant)
    -> Result<(), String>;
    fn breakpoint(&mut self, address: u32, enable: bool) -> Result<(), String>;
    fn breakpoint_before(
        &mut self,
        address: u32,
        enable: bool,
        deadline: Instant,
    ) -> Result<(), String>;
    fn register_before(&mut self, index: u32, deadline: Instant) -> Result<u32, String>;
    fn write_register_before(
        &mut self,
        index: u32,
        value: u32,
        deadline: Instant,
    ) -> Result<(), String>;
    fn call_thumb(
        &mut self,
        address: u32,
        arguments: &[u32],
        timeout: Duration,
    ) -> Result<String, String>;
    fn resume_before(&mut self, deadline: Instant) -> Result<(), String>;
    fn step_start_before(&mut self, deadline: Instant) -> Result<(), String>;
    fn poll_stop_before(
        &mut self,
        poll_deadline: Instant,
        packet_deadline: Instant,
    ) -> Result<Option<String>, String>;
    fn interrupt_before(&mut self, deadline: Instant) -> Result<String, String>;
    fn resume(&mut self) -> Result<(), String>;
    fn poll_stop(&mut self, wait: Duration) -> Result<Option<String>, String>;
    fn interrupt(&mut self) -> Result<String, String>;
    fn step(&mut self) -> Result<String, String>;
    fn disconnect(&mut self) -> Result<(), String>;
}
impl Transport for Gdb {
    fn cancellation_handle(&self) -> Result<std::os::unix::net::UnixStream, String> {
        Gdb::cancellation_handle(self).map_err(|e| e.to_string())
    }
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
    fn breakpoint_before(
        &mut self,
        address: u32,
        enable: bool,
        deadline: Instant,
    ) -> Result<(), String> {
        Gdb::breakpoint_before(self, address, enable, deadline).map_err(|e| e.to_string())
    }
    fn register_before(&mut self, index: u32, deadline: Instant) -> Result<u32, String> {
        Gdb::read_register_before(self, index, deadline).map_err(|e| e.to_string())
    }
    fn write_register_before(
        &mut self,
        index: u32,
        value: u32,
        deadline: Instant,
    ) -> Result<(), String> {
        Gdb::write_register_before(self, index, value, deadline).map_err(|e| e.to_string())
    }
    fn call_thumb(
        &mut self,
        address: u32,
        arguments: &[u32],
        timeout: Duration,
    ) -> Result<String, String> {
        Gdb::call_thumb(self, address, arguments, timeout).map_err(|e| e.to_string())
    }
    fn resume_before(&mut self, deadline: Instant) -> Result<(), String> {
        Gdb::resume_before(self, deadline).map_err(|e| e.to_string())
    }
    fn step_start_before(&mut self, deadline: Instant) -> Result<(), String> {
        Gdb::step_start_before(self, deadline).map_err(|e| e.to_string())
    }
    fn poll_stop_before(
        &mut self,
        poll_deadline: Instant,
        packet_deadline: Instant,
    ) -> Result<Option<String>, String> {
        Gdb::poll_stop_before(self, poll_deadline, packet_deadline).map_err(|e| e.to_string())
    }
    fn interrupt_before(&mut self, deadline: Instant) -> Result<String, String> {
        Gdb::interrupt_before(self, deadline).map_err(|e| e.to_string())
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

struct L6DinContext {
    stack: u32,
    original_stack: [u8; 4],
}

/// Emergency cancellation retains the cross-process lease and marks this controller unusable.
pub struct CancellationHandle {
    socket: std::os::unix::net::UnixStream,
    unusable: Arc<AtomicBool>,
    retained_lease: Arc<Mutex<Option<l6max_diagnostics::gdb::Lease>>>,
    lease: Option<l6max_diagnostics::gdb::Lease>,
}
impl CancellationHandle {
    pub fn poison_and_shutdown(mut self) -> Result<(), String> {
        self.unusable.store(true, Ordering::Release);
        if let Some(lease) = self.lease.take() {
            *self
                .retained_lease
                .lock()
                .unwrap_or_else(|e| e.into_inner()) = Some(lease);
        }
        self.socket
            .shutdown(std::net::Shutdown::Both)
            .map_err(|e| e.to_string())
    }
}

pub struct Session {
    transport: Option<Box<dyn Transport>>,
    guest: Arc<dyn Guest>,
    was_running: bool,
    poisoned: bool,
    breakpoints: BTreeSet<u32>,
    scratch: Option<Vec<u8>>,
    l6_midi: Option<L6MidiContext>,
    l6_din_semaphore: Option<u32>,
    l6_din: Option<L6DinContext>,
    // Once the startup instruction may have been written, the guest must not
    // be resumed on ordinary close, even if the transport fails partway.
    l6_meter_mutated: bool,
    l6_meter_original: Option<(Vec<u8>, Vec<u8>)>,
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
            l6_din_semaphore: None,
            l6_din: None,
            l6_meter_mutated: false,
            l6_meter_original: None,
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
        if self.l6_midi.is_some() || self.l6_meter_mutated {
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

    /// Verify the code used by the L6 level-meter probe while MAIN is stopped.
    pub fn l6_meter_guard_before(
        &mut self,
        deadline: Instant,
        mut cancelled: impl FnMut() -> bool,
    ) -> Result<(), String> {
        if cancelled() || Instant::now() >= deadline {
            return Err("L6 level-meter guard cancelled or timed out".into());
        }
        if self.guest.running_before(deadline)? {
            return Err("L6 level-meter guard requires stopped MAIN".into());
        }
        let cancelled = std::cell::RefCell::new(&mut cancelled);
        self.guest.guard_l6_meter(deadline, &|| {
            if (*cancelled.borrow_mut())() || Instant::now() >= deadline {
                Err("L6 level-meter guard cancelled or timed out".into())
            } else {
                Ok(())
            }
        })?;
        if (*cancelled.borrow_mut())() || Instant::now() >= deadline {
            return Err("L6 level-meter guard deadline exceeded".into());
        }
        Ok(())
    }

    /// Replace the guarded startup call. This is a terminal diagnostic: even
    /// when the instruction is restored, the changed task execution cannot
    /// be rolled back, so the guest must be restarted afterward.
    pub fn l6_meter_skip_startup_before(
        &mut self,
        deadline: Instant,
        mut cancelled: impl FnMut() -> bool,
    ) -> Result<(), String> {
        if self.l6_meter_mutated
            || self.l6_midi.is_some()
            || self.l6_din.is_some()
            || self.scratch.is_some()
        {
            return Err("another debug mutation is active".into());
        }
        self.l6_meter_guard_before(deadline, &mut cancelled)?;
        if cancelled() || Instant::now() >= deadline {
            return Err("L6 level-meter cancelled or timed out".into());
        }
        if self.register_before(15, deadline)? & !1 != L6_METER_TASK {
            return Err("L6 level-meter task entry was not reached".into());
        }
        if cancelled() || Instant::now() >= deadline {
            return Err("L6 level-meter cancelled or timed out".into());
        }
        let gates = self.timed(deadline, &mut cancelled, |g| g.read(L6_METER_GATES, 2))?;
        let accumulators = self.timed(deadline, &mut cancelled, |g| {
            g.read(L6_METER_ACCUMULATORS, 40)
        })?;
        if gates.len() != 2 || accumulators.len() != 40 {
            return Err("short L6 level-meter journal read".into());
        }
        self.l6_meter_original = Some((gates, accumulators));
        if cancelled() || Instant::now() >= deadline {
            return Err("L6 level-meter cancelled or timed out".into());
        }
        self.l6_meter_mutated = true;
        self.transact(|g| g.write_before(L6_METER_CALL, &[0x00, 0xbf, 0x00, 0xbf], deadline))?;
        let actual = self.timed(deadline, &mut cancelled, |g| g.read(L6_METER_CALL, 4))?;
        if actual != [0x00, 0xbf, 0x00, 0xbf] {
            return Err("L6 level-meter startup-skip readback mismatch".into());
        }
        Ok(())
    }

    /// Seed the ten converted level accumulators at a stopped builder entry.
    pub fn l6_meter_activate_before(
        &mut self,
        deadline: Instant,
        mut cancelled: impl FnMut() -> bool,
    ) -> Result<(), String> {
        if cancelled()
            || Instant::now() >= deadline
            || !self.l6_meter_mutated
            || self.guest.running_before(deadline)?
        {
            return Err("L6 level-meter activation requires an active stopped session".into());
        }
        if cancelled() || Instant::now() >= deadline {
            return Err("L6 level-meter activation cancelled or timed out".into());
        }
        self.transact(|g| g.write_before(L6_METER_GATES, &[1, 1], deadline))?;
        if cancelled() || Instant::now() >= deadline {
            return Err("L6 level-meter activation cancelled or timed out".into());
        }
        Ok(())
    }

    /// Seed the ten converted level accumulators at a stopped builder entry.
    pub fn l6_meter_seed_before(
        &mut self,
        value: u32,
        deadline: Instant,
        mut cancelled: impl FnMut() -> bool,
    ) -> Result<(), String> {
        if cancelled() || Instant::now() >= deadline {
            return Err("L6 level-meter cancelled or timed out".into());
        }
        if !self.l6_meter_mutated || self.guest.running_before(deadline)? {
            return Err("L6 level-meter session is not stopped and active".into());
        }
        if cancelled() || Instant::now() >= deadline {
            return Err("L6 level-meter cancelled or timed out".into());
        }
        if self.register_before(15, deadline)? & !1 != L6_METER_BUILDER {
            return Err("L6 level-meter builder entry was not reached".into());
        }
        let bytes: Vec<u8> = (0..10).flat_map(|_| value.to_le_bytes()).collect();
        if cancelled() || Instant::now() >= deadline {
            return Err("L6 level-meter cancelled or timed out".into());
        }
        self.transact(|g| g.write_before(L6_METER_GATES, &[1, 1], deadline))?;
        if cancelled() || Instant::now() >= deadline {
            return Err("L6 level-meter cancelled or timed out".into());
        }
        self.transact(|g| g.write_before(L6_METER_ACCUMULATORS, &bytes, deadline))?;
        let actual = self.timed(deadline, &mut cancelled, |g| {
            g.read(L6_METER_ACCUMULATORS, bytes.len())
        })?;
        if actual != bytes {
            return Err("L6 level-meter accumulator readback mismatch".into());
        }
        Ok(())
    }

    /// Best-effort restoration of journalled memory before leaving MAIN
    /// stopped. The diagnostic cannot restore task or panel progression.
    pub fn l6_meter_finish_before(
        &mut self,
        deadline: Instant,
        mut cancelled: impl FnMut() -> bool,
    ) -> Result<(), String> {
        if cancelled() || Instant::now() >= deadline {
            return Err("L6 level-meter restoration cancelled or timed out".into());
        }
        if !self.l6_meter_mutated {
            return Err("L6 level-meter session is not active".into());
        }
        if self.guest.running_before(deadline)? {
            return Err("L6 level-meter restoration requires stopped MAIN".into());
        }
        let (gates, accumulators) = self
            .l6_meter_original
            .clone()
            .ok_or("L6 level-meter journal missing")?;
        if cancelled() || Instant::now() >= deadline {
            return Err("L6 level-meter restoration cancelled or timed out".into());
        }
        self.transact(|g| g.write_before(L6_METER_CALL, &L6_METER_CALL_BYTES, deadline))?;
        if cancelled() || Instant::now() >= deadline {
            return Err("L6 level-meter restoration cancelled or timed out".into());
        }
        self.transact(|g| g.write_before(L6_METER_GATES, &gates, deadline))?;
        if cancelled() || Instant::now() >= deadline {
            return Err("L6 level-meter restoration cancelled or timed out".into());
        }
        self.transact(|g| g.write_before(L6_METER_ACCUMULATORS, &accumulators, deadline))?;
        for (address, expected) in [
            (L6_METER_CALL, &L6_METER_CALL_BYTES[..]),
            (L6_METER_GATES, gates.as_slice()),
            (L6_METER_ACCUMULATORS, accumulators.as_slice()),
        ] {
            if self.timed(deadline, &mut cancelled, |g| {
                g.read(address, expected.len())
            })? != expected
            {
                return Err(format!(
                    "L6 level-meter restoration mismatch at {address:#010x}"
                ));
            }
        }
        if !self.breakpoints.is_empty() {
            return Err("remove L6 level-meter breakpoints before finishing".into());
        }
        if cancelled() || Instant::now() >= deadline {
            return Err("L6 level-meter restoration cancelled or timed out".into());
        }
        self.guest.stop_before(deadline)?;
        if cancelled() || Instant::now() >= deadline {
            return Err("L6 level-meter restoration cancelled or timed out".into());
        }
        self.transact(|g| g.disconnect())?;
        if cancelled() || Instant::now() >= deadline {
            return Err("L6 level-meter restoration cancelled or timed out".into());
        }
        self.guest.stop_before(deadline)?;
        if cancelled() || Instant::now() >= deadline {
            return Err("L6 level-meter restoration cancelled or timed out".into());
        }
        self.unusable.store(true, Ordering::Release);
        if let Some(lease) = self.lease.take() {
            *self
                .retained_lease
                .lock()
                .unwrap_or_else(|e| e.into_inner()) = Some(lease);
        }
        self.transport.take();
        Ok(())
    }

    /// Abandon a terminal meter probe, holding the lease until guest teardown.
    /// The journal is intentionally not written if debugger state is uncertain.
    pub fn l6_meter_abort_before(&mut self, deadline: Instant) -> Result<(), String> {
        if !self.l6_meter_mutated {
            return self.close_stopped_before(deadline);
        }
        let stopped = self.guest.stop_before(deadline);
        let disconnected = self
            .transport
            .as_deref_mut()
            .map(|g| g.disconnect())
            .unwrap_or(Ok(()));
        let stopped_again = self.guest.stop_before(deadline);
        self.unusable.store(true, Ordering::Release);
        if let Some(lease) = self.lease.take() {
            *self
                .retained_lease
                .lock()
                .unwrap_or_else(|e| e.into_inner()) = Some(lease);
        }
        self.transport.take();
        match (stopped, disconnected, stopped_again) {
            (Ok(()), Ok(()), Ok(())) => Ok(()),
            (first, disconnect, second) => Err(format!(
                "L6 meter abort failed; stop={first:?}, disconnect={disconnect:?}, final stop={second:?}; guest state may be unknown"
            )),
        }
    }

    /// Clone a local emergency handle; shutting it down makes this debugger unusable and requires restart.
    pub fn cancellation_handle(&self) -> Result<CancellationHandle, String> {
        let socket = self
            .transport
            .as_deref()
            .ok_or("debug session is closed")?
            .cancellation_handle()?;
        let lease = self
            .lease
            .as_ref()
            .map(|lease| lease.try_clone().map_err(|e| e.to_string()))
            .transpose()?;
        Ok(CancellationHandle {
            socket,
            unusable: self.unusable.clone(),
            retained_lease: self.retained_lease.clone(),
            lease,
        })
    }

    /// Change one owned breakpoint under a shared wall deadline.
    pub fn breakpoint_before(
        &mut self,
        address: u32,
        enable: bool,
        deadline: Instant,
    ) -> Result<(), String> {
        if address & 1 != 0 || !(0x8000_0000..0x8420_0000).contains(&address) {
            return Err("breakpoint must be aligned MAIN SDRAM".into());
        }
        if enable
            && (self.breakpoints.contains(&address) || self.breakpoints.len() >= MAX_BREAKPOINTS)
        {
            return Err("breakpoint already owned or limit reached".into());
        }
        if !enable && !self.breakpoints.contains(&address) {
            return Err("breakpoint is not owned".into());
        }
        if Instant::now() >= deadline {
            return Err("breakpoint deadline exceeded".into());
        }
        self.transact(|g| g.breakpoint_before(address, enable, deadline))?;
        if enable {
            self.breakpoints.insert(address);
        } else {
            self.breakpoints.remove(&address);
        }
        if Instant::now() >= deadline {
            return Err("breakpoint deadline exceeded".into());
        }
        Ok(())
    }
    /// Read a register under the caller's absolute deadline.
    pub fn register_before(&mut self, index: u32, deadline: Instant) -> Result<u32, String> {
        if index > 15 || Instant::now() >= deadline {
            return Err("invalid register or deadline exceeded".into());
        }
        let value = self.transact(|g| g.register_before(index, deadline))?;
        if Instant::now() >= deadline {
            return Err("register deadline exceeded".into());
        }
        Ok(value)
    }
    /// Write one ARM register as part of a host-owned retained probe.
    pub fn write_register_before(
        &mut self,
        index: u32,
        value: u32,
        deadline: Instant,
    ) -> Result<(), String> {
        if index > 15 || Instant::now() >= deadline {
            return Err("invalid register or deadline exceeded".into());
        }
        self.transact(|g| g.write_register_before(index, value, deadline))?;
        if Instant::now() >= deadline {
            return Err("register write deadline exceeded".into());
        }
        Ok(())
    }
    /// Write MAIN RAM under this session's GDB lease and an absolute deadline.
    /// Callers that need rollback must journal a QMP before-image first.
    pub fn write_memory_before(
        &mut self,
        address: u32,
        bytes: &[u8],
        deadline: Instant,
    ) -> Result<(), String> {
        if bytes.is_empty()
            || bytes.len() > 0x1000
            || !l6_readable_ram(address, bytes.len() as u32)
            || Instant::now() >= deadline
        {
            return Err("memory write outside bounded MAIN RAM or deadline exceeded".into());
        }
        self.transact(|g| g.write_before(address, bytes, deadline))?;
        if Instant::now() >= deadline {
            return Err("memory write deadline exceeded".into());
        }
        Ok(())
    }
    /// Read bounded MAIN RAM through the QMP endpoint owned by this session.
    pub fn guest_read_before(
        &self,
        address: u32,
        length: usize,
        deadline: Instant,
    ) -> Result<Vec<u8>, String> {
        if Instant::now() >= deadline {
            return Err("guest observation deadline exceeded".into());
        }
        let bytes = self.guest.read_memory_before(address, length, deadline)?;
        if Instant::now() >= deadline {
            return Err("guest observation deadline exceeded".into());
        }
        Ok(bytes)
    }
    /// Stop MAIN through the QMP endpoint owned by this session.
    pub fn guest_stop_before(&self, deadline: Instant) -> Result<(), String> {
        self.guest.stop_before(deadline)
    }
    /// Resume MAIN through the QMP endpoint owned by this session.
    pub fn guest_resume_before(&self, deadline: Instant) -> Result<(), String> {
        self.guest.resume_before(deadline)
    }
    /// Call a bounded Thumb routine while retaining this session's GDB lease.
    pub fn call_thumb_before(
        &mut self,
        address: u32,
        arguments: &[u32],
        deadline: Instant,
    ) -> Result<String, String> {
        if address & 1 != 0
            || !(0x8000_0000..0x8420_0000).contains(&address)
            || !(1..=5).contains(&arguments.len())
        {
            return Err("Thumb call target or arguments are outside supported bounds".into());
        }
        let timeout = deadline
            .checked_duration_since(Instant::now())
            .filter(|remaining| !remaining.is_zero())
            .ok_or("Thumb call deadline exceeded")?;
        let result = self.transact(|g| g.call_thumb(address, arguments, timeout))?;
        if Instant::now() >= deadline {
            return Err("Thumb call deadline exceeded".into());
        }
        Ok(result)
    }
    /// Resume MAIN under the retained debugger lease while an in-process
    /// observer polls for owned breakpoints and handles concurrent inputs.
    pub fn resume_owned_before(&mut self, deadline: Instant) -> Result<(), String> {
        if self.poisoned || self.transport.is_none() || Instant::now() >= deadline {
            return Err("debug resume requires a live session and a valid deadline".into());
        }
        // A breakpoint may stop MAIN before a subsequent status query. The
        // caller owns the session and observes that stop itself.
        if let Err(error) = self.guest.resume_for_breakpoint_before(deadline) {
            self.poisoned = true;
            return Err(error);
        }
        if Instant::now() >= deadline {
            self.poisoned = true;
            return Err("debug resume deadline exceeded".into());
        }
        Ok(())
    }
    /// Continue to an owned breakpoint with an absolute deadline and cancellation.
    pub fn continue_before(
        &mut self,
        deadline: Instant,
        cancelled: impl FnMut() -> bool,
    ) -> Result<String, String> {
        self.run_before(deadline, cancelled, false)?
            .ok_or_else(|| "debug continue deadline exceeded".into())
    }
    /// Continue until a stop or a local deadline. A timeout is returned only after a verified interrupt.
    pub fn continue_optional_before(
        &mut self,
        deadline: Instant,
        cancelled: impl FnMut() -> bool,
    ) -> Result<Option<String>, String> {
        self.run_before(deadline, cancelled, false)
    }
    /// Step one instruction and verify the resulting stop.
    pub fn step_before(
        &mut self,
        deadline: Instant,
        cancelled: impl FnMut() -> bool,
    ) -> Result<String, String> {
        self.run_before(deadline, cancelled, true)?
            .ok_or_else(|| "debug step deadline exceeded".into())
    }
    fn run_before(
        &mut self,
        deadline: Instant,
        mut cancelled: impl FnMut() -> bool,
        step: bool,
    ) -> Result<Option<String>, String> {
        if !step && self.breakpoints.is_empty() {
            return Err("continue requires an owned breakpoint".into());
        }
        if cancelled() || Instant::now() >= deadline {
            return Err("debug execution cancelled or deadline exceeded".into());
        }
        if step {
            self.transact(|g| g.step_start_before(deadline))?;
        } else {
            self.transact(|g| g.resume_before(deadline))?;
        }
        loop {
            let now = Instant::now();
            let was_cancelled = cancelled();
            if was_cancelled || now >= deadline {
                let stop = self.transact(|g| {
                    g.interrupt_before(Instant::now() + Duration::from_secs(3))
                        .and_then(validate_stop)
                });
                stop.map_err(|error| format!("debug execution interrupt failed: {error}"))?;
                if was_cancelled || cancelled() {
                    return Err("debug execution cancelled".into());
                }
                return Ok(None);
            }
            let until = (now + Duration::from_millis(50)).min(deadline);
            if let Some(reply) = self.transact(|g| {
                g.poll_stop_before(
                    until,
                    (Instant::now() + Duration::from_secs(1)).min(deadline),
                )
            })? {
                let reply = validate_stop(reply).map_err(|error| {
                    self.poisoned = true;
                    error
                })?;
                if cancelled() || Instant::now() >= deadline {
                    return Err("debug execution cancelled or deadline exceeded".into());
                }
                return Ok(Some(reply));
            }
        }
    }
    /// Restore owned debug state and explicitly resume MAIN before releasing its lease.
    /// Callers must own the guest lifecycle and intend to start a previously paused guest.
    pub fn close_resuming_before(&mut self, deadline: Instant) -> Result<(), String> {
        self.was_running = true;
        self.close_before(deadline)
    }

    /// Restore owned debug state and explicitly leave MAIN stopped while releasing its lease.
    pub fn close_stopped_before(&mut self, deadline: Instant) -> Result<(), String> {
        self.was_running = false;
        self.close_before(deadline)
    }

    /// Release this session without changing MAIN's current run state.
    pub fn close_preserving_guest_state_before(&mut self, deadline: Instant) -> Result<(), String> {
        self.was_running = self.guest.running_before(deadline)?;
        self.close_before(deadline)
    }

    /// Report whether MAIN is currently running through this session's QMP.
    pub fn guest_running_before(&self, deadline: Instant) -> Result<bool, String> {
        self.guest.running_before(deadline)
    }

    /// Restore a previously observed MAIN run state through this session's QMP.
    pub fn guest_restore_state_before(
        &self,
        running: bool,
        deadline: Instant,
    ) -> Result<(), String> {
        let current = self.guest.running_before(deadline)?;
        match (current, running) {
            (true, false) => self.guest.stop_before(deadline),
            (false, true) => self.guest.resume_before(deadline),
            _ => Ok(()),
        }
    }

    /// Restore any scratch and owned breakpoints, then restore the prior run state.
    pub fn close_before(&mut self, deadline: Instant) -> Result<(), String> {
        if self.transport.is_none() {
            return Ok(());
        }
        let mut error = None;
        if !self.poisoned && self.l6_midi.is_none() && self.l6_din.is_none() {
            if let Some(bytes) = self.scratch.clone() {
                match self.transact(|g| g.write_before(SCRATCH, &bytes, deadline)) {
                    Ok(()) => self.scratch = None,
                    Err(e) => error = Some(format!("restore scratch: {e}")),
                }
            }
            if error.is_none() {
                for address in self.breakpoints.clone() {
                    match self.transact(|g| g.breakpoint_before(address, false, deadline)) {
                        Ok(()) => {
                            self.breakpoints.remove(&address);
                        }
                        Err(e) => {
                            error = Some(format!("remove breakpoint {address:#x}: {e}"));
                            break;
                        }
                    }
                }
            }
        }
        let close = self.close_signal_before(deadline);
        match (error, close) {
            (None, Ok(())) => Ok(()),
            (Some(error), Ok(())) | (None, Err(error)) => Err(error),
            (Some(error), Err(cleanup)) => Err(format!("{error}; {cleanup}")),
        }
    }

    fn l6_din_check_return(
        &mut self,
        deadline: Instant,
        cancelled: &mut impl FnMut() -> bool,
    ) -> Result<u32, String> {
        let stack = self
            .l6_din
            .as_ref()
            .ok_or("DIN injection is not active")?
            .stack;
        if self.guest.running_before(deadline)? {
            return Err("DIN guest must be stopped".into());
        }
        let pc = self.timed(deadline, cancelled, |g| g.read_register(15))? & !1;
        let sp = self.timed(deadline, cancelled, |g| g.read_register(13))?;
        let xpsr = self.timed(deadline, cancelled, |g| g.read_register(XPSR))?;
        if pc != firmware_api::l6::idle_loop
            || sp != stack
            || xpsr & 0x1ff != 0
            || xpsr & (1 << 24) == 0
        {
            return Err("DIN synthetic call did not return to its Idle thread stack".into());
        }
        Ok(stack)
    }

    fn l6_din_idle_evidence(
        &mut self,
        deadline: Instant,
        cancelled: &mut impl FnMut() -> bool,
    ) -> Result<Observation, String> {
        if cancelled() {
            return Err("DIN idle observation cancelled".into());
        }
        if self.guest.running_before(deadline)? {
            return Err("DIN idle observation requires stopped MAIN".into());
        }
        let pc = self.timed(deadline, cancelled, |g| g.read_register(15))? & !1;
        if pc != firmware_api::l6::idle_loop {
            return Err("MAIN is not stopped at the L6 idle loop".into());
        }
        let sp = self.timed(deadline, cancelled, |g| g.read_register(13))?;
        let xpsr = self.timed(deadline, cancelled, |g| g.read_register(XPSR))?;
        if cancelled() {
            return Err("DIN idle observation cancelled".into());
        }
        let tasks = {
            let cancelled = std::cell::RefCell::new(&mut *cancelled);
            self.guest.l6_din_idle_tasks(deadline, &|| {
                if Instant::now() >= deadline || (*cancelled.borrow_mut())() {
                    Err("DIN idle observation cancelled or timed out".into())
                } else {
                    Ok(())
                }
            })?
        };
        if cancelled() || Instant::now() >= deadline {
            return Err("DIN idle observation cancelled or timed out".into());
        }
        if self.guest.running_before(deadline)? {
            return Err("MAIN resumed during DIN idle observation".into());
        }
        if cancelled() || Instant::now() >= deadline {
            return Err("DIN idle observation cancelled or timed out".into());
        }
        let (stack_start, stack_end, stack_available) =
            crate::l6_din_dispatch::idle_stack_evidence(&tasks, sp, xpsr)?;
        Ok(Observation::L6DinIdleEvidence {
            pc,
            sp,
            xpsr,
            stack_start,
            stack_end,
            stack_available,
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
        if self.l6_meter_mutated
            && !matches!(
                command,
                Command::Register { .. }
                    | Command::Read { .. }
                    | Command::AddBreakpoint { .. }
                    | Command::RemoveBreakpoint { .. }
                    | Command::Continue { .. }
                    | Command::Step
                    | Command::L6MeterActivate { .. }
                    | Command::L6MeterSeed { .. }
                    | Command::L6MeterFinish { .. }
                    | Command::Close
            )
        {
            return Err("L6 level-meter terminal diagnostic is active".into());
        }
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
        if self.l6_din.is_some()
            && matches!(
                command,
                Command::L6maxSignalWrite { .. }
                    | Command::L6ScratchRead
                    | Command::L6ScratchWrite { .. }
                    | Command::L6MidiInject { .. }
                    | Command::L6DinCaptureSemaphore { .. }
                    | Command::L6DinInject { .. }
            )
        {
            return Err("L6 DIN injection is already active".into());
        }
        match command {
            Command::L6MeterGuard { timeout_ms } => {
                self.l6_meter_guard_before(l6_probe_deadline(timeout_ms)?, cancelled)?;
                Ok(Observation::L6MeterGuarded)
            }
            Command::L6MeterSkipStartup { timeout_ms } => {
                self.l6_meter_skip_startup_before(l6_probe_deadline(timeout_ms)?, cancelled)?;
                Ok(Observation::L6MeterStartupSkipped {
                    requires_restart: true,
                })
            }
            Command::L6MeterActivate { timeout_ms } => {
                self.l6_meter_activate_before(l6_probe_deadline(timeout_ms)?, cancelled)?;
                Ok(Observation::L6MeterActivated)
            }
            Command::L6MeterSeed { value, timeout_ms } => {
                self.l6_meter_seed_before(value, l6_probe_deadline(timeout_ms)?, cancelled)?;
                Ok(Observation::L6MeterSeeded { value })
            }
            Command::L6MeterFinish { timeout_ms } => {
                self.l6_meter_finish_before(l6_probe_deadline(timeout_ms)?, cancelled)?;
                Ok(Observation::L6MeterFinished {
                    requires_restart: true,
                })
            }
            Command::L6DinEvent { timeout_ms } => {
                let deadline = l6_probe_deadline(timeout_ms)?;
                if self.l6_din.is_none() {
                    return Err("DIN injection is not active".into());
                }
                if cancelled() || self.guest.running_before(deadline)? {
                    return Err("DIN event observation requires stopped MAIN".into());
                }
                let pc = self.timed(deadline, &mut cancelled, |g| g.read_register(15))? & !1;
                if pc != firmware_api::l6::main_class2_event_handler {
                    return Err("DIN event handler not reached".into());
                }
                let address = self.timed(deadline, &mut cancelled, |g| g.read_register(0))?;
                if address & 3 != 0 || !l6_readable_ram(address, 20) {
                    return Err("DIN event is outside aligned MAIN RAM".into());
                }
                let bytes = self.timed(deadline, &mut cancelled, |g| g.read(address, 20))?;
                if bytes.len() != 20 {
                    return Err("short DIN event".into());
                }
                let mut words = [0u32; 5];
                for (word, bytes) in words.iter_mut().zip(bytes.chunks_exact(4)) {
                    *word = u32::from_le_bytes(bytes.try_into().unwrap());
                }
                Ok(Observation::L6DinEvent { words })
            }
            Command::L6DinSetterValue { timeout_ms } => {
                let deadline = l6_probe_deadline(timeout_ms)?;
                if self.l6_din.is_none() {
                    return Err("DIN injection is not active".into());
                }
                if cancelled() || self.guest.running_before(deadline)? {
                    return Err("DIN setter observation requires stopped MAIN".into());
                }
                let pc = self.timed(deadline, &mut cancelled, |g| g.read_register(15))? & !1;
                if pc != firmware_api::l6::mixer_set_eq {
                    return Err("DIN EQ setter not reached".into());
                }
                let sp = self.timed(deadline, &mut cancelled, |g| g.read_register(13))?;
                if sp & 3 != 0 || !l6_readable_ram(sp, 4) {
                    return Err("DIN setter argument is outside aligned MAIN RAM".into());
                }
                let bytes = self.timed(deadline, &mut cancelled, |g| g.read(sp, 4))?;
                Ok(Observation::L6DinSetterValue {
                    value: u32::from_le_bytes(
                        bytes.try_into().map_err(|_| "short DIN setter argument")?,
                    ),
                })
            }
            Command::L6DinWake { timeout_ms } => {
                let deadline = l6_probe_deadline(timeout_ms)?;
                let semaphore = self.l6_din_semaphore.ok_or("DIN semaphore missing")?;
                if !self
                    .breakpoints
                    .contains(&firmware_api::l6::main_wait_return)
                {
                    return Err("DIN wake requires owned wait-return breakpoint".into());
                }
                let stack = self.l6_din_check_return(deadline, &mut cancelled)?;
                for (register, value) in [
                    (0, semaphore),
                    (1, 0),
                    (2, 0),
                    (3, 0),
                    (14, firmware_api::l6::idle_loop | 1),
                    (15, firmware_api::l6::main_wait_give),
                ] {
                    self.timed(deadline, &mut cancelled, |g| {
                        g.write_register(register, value)
                    })?;
                }
                Ok(Observation::L6DinCallPrepared {
                    pc: firmware_api::l6::main_wait_give,
                    stack,
                })
            }
            Command::L6DinSetter {
                index,
                value,
                timeout_ms,
            } => {
                let setters = [
                    firmware_api::l6::mixer_set_eq,
                    firmware_api::l6::mixer_din_setter_1,
                    firmware_api::l6::mixer_set_eq,
                    firmware_api::l6::mixer_set_eq,
                    firmware_api::l6::mixer_din_setter_4,
                    firmware_api::l6::mixer_din_setter_4,
                    firmware_api::l6::mixer_din_setter_6,
                    firmware_api::l6::mixer_din_setter_7,
                    firmware_api::l6::mixer_din_setter_8,
                ];
                let pc = *setters
                    .get(usize::from(index))
                    .ok_or("DIN setter index out of range")?;
                let deadline = l6_probe_deadline(timeout_ms)?;
                if !self.breakpoints.contains(&firmware_api::l6::idle_loop) {
                    return Err("DIN setter requires owned idle breakpoint".into());
                }
                let stack = self.l6_din_check_return(deadline, &mut cancelled)?;
                self.timed(deadline, &mut cancelled, |g| {
                    g.write(stack, &u32::from(value).to_le_bytes())
                })?;
                for (register, value) in [
                    (0, 0),
                    (1, u32::from(index)),
                    (2, 1),
                    (3, 0),
                    (14, firmware_api::l6::idle_loop | 1),
                    (15, pc),
                ] {
                    self.timed(deadline, &mut cancelled, |g| {
                        g.write_register(register, value)
                    })?;
                }
                Ok(Observation::L6DinCallPrepared { pc, stack })
            }
            Command::L6DinFinish { timeout_ms } => {
                let deadline = l6_probe_deadline(timeout_ms)?;
                let context = self.l6_din.as_ref().ok_or("DIN injection is not active")?;
                let stack = context.stack;
                let original = context.original_stack;
                self.l6_din_check_return(deadline, &mut cancelled)?;
                self.timed(deadline, &mut cancelled, |g| g.write(stack, &original))?;
                for address in self.breakpoints.clone() {
                    self.timed(deadline, &mut cancelled, |g| g.breakpoint(address, false))?;
                    self.breakpoints.remove(&address);
                }
                if cancelled() || Instant::now() >= deadline {
                    return Err("DIN finish cancelled or timed out".into());
                }
                self.guest.stop_before(deadline)?;
                if cancelled() || Instant::now() >= deadline {
                    return Err("DIN finish cancelled or timed out".into());
                }
                self.timed(deadline, &mut cancelled, |g| g.disconnect())?;
                if cancelled() || Instant::now() >= deadline {
                    return Err("DIN finish cancelled or timed out".into());
                }
                self.guest.stop_before(deadline)?;
                if cancelled() || Instant::now() >= deadline {
                    return Err("DIN finish cancelled or timed out".into());
                }
                self.unusable.store(true, Ordering::Release);
                if let Some(lease) = self.lease.take() {
                    *self
                        .retained_lease
                        .lock()
                        .unwrap_or_else(|e| e.into_inner()) = Some(lease);
                }
                self.transport.take();
                self.l6_din = None;
                Ok(Observation::L6DinFinished {
                    requires_restart: true,
                })
            }
            Command::L6DinCaptureSemaphore { timeout_ms } => {
                let deadline = l6_probe_deadline(timeout_ms)?;
                if cancelled() || self.guest.running_before(deadline)? {
                    return Err("DIN semaphore observation requires stopped MAIN".into());
                }
                if self.l6_din.is_some()
                    || !self.breakpoints.contains(&firmware_api::l6::main_wait_call)
                {
                    return Err(
                        "DIN semaphore observation requires owned wait-call breakpoint".into(),
                    );
                }
                {
                    let cancelled = std::cell::RefCell::new(&mut cancelled);
                    self.guest.guard_l6_din_dispatch(deadline, &|| {
                        if Instant::now() >= deadline || (*cancelled.borrow_mut())() {
                            Err("DIN guard cancelled or timed out".into())
                        } else {
                            Ok(())
                        }
                    })?;
                }
                let pc = self.timed(deadline, &mut cancelled, |g| g.read_register(15))? & !1;
                let wait = self.timed(deadline, &mut cancelled, |g| g.read_register(1))?;
                if pc != firmware_api::l6::main_wait_call || wait != u32::MAX {
                    return Err("DIN Main task did not reach its infinite semaphore wait".into());
                }
                let address = self.timed(deadline, &mut cancelled, |g| g.read_register(0))?;
                if address & 3 != 0 || !l6_readable_ram(address, 0x50) {
                    return Err("DIN Main semaphore is outside aligned MAIN RAM".into());
                }
                self.l6_din_semaphore = Some(address);
                Ok(Observation::L6DinSemaphore { address })
            }
            Command::L6DinInject {
                group,
                cc,
                value,
                timeout_ms,
                leave_stopped,
            } => {
                if !leave_stopped
                    || self.l6_din_semaphore.is_none()
                    || self.l6_din.is_some()
                    || self.l6_midi.is_some()
                {
                    return Err(
                        "DIN injection requires captured semaphore and terminal leave-stopped mode"
                            .into(),
                    );
                }
                if group > 15 || cc > 127 || value > 127 {
                    return Err("invalid DIN event values".into());
                }
                if !self.breakpoints.contains(&firmware_api::l6::idle_loop)
                    || !self
                        .breakpoints
                        .contains(&firmware_api::l6::main_wait_return)
                {
                    return Err(
                        "DIN injection requires owned idle and wait-return breakpoints".into(),
                    );
                }
                let deadline = l6_probe_deadline(timeout_ms)?;
                let evidence = self.l6_din_idle_evidence(deadline, &mut cancelled)?;
                let Observation::L6DinIdleEvidence {
                    sp,
                    stack_available,
                    ..
                } = evidence
                else {
                    return Err("missing DIN idle evidence".into());
                };
                if stack_available < 2048 {
                    return Err("DIN idle stack has less than 2048 bytes headroom".into());
                }
                let bytes = self.timed(deadline, &mut cancelled, |g| g.read(sp, 4))?;
                let original_stack: [u8; 4] =
                    bytes.try_into().map_err(|_| "short DIN stack read")?;
                // From the first write onward cleanup is terminal and fail-closed.
                self.l6_din = Some(L6DinContext {
                    stack: sp,
                    original_stack,
                });
                self.timed(deadline, &mut cancelled, |g| {
                    g.write(sp, &0u32.to_le_bytes())
                })?;
                for (register, value) in [
                    (0, 2),
                    (1, u32::from(group)),
                    (2, u32::from(cc)),
                    (3, u32::from(value)),
                    (14, firmware_api::l6::idle_loop | 1),
                    (15, firmware_api::l6::main_event_submit),
                ] {
                    self.timed(deadline, &mut cancelled, |g| {
                        g.write_register(register, value)
                    })?;
                }
                Ok(Observation::L6DinInjected {
                    stack: sp,
                    stack_available,
                    requires_restart: true,
                })
            }
            Command::L6DinIdleEvidence { timeout_ms } => {
                self.l6_din_idle_evidence(l6_probe_deadline(timeout_ms)?, &mut cancelled)
            }
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
            || self.l6_din.is_some()
            || self.l6_meter_mutated
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
        } else {
            self.guest.running_before(deadline).and_then(|running| {
                match (running, self.was_running) {
                    (true, false) => self.guest.stop_before(deadline),
                    (false, true) => self.guest.resume_before(deadline),
                    _ => Ok(()),
                }
            })
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
        if self.l6_din.is_some() {
            errors.push("L6 DIN synthetic execution requires a guest restart".to_owned());
        }
        if self.l6_meter_mutated {
            errors.push("L6 level-meter diagnostic requires a guest restart".to_owned());
        }
        if !self.poisoned
            && self.l6_midi.is_none()
            && self.l6_din.is_none()
            && !self.l6_meter_mutated
        {
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
        // A terminal meter diagnostic must not detach while MAIN could run
        // with its transiently patched startup code.
        if self.l6_meter_mutated {
            if let Err(error) = self
                .guest
                .stop_before(Instant::now() + Duration::from_secs(3))
            {
                errors.push(format!("could not stop MAIN before disconnect: {error}"));
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

/// In-process coordination of a retained debugger and two initially paused
/// guests. The caller owns timing, input replay and interpretation of stops.
pub struct Observer {
    main: QmpClient,
    panel: QmpClient,
    session: Option<Session>,
}
impl Observer {
    pub fn acquire(
        controller: &Controller,
        main: &QmpClient,
        panel: &QmpClient,
        deadline: Instant,
    ) -> Result<Self, String> {
        if main.running_before(deadline).map_err(|e| e.to_string())?
            || panel.running_before(deadline).map_err(|e| e.to_string())?
        {
            return Err("debug observer requires both guests initially paused".into());
        }
        let mut session = controller.begin_before(deadline)?;
        let panel_status = panel.running_before(deadline).map_err(|e| e.to_string());
        if session.was_running || !matches!(panel_status, Ok(false)) {
            let reason = match panel_status {
                Err(error) => format!("panel status during observer acquisition: {error}"),
                _ => "debug observer guests changed from paused during acquisition".into(),
            };
            let cleanup = session.close_before(Instant::now() + Duration::from_secs(3));
            return Err(match cleanup {
                Ok(()) => reason,
                Err(error) => format!("{reason}; cleanup: {error}"),
            });
        }
        Ok(Self {
            main: main.clone(),
            panel: panel.clone(),
            session: Some(session),
        })
    }
    fn session(&mut self) -> Result<&mut Session, String> {
        self.session.as_mut().ok_or("debug observer closed".into())
    }
    pub fn breakpoint_before(
        &mut self,
        address: u32,
        enabled: bool,
        deadline: Instant,
    ) -> Result<(), String> {
        self.session()?
            .breakpoint_before(address, enabled, deadline)
    }
    pub fn register_before(&mut self, index: u32, deadline: Instant) -> Result<u32, String> {
        self.session()?.register_before(index, deadline)
    }
    pub fn resume_panel_before(&self, deadline: Instant) -> Result<(), String> {
        self.panel
            .resume_before(deadline)
            .map_err(|e| e.to_string())
    }
    pub fn resume_main_before(&mut self, deadline: Instant) -> Result<(), String> {
        self.session()?.resume_owned_before(deadline)
    }
    pub fn main_running_before(&self, deadline: Instant) -> Result<bool, String> {
        self.main
            .running_before(deadline)
            .map_err(|e| e.to_string())
    }
    pub fn stop_main_before(&self, deadline: Instant) -> Result<(), String> {
        self.main.stop_before(deadline).map_err(|e| e.to_string())
    }
    pub fn close_stopped_before(&mut self, deadline: Instant) -> Result<(), String> {
        self.stop_main_before(deadline)?;
        self.session()?.close_stopped_before(deadline)?;
        self.session = None;
        Ok(())
    }
    pub fn abort(&mut self) -> Result<(), String> {
        if self.session.is_none() {
            return Ok(());
        }
        let deadline = Instant::now() + Duration::from_secs(3);
        if let Err(error) = self.stop_main_before(deadline) {
            // A failed stop makes subsequent breakpoint writes unsafe.
            self.session()?.poisoned = true;
            let cleanup = self.session()?.close_stopped_before(deadline);
            self.session = None;
            return Err(match cleanup {
                Ok(()) => error,
                Err(cleanup) => format!("{error}; debugger cleanup: {cleanup}"),
            });
        }
        let panel = self.panel.stop_before(deadline).map_err(|e| e.to_string());
        let close = self.session()?.close_stopped_before(deadline);
        self.session = None;
        match (panel, close) {
            (Ok(()), Ok(())) => Ok(()),
            (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
            (Err(error), Err(cleanup)) => {
                Err(format!("panel stop: {error}; debugger cleanup: {cleanup}"))
            }
        }
    }
}
impl Drop for Observer {
    fn drop(&mut self) {
        if let Err(error) = self.abort() {
            tracing::error!(%error, "debug observer cleanup failed");
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
        meter_memory: std::collections::BTreeMap<u32, Vec<u8>>,
        meter_partial_write: bool,
        registers: [u32; 26],
        fail: Option<&'static str>,
        guarded: bool,
        pending: bool,
        running_after_cont: bool,
        stop_before_calls: usize,
        cancel_on_guard: Option<Arc<AtomicBool>>,
        idle_snapshot: bool,
        status_calls: usize,
        cancel_on_second_status: Option<Arc<AtomicBool>>,
    }
    struct FakeGuest(Arc<Mutex<FakeState>>);
    impl Guest for FakeGuest {
        fn guard_l6_meter(
            &self,
            _deadline: Instant,
            check: &dyn Fn() -> Result<(), String>,
        ) -> Result<(), String> {
            check()?;
            self.guard_l6_scratch()?;
            if let Some(flag) = &self.0.lock().unwrap().cancel_on_guard {
                flag.store(true, Ordering::Release);
            }
            check()
        }
        fn resume_for_breakpoint_before(&self, deadline: Instant) -> Result<(), String> {
            self.resume_before(deadline)
        }
        fn guard_l6_din_dispatch(
            &self,
            _: Instant,
            check: &dyn Fn() -> Result<(), String>,
        ) -> Result<(), String> {
            check()?;
            self.guard_l6_scratch()
        }
        fn l6_din_idle_tasks(
            &self,
            _: Instant,
            _: &dyn Fn() -> Result<(), String>,
        ) -> Result<crate::freertos::TasksSnapshot, String> {
            if !self.0.lock().unwrap().idle_snapshot {
                return Err("fake task snapshot unavailable".into());
            }
            Ok(crate::freertos::TasksSnapshot {
                tasks: vec![crate::freertos::Task {
                    name: "Idle".into(),
                    state: "Current".into(),
                    tcb: 1,
                    priority: 1,
                    base_priority: 1,
                    creation_number: 0,
                    mutexes_held: 0,
                    event_list: None,
                    stack_start: 0x80200000,
                    stack_size: 4096,
                    stack_remaining: 0,
                    current: true,
                }],
            })
        }
        fn running(&self) -> Result<bool, String> {
            Ok(true)
        }
        fn running_before(&self, _deadline: Instant) -> Result<bool, String> {
            let mut state = self.0.lock().unwrap();
            state.operations.push("running-before".into());
            state.status_calls += 1;
            if state.status_calls == 2 {
                if let Some(flag) = &state.cancel_on_second_status {
                    flag.store(true, Ordering::Release);
                }
            }
            if state.fail == Some("running-before") {
                Err("timeout".into())
            } else {
                Ok(!state.idle_snapshot)
            }
        }
        fn resume_before(&self, _deadline: Instant) -> Result<(), String> {
            let mut state = self.0.lock().unwrap();
            state.operations.push("resume-before".into());
            state.running_after_cont = true;
            state.idle_snapshot = false;
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
            state.idle_snapshot = true;
            Ok(())
        }
        fn resume(&self) -> Result<(), String> {
            let mut state = self.0.lock().unwrap();
            state.operations.push("resume".into());
            state.idle_snapshot = false;
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
        fn read_memory_before(
            &self,
            _address: u32,
            length: usize,
            deadline: Instant,
        ) -> Result<Vec<u8>, String> {
            if length == 0 || length > 0x1000 || Instant::now() >= deadline {
                Err("invalid fake read".into())
            } else {
                Ok(vec![0; length])
            }
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
                Ok(self
                    .0
                    .lock()
                    .unwrap()
                    .meter_memory
                    .get(&address)
                    .cloned()
                    .unwrap_or_else(|| vec![0; length]))
            }
        }
        fn write(&mut self, address: u32, bytes: &[u8]) -> Result<(), String> {
            {
                let mut state = self.0.lock().unwrap();
                if state.meter_partial_write {
                    state.operations.push("partial-write".into());
                    state
                        .meter_memory
                        .insert(address, bytes[..bytes.len().min(2)].to_vec());
                    return Err("partial write".into());
                }
            }
            self.record("write")?;
            let mut state = self.0.lock().unwrap();
            if address == SCRATCH {
                state.scratch = bytes.to_vec();
            } else {
                state.meter_memory.insert(address, bytes.to_vec());
            }
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
        fn breakpoint_before(
            &mut self,
            address: u32,
            enable: bool,
            deadline: Instant,
        ) -> Result<(), String> {
            if Instant::now() >= deadline {
                return Err("timeout".into());
            }
            self.breakpoint(address, enable)
        }
        fn register_before(&mut self, index: u32, deadline: Instant) -> Result<u32, String> {
            if Instant::now() >= deadline {
                return Err("timeout".into());
            }
            self.read_register(index)
        }
        fn write_register_before(
            &mut self,
            index: u32,
            value: u32,
            deadline: Instant,
        ) -> Result<(), String> {
            if Instant::now() >= deadline {
                return Err("timeout".into());
            }
            self.write_register(index, value)
        }
        fn call_thumb(
            &mut self,
            _address: u32,
            _arguments: &[u32],
            _timeout: Duration,
        ) -> Result<String, String> {
            self.record("call-thumb")?;
            Ok("S05".into())
        }
        fn resume_before(&mut self, deadline: Instant) -> Result<(), String> {
            if Instant::now() >= deadline {
                return Err("timeout".into());
            }
            self.resume()
        }
        fn step_start_before(&mut self, deadline: Instant) -> Result<(), String> {
            if Instant::now() >= deadline {
                return Err("timeout".into());
            }
            self.record("step-start")
        }
        fn poll_stop_before(
            &mut self,
            poll_deadline: Instant,
            _packet_deadline: Instant,
        ) -> Result<Option<String>, String> {
            self.poll_stop(poll_deadline.saturating_duration_since(Instant::now()))
        }
        fn interrupt_before(&mut self, deadline: Instant) -> Result<String, String> {
            if Instant::now() >= deadline {
                return Err("timeout".into());
            }
            self.interrupt()
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
            idle_snapshot: true,
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
    fn meter_guard_rejects_unsupported_before_any_write() {
        let (mut session, state) = fake(false, false);
        state.lock().unwrap().idle_snapshot = true;
        assert!(
            session
                .l6_meter_skip_startup_before(Instant::now() + Duration::from_secs(1), || false)
                .is_err()
        );
        assert!(
            !state
                .lock()
                .unwrap()
                .operations
                .iter()
                .any(|op| op == "write")
        );
    }

    #[test]
    fn meter_cancelled_during_guard_never_mutates() {
        let (mut session, state) = fake(false, true);
        let cancelled = Arc::new(AtomicBool::new(false));
        {
            let mut state = state.lock().unwrap();
            state.idle_snapshot = true;
            state.registers[15] = L6_METER_TASK;
            state.cancel_on_guard = Some(cancelled.clone());
        }
        assert!(
            session
                .l6_meter_skip_startup_before(Instant::now() + Duration::from_secs(1), || cancelled
                    .load(Ordering::Acquire))
                .is_err()
        );
        assert!(
            !state
                .lock()
                .unwrap()
                .operations
                .iter()
                .any(|op| op == "write")
        );
    }

    #[test]
    fn meter_failed_first_write_never_resumes() {
        let (mut session, state) = fake(true, true);
        {
            let mut state = state.lock().unwrap();
            state.idle_snapshot = true;
            state.registers[15] = L6_METER_TASK;
            state.fail = Some("write");
        }
        assert!(
            session
                .l6_meter_skip_startup_before(Instant::now() + Duration::from_secs(1), || false)
                .is_err()
        );
        assert!(
            session
                .close_before(Instant::now() + Duration::from_secs(1))
                .is_err()
        );
        let state = state.lock().unwrap();
        assert!(state.operations.iter().any(|op| op == "stop"));
        assert!(!state.operations.iter().any(|op| op == "resume-before"));
        assert!(session.unusable.load(Ordering::Acquire));
    }

    #[test]
    fn meter_partial_code_write_keeps_guest_terminal() {
        let (mut session, state) = fake(true, true);
        {
            let mut state = state.lock().unwrap();
            state.idle_snapshot = true;
            state.registers[15] = L6_METER_TASK;
            state.meter_partial_write = true;
        }
        let deadline = Instant::now() + Duration::from_secs(1);
        assert!(
            session
                .l6_meter_skip_startup_before(deadline, || false)
                .is_err()
        );
        assert_eq!(
            state.lock().unwrap().meter_memory[&L6_METER_CALL],
            [0, 0xbf]
        );
        session.l6_meter_abort_before(deadline).unwrap();
        assert!(session.unusable.load(Ordering::Acquire));
        assert!(
            !state
                .lock()
                .unwrap()
                .operations
                .iter()
                .any(|op| op == "resume-before")
        );
    }

    #[test]
    fn meter_abort_retains_cross_process_lease_until_guest_teardown() {
        let path = std::env::temp_dir().join(format!(
            "l6-meter-lease-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::write(&path, b"").unwrap();
        let (mut session, state) = fake(true, true);
        session.lease = Some(l6max_diagnostics::gdb::Lease::acquire(&path).unwrap());
        {
            let mut state = state.lock().unwrap();
            state.idle_snapshot = true;
            state.registers[15] = L6_METER_TASK;
            state.meter_partial_write = true;
        }
        let deadline = Instant::now() + Duration::from_secs(1);
        assert!(
            session
                .l6_meter_skip_startup_before(deadline, || false)
                .is_err()
        );
        session.l6_meter_abort_before(deadline).unwrap();
        assert!(l6max_diagnostics::gdb::Lease::acquire(&path).is_err());
        session.retained_lease.lock().unwrap().take();
        assert!(l6max_diagnostics::gdb::Lease::acquire(&path).is_ok());
        std::fs::remove_file(&path).unwrap();
        let _ = std::fs::remove_file(format!("{}.lease", path.display()));
    }

    #[test]
    fn meter_socket_session_completes_and_leaves_guest_terminal() {
        use std::{
            io::{BufRead, BufReader, Write},
            os::unix::net::UnixStream,
            thread,
        };
        let (session, state) = fake(true, true);
        {
            let mut state = state.lock().unwrap();
            state.idle_snapshot = true;
            state.registers[15] = L6_METER_TASK;
        }
        let unusable = session.unusable.clone();
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
        write!(client, "{{\"command\":\"l6-meter-guard\",\"timeout_ms\":1000}}\n{{\"command\":\"l6-meter-skip-startup\",\"timeout_ms\":1000}}\n{{\"command\":\"l6-meter-activate\",\"timeout_ms\":1000}}\n").unwrap();
        for expected in [
            "l6-meter-guarded",
            "l6-meter-startup-skipped",
            "l6-meter-activated",
        ] {
            line.clear();
            reader.read_line(&mut line).unwrap();
            assert!(line.contains(expected), "{line}");
        }
        state.lock().unwrap().registers[15] = L6_METER_BUILDER;
        write!(client, "{{\"command\":\"l6-meter-seed\",\"value\":1500000,\"timeout_ms\":1000}}\n{{\"command\":\"l6-meter-finish\",\"timeout_ms\":1000}}\n").unwrap();
        for expected in ["l6-meter-seeded", "l6-meter-finished"] {
            line.clear();
            reader.read_line(&mut line).unwrap();
            assert!(line.contains(expected), "{line}");
        }
        worker.join().unwrap();
        assert!(unusable.load(Ordering::Acquire));
        assert!(
            !state
                .lock()
                .unwrap()
                .operations
                .iter()
                .any(|op| op == "resume-before")
        );
    }

    #[test]
    fn meter_socket_eof_after_mutation_stops_and_poison_session() {
        use std::{
            io::{BufRead, BufReader, Write},
            os::unix::net::UnixStream,
            thread,
        };
        let (session, state) = fake(true, true);
        {
            let mut state = state.lock().unwrap();
            state.idle_snapshot = true;
            state.registers[15] = L6_METER_TASK;
        }
        let unusable = session.unusable.clone();
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
            "{{\"command\":\"l6-meter-skip-startup\",\"timeout_ms\":1000}}"
        )
        .unwrap();
        line.clear();
        reader.read_line(&mut line).unwrap();
        assert!(line.contains("l6-meter-startup-skipped"), "{line}");
        drop(reader);
        drop(client);
        worker.join().unwrap();
        assert!(unusable.load(Ordering::Acquire));
        assert!(
            !state
                .lock()
                .unwrap()
                .operations
                .iter()
                .any(|op| op == "resume" || op == "resume-before")
        );
    }

    #[test]
    fn meter_socket_commands_reject_invalid_timeout_before_write() {
        let (mut session, state) = fake(false, true);
        state.lock().unwrap().idle_snapshot = true;
        assert!(
            session
                .execute(Command::L6MeterSkipStartup { timeout_ms: 0 })
                .is_err()
        );
        assert!(
            session
                .execute(Command::L6MeterGuard { timeout_ms: 60_001 })
                .is_err()
        );
        assert!(
            !state
                .lock()
                .unwrap()
                .operations
                .iter()
                .any(|op| op == "write")
        );
    }

    #[test]
    fn meter_restores_journal_but_retains_terminal_ownership() {
        let (mut session, state) = fake(true, true);
        {
            let mut state = state.lock().unwrap();
            state.idle_snapshot = true;
            state.registers[15] = L6_METER_TASK;
            state.meter_memory.insert(L6_METER_GATES, vec![7, 8]);
            state
                .meter_memory
                .insert(L6_METER_ACCUMULATORS, vec![9; 40]);
        }
        let deadline = Instant::now() + Duration::from_secs(1);
        session
            .l6_meter_skip_startup_before(deadline, || false)
            .unwrap();
        state.lock().unwrap().registers[15] = L6_METER_BUILDER;
        session
            .l6_meter_seed_before(1_500_000, deadline, || false)
            .unwrap();
        session.l6_meter_finish_before(deadline, || false).unwrap();
        let state = state.lock().unwrap();
        assert_eq!(state.meter_memory[&L6_METER_CALL], L6_METER_CALL_BYTES);
        assert_eq!(state.meter_memory[&L6_METER_GATES], [7, 8]);
        assert_eq!(state.meter_memory[&L6_METER_ACCUMULATORS], vec![9; 40]);
        assert!(!state.operations.iter().any(|op| op == "resume-before"));
        assert!(session.unusable.load(Ordering::Acquire));
    }

    #[test]
    fn meter_rejects_other_active_mutation_and_restore_failure_is_terminal() {
        let (mut session, state) = fake(true, true);
        {
            let mut state = state.lock().unwrap();
            state.idle_snapshot = true;
            state.registers[15] = L6_METER_TASK;
        }
        let deadline = Instant::now() + Duration::from_secs(1);
        session.scratch = Some(vec![9; SCRATCH_LEN]);
        assert!(
            session
                .l6_meter_skip_startup_before(deadline, || false)
                .is_err()
        );
        assert!(
            !state
                .lock()
                .unwrap()
                .operations
                .iter()
                .any(|op| op == "write")
        );
        session.scratch = None;
        session
            .l6_meter_skip_startup_before(deadline, || false)
            .unwrap();
        assert!(session.execute(Command::L6ScratchRead).is_err());
        state.lock().unwrap().fail = Some("write");
        assert!(session.l6_meter_finish_before(deadline, || false).is_err());
        assert!(session.close_before(deadline).is_err());
        assert!(session.unusable.load(Ordering::Acquire));
        assert!(
            !state
                .lock()
                .unwrap()
                .operations
                .iter()
                .any(|op| op == "resume-before")
        );
    }

    #[test]
    fn idle_evidence_rejects_running_guest_before_register_reads() {
        let (mut session, state) = fake(true, true);
        state.lock().unwrap().idle_snapshot = false;
        let error = session
            .execute(Command::L6DinIdleEvidence { timeout_ms: 1000 })
            .unwrap_err();
        assert!(error.contains("requires stopped MAIN"));
        assert!(
            !state
                .lock()
                .unwrap()
                .operations
                .iter()
                .any(|operation| operation == "register")
        );
    }

    #[test]
    fn idle_evidence_cancellation_precedes_guest_reads() {
        let (mut session, state) = fake(true, true);
        let error = session
            .execute_cancellable(Command::L6DinIdleEvidence { timeout_ms: 1000 }, || true)
            .unwrap_err();
        assert!(error.contains("cancelled"));
        assert!(state.lock().unwrap().operations.is_empty());
    }
    #[test]
    fn idle_evidence_rechecks_cancellation_after_final_status() {
        let (mut session, state) = fake(true, true);
        let cancelled = Arc::new(AtomicBool::new(false));
        {
            let mut state = state.lock().unwrap();
            state.idle_snapshot = true;
            state.cancel_on_second_status = Some(cancelled.clone());
            state.registers[15] = firmware_api::l6::idle_loop;
            state.registers[13] = 0x80200800;
            state.registers[25] = 1 << 24;
        }
        let error = session
            .execute_cancellable(Command::L6DinIdleEvidence { timeout_ms: 1000 }, || {
                cancelled.load(Ordering::Acquire)
            })
            .unwrap_err();
        assert!(error.contains("cancelled"));
        assert_eq!(state.lock().unwrap().status_calls, 2);
    }

    #[test]
    fn din_injection_checks_headroom_before_any_write() {
        let (mut session, state) = fake(true, true);
        session.l6_din_semaphore = Some(0x80201000);
        session.breakpoints.insert(firmware_api::l6::idle_loop);
        session
            .breakpoints
            .insert(firmware_api::l6::main_wait_return);
        {
            let mut s = state.lock().unwrap();
            s.idle_snapshot = true;
            s.registers[15] = firmware_api::l6::idle_loop;
            s.registers[13] = 0x80200300;
            s.registers[25] = 1 << 24;
        }
        let error = session
            .execute(Command::L6DinInject {
                group: 3,
                cc: 92,
                value: 35,
                timeout_ms: 1000,
                leave_stopped: true,
            })
            .unwrap_err();
        assert!(error.contains("2048"));
        assert!(
            !state
                .lock()
                .unwrap()
                .operations
                .iter()
                .any(|op| op == "write" || op == "write-register")
        );
    }

    #[test]
    fn din_injection_write_failure_requires_stopped_restart() {
        let (mut session, state) = fake(true, true);
        session.l6_din_semaphore = Some(0x80201000);
        session.breakpoints.insert(firmware_api::l6::idle_loop);
        session
            .breakpoints
            .insert(firmware_api::l6::main_wait_return);
        {
            let mut s = state.lock().unwrap();
            s.idle_snapshot = true;
            s.registers[15] = firmware_api::l6::idle_loop;
            s.registers[13] = 0x80200f00;
            s.registers[25] = 1 << 24;
            s.fail = Some("write-register");
        }
        assert!(
            session
                .execute(Command::L6DinInject {
                    group: 3,
                    cc: 92,
                    value: 35,
                    timeout_ms: 1000,
                    leave_stopped: true
                })
                .is_err()
        );
        assert!(session.l6_din.is_some());
        assert!(
            session
                .close_before(Instant::now() + Duration::from_secs(1))
                .is_err()
        );
        assert!(session.unusable.load(Ordering::Acquire));
    }

    #[test]
    fn din_semaphore_capture_rejects_unaligned_and_non_ram_pointers() {
        for address in [0, 0x80001000, 0x80200001, 0x841fffe0] {
            let (mut session, state) = fake(true, true);
            session.breakpoints.insert(firmware_api::l6::main_wait_call);
            {
                let mut state = state.lock().unwrap();
                state.idle_snapshot = true;
                state.registers[15] = firmware_api::l6::main_wait_call;
                state.registers[1] = u32::MAX;
                state.registers[0] = address;
            }
            assert!(
                session
                    .execute(Command::L6DinCaptureSemaphore { timeout_ms: 1000 })
                    .is_err()
            );
            assert!(session.l6_din_semaphore.is_none());
        }
    }

    #[test]
    fn din_finish_stops_before_detach_and_retains_terminal_state() {
        let (mut session, state) = fake(true, true);
        session.l6_din = Some(L6DinContext {
            stack: 0x80200800,
            original_stack: [1, 2, 3, 4],
        });
        {
            let mut state = state.lock().unwrap();
            state.idle_snapshot = true;
            state.registers[15] = firmware_api::l6::idle_loop;
            state.registers[13] = 0x80200800;
            state.registers[25] = 1 << 24;
        }
        assert!(matches!(
            session
                .execute(Command::L6DinFinish { timeout_ms: 1000 })
                .unwrap(),
            Observation::L6DinFinished {
                requires_restart: true
            }
        ));
        let state = state.lock().unwrap();
        let disconnect = state
            .operations
            .iter()
            .position(|op| op == "disconnect")
            .unwrap();
        assert!(state.operations[..disconnect].iter().any(|op| op == "stop"));
        assert!(
            state.operations[disconnect + 1..]
                .iter()
                .any(|op| op == "stop")
        );
        assert!(session.unusable.load(Ordering::Acquire));
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
    fn explicit_close_resume_keeps_lease_through_resume() {
        let (mut session, state) = fake(false, true);
        let deadline = Instant::now() + Duration::from_secs(1);
        session
            .breakpoint_before(0x8000_1000, true, deadline)
            .unwrap();
        session.close_resuming_before(deadline).unwrap();
        let operations = &state.lock().unwrap().operations;
        let remove = operations
            .iter()
            .position(|operation| operation == "remove")
            .unwrap();
        let disconnect = operations
            .iter()
            .position(|operation| operation == "disconnect")
            .unwrap();
        let resume = operations
            .iter()
            .position(|operation| operation == "resume-before")
            .unwrap();
        assert!(remove < disconnect && disconnect < resume);
    }

    #[test]
    fn owned_observer_resume_failure_poisoned_before_cleanup() {
        let (mut session, state) = fake(true, true);
        state.lock().unwrap().fail = Some("resume-before");
        assert!(
            session
                .resume_owned_before(Instant::now() + Duration::from_secs(1))
                .is_err()
        );
        assert!(session.poisoned);
        assert!(
            session
                .close_stopped_before(Instant::now() + Duration::from_secs(1))
                .is_err()
        );
        assert!(session.unusable.load(Ordering::Acquire));
    }

    #[test]
    fn emergency_cancellation_retains_cross_process_lease() {
        use std::os::unix::net::{UnixListener, UnixStream};
        let dir = std::env::temp_dir().join(format!(
            "l6-debug-emergency-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("stub.sock");
        let listener = UnixListener::bind(&path).unwrap();
        let original = l6max_diagnostics::gdb::Lease::acquire(&path).unwrap();
        let duplicate = original.try_clone().unwrap();
        let (socket, _peer) = UnixStream::pair().unwrap();
        let unusable = Arc::new(AtomicBool::new(false));
        let retained = Arc::new(Mutex::new(None));
        let handle = CancellationHandle {
            socket,
            unusable: unusable.clone(),
            retained_lease: retained.clone(),
            lease: Some(duplicate),
        };
        handle.poison_and_shutdown().unwrap();
        drop(original);
        assert!(unusable.load(Ordering::Acquire));
        assert!(l6max_diagnostics::gdb::Lease::acquire(&path).is_err());
        retained.lock().unwrap().take();
        assert!(l6max_diagnostics::gdb::Lease::acquire(&path).is_ok());
        drop(listener);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn six_sweep_breakpoints_are_owned_and_cleaned_up() {
        let (mut session, state) = fake(false, true);
        let deadline = Instant::now() + Duration::from_secs(1);
        for index in 0..6 {
            session
                .breakpoint_before(0x8000_1000 + index * 2, true, deadline)
                .unwrap();
        }
        assert!(
            session
                .breakpoint_before(0x8000_2000, true, deadline)
                .is_err()
        );
        session.close_before(deadline).unwrap();
        assert_eq!(
            state
                .lock()
                .unwrap()
                .operations
                .iter()
                .filter(|operation| *operation == "remove")
                .count(),
            6
        );
    }

    #[test]
    fn bounded_close_removes_owned_breakpoints_before_restoring_run_state() {
        let (mut session, state) = fake(true, true);
        let deadline = Instant::now() + Duration::from_secs(1);
        session
            .breakpoint_before(0x8000_1000, true, deadline)
            .unwrap();
        session
            .breakpoint_before(0x8000_2000, true, deadline)
            .unwrap();
        session.close_before(deadline).unwrap();
        let operations = &state.lock().unwrap().operations;
        let removals = operations
            .iter()
            .enumerate()
            .filter(|(_, operation)| *operation == "remove")
            .map(|(index, _)| index)
            .collect::<Vec<_>>();
        let disconnected = operations
            .iter()
            .position(|operation| operation == "disconnect")
            .unwrap();
        let resumed = operations
            .iter()
            .position(|operation| operation == "resume-before")
            .unwrap();
        assert_eq!(removals.len(), 2);
        assert!(removals[1] < disconnected && disconnected < resumed);
    }
    #[test]
    fn bounded_close_failed_breakpoint_removal_stops_and_retains_unusable_state() {
        let (mut session, state) = fake(true, true);
        let deadline = Instant::now() + Duration::from_secs(1);
        session
            .breakpoint_before(0x8000_1000, true, deadline)
            .unwrap();
        state.lock().unwrap().fail = Some("remove");
        assert!(session.close_before(deadline).is_err());
        let state = state.lock().unwrap();
        assert!(state.operations.contains(&"stop".into()));
        assert!(!state.operations.contains(&"resume-before".into()));
        assert!(session.unusable.load(Ordering::Acquire));
    }
    #[test]
    fn bounded_continue_interrupts_on_deadline() {
        let (mut session, state) = fake(true, true);
        let setup = Instant::now() + Duration::from_secs(1);
        session.breakpoint_before(0x8000_1000, true, setup).unwrap();
        state.lock().unwrap().pending = true;
        assert!(
            session
                .continue_before(Instant::now() + Duration::from_millis(15), || false)
                .is_err()
        );
        assert!(
            state
                .lock()
                .unwrap()
                .operations
                .contains(&"interrupt".into())
        );
        session
            .close_before(Instant::now() + Duration::from_secs(1))
            .unwrap();
    }

    #[test]
    fn bounded_optional_continue_interrupts_timeout_and_step_uses_verified_stop() {
        let (mut session, state) = fake(true, true);
        let setup = Instant::now() + Duration::from_secs(1);
        session.breakpoint_before(0x80001000, true, setup).unwrap();
        state.lock().unwrap().pending = true;
        assert!(
            session
                .continue_optional_before(Instant::now() + Duration::from_millis(15), || false)
                .unwrap()
                .is_none()
        );
        assert!(
            state
                .lock()
                .unwrap()
                .operations
                .contains(&"interrupt".into())
        );
        state.lock().unwrap().pending = false;
        session.breakpoint_before(0x80001000, false, setup).unwrap();
        session.step_before(setup, || false).unwrap();
        assert!(
            state
                .lock()
                .unwrap()
                .operations
                .contains(&"step-start".into())
        );
        session.close_before(setup).unwrap();
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
