ENTRY(menu_window_enter_hook)
SECTIONS {
  . = PAYLOAD_BASE;
  .text : { KEEP(*(.text.menu_window_enter_hook)) *(.text*) *(.rodata*) }
  .patch_hooks : { KEEP(*(.patch_hooks)) }
  .state (NOLOAD) : { *(.data*) *(.bss*) }
  ASSERT(SIZEOF(.state) == 0, "patch must not reserve mutable RAM")
  /DISCARD/ : { *(.ARM.exidx*) *(.ARM.extab*) *(.comment*) }
}
