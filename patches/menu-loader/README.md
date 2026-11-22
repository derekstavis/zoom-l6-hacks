# Menu loader

Menu loader adds **System > Patches** to the L6max main menu and builds the
menu items that other patches declare. It is installed in firmware only and
supports L6max 1.10.

**Patches** lists every patch installed in firmware, in installation order,
followed by each patch loaded from the SD card, marked `(SD)`:

```text
patch-loader 1.0.0
menu-loader 1.0.0
knob-dialog 1.0.0
midi-sync 1.0.0 (SD)
```

The rows are read-only. Long titles scroll when selected, as stock rows do.
Patches loaded from the card appear the next time the menu opens. SD patches
need the [Patch loader](../patch-loader/README.md) with registry layout 2;
without it, Patches lists the firmware patches only.

## Adding menu items from a patch

A patch declares its items once, at the root of its payload, with
`menu_items!` from the shared patch runtime:

```rust
menu_items! {
    SYSTEM => "Send MIDI State" => action(send_state_from_menu);
}

unsafe extern "C" fn send_state_from_menu() -> u32 {
    unsafe { broadcast_state() };
    0
}
```

Each entry names a parent, a title of up to 23 printable ASCII characters,
and either `action(function)` or `info` for a read-only row. Add
`, requires MASK` to gate the item with the stock requirement bits: `0x01`
needs an SD card, `0x02` a valid card, `0x10` the recorder not recording and
`0x20` not playing. A gated item stays visible and shows the stock
notification when entered.

| Parent | Menu |
| --- | --- |
| `MAIN` | The top-level menu, after Help |
| `PROJECT` | Project |
| `SOUND_PAD` | Sound Pad |
| `MIXER` | Mixer |
| `SYSTEM` | System, after Patches |

An action is an `unsafe extern "C" fn() -> u32`. The stock menu calls it on
the UI task when the item is entered; return nonzero to redraw the menu. It
must not block, because the UI task also handles the panel and recorder keys.

The patch needs no hooks of its own and does not depend on menu-loader. The
builder records the declared items in the patch's identity record (below);
when menu-loader is not installed, nothing reads them. The
[midi-sync](../midi-sync/README.md) L6max profile declares
**System > Send MIDI State** this way.

## How it works

Every payload the patch builder produces carries an identity record with the
names and versions of the patches it contains and the menu items they
declare. Static firmware payloads link their records into a ring; an SD
module carries its record at offset 0. See the
[patch API guide](../../docs/development/patch-api.md#patch-identity-and-menu-items).

Menu loader hooks the entry of the main menu window (`menu_window_enter`,
`0x8003cfb0`). On the first entry it:

1. allocates one heap block holding its state and a copy of the language 0
   text tables with room for 128 new text IDs, and points the firmware's
   language 0 table slots (`0x802050fc`, `0x80205118`) at it;
2. adds a Patches node to System, modeled on System's own node with the stock
   submenu action;
3. walks the ring of firmware identity records and adds a row per patch and a
   node per declared menu item.

On every entry it also reads the Patch loader's module table and adds rows
and items for modules loaded since the last entry.

Menu titles are text resource IDs, so each new title is a heap string with a
new ID from the reserved range. Every firmware text lookup goes through the
two table slots, so the moved tables need no other change, and the block is
found again from the pointer slot and its magic word. The patch reserves no
firmware RAM. Each node and title is a separate allocation that lives until
reset. Adding a child gives the parent a new, one entry longer child array;
the old array is left in place. A boot that lists patches before the card
mounts therefore leaves a few words of earlier arrays allocated.

Rows and items stop being added when the 128 reserved text IDs or the heap
run out.

## Build and test

```sh
cargo run --offline --locked -p l6patch -- firmware STOCK_DIR OUTPUT_DIR \
  --loader --patch menu-loader
cargo run --offline --locked -p l6test -- run --case sd-menu-loader \
  --model l6max --backend headless --jobs 1
```

The `sd-menu-loader` case installs menu-loader and knob-dialog in firmware
and loads midi-sync from the card. It compares the System menu, the Patches
list and the SD-declared item with reference images. The
[`menu-loader-l6max`](../../tools/l6dbg/recipes/menu-loader-l6max.toml)
`l6dbg` recipe also selects **Send MIDI State** and receives midi-sync's 94
Control Change messages on USB MIDI.

**These emulator checks pass; physical hardware has not been tested.**

## Limitations

- L6max 1.10 only, and installable in firmware only with `l6patch`. The
  browser patcher and the emulator's patch selector link the identity records
  of the patches they install, but do not offer menu-loader itself yet.
- Items can extend the five stock menus above. Patch-defined submenus and
  value lists are not supported yet.
- Only language 0 exists in this firmware, and titles are ASCII.
- Unloading is not supported: rows and items stay until reset, matching the
  Patch loader, which keeps modules resident after card removal.
