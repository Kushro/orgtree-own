//! El desk de un agente en RSX: la conversación en vivo (#12) y, desde #27, el
//! desk completo para usarlo de verdad. Reescribe lo esencial de
//! `renderer/src/canvas/desk.tsx`, `convo.ts`, `events/segments.tsx` y
//! `api.ts` con las mismas clases (y el mismo CSS) del renderer.
//!
//! Lo que el desk actual resuelve en JS y acá se rediseñó:
//! - **Markdown**: `marked` + DOMPurify en cada render → `pulldown-cmark` +
//!   `ammonia` en Rust, una sola vez por mensaje al cargarlo (`Row`).
//! - **Lista larga**: la ventana medida de `convo.ts` (filas visibles, alturas
//!   medidas) → `content-visibility: auto` en cada fila: el webview no pinta
//!   ni maqueta lo que está fuera de la vista.
//! - **Texto en vivo**: los `delta` de `node_stream` van a una señal aparte
//!   (`draft`), así un frame no vuelve a renderizar la lista.
//! - **Páginas anteriores**: se piden al llegar arriba con el scroll, con el
//!   cursor `before`; el anclaje de scroll del webview mantiene la posición.
//! - **Refresco**: un frame durable (`text`, herramientas, `turn_done`) o una
//!   reconexión vuelve a pedir la última página, como el `nudge` de `convo.ts`;
//!   un `changed` o un `node_event` vuelve a pedir el agente (estado del turno,
//!   detención, modelo y esfuerzo), como `refreshTree`.
//!
//! Reglas del producto que el compositor respeta (AGENTS.md y desk.tsx):
//! - un mensaje es mail y **nunca interrumpe** un turno: a mitad de turno
//!   queda en cola para el próximo límite seguro; solo STOP interrumpe, y
//!   solo aparece con una respuesta en curso;
//! - un cambio de modelo a mitad de turno **queda en cola** para el próximo
//!   turno, y uno a otro proveedor es una división de linaje: los dos piden
//!   confirmación con los textos de `modals.tsx`;
//! - un clic en un archivo lo **revela** en su carpeta, nunca lo abre
//!   (`crate::reveal`); si no se puede, la ruta se muestra como texto.

use crate::org::{provider_of, tier_letter, ALL_TIERS};
use crate::Route;
use dioxus::prelude::*;
use orgtree_engine_client::{
    Backoff, ChatMessage, ChatPayload, Client, Frame, MailRow, NodeState, NoticeRow, OpRequest, OpResult, Segment,
    SendMessage, ToolChip, TreeNode, WsEvent, HUMAN_HIDDEN_VARIANTS,
};
use std::collections::HashSet;
use std::time::Duration;

/// Igual que el renderer: 300 mensajes por página.
const PAGE: u32 = 300;
/// El borrador en vivo se recorta como en `convo.ts`.
const DRAFT_CAP: usize = 12_000;
/// `EFFORT_LEVELS` de `canvas/effort.tsx`.
const EFFORT_LEVELS: [&str; 5] = ["low", "medium", "high", "xhigh", "max"];
const TOAST_FOR: Duration = Duration::from_secs(12);

// ─── Filas ya preparadas: el Markdown se convierte una vez, al cargar ─────────

#[derive(Clone, PartialEq)]
struct Row {
    key: String,
    role: String,
    /// El id del evento, para ubicar una respuesta citada (`data-reply-event`).
    event_id: Option<String>,
    /// Markdown ya convertido y sanitizado.
    html: String,
    tools: Vec<ToolChip>,
    thinking: Option<Thought>,
    /// La composición de un mensaje del usuario (texto, mail y avisos).
    segments: Option<Vec<SegView>>,
    /// Filas de sistema: la salida de un comando o la compactación.
    sys_text: String,
    cmd_out: Option<String>,
    summary: Option<String>,
    partial: bool,
    truncated: bool,
    receipt: Option<String>,
}

#[derive(Clone, PartialEq)]
struct Thought {
    text: Option<String>,
    secs: Option<f64>,
}

#[derive(Clone, PartialEq)]
enum SegView {
    Text(String),
    /// Contexto de máquina visible (`state` o `drive`).
    Card { kind: &'static str, text: String },
    Notices(Vec<NoticeRow>),
    Mail(Vec<MailView>),
}

/// Una fila de mail con su cuerpo ya convertido.
#[derive(Clone, PartialEq)]
struct MailView {
    row: MailRow,
    html: String,
}

impl MailView {
    fn new(row: MailRow) -> MailView {
        let html = markdown(&row.body);
        MailView { row, html }
    }
}

/// Markdown seguro, como `md()` del renderer: sin scripts, handlers `on*`,
/// iframes ni URLs `javascript:` (DOMPurify). Un enlace a un archivo local
/// (`C:\…`) queda inerte, con la ruta en `data-local-path`: un clic lo revela
/// (`revealFileFromEvent`), nunca navega ni lo abre.
fn markdown(text: &str) -> String {
    use pulldown_cmark::{html, CowStr, Event, Options, Parser, Tag, TagEnd};
    let mut options = Options::empty();
    options.insert(Options::ENABLE_TABLES);
    options.insert(Options::ENABLE_STRIKETHROUGH);
    options.insert(Options::ENABLE_TASKLISTS);
    let mut local = false;
    let text = crate::reveal::normalize_links(text);
    let events = Parser::new_ext(&text, options).map(|event| match event {
        Event::Start(Tag::Link { ref dest_url, .. }) => match crate::reveal::local_path(dest_url) {
            Some(native) => {
                local = true;
                let native = attr(&native);
                Event::Html(CowStr::from(format!(
                    "<a href=\"#\" class=\"local-file\" data-local-path=\"{native}\" title=\"Show in folder — {native}\">"
                )))
            }
            None => event,
        },
        Event::End(TagEnd::Link) if local => {
            local = false;
            Event::Html(CowStr::from("</a>"))
        }
        other => other,
    });
    let mut out = String::new();
    html::push_html(&mut out, events);
    ammonia::Builder::default()
        .link_rel(Some("noopener noreferrer"))
        .add_tag_attributes("a", &["class", "data-local-path"])
        .clean(&out)
        .to_string()
}

/// Escapa un valor para un atributo HTML.
fn attr(text: &str) -> String {
    text.replace('&', "&amp;").replace('"', "&quot;").replace('<', "&lt;").replace('>', "&gt;")
}

fn segment_views(segments: Vec<Segment>) -> Vec<SegView> {
    segments
        .into_iter()
        .filter_map(|segment| match segment {
            Segment::Text { text } => Some(SegView::Text(markdown(&text))),
            Segment::State { text, event } | Segment::Drive { text, event }
                if event.as_ref().and_then(|e| e.get("variant")).and_then(|v| v.as_str()).is_some_and(|v| HUMAN_HIDDEN_VARIANTS.contains(&v)) =>
            {
                let _ = text;
                None
            }
            Segment::State { text, .. } => Some(SegView::Card { kind: "state", text }),
            Segment::Drive { text, .. } => Some(SegView::Card { kind: "drive", text }),
            Segment::Notices { rows } => Some(SegView::Notices(rows)),
            Segment::Mail { rows } => Some(SegView::Mail(rows.into_iter().map(MailView::new).collect())),
        })
        .collect()
}

fn rows(messages: &[ChatMessage], offset: usize) -> Vec<Row> {
    messages
        .iter()
        .enumerate()
        .map(|(i, m)| {
            let segments = if m.role == "user" { m.segments().map(segment_views) } else { None };
            let thinking = (m.thinking.is_some() || m.thinking_sealed == Some(true))
                .then(|| Thought { text: m.thinking.clone().filter(|t| !t.trim().is_empty()), secs: m.think_secs });
            Row {
                key: m.row_id.clone().or_else(|| m.event_id.clone()).unwrap_or_else(|| format!("i{}", offset + i)),
                role: m.role.clone(),
                event_id: m.event_id.clone(),
                html: if m.text.is_empty() || segments.is_some() || m.role == "system" { String::new() } else { markdown(&m.text) },
                tools: m.tools.clone(),
                thinking,
                segments,
                sys_text: if m.role == "system" { m.text.clone() } else { String::new() },
                cmd_out: m.cmd_out.as_deref().map(markdown),
                summary: m.summary.clone(),
                partial: m.assistant_state.as_deref() == Some("partial"),
                truncated: m.truncated == Some(true),
                receipt: m.steered.filter(|s| *s).and(m.receipt.clone()),
            }
        })
        .collect()
}

// ─── Hora local (sin UTC a la vista) ──────────────────────────────────────────

/// La zona horaria del webview: el desfase de `getTimezoneOffset` (minutos,
/// UTC menos hora local) y su nombre corto.
#[derive(Clone, PartialEq, Debug)]
pub(crate) struct Tz {
    offset_min: i64,
    name: String,
}

fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let doy = (153 * (m + if m > 2 { -3 } else { 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { yoe + era * 400 + 1 } else { yoe + era * 400 }, m, d)
}

/// Segundos UTC de un ISO 8601 (`2026-10-07T12:00:00.123Z`, `…+00:00`).
fn parse_iso(at: &str) -> Option<i64> {
    let num = |range: std::ops::Range<usize>| at.get(range)?.parse::<i64>().ok();
    let (y, mo, d, h, mi, s) = (num(0..4)?, num(5..7)?, num(8..10)?, num(11..13)?, num(14..16)?, num(17..19)?);
    let mut rest = &at[19..];
    if let Some(frac) = rest.strip_prefix('.') {
        rest = frac.trim_start_matches(|c: char| c.is_ascii_digit());
    }
    let zone = match rest.as_bytes().first() {
        Some(sign @ (b'+' | b'-')) => {
            let hh: i64 = rest.get(1..3)?.parse().ok()?;
            let mm: i64 = rest.get(4..6).and_then(|m| m.parse().ok()).unwrap_or(0);
            (hh * 60 + mm) * 60 * if *sign == b'+' { 1 } else { -1 }
        }
        _ => 0,
    };
    Some(days_from_civil(y, mo, d) * 86_400 + h * 3600 + mi * 60 + s - zone)
}

/// `fmtFull` de `timefmt.ts`: `2026-10-07 09:00:00 GMT-3`, en hora local.
fn fmt_local(at: &str, tz: Option<&Tz>) -> String {
    let (Some(utc), Some(tz)) = (parse_iso(at), tz) else { return String::new() };
    let local = utc - tz.offset_min * 60;
    let (y, m, d) = civil_from_days(local.div_euclid(86_400));
    let secs = local.rem_euclid(86_400);
    format!("{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02} {}", secs / 3600, secs / 60 % 60, secs % 60, tz.name).trim_end().to_string()
}

// ─── El contexto del desk ─────────────────────────────────────────────────────

#[derive(Clone, PartialEq)]
struct Toast {
    id: u64,
    text: String,
    class: &'static str,
}

/// El mensaje que se está enviando (la burbuja optimista de `convo.ts`).
#[derive(Clone, PartialEq)]
struct Ghost {
    text: String,
    at: String,
    error: Option<String>,
}

/// Lo que el resumen de la conversación dice del turno (más fresco que el árbol).
#[derive(Clone, Copy, Default, PartialEq)]
struct ChatState {
    busy: bool,
    responding: bool,
}

/// Un cambio de modelo que pide confirmación.
#[derive(Clone, PartialEq)]
struct SwitchAsk {
    tier: String,
    cross: bool,
    mid_turn: bool,
}

/// Todo lo que las partes del desk comparten. Las tareas se lanzan en el scope
/// del desk: un popover o un diálogo se desmontan apenas se elige la acción, y
/// `spawn` ata la tarea al componente que la lanza (la trampa de #26).
#[derive(Clone, Copy)]
struct Desk {
    client: Signal<Client>,
    org: Signal<String>,
    node: Signal<String>,
    info: Signal<Option<TreeNode>>,
    tiers: Signal<Vec<String>>,
    killswitch: Signal<bool>,
    chat: Signal<ChatState>,
    history: Signal<Vec<Row>>,
    pending: Signal<Vec<MailView>>,
    before: Signal<Option<String>>,
    has_older: Signal<bool>,
    error: Signal<Option<String>>,
    toasts: Signal<Vec<Toast>>,
    ask: Signal<Option<SwitchAsk>>,
    tz: Signal<Option<Tz>>,
    /// Los eventos cargados, para saber si una respuesta citada se puede ubicar.
    events: Signal<HashSet<String>>,
    scope: ScopeId,
}

impl Desk {
    fn spawn(self, task: impl std::future::Future<Output = ()> + 'static) {
        dioxus::core::Runtime::current().spawn(self.scope, task);
    }

    fn ids(self) -> (Client, String, String) {
        (self.client.peek().clone(), self.org.peek().clone(), self.node.peek().clone())
    }

    fn toast(self, text: impl Into<String>, class: &'static str) {
        let mut toasts = self.toasts;
        let Ok(mut list) = toasts.try_write() else { return };
        let id = list.iter().map(|t| t.id).max().unwrap_or(0) + 1;
        list.push(Toast { id, text: text.into(), class });
        drop(list);
        self.spawn(async move {
            tokio::time::sleep(TOAST_FOR).await;
            if let Ok(mut list) = toasts.try_write() {
                list.retain(|t| t.id != id);
            }
        });
    }

    fn toasts(self, lines: &[String]) {
        for line in lines {
            self.toast(line.clone(), "");
        }
    }

    /// Revela un archivo, o muestra su ruta como texto si no se puede.
    fn reveal(self, path: &str) {
        match crate::reveal::reveal(path) {
            Ok(shown) => self.toast(format!("Shown in folder: {}", shown.display()), "dx-reveal ok"),
            Err(why) => self.toast(why, "dx-reveal refused"),
        }
    }

    /// La última página de la conversación y el mail pendiente.
    async fn latest(self) {
        let (client, org, node) = self.ids();
        match client.chat(&org, &node, PAGE, None).await {
            Ok(page) => self.apply_latest(page),
            Err(e) => {
                let mut error = self.error;
                let _ = error.try_write().map(|mut w| *w = Some(e.to_string()));
            }
        }
    }

    fn apply_latest(self, page: ChatPayload) {
        let (mut history, mut before, mut has_older, mut pending, mut chat, mut events) =
            (self.history, self.before, self.has_older, self.pending, self.chat, self.events);
        let latest = rows(&page.messages, 0);
        history.with_mut(|h| {
            if h.is_empty() {
                *h = latest;
                return;
            }
            // Conservar las páginas viejas ya cargadas y reemplazar la cola desde el
            // primer mensaje de la página nueva (como el merge de `convo.ts`).
            let first = latest.first().map(|r| r.key.clone());
            let keep = first.and_then(|k| h.iter().position(|r| r.key == k)).unwrap_or(h.len());
            h.truncate(keep);
            h.extend(latest);
        });
        if before.peek().is_none() {
            before.set(page.before.clone());
            has_older.set(page.has_older == Some(true));
        }
        let now = ChatState { busy: page.busy, responding: page.responding };
        if *chat.peek() != now {
            chat.set(now);
        }
        let mail: Vec<MailView> = page.pending_mail.into_iter().map(MailView::new).collect();
        if *pending.peek() != mail {
            pending.set(mail);
        }
        let ids: HashSet<String> = self.history.peek().iter().filter_map(|r| r.event_id.clone()).collect();
        if *events.peek() != ids {
            events.set(ids);
        }
    }

    /// El agente en el árbol de la org: estado del turno, detención, modelo y esfuerzo.
    async fn load_node(self) {
        let (client, org, node) = self.ids();
        let Ok(tree) = client.tree(&org).await else { return };
        let (mut info, mut tiers, mut killswitch) = (self.info, self.tiers, self.killswitch);
        let found = tree.nodes().into_iter().find(|n| n.id == node).cloned();
        if *info.peek() != found {
            info.set(found);
        }
        let mut offered: Vec<String> = ALL_TIERS.iter().filter(|t| tree.tiers.contains_key(**t)).map(|t| t.to_string()).collect();
        offered.extend(tree.tiers.keys().filter(|t| !ALL_TIERS.contains(&t.as_str())).cloned());
        if *tiers.peek() != offered {
            tiers.set(offered);
        }
        let latched = tree.killswitch.as_ref().is_some_and(|k| !k.is_null() && *k != serde_json::Value::Bool(false));
        if *killswitch.peek() != latched {
            killswitch.set(latched);
        }
    }

    /// `op` de `App.tsx`: corre la operación y muestra sus advertencias, o el error.
    fn op(self, request: OpRequest) {
        self.spawn(async move {
            let (client, org, _) = self.ids();
            match client.op(&org, &request).await {
                Ok(result) => self.toasts(&result.warnings),
                Err(error) => self.toast(format!("error: {error}"), ""),
            }
            self.load_node().await;
        });
    }
}

// ─── El estado del turno (como `deriveTurnState` y `TurnStatusBanner`) ────────

#[derive(Clone, Copy, PartialEq, Debug)]
enum Turn {
    Working,
    Queued,
    Compacting,
    Idle,
}

fn turn_state(node: &TreeNode, chat: ChatState) -> Turn {
    if node.halt.as_ref().is_some_and(|h| h.phase == "halted") {
        return Turn::Idle;
    }
    if node.phase.as_deref() == Some("compacting") {
        return Turn::Compacting;
    }
    if node.waiting == Some(true) || node.queued_for_slot.as_ref().is_some_and(|q| !q.is_null()) {
        return Turn::Queued;
    }
    if node.busy == Some(true) || chat.busy {
        return Turn::Working;
    }
    Turn::Idle
}

#[component]
pub fn DeskView(org: String, node: String, #[props(default)] popout: bool) -> Element {
    let client = crate::engine_client();
    // El borrador del compositor se comparte entre ventanas (#13).
    let draft_key = format!("{org}/{node}");
    let composer = crate::windows::use_shared_draft(draft_key.clone());
    let mut route = use_context::<Signal<Route>>();
    let desk = Desk {
        client: use_signal(|| client.clone()),
        org: use_signal(|| org.clone()),
        node: use_signal(|| node.clone()),
        info: use_signal(|| None),
        tiers: use_signal(Vec::new),
        killswitch: use_signal(|| false),
        chat: use_signal(ChatState::default),
        history: use_signal(Vec::new),
        pending: use_signal(Vec::new),
        before: use_signal(|| None),
        has_older: use_signal(|| false),
        error: use_signal(|| None),
        toasts: use_signal(Vec::new),
        ask: use_signal(|| None),
        tz: use_signal(|| None),
        events: use_signal(HashSet::new),
        scope: dioxus::core::current_scope_id(),
    };
    use_context_provider(|| desk);
    let mut loading_older = use_signal(|| false);
    let mut draft = use_signal(String::new);
    let ghost = use_signal(|| None::<Ghost>);
    let sendmode = use_signal(|| None::<String>);

    // Al abrir: la conversación, el agente y la zona horaria del webview.
    use_hook(move || {
        desk.spawn(async move { desk.latest().await });
        desk.spawn(async move { desk.load_node().await });
        desk.spawn(async move {
            let mut eval = document::eval(
                "const name = (Intl.DateTimeFormat(undefined, { timeZoneName: 'short' }).formatToParts(new Date()) \
                 .find(p => p.type === 'timeZoneName') || {}).value || ''; \
                 dioxus.send([new Date().getTimezoneOffset(), name]);",
            );
            if let Ok((offset_min, name)) = eval.recv::<(i64, String)>().await {
                let mut tz = desk.tz;
                tz.set(Some(Tz { offset_min, name }));
            }
        });
    });
    // Como el desk actual, abre abajo: lo último de la conversación a la vista.
    let mut at_bottom_once = use_signal(|| false);
    use_effect(move || {
        if !desk.history.read().is_empty() && !at_bottom_once() {
            at_bottom_once.set(true);
            document::eval("const m = document.querySelector('.dx-desk .msgs'); if (m) m.scrollTop = m.scrollHeight;");
        }
    });

    // Los enlaces del Markdown: un archivo local se revela; nada navega el
    // webview ni abre nada. Dioxus desktop, sin esto, manda el `href` de
    // cualquier `<a>` clicado a `webbrowser::open` (`handleClickNavigate` del
    // intérprete), que puede abrir el navegador o lanzar una ruta relativa: el
    // clic se corta en la captura, antes de que llegue a su listener.
    use_future(move || async move {
        let mut eval = document::eval(
            "if (window.__dxLinks) document.removeEventListener('click', window.__dxLinks, true); \
             window.__dxLinks = e => { \
               const a = e.target && e.target.closest && e.target.closest('.dx-desk a[href]'); \
               if (!a) return; \
               e.preventDefault(); \
               e.stopPropagation(); \
               const path = a.getAttribute('data-local-path'); \
               dioxus.send(path ? { reveal: path } : { link: a.getAttribute('href') }); \
             }; \
             document.addEventListener('click', window.__dxLinks, true); \
             await new Promise(() => {});",
        );
        while let Ok(click) = eval.recv::<serde_json::Value>().await {
            if let Some(path) = click.get("reveal").and_then(|v| v.as_str()) {
                desk.reveal(path);
            } else if let Some(href) = click.get("link").and_then(|v| v.as_str()) {
                // un enlace externo no se abre desde el desk del spike: se muestra para copiarlo
                desk.toast(format!("Link: {href}"), "dx-link");
            }
        }
    });

    // WebSocket de la org: texto en vivo y avisos para refrescar.
    use_future({
        let client = client.clone();
        let (org, node) = (org.clone(), node.clone());
        move || {
            let client = client.clone();
            let (org, node) = (org.clone(), node.clone());
            async move {
                let mut events = client.subscribe(&org, Backoff::default());
                let mut connected_once = false;
                while let Some(event) = events.recv().await {
                    // varios frames juntos: una sola relectura de cada cosa
                    let mut batch = vec![event];
                    while let Ok(more) = events.try_recv() {
                        batch.push(more);
                    }
                    let (mut chat, mut tree) = (false, false);
                    for event in batch {
                        match event {
                            WsEvent::Connected => {
                                if connected_once {
                                    chat = true;
                                    tree = true;
                                }
                                connected_once = true;
                            }
                            WsEvent::Frame(Frame::NodeStream { node: who, kind, payload, .. }) if who == node => match kind.as_deref() {
                                Some("delta") => {
                                    if let Some(text) = payload.get("text").and_then(|t| t.as_str()) {
                                        let mut d = draft.write();
                                        d.push_str(text);
                                        if d.len() > DRAFT_CAP {
                                            let cut = d.len() - DRAFT_CAP;
                                            let cut = (cut..d.len()).find(|i| d.is_char_boundary(*i)).unwrap_or(0);
                                            d.drain(..cut);
                                        }
                                    }
                                }
                                Some("thinking") | Some("thinking_start") | Some("cache_forecast") | Some("mcp_tool_count") | Some("mcp_readiness") => {}
                                _ => chat = true,
                            },
                            WsEvent::Frame(Frame::NodeEvent { node: who, event, .. }) => {
                                tree = true;
                                if who == node {
                                    if event == "turn_done" {
                                        draft.set(String::new());
                                    }
                                    chat = true;
                                }
                            }
                            WsEvent::Frame(Frame::Changed { .. }) => tree = true,
                            _ => {}
                        }
                    }
                    if tree {
                        desk.load_node().await;
                    }
                    if chat {
                        desk.latest().await;
                    }
                }
            }
        }
    });

    let mut load_older = move || {
        if loading_older() || !(desk.has_older)() {
            return;
        }
        let Some(cursor) = (desk.before)() else { return };
        loading_older.set(true);
        desk.spawn(async move {
            let (client, org, node) = desk.ids();
            if let Ok(page) = client.chat(&org, &node, PAGE, Some(&cursor)).await {
                let older = rows(&page.messages, 0);
                let (mut history, mut before, mut has_older, mut events) = (desk.history, desk.before, desk.has_older, desk.events);
                history.with_mut(|h| {
                    let mut merged = older;
                    merged.extend(h.drain(..));
                    *h = merged;
                });
                before.set(page.before.clone());
                has_older.set(page.has_older == Some(true));
                let ids: HashSet<String> = history.peek().iter().filter_map(|r| r.event_id.clone()).collect();
                events.set(ids);
            }
            loading_older.set(false);
        });
    };

    let info = (desk.info)();
    let chat = (desk.chat)();
    let live = info.as_ref().is_none_or(|n| n.state == NodeState::Live);
    let back_org = org.clone();
    let popout_client = client.clone();
    let (popout_org, popout_node) = (org.clone(), node.clone());
    rsx! {
        div { class: "dx-desk desk-body",
            "data-turn": info.as_ref().map(|n| format!("{:?}", turn_state(n, chat)).to_lowercase()).unwrap_or_default(),
            header {
                if !popout {
                    button { class: "home", onclick: move |_| route.set(Route::Org(back_org.clone())), "← {org}" }
                }
                h2 { "{node}" }
                if let Some(n) = info.clone() {
                    TurnBanner { node: n }
                }
                if let Some(e) = (desk.error)() { span { class: "dim", "{e}" } }
                span { class: "dx-spacer" }
                if let Some(n) = info.clone() {
                    if n.state == NodeState::Live || n.halt.is_some() {
                        HaltControl { node: n }
                    }
                }
                if !popout {
                    button { class: "home dx-popout", title: "Abrir el desk en otra ventana",
                        onclick: move |_| crate::windows::open_desk_window(popout_client.clone(), popout_org.clone(), popout_node.clone()),
                        "⧉ Pop out"
                    }
                }
                crate::native::WindowControls {}
            }
            div { class: "msgs-wrap",
                div { class: "msgs",
                    onscroll: move |event| {
                        if event.data().scroll_top() < 400.0 {
                            load_older()
                        }
                    },
                    if (desk.has_older)() {
                        div { class: "dim small dx-earlier", if loading_older() { "loading earlier messages…" } else { "earlier messages" } }
                    }
                    for row in (desk.history)() {
                        div { key: "{row.key}", class: "dx-row", "data-reply-event": row.event_id.clone().unwrap_or_default(),
                            MessageRow { row: row.clone() }
                        }
                    }
                    PendingRows { ghost }
                    LiveDraft { draft }
                }
            }
            Composer { draft_key, composer, ghost, sendmode, live }
            if let (Some(ask), Some(n)) = ((desk.ask)(), info.clone()) {
                SwitchConfirm { ask, node: n }
            }
            DeskToasts {}
        }
    }
}

// ─── Encabezado: estado del turno y detención ─────────────────────────────────

/// `TurnStatusBanner` (desk.tsx) más la insignia de `HaltStatus`.
#[component]
fn TurnBanner(node: TreeNode) -> Element {
    let desk = use_context::<Desk>();
    let state = turn_state(&node, (desk.chat)());
    let recorded = node.last_status.as_ref().map(|s| s.status.clone()).filter(|s| !s.is_empty() && s != "idle");
    let (class, label) = match state {
        Turn::Working => ("working active".to_string(), "Active".to_string()),
        Turn::Queued => ("queued".into(), "Queued".into()),
        Turn::Compacting => ("compacting".into(), "Compacting".into()),
        Turn::Idle => match recorded {
            // fuera de turno, el último estado que informó el agente
            Some(s) => (s.clone(), format!("{}{}", s[..1].to_uppercase(), &s[1..])),
            None => ("idle".into(), "Idle".into()),
        },
    };
    let title = [Some(label.clone()), node.last_status.as_ref().and_then(|s| s.summary.clone())].into_iter().flatten().collect::<Vec<_>>().join(" · ");
    rsx! {
        span { class: "turn-status-banner {class}", title: "{title}", aria_label: "{title}",
            if state == Turn::Queued { span { class: "statusdot waiting" } }
            span { class: "turn-status-label", "{label}" }
        }
        if let Some(halt) = &node.halt {
            span { class: "badge halted", role: "status",
                title: if halt.phase == "halting" { "Turn admission is blocked; the active turn is still ending" } else { "No turn can run. Mail stays unread until explicit unhalt" },
                if halt.phase == "halting" { "Halting…" } else { "Halted" }
            }
        }
        if let Some(switch) = &node.pending_switch {
            span { class: "queued-mark", title: "a switch to {switch.tier} is QUEUED — it applies when the current turn ends; interrupting the turn applies it now",
                "→{tier_letter(&switch.tier)}"
            }
        }
    }
}

/// `HaltControl` (haltcontrol.tsx): detener sin confirmación, reanudar, y el
/// estado como aviso.
#[component]
fn HaltControl(node: TreeNode) -> Element {
    let desk = use_context::<Desk>();
    let mut pending = use_signal(|| false);
    let phase = node.halt.as_ref().map(|h| h.phase.clone());
    let halted = phase.as_deref() == Some("halted");
    let act = move |_| {
        pending.set(true);
        desk.spawn(async move {
            let (client, org, nid) = desk.ids();
            if halted {
                match client.unhalt(&org, &nid).await {
                    Ok(r) => desk.toast(
                        if r.unhalted { format!("{nid} unhalted; pending work may resume") } else { r.status.unwrap_or_else(|| "Already unhalted".into()) },
                        "",
                    ),
                    Err(e) => desk.toast(format!("error: {e}"), ""),
                }
            } else {
                match client.halt(&org, &nid).await {
                    Ok(r) => desk.toast(r.status, ""),
                    Err(e) => desk.toast(format!("error: {e}"), ""),
                }
            }
            desk.load_node().await;
            let _ = pending.try_write().map(|mut p| *p = false);
        });
    };
    rsx! {
        button { class: if halted { "halt-control" } else { "halt-control danger" },
            disabled: pending(),
            title: match phase.as_deref() {
                Some("halted") => "Allow pending work to resume",
                Some("halting") => "Check that the active turn has fully ended",
                _ => "Abruptly end this turn and block every wake until explicit unhalt",
            },
            onclick: act,
            if pending() { "Please wait…" } else if halted { "Unhalt" } else if phase.as_deref() == Some("halting") { "Finish halt" } else { "Halt" }
        }
    }
}

// ─── Filas de la conversación ─────────────────────────────────────────────────

#[component]
fn MessageRow(row: Row) -> Element {
    if row.role == "system" {
        return rsx! { SysLine { row } };
    }
    if let Some(segments) = row.segments.clone() {
        return rsx! {
            div { class: "typed-input",
                div { class: "turn-mail-batch",
                    for (i, segment) in segments.into_iter().enumerate() {
                        SegmentView { key: "{i}", segment }
                    }
                }
                if row.truncated { div { class: "trunc-note", "Shown truncated — the agent received the full message" } }
                if let Some(receipt) = &row.receipt { div { class: "trunc-note", "{receipt}" } }
            }
        };
    }
    rsx! {
        div { class: "msg {row.role}",
            if let Some(thought) = row.thinking.clone() {
                div { class: "reply-event", ThoughtLine { thought } }
            }
            for (i, tool) in row.tools.iter().enumerate() {
                ToolLine { key: "{i}", tool: tool.clone() }
            }
            if !row.html.is_empty() {
                div { class: "msgtext md", dangerous_inner_html: "{row.html}" }
            }
            if row.partial { div { class: "dim small", "partial response" } }
            if row.truncated { div { class: "trunc-note", "✂ shown truncated — the agent received the full message" } }
            if let Some(receipt) = &row.receipt { div { class: "trunc-note", "{receipt}" } }
        }
    }
}

/// `ThoughtLine` (desk.tsx): el pensamiento plegado, "thought for Ns ▸".
#[component]
fn ThoughtLine(thought: Thought) -> Element {
    let mut open = use_signal(|| false);
    let secs = thought.secs.map(|s| format!("{}s", s.round() as i64)).unwrap_or_else(|| "a moment".into());
    let Some(text) = thought.text else {
        return rsx! {
            div { class: "thoughtwrap",
                span { class: "thoughtline sealed", title: "the model's reasoning was not included in the response — only its duration is known",
                    "🧠 thought for {secs}"
                }
            }
        };
    };
    rsx! {
        div { class: "thoughtwrap",
            button { class: "thoughtline", title: if open() { "collapse" } else { "read the thought process" },
                onclick: move |_| open.toggle(),
                "🧠 thought for {secs} "
                if open() { "▾" } else { "▸" }
            }
            if open() { div { class: "thoughtbody", "{text}" } }
        }
    }
}

/// `SysLine` (desk.tsx): salida de un comando o la compactación con su resumen.
#[component]
fn SysLine(row: Row) -> Element {
    let mut open = use_signal(|| false);
    if let Some(html) = &row.cmd_out {
        return rsx! { div { class: "msg sys cmdout", div { class: "msgtext md", dangerous_inner_html: "{html}" } } };
    }
    let summary = row.summary.clone();
    rsx! {
        div { class: if summary.is_some() { "msg sys click" } else { "msg sys" },
            onclick: move |_| if row.summary.is_some() { open.toggle() },
            "{row.sys_text}"
            if summary.is_some() && !open() { " · summary ▶" }
            if let (true, Some(text)) = (open(), summary.clone()) { pre { class: "filepre", "{text}" } }
        }
    }
}

/// Un chip de herramienta (`ToolChip` de desk.tsx): el resultado plegado
/// detrás de un clic, y un archivo mandado como tarjeta que revela, no abre.
#[component]
fn ToolLine(tool: ToolChip) -> Element {
    let desk = use_context::<Desk>();
    let mut open = use_signal(|| false);
    if let Some(file) = tool.file.clone() {
        let name = file.name.clone().unwrap_or_else(|| "file".into());
        let path = file.path.clone().unwrap_or_default();
        return rsx! {
            button { class: "filecard dx-file", title: "show in folder — {path}",
                onclick: move |_| desk.reveal(&path),
                span { class: "fc-body",
                    span { class: "fc-name", "{name}" }
                    if let Some(bytes) = file.bytes { span { class: "dim", " · {fmt_bytes(bytes)}" } }
                    if let Some(note) = &file.note { span { class: "fc-note", "{note}" } }
                }
            }
        };
    }
    let expandable = tool.result.is_some();
    let lines = tool.result_lines.unwrap_or(0);
    rsx! {
        div { class: if tool.error.is_some() { "tools tchip terr" } else { "tools tchip" },
            span { class: if expandable { "tline click" } else { "tline" },
                title: if expandable { if open() { "collapse" } else { "expand" } } else { "" },
                onclick: move |_| if expandable { open.toggle() },
                span { class: "tooldot", "•" }
                " {short_tool(&tool.name)}"
                if let Some(arg) = &tool.arg { span { class: "targ", " {arg}" } }
                if tool.error.is_none() && lines > 0 { span { class: "dim", " · {lines} line{plural(lines)}" } }
                if let Some(e) = &tool.error { span { class: "terrtxt", " ⊘ {e}" } }
            }
            if let (true, Some(result)) = (open(), &tool.result) {
                pre { class: "filepre respre", "{result}" if tool.truncated == Some(true) { "\n… truncated" } }
            }
        }
    }
}

fn short_tool(name: &str) -> String {
    match name.strip_prefix("mcp__").and_then(|rest| rest.split_once("__")) {
        Some((server, tool)) => format!("{server}: {tool}"),
        None => name.to_string(),
    }
}

fn plural(n: u64) -> &'static str {
    if n == 1 { "" } else { "s" }
}

/// `fmtBytes` de `canvas/img.tsx`.
fn fmt_bytes(bytes: u64) -> String {
    match bytes {
        b if b < 1024 => format!("{b} B"),
        b if b < 1024 * 1024 => format!("{:.1} KB", b as f64 / 1024.0),
        b => format!("{:.1} MB", b as f64 / 1024.0 / 1024.0),
    }
}

/// Un segmento de un mensaje del usuario (`SegmentList` de events/segments.tsx).
#[component]
fn SegmentView(segment: SegView) -> Element {
    let desk = use_context::<Desk>();
    let tz = (desk.tz)();
    match segment {
        SegView::Text(html) => rsx! { div { class: "msg user msgtext md", dangerous_inner_html: "{html}" } },
        SegView::Card { kind, text } => rsx! {
            div { class: "event-segment-{kind}",
                section { class: "event-surface event-card",
                    div { class: "event-body", div { class: "event-field event-prose", "{text}" } }
                }
            }
        },
        SegView::Notices(rows) => rsx! {
            div { class: "event-notices",
                for (i, row) in rows.into_iter().enumerate() {
                    section { key: "{i}", class: "turn-mail event-surface event-card event-ordinary passive dx-notice",
                        header { class: "turn-mail-head event-head",
                            span { class: "event-family", title: "Notice", "·" }
                            strong { "Notice" }
                            time { class: "event-time", "{fmt_local(&row.at, tz.as_ref())}" }
                        }
                        div { class: "event-body", div { class: "event-field event-prose", "{row.text}" } }
                    }
                }
            }
        },
        SegView::Mail(rows) => rsx! {
            div { class: "event-mail",
                for (i, mail) in rows.into_iter().enumerate() {
                    MailCard { key: "{i}", mail, pending: false }
                }
            }
        },
    }
}

/// El nombre de una fila de mail: su variante tipada o su `kind`.
fn mail_label(row: &MailRow) -> String {
    let raw = row.variant().and_then(|v| v.rsplit('.').next().map(str::to_string)).unwrap_or_else(|| row.kind.clone().unwrap_or_else(|| "message".into()));
    let words = raw.replace('_', " ");
    let mut chars = words.chars();
    chars.next().map(|c| c.to_uppercase().collect::<String>() + chars.as_str()).unwrap_or_default()
}

fn sender(from: &str) -> String {
    match from {
        "@user" | "user" => "User".into(),
        "@system" | "system" => "System".into(),
        other => other.to_string(),
    }
}

/// `MailMessage` (events/segments.tsx): la misma tarjeta para el mail entregado
/// y el pendiente, con la respuesta citada y los adjuntos.
#[component]
fn MailCard(mail: MailView, pending: bool, #[props(default)] meta: Option<Element>) -> Element {
    let desk = use_context::<Desk>();
    let tz = (desk.tz)();
    let row = mail.row.clone();
    let notice = row.is_notice();
    rsx! {
        section { class: if notice { "turn-mail event-surface event-card event-ordinary passive" } else { "turn-mail event-surface event-card event-ordinary" },
            "data-mail-id": row.id.clone().unwrap_or_default(),
            header { class: "turn-mail-head event-head",
                span { class: "event-family", aria_label: "Message", "·" }
                strong { title: "Recorded mail kind: {row.kind.clone().unwrap_or_default()}", "{mail_label(&row)}" }
                span { class: "event-actor", "{sender(&row.from)}" }
                time { "{fmt_local(&row.at, tz.as_ref())}" }
                if let Some(rel) = &row.relationship { span { "{rel}" } }
                if notice { span { class: "turn-mail-passive", "no reply expected" } }
                {meta}
            }
            if let Some(reply) = row.reply() {
                ReplyPreview { event_id: reply.event_id, quote: reply.quote }
            }
            div { class: "event-body",
                div { class: "event-field", "data-event-field": "body",
                    div { class: "event-prose md", dangerous_inner_html: "{mail.html}" }
                }
            }
            if !row.attachments.is_empty() {
                div { class: "attach-row",
                    for (i, file) in row.attachments.iter().cloned().enumerate() {
                        AttachmentChip { key: "{i}", name: file.name.clone().or_else(|| file.path.as_ref().and_then(|p| p.rsplit('/').next().map(str::to_string))).unwrap_or_else(|| "File".into()), path: file.path.clone(), bytes: file.bytes }
                    }
                }
            }
            for name in row.attachments_missing.iter() {
                div { class: "dim", "Attachment unavailable: {name}" }
            }
            if pending && row.delivering == Some(true) {
                div { class: if row.stage.as_deref() == Some("stranded") { "dim pend-tag warn" } else { "dim pend-tag" }, "{pend_tag(&row)}" }
            }
        }
    }
}

/// Un adjunto: un clic intenta revelarlo; la ruta del mail es relativa a la
/// carpeta del agente, así que el desk la muestra como texto.
#[component]
fn AttachmentChip(name: String, path: Option<String>, bytes: Option<u64>) -> Element {
    let desk = use_context::<Desk>();
    let target = path.clone().unwrap_or_default();
    rsx! {
        button { class: "attach-chip dx-attach", title: "show in folder — {target}",
            onclick: move |_| desk.reveal(&target),
            "📄 {name}"
            if let Some(b) = bytes { span { class: "dim", " {fmt_bytes(b)}" } }
        }
    }
}

/// `ReplyPreview` (replypreview.tsx): el texto citado y el salto al original
/// si está en la conversación cargada.
#[component]
fn ReplyPreview(event_id: String, quote: String) -> Element {
    let desk = use_context::<Desk>();
    let available = !event_id.is_empty() && desk.events.read().contains(&event_id);
    let target = event_id.clone();
    rsx! {
        aside { class: "reply-preview", aria_label: "Replying to chat event",
            div { class: "reply-preview-head",
                button { class: "reply-preview-jump", r#type: "button", disabled: !available, aria_label: "jump to message",
                    title: if available { "Show the original event" } else { "Original event is not in this loaded conversation" },
                    onclick: move |_| {
                        document::eval(&format!(
                            "const el = document.querySelector('.dx-desk [data-reply-event=\"{}\"]'); if (el) el.scrollIntoView({{ block: 'center' }});",
                            target.replace('\\', "").replace('"', "")
                        ));
                    },
                    "↩ jump to message"
                }
            }
            blockquote { if quote.is_empty() { "(event without visible text)" } else { "{quote}" } }
            if !available { span { class: "dim", "Original event unavailable here; quoted context is retained." } }
        }
    }
}

/// `pendTag` (desk.tsx): dónde está la entrega de un mail pendiente.
fn pend_tag(row: &MailRow) -> &'static str {
    match row.stage.as_deref() {
        Some("stranded") => "⚠ stuck — no turn owns this message; report it (an orgtree restart re-presents it)",
        Some("queued") => "queued for a future turn boundary — not read yet",
        Some("requested") => "steering requested — awaiting the running process",
        Some("claimed") => "claimed for the hook — awaiting its receipt…",
        Some("acked") => "received by the hook — awaiting the CLI’s record…",
        Some("turn") => "queued for this turn — awaiting the provider receipt",
        None if row.via.as_deref() == Some("turn") => "queued for this turn — awaiting the provider receipt",
        _ => "queued mid-task — waiting for a safe tool boundary",
    }
}

/// El mail que el agente todavía no leyó (`PendingMailRow`, con el ✕ para
/// retirarlo) y el envío en curso (`PendingGhostRow`).
#[component]
fn PendingRows(ghost: Signal<Option<Ghost>>) -> Element {
    let desk = use_context::<Desk>();
    let list = (desk.pending)();
    rsx! {
        for mail in list {
            div { key: "{mail.row.id.clone().unwrap_or_default()}", class: "pending pendrow dx-row",
                "data-reply-event": mail.row.event_id.clone().unwrap_or_default(),
                MailCard {
                    mail: mail.clone(),
                    pending: true,
                    meta: match (&mail.row.id, mail.row.delivering) {
                        (Some(id), None | Some(false)) => {
                            let id = id.clone();
                            Some(rsx! {
                                button { class: "chip-x pend-x", title: "retract (undelivered)",
                                    onclick: move |_| {
                                        let id = id.clone();
                                        desk.spawn(async move {
                                            let (client, org, node) = desk.ids();
                                            if let Err(e) = client.retract_mail(&org, &node, &id).await {
                                                desk.toast(format!("error: {e}"), "");
                                            }
                                            desk.latest().await;
                                        });
                                    },
                                    "✕"
                                }
                            })
                        }
                        _ => None,
                    },
                }
            }
        }
        if let Some(g) = ghost() {
            div { class: if g.error.is_some() { "pending pendghost failed dx-row" } else { "pending pendghost dx-row" },
                MailCard {
                    mail: MailView::new(MailRow { from: "@user".into(), kind: Some("message".into()), body: g.text.clone(), at: g.at.clone(), ..Default::default() }),
                    pending: true,
                    meta: Some(rsx! {
                        span { class: "ghost-acts",
                            button { class: "chip-x", title: "dismiss", onclick: move |_| ghost.set(None), "✕" }
                        }
                    }),
                }
                if let Some(e) = &g.error {
                    div { class: "ghost-why", role: "status", "Send was not confirmed: {e}. Delivery is unknown; check before retrying." }
                }
            }
        }
    }
}

/// El texto en vivo, en su propio componente: un frame solo re-renderiza esto.
#[component]
fn LiveDraft(draft: Signal<String>) -> Element {
    // Si se está mirando el final, el texto en vivo lo sigue (como el desk actual).
    use_effect(move || {
        let _ = draft.read().len();
        document::eval(
            "const m = document.querySelector('.dx-desk .msgs'); \
             if (m && m.scrollHeight - m.scrollTop - m.clientHeight < 160) m.scrollTop = m.scrollHeight;",
        );
    });
    let text = draft();
    if text.is_empty() {
        return rsx! {};
    }
    rsx! { div { class: "msg assistant live md draft", "{text}" } }
}

// ─── Compositor ───────────────────────────────────────────────────────────────

fn now_iso() -> String {
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
    let (y, m, d) = civil_from_days(secs.div_euclid(86_400));
    let s = secs.rem_euclid(86_400);
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z", s / 3600, s / 60 % 60, s % 60)
}

/// El compositor (`cc-composer` de desk.tsx): el borrador compartido entre
/// ventanas, enviar (Enter; Shift+Enter es un salto de línea), STOP con una
/// respuesta en curso, el modelo y el esfuerzo. Arriba, los avisos de por qué
/// el agente no corre (detenido o en cola por el límite de turnos).
#[component]
fn Composer(draft_key: String, composer: Signal<String>, ghost: Signal<Option<Ghost>>, sendmode: Signal<Option<String>>, live: bool) -> Element {
    let desk = use_context::<Desk>();
    let info = (desk.info)();
    let node = (desk.node)();
    let archived = info.as_ref().is_some_and(|n| n.state == NodeState::Archived);
    // un agente retirado igual recibe mail (queda en su buzón hasta recontratarlo)
    let can_mail = live || archived;
    let responding = (desk.chat)().responding || info.as_ref().is_some_and(|n| n.responding == Some(true));
    let send = {
        let draft_key = draft_key.clone();
        move || {
            let text = crate::windows::draft(&draft_key).trim().to_string();
            if text.is_empty() || !can_mail {
                return;
            }
            crate::windows::set_draft(&draft_key, String::new());
            let mut ghost = ghost;
            let mut sendmode = sendmode;
            ghost.set(Some(Ghost { text: text.clone(), at: now_iso(), error: None }));
            sendmode.set(None);
            document::eval("const m = document.querySelector('.dx-desk .msgs'); if (m) m.scrollTop = m.scrollHeight;");
            desk.spawn(async move {
                let (client, org, node) = desk.ids();
                let op = format!("dx-{}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0));
                let message = SendMessage { text, client_op: Some(op), ..Default::default() };
                match client.send_message(&org, &node, &message).await {
                    Ok(result) => {
                        let _ = sendmode.try_write().map(|mut m| *m = Some(result.mode()));
                        desk.toasts(&result.warnings);
                        // la copia durable llega en `pending_mail` (o ya en la conversación)
                        desk.latest().await;
                        let _ = ghost.try_write().map(|mut g| *g = None);
                    }
                    Err(e) => {
                        let _ = ghost.try_write().map(|mut g| {
                            if let Some(g) = g.as_mut() {
                                g.error = Some(e.to_string());
                            }
                        });
                        desk.toast(format!("error: {e}"), "");
                    }
                }
            });
        }
    };
    let send_key = send.clone();
    let send_click = send;
    let placeholder = if live {
        format!("message {node}…")
    } else if archived {
        format!("message {node} — queued until rehire…")
    } else {
        info.as_ref().map(|n| format!("{:?}", n.state).to_lowercase()).unwrap_or_default()
    };
    let killswitched = (desk.killswitch)();
    rsx! {
        if let Some(mode) = sendmode() { div { class: "sendmode dim", "{mode}" } }
        if let Some(n) = info.clone() {
            HaltedBanner { node: n.clone(), killswitched }
            if n.state == NodeState::Live {
                if let Some(q) = n.queued_for_slot.clone().filter(|q| !q.is_null()) {
                    SlotQueuedBanner { queued: q }
                }
            }
        }
        div { class: if can_mail { "cc-composer dx-composer" } else { "cc-composer dx-composer off" },
            textarea { rows: 2, placeholder: "{placeholder}", disabled: !can_mail,
                value: "{composer}",
                oninput: move |event| {
                    sendmode.set(None);
                    crate::windows::set_draft(&draft_key, event.value())
                },
                onkeydown: move |event| {
                    if event.key() == Key::Enter && !event.modifiers().shift() {
                        event.prevent_default();
                        send_key();
                    }
                },
            }
            if let Some(n) = info.clone() {
                ModelSwitch { node: n.clone() }
                EffortButton { value: n.own_effort(), effective: n.effort_effective.clone().unwrap_or_default() }
            }
            // STOP solo cuando una interrupción puede llegar (una respuesta en
            // curso); Enter sigue encolando el mensaje.
            if responding {
                button { class: "cc-send stop", title: "interrupt the current response — Enter still queues your message",
                    onclick: move |_| desk.spawn(async move {
                        let (client, org, node) = desk.ids();
                        match client.interrupt(&org, &node).await {
                            Ok(r) if !r.interrupted => desk.toast(format!("error: {}", r.reason.unwrap_or_default()), ""),
                            Ok(_) => {}
                            Err(e) => desk.toast(format!("error: {e}"), ""),
                        }
                    }),
                    svg { width: "1em", height: "1em", view_box: "0 0 24 24", fill: "currentColor", path { d: "M6 6h12v12H6z" } }
                }
            } else {
                button { class: "cc-send", disabled: !can_mail || composer().trim().is_empty(), title: "send",
                    onclick: move |_| send_click(),
                    svg { width: "1em", height: "1em", view_box: "0 0 24 24", fill: "currentColor", path { d: "M4 12l1.41 1.41L11 7.83V20h2V7.83l5.58 5.59L20 12l-8-8-8 8z" } }
                }
            }
        }
    }
}

/// `HaltedBanner` (desk.tsx): por qué no pasa nada al enviar. El compositor
/// no se deshabilita: el motor retiene el mail sin leer.
#[component]
fn HaltedBanner(node: TreeNode, killswitched: bool) -> Element {
    if node.state != NodeState::Live || (node.halt.is_none() && !killswitched) {
        return rsx! {};
    }
    let text = match (&node.halt, killswitched) {
        (Some(_), true) => "Halted — this agent is individually halted AND the org killswitch is latched. Mail is stored unread; no turn can run until both are cleared.",
        (Some(h), false) if h.phase == "halting" => "Halting — admission is closed while the active turn settles. Mail is stored unread until explicit unhalt.",
        (Some(_), false) => "Halted — no turn can run and mail is stored unread until this agent is explicitly unhalted.",
        (None, _) => "Org killswitch latched — every agent here is halted. Mail is stored unread until the killswitch is released.",
    };
    rsx! { div { class: "halted-send-warning", role: "status", "⚠ " span { "{text}" } } }
}

/// `TurnSlotQueuedBanner` (desk.tsx): en cola detrás del límite de turnos de
/// la máquina, con la explicación. Los ajustes de la app quedan fuera del
/// recorte, así que el botón aparece deshabilitado.
#[component]
fn SlotQueuedBanner(queued: serde_json::Value) -> Element {
    let limit = queued.get("limit").and_then(|v| v.as_u64()).unwrap_or(0);
    let others = queued.get("waiting").and_then(|v| v.as_u64()).unwrap_or(0).saturating_sub(1);
    let tail = if others > 0 { format!(" ({others} other{} were waiting when it queued)", if others == 1 { "" } else { "s" }) } else { String::new() };
    rsx! {
        div { class: "slot-queued-warning", role: "status",
            "⚠ "
            span { "Waiting for a turn slot — the agent concurrency limit ({limit}) is reached, so this agent runs when a running turn finishes{tail}. The limit may be too low for this many agents; you can change it in Settings." }
            button { r#type: "button", class: "slot-queued-open", disabled: true, title: "Los ajustes de la app quedan fuera del recorte del spike", "Open settings" }
        }
    }
}

/// El modelo del agente. Un cambio dentro del mismo proveedor y fuera de un
/// turno es un clic; a mitad de turno queda en cola, y a otro proveedor es una
/// división de linaje: los dos piden confirmación (`modals.tsx`). Elegir el
/// modelo actual con un cambio en cola lo cancela.
#[component]
fn ModelSwitch(node: TreeNode) -> Element {
    let desk = use_context::<Desk>();
    let mut epoch = use_signal(|| 0u32);
    let tiers = (desk.tiers)();
    let current = node.tier.clone();
    let busy = turn_state(&node, (desk.chat)()) == Turn::Working || node.busy == Some(true);
    let pending = node.pending_switch.as_ref().map(|s| s.tier.clone());
    let nid = node.id.clone();
    rsx! {
        select { key: "{epoch}", class: "dx-model", aria_label: "model for {nid}",
            title: match &pending { Some(t) => format!("a switch to {t} is QUEUED — choose {current} to cancel it"), None => format!("model — {current}") },
            value: "{current}",
            onchange: move |event| {
                let tier = event.value();
                epoch += 1;
                if tier == current {
                    if pending.is_some() {
                        // la puerta de cancelación del ledger: el mismo op con el tier actual
                        desk.op(OpRequest::switch_model(&nid, &tier));
                    }
                    return;
                }
                let cross = provider_of(&tier).0 != provider_of(&current).0;
                if cross || busy {
                    let mut ask = desk.ask;
                    ask.set(Some(SwitchAsk { tier, cross, mid_turn: busy }));
                } else {
                    desk.op(OpRequest::switch_model(&nid, &tier));
                }
            },
            for tier in tiers {
                option { key: "{tier}", value: "{tier}", selected: tier == node.tier,
                    "{tier}"
                    if pending.as_deref() == Some(tier.as_str()) { " (queued)" }
                }
            }
        }
    }
}

/// La confirmación de un cambio de modelo, con los textos de `modals.tsx`.
#[component]
fn SwitchConfirm(ask: SwitchAsk, node: TreeNode) -> Element {
    let desk = use_context::<Desk>();
    let mut open = desk.ask;
    let (from, to) = (provider_of(&node.tier).1, provider_of(&ask.tier).1);
    let id = node.id.clone();
    let model = ask.tier.clone();
    let title = if ask.mid_turn { format!("queue {id}'s switch to {model}?") } else { format!("move {id} from {from} to {to}?") };
    let mut body = String::new();
    if ask.mid_turn {
        body += &format!("{id} is MID-TURN. A model switch asked for mid-turn is QUEUED, not applied: nothing changes until this turn ends, then {model} applies from its next turn. To switch it now, interrupt the turn first (⏸ on its desk), then save.");
        if ask.cross {
            body.push(' ');
        }
    }
    if ask.cross {
        body += &format!(
            "{id} is running on {from} and {model} runs on {to}. Its conversation CANNOT move between providers, so {} will be reset from its next turn and it will not remember this conversation. The conversation is not lost: its current self is archived in place as the knowledge bearer {id}@{} — readable from the lineage panel, and rehireable there on {from} to consult it. Its scratch files, breadcrumbs.md and mail all survive, and it is told to read them to pick up where it left off.",
            if ask.mid_turn { "when the switch applies it" } else { "it" },
            node.generation
        );
    }
    let label = if ask.mid_turn { format!("queue the switch to {model}") } else { format!("switch to {model} and reset the conversation") };
    let confirm = move |_| {
        open.set(None);
        desk.op(OpRequest::switch_model(&id, &model));
    };
    rsx! {
        div { class: "overlay", onclick: move |_| open.set(None),
            div { class: "settings content-height confirm-box dx-confirm dx-switch-confirm", role: "dialog", "aria-modal": "true",
                onclick: move |e| e.stop_propagation(),
                h3 { "{title}" }
                div { class: "confirm-body", "{body}" }
                div { class: "row",
                    button { class: "danger solid", onclick: confirm, "{label}" }
                    button { onclick: move |_| open.set(None), "cancel" }
                }
            }
        }
    }
}

/// `effortChangeToast` (canvas/effort.tsx).
fn effort_toast(node: &str, requested: &str, result: &OpResult) -> String {
    let delivery = result.extra.get("effort_delivery");
    let get = |k: &str| delivery.and_then(|d| d.get(k)).and_then(|v| v.as_str()).unwrap_or_default().to_string();
    let level = Some(get("effort")).filter(|l| EFFORT_LEVELS.contains(&l.as_str())).unwrap_or_default();
    let what = if requested.is_empty() {
        format!("{node} thinking effort: back to the org default{}", if level.is_empty() { String::new() } else { format!(" ({level})") })
    } else {
        format!("{node} thinking effort: {requested}")
    };
    match get("delivery").as_str() {
        "sent" => format!("{what} — sent to the running agent"),
        "next_turn" => format!("{what} — applies from its next turn"),
        "unchanged" => format!("{what} (unchanged)"),
        _ => what,
    }
}

/// `EffortButton` + `EffortSwitch` (desk.tsx): un botón chico junto a enviar
/// y la pista de cinco puntos en un popover. Clic en el punto activo vuelve al
/// esfuerzo heredado. Optimista hasta que el árbol responde.
#[component]
fn EffortButton(value: String, effective: String) -> Element {
    let desk = use_context::<Desk>();
    let mut open = use_signal(|| false);
    let mut pending = use_signal(|| None::<String>);
    // el árbol habló: se suelta lo optimista
    let seen = use_signal(|| (value.clone(), effective.clone()));
    if *seen.peek() != (value.clone(), effective.clone()) {
        let (mut seen, mut pending) = (seen, pending);
        seen.set((value.clone(), effective.clone()));
        pending.set(None);
    }
    let own = pending().unwrap_or(value.clone());
    let shown = if !own.is_empty() { own.clone() } else { effective.clone() };
    let why = if value.is_empty() { "inherited — change it on this agent, or org-wide in ⚙ settings" } else { "set on this agent" };
    let class = format!(
        "cc-eff{}{}",
        if !own.is_empty() { " set" } else if !shown.is_empty() { " inherited" } else { "" },
        if pending().is_some() { " saving" } else { "" }
    );
    let pinned = EFFORT_LEVELS.iter().position(|l| *l == own);
    let idx = pinned.or_else(|| EFFORT_LEVELS.iter().position(|l| *l == shown));
    let set_here = if own.is_empty() { "inherited" } else { "set here" };
    let label = if own.is_empty() { format!("Effort ({shown} — {set_here})") } else { format!("Effort ({shown})") };
    rsx! {
        span { class: "eff-wrap",
            button { r#type: "button", class: "{class}", title: "thinking effort — {shown} ({why})",
                onclick: move |_| open.toggle(),
                if shown.is_empty() { "effort" } else { "{shown}" }
            }
            if open() {
                span { class: "eff-pop",
                    span { class: "effort-switch",
                        title: "thinking effort — {shown} ({set_here}); click a dot to set, click the active dot to clear back to inherit",
                        span { class: "eff-label", "{label}" }
                        span { class: "eff-track",
                            for (i, level) in EFFORT_LEVELS.iter().enumerate() {
                                button { key: "{level}", r#type: "button", title: "{level}",
                                    class: format!("eff-dot{}{}{}",
                                        if Some(i) == idx { " on" } else { "" },
                                        if idx.is_some_and(|x| i < x) { " below" } else { "" },
                                        if pinned.is_none() { " faint" } else { "" }),
                                    onclick: move |_| {
                                        let lvl = if Some(i) == pinned { String::new() } else { level.to_string() };
                                        pending.set(Some(lvl.clone()));
                                        open.set(false);
                                        desk.spawn(async move {
                                            let (client, org, node) = desk.ids();
                                            match client.save_scope(&org, &node, &serde_json::json!({ "effort": lvl })).await {
                                                Ok(r) => desk.toast(effort_toast(&node, &lvl, &r), ""),
                                                Err(e) => {
                                                    desk.toast(format!("error: {e}"), "");
                                                    let _ = pending.try_write().map(|mut p| *p = None);
                                                }
                                            }
                                            desk.load_node().await;
                                        });
                                    },
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// Los avisos del desk (`.toasts` de `App.tsx`): clic para cerrar.
#[component]
fn DeskToasts() -> Element {
    let desk = use_context::<Desk>();
    let mut toasts = desk.toasts;
    rsx! {
        div { class: "toasts",
            for toast in toasts() {
                div { key: "{toast.id}", class: "toast {toast.class}",
                    onclick: move |_| toasts.write().retain(|t| t.id != toast.id),
                    "{toast.text}"
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markdown_seguro_y_enlaces_locales() {
        let html = markdown("**hola** <img src=x onerror=\"alert(1)\"> [informe](<C:\\a b\\_x\\informe.txt>) y [web](https://example.com)");
        assert!(html.contains("<strong>hola</strong>"));
        assert!(!html.contains("onerror"));
        assert!(html.contains("data-local-path=\"C:\\a b\\_x\\informe.txt\""), "{html}");
        assert!(html.contains("class=\"local-file\""));
        assert!(html.contains("href=\"#\""));
        assert!(html.contains("href=\"https://example.com\""));
    }

    #[test]
    fn hora_local_sin_utc() {
        let tz = Tz { offset_min: 180, name: "GMT-3".into() };
        assert_eq!(fmt_local("2026-10-07T12:00:00Z", Some(&tz)), "2026-10-07 09:00:00 GMT-3");
        assert_eq!(fmt_local("2026-10-07T01:30:05.123+00:00", Some(&tz)), "2026-10-06 22:30:05 GMT-3");
        assert_eq!(fmt_local("2026-12-31T23:00:00-02:00", Some(&Tz { offset_min: 0, name: "UTC".into() })), "2027-01-01 01:00:00 UTC");
        assert_eq!(fmt_local("no es fecha", Some(&tz)), "");
        assert_eq!(now_iso().len(), 20);
    }

    #[test]
    fn el_esfuerzo_como_el_renderer() {
        let result: OpResult = serde_json::from_value(serde_json::json!({"effort_delivery": {"delivery": "next_turn", "effort": "low"}})).unwrap();
        assert_eq!(effort_toast("worker", "low", &result), "worker thinking effort: low — applies from its next turn");
        assert_eq!(effort_toast("worker", "", &result), "worker thinking effort: back to the org default (low) — applies from its next turn");
        assert_eq!(mail_label(&MailRow { kind: Some("status".into()), ..Default::default() }), "Status");
        assert_eq!(short_tool("mcp__orgtree__orgtree_status"), "orgtree: orgtree_status");
    }
}
