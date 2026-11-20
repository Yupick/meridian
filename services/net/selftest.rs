//! Auto-pruebas del stack de red sobre virtio-net (T2.1).
//!
//! Emite los 8 marcadores exigidos por QA. El tráfico es real host<->guest a
//! través del backend UDP de QEMU y `tools/net-peer.py`; aquí solo se procesa
//! la cola RX y se responden frames durante una ventana acotada.

use core::fmt::Write;

use crate::console::Console;
use crate::drivers::virtio_net;

use super::{self as net, NetStackError, Progress};

const POLL_ITERATIONS: u32 = 40_000_000;

struct Failure {
    step: &'static str,
    message: &'static str,
}

fn failure(step: &'static str, message: &'static str) -> Failure {
    Failure { step, message }
}

pub fn run(console: &mut Console) -> bool {
    match run_inner(console) {
        Ok(()) => true,
        Err(failed) => {
            let _ = writeln!(console, "T2.1: {} FAIL: {}", failed.step, failed.message);
            // El script QA también busca marcadores individuales; si no llegaron,
            // el resumen implícito del fallo global ya está cubierto.
            false
        }
    }
}

fn error_message(error: NetStackError) -> &'static str {
    match error {
        NetStackError::Driver(virtio_net::NetError::NotFound) => "virtio-net no encontrado",
        NetStackError::Driver(virtio_net::NetError::Unsupported(msg)) => msg,
        NetStackError::Driver(virtio_net::NetError::Init(msg)) => msg,
        NetStackError::Driver(virtio_net::NetError::NotInitialized) => "driver no inicializado",
        NetStackError::Driver(virtio_net::NetError::TxQueueFull) => "cola TX llena",
        NetStackError::Driver(virtio_net::NetError::TxTimeout) => "timeout TX",
        NetStackError::Driver(virtio_net::NetError::FrameTooLarge) => "frame demasiado grande",
    }
}

fn run_inner(console: &mut Console) -> Result<(), Failure> {
    // 1. Detección/inicialización virtio-net.
    let info = net::init().map_err(|e| failure("virtio-net detect", error_message(e)))?;
    let mac_ok = info.mac == net::GUEST_MAC;
    if !mac_ok {
        return Err(failure("virtio-net detect", "MAC distinta de 52:54:00:00:00:15"));
    }
    let _ = writeln!(
        console,
        "T2.1: virtio-net detect PASS (mac={:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x} queue={})",
        info.mac[0], info.mac[1], info.mac[2], info.mac[3], info.mac[4], info.mac[5], info.queue_size
    );

    // Ventana de sondeo para recibir y responder tráfico del peer.
    let mut progress = Progress::default();
    let mut spins = 0u32;
    let mut ethernet_reported = false;
    let mut arp_reported = false;
    let mut icmp_reported = false;
    let mut udp_reported = false;
    let mut tcp_synack_reported = false;
    let mut tcp_hs_reported = false;
    let mut icmpv6_reported = false;

    while spins < POLL_ITERATIONS {
        net::poll(&mut progress).map_err(|e| failure("ethernet framing", error_message(e)))?;

        if progress.ethernet && !ethernet_reported {
            let _ = writeln!(console, "T2.1: ethernet framing PASS (ethertype=0x0806/0x0800/0x86dd)");
            ethernet_reported = true;
        }
        if progress.arp && !arp_reported {
            let _ = writeln!(console, "T2.1: ARP reply PASS (10.0.2.15 -> 10.0.2.2)");
            arp_reported = true;
        }
        if progress.icmp && !icmp_reported {
            let _ = writeln!(console, "T2.1: IPv4 ICMP ping reply PASS (echo 8 -> 0)");
            icmp_reported = true;
        }
        if progress.udp && !udp_reported {
            let _ = writeln!(console, "T2.1: UDP echo reply PASS (port={})", net::UDP_ECHO_PORT);
            udp_reported = true;
        }
        // Emitir SYN-ACK en cuanto lo enviemos; el handshake completo cuando llegue el ACK.
        if progress.tcp_synack && !tcp_synack_reported {
            // progreso intermedio no exigido como marcador independiente aparte del handshake,
            // pero útil para debug; no imprimimos marcador no listado.
            tcp_synack_reported = true;
        }
        if progress.tcp_handshake && !tcp_hs_reported {
            let _ = writeln!(console, "T2.1: TCP handshake PASS (port={} isn=0x1000)", net::TCP_PORT);
            tcp_hs_reported = true;
        }
        if progress.icmpv6 && !icmpv6_reported {
            let _ = writeln!(console, "T2.1: IPv6 ICMPv6 echo PASS (echo 128 -> 129)");
            icmpv6_reported = true;
        }

        if ethernet_reported
            && arp_reported
            && icmp_reported
            && udp_reported
            && tcp_hs_reported
            && icmpv6_reported
        {
            let _ = writeln!(console, "T2.1: network stack tests PASS");
            return Ok(());
        }

        spins += 1;
        if spins & 0x3fff == 0 {
            core::hint::spin_loop();
        }
    }

    // Diagnóstico del primer paso que falló, en orden lógico.
    if !ethernet_reported {
        return Err(failure("ethernet framing", "no se recibieron frames Ethernet válidos"));
    }
    if !arp_reported {
        return Err(failure("ARP reply", "no se recibió ARP request para 10.0.2.15"));
    }
    if !icmp_reported {
        return Err(failure("IPv4 ICMP ping reply", "no se recibió ICMP echo request IPv4"));
    }
    if !udp_reported {
        return Err(failure("UDP echo reply", "no se recibió UDP echo a puerto 9999"));
    }
    if !tcp_hs_reported {
        if progress.tcp_synack {
            return Err(failure("TCP handshake", "SYN-ACK enviado pero no llegó ACK final"));
        }
        return Err(failure("TCP handshake", "no se recibió TCP SYN a puerto 8080"));
    }
    if !icmpv6_reported {
        return Err(failure("IPv6 ICMPv6 echo", "no se recibió ICMPv6 echo request"));
    }
    Err(failure("network stack tests", "estado inconsistente"))
}
