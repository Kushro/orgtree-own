//! La cola de atención en RSX (#28): `attention/AttentionQueue.tsx` y
//! `attention/feed.ts`, con las clases de `attention.css` y del docket.
//!
//! Una sola lista con lo que espera al usuario en la org, al lado de un panel
//! de lectura, ordenada del más nuevo al más viejo (con la clave como
//! desempate, como `compareRows`):
//!
//! - **tickets** con la bandera de atención (`manual_attention`, de todos los
//!   grupos). La bandera queda arriba hasta que el usuario responde (la
//!   respuesta la baja sin cambiar el estado) o la descarta ("Dismiss with no
//!   comment": el ticket pasa a `blocked`). Descartar lo saca de la lista en el
//!   clic y lo devuelve, con el error, si el motor se niega;
//! - **mail urgente** sin leer: abrirlo lo marca leído, y queda a la vista
//!   mientras siga seleccionado (`retainSelected`);
//! - **preguntas** abiertas (`openAsks`), con la misma tarjeta que la bandeja.
//!
//! Cuando el elemento elegido se resuelve, la selección pasa al de abajo, o
//! al de arriba (`nextSelection`). Fuera del recorte: el desk del agente a la
//! derecha (`AgentDeskPanel`) y el resto del panel del ticket (historial,
//! adjuntos, aceptación).

use crate::inbox::{ask_mail_row, one_line, open_asks, MailPane, MailRowView};
use crate::org::Ctx;
use dioxus::prelude::*;
use orgtree_engine_client::{AskInfo, MailRow, WorkItem};

/// `AttentionRow`: una fila de la cola.
#[derive(Clone, PartialEq, Debug)]
pub(crate) struct Entry {
    pub key: String,
    pub at: String,
    pub kind: Kind,
}

#[derive(Clone, PartialEq, Debug)]
pub(crate) enum Kind {
    Ticket(WorkItem),
    Mail(MailRow),
    Question(AskInfo),
}

/// `buildAttentionRows`: lo que hoy espera al usuario, y nada más.
pub(crate) fn entries(ctx: &Ctx) -> Vec<Entry> {
    let mut out: Vec<Entry> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    if let Some(work) = &*ctx.work.read() {
        let groups = [work.attention.as_ref().unwrap_or(&work.items), work.archived.as_ref().unwrap_or(&Vec::new()), work.backlogged.as_ref().unwrap_or(&Vec::new())]
            .into_iter()
            .flatten()
            .cloned()
            .collect::<Vec<_>>();
        let dismissing = ctx.dismissing.read();
        for item in groups {
            let Some(flag) = item.manual_attention.as_ref() else { continue };
            if dismissing.contains(&item.slug) || !seen.insert(format!("ticket:{}", item.slug)) {
                continue;
            }
            let at = if flag.at.is_empty() { item.updated_at.clone().or(item.at.clone()).unwrap_or_default() } else { flag.at.clone() };
            out.push(Entry { key: format!("ticket:{}", item.slug), at, kind: Kind::Ticket(item) });
        }
    }
    if let Some(inbox) = &*ctx.inbox.read() {
        let read = ctx.read_here.read();
        for m in inbox.pending.iter().filter(|m| m.urgent == Some(true)) {
            let Some(id) = m.id.clone().filter(|id| !read.contains(id)) else { continue };
            out.push(Entry { key: format!("mail:{id}"), at: m.at.clone(), kind: Kind::Mail(m.clone()) });
        }
    }
    if let Some(Ok(tree)) = &*ctx.tree.read() {
        for ask in open_asks(tree, &ctx.submitted.read()) {
            out.push(Entry { key: format!("question:{}", ask.id), at: ask.at.clone(), kind: Kind::Question(ask) });
        }
    }
    out.sort_by(|a, b| b.at.cmp(&a.at).then(a.key.cmp(&b.key)));
    out
}

/// `retainSelected`: un mail recién leído queda mientras siga seleccionado.
fn retain_selected(live: Vec<Entry>, shown: &[Entry], selected: Option<&String>) -> Vec<Entry> {
    let Some(selected) = selected else { return live };
    if live.iter().any(|e| &e.key == selected) {
        return live;
    }
    let Some(held) = shown.iter().find(|e| &e.key == selected && matches!(e.kind, Kind::Mail(_))) else { return live };
    let mut rows = live;
    rows.push(held.clone());
    rows.sort_by(|a, b| b.at.cmp(&a.at).then(a.key.cmp(&b.key)));
    rows
}

/// La cola de atención (`AttentionQueue`), en la vista "Attention" de la org.
#[component]
pub(crate) fn AttentionQueue() -> Element {
    let mut ctx = use_context::<Ctx>();
    let live = entries(&ctx);
    // lo que se mostró en el render anterior (el `useRef` de `shown`)
    let shown = use_hook(|| std::rc::Rc::new(std::cell::RefCell::new(Vec::<Entry>::new())));
    let selected = (ctx.attn_sel)();
    let rows = retain_selected(live.clone(), &shown.borrow(), selected.as_ref());
    // la selección: si la elegida se resolvió, la de abajo o la de arriba;
    // sin selección, la primera (`nextSelection` y el efecto de selección)
    let previous = shown.borrow().clone();
    let target = match &selected {
        Some(key) if rows.iter().any(|r| &r.key == key) => Some(key.clone()),
        Some(key) => {
            let at = previous.iter().position(|r| &r.key == key);
            let keys: std::collections::HashSet<&String> = rows.iter().map(|r| &r.key).collect();
            at.and_then(|at| previous[at + 1..].iter().find(|r| keys.contains(&r.key)).or_else(|| previous[..at].iter().rev().find(|r| keys.contains(&r.key))))
                .map(|r| r.key.clone())
                .or_else(|| rows.first().map(|r| r.key.clone()))
        }
        None => rows.first().map(|r| r.key.clone()),
    };
    if target != selected {
        // fuera del render: elegir la fila (y marcar leído un mail urgente)
        let target = target.clone();
        let rows_now = rows.clone();
        spawn(async move {
            if let Some(row) = target.as_ref().and_then(|k| rows_now.iter().find(|r| &r.key == k)) {
                open_row(ctx, row);
            } else {
                ctx.attn_sel.set(None);
            }
        });
    }
    *shown.borrow_mut() = rows.clone();
    let current = rows.iter().find(|r| Some(&r.key) == selected.as_ref()).cloned();
    let detail = current.as_ref().map(kind_name).unwrap_or_default();
    let retained = |row: &Entry| matches!(row.kind, Kind::Mail(_)) && !live.iter().any(|l| l.key == row.key);
    rsx! {
        div { class: "attn-stage dx-attn",
            div { class: "attn-slot attn-slot-queue dx-attn-slot",
                div { class: "attn-wrap",
                    div { class: "mailer attn-mailer",
                        div { class: "mailer-list attn-mlist", role: "listbox", "aria-label": "Needs attention", tabindex: 0,
                            if rows.is_empty() {
                                if ctx.work.read().is_some() && ctx.inbox.read().is_some() {
                                    div { class: "dim pad attn-empty", "Pending questions, tickets needing attention, and urgent mail show here." }
                                } else {
                                    div { class: "dim pad attn-loading", role: "status", "Still reading tickets and mail — this is not yet a statement about what is waiting on you." }
                                }
                            }
                            for row in rows.iter().cloned() {
                                AttnCell { key: "{row.key}", selected: Some(&row.key) == selected.as_ref(), retained: retained(&row), row }
                            }
                        }
                        div { class: "mailer-read attn-mread", "data-attn-detail": "{detail}",
                            match current {
                                None => rsx! { div { class: "dim pad mailer-none", if rows.is_empty() { "" } else { "Select an entry to see it." } } },
                                Some(Entry { kind: Kind::Ticket(item), .. }) => rsx! { div { class: "attn-cell docket-modal", TicketPane { key: "{item.slug}", item } } },
                                Some(Entry { kind: Kind::Mail(m), key, .. }) => rsx! { MailPane { key: "{key}", unread: !retained_key(&live, &key), m, reply: true } },
                                Some(Entry { kind: Kind::Question(ask), key, .. }) => rsx! { MailPane { key: "{key}", m: ask_mail_row(&ask), unread: true, ask } },
                            }
                        }
                    }
                }
            }
        }
    }
}

fn retained_key(live: &[Entry], key: &str) -> bool {
    !live.iter().any(|l| l.key == key)
}

fn kind_name(row: &Entry) -> String {
    match row.kind {
        Kind::Ticket(_) => "ticket",
        Kind::Mail(_) => "mail",
        Kind::Question(_) => "question",
    }
    .to_string()
}

/// `openRow`: elegirla, y si es un mail urgente sin leer, marcarlo leído.
pub(crate) fn open_row(mut ctx: Ctx, row: &Entry) {
    ctx.attn_sel.set(Some(row.key.clone()));
    if let Kind::Mail(m) = &row.kind {
        if let Some(id) = m.id.clone() {
            ctx.mark_read(id);
        }
    }
}

#[component]
fn AttnCell(row: Entry, selected: bool, retained: bool) -> Element {
    let ctx = use_context::<Ctx>();
    let pick = {
        let row = row.clone();
        move |_: ()| open_row(ctx, &row)
    };
    let pick_ticket = pick.clone();
    let kind = kind_name(&row);
    match row.kind.clone() {
        Kind::Ticket(item) => {
            let owner = item.owner_node();
            let class = format!("mailrow docket-row attention status-{}{}", item.status, if selected { " on" } else { "" });
            rsx! {
                div { class: "attn-cell docket-modal", "data-attn-row": "{row.key}", "data-attn-kind": "{kind}", role: "option", "aria-selected": "{selected}",
                    div { class: "{class}", title: "{item.title}", onclick: move |_| pick_ticket(()),
                        div { class: "l1",
                            span { class: "mfrom docket-rowname", "{item.title}" }
                            span { class: "mtime", {crate::desk::fmt_short(&row.at, (ctx.tz)().as_ref())} }
                        }
                        div { class: "l2",
                            span { class: "docket-status status-{item.status} attention", "Needs attention" }
                            span { class: "docket-updater",
                                match owner {
                                    Some(owner) => rsx! { span { "{owner}" } },
                                    None => rsx! { span { class: "dim", "Unassigned" } },
                                }
                            }
                        }
                    }
                }
            }
        }
        Kind::Mail(m) => rsx! {
            div { class: "attn-cell", "data-attn-row": "{row.key}", "data-attn-kind": "{kind}", role: "option", "aria-selected": "{selected}",
                MailRowView { m, selected, unread: !retained, onpick: pick }
            }
        },
        Kind::Question(ask) => rsx! {
            div { class: "attn-cell", "data-attn-row": "{row.key}", "data-attn-kind": "{kind}", role: "option", "aria-selected": "{selected}",
                MailRowView { m: ask_mail_row(&ask), selected, unread: true, ask: true, onpick: pick }
            }
        },
    }
}

/// Lo esencial de `DocketPane` para un ticket con bandera: el título, el
/// estado, la descripción, la bandera con "Dismiss with no comment" y la
/// respuesta al asignado.
#[component]
fn TicketPane(item: WorkItem) -> Element {
    let ctx = use_context::<Ctx>();
    // en el scope de la vista: la respuesta termina aunque el ticket deje la lista
    let mut draft = use_hook(|| Signal::new_in_scope(String::new(), ctx.scope));
    let mut busy = use_hook(|| Signal::new_in_scope(false, ctx.scope));
    let owner = item.owner_node();
    let flag = item.manual_attention.clone();
    let by = flag.as_ref().and_then(|f| f.by.as_ref()).and_then(|b| b.get("node")).and_then(|n| n.as_str()).unwrap_or("@user").to_string();
    let can_dismiss = item.attention_sources.iter().any(|s| s == "manual") || flag.is_some();
    let dismiss_item = item.clone();
    let reply_item = item.clone();
    let mut send = move || {
        let text = draft.peek().trim().to_string();
        if text.is_empty() || *busy.peek() {
            return;
        }
        busy.set(true);
        draft.set(String::new());
        let item = reply_item.clone();
        ctx.spawn(async move {
            if let Err(error) = ctx.reply_ticket(&item, &text).await {
                let _ = draft.try_write().map(|mut d| *d = text);
                ctx.toast(vec![format!("error: {error}")], None);
            }
            let _ = busy.try_write().map(|mut b| *b = false);
        });
    };
    let mut send_key = send.clone();
    rsx! {
        div { class: "mailer-head docket-pane-head", "data-ticket": "{item.slug}",
            b { "{item.title}" }
            span { class: "spacer" }
        }
        div { class: "dim docket-pane-sub docket-pane-sub-attn",
            span { class: "docket-status status-{item.status} attention", "Needs attention" }
            " {item.slug}"
            if let Some(owner) = owner.clone() {
                " · Assigned to {owner}"
            }
        }
        if let Some(objective) = item.objective.clone().filter(|o| !o.is_empty()) {
            section { class: "docket-desc",
                div { class: "docket-detail-section-head", h4 { class: "docket-detail-section-title dim docket-list-heading", "DESCRIPTION" } }
                div { class: "docket-detail-section-body", div { class: "docket-desc-body", "{objective}" } }
            }
        }
        if let Some(flag) = flag {
            section { class: "docket-detail-section",
                div { class: "docket-detail-section-head", h4 { class: "docket-detail-section-title dim", "MANUAL ATTENTION" } }
                div { class: "docket-detail-section-body",
                    div { class: "docket-attention-box",
                        div { class: "docket-question-head docket-attention-head",
                            span { "Manual attention from {by}" }
                            if can_dismiss {
                                button { r#type: "button", class: "badge docket-dismiss", title: "clear this manually-raised flag",
                                    onclick: move |_| ctx.dismiss(dismiss_item.clone()),
                                    "Dismiss with no comment"
                                }
                            }
                        }
                        div { class: "docket-attention-body", "{one_line(&flag.reason, 2000)}" }
                    }
                }
            }
        }
        if owner.is_some() {
            section { class: "docket-detail-section",
                div { class: "docket-detail-section-head", h4 { class: "docket-detail-section-title dim", "REPLY" } }
                div { class: "docket-detail-section-body",
                    div { class: "mail-reply",
                        textarea { rows: 2, value: "{draft}", placeholder: "reply to {owner.clone().unwrap_or_default()}…",
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
                }
            }
        }
    }
}
