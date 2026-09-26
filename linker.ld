ENTRY(start)

SECTIONS {
    . = 1M;

    .boot :
    {
        /* the multiboot2 header must be near the start of the file */
        KEEP(*(.multiboot_header))
    }

    .text : { *(.text .text.*) }
    .rodata : { *(.rodata .rodata.*) }
    .data : { *(.data .data.*) }
    .bss : { *(.bss .bss.*) }
}
