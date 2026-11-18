//! Asignador físico `buddy` para páginas de 4 KiB.
//!
//! El estado se construye recorriendo el mapa UEFI con el `descriptor_size`
//! real. Solo se añaden regiones `Conventional`; el resto permanece marcado
//! como `RESERVED`. Antes de añadir, se descartan las páginas que caen en
//! [`Range`] reservadas (kernel, `BootInfo`, mapa, framebuffer, pila y zona
//! baja), de modo que nunca formen parte de una lista libre.
//!
//! Cada página tiene una entrada de metadatos pequeña. Dentro de un bloque
//! libre o asignado todas sus páginas comparten `state` y `order`; la
//! primera página de un bloque libre además enlaza las listas doblemente
//! enlazadas de cada orden. El buddy de un bloque se calcula con
//! `base ^ (1 << order)`, y al liberar se fusiona mientras el buddy esté libre
//! con el mismo orden. Eso garantiza que el estado después de `free` sea
//! idéntico al de antes de `alloc` (no hay fugas y el coalescing es correcto).

use core::cell::UnsafeCell;
use core::fmt;

use meridian_bootinfo::{MemoryMapInfo, MemoryType, PAGE_SIZE};

use super::Range;

/// Orden máximo del buddy: `2^10` páginas = 4 MiB por bloque.
pub const MAX_ORDER: usize = 10;
/// Mayor PFN gestionado: `2^18` páginas = 1 GiB de memoria física.
pub const MAX_PAGES: usize = 1 << 18;

const NONE: u32 = u32::MAX;
const ST_FREE: u8 = 0;
const ST_ALLOCATED: u8 = 0xFE;
const ST_RESERVED: u8 = 0xFF;

/// Estado público de un bloque asignado (usado por las auto-pruebas).
pub const ALLOCATED_STATE: u8 = ST_ALLOCATED;

/// Metadatos de una página física. `prev`/`next` solo son válidos en la primera
/// página de un bloque libre.
#[derive(Clone, Copy, Debug)]
struct Meta {
    prev: u32,
    next: u32,
    state: u8,
    order: u8,
}

impl Meta {
    const RESERVED: Self = Self {
        prev: NONE,
        next: NONE,
        state: ST_RESERVED,
        order: 0,
    };
}

/// Contadores observados por las auto-pruebas.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Stats {
    pub free_pages: u64,
    pub allocated_pages: u64,
    pub skipped_pages: u64,
    pub blocks: [u64; MAX_ORDER + 1],
}

#[derive(Clone, Copy, Debug)]
pub enum InitError {
    AlreadyInitialized,
    BadMap,
}

impl fmt::Display for InitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AlreadyInitialized => write!(f, "buddy ya inicializado"),
            Self::BadMap => write!(f, "mapa de memoria UEFI no coherente"),
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub enum FreeError {
    NotInitialized,
    BadOrder,
    BadAlignment,
    OutOfRange,
    NotAllocated,
}

impl fmt::Display for FreeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotInitialized => write!(f, "buddy no inicializado"),
            Self::BadOrder => write!(f, "orden fuera de rango"),
            Self::BadAlignment => write!(f, "dirección no alineada al orden"),
            Self::OutOfRange => write!(f, "página fuera del buddy"),
            Self::NotAllocated => write!(f, "el bloque no estaba asignado (posible doble free)"),
        }
    }
}

/// Estado del asignador. Se usa a través de [`GlobalBuddy`].
pub struct Buddy {
    meta: [Meta; MAX_PAGES],
    heads: [u32; MAX_ORDER + 1],
    free_pages: u64,
    allocated_pages: u64,
    skipped_pages: u64,
    initialized: bool,
}

impl Buddy {
    const fn new() -> Self {
        Self {
            meta: [Meta::RESERVED; MAX_PAGES],
            heads: [NONE; MAX_ORDER + 1],
            free_pages: 0,
            allocated_pages: 0,
            skipped_pages: 0,
            initialized: false,
        }
    }

    /// Construye el buddy a partir del mapa final de UEFI.
    pub fn init(&mut self, map: &MemoryMapInfo, reserved: &[Range]) -> Result<Stats, InitError> {
        if self.initialized {
            return Err(InitError::AlreadyInitialized);
        }
        if !map.is_sane() {
            return Err(InitError::BadMap);
        }

        for index in 0..map.entry_count() {
            // SAFETY: el bootloader conserva el buffer del mapa como LOADER_DATA
            // legible; `descriptor` valida índice, stride y desbordamientos.
            let descriptor = unsafe { map.descriptor(index) }.ok_or(InitError::BadMap)?;
            if descriptor.kind() != Some(MemoryType::Conventional) {
                continue;
            }

            // Evita recorridos patológicos si el firmware reporta un valor
            // absurdo: el resto se contabiliza como omitido.
            let reported = descriptor.number_of_pages;
            let pages = reported.min(MAX_PAGES as u64);
            self.skipped_pages += reported - pages;

            for offset in 0..pages {
                let Some(addr) = descriptor
                    .physical_start
                    .checked_add(offset.saturating_mul(PAGE_SIZE))
                else {
                    self.skipped_pages += 1;
                    continue;
                };
                if addr % PAGE_SIZE != 0 {
                    self.skipped_pages += 1;
                    continue;
                }
                let pfn = (addr / PAGE_SIZE) as usize;
                if pfn >= MAX_PAGES {
                    self.skipped_pages += 1;
                    continue;
                }
                if reserved.iter().any(|range| range.contains_page(addr)) {
                    self.skipped_pages += 1;
                    continue;
                }
                self.free_block(pfn, 0);
            }
        }

        self.initialized = true;
        Ok(self.stats())
    }

    /// Reserva un bloque de `2^order` páginas y devuelve su dirección física.
    pub fn alloc(&mut self, order: usize) -> Option<u64> {
        if !self.initialized || order > MAX_ORDER {
            return None;
        }

        let mut current = order;
        while current <= MAX_ORDER && self.heads[current] == NONE {
            current += 1;
        }
        if current > MAX_ORDER {
            return None;
        }

        let mut page = self.pop_head(current);
        // Divide el bloque encontrado hasta el orden solicitado; la mitad
        // izquierda vuelve a la lista libre en cada paso.
        while current > order {
            current -= 1;
            self.push_head(page, current);
            page += 1usize << current;
        }

        self.mark(page, order, ST_ALLOCATED);
        let pages = 1u64 << order;
        self.free_pages -= pages;
        self.allocated_pages += pages;
        Some((page as u64) * PAGE_SIZE)
    }

    /// Libera un bloque previamente asignado, fusionándolo con sus buddies.
    pub fn free(&mut self, address: u64, order: usize) -> Result<(), FreeError> {
        if !self.initialized {
            return Err(FreeError::NotInitialized);
        }
        if order > MAX_ORDER {
            return Err(FreeError::BadOrder);
        }
        if address % PAGE_SIZE != 0 {
            return Err(FreeError::BadAlignment);
        }
        let pfn = (address / PAGE_SIZE) as usize;
        let pages = 1usize << order;
        if pfn >= MAX_PAGES || pfn.saturating_add(pages) > MAX_PAGES {
            return Err(FreeError::OutOfRange);
        }
        if pfn % pages != 0 {
            return Err(FreeError::BadAlignment);
        }
        for index in pfn..pfn + pages {
            let meta = self.meta[index];
            if meta.state != ST_ALLOCATED || meta.order as usize != order {
                return Err(FreeError::NotAllocated);
            }
        }

        self.free_block(pfn, order);
        self.allocated_pages -= pages as u64;
        Ok(())
    }

    /// Estado de una página: `(state, order)` o `None` si está fuera de rango.
    pub fn page_state(&self, address: u64) -> Option<(u8, usize)> {
        let pfn = (address / PAGE_SIZE) as usize;
        let meta = self.meta.get(pfn)?;
        Some((meta.state, meta.order as usize))
    }

    pub fn stats(&self) -> Stats {
        let mut blocks = [0u64; MAX_ORDER + 1];
        for order in 0..=MAX_ORDER {
            let mut current = self.heads[order];
            let mut visited = 0usize;
            while current != NONE && visited < MAX_PAGES {
                blocks[order] += 1;
                current = self.meta[current as usize].next;
                visited += 1;
            }
        }
        Stats {
            free_pages: self.free_pages,
            allocated_pages: self.allocated_pages,
            skipped_pages: self.skipped_pages,
            blocks,
        }
    }

    /// Inserta un bloque libre y absorbe todos los buddies libres posibles.
    fn free_block(&mut self, mut base: usize, mut order: usize) {
        let original = order;
        while order < MAX_ORDER {
            let buddy = base ^ (1usize << order);
            if buddy >= MAX_PAGES
                || self.meta[buddy].state != ST_FREE
                || self.meta[buddy].order as usize != order
            {
                break;
            }
            self.unlink(buddy, order);
            base = base.min(buddy);
            order += 1;
        }
        self.push_head(base, order);
        // El bloque final incluye a los buddies, que ya estaban contabilizados;
        // el incremento neto es solo el tamaño del bloque que se devuelve.
        self.free_pages += 1u64 << original;
    }

    /// Marca todas las páginas de un bloque con el mismo estado y orden.
    fn mark(&mut self, base: usize, order: usize, state: u8) {
        for index in base..base + (1usize << order) {
            self.meta[index].state = state;
            self.meta[index].order = order as u8;
        }
    }

    fn push_head(&mut self, page: usize, order: usize) {
        self.mark(page, order, ST_FREE);
        let head = self.heads[order];
        self.meta[page].prev = NONE;
        self.meta[page].next = head;
        if head != NONE {
            self.meta[head as usize].prev = page as u32;
        }
        self.heads[order] = page as u32;
    }

    fn unlink(&mut self, page: usize, order: usize) {
        let prev = self.meta[page].prev;
        let next = self.meta[page].next;
        if prev == NONE {
            self.heads[order] = next;
        } else {
            self.meta[prev as usize].next = next;
        }
        if next != NONE {
            self.meta[next as usize].prev = prev;
        }
        self.meta[page].prev = NONE;
        self.meta[page].next = NONE;
    }

    fn pop_head(&mut self, order: usize) -> usize {
        let head = self.heads[order] as usize;
        self.unlink(head, order);
        head
    }
}

/// Envoltoror global: el kernel es mono-CPU y no hay interrupciones, por lo que
/// todas las operaciones son secuenciales. `Sync` es necesario para poder
/// declarar el `static`; el único acceso es `with`/`with_mut`.
struct GlobalBuddy(UnsafeCell<Buddy>);

// SAFETY: mono-CPU, interrupciones deshabilitadas y ninguna función llama a
// otra del buddy mientras mantiene `&mut` (no hay reentrancia).
unsafe impl Sync for GlobalBuddy {}

static BUDDY: GlobalBuddy = GlobalBuddy(UnsafeCell::new(Buddy::new()));

fn with<R>(f: impl FnOnce(&Buddy) -> R) -> R {
    // SAFETY: ver el comentario de `GlobalBuddy`.
    unsafe { f(&*BUDDY.0.get()) }
}

fn with_mut<R>(f: impl FnOnce(&mut Buddy) -> R) -> R {
    // SAFETY: ver el comentario de `GlobalBuddy`.
    unsafe { f(&mut *BUDDY.0.get()) }
}

pub fn init(map: &MemoryMapInfo, reserved: &[Range]) -> Result<Stats, InitError> {
    with_mut(|buddy| buddy.init(map, reserved))
}

pub fn alloc(order: usize) -> Option<u64> {
    with_mut(|buddy| buddy.alloc(order))
}

pub fn free(address: u64, order: usize) -> Result<(), FreeError> {
    with_mut(|buddy| buddy.free(address, order))
}

pub fn page_state(address: u64) -> Option<(u8, usize)> {
    with(|buddy| buddy.page_state(address))
}

pub fn stats() -> Stats {
    with(|buddy| buddy.stats())
}
