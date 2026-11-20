#!/usr/bin/env bash
# tests/run-desktop.sh — QA de la composición T4.1 de Meridian.
#
# Verifica que el compositor T4.1 funcione correctamente en QEMU/OVMF,
# mostrando un escritorio con 2 ventanas y capturando la pantalla.
#
# Uso:
#   tests/run-desktop.sh [--timeout SEGUNDOS] [--no-build] [--no-screenshot] [-h/--help]
#
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BUILD_DIR="${MERIDIAN_BUILD_DIR:-$ROOT/build}"
SERIAL_LOG="${BUILD_DIR}/serial.log"
SCREEN_PNG="${BUILD_DIR}/screen.png"
SCREEN_DESKTOP="${BUILD_DIR}/desktop/screen.png"

DO_BUILD=1
TAKE_SCREENSHOT=1

log()  { printf '[desktop] %s\n' "$*"; }
log_stream() { while IFS= read -r line; do log "$line"; done; }
fail() { printf '[desktop] ERROR: %s\n' "$*" >&2; exit 1; }

usage() {
    sed -n '2,25p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --timeout)
            [[ $# -ge 2 ]] || fail "--timeout requiere un valor"
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

log "arrancando QEMU/OVMF y ejecutando tests T4.1 (ver $SERIAL_LOG)"

"$ROOT/tools/run.sh" 2>&1 | log_stream

log "verificando logs en $SERIAL_LOG"

# Verificar que el serial.log exista y no esté vacío
[[ -s "$SERIAL_LOG" ]] || fail "no hay salida COM1 en $SERIAL_LOG"

# Verificar los 6 marcadores T4.1 en COM1
T41_MARKERS=(
    "T4.1: GOP activo"
    "T4.1: compositor init OK"
    "T4.1: window 0 'System Monitor' drawn PASS"
    "T4.1: window 1 'Meridian Terminal' drawn PASS"
    "T4.1: focus switch PASS"
    "T4.1: compositor tests PASS"
)

missing=0
for marker in "${T41_MARKERS[@]}"; do
    if grep -Fq -- "$marker" "$SERIAL_LOG"; then
        printf '  [OK] %s\n' "$marker"
    else
        printf '  [FALTA] %s\n' "$marker" >&2
        missing=$((missing + 1))
    fi
done

if (( missing > 0 )); then
    fail "faltan $missing marcadores T4.1 en $SERIAL_LOG"
fi

# Verificar que no haya T4.1: ... FAIL en el camino normal
if grep -Eq 'T4.1: .*FAIL' "$SERIAL_LOG"; then
    fail "hay un marcador T4.1 con FAIL en $SERIAL_LOG"
fi

# Capturar pantalla si está disponible y no seomitió
if (( TAKE_SCREENSHOT )); then
    mkdir -p "$BUILD_DIR/desktop"
    if [[ -f "$SCREEN_PNG" ]]; then
        cp "$SCREEN_PNG" "$SCREEN_DESKTOP"
        log "captura guardada en $SCREEN_DESKTOP"
    elif command -v python3 >/dev/null 2>&1 && python3 -c 'import PIL' >/dev/null 2>&1; then
        python3 - "$SCREEN_PNG" "$SCREEN_DESKTOP" <<'PY'
import sys
from PIL import Image
Image.open(sys.argv[1]).save(sys.argv[2])
PY
        log "captura convertida y guardada en $SCREEN_DESKTOP"
    else
        log "AVISO: no se pudo convertir la captura a PNG"
    fi
fi

# Verificar que build/screen.png o build/desktop/screen.png exista y no esté vacío
SCREEN_CHECK=""
if [[ -f "$SCREEN_PNG" ]] && [[ -s "$SCREEN_PNG" ]]; then
    SCREEN_CHECK="$SCREEN_PNG"
elif [[ -f "$SCREEN_DESKTOP" ]] && [[ -s "$SCREEN_DESKTOP" ]]; then
    SCREEN_CHECK="$SCREEN_DESKTOP"
else
    fail "no hay captura PNG utilizable en $SCREEN_PNG ni $SCREEN_DESKTOP"
fi
log "captura verificada: $SCREEN_CHECK"

if command -v python3 >/dev/null 2>&1 && python3 -c 'import PIL' >/dev/null 2>&1; then
    python3 - "$SCREEN_CHECK" <<'PY'
import sys
from PIL import Image
w, h = Image.open(sys.argv[1]).size
if w < 800 or h < 600:
    raise SystemExit(f"captura demasiado pequeña: {w}x{h}")
print(f"[desktop] dimensiones de captura: {w}x{h}")
PY
else
    log "AVISO: Pillow no disponible; no se verifican dimensiones de la captura"
fi

log "T4.1 desktop QA PASS: compositor con 2 ventanas verificado en QEMU/OVMF"
exit 0