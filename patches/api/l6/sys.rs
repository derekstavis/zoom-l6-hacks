// L6 MAIN 1.10 startup bindings.

firmware_functions! {
    pub unsafe fn sys_l6_initialize_and_start_scheduler() at 0x8006_7a60;
}

firmware_symbols! {
    /// Submits an event to the MAIN task's queue.
    pub main_event_submit at 0x8002_0ca0;
    /// Reads the MAIN task's event queue.
    pub main_event_queue_reader at 0x8002_0690;
    /// Entry of the MAIN task.
    pub main_task at 0x8006_dd70;
    /// The MAIN task's call that waits for an event.
    pub main_wait_call at 0x8002_c216;
    /// Return address of `main_wait_call`.
    pub main_wait_return at 0x8002_c21a;
    /// Where the MAIN task resumes with its event state.
    pub main_state_return at 0x8002_c21e;
    /// Gives the semaphore the MAIN task waits on.
    pub main_wait_give at 0x8007_63d8;
    /// MAIN handler for class-2 (DIN control) events.
    pub main_class2_event_handler at 0x8002_c510;
    /// Dispatches system events, including Play.
    pub system_event_dispatcher at 0x8002_c9d8;
    /// The Idle task's loop.
    pub idle_loop at 0x8004_6d50;
}

firmware_variables! {
    /// Events waiting in the MAIN task's queue.
    pub main_event_queue_pending: u32, at 0x806b_2f20;
    /// Sound Pad 1's WAV path (UTF-16) in the active assignment record.
    pub sound_pad1_path: u16, at 0x8044_5524;
    /// Sound Pad 1's play mode from its SD assignment; 0 is one-shot.
    pub sound_pad1_play_mode: u8, at 0x8044_1d8c;
}

firmware_symbols! {
    /// Main's startup calls, in order. Main calls the first four and then
    /// `main_task_loop`, which calls the rest before looping.
    pub main_init_word_set at 0x8001_b2f8;
    pub gpt1_microsecond_timer_start at 0x8003_28f8;
    pub main_device_initializer at 0x8001_b0e8;
    pub main_runtime_buffers_clear at 0x8002_f8a0;
    pub main_task_loop at 0x8002_c1e8;
    pub sampler_file_assign_priority_raise at 0x8000_8bc0;
    pub recorder_media_refresh at 0x8000_9a40;
    pub recplayctl_state_reset at 0x8003_4de8;
    pub sound_pad_sampler_init at 0x8000_9848;
    pub uicycle_init at 0x8003_bc30;
    pub timer_11_init at 0x8004_02c0;
    /// Drains `main_event_queue` into the Main task's event buffer. The two
    /// variants share its queue, mutex and buffer.
    pub main_event_queue_drain at 0x8002_0360;
    pub main_event_queue_drain_variant at 0x8002_0438;
    pub main_event_queue_drain_with_args at 0x8002_0548;
    /// Restart software timers 22 and 4.
    pub timer_22_restart at 0x8003_44e0;
    pub timer_4_restart at 0x8003_4530;
}

firmware_variables! {
    /// The Main task's event queue: 4096 items of 20 bytes.
    pub main_event_queue: *mut u8, at 0x801f_8f28;
}
