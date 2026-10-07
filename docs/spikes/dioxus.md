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
| 9 | #26 Organigrama y operaciones sobre agentes en RSX | Una vista con estado, menú y operaciones reales |
| 10 | #27 Desk completo en RSX | El desk de #12 en uso real: compositor, contenido y estado del turno |
| 11 | #28 Bandeja, preguntas y atención en RSX | Mail, preguntas y cola de atención, con notificaciones nativas con clic |
| 12 | #29 Docket en RSX | Lista, detalle y acciones del docket compartido de tickets |

## Decisiones de diseño ya tomadas

- **Todas las llamadas al motor salen desde Rust.** La UI no carga desde el motor, así que el token viaja como header en HTTP y en el handshake del WebSocket y el motor no cambia.
- **Markdown seguro en Rust** (`pulldown-cmark` + `ammonia` o equivalente) en lugar de `marked` + DOMPurify.
- **Estilos:** se evalúa en #8 si se reutiliza el CSS actual o se adopta rust-ui con Tailwind.

## Fuera del alcance

El lienzo WebGL del organigrama (con pan y zoom), docket, mail y demás vistas; tarea de arranque del sistema, instalación para todos los usuarios, upgrade desde instalaciones Electron, updater. El empaquetado del runtime de Python y PostgreSQL entró con #24.

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
- Abrir una org muestra su organigrama (`src/org.rs`, desde #26).
- Crear y borrar orgs entró con #26. Uso por proveedor, ajustes de la app y actualizaciones quedan fuera del recorte; sus botones aparecen deshabilitados para que el diseño coincida.
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

Fuera del recorte de #12, y hechos en #27: pensamiento (`thinking`), filas de mail y avisos, segmentos, respuestas citadas, adjuntos y el compositor para escribirle al agente.

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

### Organigrama (#26)

`src/org.rs` reemplaza la lista mínima de #11 por el organigrama, sin el lienzo WebGL ni el pan/zoom de `OrgCanvas.tsx`: un árbol indentado por `parent` con las clases `.node`, `.card`, `.kids` y `.badge` del CSS del renderer, que ya las tenía para la vista de árbol.

- **Cada agente:** la letra del tier, el nombre, el estado, el último resumen de estado, el modelo con el color del proveedor y los créditos (asiento, grant y libre). El estado sigue la precedencia de `TrayStatus` y `deriveTurnState`: detenido (`halt`), congelado, compactando, en cola por el límite de turnos (`waiting` o `queued_for_slot`), activo e inactivo.
- **Retirados:** plegados por defecto, con el botón "show N archived" de la lista de agentes (`agenttray.tsx`).
- **Barra de la org (`orgbar`):** el nombre, los agentes vivos y activos por tier (`ActiveAgentSummary`), el gasto, los créditos (`circulation · seats · free`, los de la barra del ojo en `cards.tsx`), la autoauditoría si encuentra algo y el killswitch trabado.
- **Menú de agente:** clic derecho en la tarjeta o el botón `⋯`, con las clases de `contextmenu.tsx` y el orden de `agentmenu.tsx`. La raíz ("you") ofrece contratar en el primer nivel.

| Operación | Endpoint (igual que `api.ts`) | Como en el renderer |
|---|---|---|
| Contratar | `POST /ops` `{op:'hire', parent, tier, grant, name, charter}` | El borrador de `cards.tsx`, como un diálogo con tier, nombre, grant y charter |
| Mover | `POST /ops` `{op:'move', node, new_parent}` | El renderer arrastra; acá se elige el superior. Mismo aviso con deshacer |
| Retirar o disolver | `POST /ops` `{op:'retire'\|'dissolve', node}` | La confirmación de `AgentRetireConfirm`, palabra por palabra, y el aviso con deshacer (`rehire`) |
| Recontratar | `POST /ops` `{op:'rehire', node}` | En los retirados |
| Detener y reanudar | `POST /nodes/{nid}/halt` y `/unhalt` | `HaltControl`: sin confirmación, el estado como aviso |
| Interrumpir | `POST /nodes/{nid}/interrupt` | El STOP del desk: solo con un turno en curso |
| Crear org | `POST /api/orgs` `{name, dirs}` | `NewOrg` en la página de inicio, sin las opciones avanzadas |
| Borrar org | `DELETE /api/orgs/{slug}` | La confirmación de `App.tsx` (`doomedOrg`) |

El cliente Rust suma `create_org`, `delete_org`, `op` (con `OpRequest`), `halt`, `unhalt` e `interrupt`. `tests/ops.rs` los prueba contra un servidor HTTP falso: verbo, ruta, cuerpo JSON y token de cada pedido, más un rechazo del ledger (422 con su `detail`).

**Actualización sin sondeo.** El árbol se pide al abrir y después solo cuando el WebSocket de la org trae un `changed`, un `node_event` o una reconexión, como `refreshTree` en `App.tsx`. Varios frames que llegan juntos piden una sola vez. Una operación hecha desde la vista tampoco pide el árbol: el cambio llega por el mismo frame que vería otra ventana. La lista de orgs de la página de inicio sigue cada 5 s, como `orgstatus.ts` (no hay un WebSocket de la lista), y se relee en el acto tras crear o borrar.

**Trampa de Dioxus.** `spawn` ata la tarea al componente que la lanza. El menú y los diálogos se desmontan apenas se elige la acción, así que una operación lanzada desde ahí se cancelaba antes de llegar al motor. Las tareas de la vista se lanzan en el scope del organigrama (`Runtime::spawn(scope, …)`), y los avisos usan `try_write` por si la vista ya se cerró.

**Proveedor stub.** La puerta de contratación (`provider_hire_gate`) pide el CLI de Claude instalado y una cuenta iniciada. El motor de fixture los declara presentes (`claude_install_state` y `accounts.live_identity`), sin tocar `engine/`. Ningún turno corre: `send_message` y el pool de procesos calientes ya estaban apagados.

**Verificado en WebView2** con la prueba y el motor de fixture (run [37586696948](https://github.com/Kushro/orgtree-own/actions/runs/37586696948)):

- crear "Prueba Dioxus" desde la página de inicio abre su organigrama vacío;
- en 8 s con la org quieta, cada relectura del árbol vino de un frame del WebSocket (2 frames, 2 lecturas); en toda la etapa hubo 17 lecturas: la inicial y una por cada uno de los 16 frames;
- contratar `jefe` en el primer nivel y `ayudante` bajo él desde el menú, y el árbol los anida;
- una contratación hecha por fuera de la UI (el cliente Rust directo) aparece sola, por el WebSocket;
- la barra muestra `3 live H3` y `circulation 7 · seats 3 · free 4`, y el modelo `claude-haiku-4-5` con el color de Claude;
- el menú de `ayudante` trae Open desk, Hire a subordinate…, Move to…, Interrupt (deshabilitado sin turno), Halt y Retire…;
- detenerlo muestra la insignia "Halted" y el aviso del motor, y reanudarlo vuelve a "Idle";
- moverlo al primer nivel y deshacer desde el aviso lo devuelve bajo `jefe`;
- retirarlo pide la confirmación del renderer, lo pliega, "show 1 archived" lo muestra y recontratarlo lo vuelve a "Idle";
- borrar la org con la confirmación de `App.tsx` la saca de la lista.

El CI sube las capturas `chart-hire` (el diálogo de contratación), `chart-menu` (el menú de agente) y `chart` (el árbol con un agente detenido).

**Líneas** (sin comentarios ni líneas en blanco): `org.rs` 826 y la parte nueva de `home.rs` (crear y borrar) unas 100. Los archivos TSX equivalentes suman 852: `agentmenu.tsx` 232, `contextmenu.tsx` 338, `agenttray.tsx` 116, `treeinfo.tsx` 72, `orgrows.tsx` 55 y `haltcontrol.tsx` 39. A eso se suman las partes dentro de archivos grandes (la barra de la org en `App.tsx`, `NewOrg`, `ConfirmModal`, el borrador y el arrastre de `OrgCanvas.tsx` y `cards.tsx`), que no se pueden aislar. El TSX hace bastante más: submenús, navegación por teclado, pins y popouts, compactación, cuentas y filtros. El RSX es más largo por línea de marcado (cada atributo en su línea) y no comparte tipos con el motor: los toma del cliente Rust.

### Desk completo (#27)

`src/desk.rs` lleva el desk de #12 a uso real, siguiendo `canvas/desk.tsx`, `convo.ts`, `events/segments.tsx`, `replypreview.tsx`, `haltcontrol.tsx`, `effort.tsx` y `api.ts`, con las mismas clases y el mismo CSS del renderer.

**Compositor.** Los mismos endpoints y cuerpos que `api.ts`:

| Acción | Endpoint | Como en el renderer |
|---|---|---|
| Enviar (Enter; Shift+Enter es un salto de línea) | `POST /nodes/{nid}/message` `{text, client_op}` | La burbuja optimista mientras viaja, el aviso de `flashMode` con su orden (`delivering`, `queued (N ahead)`, `halted — mail stays unread until unhalt`…) y el mail pendiente con ✕ para retirarlo (`DELETE /nodes/{nid}/mail/{mid}`) |
| STOP | `POST /nodes/{nid}/interrupt` | Solo aparece con una respuesta en curso (`responding`); Enter sigue encolando |
| Modelo | `POST /ops` `{op:'switch_model', node, tier}` | Un clic dentro del mismo proveedor y fuera de un turno. A mitad de turno queda en cola (`→S` en el header) y pide confirmación; a otro proveedor es una división de linaje y pide confirmación, con los textos de `modals.tsx`. Elegir el modelo actual cancela el cambio en cola |
| Esfuerzo | `POST /nodes/{nid}/scope` `{effort}` | El botón chico y la pista de cinco puntos en un popover, optimista, con el aviso de `effortChangeToast` |

Un mensaje es mail y nunca interrumpe un turno: el desk no llama a `interrupt` al enviar, y el motor lo deja en el buzón para el próximo límite seguro. El borrador sigue compartido entre ventanas (#13). El renderer ubica el selector de modelo en el panel de ajustes del agente; acá está en el compositor, porque el recorte no tiene ese panel.

**Contenido de la conversación:**

- pensamiento plegado (`thought for 5s ▸`) o sellado, cuando el proveedor no mandó el texto;
- los segmentos de un mensaje del usuario (`Segment` tipado en el cliente): texto, mail, avisos y contexto de máquina, sin las variantes que el transcript humano oculta (`HUMAN_HIDDEN_VARIANTS`). Una forma desconocida muestra el texto, como `isSegments`;
- la tarjeta de mail de `MailMessage` para el mail entregado y el pendiente: tipo, remitente, hora, respuesta citada (`ReplyPreview`, con el salto al original si está cargado) y adjuntos;
- chips de herramientas con el resultado plegado, la salida de comandos y el resumen de una compactación;
- las horas en la zona local del webview (`fmtFull`), nunca el ISO del motor.

**Archivos: revelar, nunca abrir** (`src/reveal.rs`). Un enlace a un archivo de Windows en el Markdown queda inerte, con la ruta en `data-local-path`, como `winFileHref`. Un clic lo revela con `explorer /select,` si la ruta es absoluta y existe (las validaciones de `desktop:reveal-file`); si no, la ruta se muestra como texto en un aviso. Los adjuntos del mail traen rutas relativas a la carpeta del agente, así que también se muestran como texto. Antes de convertir el Markdown, las barras de esos destinos se normalizan, como `escapeProse`: CommonMark toma `\_` como escape y `D:\a\_temp` llegaría como `D:\a_temp` (lo cubre un test).

**Estado del turno**, con la precedencia de `deriveTurnState` y `TurnStatusBanner`: activo, en cola por el límite de turnos (con el aviso de `TurnSlotQueuedBanner`; el botón de ajustes aparece deshabilitado porque los ajustes de la app están fuera del recorte), compactando, detenido (la insignia de `HaltStatus`, el aviso de `HaltedBanner` y `HaltControl` para detener y reanudar) e inactivo, o el último estado que informó el agente. El desk pide el agente al árbol al abrir y en cada `changed` o `node_event`, sin sondeo.

**Bloqueo del framework y cómo se rodeó.** Dioxus desktop manda el `href` de cualquier `<a>` clicado a `webbrowser::open` (`handleClickNavigate` del intérprete), aunque la página haya llamado a `preventDefault`: solo mira si el VirtualDom lo previno. En el CI, el clic en un enlace a un archivo (`href="#"`) abrió Edge, que tapó la ventana y rompió la prueba de arrastre. Con una ruta relativa podía lanzar un programa, justo lo que la regla de revelar quiere evitar. El desk corta el clic en la captura (`stopPropagation`) antes de que llegue al listener de Dioxus: un archivo local se revela y un enlace externo o relativo se muestra como texto para copiarlo. El CI falla si durante la prueba se abre un navegador.

**Cliente Rust.** Suma `send_message` (`SendMessage`, `SendResult` con el texto de `flashMode`), `save_scope`, `retract_mail`, `OpRequest::switch_model` y los tipos del contenido (`Segment`, `MailRow`, `NoticeRow`, `Attachment`, `PendingSwitch`). `tests/ops.rs` prueba los pedidos contra el servidor falso: ruta, verbo, cuerpo y token.

**Motor de fixture.** Sin tocar `engine/`:

- siembra al final de la conversación un mensaje con mail (respuesta citada y un adjunto), un estado y un aviso, como la proyección que escribe el motor al admitir un turno (`_record_prompt_view`), y una respuesta con pensamiento y enlaces a un archivo que existe, uno que no, una URL y una ruta relativa;
- el envío pasa por la puerta real (`send_message` y la admisión de halt), pero el cuerpo no arranca un turno: el mail queda aceptado en el buzón, y un agente detenido lo retiene hasta reanudarlo;
- `POST /api/fixture/turn-state` simula un turno en cola o en curso y avisa por el WebSocket.

**Verificado en WebView2** (run [37593345025](https://github.com/Kushro/orgtree-own/actions/runs/37593345025)):

- el mail de `User` y de `jefe` con la negrita, la respuesta citada, el adjunto, el aviso y el texto del segmento; el pensamiento se despliega;
- el adjunto (`Not an absolute path: uploads/informe.txt`) y el archivo inexistente (`No such file: C:\orgtree-fixture-no-existe\falta.log`) se muestran como texto; la URL y la ruta relativa también, y no se abre ningún navegador;
- inactivo, en cola con `the agent concurrency limit (16) is reached…`, activo con STOP (el motor responde que no hay una llamada al proveedor), e inactivo otra vez;
- enviar a mitad de turno deja el mensaje en el buzón (`queued (1 ahead)`), el turno sigue activo y el compositor se vacía;
- el cambio a `sonnet` a mitad de turno pide `queue worker's switch to sonnet?`, queda en cola (`→S` y el aviso del motor) y elegir `haiku` lo cancela; a `sol` pide `move worker from Claude to Codex?` con la división de linaje, y cancelar no cambia nada; fuera de un turno, `sonnet` y de vuelta `haiku` son un clic;
- el esfuerzo pasa de `high` (heredado) a `low` con `applies from its next turn`;
- detener muestra la insignia, el aviso y Unhalt; un envío queda `halted — mail stays unread until unhalt`; reanudar vuelve a inactivo;
- revelar `informe.txt` abre el Explorador en `D:\a\_temp\orgtree-dioxus-smoke-fixture-files` (lo confirma `Shell.Application`).

La prueba de scroll de #12 sigue igual y fluida: 1.206 filas (del mensaje 1 al 1200 más el contenido nuevo), 4 páginas anteriores en 859 ms, y en el scroll de punta a punta (42.213 px, 257 cuadros) un promedio de 15,6 ms por cuadro, p95 15,7 ms, máximo 15,8 ms y ninguno sobre 50 ms. Los tests de `src/desk.rs` y `src/reveal.rs` (Markdown y enlaces, revelado, hora local, esfuerzo) corren en el CI.

Capturas por marcador: `desk-content` (mail, aviso, pensamiento y enlaces), `desk-queued` (en cola por el límite), `desk-halted` (detenido, con los envíos pendientes) y `reveal` (el Explorador con la carpeta del archivo).

Fuera del recorte: subir adjuntos desde el compositor, responder citando un mensaje, el aviso pasivo (Alt+N), el historial del compositor, las pestañas de inbox, docket e historial, y la tarjeta de cada variante de evento (`EventCard`): el desk muestra el tipo y el cuerpo.

**Líneas** (sin comentarios, líneas en blanco ni tests): `desk.rs` 1.281 y `reveal.rs` 86, frente a 241 del `desk.rs` de #12. Lo equivalente en TSX suma 4.218: `desk.tsx` 3.003, `convo.ts` 890, `events/segments.tsx` 132, `effort.tsx` 62, `mailpreview.tsx` 61, `haltcontrol.tsx` 39 y `replypreview.tsx` 31, más las partes que no se pueden aislar (el cambio de modelo en `modals.tsx`, `md()` y el revelado en `canvas/shared.ts`, `EventCard`). El TSX sigue haciendo más (ver arriba), y en RSX cada atributo va en su línea.

### Bandeja, preguntas y atención (#28)

Lo que el usuario tiene que atender, en RSX con las clases del renderer y su CSS sin cambios: `styles.css` y, ahora también, la hoja propia de la vista de atención (`attention/attention.css`, con `include_str!`). Siguen `canvas/mail.tsx`, `canvas/asks.tsx`, `attention/` (`AttentionQueue.tsx`, `feed.ts`, `AttentionView.tsx`), la bandeja de `App.tsx`, `notifications.ts` y `api.ts`.

**Bandeja del usuario** (`src/inbox.rs`). La campana de la barra de la org (`iconbtn ask-bell`) cuenta el mail sin leer y los pedidos abiertos, y brilla con un urgente. Abre el panel de `App.tsx`: las carpetas `inbox` y `sent`, la lista (`mailrow`, con `unread`, `urgent` y `ask`) y el panel de lectura (`mailer-head`, la razón del urgente en `urgent-why`, el cuerpo en Markdown seguro y la caja de respuesta).

| Acción | Endpoint (igual que `api.ts`) | Como en el renderer |
|---|---|---|
| Leer y archivar | `POST /inbox/read` `{ids}` | Un mail se archiva al salir de él (elegir otro o cerrar la bandeja), como `leave` en `MailList`. La marca es optimista y vuelve atrás con el error (`markReadNow`) |
| Archivar todo | `POST /inbox/clear` | "Mark all read" |
| Responder | `POST /nodes/{remitente}/message` `{text, target, client_op}` | `replyMessage`: el `target` es la identidad del mail (`{kind:'mail', org, box:'user', id}`) y el motor arma la cita. Con un recibo durable el original queda leído |
| Responder una pregunta | `POST /nodes/{nid}/batch` `{revs, answers, credits?, scope?}` | `BatchAsk`: la tarjeta compuesta del agente, con pestañas de preguntas (opciones, Other, Skip), créditos y alcance; se va en el clic (`asksubmitted`) y vuelve si falla |
| Descartar una pregunta | el mismo `/batch`, con todas las pestañas saltadas | La ✕ de `BatchAsk`: el agente se entera y puede volver a preguntar |

Las preguntas viajan en la bandeja como filas propias (`askMailRow`), mezcladas con el mail, y su panel es la tarjeta. Salen de `openAsks`: la tarjeta de cada nodo del árbol y, para un agente que el árbol no trae, la que se arma con las filas sueltas de la cabecera (`composeBatch`). El panel de créditos no tiene la barra arrastrable de `CreditAsk`: se concede lo pedido, se niega o se salta.

**Cola de atención** (`src/attention.rs`). La barra de la org suma el selector `Chart` / `Attention` de `AttentionView.tsx`, con lo que espera en la cola. La cola es la de `feed.ts`: tickets con la bandera de atención (`manual_attention`, de todos los grupos), mail urgente sin leer y preguntas abiertas, del más nuevo al más viejo. Cada fila usa la de su lista de origen (`docket-row` para un ticket, `mailrow` para el mail y las preguntas) y su panel es el de su detalle.

- **La bandera queda arriba** hasta que el usuario responde o la descarta. Responder (`POST /work-items/{wid}/reply` `{body}`) le escribe al asignado y el motor baja la bandera sin cambiar el estado. "Dismiss with no comment" (`POST /work-items/{wid}/dismiss-attention` `{set_rev}`) la saca de la lista en el clic, el ticket pasa a `blocked` y vuelve con el error si el motor se niega.
- Abrir un mail urgente lo marca leído, y queda a la vista mientras siga elegido (`retainSelected`).
- Cuando lo elegido se resuelve, la selección pasa a la fila de abajo, o a la de arriba (`nextSelection`). Sin selección se abre la primera, como el renderer.

La bandeja y los tickets se piden con el árbol, al abrir la org y con cada frame del WebSocket, sin sondeo.

**Notificaciones nativas** (`src/notify.rs`), con la lógica de `useNativeNotifications` y del lado nativo de Electron:

- **Una sola dueña.** La pasada global (`GET /api/desktop/notifications` con todas sus páginas, la barra de tareas, retirar y mostrar) corre en la ventana principal, cada 5 s y con cada frame de una org abierta. Los desks en otra ventana no la repiten.
- **Preferencias por tipo** de `packages/contracts/notifications.ts` (`notificationEnabled`, con el `routineNotifications` viejo), en `preferences.json` de la carpeta propia de la app. El inicio tiene el grupo "Notifications" de los ajustes de escritorio (`SetGroup` y `SetToggle`). Apagar un tipo retira lo que ya se mostró de ese tipo.
- **Con Orgtree enfocado** no se notifica, salvo `notifyWhileFocused`. Una pregunta cuya tarjeta está en pantalla ya llegó al usuario (`questionVisible`).
- **Deduplicación** por `org` + `id`. Lo mostrado se recuerda en `notifications-seen.json` mientras siga pendiente, así que reiniciar la app no repite alertas.
- **Retirar las resueltas.** Lo que sale de la proyección se saca del centro de notificaciones.
- **El clic** vuelve a leer la proyección (el sistema puede retener un banner ya resuelto), muestra la principal y abre el elemento: una pregunta, un ticket con bandera o un urgente en la cola de atención; el resto del mail, en la bandeja; un agente congelado, en su desk.
- **Barra de tareas.** Parpadea (`request_user_attention`, `Critical`) con cada llegada nueva si la ventana no tiene el foco, y para cuando no queda nada pendiente. Cuenta la proyección entera, no la filtrada por preferencias: silenciar un tipo no quiere decir que dejó de esperar.

**Bloqueo y cómo se rodeó.** `notify-rust` no da el clic en Windows. El toast se arma con WinRT directo, como el spike de Tauri en #21: `ToastNotification` con tag y grupo propios para poder retirarlo (`History.RemoveGroupedTagWithId`) y el evento `Activated`, que corre en un hilo de WinRT y manda el tag por un canal a la principal. Los toasts mostrados se guardan vivos mientras estén en el centro de notificaciones, para que el handler siga suscripto. El instalador de dx no registra un AppUserModelID propio en el acceso directo, y un id sin registrar no muestra nada: se usa el de PowerShell, el mismo que `notify-rust`. Fuera de Windows se usa `notify-rust`, sin clic.

**Trampa de Dioxus.** Las cajas de respuesta lanzan el envío en el scope de la vista (por la trampa de `spawn` de #26), y sus señales también nacen ahí (`Signal::new_in_scope`): si no, Dioxus avisa que una señal de un hijo se usa desde el padre, y el texto no podría volver a la caja cuando la fila ya se fue.

**Cliente Rust.** Suma `inbox`, `mark_read`, `clear_inbox`, `reply_mail`, `resolve_batch`, `answer_ask`, `work_items`, `dismiss_attention`, `reply_work_item` y `notifications`, y los tipos (`InboxPayload`, `AskInfo`, `AskTab`, `WorkItem`, `DesktopNotice`…). La tarjeta de cada nodo y los pedidos de la cabecera entran al árbol de forma tolerante: una forma inesperada queda vacía y no rompe el árbol. `tests/ops.rs` prueba ruta, verbo, cuerpo y token de cada pedido contra el servidor falso, y las páginas de las notificaciones.

**Motor de fixture.** `POST /api/fixture/attention` `{kind}` hace que `worker` le escriba al usuario (urgente o de rutina), le pregunte, o levante la bandera en dos tickets. Todo pasa por el ledger real (`post_mail`, `ask_user`, `work_create`, `work_update`), así que la bandeja, la proyección de notificaciones y el WebSocket ven lo mismo que con un agente de verdad. `engine/` no cambia.

**Verificado en WebView2** (run [37601442161](https://github.com/Kushro/orgtree-own/actions/runs/37601442161)):

- con la ventana minimizada, el agente le escribe un urgente y uno de rutina, le pregunta y levanta la bandera en dos tickets: se muestran la pregunta, el urgente y las dos banderas, el de rutina queda filtrado por su tipo, y la barra de tareas empieza a parpadear (`started`, sin foco);
- una pasada siguiente no repite nada (`duplicate`); "All mail" muestra el de rutina y, apagado otra vez, se retira;
- con la ventana al frente, un urgente nuevo espera (`focused`) hasta prender "Notify while focused";
- la campana cuenta 4 (3 sin leer y la pregunta) y la cola, 5 filas: 2 tickets, 2 urgentes y la pregunta;
- el clic simulado en la notificación de la pregunta (lo mismo que llama `Activated`; el CI no puede hacer clic en el centro de notificaciones) abre la cola con la pregunta elegida;
- responder la pregunta la saca de la cola y retira su notificación; abrir un urgente lo marca leído y queda a la vista mientras está elegido; responderlo llega a `worker`;
- responder un ticket baja la bandera y lo deja `in_progress`; descartar el otro lo saca en el clic y lo pasa a `blocked`; una pregunta nueva se descarta con la ✕;
- en la bandeja, el de rutina se archiva al salir de él, el segundo urgente se responde y "Mark all read" deja la bandeja sin nada sin leer;
- al final la cola está vacía, cada notificación resuelta se retiró y la barra de tareas paró (`Stop`).

El primer push falló solo por la verificación de la rutina filtrada: la pasada la saca de los elegibles antes de decidir, como el renderer, y no quedaba registrada. El registro la anota ahora.

Capturas por marcador: `taskbar` (la ventana minimizada con la barra de tareas parpadeando), `notification-click` (la cola tras el clic, con la pregunta elegida), `attention` (el panel de un ticket con la bandera) e `inbox` (la bandeja con un mail abierto).

Fuera del recorte: el desk del agente a la derecha de la cola (`AgentDeskPanel`), la carpeta `record`, los pedidos de audiencia, adjuntos en las respuestas, el aviso pasivo, el historial de pedidos resueltos en la bandeja, el resto del panel del ticket (historial, adjuntos, aceptación) y los avisos de documentos.

**Líneas** (sin comentarios, líneas en blanco ni tests): `inbox.rs` 591, `attention.rs` 275 y `notify.rs` 426, más 264 nuevas en `org.rs` (el contexto con las acciones, la carga, el selector y el clic) y 52 en `home.rs` (los ajustes): 1.608. Lo equivalente en TSX suma 3.299: `mail.tsx` 1.266, `asks.tsx` 765, `AttentionQueue.tsx` 335, `AttentionView.tsx` 217, `notifications.ts` 160, `asksubmitted.ts` 162, `feed.ts` 124, `openasks.ts` 96, `mailread.ts` 84, `attndismiss.ts` 51 y `contracts/notifications.ts` 39, más el lado nativo de Electron (`main/notifications.ts` 115 y `taskbar-attention.ts` 78) y la bandeja dentro de `App.tsx`, que no se puede aislar. El TSX hace bastante más (ver arriba: carpetas, búsqueda, paginado, referencias, la barra de créditos, el desk de la cola, varias ventanas dueñas).

### Docket (#29)

`src/docket.rs` reescribe el docket compartido de tickets en RSX, con las clases del renderer y su `styles.css` sin cambios. Sigue `canvas/docket.tsx` (`DocketModal`, `DocketRow`, `DocketPane`), `docketdesc.tsx`, las referencias de `workrefs.tsx`, `refmd.tsx` y `reflinks.tsx`, "Staff…" de `quickstaff.ts` y `api.ts`. Se abre con el botón del docket de la barra de la org (`DocketToolbarButton`, con el mismo brillo y el mismo número) y desde la cola de atención de #28: el panel de un ticket en la cola ahora es el del docket, con "Open in docket".

**Lista.** Las filas livianas de `GET /work-items-view`, en el orden del motor (la última actualización del docket):

- el nombre es el slug y el título va en el tooltip; la edad, el estado (con la ayuda de `statusHelp`), `question waiting` y el dueño o `Unassigned`;
- los sub-ítems van bajo su padre (`nestRows`, con `--docket-depth`);
- filtros por estado y por dueño, la búsqueda (`matchesTerms`) y los tres arreglos: sin agrupar, por estado (`STATUS_GROUPS`, con "Needs attention" primero) y por agente;
- el backlog y el archivo se piden solo con su casilla (`?backlogged=1`, `?archived=1`) y siempre van al final, como `buildSections`. Es la regla del usuario de `AGENTS.md`: los totales de la lista no cuentan lo archivado, y el total del archivo aparece solo junto a su casilla. Los filtros eligen entre lo que está a la vista, nunca más allá de una casilla.

**Detalle.** La fila liviana se ve en el acto. El ticket entero llega con `GET /work-items/{wid}` al abrirlo, y otra vez cada vez que cambia su `rev`:

- la descripción en Markdown seguro, con el aviso de alcance (`objective_notice`);
- el estado con su información: `blocked` con su motivo y `dropped` con por qué terminó sin completarse;
- lo hecho y lo que sigue, con las marcas del renderer;
- las decisiones del registro de alcance, que solo se agregan: una reemplazada dice por cuál;
- las evidencias (tope del motor: 50), los artefactos (tope: 40) y los adjuntos del ticket;
- la bandera manual, la pregunta adjunta con su tarjeta, "Staff…" en el backlog, los holders anteriores y el historial (tope: 100; empieza plegado, como la verificación en el renderer).

El renderer no muestra las decisiones, los holders ni el historial, aunque el motor los sirve en el detalle: el RSX los suma porque el issue los pide.

**Referencias en la prosa.** Los nombres de tickets y agentes de la org y los tokens canónicos (`@item:org/slug`, `@agent:org/nodo`) se vuelven enlaces, con las reglas de borde de `workrefs.tsx` (un nombre dentro de una ruta o de una URL no es una mención; un ticket gana a un agente del mismo nombre). Un token de otra org, uno que la org no tiene o uno de un documento o un mail queda como texto que dice por qué (`reflinks.tsx`). En el Markdown de la descripción, los enlaces se arman al convertir (`markdown_with`), fuera de los enlaces y de los bloques de código, y `ammonia` sigue saneando el resultado. Un ticket se abre en el docket (prende su casilla si está en el archivo o en el backlog, como `goToItem`) y un agente, en su desk.

**Archivos: guardar y revelar, nunca abrir.** El renderer descarga un artefacto con un `<a download>`. Acá se baja con el cliente Rust (con el token), se guarda en `Descargas\Orgtree\<org>\<ticket>\` sin pisar un archivo distinto, con un nombre saneado que no puede salir de esa carpeta, y se revela en el Explorador como en #27. Un artefacto `named` sin permiso aparece sin nombre.

**Acciones del usuario.** Son las que tiene el docket del renderer, con los mismos endpoints y cuerpos:

| Acción | Endpoint (igual que `api.ts`) | Como en el renderer |
|---|---|---|
| Comentar | `POST /work-items/{wid}/reply` `{body, to?, notice?}` | La caja `REPLY`: al dueño, o a un participante elegido; "as a notice" la entrega sin despertar al agente. El aviso dice lo que hizo el motor (`deferred`, `notice`) |
| Bajar la bandera | `POST /work-items/{wid}/dismiss-attention` `{set_rev}` | "Dismiss with no comment": el motor pasa el ticket a `blocked` con el motivo `attention flag dismissed by the user (…)`. La bandera se va en el clic y vuelve con el error si el motor se niega |
| Responder la pregunta adjunta | `POST /nodes/{nid}/batch` | La tarjeta de #28 dentro del panel del ticket |
| Asignar | `GET` y `POST /work-items/{wid}/quick-staff` `{request_id, mode, configured_mode, owner, tier?, effort?}` | "Staff…" en un ticket del backlog: en modo `request` se le pide al asignado; sin asignado vivo, se contrata en el primer nivel y el ticket queda asignado. Un reintento de la misma elección repite su `request_id` |

En el renderer "Staff…" está en el menú contextual de la fila; acá es una sección del panel.

**Lo que no es del usuario.** El issue pedía también cambiar el estado, asignar a mano, levantar la bandera y adjuntar una pregunta. El docket del renderer no tiene esos controles a propósito ("Assigning, changing status, raising the flag and adding a sub-item are agents' own acts through the work tool", en `DocketRow`), y el motor no tiene rutas del usuario para eso: su superficie es leer, responder, descartar la bandera, aceptar, borrar, adjuntos y "Staff…" (`api.py`). Para no inventar controles ni tocar `engine/`, el RSX hace lo mismo que el renderer: el usuario cambia el estado a `blocked` con su motivo al descartar una bandera y asigna con "Staff…", y lo demás lo hacen los agentes y llega por el WebSocket. La prueba lo cubre con el motor de fixture actuando como esos agentes. Agregar esos controles queda como decisión pendiente (ver el reporte de #29).

**Actualización sin sondeo.** La lista se pide con el árbol, al abrir y con cada frame del WebSocket de la org; una casilla relee solo la lista. El detalle se vuelve a pedir cuando la fila trae otro `rev`.

**Cliente Rust.** Suma `work_items_view`, `work_item`, `reply_work_item_to` (`WorkReply`, `WorkReplyResult`), `quick_staff_preview` y `quick_staff` (`QuickStaffPreview`, `QuickStaffSelection`), `artifact_bytes` y `attachment_bytes`, y los tipos del detalle (`ScopeRow`, `Evidence`, `Artifact`, `Holder`, `WorkQuestion`, `Recipient`, `WorkCounts`, `WorkSummary`). Una sección con una forma inesperada queda vacía y no rompe el ticket. `tests/ops.rs` prueba ruta, verbo, cuerpo y token de cada pedido contra el servidor falso, la descarga cruda y un 404 con su `detail`.

**Motor de fixture.** `POST /api/fixture/docket` `{kind}`, sin tocar `engine/`, siempre por el ledger real:

- `seed` contrata a `jefe` y siembra seis tickets: uno `in_progress` que pasó de `jefe` a `worker` (un holder anterior), con dos decisiones, dos evidencias, un artefacto y un sub-ítem; uno `blocked` con motivo; uno de `jefe`; uno sin dueño en el backlog; y uno `dropped`, que se archiva en el acto;
- `status`, `flag` y `question` hacen que el dueño cambie el estado (con su motivo), levante la bandera o que `worker` adjunte una pregunta, como lo haría con la herramienta del docket.

**Verificado en WebView2** (run [37608842437](https://github.com/Kushro/orgtree-own/actions/runs/37608842437)):

- el botón de la barra cuenta 6 activos y abre el docket con 6 filas; los totales dicen `6 active` y no cambian al mostrar el archivo;
- el archivo y el backlog aparecen solo con su casilla, al final;
- el filtro `blocked` deja solo tickets bloqueados, el de `jefe` uno y `Unassigned` el del backlog; por estado, los grupos van con el backlog al final, y el sub-ítem queda en el nivel 1;
- el detalle trae el título, la negrita y la referencia de la descripción, 2 decisiones (`2 · append-only`), 2 evidencias (`2 of 50`), el artefacto (`1 of 40`), `jefe` como holder anterior y el historial plegado con `assign — from: jefe · to: worker`;
- guardar el artefacto lo deja en Descargas con su contenido y abre el Explorador en su carpeta;
- comentar llega a `jefe`;
- el dueño pasa un ticket a `blocked` y la fila y su motivo cambian solos (por frames del WebSocket); otro pasa a `dropped` y deja la lista en el acto, y aparece en el archivo con por qué terminó;
- el agente levanta la bandera: la fila y el botón se encienden, la cola de atención la muestra con el panel del docket y "Open in docket" lo abre; descartarla pasa el ticket a `blocked` con su motivo y el botón se apaga;
- la pregunta adjunta aparece (`question waiting`) y se responde desde el panel;
- "Staff…" en el ticket sin dueño contrata `medir-la-memoria` en el primer nivel y el ticket pasa a `Open`, asignado;
- al final, el estado de cada ticket en el motor coincide con lo que mostró la vista.

Capturas por marcador: `docket` (la lista con el backlog y el detalle de un ticket), `docket-artifact` (el Explorador con el artefacto guardado) y `docket-flag` (un ticket con la bandera, abierto desde la cola de atención).

La prueba también corre localmente en Linux con WebKitGTK bajo Xvfb y el motor de fixture, sin las partes de Windows (notificaciones, barra de tareas, Explorador). Así se depuró antes del primer push.

Fuera del recorte: subir y borrar adjuntos del ticket, la búsqueda de referencias que no están cargadas (`getWorkReferences`), el lector de documentos y la bandeja de los tokens de documentos y de mail, las secciones de aceptación, revisión de integración y hallazgos, la tarjeta de cada agente (`AgentName` con su tier), el plegado de categorías y sub-ítems, el orden por creación o por cambio de estado, y el docket de un agente en su desk (`AgentDocketView`).

**Líneas** (sin comentarios, líneas en blanco ni tests): `docket.rs` 1.330, `engine-client/src/docket.rs` 200 (tipos) y unas 90 nuevas en `org.rs` (el contexto, la apertura y los enlaces), frente a 2.864 de TSX: `docket.tsx` 2.004, `reflinks.tsx` 281, `refmd.tsx` 244, `workrefs.tsx` 180, `quickstaff.ts` 90 y `docketdesc.tsx` 65, más los tipos de `types.ts` y lo que no se puede aislar (`staffingoptions.ts`, `workrefresolve.ts`, `docketwindow.ts`, la apertura del docket en `App.tsx`). El TSX hace más (ver arriba); el RSX suma decisiones, holders e historial, y escribe cada atributo en su línea.

El workflow `.github/workflows/spike-dioxus.yml` hace lo mismo en `windows-latest` en cada push a `spike/dioxus`: compila el instalador NSIS, corre la prueba en WebView2 con el motor de fixture (incluidos el organigrama de #26, el desk completo de #27, la bandeja, las preguntas y la atención de #28 y el docket de #29), instala el instalador y prueba la app instalada (#24), informa tamaños en el resumen del run y sube el instalador como artefacto `orgtree-dioxus-installer`.

La app se instala por usuario con su propio identificador (`com.kushro.orgtree.dioxus-spike`), así que no pisa una instalación de Orgtree existente.

**Estilos:** por ahora la ventana base usa CSS en línea. La decisión entre reutilizar `apps/desktop/renderer/src/styles.css` o adoptar rust-ui con Tailwind se toma cuando se construya la primera vista real (#11), porque recién ahí se ve cuánto del CSS actual aplica.
