//! Notificaciones nativas y parpadeo de la barra de tareas (#21), como
//! `apps/desktop/main/notifications.ts`, `taskbar-attention.ts` y
//! `packages/contracts/notifications.ts`.
//!
//! - `notify` valida la notificación como Electron, aplica las preferencias por
//!   tipo (`notificationEnabled`), el filtro con una ventana de Orgtree enfocada
//!   (`notifyWhileFocused`), el conjunto activo de `syncNotifications` y la
//!   deduplicación por `org` + `id`.
//! - En Windows el toast lo arma el shell con WinRT (no el plugin), para tener
//!   el clic (`Activated`) y poder retirarlo cuando su elemento se resuelve
//!   (`ToastNotificationHistory`, por tag). Fuera de Windows se usa el plugin,
//!   sin clic.
//! - `TaskbarAttention`: el botón de la barra de tareas parpadea con cada
//!   llegada nueva, no con un sondeo que repite lo mismo, y para cuando la
//!   lista queda vacía.

use serde_json::{Map, Value};
use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

/// `NOTIFICATION_OPTIONS` (sin `notificationsEnabled`), con sus valores por defecto.
pub const OPTIONS: [(&str, bool); 8] = [
    ("notifyQuestions", true),
    ("notifyUrgentMail", true),
    ("notifyTerminalFailures", true),
    ("notifyDocketAttention", true),
    ("notifyAllMail", false),
    ("notifyDocuments", false),
    ("notifyFrozen", false),
    ("notifyWhileFocused", false),
];

const KINDS: [&str; 7] = ["question", "urgent-mail", "terminal-failure", "work-attention", "routine", "document", "agent-frozen"];
const FIELDS: [&str; 10] = ["id", "title", "body", "org", "agent", "item", "kind", "source_id", "generation", "rev"];

fn pref(prefs: &Value, key: &str) -> bool {
    match prefs.get(key).and_then(Value::as_bool) {
        Some(value) => value,
        // `routineNotifications` viejo cuenta como "todo el correo", como `notificationPreferences`.
        None if key == "notifyAllMail" => prefs.get("routineNotifications") == Some(&Value::Bool(true)),
        None => OPTIONS.iter().find(|(k, _)| *k == key).map(|(_, d)| *d).unwrap_or(false),
    }
}

/// `notificationEnabled(kind, prefs)`.
pub fn enabled(kind: &str, prefs: &Value) -> bool {
    if prefs.get("notificationsEnabled") != Some(&Value::Bool(true)) {
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

/// Una notificación validada (`notification()` de Electron).
#[derive(Clone, Debug)]
pub struct Notice {
    pub data: Map<String, Value>,
    pub kind: String,
    pub title: String,
    pub body: String,
}

impl Notice {
    pub fn key(&self) -> String {
        identity(self.data["org"].as_str().unwrap_or(""), self.data["id"].as_str().unwrap_or(""))
    }
}

fn identity(org: &str, id: &str) -> String {
    format!("{org}\u{0}{id}")
}

pub fn validate(value: &Map<String, Value>) -> Result<Notice, String> {
    if let Some(key) = value.keys().find(|k| !FIELDS.contains(&k.as_str())) {
        return Err(format!("Unknown notification field: {key}"));
    }
    let text = |key: &str, max: usize| match value.get(key).and_then(Value::as_str) {
        Some(v) if !v.is_empty() && v.chars().count() <= max => Ok(v.to_string()),
        _ => Err(format!("Invalid notification {key}")),
    };
    let kind = text("kind", 30)?;
    if !KINDS.contains(&kind.as_str()) {
        return Err("Unknown notification kind".into());
    }
    let mut data = Map::new();
    for (key, max) in [("id", 200), ("title", 200), ("body", 2000), ("org", 128)] {
        data.insert(key.into(), Value::String(text(key, max)?));
    }
    data.insert("kind".into(), Value::String(kind.clone()));
    for key in ["agent", "item", "source_id"] {
        if value.contains_key(key) {
            data.insert(key.into(), Value::String(text(key, 128)?));
        }
    }
    for key in ["generation", "rev"] {
        if let Some(v) = value.get(key) {
            match v.as_u64() {
                Some(n) if n <= (1u64 << 53) - 1 => data.insert(key.into(), Value::from(n)),
                _ => return Err(format!("Invalid notification {key}")),
            };
        }
    }
    if kind == "agent-frozen" && (!data.contains_key("agent") || !data.contains_key("generation")) {
        return Err("Missing frozen agent identity".into());
    }
    if kind == "document" && !data.contains_key("source_id") {
        return Err("Missing document identity".into());
    }
    let (title, body) = (text("title", 200)?, text("body", 2000)?);
    Ok(Notice { data, kind, title, body })
}

/// `syncNotifications(active)`: las identidades que siguen pendientes.
pub fn identities(value: &Value) -> Result<HashSet<String>, String> {
    let list = value.as_array().ok_or("Invalid notification identities")?;
    list.iter()
        .map(|item| {
            let object = item.as_object().ok_or("Invalid notification identity")?;
            let text = |key: &str, max: usize| object.get(key).and_then(Value::as_str).filter(|v| !v.is_empty() && v.chars().count() <= max);
            match (text("org", 128), text("id", 200)) {
                (Some(org), Some(id)) if object.keys().all(|k| k == "org" || k == "id") => Ok(identity(org, id)),
                _ => Err("Invalid notification identity".to_string()),
            }
        })
        .collect()
}

/// Un toast mostrado, guardado hasta que su elemento se resuelve.
struct Shown {
    kind: String,
    tag: String,
}

/// `NativeNotifications` + `NotificationGate`.
#[derive(Default)]
pub struct Notifications {
    state: Mutex<NotifyState>,
}

#[derive(Default)]
struct NotifyState {
    seen: HashSet<String>,
    active: Option<HashSet<String>>,
    shown: HashMap<String, Shown>,
    /// Notificaciones por tag, para que el clic encuentre su elemento.
    by_tag: HashMap<String, Notice>,
    sequence: u64,
}

/// Lo que decide `notify` antes de mostrar nada.
pub enum Decision {
    Show { tag: String },
    Skip(&'static str),
}

impl Notifications {
    /// El filtro de `NativeNotifications.notify` y de `NotificationGate.take`.
    pub fn decide(&self, notice: &Notice, prefs: &Value, focused: bool) -> Decision {
        let key = notice.key();
        let mut state = self.state.lock().unwrap();
        if !pref(prefs, "notifyWhileFocused") && focused {
            return Decision::Skip("focused");
        }
        if state.active.as_ref().is_some_and(|active| !active.contains(&key)) {
            return Decision::Skip("inactive");
        }
        if !enabled(&notice.kind, prefs) {
            return Decision::Skip("kind");
        }
        if !state.seen.insert(key.clone()) {
            return Decision::Skip("duplicate");
        }
        state.sequence += 1;
        // El tag de WinRT tiene un máximo de 64 caracteres: un número de secuencia.
        let tag = format!("orgtree-{}", state.sequence);
        state.shown.insert(key, Shown { kind: notice.kind.clone(), tag: tag.clone() });
        state.by_tag.insert(tag.clone(), notice.clone());
        Decision::Show { tag }
    }

    /// No se pudo mostrar: se olvida para que un próximo intento lo muestre.
    pub fn failed(&self, notice: &Notice) {
        let key = notice.key();
        let mut state = self.state.lock().unwrap();
        state.seen.remove(&key);
        if let Some(shown) = state.shown.remove(&key) {
            state.by_tag.remove(&shown.tag);
        }
    }

    /// La notificación de un tag que todavía está pendiente (para el clic).
    pub fn clicked(&self, tag: &str) -> Option<Notice> {
        let state = self.state.lock().unwrap();
        let notice = state.by_tag.get(tag)?;
        state.shown.get(&notice.key()).filter(|s| s.tag == tag).map(|_| notice.clone())
    }

    /// La notificación pendiente con esa identidad (para el clic simulado de la prueba).
    pub fn tag_of(&self, org: &str, id: &str) -> Option<String> {
        self.state.lock().unwrap().shown.get(&identity(org, id)).map(|s| s.tag.clone())
    }

    /// Saca los toasts de esas identidades; devuelve `(tag, id)` de cada uno.
    fn remove(state: &mut NotifyState, keys: Vec<String>) -> Vec<(String, String)> {
        let mut removed = Vec::new();
        for key in keys {
            if let Some(shown) = state.shown.remove(&key) {
                let id = state.by_tag.remove(&shown.tag).and_then(|n| n.data["id"].as_str().map(str::to_string)).unwrap_or_default();
                removed.push((shown.tag, id));
            }
        }
        removed
    }

    /// `sync`: el conjunto activo; devuelve los toasts que hay que retirar.
    pub fn sync(&self, active: HashSet<String>) -> Vec<(String, String)> {
        let mut state = self.state.lock().unwrap();
        state.seen.retain(|key| active.contains(key));
        let gone: Vec<String> = state.shown.keys().filter(|key| !active.contains(*key)).cloned().collect();
        let removed = Self::remove(&mut state, gone);
        state.active = Some(active);
        removed
    }

    /// `configure`: un tipo que se apagó retira sus notificaciones.
    pub fn configure(&self, prefs: &Value) -> Vec<(String, String)> {
        let mut state = self.state.lock().unwrap();
        let gone: Vec<String> = state.shown.iter().filter(|(_, s)| !enabled(&s.kind, prefs)).map(|(k, _)| k.clone()).collect();
        Self::remove(&mut state, gone)
    }
}

/// `attentionPayload(ids, items)`: la lista de identidades, con la org de cada
/// una si viene. Las dos listas salen de una sola pasada del renderer: un largo
/// distinto se rechaza.
pub fn attention_payload(ids: &Value, items: Option<&Value>) -> Result<Vec<String>, String> {
    let list = ids.as_array().filter(|l| l.len() <= 5000).ok_or("Invalid attention identities")?;
    let ids = list
        .iter()
        .map(|id| id.as_str().filter(|s| !s.is_empty() && s.chars().count() <= 400).map(str::to_string).ok_or("Invalid attention identity".to_string()))
        .collect::<Result<Vec<_>, _>>()?;
    if let Some(items) = items.filter(|v| !v.is_null()) {
        let rows = items.as_array().filter(|l| l.len() <= 5000).ok_or("Invalid attention identities")?;
        if rows.len() != ids.len() {
            return Err("Invalid attention identities".into());
        }
        for row in rows {
            let valid = match row {
                Value::String(s) => !s.is_empty() && s.chars().count() <= 400,
                Value::Object(o) => {
                    o.keys().all(|k| k == "id" || k == "org")
                        && o.get("id").and_then(Value::as_str).is_some_and(|s| !s.is_empty() && s.chars().count() <= 400)
                        && o.get("org").map_or(true, |org| org.as_str().is_some_and(|s| !s.is_empty() && s.chars().count() <= 128))
                }
                _ => false,
            };
            if !valid {
                return Err("Invalid attention identity".into());
            }
        }
    }
    Ok(ids)
}

/// `TaskbarAttention` con una sola ventana principal: la org decide qué
/// ventana parpadea, y el spike tiene una.
#[derive(Default)]
pub struct Attention {
    known: HashSet<String>,
}

/// Qué hacer con la barra de tareas después de `set`.
#[derive(Debug, PartialEq)]
pub enum Pulse {
    Start,
    Stop,
    Keep,
}

impl Attention {
    pub fn set(&mut self, ids: Vec<String>) -> Pulse {
        let next: HashSet<String> = ids.into_iter().collect();
        let arrived = next.iter().any(|id| !self.known.contains(id));
        self.known = next;
        if self.known.is_empty() {
            Pulse::Stop
        } else if arrived {
            Pulse::Start
        } else {
            Pulse::Keep
        }
    }
}

/// Escapa texto para el XML del toast.
#[cfg(any(windows, test))]
fn xml(text: &str) -> String {
    text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;").replace('\'', "&apos;")
}

#[cfg(windows)]
pub mod toast {
    //! El toast de WinRT, con tag y grupo propios y el clic.
    use windows::core::HSTRING;
    use windows::Data::Xml::Dom::XmlDocument;
    use windows::Foundation::TypedEventHandler;
    use windows::UI::Notifications::{ToastNotification, ToastNotificationManager};

    pub const GROUP: &str = "orgtree";
    /// El AppUserModelID de PowerShell: el que usa el plugin cuando la app corre
    /// desde `target/` y no tiene un acceso directo con el suyo.
    const POWERSHELL_APP_ID: &str = "{1AC14E77-02E7-4E5D-B744-2EB1AE5198B7}\\WindowsPowerShell\\v1.0\\powershell.exe";

    /// El id de la app instalada, o el de PowerShell desde `target/`, como el plugin.
    pub fn app_id(identifier: &str) -> String {
        let installed = std::env::current_exe()
            .ok()
            .and_then(|exe| exe.parent().map(|d| d.to_path_buf()))
            .map(|dir| !(dir.ends_with("target\\release") || dir.ends_with("target\\debug") || dir.ends_with("target/release")))
            .unwrap_or(true);
        if installed { identifier.to_string() } else { POWERSHELL_APP_ID.to_string() }
    }

    pub fn show(app_id: &str, tag: &str, title: &str, body: &str, on_click: impl Fn() + Send + 'static) -> windows::core::Result<()> {
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
        ToastNotificationManager::CreateToastNotifierWithId(&HSTRING::from(app_id))?.Show(&toast)
    }

    /// Retira un toast del centro de notificaciones (`notice.close()` en Electron).
    pub fn remove(app_id: &str, tag: &str) {
        if let Ok(history) = ToastNotificationManager::History() {
            let _ = history.RemoveGroupedTagWithId(&HSTRING::from(tag), &HSTRING::from(GROUP), &HSTRING::from(app_id));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn prefs() -> Value {
        let mut prefs = json!({ "notificationsEnabled": true });
        for (key, default) in OPTIONS {
            prefs[key] = Value::Bool(default);
        }
        prefs
    }

    fn notice(id: &str, kind: &str) -> Notice {
        let value = json!({ "id": id, "title": "t", "body": "b", "org": "o", "kind": kind });
        validate(value.as_object().unwrap()).unwrap()
    }

    #[test]
    fn tipos_y_foco() {
        let n = Notifications::default();
        let p = prefs();
        assert!(matches!(n.decide(&notice("1", "routine"), &p, false), Decision::Skip("kind")));
        assert!(matches!(n.decide(&notice("2", "question"), &p, true), Decision::Skip("focused")));
        assert!(matches!(n.decide(&notice("2", "question"), &p, false), Decision::Show { .. }));
        assert!(matches!(n.decide(&notice("2", "question"), &p, false), Decision::Skip("duplicate")));
        let mut focused = p.clone();
        focused["notifyWhileFocused"] = Value::Bool(true);
        assert!(matches!(n.decide(&notice("3", "question"), &focused, true), Decision::Show { .. }));
    }

    #[test]
    fn sync_retira_y_filtra() {
        let n = Notifications::default();
        let p = prefs();
        let Decision::Show { tag } = n.decide(&notice("1", "question"), &p, false) else { panic!() };
        let active = identities(&json!([{ "org": "o", "id": "2" }])).unwrap();
        assert_eq!(n.sync(active), vec![(tag, "1".to_string())]);
        assert!(matches!(n.decide(&notice("3", "question"), &p, false), Decision::Skip("inactive")));
        assert!(matches!(n.decide(&notice("2", "question"), &p, false), Decision::Show { .. }));
        assert!(identities(&json!([{ "org": "o", "id": "2", "x": 1 }])).is_err());
    }

    #[test]
    fn validacion() {
        let bad = json!({ "id": "1", "title": "t", "body": "b", "org": "o", "kind": "nope" });
        assert!(validate(bad.as_object().unwrap()).is_err());
        let extra = json!({ "id": "1", "title": "t", "body": "b", "org": "o", "kind": "question", "url": "x" });
        assert!(validate(extra.as_object().unwrap()).is_err());
        let frozen = json!({ "id": "1", "title": "t", "body": "b", "org": "o", "kind": "agent-frozen", "agent": "a" });
        assert!(validate(frozen.as_object().unwrap()).is_err());
        assert_eq!(xml("<a & b>"), "&lt;a &amp; b&gt;");
    }

    #[test]
    fn parpadeo_solo_con_llegadas_nuevas() {
        let mut a = Attention::default();
        assert_eq!(a.set(vec!["x".into()]), Pulse::Start);
        assert_eq!(a.set(vec!["x".into()]), Pulse::Keep);
        assert_eq!(a.set(vec!["x".into(), "y".into()]), Pulse::Start);
        assert_eq!(a.set(vec!["y".into()]), Pulse::Keep);
        assert_eq!(a.set(vec![]), Pulse::Stop);
        assert!(attention_payload(&json!(["a"]), Some(&json!([{ "org": "o", "id": "a" }, "b"]))).is_err());
        assert!(attention_payload(&json!(["a"]), Some(&json!([{ "org": "o", "id": "a" }]))).is_ok());
    }
}
