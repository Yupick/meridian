#!/usr/bin/env bash
# tools/run-interactive.sh — Arranca la terminal interactiva T4.3 en QEMU/OVMF.
#
# Construye (salvo --no-build) un kernel con la feature `interactive`, arranca
# QEMU con la serie expuesta en un socket UNIX y permite enviar comandos por
# una secuencia scriptada. Toda la salida COM1 se guarda en
# <build-dir>/serial.log y la captura GOP en <build-dir>/screen.png.
#
# Opciones:
#   --commands ARCHIVO    archivo con usuario, password y comandos, uno por línea
#   --timeout SEGUNDOS    timeout por espera (default 180)
#   --build-dir RUTA      directorio de build (default <repo>/build/interactive)
#   --no-build            no reconstruir la imagen
#   --no-screenshot       no capturar la pantalla GOP
#   -h/--help             muestra esta ayuda y sale
#
# Secuencia por defecto si no se da --commands:
#   root
#   meridian
#   pwd
#   mkdir qa-t43
#   ls
#   echo hola-meridian > nota.txt
#   cat nota.txt
#   exit

set -euo pipefail

log()  { printf '[interactive] %s\n' "$*"; }
fail() { printf '[interactive] ERROR: %s\n' "$*" >&2; exit 1; }

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BUILD_DIR_DEFAULT="$ROOT/build/interactive"
COMMAND_FILE=""
TIMEOUT=180
BUILD_DIR=""
NO_BUILD=0
NO_SCREENSHOT=0

while [[ $# -gt 0 ]]; do
    case "$1" in
        --commands)
            [[ $# -ge 2 ]] || fail "--commands requiere un valor"
            COMMAND_FILE="$2"
            shift 2
            ;;
        --timeout)
            [[ $# -ge 2 ]] || fail "--timeout requiere un valor"
            TIMEOUT="$2"
            shift 2
            ;;
        --build-dir)
            [[ $# -ge 2 ]] || fail "--build-dir requiere un valor"
            BUILD_DIR="$2"
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
            sed -n '2,/^set -euo pipefail$/p' "${BASH_SOURCE[0]}" | sed '$d' | sed 's/^# \{0,1\}//'
            exit 0
            ;;
        *)
            fail "opción desconocida: $1"
            ;;
    esac
done

BUILD_DIR="${BUILD_DIR:-$BUILD_DIR_DEFAULT}"
mkdir -p "$BUILD_DIR"

SERIAL_SOCK="$BUILD_DIR/serial.sock"
SERIAL_LOG="$BUILD_DIR/serial.log"
DEBUG_LOG="$BUILD_DIR/debugcon.log"
QEMU_LOG="$BUILD_DIR/qemu.log"
QEMU_MONITOR="$BUILD_DIR/qemu-monitor.sock"
FIFO="$BUILD_DIR/serial-input.fifo"
SCREEN_PPM="$BUILD_DIR/screen.ppm"
SCREEN_PNG="$BUILD_DIR/screen.png"
NETPEER_LOG="$BUILD_DIR/net-peer.log"
TLSCAP_LOG="$BUILD_DIR/tls-capture.log"
OVMF_VARS="$BUILD_DIR/OVMF_VARS_4M.fd"

QEMU_PID=""
NETPEER_PID=""
BRIDGE_PID=""
FIFO_OPEN=0
CLEANED=0

cleanup() {
    (( CLEANED )) && return
    CLEANED=1
    set +e
    if (( FIFO_OPEN )); then exec 3>&-; fi
    [[ -n "$BRIDGE_PID" ]] && kill "$BRIDGE_PID" 2>/dev/null
    [[ -n "$BRIDGE_PID" ]] && wait "$BRIDGE_PID" 2>/dev/null
    if [[ -n "$QEMU_PID" ]] && kill -0 "$QEMU_PID" 2>/dev/null; then
        kill "$QEMU_PID" 2>/dev/null
        wait "$QEMU_PID" 2>/dev/null
    fi
    if [[ -S "$QEMU_MONITOR" ]]; then
        printf 'quit\n' | socat - UNIX-CONNECT:"$QEMU_MONITOR" >/dev/null 2>&1
    fi
    if [[ -n "$NETPEER_PID" ]] && kill -0 "$NETPEER_PID" 2>/dev/null; then
        kill "$NETPEER_PID" 2>/dev/null
        wait "$NETPEER_PID" 2>/dev/null
    fi
    rm -f "$SERIAL_SOCK" "$QEMU_MONITOR" "$FIFO"
}
trap cleanup EXIT

wait_for_marker() {
    local marker="$1"
    local seconds="$2"
    local deadline=$((SECONDS + seconds))
    while (( SECONDS < deadline )); do
        if grep -Fq -- "$marker" "$SERIAL_LOG" 2>/dev/null; then
            return 0
        fi
        if grep -Fq -- 'MERIDIAN-KERNEL: PANIC' "$SERIAL_LOG" 2>/dev/null; then
            fail "PANIC del kernel esperando: $marker"
        fi
        if [[ -n "$QEMU_PID" ]] && ! kill -0 "$QEMU_PID" 2>/dev/null; then
            fail "QEMU terminó antes de: $marker"
        fi
        sleep 0.25
    done
    fail "timeout esperando: $marker"
}

send_line() {
    printf '%s\n' "$1" >&3
    sleep 0.35
}

command -v qemu-system-x86_64 >/dev/null 2>&1 || fail "qemu-system-x86_64 no está instalado"
command -v socat >/dev/null 2>&1 || fail "socat no está instalado"
command -v python3 >/dev/null 2>&1 || fail "python3 no está instalado"

OVMF_CODE=""
for candidate in /usr/share/OVMF/OVMF_CODE_4M.fd /usr/share/OVMF/OVMF_CODE.fd; do
    [[ -f "$candidate" ]] && OVMF_CODE="$candidate" && break
done
[[ -n "$OVMF_CODE" ]] || fail "no se encontró OVMF_CODE en /usr/share/OVMF/"

OVMF_VARS_TEMPLATE=""
for candidate in /usr/share/OVMF/OVMF_VARS_4M.fd /usr/share/OVMF/OVMF_VARS.fd; do
    [[ -f "$candidate" ]] && OVMF_VARS_TEMPLATE="$candidate" && break
done
[[ -n "$OVMF_VARS_TEMPLATE" ]] || fail "no se encontró OVMF_VARS en /usr/share/OVMF/"

rm -f "$SERIAL_LOG" "$DEBUG_LOG" "$QEMU_LOG" "$SERIAL_SOCK" "$QEMU_MONITOR" "$FIFO" "$SCREEN_PPM" "$SCREEN_PNG" "$NETPEER_LOG" "$TLSCAP_LOG"
cp "$OVMF_VARS_TEMPLATE" "$OVMF_VARS"

if (( NO_BUILD )); then
    log "omitiendo build (--no-build)"
else
    log "construyendo kernel interactivo en $BUILD_DIR"
    MERIDIAN_BUILD_DIR="$BUILD_DIR" "$ROOT/tools/build.sh" --interactive
fi

[[ -f "$BUILD_DIR/meridian.img" ]] || fail "no existe $BUILD_DIR/meridian.img"
[[ -f "$BUILD_DIR/blocktest.img" ]] || fail "no existe $BUILD_DIR/blocktest.img"

LOGIN_USER="root"
LOGIN_PASS="meridian"
if [[ -n "$COMMAND_FILE" ]]; then
    [[ -f "$COMMAND_FILE" ]] || fail "no existe $COMMAND_FILE"
    mapfile -t COMMAND_LINES < "$COMMAND_FILE"
    (( ${#COMMAND_LINES[@]} >= 2 )) || fail "$COMMAND_FILE necesita al menos usuario y password"
    LOGIN_USER="${COMMAND_LINES[0]}"
    LOGIN_PASS="${COMMAND_LINES[1]}"
    SHELL_COMMANDS=("${COMMAND_LINES[@]:2}")
else
    SHELL_COMMANDS=(
        "pwd"
        "mkdir qa-t43"
        "ls"
        "echo hola-meridian > nota.txt"
        "cat nota.txt"
        "exit"
    )
fi

log "arrancando net-peer T2.1/T2.3"
python3 "$ROOT/tools/net-peer.py" \
    --bind-port 55124 --peer-port 55125 \
    --log "$NETPEER_LOG" \
    --tls-log "$TLSCAP_LOG" \
    --timeout 90 \
    >"$BUILD_DIR/net-peer.stdout" 2>"$BUILD_DIR/net-peer.stderr" &
NETPEER_PID=$!
sleep 1

ACCEL=()
if [[ -w /dev/kvm ]]; then
    ACCEL=(-accel kvm)
fi

log "arrancando QEMU/OVMF con serie UNIX $SERIAL_SOCK"
qemu-system-x86_64 \
    -machine q35 \
    -m 512M \
    "${ACCEL[@]}" \
    -drive "if=pflash,format=raw,unit=0,readonly=on,file=$OVMF_CODE" \
    -drive "if=pflash,format=raw,unit=1,file=$OVMF_VARS" \
    -drive "file=$BUILD_DIR/meridian.img,format=raw,if=virtio" \
    -drive "file=$BUILD_DIR/blocktest.img,format=raw,if=none,id=blockqa" \
    -device "virtio-blk-pci,drive=blockqa,addr=03.0" \
    -netdev dgram,id=netqa,local.type=inet,local.host=127.0.0.1,local.port=55125,remote.type=inet,remote.host=127.0.0.1,remote.port=55124 \
    -device "virtio-net-pci,disable-modern=on,netdev=netqa,mac=52:54:00:00:00:15,addr=04.0" \
    -vga std \
    -display none \
    -no-reboot \
    -serial "unix:$SERIAL_SOCK,server=on,wait=off" \
    -debugcon "file:$DEBUG_LOG" \
    -monitor "unix:$QEMU_MONITOR,server,nowait" \
    -pidfile "$BUILD_DIR/qemu.pid" \
    >"$QEMU_LOG" 2>&1 &
QEMU_PID=$!

log "esperando socket serie de QEMU"
SOCKET_DEADLINE=$((SECONDS + 20))
while (( SECONDS < SOCKET_DEADLINE )); do
    [[ -S "$SERIAL_SOCK" ]] && break
    sleep 0.25
done
[[ -S "$SERIAL_SOCK" ]] || fail "QEMU no creó $SERIAL_SOCK"

log "creando puente serie y FIFO de comandos"
mkfifo "$FIFO"
exec 3<>"$FIFO"
FIFO_OPEN=1
python3 "$ROOT/tools/serial_bridge.py" "$BUILD_DIR" "$SERIAL_SOCK" "$FIFO" \
    >"$BUILD_DIR/serial-bridge.log" 2>&1 &
BRIDGE_PID=$!
sleep 0.3

wait_for_marker "T4.3: terminal interactiva activa (COM1)" "$TIMEOUT"
log "enviando credenciales dev-only"
send_line "$LOGIN_USER"
send_line "$LOGIN_PASS"

wait_for_marker "T4.3: login correcto (user=root)" "$TIMEOUT"
log "enviando comandos de shell (${#SHELL_COMMANDS[@]})"
for cmd in "${SHELL_COMMANDS[@]}"; do
    send_line "$cmd"
done

wait_for_marker "T4.3: sesion finalizada; halt" "$TIMEOUT"

if (( NO_SCREENSHOT == 0 )) && [[ -S "$QEMU_MONITOR" ]]; then
    log "capturando pantalla GOP"
    if printf 'screendump %s\n' "$SCREEN_PPM" | socat - UNIX-CONNECT:"$QEMU_MONITOR" >/dev/null 2>&1; then
        sleep 0.5
        python3 - "$SCREEN_PPM" "$SCREEN_PNG" <<'PY'
import sys
from PIL import Image
Image.open(sys.argv[1]).save(sys.argv[2])
PY
        log "captura guardada en $SCREEN_PNG"
    else
        log "AVISO: no se pudo ejecutar screendump"
    fi
fi

log "sesión T4.3 finalizada correctamente"
exit 0
