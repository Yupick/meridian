//! Driver de GPU en espacio de usuario (T4.2).
//!
//!Este servicio simula el límite de un driver en espacio de usuario con una
//!fachada de capacidades y un `GpuCommand` validado antes de tocar el
//!framebuffer. Los fallos del driver se convierten en `GpuError`, reinician el
//!estado del driver y devuelven el control al kernel; nunca invocan `panic!`
//!ni `halt()`. El estado global usa el patrón `UnsafeCell` + `unsafe impl Sync`
//!del resto del kernel (mono-CPU, IRQs desactivadas).

use core::cell::UnsafeCell;
use core::fmt;

use crate::cap::{CapError, CapTable, ObjectId, Rights};
use crate::console::Console;
use crate::framebuffer::Rgb;

pub mod selftest;

/// Objeto de capacidad que representa al driver de GPU.
pub const GPU_OBJECT_ID: ObjectId = 0x0102;
/// Slot de capacidad recomendado para el driver en sesiones.
pub const SESSION_GPU_SLOT: usize = 2;

/// Comandos aceptados por la fachada del driver.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GpuCommand {
    /// Consulta de estado; no modifica el framebuffer.
    Query,
    /// Limpieza completa con un color RGB packed `0x00RRGGBB`.
    Clear { color: u32 },
    /// Relleno de un rectángulo en coordenadas de píxel.
    FillRect {
        x: u16,
        y: u16,
        width: u16,
        height: u16,
        color: u32,
    },
    /// Fallo controlado del driver: debe reiniciarse sin tumbar el kernel.
    InjectFault,
}

/// Errores del driver de GPU.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GpuError {
    NotInitialized,
    AlreadyInitialized,
    Capability(CapError),
    WrongObject,
    InvalidCommand,
    DriverFault(&'static str),
}

impl fmt::Display for GpuError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotInitialized => write!(f, "driver de GPU no inicializado"),
            Self::AlreadyInitialized => write!(f, "driver de GPU ya inicializado"),
            Self::Capability(error) => write!(f, "capacidad: {error}"),
            Self::WrongObject => write!(f, "capacidad no corresponde al driver de GPU"),
            Self::InvalidCommand => write!(f, "comando de GPU inválido"),
            Self::DriverFault(message) => write!(f, "fallo de driver: {message}"),
        }
    }
}

impl From<CapError> for GpuError {
    fn from(error: CapError) -> Self {
        Self::Capability(error)
    }
}

/// Estadísticas públicas del driver tras cualquier operación.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GpuStats {
    pub width: u16,
    pub height: u16,
    pub generation: u16,
    pub restarts: u16,
    pub accepted: u16,
    pub rejected: u16,
    pub faults: u16,
}

impl fmt::Display for GpuStats {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "gen={} restarts={} accepted={} rejected={} faults={} gop={}x{}",
            self.generation,
            self.restarts,
            self.accepted,
            self.rejected,
            self.faults,
            self.width,
            self.height
        )
    }
}

struct State {
    width: u16,
    height: u16,
    generation: u16,
    restarts: u16,
    accepted: u16,
    rejected: u16,
    faults: u16,
}

struct GlobalGpu(UnsafeCell<Option<State>>);

// SAFETY: kernel mono-CPU con interrupciones desactivadas; todas las
// operaciones son secuenciales y no hay reentrancia (mismo modelo que
// `services/blk` y `services/fs`).
unsafe impl Sync for GlobalGpu {}

static GPU: GlobalGpu = GlobalGpu(UnsafeCell::new(None));

fn with_state<R>(f: impl FnOnce(&mut Option<State>) -> R) -> R {
    // SAFETY: ver `GlobalGpu`.
    unsafe { f(&mut *GPU.0.get()) }
}

fn stats(state: &State) -> GpuStats {
    GpuStats {
        width: state.width,
        height: state.height,
        accepted: state.accepted,
        rejected: state.rejected,
        faults: state.faults,
        generation: state.generation,
        restarts: state.restarts,
    }
}

fn check_cap(caps: &CapTable, slot: usize, required: Rights) -> Result<(), GpuError> {
    let capability = caps.check(slot, required)?;
    if capability.object != GPU_OBJECT_ID {
        return Err(GpuError::WrongObject);
    }
    Ok(())
}

fn validate(command: GpuCommand, width: u16, height: u16) -> Result<(), GpuError> {
    match command {
        GpuCommand::Query | GpuCommand::Clear { .. } | GpuCommand::InjectFault => Ok(()),
        GpuCommand::FillRect {
            x,
            y,
            width: w,
            height: h,
            ..
        } => {
            if w == 0 || h == 0 || width == 0 || height == 0 {
                return Err(GpuError::InvalidCommand);
            }
            let x_end = x as u32 + w as u32;
            let y_end = y as u32 + h as u32;
            if x >= width || y >= height || x_end > width as u32 || y_end > height as u32 {
                return Err(GpuError::InvalidCommand);
            }
            Ok(())
        }
    }
}

fn to_rgb(color: u32) -> Rgb {
    Rgb::new(
        ((color >> 16) & 0xff) as u8,
        ((color >> 8) & 0xff) as u8,
        (color & 0xff) as u8,
    )
}

/// Inicializa el driver con las dimensiones del framebuffer.
pub fn init(width: u16, height: u16) -> Result<GpuStats, GpuError> {
    with_state(|global| {
        if global.is_some() {
            return Err(GpuError::AlreadyInitialized);
        }
        let state = State {
            width,
            height,
            generation: 1,
            restarts: 0,
            accepted: 0,
            rejected: 0,
            faults: 0,
        };
        let result = stats(&state);
        *global = Some(state);
        Ok(result)
    })
}

/// Devuelve una copia de las estadísticas actuales.
pub fn stats_snapshot() -> Result<GpuStats, GpuError> {
    with_state(|global| global.as_ref().map(stats).ok_or(GpuError::NotInitialized))
}

/// Envía un comando al driver. Las órdenes de dibujo requieren `WRITE`; la
/// inyección de fallo requiere `EXECUTE`. Un fallo del driver incrementa las
/// estadísticas, reinicia su generación y devuelve el error al llamador sin
/// detener el kernel.
pub fn submit(
    caps: &CapTable,
    slot: usize,
    command: GpuCommand,
    console: &mut Console,
) -> Result<GpuStats, GpuError> {
    with_state(|global| {
        let state = global.as_mut().ok_or(GpuError::NotInitialized)?;

        let required = match command {
            GpuCommand::InjectFault => Rights::EXECUTE,
            _ => Rights::WRITE,
        };
        if let Err(error) = check_cap(caps, slot, required) {
            state.rejected += 1;
            return Err(error);
        }

        if let Err(error) = validate(command, state.width, state.height) {
            state.rejected += 1;
            return Err(error);
        }

        match command {
            GpuCommand::Query => {
                state.accepted += 1;
                Ok(stats(state))
            }
            GpuCommand::Clear { color } => {
                if let Some(fb) = console.framebuffer_mut() {
                    fb.clear(to_rgb(color));
                }
                state.accepted += 1;
                Ok(stats(state))
            }
            GpuCommand::FillRect {
                x,
                y,
                width: w,
                height: h,
                color,
            } => {
                if let Some(fb) = console.framebuffer_mut() {
                    fb.fill_rect(x as usize, y as usize, w as usize, h as usize, to_rgb(color));
                }
                state.accepted += 1;
                Ok(stats(state))
            }
            GpuCommand::InjectFault => {
                state.faults += 1;
                state.restarts += 1;
                state.generation = state.generation.wrapping_add(1);
                Err(GpuError::DriverFault("inyeccion controlada"))
            }
        }
    })
}
