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

/// Observations from a stopped L6 installer run. Segment and resource data are
/// taken from the package itself; the caller decides which results to assert.
#[derive(Debug, PartialEq, Eq, Serialize)]
pub struct L6UpdateComparison {
    pub segment_matches: std::collections::BTreeMap<String, bool>,
    pub boot_region_preserved: bool,
    pub protected_region_preserved: bool,
    pub update_marker_absent: bool,
    pub resource_nor_matches: bool,
}

/// Set the replacement installer's pending marker in a stopped L6 MAIN NOR.
/// This is an explicit state mutation, not an emulation of a firmware handoff.
pub fn mark_l6_pending(nor: &mut MainNor) -> Result<(), String> {
    if nor.model()? != l6fw::Model::L6 {
        return Err("pending marker requires L6 NOR".into());
    }
    if nor.update_marker() != UpdateMarker::None {
        return Err("pending marker already present or failed".into());
    }
    let start = crate::nor::UPDATE_MARKER;
    if &nor.bytes()[start + 8..start + 20] == b"FW UPDT ERR\0" {
        return Err("stale installer failure marker must be cleared first".into());
    }
    nor.bytes_mut()[start..start + 8].copy_from_slice(b"FW UPDT\0");
    Ok(())
}

/// Compare an L6 installation against the source package and stopped baseline.
pub fn compare_l6(
    pending: &MainNor,
    installed: &MainNor,
    resource_nor: &[u8],
    package: &[u8],
) -> Result<L6UpdateComparison, String> {
    if pending.model()? != l6fw::Model::L6 || installed.model()? != l6fw::Model::L6 {
        return Err("L6 update comparison requires L6 NOR images".into());
    }
    if l6fw::model_from_package(package).map_err(|error| error.to_string())? != l6fw::Model::L6 {
        return Err("L6 update comparison requires an L6 package".into());
    }
    let (_, segments) = l6fw::unpack_bytes(package).map_err(|error| error.to_string())?;
    let mut segment_matches = std::collections::BTreeMap::new();
    for (name, start, capacity) in [
        ("main_firmware.bin", 0x50000usize, 0x1a2ff8usize),
        ("main_trailer.bin", 0x1f2ff8, 8),
        ("secondary_firmware.bin", 0x1f3000, 0x3ff8),
        ("secondary_trailer.bin", 0x1f6ff8, 8),
        ("package_footer.bin", 0x1f7fc0, 0x40),
    ] {
        let bytes = segments
            .get(name)
            .ok_or_else(|| format!("package lacks {name}"))?;
        if bytes.len() > capacity {
            return Err(format!("{name} exceeds its NOR range"));
        }
        segment_matches.insert(
            name.to_owned(),
            installed.bytes().get(start..start + bytes.len()) == Some(bytes.as_slice()),
        );
    }
    let resource = segments
        .get("resource_region.bin")
        .ok_or("package lacks resource_region.bin")?;
    if resource.len() > crate::nor::RESOURCE_NOR_SIZE {
        return Err("package resource exceeds NOR".into());
    }
    let mut expected = vec![0xff; crate::nor::RESOURCE_NOR_SIZE];
    expected[..resource.len()].copy_from_slice(resource);
    Ok(L6UpdateComparison {
        segment_matches,
        boot_region_preserved: installed.bytes().get(..0x50000) == pending.bytes().get(..0x50000),
        protected_region_preserved: installed.bytes().get(0x1f8000..0x1ff000)
            == pending.bytes().get(0x1f8000..0x1ff000),
        update_marker_absent: installed.update_marker() == UpdateMarker::None,
        resource_nor_matches: resource_nor == expected,
    })
}

#[cfg(test)]
mod l6_tests {
    use super::*;

    #[test]
    fn pending_marker_changes_only_its_eight_bytes_and_rejects_wrong_or_stale_state() {
        let mut bytes = vec![0xff; crate::nor::MAIN_NOR_SIZE];
        bytes[0x50000..0x50004].copy_from_slice(&0x20230000u32.to_le_bytes());
        bytes[0x50004..0x50008].copy_from_slice(&0x80001415u32.to_le_bytes());
        let original = bytes.clone();
        let mut l6 = MainNor::from_bytes(bytes.clone()).unwrap();
        mark_l6_pending(&mut l6).unwrap();
        assert_eq!(l6.update_marker(), UpdateMarker::Pending);
        let marker = crate::nor::UPDATE_MARKER;
        assert_eq!(&l6.bytes()[..marker], &original[..marker]);
        assert_eq!(&l6.bytes()[marker + 8..], &original[marker + 8..]);
        assert!(mark_l6_pending(&mut l6).is_err());
        bytes[marker + 8..marker + 20].copy_from_slice(b"FW UPDT ERR\0");
        assert!(mark_l6_pending(&mut MainNor::from_bytes(bytes.clone()).unwrap()).is_err());
        bytes[0x50004..0x50008].copy_from_slice(&0x80000415u32.to_le_bytes());
        assert!(mark_l6_pending(&mut MainNor::from_bytes(bytes).unwrap()).is_err());
    }

    #[test]
    #[ignore = "requires L6_UPDATE_TEST_PACKAGE"]
    fn l6_package_comparison_detects_each_changed_region_and_rejects_other_models() {
        let package = std::fs::read(std::env::var_os("L6_UPDATE_TEST_PACKAGE").unwrap()).unwrap();
        let (_, segments) = l6fw::unpack_bytes(&package).unwrap();
        let mut installed = vec![0xff; crate::nor::MAIN_NOR_SIZE];
        let ranges = [
            ("main_firmware.bin", 0x50000),
            ("main_trailer.bin", 0x1f2ff8),
            ("secondary_firmware.bin", 0x1f3000),
            ("secondary_trailer.bin", 0x1f6ff8),
            ("package_footer.bin", 0x1f7fc0),
        ];
        for (name, start) in ranges {
            let bytes = &segments[name];
            installed[start..start + bytes.len()].copy_from_slice(bytes);
        }
        let mut resource = vec![0xff; crate::nor::RESOURCE_NOR_SIZE];
        let region = &segments["resource_region.bin"];
        resource[..region.len()].copy_from_slice(region);
        let after = MainNor::from_bytes(installed.clone()).unwrap();
        let mut pending = after.clone();
        mark_l6_pending(&mut pending).unwrap();
        assert!(mark_l6_pending(&mut pending).is_err());
        let result = compare_l6(&pending, &after, &resource, &package).unwrap();
        assert!(result.segment_matches.values().all(|ok| *ok));
        assert!(
            result.resource_nor_matches
                && result.boot_region_preserved
                && result.protected_region_preserved
                && result.update_marker_absent
        );
        for (name, start) in ranges {
            let mut corrupted = installed.clone();
            corrupted[start] ^= 1;
            let result = compare_l6(
                &pending,
                &MainNor::from_bytes(corrupted).unwrap(),
                &resource,
                &package,
            )
            .unwrap();
            assert!(!result.segment_matches[name], "{name}");
        }
        for (offset, field) in [(0, 0), (0x1f8000, 1), (0x1ff000, 2)] {
            let mut corrupted = installed.clone();
            if field == 2 {
                corrupted[offset..offset + 8].copy_from_slice(b"FW UPDT\0");
            } else {
                corrupted[offset] ^= 1;
            }
            let result = compare_l6(
                &pending,
                &MainNor::from_bytes(corrupted).unwrap(),
                &resource,
                &package,
            )
            .unwrap();
            assert!(!match field {
                0 => result.boot_region_preserved,
                1 => result.protected_region_preserved,
                _ => result.update_marker_absent,
            });
        }
        resource[0] ^= 1;
        assert!(
            !compare_l6(&pending, &after, &resource, &package)
                .unwrap()
                .resource_nor_matches
        );
        assert!(
            compare_l6(&pending, &after, &resource[..100], &package)
                .is_ok_and(|r| !r.resource_nor_matches)
        );
        let mut wrong = installed;
        wrong[0x50004..0x50008].copy_from_slice(&0x80000001u32.to_le_bytes());
        let wrong = MainNor::from_bytes(wrong).unwrap();
        assert!(compare_l6(&pending, &wrong, &resource, &package).is_err());
    }
}
