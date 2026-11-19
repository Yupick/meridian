//! Parseo de capabilities virtio-PCI modernas (T1.2).
//!
//! Se recorre la lista de capabilities desde el offset `0x34` del espacio de
//! configuración PCI. Se buscan `PCI_CAP_ID_VNDR` (`0x09`) con `cfg_type`
//! `COMMON_CFG=1`, `NOTIFY_CFG=2` y `DEVICE_CFG=4`. Todas las direcciones se
//! calculan como `bars[bar] + capability.offset` (BAR0 puede ser I/O en un
//! dispositivo transicional) y se usarán con el mapeo identidad.

use super::pci::{self, VirtioBlkDevice};

/// `PCI_CAP_ID_VNDR`.
const CAP_VENDOR: u8 = 0x09;
/// Common configuration (virtio_pci_common_cfg).
const CAP_TYPE_COMMON_CFG: u8 = 1;
/// Notification area.
const CAP_TYPE_NOTIFY_CFG: u8 = 2;
/// Device-specific configuration.
const CAP_TYPE_DEVICE_CFG: u8 = 4;

const CAP_HEADER_LEN: u16 = 16;
const MAX_CAPS: usize = 48;

/// Metadatos de una capability: BAR, offset dentro de la BAR, longitud y
/// dirección MMIO resultante (`BAR + offset`).
#[allow(dead_code)]
#[derive(Clone, Copy, Debug)]
pub struct VirtioCap {
    pub bar: u8,
    pub offset: u32,
    pub length: u32,
    pub base: u64,
}

/// Capabilities MMIO de un dispositivo virtio over PCI.
#[derive(Clone, Copy, Debug)]
pub struct VirtioPciCfg {
    /// `virtio_pci_common_cfg`.
    pub common: VirtioCap,
    /// Área de notificación (base, antes de aplicar el multiplicador).
    pub notify: VirtioCap,
    /// Configuración específica del dispositivo de bloque (`capacity` en 0).
    pub device_cfg: VirtioCap,
    /// `notify_off_multiplier` leído en el offset 16 de la capability NOTIFY.
    pub notify_multiplier: u32,
}

/// Parsea las capabilities y devuelve las direcciones MMIO necesarias.
pub fn parse(device: &VirtioBlkDevice) -> Result<VirtioPciCfg, &'static str> {
    let bdf = device.bdf;
    let mut next = (pci::pci_config_read(bdf.bus, bdf.slot, bdf.func, 0x34) & 0xFF) as u16;
    if next == 0 {
        return Err("virtio-pci capability list missing (modern device required)");
    }

    let mut common = None;
    let mut notify = None;
    let mut device_cfg = None;
    let mut common_error = "virtio-pci common cfg capability missing";
    let mut notify_error = "virtio-pci notify cfg capability missing";
    let mut device_cfg_error = "virtio-pci device cfg capability missing";
    let mut notify_multiplier = 0u32;
    let mut visited = 0usize;

    while next != 0 && visited < MAX_CAPS {
        // Las capabilities PCI están alineadas a dword y caben en 256 bytes.
        if next < 0x34 || next > 0xEC || next % 4 != 0 {
            return Err("malformed virtio-pci capability offset");
        }
        let header = pci::pci_config_read(bdf.bus, bdf.slot, bdf.func, next);
        let vendor = (header & 0xFF) as u8;
        let following = ((header >> 8) & 0xFF) as u16;
        let cap_len = ((header >> 16) & 0xFF) as u8;
        let cfg_type = ((header >> 24) & 0xFF) as u8;

        if vendor == CAP_VENDOR && cap_len >= CAP_HEADER_LEN as u8 {
            let second = pci::pci_config_read(bdf.bus, bdf.slot, bdf.func, next + 4);
            let bar = (second & 0xFF) as u8;
            let offset = pci::pci_config_read(bdf.bus, bdf.slot, bdf.func, next + 8);
            let length = pci::pci_config_read(bdf.bus, bdf.slot, bdf.func, next + 12);
            if bar as usize >= device.bars.len() {
                return Err("virtio-pci capability uses BAR >= 6");
            }
            let bar_base = device.bars[bar as usize];
            if bar_base == 0 {
                // p.ej. ISR/PCI_CFG_ACCESS con BAR0 de I/O: se ignora.
                match cfg_type {
                    CAP_TYPE_COMMON_CFG => {
                        common_error = "virtio-pci common cfg uses unassigned/IO BAR"
                    }
                    CAP_TYPE_NOTIFY_CFG => {
                        notify_error = "virtio-pci notify cfg uses unassigned/IO BAR"
                    }
                    CAP_TYPE_DEVICE_CFG => {
                        device_cfg_error = "virtio-pci device cfg uses unassigned/IO BAR"
                    }
                    _ => {}
                }
                next = following;
                visited += 1;
                continue;
            }
            let base = bar_base
                .checked_add(offset as u64)
                .ok_or("virtio-pci capability offset overflow")?;

            match cfg_type {
                CAP_TYPE_COMMON_CFG => {
                    // common cfg llega hasta `queue_device` en 48..56.
                    if length < 56 {
                        return Err("virtio-pci common cfg capability too short");
                    }
                    common = Some(VirtioCap {
                        bar,
                        offset,
                        length,
                        base,
                    });
                }
                CAP_TYPE_NOTIFY_CFG => {
                    if length < 20 {
                        return Err("virtio-pci notify cfg capability too short");
                    }
                    notify = Some(VirtioCap {
                        bar,
                        offset,
                        length,
                        base,
                    });
                    notify_multiplier =
                        pci::pci_config_read(bdf.bus, bdf.slot, bdf.func, next + 16);
                }
                CAP_TYPE_DEVICE_CFG => {
                    // virtio-blk expone `capacity` (le64) en el offset 0.
                    if length < 8 {
                        return Err("virtio-pci device cfg capability too short");
                    }
                    device_cfg = Some(VirtioCap {
                        bar,
                        offset,
                        length,
                        base,
                    });
                }
                _ => {}
            }
        }

        next = following;
        visited += 1;
    }

    Ok(VirtioPciCfg {
        common: common.ok_or(common_error)?,
        notify: notify.ok_or(notify_error)?,
        device_cfg: device_cfg.ok_or(device_cfg_error)?,
        notify_multiplier,
    })
}
