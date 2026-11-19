#![cfg_attr(not(sd_composed), no_std)]
#![cfg_attr(not(sd_composed), no_main)]

#[cfg(not(sd_composed))]
#[path = "../../../api/patchrt.rs"]
#[macro_use]
#[cfg(not(sd_composed))]
pub mod patch_api;
#[cfg(sd_composed)]
use crate::patch_api;
#[cfg(sd_composed)]
use crate::effect_replacement_config;
use patch_api::function_entry_hook;

#[cfg(not(sd_composed))]
mod firmware;
#[cfg(not(sd_composed))]
use firmware as firmware_bindings;
#[cfg(sd_composed)]
use crate::firmware_bindings;
use firmware_bindings::{
    current_window,
    dsp_effect_workspace, dsp_effect_workspace_clear, dsp_stream_callback, mixer_apply_effect_type,
    mixer_apply_channel_parameter, mixer_apply_effect_type_from_mixer, mixer_get_efx_type,
    mixer_set_efx_type, l6max_encoder_ring_pattern_apply, l6max_panel_led_value_store,
    l6max_panel_select_parameter_mode, l6max_tap_press_handler, DSP_EFFECT_WORKSPACE_BYTES,
};

#[path = "../../../api/l6max/hooks.rs"]
#[cfg(not(sd_composed))]
mod hook_catalog;
#[path = "math.rs"]
mod math;

use core::sync::atomic::{AtomicU32, Ordering};

const EFFECT_STATE_OFFSET: usize = 0x100;
const EFFECT_STATE: usize = dsp_effect_workspace.address() + EFFECT_STATE_OFFSET;
const CALLBACK_CLOSED: u32 = 1 << 31;
const ROOM_CALLBACK: u32 = 0x2028_0ab9;
const SPRING_CALLBACK: u32 = 0x2028_2221;
const ACTIVE_CALLBACK: *mut u32 = 0x202a_6f20 as *mut u32;
const BLOCK_SIZE: usize = 64;
const MAX_CHANNELS: usize = 12;

fn callback_address() -> u32 {
    dual_effect_callback as *const () as usize as u32 | 1
}

const _: () = assert!(
    EFFECT_STATE_OFFSET + effect_replacement_config::MAX_EFFECT_STATE_BYTES
        <= DSP_EFFECT_WORKSPACE_BYTES
);

#[unsafe(no_mangle)]
pub static DSP_STREAM_ENTRY_HITS: AtomicU32 = AtomicU32::new(0);
#[unsafe(no_mangle)]
pub static EFFECT_WORKSPACE_CLEAR_HITS: AtomicU32 = AtomicU32::new(0);
#[unsafe(no_mangle)]
pub static DUAL_EFFECT_TYPE_SET_HITS: AtomicU32 = AtomicU32::new(0);
#[unsafe(no_mangle)]
pub static DUAL_EFFECT_TYPE_SET_SEEN: AtomicU32 = AtomicU32::new(0);
#[unsafe(no_mangle)]
pub static DUAL_EFFECT_CALLBACK_HITS: AtomicU32 = AtomicU32::new(0);
#[unsafe(no_mangle)]
pub static ROOM_CALLBACK_HITS: AtomicU32 = AtomicU32::new(0);
#[unsafe(no_mangle)]
pub static ROOM_CALLBACK_ADMITTED: AtomicU32 = AtomicU32::new(0);
#[unsafe(no_mangle)]
pub static SPRING_CALLBACK_HITS: AtomicU32 = AtomicU32::new(0);
#[unsafe(no_mangle)]
pub static SPRING_CALLBACK_ADMITTED: AtomicU32 = AtomicU32::new(0);
#[unsafe(no_mangle)]
pub static DUAL_EFFECT_CALLBACK_ORIGINAL: AtomicU32 = AtomicU32::new(0);
#[unsafe(no_mangle)]
pub static DUAL_EFFECT_CALLBACK_CONTEXT: AtomicU32 = AtomicU32::new(0);
#[unsafe(no_mangle)]
pub static DUAL_EFFECT_CALLBACK_DESCRIPTOR: AtomicU32 = AtomicU32::new(0);
#[unsafe(no_mangle)]
pub static DUAL_EFFECT_CALLBACK_GATE: AtomicU32 = AtomicU32::new(CALLBACK_CLOSED);
#[unsafe(no_mangle)]
pub static DUAL_EFFECT_SELECTION_PENDING: AtomicU32 = AtomicU32::new(1);
#[unsafe(no_mangle)]
pub static DUAL_EFFECT_PROCESSOR_TYPE: AtomicU32 = AtomicU32::new(0);
/// The effect ID running in the selected slot, as a pointer and byte length,
/// for the emulator's checks. Zero when no replacement runs.
#[unsafe(no_mangle)]
pub static DUAL_EFFECT_ACTIVE_EFFECT: AtomicU32 = AtomicU32::new(0);
#[unsafe(no_mangle)]
pub static DUAL_EFFECT_ACTIVE_EFFECT_LEN: AtomicU32 = AtomicU32::new(0);
#[unsafe(no_mangle)]
pub static DUAL_EFFECT_OUTPUT_NONZERO: AtomicU32 = AtomicU32::new(0);
#[unsafe(no_mangle)]
pub static DUAL_EFFECT_OUTPUT_INVALID: AtomicU32 = AtomicU32::new(0);
#[unsafe(no_mangle)]
pub static DUAL_EFFECT_ROOM_OUTPUT_NONZERO: AtomicU32 = AtomicU32::new(0);
#[unsafe(no_mangle)]
pub static DUAL_EFFECT_SPRING_OUTPUT_NONZERO: AtomicU32 = AtomicU32::new(0);
#[unsafe(no_mangle)]
pub static DUAL_EFFECT_ROUTED_CALLBACK: AtomicU32 = AtomicU32::new(0);
#[unsafe(no_mangle)]
pub static DUAL_EFFECT_INPUT_NONZERO: AtomicU32 = AtomicU32::new(0);
#[unsafe(no_mangle)]
pub static DUAL_EFFECT_SEND_NONZERO: AtomicU32 = AtomicU32::new(0);
#[unsafe(no_mangle)]
pub static DUAL_EFFECT_EDIT_EVENT: AtomicU32 = AtomicU32::new(0);
#[unsafe(no_mangle)]
pub static DUAL_EFFECT_EDIT_APPLIED: AtomicU32 = AtomicU32::new(0);
#[unsafe(no_mangle)]
pub static DUAL_EFFECT_EDIT_DROPPED: AtomicU32 = AtomicU32::new(0);
#[unsafe(no_mangle)]
pub static DUAL_EFFECT_PARAMETER_EDIT_MODE: AtomicU32 = AtomicU32::new(0);
#[unsafe(no_mangle)]
pub static DUAL_EFFECT_PANEL_MODE: AtomicU32 = AtomicU32::new(u32::MAX);
#[unsafe(no_mangle)]
pub static DUAL_EFFECT_EXIT_RING_REFRESH_HITS: AtomicU32 = AtomicU32::new(0);
#[unsafe(no_mangle)]
pub static PARAMETER_HANDOFF: [AtomicU32; 16] = [const { AtomicU32::new(u32::MAX) }; 16];
#[unsafe(no_mangle)]
pub static PARAMETER_RING_POSITIONS: [AtomicU32; 8] = [const { AtomicU32::new(0) }; 8];
static STOCK_RING_PATTERNS: [AtomicU32; 8] = [const { AtomicU32::new(0) }; 8];
static STOCK_RING_VALUES: [AtomicU32; 8] = [const { AtomicU32::new(0) }; 8];
static STOCK_RING_VALID: AtomicU32 = AtomicU32::new(0);
static DRAWING_PARAMETER_RINGS: AtomicU32 = AtomicU32::new(0);
static PREVIOUS_PARAMETER_MODE: AtomicU32 = AtomicU32::new(u32::MAX);

const EVENT_PENDING: u32 = 1;
const EVENT_TAP_TEMPO: u32 = 1 << 31;
const RECORDER_WINDOW: u32 = 0x8020_0408;
const EFX_PARAMETER_MODE: u32 = 6;
const PARAMETER_RING_PATTERN: u32 = 0;

fn replacement_for(slot: u32) -> Option<&'static str> {
    effect_replacement_config::effect_for_stock_slot(slot)
}

fn ring_pattern_row(position_q16: u32) -> u32 {
    (position_q16.saturating_add(0x8000) >> 16).min(44)
}

fn refresh_parameter_positions(slot: u32) {
    let count = effect_replacement_config::parameter_count(slot);
    for encoder in 0..8 {
        let position = if encoder < count {
            unsafe {
                effect_replacement_config::parameter_position(
                    slot,
                    EFFECT_STATE as *mut u8,
                    encoder,
                )
            }
            .unwrap_or(0)
        } else {
            0
        };
        PARAMETER_RING_POSITIONS[encoder].store(position, Ordering::Release);
    }
}

unsafe fn apply_parameter_ring(ring: u32, position: u32) {
    DRAWING_PARAMETER_RINGS.store(1, Ordering::Release);
    unsafe { l6max_encoder_ring_pattern_apply(ring, PARAMETER_RING_PATTERN, position) };
    DRAWING_PARAMETER_RINGS.store(0, Ordering::Release);
}

unsafe fn draw_parameter_rings(slot: u32) {
    let count = effect_replacement_config::parameter_count(slot);
    for ring in 0..8 {
        let position = if ring < count {
            ring_pattern_row(PARAMETER_RING_POSITIONS[ring].load(Ordering::Acquire))
        } else {
            0
        };
        unsafe { apply_parameter_ring(ring as u32, position) };
    }
}

unsafe fn restore_stock_parameter_rings() {
    let valid = STOCK_RING_VALID.load(Ordering::Acquire);
    for ring in 0..8 {
        if valid & (1 << ring) == 0 {
            continue;
        }
        unsafe {
            l6max_encoder_ring_pattern_apply(
                ring as u32,
                STOCK_RING_PATTERNS[ring].load(Ordering::Relaxed),
                STOCK_RING_VALUES[ring].load(Ordering::Relaxed),
            )
        };
    }
}

unsafe fn set_tap_edit_indicator(enabled: bool) {
    const TAP_LED_SLOT: usize = 33;
    unsafe { l6max_panel_led_value_store(TAP_LED_SLOT as u32, 0, if enabled { 0x10 } else { 0 }) };
}

unsafe fn set_parameter_edit_mode(slot: u32, enabled: bool) {
    DUAL_EFFECT_PARAMETER_EDIT_MODE.store(u32::from(enabled), Ordering::Release);
    unsafe { effect_replacement_config::show_editor_state(slot, enabled) };
    if enabled {
        unsafe { set_tap_edit_indicator(true) };
        unsafe { draw_parameter_rings(slot) };
    } else {
        unsafe { mixer_apply_effect_type_from_mixer(1) };
        unsafe { restore_stock_parameter_rings() };
        unsafe { set_tap_edit_indicator(false) };
    }
}

unsafe fn deactivate_editor_for_effect_change() {
    if DUAL_EFFECT_PARAMETER_EDIT_MODE.swap(0, Ordering::AcqRel) == 0 {
        return;
    }
    PREVIOUS_PARAMETER_MODE.store(u32::MAX, Ordering::Release);
    DUAL_EFFECT_EDIT_EVENT.store(0, Ordering::Relaxed);
    unsafe {
        restore_stock_parameter_rings();
        set_tap_edit_indicator(false);
    }
}

fn close_and_drain() {
    DUAL_EFFECT_CALLBACK_GATE.fetch_or(CALLBACK_CLOSED, Ordering::AcqRel);
    while DUAL_EFFECT_CALLBACK_GATE.load(Ordering::Acquire) != CALLBACK_CLOSED {
        core::hint::spin_loop();
    }
}

fn initialize_effect(slot: u32) -> bool {
    if replacement_for(slot).is_none() {
        return false;
    }
    let Some(size) = effect_replacement_config::state_size(slot) else {
        return false;
    };
    if size + EFFECT_STATE_OFFSET > DSP_EFFECT_WORKSPACE_BYTES {
        return false;
    }
    unsafe { effect_replacement_config::initialize(slot, EFFECT_STATE as *mut u8) }
}

fn finish_effect_selection(slot: u32) {
    if initialize_effect(slot) {
        let id = replacement_for(slot).unwrap_or("");
        DUAL_EFFECT_ACTIVE_EFFECT.store(id.as_ptr() as u32, Ordering::Relaxed);
        DUAL_EFFECT_ACTIVE_EFFECT_LEN.store(id.len() as u32, Ordering::Relaxed);
        DUAL_EFFECT_PARAMETER_EDIT_MODE.store(0, Ordering::Release);
        refresh_parameter_positions(slot);
        DUAL_EFFECT_PROCESSOR_TYPE.store(slot, Ordering::Release);
        DUAL_EFFECT_SELECTION_PENDING.store(0, Ordering::Release);
        DUAL_EFFECT_CALLBACK_GATE.store(0, Ordering::Release);
    } else {
        DUAL_EFFECT_ACTIVE_EFFECT.store(0, Ordering::Relaxed);
        DUAL_EFFECT_ACTIVE_EFFECT_LEN.store(0, Ordering::Relaxed);
        DUAL_EFFECT_PROCESSOR_TYPE.store(0, Ordering::Release);
        DUAL_EFFECT_ROUTED_CALLBACK.store(0, Ordering::Relaxed);
        DUAL_EFFECT_EDIT_EVENT.store(0, Ordering::Relaxed);
        DUAL_EFFECT_PARAMETER_EDIT_MODE.store(0, Ordering::Release);
        DUAL_EFFECT_SELECTION_PENDING.store(0, Ordering::Release);
        DUAL_EFFECT_CALLBACK_GATE.store(CALLBACK_CLOSED, Ordering::Release);
    }
}

fn callback_enter() -> bool {
    DUAL_EFFECT_CALLBACK_GATE
        .compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire)
        .is_ok()
}

fn callback_leave() {
    DUAL_EFFECT_CALLBACK_GATE.fetch_sub(1, Ordering::Release);
}

unsafe fn clear_callback_output(descriptor: *mut u8) {
    if descriptor.is_null() {
        return;
    }
    let buffer = unsafe { core::ptr::read_volatile(descriptor.add(4).cast::<u32>()) } as *mut f32;
    let control_link = unsafe { core::ptr::read_volatile(descriptor.add(28).cast::<u32>()) };
    let control_target = if control_link == 0 {
        core::ptr::null_mut()
    } else {
        unsafe { core::ptr::read_volatile(control_link as *const u32) as *mut u32 }
    };
    let control_source =
        unsafe { core::ptr::read_volatile(descriptor.add(32).cast::<u32>()) as *const u32 };
    if !buffer.is_null() {
        for frame in 0..BLOCK_SIZE {
            unsafe {
                core::ptr::write_volatile(buffer.add(frame), 0.0);
                core::ptr::write_volatile(buffer.add(frame + BLOCK_SIZE), 0.0);
            }
        }
    }
    if !control_target.is_null() && !control_source.is_null() {
        for frame in 0..BLOCK_SIZE {
            unsafe {
                core::ptr::write_volatile(
                    control_target,
                    core::ptr::read_volatile(control_source.add(frame)),
                );
            }
        }
    }
}

#[function_entry_hook(dsp_effect_workspace_clear)]
pub unsafe fn effect_replacements_workspace_clear() {
    EFFECT_WORKSPACE_CLEAR_HITS.fetch_add(1, Ordering::Relaxed);
    close_and_drain();
    unsafe { deactivate_editor_for_effect_change() };
    DUAL_EFFECT_SELECTION_PENDING.store(1, Ordering::Release);
    DUAL_EFFECT_PROCESSOR_TYPE.store(0, Ordering::Release);
    DUAL_EFFECT_ACTIVE_EFFECT.store(0, Ordering::Relaxed);
    DUAL_EFFECT_ACTIVE_EFFECT_LEN.store(0, Ordering::Relaxed);
    let continuation = hook_continue();
    finish_effect_selection(unsafe { mixer_get_efx_type(0) });
    continuation
}

#[function_entry_hook(mixer_set_efx_type)]
pub unsafe fn effect_replacements_set_effect_type(bus: u32, effect_type: u32) {
    DUAL_EFFECT_TYPE_SET_HITS.fetch_add(1, Ordering::Relaxed);
    if effect_type < 32 {
        DUAL_EFFECT_TYPE_SET_SEEN.fetch_or(1 << effect_type, Ordering::Relaxed);
    }
    if bus != 0 {
        return hook_continue(bus, effect_type);
    }
    close_and_drain();
    if effect_type != DUAL_EFFECT_PROCESSOR_TYPE.load(Ordering::Acquire) {
        unsafe { deactivate_editor_for_effect_change() };
    }
    let continuation = hook_continue(bus, effect_type);
    finish_effect_selection(unsafe { mixer_get_efx_type(0) });
    continuation
}

#[function_entry_hook(mixer_apply_effect_type)]
pub unsafe fn effect_replacements_select_callback(effect_type: u32, force_refresh: u32) -> u32 {
    if effect_type != DUAL_EFFECT_PROCESSOR_TYPE.load(Ordering::Acquire) {
        unsafe { deactivate_editor_for_effect_change() };
    }
    let continuation = hook_continue(effect_type, force_refresh);
    finish_effect_selection(effect_type);
    continuation
}

#[function_entry_hook(mixer_apply_channel_parameter)]
pub unsafe fn effect_replacements_encoder_event(
    channel: u32,
    mode: u32,
    absolute: u32,
    delta: i32,
    value: u32,
) {
    let slot = unsafe { mixer_get_efx_type(0) };
    let count = effect_replacement_config::parameter_count(slot);
    if unsafe { current_window.read() } != RECORDER_WINDOW
        || DUAL_EFFECT_PARAMETER_EDIT_MODE.load(Ordering::Acquire) == 0
        || DUAL_EFFECT_PROCESSOR_TYPE.load(Ordering::Acquire) != slot
        || mode != 6
    {
        return hook_continue(channel, mode, absolute, delta, value);
    }
    // The editor owns encoder events while active. Swallow turns for parameters
    // this effect does not expose, so they cannot fall through to stock EFX level.
    if channel as usize >= count || delta == 0 {
        return hook_done();
    }
    let step = delta.clamp(i16::MIN as i32, i16::MAX as i32) as i16;
    let encoded = EVENT_PENDING
        | ((channel & 0x7) << 1)
        | ((step as u16 as u32) << 8);
    let preview_gate = DUAL_EFFECT_CALLBACK_GATE
        .compare_exchange(0, CALLBACK_CLOSED, Ordering::AcqRel, Ordering::Acquire)
        .is_ok();
    if DUAL_EFFECT_EDIT_EVENT
        .compare_exchange(0, encoded, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        DUAL_EFFECT_EDIT_DROPPED.fetch_add(1, Ordering::Relaxed);
    } else if preview_gate {
        let ring = (channel & 0x7) as usize;
        if let Some(position) = unsafe {
            effect_replacement_config::preview_parameter_position(
                slot,
                EFFECT_STATE as *mut u8,
                ring,
                step as i32,
            )
        } {
            PARAMETER_RING_POSITIONS[ring].store(position, Ordering::Release);
            unsafe { apply_parameter_ring(ring as u32, ring_pattern_row(position)) };
        }
    }
    if preview_gate {
        DUAL_EFFECT_CALLBACK_GATE.store(0, Ordering::Release);
    }
    hook_done()
}

#[function_entry_hook(l6max_tap_press_handler)]
pub unsafe fn effect_replacements_tap_event() {
    if unsafe { current_window.read() } != RECORDER_WINDOW {
        return hook_continue();
    }
    let slot = unsafe { mixer_get_efx_type(0) };
    match effect_replacement_config::tap_action(slot) {
        1 if replacement_for(slot).is_some()
            && DUAL_EFFECT_PROCESSOR_TYPE.load(Ordering::Acquire) == slot =>
        {
            let enabled = DUAL_EFFECT_PARAMETER_EDIT_MODE.fetch_xor(1, Ordering::AcqRel) == 0;
            if enabled {
                let previous = DUAL_EFFECT_PANEL_MODE.load(Ordering::Acquire);
                PREVIOUS_PARAMETER_MODE.store(previous, Ordering::Release);
                if previous != EFX_PARAMETER_MODE {
                    unsafe { l6max_panel_select_parameter_mode(EFX_PARAMETER_MODE, 1) };
                    DUAL_EFFECT_PANEL_MODE.store(EFX_PARAMETER_MODE, Ordering::Release);
                }
                unsafe { set_parameter_edit_mode(slot, true) };
            } else {
                unsafe { set_parameter_edit_mode(slot, false) };
                let previous = PREVIOUS_PARAMETER_MODE.swap(u32::MAX, Ordering::AcqRel);
                if previous != u32::MAX && previous != EFX_PARAMETER_MODE {
                    unsafe { l6max_panel_select_parameter_mode(previous, 1) };
                    DUAL_EFFECT_PANEL_MODE.store(previous, Ordering::Release);
                }
            }
            hook_done()
        }
        2 if replacement_for(slot).is_some() => {
            if DUAL_EFFECT_EDIT_EVENT
                .compare_exchange(0, EVENT_PENDING | EVENT_TAP_TEMPO, Ordering::AcqRel, Ordering::Acquire)
                .is_err()
            {
                DUAL_EFFECT_EDIT_DROPPED.fetch_add(1, Ordering::Relaxed);
            }
            hook_done()
        }
        _ => hook_continue(),
    }
}

#[function_entry_hook(l6max_panel_select_parameter_mode)]
pub unsafe fn effect_replacements_panel_mode(mode: u32, event: u32) {
    if event == 1
        && mode != EFX_PARAMETER_MODE
        && DUAL_EFFECT_PARAMETER_EDIT_MODE.swap(0, Ordering::AcqRel) != 0
    {
        PREVIOUS_PARAMETER_MODE.store(u32::MAX, Ordering::Release);
        unsafe { set_parameter_edit_mode(unsafe { mixer_get_efx_type(0) }, false) };
    }
    let continuation = hook_continue(mode, event);
    if mode < 10 {
        DUAL_EFFECT_PANEL_MODE.store(mode, Ordering::Release);
    }
    continuation
}

#[function_entry_hook(l6max_encoder_ring_pattern_apply)]
pub unsafe fn effect_replacements_ring_refresh(ring: u32, pattern: u32, value: u32) {
    if ring < 8 && DRAWING_PARAMETER_RINGS.load(Ordering::Acquire) == 0 {
        STOCK_RING_PATTERNS[ring as usize].store(pattern, Ordering::Relaxed);
        STOCK_RING_VALUES[ring as usize].store(value, Ordering::Relaxed);
        STOCK_RING_VALID.fetch_or(1 << ring, Ordering::Release);
    }
    if ring < effect_replacement_config::parameter_count(unsafe { mixer_get_efx_type(0) }) as u32
        && DUAL_EFFECT_PARAMETER_EDIT_MODE.load(Ordering::Acquire) != 0
    {
        let position = ring_pattern_row(PARAMETER_RING_POSITIONS[ring as usize].load(Ordering::Acquire));
        return hook_continue(ring, PARAMETER_RING_PATTERN, position);
    }
    hook_continue(ring, pattern, value)
}

/// Install the selected effect callback only after the stock stream wrapper
/// has refreshed the active callback pointer.
#[function_entry_hook(dsp_stream_callback)]
pub unsafe fn effect_replacements_stream_entry() {
    DSP_STREAM_ENTRY_HITS.fetch_add(1, Ordering::Relaxed);
    let continuation = hook_continue();
    let slot = unsafe { mixer_get_efx_type(0) };
    if replacement_for(slot).is_some() {
        let stock = unsafe { core::ptr::read_volatile(ACTIVE_CALLBACK) };
        if stock != callback_address() {
            DUAL_EFFECT_CALLBACK_ORIGINAL.store(stock, Ordering::Release);
        }
        if DUAL_EFFECT_SELECTION_PENDING.load(Ordering::Acquire) == 0
            && DUAL_EFFECT_PROCESSOR_TYPE.load(Ordering::Acquire) == slot
        {
            unsafe { core::ptr::write_volatile(ACTIVE_CALLBACK, callback_address()) };
        }
    }
    continuation
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn dual_effect_callback(context: *mut u8, descriptor: *mut u8) {
    DUAL_EFFECT_CALLBACK_HITS.fetch_add(1, Ordering::Relaxed);
    DUAL_EFFECT_CALLBACK_CONTEXT.store(context as u32, Ordering::Relaxed);
    DUAL_EFFECT_CALLBACK_DESCRIPTOR.store(descriptor as u32, Ordering::Relaxed);
    let original = DUAL_EFFECT_CALLBACK_ORIGINAL.load(Ordering::Acquire);
    let slot = if original == ROOM_CALLBACK {
        ROOM_CALLBACK_HITS.fetch_add(1, Ordering::Relaxed);
        ROOM_CALLBACK_ADMITTED.fetch_add(1, Ordering::Relaxed);
        1
    } else if original == SPRING_CALLBACK {
        SPRING_CALLBACK_HITS.fetch_add(1, Ordering::Relaxed);
        SPRING_CALLBACK_ADMITTED.fetch_add(1, Ordering::Relaxed);
        2
    } else {
        unsafe { mixer_get_efx_type(0) }
    };
    if !callback_enter() || replacement_for(slot).is_none() {
        unsafe { clear_callback_output(descriptor) };
        return;
    }
    let processed = unsafe { process_effect_block(slot, descriptor) };
    callback_leave();
    if !processed {
        unsafe { clear_callback_output(descriptor) };
    }
}

unsafe fn process_effect_block(slot: u32, descriptor: *mut u8) -> bool {
    if descriptor.is_null() {
        return false;
    }
    let buffer = unsafe { core::ptr::read_volatile(descriptor.add(4).cast::<u32>()) } as *mut f32;
    let control_link = unsafe { core::ptr::read_volatile(descriptor.add(28).cast::<u32>()) };
    let control_target = if control_link == 0 {
        core::ptr::null_mut()
    } else {
        unsafe { core::ptr::read_volatile(control_link as *const u32) as *mut u32 }
    };
    let control_source =
        unsafe { core::ptr::read_volatile(descriptor.add(32).cast::<u32>()) as *const u32 };
    if buffer.is_null() || control_target.is_null() || control_source.is_null() {
        return false;
    }
    process_control_event(slot);
    for frame in 0..BLOCK_SIZE {
        let mut input = 0.0f32;
        for channel in 0..MAX_CHANNELS {
            let ramp_descriptor = (0x2001_61d0usize + 16 * channel) as *const u32;
            let ramped = unsafe { core::ptr::read_volatile(ramp_descriptor) } != 0;
            let gain = if ramped {
                let ramp_base = unsafe { core::ptr::read_volatile(ramp_descriptor.add(1)) };
                if ramp_base < 256 {
                    0.0
                } else {
                    let ramp = (ramp_base - 256) as *const f32;
                    unsafe { core::ptr::read_volatile(ramp.add(frame)) }
                }
            } else {
                unsafe {
                    core::ptr::read_volatile((0x2001_6170usize + 8 * channel) as *const f32)
                }
            };
            let smoothed = unsafe {
                core::ptr::read_volatile((0x2001_6140usize + 4 * channel) as *const f32)
            };
            let sample = unsafe {
                core::ptr::read_volatile((0x2000_e410usize + 256 * channel) as *const f32)
            } * (1.0 / 2_147_483_648.0);
            let contribution = math::audio_input((sample * gain) * smoothed);
            input += contribution;
            if contribution != 0.0 {
                DUAL_EFFECT_INPUT_NONZERO.fetch_add(1, Ordering::Relaxed);
            }
        }
        input = math::audio_input(input);
        if input != 0.0 {
            DUAL_EFFECT_SEND_NONZERO.fetch_add(1, Ordering::Relaxed);
        }
        unsafe {
            core::ptr::write_volatile(
                control_target,
                core::ptr::read_volatile(control_source.add(frame)),
            );
        }
        let Some((left, right)) = (unsafe {
            effect_replacement_config::process_sample(slot, EFFECT_STATE as *mut u8, input)
        }) else {
            return false;
        };
        let (left, right) = (math::soft_clip(left), math::soft_clip(right));
        if left.is_finite() && right.is_finite() {
            if left != 0.0 || right != 0.0 {
                DUAL_EFFECT_OUTPUT_NONZERO.fetch_add(1, Ordering::Relaxed);
                if slot == 1 {
                    DUAL_EFFECT_ROOM_OUTPUT_NONZERO.fetch_add(1, Ordering::Relaxed);
                } else if slot == 2 {
                    DUAL_EFFECT_SPRING_OUTPUT_NONZERO.fetch_add(1, Ordering::Relaxed);
                }
            }
            unsafe {
                core::ptr::write_volatile(buffer.add(frame), left);
                core::ptr::write_volatile(buffer.add(frame + BLOCK_SIZE), right);
            }
        } else {
            DUAL_EFFECT_OUTPUT_INVALID.fetch_add(1, Ordering::Relaxed);
            unsafe {
                core::ptr::write_volatile(buffer.add(frame), 0.0);
                core::ptr::write_volatile(buffer.add(frame + BLOCK_SIZE), 0.0);
            }
        }
    }
    true
}

fn process_control_event(slot: u32) {
    let event = DUAL_EFFECT_EDIT_EVENT.swap(0, Ordering::AcqRel);
    if event & EVENT_PENDING == 0 {
        return;
    }
    if event & EVENT_TAP_TEMPO != 0 {
        unsafe { effect_replacement_config::tap_tempo(slot, EFFECT_STATE as *mut u8) };
        return;
    }
    let encoder = ((event >> 1) & 0x7) as usize;
    let delta = ((event >> 8) as u16 as i16) as i32;
    if let Some(position) = unsafe {
        effect_replacement_config::edit_parameter(
            slot,
            EFFECT_STATE as *mut u8,
            encoder,
            delta,
        )
    } {
        PARAMETER_RING_POSITIONS[encoder].store(position, Ordering::Release);
        DUAL_EFFECT_EDIT_APPLIED.fetch_add(1, Ordering::Relaxed);
    }
}
