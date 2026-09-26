; 64-bit entry point: reset segment registers and call the Rust kernel.
global long_mode_start
extern kernel_main

section .text
bits 64
long_mode_start:
    mov ax, 0
    mov ss, ax
    mov ds, ax
    mov es, ax
    mov fs, ax
    mov gs, ax

    call kernel_main            ; edi still holds the multiboot2 info pointer
.hang:
    cli
    hlt
    jmp .hang
