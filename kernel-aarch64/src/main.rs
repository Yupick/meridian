//! Kernel aarch64 mínimo de Meridian (T5.1).
//!
//! Objetivo de la tarea: arranque real en QEMU `virt`/OVMF aarch64 con handoff
//! UEFI -> BootInfo, consola UART PL011 y framebuffer GOP. Este HAL es deliberadamente
//! pequeño: no reutiliza los controladores PCI/virtio de x86-64 ni la paging A4.

#![no_main]
#![no_std]

use core::fmt::{self, Write};
use core::panic::PanicInfo;

use font8x8::legacy::BASIC_LEGACY;
use meridian_bootinfo::{BootInfo, FramebufferInfo, PixelFormat};

const UART_PL011: usize = 0x0900_0000;
const KIB: u64 = 1024;

// Entrada real en ensamblador: Rust generaría un prólogo que usaría la pila
// del firmware antes de que podamos cambiar SP.
core::arch::global_asm!(
    r#"
    .section .text.entry,"ax",@progbits
    .balign 16
    .global _start
    .type _start,@function
_start:
    ldr x1, =stack_top
    mov sp, x1
    mov x29, xzr
    mov x30, xzr
    bl kernel_entry
1:  b 1b
"#
);

/// x0 contiene `BootInfo` cuando arranca el bootloader UEFI y el DTB cuando
/// QEMU carga la imagen raw. La magia de `BootInfo` distingue ambos casos.
#[no_mangle]
pub extern "C" fn kernel_entry(handoff: *const BootInfo) -> ! {
    if handoff.is_null() {
        let mut serial = Serial::new();
        let _ = writeln!(serial, "T5.1: ERROR: handoff nulo");
        halt();
    }

    // SAFETY: x0 apunta a una estructura válida entregada por UEFI o a la DTB
    // que QEMU coloca en RAM antes de saltar al kernel.
    let magic = unsafe { core::ptr::read_unaligned(handoff.cast::<u64>()) };
    if magic == meridian_bootinfo::BOOTINFO_MAGIC {
        kernel_main(handoff)
    } else {
        kernel_main_dtb(handoff as u64)
    }
}

fn kernel_main_dtb(dtb: u64) -> ! {
    let mut serial = Serial::new();
    let _ = writeln!(serial, "Meridian booting");
    let _ = writeln!(serial, "T5.1: UART PL011 activo (0x09000000)");
    let _ = writeln!(
        serial,
        "T5.1: arranque directo QEMU aarch64 (DTB en 0x{dtb:016X})"
    );
    let _ = writeln!(serial, "T5.1: framebuffer GOP no direccionable (direct boot)");
    let _ = writeln!(serial, "T5.1: mapa UEFI no disponible (arranque directo)");
    let _ = writeln!(serial, "T5.1: HAL aarch64 OK; kernel vivo");
    let _ = writeln!(serial, "MERIDIAN AARCH64 BOOT OK");
    halt()
}

fn kernel_main(boot_info: *const BootInfo) -> ! {
    if boot_info.is_null() {
        halt();
    }
    // SAFETY: el bootloader entregó esta página reservada y no la reutiliza.
    let info = unsafe { &*boot_info };

    let mut serial = Serial::new();
    let mut screen = Screen::new(&info.framebuffer);

    macro_rules! out {
        ($($arg:tt)*) => {{
            let _ = writeln!(serial, $($arg)*);
            if let Some(ref mut display) = screen {
                let _ = writeln!(display, $($arg)*);
            }
        }};
    }

    out!("Meridian booting");
    out!("T5.1: UART PL011 activo (0x09000000)");

    if !info.is_valid() {
        out!(
            "T5.1: ERROR: BootInfo inválido (magic=0x{:016X}, version={})",
            info.magic,
            info.version
        );
        halt();
    }

    out!(
        "T5.1: BootInfo v{} validado ({} bytes, aarch64)",
        info.version,
        info.size
    );
    out!(
        "T5.1: kernel base=0x{:016X} entry=0x{:016X} size={} bytes",
        info.kernel_base,
        info.kernel_entry,
        info.kernel_size
    );

    let (fb_width, fb_height, fb_stride) = match &screen {
        Some(display) => (display.width(), display.height(), display.stride()),
        None => (0, 0, 0),
    };
    if fb_width > 0 {
        out!(
            "T5.1: framebuffer GOP activo {}x{} stride={}",
            fb_width,
            fb_height,
            fb_stride
        );
    } else {
        out!("T5.1: framebuffer GOP no direccionable (modo BltOnly)");
    }

    let mut usable_pages = 0u64;
    let entries = info.memory_map.entry_count();
    for index in 0..entries {
        // SAFETY: el bootloader conserva el buffer del mapa de memoria vivo.
        if let Some(descriptor) = unsafe { info.memory_map.descriptor(index) } {
            if descriptor.is_usable() {
                usable_pages = usable_pages.saturating_add(descriptor.number_of_pages);
            }
        }
    }
    let usable_mib = usable_pages.saturating_mul(meridian_bootinfo::PAGE_SIZE) / (KIB * KIB);
    out!(
        "T5.1: mapa UEFI OK ({entries} descriptores, {usable_pages} páginas / {usable_mib} MiB libres)"
    );

    out!("T5.1: HAL aarch64 OK; kernel vivo");
    out!("MERIDIAN AARCH64 BOOT OK");

    halt();
}

fn halt() -> ! {
    loop {
        core::hint::spin_loop();
    }
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    let mut serial = Serial::new();
    let _ = writeln!(serial, "T5.1: PANIC: {info}");
    halt();
}

/// UART PL011 del machine `virt` de QEMU.
struct Serial;

impl Serial {
    fn new() -> Self {
        Self
    }

    fn write_byte(&mut self, byte: u8) {
        // SAFETY: dirección MMIO fija del UART0 en la máquina `virt`.
        unsafe {
            let data = (UART_PL011 + 0x00) as *mut u32;
            core::ptr::write_volatile(data, byte as u32);
        }
    }
}

impl fmt::Write for Serial {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for byte in s.bytes() {
            self.write_byte(if byte.is_ascii() { byte } else { b'?' });
        }
        Ok(())
    }
}

/// Consola de texto directa sobre el framebuffer GOP entregado por UEFI.
struct Screen {
    base: *mut u32,
    width: usize,
    height: usize,
    stride: usize,
    format: PixelFormat,
    red_mask: u32,
    green_mask: u32,
    blue_mask: u32,
    x: usize,
    y: usize,
}

impl Screen {
    fn new(info: &FramebufferInfo) -> Option<Self> {
        if !info.is_directly_addressable() || !info.base.is_multiple_of(4) {
            return None;
        }
        let format = match info.pixel_format {
            value if value == PixelFormat::Rgb as u32 => PixelFormat::Rgb,
            value if value == PixelFormat::Bgr as u32 => PixelFormat::Bgr,
            value if value == PixelFormat::Bitmask as u32 => PixelFormat::Bitmask,
            _ => return None,
        };
        let width = usize::try_from(info.width).ok()?;
        let height = usize::try_from(info.height).ok()?;
        let stride = usize::try_from(info.stride).ok()?;
        if width == 0 || height == 0 || stride < width || width > 16_384 || height > 16_384 {
            return None;
        }
        let needed = (stride as u64)
            .checked_mul(height as u64)?
            .checked_mul(4)?;
        if info.size < needed {
            return None;
        }

        let mut screen = Self {
            base: info.base as *mut u32,
            width,
            height,
            stride,
            format,
            red_mask: info.red_mask,
            green_mask: info.green_mask,
            blue_mask: info.blue_mask,
            x: 32,
            y: 32,
        };
        screen.clear();
        Some(screen)
    }

    fn width(&self) -> usize {
        self.width
    }

    fn height(&self) -> usize {
        self.height
    }

    fn stride(&self) -> usize {
        self.stride
    }

    fn clear(&mut self) {
        for y in 0..self.height {
            for x in 0..self.width {
                self.put_pixel(x, y, self.encode(10, 16, 32));
            }
        }
    }

    fn encode(&self, red: u8, green: u8, blue: u8) -> u32 {
        match self.format {
            PixelFormat::Rgb => ((red as u32) << 16) | ((green as u32) << 8) | blue as u32,
            PixelFormat::Bgr => ((blue as u32) << 16) | ((green as u32) << 8) | red as u32,
            PixelFormat::Bitmask => {
                ((red as u32) << self.trailing_zeros(self.red_mask))
                    | ((green as u32) << self.trailing_zeros(self.green_mask))
                    | ((blue as u32) << self.trailing_zeros(self.blue_mask))
            }
            PixelFormat::BltOnly => 0,
        }
    }

    fn trailing_zeros(&self, mask: u32) -> u32 {
        if mask == 0 {
            0
        } else {
            mask.trailing_zeros()
        }
    }

    fn put_pixel(&mut self, x: usize, y: usize, color: u32) {
        if x >= self.width || y >= self.height {
            return;
        }
        // SAFETY: límites verificados y el firmware mapeó el framebuffer.
        unsafe {
            let offset = y * self.stride + x;
            core::ptr::write_volatile(self.base.add(offset), color);
        }
    }

    fn glyph(&mut self, ch: u8, color: u32) {
        let glyph = BASIC_LEGACY.get(ch as usize).copied().unwrap_or_else(|| {
            BASIC_LEGACY
                .get(b'?' as usize)
                .copied()
                .unwrap_or([0; 8])
        });
        let scale = 2usize;
        for row in 0..8 {
            for col in 0..8 {
                if glyph[row] & (0x80 >> col) == 0 {
                    continue;
                }
                for dy in 0..scale {
                    for dx in 0..scale {
                        self.put_pixel(self.x + col * scale + dx, self.y + row * scale + dy, color);
                    }
                }
            }
        }
        self.x += 8 * scale;
        if self.x + 8 * scale >= self.width {
            self.x = 32;
            self.y += 9 * scale;
        }
    }
}

impl fmt::Write for Screen {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for byte in s.bytes() {
            match byte {
                b'\n' => {
                    self.x = 32;
                    self.y += 20;
                }
                b'\r' => {}
                value if value >= 0x20 && value < 0x7f => {
                    self.glyph(value, self.encode(120, 255, 170));
                }
                _ => {}
            }
            if self.y + 16 >= self.height {
                return Ok(());
            }
        }
        Ok(())
    }
}
