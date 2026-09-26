; 32-bit entry point. GRUB jumps here in protected mode.
; We check the CPU, build identity-mapped page tables, enable
; long mode and jump to 64-bit code in long_mode.asm.
global start
extern long_mode_start

section .text
bits 32
start:
    mov esp, stack_top
    mov edi, ebx                ; multiboot2 info pointer, passed to Rust later

    call check_multiboot
    call check_cpuid
    call check_long_mode

    call set_up_page_tables
    call enable_paging

    lgdt [gdt64.pointer]
    jmp gdt64.code:long_mode_start

; Print "ERR: X" in red, where X is the error code in al, then halt.
error:
    mov dword [0xb8000], 0x4f524f45
    mov dword [0xb8004], 0x4f3a4f52
    mov dword [0xb8008], 0x4f204f20
    mov byte  [0xb800a], al
    hlt

check_multiboot:
    cmp eax, 0x36d76289         ; magic value GRUB leaves in eax
    jne .no_multiboot
    ret
.no_multiboot:
    mov al, "0"
    jmp error

check_cpuid:
    ; CPUID is supported if bit 21 (ID) in EFLAGS can be flipped.
    pushfd
    pop eax
    mov ecx, eax
    xor eax, 1 << 21
    push eax
    popfd
    pushfd
    pop eax
    push ecx
    popfd
    cmp eax, ecx
    je .no_cpuid
    ret
.no_cpuid:
    mov al, "1"
    jmp error

check_long_mode:
    mov eax, 0x80000000         ; ask for the highest extended function
    cpuid
    cmp eax, 0x80000001
    jb .no_long_mode
    mov eax, 0x80000001
    cpuid
    test edx, 1 << 29           ; LM bit
    jz .no_long_mode
    ret
.no_long_mode:
    mov al, "2"
    jmp error

; Identity-map the first 4 GiB with 2 MiB huge pages. The low 4 GiB
; include the kernel, the multiboot info and the framebuffer, which
; GRUB usually places just below 4 GiB (0xfd000000 in QEMU).
set_up_page_tables:
    mov eax, p3_table
    or eax, 0b11                ; present + writable
    mov [p4_table], eax

    ; point the first four P3 entries at the four P2 tables
    mov ecx, 0
.map_p3_table:
    mov eax, 4096
    mul ecx
    add eax, p2_tables
    or eax, 0b11
    mov [p3_table + ecx * 8], eax
    inc ecx
    cmp ecx, 4
    jne .map_p3_table

    ; fill 4 * 512 P2 entries, each mapping 2 MiB
    mov ecx, 0
.map_p2_table:
    mov eax, 0x200000           ; 2 MiB
    mul ecx
    or eax, 0b10000011          ; present + writable + huge
    mov [p2_tables + ecx * 8], eax
    inc ecx
    cmp ecx, 512 * 4
    jne .map_p2_table
    ret

enable_paging:
    mov eax, p4_table
    mov cr3, eax

    mov eax, cr4                ; enable PAE, and SSE (OSFXSR, OSXMMEXCPT)
    or eax, (1 << 5) | (1 << 9) | (1 << 10)
    mov cr4, eax

    mov ecx, 0xC0000080         ; EFER MSR: set long mode bit
    rdmsr
    or eax, 1 << 8
    wrmsr

    mov eax, cr0                ; enable paging; SSE needs EM clear and MP set
    and eax, ~(1 << 2)
    or eax, (1 << 31) | (1 << 1)
    mov cr0, eax
    ret

section .rodata
gdt64:
    dq 0                                              ; null descriptor
.code: equ $ - gdt64
    dq (1 << 43) | (1 << 44) | (1 << 47) | (1 << 53)  ; 64-bit code segment
.pointer:
    dw $ - gdt64 - 1
    dq gdt64

section .bss
align 4096
p4_table:
    resb 4096
p3_table:
    resb 4096
p2_tables:
    resb 4096 * 4
stack_bottom:
    resb 4096 * 256              ; 1 MiB: icons live on the stack, and JavaScript recurses deeply
stack_top:
