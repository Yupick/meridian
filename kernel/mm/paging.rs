//! Paginación x86-64 de 4 niveles y espacio virtual del kernel (A4).
//!
//! # Layout
//!
//! Se mantiene un mapeo identidad (`virtual == físico`) para el espacio que el
//! kernel usa hoy, porque el ELF está enlazado en `KERNEL_LOAD_BASE` y la
//! consola escribe en el framebuffer físico entregado por GOP. La tabla raíz
//! tiene una sola entrada PML4 (índice 0, `0..512 GiB`); los índices 0..3 del
//! PDPT cubren `0..4 GiB` con páginas grandes de 2 MiB:
//!
//! ```text
//! 0x0000_0000..0x0010_0000   zona baja, RW+NX (no se usa)
//! 0x0200_0000..(...+kernel) imagen del kernel: .text RX, .rodata RO+NX,
//!                                            .data/.bss RW+NX
//! BootInfo + buffer del mapa                 RO+NX (no se escriben)
//! framebuffer GOP y todo lo demás            RW+NX
//! 0x8000_0000 (GOP típico)                    RW+NX, nunca ejecutable
//! ```
//!
//! Las regiones con permisos especiales se re-mapeado con páginas de 4 KiB,
//! dividiendo la entrada de 2 MiB que las contiene. No existe ningún alias
//! virtual: cada dirección física tiene una sola traducción en el PML4, lo que
//! hace comprobable la propiedad W^X recorriendo las propias tablas.
//!
//! # Activación segura
//!
//! 1. Se habilita `EFER.NXE` antes de usar el bit 63 (NX) en las entradas.
//! 2. Se pone `CR4.PGE = 0` para que cargar CR3 invalide también las entradas
//!    globales que pudo dejar UEFI (evita traducciones RWX en TLB).
//! 3. Se activa `CR0.WP` para que las páginas solo-lectura también lo sean para
//!    el propio kernel.
//! 4. Se carga CR3 con la nueva raíz. El código sigue ejecutándose en su
//!    dirección física porque el mapeo identidad ya está construido.

use core::fmt;
use core::sync::atomic::{AtomicU64, Ordering};

use meridian_bootinfo::{BootInfo, PAGE_SIZE};

use super::buddy;
use super::{bss_range, data_range, rodata_range, text_range, Reservations};

const ENTRIES: usize = 512;
const PRESENT: u64 = 1 << 0;
const WRITABLE: u64 = 1 << 1;
const HUGE_PAGE: u64 = 1 << 7;
const NO_EXECUTE: u64 = 1u64 << 63;
const ADDR_MASK: u64 = 0x000f_ffff_ffff_f000;

const EFER_MSR: u32 = 0xC000_0080;
const EFER_NXE: u64 = 1 << 11;
const CR4_PGE: u64 = 1 << 7;
const CR0_WP: u64 = 1 << 16;

/// Raíz cargada en CR3. Cero significa "todavía no inicializado".
static ROOT: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Copy, Debug)]
pub enum PagingError {
    AlreadyInitialized,
    NotInitialized,
    NoMemory,
    NotMapped,
}

impl fmt::Display for PagingError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AlreadyInitialized => write!(f, "paginación ya inicializada"),
            Self::NotInitialized => write!(f, "paginación no inicializada"),
            Self::NoMemory => write!(f, "sin páginas libres para tablas"),
            Self::NotMapped => write!(f, "rango fuera del mapeo identidad 0..4 GiB"),
        }
    }
}

/// Permisos de una región mapeada. `executable == false` implica NX=1.
#[derive(Clone, Copy, Debug)]
struct Flags {
    writable: bool,
    executable: bool,
}

/// Resultado de la auditoría W^X sobre las tablas activas.
#[derive(Clone, Copy, Debug, Default)]
pub struct Audit {
    /// Páginas de 4 KiB equivalentes presentes en el mapeo.
    pub mapped_pages: u64,
    /// Páginas con NX=0 (potencialmente ejecutables).
    pub executable_pages: u64,
    /// Páginas con RW=1 y NX=1.
    pub writable_non_executable_pages: u64,
    /// Páginas con RW=1 y NX=0: debe ser siempre cero.
    pub writable_executable_pages: u64,
    pub nxe_enabled: bool,
    pub write_protect_enabled: bool,
    pub first_violation: Option<Violation>,
}

#[derive(Clone, Copy, Debug)]
pub struct Violation {
    pub level: &'static str,
    pub index: usize,
    pub address: u64,
    pub flags: u64,
}

impl fmt::Display for Violation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}[{}] @ 0x{:016X} flags=0x{:016X}",
            self.level, self.index, self.address, self.flags
        )
    }
}

/// Construye las tablas propias, aplica permisos y activa CR3.
pub fn init_and_switch(
    info: &BootInfo,
    _reserved: &Reservations,
    boot_info_addr: u64,
) -> Result<u64, PagingError> {
    if ROOT.load(Ordering::Relaxed) != 0 {
        return Err(PagingError::AlreadyInitialized);
    }

    let pml4 = alloc_zero_page()?;
    let pdpt = alloc_zero_page()?;
    let mut pds = [0u64; 4];
    for pd in &mut pds {
        *pd = alloc_zero_page()?;
    }

    // PML4[0] -> PDPT[0..4] -> 4 PD, cada una con 512 entradas de 2 MiB.
    write_entry(pml4, 0, pdpt | PRESENT | WRITABLE);
    for (index, pd) in pds.iter().enumerate() {
        write_entry(pdpt, index, *pd | PRESENT | WRITABLE);
    }
    for (index, pd) in pds.iter().enumerate() {
        for entry in 0..ENTRIES {
            let physical = (((index * ENTRIES) + entry) as u64) << 21;
            write_entry(
                *pd,
                entry,
                physical | PRESENT | WRITABLE | HUGE_PAGE | NO_EXECUTE,
            );
        }
    }

    // Publicamos la raíz antes de `map_range`, que la recorre para localizar la
    // PD de cada rango. Todavía no está cargada en CR3.
    ROOT.store(pml4, Ordering::Relaxed);

    // Texto: ejecutable y solo lectura. El resto: NX.
    let (text_start, text_end) = text_range();
    let (rodata_start, rodata_end) = rodata_range();
    let (data_start, data_end) = data_range();
    let (bss_start, bss_end) = bss_range();

    map_range(
        text_start,
        text_end,
        Flags {
            writable: false,
            executable: true,
        },
    )?;
    map_range(
        rodata_start,
        rodata_end,
        Flags {
            writable: false,
            executable: false,
        },
    )?;
    map_range(
        data_start,
        data_end,
        Flags {
            writable: true,
            executable: false,
        },
    )?;
    map_range(
        bss_start,
        bss_end,
        Flags {
            writable: true,
            executable: false,
        },
    )?;

    // Preventivas: ya están cubiertas por el mapeo por defecto, pero se
    // explicita que el buffer de arranque y el framebuffer no son ejecutables.
    let map = &info.memory_map;
    if map.buffer != 0 {
        map_range(
            map.buffer,
            map.buffer.saturating_add(map.size),
            Flags {
                writable: false,
                executable: false,
            },
        )?;
    }
    let boot_info_page = boot_info_addr & !(PAGE_SIZE - 1);
    map_range(
        boot_info_page,
        boot_info_page + PAGE_SIZE,
        Flags {
            writable: false,
            executable: false,
        },
    )?;
    let fb = &info.framebuffer;
    if fb.base != 0 && fb.size != 0 {
        map_range(
            fb.base,
            fb.base.saturating_add(fb.size),
            Flags {
                writable: true,
                executable: false,
            },
        )?;
    }

    enable_no_execute();
    clear_global_pages();
    load_cr3(pml4);
    // WP se activa después del cambio de CR3 para que la protección de
    // solo-lectura se aplique únicamente a nuestras tablas, no a las de UEFI.
    enable_write_protect();
    core::sync::atomic::fence(Ordering::SeqCst);

    Ok(pml4)
}

/// Cambia los permisos de todas las páginas de `[start, end)`.
fn map_range(start: u64, end: u64, flags: Flags) -> Result<(), PagingError> {
    if start >= end {
        return Ok(()); // sección vacía: nada que mapear
    }
    if end > 4 * 1024 * 1024 * 1024 {
        return Err(PagingError::NotMapped);
    }

    let mut address = start & !(PAGE_SIZE - 1);
    let end = align_up(end);
    while address < end {
        let pml4_index = (address >> 39) as usize & 511;
        let pdpt_index = (address >> 30) as usize & 511;
        let pd_index = (address >> 21) as usize & 511;
        let pt_index = (address >> 12) as usize & 511;

        let root = ROOT.load(Ordering::Relaxed);
        let pml4_entry = read_entry(root, pml4_index);
        if pml4_entry & PRESENT == 0 {
            return Err(PagingError::NotMapped);
        }
        let pdpt_physical = pml4_entry & ADDR_MASK;
        let pdpt_entry = read_entry(pdpt_physical, pdpt_index);
        if pdpt_entry & PRESENT == 0 {
            return Err(PagingError::NotMapped);
        }
        let pd_physical = pdpt_entry & ADDR_MASK;
        let pd_entry = read_entry(pd_physical, pd_index);
        if pd_entry & PRESENT == 0 {
            return Err(PagingError::NotMapped);
        }

        let pt_physical = if pd_entry & HUGE_PAGE != 0 {
            // Divide la entrada de 2 MiB en 512 entradas de 4 KiB. El mapeo
            // por defecto de esas páginas sigue siendo RW+NX.
            let table = alloc_zero_page()?;
            let base_2mib = address & !((1u64 << 21) - 1);
            for index in 0..ENTRIES {
                let physical = base_2mib + (index as u64) * PAGE_SIZE;
                write_entry(table, index, physical | PRESENT | WRITABLE | NO_EXECUTE);
            }
            // Las entradas intermedias no tienen NX: el NX efectivo se decide
            // en la entrada final.
            write_entry(pd_physical, pd_index, table | PRESENT | WRITABLE);
            table
        } else {
            pd_entry & ADDR_MASK
        };

        let mut entry = address | PRESENT;
        if flags.writable {
            entry |= WRITABLE;
        }
        if !flags.executable {
            entry |= NO_EXECUTE;
        }
        write_entry(pt_physical, pt_index, entry);
        address += PAGE_SIZE;
    }
    Ok(())
}

/// Recorre las tablas activas y comprueba que ninguna traducción sea a la vez
/// escribible y ejecutable.
pub fn audit() -> Result<Audit, PagingError> {
    let root = ROOT.load(Ordering::Relaxed);
    if root == 0 {
        return Err(PagingError::NotInitialized);
    }

    let mut audit = Audit {
        nxe_enabled: read_msr(EFER_MSR) & EFER_NXE != 0,
        write_protect_enabled: read_cr0() & CR0_WP != 0,
        ..Audit::default()
    };

    for pml4_index in 0..ENTRIES {
        let pml4_entry = read_entry(root, pml4_index);
        if pml4_entry & PRESENT == 0 {
            continue;
        }
        for pdpt_index in 0..ENTRIES {
            let pdpt_entry = read_entry(pml4_entry & ADDR_MASK, pdpt_index);
            if pdpt_entry & PRESENT == 0 {
                continue;
            }
            for pd_index in 0..ENTRIES {
                let pd_entry = read_entry(pdpt_entry & ADDR_MASK, pd_index);
                if pd_entry & PRESENT == 0 {
                    continue;
                }
                if pd_entry & HUGE_PAGE != 0 {
                    let address = page_table_address(pml4_index, pdpt_index, pd_index, 0);
                    audit.check_leaf(pd_entry, 512, "PD-2MiB", pd_index, address);
                    continue;
                }
                for pt_index in 0..ENTRIES {
                    let pt_entry = read_entry(pd_entry & ADDR_MASK, pt_index);
                    if pt_entry & PRESENT == 0 {
                        continue;
                    }
                    let address = page_table_address(pml4_index, pdpt_index, pd_index, pt_index);
                    audit.check_leaf(pt_entry, 1, "PT-4KiB", pt_index, address);
                }
            }
        }
    }
    Ok(audit)
}

impl Audit {
    fn check_leaf(
        &mut self,
        entry: u64,
        pages: u64,
        level: &'static str,
        index: usize,
        address: u64,
    ) {
        self.mapped_pages += pages;
        let writable = entry & WRITABLE != 0;
        let executable = entry & NO_EXECUTE == 0;
        if executable {
            self.executable_pages += pages;
        }
        if writable {
            if executable {
                self.writable_executable_pages += pages;
                if self.first_violation.is_none() {
                    self.first_violation = Some(Violation {
                        level,
                        index,
                        address,
                        flags: entry,
                    });
                }
            } else {
                self.writable_non_executable_pages += pages;
            }
        }
    }
}

/// Dirección lineal correspondiente a un índice de tabla dado (solo mapeo
/// identidad; no se usa para acceder, solo para el informe).
fn page_table_address(pml4: usize, pdpt: usize, pd: usize, pt: usize) -> u64 {
    ((pml4 as u64) << 39) | ((pdpt as u64) << 30) | ((pd as u64) << 21) | ((pt as u64) << 12)
}

/// Reserva una página física, la limpia y devuelve su dirección.
fn alloc_zero_page() -> Result<u64, PagingError> {
    let physical = buddy::alloc(0).ok_or(PagingError::NoMemory)?;
    // SAFETY: el buddy acaba de reservar la página y el mapeo identidad (el de
    // UEFI antes del switch y el nuestro después) permite escribirla.
    unsafe {
        core::ptr::write_bytes(physical as *mut u8, 0, PAGE_SIZE as usize);
    }
    Ok(physical)
}

#[inline]
fn read_entry(table: u64, index: usize) -> u64 {
    // SAFETY: `table` es una página de 4 KiB propiedad del kernel e `index < 512`.
    unsafe { core::ptr::read_volatile((table as *const u64).add(index)) }
}

#[inline]
fn write_entry(table: u64, index: usize, value: u64) {
    // SAFETY: ver [`read_entry`].
    unsafe { core::ptr::write_volatile((table as *mut u64).add(index), value) }
}

const fn align_up(value: u64) -> u64 {
    (value + PAGE_SIZE - 1) & !(PAGE_SIZE - 1)
}

fn enable_no_execute() {
    let efer = read_msr(EFER_MSR);
    write_msr(EFER_MSR, efer | EFER_NXE);
}

fn clear_global_pages() {
    write_cr4(read_cr4() & !CR4_PGE);
}

fn enable_write_protect() {
    write_cr0(read_cr0() | CR0_WP);
}

fn load_cr3(physical: u64) {
    // Sin `nomem`: el cambio de tablas debe observarse después de las
    // escrituras anteriores a las tablas y antes del código siguiente.
    // SAFETY: carga una raíz de 4 KiB alineada construida por nosotros.
    unsafe {
        core::arch::asm!("mov cr3, {}", in(reg) physical, options(nostack));
    }
}

fn read_cr0() -> u64 {
    let value: u64;
    // SAFETY: lectura de un registro de control; sin efectos en memoria.
    unsafe {
        core::arch::asm!("mov {}, cr0", out(reg) value, options(nostack));
    }
    value
}

fn write_cr0(value: u64) {
    // SAFETY: escritura de un registro de control con bits ya válidos.
    unsafe {
        core::arch::asm!("mov cr0, {}", in(reg) value, options(nostack));
    }
}

fn read_cr4() -> u64 {
    let value: u64;
    // SAFETY: lectura de un registro de control; sin efectos en memoria.
    unsafe {
        core::arch::asm!("mov {}, cr4", out(reg) value, options(nostack));
    }
    value
}

fn write_cr4(value: u64) {
    // SAFETY: escritura de un registro de control con bits ya válidos.
    unsafe {
        core::arch::asm!("mov cr4, {}", in(reg) value, options(nostack));
    }
}

fn read_msr(msr: u32) -> u64 {
    let low: u32;
    let high: u32;
    // SAFETY: `rdmsr` con un MSR fijo y válido (EFER en x86-64).
    unsafe {
        core::arch::asm!(
            "rdmsr",
            in("ecx") msr,
            out("eax") low,
            out("edx") high,
            options(nostack)
        );
    }
    ((high as u64) << 32) | low as u64
}

fn write_msr(msr: u32, value: u64) {
    let low = value as u32;
    let high = (value >> 32) as u32;
    // SAFETY: `wrmsr` con EFER y un valor que solo activa el bit NXE.
    unsafe {
        core::arch::asm!(
            "wrmsr",
            in("ecx") msr,
            in("eax") low,
            in("edx") high,
            options(nostack)
        );
    }
}
