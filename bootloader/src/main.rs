//! Meridian — cargador UEFI mínimo (tarea A2).
//!
//! Flujo:
//! 1. Inicializa los helpers de `uefi` (allocator, logger, panic handler).
//! 2. Localiza un dispositivo GOP y dibuja un mensaje directamente en pantalla.
//! 3. Abre el sistema de archivos de la imagen FAT y lee `\kernel.elf` y
//!    `\kernel.sig`.
//! 4. Verifica la firma Ed25519 del kernel con la clave embebida (T1.1).
//! 5. Carga los segmentos `PT_LOAD` del ELF en `KERNEL_LOAD_BASE`.
//! 6. Captura el mapa de memoria final saliendo de boot services y entrega
//!    `BootInfo` al kernel.
//!
//! El kernel es el responsable de la consola a partir del handoff (A3).

#![no_main]
#![no_std]

extern crate alloc;

mod display;
mod elf;
mod signature;

use alloc::format;
use core::time::Duration;

use display::{Canvas, ACCENT, BACKGROUND, OK, SUBTITLE, TEXT, TITLE};
use meridian_bootinfo::{
    BootInfo, FramebufferInfo, MemoryMapInfo, PixelFormat as BootPixelFormat, BOOTINFO_VERSION,
};
use uefi::boot::{self, AllocateType, MemoryType};
use uefi::fs::FileSystem;
use uefi::mem::memory_map::MemoryMap;
use uefi::prelude::*;
use uefi::proto::console::gop::{GraphicsOutput, Mode, PixelFormat as GopPixelFormat};

/// Puerto del dispositivo `debugcon` de QEMU en x86-64. Permite emitir trazas
/// después de dibujar en GOP sin que el logger de UEFI vuelva a escribir en pantalla.
#[cfg(target_arch = "x86_64")]
const QEMU_DEBUGCON_PORT: u16 = 0xE9;

/// UART PL011 de QEMU `virt` en aarch64, usado como sumidero de trazas después
/// de `ExitBootServices()`.
#[cfg(target_arch = "aarch64")]
const PL011_BASE: usize = 0x0900_0000;

#[entry]
fn main() -> Status {
    if let Err(err) = uefi::helpers::init() {
        // Sin logger todavía; stdout sigue siendo la mejor salida disponible.
        uefi::println!("Meridian: no se pudieron inicializar los helpers: {err:?}");
        return Status::ABORTED;
    }

    log::info!("Meridian UEFI bootloader: inicio");

    match run() {
        Ok(()) => Status::SUCCESS,
        Err(err) => {
            log::error!("fallo de arranque: {err:?}");
            uefi::println!("Meridian: fallo de arranque: {err:?}");
            // Mantiene el diagnóstico en pantalla para una captura o inspección.
            boot::stall(Duration::from_secs(15));
            Status::ABORTED
        }
    }
}

fn run() -> uefi::Result<()> {
    // --- GOP: seleccionar una salida de vídeo razonable -------------------
    let mut gop_handles = boot::find_handles::<GraphicsOutput>()?;
    if gop_handles.is_empty() {
        log::error!("el firmware no expone GraphicsOutputProtocol");
        return Err(Status::UNSUPPORTED.into());
    }
    let gop_handle = gop_handles.remove(0);
    drop(gop_handles);
    log::info!("GOP localizado ({} handle(s) disponibles)", 1);
    log::info!("buscando kernel.elf en la ESP");

    // --- Sistema de archivos de la ESP/FAT, kernel y firma ----------------
    let (kernel_data, signature_data) = {
        log::info!("localizando sistema de archivos de la imagen");
        let fs_proto = boot::get_image_file_system(boot::image_handle()).map_err(|err| {
            log::error!("get_image_file_system falló: {err:?}");
            err
        })?;
        let mut fs = FileSystem::new(fs_proto);

        let kernel_data = {
            let exists = fs.try_exists(cstr16!("\\kernel.elf")).map_err(|err| {
                log::error!("error consultando \\kernel.elf: {err:?}");
                uefi::Error::from(Status::LOAD_ERROR)
            })?;
            if !exists {
                log::error!("\\kernel.elf no existe en la imagen FAT");
                return Err(Status::NOT_FOUND.into());
            }

            fs.read(cstr16!("\\kernel.elf")).map_err(|err| {
                log::error!("no se pudo leer \\kernel.elf: {err:?}");
                uefi::Error::from(Status::LOAD_ERROR)
            })?
        };

        let signature_data = {
            let exists = fs.try_exists(cstr16!("\\kernel.sig")).map_err(|err| {
                log::error!("error consultando \\kernel.sig: {err:?}");
                uefi::Error::from(Status::SECURITY_VIOLATION)
            })?;
            if !exists {
                log::error!("\\kernel.sig no existe en la imagen FAT");
                debugcon("meridian: firma de kernel ausente; abortando\r\n");
                return Err(Status::SECURITY_VIOLATION.into());
            }

            fs.read(cstr16!("\\kernel.sig")).map_err(|err| {
                log::error!("no se pudo leer \\kernel.sig: {err:?}");
                uefi::Error::from(Status::SECURITY_VIOLATION)
            })?
        };

        (kernel_data, signature_data)
    };
    log::info!("\\kernel.elf leído ({} bytes)", kernel_data.len());
    log::info!("\\kernel.sig leído ({} bytes)", signature_data.len());
    log::info!(
        "verificando firma Ed25519 con {}",
        signature::KERNEL_SIGNATURE_PATH
    );

    signature::verify_kernel_signature(&kernel_data, &signature_data).map_err(|err| {
        log::error!("verificación de firma fallida: {err}");
        debugcon("meridian: firma de kernel inválida; abortando\r\n");
        uefi::Error::from(Status::SECURITY_VIOLATION)
    })?;
    debugcon("meridian: firma de kernel verificada\r\n");
    drop(signature_data);

    let kernel = elf::load(&kernel_data).map_err(|err| {
        log::error!("ELF inválido: {err}");
        uefi::Error::from(Status::LOAD_ERROR)
    })?;
    drop(kernel_data);
    log::info!(
        "kernel cargado: base=0x{:016X} entry=0x{:016X} rango={} bytes",
        kernel.base,
        kernel.entry,
        kernel.size
    );

    // Mapa informativo para la pantalla. El mapa definitivo se obtiene dentro
    // de exit_boot_services() más abajo.
    let preliminary_map = boot::memory_map(MemoryType::LOADER_DATA)?;
    let memory_map_entries = preliminary_map.meta().entry_count();
    drop(preliminary_map);
    log::info!("mapa de memoria UEFI: {memory_map_entries} descriptores");

    // --- Dibujo directo en GOP --------------------------------------------
    let framebuffer = {
        let mut gop = boot::open_protocol_exclusive::<GraphicsOutput>(gop_handle)?;

        if let Some(mode) = best_gop_mode(&gop) {
            let (width, height) = mode.info().resolution();
            match gop.set_mode(&mode) {
                Ok(()) => log::info!("modo GOP seleccionado: {width}x{height}"),
                Err(err) => {
                    log::warn!("no se pudo cambiar el modo GOP ({err:?}); se usa el actual")
                }
            }
        }

        let framebuffer = capture_framebuffer(&mut gop);
        draw_boot_screen(&mut gop, memory_map_entries, &kernel, &framebuffer)?;
        // `gop` se cierra (CloseProtocol) al salir de este bloque, antes de
        // ExitBootServices.
        framebuffer
    };

    debugcon("meridian: pantalla GOP dibujada\r\n");

    // Reserva la página donde vivirá BootInfo. Debe hacerse antes de salir de
    // boot services; el contenido se escribe después, cuando ya se conoce el
    // mapa de memoria definitivo.
    let boot_info_page = boot::allocate_pages(AllocateType::AnyPages, MemoryType::LOADER_DATA, 1)?;

    // A partir de aquí no se puede usar ningún servicio de arranque, el
    // allocator de Rust, `log!` ni `println!`.
    let memory_map = unsafe { boot::exit_boot_services(None) };

    let memory_map_info = MemoryMapInfo {
        buffer: memory_map.buffer().as_ptr() as u64,
        size: memory_map.buffer().len() as u64,
        descriptor_size: memory_map.meta().desc_size as u64,
        descriptor_version: memory_map.meta().desc_version,
        reserved: 0,
    };

    let boot_info = BootInfo {
        magic: meridian_bootinfo::BOOTINFO_MAGIC,
        version: BOOTINFO_VERSION,
        size: core::mem::size_of::<BootInfo>() as u32,
        memory_map: memory_map_info,
        framebuffer,
        kernel_base: kernel.base,
        kernel_entry: kernel.entry,
        kernel_size: kernel.size,
    };

    let boot_info_ptr = boot_info_page.as_ptr().cast::<BootInfo>();
    // SAFETY: la página fue reservada con AllocatePages y está alineada; aún no
    // contiene una referencia viva y el kernel no la reutilizará al arrancar.
    unsafe {
        boot_info_ptr.write(boot_info);
    }

    // El buffer del mapa vive en memoria LOADER_DATA reservada por
    // exit_boot_services(); hay que conservarlo durante toda la vida del kernel.
    // AAVMF deja la RAM mapeada 1:1 en EL1; solo hay que limpiar XN en las
    // entradas que cubren el kernel antes de saltar.
    #[cfg(target_arch = "aarch64")]
    {
        let ttbr0: u64;
        // SAFETY: leer TTBR0_EL1 es seguro en EL1 y no altera el estado.
        unsafe {
            core::arch::asm!("mrs {0}, ttbr0_el1", out(reg) ttbr0);
            make_kernel_executable(ttbr0, kernel.entry, kernel.size);
        }
    }
    let entry_address = kernel.entry;

    core::mem::forget(memory_map);

    debugcon("meridian: ExitBootServices completado, saltando al kernel\r\n");

    // SAFETY: el ELF fue validado y cargado en kernel.entry. La pila del
    // firmware sigue siendo válida en este punto.
    //
    // En x86-64 el bootloader UEFI usa la ABI Windows x64 para `extern "C"`
    // (primer argumento en RCX) y el kernel usa SysV (RDI), por eso se declara
    // `extern "sysv64"`. En aarch64 ambos usan AAPCS64 (x0), así que basta
    // `extern "C"`.
    #[cfg(target_arch = "x86_64")]
    let entry: extern "sysv64" fn(*const BootInfo) -> ! =
        unsafe { core::mem::transmute(entry_address as usize) };
    #[cfg(target_arch = "aarch64")]
    let entry: extern "C" fn(*const BootInfo) -> ! =
        unsafe { core::mem::transmute(entry_address as usize) };
    // AArch64: el kernel se copió con servicios UEFI; hay que hacer visible
    // el nuevo código en el caché de instrucciones antes de saltar.
    #[cfg(target_arch = "aarch64")]
    unsafe {
        core::arch::asm!(
            "dsb sy",
            "ic iallu",
            "dsb sy",
            "isb",
            options(nostack, preserves_flags)
        );
    }
    entry(boot_info_ptr);

    #[allow(unreachable_code)]
    Ok(())
}

/// Elige el modo GOP de mayor resolución dentro de un rango prudente.
fn best_gop_mode(gop: &GraphicsOutput) -> Option<Mode> {
    let mut best: Option<Mode> = None;
    for mode in gop.modes() {
        let (width, height) = mode.info().resolution();
        if width < 800 || height < 600 || width > 1920 || height > 1200 {
            continue;
        }
        let better = match &best {
            None => true,
            Some(current) => {
                let (cw, ch) = current.info().resolution();
                width * height > cw * ch
            }
        };
        if better {
            best = Some(mode);
        }
    }
    best
}

fn capture_framebuffer(gop: &mut GraphicsOutput) -> FramebufferInfo {
    let mode = gop.current_mode_info();
    let (width, height) = mode.resolution();
    let pixel_format = match mode.pixel_format() {
        GopPixelFormat::Rgb => BootPixelFormat::Rgb,
        GopPixelFormat::Bgr => BootPixelFormat::Bgr,
        GopPixelFormat::Bitmask => BootPixelFormat::Bitmask,
        GopPixelFormat::BltOnly => BootPixelFormat::BltOnly,
    };
    let (red_mask, green_mask, blue_mask, reserved_mask) = mode
        .pixel_bitmask()
        .map(|mask| (mask.red, mask.green, mask.blue, mask.reserved))
        .unwrap_or((0, 0, 0, 0));

    let (base, size) = {
        let mut framebuffer = gop.frame_buffer();
        (framebuffer.as_mut_ptr() as u64, framebuffer.size() as u64)
    };

    FramebufferInfo {
        base,
        size,
        width: width as u64,
        height: height as u64,
        stride: mode.stride() as u64,
        pixel_format: pixel_format as u32,
        reserved: 0,
        red_mask,
        green_mask,
        blue_mask,
        reserved_mask,
    }
}

fn draw_boot_screen(
    gop: &mut GraphicsOutput,
    memory_map_entries: usize,
    kernel: &elf::LoadedKernel,
    framebuffer: &FramebufferInfo,
) -> uefi::Result<()> {
    let (width, height) = gop.current_mode_info().resolution();
    let mut canvas = Canvas::new(width, height, BACKGROUND);

    // Bandas superior e inferior.
    let band = (height / 64).max(4);
    canvas.fill_rect(0, 0, width, band, ACCENT);
    canvas.fill_rect(0, height - band, width, band, ACCENT);

    let title = "MERIDIAN UEFI BOOTLOADER";
    let title_scale = display::scale_to_fit(width * 9 / 10, title.len(), 6);
    let title_y = height / 5;
    canvas.centered_text(title_y, title, title_scale, TITLE);

    let subtitle = "GOP framebuffer active - kernel loaded from FAT";
    let subtitle_scale = display::scale_to_fit(width * 9 / 10, subtitle.len(), 3);
    canvas.centered_text(
        title_y + 9 * title_scale + height / 32,
        subtitle,
        subtitle_scale,
        SUBTITLE,
    );

    let lines = [
        (
            format!(
                "GOP: {width}x{height}  framebuffer 0x{:016X}",
                framebuffer.base
            ),
            TEXT,
        ),
        (
            format!("UEFI memory map: {memory_map_entries} descriptors captured"),
            TEXT,
        ),
        (
            format!("Kernel: \\kernel.elf ({} bytes)", kernel.size),
            TEXT,
        ),
        (
            format!(
                "Entry: 0x{:016X}  base: 0x{:016X}",
                kernel.entry, kernel.base
            ),
            OK,
        ),
    ];

    let longest = lines.iter().map(|(line, _)| line.len()).max().unwrap_or(1);
    let line_scale = display::scale_to_fit(width * 9 / 10, longest, 2);
    let mut y = height / 2;
    let line_height = 9 * line_scale + height / 96;
    for (line, color) in lines {
        canvas.centered_text(y, &line, line_scale, color);
        y += line_height;
    }

    let footer = "Handing off to the kernel...";
    let footer_scale = display::scale_to_fit(width * 9 / 10, footer.len(), 2);
    canvas.centered_text(
        height - band - 9 * footer_scale - height / 24,
        footer,
        footer_scale,
        SUBTITLE,
    );

    canvas.flush(gop)
}

/// Escribe directamente en el dispositivo `debugcon` de QEMU (puerto 0xE9).
///
/// No toca GOP ni depende de boot services, por lo que es seguro tras
/// `ExitBootServices()`.
#[cfg(target_arch = "x86_64")]
fn debugcon(message: &str) {
    for byte in message.bytes() {
        // SAFETY: el puerto 0xE9 es un sumidero de depuración fijo de QEMU; la
        // escritura no tiene efectos sobre memoria ni pila.
        unsafe {
            core::arch::asm!(
                "out dx, al",
                in("dx") QEMU_DEBUGCON_PORT,
                in("al") byte,
                options(nomem, nostack, preserves_flags)
            );
        }
    }
}

/// AAVMF mapa la RAM como RW+NX. Esta función limpia el bit XN de la entrada
/// de nivel 2 que cubre el kernel, para que el salto desde UEFI sea ejecutable
/// sin abandonar el mapa de páginas del firmware.
#[cfg(target_arch = "aarch64")]
unsafe fn make_kernel_executable(ttbr0: u64, va_start: u64, size: u64) {
    const TABLE_MASK: u64 = 0x0000_ffff_ffff_f000;
    const XN: u64 = (1 << 54) | (1 << 55);
    const PAGE_SIZE: u64 = 4096;

    let mut page = va_start & !(PAGE_SIZE - 1);
    let end = va_start.saturating_add(size);
    while page < end {
        let mut table = ttbr0 & TABLE_MASK;
        for level in 0..4u32 {
            let shift = 39 - 9 * level;
            let index = ((page >> shift) & 0x1ff) as usize;
            let entry = (table as *mut u64).add(index);
            let descriptor = entry.read_volatile();

            if level == 3 {
                if descriptor & 0b11 == 0b11 {
                    entry.write_volatile(descriptor & !XN);
                    core::arch::asm!(
                        "dc civac, {addr}",
                        "dsb sy",
                        addr = in(reg) entry as usize,
                        options(nostack, preserves_flags)
                    );
                }
                break;
            }

            match descriptor & 0b11 {
                0b11 => table = descriptor & TABLE_MASK,
                0b01 => {
                    // Bloque de 1 GiB (nivel 1) o 2 MiB (nivel 2).
                    entry.write_volatile(descriptor & !XN);
                    core::arch::asm!(
                        "dc civac, {addr}",
                        "dsb sy",
                        addr = in(reg) entry as usize,
                        options(nostack, preserves_flags)
                    );
                    break;
                }
                _ => break,
            }
        }
        page = page.saturating_add(PAGE_SIZE);
    }

    core::arch::asm!(
        "tlbi vmalle1",
        "dsb sy",
        "isb",
        options(nostack, preserves_flags)
    );
}

/// Escribe trazas en el UART PL011 de QEMU `virt` tras `ExitBootServices()`.
#[cfg(target_arch = "aarch64")]
fn debugcon(message: &str) {
    for byte in message.bytes() {
        // SAFETY: PL011_BASE es el UART fijo de la máquina `virt` de QEMU; la
        // escritura de un byte al registro DR no altera memoria ni pila.
        unsafe {
            let fr = (PL011_BASE + 0x18) as *const u32;
            let dr = (PL011_BASE + 0x00) as *mut u32;
            for _ in 0..100_000 {
                if core::ptr::read_volatile(fr) & (1 << 5) != 0 {
                    break;
                }
                core::hint::spin_loop();
            }
            core::ptr::write_volatile(dr, byte as u32);
        }
    }
}
