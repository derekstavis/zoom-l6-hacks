//! Reusable input, capture and observation primitives for bounded scenarios.
use super::*;

// Embedded at build time so the staged binary's hash identifies the lifecycle
// sequence even if the checkout changes or disappears during the run.
fn valid_artifact(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

#[derive(Debug, PartialEq, Eq)]
enum InputWait {
    Applied,
    Pending,
    Expired,
}

fn input_wait(applied_before: u64, applied_now: u64, expired: bool) -> InputWait {
    if applied_now > applied_before {
        InputWait::Applied
    } else if expired {
        InputWait::Expired
    } else {
        InputWait::Pending
    }
}

#[derive(Debug, PartialEq, Eq)]
enum KnobImageWait {
    Pending,
    Ready,
    Expired,
}

fn knob_image_wait(
    different_pixels: Option<usize>,
    fresh_clock: bool,
    main_elapsed_ms: u32,
    main_timeout_ms: u32,
    shared_expired: bool,
    wall_expired: bool,
) -> KnobImageWait {
    if shared_expired || wall_expired || main_elapsed_ms >= main_timeout_ms {
        KnobImageWait::Expired
    } else if fresh_clock && different_pixels.is_none_or(|count| count == 0) {
        KnobImageWait::Ready
    } else {
        KnobImageWait::Pending
    }
}

fn mode_operation_matches(operation: &Operation) -> bool {
    matches!(
        operation,
        Operation::EnqueueTap { .. }
            | Operation::WaitInputApplied
            | Operation::WaitKnobParameterMode { .. }
            | Operation::EnqueueEncoder {
                midi_transaction: false,
                ..
            }
            | Operation::WaitKnobDisplayImage { .. }
            | Operation::CaptureDisplay { .. }
            | Operation::ExpectCapturedDisplayImage { .. }
            | Operation::KnobDialogExpectHeap
            | Operation::KnobDialogModeEnd
    )
}

pub(super) fn wait_parameter_mode(
    session: &Session,
    control: &l6ctl::OwnedControl<'_>,
    mode: u8,
    guest_timeout_ms: u32,
    wall_timeout_s: u64,
) -> Result<(), Box<dyn std::error::Error>> {
    wait_parameter_mode_checked(
        session,
        control,
        mode,
        guest_timeout_ms,
        wall_timeout_s,
        || Ok(()),
    )
}

pub(super) fn wait_parameter_mode_checked(
    session: &Session,
    control: &l6ctl::OwnedControl<'_>,
    mode: u8,
    guest_timeout_ms: u32,
    wall_timeout_s: u64,
    mut check: impl FnMut() -> Result<(), Box<dyn std::error::Error>>,
) -> Result<(), Box<dyn std::error::Error>> {
    check()?;
    let reader = control.knob_parameter_reader()?;
    let start = control.input_status(l6ctl::InputSource::Panel)?.guest_ms;
    let deadline = Instant::now() + Duration::from_secs(wall_timeout_s.min(180));
    loop {
        check()?;
        session.poll()?;
        let actual = reader.parameter_mode()?;
        if actual == mode {
            return Ok(());
        }
        let now = control.input_status(l6ctl::InputSource::Panel)?.guest_ms;
        if now.wrapping_sub(start) >= guest_timeout_ms.min(10_000) || Instant::now() >= deadline {
            return Err(format!("parameter mode did not become {mode}; found {actual}").into());
        }
        thread::sleep(Duration::from_millis(5));
    }
}

fn commit_level_capture(
    mut check: impl FnMut() -> Result<(), Box<dyn std::error::Error>>,
    save: impl FnOnce() -> Result<(), Box<dyn std::error::Error>>,
    advance: impl FnOnce() -> Result<(), Box<dyn std::error::Error>>,
) -> Result<(), Box<dyn std::error::Error>> {
    check()?;
    save()?;
    check()?;
    advance()
}

fn save_captured_display(
    capture: &CapturedDisplay,
    logs: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let native = image::GrayImage::from_fn(128, 64, |x, y| {
        let index = ((63 - y as usize) / 8) * 128 + x as usize;
        image::Luma([
            if capture.frame[index] & (1 << ((63 - y as usize) % 8)) != 0 {
                255
            } else {
                0
            },
        ])
    });
    image::DynamicImage::ImageLuma8(native)
        .resize(768, 384, image::imageops::FilterType::Nearest)
        .save(logs.join(format!("{}.png", capture.artifact)))?;
    Ok(())
}

impl State {
    pub(super) fn release_navigation_capture(
        &mut self,
        _options: &Options,
    ) -> Result<Value, Box<dyn std::error::Error>> {
        let capture = self
            .captured_display
            .as_ref()
            .ok_or("no display capture to save")?;
        let context = self
            .knob_dialog_preflight
            .as_mut()
            .ok_or("missing knob preflight")?;
        if !matches!(final_phase(context), 45 | 48) {
            return Err("navigation save out of order".into());
        }
        let artifact = capture.artifact.clone();
        note_navigation_save(context)?;
        self.captured_display = None;
        Ok(json!({"artifact":artifact}))
    }

    pub(super) fn run_mode_primitive(
        &mut self,
        session: &Session,
        options: &Options,
        operation: Operation,
        guest_timeout_ms: u32,
        wall_timeout_s: u64,
        shared: Option<(Instant, u32, u32, u64, u16)>,
    ) -> Result<Value, Box<dyn std::error::Error>> {
        if let Some(scope) = &self.knob_mode {
            if !mode_operation_matches(&operation) {
                return Err(format!(
                    "unexpected operation in knob-dialog mode {}: {operation:?}",
                    scope.scene
                )
                .into());
            }
        }
        let control = l6ctl::OwnedControl::new(&session.engine);
        match operation {
            Operation::KnobDialogModeBegin { scene } => {
                if self.knob_mode.is_some()
                    || self.pending_input.is_some()
                    || self.captured_display.is_some()
                {
                    return Err("a mode or input/display observation is already active".into());
                }
                if !valid_artifact(&scene) {
                    return Err("invalid mode scene name".into());
                }
                let context = self
                    .knob_dialog_preflight
                    .as_ref()
                    .ok_or("missing knob-dialog preflight")?;
                let index = begin_knob_mode(context)?;
                if index == 0 {
                    self.knob_heap_reader = Some(control.heap_counter_reader()?);
                } else if self.knob_heap_reader.is_none() {
                    return Err("missing guarded heap reader".into());
                }
                self.knob_mode = Some(KnobModeScope {
                    index,
                    scene: scene.clone(),
                    selected_mode: None,
                });
                Ok(json!({"scene":scene}))
            }
            Operation::EnqueueTap { target, hold_ms } => {
                if self.pending_input.is_some() {
                    return Err("input acknowledgement already pending".into());
                }
                let source = l6ctl::button_source(target).ok_or("invalid button target")?;
                let before = control.input_status(source)?;
                control.tap(target, hold_ms)?;
                self.pending_input = Some(PendingInput {
                    source,
                    applied: before.applied,
                    deadline: Instant::now() + Duration::from_secs(10),
                    kind: PendingInputKind::Tap,
                });
                Ok(json!({"target":target, "hold_ms":hold_ms, "applied_before":before.applied}))
            }
            Operation::EnqueueEncoder {
                channel,
                steps,
                midi_transaction: false,
            } => {
                if self.pending_input.is_some() {
                    return Err("input acknowledgement already pending".into());
                }
                let source = l6ctl::InputSource::Panel;
                let before = control.input_status(source)?;
                control.encoder(channel as u32, steps)?;
                self.pending_input = Some(PendingInput {
                    source,
                    applied: before.applied,
                    deadline: Instant::now() + Duration::from_secs(10),
                    kind: PendingInputKind::Encoder,
                });
                Ok(json!({"channel":channel, "steps":steps, "applied_before":before.applied}))
            }
            Operation::WaitInputApplied => {
                let pending = self
                    .pending_input
                    .as_ref()
                    .ok_or("no input acknowledgement pending")?;
                loop {
                    session.poll()?;
                    let status = control.input_status(pending.source)?;
                    match input_wait(pending.applied, status.applied, Instant::now() >= pending.deadline) {
                        InputWait::Applied => {
                            let evidence = json!({"source":pending.source, "applied_before":pending.applied, "applied_after":status.applied});
                            self.pending_input = None;
                            return Ok(evidence);
                        }
                        InputWait::Expired => return Err(format!("timed out waiting for input applied: source={:?}, before={}, observed={}", pending.source, pending.applied, status.applied).into()),
                        InputWait::Pending => {}
                    }
                    thread::sleep(Duration::from_millis(5));
                }
            }
            Operation::WaitKnobParameterMode { mode } => {
                wait_parameter_mode(session, &control, mode, guest_timeout_ms, wall_timeout_s)?;
                if let Some(scope) = self.knob_mode.as_mut() {
                    scope.selected_mode = Some(mode);
                }
                Ok(json!({"mode":mode}))
            }
            Operation::WaitKnobDisplayImage {
                image,
                main_timeout_ms,
            } => {
                if self.knob_mode.is_none() || main_timeout_ms == 0 {
                    return Err(
                        "knob image wait requires an active mode and positive MAIN timeout".into(),
                    );
                }
                let path = crate::scenario::spec_path(&image)?;
                // Reference update mode must reach the subsequent capture even
                // if the old image differs or is missing.
                let reference = if super::display::updating_references() {
                    None
                } else {
                    let image = image::open(&path)
                        .map_err(|error| format!("failed to open {}: {error}", path.display()))?
                        .to_luma8();
                    if image.dimensions() != (128, 64) {
                        return Err("knob display reference must be 128×64".into());
                    }
                    Some(image)
                };
                let inputs = session.engine.inputs.as_ref().ok_or("missing inputs")?;
                let main = inputs.main.snapshot();
                if !main.connected || main.clock_updates == 0 {
                    return Err("MAIN virtual clock unavailable".into());
                }
                let start_ms = main.guest_ms;
                let start_updates = main.clock_updates;
                let wall = Instant::now();
                let shared = shared.ok_or("missing knob-dialog shared deadline")?;
                loop {
                    let data = session.poll()?;
                    let panel = inputs.panel.snapshot();
                    let main = inputs.main.snapshot();
                    if !main.connected {
                        return Err("MAIN virtual clock disconnected".into());
                    }
                    let shared_expired = super::execute::deadline_exceeded(
                        shared.0.elapsed(),
                        shared.1,
                        panel.guest_ms,
                        shared.2,
                        shared.3,
                    );
                    match knob_image_wait(
                        reference
                            .as_ref()
                            .map(|image| display_difference_count(&data.display, image)),
                        main.clock_updates > start_updates,
                        main.guest_ms.wrapping_sub(start_ms),
                        main_timeout_ms,
                        shared_expired,
                        wall.elapsed() >= Duration::from_secs(wall_timeout_s),
                    ) {
                        KnobImageWait::Ready => {
                            return Ok(
                                json!({"image":image,"main_elapsed_ms":main.guest_ms.wrapping_sub(start_ms)}),
                            );
                        }
                        KnobImageWait::Expired => {
                            return Err(display_mismatch(
                                &data.display,
                                &path,
                                &options.logs,
                                "knob-display-wait",
                            )
                            .into());
                        }
                        KnobImageWait::Pending => {}
                    }
                    thread::sleep(Duration::from_millis(5));
                }
            }
            Operation::CaptureDisplay { guest_ms, artifact } => {
                if guest_ms == 0 || !valid_artifact(&artifact) {
                    return Err("invalid display capture duration or artifact name".into());
                }
                if self.captured_display.is_some() {
                    return Err("captured display has not been checked".into());
                }
                let start = control.input_status(l6ctl::InputSource::Panel)?.guest_ms;
                let wall = Instant::now();
                let check_shared = |guest: u32| -> Result<(), Box<dyn std::error::Error>> {
                    if let Some((begin, guest_begin, guest_budget, wall_budget, _)) = shared {
                        if super::execute::deadline_exceeded(
                            begin.elapsed(),
                            guest_begin,
                            guest,
                            guest_budget,
                            wall_budget,
                        ) {
                            return Err(
                                "knob-dialog shared deadline exceeded during display capture"
                                    .into(),
                            );
                        }
                    }
                    Ok(())
                };
                check_shared(start)?;
                let frame = loop {
                    let data = session.poll()?;
                    let now = control.input_status(l6ctl::InputSource::Panel)?.guest_ms;
                    check_shared(now)?;
                    let elapsed = now.wrapping_sub(start);
                    if elapsed >= guest_ms {
                        break data.display;
                    }
                    if elapsed > guest_timeout_ms
                        || wall.elapsed() > Duration::from_secs(wall_timeout_s)
                    {
                        return Err("display capture exceeded its deadline".into());
                    }
                    thread::sleep(Duration::from_millis(5));
                };
                check_shared(control.input_status(l6ctl::InputSource::Panel)?.guest_ms)?;
                let capture = CapturedDisplay {
                    frame,
                    artifact: artifact.clone(),
                };
                if self.knob_mode.is_none()
                    && self
                        .knob_dialog_preflight
                        .as_ref()
                        .and_then(bitmap_capture_phase)
                        .is_some_and(|p| matches!(p, 1 | 6))
                {
                    commit_level_capture(
                        || check_shared(control.input_status(l6ctl::InputSource::Panel)?.guest_ms),
                        || save_captured_display(&capture, &options.logs),
                        || note_bitmap_capture(self.knob_dialog_preflight.as_mut().unwrap()),
                    )?;
                } else if self.knob_mode.is_none()
                    && self
                        .knob_dialog_preflight
                        .as_ref()
                        .and_then(level_phase)
                        .is_some_and(|phase| matches!(phase, 2 | 5))
                {
                    commit_level_capture(
                        || check_shared(control.input_status(l6ctl::InputSource::Panel)?.guest_ms),
                        || save_captured_display(&capture, &options.logs),
                        || note_level_capture(self.knob_dialog_preflight.as_mut().unwrap()),
                    )?;
                } else if self.knob_mode.is_none()
                    && self.knob_dialog_preflight.as_ref().is_some_and(|c| {
                        expecting_overlay_refresh(c)
                            || expecting_locked_capture(c)
                            || expecting_active_display(c)
                            || expecting_task_capture(c)
                    })
                {
                    save_captured_display(&capture, &options.logs)?;
                } else if self.knob_mode.is_none()
                    && self
                        .knob_dialog_preflight
                        .as_ref()
                        .is_some_and(|context| final_phase(context) == 22)
                {
                    save_captured_display(&capture, &options.logs)?;
                    note_expiry_capture(self.knob_dialog_preflight.as_mut().unwrap())?;
                } else if self.knob_mode.is_none()
                    && self
                        .knob_dialog_preflight
                        .as_ref()
                        .is_some_and(|context| matches!(final_phase(context), 44 | 47))
                {
                    save_captured_display(&capture, &options.logs)?;
                    note_navigation_capture(self.knob_dialog_preflight.as_mut().unwrap())?;
                }
                self.captured_display = Some(capture);
                Ok(json!({"guest_ms":guest_ms,"artifact":artifact}))
            }
            Operation::ExpectCapturedDisplayImage { image } => {
                let capture = self
                    .captured_display
                    .as_ref()
                    .ok_or("no display capture to check")?;
                let path = crate::scenario::spec_path(&image)?;
                check_display_reference(&capture.frame, &path, &options.logs, &capture.artifact)?;
                save_captured_display(capture, &options.logs)?;
                let artifact = capture.artifact.clone();
                self.captured_display = None;
                Ok(json!({"image":image,"artifact":artifact}))
            }
            Operation::KnobDialogExpectHeap => {
                let context = self
                    .knob_dialog_preflight
                    .as_ref()
                    .ok_or("missing knob-dialog preflight")?;
                let counters = self
                    .knob_heap_reader
                    .as_ref()
                    .ok_or("missing guarded heap reader")?
                    .read()?;
                expect_mode_heap(context, &counters)?;
                Ok(json!({"allocations":counters.allocations,"frees":counters.frees}))
            }
            Operation::KnobDialogModeEnd => {
                if self.pending_input.is_some() || self.captured_display.is_some() {
                    return Err("mode has unfinished observations".into());
                }
                let scope = self.knob_mode.take().ok_or("no knob-dialog mode active")?;
                let selected = scope
                    .selected_mode
                    .ok_or("mode selector was not observed")?;
                let context = self
                    .knob_dialog_preflight
                    .as_mut()
                    .ok_or("missing knob-dialog preflight")?;
                end_knob_mode(context, selected);
                Ok(json!({"mode":selected,"scene":scope.scene}))
            }
            _ => Err("not a mode observation primitive".into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn acknowledgement_uses_one_applied_event_and_is_bounded() {
        assert_eq!(input_wait(4, 4, false), InputWait::Pending);
        assert_eq!(input_wait(4, 5, false), InputWait::Applied);
        assert_eq!(input_wait(4, 4, true), InputWait::Expired);
        // Preserve the original completion-before-timeout ordering.
        assert_eq!(input_wait(4, 5, true), InputWait::Applied);
    }

    #[test]
    fn scope_admits_only_mode_primitives() {
        assert!(mode_operation_matches(&Operation::EnqueueTap {
            target: 35,
            hold_ms: 50
        }));
        assert!(!mode_operation_matches(&Operation::HostMidiConnect));
        assert!(!valid_artifact("../escape"));
    }

    #[test]
    fn knob_image_wait_requires_fresh_exact_pixels_and_enforces_deadlines() {
        assert_eq!(
            knob_image_wait(Some(356), true, 100, 5000, false, false),
            KnobImageWait::Pending
        );
        assert_eq!(
            knob_image_wait(Some(150), true, 200, 5000, false, false),
            KnobImageWait::Pending
        );
        assert_eq!(
            knob_image_wait(Some(0), false, 200, 5000, false, false),
            KnobImageWait::Pending
        );
        assert_eq!(
            knob_image_wait(Some(0), true, 200, 5000, false, false),
            KnobImageWait::Ready
        );
        // Updating references accepts a changed or missing image only in this mode.
        assert_eq!(
            knob_image_wait(None, true, 200, 5000, false, false),
            KnobImageWait::Ready
        );
        assert_eq!(
            knob_image_wait(None, false, 200, 5000, false, false),
            KnobImageWait::Pending
        );
        assert_eq!(
            knob_image_wait(Some(356), true, 5000, 5000, false, false),
            KnobImageWait::Expired
        );
        assert_eq!(
            knob_image_wait(Some(0), true, 200, 5000, true, false),
            KnobImageWait::Expired
        );
        assert_eq!(
            knob_image_wait(Some(0), true, 200, 5000, false, true),
            KnobImageWait::Expired
        );
    }

    #[test]
    fn transient_readiness_does_not_skip_final_capture_assertion() {
        let root = std::env::temp_dir().join(format!("l6-transient-image-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let reference = root.join("reference.png");
        image::GrayImage::new(128, 64).save(&reference).unwrap();
        assert_eq!(
            knob_image_wait(Some(0), true, 100, 5000, false, false),
            KnobImageWait::Ready
        );
        let mut later_capture = [0; 1024];
        later_capture[0] = 1;
        assert!(check_display_reference(&later_capture, &reference, &root, "changed").is_err());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn comparison_uses_the_captured_frame_even_if_the_live_frame_changes() {
        let root = std::env::temp_dir().join(format!("l6-captured-image-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let reference = root.join("reference.png");
        image::GrayImage::new(128, 64).save(&reference).unwrap();
        let mut live = [0; 1024];
        let capture = CapturedDisplay {
            frame: live,
            artifact: "captured".into(),
        };
        live[0] = 1;
        assert!(
            check_display_reference(&capture.frame, &reference, &root, &capture.artifact).is_ok()
        );
        assert!(check_display_reference(&live, &reference, &root, "changed").is_err());
        std::fs::remove_dir_all(root).unwrap();
    }
}

#[cfg(test)]
mod level_capture_deadline_tests {
    use super::*;
    #[test]
    fn does_not_save_or_advance_if_deadline_expired() {
        let saved = std::cell::Cell::new(false);
        let advanced = std::cell::Cell::new(false);
        let result = commit_level_capture(
            || Err("expired".into()),
            || {
                saved.set(true);
                Ok(())
            },
            || {
                advanced.set(true);
                Ok(())
            },
        );
        assert!(result.is_err());
        assert!(!saved.get());
        assert!(!advanced.get());
    }
    #[test]
    fn does_not_advance_if_save_consumes_remaining_budget() {
        let checks = std::cell::Cell::new(0);
        let advanced = std::cell::Cell::new(false);
        let result = commit_level_capture(
            || {
                checks.set(checks.get() + 1);
                if checks.get() == 1 {
                    Ok(())
                } else {
                    Err("expired".into())
                }
            },
            || Ok(()),
            || {
                advanced.set(true);
                Ok(())
            },
        );
        assert!(result.is_err());
        assert!(!advanced.get());
    }
}
