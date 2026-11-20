# Meridian Alpha — guía de construcción, arranque y terminal

Esta guía cubre la **Alpha (Fase 0, A1–A10)**, la pila de red **T2.1**, los
**usuarios con permisos (T2.2)** y el **acceso remoto cifrado (T2.3)**: cómo
comprobar el entorno, construir la imagen UEFI, arrancarla en QEMU/OVMF y
verificar la terminal scriptada, el tráfico de red real host↔guest, las ACLs
sobre el volumen cifrado y el perfil TLS 1.3 PSK con captura de tráfico.

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

T1.2 (Fase 1) añade el servicio de bloques sobre virtio-blk: `tools/build.sh`
genera `build/blocktest.img` (1 MiB) y `tools/run.sh` lo conecta como segundo
disco virtio-blk en `00:03.0`. Durante el arranque las auto-pruebas leen el
sector 1 del disco de arranque (EFI PART), escriben/leen los sectores 0 y 1 del
disco QA y comprueban que una capacidad sin `WRITE` no puede escribir.

T1.3 (Fase 1) añade el sistema de objetos en disco sobre ese mismo disco QA:
superblocks duplicados con generación y SHA-256, B+ tree de fanout 8
(hasta 64 objetos), extents de hasta 4096 bytes, copy-on-write y verificación
de hashes. Las auto-pruebas formatean el disco, confirman que un commit no
sobrescribe nodos vivos, simulan un corte de energía antes del commit y
comprueban que la corrupción de un dato, un nodo o un superblock se detecta.

T1.5 (Fase 1) añade el volumen cifrado sobre ese sistema de objetos: cada
objeto de usuario viaja en un frame `MRDVOBJ1` autenticado con SHA-256 y se
cifra por completo con XTS-AES-256 antes de escribirse, de modo que el hash del
ObjectStore cubre ciphertext y no plaintext. La clave XTS de 64 bytes se deriva
con Argon2id (2 iteraciones, 1 lane, 8 KiB) a partir de una passphrase; el
objeto 0 guarda la cabecera `MRDVOL01` (versión, parámetros KDF, sal y su
checksum) y el objeto 1 un verificador cifrado que permite rechazar passphrases
incorrectas con `WrongKey`. El tweak es `LE64(object_key) || "MRDVOL01"`: no
incluye el LBA porque el commit COW puede cambiarlo, ni la clave porque el
tweak es público. Las auto-pruebas formatean el volumen, escriben un payload
conocido, verifican que una clave errónea se rechaza, que `raw_read` devuelve
ciphertext distinto del plaintext (y que su corrupción se detecta), y que el
dato sobrevive a un segundo `open` sin formatear.

La passphrase embebida (`meridian-alpha-volume-key-v1`) y la sal determinista
derivada de `sha256(b"meridian-volume-dev-salt-v1")` son **solo de desarrollo y
QA**. En producción la passphrase debe venir de una fuente segura y la sal debe
ser aleatoria por volumen y persistirse en la cabecera.

T2.1 (Fase 2) añade la pila de red sobre virtio-net transicional
(`1af4:1000`, `disable-modern=on`) en PCI `04.0`, con dos colas (RX/TX) de 256
entradas y 4 páginas contiguas cada una. El tráfico es **real host↔guest** por
QEMU usando un backend UDP dgram (sin TAP/root):
`guest 127.0.0.1:55125 <-> host peer 127.0.0.1:55124`. `tools/run.sh` arranca
`tools/net-peer.py` en background antes de QEMU; el peer construye y reenvía
frames Ethernet crudos hasta recibir las respuestas del guest y registra en
`build/net-peer.log` los seis eventos `NETPEER: ... PASS`.

Constantes de red T2.1:

| Rol | MAC | IPv4 | IPv6 |
| --- | --- | --- | --- |
| Guest | `52:54:00:00:00:15` | `10.0.2.15` | `fd00::15` |
| Host/peer | `52:54:00:00:00:02` | `10.0.2.2` | `fd00::2` |

Puertos: TCP `8080` (handshake SYN → SYN-ACK/ISN `0x1000` → ACK) y UDP `9999`
(echo). El selftest procesa ARP, ICMPv4 echo (8→0), UDP echo, handshake TCP e
ICMPv6 echo (128→129) con checksums y pseudo-headers correctos.

T2.2 (Fase 2) añade grupos y ACLs sobre los objetos del volumen cifrado de T1.5.
`services/auth` infiere los grupos del nombre al registrar (`root`:
`ADMIN|USERS`, `alice`: `USERS`, `bob`: `USERS|AUDIT`, `guest`: `GUEST`;
cualquier otro: `USERS`) y los copia a la sesión. `services/perm` define ACLs
estáticas (sin heap) sobre las claves de objeto `10`–`13` y comprueba los
grupos de la sesión **antes** de delegar en `vol::EncryptedVolume`, pasando
siempre las capacidades y el slot del servicio de bloques:

| Objeto | Lectura | Escritura |
| --- | --- | --- |
| `10` `OBJ_SHARED_CONFIG` | `USERS`, `ADMIN` | `ADMIN` |
| `11` `OBJ_AUDIT_LOG` | `AUDIT`, `ADMIN` | `ADMIN` |
| `12` `OBJ_ROOT_SECRET` | `ADMIN` | `ADMIN` |
| `13` `OBJ_PUBLIC_NOTICE` | todos (`GROUP_ALL`) | `ADMIN` |

Los errores son `UnknownObject`, `AccessDenied` y `Volume(VolError)`. Las
auto-pruebas verifican la asignación de grupos, una lectura permitida, una
escritura denegada (con root escribiendo el mismo objeto), el aislamiento entre
grupos y que las ACLs siguen aplicándose tras reabrir el volumen cifrado.
Nunca se imprimen claves, passphrases ni ciphertext. Marcadores T2.2 en
`build/serial.log`:

```
T2.2: group assignment PASS
T2.2: ACL read allowed PASS
T2.2: ACL write denied PASS
T2.2: group isolation PASS
T2.2: encrypted ACL persistence PASS
T2.2: permissions tests PASS
```

T2.3 (Fase 2) añade **acceso remoto cifrado** sobre el mismo backend UDP dgram
de T2.1 (sin NIC adicional), usando el puerto UDP `9443` (`TLS_PORT`). El guest
implementa un perfil **TLS 1.3 PSK propio, dev-only (solo QA)**:

- **PSK dev:** `SHA256(b"meridian-tls13-dev-psk-v1")` (32 bytes). No apto para
  producción; documentado solo para pruebas.
- **Cripto propia:** AES-128-GCM en `crypto/src/gcm.rs` (GHASH + CTR32 sobre
  `meridian_crypto::aes`) y HKDF-SHA256/HMAC-SHA256 en `services/tls/mod.rs`
  sobre `meridian_crypto::sha256`. Sin dependencias criptográficas externas en
  el kernel/guest.
- **Handshake (registros TLS planos 0x16, legacy 0x0303):**
  - `ClientHello` payload: `magic b"MRSTLS13"` (8) | `type=1` | `client_random[32]`
    (host: `sha256(b"meridian-tls-client-random-v1")`) | `psk_id=0`.
  - `ServerHello` payload: `b"MRSTLS13"` | `type=2` | `server_random[32]` (guest:
    `sha256(b"meridian-tls-server-random-v1" || client_random)`) | `psk_id=0` |
    `verify[16]`.
  - `transcript_hash = SHA256(ClientHello.payload || ServerHello.payload[..41])`
    (sin `verify`).
  - `early = HKDF-Extract(zeros32, psk)`;
    `hs = HKDF-Expand(early, b"meridian tls13 handshake" || transcript_hash, 32)`;
    `client_key`/`client_iv`/`server_key`/`server_iv`/`finished_key` con labels
    `b"meridian tls13 c key"`, `b"meridian tls13 c iv"`,
    `b"meridian tls13 s key"`, `b"meridian tls13 s iv"`,
    `b"meridian tls13 finished"` (HKDF-Expand `HMAC(hs, label||info||0x01)`, `info`
    vacío salvo handshake).
  - `verify = HMAC-SHA256(finished_key, transcript_hash)[..16]`.
- **Registros cifrados:** AES-128-GCM, AAD = 5 bytes de cabecera TLS (0x0303),
  nonce = `IV XOR seq` (seq big-endian en últimos 8 bytes), seq separado
  cliente/servidor desde 1, ciphertext = `plaintext||tag16`.
  - `Finished` (content_type=20): plaintext `HMAC(finished_key, b"client finished" || transcript_hash)[..16]`.
  - Aplicación (23): request `b"REQ\0" | user_len u8 | user | key u64 BE | rights u8 (1=READ,2=WRITE) | seq u64 BE`; respuesta `b"RES\0" | status u8 (1=allowed,0=denied) | user_len u8 | user | key u64 BE | value_len u16 BE | value`.
- **Replay:** Finished o request con `seq` ya usada ⇒ respuesta cifrada de alerta
  (content_type=21, plaintext `b"replay"`), sin revelar claves/objetos.
- **Integración T2.2:** el guest resuelve el usuario del request a una sesión
  `auth::Session` precargada (root/alice/guest) y aplica `perm::check` +
  `PermissionedVolume` sobre device 1 (`vol::DEV_PASSPHRASE`) antes de devolver
  el valor. Nunca se envían passwords ni plaintexts de objetos en claro por la
  red.

`tools/net-peer.py` (usa `cryptography` en el host, requerido para QA TLS)
mantiene T2.1 intacto y, tras completarlo, ejecuta el cliente TLS en 9443 y
escribe `build/tls-capture.log` con 7 eventos: ClientHello/ServerHello/Finished
observados/verificados, `application ciphertext captured` (incluye hash corto y
confirma que `remote-notice-v1` no aparece en claro), y lecturas cifradas
autorizada/denegada + replay rechazado. Timeout del peer: 60s.

Marcadores T2.3 en `build/serial.log`:

```
T2.3: TLS 1.3 handshake PASS
T2.3: Finished verified PASS
T2.3: encrypted read allowed PASS
T2.3: encrypted read denied PASS
T2.3: replay rejected PASS
T2.3: remote session tests PASS
```

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

Devuelve `0` cuando el debugcon contiene `meridian: firma de kernel verificada`,
COM1 contiene todos los marcadores `A4`–`A9`, `T1.2`, `T1.3`, `T1.5`, `T2.1`, `T2.2`
y `T2.3`, `build/net-peer.log` contiene los 6 eventos `NETPEER: ... PASS` y
`build/tls-capture.log` contiene los 7 eventos `TLSCAP: ... PASS` (83/83 en QA);
devuelve `1` si falta alguno o aparece un marcador `FAIL`/`TLSCAP: ... FAIL`.
Opciones útiles:

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

Este comando hace todo lo anterior y además verifica explícitamente:

1. que el login se muestra y autentica (A8);
2. que la terminal exige login y ejecuta **todos** los comandos de A9;
3. que el servicio de bloques detecta dos discos, configura la cola, lee,
   escribe y verifica capacidades (T1.2);
4. que el sistema de objetos en disco pasa formato, COW, corte simulado y
   detección de corrupción por checksum (T1.3);
5. que el volumen cifrado pasa formato, rechazo de clave errónea, ciphertext en
   crudo y persistencia tras `open` (T1.5);
6. que la pila de red detecta virtio-net y completa ARP, ping IPv4, UDP echo,
   handshake TCP y ping IPv6 con tráfico real verificado por `tools/net-peer.py`
   (T2.1);
7. que los grupos se asignan correctamente, que una lectura permitida y una
   escritura denegada se comportan según la ACL, que los grupos están aislados
   y que las ACLs persisten en el volumen cifrado tras `open` (T2.2);
8. que el acceso remoto cifrado T2.3 completa handshake TLS 1.3 PSK, Finished,
   lectura cifrada autorizada/denegada y rechazo de replay, y que
   `build/tls-capture.log` registra los 7 eventos `TLSCAP` (tráfico cifrado, sin
   `remote-notice-v1` en claro);
9. que no hay marcadores `A4`–`A9`, `T1.2`, `T1.3`, `T1.5`, `T2.1`, `T2.2` ni
   `T2.3` con `FAIL`, ni `TLSCAP: ... FAIL`.

Salida esperada al final:

```
[alpha] OK: Alpha QA PASS — login + comandos A9 ejecutados en QEMU (83/83 marcadores; 83 esperados)
```

Si falla, imprime los marcadores ausentes y la ruta a `build/serial.log`.
Opciones:

```bash
tests/run-alpha.sh --timeout 60
tests/run-alpha.sh --no-build     # reutiliza build/meridian.img existente
tests/run-alpha.sh --no-screenshot
```

Los logs completos quedan en `build/alpha-build.log` y `build/alpha-run.log`.
El tráfico de red T2.1 queda registrado en `build/net-peer.log` y la captura
TLS T2.3 en `build/tls-capture.log` (requiere `python3-cryptography` en el
host, ya instalado para QA).

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

## 9. Estabilización T3.1

Este apartado documenta la QA de carga T3.1 para validar la reproducibilidad
del arranque UEFI/QEMU, la red UDP dgram y el TLS PSK tras la Fase 2.

### Script `tests/run-load.sh`

Ejecuta múltiples iteraciones de `tests/run-alpha.sh` para detectar flakiness.

**Opciones:**

- `--runs N` — número de iteraciones a ejecutar (default: 5). La variable de
  entorno `MERIDIAN_LOAD_RUNS` define el valor por defecto; `--runs` la
  sobrescribe.
- `--no-screenshot` — si se pasa, cada iteración invoca
  `tests/run-alpha.sh --no-screenshot` para omitir la captura GOP y acelerar
  el proceso.

**Funcionamiento:**

1. Antes del primer burn-in, ejecuta `cargo test -p meridian-crypto` una sola vez
   y guarda la salida en `build/load/crypto.log`.
2. Limpia `build/load/` para empezar con un estado conocido.
3. Por cada iteración *i* (1 a N):
   - ejecuta `tests/run-alpha.sh` (opcionalmente con `--no-screenshot`),
     capturando la salida en `build/load/alpha-<i>.log`;
   - comprueba la línea `83/83 marcadores` en el log;
   - verifica 6/6 eventos `NETPEER` en `build/net-peer.log`;
   - verifica 7/7 eventos `TLSCAP` en `build/tls-capture.log`;
   - falla inmediatamente si aparece un marcador `FAIL` del kernel.
4. Al terminar, imprime el resumen:
   `T3.1 load QA PASS (N/N iteraciones, 83/83 marcadores por iteración)`.

### Salida esperada

```
T3.1 load QA PASS (3/3 iteraciones, 83/83 marcadores por iteración)
```

### Logs generados

- `build/load/crypto.log` — salida de `cargo test -p meridian-crypto`.
- `build/load/alpha-<i>.log` — salida de cada iteración de `run-alpha.sh`.

La limpieza previa de `build/load/` asegura que no queden restos de iteraciones
anteriores. Cada iteración delega en `tests/run-alpha.sh`, que limpia sus propios
procesos QEMU y `net-peer.py`; el script T3.1 no realiza `pkill` global.

## 10. Imagen instalable T3.3

T3.3 separa la construcción de artefactos de la instalación. `tools/build.sh`
sigue generando los artefactos y la imagen de desarrollo; `tools/install-image.sh`
crea **desde cero** una imagen GPT arrancable con el mismo layout (sector 2048,
ESP FAT32 de 64 MiB, typecode `ef00`, nombre `MERIDIAN`, con
`EFI/BOOT/BOOTX64.EFI`, `kernel.elf` y `kernel.sig`). No crea `blocktest.img`:
ese disco QA lo genera `tools/build.sh` y lo usa `tools/run.sh`.

```bash
tools/build.sh
tools/install-image.sh --disk build/meridian-installed.img
tests/run-install.sh
```

`tools/install-image.sh` acepta `--disk RUTA` (obligatorio), `--size-mb N`
(default `96`) y `--artifacts DIR` (default `build`). Falla si falta alguno de
los tres artefactos, rechaza `--disk` vacío, rutas bajo `/dev/` (incluso
canonicalizadas) y targets existentes que no sean archivos regulares: nunca
escribe en dispositivos de bloque reales. Verifica el contenido FAT con `mdir`
antes de terminar e imprime un resumen `[install] ...`.

Para arrancar directamente la imagen instalada:

```bash
tools/run.sh --image build/meridian-installed.img --no-screenshot
tools/run.sh --image build/meridian-installed.img --blocktest build/blocktest.img
```

`--image` sustituye la imagen por defecto (`build/meridian.img`) y `--blocktest`
el disco QA por defecto (`build/blocktest.img`); sin ellos el comportamiento no
cambia. Siguen disponibles `--help`, `--timeout`, `--no-screenshot` y
`--expect-signature-reject`.

`tests/run-install.sh` construye artefactos frescos, instala en
`build/install/meridian-installed.img`, verifica la GPT con `sgdisk` y los tres
archivos con `mdir`, arranca la imagen con `tools/run.sh --image ...
--no-screenshot` y comprueba código `0`, `83/83` marcadores (76 en COM1 y 7
TLSCAP), `6/6 NETPEER` y `7/7 TLSCAP`. Los logs quedan en `build/install/`
(`build.log`, `install.log`, `sgdisk.log`, `mdir.log`, `run.log`, además de los
logs de QEMU, serie y peer).

Salida esperada:

```
T3.3 install QA PASS: imagen instalada desde cero y arrancada en QEMU (83/83 marcadores)
```
