//! Read-only L6max 1.10 sound-pad key lookup and state.
use crate::qmp::QmpClient;
use firmware_api::l6max;
use l6fw::Model;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct Snapshot {
    pub revision_guard: String,
    pub atomic: bool,
    pub key_code: u32,
    pub bit: u32,
    pub word_index: u32,
    pub words: [u32; 6],
}

fn sample(
    code: u32,
    mut read: impl FnMut(u32, usize) -> Result<Vec<u8>, String>,
) -> Result<Snapshot, String> {
    if !(0x19..=0x1c).contains(&code) {
        return Err("sound-pad key code must be 25..28".into());
    }
    let lookup = read(l6max::PANEL_KEY_TABLE + code * 8, 8)?;
    if lookup.len() != 8 {
        return Err("short panel key lookup".into());
    }
    let bit = u32::from_le_bytes(lookup[..4].try_into().unwrap());
    let word_index = u32::from_le_bytes(lookup[4..].try_into().unwrap());
    let raw = read(l6max::panel_key_state, 24)?;
    if raw.len() != 24 {
        return Err("short panel key state".into());
    }
    let words =
        std::array::from_fn(|i| u32::from_le_bytes(raw[i * 4..i * 4 + 4].try_into().unwrap()));
    Ok(Snapshot {
        revision_guard: "l6max_1_10_panel_handler_and_gui".into(),
        atomic: false,
        key_code: code,
        bit,
        word_index,
        words,
    })
}

fn guard(
    model: Model,
    mut read: impl FnMut(u32, usize) -> Result<Vec<u8>, String>,
) -> Result<(), String> {
    if model != Model::L6max {
        return Err("panel key observation requires L6max 1.10".into());
    }
    let routine = read(l6max::panel_handle_main_button_event, 64)?;
    if format!("{:x}", Sha256::digest(&routine))
        != "d96ad2d649ffc331d044f809b75841b792f5e0ac915722216fcaed88c820509a"
    {
        return Err("unsupported L6max panel handler revision".into());
    }
    Ok(())
}

pub fn snapshot(qmp: &QmpClient, model: Model, code: u32) -> Result<Snapshot, String> {
    // Combine the panel handler and L6max 1.10 GUI-routine fingerprints.
    guard(model, |address, length| {
        qmp.read_memory(0, address, length)
            .map_err(|e| e.to_string())
    })?;
    let _guard = crate::knob_popup::reader(qmp, model)?;
    sample(code, |address, length| {
        qmp.read_memory(0, address, length)
            .map_err(|e| e.to_string())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_wrong_model_and_revision() {
        assert!(guard(Model::L6, |_, _| unreachable!()).is_err());
        assert!(guard(Model::L6max, |_, n| Ok(vec![0; n])).is_err());
    }
    #[test]
    #[ignore = "requires unpacked supported stock MAIN in L6MAX_PANEL_TEST_MAIN"]
    fn supported_panel_handler_matches_guard() {
        let bytes = std::fs::read(
            std::env::var_os("L6MAX_PANEL_TEST_MAIN").expect("L6MAX_PANEL_TEST_MAIN"),
        )
        .unwrap();
        guard(Model::L6max, |address, length| {
            let offset = (address - 0x8000_0000) as usize;
            bytes
                .get(offset..offset + length)
                .map(|v| v.to_vec())
                .ok_or("short image".into())
        })
        .unwrap();
    }
    #[test]
    fn preserves_raw_values_and_bounds() {
        let sample = sample(25, |address, len| {
            Ok(if address == l6max::PANEL_KEY_TABLE + 25 * 8 {
                [1u32.to_le_bytes(), 4u32.to_le_bytes()].concat()
            } else {
                vec![0; len]
            })
        })
        .unwrap();
        assert_eq!((sample.bit, sample.word_index, sample.words[4]), (1, 4, 0));
        assert!(super::sample(24, |_, _| unreachable!()).is_err());
        assert!(super::sample(25, |_, _| Ok(vec![])).is_err());
    }
}
