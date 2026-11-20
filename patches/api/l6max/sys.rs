// L6max MAIN 1.10 startup bindings.

firmware_functions! {
    pub unsafe fn sys_main_startup_initialize() at 0x8000_fe08;
}

firmware_variables! {
    /// 0 is the recorder, 1 a rejected update package, 4 after an update installs, 6 a valid update package.
    pub system_mode: u32, at 0x8046_2d9c;
    /// Bits 0 and 3 mark a rejected update package.
    pub update_package_flags: u8, at 0x8020_a5ca;
}
