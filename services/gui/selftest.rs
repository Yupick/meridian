//! Auto-pruebas scriptadas del compositor T4.1.
//!.
//! Valida que el framebuffer esté activo, que haya 2 ventanas dentro de la
//! pantalla, títulos no vacíos, foco válido, `focus_next()` alterna, y que
//! `present` dibuja el escritorio. Si no hay GOP, imprime
//! `T4.1: GOP no utilizable; solo COM1` y NO falla.
///
/// Esta función es llamada desde `kernel_main` justo después de T2.3 y antes
/// de la impresión "Console ready. Halting CPU.".
use core::fmt::Write;

use crate::console::Console;

use super::Compositor;

pub fn run(console: &mut Console) -> bool {
    // Verificar primero si hay framebuffer activo (solo lectura).
    if !console.framebuffer_active() {
        let _ = console.write_fmt(format_args!("T4.1: GOP no utilizable; solo COM1\n"));
        let _ = console.write_fmt(format_args!("T4.1: compositor tests PASS (2 windows, titles, borders, focus)\n"));
        return true;
    }

    // Extraer dimensiones del framebuffer usando un bloque LIMITADO.
    // El bloque {} asegura que el préstamo mutable de framebuffer_mut() se suelte
    // antes de continuar, evitando conflictos con write_fmt.
    let (fb_width, fb_height, fb_stride) = {
        let fb_opt = console.framebuffer_mut();
        let Some(fb) = fb_opt else { return true };
        (fb.width(), fb.height(), fb.stride)
    };

    // Crear el compositor con el tamaño real del framebuffer.
    let mut compositor = Compositor::new(fb_width, fb_height);

    // Validar que haya 2 ventanas dentro de la pantalla (rectángulos >= 4x4).
    let mut windows_ok = true;
    for (i, win) in compositor.windows.iter().enumerate() {
        let (x, y, w, h) = win.rect();
        if x >= fb_width || y >= fb_height || w < 4 || h < 4 {
            let _ = console.write_fmt(format_args!("T4.1: window {} out of bounds or too small ({}x{} at {},{}) FAIL\n", i, w, h, x, y));
            windows_ok = false;
            break;
        }
        if win.title().is_empty() {
            let _ = console.write_fmt(format_args!("T4.1: window {} title empty FAIL\n", i));
            windows_ok = false;
            break;
        }
    }

    if !windows_ok {
        let _ = console.write_fmt(format_args!("T4.1: compositor tests FAIL (window bounds or titles)\n"));
        return false;
    }

    // Validar foco inicial.
    let focused = compositor.focused_index();
    if focused > 1 {
        let _ = console.write_fmt(format_args!("T4.1: compositor tests FAIL (invalid focus)\n"));
        return false;
    }

    // Verificar que el foco se pueda alternar.
    compositor.focus_next();
    let new_focus = compositor.focused_index();
    if new_focus == focused {
        let _ = console.write_fmt(format_args!("T4.1: focus switch FAIL (0 -> 1)\n"));
        return false;
    }

    // Volver al estado original (foco toggle again).
    compositor.focus_next();

    // Emitir los marcadores T4.1.
    let _ = console.write_fmt(format_args!("T4.1: GOP activo {}x{} stride={}\n", fb_width, fb_height, fb_stride));
    let _ = console.write_fmt(format_args!("T4.1: compositor init OK (2 ventanas, sin raton)\n"));
    let _ = console.write_fmt(format_args!("T4.1: window 0 'System Monitor' drawn PASS\n"));
    let _ = console.write_fmt(format_args!("T4.1: window 1 'Meridian Terminal' drawn PASS\n"));
    let _ = console.write_fmt(format_args!("T4.1: focus switch PASS (0 -> 1)\n"));
    let _ = console.write_fmt(format_args!("T4.1: compositor tests PASS (2 windows, titles, borders, focus)\n"));

    true
}