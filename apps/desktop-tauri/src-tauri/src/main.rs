// Release builds run without a console window on Windows.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod desktop;
mod files;
mod harnesses;
mod login;
mod notifications;
mod probe;

use orgtree_engine_host::packaged::PackagedRuntime;
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
    /// Notificaciones nativas (#21): el filtro, la deduplicación y los toasts pendientes.
    notifications: notifications::Notifications,
    /// El parpadeo de la barra de tareas (`setPendingAttention`).
    attention: Mutex<notifications::Attention>,
    /// Los logins de proveedores en curso.
    logins: login::Logins,
    /// Para la prueba: lo que decidió el shell en cada `notify`,
    /// `syncNotifications` y `setPendingAttention` (`<salida>.<nombre>`).
    probe_logs: Mutex<std::collections::HashMap<&'static str, Vec<serde_json::Value>>>,
    /// Cómo se lanzó el motor (`packaged` o `development`) y con qué raíz.
    launch: Mutex<serde_json::Value>,
    /// Prueba chica del instalador (`ORGTREE_TAURI_INSTALL_PROBE`).
    install_probe: Option<PathBuf>,
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
            notifications: Default::default(),
            attention: Mutex::new(Default::default()),
            logins: Default::default(),
            probe_logs: Mutex::new(Default::default()),
            launch: Mutex::new(serde_json::Value::Null),
            install_probe: std::env::var_os("ORGTREE_TAURI_INSTALL_PROBE").filter(|v| !v.is_empty()).map(PathBuf::from),
        }
    }
}

/// Cómo arranca el motor.
///
/// - **Desarrollo**: con `ORGTREE_TAURI_PYTHON`, un Python absoluto con las
///   dependencias del motor y el `engine/` del checkout (o
///   `ORGTREE_TAURI_ENGINE_DIR`), como hasta ahora.
/// - **Empaquetado**: sin esa variable y con el runtime embebido en los
///   recursos de la app (`<recursos>/engine/runtime/python.exe`), como
///   `apps/desktop/main/index.ts` con `app.isPackaged`: el Python, el motor,
///   PostgreSQL y el renderer del paquete, `ORGTREE_PG_BOOTSTRAP=1` y el
///   descriptor `engine-paths.json`.
///
/// La raíz de datos es siempre propia de la app (`ORGTREE_TAURI_DATA` o
/// `app_local_data_dir()/data`) y nunca la de Orgtree instalado.
fn engine_options(app: &tauri::AppHandle) -> Result<(EngineOptions, &'static str), String> {
    // Nunca la raíz real de Orgtree: por defecto, una carpeta del identificador del spike.
    let profile = app.path().app_local_data_dir().map_err(|e| e.to_string())?;
    let data_root = match std::env::var_os("ORGTREE_TAURI_DATA").filter(|v| !v.is_empty()) {
        Some(root) => PathBuf::from(root),
        None => profile.join("data"),
    };
    let (mut options, mode) = match std::env::var_os("ORGTREE_TAURI_PYTHON").filter(|v| !v.is_empty()) {
        Some(python) => {
            let engine_dir = std::env::var_os("ORGTREE_TAURI_ENGINE_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../../../engine")));
            let mut options = EngineOptions::new(PathBuf::from(python), engine_dir, &data_root);
            // Build del renderer que sirve el motor (`dist/renderer`); sin él, `/` da 404.
            options.ui_dir = std::env::var_os("ORGTREE_TAURI_UI_DIR").map(PathBuf::from);
            (options, "development")
        }
        None => {
            let resources = app.path().resource_dir().map_err(|e| e.to_string())?;
            let runtime = match PackagedRuntime::locate(&resources) {
                Some(runtime) => runtime.map_err(|e| e.to_string())?,
                None => {
                    return Err("Falta el motor empaquetado y no se definió ORGTREE_TAURI_PYTHON (la ruta \
                                absoluta de un Python con las dependencias del motor)."
                        .into())
                }
            };
            // El mismo descriptor que Electron escribe en su carpeta de perfil.
            runtime
                .write_engine_paths(&profile.join("engine-paths.json"), &absolute(&data_root))
                .map_err(|e| e.to_string())?;
            (runtime.options(&data_root), "packaged")
        }
    };
    options.forbidden_roots = real_orgtree_roots();
    Ok((options, mode))
}

fn absolute(path: &std::path::Path) -> PathBuf {
    std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf())
}

/// Las raíces de Orgtree instalado, que esta app nunca sirve (como
/// `forbiddenRoot` en Electron y la lista `live-locations.json` del motor).
fn real_orgtree_roots() -> Vec<PathBuf> {
    let var = |name: &str| std::env::var_os(name).filter(|v| !v.is_empty()).map(PathBuf::from);
    let mut roots = Vec::new();
    if let Some(appdata) = var("APPDATA") {
        roots.push(appdata.join("Orgtree v2"));
    }
    if let Some(local) = var("LOCALAPPDATA") {
        roots.push(local.join("Programs").join("Orgtree"));
    }
    if let Some(home) = var("USERPROFILE").or_else(|| var("HOME")) {
        roots.push(home.join("AppData").join("Roaming").join("Orgtree v2"));
        roots.push(home.join("orgtree"));
    }
    if let Some(data) = var("ORGTREE_DATA") {
        roots.push(data);
    }
    roots
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

/// La raíz de datos en uso y el modo, en la ventana de arranque.
fn show_launch(app: &tauri::AppHandle) {
    let launch = app.state::<Shell>().launch.lock().unwrap().clone();
    if launch.is_null() {
        return;
    }
    if let Some(window) = splash_window(app) {
        let _ = window.eval(format!("window.orgtreeLaunch && window.orgtreeLaunch({launch})"));
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
        // Sin marco (#7), como Electron (`frame: false`): el renderer dibuja sus
        // botones (`WindowControls`) y marca el arrastre con `-webkit-app-region`,
        // que WebView2 respeta porque wry activa `IsNonClientRegionSupportEnabled`.
        .decorations(false)
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
            if app.state::<Shell>().install_probe.is_some() {
                let _ = window.eval(include_str!("install_probe.js").replace("__PREFIX__", INSTALL_PROBE_PREFIX));
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
        Ok((options, mode)) => {
            *app.state::<Shell>().launch.lock().unwrap() = serde_json::json!({
                "mode": mode,
                "dataRoot": absolute(&options.data_root),
                "engineDir": options.engine_dir,
            });
            show_launch(&app);
            // También en la bandeja, que queda cuando la ventana de arranque se cierra.
            if let Some(tray) = app.tray_by_id("main") {
                let _ = tray.set_tooltip(Some(format!("Orgtree (Tauri spike)\nDatos: {}", absolute(&options.data_root).display())));
            }
            options
        }
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
        .decorations(false)
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
                    // Con la ventana dueña oculta, sin popouts y sin quedarse en la
                    // bandeja (`exitOnClose`), no queda nada: la app termina.
                    let main_hidden = main_window(&app).map(|w| !w.is_visible().unwrap_or(true)).unwrap_or(true);
                    if main_hidden && exit_on_close(&app) && popout_labels(&app, Some(&label)).is_empty() {
                        app.exit(0);
                    }
                }
            });
            NewWindowResponse::Create { window }
        }
        Err(_) => NewWindowResponse::Deny,
    }
}

/// `window.close()` desde JS (el renderer cierra así un popout al volver a
/// acoplarlo): WebView2 dispara `WindowCloseRequested` y wry solo destruye el
/// HWND contenedor del webview (clase `WRY_WEBVIEW`, hijo de la ventana de
/// Tauri), no la ventana, que queda vacía. El controlador de WebView2 sigue
/// respondiendo (`url()` no falla), así que el vigilante mira el contenedor:
/// si la ventana ya no lo tiene, la destruye. Mientras se crea, un popout
/// todavía no tiene contenedor: solo cuenta como cerrado uno al que ya se le
/// vio (el CI mostró que destruirlo a mitad de la creación traba la página).
fn reap_closed_popouts(app: tauri::AppHandle) {
    std::thread::Builder::new()
        .name("orgtree-popout-reaper".into())
        .spawn(move || {
            let mut seen = std::collections::HashSet::<String>::new();
            loop {
                std::thread::sleep(std::time::Duration::from_millis(400));
                let windows = app.webview_windows();
                seen.retain(|label| windows.contains_key(label));
                for (label, window) in windows {
                    if !label.starts_with(POPOUT_PREFIX) {
                        continue;
                    }
                    if !webview_container_gone(&window) {
                        seen.insert(label);
                    } else if seen.remove(&label) {
                        let _ = window.destroy();
                    }
                }
            }
        })
        .expect("no se pudo crear el vigilante de popouts");
}

#[cfg(windows)]
fn webview_container_gone(window: &tauri::WebviewWindow) -> bool {
    use std::ffi::c_void;
    #[link(name = "user32")]
    extern "system" {
        fn FindWindowExW(parent: *mut c_void, after: *mut c_void, class: *const u16, title: *const u16) -> *mut c_void;
    }
    let Ok(hwnd) = window.hwnd() else { return false };
    let class: Vec<u16> = "WRY_WEBVIEW".encode_utf16().chain([0]).collect();
    // SAFETY: hwnd es una ventana viva de Tauri y class termina en NUL.
    unsafe { FindWindowExW(hwnd.0, std::ptr::null_mut(), class.as_ptr(), std::ptr::null()).is_null() }
}

#[cfg(not(windows))]
fn webview_container_gone(_window: &tauri::WebviewWindow) -> bool {
    // Fuera de Windows wry no separa el contenedor: el cierre llega como evento.
    false
}

/// Cerrar la ventana dueña con popouts abiertos la oculta en lugar de
/// destruirla: los popouts viven en el contexto de JavaScript del dueño, y
/// Electron también los conserva ("Main close preserves all popouts").
/// Sin popouts, con `exitOnClose` apagado (el valor por defecto) la ventana
/// también se oculta y la app sigue en la bandeja, como en Electron.
fn on_main_event(app: &tauri::AppHandle, event: &WindowEvent) {
    match event {
        WindowEvent::CloseRequested { api, .. } => {
            if !popout_labels(app, None).is_empty() || !exit_on_close(app) {
                api.prevent_close();
                if let Some(window) = main_window(app) {
                    let _ = window.hide();
                }
            }
        }
        // Los botones propios de la ventana sin marco siguen el estado real
        // (maximizar con doble clic en la zona de arrastre, Win+flechas…).
        WindowEvent::Resized(_) | WindowEvent::Focused(_) => {
            if let Some(window) = main_window(app) {
                dispatch_event(app, serde_json::json!({ "type": "window-state", "data": desktop::controls_state(&window) }));
            }
        }
        _ => {}
    }
}

fn exit_on_close(app: &tauri::AppHandle) -> bool {
    app.state::<Shell>().preferences.lock().unwrap().get("exitOnClose") == Some(&serde_json::Value::Bool(true))
}

pub(crate) fn show_main(app: &tauri::AppHandle) {
    if let Some(window) = main_window(app) {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}

/// Ícono en la bandeja (#7) con el menú: abrir, los harnesses (#21) y salir.
/// Un clic en el ícono también abre la ventana.
///
/// "Harness setup" es el submenú de Electron: cada harness con su estado
/// (detectado o no) y su enlace oficial de instalación.
fn build_tray(app: &tauri::AppHandle) -> tauri::Result<()> {
    use tauri::menu::{Menu, MenuItem, Submenu};
    use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
    let open = MenuItem::with_id(app, "open", "Abrir Orgtree", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Salir", true, None::<&str>)?;
    let harness_items = harnesses::detect()
        .iter()
        .map(|h| {
            let state = if h.detected() { "detected" } else { "not detected" };
            MenuItem::with_id(app, format!("harness:{}", h.id), format!("{}: {state} - official setup", h.id), true, None::<&str>)
        })
        .collect::<tauri::Result<Vec<_>>>()?;
    let harness_refs: Vec<&dyn tauri::menu::IsMenuItem<tauri::Wry>> = harness_items.iter().map(|i| i as _).collect();
    let setup = Submenu::with_items(app, "Harness setup", true, &harness_refs)?;
    let menu = Menu::with_items(app, &[&open, &setup, &quit])?;
    let mut tray = TrayIconBuilder::with_id("main")
        .tooltip("Orgtree (Tauri spike)")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id().as_ref() {
            "open" => show_main(app),
            "quit" => app.exit(0),
            id => {
                if let Some(url) = id.strip_prefix("harness:").and_then(harnesses::link) {
                    let _ = harnesses::open_external(url);
                }
            }
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = event {
                show_main(tray.app_handle());
            }
        });
    if let Some(icon) = app.default_window_icon() {
        tray = tray.icon(icon.clone());
    }
    tray.build(app)?;
    Ok(())
}

/// Una segunda ejecución (#7) no abre otra app: el plugin la termina y le
/// avisa a esta, que enfoca su ventana.
fn on_second_instance(app: &tauri::AppHandle, args: Vec<String>) {
    show_main(app);
    if let Some(probe) = app.state::<Shell>().probe.lock().unwrap().as_ref() {
        let focused = main_window(app).and_then(|w| w.is_focused().ok());
        probe.record_marker("second-instance", serde_json::json!({ "args": args.len(), "focused": focused }));
    }
}

/// Prefijo del título con el que `install_probe.js` devuelve su resultado.
const INSTALL_PROBE_PREFIX: &str = "orgtree-install-probe:";

/// La prueba del instalador (#18): la página del motor empaquetado cargó, se
/// autentica y el renderer se dibuja. Se escribe junto con el modo de
/// arranque y la raíz de datos que usa el shell.
fn record_install_probe(app: &tauri::AppHandle, title: &str) {
    let state = app.state::<Shell>();
    let (Some(out), Some(json)) = (state.install_probe.as_ref(), title.strip_prefix(INSTALL_PROBE_PREFIX)) else { return };
    let page: serde_json::Value = serde_json::from_str(json).unwrap_or(serde_json::Value::Null);
    let engine = state.engine.lock().unwrap().as_ref().map(|engine| {
        serde_json::json!({ "pid": engine.pid(), "origin": engine.origin(), "dataRoot": engine.data_root() })
    });
    let report = serde_json::json!({
        "launch": *state.launch.lock().unwrap(),
        "engine": engine,
        "resources": app.path().resource_dir().ok(),
        "page": page,
    });
    let _ = std::fs::write(out, serde_json::to_vec_pretty(&report).unwrap_or_default());
}

/// Para diagnosticar el vigilante de popouts: cada popout abierto, si es
/// visible y si todavía tiene su contenedor `WRY_WEBVIEW`.
fn popout_report(app: &tauri::AppHandle) -> serde_json::Value {
    app.webview_windows()
        .into_iter()
        .filter(|(label, _)| label.starts_with(POPOUT_PREFIX))
        .map(|(label, window)| {
            serde_json::json!({
                "label": label,
                "visible": window.is_visible().ok(),
                "containerGone": webview_container_gone(&window),
            })
        })
        .collect()
}

/// La prueba de `ORGTREE_TAURI_PROBE` devuelve sus resultados por el título.
fn on_title(app: &tauri::AppHandle, window: &WebviewWindow, title: &str) {
    if title.starts_with(INSTALL_PROBE_PREFIX) {
        return record_install_probe(app, title);
    }
    let state = app.state::<Shell>();
    let probe = state.probe.lock().unwrap();
    let Some(probe) = probe.as_ref() else { return };
    if let Some(json) = title.strip_prefix(probe::CLICK_PREFIX) {
        // #21: el clic en un toast, simulado por la misma función que llama su
        // handler `Activated` (el CI no puede hacer clic en el centro de notificaciones).
        let target: serde_json::Value = serde_json::from_str(json).unwrap_or_default();
        let tag = state.notifications.tag_of(target["org"].as_str().unwrap_or(""), target["id"].as_str().unwrap_or(""));
        let clicked = tag.is_some_and(|tag| desktop::notification_activated(app, &tag));
        probe.record_marker("click", serde_json::json!({ "clicked": clicked, "target": target }));
    } else if let Some(json) = title.strip_prefix(probe::LATE_PREFIX) {
        probe.record_marker("late", serde_json::from_str(json).unwrap_or_default());
    } else if title.starts_with(probe::PAUSE_PREFIX) {
        probe.record_pause(title);
    } else if probe.record_page(title, serde_json::json!({ "popouts": popout_labels(app, None).len(), "windows": popout_report(app) })) {
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
            // #7: el CI arrastra la ventana con el mouse real a partir de estos datos.
            "native": native_report(app, window),
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
        // El popout de la primitiva ya cumplió (etapa de cierre). Se cierra para
        // que no se lleve el foco en la prueba de notificaciones (#21).
        for label in popout_labels(&app, None) {
            if let Some(popout) = app.get_webview_window(&label) {
                let _ = popout.destroy();
            }
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

/// Lo que el CI necesita para verificar la ventana nativa (#7).
fn native_report(app: &tauri::AppHandle, window: &WebviewWindow) -> serde_json::Value {
    #[cfg(windows)]
    let hwnd = window.hwnd().ok().map(|h| h.0 as isize);
    #[cfg(not(windows))]
    let hwnd: Option<isize> = None;
    serde_json::json!({
        "hwnd": hwnd,
        "decorated": window.is_decorated().ok(),
        "scale": window.scale_factor().ok(),
        "tray": app.tray_by_id("main").is_some(),
    })
}

/// Para la prueba: agrega una entrada al registro `<salida>.<nombre>` (solo
/// con `ORGTREE_TAURI_PROBE`). Así el CI ve lo que hizo el renderer real con
/// el puente: qué notificaciones pidió, cuáles se mostraron y por qué no.
pub(crate) fn record_log(app: &tauri::AppHandle, name: &'static str, entry: serde_json::Value) {
    let state = app.state::<Shell>();
    let probe = state.probe.lock().unwrap();
    let Some(probe) = probe.as_ref() else { return };
    let mut logs = state.probe_logs.lock().unwrap();
    let log = logs.entry(name).or_default();
    log.push(entry);
    probe.record_marker(name, serde_json::Value::Array(log.clone()));
}

fn main() {
    let app = tauri::Builder::default()
        // Primero: una segunda ejecución termina acá, antes de crear ventanas o motor.
        .plugin(tauri_plugin_single_instance::init(|app, args, _cwd| on_second_instance(app, args)))
        .plugin(tauri_plugin_notification::init())
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
            desktop::desktop_notify,
            desktop::desktop_sync_notifications,
            desktop::desktop_pending_attention,
            desktop::desktop_open_harness,
            desktop::desktop_reveal_file,
            desktop::desktop_open_charter_folder,
            desktop::desktop_provider_login_start,
            desktop::desktop_provider_login_status,
            desktop::desktop_provider_login_code,
            desktop::desktop_provider_login_cancel,
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
                        show_launch(app);
                    }
                })
                .build()?;
            build_tray(&handle)?;
            reap_closed_popouts(handle.clone());
            probe::watch_late(handle.clone());
            start_engine(handle);
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error while building the Orgtree Tauri shell");
    app.run(|app, event| {
        if let RunEvent::Exit = event {
            app.state::<Shell>().quitting.store(true, Ordering::SeqCst);
            // Ningún login de proveedor queda vivo después de la app (como el `quit` de Electron).
            app.state::<Shell>().logins.cancel_all();
            if let Some(engine) = app.state::<Shell>().engine.lock().unwrap().take() {
                let _ = engine.stop();
            }
        }
    });
}
