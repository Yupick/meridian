//! Consola del kernel: salida simultánea por COM1 y por el framebuffer GOP.
//!
//! La consola mantiene un cursor de texto para el framebuffer y escribe los
//! mismos mensajes en el UART 16550 de COM1. No usa `alloc`: todo el estado
//! vive en la pila de `_start` y las líneas se formatean con `core::fmt`.

use core::fmt;

use meridian_bootinfo::FramebufferInfo;

use crate::framebuffer::{Framebuffer, Rgb};

/// Dirección del primer UART compatible 16550 en x86-64.
const COM1: u16 = 0x3F8;

pub const BACKGROUND: Rgb = Rgb::new(10, 16, 32);
pub const ACCENT: Rgb = Rgb::new(0, 200, 220);
pub const TITLE: Rgb = Rgb::new(245, 248, 255);
pub const TEXT: Rgb = Rgb::new(196, 208, 226);
pub const OK: Rgb = Rgb::new(120, 255, 170);
pub const WARN: Rgb = Rgb::new(255, 190, 120);

/// UART 16550 de COM1 en modo 115200 8N1.
pub struct Serial;

impl Serial {
    /// Inicializa el UART y devuelve el manejador de escritura.
    pub fn new() -> Self {
        // SAFETY: COM1 es el puerto estándar de la UART en el PC y QEMU con
        // `-serial` lo expone; las escrituras son las de inicialización 16550.
        unsafe {
            outb(COM1 + 1, 0x00); // deshabilitar interrupciones
            outb(COM1 + 3, 0x80); // DLAB on
            outb(COM1, 0x01); // divisor = 1 (115200 baudios)
            outb(COM1 + 1, 0x00);
            outb(COM1 + 3, 0x03); // 8 bits, sin paridad, 1 stop
            outb(COM1 + 2, 0xC7); // FIFO
            outb(COM1 + 4, 0x0B); // DTR/RTS/OUT2
        }
        Self
    }

    pub fn write_byte(&mut self, byte: u8) {
        // Espera a que el registro de transmisión esté vacío.
        for _ in 0..100_000 {
            // SAFETY: lectura del registro LSR del 16550 en COM1.
            if unsafe { inb(COM1 + 5) } & 0x20 != 0 {
                break;
            }
            core::hint::spin_loop();
        }
        // SAFETY: escritura del registro TX del 16550 en COM1.
        unsafe {
            outb(COM1, byte);
        }
    }

    pub fn write_crlf(&mut self) {
        self.write_byte(b'\r');
        self.write_byte(b'\n');
    }
}

impl fmt::Write for Serial {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for ch in s.chars() {
            self.write_byte(if ch.is_ascii() { ch as u8 } else { b'?' });
        }
        Ok(())
    }
}

/// Consola combinada serie + GOP.
pub struct Console {
    serial: Serial,
    framebuffer: Option<Framebuffer>,
    screen_width: usize,
    screen_height: usize,
    margin: usize,
    x: usize,
    y: usize,
    scale: usize,
    line_gap: usize,
    foreground: Rgb,
}

impl Console {
    /// Crea la consola a partir del framebuffer descrito en `BootInfo`.
    ///
    /// Si el framebuffer no es utilizable, la consola sigue funcionando por
    /// COM1. El lienzo se limpia y se dibujan las bandas decorativas.
    pub fn new(info: &FramebufferInfo) -> Self {
        let framebuffer = Framebuffer::from_boot_info(info);
        let (screen_width, screen_height) = match &framebuffer {
            Some(fb) => (fb.width(), fb.height()),
            None => (0, 0),
        };
        let margin = (screen_width / 16).max(24);

        let mut console = Self {
            serial: Serial::new(),
            framebuffer,
            screen_width,
            screen_height,
            margin,
            x: margin,
            y: margin,
            scale: 2,
            line_gap: 8,
            foreground: TEXT,
        };

        if let Some(fb) = &mut console.framebuffer {
            fb.clear(BACKGROUND);
            let band = (screen_height / 64).max(4);
            fb.fill_rect(0, 0, screen_width, band, ACCENT);
            fb.fill_rect(0, screen_height - band, screen_width, band, ACCENT);
        }

        console
    }

    pub fn framebuffer_active(&self) -> bool {
        self.framebuffer.is_some()
    }

    pub fn screen_width(&self) -> usize {
        self.screen_width
    }

    pub fn screen_height(&self) -> usize {
        self.screen_height
    }

    pub fn set_foreground(&mut self, color: Rgb) {
        self.foreground = color;
    }

    pub fn set_scale(&mut self, scale: usize) {
        self.scale = scale.max(1);
    }

    /// Escribe una línea completa en ambos canales.
    pub fn println(&mut self, text: &str) {
        self.write_text(text);
        self.newline();
    }

    /// Escribe texto sin añadir salto final.
    pub fn write_text(&mut self, text: &str) {
        for ch in text.chars() {
            match ch {
                '\n' => self.newline(),
                '\r' => {}
                _ => {
                    let byte = if ch.is_ascii() { ch as u8 } else { b'?' };
                    self.serial.write_byte(byte);

                    if self.framebuffer.is_some() {
                        let right = self.screen_width.saturating_sub(self.margin);
                        if self.x + 8 * self.scale > right {
                            self.newline();
                        }
                    }
                    if let Some(fb) = &mut self.framebuffer {
                        fb.draw_char(self.x, self.y, ch, self.scale, self.foreground);
                    }
                    self.x += 8 * self.scale;
                }
            }
        }
    }

    /// Dibuja el mensaje principal en grande, centrado y también por COM1.
    pub fn banner(&mut self, text: &str) {
        self.serial_write_line(text);
        self.serial.write_crlf();

        if let Some(fb) = &mut self.framebuffer {
            let max_width = self.screen_width * 8 / 10;
            let chars = text.chars().count().max(1);
            let scale = (max_width / (chars * 8)).clamp(2, 10);
            let text_width = chars * 8 * scale;
            let x = self.screen_width.saturating_sub(text_width) / 2;
            let y = (self.screen_height / 8).max(self.margin);
            fb.draw_text(x, y, text, scale, TITLE);

            let rule_y = y + 8 * scale + (self.margin / 2).max(6);
            fb.fill_rect(x, rule_y, text_width, 3, ACCENT);

            self.x = self.margin;
            self.y = rule_y + 2 * self.margin;
        } else {
            self.y = self.y.saturating_add(8 * self.scale + self.line_gap);
        }
    }

    /// Salto de línea: CRLF por serie y avance del cursor GOP.
    pub fn newline(&mut self) {
        self.serial.write_crlf();

        if self.framebuffer.is_some() {
            self.y += 8 * self.scale + self.line_gap;
            let bottom = self.screen_height.saturating_sub(self.margin);
            if self.y + 8 * self.scale > bottom {
                // No hay scroll todavía; vuelve al principio visible.
                self.y = self.margin;
            }
            self.x = self.margin;
        }
    }

    fn serial_write_line(&mut self, text: &str) {
        for ch in text.chars() {
            self.serial
                .write_byte(if ch.is_ascii() { ch as u8 } else { b'?' });
        }
    }
}

impl fmt::Write for Console {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        self.write_text(s);
        Ok(())
    }
}

#[inline]
unsafe fn outb(port: u16, value: u8) {
    // SAFETY: el llamador garantiza que el puerto admite escritura de 8 bits.
    unsafe {
        core::arch::asm!(
            "out dx, al",
            in("dx") port,
            in("al") value,
            options(nomem, nostack, preserves_flags)
        );
    }
}

#[inline]
unsafe fn inb(port: u16) -> u8 {
    let value: u8;
    // SAFETY: el llamador garantiza que el puerto admite lectura de 8 bits.
    unsafe {
        core::arch::asm!(
            "in al, dx",
            in("dx") port,
            out("al") value,
            options(nomem, nostack, preserves_flags)
        );
    }
    value
}
