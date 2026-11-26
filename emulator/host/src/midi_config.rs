//! Live L6 1.10 MIDI configuration, guarded by stock MIDI routines.
use crate::qmp::QmpClient;
use firmware_api::l6;
use l6fw::Model;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const GUARDS: [(u32, &str); 2] = [
    (
        l6::midi_channel,
        "6f9448a58dc770e74e085a0972a4a7252cd9b0d6c2360ba868b2e6364a1678b5",
    ),
    (
        l6::midi_send_mapped_cc,
        "90da986822a1402afa10333215ce410fdb0fa1ad8d27fb698e3338ac3e791b18",
    ),
];

/// A non-atomic sample: channel and mapping are separate guest reads.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Config {
    pub revision_guard: String,
    pub atomic: bool,
    pub channel: u8,
    /// 54 raw mapping entries in firmware order; 0xff means unmapped.
    pub cc_map: Vec<u8>,
}

fn sample(
    model: Model,
    guards: &[(u32, &str)],
    mut read: impl FnMut(u32, usize) -> Result<Vec<u8>, String>,
) -> Result<Config, String> {
    if model != Model::L6 {
        return Err("MIDI configuration observation requires L6 1.10".into());
    }
    for &(address, expected) in guards {
        if format!("{:x}", Sha256::digest(read(address, 64)?)) != expected {
            return Err(format!("unsupported L6 MIDI revision at {address:#010x}"));
        }
    }
    let channel = read(l6::midi_configured_channel, 1)?;
    let channel = *channel
        .first()
        .filter(|_| channel.len() == 1)
        .ok_or("short MIDI channel read")?;
    if channel > 15 {
        return Err(format!("invalid configured L6 MIDI channel {channel}"));
    }
    let cc_map = read(l6::midi_cc_map, 54)?;
    if cc_map.len() != 54 {
        return Err("short MIDI CC map read".into());
    }
    Ok(Config {
        revision_guard: "l6_1_10_midi_routines".into(),
        atomic: false,
        channel,
        cc_map,
    })
}

pub fn config(qmp: &QmpClient, model: Model) -> Result<Config, String> {
    sample(model, &GUARDS, |address, len| {
        qmp.read_memory(0, address, len)
            .map_err(|error| error.to_string())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guarded_sample_preserves_mapping_duplicates_and_unmapped() {
        let zero_hash = format!("{:x}", Sha256::digest([0; 64]));
        let guards = [(0x100, zero_hash.as_str())];
        let mut map = vec![0xff; 54];
        map[0] = 83;
        map[1] = 83;
        let result = sample(Model::L6, &guards, |address, len| match address {
            0x100 => Ok(vec![0; len]),
            l6::midi_configured_channel => Ok(vec![7]),
            l6::midi_cc_map => Ok(map.clone()),
            _ => panic!("unexpected read"),
        })
        .unwrap();
        assert_eq!(result.channel, 7);
        assert_eq!(result.cc_map, map);
        assert!(!result.atomic);
    }

    #[test]
    fn rejects_wrong_model_revision_channel_and_short_map() {
        assert!(sample(Model::L6max, &[], |_, _| panic!("wrong model read")).is_err());
        assert!(sample(Model::L6, &[(0x100, "bad")], |_, len| Ok(vec![0; len])).is_err());
        assert!(
            sample(Model::L6, &[], |_, _| Ok(vec![16]))
                .unwrap_err()
                .contains("channel")
        );
        assert!(
            sample(Model::L6, &[], |address, _| Ok(
                if address == l6::midi_configured_channel {
                    vec![1]
                } else {
                    vec![0; 53]
                }
            ))
            .unwrap_err()
            .contains("map")
        );
    }

    #[test]
    #[ignore = "requires an unpacked L6 1.10 MAIN in L6_MIDI_TEST_MAIN"]
    fn compatible_firmware_passes_production_guards() {
        let path = std::env::var_os("L6_MIDI_TEST_MAIN").expect("L6_MIDI_TEST_MAIN");
        let image = std::fs::read(path).unwrap();
        let result = sample(Model::L6, &GUARDS, |address, len| {
            if address == l6::midi_configured_channel {
                return Ok(vec![1]);
            }
            if address == l6::midi_cc_map {
                return Ok(vec![0xff; 54]);
            }
            let offset = (address - Model::L6.main_base()) as usize;
            Ok(image[offset..offset + len].to_vec())
        })
        .unwrap();
        assert_eq!(result.channel, 1);
        assert_eq!(result.cc_map, vec![0xff; 54]);
    }
}
