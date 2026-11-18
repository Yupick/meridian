//! Interrupt Descriptor Table (IDT) y manejo controlado de page faults.

use core::arch::asm;
use core::fmt::Write;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use super::gdt::{DescriptorTablePointer, IST_DOUBLE_FAULT, IST_PAGE_FAULT, KERNEL_CODE_SELECTOR};
use crate::console::Serial;

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct InterruptFrame {
    pub rip: u64,
    pub cs: u64,
    pub rflags: u64,
    pub rsp: u64,
    pub ss: u64,
}

#[repr(C, align(16))]
struct Idt {
    entries: [IdtEntry; 256],
}

#[derive(Clone, Copy, Default)]
#[repr(C)]
struct IdtEntry {
    offset_low: u16,
    selector: u16,
    ist: u8,
    type_attr: u8,
    offset_mid: u16,
    offset_high: u32,
    zero: u32,
}

impl IdtEntry {
    const fn missing() -> Self {
        Self {
            offset_low: 0,
            selector: 0,
            ist: 0,
            type_attr: 0,
            offset_mid: 0,
            offset_high: 0,
            zero: 0,
        }
    }

    fn set_handler(&mut self, handler: u64, ist: u8) {
        self.offset_low = handler as u16;
        self.selector = KERNEL_CODE_SELECTOR;
        self.ist = ist & 0x7;
        self.type_attr = 0x8E; // present, DPL=0, interrupt gate
        self.offset_mid = (handler >> 16) as u16;
        self.offset_high = (handler >> 32) as u32;
        self.zero = 0;
    }
}

static mut IDT: Idt = Idt {
    entries: [IdtEntry::missing(); 256],
};

/// Contexto de recuperación para fallos controlados (#PF esperado).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct RecoveryContext {
    pub rbx: u64,
    pub rbp: u64,
    pub r12: u64,
    pub r13: u64,
    pub r14: u64,
    pub r15: u64,
    pub rsp: u64,
    pub rip: u64,
    pub cr3: u64,
}

static mut RECOVERY: RecoveryContext = RecoveryContext {
    rbx: 0,
    rbp: 0,
    r12: 0,
    r13: 0,
    r14: 0,
    r15: 0,
    rsp: 0,
    rip: 0,
    cr3: 0,
};

pub static EXPECT_PAGE_FAULT: AtomicBool = AtomicBool::new(false);
pub static LAST_PF_ADDR: AtomicU64 = AtomicU64::new(0);
pub static LAST_PF_ERR: AtomicU64 = AtomicU64::new(0);
pub static PAGE_FAULT_COUNT: AtomicU64 = AtomicU64::new(0);

/// Instala la IDT con handlers mínimos.
///
/// # Safety
/// Debe llamarse una vez tras inicializar GDT/TSS, en contexto de kernel.
pub unsafe fn init() {
    IDT.entries[8].set_handler(double_fault_stub as *const () as u64, IST_DOUBLE_FAULT as u8);
    IDT.entries[13].set_handler(general_protection_stub as *const () as u64, 0);
    IDT.entries[14].set_handler(page_fault_stub as *const () as u64, IST_PAGE_FAULT as u8);

    let idtr = DescriptorTablePointer {
        limit: (core::mem::size_of::<Idt>() - 1) as u16,
        base: core::ptr::addr_of!(IDT) as u64,
    };
    asm!("lidt [{idtr}]", idtr = in(reg) &idtr, options(readonly, nostack, preserves_flags));
}

/// Ejecuta una operación con recuperación ante #PF.
/// Devuelve `Some(fault_addr, error_code)` si ocurrió un #PF controlado, o
/// `None` si la operación completó sin fallo.
///
/// # Safety
/// `func(ctx)` se ejecuta en el contexto actual. Debe ser acotado y su fallo
/// esperado debe ser un page fault recuperable (pila IST y recuperación listos).
pub unsafe fn try_recover_page_fault(
    ctx: *mut (),
    func: unsafe extern "sysv64" fn(*mut ()),
) -> Option<(u64, u64)> {
    LAST_PF_ADDR.store(0, Ordering::SeqCst);
    LAST_PF_ERR.store(0, Ordering::SeqCst);
    let before = PAGE_FAULT_COUNT.load(Ordering::SeqCst);
    EXPECT_PAGE_FAULT.store(true, Ordering::SeqCst);

    let faulted = recovery_call(ctx as u64, func as usize as u64);

    EXPECT_PAGE_FAULT.store(false, Ordering::SeqCst);
    let after = PAGE_FAULT_COUNT.load(Ordering::SeqCst);

    if faulted != 0 || after > before {
        Some((LAST_PF_ADDR.load(Ordering::SeqCst), LAST_PF_ERR.load(Ordering::SeqCst)))
    } else {
        None
    }
}

extern "sysv64" {
    /// Guarda contexto callee-saved en RECOVERY y llama a `func(arg)`.
    /// Retorna 0 si `func` vuelve normalmente, o 1 si se recuperó de #PF.
    fn recovery_call(arg: u64, func: u64) -> u64;
    /// Restaura RECOVERY y retorna 1 a recovery_call.
    fn recovery_resume() -> !;
}

core::arch::global_asm!(
    r#"
    .global recovery_call
    .type recovery_call, @function
recovery_call:
    lea rax, [rip + {recovery}]
    mov [rax + 0], rbx
    mov [rax + 8], rbp
    mov [rax + 16], r12
    mov [rax + 24], r13
    mov [rax + 32], r14
    mov [rax + 40], r15
    mov [rax + 48], rsp
    lea rcx, [rip + 2f]
    mov [rax + 56], rcx
    mov rcx, cr3
    mov [rax + 64], rcx
    # call func(arg): arg en rdi, func en rsi (ABI sysv64)
    mov rax, rsi
    call rax
    xor eax, eax
    ret
2:
    mov eax, 1
    ret

    .global recovery_resume
    .type recovery_resume, @function
recovery_resume:
    lea rax, [rip + {recovery}]
    mov rbx, [rax + 0]
    mov rbp, [rax + 8]
    mov r12, [rax + 16]
    mov r13, [rax + 24]
    mov r14, [rax + 32]
    mov r15, [rax + 40]
    mov rsp, [rax + 48]
    mov rcx, [rax + 64]
    mov cr3, rcx
    mov rdx, [rax + 56]
    mov eax, 1
    jmp rdx
"#,
    recovery = sym RECOVERY,
);

#[unsafe(naked)]
unsafe extern "C" fn page_fault_stub() {
    core::arch::naked_asm!(
        "push rax",
        "push rcx",
        "push rdx",
        "push rsi",
        "push rdi",
        "push r8",
        "push r9",
        "push r10",
        "push r11",
        "mov rdi, [rsp + 72]", // error code
        "mov rsi, rsp",
        "add rsi, 80",         // &InterruptFrame
        "call {handler}",
        "pop r11",
        "pop r10",
        "pop r9",
        "pop r8",
        "pop rdi",
        "pop rsi",
        "pop rdx",
        "pop rcx",
        "pop rax",
        "add rsp, 8",          // descartar error code
        "iretq",
        handler = sym page_fault_handler,
    );
}

#[unsafe(naked)]
unsafe extern "C" fn double_fault_stub() {
    core::arch::naked_asm!(
        "mov rdi, [rsp]",      // error code
        "lea rsi, [rsp + 8]",
        "call {handler}",
        "2: hlt; jmp 2b",
        handler = sym double_fault_handler,
    );
}

#[unsafe(naked)]
unsafe extern "C" fn general_protection_stub() {
    core::arch::naked_asm!(
        "mov rdi, [rsp]",
        "lea rsi, [rsp + 8]",
        "call {handler}",
        "2: hlt; jmp 2b",
        handler = sym gp_fault_handler,
    );
}

extern "C" fn page_fault_handler(error_code: u64, frame: &InterruptFrame) {
    let addr = read_cr2();
    LAST_PF_ADDR.store(addr, Ordering::SeqCst);
    LAST_PF_ERR.store(error_code, Ordering::SeqCst);
    PAGE_FAULT_COUNT.fetch_add(1, Ordering::SeqCst);

    if EXPECT_PAGE_FAULT.load(Ordering::SeqCst) {
        // Recuperación controlada: saltar al punto seguro guardado.
        // Safety: RECOVERY fue preparado por recovery_call.
        unsafe {
            recovery_resume();
        }
    }

    let mut serial = Serial::new();
    serial.write_crlf();
    let _ = writeln!(
        serial,
        "A5: UNEXPECTED PAGE FAULT: addr=0x{addr:016X} err=0x{error_code:X} rip=0x{:016X}",
        frame.rip
    );
    let _ = writeln!(serial, "A5: process isolation test FAIL: page fault no controlado");
    halt_forever();
}

extern "C" fn double_fault_handler(error_code: u64, frame: &InterruptFrame) {
    let mut serial = Serial::new();
    serial.write_crlf();
    let _ = writeln!(serial, "A5: DOUBLE FAULT err=0x{error_code:X} rip=0x{:016X}", frame.rip);
    let _ = writeln!(serial, "A5: FAIL: doble falta");
    halt_forever();
}

extern "C" fn gp_fault_handler(error_code: u64, frame: &InterruptFrame) {
    let mut serial = Serial::new();
    serial.write_crlf();
    let _ = writeln!(
        serial,
        "A5: GENERAL PROTECTION FAULT err=0x{error_code:X} rip=0x{:016X}",
        frame.rip
    );
    let _ = writeln!(serial, "A5: FAIL: fallo de protección general");
    halt_forever();
}

#[inline]
fn read_cr2() -> u64 {
    let value: u64;
    unsafe {
        asm!("mov {}, cr2", out(reg) value, options(nomem, nostack, preserves_flags));
    }
    value
}

fn halt_forever() -> ! {
    loop {
        unsafe {
            asm!("cli; hlt", options(nomem, nostack, preserves_flags));
        }
    }
}
