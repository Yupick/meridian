#!/usr/bin/env bash
# tools/check-env.sh — Verifica el entorno de desarrollo para Meridian (Fase A1).
# Comprueba: QEMU (x86_64), OVMF, Rust/rustup y target x86_64-unknown-uefi.
# Sale con 0 si todo está correcto, distinto de 0 en caso contrario.

set -euo pipefail

ok()   { printf '[OK]   %s\n' "$*"; }
fail() { printf '[FAIL] %s\n' "$*" >&2; errors=$((errors + 1)); }
info() { printf '[INFO] %s\n' "$*"; }

errors=0

echo "==> Meridian A1: verificación de entorno de desarrollo"
echo

# 1) QEMU x86_64
if command -v qemu-system-x86_64 >/dev/null 2>&1; then
    ver="$(qemu-system-x86_64 --version | head -n 1)"
    ok "qemu-system-x86_64 disponible: ${ver}"
else
    fail "qemu-system-x86_64 no encontrado en PATH (instala: sudo apt install -y qemu-system-x86)"
fi

# 2) OVMF (firmware UEFI)
ovmf_found=0
# Búsqueda principal en /usr/share/OVMF/ según criterio A1
if [ -d /usr/share/OVMF ]; then
    if find /usr/share/OVMF -maxdepth 1 -type f -name 'OVMF_CODE*' | grep -q .; then
        ovmf_found=1
        ok "OVMF_CODE encontrado en /usr/share/OVMF/: $(find /usr/share/OVMF -maxdepth 1 -type f -name 'OVMF_CODE*' | sort | head -n 1)"
    fi
fi
# Rutas alternativas comunes (informativas) por si el paquete usa otra ubicación
if [ "$ovmf_found" -eq 0 ]; then
    alt="$(find /usr/share -maxdepth 3 -type f -name 'OVMF_CODE*' 2>/dev/null | head -n 1 || true)"
    if [ -n "$alt" ]; then
        fail "OVMF_CODE no está en /usr/share/OVMF/ (criterio A1). Encontrado en: ${alt}"
    else
        fail "OVMF_CODE no encontrado (instala: sudo apt install -y ovmf). Se espera en /usr/share/OVMF/"
    fi
fi

# 3) Rust (rustc y rustup)
if command -v rustc >/dev/null 2>&1; then
    ok "rustc disponible: $(rustc --version)"
else
    fail "rustc no encontrado (instala rustup: https://rustup.rs)"
fi

if command -v rustup >/dev/null 2>&1; then
    ok "rustup disponible: $(rustup --version 2>/dev/null | head -n 1)"
    # 4) Target x86_64-unknown-uefi
    if rustup target list --installed | grep -qx 'x86_64-unknown-uefi'; then
        ok "Target Rust instalado: x86_64-unknown-uefi"
    else
        fail "Target x86_64-unknown-uefi no instalado (ejecuta: rustup target add x86_64-unknown-uefi)"
    fi
else
    fail "rustup no encontrado (requerido para gestionar targets; instala desde https://rustup.rs)"
    fail "No se puede verificar target x86_64-unknown-uefi sin rustup"
fi

echo
if [ "$errors" -eq 0 ]; then
    echo "Resultado: ENTORNO OK (todos los criterios de A1 satisfechos)"
    exit 0
else
    echo "Resultado: ENTORNO INCOMPLETO (${errors} comprobación(es) fallida(s))"
    echo "Sugerencia (Ubuntu 24.04): sudo apt update && sudo apt install -y qemu-system-x86 ovmf build-essential git mtools xorriso nasm"
    echo "Rust: curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh && rustup target add x86_64-unknown-uefi"
    exit 1
fi
