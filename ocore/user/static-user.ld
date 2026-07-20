OUTPUT_FORMAT(elf64-x86-64)
OUTPUT_ARCH(i386:x86-64)
ENTRY(_start)

PHDRS
{
  text PT_LOAD FLAGS(5);
  rodata PT_LOAD FLAGS(4);
  data PT_LOAD FLAGS(6);
  stack PT_GNU_STACK FLAGS(6);
}

SECTIONS
{
  /* Each process receives an independent address space, so both personality
   * images intentionally use the same bounded preferred base. */
  . = 0x02000000;
  .text : ALIGN(4K) {
    KEEP(*(.text.entry))
    *(.text .text.*)
  } :text
  . = ALIGN(4K);
  .rodata : {
    *(.rodata .rodata.*)
  } :rodata
  . = ALIGN(4K);
  .data : {
    *(.data .data.*)
  } :data
  .bss (NOLOAD) : {
    *(.bss .bss.*)
    *(COMMON)
  } :data

  ASSERT(SIZEOF(.text) > 0, "static user ELF has no executable text")
  ASSERT(. <= 0x02100000, "static user ELF exceeds its 1 MiB load window")

  /DISCARD/ : {
    *(.comment)
    *(.eh_frame)
    *(.note*)
  }
}
