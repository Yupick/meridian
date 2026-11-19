//! Verificación Ed25519 de la imagen del kernel (T1.1).
//!
//! La clave pública está embebida en el bootloader como raíz de confianza de
//! desarrollo. La firma viaja aparte, en `\kernel.sig` dentro de la ESP, y se
//! calcula sobre el contenido íntegro de `\kernel.elf`.

use core::fmt;

use meridian_crypto::ed25519::{verify, PUBLIC_KEY_LEN, SIGNATURE_LEN};

/// Ruta del fichero de firma dentro de la ESP.
pub const KERNEL_SIGNATURE_PATH: &str = "\\kernel.sig";

/// Clave pública de desarrollo correspondiente a la clave RFC 8032 TEST 1.
const KERNEL_PUBLIC_KEY: [u8; PUBLIC_KEY_LEN] = [
    0xd7, 0x5a, 0x98, 0x01, 0x82, 0xb1, 0x0a, 0xb7, 0xd5, 0x4b, 0xfe, 0xd3, 0xc9, 0x64, 0x07, 0x3a,
    0x0e, 0xe1, 0x72, 0xf3, 0xda, 0xa6, 0x23, 0x25, 0xaf, 0x02, 0x1a, 0x68, 0xf7, 0x07, 0x51, 0x1a,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignatureError {
    /// `\kernel.sig` no existe o está vacío.
    Missing,
    /// El fichero de firma no tiene exactamente 64 bytes.
    InvalidLength,
    /// La firma no corresponde al kernel o a la clave pública embebida.
    Invalid,
}

impl fmt::Display for SignatureError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Missing => write!(f, "firma de kernel ausente"),
            Self::InvalidLength => write!(f, "longitud de firma inválida"),
            Self::Invalid => write!(f, "firma de kernel inválida"),
        }
    }
}

/// Verifica `signature` sobre `kernel` con la clave pública embebida.
pub fn verify_kernel_signature(kernel: &[u8], signature: &[u8]) -> Result<(), SignatureError> {
    if signature.is_empty() {
        return Err(SignatureError::Missing);
    }
    if signature.len() != SIGNATURE_LEN {
        return Err(SignatureError::InvalidLength);
    }

    let signature: &[u8; SIGNATURE_LEN] = match signature.try_into() {
        Ok(signature) => signature,
        Err(_) => return Err(SignatureError::InvalidLength),
    };

    if verify(&KERNEL_PUBLIC_KEY, kernel, signature) {
        Ok(())
    } else {
        Err(SignatureError::Invalid)
    }
}
