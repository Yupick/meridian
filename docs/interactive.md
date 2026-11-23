# T4.3 — Terminal interactiva

## Abrir la terminal interactiva

### Modo scripted (QA automática)

```bash
./tools/run-interactive.sh
```

Ejecuta la secuencia por defecto: `root`, `meridian`, `pwd`, `mkdir qa-t43`, `ls`,
`echo hola-meridian > nota.txt`, `cat nota.txt`, `exit`. Verifica que el kernel
interactive emite los marcadores T4.3 esperados y captura la pantalla.

### Modo personalizado

```bash
./tools/run-interactive.sh --commands mi_comandos.txt
```

Envia los comandos listados en el archivo (una línea por comando) a la sesión
QEMU serie. También admite:

```bash
./tools/run-interactive.sh --timeout 120 --no-screenshot
```

## Credenciales dev-only

- **Usuario:** `root`
- **Contraseña:** `meridian`

> **Advertencia:** estas credenciales son **solo para desarrollo y QA**.
> No usan autenticación segura ni persistencia; no son aptas para entornos de
> producción.

## Comandos soportados

La secuencia scripted envía primero `root` y `meridian` como usuario y
contraseña del login dev-only. Después, la shell interactiva acepta los mismos
comandos de la fase A9:

| Comando | Descripción |
|---|---|
| `pwd` | Mostrar directorio actual |
| `mkdir qa-t43` | Crear directorio de prueba |
| `ls` | Listar archivos |
| `echo hola-meridian > nota.txt` | Escribir archivo de texto |
| `cat nota.txt` | Mostrar contenido de archivo |
| `help` | Mostrar ayuda de la shell |
| `clear` | Limpiar pantalla |
| `exit` | Finalizar sesión |

### backspace

El carácter backspace (`\b`) se maneja internamente por el kernel para
corrección de entrada en la línea de comando.

### `exit`

Emite el marcador `T4.3: sesion finalizada; halt`, detiene el kernel y el runner
cierra QEMU.

## Reflejo en la sesión T4.1 ( Escritorio )

La misma sesión serie se refleja en la ventana **"Meridian Terminal"** del
escritorio T4.1. Los comandos enviados por serie aparecen en la terminal del
escritorio en tiempo real, y la captura final queda en:

```
build/interactive/screen.png
```

La captura se genera mediante `screendump` de QEMU/OVMF convertido a PNG con
Pillow (si está disponible).

## Relación con la QA automática beta

La QA automática de la beta (`tests/run-alpha.sh`) **sigue sin interactuar**:
construye la imagen sin la feature `interactive`, arranca QEMU con el kernel
estándar y verifica los 83/83 marcadores (76 en COM1 + 7 TLSCAP) con 6/6
NETPEER. Esta QA no entra en la terminal interactiva T4.3 e imprime al final:

```
[alpha] OK: Alpha QA PASS — login + comandos A9 ejecutados en QEMU (83/83 marcadores; 83 esperados)
```

## Enlaces relacionados

- [`docs/beta.md`](beta.md) — guía de la beta de servidor (Fase 3).
- [`docs/desktop.md`](desktop.md) — composición y ventana T4.1.