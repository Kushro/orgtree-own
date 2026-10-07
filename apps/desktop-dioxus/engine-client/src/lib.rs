//! Cliente del motor de Orgtree para el shell Dioxus.
//!
//! En Dioxus la UI no carga desde el motor: todas las llamadas salen desde
//! Rust, así que el token viaja como `X-Orgtree-Desktop-Token` en cada pedido
//! HTTP y en el handshake del WebSocket, sin cookies y sin cambios en el
//! motor. El token nunca llega al webview.

mod attention;
mod docket;
mod settings;
mod types;

pub use attention::*;
pub use docket::*;
pub use settings::*;
pub use types::*;

use futures_util::{SinkExt, StreamExt};
use std::fmt;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::Message;

pub const TOKEN_HEADER: &str = "X-Orgtree-Desktop-Token";
/// El renderer manda `ping` cada 25 s (api.ts `openWs`); el motor ignora el texto.
pub const PING_INTERVAL: Duration = Duration::from_secs(25);

#[derive(Debug)]
pub enum ClientError {
    Http(reqwest::Error),
    /// Respuesta no 2xx, con el `detail` del motor si lo trae.
    Status { status: u16, detail: String },
    WebSocket(String),
}

impl fmt::Display for ClientError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ClientError::Http(e) => write!(f, "HTTP: {e}"),
            ClientError::Status { status, detail } => write!(f, "{status}: {detail}"),
            ClientError::WebSocket(e) => write!(f, "WebSocket: {e}"),
        }
    }
}

impl std::error::Error for ClientError {}

impl From<reqwest::Error> for ClientError {
    fn from(e: reqwest::Error) -> Self {
        ClientError::Http(e)
    }
}

/// Reconexión con backoff exponencial y jitter. El renderer reintenta cada
/// 1,5 s fijos; acá se arranca igual y se duplica hasta el tope, y vuelve al
/// inicio en cuanto una conexión abre.
#[derive(Debug, Clone, Copy)]
pub struct Backoff {
    pub initial: Duration,
    pub max: Duration,
}

impl Default for Backoff {
    fn default() -> Self {
        Backoff { initial: Duration::from_millis(1500), max: Duration::from_secs(30) }
    }
}

impl Backoff {
    /// Espera para el intento `attempt` (0 = primer reintento): exponencial
    /// hasta el tope, con hasta 20 % de jitter para no sincronizar ventanas.
    pub fn delay(&self, attempt: u32) -> Duration {
        let base = self.initial.saturating_mul(1u32 << attempt.min(16)).min(self.max);
        let jitter = base.mul_f64(0.2 * pseudo_random());
        base + jitter
    }
}

fn pseudo_random() -> f64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.subsec_nanos()).unwrap_or(0);
    (nanos % 1000) as f64 / 1000.0
}

/// Lo que entrega una suscripción al WebSocket de una org.
#[derive(Debug, Clone, PartialEq)]
pub enum WsEvent {
    /// Conexión abierta (también tras reconectar): pedir el árbol de nuevo,
    /// porque pudo haber frames perdidos (igual que `resetSync` en el renderer).
    Connected,
    Frame(Frame),
    /// Conexión caída; `retry_in` es la espera antes del próximo intento.
    Disconnected { reason: String, retry_in: Duration },
}

#[derive(Clone)]
pub struct Client {
    origin: String,
    token: String,
    http: reqwest::Client,
}

impl fmt::Debug for Client {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Client").field("origin", &self.origin).field("token", &"<oculto>").finish()
    }
}

impl Client {
    /// `origin` es `http://127.0.0.1:<puerto>` tal como lo informa `ready`.
    pub fn new(origin: impl Into<String>, token: impl Into<String>) -> Result<Client, ClientError> {
        let token = token.into();
        let mut headers = reqwest::header::HeaderMap::new();
        let mut value = reqwest::header::HeaderValue::from_str(&token)
            .map_err(|_| ClientError::WebSocket("token con caracteres inválidos".into()))?;
        value.set_sensitive(true);
        headers.insert(TOKEN_HEADER, value);
        let http = reqwest::Client::builder()
            .default_headers(headers)
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(60))
            .build()?;
        Ok(Client { origin: origin.into().trim_end_matches('/').to_string(), token, http })
    }

    pub fn origin(&self) -> &str {
        &self.origin
    }

    async fn json<T: serde::de::DeserializeOwned>(&self, response: reqwest::Response) -> Result<T, ClientError> {
        let status = response.status();
        if status.is_success() {
            return Ok(response.json().await?);
        }
        let body = response.text().await.unwrap_or_default();
        let detail = serde_json::from_str::<serde_json::Value>(&body)
            .ok()
            .and_then(|v| v.get("detail").map(|d| d.as_str().map(str::to_string).unwrap_or_else(|| d.to_string())))
            .unwrap_or(body);
        Err(ClientError::Status { status: status.as_u16(), detail })
    }

    pub async fn get<T: serde::de::DeserializeOwned>(&self, path: &str) -> Result<T, ClientError> {
        let response = self.http.get(format!("{}{path}", self.origin)).send().await?;
        self.json(response).await
    }

    pub async fn post<T: serde::de::DeserializeOwned>(&self, path: &str, body: &serde_json::Value) -> Result<T, ClientError> {
        let response = self.http.post(format!("{}{path}", self.origin)).json(body).send().await?;
        self.json(response).await
    }

    pub async fn put<T: serde::de::DeserializeOwned>(&self, path: &str, body: &serde_json::Value) -> Result<T, ClientError> {
        let response = self.http.put(format!("{}{path}", self.origin)).json(body).send().await?;
        self.json(response).await
    }

    pub async fn delete<T: serde::de::DeserializeOwned>(&self, path: &str) -> Result<T, ClientError> {
        let response = self.http.delete(format!("{}{path}", self.origin)).send().await?;
        self.json(response).await
    }

    /// `POST` sin cuerpo, como `req(path, { method: 'POST' })` del renderer.
    pub async fn post_empty<T: serde::de::DeserializeOwned>(&self, path: &str) -> Result<T, ClientError> {
        let response = self.http.post(format!("{}{path}", self.origin)).send().await?;
        self.json(response).await
    }

    /// `POST /api/orgs` — `createOrg` (api.ts): `net_autoconnect` solo se
    /// manda cuando es `false`, y `net_hubs` solo si hay alguno.
    pub async fn create_org(&self, name: &str, dirs: &[String], net_autoconnect: bool, net_hubs: &[String]) -> Result<CreatedOrg, ClientError> {
        let mut body = serde_json::json!({ "name": name, "dirs": dirs });
        if !net_autoconnect {
            body["net_autoconnect"] = serde_json::Value::Bool(false);
        }
        if !net_hubs.is_empty() {
            body["net_hubs"] = serde_json::json!(net_hubs);
        }
        self.post("/api/orgs", &body).await
    }

    /// `DELETE /api/orgs/{slug}` — `deleteOrg` (api.ts).
    pub async fn delete_org(&self, slug: &str) -> Result<serde_json::Value, ClientError> {
        self.delete(&format!("/api/orgs/{}", encode(slug))).await
    }

    /// `POST /api/orgs/{slug}/ops` — `runOp` (api.ts): contratar, retirar,
    /// recontratar, mover y el resto de las operaciones del ledger.
    pub async fn op(&self, slug: &str, request: &OpRequest) -> Result<OpResult, ClientError> {
        let body = serde_json::to_value(request).expect("OpRequest siempre se serializa");
        self.post(&format!("/api/orgs/{}/ops", encode(slug)), &body).await
    }

    /// `POST /api/orgs/{slug}/nodes/{nid}/halt` — `haltNode` (api.ts).
    pub async fn halt(&self, slug: &str, node: &str) -> Result<HaltResult, ClientError> {
        self.post_empty(&format!("/api/orgs/{}/nodes/{}/halt", encode(slug), encode(node))).await
    }

    /// `POST /api/orgs/{slug}/nodes/{nid}/unhalt` — `unhaltNode` (api.ts).
    pub async fn unhalt(&self, slug: &str, node: &str) -> Result<UnhaltResult, ClientError> {
        self.post_empty(&format!("/api/orgs/{}/nodes/{}/unhalt", encode(slug), encode(node))).await
    }

    /// `POST /api/orgs/{slug}/nodes/{nid}/interrupt` — `interruptNode` (api.ts).
    pub async fn interrupt(&self, slug: &str, node: &str) -> Result<InterruptResult, ClientError> {
        self.post_empty(&format!("/api/orgs/{}/nodes/{}/interrupt", encode(slug), encode(node))).await
    }

    /// `POST /api/orgs/{slug}/nodes/{nid}/message` — `sendMessage` (api.ts).
    /// Un mensaje es mail: queda guardado en el buzón del agente y nunca
    /// interrumpe un turno (a mitad de turno llega en el próximo límite seguro).
    pub async fn send_message(&self, slug: &str, node: &str, message: &SendMessage) -> Result<SendResult, ClientError> {
        let body = serde_json::to_value(message).expect("SendMessage siempre se serializa");
        self.post(&format!("/api/orgs/{}/nodes/{}/message", encode(slug), encode(node)), &body).await
    }

    /// `POST /api/orgs/{slug}/nodes/{nid}/scope` — `saveScope` (api.ts). El
    /// desk lo usa para el esfuerzo (`{effort}`; `""` vuelve al de la org).
    pub async fn save_scope(&self, slug: &str, node: &str, scope: &serde_json::Value) -> Result<OpResult, ClientError> {
        self.post(&format!("/api/orgs/{}/nodes/{}/scope", encode(slug), encode(node)), scope).await
    }

    /// `DELETE /api/orgs/{slug}/nodes/{nid}/mail/{mid}` — `retractMail` (api.ts):
    /// retira un mail que el agente todavía no leyó.
    pub async fn retract_mail(&self, slug: &str, node: &str, mail: &str) -> Result<serde_json::Value, ClientError> {
        self.delete(&format!("/api/orgs/{}/nodes/{}/mail/{}", encode(slug), encode(node), encode(mail))).await
    }

    // ---- bandeja, preguntas y atención (#28)

    /// `GET /api/orgs/{slug}/inbox` — `getInbox` (api.ts): la bandeja del usuario.
    pub async fn inbox(&self, slug: &str) -> Result<InboxPayload, ClientError> {
        self.get(&format!("/api/orgs/{}/inbox", encode(slug))).await
    }

    /// `POST /api/orgs/{slug}/inbox/read` `{ids}` — `markRead` (api.ts): el
    /// mail leído pasa del grupo sin leer al archivo de leídos.
    pub async fn mark_read(&self, slug: &str, ids: &[String]) -> Result<serde_json::Value, ClientError> {
        self.post(&format!("/api/orgs/{}/inbox/read", encode(slug)), &serde_json::json!({ "ids": ids })).await
    }

    /// `POST /api/orgs/{slug}/inbox/clear` — `clearInbox` (api.ts): "Mark all
    /// read", archiva todo lo que queda sin leer.
    pub async fn clear_inbox(&self, slug: &str) -> Result<serde_json::Value, ClientError> {
        self.post_empty(&format!("/api/orgs/{}/inbox/clear", encode(slug))).await
    }

    /// Responder un mail de la bandeja del usuario — `replyMessage` (api.ts):
    /// `POST …/nodes/{remitente}/message` con `target`, la identidad del mail.
    pub async fn reply_mail(&self, slug: &str, to: &str, text: &str, mail_id: &str, client_op: Option<String>) -> Result<SendResult, ClientError> {
        let message = SendMessage {
            text: text.to_string(),
            target: Some(serde_json::json!({ "kind": "mail", "org": slug, "box": "user", "id": mail_id })),
            client_op,
            ..SendMessage::default()
        };
        self.send_message(slug, to, &message).await
    }

    /// `POST /api/orgs/{slug}/nodes/{nid}/batch` — `resolveBatch` (api.ts): la
    /// tarjeta compuesta de un agente, resuelta de una vez.
    pub async fn resolve_batch(&self, slug: &str, node: &str, answer: &BatchAnswer) -> Result<serde_json::Value, ClientError> {
        let body = serde_json::to_value(answer).expect("BatchAnswer siempre se serializa");
        self.post(&format!("/api/orgs/{}/nodes/{}/batch", encode(slug), encode(node)), &body).await
    }

    /// `POST /api/orgs/{slug}/asks/{aid}/answer` — `answerAsk` (api.ts), para
    /// una pregunta suelta: `{selected?, text?, rev?, dismiss?}`.
    pub async fn answer_ask(&self, slug: &str, ask: &str, body: &serde_json::Value) -> Result<serde_json::Value, ClientError> {
        self.post(&format!("/api/orgs/{}/asks/{}/answer", encode(slug), encode(ask)), body).await
    }

    /// `GET /api/orgs/{slug}/work-items-view` — `getWorkItems` (api.ts) sin
    /// caché condicional: la respuesta entera.
    pub async fn work_items(&self, slug: &str) -> Result<WorkItemsPayload, ClientError> {
        self.work_items_view(slug, false, false).await
    }

    /// `GET /api/orgs/{slug}/work-items-view[?archived=1][&backlogged=1]` —
    /// `getWorkItems(slug, archived, backlogged)` (api.ts). Los dos grupos se
    /// piden solo con su casilla (`include_archived`): sin ella, la lista y
    /// sus totales no cuentan lo archivado.
    pub async fn work_items_view(&self, slug: &str, archived: bool, backlogged: bool) -> Result<WorkItemsPayload, ClientError> {
        let flags: Vec<&str> = [(archived, "archived=1"), (backlogged, "backlogged=1")].into_iter().filter(|f| f.0).map(|f| f.1).collect();
        let query = if flags.is_empty() { String::new() } else { format!("?{}", flags.join("&")) };
        self.get(&format!("/api/orgs/{}/work-items-view{query}", encode(slug))).await
    }

    /// `GET /api/orgs/{slug}/work-items/{wid}` — `getWorkItem` (api.ts): el
    /// ticket entero (decisiones, evidencias, artefactos, historial y holders),
    /// que la lista liviana no trae.
    pub async fn work_item(&self, slug: &str, item: &str) -> Result<WorkItem, ClientError> {
        let payload: WorkItemPayload = self.get(&format!("/api/orgs/{}/work-items/{}", encode(slug), encode(item))).await?;
        Ok(payload.item)
    }

    /// `POST /api/orgs/{slug}/work-items/{wid}/reply` `{body, to?, notice?}` —
    /// `replyWorkItem` (api.ts): un comentario al dueño o a un participante.
    pub async fn reply_work_item_to(&self, slug: &str, item: &str, reply: &WorkReply) -> Result<WorkReplyResult, ClientError> {
        let path = format!("/api/orgs/{}/work-items/{}/reply", encode(slug), encode(item));
        let body = serde_json::to_value(reply).expect("WorkReply siempre se serializa");
        self.post(&path, &body).await
    }

    /// `GET /api/orgs/{slug}/work-items/{wid}/quick-staff` — la vista previa
    /// de "Staff…" (`quickStaffPath` en quickstaff.ts) para un ticket del backlog.
    pub async fn quick_staff_preview(&self, slug: &str, item: &str) -> Result<QuickStaffPreview, ClientError> {
        self.get(&format!("/api/orgs/{}/work-items/{}/quick-staff", encode(slug), encode(item))).await
    }

    /// `POST /api/orgs/{slug}/work-items/{wid}/quick-staff` — `quickStaffEntry`
    /// (quickstaff.ts): asigna el ticket del backlog (o se lo pide al asignado).
    pub async fn quick_staff(&self, slug: &str, item: &str, selection: &QuickStaffSelection) -> Result<QuickStaffResult, ClientError> {
        let path = format!("/api/orgs/{}/work-items/{}/quick-staff", encode(slug), encode(item));
        let body = serde_json::to_value(selection).expect("QuickStaffSelection siempre se serializa");
        self.post(&path, &body).await
    }

    /// `GET /api/orgs/{slug}/work-items/{wid}/artifacts/{aid}` —
    /// `workItemArtifactUrl` (api.ts): los bytes de un artefacto.
    pub async fn artifact_bytes(&self, slug: &str, item: &str, artifact: &str) -> Result<Vec<u8>, ClientError> {
        self.bytes(&format!("/api/orgs/{}/work-items/{}/artifacts/{}", encode(slug), encode(item), encode(artifact))).await
    }

    /// `GET /api/orgs/{slug}/work-items/{wid}/attachments/{aid}` —
    /// `workItemAttachmentUrl` (api.ts): los bytes de un adjunto del ticket.
    pub async fn attachment_bytes(&self, slug: &str, item: &str, attachment: &str) -> Result<Vec<u8>, ClientError> {
        self.bytes(&format!("/api/orgs/{}/work-items/{}/attachments/{}", encode(slug), encode(item), encode(attachment))).await
    }

    /// Un `GET` que devuelve el cuerpo crudo (una descarga).
    pub async fn bytes(&self, path: &str) -> Result<Vec<u8>, ClientError> {
        let response = self.http.get(format!("{}{path}", self.origin)).send().await?;
        let status = response.status();
        if status.is_success() {
            return Ok(response.bytes().await?.to_vec());
        }
        self.json::<serde_json::Value>(response).await.map(|_| Vec::new())
    }

    /// `POST /api/orgs/{slug}/work-items/{wid}/dismiss-attention` `{set_rev}` —
    /// `dismissWorkItemAttention` (api.ts). Baja la bandera y pasa el ticket a
    /// `blocked` (salvo `done` y `review`).
    pub async fn dismiss_attention(&self, slug: &str, item: &str, set_rev: u64) -> Result<DismissResult, ClientError> {
        let path = format!("/api/orgs/{}/work-items/{}/dismiss-attention", encode(slug), encode(item));
        self.post(&path, &serde_json::json!({ "set_rev": set_rev })).await
    }

    /// `POST /api/orgs/{slug}/work-items/{wid}/reply` `{body}` — `replyWorkItem`
    /// (api.ts): mail al asignado. Una respuesta del usuario baja la bandera
    /// sin cambiar el estado del ticket.
    pub async fn reply_work_item(&self, slug: &str, item: &str, body: &str) -> Result<WorkReplyResult, ClientError> {
        self.reply_work_item_to(slug, item, &WorkReply { body: body.to_string(), ..WorkReply::default() }).await
    }

    /// `GET /api/desktop/notifications` con todas sus páginas, como
    /// `readNotices` (notifications.ts). Con un motor viejo sin contrato de
    /// páginas que cortó la lista, `active` queda en `None`.
    pub async fn notifications(&self) -> Result<Notices, ClientError> {
        let mut out = Notices::default();
        let mut seen = std::collections::HashSet::new();
        let mut offset = 0u64;
        loop {
            let path = if offset > 0 { format!("/api/desktop/notifications?offset={offset}") } else { "/api/desktop/notifications".to_string() };
            let page: NoticePage = self.get(&path).await?;
            for notice in page.notices {
                if seen.insert(notice.identity()) {
                    out.notices.push(notice);
                }
            }
            // la pertenencia cubre toda la proyección; vale la de la última página
            out.active = page.active;
            if !page.truncated {
                if out.active.is_none() {
                    out.active = Some(out.notices.iter().map(DesktopNotice::identity).collect());
                }
                return Ok(out);
            }
            match page.next_offset {
                Some(next) if next > offset => offset = next,
                _ => return Ok(out),
            }
        }
    }

    // ---- proveedores, cuentas y ajustes (#30)

    /// `GET /api/providers` — `getProviders` (api.ts): cada proveedor con su
    /// CLI, sus tiers y si está instalado y con sesión en esta máquina.
    pub async fn providers(&self) -> Result<ProvidersPayload, ClientError> {
        self.get("/api/providers").await
    }

    /// `PUT /api/providers/{id}/enabled` `{enabled}` — `setProviderEnabled`
    /// (api.ts): el interruptor del usuario; responde el documento entero.
    pub async fn set_provider_enabled(&self, provider: &str, enabled: bool) -> Result<ProvidersPayload, ClientError> {
        self.put(&format!("/api/providers/{}/enabled", encode(provider)), &serde_json::json!({ "enabled": enabled })).await
    }

    /// `GET /api/app-settings/runtime` — `getRuntimeSettings` (api.ts).
    pub async fn runtime_settings(&self) -> Result<RuntimeSettings, ClientError> {
        self.get("/api/app-settings/runtime").await
    }

    /// `PUT /api/app-settings/runtime` con una sola clave, como cada `set…` de
    /// api.ts (`{max_concurrent_turns}`, `{working_checkups_enabled}`…; el
    /// calentamiento es `{enabled}`). Responde los ajustes enteros.
    pub async fn set_runtime(&self, key: &str, value: serde_json::Value) -> Result<RuntimeSettings, ClientError> {
        let mut body = serde_json::Map::new();
        body.insert(key.to_string(), value);
        self.put("/api/app-settings/runtime", &serde_json::Value::Object(body)).await
    }

    /// `GET /api/accounts` — `useAccountRegistry` (accountsregistry.tsx).
    pub async fn accounts(&self) -> Result<AccountRegistry, ClientError> {
        self.get("/api/accounts").await
    }

    /// `POST /api/accounts` — `AddAccountDialog`: una cuenta administrada, una
    /// carpeta importada o una clave de API.
    pub async fn add_account(&self, account: &NewAccount) -> Result<AccountRow, ClientError> {
        let body = serde_json::to_value(account).expect("NewAccount siempre se serializa");
        self.post("/api/accounts", &body).await
    }

    /// `GET /api/accounts/{id}/identity` — el "refresh" de una cuenta: lee su
    /// perfil y actualiza su estado de sesión.
    pub async fn account_identity(&self, id: &str) -> Result<AccountIdentity, ClientError> {
        self.get(&format!("/api/accounts/{}/identity", encode(id))).await
    }

    /// `DELETE /api/accounts/{id}` — quita la cuenta y pasa sus agentes a la
    /// cuenta por defecto del proveedor.
    pub async fn remove_account(&self, id: &str) -> Result<RemovedAccount, ClientError> {
        self.delete(&format!("/api/accounts/{}", encode(id))).await
    }

    /// `POST /api/orgs/{slug}/settings` — `saveSettings` (api.ts).
    pub async fn save_org_settings(&self, slug: &str, body: &serde_json::Value) -> Result<SettingsResult, ClientError> {
        self.post(&format!("/api/orgs/{}/settings", encode(slug)), body).await
    }

    /// `GET /api/orgs/{slug}/orgmd` — `getOrgMd` (api.ts): el charter de la org.
    pub async fn org_md(&self, slug: &str) -> Result<OrgMd, ClientError> {
        self.get(&format!("/api/orgs/{}/orgmd", encode(slug))).await
    }

    /// `PUT /api/orgs/{slug}/orgmd` `{content}` — `putOrgMd` (api.ts).
    pub async fn put_org_md(&self, slug: &str, content: &str) -> Result<SettingsResult, ClientError> {
        self.put(&format!("/api/orgs/{}/orgmd", encode(slug)), &serde_json::json!({ "content": content })).await
    }

    /// `GET /api/orgs` — la ventana de inicio.
    pub async fn list_orgs(&self) -> Result<Vec<OrgListEntry>, ClientError> {
        self.get("/api/orgs").await
    }

    /// `GET /api/orgs/{slug}` — el árbol completo (sin `view=delta`).
    pub async fn tree(&self, slug: &str) -> Result<TreePayload, ClientError> {
        self.get(&format!("/api/orgs/{}", encode(slug))).await
    }

    /// `GET /api/orgs/{slug}/nodes/{nid}/chat?last=N[&before=cursor]` — el desk.
    pub async fn chat(&self, slug: &str, node: &str, last: u32, before: Option<&str>) -> Result<ChatPayload, ClientError> {
        let mut path = format!("/api/orgs/{}/nodes/{}/chat?last={last}", encode(slug), encode(node));
        if let Some(cursor) = before {
            path.push_str(&format!("&before={}", encode(cursor)));
        }
        self.get(&path).await
    }

    /// URL del WebSocket de una org.
    pub fn ws_url(&self, slug: &str) -> String {
        let base = self.origin.replacen("http://", "ws://", 1).replacen("https://", "wss://", 1);
        format!("{base}/api/orgs/{}/ws", encode(slug))
    }

    /// Una conexión: abre con el token en el handshake y entrega frames hasta
    /// que se corta. Devuelve el motivo del corte.
    async fn run_once(&self, slug: &str, events: &mpsc::UnboundedSender<WsEvent>) -> Result<String, ClientError> {
        let mut request = self.ws_url(slug).into_client_request().map_err(|e| ClientError::WebSocket(e.to_string()))?;
        let mut value = HeaderValue::from_str(&self.token).map_err(|e| ClientError::WebSocket(e.to_string()))?;
        value.set_sensitive(true);
        request.headers_mut().insert(TOKEN_HEADER, value);
        let (socket, _) = tokio_tungstenite::connect_async(request)
            .await
            .map_err(|e| ClientError::WebSocket(e.to_string()))?;
        if events.send(WsEvent::Connected).is_err() {
            return Ok("suscripción cerrada".into());
        }
        let (mut sink, mut stream) = socket.split();
        let mut ping = tokio::time::interval(PING_INTERVAL);
        ping.tick().await;
        loop {
            tokio::select! {
                _ = ping.tick() => {
                    if let Err(e) = sink.send(Message::Text("ping".into())).await {
                        return Ok(format!("ping falló: {e}"));
                    }
                }
                message = stream.next() => match message {
                    Some(Ok(Message::Text(text))) => {
                        if let Ok(frame) = Frame::parse(&text) {
                            if events.send(WsEvent::Frame(frame)).is_err() {
                                return Ok("suscripción cerrada".into());
                            }
                        }
                    }
                    Some(Ok(Message::Close(frame))) => {
                        return Ok(frame.map(|f| format!("cerrado {}: {}", u16::from(f.code), f.reason)).unwrap_or_else(|| "cerrado".into()));
                    }
                    Some(Ok(_)) => {}
                    Some(Err(e)) => return Ok(e.to_string()),
                    None => return Ok("fin del stream".into()),
                },
                _ = events.closed() => return Ok("suscripción cerrada".into()),
            }
        }
    }

    /// Se suscribe al WebSocket de una org y reconecta con `backoff` hasta que
    /// se suelta el receptor. Requiere un runtime de tokio.
    pub fn subscribe(&self, slug: &str, backoff: Backoff) -> mpsc::UnboundedReceiver<WsEvent> {
        let (sender, receiver) = mpsc::unbounded_channel();
        let client = self.clone();
        let slug = slug.to_string();
        tokio::spawn(async move {
            let mut attempt = 0u32;
            loop {
                let reason = match client.run_once(&slug, &sender).await {
                    Ok(reason) => {
                        attempt = 0; // abrió: el backoff vuelve al inicio
                        reason
                    }
                    Err(error) => error.to_string(),
                };
                if sender.is_closed() {
                    return;
                }
                let retry_in = backoff.delay(attempt);
                attempt = attempt.saturating_add(1);
                if sender.send(WsEvent::Disconnected { reason, retry_in }).is_err() {
                    return;
                }
                tokio::time::sleep(retry_in).await;
            }
        });
        receiver
    }
}

/// Codifica un segmento de ruta o un valor de query (RFC 3986, no reservados).
fn encode(text: &str) -> String {
    let mut out = String::new();
    for byte in text.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => out.push(byte as char),
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_crece_hasta_el_tope() {
        let backoff = Backoff { initial: Duration::from_millis(100), max: Duration::from_secs(1) };
        let base = |n| backoff.delay(n);
        assert!(base(0) >= Duration::from_millis(100) && base(0) <= Duration::from_millis(120));
        assert!(base(1) >= Duration::from_millis(200));
        assert!(base(3) >= Duration::from_millis(800));
        assert!(base(10) <= Duration::from_millis(1200));
        assert!(base(40) <= Duration::from_millis(1200));
    }

    #[test]
    fn codifica_rutas() {
        assert_eq!(encode("mi org/1"), "mi%20org%2F1");
        assert_eq!(encode("a-b_c.d~"), "a-b_c.d~");
    }

    #[test]
    fn el_token_no_aparece_en_debug() {
        let client = Client::new("http://127.0.0.1:1", "secreto-123").unwrap();
        assert!(!format!("{client:?}").contains("secreto-123"));
        assert_eq!(client.ws_url("x y"), "ws://127.0.0.1:1/api/orgs/x%20y/ws");
    }
}
