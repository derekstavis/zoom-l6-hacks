#![no_std]
#![no_main]
//! Adds System > Patches to the L6max main menu and builds the menu items
//! other patches declare with `menu_items!`.
//!
//! The patch keeps no RAM of its own. On the first entry into the main menu
//! it allocates one permanent heap block holding its state and a copy of the
//! language 0 text tables with room for new text IDs, and points the
//! firmware's language 0 table slots at it. Every text lookup goes through
//! those slots, so the moved table is also how later entries find the state.
//! Nodes and their titles are separate allocations that live until reset.
#[path = "../../api/patchrt.rs"]
#[macro_use]
pub mod patch_api;
use patch_api::{
    MenuGroup, MenuItem, PATCH_INFO_LAYOUT, PATCH_INFO_MAGIC, PatchInfo, function_entry_hook,
};
mod firmware;
use firmware as firmware_bindings;
#[path = "../../api/l6max/hooks.rs"]
mod hook_catalog;
use firmware::*;

use core::ptr::{addr_of, read_volatile as read, write_volatile as write};

/// "MLDR": the permanent block's state header.
const MAGIC: u32 = 0x4d4c_4452;
/// Text IDs reserved beyond the stock 316 for patch rows and menu items.
const TEXT_HEADROOM: usize = 128;
const TEXT_CAPACITY: usize = GUI_TEXT_RESOURCE_COUNT + TEXT_HEADROOM;
/// Bounds on structures read from other payloads.
const MAX_STATIC_PAYLOADS: usize = 32;
const MAX_PATCHES_PER_PAYLOAD: u16 = 32;
const MAX_MENU_GROUPS: u32 = 32;
const MAX_MENU_ITEMS: u32 = 64;
/// Stock `.data`; anything outside it is a heap block.
const DATA_START: usize = 0x8020_0000;
const DATA_END: usize = 0x8020_a100;

/// The Patch loader's registry, which records the SD modules it loaded.
#[allow(dead_code, unused_attributes)]
#[path = "../../../tools/patch-abi/src/lib.rs"]
mod patch_abi;
use patch_abi::registry;

/// Stock menus that patches can extend, indexed by `menu_parent`.
const PARENTS: [u32; 5] = [
    MENU_NODE_MAIN,
    MENU_NODE_PROJECT,
    MENU_NODE_SOUND_PAD,
    MENU_NODE_MIXER,
    MENU_NODE_SYSTEM,
];

/// The permanent block: this header, then the text pointer and length tables.
#[repr(C)]
struct State {
    magic: u32,
    /// Text IDs in use; new IDs are allocated from here up to the capacity.
    text_used: u32,
    /// The Patches node under System.
    patches: u32,
    /// SD modules already listed, from the loader's module table.
    sd_seen: u32,
}
const POINTERS_OFFSET: usize = core::mem::size_of::<State>();
const LENGTHS_OFFSET: usize = POINTERS_OFFSET + TEXT_CAPACITY * 4;
const BLOCK_LEN: usize = LENGTHS_OFFSET + TEXT_CAPACITY * 2;

fn text_pointers_slot() -> *mut u32 {
    gui_text_pointers_by_language.address() as *mut u32
}
fn text_lengths_slot() -> *mut u32 {
    gui_text_lengths_by_language.address() as *mut u32
}

unsafe fn state() -> *mut State {
    unsafe {
        let pointers = read(text_pointers_slot()) as usize;
        if (DATA_START..DATA_END).contains(&pointers) {
            return core::ptr::null_mut();
        }
        let state = pointers.wrapping_sub(POINTERS_OFFSET) as *mut State;
        if read(addr_of!((*state).magic)) == MAGIC {
            state
        } else {
            core::ptr::null_mut()
        }
    }
}

unsafe fn text_pointers(state: *mut State) -> *mut u32 {
    unsafe { (state as *mut u8).add(POINTERS_OFFSET) as *mut u32 }
}
unsafe fn text_lengths(state: *mut State) -> *mut u16 {
    unsafe { (state as *mut u8).add(LENGTHS_OFFSET) as *mut u16 }
}

/// Bytes of NUL-padded ASCII in `field`.
fn text_len(field: &[u8]) -> usize {
    field
        .iter()
        .position(|&byte| byte == 0)
        .unwrap_or(field.len())
}

/// Allocate a text ID holding the concatenation of `parts`, or return 0 when
/// the headroom or the heap is exhausted. ID 0 renders nothing.
unsafe fn add_text(state: *mut State, parts: &[&[u8]]) -> u16 {
    unsafe {
        let id = read(addr_of!((*state).text_used)) as usize;
        if id >= TEXT_CAPACITY {
            return 0;
        }
        let length = parts.iter().map(|part| text_len(part)).sum::<usize>();
        let buffer = pvPortMalloc((length + 1) * 2) as *mut u16;
        if buffer.is_null() {
            return 0;
        }
        let mut index = 0;
        for part in parts {
            for &byte in &part[..text_len(part)] {
                write(buffer.add(index), byte as u16);
                index += 1;
            }
        }
        write(buffer.add(length), 0);
        write(text_pointers(state).add(id), buffer as u32);
        write(text_lengths(state).add(id), length as u16);
        write(addr_of!((*state).text_used) as *mut u32, id as u32 + 1);
        id as u16
    }
}

unsafe fn set_u32(node: *mut u8, offset: usize, value: u32) {
    unsafe { write(node.add(offset) as *mut u32, value) }
}
unsafe fn set_u16(node: *mut u8, offset: usize, value: u16) {
    unsafe { write(node.add(offset) as *mut u16, value) }
}

/// Allocate a node modeled on System: a plain, enabled, non-value-list item.
unsafe fn new_node(title: u16, action: u32, requires: u32) -> *mut u8 {
    unsafe {
        let node = pvPortMalloc(MENU_NODE_LEN);
        if node.is_null() {
            return node;
        }
        let template = MENU_NODE_SYSTEM as *const u8;
        for offset in (0..MENU_NODE_LEN).step_by(4) {
            write(
                node.add(offset) as *mut u32,
                read(template.add(offset) as *const u32),
            );
        }
        set_u32(node, MENU_NODE_FLAGS, 0);
        set_u32(node, MENU_NODE_REQUIRES, requires);
        set_u16(node, MENU_NODE_TITLE, title);
        set_u32(node, MENU_NODE_CHILDREN, 0);
        set_u16(node, MENU_NODE_CHILD_COUNT, 0);
        set_u16(node, MENU_NODE_SELECTED, 0);
        set_u16(node, MENU_NODE_SCROLL, 0);
        set_u32(node, MENU_NODE_ACTION, action);
        node
    }
}

/// The stock no-op action, as stored in every node's reserved word.
unsafe fn leaf_action() -> u32 {
    unsafe { read((MENU_NODE_SYSTEM as *const u8).add(MENU_NODE_RESERVED) as *const u32) }
}

/// Append `node` to `parent`'s children. The parent gets a new child array
/// one entry longer. The previous array is left in place: stock arrays live
/// in `.data`, and a replaced patch array is a few words.
unsafe fn append_child(parent: *mut u8, node: *mut u8) -> bool {
    unsafe {
        let old = read(parent.add(MENU_NODE_CHILDREN) as *const u32) as *const u32;
        let count = read(parent.add(MENU_NODE_CHILD_COUNT) as *const u16) as usize;
        let new = pvPortMalloc((count + 1) * 4) as *mut u32;
        if new.is_null() {
            return false;
        }
        for index in 0..count {
            write(new.add(index), read(old.add(index)));
        }
        set_u32(node, MENU_NODE_INDEX, count as u32);
        write(new.add(count), node as u32);
        // The array is complete before the parent points at it, and the count
        // grows only after the pointer, so the menu never reads past the end.
        write(parent.add(MENU_NODE_CHILDREN) as *mut u32, new as u32);
        write(
            parent.add(MENU_NODE_CHILD_COUNT) as *mut u16,
            (count + 1) as u16,
        );
        true
    }
}

/// Add one row per patch in `info` to the Patches list, and every menu item
/// the payload declares to its parent menu.
unsafe fn add_payload(state: *mut State, info: *const PatchInfo, from_sd: bool) {
    unsafe {
        let patches = read(addr_of!((*state).patches)) as *mut u8;
        let count = read(addr_of!((*info).patch_count)).min(MAX_PATCHES_PER_PAYLOAD);
        let records = read(addr_of!((*info).patches));
        for index in 0..count as usize {
            let record = &*records.add(index);
            let suffix: &[u8] = if from_sd { b" (SD)" } else { b"" };
            let title = add_text(state, &[&record.name, b" ", &record.version, suffix]);
            let node = new_node(title, leaf_action(), 0);
            if node.is_null() || !append_child(patches, node) {
                return;
            }
        }
        let group_count = read(addr_of!((*info).menu_group_count)).min(MAX_MENU_GROUPS);
        let groups = read(addr_of!((*info).menu_groups));
        for group in 0..group_count as usize {
            let group: *const MenuGroup = groups.add(group);
            let items = read(addr_of!((*group).items));
            let item_count = read(addr_of!((*group).count)).min(MAX_MENU_ITEMS);
            for item in 0..item_count as usize {
                add_menu_item(state, items.add(item));
            }
        }
    }
}

unsafe fn add_menu_item(state: *mut State, item: *const MenuItem) {
    unsafe {
        let parent = read(addr_of!((*item).parent)) as usize;
        let Some(&parent) = PARENTS.get(parent) else {
            return;
        };
        let title = add_text(state, &[&(*item).title]);
        let action = match read(addr_of!((*item).action)) {
            Some(action) => action as usize as u32,
            None => leaf_action(),
        };
        let node = new_node(title, action, read(addr_of!((*item).requires)));
        if !node.is_null() {
            append_child(parent as *mut u8, node);
        }
    }
}

unsafe fn valid_info(info: *const PatchInfo) -> bool {
    unsafe {
        read(addr_of!((*info).magic)) == PATCH_INFO_MAGIC
            && read(addr_of!((*info).layout)) == PATCH_INFO_LAYOUT
    }
}

unsafe fn next_static(info: *const PatchInfo) -> *const PatchInfo {
    unsafe { read(addr_of!((*info).next)) as *const PatchInfo }
}

/// Every static payload's record, in placement order. The builder links them
/// into a ring that includes this patch's own record; a zero `next` means
/// this record is alone.
unsafe fn add_static_payloads(state: *mut State) {
    unsafe {
        let own = addr_of!(patch_api::__identity::__l6_patch_info);
        // Payloads are placed at increasing addresses, so the lowest record is
        // the first one installed.
        let mut first = own;
        let mut info = own;
        for _ in 0..MAX_STATIC_PAYLOADS {
            let next = next_static(info);
            if next.is_null() || next == own || !valid_info(next) {
                break;
            }
            if next < first {
                first = next;
            }
            info = next;
        }
        let mut info = first;
        for _ in 0..MAX_STATIC_PAYLOADS {
            if !valid_info(info) {
                return;
            }
            add_payload(state, info, false);
            let next = next_static(info);
            if next.is_null() || next == first {
                return;
            }
            info = next;
        }
    }
}

/// SD modules the loader recorded since the last call.
unsafe fn add_sd_modules(state: *mut State) {
    unsafe {
        let root = read(registry::L6MAX_POINTER as *const u32);
        if root == registry::L6MAX_POINTER_STOCK || read(root as *const u32) != registry::MAGIC {
            return;
        }
        let word = |offset: u32| (root + offset) as usize;
        let count = read(word(registry::MODULE_COUNT_OFFSET) as *const u32).min(registry::MAX_MODULES);
        let seen = read(addr_of!((*state).sd_seen));
        for index in seen..count {
            let entry = word(registry::MODULE_TABLE_OFFSET + index * registry::MODULE_TABLE_ENTRY_LEN);
            let base = read(entry as *const u32) as *const PatchInfo;
            if valid_info(base) {
                add_payload(state, base, true);
            } else {
                // A module built before identity records existed.
                let title = add_text(state, &[b"Unnamed SD patch"]);
                let node = new_node(title, leaf_action(), 0);
                if !node.is_null() {
                    append_child(read(addr_of!((*state).patches)) as *mut u8, node);
                }
            }
        }
        write(addr_of!((*state).sd_seen) as *mut u32, count.max(seen));
    }
}

/// Allocate the permanent block, move the text tables into it and add the
/// Patches node. Returns null when the heap cannot hold the block.
unsafe fn install() -> *mut State {
    unsafe {
        let block = pvPortMalloc(BLOCK_LEN);
        if block.is_null() {
            return core::ptr::null_mut();
        }
        let state = block as *mut State;
        let stock_pointers = read(text_pointers_slot()) as *const u32;
        let stock_lengths = read(text_lengths_slot()) as *const u16;
        for id in 0..TEXT_CAPACITY {
            let (pointer, length) = if id < GUI_TEXT_RESOURCE_COUNT {
                (read(stock_pointers.add(id)), read(stock_lengths.add(id)))
            } else {
                (0, 0)
            };
            write(text_pointers(state).add(id), pointer);
            write(text_lengths(state).add(id), length);
        }
        write(
            state,
            State {
                magic: MAGIC,
                text_used: GUI_TEXT_RESOURCE_COUNT as u32,
                patches: 0,
                sd_seen: 0,
            },
        );
        // Publish the tables before any node uses a new ID.
        write(text_lengths_slot(), text_lengths(state) as u32);
        write(text_pointers_slot(), text_pointers(state) as u32);

        let title = add_text(state, &[b"Patches"]);
        let submenu = read((MENU_NODE_SYSTEM as *const u8).add(MENU_NODE_ACTION) as *const u32);
        let patches = new_node(title, submenu, 0);
        if patches.is_null() {
            return state;
        }
        write(addr_of!((*state).patches) as *mut u32, patches as u32);
        append_child(MENU_NODE_SYSTEM as *mut u8, patches);
        add_static_payloads(state);
        state
    }
}

#[function_entry_hook(menu_window_enter)]
pub unsafe fn menu_window_enter_hook() -> u32 {
    unsafe {
        let mut state = state();
        if state.is_null() {
            state = install();
        }
        if !state.is_null() && read(addr_of!((*state).patches)) != 0 {
            add_sd_modules(state);
        }
    }
    hook_continue()
}
