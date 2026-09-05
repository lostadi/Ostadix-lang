/* Standalone x86_64 native-network qualification image. No hosted runtime. */
.section .multiboot,"a",@progbits
.align 8
multiboot_header:
.long 0xe85250d6, 0, multiboot_end - multiboot_header
.long -(0xe85250d6 + (multiboot_end - multiboot_header))
.short 0, 0
.long 8
multiboot_end:
.section .note.Xen,"a",@note
.align 4
.long 4, 4, 18
.asciz "Xen"
.align 4
.long _start
.section .text.boot,"ax"
.code32
.global _start
_start:
    cli
    mov $stack_top, %esp
    mov $pdpt, %eax
    or $3, %eax
    mov %eax, pml4
    mov $pd, %eax
    or $3, %eax
    mov %eax, pdpt
    xor %ecx, %ecx
1:
    mov %ecx, %eax
    shl $21, %eax
    or $0x83, %eax
    mov %eax, pd(,%ecx,8)
    inc %ecx
    cmp $512, %ecx
    jne 1b
    mov %cr4, %eax
    or $0x620, %eax
    mov %eax, %cr4
    mov $pml4, %eax
    mov %eax, %cr3
    mov $0xc0000080, %ecx
    rdmsr
    or $0x100, %eax
    wrmsr
    mov %cr0, %eax
    and $0xfffffffb, %eax
    or $0x80000002, %eax
    mov %eax, %cr0
    lgdt gdt_pointer
    ljmp $8, $long_entry
.code64
long_entry:
    mov $16, %ax
    mov %ax, %ds
    mov %ax, %es
    mov %ax, %ss
    mov $stack_top, %rsp
    xor %rbp, %rbp
    call native_cluster_main
2:
    cli
    hlt
    jmp 2b
.global ocore_native_cycles
ocore_native_cycles:
    lfence
    rdtsc
    shl $32, %rdx
    or %rdx, %rax
    ret
.section .rodata,"a"
.align 8
gdt:
    .quad 0, 0x00af9a000000ffff, 0x00cf92000000ffff
gdt_pointer:
    .word 23
    .long gdt
.section .bss,"aw",@nobits
.align 4096
pml4: .skip 4096
pdpt: .skip 4096
pd: .skip 4096
.align 16
stack_bottom: .skip 131072
stack_top:
.section .note.GNU-stack,"",@progbits
