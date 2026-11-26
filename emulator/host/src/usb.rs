//! Host tokens for the emulated ChipIdea device, over an inherited socket.
//! Firmware owns descriptors, class handlers and endpoint buffers.
use std::{
    io::{self, Read, Write},
    net::Shutdown,
    os::unix::net::UnixStream,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
const MAGIC: u32 = 0x4c365553;
const MAX: usize = 16384;
/// Shared deadline and cancellation for an owned USB transaction.
#[derive(Clone)]
pub struct Budget {
    state: Arc<Mutex<(Instant, Arc<dyn Fn() -> bool + Send + Sync>)>>,
}
impl Budget {
    pub fn new(deadline: Instant, cancelled: impl Fn() -> bool + Send + Sync + 'static) -> Self {
        Self {
            state: Arc::new(Mutex::new((deadline, Arc::new(cancelled)))),
        }
    }
    fn child(&self, deadline: Instant) -> Self {
        let parent = self.clone();
        Self::new(deadline, move || parent.check().is_err())
    }
    /// Reserve a separate emergency window for explicitly requested cleanup.
    pub fn cleanup_before(&self, deadline: Instant) {
        *self.state.lock().unwrap() = (deadline, Arc::new(|| false));
    }
    pub fn check(&self) -> io::Result<()> {
        let (deadline, cancelled) = {
            let state = self.state.lock().unwrap();
            (state.0, state.1.clone())
        };
        if Instant::now() >= deadline || cancelled() {
            Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "USB transaction deadline exceeded or cancelled",
            ))
        } else {
            Ok(())
        }
    }
    fn slice(&self) -> io::Result<Duration> {
        self.check()?;
        let deadline = self.state.lock().unwrap().0;
        Ok(deadline
            .saturating_duration_since(Instant::now())
            .min(Duration::from_millis(50))
            .max(Duration::from_millis(1)))
    }
}

/// Adds a description, except for the transient errors callers retry on
/// (ENODEV while the guest's USB controller is still coming up, EAGAIN on a
/// socket timeout): `io::Error::new` always clears `raw_os_error()` even
/// when passed the same `ErrorKind`, so wrapping one of these here would
/// make every retry-on-`raw_os_error()` check above this call stop matching
/// and treat a startup race as a fatal error instead of retrying it.
fn context(error: io::Error, operation: &str) -> io::Error {
    match error.raw_os_error() {
        Some(libc::ENODEV) | Some(libc::EAGAIN) | Some(libc::ECONNRESET) => error,
        _ => io::Error::new(error.kind(), format!("{operation}: {error}")),
    }
}
pub struct UsbHost {
    wire: Arc<Wire>,
    control_lock: Mutex<()>,
    shutdown_socket: UnixStream,
    pumps: Mutex<Vec<(u8, InPump)>>,
    ownership: Arc<Mutex<UsbOwner>>,
    reserved_enumerated: AtomicBool,
}
/// A bridge and direct controller client must not concurrently own USB.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum UsbOwner {
    #[default]
    None,
    Raw,
    Bridge,
    Direct,
    RawDirect,
    Unusable,
}

/// Shared access to the guest USB controller, separate from MIDI bridge ownership.
#[derive(Clone)]
pub struct Controller {
    host: Arc<UsbHost>,
    owner: Arc<Mutex<UsbOwner>>,
}

impl Controller {
    pub(crate) fn new(host: Arc<UsbHost>) -> Self {
        Self {
            owner: host.ownership.clone(),
            host,
        }
    }
    /// Attach and reserve raw USB until release or engine shutdown.
    pub fn attach(&self) -> Result<(), String> {
        let mut owner = self
            .owner
            .lock()
            .map_err(|_| "USB ownership lock poisoned")?;
        match *owner {
            UsbOwner::Bridge => return Err("USB is owned by the native MIDI bridge".into()),
            UsbOwner::Direct | UsbOwner::RawDirect => {
                return Err("USB is owned by a direct MIDI session".into());
            }
            UsbOwner::Raw => return Ok(()),
            UsbOwner::Unusable => return Err("USB transport unusable; restart guest".into()),
            UsbOwner::None => {}
        }
        self.host.connect(true).map_err(|error| error.to_string())?;
        *owner = UsbOwner::Raw;
        Ok(())
    }
    /// Release a raw USB attachment and detach the cable.
    pub fn release(&self) -> Result<(), String> {
        let mut owner = self
            .owner
            .lock()
            .map_err(|_| "USB ownership lock poisoned")?;
        match *owner {
            UsbOwner::Bridge => return Err("USB is owned by the native MIDI bridge".into()),
            UsbOwner::Direct | UsbOwner::RawDirect => {
                return Err("USB is owned by a direct MIDI session".into());
            }
            UsbOwner::None => return Err("raw USB is not attached".into()),
            UsbOwner::Raw => {}
            UsbOwner::Unusable => return Err("USB transport unusable; restart guest".into()),
        }
        self.host
            .connect(false)
            .map_err(|error| error.to_string())?;
        self.host
            .reserved_enumerated
            .store(false, Ordering::Release);
        *owner = UsbOwner::None;
        Ok(())
    }
    /// Probe whether the attached guest controller answers tokens.
    pub fn running(&self) -> Result<bool, String> {
        let owner = self
            .owner
            .lock()
            .map_err(|_| "USB ownership lock poisoned")?;
        if *owner == UsbOwner::Bridge {
            return Err("USB is owned by the native MIDI bridge".into());
        }
        if *owner == UsbOwner::Unusable {
            return Err("USB transport unusable; restart guest".into());
        }
        if matches!(*owner, UsbOwner::Direct | UsbOwner::RawDirect) {
            return Err("USB is owned by a direct MIDI session".into());
        }
        self.host
            .controller_running()
            .map_err(|error| error.to_string())
    }
    /// Enumerate under an existing raw reservation; the configured MIDI pump
    /// remains owned by the engine until explicit release or shutdown.
    pub fn enumerate_before(&self, budget: &Budget) -> Result<Enumeration, String> {
        let mut owner = loop {
            budget.check().map_err(|e| e.to_string())?;
            match self.owner.try_lock() {
                Ok(owner) => break owner,
                Err(std::sync::TryLockError::WouldBlock) => thread::sleep(Duration::from_millis(1)),
                Err(_) => return Err("USB ownership lock poisoned".into()),
            }
        };
        if *owner != UsbOwner::Raw {
            return Err("USB enumeration requires an exclusive raw attachment".into());
        }
        if self.host.reserved_enumerated.load(Ordering::Acquire) {
            return Err("USB already enumerated; release before enumerating again".into());
        }
        match self.host.enumerate_with_descriptors_before(budget) {
            Ok((device, configuration, parsed)) => {
                self.host.reserved_enumerated.store(true, Ordering::Release);
                Ok(Enumeration {
                    device,
                    configuration,
                    parsed,
                })
            }
            Err(error) => {
                *owner = UsbOwner::Unusable;
                self.host.shutdown();
                Err(format!("USB enumeration failed: {error}; restart guest"))
            }
        }
    }
}

/// Releases a direct MIDI reservation even if enumeration fails.
pub(crate) struct DirectLease {
    owner: Arc<Mutex<UsbOwner>>,
}
impl DirectLease {
    /// Roll back failed setup without detaching a pre-existing raw owner.
    pub(crate) fn rollback_before(&self, host: &UsbHost, budget: &Budget) -> Result<(), String> {
        let owner = self
            .owner
            .lock()
            .map_err(|_| "USB ownership lock poisoned")?;
        if *owner == UsbOwner::Direct {
            host.disconnect_before(budget).map_err(|e| e.to_string())?;
        }
        Ok(())
    }
    /// On failed direct setup, detach only if this session attached an unowned cable.
    pub(crate) fn rollback(&self, host: &UsbHost) -> Result<(), String> {
        let owner = self
            .owner
            .lock()
            .map_err(|_| "USB ownership lock poisoned")?;
        if *owner == UsbOwner::Direct {
            host.connect(false).map_err(|error| error.to_string())?;
        }
        Ok(())
    }
}
impl Drop for DirectLease {
    fn drop(&mut self) {
        let mut owner = self
            .owner
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        *owner = match *owner {
            UsbOwner::Direct => UsbOwner::None,
            UsbOwner::RawDirect => UsbOwner::Raw,
            other => other,
        };
    }
}

/// The token socket, shared with IN pumps so they do not own the host.
struct Wire {
    socket: Mutex<UnixStream>,
    sequence: Mutex<u32>,
}
impl UsbHost {
    pub fn new(socket: UnixStream) -> io::Result<Self> {
        socket.set_read_timeout(Some(Duration::from_secs(3)))?;
        socket.set_write_timeout(Some(Duration::from_secs(3)))?;
        Ok(Self {
            shutdown_socket: socket.try_clone()?,
            wire: Arc::new(Wire {
                socket: Mutex::new(socket),
                sequence: Mutex::new(0),
            }),
            control_lock: Mutex::new(()),
            pumps: Mutex::new(Vec::new()),
            ownership: Arc::new(Mutex::new(UsbOwner::None)),
            reserved_enumerated: AtomicBool::new(false),
        })
    }
    pub(crate) fn ownership(&self) -> Arc<Mutex<UsbOwner>> {
        self.ownership.clone()
    }
    /// Reserve USB for one direct MIDI session, retaining any earlier raw attachment.
    pub(crate) fn acquire_direct(self: &Arc<Self>) -> Result<DirectLease, String> {
        let mut owner = self
            .ownership
            .lock()
            .map_err(|_| "USB ownership lock poisoned")?;
        *owner = match *owner {
            UsbOwner::None => UsbOwner::Direct,
            UsbOwner::Raw => UsbOwner::RawDirect,
            _ => return Err("USB already has a MIDI owner".into()),
        };
        Ok(DirectLease {
            owner: self.ownership.clone(),
        })
    }
    pub fn shutdown(&self) {
        // Close the socket first so a pump blocked on a reply exits at once.
        let _ = self.shutdown_socket.shutdown(Shutdown::Both);
        self.pumps.lock().unwrap().clear();
    }
    fn request(&self, kind: u32, endpoint: u32, length: usize, data: &[u8]) -> io::Result<Vec<u8>> {
        self.wire.request(kind, endpoint, length, data)
    }
    /// Poll `endpoint` continuously from now on, as a configured host does;
    /// [`Self::try_receive`] then returns the queued transactions.
    fn start_in_pump(&self, endpoint: u8) -> io::Result<()> {
        let mut pumps = self.pumps.lock().unwrap();
        pumps.retain(|(pumped, _)| *pumped != endpoint);
        pumps.push((endpoint, InPump::start(self.wire.clone(), endpoint)?));
        Ok(())
    }
}
impl Wire {
    fn request(&self, kind: u32, endpoint: u32, length: usize, data: &[u8]) -> io::Result<Vec<u8>> {
        if length > MAX {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "USB transfer exceeds 16 KiB",
            ));
        }
        let mut sock = self.socket.lock().unwrap();
        let mut seq = self.sequence.lock().unwrap();
        *seq = seq.wrapping_add(1).max(1);
        let wire = (|| -> io::Result<(i32, Vec<u8>)> {
            for word in [MAGIC, kind, *seq, endpoint, length as u32] {
                sock.write_all(&word.to_le_bytes())?;
            }
            sock.write_all(data)?;
            let mut header = [0u8; 20];
            sock.read_exact(&mut header)?;
            let word = |i: usize| u32::from_le_bytes(header[i * 4..i * 4 + 4].try_into().unwrap());
            if word(0) != MAGIC
                || word(1) != 0x80000000
                || word(2) != *seq
                || word(4) as usize > MAX
            {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "invalid USB reply",
                ));
            }
            let mut bytes = vec![0; word(4) as usize];
            sock.read_exact(&mut bytes)?;
            Ok((word(3) as i32, bytes))
        })();
        match wire {
            Ok((0, bytes)) => Ok(bytes),
            Ok((status, _)) => Err(io::Error::from_raw_os_error(-status)),
            Err(error) => {
                let _ = sock.shutdown(Shutdown::Both);
                Err(error)
            }
        }
    }
}
impl Wire {
    fn request_before(
        &self,
        kind: u32,
        endpoint: u32,
        length: usize,
        data: &[u8],
        budget: &Budget,
    ) -> io::Result<Vec<u8>> {
        if length > MAX {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "USB transfer exceeds 16 KiB",
            ));
        }
        let mut socket = loop {
            budget.check()?;
            match self.socket.try_lock() {
                Ok(v) => break v,
                Err(std::sync::TryLockError::WouldBlock) => thread::sleep(Duration::from_millis(1)),
                Err(_) => return Err(io::Error::other("USB wire lock poisoned")),
            }
        };
        budget.check()?;
        let mut seq = self
            .sequence
            .lock()
            .map_err(|_| io::Error::other("USB sequence lock poisoned"))?;
        *seq = seq.wrapping_add(1).max(1);
        let mut packet = Vec::with_capacity(20 + data.len());
        for w in [MAGIC, kind, *seq, endpoint, length as u32] {
            packet.extend_from_slice(&w.to_le_bytes());
        }
        packet.extend_from_slice(data);
        fn transfer(
            socket: &mut UnixStream,
            bytes: &mut [u8],
            write: bool,
            budget: &Budget,
        ) -> io::Result<()> {
            let mut done = 0;
            while done < bytes.len() {
                let wait = budget.slice()?;
                if write {
                    socket.set_write_timeout(Some(wait))?;
                } else {
                    socket.set_read_timeout(Some(wait))?;
                }
                let result = if write {
                    socket.write(&bytes[done..])
                } else {
                    socket.read(&mut bytes[done..])
                };
                match result {
                    Ok(0) => {
                        return Err(io::Error::new(
                            io::ErrorKind::UnexpectedEof,
                            "USB wire closed",
                        ));
                    }
                    Ok(n) => done += n,
                    Err(e)
                        if matches!(
                            e.kind(),
                            io::ErrorKind::WouldBlock
                                | io::ErrorKind::TimedOut
                                | io::ErrorKind::Interrupted
                        ) =>
                    {
                        continue;
                    }
                    Err(e) => return Err(e),
                }
            }
            budget.check()
        }
        let result = (|| {
            transfer(&mut socket, &mut packet, true, budget)?;
            let mut header = [0u8; 20];
            transfer(&mut socket, &mut header, false, budget)?;
            let word = |i: usize| u32::from_le_bytes(header[i * 4..i * 4 + 4].try_into().unwrap());
            if word(0) != MAGIC
                || word(1) != 0x80000000
                || word(2) != *seq
                || word(4) as usize > MAX
            {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "invalid USB reply",
                ));
            }
            let mut bytes = vec![0; word(4) as usize];
            transfer(&mut socket, &mut bytes, false, budget)?;
            let status = word(3) as i32;
            Ok((status, bytes))
        })();
        let _ = socket.set_read_timeout(Some(Duration::from_secs(3)));
        let _ = socket.set_write_timeout(Some(Duration::from_secs(3)));
        match result {
            Ok((0, bytes)) => Ok(bytes),
            Ok((status, _)) => Err(io::Error::from_raw_os_error(-status)),
            Err(e) => {
                let _ = socket.shutdown(Shutdown::Both);
                Err(e)
            }
        }
    }
}
impl UsbHost {
    /// Attach or detach the cable. Detaching stops host polling; the next
    /// enumeration restarts it.
    pub fn connect(&self, connected: bool) -> io::Result<()> {
        if !connected {
            self.pumps.lock().unwrap().clear();
        }
        self.request(1, connected as u32, 0, &[]).map(|_| ())
    }
    /// Attach within a caller-owned wall deadline and cancellation budget.
    pub fn attach_before(&self, budget: &Budget) -> io::Result<()> {
        self.wire.request_before(1, 1, 0, &[], budget).map(|_| ())
    }
    fn token(&self, kind: u32, endpoint: u8, length: usize, data: &[u8]) -> io::Result<Vec<u8>> {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match self.request(kind, endpoint as u32, length, data) {
                Err(e) if e.raw_os_error() == Some(libc::EAGAIN) && Instant::now() < deadline => {
                    thread::sleep(Duration::from_millis(2))
                }
                result => return result,
            }
        }
    }
    fn token_before(
        &self,
        kind: u32,
        endpoint: u8,
        length: usize,
        data: &[u8],
        budget: &Budget,
    ) -> io::Result<Vec<u8>> {
        let local = Instant::now() + Duration::from_secs(5);
        let local_budget = budget.child(local);
        loop {
            local_budget.check()?;
            match self
                .wire
                .request_before(kind, endpoint as u32, length, data, &local_budget)
            {
                Err(e) if e.raw_os_error() == Some(libc::EAGAIN) && Instant::now() < local => {
                    thread::sleep(Duration::from_millis(2))
                }
                other => return other,
            }
        }
    }
    pub fn receive_before(
        &self,
        endpoint: u8,
        length: usize,
        budget: &Budget,
    ) -> io::Result<Vec<u8>> {
        self.token_before(3, endpoint | 0x80, length, &[], budget)
    }
    pub fn send_before(&self, endpoint: u8, data: &[u8], budget: &Budget) -> io::Result<()> {
        self.token_before(4, endpoint & 15, data.len(), data, budget)
            .map(|_| ())
    }
    fn control_token_before(
        &self,
        kind: u32,
        endpoint: u8,
        length: usize,
        data: &[u8],
        budget: &Budget,
    ) -> io::Result<Vec<u8>> {
        loop {
            budget.check()?;
            match self
                .wire
                .request_before(kind, endpoint as u32, length, data, budget)
            {
                Err(e) if e.raw_os_error() == Some(libc::EAGAIN) => {
                    thread::sleep(Duration::from_millis(2))
                }
                result => return result,
            }
        }
    }
    /// Enumerate under a caller-owned deadline, retaining the production retry rules.
    pub fn enumerate_before(&self, budget: &Budget) -> io::Result<Configuration> {
        self.enumerate_with_descriptors_before(budget)
            .map(|(_, _, config)| config)
    }
    /// Enumerate with the production transfer sequence and retain its raw evidence.
    pub fn enumerate_with_descriptors_before(
        &self,
        budget: &Budget,
    ) -> io::Result<(Vec<u8>, Vec<u8>, Configuration)> {
        budget.check()?;
        self.wire.request_before(1, 1, 0, &[], budget)?;
        let _guard = loop {
            budget.check()?;
            match self.control_lock.try_lock() {
                Ok(v) => break v,
                Err(std::sync::TryLockError::WouldBlock) => thread::sleep(Duration::from_millis(1)),
                Err(_) => return Err(io::Error::other("USB control lock poisoned")),
            }
        };
        let control = |setup: [u8; 8], expected: Option<usize>| -> io::Result<Vec<u8>> {
            let local = Instant::now() + Duration::from_secs(30);
            let local_budget = budget.child(local);
            let budget = &local_budget;
            loop {
                budget.check()?;
                if Instant::now() >= local {
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "USB control deadline",
                    ));
                }
                let request = (|| {
                    self.wire.request_before(2, 0, 8, &setup, budget)?;
                    let data = self.control_token_before(3, 0x80, MAX, &[], budget)?;
                    if setup[0] & 0x80 != 0 {
                        if data.len() > u16::from_le_bytes([setup[6], setup[7]]) as usize {
                            return Err(io::Error::new(
                                io::ErrorKind::InvalidData,
                                "control response exceeds wLength",
                            ));
                        }
                        self.control_token_before(4, 0, 0, &[], budget)?;
                    }
                    Ok(data)
                })();
                match request {
                    Ok(data) if expected.is_none_or(|n| data.len() == n) => return Ok(data),
                    Ok(_) => thread::sleep(Duration::from_millis(25)),
                    Err(e) if retry_transfer(&e) => thread::sleep(Duration::from_millis(25)),
                    Err(e) => return Err(e),
                }
            }
        };
        let device = control([0x80, 6, 0, 1, 0, 0, 18, 0], Some(18))?;
        if device[..2] != [18, 1] {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid USB device descriptor",
            ));
        }
        let bytes = control([0x80, 6, 0, 2, 0, 0, 0xff, 0x3f], None)?;
        let config = Configuration::parse(&bytes)?;
        control([0, 9, config.value, 0, 0, 0, 0, 0], None)?;
        if let Some(midi) = config.midi {
            self.start_in_pump(midi.input)?;
        }
        budget.check()?;
        Ok((device, bytes, config))
    }
    fn disconnect_before(&self, budget: &Budget) -> io::Result<()> {
        let mut guard = loop {
            budget.check()?;
            match self.pumps.try_lock() {
                Ok(v) => break v,
                Err(std::sync::TryLockError::WouldBlock) => thread::sleep(Duration::from_millis(1)),
                Err(_) => return Err(io::Error::other("USB pump lock poisoned")),
            }
        };
        let pumps = std::mem::take(&mut *guard);
        drop(guard);
        let mut error = None;
        for (_, mut pump) in pumps {
            if let Err(e) = pump.stop_before(budget) {
                error.get_or_insert(e);
            }
        }
        if let Some(error) = error {
            return Err(error);
        }
        self.wire.request_before(1, 0, 0, &[], budget).map(|_| ())
    }
    /// Whether the device answers tokens: attached, with firmware's
    /// controller running. Probes an endpoint the device does not implement,
    /// so no firmware buffer is touched.
    pub fn controller_running(&self) -> io::Result<bool> {
        match self.request(3, 0x8f, 0, &[]) {
            Err(error) if error.raw_os_error() == Some(libc::ENODEV) => Ok(false),
            Err(error) if error.raw_os_error() == Some(libc::EAGAIN) => Ok(true),
            Err(error) => Err(error),
            Ok(_) => Ok(true),
        }
    }
    pub fn controller_running_before(&self, budget: &Budget) -> io::Result<bool> {
        budget.check()?;
        let result = match self.wire.request_before(3, 0x8f, 0, &[], budget) {
            Err(error) if error.raw_os_error() == Some(libc::ENODEV) => Ok(false),
            Err(error) if error.raw_os_error() == Some(libc::EAGAIN) => Ok(true),
            Err(error) => Err(error),
            Ok(_) => Ok(true),
        };
        budget.check()?;
        result
    }
    pub fn receive(&self, endpoint: u8, length: usize) -> io::Result<Vec<u8>> {
        self.token(3, endpoint | 0x80, length, &[])
    }
    /// One IN transaction. An unprimed endpoint returns EAGAIN without waiting.
    /// An endpoint the host is already polling returns its oldest queued
    /// transaction instead, or EAGAIN when none is waiting.
    pub fn try_receive(&self, endpoint: u8, length: usize) -> io::Result<Vec<u8>> {
        if let Some((_, pump)) = self
            .pumps
            .lock()
            .unwrap()
            .iter()
            .find(|(pumped, _)| *pumped == endpoint)
        {
            return pump.try_receive();
        }
        self.request(3, (endpoint | 0x80) as u32, length, &[])
    }
    pub fn send(&self, endpoint: u8, data: &[u8]) -> io::Result<()> {
        self.token(4, endpoint & 15, data.len(), data).map(|_| ())
    }
    pub fn control(&self, setup: [u8; 8]) -> io::Result<Vec<u8>> {
        let _control = self.control_lock.lock().unwrap();
        self.request(2, 0, 8, &setup)
            .map_err(|e| context(e, "USB SETUP packet"))?;
        self.control_data_stage(setup)
    }

    fn control_data_stage(&self, setup: [u8; 8]) -> io::Result<Vec<u8>> {
        let length = u16::from_le_bytes([setup[6], setup[7]]) as usize;
        if setup[0] & 0x80 != 0 {
            let data = self
                .receive(0, MAX)
                .map_err(|error| context(error, "USB control IN data stage"))?;
            if data.len() > length {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "control response exceeds wLength",
                ));
            }
            self.send(0, &[])
                .map_err(|error| context(error, "USB control IN status stage"))?;
            Ok(data)
        } else if length == 0 {
            self.receive(0, MAX)
                .map_err(|error| context(error, "USB control OUT status stage"))
        } else {
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "control OUT data stage",
            ))
        }
    }

    fn retry_control_token(
        &self,
        endpoint: u8,
        input: bool,
        data: &[u8],
        deadline: Instant,
    ) -> io::Result<Vec<u8>> {
        loop {
            let result = if input {
                self.try_receive(endpoint, MAX)
            } else {
                self.request(4, (endpoint & 15) as u32, data.len(), data)
            };
            match result {
                Err(error)
                    if error.raw_os_error() == Some(libc::EAGAIN) && Instant::now() < deadline =>
                {
                    thread::sleep(Duration::from_millis(2));
                }
                result => return result,
            }
        }
    }

    fn control_data_stage_raw(&self, setup: [u8; 8], deadline: Instant) -> io::Result<Vec<u8>> {
        let length = u16::from_le_bytes([setup[6], setup[7]]) as usize;
        if setup[0] & 0x80 != 0 {
            let data = self.retry_control_token(0, true, &[], deadline)?;
            if data.len() > length {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "control response exceeds wLength",
                ));
            }
            self.retry_control_token(0, false, &[], deadline)?;
            Ok(data)
        } else if length == 0 {
            self.retry_control_token(0, true, &[], deadline)
        } else {
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "control OUT data stage",
            ))
        }
    }

    fn control_once_raw(&self, setup: [u8; 8], deadline: Instant) -> io::Result<Vec<u8>> {
        self.request(2, 0, 8, &setup)?;
        self.control_data_stage_raw(setup, deadline)
    }

    fn control_with_retry(&self, setup: [u8; 8]) -> io::Result<Vec<u8>> {
        let _control = self.control_lock.lock().unwrap();
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            match self.control_once_raw(setup, deadline) {
                Err(error) if retry_transfer(&error) && Instant::now() < deadline => {
                    thread::sleep(Duration::from_millis(25));
                }
                result => return result,
            }
        }
    }

    pub fn enumerate(&self) -> io::Result<Configuration> {
        self.enumerate_with_descriptors()
            .map(|(_, _, config)| config)
    }

    /// Enumerate through the production retry path, retaining wire evidence.
    pub fn enumerate_with_descriptors(&self) -> io::Result<(Vec<u8>, Vec<u8>, Configuration)> {
        // A SETUP that firmware discards while handling the bus reset is
        // reported as ECONNRESET and resent.
        self.connect(true)
            .map_err(|e| context(e, "connect USB device"))?;
        let device = self.device_descriptor()?;
        if device.len() != 18 || device[..2] != [18, 1] {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("invalid device descriptor {device:02x?}"),
            ));
        }
        let bytes = self
            .control_with_retry([0x80, 6, 0, 2, 0, 0, 0xff, 0x3f])
            .map_err(|e| context(e, "get USB configuration descriptor"))?;
        let config = Configuration::parse(&bytes)?;
        self.control_with_retry([0, 9, config.value, 0, 0, 0, 0, 0])
            .map_err(|e| context(e, "set USB configuration"))?;
        // A configured host polls MIDI IN continuously; firmware may abandon
        // a transfer that is not collected promptly.
        if let Some(midi) = config.midi {
            self.start_in_pump(midi.input)?;
        }
        Ok((device, bytes, config))
    }

    fn device_descriptor(&self) -> io::Result<Vec<u8>> {
        let setup = [0x80, 6, 0, 1, 0, 0, 18, 0];
        let deadline = Instant::now() + Duration::from_secs(30);
        let _control = self.control_lock.lock().unwrap();
        loop {
            match self.request(2, 0, 8, &setup) {
                Ok(_) => {
                    match self.control_data_stage_raw(setup, deadline) {
                        Ok(descriptor) if descriptor.len() == 18 => return Ok(descriptor),
                        Ok(descriptor) if descriptor.len() < 18 && Instant::now() < deadline => {
                            // Some firmware revisions prime EP0 with a short
                            // initial descriptor fragment while USB startup is
                            // settling. Restart the control transfer and wait
                            // for the complete fixed-size device descriptor.
                            thread::sleep(Duration::from_millis(25));
                        }
                        Ok(descriptor) => {
                            return Err(io::Error::new(
                                io::ErrorKind::UnexpectedEof,
                                format!("short USB device descriptor: {} bytes", descriptor.len()),
                            ));
                        }
                        Err(error) if retry_transfer(&error) && Instant::now() < deadline => {
                            // The setup packet can be accepted before the
                            // firmware primes EP0's IN dTD. Retry the whole
                            // control transfer until the device controller is
                            // ready, as well as retrying a rejected SETUP.
                            thread::sleep(Duration::from_millis(25));
                        }
                        Err(error) => {
                            return Err(context(error, "get USB device descriptor"));
                        }
                    }
                }
                Err(error) if retry_transfer(&error) && Instant::now() < deadline => {
                    // The controller can reject SETUP until the firmware has
                    // enabled its device role after VBUS.
                    thread::sleep(Duration::from_millis(25));
                }
                Err(error) => return Err(context(error, "get USB device descriptor")),
            }
        }
    }
}

/// Polls one bulk IN endpoint continuously, as a real host does once a
/// device is configured, and queues every transaction it returns. Firmware
/// flushes an IN transfer that the host does not collect within about 100 ms.
struct InPump {
    state: Arc<Mutex<PumpState>>,
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}
#[derive(Default)]
struct PumpState {
    transactions: std::collections::VecDeque<Vec<u8>>,
    error: Option<(io::ErrorKind, Option<i32>, String)>,
}
/// Queued transactions beyond which the pump reports an overflow rather than
/// discarding data.
const PUMP_LIMIT: usize = 4096;
impl InPump {
    fn start(wire: Arc<Wire>, endpoint: u8) -> io::Result<Self> {
        let state = Arc::new(Mutex::new(PumpState::default()));
        let stop = Arc::new(AtomicBool::new(false));
        let (worker_state, worker_stop) = (state.clone(), stop.clone());
        let worker = thread::Builder::new()
            .name(format!("usb-in-{endpoint}"))
            .spawn(move || {
                while !worker_stop.load(Ordering::Relaxed) {
                    match wire.request(3, (endpoint | 0x80) as u32, MAX, &[]) {
                        Ok(bytes) if bytes.is_empty() => {}
                        Ok(bytes) => {
                            let mut state = worker_state.lock().unwrap();
                            if state.transactions.len() >= PUMP_LIMIT {
                                state.error = Some((
                                    io::ErrorKind::OutOfMemory,
                                    None,
                                    format!("USB IN endpoint {endpoint} queue overflowed"),
                                ));
                                return;
                            }
                            state.transactions.push_back(bytes);
                            continue;
                        }
                        Err(error) if error.raw_os_error() == Some(libc::EAGAIN) => {}
                        Err(error) => {
                            worker_state.lock().unwrap().error =
                                Some((error.kind(), error.raw_os_error(), error.to_string()));
                            return;
                        }
                    }
                    thread::sleep(Duration::from_millis(1));
                }
            })?;
        Ok(Self {
            state,
            stop,
            worker: Some(worker),
        })
    }
    fn stop_before(&mut self, budget: &Budget) -> io::Result<()> {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(worker) = self.worker.take() {
            while !worker.is_finished() {
                if let Err(error) = budget.check() {
                    drop(worker);
                    return Err(error);
                }
                thread::sleep(Duration::from_millis(1));
            }
            worker
                .join()
                .map_err(|_| io::Error::other("USB input pump panicked"))?;
        }
        Ok(())
    }
    /// The oldest queued IN transaction. EAGAIN means none is waiting, like an
    /// unprimed endpoint; a failure that stopped the pump is returned once
    /// the queue is drained.
    fn try_receive(&self) -> io::Result<Vec<u8>> {
        let mut state = self.state.lock().unwrap();
        if let Some(bytes) = state.transactions.pop_front() {
            return Ok(bytes);
        }
        Err(match &state.error {
            Some((_, Some(code), _)) => io::Error::from_raw_os_error(*code),
            Some((kind, None, message)) => io::Error::new(*kind, message.clone()),
            None => io::Error::from_raw_os_error(libc::EAGAIN),
        })
    }
}
impl Drop for InPump {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

/// A control transfer that may succeed when restarted with a new SETUP:
/// ENODEV until firmware runs its controller, EAGAIN while EP0 is unprimed
/// (bounded by the caller's deadline), and ECONNRESET when the device model
/// saw firmware discard the pending SETUP by flushing endpoints or resetting.
fn retry_transfer(error: &io::Error) -> bool {
    matches!(
        error.raw_os_error(),
        Some(libc::ENODEV) | Some(libc::EAGAIN) | Some(libc::ECONNRESET)
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Endpoints {
    pub interface: u8,
    pub input: u8,
    pub output: u8,
}
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Configuration {
    pub value: u8,
    pub midi: Option<Endpoints>,
    pub storage: Option<Endpoints>,
}
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Enumeration {
    pub device: Vec<u8>,
    pub configuration: Vec<u8>,
    pub parsed: Configuration,
}
impl Configuration {
    pub fn parse(bytes: &[u8]) -> io::Result<Self> {
        let bad = || io::Error::new(io::ErrorKind::InvalidData, "malformed USB configuration");
        if bytes.len() < 9
            || bytes[..2] != [9, 2]
            || u16::from_le_bytes([bytes[2], bytes[3]]) as usize != bytes.len()
        {
            return Err(bad());
        }
        let mut result = Self {
            value: bytes[5],
            midi: None,
            storage: None,
        };
        let mut class = 0;
        let mut pair = Endpoints {
            interface: 0,
            input: 0,
            output: 0,
        };
        let mut offset = 0;
        while offset < bytes.len() {
            let length = bytes[offset] as usize;
            if length < 2 || offset + length > bytes.len() {
                return Err(bad());
            }
            let d = &bytes[offset..offset + length];
            if d[1] == 4 {
                if length < 9 {
                    return Err(bad());
                }
                class = if d[3] != 0 {
                    0
                } else if d[5..7] == [1, 3] {
                    1
                } else if d[5..8] == [8, 6, 0x50] {
                    2
                } else {
                    0
                };
                pair = Endpoints {
                    interface: d[2],
                    input: 0,
                    output: 0,
                };
            } else if d[1] == 5 && class != 0 {
                if length < 7 {
                    return Err(bad());
                }
                if d[3] & 3 == 2 {
                    if d[2] & 0x80 != 0 {
                        pair.input = d[2] & 15;
                    } else {
                        pair.output = d[2] & 15;
                    }
                    if pair.input != 0 && pair.output != 0 {
                        if class == 1 {
                            result.midi = Some(pair);
                        } else {
                            result.storage = Some(pair);
                        }
                    }
                }
            }
            offset += length;
        }
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bounded_enumeration_retains_exact_descriptor_bytes_and_configuration() {
        let (socket, mut peer) = UnixStream::pair().unwrap();
        let device = vec![
            18, 1, 0, 2, 0, 0, 0, 8, 0x34, 0x12, 0x78, 0x56, 0, 1, 0, 0, 0, 1,
        ];
        let config = vec![
            9, 2, 32, 0, 1, 1, 0, 0x80, 50, 9, 4, 2, 0, 2, 1, 3, 0, 0, 7, 5, 0x81, 2, 64, 0, 0, 7,
            5, 2, 2, 64, 0, 0,
        ];
        let expected_device = device.clone();
        let expected_config = config.clone();
        let server = thread::spawn(move || {
            for (kind, endpoint, payload) in [
                (1, 1, vec![]),
                (2, 0, vec![]),
                (3, 0x80, device),
                (4, 0, vec![]),
                (2, 0, vec![]),
                (3, 0x80, config),
                (4, 0, vec![]),
                (2, 0, vec![]),
                (3, 0x80, vec![]),
            ] {
                let mut request = vec![0; if kind == 2 { 28 } else { 20 }];
                peer.read_exact(&mut request).unwrap();
                assert_eq!(u32::from_le_bytes(request[4..8].try_into().unwrap()), kind);
                assert_eq!(
                    u32::from_le_bytes(request[12..16].try_into().unwrap()),
                    endpoint
                );
                let sequence = u32::from_le_bytes(request[8..12].try_into().unwrap());
                let mut reply = Vec::new();
                for word in [MAGIC, 0x80000000, sequence, 0, payload.len() as u32] {
                    reply.extend(word.to_le_bytes());
                }
                reply.extend(payload);
                peer.write_all(&reply).unwrap();
            }
            let mut first = true;
            loop {
                let mut request = [0u8; 20];
                if peer.read_exact(&mut request).is_err() {
                    break;
                }
                assert_eq!(u32::from_le_bytes(request[4..8].try_into().unwrap()), 3);
                assert_eq!(
                    u32::from_le_bytes(request[12..16].try_into().unwrap()),
                    0x81
                );
                let sequence = u32::from_le_bytes(request[8..12].try_into().unwrap());
                let (status, data): (i32, &[u8]) = if first {
                    first = false;
                    (0, &[0x1b, 0xb0, 0x51, 32])
                } else {
                    (-libc::EAGAIN, &[])
                };
                let mut reply = Vec::new();
                for word in [
                    MAGIC,
                    0x80000000,
                    sequence,
                    status as u32,
                    data.len() as u32,
                ] {
                    reply.extend(word.to_le_bytes());
                }
                reply.extend(data);
                if peer.write_all(&reply).is_err() {
                    break;
                }
            }
        });
        let host = Arc::new(UsbHost::new(socket).unwrap());
        let controller = Controller::new(host.clone());
        *host.ownership.lock().unwrap() = UsbOwner::Raw;
        let budget = Budget::new(Instant::now() + Duration::from_secs(5), || false);
        let result = controller.enumerate_before(&budget).unwrap();
        assert_eq!(result.device, expected_device);
        assert_eq!(result.configuration, expected_config);
        assert_eq!(result.parsed.value, 1);
        assert_eq!(
            result.parsed.midi,
            Some(Endpoints {
                interface: 2,
                input: 1,
                output: 2
            })
        );
        assert_eq!(*host.ownership.lock().unwrap(), UsbOwner::Raw);
        assert!(
            controller
                .enumerate_before(&budget)
                .unwrap_err()
                .contains("already enumerated")
        );
        let until = Instant::now() + Duration::from_secs(2);
        loop {
            match host.try_receive(1, 64) {
                Ok(bytes) => {
                    assert_eq!(bytes, [0x1b, 0xb0, 0x51, 32]);
                    break;
                }
                Err(error)
                    if error.raw_os_error() == Some(libc::EAGAIN) && Instant::now() < until =>
                {
                    thread::sleep(Duration::from_millis(5))
                }
                Err(error) => panic!("{error}"),
            }
        }
        host.shutdown();
        server.join().unwrap();
    }
    #[test]
    fn reserved_enumeration_rejects_conflicts_and_poisons_failed_wire() {
        let (socket, peer) = UnixStream::pair().unwrap();
        let host = Arc::new(UsbHost::new(socket).unwrap());
        let controller = Controller::new(host.clone());
        let budget = Budget::new(Instant::now() + Duration::from_secs(2), || false);
        assert!(controller.enumerate_before(&budget).is_err());
        *host.ownership.lock().unwrap() = UsbOwner::Bridge;
        assert!(controller.enumerate_before(&budget).is_err());
        *host.ownership.lock().unwrap() = UsbOwner::Raw;
        drop(peer);
        assert!(
            controller
                .enumerate_before(&budget)
                .unwrap_err()
                .contains("restart guest")
        );
        assert_eq!(*host.ownership.lock().unwrap(), UsbOwner::Unusable);
        assert!(controller.attach().is_err());
        assert!(host.acquire_direct().is_err());
    }
    #[test]
    fn cancelling_stalled_reserved_enumeration_poison_closes_transport() {
        let (socket, mut peer) = UnixStream::pair().unwrap();
        let host = Arc::new(UsbHost::new(socket).unwrap());
        let controller = Controller::new(host.clone());
        *host.ownership.lock().unwrap() = UsbOwner::Raw;
        let cancelled = Arc::new(AtomicBool::new(false));
        let signal = cancelled.clone();
        let peer_task = thread::spawn(move || {
            let mut request = [0u8; 20];
            peer.read_exact(&mut request).unwrap();
            signal.store(true, Ordering::Release);
            let mut byte = [0u8; 1];
            assert_eq!(peer.read(&mut byte).unwrap(), 0);
        });
        let budget = Budget::new(Instant::now() + Duration::from_secs(2), move || {
            cancelled.load(Ordering::Acquire)
        });
        assert!(
            controller
                .enumerate_before(&budget)
                .unwrap_err()
                .contains("restart guest")
        );
        assert_eq!(*host.ownership.lock().unwrap(), UsbOwner::Unusable);
        peer_task.join().unwrap();
    }
    #[test]
    fn bounded_wire_stops_on_stalled_partial_reply_and_closes_connection() {
        let (socket, mut peer) = UnixStream::pair().unwrap();
        let host = UsbHost::new(socket).unwrap();
        let worker = thread::spawn(move || {
            let mut request = [0; 20];
            peer.read_exact(&mut request).unwrap();
            peer.write_all(&[0, 1, 2]).unwrap();
            let mut byte = [0; 1];
            assert_eq!(peer.read(&mut byte).unwrap(), 0);
        });
        let budget = Budget::new(Instant::now() + Duration::from_millis(40), || false);
        assert_eq!(
            host.wire
                .request_before(3, 0x81, 8, &[], &budget)
                .unwrap_err()
                .kind(),
            io::ErrorKind::TimedOut
        );
        worker.join().unwrap();
    }
    #[test]
    fn bounded_wire_checks_deadline_while_waiting_for_lock() {
        let (socket, _peer) = UnixStream::pair().unwrap();
        let host = UsbHost::new(socket).unwrap();
        let _guard = host.wire.socket.lock().unwrap();
        let budget = Budget::new(Instant::now() + Duration::from_millis(20), || false);
        assert_eq!(
            host.wire
                .request_before(3, 0x81, 8, &[], &budget)
                .unwrap_err()
                .kind(),
            io::ErrorKind::TimedOut
        );
    }
    #[test]
    fn child_budget_cannot_outlive_or_ignore_parent_cancellation() {
        let flag = Arc::new(AtomicBool::new(false));
        let other = flag.clone();
        let parent = Budget::new(Instant::now() + Duration::from_secs(5), move || {
            other.load(Ordering::Relaxed)
        });
        let child = parent.child(Instant::now() + Duration::from_secs(30));
        assert!(child.check().is_ok());
        flag.store(true, Ordering::Relaxed);
        assert!(child.check().is_err());
    }

    #[test]
    fn controller_attaches_only_without_bridge_and_reports_guest_readiness() {
        let (socket, mut peer) = UnixStream::pair().unwrap();
        let server = thread::spawn(move || {
            for (kind, endpoint, status) in [
                (1u32, 1u32, 0),
                (3, 0x8f, -libc::ENODEV),
                (3, 0x8f, -libc::EAGAIN),
            ] {
                let mut request = [0; 20];
                peer.read_exact(&mut request).unwrap();
                let word =
                    |i: usize| u32::from_le_bytes(request[i * 4..i * 4 + 4].try_into().unwrap());
                assert_eq!((word(1), word(3)), (kind, endpoint));
                let mut reply = Vec::new();
                for value in [MAGIC, 0x80000000, word(2), status as u32, 0] {
                    reply.extend(value.to_le_bytes());
                }
                peer.write_all(&reply).unwrap();
            }
        });
        let owned = Arc::new(Mutex::new(UsbOwner::Bridge));
        let controller = Controller {
            host: Arc::new(UsbHost::new(socket).unwrap()),
            owner: owned.clone(),
        };
        assert!(controller.attach().unwrap_err().contains("bridge"));
        *owned.lock().unwrap() = UsbOwner::None;
        controller.attach().unwrap();
        assert!(!controller.running().unwrap());
        assert!(controller.running().unwrap());
        server.join().unwrap();
    }

    #[test]
    fn raw_midi_receiver_retains_exclusive_lease_until_drop() {
        let (socket, mut peer) = UnixStream::pair().unwrap();
        let server = thread::spawn(move || {
            let mut request = [0; 20];
            peer.read_exact(&mut request).unwrap();
            let word = |i: usize| u32::from_le_bytes(request[i * 4..i * 4 + 4].try_into().unwrap());
            assert_eq!((word(1), word(3)), (1, 1));
            let mut reply = Vec::new();
            for value in [MAGIC, 0x8000_0000, word(2), 0, 0] {
                reply.extend(value.to_le_bytes());
            }
            peer.write_all(&reply).unwrap();
        });
        let host = Arc::new(UsbHost::new(socket).unwrap());
        let controller = Controller::new(host.clone());
        let mut receiver = crate::usb_midi::RawReceiver::attach(host.clone()).unwrap();
        assert!(receiver.poll().is_err());
        assert!(host.acquire_direct().is_err());
        assert!(controller.attach().is_err());
        drop(receiver);
        assert!(host.acquire_direct().is_ok());
        server.join().unwrap();
    }

    #[test]
    fn claiming_enumerated_midi_preserves_queued_transfer_and_sends_on_cable_one() {
        let (socket, mut peer) = UnixStream::pair().unwrap();
        let server = thread::spawn(move || {
            for (kind, endpoint) in [(3u32, 0x81u32), (4, 2)] {
                let mut header = [0u8; 20];
                peer.read_exact(&mut header).unwrap();
                let word =
                    |i: usize| u32::from_le_bytes(header[i * 4..i * 4 + 4].try_into().unwrap());
                assert_eq!((word(1), word(3)), (kind, endpoint));
                if kind == 4 {
                    let mut payload = [0; 4];
                    peer.read_exact(&mut payload).unwrap();
                    assert_eq!(payload, [0x1b, 0xb0, 0x53, 32]);
                }
                let payload: &[u8] = if kind == 3 {
                    &[0x2b, 0xb0, 0x53, 42]
                } else {
                    &[]
                };
                for value in [MAGIC, 0x8000_0000, word(2), 0, payload.len() as u32] {
                    peer.write_all(&value.to_le_bytes()).unwrap();
                }
                peer.write_all(payload).unwrap();
            }
        });
        let host = Arc::new(UsbHost::new(socket).unwrap());
        let endpoints = Endpoints {
            interface: 1,
            input: 1,
            output: 2,
        };
        let mut receiver =
            crate::usb_midi::RawReceiver::claim_enumerated(host.clone(), endpoints).unwrap();
        assert!(host.acquire_direct().is_err());
        let (raw, messages) = receiver.poll_transfer().unwrap().unwrap();
        assert_eq!(raw, [0x2b, 0xb0, 0x53, 42]);
        assert_eq!(messages, vec![(2, vec![0xb0, 0x53, 42])]);
        receiver.send_control_change(1, 0, 0x53, 32).unwrap();
        assert!(receiver.send_control_change(16, 0, 0x53, 32).is_err());
        drop(receiver);
        assert!(host.acquire_direct().is_ok());
        server.join().unwrap();
    }

    #[test]
    fn failed_raw_midi_receiver_attach_rolls_back_and_releases_lease() {
        let (socket, mut peer) = UnixStream::pair().unwrap();
        let server = thread::spawn(move || {
            for (endpoint, status) in [(1u32, -libc::ENODEV), (0, 0)] {
                let mut request = [0; 20];
                peer.read_exact(&mut request).unwrap();
                let word =
                    |i: usize| u32::from_le_bytes(request[i * 4..i * 4 + 4].try_into().unwrap());
                assert_eq!((word(1), word(3)), (1, endpoint));
                let mut reply = Vec::new();
                for value in [MAGIC, 0x8000_0000, word(2), status as u32, 0] {
                    reply.extend(value.to_le_bytes());
                }
                peer.write_all(&reply).unwrap();
            }
        });
        let host = Arc::new(UsbHost::new(socket).unwrap());
        assert!(crate::usb_midi::RawReceiver::attach(host.clone()).is_err());
        assert!(host.acquire_direct().is_ok());
        server.join().unwrap();
    }

    #[test]
    fn direct_reservation_blocks_controller_and_restores_raw_owner() {
        let (socket, _peer) = UnixStream::pair().unwrap();
        let host = Arc::new(UsbHost::new(socket).unwrap());
        let controller = Controller::new(host.clone());
        *host.ownership.lock().unwrap() = UsbOwner::Raw;
        let lease = host.acquire_direct().unwrap();
        assert!(controller.attach().unwrap_err().contains("direct MIDI"));
        assert!(controller.running().unwrap_err().contains("direct MIDI"));
        assert!(controller.release().unwrap_err().contains("direct MIDI"));
        assert!(host.acquire_direct().is_err());
        drop(lease);
        assert_eq!(*host.ownership.lock().unwrap(), UsbOwner::Raw);
        *host.ownership.lock().unwrap() = UsbOwner::Bridge;
        assert!(host.acquire_direct().is_err());
        assert!(controller.attach().is_err());
        assert!(controller.running().is_err());
    }

    #[test]
    fn failed_direct_setup_detaches_only_without_preexisting_raw_owner() {
        let (socket, mut peer) = UnixStream::pair().unwrap();
        let server = thread::spawn(move || {
            let mut request = [0; 20];
            peer.read_exact(&mut request).unwrap();
            let word = |i: usize| u32::from_le_bytes(request[i * 4..i * 4 + 4].try_into().unwrap());
            assert_eq!((word(1), word(3)), (1, 0));
            let mut reply = Vec::new();
            for value in [MAGIC, 0x80000000, word(2), 0, 0] {
                reply.extend(value.to_le_bytes());
            }
            peer.write_all(&reply).unwrap();
        });
        let host = Arc::new(UsbHost::new(socket).unwrap());
        let lease = host.acquire_direct().unwrap();
        lease.rollback(&host).unwrap();
        drop(lease);
        assert_eq!(*host.ownership.lock().unwrap(), UsbOwner::None);
        *host.ownership.lock().unwrap() = UsbOwner::Raw;
        let lease = host.acquire_direct().unwrap();
        lease.rollback(&host).unwrap();
        drop(lease);
        assert_eq!(*host.ownership.lock().unwrap(), UsbOwner::Raw);
        server.join().unwrap();
    }

    fn configuration() -> Vec<u8> {
        let mut d = vec![9, 2, 0, 0, 2, 1, 0, 0xc0, 4];
        d.extend([
            9, 4, 4, 0, 2, 1, 3, 0, 0, 7, 5, 3, 2, 0, 2, 0, 7, 5, 0x84, 2, 0, 2, 0,
        ]);
        d.extend([
            9, 4, 5, 0, 2, 8, 6, 0x50, 0, 7, 5, 0x85, 2, 0, 2, 0, 7, 5, 6, 2, 0, 2, 0,
        ]);
        let length = d.len() as u16;
        d[2..4].copy_from_slice(&length.to_le_bytes());
        d
    }
    #[test]
    fn endpoints_are_read_from_the_firmware_configuration() {
        let c = Configuration::parse(&configuration()).unwrap();
        assert_eq!(
            c.midi,
            Some(Endpoints {
                interface: 4,
                input: 4,
                output: 3
            })
        );
        assert_eq!(
            c.storage,
            Some(Endpoints {
                interface: 5,
                input: 5,
                output: 6
            })
        );
        let mut bad = configuration();
        bad[9] = 0;
        assert!(Configuration::parse(&bad).is_err());
        assert!(Configuration::parse(&configuration()[..12]).is_err());
    }
    #[test]
    fn device_descriptor_waits_for_guest_usb_controller_enable() {
        let (socket, mut peer) = UnixStream::pair().unwrap();
        let server = thread::spawn(move || {
            for status in [-libc::ENODEV, 0] {
                let mut request = [0; 28];
                peer.read_exact(&mut request).unwrap();
                assert_eq!(u32::from_le_bytes(request[4..8].try_into().unwrap()), 2);
                assert_eq!(&request[20..], &[0x80, 6, 0, 1, 0, 0, 18, 0]);
                let sequence = u32::from_le_bytes(request[8..12].try_into().unwrap());
                let mut reply = Vec::new();
                for word in [MAGIC, 0x80000000, sequence, status as u32, 0] {
                    reply.extend(word.to_le_bytes());
                }
                peer.write_all(&reply).unwrap();
            }
            for (kind, endpoint, data) in [(3, 0x80, vec![0; 18]), (4, 0, vec![])] {
                let mut request = [0; 20];
                peer.read_exact(&mut request).unwrap();
                assert_eq!(u32::from_le_bytes(request[4..8].try_into().unwrap()), kind);
                assert_eq!(
                    u32::from_le_bytes(request[12..16].try_into().unwrap()),
                    endpoint
                );
                let sequence = u32::from_le_bytes(request[8..12].try_into().unwrap());
                let mut reply = Vec::new();
                for word in [MAGIC, 0x80000000, sequence, 0, data.len() as u32] {
                    reply.extend(word.to_le_bytes());
                }
                reply.extend(data);
                peer.write_all(&reply).unwrap();
            }
        });
        let usb = UsbHost::new(socket).unwrap();
        assert_eq!(usb.device_descriptor().unwrap(), [0; 18]);
        server.join().unwrap();
    }
    #[test]
    fn device_descriptor_retries_when_ep0_is_not_primed_after_setup() {
        let (socket, mut peer) = UnixStream::pair().unwrap();
        let server = thread::spawn(move || {
            let mut receive_reply = |kind: u32, endpoint: u32, status: i32, data: &[u8]| {
                let mut request = [0; 28];
                let request_len = if kind == 2 { 28 } else { 20 };
                peer.read_exact(&mut request[..request_len]).unwrap();
                assert_eq!(u32::from_le_bytes(request[4..8].try_into().unwrap()), kind);
                assert_eq!(
                    u32::from_le_bytes(request[12..16].try_into().unwrap()),
                    endpoint
                );
                let sequence = u32::from_le_bytes(request[8..12].try_into().unwrap());
                let mut reply = Vec::new();
                for word in [
                    MAGIC,
                    0x80000000,
                    sequence,
                    status as u32,
                    data.len() as u32,
                ] {
                    reply.extend(word.to_le_bytes());
                }
                reply.extend(data);
                peer.write_all(&reply).unwrap();
            };

            // SETUP succeeds while the device is still enabling its USB
            // controller, so the following EP0 IN transaction returns ENODEV.
            receive_reply(2, 0, 0, &[]);
            receive_reply(3, 0x80, -libc::ENODEV, &[]);
            // The first post-startup attempt can return only the two-byte
            // descriptor header; retry the full control transfer once more.
            receive_reply(2, 0, 0, &[]);
            receive_reply(3, 0x80, 0, &[18, 1]);
            receive_reply(4, 0, 0, &[]);
            // The next SETUP receives the complete device descriptor.
            receive_reply(2, 0, 0, &[]);
            receive_reply(
                3,
                0x80,
                0,
                &[18, 1, 0, 2, 0, 0, 0, 8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1],
            );
            receive_reply(4, 0, 0, &[]);
        });
        let usb = UsbHost::new(socket).unwrap();
        assert_eq!(
            usb.device_descriptor().unwrap(),
            [18, 1, 0, 2, 0, 0, 0, 8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1]
        );
        server.join().unwrap();
    }
    #[test]
    fn in_pump_collects_data_sent_while_no_consumer_is_reading() {
        let (socket, mut peer) = UnixStream::pair().unwrap();
        let server = thread::spawn(move || {
            let mut polls = 0;
            loop {
                let mut request = [0; 20];
                if peer.read_exact(&mut request).is_err() {
                    return polls;
                }
                polls += 1;
                let sequence = u32::from_le_bytes(request[8..12].try_into().unwrap());
                // Firmware primes one transfer on the fifth poll, then NAKs.
                let (status, data): (i32, &[u8]) = if polls == 5 {
                    (0, &[0x1b, 0xb0, 0x51, 32])
                } else {
                    (-libc::EAGAIN, &[])
                };
                let mut reply = Vec::new();
                for value in [
                    MAGIC,
                    0x80000000,
                    sequence,
                    status as u32,
                    data.len() as u32,
                ] {
                    reply.extend(value.to_le_bytes());
                }
                reply.extend(data);
                peer.write_all(&reply).unwrap();
            }
        });
        let usb = UsbHost::new(socket).unwrap();
        usb.start_in_pump(4).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let received = loop {
            match usb.try_receive(4, 512) {
                Ok(bytes) => break bytes,
                Err(error) if error.raw_os_error() == Some(libc::EAGAIN) => {
                    assert!(Instant::now() < deadline);
                    thread::sleep(Duration::from_millis(5));
                }
                Err(error) => panic!("{error}"),
            }
        };
        assert_eq!(received, [0x1b, 0xb0, 0x51, 32]);
        usb.shutdown();
        assert!(server.join().unwrap() >= 5);
    }
    #[test]
    fn enumeration_control_retries_transient_status_stage_eagain() {
        let (socket, mut peer) = UnixStream::pair().unwrap();
        let server = thread::spawn(move || {
            for (kind, status) in [(2, 0), (3, -libc::EAGAIN), (3, 0)] {
                let request_len = if kind == 2 { 28 } else { 20 };
                let mut request = vec![0; request_len];
                peer.read_exact(&mut request).unwrap();
                assert_eq!(u32::from_le_bytes(request[4..8].try_into().unwrap()), kind);
                if kind == 3 {
                    assert_eq!(
                        u32::from_le_bytes(request[12..16].try_into().unwrap()),
                        0x80
                    );
                }
                let sequence = u32::from_le_bytes(request[8..12].try_into().unwrap());
                let mut reply = Vec::new();
                for word in [MAGIC, 0x80000000, sequence, status as u32, 0] {
                    reply.extend(word.to_le_bytes());
                }
                peer.write_all(&reply).unwrap();
            }
        });
        let usb = UsbHost::new(socket).unwrap();
        assert!(
            usb.control_with_retry([0, 9, 1, 0, 0, 0, 0, 0])
                .unwrap()
                .is_empty()
        );
        server.join().unwrap();
    }
    #[test]
    fn fragmented_replies_and_usb_errors_preserve_stream_order() {
        let (socket, mut peer) = UnixStream::pair().unwrap();
        let server = thread::spawn(move || {
            for (status, data) in [(-libc::EAGAIN, vec![]), (0, vec![0x1b, 0xb0, 81, 32])] {
                let mut request = [0; 20];
                peer.read_exact(&mut request).unwrap();
                assert_eq!(u32::from_le_bytes(request[4..8].try_into().unwrap()), 3);
                let sequence = u32::from_le_bytes(request[8..12].try_into().unwrap());
                let mut reply = Vec::new();
                for word in [
                    MAGIC,
                    0x80000000,
                    sequence,
                    status as u32,
                    data.len() as u32,
                ] {
                    reply.extend(word.to_le_bytes());
                }
                reply.extend(data);
                for fragment in reply.chunks(3) {
                    peer.write_all(fragment).unwrap();
                }
            }
        });
        let usb = UsbHost::new(socket).unwrap();
        assert_eq!(
            usb.try_receive(4, 512).unwrap_err().raw_os_error(),
            Some(libc::EAGAIN)
        );
        assert_eq!(usb.try_receive(4, 512).unwrap(), [0x1b, 0xb0, 81, 32]);
        server.join().unwrap();
    }
    #[test]
    fn corrupt_reply_closes_the_channel() {
        let (socket, mut peer) = UnixStream::pair().unwrap();
        let server = thread::spawn(move || {
            let mut request = [0; 20];
            peer.read_exact(&mut request).unwrap();
            peer.write_all(&[0; 20]).unwrap();
            let mut b = [0];
            assert_eq!(peer.read(&mut b).unwrap(), 0);
        });
        let usb = UsbHost::new(socket).unwrap();
        assert_eq!(
            usb.connect(true).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
        server.join().unwrap();
    }
}
