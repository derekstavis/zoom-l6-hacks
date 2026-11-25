//! The knob-dialog patch: its window and popup.
use super::*;

/// Reviewed knob-dialog window images, from the repository root. The
/// knob-dialog window check compares the display with these by scene name.
pub const KNOB_DIALOG_REFERENCES: &str = "patches/knob-dialog/tests/emulator/references";

/// Wait until the MAIN 1.10 mixer call and the knob-dialog post-hook return.
/// At this point a first-use guard has either rejected the turn or published
/// the pending callback, even when the mixer value was already at its limit.
fn turn_and_wait_hook(
    session: &Session,
    debugger: &mut l6max_diagnostics::gdb::Gdb,
    delta: i32,
    wall_timeout_s: u64,
) -> Result<(), Box<dyn std::error::Error>> {
    const MIXER_HOOK_RETURN: u32 = 0x8003_6c78;
    let qmp = &session.engine.management[0];
    qmp.stop()?;
    debugger.breakpoint(MIXER_HOOK_RETURN, true)?;
    session.engine.inputs.as_ref().unwrap().knob(0, delta)?;
    debugger.continue_until_stop(Duration::from_secs(wall_timeout_s.min(30)))?;
    let pc = debugger.read_register(15)? & !1;
    debugger.breakpoint(MIXER_HOOK_RETURN, false)?;
    if pc != MIXER_HOOK_RETURN {
        return Err(format!("knob turn stopped at {pc:#x}, not the mixer hook return").into());
    }
    qmp.resume()?;
    Ok(())
}

fn recorder_controls_visible(display: &[u8; 1024], reference: &image::GrayImage) -> bool {
    (16..40).all(|y| {
        (0..128).all(|x| {
            let index = ((63 - y) / 8) * 128 + x;
            (display[index] & (1 << ((63 - y) % 8)) != 0)
                == (reference.get_pixel(x as u32, y as u32).0[0] >= 128)
        })
    })
}

pub(super) fn run_knob_dialog_ready(
    session: &Session,
    options: &Options,
    guest_timeout_ms: u32,
    wall_timeout_s: u64,
) -> Result<Value, Box<dyn std::error::Error>> {
    use std::time::Instant;
    let main = selected_main_image(options).map_err(std::io::Error::other)?;
    if main.get(0x40c..0x410) != Some(&0x8007eea9u32.to_le_bytes()) {
        return Err("patched startup hook does not reserve popup RAM".into());
    }
    let inputs = session.engine.inputs.as_ref().unwrap();
    const READY_GUEST_TIMEOUT_MS: u32 = 30_000;
    const READY_WALL_TIMEOUT_S: u64 = 180;
    const READY_POLL_MS: u64 = 5;
    let recorder_reference = image::open(
        l6max_host::paths::repository_root()
            .join("tests/emulator/references/l6max-recorder-home.png"),
    )?
    .to_luma8();
    if recorder_reference.dimensions() != (128, 64) {
        return Err("recorder reference must be 128×64".into());
    }
    let control = l6ctl::OwnedControl::new(&session.engine);
    let reader = control.knob_popup_reader()?;
    let start = inputs.panel.snapshot().guest_ms;
    let deadline = Instant::now() + Duration::from_secs(wall_timeout_s.min(READY_WALL_TIMEOUT_S));
    loop {
        let display = session.poll()?.display;
        let now = inputs.panel.snapshot().guest_ms;
        let observed = reader.recorder_state()?;
        let window = observed.window;
        let busy = observed.notification_busy;
        let active = observed.notification_active;
        if window == l6max::GUI_RECORDER_WINDOW
            && busy == 0
            && active == 0
            && recorder_controls_visible(&display, &recorder_reference)
        {
            break;
        }
        if now.wrapping_sub(start) >= guest_timeout_ms.min(READY_GUEST_TIMEOUT_MS)
            || Instant::now() >= deadline
        {
            return Err("recorder did not settle before knob-dialog checks".into());
        }
        thread::sleep(Duration::from_millis(READY_POLL_MS));
    }

    Ok(serde_json::json!({"recorder_ready": true}))
}

pub(super) fn run_knob_dialog(
    session: &Session,
    options: &Options,
    guest_timeout_ms: u32,
    wall_timeout_s: u64,
) -> Result<Value, Box<dyn std::error::Error>> {
    use std::time::Instant;

    let main = selected_main_image(options).map_err(std::io::Error::other)?;
    if main.get(0x40c..0x410) != Some(&0x8007eea9u32.to_le_bytes()) {
        return Err("patched startup hook does not reserve popup RAM".into());
    }
    let inputs = session.engine.inputs.as_ref().unwrap();
    let qmp = &session.engine.management[0];
    // The project title varies with the selected project. The recorder
    // transport controls in the middle of this reviewed image do not.
    const READY_GUEST_TIMEOUT_MS: u32 = 30_000;
    const READY_WALL_TIMEOUT_S: u64 = 180;
    const READY_POLL_MS: u64 = 5;
    let recorder_reference = image::open(
        l6max_host::paths::repository_root()
            .join("tests/emulator/references/l6max-recorder-home.png"),
    )?
    .to_luma8();
    if recorder_reference.dimensions() != (128, 64) {
        return Err("recorder reference must be 128×64".into());
    }
    let wait_recorder_controls = || -> Result<(), Box<dyn std::error::Error>> {
        let start = inputs.panel.snapshot().guest_ms;
        let deadline =
            Instant::now() + Duration::from_secs(wall_timeout_s.min(READY_WALL_TIMEOUT_S));
        loop {
            let display = session.poll()?.display;
            if recorder_controls_visible(&display, &recorder_reference) {
                return Ok(());
            }
            if inputs.panel.snapshot().guest_ms.wrapping_sub(start)
                >= guest_timeout_ms.min(READY_GUEST_TIMEOUT_MS)
                || Instant::now() >= deadline
            {
                return Err("recorder controls did not render before knob-dialog checks".into());
            }
            thread::sleep(Duration::from_millis(READY_POLL_MS));
        }
    };
    let word = |address: u32| -> Result<u32, Box<dyn std::error::Error>> {
        Ok(u32::from_le_bytes(
            qmp.read_memory(0, address, 4)?.try_into().unwrap(),
        ))
    };
    let wait_window = |recorder: bool| -> Result<u32, Box<dyn std::error::Error>> {
        let start = inputs.panel.snapshot().guest_ms;
        let deadline = Instant::now() + Duration::from_secs(wall_timeout_s.min(120));
        loop {
            session.poll()?;
            let window = word(l6max::current_window)?;
            if (window == l6max::GUI_RECORDER_WINDOW) == recorder {
                return Ok(window);
            }
            if inputs.panel.snapshot().guest_ms.wrapping_sub(start) >= guest_timeout_ms.min(10_000)
                || Instant::now() >= deadline
            {
                return Err(if recorder {
                    "Menu button did not return to recorder before dialog checks"
                } else {
                    "Menu button did not leave the recorder screen for the guard test"
                }
                .into());
            }
            thread::sleep(Duration::from_millis(5));
        }
    };
    // MAIN 1.10's parameter selector writes this byte before returning;
    // the corresponding getter at 0x8000b4a8 reads it.
    const SELECTED_PARAMETER_MODE: u32 = 0x8046_2d2c;
    let wait_parameter_mode = |expected: u8| -> Result<(), Box<dyn std::error::Error>> {
        let start = inputs.panel.snapshot().guest_ms;
        let deadline = Instant::now() + Duration::from_secs(wall_timeout_s.min(180));
        loop {
            session.poll()?;
            let actual = qmp.read_memory(0, SELECTED_PARAMETER_MODE, 1)?[0];
            if actual == expected {
                return Ok(());
            }
            if inputs.panel.snapshot().guest_ms.wrapping_sub(start) >= guest_timeout_ms.min(10_000)
                || Instant::now() >= deadline
            {
                return Err(
                    format!("parameter mode did not become {expected}; found {actual}").into(),
                );
            }
            thread::sleep(Duration::from_millis(5));
        }
    };
    let text_pointers = word(l6max::gui_text_pointers_by_language)?;
    let original_resources = qmp.read_memory(0, text_pointers + 229 * 4, 12)?;
    let lengths_table = word(l6max::gui_text_lengths_by_language)?;
    let original_lengths = qmp.read_memory(0, lengths_table + 229 * 2, 6)?;
    let mut original_text = Vec::new();
    for (pointer, capacity) in original_resources
        .chunks_exact(4)
        .zip(original_lengths.chunks_exact(2))
    {
        let address = u32::from_le_bytes(pointer.try_into().unwrap());
        let count = (usize::from(u16::from_le_bytes(capacity.try_into()?)) + 1) * 2;
        original_text.push((address, qmp.read_memory(0, address, count)?));
    }
    let heap = || -> Result<Vec<u32>, Box<dyn std::error::Error>> {
        Ok(qmp
            .read_memory(0, l6max::xFreeBytesRemaining, 16)?
            .chunks_exact(4)
            .map(|b| u32::from_le_bytes(b.try_into().unwrap()))
            .collect())
    };
    let capture = |duration: u32, name: &str| -> Result<[u8; 1024], Box<dyn std::error::Error>> {
        let start = inputs.panel.snapshot().guest_ms;
        let begin = Instant::now();
        let raw = loop {
            let data = session.poll()?;
            let elapsed = inputs.panel.snapshot().guest_ms.wrapping_sub(start);
            if elapsed >= duration {
                break data.display;
            }
            if elapsed > guest_timeout_ms || begin.elapsed() > Duration::from_secs(wall_timeout_s) {
                return Err("knob-dialog display capture exceeded its deadline".into());
            }
            thread::sleep(Duration::from_millis(5));
        };
        let native = image::GrayImage::from_fn(128, 64, |x, y| {
            let index = ((63 - y as usize) / 8) * 128 + x as usize;
            image::Luma([if raw[index] & (1 << ((63 - y as usize) % 8)) != 0 {
                255
            } else {
                0
            }])
        });
        if let Some(scene) = name.strip_prefix("window-scene-")
            && [
                "high", "freq", "mid", "low", "aux1", "aux2", "efx", "sub-mix", "pan", "level",
            ]
            .contains(&scene)
        {
            let path = l6max_host::paths::repository_root()
                .join(KNOB_DIALOG_REFERENCES)
                .join(format!("{scene}.png"));
            check_display_reference(&raw, &path, &options.logs, name)
                .map_err(|error| format!("knob-dialog screen {scene}: {error}"))?;
        }
        image::DynamicImage::ImageLuma8(native)
            .resize(768, 384, image::imageops::FilterType::Nearest)
            .save(options.logs.join(format!("{name}.png")))?;
        Ok(raw)
    };
    let expected_value = |mode: u32, raw: u8| -> String {
        let table = |offset: usize| {
            f32::from_le_bytes(
                main[offset + raw as usize * 4..offset + raw as usize * 4 + 4]
                    .try_into()
                    .unwrap(),
            )
        };
        match mode {
            0 | 2 | 3 => {
                let db = table(0xa5138);
                if db > 0.0 {
                    format!("+{db:.2} dB")
                } else {
                    format!("{db:.2} dB")
                }
            }
            1 => {
                let hz = table(0xa5738);
                if hz < 1000.0 {
                    format!("{hz:.0} Hz")
                } else {
                    format!("{:.2} kHz", hz / 1000.0)
                }
            }
            8 => match raw {
                63 | 64 => "CENTER".into(),
                0..=62 => format!("L {}%", ((63 - raw as u32) * 100 + 31) / 63),
                _ => format!("R {}%", ((raw as u32 - 64) * 100 + 31) / 63),
            },
            _ => format!("{}%", (raw as u32 * 100 + 63) / 127),
        }
    };
    let inputs = session.engine.inputs.as_ref().unwrap();
    // Check the first-use guard on an actual non-recorder screen before
    // the dialog has initialized state or claimed its display callback.
    use firmware_api::l6max::GUI_RECORDER_WINDOW;
    if u32::from_le_bytes(
        qmp.read_memory(0, l6max::current_window, 4)?
            .try_into()
            .unwrap(),
    ) != GUI_RECORDER_WINDOW
    {
        return Err("knob-dialog test did not start from the recorder screen".into());
    }
    if qmp.read_memory(0, l6max::gui_bitmap_table_entry_0, 4)? != 0u32.to_le_bytes()
        || qmp.read_memory(0, l6max::gui_display_callback_table + 20, 4)? != 0u32.to_le_bytes()
    {
        return Err(
            "knob-dialog state or callback was already active before the guard test".into(),
        );
    }
    inputs.main.tap(0, 120)?;
    let non_recorder_window = wait_window(false)?;
    capture(100, "knob-dialog-non-recorder-before")?;
    inputs.knob(0, -1)?;
    let non_recorder_knob = inputs.panel.snapshot().guest_ms;
    wait_guest(
        session,
        non_recorder_knob,
        500,
        guest_timeout_ms,
        wall_timeout_s,
    )?;
    qmp.stop()?;
    let non_recorder_state = qmp.read_memory(0, l6max::gui_bitmap_table_entry_0, 4)?;
    let non_recorder_slot5 = qmp.read_memory(0, l6max::gui_display_callback_table + 20, 4)?;
    let non_recorder_window_after = u32::from_le_bytes(
        qmp.read_memory(0, l6max::current_window, 4)?
            .try_into()
            .unwrap(),
    );
    let non_recorder_no_dialog = non_recorder_state == 0u32.to_le_bytes()
        && non_recorder_slot5 == 0u32.to_le_bytes()
        && non_recorder_window_after == non_recorder_window;
    qmp.resume()?;
    if !non_recorder_no_dialog {
        return Err(format!(
                "knob event opened or changed the dialog outside recorder: state={non_recorder_state:02x?}, slot5={non_recorder_slot5:02x?}, window={non_recorder_window_after:#010x}"
            )
            .into());
    }
    inputs.main.tap(0, 120)?;
    wait_window(true)?;
    wait_recorder_controls()?;
    let non_recorder_guard_passed = true;

    let display_callbacks = qmp.read_memory(0, l6max::gui_display_callback_table, 24)?;
    let display_callback_target = u32::from_le_bytes(display_callbacks[20..24].try_into().unwrap());
    let slot0 = u32::from_le_bytes(display_callbacks[0..4].try_into().unwrap());
    let slot2 = u32::from_le_bytes(display_callbacks[8..12].try_into().unwrap());
    if display_callback_target != 0 || slot0 != 0 || slot2 != 0 {
        return Err(format!(
                "expected an empty slot 5 before the first knob event; slot0={slot0:#010x}, slot2={slot2:#010x}, slot5={display_callback_target:#010x}"
            )
            .into());
    }
    inputs.tap(35, 50)?;
    wait_parameter_mode(9)?;
    let baseline = capture(100, "window-scene-before")?;
    let original_window = qmp.read_memory(0, l6max::current_window, 4)?;
    let original_bitmap = qmp.read_memory(0, l6max::gui_bitmap_table_entry_0 + 36 * 4, 4)?;
    let original_bitmap_descriptor = qmp.read_memory(0, l6max::GUI_BITMAP_36_DESCRIPTOR, 8)?;
    let original_state_handle = qmp.read_memory(0, l6max::gui_bitmap_table_entry_0, 4)?;
    if u32::from_le_bytes(original_bitmap.clone().try_into().unwrap())
        != l6max::GUI_BITMAP_36_DESCRIPTOR
    {
        return Err(
            "stock bitmap-table entry 36 did not point at its guarded ROM descriptor".into(),
        );
    }
    if original_state_handle != 0u32.to_le_bytes() {
        return Err(format!(
            "bitmap-table slot 0 is not the expected null sentinel: {original_state_handle:02x?}"
        )
        .into());
    }
    let original_notification_semaphore = qmp.read_memory(0, l6max::notification_slot, 4)?;
    let notification_semaphore =
        u32::from_le_bytes(original_notification_semaphore.clone().try_into().unwrap());
    if notification_semaphore == 0 || notification_semaphore & 1 != 0 {
        return Err(format!(
            "expected the stock untagged notification semaphore, got {notification_semaphore:#010x}"
        )
        .into());
    }
    let text_table = u32::from_le_bytes(
        qmp.read_memory(0, l6max::gui_text_pointers_by_language, 4)?
            .try_into()
            .unwrap(),
    );
    let decode_text = |address: u32| -> Result<String, Box<dyn std::error::Error>> {
        let pointer = u32::from_le_bytes(qmp.read_memory(0, address, 4)?.try_into().unwrap());
        let bytes = qmp.read_memory(0, pointer, 48)?;
        let units = bytes
            .chunks_exact(2)
            .map(|b| u16::from_le_bytes([b[0], b[1]]))
            .take_while(|c| *c != 0)
            .collect::<Vec<_>>();
        Ok(String::from_utf16(&units)?)
    };
    let capture_demo = |name: &str| -> Result<[u8; 1024], Box<dyn std::error::Error>> {
        let mut frame = capture(100, name)?;
        for _ in 0..6 {
            if frame.iter().map(|byte| byte.count_ones()).sum::<u32>() >= 100 {
                return Ok(frame);
            }
            frame = capture(100, name)?;
        }
        Err(format!("demo frame {name} stayed blank").into())
    };
    let original_resources = qmp.read_memory(0, text_table + 229 * 4, 8)?;
    let shared_resource_pointer = u32::from_le_bytes(
        qmp.read_memory(0, text_table + 179 * 4, 4)?
            .try_into()
            .unwrap(),
    );
    let original_shared_resource = qmp.read_memory(0, shared_resource_pointer, 512)?;
    let original_lengths_table = u32::from_le_bytes(
        qmp.read_memory(0, l6max::gui_text_lengths_by_language, 4)?
            .try_into()
            .unwrap(),
    );
    let original_lengths = qmp.read_memory(0, original_lengths_table + 229 * 2, 4)?;
    let widget_pointer = |index: u32| -> Result<u32, Box<dyn std::error::Error>> {
        Ok(u32::from_le_bytes(
            qmp.read_memory(0, l6max::gui_widget_table + index * 4, 4)?
                .try_into()
                .unwrap(),
        ))
    };
    let widget7 = widget_pointer(7)?;
    let widget33 = widget_pointer(33)?;
    let original_widget7 = qmp.read_memory(0, widget7, 16)?;
    let original_widget33 = qmp.read_memory(0, widget33, 16)?;
    if original_widget7.as_slice() != [1, 0, 0, 0, 10, 0, 1, 0, 1, 0, 126, 0, 1, 0, 1, 2]
        || original_widget33.as_slice() != [1, 0, 1, 0, 40, 0, 30, 0, 33, 0, 67, 0, 1, 0, 1, 0]
    {
        return Err("recorder widgets do not match the guarded firmware defaults".into());
    }
    let original_shared_capacity = qmp.read_memory(0, original_lengths_table + 179 * 2, 2)?;
    if u16::from_le_bytes(original_shared_capacity.try_into().unwrap()) != 255
        || original_shared_resource
            .chunks_exact(2)
            .take(255)
            .any(|unit| unit != b"*\0")
        || original_shared_resource[510..512] != [0, 0]
    {
        return Err("shared GUI resource 179 is not the expected free 255-character buffer".into());
    }
    let background_pointer = |index: u32| -> Result<u32, Box<dyn std::error::Error>> {
        Ok(u32::from_le_bytes(
            qmp.read_memory(0, l6max::gui_background_table + index * 4, 4)?
                .try_into()
                .unwrap(),
        ))
    };
    let background10 = background_pointer(10)?;
    let background37 = background_pointer(37)?;
    let background48 = background_pointer(48)?;
    let original_background_layers = [
        qmp.read_memory(0, background10, 1)?,
        qmp.read_memory(0, background37, 1)?,
        qmp.read_memory(0, background48, 1)?,
    ];
    let heap_before = heap()?;
    let socket = options
        .gdb_socket
        .as_ref()
        .ok_or("knob-dialog lock-contention check needs GDB")?;
    let mut debugger = l6max_diagnostics::gdb::Gdb::connect(socket)?;

    // A descriptor drift must make the patch decline first use without
    // allocating state or claiming its callback slot.
    qmp.stop()?;
    debugger.write(widget7 + 6, &2u16.to_le_bytes())?;
    qmp.resume()?;
    turn_and_wait_hook(session, &mut debugger, -1, wall_timeout_s)?;
    qmp.stop()?;
    let mismatch_state = qmp.read_memory(0, l6max::gui_bitmap_table_entry_0, 4)?;
    let mismatch_slot5 = qmp.read_memory(0, l6max::gui_display_callback_table + 20, 4)?;
    let mismatch_heap = heap()?;
    debugger.write(widget7 + 6, &1u16.to_le_bytes())?;
    qmp.resume()?;
    if mismatch_state != 0u32.to_le_bytes()
        || mismatch_slot5 != 0u32.to_le_bytes()
        || mismatch_heap != heap_before
    {
        return Err(format!(
                "descriptor drift was not rejected cleanly: state={mismatch_state:02x?}, slot5={mismatch_slot5:02x?}, heap={mismatch_heap:?}"
            )
            .into());
    }

    // Resource 179 is borrowed by the dialog only if no other initialized
    // widget points at it. Simulate another patch claiming that resource.
    let alias_widget = widget_pointer(60)?;
    let original_alias_resource = qmp.read_memory(0, alias_widget + 4, 2)?;
    qmp.stop()?;
    debugger.write(alias_widget + 4, &179u16.to_le_bytes())?;
    qmp.resume()?;
    turn_and_wait_hook(session, &mut debugger, -1, wall_timeout_s)?;
    qmp.stop()?;
    let alias_state = qmp.read_memory(0, l6max::gui_bitmap_table_entry_0, 4)?;
    let alias_slot5 = qmp.read_memory(0, l6max::gui_display_callback_table + 20, 4)?;
    let alias_heap = heap()?;
    debugger.write(alias_widget + 4, &original_alias_resource)?;
    qmp.resume()?;
    if alias_state != 0u32.to_le_bytes()
        || alias_slot5 != 0u32.to_le_bytes()
        || alias_heap != heap_before
    {
        return Err(format!(
                "shared text resource alias was not rejected cleanly: state={alias_state:02x?}, slot5={alias_slot5:02x?}, heap={alias_heap:?}"
            )
            .into());
    }

    // Layer 4 is reserved for this scene. An existing descriptor on that
    // layer must also prevent first use from claiming it.
    let original_alias_layer = qmp.read_memory(0, alias_widget, 2)?;
    qmp.stop()?;
    debugger.write(alias_widget, &4u16.to_le_bytes())?;
    qmp.resume()?;
    turn_and_wait_hook(session, &mut debugger, -1, wall_timeout_s)?;
    qmp.stop()?;
    let layer_state = qmp.read_memory(0, l6max::gui_bitmap_table_entry_0, 4)?;
    let layer_slot5 = qmp.read_memory(0, l6max::gui_display_callback_table + 20, 4)?;
    let layer_heap = heap()?;
    debugger.write(alias_widget, &original_alias_layer)?;
    qmp.resume()?;
    if layer_state != 0u32.to_le_bytes()
        || layer_slot5 != 0u32.to_le_bytes()
        || layer_heap != heap_before
    {
        return Err(format!(
                "scene layer collision was not rejected cleanly: state={layer_state:02x?}, slot5={layer_slot5:02x?}, heap={layer_heap:?}"
            )
            .into());
    }

    // The compact state record restores fixed stock layers, so reject a
    // changed borrowed widget layer instead of trying to save metadata.
    let original_widget_layer = qmp.read_memory(0, widget7, 1)?;
    qmp.stop()?;
    debugger.write(widget7, &[0])?;
    qmp.resume()?;
    turn_and_wait_hook(session, &mut debugger, -1, wall_timeout_s)?;
    qmp.stop()?;
    let widget_layer_state = qmp.read_memory(0, l6max::gui_bitmap_table_entry_0, 4)?;
    let widget_layer_slot5 = qmp.read_memory(0, l6max::gui_display_callback_table + 20, 4)?;
    let widget_layer_heap = heap()?;
    debugger.write(widget7, &original_widget_layer)?;
    qmp.resume()?;
    let widget_layer_guard_rejected = widget_layer_state == 0u32.to_le_bytes()
        && widget_layer_slot5 == 0u32.to_le_bytes()
        && widget_layer_heap == heap_before;
    if !widget_layer_guard_rejected {
        return Err(format!(
                "changed widget layer was not rejected cleanly: state={widget_layer_state:02x?}, slot5={widget_layer_slot5:02x?}, heap={widget_layer_heap:?}"
            )
            .into());
    }

    // Background descriptors have the same fixed-layer restoration
    // contract and must pass the same first-use preflight.
    qmp.stop()?;
    debugger.write(background10, &[1])?;
    qmp.resume()?;
    turn_and_wait_hook(session, &mut debugger, -1, wall_timeout_s)?;
    qmp.stop()?;
    let background_layer_state = qmp.read_memory(0, l6max::gui_bitmap_table_entry_0, 4)?;
    let background_layer_slot5 = qmp.read_memory(0, l6max::gui_display_callback_table + 20, 4)?;
    let background_layer_heap = heap()?;
    debugger.write(background10, &original_background_layers[0])?;
    qmp.resume()?;
    let background_layer_guard_rejected = background_layer_state == 0u32.to_le_bytes()
        && background_layer_slot5 == 0u32.to_le_bytes()
        && background_layer_heap == heap_before;
    if !background_layer_guard_rejected {
        return Err(format!(
                "changed background layer was not rejected cleanly: state={background_layer_state:02x?}, slot5={background_layer_slot5:02x?}, heap={background_layer_heap:?}"
            )
            .into());
    }

    // Simulate the notification manager owning its semaphore during first
    // use. The scene must not depend on that unrelated lock.
    qmp.stop()?;
    let semaphore_count = qmp.read_memory(0, notification_semaphore + 56, 4)?;
    if semaphore_count != 1u32.to_le_bytes() {
        qmp.resume()?;
        return Err("notification semaphore was not available before contention check".into());
    }
    debugger.write(notification_semaphore + 56, &0u32.to_le_bytes())?;
    // Stop at callback registration after the mixer-side scene preflight
    // has passed. Change a borrowed descriptor before DisplayCycle2 can
    // render so the callback-side preflight gets exercised deterministically.
    use firmware_api::l6max::gui_display_callback_register;
    debugger.breakpoint(gui_display_callback_register, true)?;
    qmp.resume()?;
    inputs.knob(0, -1)?;
    let registration_deadline = Instant::now() + Duration::from_secs(10);
    while qmp.query_status()?.running {
        session.poll()?;
        if Instant::now() >= registration_deadline {
            let _ = debugger.breakpoint(gui_display_callback_register, false);
            let _ = qmp.resume();
            return Err("first-use callback registration breakpoint timed out".into());
        }
        thread::sleep(Duration::from_millis(5));
    }
    let registration_pc = debugger.read_register(15)? & !1;
    let callback_slot = debugger.read_register(0)?;
    let callback_pointer = debugger.read_register(1)?;
    if registration_pc != gui_display_callback_register
        || callback_slot != 5
        || callback_pointer & 1 == 0
    {
        let _ = debugger.breakpoint(gui_display_callback_register, false);
        let _ = qmp.resume();
        return Err(format!(
                "unexpected display callback registration: pc={registration_pc:#010x}, slot={callback_slot}, callback={callback_pointer:#010x}"
            )
            .into());
    }
    let callback_entry = callback_pointer & !1;
    debugger.breakpoint(callback_entry, true)?;
    debugger.breakpoint(gui_display_callback_register, false)?;
    debugger.write(alias_widget, &4u16.to_le_bytes())?;
    qmp.resume()?;
    let callback_deadline = Instant::now() + Duration::from_secs(10);
    while qmp.query_status()?.running {
        session.poll()?;
        if Instant::now() >= callback_deadline {
            let _ = debugger.breakpoint(callback_entry, false);
            let _ = qmp.resume();
            return Err("first DisplayCycle2 callback breakpoint timed out".into());
        }
        thread::sleep(Duration::from_millis(5));
    }
    let callback_pc = debugger.read_register(15)? & !1;
    if callback_pc != callback_entry {
        let _ = debugger.breakpoint(callback_entry, false);
        let _ = qmp.resume();
        return Err(format!(
                "first-use race probe stopped at {callback_pc:#010x}, not callback {callback_entry:#010x}"
            )
            .into());
    }
    debugger.breakpoint(callback_entry, false)?;
    qmp.resume()?;
    let race_frame = capture(150, "window-scene-callback-preflight-race")?;
    qmp.stop()?;
    let race_state = qmp.read_memory(0, l6max::gui_bitmap_table_entry_0, 4)?;
    let race_slot5 = qmp.read_memory(0, l6max::gui_display_callback_table + 20, 4)?;
    let callback_preflight_race_rejected = race_frame == baseline
        && u16::from_le_bytes(race_state[..2].try_into().unwrap()) == 0x4b4e
        && u32::from_le_bytes(race_slot5.clone().try_into().unwrap()) == callback_pointer
        && qmp.read_memory(0, alias_widget, 1)? == [4]
        && qmp.read_memory(0, widget7, 1)? == [1]
        && qmp.read_memory(0, background10, 1)? == [0]
        && qmp.read_memory(0, shared_resource_pointer, 512)? == original_shared_resource;
    debugger.write(alias_widget, &original_alias_layer)?;
    qmp.resume()?;
    if !callback_preflight_race_rejected {
        return Err(format!(
                "callback-side scene preflight failed to reject descriptor drift: state={race_state:02x?}, slot5={race_slot5:02x?}, frame_unchanged={} ",
                race_frame == baseline
            )
            .into());
    }

    // Retry while the notification semaphore is still held. The scene
    // should now pass its callback-side preflight and render without using
    // the notification manager's lock.
    inputs.knob(0, -1)?;
    let contention_start = inputs.panel.snapshot().guest_ms;
    let contention_deadline = Instant::now() + Duration::from_secs(wall_timeout_s.min(180));
    loop {
        session.poll()?;
        let state = qmp.read_memory(0, l6max::gui_bitmap_table_entry_0, 4)?;
        let slot = word(l6max::gui_display_callback_table + 20)?;
        if u16::from_le_bytes(state[..2].try_into().unwrap()) == 0x4b4e
            && state[2] != 0
            && state[3] == u8::MAX
            && slot & 1 == 1
        {
            break;
        }
        if inputs
            .panel
            .snapshot()
            .guest_ms
            .wrapping_sub(contention_start)
            >= guest_timeout_ms.min(10_000)
            || Instant::now() >= contention_deadline
        {
            return Err("knob dialog did not render while the notification lock was held".into());
        }
        thread::sleep(Duration::from_millis(5));
    }
    qmp.stop()?;
    let first_state = qmp.read_memory(0, l6max::gui_bitmap_table_entry_0, 4)?;
    let first_slot5 = u32::from_le_bytes(
        qmp.read_memory(0, l6max::gui_display_callback_table + 20, 4)?
            .try_into()
            .unwrap(),
    );
    let heap_while_locked = heap()?;
    let notification_lock_still_held =
        qmp.read_memory(0, notification_semaphore + 56, 4)? == 0u32.to_le_bytes();
    let dialog_without_notification_lock = u16::from_le_bytes(first_state[..2].try_into().unwrap())
        == 0x4b4e
        && first_slot5 & 1 == 1
        && qmp.read_memory(0, l6max::gui_bitmap_table_entry_0 + 3, 1)? == u8::MAX.to_le_bytes()
        // A nonzero timeout means a scene is showing; its descriptors are
        // already back on their stock layers.
        && first_state[2] != 0
        && qmp.read_memory(0, widget7, 16)? == original_widget7
        && qmp.read_memory(0, background10, 1)? == original_background_layers[0]
        && notification_lock_still_held;
    debugger.write(notification_semaphore + 56, &semaphore_count)?;
    qmp.resume()?;
    if !dialog_without_notification_lock
        || heap_while_locked[2] != heap_before[2]
        || heap_while_locked[3] != heap_before[3]
    {
        return Err(format!(
                "dialog first use failed: state={first_state:02x?}, slot5={first_slot5:#010x}, heap={heap_while_locked:?}, notification_lock_still_held={notification_lock_still_held}, widget_x=({}, {})",
                u16::from_le_bytes([original_widget7[6], original_widget7[7]]),
                u16::from_le_bytes([original_widget33[6], original_widget33[7]])
            )
            .into());
    }

    inputs.knob(0, -3)?;
    let active = capture(500, "knob-dialog-active")?;
    let active_callbacks = qmp.read_memory(0, l6max::gui_display_callback_table, 24)?;
    let active_slot5 = u32::from_le_bytes(active_callbacks[20..24].try_into().unwrap());
    if active_slot5 & 1 == 0
        || u32::from_le_bytes(active_callbacks[0..4].try_into().unwrap()) != 0
        || u32::from_le_bytes(active_callbacks[8..12].try_into().unwrap()) != 0
    {
        return Err(format!(
                "first knob event did not claim only free callback slot 5; slots0/2/5={:#010x}/{:#010x}/{active_slot5:#010x}",
                u32::from_le_bytes(active_callbacks[0..4].try_into().unwrap()),
                u32::from_le_bytes(active_callbacks[8..12].try_into().unwrap()),
            )
            .into());
    }
    if active == baseline {
        return Err("custom overlay did not change the LCD".into());
    }

    // Capture emulator frames while the web demo values move in both
    // directions. The web animation plays these captures at 20 fps.
    capture_demo("knob-dialog-demo-level-start")?;
    let mut level_peak_reached = false;
    // Encoder samples can coalesce while the guest is busy. Keep the target
    // value assertion, but allow enough bounded attempts to reach it.
    for index in 0..128 {
        let displayed = decode_text(text_table + 179 * 4)?;
        let value = displayed
            .strip_suffix('%')
            .and_then(|value| value.parse::<u8>().ok())
            .unwrap_or(0);
        if value >= 69 {
            level_peak_reached = true;
            break;
        }
        inputs.knob(0, 1)?;
        capture_demo(&format!("knob-dialog-demo-level-up-{index:02}"))?;
    }
    if !level_peak_reached {
        return Err("demo level sweep did not reach 69%".into());
    }
    capture(500, "knob-dialog-demo-level-peak")?;
    // The displayed percentage is rounded from a 0..127 firmware value,
    // so 60 encoder steps are not always enough to return to zero.
    for index in 0..128 {
        if decode_text(text_table + 179 * 4)? == "0%" {
            break;
        }
        inputs.knob(0, -1)?;
        capture_demo(&format!("knob-dialog-demo-level-down-{index:02}"))?;
    }
    if decode_text(text_table + 179 * 4)? != "0%" {
        return Err("demo level sweep did not return to zero".into());
    }
    capture(500, "knob-dialog-demo-level-zero")?;

    // Observe the producer and consumer task contexts directly. The knob
    // hook runs from the mixer update path; slot 5 runs from DisplayCycle2.
    // This determines whether the atomic mailbox can be replaced safely.
    use firmware_api::l6max::pxCurrentTCB;
    // knob-dialog's call-site hook into `mixer_apply_channel_parameter`
    // (patches/knob-dialog).
    const KNOB_CALL_SITE: u32 = 0x8003_6c74;
    let knob_hook_entry = thumb_bl_target(
        KNOB_CALL_SITE,
        qmp.read_memory(0, KNOB_CALL_SITE, 4)?
            .try_into()
            .map_err(|_| "patched mixer call is not four bytes")?,
    )
    .ok_or("patched mixer dispatcher call is not a Thumb BL")?;
    let callback_probe_entry = active_slot5 & !1;
    qmp.stop()?;
    debugger.breakpoint(knob_hook_entry, true)?;
    qmp.resume()?;
    inputs.knob(0, -1)?;
    let task_probe_deadline = Instant::now() + Duration::from_secs(10);
    while qmp.query_status()?.running {
        session.poll()?;
        if Instant::now() >= task_probe_deadline {
            return Err("knob hook task-context breakpoint timed out".into());
        }
        thread::sleep(Duration::from_millis(5));
    }
    let hook_pc = debugger.read_register(15)? & !1;
    if hook_pc != knob_hook_entry {
        return Err(format!(
            "task probe stopped at {hook_pc:#010x}, not knob hook {knob_hook_entry:#010x}"
        )
        .into());
    }
    let hook_task = u32::from_le_bytes(qmp.read_memory(0, pxCurrentTCB, 4)?.try_into().unwrap());
    debugger.breakpoint(knob_hook_entry, false)?;
    debugger.breakpoint(callback_probe_entry, true)?;
    qmp.resume()?;

    let task_probe_deadline = Instant::now() + Duration::from_secs(10);
    while qmp.query_status()?.running {
        session.poll()?;
        if Instant::now() >= task_probe_deadline {
            let _ = debugger.breakpoint(callback_probe_entry, false);
            let _ = qmp.resume();
            return Err("DisplayCycle2 task-context breakpoint timed out".into());
        }
        thread::sleep(Duration::from_millis(5));
    }
    let callback_pc = debugger.read_register(15)? & !1;
    if callback_pc != callback_probe_entry {
        return Err(format!(
            "task probe stopped at {callback_pc:#010x}, not DisplayCycle2 callback"
        )
        .into());
    }
    let callback_task =
        u32::from_le_bytes(qmp.read_memory(0, pxCurrentTCB, 4)?.try_into().unwrap());
    if hook_task == callback_task {
        return Err(
                "mixer hook and DisplayCycle2 callback unexpectedly share a task; re-evaluate the atomic mailbox"
                    .into(),
            );
    }
    debugger.breakpoint(callback_probe_entry, false)?;
    qmp.resume()?;
    capture(150, "window-scene-task-context-probe")?;

    if u32::from_le_bytes(
        qmp.read_memory(0, l6max::current_window, 4)?
            .try_into()
            .unwrap(),
    ) != u32::from_le_bytes(original_window.clone().try_into().unwrap())
    {
        return Err("custom overlay replaced the active firmware window".into());
    }
    // The scene borrows the descriptors only while it draws, so a window that
    // opens over it draws its own title on the stock layers.
    let shown = (
        qmp.read_memory(0, widget7, 16)?,
        qmp.read_memory(0, widget33, 16)?,
        [background10, background37, background48]
            .into_iter()
            .map(|background| qmp.read_memory(0, background, 1))
            .collect::<Result<Vec<_>, _>>()?,
    );
    let stock = (
        original_widget7.clone(),
        original_widget33.clone(),
        original_background_layers.to_vec(),
    );
    if shown != stock {
        return Err(format!(
            "custom overlay kept borrowed descriptors after drawing: widgets and background layers are {shown:02x?}, stock {stock:02x?}"
        )
        .into());
    }
    if u32::from_le_bytes(
        qmp.read_memory(0, text_table + 179 * 4, 4)?
            .try_into()
            .unwrap(),
    ) != shared_resource_pointer
    {
        return Err("dialog redirected the shared text-resource pointer".into());
    }
    let black_pixel = |x: usize, y: usize| {
        let index = ((63 - y) / 8) * 128 + x;
        active[index] & (1 << ((63 - y) % 8)) == 0
    };
    if ![(0, 0), (127, 0), (0, 63), (127, 63)]
        .into_iter()
        .all(|(x, y)| black_pixel(x, y))
    {
        return Err("stock bitmap 22 did not paint the overlay background black".into());
    }
    let state = l6max::gui_bitmap_table_entry_0;
    if u16::from_le_bytes(qmp.read_memory(0, state, 2)?.try_into().unwrap()) != 0x4b4e
        || qmp.read_memory(0, l6max::gui_bitmap_table_entry_0 + 36 * 4, 4)? != original_bitmap
    {
        return Err("in-place state signature or stock bitmap-36 descriptor is invalid".into());
    }
    let heap_live = heap()?;
    if heap_live[0] != heap_before[0]
        || heap_live[2] != heap_before[2]
        || heap_live[3] != heap_before[3]
    {
        return Err(format!(
            "dialog state should not allocate heap: before={heap_before:?}, live={heap_live:?}"
        )
        .into());
    }
    let state_heap_bytes = heap_before[0]
        .checked_sub(heap_live[0])
        .ok_or("firmware heap free-byte count increased while allocating dialog state")?;
    if state_heap_bytes != 0 {
        return Err(
            format!("dialog state should not consume heap bytes, got {state_heap_bytes}").into(),
        );
    }
    let null_bitmap_background = background_pointer(47)?;
    if u16::from_le_bytes(
        qmp.read_memory(0, null_bitmap_background + 2, 2)?
            .try_into()
            .unwrap(),
    ) != 0
    {
        return Err("background 47 no longer uses bitmap ID 0".into());
    }
    let before_null_bitmap_draw = capture(50, "window-scene-before-null-bitmap-probe")?;
    // Call the draw routine from the display task, at the dialog's own
    // callback entry. An arbitrary stop can land in the idle task or an
    // exception handler, where the routine can block past the deadline.
    qmp.stop()?;
    debugger.breakpoint(active_slot5 & !1, true)?;
    qmp.resume()?;
    let display_task_deadline = Instant::now() + Duration::from_secs(15);
    while qmp.query_status()?.running {
        session.poll()?;
        if Instant::now() >= display_task_deadline {
            return Err("dialog callback breakpoint was not reached".into());
        }
        thread::sleep(Duration::from_millis(5));
    }
    debugger.breakpoint(active_slot5 & !1, false)?;
    debugger.call_thumb(l6max::gui_draw_background, &[47], Duration::from_secs(5))?;
    qmp.resume()?;
    let after_null_bitmap_draw = capture(100, "window-scene-after-null-bitmap-probe")?;
    if before_null_bitmap_draw != after_null_bitmap_draw
        || u16::from_le_bytes(qmp.read_memory(0, state, 2)?.try_into().unwrap()) != 0x4b4e
        || qmp.read_memory(0, l6max::gui_bitmap_table_entry_0 + 36 * 4, 4)? != original_bitmap
    {
        return Err(
            "bitmap ID 0 did not safely short-circuit while holding the state handle".into(),
        );
    }
    let value_text = decode_text(text_table + 179 * 4)?;
    let raw = qmp.read_memory(0, l6max::mixer_channel_levels, 1)?[0];
    if !knob_display_matches(9, &expected_value(9, raw), &value_text) {
        return Err(format!("custom window value mismatch: {value_text:?}").into());
    }
    let refreshed = capture(500, "window-scene-refreshed")?;
    let pixel = |frame: &[u8; 1024], x: usize, y: usize| {
        frame[((63 - y) / 8) * 128 + x] & (1 << ((63 - y) % 8)) != 0
    };
    // The value has intentionally changed during the level demonstration.
    // Compare the rest of the LCD, including the title and black backing,
    // to make sure normal recorder redraws did not replace the overlay.
    let stable_overlay = (0..64).all(|y| {
        (0..128).all(|x| {
            (30..97).contains(&x) && (33..64).contains(&y)
                || pixel(&active, x, y) == pixel(&refreshed, x, y)
        })
    });
    if !stable_overlay {
        return Err("recorder redraw changed the custom overlay outside its value widget".into());
    }

    // Hold the notification semaphore while a second value is rendered.
    // The patch's dedicated layer/widget resources and persistent bitmap
    // descriptor must remain usable without taking this unrelated lock.
    qmp.stop()?;
    let semaphore_count = qmp.read_memory(0, notification_semaphore + 56, 4)?;
    if semaphore_count != 1u32.to_le_bytes() {
        qmp.resume()?;
        return Err(
            "notification semaphore was not available before callback contention check".into(),
        );
    }
    debugger.write(notification_semaphore + 56, &0u32.to_le_bytes())?;
    qmp.resume()?;
    turn_and_wait_hook(session, &mut debugger, -1, wall_timeout_s)?;
    let callback_busy_start = inputs.panel.snapshot().guest_ms;
    let callback_deadline = Instant::now() + Duration::from_secs(wall_timeout_s.min(180));
    loop {
        session.poll()?;
        let raw = qmp.read_memory(0, l6max::mixer_channel_levels, 1)?[0];
        if qmp.read_memory(0, state + 3, 1)? == u8::MAX.to_le_bytes()
            && knob_display_matches(
                9,
                &expected_value(9, raw),
                &decode_text(text_table + 179 * 4)?,
            )
        {
            break;
        }
        if inputs
            .panel
            .snapshot()
            .guest_ms
            .wrapping_sub(callback_busy_start)
            >= guest_timeout_ms.min(10_000)
            || Instant::now() >= callback_deadline
        {
            return Err("knob dialog did not update while the notification lock was held".into());
        }
        thread::sleep(Duration::from_millis(5));
    }
    qmp.stop()?;
    let pending_during_contention = qmp.read_memory(0, state + 3, 1)?;
    let lock_held_during_render =
        qmp.read_memory(0, notification_semaphore + 56, 4)? == 0u32.to_le_bytes();
    let dialog_value_during_lock = decode_text(text_table + 179 * 4)?;
    let raw_during_lock = qmp.read_memory(0, l6max::mixer_channel_levels, 1)?[0];
    debugger.write(notification_semaphore + 56, &semaphore_count)?;
    qmp.resume()?;
    if pending_during_contention != u8::MAX.to_le_bytes()
        || !lock_held_during_render
        || !knob_display_matches(
            9,
            &expected_value(9, raw_during_lock),
            &dialog_value_during_lock,
        )
    {
        return Err(format!(
                "dialog did not process the latest update while the notification semaphore was unavailable: pending={pending_during_contention:02x?}, value={dialog_value_during_lock:?}"
            )
            .into());
    }
    capture(500, "window-scene-after-callback-contention")?;
    let pending_after_contention = qmp.read_memory(0, state + 3, 1)?;
    if pending_after_contention != u8::MAX.to_le_bytes() {
        return Err(format!(
                "display callback did not consume the pending update while the lock was held: pending={pending_after_contention:02x?}"
            )
            .into());
    }
    for (button, name, mode) in [
        (35, "level", 9),
        (28, "high", 0),
        (33, "freq", 1),
        (38, "mid", 2),
        (43, "low", 3),
        (10, "aux1", 4),
        (15, "aux2", 5),
        (20, "efx", 6),
        (25, "sub-mix", 7),
        (30, "pan", 8),
    ] {
        let selection_applied = inputs.panel.snapshot().applied;
        inputs.tap(button, 50)?;
        wait_l6_input(
            session,
            || inputs.panel.snapshot().applied > selection_applied,
            "knob-dialog parameter-selection input acknowledgement",
        )?;
        wait_parameter_mode(mode)?;
        let encoder_applied = inputs.panel.snapshot().applied;
        inputs.knob(0, -1)?;
        wait_l6_input(
            session,
            || inputs.panel.snapshot().applied > encoder_applied,
            "knob-dialog parameter encoder acknowledgement",
        )?;
        capture(400, &format!("window-scene-{name}"))?;
        let mode_heap = heap()?;
        if mode_heap[2] != heap_live[2] || mode_heap[3] != heap_live[3] {
            return Err(format!(
                "mode {name} changed the persistent heap allocation: {mode_heap:?}"
            )
            .into());
        }
    }
    // The preceding mode walk ends on Pan with a verified live scene.
    // Sweep from center through left 50% to right 50%, capturing at 20 fps.
    capture_demo("knob-dialog-demo-pan-center")?;
    if decode_text(text_table + 179 * 4)? != "CENTER" {
        return Err("demo pan sweep did not start at center".into());
    }
    use firmware_api::l6max::mixer_first_channel_pan;
    let pan_start = qmp.read_memory(0, mixer_first_channel_pan, 1)?[0];
    for name in ["left", "right"] {
        let mut reached_target = false;
        for index in 0..100 {
            let raw = qmp.read_memory(0, mixer_first_channel_pan, 1)?[0];
            let displayed = decode_text(text_table + 179 * 4)?;
            let target_reached = if name == "left" {
                displayed
                    .strip_prefix("L ")
                    .and_then(|value| value.strip_suffix('%'))
                    .and_then(|value| value.parse::<u8>().ok())
                    .is_some_and(|value| value >= 48)
            } else {
                displayed
                    .strip_prefix("R ")
                    .and_then(|value| value.strip_suffix('%'))
                    .and_then(|value| value.parse::<u8>().ok())
                    .is_some_and(|value| value >= 48)
            };
            if target_reached {
                reached_target = true;
                break;
            }
            let target = if name == "left" { 32 } else { 95 };
            inputs.knob(0, if raw < target { 1 } else { -1 })?;
            capture_demo(&format!("knob-dialog-demo-pan-{name}-{index:02}"))?;
        }
        if !reached_target {
            return Err(format!("demo pan sweep did not reach {name} 50%").into());
        }
        capture(500, &format!("knob-dialog-demo-pan-{name}-end"))?;
    }
    for _ in 0..100 {
        let raw = qmp.read_memory(0, mixer_first_channel_pan, 1)?[0];
        if raw == pan_start {
            break;
        }
        inputs.knob(0, if raw < pan_start { 1 } else { -1 })?;
        capture(50, "knob-dialog-demo-pan-reset")?;
    }
    inputs.tap(35, 50)?;
    wait_parameter_mode(9)?;
    let channel0_level = l6max::mixer_channel_levels;
    let channel1_level = l6max::mixer_channel_levels + 1;
    let channel0_raw = qmp.read_memory(0, channel0_level, 1)?[0];
    let mut channel1_raw = qmp.read_memory(0, channel1_level, 1)?[0];
    for delta in [-1, 1, -2, 2] {
        inputs.knob(1, delta)?;
        capture(250, "window-scene-channel-2-level")?;
        channel1_raw = qmp.read_memory(0, channel1_level, 1)?[0];
        if channel1_raw != channel0_raw {
            break;
        }
    }
    let channel1_display = decode_text(text_table + 179 * 4)?;
    if channel1_raw == channel0_raw
        || !knob_display_matches(9, &expected_value(9, channel1_raw), &channel1_display)
    {
        return Err(format!(
                "channel-2 event did not display its own LEVEL value: channel0={channel0_raw}, channel1={channel1_raw}, displayed={channel1_display:?}"
            )
            .into());
    }
    for (tag, description) in [(0x0f, "invalid mode 15"), (0xf0, "invalid channel 15")] {
        qmp.stop()?;
        debugger.write(state + 3, &[tag])?;
        qmp.resume()?;
        // The callback acknowledges discarded tags by clearing the mailbox.
        // Wait for that acknowledgement, rather than assuming it happened
        // after a fixed amount of guest time.
        const DISCARD_GUEST_TIMEOUT_MS: u32 = 10_000;
        const DISCARD_WALL_TIMEOUT_S: u64 = 180;
        const DISCARD_POLL_MS: u64 = 5;
        let start = inputs.panel.snapshot().guest_ms;
        let deadline =
            Instant::now() + Duration::from_secs(wall_timeout_s.min(DISCARD_WALL_TIMEOUT_S));
        loop {
            session.poll()?;
            if qmp.read_memory(0, state + 3, 1)? == u8::MAX.to_le_bytes() {
                break;
            }
            if inputs.panel.snapshot().guest_ms.wrapping_sub(start)
                >= guest_timeout_ms.min(DISCARD_GUEST_TIMEOUT_MS)
                || Instant::now() >= deadline
            {
                return Err(
                    format!("display callback did not safely discard {description}").into(),
                );
            }
            thread::sleep(Duration::from_millis(DISCARD_POLL_MS));
        }
        qmp.stop()?;
        let discarded = qmp.read_memory(0, state + 3, 1)? == u8::MAX.to_le_bytes();
        qmp.resume()?;
        if !discarded {
            return Err(format!("display callback did not safely discard {description}").into());
        }
    }
    // An encoder turn reaches the mailbox through the panel MCU, its UART
    // and the mixer, so a fixed delay can end before the dialog shows it.
    // A late event is held while a stock notification is active and then
    // changes the scene after expiry. Wait until the dialog has consumed
    // the event and shows the new mixer value.
    let settled_turn = |name: &str| -> Result<(), Box<dyn std::error::Error>> {
        let before = qmp.read_memory(0, channel0_level, 1)?[0];
        inputs.knob(0, if before == 0 { 1 } else { -1 })?;
        let start = inputs.panel.snapshot().guest_ms;
        loop {
            capture(50, name)?;
            let raw = qmp.read_memory(0, channel0_level, 1)?[0];
            if raw != before
                && qmp.read_memory(0, state + 3, 1)?[0] == u8::MAX
                && knob_display_matches(
                    9,
                    &expected_value(9, raw),
                    &decode_text(text_table + 179 * 4)?,
                )
            {
                return Ok(());
            }
            if inputs.panel.snapshot().guest_ms.wrapping_sub(start) >= 2_000 {
                return Err(format!(
                    "channel-1 LEVEL turn was not displayed: before={before}, now={raw}"
                )
                .into());
            }
        }
    };
    settled_turn("window-scene-before-invalid-state-metadata")?;
    qmp.stop()?;
    let metadata = qmp.read_memory(0, state, 4)?;
    let metadata_word = u32::from_le_bytes(metadata.clone().try_into().unwrap());
    let invalid_metadata = metadata_word ^ 1;
    debugger.write(state, &invalid_metadata.to_le_bytes())?;
    qmp.resume()?;
    let invalid_metadata_start = inputs.panel.snapshot().guest_ms;
    wait_guest(
        session,
        invalid_metadata_start,
        150,
        guest_timeout_ms,
        wall_timeout_s,
    )?;
    qmp.stop()?;
    // The payload returns borrowed descriptors after every draw, so they
    // cannot show a rejection; an accepted state rewrites its word and the
    // dialog's bitmap entry.
    let invalid_metadata_rejected = qmp.read_memory(0, state, 4)? == invalid_metadata.to_le_bytes()
        && qmp.read_memory(0, l6max::gui_bitmap_table_entry_0 + 36 * 4, 4)? == original_bitmap;
    debugger.write(state, &metadata)?;
    qmp.resume()?;
    if !invalid_metadata_rejected {
        return Err("the payload accepted an invalid state signature".into());
    }
    settled_turn("window-scene-after-invalid-state-metadata")?;
    let invalid_tag_discarded = true;
    let lit_pixels = |y0: usize, y1: usize| {
        (y0..y1)
            .flat_map(|y| {
                (0..128).map(move |x| {
                    let index = ((63 - y) / 8) * 128 + x;
                    refreshed[index] & (1 << ((63 - y) % 8)) != 0
                })
            })
            .filter(|lit| *lit)
            .count()
    };
    if lit_pixels(0, 17) < 10 || lit_pixels(30, 48) < 5 {
        return Err("shared text resource did not render both dialog lines".into());
    }
    let scene_text_before_notification = qmp.read_memory(0, shared_resource_pointer, 512)?;
    // Exercise the stock notification renderer over the layer-4 scene.
    // This calls the generic firmware API directly, without relying on
    // the patch's removed notification hooks.
    qmp.stop()?;
    let callback_entry = active_slot5 & !1;
    debugger.breakpoint(callback_entry, true)?;
    qmp.resume()?;
    let breakpoint_deadline = Instant::now() + Duration::from_secs(15);
    while qmp.query_status()?.running {
        session.poll()?;
        if Instant::now() >= breakpoint_deadline {
            return Err("dialog callback breakpoint was not reached".into());
        }
        thread::sleep(Duration::from_millis(5));
    }
    debugger.breakpoint(callback_entry, false)?;
    // Message 4 uses the three-line notification background, bitmap 36.
    debugger.register(0, 4)?;
    debugger.register(15, l6max::gui_notification_show)?;
    qmp.resume()?;
    let notification_start = inputs.panel.snapshot().guest_ms;
    let mut notification_frame = capture(100, "window-scene-stock-notification")?;
    while notification_frame == refreshed {
        let elapsed = inputs
            .panel
            .snapshot()
            .guest_ms
            .wrapping_sub(notification_start);
        if elapsed >= 1_500 {
            return Err("stock notification did not render its frame over the custom scene".into());
        }
        notification_frame = capture(100, "window-scene-stock-notification")?;
    }
    let callback_after_notification =
        qmp.read_memory(0, l6max::gui_display_callback_table + 20, 4)?;
    let resource_pointer_after_notification = qmp.read_memory(0, text_table + 179 * 4, 4)?;
    let resource_after_notification = qmp.read_memory(0, shared_resource_pointer, 512)?;
    if callback_after_notification != active_slot5.to_le_bytes()
        || resource_pointer_after_notification != shared_resource_pointer.to_le_bytes()
        || resource_after_notification != scene_text_before_notification
    {
        return Err(format!(
                "stock notification changed scene state: callback {callback_after_notification:02x?} vs {active_slot5:08x}, resource pointer {resource_pointer_after_notification:02x?} vs {shared_resource_pointer:08x}, resource modified={}",
                resource_after_notification != scene_text_before_notification
            ).into());
    }
    // The dialog has its own inactivity timeout. Wait for the notification
    // frame to clear instead of sleeping past that timeout as well.
    let after_notification = loop {
        let frame = capture(100, "window-scene-after-notification")?;
        if frame != notification_frame {
            break frame;
        }
        let elapsed = inputs
            .panel
            .snapshot()
            .guest_ms
            .wrapping_sub(notification_start);
        if elapsed >= 5_000 {
            return Err("stock notification did not clear after its timeout".into());
        }
    };
    let callback_after_expiry = qmp.read_memory(0, l6max::gui_display_callback_table + 20, 4)?;
    let resource_pointer_after_expiry = qmp.read_memory(0, text_table + 179 * 4, 4)?;
    let resource_after_expiry = qmp.read_memory(0, shared_resource_pointer, 512)?;
    if callback_after_expiry != active_slot5.to_le_bytes()
        || resource_pointer_after_expiry != shared_resource_pointer.to_le_bytes()
        || resource_after_expiry != scene_text_before_notification
    {
        let utf16 = |bytes: &[u8]| -> String {
            bytes
                .chunks(2)
                .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
                .take_while(|unit| *unit != 0)
                .take(24)
                .map(|unit| char::from_u32(unit.into()).unwrap_or('?'))
                .collect()
        };
        return Err(format!(
                "stock notification expiry damaged or prematurely removed the custom scene: callback {callback_after_expiry:02x?} vs {active_slot5:08x}, resource pointer {resource_pointer_after_expiry:02x?} vs {shared_resource_pointer:08x}, resource {:?} vs {:?}, ticks {}, window {:02x?}, notification active {} busy {}, guest ms since notification {}",
                utf16(&resource_after_expiry),
                utf16(&scene_text_before_notification),
                qmp.read_memory(0, state + 2, 1)?[0],
                qmp.read_memory(0, l6max::current_window, 4)?,
                qmp.read_memory(0, l6max::notification_active, 1)?[0],
                qmp.read_memory(0, l6max::notification_busy, 1)?[0],
                inputs.panel.snapshot().guest_ms.wrapping_sub(notification_start)
            ).into());
    }
    let timeout_start = inputs.panel.snapshot().guest_ms;
    loop {
        let ticks = qmp.read_memory(0, state + 2, 1)?[0];
        if ticks == 0 {
            break;
        }
        let now = inputs.panel.snapshot().guest_ms;
        // The guest countdown can land on its final tick while host-side
        // QEMU work is contending with other parallel cases. Keep a full
        // grace interval for that last tick before treating it as stuck.
        if now.wrapping_sub(timeout_start) >= 10_000 {
            return Err(format!(
                    "custom scene timeout did not finish after notification expiry; {ticks} ticks remain"
                )
                .into());
        }
        wait_guest(session, now, 100, guest_timeout_ms, wall_timeout_s)?;
    }
    let timeout_cleared = capture(150, "knob-dialog-timeout-cleared")?;
    if timeout_cleared == after_notification {
        return Err("knob dialog timeout screenshot still shows the custom scene".into());
    }
    if timeout_cleared == notification_frame {
        return Err("knob dialog timeout screenshot still shows the notification".into());
    }
    let restore_checks = [
        (
            "window",
            qmp.read_memory(0, l6max::current_window, 4)? == original_window,
        ),
        (
            "state signature",
            u16::from_le_bytes(qmp.read_memory(0, state, 2)?.try_into().unwrap()) == 0x4b4e,
        ),
        (
            "bitmap table",
            qmp.read_memory(0, l6max::gui_bitmap_table_entry_0 + 36 * 4, 4)? == original_bitmap,
        ),
        (
            "bitmap descriptor",
            qmp.read_memory(
                0,
                u32::from_le_bytes(original_bitmap.clone().try_into().unwrap()),
                8,
            )? == original_bitmap_descriptor,
        ),
        (
            "notification semaphore",
            qmp.read_memory(0, l6max::notification_slot, 4)? == original_notification_semaphore,
        ),
        (
            "text resource table",
            qmp.read_memory(0, text_table + 229 * 4, 8)? == original_resources,
        ),
        (
            "shared resource pointer",
            qmp.read_memory(0, text_table + 179 * 4, 4)? == shared_resource_pointer.to_le_bytes(),
        ),
        (
            "shared resource contents",
            qmp.read_memory(0, shared_resource_pointer, 512)? == original_shared_resource,
        ),
        (
            "text lengths",
            qmp.read_memory(0, original_lengths_table + 229 * 2, 4)? == original_lengths,
        ),
        (
            "widget 7",
            qmp.read_memory(0, widget7, 16)? == original_widget7,
        ),
        (
            "widget 33",
            qmp.read_memory(0, widget33, 16)? == original_widget33,
        ),
        (
            "background layer 10",
            qmp.read_memory(0, background10, 1)? == original_background_layers[0],
        ),
        (
            "background layer 37",
            qmp.read_memory(0, background37, 1)? == original_background_layers[1],
        ),
        (
            "background layer 48",
            qmp.read_memory(0, background48, 1)? == original_background_layers[2],
        ),
        (
            "display callback slot 5",
            u32::from_le_bytes(
                qmp.read_memory(0, l6max::gui_display_callback_table + 20, 4)?
                    .try_into()
                    .unwrap(),
            ) == active_slot5,
        ),
    ];
    let mismatches = restore_checks
        .iter()
        .filter_map(|(name, matches)| (!matches).then_some(*name))
        .collect::<Vec<_>>();
    if !mismatches.is_empty() {
        return Err(format!(
                "custom overlay did not restore borrowed resources or preserve stock bitmap 36: {mismatches:?}"
            )
            .into());
    }
    let heap_after = heap()?;
    if heap_after[0] != heap_live[0]
        || heap_after[2] != heap_live[2]
        || heap_after[3] != heap_live[3]
    {
        return Err(format!(
            "in-place scene state caused heap changes: live={heap_live:?}, after={heap_after:?}"
        )
        .into());
    }
    // Prove that the DisplayCycle2 callback restores borrowed global
    // descriptors on navigation, rather than relying on expiry cleanup.
    inputs.knob(0, -3)?;
    capture(400, "window-scene-before-navigation")?;
    inputs.main.tap(0, 120)?;
    capture(500, "window-scene-navigation-returned")?;
    let navigated_window = qmp.read_memory(0, l6max::current_window, 4)?;
    if navigated_window == original_window
        || u16::from_le_bytes(qmp.read_memory(0, state, 2)?.try_into().unwrap()) != 0x4b4e
        || qmp.read_memory(0, l6max::gui_bitmap_table_entry_0 + 36 * 4, 4)? != original_bitmap
        || qmp.read_memory(
            0,
            u32::from_le_bytes(original_bitmap.clone().try_into().unwrap()),
            8,
        )? != original_bitmap_descriptor
        || qmp.read_memory(0, l6max::notification_slot, 4)? != original_notification_semaphore
        || qmp.read_memory(0, shared_resource_pointer, 512)? != original_shared_resource
        || qmp.read_memory(0, widget7, 16)? != original_widget7
        || qmp.read_memory(0, widget33, 16)? != original_widget33
        || qmp.read_memory(0, background10, 1)? != original_background_layers[0]
        || qmp.read_memory(0, background37, 1)? != original_background_layers[1]
        || qmp.read_memory(0, background48, 1)? != original_background_layers[2]
        || u32::from_le_bytes(
            qmp.read_memory(0, l6max::gui_display_callback_table + 20, 4)?
                .try_into()
                .unwrap(),
        ) != active_slot5
    {
        return Err("window-push cleanup did not restore the dialog's borrowed resources".into());
    }
    return Ok(json!({
        "overlay_layer": 4,
        "black_background": true,
        "notification_semaphore_not_required_for_first_use": true,
        "callback_preflight_race_rejected": callback_preflight_race_rejected,
        "notification_semaphore_not_required_for_refresh": true,
        "stock_notification_overlap": true,
        "stock_notification_expiry": true,
        "bitmap_zero_guard_safe": true,
        "timeout_return": true,
        "window_change_restore": true,
        "non_recorder_first_use_rejected": non_recorder_guard_passed,
        "dialog_active_screenshot": "knob-dialog-active.png",
        "dialog_timeout_screenshot": "knob-dialog-timeout-cleared.png",
        "resource_restore": true,
        "shared_resource_reuse": true,
        "shared_resource_alias_rejected": true,
        "scene_layer_collision_rejected": true,
        "changed_widget_layer_rejected": widget_layer_guard_rejected,
        "changed_background_layer_rejected": background_layer_guard_rejected,
        "nonzero_channel_value_matches": true,
        "invalid_mailbox_tag_discarded": invalid_tag_discarded,
        "invalid_state_signature_rejected": invalid_metadata_rejected,
        "stock_notification_bitmap_untouched": true,
        "state_allocation_requested_bytes": 0,
        "state_heap_bytes_consumed": state_heap_bytes,
        "state_retained_for_boot": true,
        "display_callback_slot": 5,
        "display_callback_target": format!("{active_slot5:#010x}"),
        "mixer_hook_task_control_block": format!("{hook_task:#010x}"),
        "display_callback_task_control_block": format!("{callback_task:#010x}"),
        "mixer_and_callback_share_task": hook_task == callback_task,
        "display_callback_remains_registered": true
    }));
}

pub(super) fn knob_display_matches(mode: u32, expected: &str, displayed: &str) -> bool {
    if expected == displayed {
        return true;
    }
    if mode != 1 {
        return false;
    }
    let Some((expected_number, expected_unit)) = expected.rsplit_once(' ') else {
        return false;
    };
    let Some((displayed_number, displayed_unit)) = displayed.rsplit_once(' ') else {
        return false;
    };
    if expected_unit != displayed_unit {
        return false;
    }
    let (Ok(expected), Ok(displayed)) = (
        expected_number.parse::<f32>(),
        displayed_number.parse::<f32>(),
    ) else {
        return false;
    };
    let tolerance = match expected_unit {
        "Hz" => 1.0,
        "kHz" => 0.01,
        _ => return false,
    };
    (expected - displayed).abs() <= tolerance
}

/// Evidence and original overall deadlines for the popup's declarative steps.
pub(super) struct PopupContext {
    before: [u8; 1024],
    level_before: u8,
    turns: Vec<i32>,
    guest_start: u32,
    wall_deadline: Instant,
    guest_timeout_ms: u32,
}

impl PopupContext {
    fn expired(&self, now: Instant, guest: u32) -> bool {
        now > self.wall_deadline || guest.wrapping_sub(self.guest_start) > self.guest_timeout_ms
    }
}

fn popup_active(snapshot: &l6ctl::KnobPopupSnapshot) -> bool {
    u16::from_le_bytes([snapshot.state[0], snapshot.state[1]]) == 0x4b4e
        && snapshot.callback & 1 == 1
}

fn popup_opened(
    snapshot: &l6ctl::KnobPopupSnapshot,
    before: &[u8; 1024],
    after: &[u8; 1024],
    level_before: u8,
) -> bool {
    popup_active(snapshot) && after != before && snapshot.level != level_before
}

fn popup_value_matches(snapshot: &l6ctl::KnobPopupSnapshot, level_before: u8) -> bool {
    let expected = format!("{}%", (u32::from(snapshot.level) * 100 + 63) / 127);
    snapshot.state[3] == u8::MAX
        && snapshot.level != level_before
        && snapshot.text.as_deref() == Some(expected.as_str())
}

fn attempt_popup_turns(
    first_steps: i32,
    fallback_steps: i32,
    mut attempt: impl FnMut(i32) -> Result<bool, String>,
) -> Result<Vec<i32>, String> {
    let mut turns = Vec::new();
    for delta in [first_steps, fallback_steps] {
        turns.push(delta);
        if attempt(delta)? {
            break;
        }
    }
    Ok(turns)
}

impl State {
    pub(super) fn run_knob_popup(
        &mut self,
        session: &Session,
        operation: Operation,
        guest_timeout_ms: u32,
        wall_timeout_s: u64,
    ) -> Result<Value, Box<dyn std::error::Error>> {
        let inputs = session
            .engine
            .inputs
            .as_ref()
            .ok_or("knob popup needs inputs")?;
        let step_started = Instant::now();
        let step_guest = inputs.panel.snapshot().guest_ms;
        let control = l6ctl::OwnedControl::new(&session.engine);
        let reader = control.knob_popup_reader()?;
        if let Operation::KnobPopupOpen {
            first_steps,
            fallback_steps,
            attempt_guest_ms,
        } = operation
        {
            let guest_start = step_guest;
            let wall_deadline = step_started + Duration::from_secs(wall_timeout_s);
            let initial = reader.snapshot(false).map_err(std::io::Error::other)?;
            if initial.window != l6max::GUI_RECORDER_WINDOW {
                return Err(format!(
                    "knob-dialog-popup needs the recorder screen, found window {:#010x}",
                    initial.window
                )
                .into());
            }
            let before = session.poll()?.display;
            let level_before = reader.snapshot(false).map_err(std::io::Error::other)?.level;
            let mut context = PopupContext {
                before,
                level_before,
                turns: Vec::new(),
                guest_start,
                wall_deadline,
                guest_timeout_ms,
            };
            // InputHub::knob reverses panel encoder polarity; preserve that
            // exact convention while using l6ctl's one-based encoder API.
            context.turns = attempt_popup_turns(first_steps, fallback_steps, |delta| {
                if context.expired(Instant::now(), inputs.panel.snapshot().guest_ms) {
                    return Err(
                        "knob-dialog popup exceeded its shared deadline before encoder input"
                            .into(),
                    );
                }
                control
                    .encoder(1, -delta)
                    .map_err(|error| error.to_string())?;
                let attempt_start = inputs.panel.snapshot().guest_ms;
                let attempt_deadline = Instant::now() + Duration::from_secs(wall_timeout_s);
                loop {
                    let display = session.poll()?.display;
                    let snapshot = reader.snapshot(false)?;
                    if popup_opened(&snapshot, &context.before, &display, context.level_before) {
                        return Ok(true);
                    }
                    let guest = inputs.panel.snapshot().guest_ms;
                    if guest.wrapping_sub(attempt_start) >= attempt_guest_ms {
                        return Ok(false);
                    }
                    if Instant::now() >= attempt_deadline
                        || guest.wrapping_sub(attempt_start) >= guest_timeout_ms
                        || context.expired(Instant::now(), guest)
                    {
                        return Err("knob-dialog popup observation exceeded its deadline".into());
                    }
                    thread::sleep(Duration::from_millis(5));
                }
            })?;
            let turns = context.turns.clone();
            self.knob_popup = Some(context);
            return Ok(json!({"turns": turns, "level_before": level_before}));
        }
        let context = self
            .knob_popup
            .as_ref()
            .ok_or("knob popup open step has not run")?;
        if context.expired(Instant::now(), inputs.panel.snapshot().guest_ms) {
            return Err("knob-dialog popup exceeded its shared deadline".into());
        }
        match operation {
            Operation::KnobPopupExpectOpen => {
                let snapshot = reader.snapshot(false).map_err(std::io::Error::other)?;
                if !popup_active(&snapshot) {
                    return Err(format!("Knob Dialog did not open after turning channel 1 level {:?}: state={:02x?}, slot5={:#010x}", context.turns, &snapshot.state[..2], snapshot.callback).into());
                }
                if session.poll()?.display == context.before {
                    return Err("Knob Dialog opened but the display did not change".into());
                }
                if context.expired(Instant::now(), inputs.panel.snapshot().guest_ms) {
                    return Err("knob-dialog popup exceeded its shared deadline".into());
                }
                Ok(
                    json!({"state":snapshot.state,"callback":snapshot.callback,"display_changed":true}),
                )
            }
            Operation::KnobPopupExpectValue => {
                let start = inputs.panel.snapshot().guest_ms;
                let deadline = (Instant::now() + Duration::from_secs(wall_timeout_s.min(180)))
                    .min(context.wall_deadline);
                loop {
                    session.poll()?;
                    let snapshot = reader.snapshot(true).map_err(std::io::Error::other)?;
                    let displayed = snapshot.text.as_deref().unwrap_or_default();
                    let raw = snapshot.level;
                    let expected = format!("{}%", (u32::from(raw) * 100 + 63) / 127);
                    if popup_value_matches(&snapshot, context.level_before) {
                        if context.expired(Instant::now(), inputs.panel.snapshot().guest_ms) {
                            return Err("knob-dialog popup exceeded its shared deadline".into());
                        }
                        let result =
                            json!({"turns":context.turns,"level_raw":raw,"displayed":displayed});
                        self.knob_popup = None;
                        return Ok(result);
                    }
                    let guest = inputs.panel.snapshot().guest_ms;
                    if guest.wrapping_sub(start) >= guest_timeout_ms.min(10_000)
                        || Instant::now() >= deadline
                        || context.expired(Instant::now(), guest)
                    {
                        return Err(format!("Knob Dialog shows {displayed:?} for channel 1 level {raw}, expected {expected:?}").into());
                    }
                    thread::sleep(Duration::from_millis(5));
                }
            }
            _ => unreachable!(),
        }
    }
}

#[cfg(test)]
mod popup_step_tests {
    use super::*;
    #[test]
    fn tries_reverse_only_after_first_attempt_fails() {
        let mut directions = Vec::new();
        let turns = attempt_popup_turns(-2, 4, |delta| {
            directions.push(delta);
            Ok(true)
        })
        .unwrap();
        assert_eq!(turns, [-2]);
        assert_eq!(directions, [-2]);
        let mut directions = Vec::new();
        let turns = attempt_popup_turns(-2, 4, |delta| {
            directions.push(delta);
            Ok(delta == 4)
        })
        .unwrap();
        assert_eq!(turns, [-2, 4]);
        assert_eq!(directions, [-2, 4]);
        assert!(
            attempt_popup_turns(-2, 4, |_| Err("timeout".into()))
                .unwrap_err()
                .contains("timeout")
        );
    }

    #[test]
    fn open_requires_state_callback_display_and_level() {
        let mut snapshot = l6ctl::KnobPopupSnapshot {
            revision_guard: "test".into(),
            atomic: false,
            window: 0,
            state: [0x4e, 0x4b, 1, 0xff],
            callback: 1,
            level: 33,
            text: None,
        };
        let before = [0; 1024];
        let mut after = before;
        after[0] = 1;
        assert!(popup_opened(&snapshot, &before, &after, 32));
        assert!(!popup_opened(&snapshot, &before, &before, 32));
        assert!(!popup_opened(&snapshot, &before, &after, 33));
        snapshot.callback = 0;
        assert!(!popup_opened(&snapshot, &before, &after, 32));
        snapshot.callback = 1;
        snapshot.state[0] = 0;
        assert!(!popup_opened(&snapshot, &before, &after, 32));
    }
    #[test]
    fn rendered_value_requires_ready_changed_level_and_exact_text() {
        let mut snapshot = l6ctl::KnobPopupSnapshot {
            revision_guard: "test".into(),
            atomic: false,
            window: 0,
            state: [0x4e, 0x4b, 1, 0xff],
            callback: 1,
            level: 64,
            text: Some("50%".into()),
        };
        assert!(popup_value_matches(&snapshot, 63));
        assert!(!popup_value_matches(&snapshot, 64));
        snapshot.text = Some("49%".into());
        assert!(!popup_value_matches(&snapshot, 63));
        snapshot.text = Some("50%".into());
        snapshot.state[3] = 0;
        assert!(!popup_value_matches(&snapshot, 63));
    }

    #[test]
    fn popup_shared_deadline_is_bounded_by_wall_and_guest_time() {
        let now = Instant::now();
        let context = PopupContext {
            before: [0; 1024],
            level_before: 0,
            turns: vec![],
            guest_start: u32::MAX - 4,
            wall_deadline: now + Duration::from_secs(2),
            guest_timeout_ms: 10,
        };
        assert!(!context.expired(now + Duration::from_secs(1), 5));
        assert!(context.expired(now + Duration::from_secs(3), 5));
        assert!(context.expired(now + Duration::from_secs(1), 6));
    }
}

#[cfg(test)]
mod recorder_ready_tests {
    use super::recorder_controls_visible;
    #[test]
    fn compares_only_the_reviewed_transport_crop() {
        let reference = image::GrayImage::from_pixel(128, 64, image::Luma([0]));
        let mut display = [0; 1024];
        assert!(recorder_controls_visible(&display, &reference));
        display[((63 - 4) / 8) * 128] ^= 1 << ((63 - 4) % 8);
        assert!(recorder_controls_visible(&display, &reference));
        display[((63 - 20) / 8) * 128] ^= 1 << ((63 - 20) % 8);
        assert!(!recorder_controls_visible(&display, &reference));
    }
}
