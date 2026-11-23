//! Kernel de Meridian — consola (A3), memoria/paginación (A4), procesos
//! aislados (A5), IPC/capacidades (A6), servicio de archivos RAM (A7),
//! login/usuarios (A8), terminal alpha (A9), servicio de bloques (T1.2),
//! objetos en disco con copy-on-write (T1.3), volumen cifrado (T1.5),
//! pila de red sobre virtio-net (T2.1) y usuarios con permisos (T2.2).

#![no_main]
#![no_std]

use core::fmt::Write;
use core::panic::PanicInfo;
use core::sync::atomic::{AtomicPtr, Ordering};

use meridian_bootinfo::{BootInfo, PAGE_SIZE};

mod arch;
mod console;
mod framebuffer;
mod memory;
mod proc;

// Módulos de capacidades, IPC y MM en directorios propios (fuera de `src/`).
#[path = "../cap/mod.rs"]
mod cap;
#[path = "../ipc/mod.rs"]
mod ipc;
#[path = "../mm/mod.rs"]
mod mm;
// Controladores PCI/virtio-blk (T1.2).
#[path = "../drivers/mod.rs"]
mod drivers;
// Servicio de bloques sobre virtio-blk (T1.2).
#[path = "../../services/blk/mod.rs"]
mod blk;
// Objetos en disco con B+ tree, extents, COW y checksums (T1.3).
#[path = "../../services/obj/mod.rs"]
mod obj;
// Volumen cifrado XTS-AES-256 con clave Argon2id sobre objetos (T1.5).
#[path = "../../services/vol/mod.rs"]
mod vol;
// Pila de red sobre virtio-net (T2.1).
#[path = "../../services/net/mod.rs"]
mod net;
// Acceso remoto cifrado TLS 1.3 PSK (T2.3).
#[path = "../../services/tls/mod.rs"]
mod tls;
// Servicio de archivos en memoria (A7).
#[path = "../../services/fs/mod.rs"]
mod fs;
// Servicio de autenticación y usuarios (A8).
#[path = "../../services/auth/mod.rs"]
mod auth;
// Terminal/shell alpha (A9).
#[path = "../../services/term/mod.rs"]
mod term;
// Compositor y ventanas (T4.1).
#[path = "../../services/gui/mod.rs"] mod gui;
// Driver de GPU en espacio de usuario (T4.2).
#[path = "../../services/gpu/mod.rs"] mod gpu;
// Usuarios con permisos sobre objetos del volumen cifrado (T2.2).
#[path = "../../services/perm/mod.rs"]
pub mod perm;

use console::{Console, Serial, OK, TEXT, WARN};
use memory::MemorySummary;
use mm::buddy::{self, MAX_ORDER};
use mm::{paging, selftest, Reservations};

const KIB: u64 = 1024;

static BOOT_INFO_PTR: AtomicPtr<BootInfo> = AtomicPtr::new(core::ptr::null_mut());

/// Punto de entrada SysV64 invocado por el bootloader.
#[allow(clippy::not_unsafe_ptr_arg_deref)]
#[no_mangle]
pub extern "sysv64" fn _start(boot_info: *const BootInfo) -> ! {
    BOOT_INFO_PTR.store(boot_info as *mut BootInfo, Ordering::SeqCst);

    if boot_info.is_null() {
        serial_fatal("Meridian booting\r\nMERIDIAN-KERNEL: ERROR: BootInfo nulo");
    }

    // Inicializar GDT/TSS propios e IDT antes de cambiar de pila, para que
    // excepciones tempranas sean manejables con pila IST conocida.
    // SAFETY: arranque temprano, mono-CPU, memoria identidad-mapeada.
    unsafe {
        arch::x86_64::gdt::init();
        arch::x86_64::idt::init();
    }

    // Conmutar a la pila de kernel propia y continuar.
    // SAFETY: continuación válida que nunca retorna.
    unsafe {
        arch::x86_64::gdt::switch_to_kernel_stack_and_jump(kernel_main);
    }
}

extern "sysv64" fn kernel_main() -> ! {
    let boot_info = BOOT_INFO_PTR.load(Ordering::SeqCst) as *const BootInfo;
    if boot_info.is_null() {
        serial_fatal("Meridian booting\r\nMERIDIAN-KERNEL: ERROR: BootInfo perdido");
    }

    // SAFETY: puntero válido entregado por bootloader.
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
    let _ = writeln!(
        console,
        "Kernel stack: own stack active (top=0x{:016X})",
        arch::x86_64::gdt::kernel_stack_top()
    );

    console.println("");

    // --- A4: memoria física, paginación y W^X -----------------------------
    let rsp = read_rsp();
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
        "A4: reserved ({} ranges): low 1 MiB, kernel image, BootInfo, memory map, GOP framebuffer, stack(s)",
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

    // --- T1.2: servicio de bloques sobre virtio-blk -----------------------
    console.println("");
    let block_stats = match blk::init(blk::Policy {
        writable_devices: 0b0000_0010,
    }) {
        Ok(stats) => stats,
        Err(err) => t12_fatal(&mut console, &format_args!("blk init: {err}")),
    };
    let _ = writeln!(
        console,
        "T1.2: blk init OK (devices={}, sectors=[{}, {}], queue_size={})",
        block_stats.devices, block_stats.sectors[0], block_stats.sectors[1], block_stats.queue_size
    );
    if !blk::selftest::run(&mut console) {
        t12_fatal(&mut console, &format_args!("auto-pruebas T1.2 fallidas"));
    }

    // --- T1.3: objetos en disco (B+ tree, extents, COW y checksums) --------
    console.println("");
    if !obj::selftest::run(&mut console) {
        t13_fatal(&mut console);
    }

    // --- T1.5: volumen cifrado (XTS-AES-256 + Argon2id) -------------------
    console.println("");
    if !vol::selftest::run(&mut console) {
        t15_fatal(&mut console);
    }

    // --- T2.1: pila de red sobre virtio-net -------------------------------
    console.println("");
    if !net::selftest::run(&mut console) {
        t21_fatal(&mut console);
    }

    // --- A5: procesos y aislamiento de espacios de direcciones ------------
    console.println("");
    if !proc::selftest::run(&mut console) {
        a5_fatal(&mut console);
    }

    // --- A6: IPC por puertos y capacidades simples ------------------------
    console.println("");
    if !ipc::selftest::run(&mut console) {
        a6_fatal(&mut console);
    }

    // --- A7: servicio de archivos mínimo en RAM ---------------------------
    console.println("");
    if !fs::selftest::run(&mut console) {
        a7_fatal(&mut console);
    }

    // --- A8: servicio de login y usuarios ---------------------------------
    console.println("");
    if !auth::selftest::run(&mut console) {
        a8_fatal(&mut console);
    }

    // --- A9: terminal alpha -----------------------------------------------
    console.println("");
    if !term::selftest::run(&mut console) {
        a9_fatal(&mut console);
    }

    // --- T2.2: usuarios con permisos sobre el volumen cifrado --------------
    console.println("");
    if !perm::selftest::run(&mut console) {
        t22_fatal(&mut console);
    }

    // --- T2.3: acceso remoto cifrado (TLS 1.3 PSK sobre UDP) ---------------
    console.println("");
    if !tls::selftest::run(&mut console) {
        t23_fatal(&mut console);
    }

    // --- T4.1: compositor y ventanas ---
    if !gui::selftest::run(&mut console) {
        t41_fatal(&mut console);
    }

    // --- T4.2: driver de GPU en espacio de usuario ---
    // Un fallo del driver se informa y se reinicia, pero NO detiene el kernel.
    console.println("");
    let _ = gpu::selftest::run(&mut console);

    // --- T4.3: terminal interactiva (solo con la feature `interactive`) -----
    #[cfg(feature = "interactive")]
    {
        use crate::cap::{Capability, CapTable, Rights};
        use crate::fs::FS_OBJECT_ID;

        let _ = writeln!(console, "T4.3: terminal interactiva activa (COM1)");
        let _ = writeln!(console, "T4.3: login prompt listo");

        // Capacidades FS con las que `auth::login` resuelve el directorio home.
        let mut admin_caps = CapTable::new();
        let _ = admin_caps.insert(
            auth::SESSION_FS_SLOT,
            Capability {
                object: FS_OBJECT_ID,
                rights: Rights::READ.union(Rights::WRITE),
            },
        );

        // Credenciales dev-only: root / meridian (registradas por los selftests).
        let mut session = None;
        let mut attempt = 0usize;
        while attempt < 3 && session.is_none() {
            attempt += 1;

            console.write_text("login: ");
            let mut user = [0u8; 64];
            let user_len = console.read_line_echo(&mut user, true);

            console.write_text("Password: ");
            let mut password = [0u8; 64];
            let pass_len = console.read_line_echo(&mut password, false);

            // Nunca se escribe la contraseña: solo se usa para autenticar.
            let name = core::str::from_utf8(&user[..user_len]).unwrap_or("");
            let secret = core::str::from_utf8(&password[..pass_len]).unwrap_or("");
            match auth::login(name, secret, &admin_caps, auth::SESSION_FS_SLOT) {
                Ok(s) if s.username_bytes() == b"root" => {
                    let _ = writeln!(console, "T4.3: login correcto (user=root)");
                    session = Some(s);
                }
                _ => {
                    let _ = writeln!(console, "T4.3: credenciales invalidas rechazadas");
                }
            }
        }

        let Some(session) = session else {
            let _ = writeln!(console, "T4.3: sesion finalizada; halt");
            let empty = [""; 6];
            gui::present_terminal(&mut console, &empty);
            halt();
        };

        let mut shell = term::Shell::new(session);
        let mut line = [0u8; 256];
        let mut first_prompt = true;

        loop {
            shell.prompt(&mut console);
            if first_prompt {
                let _ = writeln!(console, "T4.3: prompt listo meridian:/home/root$");
                first_prompt = false;
            }

            let len = console.read_line_echo(&mut line, true);
            if len == 0 {
                continue;
            }
            let Ok(cmd) = core::str::from_utf8(&line[..len]) else { continue };

            // Compone "meridian:/home/root$ <cmd>" sin heap para la ventana.
            let mut display = [0u8; 64];
            let prefix = b"meridian:/home/root$ ";
            let mut n = 0usize;
            while n < prefix.len() && n < display.len() {
                display[n] = prefix[n];
                n += 1;
            }
            let mut i = 0usize;
            while i < cmd.len() && n < display.len() {
                display[n] = cmd.as_bytes()[i];
                n += 1;
                i += 1;
            }
            let command_line = core::str::from_utf8(&display[..n]).unwrap_or("");
            let view = [command_line, "", "", "", "", ""];
            gui::present_terminal(&mut console, &view);

            let result = shell.execute(&mut console, cmd);
            let _ = writeln!(console, "T4.3: cmd '{}' -> GUI terminal", cmd);
            if result == term::ExecResult::Exit {
                break;
            }
        }

        let final_view = ["sesion finalizada", "", "", "", "", ""];
        let _ = writeln!(console, "T4.3: sesion finalizada; halt");
        gui::present_terminal(&mut console, &final_view);
        halt();
    }

    #[cfg(not(feature = "interactive"))]
    {
        console.set_foreground(OK);
        console.println("Console ready. Halting CPU.");

        // Presentación final del escritorio (última operación de dibujo antes de halt).
        gui::present(&mut console);

        halt()
    }
}

fn read_rsp() -> u64 {
    let rsp: u64;
    // SAFETY: solo lectura del puntero de pila.
    unsafe {
        core::arch::asm!("mov {}, rsp", out(reg) rsp, options(nomem, nostack, preserves_flags));
    }
    rsp
}

fn a4_fatal(console: &mut Console, reason: &core::fmt::Arguments<'_>) -> ! {
    console.set_foreground(WARN);
    let _ = writeln!(console, "A4: INIT FAIL: {reason}");
    let _ = writeln!(console, "A4: SELFTEST FAILED - halting CPU");
    halt()
}

fn a5_fatal(console: &mut Console) -> ! {
    console.set_foreground(WARN);
    let _ = writeln!(console, "A5: SELFTEST FAILED - halting CPU");
    halt()
}

fn a6_fatal(console: &mut Console) -> ! {
    console.set_foreground(WARN);
    let _ = writeln!(console, "A6: SELFTEST FAILED - halting CPU");
    halt()
}

fn a7_fatal(console: &mut Console) -> ! {
    console.set_foreground(WARN);
    let _ = writeln!(console, "A7: SELFTEST FAILED - halting CPU");
    halt()
}

fn t12_fatal(console: &mut Console, reason: &core::fmt::Arguments<'_>) -> ! {
    console.set_foreground(WARN);
    let _ = writeln!(console, "T1.2: INIT FAIL: {reason}");
    let _ = writeln!(console, "T1.2: SELFTEST FAILED - halting CPU");
    halt()
}

fn t13_fatal(console: &mut Console) -> ! {
    console.set_foreground(WARN);
    let _ = writeln!(console, "T1.3: SELFTEST FAILED - halting CPU");
    halt()
}

fn t15_fatal(console: &mut Console) -> ! {
    console.set_foreground(WARN);
    let _ = writeln!(console, "T1.5: SELFTEST FAILED - halting CPU");
    halt()
}

fn t21_fatal(console: &mut Console) -> ! {
    console.set_foreground(WARN);
    let _ = writeln!(console, "T2.1: SELFTEST FAILED - halting CPU");
    halt()
}

fn t22_fatal(console: &mut Console) -> ! {
    console.set_foreground(WARN);
    let _ = writeln!(console, "T2.2: SELFTEST FAILED - halting CPU");
    halt()
}

fn t23_fatal(console: &mut Console) -> ! {
    console.set_foreground(WARN);
    let _ = writeln!(console, "T2.3: SELFTEST FAILED - halting CPU");
    halt()
}

fn t41_fatal(console: &mut Console) -> ! {
    console.set_foreground(WARN);
    let _ = writeln!(console, "T4.1: SELFTEST FAILED - halting CPU");
    halt()
}

fn a8_fatal(console: &mut Console) -> ! {
    console.set_foreground(WARN);
    let _ = writeln!(console, "A8: SELFTEST FAILED - halting CPU");
    halt()
}

fn a9_fatal(console: &mut Console) -> ! {
    console.set_foreground(WARN);
    let _ = writeln!(console, "A9: SELFTEST FAILED - halting CPU");
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
        // SAFETY: HLT detiene la CPU hasta interrupción.
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
