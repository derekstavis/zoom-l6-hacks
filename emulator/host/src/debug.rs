//! Scoped ownership of the optional QEMU MAIN GDB stub.
//!
//! The lease coordinates clients using `l6max-diagnostics`; arbitrary
//! third-party GDB clients that bypass it cannot be coordinated. A session
//! stops MAIN rather than resuming it if debugger state becomes uncertain. The panel is not stopped by this API.
use crate::{midi_config, qmp::QmpClient};
use l6fw::Model;
use l6max_diagnostics::gdb::Gdb;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeSet,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

const SCRATCH: u32 = 0x841f_f000;
const SCRATCH_LEN: usize = 8;
const MAX_BREAKPOINTS: usize = 4;
const MAX_CONTINUE_MS: u64 = 60_000;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "command", rename_all = "kebab-case", deny_unknown_fields)]
pub enum Command {
    Register { index: u32 },
    Read { address: u32, length: usize },
    AddBreakpoint { address: u32 },
    RemoveBreakpoint { address: u32 },
    Continue { timeout_ms: u64 },
    Step,
    L6ScratchRead,
    L6ScratchWrite { bytes: [u8; SCRATCH_LEN] },
    Close,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum Observation {
    Register { index: u32, value: u32 },
    Memory { address: u32, bytes: Vec<u8> },
    Breakpoint { address: u32, installed: bool },
    Stopped { reply: String },
    Scratch { address: u32, bytes: Vec<u8> },
    Closed,
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
    fn stop(&self) -> Result<(), String>;
    fn resume(&self) -> Result<(), String>;
    fn guard_l6_scratch(&self) -> Result<(), String>;
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
    fn guard_l6_scratch(&self) -> Result<(), String> {
        midi_config::config(&self.qmp, self.model).map(|_| ())
    }
}
trait Transport: Send {
    fn read_register(&mut self, index: u32) -> Result<u32, String>;
    fn read(&mut self, address: u32, length: usize) -> Result<Vec<u8>, String>;
    fn write(&mut self, address: u32, bytes: &[u8]) -> Result<(), String>;
    fn breakpoint(&mut self, address: u32, enable: bool) -> Result<(), String>;
    fn resume(&mut self) -> Result<(), String>;
    fn poll_stop(&mut self, wait: Duration) -> Result<Option<String>, String>;
    fn interrupt(&mut self) -> Result<String, String>;
    fn step(&mut self) -> Result<String, String>;
    fn disconnect(&mut self) -> Result<(), String>;
}
impl Transport for Gdb {
    fn read_register(&mut self, index: u32) -> Result<u32, String> {
        Gdb::read_register(self, index).map_err(|e| e.to_string())
    }
    fn read(&mut self, address: u32, length: usize) -> Result<Vec<u8>, String> {
        Gdb::read(self, address, length).map_err(|e| e.to_string())
    }
    fn write(&mut self, address: u32, bytes: &[u8]) -> Result<(), String> {
        Gdb::write(self, address, bytes).map_err(|e| e.to_string())
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

pub struct Session {
    transport: Option<Box<dyn Transport>>,
    guest: Arc<dyn Guest>,
    was_running: bool,
    poisoned: bool,
    breakpoints: BTreeSet<u32>,
    scratch: Option<Vec<u8>>,
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
    pub fn execute(&mut self, command: Command) -> Result<Observation, String> {
        self.execute_cancellable(command, || false)
    }

    /// Execute one command, checking cancellation while the target runs.
    pub fn execute_cancellable(
        &mut self,
        command: Command,
        mut cancelled: impl FnMut() -> bool,
    ) -> Result<Observation, String> {
        match command {
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
            Command::Close => {
                self.close()?;
                Ok(Observation::Closed)
            }
        }
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
        if !self.poisoned {
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
        fail: Option<&'static str>,
        guarded: bool,
        pending: bool,
    }
    struct FakeGuest(Arc<Mutex<FakeState>>);
    impl Guest for FakeGuest {
        fn running(&self) -> Result<bool, String> {
            Ok(true)
        }
        fn stop(&self) -> Result<(), String> {
            self.0.lock().unwrap().operations.push("stop".into());
            Ok(())
        }
        fn resume(&self) -> Result<(), String> {
            self.0.lock().unwrap().operations.push("resume".into());
            Ok(())
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
        fn read_register(&mut self, _: u32) -> Result<u32, String> {
            self.record("register")?;
            Ok(42)
        }
        fn read(&mut self, address: u32, length: usize) -> Result<Vec<u8>, String> {
            self.record("read")?;
            if address == SCRATCH {
                Ok(self.0.lock().unwrap().scratch.clone())
            } else {
                Ok(vec![0; length])
            }
        }
        fn write(&mut self, _: u32, bytes: &[u8]) -> Result<(), String> {
            self.record("write")?;
            self.0.lock().unwrap().scratch = bytes.to_vec();
            Ok(())
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
