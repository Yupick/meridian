//! Global Descriptor Table (GDT) y Task State Segment (TSS).
//!
//! El kernel arranca usando las estructuras de UEFI; para manejar excepciones de
//! forma robusta (p.ej. #PF/#DF con pila conocida) necesitamos GDT/TSS propios.
//! Se define un segmento de código y datos de kernel (64 bits, anillo 0) y un
//! descriptor TSS disponible con pilas IST dedicadas.

use core::arch::asm;
use core::mem::size_of;

pub const KERNEL_CODE_SELECTOR: u16 = 0x08;
pub const KERNEL_DATA_SELECTOR: u16 = 0x10;
pub const TSS_SELECTOR: u16 = 0x18;

/// Índice IST (1-based) usado para #DF.
pub const IST_DOUBLE_FAULT: u16 = 1;
/// Índice IST (1-based) usado para #PF (pruebas de aislamiento/fallos controlados).
pub const IST_PAGE_FAULT: u16 = 2;

#[repr(C, packed)]
#[derive(Clone, Copy, Default)]
pub struct DescriptorTablePointer {
    pub limit: u16,
    pub base: u64,
}

#[repr(C, packed)]
#[derive(Clone, Copy, Default)]
pub struct TaskStateSegment {
    _reserved0: u32,
    pub rsp: [u64; 3],
    _reserved1: u64,
    pub ist: [u64; 7],
    _reserved2: u64,
    _reserved3: u16,
    pub iomap_base: u16,
}

impl TaskStateSegment {
    pub const fn new() -> Self {
        Self {
            _reserved0: 0,
            rsp: [0; 3],
            _reserved1: 0,
            ist: [0; 7],
            _reserved2: 0,
            _reserved3: 0,
            iomap_base: size_of::<Self>() as u16,
        }
    }
}

#[repr(C, align(16))]
struct Gdt {
    entries: [u64; 7],
}

/// Pilas dedicadas para el kernel y para excepciones críticas.
/// Alineadas a 16 bytes para cumplir la ABI al entrar en handlers en Rust/asm.
#[repr(C, align(16))]
struct KernelStacks {
    kernel: [u8; KERNEL_STACK_SIZE],
    df: [u8; IST_STACK_SIZE],
    pf: [u8; IST_STACK_SIZE],
}

const KERNEL_STACK_SIZE: usize = 16 * 4096;
const IST_STACK_SIZE: usize = 16 * 4096;

static mut TSS: TaskStateSegment = TaskStateSegment::new();
static mut GDT: Gdt = Gdt { entries: [0; 7] };
static mut STACKS: KernelStacks = KernelStacks {
    kernel: [0; KERNEL_STACK_SIZE],
    df: [0; IST_STACK_SIZE],
    pf: [0; IST_STACK_SIZE],
};

/// Inicializa GDT/TSS propias, carga TR y conmuta a la pila de kernel.
///
/// # Safety
/// Debe llamarse una sola vez durante el arranque temprano, en contexto de
/// kernel con interrupciones deshabilitadas y memoria identidad-mapeada.
pub unsafe fn init() -> u64 {
    let tss_addr = core::ptr::addr_of!(TSS) as u64;
    let gdt_addr = core::ptr::addr_of!(GDT) as u64;
    let stacks_addr = core::ptr::addr_of!(STACKS) as *const KernelStacks;

    let kernel_top = stacks_addr as u64 + KERNEL_STACK_SIZE as u64;
    let df_top = kernel_top + IST_STACK_SIZE as u64;
    let pf_top = df_top + IST_STACK_SIZE as u64;

    TSS = TaskStateSegment::new();
    TSS.rsp[0] = kernel_top;
    TSS.ist[(IST_DOUBLE_FAULT as usize) - 1] = df_top;
    TSS.ist[(IST_PAGE_FAULT as usize) - 1] = pf_top;
    TSS.iomap_base = size_of::<TaskStateSegment>() as u16;

    let tss_limit = (size_of::<TaskStateSegment>() - 1) as u64;
    let tss_low = (tss_limit & 0xFFFF)
        | ((tss_addr & 0x00FF_FFFF) << 16)
        | (0x89u64 << 40) // present, type=9 (available 64-bit TSS)
        | ((tss_limit & 0xF_0000) << 32)
        | ((tss_addr & 0xFF00_0000) << 32);
    let tss_high = tss_addr >> 32;

    GDT.entries[0] = 0;
    // 0x08: kernel code, 64-bit (L=1), present, DPL=0
    GDT.entries[1] = (1u64 << 43) | (1u64 << 44) | (1u64 << 47) | (1u64 << 53);
    // 0x10: kernel data, present, writable, DPL=0
    GDT.entries[2] = (1u64 << 41) | (1u64 << 44) | (1u64 << 47);
    // 0x18: TSS descriptor (16 bytes)
    GDT.entries[3] = tss_low;
    GDT.entries[4] = tss_high;
    GDT.entries[5] = 0;
    GDT.entries[6] = 0;

    let gdtr = DescriptorTablePointer {
        limit: (size_of::<Gdt>() - 1) as u16,
        base: gdt_addr,
    };

    // Carga GDT, recarga segmentos de datos, recarga CS con far return y carga TR.
    asm!(
        "lgdt [{gdtr}]",
        "mov ds, {data:x}",
        "mov es, {data:x}",
        "mov ss, {data:x}",
        "xor eax, eax",
        "mov fs, ax",
        "mov gs, ax",
        "push {code}",
        "lea rax, [rip + 2f]",
        "push rax",
        "retfq",
        "2:",
        "ltr {tss:x}",
        gdtr = in(reg) &gdtr,
        code = const KERNEL_CODE_SELECTOR as usize,
        data = in(reg) KERNEL_DATA_SELECTOR as u32,
        tss = in(reg) TSS_SELECTOR,
        out("rax") _,
    );

    // Conmutar a la pila propia del kernel y saltar al continuación.
    // Devolvemos el tope de la pila de kernel para que el llamador pueda
    // informar/usar si lo necesita; el cambio real se hace aquí.
    kernel_top
}

/// Cambia RSP a la pila de kernel propia y salta a `continuation`.
/// No retorna.
///
/// # Safety
/// `continuation` debe ser una función válida que nunca retorna y el estado de
/// CPU/memoria debe ser apto para ejecutarla en la nueva pila.
pub unsafe fn switch_to_kernel_stack_and_jump(continuation: extern "sysv64" fn() -> !) -> ! {
    let kernel_top = {
        let stacks_addr = core::ptr::addr_of!(STACKS) as *const KernelStacks;
        stacks_addr as u64 + KERNEL_STACK_SIZE as u64
    };
    // Alineamos a 16 bytes y dejamos el efecto de "call" (RSP%16==8 en entrada
    // de función) para ser conservadores.
    let aligned_top = kernel_top & !0xFu64;
    asm!(
        "mov rsp, {stack}",
        "xor rbp, rbp",
        "call {cont}",
        "ud2",
        stack = in(reg) aligned_top,
        cont = in(reg) continuation,
        options(noreturn)
    );
}

/// Devuelve el tope de la pila de kernel (para diagnóstico).
pub fn kernel_stack_top() -> u64 {
    let stacks_addr = core::ptr::addr_of!(STACKS) as *const KernelStacks;
    stacks_addr as u64 + KERNEL_STACK_SIZE as u64
}
