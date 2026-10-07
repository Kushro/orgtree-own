//! Popouts del renderer (#22): las ventanas de `window.open('', nombre, features)`
//! que abre `renderer/src/popout.tsx` (desks, modales anclables y la bandeja de
//! agentes), con lo que les da `windows.ts` en Electron:
//!
//! - **Nombre de marco.** El renderer manda los comandos de una ventana
//!   (`getPopoutState`, `minimizePopout`, `toggleMaximizePopout`,
//!   `closePopout`, `focusPopout`) desde el puente de la ventana DUEÑA, porque
//!   el popout es un `about:blank` adoptado y sus handlers de React corren en
//!   el contexto de JS de la dueña. El comando nombra su ventana con el nombre
//!   de marco del `window.open`, como `popoutRegistry` (frameName de
//!   `did-create-window`). wry no pasa ese nombre a `on_new_window`, así que el
//!   shell suma su propio handler de `NewWindowRequested` en la dueña, que lee
//!   `ICoreWebView2NewWindowRequestedEventArgs2::Name` (ver `watch_owner`).
//!   Cada dueña tiene sus nombres: el de una org nunca alcanza la ventana de otra.
//! - **Eventos `popout-state`** a la dueña cuando el popout se maximiza, se
//!   restaura o se minimiza, también con un doble clic en la zona de arrastre.
//! - **Rectángulo exacto.** El renderer pide posición y tamaño del área
//!   cliente (`popupFeatures`), y al restaurar o devolver un desk prestado
//!   reusa lo que midió del popout. tao conserva `WS_CAPTION` en las ventanas
//!   sin marco, y el tamaño o la posición de la ventana no coinciden con el
//!   área cliente. `exact_bounds` mide y corrige, como `setExactPopoutBounds`.
//! - **`window.close()`.** wry atiende `WindowCloseRequested` destruyendo solo
//!   su contenedor (`WRY_WEBVIEW`), y la ventana de Tauri quedaba en blanco. El
//!   shell suma su propio handler del mismo evento, que cierra la ventana
//!   entera (ver `intercept_close`). El vigilante de main.rs queda como red
//!   de seguridad.

use crate::desktop::authorize;
use crate::mainwin;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Mutex;
use std::time::Duration;
use tauri::{AppHandle, Manager, PhysicalPosition, Webview, WebviewWindow, WindowEvent};

/// Lo que pidió el renderer en `window.open`: posición y tamaño del área
/// cliente, en píxeles lógicos.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Requested {
    pub position: Option<(f64, f64)>,
    pub size: Option<(f64, f64)>,
}

struct Entry {
    owner: String,
    name: String,
    requested: Requested,
    /// (maximizada, minimizada) la última vez que se avisó a la dueña.
    state: (bool, bool),
}

#[derive(Default)]
pub struct Popouts {
    entries: Mutex<HashMap<String, Entry>>,
    /// Los nombres de marco de cada dueña, en el orden de sus `window.open`.
    pending: Mutex<HashMap<String, VecDeque<String>>>,
    /// Popouts cuyo cierre ya está en curso (el vigilante no los toca).
    closing: Mutex<HashSet<String>>,
    /// Para la prueba: cómo se cerró cada popout.
    closed: Mutex<Vec<Value>>,
}

fn popouts(app: &AppHandle) -> &Popouts {
    &app.state::<crate::Shell>().inner().popouts
}

// ------------------------------------------------------------ nombres de marco

/// Suma a la ventana principal `window` un handler de `NewWindowRequested` que
/// anota el nombre de marco de cada `window.open`. wry registra el suyo al
/// crear el webview, así que corre antes que este; pero el suyo solo toma un
/// diferimiento y agenda `on_new_window` en el bucle de mensajes, que corre
/// después de que terminen los dos. Por eso `take_name` encuentra el nombre en
/// orden: uno por `window.open`, también los que se rechazan.
#[cfg(windows)]
pub fn watch_owner(app: &AppHandle, window: &WebviewWindow) {
    use webview2_com::Microsoft::Web::WebView2::Win32::ICoreWebView2NewWindowRequestedEventArgs2;
    use webview2_com::NewWindowRequestedEventHandler;
    use windows::core::{Interface, PWSTR};
    let app = app.clone();
    let owner = window.label().to_string();
    let _ = window.with_webview(move |webview| unsafe {
        let Ok(core) = webview.controller().CoreWebView2() else { return };
        let handler = NewWindowRequestedEventHandler::create(Box::new(move |_, args| {
            let name = args
                .and_then(|args| args.cast::<ICoreWebView2NewWindowRequestedEventArgs2>().ok())
                .and_then(|args| {
                    let mut name = PWSTR::null();
                    args.Name(&mut name).ok()?;
                    Some(webview2_com::take_pwstr(name))
                })
                .unwrap_or_default();
            popouts(&app).pending.lock().unwrap().entry(owner.clone()).or_default().push_back(name);
            Ok(())
        }));
        let mut token = 0i64;
        let _ = core.add_NewWindowRequested(&handler, &mut token);
    });
}

#[cfg(not(windows))]
pub fn watch_owner(_app: &AppHandle, _window: &WebviewWindow) {}

/// El nombre de marco del `window.open` que llega ahora a `on_new_window` de
/// la dueña `owner` (vacío fuera de Windows o si WebView2 no lo dio).
pub fn take_name(app: &AppHandle, owner: &str) -> String {
    popouts(app).pending.lock().unwrap().get_mut(owner).and_then(VecDeque::pop_front).unwrap_or_default()
}

// ------------------------------------------------------------------ registro

/// Un popout recién creado: su dueña, su nombre y lo que pidió. Desde acá
/// sigue sus cambios de estado y corrige su rectángulo.
pub fn register(app: &AppHandle, window: &WebviewWindow, owner: &str, name: String, requested: Requested) {
    let label = window.label().to_string();
    let state = (window.is_maximized().unwrap_or(false), window.is_minimized().unwrap_or(false));
    popouts(app).entries.lock().unwrap().insert(label.clone(), Entry { owner: owner.to_string(), name, requested, state });
    intercept_close(app, window);
    exact_bounds(window, requested);
    // WebView2 adopta el webview en la ventana después de `SetNewWindow`, y el
    // primer cuadro puede ajustar el marco: se vuelve a medir dos veces.
    let later = window.clone();
    std::thread::spawn(move || {
        for wait in [200, 600] {
            std::thread::sleep(Duration::from_millis(wait));
            exact_bounds(&later, requested);
        }
    });
    let events = app.clone();
    window.on_window_event(move |event| on_event(&events, &label, event));
}

fn on_event(app: &AppHandle, label: &str, event: &WindowEvent) {
    match event {
        WindowEvent::Resized(_) | WindowEvent::Moved(_) | WindowEvent::Focused(_) => publish(app, label),
        WindowEvent::Destroyed => {
            popouts(app).entries.lock().unwrap().remove(label);
            popouts(app).closing.lock().unwrap().remove(label);
        }
        _ => {}
    }
}

/// Avisa a la dueña si el popout se maximizó, se restauró o se minimizó
/// (`popoutRegistry.track`: maximize, unmaximize, minimize y restore).
fn publish(app: &AppHandle, label: &str) {
    let Some(window) = app.get_webview_window(label) else { return };
    let now = (window.is_maximized().unwrap_or(false), window.is_minimized().unwrap_or(false));
    let target = {
        let mut entries = popouts(app).entries.lock().unwrap();
        let Some(entry) = entries.get_mut(label) else { return };
        if entry.state == now {
            return;
        }
        entry.state = now;
        (entry.owner.clone(), entry.name.clone())
    };
    let (owner, name) = target;
    if name.is_empty() {
        return;
    }
    // `parent-teardown` no se da acá: una dueña que se cierra destruye sus
    // popouts sin pasar por estos eventos (`mainwin::perform_close`).
    mainwin::send_to(app, &owner, json!({
        "type": "popout-state",
        "data": { "name": name, "present": true, "maximized": now.0, "reason": "user" },
    }));
}

/// El popout de la dueña `owner` con el nombre de marco `name`, si sigue vivo.
/// Un nombre vacío no es de nadie (un `_blank`), como en Electron.
pub fn find(app: &AppHandle, owner: &str, name: &str) -> Option<WebviewWindow> {
    if name.is_empty() {
        return None;
    }
    let label = popouts(app)
        .entries
        .lock()
        .unwrap()
        .iter()
        .filter(|(_, e)| e.owner == owner && e.name == name)
        .map(|(label, _)| label.clone())
        // Un nombre reusado: el popout más nuevo (las etiquetas crecen).
        .max_by_key(|label| label.trim_start_matches(crate::POPOUT_PREFIX).parse::<u64>().unwrap_or(0))?;
    let window = app.get_webview_window(&label)?;
    if popouts(app).closing.lock().unwrap().contains(&label) {
        return None;
    }
    Some(window)
}

// ------------------------------------------------------------ rectángulo exacto

/// Deja el área cliente del popout en la posición y el tamaño que pidió el
/// renderer. El tamaño se corrige con `mainwin::exact_size` (el alto de la
/// barra de título de más). La posición de una ventana (`set_position`) es la
/// del marco exterior, que en una ventana sin marco de tao no coincide con el
/// área cliente, así que se mueve el marco lo que le falta al área cliente.
pub fn exact_bounds(window: &WebviewWindow, requested: Requested) {
    if window.is_maximized().unwrap_or(false) || window.is_minimized().unwrap_or(false) {
        return;
    }
    let scale = window.scale_factor().unwrap_or(1.0);
    if let Some((w, h)) = requested.size {
        let want = ((w * scale).round().max(1.0) as u32, (h * scale).round().max(1.0) as u32);
        mainwin::exact_size(window, want);
    }
    if let Some((x, y)) = requested.position {
        let want = ((x * scale).round() as i32, (y * scale).round() as i32);
        for _ in 0..2 {
            let (Ok(inner), Ok(outer)) = (window.inner_position(), window.outer_position()) else { return };
            if (inner.x, inner.y) == want {
                break;
            }
            let _ = window.set_position(PhysicalPosition::new(outer.x + want.0 - inner.x, outer.y + want.1 - inner.y));
        }
    }
}

// ------------------------------------------------------------------ cierre

/// `window.close()` en el popout. wry ya atiende `WindowCloseRequested`
/// destruyendo solo el HWND contenedor del webview; este segundo handler del
/// mismo evento cierra la ventana de Tauri, así no queda una ventana vacía.
/// La destrucción va al bucle de eventos (`run_on_main_thread`): destruir el
/// webview dentro de su propio evento lo dejaría a mitad de la llamada.
#[cfg(windows)]
fn intercept_close(app: &AppHandle, window: &WebviewWindow) {
    use webview2_com::WindowCloseRequestedEventHandler;
    let app = app.clone();
    let label = window.label().to_string();
    let _ = window.with_webview(move |webview| unsafe {
        let Ok(core) = webview.controller().CoreWebView2() else { return };
        let handler = WindowCloseRequestedEventHandler::create(Box::new(move |_, _| {
            if begin_close(&app, &label, "window.close") {
                let (app, label) = (app.clone(), label.clone());
                let target = app.clone();
                let _ = target.run_on_main_thread(move || {
                    if let Some(window) = app.get_webview_window(&label) {
                        let _ = window.destroy();
                    }
                });
            }
            Ok(())
        }));
        let mut token = 0i64;
        let _ = core.add_WindowCloseRequested(&handler, &mut token);
    });
}

#[cfg(not(windows))]
fn intercept_close(_app: &AppHandle, _window: &WebviewWindow) {}

/// Marca un popout como cerrándose y anota quién lo cierra. Devuelve `false`
/// si otro camino ya lo estaba cerrando.
pub fn begin_close(app: &AppHandle, label: &str, by: &str) -> bool {
    if !popouts(app).closing.lock().unwrap().insert(label.to_string()) {
        return false;
    }
    let name = popouts(app).entries.lock().unwrap().get(label).map(|e| e.name.clone());
    popouts(app).closed.lock().unwrap().push(json!({ "label": label, "name": name, "by": by }));
    true
}

pub fn is_closing(app: &AppHandle, label: &str) -> bool {
    popouts(app).closing.lock().unwrap().contains(label)
}

/// Para la prueba: cómo se cerró cada popout hasta ahora.
pub fn closed_log(app: &AppHandle) -> Vec<Value> {
    popouts(app).closed.lock().unwrap().clone()
}

// ------------------------------------------------------------------ comandos

/// `getPopoutState(name)`: `{ name, present, maximized }`.
#[tauri::command]
pub fn desktop_popout_state(webview: Webview, name: String) -> Result<Value, String> {
    let owner = authorize(&webview)?;
    let window = find(webview.app_handle(), &owner, &name);
    let maximized = window.as_ref().is_some_and(|w| w.is_maximized().unwrap_or(false));
    Ok(json!({ "name": name, "present": window.is_some(), "maximized": maximized }))
}

#[tauri::command]
pub fn desktop_popout_minimize(webview: Webview, name: String) -> Result<(), String> {
    let owner = authorize(&webview)?;
    if let Some(window) = find(webview.app_handle(), &owner, &name) {
        window.minimize().map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[tauri::command]
pub fn desktop_popout_toggle_maximize(webview: Webview, name: String) -> Result<(), String> {
    let owner = authorize(&webview)?;
    if let Some(window) = find(webview.app_handle(), &owner, &name) {
        let result = if window.is_maximized().unwrap_or(false) { window.unmaximize() } else { window.maximize() };
        result.map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Como `window.close()` en Electron: el documento del popout se va, el
/// renderer lo nota (`w.closed`, `pagehide`) y devuelve la superficie.
#[tauri::command]
pub fn desktop_popout_close(webview: Webview, name: String) -> Result<(), String> {
    let owner = authorize(&webview)?;
    let app = webview.app_handle();
    if let Some(window) = find(app, &owner, &name) {
        if begin_close(app, window.label(), "closePopout") {
            window.destroy().map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

/// "Show desk" / "Show window": `revealPopout`. Primero se restaura una
/// ventana minimizada (mostrarla la deja en la barra de tareas), después se
/// muestra y se enfoca. El `child.focus()` del renderer no la levanta.
#[tauri::command]
pub fn desktop_popout_focus(webview: Webview, name: String) -> Result<bool, String> {
    let owner = authorize(&webview)?;
    let Some(window) = find(webview.app_handle(), &owner, &name) else { return Ok(false) };
    if window.is_minimized().unwrap_or(false) {
        let _ = window.unminimize();
    }
    let _ = window.show();
    let _ = window.set_focus();
    Ok(true)
}

// ------------------------------------------------------------------ informe

/// Para la prueba: cada popout con su dueña, su nombre, lo pedido y lo real.
pub fn report(app: &AppHandle) -> Value {
    let entries = popouts(app).entries.lock().unwrap();
    let rows: Vec<Value> = app
        .webview_windows()
        .into_iter()
        .filter(|(label, _)| label.starts_with(crate::POPOUT_PREFIX))
        .map(|(label, window)| {
            let entry = entries.get(&label);
            let inner = window.inner_position().ok();
            let size = window.inner_size().ok();
            let outer = window.outer_position().ok();
            let outer_size = window.outer_size().ok();
            json!({
                "label": label,
                "owner": entry.map(|e| e.owner.clone()),
                "name": entry.map(|e| e.name.clone()),
                "requested": entry.map(|e| json!({
                    "x": e.requested.position.map(|p| p.0), "y": e.requested.position.map(|p| p.1),
                    "width": e.requested.size.map(|s| s.0), "height": e.requested.size.map(|s| s.1),
                })),
                "client": { "x": inner.map(|p| p.x), "y": inner.map(|p| p.y), "width": size.map(|s| s.width), "height": size.map(|s| s.height) },
                "outer": { "x": outer.map(|p| p.x), "y": outer.map(|p| p.y), "width": outer_size.map(|s| s.width), "height": outer_size.map(|s| s.height) },
                "scale": window.scale_factor().ok(),
                "visible": window.is_visible().ok(),
                "minimized": window.is_minimized().ok(),
                "maximized": window.is_maximized().ok(),
                "focused": window.is_focused().ok(),
                "decorated": window.is_decorated().ok(),
                "containerGone": crate::webview_container_gone(&window),
                "title": window.title().ok(),
            })
        })
        .collect();
    Value::Array(rows)
}
