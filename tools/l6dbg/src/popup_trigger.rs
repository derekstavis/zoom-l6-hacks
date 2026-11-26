//! Observe the real recorder polling callback's effect on a live knob popup.
use firmware_api::l6max;
use l6max_diagnostics::gdb;
use l6max_host::{engine::Options, headless::Headless};
use std::{
    ffi::OsString,
    fs, thread,
    time::{Duration, Instant},
};
pub fn run(args: Vec<OsString>) -> Result<(), Box<dyn std::error::Error>> {
    let mut options = Options::parse_args(args)?;
    if options.state_dir.is_some() {
        return Err("requires --volatile".into());
    }
    let socket =
        std::env::temp_dir().join(format!("l6dbg-popup-trigger-{}.sock", std::process::id()));
    options.gdb_socket = Some(socket.clone());
    let h = Headless::start(&options)?;
    // Own the stub before any probe input, including the later QMP stop.
    let lease = gdb::Lease::acquire(&socket)?;
    if h.engine.model() != l6fw::Model::L6max {
        return Err("the popup probe requires L6max firmware".into());
    }
    h.recorder()?;
    let q = &h.engine.management[0];
    let inputs = h.engine.inputs.as_ref().unwrap();
    let word = |address| -> Result<u32, Box<dyn std::error::Error>> {
        Ok(u32::from_le_bytes(
            q.read_memory(0, address, 4)?.try_into().unwrap(),
        ))
    };
    let original_bitmap = word(l6max::gui_bitmap_table_entry_0 + 36 * 4)?;
    h.tap(35, 50, 150)?;
    inputs.encoder(0, -1)?;
    h.wait(350)?;
    let snapshot = || -> Result<serde_json::Value, Box<dyn std::error::Error>> {
        Ok(serde_json::json!({
  "bitmap":word(l6max::gui_bitmap_table_entry_0 + 36 * 4)?,"busy":q.read_memory(0,l6max::notification_busy,1)?[0],
  "notification_active":q.read_memory(0,l6max::notification_active,1)?[0],
  "elapsed":u16::from_le_bytes(q.read_memory(0,l6max::notification_timer,2)?.try_into().unwrap()),
  "recorder_state_a":word(l6max::rec_command_running)?,"recorder_state_b":word(0x8020a9f4)?}))
    };
    let before = snapshot()?;
    assert_ne!(
        word(l6max::gui_bitmap_table_entry_0 + 36 * 4)?,
        original_bitmap,
        "no initial popup"
    );
    assert_eq!(before["busy"], 0);
    let mut g = gdb::Gdb::connect_leased(lease)?;
    q.stop()?;
    g.breakpoint(l6max::gui_notification_tick, true)?;
    q.resume()?;
    let deadline = Instant::now() + Duration::from_secs(15);
    while q.query_status()?.running {
        h.sample_display()?;
        if Instant::now() > deadline {
            return Err("tick breakpoint timeout".into());
        }
        thread::sleep(Duration::from_millis(5));
    }
    g.breakpoint(l6max::gui_notification_tick, false)?;
    // One callback invocation is replaced before its prologue. LR and the task
    // stack remain intact. No dirty flag or recorder state is fabricated.
    g.register(0, 0)?;
    g.register(15, l6max::rec_gui_refresh_tick)?;
    q.resume()?;
    h.wait(300)?;
    let after = snapshot()?;
    let report = serde_json::json!({"trigger":"original recorder periodic callback 0x8004da20","original_bitmap":original_bitmap,"before":before,"after":after});
    fs::write(
        options.logs.join("popup-trigger.json"),
        serde_json::to_vec_pretty(&report)?,
    )?;
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}
