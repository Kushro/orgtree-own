//! Los ajustes de la org en RSX (#30): `SettingsPanel` de `App.tsx`, con las
//! pestañas Basic (créditos, valores de los agentes y el charter `org.md`),
//! Policies (el costo de contratar y asignar que sube por la cadena) y
//! Autonomy (`headless`). Las mismas clases del renderer y su CSS.
//!
//! Como el renderer, el panel guarda solo lo que se editó encima de lo que
//! dice el árbol (un solo buffer), y un botón guarda todo: `POST /settings`
//! con las mismas claves y `PUT /orgmd` si el charter cargó. `headless` se
//! guarda en el acto, como `AutonomyTab`. Lo que guarda vuelve por el
//! WebSocket de la org con el árbol nuevo.

use crate::icons::SettingsIcon;
use crate::org::Ctx;
use crate::settings::SetToggle;
use dioxus::prelude::*;
use orgtree_engine_client::{OrgSettings, TreePayload};

#[derive(Clone, Copy, PartialEq, Debug)]
enum Tab {
    Basic,
    Policies,
    Autonomy,
}

const TABS: [(Tab, &str, &str); 3] = [(Tab::Basic, "basic", "Basic"), (Tab::Policies, "policies", "Policies"), (Tab::Autonomy, "autonomy", "Autonomy")];

/// El charter: `None` mientras carga o si no se puede editar (una lectura
/// cortada nunca se guarda encima del archivo entero).
#[derive(Clone, PartialEq, Debug)]
enum Charter {
    Loading,
    Failed(String),
    Truncated,
    Ready { saved: String, text: String },
}

#[component]
pub fn OrgSettingsPanel(payload: TreePayload) -> Element {
    let mut ctx = use_context::<Ctx>();
    let server = OrgSettings::from_tree(&payload);
    // las ediciones encima de lo que dice el árbol (`edit` del renderer)
    let mut edit = use_signal(|| None::<OrgSettings>);
    let mut tab = use_signal(|| Tab::Basic);
    let mut charter = use_signal(|| Charter::Loading);
    let mut saving = use_signal(|| false);
    use_hook(move || {
        let (client, slug) = ctx.ids();
        ctx.spawn(async move {
            let next = match client.org_md(&slug).await {
                Ok(md) if md.read_truncated => Charter::Truncated,
                Ok(md) => Charter::Ready { saved: md.content.clone(), text: md.content },
                Err(e) => Charter::Failed(e.to_string()),
            };
            let _ = charter.try_write().map(|mut c| *c = next);
        });
    });
    let current = edit().unwrap_or_else(|| server.clone());
    // sobre el buffer de ahora, no el de este render: dos cambios seguidos
    // antes de que la vista se vuelva a pintar no se pisan
    let mut change = {
        let server = server.clone();
        move |f: &dyn Fn(&mut OrgSettings)| {
            let mut next = edit.peek().clone().unwrap_or_else(|| server.clone());
            f(&mut next);
            edit.set(Some(next));
        }
    };
    let mut change2 = change.clone();
    let mut change3 = change.clone();
    let mut change4 = change.clone();
    let mut change5 = change.clone();
    let mut change6 = change.clone();
    let save = {
        let current = current.clone();
        move |_| {
            let body = current.request();
            let md = match charter() {
                Charter::Ready { saved, text } if saved != text => Some(text),
                _ => None,
            };
            let (client, slug) = ctx.ids();
            saving.set(true);
            ctx.spawn(async move {
                let mut lines = Vec::new();
                match client.save_org_settings(&slug, &body).await {
                    Ok(result) => {
                        if !result.freezes_cleared.is_empty() {
                            lines.push(format!("limit raised — cleared: {}", result.freezes_cleared.join(", ")));
                        }
                        lines.extend(result.warnings);
                    }
                    Err(e) => lines.push(format!("error: {e}")),
                }
                if let Some(text) = md {
                    match client.put_org_md(&slug, &text).await {
                        Ok(result) => {
                            lines.extend(result.warnings);
                            let _ = charter.try_write().map(|mut c| *c = Charter::Ready { saved: text.clone(), text });
                        }
                        Err(e) => lines.push(format!("error: org.md: {e}")),
                    }
                }
                let failed = lines.iter().any(|l| l.starts_with("error"));
                if lines.is_empty() {
                    lines.push("settings saved".into());
                }
                ctx.toast(lines, None);
                // lo guardado vuelve con el árbol: el buffer vuelve a la verdad del motor
                if !failed {
                    let _ = edit.try_write().map(|mut e| *e = None);
                }
                let _ = saving.try_write().map(|mut s| *s = false);
            });
        }
    };
    let headless = server.headless;
    let set_headless = move |on: bool| {
        let (client, slug) = ctx.ids();
        ctx.spawn(async move {
            let line = match client.save_org_settings(&slug, &serde_json::json!({ "headless": on })).await {
                Ok(r) if !r.warnings.is_empty() => r.warnings.join("; "),
                Ok(_) if on => "headless ON — nobody is watching now".to_string(),
                Ok(_) => "headless off".to_string(),
                Err(e) => format!("error: {e}"),
            };
            ctx.toast(vec![line], None);
        });
    };
    let selected = tab();
    let dirty = edit().is_some_and(|e| e != server) || matches!(charter(), Charter::Ready { ref saved, ref text } if saved != text);
    rsx! {
        div { class: "overlay dx-org-settings-overlay", onclick: move |_| ctx.settings_open.set(false),
            div { class: "settings dx-org-settings", role: "dialog", "aria-modal": "true", "aria-label": "Org settings",
                "data-dirty": "{dirty}",
                onclick: move |e| e.stop_propagation(),
                h3 { SettingsIcon {} " {payload.name} - Org settings" }
                div { class: "app-settings-tabs", role: "tablist", "aria-label": "Organization settings sections",
                    for (id, slug, label) in TABS {
                        button { key: "{slug}", r#type: "button", role: "tab", "data-tab": "{slug}", "aria-selected": "{selected == id}",
                            class: if selected == id { "app-settings-tab on" } else { "app-settings-tab" },
                            onclick: move |_| tab.set(id), "{label}" }
                    }
                }
                div { class: "app-settings-panel", role: "tabpanel",
                    match selected {
                        Tab::Basic => rsx! {
                            div { class: "set-group",
                                div { class: "set-group-head", "Credits" }
                                div { class: "set-row", "data-setting": "max_top_grant",
                                    span { class: "set-label", "top-level grant cap" }
                                    span { class: "set-control",
                                        input { r#type: "number", min: "1", step: "1", "aria-label": "top-level grant cap", value: "{current.max_top_grant}",
                                            oninput: move |e| if let Ok(v) = e.value().trim().parse::<u64>() { change(&move |s| s.max_top_grant = v.max(1)) } }
                                    }
                                    span { class: "set-hint", "the largest grant any top-level agent may hold" }
                                }
                                div { class: "set-row", "data-setting": "default_top_grant",
                                    span { class: "set-label", "default top-level grant" }
                                    span { class: "set-control",
                                        input { r#type: "number", min: "0", step: "1", "aria-label": "default top-level grant", value: "{current.default_top_grant}",
                                            oninput: move |e| if let Ok(v) = e.value().trim().parse::<u64>() { change2(&move |s| s.default_top_grant = v) } }
                                    }
                                    span { class: "set-hint", "pre-filled on new hires" }
                                }
                            }
                            div { class: "set-group",
                                div { class: "set-group-head", "Agent defaults" }
                                div { class: "set-row", "data-setting": "compact_at",
                                    span { class: "set-label", "compaction threshold" }
                                    span { class: "set-control",
                                        input { r#type: "number", min: "50", max: "95", step: "1", "aria-label": "compaction threshold percent", value: "{current.compact_at}",
                                            oninput: move |e| if let Ok(v) = e.value().trim().parse::<u32>() { change3(&move |s| s.compact_at = v) } }
                                        span { class: "dim", "%" }
                                    }
                                    span { class: "set-hint", "50–95%. Splits the agent when its context passes this." }
                                }
                                div { class: "set-row", "data-setting": "default_effort",
                                    span { class: "set-label", "default thinking effort" }
                                    span { class: "set-control",
                                        select { "aria-label": "default thinking effort", value: "{current.default_effort}",
                                            onchange: move |e| { let v = e.value(); change4(&move |s| s.default_effort = v.clone()) },
                                            for (value, label) in [("", "CLI default (no flag)"), ("low", "low"), ("medium", "medium"), ("high", "high"), ("xhigh", "xhigh"), ("max", "max")] {
                                                option { key: "{value}", value: "{value}", selected: current.default_effort == value, "{label}" }
                                            }
                                        }
                                    }
                                    span { class: "set-hint", "agents without their own setting inherit this, live — no rehire needed. Changing it restarts every agent that inherits it." }
                                }
                            }
                            div { class: "set-group",
                                div { class: "set-group-head", "Org charter" span { class: "dim", "org.md" } }
                                div { class: "set-block",
                                    span { class: "set-hint", "carried in the managed system prompt of EVERY agent in this org, on every provider, and read as a standing directive from you. Saving restarts every agent here. Keep it short." }
                                    match charter() {
                                        Charter::Loading => rsx! { div { class: "orgmd-status", role: "status", "Loading org.md..." } },
                                        Charter::Failed(e) => rsx! { div { class: "orgmd-status", role: "alert", "Unable to load org.md ({e}). The editor is disabled until it loads successfully." } },
                                        Charter::Truncated => rsx! { div { class: "orgmd-warn", role: "alert", "This file is larger than the editor loads. Editing is DISABLED so a partial copy cannot be saved over the whole file. Edit org.md on disk instead." } },
                                        Charter::Ready { saved, text } => rsx! {
                                            textarea { class: "orgmd-editor", "aria-label": "org.md", value: "{text}",
                                                oninput: move |e| charter.set(Charter::Ready { saved: saved.clone(), text: e.value() }) }
                                            if text.is_empty() {
                                                div { class: "orgmd-status", "No org.md charter is configured." }
                                            }
                                        },
                                    }
                                }
                            }
                        },
                        Tab::Policies => rsx! {
                            div { class: "set-group",
                                div { class: "set-group-head", "Credit cost bubbling" }
                                SetToggle { key_name: "cascade_hire", label: "hires bubble their cost up the chain", checked: current.cascade_hire, disabled: false,
                                    hint: "off: the hiring agent's superior must hold the free credits itself".to_string(),
                                    onchange: move |on: bool| change5(&move |s| s.cascade_hire = on) }
                                SetToggle { key_name: "cascade_alloc", label: "allocations & model upgrades bubble their cost up the chain", checked: current.cascade_alloc, disabled: false,
                                    hint: "off: limited to the superior's own free credits".to_string(),
                                    onchange: move |on: bool| change6(&move |s| s.cascade_alloc = on) }
                            }
                        },
                        Tab::Autonomy => rsx! {
                            label { class: "checkline dx-headless",
                                title: "no user is present: questions, credit requests and user audiences auto-deny; mail to you is stored with a no-reply note",
                                input { r#type: "checkbox", checked: headless, onchange: move |e| set_headless(e.checked()) }
                                " headless — this org runs with no user present"
                            }
                            p { class: "dim", "Saved at once. Usage-limit freezes, API keys and the mail hub stay outside this cut." }
                        },
                    }
                }
                div { class: "row",
                    button { class: "primary dx-org-settings-save", disabled: saving(), onclick: save, "save" }
                    button { onclick: move |_| ctx.settings_open.set(false), "close" }
                }
            }
        }
    }
}
