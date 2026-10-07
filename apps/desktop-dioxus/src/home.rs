//! La página de inicio en RSX: equivalente a `orgPanel` de `renderer/src/App.tsx`
//! y `OrgRows` de `renderer/src/shell/orgrows.tsx`, con las mismas clases (y el
//! mismo CSS) para que se vea igual. Crear y borrar orgs (#26) usa `NewOrg` y
//! la confirmación de `App.tsx`. Fuera del recorte: uso por proveedor, ajustes
//! de la app y el aviso de actualizaciones.

use crate::icons::{DeleteIcon, GitHubIcon, SettingsIcon};
use crate::Route;
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
    let mut route = use_context::<Signal<Route>>();
    let mut orgs = use_signal(|| None::<Result<Vec<OrgListEntry>, String>>);
    let mut doomed = use_signal(|| None::<OrgListEntry>);
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
            spawn(async move {
                // `createOrg(name, dirs, netAutoconnect = true)`, como `NewOrg`
                match client.create_org(&name, &[], true, &[]).await {
                    Ok(made) => {
                        refresh.send(());
                        route.set(Route::Org(made.slug));
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
                                    onpick: move |slug| route.set(Route::Org(slug)),
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
                button { class: "home", disabled: true, title: "fuera del recorte del spike",
                    SettingsIcon {}
                    " Default org settings"
                }
                crate::DataRoot {}
            }
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
fn OrgRow(org: OrgListEntry, onpick: EventHandler<String>, ondelete: EventHandler<OrgListEntry>) -> Element {
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
            span { class: "org-name", span { class: "org-name-text", "{org.name}" } }
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
