OUTPUT_FORMAT(elf64-x86-64)
OUTPUT_ARCH(i386:x86-64)
ENTRY(_start)

PHDRS
{
  kernel_meta PT_LOAD FLAGS(4);
  kernel_text PT_LOAD FLAGS(5);
  kernel_rodata PT_LOAD FLAGS(4);
  kernel_data PT_LOAD FLAGS(6);
  user_text PT_LOAD FLAGS(5);
  user_stack PT_LOAD FLAGS(6);
  xen_note PT_NOTE FLAGS(4);
  stack PT_GNU_STACK FLAGS(6);
}

SECTIONS
{
  . = 1M;

  .multiboot : ALIGN(8) {
    KEEP(*(.multiboot))
  } :kernel_meta

  .note.Xen : ALIGN(4) {
    KEEP(*(.note.Xen))
  } :kernel_meta :xen_note

  .text : ALIGN(4K) {
    *(.text.boot)
    *(.text.kernel)
    *(.text)
  } :kernel_text

  .rodata : ALIGN(4K) {
    *(.rodata)
  } :kernel_rodata

  .data : ALIGN(4K) {
    *(.data)
  } :kernel_data

  .bss : ALIGN(4K) {
    *(COMMON)
    *(.bss)
  } :kernel_data

  __kernel_end = .;

  /*
   * One dedicated 2 MiB user image page. Its PDE is U/S=1, R/W=0 and
   * executable. The adjacent anonymous 2 MiB page is the writable, NX user
   * stack. Keeping both outside the kernel and allocator ranges makes this
   * first privilege-boundary proof auditable without pretending that a
   * per-process address-space manager already exists.
   */
  .user 0x01000000 : ALIGN(4K) {
    __user_text_start = .;
    KEEP(*(.text.user))
    KEEP(*(.rodata.user))
    __user_text_end = .;
  } :user_text

  /* Extend the RX PT_LOAD's memory size across the full huge page so the ELF
   * loader reserves and zeros every byte that CPL3 may read. NOLOAD keeps
   * those zeros out of the kernel file. */
  .user_text_zero __user_text_end (NOLOAD) : {
    . += 0x01200000 - __user_text_end;
  } :user_text

  /* Represent the complete RW/NX stack as its own zero-fill PT_LOAD. */
  .user_stack 0x01200000 (NOLOAD) : {
    __user_stack_bottom = .;
    . += 0x00200000;
    __user_stack_top = .;
  } :user_stack

  ASSERT(__kernel_end <= 0x00400000,
         "O-core kernel overlaps the bootstrap page allocator")
  ASSERT(__user_text_end <= 0x01200000,
         "O-core user image exceeds its dedicated 2 MiB mapping")

  /DISCARD/ : {
    *(.comment)
    *(.eh_frame)
    *(.note*)
  }
}
