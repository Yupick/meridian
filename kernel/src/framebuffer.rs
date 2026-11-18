//! Framebuffer GOP utilizado por la consola del kernel.
//!
//! El bootloader entrega en `BootInfo` la dirección física, el tamaño, el
//! stride y el formato de píxel del framebuffer que dejó activo. El kernel
//! escribe directamente en esa memoria (mapeo identidad heredado de UEFI);
//! no se usan los servicios GOP, que ya no están disponibles después de
//! `ExitBootServices()`.

use font8x8::legacy::BASIC_LEGACY;
use meridian_bootinfo::{FramebufferInfo, PixelFormat};

/// Color RGB de 8 bits por canal usado por el compositor de texto.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Rgb {
    pub red: u8,
    pub green: u8,
    pub blue: u8,
}

impl Rgb {
    pub const fn new(red: u8, green: u8, blue: u8) -> Self {
        Self { red, green, blue }
    }
}

/// Vista de escritura sobre el framebuffer lineal del firmware.
pub struct Framebuffer {
    /// Puntero al primer píxel. Solo es válido si `from_boot_info` aceptó la
    /// descripción: base no nula, alineada a 4 bytes y con tamaño suficiente.
    base: *mut u32,
    width: usize,
    height: usize,
    stride: usize,
    /// Número total de píxeles del búfer, incluido el padding del stride.
    len: usize,
    format: PixelFormat,
    red_mask: u32,
    green_mask: u32,
    blue_mask: u32,
    reserved_mask: u32,
}

impl Framebuffer {
    /// Valida la descripción GOP y crea la vista de escritura.
    ///
    /// Devuelve `None` si el framebuffer no es direccionable o si sus datos no
    /// son coherentes. En ese caso la consola sigue funcionando por COM1.
    pub fn from_boot_info(info: &FramebufferInfo) -> Option<Self> {
        if !info.is_directly_addressable() || !info.base.is_multiple_of(4) {
            return None;
        }

        let format = if info.pixel_format == PixelFormat::Rgb as u32 {
            PixelFormat::Rgb
        } else if info.pixel_format == PixelFormat::Bgr as u32 {
            PixelFormat::Bgr
        } else if info.pixel_format == PixelFormat::Bitmask as u32 {
            PixelFormat::Bitmask
        } else {
            return None;
        };

        let width = usize::try_from(info.width).ok()?;
        let height = usize::try_from(info.height).ok()?;
        let stride = usize::try_from(info.stride).ok()?;
        if width == 0 || height == 0 || stride < width {
            return None;
        }

        let len = stride.checked_mul(height)?;
        let required_bytes = (len as u64).checked_mul(4)?;
        if info.size < required_bytes {
            return None;
        }

        // Evita resoluciones absurdas que desbordarían la aritmética de dibujo.
        if width > 16_384 || height > 16_384 {
            return None;
        }

        Some(Self {
            // SAFETY (invariante): el firmware mapea el framebuffer en la
            // dirección física indicada y el rango [base, base + size) es
            // escribible. Se comprobó la alineación y el tamaño mínimo.
            base: info.base as *mut u32,
            width,
            height,
            stride,
            len,
            format,
            red_mask: info.red_mask,
            green_mask: info.green_mask,
            blue_mask: info.blue_mask,
            reserved_mask: info.reserved_mask,
        })
    }

    pub fn width(&self) -> usize {
        self.width
    }

    pub fn height(&self) -> usize {
        self.height
    }

    /// Rellena todo el framebuffer (incluido el padding de cada línea).
    pub fn clear(&mut self, color: Rgb) {
        let pixel = self.encode(color);
        for index in 0..self.len {
            // SAFETY: `index < len = stride * height` y la descripción validada
            // garantiza que el búfer tiene al menos `len` píxeles de 32 bits.
            unsafe {
                core::ptr::write_volatile(self.base.add(index), pixel);
            }
        }
    }

    /// Rellena un rectángulo, recortándolo a los límites visibles.
    pub fn fill_rect(&mut self, x: usize, y: usize, width: usize, height: usize, color: Rgb) {
        let pixel = self.encode(color);
        let x_end = x.saturating_add(width).min(self.width);
        let y_end = y.saturating_add(height).min(self.height);
        for py in y..y_end {
            for px in x..x_end {
                // SAFETY: px < width <= stride y py < height; el índice cae
                // dentro de la región validada del framebuffer.
                let index = py * self.stride + px;
                unsafe {
                    core::ptr::write_volatile(self.base.add(index), pixel);
                }
            }
        }
    }

    /// Dibuja texto ASCII con la fuente 8x8 y el factor de escala indicado.
    pub fn draw_text(&mut self, x: usize, y: usize, text: &str, scale: usize, color: Rgb) {
        let scale = scale.max(1);
        let mut cursor_x = x;
        for ch in text.chars() {
            self.draw_char(cursor_x, y, ch, scale, color);
            cursor_x += 8 * scale;
        }
    }

    /// Dibuja un carácter; los no ASCII se sustituyen por `?`.
    pub(crate) fn draw_char(&mut self, x: usize, y: usize, ch: char, scale: usize, color: Rgb) {
        let glyph = if ch.is_ascii() {
            BASIC_LEGACY[ch as usize]
        } else {
            BASIC_LEGACY[b'?' as usize]
        };
        for (row, bits) in glyph.iter().enumerate() {
            for col in 0..8 {
                if bits & (1 << col) != 0 {
                    self.fill_rect(x + col * scale, y + row * scale, scale, scale, color);
                }
            }
        }
    }

    /// Convierte un color a la palabra de 32 bits que espera el framebuffer.
    fn encode(&self, color: Rgb) -> u32 {
        match self.format {
            // PixelRedGreenBlueReserved8BitPerColor: R en el byte bajo.
            PixelFormat::Rgb => {
                color.red as u32 | (color.green as u32) << 8 | (color.blue as u32) << 16
            }
            // PixelBlueGreenRedReserved8BitPerColor: B en el byte bajo.
            PixelFormat::Bgr => {
                color.blue as u32 | (color.green as u32) << 8 | (color.red as u32) << 16
            }
            PixelFormat::Bitmask => {
                scale_to_mask(color.red, self.red_mask)
                    | scale_to_mask(color.green, self.green_mask)
                    | scale_to_mask(color.blue, self.blue_mask)
                    // El canal reservado se deja a 0 salvo que el firmware
                    // exija un valor concreto; OVMF no lo hace.
                    | if self.reserved_mask == 0 {
                        0
                    } else {
                        scale_to_mask(0, self.reserved_mask)
                    }
            }
            // `from_boot_info` rechaza este formato.
            PixelFormat::BltOnly => 0,
        }
    }
}

/// Escala un canal de 8 bits al ancho y posición indicados por una máscara.
fn scale_to_mask(value: u8, mask: u32) -> u32 {
    if mask == 0 {
        return 0;
    }
    let shift = mask.trailing_zeros();
    let bits = mask.count_ones();
    let max = if bits >= 32 {
        u32::MAX
    } else {
        (1u32 << bits) - 1
    };
    let scaled = ((value as u64 * max as u64 + 127) / 255) as u32;
    (scaled << shift) & mask
}
