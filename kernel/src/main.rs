//! Stub de arranque del kernel de Meridian (tarea A2).
//!
//! En A2 el objetivo es demostrar que el bootloader puede cargar y ejecutar el
//! kernel y entregarle la estructura `BootInfo` con el mapa de memoria UEFI y
//! la información del framebuffer GOP. A3 añadirá la consola completa del
//! kernel (COM1 + GOP); aquí solo se emite un diagnóstico mínimo por COM1 y se
//! detiene la CPU.

#![no_main]
#![no_std]

use core::fmt::{self, Write};
use core::panic::PanicInfo;

use meridian_bootinfo::{BootInfo, BOOTINFO_MAGIC};

const COM1: u16 = 0x3F8;

macro_rules! kprintln {
    () => {
        $crate::_print(format_args!("\r\n"))
    };
    ($($arg:tt)*) => {
        $crate::_print(format_args!("{}\r\n", format_args!($($arg)*)))
    };
}

// El puntero lo entrega el bootloader y apunta a memoria válida; la firma forma
// parte del contrato de arranque, no de una API pública normal.
#[allow(clippy::not_unsafe_ptr_arg_deref)]
#[no_mangle]
pub extern "sysv64" fn _start(boot_info: *const BootInfo) -> ! {
    serial_init();

    kprintln!();
    kprintln!("MERIDIAN-KERNEL: entered (A2 stub)");

    if boot_info.is_null() {
        kprintln!("MERIDIAN-KERNEL: ERROR: BootInfo nulo");
        halt()
    }

    // SAFETY: el bootloader pasa un puntero a una estructura BootInfo válida en
    // memoria física que no se reutiliza antes de este punto.
    let info = unsafe { &*boot_info };

    if info.magic != BOOTINFO_MAGIC || !info.is_valid() {
        kprintln!(
            "MERIDIAN-KERNEL: ERROR: BootInfo inválido (magic=0x{:016X})",
            info.magic
        );
        halt()
    }

    kprintln!(
        "MERIDIAN-KERNEL: BootInfo v{} ({} bytes) OK",
        info.version,
        info.size
    );
    kprintln!(
        "MERIDIAN-KERNEL: memory map entries={} desc_size={} desc_version={}",
        info.memory_map.entry_count(),
        info.memory_map.descriptor_size,
        info.memory_map.descriptor_version
    );
    kprintln!(
        "MERIDIAN-KERNEL: GOP {}x{} stride={} format={} fb=0x{:016X}",
        info.framebuffer.width,
        info.framebuffer.height,
        info.framebuffer.stride,
        info.framebuffer.pixel_format,
        info.framebuffer.base
    );
    kprintln!(
        "MERIDIAN-KERNEL: image base=0x{:016X} size={} entry=0x{:016X}",
        info.kernel_base,
        info.kernel_size,
        info.kernel_entry
    );
    kprintln!("MERIDIAN-KERNEL: halt (A3 añadirá consola serie/GOP)");

    halt()
}

fn halt() -> ! {
    loop {
        // SAFETY: HLT no accede a memoria; con interrupciones habilitadas la CPU
        // despertará y volverá a este bucle.
        unsafe {
            core::arch::asm!("hlt", options(nomem, nostack, preserves_flags));
        }
    }
}

#[panic_handler]
fn panic(_info: &PanicInfo<'_>) -> ! {
    kprintln!("MERIDIAN-KERNEL: PANIC");
    halt()
}

// --- UART 16550 (COM1) -----------------------------------------------------

fn serial_init() {
    // 115200 baudios, 8N1, sin FIFO. QEMU no depende del divisor, pero así el
    // stub también funciona en un 16550 real.
    unsafe {
        outb(COM1 + 1, 0x00); // deshabilitar interrupciones
        outb(COM1 + 3, 0x80); // DLAB on
        outb(COM1, 0x01); // divisor = 1 (registro DLL)
        outb(COM1 + 1, 0x00);
        outb(COM1 + 3, 0x03); // 8 bits, sin paridad, 1 stop
        outb(COM1 + 2, 0xC7); // FIFO
        outb(COM1 + 4, 0x0B); // DTR/RTS/OUT2
    }
}

struct Serial;

impl Serial {
    fn write_byte(&mut self, byte: u8) {
        // Espera a que el registro de transmisión esté vacío.
        for _ in 0..100_000 {
            // SAFETY: lectura estándar del registro LSR del 16550.
            if unsafe { inb(COM1 + 5) } & 0x20 != 0 {
                break;
            }
            core::hint::spin_loop();
        }
        // SAFETY: escritura estándar del registro TX del 16550.
        unsafe {
            outb(COM1, byte);
        }
    }
}

impl Write for Serial {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for byte in s.bytes() {
            self.write_byte(byte);
        }
        Ok(())
    }
}

fn _print(args: fmt::Arguments<'_>) {
    let mut serial = Serial;
    let _ = serial.write_fmt(args);
}

unsafe fn outb(port: u16, value: u8) {
    // SAFETY: el llamador garantiza que el puerto es válido para escritura.
    unsafe {
        core::arch::asm!(
            "out dx, al",
            in("dx") port,
            in("al") value,
            options(nomem, nostack, preserves_flags)
        );
    }
}

unsafe fn inb(port: u16) -> u8 {
    let value: u8;
    // SAFETY: el llamador garantiza que el puerto es válido para lectura.
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
