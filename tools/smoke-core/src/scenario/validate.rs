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
        if let Some(fixture) = value.update_fixture {
            let fixture_model = if fixture.is_l6() {
                Model::L6
            } else {
                Model::L6max
            };
            if value.models != [fixture_model]
                || value.backends != [Backend::Headless]
                || value.state.is_some()
                || value.card.is_some()
                || (!fixture.is_patched() && !value.update_patches.is_empty())
                || (fixture.is_patched()
                    && (value.update_patches.len() != 1
                        || value.update_patches.get(&fixture_model).map(Vec::as_slice)
                            != Some(&["midi-sync".to_string()][..])))
                || !value.patches.is_empty()
                || value.sd_patches.is_some()
                || !value.installed_patches.is_empty()
                || !value.installed_sd_patches.is_empty()
                || !value.effect_replacements.is_empty()
                || (fixture != UpdateFixture::PatchedMidi
                    && value
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
                UpdateFixture::L6Stock | UpdateFixture::L6PatchedMidi => {
                    if value.update_goal != Some(UpdateGoal::Install) {
                        return Err("L6 update requires the full install goal".into());
                    }
                    let prefix = if fixture == UpdateFixture::L6PatchedMidi {
                        &actions[..actions.len().saturating_sub(7)]
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
                                Action::LifecycleStop
                            ]
                        )
                    {
                        return Err(
                            "patched L6 update requires complete terminal MIDI verification".into(),
                        );
                    }
                    if fixture == UpdateFixture::L6PatchedMidi {
                        let tail = &value.step[value.step.len() - 7..];
                        if tail[1].wall_timeout_s != 180
                            || tail[2..6].iter().any(|step| {
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
                UpdateFixture::PatchedMidi => {
                    if value.update_goal != Some(UpdateGoal::Install) {
                        return Err("patched update requires the full install goal".into());
                    }
                    let semantic = actions
                        .into_iter()
                        .filter(|action| {
                            !matches!(action, Action::WaitInputApplied | Action::Delay { .. })
                        })
                        .collect::<Vec<_>>();
                    if !matches!(semantic.as_slice(), [
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
                        Action::EndWallDeadline,
                        Action::WaitUsbController,
                        Action::DirectUsbMidiRoundTrip { cable: 1, expect_ring_change: true },
                        Action::HostMidiSyncBegin,
                        Action::HostMidiDrain { quiet_wall_ms: 500 },
                        Action::HostMidiSendCc { channel: 0, controller: 127, value: 127 },
                        Action::HostMidiExpectSync,
                        Action::HostMidiEnd,
                        Action::LifecycleStop
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
                                | Action::DirectUsbMidiRoundTrip { .. }
                                | Action::HostMidiSyncBegin
                                | Action::HostMidiDrain { .. }
                                | Action::HostMidiSendCc { .. }
                                | Action::HostMidiExpectSync
                                | Action::HostMidiEnd
                        ) && (step.guest_timeout_ms < 180000 || step.wall_timeout_s < 300)
                    }) {
                        return Err("post-update MIDI verification needs the original guest and wall budgets".into());
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
                if fixture == UpdateFixture::PatchedMidi {
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
                UpdateFixture::PatchedMidi | UpdateFixture::L6PatchedMidi => matches!(
                    value.step.last().map(|s| &s.action),
                    Some(Action::LifecycleStop)
                ),
                UpdateFixture::L6Stock => matches!(
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
        if value.backends.is_empty() {
            return Err("backends must contain headless and/or gui".into());
        }
        let mut checkpoints = std::collections::BTreeSet::new();
        for model in &value.models {
            let mut sd_inspection: Option<(u32, u64)> = None;
            let mut sync: Option<(u8, u32, u64)> = None;
            let mut l6_probe: Option<(u8, u32, u64)> = None;
            let mut l6_probe_finished = false;
            let mut probe_budget = None;
            let mut probe_stimulus = None;
            let mut probe_ring = None;
            let mut probe_tap = None;
            for step in value.step.iter().filter(|step| step.applies_to(*model)) {
                match (&step.action, sd_inspection) {
                    (Action::ExpectSdLoader, None) => sd_inspection = Some((step.guest_timeout_ms, step.wall_timeout_s)),
                    (Action::ExpectSdModule { .. }, Some((guest, wall))) if (guest, wall) == (step.guest_timeout_ms, step.wall_timeout_s) => sd_inspection = None,
                    (Action::ExpectSdLoader | Action::ExpectSdModule { .. }, _) => return Err("SD inspection requires adjacent loader/module steps with matching timeouts".into()),
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
                        l6_probe = None;
                        l6_probe_finished = true;
                    }
                    (
                        Action::L6MidiProbeBegin
                        | Action::L6MidiProbeInject
                        | Action::L6MidiProbeExpectHook
                        | Action::L6MidiProbeExpectReplies,
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
                Action::KnobDialogPreflightDescriptor => "knob-dialog-preflight-begin",
                Action::KnobDialogPreflightTextAlias => "knob-dialog-preflight-descriptor",
                Action::KnobDialogPreflightLayerCollision => "knob-dialog-preflight-text-alias",
                Action::KnobDialogPreflightWidgetLayer => "knob-dialog-preflight-layer-collision",
                Action::KnobDialogPreflightBackgroundLayer => "knob-dialog-preflight-widget-layer",
                Action::KnobDialogCallbackRace => "knob-dialog-preflight-background-layer",
                Action::KnobDialogFirstActivation => "knob-dialog-callback-race",
                Action::KnobDialogActiveDisplay => "knob-dialog-first-activation",
                Action::KnobDialogLevelDemo => "knob-dialog-active-display",
                Action::KnobDialogTaskContexts => "knob-dialog-level-demo",
                Action::KnobDialogOverlayInvariants => "knob-dialog-task-contexts",
                Action::KnobDialogBitmapZero => "knob-dialog-overlay-invariants",
                Action::KnobDialogActiveOverlay => "knob-dialog-bitmap-zero",
                Action::KnobDialogLockedUpdate => "knob-dialog-active-overlay",
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
                        "knob-dialog-preflight-descriptor" => {
                            matches!(previous.action, Action::KnobDialogPreflightDescriptor)
                        }
                        "knob-dialog-preflight-text-alias" => {
                            matches!(previous.action, Action::KnobDialogPreflightTextAlias)
                        }
                        "knob-dialog-preflight-layer-collision" => {
                            matches!(previous.action, Action::KnobDialogPreflightLayerCollision)
                        }
                        "knob-dialog-preflight-widget-layer" => {
                            matches!(previous.action, Action::KnobDialogPreflightWidgetLayer)
                        }
                        "knob-dialog-preflight-background-layer" => {
                            matches!(previous.action, Action::KnobDialogPreflightBackgroundLayer)
                        }
                        "knob-dialog-callback-race" => {
                            matches!(previous.action, Action::KnobDialogCallbackRace)
                        }
                        "knob-dialog-first-activation" => {
                            matches!(previous.action, Action::KnobDialogFirstActivation)
                        }
                        "knob-dialog-active-display" => {
                            matches!(previous.action, Action::KnobDialogActiveDisplay)
                        }
                        "knob-dialog-level-demo" => {
                            matches!(previous.action, Action::KnobDialogLevelDemo)
                        }
                        "knob-dialog-task-contexts" => {
                            matches!(previous.action, Action::KnobDialogTaskContexts)
                        }
                        "knob-dialog-overlay-invariants" => {
                            matches!(previous.action, Action::KnobDialogOverlayInvariants)
                        }
                        "knob-dialog-bitmap-zero" => {
                            matches!(previous.action, Action::KnobDialogBitmapZero)
                        }
                        "knob-dialog-active-overlay" => {
                            matches!(previous.action, Action::KnobDialogActiveOverlay)
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
                        _ => matches!(previous.action, Action::KnobDialogLockedUpdate),
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
                if subsequent.len() < 87
                    || !matches!(subsequent[0].action, Action::KnobDialogPreflightDescriptor)
                    || !matches!(subsequent[1].action, Action::KnobDialogPreflightTextAlias)
                    || !matches!(
                        subsequent[2].action,
                        Action::KnobDialogPreflightLayerCollision
                    )
                    || !matches!(subsequent[3].action, Action::KnobDialogPreflightWidgetLayer)
                    || !matches!(
                        subsequent[4].action,
                        Action::KnobDialogPreflightBackgroundLayer
                    )
                    || !matches!(subsequent[5].action, Action::KnobDialogCallbackRace)
                    || !matches!(subsequent[6].action, Action::KnobDialogFirstActivation)
                    || !matches!(subsequent[7].action, Action::KnobDialogActiveDisplay)
                    || !matches!(subsequent[8].action, Action::KnobDialogLevelDemo)
                    || !matches!(subsequent[9].action, Action::KnobDialogTaskContexts)
                    || !matches!(subsequent[10].action, Action::KnobDialogOverlayInvariants)
                    || !matches!(subsequent[11].action, Action::KnobDialogBitmapZero)
                    || !matches!(subsequent[12].action, Action::KnobDialogActiveOverlay)
                    || !matches!(subsequent[13].action, Action::KnobDialogLockedUpdate)
                    || !valid_knob_mode_steps(&subsequent[14..subsequent.len().saturating_sub(63)])
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
        for step in &value.step {
            if let Action::KnobDialogAdaptive { plan } = &step.action {
                plan.validate()?;
            }
            if matches!(step.action, Action::EditorPadAssignments) && !usb_descriptors_seen {
                return Err(
                    "editor-pad-assignments requires a preceding usb-descriptors step".into(),
                );
            }
            if matches!(step.action, Action::UsbDescriptors) {
                usb_descriptors_seen = true;
            }
            match &step.action {
                Action::EditorPadAssignments => pad_assignments_seen = true,
                Action::L6maxUiPadVerify { pad } => {
                    if !usb_descriptors_seen {
                        return Err(
                            "l6max-ui-pad-verify requires a preceding usb-descriptors step".into(),
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
                Action::EditorPadPlayback { pad } => {
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
                Action::SoundPadPlayback {
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
                Action::L6RecorderPlayback { target } => {
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
            if let Action::PowerPress { hold_ms, .. } = &step.action {
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
            if let Action::PanelEvent {
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
            if let Action::L6DinEventDispatch { group, .. } = &step.action {
                if *group > 15 {
                    return Err("l6-din-event-dispatch requires a group from 0 to 15".into());
                }
            }
            if let Action::L6maxRecorderPlayback { target } = step.action {
                if target != 4 {
                    return Err("l6max-recorder-playback requires MAIN target 4".into());
                }
            }

            if let Action::L6MainGpioEvent { target, event } = step.action
                && (!matches!(
                    (target, event),
                    (48, 0x19) | (49, 0x15) | (50, 0x16) | (51, 0x17) | (52, 0x18)
                ))
            {
                return Err(
                    "l6-main-gpio-event requires L6 target/event pairs 48/0x19 or 49–52/0x15–0x18"
                        .into(),
                );
            }

            if let Action::ExpectMenu { menu, .. } = &step.action
                && !["main", "project", "sound-pad", "mixer", "system"].contains(&menu.as_str())
            {
                return Err(format!("expect-menu names an unknown menu {menu:?}"));
            }
            if matches!(step.action, Action::FirmwareUpdate)
                && (value.models.is_empty() || value.step.len() != 1)
            {
                return Err("firmware-update is a standalone L6/L6max scenario operation".into());
            }
            if matches!(step.action, Action::WasmPatch)
                && (value.models.is_empty() || value.step.len() != 1)
            {
                return Err("wasm-patch is a standalone L6/L6max scenario operation".into());
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
            if let Action::ExpectSdModule {
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
                Action::SaveScene { scene } if !(1..=3).contains(scene) => {
                    return Err("scene index must be between 1 and 3".into());
                }
                Action::SaveScene { .. }
                    if (value.models.iter().any(|m| *m == Model::L6max)
                        && (step.models.is_empty()
                            || step.models.iter().any(|m| *m == Model::L6max))) =>
                {
                    return Err("save-scene currently supports L6 only".into());
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
                Action::EffectParameterEdit { encoder, steps }
                    if *encoder == 0
                        || *encoder > 8
                        || *steps == 0
                        || steps.unsigned_abs() > 120 =>
                {
                    return Err(
                        "effect-parameter-edit needs encoder 1..8 and nonzero steps up to 120"
                            .into(),
                    );
                }
                Action::EffectReplacementState { .. } => {}
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
            if let Action::DirectUsbMidiRoundTrip { cable, .. } = &step.action
                && *cable > 15
            {
                return Err("direct-usb-midi-round-trip cable must be 0..15".into());
            }
        }
        Ok(())
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
        ]
        .map(step)
        .concat();
        assert!(parse(&steps, "l6").is_ok());
        assert!(parse(&steps, "l6max").is_err());
        assert!(parse(&steps.replace(&step("l6-midi-probe-expect-hook"), ""), "l6").is_err());
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
        let fixture = "sd_patches=['midi-sync']";
        assert!(parse(&format!("{begin}{end}"), fixture).is_ok());
        assert!(parse(begin, fixture).is_err());
        assert!(parse(end, fixture).is_err());
        assert!(parse(&format!("{begin}{end}"), "").is_err());
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
                    "{begin}{}",
                    end.replace("wall_timeout_s=30", "wall_timeout_s=29")
                ),
                fixture
            )
            .is_err()
        );
        assert!(
            parse(
                &format!(
                    "{begin}{}",
                    end.replace("loaded=true", "loaded=true\nchain_count=9")
                ),
                fixture
            )
            .is_err()
        );
    }
}
