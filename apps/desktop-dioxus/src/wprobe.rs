//! Prueba de las ventanas por organización (#25), solo con
//! `ORGTREE_DIOXUS_PROBE=<archivo>` y `ORGTREE_DIOXUS_PROBE_MODE=windows`, como
//! el `wprobe` del spike de Tauri (#20). Maneja la app compilada sobre el motor
//! de fixture en dos arranques sobre la misma raíz y la misma carpeta de la app
//! (`ORGTREE_DIOXUS_WINDOWS_RUN` = `1` o `2`).
//!
//! Cada ventana principal monta un `WindowAgent`: la prueba le pide correr un
//! script en su página (las señales y `document::eval` son de cada
//! VirtualDom) y recibe lo que el script retorna.
//!
//! Primer arranque: la ventana de inicio lista la org del fixture; elegirla
//! liga esa misma ventana; "New window" abre una ventana de inicio aparte, que
//! se liga a `segunda`; pedir `tercera` desde una ventana con org (como el clic
//! de una notificación) abre otra ventana; desde otra ventana de inicio, la org
//! ya abierta dice "Already open" y elegirla enfoca su ventana sin abrir otra;
//! cerrar esa ventana de inicio (una de varias) la cierra y la saca de la
//! sesión; las preferencias (`startAtLogin`, `exitOnClose`) se guardan y
//! `startAtLogin` escribe el valor de `Run`. Se ubican las tres ventanas en
//! lugares conocidos y se sale por la bandeja.
//!
//! Segundo arranque (el CI agrega a la sesión una org que ya no existe, en un
//! monitor que ya no está): vuelven las cuatro ventanas, las tres orgs en su
//! lugar, la cuarta dentro del área de trabajo y con el error del motor; las
//! preferencias siguen ahí, y apagar `startAtLogin` borra el valor de `Run`.

use crate::orgwindows::{self, Key, MAIN};
use crate::placement::Bounds;
use crate::probe::{marker, record, report_path};
use dioxus::prelude::*;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;
use tokio::sync::{broadcast, oneshot};

fn active() -> bool {
    report_path().is_some() && crate::probe::mode().as_deref() == Some("windows")
}

/// Pedidos de script a una ventana: (ventana, número, script).
fn requests() -> &'static broadcast::Sender<(Key, u64, String)> {
    static CHANNEL: std::sync::OnceLock<broadcast::Sender<(Key, u64, String)>> = std::sync::OnceLock::new();
    CHANNEL.get_or_init(|| broadcast::channel(32).0)
}

static REPLIES: Mutex<Option<HashMap<u64, oneshot::Sender<Value>>>> = Mutex::new(None);
/// Las ventanas cuyo agente ya escucha (un pedido anterior se perdería).
static LISTENING: Mutex<Vec<Key>> = Mutex::new(Vec::new());
static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

/// En cada ventana principal: corre en su página los scripts que la prueba le pide.
#[component]
pub fn WindowAgent(win: Key) -> Element {
    use_future(move || async move {
        if !active() {
            return;
        }
        let mut incoming = requests().subscribe();
        LISTENING.lock().unwrap().push(win);
        loop {
            match incoming.recv().await {
                Ok((target, id, script)) if target == win => {
                    let value = match document::eval(&script).join::<Value>().await {
                        Ok(value) => value,
                        Err(error) => json!({ "error": error.to_string() }),
                    };
                    if let Some(reply) = REPLIES.lock().unwrap().get_or_insert_with(HashMap::new).remove(&id) {
                        let _ = reply.send(value);
                    }
                }
                Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => {}
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    });
    use_drop(move || LISTENING.lock().unwrap().retain(|w| *w != win));
    rsx! {}
}

/// Corre un script en la página de una ventana y espera lo que retorna.
async fn eval_in(win: Key, script: &str) -> Value {
    let id = NEXT.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let (sender, receiver) = oneshot::channel();
    REPLIES.lock().unwrap().get_or_insert_with(HashMap::new).insert(id, sender);
    // La ventana puede estar montándose: se reintenta hasta que la toma.
    for _ in 0..100 {
        if LISTENING.lock().unwrap().contains(&win) && requests().send((win, id, script.to_string())).is_ok() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    match tokio::time::timeout(Duration::from_secs(45), receiver).await {
        Ok(Ok(value)) => value,
        _ => json!({ "error": format!("la ventana {win} no respondió") }),
    }
}

async fn until<T>(seconds: u64, mut test: impl FnMut() -> Option<T>) -> Option<T> {
    let end = std::time::Instant::now() + Duration::from_secs(seconds);
    loop {
        if let Some(value) = test() {
            return Some(value);
        }
        if std::time::Instant::now() >= end {
            return None;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

/// Una ventana: su clave, su org y si es la dueña de las notificaciones.
fn identity(win: Key) -> Value {
    let registry = orgwindows::registry();
    let entry = registry.entry(win);
    json!({
        "window": win,
        "org": entry.and_then(|e| e.org.clone()),
        "homepage": entry.is_some_and(|e| e.org.is_none()),
        "notificationOwner": registry.notification_owner() == win,
    })
}

/// Las ventanas abiertas con su lugar real (posición exterior y área cliente).
fn windows_now() -> Vec<Value> {
    orgwindows::registry()
        .open()
        .iter()
        .map(|entry| {
            let ctx = orgwindows::context(entry.key);
            let bounds = ctx.as_ref().and_then(orgwindows::bounds_of);
            json!({
                "window": entry.key,
                "org": entry.org,
                "mounted": ctx.is_some(),
                "visible": ctx.as_ref().map(|c| c.window.is_visible()),
                "focused": ctx.as_ref().map(|c| c.window.is_focused()),
                "x": bounds.map(|b| b.x), "y": bounds.map(|b| b.y),
                "width": bounds.map(|b| b.width), "height": bounds.map(|b| b.height),
            })
        })
        .collect()
}

fn count() -> usize {
    orgwindows::registry().open().len()
}

/// Espera una fila de org en la página de inicio de una ventana y, si se pide, la elige.
fn pick_script(slug: &str, click: bool) -> String {
    format!(
        r#"
const end = Date.now() + 30000;
let row = null;
while (Date.now() < end) {{
  row = document.querySelector('.welcome-card nav .org[data-slug="{slug}"]');
  if (row) break;
  await new Promise(r => setTimeout(r, 200));
}}
const out = {{ listed: !!row, homepage: !!document.querySelector('.welcome-card'),
  alreadyOpen: !!(row && row.querySelector('.dx-open-elsewhere')) }};
if (row && {click}) row.click();
return out;
"#
    )
}

/// Espera la vista de una org en una ventana: su título, su agente o su error.
fn org_script(name: &str) -> String {
    format!(
        r#"
const end = Date.now() + 30000;
const state = () => ({{
  title: (document.querySelector('.dx-org-view h2') || {{}}).textContent || null,
  agentShown: !!document.querySelector('.dx-org-view .dx-agent[data-node="worker"]'),
  error: (document.querySelector('.dx-org-error') || {{}}).textContent || null,
  loaded: !!document.querySelector('.dx-org-view .dx-user-card'),
  homepage: !!document.querySelector('.welcome-card'),
}});
let s = state();
while (Date.now() < end && !((s.title && s.title.includes('{name}') && s.loaded) || s.error)) {{ await new Promise(r => setTimeout(r, 200)); s = state() }}
await new Promise(r => setTimeout(r, 300));
return state();
"#
    )
}

fn click_script(selector: &str) -> String {
    format!("const b = document.querySelector('{selector}'); if (b) b.click(); return !!b;")
}

/// Un clic que cierra la ventana: primero responde, después hace clic.
fn closing_click_script(selector: &str) -> String {
    format!("const b = document.querySelector('{selector}'); if (b) setTimeout(() => b.click(), 150); return !!b;")
}

fn prefs_patch(patch: Value) -> Value {
    let map = patch.as_object().cloned().unwrap_or_default();
    crate::notify::set_prefs(&map)
}

/// El director: se monta en la ventana principal cuando el motor está listo.
#[component]
pub fn WindowsProbe() -> Element {
    use_future(|| async move {
        if !active() {
            return;
        }
        match std::env::var("ORGTREE_DIOXUS_WINDOWS_RUN").as_deref() {
            Ok("2") => second_run().await,
            _ => first_run().await,
        }
        record("done", json!(true));
        crate::native::quit();
    });
    rsx! {}
}

async fn first_run() {
    // La ventana de inicio lista la org del fixture.
    let home = eval_in(MAIN, &pick_script("spike-fixture", false)).await;
    record("home", json!({ "page": home, "identity": identity(MAIN), "windows": count() }));
    // Dos orgs más, por la API (como las crearía otra ventana).
    let mut created = serde_json::Map::new();
    if let Some(client) = crate::lifecycle::current_client() {
        for name in ["segunda", "tercera"] {
            let made = client.create_org(name, &[], true, &[]).await;
            created.insert(name.into(), json!(made.map(|m| m.slug).map_err(|e| e.to_string())));
        }
    }
    record("created", Value::Object(created));

    // Elegir la org en la ventana de inicio liga esa misma ventana.
    let picked = eval_in(MAIN, &pick_script("spike-fixture", true)).await;
    let bound = eval_in(MAIN, &org_script("spike-fixture")).await;
    record("bindHome", json!({ "picked": picked, "page": bound, "identity": identity(MAIN), "windows": count() }));

    // "New window" abre una ventana de inicio aparte, que se liga a `segunda`.
    let before = count();
    let clicked = eval_in(MAIN, &click_script(".dx-org-view .dx-new-window")).await;
    let second = until(30, || orgwindows::registry().open().iter().map(|e| e.key).find(|k| *k != MAIN && orgwindows::context(*k).is_some())).await;
    let mut second_page = Value::Null;
    if let Some(win) = second {
        let picked = eval_in(win, &pick_script("segunda", true)).await;
        second_page = json!({ "picked": picked, "page": eval_in(win, &org_script("segunda")).await, "identity": identity(win) });
    }
    record("newWindow", json!({ "clicked": clicked, "windowsBefore": before, "window": second, "windows": count(), "second": second_page }));

    // Pedir `tercera` desde una ventana con org (como el clic de una notificación) abre otra.
    let decision = orgwindows::open_org(Some(MAIN), "tercera");
    let third = decision.key();
    let _ = until(30, || orgwindows::context(third).map(|_| ())).await;
    let third_page = eval_in(third, &org_script("tercera")).await;
    record("requestTercera", json!({ "action": decision.action(), "window": third, "page": third_page, "identity": identity(third), "windows": count() }));
    marker("three-orgs");
    tokio::time::sleep(Duration::from_millis(1500)).await;

    // Abrir una org ya abierta la enfoca: desde otra ventana de inicio, la fila de
    // `spike-fixture` dice "Already open" y elegirla enfoca la principal.
    let _ = eval_in(third, &click_script(".dx-org-view .dx-new-window")).await;
    let fourth = until(30, || {
        orgwindows::registry().open().iter().map(|e| e.key).find(|k| ![MAIN, third].contains(k) && Some(*k) != second && orgwindows::context(*k).is_some())
    })
    .await;
    let mut refocus = json!({ "window": fourth });
    if let Some(win) = fourth {
        let windows_before = count();
        let picked = eval_in(win, &pick_script("spike-fixture", true)).await;
        tokio::time::sleep(Duration::from_millis(1200)).await;
        let still_home = eval_in(win, "return !!document.querySelector('.welcome-card');").await;
        let main_focused = orgwindows::context(MAIN).map(|c| c.window.is_focused());
        let log = crate::probe::report_logs("request-org");
        refocus = json!({ "window": win, "picked": picked, "windowsBefore": windows_before, "windowsAfter": count(),
            "stillHomepage": still_home, "mainFocused": main_focused, "requests": log });
        record("refocus", refocus.clone());
        marker("refocus");
        tokio::time::sleep(Duration::from_millis(1500)).await;
        // Cerrar esa ventana de inicio (una de varias) la cierra y la saca de la sesión.
        let clicked = eval_in(win, &closing_click_script(".welcome-card .window-control.close")).await;
        let closed = until(20, || orgwindows::context(win).is_none().then_some(())).await.is_some();
        record("closeOne", json!({ "clicked": clicked, "closed": closed, "windows": count(), "session": orgwindows::saved_session() }));
    } else {
        record("refocus", refocus);
    }

    // Preferencias: `startAtLogin` escribe `Run` (HKCU); `exitOnClose` queda apagado.
    let saved = prefs_patch(json!({ "startAtLogin": true, "exitOnClose": false }));
    let refused = prefs_patch(json!({ "startAtLogin": "sí" }));
    record("prefs", json!({ "saved": saved, "afterBadType": refused["startAtLogin"], "autostart": crate::autostart::current() }));

    // Las tres ventanas en lugares conocidos, enteras dentro del área de trabajo
    // del runner (1024x768): el segundo arranque las tiene que traer de vuelta ahí.
    let places = [(MAIN, (20, 20, 700, 500)), (second.unwrap_or(0), (180, 90, 680, 480)), (third, (300, 180, 660, 500))];
    for (win, (x, y, width, height)) in places {
        orgwindows::set_bounds(win, Bounds { x, y, width, height });
    }
    tokio::time::sleep(Duration::from_millis(2500)).await;
    record("final", json!({ "windows": windows_now(), "areas": orgwindows::current_work_areas() }));
    marker("placed");
    tokio::time::sleep(Duration::from_millis(1500)).await;
}

async fn second_run() {
    // Vuelven las cuatro ventanas de la sesión (una, de una org que ya no existe).
    let all = until(90, || {
        let registry = orgwindows::registry();
        let open = registry.open();
        (open.len() >= 4 && open.iter().all(|e| orgwindows::context(e.key).is_some())).then_some(())
    })
    .await
    .is_some();
    tokio::time::sleep(Duration::from_millis(3000)).await;
    let mut pages = serde_json::Map::new();
    for entry in orgwindows::registry().open() {
        let name = entry.org.clone().unwrap_or_default();
        let page = eval_in(entry.key, &org_script(&name)).await;
        pages.insert(entry.key.to_string(), page);
    }
    record("restored", json!({ "all": all, "windows": windows_now(), "areas": orgwindows::current_work_areas(), "pages": pages }));
    marker("restored");
    tokio::time::sleep(Duration::from_millis(1500)).await;
    // La ventana de error, al frente.
    if let Some(win) = orgwindows::registry().holder("ya-no-existe") {
        orgwindows::reveal(win);
        tokio::time::sleep(Duration::from_millis(800)).await;
        marker("error-window");
        tokio::time::sleep(Duration::from_millis(1500)).await;
    }
    let prefs = crate::notify::prefs();
    let off = prefs_patch(json!({ "startAtLogin": false }));
    record("prefs", json!({ "before": prefs, "after": off, "autostart": crate::autostart::current() }));
}
