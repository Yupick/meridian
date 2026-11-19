#!/usr/bin/env bash
# tests/run-alpha.sh — QA de integración de la Alpha (A10, Fase 0) + T1.x/T2.1.
#
# Un único comando construye Meridian, arranca la imagen en QEMU/OVMF y
# verifica por COM1 que:
#   - el login se muestra y autentica (A8);
#   - la terminal exige login y ejecuta TODOS los comandos de A9:
#     ls, cd, pwd, mkdir/md, rmdir/rd, rm, rm -r, cat, echo > archivo,
#     help, clear y exit;
#   - los objetos en disco con COW, checksums y corte simulado (T1.3);
#   - el volumen cifrado XTS-AES-256/Argon2id y su persistencia (T1.5);
#   - la pila de red virtio-net con tráfico real host<->guest (T2.1);
#   - los grupos y ACLs sobre el volumen cifrado, con acceso permitido y
#     denegado (T2.2);
#   - no aparece ningún marcador "A4/A5/A6/A7/A8/A9/T1.2/T1.3/T1.5/T2.1/T2.2 ... FAIL".
#
# Devuelve 0 solo si la build, el arranque y todas las comprobaciones pasan.
#
# Uso:
#   tests/run-alpha.sh [--timeout SEGUNDOS] [--no-screenshot] [--no-build]
#
# Variables:
#   MERIDIAN_BUILD_DIR  directorio de salida (por defecto <repo>/build)
#   MERIDIAN_TIMEOUT    timeout de QEMU (por defecto 45 s)
#   MERIDIAN_MEMORY     memoria de la VM (por defecto 512M)
#   MERIDIAN_QEMU_DISPLAY  -display de QEMU (por defecto "none")

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BUILD_DIR="${MERIDIAN_BUILD_DIR:-$ROOT/build}"
BUILD_LOG="$BUILD_DIR/alpha-build.log"
RUN_LOG="$BUILD_DIR/alpha-run.log"
SERIAL_LOG="$BUILD_DIR/serial.log"

DO_BUILD=1
RUN_ARGS=()

log()  { printf '[alpha] %s\n' "$*"; }
fail() { printf '[alpha] ERROR: %s\n' "$*" >&2; exit 1; }

usage() {
    sed -n '2,27p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --timeout)
            [[ $# -ge 2 ]] || fail "--timeout requiere un valor"
            RUN_ARGS+=(--timeout "$2")
            shift 2
            ;;
        --no-screenshot)
            RUN_ARGS+=(--no-screenshot)
            shift
            ;;
        --no-build)
            DO_BUILD=0
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

mkdir -p "$BUILD_DIR"
log "repo: $ROOT"
log "build: $BUILD_DIR"

if (( DO_BUILD )); then
    log "construyendo imagen (ver $BUILD_LOG)"
    "$ROOT/tools/build.sh" 2>&1 | tee "$BUILD_LOG"
else
    log "omitiendo build (--no-build)"
fi

log "arrancando QEMU/OVMF y ejecutando login + comandos A9 (ver $RUN_LOG)"
"$ROOT/tools/run.sh" "${RUN_ARGS[@]}" 2>&1 | tee "$RUN_LOG"

[[ -s "$SERIAL_LOG" ]] || fail "no hay salida COM1 en $SERIAL_LOG"

# Verificación explícita de login (A8), comandos terminal (A9), T1.x, T2.1 y T2.2.
REQUIRED_MARKERS=(
    "Meridian booting"
    "A4: buddy alloc/free tests PASS"
    "A4: W^X check PASS"
    "A5: process creation OK"
    "A5: process isolation test PASS"
    "A6: ipc send/recv test PASS"
    "A6: capability denied without rights PASS"
    "A6: capability reduction/attenuation PASS"
    "A6: ipc/capabilities tests PASS"
    "A7: fs operations tests PASS"
    "A7: read-only capability denied write PASS"
    "A7: fs service tests PASS"
    "=== Meridian login ==="
    "A8: login screen shown"
    "A8: login correcto abre sesion (user=alice, home=/home/alice)"
    "A8: invalid credentials rejected without user disclosure"
    "A8: session has capabilities over personal directory"
    "A8: login/users tests PASS"
    "A9: terminal access requires login"
    "terminal: acceso denegado (login requerido)"
    "A9: access without login denied"
    "A9: login ok, terminal session started (user=root)"
    "Comandos: ls, cd, pwd, mkdir/md, rmdir/rd, rm [-r], cat, echo texto > archivo, help, clear, exit"
    "A9: help PASS"
    "A9: pwd PASS"
    "A9: mkdir/md PASS"
    "A9: ls PASS"
    "A9: cd/pwd PASS"
    "A9: echo > PASS"
    "hola-meridian"
    "A9: cat PASS"
    "rmdir: error: el directorio no esta vacio: 'docs'"
    "A9: rmdir non-empty shows clear error PASS"
    "A9: rm PASS"
    "A9: rmdir/rd PASS"
    "A9: rm -r PASS"
    "A9: clear PASS"
    "A9: exit PASS"
    "A9: command script PASS (ls,cd,pwd,mkdir,md,rmdir,rd,rm,rm -r,cat,echo>,help,clear,exit)"
    "A9: terminal tests PASS (scripted commands, rmdir non-empty error, login required)"
    "T1.2: virtio-blk detect PASS"
    "T1.2: virtqueue setup PASS"
    "T1.2: sector read PASS"
    "T1.2: write/readback PASS"
    "T1.2: capability denied without WRITE PASS"
    "T1.2: block service tests PASS"
    "T1.3: disk objects format PASS"
    "T1.3: cow commit PASS"
    "T1.3: crash consistency PASS"
    "T1.3: checksum detects corruption PASS"
    "T1.3: disk object tests PASS"
    "T1.5: encrypted volume format PASS"
    "T1.5: wrong key rejected PASS"
    "T1.5: raw sectors are ciphertext PASS"
    "T1.5: persists after reopen PASS"
    "T1.5: volume encryption tests PASS"
    "T2.1: virtio-net detect PASS"
    "T2.1: ethernet framing PASS"
    "T2.1: ARP reply PASS"
    "T2.1: IPv4 ICMP ping reply PASS"
    "T2.1: UDP echo reply PASS"
    "T2.1: TCP handshake PASS"
    "T2.1: IPv6 ICMPv6 echo PASS"
    "T2.1: network stack tests PASS"
    "T2.2: group assignment PASS"
    "T2.2: ACL read allowed PASS"
    "T2.2: ACL write denied PASS"
    "T2.2: group isolation PASS"
    "T2.2: encrypted ACL persistence PASS"
    "T2.2: permissions tests PASS"
)

log "verificando ${#REQUIRED_MARKERS[@]} marcadores en COM1"
missing=0
for marker in "${REQUIRED_MARKERS[@]}"; do
    if grep -Fq -- "$marker" "$SERIAL_LOG"; then
        printf '  [OK] %s\n' "$marker"
    else
        printf '  [FALTA] %s\n' "$marker" >&2
        missing=$((missing + 1))
    fi
done
(( missing == 0 )) || fail "faltan $missing marcadores de Alpha/T2.2 en $SERIAL_LOG"

if grep -Eq 'A4: .*FAIL|A5: .*FAIL|A6: .*FAIL|A7: .*FAIL|A8: .*FAIL|A9: .*FAIL|T1\.2: .*FAIL|T1\.3: .*FAIL|T1\.5: .*FAIL|T2\.1: .*FAIL|T2\.2: .*FAIL' "$SERIAL_LOG"; then
    fail "el kernel reportó un marcador FAIL (ver $SERIAL_LOG)"
fi

log "OK: Alpha QA PASS — login + comandos A9 ejecutados en QEMU (${#REQUIRED_MARKERS[@]}/${#REQUIRED_MARKERS[@]} marcadores; ${#REQUIRED_MARKERS[@]} esperados)"
log "Serial: $SERIAL_LOG"
log "Logs:   $BUILD_LOG, $RUN_LOG"
if [[ -f "$BUILD_DIR/net-peer.log" ]]; then
    log "Net peer: $BUILD_DIR/net-peer.log"
fi
if [[ -f "$BUILD_DIR/screen.png" ]]; then
    log "GOP:    $BUILD_DIR/screen.png"
fi
exit 0
