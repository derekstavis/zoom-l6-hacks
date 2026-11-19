#![no_std]
#![no_main]
//! Adds System > Dump ROM, which copies the L6max memories to `.BIN` files in
//! the root of the SD card, plus a text manifest.
//!
//! The main NOR holds the bootloader, which no update package carries, so it
//! is read first. Memory regions are copied through a probe that turns bus
//! faults into skipped pages: the processor is not identified, so region
//! sizes beyond what the firmware itself uses are not known to be readable.
//! Unreadable 4 KiB pages are written as zeros and listed in the manifest, so
//! every file has its full size and offsets match addresses.
#[path = "../../api/patchrt.rs"]
#[macro_use]
pub mod patch_api;
mod firmware;
use firmware as firmware_bindings;
#[path = "../../api/l6max/hooks.rs"]
mod hook_catalog;
use firmware::*;

use core::arch::asm;
use core::ptr::{read_volatile as read, write_volatile as write};

menu_items! {
    SYSTEM => "Dump ROM" => action(dump_from_menu), requires 0x01 | 0x02 | 0x10 | 0x20;
}

/// Bytes copied and written per SD write. NOR reads are limited to 65,535.
const CHUNK: usize = 32 * 1024;
/// Granularity of the bus-fault probe and of the unreadable ranges.
const PAGE: usize = 4 * 1024;
const MANIFEST_CAPACITY: usize = 4 * 1024;
/// End of the NOR range the stock checksum on the firmware version screen
/// covers: bootloader, application, panel application and boot data.
const CHECKSUM_END: usize = 0x1f_8000;
/// Unreadable ranges listed per region; more are counted, not listed.
const MAX_RANGES: usize = 8;

/// Stock notification messages.
const MESSAGE_SD_CARD_FULL: u32 = 8;
const MESSAGE_NO_SD_CARD: u32 = 9;
const MESSAGE_SD_WRITE_ERROR: u32 = 12;
const MESSAGE_DONE: u32 = 15;
const BUSY_SD_CARD: u32 = 1;
/// Our result for a write that stored fewer bytes than requested: the card
/// is full. Stock file routines return their own negative codes.
const SHORT_WRITE: i32 = i32::MIN;

const SCB_CPUID: usize = 0xe000_ed00;
const SCB_CCR: usize = 0xe000_ed14;
const SCB_CFSR: usize = 0xe000_ed28;
const MPU_TYPE: usize = 0xe000_ed90;
const MPU_CTRL: usize = 0xe000_ed94;
const CCR_BFHFNMIGN: u32 = 1 << 8;
const CFSR_BUS_FAULTS: u32 = 0xff << 8;
const MPU_CTRL_ENABLE: u32 = 1 << 0;
const MPU_CTRL_HFNMIENA: u32 = 1 << 1;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Source {
    /// Physical NOR offsets, read through the firmware's NOR driver.
    Nor,
    /// CPU addresses, read through the bus-fault probe.
    Memory,
}

struct Region {
    name: &'static str,
    file: &'static str,
    path: [u16; 32],
    source: Source,
    base: u32,
    length: u32,
}

const fn region(
    name: &'static str,
    file: &'static str,
    source: Source,
    base: u32,
    length: u32,
) -> Region {
    Region {
        name,
        file,
        path: card_path(file),
        source,
        base,
        length,
    }
}

/// The NOR first: it holds the bootloader. SDRAM, the largest, last.
const REGIONS: [Region; 6] = [
    region("NOR", "L6MAX_NOR.BIN", Source::Nor, 0, NOR_SIZE),
    region(
        "BOOTROM",
        "L6MAX_BOOTROM.BIN",
        Source::Memory,
        0x0020_0000,
        0x2_0000,
    ),
    region(
        "ITCM",
        "L6MAX_ITCM.BIN",
        Source::Memory,
        0x0000_0000,
        0x8_0000,
    ),
    region(
        "DTCM",
        "L6MAX_DTCM.BIN",
        Source::Memory,
        0x2000_0000,
        0x8_0000,
    ),
    region(
        "OCRAM",
        "L6MAX_OCRAM.BIN",
        Source::Memory,
        0x2020_0000,
        0x10_0000,
    ),
    region(
        "SDRAM",
        "L6MAX_SDRAM.BIN",
        Source::Memory,
        0x8000_0000,
        0x400_0000,
    ),
];
const MANIFEST_PATH: [u16; 32] = card_path("L6MAX_DUMP.TXT");

/// Identification words recorded in the manifest. The SoC registers are the
/// i.MX RT10xx locations; they are read through the probe like memory.
const ID_WORDS: [(&str, u32); 6] = [
    ("CPUID", SCB_CPUID as u32),
    ("MPU_TYPE", MPU_TYPE as u32),
    ("MPU_CTRL", MPU_CTRL as u32),
    ("USB_ANALOG_DIGPROG", 0x400d_8260),
    ("SRC_SBMR1", 0x400f_8004),
    ("SRC_SBMR2", 0x400f_801c),
];

/// NUL-terminated UTF-16 path of an ASCII file name in the card's root.
const fn card_path<const N: usize>(file: &str) -> [u16; N] {
    const ROOT: &[u8] = b"A:\\";
    let bytes = file.as_bytes();
    assert!(ROOT.len() + bytes.len() < N, "path is too long");
    let mut result = [0; N];
    let mut index = 0;
    while index < ROOT.len() {
        result[index] = ROOT[index] as u16;
        index += 1;
    }
    while index - ROOT.len() < bytes.len() {
        let byte = bytes[index - ROOT.len()];
        assert!(byte < 0x80, "path must be ASCII");
        result[index] = byte as u16;
        index += 1;
    }
    result
}

/// Runs on the UI task. The busy overlay animates from DisplayCycle2 while
/// the dump blocks this task.
unsafe extern "C" fn dump_from_menu() -> u32 {
    let message = unsafe {
        if fs_ready() == 0 {
            MESSAGE_NO_SD_CARD
        } else {
            gui_busy_overlay_show(BUSY_SD_CARD);
            let result = dump();
            gui_busy_overlay_hide();
            match result {
                Ok(()) => MESSAGE_DONE,
                Err(SHORT_WRITE) => MESSAGE_SD_CARD_FULL,
                Err(_) => MESSAGE_SD_WRITE_ERROR,
            }
        }
    };
    unsafe { gui_notification_show(message) };
    1
}

/// Heap working set: the copy buffer, then the manifest text.
struct Work {
    buffer: *mut u8,
    manifest: Text,
}

unsafe fn dump() -> Result<(), i32> {
    let block = unsafe { pvPortMalloc(CHUNK + MANIFEST_CAPACITY) };
    if block.is_null() {
        return Err(-1);
    }
    let mut work = Work {
        buffer: block,
        manifest: Text {
            data: unsafe { block.add(CHUNK) },
            len: 0,
        },
    };
    let result = unsafe { dump_into(&mut work) };
    unsafe { vPortFree(block) };
    result
}

unsafe fn dump_into(work: &mut Work) -> Result<(), i32> {
    let text = &mut work.manifest;
    text.push("L6max memory dump from the dump-rom patch\n");
    text.push("Unreadable pages are zero in the files and listed here.\n\n");
    for (name, address) in ID_WORDS {
        let mut word = 0u32;
        let readable = unsafe { probe_copy(address as usize, &mut word, 1) };
        text.push(name);
        text.push(" ");
        if readable {
            text.hex(word);
        } else {
            text.push("unreadable");
        }
        text.push("\n");
    }
    text.push("\nregion file base length status\n");
    let mut first_error = Ok(());
    for region in &REGIONS {
        let result = unsafe { dump_region(region, work.buffer, &mut work.manifest) };
        if let Err(code) = result {
            work.manifest.push(" write error ");
            work.manifest.hex(code as u32);
            work.manifest.push("\n");
            if first_error.is_ok() {
                first_error = Err(code);
            }
            // A full card fails every later file as well.
            if code == SHORT_WRITE {
                break;
            }
        }
    }
    let manifest = unsafe { core::slice::from_raw_parts(work.manifest.data, work.manifest.len) };
    let mut pending = Some(manifest);
    unsafe { write_file(&MANIFEST_PATH, || pending.take())? };
    first_error
}

/// Writes one region file and appends its manifest line, without the final
/// newline when the write fails.
unsafe fn dump_region(region: &Region, buffer: *mut u8, text: &mut Text) -> Result<(), i32> {
    text.push(region.name);
    text.push(" ");
    text.push(region.file);
    text.push(" ");
    text.hex(region.base);
    text.push(" ");
    text.hex(region.length);
    let mut ranges = Ranges::new();
    let chunks = region.length as usize / CHUNK;
    let mut index = 0;
    let mut bootloader_sum = 0u32;
    let mut checksum = 0u32;
    unsafe {
        write_file(&region.path, || {
            if index == chunks {
                return None;
            }
            let offset = index * CHUNK;
            index += 1;
            fill_chunk(region, offset, buffer, &mut ranges);
            let data = core::slice::from_raw_parts(buffer, CHUNK);
            if region.source == Source::Nor && offset < CHECKSUM_END {
                let sum = data
                    .iter()
                    .fold(0u32, |sum, &byte| sum.wrapping_add(byte as u32));
                checksum = checksum.wrapping_add(sum);
                if offset < NOR_BOOTLOADER_END as usize {
                    bootloader_sum = bootloader_sum.wrapping_add(sum);
                }
            }
            Some(data)
        })?;
    }
    if ranges.pages == 0 {
        text.push(" ok\n");
    } else {
        text.push(" unreadable");
        for &(start, end) in ranges.list.iter().take(ranges.listed) {
            text.push(" ");
            text.hex(region.base + start as u32);
            text.push("-");
            text.hex(region.base + end as u32 - 1);
        }
        if ranges.unlisted > 0 {
            text.push(" and more, ");
        } else {
            text.push(", ");
        }
        text.decimal(ranges.pages as u32);
        text.push(" pages\n");
    }
    if region.source == Source::Nor {
        text.push("NOR bootloader byte sum (0x00000000-0x0004ffff) ");
        text.hex(bootloader_sum);
        text.push("\nNOR CHECKSUM (low 16 bits of the byte sum of 0x00000000-0x001f7fff, as the firmware version screen shows it) ");
        text.hex4(checksum as u16);
        text.push("\n");
    }
    Ok(())
}

/// Fills `buffer` with the chunk at `offset` into `region`, zeroing and
/// recording whatever could not be read.
unsafe fn fill_chunk(region: &Region, offset: usize, buffer: *mut u8, ranges: &mut Ranges) {
    match region.source {
        Source::Nor => {
            let status = unsafe { nor_read(region.base + offset as u32, buffer, CHUNK as u32) };
            if status != 0 {
                unsafe { buffer.write_bytes(0, CHUNK) };
                ranges.add(offset, offset + CHUNK);
            }
        }
        Source::Memory => {
            let mut page = 0;
            while page < CHUNK {
                let at = offset + page;
                let destination = unsafe { buffer.add(page) };
                let readable =
                    unsafe { probe_copy(region.base as usize + at, destination.cast(), PAGE / 4) };
                if !readable {
                    unsafe { destination.write_bytes(0, PAGE) };
                    ranges.add(at, at + PAGE);
                }
                page += PAGE;
            }
        }
    }
}

/// Creates or replaces `path` and writes each slice `next` returns until it
/// returns `None`.
unsafe fn write_file<'a>(
    path: &[u16; 32],
    mut next: impl FnMut() -> Option<&'a [u8]>,
) -> Result<(), i32> {
    let mut handle = 0u32;
    let status = unsafe {
        fs_open(
            &mut handle,
            path.as_ptr(),
            FS_OPEN_CREATE_READ_WRITE,
            FS_MODE_READ_WRITE,
        )
    };
    if status != 0 {
        return Err(status);
    }
    let mut result = Ok(());
    while let Some(data) = next() {
        let mut actual = 0u32;
        let status = unsafe { fs_write(handle, data.as_ptr(), data.len() as u32, &mut actual) };
        if status != 0 {
            result = Err(status);
            break;
        }
        if actual as usize != data.len() {
            result = Err(SHORT_WRITE);
            break;
        }
    }
    let status = unsafe { fs_close(handle) };
    if result.is_ok() && status != 0 {
        result = Err(status);
    }
    result
}

/// Copies `words` words from `source` to `destination` with bus faults
/// ignored, and reports whether every read completed.
///
/// FAULTMASK raises the execution priority to -1, where `CCR.BFHFNMIGN`
/// turns a precise data bus fault into a recorded status: the load returns
/// an unknown value and BFSR records the error. With `MPU_CTRL.HFNMIENA`
/// clear, the MPU is not applied at that priority, so a region the boot code
/// may have protected cannot raise a MemManage fault instead. All interrupts
/// stay masked for one page, a few microseconds.
unsafe fn probe_copy(source: usize, destination: *mut u32, words: usize) -> bool {
    let status;
    unsafe {
        asm!("cpsid f", options(nostack, preserves_flags));
        let ccr = read(SCB_CCR as *const u32);
        let mpu = read(MPU_CTRL as *const u32);
        if mpu & MPU_CTRL_ENABLE != 0 {
            write(MPU_CTRL as *mut u32, mpu & !MPU_CTRL_HFNMIENA);
        }
        write(SCB_CCR as *mut u32, ccr | CCR_BFHFNMIGN);
        write(SCB_CFSR as *mut u32, CFSR_BUS_FAULTS);
        asm!("dsb", "isb", options(nostack, preserves_flags));
        // Assembly, not a Rust read: the ITCM starts at address 0.
        asm!(
            "2:",
            "ldr {value}, [{source}], #4",
            "str {value}, [{destination}], #4",
            "subs {count}, {count}, #1",
            "bne 2b",
            source = inout(reg) source => _,
            destination = inout(reg) destination => _,
            count = inout(reg) words => _,
            value = out(reg) _,
            options(nostack),
        );
        asm!("dsb", options(nostack, preserves_flags));
        status = read(SCB_CFSR as *const u32) & CFSR_BUS_FAULTS;
        write(SCB_CFSR as *mut u32, status);
        write(SCB_CCR as *mut u32, ccr);
        if mpu & MPU_CTRL_ENABLE != 0 {
            write(MPU_CTRL as *mut u32, mpu);
        }
        asm!("dsb", "isb", options(nostack, preserves_flags));
        asm!("cpsie f", options(nostack, preserves_flags));
    }
    status == 0
}

/// Unreadable byte ranges within one region. Adjacent ranges merge; ranges
/// past the list are only counted.
struct Ranges {
    list: [(usize, usize); MAX_RANGES],
    listed: usize,
    unlisted: usize,
    pages: usize,
}

impl Ranges {
    const fn new() -> Self {
        Self {
            list: [(0, 0); MAX_RANGES],
            listed: 0,
            unlisted: 0,
            pages: 0,
        }
    }

    fn add(&mut self, start: usize, end: usize) {
        self.pages += (end - start) / PAGE;
        if self.unlisted == 0 {
            if let Some(last) = self.list.get_mut(self.listed.wrapping_sub(1)) {
                if last.1 == start {
                    last.1 = end;
                    return;
                }
            }
        }
        if let Some(slot) = self.list.get_mut(self.listed) {
            *slot = (start, end);
            self.listed += 1;
        } else {
            self.unlisted += 1;
        }
    }
}

/// Bounded text in the heap block; output past the capacity is dropped.
struct Text {
    data: *mut u8,
    len: usize,
}

impl Text {
    fn push(&mut self, text: &str) {
        for &byte in text.as_bytes() {
            if self.len == MANIFEST_CAPACITY {
                return;
            }
            unsafe { self.data.add(self.len).write(byte) };
            self.len += 1;
        }
    }

    fn hex(&mut self, value: u32) {
        let mut digits = *b"0x00000000";
        for index in 0..8 {
            let nibble = (value >> (28 - 4 * index)) & 0xf;
            digits[2 + index] = b"0123456789abcdef"[nibble as usize];
        }
        self.push(unsafe { core::str::from_utf8_unchecked(&digits) });
    }

    /// Four uppercase hex digits, as the firmware version screen shows them.
    fn hex4(&mut self, value: u16) {
        let mut digits = [0u8; 4];
        for (index, digit) in digits.iter_mut().enumerate() {
            let nibble = (value >> (12 - 4 * index)) & 0xf;
            *digit = b"0123456789ABCDEF"[nibble as usize];
        }
        self.push(unsafe { core::str::from_utf8_unchecked(&digits) });
    }

    fn decimal(&mut self, mut value: u32) {
        let mut digits = [0u8; 10];
        let mut start = digits.len();
        for digit in digits.iter_mut().rev() {
            *digit = b'0' + (value % 10) as u8;
            start -= 1;
            value /= 10;
            if value == 0 {
                break;
            }
        }
        if let Some(text) = digits.get(start..) {
            self.push(unsafe { core::str::from_utf8_unchecked(text) });
        }
    }
}
