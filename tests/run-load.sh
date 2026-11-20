#!/usr/bin/env bash
# tests/run-load.sh — Burn-in / load QA para estabilización T3.1.
#
# Ejecuta múltiples iteraciones de tests/run-alpha.sh y verifica que los
# resultados son reproducibles. Está pensada para detectar flakiness del
# arranque UEFI/QEMU, red UDP dgram y TLS PSK tras la Fase 2.
#
# Uso:
#   tests/run-load.sh [--runs N] [--no-screenshot]
#
# Variables:
#   MERIDIAN_LOAD_RUNS  número de iteraciones por defecto (--runs la sobrescribe)
#   MERIDIAN_BUILD_DIR  directorio de build (por defecto <repo>/build)

set -euo pipefail

# --- Funciones de log ---
log()    { printf '[load] %s\n' "$*"; }
fail()   { printf '[load] ERROR: %s\n' "$*" >&2; exit 1; }

# --- Cálculo de ROOT (igual que los scripts hermanos) ---
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

# --- Parámetros ---
RUNS="${MERIDIAN_LOAD_RUNS:-5}"
NO_SCREENSHOT=0

while [[ $# -gt 0 ]]; do
    case "$1" in
        --runs)
            [[ $# -ge 2 ]] || fail "--runs requiere un valor"
            RUNS="$2"
            shift 2
            ;;
        --no-screenshot)
            NO_SCREENSHOT=1
            shift
            ;;
        -h|--help)
            sed -n '2,35p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
            exit 0
            ;;
        *)
            fail "opción desconocida: $1"
            ;;
    esac
done

[[ "$RUNS" =~ ^[1-9][0-9]*$ ]] || fail "--runs debe ser un entero positivo"

# --- Directorios ---
BUILD_DIR="${MERIDIAN_BUILD_DIR:-$ROOT/build}"
LOAD_DIR="$BUILD_DIR/load"

# --- Limpieza previa ---
rm -rf "$LOAD_DIR"
mkdir -p "$LOAD_DIR"

log "directorio de carga: $LOAD_DIR"
log "iteraciones: $RUNS"

# --- Trampa de limpieza: matar QEMU/python residual si aborta ---
# Solo mata procesos que este script comenzó; no pkill global.
cleanup() {
    if [[ -n "${QEMU_PID:-}" ]] && kill -0 "$QEMU_PID" 2>/dev/null; then
        kill "$QEMU_PID" 2>/dev/null || true
        wait "$QEMU_PID" 2>/dev/null || true
    fi
    if [[ -n "${NETPEER_PID:-}" ]] && kill -0 "$NETPEER_PID" 2>/dev/null; then
        kill "$NETPEER_PID" 2>/dev/null || true
        wait "$NETPEER_PID" 2>/dev/null || true
    fi
}
trap cleanup EXIT

# --- Test criptográfico ligero (una sola vez antes del burn-in) ---
log "ejecutando test criptográfico ligero (cargo test -p meridian-crypto)"
cargo test -p meridian-crypto > "$LOAD_DIR/crypto.log" 2>&1
log "salida criptográfica en $LOAD_DIR/crypto.log"

# --- Bucle de iteraciones ---
PASS_ITER=0
FAIL_ITER=0

for i in $(seq 1 "$RUNS"); do
    log "=== Iteración $i/$RUNS ==="

    # Ejecutar run-alpha.sh con la bandera --no-screenshot si corresponde
    if [[ $NO_SCREENSHOT -eq 1 ]]; then
        "$ROOT/tests/run-alpha.sh" --no-screenshot 2>&1 | tee "$LOAD_DIR/alpha-$i.log"
    else
        "$ROOT/tests/run-alpha.sh" 2>&1 | tee "$LOAD_DIR/alpha-$i.log"
    fi

    # Verificar código de salida 0 (run-alpha.sh imprime y devuelve 1 en fallo)
    if [[ ! -s "$LOAD_DIR/alpha-$i.log" ]]; then
        log "FALTA: iteración $i — log vacío o inexistente"
        FAIL_ITER=$((FAIL_ITER + 1))
        continue
    fi

    # Verificar código de salida 0 del script alpha
    # El último estado se captura en el log: "OK: Alpha QA PASS" o el fallo
    if ! grep -q "OK: Alpha QA PASS" "$LOAD_DIR/alpha-$i.log"; then
        log "FALLA: no se encontró 'OK: Alpha QA PASS' en iteración $i"
        FAIL_ITER=$((FAIL_ITER + 1))
        continue
    fi

    # Verificar "83/83 marcadores"
    if grep -q "83/83 marcadores" "$LOAD_DIR/alpha-$i.log"; then
        log "OK: 83/83 marcadores presentes en iteración $i"
    else
        log "FALTA: falta la línea '83/83 marcadores' en iteración $i"
        FAIL_ITER=$((FAIL_ITER + 1))
        continue
    fi

    # Verificar 6/6 NETPEER y 7/7 TLSCAP PASS revisando los logs correspondientes
    # Los logs net-peer.log y tls-capture.log se ubican en $BUILD_DIR (seteado por run-alpha.sh/run.sh)
    NETPEER_LOG="${BUILD_DIR}/net-peer.log"
    TLSCAP_LOG="${BUILD_DIR}/tls-capture.log"

    netpeer_pass=0
    tlscape_pass=0

    if [[ -f "$NETPEER_LOG" ]] && grep -q "NETPEER: ARP reply PASS" "$NETPEER_LOG" && \
       grep -q "NETPEER: ICMP echo reply PASS" "$NETPEER_LOG" && \
       grep -q "NETPEER: UDP echo reply PASS" "$NETPEER_LOG" && \
       grep -q "NETPEER: TCP SYN-ACK PASS" "$NETPEER_LOG" && \
       grep -q "NETPEER: TCP ACK sent PASS" "$NETPEER_LOG" && \
       grep -q "NETPEER: ICMPv6 echo reply PASS" "$NETPEER_LOG"; then
        netpeer_pass=1
        log "OK: 6/6 NETPEER PASS en $NETPEER_LOG"
    else
        log "FALLA: NETPEER incompleto en $NETPEER_LOG"
    fi

    if [[ -f "$TLSCAP_LOG" ]] && grep -q "TLSCAP: ClientHello observed" "$TLSCAP_LOG" && \
       grep -q "TLSCAP: ServerHello verified" "$TLSCAP_LOG" && \
       grep -q "TLSCAP: Finished verified" "$TLSCAP_LOG" && \
       grep -q "TLSCAP: application ciphertext captured" "$TLSCAP_LOG" && \
       grep -q "TLSCAP: encrypted read allowed PASS" "$TLSCAP_LOG" && \
       grep -q "TLSCAP: encrypted read denied PASS" "$TLSCAP_LOG" && \
       grep -q "TLSCAP: replay rejected PASS" "$TLSCAP_LOG"; then
        tlscape_pass=1
        log "OK: 7/7 TLSCAP PASS en $TLSCAP_LOG"
    else
        log "FALLA: TLSCAP incompleto en $TLSCAP_LOG"
    fi

    if [[ $netpeer_pass -eq 0 ]] || [[ $tlscape_pass -eq 0 ]]; then
        FAIL_ITER=$((FAIL_ITER + 1))
        continue
    fi

    # Verificar que no aparece FAIL del kernel (patrón idéntico al de run-alpha.sh)
    # Esto evita false positives como "timeout: 45s" que es solo configuración.
    if grep -Eq 'A4: .*FAIL|A5: .*FAIL|A6: .*FAIL|A7: .*FAIL|A8: .*FAIL|A9: .*FAIL|T1\.2: .*FAIL|T1\.3: .*FAIL|T1\.5: .*FAIL|T2\.1: .*FAIL|T2\.2: .*FAIL|T2\.3: .*FAIL' "$LOAD_DIR/alpha-$i.log"; then
        log "FALLA: aparece marcador FAIL de kernel en iteración $i"
        FAIL_ITER=$((FAIL_ITER + 1))
        continue
    fi

    PASS_ITER=$((PASS_ITER + 1))
done

# --- Resumen final ---
if [[ $FAIL_ITER -gt 0 ]]; then
    log "T3.1 load QA FAIL ($FAIL_ITER/$RUNS iteraciones fallaron)"
    exit 1
fi

log "T3.1 load QA PASS ($PASS_ITER/$RUNS iteraciones, 83/83 marcadores por iteración)"
exit 0
