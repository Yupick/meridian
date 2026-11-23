#!/usr/bin/env bash
# tests/run-interactive.sh — QA automatizada de T4.3: terminal interactiva en QEMU/OVMF.
#
# Ejecuta tools/run-interactive.sh con la secuencia por defecto y verifica:
#   - marcadores T4.3 de arranque, login, prompt, comandos y cierre;
#   - salidas reales del shell (/home/root, qa-t43/, hola-meridian);
#   - ausencia de FAIL/PANIC;
#   - captura GOP válida en build/interactive/screen.png.
#
# El QA de la beta (83/83) se ejecuta por separado con tests/run-alpha.sh.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BUILD_DIR="$ROOT/build/interactive"
RUN_INTERACTIVE="$ROOT/tools/run-interactive.sh"

log()  { printf '[interactive-qa] %s\n' "$*"; }
fail() { printf '[interactive-qa] ERROR: %s\n' "$*" >&2; exit 1; }

usage() {
    sed -n '2,/^set -euo pipefail$/p' "${BASH_SOURCE[0]}" | sed '$d' | sed 's/^# \{0,1\}//'
}

TIMEOUT=180
NO_BUILD=0
NO_SCREENSHOT=0

while [[ $# -gt 0 ]]; do
    case "$1" in
        --timeout)
            [[ $# -ge 2 ]] || fail "--timeout requiere un valor"
            TIMEOUT="$2"
            shift 2
            ;;
        --no-build)
            NO_BUILD=1
            shift
            ;;
        --no-screenshot)
            NO_SCREENSHOT=1
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

log "ejecutando runner interactivo T4.3"
RUN_ARGS=(--timeout "$TIMEOUT" --build-dir "$BUILD_DIR")
(( NO_BUILD )) && RUN_ARGS+=(--no-build)
(( NO_SCREENSHOT )) && RUN_ARGS+=(--no-screenshot)
"$RUN_INTERACTIVE" "${RUN_ARGS[@]}"

SERIAL_LOG="$BUILD_DIR/serial.log"
[[ -s "$SERIAL_LOG" ]] || fail "no hay salida COM1 en $SERIAL_LOG"

MARKERS=(
    "T4.3: terminal interactiva activa (COM1)"
    "T4.3: login prompt listo"
    "T4.3: login correcto (user=root)"
    "T4.3: prompt listo meridian:/home/root$"
    "T4.3: cmd 'pwd' -> GUI terminal"
    "T4.3: cmd 'mkdir qa-t43' -> GUI terminal"
    "T4.3: cmd 'ls' -> GUI terminal"
    "T4.3: cmd 'echo hola-meridian > nota.txt' -> GUI terminal"
    "T4.3: cmd 'cat nota.txt' -> GUI terminal"
    "T4.3: cmd 'exit' -> GUI terminal"
    "T4.3: sesion finalizada; halt"
)

REAL_OUTPUT=(
    "/home/root"
    "qa-t43/"
    "hola-meridian"
)

missing=0
for marker in "${MARKERS[@]}"; do
    if grep -Fq -- "$marker" "$SERIAL_LOG"; then
        printf '  [OK] %s\n' "$marker"
    else
        printf '  [FALTA] %s\n' "$marker" >&2
        missing=$((missing + 1))
    fi
done

for marker in "${REAL_OUTPUT[@]}"; do
    if grep -Fq -- "$marker" "$SERIAL_LOG"; then
        printf '  [OK] salida real: %s\n' "$marker"
    else
        printf '  [FALTA] salida real: %s\n' "$marker" >&2
        missing=$((missing + 1))
    fi
done

if grep -Eq 'T4.3: .*FAIL|MERIDIAN-KERNEL: PANIC' "$SERIAL_LOG"; then
    printf '  [FAIL] marcador T4.3 FAIL o PANIC encontrado\n' >&2
    missing=$((missing + 1))
else
    printf '  [OK] sin FAIL/PANIC\n'
fi

if (( NO_SCREENSHOT == 0 )); then
    SCREEN_PNG="$BUILD_DIR/screen.png"
    [[ -s "$SCREEN_PNG" ]] || fail "captura no encontrada o vacía: $SCREEN_PNG"
    python3 - "$SCREEN_PNG" <<'PY'
import sys
from PIL import Image
w, h = Image.open(sys.argv[1]).size
if w < 800 or h < 600:
    raise SystemExit(f"captura demasiado pequeña: {w}x{h}")
print(f"[interactive-qa] captura {w}x{h} OK")
PY
fi

if (( missing > 0 )); then
    fail "faltaron $missing verificaciones T4.3"
fi

log "T4.3 interactive QA PASS: login + shell + GUI terminal verificados en QEMU/OVMF"
