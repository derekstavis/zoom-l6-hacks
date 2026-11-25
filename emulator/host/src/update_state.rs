//! Live L6max 1.10 update state, guarded by startup and update routines.
use crate::qmp::QmpClient;
use firmware_api::l6max;
use l6fw::Model;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

// 64-byte fingerprints of startup_select_mode, update_check_file, and the
// SD status getters. These identify the inspected routines, not the entire
// firmware image; unrelated patches may still be present.
const GUARDS: [(u32, &str); 3] = [
    (
        0x8001_00f0,
        "cb56b6fe45481ef05872811822789f0c1ac8e085a4c231b4eb37d524706ca20f",
    ),
    (
        0x8000_a7a0,
        "fde1fe16572afb93998265b814074b201084be16c3c049a17a008f14091e57f8",
    ),
    (
        0x8000_ebb8,
        "5c4d43166c57f7d773aaca17cd97c86e2c262135fb69459bbd6003d41430c66a",
    ),
];

/// Raw values sampled separately; the guest may change them between reads.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateState {
    pub revision_guard: String,
    pub atomic: bool,
    pub mode: u32,
    pub flags: u8,
}

fn sample(
    model: Model,
    guards: &[(u32, &str)],
    mut read: impl FnMut(u32, usize) -> Result<Vec<u8>, String>,
) -> Result<UpdateState, String> {
    if model != Model::L6max {
        return Err("update state observation requires L6max 1.10".into());
    }
    for &(address, expected) in guards {
        if format!("{:x}", Sha256::digest(read(address, 64)?)) != expected {
            return Err(format!(
                "unsupported L6max update revision at {address:#010x}"
            ));
        }
    }
    let mode: [u8; 4] = read(l6max::system_mode, 4)?
        .try_into()
        .map_err(|_| "short update mode read")?;
    let flags = read(l6max::update_package_flags, 1)?;
    if flags.len() != 1 {
        return Err("short update flags read".into());
    }
    Ok(UpdateState {
        revision_guard: "l6max_1_10_update_routines".into(),
        atomic: false,
        mode: u32::from_le_bytes(mode),
        flags: flags[0],
    })
}

pub fn update_state(qmp: &QmpClient, model: Model) -> Result<UpdateState, String> {
    sample(model, &GUARDS, |address, len| {
        qmp.read_memory(0, address, len).map_err(|e| e.to_string())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn sample_keeps_raw_values_and_rejects_unsupported() {
        let hash = format!("{:x}", Sha256::digest([0; 64]));
        let guards = [(0x100, hash.as_str())];
        let read = |address, len| match address {
            0x100 => Ok(vec![0; len]),
            l6max::system_mode => Ok(0x12345678u32.to_le_bytes().to_vec()),
            l6max::update_package_flags => Ok(vec![0xa9]),
            _ => panic!("unexpected read"),
        };
        let state = sample(Model::L6max, &guards, read).unwrap();
        assert_eq!(
            (state.mode, state.flags, state.atomic),
            (0x12345678, 0xa9, false)
        );
        assert!(sample(Model::L6, &guards, read).is_err());
        assert!(sample(Model::L6max, &[(0x100, "wrong")], read).is_err());
        assert!(sample(Model::L6max, &[], |_, _| Ok(vec![])).is_err());
        assert!(
            sample(Model::L6max, &[], |address, _| Ok(
                if address == l6max::system_mode {
                    vec![1; 4]
                } else {
                    vec![]
                }
            ))
            .is_err()
        );
    }
    #[test]
    #[ignore = "requires unpacked L6max 1.10 MAIN in L6MAX_UPDATE_TEST_MAIN"]
    fn production_guards_match_main() {
        let image = std::fs::read(std::env::var_os("L6MAX_UPDATE_TEST_MAIN").unwrap()).unwrap();
        let state = sample(Model::L6max, &GUARDS, |address, len| {
            if address == l6max::system_mode {
                return Ok(1u32.to_le_bytes().to_vec());
            }
            if address == l6max::update_package_flags {
                return Ok(vec![0]);
            }
            let offset = (address - Model::L6max.main_base()) as usize;
            image
                .get(offset..offset + len)
                .map(|v| v.to_vec())
                .ok_or("guard outside MAIN".into())
        })
        .unwrap();
        assert_eq!((state.mode, state.flags), (1, 0));
    }
}
