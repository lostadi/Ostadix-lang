.arch armv8-a

.equ G2_FRAME_SIZE, 288

.macro G2_VECTOR target
    b \target
    .space 124
.endm

.section .vectors, "ax", %progbits
.balign 2048
.global g2_vectors
g2_vectors:
    G2_VECTOR g2_unexpected_vector // Current EL with SP0, synchronous
    G2_VECTOR g2_unexpected_vector // Current EL with SP0, IRQ
    G2_VECTOR g2_unexpected_vector // Current EL with SP0, FIQ
    G2_VECTOR g2_unexpected_vector // Current EL with SP0, SError
    G2_VECTOR g2_unexpected_vector // Current EL with SPx, synchronous
    G2_VECTOR g2_unexpected_vector // Current EL with SPx, IRQ
    G2_VECTOR g2_unexpected_vector // Current EL with SPx, FIQ
    G2_VECTOR g2_unexpected_vector // Current EL with SPx, SError
    G2_VECTOR g2_lower_a64_sync    // Lower EL using AArch64, synchronous
    G2_VECTOR g2_unexpected_vector // Lower EL using AArch64, IRQ
    G2_VECTOR g2_unexpected_vector // Lower EL using AArch64, FIQ
    G2_VECTOR g2_unexpected_vector // Lower EL using AArch64, SError
    G2_VECTOR g2_unexpected_vector // Lower EL using AArch32, synchronous
    G2_VECTOR g2_unexpected_vector // Lower EL using AArch32, IRQ
    G2_VECTOR g2_unexpected_vector // Lower EL using AArch32, FIQ
    G2_VECTOR g2_unexpected_vector // Lower EL using AArch32, SError

.section .text.g2_vectors, "ax", %progbits
.balign 16
g2_lower_a64_sync:
    sub sp, sp, #G2_FRAME_SIZE
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
    mrs x9, elr_el1
    str x9, [sp, #248]
    mrs x9, spsr_el1
    str x9, [sp, #256]
    mrs x9, sp_el0
    str x9, [sp, #264]
    mrs x9, esr_el1
    str x9, [sp, #272]
    mrs x9, far_el1
    str x9, [sp, #280]

    ldr x9, [sp, #272]
    lsr x10, x9, #26
    cmp x10, #0x15
    b.ne g2_dispatch_fault

    // AArch64 O-core syscall ABI: x8 number, x0..x5 arguments, x0 result.
    ldr x0, [sp, #64]
    ldr x1, [sp, #0]
    ldr x2, [sp, #8]
    ldr x3, [sp, #16]
    ldr x4, [sp, #24]
    ldr x5, [sp, #32]
    ldr x6, [sp, #40]
    bl g2_kernel_svc
    str x0, [sp, #0]
    bl g2_kernel_take_action
    b g2_apply_action

g2_dispatch_fault:
    ldr x0, [sp, #272]
    ldr x1, [sp, #280]
    bl g2_kernel_fault
    cbz x0, g2_saved_vector_failure
    bl g2_kernel_take_action

g2_apply_action:
    cbz x0, g2_restore_frame
    cmp x0, #1
    b.eq g2_start_principal_b
    cmp x0, #2
    b.eq g2_resume_principal_a
    cmp x0, #3
    b.eq g2_complete_lifecycle
    b g2_saved_vector_failure

g2_start_principal_b:
    // Retain A's post-SVC PC, SP_EL0, PSTATE, and every GPR exactly.  This
    // context is restored only after B replies and raises its contained BRK.
    adrp x9, g2_context_a
    add x9, x9, :lo12:g2_context_a
    mov x10, sp
    mov x11, #18
7:
    ldp x12, x13, [x10], #16
    stp x12, x13, [x9], #16
    subs x11, x11, #1
    b.ne 7b
    add sp, sp, #G2_FRAME_SIZE

    adrp x1, g2_user_b_stack_top
    add x1, x1, :lo12:g2_user_b_stack_top
    msr sp_el0, x1
    adrp x2, g2_user_b_entry
    add x2, x2, :lo12:g2_user_b_entry
    msr elr_el1, x2
    mov x3, #0x3c0
    msr spsr_el1, x3
    movz x0, #2
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

g2_resume_principal_a:
    adrp x9, g2_context_a
    add x9, x9, :lo12:g2_context_a
    mov x10, sp
    mov x11, #18
8:
    ldp x12, x13, [x9], #16
    stp x12, x13, [x10], #16
    subs x11, x11, #1
    b.ne 8b
    b g2_restore_frame

g2_complete_lifecycle:
    bl g2_kernel_finish
    b g2_arch_halt

g2_saved_vector_failure:
    ldr x0, [sp, #272]
    bl g2_kernel_vector_failure
    b g2_arch_halt

g2_restore_frame:
    ldr x9, [sp, #248]
    msr elr_el1, x9
    ldr x9, [sp, #256]
    msr spsr_el1, x9
    ldr x9, [sp, #264]
    msr sp_el0, x9
    ldr x30, [sp, #240]
    ldp x28, x29, [sp, #224]
    ldp x26, x27, [sp, #208]
    ldp x24, x25, [sp, #192]
    ldp x22, x23, [sp, #176]
    ldp x20, x21, [sp, #160]
    ldp x18, x19, [sp, #144]
    ldp x16, x17, [sp, #128]
    ldp x14, x15, [sp, #112]
    ldp x12, x13, [sp, #96]
    ldp x10, x11, [sp, #80]
    ldp x8, x9, [sp, #64]
    ldp x6, x7, [sp, #48]
    ldp x4, x5, [sp, #32]
    ldp x2, x3, [sp, #16]
    ldp x0, x1, [sp, #0]
    add sp, sp, #G2_FRAME_SIZE
    eret

g2_unexpected_vector:
    mrs x0, esr_el1
    bl g2_kernel_vector_failure
    b g2_arch_halt

.section .bss.g2_context, "aw", %nobits
.balign 16
.global g2_context_a
g2_context_a:
    .skip G2_FRAME_SIZE

.section .note.GNU-stack, "", %progbits
