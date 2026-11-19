#!/usr/bin/env python3
"""Host-side QA peer for Meridian T2.1 network selftest.

Receives/sends raw Ethernet frames over a QEMU UDP dgram backend (no TAP/root).
Binds 127.0.0.1:55124 and peers with 127.0.0.1:55125.
Writes exact NETPEER PASS lines to build/net-peer.log and exits 0 only when all
six events are observed.
"""

from __future__ import annotations

import argparse
import os
import select
import socket
import struct
import sys
import time
from typing import Dict, List, Tuple

GUEST_MAC = bytes.fromhex("525400000015")
HOST_MAC = bytes.fromhex("525400000002")
BROADCAST_MAC = b"\xff\xff\xff\xff\xff\xff"
GUEST_IPV4 = socket.inet_aton("10.0.2.15")
HOST_IPV4 = socket.inet_aton("10.0.2.2")
GUEST_IPV6 = socket.inet_pton(socket.AF_INET6, "fd00::15")
HOST_IPV6 = socket.inet_pton(socket.AF_INET6, "fd00::2")
TCP_PORT = 8080
UDP_ECHO_PORT = 9999
TCP_ISN = 0x1000

ETH_ARP = 0x0806
ETH_IPV4 = 0x0800
ETH_IPV6 = 0x86DD


def checksum(data: bytes) -> int:
    if len(data) % 2:
        data += b"\x00"
    total = 0
    for i in range(0, len(data), 2):
        total += (data[i] << 8) | data[i + 1]
    while total >> 16:
        total = (total & 0xFFFF) + (total >> 16)
    return (~total) & 0xFFFF


def eth_header(dst: bytes, src: bytes, ethertype: int) -> bytes:
    return dst + src + struct.pack("!H", ethertype)


def build_arp_request() -> bytes:
    arp = struct.pack(
        "!HHBBH6s4s6s4s",
        1,  # htype Ethernet
        ETH_IPV4,
        6,
        4,
        1,  # request
        HOST_MAC,
        HOST_IPV4,
        BROADCAST_MAC,
        GUEST_IPV4,
    )
    return eth_header(BROADCAST_MAC, HOST_MAC, ETH_ARP) + arp


def build_icmp_echo_request(ident: int, seq: int, payload: bytes) -> bytes:
    icmp = struct.pack("!BBHHH", 8, 0, 0, ident & 0xFFFF, seq & 0xFFFF) + payload
    csum = checksum(icmp)
    icmp = struct.pack("!BBHHH", 8, 0, csum, ident & 0xFFFF, seq & 0xFFFF) + payload
    ip = build_ipv4_packet(HOST_IPV4, GUEST_IPV4, 1, icmp, ident & 0xFFFF)
    return eth_header(GUEST_MAC, HOST_MAC, ETH_IPV4) + ip


def build_ipv4_packet(src: bytes, dst: bytes, proto: int, payload: bytes, identification: int) -> bytes:
    total_len = 20 + len(payload)
    header = struct.pack(
        "!BBHHHBBH4s4s",
        0x45,
        0,
        total_len,
        identification & 0xFFFF,
        0,
        64,
        proto,
        0,
        src,
        dst,
    )
    csum = checksum(header)
    header = struct.pack(
        "!BBHHHBBH4s4s",
        0x45,
        0,
        total_len,
        identification & 0xFFFF,
        0,
        64,
        proto,
        csum,
        src,
        dst,
    )
    return header + payload


def build_udp_echo_request(src_port: int, payload: bytes) -> bytes:
    udp_len = 8 + len(payload)
    udp = struct.pack("!HHHH", src_port, UDP_ECHO_PORT, udp_len, 0) + payload
    ip = build_ipv4_packet(HOST_IPV4, GUEST_IPV4, 17, udp, 0x3344)
    return eth_header(GUEST_MAC, HOST_MAC, ETH_IPV4) + ip


def tcp_checksum(src: bytes, dst: bytes, tcp_segment: bytes) -> int:
    pseudo = src + dst + struct.pack("!BBH", 0, 6, len(tcp_segment))
    return checksum(pseudo + tcp_segment)


def build_tcp_syn(seq: int, src_port: int) -> bytes:
    data_offset = (5 << 4)
    flags = 0x02  # SYN
    tcp = struct.pack(
        "!HHIIBBHHH",
        src_port,
        TCP_PORT,
        seq,
        0,
        data_offset,
        flags,
        4096,
        0,
        0,
    )
    csum = tcp_checksum(HOST_IPV4, GUEST_IPV4, tcp)
    tcp = struct.pack(
        "!HHIIBBHHH",
        src_port,
        TCP_PORT,
        seq,
        0,
        data_offset,
        flags,
        4096,
        csum,
        0,
    )
    ip = build_ipv4_packet(HOST_IPV4, GUEST_IPV4, 6, tcp, 0x4455)
    return eth_header(GUEST_MAC, HOST_MAC, ETH_IPV4) + ip


def build_tcp_ack(seq: int, ack: int, src_port: int) -> bytes:
    data_offset = (5 << 4)
    flags = 0x10  # ACK
    tcp = struct.pack(
        "!HHIIBBHHH",
        src_port,
        TCP_PORT,
        seq,
        ack,
        data_offset,
        flags,
        4096,
        0,
        0,
    )
    csum = tcp_checksum(HOST_IPV4, GUEST_IPV4, tcp)
    tcp = struct.pack(
        "!HHIIBBHHH",
        src_port,
        TCP_PORT,
        seq,
        ack,
        data_offset,
        flags,
        4096,
        csum,
        0,
    )
    ip = build_ipv4_packet(HOST_IPV4, GUEST_IPV4, 6, tcp, 0x4456)
    return eth_header(GUEST_MAC, HOST_MAC, ETH_IPV4) + ip


def icmpv6_checksum(src: bytes, dst: bytes, payload: bytes) -> int:
    pseudo = src + dst + struct.pack("!IHBB", len(payload), 0, 0, 58)
    return checksum(pseudo + payload)


def build_icmpv6_echo_request(ident: int, seq: int, payload: bytes) -> bytes:
    icmp = struct.pack("!BBHHH", 128, 0, 0, ident & 0xFFFF, seq & 0xFFFF) + payload
    csum = icmpv6_checksum(HOST_IPV6, GUEST_IPV6, icmp)
    icmp = struct.pack("!BBHHH", 128, 0, csum, ident & 0xFFFF, seq & 0xFFFF) + payload
    ip6 = struct.pack(
        "!IHBB16s16s",
        0x60000000,
        len(icmp),
        58,
        64,
        HOST_IPV6,
        GUEST_IPV6,
    ) + icmp
    return eth_header(GUEST_MAC, HOST_MAC, ETH_IPV6) + ip6


def parse_ipv4(packet: bytes) -> Tuple[bytes, bytes, int, bytes]:
    if len(packet) < 20:
        return b"", b"", 0, b""
    ver_ihl = packet[0]
    if ver_ihl >> 4 != 4:
        return b"", b"", 0, b""
    ihl = (ver_ihl & 0x0F) * 4
    if ihl < 20 or len(packet) < ihl:
        return b"", b"", 0, b""
    total_len = struct.unpack("!H", packet[2:4])[0]
    proto = packet[9]
    src = packet[12:16]
    dst = packet[16:20]
    end = min(len(packet), total_len) if total_len >= ihl else len(packet)
    return src, dst, proto, packet[ihl:end]


def main() -> int:
    parser = argparse.ArgumentParser(description="Meridian T2.1 host UDP network peer")
    parser.add_argument("--bind-host", default="127.0.0.1")
    parser.add_argument("--bind-port", type=int, default=55124)
    parser.add_argument("--peer-host", default="127.0.0.1")
    parser.add_argument("--peer-port", type=int, default=55125)
    parser.add_argument("--log", default="build/net-peer.log")
    parser.add_argument("--timeout", type=float, default=20.0)
    args = parser.parse_args()

    os.makedirs(os.path.dirname(args.log) or ".", exist_ok=True)
    log_lines: List[str] = []

    def write_log(line: str) -> None:
        if line in log_lines:
            return
        log_lines.append(line)
        with open(args.log, "a", encoding="utf-8") as f:
            f.write(line + "\n")
            f.flush()

    # Truncate/create log at start.
    with open(args.log, "w", encoding="utf-8"):
        pass

    sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    sock.setblocking(False)
    sock.bind((args.bind_host, args.bind_port))
    peer = (args.peer_host, args.peer_port)

    seen: Dict[str, bool] = {
        "arp": False,
        "icmp": False,
        "udp": False,
        "tcp_synack": False,
        "tcp_ack_sent": False,
        "icmpv6": False,
    }

    ident = 0x1234
    icmp_seq = 1
    udp_payload = b"meridian-udp-echo"
    udp_src_port = 40000
    tcp_seq = 0x22334455
    tcp_src_port = 40001

    start = time.monotonic()
    last_send = 0.0
    deadline = start + args.timeout

    try:
        while time.monotonic() < deadline:
            now = time.monotonic()
            if now - last_send >= 0.2:
                # Resend outstanding requests.
                if not seen["arp"]:
                    sock.sendto(build_arp_request(), peer)
                if not seen["icmp"]:
                    sock.sendto(build_icmp_echo_request(ident, icmp_seq, b"ping-t21"), peer)
                if not seen["udp"]:
                    sock.sendto(build_udp_echo_request(udp_src_port, udp_payload), peer)
                if not seen["tcp_synack"]:
                    sock.sendto(build_tcp_syn(tcp_seq, tcp_src_port), peer)
                if not seen["icmpv6"]:
                    sock.sendto(build_icmpv6_echo_request(ident, 2, b"ping6-t21"), peer)
                last_send = now

            if all(seen.values()):
                return 0

            rlist, _, _ = select.select([sock], [], [], 0.05)
            if not rlist:
                continue
            try:
                data, _addr = sock.recvfrom(65535)
            except BlockingIOError:
                continue
            handle_frame(data, sock, peer, seen, write_log, udp_payload, udp_src_port, tcp_seq, tcp_src_port)

        if not all(seen.values()):
            missing = [k for k, v in seen.items() if not v]
            sys.stderr.write(f"NETPEER: TIMEOUT missing={','.join(missing)}\n")
            return 1
        return 0
    finally:
        sock.close()


def handle_frame(
    frame: bytes,
    sock: socket.socket,
    peer: Tuple[str, int],
    seen: Dict[str, bool],
    write_log,
    udp_payload: bytes,
    udp_src_port: int,
    tcp_seq: int,
    tcp_src_port: int,
) -> None:
    if len(frame) < 14:
        return
    dst = frame[0:6]
    if dst not in (HOST_MAC, BROADCAST_MAC):
        return
    ethertype = struct.unpack("!H", frame[12:14])[0]
    payload = frame[14:]

    if ethertype == ETH_ARP and len(payload) >= 28:
        oper = struct.unpack("!H", payload[6:8])[0]
        sha = payload[8:14]
        spa = payload[14:18]
        tpa = payload[24:28]
        if oper == 2 and sha == GUEST_MAC and spa == GUEST_IPV4 and tpa == HOST_IPV4:
            seen["arp"] = True
            write_log("NETPEER: ARP reply PASS")
        return

    if ethertype == ETH_IPV4:
        src_ip, dst_ip, proto, ip_payload = parse_ipv4(payload)
        if dst_ip != HOST_IPV4 or src_ip != GUEST_IPV4:
            return
        if proto == 1 and len(ip_payload) >= 8:
            typ, code = ip_payload[0], ip_payload[1]
            if typ == 0 and code == 0:
                seen["icmp"] = True
                write_log("NETPEER: ICMP echo reply PASS")
            return
        if proto == 17 and len(ip_payload) >= 8:
            sport, dport, ulen, _ucsum = struct.unpack("!HHHH", ip_payload[:8])
            data = ip_payload[8 : min(len(ip_payload), ulen)] if ulen >= 8 else ip_payload[8:]
            if sport == UDP_ECHO_PORT and dport == udp_src_port and data == udp_payload:
                seen["udp"] = True
                write_log("NETPEER: UDP echo reply PASS")
            return
        if proto == 6 and len(ip_payload) >= 20:
            sport, dport, seq, ack, off_flags = struct.unpack("!HHIIH", ip_payload[:14])
            data_off = ((off_flags >> 12) & 0xF) * 4
            flags = off_flags & 0x3F
            if sport == TCP_PORT and dport == tcp_src_port:
                if flags & 0x12 == 0x12 and seq == TCP_ISN and ack == (tcp_seq + 1) & 0xFFFFFFFF:
                    if not seen["tcp_synack"]:
                        seen["tcp_synack"] = True
                        write_log("NETPEER: TCP SYN-ACK PASS")
                    if not seen["tcp_ack_sent"]:
                        ack_frame = build_tcp_ack((tcp_seq + 1) & 0xFFFFFFFF, (TCP_ISN + 1) & 0xFFFFFFFF, tcp_src_port)
                        sock.sendto(ack_frame, peer)
                        seen["tcp_ack_sent"] = True
                        write_log("NETPEER: TCP ACK sent PASS")
            return
        return

    if ethertype == ETH_IPV6 and len(payload) >= 40:
        ver = payload[0] >> 4
        if ver != 6:
            return
        plen = struct.unpack("!H", payload[4:6])[0]
        nexth = payload[6]
        src_ip6 = payload[8:24]
        dst_ip6 = payload[24:40]
        if nexth != 58 or dst_ip6 != HOST_IPV6 or src_ip6 != GUEST_IPV6:
            return
        icmp = payload[40 : 40 + plen]
        if len(icmp) >= 8 and icmp[0] == 129 and icmp[1] == 0:
            seen["icmpv6"] = True
            write_log("NETPEER: ICMPv6 echo reply PASS")
        return


if __name__ == "__main__":
    raise SystemExit(main())
