// L6 MAIN 1.10 rec bindings.

firmware_symbols! {
    /// Handles the Play event.
    pub rec_play_handler at 0x8000_8a08;
    /// Recorder playback callback.
    pub rec_playback_callback at 0x8004_b890;
}

firmware_variables! {
    pub rec_mode: u32, at 0x8057_8ebc;
    /// Eight-byte key of the selected recording.
    pub rec_selected_key: u8, at 0x8061_01a8;
}
