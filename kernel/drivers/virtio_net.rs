//! Dispositivo virtio-net transicional por I/O ports (T2.1).
//!
//! Soporta `1af4:1000` con `disable-modern=on` (QEMU virtio-net-pci transicional).
//! Dos colas split: 0=RX, 1=TX, cada una con 4 páginas contiguas y 256 entradas
//! de anillo. RX preposta buffers de `VIRTIO_NET_HDR_LEN + RX_DATA_LEN`
//! (device-write) y los repone al consumir; TX envía header + frame Ethernet
//! (device-readable) y espera el used con timeout acotado.
//!
//! El kernel es mono-CPU, sin interrupciones, sin heap y con identidad 0..4 GiB,
//! así que se usan `static` + `UnsafeCell` y accesos `volatile` + barreras
//! `SeqCst`, igual que en `virtio_blk`.

use core::arch::asm;
use core::cell::UnsafeCell;
use core::ptr;
use core::sync::atomic::{fence, Ordering};

use meridian_bootinfo::PAGE_SIZE;

use super::pci::{self, VirtioNetDevice};
use crate::mm::align_up;
use crate::mm::buddy;

/// Tamaño del header virtio-net legacy sin VIRTIO_NET_F_MRG_RXBUF (10 bytes).
pub const VIRTIO_NET_HDR_LEN: usize = 10;
/// Espacio máximo de frame por buffer RX/TX.
pub const RX_DATA_LEN: usize = 1536;
/// Tamaño de cola reportado.
#[allow(dead_code)]
pub const QUEUE_SIZE: u16 = 256;
const MAX_RING_ENTRIES: u16 = 256;
/// 4 páginas contiguas por cola.
const RING_ORDER: usize = 2;
const RING_PAGES: usize = 1 << RING_ORDER;
const RING_BYTES: u64 = RING_PAGES as u64 * PAGE_SIZE;
/// Buffers RX preposteados (suficiente para QA T2.1).
const RX_BUFFERS: usize = 32;

// Offsets legacy (BAR0 I/O).
const LEGACY_HOST_FEATURES: u16 = 0x00;
const LEGACY_GUEST_FEATURES: u16 = 0x04;
const LEGACY_QUEUE_PFN: u16 = 0x08;
const LEGACY_QUEUE_NUM: u16 = 0x0c;
const LEGACY_QUEUE_SEL: u16 = 0x0e;
const LEGACY_QUEUE_NOTIFY: u16 = 0x10;
const LEGACY_STATUS: u16 = 0x12;
#[allow(dead_code)]
const LEGACY_ISR: u16 = 0x13;
const VIRTIO_PCI_QUEUE_ADDR_SHIFT: u32 = 12;

const STATUS_RESET: u8 = 0;
const STATUS_ACKNOWLEDGE: u8 = 1;
const STATUS_DRIVER: u8 = 2;
const STATUS_DRIVER_OK: u8 = 4;

const VIRTIO_NET_F_MAC: u32 = 1 << 5;

const VIRTQ_DESC_F_WRITE: u16 = 2;

const POLL_TX_ITERATIONS: u32 = 20_000_000;

/// Errores del driver virtio-net.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NetError {
    NotFound,
    Unsupported(&'static str),
    Init(&'static str),
    NotInitialized,
    #[allow(dead_code)]
    TxQueueFull,
    TxTimeout,
    FrameTooLarge,
}

/// Información pública del dispositivo tras init.
#[derive(Clone, Copy, Debug)]
pub struct NetInfo {
    pub mac: [u8; 6],
    pub queue_size: u16,
    #[allow(dead_code)]
    pub bdf: (u8, u8, u8),
}

// Header virtio-net base: 12 bytes en legacy sin MRG_RXBUF (se usa longitud).

#[repr(C, align(4096))]
#[derive(Clone, Copy)]
struct RxBuf {
    hdr: [u8; VIRTIO_NET_HDR_LEN],
    data: [u8; RX_DATA_LEN],
}

impl RxBuf {
    const fn zeroed() -> Self {
        Self {
            hdr: [0; VIRTIO_NET_HDR_LEN],
            data: [0; RX_DATA_LEN],
        }
    }
}

#[repr(C, align(16))]
#[derive(Clone, Copy)]
struct TxBuf {
    hdr: [u8; VIRTIO_NET_HDR_LEN],
    data: [u8; RX_DATA_LEN],
}

impl TxBuf {
    const fn zeroed() -> Self {
        Self {
            hdr: [0; VIRTIO_NET_HDR_LEN],
            data: [0; RX_DATA_LEN],
        }
    }
}

#[derive(Clone, Copy)]
struct Queue {
    page: u64,
    ring_entries: u16,
    desc_off: u64,
    avail_off: u64,
    used_off: u64,
    next_avail: u16,
    last_used: u16,
}

impl Queue {
    const fn empty() -> Self {
        Self {
            page: 0,
            ring_entries: 0,
            desc_off: 0,
            avail_off: 0,
            used_off: 0,
            next_avail: 0,
            last_used: 0,
        }
    }

    fn layout(&mut self, reported: u16) -> Result<(), NetError> {
        let mut entries = reported.min(MAX_RING_ENTRIES);
        loop {
            let avail_off = align_up(16 * entries as u64, 2);
            let used_off = align_up(avail_off + 4 + 2 * entries as u64, PAGE_SIZE);
            let ring_end = align_up(used_off + 4 + 8 * entries as u64, 16);
            if ring_end <= RING_BYTES {
                self.ring_entries = entries;
                self.desc_off = 0;
                self.avail_off = avail_off;
                self.used_off = used_off;
                return Ok(());
            }
            if entries <= 3 {
                return Err(NetError::Init("virtio-net virtqueue does not fit"));
            }
            entries -= 1;
        }
    }
}

struct NetState {
    io: u16,
    mac: [u8; 6],
    rx: Queue,
    tx: Queue,
    rx_bufs: [RxBuf; RX_BUFFERS],
    tx_buf: TxBuf,
    initialized: bool,
}

impl NetState {
    const fn new() -> Self {
        Self {
            io: 0,
            mac: [0; 6],
            rx: Queue::empty(),
            tx: Queue::empty(),
            rx_bufs: [RxBuf::zeroed(); RX_BUFFERS],
            tx_buf: TxBuf::zeroed(),
            initialized: false,
        }
    }
}

struct GlobalNet(UnsafeCell<NetState>);
// SAFETY: kernel mono-CPU, IRQs desactivadas, sin reentrancia.
unsafe impl Sync for GlobalNet {}

static NET: GlobalNet = GlobalNet(UnsafeCell::new(NetState::new()));

fn with_state<R>(f: impl FnOnce(&mut NetState) -> R) -> R {
    // SAFETY: ver `GlobalNet`.
    unsafe { f(&mut *NET.0.get()) }
}

/// Inicializa el primer dispositivo virtio-net transicional encontrado.
pub fn init() -> Result<NetInfo, NetError> {
    let found = pci::scan_virtio_net().map_err(NetError::Init)?;
    let Some(dev) = found else {
        return Err(NetError::NotFound);
    };
    if dev.device == pci::DEVICE_VIRTIO_NET_MODERN
        || dev.device == pci::DEVICE_VIRTIO_NET_MODERN + 1
    {
        return Err(NetError::Unsupported(
            "virtio-net moderno (1af4:1040/1041) no soportado; use disable-modern=on",
        ));
    }
    if dev.device != pci::DEVICE_VIRTIO_NET_LEGACY {
        return Err(NetError::Unsupported("dispositivo virtio-net no reconocido"));
    }
    if dev.io_base == 0 {
        return Err(NetError::Init("virtio-net legacy I/O BAR missing"));
    }

    with_state(|state| {
        if state.initialized {
            return Ok(NetInfo {
                mac: state.mac,
                queue_size: state.rx.ring_entries,
                bdf: (dev.bdf.bus, dev.bdf.slot, dev.bdf.func),
            });
        }
        initialize_device(state, &dev)?;
        Ok(NetInfo {
            mac: state.mac,
            queue_size: state.rx.ring_entries,
            bdf: (dev.bdf.bus, dev.bdf.slot, dev.bdf.func),
        })
    })
}

fn initialize_device(state: &mut NetState, dev: &VirtioNetDevice) -> Result<(), NetError> {
    let io = dev.io_base;

    // Reservar bloques de anillo antes de tocar el dispositivo para poder
    // liberar en error de forma clara.
    let rx_page = buddy::alloc(RING_ORDER).ok_or(NetError::Init("no free pages for RX virtqueue"))?;
    let tx_page = match buddy::alloc(RING_ORDER) {
        Some(p) => p,
        None => {
            let _ = buddy::free(rx_page, RING_ORDER);
            return Err(NetError::Init("no free pages for TX virtqueue"));
        }
    };
    unsafe {
        ptr::write_bytes(rx_page as *mut u8, 0, RING_BYTES as usize);
        ptr::write_bytes(tx_page as *mut u8, 0, RING_BYTES as usize);
    }
    state.rx = Queue::empty();
    state.tx = Queue::empty();
    state.rx.page = rx_page;
    state.tx.page = tx_page;
    state.rx_bufs = [RxBuf::zeroed(); RX_BUFFERS];
    state.tx_buf = TxBuf::zeroed();

    // Reset y negociación legacy.
    outb(io + LEGACY_STATUS, STATUS_RESET);
    outb(io + LEGACY_STATUS, STATUS_ACKNOWLEDGE);
    outb(io + LEGACY_STATUS, STATUS_DRIVER);

    let host_features = inl(io + LEGACY_HOST_FEATURES);
    let guest_features = host_features & VIRTIO_NET_F_MAC;
    outl(io + LEGACY_GUEST_FEATURES, guest_features);

    // Configurar RX (queue 0).
    outw(io + LEGACY_QUEUE_SEL, 0);
    let rx_num = inw(io + LEGACY_QUEUE_NUM);
    if rx_num < 3 {
        cleanup_pages(state);
        return Err(NetError::Init("virtio-net RX queue too small"));
    }
    if let Err(err) = state.rx.layout(rx_num) {
        cleanup_pages(state);
        return Err(err);
    }
    outl(io + LEGACY_QUEUE_PFN, (rx_page >> VIRTIO_PCI_QUEUE_ADDR_SHIFT) as u32);

    // Configurar TX (queue 1).
    outw(io + LEGACY_QUEUE_SEL, 1);
    let tx_num = inw(io + LEGACY_QUEUE_NUM);
    if tx_num < 3 {
        cleanup_pages(state);
        return Err(NetError::Init("virtio-net TX queue too small"));
    }
    if let Err(err) = state.tx.layout(tx_num) {
        cleanup_pages(state);
        return Err(err);
    }
    outl(io + LEGACY_QUEUE_PFN, (tx_page >> VIRTIO_PCI_QUEUE_ADDR_SHIFT) as u32);

    // Leer MAC de device config. Legacy sin MSI-X: config en +20; con MSI-X: +24.
    let config_off = if has_msix(dev.bdf) { 24u16 } else { 20u16 };
    let mut mac = [0u8; 6];
    for (i, b) in mac.iter_mut().enumerate() {
        *b = inb(io + config_off + i as u16);
    }
    // Fallback a la MAC forzada por QEMU si el dispositivo no la expone bien.
    if mac == [0; 6] || mac == [0xff; 6] {
        mac = [0x52, 0x54, 0x00, 0x00, 0x00, 0x15];
    }
    state.mac = mac;
    state.io = io;

    outb(io + LEGACY_STATUS, STATUS_ACKNOWLEDGE | STATUS_DRIVER | STATUS_DRIVER_OK);

    // Prepostear buffers RX. Se usa correspondencia 1:1: desc i -> rx_bufs[i].
    let post_count = RX_BUFFERS.min(state.rx.ring_entries as usize);
    let mut i = 0usize;
    while i < post_count {
        post_rx_desc(state, i as u16);
        i += 1;
    }
    if post_count > 0 {
        let avail_idx_ptr = (state.rx.page + state.rx.avail_off + 2) as *mut u16;
        fence(Ordering::SeqCst);
        unsafe {
            ptr::write_volatile(avail_idx_ptr, state.rx.next_avail);
        }
        fence(Ordering::SeqCst);
        outw(io + LEGACY_QUEUE_NOTIFY, 0);
        fence(Ordering::SeqCst);
    }

    state.initialized = true;
    Ok(())
}

fn cleanup_pages(state: &mut NetState) {
    if state.rx.page != 0 {
        let _ = buddy::free(state.rx.page, RING_ORDER);
        state.rx.page = 0;
    }
    if state.tx.page != 0 {
        let _ = buddy::free(state.tx.page, RING_ORDER);
        state.tx.page = 0;
    }
}

fn has_msix(bdf: pci::Bdf) -> bool {
    let status = pci::pci_config_read(bdf.bus, bdf.slot, bdf.func, 0x04) >> 16;
    if (status & (1 << 4)) == 0 {
        return false;
    }
    let mut cap = (pci::pci_config_read(bdf.bus, bdf.slot, bdf.func, 0x34) & 0xFC) as u16;
    let mut guard = 0u8;
    while cap >= 0x40 && guard < 48 {
        let hdr = pci::pci_config_read(bdf.bus, bdf.slot, bdf.func, cap);
        let id = (hdr & 0xFF) as u8;
        if id == 0x11 {
            let ctrl = (hdr >> 16) as u16;
            return (ctrl & 0x8000) != 0;
        }
        let next = ((hdr >> 8) & 0xFC) as u16;
        if next == 0 || next == cap {
            break;
        }
        cap = next;
        guard += 1;
    }
    false
}

/// Publica un descriptor RX fijo `desc_id` (== índice de buffer) en el avail
/// ring sin actualizar `avail.idx` ni notificar; el llamador debe hacerlo en
/// bloque y mantener `next_avail` como contador monotónico.
fn post_rx_desc(state: &mut NetState, desc_id: u16) {
    let buf_index = desc_id as usize;
    if buf_index >= RX_BUFFERS {
        return;
    }
    let desc_base = state.rx.page + state.rx.desc_off;
    let buf_addr = &state.rx_bufs[buf_index] as *const RxBuf as u64;
    let len = (VIRTIO_NET_HDR_LEN + RX_DATA_LEN) as u32;
    unsafe {
        ptr::write_bytes(state.rx_bufs[buf_index].hdr.as_mut_ptr(), 0, VIRTIO_NET_HDR_LEN);
        write_desc(desc_base, desc_id as usize, buf_addr, len, VIRTQ_DESC_F_WRITE, 0);
        let ring_slot = (state.rx.page
            + state.rx.avail_off
            + 4
            + 2 * (state.rx.next_avail % state.rx.ring_entries) as u64)
            as *mut u16;
        ptr::write_volatile(ring_slot, desc_id);
    }
    state.rx.next_avail = state.rx.next_avail.wrapping_add(1);
}

/// Sondea la cola RX sin bloquear. Si hay un frame, copia hasta `out.len()`
/// bytes en `out` y devuelve `Some(len)`.
pub fn poll_rx(out: &mut [u8]) -> Option<usize> {
    with_state(|state| {
        if !state.initialized {
            return None;
        }
        let used_idx_ptr = (state.rx.page + state.rx.used_off + 2) as *const u16;
        let used_idx = unsafe { ptr::read_volatile(used_idx_ptr) };
        if state.rx.last_used == used_idx {
            // Yield breve al backend QEMU.
            let _ = inb(0x80);
            return None;
        }
        fence(Ordering::SeqCst);
        let ring_index = (state.rx.last_used % state.rx.ring_entries) as u64;
        let used_elem = state.rx.page + state.rx.used_off + 4 + ring_index * 8;
        let id = unsafe { ptr::read_volatile(used_elem as *const u32) } as u16;
        let len = unsafe { ptr::read_volatile((used_elem + 4) as *const u32) } as usize;
        state.rx.last_used = state.rx.last_used.wrapping_add(1);

        let buf_index = id as usize;
        if buf_index >= RX_BUFFERS {
            return None;
        }
        let payload_len = if len > VIRTIO_NET_HDR_LEN {
            len - VIRTIO_NET_HDR_LEN
        } else {
            0
        };
        let copy_len = core::cmp::min(payload_len, core::cmp::min(RX_DATA_LEN, out.len()));
        unsafe {
            let src = state.rx_bufs[buf_index].data.as_ptr();
            ptr::copy_nonoverlapping(src, out.as_mut_ptr(), copy_len);
        }

        // Reponer el mismo descriptor/buffer (1:1) en la cola RX.
        let desc_base = state.rx.page + state.rx.desc_off;
        let buf_addr = &state.rx_bufs[buf_index] as *const RxBuf as u64;
        let desc_len = (VIRTIO_NET_HDR_LEN + RX_DATA_LEN) as u32;
        unsafe {
            ptr::write_bytes(state.rx_bufs[buf_index].hdr.as_mut_ptr(), 0, VIRTIO_NET_HDR_LEN);
            write_desc(desc_base, buf_index, buf_addr, desc_len, VIRTQ_DESC_F_WRITE, 0);
            let ring_slot = (state.rx.page
                + state.rx.avail_off
                + 4
                + 2 * (state.rx.next_avail % state.rx.ring_entries) as u64)
                as *mut u16;
            ptr::write_volatile(ring_slot, id);
        }
        let new_idx = state.rx.next_avail.wrapping_add(1);
        let avail_idx_ptr = (state.rx.page + state.rx.avail_off + 2) as *mut u16;
        fence(Ordering::SeqCst);
        unsafe {
            ptr::write_volatile(avail_idx_ptr, new_idx);
        }
        state.rx.next_avail = new_idx;
        fence(Ordering::SeqCst);
        outw(state.io + LEGACY_QUEUE_NOTIFY, 0);
        fence(Ordering::SeqCst);

        Some(copy_len)
    })
}

/// Envía un frame Ethernet completo. Espera a que el dispositivo consuma el
/// descriptor (sondeo acotado) para poder reutilizar el buffer TX único.
pub fn send(frame: &[u8]) -> Result<(), NetError> {
    if frame.len() > RX_DATA_LEN {
        return Err(NetError::FrameTooLarge);
    }
    with_state(|state| {
        if !state.initialized {
            return Err(NetError::NotInitialized);
        }
        let tx = &mut state.tx;
        let used_idx_ptr = (tx.page + tx.used_off + 2) as *const u16;

        // Preparar buffer TX: header cero + frame.
        unsafe {
            ptr::write_bytes(state.tx_buf.hdr.as_mut_ptr(), 0, VIRTIO_NET_HDR_LEN);
            ptr::copy_nonoverlapping(frame.as_ptr(), state.tx_buf.data.as_mut_ptr(), frame.len());
        }
        let buf_addr = &state.tx_buf as *const TxBuf as u64;
        let total_len = (VIRTIO_NET_HDR_LEN + frame.len()) as u32;
        let desc_base = tx.page + tx.desc_off;
        let desc_id = (tx.next_avail % tx.ring_entries) as usize;
        unsafe {
            // Descriptor device-readable (sin WRITE).
            write_desc(desc_base, desc_id, buf_addr, total_len, 0, 0);
        }
        let avail_idx_ptr = (tx.page + tx.avail_off + 2) as *mut u16;
        let ring_slot = (tx.page + tx.avail_off + 4 + 2 * (tx.next_avail % tx.ring_entries) as u64)
            as *mut u16;
        let expected_used = tx.next_avail.wrapping_add(1);
        unsafe {
            ptr::write_volatile(ring_slot, desc_id as u16);
        }
        fence(Ordering::SeqCst);
        unsafe {
            ptr::write_volatile(avail_idx_ptr, expected_used);
        }
        tx.next_avail = expected_used;
        fence(Ordering::SeqCst);
        outw(state.io + LEGACY_QUEUE_NOTIFY, 1);
        fence(Ordering::SeqCst);

        let mut spins = 0u32;
        while unsafe { ptr::read_volatile(used_idx_ptr) } != expected_used {
            spins += 1;
            if spins & 0x3f == 0 {
                let _ = inb(0x80);
            }
            if spins >= POLL_TX_ITERATIONS {
                return Err(NetError::TxTimeout);
            }
            core::hint::spin_loop();
        }
        fence(Ordering::SeqCst);
        tx.last_used = expected_used;
        Ok(())
    })
}

/// Devuelve la MAC negociada/leída.
#[allow(dead_code)]
pub fn mac() -> Option<[u8; 6]> {
    with_state(|state| if state.initialized { Some(state.mac) } else { None })
}

unsafe fn write_desc(desc_base: u64, index: usize, address: u64, len: u32, flags: u16, next: u16) {
    let base = desc_base + (index as u64) * 16;
    ptr::write_volatile(base as *mut u64, address);
    ptr::write_volatile((base + 8) as *mut u32, len);
    ptr::write_volatile((base + 12) as *mut u16, flags);
    ptr::write_volatile((base + 14) as *mut u16, next);
}

#[inline]
fn inb(port: u16) -> u8 {
    let value: u8;
    fence(Ordering::SeqCst);
    unsafe {
        asm!("in al, dx", in("dx") port, out("al") value, options(nostack, preserves_flags));
    }
    fence(Ordering::SeqCst);
    value
}

#[inline]
fn inw(port: u16) -> u16 {
    let value: u16;
    fence(Ordering::SeqCst);
    unsafe {
        asm!("in ax, dx", in("dx") port, out("ax") value, options(nostack, preserves_flags));
    }
    fence(Ordering::SeqCst);
    value
}

#[inline]
fn inl(port: u16) -> u32 {
    let value: u32;
    fence(Ordering::SeqCst);
    unsafe {
        asm!("in eax, dx", in("dx") port, out("eax") value, options(nostack, preserves_flags));
    }
    fence(Ordering::SeqCst);
    value
}

#[inline]
fn outb(port: u16, value: u8) {
    fence(Ordering::SeqCst);
    unsafe {
        asm!("out dx, al", in("dx") port, in("al") value, options(nostack, preserves_flags));
    }
    fence(Ordering::SeqCst);
}

#[inline]
fn outw(port: u16, value: u16) {
    fence(Ordering::SeqCst);
    unsafe {
        asm!("out dx, ax", in("dx") port, in("ax") value, options(nostack, preserves_flags));
    }
    fence(Ordering::SeqCst);
}

#[inline]
fn outl(port: u16, value: u32) {
    fence(Ordering::SeqCst);
    unsafe {
        asm!("out dx, eax", in("dx") port, in("eax") value, options(nostack, preserves_flags));
    }
    fence(Ordering::SeqCst);
}
