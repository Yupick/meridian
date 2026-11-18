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

    .text : ALIGN(4096)
    {
        KEEP(*(.text.entry))
        *(.text .text.*)
    }

    .rodata : ALIGN(4096)
    {
        *(.rodata .rodata.*)
    }

    .data : ALIGN(4096)
    {
        *(.data .data.*)
        *(.got .got.*)
    }

    .bss : ALIGN(4096)
    {
        *(.bss .bss.*)
        *(COMMON)
    }

    /DISCARD/ :
    {
        *(.eh_frame)
        *(.eh_frame_hdr)
        *(.comment)
        *(.note*)
    }
}
