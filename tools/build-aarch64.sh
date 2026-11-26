#!/usr/bin/env bash
# tools/build-aarch64.sh — Construye bootloader UEFI, kernel HAL aarch64 y una
# imagen GPT/FAT arrancable en QEMU `virt` con OVMF/AAVMF.
#
# Salida (por defecto build/aarch64):
#   bootloader-aarch64.efi
#   kernel-aarch64.elf
#   kernel.sig
#   kernel-aarch64.bin     imagen raw para arranque directo en QEMU virt
#   bootrom.bin            ROM que salta al kernel raw pasando la DTB en x0
#   meridian-aarch64.img   ESP con EFI/BOOT/BOOTAA64.EFI + kernel.elf + kernel.sig

set -euo pipefail

log()  { printf '[build-aarch64] %s\n' "$*"; }
fail() { printf '[build-aarch64] ERROR: %s\n' "$*" >&2; exit 1; }

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BUILD_DIR="${MERIDIAN_AARCH64_BUILD_DIR:-$ROOT/build/aarch64}"
PROFILE="${CARGO_PROFILE:-release}"

BOOTLOADER_TARGET="aarch64-unknown-uefi"
KERNEL_TARGET="aarch64-unknown-none"

IMAGE="$BUILD_DIR/meridian-aarch64.img"
PART_IMAGE="$BUILD_DIR/esp-part.img"
BOOTLOADER_EFI="$BUILD_DIR/bootloader-aarch64.efi"
KERNEL_ELF="$BUILD_DIR/kernel-aarch64.elf"
KERNEL_SIG="$BUILD_DIR/kernel.sig"
KERNEL_BIN="$BUILD_DIR/kernel-aarch64.bin"
BOOTROM="$BUILD_DIR/bootrom.bin"

IMAGE_SIZE_MB=96
PART_SIZE_MB=64
PART_START_SECTOR=2048
PART_SIZE_SECTORS=$((PART_SIZE_MB * 1024 * 1024 / 512))
PART_END_SECTOR=$((PART_START_SECTOR + PART_SIZE_SECTORS - 1))

for tool in cargo rustc python3 sgdisk mkfs.fat mmd mcopy dd; do
    command -v "$tool" >/dev/null 2>&1 || fail "falta '$tool' en PATH"
done

if [[ "$PROFILE" == "release" ]]; then
    CARGO_FLAGS=(--release)
    PROFILE_DIR=release
else
    CARGO_FLAGS=()
    PROFILE_DIR=debug
fi

log "repo: $ROOT"
log "build: $BUILD_DIR"
log "targets: $BOOTLOADER_TARGET / $KERNEL_TARGET"

log "compilando bootloader UEFI aarch64"
cargo build \
    --manifest-path "$ROOT/Cargo.toml" \
    "${CARGO_FLAGS[@]}" \
    --target "$BOOTLOADER_TARGET" \
    -p meridian-bootloader

log "compilando kernel aarch64"
cargo build \
    --manifest-path "$ROOT/Cargo.toml" \
    "${CARGO_FLAGS[@]}" \
    --target "$KERNEL_TARGET" \
    -p meridian-kernel-aarch64

BOOTLOADER_BUILT="$ROOT/target/$BOOTLOADER_TARGET/$PROFILE_DIR/meridian-bootloader.efi"
KERNEL_BUILT="$ROOT/target/$KERNEL_TARGET/$PROFILE_DIR/meridian-kernel-aarch64"

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
    log "AVISO: usando clave de firma de desarrollo"
fi
cargo run \
    --manifest-path "$ROOT/Cargo.toml" \
    "${CARGO_FLAGS[@]}" \
    -p meridian-crypto \
    --example sign-kernel \
    -- "${SIGN_ARGS[@]}"

KERNEL_LOAD_BASE="$(python3 -c 'import re,sys; s=open(sys.argv[1]).read(); m=re.search(r"cfg\(target_arch = \"aarch64\"\)\]\s*pub const KERNEL_LOAD_BASE: u64 = (0x[0-9a-fA-F_]+)", s); print(m.group(1).replace("_", ""))' "$ROOT/bootinfo/src/lib.rs")"

log "generando imagen raw y ROM de arranque directo"
python3 "$ROOT/tools/elf-to-bin.py" "$KERNEL_ELF" "$KERNEL_BIN"
python3 "$ROOT/tools/aarch64-bootrom.py" "$BOOTROM" "$KERNEL_LOAD_BASE"

log "creando imagen GPT de ${IMAGE_SIZE_MB} MiB"
rm -f "$IMAGE" "$PART_IMAGE"
dd if=/dev/zero of="$IMAGE" bs=1M count="$IMAGE_SIZE_MB" status=none
sgdisk \
    --clear \
    --new=1:${PART_START_SECTOR}:${PART_END_SECTOR} \
    --typecode=1:ef00 \
    --change-name=1:"MERIDIAN-A64" \
    "$IMAGE" >/dev/null

log "formateando ESP FAT32"
dd if=/dev/zero of="$PART_IMAGE" bs=1M count="$PART_SIZE_MB" status=none
mkfs.fat -F 32 -n MERIDIAN "$PART_IMAGE" >/dev/null
mmd -i "$PART_IMAGE" ::/EFI ::/EFI/BOOT
mcopy -i "$PART_IMAGE" "$BOOTLOADER_EFI" ::/EFI/BOOT/BOOTAA64.EFI
mcopy -i "$PART_IMAGE" "$KERNEL_ELF" ::/kernel.elf
mcopy -i "$PART_IMAGE" "$KERNEL_SIG" ::/kernel.sig

if ! mdir -i "$PART_IMAGE" ::/EFI/BOOT/BOOTAA64.EFI >/dev/null 2>&1 ||
   ! mdir -i "$PART_IMAGE" ::/kernel.elf >/dev/null 2>&1 ||
   ! mdir -i "$PART_IMAGE" ::/kernel.sig >/dev/null 2>&1; then
    mdir -i "$PART_IMAGE" -/ ::/ || true
    rm -f "$PART_IMAGE"
    fail "la ESP no contiene BOOTAA64.EFI, kernel.elf y kernel.sig"
fi

log "insertando ESP en la GPT"
dd if="$PART_IMAGE" of="$IMAGE" bs=1M seek=1 conv=notrunc status=none
rm -f "$PART_IMAGE"

log "imagen lista: $IMAGE"
printf '       bootloader: %s (%s bytes)\n' "$BOOTLOADER_EFI" "$(stat -c %s "$BOOTLOADER_EFI")"
printf '       kernel:     %s (%s bytes)\n' "$KERNEL_ELF" "$(stat -c %s "$KERNEL_ELF")"
printf '       firma:      %s (%s bytes)\n' "$KERNEL_SIG" "$(stat -c %s "$KERNEL_SIG")"
printf '       raw:        %s (%s bytes)\n' "$KERNEL_BIN" "$(stat -c %s "$KERNEL_BIN")"
printf '       bootrom:    %s (%s bytes)\n' "$BOOTROM" "$(stat -c %s "$BOOTROM")"
printf '       imagen:     %s (%s bytes)\n' "$IMAGE" "$(stat -c %s "$IMAGE")"
