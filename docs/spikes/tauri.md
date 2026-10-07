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

### Autenticación por cookie (#3)

Electron firma cada pedido al motor con el header `X-Orgtree-Desktop-Token` (`session.webRequest.onBeforeSendHeaders`). WebView2 no deja agregar headers al handshake de un WebSocket (WebView2Feedback#4303), así que el shell Tauri entrega el mismo token por boot como cookie:

1. Cuando el motor está listo, Rust llama `set_cookie` desde el hilo de arranque (nunca desde un comando sincrónico: wry#583) con `orgtree_desktop_token=<token>; Domain=127.0.0.1; Path=/; HttpOnly; SameSite=Strict`. En WebView2, un `Domain` sin punto inicial deja la cookie host-only, y `HttpOnly` la oculta del JavaScript de la página. Como no tiene vencimiento, muere con el proceso del webview.
2. Recién después navega la ventana al origen exacto del motor. `on_navigation` solo deja ir a la página local de arranque, a `about:blank` y a ese origen.

`TokenGate` (`engine/launch.py`) acepta el header **o** la cookie y compara las dos en tiempo constante (`hmac.compare_digest`). Como el navegador separa las cookies por host y no por puerto, la cookie solo vale con prueba de que el pedido sale de la página del motor:

| Pedido | Cookie aceptada si… |
|---|---|
| WebSocket | `Origin` es exactamente `http://127.0.0.1:<puerto del motor>` |
| HTTP | no trae un `Origin` ajeno y `Sec-Fetch-Site` es `same-origin`, `none` o falta |

Además, cualquier WebSocket con un `Origin` ajeno se rechaza con 4401, traiga la credencial que traiga. Si el `Origin` falta, solo pasa con el header (clientes que no son navegadores). Electron y los agentes no cambian: siguen usando el header. Tests: `tests/test_desktop_cookie_auth.py`.

**Verificado en el webview real.** Con `ORGTREE_TAURI_PROBE=<archivo>`, la app inyecta en la página del motor una prueba (`src-tauri/src/probe.rs`) y escribe el resultado; el CI la corre en WebView2 y falla si algo no se cumple:

- la navegación, `fetch('/api/desktop/identity')` y el WebSocket se autentican;
- un iframe `sandbox="allow-scripts"` (como el HTML de agentes, `canvas/htmlresponse.ts`) tiene origen `null` y **no manda** la cookie;
- control positivo: la página del motor sí la manda a otro puerto de `127.0.0.1`.

Ese control positivo es un riesgo a tener en cuenta: **la cookie viaja a cualquier puerto de `127.0.0.1`** en los pedidos que haga la página del motor. Electron, en cambio, solo firma el origen exacto. El renderer no pide nada a otros puertos locales, y el HTML de agentes corre en iframes con origen opaco, que no la mandan. Pero cualquier contenido que se cargue sin sandbox en la ventana del motor podría filtrar el token a un servidor local. Para mitigarlo, el contenido de agentes tiene que quedar siempre en iframes sandbox o ventanas aparte (#6).

**Popouts `about:blank`.** Un `window.open('about:blank')` hereda el origen de la página que lo abre, así que sus pedidos son same-origin: llevan la cookie y el motor los acepta, igual que Electron firma los portales. Queda por verificar en #6 si el handler de ventanas nuevas de wry conserva el `opener`.

El workflow `.github/workflows/spike-tauri.yml` hace lo mismo en `windows-latest` en cada push a `spike/tauri`: compila, verifica que la ventana arranque y siga abierta 15 segundos, informa tamaños en el resumen del run y sube el instalador como artefacto `orgtree-tauri-installer`.

La app se instala por usuario con su propio identificador (`com.kushro.orgtree.tauri-spike`), así que no pisa una instalación de Orgtree existente.
