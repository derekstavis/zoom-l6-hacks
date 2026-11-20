// L6 MAIN 1.10 midi bindings.

firmware_functions! {
    pub unsafe fn midi_in(message: *const u8) at 0x8002_f491;
    pub unsafe fn midi_channel() -> u32, at 0x8000_5ed8;
    pub unsafe fn midi_send_mapped_cc(channel: u32, group: u32, value: u32) at 0x8000_8330;
}

firmware_symbols! {
    /// Stock MIDI output sender.
    pub midi_send at 0x8003_1648;
}

firmware_variables! {
    /// Configured MIDI channel, 0-15.
    pub midi_configured_channel: u8, at 0x8044_10f0;
    /// User CC map, 54 entries; 0xff is unmapped.
    pub midi_cc_map: u8, at 0x8044_10f5;
    /// CC dispatch map, 128 entries; 0xff is unmapped.
    pub midi_cc_dispatch_map: u8, at 0x8062_9bb8;
    /// DIN MIDI receive ring.
    pub midi_din_ring: u8, at 0x8062_e5a8;
    /// DIN ring state: write index at +4, read index at +12.
    pub midi_din_ring_state: u32, at 0x8062_eda8;
}
