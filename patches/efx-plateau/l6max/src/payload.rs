// One math module for every effect, from efx-loader.
use crate::efx_loader::math;
#[path = "controls.rs"]
mod controls;
#[path = "plateau.rs"]
mod plateau;

pub const EFFECT_STATE_BYTES: usize = core::mem::size_of::<plateau::PlateauReverb>();
/// Knob editing, through efx-loader's shared controls.
pub use controls::CONTROLS;

/// Initialize Plateau's state in the stock shared effect workspace.
pub unsafe fn initialize_effect(state: *mut u8) {
    let processor = unsafe { &mut *state.cast::<plateau::PlateauReverb>() };
    unsafe { core::ptr::write_bytes(processor, 0, 1) };
    unsafe { controls::initialize(state) };
}

/// Process one mono stock send and return the wet stereo effect output.
pub unsafe fn process_sample(state: *mut u8, input: f32) -> (f32, f32) {
    unsafe { &mut *state.cast::<plateau::PlateauReverb>() }.process_sample(input, input)
}
