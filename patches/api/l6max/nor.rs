// L6max MAIN 1.10 main NOR bindings. Offsets are physical NOR offsets.
//
// The main NOR is a 2 MiB single-pad SPI flash on FlexSPI, driven by IP
// commands: fast read 0x0B, write enable 0x06, 4 KiB erase 0x20, 64 KiB
// erase 0xD8, page program 0x02 and read status 0x05. Only the read routine
// is bound here; erase (0x800215a0) and program (0x80021b28) are left out on
// purpose.

/// Size of the main NOR.
pub const NOR_SIZE: u32 = 0x20_0000;
/// End of the bootloader region at the start of the NOR.
pub const NOR_BOOTLOADER_END: u32 = 0x5_0000;

firmware_functions! {
    /// Reads `length` bytes at `offset` into `data` under the NOR driver's
    /// mutex, waiting for the flash to be idle first. Returns 0 on success
    /// and nonzero when `data` is null, the range passes `NOR_SIZE` or the
    /// transfer fails. One call moves at most 65,535 bytes.
    pub unsafe fn nor_read(offset: u32, data: *mut u8, length: u32) -> i32, at 0x8002_1a31;
}
