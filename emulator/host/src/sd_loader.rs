//! Bounded read-only inspection of the model-scoped SD loader registry.
//! The guest keeps running: snapshots combine separate QMP reads and are not atomic.
use crate::qmp::QmpClient;
use l6fw::Model;
use patch_abi::{dynamic, registry};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    time::{Duration, Instant},
};

const L6_GUARDS: [(u32, &str); 2] = [
    (
        0x8000_5ed8,
        "6f9448a58dc770e74e085a0972a4a7252cd9b0d6c2360ba868b2e6364a1678b5",
    ),
    (
        0x8000_8330,
        "90da986822a1402afa10333215ce410fdb0fa1ad8d27fb698e3338ac3e791b18",
    ),
];
const MAX_GUARDS: [(u32, &str); 2] = [
    (
        0x8002_4ed0,
        "9a7023da16d1852350a9598799deb20e305f972219d2c0eb15ac10120d05f0ee",
    ),
    (
        0x8000_70d8,
        "1a01d3af4cef1eca54b6b27bd912f5dc96b5e6dd600700ed92967aa044370434",
    ),
];
const SDRAM_START: u32 = 0x8000_0000;
const EXTERNAL_RAM_START: u32 = 0x8020_0000;
const SDRAM_END: u32 = 0x8420_0000;
const MAX_LINKS: u32 = registry::MAX_MODULES * dynamic::MAX_HOOKS_PER_MODULE as u32;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    pub revision_guard: String,
    pub atomic: bool,
    /// None means the model-specific root still contains its stock sentinel.
    pub registry: Option<u32>,
    pub stage: Option<u32>,
    pub claim: Option<u32>,
    pub module_base: Option<u32>,
    pub module_len: Option<u32>,
    pub site_count: Option<u32>,
}
/// Minimal readiness observation; unlike a complete snapshot it does not
/// decode module or site metadata while installation is still in progress.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Readiness {
    pub atomic: bool,
    pub registry: Option<u32>,
    pub claim: Option<u32>,
    pub stage: Option<u32>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Link {
    pub entry: u32,
    pub next_slot: u32,
    pub order: u32,
    pub module_base: u32,
    pub next: u32,
    pub site: u32,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Site {
    pub atomic: bool,
    pub registry: u32,
    pub index: u32,
    pub record: u32,
    pub handler: u32,
    pub first: u32,
    pub last: u32,
    pub count: u32,
    pub links: Vec<Link>,
}

fn checked(address: u32, length: usize) -> Result<u32, String> {
    let end = address
        .checked_add(u32::try_from(length).map_err(|_| "invalid SD read length")?)
        .ok_or("SD address overflow")?;
    if address < SDRAM_START || end > SDRAM_END {
        return Err(format!(
            "SD registry read outside MAIN memory: {address:#010x}"
        ));
    }
    Ok(address)
}
fn checked_chunk(address: u32, length: usize) -> Result<u32, String> {
    if length == 0 || length > 4096 {
        return Err("SD inspection chunk must be 1..4096 bytes".into());
    }
    checked_external(address, length)
}
fn checked_external(address: u32, length: usize) -> Result<u32, String> {
    if address < EXTERNAL_RAM_START {
        return Err(format!(
            "SD heap address outside external RAM: {address:#010x}"
        ));
    }
    checked(address, length)
}
fn word(
    read: &mut impl FnMut(u32, usize) -> Result<Vec<u8>, String>,
    address: u32,
) -> Result<u32, String> {
    Ok(u32::from_le_bytes(
        read(checked(address, 4)?, 4)?
            .try_into()
            .map_err(|_| "short SD registry read")?,
    ))
}
fn verify(
    model: Model,
    read: &mut impl FnMut(u32, usize) -> Result<Vec<u8>, String>,
) -> Result<(), String> {
    let guards = match model {
        Model::L6 => &L6_GUARDS,
        Model::L6max => &MAX_GUARDS,
    };
    for (address, digest) in guards {
        let actual = read(*address, 64)?;
        if actual.len() != 64 || format!("{:x}", Sha256::digest(&actual)) != *digest {
            return Err(format!(
                "unsupported {} firmware revision at {address:#010x}",
                model.name()
            ));
        }
    }
    Ok(())
}
fn read_readiness(
    model: Model,
    read: &mut impl FnMut(u32, usize) -> Result<Vec<u8>, String>,
) -> Result<Readiness, String> {
    let (root, stock) = match model {
        Model::L6 => (registry::L6_POINTER, registry::L6_POINTER_STOCK),
        Model::L6max => (registry::L6MAX_POINTER, registry::L6MAX_POINTER_STOCK),
    };
    let base = word(read, root)?;
    let mut readiness = Readiness {
        atomic: false,
        registry: None,
        claim: None,
        stage: None,
    };
    if base == stock {
        return Ok(readiness);
    }
    checked_external(base, 12)?;
    let magic = word(read, base)?;
    if magic != registry::MAGIC {
        return Err(format!(
            "SD registry at {base:#010x} has invalid magic {magic:#010x}"
        ));
    }
    readiness.registry = Some(base);
    readiness.claim = Some(word(read, base + registry::CLAIM_OFFSET)?);
    readiness.stage = Some(word(read, base + registry::DEBUG_OFFSET)?);
    Ok(readiness)
}
fn read_snapshot(
    model: Model,
    read: &mut impl FnMut(u32, usize) -> Result<Vec<u8>, String>,
) -> Result<Snapshot, String> {
    let readiness = read_readiness(model, read)?;
    let mut snapshot = Snapshot {
        revision_guard: format!("{}_1_10", model.name()),
        atomic: false,
        registry: readiness.registry,
        stage: readiness.stage,
        claim: readiness.claim,
        module_base: None,
        module_len: None,
        site_count: None,
    };
    let Some(base) = readiness.registry else {
        return Ok(snapshot);
    };
    checked_external(base, registry::SITES_OFFSET as usize)?;
    let count = word(read, base + registry::SITE_COUNT_OFFSET)?;
    if count > registry::MAX_SITES {
        return Err(format!(
            "SD registry site count {count} exceeds {}",
            registry::MAX_SITES
        ));
    }
    checked_external(base, registry::len(count) as usize)?;
    snapshot.module_base = Some(word(read, base + registry::MODULE_PTR_OFFSET)?);
    snapshot.module_len = Some(word(read, base + registry::MODULE_LEN_OFFSET)?);
    if let (Some(module), Some(length)) = (snapshot.module_base, snapshot.module_len) {
        if module != 0 {
            if length as usize > dynamic::MAX_MEMORY_LEN {
                return Err("SD module length exceeds loader ABI limit".into());
            }
            checked_external(module, (length as usize).max(1))?;
        }
    }
    snapshot.site_count = Some(count);
    Ok(snapshot)
}
fn read_site(
    snapshot: &Snapshot,
    index: u32,
    read: &mut impl FnMut(u32, usize) -> Result<Vec<u8>, String>,
) -> Result<Site, String> {
    let base = snapshot
        .registry
        .ok_or("SD loader registry is not allocated")?;
    if index >= snapshot.site_count.ok_or("SD loader site count missing")? {
        return Err("SD loader site index out of range".into());
    }
    let record = base
        .checked_add(registry::site_offset(index))
        .ok_or("SD site address overflow")?;
    let handler = word(read, record)?;
    let first = word(read, record + 4)?;
    let last = word(read, record + 8)?;
    let count = word(read, record + 12)?;
    if count > MAX_LINKS {
        return Err("SD hook chain exceeds loader ABI limit".into());
    }
    let mut links = Vec::new();
    let mut seen = BTreeSet::new();
    let mut next = first;
    let mut final_address = 0;
    while next != 0 {
        if links.len() >= count as usize || !seen.insert(next) {
            return Err("SD hook chain is cyclic or exceeds declared count".into());
        }
        checked_external(next, 24)?;
        if next & 3 != 0 {
            return Err("unaligned SD hook link".into());
        }
        final_address = next;
        let link = Link {
            entry: word(read, next)?,
            next_slot: word(read, next + 4)?,
            order: word(read, next + 8)?,
            module_base: word(read, next + 12)?,
            next: word(read, next + 16)?,
            site: word(read, next + 20)?,
        };
        if link.site != index || link.entry & 1 == 0 || link.next_slot & 3 != 0 {
            return Err("invalid SD hook link identity or alignment".into());
        }
        checked_external(link.entry & !1, 2)?;
        checked_external(link.next_slot, 4)?;
        checked_external(link.module_base, 1)?;
        next = link.next;
        links.push(link);
    }
    if links.len() != count as usize
        || (count == 0 && (first != 0 || last != 0 || handler != 0))
        || (count != 0 && (last != final_address || handler != links[0].entry))
    {
        return Err(format!(
            "SD hook site {index} has inconsistent chain metadata"
        ));
    }
    Ok(Site {
        atomic: false,
        registry: base,
        index,
        record,
        handler,
        first,
        last,
        count,
        links,
    })
}
/// Verify firmware identity once for an uninterrupted sequence of observations.
pub struct Reader<'a> {
    qmp: &'a QmpClient,
    model: Model,
}
fn bounded_read<T>(
    deadline: Instant,
    cancelled: &mut impl FnMut() -> bool,
    read: impl FnOnce() -> Result<T, String>,
) -> Result<T, String> {
    if cancelled() || Instant::now() >= deadline {
        return Err("SD loader inspection deadline or cancellation".into());
    }
    let value = read()?;
    if cancelled() || Instant::now() >= deadline {
        return Err("SD loader inspection deadline or cancellation".into());
    }
    Ok(value)
}
impl<'a> Reader<'a> {
    pub fn new(qmp: &'a QmpClient, model: Model, deadline: Instant) -> Result<Self, String> {
        Self::new_cancellable(qmp, model, deadline, || false)
    }
    pub fn new_cancellable(
        qmp: &'a QmpClient,
        model: Model,
        deadline: Instant,
        mut cancelled: impl FnMut() -> bool,
    ) -> Result<Self, String> {
        verify(model, &mut |a, n| {
            bounded_read(deadline, &mut cancelled, || {
                qmp.read_memory_before(0, a, n, deadline)
                    .map_err(|e| e.to_string())
            })
        })?;
        Ok(Self { qmp, model })
    }
    pub fn snapshot(&self, deadline: Instant) -> Result<Snapshot, String> {
        self.snapshot_cancellable(deadline, || false)
    }
    pub fn readiness_cancellable(
        &self,
        deadline: Instant,
        mut cancelled: impl FnMut() -> bool,
    ) -> Result<Readiness, String> {
        read_readiness(self.model, &mut |a, n| {
            self.read_checked(a, n, deadline, &mut cancelled)
        })
    }
    pub fn snapshot_cancellable(
        &self,
        deadline: Instant,
        mut cancelled: impl FnMut() -> bool,
    ) -> Result<Snapshot, String> {
        read_snapshot(self.model, &mut |a, n| {
            self.read_checked(a, n, deadline, &mut cancelled)
        })
    }
    pub fn site(&self, index: u32, deadline: Instant) -> Result<Site, String> {
        self.site_cancellable(index, deadline, || false)
    }
    pub fn site_cancellable(
        &self,
        index: u32,
        deadline: Instant,
        mut cancelled: impl FnMut() -> bool,
    ) -> Result<Site, String> {
        let snapshot = self.snapshot_cancellable(deadline, &mut cancelled)?;
        read_site(&snapshot, index, &mut |a, n| {
            self.read_checked(a, n, deadline, &mut cancelled)
        })
    }
    /// Bounded in-process external-RAM read for a guarded SD inspection.
    /// QEMU maps RAM at 0x80200000..0x84200000; this is not a socket command.
    pub fn read_chunk_cancellable(
        &self,
        address: u32,
        length: usize,
        deadline: Instant,
        mut cancelled: impl FnMut() -> bool,
    ) -> Result<Vec<u8>, String> {
        checked_chunk(address, length)?;
        self.read_checked(address, length, deadline, &mut cancelled)
    }
    fn read_checked(
        &self,
        address: u32,
        length: usize,
        deadline: Instant,
        cancelled: &mut impl FnMut() -> bool,
    ) -> Result<Vec<u8>, String> {
        bounded_read(deadline, cancelled, || {
            self.qmp
                .read_memory_before(0, address, length, deadline)
                .map_err(|e| e.to_string())
        })
    }
}
pub fn snapshot(qmp: &QmpClient, model: Model) -> Result<Snapshot, String> {
    let deadline = Instant::now() + Duration::from_secs(30);
    Reader::new(qmp, model, deadline)?.snapshot(deadline)
}
pub fn site(qmp: &QmpClient, model: Model, index: u32) -> Result<Site, String> {
    let deadline = Instant::now() + Duration::from_secs(30);
    Reader::new(qmp, model, deadline)?.site(index, deadline)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn guard_read_rejects_cancellation_while_read_is_in_flight() {
        use std::cell::Cell;
        let cancelled = Cell::new(false);
        let mut check = || cancelled.get();
        let result = bounded_read(Instant::now() + Duration::from_secs(1), &mut check, || {
            cancelled.set(true);
            Ok(vec![1u8])
        });
        assert!(result.is_err());
    }
    use std::collections::BTreeMap;
    fn put(map: &mut BTreeMap<u32, Vec<u8>>, address: u32, value: u32) {
        map.insert(address, value.to_le_bytes().to_vec());
    }
    fn reader<'a>(
        map: &'a BTreeMap<u32, Vec<u8>>,
    ) -> impl FnMut(u32, usize) -> Result<Vec<u8>, String> + 'a {
        move |address, length| {
            map.get(&address)
                .filter(|bytes| bytes.len() == length)
                .cloned()
                .ok_or_else(|| format!("missing {address:#x}"))
        }
    }
    fn fixture(model: Model) -> BTreeMap<u32, Vec<u8>> {
        let mut map = BTreeMap::new();
        let root = if model == Model::L6 {
            registry::L6_POINTER
        } else {
            registry::L6MAX_POINTER
        };
        let base = 0x8080_1000;
        for (address, value) in [
            (root, base),
            (base, registry::MAGIC),
            (base + 4, 0x400),
            (base + 8, 1),
            (base + 160, 0x8081_0000),
            (base + 164, 300),
            (base + 168, 2),
        ] {
            put(&mut map, address, value);
        }
        let site = base + registry::site_offset(1);
        let link = 0x8082_0000;
        for (address, value) in [
            (site, 0x8081_0001),
            (site + 4, link),
            (site + 8, link),
            (site + 12, 1),
            (link, 0x8081_0001),
            (link + 4, 0x8081_0010),
            (link + 8, 0),
            (link + 12, 0x8081_0000),
            (link + 16, 0),
            (link + 20, 1),
        ] {
            put(&mut map, address, value);
        }
        map
    }
    #[test]
    fn supports_both_model_roots_and_unallocated_sentinels() {
        for model in [Model::L6, Model::L6max] {
            let map = fixture(model);
            let snapshot = read_snapshot(model, &mut reader(&map)).unwrap();
            assert!(!snapshot.atomic);
            assert_eq!(snapshot.claim, Some(1));
            let site = read_site(&snapshot, 1, &mut reader(&map)).unwrap();
            assert_eq!(site.count, 1);
            assert_eq!(site.links[0].entry, 0x8081_0001);
            assert!(read_site(&snapshot, 2, &mut reader(&map)).is_err());
            let (root, stock) = match model {
                Model::L6 => (registry::L6_POINTER, registry::L6_POINTER_STOCK),
                Model::L6max => (registry::L6MAX_POINTER, registry::L6MAX_POINTER_STOCK),
            };
            let mut absent = BTreeMap::new();
            put(&mut absent, root, stock);
            assert_eq!(
                read_snapshot(model, &mut reader(&absent)).unwrap().registry,
                None
            );
        }
    }
    #[test]
    fn rejects_bad_pointers_magic_counts_short_reads_and_cycles() {
        let mut map = fixture(Model::L6);
        put(&mut map, registry::L6_POINTER, 0xfffffff0);
        assert!(read_snapshot(Model::L6, &mut reader(&map)).is_err());
        let mut map = fixture(Model::L6);
        put(&mut map, registry::L6_POINTER, 0x801f_1000);
        assert!(read_snapshot(Model::L6, &mut reader(&map)).is_err());
        let mut map = fixture(Model::L6);
        put(&mut map, 0x8080_1000, 0);
        assert!(read_snapshot(Model::L6, &mut reader(&map)).is_err());
        let mut map = fixture(Model::L6);
        put(&mut map, 0x8080_1000 + 168, registry::MAX_SITES + 1);
        assert!(read_snapshot(Model::L6, &mut reader(&map)).is_err());
        let mut map = fixture(Model::L6);
        map.remove(&(0x8080_1000 + 160));
        assert!(read_snapshot(Model::L6, &mut reader(&map)).is_err());
        let mut map = fixture(Model::L6);
        put(&mut map, 0x8082_0000 + 16, 0x8082_0000);
        let snapshot = read_snapshot(Model::L6, &mut reader(&map)).unwrap();
        assert!(read_site(&snapshot, 1, &mut reader(&map)).is_err());
        let mut map = fixture(Model::L6);
        put(
            &mut map,
            0x8080_1000 + registry::site_offset(1) + 8,
            0x8082_0018,
        );
        let snapshot = read_snapshot(Model::L6, &mut reader(&map)).unwrap();
        assert!(read_site(&snapshot, 1, &mut reader(&map)).is_err());
        let mut map = fixture(Model::L6);
        put(&mut map, 0x8082_0000 + 20, 0);
        let snapshot = read_snapshot(Model::L6, &mut reader(&map)).unwrap();
        assert!(read_site(&snapshot, 1, &mut reader(&map)).is_err());
        let mut map = fixture(Model::L6);
        put(&mut map, 0x8082_0000 + 4, 0x801f_1000);
        let snapshot = read_snapshot(Model::L6, &mut reader(&map)).unwrap();
        assert!(read_site(&snapshot, 1, &mut reader(&map)).is_err());
        assert!(verify(Model::L6, &mut |_, n| Ok(vec![0; n])).is_err());
    }
    #[test]
    fn inspection_chunks_cover_the_mapped_ram_boundary_without_overflow() {
        assert!(checked_chunk(0x8020_0000, 1).is_ok());
        assert!(checked_chunk(0x841f_ffff, 1).is_ok());
        assert!(checked_chunk(0x841f_f000, 4096).is_ok());
        for (address, length) in [
            (0x8420_0000, 1),
            (0x841f_ffff, 2),
            (0x801f_ffff, 1),
            (0x8020_0000, 0),
            (0x8020_0000, 4097),
            (u32::MAX, 2),
        ] {
            assert!(checked_chunk(address, length).is_err());
        }
    }
    #[test]
    fn readiness_does_not_decode_in_progress_metadata_but_completed_snapshot_rejects_it() {
        let mut map = fixture(Model::L6);
        put(&mut map, 0x8080_1000 + registry::CLAIM_OFFSET, 0);
        put(
            &mut map,
            0x8080_1000 + registry::SITE_COUNT_OFFSET,
            registry::MAX_SITES + 1,
        );
        let pending = read_readiness(Model::L6, &mut reader(&map)).unwrap();
        assert_eq!(pending.claim, Some(0));
        put(&mut map, 0x8080_1000 + registry::CLAIM_OFFSET, 1);
        let complete = read_readiness(Model::L6, &mut reader(&map)).unwrap();
        assert_eq!(complete.claim, Some(1));
        assert!(read_snapshot(Model::L6, &mut reader(&map)).is_err());
    }
    #[test]
    #[ignore = "requires unpacked MAIN in SD_LOADER_TEST_MAIN and SD_LOADER_TEST_MODEL"]
    fn production_guards_match_real_main() {
        let image = std::fs::read(std::env::var_os("SD_LOADER_TEST_MAIN").unwrap()).unwrap();
        let model = match std::env::var("SD_LOADER_TEST_MODEL").unwrap().as_str() {
            "l6" => Model::L6,
            "l6max" => Model::L6max,
            _ => panic!("invalid model"),
        };
        verify(model, &mut |address, length| {
            let offset = address
                .checked_sub(model.main_base())
                .ok_or("before image")? as usize;
            image
                .get(offset..offset + length)
                .map(|b| b.to_vec())
                .ok_or_else(|| "short image".into())
        })
        .unwrap();
    }
}
