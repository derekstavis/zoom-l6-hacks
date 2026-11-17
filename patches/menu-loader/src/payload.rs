#![no_std]
#![no_main]
//! Adds System > Patches to the L6max main menu. Prototype: fixed rows.
#[path = "../../api/patchrt.rs"]
#[macro_use]
pub mod patch_api;
use patch_api::function_entry_hook;
mod firmware;
use firmware as firmware_bindings;
#[path = "../../api/l6max/hooks.rs"]
mod hook_catalog;
use firmware::*;

use core::ptr::{read_volatile as read, write_volatile as write};

const MAGIC: u32 = 0x4d4c_4452; // "MLDR"

/// Heap block layout: header, then the relocated language 0 tables, string
/// buffers, nodes and child arrays. The relocated pointer table sits at a
/// fixed offset so the block can be found again from `0x80205118[0]`.
#[repr(C)]
struct Header {
    magic: u32,
    text_count: u32,
}
const POINTERS_OFFSET: usize = core::mem::size_of::<Header>();

const ROWS: [&str; 2] = ["menu-loader 1.0.0", "patch-loader 1.0.0"];

unsafe fn installed() -> bool {
    unsafe {
        let pointers = read(gui_text_pointers_by_language.address() as *const usize);
        let header = pointers.wrapping_sub(POINTERS_OFFSET) as *const Header;
        // Stock tables live in firmware .data; ours are heap blocks.
        !(0x8020_0000..0x8020_a100).contains(&pointers) && read(core::ptr::addr_of!((*header).magic)) == MAGIC
    }
}

fn align4(value: usize) -> usize {
    (value + 3) & !3
}

/// Copy `text` into `destination` as NUL-terminated UTF-16LE.
unsafe fn put_text(destination: *mut u16, text: &str) {
    unsafe {
        for (index, byte) in text.bytes().enumerate() {
            write(destination.add(index), byte as u16);
        }
        write(destination.add(text.len()), 0);
    }
}

unsafe fn copy_node(destination: *mut u8, template: *const u8) {
    unsafe {
        for offset in (0..MENU_NODE_LEN).step_by(4) {
            write(destination.add(offset) as *mut u32, read(template.add(offset) as *const u32));
        }
    }
}

unsafe fn set_u32(node: *mut u8, offset: usize, value: u32) {
    unsafe { write(node.add(offset) as *mut u32, value) }
}
unsafe fn set_u16(node: *mut u8, offset: usize, value: u16) {
    unsafe { write(node.add(offset) as *mut u16, value) }
}

unsafe fn install() {
    unsafe {
        if installed() {
            return;
        }
        let system = MENU_NODE_SYSTEM as *mut u8;
        let stock_children = read(system.add(MENU_NODE_CHILDREN) as *const u32) as *const u32;
        let stock_count = read(system.add(MENU_NODE_CHILD_COUNT) as *const u16) as usize;
        let stock_lengths = read(gui_text_lengths_by_language.address() as *const u32) as *const u16;
        let stock_pointers = read(gui_text_pointers_by_language.address() as *const u32) as *const u32;

        let titles = 1 + ROWS.len();
        let text_count = GUI_TEXT_RESOURCE_COUNT + titles;
        let mut size = POINTERS_OFFSET + text_count * 4;
        let lengths_offset = size;
        size = align4(size + text_count * 2);
        let strings_offset = size;
        let mut strings_len = 0;
        strings_len += ("Patches".len() + 1) * 2;
        for row in ROWS {
            strings_len += (row.len() + 1) * 2;
        }
        size = align4(size + strings_len);
        let nodes_offset = size;
        size += (1 + ROWS.len()) * MENU_NODE_LEN;
        let system_children_offset = size;
        size += (stock_count + 1) * 4;
        let row_children_offset = size;
        size += ROWS.len() * 4;

        let block = pvPortMalloc(size);
        if block.is_null() {
            return;
        }
        let pointers = block.add(POINTERS_OFFSET) as *mut u32;
        let lengths = block.add(lengths_offset) as *mut u16;
        for id in 0..GUI_TEXT_RESOURCE_COUNT {
            write(pointers.add(id), read(stock_pointers.add(id)));
            write(lengths.add(id), read(stock_lengths.add(id)));
        }
        let mut text = block.add(strings_offset) as *mut u16;
        let mut add_text = |id: usize, value: &str| {
            put_text(text, value);
            write(pointers.add(id), text as u32);
            write(lengths.add(id), value.len() as u16);
            text = text.add(value.len() + 1);
        };
        let patches_title = GUI_TEXT_RESOURCE_COUNT;
        add_text(patches_title, "Patches");
        for (row, value) in ROWS.iter().enumerate() {
            add_text(patches_title + 1 + row, value);
        }

        let nodes = block.add(nodes_offset);
        let patches = nodes;
        let row_children = block.add(row_children_offset) as *mut u32;
        let leaf_action = read(system.add(MENU_NODE_RESERVED) as *const u32);
        for row in 0..ROWS.len() {
            let node = nodes.add((1 + row) * MENU_NODE_LEN);
            copy_node(node, system);
            set_u32(node, MENU_NODE_INDEX, row as u32);
            set_u32(node, MENU_NODE_FLAGS, 0);
            set_u32(node, MENU_NODE_REQUIRES, 0);
            set_u16(node, MENU_NODE_TITLE, (patches_title + 1 + row) as u16);
            set_u32(node, MENU_NODE_CHILDREN, 0);
            set_u16(node, MENU_NODE_CHILD_COUNT, 0);
            set_u16(node, MENU_NODE_SELECTED, 0);
            set_u16(node, MENU_NODE_SCROLL, 0);
            set_u32(node, MENU_NODE_ACTION, leaf_action);
            write(row_children.add(row), node as u32);
        }
        copy_node(patches, system);
        set_u32(patches, MENU_NODE_INDEX, stock_count as u32);
        set_u32(patches, MENU_NODE_FLAGS, 0);
        set_u32(patches, MENU_NODE_REQUIRES, 0);
        set_u16(patches, MENU_NODE_TITLE, patches_title as u16);
        set_u32(patches, MENU_NODE_CHILDREN, row_children as u32);
        set_u16(patches, MENU_NODE_CHILD_COUNT, ROWS.len() as u16);
        set_u16(patches, MENU_NODE_SELECTED, 0);
        set_u16(patches, MENU_NODE_SCROLL, 0);

        let system_children = block.add(system_children_offset) as *mut u32;
        for index in 0..stock_count {
            write(system_children.add(index), read(stock_children.add(index)));
        }
        write(system_children.add(stock_count), patches as u32);

        write(block as *mut Header, Header { magic: MAGIC, text_count: text_count as u32 });
        // Publish text before any node can reference the new IDs.
        write(gui_text_lengths_by_language.address() as *mut u32, lengths as u32);
        write(gui_text_pointers_by_language.address() as *mut u32, pointers as u32);
        write(system.add(MENU_NODE_CHILDREN) as *mut u32, system_children as u32);
        write(system.add(MENU_NODE_CHILD_COUNT) as *mut u16, (stock_count + 1) as u16);
    }
}

#[function_entry_hook(menu_window_enter)]
pub unsafe fn menu_window_enter_hook() -> u32 {
    unsafe { install() };
    hook_continue()
}
