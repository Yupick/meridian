//! Controladores de dispositivos del kernel de Meridian (T1.2, T2.1).
//!
//! Este módulo agrupa el acceso a configuración PCI por I/O ports y las capas
//! virtio-blk y virtio-net usadas por los servicios. Se compila desde
//! `kernel/src/main.rs` mediante `#[path = "../drivers/mod.rs"]`.

pub mod pci;
pub mod virtio_blk;
pub mod virtio_net;
pub mod virtio_pci;
