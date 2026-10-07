// Release builds run without a console window on Windows.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod home;
mod icons;
mod org;
mod probe;

use dioxus::desktop::tao::event::Event;
use dioxus::desktop::{Config, WindowBuilder};
use dioxus::prelude::*;
use orgtree_engine_client::Client;
use orgtree_engine_host::{Engine, EngineOptions};
use std::path::PathBuf;
use std::sync::Mutex;

/// El CSS del renderer React, sin cambios: la UI en RSX usa sus mismas clases.
const RENDERER_CSS: &str = include_str!("../../desktop/renderer/src/styles.css");
/// Lo poco que el recorte agrega encima (pantalla de arranque y vista de org).
const SHELL_CSS: &str = include_str!("shell.css");

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

/// Configuración del spike. El empaquetado del runtime de Python queda fuera
/// del recorte, así que el intérprete se indica por entorno.
fn engine_options() -> Result<EngineOptions, String> {
    let python = std::env::var_os("ORGTREE_DIOXUS_PYTHON")
        .map(PathBuf::from)
        .ok_or("Falta ORGTREE_DIOXUS_PYTHON: la ruta absoluta de un Python con las dependencias del motor.")?;
    let engine_dir = std::env::var_os("ORGTREE_DIOXUS_ENGINE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../../engine")));
    // Nunca la raíz real de Orgtree: por defecto, una carpeta del identificador del spike.
    let data_root = match std::env::var_os("ORGTREE_DIOXUS_DATA") {
        Some(root) => PathBuf::from(root),
        None => std::env::var_os("LOCALAPPDATA")
            .map(|base| PathBuf::from(base).join("com.kushro.orgtree.dioxus-spike").join("data"))
            .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share/orgtree-dioxus-spike/data")))
            .ok_or("No hay LOCALAPPDATA ni HOME para la raíz de datos; definir ORGTREE_DIOXUS_DATA.")?,
    };
    Ok(EngineOptions::new(python, engine_dir, data_root))
}

/// Arranca el motor en un hilo y manda cada estado por el canal.
fn start_engine(updates: futures_channel::mpsc::UnboundedSender<Update>) {
    let options = match engine_options() {
        Ok(options) => options,
        Err(message) => {
            let _ = updates.unbounded_send(Update::Status(message));
            return;
        }
    };
    std::thread::Builder::new()
        .name("orgtree-engine-start".into())
        .spawn(move || {
            let mut notify = |phase: &str| {
                let text = if phase == "converting" { "Convirtiendo datos…" } else { "Iniciando el motor…" };
                let _ = updates.unbounded_send(Update::Status(text.to_string()));
            };
            match Engine::start_with(&options, &mut notify) {
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

fn stop_engine() {
    if let Some(engine) = ENGINE.lock().unwrap().take() {
        let _ = engine.stop();
    }
}

fn main() {
    let window = WindowBuilder::new()
        .with_title("Orgtree (Dioxus spike)")
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
    use_hook(move || {
        let (sender, mut receiver) = futures_channel::mpsc::unbounded();
        start_engine(sender);
        spawn(async move {
            use futures_util::StreamExt;
            while let Some(update) = receiver.next().await {
                match update {
                    Update::Status(text) => status.set(text),
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
            }
        },
        Some(_) => match route() {
            Route::Home => rsx! { home::Home {} },
            Route::Org(slug) => rsx! { org::OrgView { slug } },
            Route::Desk { org, node } => rsx! { org::DeskView { org, node } },
        },
    };
    rsx! {
        style { {RENDERER_CSS} }
        style { {SHELL_CSS} }
        {body}
        if client().is_some() {
            probe::Probe {}
        }
    }
}
