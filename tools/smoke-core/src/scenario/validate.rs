//! Semantic checks a scenario passes when it loads.
use super::*;

impl Scenario {
    pub(super) fn validate(&self) -> Result<(), String> {
        let value = self;
        if value.version != 1 || value.name.is_empty() || value.step.is_empty() {
            return Err("scenario needs version=1, a name and nonempty steps".into());
        }
        validate_models(&value.models)?;
        crate::requirements::check_scope(value)?;
        validate_power_steps(value)?;
        validate_popup_steps(value)?;
        if value.step.iter().any(|step| {
            matches!(
                step.action,
                Action::PatchParityPrepare
                    | Action::PatchParityFirmware { .. }
                    | Action::PatchParityLoader { .. }
                    | Action::PatchParityUnsupported { .. }
                    | Action::PatchParityFinish
            )
        }) {
            validate_patch_parity(value)?;
        }
        if let Some(fixture) = value.update_fixture {
            let fixture_model = if fixture.is_l6() {
                Model::L6
            } else {
                Model::L6max
            };
            let expected_patches = match fixture {
                UpdateFixture::PatchedKnob => vec!["knob-dialog".to_string()],
                UpdateFixture::PatchedDin => vec!["din-mixer-control".to_string()],
                UpdateFixture::PatchedKnobMidi => {
                    vec!["knob-dialog".to_string(), "midi-sync".to_string()]
                }
                _ => vec!["midi-sync".to_string()],
            };
            if value.models != [fixture_model]
                || value.backends != [Backend::Headless]
                || value.state.is_some()
                || value.card.is_some()
                || (!fixture.is_patched() && !value.update_patches.is_empty())
                || (fixture.is_patched()
                    && (value.update_patches.len() != 1
                        || value.update_patches.get(&fixture_model).map(Vec::as_slice)
                            != Some(expected_patches.as_slice())))
                || !value.patches.is_empty()
                || value.sd_patches.is_some()
                || !value.installed_patches.is_empty()
                || !value.installed_sd_patches.is_empty()
                || !value.effect_replacements.is_empty()
                || (!matches!(
                    fixture,
                    UpdateFixture::PatchedMidi
                        | UpdateFixture::PatchedDin
                        | UpdateFixture::PatchedKnob
                        | UpdateFixture::PatchedKnobMidi
                ) && value
                    .step
                    .iter()
                    .any(|step| !step.models.is_empty() || step.setup_input))
            {
                return Err(
                    "managed update needs a dedicated model-scoped headless stock fixture".into(),
                );
            }
            let actions = value
                .step
                .iter()
                .map(|step| &step.action)
                .collect::<Vec<_>>();
            match fixture {
                UpdateFixture::L6Stock
                | UpdateFixture::L6Matching
                | UpdateFixture::L6PatchedMidi => {
                    if value.update_goal != Some(UpdateGoal::Install) {
                        return Err("L6 update requires the full install goal".into());
                    }
                    let prefix = if fixture == UpdateFixture::L6PatchedMidi {
                        &actions[..actions.len().saturating_sub(8)]
                    } else {
                        actions.as_slice()
                    };
                    if !matches!(
                        prefix,
                        [
                            Action::LifecycleStart {
                                profile: LifecycleBoot::Initial
                            },
                            Action::WaitPanelUptime { guest_ms: 40000.. },
                            Action::LifecycleStop,
                            Action::MarkL6UpdatePending,
                            Action::CaptureUpdateMarkerPending,
                            Action::LifecycleStart {
                                profile: LifecycleBoot::Installer
                            },
                            Action::ExpectInstallerOutcome {
                                expected: InstallerExpected::Installed
                            },
                            Action::ExpectL6UpdateResetVector,
                            Action::WaitPanelUptime { guest_ms: 40000.. },
                            Action::LifecycleStop,
                            Action::ExpectL6UpdateComparison {
                                field: L6UpdateField::MainFirmware
                            },
                            Action::ExpectL6UpdateComparison {
                                field: L6UpdateField::MainTrailer
                            },
                            Action::ExpectL6UpdateComparison {
                                field: L6UpdateField::SecondaryFirmware
                            },
                            Action::ExpectL6UpdateComparison {
                                field: L6UpdateField::SecondaryTrailer
                            },
                            Action::ExpectL6UpdateComparison {
                                field: L6UpdateField::PackageFooter
                            },
                            Action::ExpectL6UpdateComparison {
                                field: L6UpdateField::BootRegion
                            },
                            Action::ExpectL6UpdateComparison {
                                field: L6UpdateField::ProtectedRegion
                            },
                            Action::ExpectL6UpdateComparison {
                                field: L6UpdateField::ResourceNor
                            },
                            Action::ExpectUpdateMarkerAbsent,
                        ]
                    ) {
                        return Err("L6 stock update requires both complete boots, explicit marker, installed reset vector, every segment, resource and protected-region check".into());
                    }
                    if fixture == UpdateFixture::L6PatchedMidi
                        && !matches!(
                            actions.as_slice(),
                            [
                                ..,
                                Action::LifecycleStart {
                                    profile: LifecycleBoot::Verify
                                },
                                Action::Delay { guest_ms: 5000 },
                                Action::L6MidiProbeBegin,
                                Action::L6MidiProbeInject,
                                Action::L6MidiProbeExpectHook,
                                Action::L6MidiProbeExpectReplies,
                                Action::L6MidiProbeFinish,
                                Action::LifecycleStop
                            ]
                        )
                    {
                        return Err(
                            "patched L6 update requires complete terminal MIDI verification".into(),
                        );
                    }
                    if fixture == UpdateFixture::L6PatchedMidi {
                        let tail = &value.step[value.step.len() - 8..];
                        if tail[1].wall_timeout_s != 180
                            || tail[2..7].iter().any(|step| {
                                step.guest_timeout_ms != 120_000 || step.wall_timeout_s != 300
                            })
                        {
                            return Err(
                                "patched L6 verify needs the original readiness and probe budgets"
                                    .into(),
                            );
                        }
                    }
                }
                UpdateFixture::PatchedMidi
                | UpdateFixture::PatchedDin
                | UpdateFixture::PatchedKnob
                | UpdateFixture::PatchedKnobMidi => {
                    if value.update_goal != Some(UpdateGoal::Install) {
                        return Err("patched update requires the full install goal".into());
                    }
                    let semantic = actions
                        .into_iter()
                        .filter(|action| {
                            !matches!(action, Action::WaitInputApplied | Action::Delay { .. })
                        })
                        .collect::<Vec<_>>();
                    let tail_start = semantic
                        .iter()
                        .position(|action| matches!(action, Action::EndWallDeadline))
                        .map(|index| index + 1)
                        .ok_or("missing patched verify setup deadline")?;
                    let (common, tail) = semantic.split_at(tail_start);
                    if !matches!(common, [
                        Action::LifecycleStart { profile: LifecycleBoot::Initial },
                        Action::ExpectDisplayImage { image: update_image, .. },
                        Action::ExpectUpdateState { mode: 6, flags_mask: 9, flags_value: 0 },
                        Action::ExpectUpdateGui { window: 0x802003b4, progress_mask: 0, progress_value: 0 },
                        Action::EnqueueTap { target: 2, hold_ms: 50 },
                        Action::EnqueueTap { target: 3, hold_ms: 50 },
                        Action::ExpectUpdateGui { window: 0x8020029c, progress_mask: 0x100, progress_value: 0x100 },
                        Action::ExpectLiveUpdateMarkerAbsent,
                        Action::EnqueueTap { target: 53, hold_ms: 3500 },
                        Action::ExpectPowered { powered: false },
                        Action::LifecycleStop,
                        Action::CaptureUpdateMarkerPending,
                        Action::LifecycleStart { profile: LifecycleBoot::Installer },
                        Action::ExpectInstallerOutcome { expected: InstallerExpected::Installed },
                        Action::WaitUpdatePanelReady { require_programming: false },
                        Action::ExpectTrace { contains: install_trace, absent: install_absent },
                        Action::ExpectUpdateMode { mode: 4 },
                        Action::LifecycleStop,
                        Action::ExpectUpdateComparison { field: UpdateComparisonField::MainPayload },
                        Action::ExpectUpdateComparison { field: UpdateComparisonField::BootRegion },
                        Action::ExpectUpdateComparison { field: UpdateComparisonField::ProtectedRegion },
                        Action::ExpectUpdateComparison { field: UpdateComparisonField::PanelImage },
                        Action::ExpectUpdateMarkerAbsent,
                        Action::CaptureInstalledPanel,
                        Action::LifecycleStart { profile: LifecycleBoot::Restart },
                        Action::ExpectInstallerOutcome { expected: InstallerExpected::NoPending },
                        Action::WaitPanelUptime { guest_ms: 40000.. },
                        Action::ExpectTrace { contains: restart_contains, absent: restart_absent },
                        Action::LifecycleStop,
                        Action::ExpectPanelUnchanged,
                        Action::LifecycleStart { profile: LifecycleBoot::Verify },
                        Action::BeginWallDeadline { duration_s: 300 },
                        Action::Wait { until: first_boot },
                        Action::ExpectDisplayImage { image: date_image, .. },
                        Action::Press { control: first_confirm },
                        Action::ExpectDisplayImage { image: battery_image, .. },
                        Action::Press { control: second_confirm },
                        Action::ExpectDisplayImage { image: home_image, .. },
                        Action::Wait { until: home_boot },
                        Action::EndWallDeadline
                    ] if update_image == Path::new("tests/smoke/images/l6max-firmware-update-dialog.png")
                        && install_trace == &["l6 panel RTC response start"] && install_absent.is_empty()
                        && restart_contains.is_empty() && restart_absent == &["l6 panel ROM erase", "l6 panel ROM mass erase"]
                        && first_boot == "boot-ready" && home_boot == "boot-ready"
                        && date_image == Path::new("tests/smoke/images/l6max-fresh-date-time.png")
                        && battery_image == Path::new("tests/smoke/images/l6max-fresh-battery-type.png")
                        && home_image == Path::new("tests/smoke/images/l6max-recorder-no-sd.png")
                        && first_confirm == "recorder-function-4" && second_confirm == first_confirm)
                    {
                        return Err("patched update needs complete ordered handoff, installer, restart and MIDI verification checks".into());
                    }
                    let tail_valid = if fixture == UpdateFixture::PatchedDin {
                        matches!(tail, [Action::LifecycleStop])
                    } else if fixture == UpdateFixture::PatchedMidi {
                        matches!(
                            tail,
                            [
                                Action::WaitUsbController,
                                Action::DirectMidiConnect { cable: 1 },
                                Action::MidiEncoderAlternate,
                                Action::HostMidiExpectCc { channel: 0, controller: 81 },
                                Action::HostMidiSettle { guest_ms: 1000 },
                                Action::Checkpoint { name: checkpoint },
                                Action::HostMidiSendCc { channel: 0, controller: 81, value: 100 },
                                Action::ExpectRingChanged { channel: 1, checkpoint: ring_checkpoint, observe_wall_ms: Some(3000), midi_transaction: true },
                                Action::HostMidiEnd,
                                Action::HostMidiSyncBegin,
                                Action::HostMidiDrain { quiet_wall_ms: 500 },
                                Action::HostMidiSendCc {
                                    channel: 0,
                                    controller: 127,
                                    value: 127
                                },
                                Action::HostMidiExpectSync,
                                Action::HostMidiEnd,
                                Action::LifecycleStop
                            ] if checkpoint == "before-midi-cc-1" && ring_checkpoint == checkpoint
                        )
                    } else if fixture == UpdateFixture::PatchedKnobMidi {
                        matches!(
                            tail,
                            [
                                Action::KnobDialogReady,
                                Action::KnobDialogGuard,
                                Action::KnobDialogBaseline,
                                Action::KnobDialogPreflightBegin,
                                ..,
                                Action::KnobDialog,
                                Action::WaitUsbController,
                                Action::DirectMidiConnect { cable: 1 },
                                Action::MidiEncoderAlternate,
                                Action::HostMidiExpectCc { channel: 0, controller: 81 },
                                Action::HostMidiSettle { guest_ms: 1000 },
                                Action::Checkpoint { name: checkpoint },
                                Action::HostMidiSendCc { channel: 0, controller: 81, value: 100 },
                                Action::ExpectRingChanged { channel: 1, checkpoint: ring_checkpoint, observe_wall_ms: Some(3000), midi_transaction: true },
                                Action::HostMidiEnd,
                                Action::HostMidiSyncBegin,
                                Action::HostMidiDrain { quiet_wall_ms: 500 },
                                Action::HostMidiSendCc {
                                    channel: 0,
                                    controller: 127,
                                    value: 127
                                },
                                Action::HostMidiExpectSync,
                                Action::HostMidiEnd,
                                Action::LifecycleStop
                            ] if checkpoint == "before-midi-cc-1" && ring_checkpoint == checkpoint
                        )
                    } else {
                        matches!(
                            tail,
                            [
                                Action::KnobDialogReady,
                                Action::KnobDialogGuard,
                                Action::KnobDialogBaseline,
                                Action::KnobDialogPreflightBegin,
                                ..,
                                Action::KnobDialog,
                                Action::LifecycleStop
                            ]
                        )
                    };
                    if !tail_valid {
                        return Err(
                            "patched update needs the complete patch-specific verification".into(),
                        );
                    }
                    let verify = value
                        .step
                        .iter()
                        .position(|step| {
                            matches!(
                                step.action,
                                Action::LifecycleStart {
                                    profile: LifecycleBoot::Verify
                                }
                            )
                        })
                        .unwrap();
                    if value.step[verify..].iter().any(|step| {
                        matches!(
                            step.action,
                            Action::WaitUsbController
                                | Action::DirectMidiConnect { .. }
                                | Action::MidiEncoderAlternate
                                | Action::HostMidiExpectCc { .. }
                                | Action::HostMidiSettle { .. }
                                | Action::ExpectRingChanged { .. }
                                | Action::HostMidiSyncBegin
                                | Action::HostMidiDrain { .. }
                                | Action::HostMidiSendCc { .. }
                                | Action::HostMidiExpectSync
                                | Action::HostMidiEnd
                        ) && (step.guest_timeout_ms < 180000 || step.wall_timeout_s < 300)
                    }) {
                        return Err("post-update MIDI verification needs the original guest and wall budgets".into());
                    }
                    if matches!(
                        fixture,
                        UpdateFixture::PatchedKnob | UpdateFixture::PatchedKnobMidi
                    ) {
                        let first = value.step[verify..]
                            .iter()
                            .position(|step| matches!(step.action, Action::KnobDialogReady))
                            .ok_or("missing knob verification")?
                            + verify;
                        if value.step[first..value.step.len() - 1].iter().any(|step| {
                            step.guest_timeout_ms != 180_000 || step.wall_timeout_s != 300
                        }) {
                            return Err(
                                "post-update knob verification needs the original shared budgets"
                                    .into(),
                            );
                        }
                    }
                }
                UpdateFixture::BadChecksum | UpdateFixture::BadChecksumStock => {
                    if value.update_goal.is_some() {
                        return Err("bad-checksum fixture has no install goal".into());
                    }
                    if !matches!(
                        actions.as_slice(),
                        [
                            Action::LifecycleStart {
                                profile: LifecycleBoot::Initial
                            },
                            Action::WaitPanelUptime { guest_ms: 40000.. },
                            Action::ExpectUpdateState {
                                mode: 1,
                                flags_mask: 9,
                                flags_value: 0
                            },
                            Action::LifecycleStop,
                            Action::ExpectUpdateMarkerAbsent
                        ]
                    ) {
                        return Err(
                            "bad-checksum update needs its complete rejection checks".into()
                        );
                    }
                }
                UpdateFixture::SyntheticValid => {
                    let semantic = actions
                        .into_iter()
                        .filter(|action| {
                            !matches!(action, Action::WaitInputApplied | Action::Delay { .. })
                        })
                        .collect::<Vec<_>>();
                    if semantic.len() < 12 {
                        return Err("valid handoff is incomplete".into());
                    }
                    let (handoff, tail) = semantic.split_at(12);
                    match value.update_goal {
                        Some(UpdateGoal::Handoff) if !tail.is_empty() => {
                            return Err("handoff goal must end at pending marker".into());
                        }
                        Some(UpdateGoal::Install) if tail.is_empty() => {
                            return Err("install goal requires installer and restart checks".into());
                        }
                        None => {
                            return Err(
                                "valid update needs an explicit handoff or install goal".into()
                            );
                        }
                        _ => {}
                    }
                    if !matches!(handoff, [
                        Action::LifecycleStart { profile: LifecycleBoot::Initial },
                        Action::ExpectDisplayImage { image, .. },
                        Action::ExpectUpdateState { mode: 6, flags_mask: 9, flags_value: 0 },
                        Action::ExpectUpdateGui { window: 0x802003b4, progress_mask: 0, progress_value: 0 },
                        Action::EnqueueTap { target: 2, hold_ms: 50 },
                        Action::EnqueueTap { target: 3, hold_ms: 50 },
                        Action::ExpectUpdateGui { window: 0x8020029c, progress_mask: 0x100, progress_value: 0x100 },
                        Action::ExpectLiveUpdateMarkerAbsent,
                        Action::EnqueueTap { target: 53, hold_ms: 3500 },
                        Action::ExpectPowered { powered: false },
                        Action::LifecycleStop,
                        Action::CaptureUpdateMarkerPending
                    ] if image == std::path::Path::new("tests/smoke/images/l6max-firmware-update-dialog.png"))
                    {
                        return Err(
                            "valid handoff needs its complete ordered UI, power and marker checks"
                                .into(),
                        );
                    }
                    if !tail.is_empty()
                        && !matches!(tail, [
                        Action::LifecycleStart { profile: LifecycleBoot::Installer },
                        Action::ExpectInstallerOutcome { expected: InstallerExpected::Installed },
                        Action::WaitUpdatePanelReady { require_programming: true },
                        Action::ExpectTrace { contains: install_traces, absent: install_absent },
                        Action::ExpectTraceCount { pattern, count: 64, at_least: false, checkpoint: None },
                        Action::ExpectUpdateMode { mode: 4 },
                        Action::ExpectMainMemory { address: 0x8000001c, bytes: install_word },
                        Action::LifecycleStop,
                        Action::ExpectUpdateComparison { field: UpdateComparisonField::MainPayload },
                        Action::ExpectUpdateComparison { field: UpdateComparisonField::BootRegion },
                        Action::ExpectUpdateComparison { field: UpdateComparisonField::ProtectedRegion },
                        Action::ExpectUpdateComparison { field: UpdateComparisonField::PanelImage },
                        Action::ExpectUpdateMarkerAbsent,
                        Action::CaptureInstalledPanel,
                        Action::LifecycleStart { profile: LifecycleBoot::Restart },
                        Action::ExpectInstallerOutcome { expected: InstallerExpected::NoPending },
                        Action::WaitPanelUptime { guest_ms: 40000.. },
                        Action::ExpectMainMemory { address: 0x8000001c, bytes: restart_word },
                        Action::ExpectTrace { contains: restart_contains, absent: restart_absent },
                        Action::LifecycleStop,
                        Action::ExpectPanelUnchanged
                    ] if install_traces == &["l6 panel RTC response start", "l6 panel ROM 16 KiB image match: yes"]
                        && install_absent.is_empty() && pattern == "l6 panel ROM write 080"
                        && install_word == &[0xdf,0x9b,0x57,0x13] && restart_word == install_word
                        && restart_contains.is_empty() && restart_absent == &["l6 panel ROM erase", "l6 panel ROM mass erase"])
                    {
                        return Err("installed update needs complete ordered installer, NOR, panel and restart checks".into());
                    }
                }
            }
            let mut active = false;
            let mut started = false;
            let mut last_profile = None;
            for step in &value.step {
                if matches!(
                    fixture,
                    UpdateFixture::PatchedMidi
                        | UpdateFixture::PatchedDin
                        | UpdateFixture::PatchedKnob
                        | UpdateFixture::PatchedKnobMidi
                ) {
                    let entering_verify = matches!(
                        step.action,
                        Action::LifecycleStart {
                            profile: LifecycleBoot::Verify
                        }
                    );
                    if (!step.models.is_empty() || step.setup_input)
                        && last_profile != Some(LifecycleBoot::Verify)
                        && !entering_verify
                    {
                        return Err(
                            "patched update scopes and setup inputs belong only to the verify boot"
                                .into(),
                        );
                    }
                    if !step.models.is_empty() && step.models != [Model::L6max] {
                        return Err("patched verify step must target L6max".into());
                    }
                    if matches!(step.action, Action::WaitUpdatePanelReady { .. })
                        && (step.guest_timeout_ms != 60000 || step.wall_timeout_s != 300)
                    {
                        return Err(
                            "patched panel readiness needs the original joint deadline".into()
                        );
                    }
                }
                match &step.action {
                    Action::LifecycleStart { profile }
                        if !active
                            && match profile {
                                LifecycleBoot::Initial => last_profile.is_none(),
                                LifecycleBoot::Installer => {
                                    last_profile == Some(LifecycleBoot::Initial)
                                }
                                LifecycleBoot::Restart => {
                                    last_profile == Some(LifecycleBoot::Installer)
                                }
                                LifecycleBoot::Verify => {
                                    last_profile
                                        == Some(if fixture == UpdateFixture::L6PatchedMidi {
                                            LifecycleBoot::Installer
                                        } else {
                                            LifecycleBoot::Restart
                                        })
                                }
                            } =>
                    {
                        active = true;
                        started = true;
                        last_profile = Some(*profile);
                    }
                    Action::LifecycleStop if active => {
                        active = false;
                    }
                    Action::ExpectUpdateMarkerAbsent
                    | Action::MarkL6UpdatePending
                    | Action::ExpectL6UpdateComparison { .. }
                    | Action::CaptureUpdateMarkerPending
                    | Action::ExpectUpdateComparison { .. }
                    | Action::CaptureInstalledPanel
                    | Action::ExpectPanelUnchanged
                        if !active && started => {}
                    Action::LifecycleStart { .. }
                    | Action::LifecycleStop
                    | Action::ExpectUpdateMarkerAbsent
                    | Action::MarkL6UpdatePending
                    | Action::ExpectL6UpdateComparison { .. }
                    | Action::CaptureUpdateMarkerPending
                    | Action::ExpectUpdateComparison { .. }
                    | Action::CaptureInstalledPanel
                    | Action::ExpectPanelUnchanged => {
                        return Err("invalid lifecycle guest/state ordering".into());
                    }
                    _ if !active => return Err("lifecycle action requires an active guest".into()),
                    _ => {}
                }
                match step.action {
                    Action::WaitPanelUptime { guest_ms }
                        if guest_ms == 0 || guest_ms > step.guest_timeout_ms =>
                    {
                        return Err("invalid lifecycle uptime".into());
                    }
                    Action::ExpectUpdateState {
                        flags_mask,
                        flags_value,
                        ..
                    } if flags_mask == 0 || flags_value & !flags_mask != 0 => {
                        return Err("invalid masked flags assertion".into());
                    }
                    Action::ExpectUpdateGui {
                        progress_mask,
                        progress_value,
                        ..
                    } if progress_value & !progress_mask != 0 => {
                        return Err("invalid masked GUI assertion".into());
                    }
                    _ => {}
                }
            }
            let expected_end = match fixture {
                UpdateFixture::PatchedMidi
                | UpdateFixture::PatchedDin
                | UpdateFixture::PatchedKnob
                | UpdateFixture::PatchedKnobMidi
                | UpdateFixture::L6PatchedMidi => matches!(
                    value.step.last().map(|s| &s.action),
                    Some(Action::LifecycleStop)
                ),
                UpdateFixture::L6Stock | UpdateFixture::L6Matching => matches!(
                    value.step.last().map(|s| &s.action),
                    Some(Action::ExpectUpdateMarkerAbsent)
                ),
                UpdateFixture::BadChecksum | UpdateFixture::BadChecksumStock => matches!(
                    value.step.last().map(|s| &s.action),
                    Some(Action::ExpectUpdateMarkerAbsent)
                ),
                UpdateFixture::SyntheticValid => matches!(
                    value.step.last().map(|s| &s.action),
                    Some(Action::CaptureUpdateMarkerPending | Action::ExpectPanelUnchanged)
                ),
            };
            if active || !started || !expected_end {
                return Err("lifecycle must end with stopped guest and expected marker".into());
            }
        } else if value.update_goal.is_some()
            || value.step.iter().any(|step| {
                matches!(
                    step.action,
                    Action::LifecycleStart { .. }
                        | Action::LifecycleStop
                        | Action::WaitPanelUptime { .. }
                        | Action::ExpectUpdateState { .. }
                        | Action::ExpectUpdateMarkerAbsent
                        | Action::ExpectLiveUpdateMarkerAbsent
                        | Action::CaptureUpdateMarkerPending
                        | Action::MarkL6UpdatePending
                        | Action::ExpectL6UpdateResetVector
                        | Action::ExpectL6UpdateComparison { .. }
                        | Action::ExpectUpdateGui { .. }
                        | Action::ExpectInstallerOutcome { .. }
                        | Action::WaitUpdatePanelReady { .. }
                        | Action::WaitUsbController
                        | Action::BeginWallDeadline { .. }
                        | Action::EndWallDeadline
                        | Action::ExpectUpdateMode { .. }
                        | Action::ExpectMainMemory { .. }
                        | Action::ExpectUpdateComparison { .. }
                        | Action::CaptureInstalledPanel
                        | Action::ExpectPanelUnchanged
                )
            })
        {
            return Err("managed update lifecycle needs an update fixture".into());
        }
        if let Some(mode) = &value.sd_bundle_mode {
            if ![
                "valid",
                "absent",
                "corrupt",
                "corrupt-first",
                "ordered-chain",
                "composed-module",
                "duplicate-module",
                "older-module",
                "duplicate-bundle",
            ]
            .contains(&mode.as_str())
                || value.sd_patches.is_none()
            {
                return Err(
                    "sd_bundle_mode requires sd_patches and a supported bundle fixture mode".into(),
                );
            }
        }
        if !value.installed_sd_patches.is_empty()
            && (value.card.is_none() || value.sd_patches.is_some())
        {
            return Err(
                "installed_sd_patches need a card spec and cannot combine sd_patches".into(),
            );
        }
        if !value.effect_replacements.is_empty() && value.sd_patches.is_none() {
            return Err("effect_replacements require sd_patches".into());
        }
        if let Some(patches) = &value.sd_patches {
            if (!patches.is_empty() || !value.effect_replacements.is_empty())
                && value.card.is_none()
            {
                return Err("sd_patches require a card spec".into());
            }
            if !value.installed_patches.is_empty() || !value.update_patches.is_empty() {
                return Err("sd_patches cannot combine installed_patches or update_patches".into());
            }
            let replacements = value
                .effect_replacements
                .iter()
                .map(|(slot, effect)| (slot.clone(), effect.clone()))
                .collect::<Vec<_>>();
            if !patches.is_empty() || !replacements.is_empty() {
                for model in &value.models {
                    patch_core::Plan::new(
                        *model,
                        patch_core::Delivery::SdCard,
                        patches,
                        &replacements,
                    )
                    .map_err(|error| format!("invalid sd_patches for {model}: {error}"))?;
                }
            }
        }
        for model in &value.models {
            let steps: Vec<_> = value
                .step
                .iter()
                .filter(|step| step.applies_to(*model))
                .collect();
            for (i, step) in steps.iter().enumerate() {
                match step.action {
                    Action::UsbControllerReady => {
                        let next = steps
                            .get(i + 1)
                            .ok_or("USB controller readiness needs enumeration")?;
                        if !matches!(next.action, Action::UsbEnumerate)
                            || step.guest_timeout_ms != next.guest_timeout_ms
                            || step.wall_timeout_s != next.wall_timeout_s
                        {
                            return Err("USB controller readiness needs adjacent enumeration with the same budget".into());
                        }
                    }
                    Action::UsbEnumerate
                        if i == 0 || !matches!(steps[i - 1].action, Action::UsbControllerReady) =>
                    {
                        return Err("USB enumeration needs preceding controller readiness".into());
                    }
                    _ => (),
                }
            }
        }
        for model in &value.models {
            let mut pending_menu = false;
            for step in value.step.iter().filter(|step| step.applies_to(*model)) {
                match &step.action {
                    Action::CaptureMenu { .. } if !pending_menu && *model == Model::L6max => {
                        pending_menu = true
                    }
                    Action::ExpectMenuItems { .. } if pending_menu => pending_menu = false,
                    Action::CaptureMenu { .. } | Action::ExpectMenuItems { .. } => {
                        return Err(
                            "menu capture requires L6max and one following ordered-items assertion"
                                .into(),
                        );
                    }
                    _ if pending_menu => {
                        return Err(
                            "menu capture must be followed by ordered-items assertion".into()
                        );
                    }
                    _ => (),
                }
            }
            if pending_menu {
                return Err("menu capture missing ordered-items assertion".into());
            }
        }
        // Assertions consume one immutable sample and share its deadline.
        for model in &value.models {
            let mut scope: Option<(u32, u64, u16)> = None;
            for step in value.step.iter().filter(|step| step.applies_to(*model)) {
                let flag = match step.action {
                    Action::ExpectEffectType { .. } => Some((1, 0)),
                    Action::ExpectEffectProcessor { .. } => Some((2, 0)),
                    Action::ExpectEffectPanelMode { .. } => Some((4, 0)),
                    Action::ExpectEffectEditMode { .. } => Some((8, 0)),
                    Action::ExpectEffectEdits { .. } => Some((16, 256)),
                    Action::ExpectEffectDialog { .. } => Some((32, 512)),
                    Action::ExpectEffectName { .. } => Some((64, 0)),
                    Action::ExpectEffectOutput => Some((128, 0)),
                    _ => None,
                };
                match (&step.action, &mut scope, flag) {
                    (Action::CaptureEffectState { edits, dialog }, None, _) => scope = Some((step.guest_timeout_ms, step.wall_timeout_s, (if *edits { 256 } else { 0 }) | (if *dialog { 512 } else { 0 }))),
                    (_, Some((guest, wall, flags)), Some((bit, required))) if *flags & bit == 0 && *flags & required == required && (*guest,*wall)==(step.guest_timeout_ms,step.wall_timeout_s) => *flags |= bit,
                    (Action::EffectStateEnd, Some((guest, wall, flags)), _) if *flags & 0xff != 0 && (*guest,*wall)==(step.guest_timeout_ms,step.wall_timeout_s) => scope = None,
                    (Action::CaptureEffectState { .. } | Action::EffectStateEnd, _, _) | (_, _, Some(_)) => return Err("effect state needs one snapshot, captured fields, unique assertions and matching deadlines".into()),
                    (_, Some(_), _) => return Err("effect state snapshot must be followed by its assertions".into()),
                    _ => (),
                }
            }
            if scope.is_some() {
                return Err("effect state snapshot is unfinished".into());
            }
        }
        // A baseline belongs to one guest generation and one encoder. Avoid
        // silently comparing a later edit against a stale or replaced capture.
        for model in &value.models {
            let mut ring: Option<u8> = None;
            for step in value.step.iter().filter(|step| step.applies_to(*model)) {
                match step.action {
                    Action::CaptureEffectRing { encoder } if ring.is_none() => ring = Some(encoder),
                    Action::ExpectEffectRingChanged { encoder } if ring == Some(encoder) => {
                        ring = None
                    }
                    Action::CaptureEffectRing { .. } | Action::ExpectEffectRingChanged { .. } => {
                        return Err(
                            "effect ring capture and assertion must match one encoder".into()
                        );
                    }
                    Action::Restart | Action::WarmReset | Action::SdCard { .. }
                        if ring.is_some() =>
                    {
                        return Err(
                            "effect ring baseline cannot cross a restart or SD card change".into(),
                        );
                    }
                    _ => (),
                }
            }
            if ring.is_some() {
                return Err("effect ring capture needs a matching assertion".into());
            }
        }
        for model in &value.models {
            let mut panel_event: Option<(u8, u32, u64)> = None;
            for step in value.step.iter().filter(|step| step.applies_to(*model)) {
                let phase = match step.action {
                    Action::PanelEventBegin { .. } if *model == Model::L6 => Some(0),
                    Action::PanelEventPress => Some(1),
                    Action::PanelEventCapture => Some(2),
                    Action::PanelEventReport => Some(3),
                    Action::PanelEventMapping => Some(4),
                    Action::PanelEventRelease => Some(5),
                    Action::PanelEventSettle => Some(6),
                    Action::PanelEventEnd => Some(7),
                    Action::PanelEventBegin { .. } => return Err("panel event requires L6".into()),
                    _ => None,
                };
                match (panel_event, phase) {
                    (None, Some(0)) => {
                        panel_event = Some((1, step.guest_timeout_ms, step.wall_timeout_s))
                    }
                    (Some((want, guest, wall)), Some(actual))
                        if want == actual
                            && (guest, wall) == (step.guest_timeout_ms, step.wall_timeout_s) =>
                    {
                        panel_event = if want == 7 {
                            None
                        } else {
                            Some((want + 1, guest, wall))
                        };
                    }
                    (Some(_), _) | (None, Some(_)) => {
                        return Err("invalid panel event sequence or budget".into());
                    }
                    _ => {}
                }
            }
            if panel_event.is_some() {
                return Err("unterminated panel event scope".into());
            }
        }
        for model in &value.models {
            let mut meter: Option<(usize, u32, u32, u64)> = None;
            let mut meter_terminal = false;
            for step in value.step.iter().filter(|step| step.applies_to(*model)) {
                if meter_terminal {
                    if matches!(step.action, Action::Restart) {
                        meter_terminal = false;
                    } else {
                        return Err(
                            "L6 meter diagnostic requires restart before further steps".into()
                        );
                    }
                }
                match (&step.action, meter) {
                    (Action::L6MeterBegin, None) if *model == Model::L6 => {
                        meter = Some((0, 0, step.guest_timeout_ms, step.wall_timeout_s));
                    }
                    (Action::L6MeterStage { value }, Some((i, last, guest, wall)))
                        if i < 6
                            && ((i == 0 && *value == 0) || (i > 0 && *value > last))
                            && (guest, wall) == (step.guest_timeout_ms, step.wall_timeout_s) =>
                    {
                        meter = Some((i + 1, *value, guest, wall));
                    }
                    (Action::L6MeterAssert, Some((6, last, guest, wall)))
                        if (guest, wall) == (step.guest_timeout_ms, step.wall_timeout_s) =>
                    {
                        meter = Some((7, last, guest, wall));
                    }
                    (Action::L6MeterEnd, Some((7, _, guest, wall)))
                        if (guest, wall) == (step.guest_timeout_ms, step.wall_timeout_s) =>
                    {
                        meter = None;
                        meter_terminal = true;
                    }
                    (
                        Action::L6MeterBegin
                        | Action::L6MeterStage { .. }
                        | Action::L6MeterAssert
                        | Action::L6MeterEnd,
                        _,
                    )
                    | (_, Some(_)) => {
                        return Err("invalid L6 meter sequence, model or shared budget".into());
                    }
                    _ => {}
                }
            }
            if meter.is_some() {
                return Err("unterminated L6 meter diagnostic".into());
            }
        }
        for model in &value.models {
            let steps: Vec<_> = value
                .step
                .iter()
                .filter(|step| step.applies_to(*model))
                .collect();
            let mut i = 0;
            while i < steps.len() {
                match &steps[i].action {
                    Action::MomentaryDown { control, .. } => {
                        if *model != Model::L6 {
                            return Err("momentary trace scope currently supports L6 only".into());
                        }
                        crate::trace::button_packets(*model, control)?;
                        let scope = steps.get(i..i + 3).ok_or("incomplete momentary scope")?;
                        let observation = matches!(&scope[1].action,
                            Action::ExpectScene { scene, .. } if control == &format!("scene-{}", ['a','b','c'].get(scene.saturating_sub(1)).unwrap_or(&'?')))
                            || matches!(
                                scope[1].action,
                                Action::ExpectButtonLed { .. }
                                    | Action::ExpectRing { .. }
                                    | Action::ExpectMode { .. }
                            );
                        if !observation
                            || !matches!(&scope[2].action, Action::MomentaryUp { control: actual } if actual == control)
                            || scope.iter().any(|s| {
                                (s.guest_timeout_ms, s.wall_timeout_s)
                                    != (scope[0].guest_timeout_ms, scope[0].wall_timeout_s)
                            })
                        {
                            return Err("momentary scope needs a condition, matching release and shared budget".into());
                        }
                        i += 3;
                    }
                    Action::MomentaryUp { .. } => {
                        return Err("momentary release without hold".into());
                    }
                    _ => i += 1,
                }
            }
        }
        for model in &value.models {
            let steps: Vec<_> = value.step.iter().filter(|s| s.applies_to(*model)).collect();
            let is_settings = |a: &Action| {
                matches!(
                    a,
                    Action::SettingsCapture { .. }
                        | Action::SettingsVersions
                        | Action::SettingsRtc
                        | Action::SettingsBank
                        | Action::SettingsTrace
                        | Action::SettingsPanel
                        | Action::SettingsReload
                        | Action::SettingsRtcTiming
                        | Action::SettingsEnd
                )
            };
            let Some(start) = steps.iter().position(|s| is_settings(&s.action)) else {
                continue;
            };
            let first = steps
                .get(start..start + 5)
                .ok_or("incomplete saved settings observation")?;
            if *model != Model::L6max
                || !matches!(first[0].action, Action::SettingsCapture { restored: false })
                || !matches!(first[1].action, Action::SettingsVersions)
                || !matches!(first[2].action, Action::SettingsRtc)
                || !matches!(first[3].action, Action::SettingsBank)
                || !matches!(first[4].action, Action::SettingsEnd)
                || first.iter().any(|s| {
                    (s.guest_timeout_ms, s.wall_timeout_s)
                        != (first[0].guest_timeout_ms, first[0].wall_timeout_s)
                })
            {
                return Err(
                    "saved settings require ordered L6max observations and shared deadline".into(),
                );
            }
            let rest_start = steps[start + 5..]
                .iter()
                .position(|s| is_settings(&s.action))
                .map(|i| start + 5 + i)
                .ok_or("restored settings observation missing")?;
            if !steps[start + 5..rest_start]
                .iter()
                .any(|s| matches!(s.action, Action::Restart))
            {
                return Err("settings restoration requires a restart".into());
            }
            let rest = steps
                .get(rest_start..rest_start + 8)
                .ok_or("incomplete restored settings observation")?;
            if !matches!(rest[0].action, Action::SettingsCapture { restored: true })
                || !matches!(rest[1].action, Action::SettingsVersions)
                || !matches!(rest[2].action, Action::SettingsRtc)
                || !matches!(rest[3].action, Action::SettingsTrace)
                || !matches!(rest[4].action, Action::SettingsPanel)
                || !matches!(rest[5].action, Action::SettingsReload)
                || !matches!(rest[6].action, Action::SettingsRtcTiming)
                || !matches!(rest[7].action, Action::SettingsEnd)
                || rest.iter().any(|s| {
                    (s.guest_timeout_ms, s.wall_timeout_s)
                        != (rest[0].guest_timeout_ms, rest[0].wall_timeout_s)
                })
                || steps[rest_start + 8..]
                    .iter()
                    .any(|s| is_settings(&s.action))
            {
                return Err(
                    "restored settings require ordered observations and shared deadline".into(),
                );
            }
        }
        for model in &value.models {
            let steps: Vec<_> = value.step.iter().filter(|s| s.applies_to(*model)).collect();
            let is_usb_startup = |a: &Action| {
                matches!(
                    a,
                    Action::L6UsbStartupBegin
                        | Action::L6UsbStartupCapture
                        | Action::L6UsbStartupAssertHits
                        | Action::L6UsbStartupAssertWrites
                        | Action::L6UsbStartupAssertReplay
                        | Action::L6UsbStartupEnd
                )
            };
            if let Some(start) = steps.iter().position(|s| is_usb_startup(&s.action)) {
                let scope = steps
                    .get(start..start + 6)
                    .ok_or("incomplete L6 USB startup scope")?;
                if *model != Model::L6
                    || start != 0
                    || !matches!(scope[0].action, Action::L6UsbStartupBegin)
                    || !matches!(scope[1].action, Action::L6UsbStartupCapture)
                    || !matches!(scope[2].action, Action::L6UsbStartupAssertHits)
                    || !matches!(scope[3].action, Action::L6UsbStartupAssertWrites)
                    || !matches!(scope[4].action, Action::L6UsbStartupAssertReplay)
                    || !matches!(scope[5].action, Action::L6UsbStartupEnd)
                    || scope.iter().any(|s| {
                        (s.guest_timeout_ms, s.wall_timeout_s)
                            != (scope[0].guest_timeout_ms, scope[0].wall_timeout_s)
                    })
                    || steps[start + 6..].iter().any(|s| is_usb_startup(&s.action))
                {
                    return Err(
                        "L6 USB startup requires a first-step L6 scope and shared deadline".into(),
                    );
                }
            }
        }
        if value.backends.is_empty() {
            return Err("backends must contain headless and/or gui".into());
        }
        for model in &value.models {
            let steps: Vec<_> = value.step.iter().filter(|s| s.applies_to(*model)).collect();
            let is_dispatch = |a: &Action| {
                matches!(
                    a,
                    Action::L6DispatchBegin { .. }
                        | Action::L6DispatchTask
                        | Action::L6DispatchSemaphore
                        | Action::L6DispatchIdle
                        | Action::L6DispatchInject
                        | Action::L6DispatchWake
                        | Action::L6DispatchState
                        | Action::L6DispatchReader
                        | Action::L6DispatchEvent
                        | Action::L6DispatchEq
                        | Action::L6DispatchObserve
                        | Action::L6DispatchSetter { .. }
                        | Action::L6DispatchFinish
                )
            };
            if let Some(begin) = steps.iter().position(|s| is_dispatch(&s.action)) {
                let scope = steps
                    .get(begin..begin + 21)
                    .ok_or("incomplete terminal DIN dispatch")?;
                if *model != l6fw::Model::L6 || begin != 0 || begin+21 != steps.len()
                    || !matches!(scope[0].action, Action::L6DispatchBegin { .. }) || !matches!(scope[1].action, Action::L6DispatchTask)
                    || !matches!(scope[2].action, Action::L6DispatchSemaphore) || !matches!(scope[3].action, Action::L6DispatchIdle)
                    || !matches!(scope[4].action, Action::L6DispatchInject) || !matches!(scope[5].action, Action::L6DispatchWake)
                    || !matches!(scope[6].action, Action::L6DispatchState) || !matches!(scope[7].action, Action::L6DispatchReader)
                    || !matches!(scope[8].action, Action::L6DispatchEvent) || !matches!(scope[9].action, Action::L6DispatchEq)
                    || !matches!(scope[10].action, Action::L6DispatchObserve)
                    || !(0..9).all(|index| matches!(scope[11+index].action, Action::L6DispatchSetter { index: actual, value } if usize::from(actual)==index && value<=127))
                    || !matches!(scope[20].action, Action::L6DispatchFinish)
                    || scope.iter().any(|s| (s.guest_timeout_ms,s.wall_timeout_s)!=(scope[0].guest_timeout_ms,scope[0].wall_timeout_s)) {
                    return Err("terminal L6 DIN dispatch requires all ordered phases and a shared budget".into());
                }
            }
        }
        for model in &value.models {
            let steps: Vec<_> = value.step.iter().filter(|s| s.applies_to(*model)).collect();
            let is_gpio = |a: &Action| {
                matches!(
                    a,
                    Action::L6GpioBegin { .. }
                        | Action::L6GpioPress
                        | Action::L6GpioSubmit
                        | Action::L6GpioAssert
                        | Action::L6GpioDispatch
                        | Action::L6GpioRelease
                        | Action::L6GpioSettle
                        | Action::L6GpioEnd
                )
            };
            let mut index = 0;
            while index < steps.len() {
                if !is_gpio(&steps[index].action) {
                    index += 1;
                    continue;
                }
                let Action::L6GpioBegin { event, .. } = steps[index].action else {
                    return Err("GPIO scope must start with begin".into());
                };
                let pad = event != 25;
                let count = if pad { 8 } else { 7 };
                let scope = steps
                    .get(index..index + count)
                    .ok_or("incomplete GPIO scope")?;
                let tail = if pad { 5 } else { 4 };
                if *model != l6fw::Model::L6
                    || !matches!(scope[1].action, Action::L6GpioPress)
                    || !matches!(scope[2].action, Action::L6GpioSubmit)
                    || !matches!(scope[3].action, Action::L6GpioAssert)
                    || (pad && !matches!(scope[4].action, Action::L6GpioDispatch))
                    || !matches!(scope[tail].action, Action::L6GpioRelease)
                    || !matches!(scope[tail + 1].action, Action::L6GpioSettle)
                    || !matches!(scope[tail + 2].action, Action::L6GpioEnd)
                    || scope.iter().any(|step| {
                        (step.guest_timeout_ms, step.wall_timeout_s)
                            != (scope[0].guest_timeout_ms, scope[0].wall_timeout_s)
                    })
                {
                    return Err("GPIO requires a complete L6 scope with shared deadlines".into());
                }
                index += count;
            }
        }
        for model in &value.models {
            let steps: Vec<_> = value
                .step
                .iter()
                .filter(|step| step.applies_to(*model))
                .collect();
            let is_tap = |action: &Action| {
                matches!(
                    action,
                    Action::L6maxTapBegin
                        | Action::L6maxTapPress
                        | Action::L6maxTapWait
                        | Action::L6maxTapAssert
                        | Action::L6maxTapRelease
                        | Action::L6maxTapEnd
                )
            };
            if let Some(begin) = steps.iter().position(|step| is_tap(&step.action)) {
                let scope = steps
                    .get(begin..begin + 6)
                    .ok_or("incomplete TAP handler scope")?;
                if *model != l6fw::Model::L6max
                    || !matches!(scope[0].action, Action::L6maxTapBegin)
                    || !matches!(scope[1].action, Action::L6maxTapPress)
                    || !matches!(scope[2].action, Action::L6maxTapWait)
                    || !matches!(scope[3].action, Action::L6maxTapAssert)
                    || !matches!(scope[4].action, Action::L6maxTapRelease)
                    || !matches!(scope[5].action, Action::L6maxTapEnd)
                    || scope.iter().any(|step| {
                        (step.guest_timeout_ms, step.wall_timeout_s)
                            != (scope[0].guest_timeout_ms, scope[0].wall_timeout_s)
                    })
                    || steps[begin + 6..].iter().any(|step| is_tap(&step.action))
                {
                    return Err(
                        "TAP handler requires a complete L6max scope with one shared deadline"
                            .into(),
                    );
                }
            }
        }
        for model in &value.models {
            let steps: Vec<_> = value
                .step
                .iter()
                .filter(|step| step.applies_to(*model))
                .collect();
            let mut index = 0;
            while index < steps.len() {
                if matches!(steps[index].action, Action::UsbMidiOpen) {
                    if index == 0
                        || !matches!(steps[index - 1].action, Action::UsbEnumerate)
                        || index + 8 >= steps.len()
                    {
                        return Err(
                            "USB MIDI needs descriptor enumeration and a complete transaction"
                                .into(),
                        );
                    }
                    let group = &steps[index..index + 9];
                    if !matches!(group[1].action, Action::Encoder { channel: 1, .. })
                        || !matches!(group[2].action, Action::ExpectEncoderReports)
                        || !matches!(group[3].action, Action::UsbMidiBegin)
                        || !matches!(group[4].action, Action::UsbMidiExpectEncoder)
                        || !matches!(
                            group[5].action,
                            Action::UsbMidiPrepareLevel { value: 0..=127 }
                        )
                        || !matches!(group[6].action, Action::UsbMidiSetAlternate)
                        || !matches!(group[7].action, Action::UsbMidiSetTarget)
                        || !matches!(group[8].action, Action::UsbMidiEnd)
                        || group[4..].iter().any(|step| {
                            step.guest_timeout_ms != group[3].guest_timeout_ms
                                || step.wall_timeout_s != group[3].wall_timeout_s
                        })
                    {
                        return Err("USB MIDI transaction requires ordered encoder, receive and level checks with shared deadlines".into());
                    }
                    index += 9;
                } else if matches!(
                    steps[index].action,
                    Action::UsbMidiBegin
                        | Action::UsbMidiExpectEncoder
                        | Action::UsbMidiPrepareLevel { .. }
                        | Action::UsbMidiSetAlternate
                        | Action::UsbMidiSetTarget
                        | Action::UsbMidiEnd
                ) {
                    return Err("USB MIDI action outside transaction".into());
                } else {
                    index += 1;
                }
            }
        }
        let mut checkpoints = std::collections::BTreeSet::new();
        for model in &value.models {
            let mut sound_pad: Option<(u8, u32, u64)> = None;
            let mut recorder: Option<(u8, u32, u64)> = None;
            let mut max_recorder: Option<(u8, u32, u64)> = None;
            let mut input_timing: Option<(u8, u32, u64)> = None;
            let mut usb_storage: Option<(u8, u32, u64)> = None;
            let mut sd_inspection: Option<(u8, bool, u32, u64)> = None;

            let panel_steps: Vec<_> = value
                .step
                .iter()
                .filter(|step| step.applies_to(*model))
                .collect();
            let mut panel_index = 0;
            while panel_index < panel_steps.len() {
                if matches!(panel_steps[panel_index].action, Action::L6maxPanelKeyBegin) {
                    if *model != l6fw::Model::L6max {
                        return Err("panel-key scope requires L6max".into());
                    }
                    panel_index += 1;
                    let mut groups = 0;
                    while panel_index < panel_steps.len()
                        && !matches!(panel_steps[panel_index].action, Action::L6maxPanelKeyEnd)
                    {
                        if panel_index + 4 >= panel_steps.len() {
                            return Err("incomplete panel-key check sequence".into());
                        }
                        let group = &panel_steps[panel_index..panel_index + 5];
                        let (code, bit, word_index) = match group[0].action {
                            Action::L6maxPanelKeyCheck {
                                code,
                                bit,
                                word_index,
                                active: false,
                                wait: false,
                            } if (25..=28).contains(&code) && bit < 32 && word_index < 6 => {
                                (code, bit, word_index)
                            }
                            _ => return Err("invalid panel-key initial check".into()),
                        };
                        let target = match group[1].action {
                            Action::L6maxPanelKeyInput {
                                target,
                                pressed: true,
                            } if target < 54 => target,
                            _ => return Err("invalid panel-key press".into()),
                        };
                        let same = |action: &Action, expected_active| {
                            matches!(action,
                            Action::L6maxPanelKeyCheck { code: c, bit: b, word_index: w, active, wait: true }
                            if (*c,*b,*w,*active) == (code,bit,word_index,expected_active))
                        };
                        if !same(&group[2].action, true)
                            || !matches!(group[3].action, Action::L6maxPanelKeyInput { target: t, pressed: false } if t == target)
                            || !same(&group[4].action, false)
                        {
                            return Err("invalid panel-key check sequence".into());
                        }
                        groups += 1;
                        panel_index += 5;
                    }
                    if groups == 0 || panel_index == panel_steps.len() {
                        return Err("panel-key scope missing end or checks".into());
                    }
                    panel_index += 1;
                } else {
                    if matches!(
                        panel_steps[panel_index].action,
                        Action::L6maxPanelKeyCheck { .. }
                            | Action::L6maxPanelKeyInput { .. }
                            | Action::L6maxPanelKeyEnd
                    ) {
                        return Err("panel-key action outside scope".into());
                    }
                    panel_index += 1;
                }
            }
            let mut din_scope = 0u8;
            let mut din_pending: Option<&[u8]> = None;
            let mut din_pairs = 0usize;
            for step in value.step.iter().filter(|step| step.applies_to(*model)) {
                match (&step.action, din_scope) {
                    (Action::DinUsbBegin, 0) if *model == l6fw::Model::L6 => {
                        din_scope = 1;
                        din_pairs = 0;
                    }
                    (Action::Delay { guest_ms }, 1) if *guest_ms > 0 => din_scope = 2,
                    (Action::DinUsbAttach, 2) => din_scope = 3,
                    (Action::DinUsbSend { bytes }, 3)
                        if !bytes.is_empty() && bytes.len() <= 256 =>
                    {
                        din_pending = Some(bytes);
                        din_scope = 4;
                    }
                    (Action::DinUsbExpect { bytes, cable }, 4)
                        if *cable <= 15 && Some(bytes.as_slice()) == din_pending =>
                    {
                        din_pending = None;
                        din_pairs += 1;
                        din_scope = 3;
                    }
                    (Action::DinUsbEnd, 3) if din_pairs > 0 => din_scope = 0,
                    (
                        Action::DinUsbBegin
                        | Action::DinUsbAttach
                        | Action::DinUsbSend { .. }
                        | Action::DinUsbExpect { .. }
                        | Action::DinUsbEnd,
                        _,
                    ) => return Err("invalid DIN USB forwarding sequence or model".into()),
                    (_, 0) => (),
                    _ => return Err("DIN USB forwarding scope interrupted".into()),
                }
            }
            if din_scope != 0 {
                return Err("DIN USB forwarding scope incomplete".into());
            }
            let mut sync: Option<(u8, u32, u64)> = None;
            let mut l6_probe: Option<(u8, u32, u64)> = None;
            let mut l6_probe_finished = false;
            let mut probe_budget = None;
            let mut probe_stimulus = None;
            let mut probe_ring = None;
            let mut probe_tap = None;
            for step in value.step.iter().filter(|step| step.applies_to(*model)) {
                let storage_phase = match &step.action {
                    Action::UsbStorageInquiry => Some(0),
                    Action::UsbStorageReady => Some(1),
                    Action::UsbStorageCapacity => Some(2),
                    Action::UsbStorageReadBoot => Some(3),
                    Action::UsbStorageAssertBoot => Some(4),
                    Action::UsbStorageLastRead => Some(5),
                    Action::UsbStorageLastWrite => Some(6),
                    Action::UsbStorageLastAssert => Some(7),
                    Action::UsbStorageFatOpen => Some(8),
                    Action::UsbStorageCreate { .. } => Some(9),
                    Action::UsbStorageFlushCreated => Some(10),
                    Action::UsbStorageReadFile => Some(11),
                    Action::UsbStorageRemoveFile => Some(12),
                    Action::UsbStorageAssertFile => Some(13),
                    Action::UsbStorageFlushRemoved => Some(14),
                    Action::UsbStorageEnd => Some(15),
                    _ => None,
                };
                match (&step.action, usb_storage, storage_phase) {
                    (Action::UsbStorageBegin, None, None) => {
                        usb_storage = Some((0, step.guest_timeout_ms, step.wall_timeout_s))
                    }
                    (_, Some((phase, guest, wall)), Some(actual))
                        if phase == actual
                            && (guest, wall) == (step.guest_timeout_ms, step.wall_timeout_s) =>
                    {
                        usb_storage = if phase == 15 {
                            None
                        } else {
                            Some((phase + 1, guest, wall))
                        }
                    }
                    (_, Some(_), _) | (_, None, Some(_)) => {
                        return Err(
                            "USB storage requires ordered steps with matching budgets".into()
                        );
                    }
                    _ => {}
                }
                let timing_phase = match &step.action {
                    Action::L6maxTimingSettle { .. } => Some(0),
                    Action::L6maxTimingBaseline => Some(1),
                    Action::L6maxTimingEnqueue { .. } => Some(2),
                    Action::L6maxTimingWaitFirst => Some(3),
                    Action::L6maxTimingObservePaused { .. } => Some(4),
                    Action::L6maxTimingAssertPaused => Some(5),
                    Action::L6maxTimingResume => Some(6),
                    Action::L6maxTimingWaitQueued => Some(7),
                    Action::L6maxTimingWaitReports => Some(8),
                    Action::L6maxTimingAssertReports => Some(9),
                    Action::L6maxTimingObserveDisplay { .. } => Some(10),
                    Action::L6maxTimingAssertDisplay => Some(11),
                    Action::L6maxTimingEncoder { .. } => Some(12),
                    Action::L6maxTimingWaitEncoder => Some(13),
                    Action::L6maxTimingWaitEncoderReport => Some(14),
                    Action::L6maxTimingAssertEncoder => Some(15),
                    Action::L6maxTimingEnd => Some(16),
                    _ => None,
                };
                match (&step.action, input_timing, timing_phase) {
                    (Action::L6maxTimingBegin, None, None) => {
                        input_timing = Some((0, step.guest_timeout_ms, step.wall_timeout_s))
                    }
                    (_, Some((phase, guest, wall)), Some(actual))
                        if phase == actual
                            && (guest, wall) == (step.guest_timeout_ms, step.wall_timeout_s) =>
                    {
                        input_timing = if phase == 16 {
                            None
                        } else {
                            Some((phase + 1, guest, wall))
                        }
                    }
                    (_, Some(_), _) | (_, None, Some(_)) => {
                        return Err(
                            "L6max timing requires ordered steps with matching budgets".into()
                        );
                    }
                    _ => {}
                }
                let max_recorder_phase = match &step.action {
                    Action::L6maxRecorderPress => Some(0),
                    Action::L6maxRecorderSubmit => Some(1),
                    Action::L6maxRecorderQueue => Some(2),
                    Action::L6maxRecorderDispatch => Some(3),
                    Action::L6maxRecorderScheduler => Some(4),
                    Action::L6maxRecorderCallback => Some(5),
                    Action::L6maxRecorderAction => Some(6),
                    Action::L6maxRecorderStream => Some(7),
                    Action::L6maxRecorderObserve => Some(8),
                    Action::L6maxRecorderSample => Some(9),
                    Action::L6maxRecorderEvidence => Some(10),
                    Action::L6maxRecorderAssertAudio => Some(11),
                    Action::L6maxRecorderEnd => Some(12),
                    _ => None,
                };
                match (&step.action, max_recorder, max_recorder_phase) {
                    (Action::L6maxRecorderBegin { .. }, None, None) => {
                        max_recorder = Some((0, step.guest_timeout_ms, step.wall_timeout_s))
                    }
                    (_, Some((phase, guest, wall)), Some(actual))
                        if phase == actual
                            && (guest, wall) == (step.guest_timeout_ms, step.wall_timeout_s) =>
                    {
                        max_recorder = if phase == 12 {
                            None
                        } else {
                            Some((phase + 1, guest, wall))
                        }
                    }
                    (_, Some(_), _) | (_, None, Some(_)) => {
                        return Err(
                            "L6max recorder requires ordered steps with matching budgets".into(),
                        );
                    }
                    _ => {}
                }
                let recorder_phase = match &step.action {
                    Action::L6RecorderPress => Some(0),
                    Action::L6RecorderDispatcher => Some(1),
                    Action::L6RecorderAssertEvent => Some(2),
                    Action::L6RecorderHandler => Some(3),
                    Action::L6RecorderAssertSelection => Some(4),
                    Action::L6RecorderCallback => Some(5),
                    Action::L6RecorderAssertCallback => Some(6),
                    Action::L6RecorderObserve => Some(7),
                    Action::L6RecorderRelease => Some(8),
                    Action::L6RecorderSample => Some(9),
                    Action::L6RecorderEvidence => Some(10),
                    Action::L6RecorderAssertTrace => Some(11),
                    Action::L6RecorderAssertDma => Some(12),
                    Action::L6RecorderEnd => Some(13),
                    _ => None,
                };
                match (&step.action, recorder, recorder_phase) {
                    (Action::L6RecorderBegin { .. }, None, None) => {
                        recorder = Some((0, step.guest_timeout_ms, step.wall_timeout_s))
                    }
                    (_, Some((phase, guest, wall)), Some(actual))
                        if phase == actual
                            && (guest, wall) == (step.guest_timeout_ms, step.wall_timeout_s) =>
                    {
                        recorder = if phase == 13 {
                            None
                        } else {
                            Some((phase + 1, guest, wall))
                        }
                    }
                    (_, Some(_), _) | (_, None, Some(_)) => {
                        return Err(
                            "recorder transaction requires ordered steps with matching budgets"
                                .into(),
                        );
                    }
                    _ => {}
                }
                let sound_phase = match &step.action {
                    Action::SoundPadPress => Some(0),
                    Action::SoundPadHold => Some(1),
                    Action::SoundPadRelease => Some(2),
                    Action::SoundPadObserve => Some(3),
                    Action::SoundPadSample => Some(4),
                    Action::SoundPadAssertWaveform => Some(5),
                    Action::SoundPadAssertAudio => Some(6),
                    Action::SoundPadAssertLed => Some(7),
                    Action::SoundPadAssertMeter => Some(8),
                    Action::SoundPadFinish => Some(9),
                    _ => None,
                };
                match (&step.action, sound_pad, sound_phase) {
                    (Action::SoundPadBegin { .. }, None, None) => {
                        sound_pad = Some((0, step.guest_timeout_ms, step.wall_timeout_s))
                    }
                    (_, Some((phase, guest, wall)), Some(actual))
                        if phase == actual
                            && (guest, wall) == (step.guest_timeout_ms, step.wall_timeout_s) =>
                    {
                        sound_pad = if phase == 9 {
                            None
                        } else {
                            Some((phase + 1, guest, wall))
                        };
                    }
                    (_, Some(_), _) | (_, None, Some(_)) => {
                        return Err(
                            "sound-pad transaction requires ordered steps with matching budgets"
                                .into(),
                        );
                    }
                    _ => {}
                }
                match (&step.action, sd_inspection) {
                    (Action::ExpectSdLoader, None) => sd_inspection = Some((0, false, step.guest_timeout_ms, step.wall_timeout_s)),
                    (Action::CaptureSdModule { loaded }, Some((0, _, guest, wall))) if (guest, wall) == (step.guest_timeout_ms, step.wall_timeout_s) => sd_inspection = Some((1, *loaded, guest, wall)),
                    (Action::ExpectSdAllocation { loaded }, Some((1, expected, guest, wall))) if *loaded == expected && (guest, wall) == (step.guest_timeout_ms, step.wall_timeout_s) => sd_inspection = Some((2, expected, guest, wall)),
                    (Action::ExpectSdChains { loaded, .. }, Some((2, expected, guest, wall))) if *loaded == expected && (guest, wall) == (step.guest_timeout_ms, step.wall_timeout_s) => sd_inspection = Some((3, expected, guest, wall)),
                    (Action::ExpectSdContinuations { loaded }, Some((3, expected, guest, wall))) if *loaded == expected && (guest, wall) == (step.guest_timeout_ms, step.wall_timeout_s) => sd_inspection = Some((4, expected, guest, wall)),
                    (Action::ExpectSdModule { loaded }, Some((4, expected, guest, wall))) if *loaded == expected && (guest, wall) == (step.guest_timeout_ms, step.wall_timeout_s) => sd_inspection = None,
                    (Action::ExpectSdLoader | Action::CaptureSdModule { .. } | Action::ExpectSdAllocation { .. } | Action::ExpectSdChains { .. } | Action::ExpectSdContinuations { .. } | Action::ExpectSdModule { .. }, _) => return Err("SD inspection requires adjacent loader, capture, allocation, chains, continuations and module steps with matching state and timeouts".into()),
                    (_, Some(_)) => return Err("SD inspection cannot be interrupted".into()),
                    _ => {},
                }
                if l6_probe_finished
                    && !(value.update_fixture == Some(UpdateFixture::L6PatchedMidi)
                        && matches!(step.action, Action::LifecycleStop)
                        && std::ptr::eq(step, value.step.last().unwrap()))
                {
                    return Err(
                        "L6 MIDI probe must be the final action for L6: it leaves MAIN stopped"
                            .into(),
                    );
                }
                match (&step.action, l6_probe) {
                    (Action::L6MidiProbeBegin, None) if *model == l6fw::Model::L6 => {
                        l6_probe = Some((1, step.guest_timeout_ms, step.wall_timeout_s))
                    }
                    (Action::L6MidiProbeInject, Some((1, guest, wall)))
                        if (guest, wall) == (step.guest_timeout_ms, step.wall_timeout_s) =>
                    {
                        l6_probe = Some((2, guest, wall))
                    }
                    (Action::L6MidiProbeExpectHook, Some((2, guest, wall)))
                        if (guest, wall) == (step.guest_timeout_ms, step.wall_timeout_s) =>
                    {
                        l6_probe = Some((3, guest, wall))
                    }
                    (Action::L6MidiProbeExpectReplies, Some((3, guest, wall)))
                        if (guest, wall) == (step.guest_timeout_ms, step.wall_timeout_s) =>
                    {
                        l6_probe = Some((4, guest, wall));
                    }
                    (Action::L6MidiProbeFinish, Some((4, guest, wall)))
                        if (guest, wall) == (step.guest_timeout_ms, step.wall_timeout_s) =>
                    {
                        l6_probe = None;
                        l6_probe_finished = true;
                    }
                    (
                        Action::L6MidiProbeBegin
                        | Action::L6MidiProbeInject
                        | Action::L6MidiProbeExpectHook
                        | Action::L6MidiProbeExpectReplies
                        | Action::L6MidiProbeFinish,
                        _,
                    ) => return Err("invalid L6 MIDI probe sequence, model, or timeouts".into()),
                    (_, Some(_)) => return Err("L6 MIDI probe sequence was interrupted".into()),
                    _ => {}
                }
                if let Action::HostMidiSyncBegin = step.action {
                    if sync
                        .replace((0, step.guest_timeout_ms, step.wall_timeout_s))
                        .is_some()
                    {
                        return Err("MIDI sync transaction is already active".into());
                    }
                } else if let Some((phase, guest, wall)) = sync.as_mut() {
                    if (*guest, *wall) != (step.guest_timeout_ms, step.wall_timeout_s) {
                        return Err("MIDI sync actions need matching timeouts".into());
                    }
                    *phase = match (&step.action, *phase) {
                        (Action::HostMidiDrain { .. }, 0) => 1,
                        (Action::HostMidiSendCc { .. }, 1) => 2,
                        (Action::HostMidiExpectSync, 0 | 2) => 3,
                        (Action::HostMidiEnd, 3) => 4,
                        _ => {
                            return Err(
                                "MIDI sync transaction has an invalid or interrupted sequence"
                                    .into(),
                            );
                        }
                    };
                    if *phase == 4 {
                        sync = None;
                    }
                } else if matches!(step.action, Action::HostMidiExpectSync) {
                    return Err("MIDI sync assertion needs an active transaction".into());
                }
                match &step.action {
                    Action::MidiProbeBegin => {
                        if probe_budget
                            .replace((step.guest_timeout_ms, step.wall_timeout_s))
                            .is_some()
                        {
                            return Err("MIDI probe is already active".into());
                        }
                        probe_stimulus = None;
                        probe_ring = None;
                        probe_tap = None;
                    }
                    Action::MidiProbeEnd => {
                        if probe_budget.take() != Some((step.guest_timeout_ms, step.wall_timeout_s))
                        {
                            return Err(
                                "MIDI probe end needs a matching active transaction and timeouts"
                                    .into(),
                            );
                        }
                    }
                    Action::MidiProbeSendCc { .. }
                    | Action::MidiProbeExpectRing { .. }
                    | Action::MidiProbeWaitRingStable { .. }
                    | Action::MidiProbeDrainAndTap { .. }
                    | Action::MidiProbeExpectNoEvents { .. } => {
                        if probe_budget != Some((step.guest_timeout_ms, step.wall_timeout_s)) {
                            return Err("MIDI probe action needs an active transaction with matching timeouts".into());
                        }
                        match &step.action {
                            Action::MidiProbeSendCc {
                                capture_ring_channel,
                                ..
                            } => {
                                probe_stimulus = Some(*capture_ring_channel);
                                probe_ring = None;
                            }
                            Action::MidiProbeExpectRing { channel, .. } => {
                                if probe_stimulus != Some(*channel) {
                                    return Err("MIDI ring observation needs a preceding stimulus capturing that channel".into());
                                }
                                probe_ring = Some(*channel);
                            }
                            Action::MidiProbeWaitRingStable { channel, .. } => {
                                if probe_ring != Some(*channel) {
                                    return Err("MIDI ring stability needs a preceding observation of that channel".into());
                                }
                            }
                            Action::MidiProbeDrainAndTap { port, .. } => probe_tap = Some(*port),
                            Action::MidiProbeExpectNoEvents { port, .. } => {
                                if probe_tap != Some(*port) {
                                    return Err(
                                        "MIDI no-events observation needs a preceding tap for the same port".into()
                                    );
                                }
                                probe_tap = None;
                            }
                            _ => {}
                        }
                    }
                    Action::Checkpoint { .. } if probe_budget.is_some() => {}
                    _ if probe_budget.is_some() => {
                        return Err("MIDI probe transaction interrupted by another action".into());
                    }
                    _ => {}
                }
            }
            if usb_storage.is_some() {
                return Err("USB storage transaction incomplete".into());
            }
            if input_timing.is_some() {
                return Err("L6max timing transaction incomplete".into());
            }
            if max_recorder.is_some() {
                return Err("L6max recorder transaction is incomplete".into());
            }
            if recorder.is_some() {
                return Err("recorder transaction is incomplete".into());
            }
            if sound_pad.is_some() {
                return Err("sound-pad transaction is incomplete".into());
            }
            if sd_inspection.is_some() {
                return Err("SD inspection is incomplete".into());
            }
            if l6_probe.is_some() {
                return Err("L6 MIDI probe transaction is incomplete".into());
            }
            if probe_budget.is_some() {
                return Err("MIDI probe transaction is not ended".into());
            }
            if sync.is_some() {
                return Err("MIDI sync transaction is not ended".into());
            }
        }
        let mut usb_descriptors_seen = false;
        let mut pad_assignments_seen = false;
        // The display comparison borrows the immediately preceding applicable
        // press's frame and deadline. Do not let unrelated steps reuse it.
        for (index, step) in value.step.iter().enumerate() {
            if !matches!(step.action, Action::ExpectDisplayAfterPress) {
                continue;
            }
            for model in &value.models {
                if !step.applies_to(*model) {
                    continue;
                }
                let previous = value.step[..index]
                    .iter()
                    .rev()
                    .find(|previous| previous.applies_to(*model));
                if !previous.is_some_and(|previous| matches!(previous.action, Action::Press { .. }))
                {
                    return Err("expect-display-after-press requires an immediately preceding press for each model".into());
                }
            }
        }
        for (index, step) in value.step.iter().enumerate() {
            let expected = match step.action {
                Action::KnobDialogGuard => "knob-dialog-ready",
                Action::KnobDialogBaseline => "knob-dialog-guard",
                Action::KnobDialogPreflightBegin => "knob-dialog-baseline",
                Action::KnobRaceBegin => "knob-probe-assert",
                Action::KnobActivationTrigger => "knob-race-assert",
                Action::KnobActiveBegin => "knob-activation-assert",
                Action::KnobLevelBegin => "knob-active-finish",
                Action::KnobTaskBegin => "knob-level-finish",
                Action::KnobOverlayBegin => "knob-task-finish",
                Action::KnobBitmapReactivate { .. } => "knob-overlay-finish",
                Action::KnobBitmapBegin => "knob-bitmap-wait-active",
                Action::KnobOverlayReactivate { .. } => "knob-bitmap-finish",
                Action::KnobOverlayRefreshBegin => "knob-overlay-wait-active",
                Action::KnobLockAcquire => "knob-overlay-refresh-finish",
                Action::KnobDialogAdaptive { .. } => "knob-dialog-mode-end",
                Action::KnobDialog => "knob-dialog-check-navigation",
                _ => continue,
            };
            for model in &value.models {
                if !step.applies_to(*model) {
                    continue;
                }
                let previous = value.step[..index]
                    .iter()
                    .rev()
                    .find(|previous| previous.applies_to(*model));
                if !previous.is_some_and(|previous| {
                    let correct = match expected {
                        "knob-dialog-ready" => matches!(previous.action, Action::KnobDialogReady),
                        "knob-dialog-guard" => matches!(previous.action, Action::KnobDialogGuard),
                        "knob-dialog-baseline" => {
                            matches!(previous.action, Action::KnobDialogBaseline)
                        }
                        "knob-dialog-preflight-begin" => {
                            matches!(previous.action, Action::KnobDialogPreflightBegin)
                        }
                        "knob-probe-assert" => matches!(previous.action, Action::KnobProbeAssert),
                        "knob-race-assert" => {
                            matches!(previous.action, Action::KnobRaceAssert)
                        }
                        "knob-activation-assert" => {
                            matches!(previous.action, Action::KnobActivationAssert)
                        }
                        "knob-active-finish" => {
                            matches!(previous.action, Action::KnobActiveFinish)
                        }
                        "knob-level-finish" => {
                            matches!(previous.action, Action::KnobLevelFinish)
                        }
                        "knob-task-finish" => {
                            matches!(previous.action, Action::KnobTaskFinish)
                        }
                        "knob-overlay-finish" => {
                            matches!(previous.action, Action::KnobOverlayFinish)
                        }
                        "knob-bitmap-wait-active" => {
                            matches!(previous.action, Action::KnobBitmapWaitActive)
                        }
                        "knob-bitmap-finish" => {
                            matches!(previous.action, Action::KnobBitmapFinish)
                        }
                        "knob-overlay-wait-active" => {
                            matches!(previous.action, Action::KnobOverlayWaitActive)
                        }
                        "knob-overlay-refresh-finish" => {
                            matches!(previous.action, Action::KnobOverlayRefreshFinish)
                        }
                        "knob-dialog-check-navigation" => matches!(
                            previous.action,
                            Action::KnobDialogCheckNavigation {
                                field: super::NavigationField::Callback
                            }
                        ),
                        "knob-dialog-expect-restored-heap" => {
                            matches!(previous.action, Action::KnobDialogExpectRestoredHeap)
                        }
                        "knob-dialog-expect-expiry-frame" => matches!(
                            previous.action,
                            Action::KnobDialogExpectExpiryFrame { after: false }
                        ),
                        "knob-dialog-expect-notification-invariants" => matches!(
                            previous.action,
                            Action::KnobDialogExpectNotificationInvariants { clear: true }
                        ),
                        "knob-dialog-settle-level" => {
                            matches!(previous.action, Action::KnobDialogSettleLevel { .. })
                        }
                        "knob-dialog-confirm-invalid-tag" => {
                            matches!(previous.action, Action::KnobDialogConfirmInvalidTag)
                        }
                        "knob-dialog-mode-end" => {
                            matches!(previous.action, Action::KnobDialogModeEnd)
                        }
                        _ => matches!(previous.action, Action::KnobLockFinish),
                    };
                    correct
                        && previous.guest_timeout_ms == step.guest_timeout_ms
                        && previous.wall_timeout_s == step.wall_timeout_s
                }) {
                    return Err(format!(
                        "knob-dialog step requires immediately preceding {expected} with matching deadlines"
                    ));
                }
            }
        }
        // An intrusive debugger session must not outlive a truncated scenario.
        for (index, step) in value.step.iter().enumerate() {
            if !matches!(step.action, Action::KnobDialogPreflightBegin) {
                continue;
            }
            for model in &value.models {
                if !step.applies_to(*model) {
                    continue;
                }
                let applicable = value.step[index + 1..]
                    .iter()
                    .filter(|next| next.applies_to(*model))
                    .collect::<Vec<_>>();
                let Some(end) = applicable
                    .iter()
                    .position(|next| matches!(next.action, Action::KnobDialog))
                else {
                    return Err("knob-dialog preflight must end with the diagnostic".into());
                };
                let subsequent = &applicable[..=end];
                if subsequent.len() < 186
                    || !super::valid_preflight_probe_steps(&subsequent[0..30])
                    || !super::valid_callback_race_steps(&subsequent[30..38])
                    || !matches!(
                        (
                            &subsequent[38].action,
                            &subsequent[39].action,
                            &subsequent[40].action,
                            &subsequent[41].action,
                            &subsequent[42].action
                        ),
                        (
                            Action::KnobActivationTrigger,
                            Action::KnobActivationWait,
                            Action::KnobActivationSample,
                            Action::KnobActivationRestore,
                            Action::KnobActivationAssert
                        )
                    )
                    || !super::valid_active_display_steps(&subsequent[43..50])
                    || !super::valid_level_demo_steps(&subsequent[50..58])
                    || !super::valid_task_context_steps(&subsequent[58..68])
                    || !super::valid_overlay_invariant_steps(&subsequent[68..83])
                    || !super::valid_bitmap_probe_steps(&subsequent[83..97])
                    || !super::valid_refreshed_overlay_steps(&subsequent[97..104])
                    || !super::valid_locked_update_steps(&subsequent[104..112])
                    || !valid_knob_mode_steps(&subsequent[112..subsequent.len().saturating_sub(63)])
                    || !valid_invalid_tag_steps(
                        &subsequent[subsequent.len().saturating_sub(63)
                            ..subsequent.len().saturating_sub(56)],
                    )
                    || !valid_invalid_signature_steps(
                        &subsequent[subsequent.len().saturating_sub(56)
                            ..subsequent.len().saturating_sub(50)],
                    )
                    || !valid_knob_notification_steps(
                        &subsequent[subsequent.len().saturating_sub(50)
                            ..subsequent.len().saturating_sub(42)],
                    )
                    || !valid_knob_expiry_steps(
                        &subsequent[subsequent.len().saturating_sub(42)
                            ..subsequent.len().saturating_sub(38)],
                    )
                    || !valid_knob_restoration_steps(
                        &subsequent[subsequent.len().saturating_sub(38)
                            ..subsequent.len().saturating_sub(20)],
                    )
                    || !valid_knob_navigation_steps(
                        &subsequent[subsequent.len().saturating_sub(20)..subsequent.len() - 1],
                    )
                    || subsequent.iter().any(|next| {
                        next.guest_timeout_ms != step.guest_timeout_ms
                            || next.wall_timeout_s != step.wall_timeout_s
                    })
                {
                    return Err("knob-dialog preflight must be followed immediately by every probe and the diagnostic with matching deadlines".into());
                }
            }
        }
        for model in &value.models {
            let mut indicator_scope: Option<(u32, bool, Option<u8>, bool)> = None;
            for step in value.step.iter().filter(|step| step.applies_to(*model)) {
                match &step.action {
                    Action::L6IndicatorBegin
                        if indicator_scope.is_none() && *model == l6fw::Model::L6 =>
                    {
                        indicator_scope = Some((step.guest_timeout_ms, false, None, false))
                    }
                    Action::L6RingSample {
                        cc,
                        value,
                        guest_ms,
                        ring,
                    } if indicator_scope.is_some() => {
                        let (budget, checked, previous, coverage) =
                            indicator_scope.as_mut().unwrap();
                        if !(0x53..=0x58).contains(cc)
                            || *value > 127
                            || *guest_ms == 0
                            || *guest_ms > *budget
                            || ring.is_some_and(|ring| !(1..=6).contains(&ring))
                        {
                            return Err("invalid L6 ring sample".into());
                        }
                        *previous = None;
                        if ring.is_some() {
                            *checked = true;
                            *coverage = true;
                        }
                    }
                    Action::L6MuteSample {
                        cc,
                        value,
                        guest_ms,
                        reuse_previous,
                        ..
                    } if indicator_scope.is_some() => {
                        let (budget, checked, previous, _) = indicator_scope.as_mut().unwrap();
                        if *cc > 127
                            || *value > 127
                            || *guest_ms == 0
                            || *guest_ms > *budget
                            || (*reuse_previous && *previous != Some(*cc))
                        {
                            return Err("invalid L6 mute sample or baseline".into());
                        }
                        *checked = true;
                        *previous = Some(*cc);
                    }
                    Action::L6RingCoverage { mask }
                        if indicator_scope.is_some_and(|(_, _, _, coverage)| coverage)
                            && *mask <= 0x1ff =>
                    {
                        indicator_scope.as_mut().unwrap().3 = false;
                    }
                    Action::L6IndicatorEnd
                        if indicator_scope
                            .is_some_and(|(_, checked, _, coverage)| checked && !coverage) =>
                    {
                        indicator_scope = None
                    }
                    _ if indicator_scope.is_some()
                        || matches!(
                            step.action,
                            Action::L6IndicatorBegin
                                | Action::L6RingSample { .. }
                                | Action::L6MuteSample { .. }
                                | Action::L6RingCoverage { .. }
                                | Action::L6IndicatorEnd
                        ) =>
                    {
                        return Err("invalid L6 indicator campaign sequence".into());
                    }
                    _ => {}
                }
            }
            if indicator_scope.is_some() {
                return Err("unterminated L6 indicator campaign".into());
            }
        }
        for model in &value.models {
            let mut scope = None;
            for step in value.step.iter().filter(|step| step.applies_to(*model)) {
                match &step.action {
                    Action::UsbSysexBegin if scope.is_none() => {
                        scope = Some((step.guest_timeout_ms, 0usize))
                    }
                    Action::UsbSysexExchange { request, .. } if scope.is_some() => {
                        let (budget, count) = scope.as_mut().unwrap();
                        if request.reply_timeout_ms.unwrap_or(400) > *budget {
                            return Err(
                                "USB SysEx reply window exceeds campaign guest deadline".into()
                            );
                        }
                        *count += 1;
                    }
                    Action::UsbSysexEnd if scope.is_some_and(|(_, count)| count > 0) => {
                        scope = None
                    }
                    _ if scope.is_some()
                        || matches!(
                            step.action,
                            Action::UsbSysexBegin
                                | Action::UsbSysexExchange { .. }
                                | Action::UsbSysexEnd
                        ) =>
                    {
                        return Err("invalid USB SysEx campaign sequence".into());
                    }
                    _ => {}
                }
            }
            if scope.is_some() {
                return Err("unterminated USB SysEx campaign".into());
            }
        }
        for model in &value.models {
            let mut scope: Option<(u8, u32, u64)> = None;
            for step in value.step.iter().filter(|step| step.applies_to(*model)) {
                let phase = match &step.action {
                    Action::UiPadBegin { pad } => {
                        if *model != Model::L6max || *pad > 3 || scope.is_some() {
                            return Err(
                                "UI pad verification requires L6max, pad 0..3, and no active scope"
                                    .into(),
                            );
                        }
                        scope = Some((0, step.guest_timeout_ms, step.wall_timeout_s));
                        continue;
                    }
                    Action::UiPadIdentity => Some(0),
                    Action::UiPadGlobal => Some(1),
                    Action::UiPadKeepAlive => Some(2),
                    Action::UiPadQuery => Some(3),
                    Action::UiPadAssert => Some(4),
                    Action::UiPadEnd => Some(5),
                    _ => None,
                };
                match (scope, phase) {
                    (Some((want, guest, wall)), Some(actual))
                        if want == actual
                            && (guest, wall) == (step.guest_timeout_ms, step.wall_timeout_s) =>
                    {
                        scope = if actual == 5 {
                            None
                        } else {
                            Some((want + 1, guest, wall))
                        };
                    }
                    (Some(_), _) | (None, Some(_)) => {
                        return Err("invalid UI pad verification sequence or shared budget".into());
                    }
                    _ => {}
                }
            }
            if scope.is_some() {
                return Err("unterminated UI pad verification".into());
            }
        }
        for model in &value.models {
            let mut scope: Option<(u8, u32, u64)> = None;
            for step in value.step.iter().filter(|step| step.applies_to(*model)) {
                let phase = match &step.action {
                    Action::EditorPlaybackBegin { pad } => {
                        if *model != Model::L6 || *pad > 3 || scope.is_some() {
                            return Err(
                                "Editor playback requires L6, pad 0..3, and no active scope".into(),
                            );
                        }
                        scope = Some((0, step.guest_timeout_ms, step.wall_timeout_s));
                        continue;
                    }
                    Action::EditorPlaybackPlay => Some(0),
                    Action::EditorPlaybackWait => Some(1),
                    Action::EditorPlaybackObserve => Some(2),
                    Action::EditorPlaybackSample => Some(3),
                    Action::EditorPlaybackAssert => Some(4),
                    Action::EditorPlaybackEnd => Some(5),
                    _ => None,
                };
                match (scope, phase) {
                    (Some((want, guest, wall)), Some(actual))
                        if want == actual
                            && (guest, wall) == (step.guest_timeout_ms, step.wall_timeout_s) =>
                    {
                        scope = if actual == 5 {
                            None
                        } else {
                            Some((want + 1, guest, wall))
                        };
                    }
                    (Some(_), _) | (None, Some(_)) => {
                        return Err("invalid Editor playback sequence or shared budget".into());
                    }
                    _ => {}
                }
            }
            if scope.is_some() {
                return Err("unterminated Editor playback".into());
            }
        }
        for model in &value.models {
            let mut scope: Option<(u8, u8, u32, u64)> = None;
            for step in value.step.iter().filter(|step| step.applies_to(*model)) {
                let phase = match &step.action {
                    Action::EditorAssignBegin => {
                        if scope.is_some() {
                            return Err("nested Editor assignment".into());
                        }
                        scope = Some((0, 0, step.guest_timeout_ms, step.wall_timeout_s));
                        continue;
                    }
                    Action::EditorAssignIdentity => Some((0, None)),
                    Action::EditorAssignGlobal => Some((1, None)),
                    Action::EditorAssignVersion => Some((2, None)),
                    Action::EditorAssignSdInfo => Some((3, None)),
                    Action::EditorAssignRefresh => Some((4, None)),
                    Action::EditorAssignCount { pad } => Some((5, Some(*pad))),
                    Action::EditorAssignFilename { pad } => Some((6, Some(*pad))),
                    Action::EditorAssignSend { pad } => Some((7, Some(*pad))),
                    Action::EditorAssignDialog { pad } => Some((8, Some(*pad))),
                    Action::EditorAssignState { pad } => Some((9, Some(*pad))),
                    Action::EditorAssignAssert { pad } => Some((10, Some(*pad))),
                    Action::EditorAssignEnd => Some((11, None)),
                    _ => None,
                };
                match (scope, phase) {
                    (Some((want, pad, guest, wall)), Some((actual, actual_pad)))
                        if want == actual
                            && actual_pad.is_none_or(|v| v == pad)
                            && (guest, wall) == (step.guest_timeout_ms, step.wall_timeout_s) =>
                    {
                        scope = if actual == 11 {
                            None
                        } else if actual == 10 {
                            Some((if pad == 3 { 11 } else { 5 }, pad + 1, guest, wall))
                        } else {
                            Some((want + 1, pad, guest, wall))
                        };
                    }
                    (Some(_), _) | (None, Some(_)) => {
                        return Err(
                            "invalid Editor assignment sequence, pad or shared budget".into()
                        );
                    }
                    _ => {}
                }
            }
            if scope.is_some() {
                return Err("unterminated Editor assignment".into());
            }
        }
        for model in &value.models {
            let mut phase: Option<u8> = None;
            let mut budget: Option<(u32, u64)> = None;
            let mut targets = std::collections::BTreeSet::new();
            let mut encoders = std::collections::BTreeSet::new();
            for step in value.step.iter().filter(|step| step.applies_to(*model)) {
                let next = match &step.action {
                    Action::L6SweepBegin if phase.is_none() && *model == Model::L6 => {
                        phase = Some(0);
                        budget = Some((step.guest_timeout_ms, step.wall_timeout_s));
                        continue;
                    }
                    Action::L6SweepRejectMain { target: 0 } => Some((0, 1)),
                    Action::L6SweepRejectMain { target: 5 } => Some((1, 2)),
                    Action::L6SweepPanelBaseline { target }
                        if ((7..31).contains(target) || matches!(*target, 1 | 2 | 3 | 6 | 47))
                            && targets.insert(*target) =>
                    {
                        Some((2, 3))
                    }
                    Action::L6SweepPanelTap => Some((3, 4)),
                    Action::L6SweepPanelWait => Some((4, 5)),
                    Action::L6SweepPanelTrace => Some((5, 6)),
                    Action::L6SweepPanelAssert => Some((6, 2)),
                    Action::L6SweepEncoderBaseline if targets.len() == 29 => Some((2, 7)),
                    Action::L6SweepEncoderSend { index }
                        if *index < 6 && encoders.insert(*index) =>
                    {
                        Some((7, 8))
                    }
                    Action::L6SweepEncoderWait => Some((8, 7)),
                    Action::L6SweepEncoderTrace if encoders.len() == 6 => Some((7, 9)),
                    Action::L6SweepEncoderAssert => Some((9, 10)),
                    Action::L6SweepRejectPanel => Some((10, 11)),
                    Action::L6SweepRejectEncoder => Some((11, 12)),
                    Action::L6SweepEnd => Some((12, 13)),
                    Action::L6SweepBegin
                    | Action::L6SweepRejectMain { .. }
                    | Action::L6SweepPanelBaseline { .. }
                    | Action::L6SweepEncoderBaseline
                    | Action::L6SweepEncoderSend { .. }
                    | Action::L6SweepEncoderTrace => {
                        return Err("invalid L6 sweep target or model".into());
                    }
                    _ => None,
                };
                match (phase, next) {
                    (Some(want), Some((actual, next)))
                        if want == actual
                            && budget == Some((step.guest_timeout_ms, step.wall_timeout_s)) =>
                    {
                        phase = if next == 13 { None } else { Some(next) };
                    }
                    (Some(_), _) | (None, Some(_)) => {
                        return Err("invalid L6 input sweep sequence or shared budget".into());
                    }
                    _ => {}
                }
            }
            if phase.is_some() {
                return Err("unterminated L6 input sweep".into());
            }
        }
        for model in &value.models {
            let mut scope: Option<(u8, u8, usize, usize, f32, u32, u64)> = None;
            for step in value.step.iter().filter(|step| step.applies_to(*model)) {
                if matches!(step.action, Action::SignalPwmBegin) {
                    if *model != Model::L6max || scope.is_some() {
                        return Err("SIGNAL PWM requires a fresh L6max scope".into());
                    }
                    scope = Some((0, 0, 0, 0, 0.0, step.guest_timeout_ms, step.wall_timeout_s));
                    continue;
                }
                let is_signal = matches!(
                    step.action,
                    Action::SignalPwmWarmup { .. }
                        | Action::SignalPwmDrain { .. }
                        | Action::SignalPwmStage { .. }
                        | Action::SignalPwmAssert { .. }
                        | Action::SignalPwmEnd
                );
                if let Some((phase, band, stages, assertions, last, guest, wall)) = scope.as_mut() {
                    if (*guest, *wall) != (step.guest_timeout_ms, step.wall_timeout_s) {
                        return Err("SIGNAL PWM changed shared budget".into());
                    }
                    match step.action {
                        Action::SignalPwmWarmup {
                            start: 0,
                            end: 4,
                            value,
                        } if *phase == 0
                            && value.is_finite()
                            && value > 0.0
                            && value <= 40_000_000.0 =>
                        {
                            *phase = 1
                        }
                        Action::SignalPwmDrain { band: b } if *phase == 1 && b == *band => {
                            *phase = 2;
                            *stages = 0;
                            *last = 0.0;
                        }
                        Action::SignalPwmStage {
                            band: b,
                            start,
                            end,
                            value,
                        } if *phase == 2
                            && b == *band
                            && (start, end) == if b == 0 { (0, 4) } else { (4, 12) }
                            && value.is_finite()
                            && value > *last
                            && value <= 40_000_000.0 =>
                        {
                            *stages += 1;
                            *last = value;
                            if *stages == 5 {
                                *phase = 3;
                                *assertions = 0;
                            }
                        }
                        Action::SignalPwmAssert { band: b, channel }
                            if *phase == 3
                                && b == *band
                                && channel == b * 4 + *assertions as u8 =>
                        {
                            *assertions += 1;
                            if *assertions == 4 {
                                if *band == 0 {
                                    *band = 1;
                                    *phase = 1;
                                } else {
                                    *phase = 4;
                                }
                            }
                        }
                        Action::SignalPwmEnd if *phase == 4 => {
                            scope = None;
                        }
                        _ => return Err("invalid SIGNAL PWM sequence or stimulus".into()),
                    }
                } else if is_signal {
                    return Err("SIGNAL PWM step outside scope".into());
                }
            }
            if scope.is_some() {
                return Err("unterminated SIGNAL PWM scope".into());
            }
        }
        for step in &value.step {
            if let Action::KnobDialogAdaptive { plan } = &step.action {
                plan.validate()?;
            }
            if matches!(step.action, Action::EditorAssignBegin) && !usb_descriptors_seen {
                return Err(
                    "editor-pad-assignments requires a preceding usb-enumerate step".into(),
                );
            }
            if matches!(step.action, Action::UsbEnumerate) {
                usb_descriptors_seen = true;
            }
            match &step.action {
                Action::EditorAssignEnd => pad_assignments_seen = true,
                Action::UiPadBegin { pad } => {
                    if !usb_descriptors_seen {
                        return Err(
                            "l6max-ui-pad-verify requires a preceding usb-enumerate step".into(),
                        );
                    }
                    if *pad > 3 {
                        return Err("l6max-ui-pad-verify pad must be 0..3".into());
                    }
                    if value.models.as_slice() != [Model::L6max]
                        || step.models.iter().any(|model| *model != Model::L6max)
                    {
                        return Err("l6max-ui-pad-verify scenarios must be scoped to L6max".into());
                    }
                }
                Action::EditorPlaybackBegin { pad } => {
                    if !pad_assignments_seen {
                        return Err(
                            "editor-pad-playback requires a preceding editor-pad-assignments step"
                                .into(),
                        );
                    }
                    if *pad > 3 {
                        return Err("editor-pad-playback pad must be 0..3".into());
                    }
                    if step.models.iter().any(|model| *model != Model::L6)
                        || (step.models.is_empty()
                            && value.models.iter().any(|model| *model != Model::L6))
                    {
                        return Err("editor-pad-playback scenarios must be scoped to L6".into());
                    }
                }
                Action::SoundPadBegin {
                    pad,
                    target,
                    assignment,
                    playback_window_ms,
                    ..
                } => {
                    if *assignment == PadAssignmentSource::Editor && !pad_assignments_seen {
                        return Err(
                            "sound-pad-playback requires a preceding editor-pad-assignments step"
                                .into(),
                        );
                    }
                    if *pad > 3 || *target != 49 + u32::from(*pad) {
                        return Err(
                            "sound-pad-playback target must match pad (pad 0..3 maps to target 49..52)".into(),
                        );
                    }
                    if !(1000..=120_000).contains(playback_window_ms) {
                        return Err(
                            "sound-pad-playback playback_window_ms must be 1000..120000".into()
                        );
                    }
                }
                Action::L6RecorderPress
                | Action::L6RecorderDispatcher
                | Action::L6RecorderAssertEvent
                | Action::L6RecorderHandler
                | Action::L6RecorderAssertSelection
                | Action::L6RecorderCallback
                | Action::L6RecorderAssertCallback
                | Action::L6RecorderObserve
                | Action::L6RecorderRelease
                | Action::L6RecorderSample
                | Action::L6RecorderEvidence
                | Action::L6RecorderAssertTrace
                | Action::L6RecorderAssertDma
                | Action::L6RecorderEnd => {
                    if step.models.iter().any(|model| *model != Model::L6)
                        || (step.models.is_empty()
                            && value.models.iter().any(|model| *model != Model::L6))
                    {
                        return Err("l6-recorder scenarios must be scoped to L6".into());
                    }
                }
                Action::L6RecorderBegin { target } => {
                    if *target != 4 {
                        return Err("l6-recorder-playback requires MAIN target 4".into());
                    }
                    if step.models.iter().any(|model| *model != Model::L6)
                        || (step.models.is_empty()
                            && value.models.iter().any(|model| *model != Model::L6))
                    {
                        return Err("l6-recorder-playback scenarios must be scoped to L6".into());
                    }
                }
                _ => {}
            }
            if !step.models.is_empty() {
                validate_models(&step.models)?;
            }
            if step.wall_timeout_s == 0 || step.guest_timeout_ms == 0 {
                return Err("timeouts must be positive".into());
            }
            if let Action::MidiProbeExpectRing {
                channel,
                changed,
                guest_ms,
                wall_ms,
                ..
            } = &step.action
                && (*channel == 0
                    || *channel > 8
                    || (!*changed && guest_ms.is_none())
                    || guest_ms.is_some_and(|ms| ms == 0 || ms > step.guest_timeout_ms)
                    || *wall_ms == 0
                    || u64::from(*wall_ms) > step.wall_timeout_s.saturating_mul(1000))
            {
                return Err(
                    "MIDI ring observation needs channel 1..8 and bounded positive windows".into(),
                );
            }
            if let Action::ObserveDuration { guest_ms, .. } = &step.action
                && (*guest_ms == 0 || *guest_ms > step.guest_timeout_ms)
            {
                return Err("observation duration must fit within the guest timeout".into());
            }
            if step.models.iter().any(|m| !value.models.contains(m)) {
                return Err("step model outside scenario scope".into());
            }
            if let Action::ExpectRing {
                mask, settle_ms, ..
            } = &step.action
            {
                if mask.is_some_and(|v| v > 0x1ff)
                    || *settle_ms == 0
                    || *settle_ms > step.guest_timeout_ms
                {
                    return Err("invalid ring mask or settling interval".into());
                }
            }
            if let Action::ExpectMode { mode } | Action::SelectMode { mode } = &step.action {
                for model in &value.models {
                    if step.applies_to(*model) {
                        mode_index(*model, mode)?;
                    }
                }
            }
            if let Action::PowerBegin { hold_ms, .. } = &step.action {
                if *hold_ms == 0 || *hold_ms > step.guest_timeout_ms {
                    return Err("power hold must fit the guest deadline".into());
                }
            }
            if let Action::Analog { channel, steps, .. } = &step.action {
                if !(1..=5).contains(channel) || *steps == 0 {
                    return Err("analog action requires channel 1..5 and nonzero steps".into());
                }
            }
            if let Action::ExpectAnalog { channel, value } = &step.action {
                if !(1..=5).contains(channel) || *value > 1023 {
                    return Err("analog assertion requires channel 1..5 and value 0..1023".into());
                }
            }
            if let Action::PanelEventBegin {
                target,
                column,
                row,
                event,
            } = &step.action
            {
                if *target >= 54 || *column > 3 || *row > 7 || *event > 31 {
                    return Err("panel-event requires valid target/map values".into());
                }
            }
            if let Action::L6DispatchBegin { group, .. } = &step.action {
                if *group > 15 {
                    return Err("l6-din-event-dispatch requires a group from 0 to 15".into());
                }
            }
            if let Action::L6maxRecorderBegin { target } = step.action {
                if target != 4 {
                    return Err("l6max-recorder-playback requires MAIN target 4".into());
                }
            }

            if let Action::L6GpioBegin { target, event } = step.action
                && (!(48..=52).contains(&target) || !(21..=25).contains(&event))
            {
                return Err("l6-gpio-begin requires target 48–52 and event 21–25".into());
            }

            if let Action::CaptureMenu { menu, .. } = &step.action
                && !["main", "project", "sound-pad", "mixer", "system"].contains(&menu.as_str())
            {
                return Err(format!("expect-menu names an unknown menu {menu:?}"));
            }
            if let Action::UsbSysexExchange { request, .. } = &step.action {
                if request.bytes.len() < 2
                    || request.bytes.first() != Some(&0xf0)
                    || request.bytes.last() != Some(&0xf7)
                {
                    return Err("usb-sysex bytes must be a complete F0..F7 message".into());
                }
                if request.expect_prefix.as_ref().is_some_and(Vec::is_empty) {
                    return Err("usb-sysex expect_prefix must not be empty".into());
                }
                if request.reject_prefixes.iter().any(Vec::is_empty) {
                    return Err("usb-sysex reject_prefixes must not contain empty prefixes".into());
                }
                if request
                    .reply_timeout_ms
                    .is_some_and(|timeout| timeout == 0 || timeout > step.guest_timeout_ms)
                {
                    return Err("usb-sysex reply_timeout_ms must be positive and fit the step guest timeout".into());
                }
            }
            if let Action::ExpectHeap { minimum_free } = &step.action {
                if *minimum_free == 0
                    || [l6fw::Model::L6, l6fw::Model::L6max]
                        .iter()
                        .any(|&model| *minimum_free > l6max_host::heap::size(model))
                {
                    return Err("expect-heap minimum_free must be positive and fit the heap".into());
                }
            }
            if let Action::WaitAudioInput { min_ms, max_ms, .. } = &step.action
                && min_ms > max_ms
            {
                return Err("wait-audio-input minimum must not exceed maximum".into());
            }
            if let Action::WaitEffectMode {
                panel_mode: None,
                edit_mode: None,
            } = &step.action
            {
                return Err("wait-effect-mode needs a panel or edit mode".into());
            }
            if let Action::WaitRecordedFiles { files } = &step.action
                && (files.is_empty()
                    || files.len() > 16
                    || files.iter().any(|file| {
                        file.is_empty()
                            || file.contains(['/', '\\', ':'])
                            || file == "."
                            || file == ".."
                    }))
            {
                return Err("wait-recorded-files needs 1..16 simple file names".into());
            }
            if let Action::WaitL6SoundPadFile { path } = &step.action {
                if path.is_empty()
                    || path.len() > 240
                    || path.contains(['\\', ':'])
                    || path
                        .split('/')
                        .any(|part| part.is_empty() || part == "." || part == "..")
                {
                    return Err("wait-l6-sound-pad-file needs a relative SD card path".into());
                }
            }
            if let Action::ExpectMemory { symbol, bytes } | Action::WaitMemory { symbol, bytes } =
                &step.action
            {
                if bytes.is_empty() || bytes.len() > 4096 {
                    return Err("expect-memory needs 1..4096 bytes".into());
                }
                for &model in value.models.iter().filter(|&&model| step.applies_to(model)) {
                    let address = crate::firmware::symbol(model, symbol).ok_or_else(|| {
                        format!(
                            "expect-memory symbol {symbol} is not declared in patches/api/{}",
                            model.name()
                        )
                    })?;
                    if address.checked_add(bytes.len() as u32).is_none() {
                        return Err(format!(
                            "expect-memory {symbol} reads past the address space"
                        ));
                    }
                }
            }
            if let Action::ExpectSdChains {
                loaded,
                chain_count,
            } = &step.action
                && (value.sd_patches.is_none()
                    || *loaded && value.sd_patches.as_ref().is_some_and(Vec::is_empty)
                    || chain_count.is_some_and(|count| count == 0 || count > 8))
            {
                return Err("expect-patch-loader requires an Patch loader fixture (and a selected patch when loaded=true)".into());
            }
            if matches!(step.action, Action::ExpectSdLoader) && value.sd_patches.is_none() {
                return Err("SD inspection requires a Patch loader fixture".into());
            }
            if let Action::HoldChord {
                controls,
                duration_ms,
                until,
            } = &step.action
            {
                if controls.is_empty() || controls.len() > 16 {
                    return Err("hold-chord requires 1..16 controls".into());
                }
                match (duration_ms, until) {
                    (Some(duration), None) if *duration > 0 && *duration <= step.guest_timeout_ms => {},
                    (None, Some(ChordCondition::FactoryMode)) if value.models.iter().all(|model| !step.applies_to(*model) || *model == Model::L6max) => {},
                    _ => return Err("hold-chord needs either a bounded duration or the L6max factory-mode condition".into()),
                }
                for model in &value.models {
                    if step.applies_to(*model) {
                        let model = *model;
                        let mut targets = std::collections::BTreeSet::new();
                        for control in controls {
                            let target = control_target(model, control)?;
                            if !targets.insert(target) {
                                return Err("hold-chord controls must be unique".into());
                            }
                        }
                    }
                }
            }
            match &step.action {
                Action::KnobDialogModeBegin { scene } => {
                    if scene.is_empty()
                        || scene.len() > 64
                        || !scene
                            .bytes()
                            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
                    {
                        return Err("knob-dialog-mode-begin requires a simple scene name".into());
                    }
                }
                Action::EnqueueTap { target, hold_ms } => {
                    if l6ctl::button_source(*target).is_none() || *hold_ms == 0 || *hold_ms > 60_000
                    {
                        return Err(
                            "enqueue-tap requires a valid target and hold_ms in 1..=60000".into(),
                        );
                    }
                }
                Action::WaitKnobParameterMode { mode } if *mode > 9 => {
                    return Err("wait-knob-parameter-mode requires mode 0..9".into());
                }
                Action::CaptureDisplay { guest_ms, artifact } => {
                    if *guest_ms == 0
                        || *guest_ms > step.guest_timeout_ms
                        || artifact.is_empty()
                        || artifact.len() > 64
                        || !artifact
                            .bytes()
                            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
                    {
                        return Err("capture-display requires a positive duration within its deadline and a simple artifact name".into());
                    }
                }
                Action::WaitKnobDisplayImage {
                    image,
                    main_timeout_ms,
                } => {
                    spec_path(image)?;
                    if image.extension().is_none_or(|extension| extension != "png")
                        || *main_timeout_ms == 0
                    {
                        return Err(
                            "wait-knob-display-image requires a PNG and a positive MAIN timeout"
                                .into(),
                        );
                    }
                }
                Action::ExpectCapturedDisplayImage { image } => {
                    spec_path(image)?;
                    if image.extension().is_none_or(|extension| extension != "png") {
                        return Err("expect-captured-display-image needs a PNG image".into());
                    }
                }
                Action::ExpectDisplayImage { image, settle_ms } => {
                    spec_path(image)?;
                    if image.extension().is_none_or(|extension| extension != "png") {
                        return Err("expect-display-image needs a PNG image".into());
                    }
                    if *settle_ms > step.guest_timeout_ms {
                        return Err(
                            "expect-display-image settle_ms exceeds the guest timeout".into()
                        );
                    }
                }
                Action::Checkpoint { name } => {
                    if name.is_empty()
                        || name.len() > 64
                        || !name
                            .bytes()
                            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
                        || !checkpoints.insert(name.clone())
                    {
                        return Err("checkpoint needs a unique nonempty name".into());
                    }
                }
                Action::ExpectRingChanged { checkpoint, .. }
                | Action::ExpectRingUnchanged { checkpoint, .. }
                | Action::ExpectDisplay { checkpoint, .. }
                | Action::ExpectScene {
                    checkpoint: Some(checkpoint),
                    ..
                }
                | Action::ExpectButtonLed {
                    checkpoint: Some(checkpoint),
                    ..
                }
                | Action::ExpectTraceCount {
                    checkpoint: Some(checkpoint),
                    ..
                }
                | Action::ExpectRing {
                    checkpoint: Some(checkpoint),
                    ..
                }
                | Action::ExpectRing {
                    same_as_checkpoint: Some(checkpoint),
                    ..
                } => {
                    if !checkpoints.contains(checkpoint) {
                        return Err(format!("unknown checkpoint {checkpoint}"));
                    }
                }
                Action::ExpectTrace { contains, absent }
                    if contains.is_empty() && absent.is_empty() =>
                {
                    return Err("trace assertion must contain a condition".into());
                }
                Action::ExpectTraceCount { pattern, .. } if pattern.is_empty() => {
                    return Err("trace count assertion requires a pattern".into());
                }
                Action::ExpectEffect { effect }
                    if !["hall", "room", "spring", "delay", "echo"].contains(&effect.as_str()) =>
                {
                    return Err("effect must be hall, room, spring, delay or echo".into());
                }
                Action::Hold { duration_ms, .. } if *duration_ms == 0 || *duration_ms > 59_000 => {
                    return Err("hold duration must be between 1 and 59000 ms".into());
                }
                Action::MomentaryDown { duration_ms, .. }
                    if *duration_ms == 0 || *duration_ms > 59_000 =>
                {
                    return Err("momentary duration must be between 1 and 59000 ms".into());
                }
                Action::ExpectScene { scene, .. } if !(1..=3).contains(scene) => {
                    return Err("scene index must be between 1 and 3".into());
                }
                Action::ExpectScene { .. }
                    if (value.models.iter().any(|m| *m == Model::L6max)
                        && (step.models.is_empty()
                            || step.models.iter().any(|m| *m == Model::L6max))) =>
                {
                    return Err("scene lamp assertions currently support L6 only".into());
                }
                Action::ExpectButtonLed { control, .. } => {
                    for model in &value.models {
                        if step.applies_to(*model) {
                            l6_button_led_index(*model, control)?;
                        }
                    }
                }
                Action::ExpectEffect { .. }
                    if value.models.iter().any(|m| *m == Model::L6max)
                        && (step.models.is_empty()
                            || step.models.iter().any(|m| *m == Model::L6max)) =>
                {
                    return Err("effect assertions currently support L6 only".into());
                }
                Action::ExpectMode { mode } if mode_index(Model::L6max, mode).is_err() => {
                    return Err("unsupported mode".into());
                }
                Action::Wait { until }
                    if until != "boot-ready"
                        && until != "panel-settled"
                        && until != "mixer-ready"
                        && until != "sd-patch-loaded"
                        && until != "sd-patch-empty" =>
                {
                    return Err(format!("unknown wait condition: {until}"));
                }
                Action::Press { control }
                | Action::Hold { control, .. }
                | Action::Switch { control, .. }
                | Action::ExpectButtonReport { control }
                | Action::ExpectStartupReport { control } => {
                    for model in &value.models {
                        if step.applies_to(*model) {
                            let target = control_target(*model, control)?;
                            if matches!(step.action, Action::Switch { .. })
                                && !is_switch(*model, target)
                            {
                                return Err("switch action requires a latching switch".into());
                            }
                            if matches!(step.action, Action::Press { .. } | Action::Hold { .. })
                                && is_switch(*model, target)
                            {
                                return Err("use switch action for a latching switch".into());
                            }
                            if matches!(
                                step.action,
                                Action::ExpectButtonReport { .. }
                                    | Action::ExpectStartupReport { .. }
                            ) {
                                trace::button_packets(*model, control)?;
                            }
                        }
                    }
                }
                Action::EnqueueEncoder { channel, steps, .. }
                | Action::Encoder { channel, steps, .. }
                    if *steps == 0 || steps.unsigned_abs() > 120 || *channel == 0 =>
                {
                    return Err(
                        "encoder channel must be positive, steps must be nonzero and at most 120"
                            .into(),
                    );
                }
                Action::HostMidiExpectCc {
                    channel,
                    controller,
                } if *channel > 15 || *controller > 127 => {
                    return Err("MIDI channel must be 0..15 and controller 0..127".into());
                }
                Action::HostMidiSendCc {
                    channel,
                    controller,
                    value,
                } if *channel > 15 || *controller > 127 || *value > 127 => {
                    return Err("MIDI channel must be 0..15 and controller/value 0..127".into());
                }
                Action::MidiProbeSendCc {
                    channel,
                    controller,
                    value,
                    capture_ring_channel,
                    ..
                } if *channel > 15
                    || *controller > 127
                    || *value > 127
                    || *capture_ring_channel == 0
                    || *capture_ring_channel > 8 =>
                {
                    return Err(
                        "MIDI channel must be 0..15, controller/value 0..127 and ring channel 1..8"
                            .into(),
                    );
                }
                Action::MidiProbeWaitRingStable {
                    channel,
                    guest_ms,
                    wall_ms,
                } if *channel == 0
                    || *channel > 8
                    || *guest_ms == 0
                    || *guest_ms > step.guest_timeout_ms
                    || *wall_ms == 0
                    || u64::from(*wall_ms) > step.wall_timeout_s.saturating_mul(1000) =>
                {
                    return Err(
                        "MIDI ring stability needs channel 1..8 and bounded positive windows"
                            .into(),
                    );
                }
                Action::MidiProbeDrainAndTap {
                    target, hold_ms, ..
                } if l6ctl::button_source(*target).is_none() || !(1..=60_000).contains(hold_ms) => {
                    return Err("MIDI probe tap needs valid target and hold duration".into());
                }
                Action::MidiProbeExpectNoEvents { wall_ms, .. }
                    if *wall_ms == 0
                        || u64::from(*wall_ms) > step.wall_timeout_s.saturating_mul(1000) =>
                {
                    return Err("MIDI no-events wall interval must fit within timeout".into());
                }
                Action::HostMidiDrain { quiet_wall_ms }
                    if *quiet_wall_ms == 0
                        || u64::from(*quiet_wall_ms) > step.wall_timeout_s.saturating_mul(1000) =>
                {
                    return Err(
                        "MIDI quiet interval must be positive and within the wall timeout".into(),
                    );
                }
                Action::HostMidiExpectCount { count } if *count == 0 || *count > 16384 => {
                    return Err("MIDI message count must be 1..16384".into());
                }
                Action::KnobPopupOpen {
                    first_steps,
                    fallback_steps,
                    attempt_guest_ms,
                } if *first_steps == 0
                    || *fallback_steps == 0
                    || first_steps.unsigned_abs() > 120
                    || fallback_steps.unsigned_abs() > 120
                    || *attempt_guest_ms == 0
                    || *attempt_guest_ms > step.guest_timeout_ms =>
                {
                    return Err("knob popup turns must be nonzero and at most 120, and its attempt duration must fit the guest timeout".into());
                }
                Action::HostMidiSettle { guest_ms }
                    if *guest_ms == 0 || *guest_ms > step.guest_timeout_ms =>
                {
                    return Err(
                        "MIDI settle duration must be positive and within the guest timeout".into(),
                    );
                }
                Action::CaptureEffectRing { encoder }
                | Action::ExpectEffectRingChanged { encoder }
                    if !(1..=8).contains(encoder) =>
                {
                    return Err("effect ring encoder must be 1..8".into());
                }
                Action::ExpectRingLevel {
                    channel,
                    segment,
                    min,
                    max,
                } if *channel == 0
                    || segment.is_some_and(|segment| !(1..=9).contains(&segment))
                    || min > max =>
                {
                    return Err(
                        "ring level assertion needs a channel, optional segment 1..9, and min <= max".into(),
                    );
                }
                Action::ExpectRing {
                    channel,
                    mask,
                    changed,
                    checkpoint,
                    ..
                } if *channel == 0
                    || mask.is_some_and(|v| v > 0x1ff)
                    || (mask.is_none() && !changed && checkpoint.is_none()) =>
                {
                    return Err(
                        "ring assertion needs a valid channel and mask or changed=true".into(),
                    );
                }
                _ => {}
            }
            if let Action::ExpectButtonLed {
                same: true,
                checkpoint: None,
                ..
            } = &step.action
            {
                return Err("expect-button-led same=true requires a checkpoint".into());
            }
            if let Action::EnqueueEncoder { channel, .. }
            | Action::ExpectRingChanged { channel, .. }
            | Action::Encoder { channel, .. }
            | Action::ExpectRingLevel { channel, .. }
            | Action::ExpectRing { channel, .. }
            | Action::ExpectMute { channel } = &step.action
            {
                for model in &value.models {
                    if step.applies_to(*model) && !(1..=model.channels()).contains(channel) {
                        return Err(format!("channel {channel} is not available on {model}"));
                    }
                }
            }
        }
        for step in &value.step {
            if let Action::EffectPacedTurn { channel, steps } = step.action {
                if !(1..=8).contains(&channel)
                    || steps == 0
                    || steps.unsigned_abs() > 128
                    || value
                        .models
                        .iter()
                        .any(|model| step.applies_to(*model) && *model != l6fw::Model::L6max)
                {
                    return Err(
                        "effect-paced-turn requires L6max, channel 1..8 and 1..128 turns".into(),
                    );
                }
            }
        }
        for step in &value.step {
            if let Action::DirectMidiConnect { cable } = &step.action
                && *cable > 15
            {
                return Err("direct-midi-connect cable must be 0..15".into());
            }
        }
        for model in &value.models {
            let mut previous: Option<&super::Step> = None;
            for step in value.step.iter().filter(|step| step.applies_to(*model)) {
                if let Action::ExpectRingUnchanged {
                    channel,
                    observe_wall_ms,
                    ..
                } = &step.action
                {
                    if *channel == 0
                        || *channel > model.channels()
                        || *observe_wall_ms == 0
                        || u64::from(*observe_wall_ms) > step.wall_timeout_s.saturating_mul(1000)
                        || !previous.is_some_and(|p| {
                            matches!(p.action, Action::HostMidiSendCc { .. })
                                && (p.guest_timeout_ms, p.wall_timeout_s)
                                    == (step.guest_timeout_ms, step.wall_timeout_s)
                        })
                    {
                        return Err("negative MIDI ring observation requires an adjacent CC send, valid ring and shared bounded window".into());
                    }
                }
                previous = Some(step);
            }
        }
        for step in &value.step {
            if let Action::ExpectRingChanged {
                observe_wall_ms, ..
            } = &step.action
            {
                if observe_wall_ms.is_some_and(|ms| {
                    ms == 0 || u64::from(ms) > step.wall_timeout_s.saturating_mul(1000)
                }) {
                    return Err(
                        "ring observation needs a positive wall window within its step timeout"
                            .into(),
                    );
                }
            }
        }
        for model in &value.models {
            let steps: Vec<_> = value
                .step
                .iter()
                .filter(|step| step.applies_to(*model))
                .collect();
            for (index, step) in steps.iter().enumerate() {
                if let Action::WaitEffectSlot {
                    expected_effect_type,
                    expected_processor_type,
                    expected_effect,
                    require_output,
                } = &step.action
                {
                    if expected_effect
                        .as_ref()
                        .is_some_and(|effect| effect.is_empty())
                    {
                        return Err("effect slot wait needs a nonempty effect ID".into());
                    }
                    let next = steps
                        .get(index + 1)
                        .ok_or("effect slot wait needs a following assertion")?;
                    let split = matches!(&next.action, Action::CaptureEffectState { .. })
                        && {
                            let checks = steps[index + 2..]
                                .iter()
                                .take_while(|candidate| {
                                    !matches!(candidate.action, Action::EffectStateEnd)
                                })
                                .collect::<Vec<_>>();
                            checks.iter().any(|candidate| matches!(candidate.action, Action::ExpectEffectType { value } if value == *expected_effect_type))
                            && checks.iter().any(|candidate| matches!(candidate.action, Action::ExpectEffectProcessor { value } if value == *expected_processor_type))
                            && expected_effect.as_ref().is_none_or(|expected| checks.iter().any(|candidate| matches!(&candidate.action, Action::ExpectEffectName { name } if name == expected)))
                            && (!*require_output || checks.iter().any(|candidate| matches!(candidate.action, Action::ExpectEffectOutput)))
                        };
                    if (step.guest_timeout_ms, step.wall_timeout_s)
                        != (next.guest_timeout_ms, next.wall_timeout_s)
                        || !split
                    {
                        return Err("effect slot wait needs a matching adjacent assertion with the same budget".into());
                    }
                }
            }
        }
        for model in &value.models {
            let steps: Vec<_> = value
                .step
                .iter()
                .filter(|step| step.applies_to(*model))
                .collect();
            let is_sweep = |action: &Action| {
                matches!(
                    action,
                    Action::L6DinSweepBegin
                        | Action::L6DinSweepReady
                        | Action::L6DinSweepMonitor
                        | Action::L6DinSweepBaseline
                        | Action::L6DinSweepSample { .. }
                        | Action::L6DinSweepTrace
                        | Action::L6DinSweepDrain
                        | Action::L6DinSweepStop
                        | Action::L6DinSweepCapture
                        | Action::L6DinSweepAssert { .. }
                        | Action::L6DinSweepEnd
                )
            };
            let Some(begin) = steps.iter().position(|step| is_sweep(&step.action)) else {
                continue;
            };
            let Some(scope) = steps.get(begin..begin + 147) else {
                return Err("incomplete L6 DIN sweep".into());
            };
            if *model != l6fw::Model::L6 || !matches!(scope[0].action, Action::L6DinSweepBegin)
                || !matches!(scope[1].action, Action::L6DinSweepReady) || !matches!(scope[2].action, Action::L6DinSweepMonitor)
                || !matches!(scope[3].action, Action::L6DinSweepBaseline)
                || !(0..128).all(|cc| matches!(scope[4+cc].action, Action::L6DinSweepSample { cc: actual } if usize::from(actual) == cc))
                || !matches!(scope[132].action, Action::L6DinSweepTrace) || !matches!(scope[133].action, Action::L6DinSweepDrain)
                || !matches!(scope[134].action, Action::L6DinSweepStop) || !matches!(scope[135].action, Action::L6DinSweepCapture)
                || ![DinSweepCheck::Uart, DinSweepCheck::RingBytes, DinSweepCheck::WriteIndex, DinSweepCheck::ReadIndex, DinSweepCheck::Indicator, DinSweepCheck::Handler, DinSweepCheck::Submitter, DinSweepCheck::Reader, DinSweepCheck::Setter, DinSweepCheck::Mixer].iter().enumerate().all(|(i, expected)| matches!(scope[136+i].action, Action::L6DinSweepAssert { check } if check == *expected))
                || !matches!(scope[146].action, Action::L6DinSweepEnd)
                || scope.iter().any(|step| (step.guest_timeout_ms, step.wall_timeout_s) != (scope[0].guest_timeout_ms, scope[0].wall_timeout_s))
                || steps[begin+147..].iter().any(|step| is_sweep(&step.action))
            { return Err("L6 DIN sweep requires all 128 ordered CCs, full evidence phases and one shared budget".into()); }
        }
        for model in &value.models {
            let steps: Vec<_> = value
                .step
                .iter()
                .filter(|step| step.applies_to(*model))
                .collect();
            let has_audio = steps.iter().any(|step| {
                matches!(
                    step.action,
                    Action::AudioCallbacksBegin
                        | Action::AudioCallbacksNext
                        | Action::AudioCallbacksCapture
                        | Action::AudioCallbacksAssert
                        | Action::AudioCallbacksEnd
                )
            });
            if !has_audio {
                continue;
            }
            let count = if *model == l6fw::Model::L6 { 5 } else { 2 };
            if steps.len() < count + 4
                || !matches!(steps[0].action, Action::AudioCallbacksBegin)
                || !steps[1..1 + count]
                    .iter()
                    .all(|step| matches!(step.action, Action::AudioCallbacksNext))
                || !matches!(steps[1 + count].action, Action::AudioCallbacksCapture)
                || !matches!(steps[2 + count].action, Action::AudioCallbacksAssert)
                || !matches!(steps[3 + count].action, Action::AudioCallbacksEnd)
                || steps[1..count + 4].iter().any(|step| {
                    (step.guest_timeout_ms, step.wall_timeout_s)
                        != (steps[0].guest_timeout_ms, steps[0].wall_timeout_s)
                })
                || steps[count + 4..].iter().any(|step| {
                    matches!(
                        step.action,
                        Action::AudioCallbacksBegin
                            | Action::AudioCallbacksNext
                            | Action::AudioCallbacksCapture
                            | Action::AudioCallbacksAssert
                            | Action::AudioCallbacksEnd
                    )
                })
            {
                return Err(format!(
                    "{model}: audio callbacks require the full ordered scope and one shared budget"
                ));
            }
        }
        Ok(())
    }
}

/// The popup's baseline and overall deadline belong to one uninterrupted
/// transaction; each independent observation remains visible in the report.
fn validate_popup_steps(value: &Scenario) -> Result<(), String> {
    for model in &value.models {
        let mut scope: Option<(u8, u32, u64)> = None;
        for step in value.step.iter().filter(|step| step.applies_to(*model)) {
            let phase = match step.action {
                Action::KnobPopupOpen { .. } => Some(0),
                Action::KnobPopupExpectOpen => Some(1),
                Action::KnobPopupExpectDisplay => Some(2),
                Action::KnobPopupExpectValue => Some(3),
                _ => None,
            };
            match (scope, phase) {
                (None, Some(0)) if *model == l6fw::Model::L6max =>
                    scope = Some((1, step.guest_timeout_ms, step.wall_timeout_s)),
                (Some((expected, guest, wall)), Some(actual))
                    if expected == actual && (guest, wall) == (step.guest_timeout_ms, step.wall_timeout_s) => {
                    scope = if actual == 3 { None } else { Some((actual + 1, guest, wall)) };
                }
                (None, None) => (),
                _ => return Err("knob popup requires ordered open, state, display and value steps with one shared budget".into()),
            }
        }
        if scope.is_some() {
            return Err("knob popup transaction is incomplete".into());
        }
    }
    Ok(())
}

#[cfg(test)]
mod popup_transaction_tests {
    use super::super::Scenario;
    #[test]
    fn requires_each_observation_in_order_with_one_budget() {
        let header = "version=1\nname='popup'\nmodels=['l6max']\nbackends=['headless']\n";
        let fragment =
            include_str!("../../../../patches/knob-dialog/tests/emulator/fragments/check.toml");
        Scenario::from_text(&format!("{header}{fragment}")).unwrap();
        for action in [
            "knob-popup-expect-open",
            "knob-popup-expect-display",
            "knob-popup-expect-value",
        ] {
            let marker = format!("action = \"{action}\"");
            let at = fragment.find(&marker).unwrap();
            let start = fragment[..at].rfind("[[step]]").unwrap();
            let end = fragment[at..]
                .find("[[step]]")
                .map_or(fragment.len(), |offset| at + offset);
            let shortened = format!("{header}{}{}", &fragment[..start], &fragment[end..]);
            assert!(Scenario::from_text(&shortened).is_err(), "missing {action}");
        }
        assert!(
            Scenario::from_text(&format!(
                "{header}{}",
                fragment.replacen("wall_timeout_s = 120", "wall_timeout_s = 119", 1)
            ))
            .is_err()
        );
    }
}

#[cfg(test)]
mod l6_midi_probe_tests {
    fn parse(steps: &str, model: &str) -> Result<super::super::Scenario, String> {
        super::super::Scenario::from_text(&format!(
            "version=1\nname='probe'\nmodels=['{model}']\nbackends=['headless']\n{steps}"
        ))
    }
    fn step(action: &str) -> String {
        format!("[[step]]\naction='{action}'\nguest_timeout_ms=15000\nwall_timeout_s=180\n")
    }
    #[test]
    fn transaction_must_be_complete_ordered_and_terminal() {
        let steps = [
            "l6-midi-probe-begin",
            "l6-midi-probe-inject",
            "l6-midi-probe-expect-hook",
            "l6-midi-probe-expect-replies",
            "l6-midi-probe-finish",
        ]
        .map(step)
        .concat();
        assert!(parse(&steps, "l6").is_ok());
        assert!(parse(&steps, "l6max").is_err());
        assert!(parse(&steps.replace(&step("l6-midi-probe-expect-hook"), ""), "l6").is_err());
        assert!(parse(&steps.replace(&step("l6-midi-probe-finish"), ""), "l6").is_err());
        assert!(parse(&format!("{steps}{}", step("l6-midi-probe-begin")), "l6").is_err());
        assert!(
            parse(
                &steps
                    .replace("wall_timeout_s=180", "wall_timeout_s=179")
                    .replacen("wall_timeout_s=179", "wall_timeout_s=180", 1),
                "l6"
            )
            .is_err()
        );
    }
}

#[cfg(test)]
mod sd_inspection_tests {
    fn parse(steps: &str, fixture: &str) -> Result<super::super::Scenario, String> {
        super::super::Scenario::from_text(&format!(
            "version=1\nname='sd-inspection'\nmodels=['l6','l6max']\nbackends=['headless']\n{fixture}\n{steps}"
        ))
    }
    #[test]
    fn phases_are_adjacent_and_share_budget_and_fixture() {
        let begin =
            "[[step]]\naction='expect-sd-loader'\nguest_timeout_ms=10000\nwall_timeout_s=30\n";
        let end = "[[step]]\naction='expect-sd-module'\nloaded=true\nguest_timeout_ms=10000\nwall_timeout_s=30\n";
        let capture = "[[step]]\naction='capture-sd-module'\nloaded=true\nguest_timeout_ms=10000\nwall_timeout_s=30\n";
        let allocation = "[[step]]\naction='expect-sd-allocation'\nloaded=true\nguest_timeout_ms=10000\nwall_timeout_s=30\n";
        let chains = "[[step]]\naction='expect-sd-chains'\nloaded=true\nguest_timeout_ms=10000\nwall_timeout_s=30\n";
        let continuations = "[[step]]\naction='expect-sd-continuations'\nloaded=true\nguest_timeout_ms=10000\nwall_timeout_s=30\n";
        let fixture = "sd_patches=['midi-sync']";
        assert!(
            parse(
                &format!("{begin}{capture}{allocation}{chains}{continuations}{end}"),
                fixture
            )
            .is_ok()
        );
        assert!(
            parse(
                &format!("{begin}{capture}{allocation}{chains}{end}"),
                fixture
            )
            .is_err()
        );
        assert!(
            parse(
                &format!(
                    "{begin}{capture}{allocation}{chains}{}{end}",
                    continuations.replace("loaded=true", "loaded=false")
                ),
                fixture
            )
            .is_err()
        );
        assert!(parse(begin, fixture).is_err());
        assert!(parse(end, fixture).is_err());
        assert!(parse(&format!("{begin}{end}"), fixture).is_err());
        assert!(
            parse(
                &format!(
                    "{begin}{}{allocation}{chains}{continuations}{end}",
                    capture.replace("loaded=true", "loaded=false")
                ),
                fixture
            )
            .is_err()
        );
        assert!(
            parse(
                &format!("{begin}{capture}{capture}{allocation}{chains}{continuations}{end}"),
                fixture
            )
            .is_err()
        );
        assert!(
            parse(
                &format!("{begin}{capture}{allocation}{chains}{continuations}{end}"),
                ""
            )
            .is_err()
        );
        assert!(
            parse(
                &format!("{begin}[[step]]\naction='wait'\nuntil='boot-ready'\n{end}"),
                fixture
            )
            .is_err()
        );
        assert!(
            parse(
                &format!(
                    "{begin}{capture}{allocation}{chains}{continuations}{}",
                    end.replace("wall_timeout_s=30", "wall_timeout_s=29")
                ),
                fixture
            )
            .is_err()
        );
        assert!(
            parse(
                &format!(
                    "{begin}{capture}{allocation}{}{continuations}{end}",
                    chains.replace("loaded=true", "loaded=true\nchain_count=9")
                ),
                fixture
            )
            .is_err()
        );
    }
}

#[cfg(test)]
mod panel_key_scope_tests {
    #[test]
    fn rejects_incomplete_or_modified_panel_key_scope() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/emulator/cases/panel/l6max-input-smoke.toml");
        let source = std::fs::read_to_string(path).unwrap();
        assert!(crate::Scenario::parse(&source).is_ok());
        assert!(crate::Scenario::parse(&source.replace("target = 15", "target = 16")).is_ok());
        assert!(
            crate::Scenario::parse(&source.replace("word_index = 4", "word_index = 6")).is_err()
        );
        assert!(
            crate::Scenario::parse(&source.replace(
                "action = \"l6max-panel-key-end\"",
                "action = \"l6max-panel-key-begin\""
            ))
            .is_err()
        );
        assert!(
            crate::Scenario::parse(&source.replacen("pressed = false", "pressed = true", 1))
                .is_err()
        );
    }
}

#[cfg(test)]
mod din_usb_scope_tests {
    #[test]
    fn requires_bounded_paired_messages_and_complete_scope() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/emulator/cases/midi/l6-din-usb-forwarding.toml");
        let source = std::fs::read_to_string(path).unwrap();
        // Scenario::parse does not expand includes, so keep just the scope.
        let tail = source
            .split("[[step]]\naction = \"din-usb-begin\"")
            .nth(1)
            .unwrap();
        let text = format!(
            "version=1\nname='din'\nmodels=['l6']\nbackends=['headless']\n[[step]]\naction='din-usb-begin'\n{tail}"
        );
        assert!(crate::Scenario::parse(&text).is_ok());
        assert!(crate::Scenario::parse(&text.replace("cable = 0", "cable = 16")).is_err());
        assert!(
            crate::Scenario::parse(&text.replacen(
                "bytes = [179, 16, 32]",
                "bytes = [179, 16, 33]",
                1
            ))
            .is_err()
        );
        assert!(crate::Scenario::parse(&text.replace("din-usb-end", "din-usb-begin")).is_err());
        assert!(
            crate::Scenario::parse(&text.replace("models=['l6']", "models=['l6max']")).is_err()
        );
    }
}

fn validate_patch_parity(scenario: &Scenario) -> Result<(), String> {
    if scenario.models != [Model::L6, Model::L6max]
        || scenario.backends != [Backend::Headless]
        || scenario.step.iter().any(|step| step.setup_input)
    {
        return Err("patch parity requires both models and host-only headless steps".into());
    }
    for model in [Model::L6, Model::L6max] {
        let actual = scenario
            .step
            .iter()
            .filter(|step| step.applies_to(model))
            .map(|step| match &step.action {
                Action::PatchParityPrepare => Ok(("prepare", Vec::new())),
                Action::PatchParityFirmware { patches } => Ok(("firmware", patches.clone())),
                Action::PatchParityLoader { patches } => Ok(("loader", patches.clone())),
                Action::PatchParityUnsupported { patch } => {
                    Ok(("unsupported", vec![patch.clone()]))
                }
                Action::PatchParityFinish => Ok(("finish", Vec::new())),
                _ => Err("patch parity cannot include other actions"),
            })
            .collect::<Result<Vec<_>, _>>()?;
        let mut expected = vec![("prepare", Vec::new())];
        if model == Model::L6 {
            expected.push(("firmware", vec!["midi-sync".into()]));
            expected.push(("unsupported", vec!["knob-dialog".into()]));
            expected.push(("unsupported", vec!["din-mixer-control".into()]));
        } else {
            let patches = &patch_core::WASM_PATCHES;
            for mask in 1..(1usize << patches.len()) {
                expected.push((
                    "firmware",
                    patches
                        .iter()
                        .enumerate()
                        .filter(|(i, _)| mask & (1 << i) != 0)
                        .map(|(_, p)| p.to_string())
                        .collect(),
                ));
            }
            for mask in 0..(1usize << patches.len()) {
                expected.push((
                    "loader",
                    patches
                        .iter()
                        .enumerate()
                        .filter(|(i, _)| mask & (1 << i) != 0)
                        .map(|(_, p)| p.to_string())
                        .collect(),
                ));
            }
        }
        expected.push(("finish", Vec::new()));
        if actual != expected {
            return Err(format!(
                "patch parity profiles for {model} are incomplete or out of order"
            ));
        }
    }
    Ok(())
}

fn validate_power_steps(scenario: &Scenario) -> Result<(), String> {
    for model in &scenario.models {
        let steps: Vec<_> = scenario
            .step
            .iter()
            .filter(|step| step.applies_to(*model))
            .collect();
        let mut i = 0;
        while i < steps.len() {
            if let Action::PowerBegin { hold_ms, powered } = steps[i].action {
                let length = if powered { 4 } else { 5 };
                let group = steps
                    .get(i..i + length)
                    .ok_or("incomplete power transaction")?;
                if hold_ms == 0
                    || hold_ms > 60_000
                    || group.iter().any(|step| {
                        step.wall_timeout_s != group[0].wall_timeout_s
                            || step.guest_timeout_ms != group[0].guest_timeout_ms
                    })
                    || !matches!(group[1].action, Action::PowerWaitAck)
                    || !matches!(group[length - 1].action, Action::PowerEnd)
                {
                    return Err("invalid power transaction sequence or budgets".into());
                }
                if powered {
                    if !matches!(group[2].action, Action::PowerObserveOn { guest_ms } if guest_ms == hold_ms + 1000 && guest_ms <= group[0].guest_timeout_ms)
                    {
                        return Err(
                            "powered observation must cover the hold plus 1000 panel guest ms"
                                .into(),
                        );
                    }
                } else {
                    let expected = if *model == Model::L6 {
                        (Some(300), Some(800))
                    } else {
                        (None, None)
                    };
                    if !matches!(group[2].action, Action::PowerWaitOff { min_cutoff_ms, max_cutoff_ms } if (min_cutoff_ms, max_cutoff_ms) == expected)
                        || !matches!(
                            group[3].action,
                            Action::PowerObserveStopped {
                                settle_wall_ms: 100,
                                observe_wall_ms: 200
                            }
                        )
                    {
                        return Err(
                            "power-off requires model cutoff and stopped-clock observations".into(),
                        );
                    }
                }
                i += length;
            } else if matches!(
                steps[i].action,
                Action::PowerWaitAck
                    | Action::PowerObserveOn { .. }
                    | Action::PowerWaitOff { .. }
                    | Action::PowerObserveStopped { .. }
                    | Action::PowerEnd
            ) {
                return Err("power step outside a complete transaction".into());
            } else {
                i += 1;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod menu_scope_tests {
    fn parse(steps: &str, model: &str) -> Result<super::super::Scenario, String> {
        super::super::Scenario::from_text(&format!(
            "version=1\nname='menu'\nmodels=['{model}']\nbackends=['headless']\n{steps}"
        ))
    }
    #[test]
    fn capture_and_ordered_assertion_are_adjacent_and_l6max_only() {
        let capture = "[[step]]\naction='capture-menu'\nmenu='system'\n";
        let assertion = "[[step]]\naction='expect-menu-items'\nitems=['Patches']\n";
        assert!(parse(&format!("{capture}{assertion}"), "l6max").is_ok());
        assert!(parse(capture, "l6max").is_err());
        assert!(parse(assertion, "l6max").is_err());
        assert!(parse(&format!("{capture}{assertion}"), "l6").is_err());
        assert!(
            parse(
                &format!("{capture}[[step]]\naction='wait'\nguest_ms=1\n{assertion}"),
                "l6max"
            )
            .is_err()
        );
    }
}

#[cfg(test)]
mod effect_state_scope_tests {
    fn parse(steps: &str) -> Result<super::super::Scenario, String> {
        super::super::Scenario::from_text(&format!(
            "version=1\nname='modes'\nmodels=['l6max']\nbackends=['headless']\n{steps}"
        ))
    }
    #[test]
    fn wait_requires_matching_type_and_processor_assertions() {
        let wait = "[[step]]\naction='wait-effect-slot'\nexpected_effect_type=1\nexpected_processor_type=1\n";
        let capture = "[[step]]\naction='capture-effect-state'\n";
        let effect = "[[step]]\naction='expect-effect-type'\nvalue=1\n";
        let processor = "[[step]]\naction='expect-effect-processor'\nvalue=1\n";
        let end = "[[step]]\naction='effect-state-end'\n";
        assert!(parse(&format!("{wait}{capture}{effect}{processor}{end}")).is_ok());
        assert!(parse(&format!("{wait}{capture}{effect}{end}")).is_err());
        assert!(
            parse(&format!(
                "{wait}{capture}{effect}{}{end}",
                processor.replace("value=1", "value=2")
            ))
            .is_err()
        );
        assert!(
            parse(&format!(
                "{}{capture}{effect}{processor}{end}",
                wait.replace(
                    "expected_processor_type=1",
                    "expected_processor_type=1\nrequire_output=true"
                )
            ))
            .is_err()
        );
    }
    #[test]
    fn wait_requires_matching_name_and_output_checks() {
        let wait = "[[step]]\naction='wait-effect-slot'\nexpected_effect_type=1\nexpected_processor_type=1\nexpected_effect='velvet'\nrequire_output=true\n";
        let capture = "[[step]]\naction='capture-effect-state'\n";
        let basic = "[[step]]\naction='expect-effect-type'\nvalue=1\n[[step]]\naction='expect-effect-processor'\nvalue=1\n";
        let name = "[[step]]\naction='expect-effect-name'\nname='velvet'\n";
        let output = "[[step]]\naction='expect-effect-output'\n";
        let end = "[[step]]\naction='effect-state-end'\n";
        assert!(parse(&format!("{wait}{capture}{basic}{name}{output}{end}")).is_ok());
        assert!(parse(&format!("{wait}{capture}{basic}{name}{end}")).is_err());
        assert!(parse(&format!("{wait}{capture}{basic}{output}{end}")).is_err());
        assert!(
            parse(&format!(
                "{wait}{capture}{basic}{}{output}{end}",
                name.replace("velvet", "plateau")
            ))
            .is_err()
        );
    }
    #[test]
    fn optional_checks_require_their_fields_in_the_capture() {
        let capture = "[[step]]\naction='capture-effect-state'\nedits=true\ndialog=true\n";
        let edits = "[[step]]\naction='expect-effect-edits'\nminimum=1\n";
        let dialog = "[[step]]\naction='expect-effect-dialog'\nactive=true\n";
        let end = "[[step]]\naction='effect-state-end'\n";
        assert!(parse(&format!("{capture}{edits}{dialog}{end}")).is_ok());
        assert!(
            parse(&format!(
                "{}{edits}{dialog}{end}",
                capture.replace("edits=true", "edits=false")
            ))
            .is_err()
        );
        assert!(
            parse(&format!(
                "{}{edits}{dialog}{end}",
                capture.replace("dialog=true", "dialog=false")
            ))
            .is_err()
        );
        assert!(parse(&format!("{capture}{edits}{edits}{end}")).is_err());
    }
    #[test]
    fn snapshot_requires_unique_adjacent_assertions_and_shared_deadlines() {
        let capture = "[[step]]\naction='capture-effect-state'\n";
        let panel = "[[step]]\naction='expect-effect-panel-mode'\nvalue=6\n";
        let edit = "[[step]]\naction='expect-effect-edit-mode'\nenabled=true\n";
        let end = "[[step]]\naction='effect-state-end'\n";
        assert!(parse(&format!("{capture}{panel}{edit}{end}")).is_ok());
        assert!(parse(&format!("{capture}{edit}{end}")).is_ok());
        assert!(parse(&format!("{capture}{panel}{panel}{end}")).is_err());
        assert!(parse(&format!("{capture}{end}")).is_err());
        assert!(parse(&format!("{capture}{panel}")).is_err());
        assert!(
            parse(&format!(
                "{capture}{}{}",
                panel.replace("value=6", "value=6\nwall_timeout_s=1"),
                end
            ))
            .is_err()
        );
    }
}

#[cfg(test)]
mod effect_ring_scope_tests {
    fn parse(steps: &str, model: &str) -> Result<super::super::Scenario, String> {
        super::super::Scenario::from_text(&format!(
            "version=1\nname='ring'\nmodels=['{model}']\nbackends=['headless']\n{steps}"
        ))
    }
    #[test]
    fn baseline_requires_matching_encoder_and_guest_generation() {
        let capture = "[[step]]\naction='capture-effect-ring'\nencoder=1\n";
        let assertion = "[[step]]\naction='expect-effect-ring-changed'\nencoder=1\n";
        assert!(parse(&format!("{capture}{assertion}"), "l6max").is_ok());
        assert!(parse(capture, "l6max").is_err());
        assert!(parse(assertion, "l6max").is_err());
        assert!(
            parse(
                &format!("{capture}{}", assertion.replace("encoder=1", "encoder=2")),
                "l6max"
            )
            .is_err()
        );
        assert!(
            parse(
                &format!("{capture}[[step]]\naction='restart'\n{assertion}"),
                "l6max"
            )
            .is_err()
        );
        assert!(
            parse(
                &format!("{capture}[[step]]\naction='sd-card'\ninserted=false\n{assertion}"),
                "l6max"
            )
            .is_err()
        );
        assert!(parse(&format!("{capture}{assertion}"), "l6").is_err());
    }
}

#[cfg(test)]
mod usb_enumeration_scope_tests {
    fn parse(steps: &str) -> Result<super::super::Scenario, String> {
        super::super::Scenario::from_text(&format!(
            "version=1\nname='usb'\nmodels=['l6max']\nbackends=['headless']\n{steps}"
        ))
    }
    #[test]
    fn readiness_and_enumeration_require_adjacency_and_identical_budgets() {
        let ready =
            "[[step]]\naction='usb-controller-ready'\nguest_timeout_ms=60000\nwall_timeout_s=300\n";
        let enumerate =
            "[[step]]\naction='usb-enumerate'\nguest_timeout_ms=60000\nwall_timeout_s=300\n";
        assert!(parse(&format!("{ready}{enumerate}")).is_ok());
        assert!(parse(ready).is_err());
        assert!(parse(enumerate).is_err());
        assert!(parse(&format!("{ready}{}", enumerate.replace("60000", "60001"))).is_err());
        assert!(
            parse(&format!(
                "{ready}[[step]]\naction='wait'\nguest_ms=1\n{enumerate}"
            ))
            .is_err()
        );
    }
}
