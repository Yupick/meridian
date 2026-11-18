//! Capacidades simples y tabla de capacidades por proceso (A6).
//!
//! Un proceso posee una [`CapTable`] de tamaño fijo. Cada *slot* de la tabla
//! puede contener una [`Capability`]: una referencia opaca a un objeto del
//! kernel (hoy, un puerto IPC) junto con un conjunto de derechos. Para operar
//! sobre el objeto el proceso debe presentar el slot y tener el derecho
//! requerido; no existe ningún acceso por id de objeto que omita la capacidad.
//!
//! # Derechos
//!
//! | Derecho | Bit | Uso sobre un puerto IPC |
//! |---|---|---|
//! | `READ` | 0 | [`crate::ipc::recv`]: consumir mensajes de la cola |
//! | `WRITE` | 1 | [`crate::ipc::clear_queue`]: modificar el estado del puerto |
//! | `SEND` | 2 | [`crate::ipc::send`]: encolar un mensaje |
//! | `EXECUTE` | 3 | [`crate::ipc::invoke`]: invocar/consultar el puerto |
//!
//! # Regla monotónica
//!
//! [`CapTable::derive`] crea una nueva capacidad en otro slot y
//! [`CapTable::attenuate`] reduce los derechos de una capacidad existente. En
//! ambos casos los derechos solicitados deben ser un **subconjunto** de los
//! derechos de la capacidad origen. Cualquier intento de añadir un derecho no
//! presente devuelve [`CapError::RightsEscalation`]; no hay ninguna operación
//! que pueda ampliar derechos. El objeto referenciado tampoco cambia al
//! derivar.

use core::fmt;

/// Identificador de un objeto del kernel referenciable por capacidades.
///
/// El espacio de objetos es compartido por los subsistemas que lo usan; en A6
/// solo existen puertos IPC, cuyo [`crate::ipc::PortId`] es un `ObjectId`.
pub type ObjectId = u16;

/// Conjunto de derechos de una capacidad.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Rights(u32);

impl Rights {
    /// Recibir/consumir datos del objeto.
    pub const READ: Self = Self(1 << 0);
    /// Modificar el estado del objeto.
    pub const WRITE: Self = Self(1 << 1);
    /// Enviar datos al objeto.
    pub const SEND: Self = Self(1 << 2);
    /// Invocar/consultar el objeto.
    pub const EXECUTE: Self = Self(1 << 3);
    /// Todos los derechos definidos.
    pub const ALL: Self = Self(Self::READ.0 | Self::WRITE.0 | Self::SEND.0 | Self::EXECUTE.0);

    /// `true` si `self` contiene **todos** los derechos de `required`.
    pub const fn contains(self, required: Self) -> bool {
        self.0 & required.0 == required.0
    }

    /// `true` si `self` no tiene ningún derecho fuera de `other`.
    pub const fn is_subset_of(self, other: Self) -> bool {
        self.0 & !other.0 == 0
    }

    /// `true` si no hay bits de derecho desconocidos.
    pub const fn is_valid(self) -> bool {
        self.0 & !Self::ALL.0 == 0
    }

    /// Unión de dos conjuntos de derechos.
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }
}

impl fmt::Display for Rights {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let flag = |right: Self, name: char| if self.contains(right) { name } else { '-' };
        write!(
            f,
            "{}{}{}{}",
            flag(Self::READ, 'r'),
            flag(Self::WRITE, 'w'),
            flag(Self::SEND, 's'),
            flag(Self::EXECUTE, 'x')
        )
    }
}

/// Capacidad: objeto referenciado + derechos que el poseedor tiene sobre él.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Capability {
    pub object: ObjectId,
    pub rights: Rights,
}

/// Errores de la tabla de capacidades.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CapError {
    /// El índice de slot no pertenece a la tabla.
    InvalidSlot,
    /// El slot existe pero está vacío (el proceso no posee la capacidad).
    EmptySlot,
    /// El slot de destino ya contiene una capacidad.
    SlotOccupied,
    /// La capacidad existe pero no incluye el derecho requerido.
    RightsDenied,
    /// Se intentaron obtener derechos no presentes en la capacidad origen.
    RightsEscalation,
    /// El conjunto de derechos contiene bits desconocidos.
    InvalidRights,
}

impl fmt::Display for CapError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidSlot => write!(f, "slot de capacidad inválido"),
            Self::EmptySlot => write!(f, "slot de capacidad vacío"),
            Self::SlotOccupied => write!(f, "slot de capacidad ya ocupado"),
            Self::RightsDenied => write!(f, "derechos insuficientes"),
            Self::RightsEscalation => write!(f, "intento de ampliar derechos"),
            Self::InvalidRights => write!(f, "derechos desconocidos"),
        }
    }
}

/// Número de slots de capacidad por proceso.
pub const MAX_CAPS: usize = 16;

/// Tabla de capacidades de un proceso.
///
/// Es una estructura de tamaño fijo (sin `alloc`) pensada para vivir en el PCB
/// del proceso. Los slots son handles: el número de slot es lo que el proceso
/// presenta en las operaciones y el kernel resuelve la capacidad internamente.
#[derive(Clone, Copy)]
pub struct CapTable {
    slots: [Option<Capability>; MAX_CAPS],
}

impl CapTable {
    /// Crea una tabla sin capacidades.
    pub const fn new() -> Self {
        Self {
            slots: [None; MAX_CAPS],
        }
    }

    /// Inserta `capability` en un slot vacío.
    pub fn insert(&mut self, slot: usize, capability: Capability) -> Result<(), CapError> {
        if slot >= MAX_CAPS {
            return Err(CapError::InvalidSlot);
        }
        if !capability.rights.is_valid() {
            return Err(CapError::InvalidRights);
        }
        if self.slots[slot].is_some() {
            return Err(CapError::SlotOccupied);
        }
        self.slots[slot] = Some(capability);
        Ok(())
    }

    /// Devuelve la capacidad del slot o un error explícito.
    pub fn get(&self, slot: usize) -> Result<Capability, CapError> {
        if slot >= MAX_CAPS {
            return Err(CapError::InvalidSlot);
        }
        self.slots[slot].ok_or(CapError::EmptySlot)
    }

    /// Exige que el slot contenga el derecho `required` y devuelve la
    /// capacidad. Es la puerta de entrada que usan los recursos IPC.
    pub fn check(&self, slot: usize, required: Rights) -> Result<Capability, CapError> {
        let capability = self.get(slot)?;
        if capability.rights.contains(required) {
            Ok(capability)
        } else {
            Err(CapError::RightsDenied)
        }
    }

    /// Deriva una capacidad reducida a `rights` en el slot `to`.
    ///
    /// Rechaza con [`CapError::RightsEscalation`] si `rights` contiene algún
    /// derecho que no esté presente en la capacidad origen. El slot destino
    /// debe estar vacío para que una derivación nunca sobrescriba otra
    /// capacidad.
    pub fn derive(
        &mut self,
        from: usize,
        to: usize,
        rights: Rights,
    ) -> Result<Capability, CapError> {
        if to >= MAX_CAPS {
            return Err(CapError::InvalidSlot);
        }
        if !rights.is_valid() {
            return Err(CapError::InvalidRights);
        }
        let source = self.get(from)?;
        if !rights.is_subset_of(source.rights) {
            return Err(CapError::RightsEscalation);
        }
        if self.slots[to].is_some() {
            return Err(CapError::SlotOccupied);
        }
        let derived = Capability {
            object: source.object,
            rights,
        };
        self.slots[to] = Some(derived);
        Ok(derived)
    }

    /// Reduce *in place* los derechos de una capacidad existente.
    ///
    /// Igual que [`Self::derive`], solo acepta un subconjunto de los derechos
    /// actuales; nunca puede ampliarlos.
    pub fn attenuate(&mut self, slot: usize, rights: Rights) -> Result<Capability, CapError> {
        if !rights.is_valid() {
            return Err(CapError::InvalidRights);
        }
        let current = self.get(slot)?;
        if !rights.is_subset_of(current.rights) {
            return Err(CapError::RightsEscalation);
        }
        let reduced = Capability {
            object: current.object,
            rights,
        };
        self.slots[slot] = Some(reduced);
        Ok(reduced)
    }
}
