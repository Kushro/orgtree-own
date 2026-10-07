//! Ciclo de vida del motor (#19), como `engine.ts`, `conversion-window.ts`,
//! `maintenance.ts` y `window-load-recovery.ts` de Electron, sobre el
//! supervisor de `engine-host`:
//!
//! - **Arranque**: las fases llegan a la ventana de arranque; las de conversión
//!   (`database-convert…`) muestran el mensaje de la ventana de conversión de
//!   Electron y tienen su propio plazo de silencio. Un rechazo (`root-owned`,
//!   `conversion-failed`, un plazo vencido) deja un mensaje claro con
//!   **Reintentar** y **Salir**, en lugar del diálogo fatal de Electron.
//! - **Estado**: `getStatus` y los eventos `engine-status` del contrato
//!   (`EngineStatus`), la línea de estado de la bandeja y un aviso dentro de
//!   cada ventana mientras el motor no está.
//! - **Caídas y cuelgues**: el vigilante de #5 reinicia el motor, ahora con un
//!   presupuesto (3 en 10 minutos), y el de cuelgues sigue las reglas de
//!   `LIVENESS`. Al volver, cada ventana principal navega a su ruta: también la
//!   que quedó en blanco por una carga fallida.
//! - **Mantenimiento**: el pedido del motor (`/api/desktop/status`) se atiende
//!   con el motor y el usuario quietos, como `MaintenanceController`; el estado
//!   reportado sale por `getMaintenanceStatus` y el evento `maintenance`.
//! - **Salida**: todos los caminos terminan en `stop_engine_for_exit`, con el
//!   presupuesto de `QUIT_DEADLINES` y la prueba de que el árbol soltó la raíz.

use crate::{lprobe, mainwin, Shell};
use orgtree_engine_host::{
    conversion_detail, wait_root_released, Engine, EngineError, EngineHandle, EngineOptions,
    LivenessWatch, QuitOutcome, RestartBudget, StartupEvent, LIVENESS, QUIT_STOP_BUDGET,
};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tauri::{Manager, Url, Webview, WebviewWindow};

/// Reinicios automáticos tras una caída: a lo sumo 3 en 10 minutos, como el
/// presupuesto de recuperación de ventanas de Electron (`RECOVERY_LIMIT`). Una
/// caída en cada arranque deja de reiniciarse y lo dice.
const CRASH_LIMIT: usize = 3;
const CRASH_WINDOW: Duration = Duration::from_secs(600);
/// Intentos de relanzar tras una caída: ~2 min, porque en Linux el puerto
/// guardado queda en `TIME_WAIT` unos 60 s y el guardián puede tardar en
/// soltar la raíz (`root-owned` se reintenta).
const RELAUNCH_ATTEMPTS: u32 = 90;
/// El sondeo de `/api/desktop/status`, como el `poll` de 5 s de Electron.
const STATUS_POLL: Duration = Duration::from_secs(5);
/// El mantenimiento espera a que el usuario no toque nada por 60 s.
const USER_IDLE_FOR_MAINTENANCE: Duration = Duration::from_secs(60);
const DIAGNOSTICS_LIVENESS: &str = "diagnostics/engine-liveness.jsonl";

pub struct Lifecycle {
    /// El `EngineStatus` actual, el que devuelve `getStatus`.
    status: Mutex<Value>,
    starting: AtomicBool,
    /// Un relanzamiento o reinicio en curso: uno a la vez, como `restart()`.
    restarting: AtomicBool,
    watching: AtomicBool,
    crashes: Mutex<RestartBudget>,
    hangs: Mutex<RestartBudget>,
    /// Ventanas cuya última carga terminó con el motor caído.
    load_failed: Mutex<HashSet<String>>,
    /// Lo último que mostró la ventana de arranque, para reenviarlo al cargar.
    splash: Mutex<Value>,
    maintenance: Mutex<Maintenance>,
    /// `lastMaintenance` de Electron: lo último reportado, o nada.
    last_maintenance: Mutex<Option<Value>>,
    /// Qué pidió la salida (bandeja, última ventana, renderer, prueba).
    exit_trigger: Mutex<Option<&'static str>>,
    exit_recorded: AtomicBool,
    pub probe: Option<lprobe::LifecycleProbe>,
}

impl Default for Lifecycle {
    fn default() -> Self {
        Lifecycle {
            status: Mutex::new(json!({ "state": "starting" })),
            starting: AtomicBool::new(false),
            restarting: AtomicBool::new(false),
            watching: AtomicBool::new(false),
            crashes: Mutex::new(RestartBudget::new(CRASH_LIMIT, CRASH_WINDOW)),
            hangs: Mutex::new(RestartBudget::new(LIVENESS.restart_limit, LIVENESS.restart_window)),
            load_failed: Mutex::new(HashSet::new()),
            splash: Mutex::new(Value::Null),
            maintenance: Mutex::new(Maintenance::default()),
            last_maintenance: Mutex::new(None),
            exit_trigger: Mutex::new(None),
            exit_recorded: AtomicBool::new(false),
            probe: lprobe::LifecycleProbe::from_env(),
        }
    }
}

fn lc(app: &tauri::AppHandle) -> &Lifecycle {
    app.state::<Lifecycle>().inner()
}

fn shell(app: &tauri::AppHandle) -> &Shell {
    app.state::<Shell>().inner()
}

fn quitting(app: &tauri::AppHandle) -> bool {
    shell(app).quitting.load(Ordering::SeqCst)
}

// ------------------------------------------------------------------ estado

pub fn status(app: &tauri::AppHandle) -> Value {
    lc(app).status.lock().unwrap().clone()
}

pub fn is_ready(app: &tauri::AppHandle) -> bool {
    status(app)["state"] == "ready"
}

pub fn is_restarting(app: &tauri::AppHandle) -> bool {
    lc(app).restarting.load(Ordering::SeqCst)
}

/// Cambia el estado del motor: lo guarda para `getStatus`, lo emite como
/// `engine-status` a cada ventana principal (como `engine.on('status')` en
/// Electron), actualiza la bandeja y el aviso dentro de las ventanas.
pub fn set_status(app: &tauri::AppHandle, state: &str, message: Option<&str>) {
    let value = match message {
        Some(message) => json!({ "state": state, "message": message }),
        None => json!({ "state": state }),
    };
    {
        let mut current = lc(app).status.lock().unwrap();
        if *current == value {
            return;
        }
        *current = value.clone();
    }
    mainwin::broadcast(app, json!({ "type": "engine-status", "data": value }));
    crate::refresh_tray_engine(app);
    let notice = match state {
        "ready" | "starting" => None,
        _ => message,
    };
    show_notice(app, notice);
    log(app, "status", json!({ "status": value }));
}

/// El aviso dentro de cada ventana principal mientras el motor no está: el
/// renderer no tiene una vista para `engine-status`, y en Electron la bandeja
/// es la única superficie. Un `div` fijo con CSSOM (sin estilos en línea, que
/// la CSP del renderer podría bloquear), que la recarga con el motor de vuelta
/// se lleva.
fn show_notice(app: &tauri::AppHandle, text: Option<&str>) {
    let literal = serde_json::to_string(&text).unwrap_or_else(|_| "null".into());
    let script = format!(
        "(() => {{ const id = 'orgtree-engine-notice'; let el = document.getElementById(id); const text = {literal};
  if (text === null) {{ if (el) el.remove(); return }}
  if (!el) {{ el = document.createElement('div'); el.id = id; el.setAttribute('role', 'alert'); const s = el.style;
    s.position = 'fixed'; s.left = '50%'; s.top = '44px'; s.transform = 'translateX(-50%)'; s.zIndex = '2147483647';
    s.padding = '10px 16px'; s.borderRadius = '8px'; s.background = '#8a2b2b'; s.color = '#fff'; s.maxWidth = '80vw';
    s.font = '13px/1.4 system-ui, sans-serif'; s.boxShadow = '0 4px 18px rgba(0,0,0,.35)';
    (document.body || document.documentElement).appendChild(el) }}
  el.textContent = text }})()"
    );
    for id in mainwin::ids(app) {
        if let Some(window) = app.get_webview_window(&id) {
            let _ = window.eval(&script);
        }
    }
}

/// Lo que muestran la bandeja y su tooltip.
pub fn tray_line(app: &tauri::AppHandle) -> String {
    let status = status(app);
    let restarting = is_restarting(app);
    match status["state"].as_str().unwrap_or("") {
        _ if restarting => "Motor: reiniciando…".into(),
        "ready" => "Motor: listo".into(),
        "starting" => "Motor: iniciando…".into(),
        "stopped" => "Motor: detenido".into(),
        _ => "Motor: no disponible".into(),
    }
}

fn log(app: &tauri::AppHandle, name: &'static str, entry: Value) {
    if let Some(probe) = lc(app).probe.as_ref() {
        probe.log(name, entry);
    }
}

// ------------------------------------------------------- ventana de arranque

/// La ventana de arranque muestra `state` (ver `ui/status.js`).
fn splash(app: &tauri::AppHandle, state: Value) {
    *lc(app).splash.lock().unwrap() = state.clone();
    if let Some(window) = app.get_webview_window("splash") {
        let _ = window.eval(format!("window.orgtreeSplash && window.orgtreeSplash({state})"));
        // Con `--background` la ventana de arranque nace oculta; una conversión
        // o un rechazo se muestran igual (Electron abre su ventana de conversión
        // y su diálogo aunque arranque en la bandeja).
        if matches!(state["state"].as_str(), Some("converting") | Some("failed")) {
            let _ = window.show();
        }
    }
}

/// La ventana de arranque terminó de cargar: reenvía lo último.
pub fn splash_loaded(app: &tauri::AppHandle) {
    let state = lc(app).splash.lock().unwrap().clone();
    if !state.is_null() {
        splash(app, state);
    }
}

/// El mensaje y el detalle de un arranque que no llegó a `ready`.
fn failure(error: &EngineError, options: Option<&EngineOptions>) -> Value {
    let root = options.map(|o| o.data_root.display().to_string()).unwrap_or_default();
    let (title, message, detail) = match error {
        EngineError::RootOwned(reason) => (
            "Otra instancia está usando estos datos",
            format!(
                "Otro motor de Orgtree ya usa la carpeta de datos {root}. Puede ser otra copia de esta app, o un motor \
                 que todavía se está cerrando. Cerrala y tocá Reintentar."
            ),
            reason.clone(),
        ),
        EngineError::ConversionFailed(reason) => (
            "No se pudieron convertir los datos",
            "Orgtree no pudo convertir tus datos al almacenamiento nuevo. Los datos anteriores quedaron como estaban; \
             el motivo y la carpeta del registro están abajo."
                .to_string(),
            reason.clone(),
        ),
        EngineError::Timeout => (
            "El motor no respondió a tiempo",
            "El motor pasó demasiado tiempo sin avisar un avance y se detuvo. Tocá Reintentar; si vuelve a pasar, \
             el registro del motor está en la carpeta diagnostics de los datos."
                .to_string(),
            error.to_string(),
        ),
        EngineError::Config(reason) => ("La configuración del motor no es válida", reason.clone(), String::new()),
        other => ("El motor no pudo arrancar", other.to_string(), String::new()),
    };
    json!({ "state": "failed", "title": title, "message": message, "detail": detail, "retry": true })
}

/// Reintentar desde la ventana de arranque (solo ella, en su página local).
#[tauri::command]
pub fn splash_retry(webview: Webview) -> Result<(), String> {
    authorize_splash(&webview)?;
    let app = webview.app_handle().clone();
    log(&app, "retry", json!({}));
    if shell(&app).engine.lock().unwrap().is_some() {
        return Ok(());
    }
    start_engine(app);
    Ok(())
}

#[tauri::command]
pub fn splash_quit(webview: Webview) -> Result<(), String> {
    authorize_splash(&webview)?;
    note_exit_trigger(webview.app_handle(), "splash");
    mainwin::request_quit(webview.app_handle());
    Ok(())
}

fn authorize_splash(webview: &Webview) -> Result<(), String> {
    let url = webview.url().map_err(|e| e.to_string())?;
    let local = url.scheme() == "tauri" || url.host_str() == Some("tauri.localhost");
    if webview.label() == "splash" && local {
        Ok(())
    } else {
        Err("solo la ventana de arranque".into())
    }
}

// ------------------------------------------------------------------ arranque

/// Plazos de silencio en segundos desde el entorno, para las pruebas (el CI
/// acorta el de conversión para probar que mide el silencio y no el total).
fn env_seconds(name: &str) -> Option<Duration> {
    std::env::var(name).ok()?.trim().parse::<u64>().ok().filter(|s| *s > 0).map(Duration::from_secs)
}

/// Arranca el motor en un hilo y abre la app cuando está listo. Un arranque a
/// la vez; un rechazo deja la ventana de arranque con Reintentar.
pub fn start_engine(app: tauri::AppHandle) {
    if lc(&app).starting.swap(true, Ordering::SeqCst) {
        return;
    }
    let options = match crate::engine_options(&app) {
        Ok((mut options, mode)) => {
            crate::record_launch(&app, &options, mode);
            if let Some(window) = env_seconds("ORGTREE_TAURI_STARTUP_SILENCE_S") {
                options.silence_timeout = window;
            }
            if let Some(window) = env_seconds("ORGTREE_TAURI_CONVERSION_SILENCE_S") {
                options.conversion_timeout = window;
            }
            options
        }
        Err(message) => {
            lc(&app).starting.store(false, Ordering::SeqCst);
            set_status(&app, "unavailable", Some(&message));
            splash(&app, failure(&EngineError::Config(message), None));
            return;
        }
    };
    set_status(&app, "starting", None);
    splash(&app, json!({ "state": "starting", "text": "Iniciando el motor…" }));
    std::thread::Builder::new()
        .name("orgtree-engine-start".into())
        .spawn(move || {
            let began = Instant::now();
            let mut converting = false;
            let mut notify = |event: StartupEvent<'_>| {
                let ms = began.elapsed().as_millis() as u64;
                match event {
                    StartupEvent::Starting => {}
                    StartupEvent::Progress(phase) => {
                        log(&app, "phases", json!({ "phase": phase, "ms": ms }));
                        if converting {
                            converting = false;
                        }
                        splash(&app, json!({ "state": "progress", "text": "Iniciando el motor…", "phase": phase }));
                    }
                    StartupEvent::Converting(phase) => {
                        log(&app, "phases", json!({ "phase": phase, "ms": ms, "conversion": true }));
                        if !converting {
                            converting = true;
                            if let Some(probe) = lc(&app).probe.as_ref() {
                                probe.marker("converting");
                            }
                        }
                        splash(
                            &app,
                            json!({
                                "state": "converting",
                                "text": "Orgtree está convirtiendo tus datos al almacenamiento nuevo. Pasa una sola vez y puede tardar unos minutos.",
                                "phase": conversion_detail(phase),
                            }),
                        );
                    }
                }
            };
            let result = Engine::start_with(&options, &mut notify);
            lc(&app).starting.store(false, Ordering::SeqCst);
            match result {
                Ok(engine) => {
                    if quitting(&app) {
                        let mut engine = engine;
                        let _ = engine.stop_for_quit(QUIT_STOP_BUDGET);
                        return;
                    }
                    let text = format!("Motor listo en {} (pid {}).", engine.origin(), engine.pid().unwrap_or(0));
                    log(&app, "ready", json!({ "pid": engine.pid(), "ms": began.elapsed().as_millis() as u64 }));
                    let opened = crate::open_engine(&app, &engine);
                    *shell(&app).engine.lock().unwrap() = Some(engine);
                    *shell(&app).options.lock().unwrap() = Some(options.clone());
                    set_status(&app, "ready", None);
                    match opened {
                        Ok(()) => {
                            *shell(&app).status.lock().unwrap() = text;
                            watch(app.clone());
                        }
                        Err(error) => splash(
                            &app,
                            json!({ "state": "failed", "title": "No se pudo abrir la interfaz", "message": text, "detail": error, "retry": false }),
                        ),
                    }
                }
                Err(error) => {
                    let message = format!("El motor no arrancó: {error}");
                    set_status(&app, "unavailable", Some(&message));
                    let state = failure(&error, Some(&options));
                    log(&app, "refused", json!({ "error": error.to_string(), "splash": state }));
                    splash(&app, state);
                }
            }
        })
        .expect("no se pudo crear el hilo de arranque del motor");
}

// ------------------------------------------------------- caídas y reinicios

/// Los vigilantes, una sola vez: caídas (cada segundo), cuelgues (`LIVENESS`)
/// y el sondeo de estado y mantenimiento (cada 5 s).
fn watch(app: tauri::AppHandle) {
    if lc(&app).watching.swap(true, Ordering::SeqCst) {
        return;
    }
    let crash = app.clone();
    std::thread::Builder::new()
        .name("orgtree-engine-watch".into())
        .spawn(move || watch_crashes(crash))
        .expect("no se pudo crear el vigilante del motor");
    let hang = app.clone();
    std::thread::Builder::new()
        .name("orgtree-engine-liveness".into())
        .spawn(move || watch_liveness(hang))
        .expect("no se pudo crear el vigilante de cuelgues");
    std::thread::Builder::new()
        .name("orgtree-engine-poll".into())
        .spawn(move || poll_status(app))
        .expect("no se pudo crear el sondeo del motor");
}

/// Recuperación de #5: si el proceso del motor terminó solo, se avisa y se
/// relanza con las mismas opciones, dentro del presupuesto de caídas.
fn watch_crashes(app: tauri::AppHandle) {
    loop {
        std::thread::sleep(Duration::from_secs(1));
        if quitting(&app) {
            return;
        }
        if is_restarting(&app) {
            continue;
        }
        let dead = shell(&app).engine.lock().unwrap().as_mut().is_some_and(|engine| !engine.is_running());
        if !dead {
            continue;
        }
        if lc(&app).restarting.swap(true, Ordering::SeqCst) {
            continue;
        }
        let old = shell(&app).engine.lock().unwrap().take();
        let pid = old.as_ref().and_then(|e| e.pid());
        drop(old);
        #[cfg(debug_assertions)]
        eprintln!("[shell] el motor se cayó; reiniciando");
        set_status(&app, "stopped", Some("El motor de Orgtree se detuvo inesperadamente. Reiniciándolo…"));
        log(&app, "crash", json!({ "pid": pid }));
        if let Some(probe) = lc(&app).probe.as_ref() {
            probe.before_restart();
        }
        if !lc(&app).crashes.lock().unwrap().consider(Instant::now()) {
            set_status(
                &app,
                "unavailable",
                Some("El motor se detuvo varias veces en pocos minutos, así que Orgtree dejó de reiniciarlo. Usá «Reiniciar motor» en la bandeja."),
            );
            lc(&app).restarting.store(false, Ordering::SeqCst);
            crate::refresh_tray_engine(&app);
            continue;
        }
        relaunch(&app, "crash");
    }
}

/// El vigilante de cuelgues de Electron (`watchLiveness`): una sonda cada
/// 30 s; colgado (3 sondas fallidas y 5 min sin respuesta) se termina, se
/// prueba que soltó la raíz y se relanza, a lo sumo 3 veces por hora.
fn watch_liveness(app: tauri::AppHandle) {
    let mut watched: Option<(u32, LivenessWatch)> = None;
    loop {
        std::thread::sleep(LIVENESS.interval);
        if quitting(&app) {
            return;
        }
        if is_restarting(&app) || !is_ready(&app) {
            continue;
        }
        let handle = engine_handle(&app);
        let Some(handle) = handle else { continue };
        if watched.as_ref().map(|(pid, _)| *pid) != Some(handle.pid) {
            watched = Some((handle.pid, LivenessWatch::new(LIVENESS, Instant::now())));
        }
        let error = handle.probe_alive(LIVENESS.probe_timeout);
        let Some((_, watch)) = watched.as_mut() else { continue };
        watch.record(error, Instant::now());
        if !watch.hung(Instant::now()) || quitting(&app) || lc(&app).restarting.swap(true, Ordering::SeqCst) {
            continue;
        }
        // Sigue siendo el mismo motor (no lo reemplazó una caída mientras tanto).
        if engine_handle(&app).map(|h| h.pid) != Some(handle.pid) {
            lc(&app).restarting.store(false, Ordering::SeqCst);
            continue;
        }
        let root = handle.data_root.clone();
        record_liveness(&root, json!({ "event": "hung", "enginePid": handle.pid, "silentSeconds": watch.silent(Instant::now()).as_secs(),
            "failedProbes": watch.failures(), "lastError": watch.last_error() }));
        set_status(&app, "stopped", Some("El motor dejó de responder. Reiniciándolo…"));
        let engine = shell(&app).engine.lock().unwrap().take();
        if let Some(mut engine) = engine {
            engine.kill();
        }
        let released = wait_root_released(&root, orgtree_engine_host::quit_deadlines::RELEASE);
        record_liveness(&root, json!({ "event": "killed", "enginePid": handle.pid, "released": released }));
        watched = None;
        if !released {
            set_status(&app, "unavailable", Some("El motor dejó de responder y se terminó, pero no soltó sus datos: Orgtree no arranca un segundo motor sobre ellos. Salí y volvé a abrir Orgtree."));
            lc(&app).restarting.store(false, Ordering::SeqCst);
            continue;
        }
        if !lc(&app).hangs.lock().unwrap().consider(Instant::now()) {
            record_liveness(&root, json!({ "event": "not-restarted" }));
            set_status(&app, "unavailable", Some("El motor dejó de responder varias veces en una hora, así que Orgtree dejó de reiniciarlo. Usá «Reiniciar motor» en la bandeja."));
            lc(&app).restarting.store(false, Ordering::SeqCst);
            continue;
        }
        relaunch(&app, "hang");
    }
}

/// La línea de `diagnostics/engine-liveness.jsonl`, con los campos del host
/// de arranque y `watcher: "desktop"`, como `recordLiveness`.
fn record_liveness(root: &std::path::Path, mut event: Value) {
    let file = root.join(DIAGNOSTICS_LIVENESS);
    if let Some(dir) = file.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    event["watcher"] = json!("desktop");
    event["at"] = json!(chrono_like_now());
    if let Ok(mut out) = std::fs::OpenOptions::new().create(true).append(true).open(&file) {
        use std::io::Write;
        let _ = writeln!(out, "{event}");
    }
}

/// Segundos desde 1970, sin otra dependencia.
fn chrono_like_now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn engine_handle(app: &tauri::AppHandle) -> Option<EngineHandle> {
    let mut engine = shell(app).engine.lock().unwrap();
    let engine = engine.as_mut()?;
    let pid = engine.pid()?;
    engine.is_running().then(|| engine.handle(pid))
}

/// Relanza el motor con las opciones del arranque. Lo llama quien tomó
/// `restarting`; lo suelta al terminar. Al volver: la cookie nueva (el token
/// cambia en cada arranque), `ready`, y cada ventana principal navega a su ruta,
/// también la que quedó en blanco por una carga fallida (`window-load-recovery`).
fn relaunch(app: &tauri::AppHandle, why: &'static str) -> bool {
    crate::refresh_tray_engine(app);
    let Some(options) = shell(app).options.lock().unwrap().clone() else {
        lc(app).restarting.store(false, Ordering::SeqCst);
        start_engine(app.clone());
        return false;
    };
    let mut last_error = String::new();
    for attempt in 0..RELAUNCH_ATTEMPTS {
        if quitting(app) {
            break;
        }
        match Engine::start(&options) {
            Ok(mut engine) => {
                if quitting(app) {
                    let _ = engine.stop_for_quit(QUIT_STOP_BUDGET);
                    break;
                }
                let same_origin = shell(app).origin.lock().unwrap().as_deref() == Some(engine.origin().as_str());
                let windows: Vec<WebviewWindow> = mainwin::ids(app).iter().filter_map(|id| app.get_webview_window(id)).collect();
                // La cookie es del perfil de WebView2: alcanza con guardarla una vez.
                if let (Some(window), Ok(cookie)) = (windows.first(), crate::desktop_cookie(&engine)) {
                    let _ = window.set_cookie(cookie);
                }
                let pid = engine.pid();
                *shell(app).engine.lock().unwrap() = Some(engine);
                shell(app).restarts.fetch_add(1, Ordering::SeqCst);
                lc(app).restarting.store(false, Ordering::SeqCst);
                set_status(app, "ready", None);
                log(app, "relaunched", json!({ "why": why, "pid": pid, "attempt": attempt, "sameOrigin": same_origin }));
                if same_origin {
                    for window in &windows {
                        reload(app, window);
                    }
                }
                return true;
            }
            Err(error) => {
                #[cfg(debug_assertions)]
                eprintln!("[shell] reinicio {attempt} falló: {error}");
                last_error = error.to_string();
                std::thread::sleep(Duration::from_secs(1));
            }
        }
    }
    lc(app).restarting.store(false, Ordering::SeqCst);
    if !quitting(app) {
        set_status(
            app,
            "unavailable",
            Some(&format!("No se pudo reiniciar el motor ({last_error}). Usá «Reiniciar motor» en la bandeja.")),
        );
    }
    false
}

/// Vuelve a cargar una ventana principal en su ruta. Navegar (no
/// `location.reload()`) sirve también cuando la ventana quedó en la página de
/// error de una carga fallida, donde ya no corre ningún script.
fn reload(app: &tauri::AppHandle, window: &WebviewWindow) {
    let Some(origin) = shell(app).origin.lock().unwrap().clone() else { return };
    let current = window.url().ok().filter(|u| crate::origin_of(u) == origin);
    let target = current.or_else(|| {
        let route = mainwin::route_of(app, window.label())?;
        Url::parse(&format!("{origin}{route}")).ok()
    });
    if let Some(url) = target {
        let _ = window.navigate(url);
    }
}

/// "Reiniciar motor" de la bandeja (`restart()` de Electron): por las buenas,
/// probado, y recién entonces un motor nuevo. Uno a la vez; si el motor nunca
/// arrancó, vuelve a intentar el arranque.
pub fn restart_engine(app: &tauri::AppHandle, why: &'static str) -> bool {
    if quitting(app) || lc(app).starting.load(Ordering::SeqCst) {
        return false;
    }
    if shell(app).options.lock().unwrap().is_none() {
        if shell(app).engine.lock().unwrap().is_none() {
            start_engine(app.clone());
        }
        return false;
    }
    if lc(app).restarting.swap(true, Ordering::SeqCst) {
        return false;
    }
    set_status(app, "stopped", Some("Reiniciando el motor…"));
    let engine = shell(app).engine.lock().unwrap().take();
    if let Some(mut engine) = engine {
        let outcome = engine.stop_for_quit(QUIT_STOP_BUDGET);
        log(app, "restart-stop", json!({ "why": why, "outcome": outcome.as_str() }));
        if outcome == QuitOutcome::Unverified {
            lc(app).restarting.store(false, Ordering::SeqCst);
            set_status(app, "unavailable", Some("El motor no confirmó que se detuvo, y Orgtree no arranca un segundo motor sobre los mismos datos. Probá de nuevo, o salí y volvé a abrir Orgtree."));
            return false;
        }
    }
    if why == "tray" {
        lc(app).crashes.lock().unwrap().reset();
        lc(app).hangs.lock().unwrap().reset();
    }
    relaunch(app, why)
}

// --------------------------------------------------- recuperación de cargas

/// Una ventana principal terminó de cargar. Con el motor caído, la carga
/// falló (WebView2 deja su página de error): se anota, y el relanzamiento la
/// vuelve a navegar. Electron hace lo mismo con su página de espera.
pub fn page_finished(app: &tauri::AppHandle, id: &str) {
    if is_ready(app) {
        lc(app).load_failed.lock().unwrap().remove(id);
        return;
    }
    lc(app).load_failed.lock().unwrap().insert(id.to_string());
    log(app, "load-failed", json!({ "window": id, "status": status(app) }));
}

pub fn load_failed(app: &tauri::AppHandle, id: &str) -> bool {
    lc(app).load_failed.lock().unwrap().contains(id)
}

// ----------------------------------------------------------- mantenimiento

/// `MaintenanceController` de Electron sin su updater: el spike no tiene
/// actualizaciones, así que un pedido `update` se reporta `unavailable` (lo que
/// responde el `check` de Electron sin updater) y no se consume.
#[derive(Default)]
struct Maintenance {
    consumed: HashSet<String>,
    checked: HashMap<String, Instant>,
    failures: HashSet<String>,
}

/// Un pedido válido del motor, como `maintenanceRequest`.
fn maintenance_request(value: &Value) -> Option<(String, String)> {
    let id = value["id"].as_str().filter(|id| !id.is_empty() && id.len() <= 200)?;
    let action = value["action"].as_str().filter(|a| matches!(*a, "restart" | "update"))?;
    value["target"].as_str().filter(|t| matches!(*t, "org" | "mailhub" | "both"))?;
    value["reason"].as_str().filter(|r| r.len() <= 4000)?;
    Some((id.to_string(), action.to_string()))
}

/// Lo reportado: se guarda para `getMaintenanceStatus` y se emite `maintenance`.
fn report_maintenance(app: &tauri::AppHandle, state: &str) {
    let value = json!({ "state": state });
    *lc(app).last_maintenance.lock().unwrap() = Some(value.clone());
    mainwin::broadcast(app, json!({ "type": "maintenance", "data": value }));
    log(app, "maintenance", json!({ "reported": state }));
}

pub fn last_maintenance(app: &tauri::AppHandle) -> Value {
    lc(app).last_maintenance.lock().unwrap().clone().unwrap_or(Value::Null)
}

#[tauri::command]
pub fn desktop_maintenance_status(webview: Webview) -> Result<Value, String> {
    crate::desktop::authorize(&webview)?;
    Ok(last_maintenance(webview.app_handle()))
}

/// El sondeo de `/api/desktop/status` cada 5 s (Electron lo usa para la bandeja
/// y el mantenimiento).
fn poll_status(app: tauri::AppHandle) {
    loop {
        std::thread::sleep(STATUS_POLL);
        if quitting(&app) {
            return;
        }
        if is_restarting(&app) || !is_ready(&app) {
            continue;
        }
        let Some(handle) = engine_handle(&app) else { continue };
        let stats = match handle.request("GET", "/api/desktop/status", None, Duration::from_secs(4)) {
            Ok((200, body)) => serde_json::from_str::<Value>(&body).unwrap_or_default(),
            _ => continue,
        };
        maintenance_tick(&app, &handle, &stats);
        // #23: el updater busca y espera el mismo punto seguro (motor y usuario quietos).
        crate::updater::tick(&app, &stats);
    }
}

fn maintenance_tick(app: &tauri::AppHandle, handle: &EngineHandle, stats: &Value) {
    // Primero, los fallos que el motor todavía no registró.
    let pending: Vec<String> = lc(app).maintenance.lock().unwrap().failures.iter().cloned().collect();
    for id in pending {
        if report_failure(handle, &id) {
            lc(app).maintenance.lock().unwrap().failures.remove(&id);
        }
    }
    let idle = stats["idle"].as_bool() == Some(true);
    let user_idle = user_idle();
    let Some((id, action)) = maintenance_request(&stats["maintenance"]) else { return };
    if lc(app).maintenance.lock().unwrap().consumed.contains(&id) {
        return;
    }
    if !idle || user_idle < USER_IDLE_FOR_MAINTENANCE {
        log(app, "maintenance-wait", json!({ "id": id, "engineIdle": idle, "userIdleS": user_idle.as_secs() }));
        return;
    }
    if action == "update" {
        // Sin updater: el `check` de Electron responde `unavailable`, una vez por minuto.
        let mut maintenance = lc(app).maintenance.lock().unwrap();
        let recent = maintenance.checked.get(&id).is_some_and(|at| at.elapsed() < Duration::from_secs(60));
        if recent {
            return;
        }
        maintenance.checked.insert(id, Instant::now());
        drop(maintenance);
        report_maintenance(app, "unavailable");
        return;
    }
    let body = json!({ "id": id, "outcome": "execute" }).to_string();
    let accepted = match handle.request("POST", "/api/desktop/maintenance/ack", Some(&body), Duration::from_secs(4)) {
        Ok((200, text)) => serde_json::from_str::<Value>(&text).ok().and_then(|v| v["accepted"].as_bool()) == Some(true),
        Ok(_) => false,
        Err(_) => {
            // El motor pudo aceptar un acuse cuya respuesta se perdió: se resuelve
            // como fallido, nunca se adivina que ejecutar es seguro.
            maintenance_failed(app, handle, &id);
            return;
        }
    };
    log(app, "maintenance", json!({ "id": id, "action": action, "accepted": accepted }));
    if !accepted {
        return;
    }
    lc(app).maintenance.lock().unwrap().consumed.insert(id.clone());
    // Todos los objetivos reinician el motor administrado (desktop_maintenance.py).
    // Electron relanza la app entera; acá basta el motor: el shell no tiene estado
    // que refrescar y las ventanas vuelven a cargar.
    if !restart_engine(app, "maintenance") {
        maintenance_failed(app, handle, &id);
    }
}

fn maintenance_failed(app: &tauri::AppHandle, handle: &EngineHandle, id: &str) {
    {
        let mut maintenance = lc(app).maintenance.lock().unwrap();
        maintenance.consumed.insert(id.to_string());
        maintenance.failures.insert(id.to_string());
    }
    report_maintenance(app, "failed");
    if report_failure(handle, id) {
        lc(app).maintenance.lock().unwrap().failures.remove(id);
    }
}

fn report_failure(handle: &EngineHandle, id: &str) -> bool {
    let body = json!({ "id": id }).to_string();
    matches!(handle.request("POST", "/api/desktop/maintenance/failure", Some(&body), Duration::from_secs(4)),
        Ok((200, text)) if serde_json::from_str::<Value>(&text).ok().and_then(|v| v["released"].as_bool()) == Some(true))
}

/// Cuánto hace que el usuario no toca el teclado ni el mouse
/// (`powerMonitor.getSystemIdleTime`).
#[cfg(windows)]
pub(crate) fn user_idle() -> Duration {
    #[repr(C)]
    struct LastInputInfo {
        cb_size: u32,
        dw_time: u32,
    }
    #[link(name = "user32")]
    extern "system" {
        fn GetLastInputInfo(info: *mut LastInputInfo) -> i32;
    }
    #[link(name = "kernel32")]
    extern "system" {
        fn GetTickCount() -> u32;
    }
    let mut info = LastInputInfo { cb_size: std::mem::size_of::<LastInputInfo>() as u32, dw_time: 0 };
    // SAFETY: estructura de tamaño correcto, con cbSize inicializado.
    if unsafe { GetLastInputInfo(&mut info) } == 0 {
        return Duration::ZERO;
    }
    // SAFETY: sin argumentos.
    let now = unsafe { GetTickCount() };
    Duration::from_millis(now.wrapping_sub(info.dw_time) as u64)
}

#[cfg(not(windows))]
pub(crate) fn user_idle() -> Duration {
    // Linux no es destino del spike: se considera quieto.
    Duration::from_secs(3600)
}

// -------------------------------------------------------------------- salida

/// Quién pidió salir, para el registro de la salida.
pub fn note_exit_trigger(app: &tauri::AppHandle, trigger: &'static str) {
    let mut current = lc(app).exit_trigger.lock().unwrap();
    if current.is_none() {
        *current = Some(trigger);
    }
}

/// TODOS los caminos de salida terminan acá: la bandeja, la última ventana con
/// `exitOnClose`, el renderer (`quit`), la ventana de arranque, el fin de la
/// sesión de Windows y el instalador (el Restart Manager manda `WM_ENDSESSION`,
/// que tao convierte en `RunEvent::Exit`). `stop_for_quit` reparte el
/// presupuesto de `QUIT_DEADLINES` y prueba que el árbol soltó la raíz.
pub fn stop_engine_for_exit(app: &tauri::AppHandle, path: &'static str) {
    shell(app).quitting.store(true, Ordering::SeqCst);
    let began = Instant::now();
    let engine = shell(app).engine.lock().unwrap().take();
    let pid = engine.as_ref().and_then(|e| e.pid());
    let outcome = engine.map(|mut engine| engine.stop_for_quit(QUIT_STOP_BUDGET));
    if lc(app).exit_recorded.swap(true, Ordering::SeqCst) && outcome.is_none() {
        return;
    }
    let trigger = *lc(app).exit_trigger.lock().unwrap();
    let entry = json!({
        "path": path,
        "trigger": trigger,
        "enginePid": pid,
        "outcome": outcome.map(QuitOutcome::as_str).unwrap_or("no-engine"),
        "ms": began.elapsed().as_millis() as u64,
        "budgetMs": QUIT_STOP_BUDGET.as_millis() as u64,
    });
    log(app, "exit", entry.clone());
    if let Some(probe) = lc(app).probe.as_ref() {
        probe.flush();
    }
    if let Some(out) = std::env::var_os("ORGTREE_TAURI_EXIT_REPORT").filter(|v| !v.is_empty()) {
        if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(out) {
            use std::io::Write;
            let _ = writeln!(file, "{entry}");
        }
    }
}
