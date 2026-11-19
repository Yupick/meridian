//! Permisos sobre objetos del volumen cifrado (T2.2).
//!
//! Las ACLs son estáticas y viven en `readonly`: cada clave de objeto del
//! volumen ([`OBJ_SHARED_CONFIG`], [`OBJ_AUDIT_LOG`], [`OBJ_ROOT_SECRET`] y
//! [`OBJ_PUBLIC_NOTICE`]) tiene un conjunto de grupos con derecho de lectura y
//! otro con derecho de escritura. Los grupos vienen de la sesión autenticada
//! (T2.2 en `auth`) y se comprueban **antes** de delegar en el volumen cifrado.
//!
//! No hay heap, ni estado global mutable: [`PermissionedVolume`] simplemente
//! envuelve un [`vol::EncryptedVolume`] y mantiene la cadena FS/bloque intacta
//! (las operaciones reciben `caps`/`slot` y se los pasan al volumen, como en
//! T1.5). Las claves y el ciphertext nunca se escriben en la consola.

use core::fmt;

use crate::auth;
use crate::auth::{GROUP_ADMIN, GROUP_ALL, GROUP_AUDIT, GROUP_USERS};
use crate::blk::BlockDeviceId;
use crate::cap::CapTable;
use crate::vol::{self, EncryptedVolume};

pub mod selftest;

/// Derechos usados por las ACLs (los mismos que el resto del kernel).
pub use crate::cap::Rights;

/// Configuración compartida: leen `USERS`/`ADMIN`, escribe `ADMIN`.
pub const OBJ_SHARED_CONFIG: u64 = 10;
/// Registro de auditoría: leen `AUDIT`/`ADMIN`, escribe `ADMIN`.
pub const OBJ_AUDIT_LOG: u64 = 11;
/// Secreto de root: solo `ADMIN` puede leerlo y escribirlo.
pub const OBJ_ROOT_SECRET: u64 = 12;
/// Aviso público: leen todos los grupos, escribe `ADMIN`.
pub const OBJ_PUBLIC_NOTICE: u64 = 13;

/// Error de la capa de permisos.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PermError {
    /// La clave no corresponde a ningún objeto con ACL.
    UnknownObject,
    /// Ninguno de los grupos de la sesión tiene el derecho solicitado.
    AccessDenied,
    /// Fallo delegado al volumen cifrado (T1.5).
    Volume(vol::VolError),
}

impl fmt::Display for PermError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownObject => write!(f, "objeto sin ACL"),
            Self::AccessDenied => write!(f, "acceso denegado por ACL"),
            Self::Volume(error) => write!(f, "volumen: {error}"),
        }
    }
}

impl From<vol::VolError> for PermError {
    fn from(error: vol::VolError) -> Self {
        Self::Volume(error)
    }
}

/// Entrada de la tabla estática de ACLs.
#[derive(Clone, Copy)]
struct Acl {
    key: u64,
    read_groups: u8,
    write_groups: u8,
}

/// ACLs del volumen QA. Tabla inmutable: no se construye en tiempo de ejecución.
const ACLS: &[Acl] = &[
    Acl {
        key: OBJ_SHARED_CONFIG,
        read_groups: GROUP_USERS | GROUP_ADMIN,
        write_groups: GROUP_ADMIN,
    },
    Acl {
        key: OBJ_AUDIT_LOG,
        read_groups: GROUP_AUDIT | GROUP_ADMIN,
        write_groups: GROUP_ADMIN,
    },
    Acl {
        key: OBJ_ROOT_SECRET,
        read_groups: GROUP_ADMIN,
        write_groups: GROUP_ADMIN,
    },
    Acl {
        key: OBJ_PUBLIC_NOTICE,
        read_groups: GROUP_ALL,
        write_groups: GROUP_ADMIN,
    },
];

/// Comprueba que los grupos de `session` tienen todos los derechos `required`
/// sobre el objeto `key`. No consulta la clave ni el contenido del objeto.
pub fn check(session: &auth::Session, key: u64, required: Rights) -> Result<(), PermError> {
    let mut index = 0;
    while index < ACLS.len() {
        let acl = &ACLS[index];
        if acl.key == key {
            if required.contains(Rights::READ) && session.groups & acl.read_groups == 0 {
                return Err(PermError::AccessDenied);
            }
            if required.contains(Rights::WRITE) && session.groups & acl.write_groups == 0 {
                return Err(PermError::AccessDenied);
            }
            return Ok(());
        }
        index += 1;
    }
    Err(PermError::UnknownObject)
}

/// Volumen cifrado cuya lectura y escritura están mediadas por las ACLs.
pub struct PermissionedVolume {
    volume: EncryptedVolume,
}

impl PermissionedVolume {
    /// Formatea el volumen delegando en T1.5 (sin ACL: aún no hay objetos).
    pub fn format(
        caps: &CapTable,
        slot: usize,
        device: BlockDeviceId,
        passphrase: &[u8],
    ) -> Result<Self, PermError> {
        Ok(Self {
            volume: EncryptedVolume::format(caps, slot, device, passphrase)?,
        })
    }

    /// Abre un volumen existente delegando en T1.5.
    pub fn open(
        caps: &CapTable,
        slot: usize,
        device: BlockDeviceId,
        passphrase: &[u8],
    ) -> Result<Self, PermError> {
        Ok(Self {
            volume: EncryptedVolume::open(caps, slot, device, passphrase)?,
        })
    }

    /// Lee un objeto cifrado solo si la sesión tiene derecho de lectura.
    pub fn read(
        &self,
        session: &auth::Session,
        caps: &CapTable,
        slot: usize,
        key: u64,
        out: &mut [u8],
    ) -> Result<usize, PermError> {
        check(session, key, Rights::READ)?;
        Ok(self.volume.read(caps, slot, key, out)?)
    }

    /// Escribe un objeto cifrado solo si la sesión tiene derecho de escritura.
    pub fn write(
        &mut self,
        session: &auth::Session,
        caps: &CapTable,
        slot: usize,
        key: u64,
        data: &[u8],
    ) -> Result<(), PermError> {
        check(session, key, Rights::WRITE)?;
        self.volume.write(caps, slot, key, data)?;
        Ok(())
    }
}
