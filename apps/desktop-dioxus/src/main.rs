// Release builds run without a console window on Windows.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod attention;
mod autostart;
mod desk;
mod docket;
mod external;
mod harnesses;
mod home;
mod icons;
mod inbox;
mod lifecycle;
mod login;
mod lprobe;
mod native;
mod notify;
mod org;
mod orgsettings;
mod orgwindows;
mod placement;
mod probe;
mod reveal;
mod settings;
mod windows;
mod wprobe;

use dioxus::desktop::tao::event::{Event, WindowEvent};
use dioxus::desktop::{Config, WindowBuilder};
use dioxus::prelude::*;
use orgtree_engine_client::Client;
use orgtree_engine_host::{Engine, EngineOptions};
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

/// El CSS del renderer React, sin cambios: la UI en RSX usa sus mismas clases.
pub(crate) const RENDERER_CSS: &str = include_str!("../../desktop/renderer/src/styles.css");
/// Lo poco que el recorte agrega encima (pantalla de arranque y vista de org).
pub(crate) const SHELL_CSS: &str = include_str!("shell.css");
/// La hoja propia de la vista de atención del renderer (`attention/attention.css`), sin cambios (#28).
pub(crate) const ATTENTION_CSS: &str = include_str!("../../desktop/renderer/src/attention/attention.css");

/// El motor vive fuera del VirtualDom para poder apagarlo al cerrar el loop.
/// El token nunca llega al webview: solo lo usa el cliente Rust. Lo maneja
/// `lifecycle.rs` (#25): arranque, caídas, reinicios y salida.
pub(crate) static ENGINE: Mutex<Option<Engine>> = Mutex::new(None);

/// El cliente del motor, que la UI recibe por contexto cuando el motor está listo.
pub fn engine_client() -> Client {
    use_context::<Signal<Option<Client>>>()().expect("la UI solo se monta con el motor listo")
}

/// Qué muestra la ventana.
#[derive(Clone, PartialEq, Debug)]
pub enum Route {
    Home,
    Org(String),
    Desk { org: String, node: String },
}

/// Carpeta propia de la app (nunca la de Orgtree instalado): guarda la raíz de
/// datos por defecto, el descriptor `engine-paths.json`, las preferencias y el
/// lugar de las ventanas, como `userData` en Electron. `ORGTREE_DIOXUS_PROFILE`
/// la cambia (las pruebas usan una carpeta propia por paso).
pub(crate) fn app_dir() -> Option<PathBuf> {
    if let Some(profile) = std::env::var_os("ORGTREE_DIOXUS_PROFILE").filter(|p| !p.is_empty()) {
        return Some(PathBuf::from(profile));
    }
    std::env::var_os("LOCALAPPDATA")
        .map(|base| PathBuf::from(base).join("com.kushro.orgtree.dioxus-spike"))
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share/orgtree-dioxus-spike")))
}

/// Recursos empaquetados por el instalador, junto al ejecutable (`<instalación>\resources`).
fn resources_dir() -> Option<PathBuf> {
    std::env::current_exe().ok()?.parent().map(|dir| dir.join("resources"))
}

/// Las raíces que la app nunca usa, como el `forbiddenRoot` de Electron: la de
/// Orgtree instalado y la de v1, más un `ORGTREE_DATA` heredado.
fn forbidden_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Some(appdata) = std::env::var_os("APPDATA") {
        roots.push(PathBuf::from(appdata).join("Orgtree v2"));
    }
    if let Some(home) = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")) {
        roots.push(PathBuf::from(home).join("orgtree"));
    }
    if let Some(data) = std::env::var_os("ORGTREE_DATA").filter(|d| !d.is_empty()) {
        roots.push(PathBuf::from(data));
    }
    roots
}

/// Cómo arranca el motor en esta ejecución.
pub struct Launch {
    pub options: EngineOptions,
    /// Instalada: runtime, motor y PostgreSQL empaquetados. Si no, desarrollo.
    pub packaged: bool,
    /// Dónde se escribe `engine-paths.json` (solo instalada).
    pub descriptor: Option<PathBuf>,
}

/// Configuración, calculada una vez. Con `ORGTREE_DIOXUS_PYTHON` es el modo de
/// desarrollo (un Python con las dependencias y el `engine/` del checkout o
/// `ORGTREE_DIOXUS_ENGINE_DIR`). Sin él, la app instalada usa sus recursos,
/// como `app.isPackaged` en `apps/desktop/main/index.ts`.
pub fn launch() -> &'static Result<Launch, String> {
    static LAUNCH: OnceLock<Result<Launch, String>> = OnceLock::new();
    LAUNCH.get_or_init(|| {
        // Nunca la raíz real de Orgtree: por defecto, una carpeta del identificador del spike.
        let data_root = match std::env::var_os("ORGTREE_DIOXUS_DATA").filter(|d| !d.is_empty()) {
            Some(root) => PathBuf::from(root),
            None => app_dir()
                .map(|dir| dir.join("data"))
                .ok_or("No hay LOCALAPPDATA ni HOME para la raíz de datos; definir ORGTREE_DIOXUS_DATA.")?,
        };
        let mut launch = match std::env::var_os("ORGTREE_DIOXUS_PYTHON").filter(|p| !p.is_empty()) {
            Some(python) => {
                let engine_dir = std::env::var_os("ORGTREE_DIOXUS_ENGINE_DIR")
                    .map(PathBuf::from)
                    .unwrap_or_else(|| PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../../engine")));
                Launch { options: EngineOptions::new(PathBuf::from(python), engine_dir, data_root), packaged: false, descriptor: None }
            }
            None => {
                let resources = resources_dir().filter(|r| r.join("engine").join("launch.py").is_file()).ok_or(
                    "No hay un motor instalado junto al ejecutable. Para desarrollo, definir ORGTREE_DIOXUS_PYTHON \
                     con la ruta absoluta de un Python con las dependencias del motor.",
                )?;
                let options = EngineOptions::packaged(&resources, data_root).map_err(|e| e.to_string())?;
                Launch { options, packaged: true, descriptor: app_dir().map(|dir| dir.join("engine-paths.json")) }
            }
        };
        launch.options.forbidden_roots = forbidden_roots();
        Ok(launch)
    })
}

/// La raíz de datos en uso, para mostrarla en la UI.
#[component]
pub fn DataRoot() -> Element {
    let Ok(launch) = launch() else { return rsx! {} };
    let root = launch.options.data_root.display().to_string();
    let mode = if launch.packaged { "app instalada" } else { "desarrollo" };
    rsx! {
        p { class: "dim dx-data-root", title: "Raíz de datos propia de esta app ({mode}); nunca la de Orgtree instalado",
            "Datos: "
            code { "{root}" }
            span { class: "dx-launch-mode", " · {mode}" }
        }
    }
}

/// Dónde queda el registro de panics: `<datos>/diagnostics/desktop-panic.log`,
/// junto a los diagnósticos del motor. Sin raíz de datos válida, en la carpeta
/// propia de la app; nunca dentro de una raíz prohibida (la de Orgtree instalado).
fn panic_log_path() -> Option<PathBuf> {
    match launch() {
        Ok(launch) => {
            let root = &launch.options.data_root;
            if launch.options.forbidden_roots.iter().any(|forbidden| root.starts_with(forbidden)) {
                return None;
            }
            Some(root.join("diagnostics").join("desktop-panic.log"))
        }
        Err(_) => app_dir().map(|dir| dir.join("desktop-panic.log")),
    }
}

/// Deja registrado cualquier panic (mensaje, hilo y ubicación) antes del
/// handler por defecto. El perfil release usa `panic = "abort"` y la app no
/// tiene consola: sin esto, un panic solo se ve como el código 0xC0000409.
fn install_panic_log() {
    let Some(path) = panic_log_path() else { return };
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let message = info
            .payload()
            .downcast_ref::<&str>()
            .map(|s| s.to_string())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "(panic sin mensaje de texto)".into());
        let location = info.location().map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column())).unwrap_or_default();
        let thread = std::thread::current();
        let at_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or_default();
        let line = serde_json::json!({
            "atMs": at_ms as u64,
            "pid": std::process::id(),
            "thread": thread.name().unwrap_or("<sin nombre>"),
            "message": message,
            "location": location,
        });
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
            use std::io::Write;
            let _ = writeln!(file, "{line}");
        }
        previous(info);
    }));
}

fn main() {
    install_panic_log();
    // Instancia única (#14): una segunda ejecución le avisa a la primera y termina.
    if let Ok(launch) = launch() {
        if let native::Instance::Second = native::single_instance(&launch.options.data_root) {
            return;
        }
    }
    // #25: la principal abre como la primera ventana de la sesión guardada, en su
    // lugar; las demás se reabren cuando el motor está listo.
    let (initial, placement) = orgwindows::main_plan();
    *INITIAL_ROUTE.lock().unwrap() = Some(initial);
    let window = WindowBuilder::new()
        .with_title("Orgtree (Dioxus spike)")
        // Sin marco (#14), como Electron (`frame: false`): botones propios en RSX
        // y arrastre con `-webkit-app-region`, que WebView2 respeta porque wry
        // activa `IsNonClientRegionSupportEnabled`.
        .with_decorations(false)
        // Con `--background` (el inicio de Windows) la app arranca en la bandeja.
        .with_visible(!autostart::background())
        .with_min_inner_size(dioxus::desktop::LogicalSize::new(orgwindows::MIN_SIZE.0, orgwindows::MIN_SIZE.1));
    let window = orgwindows::place_builder(window, placement);

    // Sin la barra de menú por defecto de Dioxus: la app actual no tiene.
    let config = Config::new().with_window(window).with_menu(None).with_custom_event_handler(|event, _| match event {
        // Alt+F4 o el menú del sistema en una ventana principal: decide el cierre
        // (`performClose`) antes de que Dioxus lo aplique.
        Event::WindowEvent { event: WindowEvent::CloseRequested, window_id, .. } => {
            orgwindows::native_close_requested(*window_id);
        }
        // Último evento del loop antes de que la app salga: por Salir, o por el fin
        // de la sesión de Windows (tao termina el loop con WM_ENDSESSION). Se guarda
        // la sesión de ventanas y se apaga el motor en orden.
        Event::LoopDestroyed => {
            let path = orgwindows::loop_destroyed();
            // #30: ningún login de proveedor queda vivo (se mata su árbol)
            login::logins().cancel_all();
            lifecycle::stop_engine_for_exit(path);
            probe::flush();
            // Fin de sesión: tao (0.34, `thread_event_target_callback`) atiende
            // WM_ENDSESSION llevando su runner a `Destroyed` y emite este evento
            // DENTRO del wndproc, pero el loop sigue vivo. El próximo mensaje del
            // hilo (un evento de usuario, un WM_PAINT) intenta salir de
            // `Destroyed` y tao entra en pánico ("cannot move state from
            // Destroyed"); con `panic = "abort"` el proceso moría con 0xC0000409.
            // Todo está guardado y el motor apagado: se sale acá, en orden y con 0,
            // antes de devolver el control al loop. Por "Salir" (`quit`), el loop
            // ya terminó y tao llama a `process::exit` solo.
            if path == "session-end" {
                std::process::exit(0);
            }
        }
        _ => {}
    });
    dioxus::LaunchBuilder::desktop().with_cfg(config).launch(App);
}

/// La ruta con la que abre la principal (la primera ventana de la sesión).
static INITIAL_ROUTE: Mutex<Option<Route>> = Mutex::new(None);

#[component]
fn App() -> Element {
    use_hook(|| windows::register_main(dioxus::desktop::window()));
    native::use_tray();
    native::use_second_instance();
    // #28: las notificaciones nativas y la barra de tareas corren solo en la
    // principal; un clic lleva el elemento a la ventana de su org (#25).
    notify::use_notifications();
    // #25: el arranque del motor y su pantalla. La vista de la ventana aparece con
    // el primer cliente del motor y se queda (un reinicio la vuelve a montar).
    let mut ready = use_signal(|| lifecycle::current_client().is_some());
    use_hook(lifecycle::start_engine);
    use_future(move || async move {
        let mut changes = lifecycle::subscribe_client();
        loop {
            if changes.borrow_and_update().is_some() && !*ready.peek() {
                ready.set(true);
                // #30: el tema por defecto sigue al primer proveedor instalado
                if let Some(client) = lifecycle::current_client() {
                    if let Ok(payload) = client.providers().await {
                        settings::providers_known(&payload);
                    }
                }
            }
            if changes.changed().await.is_err() {
                break;
            }
        }
    });
    // Una conversión o un rechazo se muestran aunque la app arrancó en la bandeja
    // (Electron abre su ventana de conversión y su diálogo igual); la prueba de la
    // app instalada ve el motivo de un arranque fallido.
    use_future(|| async move {
        let mut changes = lifecycle::subscribe_splash();
        loop {
            let splash = changes.borrow_and_update().clone();
            match &splash {
                lifecycle::Splash::Converting { .. } => orgwindows::reveal(orgwindows::MAIN),
                lifecycle::Splash::Failed { message, .. } => {
                    orgwindows::reveal(orgwindows::MAIN);
                    probe::startup_status(&format!("El motor no arrancó: {message}"));
                }
                _ => {}
            }
            if changes.changed().await.is_err() {
                break;
            }
        }
    });
    let initial = INITIAL_ROUTE.lock().unwrap().clone().unwrap_or(Route::Home);
    rsx! {
        if ready() {
            orgwindows::Shell { win: orgwindows::MAIN, initial }
        } else {
            style { {RENDERER_CSS} }
            style { {SHELL_CSS} }
            settings::ThemeStyle {}
            lifecycle::SplashView {}
        }
        lprobe::LifecycleProbe {}
    }
}
