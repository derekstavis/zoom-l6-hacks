//! Typed control of an emulator owned in process or exposed by its control socket.
//!
//! The socket interface is deliberately narrower than direct access. In
//! particular its input replies confirm enqueueing, not guest acknowledgement.
pub use l6max_host::control::Info;
pub use l6max_host::debug::{Command as DebugCommand, Observation as DebugObservation};
pub use l6max_host::effect_editor::{
    Binding as EffectEditorBinding, Snapshot as EffectEditorSnapshot,
};
pub use l6max_host::heap::Counters as HeapCounters;
pub use l6max_host::input::button_source;
pub use l6max_host::input::{InputAck, InputAckState, InputCounters, InputSource};
pub use l6max_host::knob_popup::{ParameterState, RecorderState, Snapshot as KnobPopupSnapshot};
pub use l6max_host::memory::MemoryRead;
pub use l6max_host::midi_config::Config as MidiConfig;
pub use l6max_host::sd_loader::{Site as SdLoaderSite, Snapshot as SdLoaderSnapshot};
pub use l6max_host::update_bootloader::Outcome as InstallerOutcome;
pub use l6max_host::update_state::{UpdateGuiState, UpdateState};
use l6max_host::{control, engine::Engine, freertos, qmp};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::Value;
use std::{
    fmt,
    io::{self, BufRead, BufReader, Read, Write},
    os::unix::net::UnixStream,
    path::{Path, PathBuf},
    time::Duration,
};

/// Available RTOS categories on the host's existing control endpoint.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum RtosKind {
    Heap,
    Tasks,
    Scheduler,
    Objects,
    All,
}

/// Typed requests understood by the host's existing control socket.
#[derive(Clone, Debug, Serialize)]
#[serde(tag = "command", rename_all = "kebab-case")]
pub enum Request {
    Info,
    InputStatus { source: InputSource },
    InputAck { source: InputSource, sequence: u32 },
    SdLoaderStatus,
    SdLoaderSite { index: u32 },
    HeapCounters,
    MemoryRead { address: u32, length: usize },
    MidiConfig,
    EffectEditor { binding: EffectEditorBinding },
    UpdateState,
    UpdateGuiState,
    InstallerOutcome,
    UsbAttach,
    UsbRelease,
    UsbController,
    KnobPopup { with_text: bool },
    RecorderState,
    KnobParameterState,
    Rtos { kind: RtosKind },
    Tap { target: u32, hold_ms: u32 },
    Down { target: u32, duration_ms: u32 },
    Up { target: u32 },
    Encoder { channel: u32, steps: i32 },
    Analog { channel: u32, steps: i32 },
    SetAnalog { channel: u32, value: u16 },
}

/// The attach request only signals cable attachment, not enumeration.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct UsbAttachReceipt {
    pub attached: bool,
}
/// `running` means the attached guest controller answers tokens; it does not prove enumeration.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct UsbControllerStatus {
    pub running: bool,
}

/// The socket reports the accepted command, not a guest acknowledgement.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct ButtonReceipt {
    pub target: u32,
    pub command: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct AxisReceipt {
    pub channel: u32,
    pub steps: i32,
    pub sequence: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct AnalogPositionReceipt {
    pub channel: u32,
    pub value: u16,
}

#[derive(Debug)]
pub enum ControlError {
    Connect { path: PathBuf, source: io::Error },
    Io(io::Error),
    Protocol(serde_json::Error),
    Rejected(String),
    Unavailable(&'static str),
}

impl fmt::Display for ControlError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Connect { path, source } => write!(
                f,
                "cannot connect to {}: {source}; launch the emulator with `--control-socket {}`",
                path.display(),
                path.display()
            ),
            Self::Io(error) => error.fmt(f),
            Self::Protocol(error) => error.fmt(f),
            Self::Rejected(error) => f.write_str(error),
            Self::Unavailable(error) => f.write_str(error),
        }
    }
}

impl std::error::Error for ControlError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Connect { source, .. } | Self::Io(source) => Some(source),
            Self::Protocol(source) => Some(source),
            _ => None,
        }
    }
}

/// One persistent local control connection owns one debugger lease. Dropping
/// the client closes the connection and causes the host to clean up.
pub struct SocketDebugSession {
    writer: UnixStream,
    reader: BufReader<UnixStream>,
    closed: bool,
}
impl SocketDebugSession {
    fn read_reply(&mut self) -> Result<Value, ControlError> {
        let mut line = String::new();
        let bytes = (&mut self.reader)
            .take(65537)
            .read_line(&mut line)
            .map_err(ControlError::Io)?;
        if bytes == 0 || bytes > 65536 || !line.ends_with('\n') {
            return Err(ControlError::Rejected(
                "debug session reply ended or exceeded its limit".into(),
            ));
        }
        let reply: Value = serde_json::from_str(&line).map_err(ControlError::Protocol)?;
        if reply.get("ok").and_then(Value::as_bool) != Some(true) {
            return Err(ControlError::Rejected(
                reply
                    .get("error")
                    .and_then(Value::as_str)
                    .unwrap_or("debug request failed")
                    .to_owned(),
            ));
        }
        Ok(reply.get("result").cloned().unwrap_or(Value::Null))
    }
    pub fn execute(&mut self, command: DebugCommand) -> Result<DebugObservation, ControlError> {
        if self.closed {
            return Err(ControlError::Rejected("debug session is closed".into()));
        }
        let close = matches!(
            command,
            DebugCommand::Close | DebugCommand::L6MidiFinish { .. }
        );
        let result = (|| {
            serde_json::to_writer(&mut self.writer, &command).map_err(ControlError::Protocol)?;
            writeln!(self.writer).map_err(ControlError::Io)?;
            let value = self.read_reply()?;
            serde_json::from_value(value).map_err(ControlError::Protocol)
        })();
        if close || result.is_err() {
            self.closed = true;
            let _ = self.writer.shutdown(std::net::Shutdown::Both);
        }
        result
    }
}
impl Drop for SocketDebugSession {
    fn drop(&mut self) {
        let _ = self.writer.shutdown(std::net::Shutdown::Both);
    }
}

/// Client for an emulator explicitly started with `--control-socket`.
#[derive(Clone, Debug)]
pub struct ControlClient {
    socket: PathBuf,
}

impl ControlClient {
    pub fn new(socket: impl Into<PathBuf>) -> Self {
        Self {
            socket: socket.into(),
        }
    }

    /// Pause MAIN and begin a scoped, exclusive debug session over this connection.
    pub fn debug_session(&self) -> Result<SocketDebugSession, ControlError> {
        let mut writer =
            UnixStream::connect(&self.socket).map_err(|source| ControlError::Connect {
                path: self.socket.clone(),
                source,
            })?;
        // A maximum-length continue may need an interrupt plus bounded
        // breakpoint/scratch cleanup before the host can return its error.
        writer
            .set_read_timeout(Some(Duration::from_secs(100)))
            .map_err(ControlError::Io)?;
        writer
            .set_write_timeout(Some(Duration::from_secs(5)))
            .map_err(ControlError::Io)?;
        writeln!(writer, "{{\"command\":\"debug-begin\"}}").map_err(ControlError::Io)?;
        let reader = BufReader::new(writer.try_clone().map_err(ControlError::Io)?);
        let mut session = SocketDebugSession {
            writer,
            reader,
            closed: false,
        };
        session.read_reply()?;
        Ok(session)
    }

    pub fn socket(&self) -> &Path {
        &self.socket
    }

    /// Sample aggregate counters for one running emulator input channel.
    pub fn input_status(&self, source: InputSource) -> Result<InputCounters, ControlError> {
        self.request_as(&Request::InputStatus { source })
    }
    pub fn input_ack(&self, source: InputSource, sequence: u32) -> Result<InputAck, ControlError> {
        self.request_as(&Request::InputAck { source, sequence })
    }
    pub fn sd_loader_status(&self) -> Result<SdLoaderSnapshot, ControlError> {
        self.request_as(&Request::SdLoaderStatus)
    }
    pub fn sd_loader_site(&self, index: u32) -> Result<SdLoaderSite, ControlError> {
        self.request_as(&Request::SdLoaderSite { index })
    }

    /// Send one typed request. RTOS response bodies are currently the host's
    /// versioned schema-1 JSON; their typed shared interpretation remains in
    /// `l6max_host::freertos` and will be unified in a later slice.
    pub fn request(&self, request: &Request) -> Result<Value, ControlError> {
        let mut stream =
            UnixStream::connect(&self.socket).map_err(|source| ControlError::Connect {
                path: self.socket.clone(),
                source,
            })?;
        // SD inspection itself owns a 30-second deadline; leave room to
        // serialize and return its bounded error to this client.
        let timeout = if matches!(
            request,
            Request::SdLoaderStatus | Request::SdLoaderSite { .. }
        ) {
            35
        } else if matches!(
            request,
            Request::Rtos { .. }
                | Request::HeapCounters
                | Request::MemoryRead { .. }
                | Request::MidiConfig
                | Request::EffectEditor { .. }
                | Request::UpdateState
                | Request::UpdateGuiState
                | Request::KnobPopup { .. }
                | Request::RecorderState
                | Request::KnobParameterState
        ) {
            30
        } else {
            3
        };
        stream
            .set_read_timeout(Some(Duration::from_secs(timeout)))
            .map_err(ControlError::Io)?;
        serde_json::to_writer(&mut stream, request).map_err(ControlError::Protocol)?;
        writeln!(stream).map_err(ControlError::Io)?;
        let mut reply = String::new();
        BufReader::new(stream)
            .read_line(&mut reply)
            .map_err(ControlError::Io)?;
        let reply: Value = serde_json::from_str(&reply).map_err(ControlError::Protocol)?;
        if reply.get("ok").and_then(Value::as_bool) != Some(true) {
            return Err(ControlError::Rejected(
                reply
                    .get("error")
                    .and_then(Value::as_str)
                    .unwrap_or("control request failed")
                    .to_owned(),
            ));
        }
        Ok(reply.get("result").cloned().unwrap_or(Value::Null))
    }

    pub fn info(&self) -> Result<Info, ControlError> {
        self.request_as(&Request::Info)
    }

    /// One guarded, non-atomic live counter read (no heap traversal).
    pub fn heap_counters(&self) -> Result<HeapCounters, ControlError> {
        self.request_as(&Request::HeapCounters)
    }

    pub fn read_main_memory(
        &self,
        address: u32,
        length: usize,
    ) -> Result<MemoryRead, ControlError> {
        self.request_as(&Request::MemoryRead { address, length })
    }

    pub fn effect_editor(
        &self,
        binding: EffectEditorBinding,
    ) -> Result<EffectEditorSnapshot, ControlError> {
        self.request_as(&Request::EffectEditor { binding })
    }

    pub fn midi_config(&self) -> Result<MidiConfig, ControlError> {
        self.request_as(&Request::MidiConfig)
    }

    pub fn installer_outcome(&self) -> Result<InstallerOutcome, ControlError> {
        self.request_as(&Request::InstallerOutcome)
    }

    pub fn usb_attach(&self) -> Result<UsbAttachReceipt, ControlError> {
        self.request_as(&Request::UsbAttach)
    }
    pub fn usb_release(&self) -> Result<UsbAttachReceipt, ControlError> {
        self.request_as(&Request::UsbRelease)
    }
    pub fn usb_controller(&self) -> Result<UsbControllerStatus, ControlError> {
        self.request_as(&Request::UsbController)
    }

    pub fn update_state(&self) -> Result<UpdateState, ControlError> {
        self.request_as(&Request::UpdateState)
    }

    pub fn update_gui_state(&self) -> Result<UpdateGuiState, ControlError> {
        self.request_as(&Request::UpdateGuiState)
    }

    /// Request the host's existing versioned RTOS JSON snapshot.
    pub fn rtos(&self, kind: RtosKind) -> Result<Value, ControlError> {
        self.request(&Request::Rtos { kind })
    }

    pub fn knob_parameter_state(&self) -> Result<ParameterState, ControlError> {
        self.request_as(&Request::KnobParameterState)
    }

    pub fn recorder_state(&self) -> Result<RecorderState, ControlError> {
        self.request_as(&Request::RecorderState)
    }

    pub fn knob_popup(&self, with_text: bool) -> Result<KnobPopupSnapshot, ControlError> {
        self.request_as(&Request::KnobPopup { with_text })
    }

    pub fn tap(&self, target: u32, hold_ms: u32) -> Result<ButtonReceipt, ControlError> {
        self.request_as(&Request::Tap { target, hold_ms })
    }

    pub fn down(&self, target: u32, duration_ms: u32) -> Result<ButtonReceipt, ControlError> {
        self.request_as(&Request::Down {
            target,
            duration_ms,
        })
    }

    pub fn up(&self, target: u32) -> Result<ButtonReceipt, ControlError> {
        self.request_as(&Request::Up { target })
    }

    /// `channel` is one-based, matching the existing socket and CLI.
    pub fn encoder(&self, channel: u32, steps: i32) -> Result<AxisReceipt, ControlError> {
        self.request_as(&Request::Encoder { channel, steps })
    }

    pub fn analog(&self, channel: u32, steps: i32) -> Result<AxisReceipt, ControlError> {
        self.request_as(&Request::Analog { channel, steps })
    }

    pub fn set_analog(
        &self,
        channel: u32,
        value: u16,
    ) -> Result<AnalogPositionReceipt, ControlError> {
        self.request_as(&Request::SetAnalog { channel, value })
    }

    /// Deserialize a response into a caller's typed schema.
    pub fn request_as<T: DeserializeOwned>(&self, request: &Request) -> Result<T, ControlError> {
        serde_json::from_value(self.request(request)?).map_err(ControlError::Protocol)
    }
}

/// Access the same host-owned guest directly without opening a control socket.
/// The engine and its QMP/input resources remain owned by the caller.
pub struct OwnedControl<'a> {
    engine: &'a Engine,
}

impl<'a> OwnedControl<'a> {
    pub fn new(engine: &'a Engine) -> Self {
        Self { engine }
    }

    pub fn debug_session(&self) -> Result<l6max_host::debug::Session, ControlError> {
        self.engine
            .debugger()
            .ok_or(ControlError::Unavailable(
                "debugger unavailable; launch with --gdb-socket",
            ))?
            .begin()
            .map_err(ControlError::Rejected)
    }

    pub fn info(&self) -> Result<Info, ControlError> {
        let inputs = self
            .engine
            .inputs
            .as_ref()
            .ok_or(ControlError::Unavailable("QEMU input unavailable"))?;
        Ok(control::info(inputs, self.engine.model()))
    }

    /// Sample the host's channel counters without opening a control socket.
    pub fn input_status(&self, source: InputSource) -> Result<InputCounters, ControlError> {
        let inputs = self
            .engine
            .inputs
            .as_ref()
            .ok_or(ControlError::Unavailable("QEMU input unavailable"))?;
        Ok(inputs.counters(source))
    }
    pub fn input_ack(&self, source: InputSource, sequence: u32) -> Result<InputAck, ControlError> {
        let inputs = self
            .engine
            .inputs
            .as_ref()
            .ok_or(ControlError::Unavailable("QEMU input unavailable"))?;
        Ok(inputs.acknowledgement(source, sequence))
    }
    fn main_qmp(&self) -> Result<&qmp::QmpClient, ControlError> {
        self.engine
            .management
            .first()
            .ok_or(ControlError::Unavailable("QEMU management unavailable"))
    }
    pub fn sd_loader_reader(
        &self,
        deadline: std::time::Instant,
    ) -> Result<l6max_host::sd_loader::Reader<'_>, ControlError> {
        l6max_host::sd_loader::Reader::new(self.main_qmp()?, self.engine.model(), deadline)
            .map_err(ControlError::Rejected)
    }
    pub fn sd_loader_status(&self) -> Result<SdLoaderSnapshot, ControlError> {
        l6max_host::sd_loader::snapshot(self.main_qmp()?, self.engine.model())
            .map_err(ControlError::Rejected)
    }
    pub fn sd_loader_site(&self, index: u32) -> Result<SdLoaderSite, ControlError> {
        l6max_host::sd_loader::site(self.main_qmp()?, self.engine.model(), index)
            .map_err(ControlError::Rejected)
    }

    /// Validate the allocator once for a bounded set of live counter reads.
    /// Recreate this reader after a guest restart or firmware change.
    pub fn heap_counter_reader(&self) -> Result<l6max_host::heap::CounterReader, ControlError> {
        l6max_host::heap::counter_reader(self.main_qmp()?, self.engine.model())
            .map_err(ControlError::Rejected)
    }

    pub fn read_main_memory(
        &self,
        address: u32,
        length: usize,
    ) -> Result<MemoryRead, ControlError> {
        l6max_host::memory::read_main(self.main_qmp()?, address, length)
            .map_err(ControlError::Rejected)
    }

    pub fn read_main_memory_before(
        &self,
        address: u32,
        length: usize,
        deadline: std::time::Instant,
    ) -> Result<MemoryRead, ControlError> {
        l6max_host::memory::read_main_before(self.main_qmp()?, address, length, deadline)
            .map_err(ControlError::Rejected)
    }

    pub fn effect_editor_reader_before(
        &self,
        binding: EffectEditorBinding,
        deadline: std::time::Instant,
    ) -> Result<l6max_host::effect_editor::Reader, ControlError> {
        l6max_host::effect_editor::Reader::new_before(
            self.main_qmp()?,
            self.engine.model(),
            binding,
            deadline,
        )
        .map_err(ControlError::Rejected)
    }

    pub fn effect_editor_reader_before_checked(
        &self,
        binding: EffectEditorBinding,
        deadline: std::time::Instant,
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<l6max_host::effect_editor::Reader, ControlError> {
        l6max_host::effect_editor::Reader::new_before_checked(
            self.main_qmp()?,
            self.engine.model(),
            binding,
            deadline,
            check,
        )
        .map_err(ControlError::Rejected)
    }

    pub fn effect_editor_reader(
        &self,
        binding: EffectEditorBinding,
    ) -> Result<l6max_host::effect_editor::Reader, ControlError> {
        l6max_host::effect_editor::Reader::new(self.main_qmp()?, self.engine.model(), binding)
            .map_err(ControlError::Rejected)
    }

    pub fn midi_config(&self) -> Result<MidiConfig, ControlError> {
        l6max_host::midi_config::config(self.main_qmp()?, self.engine.model())
            .map_err(ControlError::Rejected)
    }

    pub fn update_state(&self) -> Result<UpdateState, ControlError> {
        l6max_host::update_state::update_state(self.main_qmp()?, self.engine.model())
            .map_err(ControlError::Rejected)
    }

    pub fn update_gui_state(&self) -> Result<UpdateGuiState, ControlError> {
        l6max_host::update_state::update_gui_state(self.main_qmp()?, self.engine.model())
            .map_err(ControlError::Rejected)
    }

    pub fn installer_outcome(&self) -> Result<InstallerOutcome, ControlError> {
        self.engine
            .installer_outcome()
            .cloned()
            .ok_or(ControlError::Unavailable(
                "replacement installer was not run for this engine start",
            ))
    }

    pub fn usb_attach(&self) -> Result<UsbAttachReceipt, ControlError> {
        self.engine
            .usb_controller()
            .ok_or(ControlError::Unavailable("USB controller unavailable"))?
            .attach()
            .map_err(ControlError::Rejected)?;
        Ok(UsbAttachReceipt { attached: true })
    }
    pub fn usb_release(&self) -> Result<UsbAttachReceipt, ControlError> {
        self.engine
            .usb_controller()
            .ok_or(ControlError::Unavailable("USB controller unavailable"))?
            .release()
            .map_err(ControlError::Rejected)?;
        Ok(UsbAttachReceipt { attached: false })
    }
    pub fn usb_controller(&self) -> Result<UsbControllerStatus, ControlError> {
        let running = self
            .engine
            .usb_controller()
            .ok_or(ControlError::Unavailable("USB controller unavailable"))?
            .running()
            .map_err(ControlError::Rejected)?;
        Ok(UsbControllerStatus { running })
    }

    /// Enqueue a one-based encoder input, without waiting for guest acknowledgment.
    pub fn encoder(&self, channel: u32, steps: i32) -> Result<AxisReceipt, ControlError> {
        let inputs = self
            .engine
            .inputs
            .as_ref()
            .ok_or(ControlError::Unavailable("QEMU input unavailable"))?;
        let sequence = control::enqueue_encoder(inputs, self.engine.model(), channel, steps)
            .map_err(ControlError::Rejected)?;
        Ok(AxisReceipt {
            channel,
            steps,
            sequence,
        })
    }

    /// Enqueue a tap without waiting for the guest to apply it.
    pub fn tap(&self, target: u32, hold_ms: u32) -> Result<ButtonReceipt, ControlError> {
        let inputs = self
            .engine
            .inputs
            .as_ref()
            .ok_or(ControlError::Unavailable("QEMU input unavailable"))?;
        control::enqueue_tap(inputs, target, hold_ms).map_err(ControlError::Rejected)?;
        Ok(ButtonReceipt {
            target,
            command: "tap".into(),
        })
    }

    /// Verify the GUI and parameter getter revisions for one bounded observation.
    pub fn knob_parameter_reader(
        &self,
    ) -> Result<l6max_host::knob_popup::ParameterReader<'_>, ControlError> {
        l6max_host::knob_popup::parameter_reader(self.main_qmp()?, self.engine.model())
            .map_err(ControlError::Rejected)
    }

    /// Verify the GUI revision once for a bounded in-process observation step.
    pub fn knob_popup_reader(&self) -> Result<l6max_host::knob_popup::Reader<'_>, ControlError> {
        l6max_host::knob_popup::reader(self.main_qmp()?, self.engine.model())
            .map_err(ControlError::Rejected)
    }

    pub fn knob_popup(&self, with_text: bool) -> Result<KnobPopupSnapshot, ControlError> {
        l6max_host::knob_popup::snapshot(self.main_qmp()?, self.engine.model(), with_text)
            .map_err(ControlError::Rejected)
    }

    pub fn rtos_heap(&self) -> Result<freertos::HeapSnapshot, ControlError> {
        freertos::heap(self.main_qmp()?, self.engine.model()).map_err(ControlError::Rejected)
    }

    pub fn rtos_tasks(&self) -> Result<freertos::TasksSnapshot, ControlError> {
        freertos::tasks(self.main_qmp()?, self.engine.model()).map_err(ControlError::Rejected)
    }

    pub fn rtos_scheduler(&self) -> Result<freertos::SchedulerSnapshot, ControlError> {
        freertos::scheduler(self.main_qmp()?, self.engine.model()).map_err(ControlError::Rejected)
    }

    pub fn rtos_objects(&self) -> Result<Vec<freertos::Object>, ControlError> {
        freertos::objects(self.main_qmp()?, self.engine.model()).map_err(ControlError::Rejected)
    }
}

/// Host and direct USB MIDI consumer sessions.
pub mod midi;
