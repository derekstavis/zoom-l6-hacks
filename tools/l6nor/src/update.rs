//! Comparisons for an L6max MAIN-only update against stopped state files.
use crate::nor::{MainNor, UpdateMarker};
use serde::Serialize;

#[derive(Debug, PartialEq, Eq, Serialize)]
pub struct UpdateComparison {
    pub main_payload_matches: bool,
    pub boot_region_preserved: bool,
    pub protected_region_preserved: bool,
    pub update_marker_absent: bool,
    pub panel_image_matches: bool,
}

pub fn panel_image_matches(panel: &[u8], package: &[u8]) -> Result<bool, String> {
    let images = l6fw::update::images(package).map_err(|error| error.to_string())?;
    Ok(panel.get(..0x4000) == Some(images.panel_flash))
}

/// Compare two complete stopped NOR samples and the panel against a
/// validated package. Values are observations; callers choose assertions.
pub fn compare_l6max(
    pending: &MainNor,
    installed: &MainNor,
    panel: &[u8],
    package: &[u8],
) -> Result<UpdateComparison, String> {
    if pending.model()? != l6fw::Model::L6max || installed.model()? != l6fw::Model::L6max {
        return Err("L6max update comparison requires L6max NOR images".into());
    }
    let images = l6fw::update::images(package).map_err(|error| error.to_string())?;
    let before = pending.bytes();
    let after = installed.bytes();
    Ok(UpdateComparison {
        main_payload_matches: after.get(0x50000..0x1f8000) == Some(images.main_nor_payload),
        boot_region_preserved: after.get(..0x50000) == before.get(..0x50000),
        protected_region_preserved: after.get(0x1f8000..0x1ff000) == before.get(0x1f8000..0x1ff000),
        update_marker_absent: installed.update_marker() == UpdateMarker::None,
        panel_image_matches: panel.get(..0x4000) == Some(images.panel_flash),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    #[ignore = "requires L6MAX_UPDATE_TEST_PACKAGE"]
    fn independently_detects_each_changed_region() {
        let package =
            std::fs::read(std::env::var_os("L6MAX_UPDATE_TEST_PACKAGE").unwrap()).unwrap();
        let images = l6fw::update::images(&package).unwrap();
        let mut pending = vec![0xff; crate::nor::MAIN_NOR_SIZE];
        pending[0x50000..0x1f8000].copy_from_slice(images.main_nor_payload);
        pending[0x1ff000..0x1ff008].copy_from_slice(b"FW UPDT\0");
        let mut installed = pending.clone();
        installed[0x50000..0x1f8000].copy_from_slice(images.main_nor_payload);
        installed[0x1ff000..0x1ff008].fill(0xff);
        let before = MainNor::from_bytes(pending).unwrap();
        let after = MainNor::from_bytes(installed.clone()).unwrap();
        let baseline = compare_l6max(&before, &after, images.panel_flash, &package).unwrap();
        assert!(
            baseline.main_payload_matches
                && baseline.boot_region_preserved
                && baseline.protected_region_preserved
                && baseline.update_marker_absent
                && baseline.panel_image_matches
        );
        let mut wrong_model = installed.clone();
        wrong_model[0x50004..0x50008].copy_from_slice(&0x80001001u32.to_le_bytes());
        assert!(
            compare_l6max(
                &before,
                &MainNor::from_bytes(wrong_model).unwrap(),
                images.panel_flash,
                &package
            )
            .is_err()
        );
        for (offset, field) in [(0x100, 0), (0x50010, 1), (0x1f8000, 2), (0x1ff000, 3)] {
            let mut changed = installed.clone();
            if field == 3 {
                changed[offset..offset + 8].copy_from_slice(b"FW UPDT\0");
            } else {
                changed[offset] ^= 1;
            }
            let result = compare_l6max(
                &before,
                &MainNor::from_bytes(changed).unwrap(),
                images.panel_flash,
                &package,
            )
            .unwrap();
            assert!(!match field {
                0 => result.boot_region_preserved,
                1 => result.main_payload_matches,
                2 => result.protected_region_preserved,
                _ => result.update_marker_absent,
            });
        }
        let mut panel = images.panel_flash.to_vec();
        panel[0] ^= 1;
        assert!(
            !compare_l6max(&before, &after, &panel, &package)
                .unwrap()
                .panel_image_matches
        );
    }
}
