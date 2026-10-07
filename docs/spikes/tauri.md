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

**Popouts `about:blank`.** Un `window.open('about:blank')` hereda el origen de la página que lo abre, así que sus pedidos son same-origin: llevan la cookie y el motor los acepta, igual que Electron firma los portales. En #6 se verificó que wry conserva el `opener` (ver más abajo).

### Popouts, desks temporales y pins (#6)

El renderer abre sus ventanas con `window.open('', nombre, features)` desde la ventana dueña. Escribe un documento vacío en el hijo, le clona los `<style>` y `<link>` del dueño y mueve ahí el DOM con portales de React (`renderer/src/popout.tsx`). Para que funcione, la ventana nueva tiene que quedar unida a la que la abrió, en el mismo contexto de JavaScript.

En Tauri 2.12 eso se resuelve con `on_new_window` → `NewWindowResponse::Create`:

- wry llama `SetNewWindow` de WebView2, que es el mecanismo para que `window.open` devuelva el `WindowProxy` del hijo con el `opener` conservado.
- `window_features(features)` le pasa al hijo el mismo entorno de WebView2 que el dueño, que es lo que `SetNewWindow` exige, y además la posición y el tamaño pedidos.
- Sin handler, wry bloquea `window.open`.
- Solo se abren `about:blank` y `''`. Cualquier otra URL se rechaza, y los links externos irán al navegador del sistema más adelante.

**Cerrar la ventana dueña con popouts abiertos la oculta** en lugar de destruirla, porque los popouts viven en su contexto de JavaScript. Electron hace lo mismo ("Main close preserves all popouts"). Cuando se cierra el último popout con la dueña oculta, la app termina; la bandeja llega en #7.

**Verificado en WebView2** con la prueba de `ORGTREE_TAURI_PROBE` (`src-tauri/src/probe.js`), que repite la secuencia de `popout.tsx`:

- `window.open` devuelve la ventana, el `opener` se conserva y el origen es el mismo;
- un `<style>` y un `<link>` clonados del dueño se aplican en el hijo;
- un borrador creado en el dueño, con su listener, se mueve al hijo y conserva el texto, y escribir en el hijo dispara el listener del dueño;
- después de pedir el cierre de la dueña, esta queda oculta, el popout sigue abierto y el borrador sigue ahí.

En Linux (WebKitGTK), `window.open` sin un gesto del usuario queda bloqueado y Tauri no expone el ajuste. Linux no es destino, así que no se trabajó.

### Ventana de inicio y `window.orgtreeDesktop` (#4)

El renderer React se usa **sin modificar**: `npx vite build apps/desktop/renderer --base / --outDir ../../../dist/renderer`, igual que `tools/build.mjs`, y lo sirve el motor (`ORGTREE_TAURI_UI_DIR` → `ORGTREE_V2_UI_DIR`).

Ventanas:

- `splash` (local, `ui/index.html`): muestra el estado del arranque. Solo tiene la capability `default`.
- `main`: se crea cuando el motor está listo, directo en su origen. Se crea recién ahí porque su shim lleva el **origen exacto** del motor, que antes no se conoce. Electron le pasa el mismo dato al preload por argv. `splash` se cierra cuando `main` termina de cargar: cerrarla antes termina la app, porque Tauri todavía no registró `main`.

El shim (`src-tauri/src/shim.js`) es un `initialization_script` de `main`:

- Expone `window.orgtreeDesktop` con el contrato de `apps/desktop/preload/index.ts` sobre `invoke`.
- Igual que el preload, solo existe en el frame principal y en el origen exacto del motor: los iframes y los popouts no lo tienen.
- Los métodos opcionales que el recorte no cubre (`requestOrg`, ventanas múltiples, popouts nativos, login de proveedores) se omiten a propósito. El renderer ya tiene un camino para cuando faltan, el mismo de un navegador: abre la org en la misma ventana.
- Los obligatorios fuera del recorte rechazan con un error claro.

Seguridad del puente:

- `build.rs` declara un `AppManifest` con los 12 comandos `desktop_*`.
- La capability `engine-ui` (`capabilities/engine-ui.json`) los concede solo a `main` con `remote.urls` `http://127.0.0.1:*/*`, sin `core:default`, shell, fs ni process.
- Además, cada comando verifica en Rust que lo llama `main` y que su URL está en el origen exacto del motor (`src-tauri/src/desktop.rs`).

**Motor de fixture** (`apps/desktop-tauri/fixture-engine/launch.py`): el motor real con una org sembrada (`spike-fixture`, con un agente `worker` y su historial, que incluye una herramienta) y sin proveedores, siguiendo `tests/test_engine_http.py`. El perfil (`~/.claude`) queda en una carpeta hermana de la raíz descartable. En Windows corre `launch.main()`; en Linux sirve la misma app con uvicorn, solo para desarrollar.

**Verificado en WebView2** con la prueba de `ORGTREE_TAURI_PROBE`:

- el shim existe y responde por `invoke` (`getAppVersion`, `getStatus`, `getPreferences`);
- un método fuera del recorte rechaza con un error claro;
- el shim no aparece en iframes ni en popouts;
- la página de inicio del renderer lista `spike-fixture`, y al abrirla navega a `/o/spike-fixture` y muestra al agente.

Reutilización del renderer: **100 %**. Ningún archivo de `apps/desktop/renderer` cambió (61.000 líneas TSX y 8.700 de CSS).

### Desk en vivo y recuperación del motor (#5)

El desk es el del renderer, sin cambios: carga `/chat`, se conecta al WebSocket de la org (autenticado por la cookie) y aplica los frames `node_stream`. Para probarlo sin proveedores, el motor de fixture acepta dos variables:

- `ORGTREE_FIXTURE_LIVE=1`: emite cada 1,5 s un frame `node_stream` de tipo `delta` del agente (`supervisor.stream`), igual que un turno en curso.
- `ORGTREE_FIXTURE_MESSAGES=N`: agrega N mensajes al historial del agente. El CI usa 1.200.

**Recuperación.** Un vigilante en Rust (`watch_engine`) revisa el proceso del motor cada segundo. Si se cae:

1. lo vuelve a lanzar con las mismas opciones;
2. guarda la cookie nueva (el token cambia en cada arranque);
3. recarga la ventana, que reconecta su WebSocket.

Reintenta durante unos 2 minutos. En Linux el puerto guardado queda en `TIME_WAIT` unos 60 s y el motor lo rechaza como ocupado (`engine/launch.py`, `_port`), así que la recuperación tarda eso. Si el puerto cambiara, el shim de `main` quedaría con el origen viejo. El motor persiste el puerto precisamente para que no pase, y el spike no cubre ese caso.

**Verificado en WebView2** con la prueba de `ORGTREE_TAURI_PROBE`:

- el desk abre y el texto de los frames en vivo crece;
- la conversación se carga entera (del mensaje 1 al 1200, con el chip de `Read README.md`);
- se miden los cuadros de un scroll de punta a punta, y el resultado queda en el resumen del run;
- el botón real "Open in new window" saca el desk a otra ventana, con la misma tipografía y los mensajes, y el borrador escrito ahí vuelve al dueño al cerrarla;
- tras matar el motor, el vigilante lo reinicia y la página recargada vuelve a autenticarse (`fetch` 200, WebSocket abierto) y a mostrar al agente.

El CI sube capturas de pantalla del desk y de la ventana recuperada (artefacto `orgtree-tauri-screens`).

El workflow `.github/workflows/spike-tauri.yml` hace lo mismo en `windows-latest` en cada push a `spike/tauri`: compila, verifica que la ventana arranque y siga abierta 15 segundos, informa tamaños en el resumen del run y sube el instalador como artefacto `orgtree-tauri-installer`.

La app se instala por usuario con su propio identificador (`com.kushro.orgtree.tauri-spike`), así que no pisa una instalación de Orgtree existente.
