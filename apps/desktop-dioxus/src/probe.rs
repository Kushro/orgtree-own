//! Prueba de diagnóstico del spike, solo con `ORGTREE_DIOXUS_PROBE=<archivo>`.
//!
//! Con el motor de fixture (org `spike-fixture` con el agente `worker`, una
//! conversación larga y frames en vivo), corre en el webview real un script
//! (`document::eval`) que verifica:
//!
//! - #11, inicio en RSX: la lista de orgs carga desde el cliente Rust y abrir la
//!   org muestra a sus agentes;
//! - #26, organigrama: crear una org, contratar desde el menú (y una
//!   contratación por fuera de la UI que llega por el WebSocket), detener y
//!   reanudar, mover y deshacer, retirar, recontratar y borrar la org;
//! - #12, desk en RSX: la conversación, la herramienta, el Markdown sanitizado,
//!   el texto en vivo, la carga de páginas anteriores hasta el primer mensaje y
//!   los cuadros de un scroll de punta a punta;
//! - #13, varias ventanas: el desk en otra ventana nativa, el borrador
//!   compartido en los dos sentidos y el cierre de la principal con el desk
//!   abierto (la principal se oculta, el desk sigue);
//! - #14, integración nativa: botones de la ventana sin marco, notificación,
//!   bandeja, el arrastre con el mouse real (lo hace el CI en una pausa) y la
//!   instancia única: con todas las ventanas cerradas la app sigue en la
//!   bandeja, una segunda ejecución vuelve a mostrar la principal y Salir
//!   termina la app y el motor.
//!
//! El script avisa por `dioxus.send` cuando el desk está listo para una captura
//! (Rust deja `<archivo>.desk` para el CI) y al final manda el resultado, que se
//! escribe en el archivo. El token nunca pasa por el webview.

use dioxus::prelude::*;
use std::sync::Mutex;

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
// #24: la raíz de datos en uso, a la vista en el inicio
home.dataRoot = (document.querySelector('.welcome-card .dx-data-root code') || {}).textContent || null;
if (row) {
  // pausa para la captura del CI: el inicio con la lista cargada
  dioxus.send({ pause: 'home' });
  await timeout(3000);
}

// #26: organigrama. Crear una org, contratar, detener y reanudar, mover,
// retirar y recontratar desde el menú de agente, y borrar la org.
const chart = r.chart = {};
const setValue = (el, value, type = 'input') => { el.value = value; el.dispatchEvent(new Event(type, { bubbles: true })) };
const view = () => document.querySelector('.dx-org-view');
const agentCard = id => document.querySelector(`.dx-org-view .dx-agent[data-node="${id}"]`);
const parentOf = id => {
  const card = agentCard(id);
  const up = card && card.closest('.node').parentElement.closest('.node');
  return up ? up.querySelector(':scope > .card').getAttribute('data-node') : null;
};
const closeMenu = async () => {
  document.querySelector('.dx-menu-scrim')?.click();
  await waitFor(() => !document.querySelector('.ctxmenu'), 5000);
};
// clic derecho sobre la tarjeta, con coordenadas reales: el menú se abre ahí
const menuFor = async id => {
  await closeMenu();
  const card = id === '@user' ? document.querySelector('.dx-user-card') : agentCard(id);
  if (!card) return null;
  const box = card.getBoundingClientRect();
  card.dispatchEvent(new MouseEvent('contextmenu', { bubbles: true, cancelable: true,
    clientX: Math.round(box.left + box.width * 0.4), clientY: Math.round(box.top + box.height / 2) }));
  return await waitFor(() => document.querySelector('.ctxmenu'), 5000);
};
const pick = async (id, label) => {
  const menu = await menuFor(id);
  const entry = menu && [...menu.querySelectorAll('.ctxmenu-item')].find(b => b.textContent.trim() === label);
  if (!entry) { chart.missing = (chart.missing || []).concat(`${id}: ${label}`); await closeMenu(); return false }
  entry.click();
  return true;
};
const hire = async (id, name, grant, pause) => {
  if (!(await pick(id, id === '@user' ? 'Hire a top-level agent…' : 'Hire a subordinate…'))) return false;
  const form = await waitFor(() => document.querySelector('.dx-hire'), 5000);
  if (!form) return false;
  setValue(form.querySelector('#dx-hire-name'), name);
  setValue(form.querySelector('#dx-hire-grant'), String(grant));
  await timeout(300);
  if (pause) { dioxus.send({ pause }); await timeout(3000) }
  form.querySelector('button[type="submit"]').click();
  return !!(await waitFor(() => agentCard(name), 15000));
};
const loads = () => Number(view() ? view().dataset.loads : NaN);
const newOrg = document.querySelector('.welcome-card .dx-new-org');
if (newOrg) {
  newOrg.click();
  const input = await waitFor(() => document.querySelector('.dx-new-org-form input'), 5000);
  if (input) {
    setValue(input, 'Prueba Dioxus');
    await timeout(200);
    document.querySelector('.dx-new-org-form button[type="submit"]').click();
  }
  chart.created = !!(await waitFor(() => view() && document.querySelector('.dx-user-card'), 20000));
  chart.orgName = (document.querySelector('.dx-org-view h2') || {}).textContent || null;
  chart.emptyTree = !!document.querySelector('.dx-empty');
  // sin sondeo: con la org quieta, el árbol no se vuelve a pedir
  await waitFor(() => view() && view().dataset.connected === 'true', 10000);
  // (una relectura en esos segundos solo puede venir de un frame del motor)
  const frames = () => Number(view() ? view().dataset.frames : NaN);
  const idleFrom = { loads: loads(), frames: frames() };
  await timeout(8000);
  chart.idle = { seconds: 8, loads: loads() - idleFrom.loads, frames: frames() - idleFrom.frames };
  chart.hiredTop = await hire('@user', 'jefe', 5, 'chart-hire');
  chart.hiredSub = await hire('jefe', 'ayudante', 0);
  chart.nested = parentOf('ayudante') === 'jefe';
  // un cambio hecho fuera de la UI (el cliente Rust directo) llega por el WebSocket
  dioxus.send({ externalHire: 'externo' });
  chart.external = await dioxus.recv();
  chart.externalShown = !!(await waitFor(() => agentCard('externo'), 15000));
  chart.externalTop = parentOf('externo') === '@user';
  chart.bar = (document.querySelector('.dx-org-view .chip.agents') || {}).textContent || null;
  chart.credits = (document.querySelector('.dx-org-view .dx-credits') || {}).textContent || null;
  chart.model = (agentCard('jefe')?.querySelector('.badge.prov-claude') || {}).textContent || null;
  // el menú de agente, con captura
  const menu = await menuFor('ayudante');
  chart.menu = menu ? [...menu.querySelectorAll('.ctxmenu-item')].map(b => b.textContent.trim() + (b.disabled ? ' (disabled)' : '')) : null;
  if (menu) {
    dioxus.send({ pause: 'chart-menu' });
    await timeout(3000);
    await closeMenu();
  }
  const status = id => agentCard(id) && agentCard(id).dataset.status;
  // detener y reanudar (HaltControl)
  if (await pick('ayudante', 'Halt')) {
    chart.halted = !!(await waitFor(() => status('ayudante') === 'Halted' && agentCard('ayudante').querySelector('.badge.halted'), 15000));
    chart.haltToast = [...document.querySelectorAll('.toast')].map(t => t.textContent).find(t => t.includes('halted')) || null;
    dioxus.send({ pause: 'chart' });
    await timeout(3000);
  }
  if (await pick('ayudante', 'Unhalt')) {
    chart.unhalted = !!(await waitFor(() => status('ayudante') === 'Idle', 15000));
  }
  // interrumpir: solo con un turno en curso, como el STOP del desk
  const m2 = await menuFor('ayudante');
  const interrupt = m2 && [...m2.querySelectorAll('.ctxmenu-item')].find(b => b.textContent.trim() === 'Interrupt');
  chart.interruptDisabledWhenIdle = !!(interrupt && interrupt.disabled);
  await closeMenu();
  // mover al primer nivel, y deshacer desde el aviso
  if (await pick('ayudante', 'Move to…')) {
    const form = await waitFor(() => document.querySelector('.dx-move'), 5000);
    if (form) {
      setValue(form.querySelector('#dx-move-to'), '', 'change');
      await timeout(300);
      form.querySelector('button[type="submit"]').click();
      chart.moved = !!(await waitFor(() => parentOf('ayudante') === '@user', 15000));
      const undo = await waitFor(() => [...document.querySelectorAll('.toast')].find(t => t.textContent.includes('now reports to'))?.querySelector('.toast-undo'), 5000);
      if (undo) undo.click();
      chart.movedBack = !!(await waitFor(() => parentOf('ayudante') === 'jefe', 15000));
    }
  }
  // retirar (con la confirmación de AgentRetireConfirm), plegado, y recontratar
  if (await pick('ayudante', 'Retire…')) {
    const confirm = await waitFor(() => document.querySelector('.dx-confirm'), 5000);
    chart.retireConfirm = confirm ? confirm.querySelector('.confirm-body').textContent : null;
    if (confirm) confirm.querySelector('.danger.solid').click();
    chart.retiredHidden = !!(await waitFor(() => !agentCard('ayudante') && document.querySelector('.dx-tree .tray-arch'), 15000));
    const fold = document.querySelector('.dx-tree .tray-arch');
    if (fold) fold.click();
    chart.retiredShown = !!(await waitFor(() => agentCard('ayudante') && agentCard('ayudante').closest('.node.archived'), 5000));
    if (await pick('ayudante', 'Rehire')) {
      chart.rehired = !!(await waitFor(() => status('ayudante') === 'Idle', 15000));
    }
  }
  chart.loads = loads();
  chart.frames = frames();
  chart.toasts = [...document.querySelectorAll('.toast')].map(t => t.textContent);
  // volver al inicio y borrar la org (con la confirmación de App.tsx)
  document.querySelector('.dx-org-view button.home').click();
  const mine = await waitFor(() => document.querySelector('.welcome-card nav .org[data-slug="prueba-dioxus"]'), 15000);
  chart.listed = !!mine;
  if (mine) {
    mine.querySelector('.org-del').click();
    const confirm = await waitFor(() => document.querySelector('.dx-delete-org'), 5000);
    chart.deleteConfirm = confirm ? confirm.querySelector('h3').textContent : null;
    if (confirm) confirm.querySelector('.danger.solid').click();
    chart.deleted = !!(await waitFor(() => !document.querySelector('.welcome-card nav .org[data-slug="prueba-dioxus"]'), 15000));
  }
}

const fixtureRow = await waitFor(() => [...document.querySelectorAll('.welcome-card nav .org')]
  .find(el => el.textContent.includes('spike-fixture')), 15000);
if (fixtureRow) {
  fixtureRow.click();
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

// #14: ventana sin marco. Los botones propios maximizan y restauran; el CI
// arrastra la ventana desde el header con el mouse real durante la pausa.
const native = r.native = {};
const controls = () => document.querySelector('.dx-desk header .window-controls');
native.controls = controls() ? controls().querySelectorAll('.window-control').length : 0;
const maximize = controls() && controls().querySelector('[aria-label="Maximize window"]');
if (maximize) {
  maximize.click();
  native.maximized = !!(await waitFor(() => controls().querySelector('[aria-label="Restore window"]'), 5000));
  const restore = controls().querySelector('[aria-label="Restore window"]');
  if (restore) restore.click();
  native.restored = !!(await waitFor(() => controls().querySelector('[aria-label="Maximize window"]'), 5000));
  await timeout(500);
}
const region = el => { const s = getComputedStyle(el); return s.getPropertyValue('app-region') || s.getPropertyValue('-webkit-app-region') || s.webkitAppRegion || '' };
const title = document.querySelector('.dx-desk header h2');
if (title) {
  const b = title.getBoundingClientRect();
  const x = b.right + 24, y = b.top + b.height / 2;
  const at = document.elementFromPoint(x, y);
  native.drag = { x: Math.round(x), y: Math.round(y), dpr: devicePixelRatio, region: at ? region(at) : null };
}
dioxus.send({ native });
await dioxus.recv();

// #13: el desk en otra ventana con el borrador compartido. La otra mitad de
// esta etapa corre en la ventana del desk (POPOUT_SCRIPT).
const multi = r.multiwindow = {};
const ta = document.querySelector('.dx-desk .cc-composer textarea');
multi.composer = !!ta;
const button = document.querySelector('.dx-desk .dx-popout');
if (ta && button) {
  ta.value = 'escrito en la principal';
  ta.dispatchEvent(new Event('input', { bubbles: true }));
  await timeout(300);
  button.click();
  multi.mirroredFromPopout = !!(await waitFor(() => ta.value === 'escrito en el popout', 30000));
  multi.mainDraft = ta.value;
  dioxus.send({ pause: 'popout' });
  await timeout(3000);
}
dioxus.send({ done: r });
"#;

/// La mitad de la etapa #13 que corre en la ventana del desk.
const POPOUT_SCRIPT: &str = r#"
const timeout = ms => new Promise(done => setTimeout(done, ms));
const waitFor = async (test, ms) => {
  const end = Date.now() + ms;
  while (Date.now() < end) { const value = test(); if (value) return value; await timeout(200) }
  return null;
};
const r = {};
const ta = await waitFor(() => document.querySelector('.dx-desk .cc-composer textarea'), 15000);
r.opened = !!ta;
r.messages = (await waitFor(() => document.querySelectorAll('.dx-desk .msg').length, 15000)) || 0;
r.fontFamily = getComputedStyle(document.body).fontFamily;
r.bridge = typeof window.orgtreeDesktop;
r.mirroredFromMain = !!(await waitFor(() => ta && ta.value === 'escrito en la principal', 15000));
if (ta) {
  ta.value = 'escrito en el popout';
  ta.dispatchEvent(new Event('input', { bubbles: true }));
}
dioxus.send({ ready: r });
// Rust avisa cuando ya cerró (ocultó) la ventana principal.
await dioxus.recv();
await timeout(500);
dioxus.send({ afterOwnerClose: { alive: true, draft: ta && ta.value, messages: document.querySelectorAll('.dx-desk .msg').length } });
"#;

/// #24, la app instalada (`ORGTREE_DIOXUS_PROBE_MODE=installed`): el motor
/// empaquetado sobre una raíz nueva, sin orgs. La UI RSX carga con el CSS del
/// renderer, la lista de orgs llega (vacía) y la raíz de datos está a la vista.
const INSTALLED_SCRIPT: &str = r#"
const timeout = ms => new Promise(done => setTimeout(done, ms));
const waitFor = async (test, ms) => {
  const end = Date.now() + ms;
  while (Date.now() < end) { const value = test(); if (value) return value; await timeout(200) }
  return null;
};
const r = {};
const card = await waitFor(() => document.querySelector('.welcome-card'), 30000);
r.card = !!card;
r.version = (document.querySelector('.welcome-card .build-badge') || {}).textContent || null;
r.fontFamily = card ? getComputedStyle(card).fontFamily : null;
r.bridge = typeof window.orgtreeDesktop;
// la lista viene del motor por el cliente Rust: en una raíz nueva no hay orgs
r.orgList = !!(await waitFor(() => document.querySelector('.welcome-card nav'), 30000));
r.orgs = document.querySelectorAll('.welcome-card nav .org').length;
r.empty = !!(await waitFor(() => [...document.querySelectorAll('.welcome-card .dim')].find(el => el.textContent.includes('no organizations')), 5000));
r.listError = (document.querySelector('.welcome-card .org-freshness') || {}).textContent || null;
r.dataRoot = (document.querySelector('.welcome-card .dx-data-root code') || {}).textContent || null;
r.mode = (document.querySelector('.welcome-card .dx-launch-mode') || {}).textContent || null;
// pausa para la captura del CI: la app instalada con la raíz a la vista
dioxus.send({ pause: 'installed' });
await timeout(3000);
dioxus.send({ done: r });
"#;

fn installed_mode() -> bool {
    std::env::var("ORGTREE_DIOXUS_PROBE_MODE").is_ok_and(|mode| mode == "installed")
}

async fn installed_probe() {
    let mut eval = document::eval(INSTALLED_SCRIPT);
    let page = loop {
        match eval.recv::<serde_json::Value>().await {
            Ok(message) => {
                if let Some(name) = message.get("pause").and_then(|v| v.as_str()) {
                    marker(name);
                } else if let Some(done) = message.get("done") {
                    break done.clone();
                }
            }
            Err(error) => break serde_json::json!({ "error": error.to_string() }),
        }
    };
    record("installed", page);
    if let Ok(launch) = crate::launch() {
        let engine = crate::ENGINE.lock().unwrap().as_ref().map(|e| e.data_root().display().to_string());
        record("launch", serde_json::json!({
            "packaged": launch.packaged,
            "data_root": launch.options.data_root,
            "engine_data_root": engine,
            "python": launch.options.python,
            "bootstrap_postgres": launch.options.bootstrap_postgres,
            "descriptor": launch.descriptor,
        }));
    }
    // Salir, como desde la bandeja: cierra todo y apaga el motor.
    crate::native::quit();
}

/// El último estado de arranque (por ejemplo, por qué el motor no arrancó),
/// para que el CI lo vea aunque la UI no llegue a montarse.
pub fn startup_status(text: &str) {
    record("startup_status", serde_json::json!(text));
    // En la prueba de la app instalada, un arranque fallido termina la app: el CI
    // no espera en vano y diagnostica.
    if installed_mode() && report_path().is_some() && text.starts_with("El motor no arrancó") {
        crate::native::quit();
    }
}

/// Reporte compartido por las dos ventanas.
static REPORT: Mutex<Option<serde_json::Map<String, serde_json::Value>>> = Mutex::new(None);
/// La principal ya se cerró (se ocultó): la ventana del desk sigue con su parte.
static OWNER_CLOSED: tokio::sync::Notify = tokio::sync::Notify::const_new();
/// Llegó el aviso de una segunda ejecución.
static SECOND_INSTANCE: tokio::sync::Notify = tokio::sync::Notify::const_new();

/// Una segunda ejecución avisó a esta instancia (#14), que ya mostró su ventana.
pub fn second_instance(message: &str) {
    if report_path().is_none() {
        return;
    }
    let mut visible = None;
    crate::windows::with_main(|main| visible = Some(main.window.is_visible()));
    record("second_instance", serde_json::json!({ "message": message, "main_visible": visible }));
    SECOND_INSTANCE.notify_one();
}

/// Lo que el CI necesita para verificar la ventana nativa: el HWND (para
/// arrastrarla con el mouse real), si tiene marco y si hay bandeja.
fn native_shell() -> serde_json::Value {
    let window = dioxus::desktop::window();
    #[cfg(windows)]
    let hwnd = {
        use dioxus::desktop::tao::platform::windows::WindowExtWindows;
        Some(window.window.hwnd() as isize)
    };
    #[cfg(not(windows))]
    let hwnd: Option<isize> = None;
    serde_json::json!({
        "hwnd": hwnd,
        "decorated": window.window.is_decorated(),
        "scale": window.window.scale_factor(),
        "tray": crate::native::tray_ready(),
        "notify": crate::native::notify("Orgtree", "Notificación nativa del spike de Dioxus").map(|_| true).unwrap_or_else(|e| { let _ = e; false }),
    })
}

/// Espera a que el CI termine lo que hace durante una pausa (`<salida>.<nombre>-done`).
async fn wait_for_ci(name: &str) -> bool {
    let Some(out) = report_path() else { return false };
    let mut done = out.into_os_string();
    done.push(format!(".{name}-done"));
    for _ in 0..200 {
        if std::path::Path::new(&done).exists() {
            return true;
        }
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    }
    false
}

fn report_path() -> Option<std::path::PathBuf> {
    std::env::var_os("ORGTREE_DIOXUS_PROBE").filter(|v| !v.is_empty()).map(std::path::PathBuf::from)
}

fn record(key: &str, value: serde_json::Value) {
    let Some(out) = report_path() else { return };
    let mut report = REPORT.lock().unwrap();
    let report = report.get_or_insert_with(Default::default);
    report.insert(key.to_string(), value);
    let _ = std::fs::write(out, serde_json::to_vec_pretty(report).unwrap_or_default());
}

fn marker(name: &str) {
    let Some(out) = report_path() else { return };
    let mut marker = out.into_os_string();
    marker.push(format!(".{name}"));
    let _ = std::fs::write(marker, b"");
}

#[component]
pub fn Probe() -> Element {
    let client = crate::engine_client();
    use_future(move || {
        let client = client.clone();
        async move {
        if report_path().is_none() {
            return;
        }
        if installed_mode() {
            return installed_probe().await;
        }
        let mut eval = document::eval(SCRIPT);
        let report = loop {
            match eval.recv::<serde_json::Value>().await {
                Ok(message) => {
                    if let Some(name) = message.get("pause").and_then(|v| v.as_str()) {
                        marker(name);
                    } else if let Some(page) = message.get("native") {
                        // #14: datos para el CI, pausa mientras arrastra la ventana, y seguir.
                        record("native", serde_json::json!({ "page": page, "shell": native_shell() }));
                        marker("native");
                        let finished = wait_for_ci("native").await;
                        record("native_ci_done", serde_json::json!(finished));
                        let _ = eval.send(serde_json::json!(true));
                    } else if let Some(name) = message.get("externalHire").and_then(|v| v.as_str()) {
                        // #26: una contratación por fuera de la UI, con el cliente Rust directo;
                        // la UI tiene que verla llegar por el WebSocket de la org.
                        let request = orgtree_engine_client::OpRequest::hire(None, "haiku", name, 0, None);
                        let result = match client.op("prueba-dioxus", &request).await {
                            Ok(r) => serde_json::json!({ "node": r.node, "warnings": r.warnings }),
                            Err(e) => serde_json::json!({ "error": e.to_string() }),
                        };
                        let _ = eval.send(result);
                    } else if let Some(done) = message.get("done") {
                        break done.clone();
                    }
                }
                Err(error) => break serde_json::json!({ "error": error.to_string() }),
            }
        };
        // `native` ya quedó guardado con la parte del shell durante la pausa.
        for key in ["home", "chart", "desk", "multiwindow", "error"] {
            if let Some(value) = report.get(key) {
                record(key, value.clone());
            }
        }
        // #13: cerrar la principal con el desk abierto en otra ventana.
        if crate::windows::popout_count() == 0 {
            return;
        }
        let main = dioxus::desktop::window();
        main.close();
        tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
        record("owner_close", serde_json::json!({
            "main_visible": main.window.is_visible(),
            "popouts": crate::windows::popout_count(),
        }));
        OWNER_CLOSED.notify_one();
        // #14: el desk se cierra solo; sin ventanas visibles, la app sigue en la bandeja.
        for _ in 0..50 {
            if crate::windows::popout_count() == 0 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        }
        tokio::time::sleep(std::time::Duration::from_millis(1000)).await;
        record("tray_only", serde_json::json!({
            "main_visible": main.window.is_visible(),
            "popouts": crate::windows::popout_count(),
        }));
        marker("tray");
        // El CI lanza una segunda ejecución: tiene que volver a mostrar la principal.
        let woke = tokio::time::timeout(std::time::Duration::from_secs(60), SECOND_INSTANCE.notified()).await.is_ok();
        tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
        record("quit", serde_json::json!({ "after_second_instance": woke, "main_visible": main.window.is_visible() }));
        // Salir, como desde el menú de la bandeja: cierra todo y apaga el motor.
        crate::native::quit();
        }
    });
    rsx! {}
}

/// La parte de la prueba que corre en la ventana del desk (#13). Al terminar
/// cierra su ventana: con la principal oculta, la app tiene que salir sola.
#[component]
pub fn PopoutProbe() -> Element {
    use_future(|| async move {
        if report_path().is_none() {
            return;
        }
        let mut eval = document::eval(POPOUT_SCRIPT);
        match eval.recv::<serde_json::Value>().await {
            Ok(message) => record("popout", message.get("ready").cloned().unwrap_or(message)),
            Err(error) => return record("popout", serde_json::json!({ "error": error.to_string() })),
        }
        OWNER_CLOSED.notified().await;
        let _ = eval.send(serde_json::json!(true));
        match eval.recv::<serde_json::Value>().await {
            Ok(message) => record("popout_after_owner_close", message.get("afterOwnerClose").cloned().unwrap_or(message)),
            Err(error) => record("popout_after_owner_close", serde_json::json!({ "error": error.to_string() })),
        }
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        dioxus::desktop::window().close();
    });
    rsx! {}
}
