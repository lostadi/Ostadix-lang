.intel_syntax noprefix

#ifndef OCORE_PROBE_MODE
#define OCORE_PROBE_MODE 0
#endif

// All historical probes retain the 512 KiB kernel-growth reserve. Modes 26
// and 31 use larger verified OVFS backing stores with an explicit 448 KiB
// reserve while remaining below the same 4 MiB bootstrap mapping ceiling.
.globl ocore_kernel_end_limit
#if OCORE_PROBE_MODE == 26 || OCORE_PROBE_MODE == 31
.set ocore_kernel_end_limit, 0x00390000
#else
.set ocore_kernel_end_limit, 0x00380000
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

// The native loader consumes an immutable OVFS image as opaque bytes. The
// independently linked user ELFs inside it are never linked as kernel code;
// mode 15 validates and materializes their PT_LOAD segments into private
// process frames. Earlier modes retain a zero-length image boundary.
.section .rodata,"a",@progbits
.balign 16
.globl ocore_m4_image_bytes_start
ocore_m4_image_bytes_start:
#if OCORE_PROBE_MODE == 15
#ifndef OCORE_M4_IMAGE_PATH
#error "OCORE_M4_IMAGE_PATH is required for native loader modes"
#endif
  .incbin OCORE_M4_IMAGE_PATH
#endif
.globl ocore_m4_image_bytes_end
ocore_m4_image_bytes_end:

// Mode 16 carries the separately content-addressed native control-plane
// package. It remains opaque read-only data until OVFS and ELF validation.
.balign 16
.globl ocore_m5_image_bytes_start
ocore_m5_image_bytes_start:
#if OCORE_PROBE_MODE == 16
#ifndef OCORE_M5_IMAGE_PATH
#error "OCORE_M5_IMAGE_PATH is required for native live-system mode"
#endif
  .incbin OCORE_M5_IMAGE_PATH
#endif
.globl ocore_m5_image_bytes_end
ocore_m5_image_bytes_end:

// Mode 18 carries a separate immutable package for the scalar M6A router,
// supervisor, test-personality daemon, client, and unrelated observer proof.
// Keeping it separate preserves the canonical M5 artifact and evidence.
.balign 16
.globl ocore_m6_image_bytes_start
ocore_m6_image_bytes_start:
#if OCORE_PROBE_MODE == 18
#ifndef OCORE_M6_IMAGE_PATH
#error "OCORE_M6_IMAGE_PATH is required for personality supervision mode"
#endif
  .incbin OCORE_M6_IMAGE_PATH
#endif
.globl ocore_m6_image_bytes_end
ocore_m6_image_bytes_end:

// Mode 24 carries the independently rebuilt CPL3 bounded-copy personality
// corpus.  It deliberately reuses no scalar M6A payload code.
.balign 16
.globl ocore_m6b_live_image_bytes_start
ocore_m6b_live_image_bytes_start:
#if OCORE_PROBE_MODE == 24
#ifndef OCORE_M6B_LIVE_IMAGE_PATH
#error "OCORE_M6B_LIVE_IMAGE_PATH is required for live bounded personality mode"
#endif
  .incbin OCORE_M6B_LIVE_IMAGE_PATH
#endif
.globl ocore_m6b_live_image_bytes_end
ocore_m6b_live_image_bytes_end:

// Mode 25 carries the exact pinned static Linux x86-64 ELF plus three
// independently linked O-core service principals.  These bytes remain data
// until OVFS and ELF validation; the guest is never linked into the kernel.
.balign 16
.globl ocore_m6_linux_image_bytes_start
ocore_m6_linux_image_bytes_start:
#if OCORE_PROBE_MODE == 25
#ifndef OCORE_M6_LINUX_IMAGE_PATH
#error "OCORE_M6_LINUX_IMAGE_PATH is required for live Linux personality mode"
#endif
  .incbin OCORE_M6_LINUX_IMAGE_PATH
#endif
.globl ocore_m6_linux_image_bytes_end
ocore_m6_linux_image_bytes_end:

// Mode 26 keeps Linux in the translated CPL3 provider role and adds a
// separately linked native Plan-9-style namespace client.  The package
// carries real 9P2000 wire endpoints; it is still not a foreign-kernel image.
.balign 16
.globl ocore_m7_linux_plan9_image_bytes_start
ocore_m7_linux_plan9_image_bytes_start:
#if OCORE_PROBE_MODE == 26
#ifndef OCORE_M7_LINUX_PLAN9_IMAGE_PATH
#error "OCORE_M7_LINUX_PLAN9_IMAGE_PATH is required for Linux-to-9P mode"
#endif
  .incbin OCORE_M7_LINUX_PLAN9_IMAGE_PATH
#endif
.globl ocore_m7_linux_plan9_image_bytes_end
ocore_m7_linux_plan9_image_bytes_end:

// Mode 31 embeds one deterministic provider ELF (instantiated twice), one
// client/witness ELF (instantiated twice), and the exact 20-byte immutable
// object. Provider B is admitted with A but its service loop is staged until
// A's terminal denial and stale-capability cut have completed.
.balign 16
.globl ocore_m7b_logical_read_image_bytes_start
ocore_m7b_logical_read_image_bytes_start:
#if OCORE_PROBE_MODE == 31
#ifndef OCORE_M7B_LOGICAL_READ_IMAGE_PATH
#error "OCORE_M7B_LOGICAL_READ_IMAGE_PATH is required for M7B-1 LogicalRead mode"
#endif
  .incbin OCORE_M7B_LOGICAL_READ_IMAGE_PATH
#endif
.globl ocore_m7b_logical_read_image_bytes_end
ocore_m7b_logical_read_image_bytes_end:

// Modes 20 through 23 embed the deterministic host-verified KernelWorld normal
// form. Native admission pins the complete record SHA before parsing it. Mode
// 22 adds only a TCG-compatible live lifecycle/service mechanism proof; no
// foreign kernel image is embedded by these gates.
.balign 16
.globl ocore_kernel_world_record_bytes_start
ocore_kernel_world_record_bytes_start:
#if OCORE_PROBE_MODE == 20 || OCORE_PROBE_MODE == 21 || OCORE_PROBE_MODE == 22 || OCORE_PROBE_MODE == 23
#ifndef OCORE_KERNEL_WORLD_RECORD_PATH
#error "OCORE_KERNEL_WORLD_RECORD_PATH is required for KernelWorld admission mode"
#endif
  .incbin OCORE_KERNEL_WORLD_RECORD_PATH
#endif
.globl ocore_kernel_world_record_bytes_end
ocore_kernel_world_record_bytes_end:

#if OCORE_PROBE_MODE == 34
// APs begin at physical 0x8000 in 16-bit real mode.  This template contains
// no bootloader pointer and no stack-dependent instruction.  The BSP copies
// it only after the Multiboot2 memory map admits the complete 4 KiB page.
.balign 16
smp_trampoline_template_start:
.code16
  cli
  cld
  xor ax, ax
  mov ds, ax
  mov es, ax
  mov ss, ax
  in al, 0x92
  or al, 0x02
  and al, 0xfe
  out 0x92, al
  lgdt [SMP_TRAMPOLINE_GDT_POINTER_ADDRESS]
  mov eax, cr0
  or eax, 0x01
  mov cr0, eax
  .byte 0xea
  .word SMP_TRAMPOLINE_PROTECTED_ADDRESS
  .word 0x08

.code32
smp_trampoline_protected:
  mov ax, 0x10
  mov ds, ax
  mov es, ax
  mov ss, ax
  mov eax, dword ptr [SMP_TRAMPOLINE_CR3_ADDRESS]
  mov cr3, eax
  mov eax, cr4
  or eax, 1 << 5
  mov cr4, eax
  mov ecx, 0xc0000080
  rdmsr
  or eax, (1 << 8) | (1 << 11)
  wrmsr
  mov eax, cr0
  or eax, (1 << 31) | (1 << 16)
  mov cr0, eax
  .byte 0xea
  .long smp_ap_entry64
  .word 0x18

.balign 8
smp_trampoline_gdt:
  .quad 0
  .quad 0x00cf9a000000ffff
  .quad 0x00cf92000000ffff
  .quad 0x00af9a000000ffff
smp_trampoline_gdt_end:
smp_trampoline_gdt_pointer:
  .word smp_trampoline_gdt_end - smp_trampoline_gdt - 1
  .long 0x8000 + smp_trampoline_gdt - smp_trampoline_template_start
smp_trampoline_cr3:
  .long 0
smp_trampoline_template_end:
.set SMP_TRAMPOLINE_GDT_POINTER_ADDRESS, 0x8000 + smp_trampoline_gdt_pointer - smp_trampoline_template_start
.set SMP_TRAMPOLINE_PROTECTED_ADDRESS, 0x8000 + smp_trampoline_protected - smp_trampoline_template_start
.set SMP_TRAMPOLINE_CR3_ADDRESS, 0x8000 + smp_trampoline_cr3 - smp_trampoline_template_start
.set SMP_TRAMPOLINE_CR3_OFFSET, smp_trampoline_cr3 - smp_trampoline_template_start
.set SMP_TRAMPOLINE_TEMPLATE_BYTES, smp_trampoline_template_end - smp_trampoline_template_start
.code64
#endif

.section .bss,"aw",@nobits
.balign 8
// Capture the architectural Multiboot2 handoff before page-table setup
// reuses EAX and EBX.  The O-core normalizer copies every admitted fact out of
// bootloader memory before the temporary inspection mapping is removed.
bootloader_magic_saved:
  .long 0
bootloader_info_saved:
  .long 0

#if OCORE_PROBE_MODE == 34
// One cache-line-separated, BSP-owned startup record.  APs may publish only
// into the fixed slot selected by their firmware-admitted APIC identity.
.balign 64
smp_expected_ids:
  .skip 24
smp_stack_tops:
  .skip 24
smp_observed_ids:
  .skip 32
smp_observed_stacks:
  .skip 32
smp_arrived:
  .long 0
smp_release:
  .long 0
smp_progress:
  .long 0
smp_failure:
  .long 0
smp_claimed:
  .long 0
  .long 0
  .long 0
  .long 0
smp_control_end:
.set SMP_CONTROL_QWORDS, (smp_control_end - smp_expected_ids) / 8

.balign 4096
smp_ap_stack1_bottom:
  .skip 16384
smp_ap_stack1_top:
.balign 4096
smp_ap_stack2_bottom:
  .skip 16384
smp_ap_stack2_top:
.balign 4096
smp_ap_stack3_bottom:
  .skip 16384
smp_ap_stack3_top:
#endif

.balign 4096
svm_region:
  // Mode 21 uses one boot-CPU SVM context: HSAVE, VMCB, four NPT levels,
  // three contiguous IOPM pages, and two contiguous MSRPM pages.
  .skip 45056
svm_region_end:

.balign 4096
p4_table:
  .skip 4096
p3_table:
  .skip 4096
p2_table:
  .skip 4096
p1_kernel_table:
  .skip 4096
p1_kernel_high_table:
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

// Milestone 1 gives every process slot a distinct guarded privilege-entry
// stack. Slot 0 deliberately reuses the bootstrap ring0 stack above so the
// existing single-process entry path and exported helper remain unchanged.
.balign 4096
ring0_stack1_guard:
  .skip 4096
ring0_stack1_bottom:
  .skip 65536
ring0_stack1_top:

.balign 4096
ring0_stack2_guard:
  .skip 4096
ring0_stack2_bottom:
  .skip 65536
ring0_stack2_top:

.balign 4096
ring0_stack3_guard:
  .skip 4096
ring0_stack3_bottom:
  .skip 65536
ring0_stack3_top:

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
.extern kernel_validate_user_return
.extern kernel_process_exit_continue
.extern kernel_timer_dispatch
.extern kernel_scheduler_complete_continue
.extern kernel_scheduler_syscall
#if OCORE_BOOT_INFO_ENABLED == 1
.extern ocore_boot_info_aperture_closed
#endif
_start:
  cli
  mov dword ptr [bootloader_magic_saved], eax
  mov dword ptr [bootloader_info_saved], ebx

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

  // PDEs 0 and 1 use contiguous 4 KiB leaf tables so kernel metadata, text,
  // read-only data, mutable state, page tables, stacks, and every stack guard
  // retain distinct W^X/presence permissions across the 2 MiB boundary.
  lea eax, [p1_kernel_table]
  or eax, 0x03
  mov dword ptr [p2_table], eax
  mov dword ptr [p2_table + 4], 0
  lea eax, [p1_kernel_high_table]
  or eax, 0x03
  mov dword ptr [p2_table + 1 * 8], eax
  mov dword ptr [p2_table + 1 * 8 + 4], 0

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

#if OCORE_BOOT_INFO_ENABLED == 1
  // 20..128 MiB: temporary supervisor RW/NX inspection window. Multiboot2
  // permits its information record anywhere in available RAM; the bounded
  // freestanding normalizer accepts only this mapped prefix. kernel_main must
  // remove and flush these entries before its W^X check and allocator init.
  mov ecx, 10
.map_boot_info_huge:
  mov eax, ecx
  shl eax, 21
  or eax, 0x83
  mov dword ptr [p2_table + ecx * 8], eax
  mov dword ptr [p2_table + ecx * 8 + 4], 0x80000000
  inc ecx
  cmp ecx, 64
  jne .map_boot_info_huge

  // 128..256 MiB: a complete temporary alias of physical 0..128 MiB. The
  // bounded normalizer reads the Multiboot2 record only through this alias, so
  // low physical source pages can be released without unmapping permanent
  // kernel, allocator, or user mappings below 20 MiB.
  xor ecx, ecx
.map_boot_info_alias:
  mov eax, ecx
  shl eax, 21
  or eax, 0x83
  mov dword ptr [p2_table + 64 * 8 + ecx * 8], eax
  mov dword ptr [p2_table + 64 * 8 + ecx * 8 + 4], 0x80000000
  inc ecx
  cmp ecx, 64
  jne .map_boot_info_alias
#endif

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
  lea edi, [ring0_stack1_guard]
  cmp eax, edi
  je .next_kernel_page
  lea edi, [ring0_stack2_guard]
  cmp eax, edi
  je .next_kernel_page
  lea edi, [ring0_stack3_guard]
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
  cmp ecx, 1024
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
  mov edi, dword ptr [rip + bootloader_magic_saved]
  mov esi, dword ptr [rip + bootloader_info_saved]
  mov dword ptr [rip + bootloader_magic_saved], 0
  mov dword ptr [rip + bootloader_info_saved], 0
  call kernel_main

.halt_forever:
  cli
  hlt
  jmp .halt_forever
.size _start, .-_start

// Close the bounded boot-information inspection mappings after normalization.
// Entries 10..63 are the temporary physical identity tail needed by Mode 34's
// ACPI consumer. Entries 64..127 are the normalizer's complete alias of
// physical 0..128 MiB, including source pages below 20 MiB. Permanent kernel,
// allocator, and user mappings remain intact. Reloading CR3 invalidates every
// removed translation before the O-core callback publishes the release marker.
.globl ocore_boot_info_close_window
.type ocore_boot_info_close_window, @function
ocore_boot_info_close_window:
#if OCORE_BOOT_INFO_ENABLED == 1
  lea rcx, [rip + p2_table]
  mov edx, 10
.close_boot_info_identity_page:
  mov qword ptr [rcx + rdx * 8], 0
  inc edx
  cmp edx, 64
  jne .close_boot_info_identity_page
  mov edx, 64
.close_boot_info_alias_page:
  mov qword ptr [rcx + rdx * 8], 0
  inc edx
  cmp edx, 128
  jne .close_boot_info_alias_page
  mov rax, cr3
  mov cr3, rax

  // Scan every entry rather than trusting the loop bounds. The callback also
  // requires the O-core parser cursor to be zero before it emits evidence.
  mov edx, 10
.verify_boot_info_identity_closed:
  cmp qword ptr [rcx + rdx * 8], 0
  jne .boot_info_aperture_failed
  inc edx
  cmp edx, 64
  jne .verify_boot_info_identity_closed
  mov edx, 64
.verify_boot_info_alias_closed:
  cmp qword ptr [rcx + rdx * 8], 0
  jne .boot_info_aperture_failed
  inc edx
  cmp edx, 128
  jne .verify_boot_info_alias_closed
  cmp dword ptr [rip + bootloader_magic_saved], 0
  jne .boot_info_aperture_failed
  cmp dword ptr [rip + bootloader_info_saved], 0
  jne .boot_info_aperture_failed
  mov edi, 1
  sub rsp, 8
  call ocore_boot_info_aperture_closed
  add rsp, 8
  test rax, rax
  jz .boot_info_aperture_failed
  ret
.boot_info_aperture_failed:
  cli
.boot_info_aperture_halt:
  hlt
  jmp .boot_info_aperture_halt
#else
  // Direct/PVH probes never mapped a Multiboot2 inspection aperture. Keep one
  // link-compatible no-op instead of carrying the firmware-close protocol.
  ret
#endif
.size ocore_boot_info_close_window, .-ocore_boot_info_close_window

#if OCORE_PROBE_MODE == 34
// The AP's first paged instruction is in the kernel RX segment, never on the
// writable/NX low-memory trampoline page.  Each AP enables x2APIC locally,
// claims exactly one firmware-selected slot, publishes its actual APIC ID and
// assigned stack, then crosses the BSP-owned two-phase barrier.
.balign 16
smp_ap_entry64:
  mov ax, 0x10
  mov ds, ax
  mov es, ax
  mov ss, ax
  xor ax, ax
  mov fs, ax
  mov gs, ax

  mov eax, 1
  cpuid
  test edx, 1 << 9
  jz .smp_ap_failed
  test ecx, 1 << 21
  jz .smp_ap_failed
  mov ecx, 0x1b
  rdmsr
  test eax, 1 << 11
  jz .smp_ap_failed
  or eax, (1 << 10) | (1 << 11)
  wrmsr
  mov ecx, 0x802
  rdmsr
  mov r8d, eax

  cmp r8, qword ptr [rip + smp_expected_ids]
  je .smp_ap_slot1
  cmp r8, qword ptr [rip + smp_expected_ids + 8]
  je .smp_ap_slot2
  cmp r8, qword ptr [rip + smp_expected_ids + 16]
  je .smp_ap_slot3
  jmp .smp_ap_failed

.smp_ap_slot1:
  mov r9d, 1
  lea rsp, [rip + smp_ap_stack1_top]
  jmp .smp_ap_publish
.smp_ap_slot2:
  mov r9d, 2
  lea rsp, [rip + smp_ap_stack2_top]
  jmp .smp_ap_publish
.smp_ap_slot3:
  mov r9d, 3
  lea rsp, [rip + smp_ap_stack3_top]

.smp_ap_publish:
  and rsp, -16
  lock bts dword ptr [rip + smp_claimed], r9d
  jc .smp_ap_failed
  lea r10, [rip + smp_observed_ids]
  mov qword ptr [r10 + r9 * 8], r8
  lea r10, [rip + smp_observed_stacks]
  mov qword ptr [r10 + r9 * 8], rsp
  mfence
  lock inc dword ptr [rip + smp_arrived]
.smp_ap_barrier:
  cmp dword ptr [rip + smp_release], 1
  je .smp_ap_released
  pause
  jmp .smp_ap_barrier
.smp_ap_released:
  lock inc dword ptr [rip + smp_progress]
  cli
.smp_ap_park:
  hlt
  jmp .smp_ap_park

.smp_ap_failed:
  lock or dword ptr [rip + smp_failure], 1
  cli
.smp_ap_failed_park:
  hlt
  jmp .smp_ap_failed_park

// Verify that the bounded firmware inspection aperture remains available
// while Mode 34 consumes ACPI.  The probe itself closes this aperture before
// any AP starts.
.globl ocore_smp_map_firmware_window
.type ocore_smp_map_firmware_window, @function
ocore_smp_map_firmware_window:
  cmp qword ptr [rip + p2_table + 10 * 8], 0
  je .smp_map_unavailable
  cmp qword ptr [rip + p2_table + 63 * 8], 0
  je .smp_map_unavailable
  cmp qword ptr [rip + p2_table + 64 * 8], 0
  je .smp_map_unavailable
  cmp qword ptr [rip + p2_table + 127 * 8], 0
  je .smp_map_unavailable
  mov eax, 1
  ret
.smp_map_unavailable:
  xor eax, eax
  ret
.size ocore_smp_map_firmware_window, .-ocore_smp_map_firmware_window

// Leaf-1 CPUID supplies the initial 8-bit APIC identity corresponding to the
// bounded MADT type-0 topology accepted by the O-core parser.
.globl ocore_smp_bsp_apic_id
.type ocore_smp_bsp_apic_id, @function
ocore_smp_bsp_apic_id:
  push rbx
  mov eax, 1
  cpuid
  test edx, 1 << 9
  jz .smp_bsp_id_failed
  mov eax, ebx
  shr eax, 24
  pop rbx
  ret
.smp_bsp_id_failed:
  mov rax, -1
  pop rbx
  ret
.size ocore_smp_bsp_apic_id, .-ocore_smp_bsp_apic_id

// One-shot PIT channel-2 interval.  Success requires the hardware OUT signal
// to transition; a bounded poll fails closed if the device never advances.
.type smp_pit_wait, @function
smp_pit_wait:
  test rdi, rdi
  jz .smp_pit_failed
  cmp rdi, 0xffff
  ja .smp_pit_failed
  mov r8d, edi
  mov dx, 0x61
  in al, dx
  and al, 0xfc
  out dx, al
  mov dx, 0x43
  mov al, 0xb0
  out dx, al
  mov dx, 0x42
  mov eax, r8d
  out dx, al
  shr eax, 8
  out dx, al
  mov dx, 0x61
  in al, dx
  or al, 0x01
  and al, 0xfd
  out dx, al
  mov ecx, 0x04000000
.smp_pit_poll:
  in al, dx
  test al, 0x20
  jnz .smp_pit_complete
  pause
  dec ecx
  jne .smp_pit_poll
.smp_pit_failed:
  xor eax, eax
  ret
.smp_pit_complete:
  mov eax, 1
  ret
.size smp_pit_wait, .-smp_pit_wait

.globl ocore_smp_timer_wait
.type ocore_smp_timer_wait, @function
ocore_smp_timer_wait:
  jmp smp_pit_wait
.size ocore_smp_timer_wait, .-ocore_smp_timer_wait

// Prepare an exact three-AP startup set.  The ACPI LAPIC base must agree with
// IA32_APIC_BASE, CPUID must advertise APIC+x2APIC, and the low trampoline PTE
// must remain supervisor RW/NX.  No AP has been signalled when this returns.
.globl ocore_smp_prepare
.type ocore_smp_prepare, @function
ocore_smp_prepare:
  push rbx
  push r12
  push r13
  push r14
  push r15
  mov r12, rdi
  mov r13, rsi
  mov r14, rdx
  mov r15, rcx
  cmp r12, 0x00100000
  jb .smp_prepare_failed
  mov eax, 0xfffff000
  cmp r12, rax
  ja .smp_prepare_failed
  test r12, 0xfff
  jne .smp_prepare_failed
  cmp r13, 0xff
  ja .smp_prepare_failed
  cmp r14, 0xff
  ja .smp_prepare_failed
  cmp r15, 0xff
  ja .smp_prepare_failed
  cmp r13, r14
  je .smp_prepare_failed
  cmp r13, r15
  je .smp_prepare_failed
  cmp r14, r15
  je .smp_prepare_failed

  mov eax, 1
  cpuid
  test edx, 1 << 9
  jz .smp_prepare_failed
  test ecx, 1 << 21
  jz .smp_prepare_failed
  mov ecx, 0x1b
  rdmsr
  test eax, 1 << 11
  jz .smp_prepare_failed
  mov r10d, eax
  mov r8, rdx
  shl r8, 32
  or r8, rax
  mov r9, r8
  mov eax, 0xfffff000
  and r9, rax
  cmp r9, r12
  jne .smp_prepare_failed
  mov eax, r10d
  or eax, (1 << 10) | (1 << 11)
  wrmsr
  mov ecx, 0x802
  rdmsr
  mov r11d, eax
  cmp r11, r13
  je .smp_prepare_failed
  cmp r11, r14
  je .smp_prepare_failed
  cmp r11, r15
  je .smp_prepare_failed

  // The admitted 0x8000 page is writable and non-executable after paging.
  mov rax, qword ptr [rip + p1_kernel_table + 8 * 8]
  mov rdx, rax
  and edx, 0x03
  cmp edx, 0x03
  jne .smp_prepare_failed
  bt rax, 63
  jnc .smp_prepare_failed

  lea rdi, [rip + smp_expected_ids]
  xor eax, eax
  mov ecx, offset SMP_CONTROL_QWORDS
  rep stosq
  mov qword ptr [rip + smp_expected_ids], r13
  mov qword ptr [rip + smp_expected_ids + 8], r14
  mov qword ptr [rip + smp_expected_ids + 16], r15
  lea rax, [rip + smp_ap_stack1_top]
  mov qword ptr [rip + smp_stack_tops], rax
  lea rax, [rip + smp_ap_stack2_top]
  mov qword ptr [rip + smp_stack_tops + 8], rax
  lea rax, [rip + smp_ap_stack3_top]
  mov qword ptr [rip + smp_stack_tops + 16], rax
  mov qword ptr [rip + smp_observed_ids], r11
  lea rax, [rip + stack_top]
  and rax, -16
  mov qword ptr [rip + smp_observed_stacks], rax
  mov dword ptr [rip + smp_claimed], 1

  // Clear the admitted page before copying the complete bounded template.
  mov edi, 0x8000
  xor eax, eax
  mov ecx, 512
  rep stosq
  lea rsi, [rip + smp_trampoline_template_start]
  mov edi, 0x8000
  mov ecx, offset SMP_TRAMPOLINE_TEMPLATE_BYTES
  cmp ecx, 4096
  ja .smp_prepare_failed
  rep movsb
  mov rax, cr3
  mov edi, 0x8000
  mov dword ptr [rdi + SMP_TRAMPOLINE_CR3_OFFSET], eax
  mfence
  // Phase the page from RW/NX to R/X only after the complete copy is stable.
  // No writable-and-executable trampoline mapping exists at any point.
  mov eax, 0x00008001
  mov qword ptr [rip + p1_kernel_table + 8 * 8], rax
  mov rax, cr3
  mov cr3, rax
  mov eax, 1
  jmp .smp_prepare_return
.smp_prepare_failed:
  xor eax, eax
.smp_prepare_return:
  pop r15
  pop r14
  pop r13
  pop r12
  pop rbx
  ret
.size ocore_smp_prepare, .-ocore_smp_prepare

.type smp_send_icr, @function
smp_send_icr:
  mov ecx, 0x830
  mov eax, esi
  mov edx, edi
  wrmsr
  ret
.size smp_send_icr, .-smp_send_icr

// Intel's universal startup sequence: one INIT, a measured 10 ms delay, one
// SIPI, a measured 200 us delay, and a second SIPI for each admitted AP.
.globl ocore_smp_launch
.type ocore_smp_launch, @function
ocore_smp_launch:
  cmp dword ptr [rip + smp_claimed], 1
  jne .smp_launch_failed
  cmp dword ptr [rip + smp_failure], 0
  jne .smp_launch_failed
  mov esi, 0x0000c500
  mov edi, dword ptr [rip + smp_expected_ids]
  call smp_send_icr
  mov edi, dword ptr [rip + smp_expected_ids + 8]
  call smp_send_icr
  mov edi, dword ptr [rip + smp_expected_ids + 16]
  call smp_send_icr
  mov edi, 11932
  call smp_pit_wait
  cmp eax, 1
  jne .smp_launch_failed

  mov esi, 0x00000608
  mov edi, dword ptr [rip + smp_expected_ids]
  call smp_send_icr
  mov edi, dword ptr [rip + smp_expected_ids + 8]
  call smp_send_icr
  mov edi, dword ptr [rip + smp_expected_ids + 16]
  call smp_send_icr
  mov edi, 239
  call smp_pit_wait
  cmp eax, 1
  jne .smp_launch_failed

  mov esi, 0x00000608
  mov edi, dword ptr [rip + smp_expected_ids]
  call smp_send_icr
  mov edi, dword ptr [rip + smp_expected_ids + 8]
  call smp_send_icr
  mov edi, dword ptr [rip + smp_expected_ids + 16]
  call smp_send_icr

  mov r10d, 100
.smp_wait_arrival:
  cmp dword ptr [rip + smp_failure], 0
  jne .smp_launch_failed
  cmp dword ptr [rip + smp_arrived], 3
  je .smp_arrival_complete
  mov edi, 1193
  call smp_pit_wait
  cmp eax, 1
  jne .smp_launch_failed
  dec r10d
  jne .smp_wait_arrival
  jmp .smp_launch_failed
.smp_arrival_complete:
  cmp dword ptr [rip + smp_claimed], 0x0f
  jne .smp_launch_failed
  // Every AP now executes from kernel RX text.  Restore RW/NX, erase the
  // one-shot bytes, then make the low page non-present and flush again.
  mov rax, 0x8000000000008003
  mov qword ptr [rip + p1_kernel_table + 8 * 8], rax
  mov rax, cr3
  mov cr3, rax
  mov edi, 0x8000
  xor eax, eax
  mov ecx, 512
  rep stosq
  mov qword ptr [rip + p1_kernel_table + 8 * 8], 0
  mov rax, cr3
  mov cr3, rax
  mov eax, 1
  ret
.smp_launch_failed:
  xor eax, eax
  ret
.size ocore_smp_launch, .-ocore_smp_launch

.globl ocore_smp_release_barrier
.type ocore_smp_release_barrier, @function
ocore_smp_release_barrier:
  cmp dword ptr [rip + smp_arrived], 3
  jne .smp_release_failed
  mov dword ptr [rip + smp_release], 1
  mfence
  mov r10d, 100
.smp_wait_progress:
  cmp dword ptr [rip + smp_failure], 0
  jne .smp_release_failed
  cmp dword ptr [rip + smp_progress], 3
  je .smp_progress_complete
  mov edi, 1193
  call smp_pit_wait
  cmp eax, 1
  jne .smp_release_failed
  dec r10d
  jne .smp_wait_progress
  jmp .smp_release_failed
.smp_progress_complete:
  mov rax, qword ptr [rip + smp_observed_ids + 8]
  cmp rax, qword ptr [rip + smp_expected_ids]
  jne .smp_release_failed
  mov rax, qword ptr [rip + smp_observed_ids + 16]
  cmp rax, qword ptr [rip + smp_expected_ids + 8]
  jne .smp_release_failed
  mov rax, qword ptr [rip + smp_observed_ids + 24]
  cmp rax, qword ptr [rip + smp_expected_ids + 16]
  jne .smp_release_failed
  mov rax, qword ptr [rip + smp_observed_stacks + 8]
  cmp rax, qword ptr [rip + smp_stack_tops]
  jne .smp_release_failed
  mov rax, qword ptr [rip + smp_observed_stacks + 16]
  cmp rax, qword ptr [rip + smp_stack_tops + 8]
  jne .smp_release_failed
  mov rax, qword ptr [rip + smp_observed_stacks + 24]
  cmp rax, qword ptr [rip + smp_stack_tops + 16]
  jne .smp_release_failed
  mov eax, 1
  ret
.smp_release_failed:
  xor eax, eax
  ret
.size ocore_smp_release_barrier, .-ocore_smp_release_barrier

.globl ocore_smp_cpu_apic_id
.type ocore_smp_cpu_apic_id, @function
ocore_smp_cpu_apic_id:
  cmp rdi, 3
  ja .smp_cpu_value_failed
  lea rax, [rip + smp_observed_ids]
  mov rax, qword ptr [rax + rdi * 8]
  ret
.size ocore_smp_cpu_apic_id, .-ocore_smp_cpu_apic_id

.globl ocore_smp_cpu_stack
.type ocore_smp_cpu_stack, @function
ocore_smp_cpu_stack:
  cmp rdi, 3
  ja .smp_cpu_value_failed
  lea rax, [rip + smp_observed_stacks]
  mov rax, qword ptr [rax + rdi * 8]
  ret
.smp_cpu_value_failed:
  mov rax, -1
  ret
.size ocore_smp_cpu_stack, .-ocore_smp_cpu_stack

.globl ocore_smp_arrival_count
.type ocore_smp_arrival_count, @function
ocore_smp_arrival_count:
  mov eax, dword ptr [rip + smp_arrived]
  ret
.size ocore_smp_arrival_count, .-ocore_smp_arrival_count

.globl ocore_smp_progress_count
.type ocore_smp_progress_count, @function
ocore_smp_progress_count:
  mov eax, dword ptr [rip + smp_progress]
  ret
.size ocore_smp_progress_count, .-ocore_smp_progress_count
#endif

// Return the trusted entry-stack top for the kernel-owned PCB metadata.
.globl ocore_kernel_stack_top
.type ocore_kernel_stack_top, @function
ocore_kernel_stack_top:
  lea rax, [rip + ring0_stack_top]
  ret
.size ocore_kernel_stack_top, .-ocore_kernel_stack_top

// Return the guarded ring-0 entry stack assigned to a process registry slot.
// Invalid slots return zero and therefore cannot be published in a PCB.
.globl ocore_process_kernel_stack_top
.type ocore_process_kernel_stack_top, @function
ocore_process_kernel_stack_top:
  cmp rdi, 0
  je .process_stack0
  cmp rdi, 1
  je .process_stack1
  cmp rdi, 2
  je .process_stack2
  cmp rdi, 3
  je .process_stack3
  xor eax, eax
  ret
.process_stack0:
  lea rax, [rip + ring0_stack_top]
  ret
.process_stack1:
  lea rax, [rip + ring0_stack1_top]
  ret
.process_stack2:
  lea rax, [rip + ring0_stack2_top]
  ret
.process_stack3:
  lea rax, [rip + ring0_stack3_top]
  ret
.size ocore_process_kernel_stack_top, .-ocore_process_kernel_stack_top

// Trusted paging-template addresses. These symbols name physical addresses in
// the identity-mapped bootstrap and process kernel regions; neither is exposed
// to CPL3.
.globl ocore_kernel_p1_address
.type ocore_kernel_p1_address, @function
ocore_kernel_p1_address:
  lea rax, [rip + p1_kernel_table]
  ret
.size ocore_kernel_p1_address, .-ocore_kernel_p1_address

// Trusted immutable image boundary. Callers must still validate the OVFS and
// nested ELF structures before allocating or publishing a process.
.globl ocore_m4_image_start
.type ocore_m4_image_start, @function
ocore_m4_image_start:
  lea rax, [rip + ocore_m4_image_bytes_start]
  ret
.size ocore_m4_image_start, .-ocore_m4_image_start

.globl ocore_m4_image_end
.type ocore_m4_image_end, @function
ocore_m4_image_end:
  lea rax, [rip + ocore_m4_image_bytes_end]
  ret
.size ocore_m4_image_end, .-ocore_m4_image_end

.globl ocore_m5_image_start
.type ocore_m5_image_start, @function
ocore_m5_image_start:
  lea rax, [rip + ocore_m5_image_bytes_start]
  ret
.size ocore_m5_image_start, .-ocore_m5_image_start

.globl ocore_m5_image_end
.type ocore_m5_image_end, @function
ocore_m5_image_end:
  lea rax, [rip + ocore_m5_image_bytes_end]
  ret
.size ocore_m5_image_end, .-ocore_m5_image_end

.globl ocore_m6_image_start
.type ocore_m6_image_start, @function
ocore_m6_image_start:
  lea rax, [rip + ocore_m6_image_bytes_start]
  ret
.size ocore_m6_image_start, .-ocore_m6_image_start

.globl ocore_m6_image_end
.type ocore_m6_image_end, @function
ocore_m6_image_end:
  lea rax, [rip + ocore_m6_image_bytes_end]
  ret
.size ocore_m6_image_end, .-ocore_m6_image_end

.globl ocore_m6b_live_image_start
.type ocore_m6b_live_image_start, @function
ocore_m6b_live_image_start:
  lea rax, [rip + ocore_m6b_live_image_bytes_start]
  ret
.size ocore_m6b_live_image_start, .-ocore_m6b_live_image_start

.globl ocore_m6b_live_image_end
.type ocore_m6b_live_image_end, @function
ocore_m6b_live_image_end:
  lea rax, [rip + ocore_m6b_live_image_bytes_end]
  ret
.size ocore_m6b_live_image_end, .-ocore_m6b_live_image_end

.globl ocore_m6_linux_image_start
.type ocore_m6_linux_image_start, @function
ocore_m6_linux_image_start:
  lea rax, [rip + ocore_m6_linux_image_bytes_start]
  ret
.size ocore_m6_linux_image_start, .-ocore_m6_linux_image_start

.globl ocore_m6_linux_image_end
.type ocore_m6_linux_image_end, @function
ocore_m6_linux_image_end:
  lea rax, [rip + ocore_m6_linux_image_bytes_end]
  ret
.size ocore_m6_linux_image_end, .-ocore_m6_linux_image_end

.globl ocore_m7_linux_plan9_image_start
.type ocore_m7_linux_plan9_image_start, @function
ocore_m7_linux_plan9_image_start:
  lea rax, [rip + ocore_m7_linux_plan9_image_bytes_start]
  ret
.size ocore_m7_linux_plan9_image_start, .-ocore_m7_linux_plan9_image_start

.globl ocore_m7_linux_plan9_image_end
.type ocore_m7_linux_plan9_image_end, @function
ocore_m7_linux_plan9_image_end:
  lea rax, [rip + ocore_m7_linux_plan9_image_bytes_end]
  ret
.size ocore_m7_linux_plan9_image_end, .-ocore_m7_linux_plan9_image_end

.globl ocore_m7b_logical_read_image_start
.type ocore_m7b_logical_read_image_start, @function
ocore_m7b_logical_read_image_start:
  lea rax, [rip + ocore_m7b_logical_read_image_bytes_start]
  ret
.size ocore_m7b_logical_read_image_start, .-ocore_m7b_logical_read_image_start

.globl ocore_m7b_logical_read_image_end
.type ocore_m7b_logical_read_image_end, @function
ocore_m7b_logical_read_image_end:
  lea rax, [rip + ocore_m7b_logical_read_image_bytes_end]
  ret
.size ocore_m7b_logical_read_image_end, .-ocore_m7b_logical_read_image_end

.globl ocore_kernel_world_record_start
.type ocore_kernel_world_record_start, @function
ocore_kernel_world_record_start:
  lea rax, [rip + ocore_kernel_world_record_bytes_start]
  ret
.size ocore_kernel_world_record_start, .-ocore_kernel_world_record_start

.globl ocore_kernel_world_record_end
.type ocore_kernel_world_record_end, @function
ocore_kernel_world_record_end:
  lea rax, [rip + ocore_kernel_world_record_bytes_end]
  ret
.size ocore_kernel_world_record_end, .-ocore_kernel_world_record_end

.globl ocore_svm_region
.type ocore_svm_region, @function
ocore_svm_region:
  lea rax, [rip + svm_region]
  ret
.size ocore_svm_region, .-ocore_svm_region

.globl ocore_svm_capabilities
.type ocore_svm_capabilities, @function
ocore_svm_capabilities:
  push rbx
  mov eax, 0x80000000
  cpuid
  cmp eax, 0x8000000a
  jb .svm_capabilities_none
  mov eax, 0x80000001
  cpuid
  bt ecx, 2
  jnc .svm_capabilities_none
  mov eax, 0x8000000a
  cpuid
  mov eax, 1
  bt edx, 0
  jnc .svm_capabilities_done
  or eax, 2
  jmp .svm_capabilities_done
.svm_capabilities_none:
  xor eax, eax
.svm_capabilities_done:
  pop rbx
  ret
.size ocore_svm_capabilities, .-ocore_svm_capabilities

.globl ocore_svm_enable
.type ocore_svm_enable, @function
ocore_svm_enable:
  test rdi, 0xfff
  jnz .svm_enable_failed
  mov ecx, 0xc0000080
  rdmsr
  bts eax, 12
  wrmsr
  mov rax, rdi
  mov rdx, rax
  shr rdx, 32
  mov ecx, 0xc0010117
  wrmsr
  mov eax, 1
  ret
.svm_enable_failed:
  xor eax, eax
  ret
.size ocore_svm_enable, .-ocore_svm_enable

.globl ocore_svm_disable
.type ocore_svm_disable, @function
ocore_svm_disable:
  xor eax, eax
  xor edx, edx
  mov ecx, 0xc0010117
  wrmsr
  mov ecx, 0xc0000080
  rdmsr
  btr eax, 12
  wrmsr
  ret
.size ocore_svm_disable, .-ocore_svm_disable

.globl ocore_svm_vmrun
.type ocore_svm_vmrun, @function
ocore_svm_vmrun:
  push rbx
  push rbp
  push r12
  push r13
  push r14
  push r15
  mov rax, rdi
  vmrun rax
  stgi
  pop r15
  pop r14
  pop r13
  pop r12
  pop rbp
  pop rbx
  mov eax, 1
  ret
.size ocore_svm_vmrun, .-ocore_svm_vmrun

.globl ocore_management_cr3
.type ocore_management_cr3, @function
ocore_management_cr3:
  lea rax, [rip + p4_table]
  ret
.size ocore_management_cr3, .-ocore_management_cr3

.globl ocore_management_stack_top
.type ocore_management_stack_top, @function
ocore_management_stack_top:
  lea rax, [rip + stack_top]
  ret
.size ocore_management_stack_top, .-ocore_management_stack_top

.globl ocore_current_cr3
.type ocore_current_cr3, @function
ocore_current_cr3:
  mov rax, cr3
  ret
.size ocore_current_cr3, .-ocore_current_cr3

// Install the complete architectural half of a process identity transaction.
// RDI is a page-aligned CR3 root and RSI is a 16-byte-aligned guarded entry
// stack top. The operation deliberately returns with maskable interrupts off;
// the caller must publish the matching PCB/CSpace before IRET restores IF.
.globl ocore_context_install
.type ocore_context_install, @function
ocore_context_install:
  test rdi, rdi
  jz .context_install_failed
  test rdi, 0xfff
  jnz .context_install_failed
  test rsi, rsi
  jz .context_install_failed
  test rsi, 0xf
  jnz .context_install_failed
  cli
  mov qword ptr [rip + percpu_entry], 0
  mov qword ptr [rip + percpu_entry + 8], 0
  mov qword ptr [rip + percpu_entry + 16], 0
  mov qword ptr [rip + percpu_entry + 32], 0
  mov qword ptr [rip + tss64 + 4], rsi
  mov qword ptr [rip + percpu_entry + 24], rsi
  mov cr3, rdi
  mov eax, 1
  ret
.context_install_failed:
  xor eax, eax
  ret
.size ocore_context_install, .-ocore_context_install

// Return to the pinned no-process address space. Like process installation,
// this leaves interrupts disabled so teardown cannot observe a mixed context.
.globl ocore_restore_management_context
.type ocore_restore_management_context, @function
ocore_restore_management_context:
  cli
  mov qword ptr [rip + percpu_entry], 0
  mov qword ptr [rip + percpu_entry + 8], 0
  mov qword ptr [rip + percpu_entry + 16], 0
  mov qword ptr [rip + percpu_entry + 32], 0
  lea rdx, [rip + stack_top]
  mov qword ptr [rip + tss64 + 4], rdx
  mov qword ptr [rip + percpu_entry + 24], rdx
  lea rax, [rip + p4_table]
  mov cr3, rax
  mov eax, 1
  ret
.size ocore_restore_management_context, .-ocore_restore_management_context

.globl ocore_tss_rsp0
.type ocore_tss_rsp0, @function
ocore_tss_rsp0:
  mov rax, qword ptr [rip + tss64 + 4]
  ret
.size ocore_tss_rsp0, .-ocore_tss_rsp0

.globl ocore_entry_stack_top
.type ocore_entry_stack_top, @function
ocore_entry_stack_top:
  mov rax, qword ptr [rip + percpu_entry + 24]
  ret
.size ocore_entry_stack_top, .-ocore_entry_stack_top

.globl ocore_context_matches
.type ocore_context_matches, @function
ocore_context_matches:
  mov rax, cr3
  cmp rax, rdi
  jne .context_mismatch
  cmp qword ptr [rip + tss64 + 4], rsi
  jne .context_mismatch
  cmp qword ptr [rip + percpu_entry + 24], rsi
  jne .context_mismatch
  mov eax, 1
  ret
.context_mismatch:
  xor eax, eax
  ret
.size ocore_context_matches, .-ocore_context_matches

// Deterministic scheduler stress primitive. Each half-iteration performs the
// architectural CR3, TSS.RSP0, and GS-entry-stack transaction and verifies it
// immediately. The caller separately validates saved register frames and PCB,
// domain, address-space, and CSpace identity under real timer preemption.
.globl ocore_forced_context_stress
.type ocore_forced_context_stress, @function
ocore_forced_context_stress:
  test rdi, rdi
  jz .context_stress_failed_early
  test rdi, 0xfff
  jnz .context_stress_failed_early
  test rdx, rdx
  jz .context_stress_failed_early
  test rdx, 0xfff
  jnz .context_stress_failed_early
  test rsi, rsi
  jz .context_stress_failed_early
  test rsi, 0xf
  jnz .context_stress_failed_early
  test rcx, rcx
  jz .context_stress_failed_early
  test rcx, 0xf
  jnz .context_stress_failed_early
  test r8, r8
  jz .context_stress_failed_early
  cli
  push rbx
  xor ebx, ebx
.context_stress_loop:
  mov qword ptr [rip + tss64 + 4], rsi
  mov qword ptr [rip + percpu_entry + 24], rsi
  mov cr3, rdi
  mov r9, cr3
  cmp r9, rdi
  jne .context_stress_failed
  cmp qword ptr [rip + tss64 + 4], rsi
  jne .context_stress_failed
  cmp qword ptr [rip + percpu_entry + 24], rsi
  jne .context_stress_failed
  inc rbx

  mov qword ptr [rip + tss64 + 4], rcx
  mov qword ptr [rip + percpu_entry + 24], rcx
  mov cr3, rdx
  mov r9, cr3
  cmp r9, rdx
  jne .context_stress_failed
  cmp qword ptr [rip + tss64 + 4], rcx
  jne .context_stress_failed
  cmp qword ptr [rip + percpu_entry + 24], rcx
  jne .context_stress_failed
  inc rbx
  dec r8
  jnz .context_stress_loop

  call ocore_restore_management_context
  mov rax, rbx
  pop rbx
  ret
.context_stress_failed:
  call ocore_restore_management_context
  xor eax, eax
  pop rbx
  ret
.context_stress_failed_early:
  xor eax, eax
  ret
.size ocore_forced_context_stress, .-ocore_forced_context_stress

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
  lea rax, [rip + ring0_stack1_guard]
  shr rax, 12
  cmp qword ptr [rcx + rax * 8], 0
  jne .protection_check_failed
  lea rax, [rip + ring0_stack2_guard]
  shr rax, 12
  cmp qword ptr [rcx + rax * 8], 0
  jne .protection_check_failed
  lea rax, [rip + ring0_stack3_guard]
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

  // PDE1 must name the second supervisor-only 4 KiB kernel leaf table. Ignore
  // only the hardware-owned Accessed and Dirty bits when comparing it.
  mov rdx, qword ptr [rip + p2_table + 1 * 8]
  lea rax, [rip + p1_kernel_high_table]
  or rax, 0x03
  xor rdx, rax
  and rdx, -97
  jne .protection_check_failed

  // Both temporary Multiboot2 windows have been removed: the physical identity
  // tail and the complete 0..128 MiB inspection alias are absent. No residual
  // 1 GiB identity RWX map remains.
  cmp qword ptr [rip + p2_table + 10 * 8], 0
  jne .protection_check_failed
#if OCORE_BOOT_INFO_ENABLED == 1
  cmp qword ptr [rip + p2_table + 63 * 8], 0
  jne .protection_check_failed
  cmp qword ptr [rip + p2_table + 64 * 8], 0
  jne .protection_check_failed
  cmp qword ptr [rip + p2_table + 127 * 8], 0
  jne .protection_check_failed
#endif
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

  // Materialize the same canonical 22-word frame used by IRQ and exception
  // entry. Synchronous yield/sleep can therefore return through a different
  // saved TCB without a second register representation.
  sub rsp, 176
  mov qword ptr [rsp], r15
  mov qword ptr [rsp + 8], r14
  mov qword ptr [rsp + 16], r13
  mov qword ptr [rsp + 24], r12
  mov qword ptr [rsp + 32], r11
  mov qword ptr [rsp + 40], r10
  mov qword ptr [rsp + 48], r9
  mov qword ptr [rsp + 56], r8
  mov qword ptr [rsp + 64], rdi
  mov qword ptr [rsp + 72], rsi
  mov qword ptr [rsp + 80], rbp
  mov qword ptr [rsp + 88], rdx
  mov qword ptr [rsp + 96], rcx
  mov qword ptr [rsp + 104], rbx
  mov qword ptr [rsp + 112], rax
  mov qword ptr [rsp + 120], 0x100
  mov qword ptr [rsp + 128], 0
  mov qword ptr [rsp + 136], rcx
  mov qword ptr [rsp + 144], 0x23
  mov qword ptr [rsp + 152], r11
  mov rax, qword ptr gs:[0]
  mov qword ptr [rsp + 160], rax
  mov qword ptr [rsp + 168], 0x1b

  mov rax, qword ptr [rsp + 48]
  mov rdi, qword ptr [rsp + 112]
  mov rsi, qword ptr [rsp + 64]
  mov rdx, qword ptr [rsp + 72]
  mov rcx, qword ptr [rsp + 88]
  mov r8, qword ptr [rsp + 40]
  mov r9, qword ptr [rsp + 56]
  sub rsp, 16
  mov qword ptr [rsp], rax
  call kernel_syscall_dispatch
  add rsp, 16
  // SYS_EXIT is a kernel-private control transfer, not a value that can be
  // observed at CPL3. Keep the saved process frame on its guarded stack until
  // the management CR3 is active, then abandon it permanently.
  cmp rax, -32
  je .abandon_process_context
  cmp rax, -31
  je .schedule_from_syscall

  mov qword ptr [rsp + 112], rax

  mov rdi, qword ptr [rsp + 136]
  mov rsi, qword ptr [rsp + 160]
  call kernel_validate_user_return
  test rax, rax
  jz .invalid_user_return_saved

  // Restore only the architectural reserved bit and IF. This clears IOPL,
  // NT, VM, TF, DF, AC, and every other user-controlled return hazard.
  mov qword ptr [rsp + 152], 0x202
  mov qword ptr [rsp + 32], 0x202
  mov rax, qword ptr [rsp + 136]
  mov qword ptr [rsp + 96], rax
  jmp .restore_syscall_frame

.schedule_from_syscall:
  mov qword ptr [rsp + 112], 0
  mov qword ptr [rsp + 152], 0x202
  mov qword ptr [rsp + 32], 0x202
  mov rax, qword ptr [rsp + 136]
  mov qword ptr [rsp + 96], rax
  mov rdi, rsp
  call kernel_scheduler_syscall
  test rax, rax
  jz .timer_scheduler_complete
  mov rbx, rax
  mov rdi, qword ptr [rbx + 136]
  mov rsi, qword ptr [rbx + 160]
  call kernel_validate_user_return
  test rax, rax
  jz .fatal_exception
  // The scheduler may select a different TCB whose frame was saved by IRQ0.
  // Sanitize that exact frame just as the timer-return path does below; the
  // caller frame sanitized above is no longer the frame IRETQ will consume.
  mov qword ptr [rbx + 152], 0x202
  mov rsp, rbx

.restore_syscall_frame:
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
  swapgs
  iretq

.invalid_user_return_saved:
  mov rcx, qword ptr [rsp + 136]
  mov r11, qword ptr [rsp + 160]
.invalid_user_return:
  mov rdi, rcx
  mov rsi, r11
  call kernel_invalid_user_return
  cmp rax, 1
  jne .fatal_exception
  jmp ocore_fault_idle

// Both a successful process exit and a contained M1 process fault arrive here
// with kernel GS active and interrupts masked. Switch every architectural
// identity component back to the pinned management context before discarding
// the process's guarded entry stack.
.abandon_process_context:
  call ocore_restore_management_context
  lea rsp, [rip + stack_top]
  and rsp, -16
  xor rbp, rbp
  call kernel_process_exit_continue
  jmp .fatal_exception
.size ocore_syscall_entry, .-ocore_syscall_entry

// Timer entry uses the same normalized 22-word frame as exception dispatch,
// but its dispatcher may return a different saved thread frame. This makes an
// IRQ0 preemption a complete register/IRET context switch while preserving the
// legacy same-frame behavior when the scheduler is not armed.
.globl ocore_timer_interrupt_entry
.type ocore_timer_interrupt_entry, @function
ocore_timer_interrupt_entry:
  push 0
  push 32
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

  mov rax, qword ptr [rsp + 144]
  test al, 3
  jz .timer_kernel_gs_active
  swapgs
.timer_kernel_gs_active:
  mov rdi, rsp
  call kernel_timer_dispatch
  test rax, rax
  jz .timer_scheduler_complete
  mov rbx, rax

  // Validate and sanitize every selected CPL3 frame, including a different
  // TCB returned by the scheduler. A hostile user RFLAGS value interrupted by
  // IRQ0 must not survive merely because this return uses IRETQ rather than
  // the SYSCALL return path.
  mov rax, qword ptr [rbx + 144]
  test al, 3
  jz .timer_frame_validated
  mov rdi, qword ptr [rbx + 136]
  mov rsi, qword ptr [rbx + 160]
  call kernel_validate_user_return
  test rax, rax
  jz .fatal_exception
  mov qword ptr [rbx + 152], 0x202
.timer_frame_validated:
  mov rsp, rbx

  // The selected frame, rather than the interrupted frame, decides whether
  // return crosses to CPL3 and therefore requires SWAPGS.
  mov rax, qword ptr [rsp + 144]
  test al, 3
  jz .restore_timer_frame
  swapgs
.restore_timer_frame:
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

.timer_scheduler_complete:
  call ocore_restore_management_context
  lea rsp, [rip + stack_top]
  and rsp, -16
  xor rbp, rbp
  call kernel_scheduler_complete_continue
  jmp .fatal_exception
.size ocore_timer_interrupt_entry, .-ocore_timer_interrupt_entry

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
  je ocore_fault_idle
  cmp rax, 2
  je .abandon_process_context
  test rax, rax
  jz .fatal_exception
  // Mode 14 may contain the faulting service and return a different TCB's
  // normalized frame. Validate that selected process's user return state after
  // the scheduler has installed its CR3/TSS/GS/PCB/CSpace identity.
  mov rbx, rax
  mov rax, qword ptr [rbx + 144]
  test al, 3
  jz .scheduled_exception_frame_validated
  mov rdi, qword ptr [rbx + 136]
  mov rsi, qword ptr [rbx + 160]
  call kernel_validate_user_return
  test rax, rax
  jz .fatal_exception
  mov qword ptr [rbx + 152], 0x202
.scheduled_exception_frame_validated:
  mov rsp, rbx
  mov rax, qword ptr [rsp + 144]
  test al, 3
  jz .restore_exception
  swapgs
  jmp .restore_exception

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

// M2 scheduler containment probe. SYSCALL does not consume the user stack, so
// this reaches the trusted entry stack with a deliberately unmapped guard-page
// RSP. The scheduler must retire this TCB and select a sibling; returning here
// would mean the hostile frame escaped validation.
.globl ocore_m2_hostile_rsp_yield
.type ocore_m2_hostile_rsp_yield, @function
ocore_m2_hostile_rsp_yield:
  mov rsp, 0x01200000
  mov eax, 4
  syscall
  ud2
.size ocore_m2_hostile_rsp_yield, .-ocore_m2_hostile_rsp_yield

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
  // IRETQ performs the architecturally required canonicality check before
  // committing this same-CPL return. Older QEMU releases incorrectly report
  // an indirect jump to this address as #PF rather than the expected #GP.
  sub rsp, 0x20
  pushfq
  push 0x23
  movabs rax, 0x0000800000000000
  push rax
  iretq
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
