// L6 MAIN 1.10 usb bindings.

firmware_symbols! {
    /// Wrapper that starts the USB device stack.
    pub usb_start_wrapper at 0x8007_3b84;
    /// USB device-stack initializer the start wrapper reaches.
    pub usb_start_initializer at 0x8004_1190;
    /// Gate that enables the USB PHY.
    pub usb_phy_gate at 0x8007_2e88;
}
