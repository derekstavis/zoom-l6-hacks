// FreeRTOS bindings for L6 MAIN 1.10.

firmware_functions! {
    pub unsafe fn vTaskStartScheduler() at 0x8007_45b8;
    pub unsafe fn pvPortMalloc(size: usize) -> *mut u8, at 0x8006_de78;
    pub unsafe fn vPortFree(pointer: *mut u8) at 0x8007_3f48;
    pub unsafe fn xTaskIncrementTick() -> i32, at 0x8007_7140;
}

firmware_symbols! {
    /// Creates a task and its stack from the heap. Its stack-depth argument
    /// type is unverified, so it is not declared as a function.
    pub xTaskCreate at 0x8007_6c60;
    /// Moves the current task to a delayed or suspended list.
    pub prvAddCurrentTaskToDelayedList at 0x8006_d338;
    /// The heap_4 arena's first block header.
    pub heap_first_block at 0x808e_2fe0;
    /// The heap_4 end-marker block; the arena ends here.
    pub heap_end_block at 0x8095_ffd0;
}

firmware_variables! {
    pub mut pxCurrentTCB: *mut u8, at 0x808e_291c;
    /// One 20-byte list per priority, 32 priorities.
    pub mut pxReadyTasksLists: [[u32; 5]; 32], at 0x808e_2920;
    pub mut uxTopReadyPriority: u32, at 0x801f_9038;
    /// Increments as tasks are created and deleted.
    pub mut uxTaskNumber: u32, at 0x801f_903c;
    pub mut uxCurrentNumberOfTasks: u32, at 0x801f_9044;
    pub mut xSchedulerRunning: u32, at 0x801f_9048;
    pub mut uxSchedulerSuspended: u32, at 0x801f_904c;
    pub mut xTickCount: u32, at 0x801f_9050;
    pub mut pxDelayedTaskList: *mut u8, at 0x801f_9054;
    pub mut pxOverflowDelayedTaskList: *mut u8, at 0x801f_9058;
    pub mut xYieldPending: u32, at 0x801f_905c;
    pub mut xNextTaskUnblockTime: u32, at 0x801f_9064;
    pub mut xPendedTicks: u32, at 0x801f_9068;
    pub mut xNumOfOverflows: u32, at 0x801f_906c;
    pub mut xPendingReadyList: [u32; 5], at 0x801f_9070;
    pub mut xSuspendedTaskList: [u32; 5], at 0x801f_9084;
    pub mut xDelayedTaskList1: [u32; 5], at 0x801f_912c;
    pub mut xDelayedTaskList2: [u32; 5], at 0x801f_9140;
    pub mut uxCriticalNesting: u32, at 0x801f_6090;
    /// Followed by `xMinimumEverFreeBytesRemaining` and the successful
    /// allocation and free counts.
    pub mut xFreeBytesRemaining: u32, at 0x801f_9020;
}
