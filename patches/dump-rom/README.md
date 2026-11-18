# Memory dump

Adds **System > Dump ROM** to the L6max. It copies the main NOR flash, the
boot ROM, the tightly coupled memories, on-chip RAM and SDRAM to files in the
root of the SD card. The main purpose is recovering the bootloader: it lives
in the first 320 KiB of the NOR, and no update package carries it.

Requires [menu-loader](../menu-loader/README.md), which `l6patch` adds
automatically. L6max 1.10 only, installed in firmware.

## Files

| File | Source | Base | Size |
| --- | --- | --- | ---: |
| `L6MAX_NOR.BIN` | Main NOR, physical offsets | `0x000000` | 2 MiB |
| `L6MAX_BOOTROM.BIN` | Boot ROM | `0x00200000` | 128 KiB |
| `L6MAX_ITCM.BIN` | Instruction TCM | `0x00000000` | 512 KiB |
| `L6MAX_DTCM.BIN` | Data TCM | `0x20000000` | 512 KiB |
| `L6MAX_OCRAM.BIN` | On-chip RAM | `0x20200000` | 1 MiB |
| `L6MAX_SDRAM.BIN` | SDRAM, including the running MAIN image | `0x80000000` | 64 MiB |
| `L6MAX_DUMP.TXT` | Manifest | | |

A dump needs about 69 MiB free. Existing files with these names are replaced.

- **Bootloader:** `L6MAX_NOR.BIN` bytes `0x00000`–`0x4ffff`, with its
  version at `0x4fffc`. The [storage map](../../docs/emulator/storage.md#main-nor-map)
  covers the rest of the NOR.
- **Memory ranges:** they cover what the firmware uses, rounded up to the
  sizes an i.MX RT10xx part can map. The L6max processor is not identified,
  so some of each range may not exist.
- **Manifest:** it records the CPUID, MPU and i.MX RT10xx identification
  registers, then one line per file with its base, length and either `ok` or
  the address ranges that could not be read.
- **Checking the NOR dump:** the manifest's `NOR CHECKSUM` line should match
  the CHECKSUM on the firmware version screen. The firmware computes that
  value at startup: the low 16 bits of the byte sum of NOR
  `0x000000`–`0x1f7fff`, which includes the bootloader. The manifest also
  gives the byte sum of the bootloader region alone.

RAM is copied while the firmware runs, so it is a snapshot taken over several
seconds, not a single instant.

## How it works

- **NOR:** read in 32 KiB pieces through the firmware's own NOR driver, which
  holds the driver lock and waits for the flash to be idle. The patch binds
  only the read routine, not erase or program.
- **Memory:** copied one 4 KiB page at a time with bus faults suppressed:
  - FAULTMASK is set and `CCR.BFHFNMIGN` enabled, so a read from an address
    with nothing behind it sets a status bit instead of faulting.
  - If the MPU is enabled, `MPU_CTRL.HFNMIENA` is cleared for the copy, so
    the MPU does not apply at that priority either.
  - A page whose copy set the status bit is written as zeros and listed in
    the manifest. Every file therefore has its full size, and file offsets
    equal addresses.
  - Interrupts are masked only while one page is copied, a few microseconds
    at a time.
- **SD card:** written with the stock file routines in 32 KiB writes.
- **Display:** the stock SD busy animation shows during the dump. Then
  **Done!**, **SD Card Full!** or **SD Write Error!** appears. Without a card
  the item is unavailable.

The menu item is also unavailable while the recorder records or plays.

## Validation

- **Emulator:** the `dump-rom` smoke case runs a dump. It then checks that
  `L6MAX_NOR.BIN` equals the emulator's NOR byte for byte, and that every
  file has its full size.
- **Hardware:** not tested. In particular, it is unknown which parts of the
  memory ranges read as unmapped on the real processor. The manifest records
  that.
