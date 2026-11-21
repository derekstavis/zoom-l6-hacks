ENTRY(0)
SECTIONS {
  . = PAYLOAD_BASE;
  .text : {
    /* The identity record comes first so the loader and menu-loader find it
       at the module base. */
    KEEP(*(.rodata.l6_patch_info))
    KEEP(*(.text.*_hook))
    *(.text*)
    *(.rodata*)
  }
  .data : {
    . = ALIGN(4);
    __module_data_start = .;
    *(.data*)
    . = ALIGN(4);
    __module_data_end = .;
  }
  .module_bss (NOLOAD) : {
    . = ALIGN(4);
    __module_bss_start = .;
    *(.bss*)
    *(COMMON)
    __module_memory_end = .;
  }
  /* Hook metadata for the build tools only: after the module's memory, so
     the loader never allocates it. */
  .patch_hooks : { KEEP(*(.patch_hooks)) }
  /DISCARD/ : { *(.ARM.exidx*) *(.ARM.extab*) *(.comment*) }
}
