//! Prueba de ventanas por organización (#20), solo con
//! `ORGTREE_TAURI_WINDOWS_PROBE=<archivo>` y `ORGTREE_TAURI_WINDOWS_RUN=1|2`.
//!
//! Un director en Rust recorre los pasos: evalúa `wprobe.js` en la ventana
//! que corresponde, espera su resultado (por el título de la página) y mira
//! el estado del shell (ventanas, posiciones, foco, preferencias, registro de
//! inicio). El CI arranca la app dos veces:
//!
//! 1. Homepage → vista de creación y vuelta, la Homepage se liga a
//!    `spike-fixture`, una ventana de creación aparte crea `segunda` (con la
//!    confirmación de cambios sin guardar, que el CI responde "seguir
//!    editando"), `requestOrg` abre `tercera` en otra ventana y reabrir una org
//!    enfoca la existente; la dueña de las notificaciones; el clic en una
//!    notificación va a la ventana de su org, y la abre si estaba cerrada;
//!    preferencias y tema efectivo; las ventanas se ubican en posiciones
//!    conocidas y la app sale.
//! 2. Las ventanas vuelven en sus posiciones; la que el CI agregó a la sesión
//!    para una org que no existe abre como ventana de error, dentro de la
//!    pantalla aunque se guardó fuera; las preferencias persisten.
//!
//! El reporte (`<archivo>`) se reescribe en cada paso, así que un paso que no
//! llega deja la evidencia de los anteriores. `<archivo>.<nombre>` son los
//! marcadores para las capturas.

use crate::mainwin;
use serde_json::{json, Map, Value};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};
use tauri::{Manager, PhysicalPosition, PhysicalSize};

pub const PREFIX: &str = "orgtree-wprobe:";
pub const PAUSE_PREFIX: &str = "orgtree-wprobe-pause:";

pub struct WindowsProbe {
    out: PathBuf,
    run: u32,
    results: Mutex<HashMap<String, Value>>,
    loaded: Mutex<HashSet<String>>,
    signal: Condvar,
    logs: Mutex<Map<String, Value>>,
    report: Mutex<Map<String, Value>>,
}

impl WindowsProbe {
    pub fn from_env() -> Option<WindowsProbe> {
        let out = PathBuf::from(std::env::var_os("ORGTREE_TAURI_WINDOWS_PROBE").filter(|v| !v.is_empty())?);
        let run = std::env::var("ORGTREE_TAURI_WINDOWS_RUN").ok().and_then(|v| v.parse().ok()).unwrap_or(1);
        Some(WindowsProbe {
            out,
            run,
            results: Mutex::new(HashMap::new()),
            loaded: Mutex::new(HashSet::new()),
            signal: Condvar::new(),
            logs: Mutex::new(Map::new()),
            report: Mutex::new(Map::new()),
        })
    }

    fn marker(&self, name: &str) -> PathBuf {
        let mut path = self.out.clone().into_os_string();
        path.push(format!(".{name}"));
        PathBuf::from(path)
    }

    /// Una ventana principal terminó de cargar un documento.
    pub fn loaded(&self, _app: &tauri::AppHandle, label: &str) {
        self.loaded.lock().unwrap().insert(label.to_string());
        self.signal.notify_all();
    }

    /// El título de una ventana: un resultado de paso o un pedido de captura.
    pub fn title(&self, label: &str, title: &str) {
        if let Some(name) = title.strip_prefix(PAUSE_PREFIX) {
            let _ = std::fs::write(self.marker(name), b"");
        } else if let Some(rest) = title.strip_prefix(PREFIX) {
            let Some((step, json)) = rest.split_once(':') else { return };
            let value = serde_json::from_str(json).unwrap_or(Value::String(json.into()));
            self.results.lock().unwrap().insert(format!("{label}:{step}"), value);
            // `loaded` comparte la condición: se despierta a quien espere.
            let _guard = self.loaded.lock().unwrap();
            self.signal.notify_all();
        }
    }

    /// Lo que hizo el shell (diálogos, eventos retenidos, la bandeja…).
    pub fn log(&self, name: &str, entry: Value) {
        if name == "dialog" && entry["phase"] == "shown" {
            let _ = std::fs::write(self.marker("dialog-shown"), b"");
        }
        let mut logs = self.logs.lock().unwrap();
        match logs.entry(name.to_string()).or_insert_with(|| Value::Array(Vec::new())) {
            Value::Array(rows) => rows.push(entry),
            _ => {}
        }
        drop(logs);
        let _guard = self.loaded.lock().unwrap();
        self.signal.notify_all();
    }

    fn logs(&self, name: &str) -> Vec<Value> {
        self.logs.lock().unwrap().get(name).and_then(Value::as_array).cloned().unwrap_or_default()
    }

    fn put(&self, key: &str, value: Value) {
        let mut report = self.report.lock().unwrap();
        report.insert(key.to_string(), value);
        report.insert("run".into(), json!(self.run));
        report.insert("logs".into(), Value::Object(self.logs.lock().unwrap().clone()));
        let _ = std::fs::write(&self.out, serde_json::to_vec_pretty(&*report).unwrap_or_default());
    }

    /// Espera una condición sobre el estado de la prueba, con tiempo máximo.
    fn wait(&self, timeout: Duration, mut done: impl FnMut(&Self) -> bool) -> bool {
        let end = Instant::now() + timeout;
        let mut guard = self.loaded.lock().unwrap();
        loop {
            drop(guard);
            if done(self) {
                return true;
            }
            guard = self.loaded.lock().unwrap();
            let now = Instant::now();
            if now >= end {
                return false;
            }
            guard = self.signal.wait_timeout(guard, (end - now).min(Duration::from_millis(500))).unwrap().0;
        }
    }

    fn is_loaded(&self, label: &str) -> bool {
        self.loaded.lock().unwrap().contains(label)
    }

    /// Corre un paso de `wprobe.js` en la ventana `label` y devuelve su resultado.
    fn step(&self, app: &tauri::AppHandle, label: &str, step: &str, args: Value, timeout: Duration) -> Value {
        let key = format!("{label}:{step}");
        self.results.lock().unwrap().remove(&key);
        let Some(window) = app.get_webview_window(label) else { return json!({ "error": format!("no existe {label}") }) };
        let script = format!("{}\n;window.__wprobe.run({}, {});", include_str!("wprobe.js"), json!(step), args);
        if let Err(error) = window.eval(script) {
            return json!({ "error": error.to_string() });
        }
        if self.wait(timeout, |p| p.results.lock().unwrap().contains_key(&key)) {
            self.results.lock().unwrap().remove(&key).unwrap_or(Value::Null)
        } else {
            json!({ "timeout": step })
        }
    }

    fn snapshot(&self, app: &tauri::AppHandle) -> Value {
        mainwin::report(app)
    }
}

/// Arranca el director si la prueba está pedida.
pub fn start(app: tauri::AppHandle) {
    if app.state::<crate::Shell>().windows_probe.is_none() {
        return;
    }
    std::thread::Builder::new()
        .name("orgtree-windows-probe".into())
        .spawn(move || {
            let state = app.state::<crate::Shell>();
            let probe = state.windows_probe.as_ref().unwrap();
            if probe.run == 1 {
                run_one(&app, probe);
            } else {
                run_two(&app, probe);
            }
            probe.put("done", json!(true));
            std::thread::sleep(Duration::from_millis(500));
            mainwin::request_quit(&app);
        })
        .expect("no se pudo crear el director de la prueba de ventanas");
}

fn find(app: &tauri::AppHandle, org: &str) -> Option<String> {
    app.state::<crate::Shell>().windows.lock().unwrap().holder(org).map(|e| e.id.clone())
}

fn area(app: &tauri::AppHandle) -> (i32, i32, u32, u32) {
    let monitor = app.primary_monitor().ok().flatten();
    match monitor {
        Some(m) => {
            let a = m.work_area();
            (a.position.x, a.position.y, a.size.width, a.size.height)
        }
        None => (0, 0, 1280, 720),
    }
}

fn place(app: &tauri::AppHandle, label: &str, x: i32, y: i32, w: u32, h: u32) {
    if let Some(window) = app.get_webview_window(label) {
        let _ = window.unmaximize();
        let _ = window.set_size(PhysicalSize::new(w, h));
        let _ = window.set_position(PhysicalPosition::new(x, y));
    }
}

fn prefs_file(app: &tauri::AppHandle) -> Value {
    let Ok(profile) = crate::profile_dir(app) else { return Value::Null };
    let read = |name: &str| std::fs::read(profile.join(name)).ok().and_then(|b| serde_json::from_slice::<Value>(&b).ok());
    json!({
        "preferences": read("preferences.json"),
        "placement": read("window-placement.json"),
        "autostart": crate::autostart::current(),
    })
}

const STEP: Duration = Duration::from_secs(60);

fn run_one(app: &tauri::AppHandle, probe: &WindowsProbe) {
    if !probe.wait(Duration::from_secs(240), |p| p.is_loaded("win-1")) {
        return probe.put("error", json!("la primera ventana no cargó"));
    }
    probe.put("home", probe.step(app, "win-1", "home", json!({}), STEP));
    probe.put("createSwitch", probe.step(app, "win-1", "createSwitch", json!({}), STEP));
    probe.put("bindHome", probe.step(app, "win-1", "bindHome", json!({}), STEP));

    // Una ventana de creación aparte, desde la ventana de la org.
    let created = probe.step(app, "win-1", "openCreate", json!({}), STEP);
    probe.put("openCreate", created.clone());
    let Some(create) = created["created"]["windowId"].as_str().map(String::from) else {
        return probe.put("error", json!("no se abrió la ventana de creación"));
    };
    probe.wait(STEP, |p| p.is_loaded(&create));
    probe.put("createType", probe.step(app, &create, "createType", json!({}), STEP));
    probe.put("createShot", probe.step(app, &create, "shot", json!({ "name": "create-window" }), STEP));
    // Cerrar con cambios sin guardar: la confirmación. El CI responde "seguir editando".
    if let Some(window) = app.get_webview_window(&create) {
        let _ = window.close();
    }
    let answered = probe.wait(Duration::from_secs(90), |p| p.logs("dialog").iter().any(|e| e["phase"] == "answered"));
    std::thread::sleep(Duration::from_millis(800));
    probe.put("confirm", json!({
        "answered": answered,
        "dialog": probe.logs("dialog"),
        "stillOpen": app.get_webview_window(&create).is_some(),
        "unsaved": probe.logs("unsaved"),
    }));
    probe.put("createSubmit", probe.step(app, &create, "createSubmit", json!({}), STEP));

    // requestOrg de una org cerrada desde una ventana con org: una ventana nueva.
    let opened = probe.step(app, "win-1", "requestOrg", json!({ "org": "tercera" }), STEP);
    probe.put("requestTercera", opened.clone());
    let tercera = opened["outcome"]["windowId"].as_str().map(String::from).unwrap_or_default();
    probe.wait(STEP, |p| p.is_loaded(&tercera));
    probe.put("describeTercera", probe.step(app, &tercera, "describe", json!({}), STEP));

    // Dos orgs lado a lado (y la tercera abajo), para la captura y para la restauración.
    let (x, y, w, h) = area(app);
    let half = (w / 2).saturating_sub(30).max(crate::mainwin::MIN_SIZE.0);
    let tall = h.saturating_sub(120).max(crate::mainwin::MIN_SIZE.1);
    let segunda = find(app, "segunda").unwrap_or_default();
    place(app, "win-1", x + 20, y + 20, half, tall);
    // Entera dentro del área de trabajo (en CI mide 1024 de ancho): la
    // restauración tiene que devolverla en el mismo lugar.
    let right = (x + (w / 2) as i32 + 10).min(x + w as i32 - half as i32).max(x);
    place(app, &segunda, right, y + 40, half, tall);
    place(app, &tercera, x + (w / 4) as i32, y + 90, 700, 520);
    std::thread::sleep(Duration::from_millis(1200));
    if let Some(window) = app.get_webview_window(&segunda) {
        let _ = window.set_focus();
    }
    std::thread::sleep(Duration::from_millis(500));
    probe.put("placed", probe.snapshot(app));
    probe.put("twoShot", probe.step(app, &segunda, "shot", json!({ "name": "two-orgs" }), STEP));

    // Reabrir una org abierta enfoca su ventana y no abre otra.
    let before = mainwin::ids(app).len();
    let again = probe.step(app, &tercera, "requestOrg", json!({ "org": "spike-fixture" }), STEP);
    std::thread::sleep(Duration::from_millis(800));
    let focused = app.get_webview_window("win-1").and_then(|w| w.is_focused().ok());
    probe.put("refocus", json!({ "step": again, "windowsBefore": before, "windowsAfter": mainwin::ids(app).len(), "win1Focused": focused }));
    probe.put("refocusShot", probe.step(app, "win-1", "shot", json!({ "name": "refocus" }), STEP));

    // Solo la primera ventana escribe las notificaciones globales.
    probe.put("ownerSegunda", probe.step(app, &segunda, "owner", json!({}), STEP));
    probe.put("ownerWin1", json!({ "identity": mainwin::identity(app, "win-1") }));

    // El clic en una notificación va a la ventana de SU org.
    probe.step(app, &segunda, "listen", json!({}), STEP);
    let click = |org: &str, id: &str| json!({ "type": "notification-click", "data": { "org": org, "id": id, "kind": "question", "title": "Prueba", "body": "Prueba" } });
    mainwin::reveal_org_item(app, "segunda", click("segunda", "wprobe-click-1"));
    std::thread::sleep(Duration::from_millis(800));
    let segunda_focused = app.get_webview_window(&segunda).and_then(|w| w.is_focused().ok());
    probe.put("clickOpen", json!({ "page": probe.step(app, &segunda, "clicks", json!({}), STEP), "focused": segunda_focused }));
    // Con la ventana de la org cerrada, el clic la abre y el evento espera a que cargue.
    if let Some(window) = app.get_webview_window(&tercera) {
        let _ = window.close();
    }
    let closed = probe.wait(Duration::from_secs(10), |_| app.get_webview_window(&tercera).is_none());
    let orgs_after_close = app.state::<crate::Shell>().windows.lock().unwrap().open_orgs();
    mainwin::reveal_org_item(app, "tercera", click("tercera", "wprobe-click-2"));
    let reopened = probe.wait(STEP, |_| find(app, "tercera").is_some());
    let fresh = find(app, "tercera").unwrap_or_default();
    probe.wait(STEP, |p| p.is_loaded(&fresh));
    std::thread::sleep(Duration::from_millis(1500));
    probe.put("clickClosed", json!({
        "closed": closed, "openOrgsAfterClose": orgs_after_close, "reopened": reopened, "window": fresh,
        "held": probe.logs("held"),
    }));
    probe.put("clickShot", probe.step(app, &fresh, "shot", json!({ "name": "click-reopened" }), STEP));
    place(app, &fresh, x + (w / 4) as i32, y + 90, 700, 520);

    // Preferencias persistentes, registro de inicio y tema efectivo.
    let patch = json!({ "startAtLogin": true, "exitOnClose": false, "startupMode": "restore", "visualTheme": "codex", "notifyAllMail": true });
    probe.put("prefs", probe.step(app, "win-1", "prefs", json!({ "patch": patch, "theme": "codex" }), STEP));
    probe.put("effectiveTheme", json!(app.state::<crate::Shell>().effective_theme.lock().unwrap().clone()));
    std::thread::sleep(Duration::from_millis(1500));
    probe.put("files", prefs_file(app));
    probe.put("final", probe.snapshot(app));
}

fn run_two(app: &tauri::AppHandle, probe: &WindowsProbe) {
    // Las ventanas de la sesión anterior: esperar a que todas carguen.
    let ready = probe.wait(Duration::from_secs(240), |p| {
        let ids = mainwin::ids(app);
        ids.len() >= 4 && ids.iter().all(|id| p.is_loaded(id))
    });
    std::thread::sleep(Duration::from_millis(2000));
    probe.put("restored", json!({ "ready": ready, "startup": probe.logs("startup"), "shell": probe.snapshot(app), "files": prefs_file(app) }));
    let mut pages = Map::new();
    for id in mainwin::ids(app) {
        pages.insert(id.clone(), probe.step(app, &id, "describe", json!({}), STEP));
    }
    probe.put("pages", Value::Object(pages));
    let first = mainwin::ids(app).first().cloned().unwrap_or_default();
    probe.put("prefsBefore", probe.step(app, &first, "getPrefs", json!({}), STEP));
    probe.put("restoredShot", probe.step(app, &first, "shot", json!({ "name": "restored" }), STEP));
    if let Some(error) = find(app, "ya-no-existe") {
        mainwin::reveal(app, &error);
        std::thread::sleep(Duration::from_millis(600));
        probe.put("errorShot", probe.step(app, &error, "shot", json!({ "name": "error-window" }), STEP));
    }
    // Apagar el inicio con Windows borra la entrada de Run.
    probe.put("prefsOff", probe.step(app, &first, "prefs", json!({ "patch": { "startAtLogin": false }, "theme": "codex" }), STEP));
    std::thread::sleep(Duration::from_millis(800));
    probe.put("files", prefs_file(app));
}
