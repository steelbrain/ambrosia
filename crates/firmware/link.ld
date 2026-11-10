/*
 * The image runs where vmapple loads it: the firmware region, 1 MiB at
 * 0x00100000. The region is RAM, so the image keeps its data, bss and stack
 * there too. The machine reloads the image's bytes on reset but not the
 * rest of the region, so the start code zeroes the bss itself.
 */
ENTRY(_start)

MEMORY {
    firmware (rwx) : ORIGIN = 0x00100000, LENGTH = 1M
}

SECTIONS {
    .text : {
        KEEP(*(.text.start))
        *(.text .text.*)
    } > firmware
    .rodata : ALIGN(16) {
        *(.rodata .rodata.*)
    } > firmware
    .data : ALIGN(16) {
        *(.data .data.*)
        *(.got .got.*)
    } > firmware
    __image_end = .;
    .bss (NOLOAD) : ALIGN(4096) {
        __bss_start = .;
        *(.bss .bss.*)
        *(COMMON)
        . = ALIGN(16);
        __bss_end = .;
    } > firmware
    .stack (NOLOAD) : ALIGN(16) {
        . += 0x10000;
        __stack_top = .;
    } > firmware
    /DISCARD/ : {
        *(.comment)
        *(.note .note.*)
        *(.eh_frame .eh_frame_hdr)
    }
}
