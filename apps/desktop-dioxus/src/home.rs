//! La página de inicio en RSX: equivalente a `orgPanel` de `renderer/src/App.tsx`
//! y `OrgRows` de `renderer/src/shell/orgrows.tsx`, con las mismas clases (y el
//! mismo CSS) para que se vea igual. Crear y borrar orgs (#26) usa `NewOrg` y
//! la confirmación de `App.tsx`. Los ajustes de la app y el primer uso
//! (`canvas/onboarding.tsx`) entraron con #30. Fuera del recorte: uso por
//! proveedor y el aviso de actualizaciones.
//!
//! Una ventana por org (#25): elegir una org pasa por `orgwindows::open_org`.
//! Desde una ventana de inicio, la ventana se liga a la org; si la org ya está
//! abierta en otra ventana, esa se enfoca (la fila dice "Already open").

use crate::icons::{BellIcon, DeleteIcon, GitHubIcon, SettingsIcon};
use dioxus::prelude::*;
use futures_util::StreamExt;
use orgtree_engine_client::OrgListEntry;
use std::time::Duration;

/// Igual que `orgstatus.ts`: la lista se vuelve a pedir cada pocos segundos
/// (no hay un WebSocket de la lista de orgs), y en el acto tras crear o borrar.
const ORG_POLL: Duration = Duration::from_secs(5);

#[component]
pub fn Home() -> Element {
    let client = crate::engine_client();
    // #25: la ventana que muestra este inicio y las orgs abiertas en otras
    let win = crate::orgwindows::this_window();
    let open_orgs = crate::orgwindows::use_open_orgs();
    let mut orgs = use_signal(|| None::<Result<Vec<OrgListEntry>, String>>);
    let mut doomed = use_signal(|| None::<OrgListEntry>);
    let mut notify_settings = use_signal(|| false);
    let mut app_settings = use_signal(|| false);
    let mut prefs = use_signal(crate::notify::prefs);
    let mut error = use_signal(|| None::<String>);
    // `refreshOrgs`: un aviso por el canal adelanta la próxima lectura.
    let refresh = use_coroutine({
        let client = client.clone();
        move |mut wake: UnboundedReceiver<()>| {
            let client = client.clone();
            async move {
                loop {
                    orgs.set(Some(client.list_orgs().await.map_err(|e| e.to_string())));
                    tokio::select! {
                        _ = tokio::time::sleep(ORG_POLL) => {}
                        _ = wake.next() => {}
                    }
                }
            }
        }
    });
    let create = {
        let client = client.clone();
        move |name: String| {
            let client = client.clone();
            // `onboardingCreate`: crear la primera org desde el primer uso lo termina
            if prefs.peek().get("onboarded") != Some(&serde_json::Value::Bool(true)) {
                prefs.set(set_onboarded());
            }
            spawn(async move {
                // `createOrg(name, dirs, netAutoconnect = true)`, como `NewOrg`
                match client.create_org(&name, &[], true, &[]).await {
                    Ok(made) => {
                        refresh.send(());
                        crate::orgwindows::open_org(win, &made.slug);
                    }
                    Err(e) => error.set(Some(format!("error: {e}"))),
                }
            });
        }
    };
    let delete = move |org: OrgListEntry| {
        let client = client.clone();
        spawn(async move {
            match client.delete_org(&org.slug).await {
                Ok(_) => refresh.send(()),
                Err(e) => error.set(Some(format!("error: {e}"))),
            }
        });
    };
    let version = env!("CARGO_PKG_VERSION");
    // `showOnboarding`: una instalación sin orgs que nunca terminó el primer uso
    let onboarding = matches!(&*orgs.read(), Some(Ok(list)) if list.is_empty())
        && prefs().get("onboarded") != Some(&serde_json::Value::Bool(true));
    if onboarding {
        return rsx! {
            div { class: "welcome",
                Onboarding { prefs, oncreate: create }
            }
            if app_settings() {
                crate::settings::AppSettings { onclose: move |_| app_settings.set(false) }
            }
        };
    }
    rsx! {
        div { class: "welcome",
            div { class: "welcome-card",
                h1 {
                    "Orgtree"
                    span { class: "build-badge", title: "running app version {version}", "{version}" }
                    a { class: "gh-link h1-gh", href: "https://github.com/Maurdekye/orgtree",
                        target: "_blank", rel: "noreferrer", title: "Orgtree on GitHub",
                        GitHubIcon {}
                    }
                    crate::native::WindowControls {}
                }
                match orgs() {
                    None => rsx! { div { class: "dim org-freshness", role: "status", "cargando…" } },
                    Some(Err(error)) => rsx! { div { class: "dim org-freshness", role: "status", "{error}" } },
                    Some(Ok(list)) => rsx! {
                        nav {
                            for org in list.iter() {
                                OrgRow { key: "{org.slug}", org: org.clone(),
                                    open_elsewhere: open_orgs.read().contains(&org.slug),
                                    onpick: move |slug: String| { crate::orgwindows::open_org(win, &slug); },
                                    ondelete: move |org| doomed.set(Some(org)) }
                            }
                            if list.is_empty() {
                                div { class: "dim pad", "no organizations yet" }
                            }
                        }
                    },
                }
                NewOrg { oncreate: create }
                if let Some(message) = error() {
                    div { class: "ask-warn dx-home-error", role: "alert", onclick: move |_| error.set(None), "{message}" }
                }
                button { class: "home dx-app-settings-open", onclick: move |_| app_settings.set(true),
                    SettingsIcon {}
                    " App settings"
                }
                button { class: "home dx-notify-settings-open", onclick: move |_| notify_settings.set(true),
                    BellIcon {}
                    " Notifications"
                }
                button { class: "home", disabled: true, title: "fuera del recorte del spike",
                    SettingsIcon {}
                    " Default org settings"
                }
                crate::DataRoot {}
            }
        }
        if notify_settings() {
            NotifySettings { onclose: move |_| notify_settings.set(false) }
        }
        if app_settings() {
            crate::settings::AppSettings { onclose: move |_| app_settings.set(false) }
        }
        if let Some(org) = doomed() {
            // la confirmación de `App.tsx` (`doomedOrg`), sin la línea del kiosk (quitado en v3)
            div { class: "overlay", onclick: move |_| doomed.set(None),
                div { class: "settings content-height confirm-box dx-delete-org", role: "dialog", "aria-modal": "true",
                    onclick: move |e| e.stop_propagation(),
                    h3 { "permanently delete {org.name}?" }
                    div { class: "confirm-body",
                        "Erases the organization and its {org.nodes} node(s) — ledger, mail, lineage, audiences. Workspace and scratch folders remain on disk. This cannot be undone."
                    }
                    div { class: "row",
                        button { class: "danger solid",
                            onclick: move |_| {
                                doomed.set(None);
                                delete(org.clone());
                            },
                            "delete organization"
                        }
                        button { onclick: move |_| doomed.set(None), "cancel" }
                    }
                }
            }
        }
    }
}

/// El grupo "Notifications" de los ajustes de escritorio
/// (`canvas/desktopsettings.tsx`, con `SetGroup` y `SetToggle`): el interruptor
/// general y uno por tipo de `NOTIFICATION_OPTIONS` (#28).
#[component]
fn NotifySettings(onclose: EventHandler<()>) -> Element {
    rsx! {
        div { class: "overlay", onclick: move |_| onclose.call(()),
            div { class: "settings content-height dx-notify-settings", role: "dialog", "aria-modal": "true",
                onclick: move |e| e.stop_propagation(),
                h3 { BellIcon {} " Notifications" }
                crate::settings::NotificationsGroup {}
                div { class: "row", button { onclick: move |_| onclose.call(()), "close" } }
            }
        }
    }
}

/// Marca el primer uso como terminado (`completeOnboarding`, sin poblar los
/// charters: los documentos de charter quedan fuera del recorte).
fn set_onboarded() -> serde_json::Value {
    let mut patch = serde_json::Map::new();
    patch.insert("onboarded".into(), serde_json::Value::Bool(true));
    crate::notify::set_prefs(&patch)
}

/// Lo que dice el aviso tras pedir un enlace oficial.
fn opened_line(url: &'static str) -> String {
    match crate::external::open(url) {
        Ok(url) => format!("Opened in the browser: {url}"),
        Err(why) => why,
    }
}

/// El primer uso (`canvas/onboarding.tsx`), mínimo: qué harnesses hay en la
/// máquina y, si falta alguno, qué instalar con los enlaces oficiales fijos;
/// el tema; y la primera org. Se muestra mientras una instalación nueva no
/// tiene orgs y no terminó ni salteó el primer uso; terminar o saltear guarda
/// `onboarded` en las preferencias del escritorio.
#[component]
fn Onboarding(prefs: Signal<serde_json::Value>, oncreate: EventHandler<String>) -> Element {
    let found = use_signal(crate::harnesses::detect);
    let mut note = use_signal(|| None::<String>);
    let current = prefs();
    let explicit = current.get("visualThemeExplicit").and_then(|v| v.as_bool()) == Some(true);
    let theme = current.get("visualTheme").and_then(|v| v.as_str()).filter(|_| explicit).unwrap_or("claude").to_string();
    let none = !found().iter().any(|h| h.detected());
    let mut pick = move |id: &'static str| {
        let mut patch = serde_json::Map::new();
        patch.insert("visualTheme".into(), serde_json::Value::String(id.into()));
        patch.insert("visualThemeExplicit".into(), serde_json::Value::Bool(true));
        prefs.set(crate::notify::set_prefs(&patch));
    };
    let mut finish = move || prefs.set(set_onboarded());
    let mut finish2 = finish;
    rsx! {
        div { class: "welcome-card onboarding dx-onboarding",
            h2 { class: "onboarding-head", "Welcome to Orgtree" crate::native::WindowControls {} }
            p { class: "dim", "A minute of setup — everything here can be changed later in App settings." }
            div { class: "field-label", "Agent harnesses" }
            p { class: "dim", "Orgtree runs agents through the coding CLIs installed on this PC." }
            div { class: "dx-onboard-harnesses",
                for h in found() {
                    div { key: "{h.id}", class: "set-row dx-onboard-harness", "data-harness": "{h.id}", "data-detected": "{h.detected()}",
                        span { class: "set-label", "{h.label}" }
                        span { class: "set-control",
                            span { class: if h.detected() { "set-state on" } else { "set-state" }, if h.detected() { "installed" } else { "not installed" } }
                        }
                        if !h.detected() {
                            span { class: "set-hint",
                                "Install the {h.exe} CLI, sign in, then restart Orgtree. "
                                button { r#type: "button", class: "acct-secondary-btn dx-harness-link", "data-harness": "{h.id}",
                                    onclick: move |_| note.set(Some(opened_line(h.url))),
                                    "Official setup" }
                            }
                        }
                    }
                }
            }
            if none {
                p { class: "ask-warn dx-no-harness", role: "status",
                    "No supported harness was found. Install and sign in to Claude Code, Codex or Antigravity to run agents."
                }
            }
            if let Some(line) = note() {
                p { class: "dim dx-onboard-note", role: "status", "{line}" }
            }
            div { class: "field-label", "Visual theme" }
            div { class: "onboard-themes", role: "radiogroup", "aria-label": "Visual theme",
                for (id, label, accent, ..) in crate::settings::THEMES {
                    button { key: "{id}", r#type: "button", role: "radio", "aria-checked": "{theme == id}", "data-theme": "{id}",
                        class: if theme == id { "onboard-theme selected" } else { "onboard-theme" },
                        onclick: move |_| pick(id),
                        span { class: "onboard-swatch", style: "background: {accent}" }
                        "{label}"
                    }
                }
            }
            div { class: "field-label", "Your first organization" }
            p { class: "dim", "An organization is a team of agents around one project." }
            NewOrg { oncreate }
            div { class: "onboard-actions",
                button { r#type: "button", class: "dim dx-onboard-skip", onclick: move |_| finish(), "skip setup for now" }
                button { r#type: "button", class: "primary dx-onboard-finish", onclick: move |_| finish2(), "finish setup" }
            }
            crate::DataRoot {}
        }
    }
}

/// `NewOrg` de `App.tsx`: el botón se abre en un formulario con el nombre.
/// Las opciones avanzadas (carpetas y hubs de mail) quedan fuera del recorte:
/// la org nace con los valores por defecto, como sin tocarlas.
#[component]
fn NewOrg(oncreate: EventHandler<String>) -> Element {
    let mut open = use_signal(|| false);
    let mut name = use_signal(String::new);
    if !open() {
        return rsx! { button { class: "primary dx-new-org", onclick: move |_| open.set(true), "+ New organization" } };
    }
    rsx! {
        form { class: "stack dx-new-org-form",
            onsubmit: move |e: FormEvent| {
                e.prevent_default();
                let value = name().trim().to_string();
                if value.is_empty() {
                    return;
                }
                oncreate.call(value);
                open.set(false);
                name.set(String::new());
            },
            input { autofocus: true, placeholder: "organization name", value: "{name}", required: true,
                oninput: move |e| name.set(e.value()) }
            div { class: "row",
                button { r#type: "submit", class: "primary", "create" }
                button { r#type: "button", onclick: move |_| { open.set(false); name.set(String::new()) }, "cancel" }
            }
        }
    }
}

#[component]
fn OrgRow(org: OrgListEntry, #[props(default)] open_elsewhere: bool, onpick: EventHandler<String>, ondelete: EventHandler<OrgListEntry>) -> Element {
    let counts = match org.working {
        Some(working) => format!("{working}/{}", org.live),
        None => org.live.to_string(),
    };
    let slug = org.slug.clone();
    let enter = slug.clone();
    let doomed = org.clone();
    rsx! {
        div { class: "org", role: "button", tabindex: 0, "data-slug": "{org.slug}",
            onclick: move |_| onpick.call(slug.clone()),
            onkeydown: move |event| if event.key() == Key::Enter { onpick.call(enter.clone()) },
            span { class: "org-activity" }
            span { class: "org-name",
                span { class: "org-name-text", "{org.name}" }
                if open_elsewhere {
                    span { class: "org-open-badge dx-open-elsewhere", "Already open" }
                }
            }
            span { class: "org-counts dim", title: "active / hired agents", "{counts}" }
            button { class: "org-del", title: "delete {org.name}",
                onclick: move |e| {
                    e.stop_propagation();
                    ondelete.call(doomed.clone());
                },
                DeleteIcon {}
            }
        }
    }
}
