OUTPUT_FORMAT(elf64-x86-64)
OUTPUT_ARCH(i386:x86-64)
ENTRY(_start)
PHDRS {
  metadata PT_LOAD FLAGS(4);
  text PT_LOAD FLAGS(5);
  rodata PT_LOAD FLAGS(4);
  data PT_LOAD FLAGS(6);
  xen PT_NOTE FLAGS(4);
  stack PT_GNU_STACK FLAGS(6);
}
SECTIONS {
  . = 1M;
  .multiboot : ALIGN(8) { KEEP(*(.multiboot)) } :metadata
  .note.Xen : ALIGN(4) { KEEP(*(.note.Xen)) } :metadata :xen
  .text : ALIGN(4096) { *(.text.boot) *(.text*) } :text
  .rodata : ALIGN(4096) { *(.rodata*) } :rodata
  .data : ALIGN(4096) { *(.data*) } :data
  .bss : ALIGN(4096) { *(COMMON) *(.bss*) } :data
  ASSERT(. < 16M, "native cluster image exceeds its low-memory DMA profile")
  /DISCARD/ : { *(.comment) *(.eh_frame) *(.note.GNU-stack) }
}
