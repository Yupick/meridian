#!/usr/bin/env bash
# tests/run-install.sh — QA de T3.3: imagen instalable desde cero y arrancada en QEMU.
#
# Secuencia:
#   1. tools/build.sh para artefactos frescos (log: build/install/build.log).
#   2. tools/install-image.sh --disk build/install/meridian-installed.img
#      --artifacts build (log: build/install/install.log).
#   3. Verificación de la tabla GPT (sgdisk) y de los 3 archivos de la ESP con
#      mdir sobre la imagen instalada (logs: sgdisk.log y mdir.log).
#   4. Arranque con tools/run.sh --image ... --blocktest ... --no-screenshot
#      (log: build/install/run.log) y comprobación de código 0, 83/83 marcadores
#      (76 en COM1 + 7 TLSCAP), 6/6 NETPEER y 7/7 TLSCAP.
#
# Uso:
#   tests/run-install.sh
#
# Variables:
#   MERIDIAN_BUILD_DIR  directorio de build (por defecto <repo>/build)

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BUILD_DIR="${MERIDIAN_BUILD_DIR:-$ROOT/build}"
INSTALL_DIR="$BUILD_DIR/install"

BUILD_LOG="$INSTALL_DIR/build.log"
INSTALL_LOG="$INSTALL_DIR/install.log"
SGDISK_LOG="$INSTALL_DIR/sgdisk.log"
MDIR_LOG="$INSTALL_DIR/mdir.log"
RUN_LOG="$INSTALL_DIR/run.log"

SERIAL_LOG="$INSTALL_DIR/serial.log"
NETPEER_LOG="$INSTALL_DIR/net-peer.log"
TLSCAP_LOG="$INSTALL_DIR/tls-capture.log"

INSTALLED_IMAGE="$INSTALL_DIR/meridian-installed.img"
ESP_OFFSET=$((2048 * 512)) # tools/install-image.sh: sector 2048, 512 bytes/sector

log()  { printf '[install-qa] %s\n' "$*"; }
fail() { printf '[install-qa] ERROR: %s\n' "$*" >&2; exit 1; }

# --- Marcadores esperados (idénticos a tests/run-alpha.sh: 76 COM1 + 7 TLSCAP) ---
SERIAL_MARKERS=(
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
    "T2.3: TLS 1.3 handshake PASS"
    "T2.3: Finished verified PASS"
    "T2.3: encrypted read allowed PASS"
    "T2.3: encrypted read denied PASS"
    "T2.3: replay rejected PASS"
    "T2.3: remote session tests PASS"
)

TLSCAP_MARKERS=(
    "TLSCAP: ClientHello observed"
    "TLSCAP: ServerHello verified"
    "TLSCAP: Finished verified"
    "TLSCAP: application ciphertext captured"
    "TLSCAP: encrypted read allowed PASS"
    "TLSCAP: encrypted read denied PASS"
    "TLSCAP: replay rejected PASS"
)

NETPEER_MARKERS=(
    "NETPEER: ARP reply PASS"
    "NETPEER: ICMP echo reply PASS"
    "NETPEER: UDP echo reply PASS"
    "NETPEER: TCP SYN-ACK PASS"
    "NETPEER: TCP ACK sent PASS"
    "NETPEER: ICMPv6 echo reply PASS"
)

TOTAL_MARKERS=$((${#SERIAL_MARKERS[@]} + ${#TLSCAP_MARKERS[@]}))
MARKERS_FOUND=0
MARKERS_MISSING=0

count_markers() {
    local logfile="$1"
    shift
    local marker
    for marker in "$@"; do
        if [[ -f "$logfile" ]] && grep -Fq -- "$marker" "$logfile"; then
            MARKERS_FOUND=$((MARKERS_FOUND + 1))
        else
            printf '  [FALTA] %s\n' "$marker" >&2
            MARKERS_MISSING=$((MARKERS_MISSING + 1))
        fi
    done
}

mkdir -p "$INSTALL_DIR"
log "repo: $ROOT"
log "build: $BUILD_DIR"
log "logs: $INSTALL_DIR"

# --- 1. Artefactos frescos ---
log "construyendo artefactos (ver $BUILD_LOG)"
MERIDIAN_BUILD_DIR="$BUILD_DIR" "$ROOT/tools/build.sh" 2>&1 | tee "$BUILD_LOG"

# --- 2. Instalación desde cero ---
log "instalando imagen desde cero (ver $INSTALL_LOG)"
"$ROOT/tools/install-image.sh" --disk "$INSTALLED_IMAGE" --artifacts "$BUILD_DIR" 2>&1 | tee "$INSTALL_LOG"

[[ -f "$INSTALLED_IMAGE" ]] || fail "no se generó $INSTALLED_IMAGE"
if [[ -e "$INSTALL_DIR/blocktest.img" ]]; then
    fail "install-image.sh no debe crear blocktest.img (es un disco QA separado)"
fi

# --- 3. Verificación de la imagen instalada ---
command -v sgdisk >/dev/null 2>&1 || fail "falta sgdisk en PATH"
command -v mdir >/dev/null 2>&1 || fail "falta mdir en PATH"

sgdisk -p "$INSTALLED_IMAGE" >"$SGDISK_LOG" 2>&1 || fail "sgdisk -p falló (ver $SGDISK_LOG)"
grep -q 'EF00' "$SGDISK_LOG" || fail "la partición 1 no tiene typecode ef00 (ver $SGDISK_LOG)"
grep -q 'MERIDIAN' "$SGDISK_LOG" || fail "la partición 1 no se llama MERIDIAN (ver $SGDISK_LOG)"
grep -q '2048' "$SGDISK_LOG" || fail "la partición 1 no empieza en el sector 2048 (ver $SGDISK_LOG)"
log "GPT verificada con sgdisk (ver $SGDISK_LOG)"

mdir -i "${INSTALLED_IMAGE}@@${ESP_OFFSET}" -/ ::/ >"$MDIR_LOG" 2>&1 || fail "mdir no pudo leer la ESP (ver $MDIR_LOG)"
for entry in EFI/BOOT/BOOTX64.EFI kernel.elf kernel.sig; do
    if ! mdir -i "${INSTALLED_IMAGE}@@${ESP_OFFSET}" "::/$entry" >/dev/null 2>&1; then
        cat "$MDIR_LOG" >&2
        fail "falta $entry en la ESP instalada"
    fi
    log "[OK] ESP: $entry"
done
log "ESP verificada con mdir (ver $MDIR_LOG)"

# --- 4. Arranque de la imagen instalada en QEMU ---
log "arrancando imagen instalada en QEMU/OVMF (ver $RUN_LOG)"
if MERIDIAN_BUILD_DIR="$INSTALL_DIR" "$ROOT/tools/run.sh" \
        --image "$INSTALLED_IMAGE" \
        --blocktest "$BUILD_DIR/blocktest.img" \
        --no-screenshot 2>&1 | tee "$RUN_LOG"; then
    :
else
    fail "tools/run.sh devolvió código de salida != 0 (ver $RUN_LOG)"
fi

[[ -s "$SERIAL_LOG" ]] || fail "no hay salida COM1 en $SERIAL_LOG"

log "verificando ${#SERIAL_MARKERS[@]} marcadores en COM1 y ${#TLSCAP_MARKERS[@]} en TLSCAP (total $TOTAL_MARKERS)"
count_markers "$SERIAL_LOG" "${SERIAL_MARKERS[@]}"
count_markers "$TLSCAP_LOG" "${TLSCAP_MARKERS[@]}"
if (( MARKERS_FOUND != TOTAL_MARKERS || MARKERS_MISSING != 0 )); then
    fail "marcadores incompletos: $MARKERS_FOUND/$TOTAL_MARKERS (faltan $MARKERS_MISSING)"
fi
log "$MARKERS_FOUND/$TOTAL_MARKERS marcadores verificados"

netpeer_found=0
for marker in "${NETPEER_MARKERS[@]}"; do
    if grep -Fq -- "$marker" "$NETPEER_LOG" 2>/dev/null; then
        netpeer_found=$((netpeer_found + 1))
    else
        printf '  [FALTA] %s\n' "$marker" >&2
    fi
done
(( netpeer_found == 6 )) || fail "NETPEER incompleto: $netpeer_found/6 (ver $NETPEER_LOG)"
log "6/6 NETPEER verificados en $NETPEER_LOG"
log "7/7 TLSCAP verificados en $TLSCAP_LOG"

if ! grep -Fq '6/6 NETPEER y 7/7 TLSCAP PASS' "$RUN_LOG"; then
    fail "tools/run.sh no confirmó 6/6 NETPEER y 7/7 TLSCAP (ver $RUN_LOG)"
fi

if grep -Eq 'A4: .*FAIL|A5: .*FAIL|A6: .*FAIL|A7: .*FAIL|A8: .*FAIL|A9: .*FAIL|T1\.2: .*FAIL|T1\.3: .*FAIL|T1\.5: .*FAIL|T2\.1: .*FAIL|T2\.2: .*FAIL|T2\.3: .*FAIL' "$SERIAL_LOG"; then
    fail "el kernel reportó un marcador FAIL (ver $SERIAL_LOG)"
fi
if [[ -f "$TLSCAP_LOG" ]] && grep -Eq 'TLSCAP: .*FAIL' "$TLSCAP_LOG"; then
    fail "el peer TLS reportó un marcador FAIL (ver $TLSCAP_LOG)"
fi

printf '%s\n' "T3.3 install QA PASS: imagen instalada desde cero y arrancada en QEMU ($TOTAL_MARKERS/$TOTAL_MARKERS marcadores)"
