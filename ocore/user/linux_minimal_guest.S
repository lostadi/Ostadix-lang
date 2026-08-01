/*
 * One pinned Linux x86-64 syscall corpus for O-core's minimal translated
 * personality. This is not a libc program and has no dynamic loader, rootfs,
 * auxiliary-vector, brk, mmap, read, clock, thread, or signal dependency.
 */

.section .text.entry,"ax",@progbits
.global _start
.type _start,@function
_start:
    /* write(1, stdout_bytes, STDOUT_LENGTH) */
    mov $1, %eax
    mov $1, %edi
    lea stdout_bytes(%rip), %rsi
    mov $STDOUT_LENGTH, %edx
    syscall
    cmp $STDOUT_LENGTH, %rax
    jne .Lfailed

    /* write(2, stderr_bytes, STDERR_LENGTH) */
    mov $1, %eax
    mov $2, %edi
    lea stderr_bytes(%rip), %rsi
    mov $STDERR_LENGTH, %edx
    syscall
    cmp $STDERR_LENGTH, %rax
    jne .Lfailed

    /* Deliberately unknown x86-64 syscall -> -ENOSYS (-38). */
    mov $0x7fffffff, %eax
    xor %edi, %edi
    xor %esi, %esi
    xor %edx, %edx
    xor %r10d, %r10d
    xor %r8d, %r8d
    xor %r9d, %r9d
    syscall
    cmp $-38, %rax
    jne .Lfailed

    /* exit_group(42); direct kernel teardown, no personality RPC. */
    mov $231, %eax
    mov $42, %edi
    syscall
    ud2

.Lfailed:
    mov $231, %eax
    mov $111, %edi
    syscall
    ud2
.size _start, .-_start

.section .rodata,"a",@progbits
stdout_bytes:
    .ascii "o-core linux stdout\n"
.equ STDOUT_LENGTH, .-stdout_bytes
stderr_bytes:
    .ascii "o-core linux stderr\n"
.equ STDERR_LENGTH, .-stderr_bytes

.section .note.GNU-stack,"",@progbits
