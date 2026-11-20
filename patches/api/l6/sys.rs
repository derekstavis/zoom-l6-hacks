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
}
