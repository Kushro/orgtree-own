//! Pruebas de las operaciones del organigrama (#26) y del desk (#27) contra un
//! servidor HTTP falso: cada método usa la misma ruta, el mismo verbo y el
//! mismo cuerpo que el renderer (`apps/desktop/renderer/src/api.ts`), con el
//! token como header.

use orgtree_engine_client::{Client, ClientError, OpRequest, SendMessage};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

const TOKEN: &str = "token-de-prueba";

#[derive(Debug, Clone)]
struct Seen {
    method: String,
    path: String,
    token: Option<String>,
    body: Option<Value>,
}

type Log = Arc<Mutex<Vec<Seen>>>;

/// Un servidor HTTP/1.1 mínimo: anota cada pedido y responde con `reply(method, path)`.
async fn fake_engine(reply: fn(&str, &str) -> (u16, Value)) -> (Client, Log) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let log: Log = Arc::default();
    let seen = log.clone();
    tokio::spawn(async move {
        loop {
            let (mut stream, _) = listener.accept().await.unwrap();
            let seen = seen.clone();
            tokio::spawn(async move {
                let mut buffer = Vec::new();
                let mut chunk = [0u8; 4096];
                let head_end = loop {
                    let n = stream.read(&mut chunk).await.unwrap();
                    if n == 0 {
                        return;
                    }
                    buffer.extend_from_slice(&chunk[..n]);
                    if let Some(i) = buffer.windows(4).position(|w| w == b"\r\n\r\n") {
                        break i + 4;
                    }
                };
                let head = String::from_utf8_lossy(&buffer[..head_end]).to_string();
                let mut lines = head.split("\r\n");
                let mut first = lines.next().unwrap().split(' ');
                let (method, path) = (first.next().unwrap().to_string(), first.next().unwrap().to_string());
                let mut length = 0;
                let mut token = None;
                for line in lines {
                    if let Some((name, value)) = line.split_once(':') {
                        match name.trim().to_ascii_lowercase().as_str() {
                            "content-length" => length = value.trim().parse().unwrap(),
                            "x-orgtree-desktop-token" => token = Some(value.trim().to_string()),
                            _ => {}
                        }
                    }
                }
                while buffer.len() < head_end + length {
                    let n = stream.read(&mut chunk).await.unwrap();
                    buffer.extend_from_slice(&chunk[..n]);
                }
                let body = &buffer[head_end..head_end + length];
                let body = (!body.is_empty()).then(|| serde_json::from_slice(body).unwrap());
                let (status, payload) = reply(&method, &path);
                seen.lock().unwrap().push(Seen { method, path, token, body });
                let text = payload.to_string();
                let response = format!(
                    "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{text}",
                    text.len()
                );
                let _ = stream.write_all(response.as_bytes()).await;
            });
        }
    });
    (Client::new(format!("http://127.0.0.1:{port}"), TOKEN).unwrap(), log)
}

fn last(log: &Log) -> Seen {
    log.lock().unwrap().last().cloned().expect("hubo un pedido")
}

fn ok(method: &str, path: &str) -> (u16, Value) {
    match (method, path) {
        ("POST", "/api/orgs") => (200, json!({ "slug": "prueba-dioxus" })),
        ("DELETE", _) => (200, json!({ "ok": true, "net": { "unregistered": [] } })),
        ("POST", p) if p.ends_with("/ops") => (200, json!({ "node": "jefe", "warnings": ["aviso"] })),
        ("POST", p) if p.ends_with("/unhalt") => (200, json!({ "node": "a", "unhalted": true })),
        ("POST", p) if p.ends_with("/halt") => {
            (200, json!({ "node": "a", "halted": true, "settled": true, "status": "halted; no turn can run until explicit unhalt" }))
        }
        ("POST", p) if p.ends_with("/interrupt") => {
            (200, json!({ "interrupted": false, "reason": "the turn is admitted but no provider call is active" }))
        }
        ("POST", p) if p.ends_with("/message") => (200, json!({
            "accepted": true, "queued": 1, "halted": true, "deferred": "halted", "id": "f47beb2b74b8",
            "delivery": "QUEUED, NOT READ: worker is halted."
        })),
        ("POST", p) if p.ends_with("/scope") => (200, json!({
            "scope": { "effort": "low" }, "warnings": [],
            "effort_delivery": { "delivery": "next_turn", "effort": "low" }
        })),
        _ => (404, json!({ "detail": "no existe" })),
    }
}

#[tokio::test]
async fn crear_y_borrar_orgs_como_el_renderer() {
    let (client, log) = fake_engine(ok).await;
    let created = client.create_org("Prueba Dioxus", &[], true, &[]).await.unwrap();
    assert_eq!(created.slug, "prueba-dioxus");
    let seen = last(&log);
    assert_eq!((seen.method.as_str(), seen.path.as_str()), ("POST", "/api/orgs"));
    assert_eq!(seen.token.as_deref(), Some(TOKEN));
    // net_autoconnect solo viaja cuando es false (createOrg en api.ts)
    assert_eq!(seen.body, Some(json!({ "name": "Prueba Dioxus", "dirs": [] })));

    client.create_org("Sin hub", &["C:/w".into()], false, &["http://h:7370".into()]).await.unwrap();
    assert_eq!(
        last(&log).body,
        Some(json!({ "name": "Sin hub", "dirs": ["C:/w"], "net_autoconnect": false, "net_hubs": ["http://h:7370"] }))
    );

    let deleted = client.delete_org("prueba-dioxus").await.unwrap();
    assert_eq!(deleted["ok"], true);
    let seen = last(&log);
    assert_eq!((seen.method.as_str(), seen.path.as_str(), seen.body), ("DELETE", "/api/orgs/prueba-dioxus", None));
}

#[tokio::test]
async fn operaciones_de_agente_como_el_renderer() {
    let (client, log) = fake_engine(ok).await;
    let hired = client.op("prueba-dioxus", &OpRequest::hire(None, "haiku", "jefe", 5, None)).await.unwrap();
    assert_eq!(hired.node.as_deref(), Some("jefe"));
    assert_eq!(hired.warnings, vec!["aviso".to_string()]);
    let seen = last(&log);
    assert_eq!((seen.method.as_str(), seen.path.as_str()), ("POST", "/api/orgs/prueba-dioxus/ops"));
    assert_eq!(seen.body, Some(json!({ "op": "hire", "parent": null, "tier": "haiku", "grant": 5, "name": "jefe" })));

    client.op("prueba-dioxus", &OpRequest::hire(Some("jefe"), "sonnet", "ayudante", 0, Some("revisa PRs"))).await.unwrap();
    assert_eq!(
        last(&log).body,
        Some(json!({ "op": "hire", "parent": "jefe", "tier": "sonnet", "grant": 0, "name": "ayudante", "charter": "revisa PRs" }))
    );
    for op in ["retire", "rehire", "dissolve"] {
        client.op("prueba-dioxus", &OpRequest::on_node(op, "ayudante")).await.unwrap();
        assert_eq!(last(&log).body, Some(json!({ "op": op, "node": "ayudante" })));
    }
    client.op("prueba-dioxus", &OpRequest::move_to("ayudante", None)).await.unwrap();
    assert_eq!(last(&log).body, Some(json!({ "op": "move", "node": "ayudante", "new_parent": null })));
    client.op("prueba-dioxus", &OpRequest::move_to("ayudante", Some("jefe"))).await.unwrap();
    assert_eq!(last(&log).body, Some(json!({ "op": "move", "node": "ayudante", "new_parent": "jefe" })));

    let halted = client.halt("prueba-dioxus", "ayudante").await.unwrap();
    assert!(halted.halted && halted.settled);
    let seen = last(&log);
    assert_eq!((seen.method.as_str(), seen.path.as_str(), seen.body), ("POST", "/api/orgs/prueba-dioxus/nodes/ayudante/halt", None));
    assert!(client.unhalt("prueba-dioxus", "ayudante").await.unwrap().unhalted);
    assert_eq!(last(&log).path, "/api/orgs/prueba-dioxus/nodes/ayudante/unhalt");
    let interrupted = client.interrupt("prueba-dioxus", "ayudante").await.unwrap();
    assert!(!interrupted.interrupted);
    assert!(interrupted.reason.unwrap().contains("no provider call"));
    assert_eq!(last(&log).path, "/api/orgs/prueba-dioxus/nodes/ayudante/interrupt");
    assert!(log.lock().unwrap().iter().all(|s| s.token.as_deref() == Some(TOKEN)), "todos los pedidos llevan el token");
}

#[tokio::test]
async fn un_rechazo_del_ledger_trae_su_detail() {
    fn refuse(_: &str, _: &str) -> (u16, Value) {
        (422, json!({ "detail": "tier 'haiku' is a Claude tier and the Claude Code CLI is not installed on this machine" }))
    }
    let (client, _) = fake_engine(refuse).await;
    match client.op("x", &OpRequest::hire(None, "haiku", "a", 0, None)).await {
        Err(ClientError::Status { status: 422, detail }) => assert!(detail.contains("not installed"), "{detail}"),
        other => panic!("se esperaba 422, llegó {other:?}"),
    }
}

#[tokio::test]
async fn el_desk_como_el_renderer() {
    let (client, log) = fake_engine(ok).await;
    // enviar: el mismo cuerpo que `sendMessage`, sin los campos vacíos
    let message = SendMessage { text: "Hola desde Dioxus".into(), client_op: Some("dx-1".into()), ..Default::default() };
    let sent = client.send_message("spike-fixture", "worker", &message).await.unwrap();
    let seen = last(&log);
    assert_eq!((seen.method.as_str(), seen.path.as_str()), ("POST", "/api/orgs/spike-fixture/nodes/worker/message"));
    assert_eq!(seen.body, Some(json!({ "text": "Hola desde Dioxus", "client_op": "dx-1" })));
    assert_eq!(sent.mode(), "halted — mail stays unread until unhalt");
    assert_eq!(sent.id.as_deref(), Some("f47beb2b74b8"));
    let notice = SendMessage { text: "aviso".into(), notice: true, attachments: vec!["uploads/a.txt".into()], ..Default::default() };
    client.send_message("spike-fixture", "worker", &notice).await.unwrap();
    assert_eq!(last(&log).body, Some(json!({ "text": "aviso", "attachments": ["uploads/a.txt"], "notice": true })));

    // esfuerzo: `saveScope(slug, nid, { effort })`
    let saved = client.save_scope("spike-fixture", "worker", &json!({ "effort": "low" })).await.unwrap();
    let seen = last(&log);
    assert_eq!((seen.method.as_str(), seen.path.as_str()), ("POST", "/api/orgs/spike-fixture/nodes/worker/scope"));
    assert_eq!(seen.body, Some(json!({ "effort": "low" })));
    assert_eq!(saved.extra["effort_delivery"]["delivery"], "next_turn");

    // cambio de modelo: `{op:'switch_model', node, tier}` por `/ops`
    client.op("spike-fixture", &OpRequest::switch_model("worker", "sonnet")).await.unwrap();
    let seen = last(&log);
    assert_eq!(seen.path, "/api/orgs/spike-fixture/ops");
    assert_eq!(seen.body, Some(json!({ "op": "switch_model", "node": "worker", "tier": "sonnet" })));

    // retirar un mail no leído
    client.retract_mail("spike-fixture", "worker", "f47beb2b74b8").await.unwrap();
    let seen = last(&log);
    assert_eq!((seen.method.as_str(), seen.path.as_str(), seen.body), ("DELETE", "/api/orgs/spike-fixture/nodes/worker/mail/f47beb2b74b8", None));
    assert!(log.lock().unwrap().iter().all(|s| s.token.as_deref() == Some(TOKEN)), "todos los pedidos llevan el token");
}
