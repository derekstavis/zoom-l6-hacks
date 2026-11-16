#[path = "math.rs"]
mod math;
#[path = "velvet.rs"]
mod velvet;

pub const EFFECT_STATE_BYTES: usize = core::mem::size_of::<velvet::VelvetDecorrelator>();

/// Initialize Velvet's state in the stock shared effect workspace.
pub unsafe fn initialize_effect(state: *mut u8) {
    let processor = unsafe { &mut *state.cast::<velvet::VelvetDecorrelator>() };
    unsafe { core::ptr::write_bytes(processor, 0, 1) };
    processor.init(velvet::Parameters::default());
}

/// Process one mono stock send and return the wet stereo effect output.
pub unsafe fn process_sample(state: *mut u8, input: f32) -> (f32, f32) {
    unsafe { &mut *state.cast::<velvet::VelvetDecorrelator>() }.process_sample(input, input)
}
