//! Kernel de Meridian — consola de arranque (A3) y memoria física/virtual (A4).
//!
//! El bootloader entrega un `BootInfo` con el mapa de memoria UEFI final y la
//! descripción del framebuffer GOP. El kernel valida la estructura, inicializa
//! la consola COM1+GOP, resume el mapa, construye el asignador buddy y las
//! tablas de página propias con W^X, y ejecuta las auto-pruebas de A4.
//!
//! Mensajes de aceptación: `Meridian booting` (A3) y los marcadores
//! `A4: buddy alloc/free tests PASS` / `A4: W^X check PASS` (A4).

#![no_main]
#![no_std]

use core::fmt::Write;
use core::panic::PanicInfo;

use meridian_bootinfo::{BootInfo, PAGE_SIZE};

mod console;
mod framebuffer;
mod memory;

// Los módulos de MM vivieron originalmente en `kernel/mm/` (había un
// `.gitkeep` esperando esta tarea), fuera de `src/`; se reutiliza ese directorio
// sin mover la estructura del crate.
#[path = "../mm/mod.rs"]
mod mm;

use console::{Console, Serial, OK, TEXT, WARN};
use memory::MemorySummary;
use mm::buddy::{self, MAX_ORDER};
use mm::{paging, selftest, Reservations};

const KIB: u64 = 1024;

/// Punto de entrada SysV64 invocado por el bootloader.
///
/// El puntero apunta a una página `LOADER_DATA` reservada por el bootloader y
/// sigue siendo válido durante toda la vida del kernel; el mapa de memoria
/// apunta a otra zona `LOADER_DATA` que tampoco debe liberarse.
#[allow(clippy::not_unsafe_ptr_arg_deref)]
#[no_mangle]
pub extern "sysv64" fn _start(boot_info: *const BootInfo) -> ! {
    if boot_info.is_null() {
        serial_fatal("Meridian booting\r\nMERIDIAN-KERNEL: ERROR: BootInfo nulo");
    }

    // SAFETY: el contrato de arranque garantiza que el primer argumento apunta
    // a un `BootInfo` válido en memoria física legible. La comprobación de
    // firma y versión se hace a continuación antes de usar el resto de campos.
    let info = unsafe { &*boot_info };

    if !info.is_valid() {
        let mut serial = Serial::new();
        serial.write_crlf();
        let _ = writeln!(
            serial,
            "Meridian booting\r\nMERIDIAN-KERNEL: ERROR: BootInfo inválido (magic=0x{:016X}, version={})",
            info.magic, info.version
        );
        halt();
    }

    let mut console = Console::new(&info.framebuffer);

    // Criterio A3: el mismo mensaje por serie y por GOP.
    console.banner("Meridian booting");

    console.set_scale(2);
    console.set_foreground(OK);
    console.println("Kernel console active: COM1 (0x3F8) + GOP framebuffer");
    console.set_foreground(TEXT);
    console.println("");

    let _ = writeln!(
        console,
        "BootInfo: v{} ({} bytes) validated",
        info.version, info.size
    );

    print_framebuffer(&mut console, info);
    print_memory_map(&mut console, info);
    print_kernel_image(&mut console, info);

    console.println("");

    // --- A4: memoria física, paginación y W^X -----------------------------
    let rsp: u64;
    // SAFETY: solo se lee el puntero de pila; no se accede a memoria ni se
    // modifica el estado de la CPU.
    unsafe {
        core::arch::asm!("mov {}, rsp", out(reg) rsp, options(nomem, nostack, preserves_flags));
    }

    let reservations = Reservations::new(info, boot_info as u64, rsp);
    let summary = match buddy::init(&info.memory_map, &reservations.ranges) {
        Ok(stats) => stats,
        Err(err) => a4_fatal(&mut console, &format_args!("buddy init: {err}")),
    };
    let _ = writeln!(
        console,
        "A4: buddy init OK (orders 0..{MAX_ORDER}, {} free pages / {} MiB, {} reserved/skipped)",
        summary.free_pages,
        summary.free_pages * PAGE_SIZE / (1024 * 1024),
        summary.skipped_pages
    );
    let _ = writeln!(
        console,
        "A4: reserved ({} ranges): low 1 MiB, kernel image, BootInfo, memory map, GOP framebuffer, firmware stack",
        reservations.count
    );

    let root = match paging::init_and_switch(info, &reservations, boot_info as u64) {
        Ok(root) => root,
        Err(err) => a4_fatal(&mut console, &format_args!("paging init: {err}")),
    };
    let _ = writeln!(
        console,
        "A4: paging OK (PML4=0x{root:016X}, identity 0..4 GiB, 2 MiB default + 4 KiB per section)"
    );

    if !selftest::run(&mut console) {
        a4_fatal(&mut console, &format_args!("auto-pruebas A4 fallidas"));
    }

    console.set_foreground(OK);
    console.println("Console ready. Halting CPU.");

    halt()
}

/// Informa de un fallo de A4 por consola (y por serie) y detiene la CPU.
fn a4_fatal(console: &mut Console, reason: &core::fmt::Arguments<'_>) -> ! {
    console.set_foreground(WARN);
    let _ = writeln!(console, "A4: INIT FAIL: {reason}");
    let _ = writeln!(console, "A4: SELFTEST FAILED - halting CPU");
    halt()
}

fn print_framebuffer(console: &mut Console, info: &BootInfo) {
    let fb = &info.framebuffer;
    if console.framebuffer_active() {
        let _ = writeln!(
            console,
            "GOP: {}x{} stride={} format={} base=0x{:016X}",
            fb.width,
            fb.height,
            fb.stride,
            pixel_format_name(fb.pixel_format),
            fb.base
        );
        let _ = writeln!(
            console,
            "     framebuffer={} KiB visible={}x{} ({} KiB)",
            fb.size / KIB,
            console.screen_width(),
            console.screen_height(),
            (console.screen_width() * console.screen_height() * 4) as u64 / KIB
        );
    } else {
        console.set_foreground(WARN);
        let _ = writeln!(
            console,
            "GOP: no utilizable (base=0x{:016X}, format={}, size={} KiB); solo COM1",
            fb.base,
            pixel_format_name(fb.pixel_format),
            fb.size / KIB
        );
        console.set_foreground(TEXT);
    }
}

fn print_memory_map(console: &mut Console, info: &BootInfo) {
    let map = &info.memory_map;
    if !map.is_sane() {
        console.set_foreground(WARN);
        let _ = writeln!(
            console,
            "Memory map: ERROR (buffer=0x{:016X}, size={}, desc_size={})",
            map.buffer, map.size, map.descriptor_size
        );
        console.set_foreground(TEXT);
        return;
    }

    let summary = MemorySummary::from_map(map);
    let _ = writeln!(
        console,
        "Memory map: {} descriptors (desc_size={}, version={})",
        summary.descriptors, map.descriptor_size, map.descriptor_version
    );
    let _ = writeln!(
        console,
        "  usable: {} MiB ({} pages)  loader: {} pages",
        summary.usable_mib(),
        summary.usable_pages,
        summary.loader_pages
    );
    let _ = writeln!(
        console,
        "  boot: {} pages  runtime: {} pages  ACPI: {} pages",
        summary.boot_services_pages, summary.runtime_pages, summary.acpi_pages
    );
    let _ = writeln!(
        console,
        "  mmio: {} pages  reserved: {} pages  other: {} pages",
        summary.mmio_pages, summary.reserved_pages, summary.other_pages
    );
}

fn print_kernel_image(console: &mut Console, info: &BootInfo) {
    let _ = writeln!(
        console,
        "Kernel image: base=0x{:016X} entry=0x{:016X} size={} KiB",
        info.kernel_base,
        info.kernel_entry,
        info.kernel_size / KIB
    );
}

fn pixel_format_name(raw: u32) -> &'static str {
    match raw {
        0 => "RGB",
        1 => "BGR",
        2 => "bitmask",
        3 => "BltOnly",
        _ => "unknown",
    }
}

/// Emite un mensaje de emergencia solo por COM1 antes de detener la CPU.
fn serial_fatal(message: &str) -> ! {
    let mut serial = Serial::new();
    serial.write_crlf();
    for ch in message.chars() {
        if ch == '\n' {
            serial.write_crlf();
        } else if ch != '\r' {
            serial.write_byte(if ch.is_ascii() { ch as u8 } else { b'?' });
        }
    }
    serial.write_crlf();
    halt()
}

fn halt() -> ! {
    loop {
        // SAFETY: HLT no accede a memoria. Si las interrupciones están
        // deshabilitadas (estado normal tras el handoff), la CPU queda parada.
        unsafe {
            core::arch::asm!("hlt", options(nomem, nostack, preserves_flags));
        }
    }
}

#[panic_handler]
fn panic(panic_info: &PanicInfo<'_>) -> ! {
    let mut serial = Serial::new();
    serial.write_crlf();
    let _ = writeln!(serial, "MERIDIAN-KERNEL: PANIC");
    if let Some(location) = panic_info.location() {
        let _ = writeln!(
            serial,
            "  at {}:{}:{}",
            location.file(),
            location.line(),
            location.column()
        );
    }
    halt()
}
