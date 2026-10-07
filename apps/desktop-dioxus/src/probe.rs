//! Prueba de diagnóstico del spike, solo con `ORGTREE_DIOXUS_PROBE=<archivo>`.
//!
//! Con el motor de fixture (org `spike-fixture` con el agente `worker`, una
//! conversación larga y frames en vivo), corre en el webview real un script
//! (`document::eval`) que verifica:
//!
//! - #11, inicio en RSX: la lista de orgs carga desde el cliente Rust y abrir la
//!   org muestra a sus agentes;
//! - #12, desk en RSX: la conversación, la herramienta, el Markdown sanitizado,
//!   el texto en vivo, la carga de páginas anteriores hasta el primer mensaje y
//!   los cuadros de un scroll de punta a punta.
//!
//! El script avisa por `dioxus.send` cuando el desk está listo para una captura
//! (Rust deja `<archivo>.desk` para el CI) y al final manda el resultado, que se
//! escribe en el archivo. El token nunca pasa por el webview.

use dioxus::prelude::*;

const SCRIPT: &str = r#"
const timeout = ms => new Promise(done => setTimeout(done, ms));
const waitFor = async (test, ms) => {
  const end = Date.now() + ms;
  while (Date.now() < end) { const value = test(); if (value) return value; await timeout(200) }
  return null;
};
const r = {};

// #11: inicio
const home = r.home = {};
home.card = !!(await waitFor(() => document.querySelector('.welcome-card'), 15000));
home.version = (document.querySelector('.welcome-card .build-badge') || {}).textContent || null;
const row = await waitFor(() => [...document.querySelectorAll('.welcome-card nav .org')]
  .find(el => el.textContent.includes('spike-fixture')), 15000);
home.orgListed = !!row;
home.counts = row ? (row.querySelector('.org-counts') || {}).textContent : null;
home.fontFamily = row ? getComputedStyle(row).fontFamily : null;
home.bridge = typeof window.orgtreeDesktop;
if (row) {
  // pausa para la captura del CI: el inicio con la lista cargada
  dioxus.send({ pause: 'home' });
  await timeout(3000);
  row.click();
  home.orgOpened = !!(await waitFor(() => document.querySelector('.dx-org-view'), 10000));
  home.agentShown = !!(await waitFor(() => document.querySelector('.dx-agent[data-node="worker"]'), 15000));
}

// #12: desk
const desk = r.desk = {};
const card = document.querySelector('.dx-agent[data-node="worker"]');
if (card) {
  card.click();
  const msgs = await waitFor(() => document.querySelector('.dx-desk .msgs'), 15000);
  desk.opened = !!msgs;
  if (msgs) {
    await waitFor(() => msgs.querySelectorAll('.msg').length, 15000);
    const beat = () => (msgs.textContent.match(/latido (\d+)/g) || []).map(m => Number(m.split(' ')[1]));
    const first = await waitFor(() => { const b = beat(); return b.length && Math.max(...b) }, 20000);
    const later = await waitFor(() => { const b = beat(); return b.length && Math.max(...b) > (first || 0) && Math.max(...b) }, 10000);
    desk.live = { first, later };
    const numbers = () => (msgs.textContent.match(/(?:Mensaje|Respuesta) (\d+)/g) || []).map(m => Number(m.split(' ')[1]));
    const oldest = () => { const n = numbers(); return n.length ? Math.min(...n) : null };
    desk.earlierPages = 0;
    const pagingStart = performance.now();
    for (let page = 0; page < 40; page++) {
      // hasta que no queden páginas anteriores (el aviso "earlier messages" desaparece)
      if (!document.querySelector('.dx-desk .dx-earlier')) break;
      const before = msgs.querySelectorAll('.msg').length;
      msgs.scrollTop = 0;
      msgs.dispatchEvent(new Event('scroll'));
      if (!(await waitFor(() => msgs.querySelectorAll('.msg').length > before, 8000))) break;
      desk.earlierPages++;
    }
    desk.pagingMs = Math.round(performance.now() - pagingStart);
    msgs.scrollTop = 0;
    await timeout(300);
    desk.domRows = msgs.querySelectorAll('.msg').length;
    desk.oldestLoaded = oldest();
    desk.toolShown = !!msgs.querySelector('.tools.tchip') && msgs.textContent.includes('README');
    // Markdown sanitizado: **negrita** y lista se renderizan, el onerror inyectado no
    desk.markdownBold = !!msgs.querySelector('.msgtext strong');
    desk.markdownList = !!msgs.querySelector('.msgtext ul li');
    desk.injectionBlocked = !msgs.querySelector('[onerror]') && document.title !== 'inyectado';
    // fluidez: recorrer la conversación entera de arriba abajo y medir los cuadros
    const frames = [];
    const duration = 4000, span = Math.max(1, msgs.scrollHeight - msgs.clientHeight);
    await new Promise(done => {
      const start = performance.now(); let last = start;
      const step = now => {
        frames.push(now - last); last = now;
        const t = Math.min(1, (now - start) / duration);
        msgs.scrollTop = span * t;
        if (t < 1) requestAnimationFrame(step); else done();
      };
      requestAnimationFrame(step);
    });
    frames.shift();
    const sorted = [...frames].sort((a, b) => a - b);
    desk.scroll = {
      height: msgs.scrollHeight, frames: frames.length,
      avgMs: Math.round(frames.reduce((a, b) => a + b, 0) / frames.length * 10) / 10,
      p95Ms: Math.round(sorted[Math.floor(sorted.length * 0.95)] * 10) / 10,
      maxMs: Math.round(sorted[sorted.length - 1] * 10) / 10,
      over50ms: frames.filter(f => f > 50).length,
    };
    const n = numbers();
    desk.newestLoaded = n.length ? Math.max(...n) : null;
    // pausa para la captura del CI: el desk con la conversación larga a la vista
    msgs.scrollTop = msgs.scrollHeight / 2;
    dioxus.send({ pause: 'desk' });
    await timeout(3000);
  }
}
dioxus.send({ done: r });
"#;

#[component]
pub fn Probe() -> Element {
    use_future(|| async move {
        let Some(out) = std::env::var_os("ORGTREE_DIOXUS_PROBE").filter(|v| !v.is_empty()) else { return };
        let out = std::path::PathBuf::from(out);
        let mut eval = document::eval(SCRIPT);
        let report = loop {
            match eval.recv::<serde_json::Value>().await {
                Ok(message) => {
                    if let Some(name) = message.get("pause").and_then(|v| v.as_str()) {
                        let mut marker = out.clone().into_os_string();
                        marker.push(format!(".{name}"));
                        let _ = std::fs::write(marker, b"");
                    } else if let Some(done) = message.get("done") {
                        break done.clone();
                    }
                }
                Err(error) => break serde_json::json!({ "error": error.to_string() }),
            }
        };
        let _ = std::fs::write(&out, serde_json::to_vec_pretty(&report).unwrap_or_default());
    });
    rsx! {}
}
