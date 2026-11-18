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

/// Tamaño de página usado por UEFI y por el kernel de Meridian.
pub const PAGE_SIZE: u64 = 4096;

/// Tipos de memoria UEFI (`EFI_MEMORY_TYPE`), con sus valores estables.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum MemoryType {
    Reserved = 0,
    LoaderCode = 1,
    LoaderData = 2,
    BootServicesCode = 3,
    BootServicesData = 4,
    RuntimeServicesCode = 5,
    RuntimeServicesData = 6,
    Conventional = 7,
    Unusable = 8,
    AcpiReclaim = 9,
    AcpiNvs = 10,
    MemoryMappedIo = 11,
    MemoryMappedIoPortSpace = 12,
    PalCode = 13,
    Persistent = 14,
}

impl MemoryType {
    /// Interpreta el campo `Type` de un descriptor UEFI.
    pub const fn from_raw(raw: u32) -> Option<Self> {
        match raw {
            0 => Some(Self::Reserved),
            1 => Some(Self::LoaderCode),
            2 => Some(Self::LoaderData),
            3 => Some(Self::BootServicesCode),
            4 => Some(Self::BootServicesData),
            5 => Some(Self::RuntimeServicesCode),
            6 => Some(Self::RuntimeServicesData),
            7 => Some(Self::Conventional),
            8 => Some(Self::Unusable),
            9 => Some(Self::AcpiReclaim),
            10 => Some(Self::AcpiNvs),
            11 => Some(Self::MemoryMappedIo),
            12 => Some(Self::MemoryMappedIoPortSpace),
            13 => Some(Self::PalCode),
            14 => Some(Self::Persistent),
            _ => None,
        }
    }

    /// Nombre corto y estable para diagnósticos.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Reserved => "reserved",
            Self::LoaderCode => "loader-code",
            Self::LoaderData => "loader-data",
            Self::BootServicesCode => "boot-code",
            Self::BootServicesData => "boot-data",
            Self::RuntimeServicesCode => "runtime-code",
            Self::RuntimeServicesData => "runtime-data",
            Self::Conventional => "usable",
            Self::Unusable => "unusable",
            Self::AcpiReclaim => "acpi-reclaim",
            Self::AcpiNvs => "acpi-nvs",
            Self::MemoryMappedIo => "mmio",
            Self::MemoryMappedIoPortSpace => "mmio-port",
            Self::PalCode => "pal-code",
            Self::Persistent => "persistent",
        }
    }
}

/// Descriptor UEFI de memoria (`EFI_MEMORY_DESCRIPTOR`).
///
/// El firmware puede reportar un `descriptor_size` mayor que
/// `size_of::<MemoryDescriptor>()`; por eso el mapa debe recorrerse con el
/// stride indicado y no como un array de esta estructura. Los campos que aquí
/// aparecen son los primeros 40 bytes del descriptor y conservan el orden de
/// la especificación UEFI.
#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct MemoryDescriptor {
    /// Tipo de memoria (`EFI_MEMORY_TYPE`).
    pub memory_type: u32,
    /// Relleno de alineación (no contiene información).
    pub reserved: u32,
    /// Dirección física del primer byte del rango.
    pub physical_start: u64,
    /// Dirección virtual reportada por el firmware.
    pub virtual_start: u64,
    /// Número de páginas de 4 KiB del rango.
    pub number_of_pages: u64,
    /// Atributos de memoria UEFI.
    pub attributes: u64,
}

impl MemoryDescriptor {
    /// Tamaño mínimo de un descriptor UEFI (sin extensiones del firmware).
    pub const MIN_SIZE: u64 = core::mem::size_of::<Self>() as u64;

    /// Tipo de memoria interpretado, si el valor es conocido.
    pub const fn kind(&self) -> Option<MemoryType> {
        MemoryType::from_raw(self.memory_type)
    }

    /// `true` para memoria convencional reutilizable por el kernel.
    pub const fn is_usable(&self) -> bool {
        self.memory_type == MemoryType::Conventional as u32
    }

    /// Tamaño del rango en bytes, saturando si el firmware reporta un valor enorme.
    pub const fn size_bytes(&self) -> u64 {
        self.number_of_pages.saturating_mul(PAGE_SIZE)
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

    /// Comprueba que la información del mapa es utilizable.
    pub const fn is_sane(&self) -> bool {
        self.buffer != 0
            && self.descriptor_size >= MemoryDescriptor::MIN_SIZE
            && self.size >= self.descriptor_size
            && self.entry_count() > 0
    }

    /// Copia el descriptor `index` del mapa.
    ///
    /// # Safety
    ///
    /// El llamador debe garantizar que `buffer` apunta a `size` bytes legibles
    /// y que siguen siendo válidos (el bootloader los conserva como memoria
    /// `LOADER_DATA`). El método comprueba los límites aritméticos antes de
    /// leer, pero no puede verificar la validez del puntero.
    pub unsafe fn descriptor(&self, index: usize) -> Option<MemoryDescriptor> {
        if !self.is_sane() || index >= self.entry_count() {
            return None;
        }
        let offset = (index as u64).checked_mul(self.descriptor_size)?;
        let end = offset.checked_add(MemoryDescriptor::MIN_SIZE)?;
        if end > self.size {
            return None;
        }
        let address = self.buffer.checked_add(offset)? as *const u8;
        let descriptor = address.cast::<MemoryDescriptor>();
        // SAFETY: el llamador garantiza que [buffer, buffer + size) es legible.
        // `read_unaligned` permite que descriptor_size no esté alineado.
        Some(unsafe { core::ptr::read_unaligned(descriptor) })
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
