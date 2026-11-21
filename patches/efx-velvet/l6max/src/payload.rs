// One math module for every effect, from efx-loader.
use crate::efx_loader::math;
#[path = "controls.rs"]
mod controls;
#[path = "velvet.rs"]
mod velvet;

pub const EFFECT_STATE_BYTES: usize = core::mem::size_of::<velvet::VelvetDecorrelator>();
/// Knob editing, through efx-loader's shared controls.
pub use controls::CONTROLS;

/// Initialize Velvet's state in the stock shared effect workspace.
pub unsafe fn initialize_effect(state: *mut u8) {
    let processor = unsafe { &mut *state.cast::<velvet::VelvetDecorrelator>() };
    unsafe { core::ptr::write_bytes(processor, 0, 1) };
    unsafe { controls::initialize(state) };
}

/// Process one mono stock send and return the wet stereo effect output.
pub unsafe fn process_sample(state: *mut u8, input: f32) -> (f32, f32) {
    unsafe { &mut *state.cast::<velvet::VelvetDecorrelator>() }.process_sample(input, input)
}
