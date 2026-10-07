//! Pruebas del cliente: reconexión con backoff contra un servidor WebSocket
//! local y, con `ORGTREE_TEST_ENGINE_PYTHON`, contra el motor real.

use orgtree_engine_client::{Backoff, Client, ClientError, Frame, WsEvent};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::handshake::server::{ErrorResponse, Request, Response};
use tokio_tungstenite::tungstenite::http::StatusCode;

const TOKEN: &str = "token-de-prueba";

/// Rechaza los primeros `refuse` handshakes con 503 y después acepta, manda un
/// frame `changed` y cierra. Cuenta handshakes con y sin el header del token.
async fn flaky_server(refuse: usize) -> (u16, Arc<AtomicUsize>, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let attempts = Arc::new(AtomicUsize::new(0));
    let missing_token = Arc::new(AtomicUsize::new(0));
    let (seen, missing) = (attempts.clone(), missing_token.clone());
    tokio::spawn(async move {
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            let n = seen.fetch_add(1, Ordering::SeqCst);
            let missing = missing.clone();
            tokio::spawn(async move {
                let check = move |request: &Request, response: Response| -> Result<Response, ErrorResponse> {
                    if request.headers().get("X-Orgtree-Desktop-Token").and_then(|v| v.to_str().ok()) != Some(TOKEN) {
                        missing.fetch_add(1, Ordering::SeqCst);
                    }
                    if n < refuse {
                        let mut error = ErrorResponse::new(None);
                        *error.status_mut() = StatusCode::SERVICE_UNAVAILABLE;
                        return Err(error);
                    }
                    Ok(response)
                };
                if let Ok(mut socket) = tokio_tungstenite::accept_hdr_async(stream, check).await {
                    use futures_util::SinkExt;
                    let frame = r#"{"type":"changed","org":"demo","rev":1}"#;
                    let _ = socket.send(tokio_tungstenite::tungstenite::Message::Text(frame.into())).await;
                    let _ = socket.close(None).await;
                }
            });
        }
    });
    (port, attempts, missing_token)
}

async fn next(events: &mut tokio::sync::mpsc::UnboundedReceiver<WsEvent>) -> WsEvent {
    tokio::time::timeout(Duration::from_secs(10), events.recv()).await.expect("evento a tiempo").expect("canal abierto")
}

#[tokio::test]
async fn reconecta_con_backoff_creciente_y_manda_el_token_en_cada_handshake() {
    let (port, attempts, missing) = flaky_server(3).await;
    let client = Client::new(format!("http://127.0.0.1:{port}"), TOKEN).unwrap();
    let backoff = Backoff { initial: Duration::from_millis(50), max: Duration::from_millis(400) };
    let mut events = client.subscribe("demo", backoff);

    // tres rechazos: el backoff crece
    let mut waits = Vec::new();
    for _ in 0..3 {
        match next(&mut events).await {
            WsEvent::Disconnected { retry_in, .. } => waits.push(retry_in),
            other => panic!("se esperaba Disconnected, llegó {other:?}"),
        }
    }
    assert!(waits[0] < waits[1] && waits[1] < waits[2], "{waits:?}");
    // el cuarto abre y entrega el frame
    assert_eq!(next(&mut events).await, WsEvent::Connected);
    assert_eq!(next(&mut events).await, WsEvent::Frame(Frame::Changed { org: "demo".into(), rev: 1, org_rev: None }));
    // el servidor cierra: el backoff vuelve al inicio porque la conexión abrió
    match next(&mut events).await {
        WsEvent::Disconnected { retry_in, .. } => assert!(retry_in < waits[1], "{retry_in:?} vs {waits:?}"),
        other => panic!("{other:?}"),
    }
    assert_eq!(next(&mut events).await, WsEvent::Connected);
    assert!(attempts.load(Ordering::SeqCst) >= 5);
    assert_eq!(missing.load(Ordering::SeqCst), 0, "todos los handshakes llevan el token");
    drop(events);
}

/// El motor real (`engine/launch.py`) con una raíz descartable.
#[test]
#[ignore = "necesita ORGTREE_TEST_ENGINE_PYTHON con las dependencias del motor"]
fn motor_real_orgs_arbol_desk_y_websocket() {
    use orgtree_engine_host::{Engine, EngineOptions};
    let python = std::env::var_os("ORGTREE_TEST_ENGINE_PYTHON").expect("definir ORGTREE_TEST_ENGINE_PYTHON");
    let repo = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let engine_dir = std::fs::canonicalize(repo.join("engine")).unwrap();
    let data = std::env::temp_dir().join(format!("orgtree-engine-client-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&data);
    let mut options = EngineOptions::new(python, engine_dir, &data);
    options.silence_timeout = Duration::from_secs(120);
    let engine = Engine::start(&options).expect("el motor real llega a ready");
    let client = Client::new(engine.origin(), engine.token().expose()).unwrap();
    let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();

    let result = runtime.block_on(async {
        // sin token, el TokenGate rechaza
        let anonymous = Client::new(engine.origin(), "token-incorrecto").unwrap();
        match anonymous.list_orgs().await {
            Err(ClientError::Status { status: 401, .. }) => {}
            other => panic!("se esperaba 401, llegó {other:?}"),
        }

        let before = client.list_orgs().await.expect("lista de orgs");
        let created: serde_json::Value = client
            .post("/api/orgs", &serde_json::json!({ "name": "Spike Dioxus", "dirs": [], "net_autoconnect": false }))
            .await
            .expect("crear org");
        let slug = created["slug"].as_str().expect("slug").to_string();
        let after = client.list_orgs().await.unwrap();
        assert_eq!(after.len(), before.len() + 1);
        assert!(after.iter().any(|o| o.slug == slug));

        let tree = client.tree(&slug).await.expect("árbol");
        assert_eq!(tree.slug, slug);
        eprintln!("árbol de {slug}: {} nodos, sync_rev {:?}", tree.nodes().len(), tree.sync_rev);
        if let Some(node) = tree.nodes().first() {
            let chat = client.chat(&slug, &node.id, 300, None).await.expect("desk");
            eprintln!("desk de {}: {} mensajes", node.id, chat.messages.len());
        }

        let mut events = client.subscribe(&slug, Backoff::default());
        assert_eq!(next(&mut events).await, WsEvent::Connected);
        let _: serde_json::Value = client
            .post(&format!("/api/orgs/{slug}/settings"), &serde_json::json!({ "compact_at": 80 }))
            .await
            .expect("cambiar un ajuste de la org");
        loop {
            match next(&mut events).await {
                WsEvent::Frame(Frame::Changed { org, rev, .. }) if org == slug => {
                    eprintln!("frame changed recibido: rev {rev}");
                    break;
                }
                WsEvent::Frame(other) => eprintln!("otro frame: {other:?}"),
                other => panic!("{other:?}"),
            }
        }
    });
    let _ = engine.stop();
    let _ = std::fs::remove_dir_all(&data);
    result
}
