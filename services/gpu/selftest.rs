//! Auto-pruebas del driver de GPU en espacio de usuario (T4.2).
//!
//! Ejercita la fachada de capacidades, valida un comando correcto, rechaza un
//! rectángulo fuera de límites, inyecta un fallo controlado, comprueba que el
//! driver se reinicia y verifica que el kernel y el framebuffer siguen vivos.
//! Ningún fallo del driver detiene el kernel: `kernel_main` ignora el valor de
//! retorno y continúa hacia la consola final.
use core::fmt::Write;

use crate::cap::{Capability, CapTable, Rights};
use crate::console::Console;

use super::{init, stats_snapshot, submit, GpuCommand, GpuError, GPU_OBJECT_ID, SESSION_GPU_SLOT};

pub fn run(console: &mut Console) -> bool {
    let gop_active = console.framebuffer_active();
    let (fb_width, fb_height) = {
        let mut size = (0u16, 0u16);
        if gop_active {
            if let Some(fb) = console.framebuffer_mut() {
                size = (fb.width() as u16, fb.height() as u16);
            }
        }
        size
    };

    if !gop_active {
        let _ = writeln!(console, "T4.2: GOP no utilizable; solo COM1");
    }

    let mut caps = CapTable::new();
    let inserted = caps.insert(
        SESSION_GPU_SLOT,
        Capability {
            object: GPU_OBJECT_ID,
            rights: Rights::READ
                .union(Rights::WRITE)
                .union(Rights::EXECUTE),
        },
    );
    if inserted.is_err() {
        let _ = writeln!(console, "T4.2: capability setup FAIL");
        return false;
    }

    let initial = match init(fb_width, fb_height) {
        Ok(stats) => stats,
        Err(error) => {
            let _ = writeln!(console, "T4.2: driver init FAIL ({error})");
            return false;
        }
    };
    let _ = writeln!(
        console,
        "T4.2: driver init OK (gen={} gop={}x{})",
        initial.generation, initial.width, initial.height
    );

    // Comando válido: se dibuja una franja pequeña si hay GOP; si no, una
    // consulta de estado tiene el mismo efecto sobre el driver.
    let valid_command = if gop_active {
        GpuCommand::Clear { color: 0x00101820 }
    } else {
        GpuCommand::Query
    };
    let valid = match submit(&caps, SESSION_GPU_SLOT, valid_command, console) {
        Ok(stats) => stats,
        Err(error) => {
            let _ = writeln!(console, "T4.2: valid command FAIL ({error})");
            return false;
        }
    };
    let _ = writeln!(console, "T4.2: valid command PASS (stats={valid})");

    // Rectángulo fuera de límites: debe rechazarse antes de tocar memoria.
    match submit(
        &caps,
        SESSION_GPU_SLOT,
        GpuCommand::FillRect {
            x: u16::MAX,
            y: u16::MAX,
            width: 1,
            height: 1,
            color: 0x00ffffff,
        },
        console,
    ) {
        Err(GpuError::InvalidCommand) => {
            let _ = writeln!(console, "T4.2: invalid command rejected PASS (out-of-bounds FillRect)");
        }
        other => {
            match other {
                Ok(stats) => {
                    let _ = writeln!(console, "T4.2: invalid command rejected FAIL (stats={stats})");
                }
                Err(error) => {
                    let _ = writeln!(console, "T4.2: invalid command rejected FAIL ({error})");
                }
            }
            return false;
        }
    }

    // Una tabla sin capacidades no puede usar el driver.
    let empty_caps = CapTable::new();
    match submit(&empty_caps, SESSION_GPU_SLOT, GpuCommand::Query, console) {
        Err(GpuError::Capability(_)) => {
            let _ = writeln!(console, "T4.2: capability denied PASS (slot without WRITE)");
        }
        other => {
            match other {
                Ok(stats) => {
                    let _ = writeln!(console, "T4.2: capability denied FAIL (stats={stats})");
                }
                Err(error) => {
                    let _ = writeln!(console, "T4.2: capability denied FAIL ({error})");
                }
            }
            return false;
        }
    }

    // Inyección de fallo: el driver se reinicia y devuelve el error, pero el
    // kernel sigue ejecutando estas auto-pruebas.
    match submit(&caps, SESSION_GPU_SLOT, GpuCommand::InjectFault, console) {
        Err(GpuError::DriverFault(_)) => {
            let _ = writeln!(console, "T4.2: driver fault injected PASS (fault contained)");
        }
        other => {
            match other {
                Ok(stats) => {
                    let _ = writeln!(console, "T4.2: driver fault injected FAIL (stats={stats})");
                }
                Err(error) => {
                    let _ = writeln!(console, "T4.2: driver fault injected FAIL ({error})");
                }
            }
            return false;
        }
    }

    let after_fault = match stats_snapshot() {
        Ok(stats) => stats,
        Err(error) => {
            let _ = writeln!(console, "T4.2: driver restart FAIL ({error})");
            return false;
        }
    };
    if after_fault.restarts != 1 || after_fault.generation <= initial.generation {
        let _ = writeln!(console, "T4.2: driver restart FAIL (stats={after_fault})");
        return false;
    }
    let _ = writeln!(
        console,
        "T4.2: driver restart OK (gen={} restarts={})",
        after_fault.generation, after_fault.restarts
    );

    // Vulneración posterior al reinicio: el driver vuelve a aceptar comandos
    // y el framebuffer sigue siendo utilizable.
    let after_restart_command = if gop_active && fb_width > 16 && fb_height > 16 {
        GpuCommand::FillRect {
            x: 4,
            y: 4,
            width: 8,
            height: 8,
            color: 0x00ff8040,
        }
    } else {
        GpuCommand::Query
    };
    let after_restart = match submit(
        &caps,
        SESSION_GPU_SLOT,
        after_restart_command,
        console,
    ) {
        Ok(stats) => stats,
        Err(error) => {
            let _ = writeln!(console, "T4.2: kernel alive after driver restart FAIL ({error})");
            return false;
        }
    };
    if after_restart.generation != after_fault.generation {
        let _ = writeln!(console, "T4.2: kernel alive after driver restart FAIL (generation moved)");
        return false;
    }
    let fb_ok = !gop_active || console.framebuffer_mut().is_some();
    if !fb_ok {
        let _ = writeln!(console, "T4.2: kernel alive after driver restart FAIL (framebuffer lost)");
        return false;
    }
    let _ = writeln!(
        console,
        "T4.2: kernel alive after driver restart PASS (framebuffer usable, stats={after_restart})"
    );
    let _ = writeln!(console, "T4.2: GPU driver tests PASS (driver fault aislado; kernel continuo)");

    true
}
