//! QEMU-only allocator bounds for L6max firmware 1.10.

use firmware_api::l6max;

/// The first allocation's payload, after the first block's 8-byte header.
pub const FIRST_PAYLOAD: usize = l6max::heap_first_block as usize + 8;
/// The end-marker block; no payload reaches past it.
pub const END: usize = l6max::heap_end_block as usize;

/// Check an aligned payload against the heap's first header and end marker.
pub fn valid_heap_allocation(address: usize, len: usize) -> bool {
    len != 0
        && address >= FIRST_PAYLOAD
        && address % 8 == 0
        && address.checked_add(len).is_some_and(|end| end <= END)
}
