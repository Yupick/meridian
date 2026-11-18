//! Dibujo directo sobre el framebuffer GOP.
//!
//! Se compone la pantalla en un buffer `BltPixel` y se envía con una única
//! operación `BufferToVideo`. De este modo no hace falta conocer el formato
//! exacto del framebuffer ni usar punteros crudos: el firmware GOP hace la
//! conversión de color.

use alloc::vec;
use alloc::vec::Vec;

use font8x8::{UnicodeFonts, BASIC_FONTS};
use uefi::proto::console::gop::{BltOp, BltPixel, BltRegion, GraphicsOutput};
use uefi::Result;

/// Colores usados por la pantalla de arranque.
pub const BACKGROUND: BltPixel = BltPixel::new(10, 16, 32);
pub const ACCENT: BltPixel = BltPixel::new(0, 200, 220);
pub const TITLE: BltPixel = BltPixel::new(245, 248, 255);
pub const SUBTITLE: BltPixel = BltPixel::new(110, 220, 255);
pub const TEXT: BltPixel = BltPixel::new(196, 208, 226);
pub const OK: BltPixel = BltPixel::new(120, 255, 170);

/// Lienzo en memoria con la resolución visible del modo GOP actual.
pub struct Canvas {
    width: usize,
    height: usize,
    pixels: Vec<BltPixel>,
}

impl Canvas {
    /// Crea un lienzo relleno del color de fondo.
    pub fn new(width: usize, height: usize, background: BltPixel) -> Self {
        Self {
            width,
            height,
            pixels: vec![background; width * height],
        }
    }

    /// Rellena un rectángulo.
    pub fn fill_rect(&mut self, x: usize, y: usize, width: usize, height: usize, color: BltPixel) {
        let x_end = x.saturating_add(width).min(self.width);
        let y_end = y.saturating_add(height).min(self.height);
        for py in y..y_end {
            for px in x..x_end {
                self.pixels[py * self.width + px] = color;
            }
        }
    }

    /// Dibuja texto ASCII con la fuente 8x8 y el factor de escala indicado.
    ///
    /// Los caracteres no presentes en la fuente (por ejemplo, acentos) se
    /// sustituyen por un espacio para no romper el alineado.
    pub fn text(&mut self, x: usize, y: usize, text: &str, scale: usize, color: BltPixel) {
        let mut cursor_x = x;
        let mut cursor_y = y;
        for ch in text.chars() {
            if ch == '\n' {
                cursor_x = x;
                cursor_y += 9 * scale;
                continue;
            }
            let glyph = BASIC_FONTS.get(ch).unwrap_or([0u8; 8]);
            for (row, bits) in glyph.iter().enumerate() {
                for col in 0..8 {
                    if bits & (1 << col) != 0 {
                        self.fill_rect(
                            cursor_x + col * scale,
                            cursor_y + row * scale,
                            scale,
                            scale,
                            color,
                        );
                    }
                }
            }
            cursor_x += 8 * scale;
        }
    }

    /// Dibuja texto centrado horizontalmente.
    pub fn centered_text(&mut self, y: usize, text: &str, scale: usize, color: BltPixel) {
        let text_width = text.chars().count() * 8 * scale;
        let x = self.width.saturating_sub(text_width) / 2;
        self.text(x, y, text, scale, color);
    }

    /// Envía el lienzo completo al framebuffer mediante una operación GOP.
    pub fn flush(&self, gop: &mut GraphicsOutput) -> Result {
        gop.blt(BltOp::BufferToVideo {
            buffer: &self.pixels,
            src: BltRegion::Full,
            dest: (0, 0),
            dims: (self.width, self.height),
        })
    }
}

/// Calcula la mayor escala que hace caber `char_count` caracteres en `max_width`.
pub fn scale_to_fit(max_width: usize, char_count: usize, max_scale: usize) -> usize {
    if char_count == 0 {
        return 1;
    }
    (max_width / (char_count * 8)).clamp(1, max_scale.max(1))
}
