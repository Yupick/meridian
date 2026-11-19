//! Auto-pruebas del volumen cifrado (T1.5).
//!
//! Emite los cinco marcadores exigidos por QA por COM1 (a través de
//! `Console`). Cualquier fallo imprime `T1.5: <step> FAIL: <msg>`.
//! Nunca se imprimen bytes de la passphrase, de la clave derivada ni del
//! ciphertext: solo longitudes y estados.

use core::fmt::Write;

use crate::blk::{self, BlockDeviceId, BLOCK_OBJECT_ID, SECTOR_SIZE};
use crate::cap::{CapTable, Capability, Rights};
use crate::console::Console;
use crate::obj::MAX_VALUE_LEN;

use super::{
    EncryptedVolume, VolError, DATA_KEY_BASE, DEV_PASSPHRASE, MAX_PLAINTEXT_LEN, OBJECT_MAGIC,
};

const SLOT_RW: usize = 1;
const QA_DEVICE: BlockDeviceId = 1;
const QA_KEY: u64 = 7;

struct Failure {
    step: &'static str,
    message: &'static str,
}

fn failure(step: &'static str, message: &'static str) -> Failure {
    Failure { step, message }
}

/// Ejecuta la prueba T1.5 completa.
pub fn run(console: &mut Console) -> bool {
    match run_inner(console) {
        Ok(()) => true,
        Err(failed) => {
            let _ = writeln!(console, "T1.5: {} FAIL: {}", failed.step, failed.message);
            let _ = writeln!(console, "T1.5: volume encryption tests FAIL: {}", failed.message);
            false
        }
    }
}

fn error_message(error: VolError) -> &'static str {
    match error {
        VolError::Block(_) => "error de E/S en el servicio de bloques",
        VolError::Object(_) => "error en el sistema de objetos",
        VolError::Kdf(_) => "fallo al derivar la clave",
        VolError::Xts(_) => "fallo en XTS-AES",
        VolError::BadHeader => "cabecera de volumen inválida",
        VolError::UnsupportedVersion => "versión de volumen no soportada",
        VolError::HeaderHashMismatch => "checksum de cabecera incorrecto",
        VolError::BadFrame => "frame cifrado inválido",
        VolError::FrameHashMismatch => "checksum de frame incorrecto",
        VolError::WrongKey => "passphrase incorrecta",
        VolError::PlaintextTooLarge => "plaintext mayor que 4000 bytes",
        VolError::BufferTooSmall => "búfer de salida demasiado pequeño",
        VolError::InvalidKey => "identificador de clave inválido",
    }
}

fn block_error(error: blk::BlockError) -> Failure {
    failure("block io", error_message(VolError::Block(error)))
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

    let plaintext = b"meridian-encrypted-volume-v1";
    let mut out = [0u8; MAX_PLAINTEXT_LEN];

    // 1. Formato: cabecera + verificador, y lectura con la clave correcta.
    let mut volume = EncryptedVolume::format(&caps, SLOT_RW, QA_DEVICE, DEV_PASSPHRASE)
        .map_err(|e| failure("format", error_message(e)))?;
    let opened = EncryptedVolume::open(&caps, SLOT_RW, QA_DEVICE, DEV_PASSPHRASE)
        .map_err(|e| failure("format", error_message(e)))?;
    drop(opened);

    volume
        .write(&caps, SLOT_RW, QA_KEY, plaintext)
        .map_err(|e| failure("format", error_message(e)))?;
    let len = volume
        .read(&caps, SLOT_RW, QA_KEY, &mut out)
        .map_err(|e| failure("format", error_message(e)))?;
    if len != plaintext.len() || &out[..len] != plaintext {
        return Err(failure(
            "format",
            "la lectura no devolvió el plaintext escrito",
        ));
    }
    let _ = writeln!(
        console,
        "T1.5: encrypted volume format PASS (device={} generation={} key_id={} payload_bytes={})",
        QA_DEVICE, volume.store.generation, QA_KEY, plaintext.len()
    );

    // 2. Una passphrase distinta debe derivar otra clave y fallar el verificador.
    match EncryptedVolume::open(&caps, SLOT_RW, QA_DEVICE, b"wrong-qa-passphrase") {
        Err(VolError::WrongKey) => {}
        Err(error) => return Err(failure("wrong key", error_message(error))),
        Ok(_) => return Err(failure("wrong key", "passphrase incorrecta aceptada")),
    }
    let _ = writeln!(
        console,
        "T1.5: wrong key rejected PASS (result=WrongKey)"
    );

    // 3. raw_read devuelve ciphertext: el frame mágico no aparece en claro.
    let mut raw = [0u8; MAX_VALUE_LEN];
    let raw_len = volume
        .raw_read(&caps, SLOT_RW, QA_KEY, &mut raw)
        .map_err(|e| failure("raw sectors are ciphertext", error_message(e)))?;
    if raw_len < 16 + plaintext.len() {
        return Err(failure(
            "raw sectors are ciphertext",
            "longitud de ciphertext inesperada",
        ));
    }
    if &raw[..8] == OBJECT_MAGIC || &raw[16..16 + plaintext.len()] == plaintext {
        return Err(failure(
            "raw sectors are ciphertext",
            "el objeto sin descifrar contiene plaintext",
        ));
    }

    // La reescritura del ciphertext se detecta; se restaura el sector para no
    // dejar el volumen corrupto antes de la prueba de persistencia.
    let (value_lba, _) = volume
        .store
        .locate(&caps, SLOT_RW, DATA_KEY_BASE + QA_KEY)
        .map_err(|e| {
            failure(
                "raw sectors are ciphertext",
                error_message(VolError::Object(e)),
            )
        })?
        .ok_or_else(|| {
            failure(
                "raw sectors are ciphertext",
                "el objeto de datos no quedó indexado",
            )
        })?;
    let mut original = [0u8; SECTOR_SIZE];
    blk::read_sector(&caps, SLOT_RW, QA_DEVICE, value_lba, &mut original).map_err(block_error)?;
    let mut corrupted = original;
    corrupted[32] ^= 0x01;
    blk::write_sector(&caps, SLOT_RW, QA_DEVICE, value_lba, &corrupted).map_err(block_error)?;
    if volume.read(&caps, SLOT_RW, QA_KEY, &mut out).is_ok() {
        return Err(failure(
            "raw sectors are ciphertext",
            "la corrupción del ciphertext no se detectó",
        ));
    }
    blk::write_sector(&caps, SLOT_RW, QA_DEVICE, value_lba, &original).map_err(block_error)?;
    let len = volume
        .read(&caps, SLOT_RW, QA_KEY, &mut out)
        .map_err(|e| failure("raw sectors are ciphertext", error_message(e)))?;
    if len != plaintext.len() || &out[..len] != plaintext {
        return Err(failure(
            "raw sectors are ciphertext",
            "el ciphertext restaurado no descifra el plaintext",
        ));
    }
    let _ = writeln!(
        console,
        "T1.5: raw sectors are ciphertext PASS (bytes={} frame_magic_hidden=1)",
        raw_len
    );

    // 4. Simular reinicio: segundo open sin formatear y misma lectura.
    drop(volume);
    let reopened = EncryptedVolume::open(&caps, SLOT_RW, QA_DEVICE, DEV_PASSPHRASE)
        .map_err(|e| failure("persists after reopen", error_message(e)))?;
    let len = reopened
        .read(&caps, SLOT_RW, QA_KEY, &mut out)
        .map_err(|e| failure("persists after reopen", error_message(e)))?;
    if len != plaintext.len() || &out[..len] != plaintext {
        return Err(failure(
            "persists after reopen",
            "el dato no sobrevivió al reopen",
        ));
    }
    let _ = writeln!(
        console,
        "T1.5: persists after reopen PASS (device={} key_id={} generation={} bytes={})",
        QA_DEVICE, QA_KEY, reopened.store.generation, len
    );

    let _ = writeln!(console, "T1.5: volume encryption tests PASS");
    Ok(())
}
