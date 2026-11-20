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

/// Read a menu's children from guest memory and compare their titles.
pub(super) fn expect_menu(
    session: &Session,
    menu: &str,
    path: &[String],
    items: &[String],
) -> Result<Value, String> {
    // Menu node layout and stock nodes, from patches/api/l6max/menu.rs.
    const NODE_TITLE: u32 = 0x0c;
    const NODE_CHILDREN: u32 = 0x10;
    const NODE_CHILD_COUNT: u32 = 0x14;
    use firmware_api::l6max::gui_text_pointers_by_language;
    const MAX_CHILDREN: u16 = 64;

    let root = match menu {
        "main" => l6max::MENU_NODE_MAIN,
        "project" => l6max::MENU_NODE_PROJECT,
        "sound-pad" => l6max::MENU_NODE_SOUND_PAD,
        "mixer" => l6max::MENU_NODE_MIXER,
        "system" => l6max::MENU_NODE_SYSTEM,
        _ => return Err(format!("unknown menu {menu:?}")),
    };
    let qmp = &session.engine.management[0];
    let read = |address: u32, length: usize| {
        qmp.read_memory(0, address, length)
            .map_err(|error| error.to_string())
    };
    let read_u32 = |address: u32| -> Result<u32, String> {
        Ok(u32::from_le_bytes(read(address, 4)?.try_into().unwrap()))
    };
    let read_u16 = |address: u32| -> Result<u16, String> {
        Ok(u16::from_le_bytes(read(address, 2)?.try_into().unwrap()))
    };
    let text = |id: u16| -> Result<String, String> {
        let pointer = read_u32(read_u32(gui_text_pointers_by_language)? + u32::from(id) * 4)?;
        if pointer == 0 {
            return Ok(String::new());
        }
        let units = read(pointer, 96)?
            .chunks_exact(2)
            .map(|b| u16::from_le_bytes([b[0], b[1]]))
            .take_while(|c| *c != 0)
            .collect::<Vec<_>>();
        String::from_utf16(&units).map_err(|error| error.to_string())
    };
    let children = |node: u32| -> Result<Vec<(u32, String)>, String> {
        let count = read_u16(node + NODE_CHILD_COUNT)?;
        if count > MAX_CHILDREN {
            return Err(format!("menu node {node:#010x} claims {count} children"));
        }
        let array = read_u32(node + NODE_CHILDREN)?;
        (0..u32::from(count))
            .map(|index| {
                let child = read_u32(array + index * 4)?;
                Ok((child, text(read_u16(child + NODE_TITLE)?)?))
            })
            .collect()
    };
    // Menu Loader adds its rows when the menu opens, so a press that did not
    // open it would otherwise look like missing rows.
    use firmware_api::l6max::GUI_RECORDER_WINDOW;
    use firmware_api::l6max::current_window;
    if read_u32(current_window)? == GUI_RECORDER_WINDOW {
        return Err("expect-menu: the menu is not open; the recorder screen is showing".into());
    }
    let mut node = root;
    for title in path {
        node = children(node)?
            .into_iter()
            .find(|(_, child)| child == title)
            .map(|(child, _)| child)
            .ok_or_else(|| format!("menu {menu} has no child {title:?} on path {path:?}"))?;
    }
    let actual = children(node)?
        .into_iter()
        .map(|(_, title)| title)
        .collect::<Vec<_>>();
    if actual != items {
        return Err(format!(
            "menu {menu} {path:?} lists {actual:?}, expected {items:?}"
        ));
    }
    Ok(json!({"menu":menu,"path":path,"items":actual}))
}
