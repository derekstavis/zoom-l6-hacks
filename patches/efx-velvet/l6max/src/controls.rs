//! Velvet's knob parameters, edited through efx-loader's shared controls.
use core::sync::atomic::AtomicU32;

use super::velvet::{
    MAX_TRANSITION_MS_PER_REFRESH_SECOND, Parameters, SAMPLE_RATE, VelvetDecorrelator,
};
use crate::efx_loader::controls::{Effect, Kind, Parameter};

const MIN_DENSITY_PPS: f32 = 100.0;
const MAX_DENSITY_PPS: f32 = 4000.0;
const DENSITY: usize = 2;
const TRANSITION: usize = 6;

const fn number(
    title: &'static [u8],
    step: f32,
    min: f32,
    max: f32,
    scale: f32,
    unit: &'static [u8],
) -> Parameter {
    Parameter {
        title,
        kind: Kind::Number {
            step,
            min,
            max,
            scale,
            unit,
            ring_min: min * scale,
            ring_max: max * scale,
        },
    }
}

const PARAMETERS: [Parameter; 7] = [
    number(b"VND AMOUNT", 0.02, 0.0, 1.0, 100.0, b" %"),
    number(b"VND TIME SPREAD", 1.0, 1.0, 100.0, 1.0, b" ms"),
    number(b"VND DENSITY", 50.0, MIN_DENSITY_PPS, MAX_DENSITY_PPS, 1.0, b" pps"),
    number(b"VND DECAY", 2.0, -120.0, 0.0, 1.0, b" dB"),
    number(b"VND REFRESH", 0.5, 0.1, 60.0, 1.0, b" s"),
    Parameter {
        title: b"VND MODE",
        kind: Kind::Toggle {
            on: b"DYNAMIC",
            off: b"STATIC",
        },
    },
    // Its maximum follows the refresh interval; see `dynamic_max`.
    number(b"VND TRANSITION", 25.0, 1.0, 1.0, 1.0, b" ms"),
];
const PARAMETER_COUNT: usize = PARAMETERS.len();
static SAVED: [AtomicU32; PARAMETER_COUNT] =
    [const { AtomicU32::new(u32::MAX) }; PARAMETER_COUNT];

pub static CONTROLS: Effect = Effect {
    parameters: &PARAMETERS,
    editor_title: b"VELVET EDIT",
    saved: &SAVED,
    value,
    apply,
    dynamic_max,
    shown,
};

unsafe fn processor<'a>(state: *mut u8) -> &'a mut VelvetDecorrelator {
    unsafe { &mut *state.cast::<VelvetDecorrelator>() }
}

fn assign(params: &mut Parameters, index: usize, value: f32) {
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

unsafe fn value(state: *mut u8, index: usize) -> f32 {
    let params = unsafe { processor(state) }.current;
    match index {
        0 => params.amount,
        1 => params.length_ms,
        2 => params.density,
        3 => params.decay_db,
        4 => params.refresh_seconds,
        5 => {
            if params.dynamic {
                1.0
            } else {
                0.0
            }
        }
        6 => params.transition_ms,
        _ => unreachable!(),
    }
}

unsafe fn apply(state: *mut u8, index: usize, value: f32) {
    let processor = unsafe { processor(state) };
    let mut params = processor.current;
    assign(&mut params, index, value);
    processor.set_parameters(params);
}

unsafe fn dynamic_max(state: *mut u8, index: usize) -> Option<f32> {
    (index == TRANSITION).then(|| {
        unsafe { processor(state) }.current.refresh_seconds * MAX_TRANSITION_MS_PER_REFRESH_SECOND
    })
}

/// Density shows the pulse rate the processor actually achieves.
unsafe fn shown(state: *mut u8, index: usize, value: f32) -> Option<f32> {
    (index == DENSITY).then(|| {
        let processor = unsafe { processor(state) };
        processor.tap_count(value) as f32 * (SAMPLE_RATE / processor.current_tail_samples as f32)
    })
}

pub unsafe fn initialize(state: *mut u8) {
    let mut params = Parameters::default();
    for index in 0..PARAMETER_COUNT {
        if let Some(value) = CONTROLS.saved(index) {
            assign(&mut params, index, value);
        }
    }
    unsafe { processor(state) }.init(params);
}
