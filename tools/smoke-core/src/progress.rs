//! Versioned, append-only live progress events shared by the driver and runners.
use serde::{Deserialize, Serialize};
use std::{
    fs::{File, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
};

pub const VERSION: u32 = 6;
pub const PATH_ENV: &str = "L6_TEST_PROGRESS";
pub const RUN_ENV: &str = "L6_TEST_PROGRESS_RUN";
pub const ATTEMPT_ENV: &str = "L6_TEST_PROGRESS_ATTEMPT";

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct CaseId {
    pub case: String,
    pub model: l6fw::Model,
    pub backend: crate::Backend,
}
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct AttemptId {
    pub case: CaseId,
    pub attempt: u32,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Outcome {
    Pass,
    Fail,
    Flaky,
    Blocked,
    Xfail,
    Xpass,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    PreparingFixture,
    Executing,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunPhase {
    Building,
    Staging,
    Executing,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum SuiteEvent {
    Header {
        version: u32,
        run_id: String,
    },
    RunStarted {
        total_cases: usize,
    },
    RunPhaseChanged {
        phase: RunPhase,
        started_unix_ms: u64,
    },
    AttemptStarted {
        id: AttemptId,
        progress_path: PathBuf,
    },
    AttemptPhaseChanged {
        id: AttemptId,
        phase: Phase,
    },
    AttemptFinished {
        id: AttemptId,
        outcome: Outcome,
    },
    CaseFinished {
        id: CaseId,
        outcome: Outcome,
    },
    RunFinished {
        outcome: Outcome,
    },
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CasePhase {
    StartingEmulator,
    PreparingScenario,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum CaseEvent {
    Header {
        version: u32,
        run_id: String,
        id: AttemptId,
    },
    PhaseChanged {
        phase: CasePhase,
        started_unix_ms: u64,
    },
    ScenarioStarted {
        total_steps: usize,
    },
    StepStarted {
        index: usize,
        label: String,
        started_unix_ms: u64,
    },
    StepProgress {
        index: usize,
        completed: u32,
        total: u32,
    },
    ScenarioFinished {
        outcome: Outcome,
        finished_unix_ms: u64,
    },
}

/// Wall-clock time for replay across processes and late watcher attachment.
pub fn unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

pub struct Writer {
    file: File,
}
impl Writer {
    pub fn create(path: &Path) -> io::Result<Self> {
        Ok(Self {
            file: OpenOptions::new().write(true).create_new(true).open(path)?,
        })
    }
    pub fn emit<T: Serialize>(&mut self, event: &T) -> io::Result<()> {
        let mut line = serde_json::to_vec(event).map_err(io::Error::other)?;
        line.push(b'\n');
        self.file.write_all(&line)?;
        self.file.flush()
    }
}

/// A case process owns its stream; clones serialize writes to the same file.
#[derive(Clone, Default)]
pub struct CaseProgress(Option<std::sync::Arc<std::sync::Mutex<Writer>>>);
impl CaseProgress {
    pub fn from_env() -> Self {
        let Some(path) = std::env::var_os(PATH_ENV) else {
            return Self::default();
        };
        let result = (|| -> Result<Writer, String> {
            let run_id = std::env::var(RUN_ENV).map_err(|e| e.to_string())?;
            let id = serde_json::from_str(&std::env::var(ATTEMPT_ENV).map_err(|e| e.to_string())?)
                .map_err(|e| e.to_string())?;
            let mut writer = Writer::create(Path::new(&path)).map_err(|e| e.to_string())?;
            writer
                .emit(&CaseEvent::Header {
                    version: VERSION,
                    run_id,
                    id,
                })
                .map_err(|e| e.to_string())?;
            Ok(writer)
        })();
        match result {
            Ok(writer) => Self(Some(std::sync::Arc::new(std::sync::Mutex::new(writer)))),
            Err(error) => {
                tracing::warn!(%error, "could not initialize scenario progress stream");
                Self::default()
            }
        }
    }
    pub fn emit(&self, event: CaseEvent) {
        if let Some(writer) = &self.0
            && let Ok(mut writer) = writer.lock()
            && let Err(error) = writer.emit(&event)
        {
            tracing::warn!(%error, "could not write scenario progress");
        }
    }
}

/// Describe only the parameters relevant to identifying the current work.
pub fn step_label(action: &crate::Action) -> String {
    use crate::Action;
    match action {
        Action::EffectPacedTurn { channel, steps } => {
            format!("Apply {steps} paced effect edits on channel {channel}")
        }
        Action::UsbSysexBegin => "Begin USB SysEx campaign".into(),
        Action::UsbSysexExchange { request, .. } => format!(
            "USB SysEx request {}",
            request
                .bytes
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<Vec<_>>()
                .join(" ")
        ),
        Action::UsbSysexEnd => "End USB SysEx campaign".into(),
        Action::Wait { until } => format!("Wait for {}", until.replace('-', " ")),
        Action::MidiProbeBegin => "Connect MIDI probe sessions".into(),
        Action::MidiProbeSendCc {
            port,
            controller,
            value,
            ..
        } => format!("Send {port:?} MIDI CC {controller} value {value}"),
        Action::MidiProbeExpectRing {
            channel,
            changed,
            guest_ms,
            ..
        } => {
            if *changed {
                format!("Wait for channel {channel} ring change")
            } else {
                format!(
                    "Observe channel {channel} ring unchanged for {} ms guest time",
                    guest_ms.unwrap_or(0)
                )
            }
        }
        Action::MidiProbeWaitRingStable {
            channel, guest_ms, ..
        } => format!("Wait for channel {channel} ring stable for {guest_ms} ms guest time"),
        Action::MidiProbeDrainAndTap {
            port,
            target,
            hold_ms,
        } => {
            format!("Drain {port:?} MIDI and tap button {target} ({hold_ms} ms)")
        }
        Action::MidiProbeExpectNoEvents { port, wall_ms } => {
            format!("Observe {port:?} MIDI silence for {wall_ms} ms wall time")
        }
        Action::MidiProbeEnd => "Close MIDI probe sessions".into(),
        Action::ExpectDisplayAfterPress => "Wait for display change".into(),
        Action::Press { control } => format!("Press {}", control.replace(['-', '.'], " ")),
        Action::Hold { control, .. } => format!("Hold {}", control.replace(['-', '.'], " ")),
        Action::HoldChord {
            until: Some(crate::scenario::ChordCondition::FactoryMode),
            ..
        } => "Hold chord until factory mode".into(),
        Action::HoldChord {
            duration_ms: Some(ms),
            ..
        } => format!("Hold chord for {ms} ms guest time"),
        Action::KnobDialogModeBegin { scene } => {
            format!("Check knob dialog {} scene", scene.replace('-', " "))
        }
        Action::EnqueueTap { target, hold_ms } => {
            format!("Enqueue button {target} tap ({hold_ms} ms)")
        }
        Action::WaitInputApplied => "Wait for input applied".into(),
        Action::WaitKnobParameterMode { mode } => format!("Wait for knob parameter mode {mode}"),
        Action::CaptureDisplay { guest_ms, artifact } => {
            format!("Capture {artifact} after {guest_ms} ms guest time")
        }
        Action::ExpectCapturedDisplayImage { image } => format!(
            "Check captured {} screen",
            image
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("expected")
        ),
        Action::KnobDialogExpectHeap => "Check persistent heap counters".into(),
        Action::KnobDialogModeEnd => "Finish knob dialog mode".into(),
        Action::KnobDialogFinalBegin => "Check Pan sweep and channel 2 level".into(),
        Action::KnobDialogInjectInvalidTag { tag } => {
            if tag & 0x0f >= 10 {
                format!("Inject invalid mode {}", tag & 0x0f)
            } else {
                format!("Inject invalid channel {}", tag >> 4)
            }
        }
        Action::KnobDialogWaitInvalidTag => "Wait for invalid tag discard".into(),
        Action::KnobDialogConfirmInvalidTag => "Confirm invalid tag discard".into(),
        Action::KnobDialogSettleLevel { artifact } => {
            format!("Settle channel 1 level ({artifact})")
        }
        Action::KnobDialogInjectInvalidSignature => "Inject invalid state signature".into(),
        Action::KnobDialogObserveInvalidSignature { guest_ms } => {
            format!("Observe invalid signature for {guest_ms} ms guest time")
        }
        Action::KnobDialogSampleInvalidSignature => "Sample invalid signature rejection".into(),
        Action::KnobDialogSnapshotRestoration => "Capture restored dialog resources".into(),
        Action::KnobDialogCheckRestoration { field } => format!("Check restored {field:?}"),
        Action::KnobDialogFinishRestoration => "Assert dialog resources restored".into(),
        Action::KnobDialogExpectRestoredHeap => "Check restored dialog heap".into(),
        Action::KnobDialogWaitExpiry { .. } => "Wait for dialog inactivity expiry".into(),
        Action::KnobDialogExpectExpiryFrame { after } => {
            if *after {
                "Check expired frame differs from dialog".into()
            } else {
                "Check expired frame differs from notification".into()
            }
        }
        Action::KnobDialogPrepareNotification => {
            "Check dialog text and save notification baseline".into()
        }
        Action::KnobDialogArmNotificationCallback => "Arm notification callback breakpoint".into(),
        Action::KnobDialogWaitNotificationCallback => "Wait for notification callback".into(),
        Action::KnobDialogInvokeNotification { message } => {
            format!("Invoke stock notification {message}")
        }
        Action::KnobDialogWaitNotificationFrame { clear, .. } => {
            if *clear {
                "Wait for notification to clear".into()
            } else {
                "Wait for notification frame".into()
            }
        }
        Action::KnobDialogExpectNotificationInvariants { clear } => {
            if *clear {
                "Check scene after notification expiry".into()
            } else {
                "Check scene during notification".into()
            }
        }
        Action::KnobDialogRestoreInvalidSignature => {
            "Restore state and check signature rejection".into()
        }
        Action::SelectMode { mode } => format!("Select {} mode", mode.replace('-', " ")),
        Action::ExpectMode { mode } => format!("Check {} mode", mode.replace('-', " ")),
        Action::ExpectRing { channel, .. } => format!("Check channel {channel} ring"),
        Action::ExpectMute { channel } => format!("Check channel {channel} mute"),
        Action::WaitEffectMode { .. } => "Wait for effect editor state".into(),
        Action::WaitRecordedFiles { .. } => "Wait for recorded files".into(),
        Action::WaitAudioInput {
            playing, min_ms, ..
        } => format!(
            "Wait for audio input {} at {min_ms} ms",
            if *playing { "playing" } else { "paused" }
        ),
        Action::WaitMemory { symbol, .. } => format!("Wait for {symbol}"),
        Action::WaitL6SoundPadFile { path } => format!("Wait for sound-pad file {path}"),
        Action::ExpectDisplayImage { image, .. } => format!(
            "Wait for {} screen",
            image
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("expected")
                .replace('-', " ")
        ),
        Action::EnqueueEncoder { channel, steps, .. } => {
            format!("Enqueue channel {channel} encoder ({steps} steps)")
        }
        Action::HostMidiExpectCc {
            channel,
            controller,
        } => format!("Wait for MIDI CC {controller} on channel {channel}"),
        Action::HostMidiSendCc {
            channel,
            controller,
            value,
        } => format!("Send MIDI CC {controller}={value} on channel {channel}"),
        Action::HostMidiDrain { quiet_wall_ms } => {
            format!("Drain MIDI until {quiet_wall_ms} ms wall quiet")
        }
        Action::HostMidiExpectCount { count } => format!("Wait for {count} MIDI messages"),
        Action::HostMidiSettle { guest_ms } => format!("Settle MIDI for {guest_ms} ms guest time"),
        Action::ExpectRingChanged { channel, .. } => {
            format!("Wait for channel {channel} ring change")
        }
        Action::Encoder { channel, steps, .. } => {
            format!("Turn channel {channel} encoder ({steps} steps)")
        }
        Action::Delay { guest_ms } => format!("Wait {guest_ms} ms guest time"),
        Action::ObserveDuration { activity, guest_ms } => {
            let activity = match activity {
                crate::scenario::ObservationActivity::Recording => "recording",
                crate::scenario::ObservationActivity::Playback => "playback",
                crate::scenario::ObservationActivity::AudioOutput => "audio output",
            };
            format!("Observe {activity} for {guest_ms} ms guest time")
        }
        _ => serde_json::to_value(action)
            .ok()
            .and_then(|v| {
                v.get("action")
                    .and_then(serde_json::Value::as_str)
                    .map(|s| s.replace('-', " "))
            })
            .unwrap_or_else(|| "scenario step".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_include_meaningful_action_parameters() {
        assert_eq!(
            step_label(&crate::Action::Wait {
                until: "mixer-ready".into()
            }),
            "Wait for mixer ready"
        );
        assert_eq!(
            step_label(&crate::Action::Press {
                control: "efx-select".into()
            }),
            "Press efx select"
        );
        assert_eq!(
            step_label(&crate::Action::Press {
                control: "channel.1.mute".into()
            }),
            "Press channel 1 mute"
        );
        assert_eq!(
            step_label(&crate::Action::SelectMode {
                mode: "level".into()
            }),
            "Select level mode"
        );
        assert_eq!(
            step_label(&crate::Action::Delay { guest_ms: 1500 }),
            "Wait 1500 ms guest time"
        );
        assert_eq!(
            step_label(&crate::Action::ObserveDuration {
                activity: crate::scenario::ObservationActivity::Recording,
                guest_ms: 10000,
            }),
            "Observe recording for 10000 ms guest time"
        );
        assert_eq!(
            step_label(&crate::Action::ExpectDisplayImage {
                image: "tests/smoke/images/menu-sound-pad.png".into(),
                settle_ms: 500,
            }),
            "Wait for menu sound pad screen"
        );
    }
}
