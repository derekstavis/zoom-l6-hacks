//! Read-only identity for the knob-dialog member of a composed SD module.
//!
//! This binds a supplied module/report pair; it does not by itself attest a
//! running guest or authorize debugger mutation. The relocated registry and
//! live immutable bytes must also be verified before using any addresses.
use crate::effect_editor::SlotIdentity;
use patch_abi::dynamic::Module;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::time::Instant;

const GUARD_LEN: usize = 32;

/// Dynamic objects used by the knob preflight. This binds the observed table
/// entries to bounded writable MAIN RAM; it does not acquire a debug lease or
/// authorize arbitrary writes. It does not establish allocation ownership or
/// cover widget 60 or other backgrounds. Call while MAIN is stopped; callers
/// must revalidate targets when authorizing later mutations.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PreflightTargets {
    pub widget7: u32,
    pub background10: u32,
    pub notification_semaphore: u32,
}
impl PreflightTargets {
    pub fn verify(
        self,
        mut read_word: impl FnMut(u32) -> Result<u32, String>,
    ) -> Result<Self, String> {
        fn writable(address: u32, length: u32) -> bool {
            address.checked_add(length).is_some_and(|end| {
                (address >= 0x2000_0000 && end <= 0x2030_0000)
                    || (address >= 0x8020_0000 && end <= 0x8420_0000)
            })
        }
        if !writable(self.widget7, 16)
            || !writable(self.background10, 1)
            || self.notification_semaphore & 3 != 0
            || !writable(self.notification_semaphore, 60)
        {
            return Err("knob preflight target outside writable MAIN RAM".into());
        }
        for (address, expected) in [
            (firmware_api::l6max::gui_widget_table + 7 * 4, self.widget7),
            (
                firmware_api::l6max::gui_background_table + 10 * 4,
                self.background10,
            ),
            (
                firmware_api::l6max::notification_slot,
                self.notification_semaphore,
            ),
        ] {
            if read_word(address)? != expected {
                return Err(format!("knob preflight target changed at {address:#010x}"));
            }
        }
        Ok(self)
    }
}

/// Verify the optional recorder widget used for the alias/collision probes.
/// This is a stopped-guest observation, not a persistent write capability.
pub fn verify_preflight_widget60(
    widget: u32,
    read_word: impl FnOnce(u32) -> Result<u32, String>,
) -> Result<(), String> {
    let end = widget.checked_add(16).ok_or("widget 60 range overflow")?;
    if !((widget >= 0x2000_0000 && end <= 0x2030_0000)
        || (widget >= 0x8020_0000 && end <= 0x8420_0000))
    {
        return Err("widget 60 outside writable MAIN RAM".into());
    }
    if read_word(firmware_api::l6max::gui_widget_table + 60 * 4)? != widget {
        return Err("widget 60 changed before preflight probe".into());
    }
    Ok(())
}
/// Check the stock notification semaphore slot and count-field span while
/// MAIN is stopped. This does not establish allocation ownership.
pub fn verify_notification_semaphore(
    semaphore: u32,
    read_word: impl FnOnce(u32) -> Result<u32, String>,
) -> Result<(), String> {
    let end = semaphore
        .checked_add(60)
        .ok_or("semaphore range overflow")?;
    if semaphore & 3 != 0
        || !((semaphore >= 0x2000_0000 && end <= 0x2030_0000)
            || (semaphore >= 0x8020_0000 && end <= 0x8420_0000))
    {
        return Err("notification semaphore outside writable MAIN RAM".into());
    }
    if read_word(firmware_api::l6max::notification_slot)? != semaphore {
        return Err("notification semaphore slot changed".into());
    }
    Ok(())
}
/// Validate the initialized slot-zero state before a signature or mailbox
/// probe. The mailbox's idle tag is required only when handing it to guest.
pub fn verify_bitmap_probe_state(state: [u8; 4], require_idle_mailbox: bool) -> Result<(), String> {
    if state[..2] != [0x4e, 0x4b] || (require_idle_mailbox && state[3] != 0xff) {
        return Err("knob bitmap state is not ready for the probe".into());
    }
    Ok(())
}
/// Exact callback and hook bytes derived from a report-identical firmware image.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FirmwareBinding {
    pub image_sha256: String,
    pub payload_address: u32,
    pub payload_length: u32,
    pub callback: u32,
    pub callback_guard: [u8; GUARD_LEN],
    pub hook_guard: [u8; GUARD_LEN],
}
impl FirmwareBinding {
    pub fn from_fixture(report: &[u8], image: &[u8]) -> Result<Self, String> {
        let value: serde_json::Value = serde_json::from_slice(report).map_err(|e| e.to_string())?;
        let hash = format!("{:x}", Sha256::digest(image));
        if value["model"].as_str() != Some("L6max")
            || value["image_length"].as_u64() != Some(image.len() as u64)
            || value["patched_sha256"].as_str() != Some(hash.as_str())
        {
            return Err("firmware report does not identify the selected L6max MAIN image".into());
        }
        let patches = value["patches"]
            .as_array()
            .ok_or("firmware report missing patches")?;
        let mut knob = patches
            .iter()
            .filter(|patch| patch["name"].as_str() == Some("knob-dialog"));
        let patch = knob.next().ok_or("firmware report missing knob-dialog")?;
        if knob.next().is_some() {
            return Err("ambiguous knob firmware report".into());
        }
        let address = patch["payload_address"]
            .as_str()
            .and_then(|s| u32::from_str_radix(s.trim_start_matches("0x"), 16).ok())
            .ok_or("invalid knob payload address")?;
        let length = patch["payload_length"]
            .as_u64()
            .and_then(|v| u32::try_from(v).ok())
            .ok_or("invalid knob payload length")?;
        let callback = patch["knob_callback"]
            .as_u64()
            .and_then(|v| u32::try_from(v).ok())
            .ok_or("firmware report missing knob callback export")?;
        let end = address.checked_add(length).ok_or("knob payload overflow")?;
        if address < l6fw::Model::L6max.main_base()
            || end > l6fw::Model::L6max.slot_end()
            || callback & 1 != 0
            || callback < address
            || callback
                .checked_add(GUARD_LEN as u32)
                .is_none_or(|v| v > end)
        {
            return Err("knob callback outside firmware payload".into());
        }
        let read = |a: u32| -> Result<[u8; GUARD_LEN], String> {
            let offset = a
                .checked_sub(l6fw::Model::L6max.main_base())
                .ok_or("firmware address precedes image")? as usize;
            image
                .get(
                    offset
                        ..offset
                            .checked_add(GUARD_LEN)
                            .ok_or("firmware guard overflow")?,
                )
                .ok_or("firmware guard outside image")?
                .try_into()
                .map_err(|_| "short firmware guard".into())
        };
        Ok(Self {
            image_sha256: hash,
            payload_address: address,
            payload_length: length,
            callback: callback | 1,
            callback_guard: read(callback)?,
            hook_guard: read(firmware_api::l6max::mixer_apply_channel_parameter)?,
        })
    }
    pub fn validate_live(
        &self,
        registered: u32,
        mut read: impl FnMut(u32, usize) -> Result<Vec<u8>, String>,
    ) -> Result<(), String> {
        let address = self.callback & !1;
        if self.image_sha256.len() != 64
            || !self.image_sha256.bytes().all(|b| b.is_ascii_hexdigit())
            || self.payload_address < l6fw::Model::L6max.main_base()
            || self
                .payload_address
                .checked_add(self.payload_length)
                .is_none_or(|end| end > l6fw::Model::L6max.slot_end())
            || self.callback & 1 == 0
            || registered != self.callback
            || address < self.payload_address
            || address.checked_add(GUARD_LEN as u32).is_none_or(|end| {
                self.payload_address
                    .checked_add(self.payload_length)
                    .is_none_or(|payload_end| end > payload_end)
            })
        {
            return Err("registered callback does not match firmware binding".into());
        }
        if read(
            firmware_api::l6max::mixer_apply_channel_parameter,
            GUARD_LEN,
        )? != self.hook_guard
            || read(address, GUARD_LEN)? != self.callback_guard
        {
            return Err("live knob firmware code does not match selected image".into());
        }
        Ok(())
    }
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SdBinding {
    pub identity: SlotIdentity,
    pub hook_offset: u32,
    pub callback_offset: u32,
    pub callback_guard: [u8; GUARD_LEN],
}
impl SdBinding {
    pub fn from_fixture(report: &[u8], bytes: &[u8]) -> Result<Self, String> {
        let identity = SlotIdentity::from_fixture_member(report, bytes, "knob-dialog")?;
        let report: serde_json::Value =
            serde_json::from_slice(report).map_err(|e| e.to_string())?;
        if report["model"].as_str() != Some("L6max") {
            return Err("knob SD module requires an L6max report".into());
        }
        let patch = &report["patches"][0];
        let module = Module::decode(bytes).map_err(|e| format!("invalid SD module: {e:?}"))?;
        let symbol = |name: &str| -> Result<u32, String> {
            patch["exports"][name]
                .as_u64()
                .and_then(|v| u32::try_from(v).ok())
                .ok_or_else(|| format!("missing knob module symbol {name}"))
        };
        let callback_offset = symbol("knob_dialog_display_tick")?;
        let hook_offset = symbol("__l6_dynamic_call_post_knob_changed")?;
        let mut hooks = module.hooks.iter().filter(|hook| {
            hook.address == firmware_api::l6max::mixer_apply_channel_parameter
                && hook.entry_offset == hook_offset
        });
        let hook = hooks.next().ok_or("knob mixer hook missing")?;
        if hooks.next().is_some() {
            return Err("ambiguous knob mixer hook".into());
        }
        let _ = hook;
        if hook_offset >= identity.state_offset {
            return Err("knob mixer hook outside immutable code".into());
        }
        let start = callback_offset as usize;
        let end = start
            .checked_add(GUARD_LEN)
            .ok_or("callback guard overflow")?;
        if callback_offset & 1 != 0
            || end > identity.state_offset as usize
            || identity.masked_words.iter().any(|offset| {
                (*offset as usize) < end && (*offset as usize).saturating_add(4) > start
            })
        {
            return Err("knob callback guard overlaps mutable or relocated code".into());
        }
        let callback_guard = module
            .code
            .get(start..end)
            .ok_or("knob callback guard outside code")?
            .try_into()
            .unwrap();
        let binding = Self {
            identity,
            hook_offset,
            callback_offset,
            callback_guard,
        };
        binding.validate()?;
        Ok(binding)
    }
    fn validate(&self) -> Result<(), String> {
        self.identity.validate()?;
        let end = self
            .callback_offset
            .checked_add(GUARD_LEN as u32)
            .ok_or("knob callback guard overflow")?;
        if self.hook_offset & 1 != 0
            || self.hook_offset >= self.identity.state_offset
            || self.callback_offset & 1 != 0
            || end > self.identity.state_offset
            || self
                .identity
                .masked_words
                .iter()
                .any(|offset| *offset < end && offset.saturating_add(4) > self.callback_offset)
        {
            return Err("invalid knob SD binding offsets".into());
        }
        Ok(())
    }
    /// Match the fixture against a live loader chain and its relocated code.
    /// MAIN must remain stopped throughout this non-atomic inspection.
    pub fn verify_live(
        &self,
        reader: &crate::sd_loader::Reader<'_>,
        site_index: u32,
        deadline: Instant,
        mut cancelled: impl FnMut() -> bool,
    ) -> Result<u32, String> {
        self.validate()?;
        let site = reader.site_cancellable(site_index, deadline, &mut cancelled)?;
        self.verify_site(&site, |address, length| {
            reader.read_chunk_cancellable(address, length, deadline, &mut cancelled)
        })
    }
    fn verify_site(
        &self,
        site: &crate::sd_loader::Site,
        mut read: impl FnMut(u32, usize) -> Result<Vec<u8>, String>,
    ) -> Result<u32, String> {
        self.validate()?;
        let mut matches = site.links.iter().filter(|link| {
            link.module_base
                .checked_add(self.hook_offset)
                .is_some_and(|entry| entry | 1 == link.entry)
        });
        let link = matches.next().ok_or("loaded knob hook not found")?;
        if matches.next().is_some() {
            return Err("ambiguous loaded knob hook".into());
        }
        let base = link.module_base;
        if base < 0x8020_0000
            || base
                .checked_add(self.identity.module_len)
                .is_none_or(|end| end > 0x8420_0000)
        {
            return Err("loaded knob module outside external RAM".into());
        }
        let mut bytes = Vec::with_capacity(self.identity.state_offset as usize);
        let mut offset = 0u32;
        while offset < self.identity.state_offset {
            let length = (self.identity.state_offset - offset).min(4096) as usize;
            let chunk = read(base + offset, length)?;
            if chunk.len() != length {
                return Err("short loaded knob module read".into());
            }
            bytes.extend(chunk);
            offset += length as u32;
        }
        for offset in &self.identity.masked_words {
            bytes[*offset as usize..*offset as usize + 4].fill(0);
        }
        if format!("{:x}", Sha256::digest(&bytes)) != self.identity.immutable_sha256 {
            return Err("loaded knob module does not match fixture".into());
        }
        let callback = base
            .checked_add(self.callback_offset)
            .ok_or("knob callback overflow")?;
        if read(callback, GUARD_LEN)? != self.callback_guard {
            return Err("loaded knob callback does not match fixture".into());
        }
        Ok(callback | 1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn preflight_targets_require_live_identity_and_bounded_writable_spans() {
        let targets = PreflightTargets {
            widget7: 0x8020_1000,
            background10: 0x8020_2000,
            notification_semaphore: 0x8020_3000,
        };
        let read = |address| match address {
            a if a == firmware_api::l6max::gui_widget_table + 28 => Ok(targets.widget7),
            a if a == firmware_api::l6max::gui_background_table + 40 => Ok(targets.background10),
            a if a == firmware_api::l6max::notification_slot => Ok(targets.notification_semaphore),
            _ => Err("unexpected address".into()),
        };
        assert_eq!(targets.verify(read).unwrap(), targets);
        assert!(targets.verify(|_| Ok(0)).is_err());
        assert!(
            PreflightTargets {
                widget7: 0x841f_fff8,
                ..targets
            }
            .verify(read)
            .is_err()
        );
        assert!(
            PreflightTargets {
                notification_semaphore: 0x8020_3001,
                ..targets
            }
            .verify(read)
            .is_err()
        );
        assert!(targets.verify(|_| Err("deadline".into())).is_err());
        let alias = 0x8020_4000;
        assert!(verify_preflight_widget60(alias, |_| Ok(alias)).is_ok());
        assert!(verify_preflight_widget60(alias, |_| Ok(alias + 4)).is_err());
        assert!(verify_preflight_widget60(0x841f_fff8, |_| Ok(0)).is_err());
    }
    #[test]
    fn semaphore_and_bitmap_guards_reject_changed_or_wrong_phase_state() {
        let semaphore = 0x8020_3000;
        assert!(verify_notification_semaphore(semaphore, |_| Ok(semaphore)).is_ok());
        assert!(verify_notification_semaphore(semaphore, |_| Ok(semaphore + 4)).is_err());
        assert!(verify_notification_semaphore(semaphore | 1, |_| Ok(semaphore)).is_err());
        assert!(verify_notification_semaphore(0x841f_ffc8, |_| Ok(0)).is_err());
        assert!(verify_notification_semaphore(semaphore, |_| Err("deadline".into())).is_err());
        assert!(verify_bitmap_probe_state([0x4e, 0x4b, 1, 0xff], true).is_ok());
        assert!(verify_bitmap_probe_state([0x4e, 0x4b, 1, 2], false).is_ok());
        assert!(verify_bitmap_probe_state([0x4e, 0x4b, 1, 2], true).is_err());
        assert!(verify_bitmap_probe_state([0x4f, 0x4b, 1, 0xff], false).is_err());
    }
    #[test]
    fn firmware_binding_requires_exact_export_image_and_live_bytes() {
        let base = l6fw::Model::L6max.main_base();
        let mut image = vec![0u8; 0x100000];
        let payload = base + 0xe0000;
        let callback = payload + 0x100;
        image[(callback - base) as usize] = 0x42;
        let report = serde_json::json!({"model":"L6max","image_length":image.len(),"patched_sha256":format!("{:x}",Sha256::digest(&image)),"patches":[{"name":"knob-dialog","payload_address":format!("{payload:#x}"),"payload_length":1024,"knob_callback":callback}]});
        let binding =
            FirmwareBinding::from_fixture(&serde_json::to_vec(&report).unwrap(), &image).unwrap();
        let read = |address: u32, length: usize| -> Result<Vec<u8>, String> {
            let offset = (address - base) as usize;
            Ok(image[offset..offset + length].to_vec())
        };
        assert!(binding.validate_live(callback | 1, read).is_ok());
        assert!(binding.validate_live((callback + 2) | 1, read).is_err());
        assert!(
            binding
                .validate_live(callback | 1, |_, n| Ok(vec![0xff; n]))
                .is_err()
        );
        let mut missing = report.clone();
        missing["patches"][0]
            .as_object_mut()
            .unwrap()
            .remove("knob_callback");
        assert!(
            FirmwareBinding::from_fixture(&serde_json::to_vec(&missing).unwrap(), &image).is_err()
        );
        let mut outside = report.clone();
        outside["patches"][0]["knob_callback"] = serde_json::json!(payload + 1024);
        assert!(
            FirmwareBinding::from_fixture(&serde_json::to_vec(&outside).unwrap(), &image).is_err()
        );
        image[0] ^= 1;
        assert!(
            FirmwareBinding::from_fixture(&serde_json::to_vec(&report).unwrap(), &image).is_err()
        );
    }
    #[test]
    #[ignore = "requires L6_KNOB_SD_REPORT and L6_KNOB_SD_MODULE"]
    fn real_sd_fixture_binds_hook_and_callback_and_rejects_mutation() {
        let report = std::fs::read(std::env::var("L6_KNOB_SD_REPORT").unwrap()).unwrap();
        let module = std::fs::read(std::env::var("L6_KNOB_SD_MODULE").unwrap()).unwrap();
        let binding = SdBinding::from_fixture(&report, &module).unwrap();
        let report_value: serde_json::Value = serde_json::from_slice(&report).unwrap();
        assert_eq!(
            u64::from(binding.hook_offset),
            report_value["patches"][0]["exports"]["__l6_dynamic_call_post_knob_changed"]
                .as_u64()
                .unwrap()
        );
        assert_eq!(
            u64::from(binding.callback_offset),
            report_value["patches"][0]["exports"]["knob_dialog_display_tick"]
                .as_u64()
                .unwrap()
        );
        let decoded = Module::decode(&module).unwrap();
        let base = 0x8100_0000u32;
        let site = crate::sd_loader::Site {
            atomic: false,
            registry: 0x8020_1000,
            index: 0,
            record: 0x8020_1020,
            handler: (base + binding.hook_offset) | 1,
            first: 0,
            last: 0,
            count: 1,
            links: vec![crate::sd_loader::Link {
                entry: (base + binding.hook_offset) | 1,
                next_slot: base,
                order: 0,
                module_base: base,
                next: 0,
                site: 0,
            }],
        };
        let read = |address: u32, length: usize| -> Result<Vec<u8>, String> {
            let offset = (address - base) as usize;
            Ok(decoded
                .code
                .get(offset..offset + length)
                .ok_or("outside fixture")?
                .to_vec())
        };
        assert_eq!(
            binding.verify_site(&site, read).unwrap(),
            (base + binding.callback_offset) | 1
        );
        let mut wrong = decoded.code.to_vec();
        wrong[100] ^= 1;
        assert!(
            binding
                .verify_site(&site, |address, length| {
                    let offset = (address - base) as usize;
                    Ok(wrong[offset..offset + length].to_vec())
                })
                .is_err()
        );
        let mut altered = module.clone();
        altered[0] ^= 1;
        assert!(SdBinding::from_fixture(&report, &altered).is_err());
        let mut report: serde_json::Value = serde_json::from_slice(&report).unwrap();
        report["patches"][0]["exports"]["knob_dialog_display_tick"] = serde_json::json!(u32::MAX);
        assert!(SdBinding::from_fixture(&serde_json::to_vec(&report).unwrap(), &module).is_err());
    }
}
