//! Controladores de dispositivos del kernel de Meridian (T1.2).
//!
//! Este módulo agrupa el acceso a configuración PCI por I/O ports y la capa
//! virtio-PCI/virtio-blk usada por el servicio de bloques. Se compila desde
//! `kernel/src/main.rs` mediante `#[path = "../drivers/mod.rs"]`.

pub mod pci;
pub mod virtio_blk;
pub mod virtio_pci;
