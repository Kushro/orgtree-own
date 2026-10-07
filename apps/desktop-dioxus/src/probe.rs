//! Prueba de diagnóstico del spike, solo con `ORGTREE_DIOXUS_PROBE=<archivo>`.
//!
//! Con el motor de fixture (org `spike-fixture` con el agente `worker`), corre
//! en el webview real un script (`document::eval`) que verifica la página de
//! inicio en RSX: la lista de orgs carga desde el cliente Rust, abrir la org
//! muestra a sus agentes. El resultado se escribe en el archivo; el CI falla si
//! algo no se cumple. El token nunca pasa por el webview.

use dioxus::prelude::*;

const HOME_SCRIPT: &str = r#"
const timeout = ms => new Promise(done => setTimeout(done, ms));
const waitFor = async (test, ms) => {
  const end = Date.now() + ms;
  while (Date.now() < end) { const value = test(); if (value) return value; await timeout(200) }
  return null;
};
const r = {};
r.card = !!(await waitFor(() => document.querySelector('.welcome-card'), 15000));
r.version = (document.querySelector('.welcome-card .build-badge') || {}).textContent || null;
const row = await waitFor(() => [...document.querySelectorAll('.welcome-card nav .org')]
  .find(el => el.textContent.includes('spike-fixture')), 15000);
r.orgListed = !!row;
r.counts = row ? (row.querySelector('.org-counts') || {}).textContent : null;
r.fontFamily = row ? getComputedStyle(row).fontFamily : null;
r.cardBackground = getComputedStyle(document.querySelector('.welcome-card')).backgroundColor;
if (row) {
  row.click();
  r.orgOpened = !!(await waitFor(() => document.querySelector('.dx-org-view'), 10000));
  r.agentShown = !!(await waitFor(() => document.querySelector('.dx-agent[data-node="worker"]'), 15000));
}
r.bridge = typeof window.orgtreeDesktop;
return r;
"#;

#[component]
pub fn Probe() -> Element {
    use_future(|| async move {
        let Some(out) = std::env::var_os("ORGTREE_DIOXUS_PROBE").filter(|v| !v.is_empty()) else { return };
        let result = document::eval(HOME_SCRIPT).join::<serde_json::Value>().await;
        let report = match result {
            Ok(home) => serde_json::json!({ "home": home }),
            Err(error) => serde_json::json!({ "error": error.to_string() }),
        };
        let _ = std::fs::write(out, serde_json::to_vec_pretty(&report).unwrap_or_default());
    });
    rsx! {}
}
