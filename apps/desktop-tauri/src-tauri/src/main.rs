// Release builds run without a console window on Windows.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod probe;

use orgtree_engine_host::{Engine, EngineOptions};
use std::path::PathBuf;
use std::sync::Mutex;
use tauri::webview::{Cookie, PageLoadEvent};
use tauri::{Manager, RunEvent, Url, WebviewWindow, WebviewWindowBuilder};

/// Nombre de la cookie que `TokenGate` (engine/launch.py) acepta en lugar del header.
const DESKTOP_COOKIE: &str = "orgtree_desktop_token";

/// Estado del shell. El token nunca pasa por acá: va del motor a la cookie.
#[derive(Default)]
struct Shell {
    engine: Mutex<Option<Engine>>,
    /// Origen exacto del motor una vez listo; la única URL remota permitida.
    origin: Mutex<Option<String>>,
    status: Mutex<String>,
    probe: Mutex<Option<probe::Probe>>,
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
    // Build del renderer que sirve el motor (#4); sin él, el motor responde 404 en `/`.
    options.ui_dir = std::env::var_os("ORGTREE_TAURI_UI_DIR").map(PathBuf::from);
    Ok(options)
}

fn main_window(app: &tauri::AppHandle) -> Option<WebviewWindow> {
    app.get_webview_window("main")
}

fn show_status(app: &tauri::AppHandle, text: &str) {
    *app.state::<Shell>().status.lock().unwrap() = text.to_string();
    if let Some(window) = main_window(app) {
        let literal = serde_json::to_string(text).unwrap_or_default();
        let _ = window.eval(format!("window.orgtreeEngineStatus && window.orgtreeEngineStatus({literal})"));
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

/// Pasa la ventana al motor: cookie primero, después la navegación. Corre en el
/// hilo de arranque, nunca en un comando sincrónico (set_cookie se puede trabar
/// en Windows desde ahí: wry#583).
fn open_engine(app: &tauri::AppHandle, engine: &Engine) -> Result<(), String> {
    let window = main_window(app).ok_or("no hay ventana principal")?;
    window.set_cookie(desktop_cookie(engine)?).map_err(|e| format!("set_cookie: {e}"))?;
    *app.state::<Shell>().origin.lock().unwrap() = Some(engine.origin());
    let url = Url::parse(&format!("{}/", engine.origin())).map_err(|e| e.to_string())?;
    window.navigate(url).map_err(|e| format!("navigate: {e}"))
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
                    let opened = open_engine(&app, &engine);
                    let text = format!("Motor listo en {} (pid {}).", engine.origin(), engine.pid().unwrap_or(0));
                    *app.state::<Shell>().engine.lock().unwrap() = Some(engine);
                    match opened {
                        Ok(()) => *app.state::<Shell>().status.lock().unwrap() = text,
                        Err(error) => show_status(&app, &format!("{text} No se pudo abrir la interfaz: {error}")),
                    }
                }
                Err(error) => show_status(&app, &format!("El motor no arrancó: {error}")),
            }
        })
        .expect("no se pudo crear el hilo de arranque del motor");
}

/// Origen `scheme://host:port` de una URL, como lo compara el navegador.
fn origin_of(url: &Url) -> String {
    url.origin().ascii_serialization()
}

/// La ventana solo navega a la página local de arranque o al origen exacto del motor.
fn navigation_allowed(app: &tauri::AppHandle, url: &Url) -> bool {
    let local = matches!(url.scheme(), "tauri") || url.host_str() == Some("tauri.localhost");
    let engine = app.state::<Shell>().origin.lock().unwrap().clone();
    local || url.as_str() == "about:blank" || engine.is_some_and(|origin| origin == origin_of(url))
}

fn main() {
    let app = tauri::Builder::default()
        .manage(Shell::default())
        .setup(|app| {
            let handle = app.handle().clone();
            *app.state::<Shell>().probe.lock().unwrap() = probe::Probe::from_env();
            let config = app.config().app.windows.first().cloned().ok_or("falta la ventana main en tauri.conf.json")?;
            let navigation = handle.clone();
            let titles = handle.clone();
            WebviewWindowBuilder::from_config(&handle, &config)?
                .on_navigation(move |url| navigation_allowed(&navigation, url))
                .on_page_load(|window, payload| {
                    if payload.event() != PageLoadEvent::Finished {
                        return;
                    }
                    let app = window.app_handle();
                    let on_engine = app.state::<Shell>().origin.lock().unwrap().clone() == Some(origin_of(payload.url()));
                    if on_engine {
                        if let Some(probe) = app.state::<Shell>().probe.lock().unwrap().as_ref() {
                            let _ = window.eval(probe.script());
                        }
                    } else {
                        // La ventana de inicio puede cargar después del primer aviso: reenviarlo.
                        let status = app.state::<Shell>().status.lock().unwrap().clone();
                        if !status.is_empty() {
                            show_status(app, &status);
                        }
                    }
                })
                .on_document_title_changed(move |_, title| {
                    if let Some(probe) = titles.state::<Shell>().probe.lock().unwrap().as_ref() {
                        probe.record_title(&title);
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
            if let Some(engine) = app.state::<Shell>().engine.lock().unwrap().take() {
                let _ = engine.stop();
            }
        }
    });
}
