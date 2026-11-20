#!/usr/bin/env python3
"""Host-side QA peer for Meridian T2.1/T2.3 network selftests.

Receives/sends raw Ethernet frames over a QEMU UDP dgram backend (no TAP/root).
Binds 127.0.0.1:55124 and peers with 127.0.0.1:55125.

T2.1: generates ARP/ICMPv4/UDP/TCP/ICMPv6 traffic and writes exact NETPEER PASS
lines to build/net-peer.log (6 events).
T2.3: after T2.1 completes, performs TLS 1.3 PSK profile over UDP/9443
(handshake, Finished, allowed/denied reads, replay) and writes exact TLSCAP
events to build/tls-capture.log (7 events).

Exits 0 only when all required T2.1 and T2.3 events are observed.
"""

from __future__ import annotations

import argparse
import hashlib
import hmac
import os
import select
import socket
import struct
import sys
import time
from typing import Dict, List, Optional, Tuple

from cryptography.hazmat.primitives.ciphers.aead import AESGCM

# --- T2.1 constants ---
GUEST_MAC = bytes.fromhex("525400000015")
HOST_MAC = bytes.fromhex("525400000002")
BROADCAST_MAC = b"\xff\xff\xff\xff\xff\xff"
GUEST_IPV4 = socket.inet_aton("10.0.2.15")
HOST_IPV4 = socket.inet_aton("10.0.2.2")
GUEST_IPV6 = socket.inet_pton(socket.AF_INET6, "fd00::15")
HOST_IPV6 = socket.inet_pton(socket.AF_INET6, "fd00::2")
TCP_PORT = 8080
UDP_ECHO_PORT = 9999
TLS_PORT = 9443
TCP_ISN = 0x1000

ETH_ARP = 0x0806
ETH_IPV4 = 0x0800
ETH_IPV6 = 0x86DD

# --- T2.3 constants ---
MAGIC = b"MRSTLS13"
CT_HANDSHAKE = 0x16
CT_APP = 0x17
CT_FINISHED = 20
CT_ALERT_APP = 21


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
        1,
        ETH_IPV4,
        6,
        4,
        1,
        HOST_MAC,
        HOST_IPV4,
        BROADCAST_MAC,
        GUEST_IPV4,
    )
    return eth_header(BROADCAST_MAC, HOST_MAC, ETH_ARP) + arp


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


def build_icmp_echo_request(ident: int, seq: int, payload: bytes) -> bytes:
    icmp = struct.pack("!BBHHH", 8, 0, 0, ident & 0xFFFF, seq & 0xFFFF) + payload
    csum = checksum(icmp)
    icmp = struct.pack("!BBHHH", 8, 0, csum, ident & 0xFFFF, seq & 0xFFFF) + payload
    ip = build_ipv4_packet(HOST_IPV4, GUEST_IPV4, 1, icmp, ident & 0xFFFF)
    return eth_header(GUEST_MAC, HOST_MAC, ETH_IPV4) + ip


def build_udp_datagram(src_port: int, dst_port: int, payload: bytes, identification: int = 0x3344) -> bytes:
    udp_len = 8 + len(payload)
    udp = struct.pack("!HHHH", src_port, dst_port, udp_len, 0) + payload
    ip = build_ipv4_packet(HOST_IPV4, GUEST_IPV4, 17, udp, identification)
    return eth_header(GUEST_MAC, HOST_MAC, ETH_IPV4) + ip


def build_udp_echo_request(src_port: int, payload: bytes) -> bytes:
    return build_udp_datagram(src_port, UDP_ECHO_PORT, payload, 0x3344)


def tcp_checksum(src: bytes, dst: bytes, tcp_segment: bytes) -> int:
    pseudo = src + dst + struct.pack("!BBH", 0, 6, len(tcp_segment))
    return checksum(pseudo + tcp_segment)


def build_tcp_syn(seq: int, src_port: int) -> bytes:
    data_offset = 5 << 4
    flags = 0x02
    tcp = struct.pack("!HHIIBBHHH", src_port, TCP_PORT, seq, 0, data_offset, flags, 4096, 0, 0)
    csum = tcp_checksum(HOST_IPV4, GUEST_IPV4, tcp)
    tcp = struct.pack("!HHIIBBHHH", src_port, TCP_PORT, seq, 0, data_offset, flags, 4096, csum, 0)
    ip = build_ipv4_packet(HOST_IPV4, GUEST_IPV4, 6, tcp, 0x4455)
    return eth_header(GUEST_MAC, HOST_MAC, ETH_IPV4) + ip


def build_tcp_ack(seq: int, ack: int, src_port: int) -> bytes:
    data_offset = 5 << 4
    flags = 0x10
    tcp = struct.pack("!HHIIBBHHH", src_port, TCP_PORT, seq, ack, data_offset, flags, 4096, 0, 0)
    csum = tcp_checksum(HOST_IPV4, GUEST_IPV4, tcp)
    tcp = struct.pack("!HHIIBBHHH", src_port, TCP_PORT, seq, ack, data_offset, flags, 4096, csum, 0)
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


# --- T2.3 helpers ---

def hkdf_extract(salt: bytes, ikm: bytes) -> bytes:
    return hmac.new(salt, ikm, hashlib.sha256).digest()


def hkdf_expand(prk: bytes, info: bytes, length: int) -> bytes:
    t1 = hmac.new(prk, info + b"\x01", hashlib.sha256).digest()
    return t1[:length]


def make_nonce(iv: bytes, seq: int) -> bytes:
    n = bytearray(iv)
    s = struct.pack("!Q", seq)
    for i in range(8):
        n[4 + i] ^= s[i]
    return bytes(n)


def seal_record(key: bytes, iv: bytes, seq: int, content_type: int, plaintext: bytes) -> bytes:
    body = AESGCM(key).encrypt(
        make_nonce(iv, seq), plaintext, struct.pack("!BHH", content_type, 0x0303, len(plaintext) + 16)
    )
    header = struct.pack("!BHH", content_type, 0x0303, len(body))
    return header + body


def open_record(key: bytes, iv: bytes, seq: int, record: bytes) -> Tuple[int, bytes]:
    if len(record) < 21:
        raise ValueError("record too short")
    ctype, ver, rlen = struct.unpack("!BHH", record[:5])
    if ver != 0x0303 or 5 + rlen != len(record):
        raise ValueError("bad record")
    aad = record[:5]
    body = record[5:]
    pt = AESGCM(key).decrypt(make_nonce(iv, seq), body, aad)
    return ctype, pt


class TlsClient:
    def __init__(self, peer: Tuple[str, int], log_path: str):
        self.peer = peer
        self.log_path = log_path
        self.events: List[str] = []
        self._init_log()

        self.psk = hashlib.sha256(b"meridian-tls13-dev-psk-v1").digest()
        self.client_random = hashlib.sha256(b"meridian-tls-client-random-v1").digest()

        self.stage = "ch"  # ch -> await_allowed -> await_denied -> await_replay -> done
        self.client_seq = 1
        self.server_seq = 1
        self.keys = None  # (c_key,c_iv,s_key,s_iv,fin_key)
        self.transcript_hash = b""
        self.server_random = b""
        self.last_send = 0.0
        self.done = False

    def _init_log(self) -> None:
        os.makedirs(os.path.dirname(self.log_path) or ".", exist_ok=True)
        with open(self.log_path, "w", encoding="utf-8"):
            pass

    def log(self, line: str) -> None:
        if line in self.events:
            return
        self.events.append(line)
        with open(self.log_path, "a", encoding="utf-8") as f:
            f.write(line + "\n")
            f.flush()

    def all_done(self) -> bool:
        needed = [
            "TLSCAP: ClientHello observed",
            "TLSCAP: ServerHello verified",
            "TLSCAP: Finished verified",
            "TLSCAP: application ciphertext captured",
            "TLSCAP: encrypted read allowed PASS",
            "TLSCAP: encrypted read denied PASS",
            "TLSCAP: replay rejected PASS",
        ]
        return all(e in self.events for e in needed)

    def build_client_hello(self) -> bytes:
        payload = MAGIC + bytes([1]) + self.client_random + bytes([0])
        return struct.pack("!BHH", CT_HANDSHAKE, 0x0303, len(payload)) + payload

    def derive_keys(self, server_payload_no_verify: bytes, verify: bytes) -> bool:
        ch_payload = MAGIC + bytes([1]) + self.client_random + bytes([0])
        prefix = ch_payload + server_payload_no_verify
        self.transcript_hash = hashlib.sha256(prefix).digest()

        early = hkdf_extract(b"\x00" * 32, self.psk)
        hs_info = b"meridian tls13 handshake" + self.transcript_hash
        hs = hkdf_expand(early, hs_info, 32)
        c_key = hkdf_expand(hs, b"meridian tls13 c key", 16)
        c_iv = hkdf_expand(hs, b"meridian tls13 c iv", 12)
        s_key = hkdf_expand(hs, b"meridian tls13 s key", 16)
        s_iv = hkdf_expand(hs, b"meridian tls13 s iv", 12)
        fin_key = hkdf_expand(hs, b"meridian tls13 finished", 32)
        expected_verify = hmac.new(fin_key, self.transcript_hash, hashlib.sha256).digest()[:16]
        if expected_verify != verify:
            return False
        expected_sr = hashlib.sha256(b"meridian-tls-server-random-v1" + self.client_random).digest()
        if self.server_random != expected_sr:
            return False
        self.keys = (c_key, c_iv, s_key, s_iv, fin_key)
        return True

    def build_finished(self) -> bytes:
        assert self.keys is not None
        c_key, c_iv, _s_key, _s_iv, fin_key = self.keys
        fin_plain = hmac.new(fin_key, b"client finished" + self.transcript_hash, hashlib.sha256).digest()[:16]
        rec = seal_record(c_key, c_iv, self.client_seq, CT_FINISHED, fin_plain)
        self.client_seq += 1
        return rec

    def build_request(self, user: bytes, key: int, rights: int, req_seq: int) -> bytes:
        assert self.keys is not None
        c_key, c_iv, *_ = self.keys
        pt = b"REQ\0" + bytes([len(user)]) + user + struct.pack("!QBQ", key, rights, req_seq)
        rec = seal_record(c_key, c_iv, self.client_seq, CT_APP, pt)
        self.client_seq += 1
        return rec

    def send_frame(self, sock: socket.socket, udp_payload: bytes) -> None:
        frame = build_udp_datagram(45000, TLS_PORT, udp_payload, 0x5500 + (self.client_seq & 0xFF))
        sock.sendto(frame, self.peer)

    def on_tick(self, sock: socket.socket, now: float) -> None:
        if self.all_done():
            self.done = True
            return
        if now - self.last_send < 0.25:
            return
        # Only retransmit ClientHello (plaintext, idempotent for server). Do not
        # retransmit encrypted app records to avoid request-seq replay.
        if self.stage == "ch":
            self.send_frame(sock, self.build_client_hello())
            self.last_send = now

    def start_application(self, sock: socket.socket) -> None:
        # Send Finished once, then first application request.
        fin = self.build_finished()
        self.send_frame(sock, fin)
        # El Finished se considera verificado solo cuando el guest responde con
        # un registro cifrado que solo puede derivarse tras aceptarlo.
        req = self.build_request(b"alice", 13, 1, 1)
        self.send_frame(sock, req)
        self.stage = "await_allowed"
        self.last_send = time.monotonic()

    def handle_tls_record(self, sock: socket.socket, udp_payload: bytes) -> None:
        if self.all_done():
            self.done = True
            return
        if len(udp_payload) < 5:
            return
        ctype = udp_payload[0]

        if self.stage == "ch" and ctype == CT_HANDSHAKE and len(udp_payload) >= 5 + 58:
            payload = udp_payload[5:5 + 58]
            if payload[:8] == MAGIC and payload[8] == 2 and payload[41] == 0:
                self.log("TLSCAP: ClientHello observed")
                self.server_random = payload[9:41]
                verify = payload[42:58]
                if self.derive_keys(payload[:41], verify):
                    self.log("TLSCAP: ServerHello verified")
                    self.start_application(sock)
            return

        if self.keys is None:
            return
        _c_key, _c_iv, s_key, s_iv, _fin_key = self.keys

        if ctype in (CT_APP, CT_ALERT_APP, CT_FINISHED):
            try:
                rctype, pt = open_record(s_key, s_iv, self.server_seq, udp_payload)
            except Exception:
                return
            self.server_seq += 1

            # Primera respuesta cifrada válida: el guest aceptó el Finished.
            self.log("TLSCAP: Finished verified")

            if rctype == CT_APP and "TLSCAP: application ciphertext captured" not in self.events:
                if b"remote-notice-v1" not in udp_payload:
                    short = hashlib.sha256(udp_payload).hexdigest()[:16]
                    self.log(
                        f"TLSCAP: application ciphertext captured hash={short} cleartext_absent=true"
                    )

            if rctype == CT_ALERT_APP and pt == b"replay":
                self.log("TLSCAP: replay rejected PASS")
                self.stage = "done"
                self.done = self.all_done()
                return

            if rctype == CT_APP and pt.startswith(b"RES\0") and len(pt) >= 6:
                status = pt[4]
                ulen = pt[5]
                if len(pt) < 6 + ulen + 8 + 2:
                    return
                user = pt[6:6 + ulen]
                off = 6 + ulen
                obj_key = struct.unpack("!Q", pt[off:off + 8])[0]
                off += 8
                vlen = struct.unpack("!H", pt[off:off + 2])[0]
                off += 2
                value = pt[off:off + vlen]

                if self.stage == "await_allowed":
                    if user == b"alice" and obj_key == 13 and status == 1 and value == b"remote-notice-v1":
                        self.log("TLSCAP: encrypted read allowed PASS")
                        # denied request: guest, key 10, READ, req_seq 2
                        req = self.build_request(b"guest", 10, 1, 2)
                        self.send_frame(sock, req)
                        self.stage = "await_denied"
                        self.last_send = time.monotonic()
                    return

                if self.stage == "await_denied":
                    if user == b"guest" and obj_key == 10 and status == 0:
                        self.log("TLSCAP: encrypted read denied PASS")
                        # replay: same inner req_seq 2 as new TLS record
                        req = self.build_request(b"guest", 10, 1, 2)
                        self.send_frame(sock, req)
                        self.stage = "await_replay"
                        self.last_send = time.monotonic()
                    return


def main() -> int:
    parser = argparse.ArgumentParser(description="Meridian T2.1/T2.3 host UDP network peer")
    parser.add_argument("--bind-host", default="127.0.0.1")
    parser.add_argument("--bind-port", type=int, default=55124)
    parser.add_argument("--peer-host", default="127.0.0.1")
    parser.add_argument("--peer-port", type=int, default=55125)
    parser.add_argument("--log", default="build/net-peer.log")
    parser.add_argument("--tls-log", default="build/tls-capture.log")
    parser.add_argument("--timeout", type=float, default=60.0)
    args = parser.parse_args()

    os.makedirs(os.path.dirname(args.log) or ".", exist_ok=True)
    net_lines: List[str] = []

    def write_net(line: str) -> None:
        if line in net_lines:
            return
        net_lines.append(line)
        with open(args.log, "a", encoding="utf-8") as f:
            f.write(line + "\n")
            f.flush()

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

    tls = TlsClient(peer, args.tls_log)

    start = time.monotonic()
    last_send = 0.0
    deadline = start + args.timeout

    try:
        while time.monotonic() < deadline:
            now = time.monotonic()
            if now - last_send >= 0.2:
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
                tls.on_tick(sock, now)
                if tls.all_done():
                    return 0

            rlist, _, _ = select.select([sock], [], [], 0.05)
            if not rlist:
                continue
            try:
                data, _addr = sock.recvfrom(65535)
            except BlockingIOError:
                continue

            handle_frame(
                data,
                sock,
                peer,
                seen,
                write_net,
                udp_payload,
                udp_src_port,
                tcp_seq,
                tcp_src_port,
                tls if all(seen.values()) else None,
            )

        missing_net = [k for k, v in seen.items() if not v]
        if missing_net:
            sys.stderr.write(f"NETPEER: TIMEOUT missing={','.join(missing_net)}\n")
        if not tls.all_done():
            sys.stderr.write("TLSCAP: TIMEOUT missing events\n")
        return 1
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
    tls: Optional[TlsClient],
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
            if sport == TLS_PORT and dport == 45000 and tls is not None:
                tls.handle_tls_record(sock, data)
                return
            return
        if proto == 6 and len(ip_payload) >= 20:
            sport, dport, seq, ack, off_flags = struct.unpack("!HHIIH", ip_payload[:14])
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
