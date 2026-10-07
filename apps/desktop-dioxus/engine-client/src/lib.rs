//! Cliente del motor de Orgtree para el shell Dioxus.
//!
//! En Dioxus la UI no carga desde el motor: todas las llamadas salen desde
//! Rust, así que el token viaja como `X-Orgtree-Desktop-Token` en cada pedido
//! HTTP y en el handshake del WebSocket, sin cookies y sin cambios en el
//! motor. El token nunca llega al webview.

mod types;

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
