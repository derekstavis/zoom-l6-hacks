//! Read-only observations of L6 1.10 saved sound-pad assignments.
use crate::qmp::QmpClient;
use firmware_api::l6;
use l6fw::Model;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::time::Instant;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct Assignment {
    pub revision_guard: String,
    pub atomic: bool,
    /// Zero-based pad index.
    pub pad: u8,
    pub pending: u8,
    /// Whether pad 1's raw UTF-16 path matches the requested path, if supplied.
    pub path_matches: Option<bool>,
}

pub struct Reader {
    qmp: QmpClient,
}
impl Reader {
    pub fn new_before(qmp: &QmpClient, model: Model, deadline: Instant) -> Result<Self, String> {
        if model != Model::L6 {
            return Err("saved sound-pad observation requires L6 1.10".into());
        }
        let code = qmp
            .read_memory_before(0, l6::sound_pad_assignment_worker, 64, deadline)
            .map_err(|e| e.to_string())?;
        if format!("{:x}", Sha256::digest(&code))
            != "e660c7c874ee434bce68a34a76fddf888b5751d7e9ee5a76d33a3d7b86ee8ba9"
        {
            return Err("unsupported L6 sound-pad assignment worker revision".into());
        }
        Ok(Self { qmp: qmp.clone() })
    }
    pub fn sample_before(
        &self,
        pad: u8,
        path: Option<&str>,
        deadline: Instant,
    ) -> Result<Assignment, String> {
        sample(pad, path, |address, len| {
            self.qmp
                .read_memory_before(0, address, len, deadline)
                .map_err(|e| e.to_string())
        })
    }
}
fn sample(
    pad: u8,
    path: Option<&str>,
    mut read: impl FnMut(u32, usize) -> Result<Vec<u8>, String>,
) -> Result<Assignment, String> {
    if pad > 3 {
        return Err("sound pad must be 0..3".into());
    }
    if path.is_some() && pad != 0 {
        return Err("path observation is only available for pad 1".into());
    }
    let path_matches = if let Some(path) = path {
        let expected: Vec<u8> = format!("A:\\{}", path.replace('/', "\\"))
            .encode_utf16()
            .chain(std::iter::once(0))
            .flat_map(u16::to_le_bytes)
            .collect();
        if expected.len() > 1024 {
            return Err("sound-pad path is too long".into());
        }
        Some(read(l6::sound_pad1_path, expected.len())? == expected)
    } else {
        None
    };
    let raw = read(l6::sound_pad_assignments + u32::from(pad), 1)?;
    let pending = *raw
        .first()
        .filter(|_| raw.len() == 1)
        .ok_or("short sound-pad assignment read")?;
    Ok(Assignment {
        revision_guard: "l6_1_10_sound_pad_assignment_worker".into(),
        atomic: false,
        pad,
        pending,
        path_matches,
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    #[ignore = "requires supported unpacked MAIN in L6_SOUND_PAD_TEST_MAIN"]
    fn supported_worker_fingerprint() {
        let image = std::fs::read(std::env::var_os("L6_SOUND_PAD_TEST_MAIN").unwrap()).unwrap();
        let offset = (l6::sound_pad_assignment_worker - Model::L6.main_base()) as usize;
        assert_eq!(
            format!("{:x}", Sha256::digest(&image[offset..offset + 64])),
            "e660c7c874ee434bce68a34a76fddf888b5751d7e9ee5a76d33a3d7b86ee8ba9"
        );
    }
    #[test]
    fn reads_path_and_pending_and_checks_bounds() {
        let expected: Vec<u8> = "A:\\SOUND\\x.wav\0"
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect();
        let value = sample(0, Some("SOUND/x.wav"), |address, _| {
            Ok(if address == l6::sound_pad1_path {
                expected.clone()
            } else {
                vec![0]
            })
        })
        .unwrap();
        assert_eq!(value.path_matches, Some(true));
        assert_eq!(value.pending, 0);
        assert!(sample(4, None, |_, _| unreachable!()).is_err());
        assert!(sample(1, Some("x"), |_, _| unreachable!()).is_err());
        assert!(sample(0, None, |_, _| Ok(vec![])).is_err());
    }
}
