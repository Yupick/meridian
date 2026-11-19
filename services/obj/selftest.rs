//! Auto-pruebas del sistema de objetos en disco (T1.3).
//!
//! Emite los cinco marcadores exigidos por QA por COM1 (a través de
//! `Console`). Cualquier fallo imprime `T1.3: <step> FAIL: <msg>` para que no
//! se confunda con los marcadores de T1.2.

use core::fmt::Write;

use crate::blk::{self, BlockDeviceId, BLOCK_OBJECT_ID, SECTOR_SIZE};
use crate::cap::{CapTable, Capability, Rights};
use crate::console::Console;

use super::{ObjError, ObjectStore, SUPERBLOCK_A, SUPERBLOCK_B};

const SLOT_RW: usize = 1;
const QA_DEVICE: BlockDeviceId = 1;

struct Failure {
    step: &'static str,
    message: &'static str,
}

fn failure(step: &'static str, message: &'static str) -> Failure {
    Failure { step, message }
}

/// Ejecuta la prueba T1.3 completa.
pub fn run(console: &mut Console) -> bool {
    match run_inner(console) {
        Ok(()) => true,
        Err(failed) => {
            let _ = writeln!(console, "T1.3: {} FAIL: {}", failed.step, failed.message);
            let _ = writeln!(console, "T1.3: disk object tests FAIL: {}", failed.message);
            false
        }
    }
}

fn error_message(error: ObjError) -> &'static str {
    match error {
        ObjError::Block(_) => "error de E/S en el servicio de bloques",
        ObjError::NotFormatted => "superblock no formateado",
        ObjError::BadSuperblock => "superblock inválido",
        ObjError::BadNode => "nodo inválido",
        ObjError::NodeHashMismatch => "checksum de nodo incorrecto",
        ObjError::DataHashMismatch => "hash del dato incorrecto",
        ObjError::KeyNotFound => "clave no encontrada",
        ObjError::ValueTooLarge => "valor mayor que 4096 bytes",
        ObjError::BufferTooSmall => "búfer de salida demasiado pequeño",
        ObjError::TooManyObjects => "límite de objetos alcanzado",
        ObjError::OutOfSpace => "sin sectores libres",
    }
}

fn block_error(error: blk::BlockError) -> Failure {
    failure("block io", error_message(ObjError::Block(error)))
}

fn run_inner(console: &mut Console) -> Result<(), Failure> {
    let mut caps = CapTable::new();
    caps.insert(
        SLOT_RW,
        Capability {
            object: BLOCK_OBJECT_ID,
            rights: Rights::READ.union(Rights::WRITE),
        },
    )
    .map_err(|_| failure("capabilities", "no se pudo crear capacidad RW"))?;

    let sectors = blk::sector_count(QA_DEVICE);
    if sectors < 64 {
        return Err(failure("format", "el disco QA tiene menos de 64 sectores"));
    }

    // 1. Formato: dos superblocks con generación 1 y raíz verificable.
    let mut store = ObjectStore::format(&caps, SLOT_RW, QA_DEVICE)
        .map_err(|e| failure("format", error_message(e)))?;
    let opened = ObjectStore::open(&caps, SLOT_RW, QA_DEVICE)
        .map_err(|e| failure("format", error_message(e)))?;
    if opened != store || store.generation != 1 {
        return Err(failure("format", "open no devolvió el estado formateado"));
    }
    let _ = writeln!(
        console,
        "T1.3: disk objects format PASS (device=1 generation=1 root_lba={} next_free={})",
        store.root_lba, store.next_free
    );

    // 2. Copy-on-write: ni la raíz antigua ni el valor antiguo se sobrescriben.
    let old_root_lba = store.root_lba;
    let mut old_root = [0u8; SECTOR_SIZE];
    blk::read_sector(&caps, SLOT_RW, QA_DEVICE, old_root_lba, &mut old_root)
        .map_err(block_error)?;

    store
        .write(&caps, SLOT_RW, 1, b"meridian-value-v1")
        .map_err(|e| failure("cow commit", error_message(e)))?;
    if store.generation != 2 || store.root_lba == old_root_lba {
        return Err(failure("cow commit", "el commit no publicó una raíz nueva"));
    }
    let mut after_root = [0u8; SECTOR_SIZE];
    blk::read_sector(&caps, SLOT_RW, QA_DEVICE, old_root_lba, &mut after_root)
        .map_err(block_error)?;
    if after_root != old_root {
        return Err(failure("cow commit", "la raíz antigua fue sobrescrita"));
    }

    let (old_value_lba, _) = store
        .locate(&caps, SLOT_RW, 1)
        .map_err(|e| failure("cow commit", error_message(e)))?
        .ok_or_else(|| failure("cow commit", "la clave 1 no quedó indexada"))?;
    let mut old_value = [0u8; SECTOR_SIZE];
    blk::read_sector(&caps, SLOT_RW, QA_DEVICE, old_value_lba, &mut old_value)
        .map_err(block_error)?;

    // Valor de dos sectores: también ejercita extents multi-sector.
    let mut large = [0x5Au8; 640];
    large[0] = b'L';
    large[639] = b'Z';
    store
        .write(&caps, SLOT_RW, 1, &large)
        .map_err(|e| failure("cow commit", error_message(e)))?;
    let mut after_value = [0u8; SECTOR_SIZE];
    blk::read_sector(&caps, SLOT_RW, QA_DEVICE, old_value_lba, &mut after_value)
        .map_err(block_error)?;
    if after_value != old_value {
        return Err(failure("cow commit", "el extent antiguo fue sobrescrito"));
    }

    // Más de FANOUT entradas obligan a reconstruir otro nivel de hojas.
    let mut key = 10u64;
    while key < 20 {
        store
            .write(&caps, SLOT_RW, key, b"meridian-object")
            .map_err(|e| failure("cow commit", error_message(e)))?;
        key += 1;
    }
    let mut out = [0u8; 640];
    let read_len = store
        .read(&caps, SLOT_RW, 1, &mut out)
        .map_err(|e| failure("cow commit", error_message(e)))?;
    if read_len != large.len() || &out[..read_len] != &large[..] {
        return Err(failure(
            "cow commit",
            "la lectura no devolvió el valor actual",
        ));
    }
    let mut key = 10u64;
    while key < 20 {
        let read_len = store
            .read(&caps, SLOT_RW, key, &mut out)
            .map_err(|e| failure("cow commit", error_message(e)))?;
        if read_len != b"meridian-object".len() || &out[..read_len] != b"meridian-object" {
            return Err(failure("cow commit", "clave intermedia corrupta"));
        }
        key += 1;
    }
    let _ = writeln!(
        console,
        "T1.3: cow commit PASS (keys=11 generation={} root_lba={})",
        store.generation, store.root_lba
    );

    // 3. Corte simulado: se escriben nodos nuevos sin tocar el superblock.
    store
        .write(&caps, SLOT_RW, 99, b"committed-state")
        .map_err(|e| failure("crash consistency", error_message(e)))?;
    let committed_generation = store.generation;
    let committed_root = store.root_lba;
    store
        .write_uncommitted(&caps, SLOT_RW, 99, b"UNCOMMITTED-DO-NOT-SEE")
        .map_err(|e| failure("crash consistency", error_message(e)))?;
    if store.generation != committed_generation || store.root_lba != committed_root {
        return Err(failure(
            "crash consistency",
            "write_uncommitted modificó el estado en memoria",
        ));
    }
    let reopened = ObjectStore::open(&caps, SLOT_RW, QA_DEVICE)
        .map_err(|e| failure("crash consistency", error_message(e)))?;
    if reopened.generation != committed_generation || reopened.root_lba != committed_root {
        return Err(failure(
            "crash consistency",
            "open vio la generación no comprometida",
        ));
    }
    let read_len = reopened
        .read(&caps, SLOT_RW, 99, &mut out)
        .map_err(|e| failure("crash consistency", error_message(e)))?;
    if read_len != b"committed-state".len() || &out[..read_len] != b"committed-state" {
        return Err(failure(
            "crash consistency",
            "open devolvió el valor no comprometido",
        ));
    }
    let _ = writeln!(
        console,
        "T1.3: crash consistency PASS (generation={} root_lba={})",
        reopened.generation, reopened.root_lba
    );

    // 4. Checksums: dato, nodo y superblock. Todo se restaura para no dejar
    //    basura que afecte a comprobaciones posteriores.
    let (value_lba, _) = reopened
        .locate(&caps, SLOT_RW, 99)
        .map_err(|e| failure("checksum detects corruption", error_message(e)))?
        .ok_or_else(|| failure("checksum detects corruption", "clave 99 no indexada"))?;
    let mut original = [0u8; SECTOR_SIZE];
    blk::read_sector(&caps, SLOT_RW, QA_DEVICE, value_lba, &mut original).map_err(block_error)?;
    let mut corrupt = original;
    corrupt[0] ^= 0x01;
    blk::write_sector(&caps, SLOT_RW, QA_DEVICE, value_lba, &corrupt).map_err(block_error)?;
    if reopened.read(&caps, SLOT_RW, 99, &mut out).is_ok() {
        return Err(failure(
            "checksum detects corruption",
            "el hash del dato no detectó la corrupción",
        ));
    }
    blk::write_sector(&caps, SLOT_RW, QA_DEVICE, value_lba, &original).map_err(block_error)?;
    reopened.read(&caps, SLOT_RW, 99, &mut out).map_err(|_| {
        failure(
            "checksum detects corruption",
            "no se pudo restaurar el dato",
        )
    })?;

    let root_lba = reopened.root_lba;
    blk::read_sector(&caps, SLOT_RW, QA_DEVICE, root_lba, &mut original).map_err(block_error)?;
    corrupt = original;
    corrupt[16] ^= 0x01;
    blk::write_sector(&caps, SLOT_RW, QA_DEVICE, root_lba, &corrupt).map_err(block_error)?;
    if reopened.read(&caps, SLOT_RW, 99, &mut out).is_ok() {
        return Err(failure(
            "checksum detects corruption",
            "el checksum del nodo no detectó la corrupción",
        ));
    }
    blk::write_sector(&caps, SLOT_RW, QA_DEVICE, root_lba, &original).map_err(block_error)?;
    reopened.read(&caps, SLOT_RW, 99, &mut out).map_err(|_| {
        failure(
            "checksum detects corruption",
            "no se pudo restaurar el nodo",
        )
    })?;

    // El superblock con mayor generación se corrompe: `open` debe usar el otro.
    let latest = reopened.generation % 2;
    let super_lba = if latest == 0 {
        SUPERBLOCK_A
    } else {
        SUPERBLOCK_B
    };
    blk::read_sector(&caps, SLOT_RW, QA_DEVICE, super_lba, &mut original).map_err(block_error)?;
    corrupt = original;
    corrupt[16] ^= 0x01;
    blk::write_sector(&caps, SLOT_RW, QA_DEVICE, super_lba, &corrupt).map_err(block_error)?;
    let fallback = ObjectStore::open(&caps, SLOT_RW, QA_DEVICE)
        .map_err(|e| failure("checksum detects corruption", error_message(e)))?;
    if fallback.generation >= reopened.generation {
        return Err(failure(
            "checksum detects corruption",
            "open no usó el superblock de reserva",
        ));
    }
    blk::write_sector(&caps, SLOT_RW, QA_DEVICE, super_lba, &original).map_err(block_error)?;

    let _ = writeln!(console, "T1.3: checksum detects corruption PASS");

    let _ = writeln!(console, "T1.3: disk object tests PASS");
    Ok(())
}
