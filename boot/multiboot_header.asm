; Multiboot2 header: tells GRUB that this file is a kernel it can load.
section .multiboot_header
align 8
header_start:
    dd 0xe85250d6                ; multiboot2 magic number
    dd 0                         ; architecture: i386 (protected mode)
    dd header_end - header_start ; header length
    ; checksum: magic + architecture + length + checksum must equal 0
    dd 0x100000000 - (0xe85250d6 + 0 + (header_end - header_start))

    ; framebuffer tag: ask GRUB for a 1920x1080 32-bit graphics mode.
    ; It is optional (flags = 1), so GRUB may still boot us in text mode.
align 8
    dw 5                         ; type: framebuffer
    dw 1                         ; flags: optional
    dd 20                        ; size
    dd 1920                      ; width
    dd 1080                      ; height
    dd 32                        ; depth (bits per pixel)

    ; end tag
align 8
    dw 0
    dw 0
    dd 8
header_end:
