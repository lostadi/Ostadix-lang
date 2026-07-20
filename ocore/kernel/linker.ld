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

  __kernel_meta_start = .;
  .multiboot : ALIGN(8) {
    KEEP(*(.multiboot))
  } :kernel_meta

  .note.Xen : ALIGN(4) {
    KEEP(*(.note.Xen))
  } :kernel_meta :xen_note
  . = ALIGN(4K);
  __kernel_meta_end = .;

  __kernel_text_start = .;
  .text : ALIGN(4K) {
    *(.text.boot)
    *(.text.kernel)
    *(.text)
  } :kernel_text
  . = ALIGN(4K);
  __kernel_text_end = .;

  __kernel_rodata_start = .;
  .rodata : ALIGN(4K) {
    *(.rodata)
  } :kernel_rodata
  . = ALIGN(4K);
  __kernel_rodata_end = .;

  __kernel_data_start = .;
  .data : ALIGN(4K) {
    *(.data)
  } :kernel_data

  .bss : ALIGN(4K) {
    *(COMMON)
    *(.bss)
  } :kernel_data

  . = ALIGN(4K);
  __kernel_data_end = .;
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
    __user_payload_end = .;
  } :user_text

  /* Test-only architectural SYSCALL at the end of the permitted return-RIP
   * interval. Its two-byte instruction leaves RCX at the first excluded byte. */
  .user_return_edge 0x0100fffe : {
    KEEP(*(.text.user.return_edge))
    __user_exec_end = .;
  } :user_text

  /* Extend the RX PT_LOAD's memory size across the full huge page so the ELF
   * loader reserves and zeros every byte that CPL3 may read. NOLOAD keeps
   * those zeros out of the kernel file. */
  .user_text_zero __user_exec_end (NOLOAD) : {
    . += 0x01200000 - __user_exec_end;
  } :user_text

  /* Leave the first 4 KiB non-present as the user-stack guard. The remaining
   * pages are represented by an RW PT_LOAD and mapped NX by bootstrap PTEs. */
  __user_stack_guard_start = 0x01200000;
  __user_stack_guard_end = 0x01201000;
  .user_stack 0x01201000 (NOLOAD) : {
    __user_stack_bottom = .;
    . += 0x001ff000;
    __user_stack_top = .;
  } :user_stack

  /* Keep at least 512 KiB free below the bootstrap's 4 MiB mapping ceiling so
   * a successful link cannot silently consume all native growth headroom. */
  ASSERT(__kernel_end <= 0x00380000,
         "O-core bootstrap kernel leaves less than 512 KiB headroom")
  ASSERT(__user_payload_end <= 0x0100fffe,
         "O-core user payload overlaps the syscall-return edge probe")

  /DISCARD/ : {
    *(.comment)
    *(.eh_frame)
    *(.note*)
  }
}
