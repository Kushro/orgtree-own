// Release builds run without a console window on Windows.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod desktop;
mod probe;

use orgtree_engine_host::{Engine, EngineOptions};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Mutex;
use tauri::webview::{Cookie, NewWindowFeatures, NewWindowResponse, PageLoadEvent};
use tauri::{Manager, RunEvent, Url, WebviewUrl, WebviewWindow, WebviewWindowBuilder, WindowEvent};

/// Nombre de la cookie que `TokenGate` (engine/launch.py) acepta en lugar del header.
const DESKTOP_COOKIE: &str = "orgtree_desktop_token";

/// Estado del shell. El token nunca pasa por acá: va del motor a la cookie.
struct Shell {
    engine: Mutex<Option<Engine>>,
    /// Origen exacto del motor una vez listo; la única URL remota permitida.
    origin: Mutex<Option<String>>,
    status: Mutex<String>,
    preferences: Mutex<serde_json::Value>,
    probe: Mutex<Option<probe::Probe>>,
    /// Opciones del arranque, para reiniciar el motor si se cae.
    options: Mutex<Option<EngineOptions>>,
    restarts: AtomicU32,
    quitting: AtomicBool,
}

impl Default for Shell {
    fn default() -> Self {
        Shell {
            engine: Mutex::new(None),
            origin: Mutex::new(None),
            status: Mutex::new(String::new()),
            preferences: Mutex::new(desktop::default_preferences()),
            probe: Mutex::new(None),
            options: Mutex::new(None),
            restarts: AtomicU32::new(0),
            quitting: AtomicBool::new(false),
        }
    }
}

/// Configuración del spike. El empaquetado del runtime de Python queda fuera
/// del recorte, así que el intérprete se indica por entorno.
fn engine_options(app: &tauri::AppHandle) -> Result<EngineOptions, String> {
    let python = std::env::var_os("ORGTREE_TAURI_PYTHON")
        .map(PathBuf::from)
        .ok_or("Falta ORGTREE_TAURI_PYTHON: la ruta absoluta de un Python con las dependencias del motor.")?;
    let engine_dir = std::env::var_os("ORGTREE_TAURI_ENGINE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../../../engine")));
    // Nunca la raíz real de Orgtree: por defecto, una carpeta del identificador del spike.
    let data_root = match std::env::var_os("ORGTREE_TAURI_DATA") {
        Some(root) => PathBuf::from(root),
        None => app.path().app_local_data_dir().map_err(|e| e.to_string())?.join("data"),
    };
    let mut options = EngineOptions::new(python, engine_dir, data_root);
    // Build del renderer que sirve el motor (`dist/renderer`); sin él, `/` da 404.
    options.ui_dir = std::env::var_os("ORGTREE_TAURI_UI_DIR").map(PathBuf::from);
    Ok(options)
}

/// La ventana de la app sobre el origen del motor (existe desde que el motor está listo).
pub(crate) fn main_window(app: &tauri::AppHandle) -> Option<WebviewWindow> {
    app.get_webview_window("main")
}

/// La ventana local de arranque, hasta que el motor está listo.
fn splash_window(app: &tauri::AppHandle) -> Option<WebviewWindow> {
    app.get_webview_window("splash")
}

fn show_status(app: &tauri::AppHandle, text: &str) {
    *app.state::<Shell>().status.lock().unwrap() = text.to_string();
    if let Some(window) = splash_window(app) {
        let literal = serde_json::to_string(text).unwrap_or_default();
        let _ = window.eval(format!("window.orgtreeEngineStatus && window.orgtreeEngineStatus({literal})"));
    }
}

/// Entrega un `DesktopEvent` a los listeners de `orgtreeDesktop.onEvent`.
pub(crate) fn dispatch_event(app: &tauri::AppHandle, event: serde_json::Value) {
    if let Some(window) = main_window(app) {
        let _ = window.eval(format!("window.__orgtreeDesktopDispatch && window.__orgtreeDesktopDispatch({event})"));
    }
}

/// La cookie que reemplaza a `onBeforeSendHeaders` de Electron: WebView2 no deja
/// agregar headers al handshake del WebSocket (WebView2Feedback#4303).
/// `Domain` sin punto inicial es host-only en WebView2; `HttpOnly` la oculta del
/// JavaScript de la página; sin `Expires` muere con el proceso del webview.
fn desktop_cookie(engine: &Engine) -> Result<Cookie<'static>, String> {
    let text = format!(
        "{DESKTOP_COOKIE}={}; Domain=127.0.0.1; Path=/; HttpOnly; SameSite=Strict",
        engine.token().expose()
    );
    Cookie::parse(text).map_err(|_| "no se pudo construir la cookie del motor".to_string())
}

/// Abre la app sobre el motor. Corre en el hilo de arranque, nunca en un
/// comando sincrónico (set_cookie se puede trabar en Windows desde ahí: wry#583).
///
/// 1. La cookie se guarda desde la ventana de arranque: el almacén de cookies
///    es el del perfil de WebView2, compartido por todas las ventanas.
/// 2. La ventana `main` se crea recién ahora porque su shim de
///    `window.orgtreeDesktop` lleva el origen exacto del motor, que antes no se
///    conoce (Electron le pasa el mismo dato al preload por argv).
fn open_engine(app: &tauri::AppHandle, engine: &Engine) -> Result<(), String> {
    let splash = splash_window(app).ok_or("no hay ventana de arranque")?;
    splash.set_cookie(desktop_cookie(engine)?).map_err(|e| format!("set_cookie: {e}"))?;
    let origin = engine.origin();
    *app.state::<Shell>().origin.lock().unwrap() = Some(origin.clone());
    let url = Url::parse(&format!("{origin}/")).map_err(|e| e.to_string())?;
    let shim = include_str!("shim.js").replace("__ORIGIN__", &origin);
    let navigation = app.clone();
    let popouts = app.clone();
    let titles = app.clone();
    let window = WebviewWindowBuilder::new(app, "main", WebviewUrl::External(url))
        .title("Orgtree (Tauri spike)")
        .inner_size(1200.0, 800.0)
        .min_inner_size(640.0, 480.0)
        .initialization_script(shim)
        .on_navigation(move |url| navigation_allowed(&navigation, url))
        .on_new_window(move |url, features| open_popout(&popouts, &url, features))
        .on_page_load(|window, payload| {
            if payload.event() != PageLoadEvent::Finished {
                return;
            }
            let app = window.app_handle();
            // La ventana de arranque se cierra recién cuando `main` ya cargó: cerrarla
            // antes deja a la app sin ventanas registradas y la termina.
            if let Some(splash) = splash_window(app) {
                let _ = splash.close();
            }
            if let Some(probe) = app.state::<Shell>().probe.lock().unwrap().as_ref() {
                let _ = window.eval(probe.script_for_load());
            }
        })
        .on_document_title_changed(move |window, title| on_title(&titles, &window, &title))
        .build()
        .map_err(|e| format!("ventana principal: {e}"))?;
    let events = app.clone();
    window.on_window_event(move |event| on_main_event(&events, event));
    Ok(())
}

fn start_engine(app: tauri::AppHandle) {
    let options = match engine_options(&app) {
        Ok(options) => options,
        Err(message) => return show_status(&app, &message),
    };
    std::thread::Builder::new()
        .name("orgtree-engine-start".into())
        .spawn(move || {
            let mut notify = |phase: &str| {
                let text = match phase {
                    "converting" => "Convirtiendo datos…",
                    _ => "Iniciando el motor…",
                };
                show_status(&app, text);
            };
            match Engine::start_with(&options, &mut notify) {
                Ok(engine) => {
                    let text = format!("Motor listo en {} (pid {}).", engine.origin(), engine.pid().unwrap_or(0));
                    let opened = open_engine(&app, &engine);
                    *app.state::<Shell>().engine.lock().unwrap() = Some(engine);
                    *app.state::<Shell>().options.lock().unwrap() = Some(options.clone());
                    match opened {
                        Ok(()) => {
                            *app.state::<Shell>().status.lock().unwrap() = text;
                            watch_engine(app.clone());
                        }
                        Err(error) => show_status(&app, &format!("{text} No se pudo abrir la interfaz: {error}")),
                    }
                }
                Err(error) => show_status(&app, &format!("El motor no arrancó: {error}")),
            }
        })
        .expect("no se pudo crear el hilo de arranque del motor");
}

/// Recuperación (#5): si el motor se cae, lo vuelve a lanzar con las mismas
/// opciones, renueva la cookie (el token es nuevo en cada arranque) y recarga la
/// ventana, que reconecta su WebSocket. El guardián del motor puede tardar en
/// liberar la raíz, así que un `root-owned` se reintenta. Si el puerto cambiara,
/// el shim de `main` quedaría con el origen viejo: el motor persiste su puerto
/// (`engine-port.json`) justamente para que no pase, y el spike no cubre ese caso.
fn watch_engine(app: tauri::AppHandle) {
    std::thread::Builder::new()
        .name("orgtree-engine-watch".into())
        .spawn(move || loop {
            std::thread::sleep(std::time::Duration::from_secs(1));
            let state = app.state::<Shell>();
            if state.quitting.load(Ordering::SeqCst) {
                return;
            }
            let dead = state.engine.lock().unwrap().as_mut().is_some_and(|engine| !engine.is_running());
            if !dead {
                continue;
            }
            drop(state.engine.lock().unwrap().take());
            #[cfg(debug_assertions)]
            eprintln!("[shell] el motor se cayó; reiniciando");
            let Some(options) = state.options.lock().unwrap().clone() else { return };
            // ~2 min: en Linux el puerto guardado sigue en TIME_WAIT unos 60 s después
            // de la caída y el motor rechaza un puerto ocupado (engine/launch.py `_port`).
            for attempt in 0..90 {
                if state.quitting.load(Ordering::SeqCst) {
                    return;
                }
                match Engine::start(&options) {
                    Ok(engine) => {
                        let same_origin = state.origin.lock().unwrap().as_deref() == Some(engine.origin().as_str());
                        if let Some(window) = main_window(&app) {
                            if let Ok(cookie) = desktop_cookie(&engine) {
                                let _ = window.set_cookie(cookie);
                            }
                            if same_origin {
                                let _ = window.eval("location.reload()");
                            }
                        }
                        *state.engine.lock().unwrap() = Some(engine);
                        state.restarts.fetch_add(1, Ordering::SeqCst);
                        break;
                    }
                    Err(error) if attempt < 89 => {
                        #[cfg(debug_assertions)]
                        eprintln!("[shell] reinicio {attempt} falló: {error}");
                        let _ = error;
                        std::thread::sleep(std::time::Duration::from_secs(1))
                    }
                    Err(error) => {
                        #[cfg(debug_assertions)]
                        eprintln!("[shell] el motor no volvió: {error}");
                        let _ = error;
                    }
                }
            }
        })
        .expect("no se pudo crear el vigilante del motor");
}

/// Origen `scheme://host:port` de una URL, como lo compara el navegador.
pub(crate) fn origin_of(url: &Url) -> String {
    url.origin().ascii_serialization()
}

/// Las ventanas solo navegan a `about:blank` o al origen exacto del motor.
fn navigation_allowed(app: &tauri::AppHandle, url: &Url) -> bool {
    let engine = app.state::<Shell>().origin.lock().unwrap().clone();
    url.as_str() == "about:blank" || engine.is_some_and(|origin| origin == origin_of(url))
}

const POPOUT_PREFIX: &str = "popout-";
static POPOUT_SEQ: AtomicU32 = AtomicU32::new(1);

fn popout_labels(app: &tauri::AppHandle, except: Option<&str>) -> Vec<String> {
    app.webview_windows()
        .into_keys()
        .filter(|label| label.starts_with(POPOUT_PREFIX) && Some(label.as_str()) != except)
        .collect()
}

/// `window.open('about:blank' | '')` del renderer (popouts, desks temporales,
/// pins). Con `NewWindowResponse::Create`, wry llama `SetNewWindow` de WebView2:
/// la ventana nueva queda unida a la que la abrió, `window.open` devuelve su
/// `WindowProxy` y el `opener` se conserva, así que los portales de React del
/// dueño pueden escribir en su documento. `window_features` le pasa el mismo
/// entorno de WebView2 que el dueño, que es lo que exige `SetNewWindow`.
fn open_popout(app: &tauri::AppHandle, url: &Url, features: NewWindowFeatures) -> NewWindowResponse<tauri::Wry> {
    if !(url.as_str() == "about:blank" || url.as_str().is_empty()) {
        // Links externos y cualquier otra URL: no se abren dentro de la app.
        return NewWindowResponse::Deny;
    }
    let label = format!("{POPOUT_PREFIX}{}", POPOUT_SEQ.fetch_add(1, Ordering::Relaxed));
    let navigation = app.clone();
    let built = WebviewWindowBuilder::new(app, &label, WebviewUrl::External("about:blank".parse().unwrap()))
        .window_features(features)
        .title("Orgtree")
        .on_navigation(move |url| navigation_allowed(&navigation, url))
        // El renderer pone el título en el documento del hijo (`d.title`).
        .on_document_title_changed(|window, title| {
            let _ = window.set_title(&title);
        })
        // Un popout no abre otros popouts: el renderer siempre abre desde el dueño.
        .on_new_window(|_, _| NewWindowResponse::Deny)
        .build();
    match built {
        Ok(window) => {
            let app = app.clone();
            window.on_window_event(move |event| {
                if let WindowEvent::Destroyed = event {
                    // Con la ventana dueña oculta y sin popouts, no queda nada visible.
                    let main_hidden = main_window(&app).map(|w| !w.is_visible().unwrap_or(true)).unwrap_or(true);
                    if main_hidden && popout_labels(&app, Some(&label)).is_empty() {
                        app.exit(0);
                    }
                }
            });
            NewWindowResponse::Create { window }
        }
        Err(_) => NewWindowResponse::Deny,
    }
}

/// Cerrar la ventana dueña con popouts abiertos la oculta en lugar de
/// destruirla: los popouts viven en el contexto de JavaScript del dueño, y
/// Electron también los conserva ("Main close preserves all popouts").
fn on_main_event(app: &tauri::AppHandle, event: &WindowEvent) {
    if let WindowEvent::CloseRequested { api, .. } = event {
        if !popout_labels(app, None).is_empty() {
            api.prevent_close();
            if let Some(window) = main_window(app) {
                let _ = window.hide();
            }
        }
    }
}

/// La prueba de `ORGTREE_TAURI_PROBE` devuelve sus resultados por el título.
fn on_title(app: &tauri::AppHandle, window: &WebviewWindow, title: &str) {
    let state = app.state::<Shell>();
    let probe = state.probe.lock().unwrap();
    let Some(probe) = probe.as_ref() else { return };
    if probe.record_page(title) {
        probe_owner_close(app.clone());
    } else if title.starts_with(probe::CLOSE_PREFIX) {
        let shell = serde_json::json!({
            "main_visible": window.is_visible().ok(),
            "popouts": popout_labels(app, None).len(),
        });
        if probe.record_close(title, shell) {
            probe_engine_crash(app.clone());
        }
    } else if title.starts_with(probe::RECONNECT_PREFIX) {
        let state = app.state::<Shell>();
        let pid = state.engine.lock().unwrap().as_ref().and_then(|engine| engine.pid());
        probe.record_reconnect(title, serde_json::json!({
            "restarts": state.restarts.load(Ordering::SeqCst),
            "engine_pid": pid,
        }));
    }
}

/// Etapa de la prueba (#5): mata el motor como si se cayera; el vigilante tiene
/// que reiniciarlo y la ventana, reconectarse. La página recargada corre el
/// script de reconexión.
fn probe_engine_crash(app: tauri::AppHandle) {
    std::thread::spawn(move || {
        let state = app.state::<Shell>();
        if let Some(probe) = state.probe.lock().unwrap().as_ref() {
            probe.begin_reconnect();
        }
        if let Some(window) = main_window(&app) {
            let _ = window.show();
        }
        let old_pid = state.engine.lock().unwrap().as_mut().and_then(|engine| {
            let pid = engine.pid();
            engine.kill();
            pid
        });
        let probe = state.probe.lock().unwrap();
        if let Some(probe) = probe.as_ref() {
            probe.note_crashed_pid(old_pid);
        }
    });
}

/// Etapa de la prueba: pide cerrar la ventana dueña y le pregunta a su página
/// si el popout sigue vivo.
fn probe_owner_close(app: tauri::AppHandle) {
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(500));
        let Some(window) = main_window(&app) else { return };
        let _ = window.close();
        std::thread::sleep(std::time::Duration::from_millis(1500));
        let _ = window.eval(probe::Probe::close_script());
    });
}

fn main() {
    let app = tauri::Builder::default()
        .manage(Shell::default())
        .invoke_handler(tauri::generate_handler![
            desktop::desktop_app_version,
            desktop::desktop_status,
            desktop::desktop_window_state,
            desktop::desktop_window_controls_state,
            desktop::desktop_preferences,
            desktop::desktop_set_preferences,
            desktop::desktop_show,
            desktop::desktop_quit,
            desktop::desktop_window_minimize,
            desktop::desktop_window_toggle_maximize,
            desktop::desktop_window_close,
            desktop::desktop_harnesses,
        ])
        .setup(|app| {
            let handle = app.handle().clone();
            *app.state::<Shell>().probe.lock().unwrap() = probe::Probe::from_env();
            let config = app.config().app.windows.first().cloned().ok_or("falta la ventana splash en tauri.conf.json")?;
            WebviewWindowBuilder::from_config(&handle, &config)?
                .on_navigation(|url| matches!(url.scheme(), "tauri") || url.host_str() == Some("tauri.localhost"))
                .on_new_window(|_, _| NewWindowResponse::Deny)
                .on_page_load(|window, payload| {
                    // La ventana de inicio puede cargar después del primer aviso: reenviarlo.
                    if payload.event() == PageLoadEvent::Finished {
                        let app = window.app_handle();
                        let status = app.state::<Shell>().status.lock().unwrap().clone();
                        if !status.is_empty() {
                            show_status(app, &status);
                        }
                    }
                })
                .build()?;
            start_engine(handle);
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error while building the Orgtree Tauri shell");
    app.run(|app, event| {
        if let RunEvent::Exit = event {
            app.state::<Shell>().quitting.store(true, Ordering::SeqCst);
            if let Some(engine) = app.state::<Shell>().engine.lock().unwrap().take() {
                let _ = engine.stop();
            }
        }
    });
}
