OUTPUT_FORMAT(elf64-x86-64)
OUTPUT_ARCH(i386:x86-64)
ENTRY(_start)

SECTIONS
{
  . = 1M;

  .multiboot : ALIGN(8) {
    KEEP(*(.multiboot))
  }

  .note.Xen : ALIGN(4) {
    KEEP(*(.note.Xen))
  }

  .text : ALIGN(4K) {
    *(.text.boot)
    *(.text.kernel)
    *(.text .text.*)
  }

  .rodata : ALIGN(4K) {
    *(.rodata .rodata.*)
  }

  .data : ALIGN(4K) {
    *(.data .data.*)
  }

  .bss : ALIGN(4K) {
    *(COMMON)
    *(.bss .bss.*)
  }

  /DISCARD/ : {
    *(.comment)
    *(.eh_frame)
    *(.note*)
  }
}
