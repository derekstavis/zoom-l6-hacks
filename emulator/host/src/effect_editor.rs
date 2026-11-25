//! Read-only, module-bound effect editor mailbox and counters.
use crate::qmp::QmpClient;
use l6fw::Model;
use patch_abi::{dynamic::Module, registry};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    time::{Duration, Instant},
};

// QEMU maps the 64 MiB external RAM after the separate MAIN flash window.
const START: u32 = 0x8020_0000;
const END: u32 = 0x8420_0000;

fn external_ram_span(address: u32, length: u32) -> bool {
    length != 0 && address >= START && address.checked_add(length).is_some_and(|end| end <= END)
}
const GUARD_LEN: usize = 32;

/// Offsets derived from one decoded composed module and its own build report.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Binding {
    pub module_len: u32,
    pub code_len: u32,
    pub state_offset: u32,
    pub hook_offset: u32,
    pub hook_guard: [u8; GUARD_LEN],
    pub immutable_sha256: String,
    pub masked_words: Vec<u32>,
    pub applied_offset: u32,
    pub dropped_offset: u32,
    pub pending_offset: u32,
    pub gate_offset: u32,
    pub fixture_module_sha256: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct Snapshot {
    pub revision_guard: String,
    pub fixture_module_sha256: String,
    pub atomic: bool,
    pub applied: u32,
    pub dropped: u32,
    pub pending: u32,
    pub gate: u32,
}
impl Binding {
    pub fn from_fixture(report: &[u8], bytes: &[u8]) -> Result<Self, String> {
        let report: serde_json::Value =
            serde_json::from_slice(report).map_err(|e| e.to_string())?;
        let patches = report["patches"]
            .as_array()
            .ok_or("module report has no patches")?;
        if patches.len() != 1 {
            return Err("effect editor requires one composed SD module".into());
        }
        let patch = &patches[0];
        if !patch["members"]
            .as_array()
            .is_some_and(|m| m.iter().any(|v| v.as_str() == Some("efx-loader")))
            || patch["file_length"].as_u64() != Some(bytes.len() as u64)
        {
            return Err("module report does not describe the effect loader".into());
        }
        let module = Module::decode(bytes).map_err(|e| format!("invalid SD module: {e:?}"))?;
        if patch["memory_length"].as_u64() != Some(module.memory_len as u64)
            || patch["payload_length"].as_u64() != Some(module.code.len() as u64)
        {
            return Err("module report length mismatch".into());
        }
        let symbol = |name: &str| -> Result<u32, String> {
            patch["exports"][name]
                .as_u64()
                .and_then(|v| u32::try_from(v).ok())
                .ok_or_else(|| format!("missing effect loader symbol {name}"))
        };
        let hook_offset = symbol("effect_replacements_encoder_event")?;
        if !module.hooks.iter().any(|h| {
            h.address == firmware_api::l6max::mixer_apply_channel_parameter
                && h.entry_offset == hook_offset
        }) {
            return Err("effect encoder hook is absent from module".into());
        }
        let start = hook_offset as usize;
        let end = start
            .checked_add(GUARD_LEN)
            .ok_or("effect hook offset overflow")?;
        let guard: [u8; GUARD_LEN] = module
            .code
            .get(start..end)
            .ok_or("effect hook guard outside code")?
            .try_into()
            .unwrap();
        for relocation in &module.relocations {
            let [kind, first, second] = relocation.record();
            for offset in [Some(first), (kind == 2).then_some(second)]
                .into_iter()
                .flatten()
            {
                let r = offset as usize;
                if r < end && r.saturating_add(4) > start {
                    return Err("effect hook guard overlaps a relocation".into());
                }
            }
        }
        let state_offset = patch["writable_data"]["offset"]
            .as_u64()
            .and_then(|v| u32::try_from(v).ok())
            .ok_or("effect writable state missing")?;
        let mut mask = BTreeSet::new();
        for relocation in &module.relocations {
            let [kind, first, second] = relocation.record();
            for offset in [Some(first), (kind == 2).then_some(second)]
                .into_iter()
                .flatten()
            {
                if offset < state_offset {
                    mask.insert(offset);
                }
            }
        }
        for hook in &module.hooks {
            if hook.next_slot_offset != u32::MAX && hook.next_slot_offset < state_offset {
                mask.insert(hook.next_slot_offset);
            }
        }
        let masked_words: Vec<_> = mask.into_iter().collect();
        let mut immutable = module
            .code
            .get(..state_offset as usize)
            .ok_or("effect state starts outside module code")?
            .to_vec();
        for offset in &masked_words {
            let start = *offset as usize;
            immutable
                .get_mut(start..start.checked_add(4).ok_or("mask overflow")?)
                .ok_or("masked word crosses effect state")?
                .fill(0);
        }
        let immutable_sha256 = format!("{:x}", Sha256::digest(&immutable));
        let result = Self {
            module_len: u32::try_from(module.memory_len).map_err(|_| "module too large")?,
            code_len: u32::try_from(module.code.len()).map_err(|_| "module too large")?,
            state_offset,
            hook_offset,
            hook_guard: guard,
            immutable_sha256,
            masked_words,
            applied_offset: symbol("DUAL_EFFECT_EDIT_APPLIED")?,
            dropped_offset: symbol("DUAL_EFFECT_EDIT_DROPPED")?,
            pending_offset: symbol("DUAL_EFFECT_EDIT_EVENT")?,
            gate_offset: symbol("DUAL_EFFECT_CALLBACK_GATE")?,
            fixture_module_sha256: format!("{:x}", Sha256::digest(bytes)),
        };
        result.validate()?;
        Ok(result)
    }
    fn validate(&self) -> Result<(), String> {
        if self.module_len == 0
            || self.module_len > patch_abi::dynamic::MAX_MEMORY_LEN as u32
            || self.code_len > self.module_len
            || self.state_offset > self.code_len
            || self.state_offset > 262_144
            || self.immutable_sha256.len() != 64
            || !self.immutable_sha256.bytes().all(|b| b.is_ascii_hexdigit())
            || self.masked_words.len() > 512
            || self.masked_words.windows(2).any(|w| w[0] >= w[1])
            || self
                .masked_words
                .iter()
                .any(|o| o.checked_add(4).is_none_or(|end| end > self.state_offset))
            || self
                .hook_offset
                .checked_add(GUARD_LEN as u32)
                .is_none_or(|end| end > self.state_offset)
            || self
                .hook_offset
                .checked_add(GUARD_LEN as u32)
                .is_none_or(|v| v > self.code_len)
            || self.fixture_module_sha256.len() != 64
            || !self
                .fixture_module_sha256
                .bytes()
                .all(|b| b.is_ascii_hexdigit())
        {
            return Err("invalid effect module binding".into());
        }
        let offsets = [
            self.applied_offset,
            self.dropped_offset,
            self.pending_offset,
            self.gate_offset,
        ];
        if offsets.into_iter().collect::<BTreeSet<_>>().len() != 4
            || offsets.into_iter().any(|o| {
                o % 4 != 0
                    || o < self.state_offset
                    || o.checked_add(4).is_none_or(|v| v > self.module_len)
            })
        {
            return Err("effect counter offsets outside module state".into());
        }
        Ok(())
    }
}

fn checked_read<T>(
    check: &dyn Fn() -> Result<(), String>,
    read: impl FnOnce() -> Result<T, String>,
) -> Result<T, String> {
    check()?;
    let result = read()?;
    check()?;
    Ok(result)
}

pub struct Reader {
    qmp: QmpClient,
    binding: Binding,
    module: u32,
    registry: u32,
}
impl Reader {
    pub fn new(qmp: &QmpClient, model: Model, binding: Binding) -> Result<Self, String> {
        Self::new_before(
            qmp,
            model,
            binding,
            Instant::now() + Duration::from_secs(30),
        )
    }
    pub fn new_before(
        qmp: &QmpClient,
        model: Model,
        binding: Binding,
        deadline: Instant,
    ) -> Result<Self, String> {
        Self::new_before_checked(qmp, model, binding, deadline, &|| Ok(()))
    }
    pub fn new_before_checked(
        qmp: &QmpClient,
        model: Model,
        binding: Binding,
        deadline: Instant,
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<Self, String> {
        binding.validate()?;
        if model != Model::L6max {
            return Err("effect editor requires L6max 1.10".into());
        }
        let read = |address, length| {
            checked_read(check, || {
                qmp.read_memory_before(0, address, length, deadline)
                    .map_err(|e| e.to_string())
            })
        };
        for (address, expected) in [
            (
                0x8002_4ed0,
                "9a7023da16d1852350a9598799deb20e305f972219d2c0eb15ac10120d05f0ee",
            ),
            (
                0x8000_70d8,
                "1a01d3af4cef1eca54b6b27bd912f5dc96b5e6dd600700ed92967aa044370434",
            ),
        ] {
            let bytes = read(address, 64)?;
            if format!("{:x}", Sha256::digest(bytes)) != expected {
                return Err("unsupported L6max firmware revision".into());
            }
        }
        let read_word = |address| -> Result<u32, String> {
            Ok(u32::from_le_bytes(
                read(address, 4)?
                    .try_into()
                    .map_err(|_| "short effect registry word")?,
            ))
        };
        let root = read_word(registry::L6MAX_POINTER)?;
        if !external_ram_span(root, registry::SITE_COUNT_OFFSET + 4)
            || read_word(root)? != registry::MAGIC
        {
            return Err("invalid effect loader registry".into());
        }
        let module = read_word(root + registry::MODULE_PTR_OFFSET)?;
        let length = read_word(root + registry::MODULE_LEN_OFFSET)?;
        if length != binding.module_len || !external_ram_span(module, length) {
            return Err("effect loader module bounds do not match fixture".into());
        }
        let actual = read(module + binding.hook_offset, GUARD_LEN)?;
        if actual != binding.hook_guard {
            return Err("loaded effect hook does not match fixture".into());
        }
        let mut immutable = Vec::with_capacity(binding.state_offset as usize);
        let mut offset = 0u32;
        while offset < binding.state_offset {
            let length = (binding.state_offset - offset).min(4096) as usize;
            let chunk = read(module + offset, length)?;
            if chunk.len() != length {
                return Err("short effect module code read".into());
            }
            immutable.extend(chunk);
            offset += length as u32;
        }
        for offset in &binding.masked_words {
            immutable[*offset as usize..*offset as usize + 4].fill(0);
        }
        if format!("{:x}", Sha256::digest(&immutable)) != binding.immutable_sha256 {
            return Err("loaded effect module code does not match fixture".into());
        }
        Ok(Self {
            qmp: qmp.clone(),
            binding,
            module,
            registry: root,
        })
    }
    pub fn read(&self) -> Result<Snapshot, String> {
        self.read_before(Instant::now() + Duration::from_secs(30))
    }
    pub fn read_before(&self, deadline: Instant) -> Result<Snapshot, String> {
        self.read_before_checked(deadline, &|| Ok(()))
    }
    pub fn read_before_checked(
        &self,
        deadline: Instant,
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<Snapshot, String> {
        let read = |address, length| {
            checked_read(check, || {
                self.qmp
                    .read_memory_before(0, address, length, deadline)
                    .map_err(|e| e.to_string())
            })
        };
        let read_address = |address| -> Result<u32, String> {
            Ok(u32::from_le_bytes(
                read(address, 4)?
                    .try_into()
                    .map_err(|_| "short effect registry word")?,
            ))
        };
        if read_address(registry::L6MAX_POINTER)? != self.registry
            || read_address(self.registry)? != registry::MAGIC
            || read_address(self.registry + registry::MODULE_PTR_OFFSET)? != self.module
            || read_address(self.registry + registry::MODULE_LEN_OFFSET)? != self.binding.module_len
        {
            return Err("effect loader module changed during inspection".into());
        }
        let hook = read(self.module + self.binding.hook_offset, GUARD_LEN)?;
        if hook != self.binding.hook_guard {
            return Err("effect loader hook changed during inspection".into());
        }
        let word = |offset| -> Result<u32, String> {
            Ok(u32::from_le_bytes(
                read(self.module + offset, 4)?
                    .try_into()
                    .map_err(|_| "short effect counter")?,
            ))
        };
        Ok(Snapshot {
            revision_guard: "l6max_1_10_fixture_hook".into(),
            fixture_module_sha256: self.binding.fixture_module_sha256.clone(),
            atomic: false,
            applied: word(self.binding.applied_offset)?,
            dropped: word(self.binding.dropped_offset)?,
            pending: word(self.binding.pending_offset)?,
            gate: word(self.binding.gate_offset)?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn checked_read_cancels_before_and_after_each_operation() {
        use std::cell::Cell;
        let checks = Cell::new(0);
        let reads = Cell::new(0);
        let check = || {
            checks.set(checks.get() + 1);
            if checks.get() >= 4 {
                Err("guest deadline".into())
            } else {
                Ok(())
            }
        };
        let read = || {
            reads.set(reads.get() + 1);
            Ok(())
        };
        assert!(checked_read(&check, read).is_ok());
        assert_eq!(checked_read(&check, read).unwrap_err(), "guest deadline");
        assert_eq!(reads.get(), 2);
        assert_eq!(checked_read(&check, read).unwrap_err(), "guest deadline");
        assert_eq!(reads.get(), 2);
    }
    #[test]
    fn external_ram_bounds_reject_flash_end_crossing_and_overflow() {
        assert!(external_ram_span(0x8020_0000, 1));
        assert!(external_ram_span(0x841f_ffff, 1));
        assert!(external_ram_span(0x841f_f000, 4096));
        assert!(!external_ram_span(0x8000_0000, 4));
        assert!(!external_ram_span(0x801f_ffff, 2));
        assert!(!external_ram_span(0x8420_0000, 1));
        assert!(!external_ram_span(0x841f_ffff, 2));
        assert!(!external_ram_span(u32::MAX, 2));
        assert!(!external_ram_span(0x8020_0000, 0));
    }
    #[test]
    fn rejects_unbounded_or_aliased_counter_bindings() {
        let mut binding = Binding {
            module_len: 512,
            code_len: 256,
            state_offset: 128,
            hook_offset: 0,
            hook_guard: [0; 32],
            immutable_sha256: "0".repeat(64),
            masked_words: vec![],
            applied_offset: 256,
            dropped_offset: 260,
            pending_offset: 264,
            gate_offset: 128,
            fixture_module_sha256: "0".repeat(64),
        };
        assert!(binding.validate().is_ok());
        binding.gate_offset = 264;
        assert!(binding.validate().is_err());
        binding.gate_offset = 512;
        assert!(binding.validate().is_err());
        binding.gate_offset = 128;
        binding.state_offset = 0;
        assert!(binding.validate().is_err());
    }
    #[test]
    #[ignore = "requires L6_EFFECT_REPORT and L6_EFFECT_MODULE fixture paths"]
    fn supported_composed_module_builds_guarded_binding() {
        let report = std::fs::read(std::env::var_os("L6_EFFECT_REPORT").unwrap()).unwrap();
        let module = std::fs::read(std::env::var_os("L6_EFFECT_MODULE").unwrap()).unwrap();
        let binding = Binding::from_fixture(&report, &module).unwrap();
        assert_ne!(binding.applied_offset, binding.pending_offset);
        let mut corrupt = module;
        corrupt[40] ^= 1;
        assert!(Binding::from_fixture(&report, &corrupt).is_err());
    }
}
