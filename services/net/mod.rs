//! Pila de red mínima para T2.1 (Ethernet/ARP/IPv4/IPv6/TCP/UDP).
//!
//! Implementa el procesamiento de frames Ethernet reales recibidos por
//! virtio-net y genera las respuestas exigidas por el peer QA del host.
//! Diseñada para no_std, sin heap y ejecución mono-CPU.

use crate::drivers::virtio_net;

pub mod selftest;

pub const GUEST_MAC: [u8; 6] = [0x52, 0x54, 0x00, 0x00, 0x00, 0x15];
pub const HOST_MAC: [u8; 6] = [0x52, 0x54, 0x00, 0x00, 0x00, 0x02];
pub const GUEST_IPV4: [u8; 4] = [10, 0, 2, 15];
pub const HOST_IPV4: [u8; 4] = [10, 0, 2, 2];
pub const GUEST_IPV6: [u8; 16] = [0xfd, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x15];
#[allow(dead_code)]
pub const HOST_IPV6: [u8; 16] = [0xfd, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x02];
pub const TCP_PORT: u16 = 8080;
pub const UDP_ECHO_PORT: u16 = 9999;
pub const TCP_ISN: u32 = 0x1000;

pub const ETHERTYPE_ARP: u16 = 0x0806;
pub const ETHERTYPE_IPV4: u16 = 0x0800;
pub const ETHERTYPE_IPV6: u16 = 0x86DD;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Progress {
    pub ethernet: bool,
    pub arp: bool,
    pub icmp: bool,
    pub udp: bool,
    pub tcp_synack: bool,
    pub tcp_handshake: bool,
    pub icmpv6: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NetStackError {
    Driver(virtio_net::NetError),
}

impl From<virtio_net::NetError> for NetStackError {
    fn from(e: virtio_net::NetError) -> Self {
        Self::Driver(e)
    }
}

/// Inicializa el driver virtio-net subyacente.
pub fn init() -> Result<virtio_net::NetInfo, NetStackError> {
    Ok(virtio_net::init()?)
}

/// Procesa todos los frames RX disponibles y devuelve los eventos observados
/// en esta ronda. Envía las respuestas necesarias por TX.
pub fn poll(progress: &mut Progress) -> Result<(), NetStackError> {
    let mut buf = [0u8; 1600];
    loop {
        let Some(len) = virtio_net::poll_rx(&mut buf) else {
            break;
        };
        if len < 14 {
            continue;
        }
        handle_frame(&buf[..len], progress)?;
    }
    Ok(())
}

fn handle_frame(frame: &[u8], progress: &mut Progress) -> Result<(), NetStackError> {
    let dst = &frame[0..6];
    if dst != GUEST_MAC && dst != [0xff; 6] {
        return Ok(());
    }
    let ethertype = u16::from_be_bytes([frame[12], frame[13]]);
    progress.ethernet = true;
    match ethertype {
        ETHERTYPE_ARP => handle_arp(frame, progress)?,
        ETHERTYPE_IPV4 => handle_ipv4(frame, progress)?,
        ETHERTYPE_IPV6 => handle_ipv6(frame, progress)?,
        _ => {}
    }
    Ok(())
}

fn handle_arp(frame: &[u8], progress: &mut Progress) -> Result<(), NetStackError> {
    // Ethernet(14) + ARP(28)
    if frame.len() < 42 {
        return Ok(());
    }
    let arp = &frame[14..];
    let htype = u16::from_be_bytes([arp[0], arp[1]]);
    let ptype = u16::from_be_bytes([arp[2], arp[3]]);
    let hlen = arp[4];
    let plen = arp[5];
    let oper = u16::from_be_bytes([arp[6], arp[7]]);
    if htype != 1 || ptype != ETHERTYPE_IPV4 || hlen != 6 || plen != 4 || oper != 1 {
        return Ok(());
    }
    let _sha = &arp[8..14];
    let _spa = &arp[14..18];
    let _tha = &arp[18..24];
    let tpa = &arp[24..28];
    if tpa != GUEST_IPV4 {
        return Ok(());
    }

    let mut reply = [0u8; 42];
    // Ethernet header
    reply[0..6].copy_from_slice(HOST_MAC.as_ref());
    reply[6..12].copy_from_slice(GUEST_MAC.as_ref());
    reply[12..14].copy_from_slice(&ETHERTYPE_ARP.to_be_bytes());
    // ARP
    reply[14..16].copy_from_slice(&1u16.to_be_bytes());
    reply[16..18].copy_from_slice(&ETHERTYPE_IPV4.to_be_bytes());
    reply[18] = 6;
    reply[19] = 4;
    reply[20..22].copy_from_slice(&2u16.to_be_bytes()); // reply
    reply[22..28].copy_from_slice(GUEST_MAC.as_ref());
    reply[28..32].copy_from_slice(GUEST_IPV4.as_ref());
    reply[32..38].copy_from_slice(HOST_MAC.as_ref());
    reply[38..42].copy_from_slice(HOST_IPV4.as_ref());

    virtio_net::send(&reply)?;
    progress.arp = true;
    Ok(())
}

fn handle_ipv4(frame: &[u8], progress: &mut Progress) -> Result<(), NetStackError> {
    if frame.len() < 34 {
        return Ok(());
    }
    let ip = &frame[14..];
    let ver_ihl = ip[0];
    if ver_ihl >> 4 != 4 {
        return Ok(());
    }
    let ihl = ((ver_ihl & 0x0f) as usize) * 4;
    if ihl < 20 || frame.len() < 14 + ihl {
        return Ok(());
    }
    let total_len = u16::from_be_bytes([ip[2], ip[3]]) as usize;
    if total_len < ihl || 14 + total_len > frame.len() {
        // permitir paquetes donde el buffer sea mayor
        if 14 + ihl > frame.len() {
            return Ok(());
        }
    }
    let proto = ip[9];
    let src = &ip[12..16];
    let dst = &ip[16..20];
    if dst != GUEST_IPV4 {
        return Ok(());
    }
    let payload_end = core::cmp::min(frame.len(), 14 + if total_len >= ihl { total_len } else { frame.len() - 14 });
    let payload = &frame[14 + ihl..payload_end];

    match proto {
        1 => handle_icmpv4(payload, src, progress)?,
        17 => handle_udp(payload, src, progress)?,
        6 => handle_tcp(payload, src, progress)?,
        _ => {}
    }
    Ok(())
}

fn handle_icmpv4(payload: &[u8], src: &[u8], progress: &mut Progress) -> Result<(), NetStackError> {
    if payload.len() < 8 {
        return Ok(());
    }
    let typ = payload[0];
    let code = payload[1];
    if typ != 8 || code != 0 {
        return Ok(());
    }
    // Echo reply: type 0, rest same (id, seq, data), recompute checksum.
    let mut icmp = [0u8; 1480];
    let len = core::cmp::min(payload.len(), icmp.len());
    icmp[..len].copy_from_slice(&payload[..len]);
    icmp[0] = 0; // echo reply
    icmp[2] = 0;
    icmp[3] = 0;
    let csum = checksum(&icmp[..len]);
    icmp[2..4].copy_from_slice(&csum.to_be_bytes());
    send_ipv4(src, 1, &icmp[..len])?;
    progress.icmp = true;
    Ok(())
}

fn handle_udp(payload: &[u8], src: &[u8], progress: &mut Progress) -> Result<(), NetStackError> {
    if payload.len() < 8 {
        return Ok(());
    }
    let src_port = u16::from_be_bytes([payload[0], payload[1]]);
    let dst_port = u16::from_be_bytes([payload[2], payload[3]]);
    let udp_len = u16::from_be_bytes([payload[4], payload[5]]) as usize;
    if dst_port != UDP_ECHO_PORT {
        return Ok(());
    }
    let data_len = if udp_len >= 8 && udp_len <= payload.len() { udp_len } else { payload.len() };
    let data = &payload[8..data_len];
    let mut out = [0u8; 1480];
    if data_len > out.len() {
        return Ok(());
    }
    // swap ports
    out[0..2].copy_from_slice(&UDP_ECHO_PORT.to_be_bytes());
    out[2..4].copy_from_slice(&src_port.to_be_bytes());
    out[4..6].copy_from_slice(&(data.len() as u16 + 8).to_be_bytes());
    out[6..8].copy_from_slice(&0u16.to_be_bytes()); // checksum 0 valid in IPv4
    out[8..8 + data.len()].copy_from_slice(data);
    send_ipv4(src, 17, &out[..8 + data.len()])?;
    progress.udp = true;
    Ok(())
}

fn handle_tcp(payload: &[u8], src: &[u8], progress: &mut Progress) -> Result<(), NetStackError> {
    if payload.len() < 20 {
        return Ok(());
    }
    let src_port = u16::from_be_bytes([payload[0], payload[1]]);
    let dst_port = u16::from_be_bytes([payload[2], payload[3]]);
    let seq = u32::from_be_bytes([payload[4], payload[5], payload[6], payload[7]]);
    let ack = u32::from_be_bytes([payload[8], payload[9], payload[10], payload[11]]);
    let data_offset = ((payload[12] >> 4) as usize) * 4;
    if data_offset < 20 || data_offset > payload.len() {
        return Ok(());
    }
    let flags = payload[13];
    if dst_port != TCP_PORT {
        return Ok(());
    }

    let syn = (flags & 0x02) != 0;
    let ack_flag = (flags & 0x10) != 0;
    if syn && !ack_flag {
        // SYN -> SYN-ACK
        let mut tcp = [0u8; 40];
        tcp[0..2].copy_from_slice(&TCP_PORT.to_be_bytes());
        tcp[2..4].copy_from_slice(&src_port.to_be_bytes());
        tcp[4..8].copy_from_slice(&TCP_ISN.to_be_bytes());
        tcp[8..12].copy_from_slice(&seq.wrapping_add(1).to_be_bytes());
        tcp[12] = 5 << 4; // data offset 20
        tcp[13] = 0x12; // SYN|ACK
        tcp[14..16].copy_from_slice(&4096u16.to_be_bytes()); // window
        tcp[16..18].copy_from_slice(&0u16.to_be_bytes()); // checksum placeholder
        tcp[18..20].copy_from_slice(&0u16.to_be_bytes()); // urgent
        let csum = tcp_checksum(GUEST_IPV4, if src.len() == 4 { [src[0], src[1], src[2], src[3]] } else { HOST_IPV4 }, &tcp);
        tcp[16..18].copy_from_slice(&csum.to_be_bytes());
        send_ipv4(src, 6, &tcp)?;
        progress.tcp_synack = true;
        return Ok(());
    }

    if ack_flag && !syn && ack == TCP_ISN.wrapping_add(1) {
        progress.tcp_handshake = true;
    }
    Ok(())
}

fn handle_ipv6(frame: &[u8], progress: &mut Progress) -> Result<(), NetStackError> {
    if frame.len() < 14 + 40 {
        return Ok(());
    }
    let ip6 = &frame[14..];
    let version = ip6[0] >> 4;
    if version != 6 {
        return Ok(());
    }
    let payload_len = u16::from_be_bytes([ip6[4], ip6[5]]) as usize;
    let next_header = ip6[6];
    let src = &ip6[8..24];
    let dst = &ip6[24..40];
    if dst != GUEST_IPV6 {
        return Ok(());
    }
    if next_header != 58 {
        // No extension headers expected in QA
        return Ok(());
    }
    let start = 14 + 40;
    let end = core::cmp::min(frame.len(), start + payload_len);
    if end < start {
        return Ok(());
    }
    let payload = &frame[start..end];
    handle_icmpv6(payload, src, progress)
}

fn handle_icmpv6(payload: &[u8], src: &[u8], progress: &mut Progress) -> Result<(), NetStackError> {
    if payload.len() < 8 {
        return Ok(());
    }
    let typ = payload[0];
    let code = payload[1];
    if typ != 128 || code != 0 {
        // Echo Request
        return Ok(());
    }
    let mut icmp = [0u8; 1480];
    let len = core::cmp::min(payload.len(), icmp.len());
    icmp[..len].copy_from_slice(&payload[..len]);
    icmp[0] = 129; // Echo Reply
    icmp[2] = 0;
    icmp[3] = 0;
    let mut src_arr = [0u8; 16];
    src_arr.copy_from_slice(&src[..16]);
    let csum = icmpv6_checksum(GUEST_IPV6, src_arr, &icmp[..len]);
    icmp[2..4].copy_from_slice(&csum.to_be_bytes());
    send_ipv6(src_arr, 58, &icmp[..len])?;
    progress.icmpv6 = true;
    Ok(())
}

fn send_ipv4(dst_ip: &[u8], proto: u8, payload: &[u8]) -> Result<(), NetStackError> {
    let mut frame = [0u8; 1514];
    let ihl = 20usize;
    let total_len = ihl + payload.len();
    if 14 + total_len > frame.len() {
        return Ok(());
    }
    // Ethernet
    frame[0..6].copy_from_slice(HOST_MAC.as_ref());
    frame[6..12].copy_from_slice(GUEST_MAC.as_ref());
    frame[12..14].copy_from_slice(&ETHERTYPE_IPV4.to_be_bytes());
    // IPv4 header
    let ip = &mut frame[14..14 + ihl];
    ip[0] = 0x45; // v4, ihl=5
    ip[1] = 0;
    ip[2..4].copy_from_slice(&(total_len as u16).to_be_bytes());
    ip[4..6].copy_from_slice(&0u16.to_be_bytes()); // id
    ip[6..8].copy_from_slice(&0u16.to_be_bytes()); // flags/frag
    ip[8] = 64; // ttl
    ip[9] = proto;
    ip[10..12].copy_from_slice(&0u16.to_be_bytes()); // checksum
    ip[12..16].copy_from_slice(GUEST_IPV4.as_ref());
    if dst_ip.len() == 4 {
        ip[16..20].copy_from_slice(dst_ip);
    } else {
        ip[16..20].copy_from_slice(HOST_IPV4.as_ref());
    }
    let hsum = checksum(ip);
    ip[10..12].copy_from_slice(&hsum.to_be_bytes());
    // payload
    frame[14 + ihl..14 + ihl + payload.len()].copy_from_slice(payload);
    virtio_net::send(&frame[..14 + ihl + payload.len()])?;
    Ok(())
}

fn send_ipv6(dst_ip: [u8; 16], next_header: u8, payload: &[u8]) -> Result<(), NetStackError> {
    let mut frame = [0u8; 1514];
    let total = 14 + 40 + payload.len();
    if total > frame.len() {
        return Ok(());
    }
    frame[0..6].copy_from_slice(HOST_MAC.as_ref());
    frame[6..12].copy_from_slice(GUEST_MAC.as_ref());
    frame[12..14].copy_from_slice(&ETHERTYPE_IPV6.to_be_bytes());
    let ip = &mut frame[14..54];
    ip[0] = 0x60; // v6
    ip[1] = 0;
    ip[2] = 0;
    ip[3] = 0;
    ip[4..6].copy_from_slice(&(payload.len() as u16).to_be_bytes());
    ip[6] = next_header;
    ip[7] = 64; // hop limit
    ip[8..24].copy_from_slice(GUEST_IPV6.as_ref());
    ip[24..40].copy_from_slice(&dst_ip);
    frame[54..54 + payload.len()].copy_from_slice(payload);
    virtio_net::send(&frame[..total])?;
    Ok(())
}

fn checksum(data: &[u8]) -> u16 {
    let mut sum: u32 = 0;
    let mut i = 0;
    while i + 1 < data.len() {
        sum += u16::from_be_bytes([data[i], data[i + 1]]) as u32;
        i += 2;
    }
    if i < data.len() {
        sum += (data[i] as u32) << 8;
    }
    while (sum >> 16) != 0 {
        sum = (sum & 0xffff) + (sum >> 16);
    }
    !(sum as u16)
}

fn tcp_checksum(src: [u8; 4], dst: [u8; 4], tcp_segment: &[u8]) -> u16 {
    let mut sum: u32 = 0;
    // Pseudo-header
    sum += u16::from_be_bytes([src[0], src[1]]) as u32;
    sum += u16::from_be_bytes([src[2], src[3]]) as u32;
    sum += u16::from_be_bytes([dst[0], dst[1]]) as u32;
    sum += u16::from_be_bytes([dst[2], dst[3]]) as u32;
    sum += 6u32; // protocol TCP
    sum += tcp_segment.len() as u32;
    // TCP segment
    let mut i = 0;
    while i + 1 < tcp_segment.len() {
        sum += u16::from_be_bytes([tcp_segment[i], tcp_segment[i + 1]]) as u32;
        i += 2;
    }
    if i < tcp_segment.len() {
        sum += (tcp_segment[i] as u32) << 8;
    }
    while (sum >> 16) != 0 {
        sum = (sum & 0xffff) + (sum >> 16);
    }
    !(sum as u16)
}

fn icmpv6_checksum(src: [u8; 16], dst: [u8; 16], icmp: &[u8]) -> u16 {
    let mut sum: u32 = 0;
    let mut i = 0;
    while i < 16 {
        sum += u16::from_be_bytes([src[i], src[i + 1]]) as u32;
        i += 2;
    }
    i = 0;
    while i < 16 {
        sum += u16::from_be_bytes([dst[i], dst[i + 1]]) as u32;
        i += 2;
    }
    let len = icmp.len() as u32;
    sum += (len >> 16) & 0xffff;
    sum += len & 0xffff;
    sum += 58u32; // next header ICMPv6
    i = 0;
    while i + 1 < icmp.len() {
        sum += u16::from_be_bytes([icmp[i], icmp[i + 1]]) as u32;
        i += 2;
    }
    if i < icmp.len() {
        sum += (icmp[i] as u32) << 8;
    }
    while (sum >> 16) != 0 {
        sum = (sum & 0xffff) + (sum >> 16);
    }
    !(sum as u16)
}
