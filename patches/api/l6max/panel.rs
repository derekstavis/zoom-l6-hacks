// L6max MAIN 1.10 panel and UI-event bindings.

firmware_functions! {
    /// Applies one of the stock nine-segment patterns to an encoder ring.
    /// `ring` is 0..8 exclusive, pattern 0 accepts values 0..44, and pattern 1 accepts 0..40.
    pub unsafe fn l6max_encoder_ring_pattern_apply(ring: u32, pattern: u32, value: u32) at 0x8000_c4f0;

    /// Stores a brightness code for one panel-matrix output slot.
    pub unsafe fn l6max_panel_led_value_store(slot: u32, mode: u32, brightness: u32) at 0x8003_3348;

    /// Route-0 main-button event handler. `event` points to the five-word UI
    /// event and `context` is the current window/controller context.
    pub unsafe fn panel_handle_main_button_event(event: *const u32, context: *mut u32) at 0x8003_6a40;

    /// Handles a physical TAP button press on L6max MAIN.
    pub unsafe fn l6max_tap_press_handler() at 0x8005_5468;

    /// Selects the recorder's encoder parameter page (`mode` 6 is EFX).
    /// `event` 1 applies the selection through the stock panel path.
    pub unsafe fn l6max_panel_select_parameter_mode(mode: u32, event: u32) at 0x8005_3bb0;

    /// Enqueue a five-word event through the stock UI queue. The fifth word is
    /// passed on the caller's stack per AAPCS.
    pub unsafe fn panel_ui_event_queue_send(source: u32, kind: u32, event: u32, reserved: u32, reserved2: u32) -> u32, at 0x8002_8088;
}

firmware_symbols! {
    /// End of `panel_handle_main_button_event`.
    pub panel_handle_main_button_event_end at 0x8003_728c;
    /// Submits the Play event to its queue.
    pub play_event_submit at 0x8004_5458;
    /// Where `play_event_submit` returns its queue-send result.
    pub play_event_send_return at 0x8002_80a0;
}

firmware_variables! {
    pub panel_key_state: u8, at 0x8088_3554;
}

/// Key-reader table mapping panel targets to keys.
pub const PANEL_KEY_TABLE: u32 = 0x800c_48c4;

firmware_variables! {
    /// The four sound-pad lamps, LED records 0x1b-0x1e: a value byte and a
    /// mode byte each, as SoundPadLed stores them through
    /// `l6max_panel_led_value_store`.
    pub sound_pad_lamps: [u16; 4], at 0x805d_b31e;
}
