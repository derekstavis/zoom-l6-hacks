//! Bounded, read-only MAIN SDRAM observations.
use crate::qmp::QmpClient;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct MemoryRead {
    pub address: u32,
    pub bytes: Vec<u8>,
    pub atomic: bool,
}

fn validate(address: u32, length: usize) -> Result<(), String> {
    if length == 0 || length > 4096 {
        return Err("memory read length must be 1..4096".into());
    }
    let end = address
        .checked_add(length as u32)
        .ok_or("memory read address overflow")?;
    if address < 0x8000_0000 || end > 0x8400_0000 {
        return Err("memory read is outside MAIN SDRAM".into());
    }
    Ok(())
}

/// No model/revision interpretation or atomicity is implied.
pub fn read_main(qmp: &QmpClient, address: u32, length: usize) -> Result<MemoryRead, String> {
    validate(address, length)?;
    let bytes = qmp
        .read_memory(0, address, length)
        .map_err(|e| e.to_string())?;
    Ok(MemoryRead {
        address,
        bytes,
        atomic: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_zero_oversize_out_of_range_and_overflow() {
        assert!(validate(0x8000_0000, 1).is_ok());
        assert!(validate(0x83ff_ffff, 1).is_ok());
        for (address, len) in [
            (0x8000_0000, 0),
            (0x8000_0000, 4097),
            (0x7fff_ffff, 1),
            (0x83ff_ffff, 2),
            (u32::MAX, 2),
        ] {
            assert!(validate(address, len).is_err());
        }
    }
}
