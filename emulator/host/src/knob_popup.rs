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

fn verify(
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

/// Revision-checked reader for one bounded overlay transaction.
/// Recreate after guest restart or firmware changes.
pub struct OverlayReader {
    qmp: QmpClient,
}
impl OverlayReader {
    fn borrowed(&self) -> Reader<'_> {
        Reader { qmp: &self.qmp }
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
