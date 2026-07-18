.intel_syntax noprefix

#ifndef OCORE_PROBE_MODE
#define OCORE_PROBE_MODE 0
#endif

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
p1_kernel_table:
  .skip 4096
p1_user_image_table:
  .skip 4096
p1_user_stack_table:
  .skip 4096

// Every privileged stack has an unmapped lower guard page. The bootstrap
// page-table builder recognizes these page-aligned symbols and leaves their
// PTEs absent.
.balign 4096
stack_guard:
  .skip 4096
stack_bottom:
  .skip 65536
stack_top:

// SYSCALL does not switch stacks in long mode. This stack is also installed
// as TSS.rsp0 so hardware interrupts arriving from CPL3 have a trusted landing
// zone. The first proof is single-core and permits no nested kernel entries.
.balign 4096
ring0_stack_guard:
  .skip 4096
ring0_stack_bottom:
  .skip 65536
ring0_stack_top:

// IST1 is reserved exclusively for double fault delivery.
.balign 4096
double_fault_stack_guard:
  .skip 4096
double_fault_stack_bottom:
  .skip 32768
double_fault_stack_top:

// 64-bit TSS: 104-byte architectural fixed portion, with no I/O bitmap.
.balign 16
tss64:
  .skip 104

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

// CPU-local architectural entry state. IA32_KERNEL_GS_BASE points here while
// CPL3 is running; SWAPGS makes it current before any SYSCALL state is stored.
// A later SMP milestone allocates one instance per CPU.
.balign 64
percpu_entry:
  .quad 0 // +0: saved user RSP
  .quad 0 // +8: saved user RIP
  .quad 0 // +16: saved user RFLAGS
  .quad ring0_stack_top // +24: trusted entry stack top
  .quad 0 // +32: user copy state (0 none, 1 from, 2 to)

.section .text.boot,"ax",@progbits
.code32
.globl _start
.type _start, @function
.extern kernel_main
.extern kernel_exception_dispatch
.extern kernel_invalid_user_return
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

  // PDE 0 uses 4 KiB leaves so kernel metadata, text, read-only data, mutable
  // state, page tables, and stacks can have distinct W^X permissions.
  lea eax, [p1_kernel_table]
  or eax, 0x03
  mov dword ptr [p2_table], eax
  mov dword ptr [p2_table + 4], 0

  // 4..16 MiB: supervisor RW/NX huge pages. This contains the bootstrap page
  // allocator and no executable kernel code.
  mov ecx, 2
.map_kernel_data_huge:
  mov eax, ecx
  shl eax, 21
  or eax, 0x83
  mov dword ptr [p2_table + ecx * 8], eax
  mov dword ptr [p2_table + ecx * 8 + 4], 0x80000000
  inc ecx
  cmp ecx, 8
  jne .map_kernel_data_huge

  // 16..18 MiB: user image through 4 KiB PTEs. Its first 64 KiB are RX and
  // its zero-fill tail is R/NX. Probe mode 9 deliberately omits the final leaf
  // to exercise the user-copy fault fixup.
  lea eax, [p1_user_image_table]
  or eax, 0x05
  mov dword ptr [p2_table + 8 * 8], eax
  mov dword ptr [p2_table + 8 * 8 + 4], 0

  // 18..20 MiB: user RW/NX stack through 4 KiB PTEs.
  lea eax, [p1_user_stack_table]
  or eax, 0x07
  mov dword ptr [p2_table + 9 * 8], eax
  mov dword ptr [p2_table + 9 * 8 + 4], 0

  xor ecx, ecx
.map_kernel_page:
  mov eax, ecx
  shl eax, 12

  // Guard pages stay non-present.
  lea edi, [stack_guard]
  cmp eax, edi
  je .next_kernel_page
  lea edi, [ring0_stack_guard]
  cmp eax, edi
  je .next_kernel_page
  lea edi, [double_fault_stack_guard]
  cmp eax, edi
  je .next_kernel_page

  // Default supervisor memory is writable and non-executable.
  mov edx, eax
  or edx, 0x03
  mov ebx, 0x80000000

  // Kernel text is supervisor read/execute.
  lea edi, [__kernel_text_start]
  cmp eax, edi
  jb .check_kernel_readonly
  lea edi, [__kernel_text_end]
  cmp eax, edi
  jae .check_kernel_readonly
  mov edx, eax
  or edx, 0x01
  xor ebx, ebx
  jmp .store_kernel_page

.check_kernel_readonly:
  // Boot metadata and O-core read-only data are supervisor read/NX.
  lea edi, [__kernel_meta_start]
  cmp eax, edi
  jb .check_kernel_rodata
  lea edi, [__kernel_meta_end]
  cmp eax, edi
  jb .kernel_readonly
.check_kernel_rodata:
  lea edi, [__kernel_rodata_start]
  cmp eax, edi
  jb .store_kernel_page
  lea edi, [__kernel_rodata_end]
  cmp eax, edi
  jae .store_kernel_page
.kernel_readonly:
  mov edx, eax
  or edx, 0x01
.store_kernel_page:
  mov dword ptr [p1_kernel_table + ecx * 8], edx
  mov dword ptr [p1_kernel_table + ecx * 8 + 4], ebx
.next_kernel_page:
  inc ecx
  cmp ecx, 512
  jne .map_kernel_page

  xor ecx, ecx
.map_user_image_page:
#if OCORE_PROBE_MODE == 9
  cmp ecx, 511
  je .next_user_image_page
#endif
  mov eax, ecx
  shl eax, 12
  add eax, 0x01000000
  or eax, 0x05
  mov dword ptr [p1_user_image_table + ecx * 8], eax
  xor edx, edx
  cmp ecx, 16
  jb .store_user_image_high
  mov edx, 0x80000000
.store_user_image_high:
  mov dword ptr [p1_user_image_table + ecx * 8 + 4], edx
.next_user_image_page:
  inc ecx
  cmp ecx, 512
  jne .map_user_image_page

  // PTE 0 is the non-present guard. All remaining user-stack pages are RW/NX.
  mov ecx, 1
.map_user_stack_page:
  mov eax, ecx
  shl eax, 12
  add eax, 0x01200000
  or eax, 0x07
  mov dword ptr [p1_user_stack_table + ecx * 8], eax
  mov dword ptr [p1_user_stack_table + ecx * 8 + 4], 0x80000000
  inc ecx
  cmp ecx, 512
  jne .map_user_stack_page

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
  lea rax, [rip + double_fault_stack_top]
  mov qword ptr [rip + tss64 + 36], rax
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

.globl ocore_current_cr3
.type ocore_current_cr3, @function
ocore_current_cr3:
  mov rax, cr3
  ret
.size ocore_current_cr3, .-ocore_current_cr3

.globl ocore_read_cr2
.type ocore_read_cr2, @function
ocore_read_cr2:
  mov rax, cr2
  ret
.size ocore_read_cr2, .-ocore_read_cr2

.globl ocore_kernel_probe_address
.type ocore_kernel_probe_address, @function
ocore_kernel_probe_address:
  lea rax, [rip + _start]
  ret
.size ocore_kernel_probe_address, .-ocore_kernel_probe_address

.globl ocore_boot_protection_check
.type ocore_boot_protection_check, @function
ocore_boot_protection_check:
  mov rax, cr0
  test rax, 1 << 16
  jz .protection_check_failed
  mov ecx, 0xc0000080
  rdmsr
  test eax, 1 << 11
  jz .protection_check_failed
  lea rcx, [rip + p1_kernel_table]

  // Kernel text: supervisor, read-only, executable.
  lea rax, [rip + _start]
  shr rax, 12
  mov rdx, qword ptr [rcx + rax * 8]
  test rdx, 1
  jz .protection_check_failed
  test rdx, 2 | 4
  jnz .protection_check_failed
  bt rdx, 63
  jc .protection_check_failed

  // Kernel read-only data: supervisor, read-only, NX.
  lea rax, [rip + OCORE_KERNEL_ONLY_MARKER]
  shr rax, 12
  mov rdx, qword ptr [rcx + rax * 8]
  test rdx, 1
  jz .protection_check_failed
  test rdx, 2 | 4
  jnz .protection_check_failed
  bt rdx, 63
  jnc .protection_check_failed

  // Mutable kernel state: supervisor, writable, NX.
  lea rax, [rip + percpu_entry]
  shr rax, 12
  mov rdx, qword ptr [rcx + rax * 8]
  mov rax, rdx
  and eax, 0x03
  cmp eax, 0x03
  jne .protection_check_failed
  test rdx, 4
  jnz .protection_check_failed
  bt rdx, 63
  jnc .protection_check_failed

  // All privileged stack guards are absent.
  lea rax, [rip + stack_guard]
  shr rax, 12
  cmp qword ptr [rcx + rax * 8], 0
  jne .protection_check_failed
  lea rax, [rip + ring0_stack_guard]
  shr rax, 12
  cmp qword ptr [rcx + rax * 8], 0
  jne .protection_check_failed
  lea rax, [rip + double_fault_stack_guard]
  shr rax, 12
  cmp qword ptr [rcx + rax * 8], 0
  jne .protection_check_failed

  // User image leaves are RX, stack leaves are RW/NX, and stack PTE 0 is the
  // absent guard.
  mov rdx, qword ptr [rip + p2_table + 8 * 8]
  and edx, 0x87
  cmp edx, 0x05
  jne .protection_check_failed
  lea rcx, [rip + p1_user_image_table]
  mov rdx, qword ptr [rcx]
  mov rax, rdx
  and eax, 0x07
  cmp eax, 0x05
  jne .protection_check_failed
  bt rdx, 63
  jc .protection_check_failed
  mov rdx, qword ptr [rcx + 16 * 8]
  mov rax, rdx
  and eax, 0x07
  cmp eax, 0x05
  jne .protection_check_failed
  bt rdx, 63
  jnc .protection_check_failed
  lea rcx, [rip + p1_user_stack_table]
  cmp qword ptr [rcx], 0
  jne .protection_check_failed
  mov rdx, qword ptr [rcx + 8]
  mov rax, rdx
  and eax, 0x07
  cmp eax, 0x07
  jne .protection_check_failed
  bt rdx, 63
  jnc .protection_check_failed

  // The unused 2..4 MiB gap is absent, bootstrap mappings stop at 20 MiB, and
  // no residual 1 GiB identity RWX map remains.
  cmp qword ptr [rip + p2_table + 1 * 8], 0
  jne .protection_check_failed
  cmp qword ptr [rip + p2_table + 10 * 8], 0
  jne .protection_check_failed
  mov eax, 1
  ret
.protection_check_failed:
  xor eax, eax
  ret
.size ocore_boot_protection_check, .-ocore_boot_protection_check

.globl ocore_boot_probe_mode
.type ocore_boot_probe_mode, @function
ocore_boot_probe_mode:
  mov eax, OCORE_PROBE_MODE
  ret
.size ocore_boot_probe_mode, .-ocore_boot_probe_mode

// Install the architectural SYSCALL boundary. STAR selects kernel CS 0x08;
// its SYSRET base is 0x10, which corresponds to user SS 0x1b and CS 0x23.
// The entry path currently returns with IRETQ so RIP/RSP can be validated.
.globl ocore_syscall_init
.type ocore_syscall_init, @function
ocore_syscall_init:
  // The boot CPU starts with its own entry state active. CPL3 receives a null
  // GS base, and SWAPGS selects this record before SYSCALL stores user state.
  mov ecx, 0xc0000101
  lea rax, [rip + percpu_entry]
  mov rdx, rax
  shr rdx, 32
  wrmsr
  mov ecx, 0xc0000102
  xor eax, eax
  xor edx, edx
  wrmsr

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
  swapgs
  iretq
.size ocore_enter_user, .-ocore_enter_user

// x86-64 syscall ABI on entry:
//   RAX number; RDI,RSI,RDX,R10,R8,R9 arguments 0..5.
// The O-core dispatcher is an ordinary SysV function:
//   RDI number; RSI,RDX,RCX,R8,R9 arguments 0..4; stack argument 5.
.type ocore_syscall_entry, @function
ocore_syscall_entry:
  swapgs
  mov qword ptr gs:[0], rsp
  mov qword ptr gs:[8], rcx
  mov qword ptr gs:[16], r11
  mov rsp, qword ptr gs:[24]

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
  mov rcx, qword ptr gs:[8]
  cmp rcx, 0x01000000
  jb .invalid_user_return
  cmp rcx, 0x01010000
  jae .invalid_user_return
  mov r11, qword ptr gs:[0]
  cmp r11, 0x01201000
  jb .invalid_user_return
  cmp r11, 0x01400000
  jae .invalid_user_return

  push 0x1b
  push r11
  // The bootstrap restores only the architectural reserved bit and IF. This
  // clears IOPL, NT, VM, TF, and DF instead of trusting user-supplied flags.
  push 0x202
  push 0x23
  push rcx
  swapgs
  iretq

.invalid_user_return:
  mov rdi, rcx
  mov rsi, r11
  call kernel_invalid_user_return
  cmp rax, 1
  jne .fatal_exception
  jmp ocore_fault_idle
.size ocore_syscall_entry, .-ocore_syscall_entry

// Exception entry -----------------------------------------------------------
// Each stub normalizes the architectural frame to vector,error before the
// fixed RIP,CS,RFLAGS,RSP,SS tail. Error-code vectors already receive the
// hardware error and therefore push only their vector number.
.macro OCORE_ISR_NO_ERROR vector
.globl ocore_exception_\vector
.type ocore_exception_\vector, @function
ocore_exception_\vector:
  push 0
  push \vector
  jmp ocore_exception_common
.size ocore_exception_\vector, .-ocore_exception_\vector
.endm

.macro OCORE_ISR_ERROR vector
.globl ocore_exception_\vector
.type ocore_exception_\vector, @function
ocore_exception_\vector:
  push \vector
  jmp ocore_exception_common
.size ocore_exception_\vector, .-ocore_exception_\vector
.endm

OCORE_ISR_NO_ERROR 0
OCORE_ISR_NO_ERROR 1
OCORE_ISR_NO_ERROR 2
OCORE_ISR_NO_ERROR 3
OCORE_ISR_NO_ERROR 4
OCORE_ISR_NO_ERROR 5
OCORE_ISR_NO_ERROR 6
OCORE_ISR_NO_ERROR 7
OCORE_ISR_ERROR 8
OCORE_ISR_NO_ERROR 9
OCORE_ISR_ERROR 10
OCORE_ISR_ERROR 11
OCORE_ISR_ERROR 12
OCORE_ISR_ERROR 13
OCORE_ISR_ERROR 14
OCORE_ISR_NO_ERROR 15
OCORE_ISR_NO_ERROR 16
OCORE_ISR_ERROR 17
OCORE_ISR_NO_ERROR 18
OCORE_ISR_NO_ERROR 19
OCORE_ISR_NO_ERROR 20
OCORE_ISR_ERROR 21
OCORE_ISR_NO_ERROR 22
OCORE_ISR_NO_ERROR 23
OCORE_ISR_NO_ERROR 24
OCORE_ISR_NO_ERROR 25
OCORE_ISR_NO_ERROR 26
OCORE_ISR_NO_ERROR 27
OCORE_ISR_NO_ERROR 28
OCORE_ISR_ERROR 29
OCORE_ISR_ERROR 30
OCORE_ISR_NO_ERROR 31

.type ocore_exception_common, @function
ocore_exception_common:
  push rax
  push rbx
  push rcx
  push rdx
  push rbp
  push rsi
  push rdi
  push r8
  push r9
  push r10
  push r11
  push r12
  push r13
  push r14
  push r15
  cld

  // CPL3 entry has the user GS base active. Switch to the boot CPU's trusted
  // entry record before entering any O-core code.
  mov rax, qword ptr [rsp + 144]
  test al, 3
  jnz .user_exception

  // The only recoverable CPL0 page faults are exact fault sites in an active
  // CPU-local user-copy operation, with CR2 inside the bootstrap user window.
  cmp qword ptr [rsp + 120], 14
  jne .dispatch_kernel_exception
  mov rax, qword ptr gs:[32]
  test rax, rax
  jz .dispatch_kernel_exception
  mov rdx, cr2
  cmp rdx, 0x01000000
  jb .dispatch_kernel_exception
  cmp rdx, 0x01400000
  jae .dispatch_kernel_exception
  mov rcx, qword ptr [rsp + 128]
  cmp rax, 1
  je .recover_copy_from
  cmp rax, 2
  je .recover_copy_to
  jmp .dispatch_kernel_exception

.recover_copy_from:
  test rcx, 2
  jnz .dispatch_kernel_exception
  mov rax, qword ptr [rsp + 136]
  lea rdx, [rip + ocore_copy_from_fault_start]
  cmp rax, rdx
  jb .dispatch_kernel_exception
  lea rdx, [rip + ocore_copy_from_fault_end]
  cmp rax, rdx
  jae .dispatch_kernel_exception
  lea rax, [rip + ocore_copy_from_fixup]
  mov qword ptr [rsp + 136], rax
  jmp .restore_exception

.recover_copy_to:
  test rcx, 2
  jz .dispatch_kernel_exception
  mov rax, qword ptr [rsp + 136]
  lea rdx, [rip + ocore_copy_to_fault_start]
  cmp rax, rdx
  jb .dispatch_kernel_exception
  lea rdx, [rip + ocore_copy_to_fault_end]
  cmp rax, rdx
  jae .dispatch_kernel_exception
  lea rax, [rip + ocore_copy_to_fixup]
  mov qword ptr [rsp + 136], rax
  jmp .restore_exception

.user_exception:
  swapgs
  mov rdi, rsp
  call kernel_exception_dispatch
  cmp rax, 1
  jne .fatal_exception
  jmp ocore_fault_idle

.dispatch_kernel_exception:
  mov rdi, rsp
  call kernel_exception_dispatch

.fatal_exception:
  cli
.fatal_exception_halt:
  hlt
  jmp .fatal_exception_halt

.restore_exception:
  pop r15
  pop r14
  pop r13
  pop r12
  pop r11
  pop r10
  pop r9
  pop r8
  pop rdi
  pop rsi
  pop rbp
  pop rdx
  pop rcx
  pop rbx
  pop rax
  add rsp, 16
  iretq
.size ocore_exception_common, .-ocore_exception_common

.globl ocore_fault_idle
.type ocore_fault_idle, @function
ocore_fault_idle:
  lea rsp, [rip + ring0_stack_top]
  and rsp, -16
  xor rbp, rbp
  sti
.fault_idle_loop:
  hlt
  jmp .fault_idle_loop
.size ocore_fault_idle, .-ocore_fault_idle

.globl ocore_exception_stub_address
.type ocore_exception_stub_address, @function
ocore_exception_stub_address:
  cmp rdi, 32
  jae .invalid_exception_vector
  lea rax, [rip + ocore_exception_stub_table]
  mov rax, qword ptr [rax + rdi * 8]
  ret
.invalid_exception_vector:
  xor eax, eax
  ret
.size ocore_exception_stub_address, .-ocore_exception_stub_address

// Recoverable user copy -----------------------------------------------------
// Return zero on success and one after an exact page-fault fixup. These are
// single-CPU bootstrap primitives; copy state becomes per-CPU by construction.
.globl ocore_copy_from_user
.type ocore_copy_from_user, @function
ocore_copy_from_user:
  mov qword ptr gs:[32], 1
  test rdx, rdx
  jz .copy_from_success
.copy_from_loop:
ocore_copy_from_fault_start:
  mov al, byte ptr [rsi]
ocore_copy_from_fault_end:
  mov byte ptr [rdi], al
  inc rsi
  inc rdi
  dec rdx
  jnz .copy_from_loop
.copy_from_success:
  mov qword ptr gs:[32], 0
  xor eax, eax
  ret
ocore_copy_from_fixup:
  mov qword ptr gs:[32], 0
  mov eax, 1
  ret
.size ocore_copy_from_user, .-ocore_copy_from_user

.globl ocore_copy_to_user
.type ocore_copy_to_user, @function
ocore_copy_to_user:
  mov qword ptr gs:[32], 2
  test rdx, rdx
  jz .copy_to_success
.copy_to_loop:
  mov al, byte ptr [rsi]
ocore_copy_to_fault_start:
  mov byte ptr [rdi], al
ocore_copy_to_fault_end:
  inc rsi
  inc rdi
  dec rdx
  jnz .copy_to_loop
.copy_to_success:
  mov qword ptr gs:[32], 0
  xor eax, eax
  ret
ocore_copy_to_fixup:
  mov qword ptr gs:[32], 0
  mov eax, 1
  ret
.size ocore_copy_to_user, .-ocore_copy_to_user

.section .rodata,"a",@progbits
.balign 8
ocore_exception_stub_table:
  .quad ocore_exception_0, ocore_exception_1
  .quad ocore_exception_2, ocore_exception_3
  .quad ocore_exception_4, ocore_exception_5
  .quad ocore_exception_6, ocore_exception_7
  .quad ocore_exception_8, ocore_exception_9
  .quad ocore_exception_10, ocore_exception_11
  .quad ocore_exception_12, ocore_exception_13
  .quad ocore_exception_14, ocore_exception_15
  .quad ocore_exception_16, ocore_exception_17
  .quad ocore_exception_18, ocore_exception_19
  .quad ocore_exception_20, ocore_exception_21
  .quad ocore_exception_22, ocore_exception_23
  .quad ocore_exception_24, ocore_exception_25
  .quad ocore_exception_26, ocore_exception_27
  .quad ocore_exception_28, ocore_exception_29
  .quad ocore_exception_30, ocore_exception_31

// Linked CPL3 fault and register probes -------------------------------------
.section .text.user,"ax",@progbits
.globl ocore_user_register_probe
.type ocore_user_register_probe, @function
ocore_user_register_probe:
  push rbx
  push rbp
  push r12
  push r13
  push r14
  push r15
  mov rbx, 0x11223344
  mov rbp, 0x22334455
  mov r10, 0x33445566
  mov r12, 0x44556677
  mov r13, 0x55667788
  mov r14, 0x66778899
  mov r15, 0x778899aa
  mov eax, 5
  syscall
  cmp rbx, 0x11223344
  jne .register_probe_failed
  cmp rbp, 0x22334455
  jne .register_probe_failed
  cmp r10, 0x33445566
  jne .register_probe_failed
  cmp r12, 0x44556677
  jne .register_probe_failed
  cmp r13, 0x55667788
  jne .register_probe_failed
  cmp r14, 0x66778899
  jne .register_probe_failed
  cmp r15, 0x778899aa
  jne .register_probe_failed
  mov eax, 1
  jmp .register_probe_restore
.register_probe_failed:
  xor eax, eax
.register_probe_restore:
  pop r15
  pop r14
  pop r13
  pop r12
  pop rbp
  pop rbx
  ret
.size ocore_user_register_probe, .-ocore_user_register_probe

.globl ocore_user_probe_divide
.type ocore_user_probe_divide, @function
ocore_user_probe_divide:
  xor edx, edx
  mov eax, 1
  xor ecx, ecx
  div rcx
  ud2
.size ocore_user_probe_divide, .-ocore_user_probe_divide

.globl ocore_user_probe_invalid_opcode
.type ocore_user_probe_invalid_opcode, @function
ocore_user_probe_invalid_opcode:
  ud2
.size ocore_user_probe_invalid_opcode, .-ocore_user_probe_invalid_opcode

.globl ocore_user_probe_nonpresent
.type ocore_user_probe_nonpresent, @function
ocore_user_probe_nonpresent:
  mov rax, 0x40000000
  mov rax, qword ptr [rax]
  ud2
.size ocore_user_probe_nonpresent, .-ocore_user_probe_nonpresent

.globl ocore_user_probe_supervisor
.type ocore_user_probe_supervisor, @function
ocore_user_probe_supervisor:
  lea rax, [rip + _start]
  mov rax, qword ptr [rax]
  ud2
.size ocore_user_probe_supervisor, .-ocore_user_probe_supervisor

.globl ocore_user_probe_guard_stack
.type ocore_user_probe_guard_stack, @function
ocore_user_probe_guard_stack:
  mov rsp, 0x01200008
  push rax
  ud2
.size ocore_user_probe_guard_stack, .-ocore_user_probe_guard_stack

.globl ocore_user_probe_nx
.type ocore_user_probe_nx, @function
ocore_user_probe_nx:
  mov rax, 0x01201000
  jmp rax
.size ocore_user_probe_nx, .-ocore_user_probe_nx

.globl ocore_user_probe_noncanonical
.type ocore_user_probe_noncanonical, @function
ocore_user_probe_noncanonical:
  movabs rax, 0x0000800000000000
  jmp rax
.size ocore_user_probe_noncanonical, .-ocore_user_probe_noncanonical

.globl ocore_user_probe_bad_return
.type ocore_user_probe_bad_return, @function
ocore_user_probe_bad_return:
  mov eax, 5
  lea rdx, [rip + ocore_user_return_edge]
  jmp rdx
.size ocore_user_probe_bad_return, .-ocore_user_probe_bad_return

.section .text.user.return_edge,"ax",@progbits
.globl ocore_user_return_edge
.type ocore_user_return_edge, @function
ocore_user_return_edge:
  syscall
.size ocore_user_return_edge, .-ocore_user_return_edge
