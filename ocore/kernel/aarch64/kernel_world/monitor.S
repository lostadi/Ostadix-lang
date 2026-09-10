.arch armv8-a

// Isolated QEMU/TCG bring-up monitor. All guest addresses use stage 2;
// compiled O-core and its broker execute at EL2 for this bounded lane.
.section .text.boot, "ax", %progbits
.global _start
.type _start, %function
_start:
    msr daifset, #0xf
    mrs x0, CurrentEL
    cmp x0, #8
    b.ne kw_halt
    adrp x0, kw_stack_top
    add x0, x0, :lo12:kw_stack_top
    mov sp, x0
    adrp x0, __bss_start
    add x0, x0, :lo12:__bss_start
    adrp x1, __bss_end
    add x1, x1, :lo12:__bss_end
1:
    cmp x0, x1
    b.hs 2f
    str xzr, [x0], #8
    b 1b
2:
    adrp x0, kw_vectors
    add x0, x0, :lo12:kw_vectors
    msr vbar_el2, x0
    msr cptr_el2, xzr
    msr mdcr_el2, xzr
    mov x0, #3
    msr cnthctl_el2, x0
    msr cntvoff_el2, xzr
    mrs x0, midr_el1
    msr vpidr_el2, x0
    mrs x0, mpidr_el1
    msr vmpidr_el2, x0
    bl kw_stage2_install
    bl kw_monitor_init
    cbz x0, kw_halt

    // Linux's documented non-secure EL1, MMU-off direct Image entry.
    movz x0, #0x0800
    movk x0, #0x30d0, lsl #16
    msr sctlr_el1, x0
    msr ttbr0_el1, xzr
    msr ttbr1_el1, xzr
    msr tcr_el1, xzr
    msr mair_el1, xzr
    msr vbar_el1, xzr
    msr contextidr_el1, xzr
    msr cntkctl_el1, xzr
    mov x0, #0x300000
    msr cpacr_el1, x0
    movz x0, #0
    movk x0, #0x4000, lsl #16
    msr elr_el2, x0
    mov x0, #0x3c5
    msr spsr_el2, x0
    movz x0, #0
    movk x0, #0x47e0, lsl #16
    mov x1, xzr
    mov x2, xzr
    mov x3, xzr
    dsb sy
    isb
    eret
.size _start, . - _start

.section .text, "ax", %progbits
.type kw_stage2_install, %function
kw_stage2_install:
    adrp x0, kw_stage2_root
    add x0, x0, :lo12:kw_stage2_root
    adrp x1, kw_stage2_devices
    add x1, x1, :lo12:kw_stage2_devices
    orr x2, x1, #3
    str x2, [x0]
    adrp x2, kw_stage2_ram
    add x2, x2, :lo12:kw_stage2_ram
    orr x3, x2, #3
    str x3, [x0, #8]
    // Guest 512MiB IPA 0x40000000 maps only PA 0x48000000..0x68000000.
    movz x3, #0x07fd
    movk x3, #0x4800, lsl #16
    mov x4, #256
1:
    str x3, [x2], #8
    add x3, x3, #0x200, lsl #12
    subs x4, x4, #1
    b.ne 1b
    // Only the outer emulated GICv2 is directly assigned. UART and virtio
    // remain unmapped and therefore dispatch through actual stage-2 aborts.
    adrp x2, kw_stage2_gic
    add x2, x2, :lo12:kw_stage2_gic
    orr x3, x2, #3
    str x3, [x1, #512]
    movz x3, #0x06c3
    movk x3, #0x0800, lsl #16
    movk x3, #0x0040, lsl #48
    mov x4, #32
2:
    str x3, [x2], #8
    add x3, x3, #1, lsl #12
    subs x4, x4, #1
    b.ne 2b
    movz x1, #0x3560
    movk x1, #0x8000, lsl #16
    msr vtcr_el2, x1
    mov x1, #1
    lsl x1, x1, #48
    orr x0, x0, x1
    msr vttbr_el2, x0
    dsb sy
    tlbi vmalls12e1is
    dsb sy
    isb
    // RW=1, VM=1, TSC=1. Physical GIC interrupts go directly to guest EL1;
    // this lane assigns one emulated controller to one guest, not an IRQ VMM.
    movz x0, #1
    movk x0, #0x8008, lsl #16
    msr hcr_el2, x0
    isb
    ret
.size kw_stage2_install, . - kw_stage2_install

.global kw_revoke_guest_ram
.type kw_revoke_guest_ram, %function
kw_revoke_guest_ram:
    adrp x0, kw_stage2_ram
    add x0, x0, :lo12:kw_stage2_ram
    mov x1, #512
1:
    str xzr, [x0], #8
    subs x1, x1, #1
    b.ne 1b
    dsb ishst
    tlbi vmalls12e1is
    dsb ish
    isb
    ret
.size kw_revoke_guest_ram, . - kw_revoke_guest_ram

.global kw_memory_barrier
.type kw_memory_barrier, %function
kw_memory_barrier:
    dmb sy
    ret
.size kw_memory_barrier, . - kw_memory_barrier

.global kw_irq_level
.type kw_irq_level, %function
kw_irq_level:
    movz x1, #0x0204
    movk x1, #0x0800, lsl #16
    cbnz x0, 1f
    add x1, x1, #0x80
1:
    mov w2, #0x10000
    str w2, [x1]
    dsb sy
    ret
.size kw_irq_level, . - kw_irq_level

.global kw_counter_read
.type kw_counter_read, %function
kw_counter_read:
    mrs x0, cntpct_el0
    ret
.size kw_counter_read, . - kw_counter_read

.global kw_counter_frequency
.type kw_counter_frequency, %function
kw_counter_frequency:
    mrs x0, cntfrq_el0
    ret
.size kw_counter_frequency, . - kw_counter_frequency

.global kw_serial_putc
.type kw_serial_putc, %function
kw_serial_putc:
    mov x1, #0x09000000
1:
    ldr w2, [x1, #0x18]
    tbnz w2, #5, 1b
    str w0, [x1]
    ret
.size kw_serial_putc, . - kw_serial_putc

.global kw_halt
.type kw_halt, %function
kw_halt:
    msr daifset, #0xf
1:
    wfe
    b 1b
.size kw_halt, . - kw_halt

.macro KW_VECTOR target
    b \target
    .space 124
.endm
.section .vectors, "ax", %progbits
.balign 2048
.global kw_vectors
kw_vectors:
    KW_VECTOR kw_unexpected
    KW_VECTOR kw_unexpected
    KW_VECTOR kw_unexpected
    KW_VECTOR kw_unexpected
    KW_VECTOR kw_unexpected
    KW_VECTOR kw_unexpected
    KW_VECTOR kw_unexpected
    KW_VECTOR kw_unexpected
    KW_VECTOR kw_lower_sync
    KW_VECTOR kw_unexpected
    KW_VECTOR kw_unexpected
    KW_VECTOR kw_unexpected
    KW_VECTOR kw_unexpected
    KW_VECTOR kw_unexpected
    KW_VECTOR kw_unexpected
    KW_VECTOR kw_unexpected

.section .text, "ax", %progbits
kw_lower_sync:
    sub sp, sp, #272
    stp x0, x1, [sp, #0]
    stp x2, x3, [sp, #16]
    stp x4, x5, [sp, #32]
    stp x6, x7, [sp, #48]
    stp x8, x9, [sp, #64]
    stp x10, x11, [sp, #80]
    stp x12, x13, [sp, #96]
    stp x14, x15, [sp, #112]
    stp x16, x17, [sp, #128]
    stp x18, x19, [sp, #144]
    stp x20, x21, [sp, #160]
    stp x22, x23, [sp, #176]
    stp x24, x25, [sp, #192]
    stp x26, x27, [sp, #208]
    stp x28, x29, [sp, #224]
    str x30, [sp, #240]
    mrs x0, elr_el2
    str x0, [sp, #248]
    mrs x0, spsr_el2
    str x0, [sp, #256]
    mov x0, sp
    mrs x1, esr_el2
    mrs x2, far_el2
    mrs x3, hpfar_el2
    bl kw_handle_exit
    cbnz x0, kw_halt
    ldr x0, [sp, #248]
    msr elr_el2, x0
    ldr x0, [sp, #256]
    msr spsr_el2, x0
    ldp x0, x1, [sp, #0]
    ldp x2, x3, [sp, #16]
    ldp x4, x5, [sp, #32]
    ldp x6, x7, [sp, #48]
    ldp x8, x9, [sp, #64]
    ldp x10, x11, [sp, #80]
    ldp x12, x13, [sp, #96]
    ldp x14, x15, [sp, #112]
    ldp x16, x17, [sp, #128]
    ldp x18, x19, [sp, #144]
    ldp x20, x21, [sp, #160]
    ldp x22, x23, [sp, #176]
    ldp x24, x25, [sp, #192]
    ldp x26, x27, [sp, #208]
    ldp x28, x29, [sp, #224]
    ldr x30, [sp, #240]
    add sp, sp, #272
    eret

kw_unexpected:
    mrs x0, esr_el2
    mrs x1, far_el2
    mrs x2, elr_el2
    bl kw_unexpected_exit
    b kw_halt

.section .bss, "aw", %nobits
.balign 4096
kw_stage2_root: .skip 4096
kw_stage2_devices: .skip 4096
kw_stage2_ram: .skip 4096
kw_stage2_gic: .skip 4096
