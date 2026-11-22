#![no_std]
//! The binary interface between the patch tools and the resident Patch loader:
//! the registry the loader publishes, the SD module format, load-time
//! relocation and the duplicate-package rule. The loader includes this file directly, so
//! everything outside the `alloc` feature parses without allocating. With
//! `alloc`, the host tools also get the matching writers.
#[cfg(feature = "alloc")]
extern crate alloc;
#[cfg(test)]
extern crate std;

/// The resident Patch loader's hook registry. The loader allocates it from the
/// FreeRTOS heap on the first card mount and publishes its address in a root
/// word. Dispatch stubs, consumers such as menu-loader, and the test harness
/// follow that word. Until the loader publishes a registry, the root holds its
/// stock value, which never points at the registry magic, so every dispatch
/// takes the stock path.
pub mod registry {
    /// The registry root on L6 1.10. The L6 scatter table places `.data` at
    /// 0x801f5400, and the firmware never references the RAM below it from
    /// 0x801f0000. An earlier fixed registry at 0x801f8000 overwrote live
    /// `.data`, and the L6 then never started its USB controller.
    pub const L6_POINTER: u32 = 0x801f_0000;
    /// The root's value before the loader publishes a registry on L6. Reset
    /// does not reinitialize this word, so the loader's reset hook restores it.
    pub const L6_POINTER_STOCK: u32 = 0;
    /// The registry root on L6max 1.10: the language 6 entry of the
    /// per-language text pointer table at 0x80205118. Only language 0 has
    /// text, `gui_current_language` always returns 0, and every table reader
    /// indexes by that value, so the firmware never reads this entry. It is
    /// part of `.data`, so every reset restores its stock value.
    pub const L6MAX_POINTER: u32 = 0x8020_5130;
    /// The stock `.data` value of the L6max root: the address of the empty
    /// language 6 string pointer array.
    pub const L6MAX_POINTER_STOCK: u32 = 0x8020_69e4;

    /// Registry layout 2 ("L6R2"): a fixed header with the module table, then
    /// one [`SiteChain`](super::dynamic::SiteChain) record per dynamic dispatch
    /// site in the installed hook catalog. Hook links live in each module's
    /// own allocation, so the only limit on how many SD patches can hook a
    /// site is free memory.
    pub const MAGIC: u32 = 0x4c36_5232;
    pub const MAGIC_OFFSET: u32 = 0;
    pub const DEBUG_OFFSET: u32 = 4;
    pub const CLAIM_OFFSET: u32 = 8;
    /// Number of entries used in the module table.
    pub const MODULE_COUNT_OFFSET: u32 = 12;
    /// One `(base, memory_len)` pair per loaded module, in load order.
    pub const MODULE_TABLE_OFFSET: u32 = 16;
    pub const MODULE_TABLE_ENTRY_LEN: u32 = 8;
    pub const MAX_MODULES: u32 = 16;
    /// The most recently loaded module, kept for diagnostics.
    pub const MODULE_PTR_OFFSET: u32 = 160;
    pub const MODULE_LEN_OFFSET: u32 = 164;
    /// Number of site records after the header, from the loader's catalog.
    pub const SITE_COUNT_OFFSET: u32 = 168;
    /// First site record. Site `n`'s record is at `SITES_OFFSET + n * 16`.
    pub const SITES_OFFSET: u32 = 0x100;
    pub const SITE_RECORD_LEN: u32 = 16;
    /// Dispatch stubs load a site's handler with a 12-bit offset, which
    /// bounds the number of sites a registry can hold.
    pub const MAX_SITES: u32 = (0x1000 - SITES_OFFSET) / SITE_RECORD_LEN;

    const _: () = assert!(
        MODULE_TABLE_OFFSET + MAX_MODULES * MODULE_TABLE_ENTRY_LEN <= MODULE_PTR_OFFSET
            && MODULE_LEN_OFFSET < SITE_COUNT_OFFSET
            && SITE_COUNT_OFFSET + 4 <= SITES_OFFSET
            && SITE_RECORD_LEN as usize == core::mem::size_of::<super::dynamic::SiteChain>()
    );

    /// Offset of site `index`'s record, whose first word is its handler.
    pub const fn site_offset(index: u32) -> u32 {
        SITES_OFFSET + index * SITE_RECORD_LEN
    }

    /// Bytes allocated for a registry with `sites` dispatch sites.
    pub const fn len(sites: u32) -> u32 {
        site_offset(sites)
    }

    /// The registry root word for a MAIN image loaded at `main_base`.
    pub const fn pointer_for_main_base(main_base: u32) -> u32 {
        if main_base == 0x8000_1000 {
            L6_POINTER
        } else {
            L6MAX_POINTER
        }
    }

    /// The root's value before a registry is published.
    pub const fn stock_pointer_for_main_base(main_base: u32) -> u32 {
        if main_base == 0x8000_1000 {
            L6_POINTER_STOCK
        } else {
            L6MAX_POINTER_STOCK
        }
    }
}

/// Load-time fixups for a module linked at one address and loaded at another.
pub mod relocation {
    /// A relocation record that does not fit the image or its target.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct Error;

    /// Apply a module's load bias to module-internal absolute references.
    /// The file image remains unchanged, so callers may use it as the source
    /// while relocating the newly allocated copy in place.
    pub fn relocate<I: Iterator<Item = Relocation>>(
        image: &mut [u8],
        relocations: I,
        linked_base: u32,
        loaded_base: u32,
    ) -> Result<(), Error> {
        relocate_with_memory(image, relocations, linked_base, loaded_base, image.len())
    }

    /// Apply relocations whose absolute targets may point into zero-filled
    /// writable state following the streamed code image.
    pub fn relocate_with_memory<I: Iterator<Item = Relocation>>(
        image: &mut [u8],
        relocations: I,
        linked_base: u32,
        loaded_base: u32,
        memory_len: usize,
    ) -> Result<(), Error> {
        if memory_len < image.len() {
            return Err(Error);
        }
        let linked_end = linked_base
            .checked_add(u32::try_from(memory_len).map_err(|_| Error)?)
            .ok_or(Error)?;
        let delta = i64::from(loaded_base) - i64::from(linked_base);
        for relocation in relocations {
            match relocation {
                Relocation::Abs32 { offset } => {
                    let end = offset.checked_add(4).ok_or(Error)?;
                    // A fixed-size view: `copy_from_slice` would take a
                    // caller location that stays in the loader.
                    let bytes: &mut [u8; 4] = image
                        .get_mut(offset..end)
                        .and_then(|bytes| bytes.try_into().ok())
                        .ok_or(Error)?;
                    let target = u32::from_le_bytes(*bytes);
                    if !(linked_base..=linked_end).contains(&(target & !1)) {
                        return Err(Error);
                    }
                    let relocated = (i64::from(target) + delta).try_into().map_err(|_| Error)?;
                    *bytes = u32::to_le_bytes(relocated);
                }
                Relocation::ThumbMov32 {
                    low_offset,
                    high_offset,
                } => {
                    let (low_reg, low) = read_thumb_mov(image, low_offset, 0xf240)?;
                    let (high_reg, high) = read_thumb_mov(image, high_offset, 0xf2c0)?;
                    if low_reg != high_reg {
                        return Err(Error);
                    }
                    let target = (u32::from(high) << 16) | u32::from(low);
                    if !(linked_base..=linked_end).contains(&(target & !1)) {
                        return Err(Error);
                    }
                    let relocated: u32 =
                        (i64::from(target) + delta).try_into().map_err(|_| Error)?;
                    write_thumb_mov(image, low_offset, 0xf240, low_reg, relocated as u16)?;
                    write_thumb_mov(
                        image,
                        high_offset,
                        0xf2c0,
                        high_reg,
                        (relocated >> 16) as u16,
                    )?;
                }
            }
        }
        Ok(())
    }

    fn read_thumb_mov(image: &[u8], offset: usize, opcode: u16) -> Result<(u8, u16), Error> {
        let end = offset.checked_add(4).ok_or(Error)?;
        let instruction = image.get(offset..end).ok_or(Error)?;
        let first = u16::from_le_bytes(instruction[..2].try_into().unwrap());
        let second = u16::from_le_bytes(instruction[2..].try_into().unwrap());
        if first & 0xfbf0 != opcode {
            return Err(Error);
        }
        let immediate = ((first & 0x000f) << 12)
            | ((first & 0x0400) << 1)
            | ((second & 0x7000) >> 4)
            | (second & 0x00ff);
        Ok((((second >> 8) & 0xf) as u8, immediate))
    }

    fn write_thumb_mov(
        image: &mut [u8],
        offset: usize,
        opcode: u16,
        register: u8,
        immediate: u16,
    ) -> Result<(), Error> {
        let end = offset.checked_add(4).ok_or(Error)?;
        // A fixed-size view: `copy_from_slice` would take a caller location
        // that stays in the loader.
        let instruction: &mut [u8; 4] = image
            .get_mut(offset..end)
            .and_then(|bytes| bytes.try_into().ok())
            .ok_or(Error)?;
        let first = u16::from_le_bytes([instruction[0], instruction[1]]);
        let second = u16::from_le_bytes([instruction[2], instruction[3]]);
        if first & 0xfbf0 != opcode || ((second >> 8) & 0xf) as u8 != register {
            return Err(Error);
        }
        let first = (first & !0x040f) | ((immediate >> 12) & 0xf) | ((immediate >> 1) & 0x0400);
        let second = (second & !0x70ff) | ((immediate << 4) & 0x7000) | (immediate & 0x00ff);
        let [first_low, first_high] = first.to_le_bytes();
        let [second_low, second_high] = second.to_le_bytes();
        *instruction = [first_low, first_high, second_low, second_high];
        Ok(())
    }

    /// Load-time fixups for addresses that refer back into a linked patch
    /// image. PC-relative calls and references need no fixup when the
    /// complete image moves.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum Relocation {
        Abs32 {
            offset: usize,
        },
        ThumbMov32 {
            low_offset: usize,
            high_offset: usize,
        },
    }

    impl Relocation {
        /// The `(kind, first, second)` words of the relocation's file record.
        pub fn record(&self) -> [u32; 3] {
            match *self {
                Relocation::Abs32 { offset } => [1, offset as u32, 0],
                Relocation::ThumbMov32 {
                    low_offset,
                    high_offset,
                } => [2, low_offset as u32, high_offset as u32],
            }
        }
    }
}

/// Compact, one-patch-per-file dynamic module stream (format version 1).
///
/// The fixed version-1 header is 28 bytes. Code starts immediately after it,
/// followed by fixed 12-byte hook records and 12-byte relocation records.
/// The CRC covers the complete file except the checksum word itself. No whole-file staging buffer
/// is needed: code is read into its final allocation and metadata is processed
/// one bounded record at a time.
pub mod dynamic {
    use super::relocation::{self, Relocation};

    /// A sequential SD file reader. A zero-byte read denotes end of file.
    pub trait Reader {
        type Error;
        fn read(&mut self, destination: &mut [u8]) -> Result<usize, Self::Error>;

        /// Confirm there are no bytes after the declared body. Readers whose
        /// underlying API reports EOF as an error can override this method.
        fn finish(&mut self) -> Result<(), ReadError> {
            let mut trailing = [0u8; 1];
            match self.read(&mut trailing) {
                Ok(0) => Ok(()),
                Ok(_) => Err(ReadError::Invalid),
                Err(_) => Err(ReadError::Io),
            }
        }
    }

    #[derive(Debug, PartialEq, Eq)]
    pub enum ReadError {
        Io,
        Truncated,
        Invalid,
        TooLarge,
    }

    /// Reads a file already in memory.
    impl Reader for &[u8] {
        type Error = ();
        fn read(&mut self, destination: &mut [u8]) -> Result<usize, ()> {
            let count = destination.len().min(self.len());
            destination[..count].copy_from_slice(&self[..count]);
            *self = &self[count..];
            Ok(count)
        }
    }

    pub const HEADER_LEN_V1: usize = 28;
    pub const FORMAT_VERSION: u16 = 1;
    pub const LOADER_ABI: u16 = 1;
    pub const HOOK_RECORD_LEN: usize = 12;
    pub const RELOCATION_RECORD_LEN: usize = 12;
    /// Upper bound on hook records in one patch file. The loader keeps each
    /// module's hook links in the module's own allocation, so this only
    /// rejects nonsensical headers before allocating.
    pub const MAX_HOOKS_PER_MODULE: usize = 1024;
    /// Maximum relocation records accepted in one patch file. The builder
    /// refuses larger modules, because the loader would reject them.
    pub const MAX_RELOCATIONS_PER_MODULE: usize = 256;
    /// The largest code plus zero-filled state the loader allocates for one
    /// module from the shared FreeRTOS heap.
    pub const MAX_MEMORY_LEN: usize = 0x60000;

    #[cfg(feature = "alloc")]
    mod module;
    #[cfg(feature = "alloc")]
    pub use module::Module;

    /// One installed hook. The loader places a module's links at the start of
    /// the module's allocation, ahead of its code, so a link lives exactly as
    /// long as the handler it describes.
    #[repr(C)]
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
    pub struct HookLink {
        pub entry: u32,
        /// Address of this module's writable continuation pointer.
        pub next_slot: u32,
        /// Load position: the file's place in filename order in the high 16
        /// bits, the hook record's index in the low 16 bits.
        pub order: u32,
        pub module_base: u32,
        /// The next link on the same site, or zero at the tail.
        pub next: u32,
        /// Dispatch site index in the loader's hook catalog.
        pub site: u32,
    }
    pub const LINK_LEN: usize = core::mem::size_of::<HookLink>();

    /// A dispatch site's record in the registry. The dispatch stub reads
    /// `handler`; the loader keeps the chain of links behind it.
    #[repr(C)]
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
    pub struct SiteChain {
        /// The first link's entry, or zero for the stock path.
        pub handler: u32,
        pub first: u32,
        pub last: u32,
        pub count: u32,
    }

    impl SiteChain {
        /// Whether the record is empty or describes a chain whose head is
        /// published. `entry_of` reads a link's entry.
        pub fn is_consistent(&self, entry_of: impl Fn(u32) -> u32) -> bool {
            if self.count == 0 {
                self.handler == 0 && self.first == 0 && self.last == 0
            } else {
                self.first != 0 && self.last != 0 && self.handler == entry_of(self.first)
            }
        }

        /// Append the link at `link` with entry `entry`. Returns the previous
        /// tail's address, whose `next` and continuation must then point at
        /// the new link, or `None` when the link is the first and `handler`
        /// now publishes it.
        pub fn append(&mut self, link: u32, entry: u32) -> Option<u32> {
            let previous = (self.count != 0).then_some(self.last);
            if previous.is_none() {
                self.first = link;
                self.handler = entry;
            }
            self.last = link;
            self.count += 1;
            previous
        }
    }

    /// Hook records are read into the start of the link area, then turned
    /// into links from the last to the first: link `i` covers bytes
    /// `24i..24i+24`, and every record `j < i` ends at or before `12i`, so no
    /// unread record is overwritten.
    pub const _HOOK_RECORDS_FIT_IN_LINKS: () =
        assert!(core::mem::size_of::<Hook>() <= LINK_LEN && LINK_LEN % 8 == 0);

    pub trait PatchFilename {
        fn name_len(&self) -> usize;
        fn name_byte(&self, index: usize) -> u8;
    }

    impl PatchFilename for [u8] {
        fn name_len(&self) -> usize {
            self.len()
        }
        fn name_byte(&self, index: usize) -> u8 {
            self[index]
        }
    }

    fn numeric_prefix<N: PatchFilename + ?Sized>(name: &N) -> Option<(usize, usize)> {
        let dash = (0..name.name_len()).find(|&index| name.name_byte(index) == b'-')?;
        if dash == 0 || !(0..dash).all(|index| name.name_byte(index).is_ascii_digit()) {
            return None;
        }
        let mut first = 0;
        while first + 1 < dash && name.name_byte(first) == b'0' {
            first += 1;
        }
        Some((first, dash - first))
    }

    /// Compare patch names: decimal prefixes before `-` sort numerically and
    /// before unnumbered names; ties use case-insensitive then bytewise order.
    pub fn compare_patch_filenames<L: PatchFilename + ?Sized, R: PatchFilename + ?Sized>(
        left: &L,
        right: &R,
    ) -> core::cmp::Ordering {
        use core::cmp::Ordering;
        match (numeric_prefix(left), numeric_prefix(right)) {
            (Some((left_start, left_len)), Some((right_start, right_len))) => {
                let numeric = left_len.cmp(&right_len).then_with(|| {
                    (0..left_len)
                        .map(|i| left.name_byte(left_start + i))
                        .cmp((0..right_len).map(|i| right.name_byte(right_start + i)))
                });
                if numeric != Ordering::Equal {
                    return numeric;
                }
            }
            (Some(_), None) => return Ordering::Less,
            (None, Some(_)) => return Ordering::Greater,
            (None, None) => {}
        }
        let folded = (0..left.name_len())
            .map(|i| left.name_byte(i).to_ascii_lowercase())
            .cmp((0..right.name_len()).map(|i| right.name_byte(i).to_ascii_lowercase()));
        if folded != Ordering::Equal {
            return folded;
        }
        (0..left.name_len())
            .map(|i| left.name_byte(i))
            .cmp((0..right.name_len()).map(|i| right.name_byte(i)))
    }

    #[repr(C)]
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
    pub struct Hook {
        pub address: u32,
        pub entry_offset: u32,
        /// Optional aligned code offset containing a writable `next` function
        /// pointer. `u32::MAX` marks a replacement that cannot call through.
        pub next_slot_offset: u32,
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct Header {
        pub code_len: usize,
        /// Total writable allocation size, including zero-filled module state.
        pub memory_len: usize,
        pub hook_count: usize,
        pub relocation_count: usize,
        pub reserved: u32,
        pub crc32: u32,
    }

    #[derive(Debug)]
    pub struct Pending {
        pub header: Header,
        crc_state: u32,
    }

    #[derive(Debug, PartialEq, Eq)]
    pub enum Error {
        Read(ReadError),
        Invalid,
        TooLarge,
        Relocation,
        Checksum,
    }

    fn u16_at(bytes: &[u8], offset: usize) -> u16 {
        u16::from_le_bytes(bytes[offset..offset + 2].try_into().unwrap())
    }

    fn u32_at(bytes: &[u8], offset: usize) -> u32 {
        u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
    }

    fn read_exact<R: Reader>(reader: &mut R, mut dst: &mut [u8]) -> Result<(), Error> {
        while !dst.is_empty() {
            let count = reader.read(dst).map_err(|_| Error::Read(ReadError::Io))?;
            if count == 0 {
                return Err(Error::Read(ReadError::Truncated));
            }
            if count > dst.len() {
                return Err(Error::Read(ReadError::Invalid));
            }
            dst = &mut dst[count..];
        }
        Ok(())
    }

    fn crc_update(mut crc: u32, bytes: &[u8]) -> u32 {
        for &byte in bytes {
            crc ^= u32::from(byte);
            for _ in 0..8 {
                crc = (crc >> 1) ^ (0xedb8_8320 & 0u32.wrapping_sub(crc & 1));
            }
        }
        crc
    }

    /// Read and validate the fixed header before the caller allocates code.
    pub fn read_header<R: Reader>(
        reader: &mut R,
        max_code_len: usize,
        max_hooks: usize,
        max_relocations: usize,
    ) -> Result<Pending, Error> {
        let mut header = [0u8; HEADER_LEN_V1];
        read_exact(reader, &mut header[..8])?;
        if &header[..4] != b"L6SB"
            || u16_at(&header, 4) != FORMAT_VERSION
            || u16_at(&header, 6) != LOADER_ABI
        {
            return Err(Error::Invalid);
        }
        read_exact(reader, &mut header[8..])?;
        let code_len = u32_at(&header, 8) as usize;
        let memory_len = u32_at(&header, 12) as usize;
        let hook_count = u16_at(&header, 16) as usize;
        let relocation_count = u16_at(&header, 18) as usize;
        let reserved = u32_at(&header, 20);
        let crc_offset = 24;
        let expected_crc = u32_at(&header, crc_offset);
        if code_len == 0
            || memory_len < code_len
            || memory_len > max_code_len
            || hook_count == 0
            || hook_count > max_hooks
            || relocation_count > max_relocations
        {
            return Err(Error::TooLarge);
        }
        if reserved != 0 {
            return Err(Error::Invalid);
        }
        Ok(Pending {
            header: Header {
                code_len,
                memory_len,
                hook_count,
                relocation_count,
                reserved,
                crc32: expected_crc,
            },
            // Header checksum bytes are deliberately omitted from CRC input.
            crc_state: crc_update(!0u32, &header[..crc_offset]),
        })
    }

    /// Stream code into the final allocation, zero its writable tail, and
    /// apply relocations as records arrive. Hooks remain unpublished until CRC
    /// validation succeeds.
    pub fn read_body<R: Reader>(
        reader: &mut R,
        pending: Pending,
        code: &mut [u8],
        hooks: &mut [Hook],
        linked_base: u32,
        loaded_base: u32,
    ) -> Result<Header, Error> {
        let header = pending.header;
        if code.len() != header.memory_len || hooks.len() < header.hook_count {
            return Err(Error::TooLarge);
        }
        let mut crc = pending.crc_state;
        read_exact(reader, &mut code[..header.code_len])?;
        crc = crc_update(crc, &code[..header.code_len]);
        code[header.code_len..].fill(0);

        let mut hook_bytes = [0u8; HOOK_RECORD_LEN];
        for index in 0..header.hook_count {
            read_exact(reader, &mut hook_bytes)?;
            crc = crc_update(crc, &hook_bytes);
            let hook = Hook {
                address: u32_at(&hook_bytes, 0),
                entry_offset: u32_at(&hook_bytes, 4),
                next_slot_offset: u32_at(&hook_bytes, 8),
            };
            if hook.address & 1 != 0
                || hook.entry_offset & 1 != 0
                || hook.entry_offset as usize >= header.code_len
                || (hook.next_slot_offset != u32::MAX
                    && (hook.next_slot_offset & 3 != 0
                        || hook.next_slot_offset as usize + 4 > header.code_len))
            {
                return Err(Error::Invalid);
            }
            hooks[index] = hook;
        }

        let mut relocation_bytes = [0u8; RELOCATION_RECORD_LEN];
        for _ in 0..header.relocation_count {
            read_exact(reader, &mut relocation_bytes)?;
            crc = crc_update(crc, &relocation_bytes);
            let kind = u32_at(&relocation_bytes, 0);
            let first = u32_at(&relocation_bytes, 4) as usize;
            let second = u32_at(&relocation_bytes, 8) as usize;
            let relocation = match kind {
                1 if second == 0 => Relocation::Abs32 { offset: first },
                2 => Relocation::ThumbMov32 {
                    low_offset: first,
                    high_offset: second,
                },
                _ => return Err(Error::Relocation),
            };
            relocation::relocate_with_memory(
                &mut code[..header.code_len],
                core::iter::once(relocation),
                linked_base,
                loaded_base,
                header.memory_len,
            )
            .map_err(|_| Error::Relocation)?;
        }
        if !crc != header.crc32 {
            return Err(Error::Checksum);
        }
        reader.finish().map_err(Error::Read)?;
        Ok(header)
    }

    /// CRC-32/ISO-HDLC for a version-1 file, excluding bytes 24..28.
    pub fn checksum(file: &[u8]) -> Option<u32> {
        if file.len() < HEADER_LEN_V1 || &file[..4] != b"L6SB" || u16_at(file, 4) != FORMAT_VERSION
        {
            return None;
        }
        let crc = crc_update(!0u32, &file[..24]);
        Some(!crc_update(crc, &file[HEADER_LEN_V1..]))
    }
}

/// Which SD modules to load when several carry the same package.
///
/// Every module names its packages in its identity record (`patchrt`'s
/// `PatchInfo`). A package in two single-package modules loads once, from
/// the newest version; equal versions keep the first in load order. A package
/// shared with a multi-package module has no safe choice, so no module
/// carrying it loads.
pub mod identity {
    /// `patchrt::PatchRecord`: ASCII, NUL-padded.
    #[repr(C)]
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct PackageRecord {
        pub name: [u8; 32],
        pub version: [u8; 16],
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum Decision {
        Load,
        /// A newer or earlier copy of its only package loads instead.
        Superseded,
        /// It shares a package with another module, and one of them
        /// carries more than one package.
        Conflict,
    }

    /// `major.minor.patch`, NUL-padded.
    pub fn version(text: &[u8]) -> Option<(u32, u32, u32)> {
        let len = text
            .iter()
            .position(|&byte| byte == 0)
            .unwrap_or(text.len());
        let mut parts = [0u32; 3];
        let mut index = 0;
        let mut digits = 0;
        for &byte in &text[..len] {
            match byte {
                b'0'..=b'9' => {
                    parts[index] = parts[index]
                        .checked_mul(10)?
                        .checked_add(u32::from(byte - b'0'))?;
                    digits += 1;
                }
                b'.' if digits > 0 && index < 2 => {
                    index += 1;
                    digits = 0;
                }
                _ => return None,
            }
        }
        (index == 2 && digits > 0).then_some((parts[0], parts[1], parts[2]))
    }

    /// Decide every module at once. `modules[i]` is module `i`'s packages, in
    /// load order; `decisions` must be as long as `modules`.
    pub fn resolve(modules: &[&[PackageRecord]], decisions: &mut [Decision]) {
        for decision in decisions.iter_mut() {
            *decision = Decision::Load;
        }
        for (index, packages) in modules.iter().enumerate() {
            for package in packages.iter() {
                let carries = |other: &&[PackageRecord]| {
                    other.iter().any(|record| record.name == package.name)
                };
                let holders = modules.iter().filter(|other| carries(other)).count();
                if holders < 2 {
                    continue;
                }
                let bundled = modules
                    .iter()
                    .filter(|other| carries(other))
                    .any(|other| other.len() > 1);
                // The first holder decides for the whole group.
                if modules.iter().position(|other| carries(&other)) != Some(index) {
                    continue;
                }
                let newest = if bundled {
                    None
                } else {
                    modules
                        .iter()
                        .enumerate()
                        .filter(|(_, other)| carries(other))
                        .try_fold(None::<(usize, (u32, u32, u32))>, |best, (holder, other)| {
                            let found = version(&other[0].version)?;
                            Some(match best {
                                Some((_, newest)) if newest >= found => best,
                                _ => Some((holder, found)),
                            })
                        })
                        .flatten()
                };
                for (holder, other) in modules.iter().enumerate() {
                    if !carries(other) {
                        continue;
                    }
                    decisions[holder] = match newest {
                        Some((keep, _)) if keep == holder => decisions[holder],
                        Some(_) if decisions[holder] != Decision::Conflict => Decision::Superseded,
                        _ => Decision::Conflict,
                    };
                }
            }
        }
    }
}

#[cfg(test)]
mod identity_tests {
    extern crate std;
    use std::{vec, vec::Vec};

    use super::identity::{Decision, PackageRecord, resolve, version};

    fn record(name: &str, version: &str) -> PackageRecord {
        let mut record = PackageRecord {
            name: [0; 32],
            version: [0; 16],
        };
        record.name[..name.len()].copy_from_slice(name.as_bytes());
        record.version[..version.len()].copy_from_slice(version.as_bytes());
        record
    }

    fn decide(modules: &[&[PackageRecord]]) -> Vec<Decision> {
        let mut decisions = vec![Decision::Load; modules.len()];
        resolve(modules, &mut decisions);
        decisions
    }

    #[test]
    fn versions_parse_as_three_numbers() {
        assert_eq!(version(b"1.0.1\0\0"), Some((1, 0, 1)));
        assert_eq!(version(b"10.20.300"), Some((10, 20, 300)));
        for bad in [
            &b""[..],
            b"1.0",
            b"1..0",
            b"1.0.0.0",
            b"1.0.x",
            b".1.0",
            b"1.0.",
        ] {
            assert_eq!(version(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn distinct_packages_all_load() {
        let knob = [record("knob-dialog", "1.0.0")];
        let midi = [record("midi-sync", "1.0.0")];
        let effects = [record("efx-loader", "1.0.1"), record("efx-velvet", "1.0.1")];
        assert_eq!(decide(&[&knob, &midi, &effects]), [Decision::Load; 3]);
    }

    #[test]
    fn the_newest_single_package_module_loads() {
        let old = [record("knob-dialog", "1.0.0")];
        let new = [record("knob-dialog", "1.2.0")];
        let middle = [record("knob-dialog", "1.1.9")];
        let other = [record("midi-sync", "1.0.0")];
        assert_eq!(
            decide(&[&old, &new, &other, &middle]),
            [
                Decision::Superseded,
                Decision::Load,
                Decision::Load,
                Decision::Superseded
            ]
        );
        // Numeric, not textual: 1.10.0 is newer than 1.9.0.
        let nine = [record("knob-dialog", "1.9.0")];
        let ten = [record("knob-dialog", "1.10.0")];
        assert_eq!(
            decide(&[&ten, &nine]),
            [Decision::Load, Decision::Superseded]
        );
    }

    #[test]
    fn equal_versions_keep_the_first_in_load_order() {
        let first = [record("knob-dialog", "1.0.0")];
        let second = [record("knob-dialog", "1.0.0")];
        assert_eq!(
            decide(&[&first, &second]),
            [Decision::Load, Decision::Superseded]
        );
    }

    #[test]
    fn an_unreadable_version_has_no_safe_choice() {
        let good = [record("knob-dialog", "1.0.0")];
        let bad = [record("knob-dialog", "dev")];
        assert_eq!(
            decide(&[&good, &bad]),
            [Decision::Conflict, Decision::Conflict]
        );
    }

    #[test]
    fn a_package_shared_with_a_bundle_loads_from_neither() {
        let knob = [record("knob-dialog", "1.0.0")];
        let effects = [
            record("efx-loader", "1.0.1"),
            record("knob-dialog", "1.0.0"),
            record("efx-velvet", "1.0.1"),
        ];
        let midi = [record("midi-sync", "1.0.0")];
        assert_eq!(
            decide(&[&knob, &effects, &midi]),
            [Decision::Conflict, Decision::Conflict, Decision::Load]
        );
        // Two versions of the same bundle, as an unreplaced PATCHES folder leaves.
        let old_effects = [
            record("efx-loader", "1.0.0"),
            record("knob-dialog", "1.0.0"),
            record("efx-velvet", "1.0.0"),
        ];
        assert_eq!(
            decide(&[&old_effects, &effects]),
            [Decision::Conflict, Decision::Conflict]
        );
    }

    #[test]
    fn a_conflict_outranks_a_newer_version() {
        // Each package is decided on its own: knob-dialog's newest single
        // module loads, while midi-sync is shared with a bundle.
        let old = [record("knob-dialog", "1.0.0")];
        let new = [record("knob-dialog", "1.1.0")];
        let bundle = [
            record("midi-sync", "1.0.0"),
            record("din-mixer-control", "1.0.0"),
        ];
        let midi = [record("midi-sync", "1.0.1")];
        assert_eq!(
            decide(&[&old, &new, &bundle, &midi]),
            [
                Decision::Superseded,
                Decision::Load,
                Decision::Conflict,
                Decision::Conflict
            ]
        );
        // Modules without identity records never conflict.
        assert_eq!(decide(&[&[], &[], &old]), [Decision::Load; 3]);
    }
}

#[cfg(test)]
mod dynamic_stream_tests {
    extern crate std;
    use super::dynamic::{self, Hook};
    use super::dynamic::{ReadError, Reader};
    use std::vec::Vec;

    struct Chunks<'a> {
        bytes: &'a [u8],
        chunk: usize,
    }
    impl Reader for Chunks<'_> {
        type Error = ();
        fn read(&mut self, dst: &mut [u8]) -> Result<usize, Self::Error> {
            let count = dst.len().min(self.chunk).min(self.bytes.len());
            dst[..count].copy_from_slice(&self.bytes[..count]);
            self.bytes = &self.bytes[count..];
            Ok(count)
        }
    }

    fn fixture() -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"L6SB");
        bytes.extend_from_slice(&dynamic::FORMAT_VERSION.to_le_bytes());
        bytes.extend_from_slice(&dynamic::LOADER_ABI.to_le_bytes());
        bytes.extend_from_slice(&12u32.to_le_bytes()); // code_len
        bytes.extend_from_slice(&12u32.to_le_bytes()); // memory_len
        bytes.extend_from_slice(&1u16.to_le_bytes()); // hooks
        bytes.extend_from_slice(&1u16.to_le_bytes()); // relocations
        bytes.extend_from_slice(&0u32.to_le_bytes()); // reserved
        bytes.extend_from_slice(&[0; 4]); // CRC
        bytes.extend_from_slice(&[4, 0, 0, 0, 0, 0xbf, 0x70, 0x47, 0, 0, 0, 0]);
        bytes.extend_from_slice(&0x8000_1000u32.to_le_bytes());
        bytes.extend_from_slice(&4u32.to_le_bytes());
        bytes.extend_from_slice(&8u32.to_le_bytes());
        bytes.extend_from_slice(&1u32.to_le_bytes());
        bytes.extend_from_slice(&0u32.to_le_bytes());
        bytes.extend_from_slice(&0u32.to_le_bytes());
        let checksum = dynamic::checksum(&bytes).unwrap();
        bytes[24..28].copy_from_slice(&checksum.to_le_bytes());
        bytes
    }

    #[test]
    fn streams_code_metadata_and_relocations_without_a_staging_buffer() {
        let bytes = fixture();
        assert_eq!(
            dynamic::checksum(&bytes),
            Some(u32::from_le_bytes(bytes[24..28].try_into().unwrap()))
        );
        let mut reader = Chunks {
            bytes: &bytes,
            chunk: 3,
        };
        let pending = dynamic::read_header(&mut reader, 32, 4, 8).unwrap();
        let mut code = [0; 12];
        let mut hooks = [Hook::default(); 4];
        let header =
            dynamic::read_body(&mut reader, pending, &mut code, &mut hooks, 0, 0x2000).unwrap();
        assert_eq!(header.code_len, 12);
        assert_eq!(header.hook_count, 1);
        assert_eq!(header.relocation_count, 1);
        assert_eq!(
            hooks[0],
            Hook {
                address: 0x8000_1000,
                entry_offset: 4,
                next_slot_offset: 8,
            }
        );
        assert_eq!(u32::from_le_bytes(code[..4].try_into().unwrap()), 0x2004);
    }

    #[test]
    fn accepts_multiple_handlers_for_the_same_function_entry() {
        let mut bytes = fixture();
        bytes[16..18].copy_from_slice(&2u16.to_le_bytes());
        let mut second_hook = bytes[40..52].to_vec();
        second_hook[4..8].copy_from_slice(&0u32.to_le_bytes());
        second_hook[8..12].copy_from_slice(&0u32.to_le_bytes());
        bytes.splice(52..52, second_hook);
        let checksum = dynamic::checksum(&bytes).unwrap();
        bytes[24..28].copy_from_slice(&checksum.to_le_bytes());

        let mut reader = Chunks {
            bytes: &bytes,
            chunk: 5,
        };
        let pending = dynamic::read_header(&mut reader, 32, 4, 8).unwrap();
        let mut code = [0; 12];
        let mut hooks = [Hook::default(); 4];
        let header =
            dynamic::read_body(&mut reader, pending, &mut code, &mut hooks, 0, 0x2000).unwrap();

        assert_eq!(header.hook_count, 2);
        assert_eq!(hooks[0].address, hooks[1].address);
        assert_ne!(hooks[0].entry_offset, hooks[1].entry_offset);
        assert_ne!(hooks[0].next_slot_offset, hooks[1].next_slot_offset);
    }

    #[test]
    fn v1_streams_code_and_relocates_into_zeroed_module_state() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"L6SB");
        bytes.extend_from_slice(&dynamic::FORMAT_VERSION.to_le_bytes());
        bytes.extend_from_slice(&dynamic::LOADER_ABI.to_le_bytes());
        bytes.extend_from_slice(&4u32.to_le_bytes()); // code_len
        bytes.extend_from_slice(&12u32.to_le_bytes()); // memory_len
        bytes.extend_from_slice(&1u16.to_le_bytes()); // hooks
        bytes.extend_from_slice(&1u16.to_le_bytes()); // relocations
        bytes.extend_from_slice(&0u32.to_le_bytes()); // reserved
        bytes.extend_from_slice(&[0; 4]); // CRC
        bytes.extend_from_slice(&8u32.to_le_bytes()); // code points into state
        bytes.extend_from_slice(&0x8000_1000u32.to_le_bytes());
        bytes.extend_from_slice(&0u32.to_le_bytes());
        bytes.extend_from_slice(&0u32.to_le_bytes());
        bytes.extend_from_slice(&1u32.to_le_bytes()); // ABS32
        bytes.extend_from_slice(&0u32.to_le_bytes()); // relocation site
        bytes.extend_from_slice(&0u32.to_le_bytes());
        let checksum = dynamic::checksum(&bytes).unwrap();
        bytes[24..28].copy_from_slice(&checksum.to_le_bytes());

        let mut reader = Chunks {
            bytes: &bytes,
            chunk: 2,
        };
        let pending = dynamic::read_header(&mut reader, 32, 4, 8).unwrap();
        assert_eq!(pending.header.code_len, 4);
        assert_eq!(pending.header.memory_len, 12);
        let mut allocation = [0xa5; 12];
        let mut hooks = [Hook::default(); 4];
        let header =
            dynamic::read_body(&mut reader, pending, &mut allocation, &mut hooks, 0, 0x2000)
                .unwrap();
        assert_eq!(header.code_len, 4);
        assert_eq!(header.memory_len, 12);
        assert_eq!(
            u32::from_le_bytes(allocation[..4].try_into().unwrap()),
            0x2008
        );
        assert_eq!(&allocation[4..], &[0; 8]);
    }

    #[test]
    fn site_chain_appends_in_load_order_without_a_limit() {
        let mut chain = dynamic::SiteChain::default();
        assert!(chain.is_consistent(|_| 0));
        assert_eq!(chain.append(0x2000, 0x2101), None);
        assert_eq!(chain.handler, 0x2101);
        let mut tail = 0x2000;
        for index in 1..64u32 {
            let link = 0x2000 + index * 0x100;
            assert_eq!(chain.append(link, link + 0x101), Some(tail));
            tail = link;
        }
        assert_eq!(chain.count, 64);
        assert_eq!(
            (chain.first, chain.last, chain.handler),
            (0x2000, tail, 0x2101)
        );
        assert!(chain.is_consistent(|link| if link == 0x2000 { 0x2101 } else { 0 }));
        assert!(!chain.is_consistent(|_| 0x9999));
    }

    #[test]
    fn patch_filename_priorities_use_numeric_order_and_stable_ties() {
        use core::cmp::Ordering;
        let compare = |left: &[u8], right: &[u8]| dynamic::compare_patch_filenames(left, right);
        assert_eq!(
            compare(b"2-MIDI.BIN" as &[u8], b"10-DIN.BIN"),
            Ordering::Less
        );
        assert_eq!(compare(b"02-MIDI.BIN", b"2-DIN.BIN"), Ordering::Less);
        assert_eq!(compare(b"9-LAST.BIN", b"MIDI.BIN"), Ordering::Less);
        assert_eq!(compare(b"abc.BIN", b"ABC.BIN"), Ordering::Greater);
    }

    #[test]
    fn streaming_parser_rejects_corruption_and_bad_counts() {
        let mut bytes = fixture();
        bytes[24] ^= 0x80;
        let mut reader = Chunks {
            bytes: &bytes,
            chunk: 5,
        };
        let pending = dynamic::read_header(&mut reader, 32, 4, 8).unwrap();
        let mut code = [0; 12];
        let mut hooks = [Hook::default(); 4];
        assert_eq!(
            dynamic::read_body(&mut reader, pending, &mut code, &mut hooks, 0, 0x2000),
            Err(dynamic::Error::Checksum)
        );

        let bytes = fixture();
        let mut reader = Chunks {
            bytes: &bytes,
            chunk: 7,
        };
        assert_eq!(
            dynamic::read_header(&mut reader, 4, 4, 8).unwrap_err(),
            dynamic::Error::TooLarge
        );

        let mut bytes = fixture();
        bytes.push(0);
        let mut reader = Chunks {
            bytes: &bytes,
            chunk: 64,
        };
        let pending = dynamic::read_header(&mut reader, 32, 4, 8).unwrap();
        let mut code = [0; 12];
        let mut hooks = [Hook::default(); 4];
        assert_eq!(
            dynamic::read_body(&mut reader, pending, &mut code, &mut hooks, 0, 0x2000),
            Err(dynamic::Error::Read(ReadError::Invalid))
        );
    }
}
