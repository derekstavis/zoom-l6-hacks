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
  sizes an i.MX RT10xx part can map. The available identification points to
  the RT106x family, but some of each range may not exist.
- **Manifest:** it records CPUID, CCR, MPU type/control and selected region,
  cache identification, ITCMCR, DTCMCR, IOMUXC GPR14/16/17 and SoC
  identification/boot-mode registers, then one
  line per file with its base, length and either `ok` or unreadable ranges.
  These register values are sampled individually while MAIN is running;
  they are not a snapshot of bootloader handoff. A zero read is not by
  itself proof that a register is implemented or that memory is accessible.
  `MPU_RNR_BEFORE` and `MPU_RNR_AFTER` bracket the RBAR/RASR read, and
  `CSSELR_BEFORE` and `CSSELR_AFTER` bracket CCSIDR. If either pair differs,
  its associated values cannot be attributed to a single selected region or
  cache. The patch does not change either selector.
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
- **Hardware:** the earlier dump patch was used to obtain a physical dump;
  the additional register captures have not yet been tested on hardware.
  Unreadable ranges describe the running configuration at the time of the
  dump and do not by themselves establish physical memory capacity.
