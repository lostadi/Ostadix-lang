.arch armv8-a

.section .text.boot, "ax", %progbits
.balign 16
.global _start
.type _start, %function
_start:
    msr daifset, #0xf
    mrs x1, CurrentEL
    cmp x1, #0x8
    b.eq g2_drop_el2
    b g2_early_halt

g2_drop_el2:
    // EL2 remains resident.  Install its vector before changing privilege,
    // retain a private EL2 stack, give EL1 physical-counter access, and enter
    // 64-bit EL1h.  The G2 corpus refuses an EL1-only direct-kernel entry.
    adrp x0, g2_el2_vectors
    add x0, x0, :lo12:g2_el2_vectors
    msr vbar_el2, x0
    adrp x0, g2_el2_stack_top
    add x0, x0, :lo12:g2_el2_stack_top
    mov sp, x0
    mov x0, #3
    msr cnthctl_el2, x0
    msr cntvoff_el2, xzr
    movz x0, #0x8000, lsl #16
    msr hcr_el2, x0
    adrp x0, g2_kernel_stack_top
    add x0, x0, :lo12:g2_kernel_stack_top
    msr sp_el1, x0
    adrp x0, g2_el1_entry
    add x0, x0, :lo12:g2_el1_entry
    msr elr_el2, x0
    mov x0, #0x3c5
    msr spsr_el2, x0
    isb
    eret

// Narrow G2 EL1->EL2 ABI probe.  EL2 accepts exactly one operation domain and
// request value.  The wrapper proves the EL1 stack and preserved sentinel
// registers survive the HVC/ERET round trip before compiled O-core reports it.
.global g2_el2_probe
.type g2_el2_probe, %function
g2_el2_probe:
    stp x19, x20, [sp, #-32]!
    stp x29, x30, [sp, #16]
    mov x19, sp
    movz x0, #0x0001
    movk x0, #0x4f4d, lsl #16
    movz x1, #0x1111
    movk x1, #0x1111, lsl #16
    movz x2, #0x2222
    movk x2, #0x2222, lsl #16
    movz x3, #0x3333
    movk x3, #0x3333, lsl #16
    movz x4, #0x4444
    movk x4, #0x4444, lsl #16
    movz x5, #0x5555
    movk x5, #0x5555, lsl #16
    movz x6, #0x6666
    movk x6, #0x6666, lsl #16
    movz x7, #0x7777
    movk x7, #0x7777, lsl #16
    hvc #0x4f4d
    movz x8, #0x0002
    movk x8, #0x4f4d, lsl #16
    cmp x0, x8
    b.ne 1f
    movz x8, #0x1111
    movk x8, #0x1111, lsl #16
    cmp x1, x8
    b.ne 1f
    movz x8, #0x2222
    movk x8, #0x2222, lsl #16
    cmp x2, x8
    b.ne 1f
    movz x8, #0x3333
    movk x8, #0x3333, lsl #16
    cmp x3, x8
    b.ne 1f
    movz x8, #0x4444
    movk x8, #0x4444, lsl #16
    cmp x4, x8
    b.ne 1f
    movz x8, #0x5555
    movk x8, #0x5555, lsl #16
    cmp x5, x8
    b.ne 1f
    movz x8, #0x6666
    movk x8, #0x6666, lsl #16
    cmp x6, x8
    b.ne 1f
    movz x8, #0x7777
    movk x8, #0x7777, lsl #16
    cmp x7, x8
    b.ne 1f
    cmp sp, x19
    b.ne 1f
    mov x0, #1
    b 2f
1:
    mov x0, xzr
2:
    ldp x29, x30, [sp, #16]
    ldp x19, x20, [sp], #32
    ret
.size g2_el2_probe, . - g2_el2_probe

g2_el1_entry:
    msr spsel, #1
    adrp x0, g2_kernel_stack_top
    add x0, x0, :lo12:g2_kernel_stack_top
    mov sp, x0

    // QEMU's direct-kernel path arrives without stage-1 translation.  Preserve
    // required implementation bits while explicitly keeping MMU/caches off for
    // this bounded single-address-space G2 corpus.
    mrs x0, sctlr_el1
    bic x0, x0, #1
    bic x0, x0, #4
    bic x0, x0, #0x1000
    msr sctlr_el1, x0
    isb

    adrp x0, g2_vectors
    add x0, x0, :lo12:g2_vectors
    msr vbar_el1, x0
    isb

    // Clear every O-core static and saved-context/stack byte deterministically.
    adrp x0, __bss_start
    add x0, x0, :lo12:__bss_start
    adrp x1, __bss_end
    add x1, x1, :lo12:__bss_end
1:
    cmp x0, x1
    b.hs 2f
    stp xzr, xzr, [x0], #16
    b 1b
2:
    bl g2_kernel_init
    movz x1, #1
    movk x1, #1, lsl #32
    cmp x0, x1
    b.ne g2_early_halt

    // First real compiled O-core principal.  EL0t plus masked asynchronous
    // exceptions ensures all observed crossings are synchronous SVC or BRK.
    adrp x1, g2_user_a_stack_top
    add x1, x1, :lo12:g2_user_a_stack_top
    msr sp_el0, x1
    adrp x2, g2_user_a_entry
    add x2, x2, :lo12:g2_user_a_entry
    msr elr_el1, x2
    mov x3, #0x3c0
    msr spsr_el1, x3
    movz x0, #1
    movk x0, #1, lsl #32
    mov x1, xzr
    mov x2, xzr
    mov x3, xzr
    mov x4, xzr
    mov x5, xzr
    mov x6, xzr
    mov x7, xzr
    mov x8, xzr
    mov x9, xzr
    mov x10, xzr
    mov x11, xzr
    mov x12, xzr
    mov x13, xzr
    mov x14, xzr
    mov x15, xzr
    mov x16, xzr
    mov x17, xzr
    mov x29, xzr
    mov x30, xzr
    isb
    eret
.size _start, . - _start

.global g2_serial_putc
.type g2_serial_putc, %function
g2_serial_putc:
    movz x2, #0x0900, lsl #16
3:
    ldr w1, [x2, #0x18]
    tbnz w1, #5, 3b
    str w0, [x2]
    ret
.size g2_serial_putc, . - g2_serial_putc

.global g2_counter_read
.type g2_counter_read, %function
g2_counter_read:
    isb
    mrs x0, cntpct_el0
    ret
.size g2_counter_read, . - g2_counter_read

.global g2_arch_halt
.type g2_arch_halt, %function
g2_arch_halt:
4:
    wfe
    b 4b
.size g2_arch_halt, . - g2_arch_halt

g2_early_halt:
5:
    wfe
    b 5b

// The B principal calls this ordinary AAPCS64 symbol from compiled .oc.  BRK
// raises a genuine lower-EL synchronous exception; it is not an exit syscall.
.section .text.g2_user_fault, "ax", %progbits
.balign 16
.global g2_user_fault
.type g2_user_fault, %function
g2_user_fault:
    brk #0x742
6:
    b 6b
.size g2_user_fault, . - g2_user_fault

.section .note.GNU-stack, "", %progbits
