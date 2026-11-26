#!/usr/bin/env bash
# tests/run-aarch64.sh — QA de la tarea T5.1: arranque aarch64 en QEMU.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BUILD_DIR="${MERIDIAN_AARCH64_BUILD_DIR:-$ROOT/build/aarch64}"

log()  { printf '[qa-aarch64] %s\n' "$*"; }
fail() { printf '[qa-aarch64] FAIL: %s\n' "$*" >&2; exit 1; }

for artifact in meridian-aarch64.img kernel-aarch64.elf kernel-aarch64.bin bootrom.bin; do
    [[ -f "$BUILD_DIR/$artifact" ]] || fail "falta $artifact; ejecuta tools/build-aarch64.sh"
done

log "arquitectura del ELF del kernel"
readelf -h "$BUILD_DIR/kernel-aarch64.elf" | grep -qi 'AArch64' || fail "el kernel no es AArch64"

log "firma del kernel"
[[ -s "$BUILD_DIR/kernel.sig" ]] || fail "kernel.sig vacío"

log "arrancando kernel aarch64 en modo direct (DTB + raw)"
"$ROOT/tools/run-aarch64.sh" --mode direct --no-screenshot

if [[ "${MERIDIAN_AARCH64_UEFI_QA:-0}" == "1" ]]; then
    log "arrancando imagen aarch64 en modo UEFI (experimental)"
    "$ROOT/tools/run-aarch64.sh" --mode uefi --no-screenshot
fi

log "PASS: T5.1 aarch64 HAL verificado"
