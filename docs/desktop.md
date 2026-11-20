# T4.1 — Compositor y ventanas

## Alcance de T4.1

La fase T4.1 introduce un compositor minimalista de escritorio con exactamente **2 ventanas**, zona de enfoque y orden Z fijo. El objetivo es demostrar el uso de las primitivas de framebuffer (`fill_rect`, `draw_text`) para dibujar elementos visuales simples sin necesidad de alloc, ratón ni teclado.

### Ventanas

- **Ventana 0**: "System Monitor" — muestra información estática del framebuffer (resolución, stride/formato, "COM1 115200", "Meridian Desktop T4.1").
- **Ventana 1**: "Meridian Terminal" — muestra una barra de título y líneas de demo/prompt (`meridian:/$`).

### Enfoque

- El foco puede alternarse entre las 2 ventanas con `focus_next()`.
- El borde de la ventana cambia de color cuando tiene foco (verde/azul activo vs. gris inactivo).
- El contenido de la barra de título también cambia de color.

### Primitiveivas utilizadas

- `Framebuffer::fill_rect` — para fondos, bordes y barras de título.
- `Framebuffer::draw_text` — para dibujar títulos y contenido de texto.
- `Rgb::new` — para definir colores de 8 bits por canal.

### Captura de pantalla

- La última operación de dibujo antes de `halt()` es `gui::present(&mut console)`, que dibuja el escritorio completo.
- La captura `build/screen.png` (generada por `screendump` de QEMU + Pillow) muestra el escritorio con ambas ventanas.

### Comando de verificación

- `tests/run-desktop.sh` — construye, arranca QEMU/OVMF y verifica los 6 marcadores T4.1 en `build/serial.log` y la existencia de `build/screen.png` o `build/desktop/screen.png`.

### Limitaciones

- **Sin ratón:** no hay soporte para eventos de ratón en T4.1.
- **Sin teclado:** la terminal de ventana es puramente demostrativa; no hay entrada interactiva de usuario.
- **Sin ratón ni teclado:** la interacción completa llega en T4.3.

### Enlaces

- [docs/beta.md](../beta.md) — guía de la beta de servidor.