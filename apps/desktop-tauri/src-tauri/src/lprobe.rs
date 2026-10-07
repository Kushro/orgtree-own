//! Prueba del ciclo de vida del motor (#19), solo con
//! `ORGTREE_TAURI_LIFECYCLE_PROBE=<archivo>` y `ORGTREE_TAURI_LIFECYCLE_RUN`:
//!
//! - `full`: el motor arranca contra una raíz que tiene otro motor (lo lanza el
//!   CI), así que la ventana de arranque muestra el rechazo con Reintentar; el
//!   CI libera la raíz y el director toca el botón real. El motor de fixture
//!   simula una conversión (`database-convert…`) con un plazo de silencio corto.
//!   Ya en la app: estado y eventos del puente, una caída con el aviso en la
//!   ventana, una recarga que falla con el motor caído y la recuperación de esa
//!   ventana, un pedido de mantenimiento `restart` (el motor se reinicia) y uno
//!   `update` (sin updater: `unavailable`), y la salida por la bandeja.
//! - `close`: con `exitOnClose`, cerrar la última ventana sale de la app.
//! - `session`: la app queda lista y el CI le manda `WM_QUERYENDSESSION` y
//!   `WM_ENDSESSION`, como Windows al cerrar la sesión.
//!
//! El reporte (`<archivo>`) se reescribe en cada paso; `<archivo>.<nombre>`
//! son marcadores para las capturas, que el CI borra después de sacarlas.

use crate::{lifecycle, mainwin, Shell};
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tauri::Manager;

pub const PREFIX: &str = "orgtree-lprobe:";
const WINDOW: &str = "win-1";

pub struct LifecycleProbe {
    out: PathBuf,
    run: String,
    began: Instant,
    results: Mutex<HashMap<String, Value>>,
    loads: Mutex<HashMap<String, u32>>,
    logs: Mutex<Map<String, Value>>,
    report: Mutex<Map<String, Value>>,
    /// Mientras está puesto, el vigilante espera antes de relanzar el motor.
    hold: AtomicBool,
}

impl LifecycleProbe {
    pub fn from_env() -> Option<LifecycleProbe> {
        let out = PathBuf::from(std::env::var_os("ORGTREE_TAURI_LIFECYCLE_PROBE").filter(|v| !v.is_empty())?);
        let run = std::env::var("ORGTREE_TAURI_LIFECYCLE_RUN").unwrap_or_else(|_| "full".into());
        Some(LifecycleProbe {
            out,
            run,
            began: Instant::now(),
            results: Mutex::new(HashMap::new()),
            loads: Mutex::new(HashMap::new()),
            logs: Mutex::new(Map::new()),
            report: Mutex::new(Map::new()),
            hold: AtomicBool::new(false),
        })
    }

    fn ms(&self) -> u64 {
        self.began.elapsed().as_millis() as u64
    }

    fn path(&self, name: &str) -> PathBuf {
        let mut path = self.out.clone().into_os_string();
        path.push(format!(".{name}"));
        PathBuf::from(path)
    }

    /// Un marcador para que el CI saque una captura.
    pub fn marker(&self, name: &str) {
        let _ = std::fs::write(self.path(name), b"");
    }

    /// Marca y espera a que el CI saque la captura (borra el marcador).
    fn shot(&self, name: &str) {
        self.marker(name);
        let path = self.path(name);
        self.wait(Duration::from_secs(20), || !path.exists());
    }

    pub fn loaded(&self, label: &str) {
        *self.loads.lock().unwrap().entry(label.to_string()).or_default() += 1;
    }

    fn loads(&self, label: &str) -> u32 {
        self.loads.lock().unwrap().get(label).copied().unwrap_or(0)
    }

    pub fn title(&self, label: &str, title: &str) {
        let Some(rest) = title.strip_prefix(PREFIX) else { return };
        let Some((step, json)) = rest.split_once(':') else { return };
        let value = serde_json::from_str(json).unwrap_or(Value::String(json.into()));
        self.results.lock().unwrap().insert(format!("{label}:{step}"), value);
    }

    /// Lo que hizo el shell: estados, fases, rechazos, relanzamientos, salida.
    pub fn log(&self, name: &str, mut entry: Value) {
        if let Value::Object(map) = &mut entry {
            map.insert("t".into(), json!(self.ms()));
        }
        let mut logs = self.logs.lock().unwrap();
        if let Value::Array(rows) = logs.entry(name.to_string()).or_insert_with(|| Value::Array(Vec::new())) {
            rows.push(entry);
        }
        drop(logs);
        self.flush();
    }

    fn logs(&self, name: &str) -> Vec<Value> {
        self.logs.lock().unwrap().get(name).and_then(Value::as_array).cloned().unwrap_or_default()
    }

    fn put(&self, key: &str, value: Value) {
        self.report.lock().unwrap().insert(key.to_string(), value);
        self.flush();
    }

    pub fn flush(&self) {
        let mut report = self.report.lock().unwrap().clone();
        report.insert("run".into(), json!(self.run));
        report.insert("logs".into(), Value::Object(self.logs.lock().unwrap().clone()));
        let _ = std::fs::write(&self.out, serde_json::to_vec_pretty(&report).unwrap_or_default());
    }

    fn wait(&self, timeout: Duration, mut done: impl FnMut() -> bool) -> bool {
        let end = Instant::now() + timeout;
        loop {
            if done() {
                return true;
            }
            if Instant::now() >= end {
                return false;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    /// El vigilante llama esto antes de relanzar: la prueba lo retiene mientras
    /// mira la ventana con el motor caído (a lo sumo 90 s).
    pub fn before_restart(&self) {
        self.wait(Duration::from_secs(90), || !self.hold.load(Ordering::SeqCst));
    }

    /// Corre un paso de `lprobe.js` en la ventana y devuelve su resultado.
    fn step(&self, app: &tauri::AppHandle, step: &str, args: Value, timeout: Duration) -> Value {
        let key = format!("{WINDOW}:{step}");
        self.results.lock().unwrap().remove(&key);
        let Some(window) = app.get_webview_window(WINDOW) else { return json!({ "error": "no existe la ventana" }) };
        let script = format!("{}\n;window.__lprobe.run({}, {});", include_str!("lprobe.js"), json!(step), args);
        if let Err(error) = window.eval(script) {
            return json!({ "error": error.to_string() });
        }
        if self.wait(timeout, || self.results.lock().unwrap().contains_key(&key)) {
            self.results.lock().unwrap().remove(&key).unwrap_or(Value::Null)
        } else {
            json!({ "timeout": step })
        }
    }
}

fn probe(app: &tauri::AppHandle) -> &LifecycleProbe {
    app.state::<lifecycle::Lifecycle>().inner().probe.as_ref().expect("prueba de ciclo de vida activa")
}

fn engine_pid(app: &tauri::AppHandle) -> Option<u32> {
    app.state::<Shell>().engine.lock().unwrap().as_ref().and_then(|e| e.pid())
}

/// Arranca el director si la prueba está pedida.
pub fn start(app: tauri::AppHandle) {
    if app.state::<lifecycle::Lifecycle>().probe.is_none() {
        return;
    }
    std::thread::Builder::new()
        .name("orgtree-lifecycle-probe".into())
        .spawn(move || {
            let p = probe(&app);
            p.put("started", json!(true));
            match p.run.as_str() {
                "close" => run_close(&app, p),
                "session" => run_session(&app, p),
                _ => run_full(&app, p),
            }
        })
        .expect("no se pudo crear el director de la prueba de ciclo de vida");
}

fn wait_window(app: &tauri::AppHandle, p: &LifecycleProbe) -> bool {
    let _ = app;
    p.wait(Duration::from_secs(240), || p.loads(WINDOW) >= 1)
}

fn quit(app: &tauri::AppHandle, p: &LifecycleProbe, trigger: &'static str) {
    p.put("done", json!(true));
    std::thread::sleep(Duration::from_millis(500));
    lifecycle::note_exit_trigger(app, trigger);
    mainwin::request_quit(app);
}

fn run_full(app: &tauri::AppHandle, p: &LifecycleProbe) {
    // 1. El rechazo: otro motor tiene la raíz.
    if !p.wait(Duration::from_secs(150), || !p.logs("refused").is_empty()) {
        p.put("error", json!("no hubo rechazo"));
        return quit(app, p, "probe");
    }
    p.put("refused", json!({ "log": p.logs("refused"), "status": lifecycle::status(app) }));
    std::thread::sleep(Duration::from_millis(700));
    p.shot("refused");
    // 2. El CI libera la raíz y avisa; el director toca el botón real.
    let go = p.path("retry-go");
    if !p.wait(Duration::from_secs(180), || go.exists()) {
        p.put("error", json!("el CI no liberó la raíz"));
        return quit(app, p, "probe");
    }
    let clicked = app
        .get_webview_window("splash")
        .map(|w| w.eval("document.getElementById('retry') && document.getElementById('retry').click()").is_ok());
    p.put("retryClicked", json!(clicked));
    // 3. Conversión y arranque.
    if !wait_window(app, p) {
        p.put("error", json!("la ventana principal no cargó"));
        return quit(app, p, "probe");
    }
    p.put("phases", json!(p.logs("phases")));
    let ready = p.step(app, "ready", json!({}), Duration::from_secs(40));
    p.put("ready", json!({ "page": ready, "pid": engine_pid(app) }));
    p.shot("ready");

    // 4. Caída, carga fallida y recuperación.
    p.hold.store(true, Ordering::SeqCst);
    let crashed = app.state::<Shell>().engine.lock().unwrap().as_mut().map(|e| {
        let pid = e.pid();
        e.kill();
        pid
    });
    let stopped = p.wait(Duration::from_secs(10), || lifecycle::status(app)["state"] == "stopped");
    std::thread::sleep(Duration::from_millis(800));
    let down = p.step(app, "down", json!({}), Duration::from_secs(15));
    p.put("down", json!({ "crashedPid": crashed, "stopped": stopped, "status": lifecycle::status(app), "page": down }));
    p.shot("engine-down");
    let before = p.loads(WINDOW);
    if let Some(window) = app.get_webview_window(WINDOW) {
        let _ = window.eval("location.reload()");
    }
    let failed = p.wait(Duration::from_secs(20), || lifecycle::load_failed(app, WINDOW));
    std::thread::sleep(Duration::from_millis(1500));
    p.put("loadFailed", json!({ "recorded": failed, "loadsBefore": before, "url": app.get_webview_window(WINDOW).and_then(|w| w.url().ok()).map(|u| u.to_string()) }));
    p.shot("load-failed");
    p.hold.store(false, Ordering::SeqCst);
    let back = p.wait(Duration::from_secs(180), || {
        lifecycle::is_ready(app) && p.loads(WINDOW) > before + 1 && !lifecycle::load_failed(app, WINDOW)
    });
    std::thread::sleep(Duration::from_millis(500));
    let recovered = p.step(app, "recovered", json!({}), Duration::from_secs(60));
    p.put("recovered", json!({ "back": back, "pid": engine_pid(app), "page": recovered, "relaunched": p.logs("relaunched") }));
    p.shot("recovered");

    // 5. Mantenimiento: reinicio pedido por el motor.
    let pid_before = engine_pid(app);
    let loads_before = p.loads(WINDOW);
    let request = p.step(app, "maintenance", json!({ "action": "restart" }), Duration::from_secs(20));
    let restarted = p.wait(Duration::from_secs(240), || {
        p.logs("relaunched").iter().any(|r| r["why"] == "maintenance") && lifecycle::is_ready(app) && p.loads(WINDOW) > loads_before
    });
    p.put("maintenanceRestart", json!({
        "request": request, "restarted": restarted, "pidBefore": pid_before, "pidAfter": engine_pid(app),
        "maintenance": p.logs("maintenance"), "waits": p.logs("maintenance-wait").len(),
    }));
    // 6. Mantenimiento: actualización sin updater.
    std::thread::sleep(Duration::from_secs(2));
    let request = p.step(app, "maintenance", json!({ "action": "update" }), Duration::from_secs(40));
    let reported = p.wait(Duration::from_secs(150), || lifecycle::last_maintenance(app)["state"] == "unavailable");
    let status = p.step(app, "maintenanceStatus", json!({}), Duration::from_secs(20));
    p.put("maintenanceUpdate", json!({ "request": request, "reported": reported, "page": status }));
    p.shot("maintenance");
    // 7. Salida por la bandeja (el mismo camino que su "Salir").
    quit(app, p, "tray");
}

fn run_close(app: &tauri::AppHandle, p: &LifecycleProbe) {
    if !wait_window(app, p) {
        p.put("error", json!("la ventana principal no cargó"));
        return quit(app, p, "probe");
    }
    let ready = p.step(app, "ready", json!({}), Duration::from_secs(40));
    let exit_on_close = crate::exit_on_close(app);
    p.put("ready", json!({ "page": ready, "pid": engine_pid(app), "exitOnClose": exit_on_close }));
    p.put("done", json!(true));
    // El CI anota el árbol de procesos con la captura, para buscar huérfanos después.
    p.shot("close-ready");
    // Como el botón de cerrar: CloseRequested → `perform_close` → `exitOnClose`.
    if let Some(window) = app.get_webview_window(WINDOW) {
        let _ = window.close();
    }
}

fn run_session(app: &tauri::AppHandle, p: &LifecycleProbe) {
    if !wait_window(app, p) {
        p.put("error", json!("la ventana principal no cargó"));
        return quit(app, p, "probe");
    }
    let ready = p.step(app, "ready", json!({}), Duration::from_secs(40));
    let hwnd = app.get_webview_window(WINDOW).as_ref().and_then(mainwin::hwnd_of);
    p.put("ready", json!({ "page": ready, "pid": engine_pid(app), "hwnd": hwnd }));
    p.put("done", json!(true));
    // El CI manda los mensajes de fin de sesión a esta ventana.
    p.marker("session-ready");
}
