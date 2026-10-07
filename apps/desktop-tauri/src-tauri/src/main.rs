// Release builds run without a console window on Windows.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod probe;

use orgtree_engine_host::{Engine, EngineOptions};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Mutex;
use tauri::webview::{Cookie, NewWindowFeatures, NewWindowResponse, PageLoadEvent};
use tauri::{Manager, RunEvent, Url, WebviewUrl, WebviewWindow, WebviewWindowBuilder, WindowEvent};

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
        .setup(|app| {
            let handle = app.handle().clone();
            *app.state::<Shell>().probe.lock().unwrap() = probe::Probe::from_env();
            let config = app.config().app.windows.first().cloned().ok_or("falta la ventana main en tauri.conf.json")?;
            let navigation = handle.clone();
            let titles = handle.clone();
            let popouts = handle.clone();
            let window = WebviewWindowBuilder::from_config(&handle, &config)?
                .on_navigation(move |url| navigation_allowed(&navigation, url))
                .on_new_window(move |url, features| open_popout(&popouts, &url, features))
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
                .on_document_title_changed(move |window, title| {
                    let state = titles.state::<Shell>();
                    let probe = state.probe.lock().unwrap();
                    let Some(probe) = probe.as_ref() else { return };
                    if probe.record_page(&title) {
                        probe_owner_close(titles.clone());
                    } else if title.starts_with(probe::CLOSE_PREFIX) {
                        let shell = serde_json::json!({
                            "main_visible": window.is_visible().ok(),
                            "popouts": popout_labels(&titles, None).len(),
                        });
                        probe.record_close(&title, shell);
                    }
                })
                .build()?;
            let events = handle.clone();
            window.on_window_event(move |event| on_main_event(&events, event));
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
