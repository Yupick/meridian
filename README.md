# Meridian

Sistema operativo moderno, flexible y adaptable, para servidor y escritorio,
desarrollado desde cero en Rust.

> Nombre clave del proyecto. El nombre definitivo del sistema está pendiente.

## Estado

Fase 0 (Alpha) completada — tag `v0.1.0-alpha`: arranque UEFI, consola,
memoria, procesos, IPC, sistema de archivos RAM, login y terminal scriptada
verificados en QEMU con `tests/run-alpha.sh`. Ver `docs/alpha.md`.

Fase 1 completada — tag `v0.2.0-base`: el bootloader verifica la firma
Ed25519 de `\kernel.elf` antes de cargarlo y aborta con `SECURITY_VIOLATION`
si es inválida o ausente (T1.1, QA con `tests/run-signature.sh`); además
criptografía base, servicio de bloques virtio-blk, objetos en disco con COW y
volumen cifrado. Ver `docs/boot.md`.

Fase 2 completada — tag `v0.3.0-network`: pila de red virtio-net (T2.1),
grupos y ACLs sobre el volumen cifrado (T2.2) y acceso remoto cifrado TLS 1.3
PSK dev-only (T2.3).

Fase 3 (beta de servidor) en curso: T3.1 (estabilización) y T3.3 (imagen
instalable) completados; la documentación de usuario e instalación (T3.2) vive
en `docs/beta.md`. La beta **no es apta para datos sensibles** hasta la
auditoría de la Fase 8.

## Arranque rápido (beta de servidor)

```bash
tools/check-env.sh
tools/build.sh
tests/run-alpha.sh
```

`tests/run-alpha.sh` construye la imagen UEFI firmada, la arranca en
QEMU/OVMF y verifica 83/83 marcadores (76 en COM1 + 7 TLSCAP) con 6/6
`NETPEER`. Guía de instalación y uso: [`docs/beta.md`](docs/beta.md).

## Principios

- **Microkernel:** el núcleo solo gestiona memoria, planificación, IPC y capacidades.
  Sistemas de archivos, drivers y red son procesos de usuario.
- **Rust** en el núcleo (`no_std`), con `unsafe` mínimo y documentado.
- **Capacidades simples:** los procesos solo acceden a los recursos para los
  que reciben una capacidad explícita.
- **Arquitectura:** x86-64 primero; ARM64 después mediante la capa HAL.
- **Arranque:** UEFI. Soporte multi-SO previsto para después de la primera beta.
- **Criptografía propia**, validada con vectores oficiales y auditoría externa
  antes de la versión de producción.

## Estructura

```
bootloader/   Cargador UEFI propio
kernel/       Microkernel (arch, mm, sched, ipc, cap)
hal/          Capa de abstracción de hardware
services/     Servicios de usuario privilegiados (fs, drivers, net)
crypto/       Primitivas criptográficas propias
userland/     Terminal y biblioteca de sistema
toolchain/    Target de Rust y scripts de compilación cruzada
tools/        Creación de imágenes y ejecución en QEMU
tests/        Pruebas unitarias e integración
docs/         Especificaciones internas
```

## Documentación

- [`docs/beta.md`](docs/beta.md) — guía de usuario e instalación de la beta de
  servidor (Fase 3): requisitos, construcción, arranque, imagen instalable y QA.
- [`docs/alpha.md`](docs/alpha.md) — guía de construcción, arranque y terminal
  de la Alpha, con la pila de red, permisos, acceso remoto y QA detallada.
- [`docs/boot.md`](docs/boot.md) — arranque UEFI, handoff `BootInfo` y
  verificación de firma del kernel (T1.1).

## Requisitos de desarrollo (Ubuntu 24.04)

```bash
sudo apt update
sudo apt install -y qemu-system-x86 ovmf build-essential git gdisk dosfstools \
  mtools xorriso nasm socat python3 python3-cryptography
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
rustup target add x86_64-unknown-uefi x86_64-unknown-none
rustup component add rust-src llvm-tools
```

Esto instala QEMU y OVMF, las herramientas de imagen (`sgdisk`, `mtools`,
`xorriso`, `mkfs.fat`), `socat` y `python3-cryptography` (necesario para la QA
de red/TLS), además de Rust con los targets `x86_64-unknown-uefi` y
`x86_64-unknown-none`. Verifica el entorno con `tools/check-env.sh`.

## Licencia

Meridian se distribuye bajo **GNU AGPL v3.0 o posterior**. Ver `LICENSE` y `NOTICE`.
