//! Comandos detrás del shim de `window.orgtreeDesktop` (`shim.js`).
//!
//! Cada comando verifica que lo llama la ventana principal y que su página
//! está en el origen exacto del motor, igual que los handlers de Electron
//! verifican el `webContents`, el frame y la ruta. Además, la capability
//! `engine-ui` solo deja invocar estos comandos desde `main` en `127.0.0.1`.

use crate::{main_window, origin_of, Shell};
use serde_json::{json, Map, Value};
use tauri::{AppHandle, Manager, Webview};

/// `DEFAULT_PREFERENCES` de `apps/desktop/main/policy.ts`. Sin persistencia en
/// el spike: viven en memoria mientras corre la app.
pub fn default_preferences() -> Value {
    json!({
        "notificationsEnabled": true,
        "visualTheme": "orgtree",
        "contrastTheme": "charcoal",
        "agentColorSource": "provider",
        "visualThemeExplicit": false,
        "exitOnClose": false,
        "startAtLogin": false,
        "automaticUpdates": false,
        "routineNotifications": false,
        "onboarded": false,
        "startupMode": "restore"
    })
}

fn authorize(webview: &Webview) -> Result<(), String> {
    let app = webview.app_handle();
    let engine = app.state::<Shell>().origin.lock().unwrap().clone();
    let url = webview.url().map_err(|e| e.to_string())?;
    if webview.label() == "main" && engine.is_some_and(|origin| origin == origin_of(&url)) {
        Ok(())
    } else {
        Err("orgtreeDesktop: solo la ventana principal en el origen del motor".into())
    }
}

#[tauri::command]
pub fn desktop_app_version(webview: Webview, app: AppHandle) -> Result<String, String> {
    authorize(&webview)?;
    Ok(app.package_info().version.to_string())
}

#[tauri::command]
pub fn desktop_status(webview: Webview) -> Result<Value, String> {
    authorize(&webview)?;
    let app = webview.app_handle();
    let running = app.state::<Shell>().engine.lock().unwrap().as_mut().is_some_and(|e| e.is_running());
    Ok(if running { json!({ "state": "ready" }) } else { json!({ "state": "stopped", "message": "El motor no está corriendo." }) })
}

#[tauri::command]
pub fn desktop_window_state(webview: Webview) -> Result<Value, String> {
    authorize(&webview)?;
    let visible = main_window(webview.app_handle()).and_then(|w| w.is_visible().ok()).unwrap_or(true);
    Ok(json!({ "visible": visible, "restoreWindows": false }))
}

/// `DesktopControlsState`: lo que dibujan los botones propios de la ventana sin marco.
pub(crate) fn controls_state(window: &tauri::WebviewWindow) -> Value {
    json!({
        "visible": window.is_visible().unwrap_or(true),
        "restoreWindows": false,
        "minimized": window.is_minimized().unwrap_or(false),
        "maximized": window.is_maximized().unwrap_or(false),
    })
}

#[tauri::command]
pub fn desktop_window_controls_state(webview: Webview) -> Result<Value, String> {
    authorize(&webview)?;
    let window = main_window(webview.app_handle()).ok_or("sin ventana principal")?;
    Ok(controls_state(&window))
}

#[tauri::command]
pub fn desktop_preferences(webview: Webview) -> Result<Value, String> {
    authorize(&webview)?;
    Ok(webview.app_handle().state::<Shell>().preferences.lock().unwrap().clone())
}

#[tauri::command]
pub fn desktop_set_preferences(webview: Webview, patch: Map<String, Value>) -> Result<Value, String> {
    authorize(&webview)?;
    let app = webview.app_handle();
    let state = app.state::<Shell>();
    let mut preferences = state.preferences.lock().unwrap();
    if let Value::Object(current) = &mut *preferences {
        // Solo claves conocidas, como `preferencesPatch` en Electron.
        for (key, value) in patch {
            if current.contains_key(&key) {
                current.insert(key, value);
            }
        }
    }
    let updated = preferences.clone();
    drop(preferences);
    crate::dispatch_event(app, json!({ "type": "preferences", "data": updated }));
    Ok(updated)
}

#[tauri::command]
pub fn desktop_show(webview: Webview) -> Result<(), String> {
    authorize(&webview)?;
    let window = main_window(webview.app_handle()).ok_or("sin ventana principal")?;
    window.show().and_then(|_| window.unminimize()).and_then(|_| window.set_focus()).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn desktop_quit(webview: Webview) -> Result<(), String> {
    authorize(&webview)?;
    webview.app_handle().exit(0);
    Ok(())
}

#[tauri::command]
pub fn desktop_window_minimize(webview: Webview) -> Result<(), String> {
    authorize(&webview)?;
    main_window(webview.app_handle()).ok_or("sin ventana principal")?.minimize().map_err(|e| e.to_string())
}

#[tauri::command]
pub fn desktop_window_toggle_maximize(webview: Webview) -> Result<(), String> {
    authorize(&webview)?;
    let window = main_window(webview.app_handle()).ok_or("sin ventana principal")?;
    let result = if window.is_maximized().unwrap_or(false) { window.unmaximize() } else { window.maximize() };
    result.map_err(|e| e.to_string())
}

#[tauri::command]
pub fn desktop_window_close(webview: Webview) -> Result<(), String> {
    authorize(&webview)?;
    main_window(webview.app_handle()).ok_or("sin ventana principal")?.close().map_err(|e| e.to_string())
}

/// Notificación nativa (#7), con el filtro mínimo de `NotificationGate` de
/// Electron: campos obligatorios, notificaciones activadas en las preferencias
/// y una sola vez por `org` + `id`. Las preferencias por tipo, el filtro con la
/// ventana enfocada, el clic que abre el elemento y `syncNotifications` quedan
/// fuera del recorte. En Windows, el plugin usa el AppUserModelID de la app
/// instalada; desde `target/release` usa el de PowerShell.
#[tauri::command]
pub fn desktop_notify(webview: Webview, notification: Map<String, Value>) -> Result<bool, String> {
    use tauri_plugin_notification::NotificationExt;
    authorize(&webview)?;
    let text = |key: &str, max: usize| match notification.get(key).and_then(Value::as_str) {
        Some(value) if !value.is_empty() && value.chars().count() <= max => Ok(value.to_string()),
        _ => Err(format!("notificación inválida: {key}")),
    };
    let (id, title, body, org) = (text("id", 200)?, text("title", 200)?, text("body", 2000)?, text("org", 128)?);
    text("kind", 30)?;
    let app = webview.app_handle();
    let state = app.state::<Shell>();
    if state.preferences.lock().unwrap().get("notificationsEnabled") != Some(&Value::Bool(true)) {
        return Ok(false);
    }
    if !state.notified.lock().unwrap().insert(format!("{org}\u{0}{id}")) {
        return Ok(false);
    }
    app.notification().builder().title(title).body(body).show().map_err(|e| e.to_string())?;
    Ok(true)
}

/// Presencia de los harnesses: fuera del recorte, se informan como no detectados.
#[tauri::command]
pub fn desktop_harnesses(webview: Webview) -> Result<Value, String> {
    authorize(&webview)?;
    Ok(json!([
        { "id": "claude", "detected": false, "url": "https://code.claude.com/docs/en/setup" },
        { "id": "codex", "detected": false, "url": "https://developers.openai.com/codex/cli" },
        { "id": "antigravity", "detected": false, "url": "https://antigravity.google/download" }
    ]))
}
