# Arranque UEFI, consola, memoria, procesos, IPC, FS RAM y login (A2–A8)

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

A6 añade **IPC por puertos** y **capacidades simples** por proceso:

- puertos del kernel con cola FIFO acotada de mensajes de tamaño fijo (sin
  heap), identificados por un `PortId` y propiedad de un proceso lógico;
- tabla de capacidades fija por proceso con slots/handles y derechos `READ`,
  `WRITE`, `SEND` y `EXECUTE`;
- toda operación IPC exige la capacidad y el derecho adecuados desde la tabla
  del proceso que actúa; si faltan, devuelve un error explícito;
- derivación/reducción monotónica de derechos (`derive`/`attenuate`): solo se
  aceptan subconjuntos; cualquier ampliación se rechaza;
- auto-pruebas de envío/recepción, acceso denegado y reducción sin ampliación.

A7 añade un **servicio de archivos y directorios mínimo en memoria (RAM)**:

- objetos tipados `file`/`dir` con atributos (nombre, tipo, tamaño) en tablas
  de tamaño fijo (sin heap), con raíz `/`;
- operaciones: `mkdir`, `rmdir` (solo directorio vacío), `rm` (archivo),
  `ls`/listar, `read`/leer, `write`/escribir (crea o trunca) y `cd` (resuelve
  ruta y devuelve nuevo directorio actual);
- control de acceso por capacidades: las operaciones exigen una capacidad que
  referencie el objeto del servicio FS con `READ` (consultar/navegar) o
  `WRITE` (modificar); una capacidad de solo lectura no permite escribir ni
  alterar la estructura;
- auto-pruebas de cada operación, incluido error controlado al borrar un
  directorio no vacío y denegación de escritura con capacidad read-only.

A8 añade un **servicio de login y usuarios** (alpha):

- usuarios con contraseña almacenada como hash **temporal** (no criptográfico,
  marcado explícitamente como no apto para producción; en la fase 1/T1.4 se
  sustituirá por Argon2id propio validado);
- autenticación: login correcto abre sesión; credenciales incorrectas
  (contraseña errónea o usuario inexistente) se rechazan con el mismo error
  genérico, sin revelar si el usuario existe;
- cada sesión autenticada recibe una tabla de capacidades con acceso `READ|WRITE`
  al sistema de archivos y su directorio personal (`/home/<usuario>`), sobre el
  que puede operar según esas capacidades;
- pantalla de login textual dibujada por la consola (frontend de texto) y
  auto-pruebas integradas de login, rechazo genérico y capacidades sobre home.

## Estructura

```
bootinfo/   contrato #[repr(C)] compartido entre bootloader y kernel
bootloader/ aplicación UEFI: GOP + FAT + cargador ELF + handoff
kernel/     bare-metal x86-64: consola, MM, CPU, procesos, capacidades, IPC, FS y auth
  src/main.rs         arranque, init GDT/IDT, pila propia, A3–A8
  src/console.rs      UART 16550, cursor de texto y salida combinada
  src/framebuffer.rs  escritura lineal en GOP (RGB/BGR/bitmask) y fuente 8x8
  src/memory.rs       recorrido y resumen del mapa de memoria UEFI
  src/arch/x86_64/    GDT/TSS, IDT y recuperación de #PF controlado
  src/proc/           bloque de proceso (id, PML4, tabla de capacidades) y A5
  cap/mod.rs          derechos, capacidades y tabla de slots por proceso
  ipc/mod.rs          puertos, colas de mensajes y operaciones con derechos
  ipc/selftest.rs     pruebas A6 de send/recv, denegación y atenuación
  mm/mod.rs           reservas físicas y símbolos de sección del linker
  mm/buddy.rs         asignador buddy de páginas/bloques y contadores
  mm/paging.rs        tablas 4 niveles, W^X y espacios por proceso
  mm/selftest.rs      pruebas de buddy y W^X con marcadores A4
services/fs/    servicio de archivos en RAM (A7)
  mod.rs              nodos/atributos tipados y operaciones fs con capacidades
  selftest.rs         pruebas A7 de operaciones y control de acceso
services/auth/  servicio de login y usuarios (A8, alpha)
  mod.rs              usuarios, hash temporal, login y sesiones con capacidades/home
  selftest.rs         pruebas A8 de login, rechazo genérico y capacidades sobre home
tools/build.sh  compila y genera build/meridian.img
tools/run.sh    arranca OVMF/QEMU y valida marcadores A3–A8
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
A4, A5, A6, A7 y A8; si aparece cualquier `A4: ... FAIL`, `A5: ... FAIL`,
`A6: ... FAIL`, `A7: ... FAIL` o `A8: ... FAIL`, o falta alguna prueba
requerida, devuelve 1.

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

## IPC por puertos y capacidades (A6)

### Procesos, handles y tabla de capacidades

`src/proc/mod.rs` define un `Process` mínimo con:

- `id: ProcessId` (`u16`), identificador lógico;
- `root: u64`, la raíz PML4 de su espacio de direcciones (A5);
- `caps: CapTable`, la tabla de capacidades del proceso.

La tabla (`cap/mod.rs`) es fija (`MAX_CAPS = 16`) y no usa `alloc`. Cada slot es
un **handle**: o está vacío o contiene una `Capability { object, rights }`. El
proceso nunca accede a un objeto por su id directo; presenta el índice de slot y
el kernel resuelve la capacidad. `grant` inserta una capacidad y
`check(slot, required)` devuelve `RightsDenied` si falta el derecho o `EmptySlot`
si el handle no existe.

Derechos definidos como bits:

| Derecho | Bit | Operación IPC que lo exige |
|---|---|---|
| `READ` | 0 | `recv`: consumir un mensaje de la cola |
| `WRITE` | 1 | `clear_queue`: modificar el estado del puerto |
| `SEND` | 2 | `send`: encolar un mensaje |
| `EXECUTE` | 3 | `invoke`: consultar/invocar el puerto |

Los errores son valores (`CapError`/`IpcError`), nunca pánicos. Así un proceso sin
capacidad recibe `IpcError::Capability(EmptySlot)` y uno con derechos
insuficientes recibe `IpcError::Capability(RightsDenied)`.

### Puertos y mensajes

`ipc/mod.rs` mantiene una tabla global fija de `MAX_PORTS = 8` puertos. Cada
puerto tiene:

- un `PortId` (índice estable, compartido con `ObjectId`);
- un `ProcessId` propietario (informativo);
- una cola circular FIFO de `QUEUE_DEPTH = 4` mensajes.

Un `Message` es `Copy`, determinista y acotado: `tag: u64`, longitud y payload de
hasta `MESSAGE_MAX_BYTES = 32` bytes. `Message::with_bytes` rechaza payloads
mayores. `send` copia el mensaje a la cola; `recv` lo extrae. Los errores
`QueueFull`/`QueueEmpty`/`NoSuchPort`/`NoPorts` son explícitos. En A6 no se
destruyen puertos para que una capacidad nunca apunte a un objeto reciclado.

El estado global vive en un `UnsafeCell` con `unsafe impl Sync` documentado (el
kernel es mono-CPU y arranca con interrupciones deshabilitadas), el mismo patrón
que el buddy de A4.

### Derivación y reducción monotónica

`CapTable::derive(from, to, rights)` crea una capacidad nueva en `to` con el
mismo objeto y un subconjunto de los derechos de `from`.
`CapTable::attenuate(slot, rights)` reduce los derechos de un slot existente.
Ambas comprueban `requested.is_subset_of(current)`; si se pide cualquier derecho
no presente devuelven `CapError::RightsEscalation`. No existe ninguna operación
que añada derechos. Derivar tampoco modifica la capacidad origen, y el slot
destino debe estar vacío.

### Prueba integrada

`ipc/selftest.rs` crea dos procesos lógicos con PML4 propia (A5), un puerto
propiedad de B y estas capacidades: A con todos los derechos y B solo con
`READ`. Conmuta `CR3` al espacio del proceso que actúa, igual que la prueba de
aislamiento, y verifica envío/recepción, denegación y atenuación. Se ejecuta
después de A5 desde `kernel_main`.

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

## Verificación de A6

`tools/run.sh` exige los marcadores de A6 y falla ante `A6: ... FAIL` o pruebas
faltantes. La salida esperada por COM1 incluye:

```
A6: ipc port created (id=0, owner=B, A[0] rights=rwsx, B[0] rights=r---)
A6: ipc send/recv test PASS (A root=0x... -> port 0 -> B root=0x..., tag=0x..., 16 bytes)
A6: capability denied without rights PASS (B[0] rights=r---: send/clear/invoke -> RightsDenied, slot 9 -> EmptySlot)
A6: capability reduction/attenuation PASS (slot0 rwsx -> slot1 r-s- derivado y luego atenuado a r---; EXECUTE ampliado rechazado, ampliacion in-place rechazada, origen intacto)
A6: ipc/capabilities tests PASS (send/recv through port, denial without rights, monotonic attenuation)
```

Esto demuestra los tres criterios:

1. **Envío por puerto:** A, ejecutando con su PML4 y su tabla de capacidades,
   envía un mensaje con tag y payload conocidos; B lo recibe íntegro desde la
   cola del mismo puerto.
2. **Error sin capacidad:** B solo tiene `READ`, así que `send`, `clear_queue` e
   `invoke` devuelven `RightsDenied`; usar un slot vacío devuelve `EmptySlot`.
   No hay pánico ni éxito silencioso.
3. **Reducción sin ampliación:** se deriva una capacidad `r-s-` desde `rwsx`, se
   usa para enviar y luego se atenúa a `r---`. Derivar añadiendo `EXECUTE` o
   atenuar a `rwsx` se rechaza con `RightsEscalation`, y la capacidad origen
   permanece `rwsx`.

## Verificación de A7

`tools/run.sh` exige los marcadores de A7 y falla ante `A7: ... FAIL` o pruebas
faltantes. La salida esperada por COM1 incluye:

```
A7: fs operations tests PASS (mkdir, rmdir-vacio, rm, listar, leer, escribir, cd; rmdir-no-vacio -> DirectoryNotEmpty)
A7: read-only capability denied write PASS (write/mkdir/rm/rmdir -> RightsDenied; read/ls/cd permitidos)
A7: fs service tests PASS (ops completas, atributos tipados, rmdir-no-vacio controlado, read-only sin escritura)
```

Esto demuestra los criterios de aceptación:

1. **Operaciones y atributos tipados:** se crean/listan directorios y archivos,
   se escribe/lee contenido, se navega con `cd` y se eliminan archivo y
   directorio vacío; `ls` expone nombre, tipo (`file`/`dir`) y tamaño.
2. **Error controlado:** `rmdir` sobre un directorio no vacío devuelve
   `DirectoryNotEmpty` sin modificarlo.
3. **Control por capacidades:** con una capacidad de solo `READ` sobre el
   objeto FS, `write`/`mkdir`/`rm`/`rmdir` devuelven `RightsDenied`, mientras
   que `read`/`ls`/`cd` siguen permitidos.

## Verificación de A8

`tools/run.sh` exige los marcadores de A8 y falla ante `A8: ... FAIL` o pruebas
faltantes. La salida esperada por COM1 incluye:

```
=== Meridian login ===
usuario: _
contrasena: _
(alpha: hash temporal, no apto para produccion)
A8: login screen shown
A8: login correcto abre sesion (user=alice, home=/home/alice)
A8: invalid credentials rejected without user disclosure (wrong-pass == unknown-user == InvalidCredentials)
A8: session has capabilities over personal directory (RW en /home/alice: write/read/mkdir ok; cap sin WRITE denegada)
A8: login/users tests PASS (login ok, rechazo generico, capacidades sobre home)
```

Esto demuestra los criterios de aceptación:

1. **Autenticación:** el login correcto abre sesión; una contraseña incorrecta
   y un usuario inexistente se rechazan con el mismo error genérico
   (`InvalidCredentials`), sin revelar si el usuario existe.
2. **Capacidades de sesión:** tras el login, la sesión recibe capacidades
   `READ|WRITE` sobre el sistema de archivos y opera sobre su directorio
   personal (`/home/<usuario>`: crear/escribir/leer). Una capacidad sin `WRITE`
   no permite modificarlo.

El hash de contraseña es **temporal y no apto para producción** (sustituible
por Argon2id propio validado en la fase 1, T1.4).

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

## Limitaciones / pendientes para A9

- El login/almacén de usuarios es en memoria (sin persistencia), con hash
  temporal **no apto para producción** (pendiente Argon2id propio en T1.4).
  No hay entrada interactiva real de credenciales ni gestión de usuarios/grupos;
  la "pantalla de login" y la autenticación se ejercitan en auto-pruebas.
- No existe aún la terminal/shell interactiva (A9) ni comandos de usuario;
  tampoco gate de acceso a terminal basado en sesión.
- El sistema de archivos es volátil (RAM), de tamaño/capacidad fija y sin
  persistencia, permisos por usuario ni cuotas; vive integrado en el kernel
  (módulo `services/fs`) sin interfaz de servicio/IPC expuesta a procesos de
  usuario.
- No hay scheduler, contexto de ejecución en user mode (ring 3) ni syscalls;
  los "procesos" de A5/A6/A7/A8 son estructuras lógicas con PML4 y tabla de
  capacidades ejercitadas desde el kernel con conmutación de `CR3`, no hilos de
  usuario independientes.
- La tabla de procesos no es global: las auto-pruebas mantienen los `Process`
  en la pila del kernel. Hacen falta registro global, planificación y
  bloqueo/desbloqueo en `send`/`recv`.
- No existe PCB completo, carga de ELF de usuario ni ciclo de vida/recursos
  (cierre de puertos, revocación de capacidades, herencia entre procesos).
- `send`/`recv` son no bloqueantes (devuelven `QueueFull`/`QueueEmpty`); no hay
  espera ni notificación. Los puertos no se destruyen ni se reciclan (faltan
  generaciones/refcounts).
- Las tablas clonadas/espacios de proceso y algunas páginas de tablas no se
  liberan de forma exhaustiva (no hay recorrido/ownership completo de page
  tables ni heap).
- Las páginas de tabla permanecen RW+NX; no se protegen como solo lectura. El
  espacio es identidad en `0..4 GiB` (sin higher-half kernel separado ni layout
  usuario/kernel definitivo).
- Buddy limitado a 1 GiB físico gestionado y mapeo a 0..4 GiB.
- La consola escribe directamente en el framebuffer físico sin scroll.
- Imagen/kernel enlazados a dirección física fija; imagen FAT GPT sencilla.