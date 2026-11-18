# Arranque UEFI, consola y memoria del kernel (A2 + A3 + A4)

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
kernel también recorre el mapa de memoria UEFI recibido y resume sus regiones.

A4 añade el asignador físico buddy, las tablas de página propias del kernel
(x86-64, 4 niveles) y el espacio virtual con W^X. Todo se comprueba en cada
arranque con auto-pruebas que informan por COM1 con marcadores `A4: ... PASS`.

## Estructura

```
bootinfo/   contrato #[repr(C)] compartido entre bootloader y kernel
bootloader/ aplicación UEFI: GOP + FAT + cargador ELF + handoff
kernel/     bare-metal x86-64: valida BootInfo, consola COM1 + GOP y MM de A4
  src/console.rs      UART 16550, cursor de texto y salida combinada
  src/framebuffer.rs  escritura lineal en GOP (RGB/BGR/bitmask) y fuente 8x8
  src/memory.rs       recorrido y resumen del mapa de memoria UEFI
  mm/mod.rs           reservas físicas y símbolos de sección del linker
  mm/buddy.rs         asignador buddy de páginas/bloques y contadores
  mm/paging.rs        PML4/PDPT/PD/PT, CR3, NXE/CR0.WP y auditoría W^X
  mm/selftest.rs      pruebas de alloc/free y W^X con marcadores A4
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

El script devuelve 0 cuando COM1 contiene `Meridian booting` **y** los dos
marcadores de A4; si aparece cualquier `A4: ... FAIL`, devuelve 1.

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

## Memoria física: asignador buddy (A4)

El asignador se construye recorriendo el mapa con el `descriptor_size` real y
añadiendo **solo** descriptores `Conventional`. Toda memoria no convencional
(runtime, ACPI, MMIO, reservada, LOADER_*, etc.) queda marcada como reservada y
nunca entra en una lista libre. Además se excluyen explícitamente seis rangos:

1. zona baja `0..0x10_0000`;
2. imagen completa del kernel (incluido `.bss`, donde viven la metadata del
   buddy y las tablas);
3. la página de `BootInfo`;
4. el buffer del mapa UEFI;
5. el framebuffer GOP, aunque el firmware lo marque como convencional;
6. 64 KiB por debajo de `RSP` más la página actual (pila heredada de UEFI).

Ni el buffer del mapa ni la página de `BootInfo` se liberan: pertenecen al
bootloader y siguen siendo legibles mientras el kernel corre.

El buddy gestiona PFNs `0..2^18` (hasta 1 GiB físico) con órdenes `0..10`
(páginas de 4 KiB a bloques de 4 MiB). Cada página tiene metadatos con estado
(`FREE` / `ALLOCATED` / `RESERVED`), orden y enlaces de lista. `alloc(order)`
toma un bloque del orden pedido o divide uno mayor; `free(addr, order)` valida
que todo el bloque estaba asignado y lo fusiona con su buddy mientras ambos
estén libres con el mismo orden (`base ^ (1 << order)`). Las auto-pruebas
comprueban que, tras secuencias de alloc/free, los contadores y el número de
bloques por orden vuelven exactamente a su valor inicial.

## Espacio virtual y W^X (A4)

El kernel crea sus propias tablas de 4 niveles (PML4 → PDPT → PD → PT) con
páginas obtenidas del buddy. Se conserva el mapeo identidad porque el ELF está
enlazado a una dirección física fija y la consola escribe en el framebuffer
físico. El layout es:

| Rango | Traducción | Permisos |
|---|---|---|
| `0..4 GiB` (por defecto) | 2 MiB | RW + NX |
| `.text` del kernel | 4 KiB | RO + ejecutable (RX) |
| `.rodata` | 4 KiB | RO + NX |
| `.data` / `.bss` (incluye la metadata del buddy) | 4 KiB | RW + NX |
| `BootInfo` y buffer del mapa | 4 KiB | RO + NX |
| framebuffer GOP y demás MMIO | 2 MiB / 4 KiB | RW + NX |

Los límites de sección los exporta `kernel/linker.ld` como símbolos alineados a
página; no se necesita interpretar el ELF en memoria. Las regiones con

permisos propios dividen la entrada de 2 MiB que las contiene en 512 entradas
de 4 KiB; el resto de esa región mantiene RW+NX. Solo hay una entrada PML4 (la
0), así que no existen alias que puedan romper W^X.

La activación se hace en este orden: `EFER.NXE=1` (para que el bit 63 tenga
efecto), `CR4.PGE=0` (la carga de CR3 invalida también traducciones globales
heredadas de UEFI), carga de `CR3` con la nueva raíz y, ya con las tablas
propias activas, `CR0.WP=1` (las páginas RO también lo son para el kernel). El
código sigue ejecutándose porque su página es RX y su dirección virtual no
cambia; la consola sigue funcionando porque el framebuffer queda RW+NX.

## Verificación de A4

`tools/run.sh` espera los tres marcadores en COM1 y falla si aparece cualquier
`A4: ... FAIL`:

```
Meridian booting
A4: buddy init OK (orders 0..10, ...)
A4: reserved (6 ranges): low 1 MiB, kernel image, BootInfo, memory map, GOP framebuffer, firmware stack
A4: paging OK (PML4=0x..., identity 0..4 GiB, 2 MiB default + 4 KiB per section)
A4: buddy alloc/free tests PASS (90 ops, coalescing exacto, ...)
A4: W^X check PASS (... W+X=0, NXE=1, WP=1)
```

* **Sin fugas:** se comparan `free_pages`, `allocated_pages` y el número de
  bloques libres por orden antes y después de 90 operaciones (bloques de órdenes
  0..5, liberación inversa y permutada, y rechazo de una doble liberación). La
  igualdad exacta demuestra que todo lo reservado volvió a su lista original.
* **W^X:** `paging::audit()` recorre las tablas activas entrada a entrada y
  cuenta las traducciones con `RW=1 && NX=0`; también verifica `EFER.NXE` y
  `CR0.WP`.
  El marcador solo se imprime si el contador es 0 y ambos bits están activos.

## Verificación de la consola

Serie (`build/serial.log`): el kernel escribe el título y un resumen de
diagnóstico. La primera línea del kernel debe ser:

```
Meridian booting
```

Seguida de la versión de `BootInfo`, el modo GOP, el resumen del mapa de
memoria, los datos de la imagen del kernel y las líneas de A4 (buddy, paginación
y resultados de pruebas). Estas últimas se imprimen **después** de cargar CR3,
así que su presencia confirma que la consola y el framebuffer siguen activos
con las tablas propias.

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

## Limitaciones / pendientes para A5

- El kernel sigue usando la pila del firmware: se reserva su zona para que el
  buddy no la reutilice, pero A5 debe crear una pila propia y un anillo 0 con
  GDT/IDT.
- El espacio virtual es identidad y cubre `0..4 GiB`; no hay half-high/kernel
  superior ni espacios por proceso. A5 debe crear tablas aisladas por proceso.
- No hay heap ni asignador de memoria virtual: el buddy entrega páginas y las
  tablas se escriben directamente en ellas gracias al mapeo identidad.
- Las páginas de tabla se dejan RW+NX (conveniente para A5); no se han
  protegido como RO. W^X no se ve afectado porque nunca son ejecutables.
- Se ignoran regiones físicas por encima de 1 GiB en el buddy, y solo se mapean
  `0..4 GiB`. Suficiente para la imagen QEMU de 512 MiB usada en las pruebas.
- La consola escribe directamente en el framebuffer físico sin scroll; si se
  llena la pantalla, el cursor vuelve al principio visible.
- El kernel está enlazado a una dirección física fija y la imagen FAT es GPT
  sencilla (sin variables UEFI persistentes entre ejecuciones).
