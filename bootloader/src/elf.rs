//! Cargador ELF64 mínimo para la imagen del kernel.
//!
//! Admite ELF64 little-endian de x86-64 o aarch64 con segmentos `PT_LOAD`. La
//! imagen se enlaza en `KERNEL_LOAD_BASE` (ver `kernel*/linker.ld`), por lo que
//! el bootloader reserva ese rango físico exacto con `AllocatePages` y copia
//! cada segmento a su dirección de carga.

use core::fmt;

use meridian_bootinfo::KERNEL_LOAD_BASE;
use uefi::boot::{self, AllocateType, MemoryType};
use uefi::Status;

const ELF_HEADER_SIZE: usize = 64;
const PROGRAM_HEADER_SIZE: usize = 56;
const PT_LOAD: u32 = 1;
#[cfg(target_arch = "x86_64")]
const EM_X86_64: u16 = 62;
#[cfg(target_arch = "aarch64")]
const EM_AARCH64: u16 = 183;
const ET_EXEC: u16 = 2;

#[cfg(target_arch = "x86_64")]
const EXPECTED_MACHINE: u16 = EM_X86_64;
#[cfg(target_arch = "aarch64")]
const EXPECTED_MACHINE: u16 = EM_AARCH64;
const PAGE_SIZE: u64 = 4096;

/// Imagen del kernel ya cargada en memoria física.
#[derive(Clone, Copy, Debug)]
pub struct LoadedKernel {
    /// Base física de la imagen (alineada a página).
    pub base: u64,
    /// Punto de entrada (`e_entry`).
    pub entry: u64,
    /// Tamaño total del rango reservado, en bytes.
    pub size: u64,
}

/// Error de formato o de carga del ELF.
#[derive(Debug)]
pub enum ElfError {
    TooSmall,
    BadMagic,
    NotElf64,
    UnexpectedArchitecture(u16),
    NotExecutable,
    BadHeader,
    BadProgramHeader(usize),
    NoLoadSegments,
    TooLarge,
    EntryOutsideImage,
    UnexpectedLoadAddress(u64),
    Allocation(Status),
}

impl fmt::Display for ElfError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooSmall => write!(f, "la imagen es más pequeña que una cabecera ELF64"),
            Self::BadMagic => write!(f, "firma ELF inválida"),
            Self::NotElf64 => write!(f, "no es un ELF de 64 bits little-endian"),
            Self::UnexpectedArchitecture(machine) => {
                write!(f, "el ELF no es para la arquitectura esperada (e_machine={machine})")
            }
            Self::NotExecutable => write!(f, "el ELF no es de tipo ET_EXEC"),
            Self::BadHeader => write!(f, "cabecera ELF64 truncada"),
            Self::BadProgramHeader(i) => write!(f, "program header {i} inválido"),
            Self::NoLoadSegments => write!(f, "el ELF no contiene segmentos PT_LOAD"),
            Self::TooLarge => write!(f, "la imagen del kernel excede el rango soportado"),
            Self::EntryOutsideImage => write!(f, "el punto de entrada cae fuera de la imagen"),
            Self::UnexpectedLoadAddress(addr) => write!(
                f,
                "dirección de carga 0x{addr:016X} distinta de KERNEL_LOAD_BASE"
            ),
            Self::Allocation(status) => {
                write!(f, "no se pudo reservar memoria para el kernel: {status:?}")
            }
        }
    }
}

fn u16_at(data: &[u8], offset: usize) -> Option<u16> {
    let bytes = data.get(offset..offset + 2)?;
    Some(u16::from_le_bytes([bytes[0], bytes[1]]))
}

fn u64_at(data: &[u8], offset: usize) -> Option<u64> {
    let bytes = data.get(offset..offset + 8)?;
    Some(u64::from_le_bytes([
        bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
    ]))
}

/// Carga un kernel ELF64 en memoria física y devuelve su información de arranque.
///
/// # Safety
///
/// Reserva páginas `LOADER_DATA` en `KERNEL_LOAD_BASE` y escribe en ellas. Si el
/// firmware entrega esas páginas, la escritura es válida hasta que el kernel
/// decida reutilizarlas.
pub fn load(data: &[u8]) -> Result<LoadedKernel, ElfError> {
    if data.len() < ELF_HEADER_SIZE {
        return Err(ElfError::TooSmall);
    }
    if &data[0..4] != b"\x7fELF" {
        return Err(ElfError::BadMagic);
    }
    if data[4] != 2 || data[5] != 1 {
        return Err(ElfError::NotElf64);
    }

    let e_type = u16_at(data, 16).ok_or(ElfError::BadHeader)?;
    let e_machine = u16_at(data, 18).ok_or(ElfError::BadHeader)?;
    if e_machine != EXPECTED_MACHINE {
        return Err(ElfError::UnexpectedArchitecture(e_machine));
    }
    if e_type != ET_EXEC {
        return Err(ElfError::NotExecutable);
    }

    let entry = u64_at(data, 24).ok_or(ElfError::BadHeader)?;
    let phoff = u64_at(data, 32).ok_or(ElfError::BadHeader)?;
    let phentsize = u16_at(data, 54).ok_or(ElfError::BadHeader)? as usize;
    let phnum = u16_at(data, 56).ok_or(ElfError::BadHeader)? as usize;

    if phentsize < PROGRAM_HEADER_SIZE || phnum == 0 {
        return Err(ElfError::BadHeader);
    }

    let mut min_addr = u64::MAX;
    let mut max_addr = 0u64;
    let mut load_segments = 0usize;

    for index in 0..phnum {
        let base = phoff
            .checked_add(
                (index as u64)
                    .checked_mul(phentsize as u64)
                    .ok_or(ElfError::BadHeader)?,
            )
            .ok_or(ElfError::BadHeader)? as usize;
        let header = data
            .get(base..base + PROGRAM_HEADER_SIZE)
            .ok_or(ElfError::BadProgramHeader(index))?;

        let p_type = u32::from_le_bytes([header[0], header[1], header[2], header[3]]);
        if p_type != PT_LOAD {
            continue;
        }

        let p_offset = u64::from_le_bytes(header[8..16].try_into().unwrap());
        let p_paddr = u64::from_le_bytes(header[24..32].try_into().unwrap());
        let p_filesz = u64::from_le_bytes(header[32..40].try_into().unwrap());
        let p_memsz = u64::from_le_bytes(header[40..48].try_into().unwrap());

        if p_filesz > p_memsz {
            return Err(ElfError::BadProgramHeader(index));
        }
        let file_end = p_offset
            .checked_add(p_filesz)
            .ok_or(ElfError::BadProgramHeader(index))?;
        if file_end > data.len() as u64 {
            return Err(ElfError::BadProgramHeader(index));
        }
        let mem_end = p_paddr
            .checked_add(p_memsz)
            .ok_or(ElfError::BadProgramHeader(index))?;

        min_addr = min_addr.min(p_paddr);
        max_addr = max_addr.max(mem_end);
        load_segments += 1;
    }

    if load_segments == 0 {
        return Err(ElfError::NoLoadSegments);
    }

    let base = min_addr & !(PAGE_SIZE - 1);
    let end = max_addr
        .checked_add(PAGE_SIZE - 1)
        .ok_or(ElfError::TooLarge)?
        & !(PAGE_SIZE - 1);
    let size = end.checked_sub(base).ok_or(ElfError::TooLarge)?;

    if base != KERNEL_LOAD_BASE {
        return Err(ElfError::UnexpectedLoadAddress(base));
    }
    if size == 0 || size > 512 * 1024 * 1024 {
        return Err(ElfError::TooLarge);
    }
    if entry < base || entry >= end {
        return Err(ElfError::EntryOutsideImage);
    }

    let pages = (size / PAGE_SIZE) as usize;
    // `allocate_pages` devuelve páginas LOADER_DATA reservadas y alineadas a
    // 4 KiB. Después escribimos únicamente dentro de ese rango exacto.
    let allocated =
        boot::allocate_pages(AllocateType::Address(base), MemoryType::LOADER_DATA, pages)
            .map_err(|err| ElfError::Allocation(err.status()))?;
    debug_assert_eq!(allocated.as_ptr() as u64, base);

    // Limpia todo el rango reservado (incluye padding entre segmentos y BSS).
    // SAFETY: `allocated` apunta a `pages * PAGE_SIZE` bytes reservados.
    unsafe {
        core::ptr::write_bytes(allocated.as_ptr(), 0, pages * PAGE_SIZE as usize);
    }

    for index in 0..phnum {
        let base_offset = phoff as usize + index * phentsize;
        let header = &data[base_offset..base_offset + PROGRAM_HEADER_SIZE];
        let p_type = u32::from_le_bytes([header[0], header[1], header[2], header[3]]);
        if p_type != PT_LOAD {
            continue;
        }

        let p_offset = u64::from_le_bytes(header[8..16].try_into().unwrap()) as usize;
        let p_paddr = u64::from_le_bytes(header[24..32].try_into().unwrap());
        let p_filesz = u64::from_le_bytes(header[32..40].try_into().unwrap()) as usize;
        let p_memsz = u64::from_le_bytes(header[40..48].try_into().unwrap()) as usize;

        // SAFETY: el rango [p_paddr, p_paddr + p_memsz) está dentro de la
        // reserva verificada más arriba; el origen está dentro de `data`.
        unsafe {
            let dest = p_paddr as *mut u8;
            core::ptr::copy_nonoverlapping(data.as_ptr().add(p_offset), dest, p_filesz);
            core::ptr::write_bytes(dest.add(p_filesz), 0, p_memsz - p_filesz);
        }
    }

    Ok(LoadedKernel { base, entry, size })
}
