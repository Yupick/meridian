#!/usr/bin/env python3
"""Genera un ROM mínimo para arrancar el kernel raw de Meridian en QEMU virt.

QEMU `virt` ejecuta el vector de reset en 0x200, pero el ROM se coloca desde
0x0. Por eso el stub se duplica en 0x0 y 0x200. El stub carga la DTB en x0 y
salta a la dirección física del kernel.
"""
import struct
import sys

DTB_ADDR = 0x4000_0000
DEFAULT_KERNEL_ADDR = 0x4010_0000


def stub(kernel_addr: int) -> bytes:
    # Los literales deben quedar alineados a 8 bytes: se colocan en +0x10 y
    # +0x18, con un NOP de relleno antes de ellos.
    ldr_x0 = 0x58000000 | (4 << 5) | 0  # ldr x0, dtb literal (+0x10)
    ldr_x1 = 0x58000000 | (5 << 5) | 1  # ldr x1, kernel literal (+0x14)
    br_x1 = 0xD61F0020
    nop = 0xD503201F
    return struct.pack("<IIIIQQ", ldr_x0, ldr_x1, br_x1, nop, DTB_ADDR, kernel_addr)


def main() -> None:
    dst = sys.argv[1]
    kernel_addr = int(sys.argv[2], 0) if len(sys.argv) > 2 else DEFAULT_KERNEL_ADDR
    rom = bytearray(0x1000)
    rom[0x000:0x020] = stub(kernel_addr)
    rom[0x200:0x220] = stub(kernel_addr)
    with open(dst, "wb") as output:
        output.write(rom)
    print(f"[bootrom] {dst}: dtb=0x{DTB_ADDR:08X} kernel=0x{kernel_addr:08X}")


if __name__ == "__main__":
    main()
