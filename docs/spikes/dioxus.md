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

## Decisiones de diseño ya tomadas

- **Todas las llamadas al motor salen desde Rust.** La UI no carga desde el motor, así que el token viaja como header en HTTP y en el handshake del WebSocket y el motor no cambia.
- **Markdown seguro en Rust** (`pulldown-cmark` + `ammonia` o equivalente) en lugar de `marked` + DOMPurify.
- **Estilos:** se evalúa en #8 si se reutiliza el CSS actual o se adopta rust-ui con Tailwind.

## Fuera del alcance

Canvas del organigrama, docket, mail y demás vistas; tarea de arranque del sistema, instalación para todos los usuarios, upgrade desde instalaciones Electron, empaquetado del runtime de Python y PostgreSQL, updater.

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
| `ORGTREE_DIOXUS_PYTHON` | Python absoluto con `tools/runtime-requirements.in` instalado | obligatoria |
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

El workflow `.github/workflows/spike-dioxus.yml` hace lo mismo en `windows-latest` en cada push a `spike/dioxus`: compila el instalador NSIS, verifica que la ventana arranque y siga abierta 15 segundos, informa tamaños en el resumen del run y sube el instalador como artefacto `orgtree-dioxus-installer`.

La app se instala por usuario con su propio identificador (`com.kushro.orgtree.dioxus-spike`), así que no pisa una instalación de Orgtree existente.

**Estilos:** por ahora la ventana base usa CSS en línea. La decisión entre reutilizar `apps/desktop/renderer/src/styles.css` o adoptar rust-ui con Tailwind se toma cuando se construya la primera vista real (#11), porque recién ahí se ve cuánto del CSS actual aplica.
