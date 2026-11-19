#!/usr/bin/env bash
# tests/run-signature.sh — QA de T1.1: el bootloader debe rechazar un kernel
# cuya firma Ed25519 no sea válida.
#
# Construye una imagen aislada, corrompe \kernel.sig dentro de la ESP y exige
# que el bootloader aborte antes de transferir el control al kernel.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BUILD_DIR="${MERIDIAN_SIGN_TEST_BUILD_DIR:-$ROOT/build-signature-test}"
IMAGE="$BUILD_DIR/meridian.img"
ESP_OFFSET=$((2048 * 512)) # tools/build.sh: sector 2048, 512 bytes/sector

log()  { printf '[signature-qa] %s\n' "$*"; }
fail() { printf '[signature-qa] ERROR: %s\n' "$*" >&2; exit 1; }

command -v mcopy >/dev/null 2>&1 || fail "falta mcopy en PATH"
command -v python3 >/dev/null 2>&1 || fail "falta python3 en PATH"

log "construyendo imagen de prueba en $BUILD_DIR"
rm -rf "$BUILD_DIR"
MERIDIAN_BUILD_DIR="$BUILD_DIR" "$ROOT/tools/build.sh"

[[ -f "$IMAGE" ]] || fail "no se generó $IMAGE"

log "corrompiendo \\kernel.sig dentro de la ESP"
mcopy -i "${IMAGE}@@${ESP_OFFSET}" "::/kernel.sig" "$BUILD_DIR/kernel.sig.original"
python3 - "$BUILD_DIR/kernel.sig.original" "$BUILD_DIR/kernel.sig.tampered" <<'PY'
import sys

source, target = sys.argv[1:3]
with open(source, "rb") as handle:
    signature = bytearray(handle.read())
if len(signature) != 64:
    raise SystemExit(f"firma de {len(signature)} bytes; se esperaban 64")
signature[0] ^= 0x01
with open(target, "wb") as handle:
    handle.write(signature)
PY
# -D o permite sobrescribir el kernel.sig ya presente en la ESP.
mcopy -D o -i "${IMAGE}@@${ESP_OFFSET}" "$BUILD_DIR/kernel.sig.tampered" "::/kernel.sig"

log "arrancando QEMU esperando rechazo de firma"
MERIDIAN_BUILD_DIR="$BUILD_DIR" MERIDIAN_TIMEOUT=35 \
    "$ROOT/tools/run.sh" --expect-signature-reject --no-screenshot

log "[OK] T1.1 firma inválida rechazada"
