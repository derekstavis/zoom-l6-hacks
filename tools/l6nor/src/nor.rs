//! The MAIN MCU's 2 MiB serial NOR, as persisted in `main-nor.bin`.

use l6fw::Model;
use std::{fs, path::Path};

pub const MAIN_NOR_SIZE: usize = 0x200000;
pub const RESOURCE_NOR_SIZE: usize = 0x400000;
pub const MAIN_IMAGE: usize = 0x50000;
pub const MAIN_IMAGE_CAPACITY: usize = 0x1a2ff8;
pub const MAIN_TRAILER: usize = 0x1f2ff8;
pub const PANEL_IMAGE: usize = 0x1f3000;
pub const PANEL_IMAGE_CAPACITY: usize = 0x3ff8;
pub const PANEL_TRAILER: usize = 0x1f6ff8;
pub const PANEL_REGION_END: usize = 0x1f7000;
pub const BOOT_VERSION: usize = 0x4fffc;
pub const SYSTEM_VERSION: usize = 0x1f7ffc;
pub const UPDATE_MARKER: usize = 0x1ff000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UpdateMarker {
    None,
    Pending,
    Failed,
}

#[derive(Clone, Debug)]
pub struct Versions {
    pub boot: Option<String>,
    pub system: Option<String>,
    pub packaged_main: Option<String>,
    pub packaged_panel: Option<String>,
}

#[derive(Clone)]
pub struct MainNor {
    bytes: Vec<u8>,
}

fn read_segment(directory: &Path, name: &str, capacity: usize) -> Result<Vec<u8>, String> {
    let path = directory.join(name);
    let bytes = fs::read(&path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    if bytes.len() > capacity {
        return Err(format!("{} exceeds its NOR region", path.display()));
    }
    Ok(bytes)
}

impl MainNor {
    pub fn from_bytes(bytes: Vec<u8>) -> Result<Self, String> {
        if bytes.len() != MAIN_NOR_SIZE {
            return Err(format!(
                "MAIN NOR must contain {MAIN_NOR_SIZE} bytes, found {}",
                bytes.len()
            ));
        }
        Ok(Self { bytes })
    }

    pub fn read(path: &Path) -> Result<Self, String> {
        Self::from_bytes(
            fs::read(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?,
        )
    }

    /// Seed a device from an unpacked firmware directory exactly as QEMU's
    /// first persistent boot does: erased flash, MAIN image and trailer,
    /// packaged panel image and trailer, and model-specific installed footer.
    pub fn seed(firmware_dir: &Path) -> Result<Self, String> {
        let mut bytes = vec![0xff; MAIN_NOR_SIZE];
        let segments = [
            ("main_firmware.bin", MAIN_IMAGE, MAIN_IMAGE_CAPACITY),
            ("main_trailer.bin", MAIN_TRAILER, 8),
            ("secondary_firmware.bin", PANEL_IMAGE, PANEL_IMAGE_CAPACITY),
            ("secondary_trailer.bin", PANEL_TRAILER, 8),
        ];
        let mut main_length = 0;
        for (name, offset, capacity) in segments {
            let segment = read_segment(firmware_dir, name, capacity)?;
            if name == "main_firmware.bin" {
                main_length = segment.len();
            }
            bytes[offset..offset + segment.len()].copy_from_slice(&segment);
        }
        if l6fw::model_from_main_nor(&bytes)? == Model::L6max {
            let footer = read_segment(firmware_dir, "package_footer.bin", 0x40)?;
            if footer.len() != 0x40 {
                return Err("L6max package footer must contain 64 bytes".into());
            }
            let length = u32::from_le_bytes(footer[0x38..0x3c].try_into().unwrap()) as usize;
            if length < 8 || length > MAIN_IMAGE_CAPACITY || length != main_length {
                return Err("L6max package footer has an invalid MAIN length".into());
            }
            bytes[0x1f7fc0..0x1f8000].copy_from_slice(&footer);
        } else {
            bytes.copy_within(MAIN_TRAILER + 4..MAIN_TRAILER + 8, SYSTEM_VERSION);
        }
        let nor = Self { bytes };
        nor.model()?;
        Ok(nor)
    }

    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    pub fn bytes_mut(&mut self) -> &mut [u8] {
        &mut self.bytes
    }

    pub fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }

    pub fn model(&self) -> Result<Model, String> {
        l6fw::model_from_main_nor(&self.bytes)
    }

    /// The installed MAIN region, starting at the image's load base.
    pub fn main_region(&self) -> &[u8] {
        &self.bytes[MAIN_IMAGE..MAIN_TRAILER]
    }

    /// The packaged panel application and its trailer as programmed into
    /// panel flash `[0, 0x4000)`.
    pub fn panel_region(&self) -> &[u8] {
        &self.bytes[PANEL_IMAGE..PANEL_REGION_END]
    }

    pub fn versions(&self) -> Versions {
        let record = |offset: usize| {
            let bytes = &self.bytes[offset..offset + 4];
            bytes
                .iter()
                .all(u8::is_ascii_digit)
                .then(|| String::from_utf8_lossy(bytes).into_owned())
        };
        Versions {
            boot: record(BOOT_VERSION),
            system: record(SYSTEM_VERSION),
            packaged_main: record(MAIN_TRAILER + 4),
            packaged_panel: record(PANEL_TRAILER + 4),
        }
    }

    pub fn update_marker(&self) -> UpdateMarker {
        let marker = &self.bytes[UPDATE_MARKER..];
        if &marker[..8] != b"FW UPDT\0" {
            UpdateMarker::None
        } else if &marker[8..20] == b"FW UPDT ERR\0" {
            UpdateMarker::Failed
        } else {
            UpdateMarker::Pending
        }
    }
}

/// Seed the L6 external resource NOR as QEMU's first persistent boot does:
/// the package resource region followed by erased flash.
pub fn seed_resource(firmware_dir: &Path) -> Result<Vec<u8>, String> {
    let region = read_segment(firmware_dir, "resource_region.bin", RESOURCE_NOR_SIZE)?;
    let mut bytes = vec![0xff; RESOURCE_NOR_SIZE];
    bytes[..region.len()].copy_from_slice(&region);
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn blank() -> MainNor {
        MainNor::from_bytes(vec![0xff; MAIN_NOR_SIZE]).unwrap()
    }

    #[test]
    fn update_marker_states() {
        let mut nor = blank();
        assert_eq!(nor.update_marker(), UpdateMarker::None);
        nor.bytes[UPDATE_MARKER..UPDATE_MARKER + 8].copy_from_slice(b"FW UPDT\0");
        assert_eq!(nor.update_marker(), UpdateMarker::Pending);
        nor.bytes[UPDATE_MARKER + 8..UPDATE_MARKER + 20].copy_from_slice(b"FW UPDT ERR\0");
        assert_eq!(nor.update_marker(), UpdateMarker::Failed);
    }

    #[test]
    fn seed_sets_l6max_installed_footer_without_changing_l6() {
        let root = std::env::temp_dir().join(format!(
            "l6nor-seed-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&root).unwrap();
        for (model, reset) in [(Model::L6max, 0x80000415u32), (Model::L6, 0x80001415)] {
            let mut main = Vec::new();
            main.extend_from_slice(&0x20230000u32.to_le_bytes());
            main.extend_from_slice(&reset.to_le_bytes());
            fs::write(root.join("main_firmware.bin"), &main).unwrap();
            fs::write(root.join("main_trailer.bin"), b"\x08\0\0\x000110").unwrap();
            fs::write(root.join("secondary_firmware.bin"), [0xff]).unwrap();
            fs::write(root.join("secondary_trailer.bin"), b"\x01\0\0\x000100").unwrap();
            let mut footer = [0xff; 0x40];
            footer[..4].copy_from_slice(b"TEST");
            footer[0x38..0x3c].copy_from_slice(&8u32.to_le_bytes());
            footer[0x3c..].copy_from_slice(b"0110");
            fs::write(root.join("package_footer.bin"), footer).unwrap();
            let nor = MainNor::seed(&root).unwrap();
            if model == Model::L6max {
                assert_eq!(&nor.bytes()[0x1f7fc0..0x1f8000], &footer);
                footer[0x38..0x3c].copy_from_slice(&9u32.to_le_bytes());
                fs::write(root.join("package_footer.bin"), footer).unwrap();
                assert!(MainNor::seed(&root).is_err());
            } else {
                assert!(nor.bytes()[0x1f7fc0..0x1f7ffc].iter().all(|b| *b == 0xff));
                assert_eq!(&nor.bytes()[0x1f7ffc..0x1f8000], b"0110");
            }
        }
        fs::remove_dir_all(root).unwrap();
    }
}
