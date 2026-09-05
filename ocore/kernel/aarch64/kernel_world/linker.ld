OUTPUT_ARCH(aarch64)
ENTRY(_start)
SECTIONS
{
    . = 0x40080000;
    __monitor_start = .;
    .text : ALIGN(4096) { KEEP(*(.text.boot)) *(.text) *(.text.*) }
    .vectors : ALIGN(2048) { KEEP(*(.vectors)) }
    .rodata : ALIGN(4096) { *(.rodata) *(.rodata.*) }
    .data : ALIGN(4096) { *(.data) *(.data.*) }
    .bss (NOLOAD) : ALIGN(4096) {
        __bss_start = .;
        *(.bss) *(.bss.*) *(COMMON)
        . = ALIGN(4096);
        kw_stack_bottom = .;
        . += 0x20000;
        kw_stack_top = .;
        __bss_end = .;
    }
    __monitor_end = .;
    /DISCARD/ : { *(.comment) *(.note*) *(.eh_frame*) }
}
ASSERT(__monitor_end <= 0x41000000, "KernelWorld monitor exceeds reserved host window")
ASSERT((kw_vectors & 0x7ff) == 0, "EL2 vectors must be 2KiB aligned")
