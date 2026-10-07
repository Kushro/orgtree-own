//! Tipos de las respuestas del motor que usa el recorte del spike, derivados
//! de `apps/desktop/renderer/src/types.ts` y `generated/events.ts`.
//!
//! Son permisivos a propósito: un campo nuevo en el motor no rompe el
//! cliente, y los nodos archivados llegan sin campos de ejecución
//! (`archived_defaults`, como en `hydrateTree`). Lo que el recorte no lee
//! queda en `extra` como JSON.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// `OrgListEntry` (types.ts): una fila de `GET /api/orgs`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OrgListEntry {
    pub slug: String,
    pub name: String,
    #[serde(default)]
    pub nodes: u64,
    #[serde(default)]
    pub live: u64,
    #[serde(default)]
    pub created: Option<String>,
    #[serde(default)]
    pub cost_usd_total: Option<f64>,
    /// Agentes con un turno en curso.
    #[serde(default)]
    pub working: Option<u64>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// `NodeState` (types.ts).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NodeState {
    Live,
    Archived,
    Unrecoverable,
    #[serde(other)]
    Unknown,
}

/// `TreeNode` (types.ts): un agente del organigrama.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TreeNode {
    pub id: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub tier: String,
    #[serde(default)]
    pub model_id: String,
    #[serde(default)]
    pub account: Option<String>,
    pub state: NodeState,
    #[serde(default)]
    pub generation: u64,
    #[serde(default)]
    pub children: Vec<TreeNode>,
    // Campos de ejecución: ausentes en nodos archivados.
    #[serde(default)]
    pub busy: Option<bool>,
    #[serde(default)]
    pub waiting: Option<bool>,
    #[serde(default)]
    pub responding: Option<bool>,
    #[serde(default)]
    pub phase: Option<String>,
    #[serde(default)]
    pub queued: Option<u64>,
    #[serde(default)]
    pub mail_pending: Option<u64>,
    #[serde(default)]
    pub last_error: Option<String>,
    #[serde(default)]
    pub occupancy: Option<f64>,
    #[serde(default)]
    pub context_window: Option<u64>,
    #[serde(default)]
    pub cost_usd: Option<f64>,
    // Organigrama (#26): créditos, detención, cola de turnos y último estado.
    /// Créditos del asiento (puede ser fraccionario, con piso 0,10).
    #[serde(default)]
    pub seat: Option<f64>,
    /// Créditos concedidos para financiar a los subordinados.
    #[serde(default)]
    pub grant: Option<f64>,
    /// Lo que queda libre de `grant`; `null` en nodos no vivos.
    #[serde(default)]
    pub free: Option<f64>,
    /// Detención explícita (`halting` o `halted`); ausente si no está detenido.
    #[serde(default)]
    pub halt: Option<HaltState>,
    /// En cola detrás del límite de turnos de la máquina.
    #[serde(default)]
    pub queued_for_slot: Option<Value>,
    /// Congelado (límite de uso o red).
    #[serde(default)]
    pub frozen: Option<Value>,
    #[serde(default)]
    pub last_status: Option<NodeStatus>,
    #[serde(default)]
    pub inflight_at: Option<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// `TreeNode.halt` (types.ts).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HaltState {
    /// `halting` mientras el turno activo termina, `halted` después.
    pub phase: String,
    #[serde(default)]
    pub requested_at: Option<String>,
    #[serde(default)]
    pub at: Option<String>,
    #[serde(default)]
    pub by: Option<String>,
}

/// `NodeStatus` (types.ts): el último `orgtree_status` del agente.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NodeStatus {
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub summary: Option<String>,
    #[serde(default)]
    pub at: Option<String>,
}

/// `AuditReport` (types.ts): la autoauditoría del ledger que trae el árbol.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AuditReport {
    #[serde(default)]
    pub live_nodes: u64,
    /// Créditos en circulación: lo que tienen los agentes de primer nivel.
    #[serde(default)]
    pub top_level_holds: f64,
    #[serde(default = "yes")]
    pub no_overdraft: bool,
    #[serde(default)]
    pub problems: Vec<String>,
}

fn yes() -> bool {
    true
}

impl TreeNode {
    /// Recorre el subárbol en preorden.
    pub fn walk<'a>(&'a self, out: &mut Vec<&'a TreeNode>) {
        out.push(self);
        for child in &self.children {
            child.walk(out);
        }
    }
}

/// `TreePayload` (types.ts): `GET /api/orgs/{slug}` sin `view=delta`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TreePayload {
    pub slug: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub sync_rev: Option<u64>,
    #[serde(default)]
    pub org_rev: Option<u64>,
    #[serde(default)]
    pub roots: Vec<TreeNode>,
    #[serde(default)]
    pub archived_defaults: Option<Map<String, Value>>,
    /// Organigrama (#26): la barra de la org.
    #[serde(default)]
    pub audit: Option<AuditReport>,
    #[serde(default)]
    pub cost_usd_total: f64,
    #[serde(default)]
    pub cost_usd_unknown: Option<bool>,
    /// Tiers contratables y su costo de asiento.
    #[serde(default)]
    pub tiers: Map<String, Value>,
    /// Detención de toda la org (killswitch), si está trabada.
    #[serde(default)]
    pub killswitch: Option<Value>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl TreePayload {
    pub fn nodes(&self) -> Vec<&TreeNode> {
        let mut out = Vec::new();
        for root in &self.roots {
            root.walk(&mut out);
        }
        out
    }
}

/// `ToolChip` (types.ts): una herramienta dentro de un mensaje.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolChip {
    pub name: String,
    #[serde(default)]
    pub arg: Option<String>,
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub result: Option<String>,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub truncated: Option<bool>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// `ChatMessage` (types.ts): una fila del desk.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub ts: Option<String>,
    #[serde(default)]
    pub row_id: Option<String>,
    #[serde(default)]
    pub event_id: Option<String>,
    #[serde(default)]
    pub tools: Vec<ToolChip>,
    #[serde(default)]
    pub thinking: Option<String>,
    /// `Segment[]` de `generated/events.ts`, sin tipar en el recorte.
    #[serde(default)]
    pub segments: Option<Value>,
    #[serde(default)]
    pub assistant_state: Option<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// `ChatPayload` (types.ts): `GET /api/orgs/{slug}/nodes/{nid}/chat`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChatPayload {
    #[serde(default)]
    pub busy: bool,
    #[serde(default)]
    pub queued: u64,
    #[serde(default)]
    pub responding: bool,
    #[serde(default)]
    pub last_error: Option<String>,
    #[serde(default)]
    pub messages: Vec<ChatMessage>,
    #[serde(default)]
    pub mail_pending: u64,
    #[serde(default)]
    pub has_older: Option<bool>,
    /// Cursor para pedir la página anterior.
    #[serde(default)]
    pub before: Option<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// `OpRequest` (types.ts): el cuerpo de `POST /api/orgs/{slug}/ops`. Solo
/// los campos de las operaciones del organigrama; los `None` no se mandan.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct OpRequest {
    pub op: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub node: Option<String>,
    /// `hire`: el superior (`Some(None)` = primer nivel, bajo el usuario). En
    /// `hire` se manda siempre, como el renderer (`parent: null`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent: Option<Option<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tier: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub grant: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub charter: Option<String>,
    /// `move`: el nuevo superior (`Some(None)` = primer nivel). En `move` se manda siempre.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub new_parent: Option<Option<String>>,
}

impl OpRequest {
    /// `{op:'hire', parent, tier, grant, name, charter}` (OrgCanvas.tsx `confirmDraft`).
    pub fn hire(parent: Option<&str>, tier: &str, name: &str, grant: u64, charter: Option<&str>) -> OpRequest {
        OpRequest {
            op: "hire".into(),
            parent: Some(parent.map(str::to_string)),
            tier: Some(tier.into()),
            grant: Some(grant),
            name: Some(name.into()),
            // `charter?.trim() || undefined`, como el renderer
            charter: charter.map(str::trim).filter(|c| !c.is_empty()).map(str::to_string),
            ..Default::default()
        }
    }

    /// `{op:'retire'|'rehire'|'dissolve', node}` (agentmenu.tsx, desk.tsx).
    pub fn on_node(op: &str, node: &str) -> OpRequest {
        OpRequest { op: op.into(), node: Some(node.into()), ..Default::default() }
    }

    /// `{op:'move', node, new_parent}` (OrgCanvas.tsx, el deshacer de un arrastre).
    pub fn move_to(node: &str, new_parent: Option<&str>) -> OpRequest {
        OpRequest {
            op: "move".into(),
            node: Some(node.into()),
            new_parent: Some(new_parent.map(str::to_string)),
            ..Default::default()
        }
    }
}

/// `OpResult` (types.ts): abierto; `warnings` es lo que el renderer muestra.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct OpResult {
    #[serde(default)]
    pub warnings: Vec<String>,
    /// `hire`: el id del agente nuevo.
    #[serde(default)]
    pub node: Option<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// `POST /api/orgs` responde el slug de la org nueva.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CreatedOrg {
    pub slug: String,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// `haltNode` (api.ts).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct HaltResult {
    #[serde(default)]
    pub halted: bool,
    #[serde(default)]
    pub settled: bool,
    #[serde(default)]
    pub halting: Option<bool>,
    #[serde(default)]
    pub status: String,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// `unhaltNode` (api.ts).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct UnhaltResult {
    #[serde(default)]
    pub unhalted: bool,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// `interruptNode` (api.ts).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct InterruptResult {
    #[serde(default)]
    pub interrupted: bool,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// Un frame del WebSocket `/api/orgs/{slug}/ws` (api.py, `_Hub`).
#[derive(Debug, Clone, PartialEq)]
pub enum Frame {
    /// Algo cambió en la org: el renderer vuelve a pedir el árbol.
    Changed { org: String, rev: u64, org_rev: Option<u64> },
    /// Evento de un nodo (`turn_done`, `renamed`, `frozen`, …).
    NodeEvent { org: String, node: String, event: String, rev: Option<u64>, detail: Map<String, Value> },
    /// Texto en vivo del desk (`delta`, `thinking`, filas de herramientas, …).
    NodeStream { org: String, node: String, kind: Option<String>, rev: Option<u64>, payload: Map<String, Value> },
    /// Animación de mail entre nodos, sin `rev`.
    Mail { org: String, from: String, to: String },
    /// Un tipo que el cliente no conoce todavía.
    Other(Value),
}

impl Frame {
    pub fn parse(text: &str) -> Result<Frame, serde_json::Error> {
        let value: Value = serde_json::from_str(text)?;
        Ok(Frame::from_value(value))
    }

    pub fn from_value(value: Value) -> Frame {
        let Value::Object(mut map) = value else { return Frame::Other(value) };
        let take_str = |map: &mut Map<String, Value>, key: &str| match map.remove(key) {
            Some(Value::String(s)) => Some(s),
            _ => None,
        };
        let kind = map.get("type").and_then(Value::as_str).map(str::to_string);
        let rev = map.get("rev").and_then(Value::as_u64);
        match kind.as_deref() {
            Some("changed") if rev.is_some() => {
                let org = take_str(&mut map, "org").unwrap_or_default();
                let org_rev = map.get("org_rev").and_then(Value::as_u64);
                Frame::Changed { org, rev: rev.unwrap_or_default(), org_rev }
            }
            Some("node_event") => {
                map.remove("type");
                map.remove("rev");
                let org = take_str(&mut map, "org").unwrap_or_default();
                let node = take_str(&mut map, "node").unwrap_or_default();
                let event = take_str(&mut map, "event").unwrap_or_default();
                Frame::NodeEvent { org, node, event, rev, detail: map }
            }
            Some("node_stream") => {
                map.remove("type");
                map.remove("rev");
                let org = take_str(&mut map, "org").unwrap_or_default();
                let node = take_str(&mut map, "node").unwrap_or_default();
                let kind = map.get("kind").and_then(Value::as_str).map(str::to_string);
                Frame::NodeStream { org, node, kind, rev, payload: map }
            }
            Some("mail") => {
                let org = take_str(&mut map, "org").unwrap_or_default();
                let from = take_str(&mut map, "from").unwrap_or_default();
                let to = take_str(&mut map, "to").unwrap_or_default();
                Frame::Mail { org, from, to }
            }
            _ => Frame::Other(Value::Object(map)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_conocidos() {
        assert_eq!(
            Frame::parse(r#"{"type":"changed","org":"a","rev":7,"org_rev":3}"#).unwrap(),
            Frame::Changed { org: "a".into(), rev: 7, org_rev: Some(3) }
        );
        match Frame::parse(r#"{"type":"node_event","org":"a","node":"n1","event":"turn_done","rev":8,"was":"x"}"#).unwrap() {
            Frame::NodeEvent { node, event, rev, detail, .. } => {
                assert_eq!((node.as_str(), event.as_str(), rev), ("n1", "turn_done", Some(8)));
                assert_eq!(detail.get("was"), Some(&Value::String("x".into())));
            }
            other => panic!("{other:?}"),
        }
        match Frame::parse(r#"{"type":"node_stream","org":"a","node":"n1","rev":9,"kind":"delta","text":"hola"}"#).unwrap() {
            Frame::NodeStream { kind, payload, .. } => {
                assert_eq!(kind.as_deref(), Some("delta"));
                assert_eq!(payload.get("text"), Some(&Value::String("hola".into())));
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(
            Frame::parse(r#"{"type":"mail","org":"a","from":"x","to":"y"}"#).unwrap(),
            Frame::Mail { org: "a".into(), from: "x".into(), to: "y".into() }
        );
        assert!(matches!(Frame::parse(r#"{"type":"nuevo"}"#).unwrap(), Frame::Other(_)));
    }

    #[test]
    fn op_request_como_el_renderer() {
        let hire = serde_json::to_value(OpRequest::hire(None, "haiku", "jefe", 5, Some("  "))).unwrap();
        assert_eq!(hire, serde_json::json!({"op":"hire","parent":null,"tier":"haiku","grant":5,"name":"jefe"}));
        let under = serde_json::to_value(OpRequest::hire(Some("jefe"), "haiku", "ayudante", 0, None)).unwrap();
        assert_eq!(under["parent"], "jefe");
        let moved = serde_json::to_value(OpRequest::move_to("ayudante", None)).unwrap();
        assert_eq!(moved, serde_json::json!({"op":"move","node":"ayudante","new_parent":null}));
        let retire = serde_json::to_value(OpRequest::on_node("retire", "x")).unwrap();
        assert_eq!(retire, serde_json::json!({"op":"retire","node":"x"}));
    }

    #[test]
    fn nodo_detenido_y_auditoria() {
        let node: TreeNode = serde_json::from_str(
            r#"{"id":"a","state":"live","seat":0.1,"grant":5,"free":4.9,"halt":{"phase":"halted","requested_at":"t","by":"@user"},"last_status":{"status":"idle","summary":"hired"}}"#,
        )
        .unwrap();
        assert_eq!(node.halt.as_ref().map(|h| h.phase.as_str()), Some("halted"));
        assert_eq!((node.seat, node.grant, node.free), (Some(0.1), Some(5.0), Some(4.9)));
        let audit: AuditReport = serde_json::from_str(r#"{"live_nodes":2,"top_level_holds":6}"#).unwrap();
        assert!(audit.no_overdraft);
    }

    #[test]
    fn nodo_archivado_sin_campos_de_ejecucion() {
        let node: TreeNode = serde_json::from_str(r#"{"id":"n1","state":"archived","children":[]}"#).unwrap();
        assert_eq!(node.state, NodeState::Archived);
        assert_eq!(node.busy, None);
        let node: TreeNode = serde_json::from_str(r#"{"id":"n2","state":"algo-nuevo"}"#).unwrap();
        assert_eq!(node.state, NodeState::Unknown);
    }
}
