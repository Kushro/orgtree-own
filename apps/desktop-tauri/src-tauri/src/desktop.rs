//! Comandos detrás del shim de `window.orgtreeDesktop` (`shim.js`).
//!
//! Cada comando resuelve QUÉ ventana principal lo llama (su etiqueta, que
//! tiene que estar registrada) y verifica que su página está en el origen
//! exacto del motor, como `resolveNativeSender` en Electron. Los comandos de
//! ventana actúan siempre sobre esa ventana: un id que mandara la página
//! dejaría a una org manejar la ventana de otra. Además, la capability
//! `engine-ui` solo deja invocar estos comandos desde `win-*` en `127.0.0.1`.

use crate::mainwin;
use crate::notifications::{self, Decision, Notice, Pulse};
use crate::{autostart, files, harnesses, login, origin_of, Shell};
use serde_json::{json, Map, Value};
use tauri::{AppHandle, Manager, Webview};

/// La ventana principal que llama, o un rechazo.
pub(crate) fn authorize(webview: &Webview) -> Result<String, String> {
    let app = webview.app_handle();
    let engine = app.state::<Shell>().origin.lock().unwrap().clone();
    let url = webview.url().map_err(|e| e.to_string())?;
    let label = webview.label();
    let registered = mainwin::is_main(label) && app.state::<Shell>().windows.lock().unwrap().get(label).is_some();
    if registered && engine.is_some_and(|origin| origin == origin_of(&url)) {
        Ok(label.to_string())
    } else {
        Err("orgtreeDesktop: solo una ventana principal en el origen del motor".into())
    }
}

fn caller_window(webview: &Webview) -> Result<(String, tauri::WebviewWindow), String> {
    let id = authorize(webview)?;
    let window = webview.app_handle().get_webview_window(&id).ok_or("sin ventana principal")?;
    Ok((id, window))
}

#[tauri::command]
pub fn desktop_app_version(webview: Webview, app: AppHandle) -> Result<String, String> {
    authorize(&webview)?;
    Ok(app.package_info().version.to_string())
}

#[tauri::command]
pub fn desktop_status(webview: Webview) -> Result<Value, String> {
    authorize(&webview)?;
    // #19: el `EngineStatus` que sigue el ciclo de vida (y que emite `engine-status`).
    Ok(crate::lifecycle::status(webview.app_handle()))
}

#[tauri::command]
pub fn desktop_window_state(webview: Webview) -> Result<Value, String> {
    let (_, window) = caller_window(&webview)?;
    Ok(mainwin::window_state(webview.app_handle(), &window))
}

#[tauri::command]
pub fn desktop_window_controls_state(webview: Webview) -> Result<Value, String> {
    let (_, window) = caller_window(&webview)?;
    Ok(mainwin::controls_state(webview.app_handle(), &window))
}

#[tauri::command]
pub fn desktop_preferences(webview: Webview) -> Result<Value, String> {
    authorize(&webview)?;
    Ok(webview.app_handle().state::<Shell>().preferences.lock().unwrap().get())
}

/// Aplica un cambio de preferencias, lo guarda y lo anuncia a todas las
/// ventanas. Lo usan el puente y el menú de la bandeja.
pub fn set_preferences(app: &AppHandle, patch: &Value) -> Result<Value, String> {
    let state = app.state::<Shell>();
    let (before, updated) = {
        let mut preferences = state.preferences.lock().unwrap();
        let before = preferences.bool("startAtLogin");
        (before, preferences.set(patch)?)
    };
    // La resolución de tema del renderer manda: el tema efectivo anterior se descarta.
    *state.effective_theme.lock().unwrap() = None;
    let start = updated["startAtLogin"] == true;
    if start != before || patch.get("startAtLogin").is_some() {
        if let Err(error) = autostart::apply(start) {
            crate::record_windows_log(app, "autostart", json!({ "error": error }));
        }
    }
    // Un tipo de notificación que se apagó retira sus toasts (`configure`).
    for (tag, _) in state.notifications.configure(&updated) {
        remove_toast(app, &tag);
    }
    crate::refresh_tray_checks(app);
    mainwin::broadcast(app, json!({ "type": "preferences", "data": updated }));
    Ok(updated)
}

#[tauri::command]
pub fn desktop_set_preferences(webview: Webview, patch: Value) -> Result<Value, String> {
    authorize(&webview)?;
    set_preferences(webview.app_handle(), &patch)
}

/// El tema que el renderer resolvió (`setEffectiveTheme`). Electron lo usa
/// para el ícono de la bandeja; acá queda en el shell para la bandeja y la
/// prueba, hasta el próximo cambio de preferencias.
#[tauri::command]
pub fn desktop_set_effective_theme(webview: Webview, theme: Value) -> Result<(), String> {
    authorize(&webview)?;
    if !crate::preferences::is_visual_theme(&theme) {
        return Err("Unknown visual theme".into());
    }
    let app = webview.app_handle();
    *app.state::<Shell>().effective_theme.lock().unwrap() = theme.as_str().map(String::from);
    crate::refresh_tray_checks(app);
    Ok(())
}

#[tauri::command]
pub fn desktop_show(webview: Webview) -> Result<(), String> {
    let id = authorize(&webview)?;
    mainwin::reveal(webview.app_handle(), &id);
    Ok(())
}

#[tauri::command]
pub fn desktop_quit(webview: Webview) -> Result<(), String> {
    authorize(&webview)?;
    crate::lifecycle::note_exit_trigger(webview.app_handle(), "renderer");
    mainwin::request_quit(webview.app_handle());
    Ok(())
}

#[tauri::command]
pub fn desktop_window_minimize(webview: Webview) -> Result<(), String> {
    caller_window(&webview)?.1.minimize().map_err(|e| e.to_string())
}

#[tauri::command]
pub fn desktop_window_toggle_maximize(webview: Webview) -> Result<(), String> {
    let (_, window) = caller_window(&webview)?;
    let result = if window.is_maximized().unwrap_or(false) { window.unmaximize() } else { window.maximize() };
    result.map_err(|e| e.to_string())
}

#[tauri::command]
pub fn desktop_window_close(webview: Webview) -> Result<(), String> {
    caller_window(&webview)?.1.close().map_err(|e| e.to_string())
}

// ------------------------------------------------------------------ #20

/// La identidad de la ventana que llama (`getWindowIdentity`).
#[tauri::command]
pub fn desktop_window_identity(webview: Webview) -> Result<Value, String> {
    let id = authorize(&webview)?;
    Ok(mainwin::identity(webview.app_handle(), &id).unwrap_or(Value::Null))
}

// Los comandos que crean ventanas son `async`: construir una ventana desde un
// comando sincrónico traba el hilo de eventos en Windows.

#[tauri::command]
pub async fn desktop_open_homepage_window(webview: Webview) -> Result<Value, String> {
    authorize(&webview)?;
    let app = webview.app_handle();
    let id = mainwin::open_window(app, crate::orgwindows::Kind::Homepage, None)?;
    mainwin::reveal(app, &id);
    Ok(mainwin::identity(app, &id).unwrap_or(Value::Null))
}

#[tauri::command]
pub async fn desktop_open_create_window(webview: Webview) -> Result<Value, String> {
    let id = authorize(&webview)?;
    mainwin::begin_creation(webview.app_handle(), &id)
}

#[tauri::command]
pub async fn desktop_cancel_creation(webview: Webview) -> Result<Value, String> {
    let id = authorize(&webview)?;
    Ok(Value::String(mainwin::cancel_creation(webview.app_handle(), &id).into()))
}

#[tauri::command]
pub async fn desktop_request_org(webview: Webview, org: Value) -> Result<Value, String> {
    let id = authorize(&webview)?;
    Ok(mainwin::request_org(webview.app_handle(), org.as_str().unwrap_or(""), Some(&id)))
}

#[tauri::command]
pub fn desktop_bind_created_org(webview: Webview, org: Value) -> Result<Value, String> {
    let id = authorize(&webview)?;
    Ok(mainwin::bind_created(webview.app_handle(), &id, org.as_str().unwrap_or("")))
}

#[tauri::command]
pub fn desktop_set_unsaved_creation(webview: Webview, dirty: Value) -> Result<(), String> {
    let id = authorize(&webview)?;
    let app = webview.app_handle();
    app.state::<Shell>().windows.lock().unwrap().set_unsaved(&id, dirty == Value::Bool(true));
    crate::record_windows_log(app, "unsaved", json!({ "window": id, "dirty": dirty }));
    Ok(())
}

#[tauri::command]
pub fn desktop_open_orgs(webview: Webview) -> Result<Value, String> {
    authorize(&webview)?;
    Ok(json!(webview.app_handle().state::<Shell>().windows.lock().unwrap().open_orgs()))
}

/// Lo retenido para este documento que todavía no se entregó. El shell lo
/// entrega solo al terminar la carga, así que en general vuelve vacío; el
/// shim, además, guarda los retenidos hasta el primer listener.
#[tauri::command]
pub fn desktop_take_pending_events(webview: Webview) -> Result<Value, String> {
    let id = authorize(&webview)?;
    let held = webview.app_handle().state::<Shell>().windows.lock().unwrap().loaded(&id);
    Ok(Value::Array(held))
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

/// El clic en un toast: abre o enfoca la ventana de SU organización y le
/// entrega `notification-click`, que abre el elemento (`useNativeNotifications`).
/// Si la ventana de esa org no existe, se abre y el evento espera a que cargue
/// (`revealOrgItem`). Un toast cuyo elemento ya se resolvió no hace nada.
pub(crate) fn notification_activated(app: &AppHandle, tag: &str) -> bool {
    let Some(notice) = app.state::<Shell>().notifications.clicked(tag) else { return false };
    let org = notice.data.get("org").and_then(Value::as_str).unwrap_or_default().to_string();
    mainwin::reveal_org_item(app, &org, json!({ "type": "notification-click", "data": notice.data }));
    true
}

/// Las tres escrituras globales de notificaciones (`notify`, `sync` y la barra
/// de tareas) solo las hace la ventana dueña (`handleOwner`); las demás se
/// ignoran sin error, como en Electron.
fn owner(app: &AppHandle, id: &str) -> bool {
    app.state::<Shell>().windows.lock().unwrap().is_owner(id)
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
pub fn desktop_notify(webview: Webview, notification: Map<String, Value>) -> Result<Value, String> {
    let id = authorize(&webview)?;
    let app = webview.app_handle();
    if !owner(app, &id) {
        crate::record_log(app, "notify-refused", json!({ "window": id }));
        return Ok(Value::Null);
    }
    let notice = notifications::validate(&notification)?;
    let state = app.state::<Shell>();
    let preferences = state.preferences.lock().unwrap().get();
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
    Ok(Value::Bool(shown))
}

/// `syncNotifications(active)`: retira los toasts cuyo elemento ya no está pendiente.
#[tauri::command]
pub fn desktop_sync_notifications(webview: Webview, active: Value) -> Result<(), String> {
    let id = authorize(&webview)?;
    let app = webview.app_handle();
    if !owner(app, &id) {
        return Ok(());
    }
    let active = notifications::identities(&active)?;
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
///
/// Parpadea la ventana de la org del primer elemento, o la última usada si
/// esa org no tiene ventana; nunca todas (usuario, 2026-09-21).
#[tauri::command]
pub fn desktop_pending_attention(webview: Webview, ids: Value, items: Option<Value>) -> Result<bool, String> {
    use tauri::UserAttentionType;
    let caller = authorize(&webview)?;
    let app = webview.app_handle();
    if !owner(app, &caller) {
        return Ok(false);
    }
    let ids = notifications::attention_payload(&ids, items.as_ref())?;
    let state = app.state::<Shell>();
    let count = ids.len();
    let pulse = state.attention.lock().unwrap().set(ids);
    let org = items.as_ref().and_then(|v| v.get(0)).and_then(|row| row.get("org")).and_then(Value::as_str).map(String::from);
    let target = org.and_then(|org| state.windows.lock().unwrap().holder(&org).map(|e| e.id.clone()));
    let window = match target.and_then(|id| app.get_webview_window(&id)) {
        Some(window) => window,
        None => mainwin::last_used(app).ok_or("sin ventana principal")?,
    };
    let focused = window.is_focused().unwrap_or(false) && !window.is_minimized().unwrap_or(false);
    let started = match pulse {
        Pulse::Start if !focused => {
            window.request_user_attention(Some(UserAttentionType::Critical)).map_err(|e| e.to_string())?;
            true
        }
        Pulse::Stop => {
            for id in mainwin::ids(app) {
                if let Some(window) = app.get_webview_window(&id) {
                    let _ = window.request_user_attention(None);
                }
            }
            false
        }
        _ => false,
    };
    crate::record_log(app, "attention", json!({ "ids": count, "pulse": format!("{pulse:?}"), "started": started, "focused": focused, "window": window.label() }));
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
