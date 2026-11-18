# Meridian

Sistema operativo moderno, flexible y adaptable, para servidor y escritorio,
desarrollado desde cero en Rust.

> Nombre clave del proyecto. El nombre definitivo del sistema está pendiente.

## Estado

Fase 0 (Alpha) completada: arranque UEFI, consola, memoria, procesos, IPC,
sistema de archivos RAM, login y terminal scriptada verificados en QEMU con
`tests/run-alpha.sh`. Ver `docs/alpha.md`.

## Arranque rápido (Alpha)

```bash
tools/check-env.sh
tests/run-alpha.sh
```

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

## Requisitos de desarrollo (Ubuntu 24.04)

```bash
sudo apt update
sudo apt install -y qemu-system-x86 ovmf build-essential git mtools xorriso nasm
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
rustup target add x86_64-unknown-uefi
rustup component add rust-src llvm-tools
```

## Licencia

Meridian se distribuye bajo **GNU AGPL v3.0 o posterior**. Ver `LICENSE` y `NOTICE`.
