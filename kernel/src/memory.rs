//! Consumo del mapa de memoria UEFI entregado en `BootInfo`.
//!
//! A3 no implementa todavía un asignador (eso corresponde a A4), pero sí
//! recorre y resume el mapa final para comprobar que la estructura recibida es
//! coherente y que el kernel puede usar los descriptores.

use meridian_bootinfo::{MemoryMapInfo, MemoryType, PAGE_SIZE};

/// Totales del mapa de memoria, expresados en páginas de 4 KiB.
#[derive(Clone, Copy, Debug, Default)]
pub struct MemorySummary {
    pub descriptors: usize,
    pub usable_pages: u64,
    pub loader_pages: u64,
    pub boot_services_pages: u64,
    pub runtime_pages: u64,
    pub acpi_pages: u64,
    pub mmio_pages: u64,
    pub reserved_pages: u64,
    pub other_pages: u64,
}

impl MemorySummary {
    /// Recorre el mapa y agrupa las páginas por categoría.
    pub fn from_map(map: &MemoryMapInfo) -> Self {
        let mut summary = Self {
            descriptors: map.entry_count(),
            ..Self::default()
        };

        for index in 0..summary.descriptors {
            // SAFETY: el contrato de arranque garantiza que `buffer` sigue
            // siendo legible durante toda la vida del kernel y que `size`
            // describe su longitud. `descriptor` valida índice y stride.
            let Some(descriptor) = (unsafe { map.descriptor(index) }) else {
                break;
            };
            let pages = descriptor.number_of_pages;
            match descriptor.kind() {
                Some(MemoryType::Conventional) => {
                    summary.usable_pages = summary.usable_pages.saturating_add(pages)
                }
                Some(MemoryType::LoaderCode) | Some(MemoryType::LoaderData) => {
                    summary.loader_pages = summary.loader_pages.saturating_add(pages)
                }
                Some(MemoryType::BootServicesCode) | Some(MemoryType::BootServicesData) => {
                    summary.boot_services_pages = summary.boot_services_pages.saturating_add(pages)
                }
                Some(MemoryType::RuntimeServicesCode) | Some(MemoryType::RuntimeServicesData) => {
                    summary.runtime_pages = summary.runtime_pages.saturating_add(pages)
                }
                Some(MemoryType::AcpiReclaim) | Some(MemoryType::AcpiNvs) => {
                    summary.acpi_pages = summary.acpi_pages.saturating_add(pages)
                }
                Some(MemoryType::MemoryMappedIo) | Some(MemoryType::MemoryMappedIoPortSpace) => {
                    summary.mmio_pages = summary.mmio_pages.saturating_add(pages)
                }
                Some(MemoryType::Reserved) | Some(MemoryType::Unusable) => {
                    summary.reserved_pages = summary.reserved_pages.saturating_add(pages)
                }
                Some(_) | None => summary.other_pages = summary.other_pages.saturating_add(pages),
            }
        }

        summary
    }

    pub fn usable_bytes(&self) -> u64 {
        self.usable_pages.saturating_mul(PAGE_SIZE)
    }

    pub fn usable_mib(&self) -> u64 {
        self.usable_bytes() / (1024 * 1024)
    }
}
