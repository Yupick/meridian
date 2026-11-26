/* Enlazado físico del kernel aarch64 de Meridian.
 *
 * La base debe coincidir con KERNEL_LOAD_BASE de meridian-bootinfo. El
 * bootloader UEFI reserva este rango y copia aquí los segmentos PT_LOAD.
 */
ENTRY(_start)

SECTIONS
{
    . = 0x40100000;

    .text : ALIGN(4096)
    {
        __text_start = .;
        KEEP(*(.text.entry))
        *(.text .text.*)
        . = ALIGN(4096);
        __text_end = .;
    }

    .rodata : ALIGN(4096)
    {
        __rodata_start = .;
        *(.rodata .rodata.*)
        . = ALIGN(4096);
        __rodata_end = .;
    }

    .data : ALIGN(4096)
    {
        __data_start = .;
        *(.data .data.*)
        *(.got .got.*)
        . = ALIGN(4096);
        __data_end = .;
    }

    .bss : ALIGN(4096)
    {
        __bss_start = .;
        *(.bss .bss.*)
        *(COMMON)
        . = ALIGN(4096);
        __bss_end = .;
    }

    . = ALIGN(16);
    . += 0x10000;
    stack_top = .;

    __kernel_end = .;

    /DISCARD/ :
    {
        *(.eh_frame)
        *(.eh_frame_hdr)
        *(.comment)
        *(.note*)
    }
}
