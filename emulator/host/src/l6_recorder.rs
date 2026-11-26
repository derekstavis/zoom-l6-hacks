//! L6 1.10 recorder observations for bounded, stopped-guest diagnostics.
use crate::qmp::QmpClient;
use l6fw::Model;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::time::Instant;

const GUARDS: [(u32, &str); 3] = [
    (
        firmware_api::l6::system_event_dispatcher,
        "f1a8476c264ac8f3ffcbb07578ab238a9a4d04f35f8dcaedc5d56e24089f809f",
    ),
    (
        firmware_api::l6::rec_play_handler,
        "d548708cae77f27ea25fd85bb9ffca19f91dbac09d287a3ba4caa19825a28d03",
    ),
    (
        firmware_api::l6::rec_playback_callback,
        "9ba1dbc15b17a700faa56aefe56d5388c2fb17e8b4b53aece495d4d7347a585f",
    ),
];
const TX_TCD: u32 = 0x400e_91c0;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Event {
    pub pointer: u32,
    pub bytes: Vec<u8>,
    pub state: u32,
    pub class: u32,
    pub code: u32,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Selection {
    pub mode: u32,
    pub key: Vec<u8>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Dma {
    pub source: u32,
    pub source_words: Vec<u32>,
    pub source_has_audio: bool,
}

fn checked_ram(address: u32, length: usize) -> Result<(), String> {
    let end = address
        .checked_add(u32::try_from(length).map_err(|_| "invalid RAM length")?)
        .ok_or("RAM span overflow")?;
    if length == 0
        || ![
            (0x2000_0000, 0x2020_0000),
            (0x2020_0000, 0x2030_0000),
            (0x801f_0000, 0x8020_0000),
            (0x8020_0000, 0x8420_0000),
        ]
        .iter()
        .any(|(start, stop)| address >= *start && end <= *stop)
    {
        return Err(format!(
            "recorder pointer span {address:#010x}..{end:#010x} is outside emulated MAIN RAM"
        ));
    }
    Ok(())
}
fn guard(
    model: Model,
    mut read: impl FnMut(u32, usize) -> Result<Vec<u8>, String>,
) -> Result<(), String> {
    if model != Model::L6 {
        return Err("recorder observation requires L6".into());
    }
    for (address, expected) in GUARDS {
        let bytes = read(address, 32)?;
        if bytes.len() != 32 || format!("{:x}", Sha256::digest(&bytes)) != expected {
            return Err(format!(
                "unsupported L6 recorder revision at {address:#010x}"
            ));
        }
    }
    Ok(())
}
/// Revision-checked reader for one stopped-guest transaction. Recreate after restart.
pub struct Reader {
    qmp: QmpClient,
}
impl Reader {
    pub fn new(
        qmp: &QmpClient,
        model: Model,
        deadline: Instant,
        mut cancelled: impl FnMut() -> bool,
    ) -> Result<Self, String> {
        guard(model, |address, length| {
            read(qmp, address, length, deadline, &mut cancelled)
        })?;
        Ok(Self { qmp: qmp.clone() })
    }
    pub fn event(
        &self,
        pointer: u32,
        deadline: Instant,
        mut cancelled: impl FnMut() -> bool,
    ) -> Result<Event, String> {
        checked_ram(pointer, 20)?;
        let bytes = read(&self.qmp, pointer, 20, deadline, &mut cancelled)?;
        let word = |i: usize| u32::from_le_bytes(bytes[i..i + 4].try_into().unwrap());
        let (state, class, code) = (word(0), word(4), word(8));
        Ok(Event {
            pointer,
            bytes,
            state,
            class,
            code,
        })
    }
    pub fn selection(
        &self,
        deadline: Instant,
        mut cancelled: impl FnMut() -> bool,
    ) -> Result<Selection, String> {
        let bytes = read(
            &self.qmp,
            firmware_api::l6::rec_mode,
            4,
            deadline,
            &mut cancelled,
        )?;
        let mode = u32::from_le_bytes(bytes.try_into().map_err(|_| "short recorder mode")?);
        let key = read(
            &self.qmp,
            firmware_api::l6::rec_selected_key,
            8,
            deadline,
            &mut cancelled,
        )?;
        Ok(Selection { mode, key })
    }
    pub fn dma(
        &self,
        deadline: Instant,
        mut cancelled: impl FnMut() -> bool,
    ) -> Result<Dma, String> {
        let tcd = read(&self.qmp, TX_TCD, 32, deadline, &mut cancelled)?;
        let source = u32::from_le_bytes(tcd[..4].try_into().unwrap());
        checked_ram(source, 16)?;
        let data = read(&self.qmp, source, 16, deadline, &mut cancelled)?;
        let source_words = data
            .chunks_exact(4)
            .map(|word| u32::from_le_bytes(word.try_into().unwrap()))
            .collect::<Vec<_>>();
        let source_has_audio = source_words
            .iter()
            .any(|word| *word != 0 && *word != u32::MAX);
        Ok(Dma {
            source,
            source_words,
            source_has_audio,
        })
    }
}
fn read(
    qmp: &QmpClient,
    address: u32,
    length: usize,
    deadline: Instant,
    cancelled: &mut impl FnMut() -> bool,
) -> Result<Vec<u8>, String> {
    if cancelled() || Instant::now() >= deadline {
        return Err("recorder observation cancelled or deadline exceeded".into());
    }
    let bytes = qmp
        .read_memory_before(0, address, length, deadline)
        .map_err(|e| e.to_string())?;
    if cancelled() || Instant::now() >= deadline {
        return Err("recorder observation cancelled or deadline exceeded".into());
    }
    if bytes.len() != length {
        return Err(format!("short recorder read at {address:#010x}"));
    }
    Ok(bytes)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pointer_bounds_match_separate_emulated_ram_regions() {
        for address in [
            0x2000_d4f0,
            0x2020_0000,
            0x801f_0000,
            0x8090_725c,
            0x841f_fff0,
        ] {
            assert!(checked_ram(address, 16).is_ok());
        }
        for address in [
            0x1fff_ffff,
            0x201f_fff8,
            0x202f_fff8,
            0x801e_fff8,
            0x801f_fff8,
            0x841f_fff8,
            0xffff_fff8,
            TX_TCD,
        ] {
            assert!(checked_ram(address, 16).is_err());
        }
    }
    #[test]
    fn guard_rejects_wrong_model_and_code() {
        assert!(guard(Model::L6max, |_, _| Ok(vec![0; 32])).is_err());
        assert!(guard(Model::L6, |_, _| Ok(vec![0; 32])).is_err());
    }
}
