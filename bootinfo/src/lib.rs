//! Contrato de arranque mínimo entre el bootloader UEFI y el kernel.
//!
//! Esta crate es `no_std` y solo contiene tipos `#[repr(C)]` con campos de
//! tamaño fijo. El bootloader construye una instancia en memoria que sigue
//! siendo válida después de `ExitBootServices()` y pasa un puntero a ella al
//! kernel (primer argumento, ABI `extern "C"`).
//!
//! El diseño deliberadamente pequeño de A2 permite que A3 amplíe la estructura
//! (por ejemplo, con más información de vídeo o regiones reservadas) sin
//! cambiar el mecanismo de entrega.

#![no_std]

/// Firma que identifica una estructura [`BootInfo`] válida.
///
/// Los bytes son `b"MERIDIAN"` interpretados como entero little-endian, de
/// modo que en memoria se lee exactamente la cadena.
pub const BOOTINFO_MAGIC: u64 = u64::from_le_bytes(*b"MERIDIAN");

/// Versión del contrato de arranque.
pub const BOOTINFO_VERSION: u32 = 1;

/// Dirección física base donde el bootloader carga el kernel.
///
/// Debe coincidir con `kernel/linker.ld`. Se eligió 32 MiB porque OVMF reserva
/// parte de la memoria baja y deja un hueco no convencional alrededor de 16 MiB.
pub const KERNEL_LOAD_BASE: u64 = 0x0200_0000;

/// Formatos de píxel de GOP, codificados de forma estable para el kernel.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum PixelFormat {
    /// R, G, B, reservado (32 bits).
    Rgb = 0,
    /// B, G, R, reservado (32 bits).
    Bgr = 1,
    /// Formato definido por las máscaras de [`FramebufferInfo`].
    Bitmask = 2,
    /// Solo se puede dibujar mediante `Blt`; no hay framebuffer mapeable.
    BltOnly = 3,
}

/// Información del framebuffer GOP vigente en el momento del handoff.
#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct FramebufferInfo {
    /// Dirección física del inicio del framebuffer (0 si no es mapeable).
    pub base: u64,
    /// Tamaño del framebuffer en bytes.
    pub size: u64,
    /// Anchura visible en píxeles.
    pub width: u64,
    /// Altura visible en píxeles.
    pub height: u64,
    /// Píxeles por línea de barrido (puede incluir padding).
    pub stride: u64,
    /// Formato de píxel, según [`PixelFormat`].
    pub pixel_format: u32,
    /// Reservado para futuras ampliaciones (debe ser 0).
    pub reserved: u32,
    /// Máscara del canal rojo (solo para [`PixelFormat::Bitmask`]).
    pub red_mask: u32,
    /// Máscara del canal verde (solo para [`PixelFormat::Bitmask`]).
    pub green_mask: u32,
    /// Máscara del canal azul (solo para [`PixelFormat::Bitmask`]).
    pub blue_mask: u32,
    /// Máscara del canal reservado (solo para [`PixelFormat::Bitmask`]).
    pub reserved_mask: u32,
}

impl FramebufferInfo {
    /// Devuelve `true` si el kernel puede escribir píxeles directamente.
    pub const fn is_directly_addressable(&self) -> bool {
        self.base != 0 && self.pixel_format != PixelFormat::BltOnly as u32
    }
}

/// Copia del mapa de memoria UEFI obtenido justo antes de `ExitBootServices()`.
///
/// `buffer` apunta a una zona de memoria `LOADER_DATA` propiedad del bootloader.
/// El kernel no debe liberarla; puede recorrerla usando `descriptor_size`, que
/// puede ser mayor que `size_of::<MemoryDescriptor>()` (la especificación UEFI
/// permite descriptores extendidos).
#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct MemoryMapInfo {
    /// Dirección física del primer descriptor.
    pub buffer: u64,
    /// Número total de bytes ocupados por los descriptores.
    pub size: u64,
    /// Tamaño en bytes de cada descriptor.
    pub descriptor_size: u64,
    /// Versión del formato de descriptor reportada por el firmware.
    pub descriptor_version: u32,
    /// Reservado para futuras ampliaciones (debe ser 0).
    pub reserved: u32,
}

impl MemoryMapInfo {
    /// Número de descriptores del mapa, o 0 si la información no es coherente.
    pub const fn entry_count(&self) -> usize {
        match self.size.checked_div(self.descriptor_size) {
            Some(count) => count as usize,
            None => 0,
        }
    }
}

/// Estructura de arranque entregada al kernel.
#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct BootInfo {
    /// Debe ser [`BOOTINFO_MAGIC`].
    pub magic: u64,
    /// Versión del contrato, actualmente [`BOOTINFO_VERSION`].
    pub version: u32,
    /// Tamaño de esta estructura en bytes (permite ampliarla en el futuro).
    pub size: u32,
    /// Mapa de memoria UEFI capturado antes de salir de boot services.
    pub memory_map: MemoryMapInfo,
    /// Framebuffer GOP activo.
    pub framebuffer: FramebufferInfo,
    /// Dirección física de la base de la imagen del kernel.
    pub kernel_base: u64,
    /// Punto de entrada del kernel (dirección física).
    pub kernel_entry: u64,
    /// Tamaño en bytes de la imagen del kernel cargada.
    pub kernel_size: u64,
}

impl BootInfo {
    /// Comprueba la firma y la versión de la estructura.
    pub const fn is_valid(&self) -> bool {
        self.magic == BOOTINFO_MAGIC && self.version == BOOTINFO_VERSION
    }
}
