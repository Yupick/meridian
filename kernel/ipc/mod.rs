//! IPC por puertos con colas de mensajes de tamaño fijo (A6).
//!
//! Un *puerto* es un objeto del kernel con una cola FIFO acotada de mensajes de
//! tamaño fijo. Los procesos no pueden operar sobre un puerto usando su id
//! directamente: deben presentar un slot de su [`CapTable`] que referencie el
//! puerto y contenga el derecho adecuado.
//!
//! | Operación | Derecho exigido |
//! |---|---|
//! | [`send`] | `SEND` |
//! | [`recv`] | `READ` |
//! | [`clear_queue`] | `WRITE` |
//! | [`invoke`] | `EXECUTE` |
//!
//! Si el slot está vacío o no tiene el derecho, la operación devuelve
//! [`IpcError::Capability`] con el error de capacidad concreto. Ninguna
//! operación entra en pánico por un fallo de permisos.
//!
//! El estado global (tabla de puertos + colas) vive en una estructura fija, sin
//! heap. El kernel es mono-CPU y ejecuta con interrupciones deshabilitadas, por
//! lo que un `UnsafeCell` con `unsafe impl Sync` documentado serializa el
//! acceso. No se destruyen puertos en A6: así una capacidad nunca puede quedar
//! apuntando a un objeto reciclado.

use core::cell::UnsafeCell;
use core::fmt;

use crate::cap::{CapError, CapTable, ObjectId, Rights};
use crate::proc::ProcessId;

/// Identificador de puerto. Comparte espacio con [`ObjectId`].
pub type PortId = ObjectId;

/// Número máximo de puertos vivos.
pub const MAX_PORTS: usize = 8;
/// Profundidad de la cola FIFO de cada puerto.
pub const QUEUE_DEPTH: usize = 4;
/// Tamaño máximo del payload de un mensaje.
pub const MESSAGE_MAX_BYTES: usize = 32;

/// Mensaje pequeño y determinista, copiado por valor entre procesos.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Message {
    /// Etiqueta libre elegida por el emisor (tipo/protocolo del mensaje).
    pub tag: u64,
    len: u8,
    payload: [u8; MESSAGE_MAX_BYTES],
}

impl Message {
    /// Mensaje vacío (tag 0, sin payload).
    pub const fn empty() -> Self {
        Self {
            tag: 0,
            len: 0,
            payload: [0; MESSAGE_MAX_BYTES],
        }
    }

    /// Construye un mensaje con `tag` y copia como máximo
    /// [`MESSAGE_MAX_BYTES`] bytes de `bytes`.
    pub fn with_bytes(tag: u64, bytes: &[u8]) -> Result<Self, IpcError> {
        if bytes.len() > MESSAGE_MAX_BYTES {
            return Err(IpcError::MessageTooLarge);
        }
        let mut message = Self::empty();
        message.tag = tag;
        message.len = bytes.len() as u8;
        let mut index = 0;
        while index < bytes.len() {
            message.payload[index] = bytes[index];
            index += 1;
        }
        Ok(message)
    }

    /// Longitud efectiva del payload.
    pub fn len(&self) -> usize {
        self.len as usize
    }

    /// Vista del payload con la longitud efectiva.
    pub fn bytes(&self) -> &[u8] {
        &self.payload[..self.len as usize]
    }
}

/// Metadatos públicos del puerto devueltos por [`invoke`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PortInfo {
    pub id: PortId,
    pub owner: ProcessId,
    pub pending: usize,
}

/// Errores de las operaciones IPC.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IpcError {
    /// La tabla de capacidades rechazó la operación (slot vacío o sin derecho).
    Capability(CapError),
    /// El objeto referenciado no es un puerto vivo.
    NoSuchPort,
    /// No quedan puertos libres en la tabla del kernel.
    NoPorts,
    /// La cola del puerto está llena.
    QueueFull,
    /// La cola del puerto está vacía.
    QueueEmpty,
    /// El payload supera [`MESSAGE_MAX_BYTES`].
    MessageTooLarge,
}

impl fmt::Display for IpcError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Capability(error) => write!(f, "capacidad: {error}"),
            Self::NoSuchPort => write!(f, "puerto inexistente"),
            Self::NoPorts => write!(f, "sin puertos libres"),
            Self::QueueFull => write!(f, "cola de puerto llena"),
            Self::QueueEmpty => write!(f, "cola de puerto vacía"),
            Self::MessageTooLarge => write!(f, "mensaje demasiado grande"),
        }
    }
}

/// Objeto puerto: propietario lógico + cola circular de mensajes.
#[derive(Clone, Copy)]
struct Port {
    id: PortId,
    owner: ProcessId,
    in_use: bool,
    head: usize,
    tail: usize,
    count: usize,
    queue: [Message; QUEUE_DEPTH],
}

impl Port {
    const fn empty() -> Self {
        Self {
            id: 0,
            owner: 0,
            in_use: false,
            head: 0,
            tail: 0,
            count: 0,
            queue: [Message::empty(); QUEUE_DEPTH],
        }
    }

    const fn new(id: PortId, owner: ProcessId) -> Self {
        Self {
            id,
            owner,
            in_use: true,
            head: 0,
            tail: 0,
            count: 0,
            queue: [Message::empty(); QUEUE_DEPTH],
        }
    }

    fn enqueue(&mut self, message: Message) -> Result<(), IpcError> {
        if self.count == QUEUE_DEPTH {
            return Err(IpcError::QueueFull);
        }
        self.queue[self.tail] = message;
        self.tail = (self.tail + 1) % QUEUE_DEPTH;
        self.count += 1;
        Ok(())
    }

    fn dequeue(&mut self) -> Result<Message, IpcError> {
        if self.count == 0 {
            return Err(IpcError::QueueEmpty);
        }
        let message = self.queue[self.head];
        self.head = (self.head + 1) % QUEUE_DEPTH;
        self.count -= 1;
        Ok(message)
    }

    fn info(&self) -> PortInfo {
        PortInfo {
            id: self.id,
            owner: self.owner,
            pending: self.count,
        }
    }
}

/// Estado global del subsistema IPC.
struct Ipc {
    ports: [Port; MAX_PORTS],
}

impl Ipc {
    const fn new() -> Self {
        Self {
            ports: [Port::empty(); MAX_PORTS],
        }
    }

    fn create_port(&mut self, owner: ProcessId) -> Result<PortId, IpcError> {
        for index in 0..MAX_PORTS {
            if !self.ports[index].in_use {
                let id = index as PortId;
                self.ports[index] = Port::new(id, owner);
                return Ok(id);
            }
        }
        Err(IpcError::NoPorts)
    }

    fn port(&self, id: PortId) -> Result<&Port, IpcError> {
        let port = self.ports.get(id as usize).ok_or(IpcError::NoSuchPort)?;
        if port.in_use {
            Ok(port)
        } else {
            Err(IpcError::NoSuchPort)
        }
    }

    fn port_mut(&mut self, id: PortId) -> Result<&mut Port, IpcError> {
        let port = self
            .ports
            .get_mut(id as usize)
            .ok_or(IpcError::NoSuchPort)?;
        if port.in_use {
            Ok(port)
        } else {
            Err(IpcError::NoSuchPort)
        }
    }
}

/// Envoltorio global con acceso secuencial (kernel mono-CPU).
struct GlobalIpc(UnsafeCell<Ipc>);

// SAFETY: el kernel es mono-CPU, arranca con interrupciones deshabilitadas y
// ninguna función llama a otra del subsistema mientras mantiene un préstamo
// `&mut` (no hay reentrancia). Es el mismo patrón documentado en `mm::buddy`.
unsafe impl Sync for GlobalIpc {}

static IPC: GlobalIpc = GlobalIpc(UnsafeCell::new(Ipc::new()));

fn with_mut<R>(f: impl FnOnce(&mut Ipc) -> R) -> R {
    // SAFETY: ver el comentario de `GlobalIpc`.
    unsafe { f(&mut *IPC.0.get()) }
}

/// Crea un puerto propiedad lógica de `owner`.
///
/// La creación la hace el kernel (o un servicio con permisos de administración
/// en el futuro); un proceso normal recibe después una capacidad derivada.
pub fn create_port(owner: ProcessId) -> Result<PortId, IpcError> {
    with_mut(|ipc| ipc.create_port(owner))
}

/// Envía `message` al puerto referenciado por `caps[slot]` (derecho `SEND`).
///
/// Devuelve el id del puerto de destino para diagnósticos.
pub fn send(caps: &CapTable, slot: usize, message: Message) -> Result<PortId, IpcError> {
    let capability = caps
        .check(slot, Rights::SEND)
        .map_err(IpcError::Capability)?;
    with_mut(|ipc| {
        let port = ipc.port_mut(capability.object)?;
        port.enqueue(message)?;
        Ok(port.id)
    })
}

/// Recibe el mensaje más antiguo del puerto referenciado por `caps[slot]`
/// (derecho `READ`).
pub fn recv(caps: &CapTable, slot: usize) -> Result<Message, IpcError> {
    let capability = caps
        .check(slot, Rights::READ)
        .map_err(IpcError::Capability)?;
    with_mut(|ipc| ipc.port_mut(capability.object)?.dequeue())
}

/// Vacía la cola del puerto referenciado por `caps[slot]` (derecho `WRITE`).
/// Devuelve cuántos mensajes se descartaron.
pub fn clear_queue(caps: &CapTable, slot: usize) -> Result<usize, IpcError> {
    let capability = caps
        .check(slot, Rights::WRITE)
        .map_err(IpcError::Capability)?;
    with_mut(|ipc| {
        let port = ipc.port_mut(capability.object)?;
        let removed = port.count;
        port.head = 0;
        port.tail = 0;
        port.count = 0;
        Ok(removed)
    })
}

/// Invoca/consulta el puerto referenciado por `caps[slot]` (derecho
/// `EXECUTE`) y devuelve sus metadatos.
pub fn invoke(caps: &CapTable, slot: usize) -> Result<PortInfo, IpcError> {
    let capability = caps
        .check(slot, Rights::EXECUTE)
        .map_err(IpcError::Capability)?;
    with_mut(|ipc| ipc.port(capability.object).map(Port::info))
}

pub mod selftest;
