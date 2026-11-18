# Arranque UEFI, consola, memoria y procesos (A2 + A3 + A4 + A5)

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

A5 añade infraestructura mínima de ejecución del kernel en entorno propio y
procesos con espacios de direcciones aislados:

- GDT/TSS propias, conmutación a una pila de kernel dedicada e IDT propia;
- manejo controlado de excepciones críticas (#PF, #DF, #GP) usando pilas IST;
- creación de espacios de direcciones por proceso (PML4 propia) y conmutación
  de `CR3`;
- auto-prueba de aislamiento: dos procesos de prueba no pueden leer la memoria
  privada del otro y el intento provoca un **page fault controlado** recuperado
  por el kernel.

## Estructura

```
bootinfo/   contrato #[repr(C)] compartido entre bootloader y kernel
bootloader/ aplicación UEFI: GOP + FAT + cargador ELF + handoff
kernel/     bare-metal x86-64: consola, MM, CPU y procesos
  src/main.rs         arranque, init GDT/IDT, pila propia, A3/A4/A5
  src/console.rs      UART 16550, cursor de texto y salida combinada
  src/framebuffer.rs  escritura lineal en GOP (RGB/BGR/bitmask) y fuente 8x8
  src/memory.rs       recorrido y resumen del mapa de memoria UEFI
  src/arch/x86_64/    GDT/TSS, IDT y recuperación de #PF controlado
  src/proc/           auto-pruebas de procesos/aislamiento (A5)
  mm/mod.rs           reservas físicas y símbolos de sección del linker
  mm/buddy.rs         asignador buddy de páginas/bloques y contadores
  mm/paging.rs        tablas 4 niveles, W^X y espacios por proceso
  mm/selftest.rs      pruebas de buddy y W^X con marcadores A4
tools/build.sh  compila y genera build/meridian.img
tools/run.sh    arranca OVMF/QEMU y valida marcadores A3/A4/A5
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

El script devuelve 0 cuando COM1 contiene `Meridian booting`, los marcadores de
A4 y los marcadores de A5; si aparece cualquier `A4: ... FAIL` o `A5: ... FAIL`,
o falta alguna prueba requerida, devuelve 1.

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
`BootInfo` sigue en versión 1. La crate `bootinfo` añade `MemoryDescriptor` y
`MemoryType` para interpretar el buffer del mapa. El stride real
(`descriptor_size`, hoy 48 bytes en OVMF) puede ser mayor que
`size_of::<MemoryDescriptor>()` (40 bytes), así que el kernel recorre los
descriptores con ese stride y copia solo los campos comunes con
`read_unaligned`.

## GDT/TSS/IDT y pila de kernel (A5)

Al entrar en `_start` el kernel ya no confía en la pila de firmware para el
resto de la ejecución:

1. Inicializa una **GDT propia** con segmentos planos de código/datos de kernel
   (64-bit, anillo 0) y un descriptor de **TSS disponible**.
2. Carga la GDT, recarga segmentos y ejecuta `ltr` para activar el TSS.
3. Instala una **IDT propia** con handlers mínimos para `#DF` (vector 8),
   `#GP` (13) y `#PF` (14).
4. Conmuta a una **pila de kernel dedicada** (estática, alineada) y continúa en
   `kernel_main`.

El TSS define `RSP0` y, especialmente, **IST** dedicadas:

- IST1 para `#DF`
- IST2 para `#PF`

Esto evita depender de la pila corriente cuando ocurre un fallo de página
(p.ej. durante pruebas de aislamiento con `CR3` de proceso) y permite un manejo
controlado sin triple fault en el caso esperado de A5.

## Memoria física: asignador buddy (A4)

El asignador se construye recorriendo el mapa con el `descriptor_size` real y
añadiendo **solo** descriptores `Conventional`. Toda memoria no convencional
(runtime, ACPI, MMIO, reservada, LOADER_*, etc.) queda marcada como reservada y
nunca entra en una lista libre. Además se excluyen explícitamente rangos
críticos:

1. zona baja `0..0x10_0000`;
2. imagen completa del kernel (incluido `.bss`, donde viven metadata/tablas);
3. la página de `BootInfo`;
4. el buffer del mapa UEFI;
5. el framebuffer GOP, aunque el firmware lo marque como convencional;
6. región de pila usada en arranque/operación (incluyendo la zona heredada de
   UEFI reservada preventivamente).

Ni el buffer del mapa ni la página de `BootInfo` se liberan: pertenecen al
handoff y siguen siendo legibles mientras el kernel corre.

El buddy gestiona PFNs `0..2^18` (hasta 1 GiB físico) con órdenes `0..10`
(páginas de 4 KiB a bloques de 4 MiB). Cada página tiene metadatos con estado
(`FREE` / `ALLOCATED` / `RESERVED`), orden y enlaces de lista. `alloc(order)`
toma un bloque del orden pedido o divide uno mayor; `free(addr, order)` valida
que todo el bloque estaba asignado y lo fusiona con su buddy mientras ambos
estén libres con el mismo orden (`base ^ (1 << order)`). Las auto-pruebas
comprueban que, tras secuencias de alloc/free, los contadores y el número de
bloques por orden vuelven exactamente a su valor inicial.

## Espacio virtual, W^X y espacios por proceso (A4 + A5)

El kernel crea sus propias tablas de 4 niveles (PML4 → PDPT → PD → PT) con
páginas obtenidas del buddy. Se conserva el mapeo identidad porque el ELF está
enlazado a una dirección física fija y la consola escribe en el framebuffer
físico.

Layout base del kernel:

| Rango | Traducción | Permisos |
|---|---|---|
| `0..4 GiB` (por defecto) | 2 MiB | RW + NX |
| `.text` del kernel | 4 KiB | RO + ejecutable (RX) |
| `.rodata` | 4 KiB | RO + NX |
| `.data` / `.bss` (incluye metadata del buddy/tablas) | 4 KiB | RW + NX |
| `BootInfo` y buffer del mapa | 4 KiB | RO + NX |
| framebuffer GOP y demás MMIO | 2 MiB / 4 KiB | RW + NX |

Los límites de sección los exporta `kernel/linker.ld` como símbolos alineados a
página. Las regiones con permisos propios dividen la entrada de 2 MiB que las
contiene en 512 entradas de 4 KiB. La activación se hace en este orden:
`EFER.NXE=1`, `CR4.PGE=0`, carga de `CR3` con la nueva raíz y `CR0.WP=1`.

### Creación de espacios aislados (A5)

`mm::paging` permite crear espacios adicionales:

- **`create_address_space()`**: asigna un nuevo PML4 y **copia** el subárbol de
  tablas del kernel para `0..4 GiB` (entradas PDPT 0..3 bajo PML4[0]). Las
  tablas intermedias/finales se clonan para que un proceso pueda modificar sus
  mapeos (p.ej. desmapear una página) sin alterar el espacio del kernel ni el
  de otro proceso. Las hojas de 2 MiB compartidas siguen apuntando a los mismos
  marcos físicos subyacentes (mapeo identidad coherente del kernel).
- **`map_page_in(root, va, pa, flags)`**: mapea una página de 4 KiB en una raíz
  concreta (dividiendo páginas grandes si es necesario).
- **`unmap_page_in(root, va)`**: desmapea una página de 4 KiB en una raíz
  concreta (idempotente si ya no está presente).
- **`switch(root)`**: carga `CR3` para conmutar de espacio.

De este modo, cada proceso de prueba tiene su **propia tabla raíz (PML4)** y
mantiene visibles las regiones necesarias para que el kernel siga ejecutándose
(código/datos/BSS, estructuras de MM, consola COM1/GOP, framebuffer, `BootInfo`
y mapa) según la estrategia de copia del mapeo identidad de kernel. Las páginas
privadas de prueba se controlan explícitamente por proceso.

## Aislamiento de procesos y fallo controlado (A5)

La auto-prueba de A5 (`kernel/src/proc/selftest.rs`, integrada en el arranque)
hace lo siguiente:

1. Reserva dos páginas físicas privadas desde el buddy y escribe un secreto
   distinto en cada una (`pa_a`, `pa_b`) desde el espacio kernel.
2. Crea dos espacios de direcciones: **Proceso A** y **Proceso B** (PML4
   distintas).
3. Mapea la página privada propia en cada proceso en una VA de prueba
   (`A: 0x1800_0000 -> pa_a`, `B: 0x1810_0000 -> pa_b`).
4. Aísla explícitamente la memoria del otro proceso desmapeando en cada espacio
   la VA privada ajena y el **alias identidad** de la página física privada del
   otro (que existía por la copia del mapeo kernel). Así, A no puede acceder a
   `pa_b` ni B a `pa_a`.
5. Conmuta a `CR3` de A, verifica que A puede leer su propio secreto, y luego
   intenta leer `pa_b`: esto provoca `#PF`. El handler de `#PF` corre en la IST
   dedicada y, al estar marcado como fallo esperado, usa un mecanismo de
   recuperación (`recovery_call`/`recovery_resume` en `arch/x86_64/idt.rs`) que
   restaura contexto callee-saved, RSP, RIP y CR3 guardados y devuelve el control
   al kernel (que restaura el `CR3` de kernel y continúa). El fallo queda
   registrado (dirección `CR2` y error code).
6. Repite la verificación simétrica para B (acceso propio correcto y lectura de
   `pa_a` bloqueada con `#PF` controlado).
7. Vuelve al espacio de kernel y emite marcadores de éxito.

El manejo es **controlado**: no hay triple fault, reset ni cuelgue; el kernel
continúa, reporta los fallos esperados y finaliza la prueba. Un `#PF` no
esperado se reporta como fallo y detiene el sistema.

## Verificación de A4

`tools/run.sh` exige los marcadores de A4 y falla si aparece cualquier
`A4: ... FAIL`:

```
Meridian booting
A4: buddy init OK (orders 0..10, ...)
A4: reserved (6 ranges): ...
A4: paging OK (PML4=0x..., identity 0..4 GiB, 2 MiB default + 4 KiB per section)
A4: buddy alloc/free tests PASS (90 ops, coalescing exacto, ...)
A4: W^X check PASS (... W+X=0, NXE=1, WP=1)
```

- **Sin fugas:** igualdad de contadores/bloques libres antes y después de
  secuencias de alloc/free.
- **W^X:** auditoría de tablas activas sin páginas `RW+X` y con `EFER.NXE` y
  `CR0.WP` activos.

## Verificación de A5

`tools/run.sh` exige además los marcadores de A5 y falla ante `A5: ... FAIL` o
pruebas faltantes:

```
A5: process creation OK (proc A root=0x..., priv_pa=0x..., va=0x..., proc B root=0x..., priv_pa=0x..., va=0x...)
A5: isolation A cannot read B OK (tried va=0x..., controlled #PF addr=0x... err=0x...)
A5: isolation B cannot read A OK (tried va=0x..., controlled #PF addr=0x... err=0x...)
A5: process isolation test PASS (2 processes, separate PML4, cross-read blocked by controlled #PF, kernel resumed)
```

Esto demuestra que:

- se crearon **dos procesos con PML4/espacio de direcciones propio**;
- cada proceso accede a su propia página privada;
- ningún proceso puede leer la página privada del otro (acceso por dirección
  física identidad de la página ajena desmapeada en su espacio);
- el intento inválido produce `#PF` **capturado y recuperado** de forma
  controlada, y el kernel continúa su ejecución.

## Verificación de la consola

Serie (`build/serial.log`): el kernel escribe el título y un resumen de
diagnóstico. La primera línea del kernel debe ser:

```
Meridian booting
```

Pantalla (`build/screen.png`): el kernel limpia el framebuffer, dibuja
`Meridian booting` en grande y centrado, y debajo muestra las líneas de
diagnóstico en texto 8x8.

`build/debugcon.log` debe contener las trazas del bootloader:

```
meridian: pantalla GOP dibujada
meridian: ExitBootServices completado, saltando al kernel
```

## Limitaciones / pendientes para A6

- No hay scheduler, contexto de ejecución en user mode (ring 3) ni syscalls;
  los "procesos" de A5 son espacios de direcciones de prueba ejercitados desde
  el kernel (conmutación de CR3 + acceso/fallo controlado), no hilos de usuario
  independientes.
- No existe estructura de proceso completa (PCB), carga de ELF de usuario ni
  gestión de ciclo de vida/recursos más allá de la prueba.
- Las tablas clonadas para espacios de proceso y algunas páginas de tablas no
  se liberan de forma exhaustiva (no hay recorrido/ownership completo de page
  tables ni heap); se liberan las páginas de datos privadas de la prueba.
- Las páginas de tabla permanecen RW+NX; no se protegen como solo lectura.
- El espacio es identidad en `0..4 GiB` (sin higher-half kernel separado ni
  layout usuario/kernel definitivo).
- Buddy limitado a 1 GiB físico gestionado y mapeo a 0..4 GiB.
- La consola escribe directamente en el framebuffer físico sin scroll.
- Imagen/kernel enlazados a dirección física fija; imagen FAT GPT sencilla.