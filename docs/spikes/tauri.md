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
| 8 | #18 Instalador autónomo con motor, runtime y PostgreSQL | Instalador que se usa sin preparar nada |
| 9 | #21 Integraciones: harnesses, login de proveedores, notificaciones y archivos | Contratar agentes con el instalador |

## Decisiones de diseño ya tomadas

- **La lógica sensible vive en Rust**, no en el webview: lanzar el motor, el token, la elevación y el updater. El renderer muestra HTML y Markdown de agentes, así que el webview no recibe capacidades de shell, fs ni process.
- **Autenticación por cookie** `HttpOnly` y `SameSite=Strict`, más verificación de `Origin` en el WebSocket. El motor acepta el header o la cookie, así que Electron y los agentes siguen funcionando igual.
- **Mismo contrato de `window.orgtreeDesktop`** que el preload actual, para que el renderer casi no cambie y se puedan seguir trayendo cambios de upstream.

## Fuera del alcance

Tarea de arranque del sistema, instalación para todos los usuarios, upgrade desde instalaciones Electron, updater firmado. El empaquetado del runtime de Python y PostgreSQL quedó fuera al principio y se agregó en #18.

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

La app lanza `engine/launch.py` al abrir y lo apaga al salir. El supervisor vive en el crate `apps/desktop-tauri/engine-host`, sin dependencia de Tauri, para poder probarlo solo y reutilizarlo en el spike de Dioxus. En desarrollo, el intérprete se indica por entorno; la app instalada usa el que trae el paquete (ver #18 más abajo):

| Variable | Qué es | Por defecto |
|---|---|---|
| `ORGTREE_TAURI_PYTHON` | Python absoluto con `tools/runtime-requirements.in` instalado | obligatoria sin el paquete |
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

**`window.close()` desde JS.** El renderer cierra así un popout al volver a acoplarlo. En Tauri 2.12, WebView2 dispara `WindowCloseRequested` y wry destruye solo el HWND contenedor del webview, no la ventana de Tauri, que queda vacía. Esto apareció en una captura del CI. Para rodearlo, un vigilante en el shell (`reap_closed_popouts`) detecta el popout que ya no tiene su contenedor (`FindWindowExW` con la clase `WRY_WEBVIEW`) y destruye su ventana. Mirar `url()` no sirve: el controlador de WebView2 sigue respondiendo después del cierre (lo mostró el CI). Mientras se crea, un popout todavía no tiene contenedor, así que solo se cierra uno al que ya se le vio: destruirlo a mitad de la creación trabó la página en un run. La prueba verifica que, después de acoplar el desk, solo queda abierta la ventana del popout de la primitiva.

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
- Los métodos opcionales que el recorte no cubre (`requestOrg`, ventanas múltiples, popouts nativos) se omiten a propósito (el login de proveedores llegó en #21). El renderer ya tiene un camino para cuando faltan, el mismo de un navegador: abre la org en la misma ventana.
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

### Integración nativa (#7)

**Ventana sin marco.** `main` y los popouts usan `decorations(false)`, como `frame: false` en Electron.

- El renderer ya dibuja sus botones (`WindowControls`), que llegan por el shim (`minimizeWindow`, `toggleMaximizeWindow`, `closeWindow`, `getWindowControlsState`).
- El shell manda eventos `window-state` al cambiar el tamaño o el foco, para que el ícono de maximizar y restaurar siga el estado real.
- El arrastre usa el `-webkit-app-region` del CSS del renderer **sin cambios**. WebView2 lo respeta porque wry activa `IsNonClientRegionSupportEnabled`, así que no hizo falta `data-tauri-drag-region` ni `startDragging`.
- tao deja `WS_CAPTION` en la ventana sin marco (para el snap y las animaciones de Windows) y quita la barra de título. Queda el borde de redimensionado invisible de Windows, 8 px por lado, dentro del rectángulo de la ventana (1044x788 contra 1028x779 de área cliente). Por eso la prueba busca una barra de título real, de unos 30 px, en lugar de mirar el estilo.

**Bandeja.** Feature `tray-icon`, con el menú Abrir Orgtree / Salir. Un clic en el ícono abre la ventana.

- Con `exitOnClose` apagado (el valor por defecto, como en Electron), cerrar `main` la oculta y la app sigue en la bandeja.
- Con `exitOnClose` prendido, `main` se cierra, salvo que haya popouts abiertos.

**Notificación.** `orgtreeDesktop.notify` llama a `desktop_notify` (`tauri-plugin-notification`), con el filtro mínimo de `NotificationGate`: campos obligatorios, `notificationsEnabled` y una sola vez por org + id.

- El filtro por tipo, el filtro con la ventana enfocada, el clic que abre el elemento y `syncNotifications` llegaron en #21 (más abajo).
- Desde `target/release`, el toast usa el AppUserModelID de PowerShell. La app instalada usa el suyo.

**Instancia única.** `tauri-plugin-single-instance`: una segunda ejecución termina antes de crear ventanas o lanzar el motor, y la primera muestra y enfoca `main`.

**Verificado en WebView2** con la prueba y el CI:

- `main` no tiene marco y el ícono de la bandeja existe;
- los botones del renderer se ven, y maximizar y restaurar llegan con sus eventos `window-state` (el ícono cambia a Restore);
- una segunda ejecución termina sola, y la ventana existente queda enfocada y al frente;
- el CI arrastra la ventana con el mouse real desde la zona de arrastre del renderer (`.native-header-main`), y la ventana se mueve lo mismo que el mouse (dx 120, dy 60).

Escalado a 125 % y 150 %: el runner de CI corre al 100 % y no se puede cambiar sin cerrar la sesión. Lo prueba una persona en Windows, con las métricas de tamaño, RAM y arranque.

### Instalador autónomo (#18)

El instalador NSIS trae lo mismo que el de Electron (`build.extraResources` en `package.json`), así que una persona lo instala y lo usa sin preparar nada:

- el motor `engine/`, con el submódulo `engine/mailhub`, sin `__pycache__`, `.git` ni `native/**/target`;
- el runtime de Python 3.13 embebido con sus dependencias (`tools/provision-runtime.py`);
- PostgreSQL 18.6 y `pg-custodian.exe` (`tools/provision-postgres.py`);
- `tools/pypg/pgimport.py` y `cutover_verify.py`;
- el renderer construido, en `ui/`, y un `build-info.json` con el commit.

`bundle.resources` de Tauri no admite exclusiones, así que `tools/stage-resources.py` arma esa carpeta en `bundle-resources/` y `src-tauri/tauri.bundle.conf.json` la agrega al build:

```powershell
python tools/provision-runtime.py          # desde la raíz del repo, en Windows
python tools/provision-postgres.py
npx vite build apps/desktop/renderer --base / --outDir ../../../dist/renderer --emptyOutDir
cd apps/desktop-tauri
npm run build:standalone                   # stage-resources.py + tauri build --config src-tauri/tauri.bundle.conf.json
```

`tools/verify-resources.mjs` aplica los controles del empaquetado de Electron (`assertRuntimeLayout` y `assertPostgresRuntime`, cada byte de PostgreSQL contra su manifiesto). Con `--compare`, exige además que el motor instalado sea idéntico, archivo por archivo, al armado.

**Modo empaquetado.** Sin `ORGTREE_TAURI_PYTHON` y con `<recursos>/engine/runtime/python.exe` presente, el shell lanza el motor como `apps/desktop/main/index.ts` con `app.isPackaged`:

- el Python embebido y el `ui/` del paquete (`ORGTREE_V2_UI_DIR`);
- `ORGTREE_PG_BOOTSTRAP=1`, `ORGTREE_PG_CUSTODIAN` y `ORGTREE_P03_PG_BIN`, como `postgres-runtime.ts`;
- el descriptor `engine-paths.json` en `%LOCALAPPDATA%\com.kushro.orgtree.tauri-spike`, como `writeEnginePaths`.

Lo implementa el módulo `packaged` de `engine-host`. Un paquete al que le falta una pieza da un error en la ventana de arranque y no cae al modo de desarrollo. Con `ORGTREE_TAURI_PYTHON`, el modo de desarrollo sigue igual y lo usa el smoke test.

**Raíz de datos.** Sigue siendo la de la app (`%LOCALAPPDATA%\com.kushro.orgtree.tauri-spike\data`, o `ORGTREE_TAURI_DATA`). La ventana de arranque la muestra con el modo, y el ícono de la bandeja también, en su tooltip. El supervisor rechaza una raíz que se superpone con las de Orgtree instalado (`%APPDATA%\Orgtree v2`, `%LOCALAPPDATA%\Programs\Orgtree`, `~/orgtree`, `ORGTREE_DATA`). Hace falta porque el motor, en modo producto, sí acepta la raíz real: es la suya.

**Tres trampas del empaquetado**, todas visibles solo en Windows:

- Tauri da la carpeta de recursos con el prefijo `\\?\`. Con una ruta así, el Python embebido no resuelve las entradas relativas de su `._pth` (`../backend`, `../../`), porque Windows no normaliza `..` en rutas literales, y el motor termina antes de `ready`. `PackagedRuntime::locate` quita el prefijo.
- libpq prefiere `PGPASSWORD` al `passfile` de `pg-custodian`. Los runners de GitHub traen un PostgreSQL propio con `PGPASSWORD=root`, y las migraciones fallaban con "password authentication failed for user orgtree_admin". El supervisor ya no le pasa al motor las variables de libpq (`PGPASSWORD`, `PGUSER`, `PGHOST`, etc.). Electron tiene el mismo riesgo con cualquier usuario que tenga esas variables.
- En el runner, `pg-custodian init` se negaba con `acl.not_owner_only` sobre su propia carpeta de secretos, con DACL `O:LAD:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)(A;OICI;FA;;;LA)`. El usuario del runner (`runneradmin`) es la cuenta Administrador integrada (RID 500), cuyo SID Windows abrevia `LA` en SDDL, y `prototype-guard` (`acl.rs`) lo compara con el SID completo. Es un problema del producto con esa cuenta, no del token elevado: PostgreSQL arrancaba bien con el token restringido de `pg-custodian`. No se cambió el motor. El CI corre la app instalada como un usuario local estándar creado para la prueba, como la usaría una persona.

**Verificado en el CI** (paso "Install and run the standalone installer"):

- el instalador se instala en silencio (`/S /D=<carpeta en RUNNER_TEMP>`), y el motor instalado es idéntico al armado;
- la app instalada arranca como usuario estándar, sin `ORGTREE_TAURI_PYTHON`, en modo `packaged`, con la raíz descartable que se le pidió;
- la raíz se crea sobre PostgreSQL (`store-backend.json` con `postgres`, el cluster en `<raíz>\pg\cluster\data`), y corren `postgres.exe` y el Python embebido desde la carpeta instalada;
- `engine-paths.json` describe la instalación;
- en la página del motor, la navegación y `fetch` se autentican, `window.orgtreeDesktop` responde, el renderer se dibuja, y una org creada por la API (`instalado`) aparece en la lista y en la pantalla;
- las capturas `installed-splash` e `installed-app` (artefacto `orgtree-tauri-screens`) muestran la ventana de arranque con la raíz y la app cargada.

Números del CI: instalador **50,8 MB** (LZMA), exe **4,4 MB**, unos **227 MB** instalada. De los 222 MB armados, 141 son de PostgreSQL y 63 del runtime de Python. Con una raíz nueva, la primera página está lista en unos **15 s**, incluido `initdb`.

**Pre-release.** Un `workflow_dispatch` con `prerelease: true` agrega el job `prerelease`. Publica el instalador que el job de Windows acaba de probar como pre-release `tauri-preview-<run_number>`, sobre el commit del run, con notas en español. No toca `main` ni los releases normales.

### Integraciones (#21)

Paridad con `harnesses.ts`, `providerlogin.ts`, `notifications.ts` y `taskbar-attention.ts`, para que el instalador sirva para contratar agentes de verdad. Todo vive en el shell (`src-tauri/src/harnesses.rs`, `login.rs`, `notifications.rs`, `files.rs`), detrás de 9 comandos nuevos: cada uno está en el `AppManifest` de `build.rs`, en `capabilities/engine-ui.json`, pasa por `authorize()` y tiene su método en el shim con la firma del contrato.

**Harnesses.** `getHarnesses` busca `claude`, `codex` y `agy` como Electron: en el `PATH` del proceso (con `.exe`, `.cmd` y `.bat`), en `ORGTREE_CODEX` y `ORGTREE_ANTIGRAVITY`, y en la carpeta del instalador de Antigravity. Solo informa presencia: nunca lanza ni instala nada. `openHarnessLink` abre en el navegador del sistema (`ShellExecuteW`) la URL fija del harness; la página solo elige el id, y cualquier otro valor se rechaza. La bandeja suma el submenú "Harness setup" de Electron.

Lo que decide si un proveedor se puede contratar no es `getHarnesses`: el renderer lee `/api/providers` del motor, que hereda el `PATH` del shell. Con el motor del paquete eso ya funcionaba; esta lista la usan la bandeja y el login.

**Login de proveedores.** `startProviderLogin`, `getProviderLoginStatus`, `submitProviderLoginCode` y `cancelProviderLogin`, con el mismo ciclo de estados (`starting`, `awaiting_code`, `done`, `error`). Reglas de seguridad:

- solo se ejecuta el CLI de un harness **detectado por el shell**, con los argumentos fijos de Electron (`claude auth login --claudeai`, `codex login`, y `agy` en una consola visible); el proveedor se valida contra la lista y nada de la página llega a la línea de comandos;
- `profileDir` solo elige la carpeta de perfil (`CLAUDE_CONFIG_DIR` o `CODEX_HOME`), tiene que ser absoluta, existir y aceptar escritura; `accountId` solo admite letras, números, `-` y `_`;
- el token del escritorio no llega al hijo: el shell no lo tiene en su entorno (va al motor por `ORGTREE_V2_TOKEN`, solo en su proceso) y además lo quita del entorno del hijo, junto con las variables de la prueba;
- el código pegado va solo al stdin del hijo, nunca a la salida ni a un registro;
- cancelar, el tiempo máximo (5 min) y salir de la app matan el árbol entero con `taskkill /T` (un `.cmd` corre bajo `cmd.exe`);
- un login que termina bien se verifica contra el motor (`/api/providers?force=true&force_provider=…` o la identidad de la cuenta), con el header del token, como Electron.

**Notificaciones.** `notify` aplica el filtro completo de `NativeNotifications`: la validación de campos y tipos, las preferencias por tipo (`packages/contracts/notifications`, ahora en las preferencias del shell), el filtro con una ventana de Orgtree enfocada (`notifyWhileFocused`), el conjunto activo de `syncNotifications` y una sola vez por `org` + `id`. Un tipo que se apaga retira sus toasts.

El plugin de notificaciones no da el clic en escritorio ni deja retirar un toast, así que en Windows el shell arma el toast con WinRT (crate `windows`, la misma versión que ya traen Tauri y wry), con tag y grupo propios. El clic (`Activated`) muestra la ventana y entrega `notification-click`, el evento que el renderer espera para abrir el elemento; el shim lo guarda si llega antes que el primer listener. `syncNotifications` retira con `ToastNotificationHistory` los toasts cuyo elemento ya se resolvió.

**Barra de tareas.** `setPendingAttention(ids, items)` valida la carga como `attentionPayload` y hace parpadear el botón (`request_user_attention`, con `FLASHW_ALL | FLASHW_TIMERNOFG`, hasta que la ventana pasa al frente) solo cuando llega una identidad nueva. Un sondeo con la misma lista no lo reinicia, nunca parpadea la ventana que el usuario está mirando, y la lista vacía lo detiene.

**Archivos.** `revealFile` exige una ruta absoluta, sin `.` ni `..`, sin comillas y que exista, y abre `explorer /select,"<ruta>"`: el Explorador muestra el archivo seleccionado y nunca lo abre ni lo ejecuta (decisión del usuario, 2026-09-13). `openCharterFolder` crea `~/.orgtree/charters` si falta y la abre.

**Verificado en WebView2 y el CI** (paso de smoke test, con el renderer real):

- `getHarnesses` tiene la forma del contrato, y un `codex.cmd` falso en el `PATH` de la app aparece detectado;
- el login de un proveedor no detectado se niega con `not-installed`, uno inventado se rechaza, y el de `codex` arranca el CLI falso con `login`, no deja arrancar otro, rechaza un código, se cancela y vuelve a `idle`; el CLI falso confirma que no recibió el token ni la variable de la prueba, y no llega a terminar (el árbol murió);
- el motor de fixture publica avisos sintéticos (`PUT /api/fixture/notices`) y el **renderer real** los convierte en notificaciones: la pregunta se muestra sin foco, el correo de rutina recién cuando se prende "todo el correo" (antes el shell lo filtra como inactivo, porque el renderer solo sincroniza los avisos habilitados), una pregunta con la ventana enfocada recién con `notifyWhileFocused`, y un aviso resuelto retira su toast. Las llamadas directas confirman la deduplicación, el filtro de inactivos y la validación de campos. El shell anota cada decisión y el CI la compara;
- el parpadeo empieza con la primera llegada (ventana minimizada), no se reinicia con la misma lista y para cuando no queda nada, según el registro del shell. En la captura `taskbar-attention` no se distingue el botón resaltado;
- el clic en el toast, simulado con la misma función que llama su handler (el CI no puede hacer clic en el centro de notificaciones), muestra la ventana minimizada con el foco, y el renderer recibe `notification-click` y abre su bandeja de entrada en el elemento, como en Electron (captura `notification-click`; el aviso es sintético, así que la bandeja dice que no lo encuentra). Va en una etapa final porque la bandeja tapa la zona de arrastre;
- `revealFile` rechaza una ruta relativa, una inexistente y una con `..`; en la etapa final, con un `.cmd` existente, el Explorador se abre en su carpeta (`Shell.Application`) y el `.cmd` no se ejecuta. La selección no se pudo leer en el runner: `Shell.Application` devuelve `SelectedItems` vacío, y en la captura `reveal-file` la carpeta (`D:\a\_temp`, con muchos archivos) todavía está cargando. `openCharterFolder` abre su carpeta y `openHarnessLink('codex')` abre Edge con el enlace oficial (captura `integrations`);
- pruebas unitarias del shell (`cargo test` en `src-tauri`) en Windows.

Fuera del recorte: el diálogo de Electron que avisa en el primer arranque si no hay ningún harness, y el login real contra un proveedor, que necesita una cuenta y lo prueba una persona.
