#!/usr/bin/env bash
# tools/install-image.sh — Instala una imagen GPT arrancable de Meridian desde cero.
#
# Usa artefactos ya construidos: bootloader.efi, kernel.elf y kernel.sig.
# La imagen se crea con la misma tabla GPT y ESP FAT32 que tools/build.sh:
#   sector inicial 2048, ESP de 64 MiB, typecode ef00, nombre MERIDIAN,
#   EFI/BOOT/BOOTX64.EFI, kernel.elf y kernel.sig.
#
# NO crea blocktest.img: ese disco QA lo genera tools/build.sh y lo usa
# tools/run.sh.
#
# Seguridad: rechaza --disk vacío, rutas bajo /dev/ (incluso canonicalizadas) y
# targets existentes que no sean archivos regulares (o que sean enlaces). Nunca
# se escribe en dispositivos de bloque reales.
#
# Uso:
#   tools/install-image.sh --disk RUTA [--size-mb N] [--artifacts DIR]
#
# Variables:
#   Ninguna; los valores por defecto se indican en el uso (SIZE_MB=96,
#   ARTIFACTS=<repo>/build).

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

DISK=""
SIZE_MB=96
ARTIFACTS="$ROOT/build"

IMAGE_SIZE_MB=96
PART_SIZE_MB=64
PART_START_SECTOR=2048
PART_SIZE_SECTORS=$((PART_SIZE_MB * 1024 * 1024 / 512))
PART_END_SECTOR=$((PART_START_SECTOR + PART_SIZE_SECTORS - 1))
ESP_OFFSET=$((PART_START_SECTOR * 512))

BOOTLOADER_EFI=""
KERNEL_ELF=""
KERNEL_SIG=""
ESP_IMAGE=""

log()  { printf '[install] %s\n' "$*"; }
fail() { printf '[install] ERROR: %s\n' "$*" >&2; exit 1; }

require() {
    command -v "$1" >/dev/null 2>&1 || fail "falta '$1' en PATH (ver tools/check-env.sh y README)"
}

usage() {
    sed -n '2,/^set -euo pipefail$/p' "${BASH_SOURCE[0]}" | sed '$d' | sed 's/^# \{0,1\}//'
}

cleanup() {
    if [[ -n "$ESP_IMAGE" ]]; then
        rm -f "$ESP_IMAGE"
    fi
}
trap cleanup EXIT

while [[ $# -gt 0 ]]; do
    case "$1" in
        --disk)
            [[ $# -ge 2 ]] || fail "--disk requiere un valor"
            DISK="$2"
            shift 2
            ;;
        --size-mb)
            [[ $# -ge 2 ]] || fail "--size-mb requiere un valor"
            SIZE_MB="$2"
            shift 2
            ;;
        --artifacts)
            [[ $# -ge 2 ]] || fail "--artifacts requiere un valor"
            ARTIFACTS="$2"
            shift 2
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

for tool in dd sgdisk mkfs.fat mmd mcopy mdir mktemp realpath; do
    require "$tool"
done

[[ -n "$DISK" ]] || fail "--disk es obligatorio y no puede estar vacío"
[[ "$SIZE_MB" =~ ^[1-9][0-9]*$ ]] || fail "--size-mb debe ser un entero positivo"
# 1 MiB de offset + 64 MiB de ESP + espacio para la GPT de respaldo.
(( SIZE_MB >= IMAGE_SIZE_MB )) || fail "--size-mb debe ser >= $IMAGE_SIZE_MB"

DISK_DIR="$(dirname -- "$DISK")"
DISK_BASE="$(basename -- "$DISK")"
[[ -n "$DISK_BASE" && "$DISK_BASE" != "." && "$DISK_BASE" != ".." ]] || fail "--disk no apunta a un archivo"

DISK_CANON="$(realpath -m -- "$DISK")"
if [[ "$DISK" == /dev/* || "$DISK_CANON" == /dev/* ]]; then
    fail "rechazado: --disk apunta a /dev/ ($DISK); solo se permiten archivos regulares"
fi
if [[ -L "$DISK" ]]; then
    fail "rechazado: --disk es un enlace simbólico ($DISK)"
fi
if [[ -e "$DISK" && ! -f "$DISK" ]]; then
    fail "rechazado: --disk existe pero no es un archivo regular ($DISK)"
fi

[[ -d "$ARTIFACTS" ]] || fail "no existe el directorio de artefactos $ARTIFACTS"
BOOTLOADER_EFI="$ARTIFACTS/bootloader.efi"
KERNEL_ELF="$ARTIFACTS/kernel.elf"
KERNEL_SIG="$ARTIFACTS/kernel.sig"
for artifact in "$BOOTLOADER_EFI" "$KERNEL_ELF" "$KERNEL_SIG"; do
    [[ -f "$artifact" ]] || fail "falta el artefacto $artifact; ejecuta tools/build.sh primero"
done

mkdir -p "$DISK_DIR"

verify_esp() {
    local image="$1"
    local entry
    for entry in ::/EFI/BOOT/BOOTX64.EFI ::/kernel.elf ::/kernel.sig; do
        if ! mdir -i "$image" "$entry" >/dev/null 2>&1; then
            log "contenido de la ESP ($image):"
            mdir -i "$image" -/ ::/ || true
            fail "la imagen FAT no contiene $entry"
        fi
    done
}

log "repo:       $ROOT"
log "artefactos: $ARTIFACTS"
log "destino:    $DISK"
log "tamaño:     ${SIZE_MB} MiB (ESP ${PART_SIZE_MB} MiB en sector ${PART_START_SECTOR})"

ESP_IMAGE="$(mktemp "$DISK_DIR/.meridian-esp.XXXXXX")"

# El target ya fue validado como archivo regular (o inexistente bajo un directorio normal).
rm -f "$DISK"
log "creando imagen GPT de ${SIZE_MB} MiB desde cero"
dd if=/dev/zero of="$DISK" bs=1M count="$SIZE_MB" status=none
sgdisk \
    --clear \
    --new=1:${PART_START_SECTOR}:${PART_END_SECTOR} \
    --typecode=1:ef00 \
    --change-name=1:"MERIDIAN" \
    "$DISK" >/dev/null

log "formateando ESP FAT32 (${PART_SIZE_MB} MiB)"
dd if=/dev/zero of="$ESP_IMAGE" bs=1M count="$PART_SIZE_MB" status=none
mkfs.fat -F 32 -n MERIDIAN "$ESP_IMAGE" >/dev/null

mmd -i "$ESP_IMAGE" ::/EFI ::/EFI/BOOT
mcopy -i "$ESP_IMAGE" "$BOOTLOADER_EFI" ::/EFI/BOOT/BOOTX64.EFI
mcopy -i "$ESP_IMAGE" "$KERNEL_ELF" ::/kernel.elf
mcopy -i "$ESP_IMAGE" "$KERNEL_SIG" ::/kernel.sig

log "verificando contenido de la ESP antes de insertarla"
verify_esp "$ESP_IMAGE"

log "insertando la ESP en la imagen GPT (offset ${ESP_OFFSET} bytes)"
dd if="$ESP_IMAGE" of="$DISK" bs=1M seek=1 conv=notrunc status=none

log "verificando contenido de la imagen instalada"
verify_esp "${DISK}@@${ESP_OFFSET}"

log "imagen instalada: $DISK (${SIZE_MB} MiB)"
printf '       bootloader: %s (%s bytes)\n' "$BOOTLOADER_EFI" "$(stat -c %s "$BOOTLOADER_EFI")"
printf '       kernel:     %s (%s bytes)\n' "$KERNEL_ELF" "$(stat -c %s "$KERNEL_ELF")"
printf '       firma:      %s (%s bytes)\n' "$KERNEL_SIG" "$(stat -c %s "$KERNEL_SIG")"
printf '       imagen:     %s (%s bytes)\n' "$DISK" "$(stat -c %s "$DISK")"
log "NOTA: blocktest.img no se crea aquí; tools/build.sh lo genera para tools/run.sh"
