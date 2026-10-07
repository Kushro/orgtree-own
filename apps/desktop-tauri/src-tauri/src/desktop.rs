//! Comandos detrás del shim de `window.orgtreeDesktop` (`shim.js`).
//!
//! Cada comando verifica que lo llama la ventana principal y que su página
//! está en el origen exacto del motor, igual que los handlers de Electron
//! verifican el `webContents`, el frame y la ruta. Además, la capability
//! `engine-ui` solo deja invocar estos comandos desde `main` en `127.0.0.1`.

use crate::notifications::{self, Decision, Notice, Pulse};
use crate::{files, harnesses, login, main_window, origin_of, Shell};
use serde_json::{json, Map, Value};
use tauri::{AppHandle, Manager, Webview};

/// `DEFAULT_PREFERENCES` de `apps/desktop/main/policy.ts`. Sin persistencia en
/// el spike: viven en memoria mientras corre la app.
pub fn default_preferences() -> Value {
    let mut preferences = json!({
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
    });
    // Las preferencias por tipo de notificación (`DEFAULT_NOTIFICATIONS`, #21).
    for (key, default) in notifications::OPTIONS {
        preferences[key] = Value::Bool(default);
    }
    preferences
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
            // Las booleanas solo aceptan booleanos, como `preferencesPatch`.
            let boolean = current.get(&key).is_some_and(Value::is_boolean);
            if current.contains_key(&key) && (!boolean || value.is_boolean()) {
                current.insert(key, value);
            }
        }
    }
    let updated = preferences.clone();
    drop(preferences);
    // Un tipo de notificación que se apagó retira sus toasts (`configure`).
    for (tag, _) in state.notifications.configure(&updated) {
        remove_toast(app, &tag);
    }
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


// ------------------------------------------------------------------ #21

/// Alguna ventana de Orgtree visible, no minimizada y enfocada
/// (`anyOrgtreeWindowFocused`): la principal o un popout.
fn any_window_focused(app: &AppHandle) -> bool {
    app.webview_windows().values().any(|w| {
        w.is_visible().unwrap_or(false) && !w.is_minimized().unwrap_or(false) && w.is_focused().unwrap_or(false)
    })
}

/// El AppUserModelID de los toasts: el de la app instalada o, desde `target/`,
/// el de PowerShell (como el plugin de notificaciones).
#[cfg(windows)]
fn toast_app_id(app: &AppHandle) -> String {
    notifications::toast::app_id(&app.config().identifier)
}

/// Retira un toast que ya no corresponde (`notice.close()` en Electron).
pub(crate) fn remove_toast(app: &AppHandle, tag: &str) {
    #[cfg(windows)]
    notifications::toast::remove(&toast_app_id(app), tag);
    #[cfg(not(windows))]
    let _ = (app, tag);
}

/// El clic en un toast: muestra la ventana y le entrega al renderer el evento
/// `notification-click`, que abre el elemento (`useNativeNotifications`).
/// Un toast cuyo elemento ya se resolvió no hace nada.
pub(crate) fn notification_activated(app: &AppHandle, tag: &str) -> bool {
    let Some(notice) = app.state::<Shell>().notifications.clicked(tag) else { return false };
    crate::show_main(app);
    crate::dispatch_event(app, json!({ "type": "notification-click", "data": notice.data }));
    true
}

fn show_notice(app: &AppHandle, notice: &Notice, tag: &str) -> Result<(), String> {
    #[cfg(windows)]
    {
        let clicks = app.clone();
        let clicked = tag.to_string();
        // El handler `Activated` corre en un hilo de WinRT; las APIs de Tauri
        // que usa (`show`, `eval`) se pueden llamar desde cualquier hilo.
        notifications::toast::show(&toast_app_id(app), tag, &notice.title, &notice.body, move || {
            notification_activated(&clicks, &clicked);
        })
        .map_err(|e| e.to_string())
    }
    #[cfg(not(windows))]
    {
        use tauri_plugin_notification::NotificationExt;
        let _ = tag;
        app.notification().builder().title(&notice.title).body(&notice.body).show().map_err(|e| e.to_string())
    }
}

/// Notificación nativa con el filtro completo de `NativeNotifications`:
/// preferencias por tipo, ventana enfocada, conjunto activo de
/// `syncNotifications` y una sola vez por `org` + `id`. Devuelve si se mostró.
#[tauri::command]
pub fn desktop_notify(webview: Webview, notification: Map<String, Value>) -> Result<bool, String> {
    authorize(&webview)?;
    let notice = notifications::validate(&notification)?;
    let app = webview.app_handle();
    let state = app.state::<Shell>();
    let preferences = state.preferences.lock().unwrap().clone();
    let (shown, decision) = match state.notifications.decide(&notice, &preferences, any_window_focused(app)) {
        Decision::Skip(reason) => (false, reason.to_string()),
        Decision::Show { tag } => match show_notice(app, &notice, &tag) {
            Ok(()) => (true, "shown".to_string()),
            Err(error) => {
                state.notifications.failed(&notice);
                (false, format!("failed: {error}"))
            }
        },
    };
    crate::record_log(app, "notify", json!({ "id": notice.data["id"], "kind": notice.kind, "decision": decision }));
    Ok(shown)
}

/// `syncNotifications(active)`: retira los toasts cuyo elemento ya no está pendiente.
#[tauri::command]
pub fn desktop_sync_notifications(webview: Webview, active: Value) -> Result<(), String> {
    authorize(&webview)?;
    let active = notifications::identities(&active)?;
    let app = webview.app_handle();
    let count = active.len();
    let removed = app.state::<Shell>().notifications.sync(active);
    for (tag, _) in &removed {
        remove_toast(app, tag);
    }
    let ids: Vec<&str> = removed.iter().map(|(_, id)| id.as_str()).collect();
    crate::record_log(app, "sync", json!({ "active": count, "removed": ids }));
    Ok(())
}

/// `setPendingAttention(ids, items)`: el botón de la barra de tareas parpadea
/// (`request_user_attention`, FLASHW_ALL | FLASHW_TIMERNOFG) con cada llegada
/// nueva y para cuando no queda nada. Nunca parpadea la ventana que el usuario
/// está mirando. Devuelve si empezó un parpadeo (el shim lo descarta).
#[tauri::command]
pub fn desktop_pending_attention(webview: Webview, ids: Value, items: Option<Value>) -> Result<bool, String> {
    use tauri::UserAttentionType;
    authorize(&webview)?;
    let ids = notifications::attention_payload(&ids, items.as_ref())?;
    let app = webview.app_handle();
    let state = app.state::<Shell>();
    let count = ids.len();
    let pulse = state.attention.lock().unwrap().set(ids);
    let window = main_window(app).ok_or("sin ventana principal")?;
    let focused = window.is_focused().unwrap_or(false) && !window.is_minimized().unwrap_or(false);
    let started = match pulse {
        Pulse::Start if !focused => {
            window.request_user_attention(Some(UserAttentionType::Critical)).map_err(|e| e.to_string())?;
            true
        }
        Pulse::Stop => {
            window.request_user_attention(None).map_err(|e| e.to_string())?;
            false
        }
        _ => false,
    };
    crate::record_log(app, "attention", json!({ "ids": count, "pulse": format!("{pulse:?}"), "started": started, "focused": focused }));
    Ok(started)
}

/// Los harnesses instalados en esta máquina (`detectHarnesses`).
#[tauri::command]
pub fn desktop_harnesses(webview: Webview) -> Result<Value, String> {
    authorize(&webview)?;
    Ok(harnesses::as_json(&harnesses::detect()))
}

/// Abre el enlace oficial de un harness en el navegador del sistema. La URL es
/// fija: la página solo elige el id.
#[tauri::command]
pub fn desktop_open_harness(webview: Webview, harness: String) -> Result<(), String> {
    authorize(&webview)?;
    let url = harnesses::link(&harness).ok_or("Unknown harness")?;
    harnesses::open_external(url)
}

/// Selecciona un archivo en el Explorador; nunca lo abre ni lo ejecuta.
#[tauri::command]
pub fn desktop_reveal_file(webview: Webview, path: Value) -> Result<Value, String> {
    authorize(&webview)?;
    Ok(files::reveal_file(path.as_str().unwrap_or("")))
}

#[tauri::command]
pub fn desktop_open_charter_folder(webview: Webview) -> Result<Value, String> {
    authorize(&webview)?;
    Ok(files::open_charter_folder())
}

/// Cómo llega el login al motor: su puerto y el token del header.
fn engine_access(app: &AppHandle) -> Option<login::EngineAccess> {
    let state = app.state::<Shell>();
    let engine = state.engine.lock().unwrap();
    engine.as_ref().map(|e| login::EngineAccess { port: e.port(), token: e.token().expose().to_string() })
}

#[tauri::command]
pub fn desktop_provider_login_start(webview: Webview, provider: String, opts: Option<Value>) -> Result<Value, String> {
    authorize(&webview)?;
    let provider = login::provider(&provider)?;
    let app = webview.app_handle();
    let options = login::LoginOptions::from_value(opts.as_ref().filter(|v| v.is_object()));
    Ok(app.state::<Shell>().logins.start(provider, options, engine_access(app)))
}

#[tauri::command]
pub fn desktop_provider_login_status(webview: Webview, provider: String) -> Result<Value, String> {
    authorize(&webview)?;
    Ok(webview.app_handle().state::<Shell>().logins.status(login::provider(&provider)?))
}

#[tauri::command]
pub fn desktop_provider_login_code(webview: Webview, provider: String, code: Value) -> Result<Value, String> {
    authorize(&webview)?;
    let provider = login::provider(&provider)?;
    let code = code.as_str().ok_or("code must be a string")?;
    webview.app_handle().state::<Shell>().logins.submit_code(provider, code)
}

#[tauri::command]
pub fn desktop_provider_login_cancel(webview: Webview, provider: String) -> Result<Value, String> {
    authorize(&webview)?;
    Ok(webview.app_handle().state::<Shell>().logins.cancel(login::provider(&provider)?))
}
