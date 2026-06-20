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

.section .rodata,"a",@progbits
.balign 8
gdt64:
  .quad 0
  .quad 0x00af9a000000ffff
  .quad 0x00af92000000ffff
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
  or eax, 0x03
  mov dword ptr [p4_table], eax
  mov dword ptr [p4_table + 4], 0

  lea eax, [p2_table]
  or eax, 0x03
  mov dword ptr [p3_table], eax
  mov dword ptr [p3_table + 4], 0

  xor ecx, ecx
.map_identity:
  mov eax, ecx
  shl eax, 21
  or eax, 0x83
  mov dword ptr [p2_table + ecx * 8], eax
  mov dword ptr [p2_table + ecx * 8 + 4], 0
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
  or eax, 1 << 8
  wrmsr

  mov eax, cr0
  or eax, 1 << 31
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
