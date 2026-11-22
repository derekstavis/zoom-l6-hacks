// L6max MAIN 1.10 recorder callback bindings.

firmware_functions! {
    /// Queued Recorder callbacks receive the recorder control context. The
    /// current firmware callbacks may ignore it, but forwarding it preserves
    /// the callback ABI for entry hooks.
    pub unsafe fn rec_play_start_callback(context: *mut u32) -> u32, at 0x8004_6d80;
    pub unsafe fn rec_play_stop_callback(context: *mut u32) -> u32, at 0x8004_6e09;
    pub unsafe fn rec_start_callback(context: *mut u32) -> u32, at 0x8004_7268;
    pub unsafe fn rec_stop_callback(context: *mut u32) -> u32, at 0x8004_7338;

}

firmware_variables! {
    /// Recorder state byte read by the stock Rec/Play callbacks.
    pub rec_mode: u8, at 0x8020_a9ec;
}

firmware_symbols! {
    /// Recorder action the Play callback reaches.
    pub rec_play_action at 0x8004_6d68;
    /// Reads recorder stream data.
    pub rec_stream_read at 0x8004_a130;
}

firmware_symbols! {
    /// The RecPlayCtl task: receives queued commands and runs their callbacks.
    pub rec_play_ctl_task at 0x8004_69d8;
    /// Sends a Play command to RecPlayCtl once the Play gates pass.
    pub rec_play_request_send at 0x8004_6cb0;
    /// The Record action. Refuses with a notification in SD card reader mode,
    /// without a valid card, when the card is full, at project 99, or for a
    /// protected project.
    pub rec_record_action at 0x8004_6fb8;
    /// Opens a recorder stream file through StreamFileAccess.
    pub rec_stream_file_open_request at 0x8004_a250;
    /// Scans the project's audio track files.
    pub rec_scan_audio_track_files at 0x8000_7e08;
    /// Updates the playback state the recorder screen shows.
    pub rec_playback_state_update at 0x8000_f7a0;
    /// Updates the stream playback window.
    pub rec_stream_play_window_update at 0x8000_f7e0;
    /// The recorder screen's periodic refresh.
    pub rec_gui_refresh_tick at 0x8004_da20;
}

firmware_variables! {
    /// RecPlayCtl's command queue.
    pub rec_play_ctl_queue: *mut u8, at 0x8020_a9d8;
    /// Semaphore guarding `rec_mode` and `rec_command_running`.
    pub rec_mode_lock: *mut u8, at 0x8020_a9e0;
    /// 1 while RecPlayCtl runs a queued command's callback; Play waits for it.
    pub rec_command_running: u32, at 0x8020_a9f0;
    /// StreamFileAccess's request queue, then its two semaphores.
    pub stream_file_request_queue: *mut u8, at 0x8020_a928;
    pub stream_file_semaphores: [*mut u8; 2], at 0x8020_a92c;
    /// StreamPreLoad's task handle, then its queue.
    pub stream_preload_task: *mut u8, at 0x8020_a934;
    pub stream_preload_queue: *mut u8, at 0x8020_a938;
    /// The stream end position StreamPreLoad computes.
    pub stream_end_position: u64, at 0x805c_7c28;
    /// UTF-16 path of the active recording-date directory.
    pub recording_date_directory: [u16; 256], at 0x805c_7a00;
    /// 1 while SD card reader mode is on; Record and Play refuse then.
    pub sd_card_reader_mode: u32, at 0x8046_2d74;
    /// The current project number; Record refuses at 99.
    pub current_project: u32, at 0x8046_0b9c;
    /// The number a new project takes, capped at 99.
    pub next_project: u32, at 0x8046_0ba0;
    /// 1 per pad while its assignment worker is queued.
    pub sound_pad_assignment_pending: [u8; 4], at 0x8046_3001;
    /// The sound pad being recorded, or -1. Play does nothing while set.
    pub recording_pad: i8, at 0x8046_3005;
}
