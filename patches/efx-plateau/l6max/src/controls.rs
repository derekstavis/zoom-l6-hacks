use core::sync::atomic::{AtomicU32, Ordering};

use super::plateau::{MAX_PREDELAY_MS, MAX_SIZE, PlateauParameters, PlateauReverb};

pub const PARAMETER_COUNT: usize = 8;
const LOW_CUT_MIN_HZ: f32 = 20.0;
const LOW_CUT_MAX_HZ: f32 = 1000.0;
const HIGH_CUT_MIN_HZ: f32 = 1000.0;
const HIGH_CUT_MAX_HZ: f32 = 16000.0;
static SAVED: [AtomicU32; PARAMETER_COUNT] = [const { AtomicU32::new(u32::MAX) }; PARAMETER_COUNT];

unsafe fn processor<'a>(state: *mut u8) -> &'a mut PlateauReverb {
    unsafe { &mut *state.cast::<PlateauReverb>() }
}

pub unsafe fn initialize(state: *mut u8) {
    let mut params = PlateauParameters::default();
    for (index, value) in SAVED.iter().enumerate() {
        let bits = value.load(Ordering::Acquire);
        if bits == u32::MAX {
            continue;
        }
        let value = f32::from_bits(bits);
        match index {
            0 => params.predelay_ms = value,
            1 => params.size = value,
            2 => params.decay = value,
            3 => params.low_cut_hz = value,
            4 => params.high_cut_hz = value,
            5 => params.diffusion = value,
            6 => params.mod_depth = value,
            7 => params.freeze = value != 0.0,
            _ => unreachable!(),
        }
    }
    unsafe { processor(state) }.init(params);
}

pub unsafe fn parameter_ring_position(state: *mut u8, encoder: usize) -> Option<u32> {
    position_for_params(unsafe { processor(state) }.params, encoder)
}

pub unsafe fn preview_parameter_ring_position(
    state: *mut u8,
    encoder: usize,
    delta: i32,
) -> Option<u32> {
    if delta == 0 || encoder >= PARAMETER_COUNT {
        return None;
    }
    let mut params = unsafe { processor(state) }.params;
    let step = delta as f32;
    match encoder {
        0 => params.predelay_ms = clamp(params.predelay_ms + step * 2.0, 0.0, MAX_PREDELAY_MS),
        1 => params.size = clamp(params.size + step * 0.02, 0.01, MAX_SIZE),
        2 => params.decay = clamp(params.decay + step * 0.02, 0.1, 0.999),
        3 => {
            params.low_cut_hz = clamp(
                params.low_cut_hz + step * 20.0,
                LOW_CUT_MIN_HZ,
                LOW_CUT_MAX_HZ,
            )
        }
        4 => {
            params.high_cut_hz = clamp(
                params.high_cut_hz + step * 250.0,
                HIGH_CUT_MIN_HZ,
                HIGH_CUT_MAX_HZ,
            )
        }
        5 => params.diffusion = clamp(params.diffusion + step * 0.05, 0.0, 1.0),
        6 => params.mod_depth = clamp(params.mod_depth + step * 0.25, 0.0, 16.0),
        7 => params.freeze = delta > 0,
        _ => unreachable!(),
    }
    position_for_params(params, encoder)
}

fn position_for_params(params: PlateauParameters, encoder: usize) -> Option<u32> {
    let (value, min, max) = match encoder {
        0 => (params.predelay_ms, 0.0, MAX_PREDELAY_MS),
        1 => (params.size * 100.0, 1.0, MAX_SIZE * 100.0),
        2 => (params.decay * 100.0, 10.0, 99.9),
        3 => (params.low_cut_hz, LOW_CUT_MIN_HZ, LOW_CUT_MAX_HZ),
        4 => (params.high_cut_hz, HIGH_CUT_MIN_HZ, HIGH_CUT_MAX_HZ),
        5 => (params.diffusion * 100.0, 0.0, 100.0),
        6 => (params.mod_depth, 0.0, 16.0),
        7 => return Some(if params.freeze { 44 << 16 } else { 0 }),
        _ => return None,
    };
    Some(ring_q16(value, min, max))
}

pub unsafe fn edit_parameter(state: *mut u8, encoder: usize, delta: i32) -> Option<u32> {
    if delta == 0 || encoder >= PARAMETER_COUNT {
        return None;
    }
    let processor = unsafe { processor(state) };
    let mut params = processor.params;
    let step = delta as f32;
    match encoder {
        0 => {
            params.predelay_ms = clamp(params.predelay_ms + step * 2.0, 0.0, MAX_PREDELAY_MS);
            processor.set_predelay_live(params.predelay_ms);
        }
        1 => {
            params.size = clamp(params.size + step * 0.02, 0.01, MAX_SIZE);
            processor.set_size_live(params.size);
        }
        2 => {
            params.decay = clamp(params.decay + step * 0.02, 0.1, 0.999);
            processor.set_live_parameters(params);
        }
        3 => {
            params.low_cut_hz = clamp(
                params.low_cut_hz + step * 20.0,
                LOW_CUT_MIN_HZ,
                LOW_CUT_MAX_HZ,
            );
            processor.set_parameters(params);
        }
        4 => {
            params.high_cut_hz = clamp(
                params.high_cut_hz + step * 250.0,
                HIGH_CUT_MIN_HZ,
                HIGH_CUT_MAX_HZ,
            );
            processor.set_parameters(params);
        }
        5 => {
            params.diffusion = clamp(params.diffusion + step * 0.05, 0.0, 1.0);
            processor.set_diffusion_live(params.diffusion);
        }
        6 => {
            params.mod_depth = clamp(params.mod_depth + step * 0.25, 0.0, 16.0);
            processor.set_live_parameters(params);
        }
        7 => {
            params.freeze = delta > 0;
            processor.set_live_parameters(params);
        }
        _ => unreachable!(),
    }
    let applied = processor.params;
    let value = match encoder {
        0 => applied.predelay_ms,
        1 => applied.size,
        2 => applied.decay,
        3 => applied.low_cut_hz,
        4 => applied.high_cut_hz,
        5 => applied.diffusion,
        6 => applied.mod_depth,
        7 => {
            if applied.freeze {
                1.0
            } else {
                0.0
            }
        }
        _ => unreachable!(),
    };
    SAVED[encoder].store(value.to_bits(), Ordering::Release);
    let (title, value_text, value_len) = match encoder {
        0 => { let (v, n) = format_unit(applied.predelay_ms, b" ms"); (b"PLATEAU PREDELAY".as_slice(), v, n) },
        1 => { let (v, n) = format_unit(applied.size * 100.0, b" %"); (b"PLATEAU SIZE".as_slice(), v, n) },
        2 => { let (v, n) = format_unit(applied.decay * 100.0, b" %"); (b"PLATEAU DECAY".as_slice(), v, n) },
        3 => { let (v, n) = format_unit(applied.low_cut_hz, b" Hz"); (b"PLATEAU LOW CUT".as_slice(), v, n) },
        4 => { let (v, n) = format_unit(applied.high_cut_hz, b" Hz"); (b"PLATEAU HIGH CUT".as_slice(), v, n) },
        5 => { let (v, n) = format_unit(applied.diffusion * 100.0, b" %"); (b"PLATEAU DIFFUSION".as_slice(), v, n) },
        6 => { let (v, n) = format_unit(applied.mod_depth, b" samples"); (b"PLATEAU MOD DEPTH".as_slice(), v, n) },
        7 => {
            let value = if applied.freeze { b"FROZEN".as_slice() } else { b"OFF".as_slice() };
            let mut text = [0; 16];
            for (slot, byte) in text.iter_mut().zip(value) {
                *slot = *byte;
            }
            (b"PLATEAU FREEZE".as_slice(), text, value.len())
        }
        _ => unreachable!(),
    };
    show_external_text(title, &value_text[..value_len]);
    unsafe { parameter_ring_position(state, encoder) }
}

pub unsafe fn show_editor_state(enabled: bool) {
    show_external_text(
        b"PLATEAU EDIT",
        if enabled { b"ON" } else { b"OFF" },
    );
}

fn show_external_text(title: &[u8], value: &[u8]) {
    let _ = crate::knob_dialog::prepare_external_text();
    let _ = crate::knob_dialog::show_external_text(title, value);
}

fn ring_q16(value: f32, min: f32, max: f32) -> u32 {
    (clamp((value - min) / (max - min), 0.0, 1.0) * (44.0 * 65536.0) + 0.5) as u32
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
    for (slot, byte) in out[len..].iter_mut().zip(unit) {
        *slot = *byte;
    }
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
