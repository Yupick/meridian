# Arranque UEFI y consola del kernel (A2 + A3)

A2 implementa el cargador mínimo de Meridian:

- aplicación UEFI (`EFI/BOOT/BOOTX64.EFI`) escrita en Rust con la crate `uefi`
  0.41;
- dibujo directo sobre el framebuffer GOP con el mensaje
  `MERIDIAN UEFI BOOTLOADER`;
- lectura de `\kernel.elf` desde la ESP/FAT de la imagen;
- carga de los segmentos `PT_LOAD` del ELF;
- captura del mapa de memoria UEFI al salir de boot services y entrega de una
  estructura `BootInfo` al kernel.

A3 implementa la consola del kernel, que usa `BootInfo` para escribir
simultáneamente por COM1 (UART 16550, puerto `0x3F8`) y por el framebuffer GOP.
El mensaje de aceptación es `Meridian booting`, visible en ambos canales. El
kernel también recorre el mapa de memoria UEFI recibido y resume sus regiones;
la gestión completa del mapa queda para A4.

## Estructura

```
bootinfo/   contrato #[repr(C)] compartido entre bootloader y kernel
bootloader/ aplicación UEFI: GOP + FAT + cargador ELF + handoff
kernel/     bare-metal x86-64: valida BootInfo y arranca la consola COM1 + GOP
  src/console.rs      UART 16550, cursor de texto y salida combinada
  src/framebuffer.rs  escritura lineal en GOP (RGB/BGR/bitmask) y fuente 8x8
  src/memory.rs       recorrido y resumen del mapa de memoria UEFI
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

El script devuelve 0 cuando COM1 contiene `Meridian booting`.

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
`BootInfo` sigue en versión 1: A3 no cambia su layout. La crate `bootinfo`
añade el tipo `MemoryDescriptor` y `MemoryType` para interpretar el buffer del
mapa. El stride real (`descriptor_size`, hoy 48 bytes en OVMF) puede ser mayor
que `size_of::<MemoryDescriptor>()` (40 bytes), así que el kernel recorre los
descriptores con ese stride y copia solo los campos comunes con
`read_unaligned`.

## Verificación de la consola

Serie (`build/serial.log`): el kernel escribe el título y un resumen de
diagnóstico. La primera línea del kernel debe ser:

```
Meridian booting
```

Seguida de la versión de `BootInfo`, el modo GOP, el resumen del mapa de
memoria y los datos de la imagen del kernel.

Pantalla (`build/screen.png`): el kernel limpia el framebuffer, dibuja
`Meridian booting` en grande y centrado, y debajo muestra las mismas líneas de
diagnóstico en texto 8x8. La captura se puede inspeccionar directamente. Para
una comprobación no visual, el título usa el color `TITLE` exacto de la paleta
de la consola; reconstruyendo la rejilla 8x8 de la fuente se lee:

```
Meridian booting
```

Además, `build/debugcon.log` debe contener las trazas del bootloader:

```
meridian: pantalla GOP dibujada
meridian: ExitBootServices completado, saltando al kernel
```

## Limitaciones / pendientes para A4

- El kernel no tiene todavía paginación propia, GDT/IDT ni pila de kernel; usa
  la pila del firmware durante el handoff y el mapeo identidad de UEFI.
- La consola escribe directamente en el framebuffer físico sin scroll; si se
  llena la pantalla, el cursor vuelve al principio visible.
- El mapa de memoria solo se resume. A4 debe construir el asignador de páginas
  físicas (buddy) a partir de esos descriptores y crear el espacio virtual del
  kernel con W^X.
- El kernel está enlazado a una dirección física fija y la imagen FAT es GPT
  sencilla (sin variables UEFI persistentes entre ejecuciones).
