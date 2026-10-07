//! Prueba de popouts, pins y desks temporales (#22), solo con
//! `ORGTREE_TAURI_POPOUT_PROBE=<archivo>`.
//!
//! Un director en Rust recorre los pasos sobre el renderer real y el motor de
//! fixture: evalúa `pprobe.js` en la ventana `win-1`, espera su resultado (por
//! el título de la página) y mide los popouts del lado nativo
//! (`popouts::report`: lo que pidió el renderer, el área cliente real, si está
//! maximizado, minimizado o enfocado, y cómo se cerró cada uno).
//!
//! 1. La primitiva: `window.open('', nombre, 'left,top,width,height')` abre
//!    con ese rectángulo de área cliente, y `window.close()` cierra la ventana
//!    entera.
//! 2. El desk real de `worker` en un popout: el estado, maximizar, restaurar,
//!    minimizar y enfocar desde los botones del renderer.
//! 3. Un desk temporal que toma prestado el desk del popout y lo devuelve a
//!    una ventana con el mismo rectángulo.
//! 4. El modal de Usage anclado (queda encima y en su lugar) y su pop-out con
//!    el rectángulo del panel, sin cerrar el modal antes de que la ventana lo
//!    adopte; el cierre desde su botón de ventana.
//! 5. Devolver el desk a la ventana principal (`window.close()`): no queda
//!    ninguna ventana de popout, en blanco o no.
//!
//! El reporte (`<archivo>`) se reescribe en cada paso, así que un paso que no
//! llega deja la evidencia de los anteriores. `<archivo>.<nombre>` son los
//! marcadores para las capturas.

use crate::{mainwin, popouts};
use serde_json::{json, Map, Value};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};
use tauri::{Manager, PhysicalPosition, PhysicalSize};

pub const PREFIX: &str = "orgtree-pprobe:";
pub const PAUSE_PREFIX: &str = "orgtree-pprobe-pause:";

pub struct PopoutProbe {
    out: PathBuf,
    results: Mutex<HashMap<String, Value>>,
    loaded: Mutex<HashSet<String>>,
    signal: Condvar,
    report: Mutex<Map<String, Value>>,
}

impl PopoutProbe {
    pub fn from_env() -> Option<PopoutProbe> {
        let out = PathBuf::from(std::env::var_os("ORGTREE_TAURI_POPOUT_PROBE").filter(|v| !v.is_empty())?);
        Some(PopoutProbe {
            out,
            results: Mutex::new(HashMap::new()),
            loaded: Mutex::new(HashSet::new()),
            signal: Condvar::new(),
            report: Mutex::new(Map::new()),
        })
    }

    fn marker(&self, name: &str) -> PathBuf {
        let mut path = self.out.clone().into_os_string();
        path.push(format!(".{name}"));
        PathBuf::from(path)
    }

    pub fn loaded(&self, label: &str) {
        self.loaded.lock().unwrap().insert(label.to_string());
        self.signal.notify_all();
    }

    /// El título de la ventana: un resultado de paso o un pedido de captura.
    pub fn title(&self, label: &str, title: &str) {
        if let Some(name) = title.strip_prefix(PAUSE_PREFIX) {
            let _ = std::fs::write(self.marker(name), b"");
        } else if let Some(rest) = title.strip_prefix(PREFIX) {
            let Some((step, json)) = rest.split_once(':') else { return };
            let value = serde_json::from_str(json).unwrap_or(Value::String(json.into()));
            self.results.lock().unwrap().insert(format!("{label}:{step}"), value);
            let _guard = self.loaded.lock().unwrap();
            self.signal.notify_all();
        }
    }

    fn put(&self, key: &str, value: Value) {
        let mut report = self.report.lock().unwrap();
        report.insert(key.to_string(), value);
        let _ = std::fs::write(&self.out, serde_json::to_vec_pretty(&*report).unwrap_or_default());
    }

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

    /// Corre un paso de `pprobe.js` en `win-1` y devuelve su resultado.
    fn step(&self, app: &tauri::AppHandle, step: &str, args: Value) -> Value {
        let key = format!("{MAIN}:{step}");
        self.results.lock().unwrap().remove(&key);
        let Some(window) = app.get_webview_window(MAIN) else { return json!({ "error": "no existe win-1" }) };
        let script = format!("{}\n;window.__pprobe.run({}, {});", include_str!("pprobe.js"), json!(step), args);
        if let Err(error) = window.eval(script) {
            return json!({ "error": error.to_string() });
        }
        if self.wait(STEP, |p| p.results.lock().unwrap().contains_key(&key)) {
            self.results.lock().unwrap().remove(&key).unwrap_or(Value::Null)
        } else {
            json!({ "timeout": step })
        }
    }

    /// Un paso y su resultado en el reporte.
    fn record(&self, app: &tauri::AppHandle, step: &str, args: Value) -> Value {
        let value = self.step(app, step, args);
        self.put(step, value.clone());
        value
    }

    fn shot(&self, app: &tauri::AppHandle, name: &str) {
        self.step(app, "shot", json!({ "name": name }));
    }
}

const MAIN: &str = "win-1";
const STEP: Duration = Duration::from_secs(60);
/// `popouts::register` vuelve a medir a los 200 y 800 ms.
const SETTLE: Duration = Duration::from_millis(1500);

/// La ventana principal: su área cliente en pantalla, para ubicar los
/// rectángulos que el renderer mide en coordenadas de la página.
fn main_native(app: &tauri::AppHandle) -> Value {
    let Some(window) = app.get_webview_window(MAIN) else { return Value::Null };
    let inner = window.inner_position().ok();
    let size = window.inner_size().ok();
    let outer = window.outer_position().ok();
    json!({
        "client": { "x": inner.map(|p| p.x), "y": inner.map(|p| p.y), "width": size.map(|s| s.width), "height": size.map(|s| s.height) },
        "outer": { "x": outer.map(|p| p.x), "y": outer.map(|p| p.y) },
        "scale": window.scale_factor().ok(),
        "focused": window.is_focused().ok(),
    })
}

fn native(app: &tauri::AppHandle) -> Value {
    json!({ "popouts": popouts::report(app), "closed": popouts::closed_log(app), "main": main_native(app) })
}

/// Arranca el director si la prueba está pedida.
pub fn start(app: tauri::AppHandle) {
    if app.state::<crate::Shell>().popout_probe.is_none() {
        return;
    }
    std::thread::Builder::new()
        .name("orgtree-popout-probe".into())
        .spawn(move || {
            let state = app.state::<crate::Shell>();
            let probe = state.popout_probe.as_ref().unwrap();
            run(&app, probe);
            probe.put("done", json!(true));
            std::thread::sleep(Duration::from_millis(500));
            mainwin::request_quit(&app);
        })
        .expect("no se pudo crear el director de la prueba de popouts");
}

fn run(app: &tauri::AppHandle, probe: &PopoutProbe) {
    if !probe.wait(Duration::from_secs(240), |p| p.loaded.lock().unwrap().contains(MAIN)) {
        return probe.put("error", json!("la primera ventana no cargó"));
    }
    // La ventana principal en un lugar conocido, entera dentro del área de trabajo.
    let (x, y, w, h) = match app.primary_monitor().ok().flatten() {
        Some(m) => {
            let a = m.work_area();
            (a.position.x, a.position.y, a.size.width, a.size.height)
        }
        None => (0, 0, 1280, 720),
    };
    if let Some(window) = app.get_webview_window(MAIN) {
        let size = (w.saturating_sub(60).min(980), h.saturating_sub(40).min(700));
        let _ = window.unmaximize();
        let _ = window.set_size(PhysicalSize::new(size.0, size.1));
        mainwin::exact_size(&window, size);
        let _ = window.set_position(PhysicalPosition::new(x + 20, y + 10));
        let _ = window.set_focus();
    }
    std::thread::sleep(Duration::from_millis(1200));
    probe.put("main", main_native(app));
    probe.record(app, "org", json!({}));

    // 1. La primitiva, con un rectángulo en coordenadas de pantalla.
    let client = main_native(app)["client"].clone();
    let left = client["x"].as_i64().unwrap_or(0) + 140;
    let top = client["y"].as_i64().unwrap_or(0) + 90;
    probe.record(app, "primitive", json!({ "left": left, "top": top, "width": 420, "height": 320 }));
    std::thread::sleep(SETTLE);
    probe.put("primitiveNative", native(app));
    probe.record(app, "primitiveClose", json!({}));
    probe.wait(Duration::from_secs(5), |_| popouts::report(app).as_array().is_some_and(|a| a.is_empty()));
    std::thread::sleep(Duration::from_millis(800));
    probe.put("primitiveClosedNative", native(app));

    // 2. El desk real en un popout y sus controles.
    probe.record(app, "desk", json!({}));
    std::thread::sleep(SETTLE);
    probe.put("deskNative", native(app));
    probe.shot(app, "popout-desk");
    probe.record(app, "state", json!({}));
    probe.record(app, "maximize", json!({}));
    std::thread::sleep(Duration::from_millis(800));
    probe.put("maximizedNative", native(app));
    probe.shot(app, "popout-maximized");
    probe.record(app, "restore", json!({}));
    std::thread::sleep(Duration::from_millis(800));
    probe.put("restoredNative", native(app));
    probe.record(app, "minimize", json!({}));
    std::thread::sleep(Duration::from_millis(800));
    probe.put("minimizedNative", native(app));
    probe.record(app, "focus", json!({}));
    std::thread::sleep(Duration::from_millis(800));
    probe.put("focusedNative", native(app));

    // 3. El desk temporal toma prestado el desk del popout y lo devuelve.
    probe.put("beforeTempNative", native(app));
    probe.record(app, "tempDesk", json!({}));
    std::thread::sleep(SETTLE);
    probe.put("duringTempNative", native(app));
    probe.shot(app, "tempdesk-open");
    probe.record(app, "tempDeskClose", json!({}));
    std::thread::sleep(SETTLE);
    probe.put("afterTempNative", native(app));
    probe.shot(app, "tempdesk-restored");

    // 4. El modal de Usage: anclado, y su pop-out con el rectángulo del panel.
    probe.record(app, "modalPin", json!({}));
    probe.put("modalPinNative", native(app));
    probe.shot(app, "modal-pinned");
    probe.record(app, "modalPopout", json!({}));
    std::thread::sleep(SETTLE);
    probe.put("modalNative", native(app));
    probe.shot(app, "modal-popout");
    probe.record(app, "modalClose", json!({}));
    std::thread::sleep(Duration::from_millis(800));
    probe.put("modalClosedNative", native(app));

    // 5. El desk vuelve a la ventana principal: el renderer cierra el popout
    //    con `window.close()`.
    probe.record(app, "deskClose", json!({}));
    std::thread::sleep(SETTLE);
    probe.put("finalNative", native(app));
    probe.shot(app, "popouts-closed");
}
