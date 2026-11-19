//! Paginación x86-64 de 4 niveles y espacios de direcciones (A4 + A5).
//!
//! # Layout base (kernel)
//!
//! Se mantiene un mapeo identidad (`virtual == físico`) para el espacio que el
//! kernel usa, porque el ELF está enlazado en `KERNEL_LOAD_BASE` y la consola
//! escribe en el framebuffer físico entregado por GOP. La tabla raíz tiene una
//! sola entrada PML4 (índice 0, `0..512 GiB`); los índices 0..3 del PDPT cubren
//! `0..4 GiB` con páginas grandes de 2 MiB por defecto:
//!
//! - `.text`: RX (RO + ejecutable)
//! - `.rodata`, `BootInfo`, buffer del mapa: RO + NX
//! - `.data`/`.bss`, framebuffer y resto: RW + NX
//!
//! Las regiones con permisos especiales dividen la entrada de 2 MiB que las
//! contiene en 512 entradas de 4 KiB. No existen alias virtuales en el espacio
//! kernel inicial, lo que hace comprobable W^X recorriendo las tablas.
//!
//! T1.2 añade [`identity_map_mmio`], un mapeo identidad puntual de MMIO alto
//! (por ejemplo una BAR PCI a 32 GiB) mediante páginas de 4 KiB no cacheables.
//! Ese mapeo se crea **después** del selftest A4 y no se propaga a los
//! procesos: `create_address_space` sigue copiando solo las entradas PDPT `0..3`.
//!
//! # A5: espacios por proceso
//!
//! Cada proceso obtiene su propio PML4. Para mantener operativo el kernel (y el
//! manejo de excepciones/retorno controlado) se copian las tablas del mapeo de
//! kernel para `0..4 GiB` (subárbol bajo PDPT[0..3]), de modo que código/datos,
//! pila, estructuras de MM, consola/framebuffer y bootinfo siguen accesibles.
//! Las páginas privadas de prueba se mapean solo en el espacio del proceso y se
//! desmapean explícitamente del espacio del otro proceso, garantizando
//! aislamiento (el acceso provoca #PF controlado).

use core::fmt;
use core::sync::atomic::{AtomicU64, Ordering};

use meridian_bootinfo::{BootInfo, PAGE_SIZE};

use super::buddy;
use super::{bss_range, data_range, rodata_range, text_range, Reservations};

const ENTRIES: usize = 512;
const PRESENT: u64 = 1 << 0;
const WRITABLE: u64 = 1 << 1;
const USER: u64 = 1 << 2;
const PAGE_WRITE_THROUGH: u64 = 1 << 3;
const PAGE_CACHE_DISABLE: u64 = 1 << 4;
const HUGE_PAGE: u64 = 1 << 7;
const NO_EXECUTE: u64 = 1u64 << 63;
const ADDR_MASK: u64 = 0x000f_ffff_ffff_f000;
const HUGE_PAGE_SIZE: u64 = 2 * 1024 * 1024;

const EFER_MSR: u32 = 0xC000_0080;
const EFER_NXE: u64 = 1 << 11;
const CR4_PGE: u64 = 1 << 7;
const CR0_WP: u64 = 1 << 16;

/// Raíz del kernel cargada en CR3. Cero significa "todavía no inicializado".
static KERNEL_ROOT: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PagingError {
    AlreadyInitialized,
    NotInitialized,
    NoMemory,
    NotMapped,
    InvalidAddress,
}

impl fmt::Display for PagingError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AlreadyInitialized => write!(f, "paginación ya inicializada"),
            Self::NotInitialized => write!(f, "paginación no inicializada"),
            Self::NoMemory => write!(f, "sin páginas libres para tablas"),
            Self::NotMapped => write!(f, "rango fuera del mapeo"),
            Self::InvalidAddress => write!(f, "dirección no soportada"),
        }
    }
}

/// Permisos para mapeos explícitos.
#[derive(Clone, Copy, Debug)]
pub struct PageFlags {
    pub writable: bool,
    pub executable: bool,
    pub user: bool,
}

impl PageFlags {
    #[allow(dead_code)]
    const fn kernel_default() -> Self {
        Self {
            writable: true,
            executable: false,
            user: false,
        }
    }

    fn to_pte_bits(self) -> u64 {
        let mut bits = PRESENT;
        if self.writable {
            bits |= WRITABLE;
        }
        if self.user {
            bits |= USER;
        }
        if !self.executable {
            bits |= NO_EXECUTE;
        }
        bits
    }
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

/// Construye las tablas propias del kernel, aplica permisos y activa CR3.
pub fn init_and_switch(
    info: &BootInfo,
    _reserved: &Reservations,
    boot_info_addr: u64,
) -> Result<u64, PagingError> {
    if KERNEL_ROOT.load(Ordering::Relaxed) != 0 {
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

    // Publicamos la raíz antes de `map_range`, que la recorre.
    KERNEL_ROOT.store(pml4, Ordering::Relaxed);

    // Texto: ejecutable y solo lectura. El resto: NX.
    let (text_start, text_end) = text_range();
    let (rodata_start, rodata_end) = rodata_range();
    let (data_start, data_end) = data_range();
    let (bss_start, bss_end) = bss_range();

    set_range_flags(
        pml4,
        text_start,
        text_end,
        PageFlags {
            writable: false,
            executable: true,
            user: false,
        },
    )?;
    set_range_flags(
        pml4,
        rodata_start,
        rodata_end,
        PageFlags {
            writable: false,
            executable: false,
            user: false,
        },
    )?;
    set_range_flags(
        pml4,
        data_start,
        data_end,
        PageFlags {
            writable: true,
            executable: false,
            user: false,
        },
    )?;
    set_range_flags(
        pml4,
        bss_start,
        bss_end,
        PageFlags {
            writable: true,
            executable: false,
            user: false,
        },
    )?;

    // Preventivas: buffer de arranque y framebuffer no ejecutables.
    let map = &info.memory_map;
    if map.buffer != 0 {
        set_range_flags(
            pml4,
            map.buffer,
            map.buffer.saturating_add(map.size),
            PageFlags {
                writable: false,
                executable: false,
                user: false,
            },
        )?;
    }
    let boot_info_page = boot_info_addr & !(PAGE_SIZE - 1);
    set_range_flags(
        pml4,
        boot_info_page,
        boot_info_page + PAGE_SIZE,
        PageFlags {
            writable: false,
            executable: false,
            user: false,
        },
    )?;
    let fb = &info.framebuffer;
    if fb.base != 0 && fb.size != 0 {
        set_range_flags(
            pml4,
            fb.base,
            fb.base.saturating_add(fb.size),
            PageFlags {
                writable: true,
                executable: false,
                user: false,
            },
        )?;
    }

    enable_no_execute();
    clear_global_pages();
    load_cr3(pml4);
    enable_write_protect();
    core::sync::atomic::fence(Ordering::SeqCst);

    Ok(pml4)
}

/// Devuelve la raíz de tablas del kernel (PML4 físico).
pub fn kernel_root() -> u64 {
    KERNEL_ROOT.load(Ordering::Relaxed)
}

/// Mapea MMIO identidad (`virtual == físico`) en la raíz del kernel.
///
/// Se usa para BARs PCI por encima de 4 GiB: el rango `[base, base + len)` se
/// cubre con páginas de 4 KiB, creando/reutilizando PML4, PDPT, PD y PT. Cada
/// PTE nueva lleva `PWT=1` y `PCD=1` (PAT=0 → UC) para que los registros MMIO
/// no se cacheen con Write-Back. No modifica mapeos existentes: un PDE de 2 MiB
/// (caso A4 en direcciones < 4 GiB) o una PTE ya presente se respetan.
pub fn identity_map_mmio(base: u64, len: u64) -> Result<(), PagingError> {
    let root = kernel_root();
    if root == 0 {
        return Err(PagingError::NotInitialized);
    }
    if base == 0
        || len == 0
        || len > HUGE_PAGE_SIZE
        || base % PAGE_SIZE != 0
        || !is_canonical_low(base)
    {
        return Err(PagingError::InvalidAddress);
    }
    let end = base.checked_add(len).ok_or(PagingError::InvalidAddress)?;
    if !is_canonical_low(end) {
        return Err(PagingError::InvalidAddress);
    }
    let offset_in_huge = base & (HUGE_PAGE_SIZE - 1);
    if offset_in_huge + len > HUGE_PAGE_SIZE {
        return Err(PagingError::InvalidAddress);
    }
    let end = align_up(end);

    let mut address = base;
    while address < end {
        let indices = indices_for(address);

        let pml4e = read_entry(root, indices.pml4);
        let pdpt = if pml4e & PRESENT == 0 {
            let table = alloc_zero_page()?;
            write_entry(root, indices.pml4, table | PRESENT | WRITABLE);
            table
        } else if pml4e & HUGE_PAGE != 0 {
            return Err(PagingError::InvalidAddress);
        } else {
            pml4e & ADDR_MASK
        };

        let pdpte = read_entry(pdpt, indices.pdpt);
        let pd = if pdpte & PRESENT == 0 {
            let table = alloc_zero_page()?;
            write_entry(pdpt, indices.pdpt, table | PRESENT | WRITABLE);
            table
        } else if pdpte & HUGE_PAGE != 0 {
            return Err(PagingError::InvalidAddress);
        } else {
            pdpte & ADDR_MASK
        };

        let pde = read_entry(pd, indices.pd);
        if pde & HUGE_PAGE != 0 {
            // A4 ya cubre este rango con una página de 2 MiB: no se toca y se
            // avanza directamente al siguiente límite.
            address = (address & !(HUGE_PAGE_SIZE - 1)) + HUGE_PAGE_SIZE;
            continue;
        }

        let pt = if pde & PRESENT == 0 {
            let table = alloc_zero_page()?;
            write_entry(pd, indices.pd, table | PRESENT | WRITABLE);
            table
        } else {
            pde & ADDR_MASK
        };

        if read_entry(pt, indices.pt) & PRESENT == 0 {
            write_entry(
                pt,
                indices.pt,
                address | PRESENT | WRITABLE | PAGE_WRITE_THROUGH | PAGE_CACHE_DISABLE | NO_EXECUTE,
            );
        }
        address += PAGE_SIZE;
    }

    load_cr3(kernel_root());
    Ok(())
}

/// Carga una raíz en CR3.
pub fn switch(root: u64) {
    load_cr3(root);
}

/// Lee CR3 actual.
#[allow(dead_code)]
pub fn read_cr3() -> u64 {
    let value: u64;
    // SAFETY: lectura de registro de control.
    unsafe {
        core::arch::asm!("mov {}, cr3", out(reg) value, options(nomem, nostack, preserves_flags));
    }
    value
}

/// Crea un nuevo espacio de direcciones copiando el mapeo de kernel para
/// `0..4 GiB` (entradas PDPT 0..3 del PML4[0]). Las entradas altas quedan
/// vacías. Las tablas se copian (no se comparten) para permitir
/// mapeos/desmapeos por proceso sin afectar al kernel.
pub fn create_address_space() -> Result<u64, PagingError> {
    let kernel = kernel_root();
    if kernel == 0 {
        return Err(PagingError::NotInitialized);
    }
    let new_pml4 = alloc_zero_page()?;

    let pml4e = read_entry(kernel, 0);
    if pml4e & PRESENT == 0 {
        return Err(PagingError::NotMapped);
    }
    let kernel_pdpt = pml4e & ADDR_MASK;
    let new_pdpt = alloc_zero_page()?;
    // Copiar flags de la entrada PML4 (P/W, sin NX en niveles intermedios).
    write_entry(new_pml4, 0, new_pdpt | (pml4e & !ADDR_MASK));

    for pdpt_index in 0..4 {
        let pdpte = read_entry(kernel_pdpt, pdpt_index);
        if pdpte & PRESENT == 0 {
            continue;
        }
        if pdpte & HUGE_PAGE != 0 {
            // Inusual en este layout, pero soportado.
            write_entry(new_pdpt, pdpt_index, pdpte);
            continue;
        }
        let kernel_pd = pdpte & ADDR_MASK;
        let new_pd = alloc_zero_page()?;
        write_entry(new_pdpt, pdpt_index, new_pd | (pdpte & !ADDR_MASK));

        for pd_index in 0..ENTRIES {
            let pde = read_entry(kernel_pd, pd_index);
            if pde & PRESENT == 0 {
                continue;
            }
            if pde & HUGE_PAGE != 0 {
                write_entry(new_pd, pd_index, pde);
            } else {
                let kernel_pt = pde & ADDR_MASK;
                let new_pt = alloc_zero_page()?;
                // Copiar las 512 entradas de 4 KiB tal cual.
                for pt_index in 0..ENTRIES {
                    let pte = read_entry(kernel_pt, pt_index);
                    write_entry(new_pt, pt_index, pte);
                }
                write_entry(new_pd, pd_index, new_pt | (pde & !ADDR_MASK));
            }
        }
    }

    Ok(new_pml4)
}

/// Mapea una página de 4 KiB en un espacio de direcciones concreto.
pub fn map_page_in(root: u64, va: u64, pa: u64, flags: PageFlags) -> Result<(), PagingError> {
    if root == 0 {
        return Err(PagingError::NotInitialized);
    }
    if !is_canonical_low(va) || !is_canonical_low(pa) || va % PAGE_SIZE != 0 || pa % PAGE_SIZE != 0
    {
        return Err(PagingError::InvalidAddress);
    }

    let indices = indices_for(va);
    let pml4e = read_entry(root, indices.pml4);
    if pml4e & PRESENT == 0 {
        return Err(PagingError::NotMapped);
    }
    let pdpt = pml4e & ADDR_MASK;
    let pdpte = read_entry(pdpt, indices.pdpt);
    if pdpte & PRESENT == 0 {
        return Err(PagingError::NotMapped);
    }
    if pdpte & HUGE_PAGE != 0 {
        return Err(PagingError::InvalidAddress);
    }
    let pd = pdpte & ADDR_MASK;
    let pde = read_entry(pd, indices.pd);
    if pde & PRESENT == 0 {
        return Err(PagingError::NotMapped);
    }

    let pt = if pde & HUGE_PAGE != 0 {
        split_huge_pd_entry(pd, indices.pd, pde)?
    } else {
        pde & ADDR_MASK
    };

    write_entry(pt, indices.pt, pa | flags.to_pte_bits());
    invlpg(va);
    Ok(())
}

/// Desmapea una página de 4 KiB en un espacio de direcciones concreto.
/// Si la página no está mapeada, devuelve `Ok(())` (operación idempotente para
/// facilitar aislamiento defensivo).
pub fn unmap_page_in(root: u64, va: u64) -> Result<(), PagingError> {
    if root == 0 {
        return Err(PagingError::NotInitialized);
    }
    if !is_canonical_low(va) || va % PAGE_SIZE != 0 {
        return Err(PagingError::InvalidAddress);
    }
    let indices = indices_for(va);
    let pml4e = read_entry(root, indices.pml4);
    if pml4e & PRESENT == 0 {
        return Ok(());
    }
    let pdpt = pml4e & ADDR_MASK;
    let pdpte = read_entry(pdpt, indices.pdpt);
    if pdpte & PRESENT == 0 {
        return Ok(());
    }
    if pdpte & HUGE_PAGE != 0 {
        // No usamos huge pages a nivel PDPT en este diseño.
        return Err(PagingError::InvalidAddress);
    }
    let pd = pdpte & ADDR_MASK;
    let pde = read_entry(pd, indices.pd);
    if pde & PRESENT == 0 {
        return Ok(());
    }
    let pt = if pde & HUGE_PAGE != 0 {
        split_huge_pd_entry(pd, indices.pd, pde)?
    } else {
        pde & ADDR_MASK
    };
    let pte = read_entry(pt, indices.pt);
    if pte & PRESENT == 0 {
        return Ok(());
    }
    write_entry(pt, indices.pt, 0);
    invlpg(va);
    Ok(())
}

/// Cambia permisos de todas las páginas de `[start, end)` en `root`.
fn set_range_flags(root: u64, start: u64, end: u64, flags: PageFlags) -> Result<(), PagingError> {
    if start >= end {
        return Ok(());
    }
    if end > 4 * 1024 * 1024 * 1024 {
        return Err(PagingError::NotMapped);
    }

    let mut address = start & !(PAGE_SIZE - 1);
    let end = align_up(end);
    while address < end {
        set_page_flags(root, address, flags)?;
        address += PAGE_SIZE;
    }
    Ok(())
}

fn set_page_flags(root: u64, va: u64, flags: PageFlags) -> Result<(), PagingError> {
    let indices = indices_for(va);
    let pml4e = read_entry(root, indices.pml4);
    if pml4e & PRESENT == 0 {
        return Err(PagingError::NotMapped);
    }
    let pdpt = pml4e & ADDR_MASK;
    let pdpte = read_entry(pdpt, indices.pdpt);
    if pdpte & PRESENT == 0 {
        return Err(PagingError::NotMapped);
    }
    if pdpte & HUGE_PAGE != 0 {
        return Err(PagingError::InvalidAddress);
    }
    let pd = pdpte & ADDR_MASK;
    let pde = read_entry(pd, indices.pd);
    if pde & PRESENT == 0 {
        return Err(PagingError::NotMapped);
    }

    let pt = if pde & HUGE_PAGE != 0 {
        split_huge_pd_entry(pd, indices.pd, pde)?
    } else {
        pde & ADDR_MASK
    };

    write_entry(
        pt,
        indices.pt,
        (va & !(PAGE_SIZE - 1)) | flags.to_pte_bits(),
    );
    Ok(())
}

/// Divide una entrada PD de 2 MiB en una tabla PT de 4 KiB, preservando los
/// flags base (RW/NX/US) de la entrada huge en cada página.
fn split_huge_pd_entry(pd: u64, pd_index: usize, pde: u64) -> Result<u64, PagingError> {
    let table = alloc_zero_page()?;
    let base_2mib = pde & ADDR_MASK;
    let base_flags = (pde & (WRITABLE | USER | NO_EXECUTE)) | PRESENT;
    for index in 0..ENTRIES {
        let physical = base_2mib + (index as u64) * PAGE_SIZE;
        write_entry(table, index, physical | base_flags);
    }
    // La entrada PD apunta a la nueva PT; niveles intermedios no llevan NX.
    let pd_flags = PRESENT | WRITABLE | USER; // permisivo en intermedios
    write_entry(pd, pd_index, table | pd_flags);
    // Si estamos operando sobre el espacio activo, invalidar rango completo es
    // más seguro; invlpg por página se hará por el llamador cuando proceda.
    Ok(table)
}

/// Recorre las tablas activas (kernel/root actual en KERNEL_ROOT) y comprueba W^X.
pub fn audit() -> Result<Audit, PagingError> {
    let root = KERNEL_ROOT.load(Ordering::Relaxed);
    if root == 0 {
        return Err(PagingError::NotInitialized);
    }
    audit_root(root)
}

/// Auditoría de una raíz concreta.
pub fn audit_root(root: u64) -> Result<Audit, PagingError> {
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
            if pdpt_entry & HUGE_PAGE != 0 {
                let address = ((pml4_index as u64) << 39) | ((pdpt_index as u64) << 30);
                audit.check_leaf(pdpt_entry, 512 * 512, "PDPT-1GiB", pdpt_index, address);
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

#[derive(Clone, Copy)]
struct Indices {
    pml4: usize,
    pdpt: usize,
    pd: usize,
    pt: usize,
}

#[inline]
fn indices_for(va: u64) -> Indices {
    Indices {
        pml4: ((va >> 39) & 0x1FF) as usize,
        pdpt: ((va >> 30) & 0x1FF) as usize,
        pd: ((va >> 21) & 0x1FF) as usize,
        pt: ((va >> 12) & 0x1FF) as usize,
    }
}

fn page_table_address(pml4: usize, pdpt: usize, pd: usize, pt: usize) -> u64 {
    ((pml4 as u64) << 39) | ((pdpt as u64) << 30) | ((pd as u64) << 21) | ((pt as u64) << 12)
}

fn is_canonical_low(addr: u64) -> bool {
    // Solo usamos la mitad baja canónica (y en práctica 0..4GiB).
    addr < (1u64 << 47)
}

fn alloc_zero_page() -> Result<u64, PagingError> {
    let physical = buddy::alloc(0).ok_or(PagingError::NoMemory)?;
    // SAFETY: el buddy acaba de reservar la página y el mapeo identidad vigente
    // permite escribirla.
    unsafe {
        core::ptr::write_bytes(physical as *mut u8, 0, PAGE_SIZE as usize);
    }
    Ok(physical)
}

#[inline]
fn read_entry(table: u64, index: usize) -> u64 {
    // SAFETY: `table` es una página de tabla válida e `index < 512`.
    unsafe { core::ptr::read_volatile((table as *const u64).add(index)) }
}

#[inline]
fn write_entry(table: u64, index: usize, value: u64) {
    // SAFETY: ver [`read_entry`].
    unsafe { core::ptr::write_volatile((table as *mut u64).add(index), value) }
}

#[inline]
fn invlpg(va: u64) {
    // SAFETY: invalida TLB para una dirección; no tiene efectos de memoria.
    unsafe {
        core::arch::asm!("invlpg [{}]", in(reg) va, options(nostack, preserves_flags));
    }
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
    // SAFETY: carga una raíz válida.
    unsafe {
        core::arch::asm!("mov cr3, {}", in(reg) physical, options(nostack));
    }
}

fn read_cr0() -> u64 {
    let value: u64;
    unsafe {
        core::arch::asm!("mov {}, cr0", out(reg) value, options(nostack));
    }
    value
}

fn write_cr0(value: u64) {
    unsafe {
        core::arch::asm!("mov cr0, {}", in(reg) value, options(nostack));
    }
}

fn read_cr4() -> u64 {
    let value: u64;
    unsafe {
        core::arch::asm!("mov {}, cr4", out(reg) value, options(nostack));
    }
    value
}

fn write_cr4(value: u64) {
    unsafe {
        core::arch::asm!("mov cr4, {}", in(reg) value, options(nostack));
    }
}

fn read_msr(msr: u32) -> u64 {
    let low: u32;
    let high: u32;
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
