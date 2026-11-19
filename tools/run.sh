#!/usr/bin/env bash
# tools/run.sh — Arranca la imagen de Meridian en QEMU con OVMF (A2-A9, T1.1-T1.5, T2.1).
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
#   A9: access without login denied
#   A9: command script PASS
#   A9: rmdir non-empty shows clear error PASS
#   A9: terminal tests PASS
#   T1.2: virtio-blk detect PASS
#   T1.2: virtqueue setup PASS
#   T1.2: sector read PASS
#   T1.2: write/readback PASS
#   T1.2: capability denied without WRITE PASS
#   T1.2: block service tests PASS
#   T1.3: disk objects format PASS
#   T1.3: cow commit PASS
#   T1.3: crash consistency PASS
#   T1.3: checksum detects corruption PASS
#   T1.3: disk object tests PASS
#   T1.5: encrypted volume format PASS
#   T1.5: wrong key rejected PASS
#   T1.5: raw sectors are ciphertext PASS
#   T1.5: persists after reopen PASS
#   T1.5: volume encryption tests PASS
#   T2.1: virtio-net detect PASS
#   T2.1: ethernet framing PASS
#   T2.1: ARP reply PASS
#   T2.1: IPv4 ICMP ping reply PASS
#   T2.1: UDP echo reply PASS
#   T2.1: TCP handshake PASS
#   T2.1: IPv6 ICMPv6 echo PASS
#   T2.1: network stack tests PASS
# El debugcon del bootloader también debe contener:
#   meridian: firma de kernel verificada
# En modo --expect-signature-reject se exige en cambio el rechazo y ausencia de
# "Meridian booting" en COM1.
# Cualquier marcador "A4/A5/A6/A7/A8/A9/T1.2/T1.3/T1.5/T2.1 ... FAIL" hace fallar el script.
#
# T2.1 usa un backend UDP QEMU (`-netdev dgram`) para tráfico Ethernet real
# host<->guest sin TAP/root. `tools/net-peer.py` se arranca en background antes
# de QEMU y debe registrar en `build/net-peer.log`:
#   NETPEER: ARP reply PASS
#   NETPEER: ICMP echo reply PASS
#   NETPEER: UDP echo reply PASS
#   NETPEER: TCP SYN-ACK PASS
#   NETPEER: TCP ACK sent PASS
#   NETPEER: ICMPv6 echo reply PASS
#
# Uso:
#   tools/run.sh [--timeout SEGUNDOS] [--no-screenshot] [--expect-signature-reject]
#
# Variables:
#   MERIDIAN_TIMEOUT        timeout total en segundos (por defecto 45)
#   MERIDIAN_QEMU_DISPLAY   -display de QEMU (por defecto "none")
#   MERIDIAN_MEMORY         memoria de la VM (por defecto "512M")

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BUILD_DIR="${MERIDIAN_BUILD_DIR:-$ROOT/build}"
IMAGE="$BUILD_DIR/meridian.img"
BLOCKTEST_IMAGE="$BUILD_DIR/blocktest.img"

TIMEOUT="${MERIDIAN_TIMEOUT:-45}"
QEMU_DISPLAY="${MERIDIAN_QEMU_DISPLAY:-none}"
MEMORY="${MERIDIAN_MEMORY:-512M}"
TAKE_SCREENSHOT=1
EXPECT_SIGNATURE_REJECT=0

SERIAL_LOG="$BUILD_DIR/serial.log"
DEBUG_LOG="$BUILD_DIR/debugcon.log"
QEMU_LOG="$BUILD_DIR/qemu.log"
MONITOR="$BUILD_DIR/qemu-monitor.sock"
SCREEN_PPM="$BUILD_DIR/screen.ppm"
SCREEN_PNG="$BUILD_DIR/screen.png"
OVMF_VARS="$BUILD_DIR/OVMF_VARS_4M.fd"
NETPEER_LOG="$BUILD_DIR/net-peer.log"
NETPEER_STDOUT="$BUILD_DIR/net-peer.stdout"
NETPEER_STDERR="$BUILD_DIR/net-peer.stderr"

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
        --expect-signature-reject)
            EXPECT_SIGNATURE_REJECT=1
            shift
            ;;
        -h|--help)
            sed -n '2,75p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
            exit 0
            ;;
        *)
            fail "opción desconocida: $1"
            ;;
    esac
done

command -v qemu-system-x86_64 >/dev/null 2>&1 || fail "qemu-system-x86_64 no está instalado"
command -v socat >/dev/null 2>&1 || fail "socat no está instalado (necesario para el monitor QEMU)"
command -v python3 >/dev/null 2>&1 || fail "python3 no está instalado (necesario para el peer T2.1)"
[[ -f "$IMAGE" ]] || fail "no existe $IMAGE; ejecuta tools/build.sh primero"
[[ -f "$BLOCKTEST_IMAGE" ]] || fail "no existe $BLOCKTEST_IMAGE; ejecuta tools/build.sh primero"

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
rm -f "$SERIAL_LOG" "$DEBUG_LOG" "$QEMU_LOG" "$MONITOR" "$SCREEN_PPM" "$SCREEN_PNG" "$NETPEER_LOG" "$NETPEER_STDOUT" "$NETPEER_STDERR"
cp "$OVMF_VARS_TEMPLATE" "$OVMF_VARS"

ACCEL=()
if [[ -w /dev/kvm ]]; then
    ACCEL=(-accel kvm)
fi

QEMU_PID=""
NETPEER_PID=""
cleanup() {
    if [[ -n "$QEMU_PID" ]] && kill -0 "$QEMU_PID" 2>/dev/null; then
        kill "$QEMU_PID" 2>/dev/null || true
        wait "$QEMU_PID" 2>/dev/null || true
    fi
    if [[ -n "$NETPEER_PID" ]] && kill -0 "$NETPEER_PID" 2>/dev/null; then
        kill "$NETPEER_PID" 2>/dev/null || true
        wait "$NETPEER_PID" 2>/dev/null || true
    fi
}
trap cleanup EXIT

log "firmware: $OVMF_CODE"
log "imagen:   $IMAGE"
log "timeout:  ${TIMEOUT}s"

# Peer T2.1: recibe frames del guest en 55124 y envía al guest en 55125.
if (( !EXPECT_SIGNATURE_REJECT )); then
    python3 "$ROOT/tools/net-peer.py" \
        --bind-port 55124 --peer-port 55125 \
        --log "$NETPEER_LOG" \
        >"$NETPEER_STDOUT" 2>"$NETPEER_STDERR" &
    NETPEER_PID=$!
    log "net-peer PID $NETPEER_PID (log: $NETPEER_LOG)"
    sleep 0.2
fi

qemu-system-x86_64 \
    -machine q35 \
    -m "$MEMORY" \
    "${ACCEL[@]}" \
    -drive "if=pflash,format=raw,unit=0,readonly=on,file=$OVMF_CODE" \
    -drive "if=pflash,format=raw,unit=1,file=$OVMF_VARS" \
    -drive "file=$IMAGE,format=raw,if=virtio" \
    -drive "file=$BLOCKTEST_IMAGE,format=raw,if=none,id=blockqa" \
    -device "virtio-blk-pci,drive=blockqa,addr=03.0" \
    -netdev dgram,id=netqa,local.type=inet,local.host=127.0.0.1,local.port=55125,remote.type=inet,remote.host=127.0.0.1,remote.port=55124 \
    -device "virtio-net-pci,disable-modern=on,netdev=netqa,mac=52:54:00:00:00:15,addr=04.0" \
    -vga std \
    -display "$QEMU_DISPLAY" \
    -no-reboot \
    -serial "file:$SERIAL_LOG" \
    -debugcon "file:$DEBUG_LOG" \
    -monitor "unix:$MONITOR,server,nowait" \
    -pidfile "$BUILD_DIR/qemu.pid" \
    >"$QEMU_LOG" 2>&1 &
QEMU_PID=$!

log "QEMU PID $QEMU_PID; esperando al kernel en COM1..."

deadline=$((SECONDS + TIMEOUT))
signature_ok=0
signature_reject_ok=0
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
a9_access_ok=0
a9_script_ok=0
a9_rmdir_nonempty_ok=0
a9_summary_ok=0
t12_detect_ok=0
t12_queue_ok=0
t12_read_ok=0
t12_write_ok=0
t12_cap_ok=0
t12_summary_ok=0
t13_format_ok=0
t13_cow_ok=0
t13_crash_ok=0
t13_checksum_ok=0
t13_summary_ok=0
t15_format_ok=0
t15_wrong_key_ok=0
t15_raw_ok=0
t15_reopen_ok=0
t15_summary_ok=0
t21_detect_ok=0
t21_eth_ok=0
t21_arp_ok=0
t21_icmp_ok=0
t21_udp_ok=0
t21_tcp_ok=0
t21_icmpv6_ok=0
t21_summary_ok=0
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
        if grep -q "A9: access without login denied" "$SERIAL_LOG"; then a9_access_ok=1; fi
        if grep -q "A9: command script PASS" "$SERIAL_LOG"; then a9_script_ok=1; fi
        if grep -q "A9: rmdir non-empty shows clear error PASS" "$SERIAL_LOG"; then a9_rmdir_nonempty_ok=1; fi
        if grep -q "A9: terminal tests PASS" "$SERIAL_LOG"; then a9_summary_ok=1; fi
        if grep -q "T1.2: virtio-blk detect PASS" "$SERIAL_LOG"; then t12_detect_ok=1; fi
        if grep -q "T1.2: virtqueue setup PASS" "$SERIAL_LOG"; then t12_queue_ok=1; fi
        if grep -q "T1.2: sector read PASS" "$SERIAL_LOG"; then t12_read_ok=1; fi
        if grep -q "T1.2: write/readback PASS" "$SERIAL_LOG"; then t12_write_ok=1; fi
        if grep -q "T1.2: capability denied without WRITE PASS" "$SERIAL_LOG"; then t12_cap_ok=1; fi
        if grep -q "T1.2: block service tests PASS" "$SERIAL_LOG"; then t12_summary_ok=1; fi
        if grep -q "T1.3: disk objects format PASS" "$SERIAL_LOG"; then t13_format_ok=1; fi
        if grep -q "T1.3: cow commit PASS" "$SERIAL_LOG"; then t13_cow_ok=1; fi
        if grep -q "T1.3: crash consistency PASS" "$SERIAL_LOG"; then t13_crash_ok=1; fi
        if grep -q "T1.3: checksum detects corruption PASS" "$SERIAL_LOG"; then t13_checksum_ok=1; fi
        if grep -q "T1.3: disk object tests PASS" "$SERIAL_LOG"; then t13_summary_ok=1; fi
        if grep -q "T1.5: encrypted volume format PASS" "$SERIAL_LOG"; then t15_format_ok=1; fi
        if grep -q "T1.5: wrong key rejected PASS" "$SERIAL_LOG"; then t15_wrong_key_ok=1; fi
        if grep -q "T1.5: raw sectors are ciphertext PASS" "$SERIAL_LOG"; then t15_raw_ok=1; fi
        if grep -q "T1.5: persists after reopen PASS" "$SERIAL_LOG"; then t15_reopen_ok=1; fi
        if grep -q "T1.5: volume encryption tests PASS" "$SERIAL_LOG"; then t15_summary_ok=1; fi
        if grep -q "T2.1: virtio-net detect PASS" "$SERIAL_LOG"; then t21_detect_ok=1; fi
        if grep -q "T2.1: ethernet framing PASS" "$SERIAL_LOG"; then t21_eth_ok=1; fi
        if grep -q "T2.1: ARP reply PASS" "$SERIAL_LOG"; then t21_arp_ok=1; fi
        if grep -q "T2.1: IPv4 ICMP ping reply PASS" "$SERIAL_LOG"; then t21_icmp_ok=1; fi
        if grep -q "T2.1: UDP echo reply PASS" "$SERIAL_LOG"; then t21_udp_ok=1; fi
        if grep -q "T2.1: TCP handshake PASS" "$SERIAL_LOG"; then t21_tcp_ok=1; fi
        if grep -q "T2.1: IPv6 ICMPv6 echo PASS" "$SERIAL_LOG"; then t21_icmpv6_ok=1; fi
        if grep -q "T2.1: network stack tests PASS" "$SERIAL_LOG"; then t21_summary_ok=1; fi
        if grep -Eq "A4: .*FAIL|A5: .*FAIL|A6: .*FAIL|A7: .*FAIL|A8: .*FAIL|A9: .*FAIL|T1\.2: .*FAIL|T1\.3: .*FAIL|T1\.5: .*FAIL|T2\.1: .*FAIL" "$SERIAL_LOG"; then
            fail_marker=1
            break
        fi
    fi
    if [[ -f "$DEBUG_LOG" ]]; then
        if grep -q "meridian: firma de kernel verificada" "$DEBUG_LOG"; then
            signature_ok=1
        fi
        if grep -Eq "meridian: firma de kernel (inválida|ausente); abortando" "$DEBUG_LOG"; then
            signature_reject_ok=1
        fi
    fi
    if (( EXPECT_SIGNATURE_REJECT )); then
        if (( signature_reject_ok )); then
            break
        fi
    elif (( boot_ok && buddy_ok && wx_ok && a5_create_ok && a5_isolation_ok \
            && a6_ipc_ok && a6_denied_ok && a6_reduce_ok && a6_summary_ok \
            && a7_ops_ok && a7_ro_ok && a7_summary_ok \
            && a8_screen_ok && a8_reject_ok && a8_caps_ok && a8_summary_ok \
            && a9_access_ok && a9_script_ok && a9_rmdir_nonempty_ok && a9_summary_ok \
            && t12_detect_ok && t12_queue_ok && t12_read_ok && t12_write_ok && t12_cap_ok && t12_summary_ok \
            && t13_format_ok && t13_cow_ok && t13_crash_ok && t13_checksum_ok && t13_summary_ok \
            && t15_format_ok && t15_wrong_key_ok && t15_raw_ok && t15_reopen_ok && t15_summary_ok \
            && t21_detect_ok && t21_eth_ok && t21_arp_ok && t21_icmp_ok && t21_udp_ok && t21_tcp_ok && t21_icmpv6_ok && t21_summary_ok \
            && signature_ok )); then
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

# Esperar al peer T2.1 y comprobar su log (tráfico real host<->guest).
peer_ok=1
if (( !EXPECT_SIGNATURE_REJECT )); then
    peer_wait_deadline=$((SECONDS + 5))
    while (( SECONDS < peer_wait_deadline )); do
        if [[ -n "$NETPEER_PID" ]] && kill -0 "$NETPEER_PID" 2>/dev/null; then
            sleep 0.2
        else
            break
        fi
    done
    if [[ -n "$NETPEER_PID" ]] && kill -0 "$NETPEER_PID" 2>/dev/null; then
        kill "$NETPEER_PID" 2>/dev/null || true
        wait "$NETPEER_PID" 2>/dev/null || true
    else
        wait "$NETPEER_PID" 2>/dev/null || true
    fi
    NETPEER_PID=""
    for line in \
        "NETPEER: ARP reply PASS" \
        "NETPEER: ICMP echo reply PASS" \
        "NETPEER: UDP echo reply PASS" \
        "NETPEER: TCP SYN-ACK PASS" \
        "NETPEER: TCP ACK sent PASS" \
        "NETPEER: ICMPv6 echo reply PASS"; do
        if ! grep -Fq "$line" "$NETPEER_LOG" 2>/dev/null; then
            peer_ok=0
            log "FALLO peer: falta '$line' en $NETPEER_LOG"
        fi
    done
    if (( peer_ok )); then
        log "net-peer: 6/6 eventos PASS ($NETPEER_LOG)"
    fi
fi

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
    log "FALLO: el kernel reportó un marcador 'A4/A5/A6/A7/A8/A9/T1.2/T1.3/T1.5/T2.1 ... FAIL' (ver COM1)."
    exit 1
fi

if (( EXPECT_SIGNATURE_REJECT )); then
    if (( signature_reject_ok && !boot_ok && !signature_ok )); then
        log "OK: bootloader rechazó la firma inválida y el kernel no arrancó."
        exit 0
    fi
    log "FALLO: se esperaba rechazo de firma (reject=$signature_reject_ok, boot=$boot_ok, firma_ok=$signature_ok)."
    if [[ -s "$QEMU_LOG" ]]; then
        log "--- log de QEMU ---"
        cat "$QEMU_LOG"
    fi
    exit 1
fi

if (( !peer_ok )); then
    if [[ -s "$NETPEER_STDERR" ]]; then
        log "--- net-peer stderr ---"
        cat "$NETPEER_STDERR"
    fi
    if [[ -s "$NETPEER_LOG" ]]; then
        log "--- net-peer log ---"
        cat "$NETPEER_LOG"
    fi
    exit 1
fi

if (( !signature_reject_ok && signature_ok && boot_ok && buddy_ok && wx_ok \
      && a5_create_ok && a5_isolation_ok \
      && a6_ipc_ok && a6_denied_ok && a6_reduce_ok && a6_summary_ok \
      && a7_ops_ok && a7_ro_ok && a7_summary_ok \
      && a8_screen_ok && a8_reject_ok && a8_caps_ok && a8_summary_ok \
      && a9_access_ok && a9_script_ok && a9_rmdir_nonempty_ok && a9_summary_ok \
      && t12_detect_ok && t12_queue_ok && t12_read_ok && t12_write_ok && t12_cap_ok && t12_summary_ok \
      && t13_format_ok && t13_cow_ok && t13_crash_ok && t13_checksum_ok && t13_summary_ok \
      && t15_format_ok && t15_wrong_key_ok && t15_raw_ok && t15_reopen_ok && t15_summary_ok \
      && t21_detect_ok && t21_eth_ok && t21_arp_ok && t21_icmp_ok && t21_udp_ok && t21_tcp_ok && t21_icmpv6_ok && t21_summary_ok )); then
    log "OK: firma verificada; kernel arrancó ('Meridian booting') y pasó A4, A5 (aislamiento de procesos), A6 (IPC/capacidades), A7 (sistema de archivos RAM), A8 (login/usuarios), A9 (terminal alpha), T1.2 (block service), T1.3 (disk objects: COW/checksums/corte simulado), T1.5 (volume encryption: XTS/Argon2id, wrong key, ciphertext y persistencia) y T2.1 (network stack: virtio-net, ARP/IPv4 ICMP/UDP/TCP/IPv6 ICMPv6 con tráfico real host<->guest) por COM1."
    if (( TAKE_SCREENSHOT )); then
        if [[ -f "$SCREEN_PNG" ]]; then
            log "Captura de pantalla GOP: $SCREEN_PNG"
        elif [[ -f "$SCREEN_PPM" ]]; then
            log "Captura de pantalla GOP: $SCREEN_PPM"
        fi
    fi
    exit 0
fi

log "FALLO: marcadores pendientes (firma=$signature_ok, reject=$signature_reject_ok, boot=$boot_ok, buddy=$buddy_ok, wx=$wx_ok, a5_create=$a5_create_ok, a5_isolation=$a5_isolation_ok, a6_ipc=$a6_ipc_ok, a6_denied=$a6_denied_ok, a6_reduce=$a6_reduce_ok, a6_summary=$a6_summary_ok, a7_ops=$a7_ops_ok, a7_ro=$a7_ro_ok, a7_summary=$a7_summary_ok, a8_screen=$a8_screen_ok, a8_reject=$a8_reject_ok, a8_caps=$a8_caps_ok, a8_summary=$a8_summary_ok, a9_access=$a9_access_ok, a9_script=$a9_script_ok, a9_rmdir_nonempty=$a9_rmdir_nonempty_ok, a9_summary=$a9_summary_ok, t12_detect=$t12_detect_ok, t12_queue=$t12_queue_ok, t12_read=$t12_read_ok, t12_write=$t12_write_ok, t12_cap=$t12_cap_ok, t12_summary=$t12_summary_ok, t13_format=$t13_format_ok, t13_cow=$t13_cow_ok, t13_crash=$t13_crash_ok, t13_checksum=$t13_checksum_ok, t13_summary=$t13_summary_ok, t15_format=$t15_format_ok, t15_wrong_key=$t15_wrong_key_ok, t15_raw=$t15_raw_ok, t15_reopen=$t15_reopen_ok, t15_summary=$t15_summary_ok, t21_detect=$t21_detect_ok, t21_eth=$t21_eth_ok, t21_arp=$t21_arp_ok, t21_icmp=$t21_icmp_ok, t21_udp=$t21_udp_ok, t21_tcp=$t21_tcp_ok, t21_icmpv6=$t21_icmpv6_ok, t21_summary=$t21_summary_ok, peer=$peer_ok)."
if [[ -s "$NETPEER_LOG" ]]; then
    log "--- net-peer log ---"
    cat "$NETPEER_LOG"
fi
if [[ -s "$QEMU_LOG" ]]; then
    log "--- log de QEMU ---"
    cat "$QEMU_LOG"
fi
exit 1
