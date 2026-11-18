//! Gestión de memoria física y virtual del kernel de Meridian (tarea A4).
//!
//! Este módulo agrupa tres piezas independientes pero complementarias:
//!
//! * [`buddy`]: asignador físico de páginas/bloques construido a partir del
//!   mapa UEFI recibido en `BootInfo`.
//! * [`paging`]: tablas de página x86-64 de 4 niveles, mapeo identidad del
//!   espacio usado por el kernel y aplicación de W^X.
//! * [`selftest`]: comprobaciones ejecutadas en cada arranque que informan por
//!   COM1 con marcadores `A4: ... PASS/FAIL`.
//!
//! Todo el código es `no_std`. El acceso al estado global está confinado a
//! envoltorios con `unsafe` documentado: el kernel es mono-CPU y arranca con
//! interrupciones deshabilitadas, así que no hay reentrancias.

pub mod buddy;
pub mod paging;
pub mod selftest;

use meridian_bootinfo::{BootInfo, PAGE_SIZE};

// Símbolos definidos en `kernel/linker.ld`. Delimitan páginas completas, por
// lo que el mapper puede aplicar permisos por sección sin leer el ELF.
#[allow(non_upper_case_globals)]
extern "C" {
    static __text_start: u8;
    static __text_end: u8;
    static __rodata_start: u8;
    static __rodata_end: u8;
    static __data_start: u8;
    static __data_end: u8;
    static __bss_start: u8;
    static __bss_end: u8;
    static __kernel_end: u8;
}

/// Devuelve la dirección de un símbolo del linker sin leer su contenido.
fn symbol_address(symbol: &u8) -> u64 {
    symbol as *const u8 as u64
}

pub fn text_range() -> (u64, u64) {
    // SAFETY: los símbolos son absolutos y están inicializados por el enlazador.
    unsafe { (symbol_address(&__text_start), symbol_address(&__text_end)) }
}

pub fn rodata_range() -> (u64, u64) {
    // SAFETY: ver [`text_range`].
    unsafe { (symbol_address(&__rodata_start), symbol_address(&__rodata_end)) }
}

pub fn data_range() -> (u64, u64) {
    // SAFETY: ver [`text_range`].
    unsafe { (symbol_address(&__data_start), symbol_address(&__data_end)) }
}

pub fn bss_range() -> (u64, u64) {
    // SAFETY: ver [`text_range`].
    unsafe { (symbol_address(&__bss_start), symbol_address(&__bss_end)) }
}

fn linker_kernel_end() -> u64 {
    // SAFETY: ver [`text_range`].
    unsafe { symbol_address(&__kernel_end) }
}

/// Rango semiabierto `[start, end)` de direcciones físicas.
#[derive(Clone, Copy, Debug)]
pub struct Range {
    pub start: u64,
    pub end: u64,
}

impl Range {
    pub const fn new(start: u64, end: u64) -> Self {
        Self { start, end }
    }

    /// `true` si la página que contiene `addr` intersecta el rango.
    pub const fn contains_page(&self, addr: u64) -> bool {
        addr >= self.start && addr < self.end
    }

    pub const fn is_empty(&self) -> bool {
        self.start >= self.end
    }
}

/// Regiones que el asignador **nunca** debe reutilizar.
///
/// Incluye memoria que UEFI ya marca como no convencional (que simplemente no
/// se añade al buddy) y las regiones convencionales que deben quedar intactas:
/// imagen del kernel, `BootInfo`, buffer del mapa, framebuffer, zona baja y la
/// pila heredada del firmware.
pub struct Reservations {
    pub ranges: [Range; 6],
    pub count: usize,
}

impl Reservations {
    /// Construye la lista de reservas a partir del `BootInfo` y del valor de
    /// `RSP` en el momento del handoff.
    pub fn new(info: &BootInfo, boot_info_addr: u64, rsp: u64) -> Self {
        let mut ranges = [Range::new(0, 0); 6];
        let mut count = 0;

        let mut push = |range: Range| {
            if !range.is_empty() && count < ranges.len() {
                ranges[count] = range;
                count += 1;
            }
        };

        // 1. Zona baja: se conserva por convención (BIOS/VGA/EBDA) aunque OVMF
        //    la reporte como convencional.
        push(Range::new(0, 0x10_0000));

        // 2. Imagen completa del kernel, incluido .bss (donde vive la metadata
        //    del buddy). La bootloader ya reserva todo el rango PT_LOAD.
        let image_end = info
            .kernel_base
            .saturating_add(info.kernel_size)
            .max(linker_kernel_end());
        push(Range::new(
            info.kernel_base,
            align_up(image_end, PAGE_SIZE),
        ));

        // 3. La página de BootInfo: propiedad del bootloader.
        let boot_info_page = boot_info_addr & !(PAGE_SIZE - 1);
        push(Range::new(boot_info_page, boot_info_page + PAGE_SIZE));

        // 4. Buffer del mapa UEFI: también propiedad del bootloader.
        let map = &info.memory_map;
        if map.buffer != 0 {
            push(Range::new(
                map.buffer & !(PAGE_SIZE - 1),
                align_up(map.buffer.saturating_add(map.size), PAGE_SIZE),
            ));
        }

        // 5. Framebuffer GOP: aunque el firmware lo marque como convencional,
        //    la consola lo usa durante toda la vida del kernel.
        let fb = &info.framebuffer;
        if fb.base != 0 && fb.size != 0 {
            push(Range::new(
                fb.base & !(PAGE_SIZE - 1),
                align_up(fb.base.saturating_add(fb.size), PAGE_SIZE),
            ));
        }

        // 6. Pila heredada de UEFI: 64 KiB por debajo de RSP más la página
        //    actual. No se libera ni se puede reutilizar mientras `_start` siga
        //    ejecutándose sobre ella.
        let stack_low = rsp.saturating_sub(64 * 1024) & !(PAGE_SIZE - 1);
        let stack_high = rsp.saturating_add(PAGE_SIZE - 1) & !(PAGE_SIZE - 1);
        push(Range::new(stack_low, stack_high));

        Self { ranges, count }
    }
}

/// Redondea hacia arriba a múltiplo de `align` (debe ser potencia de dos).
pub const fn align_up(value: u64, align: u64) -> u64 {
    (value + align - 1) & !(align - 1)
}
