//! Configuración PCI por I/O ports (`0xCF8`/`0xCFC`) y descubrimiento de
//! dispositivos virtio-blk en el bus 0 (T1.2).
//!
//! El kernel arranca en contexto mono-CPU con interrupciones desactivadas y el
//! espacio `0..4 GiB` mapeado identidad, así que no hacen falta IDT/PIC/APIC
//! para sondear el bus: solo configuración PCI y MMIO.

use core::arch::asm;
use core::sync::atomic::{fence, Ordering};

/// Puerto de dirección del espacio de configuración PCI.
const CONFIG_ADDRESS: u16 = 0xCF8;
/// Puerto de datos del espacio de configuración PCI.
const CONFIG_DATA: u16 = 0xCFC;

/// Vendor ID de virtio.
pub const VENDOR_VIRTIO: u16 = 0x1AF4;
/// Device ID de virtio-blk transicional.
pub const DEVICE_VIRTIO_BLK_LEGACY: u16 = 0x1001;
/// Device ID de virtio-blk moderno.
pub const DEVICE_VIRTIO_BLK_MODERN: u16 = 0x1042;

/// I/O Space | Memory Space | Bus Master | Interrupt Disable.
const COMMAND_ENABLE: u32 = 0x0407;
const PCI_BAR_IO: u32 = 1 << 0;
const PCI_BAR_TYPE_MASK: u32 = 0b110;
const PCI_BAR_TYPE_64: u32 = 0b100;
const PCI_BAR_ADDR_MASK: u32 = !0xF;

/// Identificador bus/device/function de un dispositivo PCI.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Bdf {
    pub bus: u8,
    pub slot: u8,
    pub func: u8,
}

/// Dispositivo virtio-blk descubierto, con todas sus BARs decodificadas.
///
/// `bars[i]` contiene la dirección base MMIO de la BAR `i`, o `0` si la BAR es
/// de I/O o no está asignada. `io_base` guarda la BAR0 de I/O de los dispositivos
/// transicionales (`1af4:1001`), cuyo acceso legacy se usa en vez del MMIO alto.
#[derive(Clone, Copy, Debug)]
pub struct VirtioBlkDevice {
    pub bdf: Bdf,
    pub vendor: u16,
    pub device: u16,
    pub io_base: u16,
    pub bars: [u64; 6],
}

/// Lee una dword alineada del espacio de configuración PCI.
///
/// El offset se redondea hacia abajo a 4 bytes, como exige el acceso por
/// `0xCF8`/`0xCFC`. Las dwords devueltas se interpretan en little endian (x86).
#[inline]
pub fn pci_config_read(bus: u8, slot: u8, func: u8, offset: u16) -> u32 {
    let address = 0x8000_0000
        | ((bus as u32) << 16)
        | ((slot as u32) << 11)
        | ((func as u32) << 8)
        | ((offset as u32) & 0xFC);
    fence(Ordering::SeqCst);
    // SAFETY: `0xCF8`/`0xCFC` son los puertos estándar de configuración PCI en
    // x86; la dirección construida arriba es una dirección Type 1 válida.
    unsafe {
        asm!(
            "out dx, eax",
            in("dx") CONFIG_ADDRESS,
            in("eax") address,
            options(nostack, preserves_flags)
        );
    }
    let value: u32;
    // SAFETY: el puerto de datos acepta lecturas de 32 bits tras escribir CF8.
    unsafe {
        asm!(
            "in eax, dx",
            in("dx") CONFIG_DATA,
            out("eax") value,
            options(nostack, preserves_flags)
        );
    }
    fence(Ordering::SeqCst);
    value
}

/// Escribe una dword alineada en el espacio de configuración PCI.
#[inline]
pub fn pci_config_write(bus: u8, slot: u8, func: u8, offset: u16, value: u32) {
    let address = 0x8000_0000
        | ((bus as u32) << 16)
        | ((slot as u32) << 11)
        | ((func as u32) << 8)
        | ((offset as u32) & 0xFC);
    fence(Ordering::SeqCst);
    // SAFETY: ver [`pci_config_read`].
    unsafe {
        asm!(
            "out dx, eax",
            in("dx") CONFIG_ADDRESS,
            in("eax") address,
            options(nostack, preserves_flags)
        );
        asm!(
            "out dx, eax",
            in("dx") CONFIG_DATA,
            in("eax") value,
            options(nostack, preserves_flags)
        );
    }
    fence(Ordering::SeqCst);
}

#[inline]
fn read_bdf(bdf: Bdf, offset: u16) -> u32 {
    pci_config_read(bdf.bus, bdf.slot, bdf.func, offset)
}

#[inline]
fn write_bdf(bdf: Bdf, offset: u16, value: u32) {
    pci_config_write(bdf.bus, bdf.slot, bdf.func, offset, value);
}

/// Habilita MMIO, bus master y deshabilita IRQ legacy (INTx).
fn enable_device(bdf: Bdf) {
    let command = read_bdf(bdf, 0x04);
    write_bdf(bdf, 0x04, (command & 0xFFFF_0000) | COMMAND_ENABLE);
}

/// Decodifica las seis BARs de configuración PCI y mapea su MMIO.
///
/// Devuelve `(io_base, bars)`: `io_base` es la BAR0 de I/O (transicional) y
/// `bars[i]` las bases MMIO. Las BARs de I/O no se mapean; las de 64 bits
/// consumen dos dwords. Toda base MMIO útil se mapea identidad con
/// [`crate::mm::paging::identity_map_mmio`], incluidas las de más de 4 GiB,
/// solo cuando `map_mmio` es `true` (ruta moderna). Los dispositivos
/// transicionales usan I/O ports y no necesitan tocar el MMIO alto.
fn decode_bars(bdf: Bdf, map_mmio: bool) -> Result<(u16, [u64; 6]), &'static str> {
    const MMIO_MAP_LEN: u64 = 2 * 1024 * 1024;

    let bar0_raw = read_bdf(bdf, 0x10);
    let io_base = if bar0_raw & PCI_BAR_IO != 0 {
        (bar0_raw & !0x3) as u16
    } else {
        0
    };

    let map_base = |base: u64| -> Result<u64, &'static str> {
        if base == 0 {
            return Ok(0);
        }
        if !map_mmio {
            return Ok(base);
        }
        // La BAR puede empezar a mitad de una región de 2 MiB (p.ej.
        // 0x81012000); se pide solo el resto y el helper instala las páginas.
        let offset = base & (MMIO_MAP_LEN - 1);
        let len = MMIO_MAP_LEN - offset;
        crate::mm::paging::identity_map_mmio(base, len)
            .map_err(|_| "virtio-blk MMIO BAR could not be identity-mapped")?;
        Ok(base)
    };

    let mut bars = [0u64; 6];
    let mut index = 0usize;
    while index < bars.len() {
        let raw = read_bdf(bdf, 0x10 + (index as u16) * 4);
        if raw & PCI_BAR_IO != 0 {
            index += 1;
            continue;
        }

        let mut base = (raw & PCI_BAR_ADDR_MASK) as u64;
        if raw & PCI_BAR_TYPE_MASK == PCI_BAR_TYPE_64 {
            // Una BAR de 64 bits ocupa esta dword y la siguiente.
            if index + 1 < bars.len() {
                let high = read_bdf(bdf, 0x10 + ((index + 1) as u16) * 4) as u64;
                base |= high << 32;
            }
            bars[index] = map_base(base)?;
            if index + 1 < bars.len() {
                bars[index + 1] = 0;
            }
            index += 2;
            continue;
        }

        bars[index] = map_base(base)?;
        index += 1;
    }
    Ok((io_base, bars))
}

/// Comprueba si el BDF corresponde a un dispositivo virtio-blk.
fn is_virtio_blk(bdf: Bdf) -> Option<(u16, u16)> {
    let ids = read_bdf(bdf, 0x00);
    let vendor = (ids & 0xFFFF) as u16;
    let device = (ids >> 16) as u16;
    if vendor != VENDOR_VIRTIO {
        return None;
    }
    if device != DEVICE_VIRTIO_BLK_LEGACY && device != DEVICE_VIRTIO_BLK_MODERN {
        return None;
    }
    Some((vendor, device))
}

/// Escanea el bus 0 y devuelve hasta dos dispositivos virtio-blk.
///
/// El primero encontrado (menor slot/función) es el disco de arranque (id 0) y
/// el segundo el disco de QA (id 1). El orden de escaneo es determinista.
pub fn scan_virtio_blk() -> Result<([Option<VirtioBlkDevice>; 2], usize), &'static str> {
    let mut found = [None, None];
    let mut count = 0usize;

    for slot in 0u8..32 {
        if count == found.len() {
            break;
        }
        let base = Bdf {
            bus: 0,
            slot,
            func: 0,
        };
        let Some((vendor, device)) = is_virtio_blk(base) else {
            continue;
        };

        // Configurar antes de leer las BARs; las capabilities pueden estar en
        // cualquier BAR MMIO (BAR0 suele ser I/O en dispositivos transicionales).
        enable_device(base);
        let (io_base, bars) = decode_bars(base, device == DEVICE_VIRTIO_BLK_MODERN)?;
        found[count] = Some(VirtioBlkDevice {
            bdf: base,
            vendor,
            device,
            io_base,
            bars,
        });
        count += 1;

        // Si es multifunción, buscar también functions 1..7 del mismo slot.
        let header_type = (read_bdf(base, 0x0C) >> 16) as u8;
        if header_type & 0x80 == 0 {
            continue;
        }
        for func in 1u8..8 {
            if count == found.len() {
                break;
            }
            let bdf = Bdf { bus: 0, slot, func };
            let Some((vendor, device)) = is_virtio_blk(bdf) else {
                continue;
            };
            enable_device(bdf);
            let (io_base, bars) = decode_bars(bdf, device == DEVICE_VIRTIO_BLK_MODERN)?;
            found[count] = Some(VirtioBlkDevice {
                bdf,
                vendor,
                device,
                io_base,
                bars,
            });
            count += 1;
        }
    }

    Ok((found, count))
}
