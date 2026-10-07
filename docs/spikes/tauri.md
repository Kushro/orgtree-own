# Spike: migración de Electron a Tauri 2

Prueba técnica acotada para decidir si Orgtree sale de Electron hacia Tauri 2. Se compara contra el spike de Dioxus (rama `spike/dioxus`) en el issue de decisión #15.

## Alcance

Se reemplaza solo la capa de escritorio (`apps/desktop/main` y `preload`). El renderer React se reutiliza; el motor Python y PostgreSQL no se migran.

| # | Issue | Qué valida |
|---|---|---|
| 1 | #1 Scaffold de Tauri 2 y CI de Windows | Build e instalador NSIS en `windows-latest` |
| 2 | #2 Supervisión del motor Python desde Rust | Arranque, `ready`, apagado limpio |
| 3 | #3 Autenticación con cookie HttpOnly y ajuste de TokenGate | Token sin inyección de headers (WebView2 no los agrega al WebSocket) |
| 4 | #4 Ventana de inicio con lista de organizaciones | Shim de `window.orgtreeDesktop` sobre `invoke` |
| 5 | #5 Ventana de organización con desk en vivo | Feed WebSocket y fluidez |
| 6 | #6 Popouts con `window.open` y portales de React | **Riesgo principal** del spike |
| 7 | #7 Ventana sin marco, bandeja y notificación | Integración nativa básica |

## Decisiones de diseño ya tomadas

- **La lógica sensible vive en Rust**, no en el webview: lanzar el motor, el token, la elevación y el updater. El renderer muestra HTML y Markdown de agentes, así que el webview no recibe capacidades de shell, fs ni process.
- **Autenticación por cookie** `HttpOnly` y `SameSite=Strict`, más verificación de `Origin` en el WebSocket. El motor acepta el header o la cookie, así que Electron y los agentes siguen funcionando igual.
- **Mismo contrato de `window.orgtreeDesktop`** que el preload actual, para que el renderer casi no cambie y se puedan seguir trayendo cambios de upstream.

## Fuera del alcance

Tarea de arranque del sistema, instalación para todos los usuarios, upgrade desde instalaciones Electron, empaquetado del runtime de Python y PostgreSQL, updater firmado.

## Cómo compilar

El proyecto vive en `apps/desktop-tauri` y no reemplaza a `apps/desktop` (Electron): conviven durante el spike.

Requisitos en Windows: Rust estable (MSVC), Node 22 y WebView2 (viene con Windows 11).

```powershell
cd apps/desktop-tauri
npm ci
npx tauri dev     # ventana de desarrollo
npx tauri build   # instalador NSIS en src-tauri/target/release/bundle/nsis/
```

### Motor Python (#2)

La app lanza `engine/launch.py` al abrir y lo apaga al salir. El supervisor vive en el crate `apps/desktop-tauri/engine-host`, sin dependencia de Tauri, para poder probarlo solo y reutilizarlo en el spike de Dioxus. Como el empaquetado del runtime queda fuera del spike, el intérprete se indica por entorno:

| Variable | Qué es | Por defecto |
|---|---|---|
| `ORGTREE_TAURI_PYTHON` | Python absoluto con `tools/runtime-requirements.in` instalado | obligatoria |
| `ORGTREE_TAURI_ENGINE_DIR` | Carpeta con `launch.py` | `engine/` del checkout donde se compiló |
| `ORGTREE_TAURI_DATA` | Raíz de datos | `%LOCALAPPDATA%\com.kushro.orgtree.tauri-spike\data` |

Nunca apuntar `ORGTREE_TAURI_DATA` a la raíz real de Orgtree (`%APPDATA%\Orgtree v2\data`).

```powershell
python -m pip install -r tools/runtime-requirements.in
$env:ORGTREE_TAURI_PYTHON = (Get-Command python).Source
cd apps/desktop-tauri/engine-host
$env:ORGTREE_TEST_ENGINE_PYTHON = $env:ORGTREE_TAURI_PYTHON
cargo test -- --include-ignored   # motor falso y motor real
```

El workflow `.github/workflows/spike-tauri.yml` hace lo mismo en `windows-latest` en cada push a `spike/tauri`: compila, verifica que la ventana arranque y siga abierta 15 segundos, informa tamaños en el resumen del run y sube el instalador como artefacto `orgtree-tauri-installer`.

La app se instala por usuario con su propio identificador (`com.kushro.orgtree.tauri-spike`), así que no pisa una instalación de Orgtree existente.
