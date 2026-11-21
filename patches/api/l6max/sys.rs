// L6max MAIN 1.10 startup bindings.

firmware_functions! {
    pub unsafe fn sys_main_startup_initialize() at 0x8000_fe08;
}

firmware_variables! {
    /// 0 is the recorder, 1 a rejected update package, 4 after an update installs, 6 a valid update package.
    pub system_mode: u32, at 0x8046_2d9c;
    /// Bits 0 and 3 mark a rejected update package.
    pub update_package_flags: u8, at 0x8020_a5ca;
    /// Start of the live global settings; while nonzero, startup opens the
    /// Date/Time setup window.
    pub setup_required: u8, at 0x8046_01e0;
    /// Set while a factory session is connected.
    pub factory_connected: u32, at 0x8046_2da8;
    /// Sound Pad 4's WAV path (UTF-16) in the active settings table.
    pub sound_pad4_path: u16, at 0x8046_1872;
}
