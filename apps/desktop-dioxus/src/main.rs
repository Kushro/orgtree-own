// Release builds run without a console window on Windows.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod attention;
mod desk;
mod home;
mod icons;
mod inbox;
mod native;
mod notify;
mod org;
mod probe;
mod reveal;
mod windows;

use dioxus::desktop::tao::event::Event;
use dioxus::desktop::{Config, WindowBuilder};
use dioxus::prelude::*;
use orgtree_engine_client::Client;
use orgtree_engine_host::{write_engine_paths, Engine, EngineOptions};
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

/// El CSS del renderer React, sin cambios: la UI en RSX usa sus mismas clases.
pub(crate) const RENDERER_CSS: &str = include_str!("../../desktop/renderer/src/styles.css");
/// Lo poco que el recorte agrega encima (pantalla de arranque y vista de org).
pub(crate) const SHELL_CSS: &str = include_str!("shell.css");
/// La hoja propia de la vista de atención del renderer (`attention/attention.css`), sin cambios (#28).
pub(crate) const ATTENTION_CSS: &str = include_str!("../../desktop/renderer/src/attention/attention.css");

/// El motor vive fuera del VirtualDom para poder apagarlo al cerrar el loop.
/// El token nunca llega al webview: solo lo usa el cliente Rust.
static ENGINE: Mutex<Option<Engine>> = Mutex::new(None);

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

/// Lo que el hilo de arranque le cuenta a la UI.
enum Update {
    Status(String),
    Ready(Client),
}

/// Carpeta propia de la app (nunca la de Orgtree instalado): guarda la raíz de
/// datos por defecto y el descriptor `engine-paths.json`, como `userData` en Electron.
pub(crate) fn app_dir() -> Option<PathBuf> {
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

/// Arranca el motor en un hilo y manda cada estado por el canal.
fn start_engine(updates: futures_channel::mpsc::UnboundedSender<Update>) {
    let launch = match launch() {
        Ok(launch) => launch,
        Err(message) => {
            let _ = updates.unbounded_send(Update::Status(message.clone()));
            return;
        }
    };
    std::thread::Builder::new()
        .name("orgtree-engine-start".into())
        .spawn(move || {
            let options = &launch.options;
            if let Some(descriptor) = &launch.descriptor {
                if let Err(error) = write_engine_paths(descriptor, options) {
                    let _ = updates.unbounded_send(Update::Status(format!("El motor no arrancó: {error}")));
                    return;
                }
            }
            let mut notify = |phase: &str| {
                let text = if phase == "converting" { "Convirtiendo datos…" } else { "Iniciando el motor…" };
                let _ = updates.unbounded_send(Update::Status(text.to_string()));
            };
            match Engine::start_with(options, &mut notify) {
                Ok(engine) => match Client::new(engine.origin(), engine.token().expose()) {
                    Ok(client) => {
                        *ENGINE.lock().unwrap() = Some(engine);
                        let _ = updates.unbounded_send(Update::Ready(client));
                    }
                    Err(error) => {
                        let _ = updates.unbounded_send(Update::Status(format!("Cliente del motor: {error}")));
                    }
                },
                Err(error) => {
                    let _ = updates.unbounded_send(Update::Status(format!("El motor no arrancó: {error}")));
                }
            }
        })
        .expect("no se pudo crear el hilo de arranque del motor");
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

fn stop_engine() {
    if let Some(engine) = ENGINE.lock().unwrap().take() {
        let _ = engine.stop();
    }
}

fn main() {
    // Instancia única (#14): una segunda ejecución le avisa a la primera y termina.
    if let Ok(launch) = launch() {
        if let native::Instance::Second = native::single_instance(&launch.options.data_root) {
            return;
        }
    }
    let window = WindowBuilder::new()
        .with_title("Orgtree (Dioxus spike)")
        // Sin marco (#14), como Electron (`frame: false`): botones propios en RSX
        // y arrastre con `-webkit-app-region`, que WebView2 respeta porque wry
        // activa `IsNonClientRegionSupportEnabled`.
        .with_decorations(false)
        .with_inner_size(dioxus::desktop::LogicalSize::new(1200.0, 800.0))
        .with_min_inner_size(dioxus::desktop::LogicalSize::new(640.0, 480.0));

    // Sin la barra de menú por defecto de Dioxus: la app actual no tiene.
    let config = Config::new().with_window(window).with_menu(None).with_custom_event_handler(|event, _| {
        // Último evento del loop antes de que la app salga: apagar el motor.
        if let Event::LoopDestroyed = event {
            stop_engine();
        }
    });
    dioxus::LaunchBuilder::desktop().with_cfg(config).launch(App);
}

#[component]
fn App() -> Element {
    let mut status = use_signal(|| "Iniciando…".to_string());
    let mut client = use_signal(|| None::<Client>);
    let route = use_signal(|| Route::Home);
    use_context_provider(|| route);
    use_context_provider(|| client);
    use_hook(|| windows::register_main(dioxus::desktop::window()));
    native::use_tray();
    native::use_second_instance();
    // #28: las notificaciones nativas y la barra de tareas corren solo en la
    // principal; un clic deja acá el elemento que la vista de la org abre.
    let focus = use_signal(|| None::<orgtree_engine_client::DesktopNotice>);
    use_context_provider(|| focus);
    notify::use_notifications(focus);
    use_hook(move || {
        let (sender, mut receiver) = futures_channel::mpsc::unbounded();
        start_engine(sender);
        spawn(async move {
            use futures_util::StreamExt;
            while let Some(update) = receiver.next().await {
                match update {
                    Update::Status(text) => {
                        probe::startup_status(&text);
                        status.set(text)
                    }
                    Update::Ready(ready) => client.set(Some(ready)),
                }
            }
        });
    });
    let body = match client() {
        None => rsx! {
            main { class: "dx-splash",
                h1 { "Orgtree" }
                p { id: "status", "{status}" }
                DataRoot {}
            }
        },
        Some(_) => match route() {
            Route::Home => rsx! { home::Home {} },
            Route::Org(slug) => rsx! { org::OrgView { slug } },
            Route::Desk { org, node } => rsx! { desk::DeskView { org, node } },
        },
    };
    rsx! {
        style { {RENDERER_CSS} }
        style { {SHELL_CSS} }
        style { {ATTENTION_CSS} }
        {body}
        if client().is_some() {
            probe::Probe {}
        }
    }
}
