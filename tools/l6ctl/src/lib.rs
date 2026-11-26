//! Typed control of an emulator owned in process or exposed by its control socket.
//!
//! The socket interface is deliberately narrower than direct access. In
//! particular its input replies confirm enqueueing, not guest acknowledgement.
pub use l6max_host::audio_trace::{
    L6RecorderBaseline, L6RecorderSample, SAI1_TX_PREFIX, TxSample as Sai1TxSample,
    l6_recorder_baseline, l6_recorder_samples, l6_recorder_signature_changed, sai1_tx_nonzero,
    sai1_tx_nonzero_counts, sai1_tx_samples,
};
pub use l6max_host::control::Info;
pub use l6max_host::debug::{Command as DebugCommand, Observation as DebugObservation};
pub use l6max_host::effect_editor::{
    Binding as EffectEditorBinding, SlotBinding as EffectSlotBinding,
    SlotFields as EffectSlotFields, SlotSnapshot as EffectSlotSnapshot,
    Snapshot as EffectEditorSnapshot,
};
pub use l6max_host::heap::Counters as HeapCounters;
pub use l6max_host::indicators::rings as decode_indicator_rings;
pub use l6max_host::input::button_source;
pub use l6max_host::input::{InputAck, InputAckState, InputCounters, InputSource};
pub use l6max_host::knob_binding::{
    FirmwareBinding as KnobFirmwareBinding, PreflightTargets as KnobPreflightTargets,
    SdBinding as KnobSdBinding, verify_bitmap_probe_state, verify_notification_semaphore,
    verify_preflight_widget60,
};
pub use l6max_host::knob_popup::{
    NavigationSnapshot, OverlayDescriptors, ParameterState, RecorderState, RestorationPointers,
    RestorationSnapshot, Snapshot as KnobPopupSnapshot,
};
pub use l6max_host::memory::MemoryRead;
pub use l6max_host::menu::Snapshot as MenuSnapshot;
pub use l6max_host::midi_config::Config as MidiConfig;
pub use l6max_host::qmp::Status as QmpStatus;
pub use l6max_host::sd_loader::{Site as SdLoaderSite, Snapshot as SdLoaderSnapshot};
pub use l6max_host::usb::Enumeration as UsbEnumeration;

pub use l6max_host::editor::pad_assignment_present as editor_pad_assignment_present;
pub use l6max_host::editor::{
    pad_assignment_matches as editor_pad_assignment_matches,
    pad_file_count as editor_pad_file_count, pad_filename_valid as editor_pad_filename_valid,
};
pub use l6max_host::input_trace::panel_uart_count;
pub use l6max_host::l6_din::Snapshot as L6DinSnapshot;
pub use l6max_host::l6_recorder::{
    Dma as L6RecorderDma, Event as L6RecorderEvent, Selection as L6RecorderSelection,
};
pub use l6max_host::mixer_level::MixerLevel;
pub use l6max_host::panel_keys::Snapshot as PanelKeySnapshot;
pub use l6max_host::sound_pad::Assignment as SoundPadAssignment;
pub use l6max_host::update_bootloader::Outcome as InstallerOutcome;
pub use l6max_host::update_state::{UpdateGuiState, UpdateState};
pub use l6max_host::usb_midi::SysexDecoder as UsbSysexDecoder;
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
    QmpStatus,
    WarmReset {
        timeout_s: u64,
    },
    SdCard {
        path: Option<String>,
    },
    InputStatus {
        source: InputSource,
    },
    InputAck {
        source: InputSource,
        sequence: u32,
    },
    SdLoaderStatus,
    SdLoaderSite {
        index: u32,
    },
    HeapCounters,
    Menu {
        menu: String,
        path: Vec<String>,
    },
    MemoryRead {
        address: u32,
        length: usize,
    },
    SymbolRead {
        symbol: String,
        length: usize,
    },
    MidiConfig,
    L6DinState,
    EffectEditor {
        binding: EffectEditorBinding,
    },
    EffectSlot {
        binding: EffectSlotBinding,
        fields: EffectSlotFields,
    },

    MixerLevel,
    PanelKey {
        code: u32,
    },
    SoundPadAssignment {
        pad: u8,
        path: Option<String>,
    },
    UpdateState,
    UpdateGuiState,
    InstallerOutcome,
    UsbAttach,
    UsbRelease,
    UsbController,
    UsbEnumerate,
    KnobPopup {
        with_text: bool,
    },
    RecorderState,
    KnobParameterState,
    Rtos {
        kind: RtosKind,
    },
    Tap {
        target: u32,
        hold_ms: u32,
    },
    Down {
        target: u32,
        duration_ms: u32,
    },
    Up {
        target: u32,
    },
    Encoder {
        channel: u32,
        steps: i32,
    },
    Analog {
        channel: u32,
        steps: i32,
    },
    SetAnalog {
        channel: u32,
        value: u16,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct ResetReceipt {
    pub reset: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct SdCardReceipt {
    pub inserted: bool,
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
    pub sequence: u32,
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
            DebugCommand::Close
                | DebugCommand::L6MidiFinish { .. }
                | DebugCommand::L6DinFinish { .. }
                | DebugCommand::L6MeterFinish { .. }
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
    pub fn qmp_status(&self) -> Result<QmpStatus, ControlError> {
        self.request_as(&Request::QmpStatus)
    }
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
        let timeout = if let Request::WarmReset { timeout_s } = request {
            timeout_s.saturating_add(5)
        } else if matches!(request, Request::UsbEnumerate) {
            105
        } else if matches!(
            request,
            Request::SdLoaderStatus | Request::SdLoaderSite { .. } | Request::Menu { .. }
        ) {
            35
        } else if matches!(
            request,
            Request::Rtos { .. }
                | Request::HeapCounters
                | Request::MemoryRead { .. }
                | Request::SymbolRead { .. }
                | Request::MidiConfig
                | Request::L6DinState
                | Request::EffectEditor { .. }
                | Request::EffectSlot { .. }
                | Request::MixerLevel
                | Request::PanelKey { .. }
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

    pub fn warm_reset(&self, timeout_s: u64) -> Result<ResetReceipt, ControlError> {
        self.request_as(&Request::WarmReset { timeout_s })
    }
    pub fn sd_card(&self, path: Option<String>) -> Result<SdCardReceipt, ControlError> {
        self.request_as(&Request::SdCard { path })
    }

    pub fn info(&self) -> Result<Info, ControlError> {
        self.request_as(&Request::Info)
    }

    /// One guarded, non-atomic live counter read (no heap traversal).
    pub fn heap_counters(&self) -> Result<HeapCounters, ControlError> {
        self.request_as(&Request::HeapCounters)
    }
    pub fn menu(&self, menu: &str, path: &[String]) -> Result<MenuSnapshot, ControlError> {
        self.request_as(&Request::Menu {
            menu: menu.into(),
            path: path.into(),
        })
    }

    pub fn read_main_memory(
        &self,
        address: u32,
        length: usize,
    ) -> Result<MemoryRead, ControlError> {
        self.request_as(&Request::MemoryRead { address, length })
    }

    pub fn read_symbol(&self, symbol: &str, length: usize) -> Result<MemoryRead, ControlError> {
        self.request_as(&Request::SymbolRead {
            symbol: symbol.into(),
            length,
        })
    }

    pub fn effect_editor(
        &self,
        binding: EffectEditorBinding,
    ) -> Result<EffectEditorSnapshot, ControlError> {
        self.request_as(&Request::EffectEditor { binding })
    }

    pub fn effect_slot(
        &self,
        binding: EffectSlotBinding,
        fields: EffectSlotFields,
    ) -> Result<EffectSlotSnapshot, ControlError> {
        self.request_as(&Request::EffectSlot { binding, fields })
    }

    pub fn l6_din_state(&self) -> Result<L6DinSnapshot, ControlError> {
        self.request_as(&Request::L6DinState)
    }

    pub fn sound_pad_assignment(
        &self,
        pad: u8,
        path: Option<String>,
    ) -> Result<SoundPadAssignment, ControlError> {
        self.request_as(&Request::SoundPadAssignment { pad, path })
    }

    pub fn panel_key(&self, code: u32) -> Result<PanelKeySnapshot, ControlError> {
        self.request_as(&Request::PanelKey { code })
    }

    pub fn midi_config(&self) -> Result<MidiConfig, ControlError> {
        self.request_as(&Request::MidiConfig)
    }

    pub fn mixer_level(&self) -> Result<MixerLevel, ControlError> {
        self.request_as(&Request::MixerLevel)
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
    pub fn usb_enumerate(&self) -> Result<UsbEnumeration, ControlError> {
        self.request_as(&Request::UsbEnumerate)
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

    /// Enable the in-process guest's DIN/TRS MIDI bridge.
    pub fn connect_din(&self) -> Result<(), ControlError> {
        if self.engine.din_midi.is_none() {
            return Err(ControlError::Unavailable("DIN MIDI unavailable"));
        }
        self.engine.connect_midi();
        Ok(())
    }
    /// Connect a direct MIDI consumer without exposing the engine's USB transport.
    pub fn direct_midi_before(
        &self,
        cable: u8,
        budget: &l6max_host::usb::Budget,
    ) -> Result<midi::MidiSession, ControlError> {
        let usb = self
            .engine
            .usb
            .as_ref()
            .ok_or(ControlError::Unavailable("USB host unavailable"))?
            .clone();
        midi::MidiSession::connect_direct_before(usb, cable, budget).map_err(ControlError::Rejected)
    }

    /// Reserve the owned guest's USB controller for raw USB-MIDI observation.
    pub fn raw_usb_midi(&self) -> Result<l6max_host::usb_midi::RawReceiver, ControlError> {
        let usb = self
            .engine
            .usb
            .as_ref()
            .ok_or(ControlError::Unavailable("USB host unavailable"))?
            .clone();
        l6max_host::usb_midi::RawReceiver::attach(usb).map_err(ControlError::Rejected)
    }

    /// Claim endpoints from an existing enumeration without losing queued packets.
    pub fn raw_usb_midi_enumerated(
        &self,
        endpoints: l6max_host::usb::Endpoints,
    ) -> Result<l6max_host::usb_midi::RawReceiver, ControlError> {
        let usb = self
            .engine
            .usb
            .as_ref()
            .ok_or(ControlError::Unavailable("USB host unavailable"))?
            .clone();
        l6max_host::usb_midi::RawReceiver::claim_enumerated(usb, endpoints)
            .map_err(|error| ControlError::Rejected(error.to_string()))
    }

    pub fn menu_before(
        &self,
        menu: &str,
        path: &[String],
        deadline: std::time::Instant,
        cancelled: impl Fn() -> bool,
    ) -> Result<MenuSnapshot, ControlError> {
        l6max_host::menu::snapshot_before(
            self.main_qmp()?,
            self.engine.model(),
            menu,
            path,
            deadline,
            cancelled,
        )
        .map_err(ControlError::Rejected)
    }
    /// Enumerate the engine's reserved raw controller without replacing its pump.
    pub fn usb_enumerate_before(
        &self,
        budget: &l6max_host::usb::Budget,
    ) -> Result<(Vec<u8>, Vec<u8>, l6max_host::usb::Configuration), ControlError> {
        let result = self
            .engine
            .usb_controller()
            .ok_or(ControlError::Unavailable("USB controller unavailable"))?
            .enumerate_before(budget)
            .map_err(ControlError::Rejected)?;
        Ok((result.device, result.configuration, result.parsed))
    }
    pub fn usb_attach_before(&self, budget: &l6max_host::usb::Budget) -> Result<(), ControlError> {
        self.engine
            .usb_controller()
            .ok_or(ControlError::Unavailable("USB controller unavailable"))?
            .attach_before(budget)
            .map_err(ControlError::Rejected)
    }
    pub fn usb_controller_before(
        &self,
        budget: &l6max_host::usb::Budget,
    ) -> Result<bool, ControlError> {
        self.engine
            .usb_controller()
            .ok_or(ControlError::Unavailable("USB controller unavailable"))?
            .running_before(budget)
            .map_err(ControlError::Rejected)
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

    /// Begin a bounded in-process debugger session.
    pub fn debug_session_before(
        &self,
        deadline: std::time::Instant,
    ) -> Result<l6max_host::debug::Session, ControlError> {
        self.engine
            .debugger()
            .ok_or(ControlError::Unavailable(
                "debugger unavailable; launch with --gdb-socket",
            ))?
            .begin_before(deadline)
            .map_err(ControlError::Rejected)
    }

    /// Coordinate breakpoints while both initially paused guests and input
    /// replay are observed by the caller. This is an in-process API only.
    pub fn debug_observer_before(
        &self,
        deadline: std::time::Instant,
    ) -> Result<l6max_host::debug::Observer, ControlError> {
        let controller = self.engine.debugger().ok_or(ControlError::Unavailable(
            "debugger unavailable; launch with --gdb-socket",
        ))?;
        let main = self
            .engine
            .management
            .first()
            .ok_or(ControlError::Unavailable("MAIN management unavailable"))?;
        let panel = self
            .engine
            .management
            .get(1)
            .ok_or(ControlError::Unavailable("panel management unavailable"))?;
        l6max_host::debug::Observer::acquire(controller, main, panel, deadline)
            .map_err(ControlError::Rejected)
    }

    /// Wait for QEMU's reset event; guest readiness is a separate observation.
    pub fn warm_reset(&self, timeout: std::time::Duration) -> Result<ResetReceipt, ControlError> {
        self.main_qmp()?
            .system_reset_and_wait(timeout)
            .map_err(|e| ControlError::Rejected(e.to_string()))?;
        Ok(ResetReceipt {
            reset: "qmp-system-reset".into(),
        })
    }
    pub fn sd_card(&self, path: Option<&std::path::Path>) -> Result<SdCardReceipt, ControlError> {
        let qmp = self.main_qmp()?;
        match path {
            Some(path) => qmp.insert_sd(path),
            None => qmp.eject_sd(),
        }
        .map_err(|e| ControlError::Rejected(e.to_string()))?;
        Ok(SdCardReceipt {
            inserted: path.is_some(),
        })
    }

    pub fn info(&self) -> Result<Info, ControlError> {
        let inputs = self
            .engine
            .inputs
            .as_ref()
            .ok_or(ControlError::Unavailable("QEMU input unavailable"))?;
        Ok(control::info(inputs, self.engine.model()))
    }

    /// Create a revision-guarded L6 recorder reader for a bounded transaction.
    /// Create a guarded reader for one stopped L6max guest transaction.
    pub fn l6max_recorder_reader_before(
        &self,
        deadline: std::time::Instant,
        cancelled: impl FnMut() -> bool,
    ) -> Result<l6max_host::l6max_recorder::Reader, String> {
        l6max_host::l6max_recorder::Reader::new(
            &self.engine.management[0],
            self.engine.model(),
            deadline,
            cancelled,
        )
    }
    /// Own the guest USB mass-storage session for one bounded transaction.
    pub fn usb_storage_before(
        &self,
        deadline: std::time::Instant,
        guest_start: u32,
        guest_budget: u32,
    ) -> Result<l6max_host::usb_storage::Session, String> {
        let usb = self
            .engine
            .usb
            .as_ref()
            .ok_or("USB backend unavailable")?
            .clone();
        let panel = self
            .engine
            .inputs
            .as_ref()
            .ok_or("panel input unavailable")?
            .panel
            .clone();
        let budget = l6max_host::usb::Budget::new(deadline, move || {
            panel.snapshot().guest_ms.wrapping_sub(guest_start) > guest_budget
        });
        l6max_host::usb_storage::Session::open(usb, budget).map_err(|e| e.to_string())
    }
    /// Pause both emulated processors, restoring their prior run states on close.
    pub fn pause_all_before(
        &self,
        deadline: std::time::Instant,
        cancelled: impl FnMut() -> bool,
    ) -> Result<l6max_host::pause_scope::PauseScope, String> {
        l6max_host::pause_scope::PauseScope::acquire(&self.engine.management, deadline, cancelled)
    }
    pub fn l6_recorder_reader_before(
        &self,
        deadline: std::time::Instant,
        cancelled: impl FnMut() -> bool,
    ) -> Result<l6max_host::l6_recorder::Reader, ControlError> {
        l6max_host::l6_recorder::Reader::new(
            self.main_qmp()?,
            self.engine.model(),
            deadline,
            cancelled,
        )
        .map_err(ControlError::Rejected)
    }

    /// Sample the host's channel counters without opening a control socket.
    pub fn qmp_status_before(
        &self,
        deadline: std::time::Instant,
    ) -> Result<QmpStatus, ControlError> {
        self.main_qmp()?
            .status_before(deadline)
            .map_err(|e| ControlError::Rejected(e.to_string()))
    }
    pub fn qmp_status(&self) -> Result<QmpStatus, ControlError> {
        self.main_qmp()?
            .query_status()
            .map_err(|e| ControlError::Rejected(e.to_string()))
    }
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
    pub fn sd_loader_reader_cancellable(
        &self,
        deadline: std::time::Instant,
        cancelled: impl FnMut() -> bool,
    ) -> Result<l6max_host::sd_loader::Reader<'_>, ControlError> {
        l6max_host::sd_loader::Reader::new_cancellable(
            self.main_qmp()?,
            self.engine.model(),
            deadline,
            cancelled,
        )
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

    /// Read allocator counters with the host's plausibility checks.
    pub fn checked_heap(&self) -> Result<l6max_host::heap::Heap, ControlError> {
        l6max_host::heap::read(self.main_qmp()?, self.engine.model())
            .map_err(ControlError::Rejected)
    }

    /// Validate the allocator once for a bounded set of live counter reads.
    /// Recreate this reader after a guest restart or firmware change.
    pub fn heap_counter_reader(&self) -> Result<l6max_host::heap::CounterReader, ControlError> {
        l6max_host::heap::counter_reader(self.main_qmp()?, self.engine.model())
            .map_err(ControlError::Rejected)
    }

    pub fn read_symbol(&self, symbol: &str, length: usize) -> Result<MemoryRead, ControlError> {
        l6max_host::memory::read_symbol(self.main_qmp()?, self.engine.model(), symbol, length)
            .map_err(ControlError::Rejected)
    }
    pub fn read_symbol_before(
        &self,
        symbol: &str,
        length: usize,
        deadline: std::time::Instant,
    ) -> Result<MemoryRead, ControlError> {
        l6max_host::memory::read_symbol_before(
            self.main_qmp()?,
            self.engine.model(),
            symbol,
            length,
            deadline,
        )
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

    /// Send MIDI through a configured raw endpoint, preserving encoder state.
    pub fn usb_midi_send(
        &self,
        output: u8,
        encoder: &mut l6max_host::usb_midi::Encoder,
        cable: u8,
        bytes: &[u8],
    ) -> std::io::Result<()> {
        let usb = self
            .engine
            .usb
            .as_ref()
            .ok_or_else(|| std::io::Error::other("USB backend unavailable"))?;
        usb.send(output, &encoder.push(cable, bytes))
    }
    /// Poll and decode one transfer, retaining cable and packet ordering.
    pub fn usb_midi_poll(&self, input: u8) -> std::io::Result<Option<Vec<(u8, Vec<u8>)>>> {
        let usb = self
            .engine
            .usb
            .as_ref()
            .ok_or_else(|| std::io::Error::other("USB backend unavailable"))?;
        l6max_host::usb_midi::poll_endpoint(usb, input)
    }

    /// Send through an already-enumerated USB endpoint owned by the caller.
    pub fn usb_sysex_send(
        &self,
        endpoints: l6max_host::usb::Endpoints,
        cable: u8,
        bytes: &[u8],
    ) -> Result<(), ControlError> {
        let usb = self
            .engine
            .usb
            .as_ref()
            .ok_or_else(|| ControlError::Rejected("USB backend unavailable".into()))?;
        l6max_host::usb_midi::send_sysex(usb, endpoints.output, cable, bytes)
            .map_err(|error| ControlError::Rejected(error.to_string()))
    }
    /// Poll the same endpoint without reconnecting or taking ownership of it.
    pub fn usb_sysex_poll(
        &self,
        endpoints: l6max_host::usb::Endpoints,
        cable: u8,
        decoder: &mut UsbSysexDecoder,
    ) -> Result<(), ControlError> {
        let usb = self
            .engine
            .usb
            .as_ref()
            .ok_or_else(|| ControlError::Rejected("USB backend unavailable".into()))?;
        l6max_host::usb_midi::poll_sysex(usb, endpoints.input, cable, decoder)
            .map_err(|error| ControlError::Rejected(error.to_string()))
    }

    pub fn l6_din_reader_before_checked(
        &self,
        deadline: std::time::Instant,
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<l6max_host::l6_din::Reader, ControlError> {
        l6max_host::l6_din::Reader::new_before_checked(
            self.main_qmp()?,
            self.engine.model(),
            deadline,
            check,
        )
        .map_err(ControlError::Rejected)
    }

    pub fn effect_slot_reader_before_checked(
        &self,
        binding: EffectSlotBinding,
        deadline: std::time::Instant,
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<l6max_host::effect_editor::SlotReader, ControlError> {
        l6max_host::effect_editor::SlotReader::new_before_checked(
            self.main_qmp()?,
            self.engine.model(),
            binding,
            deadline,
            check,
        )
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

    /// Enqueue an input on a specific emulated channel, including unsupported
    /// targets used by protocol rejection diagnostics. Returns its sequence.
    pub fn raw_tap(
        &self,
        source: InputSource,
        target: u32,
        hold_ms: u32,
    ) -> Result<u32, ControlError> {
        let inputs = self
            .engine
            .inputs
            .as_ref()
            .ok_or(ControlError::Unavailable("QEMU input unavailable"))?;
        let input = match source {
            InputSource::Main => &inputs.main,
            InputSource::Panel => &inputs.panel,
        };
        input
            .tap(target, hold_ms)
            .map_err(|error| ControlError::Rejected(error.to_string()))
    }
    /// Enqueue a raw down on the explicitly selected channel.
    pub fn raw_down(
        &self,
        source: InputSource,
        target: u32,
        duration_ms: u32,
    ) -> Result<u32, ControlError> {
        let inputs = self
            .engine
            .inputs
            .as_ref()
            .ok_or(ControlError::Unavailable("QEMU input unavailable"))?;
        let input = match source {
            InputSource::Main => &inputs.main,
            InputSource::Panel => &inputs.panel,
        };
        input
            .down(target, duration_ms)
            .map_err(|error| ControlError::Rejected(error.to_string()))
    }
    /// Enqueue a raw release on the explicitly selected channel.
    pub fn raw_up(&self, source: InputSource, target: u32) -> Result<u32, ControlError> {
        let inputs = self
            .engine
            .inputs
            .as_ref()
            .ok_or(ControlError::Unavailable("QEMU input unavailable"))?;
        let input = match source {
            InputSource::Main => &inputs.main,
            InputSource::Panel => &inputs.panel,
        };
        input
            .up(target)
            .map_err(|error| ControlError::Rejected(error.to_string()))
    }
    /// Enqueue a zero-based encoder index on the panel channel, including
    /// unsupported indices for protocol rejection diagnostics.
    pub fn raw_panel_encoder(&self, index: u32, steps: i32) -> Result<u32, ControlError> {
        let inputs = self
            .engine
            .inputs
            .as_ref()
            .ok_or(ControlError::Unavailable("QEMU input unavailable"))?;
        inputs
            .panel
            .encoder(index, steps)
            .map_err(|error| ControlError::Rejected(error.to_string()))
    }

    /// Enqueue a held button without waiting for guest acknowledgement.
    pub fn down(&self, target: u32, duration_ms: u32) -> Result<ButtonReceipt, ControlError> {
        if target >= l6max_host::controls::INPUT_COUNT as u32 || duration_ms > 60_000 {
            return Err(ControlError::Rejected("invalid held button".into()));
        }
        let sequence = self
            .engine
            .inputs
            .as_ref()
            .ok_or(ControlError::Unavailable("QEMU input unavailable"))?
            .down(target, duration_ms)
            .map_err(|e| ControlError::Rejected(e.to_string()))?;
        Ok(ButtonReceipt {
            target,
            command: "down".into(),
            sequence,
        })
    }
    pub fn up(&self, target: u32) -> Result<ButtonReceipt, ControlError> {
        if target >= l6max_host::controls::INPUT_COUNT as u32 {
            return Err(ControlError::Rejected("invalid held button".into()));
        }
        let sequence = self
            .engine
            .inputs
            .as_ref()
            .ok_or(ControlError::Unavailable("QEMU input unavailable"))?
            .up(target)
            .map_err(|e| ControlError::Rejected(e.to_string()))?;
        Ok(ButtonReceipt {
            target,
            command: "up".into(),
            sequence,
        })
    }

    pub fn sound_pad_reader_before(
        &self,
        deadline: std::time::Instant,
    ) -> Result<l6max_host::sound_pad::Reader, ControlError> {
        l6max_host::sound_pad::Reader::new_before(self.main_qmp()?, self.engine.model(), deadline)
            .map_err(ControlError::Rejected)
    }

    pub fn panel_key(&self, code: u32) -> Result<PanelKeySnapshot, ControlError> {
        l6max_host::panel_keys::snapshot(self.main_qmp()?, self.engine.model(), code)
            .map_err(ControlError::Rejected)
    }

    pub fn midi_config(&self) -> Result<MidiConfig, ControlError> {
        l6max_host::midi_config::config(self.main_qmp()?, self.engine.model())
            .map_err(ControlError::Rejected)
    }

    pub fn mixer_level(&self) -> Result<MixerLevel, ControlError> {
        l6max_host::mixer_level::first_channel(self.main_qmp()?, self.engine.model())
            .map_err(ControlError::Rejected)
    }

    pub fn mixer_level_before(
        &self,
        deadline: std::time::Instant,
    ) -> Result<MixerLevel, ControlError> {
        l6max_host::mixer_level::first_channel_before(
            self.main_qmp()?,
            self.engine.model(),
            deadline,
        )
        .map_err(ControlError::Rejected)
    }

    pub fn mixer_level_cancellable(
        &self,
        deadline: std::time::Instant,
        cancelled: impl Fn() -> bool,
    ) -> Result<MixerLevel, ControlError> {
        l6max_host::mixer_level::first_channel_before_cancellable(
            self.main_qmp()?,
            self.engine.model(),
            deadline,
            cancelled,
        )
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
        let sequence =
            control::enqueue_tap(inputs, target, hold_ms).map_err(ControlError::Rejected)?;
        Ok(ButtonReceipt {
            target,
            command: "tap".into(),
            sequence,
        })
    }

    /// Enqueue a button down and return its input sequence for acknowledgement.
    pub fn down_with_sequence(&self, target: u32, duration_ms: u32) -> Result<u32, ControlError> {
        if l6max_host::input::button_source(target).is_none()
            || duration_ms == 0
            || duration_ms > 60_000
        {
            return Err(ControlError::Rejected(
                "invalid button down target or duration".into(),
            ));
        }
        self.engine
            .inputs
            .as_ref()
            .ok_or(ControlError::Unavailable("QEMU input unavailable"))?
            .down(target, duration_ms)
            .map_err(|error| ControlError::Rejected(error.to_string()))
    }

    /// Enqueue a button release and return its input sequence for acknowledgement.
    pub fn up_with_sequence(&self, target: u32) -> Result<u32, ControlError> {
        if l6max_host::input::button_source(target).is_none() {
            return Err(ControlError::Rejected(
                "invalid button release target".into(),
            ));
        }
        self.engine
            .inputs
            .as_ref()
            .ok_or(ControlError::Unavailable("QEMU input unavailable"))?
            .up(target)
            .map_err(|error| ControlError::Rejected(error.to_string()))
    }

    /// Verify the GUI and parameter getter revisions for one bounded observation.
    pub fn knob_parameter_reader(
        &self,
    ) -> Result<l6max_host::knob_popup::ParameterReader<'_>, ControlError> {
        l6max_host::knob_popup::parameter_reader(self.main_qmp()?, self.engine.model())
            .map_err(ControlError::Rejected)
    }

    /// Verify the GUI revision once for a bounded overlay transaction.
    /// Recreate this reader after a guest restart or firmware change.
    pub fn knob_overlay_reader(
        &self,
    ) -> Result<l6max_host::knob_popup::OverlayReader, ControlError> {
        l6max_host::knob_popup::overlay_reader(self.main_qmp()?, self.engine.model())
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

    /// Guard the kernel revision once for a bounded current-task observation.
    /// Recreate this reader after guest restart or a firmware change.
    pub fn current_task_reader(&self) -> Result<freertos::CurrentTaskReader, ControlError> {
        freertos::current_task_reader(self.main_qmp()?, self.engine.model())
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
