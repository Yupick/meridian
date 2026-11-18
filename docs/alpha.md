# Meridian Alpha — guía de construcción, arranque y terminal

Esta guía cubre la **Alpha (Fase 0, A1–A10)**: cómo comprobar el entorno,
construir la imagen UEFI, arrancarla en QEMU/OVMF y ejecutar/verificar la
terminal scriptada.

## 1. Alcance de la Alpha

La Alpha incluye:

- bootloader UEFI propio y entrega de `BootInfo` al kernel (A2);
- consola COM1 + framebuffer GOP (A3);
- asignador buddy, paginación 4 niveles y W^X (A4);
- GDT/TSS/IDT y aislamiento de procesos con `#PF` controlado (A5);
- IPC por puertos y capacidades con reducción monotónica (A6);
- sistema de archivos RAM con capacidades (A7);
- login/usuarios en memoria con hash temporal **no apto para producción** (A8);
- terminal alpha que exige login y ejecuta comandos scriptados (A9);
- QA de integración en un único comando y esta guía (A10).

Además, desde T1.1 (Fase 1, ver `docs/boot.md`), `tools/build.sh` firma el
kernel con Ed25519 (`build/kernel.sig`, copiado a la ESP como `\kernel.sig`) y
el bootloader verifica esa firma antes de cargar el ELF; esta guía refleja ese
comportamiento en construcción, arranque y QA.

No hay entrada interactiva real de teclado ni REPL de usuario en tiempo de
ejecución: la pantalla de login y la terminal se **ejercitan mediante
auto-pruebas scriptadas durante el arranque**. Esto se documenta en detalle en
`docs/boot.md`.

## 2. Requisitos

En Ubuntu 24.04:

```bash
tools/check-env.sh
```

Necesitas, como mínimo, Rust con los targets `x86_64-unknown-uefi` y
`x86_64-unknown-none`, QEMU, OVMF, `sgdisk`, `mtools`/`mkfs.fat` y `socat`.
El script muestra qué falta.

Variables opcionales:

| Variable | Significado | Valor por defecto |
| --- | --- | --- |
| `MERIDIAN_BUILD_DIR` | directorio de salida | `<repo>/build` |
| `MERIDIAN_TIMEOUT` | timeout de QEMU | `45` s |
| `MERIDIAN_MEMORY` | memoria de la VM | `512M` |
| `MERIDIAN_QEMU_DISPLAY` | opción `-display` de QEMU | `none` |
| `CARGO_PROFILE` | perfil de Cargo | `release` |
| `MERIDIAN_KERNEL_SIGNING_KEY` | clave privada Ed25519 (hex, 64 bytes) con la que se firma `kernel.elf` | clave de desarrollo (solo desarrollo) |

## 3. Construcción

```bash
tools/build.sh
```

Genera:

```
build/bootloader.efi     # aplicación UEFI
build/kernel.elf         # kernel bare-metal
build/kernel.sig         # firma Ed25519 de kernel.elf (64 bytes en bruto)
build/meridian.img       # GPT + ESP FAT32 con EFI/BOOT/BOOTX64.EFI, kernel.elf y kernel.sig
```

La firma se calcula sobre el contenido íntegro de `kernel.elf` con la clave
privada de `MERIDIAN_KERNEL_SIGNING_KEY` o, si no está definida, con la clave
de desarrollo (RFC 8032 TEST 1, solo para desarrollo). El bootloader verifica
`\kernel.sig` antes de cargar el ELF y aborta con `SECURITY_VIOLATION` si es
inválida o ausente (T1.1, ver `docs/boot.md`).

## 4. Arranque

```bash
tools/run.sh
```

`tools/run.sh` arranca QEMU con OVMF, captura la salida del kernel por COM1 y
guarda:

```
build/serial.log     # salida serie de OVMF y del kernel
build/debugcon.log   # trazas del bootloader
build/qemu.log       # log de QEMU
build/screen.ppm     # captura GOP
build/screen.png     # conversión PNG si Pillow está disponible
```

Devuelve `0` cuando el debugcon contiene `meridian: firma de kernel verificada`
y COM1 contiene todos los marcadores `A4`–`A9`; devuelve `1` si falta alguno o
aparece un marcador `FAIL`. Opciones útiles:

```bash
tools/run.sh --timeout 60
tools/run.sh --no-screenshot
tools/run.sh --expect-signature-reject   # exige rechazo de firma (QA T1.1)
MERIDIAN_QEMU_DISPLAY=gtk tools/run.sh
```

## 5. QA de Alpha (un solo comando)

```bash
tests/run-alpha.sh
```

Este comando hace todo lo anterior y además verifica explícitamente por COM1:

1. que el login se muestra y autentica (A8);
2. que la terminal exige login y ejecuta **todos** los comandos de A9;
3. que no hay marcadores `A4`–`A9` con `FAIL`.

Salida esperada al final:

```
[alpha] OK: Alpha QA PASS — login + comandos A9 ejecutados en QEMU (40/40 marcadores; 40 esperados)
```

Si falla, imprime los marcadores ausentes y la ruta a `build/serial.log`.
Opciones:

```bash
tests/run-alpha.sh --timeout 60
tests/run-alpha.sh --no-build     # reutiliza build/meridian.img existente
tests/run-alpha.sh --no-screenshot
```

Los logs completos quedan en `build/alpha-build.log` y `build/alpha-run.log`.

QA adicional de la firma del kernel (T1.1, fuera del alcance de la Alpha):

```bash
tests/run-signature.sh
```

Construye una imagen aislada en `build-signature-test`, corrompe `\kernel.sig`
dentro de la ESP y usa `tools/run.sh --expect-signature-reject`; pasa solo si
el bootloader aborta con un marcador de rechazo y en COM1 **no** aparece
`Meridian booting`. Ver `docs/boot.md`.

## 6. Uso de la terminal (demo scriptada)

Tras el arranque, la salida serie muestra la pantalla de login:

```
=== Meridian login ===
usuario: _
contrasena: _
(alpha: hash temporal, no apto para produccion)
```

Las auto-pruebas usan estas credenciales de demostración:

| Usuario | Contraseña | Uso |
| --- | --- | --- |
| `alice` | `secret` | prueba de login A8 y capacidades sobre `/home/alice` |
| `root` | `meridian` | sesión desde la que se ejecuta la terminal A9 |

La terminal se inicia solo con sesión válida. Sin login, la salida incluye:

```
A9: terminal access requires login
terminal: acceso denegado (login requerido)
A9: access without login denied
```

Con sesión válida, se ejecutan estos comandos y se verifica cada resultado:

| Comando | Efecto |
| --- | --- |
| `help` | lista los comandos disponibles |
| `pwd` | muestra el directorio actual (`/home/root`) |
| `mkdir docs`, `md tmp` | crean directorios (`md` es alias de `mkdir`) |
| `ls` | lista entradas con nombre, tipo y tamaño |
| `cd docs`, `cd ..` | cambian de directorio |
| `echo hola-meridian > nota.txt` | escribe un archivo |
| `cat nota.txt` | muestra el contenido |
| `rmdir docs` | falla con error claro si el directorio no está vacío |
| `rm docs/nota.txt` | elimina un archivo |
| `rmdir docs`, `rd tmp` | eliminan directorios vacíos (`rd` es alias de `rmdir`) |
| `mkdir arbol`, `echo ...`, `rm -r arbol` | crean y eliminan un árbol recursivamente |
| `clear` | limpia consola serie y framebuffer |
| `exit` | cierra la sesión |

Marcadores representativos en `build/serial.log`:

```
A9: login ok, terminal session started (user=root)
A9: help PASS
A9: mkdir/md PASS
A9: echo > PASS
A9: cat PASS
rmdir: error: el directorio no esta vacio: 'docs'
A9: rmdir non-empty shows clear error PASS
A9: rm -r PASS
A9: command script PASS (ls,cd,pwd,mkdir,md,rmdir,rd,rm,rm -r,cat,echo>,help,clear,exit)
A9: terminal tests PASS (scripted commands, rmdir non-empty error, login required)
```

Recuerda que la FS es volátil: todo lo creado por la demo se pierde al apagar
la VM.

## 7. Solución de problemas

- **`tools/run.sh` dice que falta qemu/socat/OVMF:** ejecuta
  `tools/check-env.sh` e instala lo que indique.
- **La imagen no existe:** ejecuta `tools/build.sh` antes que `tools/run.sh`, o
  usa directamente `tests/run-alpha.sh`.
- **Falta un marcador:** abre `build/serial.log`; los mensajes `FAIL` indican
  qué auto-prueba falló. `build/qemu.log` contiene errores de QEMU.
- **Falta el marcador de firma:** `build/debugcon.log` debe contener
  `meridian: firma de kernel verificada`. Si aparece
  `meridian: firma de kernel inválida; abortando` o
  `meridian: firma de kernel ausente; abortando`, comprueba que `\kernel.sig`
  corresponde a `\kernel.elf` y a la clave pública embebida en el bootloader.
- **Timeout:** usa `MERIDIAN_TIMEOUT=90 tests/run-alpha.sh` en máquinas lentas.
- **Pantalla en blanco en `screen.png`:** comprueba que el firmware encontró
  GOP en `build/debugcon.log` y que la resolución seleccionada es válida.

## 8. Cierre de la Alpha

La Alpha se considera cerrada cuando:

- `tests/run-alpha.sh` termina con código `0` sin marcadores `FAIL`;
- los PR de A1–A10 están fusionados en `develop`;
- el plan `.opencode/state/plan.md` marca todas las tareas `A*` como
  `completed`;
- se abre el PR `develop -> main` y se etiqueta la versión de cierre de la Alpha
  siguiendo las reglas de GitFlow del plan.
