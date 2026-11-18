#!/usr/bin/env bash
# tools/build.sh — Construye el bootloader UEFI, el kernel y una imagen FAT
# arrancable con OVMF (tarea A2).
#
# Salida (por defecto):
#   build/bootloader.efi        copia del binario UEFI
#   build/kernel.elf            copia de la imagen ELF del kernel
#   build/meridian.img          imagen GPT con ESP FAT32:
#                                 EFI/BOOT/BOOTX64.EFI
#                                 kernel.elf
#                                 kernel.sig
#
# Variables:
#   MERIDIAN_BUILD_DIR          directorio de salida (por defecto: <repo>/build)
#   CARGO_PROFILE               dev|release (por defecto: release)
#   MERIDIAN_KERNEL_SIGNING_KEY clave Ed25519 privada en hexadecimal (64 bytes).
#                               Si no se define se usa una clave de desarrollo.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BUILD_DIR="${MERIDIAN_BUILD_DIR:-$ROOT/build}"
PROFILE="${CARGO_PROFILE:-release}"
BOOTLOADER_TARGET="x86_64-unknown-uefi"
KERNEL_TARGET="x86_64-unknown-none"

IMAGE="$BUILD_DIR/meridian.img"
PART_IMAGE="$BUILD_DIR/esp-part.img"
BOOTLOADER_EFI="$BUILD_DIR/bootloader.efi"
KERNEL_ELF="$BUILD_DIR/kernel.elf"
KERNEL_SIG="$BUILD_DIR/kernel.sig"

# Imagen de 96 MiB; la ESP empieza en el sector 2048 (1 MiB) y ocupa 64 MiB.
# Se deja el resto libre para la GPT de respaldo y futuras particiones.
IMAGE_SIZE_MB=96
PART_SIZE_MB=64
PART_START_SECTOR=2048
PART_SIZE_SECTORS=$((PART_SIZE_MB * 1024 * 1024 / 512))
PART_END_SECTOR=$((PART_START_SECTOR + PART_SIZE_SECTORS - 1))

log()  { printf '[build] %s\n' "$*"; }
fail() { printf '[build] ERROR: %s\n' "$*" >&2; exit 1; }

require() {
    command -v "$1" >/dev/null 2>&1 || fail "falta '$1' en PATH (ver tools/check-env.sh y README)"
}

for tool in cargo rustc sgdisk mkfs.fat mmd mcopy dd; do
    require "$tool"
done

log "repo: $ROOT"
log "build: $BUILD_DIR"

if [[ "$PROFILE" == "release" ]]; then
    CARGO_FLAGS=(--release)
else
    CARGO_FLAGS=()
fi

log "compilando bootloader ($BOOTLOADER_TARGET, perfil $PROFILE)"
cargo build \
    --manifest-path "$ROOT/Cargo.toml" \
    "${CARGO_FLAGS[@]}" \
    --target "$BOOTLOADER_TARGET" \
    -p meridian-bootloader

log "compilando kernel ($KERNEL_TARGET, perfil $PROFILE)"
cargo build \
    --manifest-path "$ROOT/Cargo.toml" \
    "${CARGO_FLAGS[@]}" \
    --target "$KERNEL_TARGET" \
    -p meridian-kernel

if [[ "$PROFILE" == "dev" ]]; then
    PROFILE_DIR="debug"
else
    PROFILE_DIR="$PROFILE"
fi
BOOTLOADER_BUILT="$ROOT/target/$BOOTLOADER_TARGET/$PROFILE_DIR/meridian-bootloader.efi"
KERNEL_BUILT="$ROOT/target/$KERNEL_TARGET/$PROFILE_DIR/meridian-kernel"

[[ -f "$BOOTLOADER_BUILT" ]] || fail "no se generó $BOOTLOADER_BUILT"
[[ -f "$KERNEL_BUILT" ]] || fail "no se generó $KERNEL_BUILT"

mkdir -p "$BUILD_DIR"
cp "$BOOTLOADER_BUILT" "$BOOTLOADER_EFI"
cp "$KERNEL_BUILT" "$KERNEL_ELF"

log "firmando kernel con Ed25519"
SIGN_ARGS=("$KERNEL_ELF" "$KERNEL_SIG")
if [[ -n "${MERIDIAN_KERNEL_SIGNING_KEY:-}" ]]; then
    SIGN_ARGS+=("$MERIDIAN_KERNEL_SIGNING_KEY")
else
    log "AVISO: usando clave de firma de desarrollo (define MERIDIAN_KERNEL_SIGNING_KEY en producción)"
fi
cargo run \
    --manifest-path "$ROOT/Cargo.toml" \
    "${CARGO_FLAGS[@]}" \
    -p meridian-crypto \
    --example sign-kernel \
    -- "${SIGN_ARGS[@]}"

log "creando imagen GPT de ${IMAGE_SIZE_MB} MiB"
rm -f "$IMAGE" "$PART_IMAGE"
dd if=/dev/zero of="$IMAGE" bs=1M count="$IMAGE_SIZE_MB" status=none
sgdisk \
    --clear \
    --new=1:${PART_START_SECTOR}:${PART_END_SECTOR} \
    --typecode=1:ef00 \
    --change-name=1:"MERIDIAN" \
    "$IMAGE" >/dev/null

log "formateando ESP FAT32 (${PART_SIZE_MB} MiB)"
dd if=/dev/zero of="$PART_IMAGE" bs=1M count="$PART_SIZE_MB" status=none
mkfs.fat -F 32 -n MERIDIAN "$PART_IMAGE" >/dev/null

mmd -i "$PART_IMAGE" ::/EFI ::/EFI/BOOT
mcopy -i "$PART_IMAGE" "$BOOTLOADER_EFI" ::/EFI/BOOT/BOOTX64.EFI
mcopy -i "$PART_IMAGE" "$KERNEL_ELF" ::/kernel.elf
mcopy -i "$PART_IMAGE" "$KERNEL_SIG" ::/kernel.sig

# Comprobación de la estructura FAT generada (antes de insertarla en la GPT).
if ! mdir -i "$PART_IMAGE" ::/EFI/BOOT/BOOTX64.EFI >/dev/null 2>&1 ||
   ! mdir -i "$PART_IMAGE" ::/kernel.elf >/dev/null 2>&1 ||
   ! mdir -i "$PART_IMAGE" ::/kernel.sig >/dev/null 2>&1; then
    log "contenido de la ESP:"
    mdir -i "$PART_IMAGE" -/ ::/ || true
    rm -f "$PART_IMAGE"
    fail "la imagen FAT no contiene EFI/BOOT/BOOTX64.EFI, kernel.elf y kernel.sig"
fi

log "insertando la ESP en la imagen GPT (offset $((PART_START_SECTOR * 512)) bytes)"
dd if="$PART_IMAGE" of="$IMAGE" bs=1M seek=1 conv=notrunc status=none
rm -f "$PART_IMAGE"

log "imagen lista: $IMAGE"
printf '       bootloader: %s (%s bytes)\n' "$BOOTLOADER_EFI" "$(stat -c %s "$BOOTLOADER_EFI")"
printf '       kernel:     %s (%s bytes)\n' "$KERNEL_ELF" "$(stat -c %s "$KERNEL_ELF")"
printf '       firma:      %s (%s bytes)\n' "$KERNEL_SIG" "$(stat -c %s "$KERNEL_SIG")"
printf '       imagen:     %s (%s bytes)\n' "$IMAGE" "$(stat -c %s "$IMAGE")"
