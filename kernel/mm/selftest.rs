//! Auto-pruebas ejecutadas en cada arranque (tarea A4).
//!
//! Al ser un kernel `no_std` sin arnés de tests, las comprobaciones se hacen
//! sobre el estado real del sistema y se informan por COM1 (y por GOP, ya que
//! se usa la consola combinada) con marcadores que `tools/run.sh` valida:
//!
//! ```text
//! A4: buddy alloc/free tests PASS (...)
//! A4: W^X check PASS (...)
//! ```
//!
//! Cualquier fallo emite `A4: ... FAIL: <motivo>` y el kernel se detiene.

use core::fmt::Write;

use meridian_bootinfo::PAGE_SIZE;

use crate::console::Console;
use crate::mm::buddy::{self, ALLOCATED_STATE};
use crate::mm::paging::{self, Violation};

const MAGIC: u64 = 0x4D45_5249_4449_414E;
const SLOTS: usize = 32;

/// Ejecuta todas las pruebas. Se usan `&` (no `&&`) para que ambas corran
/// aunque la primera falle y el diagnóstico sea completo.
pub fn run(console: &mut Console) -> bool {
    let buddy_ok = buddy_tests(console);
    let wx_ok = wx_test(console);
    let _ = writeln!(console, "");
    buddy_ok & wx_ok
}

fn buddy_tests(console: &mut Console) -> bool {
    macro_rules! fail {
        ($($arg:tt)*) => {{
            let _ = writeln!(console, "A4: buddy alloc/free tests FAIL: {}", format_args!($($arg)*));
            return false;
        }};
    }

    let before = buddy::stats();
    let mut operations = 0usize;

    // Secuencia 1: bloques de órdenes variados, incluido uno de 32 páginas.
    // Se comprueba alineación, no solapamiento, estado del buddy y que la
    // memoria reservada realmente se pueda escribir y leer.
    let mut blocks = [(0u64, 0usize); SLOTS];
    let mut used = 0usize;
    for order in [0usize, 1, 2, 0, 3, 1, 0, 2, 4, 0, 5, 1] {
        if used >= blocks.len() {
            fail!("demasiados bloques en la secuencia 1");
        }
        let Some(address) = buddy::alloc(order) else {
            fail!("alloc(order={order}) devolvió None");
        };
        let block = (address, order);
        let block_bytes = (1u64 << order) * PAGE_SIZE;
        if address % block_bytes != 0 {
            fail!("bloque 0x{address:016X} no alineado a {block_bytes} bytes");
        }
        if blocks[..used].iter().any(|other| overlaps(*other, block)) {
            fail!("bloque 0x{address:016X} solapa una reserva anterior");
        }
        if !fill_and_check(address, order) {
            fail!("lectura/escritura falló en 0x{address:016X} (order={order})");
        }
        match buddy::page_state(address) {
            Some((state, recorded_order)) if state == ALLOCATED_STATE && recorded_order == order => {}
            other => fail!("estado inesperado tras alloc: {other:?}"),
        }
        blocks[used] = block;
        used += 1;
        operations += 1;
    }

    // Liberación en orden inverso: fuerza coalescing de buddies de todos los
    // órdenes usados.
    while used > 0 {
        used -= 1;
        let (address, order) = blocks[used];
        if let Err(err) = buddy::free(address, order) {
            fail!("free(0x{address:016X}, order={order}) falló: {err}");
        }
        operations += 1;
    }

    // Secuencia 2: muchas páginas sueltas liberadas en orden permutado, para
    // verificar que el coalescing no depende del orden de liberación.
    let mut pages = [0u64; SLOTS];
    for (index, page) in pages.iter_mut().enumerate() {
        let Some(address) = buddy::alloc(0) else {
            fail!("alloc(order=0) #{index} devolvió None");
        };
        if !fill_and_check(address, 0) {
            fail!("lectura/escritura falló en 0x{address:016X}");
        }
        *page = address;
        operations += 1;
    }
    let mut freed = [false; SLOTS];
    for step in 0..SLOTS {
        let index = (step * 17) % SLOTS;
        if freed[index] {
            fail!("índice {index} liberado dos veces en la permutación");
        }
        let address = pages[index];
        if let Err(err) = buddy::free(address, 0) {
            fail!("free(0x{address:016X}, 0) falló: {err}");
        }
        freed[index] = true;
        operations += 1;
    }

    // Doble liberación: debe rechazarse sin alterar los contadores.
    let Some(address) = buddy::alloc(0) else {
        fail!("alloc para la prueba de doble free devolvió None");
    };
    if buddy::free(address, 0).is_err() {
        fail!("primer free de la prueba de doble free falló");
    }
    operations += 2;
    if buddy::free(address, 0).is_ok() {
        fail!("se aceptó una doble liberación en 0x{address:016X}");
    }

    let after = buddy::stats();
    if after != before {
        fail!(
            "fuga: free_pages {}->{}, allocated_pages {}->{}, bloques {:?} -> {:?}",
            before.free_pages,
            after.free_pages,
            before.allocated_pages,
            after.allocated_pages,
            before.blocks,
            after.blocks
        );
    }

    let mib = after.free_pages * PAGE_SIZE / (1024 * 1024);
    let _ = writeln!(
        console,
        "A4: buddy alloc/free tests PASS ({operations} ops, coalescing exacto, {} free pages / {} MiB, skipped={})",
        after.free_pages, mib, after.skipped_pages
    );
    true
}

fn wx_test(console: &mut Console) -> bool {
    match paging::audit() {
        Ok(audit) => {
            if audit.writable_executable_pages != 0
                || !audit.nxe_enabled
                || !audit.write_protect_enabled
            {
                let _ = writeln!(
                    console,
                    "A4: W^X check FAIL: W+X={} pages, NXE={}, WP={}, first={}",
                    audit.writable_executable_pages,
                    audit.nxe_enabled,
                    audit.write_protect_enabled,
                    ViolationDisplay(audit.first_violation)
                );
                return false;
            }
            let _ = writeln!(
                console,
                "A4: W^X check PASS ({} mapped pages, {} executable, {} W+RW-NX, W+X=0, NXE=1, WP=1)",
                audit.mapped_pages,
                audit.executable_pages,
                audit.writable_non_executable_pages
            );
            true
        }
        Err(err) => {
            let _ = writeln!(console, "A4: W^X check FAIL: auditoría: {err}");
            false
        }
    }
}

/// Adaptador para imprimir la primera violación W^X sin `alloc`.
struct ViolationDisplay(Option<Violation>);

impl core::fmt::Display for ViolationDisplay {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self.0 {
            Some(violation) => write!(f, "{violation}"),
            None => write!(f, "ninguna"),
        }
    }
}

/// Comprueba que todas las páginas de un bloque se pueden escribir y leer.
fn fill_and_check(address: u64, order: usize) -> bool {
    for index in 0..(1u64 << order) {
        let pointer = (address + index * PAGE_SIZE) as *mut u64;
        // SAFETY: el buddy acaba de reservar el bloque y el mapeo identidad lo
        // cubre como RW+NX; las direcciones están dentro del bloque.
        unsafe { core::ptr::write_volatile(pointer, MAGIC ^ index) };
    }
    for index in 0..(1u64 << order) {
        let pointer = (address + index * PAGE_SIZE) as *const u64;
        // SAFETY: ver el bucle anterior.
        let value = unsafe { core::ptr::read_volatile(pointer) };
        if value != MAGIC ^ index {
            return false;
        }
    }
    true
}

/// `true` si dos bloques `[address, address + 2^order * PAGE_SIZE)` se solapan.
fn overlaps(first: (u64, usize), second: (u64, usize)) -> bool {
    let first_end = first.0 + ((1u64 << first.1) * PAGE_SIZE);
    let second_end = second.0 + ((1u64 << second.1) * PAGE_SIZE);
    first.0 < second_end && second.0 < first_end
}
