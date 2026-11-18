//! Auto-pruebas de creación de procesos y aislamiento de memoria (A5).
//!
//! Criterio: dos procesos de prueba no pueden leer la memoria del otro
//! (fallo controlado). Se crean dos espacios de direcciones con páginas
//! privadas; desde cada uno se verifica acceso propio y se provoca un #PF
//! controlado al intentar leer la página privada del otro.

use core::fmt::Write;

use meridian_bootinfo::PAGE_SIZE;

use crate::arch::x86_64::idt;
use crate::console::Console;
use crate::mm::buddy;
use crate::mm::paging::{self, PageFlags};

const PROC_A_VA: u64 = 0x1800_0000;
const PROC_B_VA: u64 = 0x1810_0000;
const SECRET_A: u64 = 0xA5A5_0001_5052_4F41; // "PROA"
const SECRET_B: u64 = 0xA5A5_0002_5052_4F42; // "PROB"

/// Ejecuta la prueba de aislamiento A5.
pub fn run(console: &mut Console) -> bool {
    match run_inner(console) {
        Ok(()) => true,
        Err(msg) => {
            let _ = writeln!(console, "A5: process isolation test FAIL: {msg}");
            false
        }
    }
}

fn run_inner(console: &mut Console) -> Result<(), &'static str> {
    let kernel_root = paging::kernel_root();
    if kernel_root == 0 {
        return Err("kernel paging root no inicializado");
    }

    // Reservar páginas físicas privadas y escribir secretos desde el espacio
    // kernel actual (mapeo identidad vigente).
    let pa_a = buddy::alloc(0).ok_or("sin memoria para página privada A")?;
    let pa_b = buddy::alloc(0).ok_or("sin memoria para página privada B")?;
    // SAFETY: páginas recién asignadas, identidad-mapeadas en kernel.
    unsafe {
        core::ptr::write_bytes(pa_a as *mut u8, 0, PAGE_SIZE as usize);
        core::ptr::write_bytes(pa_b as *mut u8, 0, PAGE_SIZE as usize);
        core::ptr::write_volatile(pa_a as *mut u64, SECRET_A);
        core::ptr::write_volatile(pa_b as *mut u64, SECRET_B);
    }

    let root_a = paging::create_address_space().map_err(|_| "no se pudo crear espacio A")?;
    let root_b = paging::create_address_space().map_err(|_| "no se pudo crear espacio B")?;

    // Mapear página privada propia en cada proceso.
    paging::map_page_in(
        root_a,
        PROC_A_VA,
        pa_a,
        PageFlags {
            writable: true,
            executable: false,
            user: false,
        },
    )
    .map_err(|_| "no se pudo mapear página privada de A")?;
    paging::map_page_in(
        root_b,
        PROC_B_VA,
        pa_b,
        PageFlags {
            writable: true,
            executable: false,
            user: false,
        },
    )
    .map_err(|_| "no se pudo mapear página privada de B")?;

    // Aislamiento: impedir acceso a la página física privada del otro tanto por
    // su VA privada como por el alias identidad (presente en el mapeo kernel copiado).
    isolate_other(root_a, pa_b, PROC_B_VA).map_err(|_| "fallo aislando B de A")?;
    isolate_other(root_b, pa_a, PROC_A_VA).map_err(|_| "fallo aislando A de B")?;

    let _ = writeln!(
        console,
        "A5: process creation OK (proc A root=0x{root_a:016X} priv_pa=0x{pa_a:016X} va=0x{PROC_A_VA:016X}, proc B root=0x{root_b:016X} priv_pa=0x{pa_b:016X} va=0x{PROC_B_VA:016X})"
    );

    // Proceso A accede a su propia memoria.
    let own_a = read_in_space(root_a, PROC_A_VA, kernel_root);
    if own_a != SECRET_A {
        paging::switch(kernel_root);
        let _ = writeln!(
            console,
            "A5: FAIL: proceso A leyó 0x{own_a:016X} en su propia página (esperado 0x{SECRET_A:016X})"
        );
        return Err("proceso A no pudo leer su propia memoria");
    }

    // Proceso A intenta leer memoria privada de B (alias identidad) -> #PF controlado.
    let fault_a = controlled_read_fault(root_a, pa_b, kernel_root)
        .ok_or("la lectura de A a memoria privada de B no produjo #PF controlado")?;
    let _ = writeln!(
        console,
        "A5: isolation A cannot read B OK (tried va=0x{pa_b:016X}, controlled #PF addr=0x{:016X} err=0x{:X})",
        fault_a.0, fault_a.1
    );

    // Proceso B accede a su propia memoria.
    let own_b = read_in_space(root_b, PROC_B_VA, kernel_root);
    if own_b != SECRET_B {
        paging::switch(kernel_root);
        let _ = writeln!(
            console,
            "A5: FAIL: proceso B leyó 0x{own_b:016X} en su propia página (esperado 0x{SECRET_B:016X})"
        );
        return Err("proceso B no pudo leer su propia memoria");
    }

    // Proceso B intenta leer memoria privada de A (alias identidad) -> #PF controlado.
    let fault_b = controlled_read_fault(root_b, pa_a, kernel_root)
        .ok_or("la lectura de B a memoria privada de A no produjo #PF controlado")?;
    let _ = writeln!(
        console,
        "A5: isolation B cannot read A OK (tried va=0x{pa_a:016X}, controlled #PF addr=0x{:016X} err=0x{:X})",
        fault_b.0, fault_b.1
    );

    paging::switch(kernel_root);

    let _ = writeln!(
        console,
        "A5: process isolation test PASS (2 processes, separate PML4, cross-read blocked by controlled #PF, kernel resumed)"
    );

    // Best-effort: liberar páginas privadas de datos. Las tablas clonadas no se
    // recorren/liberan en esta versión mínima.
    let _ = buddy::free(pa_a, 0);
    let _ = buddy::free(pa_b, 0);

    Ok(())
}

fn isolate_other(space_root: u64, other_pa: u64, other_va: u64) -> Result<(), &'static str> {
    paging::unmap_page_in(space_root, other_va).map_err(|_| "unmap va ajena")?;
    paging::unmap_page_in(space_root, other_pa).map_err(|_| "unmap pa identidad ajena")?;
    Ok(())
}

fn read_in_space(root: u64, va: u64, kernel_root: u64) -> u64 {
    paging::switch(root);
    // SAFETY: la VA debe estar mapeada en este espacio; si no, sería un fallo no esperado.
    let value = unsafe { core::ptr::read_volatile(va as *const u64) };
    paging::switch(kernel_root);
    value
}

/// Intenta leer `va` en `root` y espera un #PF controlado.
fn controlled_read_fault(root: u64, va: u64, kernel_root: u64) -> Option<(u64, u64)> {
    paging::switch(root);
    let result = unsafe {
        idt::try_recover_page_fault(va as *mut (), faulting_read)
    };
    paging::switch(kernel_root);
    result
}

unsafe extern "sysv64" fn faulting_read(addr: *mut ()) {
    let ptr = addr as *const u64;
    let _value = core::ptr::read_volatile(ptr);
    // Si llegamos aquí, la lectura no falló.
    core::hint::black_box(_value);
}
