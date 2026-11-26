//! Bounded, non-atomic L6max menu tree observation.
use crate::qmp::QmpClient;
use firmware_api::l6max;
use l6fw::Model;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::time::Instant;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Snapshot {
    pub menu: String,
    pub path: Vec<String>,
    pub items: Vec<String>,
    pub window: u32,
    pub atomic: bool,
    pub revision_guard: String,
}

fn snapshot_with(
    menu: &str,
    path: &[String],
    mut read: impl FnMut(u32, usize) -> Result<Vec<u8>, String>,
) -> Result<Snapshot, String> {
    if path.len() > 32 || path.iter().any(|s| s.len() > 384) {
        return Err("menu path exceeds observation bounds".into());
    }
    let root = match menu {
        "main" => l6max::MENU_NODE_MAIN,
        "project" => l6max::MENU_NODE_PROJECT,
        "sound-pad" => l6max::MENU_NODE_SOUND_PAD,
        "mixer" => l6max::MENU_NODE_MIXER,
        "system" => l6max::MENU_NODE_SYSTEM,
        _ => return Err(format!("unknown menu {menu:?}")),
    };
    fn word(
        read: &mut impl FnMut(u32, usize) -> Result<Vec<u8>, String>,
        address: u32,
    ) -> Result<u32, String> {
        Ok(u32::from_le_bytes(
            read(address, 4)?
                .try_into()
                .map_err(|_| "short menu word")?,
        ))
    }
    fn half(
        read: &mut impl FnMut(u32, usize) -> Result<Vec<u8>, String>,
        address: u32,
    ) -> Result<u16, String> {
        Ok(u16::from_le_bytes(
            read(address, 2)?
                .try_into()
                .map_err(|_| "short menu halfword")?,
        ))
    }
    fn add(base: u32, offset: u32) -> Result<u32, String> {
        base.checked_add(offset)
            .ok_or_else(|| "menu address overflow".into())
    }
    fn children(
        read: &mut impl FnMut(u32, usize) -> Result<Vec<u8>, String>,
        node: u32,
    ) -> Result<Vec<(u32, String)>, String> {
        let count = half(read, add(node, 0x14)?)?;
        if count > 64 {
            return Err(format!("menu node {node:#010x} claims {count} children"));
        }
        let array = word(read, add(node, 0x10)?)?;
        let mut result = Vec::new();
        for index in 0..u32::from(count) {
            let child = word(read, add(array, index * 4)?)?;
            let id = half(read, add(child, 0x0c)?)?;
            let table = word(read, l6max::gui_text_pointers_by_language)?;
            let pointer = word(read, add(table, u32::from(id) * 4)?)?;
            let title = if pointer == 0 {
                String::new()
            } else {
                let bytes = read(pointer, 96)?;
                if bytes.len() != 96 {
                    return Err("short menu text".into());
                }
                let units = bytes
                    .chunks_exact(2)
                    .map(|b| u16::from_le_bytes([b[0], b[1]]))
                    .take_while(|x| *x != 0)
                    .collect::<Vec<_>>();
                String::from_utf16(&units).map_err(|e| e.to_string())?
            };
            result.push((child, title));
        }
        Ok(result)
    }
    let window = word(&mut read, l6max::current_window)?;
    if window == l6max::GUI_RECORDER_WINDOW {
        return Err("expect-menu: the menu is not open; the recorder screen is showing".into());
    }
    let mut node = root;
    for title in path {
        node = children(&mut read, node)?
            .into_iter()
            .find(|(_, item)| item == title)
            .map(|(node, _)| node)
            .ok_or_else(|| format!("menu {menu} has no child {title:?} on path {path:?}"))?;
    }
    Ok(Snapshot {
        menu: menu.into(),
        path: path.into(),
        items: children(&mut read, node)?
            .into_iter()
            .map(|(_, title)| title)
            .collect(),
        window,
        atomic: false,
        revision_guard: "L6max 1.10 GUI and menu submenu routine fingerprints".into(),
    })
}

pub fn snapshot_before(
    qmp: &QmpClient,
    model: Model,
    menu: &str,
    path: &[String],
    deadline: Instant,
    cancelled: impl Fn() -> bool,
) -> Result<Snapshot, String> {
    let mut read = |address: u32, length: usize| {
        if cancelled() || Instant::now() >= deadline {
            return Err("menu observation cancelled or timed out".into());
        }
        let end = address
            .checked_add(u32::try_from(length).map_err(|_| "menu read too large")?)
            .ok_or("menu address overflow")?;
        if !((address >= 0x8000_0000 && end <= 0x8020_0000)
            || (address >= 0x8020_0000 && end <= 0x8420_0000)
            || (address >= 0x2000_0000 && end <= 0x2030_0000))
        {
            return Err(format!("menu read {address:#010x} outside MAIN memory"));
        }
        let bytes = qmp
            .read_memory_before(0, address, length, deadline)
            .map_err(|e| e.to_string())?;
        if cancelled() || Instant::now() >= deadline {
            return Err("menu observation cancelled or timed out".into());
        }
        Ok(bytes)
    };
    crate::knob_popup::verify(model, &mut read)?;
    let submenu = read(0x8003_d278, 64)?;
    if format!("{:x}", Sha256::digest(&submenu))
        != "968ceb983bedd48b6d270a4168554d9c6bdb0a31f155a9985e49a636129bc3ad"
    {
        return Err("unsupported L6max menu submenu routine".into());
    }
    snapshot_with(menu, path, read)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    #[test]
    fn traverses_titles_in_order_and_reports_missing_path_and_invalid_text() {
        let mut memory = BTreeMap::<u32, u8>::new();
        let mut put = |address: u32, bytes: &[u8]| {
            for (i, b) in bytes.iter().enumerate() {
                memory.insert(address + i as u32, *b);
            }
        };
        put(l6max::current_window, &1u32.to_le_bytes());
        put(
            l6max::gui_text_pointers_by_language,
            &0x8020_0100u32.to_le_bytes(),
        );
        put(l6max::MENU_NODE_SYSTEM + 0x14, &1u16.to_le_bytes());
        put(
            l6max::MENU_NODE_SYSTEM + 0x10,
            &0x8020_0200u32.to_le_bytes(),
        );
        put(0x8020_0200, &0x8020_0300u32.to_le_bytes());
        put(0x8020_030c, &1u16.to_le_bytes());
        put(0x8020_030e, &[0, 0]);
        put(0x8020_0310, &0x8020_0400u32.to_le_bytes());
        put(0x8020_0314, &1u16.to_le_bytes());
        put(0x8020_0400, &0x8020_0500u32.to_le_bytes());
        put(0x8020_050c, &2u16.to_le_bytes());
        put(0x8020_0104, &0x8020_0600u32.to_le_bytes());
        put(0x8020_0108, &0x8020_0700u32.to_le_bytes());
        let text = |s: &str| {
            let mut b = vec![0; 96];
            for (i, c) in s.encode_utf16().enumerate() {
                b[2 * i..2 * i + 2].copy_from_slice(&c.to_le_bytes());
            }
            b
        };
        put(0x8020_0600, &text("Patches"));
        put(0x8020_0700, &text("Knob"));
        let read = |address: u32, length: usize| -> Result<Vec<u8>, String> {
            (0..length)
                .map(|i| {
                    memory
                        .get(&(address + i as u32))
                        .copied()
                        .ok_or_else(|| format!("missing {address:#x}"))
                })
                .collect()
        };
        let snapshot = snapshot_with("system", &["Patches".into()], read).unwrap();
        assert_eq!(snapshot.items, ["Knob"]);
        assert!(!snapshot.atomic);
        assert!(
            snapshot_with("system", &["Missing".into()], read)
                .unwrap_err()
                .contains("no child")
        );
        assert!(snapshot_with("invalid", &[], read).is_err());
        memory.insert(0x8020_0700, 0x00);
        memory.insert(0x8020_0701, 0xd8);
        let read = |address: u32, length: usize| -> Result<Vec<u8>, String> {
            (0..length)
                .map(|i| {
                    memory
                        .get(&(address + i as u32))
                        .copied()
                        .ok_or_else(|| "missing".into())
                })
                .collect()
        };
        assert!(snapshot_with("system", &["Patches".into()], read).is_err());
    }
}
