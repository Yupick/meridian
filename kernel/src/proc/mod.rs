//! Procesos mínimos, espacios de direcciones y capacidades (A5 + A6).
//!
//! Un [`Process`] lógico agrupa el identificador, la raíz de paginación (PML4)
//! y la tabla de capacidades del proceso. En A6 todavía no hay scheduler ni
//! ring 3: las auto-pruebas crean procesos desde el kernel y conmutan `CR3`
//! para ejercitar cada espacio y su tabla de capacidades por separado.

pub mod selftest;

use crate::cap::{CapError, CapTable, Capability, ObjectId, Rights};

/// Identificador lógico de proceso dentro del kernel.
pub type ProcessId = u16;

/// Bloque de control de proceso mínimo.
pub struct Process {
    pub id: ProcessId,
    /// Raíz de tablas de página (PML4 físico) del espacio del proceso.
    pub root: u64,
    /// Capacidades que el proceso puede presentar en operaciones IPC.
    pub caps: CapTable,
}

impl Process {
    /// Crea un proceso lógico sin capacidades sobre un espacio ya creado.
    pub fn new(id: ProcessId, root: u64) -> Self {
        Self {
            id,
            root,
            caps: CapTable::new(),
        }
    }

    /// Inserta una capacidad en `slot`. Es el punto donde el kernel (o un
    /// servicio privilegiado) concede acceso a un objeto.
    pub fn grant(&mut self, slot: usize, object: ObjectId, rights: Rights) -> Result<(), CapError> {
        self.caps.insert(slot, Capability { object, rights })
    }
}
