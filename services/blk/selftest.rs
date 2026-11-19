//! Auto-pruebas del servicio de bloques sobre virtio-blk (T1.2).
//!
//! Emite los marcadores `T1.2: ... PASS/FAIL` por COM1 (a través de `Console`,
//! igual que el resto de selftests). Ante cualquier error imprime un marcador
//! `FAIL` con el motivo para que QA no se quede esperando.

use core::fmt::Write;

use crate::cap::{CapError, CapTable, Capability, Rights};
use crate::console::Console;
use crate::drivers::virtio_blk::QUEUE_SIZE;

use super::{self as blk, BlockError, DeviceIds, SECTOR_SIZE, SESSION_BLOCK_SLOT};

const SLOT_RW: usize = SESSION_BLOCK_SLOT;
const SLOT_RO: usize = 3;

const VENDOR_VIRTIO: u16 = 0x1AF4;
const DEVICE_BLK_LEGACY: u16 = 0x1001;
const DEVICE_BLK_MODERN: u16 = 0x1042;
const BOOT_DISK: u8 = 0;
const QA_DISK: u8 = 1;

struct Failure {
    step: &'static str,
    message: &'static str,
}

fn failure(step: &'static str, message: &'static str) -> Failure {
    Failure { step, message }
}

/// Ejecuta la prueba T1.2 completa.
pub fn run(console: &mut Console) -> bool {
    match run_inner(console) {
        Ok(()) => true,
        Err(failure) => {
            let _ = writeln!(console, "T1.2: {} FAIL: {}", failure.step, failure.message);
            let _ = writeln!(
                console,
                "T1.2: block service tests FAIL: {}",
                failure.message
            );
            false
        }
    }
}

fn error_message(error: BlockError) -> &'static str {
    match error {
        BlockError::Capability(CapError::RightsDenied) => "capacidad sin WRITE",
        BlockError::Capability(_) => "capacidad inválida para el servicio de bloques",
        BlockError::WrongObject => "capacidad de otro objeto",
        BlockError::NoDevice(_) => "dispositivo no presente",
        BlockError::ReadOnlyDevice => "dispositivo de solo lectura",
        BlockError::InvalidSector => "sector fuera de rango",
        BlockError::InvalidBuffer => "búfer inválido",
        BlockError::QueueFull => "cola virtio llena",
        BlockError::Timeout => "timeout en petición virtio-blk",
        BlockError::IoStatus(_) => "estado de E/S distinto de OK",
        BlockError::UnexpectedDescriptor(_) => "descriptor devuelto inesperado",
        BlockError::Init(message) => message,
        BlockError::NotInitialized => "servicio no inicializado",
        BlockError::AlreadyInitialized => "servicio ya inicializado",
    }
}

fn run_inner(console: &mut Console) -> Result<(), Failure> {
    let stats = blk::stats().map_err(|e| failure("init", error_message(e)))?;
    let ids = blk::detected_device_ids();

    // 1. Detección de los dos dispositivos virtio-blk.
    if stats.devices != 2 {
        return Err(failure("virtio-blk detect", "se esperaban 2 dispositivos"));
    }
    let valid_id = |id: DeviceIds| {
        id.vendor == VENDOR_VIRTIO
            && (id.device == DEVICE_BLK_LEGACY || id.device == DEVICE_BLK_MODERN)
    };
    if !valid_id(ids[0]) || !valid_id(ids[1]) {
        return Err(failure(
            "virtio-blk detect",
            "IDs PCI distintos de 1af4:1001/1042",
        ));
    }
    let bdf0 = blk::device_bdf(BOOT_DISK).unwrap_or((0, 0, 0));
    let bdf1 = blk::device_bdf(QA_DISK).unwrap_or((0, 0, 0));
    let _ = writeln!(
        console,
        "T1.2: virtio-blk detect PASS (devices={} ids=1af4:1001/1042 found={:04x}:{:04x}@{:02x}:{:02x}.{}, {:04x}:{:04x}@{:02x}:{:02x}.{})",
        stats.devices,
        ids[0].vendor,
        ids[0].device,
        bdf0.0,
        bdf0.1,
        bdf0.2,
        ids[1].vendor,
        ids[1].device,
        bdf1.0,
        bdf1.1,
        bdf1.2
    );

    // 2. Configuración de la virtqueue (16 descriptores).
    if stats.queue_size != QUEUE_SIZE {
        return Err(failure("virtqueue setup", "tamaño de cola distinto de 16"));
    }
    let _ = writeln!(
        console,
        "T1.2: virtqueue setup PASS (queue_size={})",
        stats.queue_size
    );

    // Capacidad con RW para las pruebas de lectura y escritura.
    let mut caps = CapTable::new();
    caps.insert(
        SLOT_RW,
        Capability {
            object: blk::BLOCK_OBJECT_ID,
            rights: Rights::READ.union(Rights::WRITE),
        },
    )
    .map_err(|_| failure("capabilities", "no se pudo crear capacidad RW"))?;

    // 3. Lectura del LBA 1 del disco de arranque: cabecera GPT "EFI PART".
    if blk::sector_count(BOOT_DISK) <= 1 {
        return Err(failure(
            "sector read",
            "el disco de arranque tiene menos de 2 sectores",
        ));
    }
    let mut sector = [0u8; SECTOR_SIZE];
    blk::read_sector(&caps, SLOT_RW, BOOT_DISK, 1, &mut sector)
        .map_err(|e| failure("sector read", error_message(e)))?;
    if &sector[..8] != b"EFI PART" {
        return Err(failure(
            "sector read",
            "LBA 1 del disco 0 no contiene 'EFI PART'",
        ));
    }
    let _ = writeln!(console, "T1.2: sector read PASS (device=0, lba=1)");

    // 4. Escritura y lectura de los sectores 0 y 1 del disco QA.
    if blk::sector_count(QA_DISK) < 2 {
        return Err(failure(
            "write/readback",
            "el disco QA tiene menos de 2 sectores",
        ));
    }
    let mut pattern = [0u8; SECTOR_SIZE];
    for (index, byte) in pattern.iter_mut().enumerate() {
        *byte = (index as u8).wrapping_mul(37) ^ 0xA5;
    }
    for lba in [0u64, 1u64] {
        blk::write_sector(&caps, SLOT_RW, QA_DISK, lba, &pattern)
            .map_err(|e| failure("write/readback", error_message(e)))?;
        let mut readback = [0u8; SECTOR_SIZE];
        blk::read_sector(&caps, SLOT_RW, QA_DISK, lba, &mut readback)
            .map_err(|e| failure("write/readback", error_message(e)))?;
        if readback != pattern {
            return Err(failure(
                "write/readback",
                "el contenido leído no coincide con el patrón escrito",
            ));
        }
    }
    let _ = writeln!(
        console,
        "T1.2: write/readback PASS (device=1, lba=0/1, pattern={:02x}{:02x}..{:02x})",
        pattern[0],
        pattern[1],
        pattern[SECTOR_SIZE - 1]
    );

    // 5. Una capacidad solo READ no puede escribir.
    let mut read_only_caps = CapTable::new();
    read_only_caps
        .insert(
            SLOT_RO,
            Capability {
                object: blk::BLOCK_OBJECT_ID,
                rights: Rights::READ,
            },
        )
        .map_err(|_| failure("capability check", "no se pudo crear capacidad RO"))?;
    let denied = blk::write_sector(&read_only_caps, SLOT_RO, QA_DISK, 0, &pattern);
    if denied != Err(BlockError::Capability(CapError::RightsDenied)) {
        return Err(failure(
            "capability check",
            "capacidad READ permitió escribir",
        ));
    }
    // El disco de arranque es de solo lectura incluso con capacidad RW.
    let boot_write = blk::write_sector(&caps, SLOT_RW, BOOT_DISK, 1, &pattern);
    if boot_write != Err(BlockError::ReadOnlyDevice) {
        return Err(failure(
            "capability check",
            "el disco 0 aceptó una escritura",
        ));
    }
    let _ = writeln!(console, "T1.2: capability denied without WRITE PASS");

    let _ = writeln!(console, "T1.2: block service tests PASS");
    Ok(())
}
