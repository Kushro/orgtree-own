//! La bandeja del usuario y las preguntas de los agentes en RSX (#28), con las
//! clases y el CSS del renderer: la bandeja de `App.tsx` (`InboxPanel`), las
//! filas y el panel de lectura de `canvas/mail.tsx` (`MailRowView`,
//! `MailReadPane`, `MailReplyBox`) y la tarjeta de pedidos de
//! `canvas/asks.tsx` (`BatchAsk`), con los mismos endpoints que `api.ts`.
//!
//! - **Leer y archivar.** Como el renderer, un mail leído se archiva cuando el
//!   usuario sale de él (selecciona otro o cierra la bandeja): pasa del grupo
//!   sin leer al archivo de leídos (`markRead`). "Mark all read" archiva todo
//!   (`clearInbox`). La marca es optimista y vuelve atrás si el motor la
//!   rechaza (`markReadNow`).
//! - **Responder** va al remitente con la identidad del mail como `target`
//!   (`replyMessage`), y con un recibo durable el original queda leído.
//! - **Preguntas.** Viajan en la bandeja como filas propias (`askMailRow`) y
//!   su panel es la tarjeta de pedidos. Responder y descartar (la ✕, que salta
//!   todas las pestañas) resuelven la tarjeta entera de una vez
//!   (`resolveBatch`); la tarjeta se va en el clic y vuelve si falla.

use crate::desk::{fmt_local, fmt_short, markdown};
use crate::org::Ctx;
use dioxus::prelude::*;
use orgtree_engine_client::{AskInfo, AskTab, BatchAnswer, MailRow, TreePayload};
use serde_json::{json, Map, Value};

/// `openAsks` (canvas/openasks.ts): la tarjeta compuesta de cada agente con
/// algo abierto; para un agente que el árbol no trae, la tarjeta se arma con
/// las filas sueltas de la cabecera (`composeBatch`). Las que el usuario
/// acaba de enviar no se listan.
pub(crate) fn open_asks(tree: &TreePayload, submitted: &std::collections::HashSet<String>) -> Vec<AskInfo> {
    let mut out = Vec::new();
    let mut batched = std::collections::HashSet::new();
    for node in tree.nodes() {
        if let Some(ask) = node.ask.as_ref().filter(|a| a.is_open()) {
            out.push(ask.clone());
            batched.insert(node.id.clone());
        }
    }
    let mut loose: Vec<(String, Vec<&AskInfo>)> = Vec::new();
    for ask in tree.asks.iter().filter(|a| a.is_open() && !batched.contains(&a.node)) {
        match loose.iter_mut().find(|(node, _)| *node == ask.node) {
            Some((_, rows)) => rows.push(ask),
            None => loose.push((ask.node.clone(), vec![ask])),
        }
    }
    for (node, rows) in loose {
        if let Some(batch) = rows.iter().find(|r| r.kind.as_deref() == Some("batch")).map(|r| (*r).clone()).or_else(|| compose_batch(&node, &rows)) {
            out.push(batch);
        }
    }
    out.retain(|a| !submitted.contains(&a.id));
    out
}

/// `composeBatch`: la primera pregunta abierta, el primer pedido de créditos
/// y el primero de alcance, como una tarjeta con sus pestañas y sus `revs`.
fn compose_batch(node: &str, rows: &[&AskInfo]) -> Option<AskInfo> {
    let kind = |a: &AskInfo| a.kind.clone().unwrap_or_default();
    let ask = rows.iter().find(|r| kind(r) != "credit" && kind(r) != "scope" && r.status == "open");
    let credit = rows.iter().find(|r| kind(r) == "credit" && r.status == "pending");
    let scope = rows.iter().find(|r| kind(r) == "scope" && r.status == "pending");
    let base = ask.or(credit).or(scope)?;
    let mut tabs = Vec::new();
    let mut revs = Map::new();
    if let Some(ask) = ask {
        revs.insert("ask".into(), json!(ask.rev.unwrap_or(1)));
        tabs.extend(question_tabs(ask));
    }
    if let Some(cr) = credit {
        revs.insert("credits".into(), json!(cr.rev.unwrap_or(1)));
        tabs.push(AskTab { kind: "credits".into(), old: cr.old, new: cr.new, reason: cr.reason.clone(), ..Default::default() });
    }
    if let Some(sr) = scope {
        revs.insert("scope".into(), json!(sr.rev.unwrap_or(1)));
        for item in sr.extra.get("items").and_then(Value::as_array).into_iter().flatten() {
            tabs.push(AskTab { kind: "scope".into(), item: Some(item.clone()), reason: sr.reason.clone(), label: Some(item.to_string()), ..Default::default() });
        }
    }
    let at = [ask, credit, scope].into_iter().flatten().map(|a| a.at.clone()).min().unwrap_or_default();
    Some(AskInfo {
        id: base.id.clone(),
        node: node.to_string(),
        kind: Some("batch".into()),
        status: "open".into(),
        at,
        question: tabs.first().and_then(|t| t.question.clone().or(t.label.clone())),
        tabs,
        revs,
        rev: ask.and(Some(base.rev.unwrap_or(1))),
        ..Default::default()
    })
}

/// Las preguntas de una fila suelta (`questions`, o la del primer nivel).
fn question_tabs(ask: &AskInfo) -> Vec<AskTab> {
    if !ask.questions.is_empty() {
        return ask.questions.iter().map(|q| AskTab { kind: "question".into(), ..q.clone() }).collect();
    }
    vec![AskTab {
        kind: "question".into(),
        question: ask.question.clone(),
        header: ask.header.clone(),
        options: ask.options.clone(),
        multi: ask.multi,
        ..Default::default()
    }]
}

/// Las pestañas de una tarjeta: las de la compuesta, o las de una pregunta suelta.
fn tabs_of(ask: &AskInfo) -> Vec<AskTab> {
    if ask.kind.as_deref() == Some("batch") && !ask.tabs.is_empty() {
        ask.tabs.clone()
    } else {
        question_tabs(ask)
    }
}

/// `askMailRow`: un pedido como fila de la bandeja.
pub(crate) fn ask_mail_row(ask: &AskInfo) -> MailRow {
    let tabs = tabs_of(ask);
    let (kind, body) = if ask.kind.as_deref() == Some("batch") {
        ("request batch", format!("{} request(s) awaiting one submit", tabs.len()))
    } else {
        ("question", ask.question.clone().unwrap_or_default())
    };
    MailRow { id: Some(format!("ask:{}", ask.id)), from: ask.node.clone(), at: ask.at.clone(), kind: Some(kind.into()), body, ..Default::default() }
}

/// `oneLine` (attention/feed.ts) y `briefLine`: una línea, recortada.
pub(crate) fn one_line(text: &str, max: usize) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() > max {
        flat.chars().take(max - 1).collect::<String>() + "…"
    } else {
        flat
    }
}

/// `MailRowView`: una fila de la bandeja (también de la cola de atención).
#[component]
pub(crate) fn MailRowView(m: MailRow, selected: bool, unread: bool, #[props(default)] ask: bool, onpick: EventHandler<()>) -> Element {
    let ctx = use_context::<Ctx>();
    let tz = (ctx.tz)();
    let urgent = !ask && m.urgent == Some(true);
    let mut class = String::from("mailrow");
    for (on, name) in [(selected, " on"), (unread, " unread"), (ask, " ask"), (urgent, " urgent")] {
        if on {
            class.push_str(name);
        }
    }
    let title = m.urgent_reason.as_ref().map(|r| format!("urgent — {r}")).unwrap_or_else(|| "urgent".into());
    rsx! {
        div { class: "{class}", "data-mail": m.id.clone().unwrap_or_default(), onclick: move |_| onpick.call(()),
            div { class: "l1",
                span { class: "mfrom", span { "{m.from}" } }
                if ask {
                    span { class: "askkind", {m.kind.clone().unwrap_or_else(|| "ask".into())} }
                }
                if urgent {
                    span { class: "urgentkind", title: "{title}", "urgent" }
                }
                span { class: "mtime", {fmt_short(&m.at, tz.as_ref())} }
            }
            div { class: "l2", {one_line(&m.body, 140)} }
        }
    }
}

/// `MailReadPane`: la cabecera, la razón de un urgente, el cuerpo (o la
/// tarjeta de un pedido) y, si se puede, la caja de respuesta.
#[component]
pub(crate) fn MailPane(m: MailRow, unread: bool, #[props(default)] ask: Option<AskInfo>, #[props(default)] reply: bool) -> Element {
    let ctx = use_context::<Ctx>();
    let tz = (ctx.tz)();
    let html = markdown(&m.body);
    let urgent = ask.is_none() && m.urgent == Some(true);
    let kind = m.kind.clone().unwrap_or_default();
    rsx! {
        div { class: "mailer-head event-head",
            span { class: "dx-sender", "{m.from}" }
            span { class: "dim", "{kind}" }
            if urgent {
                span { class: "urgentkind", "urgent" }
            }
            span { class: "dim", {fmt_local(&m.at, tz.as_ref())} }
            if unread {
                span { class: "wait", "unread" }
            }
        }
        if let Some(reason) = m.urgent_reason.clone().filter(|_| urgent) {
            div { class: "urgent-why", "{reason}" }
        }
        match ask {
            Some(ask) => rsx! { div { class: "mailer-body", AskCard { key: "{ask.id}", ask } } },
            None => rsx! { div { class: "mailer-body md", dangerous_inner_html: html } },
        }
        if reply {
            ReplyBox { key: "{m.id.clone().unwrap_or_default()}", m: m.clone() }
        }
    }
}

/// `MailReplyBox` sin adjuntos ni aviso pasivo: Enter envía, Shift+Enter es
/// un salto de línea. Un envío que falla devuelve el texto a la caja.
#[component]
fn ReplyBox(m: MailRow) -> Element {
    let ctx = use_context::<Ctx>();
    // en el scope de la vista: el envío termina aunque la fila se vaya (y la caja con ella)
    let mut draft = use_hook(|| Signal::new_in_scope(String::new(), ctx.scope));
    let mut state = use_hook(|| Signal::new_in_scope(None::<String>, ctx.scope));
    let mut busy = use_hook(|| Signal::new_in_scope(false, ctx.scope));
    let target = m.from.clone();
    let mut send = move || {
        let text = draft.peek().trim().to_string();
        if text.is_empty() || *busy.peek() {
            return;
        }
        busy.set(true);
        draft.set(String::new());
        let m = m.clone();
        ctx.spawn(async move {
            match ctx.reply_mail(&m, &text).await {
                Ok(()) => {
                    let _ = state.try_write().map(|mut s| *s = None);
                }
                Err(error) => {
                    let _ = draft.try_write().map(|mut d| *d = text);
                    let _ = state.try_write().map(|mut s| *s = Some(format!("not sent: {error} — your text is back in the box")));
                }
            }
            let _ = busy.try_write().map(|mut b| *b = false);
        });
    };
    let mut send_key = send.clone();
    rsx! {
        div { class: "mail-reply",
            textarea { rows: 2, value: "{draft}", placeholder: "reply to {target}…",
                oninput: move |e| draft.set(e.value()),
                onkeydown: move |e: KeyboardEvent| {
                    if e.key() == Key::Enter && !e.modifiers().shift() {
                        e.prevent_default();
                        send_key();
                    }
                },
            }
            button { class: "mail-reply-send", disabled: draft().trim().is_empty() || busy(), onclick: move |_| send(), "reply" }
        }
        if let Some(text) = state() {
            div { class: "mail-reply-state dim failed", role: "alert", "{text}" }
        }
    }
}

/// El borrador de una pestaña (`BatchDraft`).
#[derive(Clone, Default, PartialEq, Debug)]
struct TabDraft {
    sel: Vec<String>,
    text: String,
    skip: bool,
    /// créditos: `grant`, `deny` o `skip`; alcance: `approve`, `deny` o `skip`.
    choice: Option<&'static str>,
}

const OTHER: &str = "\u{0}other";

fn decided(tab: &AskTab, d: &TabDraft) -> bool {
    match tab.kind.as_str() {
        "question" => {
            let chosen = d.sel.iter().any(|s| s != OTHER);
            d.skip || chosen || (d.sel.iter().any(|s| s == OTHER) && !d.text.trim().is_empty()) || (tab.options.is_empty() && !d.text.trim().is_empty())
        }
        _ => d.choice.is_some(),
    }
}

/// `tabValue`: lo que manda una pestaña respondida.
fn tab_value(tab: &AskTab, d: &TabDraft) -> Value {
    if d.skip {
        return Value::Null;
    }
    let chosen: Vec<String> = d.sel.iter().filter(|s| *s != OTHER).cloned().collect();
    let extra = if d.sel.iter().any(|s| s == OTHER) || tab.options.is_empty() { d.text.trim().to_string() } else { String::new() };
    if tab.multi == Some(true) {
        let mut all = chosen;
        if !extra.is_empty() {
            all.push(extra);
        }
        return json!(all);
    }
    json!(chosen.into_iter().next().unwrap_or(extra))
}

/// El cuerpo de `resolveBatch` para estos borradores.
fn batch_answer(ask: &AskInfo, tabs: &[AskTab], drafts: &[TabDraft]) -> BatchAnswer {
    let at = |i: usize| drafts.get(i).cloned().unwrap_or_default();
    let questions: Vec<Value> = tabs.iter().enumerate().filter(|(_, t)| t.kind == "question").map(|(i, t)| tab_value(t, &at(i))).collect();
    let credits = tabs.iter().position(|t| t.kind == "credits").map(|i| match at(i).choice {
        Some("grant") => json!({ "granted": tabs[i].new.unwrap_or(0.0) }),
        Some("deny") => json!({ "deny": true }),
        _ => json!({ "skip": true }),
    });
    let scope: Vec<String> = tabs.iter().enumerate().filter(|(_, t)| t.kind == "scope").map(|(i, _)| at(i).choice.unwrap_or("skip").to_string()).collect();
    let mut revs = ask.revs.clone();
    if revs.is_empty() {
        revs.insert("ask".into(), json!(ask.rev.unwrap_or(1)));
    }
    BatchAnswer {
        revs,
        answers: (!questions.is_empty()).then_some(questions),
        credits,
        scope: (!scope.is_empty()).then_some(scope),
    }
}

/// La tarjeta de pedidos (`AskCard` → `BatchAsk`): pestañas de preguntas, de
/// créditos y de alcance, una sola respuesta. La ✕ la descarta saltando todas
/// las pestañas (el agente se entera y puede volver a preguntar).
#[component]
pub(crate) fn AskCard(ask: AskInfo) -> Element {
    let ctx = use_context::<Ctx>();
    let tabs = tabs_of(&ask);
    let count = tabs.len();
    let mut tab = use_signal(|| 0usize);
    let mut drafts = use_signal(|| vec![TabDraft::default(); count]);
    let mut busy = use_signal(|| false);
    // una tarjeta a la vista ya llegó al usuario: no se notifica (`questionVisible`)
    let visible = (ctx.slug.peek().clone(), ask.id.clone());
    use_hook({
        let visible = visible.clone();
        move || crate::notify::question_visible(&visible.0, &visible.1, true)
    });
    use_drop(move || crate::notify::question_visible(&visible.0, &visible.1, false));

    let cur = tab().min(count.saturating_sub(1));
    let Some(t) = tabs.get(cur).cloned() else { return rsx! { div { class: "dim", "this request has nothing to answer" } } };
    let d = drafts.read().get(cur).cloned().unwrap_or_default();
    let done = tabs.iter().enumerate().filter(|(i, x)| decided(x, &drafts.read().get(*i).cloned().unwrap_or_default())).count();
    let ready = done == count;
    let label = format!("{count} request{}", if count == 1 { "" } else { "s" });
    let multi = t.multi == Some(true);

    let submit = {
        let ask = ask.clone();
        let tabs = tabs.clone();
        move |all_skip: bool| {
            if *busy.peek() {
                return;
            }
            let ds: Vec<TabDraft> = if all_skip {
                tabs.iter().map(|_| TabDraft { skip: true, choice: Some("skip"), ..Default::default() }).collect()
            } else {
                drafts.peek().clone()
            };
            busy.set(true);
            ctx.resolve_batch(ask.clone(), batch_answer(&ask, &tabs, &ds), all_skip);
        }
    };
    let tabs_sig = use_signal(|| tabs.clone());
    let mut patch = move |f: &dyn Fn(&mut TabDraft), advance: bool| {
        let tabs = tabs_sig.peek();
        let mut all = drafts.write();
        if let Some(x) = all.get_mut(cur) {
            f(x);
        }
        // auto-avance (como Claude Code): una decisión mueve a la próxima pestaña sin decidir
        if advance && all.get(cur).zip(tabs.get(cur)).is_some_and(|(x, t)| decided(t, x)) {
            for s in 1..count {
                let j = (cur + s) % count;
                if let (Some(t), Some(x)) = (tabs.get(j), all.get(j)) {
                    if !decided(t, x) {
                        tab.set(j);
                        break;
                    }
                }
            }
        }
    };
    let mut submit_all = submit.clone();
    let mut submit_ready = submit.clone();
    let other_on = d.sel.iter().any(|s| s == OTHER);
    rsx! {
        div { class: "askcard", tabindex: "-1", "data-ask": "{ask.id}",
            div { class: "ask-tab",
                span { class: "ask-tab-label", "{label}" }
                span { class: "dim", "· {ask.node}" }
                span { class: "spacer" }
                button { class: "chip-x dx-ask-dismiss", title: "close, skipping every tab (the agent is told; it may re-ask)",
                    disabled: busy(), onclick: move |_| submit_all(true), "✕" }
            }
            if count > 1 {
                div { class: "ask-tabstrip",
                    for (i, x) in tabs.iter().enumerate() {
                        button { key: "{i}", r#type: "button", disabled: busy(),
                            class: if i == cur { "ask-tabbtn on" } else if decided(x, &drafts.read().get(i).cloned().unwrap_or_default()) { "ask-tabbtn done" } else { "ask-tabbtn" },
                            onclick: move |_| tab.set(i),
                            {tab_label(x, i)}
                        }
                    }
                }
            }
            match t.kind.as_str() {
                "question" => rsx! {
                    div { class: "ask-q md", dangerous_inner_html: markdown(t.question.as_deref().unwrap_or_default()) }
                    div { class: "ask-rows",
                        for o in t.options.iter().cloned() {
                            OptionRow { key: "{o.label}", label: o.label.clone(), description: o.description.clone(),
                                on: d.sel.contains(&o.label), multi, busy: busy(),
                                onpick: move |_| {
                                    let label = o.label.clone();
                                    patch(&move |x: &mut TabDraft| {
                                        x.skip = false;
                                        if x.sel.contains(&label) {
                                            x.sel.retain(|s| *s != label);
                                        } else if multi {
                                            x.sel.push(label.clone());
                                        } else {
                                            x.sel = vec![label.clone()];
                                        }
                                    }, !multi);
                                } }
                        }
                        if !t.options.is_empty() {
                            button { r#type: "button", disabled: busy(), class: if other_on { "ask-row on" } else { "ask-row" },
                                onclick: move |_| patch(&|x: &mut TabDraft| {
                                    x.skip = false;
                                    if x.sel.iter().any(|s| s == OTHER) { x.sel.retain(|s| s != OTHER) } else if multi { x.sel.push(OTHER.into()) } else { x.sel = vec![OTHER.into()] }
                                }, false),
                                span { class: if other_on { "ask-dot on" } else { "ask-dot" } }
                                span { class: "ask-row-body", b { "Other" } }
                            }
                            if other_on {
                                input { class: "ask-other", placeholder: "your answer…", value: "{d.text}", disabled: busy(),
                                    oninput: move |e| { let v = e.value(); patch(&move |x: &mut TabDraft| x.text = v.clone(), false) } }
                            }
                        } else {
                            div { class: "ask-free-response",
                                label { "Your answer" }
                                input { class: "ask-free-response-input", placeholder: "Type your answer…", value: "{d.text}", disabled: busy(),
                                    oninput: move |e| { let v = e.value(); patch(&move |x: &mut TabDraft| x.text = v.clone(), false) } }
                            }
                        }
                        if count > 1 {
                            button { r#type: "button", disabled: busy(), class: if d.skip { "ask-row skiprow on" } else { "ask-row skiprow" },
                                onclick: move |_| patch(&|x: &mut TabDraft| { x.skip = !x.skip; x.sel.clear() }, true),
                                span { class: if d.skip { "ask-dot on" } else { "ask-dot" } }
                                span { class: "ask-row-body", b { "Skip" } span { class: "dim", "leave this one unanswered" } }
                            }
                        }
                    }
                    if multi {
                        div { class: "dim", "several may apply" }
                    }
                },
                kind => {
                    let credits = kind == "credits";
                    let choices: [(&'static str, &'static str); 3] = if credits {
                        [("grant", "grant"), ("deny", "deny"), ("skip", "skip")]
                    } else {
                        [("approve", "approve"), ("deny", "deny"), ("skip", "skip")]
                    };
                    let head = if credits {
                        format!("{} → {}", t.old.unwrap_or(0.0), t.new.unwrap_or(0.0))
                    } else {
                        t.label.clone().unwrap_or_default()
                    };
                    rsx! {
                        div { class: "ask-q", b { "{head}" } }
                        if let Some(reason) = t.reason.clone() {
                            div { class: "ask-q dim", "{reason}" }
                        }
                        div { class: "ask-rows",
                            for (value, text) in choices {
                                button { key: "{value}", r#type: "button", disabled: busy(),
                                    class: if d.choice == Some(value) { "ask-row on" } else { "ask-row" },
                                    onclick: move |_| patch(&move |x: &mut TabDraft| x.choice = Some(value), true),
                                    span { class: if d.choice == Some(value) { "ask-dot on" } else { "ask-dot" } }
                                    span { class: "ask-row-body", b { "{text}" } }
                                }
                            }
                        }
                    }
                }
            }
            button { class: "ask-submit", disabled: busy() || !ready, title: "ctrl+enter",
                onclick: move |_| submit_ready(false),
                if busy() { "sending…" } else if count > 1 && !ready { "{done}/{count} answered" } else if count > 1 { "submit {count} answers" } else { "submit answer" }
            }
        }
    }
}

/// La etiqueta de una pestaña (`label` en `BatchAsk`).
fn tab_label(tab: &AskTab, i: usize) -> String {
    match tab.kind.as_str() {
        "question" => tab.header.clone().unwrap_or_else(|| format!("Q{}", i + 1)),
        "credits" => "credits".into(),
        _ => tab.item.as_ref().and_then(|it| it.get("kind")).and_then(Value::as_str).map(|k| if k == "dir" { "folder".to_string() } else { k.to_string() }).unwrap_or_else(|| "mode".into()),
    }
}

#[component]
fn OptionRow(label: String, description: Option<String>, on: bool, multi: bool, busy: bool, onpick: EventHandler<()>) -> Element {
    let dot = match (multi, on) {
        (true, true) => "ask-dot sqr on",
        (true, false) => "ask-dot sqr",
        (false, true) => "ask-dot on",
        (false, false) => "ask-dot",
    };
    rsx! {
        button { r#type: "button", disabled: busy, class: if on { "ask-row on" } else { "ask-row" }, "data-option": "{label}",
            onclick: move |_| onpick.call(()),
            span { class: "{dot}" }
            span { class: "ask-row-body",
                b { class: "ask-option-label", "{label}" }
                if let Some(text) = description {
                    span { class: "dim ask-option-description", "{text}" }
                }
            }
        }
    }
}

/// Una fila de la bandeja: un mail o un pedido.
#[derive(Clone, PartialEq)]
struct Row {
    key: String,
    mail: MailRow,
    unread: bool,
    ask: Option<AskInfo>,
}

/// La campana de la bandeja en la barra de la org (`iconbtn ask-bell` de
/// `App.tsx`): mail sin leer más pedidos abiertos; brilla con un urgente.
#[component]
pub(crate) fn InboxBell() -> Element {
    let mut ctx = use_context::<Ctx>();
    let (unread, urgent, asks) = ctx.counts();
    let count = unread + asks;
    let title = if count > 0 { format!("your inbox — {count} waiting") } else { "your inbox".to_string() };
    rsx! {
        button { class: if urgent > 0 { "iconbtn ask-bell glow dx-inbox-bell" } else { "iconbtn ask-bell dx-inbox-bell" },
            "aria-label": "{title}", title: "{title}", onclick: move |_| ctx.inbox_open.set(true),
            crate::icons::MailIcon {}
            if count > 0 {
                b { class: if urgent > 0 { "eye-count asks" } else { "eye-count" }, "{count}" }
            }
        }
    }
}

/// `InboxPanel` de `App.tsx`: las carpetas, la lista y el panel de lectura.
#[component]
pub(crate) fn InboxPanel() -> Element {
    let mut ctx = use_context::<Ctx>();
    let mut folder = use_signal(|| "inbox");
    let inbox = (ctx.inbox)();
    let read_here = (ctx.read_here)();
    let asks = match &*ctx.tree.read() {
        Some(Ok(tree)) => open_asks(tree, &ctx.submitted.read()),
        _ => Vec::new(),
    };
    let mut rows: Vec<Row> = Vec::new();
    if let Some(inbox) = &inbox {
        if folder() == "inbox" {
            // sin leer y pedidos abiertos arriba; después lo leído (el más nuevo primero)
            let mut waiting: Vec<Row> = inbox
                .pending
                .iter()
                .filter(|m| !read_here.contains(m.id.as_deref().unwrap_or_default()))
                .map(|m| Row { key: format!("mail:{}", m.id.clone().unwrap_or_default()), mail: m.clone(), unread: true, ask: None })
                .collect();
            waiting.extend(asks.iter().map(|a| Row { key: format!("ask:{}", a.id), mail: ask_mail_row(a), unread: true, ask: Some(a.clone()) }));
            waiting.sort_by(|a, b| b.mail.at.cmp(&a.mail.at).then(a.key.cmp(&b.key)));
            let mut read: Vec<Row> = inbox
                .pending
                .iter()
                .filter(|m| read_here.contains(m.id.as_deref().unwrap_or_default()))
                .chain(inbox.delivered.iter())
                .map(|m| Row { key: format!("mail:{}", m.id.clone().unwrap_or_default()), mail: m.clone(), unread: false, ask: None })
                .collect();
            read.sort_by(|a, b| b.mail.at.cmp(&a.mail.at).then(a.key.cmp(&b.key)));
            read.truncate(60);
            rows.extend(waiting);
            rows.extend(read);
        } else {
            let mut sent: Vec<Row> =
                inbox.sent.iter().map(|m| Row { key: format!("sent:{}", m.id.clone().unwrap_or_default()), mail: m.clone(), unread: false, ask: None }).collect();
            sent.sort_by(|a, b| b.mail.at.cmp(&a.mail.at));
            sent.truncate(60);
            rows = sent;
        }
    }
    let unread_count = rows.iter().filter(|r| r.unread).count();
    let selected = (ctx.inbox_sel)();
    let current = rows.iter().find(|r| Some(&r.key) == selected.as_ref()).cloned();
    // salir de un mail sin leer lo archiva (`leave` en MailList)
    let mut pick = move |key: Option<String>| {
        let previous = ctx.inbox_sel.peek().clone();
        if previous != key {
            if let Some(id) = previous.and_then(|p| p.strip_prefix("mail:").map(str::to_string)) {
                ctx.mark_read(id);
            }
        }
        ctx.inbox_sel.set(key);
    };
    let close = move |_| {
        pick(None);
        ctx.inbox_open.set(false);
    };
    let pending_mail = inbox.as_ref().map(|i| i.pending.len()).unwrap_or(0);
    rsx! {
        div { class: "overlay dx-inbox-overlay", onclick: close,
            div { class: "settings wide dx-inbox", role: "dialog", "aria-modal": "true", onclick: move |e| e.stop_propagation(),
                h3 { crate::icons::MailIcon {} " Your inbox" }
                div { class: "mail-folders",
                    for f in ["inbox", "sent"] {
                        button { key: "{f}", class: if folder() == f { "on" } else { "" },
                            onclick: move |_| { pick(None); folder.set(f) },
                            "{f}"
                            if f == "inbox" && unread_count > 0 {
                                " "
                                span { class: "tab-count", "{unread_count}" }
                            }
                        }
                    }
                }
                div { class: "mailpane",
                    if inbox.is_none() {
                        div { class: "dim", "loading…" }
                    } else if rows.is_empty() {
                        div { class: "dim pad", "no mail yet" }
                    } else {
                        div { class: "mailer",
                            div { class: "mailer-list",
                                for row in rows.iter().cloned() {
                                    MailRowView { key: "{row.key}", m: row.mail.clone(), selected: Some(&row.key) == selected.as_ref(),
                                        unread: row.unread, ask: row.ask.is_some(),
                                        onpick: move |_| {
                                            let again = ctx.inbox_sel.peek().as_ref() == Some(&row.key);
                                            pick(if again { None } else { Some(row.key.clone()) });
                                        } }
                                }
                            }
                            div { class: "mailer-read",
                                match current {
                                    Some(row) => rsx! {
                                        MailPane { key: "{row.key}", m: row.mail.clone(), unread: row.unread, ask: row.ask.clone(),
                                            reply: row.ask.is_none() && folder() == "inbox" && row.mail.from != "@user" }
                                    },
                                    None => rsx! { div { class: "dim pad mailer-none", "Select a message to read it." } },
                                }
                            }
                        }
                    }
                }
                if folder() == "inbox" && pending_mail > 0 {
                    div { class: "row",
                        button { class: "dx-mark-all", onclick: move |_| { ctx.inbox_sel.set(None); ctx.clear_inbox() }, "Mark all read" }
                    }
                }
            }
        }
    }
}
