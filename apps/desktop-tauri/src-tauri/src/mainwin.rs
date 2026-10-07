//! Las ventanas principales (#20): una de inicio (Homepage) o de creación, y
//! una por organización, como `org-windows.ts`, `index.ts` y `window-close.ts`
//! de Electron. Las decisiones viven en `orgwindows.rs` (puro); acá están los
//! efectos: construir, mostrar, enfocar, cerrar, recordar posiciones y
//! entregar eventos.
//!
//! Regla de concurrencia: el lock del registro solo se toma para leer o
//! cambiar datos, nunca mientras se llama una API de ventana. Construir una
//! ventana desde otro hilo espera al hilo de eventos, y ese hilo puede estar
//! esperando el mismo lock en un manejador de eventos.

use crate::orgwindows::{CloseGate, CreationStart, Kind, QuitGate, Routing, LABEL_PREFIX};
use crate::placement::{self, Bounds, Placement};
use crate::{dialog, Shell};
use serde_json::{json, Value};
use std::sync::atomic::Ordering;
use tauri::webview::{PageLoadEvent, PageLoadPayload};
use tauri::{Manager, PhysicalPosition, PhysicalSize, Url, WebviewUrl, WebviewWindow, WebviewWindowBuilder, WindowEvent};

/// Tamaño mínimo del área cliente, como `minWidth`/`minHeight` de Electron.
pub const MIN_SIZE: (u32, u32) = (640, 480);

pub fn is_main(label: &str) -> bool {
    label.starts_with(LABEL_PREFIX)
}

fn shell(app: &tauri::AppHandle) -> &Shell {
    app.state::<Shell>().inner()
}

/// Las ventanas principales vivas, en orden de registro.
pub fn ids(app: &tauri::AppHandle) -> Vec<String> {
    shell(app).windows.lock().unwrap().ids()
}

/// La última ventana usada (bandeja, segunda instancia, clic sin org).
pub fn last_used(app: &tauri::AppHandle) -> Option<WebviewWindow> {
    let id = shell(app).windows.lock().unwrap().last_activated()?;
    app.get_webview_window(&id)
}

pub fn identity(app: &tauri::AppHandle, id: &str) -> Option<Value> {
    shell(app).windows.lock().unwrap().identity(id)
}

fn eval_event(window: &WebviewWindow, event: &Value) {
    let _ = window.eval(format!("window.__orgtreeDesktopDispatch && window.__orgtreeDesktopDispatch({event})"));
}

/// Un evento a UNA ventana. Si su documento todavía no cargó, los tipos que
/// no se pueden volver a pedir quedan retenidos hasta la carga.
pub fn send_to(app: &tauri::AppHandle, id: &str, event: Value) {
    let deliver = shell(app).windows.lock().unwrap().offer(id, &event);
    if deliver {
        if let Some(window) = app.get_webview_window(id) {
            eval_event(&window, &event);
        }
    }
}

/// Un hecho de toda la app (preferencias, orgs abiertas) a cada ventana.
pub fn broadcast(app: &tauri::AppHandle, event: Value) {
    for id in ids(app) {
        send_to(app, &id, event.clone());
    }
}

pub fn window_state(app: &tauri::AppHandle, window: &WebviewWindow) -> Value {
    json!({
        "visible": window.is_visible().unwrap_or(true),
        "restoreWindows": shell(app).restore_windows.load(Ordering::SeqCst),
    })
}

/// `DesktopControlsState`: lo que dibujan los botones de la ventana sin marco.
pub fn controls_state(app: &tauri::AppHandle, window: &WebviewWindow) -> Value {
    let mut state = window_state(app, window);
    state["minimized"] = Value::Bool(window.is_minimized().unwrap_or(false));
    state["maximized"] = Value::Bool(window.is_maximized().unwrap_or(false));
    state
}

fn publish_open_orgs(app: &tauri::AppHandle) {
    let orgs = shell(app).windows.lock().unwrap().open_orgs();
    broadcast(app, json!({ "type": "open-orgs", "data": orgs }));
}

/// La ventana que gana las obligaciones de notificación tiene que saberlo:
/// es lo único que arranca su sondeo.
fn announce_owner(app: &tauri::AppHandle) {
    let moved = {
        let mut windows = shell(app).windows.lock().unwrap();
        windows.reconcile_owner().and_then(|id| windows.identity(&id).map(|identity| (id, identity)))
    };
    if let Some((id, identity)) = moved {
        send_to(app, &id, json!({ "type": "window-identity", "data": identity }));
    }
}

/// La identidad de la ventana cambió (una Homepage pasó a ser una org, o a
/// la vista de creación y de vuelta). No navega: el renderer se mueve solo a la
/// ruta con `history.pushState`, en el mismo documento.
pub fn adopt_identity(app: &tauri::AppHandle, id: &str) {
    let (identity, previous, key) = {
        let mut windows = shell(app).windows.lock().unwrap();
        let Some(entry) = windows.get_mut(id) else { return };
        let key = placement::key_of(entry.kind, entry.org.as_deref());
        let previous = std::mem::replace(&mut entry.placement_key, key.clone());
        (windows.identity(id), previous, key)
    };
    if previous != key {
        // La ventana deja la sesión con su clave vieja y entra con la nueva
        // (la Homepage ahora es una org), con su posición actual.
        if let (Some(previous), Some(store)) = (&previous, shell(app).placement.lock().unwrap().as_mut()) {
            store.closed(previous);
        }
        if let Some(key) = &key {
            capture_placement(app, id);
            if let Some(store) = shell(app).placement.lock().unwrap().as_mut() {
                store.opened(key);
            }
        }
    }
    if let Some(identity) = identity {
        send_to(app, id, json!({ "type": "window-identity", "data": identity }));
    }
    publish_open_orgs(app);
}

/// Restaura, muestra y enfoca una ventana (`revealWindow`), y avisa al
/// renderer que ya se puede restaurar su disposición de popouts.
pub fn reveal(app: &tauri::AppHandle, id: &str) {
    let Some(window) = app.get_webview_window(id) else { return };
    shell(app).restore_windows.store(true, Ordering::SeqCst);
    let _ = window.show();
    if window.is_minimized().unwrap_or(false) {
        let _ = window.unminimize();
    }
    let _ = window.set_focus();
    shell(app).windows.lock().unwrap().activate(id);
    let state = window_state(app, &window);
    send_to(app, id, json!({ "type": "main-window-shown", "data": state }));
}

/// La bandeja y una segunda instancia: la última ventana usada, o una
/// Homepage nueva si no queda ninguna (`showLastUsedOrHomepage`).
pub fn show_last_used_or_homepage(app: &tauri::AppHandle) {
    let last = shell(app).windows.lock().unwrap().last_activated();
    match last {
        Some(id) => reveal(app, &id),
        None => {
            if shell(app).origin.lock().unwrap().is_some() {
                let app = app.clone();
                std::thread::spawn(move || {
                    if let Ok(id) = open_window(&app, Kind::Homepage, None) {
                        reveal(&app, &id);
                    }
                });
            }
        }
    }
}

/// Los rectángulos de trabajo de los monitores actuales, en píxeles físicos.
fn work_areas(app: &tauri::AppHandle) -> Vec<Bounds> {
    app.available_monitors()
        .unwrap_or_default()
        .iter()
        .map(|m| {
            let area = m.work_area();
            Bounds { x: area.position.x, y: area.position.y, width: area.size.width, height: area.size.height }
        })
        .collect()
}

/// Recuerda dónde está una ventana (`savePlacement`). Una ventana minimizada
/// no se guarda; una maximizada guarda solo la marca y conserva su rectángulo normal.
pub fn capture_placement(app: &tauri::AppHandle, id: &str) {
    let Some(window) = app.get_webview_window(id) else { return };
    let key = {
        let windows = shell(app).windows.lock().unwrap();
        windows.get(id).and_then(|e| placement::key_of(e.kind, e.org.as_deref()))
    };
    let Some(key) = key else { return };
    if window.is_minimized().unwrap_or(true) || !window.is_visible().unwrap_or(false) {
        return;
    }
    let maximized = window.is_maximized().unwrap_or(false);
    let bounds = (|| {
        let position = window.outer_position().ok()?;
        let size = window.inner_size().ok()?;
        Some(Bounds { x: position.x, y: position.y, width: size.width, height: size.height })
    })();
    let mut guard = shell(app).placement.lock().unwrap();
    let Some(store) = guard.as_mut() else { return };
    match (maximized, bounds) {
        (true, _) => store.capture_maximized(&key, true),
        (false, Some(bounds)) if bounds.width > 0 && bounds.height > 0 => store.capture(&key, Placement { bounds, maximized: false }),
        _ => {}
    }
}

fn route_for(kind: Kind, org: Option<&str>) -> String {
    match (kind, org) {
        (Kind::Org, Some(org)) => format!("/o/{org}"),
        _ => "/".into(),
    }
}

/// Abre una ventana principal nueva de ese tipo. La registra antes de
/// construirla, así que su shim nace con la identidad correcta.
pub fn open_window(app: &tauri::AppHandle, kind: Kind, org: Option<&str>) -> Result<String, String> {
    let id = {
        let mut windows = shell(app).windows.lock().unwrap();
        let id = windows.next_label();
        windows.register(&id, kind, org)?;
        id
    };
    build(app, &id)?;
    Ok(id)
}

/// Construye la ventana `id`, ya registrada. Si falla, la saca del registro.
pub fn build(app: &tauri::AppHandle, id: &str) -> Result<(), String> {
    let built = build_inner(app, id);
    if built.is_err() {
        shell(app).windows.lock().unwrap().forget(id);
        announce_owner(app);
        publish_open_orgs(app);
    }
    built
}

fn build_inner(app: &tauri::AppHandle, id: &str) -> Result<(), String> {
    let state = shell(app);
    let origin = state.origin.lock().unwrap().clone().ok_or("el motor todavía no está listo")?;
    let (kind, org, identity) = {
        let mut windows = state.windows.lock().unwrap();
        let entry = windows.get(id).ok_or("ventana desconocida")?;
        let (kind, org) = (entry.kind, entry.org.clone());
        (kind, org, windows.identity(id).unwrap_or(Value::Null))
    };
    let url = Url::parse(&format!("{origin}{}", route_for(kind, org.as_deref()))).map_err(|e| e.to_string())?;
    let shim = include_str!("shim.js")
        .replace("__ORIGIN__", &origin)
        .replace("__WINDOW__", id)
        .replace("__IDENTITY__", &identity.to_string());
    let key = placement::key_of(kind, org.as_deref());
    let areas = work_areas(app);
    let saved = key.as_ref().and_then(|key| {
        state.placement.lock().unwrap().as_ref().and_then(|store| store.restore(key, &areas, MIN_SIZE))
    });
    let navigation = app.clone();
    let popouts = app.clone();
    let titles = app.clone();
    let owner = id.to_string();
    let window = WebviewWindowBuilder::new(app, id, WebviewUrl::External(url))
        .title("Orgtree (Tauri spike)")
        .inner_size(1200.0, 800.0)
        .min_inner_size(MIN_SIZE.0 as f64, MIN_SIZE.1 as f64)
        // Sin marco (#7), como Electron (`frame: false`).
        .decorations(false)
        // Oculta hasta ubicarla: así no aparece primero en el lugar por defecto.
        .visible(false)
        .initialization_script(shim)
        .on_navigation(move |url| crate::navigation_allowed(&navigation, url))
        .on_new_window(move |url, features| crate::open_popout(&popouts, &owner, &url, features))
        .on_page_load(on_page_load)
        .on_document_title_changed(move |window, title| crate::on_title(&titles, &window, &title))
        .build()
        .map_err(|e| format!("ventana {id}: {e}"))?;
    if let Some(saved) = saved {
        let b = saved.bounds;
        let _ = window.set_size(PhysicalSize::new(b.width, b.height));
        let _ = window.set_position(PhysicalPosition::new(b.x, b.y));
        if saved.maximized {
            let _ = window.maximize();
        }
    }
    let events = app.clone();
    let label = id.to_string();
    window.on_window_event(move |event| on_window_event(&events, &label, event));
    if let Some(entry) = state.windows.lock().unwrap().get_mut(id) {
        entry.placement_key = key.clone();
    }
    if let Some(key) = &key {
        if let Some(store) = state.placement.lock().unwrap().as_mut() {
            store.opened(key);
        }
    }
    // Lo que esperaba a esta org (un clic en una notificación que la abrió).
    if let Some(org) = &org {
        let waiting = state.windows.lock().unwrap().take_reveals(org);
        for event in waiting {
            send_to(app, id, event);
        }
    }
    announce_owner(app);
    publish_open_orgs(app);
    if !state.background {
        reveal(app, id);
    }
    // La primera vez sin posición guardada: se registra la del sistema.
    if saved.is_none() {
        capture_placement(app, id);
    }
    Ok(())
}

/// Carga de documento de una ventana principal: retiene los eventos mientras
/// navega y entrega lo retenido al terminar. Además cierra la ventana de
/// arranque y corre las pruebas del CI.
fn on_page_load(window: WebviewWindow, payload: PageLoadPayload<'_>) {
    let app = window.app_handle().clone();
    let id = window.label().to_string();
    match payload.event() {
        PageLoadEvent::Started => shell(&app).windows.lock().unwrap().unloaded(&id),
        PageLoadEvent::Finished => {
            let held = shell(&app).windows.lock().unwrap().loaded(&id);
            if !held.is_empty() {
                let types: Vec<&Value> = held.iter().map(|e| &e["type"]).collect();
                crate::record_windows_log(&app, "held", json!({ "window": id, "delivered": types }));
            }
            for event in held {
                eval_event(&window, &event);
            }
            // La ventana de arranque se cierra recién cuando una principal cargó:
            // cerrarla antes deja a la app sin ventanas y la termina.
            if let Some(splash) = app.get_webview_window("splash") {
                let _ = splash.close();
            }
            crate::on_main_loaded(&app, &window);
        }
    }
}

fn on_window_event(app: &tauri::AppHandle, id: &str, event: &WindowEvent) {
    match event {
        WindowEvent::CloseRequested { api, .. } => {
            if !perform_close(app, id) {
                api.prevent_close();
            }
        }
        WindowEvent::Destroyed => {
            let gone = shell(app).windows.lock().unwrap().forget(id);
            shell(app).popout_owner.lock().unwrap().retain(|_, owner| owner != id);
            if gone.is_some() {
                announce_owner(app);
                publish_open_orgs(app);
            }
        }
        WindowEvent::Moved(_) | WindowEvent::Resized(_) => {
            capture_placement(app, id);
            publish_window_state(app, id);
        }
        WindowEvent::Focused(focused) => {
            if *focused {
                shell(app).windows.lock().unwrap().activate(id);
            }
            publish_window_state(app, id);
        }
        _ => {}
    }
}

/// Los botones propios de la ventana sin marco siguen el estado real.
pub fn publish_window_state(app: &tauri::AppHandle, id: &str) {
    if let Some(window) = app.get_webview_window(id) {
        let state = controls_state(app, &window);
        send_to(app, id, json!({ "type": "window-state", "data": state }));
    }
}

pub fn hwnd_of(window: &WebviewWindow) -> Option<isize> {
    #[cfg(windows)]
    return window.hwnd().ok().map(|h| h.0 as isize);
    #[cfg(not(windows))]
    {
        let _ = window;
        None
    }
}

/// La pregunta de descarte, en un hilo propio (es modal y bloquea).
/// `then` recibe la respuesta.
fn ask_discard(app: &tauri::AppHandle, id: &str, then: impl FnOnce(bool) + Send + 'static) {
    let owner = app.get_webview_window(id).as_ref().and_then(hwnd_of);
    let app = app.clone();
    let id = id.to_string();
    std::thread::spawn(move || {
        crate::record_windows_log(&app, "dialog", json!({ "window": id, "phase": "shown" }));
        let discard = dialog::confirm_discard(owner);
        crate::record_windows_log(&app, "dialog", json!({ "window": id, "phase": "answered", "discard": discard }));
        then(discard);
    });
}

/// Qué pasa cuando se pide cerrar una ventana principal (`performClose`).
/// Devuelve `true` si el cierre sigue. Es la única función que desarma la
/// ventana (`teardown`), así que un cierre rechazado no toca nada.
fn perform_close(app: &tauri::AppHandle, id: &str) -> bool {
    let state = shell(app);
    let quitting = state.quitting.load(Ordering::SeqCst);
    if !quitting {
        let gate = state.windows.lock().unwrap().begin_close(id);
        match gate {
            CloseGate::Awaiting => return false,
            CloseGate::Confirm => {
                let app2 = app.clone();
                let label = id.to_string();
                ask_discard(app, id, move |discard| {
                    shell(&app2).windows.lock().unwrap().settle_close(&label, discard);
                    if discard {
                        if let Some(window) = app2.get_webview_window(&label) {
                            let _ = window.close();
                        }
                    }
                });
                return false;
            }
            CloseGate::Close => {}
        }
    }
    let others: Vec<WebviewWindow> = app.webview_windows().into_iter().filter(|(label, _)| label != id && label != "splash").map(|(_, w)| w).collect();
    let other_mains_visible = others.iter().any(|w| is_main(w.label()) && w.is_visible().unwrap_or(false));
    // Cerrar una de varias la cierra. Solo la última consulta `exitOnClose`.
    if !other_mains_visible {
        let other_views = others.iter().filter(|w| w.is_visible().unwrap_or(false)).count();
        let exit_on_close = crate::exit_on_close(app);
        if !quitting {
            if exit_on_close && other_views == 0 {
                request_quit(app);
            } else if let Some(window) = app.get_webview_window(id) {
                // Se queda en la bandeja; los popouts siguen vivos en su contexto de JS.
                let _ = window.hide();
                publish_window_state(app, id);
            }
            return false;
        }
    }
    // teardown: la ventana deja la sesión (salvo en la salida) y cierra SUS popouts.
    let key = {
        let windows = state.windows.lock().unwrap();
        windows.get(id).and_then(|e| placement::key_of(e.kind, e.org.as_deref()))
    };
    capture_placement(app, id);
    if let (Some(key), false) = (key, quitting) {
        if let Some(store) = state.placement.lock().unwrap().as_mut() {
            store.closed(&key);
        }
    }
    let owned: Vec<String> = state.popout_owner.lock().unwrap().iter().filter(|(_, owner)| *owner == id).map(|(p, _)| p.clone()).collect();
    for label in owned {
        if let Some(popout) = app.get_webview_window(&label) {
            let _ = popout.destroy();
        }
    }
    true
}

/// Salir de la app (bandeja, `quit`, el último cierre con `exitOnClose`).
/// Antes pregunta por cada formulario de creación sin guardar; un "seguir
/// editando" cancela la salida entera y deja todo como estaba.
pub fn request_quit(app: &tauri::AppHandle) {
    let state = shell(app);
    if state.quitting.load(Ordering::SeqCst) || state.quit_prompting.swap(true, Ordering::SeqCst) {
        return;
    }
    let app = app.clone();
    std::thread::spawn(move || {
        let proceed = confirm_before_quit(&app);
        shell(&app).quit_prompting.store(false, Ordering::SeqCst);
        if proceed {
            shutdown(&app);
        }
    });
}

fn confirm_before_quit(app: &tauri::AppHandle) -> bool {
    let gate = shell(app).windows.lock().unwrap().quit_gate();
    let ids = match gate {
        QuitGate::Proceed => return true,
        QuitGate::Busy => return false,
        QuitGate::Confirm(ids) => ids,
    };
    let mut agreed = Vec::new();
    for id in ids {
        if shell(app).windows.lock().unwrap().begin_close(&id) != CloseGate::Confirm {
            return false;
        }
        let owner = app.get_webview_window(&id).as_ref().and_then(hwnd_of);
        crate::record_windows_log(app, "dialog", json!({ "window": id, "phase": "shown", "quit": true }));
        let discard = dialog::confirm_discard(owner);
        crate::record_windows_log(app, "dialog", json!({ "window": id, "phase": "answered", "discard": discard, "quit": true }));
        // Cierra la pregunta sin registrar todavía el descarte.
        shell(app).windows.lock().unwrap().settle_close(&id, false);
        if !discard {
            return false;
        }
        agreed.push(id);
    }
    let mut windows = shell(app).windows.lock().unwrap();
    for id in agreed {
        windows.set_unsaved(&id, false);
    }
    true
}

/// La salida en sí: guarda las ventanas abiertas ANTES de cerrar nada, deja
/// que cada renderer guarde su disposición y termina.
pub fn shutdown(app: &tauri::AppHandle) {
    let state = shell(app);
    if state.quitting.swap(true, Ordering::SeqCst) {
        return;
    }
    save_session(app);
    for id in ids(app) {
        if let Some(window) = app.get_webview_window(&id) {
            let _ = window.eval("window.dispatchEvent(new Event('orgtree:before-exit'))");
        }
    }
    std::thread::sleep(std::time::Duration::from_millis(300));
    app.exit(0);
}

/// Las ventanas abiertas ahora, en orden, como la sesión que se reabre.
pub fn save_session(app: &tauri::AppHandle) {
    let ids = ids(app);
    for id in &ids {
        capture_placement(app, id);
    }
    let keys: Vec<String> = {
        let windows = shell(app).windows.lock().unwrap();
        ids.iter().filter_map(|id| windows.get(id).and_then(|e| placement::key_of(e.kind, e.org.as_deref()))).collect()
    };
    if let Some(store) = shell(app).placement.lock().unwrap().as_mut() {
        store.begin_shutdown(&keys);
    }
}

/// `requestOrg` de punta a punta: enfoca, liga la ventana que pidió o
/// construye una nueva, y devuelve el `OrgOpenOutcome` ya cumplido.
pub fn request_org(app: &tauri::AppHandle, org: &str, caller: Option<&str>) -> Value {
    let routing = shell(app).windows.lock().unwrap().request_org(org, caller, |r| r.next_label());
    match &routing {
        Routing::Focused { window, org } => {
            reveal(app, window);
            deliver_reveals(app, window, org);
        }
        Routing::Bound { window, org } => {
            adopt_identity(app, window);
            reveal(app, window);
            deliver_reveals(app, window, org);
        }
        Routing::Open { window, .. } => {
            if let Err(error) = build(app, window) {
                return json!({ "action": "refused", "org": org, "reason": format!("could not open the window: {error}") });
            }
        }
        Routing::Pending { .. } | Routing::Refused { .. } => {}
    }
    routing.outcome()
}

fn deliver_reveals(app: &tauri::AppHandle, id: &str, org: &str) {
    let waiting = shell(app).windows.lock().unwrap().take_reveals(org);
    for event in waiting {
        send_to(app, id, event);
    }
}

/// Revela un elemento en la ventana de SU organización (el clic en una
/// notificación): la abre o la enfoca primero, y si todavía no existe el
/// evento espera a que cargue.
pub fn reveal_org_item(app: &tauri::AppHandle, org: &str, event: Value) {
    let target = shell(app).windows.lock().unwrap().queue_reveal(org, event.clone());
    match target {
        Some(id) => {
            reveal(app, &id);
            send_to(app, &id, event);
        }
        None => {
            let _ = request_org(app, org, None);
        }
    }
}

/// "Crear organización" desde la ventana `id`: una Homepage pasa a la vista
/// de creación; una ventana con una org abre una ventana de creación aparte.
pub fn begin_creation(app: &tauri::AppHandle, id: &str) -> Result<Value, String> {
    let start = shell(app).windows.lock().unwrap().start_creation(id);
    match start {
        CreationStart::Switched => {
            adopt_identity(app, id);
            Ok(identity(app, id).unwrap_or(Value::Null))
        }
        CreationStart::AlreadyCreating => Ok(identity(app, id).unwrap_or(Value::Null)),
        CreationStart::Open => {
            let created = open_window(app, Kind::Create, None)?;
            reveal(app, &created);
            Ok(identity(app, &created).unwrap_or(Value::Null))
        }
    }
}

/// Cancelar la creación: la que empezó en una Homepage vuelve a ella después
/// de la misma pregunta que un cierre; cualquier otra se cierra.
pub fn cancel_creation(app: &tauri::AppHandle, id: &str) -> &'static str {
    let return_home = shell(app).windows.lock().unwrap().get(id).is_some_and(|e| e.return_home);
    if !return_home {
        if let Some(window) = app.get_webview_window(id) {
            let _ = window.close();
        }
        return "close";
    }
    let gate = shell(app).windows.lock().unwrap().begin_close(id);
    match gate {
        CloseGate::Awaiting => return "kept",
        CloseGate::Confirm => {
            let owner = app.get_webview_window(id).as_ref().and_then(hwnd_of);
            crate::record_windows_log(app, "dialog", json!({ "window": id, "phase": "shown", "cancel": true }));
            let discard = dialog::confirm_discard(owner);
            crate::record_windows_log(app, "dialog", json!({ "window": id, "phase": "answered", "discard": discard, "cancel": true }));
            shell(app).windows.lock().unwrap().settle_close(id, discard);
            if !discard {
                return "kept";
            }
        }
        CloseGate::Close => {}
    }
    if !shell(app).windows.lock().unwrap().return_home(id) {
        return "kept";
    }
    adopt_identity(app, id);
    "home"
}

/// La creación terminó: la ventana de creación pasa a ser la org nueva.
pub fn bind_created(app: &tauri::AppHandle, id: &str, org: &str) -> Value {
    let routing = shell(app).windows.lock().unwrap().bind_created(id, org);
    if let Routing::Bound { .. } = routing {
        adopt_identity(app, id);
    }
    routing.outcome()
}

/// Lo que abre un arranque normal: las ventanas de la última sesión (con
/// `startupMode` = `restore`), en sus posiciones, o una Homepage.
///
/// Una ventana guardada cuya org ya no existe se abre igual, como ventana de
/// esa org en estado de error (decisión del usuario, 2026-09-21): nada
/// distingue una org borrada de una que no se puede leer ahora, y saltearla
/// tiraría una ventana que la persona había dejado. Solo se saltea un
/// registro dañado (un slug inválido), y se avisa con `restore-skipped`.
pub fn open_startup_windows(app: &tauri::AppHandle) -> Result<String, String> {
    let homepage_only = shell(app).preferences.lock().unwrap().get()["startupMode"] == "homepage";
    let session = if homepage_only {
        Vec::new()
    } else {
        shell(app).placement.lock().unwrap().as_ref().map(|s| s.session()).unwrap_or_default()
    };
    let mut plan: Vec<(Kind, Option<String>)> = Vec::new();
    let mut skipped = Vec::new();
    for key in session {
        match placement::org_of_key(&key) {
            None if key == placement::HOMEPAGE_KEY => plan.push((Kind::Homepage, None)),
            Some(org) if crate::orgwindows::is_org_slug(org) => plan.push((Kind::Org, Some(org.to_string()))),
            Some(org) => skipped.push(org.to_string()),
            None => skipped.push(key.clone()),
        }
    }
    let mut opened = Vec::new();
    for (kind, org) in &plan {
        match open_window(app, *kind, org.as_deref()) {
            Ok(id) => opened.push(id),
            Err(error) => {
                #[cfg(debug_assertions)]
                eprintln!("[shell] no se pudo reabrir {org:?}: {error}");
                let _ = error;
            }
        }
    }
    let first = match opened.first() {
        Some(first) => first.clone(),
        None => open_window(app, Kind::Homepage, None)?,
    };
    if !skipped.is_empty() {
        let notice = format!(
            "Orgtree could not reopen {} from the last session, because the record was damaged ({}).",
            if skipped.len() == 1 { "a saved window".to_string() } else { format!("{} saved windows", skipped.len()) },
            skipped.join(", ")
        );
        send_to(app, &first, json!({ "type": "restore-skipped", "data": { "orgs": skipped, "panels": [], "notice": notice } }));
    }
    if !shell(app).background {
        reveal(app, &first);
    }
    crate::record_windows_log(app, "startup", json!({ "plan": plan.iter().map(|(k, o)| json!({ "kind": k.as_str(), "org": o })).collect::<Vec<_>>(), "opened": opened, "skipped": skipped, "first": first }));
    Ok(first)
}

/// El estado de las ventanas para la prueba: identidad, posición y tamaño.
pub fn report(app: &tauri::AppHandle) -> Value {
    let ids = ids(app);
    let rows: Vec<Value> = ids
        .iter()
        .map(|id| {
            let window = app.get_webview_window(id);
            let position = window.as_ref().and_then(|w| w.outer_position().ok());
            let size = window.as_ref().and_then(|w| w.inner_size().ok());
            json!({
                "id": id,
                "identity": identity(app, id),
                "x": position.map(|p| p.x), "y": position.map(|p| p.y),
                "width": size.map(|s| s.width), "height": size.map(|s| s.height),
                "visible": window.as_ref().and_then(|w| w.is_visible().ok()),
                "focused": window.as_ref().and_then(|w| w.is_focused().ok()),
                "maximized": window.as_ref().and_then(|w| w.is_maximized().ok()),
                "hwnd": window.as_ref().and_then(hwnd_of),
                "url": window.as_ref().and_then(|w| w.url().ok()).map(|u| u.path().to_string()),
            })
        })
        .collect();
    json!({ "windows": rows, "areas": work_areas(app) })
}
