use core::sync::atomic::{AtomicU32, Ordering};

use super::velvet::{Parameters, SAMPLE_RATE, VelvetDecorrelator};

pub const PARAMETER_COUNT: usize = 7;
static SAVED: [AtomicU32; PARAMETER_COUNT] = [const { AtomicU32::new(u32::MAX) }; PARAMETER_COUNT];

unsafe fn processor<'a>(state: *mut u8) -> &'a mut VelvetDecorrelator {
    unsafe { &mut *state.cast::<VelvetDecorrelator>() }
}

pub unsafe fn initialize(state: *mut u8) {
    let mut params = Parameters::default();
    for (index, value) in SAVED.iter().enumerate() {
        let bits = value.load(Ordering::Acquire);
        if bits == u32::MAX {
            continue;
        }
        let value = f32::from_bits(bits);
        match index {
            0 => params.amount = value,
            1 => params.length_ms = value,
            2 => params.density = value,
            3 => params.decay_db = value,
            4 => params.refresh_seconds = value,
            5 => params.dynamic = value != 0.0,
            6 => params.transition_ms = value,
            _ => unreachable!(),
        }
    }
    unsafe { processor(state) }.init(params);
}

pub unsafe fn parameter_ring_position(state: *mut u8, encoder: usize) -> Option<u32> {
    let processor = unsafe { processor(state) };
    let params = processor.current;
    let (value, min, max) = match encoder {
        0 => (params.amount * 100.0, 0.0, 100.0),
        1 => (params.length_ms, 1.0, 100.0),
        2 => (
            processor.tap_count(params.density) as f32
                * (SAMPLE_RATE / processor.current_tail_samples as f32),
            10.0,
            4600.0,
        ),
        3 => (params.decay_db, -120.0, 0.0),
        4 => (params.refresh_seconds, 0.1, 60.0),
        5 => return Some(if params.dynamic { 40 << 16 } else { 0 }),
        6 => (params.transition_ms, 1.0, 54000.0),
        _ => return None,
    };
    Some(ring_q16(value, min, max))
}

pub unsafe fn edit_parameter(state: *mut u8, encoder: usize, delta: i32) -> Option<u32> {
    if delta == 0 || encoder >= PARAMETER_COUNT {
        return None;
    }
    let processor = unsafe { processor(state) };
    let mut params = processor.current;
    let step = delta as f32;
    match encoder {
        0 => params.amount = clamp(params.amount + step * 0.02, 0.0, 1.0),
        1 => params.length_ms = clamp(params.length_ms + step, 1.0, 100.0),
        2 => params.density = clamp(params.density + step * 50.0, 100.0, 4000.0),
        3 => params.decay_db = clamp(params.decay_db + step * 2.0, -120.0, 0.0),
        4 => params.refresh_seconds = clamp(params.refresh_seconds + step * 0.5, 0.1, 60.0),
        5 => params.dynamic = delta > 0,
        6 => params.transition_ms = clamp(params.transition_ms + step * 25.0, 1.0, params.refresh_seconds * 900.0),
        _ => unreachable!(),
    }
    processor.set_parameters(params);
    let applied = processor.current;
    let value = match encoder {
        0 => applied.amount,
        1 => applied.length_ms,
        2 => applied.density,
        3 => applied.decay_db,
        4 => applied.refresh_seconds,
        5 => {
            if applied.dynamic {
                1.0
            } else {
                0.0
            }
        }
        6 => applied.transition_ms,
        _ => unreachable!(),
    };
    SAVED[encoder].store(value.to_bits(), Ordering::Release);
    let (title, text, len) = match encoder {
        0 => { let (v, n) = format_unit(applied.amount * 100.0, b" %"); (b"VND AMOUNT".as_slice(), v, n) },
        1 => { let (v, n) = format_unit(applied.length_ms, b" ms"); (b"VND TIME SPREAD".as_slice(), v, n) },
        2 => {
            let density = processor.tap_count(applied.density) as f32
                * (SAMPLE_RATE / processor.current_tail_samples as f32);
            let (v, n) = format_unit(density, b" pps");
            (b"VND DENSITY".as_slice(), v, n)
        }
        3 => { let (v, n) = format_unit(applied.decay_db, b" dB"); (b"VND DECAY".as_slice(), v, n) },
        4 => { let (v, n) = format_unit(applied.refresh_seconds, b" s"); (b"VND REFRESH".as_slice(), v, n) },
        5 => {
            let value = if applied.dynamic { b"DYNAMIC".as_slice() } else { b"STATIC".as_slice() };
            let mut text = [0; 16];
            text[..value.len()].copy_from_slice(value);
            (b"VND MODE".as_slice(), text, value.len())
        }
        6 => { let (v, n) = format_unit(applied.transition_ms, b" ms"); (b"VND TRANSITION".as_slice(), v, n) },
        _ => unreachable!(),
    };
    show_external_text(title, &text[..len]);
    unsafe { parameter_ring_position(state, encoder) }
}

pub unsafe fn show_editor_state(enabled: bool) {
    show_external_text(b"VELVET EDIT", if enabled { b"ON" } else { b"OFF" });
}

fn show_external_text(title: &[u8], value: &[u8]) {
    let _ = crate::knob_dialog::prepare_external_text();
    let _ = crate::knob_dialog::show_external_text(title, value);
}

fn ring_q16(value: f32, min: f32, max: f32) -> u32 {
    (clamp((value - min) / (max - min), 0.0, 1.0) * (40.0 * 65536.0) + 0.5) as u32
}

fn clamp(value: f32, min: f32, max: f32) -> f32 {
    if value < min {
        min
    } else if value > max {
        max
    } else {
        value
    }
}

fn format_unit(value: f32, unit: &[u8]) -> ([u8; 16], usize) {
    let mut out = [0; 16];
    let mut len = 0;
    let scaled = (value.abs() * 10.0 + 0.5) as u32;
    if value < 0.0 {
        out[len] = b'-';
        len += 1;
    }
    len += push_integer(&mut out[len..], scaled / 10);
    out[len] = b'.';
    len += 1;
    out[len] = b'0' + (scaled % 10) as u8;
    len += 1;
    let copy_len = unit.len().min(out.len() - len);
    out[len..len + copy_len].copy_from_slice(&unit[..copy_len]);
    len += copy_len;
    (out, len)
}

fn push_integer(out: &mut [u8], mut value: u32) -> usize {
    let mut digits = [0u8; 10];
    let mut count = 0;
    if value == 0 {
        out[0] = b'0';
        return 1;
    }
    while value > 0 {
        digits[count] = b'0' + (value % 10) as u8;
        count += 1;
        value /= 10;
    }
    for (target, digit) in out.iter_mut().zip(digits[..count].iter().rev()) {
        *target = *digit;
    }
    count
}
