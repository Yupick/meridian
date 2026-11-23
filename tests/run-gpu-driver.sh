#!/usr/bin/env bash
# tests/run-gpu-driver.sh — QA del driver de GPU userspace T4.2 de Meridian.
#
# Verifica en QEMU/OVMF que un comando inválido y un fallo inyectado del driver
# no tumban el kernel: el driver se reinicia, el framebuffer sigue usable y el
# arranque normal continúa.
#
# Uso:
#   tests/run-gpu-driver.sh [--timeout SEGUNDOS] [--no-build] [--no-screenshot] [-h/--help]
#
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BUILD_DIR="${MERIDIAN_BUILD_DIR:-$ROOT/build}"
SERIAL_LOG="${BUILD_DIR}/serial.log"
SCREEN_PNG="${BUILD_DIR}/screen.png"
SCREEN_GPU="${BUILD_DIR}/gpu/screen.png"

DO_BUILD=1
TAKE_SCREENSHOT=1
TIMEOUT=45

log()  { printf '[gpu-driver] %s\n' "$*"; }
log_stream() { while IFS= read -r line; do log "$line"; done; }
fail() { printf '[gpu-driver] ERROR: %s\n' "$*" >&2; exit 1; }

usage() {
    sed -n '2,24p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --timeout)
            [[ $# -ge 2 ]] || fail "--timeout requiere un valor"
            TIMEOUT="$2"
            shift 2
            ;;
        --no-build)
            DO_BUILD=0
            shift
            ;;
        --no-screenshot)
            TAKE_SCREENSHOT=0
            shift
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        *)
            fail "opción desconocida: $1"
            ;;
    esac
done

if (( DO_BUILD )); then
    log "construyendo imagen (ver $ROOT/tools/build.sh)"
    "$ROOT/tools/build.sh" 2>&1 | log_stream
else
    log "omitiendo build (--no-build)"
fi

log "arrancando QEMU/OVMF y ejecutando tests T4.2 (ver $SERIAL_LOG)"
"$ROOT/tools/run.sh" --timeout "$TIMEOUT" 2>&1 | log_stream

log "verificando logs en $SERIAL_LOG"
[[ -s "$SERIAL_LOG" ]] || fail "no hay salida COM1 en $SERIAL_LOG"

T42_MARKERS=(
    "T4.2: driver init OK"
    "T4.2: valid command PASS"
    "T4.2: invalid command rejected PASS"
    "T4.2: capability denied PASS"
    "T4.2: driver fault injected PASS"
    "T4.2: driver restart OK"
    "T4.2: kernel alive after driver restart PASS"
    "T4.2: GPU driver tests PASS"
)

missing=0
for marker in "${T42_MARKERS[@]}"; do
    if grep -Fq -- "$marker" "$SERIAL_LOG"; then
        printf '  [OK] %s\n' "$marker"
    else
        printf '  [FALTA] %s\n' "$marker" >&2
        missing=$((missing + 1))
    fi
done

if (( missing > 0 )); then
    fail "faltan $missing marcadores T4.2 en $SERIAL_LOG"
fi

if grep -Eq 'T4\.2: .*FAIL|MERIDIAN-KERNEL: PANIC' "$SERIAL_LOG"; then
    fail "el driver T4.2 falló o el kernel hizo panic en $SERIAL_LOG"
fi

if (( TAKE_SCREENSHOT )); then
    mkdir -p "$BUILD_DIR/gpu"
    if [[ -f "$SCREEN_PNG" ]]; then
        cp "$SCREEN_PNG" "$SCREEN_GPU"
        log "captura guardada en $SCREEN_GPU"
    else
        log "AVISO: no se encontró captura PNG en $SCREEN_PNG"
    fi
fi

SCREEN_CHECK=""
if [[ -f "$SCREEN_PNG" ]] && [[ -s "$SCREEN_PNG" ]]; then
    SCREEN_CHECK="$SCREEN_PNG"
elif [[ -f "$SCREEN_GPU" ]] && [[ -s "$SCREEN_GPU" ]]; then
    SCREEN_CHECK="$SCREEN_GPU"
else
    fail "no hay captura PNG utilizable en $SCREEN_PNG ni $SCREEN_GPU"
fi
log "captura verificada: $SCREEN_CHECK"

if command -v python3 >/dev/null 2>&1 && python3 -c 'import PIL' >/dev/null 2>&1; then
    python3 - "$SCREEN_CHECK" <<'PY'
import sys
from PIL import Image
w, h = Image.open(sys.argv[1]).size
if w < 800 or h < 600:
    raise SystemExit(f"captura demasiado pequeña: {w}x{h}")
print(f"[gpu-driver] dimensiones de captura: {w}x{h}")
PY
else
    log "AVISO: Pillow no disponible; no se verifican dimensiones de la captura"
fi

log "T4.2 GPU driver QA PASS: fallo aislado, driver reiniciado y kernel vivo en QEMU/OVMF"
exit 0
