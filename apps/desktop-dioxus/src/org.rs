//! La vista de una organización: sus agentes, desde `GET /api/orgs/{slug}`.
//! Es una versión mínima del organigrama (el lienzo queda fuera del recorte):
//! alcanza para abrir el desk de un agente (#12).

use crate::icons::HomeIcon;
use crate::Route;
use dioxus::prelude::*;
use orgtree_engine_client::{NodeState, TreePayload};

#[component]
pub fn OrgView(slug: String) -> Element {
    let client = crate::engine_client();
    let mut route = use_context::<Signal<Route>>();
    let tree = use_resource({
        let slug = slug.clone();
        move || {
            let client = client.clone();
            let slug = slug.clone();
            async move { client.tree(&slug).await.map_err(|e| e.to_string()) }
        }
    });
    let content = match &*tree.read() {
        None => rsx! { p { class: "dim", "cargando…" } },
        Some(Err(error)) => rsx! { p { class: "dim", "{error}" } },
        Some(Ok(payload)) => agents(payload, &slug, route),
    };
    rsx! {
        div { class: "dx-org-view",
            header {
                button { class: "home", onclick: move |_| route.set(Route::Home), HomeIcon {} " All organizations" }
                h2 { "{slug}" }
            }
            {content}
        }
    }
}

fn agents(payload: &TreePayload, slug: &str, mut route: Signal<Route>) -> Element {
    let nodes: Vec<_> = payload.nodes().into_iter().filter(|n| n.state == NodeState::Live).cloned().collect();
    rsx! {
        div { class: "dx-agents",
            for node in nodes {
                div { class: "dx-agent", key: "{node.id}", role: "button", tabindex: 0,
                    "data-node": "{node.id}",
                    onclick: {
                        let org = slug.to_string();
                        let id = node.id.clone();
                        move |_| route.set(Route::Desk { org: org.clone(), node: id.clone() })
                    },
                    div { class: "dx-agent-title", "{node.id}" }
                    div { class: "dx-agent-meta dim",
                        "{node.tier} · "
                        if node.busy == Some(true) { "working" } else { "idle" }
                    }
                }
            }
        }
    }
}

/// El desk del agente: llega completo en #12.
#[component]
pub fn DeskView(org: String, node: String) -> Element {
    let mut route = use_context::<Signal<Route>>();
    rsx! {
        div { class: "dx-org-view",
            header {
                button { class: "home", onclick: { let org = org.clone(); move |_| route.set(Route::Org(org.clone())) }, "← {org}" }
                h2 { "{node}" }
            }
            p { class: "dim", "El desk llega en #12." }
        }
    }
}
