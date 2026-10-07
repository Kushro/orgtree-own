//! Ciclo de vida del motor (#25), como `engine.ts`, `conversion-window.ts`,
//! `maintenance.ts` y `process-failure.ts` de Electron, sobre el supervisor de
//! `engine-host`. Es el mismo diseño que el spike de Tauri (#19,
//! `lifecycle.rs`), con la UI en RSX:
//!
//! - **Arranque**: las fases llegan a la pantalla de arranque de la ventana
//!   principal; las de conversión (`database-convert…`) muestran el mensaje de
//!   la ventana de conversión de Electron y tienen su propio plazo de silencio.
//!   Un rechazo (`root-owned`, `conversion-failed`, un plazo vencido) deja un
//!   mensaje claro con **Reintentar** y **Salir**, en lugar del diálogo fatal
//!   de Electron. Los botones son RSX con handlers en Rust: no hay un puente
//!   que autorizar.
//! - **Estado**: el `EngineStatus` del contrato (`starting`, `ready`,
//!   `stopped`, `unavailable`) en un canal `watch` que cada ventana escucha: un
//!   aviso dentro de cada ventana mientras el motor no está, y la línea de
//!   estado de la bandeja.
//! - **Caídas y cuelgues**: un vigilante relanza el motor con un presupuesto
//!   (3 en 10 minutos, el `RECOVERY_LIMIT` de Electron) y el de cuelgues sigue
//!   las reglas de `LIVENESS`. El token cambia en cada arranque: el cliente
//!   nuevo llega por otro canal con una generación, y cada ventana vuelve a
//!   montar su vista con él (el equivalente a recargar la página).
//! - **Mantenimiento**: el pedido del motor (`/api/desktop/status`) se atiende
//!   con el motor y el usuario quietos, como `MaintenanceController`; lo
//!   reportado queda en `last_maintenance` (el `getMaintenanceStatus` de
//!   Electron) y se avisa en las ventanas.
//! - **Salida**: todos los caminos terminan en `stop_engine_for_exit`, con el
//!   presupuesto de `QUIT_DEADLINES` y la prueba de que el árbol soltó la raíz.

use dioxus::prelude::*;
use orgtree_engine_client::Client;
use orgtree_engine_host::{
    conversion_detail, wait_root_released, write_engine_paths, Engine, EngineError, EngineHandle, EngineOptions, LivenessWatch,
    QuitOutcome, RestartBudget, StartupEvent, LIVENESS, QUIT_STOP_BUDGET,
};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};
use tokio::sync::watch;

/// Reinicios automáticos tras una caída: a lo sumo 3 en 10 minutos, como el
/// presupuesto de recuperación de Electron (`RECOVERY_LIMIT`). Una caída en
/// cada arranque deja de reiniciarse y lo dice.
const CRASH_LIMIT: usize = 3;
const CRASH_WINDOW: Duration = Duration::from_secs(600);
/// Intentos de relanzar tras una caída: ~2 min, porque el puerto guardado
/// puede quedar en `TIME_WAIT` y el guardián puede tardar en soltar la raíz
/// (`root-owned` se reintenta).
const RELAUNCH_ATTEMPTS: u32 = 90;
/// El sondeo de `/api/desktop/status`, como el `poll` de 5 s de Electron.
const STATUS_POLL: Duration = Duration::from_secs(5);
/// El mantenimiento espera a que el usuario no toque nada por 60 s.
const USER_IDLE_FOR_MAINTENANCE: Duration = Duration::from_secs(60);
const DIAGNOSTICS_LIVENESS: &str = "diagnostics/engine-liveness.jsonl";

/// El `EngineStatus` del contrato.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Status {
    pub state: &'static str,
    pub message: Option<String>,
}

impl Status {
    pub fn json(&self) -> Value {
        match &self.message {
            Some(message) => json!({ "state": self.state, "message": message }),
            None => json!({ "state": self.state }),
        }
    }
}

/// Lo que muestra la pantalla de arranque.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Splash {
    Starting { phase: Option<String> },
    Converting { phase: String },
    Failed { title: String, message: String, detail: String, retry: bool },
}

impl Splash {
    pub fn json(&self) -> Value {
        match self {
            Splash::Starting { phase } => json!({ "state": "starting", "phase": phase }),
            Splash::Converting { phase } => json!({ "state": "converting", "phase": phase }),
            Splash::Failed { title, message, detail, retry } => {
                json!({ "state": "failed", "title": title, "message": message, "detail": detail, "retry": retry })
            }
        }
    }
}

/// `MaintenanceController` de Electron sin su updater: el spike no tiene
/// actualizaciones, así que un pedido `update` se reporta `unavailable` (lo que
/// responde el `check` de Electron sin updater) y no se consume.
#[derive(Default)]
struct Maintenance {
    consumed: HashSet<String>,
    checked: HashMap<String, Instant>,
    failures: HashSet<String>,
}

struct Lifecycle {
    status: watch::Sender<Status>,
    splash: watch::Sender<Splash>,
    /// El cliente del motor en uso y su generación (cambia en cada arranque).
    client: watch::Sender<Option<(u64, Client)>>,
    /// `lastMaintenance` de Electron: lo último reportado, o nada.
    maintenance_note: watch::Sender<Option<String>>,
    generation: AtomicU64,
    starting: AtomicBool,
    /// Un relanzamiento o reinicio en curso: uno a la vez, como `restart()`.
    restarting: AtomicBool,
    watching: AtomicBool,
    quitting: AtomicBool,
    crashes: Mutex<RestartBudget>,
    hangs: Mutex<RestartBudget>,
    /// Las opciones del último arranque bueno, para relanzar.
    options: Mutex<Option<EngineOptions>>,
    maintenance: Mutex<Maintenance>,
    /// Qué pidió la salida (bandeja, última ventana, arranque, prueba).
    exit_trigger: Mutex<Option<&'static str>>,
    exit_recorded: AtomicBool,
}

fn lc() -> &'static Lifecycle {
    static LIFECYCLE: OnceLock<Lifecycle> = OnceLock::new();
    LIFECYCLE.get_or_init(|| Lifecycle {
        status: watch::Sender::new(Status { state: "starting", message: None }),
        splash: watch::Sender::new(Splash::Starting { phase: None }),
        client: watch::Sender::new(None),
        maintenance_note: watch::Sender::new(None),
        generation: AtomicU64::new(0),
        starting: AtomicBool::new(false),
        restarting: AtomicBool::new(false),
        watching: AtomicBool::new(false),
        quitting: AtomicBool::new(false),
        crashes: Mutex::new(RestartBudget::new(CRASH_LIMIT, CRASH_WINDOW)),
        hangs: Mutex::new(RestartBudget::new(LIVENESS.restart_limit, LIVENESS.restart_window)),
        options: Mutex::new(None),
        maintenance: Mutex::new(Maintenance::default()),
        exit_trigger: Mutex::new(None),
        exit_recorded: AtomicBool::new(false),
    })
}

// ------------------------------------------------------------------ estado

pub fn status() -> Status {
    lc().status.borrow().clone()
}

pub fn subscribe_status() -> watch::Receiver<Status> {
    lc().status.subscribe()
}

pub fn splash() -> Splash {
    lc().splash.borrow().clone()
}

pub fn subscribe_splash() -> watch::Receiver<Splash> {
    lc().splash.subscribe()
}

pub fn subscribe_client() -> watch::Receiver<Option<(u64, Client)>> {
    lc().client.subscribe()
}

/// El cliente del motor en uso (el de la última generación).
pub fn current_client() -> Option<Client> {
    lc().client.borrow().as_ref().map(|(_, client)| client.clone())
}

pub fn generation() -> u64 {
    lc().client.borrow().as_ref().map(|(g, _)| *g).unwrap_or(0)
}

/// Lo último que reportó el mantenimiento (`getMaintenanceStatus`), o nada.
pub fn last_maintenance() -> Option<String> {
    lc().maintenance_note.borrow().clone()
}

pub fn is_ready() -> bool {
    status().state == "ready"
}

pub fn is_restarting() -> bool {
    lc().restarting.load(Ordering::SeqCst)
}

pub fn quitting() -> bool {
    lc().quitting.load(Ordering::SeqCst)
}

/// La salida empezó: los vigilantes paran y nada se relanza.
pub fn begin_quit() -> bool {
    !lc().quitting.swap(true, Ordering::SeqCst)
}

/// Cambia el estado del motor: lo guarda y lo avisa a cada ventana (el aviso
/// dentro de la ventana y la bandeja escuchan el mismo canal).
fn set_status(state: &'static str, message: Option<&str>) {
    let value = Status { state, message: message.map(str::to_string) };
    if *lc().status.borrow() == value {
        return;
    }
    log("status", value.json());
    lc().status.send_replace(value);
}

fn set_splash(value: Splash) {
    lc().splash.send_replace(value);
}

fn log(name: &'static str, entry: Value) {
    crate::probe::log(name, entry);
}

/// Lo que muestra la bandeja y su tooltip.
pub fn tray_line() -> String {
    let status = status();
    match status.state {
        _ if is_restarting() => "Motor: reiniciando…".into(),
        "ready" => "Motor: listo".into(),
        "starting" => "Motor: iniciando…".into(),
        "stopped" => "Motor: detenido".into(),
        _ => "Motor: no disponible".into(),
    }
}

// ------------------------------------------------------- pantalla de arranque

/// El mensaje y el detalle de un arranque que no llegó a `ready`.
fn failure(error: &EngineError, options: Option<&EngineOptions>) -> Splash {
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
    Splash::Failed { title: title.into(), message, detail, retry: true }
}

/// La pantalla de arranque de la ventana principal: las fases, la conversión
/// y, si el arranque falla, el motivo con Reintentar y Salir.
#[component]
pub fn SplashView() -> Element {
    let mut state = use_signal(splash);
    use_future(move || async move {
        let mut changes = subscribe_splash();
        loop {
            let next = changes.borrow_and_update().clone();
            if *state.peek() != next {
                state.set(next);
            }
            if changes.changed().await.is_err() {
                break;
            }
        }
    });
    let body = match state() {
        Splash::Starting { phase } => rsx! {
            p { id: "status", class: "dx-splash-status", "Iniciando el motor…" }
            if let Some(phase) = phase {
                p { class: "dim dx-splash-phase", "{phase}" }
            }
        },
        Splash::Converting { phase } => rsx! {
            // El texto de la ventana de conversión de Electron (`conversionPage`).
            p { id: "status", class: "dx-splash-status dx-converting",
                "Orgtree está convirtiendo tus datos al almacenamiento nuevo. Pasa una sola vez y puede tardar unos minutos."
            }
            p { class: "dim dx-splash-phase", "{phase}" }
        },
        Splash::Failed { title, message, detail, retry } => rsx! {
            div { class: "dx-splash-failure", role: "alert",
                h2 { "{title}" }
                p { id: "status", "{message}" }
                if !detail.is_empty() {
                    pre { class: "dx-splash-detail", "{detail}" }
                }
                div { class: "row",
                    if retry {
                        button { class: "solid dx-splash-retry", onclick: move |_| retry_from_splash(), "Reintentar" }
                    }
                    button { class: "dx-splash-quit", onclick: move |_| crate::orgwindows::quit("splash"), "Salir" }
                }
            }
        },
    };
    rsx! {
        main { class: "dx-splash",
            header { class: "dx-splash-bar", crate::native::WindowControls {} }
            h1 { "Orgtree" }
            {body}
            crate::DataRoot {}
        }
    }
}

/// Reintentar desde la pantalla de arranque.
pub fn retry_from_splash() {
    log("retry", json!({}));
    if crate::ENGINE.lock().unwrap().is_some() {
        return;
    }
    start_engine();
}

// ------------------------------------------------------------------ arranque

/// Plazos de silencio en segundos desde el entorno, para las pruebas (el CI
/// acorta el de conversión para probar que mide el silencio y no el total).
fn env_seconds(name: &str) -> Option<Duration> {
    std::env::var(name).ok()?.trim().parse::<u64>().ok().filter(|s| *s > 0).map(Duration::from_secs)
}

/// Las opciones de esta ejecución, con los plazos de la prueba si los hay.
fn engine_options() -> Result<(EngineOptions, Option<std::path::PathBuf>), String> {
    let launch = crate::launch().as_ref().map_err(Clone::clone)?;
    let mut options = launch.options.clone();
    if let Some(window) = env_seconds("ORGTREE_DIOXUS_STARTUP_SILENCE_S") {
        options.silence_timeout = window;
    }
    if let Some(window) = env_seconds("ORGTREE_DIOXUS_CONVERSION_SILENCE_S") {
        options.conversion_timeout = window;
    }
    Ok((options, launch.descriptor.clone()))
}

/// Publica un motor listo: el motor, su cliente (con una generación nueva) y
/// el estado `ready`.
fn install(engine: Engine) -> Result<Option<u32>, String> {
    let client = Client::new(engine.origin(), engine.token().expose()).map_err(|e| format!("Cliente del motor: {e}"))?;
    let pid = engine.pid();
    *crate::ENGINE.lock().unwrap() = Some(engine);
    let generation = lc().generation.fetch_add(1, Ordering::SeqCst) + 1;
    lc().client.send_replace(Some((generation, client)));
    Ok(pid)
}

/// Arranca el motor en un hilo. Un arranque a la vez; un rechazo deja la
/// pantalla de arranque con Reintentar.
pub fn start_engine() {
    if lc().starting.swap(true, Ordering::SeqCst) {
        return;
    }
    let (options, descriptor) = match engine_options() {
        Ok(found) => found,
        Err(message) => {
            lc().starting.store(false, Ordering::SeqCst);
            set_status("unavailable", Some(&message));
            set_splash(failure(&EngineError::Config(message), None));
            return;
        }
    };
    set_status("starting", None);
    set_splash(Splash::Starting { phase: None });
    std::thread::Builder::new()
        .name("orgtree-engine-start".into())
        .spawn(move || {
            if let Some(descriptor) = &descriptor {
                if let Err(error) = write_engine_paths(descriptor, &options) {
                    lc().starting.store(false, Ordering::SeqCst);
                    let message = format!("El motor no arrancó: {error}");
                    set_status("unavailable", Some(&message));
                    set_splash(failure(&error, Some(&options)));
                    return;
                }
            }
            let began = Instant::now();
            let mut notify = |event: StartupEvent<'_>| {
                let ms = began.elapsed().as_millis() as u64;
                match event {
                    StartupEvent::Starting => {}
                    StartupEvent::Progress(phase) => {
                        log("phases", json!({ "phase": phase, "ms": ms }));
                        set_splash(Splash::Starting { phase: Some(phase.to_string()) });
                    }
                    StartupEvent::Converting(phase) => {
                        log("phases", json!({ "phase": phase, "ms": ms, "conversion": true }));
                        set_splash(Splash::Converting { phase: conversion_detail(phase).to_string() });
                    }
                }
            };
            let result = Engine::start_with(&options, &mut notify);
            lc().starting.store(false, Ordering::SeqCst);
            match result {
                Ok(mut engine) => {
                    if quitting() {
                        let _ = engine.stop_for_quit(QUIT_STOP_BUDGET);
                        return;
                    }
                    let ms = began.elapsed().as_millis() as u64;
                    match install(engine) {
                        Ok(pid) => {
                            *lc().options.lock().unwrap() = Some(options);
                            log("ready", json!({ "pid": pid, "ms": ms }));
                            set_status("ready", None);
                            watch_engine();
                        }
                        Err(message) => {
                            set_status("unavailable", Some(&message));
                            set_splash(Splash::Failed {
                                title: "No se pudo abrir la interfaz".into(),
                                message,
                                detail: String::new(),
                                retry: false,
                            });
                        }
                    }
                }
                Err(error) => {
                    let message = format!("El motor no arrancó: {error}");
                    set_status("unavailable", Some(&message));
                    let state = failure(&error, Some(&options));
                    log("refused", json!({ "error": error.to_string(), "splash": state.json() }));
                    set_splash(state);
                }
            }
        })
        .expect("no se pudo crear el hilo de arranque del motor");
}

// ------------------------------------------------------- caídas y reinicios

/// Los vigilantes, una sola vez: caídas (cada segundo), cuelgues (`LIVENESS`)
/// y el sondeo de estado y mantenimiento (cada 5 s).
fn watch_engine() {
    if lc().watching.swap(true, Ordering::SeqCst) {
        return;
    }
    let spawn = |name: &str, body: fn()| {
        std::thread::Builder::new().name(name.into()).spawn(body).expect("no se pudo crear un vigilante del motor");
    };
    spawn("orgtree-engine-watch", watch_crashes);
    spawn("orgtree-engine-liveness", watch_liveness);
    spawn("orgtree-engine-poll", poll_status);
}

fn engine_pid() -> Option<u32> {
    crate::ENGINE.lock().unwrap().as_ref().and_then(Engine::pid)
}

/// El PID del motor en uso (para la prueba).
pub fn pid() -> Option<u32> {
    engine_pid()
}

/// Si el proceso del motor terminó solo, se avisa y se relanza con las mismas
/// opciones, dentro del presupuesto de caídas.
fn watch_crashes() {
    loop {
        std::thread::sleep(Duration::from_secs(1));
        if quitting() {
            return;
        }
        if is_restarting() {
            continue;
        }
        let dead = crate::ENGINE.lock().unwrap().as_mut().is_some_and(|engine| !engine.is_running());
        if !dead || lc().restarting.swap(true, Ordering::SeqCst) {
            continue;
        }
        let old = crate::ENGINE.lock().unwrap().take();
        let pid = old.as_ref().and_then(Engine::pid);
        drop(old);
        set_status("stopped", Some("El motor de Orgtree se detuvo inesperadamente. Reiniciándolo…"));
        log("crash", json!({ "pid": pid }));
        if !lc().crashes.lock().unwrap().consider(Instant::now()) {
            lc().restarting.store(false, Ordering::SeqCst);
            set_status(
                "unavailable",
                Some("El motor se detuvo varias veces en pocos minutos, así que Orgtree dejó de reiniciarlo. Usá «Reiniciar motor» en la bandeja."),
            );
            continue;
        }
        relaunch("crash");
    }
}

fn engine_handle() -> Option<EngineHandle> {
    let mut engine = crate::ENGINE.lock().unwrap();
    let engine = engine.as_mut()?;
    let pid = engine.pid()?;
    engine.is_running().then(|| engine.handle(pid))
}

/// El vigilante de cuelgues de Electron (`watchLiveness`): una sonda cada
/// 30 s; colgado (3 sondas fallidas y 5 min sin respuesta) se termina, se
/// prueba que soltó la raíz y se relanza, a lo sumo 3 veces por hora.
fn watch_liveness() {
    let mut watched: Option<(u32, LivenessWatch)> = None;
    loop {
        std::thread::sleep(LIVENESS.interval);
        if quitting() {
            return;
        }
        if is_restarting() || !is_ready() {
            continue;
        }
        let Some(handle) = engine_handle() else { continue };
        if watched.as_ref().map(|(pid, _)| *pid) != Some(handle.pid) {
            watched = Some((handle.pid, LivenessWatch::new(LIVENESS, Instant::now())));
        }
        let error = handle.probe_alive(LIVENESS.probe_timeout);
        let Some((_, watch)) = watched.as_mut() else { continue };
        watch.record(error, Instant::now());
        if !watch.hung(Instant::now()) || quitting() || lc().restarting.swap(true, Ordering::SeqCst) {
            continue;
        }
        // Sigue siendo el mismo motor (no lo reemplazó una caída mientras tanto).
        if engine_handle().map(|h| h.pid) != Some(handle.pid) {
            lc().restarting.store(false, Ordering::SeqCst);
            continue;
        }
        let root = handle.data_root.clone();
        record_liveness(
            &root,
            json!({ "event": "hung", "enginePid": handle.pid, "silentSeconds": watch.silent(Instant::now()).as_secs(),
                "failedProbes": watch.failures(), "lastError": watch.last_error() }),
        );
        set_status("stopped", Some("El motor dejó de responder. Reiniciándolo…"));
        let engine = crate::ENGINE.lock().unwrap().take();
        if let Some(mut engine) = engine {
            engine.kill();
        }
        let released = wait_root_released(&root, orgtree_engine_host::quit_deadlines::RELEASE);
        record_liveness(&root, json!({ "event": "killed", "enginePid": handle.pid, "released": released }));
        watched = None;
        if !released {
            lc().restarting.store(false, Ordering::SeqCst);
            set_status("unavailable", Some("El motor dejó de responder y se terminó, pero no soltó sus datos: Orgtree no arranca un segundo motor sobre ellos. Salí y volvé a abrir Orgtree."));
            continue;
        }
        if !lc().hangs.lock().unwrap().consider(Instant::now()) {
            record_liveness(&root, json!({ "event": "not-restarted" }));
            lc().restarting.store(false, Ordering::SeqCst);
            set_status("unavailable", Some("El motor dejó de responder varias veces en una hora, así que Orgtree dejó de reiniciarlo. Usá «Reiniciar motor» en la bandeja."));
            continue;
        }
        relaunch("hang");
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
    event["at"] = json!(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0));
    if let Ok(mut out) = std::fs::OpenOptions::new().create(true).append(true).open(&file) {
        use std::io::Write;
        let _ = writeln!(out, "{event}");
    }
}

/// Relanza el motor con las opciones del arranque. Lo llama quien tomó
/// `restarting`; lo suelta al terminar. Al volver hay un cliente nuevo (el
/// token cambia en cada arranque) y cada ventana vuelve a montar su vista.
fn relaunch(why: &'static str) -> bool {
    let Some(options) = lc().options.lock().unwrap().clone() else {
        lc().restarting.store(false, Ordering::SeqCst);
        start_engine();
        return false;
    };
    let mut last_error = String::new();
    for attempt in 0..RELAUNCH_ATTEMPTS {
        if quitting() {
            break;
        }
        match Engine::start(&options) {
            Ok(mut engine) => {
                if quitting() {
                    let _ = engine.stop_for_quit(QUIT_STOP_BUDGET);
                    break;
                }
                match install(engine) {
                    Ok(pid) => {
                        lc().restarting.store(false, Ordering::SeqCst);
                        set_status("ready", None);
                        log("relaunched", json!({ "why": why, "pid": pid, "attempt": attempt }));
                        return true;
                    }
                    Err(error) => last_error = error,
                }
            }
            Err(error) => {
                last_error = error.to_string();
                std::thread::sleep(Duration::from_secs(1));
            }
        }
    }
    lc().restarting.store(false, Ordering::SeqCst);
    if !quitting() {
        set_status(
            "unavailable",
            Some(&format!("No se pudo reiniciar el motor ({last_error}). Usá «Reiniciar motor» en la bandeja.")),
        );
    }
    false
}

/// "Reiniciar motor" de la bandeja (`restart()` de Electron): por las buenas,
/// probado, y recién entonces un motor nuevo. Uno a la vez; si el motor nunca
/// arrancó, vuelve a intentar el arranque. Bloquea: llamarlo fuera del hilo de UI.
pub fn restart_engine(why: &'static str) -> bool {
    if quitting() || lc().starting.load(Ordering::SeqCst) {
        return false;
    }
    if lc().options.lock().unwrap().is_none() {
        if crate::ENGINE.lock().unwrap().is_none() {
            start_engine();
        }
        return false;
    }
    if lc().restarting.swap(true, Ordering::SeqCst) {
        return false;
    }
    set_status("stopped", Some("Reiniciando el motor…"));
    let engine = crate::ENGINE.lock().unwrap().take();
    if let Some(mut engine) = engine {
        let outcome = engine.stop_for_quit(QUIT_STOP_BUDGET);
        log("restart-stop", json!({ "why": why, "outcome": outcome.as_str() }));
        if outcome == QuitOutcome::Unverified {
            lc().restarting.store(false, Ordering::SeqCst);
            set_status("unavailable", Some("El motor no confirmó que se detuvo, y Orgtree no arranca un segundo motor sobre los mismos datos. Probá de nuevo, o salí y volvé a abrir Orgtree."));
            return false;
        }
    }
    if why == "tray" {
        lc().crashes.lock().unwrap().reset();
        lc().hangs.lock().unwrap().reset();
    }
    relaunch(why)
}

/// "Reiniciar motor" desde la bandeja, en un hilo (el reinicio bloquea).
pub fn restart_from_tray() {
    let _ = std::thread::Builder::new().name("orgtree-engine-restart".into()).spawn(|| {
        restart_engine("tray");
    });
}

// ----------------------------------------------------------- mantenimiento

/// Un pedido válido del motor, como `maintenanceRequest`.
fn maintenance_request(value: &Value) -> Option<(String, String)> {
    let id = value["id"].as_str().filter(|id| !id.is_empty() && id.len() <= 200)?;
    let action = value["action"].as_str().filter(|a| matches!(*a, "restart" | "update"))?;
    value["target"].as_str().filter(|t| matches!(*t, "org" | "mailhub" | "both"))?;
    value["reason"].as_str().filter(|r| r.len() <= 4000)?;
    Some((id.to_string(), action.to_string()))
}

/// Lo reportado: queda para `last_maintenance` y se avisa en las ventanas.
fn report_maintenance(state: &str) {
    lc().maintenance_note.send_replace(Some(state.to_string()));
    log("maintenance", json!({ "reported": state }));
}

pub fn subscribe_maintenance() -> watch::Receiver<Option<String>> {
    lc().maintenance_note.subscribe()
}

/// El sondeo de `/api/desktop/status` cada 5 s (Electron lo usa para la bandeja
/// y el mantenimiento).
fn poll_status() {
    loop {
        std::thread::sleep(STATUS_POLL);
        if quitting() {
            return;
        }
        if is_restarting() || !is_ready() {
            continue;
        }
        let Some(handle) = engine_handle() else { continue };
        let stats = match handle.request("GET", "/api/desktop/status", None, Duration::from_secs(4)) {
            Ok((200, body)) => serde_json::from_str::<Value>(&body).unwrap_or_default(),
            _ => continue,
        };
        maintenance_tick(&handle, &stats);
    }
}

fn maintenance_tick(handle: &EngineHandle, stats: &Value) {
    // Primero, los fallos que el motor todavía no registró.
    let pending: Vec<String> = lc().maintenance.lock().unwrap().failures.iter().cloned().collect();
    for id in pending {
        if report_failure(handle, &id) {
            lc().maintenance.lock().unwrap().failures.remove(&id);
        }
    }
    let idle = stats["idle"].as_bool() == Some(true);
    let user_idle = user_idle();
    let Some((id, action)) = maintenance_request(&stats["maintenance"]) else { return };
    if lc().maintenance.lock().unwrap().consumed.contains(&id) {
        return;
    }
    if !idle || user_idle < USER_IDLE_FOR_MAINTENANCE {
        log("maintenance-wait", json!({ "id": id, "engineIdle": idle, "userIdleS": user_idle.as_secs() }));
        return;
    }
    if action == "update" {
        // Sin updater: el `check` de Electron responde `unavailable`, una vez por minuto.
        let mut maintenance = lc().maintenance.lock().unwrap();
        if maintenance.checked.get(&id).is_some_and(|at| at.elapsed() < Duration::from_secs(60)) {
            return;
        }
        maintenance.checked.insert(id, Instant::now());
        drop(maintenance);
        report_maintenance("unavailable");
        return;
    }
    let body = json!({ "id": id, "outcome": "execute" }).to_string();
    let accepted = match handle.request("POST", "/api/desktop/maintenance/ack", Some(&body), Duration::from_secs(4)) {
        Ok((200, text)) => serde_json::from_str::<Value>(&text).ok().and_then(|v| v["accepted"].as_bool()) == Some(true),
        Ok(_) => false,
        Err(_) => {
            // El motor pudo aceptar un acuse cuya respuesta se perdió: se resuelve
            // como fallido, nunca se adivina que ejecutar es seguro.
            maintenance_failed(handle, &id);
            return;
        }
    };
    log("maintenance", json!({ "id": id, "action": action, "accepted": accepted }));
    if !accepted {
        return;
    }
    lc().maintenance.lock().unwrap().consumed.insert(id.clone());
    // Todos los objetivos reinician el motor administrado (desktop_maintenance.py).
    // Electron relanza la app entera; acá basta el motor: las ventanas vuelven a
    // montar su vista con el cliente nuevo.
    if !restart_engine("maintenance") {
        maintenance_failed(handle, &id);
    }
}

fn maintenance_failed(handle: &EngineHandle, id: &str) {
    {
        let mut maintenance = lc().maintenance.lock().unwrap();
        maintenance.consumed.insert(id.to_string());
        maintenance.failures.insert(id.to_string());
    }
    report_maintenance("failed");
    if report_failure(handle, id) {
        lc().maintenance.lock().unwrap().failures.remove(id);
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
fn user_idle() -> Duration {
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
fn user_idle() -> Duration {
    // Linux no es destino del spike: se considera quieto.
    Duration::from_secs(3600)
}

// --------------------------------------------------------------- en la UI

/// El cliente del motor de esta ventana y su generación. Cada ventana copia el
/// valor del canal (las señales no se comparten entre VirtualDom) y vuelve a
/// montar su vista cuando cambia la generación.
/// `fallback` es el cliente que tenía quien abrió la ventana, si el canal todavía no tiene uno.
pub fn use_engine_client(fallback: Option<Client>) -> (Signal<Option<Client>>, Signal<u64>) {
    let mut client = use_signal(move || current_client().or(fallback));
    let mut generation = use_signal(generation);
    use_future(move || async move {
        let mut changes = subscribe_client();
        loop {
            let next = changes.borrow_and_update().clone();
            if let Some((g, c)) = next {
                if *generation.peek() != g {
                    client.set(Some(c));
                    generation.set(g);
                }
            }
            if changes.changed().await.is_err() {
                break;
            }
        }
    });
    (client, generation)
}

/// El aviso dentro de cada ventana mientras el motor no está (en Electron la
/// bandeja es la única superficie), y lo que reportó el mantenimiento.
#[component]
pub fn EngineNotice() -> Element {
    let mut state = use_signal(status);
    let mut maintenance = use_signal(last_maintenance);
    use_future(move || async move {
        let mut changes = subscribe_status();
        loop {
            let next = changes.borrow_and_update().clone();
            if *state.peek() != next {
                state.set(next);
            }
            if changes.changed().await.is_err() {
                break;
            }
        }
    });
    use_future(move || async move {
        let mut changes = subscribe_maintenance();
        loop {
            let next = changes.borrow_and_update().clone();
            if *maintenance.peek() != next {
                maintenance.set(next);
            }
            if changes.changed().await.is_err() {
                break;
            }
        }
    });
    let current = state();
    let down = matches!(current.state, "stopped" | "unavailable");
    let note = match maintenance().as_deref() {
        Some("unavailable") => Some("Hay un pedido de actualización del motor, pero esta versión no se actualiza sola."),
        Some("failed") => Some("El mantenimiento pedido por el motor no se pudo completar."),
        _ => None,
    };
    rsx! {
        if down {
            div { class: "dx-engine-notice", role: "alert", "data-state": "{current.state}",
                {current.message.clone().unwrap_or_else(|| "El motor de Orgtree no está disponible.".into())}
            }
        }
        if let Some(note) = note {
            div { class: "dx-maintenance-notice", role: "status", "data-state": "{maintenance().unwrap_or_default()}", "{note}" }
        }
    }
}

// -------------------------------------------------------------------- salida

/// Quién pidió salir, para el registro de la salida.
pub fn note_exit_trigger(trigger: &'static str) {
    let mut current = lc().exit_trigger.lock().unwrap();
    if current.is_none() {
        *current = Some(trigger);
    }
}

/// TODOS los caminos de salida terminan acá, en `LoopDestroyed`: la bandeja, la
/// última ventana con `exitOnClose`, la pantalla de arranque, la prueba y el
/// fin de la sesión de Windows (tao atiende `WM_ENDSESSION` terminando el loop).
/// `stop_for_quit` reparte el presupuesto de `QUIT_DEADLINES` y prueba que el
/// árbol soltó la raíz.
pub fn stop_engine_for_exit(path: &'static str) {
    lc().quitting.store(true, Ordering::SeqCst);
    let began = Instant::now();
    let engine = crate::ENGINE.lock().unwrap().take();
    let pid = engine.as_ref().and_then(Engine::pid);
    let outcome = engine.map(|mut engine| engine.stop_for_quit(QUIT_STOP_BUDGET));
    if lc().exit_recorded.swap(true, Ordering::SeqCst) && outcome.is_none() {
        return;
    }
    let trigger = *lc().exit_trigger.lock().unwrap();
    let entry = json!({
        "path": path,
        "trigger": trigger,
        "enginePid": pid,
        "outcome": outcome.map(QuitOutcome::as_str).unwrap_or("no-engine"),
        "ms": began.elapsed().as_millis() as u64,
        "budgetMs": QUIT_STOP_BUDGET.as_millis() as u64,
    });
    log("exit", entry.clone());
    if let Some(out) = std::env::var_os("ORGTREE_DIOXUS_EXIT_REPORT").filter(|v| !v.is_empty()) {
        if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(out) {
            use std::io::Write;
            let _ = writeln!(file, "{entry}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pedidos_de_mantenimiento_validos() {
        let ok = json!({ "id": "a1", "action": "restart", "target": "org", "reason": "r" });
        assert_eq!(maintenance_request(&ok), Some(("a1".into(), "restart".into())));
        assert_eq!(maintenance_request(&json!({ "id": "a1", "action": "explode", "target": "org", "reason": "r" })), None);
        assert_eq!(maintenance_request(&json!({ "id": "", "action": "update", "target": "org", "reason": "r" })), None);
        assert_eq!(maintenance_request(&json!({ "id": "a1", "action": "update", "target": "disk", "reason": "r" })), None);
        assert_eq!(maintenance_request(&Value::Null), None);
    }

    #[test]
    fn rechazos_con_mensaje_claro() {
        let options = EngineOptions::new("/usr/bin/python3", "/engine", "/datos/raiz");
        match failure(&EngineError::RootOwned("root-owned".into()), Some(&options)) {
            Splash::Failed { title, message, retry, .. } => {
                assert_eq!(title, "Otra instancia está usando estos datos");
                assert!(message.contains("/datos/raiz") && retry);
            }
            other => panic!("{other:?}"),
        }
        assert!(matches!(failure(&EngineError::ConversionFailed("x".into()), None), Splash::Failed { detail, .. } if detail == "x"));
        assert!(matches!(failure(&EngineError::Timeout, None), Splash::Failed { title, .. } if title.contains("a tiempo")));
    }
}
