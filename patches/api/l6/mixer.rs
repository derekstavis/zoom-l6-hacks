// L6 MAIN 1.10 mixer bindings.

firmware_functions! {
    pub unsafe fn mixer_get_eq_frequency(channel: u32, band: u32) -> u32, at 0x8000_5f90;
    pub unsafe fn mixer_get_eq_gain(channel: u32, band: u32) -> u32, at 0x8000_5fa8;
    pub unsafe fn mixer_get_aux(channel: u32, bus: u32) -> u32, at 0x8000_60e8;
    pub unsafe fn mixer_get_efx(channel: u32, bus: u32) -> u32, at 0x8000_5f60;
    pub unsafe fn mixer_get_pan(channel: u32) -> u32, at 0x8000_6028;
    pub unsafe fn mixer_get_level(channel: u32) -> u32, at 0x8000_5f40;
    pub unsafe fn mixer_channel_index(channel: u32) -> u32, at 0x8004_0ae8;
}

firmware_symbols! {
    /// Mixer EQ setter; DIN class-2 groups 0, 2 and 3 reach it.
    pub mixer_set_eq at 0x8003_e408;
    /// Mixer setter DIN class-2 group 1 reaches.
    pub mixer_din_setter_1 at 0x8003_e340;
    /// Mixer setter DIN class-2 groups 4 and 5 reach.
    pub mixer_din_setter_4 at 0x8003_d860;
    /// Mixer setter DIN class-2 group 6 reaches.
    pub mixer_din_setter_6 at 0x8003_e0c8;
    /// Mixer setter DIN class-2 group 7 reaches.
    pub mixer_din_setter_7 at 0x8003_db10;
    /// Mixer setter DIN class-2 group 8 reaches.
    pub mixer_din_setter_8 at 0x8003_da58;
    /// Another mixer setter the DIN dispatch monitor counts.
    pub mixer_setter at 0x8003_d790;
}

firmware_variables! {
    /// Mixer state, 0x7e bytes.
    pub mixer_state: u8, at 0x8044_0dca;
    /// Further mixer state, 6 bytes.
    pub mixer_state_tail: u8, at 0x8044_0e9a;
}
