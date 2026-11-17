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
