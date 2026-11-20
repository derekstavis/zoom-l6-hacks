// FreeRTOS bindings for L6 MAIN 1.10.

firmware_functions! {
    pub unsafe fn vTaskStartScheduler() at 0x8007_45b8;
    pub unsafe fn pvPortMalloc(size: usize) -> *mut u8, at 0x8006_de78;
    pub unsafe fn vPortFree(pointer: *mut u8) at 0x8007_3f48;
}

firmware_variables! {
    /// Followed by `xMinimumEverFreeBytesRemaining` and the successful
    /// allocation and free counts.
    pub mut xFreeBytesRemaining: u32, at 0x801f_9020;
}
