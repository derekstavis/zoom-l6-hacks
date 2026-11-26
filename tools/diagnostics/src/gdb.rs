//! Minimal GDB remote client for all-stop QEMU firmware diagnostics.
use std::{
    fs::{File, OpenOptions},
    io::{self, Read, Write},
    net::Shutdown,
    os::{
        fd::AsRawFd,
        unix::{fs::OpenOptionsExt, net::UnixStream},
    },
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
pub struct Gdb {
    stream: UnixStream,
    // Every in-repository GDB client shares this process-safe lease. The OS
    // releases it on crash or disconnect; the sidecar itself stays in place.
    _lease: Option<Lease>,
    request_timeout: Duration,
}
/// QEMU's GDB register number for xPSR on M-profile cores.
const XPSR: u32 = 25;
/// The active exception number in xPSR; zero in thread mode.
const IPSR_MASK: u32 = 0x1ff;
/// Cross-process advisory ownership of one canonical GDB socket.
pub struct Lease {
    path: PathBuf,
    _file: File,
}
impl Lease {
    pub fn try_clone(&self) -> io::Result<Self> {
        Ok(Self {
            path: self.path.clone(),
            _file: self._file.try_clone()?,
        })
    }
    pub fn acquire(path: &Path) -> io::Result<Self> {
        let canonical = path.canonicalize()?;
        let mut name = canonical.as_os_str().to_os_string();
        name.push(".lease");
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(name)?;
        if !file.metadata()?.file_type().is_file() {
            return Err(io::Error::other("GDB lease path is not a regular file"));
        }
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            let error = io::Error::last_os_error();
            return Err(io::Error::new(
                error.kind(),
                format!("GDB socket already leased: {error}"),
            ));
        }
        Ok(Self {
            path: canonical,
            _file: file,
        })
    }
}
impl Gdb {
    pub fn connect(path: &Path) -> io::Result<Self> {
        Self::connect_leased(Lease::acquire(path)?)
    }
    /// Open the stub using ownership obtained before changing guest state.
    pub fn connect_leased(lease: Lease) -> io::Result<Self> {
        let socket = UnixStream::connect(&lease.path)?;
        socket.set_read_timeout(Some(Duration::from_secs(5)))?;
        socket.set_write_timeout(Some(Duration::from_secs(5)))?;
        let mut client = Self {
            stream: socket,
            _lease: Some(lease),
            request_timeout: Duration::from_secs(5),
        };
        let reply = client.request("Hg1")?;
        if reply != "OK" {
            return Err(io::Error::other(format!(
                "unexpected GDB handshake reply: {reply}"
            )));
        }
        Ok(client)
    }
    /// Connect and complete the initial handshake before one absolute deadline.
    pub fn connect_leased_before(lease: Lease, deadline: Instant) -> io::Result<Self> {
        use std::os::{
            fd::{FromRawFd, OwnedFd},
            unix::ffi::OsStrExt,
        };
        let path = lease.path.as_os_str().as_bytes();
        let mut address: libc::sockaddr_un = unsafe { std::mem::zeroed() };
        if path.len() >= address.sun_path.len() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "GDB socket path too long",
            ));
        }
        address.sun_family = libc::AF_UNIX as libc::sa_family_t;
        for (destination, source) in address.sun_path.iter_mut().zip(path) {
            *destination = *source as libc::c_char;
        }
        #[cfg(any(target_os = "macos", target_os = "ios"))]
        {
            address.sun_len =
                (std::mem::offset_of!(libc::sockaddr_un, sun_path) + path.len() + 1) as u8;
        }
        let fd = unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_STREAM, 0) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        let owned = unsafe { OwnedFd::from_raw_fd(fd) };
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
        if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
            return Err(io::Error::last_os_error());
        }
        let length =
            (std::mem::offset_of!(libc::sockaddr_un, sun_path) + path.len() + 1) as libc::socklen_t;
        let connected =
            unsafe { libc::connect(fd, &address as *const _ as *const libc::sockaddr, length) };
        if connected != 0 {
            let error = io::Error::last_os_error();
            if !matches!(
                error.raw_os_error(),
                Some(libc::EINPROGRESS) | Some(libc::EAGAIN)
            ) {
                return Err(error);
            }
            loop {
                let remaining = deadline
                    .checked_duration_since(Instant::now())
                    .filter(|v| !v.is_zero())
                    .ok_or_else(|| {
                        io::Error::new(io::ErrorKind::TimedOut, "GDB connect deadline exceeded")
                    })?;
                let mut entry = libc::pollfd {
                    fd,
                    events: libc::POLLOUT,
                    revents: 0,
                };
                let ready = unsafe {
                    libc::poll(
                        &mut entry,
                        1,
                        remaining.as_millis().max(1).min(i32::MAX as u128) as i32,
                    )
                };
                if ready == 0 {
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "GDB connect deadline exceeded",
                    ));
                }
                if ready < 0 {
                    let error = io::Error::last_os_error();
                    if error.kind() == io::ErrorKind::Interrupted {
                        continue;
                    }
                    return Err(error);
                }
                let mut error = 0i32;
                let mut size = std::mem::size_of::<i32>() as libc::socklen_t;
                if unsafe {
                    libc::getsockopt(
                        fd,
                        libc::SOL_SOCKET,
                        libc::SO_ERROR,
                        &mut error as *mut _ as *mut libc::c_void,
                        &mut size,
                    )
                } < 0
                {
                    return Err(io::Error::last_os_error());
                }
                if error != 0 {
                    return Err(io::Error::from_raw_os_error(error));
                }
                break;
            }
        }
        if unsafe { libc::fcntl(fd, libc::F_SETFL, flags) } < 0 {
            return Err(io::Error::last_os_error());
        }
        let stream = UnixStream::from(owned);
        let timeout = deadline
            .checked_duration_since(Instant::now())
            .filter(|v| !v.is_zero())
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::TimedOut, "GDB handshake deadline exceeded")
            })?;
        stream.set_read_timeout(Some(timeout))?;
        stream.set_write_timeout(Some(timeout))?;
        let mut client = Self {
            stream,
            _lease: Some(lease),
            request_timeout: timeout,
        };
        let reply = client.request_before("Hg1", deadline)?;
        if reply != "OK" {
            return Err(io::Error::other(format!(
                "unexpected GDB handshake reply: {reply}"
            )));
        }
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "GDB handshake deadline exceeded",
            ));
        }
        client.set_request_timeout(Duration::from_secs(5))?;
        Ok(client)
    }
    /// Disconnect the stub while retaining its lease until this value drops.
    pub fn disconnect(&mut self) -> io::Result<()> {
        self.stream.shutdown(Shutdown::Both)
    }

    pub fn set_request_timeout(&mut self, timeout: Duration) -> io::Result<()> {
        if timeout.is_zero() {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "GDB request deadline exceeded",
            ));
        }
        self.request_timeout = timeout;
        Ok(())
    }
    pub fn request(&mut self, command: &str) -> io::Result<String> {
        self.request_with_timeout(command, self.request_timeout)
    }

    fn request_with_timeout(&mut self, command: &str, timeout: Duration) -> io::Result<String> {
        let deadline = Instant::now()
            .checked_add(timeout)
            .ok_or_else(|| io::Error::other("GDB deadline overflow"))?;
        self.request_before(command, deadline)
    }

    fn request_before(&mut self, command: &str, deadline: Instant) -> io::Result<String> {
        let checksum = command.bytes().fold(0u8, u8::wrapping_add);
        let packet = format!("${command}#{checksum:02x}");
        self.write_bytes_before(packet.as_bytes(), deadline)?;
        let first = self.read_byte(deadline)?;
        self.receive_packet_from(first, deadline)
    }

    fn write_bytes_before(&mut self, bytes: &[u8], deadline: Instant) -> io::Result<()> {
        let mut remaining_packet = bytes;
        while !remaining_packet.is_empty() {
            let remaining = deadline
                .checked_duration_since(Instant::now())
                .filter(|v| !v.is_zero())
                .ok_or_else(|| {
                    io::Error::new(io::ErrorKind::TimedOut, "GDB packet deadline exceeded")
                })?;
            self.stream.set_write_timeout(Some(remaining))?;
            match self.stream.write(remaining_packet) {
                Ok(0) => {
                    return Err(io::Error::new(
                        io::ErrorKind::WriteZero,
                        "GDB socket closed",
                    ));
                }
                Ok(written) => remaining_packet = &remaining_packet[written..],
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }

    fn read_byte(&mut self, deadline: Instant) -> io::Result<u8> {
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .filter(|duration| !duration.is_zero())
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::TimedOut, "GDB packet deadline exceeded")
            })?;
        self.stream.set_read_timeout(Some(remaining))?;
        let mut byte = [0];
        self.stream.read_exact(&mut byte)?;
        Ok(byte[0])
    }

    fn receive_packet(&mut self) -> io::Result<String> {
        let deadline = Instant::now() + Duration::from_secs(5);
        let first = self.read_byte(deadline)?;
        self.receive_packet_from(first, deadline)
    }
    /// Parse a packet whose first byte, `first`, was already read.
    fn receive_packet_from(&mut self, mut first: u8, deadline: Instant) -> io::Result<String> {
        while first != b'$' {
            first = self.read_byte(deadline)?;
        }
        let mut encoded = Vec::new();
        loop {
            let byte = self.read_byte(deadline)?;
            if byte == b'#' {
                break;
            }
            if encoded.len() >= 65536 {
                return Err(io::Error::other("GDB packet exceeds 65536 bytes"));
            }
            encoded.push(byte);
        }
        let checksum_bytes = [self.read_byte(deadline)?, self.read_byte(deadline)?];
        let checksum_text = std::str::from_utf8(&checksum_bytes).map_err(io::Error::other)?;
        let expected = u8::from_str_radix(checksum_text, 16).map_err(io::Error::other)?;
        if encoded.iter().copied().fold(0u8, u8::wrapping_add) != expected {
            return Err(io::Error::other("GDB checksum mismatch"));
        }
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .filter(|v| !v.is_zero())
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::TimedOut,
                    "GDB acknowledgement deadline exceeded",
                )
            })?;
        self.stream.set_write_timeout(Some(remaining))?;
        self.stream.write_all(b"+")?;
        let mut data = Vec::new();
        let mut i = 0;
        while i < encoded.len() {
            match encoded[i] {
                b'}' => {
                    i += 1;
                    data.push(
                        *encoded
                            .get(i)
                            .ok_or_else(|| io::Error::other("truncated GDB escape"))?
                            ^ 0x20,
                    );
                }
                b'*' => {
                    i += 1;
                    let previous = *data
                        .last()
                        .ok_or_else(|| io::Error::other("bad GDB repeat"))?;
                    let repeat = encoded
                        .get(i)
                        .and_then(|count| count.checked_sub(29))
                        .ok_or_else(|| io::Error::other("bad GDB repeat count"))?
                        as usize;
                    if data.len() + repeat > 65536 {
                        return Err(io::Error::other("decoded GDB packet exceeds 65536 bytes"));
                    }
                    data.extend(std::iter::repeat_n(previous, repeat));
                }
                b => data.push(b),
            }
            i += 1;
        }
        String::from_utf8(data).map_err(io::Error::other)
    }
    /// Interrupt a running target and return its GDB stop reply.
    pub fn interrupt(&mut self) -> io::Result<String> {
        self.stream.write_all(&[3])?;
        self.receive_packet()
    }
    /// Bring a stopped target to thread mode. A stop can land inside an
    /// exception handler; an injected call there would run in that
    /// handler's context, where FreeRTOS calls such as taking a mutex assert
    /// and never return. Let the guest run briefly and stop it again until
    /// IPSR is zero.
    pub fn stop_in_thread_mode(&mut self) -> io::Result<()> {
        for _ in 0..200 {
            if self.read_register(XPSR)? & IPSR_MASK == 0 {
                return Ok(());
            }
            let checksum = b'c';
            self.stream
                .write_all(format!("$c#{checksum:02x}").as_bytes())?;
            std::thread::sleep(Duration::from_millis(1));
            self.interrupt()?;
        }
        Err(io::Error::other(
            "target stayed in exception context; cannot inject a call",
        ))
    }
    /// Let the target run without waiting for it to stop; collect the stop
    /// reply with [`Gdb::poll_stop`].
    pub fn resume(&mut self) -> io::Result<()> {
        self.stream.write_all(b"$c#63")
    }
    /// The stop reply of a target [`Gdb::resume`] let run, or `None` when it
    /// is still running after `wait`. A timeout leaves the stream intact.
    pub fn poll_stop(&mut self, wait: Duration) -> io::Result<Option<String>> {
        let deadline = Instant::now()
            .checked_add(wait)
            .ok_or_else(|| io::Error::other("GDB deadline overflow"))?;
        loop {
            match self.read_byte(deadline) {
                Ok(b'+') => continue,
                Ok(b'-') => return Err(io::Error::other("GDB rejected the resume request")),
                Ok(first) => return self.receive_packet_from(first, deadline).map(Some),
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                    ) =>
                {
                    return Ok(None);
                }
                Err(error) => return Err(error),
            }
        }
    }
    /// Resume the selected target until a breakpoint/exception, allowing a
    /// longer wait for firmware boot or a deliberately injected call.
    pub fn continue_until_stop(&mut self, timeout: Duration) -> io::Result<String> {
        self.request_with_timeout("c", timeout)
    }
    fn ok(&mut self, command: &str) -> io::Result<()> {
        let response = self.request(command)?;
        if response != "OK" {
            return Err(io::Error::other(format!("{command}: {response}")));
        }
        Ok(())
    }
    pub fn breakpoint(&mut self, address: u32, enable: bool) -> io::Result<()> {
        self.ok(&format!(
            "{}0,{address:x},2",
            if enable { "Z" } else { "z" }
        ))
    }
    /// Change a breakpoint under an absolute deadline.
    pub fn breakpoint_before(
        &mut self,
        address: u32,
        enable: bool,
        deadline: Instant,
    ) -> io::Result<()> {
        let command = format!("{}0,{address:x},2", if enable { "Z" } else { "z" });
        let response = self.request_before(&command, deadline)?;
        if response != "OK" {
            return Err(io::Error::other(format!("{command}: {response}")));
        }
        Ok(())
    }
    /// Read one ARM register under an absolute deadline.
    pub fn read_register_before(&mut self, index: u32, deadline: Instant) -> io::Result<u32> {
        let value = self.request_before(&format!("p{index:x}"), deadline)?;
        if value.len() != 8 {
            return Err(io::Error::other(format!(
                "unexpected ARM register: {value}"
            )));
        }
        let mut bytes = [0; 4];
        for (i, byte) in bytes.iter_mut().enumerate() {
            *byte = u8::from_str_radix(&value[2 * i..2 * i + 2], 16).map_err(io::Error::other)?;
        }
        Ok(u32::from_le_bytes(bytes))
    }
    /// Resume before a shared deadline; use poll_stop_before for its reply.
    pub fn resume_before(&mut self, deadline: Instant) -> io::Result<()> {
        self.write_bytes_before(b"$c#63", deadline)
    }
    /// Start a single instruction under an absolute deadline.
    pub fn step_start_before(&mut self, deadline: Instant) -> io::Result<()> {
        self.write_bytes_before(b"$s#73", deadline)
    }
    /// Poll for a stop without extending an absolute deadline.
    pub fn poll_stop_before(&mut self, deadline: Instant) -> io::Result<Option<String>> {
        loop {
            match self.read_byte(deadline) {
                Ok(b'+') => continue,
                Ok(b'-') => return Err(io::Error::other("GDB rejected the resume request")),
                Ok(first) => return self.receive_packet_from(first, deadline).map(Some),
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                    ) =>
                {
                    return Ok(None);
                }
                Err(error) => return Err(error),
            }
        }
    }
    /// Interrupt and read the stop packet before an absolute deadline.
    pub fn interrupt_before(&mut self, deadline: Instant) -> io::Result<String> {
        self.write_bytes_before(&[3], deadline)?;
        let first = self.read_byte(deadline)?;
        self.receive_packet_from(first, deadline)
    }
    pub fn register(&mut self, index: u32, value: u32) -> io::Result<()> {
        let hex = value
            .to_le_bytes()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>();
        self.ok(&format!("P{index:x}={hex}"))
    }
    pub fn read_register(&mut self, index: u32) -> io::Result<u32> {
        let value = self.request(&format!("p{index:x}"))?;
        if value.len() != 8 {
            return Err(io::Error::other(format!(
                "unexpected ARM register: {value}"
            )));
        }
        let mut bytes = [0; 4];
        for (i, b) in bytes.iter_mut().enumerate() {
            *b = u8::from_str_radix(&value[2 * i..2 * i + 2], 16).map_err(io::Error::other)?;
        }
        Ok(u32::from_le_bytes(bytes))
    }
    pub fn write(&mut self, address: u32, data: &[u8]) -> io::Result<()> {
        let hex = data.iter().map(|b| format!("{b:02x}")).collect::<String>();
        self.ok(&format!("M{address:x},{:x}:{hex}", data.len()))
    }

    /// Write one memory packet before an absolute deadline.
    pub fn write_before(&mut self, address: u32, data: &[u8], deadline: Instant) -> io::Result<()> {
        let hex = data
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let response =
            self.request_before(&format!("M{address:x},{:x}:{hex}", data.len()), deadline)?;
        if response != "OK" {
            return Err(io::Error::other(format!("GDB write failed: {response}")));
        }
        Ok(())
    }

    pub fn read(&mut self, address: u32, length: usize) -> io::Result<Vec<u8>> {
        let response = self.request(&format!("m{address:x},{length:x}"))?;
        if response.len() != length * 2 {
            return Err(io::Error::other(format!(
                "GDB read returned {} bytes, expected {length}",
                response.len() / 2
            )));
        }
        (0..length)
            .map(|index| {
                u8::from_str_radix(&response[index * 2..index * 2 + 2], 16)
                    .map_err(io::Error::other)
            })
            .collect()
    }

    /// Call a Thumb routine on the selected QEMU CPU, using a temporary
    /// return trap just below the saved stack pointer. The target must be
    /// stopped by the caller before invoking this method; the call is made
    /// from thread mode (see [`Self::stop_in_thread_mode`]).
    pub fn call_thumb(
        &mut self,
        address: u32,
        arguments: &[u32],
        timeout: Duration,
    ) -> io::Result<String> {
        if address & 1 != 0 || !(1..=5).contains(&arguments.len()) {
            return Err(io::Error::other(
                "Thumb call needs an even address and 1..5 arguments",
            ));
        }
        self.stop_in_thread_mode()?;
        let saved = (0..16)
            .map(|index| self.read_register(index))
            .collect::<io::Result<Vec<_>>>()
            .map_err(|error| io::Error::other(format!("read caller registers: {error}")))?;
        let old_sp = saved[13];
        let in_internal_ram = (0x2000_0100..0x2030_0000).contains(&old_sp);
        let in_external_ram = (0x8020_0000..0x8420_0000).contains(&old_sp);
        if !in_internal_ram && !in_external_ram {
            return Err(io::Error::other(format!(
                "unexpected main RAM stack pointer {old_sp:#010x}"
            )));
        }
        let return_address = old_sp - 2;
        self.breakpoint(return_address, true).map_err(|error| {
            io::Error::other(format!("set firmware return breakpoint: {error}"))
        })?;
        let call_sp = (old_sp - 32) & !7;
        let result = (|| {
            if let Some(argument) = arguments.get(4) {
                self.write(call_sp, &argument.to_le_bytes())?;
            }
            self.register(13, call_sp)?;
            for index in 0..4 {
                self.register(index as u32, arguments.get(index).copied().unwrap_or(0))?;
            }
            self.register(14, return_address | 1)?;
            self.register(15, address | 1)?;
            match self.continue_until_stop(timeout) {
                Ok(stop) => Ok(stop),
                Err(error) => {
                    // Stop the guest and report where the routine was, so a
                    // timeout shows whether it faulted (IPSR is the exception
                    // number) or was waiting in firmware code.
                    let location = self
                        .interrupt()
                        .and_then(|_| {
                            Ok(format!(
                                "; stopped at pc={:#010x} lr={:#010x} xpsr={:#010x}",
                                self.read_register(15)?,
                                self.read_register(14)?,
                                self.read_register(XPSR)?,
                            ))
                        })
                        .unwrap_or_default();
                    Err(io::Error::other(format!(
                        "wait for firmware return breakpoint: {error}{location}"
                    )))
                }
            }
        })();

        // The caller stops QEMU after the trap and before restoring this
        // context, so the scratch instruction cannot race the guest.
        let stopped_pc = if result.is_ok() {
            self.read_register(15).ok()
        } else {
            None
        };
        let restore_result = (|| {
            self.breakpoint(return_address, false)?;
            for index in 0..13 {
                self.register(index, saved[index as usize])?;
            }
            self.register(13, saved[13])?;
            self.register(14, saved[14])?;
            self.register(15, saved[15] | 1)?;
            Ok::<(), io::Error>(())
        })();
        let stop = result?;
        restore_result?;
        if let Some(pc) = stopped_pc {
            if pc < return_address || pc > return_address + 4 {
                return Err(io::Error::other(format!(
                    "firmware routine stopped at {pc:#010x}, outside its return trap"
                )));
            }
        }
        Ok(stop)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gdb_connections_share_exclusive_process_safe_lease() {
        use std::{os::unix::net::UnixListener, thread};
        let dir = std::env::temp_dir().join(format!("l6-gdb-lease-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir(&dir).unwrap();
        let path = dir.join("gdb.sock");
        let listener = UnixListener::bind(&path).unwrap();
        let server = thread::spawn(move || {
            for _ in 0..2 {
                let (mut stream, _) = listener.accept().unwrap();
                let mut command = [0; 7];
                stream.read_exact(&mut command).unwrap();
                assert_eq!(&command, b"$Hg1#e0");
                stream.write_all(b"+$OK#9a").unwrap();
                let mut ack = [0];
                stream.read_exact(&mut ack).unwrap();
                assert_eq!(&ack, b"+");
            }
        });
        let first = Gdb::connect(&path).unwrap();
        assert!(
            Gdb::connect(&path)
                .err()
                .unwrap()
                .to_string()
                .contains("already leased")
        );
        drop(first);
        // A concurrently spawning process can briefly inherit the close-on-exec
        // lease descriptor between fork and exec. Wait for that handoff.
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            match Gdb::connect(&path) {
                Ok(second) => {
                    drop(second);
                    break;
                }
                Err(error)
                    if error.kind() == io::ErrorKind::WouldBlock && Instant::now() < deadline =>
                {
                    std::thread::sleep(Duration::from_millis(5))
                }
                Err(error) => panic!("lease was not released: {error}"),
            }
        }
        server.join().unwrap();
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn lease_rejects_fifo_without_blocking() {
        use std::{
            ffi::CString,
            os::unix::{ffi::OsStrExt, net::UnixListener},
        };
        let dir = std::env::temp_dir().join(format!("l6-gdb-fifo-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir(&dir).unwrap();
        let path = dir.join("gdb.sock");
        let listener = UnixListener::bind(&path).unwrap();
        let fifo = dir.join("gdb.sock.lease");
        let fifo_name = CString::new(fifo.as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(fifo_name.as_ptr(), 0o600) }, 0);
        let start = Instant::now();
        assert!(
            Lease::acquire(&path)
                .err()
                .unwrap()
                .to_string()
                .contains("not a regular file")
        );
        assert!(start.elapsed() < Duration::from_secs(1));
        drop(listener);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn lease_child_probe() {
        let Some(path) = std::env::var_os("L6_TEST_GDB_LEASE_PATH") else {
            return;
        };
        let error = Lease::acquire(Path::new(&path))
            .err()
            .expect("child acquired a held lease");
        assert!(error.to_string().contains("already leased"));
    }

    #[test]
    fn lease_excludes_a_separate_process() {
        use std::{os::unix::net::UnixListener, process::Command};
        let dir = std::env::temp_dir().join(format!("l6-gdb-process-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir(&dir).unwrap();
        let path = dir.join("gdb.sock");
        let listener = UnixListener::bind(&path).unwrap();
        let lease = Lease::acquire(&path).unwrap();
        let result = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "gdb::tests::lease_child_probe", "--nocapture"])
            .env("L6_TEST_GDB_LEASE_PATH", &path)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert!(String::from_utf8_lossy(&result.stdout).contains("1 passed"));
        drop(lease);
        assert!(Lease::acquire(&path).is_ok());
        drop(listener);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn bounded_connect_times_out_on_a_stalled_gdb_handshake() {
        use std::{os::unix::net::UnixListener, thread};
        let dir = std::env::temp_dir().join(format!(
            "l6-gdb-deadline-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("stub.sock");
        let listener = UnixListener::bind(&path).unwrap();
        let server = thread::spawn(move || {
            let (_stream, _) = listener.accept().unwrap();
            thread::sleep(Duration::from_millis(100));
        });
        let lease = Lease::acquire(&path).unwrap();
        let start = Instant::now();
        assert!(Gdb::connect_leased_before(lease, start + Duration::from_millis(20)).is_err());
        assert!(start.elapsed() < Duration::from_secs(1));
        server.join().unwrap();
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn packet_deadline_cannot_be_extended_by_slow_bytes() {
        use std::thread;
        let (client, mut stub) = UnixStream::pair().unwrap();
        let mut gdb = Gdb {
            stream: client,
            _lease: None,
            request_timeout: Duration::from_secs(5),
        };
        let server = thread::spawn(move || {
            let mut request = [0; 5];
            let _ = stub.read_exact(&mut request);
            for byte in b"+$S05#b8" {
                if stub.write_all(&[*byte]).is_err() {
                    break;
                }
                thread::sleep(Duration::from_millis(15));
            }
        });
        let start = Instant::now();
        assert!(
            gdb.request_with_timeout("c", Duration::from_millis(40))
                .is_err()
        );
        assert!(start.elapsed() < Duration::from_millis(300));
        drop(gdb);
        server.join().unwrap();
    }

    #[test]
    fn malformed_escape_is_an_error_instead_of_a_panic() {
        let (client, mut stub) = UnixStream::pair().unwrap();
        let mut gdb = Gdb {
            stream: client,
            _lease: None,
            request_timeout: Duration::from_secs(5),
        };
        stub.write_all(b"$}#7d").unwrap();
        assert!(
            gdb.receive_packet()
                .unwrap_err()
                .to_string()
                .contains("truncated")
        );
    }

    #[test]
    fn poll_stop_waits_past_the_resume_acknowledgement() {
        let (client, mut stub) = UnixStream::pair().unwrap();
        let mut gdb = Gdb {
            stream: client,
            _lease: None,
            request_timeout: Duration::from_secs(5),
        };
        gdb.resume().unwrap();
        let mut request = [0; 5];
        stub.read_exact(&mut request).unwrap();
        assert_eq!(&request, b"$c#63");
        // The acknowledgement alone is not a stop: the target is still running.
        stub.write_all(b"+").unwrap();
        let started = std::time::Instant::now();
        assert_eq!(gdb.poll_stop(Duration::from_millis(20)).unwrap(), None);
        assert!(started.elapsed() < Duration::from_secs(1));
        assert_eq!(gdb.poll_stop(Duration::from_millis(20)).unwrap(), None);
        stub.write_all(b"$S05#b8").unwrap();
        assert_eq!(
            gdb.poll_stop(Duration::from_millis(200))
                .unwrap()
                .as_deref(),
            Some("S05")
        );
        let mut ack = [0];
        stub.read_exact(&mut ack).unwrap();
        assert_eq!(&ack, b"+");
    }
}
