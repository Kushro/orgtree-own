//! Tipos del docket compartido de tickets (#29), derivados de
//! `apps/desktop/renderer/src/types.ts` (`WorkItem`, `WorkItemsPayload`,
//! `WorkItemReplyResult`) y de `canvas/quickstaff.ts`.
//!
//! Como el resto del cliente, son permisivos: una sección con una forma
//! inesperada queda vacía (`lenient`) y no tira abajo el ticket entero.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// `counts` de `GET /work-items-view`: el resumen del docket entero. `active`
/// no cuenta el backlog ni lo cerrado; `archived` y `backlogged` son los
/// tamaños de los dos grupos que se piden aparte.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct WorkCounts {
    #[serde(default)]
    pub attention: u64,
    #[serde(default)]
    pub active: u64,
    #[serde(default)]
    pub archived: u64,
    #[serde(default)]
    pub backlogged: u64,
}

/// `work_items_summary` del árbol: el número del botón del docket en la barra
/// de la org (`DocketToolbarButton`), que viaja con el árbol.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct WorkSummary {
    #[serde(default)]
    pub attention: u64,
    #[serde(default)]
    pub active: u64,
    /// Cada bandera manual como `[slug, set_rev]`.
    #[serde(default)]
    pub raises: Vec<(String, u64)>,
}

/// Un destinatario posible de la respuesta (`reply_recipients`): el dueño o un
/// participante, con su estado.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Recipient {
    pub node: String,
    /// `owner` o `participant`.
    #[serde(default)]
    pub role: String,
    /// `live`, `retired` o `missing`.
    #[serde(default)]
    pub state: String,
}

/// Una pregunta de un agente adjunta al ticket (`WorkItemQuestion`). La
/// tarjeta para responderla es la del agente en el árbol (`ask_id`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct WorkQuestion {
    #[serde(default)]
    pub ask_id: String,
    #[serde(default)]
    pub node: String,
    #[serde(default)]
    pub at: String,
    #[serde(default)]
    pub tabs: Vec<Value>,
}

/// Una fila del registro de alcance (W03): las decisiones y las versiones de
/// la descripción. Solo se agregan; una fila reemplazada conserva su texto y
/// gana `superseded_by`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ScopeRow {
    #[serde(default)]
    pub seq: u64,
    #[serde(default)]
    pub at: String,
    #[serde(default)]
    pub by: Option<Value>,
    /// `decision`, o una versión de la descripción.
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub supersedes: Option<u64>,
    #[serde(default)]
    pub superseded_by: Option<u64>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// Una evidencia (`evidence[]`): a qué apunta y quién la dejó. Tope del motor:
/// 50 por ticket (`WORK_EVIDENCE_MAX`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Evidence {
    #[serde(default)]
    pub at: String,
    #[serde(default)]
    pub by: Option<Value>,
    #[serde(default)]
    pub kind: String,
    #[serde(default, rename = "ref")]
    pub reference: Option<String>,
    #[serde(default)]
    pub note: Option<String>,
    #[serde(default)]
    pub execution: Option<String>,
    /// El recibo de verificación (W08), si lo hay.
    #[serde(default)]
    pub receipt: Option<Value>,
}

/// Un artefacto (W08): evidencia inmutable que el usuario descarga y nunca
/// sube. Uno `named` sin permiso llega como `{visible: false, scope}`, sin
/// nombre. Tope del motor: 40 por ticket (`WORK_ARTIFACT_MAX`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Artifact {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub bytes: u64,
    #[serde(default)]
    pub sha256: String,
    #[serde(default)]
    pub scope: String,
    #[serde(default)]
    pub visible: Option<bool>,
    #[serde(default)]
    pub grants_live: Vec<String>,
}

/// Un archivo adjunto al ticket por el usuario (`attachments[]`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ItemAttachment {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub bytes: u64,
}

/// Un tramo del ticket en manos de un agente (`holders`, el más viejo
/// primero). El último es el dueño actual; `derived` marca una fila
/// reconstruida del historial.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Holder {
    #[serde(default)]
    pub node: String,
    #[serde(default)]
    pub generation: Option<u64>,
    #[serde(default)]
    pub from: Option<String>,
    #[serde(default)]
    pub by: Option<Value>,
    #[serde(default)]
    pub derived: Option<bool>,
}

/// El cuerpo de `POST …/work-items/{wid}/reply` (`replyWorkItem`): `to` y
/// `notice` solo viajan cuando se eligieron.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct WorkReply {
    pub body: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub to: Option<String>,
    #[serde(skip_serializing_if = "std::ops::Not::not", default)]
    pub notice: bool,
}

/// `WorkItemReplyResult`: a quién llegó, y si espera a que lo recontraten.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct WorkReplyResult {
    #[serde(default)]
    pub to: Option<String>,
    #[serde(default)]
    pub role: Option<String>,
    #[serde(default)]
    pub deferred: Option<bool>,
    #[serde(default)]
    pub notice: Option<bool>,
    #[serde(default)]
    pub warnings: Vec<String>,
}

/// Un modelo que "Staff…" ofrece (`QuickStaffModel`): solo los que se pueden
/// usar; el motor no manda filas deshabilitadas.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct StaffModel {
    pub tier: String,
    #[serde(default)]
    pub seat: f64,
    #[serde(default)]
    pub efforts: Vec<String>,
    #[serde(default)]
    pub accounts: Vec<Value>,
    #[serde(default)]
    pub default_ok: Option<bool>,
}

/// `GET …/work-items/{wid}/quick-staff` (`QuickStaffPreview`): cómo se
/// asignaría un ticket del backlog y con qué modelos.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct QuickStaffPreview {
    /// `request` (se le pide al asignado), `under_assignee` o `top_level`.
    #[serde(default)]
    pub mode: String,
    #[serde(default)]
    pub configured_mode: String,
    #[serde(default)]
    pub owner: Value,
    #[serde(default)]
    pub fallback: bool,
    #[serde(default)]
    pub disclosure: String,
    #[serde(default)]
    pub models: Vec<StaffModel>,
    #[serde(default)]
    pub availability: Option<Value>,
}

/// El cuerpo de `POST …/work-items/{wid}/quick-staff`, como `quickStaffEntry`:
/// lo que mostró la vista previa más la elección. `request_id` es un UUID que
/// se repite si se reintenta la misma elección.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct QuickStaffSelection {
    pub request_id: String,
    pub mode: String,
    pub configured_mode: String,
    pub owner: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tier: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub account: Option<String>,
}

/// Lo que devuelve "Staff…": el texto para el usuario y a quién quedó.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct QuickStaffResult {
    #[serde(default)]
    pub message: String,
    #[serde(default)]
    pub assigned_to: Option<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// `GET …/work-items/{wid}` (`WorkItemPayload`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct WorkItemPayload {
    pub item: crate::WorkItem,
}

/// El nombre de un actor del docket: `{node}`, `"user"` o `"@user"`.
pub fn actor_name(actor: Option<&Value>) -> String {
    match actor {
        Some(Value::String(s)) if s == "user" || s == "@user" => "you".to_string(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Object(map)) => map.get("node").and_then(Value::as_str).unwrap_or("?").to_string(),
        _ => "?".to_string(),
    }
}
