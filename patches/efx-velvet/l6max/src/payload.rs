#[path = "math.rs"]
mod math;
#[path = "controls.rs"]
mod controls;
#[path = "velvet.rs"]
mod velvet;

pub const EFFECT_STATE_BYTES: usize = core::mem::size_of::<velvet::VelvetDecorrelator>();
pub const PARAMETER_COUNT: usize = controls::PARAMETER_COUNT;

/// Initialize Velvet's state in the stock shared effect workspace.
pub unsafe fn initialize_effect(state: *mut u8) {
    let processor = unsafe { &mut *state.cast::<velvet::VelvetDecorrelator>() };
    unsafe { core::ptr::write_bytes(processor, 0, 1) };
    controls::initialize(state);
}

/// Process one mono stock send and return the wet stereo effect output.
pub unsafe fn process_sample(state: *mut u8, input: f32) -> (f32, f32) {
    unsafe { &mut *state.cast::<velvet::VelvetDecorrelator>() }.process_sample(input, input)
}

pub unsafe fn parameter_ring_position(state: *mut u8, encoder: usize) -> Option<u32> {
    unsafe { controls::parameter_ring_position(state, encoder) }
}

pub unsafe fn preview_parameter_ring_position(
    state: *mut u8,
    encoder: usize,
    delta: i32,
) -> Option<u32> {
    unsafe { controls::preview_parameter_ring_position(state, encoder, delta) }
}

pub unsafe fn edit_parameter(state: *mut u8, encoder: usize, delta: i32) -> Option<u32> {
    unsafe { controls::edit_parameter(state, encoder, delta) }
}

pub unsafe fn show_editor_state(enabled: bool) {
    unsafe { controls::show_editor_state(enabled) }
}
