//! Read-only Knob Dialog observations for the recovered L6max 1.10 layout.
use crate::qmp::QmpClient;
use firmware_api::l6max;
use l6fw::Model;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const GUI_GUARDS: [(u32, &str); 2] = [
    (
        0x8002_4ed0,
        "9a7023da16d1852350a9598799deb20e305f972219d2c0eb15ac10120d05f0ee",
    ),
    (
        0x8000_70d8,
        "1a01d3af4cef1eca54b6b27bd912f5dc96b5e6dd600700ed92967aa044370434",
    ),
];
const SDRAM_START: u32 = 0x8000_0000;
const SDRAM_END: u32 = 0x8400_0000;
const TEXT_ID: u32 = 179;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct Snapshot {
    pub revision_guard: String,
    pub atomic: bool,
    pub window: u32,
    pub state: [u8; 4],
    pub callback: u32,
    pub level: u8,
    /// Only read on request; it may change while this non-atomic sample runs.
    pub text: Option<String>,
}

/// Addresses captured during the same L6max 1.10 GUI transaction.
#[derive(Clone, Debug)]
pub struct RestorationPointers {
    pub bitmap_descriptor: u32,
    pub text_table: u32,
    pub shared_resource: u32,
    pub lengths_table: u32,
    pub widget7: u32,
    pub widget33: u32,
    pub backgrounds: [u32; 3],
}

/// Ordered QMP reads while the guest runs; values are not atomic.
#[derive(Clone, Debug)]
pub struct RestorationSnapshot {
    pub window: Vec<u8>,
    pub signature: Vec<u8>,
    pub bitmap: Vec<u8>,
    pub bitmap_descriptor: Vec<u8>,
    pub notification_semaphore: Vec<u8>,
    pub text_resources: Vec<u8>,
    pub shared_pointer: Vec<u8>,
    pub shared_contents: Vec<u8>,
    pub text_lengths: Vec<u8>,
    pub widget7: Vec<u8>,
    pub widget33: Vec<u8>,
    pub background10: Vec<u8>,
    pub background37: Vec<u8>,
    pub background48: Vec<u8>,
    pub callback: Vec<u8>,
}

/// Ordered navigation checks, sampled while the guest runs (not atomic).
#[derive(Clone, Debug, Default)]
pub struct NavigationSnapshot {
    pub window: Vec<u8>,
    pub signature: Vec<u8>,
    pub bitmap: Vec<u8>,
    pub bitmap_descriptor: Vec<u8>,
    pub notification_semaphore: Vec<u8>,
    pub shared_contents: Vec<u8>,
    pub widget7: Vec<u8>,
    pub widget33: Vec<u8>,
    pub background10: Vec<u8>,
    pub background37: Vec<u8>,
    pub background48: Vec<u8>,
    pub callback: Vec<u8>,
}

/// Ordered widget and layer reads; the guest remains running.
#[derive(Clone, Debug)]
pub struct OverlayDescriptors {
    pub widget7: Vec<u8>,
    pub widget33: Vec<u8>,
    pub backgrounds: [Vec<u8>; 3],
}

/// Non-atomic GUI state; display pixels are sampled separately by PanelFeed.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct RecorderState {
    pub revision_guard: String,
    pub atomic: bool,
    pub window: u32,
    pub notification_busy: u8,
    pub notification_active: u8,
}

/// Read-only guard fields. Coherence requires the caller to stop the guest.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct GuardState {
    pub window: u32,
    pub state: u32,
    pub callback: u32,
}

/// Non-atomic L6max 1.10 parameter selector and display callback slots.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct ParameterState {
    pub revision_guard: String,
    pub atomic: bool,
    pub mode: u8,
    pub callback_slots: [u32; 3],
}

fn checked_sdram(address: u32, size: usize) -> Result<u32, String> {
    let end = address
        .checked_add(u32::try_from(size).map_err(|_| "invalid size")?)
        .ok_or("GUI text pointer overflow")?;
    if address < SDRAM_START || end > SDRAM_END {
        return Err(format!("GUI text pointer {address:#010x} outside SDRAM"));
    }
    Ok(address)
}

fn read_word(
    read: &mut impl FnMut(u32, usize) -> Result<Vec<u8>, String>,
    address: u32,
) -> Result<u32, String> {
    let bytes = read(address, 4)?;
    Ok(u32::from_le_bytes(
        bytes.try_into().map_err(|_| "short GUI word")?,
    ))
}

pub(crate) fn verify(
    model: Model,
    read: &mut impl FnMut(u32, usize) -> Result<Vec<u8>, String>,
) -> Result<(), String> {
    if model != Model::L6max {
        return Err("knob popup observation requires L6max 1.10".into());
    }
    for (address, expected) in GUI_GUARDS {
        let actual = read(address, 64)?;
        if format!("{:x}", Sha256::digest(actual)) != expected {
            return Err("unsupported L6max GUI revision".into());
        }
    }
    Ok(())
}

fn read_text(
    read: &mut impl FnMut(u32, usize) -> Result<Vec<u8>, String>,
) -> Result<String, String> {
    let table = read_word(read, l6max::gui_text_pointers_by_language)?;
    read_text_at_table(read, table)
}

fn read_text_at_table(
    read: &mut impl FnMut(u32, usize) -> Result<Vec<u8>, String>,
    table: u32,
) -> Result<String, String> {
    let entry = table
        .checked_add(TEXT_ID * 4)
        .ok_or("GUI text table overflow")?;
    let pointer = read_word(read, checked_sdram(entry, 4)?)?;
    let bytes = read(checked_sdram(pointer, 48)?, 48)?;
    if bytes.len() != 48 {
        return Err("short GUI text".into());
    }
    let units = bytes
        .chunks_exact(2)
        .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
        .take_while(|unit| *unit != 0)
        .collect::<Vec<_>>();
    String::from_utf16(&units).map_err(|error| error.to_string())
}

fn read_snapshot(
    with_text: bool,
    read: &mut impl FnMut(u32, usize) -> Result<Vec<u8>, String>,
) -> Result<Snapshot, String> {
    let window = read_word(read, l6max::current_window)?;
    let state = read(l6max::gui_bitmap_table_entry_0, 4)?
        .try_into()
        .map_err(|_| "short dialog state")?;
    let callback = read_word(read, l6max::gui_display_callback_table + 20)?;
    let level = *read(l6max::mixer_channel_levels, 1)?
        .first()
        .ok_or("short mixer level")?;
    let text = if with_text {
        Some(read_text(read)?)
    } else {
        None
    };
    Ok(Snapshot {
        revision_guard: "l6max_1_10_gui_routines".into(),
        atomic: false,
        window,
        state,
        callback,
        level,
        text,
    })
}

fn read_background_bitmap_id(
    index: u32,
    read: &mut impl FnMut(u32, usize) -> Result<Vec<u8>, String>,
) -> Result<u16, String> {
    if index != 47 {
        return Err(format!(
            "unsupported background index {index}; only 47 is validated"
        ));
    }
    let address = l6max::gui_background_table
        .checked_add(index.checked_mul(4).ok_or("background index overflow")?)
        .ok_or("background table overflow")?;
    let pointer = u32::from_le_bytes(
        read(address, 4)?
            .try_into()
            .map_err(|_| "short background pointer")?,
    );
    let field = pointer
        .checked_add(2)
        .ok_or("background pointer overflow")?;
    let end = field.checked_add(2).ok_or("background pointer overflow")?;
    if pointer < SDRAM_START || end > SDRAM_END {
        return Err(format!("background pointer {pointer:#010x} outside SDRAM"));
    }
    let raw = read(field, 2)?;
    Ok(u16::from_le_bytes(
        raw.try_into().map_err(|_| "short background bitmap id")?,
    ))
}

fn read_preflight_state(
    read: &mut impl FnMut(u32, usize) -> Result<Vec<u8>, String>,
) -> Result<(Vec<u8>, Vec<u8>), String> {
    let state = read(l6max::gui_bitmap_table_entry_0, 4)?;
    let callback = read(l6max::gui_display_callback_table + 20, 4)?;
    Ok((state, callback))
}

fn read_restoration(
    pointers: &RestorationPointers,
    read: &mut impl FnMut(u32, usize) -> Result<Vec<u8>, String>,
) -> Result<RestorationSnapshot, String> {
    Ok(RestorationSnapshot {
        window: read(l6max::current_window, 4)?,
        signature: read(l6max::gui_bitmap_table_entry_0, 2)?,
        bitmap: read(l6max::gui_bitmap_table_entry_0 + 36 * 4, 4)?,
        bitmap_descriptor: read(pointers.bitmap_descriptor, 8)?,
        notification_semaphore: read(l6max::notification_slot, 4)?,
        text_resources: read(pointers.text_table + 229 * 4, 8)?,
        shared_pointer: read(pointers.text_table + 179 * 4, 4)?,
        shared_contents: read(pointers.shared_resource, 512)?,
        text_lengths: read(pointers.lengths_table + 229 * 2, 4)?,
        widget7: read(pointers.widget7, 16)?,
        widget33: read(pointers.widget33, 16)?,
        background10: read(pointers.backgrounds[0], 1)?,
        background37: read(pointers.backgrounds[1], 1)?,
        background48: read(pointers.backgrounds[2], 1)?,
        callback: read(l6max::gui_display_callback_table + 20, 4)?,
    })
}

fn read_navigation(
    p: &RestorationPointers,
    expected: &NavigationSnapshot,
    read: &mut impl FnMut(u32, usize) -> Result<Vec<u8>, String>,
) -> Result<NavigationSnapshot, String> {
    let mut out = NavigationSnapshot::default();
    out.window = read(l6max::current_window, 4)?;
    if out.window == expected.window {
        return Ok(out);
    }
    macro_rules! field {
        ($field:ident, $address:expr, $length:expr) => {{
            out.$field = read($address, $length)?;
            if out.$field != expected.$field {
                return Ok(out);
            }
        }};
    }
    field!(signature, l6max::gui_bitmap_table_entry_0, 2);
    field!(bitmap, l6max::gui_bitmap_table_entry_0 + 36 * 4, 4);
    field!(bitmap_descriptor, p.bitmap_descriptor, 8);
    field!(notification_semaphore, l6max::notification_slot, 4);
    field!(shared_contents, p.shared_resource, 512);
    field!(widget7, p.widget7, 16);
    field!(widget33, p.widget33, 16);
    field!(background10, p.backgrounds[0], 1);
    field!(background37, p.backgrounds[1], 1);
    field!(background48, p.backgrounds[2], 1);
    field!(callback, l6max::gui_display_callback_table + 20, 4);
    Ok(out)
}

/// A revision-checked reader for one uninterrupted observation step.
pub struct Reader<'a> {
    qmp: &'a QmpClient,
}

impl Reader<'_> {
    /// The caller owns guest pause/resume; reads are coherent only while paused.
    pub fn guard_state(&self) -> Result<GuardState, String> {
        let mut read = |address, size| {
            self.qmp
                .read_memory(0, address, size)
                .map_err(|error| error.to_string())
        };
        Ok(GuardState {
            state: read_word(&mut read, l6max::gui_bitmap_table_entry_0)?,
            callback: read_word(&mut read, l6max::gui_display_callback_table + 20)?,
            window: read_word(&mut read, l6max::current_window)?,
        })
    }

    pub fn recorder_state(&self) -> Result<RecorderState, String> {
        let mut read = |address, size| {
            self.qmp
                .read_memory(0, address, size)
                .map_err(|error| error.to_string())
        };
        Ok(RecorderState {
            revision_guard: "l6max_1_10_gui_routines".into(),
            atomic: false,
            window: read_word(&mut read, l6max::current_window)?,
            notification_busy: *read(l6max::notification_busy, 1)?
                .first()
                .ok_or("short notification state")?,
            notification_active: *read(l6max::notification_active, 1)?
                .first()
                .ok_or("short notification state")?,
        })
    }

    /// Channel-one raw level, sampled independently.
    pub fn level(&self) -> Result<u8, String> {
        self.qmp
            .read_memory(0, l6max::mixer_channel_levels, 1)
            .map_err(|error| error.to_string())?
            .first()
            .copied()
            .ok_or("short mixer level".into())
    }

    /// Capture restoration fields in firmware check order; the guest is not paused.
    pub fn restoration(
        &self,
        pointers: &RestorationPointers,
    ) -> Result<RestorationSnapshot, String> {
        read_restoration(pointers, &mut |address, len| {
            self.qmp
                .read_memory(0, address, len)
                .map_err(|error| error.to_string())
        })
    }

    /// Capture navigation fields in firmware check order while the guest runs.
    pub fn navigation(
        &self,
        pointers: &RestorationPointers,
        expected: &NavigationSnapshot,
    ) -> Result<NavigationSnapshot, String> {
        read_navigation(pointers, expected, &mut |address, len| {
            self.qmp
                .read_memory(0, address, len)
                .map_err(|error| error.to_string())
        })
    }

    /// Channel-one pan byte for the guarded L6max mixer layout.
    pub fn pan(&self) -> Result<u8, String> {
        self.qmp
            .read_memory(0, l6max::mixer_first_channel_pan, 1)
            .map_err(|error| error.to_string())?
            .first()
            .copied()
            .ok_or("short pan value".into())
    }

    /// Mixer level for a one-based L6max channel.
    pub fn channel_level(&self, channel: u32) -> Result<u8, String> {
        if !(1..=8).contains(&channel) {
            return Err("channel must be 1..8".into());
        }
        self.qmp
            .read_memory(0, l6max::mixer_channel_levels + channel - 1, 1)
            .map_err(|error| error.to_string())?
            .first()
            .copied()
            .ok_or("short mixer level".into())
    }

    /// Read the bitmap identifier from a background table entry.
    pub fn background_bitmap_id(&self, index: u32) -> Result<u16, String> {
        read_background_bitmap_id(index, &mut |address, len| {
            self.qmp
                .read_memory(0, address, len)
                .map_err(|e| e.to_string())
        })
    }

    /// Read the in-place popup state then callback slot five, in that order.
    pub fn preflight_state(&self) -> Result<(Vec<u8>, Vec<u8>), String> {
        read_preflight_state(&mut |address, len| {
            self.qmp
                .read_memory(0, address, len)
                .map_err(|e| e.to_string())
        })
    }

    /// Sample the six callback slots in one non-atomic guest read.
    pub fn callback_slots(&self) -> Result<[u32; 6], String> {
        let bytes = self
            .qmp
            .read_memory(0, l6max::gui_display_callback_table, 24)
            .map_err(|e| e.to_string())?;
        if bytes.len() != 24 {
            return Err("short callback table".into());
        }
        Ok(std::array::from_fn(|i| {
            u32::from_le_bytes(bytes[i * 4..i * 4 + 4].try_into().unwrap())
        }))
    }

    /// Current recorder window as its raw word.
    pub fn window(&self) -> Result<Vec<u8>, String> {
        self.qmp
            .read_memory(0, l6max::current_window, 4)
            .map_err(|e| e.to_string())
    }
    /// Widget and background descriptors in original firmware-check order.
    pub fn overlay_descriptors(
        &self,
        widget7: u32,
        widget33: u32,
        backgrounds: [u32; 3],
    ) -> Result<OverlayDescriptors, String> {
        let read = |address, len| {
            self.qmp
                .read_memory(0, address, len)
                .map_err(|e| e.to_string())
        };
        Ok(OverlayDescriptors {
            widget7: read(widget7, 16)?,
            widget33: read(widget33, 16)?,
            backgrounds: [
                read(backgrounds[0], 1)?,
                read(backgrounds[1], 1)?,
                read(backgrounds[2], 1)?,
            ],
        })
    }
    /// Resource 179 pointer from a previously captured text table.
    pub fn shared_resource_pointer(&self, table: u32) -> Result<Vec<u8>, String> {
        self.qmp
            .read_memory(0, table + 179 * 4, 4)
            .map_err(|e| e.to_string())
    }
    /// Current dialog signature bytes.
    pub fn state_signature(&self) -> Result<Vec<u8>, String> {
        self.qmp
            .read_memory(0, l6max::gui_bitmap_table_entry_0, 2)
            .map_err(|e| e.to_string())
    }
    /// Current pointer for bitmap 36.
    pub fn bitmap36(&self) -> Result<Vec<u8>, String> {
        self.qmp
            .read_memory(0, l6max::gui_bitmap_table_entry_0 + 36 * 4, 4)
            .map_err(|e| e.to_string())
    }

    /// Dialog inactivity countdown byte, sampled independently.
    pub fn countdown_ticks(&self) -> Result<u8, String> {
        self.qmp
            .read_memory(0, l6max::gui_bitmap_table_entry_0 + 2, 1)
            .map_err(|error| error.to_string())?
            .first()
            .copied()
            .ok_or("short dialog state".into())
    }

    /// Dialog callback mailbox byte, sampled independently.
    pub fn mailbox(&self) -> Result<u8, String> {
        self.qmp
            .read_memory(0, l6max::gui_bitmap_table_entry_0 + 3, 1)
            .map_err(|error| error.to_string())?
            .first()
            .copied()
            .ok_or("short dialog state".into())
    }

    /// Decode current dialog text using a previously captured table pointer.
    pub fn text_at_table(&self, table: u32) -> Result<String, String> {
        read_text_at_table(
            &mut |address, size| {
                self.qmp
                    .read_memory(0, address, size)
                    .map_err(|error| error.to_string())
            },
            table,
        )
    }

    /// Values can change between QMP reads; this is not an atomic snapshot.
    pub fn snapshot(&self, with_text: bool) -> Result<Snapshot, String> {
        read_snapshot(with_text, &mut |address, size| {
            self.qmp
                .read_memory(0, address, size)
                .map_err(|error| error.to_string())
        })
    }
}

fn checked_overlay_read<T>(
    check: &dyn Fn() -> Result<(), String>,
    read: impl FnOnce() -> Result<T, String>,
) -> Result<T, String> {
    check()?;
    let value = read()?;
    check()?;
    Ok(value)
}

/// Revision-checked reader for one bounded overlay transaction.
/// Recreate after guest restart or firmware changes.
pub struct OverlayReader {
    qmp: QmpClient,
}
impl OverlayReader {
    fn borrowed(&self) -> Reader<'_> {
        Reader { qmp: &self.qmp }
    }
    pub fn background_bitmap_id(&self, index: u32) -> Result<u16, String> {
        self.borrowed().background_bitmap_id(index)
    }
    pub fn preflight_state(&self) -> Result<(Vec<u8>, Vec<u8>), String> {
        self.borrowed().preflight_state()
    }
    pub fn window(&self) -> Result<Vec<u8>, String> {
        self.borrowed().window()
    }
    pub fn overlay_descriptors(
        &self,
        widget7: u32,
        widget33: u32,
        backgrounds: [u32; 3],
    ) -> Result<OverlayDescriptors, String> {
        self.borrowed()
            .overlay_descriptors(widget7, widget33, backgrounds)
    }
    pub fn shared_resource_pointer(&self, table: u32) -> Result<Vec<u8>, String> {
        self.borrowed().shared_resource_pointer(table)
    }
    pub fn state_signature(&self) -> Result<Vec<u8>, String> {
        self.borrowed().state_signature()
    }
    pub fn bitmap36(&self) -> Result<Vec<u8>, String> {
        self.borrowed().bitmap36()
    }
    /// Samples the notification semaphore count at a previously verified address.
    pub fn notification_count(&self, semaphore: u32) -> Result<Vec<u8>, String> {
        self.qmp
            .read_memory(0, semaphore + 56, 4)
            .map_err(|e| e.to_string())
    }
    pub fn countdown_ticks(&self) -> Result<u8, String> {
        self.borrowed().countdown_ticks()
    }
    pub fn background_bitmap_id_before_checked(
        &self,
        index: u32,
        deadline: std::time::Instant,
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<u16, String> {
        read_background_bitmap_id(index, &mut |address, size| {
            check()?;
            let bytes = self
                .qmp
                .read_memory_before(0, address, size, deadline)
                .map_err(|e| e.to_string())?;
            check()?;
            Ok(bytes)
        })
    }
    fn byte_before_checked(
        &self,
        address: u32,
        deadline: std::time::Instant,
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<u8, String> {
        let bytes = checked_overlay_read(check, || {
            self.qmp
                .read_memory_before(0, address, 1, deadline)
                .map_err(|e| e.to_string())
        })?;
        bytes.first().copied().ok_or("short overlay byte".into())
    }
    pub fn countdown_ticks_before_checked(
        &self,
        deadline: std::time::Instant,
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<u8, String> {
        self.byte_before_checked(l6max::gui_bitmap_table_entry_0 + 2, deadline, check)
    }
    pub fn mailbox_before_checked(
        &self,
        deadline: std::time::Instant,
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<u8, String> {
        self.byte_before_checked(l6max::gui_bitmap_table_entry_0 + 3, deadline, check)
    }
    pub fn level_before_checked(
        &self,
        deadline: std::time::Instant,
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<u8, String> {
        self.byte_before_checked(l6max::mixer_channel_levels, deadline, check)
    }

    pub fn level(&self) -> Result<u8, String> {
        self.borrowed().level()
    }
    pub fn mailbox(&self) -> Result<u8, String> {
        self.borrowed().mailbox()
    }
    pub fn text_at_table(&self, table: u32) -> Result<String, String> {
        self.borrowed().text_at_table(table)
    }
}

pub fn overlay_reader(qmp: &QmpClient, model: Model) -> Result<OverlayReader, String> {
    verify(model, &mut |address, size| {
        qmp.read_memory(0, address, size).map_err(|e| e.to_string())
    })?;
    Ok(OverlayReader { qmp: qmp.clone() })
}

/// Construct an overlay reader using a shared QMP deadline and cancellation check.
pub fn overlay_reader_before_checked(
    qmp: &QmpClient,
    model: Model,
    deadline: std::time::Instant,
    check: &dyn Fn() -> Result<(), String>,
) -> Result<OverlayReader, String> {
    verify(model, &mut |address, size| {
        check()?;
        let bytes = qmp
            .read_memory_before(0, address, size, deadline)
            .map_err(|e| e.to_string())?;
        check()?;
        Ok(bytes)
    })?;
    Ok(OverlayReader { qmp: qmp.clone() })
}

/// Host-owned retained debugger/QMP scope for the multi-phase knob-dialog
/// probes. It keeps transport ownership, temporary mutations, and rollback
/// bookkeeping together while the scenario retains ordering and assertions.
pub struct ProbeSession {
    debugger: crate::debug::Session,
    journal: crate::debug_journal::Journal,
    finalized: bool,
}

trait ProbeDebugger {
    fn stop_before(&self, deadline: std::time::Instant) -> Result<(), String>;
    fn verify_revision_before(&self, deadline: std::time::Instant) -> Result<(), String>;
    fn read_memory_before(
        &self,
        address: u32,
        length: usize,
        deadline: std::time::Instant,
    ) -> Result<Vec<u8>, String>;
    fn write_memory_before(
        &mut self,
        address: u32,
        bytes: &[u8],
        deadline: std::time::Instant,
    ) -> Result<(), String>;
    fn clear_breakpoint_before(
        &mut self,
        address: u32,
        deadline: std::time::Instant,
    ) -> Result<(), String>;
    fn write_register_before(
        &mut self,
        index: u32,
        value: u32,
        deadline: std::time::Instant,
    ) -> Result<(), String>;
    fn restore_state_before(
        &self,
        running: bool,
        deadline: std::time::Instant,
    ) -> Result<(), String>;
}

impl ProbeDebugger for crate::debug::Session {
    fn stop_before(&self, deadline: std::time::Instant) -> Result<(), String> {
        self.guest_stop_before(deadline)
    }

    fn verify_revision_before(&self, deadline: std::time::Instant) -> Result<(), String> {
        verify(Model::L6max, &mut |address, length| {
            self.guest_read_before(address, length, deadline)
        })
    }

    fn read_memory_before(
        &self,
        address: u32,
        length: usize,
        deadline: std::time::Instant,
    ) -> Result<Vec<u8>, String> {
        self.guest_read_before(address, length, deadline)
    }

    fn write_memory_before(
        &mut self,
        address: u32,
        bytes: &[u8],
        deadline: std::time::Instant,
    ) -> Result<(), String> {
        crate::debug::Session::write_memory_before(self, address, bytes, deadline)
    }

    fn clear_breakpoint_before(
        &mut self,
        address: u32,
        deadline: std::time::Instant,
    ) -> Result<(), String> {
        self.breakpoint_before(address, false, deadline)
    }

    fn write_register_before(
        &mut self,
        index: u32,
        value: u32,
        deadline: std::time::Instant,
    ) -> Result<(), String> {
        crate::debug::Session::write_register_before(self, index, value, deadline)
    }

    fn restore_state_before(
        &self,
        running: bool,
        deadline: std::time::Instant,
    ) -> Result<(), String> {
        self.guest_restore_state_before(running, deadline)
    }
}

struct ProbeCleanup<'a, D: ProbeDebugger> {
    debugger: &'a mut D,
    journal: &'a mut crate::debug_journal::Journal,
    deadline: std::time::Instant,
    was_running: bool,
}

impl<D: ProbeDebugger> crate::debug_journal::Cleanup for ProbeCleanup<'_, D> {
    fn stop(&mut self) -> Result<(), String> {
        self.debugger.stop_before(self.deadline)
    }

    fn synchronized(&self) -> bool {
        self.journal.synchronized && !self.journal.mailbox_pending
    }

    fn restore_writes(&mut self) -> Vec<String> {
        while let Some(write) = self.journal.writes.last().cloned() {
            let current = self
                .debugger
                .verify_revision_before(self.deadline)
                .and_then(|()| {
                    self.debugger.read_memory_before(
                        write.address,
                        write.after.len(),
                        self.deadline,
                    )
                });
            let guard = current.and_then(|bytes| {
                if bytes == write.after {
                    Ok(())
                } else {
                    Err(format!(
                        "temporary target changed at {:#010x}",
                        write.address
                    ))
                }
            });
            if let Err(error) = self.journal.restoration_guard(guard) {
                return vec![format!("restore guard: {error}")];
            }
            if let Err(error) =
                self.debugger
                    .write_memory_before(write.address, &write.before, self.deadline)
            {
                self.journal.synchronized = false;
                return vec![format!("restore {:#010x}: {error}", write.address)];
            }
            self.journal.writes.pop();
        }
        Vec::new()
    }

    fn clear_breakpoints(&mut self) -> Vec<String> {
        while let Some(address) = self.journal.breakpoints.last().copied() {
            if let Err(error) = self
                .debugger
                .clear_breakpoint_before(address, self.deadline)
            {
                self.journal.synchronized = false;
                return vec![format!("remove breakpoint {address:#010x}: {error}")];
            }
            self.journal.breakpoints.pop();
        }
        Vec::new()
    }

    fn restore_registers(&mut self) -> Vec<String> {
        while let Some((index, value)) = self.journal.registers.last().copied() {
            if let Err(error) = self
                .debugger
                .write_register_before(index, value, self.deadline)
            {
                self.journal.synchronized = false;
                return vec![format!("restore register {index}: {error}")];
            }
            self.journal.registers.pop();
        }
        Vec::new()
    }

    fn resume(&mut self) -> Result<(), String> {
        if let Err(error) = self
            .debugger
            .restore_state_before(self.was_running, self.deadline)
        {
            self.journal.synchronized = false;
            return Err(error);
        }
        Ok(())
    }
}

impl ProbeSession {
    pub fn begin_before(
        controller: &crate::debug::Controller,
        deadline: std::time::Instant,
    ) -> Result<Self, String> {
        let debugger = controller.begin_before(deadline)?;
        let mut session = Self {
            debugger,
            journal: crate::debug_journal::Journal::default(),
            finalized: false,
        };
        let guard = verify(Model::L6max, &mut |address, length| {
            session
                .debugger
                .guest_read_before(address, length, deadline)
        });
        if let Err(error) = guard {
            let cleanup = session.debugger.close_before(deadline);
            session.finalized = true;
            return Err(match cleanup {
                Ok(()) => error,
                Err(cleanup) => {
                    format!("{error}; knob probe acquisition cleanup failed: {cleanup}")
                }
            });
        }
        Ok(session)
    }

    pub fn read_memory_before(
        &self,
        address: u32,
        length: usize,
        deadline: std::time::Instant,
    ) -> Result<Vec<u8>, String> {
        self.debugger.guest_read_before(address, length, deadline)
    }

    pub fn stop_before(&self, deadline: std::time::Instant) -> Result<(), String> {
        self.debugger.guest_stop_before(deadline)
    }

    pub fn stop(&self) -> Result<(), String> {
        self.stop_before(std::time::Instant::now() + std::time::Duration::from_secs(5))
    }

    pub fn resume_guest_before(&mut self, deadline: std::time::Instant) -> Result<(), String> {
        if !self.journal.synchronized {
            return Err("debugger transport not synchronized; MAIN left stopped".into());
        }
        self.journal.registers.clear();
        if let Err(error) = self.debugger.guest_resume_before(deadline) {
            self.journal.synchronized = false;
            return Err(error);
        }
        Ok(())
    }

    pub fn resume_guest(&mut self) -> Result<(), String> {
        self.resume_guest_before(std::time::Instant::now() + std::time::Duration::from_secs(5))
    }

    pub fn write_before(
        &mut self,
        address: u32,
        data: &[u8],
        deadline: std::time::Instant,
        mut check: impl FnMut() -> Result<(), String>,
    ) -> Result<(), String> {
        check()?;
        verify(Model::L6max, &mut |at, length| {
            check()?;
            let bytes = self.debugger.guest_read_before(at, length, deadline)?;
            check()?;
            Ok(bytes)
        })?;
        let before = self
            .debugger
            .guest_read_before(address, data.len(), deadline)?;
        check()?;
        let restoring = self
            .journal
            .begin_write(address, before, data)
            .map_err(|e| e.to_string())?;
        if let Err(error) = self.debugger.write_memory_before(address, data, deadline) {
            self.journal.synchronized = false;
            return Err(error);
        }
        check()?;
        self.journal.commit_write(restoring);
        Ok(())
    }

    pub fn write_register_before(
        &mut self,
        index: u32,
        value: u32,
        deadline: std::time::Instant,
    ) -> Result<(), String> {
        let before = self.debugger.register_before(index, deadline)?;
        self.journal.registers.push((index, before));
        if let Err(error) = self.debugger.write_register_before(index, value, deadline) {
            self.journal.synchronized = false;
            return Err(error);
        }
        Ok(())
    }

    pub fn write_register(&mut self, index: u32, value: u32) -> Result<(), String> {
        self.write_register_before(
            index,
            value,
            std::time::Instant::now() + std::time::Duration::from_secs(5),
        )
    }

    pub fn register_before(
        &mut self,
        index: u32,
        deadline: std::time::Instant,
    ) -> Result<u32, String> {
        let result = self.debugger.register_before(index, deadline);
        if result.is_err() {
            self.journal.synchronized = false;
        }
        result
    }

    pub fn register(&mut self, index: u32) -> Result<u32, String> {
        self.register_before(
            index,
            std::time::Instant::now() + std::time::Duration::from_secs(5),
        )
    }

    pub fn breakpoint_before(
        &mut self,
        address: u32,
        enable: bool,
        deadline: std::time::Instant,
    ) -> Result<(), String> {
        if enable && !self.journal.breakpoints.contains(&address) {
            self.journal.breakpoints.push(address);
        }
        if let Err(error) = self.debugger.breakpoint_before(address, enable, deadline) {
            self.journal.synchronized = false;
            return Err(error);
        }
        if !enable {
            self.journal.breakpoints.retain(|item| *item != address);
        }
        Ok(())
    }

    pub fn breakpoint(&mut self, address: u32, enable: bool) -> Result<(), String> {
        self.breakpoint_before(
            address,
            enable,
            std::time::Instant::now() + std::time::Duration::from_secs(5),
        )
    }

    pub fn continue_before(&mut self, deadline: std::time::Instant) -> Result<String, String> {
        self.journal.registers.clear();
        let result = self.debugger.continue_before(deadline, || false);
        if result.is_err() {
            self.journal.synchronized = false;
        }
        result
    }

    pub fn continue_until_stop(&mut self, timeout: std::time::Duration) -> Result<String, String> {
        self.continue_before(std::time::Instant::now() + timeout)
    }

    pub fn call_thumb_before(
        &mut self,
        address: u32,
        arguments: &[u32],
        deadline: std::time::Instant,
    ) -> Result<String, String> {
        self.journal.registers.clear();
        let result = self
            .debugger
            .call_thumb_before(address, arguments, deadline);
        if result.is_err() {
            self.journal.synchronized = false;
        }
        result
    }

    pub fn call_thumb(
        &mut self,
        address: u32,
        arguments: &[u32],
        timeout: std::time::Duration,
    ) -> Result<String, String> {
        self.call_thumb_before(address, arguments, std::time::Instant::now() + timeout)
    }

    pub fn handoff_mailbox(&mut self, address: u32) {
        self.journal.handoff_mailbox(address);
    }

    pub fn acknowledge_mailbox(&mut self) {
        self.journal.acknowledge_mailbox();
    }

    pub fn restoration_guard(&mut self, result: Result<(), String>) -> Result<(), String> {
        self.journal.restoration_guard(result)
    }

    pub fn finish<T>(
        mut self,
        result: Result<T, String>,
        deadline: std::time::Instant,
    ) -> Result<T, String> {
        match result {
            Ok(value) if self.journal.clean() => {
                let close = self.debugger.close_preserving_guest_state_before(deadline);
                self.finalized = true;
                close.map(|()| value)
            }
            Ok(_) => {
                let failures = self.cleanup(deadline);
                self.finalized = true;
                Err(format!(
                    "knob probe ended with active mutations; cleanup: {}",
                    if failures.is_empty() {
                        "completed".to_owned()
                    } else {
                        failures.join("; ")
                    }
                ))
            }
            Err(error) => {
                let failures = self.cleanup(deadline);
                self.finalized = true;
                if failures.is_empty() {
                    Err(error)
                } else {
                    Err(format!(
                        "{error}; knob probe cleanup failed: {}",
                        failures.join("; ")
                    ))
                }
            }
        }
    }

    pub fn abort(mut self, deadline: std::time::Instant) -> Result<(), String> {
        let failures = self.cleanup(deadline);
        self.finalized = true;
        if failures.is_empty() {
            Ok(())
        } else {
            Err(failures.join("; "))
        }
    }

    fn cleanup(&mut self, deadline: std::time::Instant) -> Vec<String> {
        let was_running = self
            .debugger
            .guest_running_before(deadline)
            .unwrap_or(false);
        let failures = crate::debug_journal::cleanup(&mut ProbeCleanup {
            debugger: &mut self.debugger,
            journal: &mut self.journal,
            deadline,
            was_running,
        });
        if failures.is_empty() {
            if let Err(error) = self.debugger.close_preserving_guest_state_before(deadline) {
                return vec![format!("close debugger: {error}")];
            }
            return Vec::new();
        }
        let mut errors = failures;
        if let Err(error) = self.debugger.close_stopped_before(deadline) {
            errors.push(format!("close stopped debugger: {error}"));
        }
        errors
    }
}

impl Drop for ProbeSession {
    fn drop(&mut self) {
        if !self.finalized {
            let failures =
                self.cleanup(std::time::Instant::now() + std::time::Duration::from_secs(3));
            if !failures.is_empty() {
                tracing::error!(
                    "abandoned knob probe cleanup failed: {}",
                    failures.join("; ")
                );
            }
        }
    }
}

#[cfg(test)]
mod probe_cleanup_tests {
    use super::*;
    use std::cell::RefCell;

    struct FakeDebugger {
        calls: RefCell<Vec<&'static str>>,
        bytes: Vec<u8>,
        revision_fails: bool,
        restore_fails: bool,
    }

    impl FakeDebugger {
        fn new(bytes: Vec<u8>) -> Self {
            Self {
                calls: RefCell::new(Vec::new()),
                bytes,
                revision_fails: false,
                restore_fails: false,
            }
        }

        fn calls(&self) -> Vec<&'static str> {
            self.calls.borrow().clone()
        }
    }

    impl ProbeDebugger for FakeDebugger {
        fn stop_before(&self, _: std::time::Instant) -> Result<(), String> {
            self.calls.borrow_mut().push("stop");
            Ok(())
        }

        fn verify_revision_before(&self, _: std::time::Instant) -> Result<(), String> {
            self.calls.borrow_mut().push("revision");
            if self.revision_fails {
                Err("revision changed".into())
            } else {
                Ok(())
            }
        }

        fn read_memory_before(
            &self,
            _: u32,
            _: usize,
            _: std::time::Instant,
        ) -> Result<Vec<u8>, String> {
            self.calls.borrow_mut().push("read");
            Ok(self.bytes.clone())
        }

        fn write_memory_before(
            &mut self,
            _: u32,
            _: &[u8],
            _: std::time::Instant,
        ) -> Result<(), String> {
            self.calls.borrow_mut().push("write");
            if self.restore_fails {
                Err("write failed".into())
            } else {
                Ok(())
            }
        }

        fn clear_breakpoint_before(&mut self, _: u32, _: std::time::Instant) -> Result<(), String> {
            self.calls.borrow_mut().push("breakpoint");
            Ok(())
        }

        fn write_register_before(
            &mut self,
            _: u32,
            _: u32,
            _: std::time::Instant,
        ) -> Result<(), String> {
            self.calls.borrow_mut().push("register");
            Ok(())
        }

        fn restore_state_before(&self, _: bool, _: std::time::Instant) -> Result<(), String> {
            self.calls.borrow_mut().push("resume");
            Ok(())
        }
    }

    fn write_journal() -> crate::debug_journal::Journal {
        let mut journal = crate::debug_journal::Journal::default();
        journal
            .begin_write(0x8000_1000, vec![0x11], &[0x22])
            .unwrap();
        journal.breakpoints.push(0x8000_2000);
        journal.registers.push((0, 1));
        journal
    }

    fn cleanup(
        debugger: &mut FakeDebugger,
        journal: &mut crate::debug_journal::Journal,
    ) -> Vec<String> {
        crate::debug_journal::cleanup(&mut ProbeCleanup {
            debugger,
            journal,
            deadline: std::time::Instant::now() + std::time::Duration::from_secs(1),
            was_running: true,
        })
    }

    #[test]
    fn host_cleanup_restores_in_order_before_restoring_guest_state() {
        let mut debugger = FakeDebugger::new(vec![0x22]);
        let mut journal = write_journal();
        assert!(cleanup(&mut debugger, &mut journal).is_empty());
        assert_eq!(
            debugger.calls(),
            [
                "stop",
                "revision",
                "read",
                "write",
                "breakpoint",
                "register",
                "resume"
            ]
        );
        assert!(journal.clean());
    }

    #[test]
    fn failed_revision_guard_retains_mutations_and_never_resumes() {
        let mut debugger = FakeDebugger::new(vec![0x22]);
        debugger.revision_fails = true;
        let mut journal = write_journal();
        let failures = cleanup(&mut debugger, &mut journal);
        assert!(failures.join(" ").contains("revision changed"));
        assert_eq!(debugger.calls(), ["stop", "revision"]);
        assert_eq!(journal.writes.len(), 1);
        assert!(!journal.synchronized);
    }

    #[test]
    fn failed_after_image_check_retains_mutations_and_never_resumes() {
        let mut debugger = FakeDebugger::new(vec![0x99]);
        let mut journal = write_journal();
        let failures = cleanup(&mut debugger, &mut journal);
        assert!(failures.join(" ").contains("temporary target changed"));
        assert_eq!(debugger.calls(), ["stop", "revision", "read"]);
        assert_eq!(journal.writes.len(), 1);
        assert!(!journal.synchronized);
    }

    #[test]
    fn failed_restore_write_keeps_journal_and_skips_later_cleanup() {
        let mut debugger = FakeDebugger::new(vec![0x22]);
        debugger.restore_fails = true;
        let mut journal = write_journal();
        let failures = cleanup(&mut debugger, &mut journal);
        assert!(failures.join(" ").contains("write failed"));
        assert_eq!(debugger.calls(), ["stop", "revision", "read", "write"]);
        assert_eq!(journal.writes.len(), 1);
        assert!(!journal.synchronized);
    }
}

/// GUI reader additionally guarded by the parameter-mode getter code.
pub struct ParameterReader<'a> {
    qmp: &'a QmpClient,
}

impl ParameterReader<'_> {
    pub fn parameter_mode(&self) -> Result<u8, String> {
        self.qmp
            .read_memory(0, 0x8046_2d2c, 1)
            .map_err(|error| error.to_string())?
            .first()
            .copied()
            .ok_or("short parameter mode".into())
    }

    pub fn parameter_state(&self) -> Result<ParameterState, String> {
        let callback = self
            .qmp
            .read_memory(0, l6max::gui_display_callback_table, 24)
            .map_err(|error| error.to_string())?;
        if callback.len() != 24 {
            return Err("short callback table".into());
        }
        let mode = self
            .qmp
            .read_memory(0, 0x8046_2d2c, 1)
            .map_err(|error| error.to_string())?;
        Ok(ParameterState {
            revision_guard: "l6max_1_10_gui_and_parameter_getter".into(),
            atomic: false,
            mode: *mode.first().ok_or("short parameter mode")?,
            callback_slots: [
                u32::from_le_bytes(callback[0..4].try_into().unwrap()),
                u32::from_le_bytes(callback[8..12].try_into().unwrap()),
                u32::from_le_bytes(callback[20..24].try_into().unwrap()),
            ],
        })
    }
}

fn verify_parameter_getter(
    read: &mut impl FnMut(u32, usize) -> Result<Vec<u8>, String>,
) -> Result<(), String> {
    let actual = read(0x8000_b4a8, 64)?;
    if format!("{:x}", Sha256::digest(actual))
        != "9994530d6e08e423209efa0bfc2e17c32a10eaf2290f1313ae05337b076e9a42"
    {
        return Err("unsupported L6max parameter getter revision".into());
    }
    Ok(())
}

pub fn parameter_reader(qmp: &QmpClient, model: Model) -> Result<ParameterReader<'_>, String> {
    reader(qmp, model)?;
    verify_parameter_getter(&mut |address, size| {
        qmp.read_memory(0, address, size)
            .map_err(|error| error.to_string())
    })?;
    Ok(ParameterReader { qmp })
}

/// Verify the GUI layout before observing it during a single guest step.
/// Guard and sample notification active/timer state within a shared deadline.
/// The guest runs during reads, so the result is not atomic.
pub fn notification_state_before_checked(
    qmp: &QmpClient,
    model: Model,
    deadline: std::time::Instant,
    check: &dyn Fn() -> Result<(), String>,
) -> Result<(u8, u16), String> {
    if model != Model::L6max {
        return Err("notification state requires L6max 1.10".into());
    }
    let read = |address, size| {
        check()?;
        let bytes = qmp
            .read_memory_before(0, address, size, deadline)
            .map_err(|e| e.to_string())?;
        check()?;
        Ok::<_, String>(bytes)
    };
    for (address, expected) in [
        (
            0x8000_64c0,
            "c548c9e75ca74bac0b93736a67e5fef8045ba54f50f1b82b6d2da3b561f3eab5",
        ),
        (
            0x8000_6660,
            "dc6b83b74e0c81e2d6c9dd2ceda85e7d2a907c7f0c3a6bb3742e81cb746ce139",
        ),
    ] {
        if format!("{:x}", Sha256::digest(read(address, 64)?)) != expected {
            return Err("unsupported notification routines".into());
        }
    }
    let bytes = read(0x8020_a2c0, 6)?;
    if bytes.len() != 6 {
        return Err("short notification state".into());
    }
    Ok((bytes[0], u16::from_le_bytes([bytes[4], bytes[5]])))
}

pub fn reader(qmp: &QmpClient, model: Model) -> Result<Reader<'_>, String> {
    verify(model, &mut |address, size| {
        qmp.read_memory(0, address, size)
            .map_err(|error| error.to_string())
    })?;
    Ok(Reader { qmp })
}

/// Take a one-shot snapshot, validating the firmware revision for this request.
pub fn snapshot(qmp: &QmpClient, model: Model, with_text: bool) -> Result<Snapshot, String> {
    reader(qmp, model)?.snapshot(with_text)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn overlay_read_rechecks_deadline_after_delayed_final_read() {
        let expired = std::cell::Cell::new(false);
        let read_started = std::cell::Cell::new(false);
        let check = || {
            if expired.get() {
                Err("local deadline".to_owned())
            } else {
                Ok(())
            }
        };
        let error = checked_overlay_read(&check, || {
            read_started.set(true);
            expired.set(true);
            Ok(0u8)
        })
        .unwrap_err();
        assert!(read_started.get());
        assert_eq!(error, "local deadline");
    }
    #[test]
    fn parameter_getter_rejects_a_mismatched_revision() {
        let error = verify_parameter_getter(&mut |address, size| {
            assert_eq!(address, 0x8000_b4a8);
            Ok(vec![0; size])
        })
        .unwrap_err();
        assert!(error.contains("getter revision"));
    }
    #[test]
    fn bounds_and_model_are_checked_before_indirect_reads() {
        assert!(checked_sdram(0x7fff_ffff, 4).is_err());
        assert!(checked_sdram(0x83ff_ffff, 4).is_err());
        assert!(checked_sdram(u32::MAX, 4).is_err());
        assert!(checked_sdram(0x8020_0000, 48).is_ok());
        assert!(verify(Model::L6, &mut |_, _| panic!("wrong model was read")).is_err());
        assert!(
            verify(Model::L6max, &mut |_, size| Ok(vec![0; size]))
                .unwrap_err()
                .contains("revision")
        );
    }
    #[test]
    fn snapshot_validates_text_pointers_and_decodes_utf16() {
        let read = |invalid_pointer: bool| {
            move |address: u32, size: usize| -> Result<Vec<u8>, String> {
                let word = if address == l6max::current_window {
                    Some(l6max::GUI_RECORDER_WINDOW)
                } else if address == l6max::gui_bitmap_table_entry_0 {
                    Some(0xff01_4b4e)
                } else if address == l6max::gui_display_callback_table + 20 {
                    Some(0x8000_0001)
                } else if address == l6max::gui_text_pointers_by_language {
                    Some(0x8020_0000)
                } else if address == 0x8020_0000 + TEXT_ID * 4 {
                    Some(if invalid_pointer { 0 } else { 0x8020_1000 })
                } else {
                    None
                };
                if let Some(word) = word {
                    return Ok(word.to_le_bytes().to_vec());
                }
                if address == l6max::mixer_channel_levels {
                    return Ok(vec![64]);
                }
                if address == 0x8020_1000 {
                    let mut bytes = vec![0; size];
                    for (index, unit) in "50%".encode_utf16().enumerate() {
                        bytes[index * 2..index * 2 + 2].copy_from_slice(&unit.to_le_bytes());
                    }
                    return Ok(bytes);
                }
                Err(format!("unexpected read {address:#x}"))
            }
        };
        let snapshot = read_snapshot(true, &mut read(false)).unwrap();
        assert_eq!(snapshot.text.as_deref(), Some("50%"));
        assert_eq!(snapshot.level, 64);
        assert_eq!(snapshot.state, [0x4e, 0x4b, 1, 0xff]);
        assert!(
            read_snapshot(true, &mut read(true))
                .unwrap_err()
                .contains("outside SDRAM")
        );
        assert_eq!(read_snapshot(false, &mut read(true)).unwrap().text, None);
    }
}

#[cfg(test)]
mod restoration_tests {
    use super::*;
    #[test]
    fn reads_navigation_fields_in_original_order() {
        let p = RestorationPointers {
            bitmap_descriptor: 0x100,
            text_table: 0x200,
            shared_resource: 0x300,
            lengths_table: 0x400,
            widget7: 0x500,
            widget33: 0x600,
            backgrounds: [0x700, 0x800, 0x900],
        };
        let expected = NavigationSnapshot {
            window: vec![1; 4],
            signature: vec![0; 2],
            bitmap: vec![0; 4],
            bitmap_descriptor: vec![0; 8],
            notification_semaphore: vec![0; 4],
            shared_contents: vec![0; 512],
            widget7: vec![0; 16],
            widget33: vec![0; 16],
            background10: vec![0; 1],
            background37: vec![0; 1],
            background48: vec![0; 1],
            callback: vec![0; 4],
        };
        let mut calls = Vec::new();
        let snapshot = read_navigation(&p, &expected, &mut |address, len| {
            calls.push((address, len));
            Ok(vec![0; len])
        })
        .unwrap();
        assert_eq!(
            calls,
            vec![
                (l6max::current_window, 4),
                (l6max::gui_bitmap_table_entry_0, 2),
                (l6max::gui_bitmap_table_entry_0 + 36 * 4, 4),
                (0x100, 8),
                (l6max::notification_slot, 4),
                (0x300, 512),
                (0x500, 16),
                (0x600, 16),
                (0x700, 1),
                (0x800, 1),
                (0x900, 1),
                (l6max::gui_display_callback_table + 20, 4)
            ]
        );
        assert_eq!(snapshot.window, vec![0; 4]);
        assert_eq!(snapshot.shared_contents, vec![0; 512]);
        assert_eq!(snapshot.callback, vec![0; 4]);
    }
    #[test]
    fn navigation_stops_reading_at_first_failed_check() {
        let p = RestorationPointers {
            bitmap_descriptor: 1,
            text_table: 2,
            shared_resource: 3,
            lengths_table: 4,
            widget7: 5,
            widget33: 6,
            backgrounds: [7, 8, 9],
        };
        let expected = NavigationSnapshot {
            window: vec![0; 4],
            ..Default::default()
        };
        let mut calls = Vec::new();
        let _ = read_navigation(&p, &expected, &mut |address, len| {
            calls.push((address, len));
            Ok(vec![0; len])
        })
        .unwrap();
        assert_eq!(calls, [(l6max::current_window, 4)]);
    }
    #[test]
    fn reads_all_baselines_in_original_order() {
        let p = RestorationPointers {
            bitmap_descriptor: 0x100,
            text_table: 0x200,
            shared_resource: 0x300,
            lengths_table: 0x400,
            widget7: 0x500,
            widget33: 0x600,
            backgrounds: [0x700, 0x800, 0x900],
        };
        let mut calls = Vec::new();
        let snapshot = read_restoration(&p, &mut |address, len| {
            calls.push((address, len));
            Ok(vec![calls.len() as u8; len])
        })
        .unwrap();
        assert_eq!(
            calls,
            vec![
                (l6max::current_window, 4),
                (l6max::gui_bitmap_table_entry_0, 2),
                (l6max::gui_bitmap_table_entry_0 + 36 * 4, 4),
                (0x100, 8),
                (l6max::notification_slot, 4),
                (0x200 + 229 * 4, 8),
                (0x200 + 179 * 4, 4),
                (0x300, 512),
                (0x400 + 229 * 2, 4),
                (0x500, 16),
                (0x600, 16),
                (0x700, 1),
                (0x800, 1),
                (0x900, 1),
                (l6max::gui_display_callback_table + 20, 4)
            ]
        );
        assert_eq!(snapshot.window, vec![1; 4]);
        assert_eq!(snapshot.shared_contents, vec![8; 512]);
        assert_eq!(snapshot.callback, vec![15; 4]);
    }
}

#[cfg(test)]
mod preflight_order_tests {
    use super::*;
    #[test]
    fn reads_state_before_callback_and_stops_on_error() {
        let mut reads = Vec::new();
        let result = read_preflight_state(&mut |address, len| {
            reads.push((address, len));
            Ok(vec![0; len])
        })
        .unwrap();
        assert_eq!(
            reads,
            [
                (l6max::gui_bitmap_table_entry_0, 4),
                (l6max::gui_display_callback_table + 20, 4)
            ]
        );
        assert_eq!(result, (vec![0; 4], vec![0; 4]));
        let mut calls = 0;
        assert!(
            read_preflight_state(&mut |_, _| {
                calls += 1;
                Err("unavailable".into())
            })
            .is_err()
        );
        assert_eq!(calls, 1);
    }
}

#[cfg(test)]
mod background_bitmap_tests {
    use super::*;
    #[test]
    fn reads_table_then_bitmap_id_and_rejects_short_pointer() {
        let mut reads = Vec::new();
        let id = read_background_bitmap_id(47, &mut |address, len| {
            reads.push((address, len));
            if len == 4 {
                Ok(0x8020_0100u32.to_le_bytes().to_vec())
            } else {
                Ok(0u16.to_le_bytes().to_vec())
            }
        })
        .unwrap();
        assert_eq!(id, 0);
        assert_eq!(
            reads,
            [(l6max::gui_background_table + 47 * 4, 4), (0x8020_0102, 2)]
        );
        let mut calls = 0;
        assert!(
            read_background_bitmap_id(47, &mut |_, _| {
                calls += 1;
                Ok(vec![0; 3])
            })
            .is_err()
        );
        assert_eq!(calls, 1);
        assert!(read_background_bitmap_id(48, &mut |_, _| panic!("invalid index read")).is_err());
        for pointer in [0u32, SDRAM_END - 3, u32::MAX] {
            let mut calls = 0;
            assert!(
                read_background_bitmap_id(47, &mut |_, _| {
                    calls += 1;
                    Ok(pointer.to_le_bytes().to_vec())
                })
                .is_err()
            );
            assert_eq!(calls, 1);
        }
    }
}
