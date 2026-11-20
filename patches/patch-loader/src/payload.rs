#![no_std]
#![no_main]
//! Experimental Patch loader for L6 and L6max 1.10.
#[path = "../../api/patchrt.rs"]
#[macro_use]
pub mod patch_api;
use core::{arch::asm, ptr, slice};
use patch_api::{function_call_post_hook, function_offset_hook};
#[allow(dead_code, unused_attributes)]
#[path = "../../../tools/patch-loader-core/src/lib.rs"]
mod core_loader;

#[cfg(not(sd_l6))]
const IMAGE_BASE: usize = 0x8000_0000;
#[cfg(sd_l6)]
const IMAGE_BASE: usize = 0x8000_1000;
#[cfg(not(sd_l6))]
const IMAGE_LEN: usize = 0x107fe8;
#[cfg(sd_l6)]
const IMAGE_LEN: usize = 0xb5ae4;
#[cfg(not(sd_l6))]
const REGISTRY_POINTER: usize = core_loader::registry::L6MAX_POINTER as usize;
#[cfg(sd_l6)]
const REGISTRY_POINTER: usize = core_loader::registry::L6_POINTER as usize;
#[cfg(not(sd_l6))]
const REGISTRY_STOCK: u32 = core_loader::registry::L6MAX_POINTER_STOCK;
#[cfg(sd_l6)]
const REGISTRY_STOCK: u32 = core_loader::registry::L6_POINTER_STOCK;
// Cap a single code + zero-filled state allocation at 384 KiB. Larger modules
// are rejected before allocating from the shared FreeRTOS heap.
const MODULE_CAPACITY: usize = 0x60000;
/// Dynamic dispatch sites in this loader's hook catalog. The registry holds
/// one record per site, so no set of SD modules can outgrow it.
const SITES: usize = {
    let sites = hook_catalog::FUNCTION_HOOK_SITES;
    let mut count = 0;
    let mut index = 0;
    while index < sites.len() {
        if sites[index].dynamic_dispatch {
            count += 1;
        }
        index += 1;
    }
    count
};
const _: () = assert!(SITES as u32 <= core_loader::registry::MAX_SITES);
const REGISTRY_LEN: usize = core_loader::registry::len(SITES as u32) as usize;

/// The heap-allocated registry, or zero while the root still holds its stock
/// value (before the first card mount).
fn registry_base() -> usize {
    let root = unsafe { ptr::read_volatile(REGISTRY_POINTER as *const u32) };
    if root == REGISTRY_STOCK { 0 } else { root as usize }
}
fn registry_word(offset: u32) -> usize {
    registry_base() + offset as usize
}
fn debug_word() -> usize {
    registry_word(core_loader::registry::DEBUG_OFFSET)
}
fn claim_word() -> usize {
    registry_word(core_loader::registry::CLAIM_OFFSET)
}
fn module_ptr_word() -> usize {
    registry_word(core_loader::registry::MODULE_PTR_OFFSET)
}
fn module_len_word() -> usize {
    registry_word(core_loader::registry::MODULE_LEN_OFFSET)
}
fn site_record(index: usize) -> *mut core_loader::dynamic::SiteChain {
    registry_word(core_loader::registry::site_offset(index as u32))
        as *mut core_loader::dynamic::SiteChain
}
/// The catalog entry for dispatch site `index`.
fn dynamic_site(index: usize) -> Option<&'static patch_api::FunctionHookSite> {
    hook_catalog::FUNCTION_HOOK_SITES
        .iter()
        .filter(|site| site.dynamic_dispatch)
        .nth(index)
}
#[cfg(not(sd_l6))]
const SHA: [u8; 32] = [
    0x98, 0x0d, 0x92, 0x27, 0x2c, 0x1f, 0x84, 0xbe, 0xd2, 0x00, 0xb4, 0xd2, 0xf8, 0x35, 0x75, 0x20,
    0x2f, 0x6a, 0x00, 0x08, 0x2c, 0x92, 0x8d, 0x99, 0xb9, 0xe8, 0xb5, 0x3a, 0xf8, 0xd4, 0x44, 0x61,
];
#[cfg(sd_l6)]
const SHA: [u8; 32] = [
    0x82, 0x7c, 0x7c, 0x5c, 0xb2, 0xa4, 0x3c, 0xc3, 0xa0, 0x11, 0xd4, 0x55, 0x64, 0x62, 0xb8, 0xcb,
    0x95, 0x39, 0x24, 0x2f, 0x81, 0x7e, 0xbe, 0xed, 0x77, 0x27, 0xd6, 0x6a, 0x36, 0xc0, 0x37, 0x99,
];
#[cfg(not(sd_l6))]
#[path = "../../api/l6max/mod.rs"]
mod firmware_bindings;
#[cfg(sd_l6)]
#[path = "../../api/l6/mod.rs"]
mod firmware_bindings;
use firmware_bindings::*;
#[cfg(not(sd_l6))]
#[path = "../../api/l6max/hooks.rs"]
mod hook_catalog;
#[cfg(sd_l6)]
#[path = "../../api/l6/hooks.rs"]
mod hook_catalog;
const PATCH_PATTERN: [u16; 17] = [
    65, 58, 92, 80, 65, 84, 67, 72, 69, 83, 92, 42, 46, 66, 73, 78, 0,
];
const PATCH_DIRECTORY: [u16; 11] = [65, 58, 92, 80, 65, 84, 67, 72, 69, 83, 92];
const SEARCH_INFO_LEN: usize = 0x46c;
const SEARCH_NAME_OFFSET: usize = 0x12;
const SEARCH_ALT_NAME_OFFSET: usize = 0x2c;
const SEARCH_NAME_CAPACITY: usize = 256;
const MAX_PATCH_FILES: usize = 64;

#[cfg(not(sd_l6))]
const fn midi_hook_offset() -> usize {
    0x3f7c8
}
#[cfg(sd_l6)]
const fn midi_hook_offset() -> usize {
    0x2e490
}

/// Allocate the registry from the FreeRTOS heap on the first card mount and
/// publish it. Mount callers can race here, so the address is published only if
/// no other caller published one first; the loser frees its allocation.
unsafe fn ensure_registry() -> bool {
    let current = registry_base();
    if current != 0 {
        return unsafe { ptr::read_volatile(current as *const u32) } == core_loader::registry::MAGIC;
    }
    let len = REGISTRY_LEN;
    let fresh = unsafe { pvPortMalloc(len) };
    if fresh.is_null() {
        return false;
    }
    let base = fresh as usize;
    unsafe {
        ptr::write_bytes(fresh, 0, len);
        ptr::write_volatile(
            (base + core_loader::registry::SITE_COUNT_OFFSET as usize) as *mut u32,
            SITES as u32,
        );
        ptr::write_volatile(base as *mut u32, core_loader::registry::MAGIC);
        let primask: u32;
        asm!("dmb", "mrs {0}, PRIMASK", "cpsid i", out(reg) primask, options(nostack, preserves_flags));
        let published = ptr::read_volatile(REGISTRY_POINTER as *const u32) == REGISTRY_STOCK;
        if published {
            ptr::write_volatile(REGISTRY_POINTER as *mut u32, base as u32);
        }
        asm!("dmb", "msr PRIMASK, {0}", in(reg) primask, options(nostack, preserves_flags));
        if !published {
            vPortFree(fresh);
        }
    }
    true
}

/// Called from the early MAIN startup function on every reset. The firmware
/// rebuilds its FreeRTOS heap during reset initialization, and external RAM can
/// retain the old pointer across a warm reset, so forget the registry before
/// any dispatch stub or ordinary feature hook can run.
#[unsafe(no_mangle)]
#[unsafe(link_section = ".text.sd_reset_registry")]
pub unsafe extern "C" fn sd_reset_registry() {
    unsafe {
        ptr::write_volatile(REGISTRY_POINTER as *mut u32, REGISTRY_STOCK);
    }
}

// Explicit function-pointer conversions avoid loading data from the firmware address.
unsafe fn mount(drive: u32) -> i32 {
    unsafe { fs_mount(drive) }
}
unsafe fn ready() -> u32 {
    unsafe { fs_ready() }
}
unsafe fn open(handle: *mut u32, path: *const u16) -> i32 {
    unsafe { fs_open(handle, path, 0, 0x100) }
}
unsafe fn find_first(path: *const u16, attributes: u32, info: *mut u8) -> i32 {
    unsafe { fs_find_first(path, attributes, info) }
}
unsafe fn find_next(info: *mut u8) -> i32 {
    unsafe { fs_find_next(info) }
}
unsafe fn find_close(info: *mut u8) -> i32 {
    unsafe { fs_find_close(info) }
}
unsafe fn read(handle: u32, dest: *mut u8, count: u32, actual: *mut u32) -> i32 {
    unsafe { fs_read(handle, dest, count, actual) }
}
unsafe fn close(handle: u32) {
    unsafe {
        let _ = fs_close(handle);
    }
}
unsafe fn alloc(size: usize) -> *mut u8 {
    unsafe { pvPortMalloc(size) }
}
unsafe fn free(p: *mut u8) {
    unsafe { vPortFree(p) }
}

#[repr(align(4))]
struct SearchInfo([u8; SEARCH_INFO_LEN]);

#[derive(Clone, Copy)]
enum Filename<'a> {
    Search(&'a SearchInfo, usize, usize),
    Bytes(&'a [u8]),
}

impl Filename<'_> {
    fn len(self) -> usize {
        match self {
            Self::Search(_, _, len) => len,
            Self::Bytes(bytes) => bytes.len(),
        }
    }
    fn byte(self, index: usize) -> u8 {
        match self {
            Self::Search(info, offset, _) => {
                u16::from_le_bytes([info.0[offset + index * 2], info.0[offset + index * 2 + 1]])
                    as u8
            }
            Self::Bytes(bytes) => bytes[index],
        }
    }
}

impl core_loader::dynamic::PatchFilename for Filename<'_> {
    fn name_len(&self) -> usize {
        self.len()
    }
    fn name_byte(&self, index: usize) -> u8 {
        self.byte(index)
    }
}

/// Read the search record's basename as a view. The firmware returns either a
/// basename or a full `A:\\PATCHES\\` path, depending on model/API wrapper.
fn search_filename(info: &SearchInfo) -> Option<Filename<'_>> {
    let read_name = |offset: usize| u16::from_le_bytes([info.0[offset], info.0[offset + 1]]);
    let name_offset = if read_name(SEARCH_ALT_NAME_OFFSET) != 0 {
        SEARCH_ALT_NAME_OFFSET
    } else {
        SEARCH_NAME_OFFSET
    };
    let mut full_len = 0;
    while full_len < SEARCH_NAME_CAPACITY && read_name(name_offset + full_len * 2) != 0 {
        full_len += 1;
    }
    if full_len == 0 || full_len == SEARCH_NAME_CAPACITY {
        return None;
    }
    let prefix_len = PATCH_DIRECTORY.len();
    let has_prefix = full_len >= prefix_len
        && (0..prefix_len).all(|index| {
            let value = read_name(name_offset + index * 2);
            value <= 0x7f
                && (value as u8).to_ascii_lowercase()
                    == (PATCH_DIRECTORY[index] as u8).to_ascii_lowercase()
        });
    let start = if has_prefix { prefix_len } else { 0 };
    let len = full_len - start;
    if len < 4
        || (0..len).any(|index| {
            let value = read_name(name_offset + (start + index) * 2);
            value > 0x7f || value == b'/' as u16 || value == b'\\' as u16
        })
    {
        return None;
    }
    let extension = start + len - 4;
    if read_name(name_offset + extension * 2) != b'.' as u16
        || (read_name(name_offset + (extension + 1) * 2) as u8).to_ascii_lowercase() != b'b'
        || (read_name(name_offset + (extension + 2) * 2) as u8).to_ascii_lowercase() != b'i'
        || (read_name(name_offset + (extension + 3) * 2) as u8).to_ascii_lowercase() != b'n'
    {
        return None;
    }
    Some(Filename::Search(info, name_offset + start * 2, len))
}

fn copy_filename(source: Filename<'_>, destination: &mut [u8; SEARCH_NAME_CAPACITY]) -> usize {
    for index in 0..source.len() {
        destination[index] = source.byte(index);
    }
    source.len()
}

fn make_patch_path(
    filename: &[u8],
    output: &mut [u16; SEARCH_NAME_CAPACITY + 12],
) -> Option<usize> {
    let prefix_len = PATCH_DIRECTORY.len();
    let len = prefix_len.checked_add(filename.len())?;
    if len >= output.len() {
        return None;
    }
    // Element-wise copies: `copy_from_slice` takes a caller location that
    // would stay in the payload.
    for (slot, unit) in output.iter_mut().zip(PATCH_DIRECTORY) {
        *slot = unit;
    }
    for (index, byte) in filename.iter().enumerate() {
        output[prefix_len + index] = u16::from(*byte);
    }
    output[len] = 0;
    Some(len)
}

/// Find the least filename greater than `after` without retaining a directory
/// worth of long names on the task stack. Repeated scans trade directory I/O
/// for a small fixed scratch footprint and deterministic ordering.
unsafe fn find_next_patch(
    after: &[u8],
    selected: &mut [u8; SEARCH_NAME_CAPACITY],
) -> Result<Option<usize>, ()> {
    let mut info = SearchInfo([0; SEARCH_INFO_LEN]);
    if unsafe { find_first(PATCH_PATTERN.as_ptr(), 0x31, info.0.as_mut_ptr()) } != 0 {
        unsafe { find_close(info.0.as_mut_ptr()) };
        return Ok(None);
    }
    let mut count = 0;
    let mut selected_len = 0;
    loop {
        if count == MAX_PATCH_FILES {
            unsafe { find_close(info.0.as_mut_ptr()) };
            return Err(());
        }
        count += 1;
        if let Some(candidate) = search_filename(&info) {
            let after_order = if after.is_empty() {
                core::cmp::Ordering::Greater
            } else {
                core_loader::dynamic::compare_patch_filenames(&candidate, after)
            };
            let best_order = if selected_len == 0 {
                core::cmp::Ordering::Less
            } else {
                core_loader::dynamic::compare_patch_filenames(&candidate, &selected[..selected_len])
            };
            if after_order == core::cmp::Ordering::Greater
                && best_order == core::cmp::Ordering::Less
            {
                selected_len = copy_filename(candidate, selected);
            }
        }
        if unsafe { find_next(info.0.as_mut_ptr()) } != 0 {
            break;
        }
    }
    unsafe { find_close(info.0.as_mut_ptr()) };
    Ok((selected_len != 0).then_some(selected_len))
}

struct FirmwareReader(u32);
impl core_loader::bundle::Reader for FirmwareReader {
    type Error = ();
    fn read(&mut self, destination: &mut [u8]) -> Result<usize, ()> {
        let mut actual = 0;
        if unsafe {
            read(
                self.0,
                destination.as_mut_ptr(),
                destination.len() as u32,
                &mut actual,
            )
        } != 0
        {
            return Err(());
        }
        Ok(actual as usize)
    }

    fn finish(&mut self) -> Result<(), core_loader::bundle::ReadError> {
        let mut byte = 0;
        let mut actual = 0;
        let _status = unsafe { read(self.0, &mut byte, 1, &mut actual) };
        if actual != 0 {
            Err(core_loader::bundle::ReadError::Invalid)
        } else {
            // FatFs reports EOF as an error status with an unchanged byte
            // count. The declared file body and CRC have already been read.
            Ok(())
        }
    }
}

unsafe fn clean_cache(start: usize, len: usize) {
    let mut line = start & !31;
    let end = (start + len + 31) & !31;
    while line < end {
        unsafe { ptr::write_volatile(0xe000_ef68 as *mut u32, line as u32) };
        line += 32;
    }
}

unsafe fn synchronize_instructions() {
    unsafe {
        asm!("dsb sy", options(nostack, preserves_flags));
        ptr::write_volatile(0xe000_ef50 as *mut u32, 0); // ICIALLU
        asm!("dsb sy", "isb sy", options(nostack, preserves_flags));
    }
}

unsafe fn clear_payload(base: usize, len: usize) {
    unsafe {
        ptr::write_bytes(base as *mut u8, 0xff, len);
        clean_cache(base, len);
        synchronize_instructions();
    }
}

/// Discard a module that will not be installed: poison its code and free the
/// allocation that also holds its links.
unsafe fn discard(block: *mut u8, loaded_base: usize, memory_len: usize) {
    unsafe {
        clear_payload(loaded_base, memory_len);
        free(block);
    }
}

/// Load one compact version-1 module directly into its final allocation:
/// its hook links first, then its code and state. Hook registrations stay
/// local until the streaming CRC, relocations and every site check pass. The
/// complete module's hook set is published under one interrupt-masked
/// registry update.
unsafe fn load_dynamic<R: core_loader::bundle::Reader>(reader: &mut R, order: u32) -> bool {
    use core_loader::dynamic::{Hook, HookLink, LINK_LEN};
    let pending = match core_loader::dynamic::read_header(
        reader,
        MODULE_CAPACITY,
        core_loader::dynamic::MAX_HOOKS_PER_MODULE,
        core_loader::dynamic::MAX_RELOCATIONS_PER_MODULE,
    ) {
        Ok(header) => header,
        Err(_) => return false,
    };
    let hook_count = pending.header.hook_count;
    let memory_len = pending.header.memory_len;
    // The load order and record index share the link's order word.
    if hook_count > 0xffff || order > 0xffff {
        return false;
    }
    let links_len = hook_count * LINK_LEN;
    let block = unsafe { alloc(links_len + memory_len) };
    if block.is_null() || block as usize + links_len + memory_len > u32::MAX as usize {
        if !block.is_null() {
            unsafe { free(block) };
        }
        return false;
    }
    let links = block as *mut HookLink;
    let loaded_base = block as usize + links_len;
    {
        let code = unsafe { slice::from_raw_parts_mut(loaded_base as *mut u8, memory_len) };
        // The hook records are read into the start of the link area.
        let hooks = unsafe { slice::from_raw_parts_mut(block as *mut Hook, hook_count) };
        if core_loader::dynamic::read_body(reader, pending, code, hooks, 0, loaded_base as u32)
            .is_err()
        {
            unsafe { discard(block, loaded_base, memory_len) };
            return false;
        }
    }
    // Turn each record into its link, last first, so no unread record is
    // overwritten (see `_HOOK_RECORDS_FIT_IN_LINKS`).
    for index in (0..hook_count).rev() {
        let hook = unsafe { ptr::read((block as *const Hook).add(index)) };
        let site = hook_catalog::FUNCTION_HOOK_SITES
            .iter()
            .filter(|site| site.dynamic_dispatch)
            .position(|site| site.address == hook.address);
        let Some(site) = site.filter(|_| hook.next_slot_offset != u32::MAX) else {
            unsafe { discard(block, loaded_base, memory_len) };
            return false;
        };
        unsafe {
            ptr::write(
                links.add(index),
                HookLink {
                    entry: (loaded_base as u32 + hook.entry_offset) | 1,
                    next_slot: loaded_base as u32 + hook.next_slot_offset,
                    order: order << 16 | index as u32,
                    module_base: loaded_base as u32,
                    next: 0,
                    site: site as u32,
                },
            );
        }
    }
    unsafe {
        clean_cache(block as usize, links_len + memory_len);
        synchronize_instructions();
        let primask: u32;
        asm!("mrs {0}, PRIMASK", "cpsid i", out(reg) primask, options(nostack, preserves_flags));
        // Verify every site record this module joins before changing any.
        // `claim()` excludes competing loader transactions; this catches
        // corrupt or stale registry state without exposing a partially
        // installed multi-site module.
        for index in 0..hook_count {
            let link = ptr::read(links.add(index));
            let chain = ptr::read_volatile(site_record(link.site as usize));
            if !chain.is_consistent(|address| ptr::read_volatile(address as *const u32)) {
                asm!("msr PRIMASK, {0}", in(reg) primask, options(nostack, preserves_flags));
                discard(block, loaded_base, memory_len);
                return false;
            }
        }
        // Initialize each module continuation before exposing any handler.
        for index in 0..hook_count {
            let link = ptr::read(links.add(index));
            let site = dynamic_site(link.site as usize).unwrap();
            let continuation = (crate::patch_api::__continuations::address(site.name) as u32) | 1;
            ptr::write_volatile(link.next_slot as *mut u32, continuation);
        }
        // Append each link to its site in load order. A first link publishes
        // the site's handler; a later one is reached through the previous
        // tail's continuation.
        for index in 0..hook_count {
            let link = links.add(index);
            let record = site_record((*link).site as usize);
            let mut chain = ptr::read_volatile(record);
            if let Some(previous) = chain.append(link as u32, (*link).entry) {
                let previous = previous as *mut HookLink;
                (*previous).next = link as u32;
                ptr::write_volatile((*previous).next_slot as *mut u32, (*link).entry);
            }
            ptr::write_volatile(record, chain);
        }
        ptr::write_volatile(module_ptr_word() as *mut u32, loaded_base as u32);
        ptr::write_volatile(module_len_word() as *mut u32, memory_len as u32);
        // Record every module for consumers such as menu-loader. Entries are
        // written before the count, so a reader never sees a partial entry.
        let count_word = registry_word(core_loader::registry::MODULE_COUNT_OFFSET) as *mut u32;
        let count = ptr::read_volatile(count_word);
        if count < core_loader::registry::MAX_MODULES {
            let entry = registry_word(
                core_loader::registry::MODULE_TABLE_OFFSET
                    + count * core_loader::registry::MODULE_TABLE_ENTRY_LEN,
            ) as *mut u32;
            ptr::write_volatile(entry, loaded_base as u32);
            ptr::write_volatile(entry.add(1), memory_len as u32);
            asm!("dmb", options(nostack, preserves_flags));
            ptr::write_volatile(count_word, count + 1);
        }
        ptr::write_volatile(debug_word() as *mut u32, 0x327);
        asm!("msr PRIMASK, {0}", in(reg) primask, options(nostack, preserves_flags));
    }
    true
}

/// Enumerate the patch directory, loading each supported BIN independently.
/// Invalid or unsupported files are skipped so another candidate can load.
unsafe fn load_patch_files() -> bool {
    let mut loaded_any = false;
    let mut last_name = [0; SEARCH_NAME_CAPACITY];
    let mut last_len = 0;
    let mut path = [0u16; SEARCH_NAME_CAPACITY + 12];
    for order in 0..MAX_PATCH_FILES {
        let mut selected = [0; SEARCH_NAME_CAPACITY];
        let selected_len = match unsafe { find_next_patch(&last_name[..last_len], &mut selected) } {
            Ok(Some(len)) => len,
            Ok(None) => break,
            Err(()) => break,
        };
        if make_patch_path(&selected[..selected_len], &mut path).is_some() {
            let mut handle = 0;
            if unsafe { open(&mut handle, path.as_ptr()) } == 0 {
                let loaded = unsafe { load_dynamic(&mut FirmwareReader(handle), order as u32) };
                unsafe { close(handle) };
                loaded_any |= loaded;
            }
        }
        for (slot, byte) in last_name.iter_mut().zip(&selected[..selected_len]) {
            *slot = *byte;
        }
        last_len = selected_len;
    }
    loaded_any
}

/// Serialize attempts from different mount callers. The final word belongs to
/// the loader, not the module payload; the installed image initializes it to ff.
unsafe fn claim() -> bool {
    let primask: u32;
    unsafe {
        asm!("mrs {0}, PRIMASK", "cpsid i", out(reg) primask, options(nostack, preserves_flags));
    }
    let flag = claim_word() as *mut u32;
    let state = unsafe { ptr::read_volatile(flag) };
    let free = state == 0 || state == u32::MAX;
    if free {
        unsafe {
            // Reserve the loader while one file is being streamed. Without a
            // distinct in-progress state, parallel mount callers can both
            // allocate and mutate the same hook registry.
            ptr::write_volatile(flag, 2);
        }
    }
    unsafe {
        asm!("msr PRIMASK, {0}", in(reg) primask, options(nostack, preserves_flags));
    }
    free
}
unsafe fn finish(success: bool) {
    unsafe {
        ptr::write_volatile(claim_word() as *mut u32, if success { 1 } else { 0 });
    }
}

/// Runs after a startup mount call; the call-hook dispatcher preserves the
/// firmware mount result and selects only the cataloged call sites below.
unsafe fn after_sd_mount(drive: u32) {
    if !unsafe { ensure_registry() } {
        return;
    }
    if unsafe { ptr::read_volatile(claim_word() as *const u32) } == 1 {
        return;
    }
    unsafe { ptr::write_volatile(debug_word() as *mut u32, 1) };
    unsafe { ptr::write_volatile(debug_word() as *mut u32, 2) };
    if drive != 0x41 {
        unsafe { ptr::write_volatile(debug_word() as *mut u32, 3) };
        return;
    }
    let is_ready = unsafe { ready() };
    unsafe { ptr::write_volatile(debug_word() as *mut u32, 0x100 | (is_ready & 0xff)) };
    if is_ready == 0 {
        return;
    }
    if !unsafe { claim() } {
        unsafe { ptr::write_volatile(debug_word() as *mut u32, 4) };
        return;
    }
    unsafe { ptr::write_volatile(debug_word() as *mut u32, 5) };
    let success = unsafe { load_patch_files() };
    unsafe {
        if success {
            ptr::write_volatile(debug_word() as *mut u32, 0x500);
        } else {
            ptr::write_volatile(debug_word() as *mut u32, 0x400);
        }
    }
    unsafe {
        finish(success);
    }
}

#[cfg(sd_l6)]
#[function_call_post_hook(fs_mount, at = 0x8000_9a60)]
unsafe fn sd_mount_post_l6_recorder(drive: u32) {
    unsafe { after_sd_mount(drive) };
}

#[cfg(sd_l6)]
#[function_call_post_hook(fs_mount, at = 0x8000_9aa0)]
unsafe fn sd_mount_post_l6_settings(drive: u32) {
    unsafe { after_sd_mount(drive) };
}

#[cfg(not(sd_l6))]
#[function_call_post_hook(fs_mount, at = 0x8000_e9fa)]
unsafe fn sd_mount_post_l6max_recorder_a(drive: u32) {
    unsafe { after_sd_mount(drive) };
}

#[cfg(not(sd_l6))]
#[function_call_post_hook(fs_mount, at = 0x8000_ea24)]
unsafe fn sd_mount_post_l6max_recorder_b(drive: u32) {
    unsafe { after_sd_mount(drive) };
}

#[cfg(not(sd_l6))]
#[function_call_post_hook(fs_mount, at = 0x8001_0118)]
unsafe fn sd_mount_post_l6max_startup(drive: u32) {
    unsafe { after_sd_mount(drive) };
}

#[cfg(not(sd_l6))]
#[function_call_post_hook(fs_mount, at = 0x8002_bc7e)]
unsafe fn sd_mount_post_l6max_patch(drive: u32) {
    unsafe { after_sd_mount(drive) };
}

#[cfg(not(sd_l6))]
#[function_call_post_hook(fs_mount, at = 0x8000_ebac)]
unsafe fn sd_mount_post_l6max_factory(drive: u32) {
    unsafe { after_sd_mount(drive) };
}

#[cfg(sd_l6)]
#[function_offset_hook(
    sys_l6_initialize_and_start_scheduler,
    at = 0x24,
    displaced_len = 4,
    guard_sha256 = "0360cab6934417a2ec4e6d8e81a452db698d5fc5f0965ae39b13d02391dc2132"
)]
unsafe fn sd_reset_hook() {
    unsafe { sd_reset_registry() };
    unsafe { vTaskStartScheduler() };
    hook_done()
}

#[cfg(not(sd_l6))]
#[function_offset_hook(
    sys_main_startup_initialize,
    at = 0,
    displaced_len = 4,
    guard_sha256 = "07f4993f36ec2892ba7daa618956f411293bf2fc693e4a3a2aebb9abd0c2ea04"
)]
unsafe fn sd_reset_hook() {
    unsafe { sd_reset_registry() };
    hook_continue()
}
