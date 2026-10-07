//! Notificaciones nativas y parpadeo de la barra de tareas (#28), con la
//! lógica de la sincronización del renderer (`useNativeNotifications` en
//! `renderer/src/notifications.ts`) y del lado nativo de Electron
//! (`apps/desktop/main/notifications.ts`, `taskbar-attention.ts`):
//!
//! - **Una sola dueña.** La pasada global (leer `/api/desktop/notifications`,
//!   la barra de tareas, retirar y mostrar) corre en la ventana principal; las
//!   ventanas de desk no la repiten, como `owner` en el renderer.
//! - **Preferencias por tipo** (`packages/contracts/notifications.ts`), en
//!   `preferences.json` de la carpeta propia de la app.
//! - **Deduplicación** por `org` + `id`; lo que ya se mostró se recuerda en
//!   `notifications-seen.json` mientras siga pendiente, para que reiniciar la
//!   app no repita alertas.
//! - **Retirar** las resueltas: lo que sale de la proyección (o de las
//!   preferencias) se saca del centro de notificaciones.
//! - **El clic** abre el elemento. `notify-rust` no da el clic en Windows: el
//!   toast se arma con WinRT directo, como el spike de Tauri (#21), con tag y
//!   grupo propios para poder retirarlo y el evento `Activated`.
//! - **La barra de tareas** parpadea con cada llegada nueva que nadie está
//!   mirando, y para cuando no queda nada pendiente.

use dioxus::prelude::*;
use orgtree_engine_client::{Client, DesktopNotice};
use serde_json::{Map, Value};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;

/// `NOTIFICATION_OPTIONS`: clave, etiqueta y valor por defecto.
pub const OPTIONS: [(&str, &str, bool); 8] = [
    ("notifyQuestions", "Questions", true),
    ("notifyUrgentMail", "Urgent mail", true),
    ("notifyTerminalFailures", "Terminal failures", true),
    ("notifyDocketAttention", "Docket attention", true),
    ("notifyAllMail", "All mail", false),
    ("notifyDocuments", "New presented document", false),
    ("notifyFrozen", "Agent frozen", false),
    ("notifyWhileFocused", "Notify while Orgtree is focused", false),
];

/// Cada cuánto se relee la proyección sin que nada avise (el reloj nativo de
/// Electron); un frame del WebSocket de una org abierta la relee antes.
const POLL_EVERY: Duration = Duration::from_secs(5);

// ─── Preferencias ─────────────────────────────────────────────────────────────

/// `notificationPreferences(value)`: los valores guardados sobre los de
/// fábrica, con el `routineNotifications` viejo como "todo el correo".
pub fn normalize(value: &Value) -> Value {
    let mut out = Map::new();
    out.insert("notificationsEnabled".into(), Value::Bool(value.get("notificationsEnabled").and_then(Value::as_bool).unwrap_or(true)));
    for (key, _, default) in OPTIONS {
        let v = match value.get(key).and_then(Value::as_bool) {
            Some(v) => v,
            None if key == "notifyAllMail" => value.get("routineNotifications") == Some(&Value::Bool(true)),
            None => default,
        };
        out.insert(key.into(), Value::Bool(v));
    }
    Value::Object(out)
}

fn pref(prefs: &Value, key: &str) -> bool {
    prefs.get(key).and_then(Value::as_bool).unwrap_or(false)
}

/// `notificationEnabled(kind, prefs)`.
pub fn enabled(kind: &str, prefs: &Value) -> bool {
    if !pref(prefs, "notificationsEnabled") {
        return false;
    }
    match kind {
        "question" => pref(prefs, "notifyQuestions"),
        "urgent-mail" => pref(prefs, "notifyUrgentMail") || pref(prefs, "notifyAllMail"),
        "terminal-failure" => prefs.get("notifyTerminalFailures") != Some(&Value::Bool(false)),
        "work-attention" => pref(prefs, "notifyDocketAttention"),
        "routine" => pref(prefs, "notifyAllMail"),
        "document" => pref(prefs, "notifyDocuments"),
        "agent-frozen" => pref(prefs, "notifyFrozen"),
        _ => false,
    }
}

fn file(name: &str) -> Option<PathBuf> {
    crate::app_dir().map(|dir| dir.join(name))
}

static PREFS: Mutex<Option<Value>> = Mutex::new(None);

/// Las preferencias en uso (se leen una vez del archivo).
pub fn prefs() -> Value {
    let mut cached = PREFS.lock().unwrap();
    cached
        .get_or_insert_with(|| {
            let saved = file("preferences.json")
                .and_then(|path| std::fs::read(path).ok())
                .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
                .unwrap_or(Value::Null);
            normalize(&saved)
        })
        .clone()
}

/// `setPreferences(patch)`: guarda, retira lo de un tipo que se apagó
/// (`configure` en Electron) y relee la proyección.
pub fn set_prefs(patch: &Map<String, Value>) -> Value {
    let mut next = prefs();
    if let Some(map) = next.as_object_mut() {
        for (key, value) in patch {
            if value.is_boolean() && (key == "notificationsEnabled" || OPTIONS.iter().any(|(k, _, _)| k == key)) {
                map.insert(key.clone(), value.clone());
            }
        }
    }
    *PREFS.lock().unwrap() = Some(next.clone());
    if let Some(path) = file("preferences.json") {
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(path, serde_json::to_vec_pretty(&next).unwrap_or_default());
    }
    let removed = STATE.lock().unwrap().configure(&next);
    retract(&removed);
    log("configure", serde_json::json!({ "prefs": next, "removed": removed_json(&removed) }));
    bump();
    next
}

// ─── Estado: deduplicación, conjunto activo y barra de tareas ─────────────────

fn identity(org: &str, id: &str) -> String {
    format!("{org}\u{0}{id}")
}

fn key_of(n: &DesktopNotice) -> String {
    identity(&n.org, &n.id)
}

struct Shown {
    kind: String,
    tag: String,
}

/// Un toast retirado: su tag, y el id y el tipo de su notificación.
#[derive(Debug, PartialEq)]
struct Removed {
    tag: String,
    id: String,
    kind: String,
}

fn removed_json(removed: &[Removed]) -> Value {
    Value::Array(removed.iter().map(|r| serde_json::json!({ "id": r.id, "kind": r.kind })).collect())
}

/// Qué decide `decide` antes de mostrar nada.
#[derive(Debug, PartialEq)]
pub enum Decision {
    Show { tag: String },
    Skip(&'static str),
}

/// Qué hacer con la barra de tareas.
#[derive(Debug, PartialEq, Clone, Copy)]
pub enum Pulse {
    Start,
    Stop,
    Keep,
}

/// `NativeNotifications` + `NotificationGate` + `TaskbarAttention`.
#[derive(Default)]
struct State {
    /// Lo que ya llegó al usuario (mostrado, o visible en pantalla), mientras siga pendiente.
    seen: HashSet<String>,
    seen_loaded: bool,
    /// Lo que la última sincronización dejó elegible.
    active: Option<HashSet<String>>,
    shown: HashMap<String, Shown>,
    by_tag: HashMap<String, DesktopNotice>,
    sequence: u64,
    /// Lo pendiente en la última pasada, para la barra de tareas.
    known: HashSet<String>,
    documents_observed: bool,
}

impl State {
    fn load_seen(&mut self) {
        if self.seen_loaded {
            return;
        }
        self.seen_loaded = true;
        let saved = file("notifications-seen.json").and_then(|p| std::fs::read(p).ok()).and_then(|b| serde_json::from_slice::<Value>(&b).ok());
        if let Some(Value::Object(saved)) = saved {
            if let Some(keys) = saved.get("seen").and_then(Value::as_array) {
                self.seen.extend(keys.iter().filter_map(Value::as_str).map(str::to_string));
            }
            self.documents_observed = saved.get("documentsObserved") == Some(&Value::Bool(true));
        }
    }

    fn save_seen(&self) {
        let Some(path) = file("notifications-seen.json") else { return };
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let body = serde_json::json!({ "seen": self.seen.iter().collect::<Vec<_>>(), "documentsObserved": self.documents_observed });
        let _ = std::fs::write(path, serde_json::to_vec(&body).unwrap_or_default());
    }

    /// El filtro de `NativeNotifications.notify`: foco, conjunto activo, tipo y una sola vez.
    fn decide(&mut self, notice: &DesktopNotice, prefs: &Value, focused: bool) -> Decision {
        let key = key_of(notice);
        if !pref(prefs, "notifyWhileFocused") && focused {
            return Decision::Skip("focused");
        }
        if self.active.as_ref().is_some_and(|active| !active.contains(&key)) {
            return Decision::Skip("inactive");
        }
        if !enabled(&notice.kind, prefs) {
            return Decision::Skip("kind");
        }
        if !self.seen.insert(key.clone()) {
            return Decision::Skip("duplicate");
        }
        self.sequence += 1;
        // el tag de WinRT admite hasta 64 caracteres: un número de secuencia
        let tag = format!("orgtree-{}", self.sequence);
        self.shown.insert(key, Shown { kind: notice.kind.clone(), tag: tag.clone() });
        self.by_tag.insert(tag.clone(), notice.clone());
        Decision::Show { tag }
    }

    /// No se pudo mostrar: se olvida, para que la próxima pasada lo intente.
    fn failed(&mut self, notice: &DesktopNotice) {
        let key = key_of(notice);
        self.seen.remove(&key);
        if let Some(shown) = self.shown.remove(&key) {
            self.by_tag.remove(&shown.tag);
        }
    }

    fn remove(&mut self, keys: Vec<String>) -> Vec<Removed> {
        keys.into_iter()
            .filter_map(|key| {
                let shown = self.shown.remove(&key)?;
                let id = self.by_tag.remove(&shown.tag).map(|n| n.id).unwrap_or_default();
                Some(Removed { tag: shown.tag, id, kind: shown.kind })
            })
            .collect()
    }

    /// `syncNotifications(eligible)`: retira lo que ya no es elegible y
    /// recuerda lo visto solo mientras siga pendiente (`retainHistory`).
    fn sync(&mut self, eligible: HashSet<String>, pending: &HashSet<String>) -> Vec<Removed> {
        self.seen.retain(|key| pending.contains(key));
        let gone: Vec<String> = self.shown.keys().filter(|key| !eligible.contains(*key)).cloned().collect();
        self.active = Some(eligible);
        self.remove(gone)
    }

    /// Un tipo que se apagó retira sus notificaciones.
    fn configure(&mut self, prefs: &Value) -> Vec<Removed> {
        let gone: Vec<String> = self.shown.iter().filter(|(_, s)| !enabled(&s.kind, prefs)).map(|(k, _)| k.clone()).collect();
        self.remove(gone)
    }

    /// `TaskbarAttention.set`: parpadea con una llegada nueva, para sin nada pendiente.
    fn attention(&mut self, ids: HashSet<String>) -> Pulse {
        if ids.is_empty() && self.known.is_empty() {
            // nada pendiente antes ni ahora: no hay nada que parar otra vez
            return Pulse::Keep;
        }
        let arrived = ids.iter().any(|id| !self.known.contains(id));
        self.known = ids;
        if self.known.is_empty() {
            Pulse::Stop
        } else if arrived {
            Pulse::Start
        } else {
            Pulse::Keep
        }
    }
}

static STATE: std::sync::LazyLock<Mutex<State>> = std::sync::LazyLock::new(Mutex::default);

// ─── Preguntas a la vista ─────────────────────────────────────────────────────

/// Preguntas cuya tarjeta está en pantalla (`questionVisible`): ya llegaron al
/// usuario, así que no se notifican.
static VISIBLE: Mutex<Option<HashMap<String, usize>>> = Mutex::new(None);

pub fn question_visible(org: &str, ask: &str, visible: bool) {
    let mut map = VISIBLE.lock().unwrap();
    let map = map.get_or_insert_with(HashMap::new);
    let key = identity(org, ask);
    let count = map.entry(key.clone()).or_default();
    if visible {
        *count += 1;
    } else {
        *count = count.saturating_sub(1);
        if *count == 0 {
            map.remove(&key);
        }
    }
}

fn is_question_visible(n: &DesktopNotice) -> bool {
    n.kind == "question"
        && VISIBLE.lock().unwrap().as_ref().is_some_and(|m| m.contains_key(&identity(&n.org, n.source_id.as_deref().unwrap_or(&n.id))))
}

// ─── Registro para la prueba ──────────────────────────────────────────────────

static LOG: Mutex<Vec<Value>> = Mutex::new(Vec::new());

fn log(kind: &str, value: Value) {
    if std::env::var_os("ORGTREE_DIOXUS_PROBE").is_none() {
        return;
    }
    let mut entry = Map::new();
    entry.insert("type".into(), Value::String(kind.into()));
    if let Value::Object(map) = value {
        entry.extend(map);
    }
    LOG.lock().unwrap().push(Value::Object(entry));
}

/// Lo que pasó desde el arranque: cada decisión, sincronización, parpadeo y clic.
pub fn log_snapshot() -> Vec<Value> {
    LOG.lock().unwrap().clone()
}

// ─── El toast ─────────────────────────────────────────────────────────────────

/// Escapa texto para el XML del toast.
#[cfg(any(windows, test))]
fn xml(text: &str) -> String {
    text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;").replace('\'', "&apos;")
}

#[cfg(windows)]
mod toast {
    //! El toast de WinRT con tag y grupo propios y el evento `Activated`, como
    //! `notifications.rs` del spike de Tauri.
    use windows::core::HSTRING;
    use windows::Data::Xml::Dom::XmlDocument;
    use windows::Foundation::TypedEventHandler;
    use windows::UI::Notifications::{ToastNotification, ToastNotificationManager};

    pub const GROUP: &str = "orgtree";
    /// El AppUserModelID de PowerShell, el mismo que usa `notify-rust`: el
    /// instalador de dx no registra uno propio en el acceso directo, y un id
    /// sin registrar no muestra nada.
    pub const APP_ID: &str = "{1AC14E77-02E7-4E5D-B744-2EB1AE5198B7}\\WindowsPowerShell\\v1.0\\powershell.exe";

    thread_local! {
        /// Los toasts mostrados viven mientras estén en el centro de
        /// notificaciones: su handler de `Activated` sigue suscripto.
        static ALIVE: std::cell::RefCell<std::collections::HashMap<String, ToastNotification>> = Default::default();
    }

    pub fn show(tag: &str, title: &str, body: &str, on_click: impl Fn() + Send + 'static) -> windows::core::Result<()> {
        let document = XmlDocument::new()?;
        document.LoadXml(&HSTRING::from(format!(
            "<toast><visual><binding template=\"ToastGeneric\"><text>{}</text><text>{}</text></binding></visual></toast>",
            super::xml(title),
            super::xml(body)
        )))?;
        let toast = ToastNotification::CreateToastNotification(&document)?;
        toast.SetTag(&HSTRING::from(tag))?;
        toast.SetGroup(&HSTRING::from(GROUP))?;
        toast.Activated(&TypedEventHandler::new(move |_, _| {
            on_click();
            Ok(())
        }))?;
        ToastNotificationManager::CreateToastNotifierWithId(&HSTRING::from(APP_ID))?.Show(&toast)?;
        ALIVE.with(|alive| alive.borrow_mut().insert(tag.to_string(), toast));
        Ok(())
    }

    /// Retira un toast del centro de notificaciones (`notice.close()` en Electron).
    pub fn remove(tag: &str) {
        ALIVE.with(|alive| alive.borrow_mut().remove(tag));
        if let Ok(history) = ToastNotificationManager::History() {
            let _ = history.RemoveGroupedTagWithId(&HSTRING::from(tag), &HSTRING::from(GROUP), &HSTRING::from(APP_ID));
        }
    }
}

/// Muestra el toast. Fuera de Windows, `notify-rust` (sin clic).
fn show(notice: &DesktopNotice, tag: &str) -> Result<(), String> {
    #[cfg(windows)]
    {
        let clicked = tag.to_string();
        // `Activated` corre en un hilo de WinRT: el tag viaja por un canal a la
        // ventana principal, que abre el elemento.
        toast::show(tag, &notice.title, &notice.body, move || click(&clicked)).map_err(|e| e.to_string())
    }
    #[cfg(not(windows))]
    {
        let _ = tag;
        notify_rust::Notification::new().summary(&notice.title).body(&notice.body).show().map(|_| ()).map_err(|e| e.to_string())
    }
}

fn retract(removed: &[Removed]) {
    for Removed { tag, .. } in removed {
        #[cfg(windows)]
        toast::remove(tag);
        #[cfg(not(windows))]
        let _ = tag;
    }
}

// ─── El clic ──────────────────────────────────────────────────────────────────

static CLICKS: Mutex<Option<futures_channel::mpsc::UnboundedSender<String>>> = Mutex::new(None);

/// El clic en un toast (o la prueba, que llama a lo mismo que `Activated`).
pub fn click(tag: &str) {
    if let Some(sender) = CLICKS.lock().unwrap().as_ref() {
        let _ = sender.unbounded_send(tag.to_string());
    }
}

/// El tag del toast pendiente con esa identidad (para el clic simulado).
pub fn tag_of(org: &str, id: &str) -> Option<String> {
    STATE.lock().unwrap().shown.get(&identity(org, id)).map(|s| s.tag.clone())
}

static BUMP: tokio::sync::Notify = tokio::sync::Notify::const_new();

/// Algo cambió en una org (un frame del WebSocket): releer la proyección ya.
pub fn bump() {
    BUMP.notify_one();
}

/// ¿Alguna ventana de Orgtree tiene el foco?
fn any_focused() -> bool {
    let mut focused = false;
    crate::windows::with_main(|main| focused |= main.window.is_focused() && !main.window.is_minimized());
    focused || crate::windows::popout_focused()
}

/// Una pasada de `poll()`: leer, la barra de tareas, sincronizar y mostrar.
async fn pass(client: &Client) {
    let Ok(read) = client.notifications().await else { return };
    let prefs = prefs();
    let pending: Option<HashSet<String>> = read.active.as_ref().map(|a| a.iter().map(|i| identity(&i.org, &i.id)).collect());
    let candidates: Vec<DesktopNotice> = read.notices.into_iter().filter(|n| pending.as_ref().is_none_or(|p| p.contains(&key_of(n)))).collect();

    // La barra de tareas lee la proyección entera, no la filtrada por
    // preferencias: silenciar un tipo no quiere decir que dejó de esperar.
    let ids: HashSet<String> = candidates.iter().map(key_of).collect();
    let pulse = STATE.lock().unwrap().attention(ids.clone());
    let mut started = false;
    let mut main_focused = false;
    crate::windows::with_main(|main| {
        main_focused = main.window.is_focused() && !main.window.is_minimized();
        match pulse {
            Pulse::Start if !main_focused => {
                main.window.request_user_attention(Some(dioxus::desktop::tao::window::UserAttentionType::Critical));
                started = true;
            }
            Pulse::Stop => main.window.request_user_attention(None),
            _ => {}
        }
    });
    if pulse != Pulse::Keep {
        log("attention", serde_json::json!({ "ids": ids.len(), "pulse": format!("{pulse:?}"), "started": started, "focused": main_focused }));
    }

    // Lo que ya está en pantalla llegó al usuario; los documentos avisan solo
    // después de un primer inventario.
    let mut state = STATE.lock().unwrap();
    state.load_seen();
    let documents_observed = state.documents_observed;
    for n in &candidates {
        if is_question_visible(n) || (n.kind == "document" && (!documents_observed || !pref(&prefs, "notifyDocuments"))) {
            state.seen.insert(key_of(n));
        }
    }
    state.documents_observed = true;
    let (eligible, muted): (Vec<DesktopNotice>, Vec<DesktopNotice>) =
        candidates.into_iter().filter(|n| !is_question_visible(n)).partition(|n| enabled(&n.kind, &prefs));
    let removed = match &pending {
        Some(pending) => state.sync(eligible.iter().map(key_of).collect(), pending),
        None => Vec::new(),
    };
    state.save_seen();
    drop(state);
    retract(&removed);
    if !removed.is_empty() || pending.is_some() {
        log("sync", serde_json::json!({ "eligible": eligible.len(), "removed": removed_json(&removed) }));
    }

    for n in muted {
        log("notify", serde_json::json!({ "id": n.id, "kind": n.kind, "org": n.org, "decision": "kind" }));
    }
    let focused = any_focused();
    for n in eligible {
        let decision = STATE.lock().unwrap().decide(&n, &prefs, focused);
        let outcome = match decision {
            Decision::Skip(why) => why.to_string(),
            Decision::Show { tag } => match show(&n, &tag) {
                Ok(()) => "shown".to_string(),
                Err(error) => {
                    STATE.lock().unwrap().failed(&n);
                    format!("failed: {error}")
                }
            },
        };
        log("notify", serde_json::json!({ "id": n.id, "kind": n.kind, "org": n.org, "source_id": n.source_id, "item": n.item, "decision": outcome }));
    }
}

/// El clic: la notificación tiene que seguir pendiente y su tipo encendido
/// (el sistema puede retener un banner que ya se resolvió en otro lado). Se
/// muestra la ventana principal y la vista abre el elemento.
async fn clicked(client: &Client, tag: &str, mut focus: Signal<Option<DesktopNotice>>, mut route: Signal<crate::Route>) {
    let notice = STATE.lock().unwrap().by_tag.get(tag).cloned();
    let Some(notice) = notice else {
        log("click", serde_json::json!({ "tag": tag, "opened": false, "why": "unknown tag" }));
        return;
    };
    let current = client.notifications().await.ok().and_then(|read| {
        let still = read.active.as_ref().is_none_or(|a| a.iter().any(|i| i.org == notice.org && i.id == notice.id));
        read.notices.into_iter().find(|n| n.org == notice.org && n.id == notice.id && still)
    });
    let Some(current) = current.filter(|n| enabled(&n.kind, &prefs())) else {
        log("click", serde_json::json!({ "tag": tag, "id": notice.id, "opened": false, "why": "resolved" }));
        return;
    };
    crate::native::show_main();
    if *route.peek() != crate::Route::Org(current.org.clone()) {
        route.set(crate::Route::Org(current.org.clone()));
    }
    log("click", serde_json::json!({ "tag": tag, "id": current.id, "kind": current.kind, "opened": true }));
    focus.set(Some(current));
}

/// En la ventana principal: la pasada global con su reloj, los avisos de las
/// orgs abiertas y los clics. `focus` lleva a la vista el elemento a abrir.
pub fn use_notifications(focus: Signal<Option<DesktopNotice>>) {
    let route = use_context::<Signal<crate::Route>>();
    let client = use_context::<Signal<Option<Client>>>();
    use_future(move || async move {
        let (sender, mut clicks) = futures_channel::mpsc::unbounded::<String>();
        *CLICKS.lock().unwrap() = Some(sender);
        // el cliente llega cuando el motor está listo
        let client = loop {
            if let Some(client) = client.peek().clone() {
                break client;
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        };
        use futures_util::StreamExt;
        loop {
            pass(&client).await;
            tokio::select! {
                _ = BUMP.notified() => {
                    // varios frames juntos: una sola pasada
                    tokio::time::sleep(Duration::from_millis(150)).await;
                }
                _ = tokio::time::sleep(POLL_EVERY) => {}
                tag = clicks.next() => {
                    if let Some(tag) = tag {
                        clicked(&client, &tag, focus, route).await;
                    }
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn notice(id: &str, kind: &str) -> DesktopNotice {
        DesktopNotice { id: id.into(), org: "o".into(), kind: kind.into(), title: "t".into(), body: "b".into(), ..Default::default() }
    }

    fn defaults() -> Value {
        normalize(&Value::Null)
    }

    #[test]
    fn preferencias_por_tipo() {
        let p = defaults();
        assert!(enabled("question", &p) && enabled("urgent-mail", &p) && enabled("work-attention", &p) && enabled("terminal-failure", &p));
        assert!(!enabled("routine", &p) && !enabled("document", &p) && !enabled("agent-frozen", &p));
        let all_mail = normalize(&serde_json::json!({ "routineNotifications": true }));
        assert!(enabled("routine", &all_mail));
        let off = normalize(&serde_json::json!({ "notificationsEnabled": false }));
        assert!(!enabled("question", &off));
    }

    #[test]
    fn foco_tipo_y_una_sola_vez() {
        let mut s = State::default();
        let p = defaults();
        assert_eq!(s.decide(&notice("1", "routine"), &p, false), Decision::Skip("kind"));
        assert_eq!(s.decide(&notice("2", "question"), &p, true), Decision::Skip("focused"));
        assert!(matches!(s.decide(&notice("2", "question"), &p, false), Decision::Show { .. }));
        assert_eq!(s.decide(&notice("2", "question"), &p, false), Decision::Skip("duplicate"));
        let mut focused = p.clone();
        focused["notifyWhileFocused"] = Value::Bool(true);
        assert!(matches!(s.decide(&notice("3", "question"), &focused, true), Decision::Show { .. }));
    }

    #[test]
    fn sincronizar_retira_lo_resuelto() {
        let mut s = State::default();
        let p = defaults();
        let Decision::Show { tag } = s.decide(&notice("1", "question"), &p, false) else { panic!() };
        let pending: HashSet<String> = [identity("o", "2")].into();
        assert_eq!(s.sync(pending.clone(), &pending), vec![Removed { tag, id: "1".into(), kind: "question".into() }]);
        assert_eq!(s.decide(&notice("3", "question"), &p, false), Decision::Skip("inactive"));
        assert!(matches!(s.decide(&notice("2", "question"), &p, false), Decision::Show { .. }));
        // apagar un tipo retira lo que ya se mostró de ese tipo
        let mut off = p.clone();
        off["notifyQuestions"] = Value::Bool(false);
        assert_eq!(s.configure(&off).len(), 1);
        assert_eq!(xml("<a & b>"), "&lt;a &amp; b&gt;");
    }

    #[test]
    fn parpadeo_solo_con_llegadas_nuevas() {
        let mut s = State::default();
        let set = |ids: &[&str]| ids.iter().map(|s| s.to_string()).collect::<HashSet<_>>();
        assert_eq!(s.attention(set(&["x"])), Pulse::Start);
        assert_eq!(s.attention(set(&["x"])), Pulse::Keep);
        assert_eq!(s.attention(set(&["x", "y"])), Pulse::Start);
        assert_eq!(s.attention(set(&["y"])), Pulse::Keep);
        assert_eq!(s.attention(set(&[])), Pulse::Stop);
        assert_eq!(s.attention(set(&[])), Pulse::Keep);
    }
}
