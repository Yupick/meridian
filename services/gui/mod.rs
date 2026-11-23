//! Compositor y gestor de ventanas para Meridian T4.1.
//!.
//! Dibuja un escritorio minimalista con exactamente 2 ventanas, zona de
//! enfoque y orden Z. Solo usa `fill_rect` y `draw_text` del framebuffer;
//! no tiene heap, ni nuevas dependencias.

#![allow(dead_code)]

use crate::console::Console;
use crate::framebuffer::{Rgb, Framebuffer};

pub mod selftest;

/// Color de fondo del escritorio.
const DESKTOP_BG: Rgb = Rgb::new(10, 16, 32);
/// Color de la barra de título activa.
const TITLE_ACTIVE: Rgb = Rgb::new(0, 200, 220);
/// Color de la barra de título inactiva.
const TITLE_INACTIVE: Rgb = Rgb::new(60, 70, 90);
/// Color del borde de la ventana activa.
const BORDER_ACTIVE: Rgb = Rgb::new(0, 200, 220);
/// Color del borde de la ventana inactiva.
const BORDER_INACTIVE: Rgb = Rgb::new(50, 60, 80);
/// Color del texto de la ventana.
const WINDOW_TEXT: Rgb = Rgb::new(196, 208, 226);

/// Altura fija de la barra de título en píxeles.
const TITLE_BAR_H: usize = 22;

/// Una ventana del compositor.
#[derive(Clone)]
pub struct Window<'a> {
    /// Título de la ventana.
    title: &'a str,
    /// Rectángulo de la ventana (x, y, ancho, alto) relativo a la pantalla.
    rect: (usize, usize, usize, usize),
    /// Si la ventana tiene el foco actual.
    focused: bool,
    /// Líneas de contenido de la ventana.
    lines: [&'a str; 6],
}

impl<'a> Window<'a> {
    /// Crea una nueva ventana con título, rectángulo y contenido.
    const fn new(title: &'a str, rect: (usize, usize, usize, usize), lines: [&'a str; 6]) -> Self {
        Self {
            title,
            rect,
            focused: false,
            lines,
        }
    }

    /// Devuelve el rectángulo de la ventana.
    pub fn rect(&self) -> (usize, usize, usize, usize) {
        self.rect
    }

    /// Cambia el estado de foco.
    pub fn set_focused(&mut self, focused: bool) {
        self.focused = focused;
    }

    /// Título de la ventana.
    pub fn title(&self) -> &str {
        self.title
    }

    /// Número de líneas no vacías entre `lines[0..6]`.
    fn non_empty_count(&self) -> usize {
        let mut n = 0;
        for i in 0..6 {
            if !self.lines[i].is_empty() { n = i + 1; } else { break; }
        }
        n
    }

    /// Dibuja esta ventana en el framebuffer (fondo, borde, título y contenido).
    fn draw(&self, fb: &mut Framebuffer, _screen_w: usize, _screen_h: usize) {
        let (x, y, w, h) = self.rect();
        let border_color = if self.focused { BORDER_ACTIVE } else { BORDER_INACTIVE };
        let title_color = if self.focused { TITLE_ACTIVE } else { TITLE_INACTIVE };

        // Fondo de la ventana (dentro del borde).
        let inner_x = x + 2;
        let inner_y = y + TITLE_BAR_H;
        let inner_w = w.saturating_sub(4);
        let inner_h = h.saturating_sub(TITLE_BAR_H);
        if inner_w > 0 && inner_h > 0 {
            fb.fill_rect(inner_x, inner_y, inner_w, inner_h, DESKTOP_BG);
        }

        // Borde exterior: rectángulo de 2 píxeles.
        if w > 2 && h > 2 {
            fb.fill_rect(x, y, w, 2, border_color);           // arriba
            fb.fill_rect(x, y + h - 2, w, 2, border_color);   // abajo
            fb.fill_rect(x, y, 2, h, border_color);           // izquierda
            fb.fill_rect(x + w - 2, y, 2, h, border_color);   // derecha
        }

        // Barra de título: TITLE_BAR_H píxeles altos encima del contenido.
        if h > TITLE_BAR_H {
            fb.fill_rect(x + 2, y + 2, w - 4, TITLE_BAR_H - 4, title_color);
        }

        // Título centrado en la barra.
        let title_y = y + 2 + (TITLE_BAR_H - 8) / 2;
        if self.title.len() > 0 {
            let _ = fb.draw_text(x + 4, title_y, self.title, 2, WINDOW_TEXT);
        }

        // Contenido: cada línea espaciada 10 píxeles aproximadamente.
        let nc = self.non_empty_count();
        for i in 0..nc {
            let line = self.lines[i];
            let content_y = inner_y + i * 10;
            if content_y + 8 > inner_y + inner_h { break; }
            let _ = fb.draw_text(x + 4, content_y, line, 2, WINDOW_TEXT);
        }
    }
}

/// Compositor fijo con exactamente 2 ventanas y orden Z.
///
/// Ventana 0 siempre está detrás (Z-order inferior) y la ventana 1 delante.
/// El foco puede alternarse entre ellas con `focus_next()`.
pub struct Compositor<'a> {
    windows: [Window<'a>; 2],
    /// Índice de la ventana con foco actual (0 o 1).
    focused_index: usize,
}

impl<'a> Compositor<'a> {
    /// Ventana 0 ("System Monitor"): información estática, sin foco.
    fn monitor_window(screen_w: usize, screen_h: usize) -> Window<'a> {
        let rect = (0, screen_h / 4, screen_w / 2, screen_h / 2);
        let lines = ["resolución", "stride/formato", "COM1 115200", "Meridian Desktop T4.1", "", ""];
        Window::new("System Monitor", rect, lines)
    }

    /// Crea el compositor con las 2 ventanas del escritorio T4.1.
    ///
    /// Ventana 0: "System Monitor", información estática.
    /// Ventana 1: "Meridian Terminal", barra de título y demo/prompt.
    /// Los rectángulos son proporcionales al tamaño de la pantalla.
    pub fn new(screen_w: usize, screen_h: usize) -> Self {
        let win0 = Self::monitor_window(screen_w, screen_h);

        // Ventana 1: Meridian Terminal - ocupa la mitad derecha.
        let win1_rect = (screen_w / 2, screen_h / 4, screen_w / 2, screen_h / 2);
        let win1_lines = ["meridian:/$", "", "", "", "", ""];
        let win1 = Window::new("Meridian Terminal", win1_rect, win1_lines);

        Self {
            windows: [win0, win1],
            focused_index: 0,
        }
    }

    /// Crea el compositor para la terminal interactiva (T4.3).
    ///
    /// Usa las 6 líneas indicadas en la ventana "Meridian Terminal" y la deja
    /// enfocada; "System Monitor" conserva la información estática de T4.1.
    pub fn new_terminal(screen_w: usize, screen_h: usize, lines: [&'a str; 6]) -> Self {
        let mut win0 = Self::monitor_window(screen_w, screen_h);
        win0.set_focused(false);

        let win1_rect = (screen_w / 2, screen_h / 4, screen_w / 2, screen_h / 2);
        let mut win1 = Window::new("Meridian Terminal", win1_rect, lines);
        win1.set_focused(true);

        Self {
            windows: [win0, win1],
            focused_index: 1,
        }
    }

    /// Devuelve el índice de la ventana con foco actual.
    pub fn focused_index(&self) -> usize {
        self.focused_index
    }

    /// Alterna el foco entre las 2 ventanas (0 -> 1 -> 0).
    pub fn focus_next(&mut self) {
        self.focused_index = (self.focused_index + 1) % 2;
        self.windows[self.focused_index].set_focused(true);
        self.windows[1 - self.focused_index].set_focused(false);
    }

    /// Presenta el escritorio completo dibujando todas las ventanas en el framebuffer.
    /// Esta es la última operación de dibujo antes de capturar la pantalla.
    pub fn present(&mut self, fb: &mut Framebuffer) {
        // Limpiar framebuffer con color de escritorio.
        fb.clear(DESKTOP_BG);

        // Dibujar cada ventana usando el tamaño real del framebuffer.
        let screen_w = fb.width();
        let screen_h = fb.height();
        for window in &self.windows {
            window.draw(fb, screen_w, screen_h);
        }
    }
}

/// Presentación del escritorio T4.1.
///
/// Dibuja el compositor con 2 ventanas en el framebuffer de la consola.
/// Esta función es la operación de dibujo final antes de que el kernel haga halt.
pub fn present(console: &mut Console) {
    // Verificar si el framebuffer está activo (préstamo compartido, no mutable).
    if !console.framebuffer_active() {
        return;
    }

    // Tomar el framebuffer mutable para dibujar.
    let fb_opt = console.framebuffer_mut();
    let Some(fb) = fb_opt else { return };

    // Crear el compositor y dibujar el escritorio.
    // El préstamo mutable de `fb` se mantiene durante esta función;
    // al retornar, el préstamo se libera automáticamente.
    let mut compositor = Compositor::new(fb.width(), fb.height());
    compositor.present(fb);
}

/// Presentación del escritorio con la terminal interactiva (T4.3).
///
/// Dibuja el mismo escritorio de T4.1, pero la ventana "Meridian Terminal"
/// muestra las 6 líneas indicadas y aparece enfocada. Sin heap.
pub fn present_terminal<'a>(console: &mut Console, lines: &[&'a str; 6]) {
    if !console.framebuffer_active() {
        return;
    }

    let Some(fb) = console.framebuffer_mut() else { return };

    let mut compositor = Compositor::new_terminal(fb.width(), fb.height(), *lines);
    compositor.present(fb);
}