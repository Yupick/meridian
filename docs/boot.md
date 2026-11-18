# Arranque UEFI (A2)

Esta fase implementa el cargador mínimo de Meridian:

- aplicación UEFI (`EFI/BOOT/BOOTX64.EFI`) escrita en Rust con la crate `uefi`
  0.41;
- dibujo directo sobre el framebuffer GOP con el mensaje
  `MERIDIAN UEFI BOOTLOADER`;
- lectura de `\kernel.elf` desde la ESP/FAT de la imagen;
- carga de los segmentos `PT_LOAD` del ELF;
- captura del mapa de memoria UEFI al salir de boot services y entrega de una
  estructura `BootInfo` al kernel.

## Estructura

```
bootinfo/   contrato #[repr(C)] compartido entre bootloader y kernel
bootloader/ aplicación UEFI: GOP + FAT + cargador ELF + handoff
kernel/     stub bare-metal x86-64 que valida BootInfo y se detiene
tools/build.sh  compila y genera build/meridian.img
tools/run.sh    arranca OVMF/QEMU y captura pantalla + logs
```

## Construcción y ejecución

Requisitos: los de `tools/check-env.sh` más `sgdisk` y `socat`.

```bash
tools/check-env.sh
tools/build.sh
tools/run.sh
```

`tools/build.sh` genera:

```
build/bootloader.efi
build/kernel.elf
build/meridian.img   # GPT + ESP FAT32 de 64 MiB
```

`tools/run.sh` arranca QEMU con OVMF sin ventana, espera al kernel por COM1 y
guarda:

```
build/serial.log    salida serie de OVMF y del kernel
build/debugcon.log  trazas del bootloader (puerto 0xE9)
build/screen.ppm    captura GOP (screendump)
build/screen.png    la misma captura convertida con Pillow, si está disponible
```

El script devuelve 0 cuando COM1 contiene `MERIDIAN-KERNEL: entered`.

## Handoff

`KERNEL_LOAD_BASE` es `0x0200_0000` (32 MiB). El bootloader reserva ese rango
con `AllocatePages(Address, LOADER_DATA)` y carga el ELF allí porque OVMF deja
un hueco no convencional alrededor de 16 MiB. La constante de
`bootinfo/src/lib.rs` y `kernel/linker.ld` deben permanecer sincronizadas.

El kernel se enlaza como `ET_EXEC` (no PIE) y se invoca con la ABI SysV64:
primer argumento (`*const BootInfo`) en `RDI`. El target UEFI usa la ABI
Windows x64 para `extern "C"`, por lo que el puntero de función se declara
explícitamente como `extern "sysv64"`.

`BootInfo` contiene:

- firma `MERIDIAN` y versión;
- mapa de memoria UEFI (`buffer`, tamaño, tamaño de descriptor, versión);
- información del framebuffer GOP (base, tamaño, resolución, stride, formato);
- base, entrada y tamaño del kernel.

El mapa se obtiene con `ExitBootServices`, así que es el mapa final. Su buffer
y la página de `BootInfo` son memoria `LOADER_DATA` que el kernel no debe
liberar. Se asume el mapeo identidad de memoria física/virtual de UEFI x86-64.
A3 ampliará la estructura si necesita más información, manteniendo la misma
firma y mecanismo de paso.

## Verificación de la pantalla GOP

La captura se puede inspeccionar directamente (`build/screen.png`). Para una
comprobación no visual, el título usa los colores exactos de la paleta del
bootloader. Reconstruyendo la rejilla 8x8 de la fuente se lee:

```
MERIDIAN UEFI BOOTLOADER
```

Además, `build/debugcon.log` debe contener:

```
meridian: pantalla GOP dibujada
meridian: ExitBootServices completado, saltando al kernel
```

## Limitaciones / pendientes para A3

- El kernel todavía no tiene consola completa: solo emite diagnóstico por COM1
  y se detiene con `hlt`.
- No hay paginación propia, GDT/IDT ni pila de kernel; se usa la pila del
  firmware durante el handoff.
- El kernel está enlazado a una dirección física fija y la imagen FAT es GPT
  sencilla (sin variables UEFI persistentes entre ejecuciones).
- A3 debe consumir `BootInfo`, imprimir `Meridian booting` por COM1 y GOP, y
  sustituir el stub por la inicialización real del kernel.
