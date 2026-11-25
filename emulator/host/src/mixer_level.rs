//! Read-only first-channel mixer level, guarded by the complete 1.10 getter.
use crate::qmp::QmpClient;
use l6fw::Model;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::time::Instant;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MixerLevel {
    pub model: Model,
    pub channel: u8,
    pub value: u8,
    /// This fingerprints the getter, not the whole firmware image.
    pub getter_guard: String,
    pub atomic: bool,
}

fn sample(
    model: Model,
    mut read: impl FnMut(u32, usize) -> Result<Vec<u8>, String>,
) -> Result<MixerLevel, String> {
    let (getter, expected, level) = match model {
        Model::L6 => (
            0x80005f40,
            "079bbd9dcadaa7005e2e868d6e5d1e749aac1ca6605057c1c193d5ac609f7513",
            firmware_api::l6::mixer_state_tail,
        ),
        Model::L6max => (
            0x8000b0b0,
            "bcfe88b2415ffbe2fbd450a7cb5c75d2ea92b04d803399e3fc0533363cb30929",
            firmware_api::l6max::mixer_channel_levels,
        ),
    };
    let bytes = read(getter, 16)?;
    if bytes.len() != 16 || format!("{:x}", Sha256::digest(&bytes)) != expected {
        return Err(format!(
            "unsupported {} mixer getter at {getter:#010x}",
            model.name()
        ));
    }
    let bytes = read(level, 1)?;
    let [value] = bytes.as_slice() else {
        return Err("short mixer level read".into());
    };
    Ok(MixerLevel {
        model,
        channel: 1,
        value: *value,
        getter_guard: format!("{}_1_10_mixer_getter", model.name()),
        atomic: false,
    })
}

pub fn first_channel(qmp: &QmpClient, model: Model) -> Result<MixerLevel, String> {
    sample(model, |address, len| {
        qmp.read_memory(0, address, len).map_err(|e| e.to_string())
    })
}

pub fn first_channel_before(
    qmp: &QmpClient,
    model: Model,
    deadline: Instant,
) -> Result<MixerLevel, String> {
    first_channel_before_cancellable(qmp, model, deadline, || false)
}

pub fn first_channel_before_cancellable(
    qmp: &QmpClient,
    model: Model,
    deadline: Instant,
    cancelled: impl Fn() -> bool,
) -> Result<MixerLevel, String> {
    sample_cancellable(
        model,
        |address, len| {
            qmp.read_memory_before(0, address, len, deadline)
                .map_err(|e| e.to_string())
        },
        cancelled,
    )
}

fn sample_cancellable(
    model: Model,
    mut read: impl FnMut(u32, usize) -> Result<Vec<u8>, String>,
    cancelled: impl Fn() -> bool,
) -> Result<MixerLevel, String> {
    sample(model, |address, len| {
        if cancelled() {
            return Err("mixer level deadline expired".into());
        }
        let bytes = read(address, len)?;
        if cancelled() {
            return Err("mixer level deadline expired".into());
        }
        Ok(bytes)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn checks_model_getter_and_reads_correct_level() {
        for (model, getter, level, code) in [
            (
                Model::L6,
                0x80005f40,
                0x80440e9a,
                [
                    0x40, 0xf6, 0xc8, 0x51, 0xc8, 0xf2, 0x44, 0x01, 0x08, 0x44, 0x90, 0xf9, 0xd2,
                    0x00, 0x70, 0x47,
                ],
            ),
            (
                Model::L6max,
                0x8000b0b0,
                0x804600a4,
                [
                    0x4f, 0xf6, 0x84, 0x71, 0xc8, 0xf2, 0x45, 0x01, 0x08, 0x44, 0x90, 0xf9, 0x20,
                    0x01, 0x70, 0x47,
                ],
            ),
        ] {
            let read = |address, len| {
                if address == getter && len == 16 {
                    Ok(code.to_vec())
                } else if address == level && len == 1 {
                    Ok(vec![32])
                } else {
                    Err("unexpected read".into())
                }
            };
            let observed = sample(model, read).unwrap();
            assert_eq!(observed.value, 32);
            assert_eq!(observed.channel, 1);
            assert!(!observed.atomic);
            assert!(sample(model, |_, len| Ok(vec![0; len])).is_err());
        }
    }

    #[test]
    fn guest_expiry_during_getter_prevents_the_level_read() {
        use std::cell::Cell;
        let expired = Cell::new(false);
        let reads = Cell::new(0);
        let result = sample_cancellable(
            Model::L6,
            |_, _| {
                reads.set(reads.get() + 1);
                expired.set(true);
                Ok(vec![0; 16])
            },
            || expired.get(),
        );
        assert!(result.unwrap_err().contains("deadline"));
        assert_eq!(reads.get(), 1);
    }

    #[test]
    #[ignore = "requires L6_MIXER_TEST_MAIN and L6MAX_MIXER_TEST_MAIN stock 1.10 files"]
    fn production_getters_match_both_stock_images() {
        for (model, variable) in [
            (Model::L6, "L6_MIXER_TEST_MAIN"),
            (Model::L6max, "L6MAX_MIXER_TEST_MAIN"),
        ] {
            let image = std::fs::read(std::env::var_os(variable).unwrap()).unwrap();
            let base = model.main_base();
            let result = sample(model, |address, len| {
                if len == 1 {
                    return Ok(vec![32]);
                }
                let start = (address - base) as usize;
                image
                    .get(start..start + len)
                    .map(|slice| slice.to_vec())
                    .ok_or_else(|| "getter outside image".into())
            })
            .unwrap();
            assert_eq!(result.value, 32);
        }
    }
}
