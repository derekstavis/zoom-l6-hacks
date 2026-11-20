//! Which firmware a case boots, by name rather than by pinned hashes.
//!
//! The supported stock images are the ones the patch tools accept
//! (`firmware_patch_core`). A patched image is known when the report our patch
//! tools wrote for the case (`fixture_layout::FIRMWARE_REPORT`) names its
//! exact hash; those tools only build from the supported stock image.
use crate::fixture_layout::FIRMWARE_REPORT;
use l6max_host::engine::Options;
use sha2::{Digest, Sha256};
use std::{fs, path::Path};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FirmwareIdentity {
    /// The supported stock MAIN image.
    Stock { model: l6fw::Model },
    /// An image our patch tools built from the stock image and reported.
    Built { sha256: String },
    /// Anything else.
    Unknown { sha256: String },
}

/// SHA-256 of the supported stock L6max 1.10 update package, which the
/// firmware-update lifecycle reinstalls byte for byte.
pub const STOCK_L6MAX_PACKAGE_SHA256: &str =
    "4e20f92c9c5131ed082c0feec2b2d7d0445fb576296f11f6dace56a4fc0c9b47";

/// SHA-256 of the supported stock MAIN image for `model`.
pub fn stock_sha256(model: l6fw::Model) -> &'static str {
    match model {
        l6fw::Model::L6 => firmware_patch_core::L6_EXPECTED_FIRMWARE_SHA256,
        l6fw::Model::L6max => firmware_patch_core::EXPECTED_FIRMWARE_SHA256,
    }
}

/// Whether `main` is the supported stock image of `model`.
pub fn is_stock(main: &[u8], model: l6fw::Model) -> bool {
    format!("{:x}", Sha256::digest(main)) == stock_sha256(model)
}

/// Identify `main`, the image a case boots, using the build report in `case`.
pub fn identify(main: &[u8], case: &Path) -> FirmwareIdentity {
    let sha256 = format!("{:x}", Sha256::digest(main));
    for model in [l6fw::Model::L6, l6fw::Model::L6max] {
        if sha256 == stock_sha256(model) {
            return FirmwareIdentity::Stock { model };
        }
    }
    let reported = fs::read(case.join(FIRMWARE_REPORT))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        .and_then(|report| report["patched_sha256"].as_str().map(str::to_owned));
    if reported.as_deref() == Some(sha256.as_str()) {
        FirmwareIdentity::Built { sha256 }
    } else {
        FirmwareIdentity::Unknown { sha256 }
    }
}

/// Identify the MAIN image the case selected in `options` boots: the one
/// installed in its persistent NOR, or else the firmware directory's.
pub fn identify_selected(options: &Options) -> Result<FirmwareIdentity, String> {
    Ok(identify(
        &crate::devices::selected_main_image(options)?,
        &options.logs,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_patched_image_is_known_only_when_its_report_names_it() {
        let case = std::env::temp_dir().join(format!("firmware-identity-{}", std::process::id()));
        fs::create_dir_all(&case).unwrap();
        let main = b"not a stock image";
        let sha256 = format!("{:x}", Sha256::digest(main));
        assert_eq!(
            identify(main, &case),
            FirmwareIdentity::Unknown {
                sha256: sha256.clone()
            }
        );
        fs::write(
            case.join(FIRMWARE_REPORT),
            format!(r#"{{"patched_sha256":"{}"}}"#, "0".repeat(64)),
        )
        .unwrap();
        assert!(matches!(
            identify(main, &case),
            FirmwareIdentity::Unknown { .. }
        ));
        fs::write(
            case.join(FIRMWARE_REPORT),
            format!(r#"{{"patched_sha256":"{sha256}"}}"#),
        )
        .unwrap();
        assert_eq!(identify(main, &case), FirmwareIdentity::Built { sha256 });
        fs::remove_dir_all(&case).unwrap();
    }
}

/// The address `name` is declared at in `patches/api` for `model`.
pub fn symbol(model: l6fw::Model, name: &str) -> Option<u32> {
    let table = match model {
        l6fw::Model::L6 => firmware_api::l6::SYMBOLS,
        l6fw::Model::L6max => firmware_api::l6max::SYMBOLS,
    };
    table
        .binary_search_by(|(declared, _)| declared.cmp(&name))
        .ok()
        .map(|index| table[index].1)
}
