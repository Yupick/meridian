#!/usr/bin/env bash
# tools/run.sh — Arranca la imagen de Meridian en QEMU con OVMF (A2-A8).
#
# Por defecto se ejecuta sin ventana (`-display none`), guarda la salida serie,
# el debugcon y una captura de pantalla (`screendump`) para poder verificar el
# mensaje dibujado vía GOP. El proceso termina automáticamente y devuelve 0 si
# el kernel imprime los marcadores esperados por COM1:
#   Meridian booting
#   A4: buddy alloc/free tests PASS
#   A4: W^X check PASS
#   A5: process creation OK
#   A5: process isolation test PASS
#   A6: ipc send/recv test PASS
#   A6: capability denied without rights PASS
#   A6: capability reduction/attenuation PASS
#   A6: ipc/capabilities tests PASS
#   A7: fs operations tests PASS
#   A7: read-only capability denied write PASS
#   A7: fs service tests PASS
#   A8: login screen shown
#   A8: invalid credentials rejected without user disclosure
#   A8: session has capabilities over personal directory
#   A8: login/users tests PASS
# Cualquier marcador "A4/A5/A6/A7/A8 ... FAIL" hace fallar el script.
#
# Uso:
#   tools/run.sh [--timeout SEGUNDOS] [--no-screenshot]
#
# Variables:
#   MERIDIAN_TIMEOUT        timeout total en segundos (por defecto 45)
#   MERIDIAN_QEMU_DISPLAY   -display de QEMU (por defecto "none")
#   MERIDIAN_MEMORY         memoria de la VM (por defecto "512M")

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BUILD_DIR="${MERIDIAN_BUILD_DIR:-$ROOT/build}"
IMAGE="$BUILD_DIR/meridian.img"

TIMEOUT="${MERIDIAN_TIMEOUT:-45}"
QEMU_DISPLAY="${MERIDIAN_QEMU_DISPLAY:-none}"
MEMORY="${MERIDIAN_MEMORY:-512M}"
TAKE_SCREENSHOT=1

SERIAL_LOG="$BUILD_DIR/serial.log"
DEBUG_LOG="$BUILD_DIR/debugcon.log"
QEMU_LOG="$BUILD_DIR/qemu.log"
MONITOR="$BUILD_DIR/qemu-monitor.sock"
SCREEN_PPM="$BUILD_DIR/screen.ppm"
SCREEN_PNG="$BUILD_DIR/screen.png"
OVMF_VARS="$BUILD_DIR/OVMF_VARS_4M.fd"

log()  { printf '[run] %s\n' "$*"; }
fail() { printf '[run] ERROR: %s\n' "$*" >&2; exit 1; }

while [[ $# -gt 0 ]]; do
    case "$1" in
        --timeout)
            [[ $# -ge 2 ]] || fail "--timeout requiere un valor"
            TIMEOUT="$2"
            shift 2
            ;;
        --no-screenshot)
            TAKE_SCREENSHOT=0
            shift
            ;;
        -h|--help)
            sed -n '2,25p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
            exit 0
            ;;
        *)
            fail "opción desconocida: $1"
            ;;
    esac
done

command -v qemu-system-x86_64 >/dev/null 2>&1 || fail "qemu-system-x86_64 no está instalado"
command -v socat >/dev/null 2>&1 || fail "socat no está instalado (necesario para el monitor QEMU)"
[[ -f "$IMAGE" ]] || fail "no existe $IMAGE; ejecuta tools/build.sh primero"

# Selección del firmware OVMF.
OVMF_CODE=""
OVMF_VARS_TEMPLATE=""
for candidate in /usr/share/OVMF/OVMF_CODE_4M.fd /usr/share/OVMF/OVMF_CODE.fd; do
    if [[ -f "$candidate" ]]; then
        OVMF_CODE="$candidate"
        break
    fi
done
for candidate in /usr/share/OVMF/OVMF_VARS_4M.fd /usr/share/OVMF/OVMF_VARS.fd; do
    if [[ -f "$candidate" ]]; then
        OVMF_VARS_TEMPLATE="$candidate"
        break
    fi
done
[[ -n "$OVMF_CODE" ]] || fail "no se encontró OVMF_CODE en /usr/share/OVMF/"
[[ -n "$OVMF_VARS_TEMPLATE" ]] || fail "no se encontró OVMF_VARS en /usr/share/OVMF/"

mkdir -p "$BUILD_DIR"
rm -f "$SERIAL_LOG" "$DEBUG_LOG" "$QEMU_LOG" "$MONITOR" "$SCREEN_PPM" "$SCREEN_PNG"
cp "$OVMF_VARS_TEMPLATE" "$OVMF_VARS"

ACCEL=()
if [[ -w /dev/kvm ]]; then
    ACCEL=(-accel kvm)
fi

QEMU_PID=""
cleanup() {
    if [[ -n "$QEMU_PID" ]] && kill -0 "$QEMU_PID" 2>/dev/null; then
        kill "$QEMU_PID" 2>/dev/null || true
        wait "$QEMU_PID" 2>/dev/null || true
    fi
}
trap cleanup EXIT

log "firmware: $OVMF_CODE"
log "imagen:   $IMAGE"
log "timeout:  ${TIMEOUT}s"

qemu-system-x86_64 \
    -machine q35 \
    -m "$MEMORY" \
    "${ACCEL[@]}" \
    -drive "if=pflash,format=raw,unit=0,readonly=on,file=$OVMF_CODE" \
    -drive "if=pflash,format=raw,unit=1,file=$OVMF_VARS" \
    -drive "file=$IMAGE,format=raw,if=virtio" \
    -vga std \
    -display "$QEMU_DISPLAY" \
    -net none \
    -no-reboot \
    -serial "file:$SERIAL_LOG" \
    -debugcon "file:$DEBUG_LOG" \
    -monitor "unix:$MONITOR,server,nowait" \
    -pidfile "$BUILD_DIR/qemu.pid" \
    >"$QEMU_LOG" 2>&1 &
QEMU_PID=$!

log "QEMU PID $QEMU_PID; esperando al kernel en COM1..."

deadline=$((SECONDS + TIMEOUT))
boot_ok=0
buddy_ok=0
wx_ok=0
a5_create_ok=0
a5_isolation_ok=0
a6_ipc_ok=0
a6_denied_ok=0
a6_reduce_ok=0
a6_summary_ok=0
a7_ops_ok=0
a7_ro_ok=0
a7_summary_ok=0
a8_screen_ok=0
a8_reject_ok=0
a8_caps_ok=0
a8_summary_ok=0
fail_marker=0
while (( SECONDS < deadline )); do
    if ! kill -0 "$QEMU_PID" 2>/dev/null; then
        log "QEMU terminó antes de tiempo"
        break
    fi
    if [[ -f "$SERIAL_LOG" ]]; then
        if grep -q "Meridian booting" "$SERIAL_LOG"; then boot_ok=1; fi
        if grep -q "A4: buddy alloc/free tests PASS" "$SERIAL_LOG"; then buddy_ok=1; fi
        if grep -q "A4: W^X check PASS" "$SERIAL_LOG"; then wx_ok=1; fi
        if grep -q "A5: process creation OK" "$SERIAL_LOG"; then a5_create_ok=1; fi
        if grep -q "A5: process isolation test PASS" "$SERIAL_LOG"; then a5_isolation_ok=1; fi
        if grep -q "A6: ipc send/recv test PASS" "$SERIAL_LOG"; then a6_ipc_ok=1; fi
        if grep -q "A6: capability denied without rights PASS" "$SERIAL_LOG"; then a6_denied_ok=1; fi
        if grep -q "A6: capability reduction/attenuation PASS" "$SERIAL_LOG"; then a6_reduce_ok=1; fi
        if grep -q "A6: ipc/capabilities tests PASS" "$SERIAL_LOG"; then a6_summary_ok=1; fi
        if grep -q "A7: fs operations tests PASS" "$SERIAL_LOG"; then a7_ops_ok=1; fi
        if grep -q "A7: read-only capability denied write PASS" "$SERIAL_LOG"; then a7_ro_ok=1; fi
        if grep -q "A7: fs service tests PASS" "$SERIAL_LOG"; then a7_summary_ok=1; fi
        if grep -q "A8: login screen shown" "$SERIAL_LOG"; then a8_screen_ok=1; fi
        if grep -q "A8: invalid credentials rejected without user disclosure" "$SERIAL_LOG"; then a8_reject_ok=1; fi
        if grep -q "A8: session has capabilities over personal directory" "$SERIAL_LOG"; then a8_caps_ok=1; fi
        if grep -q "A8: login/users tests PASS" "$SERIAL_LOG"; then a8_summary_ok=1; fi
        if grep -Eq "A4: .*FAIL|A5: .*FAIL|A6: .*FAIL|A7: .*FAIL|A8: .*FAIL" "$SERIAL_LOG"; then
            fail_marker=1
            break
        fi
    fi
    if (( boot_ok && buddy_ok && wx_ok && a5_create_ok && a5_isolation_ok \
          && a6_ipc_ok && a6_denied_ok && a6_reduce_ok && a6_summary_ok \
          && a7_ops_ok && a7_ro_ok && a7_summary_ok \
          && a8_screen_ok && a8_reject_ok && a8_caps_ok && a8_summary_ok )); then
        break
    fi
    sleep 0.5
done

# Deja que el kernel termine de imprimir antes de capturar.
sleep 1

if (( TAKE_SCREENSHOT )) && [[ -S "$MONITOR" ]]; then
    log "capturando pantalla GOP en $SCREEN_PPM"
    if printf 'screendump %s\n' "$SCREEN_PPM" | socat - UNIX-CONNECT:"$MONITOR" >/dev/null 2>&1; then
        if command -v python3 >/dev/null 2>&1 && python3 -c 'import PIL' >/dev/null 2>&1; then
            python3 - "$SCREEN_PPM" "$SCREEN_PNG" <<'PY'
import sys
from PIL import Image
Image.open(sys.argv[1]).save(sys.argv[2])
PY
            log "captura convertida a PNG: $SCREEN_PNG"
        fi
    else
        log "AVISO: no se pudo ejecutar screendump"
    fi
fi

if [[ -S "$MONITOR" ]]; then
    printf 'quit\n' | socat - UNIX-CONNECT:"$MONITOR" >/dev/null 2>&1 || true
fi

wait "$QEMU_PID" 2>/dev/null || true
QEMU_PID=""

echo
log "--- COM1 (kernel) ---"
if [[ -s "$SERIAL_LOG" ]]; then
    cat "$SERIAL_LOG"
else
    echo "(vacío)"
fi

echo
log "--- debugcon (bootloader) ---"
if [[ -s "$DEBUG_LOG" ]]; then
    cat "$DEBUG_LOG"
else
    echo "(vacío)"
fi

echo
if (( fail_marker )); then
    log "FALLO: el kernel reportó un marcador 'A4/A5/A6/A7/A8 ... FAIL' (ver COM1)."
    exit 1
fi
if (( boot_ok && buddy_ok && wx_ok && a5_create_ok && a5_isolation_ok \
      && a6_ipc_ok && a6_denied_ok && a6_reduce_ok && a6_summary_ok \
      && a7_ops_ok && a7_ro_ok && a7_summary_ok \
      && a8_screen_ok && a8_reject_ok && a8_caps_ok && a8_summary_ok )); then
    log "OK: kernel arrancó ('Meridian booting'), pasó A4, A5 (aislamiento de procesos), A6 (IPC/capacidades), A7 (sistema de archivos RAM) y A8 (login/usuarios) por COM1."
    if (( TAKE_SCREENSHOT )); then
        if [[ -f "$SCREEN_PNG" ]]; then
            log "Captura de pantalla GOP: $SCREEN_PNG"
        elif [[ -f "$SCREEN_PPM" ]]; then
            log "Captura de pantalla GOP: $SCREEN_PPM"
        fi
    fi
    exit 0
fi

log "FALLO: marcadores pendientes (boot=$boot_ok, buddy=$buddy_ok, wx=$wx_ok, a5_create=$a5_create_ok, a5_isolation=$a5_isolation_ok, a6_ipc=$a6_ipc_ok, a6_denied=$a6_denied_ok, a6_reduce=$a6_reduce_ok, a6_summary=$a6_summary_ok, a7_ops=$a7_ops_ok, a7_ro=$a7_ro_ok, a7_summary=$a7_summary_ok, a8_screen=$a8_screen_ok, a8_reject=$a8_reject_ok, a8_caps=$a8_caps_ok, a8_summary=$a8_summary_ok)."
if [[ -s "$QEMU_LOG" ]]; then
    log "--- log de QEMU ---"
    cat "$QEMU_LOG"
fi
exit 1
