//! Firmware-guarded stopped-guest observations for L6max recorder playback.
use crate::qmp::QmpClient;
use firmware_api::l6max;
use l6fw::Model;
use sha2::{Digest, Sha256};
use std::time::Instant;

const GUARDS: [(u32, &str); 6] = [
    (
        l6max::play_event_submit,
        "88d47330e0a751a4e25e9a2ba4781d86eecd7491bd54be534b01e9e58997f9a9",
    ),
    (
        l6max::play_event_send_return,
        "6fa47e7da4896bb36a8b168935e86bdea25b888fa25b2c2aecef30e7e1da594f",
    ),
    (
        l6max::panel_handle_main_button_event,
        "242feeb12282a19debde6e6da7af05c5ccba64c9c15c41b2efd21215e3d7176c",
    ),
    (
        l6max::gui_callback_schedule,
        "267e6a94980ba6ba23101e48111c23a532f4dc96af824070fb50ec7b453fb762",
    ),
    (
        l6max::rec_play_action,
        "7788394265bab6af4a7cac723b1a4c33b73e0dbb926ab2c46751c42492a52b96",
    ),
    (
        l6max::rec_stream_read,
        "2468842acb4d59a0b92c06934b90067d8647b5b1fc6b6d6558dbac22bf8230e6",
    ),
];
fn span(address: u32, length: usize) -> Result<(), String> {
    let end = address
        .checked_add(u32::try_from(length).map_err(|_| "invalid length")?)
        .ok_or("address overflow")?;
    if length == 0
        || ![
            (0x2000_0000, 0x2020_0000),
            (0x2020_0000, 0x2030_0000),
            (0x8000_0000, 0x8020_0000),
            (0x8020_0000, 0x8420_0000),
        ]
        .iter()
        .any(|(a, b)| address >= *a && end <= *b)
    {
        return Err(format!("unmapped recorder span {address:#x}..{end:#x}"));
    }
    Ok(())
}
fn read(
    qmp: &QmpClient,
    address: u32,
    length: usize,
    deadline: Instant,
    cancelled: &mut impl FnMut() -> bool,
) -> Result<Vec<u8>, String> {
    if cancelled() || Instant::now() >= deadline {
        return Err("recorder observation deadline exceeded".into());
    }
    let bytes = qmp
        .read_memory_before(0, address, length, deadline)
        .map_err(|e| e.to_string())?;
    if cancelled() || Instant::now() >= deadline {
        return Err("recorder observation deadline exceeded".into());
    }
    if bytes.len() != length {
        return Err("short recorder read".into());
    }
    Ok(bytes)
}
fn guard(
    model: Model,
    mut get: impl FnMut(u32, usize) -> Result<Vec<u8>, String>,
) -> Result<(), String> {
    if model != Model::L6max {
        return Err("recorder reader requires L6max".into());
    }
    for (address, expected) in GUARDS {
        let bytes = get(address, 32)?;
        if bytes.len() != 32 || format!("{:x}", Sha256::digest(bytes)) != expected {
            return Err(format!("unsupported L6max recorder code at {address:#x}"));
        }
    }
    Ok(())
}
/// Bound to one guest boot; callers must keep MAIN stopped while sampling.
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
    ) -> Result<Vec<u32>, String> {
        span(pointer, 20)?;
        Ok(read(&self.qmp, pointer, 20, deadline, &mut cancelled)?
            .chunks_exact(4)
            .map(|b| u32::from_le_bytes(b.try_into().unwrap()))
            .collect())
    }
    pub fn callback_target(
        &self,
        deadline: Instant,
        mut cancelled: impl FnMut() -> bool,
    ) -> Result<u32, String> {
        let mut word = |address| -> Result<u32, String> {
            span(address, 4)?;
            Ok(u32::from_le_bytes(
                read(&self.qmp, address, 4, deadline, &mut cancelled)?
                    .try_into()
                    .unwrap(),
            ))
        };
        let window = word(l6max::current_window)?;
        let table = word(window.checked_add(24).ok_or("window pointer overflow")?)?;
        let target = word(table.checked_add(8).ok_or("callback table overflow")?)? & !1;
        if !(0x8000_0000..0x8100_0000).contains(&target) {
            return Err(format!("invalid callback target {target:#x}"));
        }
        Ok(target)
    }
    pub fn dma(
        &self,
        deadline: Instant,
        mut cancelled: impl FnMut() -> bool,
    ) -> Result<(u32, Vec<u32>), String> {
        let tcd = read(&self.qmp, 0x400e_91c0, 32, deadline, &mut cancelled)?;
        let source = u32::from_le_bytes(tcd[..4].try_into().unwrap());
        let words = if (0x2000_0000..0x2008_0000).contains(&source) {
            span(source, 16)?;
            read(&self.qmp, source, 16, deadline, &mut cancelled)?
                .chunks_exact(4)
                .map(|b| u32::from_le_bytes(b.try_into().unwrap()))
                .collect()
        } else {
            Vec::new()
        };
        Ok((source, words))
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn guard_rejects_wrong_model_code_and_short_reads() {
        assert!(guard(Model::L6, |_, _| unreachable!()).is_err());
        assert!(guard(Model::L6max, |_, _| Ok(vec![0; 32])).is_err());
        assert!(guard(Model::L6max, |_, _| Ok(vec![0; 31])).is_err());
    }
    #[test]
    #[ignore = "requires supported L6max MAIN in L6MAX_RECORDER_TEST_MAIN"]
    fn supported_firmware_matches_all_guards() {
        let bytes = std::fs::read(
            std::env::var_os("L6MAX_RECORDER_TEST_MAIN").expect("L6MAX_RECORDER_TEST_MAIN"),
        )
        .unwrap();
        guard(Model::L6max, |address, length| {
            let offset = (address - 0x80000000) as usize;
            bytes
                .get(offset..offset + length)
                .map(|s| s.to_vec())
                .ok_or("short image".into())
        })
        .unwrap();
    }
    #[test]
    fn spans_validate_full_mapping_and_wrap() {
        assert!(span(0x841ffff0, 16).is_ok());
        assert!(span(0x841ffff8, 16).is_err());
        assert!(span(u32::MAX - 2, 4).is_err());
        assert!(span(0x801ffff8, 16).is_err());
    }
}
