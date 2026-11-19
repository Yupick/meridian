//! Servicio de bloques sobre virtio-blk (T1.2).
//!
//! Expone lectura/escritura de sectores de 512 bytes con control de
//! capacidades: `BLOCK_OBJECT_ID` requiere `Rights::READ` para leer y
//! `Rights::WRITE` para escribir. El disco 0 (arranque) es siempre de solo
//! lectura; la política `Policy::writable_devices` habilita la escritura en el
//! disco 1 (QA). El estado global usa el patrón `UnsafeCell` + `unsafe impl
//! Sync` del resto del kernel (mono-CPU, IRQs desactivadas).

use core::cell::UnsafeCell;
use core::fmt;

use crate::cap::{CapError, CapTable, ObjectId, Rights};
use crate::drivers::virtio_blk::{self, BlkError as DriverError, Device};

pub mod selftest;

/// Tamaño de sector del servicio.
pub const SECTOR_SIZE: usize = 512;
/// Objeto de capacidad que representa al servicio de bloques.
pub const BLOCK_OBJECT_ID: ObjectId = 0x0101;
/// Slot de capacidad recomendado para el servicio de bloques en sesiones.
pub const SESSION_BLOCK_SLOT: usize = 1;

/// Identificador de dispositivo: 0 = disco de arranque, 1 = disco de QA.
pub type BlockDeviceId = u8;

/// Errores del servicio de bloques.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlockError {
    NotInitialized,
    AlreadyInitialized,
    Capability(CapError),
    WrongObject,
    NoDevice(BlockDeviceId),
    ReadOnlyDevice,
    InvalidSector,
    /// Reservado: la API usa búferes de tamaño fijo, así que hoy no se produce.
    #[allow(dead_code)]
    InvalidBuffer,
    QueueFull,
    Timeout,
    IoStatus(u8),
    UnexpectedDescriptor(u32),
    Init(&'static str),
}

impl fmt::Display for BlockError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotInitialized => write!(f, "servicio de bloques no inicializado"),
            Self::AlreadyInitialized => write!(f, "servicio de bloques ya inicializado"),
            Self::Capability(error) => write!(f, "capacidad: {error}"),
            Self::WrongObject => write!(f, "capacidad no corresponde al servicio de bloques"),
            Self::NoDevice(id) => write!(f, "dispositivo de bloques {id} no presente"),
            Self::ReadOnlyDevice => write!(f, "dispositivo de solo lectura"),
            Self::InvalidSector => write!(f, "sector fuera del dispositivo"),
            Self::InvalidBuffer => write!(f, "búfer inválido"),
            Self::QueueFull => write!(f, "cola virtio llena"),
            Self::Timeout => write!(f, "timeout esperando la petición virtio-blk"),
            Self::IoStatus(status) => write!(f, "estado de E/S virtio-blk {status:#04x}"),
            Self::UnexpectedDescriptor(id) => {
                write!(f, "descriptor virtio inesperado {id}")
            }
            Self::Init(message) => write!(f, "{message}"),
        }
    }
}

impl From<CapError> for BlockError {
    fn from(error: CapError) -> Self {
        Self::Capability(error)
    }
}

/// Política de escritura: bit `n` autoriza escribir en el dispositivo `n`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Policy {
    pub writable_devices: u8,
}

/// Estadística pública del servicio tras `init`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BlockStats {
    pub devices: usize,
    pub sectors: [u64; 2],
    pub queue_size: u16,
}

/// IDs PCI detectados (vendor, device) para cada dispositivo.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DeviceIds {
    pub vendor: u16,
    pub device: u16,
}

struct State {
    policy: Policy,
    devices: [Option<Device>; 2],
    count: usize,
    sectors: [u64; 2],
    queue_size: u16,
    ids: [DeviceIds; 2],
}

struct GlobalBlock(UnsafeCell<Option<State>>);

// SAFETY: kernel mono-CPU con interrupciones desactivadas; todas las operaciones
// son secuenciales y no hay reentrancia (mismo modelo que `fs`/`mm::buddy`).
unsafe impl Sync for GlobalBlock {}

static BLOCK: GlobalBlock = GlobalBlock(UnsafeCell::new(None));

fn with_state<R>(f: impl FnOnce(&mut Option<State>) -> R) -> R {
    // SAFETY: ver `GlobalBlock`.
    unsafe { f(&mut *BLOCK.0.get()) }
}

fn map_driver(error: DriverError) -> BlockError {
    match error {
        DriverError::Init(message) => BlockError::Init(message),
        DriverError::QueueFull => BlockError::QueueFull,
        DriverError::Timeout => BlockError::Timeout,
        DriverError::IoStatus(status) => BlockError::IoStatus(status),
        DriverError::UnexpectedDescriptor(id) => BlockError::UnexpectedDescriptor(id),
    }
}

fn check_cap(caps: &CapTable, slot: usize, required: Rights) -> Result<(), BlockError> {
    let capability = caps.check(slot, required)?;
    if capability.object != BLOCK_OBJECT_ID {
        return Err(BlockError::WrongObject);
    }
    Ok(())
}

/// Inicializa el servicio: escanea PCI, negocia virtio y configura la cola.
pub fn init(policy: Policy) -> Result<BlockStats, BlockError> {
    with_state(|global| {
        if global.is_some() {
            return Err(BlockError::AlreadyInitialized);
        }
        let detected = virtio_blk::init_all().map_err(map_driver)?;

        let mut devices = [None, None];
        let mut sectors = [0u64; 2];
        let mut ids = [DeviceIds::default(); 2];
        let mut queue_size = 0u16;
        let mut index = 0usize;
        while index < detected.count {
            let device = detected.devices[index].ok_or(BlockError::Init("missing device"))?;
            sectors[index] = device.sectors;
            queue_size = device.queue_size;
            ids[index] = DeviceIds {
                vendor: device.vendor,
                device: device.device_id,
            };
            devices[index] = Some(device);
            index += 1;
        }

        *global = Some(State {
            policy,
            devices,
            count: detected.count,
            sectors,
            queue_size,
            ids,
        });
        Ok(BlockStats {
            devices: detected.count,
            sectors,
            queue_size,
        })
    })
}

/// Devuelve las estadísticas actuales.
pub fn stats() -> Result<BlockStats, BlockError> {
    with_state(|global| {
        let state = global.as_ref().ok_or(BlockError::NotInitialized)?;
        Ok(BlockStats {
            devices: state.count,
            sectors: state.sectors,
            queue_size: state.queue_size,
        })
    })
}

/// IDs PCI de los dispositivos detectados (`(0, 0)` si el slot está vacío).
pub fn detected_device_ids() -> [DeviceIds; 2] {
    with_state(|global| match global {
        Some(state) => state.ids,
        None => [DeviceIds::default(); 2],
    })
}

/// BDF `(bus, slot, func)` de un dispositivo detectado.
pub fn device_bdf(device: BlockDeviceId) -> Option<(u8, u8, u8)> {
    with_state(|global| {
        let index = device as usize;
        global.as_ref().and_then(|state| {
            if index < state.count {
                state.devices[index].map(|dev| (dev.bdf.bus, dev.bdf.slot, dev.bdf.func))
            } else {
                None
            }
        })
    })
}

/// Número de sectores de 512 bytes de un dispositivo (0 si no existe).
pub fn sector_count(device: BlockDeviceId) -> u64 {
    with_state(|global| match global {
        Some(state) if (device as usize) < state.count => state.sectors[device as usize],
        _ => 0,
    })
}

/// Lee un sector de 512 bytes. Exige `Rights::READ` sobre `BLOCK_OBJECT_ID`.
pub fn read_sector(
    caps: &CapTable,
    slot: usize,
    device: BlockDeviceId,
    sector: u64,
    out: &mut [u8; SECTOR_SIZE],
) -> Result<(), BlockError> {
    with_state(|global| {
        let state = global.as_mut().ok_or(BlockError::NotInitialized)?;
        check_cap(caps, slot, Rights::READ)?;
        let index = device as usize;
        if index >= state.count || state.devices[index].is_none() {
            return Err(BlockError::NoDevice(device));
        }
        if sector >= state.sectors[index] {
            return Err(BlockError::InvalidSector);
        }
        state.devices[index]
            .as_mut()
            .ok_or(BlockError::NoDevice(device))?
            .read_sector(sector, out)
            .map_err(map_driver)
    })
}

/// Escribe un sector de 512 bytes. Exige `Rights::WRITE` sobre
/// `BLOCK_OBJECT_ID` y que la política autorice el dispositivo.
///
/// El disco 0 se rechaza siempre con [`BlockError::ReadOnlyDevice`], incluso si
/// la capacidad concede `WRITE`.
pub fn write_sector(
    caps: &CapTable,
    slot: usize,
    device: BlockDeviceId,
    sector: u64,
    data: &[u8; SECTOR_SIZE],
) -> Result<(), BlockError> {
    with_state(|global| {
        let state = global.as_mut().ok_or(BlockError::NotInitialized)?;
        check_cap(caps, slot, Rights::WRITE)?;
        let index = device as usize;
        if index >= state.count || state.devices[index].is_none() {
            return Err(BlockError::NoDevice(device));
        }
        if device == 0 || (state.policy.writable_devices & (1u8 << device)) == 0 {
            return Err(BlockError::ReadOnlyDevice);
        }
        if sector >= state.sectors[index] {
            return Err(BlockError::InvalidSector);
        }
        state.devices[index]
            .as_mut()
            .ok_or(BlockError::NoDevice(device))?
            .write_sector(sector, data)
            .map_err(map_driver)
    })
}
