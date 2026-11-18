// L6MAX MAIN 1.10 fs bindings.

// `fs_open` takes POSIX-style flags and a mode, in that order: flags 0 reads,
// 2 reads and writes, and 0x100 creates or replaces the file. Mode 0x180
// (0600) is what the stock writers pass; 0x100 creates a read-only file.
pub const FS_OPEN_READ: u32 = 0;
pub const FS_OPEN_CREATE_READ_WRITE: u32 = 0x102;
pub const FS_MODE_READ_WRITE: u32 = 0x180;

firmware_functions! {
    pub unsafe fn fs_mount(drive: u32) -> i32, at 0x8007_6e31;
    pub unsafe fn fs_ready() -> u32, at 0x8000_ebc9;
    pub unsafe fn fs_open(handle: *mut u32, path: *const u16, mode: u32, flags: u32) -> i32, at 0x8007_71a1;
    pub unsafe fn fs_read(handle: u32, data: *mut u8, length: u32, actual: *mut u32) -> i32, at 0x8007_77c9;
    /// Writes `length` bytes at the file position. `actual` receives the count
    /// written. Returns 0 on success.
    pub unsafe fn fs_write(handle: u32, data: *const u8, length: u32, actual: *mut u32) -> i32, at 0x8007_95c9;
    pub unsafe fn fs_close(handle: u32) -> i32, at 0x8007_33e1;
    pub unsafe fn fs_find_first(pattern: *const u16, flags: u32, info: *mut u8) -> i32, at 0x8007_4a79;
    pub unsafe fn fs_find_next(info: *mut u8) -> i32, at 0x8007_4fd1;
    pub unsafe fn fs_find_close(info: *mut u8) -> i32, at 0x8007_48c1;
}
