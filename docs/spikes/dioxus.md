# Spike: migración de Electron + React a Dioxus

Prueba técnica acotada para decidir si Orgtree sale de Electron hacia Dioxus. Se compara contra el spike de Tauri 2 (rama `spike/tauri`) en el issue de decisión #15.

## Alcance

Se reemplazan la capa de escritorio y el renderer React por Dioxus. El motor Python y PostgreSQL no se migran.

| # | Issue | Qué valida |
|---|---|---|
| 1 | #8 Scaffold de Dioxus desktop y CI de Windows | Build e instalador en `windows-latest` |
| 2 | #9 Supervisión del motor Python desde Rust | Arranque, `ready`, apagado limpio |
| 3 | #10 Cliente del motor en Rust (HTTP y WebSocket) | Token como header desde Rust, sin cambios en el motor |
| 4 | #11 Ventana de inicio con lista de organizaciones en RSX | Primera vista reescrita |
| 5 | #12 Desk de agente en vivo en RSX | **Riesgo principal**: costo de reescribir la pieza más pesada |
| 6 | #13 Multi-ventana nativa | Ventanas separadas con estado compartido |
| 7 | #14 Ventana sin marco, bandeja y notificación | Integración nativa básica |
| 8 | #24 Instalador autónomo con motor, runtime y PostgreSQL | Instalar y usar sin preparar nada |

## Decisiones de diseño ya tomadas

- **Todas las llamadas al motor salen desde Rust.** La UI no carga desde el motor, así que el token viaja como header en HTTP y en el handshake del WebSocket y el motor no cambia.
- **Markdown seguro en Rust** (`pulldown-cmark` + `ammonia` o equivalente) en lugar de `marked` + DOMPurify.
- **Estilos:** se evalúa en #8 si se reutiliza el CSS actual o se adopta rust-ui con Tailwind.

## Fuera del alcance

Canvas del organigrama, docket, mail y demás vistas; tarea de arranque del sistema, instalación para todos los usuarios, upgrade desde instalaciones Electron, updater. El empaquetado del runtime de Python y PostgreSQL entró con #24.

## Cómo compilar

El proyecto vive en `apps/desktop-dioxus` y no reemplaza a `apps/desktop` (Electron): conviven durante el spike.

Requisitos en Windows: Rust estable (MSVC), WebView2 (viene con Windows 11) y la CLI de Dioxus en la misma versión que el crate (`0.7.10`).

```powershell
cargo binstall dioxus-cli@0.7.10   # o: cargo install dioxus-cli --version 0.7.10
cd apps/desktop-dioxus
dx serve --platform desktop        # ventana de desarrollo
dx bundle --platform desktop --package-types nsis --release --out-dir dist
```

### Motor Python (#9)

La app lanza `engine/launch.py` al abrir y lo apaga al salir (`Event::LoopDestroyed` en `with_custom_event_handler`). El supervisor es el crate `apps/desktop-dioxus/engine-host`, copia del de `spike/tauri` (`apps/desktop-tauri/engine-host`): no depende del framework, así que es el mismo código en los dos spikes. Es miembro del workspace de la app. El intérprete se indica por entorno:

| Variable | Qué es | Por defecto |
|---|---|---|
| `ORGTREE_DIOXUS_PYTHON` | Python absoluto con `tools/runtime-requirements.in` instalado (modo de desarrollo) | sin ella, el motor instalado (#24) |
| `ORGTREE_DIOXUS_ENGINE_DIR` | Carpeta con `launch.py` | `engine/` del checkout donde se compiló |
| `ORGTREE_DIOXUS_DATA` | Raíz de datos | `%LOCALAPPDATA%\com.kushro.orgtree.dioxus-spike\data` |

Nunca apuntar `ORGTREE_DIOXUS_DATA` a la raíz real de Orgtree (`%APPDATA%\Orgtree v2\data`).

```powershell
python -m pip install -r tools/runtime-requirements.in
cd apps/desktop-dioxus
$env:ORGTREE_TEST_ENGINE_PYTHON = (Get-Command python).Source
cargo test -p orgtree-engine-host -- --include-ignored   # motor falso y motor real
```

### Cliente del motor (#10)

El crate `apps/desktop-dioxus/engine-client` es el cliente HTTP (`reqwest`, sin TLS) y WebSocket (`tokio-tungstenite`) del motor. Cada pedido y cada handshake llevan `X-Orgtree-Desktop-Token` desde Rust: no hace falta cookie ni cambio en el motor, y el token no llega al webview.

- **Rutas del recorte:** `GET /api/orgs` (ventana de inicio), `GET /api/orgs/{slug}` (árbol completo, sin `view=delta`) y `GET /api/orgs/{slug}/nodes/{nid}/chat?last=N` (desk).
- **Tipos:** derivados de `renderer/src/types.ts` (`OrgListEntry`, `TreePayload`, `TreeNode`, `ChatPayload`, `ChatMessage`, `ToolChip`). Son permisivos: lo que el recorte no lee queda en `extra`, y los nodos archivados llegan sin campos de ejecución.
- **WebSocket `/api/orgs/{slug}/ws`:** frames `changed`, `node_event`, `node_stream`, `mail` y `Other` para tipos nuevos. Manda `ping` cada 25 s, como el renderer.
- **Reconexión:** backoff exponencial de 1,5 s a 30 s con jitter, que vuelve al inicio cuando una conexión abre. El renderer reintenta cada 1,5 s fijos. Cada `Connected` indica que hay que volver a pedir el árbol, porque pudo haber frames perdidos.

### Ventana de inicio en RSX (#11)

La página de inicio está reescrita en RSX (`src/home.rs`): la tarjeta con la versión, la lista de organizaciones y los botones. Usa **las mismas clases que el renderer** (`welcome`, `welcome-card`, `nav > .org`, `org-counts`…) y su `styles.css` **sin cambios**, incluido en el binario con `include_str!`. La decisión de estilos que quedó abierta en #8 se resolvió así: reutilizar el CSS actual y escribir en RSX solo el marcado.

- Los datos vienen del cliente Rust de #10 (`GET /api/orgs`, cada 5 s como `orgstatus.ts`). El webview no tiene token ni puente.
- Abrir una org muestra sus agentes (`src/org.rs`, una vista mínima: el lienzo del organigrama queda fuera del recorte).
- Crear y borrar orgs, uso por proveedor, ajustes de la app y actualizaciones quedan fuera del recorte; sus botones aparecen deshabilitados para que el diseño coincida.
- Sin la barra de menú por defecto de Dioxus (`with_menu(None)`).

**Verificado en WebView2** con `ORGTREE_DIOXUS_PROBE` (`src/probe.rs`, con `document::eval`) y el motor de fixture (`fixture-engine/launch.py`, copia del de `spike/tauri`):

- la tarjeta y la versión aparecen;
- la lista muestra `spike-fixture`;
- la tipografía es la del CSS del renderer;
- abrir la org muestra al agente `worker`;
- en el webview no hay puente de escritorio.

El CI sube una captura del inicio para compararla con la del renderer real en Tauri.

**Líneas:** 105 de RSX (`home.rs` 75, `icons.rs` 30) frente a 130 de TSX equivalentes (`OrgRows` 66, `orgPanel` 45, `TitleBadge` 19). El TSX incluye además lo que quedó fuera del recorte.

### Desk en vivo en RSX (#12)

`src/desk.rs` reescribe lo esencial del desk (`canvas/desk.tsx` y `convo.ts`): la conversación, las herramientas y el texto en vivo. Usa las mismas clases del renderer (`msgs`, `msg user|assistant`, `tools tchip`, `tline`, `targ`, `msgtext md`), así que el CSS es el mismo.

Lógica de rendimiento que hubo que rediseñar:

| En el desk actual (React) | En RSX |
|---|---|
| `marked` + DOMPurify en cada render | `pulldown-cmark` + `ammonia` en Rust, una sola vez por mensaje al cargarlo |
| Ventana medida de `convo.ts` (filas visibles, alturas medidas, `MAX_WINDOW`) | `content-visibility: auto` en cada fila: el webview no pinta ni maqueta lo que está fuera de la vista |
| Texto en vivo en el estado de la conversación | Señal aparte (`draft`) en su propio componente: un frame `delta` no vuelve a renderizar la lista |
| Paginación con cursor y anclaje propio | La misma paginación (`before`) al llegar arriba con el scroll; el anclaje de scroll del webview mantiene la posición |
| `nudge` (refrescar `/chat` 200 ms después de un frame durable) | Un frame durable, `turn_done` o una reconexión vuelve a pedir la última página y la fusiona |
| Seguir el final mientras llega texto | Igual: si se está mirando el final, el texto en vivo lo sigue |

Fuera del recorte: pensamiento (`thinking`), filas de mail y avisos, segmentos, respuestas citadas, adjuntos y el compositor para escribirle al agente.

**Verificado en WebView2** con la prueba de `ORGTREE_DIOXUS_PROBE` y el motor de fixture (1.200 mensajes, frames en vivo y un mensaje con Markdown y un `<img onerror>` inyectado):

- el desk abre y el texto en vivo crece;
- la conversación se carga entera, del mensaje 1 al 1200;
- el chip de la herramienta `Read README.md` se ve;
- la negrita y la lista se renderizan, y el `onerror` no sobrevive;
- se miden los cuadros de un scroll de punta a punta, y el resultado queda en el resumen del run.

El CI sube capturas del inicio y del desk.

**Líneas:** `desk.rs` tiene 222 líneas de código, frente a 3.851 de `desk.tsx` y 1.262 de `convo.ts`. El desk actual hace mucho más: compositor, mail, pensamiento, archivos, respuestas citadas, popouts.

### Desk en otra ventana nativa (#13)

El botón **⧉ Pop out** del desk abre el mismo desk en una ventana nativa nueva (`src/windows.rs`, con `window().new_window(VirtualDom, Config)`). Cada ventana tiene su propio VirtualDom, carga el mismo CSS del renderer y usa el mismo cliente Rust del motor.

**Patrón de estado compartido.** Las señales de Dioxus pertenecen al runtime de su VirtualDom, así que una ventana no puede leer ni escribir las de otra. El estado compartido vive fuera de los VirtualDom, en el proceso: un mapa con los borradores y un canal `tokio::sync::broadcast`.

- Cada ventana copia el valor en una señal propia (`use_shared_draft`).
- Cada ventana publica sus cambios en el canal (`set_draft`), y las demás los reciben y re-renderizan.

Es un *store* externo con suscripción. En React, el equivalente más cercano es un `useSyncExternalStore` o un `BroadcastChannel`. Los popouts de Electron, en cambio, comparten el mismo contexto de JavaScript: el árbol de React del dueño se monta en el documento del hijo por portales.

Límites:

- Cada ventana tiene su copia. Un cambio llega a las demás en el siguiente tick del executor, no en el mismo render.
- El estado compartido tiene que ser `Clone + Send` y se copia entero en cada cambio. Sirve para borradores y selección, no para la conversación entera: cada ventana pide sus mensajes al motor y recibe su propio WebSocket.
- No hay portales: una ventana no puede renderizar dentro de otra. Cada una monta su propio árbol.
- Nada sobrevive al proceso.
- El registro de ventanas (`thread_local!`) depende de que todas corran en el hilo del event loop, como hace Dioxus.

**Cerrar la principal.** Con desks abiertos en otras ventanas, cerrar la principal la oculta (`WindowCloseBehaviour::WindowHides`) y los desks siguen vivos con su borrador. Así lo hace Electron: "Main close preserves all popouts". Al cerrar el último desk con la principal oculta, la app termina y apaga el motor. Un desk se da por cerrado cuando se suelta su VirtualDom (`use_drop`).

**Verificado en WebView2:**

- el desk abre en otra ventana con los mensajes y el CSS del renderer, sin puente;
- un borrador escrito en la principal aparece en el desk, y uno escrito en el desk aparece en la principal;
- al cerrar la principal, queda oculta y el desk sigue vivo con su borrador;
- al cerrar el desk, la app sale sola y el puerto del motor queda cerrado.

El CI sube la captura `popout` con las dos ventanas.

### Integración nativa (#14)

`src/native.rs` cubre cuatro cosas.

**Ventana sin marco.** Se crea con `with_decorations(false)`, como `frame: false` en Electron. El desk en otra ventana tampoco tiene marco.

- Los botones de minimizar, maximizar y cerrar son un componente RSX con las clases de `WindowControls` del renderer. Actúan sobre su propia ventana y siguen el estado real a través de `Resized`.
- El arrastre usa `-webkit-app-region: drag` en el título de la tarjeta de inicio (con el CSS del renderer) y en los headers de la org y del desk (`shell.css`). WebView2 lo respeta porque wry activa `IsNonClientRegionSupportEnabled`, igual que en Tauri.

**Bandeja.** Usa la de Dioxus (`init_tray_icon`), con el menú Abrir Orgtree / Salir. Un clic en el ícono muestra las ventanas.

- Cerrar la principal la oculta y la app sigue en la bandeja, como Electron con `exitOnClose` apagado, su valor por defecto.
- Salir cierra todas las ventanas. Dioxus no tiene un `exit()`: la principal pasa a `WindowCloses`, se cierran todas y, sin ventanas, el loop termina y `LoopDestroyed` apaga el motor.

**Notificación.** Usa `notify-rust`, porque Dioxus no tiene plugin de notificaciones. En Windows toma el AppUserModelID de PowerShell cuando la app no está instalada con el suyo.

**Instancia única.** Dioxus tampoco tiene plugin para esto; son unas 40 líneas propias.

- La primera instancia toma un candado de archivo junto a la raíz de datos (`File::try_lock`) y escucha en un puerto de `127.0.0.1` que guarda en otro archivo.
- Una segunda ejecución no consigue el candado, le avisa por ese puerto y termina. La primera, al recibir el aviso, muestra y enfoca su ventana.
- Cualquier proceso local puede mandar ese aviso, pero lo único que puede pedir es mostrar la ventana.

**Verificado en WebView2** con la prueba y el CI:

- la ventana no tiene barra de título. Queda el borde de redimensionado invisible de Windows, 8 px por lado, que tao cuenta dentro del rectángulo de la ventana, igual que en Tauri. Es lo mismo que `AGENTS.md` describe para Electron con escalados fraccionarios;
- los botones RSX maximizan y restauran;
- la notificación se muestra y el ícono de la bandeja existe;
- el CI arrastra la ventana con el mouse real desde el header del desk, y se mueve;
- con todas las ventanas cerradas, la app sigue viva en la bandeja;
- una segunda ejecución termina sola y vuelve a mostrar la principal;
- Salir termina la app y cierra el puerto del motor.

Escalado a 125 % y 150 %: el runner de CI corre al 100 % y no se puede cambiar sin cerrar la sesión. Lo prueba una persona en Windows, con las métricas de tamaño, RAM y arranque.

### Instalador autónomo (#24)

El instalador NSIS trae todo lo que el motor necesita, como el de Electron: se instala y se usa sin preparar nada. La app instalada no necesita `ORGTREE_DIOXUS_PYTHON`.

**Qué lleva.** Lo mismo que `build.extraResources` de `package.json`, menos el renderer React, que la UI en RSX no usa. Va en `<instalación>\resources`, con la misma disposición que `resources` en Electron:

- `engine/` con el submódulo `engine/mailhub` (el CI hace checkout con `submodules: true`);
- el runtime de Python 3.13 embebido con sus dependencias en `engine/runtime` (`tools/provision-runtime.py`);
- PostgreSQL 18.6 (`bin`, `lib` y `share`) en `engine/postgresql` y `engine/pg-custodian.exe` (`tools/provision-postgres.py`; el ZIP de ~380 MB queda en caché de `actions/cache` por el pin);
- `tools/pypg/pgimport.py` y `cutover_verify.py`, que la conversión de la primera ejecución busca junto a `engine/`;
- `build-info.json` con el commit, para que el motor sepa qué artefacto corre.

`installer/stage_resources.py` arma todo eso en `target/bundle-resources`. Deja afuera `__pycache__`, `*.pyc`, `native/**/target` y los archivos de desarrollo: `engine/docs` y las fuentes de `engine/native`, salvo `prototype-guard/live-locations.json`, que `pg_process.py` lee al correr. Antes de compilar, el CI verifica lo armado con `assertRuntimeLayout` y `assertPostgresRuntime`, las mismas funciones que usa `tools/package-preflight.mjs`.

**Cómo entra en el instalador.** `[bundle] resources` de dx 0.7.10 no sirve para un árbol: `copy_resources` copia cada entrada con `fs::copy` como un archivo suelto en la raíz, sin carpetas ni comodines. Por eso `Dioxus.toml` usa dos ajustes de NSIS:

- `template = "installer/template.nsi"`: la plantilla de dx con dos cambios, LZMA sólido (dx usa zlib) y la inclusión de los recursos dentro de la sección `Install`;
- `installer_hooks = "installer/resources.nsh"`: un `File /r` de `target/bundle-resources` a `$INSTDIR\resources`.

**Modo instalado.** Sin `ORGTREE_DIOXUS_PYTHON`, la app busca `resources\engine\launch.py` junto al ejecutable. Si está, lanza el motor como `apps/desktop/main/index.ts` y `engine.ts` cuando `app.isPackaged`. `EngineOptions::packaged` en `engine-host` verifica cada archivo y arma las opciones:

- el Python es `resources\engine\runtime\python.exe`;
- pasa `ORGTREE_PG_CUSTODIAN` y `ORGTREE_P03_PG_BIN` con los mismos archivos que revisa `postgres-runtime.ts`, y `ORGTREE_PG_BOOTSTRAP=1`, así que una raíz nueva nace en PostgreSQL;
- escribe el descriptor `engine-paths.json` (`write_engine_paths`, esquema `orgtree.engine-paths/v1`) en la carpeta propia de la app, `%LOCALAPPDATA%\com.kushro.orgtree.dioxus-spike`;
- no le pasa al motor las variables de conexión de libpq (`PGPASSWORD`, `PGUSER`…). libpq prefiere `PGPASSWORD` al passfile del custodio, y el clúster propio rechaza la conexión. Electron las hereda, así que fallaría igual en una máquina que las tenga definidas.

**Raíz de datos.** Por defecto es `%LOCALAPPDATA%\com.kushro.orgtree.dioxus-spike\data`. `ORGTREE_DIOXUS_DATA` la cambia. `engine-host` se niega a usar una raíz que esté dentro de `%APPDATA%\Orgtree v2` o de `~/orgtree`, o que las contenga (`forbidden_roots`, como `validateDataRoot` en Electron). La tarjeta de inicio y la pantalla de arranque muestran la raíz en uso y si la app es la instalada o la de desarrollo.

**Verificado en el CI** (paso "Install the NSIS installer and start the installed app"):

- instala el NSIS generado en silencio (`/S /D=<RUNNER_TEMP>\orgtree-dioxus-installed`) y revisa lo instalado: el runtime tiene el mismo digest que lo armado, PostgreSQL coincide con su manifiesto y el renderer React no está;
- arranca el exe instalado con una raíz descartable y sin `ORGTREE_DIOXUS_PYTHON`, con `ORGTREE_DIOXUS_PROBE_MODE=installed`;
- la UI RSX carga con el CSS del renderer y sin puente, la lista de orgs llega vacía del motor, y la tarjeta muestra la raíz y "app instalada";
- el motor ligó la raíz a PostgreSQL (`store-backend.json` con `fresh-bootstrap`, `orgtree-product-root.json`), creó el clúster (`pg\cluster\data\PG_VERSION` = 18) y `postgres.log` muestra que aceptó conexiones;
- `engine-paths.json` apunta al runtime, el custodio, PostgreSQL y la raíz instalados;
- al salir, la app termina sola, PostgreSQL se apaga (`postgres.log`) y no queda ningún proceso de la instalación.

Capturas: `installed-splash` (arranque, con la raíz) e `installed` (inicio de la app instalada).

**Bloqueo del runner y cómo se rodeó.** La cuenta de los runners de Windows de GitHub (`runneradmin`) es el Administrador integrado (RID 500). Windows escribe su SID como el alias `LA` en SDDL, y `parse_sddl` de `prototype-guard`, que usa pg-custodian, solo reconoce `SY` y `BA`. Entonces rechaza la carpeta de secretos del clúster recién creada (`acl.not_owner_only`). Además el runner corre elevado, y PostgreSQL no corre con un token de administrador. El CI no cambia el producto:

- crea un usuario local estándar, le da la instalación (como una instalación por usuario) y una carpeta descartable;
- arranca la app como ese usuario, con su perfil y su `LOCALAPPDATA` (`ProcessStartInfo` con credenciales).

Así corre como una persona real, sin elevación. Quien use Orgtree con la cuenta Administrador integrada tendría el mismo problema con el instalador de Electron: queda anotado para el motor (`engine/native/prototype-guard/src/acl.rs`).

**Pre-release.** `workflow_dispatch` con `prerelease: true` agrega el job `prerelease`: baja el instalador del run y lo publica con `gh release create --prerelease` como `dioxus-preview-<run_number>`, sobre el sha del run, con notas en español (qué incluye, la raíz de datos propia, que no es un release oficial). Usa `permissions: contents: write` y el `GITHUB_TOKEN`. Un push nunca publica nada.

**Tamaños** (run [37581043040](https://github.com/Kushro/orgtree-own/actions/runs/37581043040)):

| | Tamaño |
|---|---|
| Instalador NSIS | 51,3 MB |
| `orgtree-dioxus.exe` | 4,8 MB |
| Instalado (app más recursos) | 219,3 MB |
| `resources\engine\runtime` sin comprimir | 62,8 MB |
| `resources\engine\postgresql` sin comprimir | 141,3 MB |

`makensis` con LZMA sólido tarda unos 90 s en el runner. El primer arranque, con `initdb`, el clúster, las migraciones y el hub de mail, tarda unos 14 s hasta la UI; con la pausa de la captura, la app sale sola a los 18 s.

El workflow `.github/workflows/spike-dioxus.yml` hace lo mismo en `windows-latest` en cada push a `spike/dioxus`: compila el instalador NSIS, corre la prueba en WebView2 con el motor de fixture, instala el instalador y prueba la app instalada (#24), informa tamaños en el resumen del run y sube el instalador como artefacto `orgtree-dioxus-installer`.

La app se instala por usuario con su propio identificador (`com.kushro.orgtree.dioxus-spike`), así que no pisa una instalación de Orgtree existente.

**Estilos:** por ahora la ventana base usa CSS en línea. La decisión entre reutilizar `apps/desktop/renderer/src/styles.css` o adoptar rust-ui con Tailwind se toma cuando se construya la primera vista real (#11), porque recién ahí se ve cuánto del CSS actual aplica.
