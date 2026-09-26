; Interrupt entry stubs. Each stub pushes an error code (a dummy 0 when
; the CPU does not push one) and its vector number, then jumps to
; isr_common, which saves the general purpose registers and calls
; interrupt_dispatch(frame) in kernel/src/interrupts.rs.
global isr_stub_table
extern interrupt_dispatch

section .text
bits 64

%assign i 0
%rep 48
isr_%[i]:
    ; exceptions 8, 10-14, 17, 21, 29 and 30 push an error code themselves
  %if i == 8 || (i >= 10 && i <= 14) || i == 17 || i == 21 || i == 29 || i == 30
  %else
    push 0
  %endif
    push i
    jmp isr_common
%assign i i + 1
%endrep

isr_common:
    push rax
    push rbx
    push rcx
    push rdx
    push rsi
    push rdi
    push rbp
    push r8
    push r9
    push r10
    push r11
    push r12
    push r13
    push r14
    push r15

    mov rdi, rsp                ; pointer to the saved frame
    cld
    call interrupt_dispatch

    pop r15
    pop r14
    pop r13
    pop r12
    pop r11
    pop r10
    pop r9
    pop r8
    pop rbp
    pop rdi
    pop rsi
    pop rdx
    pop rcx
    pop rbx
    pop rax
    add rsp, 16                 ; drop vector number and error code
    iretq

section .rodata
align 8
; addresses of the 48 stubs, used by Rust to fill the IDT
isr_stub_table:
%assign i 0
%rep 48
    dq isr_%[i]
%assign i i + 1
%endrep
