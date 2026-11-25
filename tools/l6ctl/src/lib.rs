//! Typed control of an emulator owned in process or exposed by its control socket.
//!
//! The socket interface is deliberately narrower than direct access. In
//! particular its input replies confirm enqueueing, not guest acknowledgement.
pub use l6max_host::control::Info;
pub use l6max_host::knob_popup::{ParameterState, RecorderState, Snapshot as KnobPopupSnapshot};
use l6max_host::{control, engine::Engine, freertos, qmp};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::Value;
use std::{
    fmt,
    io::{self, BufRead, BufReader, Write},
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

    pub fn socket(&self) -> &Path {
        &self.socket
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
        let timeout = if matches!(
            request,
            Request::Rtos { .. }
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

    pub fn info(&self) -> Result<Info, ControlError> {
        let inputs = self
            .engine
            .inputs
            .as_ref()
            .ok_or(ControlError::Unavailable("QEMU input unavailable"))?;
        Ok(control::info(inputs, self.engine.model()))
    }

    fn main_qmp(&self) -> Result<&qmp::QmpClient, ControlError> {
        self.engine
            .management
            .first()
            .ok_or(ControlError::Unavailable("QEMU management unavailable"))
    }

    /// Enqueue a one-based encoder input, without waiting for guest acknowledgment.
    pub fn encoder(&self, channel: u32, steps: i32) -> Result<AxisReceipt, ControlError> {
        let inputs = self
            .engine
            .inputs
            .as_ref()
            .ok_or(ControlError::Unavailable("QEMU input unavailable"))?;
        control::enqueue_encoder(inputs, self.engine.model(), channel, steps)
            .map_err(ControlError::Rejected)?;
        Ok(AxisReceipt { channel, steps })
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
