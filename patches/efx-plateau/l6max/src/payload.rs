#[path = "math.rs"]
mod math;
#[path = "plateau.rs"]
mod plateau;

pub const EFFECT_STATE_BYTES: usize = core::mem::size_of::<plateau::PlateauReverb>();

/// Initialize Plateau's state in the stock shared effect workspace.
pub unsafe fn initialize_effect(state: *mut u8) {
    let processor = unsafe { &mut *state.cast::<plateau::PlateauReverb>() };
    unsafe { core::ptr::write_bytes(processor, 0, 1) };
    processor.init(plateau::PlateauParameters::default());
}

/// Process one mono stock send and return the wet stereo effect output.
pub unsafe fn process_sample(state: *mut u8, input: f32) -> (f32, f32) {
    unsafe { &mut *state.cast::<plateau::PlateauReverb>() }.process_sample(input, input)
}
