//! El registro de ventanas principales (#20): quién es cada ventana y qué
//! ventana corresponde a un pedido de organización, un comando o un evento.
//!
//! Es el equivalente de `apps/desktop/main/org-windows.ts`, y como ese módulo
//! es puro: no toca Tauri, ni el disco, ni relojes. El shell (`mainwin.rs`)
//! hace los efectos (crear, enfocar, cerrar ventanas) con lo que este registro
//! decide, y nunca llama a una API de ventana mientras tiene tomado el lock.
//!
//! Reglas de Electron que se conservan:
//!
//! - **Una ventana por organización.** `request_org` decide y cambia el estado
//!   en el mismo paso: una org abierta se enfoca, una que se está abriendo da
//!   `pending`, una Homepage que pide una org se convierte en esa org (`bound`)
//!   y cualquier otro caso abre una ventana nueva. La ventana nueva se registra
//!   antes de construirla (hace de reserva), así que dos pedidos simultáneos no
//!   abren dos ventanas.
//! - Una ventana `org` es terminal: nunca cambia de organización.
//! - "Crear organización" convierte una Homepage en la vista de creación; desde
//!   una org abre una ventana de creación aparte (usuario, 2026-09-29).
//! - Las obligaciones globales de notificación (`notificationOwner`) son de la
//!   ventana viva registrada primero, y pasan a la siguiente cuando se cierra.
//! - Un formulario de creación con cambios sin guardar pide confirmación antes
//!   de cerrarse o de salir, y nunca dos preguntas a la vez por la misma ventana.

use serde_json::{json, Value};
use std::collections::HashMap;

/// Prefijo de las etiquetas de las ventanas principales. La capability
/// `engine-ui` concede los comandos a `win-*` y a nada más.
pub const LABEL_PREFIX: &str = "win-";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Homepage,
    Create,
    Org,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Homepage => "homepage",
            Kind::Create => "create",
            Kind::Org => "org",
        }
    }
}

/// `isOrgSlug` de `packages/contracts/desktop-window.ts`: exactamente el slug
/// que `isAppPath` admite en `/o/<org>`, sin un límite de largo inventado.
pub fn is_org_slug(value: &str) -> bool {
    !value.is_empty() && value.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'@' || b == b'-')
}

#[derive(Debug)]
pub struct Entry {
    pub id: String,
    pub kind: Kind,
    pub org: Option<String>,
    registered: u64,
    activated: u64,
    /// El renderer informó que el formulario de creación tiene datos sin guardar.
    pub unsaved: bool,
    /// Hay una confirmación de descarte en pantalla para esta ventana.
    pub confirming: bool,
    /// Esta vista de creación empezó en una Homepage: cancelarla vuelve a ella.
    pub return_home: bool,
    /// La ventana todavía se está construyendo (hace de reserva de su org).
    pub building: bool,
    /// La clave de posición con la que la ventana está en la sesión
    /// (`homepage`, `org:<slug>` o ninguna para la creación). La mantiene el shell.
    pub placement_key: Option<String>,
    /// El tamaño de área cliente restaurado que la ventana tiene que tener al
    /// mostrarse (se corrige después de `show`, ver `mainwin::exact_size`).
    pub exact_size: Option<(u32, u32)>,
    /// El documento actual terminó de cargar: los eventos retenidos se pueden entregar.
    pub loaded: bool,
    /// Eventos que el renderer no puede volver a pedir, retenidos hasta que el
    /// documento cargue (`window-outbox.ts`).
    outbox: Vec<Value>,
}

/// Qué hacer con un pedido de abrir una organización (`OrgRoutingDecision`).
#[derive(Debug, PartialEq, Eq)]
pub enum Routing {
    /// Ya está abierta en esa ventana: se enfoca, la que pidió no cambia.
    Focused { window: String, org: String },
    /// La ventana que pidió (una Homepage) ahora es esa org.
    Bound { window: String, org: String },
    /// Hay que construir esta ventana nueva, ya registrada como `building`.
    Open { window: String, org: String },
    /// Otra ventana para esa org ya está en camino.
    Pending { org: String },
    Refused { org: String, reason: &'static str },
}

impl Routing {
    /// `OrgOpenOutcome` del contrato: lo que el renderer ve, ya resuelto.
    pub fn outcome(&self) -> Value {
        match self {
            Routing::Focused { window, org } => json!({ "action": "focused", "windowId": window, "org": org }),
            Routing::Bound { window, org } => json!({ "action": "bound", "windowId": window, "org": org }),
            Routing::Open { window, org } => json!({ "action": "opened", "windowId": window, "org": org }),
            Routing::Pending { org } => json!({ "action": "pending", "org": org }),
            Routing::Refused { org, reason } => json!({ "action": "refused", "org": org, "reason": reason }),
        }
    }
}

/// "Crear organización" desde una ventana (`CreationStart`).
#[derive(Debug, PartialEq, Eq)]
pub enum CreationStart {
    Switched,
    AlreadyCreating,
    Open,
}

/// `CreationCloseDecision`.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum CloseGate {
    Close,
    Confirm,
    Awaiting,
}

#[derive(Debug, PartialEq, Eq)]
pub enum QuitGate {
    Proceed,
    Confirm(Vec<String>),
    Busy,
}

/// Los eventos que se retienen hasta que haya un documento que los reciba
/// (`HELD_EVENT_TYPES` en Electron).
pub const HELD: [&str; 4] = ["open-org", "notification-click", "window-identity", "restore-skipped"];
const OUTBOX_LIMIT: usize = 64;

#[derive(Default, Debug)]
pub struct Registry {
    entries: HashMap<String, Entry>,
    registrations: u64,
    activations: u64,
    windows: u64,
    owner: Option<String>,
    /// Revelados para una org cuya ventana todavía no existe (`queueReveal`).
    waiting: HashMap<String, Vec<Value>>,
}

impl Registry {
    /// Una etiqueta nueva, única durante toda la vida del proceso.
    pub fn next_label(&mut self) -> String {
        self.windows += 1;
        format!("{LABEL_PREFIX}{}", self.windows)
    }

    /// Registra una ventana antes de construirla. Una ventana `org` necesita un
    /// slug válido y que nadie más tenga esa org.
    pub fn register(&mut self, id: &str, kind: Kind, org: Option<&str>) -> Result<(), String> {
        if self.entries.contains_key(id) {
            return Err(format!("Window {id} is already registered"));
        }
        let org = match (kind, org) {
            (Kind::Org, Some(org)) if is_org_slug(org) => {
                if self.holder(org).is_some() {
                    return Err(format!("Organization {org} is already open"));
                }
                Some(org.to_string())
            }
            (Kind::Org, _) => return Err("An org-bound window needs a valid organization".into()),
            (_, Some(_)) => return Err("Only an org-bound window carries an organization".into()),
            (_, None) => None,
        };
        self.registrations += 1;
        self.activations += 1;
        self.entries.insert(
            id.to_string(),
            Entry {
                id: id.to_string(),
                kind,
                org,
                registered: self.registrations,
                activated: self.activations,
                unsaved: false,
                confirming: false,
                return_home: false,
                building: true,
                placement_key: None,
                exact_size: None,
                loaded: false,
                outbox: Vec::new(),
            },
        );
        Ok(())
    }

    /// La ventana se destruyó (o no se pudo construir).
    pub fn forget(&mut self, id: &str) -> Option<Entry> {
        self.entries.remove(id)
    }

    pub fn get(&self, id: &str) -> Option<&Entry> {
        self.entries.get(id)
    }

    pub fn get_mut(&mut self, id: &str) -> Option<&mut Entry> {
        self.entries.get_mut(id)
    }

    pub fn ids(&self) -> Vec<String> {
        let mut ids: Vec<&Entry> = self.entries.values().collect();
        ids.sort_by_key(|e| e.registered);
        ids.into_iter().map(|e| e.id.clone()).collect()
    }

    pub fn holder(&self, org: &str) -> Option<&Entry> {
        self.entries.values().find(|e| e.kind == Kind::Org && e.org.as_deref() == Some(org))
    }

    /// Las orgs que tienen ventana (`openOrgs`): nombres de org, nunca ids de ventana.
    pub fn open_orgs(&self) -> Vec<String> {
        let mut rows: Vec<&Entry> = self.entries.values().filter(|e| e.kind == Kind::Org).collect();
        rows.sort_by_key(|e| e.registered);
        rows.into_iter().filter_map(|e| e.org.clone()).collect()
    }

    /// La identidad que ve el renderer (`OrgWindowIdentity`).
    pub fn identity(&mut self, id: &str) -> Option<Value> {
        let owner = self.current_owner();
        let entry = self.entries.get(id)?;
        let mut identity = json!({
            "windowId": entry.id,
            "kind": entry.kind.as_str(),
            "notificationOwner": owner.as_deref() == Some(id),
        });
        if let (Kind::Org, Some(org)) = (entry.kind, &entry.org) {
            identity["org"] = Value::String(org.clone());
        }
        Some(identity)
    }

    fn earliest(&self) -> Option<String> {
        self.entries.values().min_by_key(|e| e.registered).map(|e| e.id.clone())
    }

    fn current_owner(&mut self) -> Option<String> {
        self.reconcile_owner();
        self.owner.clone()
    }

    /// Recalcula la dueña de las notificaciones. Devuelve la nueva dueña si
    /// cambió, para avisarle con `window-identity` (es lo único que arranca su sondeo).
    pub fn reconcile_owner(&mut self) -> Option<String> {
        let next = self.earliest();
        if next == self.owner {
            return None;
        }
        self.owner = next.clone();
        next
    }

    pub fn is_owner(&mut self, id: &str) -> bool {
        self.current_owner().as_deref() == Some(id)
    }

    /// LA forma de abrir una organización: la Homepage, la bandeja y el clic en
    /// una notificación llegan acá. `caller` es la ventana que pidió, o `None`
    /// para la bandeja y las notificaciones. `label` es la etiqueta que tendrá
    /// una ventana nueva si hace falta.
    pub fn request_org(&mut self, org: &str, caller: Option<&str>, label: impl FnOnce(&mut Self) -> String) -> Routing {
        if !is_org_slug(org) {
            return Routing::Refused { org: org.to_string(), reason: "invalid-org" };
        }
        if let Some(open) = self.holder(org) {
            return if open.building {
                Routing::Pending { org: org.into() }
            } else {
                Routing::Focused { window: open.id.clone(), org: org.into() }
            };
        }
        let caller_kind = match caller {
            Some(id) => match self.entries.get(id) {
                Some(entry) => Some(entry.kind),
                None => return Routing::Refused { org: org.into(), reason: "unknown-window" },
            },
            None => None,
        };
        if let (Some(id), Some(Kind::Homepage)) = (caller, caller_kind) {
            self.bind(id, org);
            return Routing::Bound { window: id.to_string(), org: org.into() };
        }
        let id = label(self);
        // Registrada ya, como `building`: hace de reserva de la org.
        if self.register(&id, Kind::Org, Some(org)).is_err() {
            return Routing::Pending { org: org.into() };
        }
        Routing::Open { window: id, org: org.into() }
    }

    fn bind(&mut self, id: &str, org: &str) {
        if let Some(entry) = self.entries.get_mut(id) {
            entry.kind = Kind::Org;
            entry.org = Some(org.to_string());
            entry.return_home = false;
            entry.unsaved = false;
        }
    }

    /// La creación terminó bien: la ventana de creación pasa a ser la org nueva.
    pub fn bind_created(&mut self, id: &str, org: &str) -> Routing {
        if !is_org_slug(org) {
            return Routing::Refused { org: org.to_string(), reason: "invalid-org" };
        }
        let Some(entry) = self.entries.get(id) else {
            return Routing::Refused { org: org.into(), reason: "unknown-window" };
        };
        match entry.kind {
            Kind::Org => return Routing::Refused { org: org.into(), reason: "already-bound" },
            Kind::Homepage => return Routing::Refused { org: org.into(), reason: "not-a-creation-window" },
            Kind::Create => {}
        }
        if self.holder(org).is_some() {
            return Routing::Refused { org: org.into(), reason: "already-open" };
        }
        self.bind(id, org);
        Routing::Bound { window: id.to_string(), org: org.into() }
    }

    /// "Crear organización" desde la ventana `id`.
    pub fn start_creation(&mut self, id: &str) -> CreationStart {
        match self.entries.get_mut(id) {
            Some(entry) if entry.kind == Kind::Homepage => {
                entry.kind = Kind::Create;
                entry.return_home = true;
                entry.unsaved = false;
                CreationStart::Switched
            }
            Some(entry) if entry.kind == Kind::Create => CreationStart::AlreadyCreating,
            _ => CreationStart::Open,
        }
    }

    /// Cancelar una creación que empezó en una Homepage vuelve a la Homepage.
    pub fn return_home(&mut self, id: &str) -> bool {
        match self.entries.get_mut(id) {
            Some(entry) if entry.kind == Kind::Create && entry.return_home => {
                entry.kind = Kind::Homepage;
                entry.return_home = false;
                entry.unsaved = false;
                true
            }
            _ => false,
        }
    }

    pub fn set_unsaved(&mut self, id: &str, dirty: bool) {
        if let Some(entry) = self.entries.get_mut(id) {
            entry.unsaved = dirty && entry.kind != Kind::Org;
        }
    }

    /// Qué hacer con un pedido de cierre de esta ventana.
    pub fn begin_close(&mut self, id: &str) -> CloseGate {
        match self.entries.get_mut(id) {
            Some(entry) if entry.unsaved => {
                if entry.confirming {
                    CloseGate::Awaiting
                } else {
                    entry.confirming = true;
                    CloseGate::Confirm
                }
            }
            _ => CloseGate::Close,
        }
    }

    /// La persona respondió: `discard` borra el borrador para que el cierre siga.
    pub fn settle_close(&mut self, id: &str, discard: bool) {
        if let Some(entry) = self.entries.get_mut(id) {
            entry.confirming = false;
            if discard {
                entry.unsaved = false;
            }
        }
    }

    /// Lo que una salida ordenada le debe a los formularios sin guardar.
    pub fn quit_gate(&self) -> QuitGate {
        let dirty: Vec<&Entry> = self.entries.values().filter(|e| e.unsaved).collect();
        if dirty.iter().any(|e| e.confirming) {
            QuitGate::Busy
        } else if dirty.is_empty() {
            QuitGate::Proceed
        } else {
            let mut ids: Vec<&Entry> = dirty;
            ids.sort_by_key(|e| e.registered);
            QuitGate::Confirm(ids.into_iter().map(|e| e.id.clone()).collect())
        }
    }

    pub fn activate(&mut self, id: &str) {
        self.activations += 1;
        let next = self.activations;
        if let Some(entry) = self.entries.get_mut(id) {
            entry.activated = next;
        }
    }

    /// La última ventana usada: la que restauran la bandeja y una segunda instancia.
    pub fn last_activated(&self) -> Option<String> {
        self.entries.values().max_by_key(|e| e.activated).map(|e| e.id.clone())
    }

    /// Ofrece un evento a una ventana. Devuelve `true` si se puede entregar
    /// ya; si no, queda retenido (solo los tipos de `HELD`).
    pub fn offer(&mut self, id: &str, event: &Value) -> bool {
        let Some(entry) = self.entries.get_mut(id) else { return false };
        if entry.loaded {
            return true;
        }
        let held = event["type"].as_str().is_some_and(|t| HELD.contains(&t));
        if held {
            entry.outbox.push(event.clone());
            if entry.outbox.len() > OUTBOX_LIMIT {
                entry.outbox.remove(0);
            }
        }
        false
    }

    /// El documento terminó de cargar: devuelve lo retenido para entregarlo.
    pub fn loaded(&mut self, id: &str) -> Vec<Value> {
        match self.entries.get_mut(id) {
            Some(entry) => {
                entry.loaded = true;
                entry.building = false;
                std::mem::take(&mut entry.outbox)
            }
            None => Vec::new(),
        }
    }

    /// Empieza una navegación: lo que llegue se retiene hasta la carga.
    pub fn unloaded(&mut self, id: &str) {
        if let Some(entry) = self.entries.get_mut(id) {
            entry.loaded = false;
        }
    }

    /// Un revelado para una org: la ventana a la que entregarlo ya, o `None`
    /// si quedó esperando a que la ventana exista.
    pub fn queue_reveal(&mut self, org: &str, event: Value) -> Option<String> {
        if let Some(open) = self.holder(org) {
            return Some(open.id.clone());
        }
        self.waiting.entry(org.to_string()).or_default().push(event);
        None
    }

    /// Lo que esperaba a la ventana de esa org (se entrega al registrarla).
    pub fn take_reveals(&mut self, org: &str) -> Vec<Value> {
        self.waiting.remove(org).unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn label(r: &mut Registry) -> String {
        r.next_label()
    }

    fn ready(r: &mut Registry, id: &str) {
        r.loaded(id);
    }

    #[test]
    fn homepage_binds_itself_and_others_open_or_focus() {
        let mut r = Registry::default();
        let home = r.next_label();
        r.register(&home, Kind::Homepage, None).unwrap();
        ready(&mut r, &home);
        assert_eq!(r.request_org("a", Some(&home), label), Routing::Bound { window: home.clone(), org: "a".into() });
        assert_eq!(r.get(&home).unwrap().kind, Kind::Org);
        // Una ventana org pide otra org: se abre una ventana nueva.
        let opened = r.request_org("b", Some(&home), label);
        let Routing::Open { window: b, .. } = opened else { panic!("{opened:?}") };
        // Mientras se construye, otro pedido de la misma org espera.
        assert_eq!(r.request_org("b", None, label), Routing::Pending { org: "b".into() });
        ready(&mut r, &b);
        assert_eq!(r.request_org("b", Some(&home), label), Routing::Focused { window: b.clone(), org: "b".into() });
        assert_eq!(r.request_org("a", Some(&b), label), Routing::Focused { window: home.clone(), org: "a".into() });
        assert_eq!(r.open_orgs(), vec!["a".to_string(), "b".to_string()]);
        assert_eq!(r.request_org("A b", None, label), Routing::Refused { org: "A b".into(), reason: "invalid-org" });
        assert_eq!(r.request_org("c", Some("win-99"), label), Routing::Refused { org: "c".into(), reason: "unknown-window" });
    }

    #[test]
    fn notification_owner_is_the_earliest_and_transfers() {
        let mut r = Registry::default();
        let one = r.next_label();
        let two = r.next_label();
        r.register(&one, Kind::Homepage, None).unwrap();
        r.register(&two, Kind::Org, Some("a")).unwrap();
        assert_eq!(r.identity(&one).unwrap()["notificationOwner"], true);
        assert_eq!(r.identity(&two).unwrap()["notificationOwner"], false);
        assert_eq!(r.identity(&two).unwrap()["org"], "a");
        r.forget(&one);
        assert_eq!(r.reconcile_owner(), Some(two.clone()));
        assert!(r.is_owner(&two));
    }

    #[test]
    fn creation_switches_cancels_and_binds() {
        let mut r = Registry::default();
        let home = r.next_label();
        r.register(&home, Kind::Homepage, None).unwrap();
        assert_eq!(r.start_creation(&home), CreationStart::Switched);
        assert_eq!(r.start_creation(&home), CreationStart::AlreadyCreating);
        r.set_unsaved(&home, true);
        assert_eq!(r.begin_close(&home), CloseGate::Confirm);
        assert_eq!(r.begin_close(&home), CloseGate::Awaiting);
        assert_eq!(r.quit_gate(), QuitGate::Busy);
        r.settle_close(&home, false);
        assert_eq!(r.quit_gate(), QuitGate::Confirm(vec![home.clone()]));
        r.settle_close(&home, true);
        assert_eq!(r.begin_close(&home), CloseGate::Close);
        assert!(r.return_home(&home));
        assert_eq!(r.get(&home).unwrap().kind, Kind::Homepage);
        // Desde una org, "crear" abre una ventana aparte, que no vuelve a ninguna Homepage.
        let org = r.next_label();
        r.register(&org, Kind::Org, Some("a")).unwrap();
        assert_eq!(r.start_creation(&org), CreationStart::Open);
        let create = r.next_label();
        r.register(&create, Kind::Create, None).unwrap();
        assert!(!r.return_home(&create));
        assert_eq!(r.bind_created(&create, "a"), Routing::Refused { org: "a".into(), reason: "already-open" });
        assert_eq!(r.bind_created(&home, "n"), Routing::Refused { org: "n".into(), reason: "not-a-creation-window" });
        assert_eq!(r.bind_created(&org, "n"), Routing::Refused { org: "n".into(), reason: "already-bound" });
        assert_eq!(r.bind_created(&create, "n"), Routing::Bound { window: create.clone(), org: "n".into() });
        // Una ventana org no guarda borradores.
        r.set_unsaved(&create, true);
        assert_eq!(r.begin_close(&create), CloseGate::Close);
    }

    #[test]
    fn held_events_wait_for_the_document() {
        let mut r = Registry::default();
        let id = r.next_label();
        r.register(&id, Kind::Org, Some("a")).unwrap();
        assert!(!r.offer(&id, &json!({ "type": "notification-click", "data": {} })));
        assert!(!r.offer(&id, &json!({ "type": "window-state", "data": {} })));
        let held = r.loaded(&id);
        assert_eq!(held.len(), 1);
        assert!(r.offer(&id, &json!({ "type": "window-state" })));
        r.unloaded(&id);
        assert!(!r.offer(&id, &json!({ "type": "window-identity" })));
        assert_eq!(r.loaded(&id).len(), 1);
        // Un revelado para una org sin ventana espera a que se abra.
        assert_eq!(r.queue_reveal("b", json!({ "type": "notification-click" })), None);
        assert_eq!(r.take_reveals("b").len(), 1);
        assert_eq!(r.queue_reveal("a", json!({})), Some(id));
    }

    #[test]
    fn slugs_follow_the_route_rule() {
        assert!(is_org_slug("spike-fixture"));
        assert!(is_org_slug("a@3"));
        assert!(!is_org_slug(""));
        assert!(!is_org_slug("Mayus"));
        assert!(!is_org_slug("a/b"));
    }
}
