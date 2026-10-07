//! La página de inicio en RSX: equivalente a `orgPanel` de `renderer/src/App.tsx`
//! y `OrgRows` de `renderer/src/shell/orgrows.tsx`, con las mismas clases (y el
//! mismo CSS) para que se vea igual. Fuera del recorte: crear y borrar orgs,
//! uso por proveedor, ajustes de la app y el aviso de actualizaciones.

use crate::icons::{DeleteIcon, GitHubIcon, SettingsIcon};
use crate::Route;
use dioxus::prelude::*;
use orgtree_engine_client::OrgListEntry;
use std::time::Duration;

/// Igual que `orgstatus.ts`: la lista se vuelve a pedir cada pocos segundos.
const ORG_POLL: Duration = Duration::from_secs(5);

#[component]
pub fn Home() -> Element {
    let client = crate::engine_client();
    let mut route = use_context::<Signal<Route>>();
    let mut orgs = use_signal(|| None::<Result<Vec<OrgListEntry>, String>>);
    use_future(move || {
        let client = client.clone();
        async move {
            loop {
                orgs.set(Some(client.list_orgs().await.map_err(|e| e.to_string())));
                tokio::time::sleep(ORG_POLL).await;
            }
        }
    });
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
                            for org in list {
                                OrgRow { key: "{org.slug}", org: org.clone(),
                                    onpick: move |slug| route.set(Route::Org(slug)) }
                            }
                            if orgs().is_some_and(|r| r.is_ok_and(|l| l.is_empty())) {
                                div { class: "dim pad", "no organizations yet" }
                            }
                        }
                    },
                }
                button { class: "primary", disabled: true, title: "fuera del recorte del spike", "+ New organization" }
                button { class: "home", disabled: true, title: "fuera del recorte del spike",
                    SettingsIcon {}
                    " Default org settings"
                }
            }
        }
    }
}

#[component]
fn OrgRow(org: OrgListEntry, onpick: EventHandler<String>) -> Element {
    let counts = match org.working {
        Some(working) => format!("{working}/{}", org.live),
        None => org.live.to_string(),
    };
    let slug = org.slug.clone();
    let enter = slug.clone();
    rsx! {
        div { class: "org", role: "button", tabindex: 0,
            onclick: move |_| onpick.call(slug.clone()),
            onkeydown: move |event| if event.key() == Key::Enter { onpick.call(enter.clone()) },
            span { class: "org-activity" }
            span { class: "org-name", span { class: "org-name-text", "{org.name}" } }
            span { class: "org-counts dim", title: "active / hired agents", "{counts}" }
            // Borrar está fuera del recorte; el botón queda para que la fila tenga la misma disposición.
            button { class: "org-del", disabled: true, title: "fuera del recorte del spike", DeleteIcon {} }
        }
    }
}
