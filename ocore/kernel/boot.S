.intel_syntax noprefix

.section .note.Xen,"a",@note
.balign 4
  .long 4
  .long 4
  .long 18
  .asciz "Xen"
  .balign 4
  .long _start

.section .multiboot,"a",@progbits
.balign 8
multiboot_header:
  .long 0xe85250d6
  .long 0
  .long multiboot_header_end - multiboot_header
  .long -(0xe85250d6 + 0 + (multiboot_header_end - multiboot_header))
  .short 0
  .short 0
  .long 8
multiboot_header_end:

.section .bss,"aw",@nobits
.balign 4096
p4_table:
  .skip 4096
p3_table:
  .skip 4096
p2_table:
  .skip 4096
.balign 16
stack_bottom:
  .skip 65536
stack_top:

// SYSCALL does not switch stacks in long mode. This stack is also installed
// as TSS.rsp0 so hardware interrupts arriving from CPL3 have a trusted landing
// zone. The first proof is single-core and permits no nested kernel entries.
.balign 16
ring0_stack_bottom:
  .skip 65536
ring0_stack_top:

// 64-bit TSS: 104-byte architectural fixed portion, with no I/O bitmap.
.balign 16
tss64:
  .skip 104

// Per-CPU entry scratch for the single-process bootstrap. These values are
// written before touching the untrusted user stack and consumed after the
// personality dispatcher returns.
.balign 8
syscall_user_rsp:
  .skip 8
syscall_user_rip:
  .skip 8
syscall_user_rflags:
  .skip 8

.section .data,"aw",@progbits
.balign 8
gdt64:
  .quad 0
  .quad 0x00af9a000000ffff
  .quad 0x00cf92000000ffff
  .quad 0x00cff2000000ffff
  .quad 0x00affa000000ffff
gdt64_tss:
  .quad 0
  .quad 0
gdt64_end:
gdt64_pointer:
  .word gdt64_end - gdt64 - 1
  .quad gdt64

.section .text.boot,"ax",@progbits
.code32
.globl _start
.type _start, @function
.extern kernel_main
_start:
  cli
  mov esi, ebx

  lea eax, [p3_table]
  // Upper-level U/S bits permit traversal. Leaf PDEs still keep every kernel
  // page supervisor-only except the two explicit user regions below.
  or eax, 0x07
  mov dword ptr [p4_table], eax
  mov dword ptr [p4_table + 4], 0

  lea eax, [p2_table]
  or eax, 0x07
  mov dword ptr [p3_table], eax
  mov dword ptr [p3_table + 4], 0

  xor ecx, ecx
.map_identity:
  mov eax, ecx
  shl eax, 21
  mov edx, 0x83
  cmp ecx, 8
  je .map_user_text
  cmp ecx, 9
  je .map_user_stack
  jmp .store_mapping
.map_user_text:
  // 0x01000000..0x01200000: present, user, 2 MiB, read-only, executable.
  mov edx, 0x85
  jmp .store_mapping
.map_user_stack:
  // 0x01200000..0x01400000: present, user, 2 MiB, writable, non-executable.
  mov edx, 0x87
.store_mapping:
  or eax, edx
  mov dword ptr [p2_table + ecx * 8], eax
  xor eax, eax
  cmp ecx, 9
  jne .store_mapping_high
  mov eax, 0x80000000
.store_mapping_high:
  mov dword ptr [p2_table + ecx * 8 + 4], eax
  inc ecx
  cmp ecx, 512
  jne .map_identity

  lea eax, [p4_table]
  mov cr3, eax

  mov eax, cr4
  or eax, 1 << 5
  mov cr4, eax

  mov ecx, 0xc0000080
  rdmsr
  // LME plus NXE. SCE is enabled later only after LSTAR is installed.
  or eax, (1 << 8) | (1 << 11)
  wrmsr

  mov eax, cr0
  // Enable paging and supervisor write protection so the read-only user image
  // remains read-only even while the kernel is executing at CPL0.
  or eax, (1 << 31) | (1 << 16)
  mov cr0, eax

  lgdt [gdt64_pointer]
  .byte 0xea
  .long long_mode_start
  .word 0x08

.code64
long_mode_start:
  mov ax, 0x10
  mov ds, ax
  mov es, ax
  mov ss, ax
  xor ax, ax
  mov fs, ax
  mov gs, ax

  // Populate the 16-byte available-TSS descriptor now that 64-bit address
  // arithmetic is available, install a ring-0 interrupt stack, and load TR.
  lea rdx, [rip + tss64]
  mov word ptr [rip + gdt64_tss], 103
  mov word ptr [rip + gdt64_tss + 2], dx
  shr rdx, 16
  mov byte ptr [rip + gdt64_tss + 4], dl
  mov byte ptr [rip + gdt64_tss + 5], 0x89
  mov byte ptr [rip + gdt64_tss + 6], 0
  mov byte ptr [rip + gdt64_tss + 7], dh
  shr rdx, 16
  mov dword ptr [rip + gdt64_tss + 8], edx
  mov dword ptr [rip + gdt64_tss + 12], 0

  lea rax, [rip + ring0_stack_top]
  mov qword ptr [rip + tss64 + 4], rax
  mov word ptr [rip + tss64 + 102], 104
  mov ax, 0x28
  ltr ax

  lea rsp, [stack_top]
  and rsp, -16
  xor rbp, rbp
  mov edi, esi
  call kernel_main

.halt_forever:
  cli
  hlt
  jmp .halt_forever
.size _start, .-_start

// Return the trusted entry-stack top for the kernel-owned PCB metadata.
.globl ocore_kernel_stack_top
.type ocore_kernel_stack_top, @function
ocore_kernel_stack_top:
  lea rax, [rip + ring0_stack_top]
  ret
.size ocore_kernel_stack_top, .-ocore_kernel_stack_top

// Install the architectural SYSCALL boundary. STAR selects kernel CS 0x08;
// its SYSRET base is 0x10, which corresponds to user SS 0x1b and CS 0x23.
// The entry path currently returns with IRETQ so RIP/RSP can be validated.
.globl ocore_syscall_init
.type ocore_syscall_init, @function
ocore_syscall_init:
  mov ecx, 0xc0000081
  xor eax, eax
  mov edx, 0x00100008
  wrmsr

  mov ecx, 0xc0000082
  lea rax, [rip + ocore_syscall_entry]
  mov rdx, rax
  shr rdx, 32
  wrmsr

  mov ecx, 0xc0000084
  // Clear user-controlled state that is unsafe or meaningless while CPL0 is
  // running: TF, IF, DF, IOPL, NT, RF, VM, AC, VIF, VIP, and ID. In
  // particular, NT must be clear before the IRETQ return path.
  mov eax, 0x003f7700
  xor edx, edx
  wrmsr

  // Publish SCE only after every selector, entrypoint, and mask MSR is ready.
  mov ecx, 0xc0000080
  rdmsr
  or eax, 1
  wrmsr
  ret
.size ocore_syscall_init, .-ocore_syscall_init

// Enter the first process at CPL3 through an IRETQ frame.
//   RDI = user entry, RSI = user stack top, RDX = first user argument.
.globl ocore_enter_user
.type ocore_enter_user, @function
ocore_enter_user:
  cli
  mov r8, rdi
  mov r9, rsi
  // IRETQ enters the payload directly rather than through CALL. Reserve the
  // missing return-address slot so the compiler-generated function still
  // observes the SysV64 entry invariant RSP % 16 == 8.
  sub r9, 8
  mov rdi, rdx
  mov ax, 0x1b
  mov ds, ax
  mov es, ax
  push 0x1b
  push r9
  push 0x202
  push 0x23
  push r8
  iretq
.size ocore_enter_user, .-ocore_enter_user

// x86-64 syscall ABI on entry:
//   RAX number; RDI,RSI,RDX,R10,R8,R9 arguments 0..5.
// The O-core dispatcher is an ordinary SysV function:
//   RDI number; RSI,RDX,RCX,R8,R9 arguments 0..4; stack argument 5.
.type ocore_syscall_entry, @function
ocore_syscall_entry:
  mov qword ptr [rip + syscall_user_rsp], rsp
  mov qword ptr [rip + syscall_user_rip], rcx
  mov qword ptr [rip + syscall_user_rflags], r11
  lea rsp, [rip + ring0_stack_top]

  // Preserve every user argument register. The architectural SYSCALL
  // instruction defines RCX and R11 as clobbered and returns its result in
  // RAX; the remaining registers keep their call-side values.
  sub rsp, 64
  mov qword ptr [rsp], r9
  mov qword ptr [rsp + 16], rdi
  mov qword ptr [rsp + 24], rsi
  mov qword ptr [rsp + 32], rdx
  mov qword ptr [rsp + 40], r10
  mov qword ptr [rsp + 48], r8
  mov qword ptr [rsp + 56], r9
  mov r9, r8
  mov r8, r10
  mov rcx, rdx
  mov rdx, rsi
  mov rsi, rdi
  mov rdi, rax
  call kernel_syscall_dispatch
  mov qword ptr [rsp + 8], rax
  mov rdi, qword ptr [rsp + 16]
  mov rsi, qword ptr [rsp + 24]
  mov rdx, qword ptr [rsp + 32]
  mov r10, qword ptr [rsp + 40]
  mov r8, qword ptr [rsp + 48]
  mov r9, qword ptr [rsp + 56]
  mov rax, qword ptr [rsp + 8]
  add rsp, 64

  // Fail closed if the saved return frame is outside the dedicated process
  // mappings. A later scheduler will store this state in per-CPU/thread data.
  mov r10, qword ptr [rip + syscall_user_rip]
  cmp r10, 0x01000000
  jb .invalid_user_return
  cmp r10, 0x01200000
  jae .invalid_user_return
  mov r11, qword ptr [rip + syscall_user_rsp]
  cmp r11, 0x01200000
  jb .invalid_user_return
  cmp r11, 0x01400000
  jae .invalid_user_return

  push 0x1b
  push r11
  // The bootstrap restores only the architectural reserved bit and IF. This
  // clears IOPL, NT, VM, TF, and DF instead of trusting user-supplied flags.
  push 0x202
  push 0x23
  push r10
  iretq

.invalid_user_return:
  cli
.invalid_user_return_halt:
  hlt
  jmp .invalid_user_return_halt
.size ocore_syscall_entry, .-ocore_syscall_entry
