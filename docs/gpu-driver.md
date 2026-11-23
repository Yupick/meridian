# T4.2 — Driver de GPU en espacio de usuario

## Objetivo

Validar que un fallo del subsistema gráfico **no tumba el kernel**. El driver se
ejecuta detrás de una fachada de capacidades: los comandos se validan antes de
tocar el framebuffer, los errores se devuelven como `GpuError` y un fallo
inyectado reinicia la generación del driver sin `panic!` ni `halt()`.

## API

- `services/gpu/mod.rs`: `GPU_OBJECT_ID`, `SESSION_GPU_SLOT`, `GpuCommand`,
  `GpuError`, `GpuStats`, `init()`, `stats_snapshot()` y `submit()`.
- `services/gpu/selftest.rs`: secuencia de QA y marcadores `T4.2: ...`.

Comandos soportados:

| Comando | Derecho requerido | Descripción |
|---|---|---|
| `Query` | `WRITE` | Consulta de estado, sin dibujar. |
| `Clear { color }` | `WRITE` | Limpia el framebuffer con `0x00RRGGBB`. |
| `FillRect { x, y, width, height, color }` | `WRITE` | Rectángulo validado contra las dimensiones del GOP. |
| `InjectFault` | `EXECUTE` | Fallo controlado del driver; se reinicia y el kernel continúa. |

## Aislamiento

1. `submit()` comprueba la capacidad del slot antes de cualquier operación.
2. `validate()` rechaza rectángulos vacíos o fuera de pantalla.
3. Las órdenes de dibujo toman `&mut Framebuffer` solo durante la operación.
4. `InjectFault` incrementa `faults` y `restarts`, avanza `generation` y
   devuelve `GpuError::DriverFault`. `kernel_main` ignora el resultado y sigue.
5. Después del reinicio se envían nuevos comandos y se comprueba que el
   framebuffer sigue disponible.

## QA

```bash
tests/run-gpu-driver.sh
```

El script construye la imagen, arranca QEMU/OVMF y verifica estos marcadores en
COM1:

```text
T4.2: driver init OK
T4.2: valid command PASS
T4.2: invalid command rejected PASS
T4.2: capability denied PASS
T4.2: driver fault injected PASS
T4.2: driver restart OK
T4.2: kernel alive after driver restart PASS
T4.2: GPU driver tests PASS
```

También falla si aparece `T4.2: ... FAIL`, `MERIDIAN-KERNEL: PANIC` o si no hay
una captura GOP de al menos 800x600. La captura se copia a
`build/gpu/screen.png`.

En ausencia de GOP, el driver funciona en modo COM1 y el selftest imprime
`T4.2: GOP no utilizable; solo COM1` sin considerarlo un fallo.

## Enlaces relacionados

- [`docs/desktop.md`](desktop.md) — compositor T4.1 que consume el framebuffer.
- [`docs/interactive.md`](interactive.md) — terminal interactiva T4.3.
