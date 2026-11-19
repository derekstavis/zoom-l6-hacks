// Main menu bindings for L6max MAIN 1.10. The trees are `.data` records that
// the firmware initializes at boot; see the menu-loader README for the node
// layout this API relies on.

firmware_functions! {
    /// Main menu window enter callback: installs the menu controller and
    /// renderer, then redraws. Runs on the UI task with no arguments.
    pub unsafe fn menu_window_enter() -> u32, at 0x8003_cfb0;
    /// Generic submenu action: pushes the selected child and returns 1.
    pub unsafe fn menu_open_submenu() -> u32, at 0x8003_d278;
    /// Default action and refresh hook; returns 0.
    pub unsafe fn menu_action_none() -> u32, at 0x8003_d080;
}

/// Size of one menu node record.
pub const MENU_NODE_LEN: usize = 40;
/// Node field offsets.
pub const MENU_NODE_INDEX: usize = 0x00;
pub const MENU_NODE_FLAGS: usize = 0x04;
pub const MENU_NODE_REQUIRES: usize = 0x08;
pub const MENU_NODE_TITLE: usize = 0x0c;
pub const MENU_NODE_CHILDREN: usize = 0x10;
pub const MENU_NODE_CHILD_COUNT: usize = 0x14;
pub const MENU_NODE_SELECTED: usize = 0x16;
pub const MENU_NODE_SCROLL: usize = 0x18;
pub const MENU_NODE_REFRESH: usize = 0x1c;
pub const MENU_NODE_RESERVED: usize = 0x20;
pub const MENU_NODE_ACTION: usize = 0x24;

/// Context 0 nodes that patches can extend.
pub const MENU_NODE_MAIN: u32 = 0x8020_256c;
pub const MENU_NODE_PROJECT: u32 = 0x8020_1750;
pub const MENU_NODE_SOUND_PAD: u32 = 0x8020_17c8;
pub const MENU_NODE_MIXER: u32 = 0x8020_1728;
pub const MENU_NODE_SYSTEM: u32 = 0x8020_17f0;

/// Language 0 text resources: IDs 0..316 (ID 0 is "no text").
pub const GUI_TEXT_RESOURCE_COUNT: usize = 316;
