ENTRY(__l6_patch_info)
SECTIONS {
  . = PAYLOAD_BASE;
  .text : { KEEP(*(.rodata.l6_patch_info)) KEEP(*(.rodata.l6_menu_items)) *(.text*) *(.rodata*) }
  .patch_hooks : { KEEP(*(.patch_hooks)) }
  .state (NOLOAD) : { *(.data*) *(.bss*) }
  ASSERT(SIZEOF(.state) == 0, "patch must not reserve mutable RAM")
  /DISCARD/ : { *(.ARM.exidx*) *(.ARM.extab*) *(.comment*) }
}
