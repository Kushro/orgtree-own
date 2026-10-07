// Release builds run without a console window on Windows.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use dioxus::desktop::tao::event::Event;
use dioxus::desktop::{Config, WindowBuilder};
use dioxus::prelude::*;
use orgtree_engine_host::{Engine, EngineOptions};
use std::path::PathBuf;
use std::sync::Mutex;

const STYLE: &str = r#"
    :root { color-scheme: light dark; font-family: system-ui, sans-serif; }
    body { margin: 0; min-height: 100vh; display: grid; place-items: center; }
    main { text-align: center; max-width: 32rem; padding: 1rem; }
    p { opacity: .75; }
"#;

/// El motor vive fuera del VirtualDom para poder apagarlo al cerrar el loop.
/// El token nunca sale de este proceso: la UI solo ve el texto de estado.
static ENGINE: Mutex<Option<Engine>> = Mutex::new(None);

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
fn start_engine(updates: futures_channel::mpsc::UnboundedSender<String>) {
    let options = match engine_options() {
        Ok(options) => options,
        Err(message) => {
            let _ = updates.unbounded_send(message);
            return;
        }
    };
    std::thread::Builder::new()
        .name("orgtree-engine-start".into())
        .spawn(move || {
            let mut notify = |phase: &str| {
                let text = if phase == "converting" { "Convirtiendo datos…" } else { "Iniciando el motor…" };
                let _ = updates.unbounded_send(text.to_string());
            };
            let text = match Engine::start_with(&options, &mut notify) {
                Ok(engine) => {
                    let text = format!("Motor listo en {} (pid {}).", engine.origin(), engine.pid().unwrap_or(0));
                    *ENGINE.lock().unwrap() = Some(engine);
                    text
                }
                Err(error) => format!("El motor no arrancó: {error}"),
            };
            let _ = updates.unbounded_send(text);
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

    let config = Config::new().with_window(window).with_custom_event_handler(|event, _| {
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
    use_hook(move || {
        let (sender, mut receiver) = futures_channel::mpsc::unbounded();
        start_engine(sender);
        spawn(async move {
            use futures_util::StreamExt;
            while let Some(text) = receiver.next().await {
                status.set(text);
            }
        });
    });
    rsx! {
        style { {STYLE} }
        main {
            h1 { "Orgtree" }
            p { id: "status", "{status}" }
        }
    }
}
