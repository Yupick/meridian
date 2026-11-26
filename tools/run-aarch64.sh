#!/usr/bin/env bash
# tools/run-aarch64.sh — Arranca el kernel aarch64 en QEMU `virt` y verifica los
# marcadores del HAL T5.1 por UART PL011.
#
# Uso: tools/run-aarch64.sh [--mode direct|uefi] [--timeout SEGUNDOS] [--no-screenshot]
#
# Modos:
#   direct (por defecto): ROM mínima + loader raw; pasa la DTB en x0.
#   uefi: imagen GPT con AAVMF, bootloader y handoff BootInfo.
#
# Variables:
#   MERIDIAN_AARCH64_BUILD_DIR   directorio de build (por defecto build/aarch64)
#   MERIDIAN_AARCH64_MODE        modo por defecto (direct|uefi)
#   MERIDIAN_QEMU_AARCH64        ruta a qemu-system-aarch64
#   MERIDIAN_AAVMF_CODE          ruta al firmware AAVMF/QEMU_EFI code
#   MERIDIAN_AAVMF_VARS          plantilla de variables AAVMF

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BUILD_DIR="${MERIDIAN_AARCH64_BUILD_DIR:-$ROOT/build/aarch64}"
MODE="${MERIDIAN_AARCH64_MODE:-direct}"
IMAGE="$BUILD_DIR/meridian-aarch64.img"
KERNEL_ELF="$BUILD_DIR/kernel-aarch64.elf"
KERNEL_BIN="$BUILD_DIR/kernel-aarch64.bin"
BOOTROM="$BUILD_DIR/bootrom.bin"
SERIAL="$BUILD_DIR/serial.log"
MONITOR="$BUILD_DIR/qemu-monitor.sock"
PIDFILE="$BUILD_DIR/qemu.pid"
SCREEN_PPM="$BUILD_DIR/screen.ppm"
SCREEN_PNG="$BUILD_DIR/screen.png"
VARS="$BUILD_DIR/AAVMF_VARS.fd"
TIMEOUT=90
TAKE_SCREENSHOT=1

log()  { printf '[run-aarch64] %s\n' "$*"; }
fail() { printf '[run-aarch64] ERROR: %s\n' "$*" >&2; exit 1; }

while [[ $# -gt 0 ]]; do
    case "$1" in
        --mode)
            [[ $# -ge 2 ]] || fail "--mode requiere un valor"
            MODE="$2"; shift 2
            ;;
        --timeout)
            [[ $# -ge 2 ]] || fail "--timeout requiere un valor"
            TIMEOUT="$2"; shift 2
            ;;
        --no-screenshot)
            TAKE_SCREENSHOT=0; shift
            ;;
        -h|--help)
            sed -n '2,/^set -euo pipefail$/p' "${BASH_SOURCE[0]}" | sed '$d' | sed 's/^# \{0,1\}//'
            exit 0
            ;;
        *) fail "opción desconocida: $1" ;;
    esac
done

[[ "$MODE" == "direct" || "$MODE" == "uefi" ]] || fail "modo desconocido: $MODE"

find_qemu() {
    if [[ -n "${MERIDIAN_QEMU_AARCH64:-}" ]]; then
        printf '%s\n' "$MERIDIAN_QEMU_AARCH64"
    elif command -v qemu-system-aarch64 >/dev/null 2>&1; then
        command -v qemu-system-aarch64
    elif [[ -x /tmp/opencode/a64-root/usr/bin/qemu-system-aarch64 ]]; then
        printf '%s\n' /tmp/opencode/a64-root/usr/bin/qemu-system-aarch64
    else
        fail "qemu-system-aarch64 no está disponible"
    fi
}

find_code() {
    local candidate
    for candidate in \
        "${MERIDIAN_AAVMF_CODE:-}" \
        /usr/share/AAVMF/AAVMF_CODE.no-secboot.fd \
        /usr/share/qemu-efi-aarch64/QEMU_EFI.fd \
        /tmp/opencode/a64-root/usr/share/AAVMF/AAVMF_CODE.no-secboot.fd \
        /tmp/opencode/a64-root/usr/share/qemu-efi-aarch64/QEMU_EFI.fd
    do
        [[ -n "$candidate" && -f "$candidate" ]] && { printf '%s\n' "$candidate"; return; }
    done
    fail "no se encontró firmware AAVMF/QEMU_EFI"
}

find_vars() {
    local candidate
    for candidate in \
        "${MERIDIAN_AAVMF_VARS:-}" \
        /usr/share/AAVMF/AAVMF_VARS.fd \
        /usr/share/qemu-efi-aarch64/QEMU_VARS.fd \
        /tmp/opencode/a64-root/usr/share/AAVMF/AAVMF_VARS.fd
    do
        [[ -n "$candidate" && -f "$candidate" ]] && { printf '%s\n' "$candidate"; return; }
    done
    fail "no se encontró plantilla AAVMF_VARS/QEMU_VARS"
}

QEMU="$(find_qemu)"
command -v socat >/dev/null 2>&1 || fail "falta socat"
[[ -f "$KERNEL_ELF" ]] || fail "no existe $KERNEL_ELF (ejecuta tools/build-aarch64.sh)"

mkdir -p "$BUILD_DIR"
rm -f "$SERIAL" "$MONITOR" "$PIDFILE" "$SCREEN_PPM"

COMMON_ARGS=(
    -machine virt
    -cpu cortex-a57
    -smp 1
    -m 1024M
    -device ramfb
    -serial file:"$SERIAL"
    -display none
    -monitor unix:"$MONITOR",server,nowait
    -pidfile "$PIDFILE"
    -daemonize
)

if [[ "$MODE" == "direct" ]]; then
    [[ -f "$KERNEL_BIN" ]] || fail "no existe $KERNEL_BIN (ejecuta tools/build-aarch64.sh)"
    [[ -f "$BOOTROM" ]] || fail "no existe $BOOTROM (ejecuta tools/build-aarch64.sh)"
    KERNEL_LOAD_BASE="$(python3 -c 'import re,sys; s=open(sys.argv[1]).read(); m=re.search(r"cfg\(target_arch = \"aarch64\"\)\]\s*pub const KERNEL_LOAD_BASE: u64 = (0x[0-9a-fA-F_]+)", s); print(m.group(1).replace("_", ""))' "$ROOT/bootinfo/src/lib.rs")"
    log "QEMU: $QEMU"
    log "modo: direct (kernel raw en $KERNEL_LOAD_BASE)"
    "$QEMU" \
        -bios "$BOOTROM" \
        -device loader,file="$KERNEL_BIN",addr="$KERNEL_LOAD_BASE" \
        "${COMMON_ARGS[@]}"
else
    CODE="$(find_code)"
    VARS_TEMPLATE="$(find_vars)"
    [[ -f "$IMAGE" ]] || fail "no existe $IMAGE (ejecuta tools/build-aarch64.sh)"
    cp "$VARS_TEMPLATE" "$VARS"
    log "QEMU: $QEMU"
    log "modo: uefi (firmware $CODE)"
    "$QEMU" \
        -drive if=pflash,format=raw,readonly=on,file="$CODE" \
        -drive if=pflash,format=raw,file="$VARS" \
        -drive if=none,file="$IMAGE",format=raw,id=hd0 \
        -device virtio-blk-device,drive=hd0 \
        "${COMMON_ARGS[@]}"
fi

QEMU_PID="$(cat "$PIDFILE")"
cleanup() {
    if kill -0 "$QEMU_PID" 2>/dev/null; then
        kill "$QEMU_PID" 2>/dev/null || true
        wait "$QEMU_PID" 2>/dev/null || true
    fi
    rm -f "$MONITOR"
}
trap cleanup EXIT

MARKERS=(
    "Meridian booting"
    "T5.1: UART PL011 activo"
    "T5.1: HAL aarch64 OK; kernel vivo"
    "MERIDIAN AARCH64 BOOT OK"
)
if [[ "$MODE" == "direct" ]]; then
    MARKERS+=("T5.1: arranque directo QEMU aarch64")
else
    MARKERS+=("T5.1: BootInfo v")
    MARKERS+=("T5.1: framebuffer GOP activo")
fi

deadline=$((SECONDS + TIMEOUT))
while (( SECONDS < deadline )); do
    if grep -q 'PANIC\|MERIDIAN-KERNEL: ERROR\|T5.1: ERROR\|Synchronous Exception' "$SERIAL" 2>/dev/null; then
        fail "el kernel aarch64 informó un error"
    fi
    missing=0
    for marker in "${MARKERS[@]}"; do
        grep -qF "$marker" "$SERIAL" 2>/dev/null || missing=1
    done
    if (( missing == 0 )); then
        break
    fi
    sleep 1
done

for marker in "${MARKERS[@]}"; do
    grep -qF "$marker" "$SERIAL" || fail "falta el marcador: $marker"
done

if (( TAKE_SCREENSHOT )); then
    printf 'screendump %s\n' "$SCREEN_PPM" | socat - "UNIX-CONNECT:$MONITOR" >/dev/null 2>&1 || true
    sleep 1
    if [[ -f "$SCREEN_PPM" ]]; then
        if command -v convert >/dev/null 2>&1; then
            convert "$SCREEN_PPM" "$SCREEN_PNG" >/dev/null 2>&1 || cp "$SCREEN_PPM" "$SCREEN_PNG"
        else
            cp "$SCREEN_PPM" "$SCREEN_PNG"
        fi
    else
        log "AVISO: no hay captura GOP; UART verificado"
    fi
fi

log "logueando salida UART:"
grep -E 'Meridian booting|T5\.1:|MERIDIAN AARCH64' "$SERIAL" || true
log "PASS: arranque aarch64 verificado en modo $MODE"
