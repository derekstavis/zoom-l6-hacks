//! Display reference images and menu checks.
use super::*;

/// Environment variable the driver sets to `1` for `--update-references`.
pub const UPDATE_REFERENCES_ENV: &str = "SMOKE_UPDATE_REFERENCES";
/// Directory, shared by a run's cases, where `--update-references` records
/// each display it writes, so cases that render one reference differently
/// fail instead of overwriting each other.
pub const REFERENCE_RECORDS_ENV: &str = "SMOKE_REFERENCE_RECORDS";
/// File in a case's logs listing the reference images it rewrote, one
/// repository-relative path per line.
pub const UPDATED_REFERENCES_FILE: &str = "updated-references.txt";

/// The L6max display as a 128×64 black-and-white image.
pub(crate) fn display_image(display: &[u8; 1024]) -> image::GrayImage {
    image::GrayImage::from_fn(128, 64, |x, y| {
        let index = ((63 - y as usize) / 8) * 128 + x as usize;
        image::Luma([if display[index] & (1 << ((63 - y as usize) % 8)) != 0 {
            255
        } else {
            0
        }])
    })
}

/// How many of the display's 8192 pixels differ from the reference image at
/// `path`; None when it is missing or not 128×64.
pub(crate) fn reference_differences(display: &[u8; 1024], path: &Path) -> Option<usize> {
    let expected = image::open(path).ok()?.to_luma8();
    (expected.dimensions() == (128, 64)).then(|| display_difference_count(display, &expected))
}

/// Whether the driver asked display checks to record references
/// (`--update-references`).
pub(crate) fn updating_references() -> bool {
    std::env::var(UPDATE_REFERENCES_ENV).as_deref() == Ok("1")
}

pub(super) fn repository_relative(path: &Path) -> String {
    path.strip_prefix(l6max_host::paths::repository_root())
        .unwrap_or(path)
        .display()
        .to_string()
}

/// Replace the reference at `path` with the display, for review in the diff,
/// and list it in [`UPDATED_REFERENCES_FILE`]. Fails when another case in the
/// run recorded a different display for the same reference.
pub(crate) fn record_display_reference(
    display: &[u8; 1024],
    path: &Path,
    logs: &Path,
    label: &str,
) -> Result<(), String> {
    let actual = display_image(display);
    let relative = repository_relative(path);
    let mut encoded = Vec::new();
    actual
        .write_to(
            &mut std::io::Cursor::new(&mut encoded),
            image::ImageFormat::Png,
        )
        .map_err(|e| e.to_string())?;
    if let Some(records) = std::env::var_os(REFERENCE_RECORDS_ENV) {
        use sha2::Digest;
        let records = std::path::Path::new(&records);
        std::fs::create_dir_all(records).map_err(|e| e.to_string())?;
        let record = records.join(format!(
            "{:x}.png",
            sha2::Sha256::digest(relative.as_bytes())
        ));
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&record)
        {
            Ok(mut file) => {
                std::io::Write::write_all(&mut file, &encoded).map_err(|e| e.to_string())?
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                if std::fs::read(&record).map_err(|e| e.to_string())? != encoded {
                    let saved = logs.join(format!("{label}.actual.png"));
                    actual.save(&saved).map_err(|e| e.to_string())?;
                    return Err(format!(
                        "another case in this run recorded a different display for {relative}; this one is in {}. A reference must show the same screen in every case that uses it",
                        saved.display()
                    ));
                }
            }
            Err(error) => return Err(error.to_string()),
        }
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    std::fs::write(path, &encoded).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut list = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(logs.join(UPDATED_REFERENCES_FILE))
        .map_err(|e| e.to_string())?;
    std::io::Write::write_all(&mut list, format!("{relative}\n").as_bytes())
        .map_err(|e| e.to_string())?;
    tracing::info!(event = "reference_updated", path = %relative);
    Ok(())
}

/// Save the display as `<logs>/<label>.actual.png` and describe how it
/// differs from the reference at `path`.
pub(crate) fn display_mismatch(
    display: &[u8; 1024],
    path: &Path,
    logs: &Path,
    label: &str,
) -> String {
    let relative = repository_relative(path);
    let saved = logs.join(format!("{label}.actual.png"));
    if let Err(error) = display_image(display).save(&saved) {
        return format!("display differs from {relative}; saving it failed: {error}");
    }
    match reference_differences(display, path) {
        None if !path.exists() => format!(
            "display reference {relative} is missing; the display is in {}. Rerun with --update-references to record it, then review it in the diff",
            saved.display()
        ),
        None => format!("display reference {relative} is not a 128×64 image"),
        Some(count) => format!(
            "display differs from {relative} in {count} of 8192 pixels; the display is in {}. If the change is intended, rerun with --update-references and review the image in the diff",
            saved.display()
        ),
    }
}

/// Compare one captured display with the reference at `path`: pass when it
/// matches; with `--update-references`, record it; otherwise fail with
/// [`display_mismatch`].
pub(crate) fn check_display_reference(
    display: &[u8; 1024],
    path: &Path,
    logs: &Path,
    label: &str,
) -> Result<(), String> {
    if reference_differences(display, path) == Some(0) {
        Ok(())
    } else if updating_references() {
        record_display_reference(display, path, logs, label)
    } else {
        Err(display_mismatch(display, path, logs, label))
    }
}

pub(crate) fn display_difference_count(
    display: &[u8; 1024],
    reference: &image::GrayImage,
) -> usize {
    (0..64)
        .flat_map(|y| {
            (0..128).map(move |x| {
                let index = ((63 - y) / 8) * 128 + x;
                let mask = 1 << ((63 - y) % 8);
                let actual = display[index] & mask != 0;
                let expected = reference.get_pixel(x as u32, y as u32).0[0] >= 128;
                usize::from(actual != expected)
            })
        })
        .sum()
}

/// Capture one bounded host menu observation for later assertions.
pub(super) fn capture_menu(
    session: &Session,
    menu: &str,
    path: &[String],
    guest_timeout_ms: u32,
    wall_timeout_s: u64,
) -> Result<l6ctl::MenuSnapshot, String> {
    let control = session.control();
    let start = control
        .input_status(l6ctl::InputSource::Panel)
        .map_err(|e| e.to_string())?
        .guest_ms;
    let deadline = Instant::now() + Duration::from_secs(wall_timeout_s);
    control
        .menu_before(menu, path, deadline, || {
            control
                .input_status(l6ctl::InputSource::Panel)
                .map(|s| s.guest_ms.wrapping_sub(start) >= guest_timeout_ms)
                .unwrap_or(true)
        })
        .map_err(|e| e.to_string())
}
