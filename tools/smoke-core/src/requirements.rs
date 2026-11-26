//! What each action needs to run: the models it supports, the firmware it
//! reads, whether only the headless runner can drive it, and whether it must
//! be the scenario's first step. [`of`] lists every action, so a new action
//! does not compile until its requirement is written here. Scenario
//! validation checks the model scope, backends and position when a scenario
//! loads;
//! [`check_firmware`] checks the booted firmware before the guest starts, so
//! an unmet requirement is reported before anything runs.
use crate::firmware::FirmwareIdentity;
use crate::scenario::{Action, Backend, Scenario};
use l6fw::Model;

/// The firmware an action can run on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Firmware {
    /// The supported stock MAIN image, or an image our patch tools built from
    /// it and reported. Every scenario needs at least this.
    Supported,
    /// Exactly the supported stock MAIN image: the action reads addresses or
    /// behavior a patch could change.
    Stock,
}

/// What an action needs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Requirement {
    /// Models the action supports; empty means every model.
    pub models: &'static [Model],
    pub firmware: Firmware,
    /// The action drives devices only the headless runner emulates.
    pub headless: bool,
    /// The action must be the scenario's first step.
    pub first: bool,
}

const ANY: Requirement = Requirement {
    models: &[],
    firmware: Firmware::Supported,
    headless: false,
    first: false,
};
const HEADLESS: Requirement = Requirement {
    headless: true,
    ..ANY
};
const L6: &[Model] = &[Model::L6];
const L6MAX: &[Model] = &[Model::L6max];

const fn on(models: &'static [Model]) -> Requirement {
    Requirement { models, ..HEADLESS }
}
const fn stock_on(models: &'static [Model]) -> Requirement {
    Requirement {
        models,
        firmware: Firmware::Stock,
        ..HEADLESS
    }
}

/// The requirement of `action`.
pub fn of(action: &Action) -> Requirement {
    match action {
        Action::AudioCallbacks => Requirement {
            first: true,
            ..stock_on(&[])
        },
        Action::HoldChord {
            until: Some(crate::scenario::ChordCondition::FactoryMode),
            ..
        } => stock_on(L6MAX),
        Action::ExpectSettings { .. } | Action::ExpectRecorder => stock_on(L6MAX),
        Action::L6maxChannelSignalPwm | Action::L6maxTapHandler => stock_on(L6MAX),
        Action::WaitL6SoundPadFile { .. }
        | Action::L6LevelMeterPwm
        | Action::L6DinSweep
        | Action::PanelEvent { .. }
        | Action::L6MainGpioEvent { .. }
        | Action::L6DinEventDispatch { .. } => stock_on(L6),
        Action::L6UsbStartup => Requirement {
            first: true,
            ..stock_on(L6)
        },
        Action::DinUsbBegin
        | Action::DinUsbAttach
        | Action::DinUsbSend { .. }
        | Action::DinUsbExpect { .. }
        | Action::DinUsbEnd
        | Action::L6SweepBegin
        | Action::L6SweepRejectMain { .. }
        | Action::L6SweepPanelBaseline { .. }
        | Action::L6SweepPanelTap
        | Action::L6SweepPanelWait
        | Action::L6SweepPanelTrace
        | Action::L6SweepPanelAssert
        | Action::L6SweepEncoderBaseline
        | Action::L6SweepEncoderSend { .. }
        | Action::L6SweepEncoderWait
        | Action::L6SweepEncoderTrace
        | Action::L6SweepEncoderAssert
        | Action::L6SweepRejectPanel
        | Action::L6SweepRejectEncoder
        | Action::L6SweepEnd => on(L6),
        Action::L6maxRecorderPlayback { .. }
        | Action::L6maxInputTiming
        | Action::L6maxPanelKeyBegin
        | Action::L6maxPanelKeyCheck { .. }
        | Action::L6maxPanelKeyInput { .. }
        | Action::L6maxPanelKeyEnd
        | Action::MidiProbeBegin
        | Action::MidiProbeSendCc { .. }
        | Action::MidiProbeExpectRing { .. }
        | Action::MidiProbeWaitRingStable { .. }
        | Action::MidiProbeDrainAndTap { .. }
        | Action::MidiProbeExpectNoEvents { .. }
        | Action::MidiProbeEnd
        | Action::KnobDialogReady
        | Action::KnobDialogGuard
        | Action::KnobDialogBaseline
        | Action::KnobDialogPreflightBegin
        | Action::KnobProbePrepare { .. }
        | Action::KnobProbeInject
        | Action::KnobProbeTrigger { .. }
        | Action::KnobProbeSample
        | Action::KnobProbeRestore
        | Action::KnobProbeAssert
        | Action::KnobRaceBegin
        | Action::KnobRaceWaitRegistration
        | Action::KnobRaceArmCallback
        | Action::KnobRaceWaitCallback
        | Action::KnobRaceSample
        | Action::KnobRaceRestore
        | Action::KnobRaceAssert
        | Action::KnobRaceCapture { .. }
        | Action::KnobDialogFirstActivation
        | Action::KnobActiveBegin
        | Action::KnobActiveSampleCallbacks
        | Action::KnobActiveCheckCallbacks
        | Action::KnobActiveCheckFrame
        | Action::KnobActiveFinish
        | Action::KnobLevelBegin
        | Action::KnobLevelStart { .. }
        | Action::KnobLevelSweep { .. }
        | Action::KnobLevelExpectZero
        | Action::KnobLevelFinish
        | Action::KnobTaskBegin
        | Action::KnobTaskArmHook
        | Action::KnobTaskWaitHook
        | Action::KnobTaskArmCallback
        | Action::KnobTaskWaitCallback
        | Action::KnobTaskAssertDistinct
        | Action::KnobTaskRelease
        | Action::KnobTaskFinish
        | Action::KnobOverlayBegin
        | Action::KnobOverlayCheckWindow
        | Action::KnobOverlaySnapshotDescriptors
        | Action::KnobOverlayCheckDescriptor { .. }
        | Action::KnobOverlayFinishDescriptors
        | Action::KnobOverlayCheckResource
        | Action::KnobOverlayCheckCorners
        | Action::KnobOverlayCheckSignature
        | Action::KnobOverlayCheckBitmap
        | Action::KnobOverlayCheckHeap
        | Action::KnobOverlayFinish
        | Action::KnobBitmapBegin
        | Action::KnobBitmapCheckBackground { .. }
        | Action::KnobBitmapStoreBefore
        | Action::KnobBitmapArm
        | Action::KnobBitmapWait
        | Action::KnobBitmapInvoke { .. }
        | Action::KnobBitmapCheckFrame
        | Action::KnobBitmapCheckSignature
        | Action::KnobBitmapCheckDescriptor
        | Action::KnobBitmapFinish
        | Action::KnobOverlayReactivate { .. }
        | Action::KnobOverlayWaitActive
        | Action::KnobOverlayRefreshBegin
        | Action::KnobOverlayRefreshValue
        | Action::KnobOverlayRefreshPixels
        | Action::KnobOverlayRefreshFinish
        | Action::KnobLockAcquire
        | Action::KnobLockTrigger { .. }
        | Action::KnobLockWait
        | Action::KnobLockSample
        | Action::KnobLockRestore
        | Action::KnobLockAssert
        | Action::KnobLockFinish
        | Action::KnobDialogModeBegin { .. }
        | Action::WaitKnobParameterMode { .. }
        | Action::WaitKnobDisplayImage { .. }
        | Action::KnobDialogExpectHeap
        | Action::KnobDialogModeEnd
        | Action::KnobDialogAdaptive { .. }
        | Action::KnobDialogInjectInvalidTag { .. }
        | Action::KnobDialogWaitInvalidTag
        | Action::KnobDialogConfirmInvalidTag
        | Action::KnobDialogSettleLevel { .. }
        | Action::KnobDialogInjectInvalidSignature
        | Action::KnobDialogObserveInvalidSignature { .. }
        | Action::KnobDialogSampleInvalidSignature
        | Action::KnobDialogRestoreInvalidSignature
        | Action::KnobDialogPrepareNotification
        | Action::KnobDialogArmNotificationCallback
        | Action::KnobDialogWaitNotificationCallback
        | Action::KnobDialogInvokeNotification { .. }
        | Action::KnobDialogWaitNotificationFrame { .. }
        | Action::KnobDialogExpectNotificationInvariants { .. }
        | Action::KnobDialogWaitExpiry { .. }
        | Action::KnobDialogExpectExpiryFrame { .. }
        | Action::KnobDialogSnapshotRestoration
        | Action::KnobDialogCheckRestoration { .. }
        | Action::KnobDialogFinishRestoration
        | Action::KnobDialogExpectRestoredHeap
        | Action::EnqueueEncoderNoAck { .. }
        | Action::EnqueueTapNoAck { .. }
        | Action::ReleaseCapturedDisplay
        | Action::KnobDialogSnapshotNavigation
        | Action::KnobDialogCheckNavigation { .. }
        | Action::CaptureDisplay { .. }
        | Action::ExpectCapturedDisplayImage { .. }
        | Action::KnobDialog
        | Action::KnobPopupOpen { .. }
        | Action::KnobPopupExpectOpen
        | Action::KnobPopupExpectValue
        | Action::ExpectMenu { .. }
        | Action::WaitEffectMode { .. } => on(L6MAX),
        Action::ExpectDisplayAfterPress => Requirement {
            models: L6MAX,
            ..ANY
        },
        Action::ExpectUpdateState { .. }
        | Action::ExpectLiveUpdateMarkerAbsent
        | Action::ExpectUpdateGui { .. }
        | Action::WaitUpdatePanelReady { .. }
        | Action::WaitUsbController
        | Action::BeginWallDeadline { .. }
        | Action::EndWallDeadline
        | Action::ExpectUpdateMode { .. }
        | Action::ExpectMainMemory { .. }
        | Action::ExpectUpdateComparison { .. }
        | Action::CaptureInstalledPanel
        | Action::ExpectPanelUnchanged => on(L6MAX),
        Action::WaitPanelUptime { .. }
        | Action::LifecycleStart { .. }
        | Action::LifecycleStop
        | Action::CaptureUpdateMarkerPending
        | Action::ExpectInstallerOutcome { .. }
        | Action::ExpectUpdateMarkerAbsent => ANY,
        Action::L6MidiProbeBegin
        | Action::L6MidiProbeInject
        | Action::L6MidiProbeExpectHook
        | Action::L6MidiProbeExpectReplies
        | Action::MarkL6UpdatePending
        | Action::ExpectL6UpdateResetVector
        | Action::ExpectL6UpdateComparison { .. }
        | Action::L6IndicatorBegin
        | Action::L6RingSample { .. }
        | Action::L6RingCoverage { .. }
        | Action::L6MuteSample { .. }
        | Action::L6IndicatorEnd => on(L6),
        Action::SdCard { .. }
        | Action::UsbDescriptors
        | Action::UsbStorage
        | Action::UsbMidiOpen
        | Action::UsbMidiBegin
        | Action::UsbMidiExpectEncoder
        | Action::UsbMidiPrepareLevel { .. }
        | Action::UsbMidiSetAlternate
        | Action::UsbMidiSetTarget
        | Action::UsbMidiEnd
        | Action::HostMidiRoundTrip
        | Action::HostMidiConnect
        | Action::HostMidiBegin
        | Action::HostMidiSyncBegin
        | Action::HostMidiExpectSync
        | Action::HostMidiDrain { .. }
        | Action::HostMidiExpectCount { .. }
        | Action::HostMidiEnd
        | Action::EnqueueEncoder { .. }
        | Action::EnqueueTap { .. }
        | Action::WaitInputApplied
        | Action::HostMidiExpectCc { .. }
        | Action::HostMidiSettle { .. }
        | Action::HostMidiSendCc { .. }
        | Action::ExpectRingChanged { .. }
        | Action::DirectUsbMidiRoundTrip { .. }
        | Action::UsbSysexBegin
        | Action::UsbSysexExchange { .. }
        | Action::UsbSysexEnd
        | Action::EditorAssignBegin
        | Action::EditorAssignIdentity
        | Action::EditorAssignGlobal
        | Action::EditorAssignVersion
        | Action::EditorAssignSdInfo
        | Action::EditorAssignRefresh
        | Action::EditorAssignCount { .. }
        | Action::EditorAssignFilename { .. }
        | Action::EditorAssignSend { .. }
        | Action::EditorAssignState { .. }
        | Action::EditorAssignEnd
        | Action::EditorPlaybackBegin { .. }
        | Action::EditorPlaybackPlay
        | Action::EditorPlaybackWait
        | Action::EditorPlaybackObserve
        | Action::EditorPlaybackSample
        | Action::EditorPlaybackAssert
        | Action::EditorPlaybackEnd
        | Action::UiPadBegin { .. }
        | Action::UiPadIdentity
        | Action::UiPadGlobal
        | Action::UiPadKeepAlive
        | Action::UiPadQuery
        | Action::UiPadAssert
        | Action::UiPadEnd
        | Action::SoundPadBegin { .. }
        | Action::SoundPadPress
        | Action::SoundPadHold
        | Action::SoundPadRelease
        | Action::SoundPadObserve
        | Action::SoundPadSample
        | Action::SoundPadAssertWaveform
        | Action::SoundPadAssertAudio
        | Action::SoundPadAssertLed
        | Action::SoundPadAssertMeter
        | Action::SoundPadFinish
        | Action::ExpectMemory { .. }
        | Action::WaitMemory { .. }
        | Action::ExpectSdLoader
        | Action::ExpectSdModule { .. }
        | Action::EffectPacedTurn { .. }
        | Action::EffectParameterEdit { .. }
        | Action::EffectReplacementState { .. }
        | Action::ExpectHeap { .. }
        | Action::HoldChord { .. }
        | Action::FirmwareUpdate
        | Action::WarmReset
        | Action::Restart
        | Action::PowerPress { .. }
        | Action::ExpectCardEntry { .. }
        | Action::AudioInputTransport { .. }
        | Action::AudioInputLoad { .. }
        | Action::ExpectAudioInput { .. }
        | Action::WaitAudioInput { .. }
        | Action::ExpectOutputTone { .. }
        | Action::ExpectRecordedTone { .. }
        | Action::WaitRecordedFiles { .. } => HEADLESS,
        // Panel input and observation every backend can drive.
        Action::Include { .. }
        | Action::Checkpoint { .. }
        | Action::ExpectDisplay { .. }
        | Action::ExpectDisplayImage { .. }
        | Action::ExpectTrace { .. }
        | Action::ExpectTraceCount { .. }
        | Action::ExpectButtonReport { .. }
        | Action::ExpectStartupReport { .. }
        | Action::L6RecorderPlayback { .. }
        | Action::WasmPatch
        | Action::ExpectPowered { .. }
        | Action::Wait { .. }
        | Action::Delay { .. }
        | Action::ObserveDuration { .. }
        | Action::Press { .. }
        | Action::Hold { .. }
        | Action::SaveScene { .. }
        | Action::ExpectScene { .. }
        | Action::ExpectButtonLed { .. }
        | Action::ExpectEffect { .. }
        | Action::Switch { .. }
        | Action::Encoder { .. }
        | Action::Analog { .. }
        | Action::ExpectAnalog { .. }
        | Action::ExpectEncoderReports
        | Action::ExpectRing { .. }
        | Action::ExpectRingLevel { .. }
        | Action::ExpectMute { .. }
        | Action::SelectMode { .. }
        | Action::ExpectMode { .. } => ANY,
    }
}

/// Check each step's model scope and position.
pub fn check_scope(scenario: &Scenario) -> Result<(), String> {
    for (index, step) in scenario.step.iter().enumerate() {
        let requirement = of(&step.action);
        let name = action_name(&step.action);
        if !requirement.models.is_empty()
            && scenario
                .models
                .iter()
                .any(|&model| step.applies_to(model) && !requirement.models.contains(&model))
        {
            let names: Vec<_> = requirement
                .models
                .iter()
                .map(|model| model.name())
                .collect();
            return Err(format!("{name} supports only {}", names.join(", ")));
        }
        if requirement.headless && scenario.backends.iter().any(|b| *b != Backend::Headless) {
            return Err(format!("{name} requires backends=[\"headless\"]"));
        }
        if requirement.first && index != 0 {
            return Err(format!("{name} must be the scenario's first step"));
        }
    }
    Ok(())
}

/// Check that `firmware`, the image the case boots on `model`, satisfies every
/// step that applies to that model.
pub fn check_firmware(
    scenario: &Scenario,
    model: Model,
    firmware: &FirmwareIdentity,
) -> Result<(), String> {
    let mut needed = Firmware::Supported;
    let mut needed_by = None;
    for step in &scenario.step {
        if step.applies_to(model) && of(&step.action).firmware == Firmware::Stock {
            needed = Firmware::Stock;
            needed_by.get_or_insert_with(|| action_name(&step.action));
        }
    }
    match (needed, firmware) {
        (_, FirmwareIdentity::Stock { .. }) => Ok(()),
        (Firmware::Supported, FirmwareIdentity::Built { .. }) => Ok(()),
        (Firmware::Stock, FirmwareIdentity::Built { sha256 }) => Err(format!(
            "{} reads the stock firmware, but MAIN is a patched build ({sha256})",
            needed_by.unwrap_or_default()
        )),
        (_, FirmwareIdentity::Unknown { sha256 }) => Err(format!(
            "MAIN {sha256} is neither the supported stock image nor a build our patch tools reported"
        )),
    }
}

/// The action's name as scenarios write it.
fn action_name(action: &Action) -> String {
    toml::Value::try_from(action)
        .ok()
        .and_then(|value| value.get("action")?.as_str().map(str::to_owned))
        .unwrap_or_else(|| format!("{action:?}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scenario(models: &str, actions: &[&str]) -> Scenario {
        let steps: String = actions
            .iter()
            .map(|action| format!("[[step]]\naction='{action}'\n"))
            .collect();
        Scenario::from_text(&format!(
            "version=1\nname='t'\nmodels={models}\nbackends=['headless']\n{steps}"
        ))
        .unwrap()
    }

    #[test]
    fn stock_actions_need_the_stock_image_and_others_accept_reported_builds() {
        let stock = FirmwareIdentity::Stock {
            model: l6fw::Model::L6,
        };
        let built = FirmwareIdentity::Built {
            sha256: "b".repeat(64),
        };
        let unknown = FirmwareIdentity::Unknown {
            sha256: "c".repeat(64),
        };
        let reads_stock = scenario("['l6']", &["l6-din-sweep"]);
        let ordinary = scenario("['l6']", &["restart"]);
        assert!(check_firmware(&reads_stock, Model::L6, &stock).is_ok());
        assert!(
            check_firmware(&reads_stock, Model::L6, &built)
                .unwrap_err()
                .contains("l6-din-sweep")
        );
        assert!(check_firmware(&ordinary, Model::L6, &built).is_ok());
        for scenario in [&reads_stock, &ordinary] {
            assert!(check_firmware(scenario, Model::L6, &unknown).is_err());
        }
    }

    #[test]
    fn scope_and_position_come_from_the_table() {
        let error = Scenario::from_text(
            "version=1\nname='t'\nmodels=['l6max']\n[[step]]\naction='l6-din-sweep'\n",
        )
        .unwrap_err();
        assert!(error.contains("l6-din-sweep supports only l6"), "{error}");
        let late = Scenario::from_text(
            "version=1\nname='t'\nmodels=['l6']\nbackends=['headless']\n[[step]]\naction='delay'\nguest_ms=1\n[[step]]\naction='l6-usb-startup'\n",
        )
        .unwrap_err();
        assert!(late.contains("first step"), "{late}");
        let gui = Scenario::from_text(
            "version=1\nname='t'\nmodels=['l6max']\nbackends=['headless','gui']\n[[step]]\naction='usb-descriptors'\n",
        )
        .unwrap_err();
        assert!(gui.contains("usb-descriptors requires backends"), "{gui}");
        let undeclared = Scenario::from_text(
            "version=1\nname='t'\nmodels=['l6','l6max']\nbackends=['headless']\n[[step]]\naction='expect-memory'\nsymbol='system_mode'\nbytes=[0]\n",
        )
        .unwrap_err();
        assert!(
            undeclared.contains("system_mode is not declared in patches/api/l6"),
            "{undeclared}"
        );
    }
}
