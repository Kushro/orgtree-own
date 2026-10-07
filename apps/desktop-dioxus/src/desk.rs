//! El desk de un agente en RSX (#12): la conversación, las herramientas y el
//! texto en vivo. Reescribe lo esencial de `renderer/src/canvas/desk.tsx` y
//! `convo.ts` con las mismas clases (y el mismo CSS) del renderer.
//!
//! Lo que el desk actual resuelve en JS y acá se rediseñó:
//! - **Markdown**: `marked` + DOMPurify en cada render → `pulldown-cmark` +
//!   `ammonia` en Rust, una sola vez por mensaje al cargarlo (`Row::html`).
//! - **Lista larga**: la ventana medida de `convo.ts` (filas visibles, alturas
//!   medidas) → `content-visibility: auto` en cada fila: el webview no pinta
//!   ni maqueta lo que está fuera de la vista.
//! - **Texto en vivo**: los `delta` de `node_stream` van a una señal aparte
//!   (`draft`), así un frame no vuelve a renderizar la lista.
//! - **Páginas anteriores**: se piden al llegar arriba con el scroll, con el
//!   cursor `before`; el anclaje de scroll del webview mantiene la posición.
//! - **Refresco**: un frame durable (`text`, herramientas, `turn_done`) o una
//!   reconexión vuelve a pedir la última página, como el `nudge` de `convo.ts`.

use crate::Route;
use dioxus::prelude::*;
use orgtree_engine_client::{Backoff, ChatMessage, ChatPayload, Frame, ToolChip, WsEvent};

/// Igual que el renderer: 300 mensajes por página.
const PAGE: u32 = 300;
/// El borrador en vivo se recorta como en `convo.ts`.
const DRAFT_CAP: usize = 12_000;

#[derive(Clone, PartialEq)]
struct Row {
    key: String,
    role: String,
    /// Markdown ya convertido y sanitizado.
    html: String,
    tools: Vec<ToolChip>,
}

fn markdown(text: &str) -> String {
    use pulldown_cmark::{html, Options, Parser};
    let mut options = Options::empty();
    options.insert(Options::ENABLE_TABLES);
    options.insert(Options::ENABLE_STRIKETHROUGH);
    options.insert(Options::ENABLE_TASKLISTS);
    let mut out = String::new();
    html::push_html(&mut out, Parser::new_ext(text, options));
    // Sin scripts, handlers on*, iframes ni URLs javascript:, como DOMPurify.
    ammonia::Builder::default().link_rel(Some("noopener noreferrer")).clean(&out).to_string()
}

fn rows(messages: &[ChatMessage], offset: usize) -> Vec<Row> {
    messages
        .iter()
        .enumerate()
        .map(|(i, m)| Row {
            key: m.row_id.clone().or_else(|| m.event_id.clone()).unwrap_or_else(|| format!("i{}", offset + i)),
            role: m.role.clone(),
            html: if m.text.is_empty() { String::new() } else { markdown(&m.text) },
            tools: m.tools.clone(),
        })
        .collect()
}

#[component]
pub fn DeskView(org: String, node: String, #[props(default)] popout: bool) -> Element {
    let client = crate::engine_client();
    // El borrador del compositor se comparte entre ventanas (#13).
    let draft_key = format!("{org}/{node}");
    let composer = crate::windows::use_shared_draft(draft_key.clone());
    let mut route = use_context::<Signal<Route>>();
    let mut history = use_signal(Vec::<Row>::new);
    let mut before = use_signal(|| None::<String>);
    let mut has_older = use_signal(|| false);
    let mut loading_older = use_signal(|| false);
    let mut error = use_signal(|| None::<String>);
    let mut draft = use_signal(String::new);

    // Última página: al abrir, tras un frame durable y tras reconectar.
    let latest = {
        let client = client.clone();
        let (org, node) = (org.clone(), node.clone());
        move || {
            let client = client.clone();
            let (org, node) = (org.clone(), node.clone());
            spawn(async move {
                match client.chat(&org, &node, PAGE, None).await {
                    Ok(page) => apply_latest(page, history, before, has_older),
                    Err(e) => error.set(Some(e.to_string())),
                }
            });
        }
    };
    use_hook({
        let latest = latest.clone();
        move || latest()
    });
    // Como el desk actual, abre abajo: lo último de la conversación a la vista.
    let mut at_bottom_once = use_signal(|| false);
    use_effect(move || {
        if !history.read().is_empty() && !at_bottom_once() {
            at_bottom_once.set(true);
            document::eval("const m = document.querySelector('.dx-desk .msgs'); if (m) m.scrollTop = m.scrollHeight;");
        }
    });

    // WebSocket de la org: texto en vivo y avisos para refrescar.
    use_future({
        let client = client.clone();
        let (org, node) = (org.clone(), node.clone());
        let latest = latest.clone();
        move || {
            let client = client.clone();
            let (org, node) = (org.clone(), node.clone());
            let latest = latest.clone();
            async move {
                let mut events = client.subscribe(&org, Backoff::default());
                let mut connected_once = false;
                while let Some(event) = events.recv().await {
                    match event {
                        WsEvent::Connected => {
                            if connected_once {
                                latest();
                            }
                            connected_once = true;
                        }
                        WsEvent::Frame(Frame::NodeStream { node: who, kind, payload, .. }) if who == node => {
                            match kind.as_deref() {
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
                                _ => latest(),
                            }
                        }
                        WsEvent::Frame(Frame::NodeEvent { node: who, event, .. }) if who == node => {
                            if event == "turn_done" {
                                draft.set(String::new());
                            }
                            latest();
                        }
                        _ => {}
                    }
                }
            }
        }
    });

    let mut load_older = {
        let client = client.clone();
        let (org, node) = (org.clone(), node.clone());
        move || {
            if loading_older() || !has_older() {
                return;
            }
            let Some(cursor) = before() else { return };
            loading_older.set(true);
            let client = client.clone();
            let (org, node) = (org.clone(), node.clone());
            spawn(async move {
                if let Ok(page) = client.chat(&org, &node, PAGE, Some(&cursor)).await {
                    let older = rows(&page.messages, 0);
                    history.with_mut(|h| {
                        let mut merged = older;
                        merged.extend(h.drain(..));
                        *h = merged;
                    });
                    before.set(page.before.clone());
                    has_older.set(page.has_older == Some(true));
                }
                loading_older.set(false);
            });
        }
    };

    let back_org = org.clone();
    let popout_client = client.clone();
    let (popout_org, popout_node) = (org.clone(), node.clone());
    rsx! {
        div { class: "dx-desk desk-body",
            header {
                if !popout {
                    button { class: "home", onclick: move |_| route.set(Route::Org(back_org.clone())), "← {org}" }
                }
                h2 { "{node}" }
                if let Some(e) = error() { span { class: "dim", "{e}" } }
                if !popout {
                    button { class: "home dx-popout", title: "Abrir el desk en otra ventana",
                        onclick: move |_| crate::windows::open_desk_window(popout_client.clone(), popout_org.clone(), popout_node.clone()),
                        "⧉ Pop out"
                    }
                }
            }
            div { class: "msgs-wrap",
                div { class: "msgs",
                    onscroll: move |event| {
                        if event.data().scroll_top() < 400.0 {
                            load_older()
                        }
                    },
                    if has_older() {
                        div { class: "dim small dx-earlier", if loading_older() { "loading earlier messages…" } else { "earlier messages" } }
                    }
                    for row in history() {
                        MessageRow { key: "{row.key}", row: row.clone() }
                    }
                    LiveDraft { draft }
                }
            }
            // Compositor mínimo: el borrador compartido. Enviar queda fuera del recorte.
            div { class: "cc-composer dx-composer",
                textarea { rows: 2, placeholder: "Borrador para {node} (enviar queda fuera del recorte)",
                    value: "{composer}",
                    oninput: move |event| crate::windows::set_draft(&draft_key, event.value()),
                }
            }
        }
    }
}

fn apply_latest(page: ChatPayload, mut history: Signal<Vec<Row>>, mut before: Signal<Option<String>>, mut has_older: Signal<bool>) {
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
}

#[component]
fn MessageRow(row: Row) -> Element {
    rsx! {
        div { class: "msg {row.role}",
            for (i, tool) in row.tools.iter().enumerate() {
                div { key: "{i}", class: if tool.error.is_some() { "tools tchip terr" } else { "tools tchip" },
                    span { class: "tline",
                        span { class: "tooldot", "•" }
                        " {tool.name}"
                        if let Some(arg) = &tool.arg { span { class: "targ", " {arg}" } }
                        if let Some(e) = &tool.error { span { class: "terrtxt", " ⊘ {e}" } }
                    }
                }
            }
            if !row.html.is_empty() {
                div { class: "msgtext md", dangerous_inner_html: "{row.html}" }
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
