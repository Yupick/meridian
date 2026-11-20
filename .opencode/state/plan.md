# Plan — Meridian

## Objetivo
Construir Meridian, un sistema operativo de código abierto (AGPL 3.0), con microkernel en Rust,
orientado a servidor primero y escritorio después, con arranque UEFI, capacidades simples,
criptografía propia validada y soporte multiarquitectura (x86-64, luego ARM64).

## Contexto
- Proyecto de investigación que termina en producto real. Se permite la venta con servicios de soporte.
- Licencia: GNU AGPL v3.0 o posterior. Autor original: Cristian Saqueta Melo (NOTICE).
- Desarrollo en Ubuntu 24.04.5 LTS. Los paquetes (QEMU, OVMF, Rust) se instalan en el equipo de
  desarrollo, que es distinto del equipo de planificación. Las tareas de entorno se marcan como
  "en el equipo de desarrollo".
- Flujo Git: GitFlow (ver sección Flujo de trabajo). Commits, PR y sincronización con `update.sh`.
- Decisiones cerradas: microkernel; Rust no_std; capacidades simples; UEFI en la beta;
  criptografía propia con vectores oficiales y auditoría antes de producción.
- Pendiente: nombre definitivo del SO.
- Compiladores nativos C/C++/Rust/Java: fase 6. Se construyen desde Ubuntu como herramienta.

## Estados y reglas
- Estados: `pending`, `in_progress`, `completed`, `cancelled`.
- Una tarea solo pasa a `in_progress` si todas sus dependencias están `completed`.
- Varias tareas pueden estar `in_progress` A LA VEZ, solo si: (a) no dependen entre sí,
  (b) tocan archivos o directorios distintos, y (c) cada una tiene su propia rama y su propio agente.
- Una tarea pasa a `completed` solo cuando sus criterios de aceptación se verifican
  (QA lo confirma) y su PR está fusionado en `develop`.
- `completed-at` solo cuando el estado es `completed`; se escribe la fecha de fusión del PR.
- Cada tarea se verifica con sus criterios de aceptación antes de marcarse `completed`.

## Flujo de trabajo (GitFlow)

Ramas:
- `main`: solo versiones publicadas. Se actualiza desde `develop` (o `release/*` / `hotfix/*`) mediante PR.
- `develop`: integración. Todas las tareas terminan aquí.
- `feature/<ID>-<nombre-corto>`: una rama por tarea. Ejemplo: `feature/A6-ipc-capacidades`.
- `fix/<ID>-<nombre>`: corrección de una tarea ya completada.
- `release/<version>` y `hotfix/<version>`: se definen cuando exista una beta (fase 3).

Reglas para el orquestador:
1. Antes de empezar una tarea, crear su rama desde `develop` actualizado:
   `git checkout develop && ./update.sh --pull && git checkout -b feature/<ID>-<nombre>`.
2. Commits solo en la rama de la tarea. Nunca commits directos en `main` ni en `develop`.
3. Al terminar y pasar los criterios de aceptación: `./update.sh --commit --push -m "..."` y
   `./update.sh --pr -t "<ID>: <título>"`. El PR apunta a `develop`.
4. Fusionar un PR en `develop` solo cuando QA haya verificado los criterios. La fusión la confirma
   el usuario (`./update.sh --merge-pr N` o botón de GitHub). Los agentes no fusionan por su cuenta.
5. Tras cada fusión: `develop` se actualiza localmente con `./update.sh --pull`, y las ramas
   de tareas que dependan de ella se rebasan o se reabren desde `develop` actualizado.
6. Cuando una fase cierra (todas sus tareas `completed`), se abre PR `develop -> main` y se
   etiqueta la versión.
7. Conflictos: no se fuerza nada (sin `push --force`, sin reescribir historial). Si dos tareas
   en paralelo tocan el mismo archivo, la segunda espera a que se fusione la primera.
8. No se suben archivos sensibles (claves, `auth.json`, `llA.txt`, `.env`). `update.sh` los filtra,
   pero el agente debe revisarlos también.

Política de ramas (obligatoria para el orquestador y todos los agentes):
- El orquestador NUNCA modifica `main`. Todo cambio va en una rama `feature/*` o `fix/*`,
  o directamente en `develop` solo para cambios de documentación del plan.
- Un cambio llega a `main` únicamente por PR desde `develop` (o desde `release/*`/`hotfix/*`),
  confirmado por el usuario.
- Limpieza de ramas: la rama de una tarea se borra en cuanto su PR está fusionado en `develop`
  (local y en `origin`). No se acumulan ramas ya integradas.
- Auditoría periódica: al inicio de cada sesión y al cerrar cada fase, el orquestador lista las
  ramas y comprueba cuáles están totalmente fusionadas en `develop` (`git branch --merged develop`)
  y cuáles tienen PR cerrado. Las ramas integradas que sigan existentes se reportan al usuario
  para borrarlas. Las ramas sin fusionar nunca se borran sin confirmación.
- Ramas que se conservan hasta el cierre de versión: `main`, `develop`, y las `release/*` de
  release candidates hasta que se publique la versión final y su tag.
- Nunca se borran ramas remotas de otros colaboradores ni ramas con trabajo sin subir.

Paralelismo: las tareas sin dependencias pendientes (por ejemplo T1.4 criptografía base mientras
se trabaja en A-fase) pueden ejecutarse a la vez, cada una en su rama y en su propio agente.
Para evitar mezclar cambios en el mismo directorio de trabajo, cada agente usa un `git worktree`
separado: `git worktree add ../meridian-<ID> feature/<ID>-<nombre>`.

## Fases del proyecto

| Fase | Nombre | Objetivo | Criterio de salida |
|---|---|---|---|
| **A** | Alpha | Kernel que arranca, login y terminal con comandos básicos de archivos | Sesión de login y comandos ls, cd, pwd, mkdir, rmdir, rm, cat, echo, help, exit funcionando en QEMU |
| **1** | Base del sistema | Cargador UEFI propio, servicio de bloques, sistema de objetos en disco, cifrado de volumen | Volumen cifrado que persiste tras reinicio |
| **2** | Red y servicios de servidor | Pila de red, usuarios con permisos, acceso remoto cifrado | Login remoto cifrado desde otra máquina |
| **3** | Beta de servidor | Estabilización, pruebas, documentación, imagen instalable | Imagen instalable probada en QEMU desde cero |
| **4** | Escritorio y gráficos | Compositor, ventanas, GOP y luego driver de GPU en espacio de usuario | Escritorio usable en QEMU |
| **5** | ARM64 | HAL y arranque para aarch64 | Arranque en QEMU aarch64 |
| **6** | Compiladores y runtimes | Clang/LLVM (C/C++), rustc, runtime de Java | Compilar y ejecutar un programa en cada lenguaje |
| **7** | Multi-SO y hardware real | Cargador multi-SO y una placa de referencia x86-64 | Meridian arranca junto a otro SO en hardware real |
| **8** | Producto | Auditoría criptográfica externa, soporte y licenciamiento | Informe de auditoría sin hallazgos críticos abiertos |

Alcance de la beta: fases A, 1, 2 y 3. La beta de servidor NO se marca como apta para datos
sensibles hasta completar la auditoría de la fase 8.

---

## Fase A — Alpha

Dependencias: ninguna para A1. Después, en orden de la lista.

### A1 — Entorno de desarrollo (equipo de desarrollo)
- **Estado:** completed
- **Prioridad:** alta
- **Asignado:** usuario (instalación) + Backend (script de verificación)
- **Dependencias:** ninguna
- **Descripción:** instalar en el equipo de desarrollo QEMU, OVMF, Rust con rustup y el target `x86_64-unknown-uefi`.
- **Criterios de aceptación:**
  - `qemu-system-x86_64 --version` responde.
  - Existe OVMF_CODE en `/usr/share/OVMF/`.
  - `rustc --version` y `rustup target list --installed` incluyen `x86_64-unknown-uefi`.
  - `tools/check-env.sh` verifica lo anterior y termina con código 0.
- **completed-at:** 2026-11-18 (PR #6 mergeado en develop, commit f22b8f9; instalación de paquetes/rust en el equipo de desarrollo confirmada por el usuario)

### A2 — Cargador UEFI mínimo
- **Estado:** completed
- **Prioridad:** alta
- **Asignado:** Backend
- **Dependencias:** A1
- **Descripción:** cargador en `bootloader/` que arranca en UEFI, obtiene el mapa de memoria y carga el kernel desde una imagen FAT.
- **Criterios de aceptación:**
  - `tools/build.sh` genera la imagen FAT.
  - `tools/run.sh` arranca la imagen en QEMU con OVMF sin errores.
  - Se muestra un mensaje en pantalla GOP.
- **completed-at:** 2026-11-18 (PR #7 mergeado en develop, commit f603425)

### A3 — Consola del kernel
- **Estado:** completed
- **Prioridad:** alta
- **Asignado:** Backend
- **Dependencias:** A2
- **Descripción:** salida por COM1 (serie) y por framebuffer GOP desde el kernel. El kernel recibe la estructura de arranque con el mapa de memoria.
- **Criterios de aceptación:**
  - "Meridian booting" aparece en la consola serie de QEMU.
  - El mensaje aparece también en pantalla.
- **completed-at:** 2026-11-18 (PR #8 mergeado en develop, commit 7d03685)

### A4 — Memoria física y virtual
- **Estado:** completed
- **Prioridad:** alta
- **Asignado:** Backend
- **Dependencias:** A3
- **Descripción:** asignador de páginas físicas (buddy), paginación de 4 niveles y espacio virtual del kernel con W^X.
- **Criterios de aceptación:**
  - Pruebas de reserva y liberación de páginas sin fugas.
  - Ninguna página es escribible y ejecutable a la vez.
- **completed-at:** 2026-11-18 (PR #9 mergeado en develop, commit d6441a4)

### A5 — Procesos y espacios de direcciones aislados
- **Estado:** completed
- **Prioridad:** alta
- **Asignado:** Backend
- **Dependencias:** A4
- **Descripción:** creación de procesos con tabla de páginas propia.
- **Criterios de aceptación:**
  - Dos procesos de prueba no pueden leer la memoria del otro (fallo controlado).
- **completed-at:** 2026-11-18 (PR #10 mergeado en develop, commit 57ff6c1)

### A6 — IPC y capacidades simples
- **Estado:** completed
- **Prioridad:** alta
- **Asignado:** Backend
- **Dependencias:** A5
- **Descripción:** paso de mensajes por puertos y capacidades con derechos leer, escribir, enviar y ejecutar.
- **Criterios de aceptación:**
  - Un proceso envía un mensaje a otro por un puerto.
  - Un proceso sin capacidad recibe error al usar el recurso.
  - Una capacidad puede reducirse, nunca ampliarse.
- **completed-at:** 2026-11-18 (PR #11 mergeado en develop, commit a93b9bb)

### A7 — Servicio de archivos mínimo
- **Estado:** completed
- **Prioridad:** alta
- **Asignado:** Backend (servicio) + Db (modelo de objetos)
- **Dependencias:** A6
- **Descripción:** servicio de archivos y directorios en memoria (RAM). Operaciones: crear directorio, eliminar directorio vacío, eliminar archivo, listar, leer, escribir, cambiar directorio. Cada objeto tiene atributos tipados (nombre, tipo, tamaño).
- **Criterios de aceptación:**
  - Pruebas unitarias de cada operación, incluido error al borrar un directorio no vacío.
  - Una capacidad de solo lectura no permite escribir.
- **completed-at:** 2026-11-18 (PR #12 mergeado en develop, commit 2aa0f1c)

### A8 — Servicio de login y usuarios
- **Estado:** completed
- **Prioridad:** alta
- **Asignado:** Backend (servicio) + Frontend (pantalla de login)
- **Dependencias:** A6, A7
- **Descripción:** usuarios con contraseña almacenada como hash Argon2id (implementación propia validada en la tarea de criptografía de la fase 1; en alpha se usa un hash temporal marcado como no apto para producción). Cada sesión recibe capacidades según el usuario.
- **Criterios de aceptación:**
  - Login correcto abre la sesión; contraseña incorrecta se rechaza sin revelar si el usuario existe.
  - Tras login, el usuario tiene capacidades sobre su directorio personal.
- **completed-at:** 2026-11-18 (PR #13 mergeado en develop, commit b4bd33b)

### A9 — Terminal alpha
- **Estado:** completed
- **Prioridad:** alta
- **Asignado:** Frontend (interfaz de texto) + Backend (integración)
- **Dependencias:** A3, A8
- **Descripción:** shell de texto con comandos:
  - `ls` lista archivos y directorios.
  - `cd` cambia de directorio.
  - `pwd` muestra el directorio actual.
  - `mkdir` (alias `md`) crea un directorio.
  - `rmdir` (alias `rd`) elimina un directorio vacío.
  - `rm` elimina un archivo; con `-r` elimina un directorio con contenido.
  - `cat` muestra un archivo.
  - `echo texto > archivo` escribe en un archivo.
  - `help` lista los comandos; `clear` limpia la pantalla; `exit` cierra la sesión.
- **Criterios de aceptación:**
  - Cada comando funciona en QEMU con una prueba scriptada.
  - `rmdir` sobre un directorio con contenido muestra error claro.
  - Sin login no se accede a la terminal.
- **completed-at:** 2026-11-18 (PR #14 mergeado en develop, commit 71fab07)

### A10 — Pruebas y cierre de alpha
- **Estado:** completed
- **Prioridad:** media
- **Asignado:** QA (pruebas) + Docs (guía de uso)
- **Dependencias:** A9
- **Descripción:** pruebas de integración en QEMU con un único comando y guía de arranque.
- **Criterios de aceptación:**
  - `tests/run-alpha.sh` ejecuta login y todos los comandos de A9 y termina con código 0.
  - `docs/alpha.md` describe cómo construir, ejecutar y usar la terminal.
- **completed-at:** 2026-11-18 (PR #15 mergeado en develop, commit ae57c85)

---

## Fase 1 — Base del sistema (completed)

- **completed-at:** 2026-11-19 (PR #22 `develop -> main` mergeado, commit `72f9bf3`; tag `v0.2.0-base`; criterio de salida cumplido con volumen cifrado persistente y 56/56 marcadores en `tests/run-alpha.sh`).

- **T1.1 — Cargador con verificación de firma** — completed, alta, Backend. Dependencias: A10. Criterio: el cargador rechaza un kernel con firma Ed25519 inválida.
  - **completed-at:** 2026-11-18 (PR #18 mergeado en develop, commit fdfd2a0; `tests/run-signature.sh` rechaza firma corrompida y `tests/run-alpha.sh` pasa 40/40 marcadores)
- **T1.2 — Servicio de bloques** — completed, alta, Backend. Dependencias: T1.1. Criterio: lectura y escritura de sectores con virtio-blk en QEMU.
  - **completed-at:** 2026-11-19 (PR #19 mergeado en develop, commit 114fbfb; `tests/run-alpha.sh` pasa 46/46 marcadores con lectura, escritura y control de capacidades del servicio de bloques)
- **T1.3 — Sistema de objetos en disco** — completed, alta, Db + Backend. Dependencias: T1.2. B+ trees para índices, extents para datos, copy-on-write y checksums. Criterio: datos íntegros tras corte de energía simulado.
  - **completed-at:** 2026-11-19 (PR #20 mergeado en develop, commit bd4d0c4; `tests/run-alpha.sh` pasa 51/51 marcadores con formato, COW, corte simulado y detección de corrupción por checksum; `tests/run-signature.sh` OK)
- **T1.4 — Criptografía base** — completed, alta, Backend + QA. Dependencias: ninguna (puede hacerse en paralelo). AES-256-XTS, SHA-256, Argon2id y Ed25519 propios. Criterio: pasan todos los vectores oficiales NIST/RFC.
  - **completed-at:** 2026-11-18 (PR #17 mergeado en develop, commit 0ec845d; `tests/run-crypto-vectors.sh` OK en debug y release, 14/14 tests)
- **T1.5 — Cifrado de volumen** — completed, alta, Backend. Dependencias: T1.3, T1.4. Criterio: el volumen es ilegible sin clave y persiste tras reinicio.
  - **completed-at:** 2026-11-19 (PR #21 mergeado en develop, commit cfc2a8a; `tests/run-alpha.sh` pasa 56/56 marcadores con XTS-AES-256, Argon2id, rechazo de clave errónea, ciphertext en crudo y persistencia tras `open`; `tests/run-signature.sh` OK)

## Fase 2 — Red y servicios de servidor (completed)

- **completed-at:** 2026-11-20 (PR #26 `develop -> main` mergeado; criterio de salida cumplido con login remoto cifrado TLS 1.3 PSK verificado sobre tráfico real host↔guest y 83/83 marcadores en `tests/run-alpha.sh`).

- **T2.1 — Pila de red** — completed, alta, Backend. Dependencias: A10. Ethernet, ARP, IPv4/IPv6, TCP y UDP con virtio-net. Criterio: ping y conexión TCP desde el host.
  - **completed-at:** 2026-11-19 (PR #23 mergeado en develop, commit eb581df; `tests/run-alpha.sh` pasa 64/64 marcadores con virtio-net, ARP, ICMPv4, UDP, TCP e ICMPv6 sobre tráfico real host↔guest; `build/net-peer.log` registra 6/6 eventos PASS; `tests/run-signature.sh` OK)
- **T2.2 — Usuarios con permisos** — completed, alta, Backend. Dependencias: A8, T1.5. Grupos y permisos sobre el sistema de objetos. Criterio: pruebas de acceso permitido y denegado.
  - **completed-at:** 2026-11-19 (PR #24 mergeado en develop, commit 5419f0a; `tests/run-alpha.sh` pasa 70/70 marcadores con grupos, ACLs estáticas sobre objetos cifrados, lectura permitida, escritura denegada, aislamiento entre grupos y persistencia tras `open`; `build/net-peer.log` 6/6; `tests/run-signature.sh` OK)
- **T2.3 — Acceso remoto cifrado** — completed, alta, Backend + QA. Dependencias: T2.1, T2.2, T1.4. Protocolo sobre TLS 1.3 propio. Criterio: sesión remota cifrada verificada con captura de tráfico.
  - **completed-at:** 2026-11-20 (PR #25 mergeado en develop, commit `b25aed8`; AES-128-GCM con vector NIST propio, perfil TLS 1.3 PSK dev-only sobre UDP/9443, lecturas cifradas autorizada/denegada integradas con T2.2, replay rechazado; `tests/run-alpha.sh` pasa 83/83 marcadores con 6/6 NETPEER y 7/7 TLSCAP; `tests/run-signature.sh` OK; `cargo test -p meridian-crypto` OK)

## Fase 3 — Beta de servidor (pending)

- **T3.1 — Estabilización y pruebas de carga** — pending, alta, QA. Dependencias: T2.3.
- **T3.2 — Documentación de usuario e instalación** — pending, media, Docs. Dependencias: T3.1.
- **T3.3 — Imagen instalable** — pending, alta, Backend. Dependencias: T3.1. Criterio: imagen instalada desde cero en QEMU.

## Fase 4 — Escritorio y gráficos (pending)

- **T4.1 — Compositor y ventanas** — pending, alta, Frontend. Dependencias: T3.3.
- **T4.2 — Driver de GPU en espacio de usuario** — pending, media, Backend. Dependencias: T4.1. Criterio: un fallo del driver no tumba el kernel.

## Fase 5 — ARM64 (pending)

- **T5.1 — HAL para aarch64** — pending, alta, Backend. Dependencias: T4.1. Criterio: arranque en QEMU aarch64.

## Fase 6 — Compiladores y runtimes (pending)

- **T6.1 — Port de Clang/LLVM (C/C++)** — pending, alta, Backend. Dependencias: T5.1.
- **T6.2 — Port de rustc y libstd** — pending, alta, Backend. Dependencias: T5.1.
- **T6.3 — Runtime de Java** — pending, media, Backend. Dependencias: T6.1. JVM escrita desde la especificación pública.

## Fase 7 — Multi-SO y hardware real (pending)

- **T7.1 — Cargador multi-SO** — pending, media, Backend. Dependencias: T3.3.
- **T7.2 — Soporte de una placa de referencia x86-64** — pending, media, Backend. Dependencias: T7.1.

## Fase 8 — Producto (pending)

- **T8.1 — Auditoría criptográfica externa** — pending, alta, QA + usuario. Dependencias: T1.5, T2.3.
- **T8.2 — Documentación de soporte y licenciamiento de servicios** — pending, media, Docs. Dependencias: T8.1.

## Riesgos

| ID | Riesgo | Impacto | Mitigación |
|---|---|---|---|
| R1 | Rendimiento de la IPC del microkernel | Alto | Mensajes de tamaño fijo; memoria compartida para datos grandes; medir desde A6 |
| R2 | Criptografía propia con errores sutiles | Alto | Vectores NIST/RFC; pruebas de tiempo constante; auditoría antes de producción |
| R3 | Alcance de compiladores nativos | Alto | Fase 6; construir desde Ubuntu como herramienta |
| R4 | Documentación limitada de GPU | Medio | Framebuffer UEFI y virtio-gpu en desarrollo |
| R5 | Cambios de versiones de paquetes | Bajo | Fijar versión de Rust con `rust-toolchain.toml` |
| R6 | AGPL limita la adopción por algunas empresas | Medio | Documentar opciones de licencia comercial antes de recibir contribuciones externas |
| R7 | Alpha y beta usan criptografía sin auditar | Alto | Marcar la beta como no apta para datos sensibles hasta la fase 8 |
