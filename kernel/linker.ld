/* Enlazado físico del kernel de Meridian.
 *
 * La base debe coincidir con KERNEL_LOAD_BASE de la crate meridian-bootinfo.
 * El bootloader reserva exactamente este rango con AllocatePages antes de
 * copiar los segmentos PT_LOAD del ELF.
 */
ENTRY(_start)

SECTIONS
{
    . = 0x02000000;

    /* Los símbolos delimitan páginas completas: el mapper de A4 los usa para
     * aplicar permisos RX (texto) y RO/RW+NX (datos) sin depender del ELF. */
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

    __kernel_end = .;

    /DISCARD/ :
    {
        *(.eh_frame)
        *(.eh_frame_hdr)
        *(.comment)
        *(.note*)
    }
}
