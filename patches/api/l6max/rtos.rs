// FreeRTOS and firmware semaphore bindings for L6max MAIN 1.10.

firmware_functions! {
    pub unsafe fn vTaskStartScheduler() at 0x8008_ec88;
    pub unsafe fn pvPortMalloc(size: usize) -> *mut u8, at 0x8008_5478;
    pub unsafe fn vPortFree(pointer: *mut u8) at 0x8008_e588;
    pub unsafe fn xQueueSemaphoreTake(queue: *mut u8, ticks_to_wait: u32) -> i32, at 0x8009_1048;
    pub unsafe fn xQueueGenericSend(
        queue: *mut u8,
        item: *const u8,
        ticks_to_wait: u32,
        copy_position: i32,
    ) -> i32, at 0x8009_0ad0;
    pub unsafe fn xQueueCreateCountingSemaphore(max_count: u32, initial_count: u32) -> *mut u8, at 0x8009_09c0;
    pub unsafe fn xTaskIncrementTick() -> i32, at 0x8009_1a20;
    pub unsafe fn vPortEnterCritical() at 0x8008_e508;
    pub unsafe fn vPortExitCritical() at 0x8008_e558;

    /// Firmware wrapper around xQueueCreateCountingSemaphore(max_count = 1).
    pub unsafe fn fw_semaphore_create(slot: u32, initial: u32) at 0x8005_f600;
}

firmware_symbols! {
    /// Creates a task and its stack from the heap. Its stack-depth argument
    /// type is unverified, so it is not declared as a function.
    pub xTaskCreate at 0x8009_1360;
    /// Moves the current task to a delayed or suspended list.
    pub prvAddCurrentTaskToDelayedList at 0x8008_4e90;
    /// The heap_4 arena's first block header.
    pub heap_first_block at 0x808a_4148;
    /// The heap_4 end-marker block; the arena ends here.
    pub heap_end_block at 0x8092_1140;
}

firmware_variables! {
    pub mut pxCurrentTCB: *mut u8, at 0x808a_3af0;
    /// One 20-byte list per priority, 32 priorities.
    pub mut pxReadyTasksLists: [[u32; 5]; 32], at 0x808a_3af4;
    pub mut uxTopReadyPriority: u32, at 0x8020_ab34;
    /// Increments as tasks are created and deleted.
    pub mut uxTaskNumber: u32, at 0x8020_ab38;
    pub mut uxCurrentNumberOfTasks: u32, at 0x8020_ab40;
    pub mut xSchedulerRunning: u32, at 0x8020_ab44;
    pub mut uxSchedulerSuspended: u32, at 0x8020_ab48;
    pub mut xTickCount: u32, at 0x8020_ab4c;
    pub mut pxDelayedTaskList: *mut u8, at 0x8020_ab50;
    pub mut pxOverflowDelayedTaskList: *mut u8, at 0x8020_ab54;
    pub mut xYieldPending: u32, at 0x8020_ab58;
    pub mut xNextTaskUnblockTime: u32, at 0x8020_ab60;
    pub mut xPendedTicks: u32, at 0x8020_ab64;
    pub mut xNumOfOverflows: u32, at 0x8020_ab68;
    pub mut xPendingReadyList: [u32; 5], at 0x8020_ab6c;
    pub mut xSuspendedTaskList: [u32; 5], at 0x8020_ab80;
    pub mut xDelayedTaskList1: [u32; 5], at 0x8020_aea8;
    pub mut xDelayedTaskList2: [u32; 5], at 0x8020_aebc;
    pub mut uxCriticalNesting: u32, at 0x8020_7634;
    pub mut xFreeBytesRemaining: u32, at 0x8020_ab1c;
    pub mut xMinimumEverFreeBytesRemaining: u32, at 0x8020_ab20;
    pub mut xNumberOfSuccessfulAllocations: u32, at 0x8020_ab24;
    pub mut xNumberOfSuccessfulFrees: u32, at 0x8020_ab28;
}
