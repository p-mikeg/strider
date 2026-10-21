SECTIONS
{
  . = 0x10000;
  .text : { *(.text) }
  . = ALIGN(0x1000);
  .rodata : { *(.rodata) }
  .rela.dyn : { *(.rela .rela.*) }
  /DISCARD/ : { *(.interp .dynamic) *(.dynsym .dynstr .hash .gnu.hash) }
}
