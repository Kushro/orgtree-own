//! Tipos de la bandeja del usuario, las preguntas de los agentes, la bandera
//! de atención de los tickets y las notificaciones (#28), derivados de
//! `apps/desktop/renderer/src/types.ts` y `notifications.ts`.
//!
//! Como el resto del cliente, son permisivos: lo que la vista no lee queda en
//! `extra`, y una forma inesperada dentro del árbol no rompe el árbol entero
//! (`lenient`).

use crate::MailRow;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Map, Value};

/// Deserializa `T` y, si la forma no encaja, deja el valor por defecto: una
/// pregunta con un campo nuevo no tiene que tirar abajo el árbol de la org.
pub(crate) fn lenient<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: serde::de::DeserializeOwned + Default,
{
    let value = Value::deserialize(deserializer)?;
    Ok(serde_json::from_value(value).unwrap_or_default())
}

/// `GET /api/orgs/{slug}/inbox` (`InboxPayload`): el mail sin leer, el archivo
/// de leídos y lo enviado por el usuario.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct InboxPayload {
    #[serde(default)]
    pub pending: Vec<MailRow>,
    #[serde(default)]
    pub delivered: Vec<MailRow>,
    #[serde(default)]
    pub sent: Vec<MailRow>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// Una opción de una pregunta: el motor manda un texto o `{label, description}`.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct AskOption {
    pub label: String,
    pub description: Option<String>,
}

impl<'de> Deserialize<'de> for AskOption {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(match Value::deserialize(deserializer)? {
            Value::String(label) => AskOption { label, description: None },
            Value::Object(map) => AskOption {
                label: map.get("label").and_then(Value::as_str).unwrap_or_default().to_string(),
                description: map.get("description").and_then(Value::as_str).map(str::to_string),
            },
            other => AskOption { label: other.to_string(), description: None },
        })
    }
}

/// `AskTab` (types.ts): una pestaña de la tarjeta compuesta (`node_ask`):
/// una pregunta, el pedido de créditos o un ítem de alcance.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AskTab {
    /// `question`, `credits` o `scope`.
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub question: Option<String>,
    #[serde(default)]
    pub header: Option<String>,
    #[serde(default)]
    pub options: Vec<AskOption>,
    #[serde(default)]
    pub multi: Option<bool>,
    // créditos y alcance
    #[serde(default)]
    pub id: Option<Value>,
    #[serde(default)]
    pub old: Option<f64>,
    #[serde(default)]
    pub new: Option<f64>,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub item: Option<Value>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// `AskInfo` (types.ts): un pedido de un agente al usuario. En el árbol, cada
/// nodo con algo abierto trae su tarjeta compuesta (`kind: batch`, con
/// `tabs` y `revs`); la cabecera (`asks`) trae las filas sueltas.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AskInfo {
    #[serde(default, deserialize_with = "id_text")]
    pub id: String,
    #[serde(default)]
    pub node: String,
    #[serde(default)]
    pub kind: Option<String>,
    /// `open` / `pending` mientras espera; después `answered`, `denied`…
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub at: String,
    #[serde(default)]
    pub question: Option<String>,
    #[serde(default)]
    pub header: Option<String>,
    #[serde(default)]
    pub options: Vec<AskOption>,
    #[serde(default)]
    pub multi: Option<bool>,
    /// Varias preguntas en una tarjeta (FR-04).
    #[serde(default)]
    pub questions: Vec<AskTab>,
    #[serde(default)]
    pub tabs: Vec<AskTab>,
    /// El sello CAS de cada almacén que resuelve la tarjeta compuesta.
    #[serde(default)]
    pub revs: Map<String, Value>,
    #[serde(default)]
    pub rev: Option<u64>,
    // créditos
    #[serde(default)]
    pub old: Option<f64>,
    #[serde(default)]
    pub new: Option<f64>,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub resolved_at: Option<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

fn id_text<'de, D: Deserializer<'de>>(deserializer: D) -> Result<String, D::Error> {
    Ok(match Value::deserialize(deserializer)? {
        Value::String(s) => s,
        Value::Null => String::new(),
        other => other.to_string(),
    })
}

impl AskInfo {
    /// `askIsOpen` (openasks.ts).
    pub fn is_open(&self) -> bool {
        self.status == "open" || self.status == "pending"
    }
}

/// `WorkManualAttention` (types.ts): la bandera que un agente levantó para el
/// usuario. Queda arriba hasta que el usuario responde o la descarta.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ManualAttention {
    #[serde(default)]
    pub reason: String,
    #[serde(default)]
    pub at: String,
    #[serde(default)]
    pub by: Option<Value>,
    #[serde(default)]
    pub set_rev: u64,
}

/// `WorkItem` (types.ts), con lo que usan la cola de atención y su panel.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct WorkItem {
    pub slug: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub owner: Option<Value>,
    #[serde(default)]
    pub manual_attention: Option<ManualAttention>,
    #[serde(default)]
    pub attention_sources: Vec<String>,
    #[serde(default)]
    pub objective: Option<String>,
    #[serde(default)]
    pub blocked_reason: Option<String>,
    #[serde(default)]
    pub archived: Option<bool>,
    #[serde(default)]
    pub at: Option<String>,
    #[serde(default)]
    pub updated_at: Option<String>,
    #[serde(default)]
    pub rev: Option<u64>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl WorkItem {
    /// El agente al que está asignado (`owner.node`).
    pub fn owner_node(&self) -> Option<String> {
        self.owner.as_ref()?.get("node")?.as_str().map(str::to_string)
    }
}

/// `GET /api/orgs/{slug}/work-items-view` (`WorkItemsPayload`) sin
/// `If-None-Match`: la respuesta entera. `attention` trae las banderas de
/// todos los grupos, también los archivados y el backlog.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct WorkItemsPayload {
    #[serde(default)]
    pub items: Vec<WorkItem>,
    #[serde(default)]
    pub attention: Option<Vec<WorkItem>>,
    #[serde(default)]
    pub archived: Option<Vec<WorkItem>>,
    #[serde(default)]
    pub backlogged: Option<Vec<WorkItem>>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// `POST …/work-items/{wid}/dismiss-attention`: el ticket queda `blocked`
/// salvo que estuviera en `done` o `review`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct DismissResult {
    #[serde(default)]
    pub dismissed: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub pending_questions: Option<u64>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// El cuerpo de `POST …/nodes/{nid}/batch` (`resolveBatch` en api.ts): las
/// respuestas por posición (`null` = saltada), la decisión de créditos y la
/// de cada ítem de alcance; los que faltan no se mandan.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct BatchAnswer {
    pub revs: Map<String, Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub answers: Option<Vec<Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub credits: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope: Option<Vec<String>>,
}

/// `NotificationIdentity` (contracts): una notificación es `org` + `id`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct NoticeIdentity {
    pub org: String,
    pub id: String,
}

/// `DesktopNotification` (contracts): una fila de la proyección de atención.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct DesktopNotice {
    pub id: String,
    pub org: String,
    /// `question`, `urgent-mail`, `terminal-failure`, `work-attention`,
    /// `routine`, `document` o `agent-frozen`.
    pub kind: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub body: String,
    #[serde(default)]
    pub agent: Option<String>,
    #[serde(default)]
    pub item: Option<String>,
    #[serde(default)]
    pub source_id: Option<String>,
    #[serde(default)]
    pub generation: Option<u64>,
    #[serde(default)]
    pub rev: Option<u64>,
}

impl DesktopNotice {
    pub fn identity(&self) -> NoticeIdentity {
        NoticeIdentity { org: self.org.clone(), id: self.id.clone() }
    }
}

/// `GET /api/desktop/notifications[?offset=N]`: una página de la proyección
/// de atención de todas las orgs. `active` es la pertenencia de la
/// proyección entera, también de lo que queda en otras páginas.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct NoticePage {
    #[serde(default)]
    pub notices: Vec<DesktopNotice>,
    #[serde(default)]
    pub total: u64,
    #[serde(default)]
    pub truncated: bool,
    #[serde(default)]
    pub next_offset: Option<u64>,
    #[serde(default)]
    pub active: Option<Vec<NoticeIdentity>>,
}

/// Lo que devuelve `notifications()`: todas las páginas leídas.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Notices {
    pub notices: Vec<DesktopNotice>,
    /// `None` con un motor sin contrato de páginas que cortó la lista: entonces
    /// no se puede dar por resuelto lo que no apareció.
    pub active: Option<Vec<NoticeIdentity>>,
}
