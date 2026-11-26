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

/// The same bounded read under a caller-owned wall deadline, including QMP lock contention.
pub fn read_main_before(
    qmp: &QmpClient,
    address: u32,
    length: usize,
    deadline: std::time::Instant,
) -> Result<MemoryRead, String> {
    validate(address, length)?;
    let bytes = qmp
        .read_memory_before(0, address, length, deadline)
        .map_err(|e| e.to_string())?;
    Ok(MemoryRead {
        address,
        bytes,
        atomic: false,
    })
}

/// Resolve a declared firmware API symbol for the selected model. This is a
/// catalog lookup, not proof that the running firmware matches that revision.
fn symbol_address(model: l6fw::Model, name: &str, length: usize) -> Result<u32, String> {
    if length == 0 || length > 4096 {
        return Err("symbol read length must be 1..4096".into());
    }
    let address = match model {
        l6fw::Model::L6 => firmware_api::l6::symbol(name),
        l6fw::Model::L6max => firmware_api::l6max::symbol(name),
    }
    .ok_or_else(|| format!("symbol {name} is not declared for {}", model.name()))?;
    address
        .checked_add(length as u32)
        .ok_or("symbol read address overflow")?;
    Ok(address)
}

pub fn read_symbol(
    qmp: &QmpClient,
    model: l6fw::Model,
    name: &str,
    length: usize,
) -> Result<MemoryRead, String> {
    let address = symbol_address(model, name, length)?;
    let bytes = qmp
        .read_memory(0, address, length)
        .map_err(|e| e.to_string())?;
    Ok(MemoryRead {
        address,
        bytes,
        atomic: false,
    })
}

pub fn read_symbol_before(
    qmp: &QmpClient,
    model: l6fw::Model,
    name: &str,
    length: usize,
    deadline: std::time::Instant,
) -> Result<MemoryRead, String> {
    let address = symbol_address(model, name, length)?;
    let bytes = qmp
        .read_memory_before(0, address, length, deadline)
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
    fn resolves_model_specific_symbols_outside_sdram() {
        assert_eq!(
            symbol_address(l6fw::Model::L6, "dsp_eq_coefficients", 12).unwrap(),
            firmware_api::l6::dsp_eq_coefficients
        );
        assert_eq!(
            symbol_address(l6fw::Model::L6max, "dsp_eq_coefficients", 12).unwrap(),
            firmware_api::l6max::dsp_eq_coefficients
        );
        assert!(symbol_address(l6fw::Model::L6, "not_a_symbol", 4).is_err());
        assert!(symbol_address(l6fw::Model::L6, "dsp_eq_coefficients", 0).is_err());
        assert!(symbol_address(l6fw::Model::L6, "dsp_eq_coefficients", 4097).is_err());
    }
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
