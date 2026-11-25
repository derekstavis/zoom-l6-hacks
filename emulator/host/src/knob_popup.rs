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
        let table = read_word(read, l6max::gui_text_pointers_by_language)?;
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
        Some(String::from_utf16(&units).map_err(|error| error.to_string())?)
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

/// A revision-checked reader for one uninterrupted observation step.
pub struct Reader<'a> {
    qmp: &'a QmpClient,
}

impl Reader<'_> {
    /// Values can change between QMP reads; this is not an atomic snapshot.
    pub fn snapshot(&self, with_text: bool) -> Result<Snapshot, String> {
        read_snapshot(with_text, &mut |address, size| {
            self.qmp
                .read_memory(0, address, size)
                .map_err(|error| error.to_string())
        })
    }
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
