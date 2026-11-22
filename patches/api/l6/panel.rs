// L6 MAIN 1.10 panel bindings.

firmware_symbols! {
    /// Entry of the level-meter task.
    pub level_meter_task at 0x8002_b140;
    /// Call in the level-meter task that runs its long boot-time output sequence.
    pub level_meter_task_startup_call at 0x8002_b150;
    /// Builds the level-meter threshold stages.
    pub level_meter_threshold_builder at 0x8002_6f88;
    /// MAIN dispatcher for panel reports.
    pub panel_event_dispatcher at 0x8003_93d0;
    /// Handles a sound-pad GPIO event.
    pub sound_pad_event_handler at 0x8004_08b0;
}

firmware_variables! {
    pub level_meter_active_gates: u16, at 0x801f_8e74;
    pub level_meter_task_phase: u32, at 0x801f_8e84;
    /// Threshold masks and codes, 0x32 bytes.
    pub level_meter_thresholds: u8, at 0x801f_8e88;
    pub level_meter_converted_accumulators: u32, at 0x801f_90b8;
    /// Twelve bytes of meter group A records.
    pub level_meter_group_a_records: u8, at 0x8062_93dc;
    /// Twelve bytes of meter group B records.
    pub level_meter_group_b_records: u8, at 0x8062_93e8;
    /// The three-byte panel report being dispatched.
    pub panel_report: u8, at 0x808c_b54d;
    /// One byte per sound pad.
    /// Per-pad assignment pending flags, one byte per pad; the worker clears
    /// each when it finishes.
    pub sound_pad_assignments: u8, at 0x8044_337f;
}

/// Panel event map: 0xa0 bytes per column, 0x14 per row; the mapped event is at +8.
pub const PANEL_EVENT_TABLE: u32 = 0x800a_15b4;
/// Panel event map for column 0, laid out like `PANEL_EVENT_TABLE` rows.
pub const PANEL_COLUMN0_EVENT_TABLE: u32 = 0x800a_1834;
