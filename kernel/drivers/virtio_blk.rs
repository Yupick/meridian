//! Dispositivo virtio-blk con dos transportes (T1.2):
//!
//! - `1af4:1001` (transicional): interfaz legacy por I/O ports de BAR0.
//! - `1af4:1042` (moderno): capabilities virtio-PCI sobre MMIO mapeado.
//!
//! En ambos casos se usan cuatro páginas contiguas para la virtqueue. El layout de la
//! cola se calcula a partir del `QUEUE_NUM` anunciado por el dispositivo. El
//! request de bloque encadena tres descriptores (header, datos y estado) y se
//! atiende con sondeo acotado de `used.idx`. Todas las estructuras compartidas
//! se acceden con `read_volatile`/`write_volatile` y barreras `SeqCst`.

use core::arch::asm;
use core::ptr;
use core::sync::atomic::{fence, Ordering};

use meridian_bootinfo::PAGE_SIZE;

use super::pci::{self, Bdf};
use super::virtio_pci;
use crate::mm::align_up;
use crate::mm::buddy;

/// Tamaño de sector obligatorio para el servicio.
pub const SECTOR_SIZE: usize = 512;
/// Tamaño de cola esperado por el selftest T1.2.
pub const QUEUE_SIZE: u16 = 16;

/// El dispositivo anuncia `QUEUE_NUM = 256`; el anillo debe dimensionarse con
/// ese valor exacto para que `avail` y `used` coincidan con el cálculo de QEMU.
const MAX_RING_ENTRIES: u16 = 256;
/// Orden del bloque contiguo del buddy: 4 páginas (el layout de 256 entradas
/// necesita 3, y el buddy solo devuelve potencias de dos).
const RING_ORDER: usize = 2;
const RING_PAGES: usize = 1 << RING_ORDER;
const RING_BYTES: u64 = RING_PAGES as u64 * PAGE_SIZE;

/// `VIRTIO_F_VERSION_1` es el bit 32: se negocia con `feature_select = 1`.
const FEATURE_SELECT_VERSION_1: u32 = 1;
const FEATURE_VERSION_1: u32 = 1 << 0;

const STATUS_RESET: u8 = 0;
const STATUS_ACKNOWLEDGE: u8 = 1;
const STATUS_DRIVER: u8 = 2;
const STATUS_DRIVER_OK: u8 = 4;
const STATUS_FEATURES_OK: u8 = 8;

// Offsets de la interfaz legacy (BAR0 I/O).
const LEGACY_HOST_FEATURES: u16 = 0x00;
const LEGACY_GUEST_FEATURES: u16 = 0x04;
const LEGACY_QUEUE_PFN: u16 = 0x08;
const LEGACY_QUEUE_NUM: u16 = 0x0c;
const LEGACY_QUEUE_SEL: u16 = 0x0e;
const LEGACY_QUEUE_NOTIFY: u16 = 0x10;
const LEGACY_STATUS: u16 = 0x12;
const LEGACY_CONFIG: u16 = 0x14;
const VIRTIO_PCI_QUEUE_ADDR_SHIFT: u32 = 12;

// Offsets de virtio_pci_common_cfg (verificados contra la spec 1.1).
const COMMON_DEVICE_FEATURE_SELECT: u64 = 0;
const COMMON_DEVICE_FEATURE: u64 = 4;
const COMMON_DRIVER_FEATURE_SELECT: u64 = 8;
const COMMON_DRIVER_FEATURE: u64 = 12;
const COMMON_DEVICE_STATUS: u64 = 20;
const COMMON_QUEUE_SELECT: u64 = 22;
const COMMON_QUEUE_SIZE: u64 = 24;
const COMMON_QUEUE_ENABLE: u64 = 28;
const COMMON_QUEUE_NOTIFY_OFF: u64 = 30;
const COMMON_QUEUE_DESC: u64 = 32;
const COMMON_QUEUE_DRIVER: u64 = 40;
const COMMON_QUEUE_DEVICE: u64 = 48;

const VIRTQ_DESC_F_NEXT: u16 = 1;
const VIRTQ_DESC_F_WRITE: u16 = 2;

const VIRTIO_BLK_T_IN: u32 = 0;
const VIRTIO_BLK_T_OUT: u32 = 1;
const VIRTIO_BLK_S_OK: u8 = 0;

const POLL_ITERATIONS: u32 = 20_000_000;

/// Transporte subyacente de un dispositivo virtio-blk.
#[derive(Clone, Copy, Debug)]
enum Transport {
    /// Dispositivo transicional: registros en los I/O ports de BAR0.
    Legacy { io: u16 },
    /// Dispositivo moderno: capabilities MMIO ya mapeadas.
    Modern {
        common: u64,
        notify: u64,
        device_cfg: u64,
        notify_multiplier: u32,
        notify_off: u16,
    },
}

/// Errores de la capa de dispositivo. El servicio los traduce a `BlockError`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlkError {
    Init(&'static str),
    QueueFull,
    Timeout,
    IoStatus(u8),
    UnexpectedDescriptor(u32),
}

/// Resultado del descubrimiento e inicialización.
pub struct Detected {
    pub devices: [Option<Device>; 2],
    pub count: usize,
}

/// Un dispositivo virtio-blk listo para servir peticiones.
#[derive(Clone, Copy, Debug)]
pub struct Device {
    pub bdf: Bdf,
    pub vendor: u16,
    pub device_id: u16,
    /// Tamaño reportado al servicio (topado en `QUEUE_SIZE` para el selftest).
    pub queue_size: u16,
    pub sectors: u64,
    page: u64,
    transport: Transport,
    /// Entradas reales del anillo según `QUEUE_NUM` (legacy) o la capability.
    ring_entries: u16,
    /// Próximo índice de `avail.idx`; los índices son contadores monotonicos.
    next_avail: u16,
    desc_off: u64,
    avail_off: u64,
    used_off: u64,
    header_off: u64,
    data_off: u64,
    status_off: u64,
}

/// Escanea el PCI y prepara hasta dos dispositivos (disco 0 y disco 1).
pub fn init_all() -> Result<Detected, BlkError> {
    let (pci_devices, count) = pci::scan_virtio_blk().map_err(BlkError::Init)?;
    let mut devices = [None, None];
    let mut index = 0usize;
    while index < count {
        let pci_device = pci_devices[index].ok_or(BlkError::Init("missing virtio-blk device"))?;
        let transport = if pci_device.device == pci::DEVICE_VIRTIO_BLK_LEGACY {
            if pci_device.io_base == 0 {
                return Err(BlkError::Init("virtio-blk legacy I/O BAR missing"));
            }
            Transport::Legacy {
                io: pci_device.io_base,
            }
        } else {
            let cfg = virtio_pci::parse(&pci_device).map_err(BlkError::Init)?;
            Transport::Modern {
                common: cfg.common.base,
                notify: cfg.notify.base,
                device_cfg: cfg.device_cfg.base,
                notify_multiplier: cfg.notify_multiplier,
                notify_off: 0,
            }
        };
        devices[index] = Some(Device::new(pci_device, transport)?);
        index += 1;
    }
    Ok(Detected { devices, count })
}

impl Device {
    fn new(pci_device: pci::VirtioBlkDevice, transport: Transport) -> Result<Self, BlkError> {
        // El buddy solo gestiona memoria por debajo de 1 GiB, cubierta por el
        // mapeo identidad 0..4 GiB instalado en A4. Se reservan cuatro páginas
        // contiguas: con `QUEUE_NUM = 256`, `avail` empieza en la segunda página
        // y `used` debe quedar alineado a 4096 (tercera página).
        let page = buddy::alloc(RING_ORDER).ok_or(BlkError::Init("no free pages for virtqueue"))?;
        // SAFETY: el buddy acaba de reservar el bloque de orden `RING_ORDER` y está mapeado RW.
        unsafe {
            ptr::write_bytes(page as *mut u8, 0, RING_BYTES as usize);
        }

        let mut device = Self {
            bdf: pci_device.bdf,
            vendor: pci_device.vendor,
            device_id: pci_device.device,
            queue_size: 0,
            sectors: 0,
            page,
            transport,
            ring_entries: 0,
            next_avail: 0,
            desc_off: 0,
            avail_off: 0,
            used_off: 0,
            header_off: 0,
            data_off: 0,
            status_off: 0,
        };
        match device.initialize() {
            Ok(()) => Ok(device),
            Err(err) => {
                let _ = buddy::free(page, RING_ORDER);
                Err(err)
            }
        }
    }

    fn initialize(&mut self) -> Result<(), BlkError> {
        match self.transport {
            Transport::Legacy { io } => self.initialize_legacy(io),
            Transport::Modern {
                common, device_cfg, ..
            } => self.initialize_modern(common, device_cfg),
        }
    }

    /// Arranque de un dispositivo transicional usando solo I/O ports.
    fn initialize_legacy(&mut self, io: u16) -> Result<(), BlkError> {
        outb(io + LEGACY_STATUS, STATUS_RESET);
        outb(io + LEGACY_STATUS, STATUS_ACKNOWLEDGE);
        outb(io + LEGACY_STATUS, STATUS_DRIVER);

        // Legacy no ofrece VIRTIO_F_VERSION_1: se leen las features y se
        // anuncian cero features del driver. No hay FEATURES_OK.
        let _host_features = inl(io + LEGACY_HOST_FEATURES);
        outl(io + LEGACY_GUEST_FEATURES, 0);

        outw(io + LEGACY_QUEUE_SEL, 0);
        let reported = inw(io + LEGACY_QUEUE_NUM);
        if reported < 3 {
            return Err(BlkError::Init("virtqueue 0 has fewer than 3 descriptors"));
        }
        self.set_layout(reported)?;

        let pfn = (self.page >> VIRTIO_PCI_QUEUE_ADDR_SHIFT) as u32;
        outl(io + LEGACY_QUEUE_PFN, pfn);

        self.sectors =
            u64::from(inl(io + LEGACY_CONFIG)) | (u64::from(inl(io + LEGACY_CONFIG + 4)) << 32);
        if self.sectors == 0 {
            return Err(BlkError::Init("virtio-blk device reports zero sectors"));
        }

        outb(
            io + LEGACY_STATUS,
            STATUS_ACKNOWLEDGE | STATUS_DRIVER | STATUS_DRIVER_OK,
        );
        Ok(())
    }

    /// Arranque de un dispositivo moderno (ruta MMIO actual).
    fn initialize_modern(&mut self, common: u64, device_cfg: u64) -> Result<(), BlkError> {
        mmio_write8(common, COMMON_DEVICE_STATUS, STATUS_RESET);
        mmio_write8(common, COMMON_DEVICE_STATUS, STATUS_ACKNOWLEDGE);
        mmio_write8(common, COMMON_DEVICE_STATUS, STATUS_DRIVER);

        mmio_write32(common, COMMON_DEVICE_FEATURE_SELECT, 0);
        mmio_write32(common, COMMON_DRIVER_FEATURE_SELECT, 0);
        mmio_write32(common, COMMON_DRIVER_FEATURE, 0);
        mmio_write32(
            common,
            COMMON_DEVICE_FEATURE_SELECT,
            FEATURE_SELECT_VERSION_1,
        );
        let features = mmio_read32(common, COMMON_DEVICE_FEATURE);
        if features & FEATURE_VERSION_1 == 0 {
            return Err(BlkError::Init("VIRTIO_F_VERSION_1 not offered by device"));
        }
        mmio_write32(
            common,
            COMMON_DRIVER_FEATURE_SELECT,
            FEATURE_SELECT_VERSION_1,
        );
        mmio_write32(common, COMMON_DRIVER_FEATURE, FEATURE_VERSION_1);

        let status = STATUS_ACKNOWLEDGE | STATUS_DRIVER | STATUS_FEATURES_OK;
        mmio_write8(common, COMMON_DEVICE_STATUS, status);
        if mmio_read8(common, COMMON_DEVICE_STATUS) & STATUS_FEATURES_OK == 0 {
            return Err(BlkError::Init("FEATURES_OK was not accepted"));
        }

        self.setup_queue_modern(common)?;
        // SAFETY: `capacity` (le64) está en el offset 0 de DEVICE_CFG.
        self.sectors = unsafe { ptr::read_volatile(device_cfg as *const u64) };
        if self.sectors == 0 {
            return Err(BlkError::Init("virtio-blk device reports zero sectors"));
        }

        mmio_write8(common, COMMON_DEVICE_STATUS, status | STATUS_DRIVER_OK);
        Ok(())
    }

    fn setup_queue_modern(&mut self, common: u64) -> Result<(), BlkError> {
        mmio_write16(common, COMMON_QUEUE_SELECT, 0);
        let reported = mmio_read16(common, COMMON_QUEUE_SIZE);
        if reported < 3 {
            return Err(BlkError::Init("virtqueue 0 has fewer than 3 descriptors"));
        }
        self.set_layout(reported)?;

        mmio_write16(common, COMMON_QUEUE_ENABLE, 0);
        mmio_write64(common, COMMON_QUEUE_DESC, self.page + self.desc_off);
        mmio_write64(common, COMMON_QUEUE_DRIVER, self.page + self.avail_off);
        mmio_write64(common, COMMON_QUEUE_DEVICE, self.page + self.used_off);
        mmio_write16(common, COMMON_QUEUE_ENABLE, 1);

        let notify_off = mmio_read16(common, COMMON_QUEUE_NOTIFY_OFF);
        if let Transport::Modern {
            notify_off: slot, ..
        } = &mut self.transport
        {
            *slot = notify_off;
        }
        Ok(())
    }

    /// Calcula los offsets del vring dentro del bloque contiguo de cuatro páginas.
    ///
    /// El dispositivo dimensiona el anillo con `QUEUE_NUM`, así que los offsets
    /// se derivan de `reported` (topado en `MAX_RING_ENTRIES`) y no al revés.
    /// `used` se alinea a `PAGE_SIZE` como exige `vring_init()`; `queue_size` se
    /// topa en `QUEUE_SIZE` para no cambiar el marcador QA.
    fn set_layout(&mut self, reported: u16) -> Result<(), BlkError> {
        let mut entries = reported.min(MAX_RING_ENTRIES);
        loop {
            let avail_off = align_up(16 * entries as u64, 2);
            let used_off = align_up(avail_off + 4 + 2 * entries as u64, PAGE_SIZE);
            let header_off = align_up(used_off + 4 + 8 * entries as u64, 16);
            let data_off = header_off + 16;
            let status_off = data_off + SECTOR_SIZE as u64;

            if status_off + 1 <= RING_BYTES {
                self.ring_entries = entries;
                self.queue_size = entries.min(QUEUE_SIZE);
                self.desc_off = 0;
                self.avail_off = avail_off;
                self.used_off = used_off;
                self.header_off = header_off;
                self.data_off = data_off;
                self.status_off = status_off;
                return Ok(());
            }
            if entries <= 3 {
                return Err(BlkError::Init("virtqueue does not fit in ring block"));
            }
            entries -= 1;
        }
    }

    /// Lee un sector de 512 bytes en `out`.
    pub fn read_sector(
        &mut self,
        sector: u64,
        out: &mut [u8; SECTOR_SIZE],
    ) -> Result<(), BlkError> {
        self.request(VIRTIO_BLK_T_IN, sector, None, Some(out))
    }

    /// Escribe un sector de 512 bytes desde `data`.
    pub fn write_sector(&mut self, sector: u64, data: &[u8; SECTOR_SIZE]) -> Result<(), BlkError> {
        self.request(VIRTIO_BLK_T_OUT, sector, Some(data), None)
    }

    fn request(
        &mut self,
        request_type: u32,
        sector: u64,
        source: Option<&[u8; SECTOR_SIZE]>,
        target: Option<&mut [u8; SECTOR_SIZE]>,
    ) -> Result<(), BlkError> {
        if self.ring_entries < 3 {
            return Err(BlkError::Init("virtqueue too small for block request"));
        }

        let desc_base = self.page + self.desc_off;
        let header = self.page + self.header_off;
        let data = self.page + self.data_off;
        let status = self.page + self.status_off;
        let avail = self.page + self.avail_off;
        let used = self.page + self.used_off;

        // SAFETY: las direcciones son offsets dentro del bloque reservado.
        unsafe {
            // Descriptor 0: header (solo lectura por el dispositivo).
            write_desc(desc_base, 0, header, 16, VIRTQ_DESC_F_NEXT, 1);
            // Descriptor 1: datos. En T_IN el dispositivo escribe (F_WRITE);
            // en T_OUT el dispositivo lee.
            let data_flags = VIRTQ_DESC_F_NEXT
                | if request_type == VIRTIO_BLK_T_IN {
                    VIRTQ_DESC_F_WRITE
                } else {
                    0
                };
            write_desc(desc_base, 1, data, SECTOR_SIZE as u32, data_flags, 2);
            // Descriptor 2: byte de estado, siempre escrito por el dispositivo.
            write_desc(desc_base, 2, status, 1, VIRTQ_DESC_F_WRITE, 0);
        }

        // SAFETY: offsets dentro del bloque de cuatro páginas de la virtqueue.
        unsafe {
            ptr::write_volatile(header as *mut u32, request_type);
            ptr::write_volatile((header + 4) as *mut u32, 0);
            ptr::write_volatile((header + 8) as *mut u64, sector);
            match source {
                Some(bytes) => {
                    for (index, byte) in bytes.iter().enumerate() {
                        ptr::write_volatile((data as *mut u8).add(index), *byte);
                    }
                }
                None => {
                    for index in 0..SECTOR_SIZE {
                        ptr::write_volatile((data as *mut u8).add(index), 0);
                    }
                }
            }
            ptr::write_volatile(status as *mut u8, 0xFF);
        }
        fence(Ordering::SeqCst);

        // Publicar un único descriptor en el avail ring. `avail.idx` y
        // `used.idx` son contadores monotonicos: reiniciarlos a cero después
        // de cada petición hace que QEMU descarte la siguiente notificación.
        let token = self.next_avail;
        if token == u16::MAX {
            return Err(BlkError::QueueFull);
        }
        let expected_used = token.wrapping_add(1);
        let avail_idx = avail + 2;
        let avail_ring = avail + 4 + 2 * (token % self.ring_entries) as u64;
        // SAFETY: `token % ring_entries` siempre cae dentro del anillo.
        unsafe {
            ptr::write_volatile(avail_ring as *mut u16, 0);
        }
        fence(Ordering::SeqCst);
        unsafe {
            ptr::write_volatile(avail_idx as *mut u16, expected_used);
        }
        self.next_avail = expected_used;
        fence(Ordering::SeqCst);

        // Kick según el transporte.
        match self.transport {
            Transport::Legacy { io } => outw(io + LEGACY_QUEUE_NOTIFY, 0),
            Transport::Modern {
                notify,
                notify_multiplier,
                notify_off,
                ..
            } => {
                let notify_address =
                    notify + (notify_off as u64).saturating_mul(notify_multiplier as u64);
                // SAFETY: dirección derivada de la capability NOTIFY.
                unsafe {
                    ptr::write_volatile(notify_address as *mut u16, 0);
                }
            }
        }
        fence(Ordering::SeqCst);

        // Espera acotada: nunca colgar QEMU si el dispositivo no responde.
        let used_idx = used + 2;
        let mut spins = 0u32;
        while unsafe { ptr::read_volatile(used_idx as *const u16) } != expected_used {
            spins += 1;
            // Leer un puerto I/O provoca VM-exit bajo KVM y deja que el
            // bottom-half de QEMU procese la cola mientras sondeamos.
            if spins & 0x3f == 0 {
                let _ = inb(0x80);
            }
            if spins >= POLL_ITERATIONS {
                return Err(BlkError::Timeout);
            }
            core::hint::spin_loop();
        }
        fence(Ordering::SeqCst);

        // SAFETY: offsets dentro del bloque de cuatro páginas de la virtqueue.
        let used_id = unsafe {
            ptr::read_volatile((used + 4 + 8 * (token % self.ring_entries) as u64) as *const u32)
        };
        let io_status = unsafe { ptr::read_volatile(status as *const u8) };
        if used_id != 0 {
            return Err(BlkError::UnexpectedDescriptor(used_id));
        }
        if io_status != VIRTIO_BLK_S_OK {
            return Err(BlkError::IoStatus(io_status));
        }

        if let Some(out) = target {
            // SAFETY: `data` se llenó con 512 bytes y el dispositivo ha
            // completado la petición (barrera SeqCst arriba).
            unsafe {
                for (index, byte) in out.iter_mut().enumerate() {
                    *byte = ptr::read_volatile((data as *const u8).add(index));
                }
            }
        }
        Ok(())
    }
}

/// Escribe un descriptor de 16 bytes en el anillo de descriptores.
///
/// # Safety
/// `desc_base` debe apuntar a memoria reservada y mapeada, e `index < ring_entries`.
unsafe fn write_desc(desc_base: u64, index: usize, address: u64, len: u32, flags: u16, next: u16) {
    let base = desc_base + (index as u64) * 16;
    ptr::write_volatile(base as *mut u64, address);
    ptr::write_volatile((base + 8) as *mut u32, len);
    ptr::write_volatile((base + 12) as *mut u16, flags);
    ptr::write_volatile((base + 14) as *mut u16, next);
}

#[inline]
fn mmio_read8(base: u64, offset: u64) -> u8 {
    fence(Ordering::SeqCst);
    let value = unsafe { ptr::read_volatile((base + offset) as *const u8) };
    fence(Ordering::SeqCst);
    value
}

#[inline]
fn mmio_read16(base: u64, offset: u64) -> u16 {
    fence(Ordering::SeqCst);
    let value = unsafe { ptr::read_volatile((base + offset) as *const u16) };
    fence(Ordering::SeqCst);
    value
}

#[inline]
fn mmio_read32(base: u64, offset: u64) -> u32 {
    fence(Ordering::SeqCst);
    let value = unsafe { ptr::read_volatile((base + offset) as *const u32) };
    fence(Ordering::SeqCst);
    value
}

#[inline]
fn mmio_write8(base: u64, offset: u64, value: u8) {
    fence(Ordering::SeqCst);
    unsafe {
        ptr::write_volatile((base + offset) as *mut u8, value);
    }
    fence(Ordering::SeqCst);
}

#[inline]
fn mmio_write16(base: u64, offset: u64, value: u16) {
    fence(Ordering::SeqCst);
    unsafe {
        ptr::write_volatile((base + offset) as *mut u16, value);
    }
    fence(Ordering::SeqCst);
}

#[inline]
fn mmio_write32(base: u64, offset: u64, value: u32) {
    fence(Ordering::SeqCst);
    unsafe {
        ptr::write_volatile((base + offset) as *mut u32, value);
    }
    fence(Ordering::SeqCst);
}

#[inline]
fn mmio_write64(base: u64, offset: u64, value: u64) {
    fence(Ordering::SeqCst);
    unsafe {
        ptr::write_volatile((base + offset) as *mut u64, value);
    }
    fence(Ordering::SeqCst);
}

#[allow(dead_code)]
#[inline]
fn inb(port: u16) -> u8 {
    let value: u8;
    fence(Ordering::SeqCst);
    // SAFETY: puerto de E/S válido devuelto por la BAR0 del dispositivo.
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
    // SAFETY: ver [`inb`].
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
    // SAFETY: ver [`inb`].
    unsafe {
        asm!("in eax, dx", in("dx") port, out("eax") value, options(nostack, preserves_flags));
    }
    fence(Ordering::SeqCst);
    value
}

#[inline]
fn outb(port: u16, value: u8) {
    fence(Ordering::SeqCst);
    // SAFETY: ver [`inb`].
    unsafe {
        asm!("out dx, al", in("dx") port, in("al") value, options(nostack, preserves_flags));
    }
    fence(Ordering::SeqCst);
}

#[inline]
fn outw(port: u16, value: u16) {
    fence(Ordering::SeqCst);
    // SAFETY: ver [`inb`].
    unsafe {
        asm!("out dx, ax", in("dx") port, in("ax") value, options(nostack, preserves_flags));
    }
    fence(Ordering::SeqCst);
}

#[inline]
fn outl(port: u16, value: u32) {
    fence(Ordering::SeqCst);
    // SAFETY: ver [`inb`].
    unsafe {
        asm!("out dx, eax", in("dx") port, in("eax") value, options(nostack, preserves_flags));
    }
    fence(Ordering::SeqCst);
}
