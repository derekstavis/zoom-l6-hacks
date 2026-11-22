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
    /// The Record lamp as level-meter record 0x15; 16 while recording.
    pub rec_record_lamp: u16, at 0x8062_93c6;
}

firmware_symbols! {
    /// Sound-pad assignment worker that `l6_schedule_sound_pad_assignment`
    /// queues.
    pub sound_pad_assignment_worker at 0x8004_9d60;
}

firmware_variables! {
    /// 1 while SD card reader mode is on.
    pub sd_card_reader_mode: u32, at 0x8044_3108;
    /// RecPlayCtl word read under the same semaphore as `rec_mode`.
    pub rec_play_ctl_word: u32, at 0x8057_8ec0;
    /// Signed recorder state byte the play-button path reads.
    pub rec_state: i8, at 0x801f_5f00;
    /// The Play/Stop lamp as level-meter record 0x16.
    pub rec_playstop_lamp: u16, at 0x8062_93c8;
    /// The four sound-pad lamps, level-meter records 0x2c-0x2f.
    pub sound_pad_lamps: [u16; 4], at 0x8062_93f4;
}
