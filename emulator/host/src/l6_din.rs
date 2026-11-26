//! Revision-guarded, read-only L6 DIN sweep observations.
use crate::qmp::QmpClient;
use firmware_api::l6;
use l6fw::Model;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::time::Instant;

pub const BREAKPOINTS: [u32; 6] = [
    l6::midi_in,
    l6::main_event_submit,
    l6::main_event_queue_reader,
    l6::mixer_setter,
    l6::mixer_din_setter_6,
    l6::mixer_set_eq,
];
const HASHES: [&str; 6] = [
    "bbee492892a848fa5d426ec1dc05f425f427bff5e2880f6d63a4967ebe17858c",
    "aeac5eb8ff0f16e2d3ecc5d5cc41af4bc3334d08940d5fdb58cb267babf2744b",
    "8770796cd16904c61d3bfa2c3d767d5f91c6988e79fb9d705121f6f1a1521e6a",
    "0f76b5a420aee00767f1b06932f8def6c27a57401ff564e9d1427a91ddc38490",
    "d3ad8fd90e0be9dad58062764ff71b1fa57687853e9ea8cc0af79f73d8ae3ef9",
    "9cdb0f21e8f3a957d07e76e900b56439bb343b29907a9fcc4eab4a9546a4c8d7",
];

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Snapshot {
    pub revision_guard: String,
    pub atomic: bool,
    pub queue_pending: u32,
    pub ring_state: Vec<u8>,
    pub ring: Vec<u8>,
    pub mixer: Vec<u8>,
    pub dispatch_map: Vec<u8>,
}

pub struct Reader {
    qmp: QmpClient,
}
impl Reader {
    pub fn new_before_checked(
        qmp: &QmpClient,
        model: Model,
        deadline: Instant,
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<Self, String> {
        if model != Model::L6 {
            return Err("DIN observation requires L6 1.10".into());
        }
        let reader = Self { qmp: qmp.clone() };
        reader.verify_before_checked(deadline, check)?;
        Ok(reader)
    }
    pub fn verify_before_checked(
        &self,
        deadline: Instant,
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<(), String> {
        for (address, expected) in BREAKPOINTS.into_iter().zip(HASHES) {
            check()?;
            let bytes = self
                .qmp
                .read_memory_before(0, address, 64, deadline)
                .map_err(|e| e.to_string())?;
            check()?;
            if format!("{:x}", Sha256::digest(bytes)) != expected {
                return Err(format!("unsupported L6 DIN routine at {address:#010x}"));
            }
        }
        Ok(())
    }
    fn read(
        &self,
        address: u32,
        len: usize,
        deadline: Instant,
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<Vec<u8>, String> {
        check()?;
        let bytes = self
            .qmp
            .read_memory_before(0, address, len, deadline)
            .map_err(|e| e.to_string())?;
        check()?;
        if bytes.len() != len {
            return Err("short L6 DIN observation".into());
        }
        Ok(bytes)
    }
    pub fn queue(
        &self,
        deadline: Instant,
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<u32, String> {
        Ok(u32::from_le_bytes(
            self.read(l6::main_event_queue_pending, 4, deadline, check)?
                .try_into()
                .unwrap(),
        ))
    }
    pub fn mixer(
        &self,
        deadline: Instant,
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<Vec<u8>, String> {
        let mut result = self.read(l6::mixer_state, 0x7e, deadline, check)?;
        result.extend(self.read(l6::mixer_state_tail, 6, deadline, check)?);
        Ok(result)
    }
    pub fn ring_state(
        &self,
        deadline: Instant,
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<Vec<u8>, String> {
        self.read(l6::midi_din_ring_state, 0x20, deadline, check)
    }
    pub fn ring(
        &self,
        deadline: Instant,
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<Vec<u8>, String> {
        self.read(l6::midi_din_ring, 384, deadline, check)
    }
    pub fn map(
        &self,
        deadline: Instant,
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<Vec<u8>, String> {
        self.read(l6::midi_cc_dispatch_map, 128, deadline, check)
    }
    pub fn snapshot(
        &self,
        deadline: Instant,
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<Snapshot, String> {
        Ok(Snapshot {
            revision_guard: "l6_1_10_din_dispatch_sites".into(),
            atomic: false,
            queue_pending: self.queue(deadline, check)?,
            ring_state: self.ring_state(deadline, check)?,
            ring: self.ring(deadline, check)?,
            mixer: self.mixer(deadline, check)?,
            dispatch_map: self.map(deadline, check)?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    #[ignore = "requires L6_DIN_STOCK_MAIN"]
    fn supported_stock_matches_all_dispatch_guards() {
        let image = std::fs::read(std::env::var("L6_DIN_STOCK_MAIN").unwrap()).unwrap();
        assert_eq!(
            format!("{:x}", Sha256::digest(&image)),
            Model::L6.stock_main_sha256()
        );
        for (address, expected) in BREAKPOINTS.into_iter().zip(HASHES) {
            let offset = (address - Model::L6.main_base()) as usize;
            assert_eq!(
                format!("{:x}", Sha256::digest(&image[offset..offset + 64])),
                expected
            );
        }
    }
}
