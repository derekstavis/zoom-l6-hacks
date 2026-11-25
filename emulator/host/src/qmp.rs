//! Typed QMP management over an inherited socket, using the upstream QAPI crate.
//! Device input and display events use the separate runtime protocol.
use qapi::{Command, Qmp, Stream, qmp};
use std::{
    io::{self, BufRead, BufReader, Read},
    net::Shutdown,
    os::unix::net::UnixStream,
    sync::{Arc, Mutex, TryLockError},
    time::{Duration, Instant},
};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Status {
    pub running: bool,
    pub status: String,
}

/// Apply a deadline to the entire request, including intervening QMP events.
/// A socket read timeout alone would restart the timeout after every event.
struct DeadlineReader {
    socket: UnixStream,
    deadline: Instant,
}

impl Read for DeadlineReader {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        let remaining = self
            .deadline
            .checked_duration_since(Instant::now())
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::TimedOut, "QMP request deadline exceeded")
            })?;
        self.socket.set_read_timeout(Some(remaining))?;
        self.socket.read(bytes)
    }
}

type Management = Qmp<Stream<BufReader<DeadlineReader>, UnixStream>>;

#[derive(Clone)]
pub struct QmpClient {
    management: Arc<Mutex<Management>>,
    preserved_events: Arc<Mutex<Vec<qmp::Event>>>,
}

impl QmpClient {
    /// The other end of `stream` is inherited by QEMU's socket chardev. Requests
    /// are serialized because the synchronous QAPI client consumes replies in
    /// order; QAPI queues asynchronous events encountered before each reply.
    pub fn connect(stream: UnixStream) -> io::Result<Self> {
        stream.set_write_timeout(Some(REQUEST_TIMEOUT))?;
        let reader = DeadlineReader {
            socket: stream.try_clone()?,
            deadline: Instant::now() + REQUEST_TIMEOUT,
        };
        let mut management = Qmp::new(Stream::new(BufReader::new(reader), stream));
        // qapi::read_capabilities currently panics on greeting EOF. Avoid that
        // by checking for an initial byte without consuming the greeting.
        if management.inner_mut().get_mut_read().fill_buf()?.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "QMP disconnected before greeting",
            ));
        }
        management.handshake().map_err(io::Error::from)?;
        Ok(Self {
            management: Arc::new(Mutex::new(management)),
            preserved_events: Arc::new(Mutex::new(Vec::new())),
        })
    }

    /// Whether two handles refer to the same QEMU management connection.
    pub fn same_connection(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.management, &other.management)
    }

    fn execute<C: Command>(&self, command: &C) -> io::Result<C::Ok> {
        let mut management = self.management.lock().unwrap();
        Self::execute_locked(&mut management, command, Instant::now() + REQUEST_TIMEOUT)
    }

    /// Execute while holding the management connection, with one deadline for
    /// the request. A shorter enclosing deadline also bounds socket writes.
    fn execute_locked<C: Command>(
        management: &mut Management,
        command: &C,
        deadline: Instant,
    ) -> io::Result<C::Ok> {
        let started = Instant::now();
        let remaining = deadline
            .checked_duration_since(started)
            .filter(|remaining| !remaining.is_zero())
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::TimedOut, "QMP request deadline exceeded")
            })?;
        let request_deadline = deadline.min(started + REQUEST_TIMEOUT);
        management.inner_mut().get_mut_read().get_mut().deadline = request_deadline;
        management
            .inner()
            .get_ref_write()
            .set_write_timeout(Some(remaining.min(REQUEST_TIMEOUT)))?;
        let result = management.execute(command);
        let elapsed_ms = started.elapsed().as_secs_f64() * 1000.0;
        if matches!(result, Err(qapi::ExecuteError::Io(_))) {
            // A timed out response might arrive later. Closing prevents a later
            // request from accidentally consuming that response as its own.
            let _ = management.inner().get_ref_write().shutdown(Shutdown::Both);
        }
        match result {
            Ok(value) => {
                tracing::debug!(target: "l6max_host::qmp", event = "qmp_command", command = C::NAME, elapsed_ms, outcome = "ok", "QMP command completed");
                Ok(value)
            }
            Err(error) => {
                tracing::warn!(target: "l6max_host::qmp", event = "qmp_command", command = C::NAME, elapsed_ms, outcome = "error", error = %error, "QMP command failed");
                if Instant::now() >= request_deadline {
                    Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "QMP request deadline exceeded",
                    ))
                } else {
                    Err(io::Error::from(error))
                }
            }
        }
    }

    /// Execute a command within a caller-supplied deadline, including time
    /// waiting for the shared QMP connection. I/O timeout still closes QMP to
    /// prevent a late response being consumed by another request.
    fn execute_before<C: Command>(&self, command: &C, deadline: Instant) -> io::Result<C::Ok> {
        let mut management = loop {
            match self.management.try_lock() {
                Ok(guard) => break guard,
                Err(std::sync::TryLockError::Poisoned(_)) => {
                    return Err(io::Error::other("QMP lock poisoned"));
                }
                Err(std::sync::TryLockError::WouldBlock) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(1))
                }
                Err(std::sync::TryLockError::WouldBlock) => {
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "QMP deadline elapsed waiting for connection",
                    ));
                }
            }
        };
        let now = Instant::now();
        if now >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "QMP deadline elapsed",
            ));
        }
        management.inner_mut().get_mut_read().get_mut().deadline =
            deadline.min(now + REQUEST_TIMEOUT);
        let write_timeout = deadline.saturating_duration_since(now).min(REQUEST_TIMEOUT);
        management
            .inner()
            .get_ref_write()
            .set_write_timeout(Some(write_timeout))?;
        let result = management.execute(command);
        let restore_timeout = management
            .inner()
            .get_ref_write()
            .set_write_timeout(Some(REQUEST_TIMEOUT));
        if matches!(result, Err(qapi::ExecuteError::Io(_))) {
            let _ = management.inner().get_ref_write().shutdown(Shutdown::Both);
        }
        let result = result.map_err(io::Error::from)?;
        restore_timeout?;
        Ok(result)
    }

    pub fn query_status(&self) -> io::Result<Status> {
        let result = self.execute(&qmp::query_status {})?;
        let status = serde_json::to_value(result.status)?;
        Ok(Status {
            running: result.running,
            status: status
                .as_str()
                .ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidData, "QMP status is not a name")
                })?
                .to_owned(),
        })
    }

    /// Query the named run state within a caller-owned deadline.
    pub fn status_before(&self, deadline: Instant) -> io::Result<Status> {
        let result = self.execute_before(&qmp::query_status {}, deadline)?;
        let status = serde_json::to_value(result.status)?;
        Ok(Status {
            running: result.running,
            status: status
                .as_str()
                .ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidData, "QMP status is not a name")
                })?
                .to_owned(),
        })
    }

    /// Query run state within a caller-owned deadline.
    pub fn running_before(&self, deadline: Instant) -> io::Result<bool> {
        self.execute_before(&qmp::query_status {}, deadline)
            .map(|status| status.running)
    }

    pub fn stop(&self) -> io::Result<()> {
        self.execute(&qmp::stop {}).map(|_| ())
    }

    /// Stop and verify MAIN within one caller-owned deadline.
    pub fn stop_before(&self, deadline: Instant) -> io::Result<()> {
        self.execute_before(&qmp::stop {}, deadline)?;
        let status = self.execute_before(&qmp::query_status {}, deadline)?;
        if status.running {
            return Err(io::Error::other("MAIN still running after stop"));
        }
        Ok(())
    }

    pub fn resume(&self) -> io::Result<()> {
        self.execute(&qmp::cont {}).map(|_| ())
    }

    /// Resume with a caller-owned deadline, including QMP connection contention.
    pub fn resume_before(&self, deadline: Instant) -> io::Result<()> {
        self.execute_before(&qmp::cont {}, deadline).map(|_| ())
    }

    /// Reset the guest in place. RAM devices retain contents unless their
    /// model explicitly clears them during reset.
    pub fn system_reset(&self) -> io::Result<()> {
        self.execute(&qmp::system_reset {}).map(|_| ())
    }

    /// Request a reset and wait until QEMU reports completing that reset.
    /// This is a reset barrier, not a firmware or panel readiness signal.
    pub fn system_reset_and_wait(&self, timeout: Duration) -> io::Result<()> {
        let deadline = Instant::now() + timeout;
        // Keep event ownership from the pre-reset pump through the matching
        // event. A concurrent clone may issue commands or drain events, so it
        // must not be able to consume the event between a query and its drain.
        let mut management = loop {
            match self.management.try_lock() {
                Ok(management) => break management,
                Err(TryLockError::Poisoned(_)) => panic!("QMP management lock poisoned"),
                Err(TryLockError::WouldBlock) => {
                    let remaining = deadline
                        .checked_duration_since(Instant::now())
                        .filter(|remaining| !remaining.is_zero())
                        .ok_or_else(|| {
                            io::Error::new(io::ErrorKind::TimedOut, "QMP reset event timed out")
                        })?;
                    std::thread::sleep(remaining.min(Duration::from_millis(10)));
                }
            }
        };
        Self::execute_locked(&mut management, &qmp::query_status {}, deadline)?;
        self.preserved_events
            .lock()
            .unwrap()
            .extend(management.events());
        Self::execute_locked(&mut management, &qmp::system_reset {}, deadline)?;
        loop {
            Self::execute_locked(&mut management, &qmp::query_status {}, deadline)?;
            let mut found = false;
            let mut preserved = self.preserved_events.lock().unwrap();
            for event in management.events() {
                if !found
                    && matches!(
                        event,
                        qmp::Event::RESET {
                            data: qmp::RESET {
                                reason: qmp::ShutdownCause::host_qmp_system_reset,
                                ..
                            },
                            ..
                        }
                    )
                {
                    found = true;
                } else {
                    preserved.push(event);
                }
            }
            drop(preserved);
            if found {
                return Ok(());
            }
            let remaining = deadline
                .checked_duration_since(Instant::now())
                .filter(|remaining| !remaining.is_zero())
                .ok_or_else(|| {
                    io::Error::new(io::ErrorKind::TimedOut, "QMP reset event timed out")
                })?;
            std::thread::sleep(remaining.min(Duration::from_millis(10)));
        }
    }

    pub fn quit(&self) -> io::Result<()> {
        self.execute(&qmp::quit {}).map(|_| ())
    }
    /// Remove the attached SD medium through QEMU's block layer and SDBus.
    #[allow(deprecated)]
    pub fn eject_sd(&self) -> io::Result<()> {
        self.execute(&qmp::eject {
            device: Some("l6-sd".into()),
            id: None,
            force: Some(true),
        })
        .map(|_| ())
    }
    #[allow(deprecated)]
    pub fn insert_sd(&self, image: &std::path::Path) -> io::Result<()> {
        let filename = image
            .canonicalize()?
            .to_str()
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidInput, "SD image path is not UTF-8")
            })?
            .to_owned();
        self.execute(&qmp::blockdev_change_medium {
            device: Some("l6-sd".into()),
            id: None,
            filename,
            format: Some("raw".into()),
            force: Some(true),
            read_only_mode: None,
        })
        .map(|_| ())
    }

    /// Diagnostic register snapshot from one CPU, using QAPI's HMP bridge.
    pub fn registers(&self, cpu: i64) -> io::Result<String> {
        self.execute(&qmp::human_monitor_command {
            command_line: "info registers".into(),
            cpu_index: Some(cpu),
        })
    }

    /// Snapshot TCG translation-cache and invalidation counters.
    pub fn jit_stats(&self) -> io::Result<String> {
        self.execute(&qmp::human_monitor_command {
            command_line: "info jit".into(),
            cpu_index: None,
        })
    }

    /// Read a bounded physical-memory region for firmware diagnostics.
    pub fn read_memory(&self, cpu: i64, address: u32, size: usize) -> io::Result<Vec<u8>> {
        if size == 0 || size > 4096 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "memory diagnostic size must be 1..4096",
            ));
        }
        let reply = self.execute(&qmp::human_monitor_command {
            command_line: format!("xp /{size}bx 0x{address:x}"),
            cpu_index: Some(cpu),
        })?;
        Self::parse_memory_reply(reply, size)
    }

    /// Bounded read whose entire QMP transaction uses the shared deadline.
    pub fn read_memory_before(
        &self,
        cpu: i64,
        address: u32,
        size: usize,
        deadline: Instant,
    ) -> io::Result<Vec<u8>> {
        if size == 0 || size > 4096 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "memory diagnostic size must be 1..4096",
            ));
        }
        let reply = self.execute_before(
            &qmp::human_monitor_command {
                command_line: format!("xp /{size}bx 0x{address:x}"),
                cpu_index: Some(cpu),
            },
            deadline,
        )?;
        Self::parse_memory_reply(reply, size)
    }

    fn parse_memory_reply(reply: String, size: usize) -> io::Result<Vec<u8>> {
        let bytes = reply
            .lines()
            .filter_map(|line| line.split_once(':').map(|(_, data)| data))
            .flat_map(str::split_whitespace)
            .map(|word| u8::from_str_radix(word.trim_start_matches("0x"), 16))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        if bytes.len() != size {
            return Err(io::Error::new(io::ErrorKind::InvalidData, reply));
        }
        Ok(bytes)
    }

    /// Querying status also pumps pending asynchronous events through QAPI.
    /// Call at the supervisor's health tick; display events use runtime IPC.
    pub fn poll_events(&self) -> io::Result<Vec<qmp::Event>> {
        self.query_status()?;
        Ok(self.drain_events())
    }

    pub fn drain_events(&self) -> Vec<qmp::Event> {
        let mut events = std::mem::take(&mut *self.preserved_events.lock().unwrap());
        events.extend(self.drain_management_events());
        for event in &events {
            let name = serde_json::to_value(event)
                .ok()
                .and_then(|value| {
                    value
                        .get("event")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_owned)
                })
                .unwrap_or_else(|| "unknown".into());
            tracing::debug!(target: "l6max_host::qmp", event = "qmp_event", qmp_event = %name, "QMP event received");
        }
        events
    }

    fn drain_management_events(&self) -> Vec<qmp::Event> {
        self.management.lock().unwrap().events().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{io::Write, thread};

    fn greeting(stream: &mut UnixStream) {
        stream.write_all(b"{\"QMP\":{\"version\":{\"qemu\":{\"major\":10,\"minor\":0,\"micro\":0},\"package\":\"\"},\"capabilities\":[]}}\r\n").unwrap();
    }

    fn expect_command(reader: &mut BufReader<UnixStream>, name: &str) {
        let mut command = String::new();
        reader.read_line(&mut command).unwrap();
        let command: serde_json::Value = serde_json::from_str(&command).unwrap();
        assert_eq!(command["execute"], name);
    }

    #[test]
    fn caller_deadline_includes_waiting_for_shared_connection() {
        let (client, mut peer) = UnixStream::pair().unwrap();
        let server = thread::spawn(move || {
            let mut reader = BufReader::new(peer.try_clone().unwrap());
            greeting(&mut peer);
            expect_command(&mut reader, "qmp_capabilities");
            peer.write_all(b"{\"return\":{}}\r\n").unwrap();
            let mut line = String::new();
            assert_eq!(reader.read_line(&mut line).unwrap(), 0);
        });
        let client = QmpClient::connect(client).unwrap();
        let lock = client.management.lock().unwrap();
        let started = Instant::now();
        let error = client
            .execute_before(&qmp::stop {}, started + Duration::from_millis(20))
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert!(started.elapsed() < Duration::from_secs(1));
        drop(lock);
        drop(client);
        server.join().unwrap();
    }

    #[test]
    fn notification_and_scheduler_observations_respect_a_busy_qmp_deadline() {
        let (socket, mut peer) = UnixStream::pair().unwrap();
        let server = thread::spawn(move || {
            let mut reader = BufReader::new(peer.try_clone().unwrap());
            greeting(&mut peer);
            expect_command(&mut reader, "qmp_capabilities");
            peer.write_all(b"{\"return\":{}}\r\n").unwrap();
            let mut line = String::new();
            assert_eq!(reader.read_line(&mut line).unwrap(), 0);
        });
        let client = QmpClient::connect(socket).unwrap();
        let lock = client.management.lock().unwrap();
        let deadline = Instant::now() + Duration::from_millis(20);
        let error = crate::freertos::ticks_before_checked(
            &client,
            l6fw::Model::L6max,
            deadline,
            &|| Ok(()),
        )
        .unwrap_err();
        assert!(error.contains("deadline"), "{error}");
        let deadline = Instant::now() + Duration::from_millis(20);
        let error = crate::knob_popup::notification_state_before_checked(
            &client,
            l6fw::Model::L6max,
            deadline,
            &|| Ok(()),
        )
        .unwrap_err();
        assert!(error.contains("deadline"), "{error}");
        let deadline = Instant::now() + Duration::from_millis(20);
        let error = match crate::knob_popup::overlay_reader_before_checked(
            &client,
            l6fw::Model::L6max,
            deadline,
            &|| Ok(()),
        ) {
            Ok(_) => panic!("busy QMP unexpectedly returned overlay reader"),
            Err(error) => error,
        };
        assert!(error.contains("deadline"), "{error}");
        drop(lock);
        drop(client);
        server.join().unwrap();
    }

    #[test]
    fn typed_management_preserves_events_interleaved_with_replies() {
        let (client, mut peer) = UnixStream::pair().unwrap();
        let server = thread::spawn(move || {
            let mut reader = BufReader::new(peer.try_clone().unwrap());
            greeting(&mut peer);
            expect_command(&mut reader, "qmp_capabilities");
            peer.write_all(b"{\"return\":{}}\r\n").unwrap();
            expect_command(&mut reader, "stop");
            peer.write_all(b"{\"event\":\"STOP\",\"timestamp\":{\"seconds\":1,\"microseconds\":2}}\r\n{\"return\":{}}\r\n").unwrap();
            expect_command(&mut reader, "query-status");
            peer.write_all(
                b"{\"return\":{\"running\":false,\"singlestep\":false,\"status\":\"paused\"}}\r\n",
            )
            .unwrap();
            expect_command(&mut reader, "cont");
            peer.write_all(b"{\"event\":\"RESUME\",\"timestamp\":{\"seconds\":1,\"microseconds\":3}}\r\n{\"return\":{}}\r\n").unwrap();
            expect_command(&mut reader, "quit");
            peer.write_all(b"{\"return\":{}}\r\n").unwrap();
        });
        let client = QmpClient::connect(client).unwrap();
        client.stop().unwrap();
        assert_eq!(
            client.query_status().unwrap(),
            Status {
                running: false,
                status: "paused".into()
            }
        );
        let events = client.drain_events();
        assert_eq!(events.len(), 1);
        assert_eq!(serde_json::to_value(&events[0]).unwrap()["event"], "STOP");
        client.resume().unwrap();
        assert_eq!(client.drain_events().len(), 1);
        client.quit().unwrap();
        server.join().unwrap();
    }

    #[test]
    fn reset_wait_ignores_old_events_and_preserves_unrelated_events() {
        let (client, mut peer) = UnixStream::pair().unwrap();
        let server = thread::spawn(move || {
            let mut reader = BufReader::new(peer.try_clone().unwrap());
            greeting(&mut peer);
            expect_command(&mut reader, "qmp_capabilities");
            peer.write_all(b"{\"return\":{}}\r\n").unwrap();
            expect_command(&mut reader, "query-status");
            peer.write_all(b"{\"event\":\"RESET\",\"data\":{\"guest\":false,\"reason\":\"host-qmp-system-reset\"},\"timestamp\":{\"seconds\":1,\"microseconds\":0}}\r\n{\"return\":{\"running\":true,\"singlestep\":false,\"status\":\"running\"}}\r\n").unwrap();
            expect_command(&mut reader, "system_reset");
            peer.write_all(b"{\"event\":\"STOP\",\"timestamp\":{\"seconds\":2,\"microseconds\":0}}\r\n{\"return\":{}}\r\n").unwrap();
            expect_command(&mut reader, "query-status");
            peer.write_all(
                b"{\"return\":{\"running\":true,\"singlestep\":false,\"status\":\"running\"}}\r\n",
            )
            .unwrap();
            expect_command(&mut reader, "query-status");
            peer.write_all(b"{\"event\":\"RESET\",\"data\":{\"guest\":false,\"reason\":\"host-qmp-system-reset\"},\"timestamp\":{\"seconds\":3,\"microseconds\":0}}\r\n{\"return\":{\"running\":true,\"singlestep\":false,\"status\":\"running\"}}\r\n").unwrap();
        });
        let client = QmpClient::connect(client).unwrap();
        client
            .system_reset_and_wait(Duration::from_secs(1))
            .unwrap();
        let events = client.drain_events();
        assert_eq!(events.len(), 2);
        assert!(matches!(events[0], qmp::Event::RESET { .. }));
        assert!(matches!(events[1], qmp::Event::STOP { .. }));
        server.join().unwrap();
    }

    #[test]
    fn reset_wait_exclusively_owns_events_against_concurrent_drain() {
        use std::sync::mpsc;
        let (client, mut peer) = UnixStream::pair().unwrap();
        let (query_tx, query_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let server = thread::spawn(move || {
            let mut reader = BufReader::new(peer.try_clone().unwrap());
            greeting(&mut peer);
            expect_command(&mut reader, "qmp_capabilities");
            peer.write_all(b"{\"return\":{}}\r\n").unwrap();
            expect_command(&mut reader, "query-status");
            peer.write_all(
                b"{\"return\":{\"running\":true,\"singlestep\":false,\"status\":\"running\"}}\r\n",
            )
            .unwrap();
            expect_command(&mut reader, "system_reset");
            peer.write_all(b"{\"return\":{}}\r\n").unwrap();
            expect_command(&mut reader, "query-status");
            query_tx.send(()).unwrap();
            release_rx.recv().unwrap();
            peer.write_all(b"{\"event\":\"RESET\",\"data\":{\"guest\":false,\"reason\":\"host-qmp-system-reset\"},\"timestamp\":{\"seconds\":3,\"microseconds\":0}}\r\n{\"return\":{\"running\":true,\"singlestep\":false,\"status\":\"running\"}}\r\n").unwrap();
        });
        let client = QmpClient::connect(client).unwrap();
        let reset_client = client.clone();
        let reset =
            thread::spawn(move || reset_client.system_reset_and_wait(Duration::from_secs(2)));
        query_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        let (drain_tx, drain_rx) = mpsc::channel();
        let drain = thread::spawn(move || drain_tx.send(client.drain_events()).unwrap());
        assert!(drain_rx.recv_timeout(Duration::from_millis(50)).is_err());
        release_tx.send(()).unwrap();
        reset.join().unwrap().unwrap();
        assert!(
            drain_rx
                .recv_timeout(Duration::from_secs(1))
                .unwrap()
                .is_empty()
        );
        drain.join().unwrap();
        server.join().unwrap();
    }

    #[test]
    fn reset_deadline_bounds_a_stalled_status_reply() {
        let (client, mut peer) = UnixStream::pair().unwrap();
        let server = thread::spawn(move || {
            let mut reader = BufReader::new(peer.try_clone().unwrap());
            greeting(&mut peer);
            expect_command(&mut reader, "qmp_capabilities");
            peer.write_all(b"{\"return\":{}}\r\n").unwrap();
            expect_command(&mut reader, "query-status");
            thread::sleep(Duration::from_millis(1_200));
        });
        let client = QmpClient::connect(client).unwrap();
        let started = Instant::now();
        assert_eq!(
            client
                .system_reset_and_wait(Duration::from_millis(60))
                .unwrap_err()
                .kind(),
            io::ErrorKind::TimedOut
        );
        assert!(started.elapsed() < Duration::from_millis(800));
        server.join().unwrap();
    }

    #[test]
    fn reset_deadline_includes_waiting_for_a_busy_connection() {
        let (client, mut peer) = UnixStream::pair().unwrap();
        let server = thread::spawn(move || {
            let mut reader = BufReader::new(peer.try_clone().unwrap());
            greeting(&mut peer);
            expect_command(&mut reader, "qmp_capabilities");
            peer.write_all(b"{\"return\":{}}\r\n").unwrap();
        });
        let client = QmpClient::connect(client).unwrap();
        let held = client.management.lock().unwrap();
        let clone = client.clone();
        let started = Instant::now();
        let reset = thread::spawn(move || clone.system_reset_and_wait(Duration::from_millis(60)));
        assert_eq!(
            reset.join().unwrap().unwrap_err().kind(),
            io::ErrorKind::TimedOut
        );
        assert!(started.elapsed() < Duration::from_millis(800));
        drop(held);
        server.join().unwrap();
    }

    #[test]
    fn dropping_a_clone_keeps_the_connection_usable() {
        let (client, mut peer) = UnixStream::pair().unwrap();
        let server = thread::spawn(move || {
            let mut reader = BufReader::new(peer.try_clone().unwrap());
            greeting(&mut peer);
            expect_command(&mut reader, "qmp_capabilities");
            peer.write_all(b"{\"return\":{}}\r\n").unwrap();
            expect_command(&mut reader, "query-status");
            peer.write_all(
                b"{\"return\":{\"running\":true,\"singlestep\":false,\"status\":\"running\"}}\r\n",
            )
            .unwrap();
        });
        let client = QmpClient::connect(client).unwrap();
        let clone = client.clone();
        drop(clone);
        assert!(client.query_status().unwrap().running);
        server.join().unwrap();
    }

    #[test]
    fn greeting_eof_is_an_error_instead_of_qapi_panic() {
        let (client, peer) = UnixStream::pair().unwrap();
        drop(peer);
        // macOS may report ConnectionReset before a read reaches EOF.
        assert!(QmpClient::connect(client).is_err());
    }
}
