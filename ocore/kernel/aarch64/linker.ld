ENTRY(_start)

SECTIONS
{
    . = 0x40080000;
    __image_start = .;

    .text.boot : ALIGN(16) {
        KEEP(*(.text.boot))
    }

    .vectors : ALIGN(2048) {
        KEEP(*(.vectors))
    }

    .text.g2_user : ALIGN(4096) {
        __g2_user_text_start = .;
        *(.text.g2_user)
        *(.text.g2_user_fault)
        __g2_user_text_end = .;
    }

    .text : ALIGN(16) {
        *(.text)
        *(.text.*)
    }

    .rodata : ALIGN(16) {
        *(.rodata)
        *(.rodata.*)
    }

    .data : ALIGN(16) {
        *(.data)
        *(.data.*)
    }

    .bss (NOLOAD) : ALIGN(16) {
        __bss_start = .;
        *(.bss)
        *(.bss.*)
        *(COMMON)

        . = ALIGN(16);
        g2_kernel_stack_bottom = .;
        . += 0x10000;
        g2_kernel_stack_top = .;

        . = ALIGN(16);
        g2_user_a_stack_bottom = .;
        . += 0x4000;
        g2_user_a_stack_top = .;

        . = ALIGN(16);
        g2_user_b_stack_bottom = .;
        . += 0x4000;
        g2_user_b_stack_top = .;
        __bss_end = .;
    }

    __image_end = .;

    /DISCARD/ : {
        *(.comment)
        *(.note*)
        *(.eh_frame*)
    }
}

ASSERT((g2_vectors & 0x7ff) == 0, "AArch64 VBAR must be 2 KiB aligned")
ASSERT(__g2_user_text_end > __g2_user_text_start,
       "G2 must contain compiled EL0 O-core text")
ASSERT(__image_end < 0x41000000, "G2 image exceeds bounded QEMU virt window")
