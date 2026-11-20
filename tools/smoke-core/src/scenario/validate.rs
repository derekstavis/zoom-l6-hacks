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
        if let Some(mode) = &value.sd_bundle_mode {
            if ![
                "valid",
                "absent",
                "corrupt",
                "corrupt-first",
                "ordered-chain",
                "composed-module",
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
            if patches.iter().any(|name| {
                firmware_patch_core::patch_package(name)
                    .is_none_or(|package| package.kind != "patch")
            }) {
                return Err("unknown SD patch name".into());
            }
            for model in &value.models {
                let firmware_model = match model {
                    Model::L6 => firmware_patch_core::FirmwareModel::L6,
                    Model::L6max => firmware_patch_core::FirmwareModel::L6max,
                };
                if let Some(name) = patches
                    .iter()
                    .find(|name| !firmware_patch_core::supports_sd_patch(name, firmware_model))
                {
                    return Err(format!(
                        "patch {name} does not support SD loading on {model}"
                    ));
                }
                if !value.effect_replacements.is_empty()
                    && firmware_model != firmware_patch_core::FirmwareModel::L6max
                {
                    return Err("effect_replacements are currently supported on L6max only".into());
                }
            }
            let replacements = value
                .effect_replacements
                .iter()
                .map(|(slot, effect)| (slot.clone(), effect.clone()))
                .collect::<Vec<_>>();
            firmware_patch_core::effect_replacement_packages(
                &replacements,
                firmware_patch_core::patch_catalog(),
            )
            .map_err(|error| format!("invalid effect_replacements: {error}"))?;
        }
        if value.backends.is_empty() {
            return Err("backends must contain headless and/or gui".into());
        }
        let mut checkpoints = std::collections::BTreeSet::new();
        let mut usb_descriptors_seen = false;
        let mut pad_assignments_seen = false;
        for step in &value.step {
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
                Action::L6maxUiPadAssignment { pad } => {
                    if !usb_descriptors_seen {
                        return Err(
                            "l6max-ui-pad-assignment requires a preceding usb-descriptors step"
                                .into(),
                        );
                    }
                    if *pad > 3 {
                        return Err("l6max-ui-pad-assignment pad must be 0..3".into());
                    }
                    if value.models.as_slice() != [Model::L6max]
                        || step.models.iter().any(|model| *model != Model::L6max)
                    {
                        return Err(
                            "l6max-ui-pad-assignment scenarios must be scoped to L6max".into()
                        );
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
            if let Action::UsbSysex { requests } = &step.action {
                if requests.is_empty() {
                    return Err("usb-sysex needs at least one request".into());
                }
                for request in requests {
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
                        return Err(
                            "usb-sysex reject_prefixes must not contain empty prefixes".into()
                        );
                    }
                    if request
                        .reply_timeout_ms
                        .is_some_and(|timeout| timeout == 0 || timeout > step.guest_timeout_ms)
                    {
                        return Err("usb-sysex reply_timeout_ms must be positive and fit the step guest timeout".into());
                    }
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
            if let Action::ExpectMemory { symbol, bytes } = &step.action {
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
            if let Action::ExpectSdPatch {
                loaded,
                chain_count,
            } = &step.action
                && (value.sd_patches.is_none()
                    || *loaded && value.sd_patches.as_ref().is_some_and(Vec::is_empty)
                    || chain_count.is_some_and(|count| count == 0 || count > 8))
            {
                return Err("expect-patch-loader requires an Patch loader fixture (and a selected patch when loaded=true)".into());
            }
            if let Action::HoldChord {
                controls,
                duration_ms,
            } = &step.action
            {
                if controls.is_empty()
                    || controls.len() > 16
                    || *duration_ms == 0
                    || *duration_ms > step.guest_timeout_ms
                {
                    return Err(
                        "hold-chord requires 1..16 controls and a duration within its deadline"
                            .into(),
                    );
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
                Action::ExpectDisplayImage { image } => {
                    spec_path(image)?;
                    if image.extension().is_none_or(|extension| extension != "png") {
                        return Err("expect-display-image needs a PNG image".into());
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
                Action::ExpectDisplay { checkpoint, .. }
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
                        && until != "mixer-ready" =>
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
                Action::Encoder { channel, steps, .. }
                    if *steps == 0 || steps.unsigned_abs() > 120 || *channel == 0 =>
                {
                    return Err(
                        "encoder channel must be positive, steps must be nonzero and at most 120"
                            .into(),
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
            if let Action::Encoder { channel, .. }
            | Action::ExpectRingLevel { channel, .. }
            | Action::ExpectRing { channel, .. }
            | Action::ExpectMute { channel } = &step.action
            {
                for model in &value.models {
                    if step.applies_to(*model)
                        && !(1..=if *model == Model::L6 { 6 } else { 8 }).contains(channel)
                    {
                        return Err(format!("channel {channel} is not available on {model}"));
                    }
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
