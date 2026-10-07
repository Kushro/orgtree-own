// Release builds run without a console window on Windows.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use orgtree_engine_host::{Engine, EngineOptions};
use std::path::PathBuf;
use std::sync::Mutex;
use tauri::webview::PageLoadEvent;
use tauri::{Manager, RunEvent};

/// Estado del motor que ve la ventana de inicio. El token nunca pasa por acá.
#[derive(Default)]
struct Shell {
    engine: Mutex<Option<Engine>>,
    status: Mutex<String>,
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
    Ok(EngineOptions::new(python, engine_dir, data_root))
}

fn show_status(app: &tauri::AppHandle, text: &str) {
    *app.state::<Shell>().status.lock().unwrap() = text.to_string();
    if let Some(window) = app.get_webview_window("main") {
        let literal = serde_json::to_string(text).unwrap_or_default();
        let _ = window.eval(format!("window.orgtreeEngineStatus && window.orgtreeEngineStatus({literal})"));
    }
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
                    *app.state::<Shell>().engine.lock().unwrap() = Some(engine);
                    show_status(&app, &text);
                }
                Err(error) => show_status(&app, &format!("El motor no arrancó: {error}")),
            }
        })
        .expect("no se pudo crear el hilo de arranque del motor");
}

fn main() {
    let app = tauri::Builder::default()
        .manage(Shell::default())
        // La ventana de inicio puede cargar después del primer aviso: reenviarlo.
        .on_page_load(|webview, payload| {
            if payload.event() == PageLoadEvent::Finished {
                let status = webview.app_handle().state::<Shell>().status.lock().unwrap().clone();
                if !status.is_empty() {
                    show_status(webview.app_handle(), &status);
                }
            }
        })
        .setup(|app| {
            start_engine(app.handle().clone());
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
