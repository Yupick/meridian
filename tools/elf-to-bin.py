#!/usr/bin/env python3
"""Convierte los segmentos PT_LOAD de un ELF64 en una imagen raw plana."""
import struct
import sys

src, dst = sys.argv[1], sys.argv[2]
data = open(src, "rb").read()
if data[:4] != b"\x7fELF" or data[4] != 2 or data[5] != 1:
    raise SystemExit("no es ELF64 little-endian")

phoff = struct.unpack_from("<Q", data, 32)[0]
phentsize = struct.unpack_from("<H", data, 54)[0]
phnum = struct.unpack_from("<H", data, 56)[0]

segments = []
for i in range(phnum):
    off = phoff + i * phentsize
    p_type = struct.unpack_from("<I", data, off)[0]
    if p_type != 1:
        continue
    p_offset = struct.unpack_from("<Q", data, off + 8)[0]
    p_paddr = struct.unpack_from("<Q", data, off + 24)[0]
    p_filesz = struct.unpack_from("<Q", data, off + 32)[0]
    p_memsz = struct.unpack_from("<Q", data, off + 40)[0]
    segments.append((p_offset, p_paddr, p_filesz, p_memsz))

if not segments:
    raise SystemExit("sin segmentos PT_LOAD")

base = min(p[1] for p in segments)
end = max(p[1] + p[3] for p in segments)
image = bytearray(end - base)
for p_offset, p_paddr, p_filesz, p_memsz in segments:
    start = p_paddr - base
    image[start:start + p_filesz] = data[p_offset:p_offset + p_filesz]

open(dst, "wb").write(image)
print(f"[elf2bin] {dst}: {len(image)} bytes desde base 0x{base:08X}")
