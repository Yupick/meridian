# Plan — Meridian

## Objetivo
Construir Meridian, un sistema operativo de código abierto (AGPL 3.0), con microkernel en Rust,
orientado a servidor primero y escritorio después, con arranque UEFI, capacidades simples,
criptografía propia validada y soporte multiarquitectura (x86-64, luego ARM64).

## Contexto
- Proyecto de investigación que termina en producto real. Se permite la venta con servicios de soporte.
- Licencia: GNU AGPL v3.0 o posterior. Autor original indicado en NOTICE.
- Entorno de desarrollo: Ubuntu 24.04.5 LTS, QEMU + OVMF, Rust vía rustup.
- Decisiones cerradas: microkernel; Rust no_std; capacidades simples; UEFI en la beta;
  criptografía propia con vectores oficiales y auditoría antes de producción.
- Pendiente: nombre definitivo del SO; autor en NOTICE (`<TU NOMBRE>`).
- Compiladores nativos C/C++/Rust/Java: fase posterior a la beta. Se construye desde Ubuntu
  como herramienta, no como parte del sistema.

## Tareas

Estados permitidos: `pending`, `in_progress`, `completed`, `cancelled`.
`completed-at` solo cuando el estado es `completed`.

### T0.1 — Entorno de desarrollo
- **Estado:** in_progress
- **Prioridad:** alta
- **Asignado:** Backend (script de verificación) + usuario (instalación con sudo)
- **Descripción:** instalar QEMU, OVMF, Rust con rustup y los targets necesarios.
- **Criterios de aceptación:**
  - `qemu-system-x86_64 --version` responde.
  - Existe `/usr/share/OVMF/` con OVMF_CODE.
  - `rustc --version` y `rustup target list --installed` incluyen `x86_64-unknown-uefi`.
  - Existe un script en `tools/` que verifica lo anterior.
- **completed-at:** —

### T0.2 — Cargador UEFI mínimo
- **Estado:** pending
- **Prioridad:** alta
- **Asignado:** Backend
- **Descripción:** cargador en `bootloader/` que arranca en UEFI, obtiene el mapa de memoria y carga el kernel.
- **Criterios de aceptación:**
  - Se genera una imagen FAT ejecutable por OVMF.
  - `tools/run.sh` arranca la imagen en QEMU sin errores.
  - El cargador muestra un mensaje en pantalla GOP.
- **completed-at:** —

### T0.3 — Consola del kernel
- **Estado:** pending
- **Prioridad:** alta
- **Asignado:** Backend
- **Descripción:** salida por puerto serie COM1 y por framebuffer GOP desde el kernel.
- **Criterios de aceptación:**
  - El mensaje "Meridian booting" aparece en la consola serie de QEMU.
  - El kernel recibe la estructura de arranque con el mapa de memoria.
- **completed-at:** —

### T0.4 — Memoria física y virtual
- **Estado:** pending
- **Prioridad:** alta
- **Asignado:** Backend
- **Descripción:** asignador de páginas físicas (buddy), paginación de 4 niveles y espacio virtual del kernel.
- **Criterios de aceptación:**
  - Reserva y libera páginas sin fugas en pruebas.
  - Mapeos con permisos W^X verificados.
- **completed-at:** —

### T0.5 — Procesos aislados
- **Estado:** pending
- **Prioridad:** alta
- **Asignado:** Backend
- **Descripción:** creación de procesos con espacios de direcciones independientes.
- **Criterios de aceptación:**
  - Dos procesos de prueba no pueden leer la memoria del otro (prueba de fallo controlado).
- **completed-at:** —

### T0.6 — IPC y capacidades simples
- **Estado:** pending
- **Prioridad:** alta
- **Asignado:** Backend
- **Descripción:** paso de mensajes y capacidades con derechos básicos (leer, escribir, enviar, ejecutar).
- **Criterios de aceptación:**
  - Un proceso envía un mensaje a otro a través de un puerto.
  - Un proceso sin capacidad recibe error al intentar usar el recurso.
  - Una capacidad puede reducirse, pero no ampliarse.
- **completed-at:** —

### T0.7 — Servicio de archivos mínimo
- **Estado:** pending
- **Prioridad:** media
- **Asignado:** Backend (servicio) + Db (modelo de objetos e índices)
- **Descripción:** objetos con atributos tipados en memoria y consulta por atributo.
- **Criterios de aceptación:**
  - Crear, leer y consultar objetos por atributo.
  - Pruebas unitarias de consulta.
- **completed-at:** —

### T0.8 — Terminal de servidor
- **Estado:** pending
- **Prioridad:** alta
- **Asignado:** Frontend (interfaz de texto) + Backend (integración con servicios)
- **Descripción:** shell de texto que usa los servicios anteriores.
- **Criterios de aceptación:**
  - Comandos básicos (listar, leer, consultar, salir) funcionan en QEMU.
  - Un comando sin capacidad suficiente se rechaza con mensaje claro.
- **completed-at:** —

### T0.9 — Pruebas y documentación de la fase 0
- **Estado:** pending
- **Prioridad:** media
- **Asignado:** QA (pruebas) + Docs (documentación)
- **Descripción:** pruebas de integración en QEMU y documentación de arranque y de la ABI inicial.
- **Criterios de aceptación:**
  - `tests/` ejecuta las pruebas de integración con un solo comando.
  - `docs/` describe la construcción, la ejecución y las capacidades.
- **completed-at:** —

## Riesgos

| ID | Riesgo | Impacto | Mitigación |
|---|---|---|---|
| R1 | Rendimiento de la IPC del microkernel | Alto | Mensajes de tamaño fijo; memoria compartida para datos grandes; medir desde T0.6 |
| R2 | Criptografía propia con errores sutiles | Alto | Vectores oficiales NIST/RFC; pruebas de tiempo constante; auditoría externa antes de producción |
| R3 | Alcance de compiladores nativos (Rust/C/C++/Java) | Alto | Fuera de la fase 0; construir desde Ubuntu; planificar en fase posterior a la beta |
| R4 | Documentación limitada de GPU | Medio | Usar framebuffer UEFI y virtio-gpu en desarrollo; GPU real en fase posterior |
| R5 | Cambios de versión de paquetes en Ubuntu | Bajo | Fijar versiones de Rust con `rust-toolchain.toml` |
| R6 | Licencia AGPL limita adopción por algunas empresas | Medio | Documentar opciones de licencia comercial con el titular del copyright antes de recibir contribuciones externas |

---

## Fases del proyecto

Orden decidido: beta de servidor antes que escritorio; ARM64 después del escritorio;
compiladores nativos después del escritorio; multi-SO en la fase 7.
Solo una tarea está `in_progress` a la vez. Las tareas de la fase 0 (T0.1 a T0.9) son las
que se ejecutan ahora; el resto está en `pending` y se activa al cerrar la fase anterior.

| Fase | Objetivo | Criterio de salida |
|---|---|---|
| **0. Inicio** | Kernel que arranca en QEMU con terminal de texto | Shell funcional en QEMU (T0.1 a T0.9) |
| **1. Base del sistema** | Cargador UEFI propio, servicio de bloques, sistema de objetos en disco, cifrado de volumen | Volumen cifrado que persiste tras reinicio |
| **2. Red y servicios de servidor** | Pila de red (IPv4/IPv6, TCP/UDP), usuarios y autenticación, acceso remoto cifrado | Inicio de sesión remoto cifrado desde otra máquina |
| **3. Beta de servidor** | Estabilización, pruebas de carga, documentación de usuario, imagen instalable | Imagen instalable publicada y probada en QEMU |
| **4. Escritorio y gráficos** | Compositor, ventanas, framebuffer GOP y luego driver de GPU en espacio de usuario | Escritorio usable en QEMU |
| **5. ARM64** | HAL y arranque para aarch64 | Arranque de Meridian en QEMU aarch64 |
| **6. Compiladores y runtimes nativos** | Port de Clang/LLVM (C/C++), rustc, runtime de Java | Compilar y ejecutar un programa en cada lenguaje dentro de Meridian |
| **7. Multi-SO y hardware real** | Cargador multi-SO y soporte de placas concretas | Meridian arranca junto a otro SO en hardware real |
| **8. Producto** | Auditoría criptográfica externa, documentación de soporte, servicios comerciales | Informe de auditoría publicado sin hallazgos críticos abiertos |

### Fase 1 — Base del sistema (pending)

- **T1.1 — Cargador UEFI propio** — pending, prioridad alta, Backend. Cargar kernel y pasar mapa de memoria; firma del kernel con Ed25519 (criterio: arranque rechaza kernel con firma inválida).
- **T1.2 — Servicio de bloques** — pending, prioridad alta, Backend. Driver virtio-blk en espacio de usuario (criterio: lectura y escritura de sectores en QEMU).
- **T1.3 — Sistema de objetos en disco** — pending, prioridad alta, Db + Backend. B+ trees para índices, extents para datos, copy-on-write y checksums (criterio: datos íntegros tras corte de energía simulado).
- **T1.4 — Cifrado de volumen** — pending, prioridad alta, Backend + Db. AES-256-XTS y Argon2id propios, validados con vectores NIST/RFC (criterio: vectores oficiales pasan; volumen ilegible sin clave).

### Fase 2 — Red y servicios de servidor (pending)

- **T2.1 — Pila de red** — pending, prioridad alta, Backend. Ethernet, ARP, IPv4/IPv6, TCP y UDP con driver virtio-net (criterio: ping y conexión TCP desde host).
- **T2.2 — Usuarios y autenticación** — pending, prioridad alta, Backend. Cuentas, grupos y contraseñas con Argon2id (criterio: login correcto e incorrecto en la terminal).
- **T2.3 — Acceso remoto cifrado** — pending, prioridad alta, Backend + QA. Protocolo propio sobre TLS 1.3 implementado desde cero (criterio: sesión remota cifrada verificada con capturas de tráfico).

### Fase 3 — Beta de servidor (pending)

- **T3.1 — Estabilización y pruebas de carga** — pending, prioridad alta, QA.
- **T3.2 — Documentación de usuario e instalación** — pending, prioridad media, Docs.
- **T3.3 — Imagen instalable** — pending, prioridad alta, Backend. Criterio: imagen probada en QEMU desde cero.

### Fase 4 — Escritorio y gráficos (pending)

- **T4.1 — Compositor y ventanas** — pending, prioridad alta, Frontend.
- **T4.2 — Driver de GPU en espacio de usuario** — pending, prioridad media, Backend. Criterio: un fallo del driver no tumba el kernel.

### Fase 5 — ARM64 (pending)

- **T5.1 — HAL para aarch64** — pending, prioridad alta, Backend. Criterio: arranque en QEMU aarch64.

### Fase 6 — Compiladores y runtimes nativos (pending)

- **T6.1 — Port de Clang/LLVM (C/C++)** — pending, prioridad alta, Backend.
- **T6.2 — Port de rustc y libstd** — pending, prioridad alta, Backend.
- **T6.3 — Runtime de Java** — pending, prioridad media, Backend. JVM escrita desde la especificación pública, no desde OpenJDK.

### Fase 7 — Multi-SO y hardware real (pending)

- **T7.1 — Cargador multi-SO** — pending, prioridad media, Backend.
- **T7.2 — Soporte de una placa de referencia x86-64** — pending, prioridad media, Backend.

### Fase 8 — Producto (pending)

- **T8.1 — Auditoría criptográfica externa** — pending, prioridad alta, QA + usuario.
- **T8.2 — Documentación de soporte y licenciamiento de servicios** — pending, prioridad media, Docs.
