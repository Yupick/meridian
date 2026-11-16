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
