//! Plateau's knob parameters, edited through efx-loader's shared controls.
use core::sync::atomic::AtomicU32;

use super::plateau::{MAX_PREDELAY_MS, MAX_SIZE, PlateauParameters, PlateauReverb};
use crate::efx_loader::controls::{Effect, Kind, Parameter, none, scaled};

const fn number(
    title: &'static [u8],
    step: f32,
    min: f32,
    max: f32,
    scale: f32,
    unit: &'static [u8],
    ring: (f32, f32),
) -> Parameter {
    Parameter {
        title,
        kind: Kind::Number {
            step,
            min,
            max,
            scale,
            unit,
            ring_min: ring.0,
            ring_max: ring.1,
        },
    }
}

const PARAMETERS: [Parameter; 8] = [
    number(b"PLATEAU PREDELAY", 2.0, 0.0, MAX_PREDELAY_MS, 1.0, b" ms", (0.0, MAX_PREDELAY_MS)),
    number(b"PLATEAU SIZE", 0.02, 0.01, MAX_SIZE, 100.0, b" %", (1.0, MAX_SIZE * 100.0)),
    number(b"PLATEAU DECAY", 0.02, 0.1, 0.999, 100.0, b" %", (10.0, 99.9)),
    number(b"PLATEAU LOW CUT", 20.0, 20.0, 1000.0, 1.0, b" Hz", (20.0, 1000.0)),
    number(b"PLATEAU HIGH CUT", 250.0, 1000.0, 16000.0, 1.0, b" Hz", (1000.0, 16000.0)),
    number(b"PLATEAU DIFFUSION", 0.05, 0.0, 1.0, 100.0, b" %", (0.0, 100.0)),
    number(b"PLATEAU MOD DEPTH", 0.25, 0.0, 16.0, 1.0, b" samples", (0.0, 16.0)),
    Parameter {
        title: b"PLATEAU FREEZE",
        kind: Kind::Toggle {
            on: b"FROZEN",
            off: b"OFF",
        },
    },
];
const PARAMETER_COUNT: usize = PARAMETERS.len();
static SAVED: [AtomicU32; PARAMETER_COUNT] =
    [const { AtomicU32::new(u32::MAX) }; PARAMETER_COUNT];

pub static CONTROLS: Effect = Effect {
    parameters: &PARAMETERS,
    editor_title: b"PLATEAU EDIT",
    saved: &SAVED,
    value,
    apply,
    dynamic_max: none,
    shown: scaled,
};

unsafe fn processor<'a>(state: *mut u8) -> &'a mut PlateauReverb {
    unsafe { &mut *state.cast::<PlateauReverb>() }
}

fn assign(params: &mut PlateauParameters, index: usize, value: f32) {
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

unsafe fn value(state: *mut u8, index: usize) -> f32 {
    let params = unsafe { processor(state) }.params;
    match index {
        0 => params.predelay_ms,
        1 => params.size,
        2 => params.decay,
        3 => params.low_cut_hz,
        4 => params.high_cut_hz,
        5 => params.diffusion,
        6 => params.mod_depth,
        7 => {
            if params.freeze {
                1.0
            } else {
                0.0
            }
        }
        _ => unreachable!(),
    }
}

/// Apply through the cheapest update that keeps the tail continuous.
unsafe fn apply(state: *mut u8, index: usize, value: f32) {
    let processor = unsafe { processor(state) };
    let mut params = processor.params;
    assign(&mut params, index, value);
    match index {
        0 => processor.set_predelay_live(params.predelay_ms),
        1 => processor.set_size_live(params.size),
        3 | 4 => processor.set_parameters(params),
        5 => processor.set_diffusion_live(params.diffusion),
        _ => processor.set_live_parameters(params),
    }
}

pub unsafe fn initialize(state: *mut u8) {
    let mut params = PlateauParameters::default();
    for index in 0..PARAMETER_COUNT {
        if let Some(value) = CONTROLS.saved(index) {
            assign(&mut params, index, value);
        }
    }
    unsafe { processor(state) }.init(params);
}
