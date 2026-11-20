# Meridian Beta de servidor — guía de usuario e instalación

Esta guía cubre la **beta de servidor (Fase 3)**: cómo preparar el entorno,
construir la imagen UEFI firmada, arrancarla en QEMU/OVMF, instalarla en una
imagen de disco desde cero, ejecutar la QA completa y usar la consola y el
acceso remoto cifrado que ya existen. La Alpha (Fase 0) y el detalle técnico de
las fases 1 y 2 se documentan en [`docs/alpha.md`](alpha.md) y
[`docs/boot.md`](boot.md); aquí se repite solo lo necesario para instalar y
verificar la beta.

> **La beta no es apta para datos sensibles** hasta completar la auditoría
> externa de la Fase 8. Ver «Limitaciones y seguridad».

## 1. Qué incluye la beta

La beta de servidor reúne el alcance de las fases A, 1, 2 y 3:

- **Arranque UEFI firmado:** bootloader propio que verifica la firma Ed25519
  de `\kernel.elf` antes de cargarlo y aborta con `SECURITY_VIOLATION` si es
  inválida o ausente (T1.1; ver `docs/boot.md`).
- **Particiones GPT/ESP:** imágenes GPT con una ESP FAT32 (inicio en el
  sector 2048, 64 MiB, typecode `ef00`, nombre `MERIDIAN`) que contiene
  `EFI/BOOT/BOOTX64.EFI`, `kernel.elf` y `kernel.sig`.
- **Capacidades:** IPC por puertos con derechos `READ`/`WRITE`/`SEND`/
  `EXECUTE`, reducción monotónica de derechos y aislamiento de procesos
  (A5/A6).
- **FS RAM:** sistema de archivos en memoria con control de acceso por
  capacidades (A7); todo lo creado durante una sesión se pierde al apagar la
  VM.
- **Login y terminal scriptada:** pantalla de login y terminal con los
  comandos A9 (A8/A9). La terminal se ejercita mediante **auto-pruebas en el
  arranque**; no hay REPL interactivo (ver «Uso desde la consola»).
- **Volumen cifrado:** sistema de objetos en disco con copy-on-write,
  checksums y detección de corrupción (T1.3), y volumen cifrado
  XTS-AES-256 con KDF Argon2id sobre una passphrase (T1.5).
- **Red virtio-net:** pila Ethernet/ARP/IPv4/IPv6/TCP/UDP con tráfico real
  host↔guest sobre el backend UDP dgram de QEMU, sin TAP ni root (T2.1).
- **Usuarios y ACLs:** grupos inferidos del nombre de usuario y ACLs
  estáticas sobre los objetos del volumen cifrado, con acceso permitido y
  denegado comprobado (T2.2).
- **Acceso remoto cifrado:** perfil TLS 1.3 PSK **dev-only** sobre UDP/9443
  con lecturas autorizada/denegada según ACL y rechazo de replay (T2.3).
- **Imagen instalable:** `tools/install-image.sh` crea una imagen GPT
  arrancable desde cero a partir de los artefactos construidos (T3.3).
- **QA de estabilidad:** burn-in de arranque reproducible con
  `tests/run-load.sh` (T3.1).

## 2. Requisitos

- **Ubuntu 24.04** (sistema de referencia probado).
- **QEMU** (`qemu-system-x86_64`) y **OVMF** (firmware UEFI) para arrancar la
  imagen.
- **Rust** con rustup y los targets `x86_64-unknown-uefi` (bootloader) y
  `x86_64-unknown-none` (kernel); `rust-toolchain.toml` fija ambos targets y
  los componentes `rust-src`/`llvm-tools`.
- **Herramientas de imagen:** `sgdisk` (paquete `gdisk`), `mtools`
  (`mmd`/`mcopy`/`mdir`) y `mkfs.fat` (paquete `dosfstools`); `xorriso` queda
  incluido en la línea de instalación recomendada por `tools/check-env.sh`.
- **`socat`** (monitor de QEMU usado por `tools/run.sh`) y **`python3`**.
- **`python3-cryptography`** (paquete `python3-cryptography`): lo usa
  `tools/net-peer.py` para la QA de red y TLS.

Instalación en Ubuntu 24.04:

```bash
sudo apt update
sudo apt install -y qemu-system-x86 ovmf build-essential git gdisk dosfstools \
  mtools xorriso nasm socat python3 python3-cryptography
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
rustup target add x86_64-unknown-uefi x86_64-unknown-none
rustup component add rust-src llvm-tools
```

Verifica el entorno con:

```bash
tools/check-env.sh
```

El script comprueba QEMU, OVMF en `/usr/share/OVMF/`, `rustc`/`rustup` y el
target `x86_64-unknown-uefi`, y termina con código 0 si todo está correcto
(imprime qué falta en caso contrario). Las demás herramientas (`sgdisk`,
`mtools`, `socat`, `python3-cryptography`, …) las exigen `tools/build.sh`,
`tools/run.sh` y `tools/install-image.sh` al usarlas y fallan con un mensaje
claro si no están en `PATH`.

## 3. Construcción

```bash
tools/build.sh
```

Genera en `build/`:

```
build/bootloader.efi   aplicación UEFI (EFI/BOOT/BOOTX64.EFI)
build/kernel.elf       kernel bare-metal
build/kernel.sig       firma Ed25519 de kernel.elf (64 bytes en bruto)
build/meridian.img     imagen GPT + ESP FAT32 con los tres archivos anteriores
build/blocktest.img    disco QA de 1 MiB para el servicio de bloques (T1.2)
```

La firma se calcula sobre el contenido íntegro de `kernel.elf` con la clave
privada de `MERIDIAN_KERNEL_SIGNING_KEY` o, si no está definida, con la clave
de desarrollo (vector público RFC 8032 TEST 1, **solo para desarrollo**; el
script avisa cuando la usa). El bootloader verifica `\kernel.sig` en cada
arranque antes de cargar el ELF (ver `docs/boot.md`).

Variables útiles: `MERIDIAN_BUILD_DIR` (directorio de salida, por defecto
`build/`), `CARGO_PROFILE` (`release` por defecto) y
`MERIDIAN_KERNEL_SIGNING_KEY` (clave Ed25519 en hexadecimal de 64 bytes).

## 4. Arranque de desarrollo

Para una sola ejecución con verificación:

```bash
tools/run.sh
```

`tools/run.sh` arranca `build/meridian.img` con QEMU/OVMF (junto al disco QA
`build/blocktest.img`), lanza `tools/net-peer.py` en background y guarda los
logs en `build/`:

```
build/serial.log       salida serie de OVMF y del kernel (COM1)
build/debugcon.log     trazas del bootloader (puerto 0xE9)
build/qemu.log         log de QEMU
build/screen.ppm       captura GOP (screen.png si Pillow está disponible)
build/net-peer.log     eventos de la pila de red T2.1
build/tls-capture.log  captura del perfil TLS T2.3
```

Para el QA completo (construye, arranca y verifica todos los marcadores):

```bash
tests/run-alpha.sh
```

Salida esperada al final:

```
[alpha] OK: Alpha QA PASS — login + comandos A9 ejecutados en QEMU (83/83 marcadores; 83 esperados)
```

Los **83 marcadores** se reparten en **76 en COM1** (`build/serial.log`:
marcadores A4–A9, T1.2, T1.3, T1.5, T2.1, T2.2 y T2.3, además del mensaje de
arranque y del banner de login) **+ 7 TLSCAP** (`build/tls-capture.log`,
T2.3). Además, la pila de red T2.1 debe registrar
**6/6 eventos `NETPEER: … PASS`** en `build/net-peer.log`; cuando todo pasa,
`tools/run.sh` lo confirma con `net-peer: 6/6 NETPEER y 7/7 TLSCAP PASS`.

Opciones frecuentes (el detalle completo está en `docs/alpha.md`):

```bash
tests/run-alpha.sh --timeout 60      # o MERIDIAN_TIMEOUT=90
tests/run-alpha.sh --no-screenshot
tests/run-alpha.sh --no-build        # reutiliza build/meridian.img existente
tools/run.sh --expect-signature-reject   # QA T1.1: exige rechazo de firma
```

## 5. Instalación (imagen instalable)

`tools/install-image.sh` crea **desde cero** una imagen GPT arrancable con el
mismo layout que `tools/build.sh` (sector 2048, ESP FAT32 de 64 MiB,
typecode `ef00`, nombre `MERIDIAN`, con `EFI/BOOT/BOOTX64.EFI`, `kernel.elf`
y `kernel.sig`) a partir de artefactos ya construidos:

```bash
tools/build.sh
tools/install-image.sh --disk build/meridian-installed.img
```

Opciones:

| Opción | Significado | Valor por defecto |
| --- | --- | --- |
| `--disk RUTA` | archivo de imagen a crear (obligatorio) | — |
| `--size-mb N` | tamaño de la imagen en MiB (mínimo 96) | `96` |
| `--artifacts DIR` | directorio con `bootloader.efi`, `kernel.elf` y `kernel.sig` | `build` |
| `--help` | muestra el uso | — |

**Seguridad:** el script rechaza `--disk` vacío, cualquier ruta bajo `/dev/`
(incluso tras canonicalizar con `realpath`), enlaces simbólicos y targets
existentes que no sean archivos regulares: **nunca escribe en dispositivos de
bloque reales**. Antes de terminar verifica el contenido de la ESP con `mdir`
e imprime un resumen `[install] …`. No crea `blocktest.img`: ese disco QA lo
genera `tools/build.sh`.

Para arrancar directamente la imagen instalada:

```bash
tools/run.sh --image build/meridian-installed.img --no-screenshot
```

`--image` sustituye la imagen por defecto (`build/meridian.img`). El disco QA
`build/blocktest.img` sigue siendo obligatorio para `tools/run.sh` (lo crea
`tools/build.sh`; si usas otro directorio de artefactos, pásalo con
`--blocktest RUTA`). El ciclo completo —construir, instalar, verificar GPT/ESP
y arrancar— está automatizado en:

```bash
tests/run-install.sh
```

## 6. QA completa

| Comando | Qué comprueba | Salida esperada |
| --- | --- | --- |
| `tests/run-alpha.sh` | Construcción + arranque QEMU/OVMF + **83/83 marcadores** (76 COM1 + 7 TLSCAP) y 6/6 `NETPEER` | `[alpha] OK: Alpha QA PASS — login + comandos A9 ejecutados en QEMU (83/83 marcadores; 83 esperados)` |
| `tests/run-load.sh --runs 5` | `cargo test -p meridian-crypto` una vez y 5 iteraciones de `tests/run-alpha.sh` con 83/83, 6/6 y 7/7 en cada una (T3.1) | `T3.1 load QA PASS (5/5 iteraciones, 83/83 marcadores por iteración)` |
| `tests/run-install.sh` | Artefactos frescos, imagen instalada desde cero, GPT con `sgdisk` y ESP con `mdir`, y arranque de la imagen instalada (T3.3) | `T3.3 install QA PASS: imagen instalada desde cero y arrancada en QEMU (83/83 marcadores)` |
| `tests/run-signature.sh` | Imagen aislada con `\kernel.sig` corrupta: el bootloader debe abortar antes de cargar el ELF (T1.1) | `[OK] T1.1 firma inválida rechazada` |
| `cargo test -p meridian-crypto` | Vectores de las primitivas criptográficas propias (también ejecutado por `tests/run-load.sh` antes del burn-in) | resumen `test result: ok` |

Logs de referencia: `build/alpha-build.log`, `build/alpha-run.log`,
`build/serial.log`, `build/net-peer.log`, `build/tls-capture.log`,
`build/load/` (iteraciones del burn-in y `crypto.log`) y `build/install/`
(instalación completa).

## 7. Uso desde la consola

Tras el arranque, la salida serie muestra la pantalla de login:

```
=== Meridian login ===
usuario: _
contrasena: _
(alpha: hash temporal, no apto para produccion)
```

Credenciales de demostración usadas por las auto-pruebas:

| Usuario | Contraseña | Uso |
| --- | --- | --- |
| `alice` | `secret` | prueba de login A8 y capacidades sobre `/home/alice` |
| `root` | `meridian` | sesión desde la que se ejecuta la terminal A9 |

La terminal exige una sesión válida y ejecuta estos comandos (los mismos de
la Alpha; el detalle y la tabla completa están en `docs/alpha.md`):

`help`, `pwd`, `mkdir` (alias `md`), `ls`, `cd`, `echo texto > archivo`,
`cat`, `rmdir` (alias `rd`, solo directorios vacíos), `rm`, `rm -r`, `clear`
y `exit`.

**Importante:** no hay entrada interactiva real de teclado ni REPL de usuario
en tiempo de ejecución. La pantalla de login y la terminal se **ejercitan
mediante auto-pruebas scriptadas durante el arranque**; la salida que ves en
`build/serial.log` es el resultado de esas pruebas, no de una sesión
interactiva. Además, la FS es volátil (RAM): todo lo creado durante la demo
se pierde al apagar la VM. Ver `docs/boot.md` y `docs/alpha.md` §6.

## 8. Acceso remoto cifrado

T2.3 añade un perfil **TLS 1.3 PSK propio, dev-only (solo QA)** sobre el
mismo backend UDP dgram de T2.1, en el puerto **UDP 9443**. No es un servicio
de login remoto para usuarios finales: es la demostración cifrada de la Fase 2.
Durante cada arranque QA demuestra:

- **handshake TLS 1.3** con PSK (ClientHello/ServerHello/Finished verificados);
- **lectura cifrada autorizada** según la ACL del usuario (T2.2) y **lectura
  cifrada denegada** para un usuario sin derecho;
- **replay rechazado:** un request con número de secuencia ya usado recibe una
  alerta cifrada;
- **captura de la sesión** en `build/tls-capture.log` con 7 eventos
  `TLSCAP: … PASS`, incluida la confirmación de que el valor
  `remote-notice-v1` nunca viaja en claro.

El cliente TLS en el host lo ejecuta `tools/net-peer.py` (requiere
`python3-cryptography` en el host). El PSK es fijo y de desarrollo
(`SHA256(b"meridian-tls13-dev-psk-v1")`); el detalle del protocolo y de los
marcadores está en `docs/alpha.md` §1 (T2.3).

## 9. Limitaciones y seguridad

La beta **no es producción** y **no debe usarse con datos sensibles**:

- **Claves de desarrollo:** la firma Ed25519 por defecto es el vector público
  RFC 8032 TEST 1; la passphrase XTS del volumen está embebida
  (`meridian-alpha-volume-key-v1`) con sal determinista; el PSK de TLS es
  fijo. Solo sirven para desarrollo y QA.
- **Criptografía propia sin auditar:** las primitivas (AES, SHA-256,
  Argon2id, XTS, Ed25519, GCM, HKDF) se validan con vectores oficiales
  NIST/RFC y `cargo test -p meridian-crypto`, pero **no han pasado auditoría
  externa** (Fase 8). El riesgo R7 del plan mantiene la beta marcada como no
  apta para datos sensibles hasta entonces.
- **Login:** el hash de contraseña sigue siendo **temporal y no
  criptográfico** (A8), en memoria y sin persistencia.
- **Alcance:** sin soporte **ARM64** (Fase 5) ni escritorio gráfico usable
  (Fase 4); la FS es RAM volátil y la terminal es scriptada, sin REPL.
- **Arranque:** la verificación de firma es interna del bootloader, no hay
  Secure Boot de firmware ni cadena de confianza del UEFI.
- **Acceso remoto:** el perfil TLS 1.3 PSK es dev-only con PSK fijo; no
  exponer el puerto 9443 en ninguna red.

## 10. Solución de problemas

- **Falta OVMF o QEMU:** ejecuta `tools/check-env.sh` e instala lo que
  indique (`sudo apt install -y qemu-system-x86 ovmf`). OVMF debe estar en
  `/usr/share/OVMF/`.
- **Falta `python3-cryptography`:** `tools/net-peer.py` termina con un error
  de importación y faltan eventos `NETPEER`/`TLSCAP` (o no se crea
  `build/tls-capture.log`). Instala el paquete:
  `sudo apt install -y python3-cryptography`.
- **Firma inválida:** `build/debugcon.log` muestra
  `meridian: firma de kernel inválida; abortando` o
  `meridian: firma de kernel ausente; abortando` y el kernel no arranca
  (`Meridian booting` no aparece). Reconstruye con `tools/build.sh`; si usas
  `MERIDIAN_KERNEL_SIGNING_KEY`, la clave pública embebida en el bootloader
  debe corresponder a esa clave. El caso esperado de rechazo lo cubre
  `tests/run-signature.sh`. Detalle en `docs/boot.md`.
- **Timeout o máquina lenta:** aumenta el tiempo de QEMU, por ejemplo
  `MERIDIAN_TIMEOUT=90 tests/run-alpha.sh` o `tools/run.sh --timeout 60`; en
  burn-in largos puede ayudar `tests/run-load.sh --runs 5 --no-screenshot`.
- **Puertos UDP 55124/55125 ocupados:** `tools/net-peer.py` (55124) y el
  backend dgram de QEMU (55125) no pueden bindear si una ejecución anterior
  quedó colgada. Comprueba procesos residuales de QEMU/`net-peer.py` de una
  ejecución interrumpida, termínalos y vuelve a ejecutar; los puertos del peer
  se configuran en `tools/run.sh`/`tools/net-peer.py`
  (`--bind-port`/`--peer-port`).

Si algo falla, revisa `build/serial.log` (marcadores `FAIL` del kernel),
`build/qemu.log` y `build/net-peer.log`.

## 11. Documentación relacionada

- [`docs/alpha.md`](alpha.md) — guía técnica detallada: Alpha, red T2.1,
  permisos T2.2, acceso remoto T2.3, QA de estabilidad T3.1 e imagen
  instalable T3.3.
- [`docs/boot.md`](boot.md) — arranque UEFI, handoff `BootInfo` y
  verificación de firma del kernel (T1.1).
- [`README.md`](../README.md) — resumen del proyecto, principios, requisitos
  y licencia.
