//! Pruebas de las operaciones del organigrama (#26) y del desk (#27) contra un
//! servidor HTTP falso: cada método usa la misma ruta, el mismo verbo y el
//! mismo cuerpo que el renderer (`apps/desktop/renderer/src/api.ts`), con el
//! token como header.

use orgtree_engine_client::{
    BatchAnswer, Client, ClientError, NewAccount, NoticeIdentity, Offer, OpRequest, OrgSettings, QuickStaffSelection, SendMessage, TreePayload, WorkReply,
};
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
        ("DELETE", p) if !p.starts_with("/api/accounts/") => (200, json!({ "ok": true, "net": { "unregistered": [] } })),
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
        // bandeja, preguntas y atención (#28)
        ("GET", "/api/orgs/spike-fixture/inbox") => (200, json!({
            "pending": [{ "id": "m1", "from": "worker", "kind": "message", "body": "El CI está rojo", "at": "2026-10-07T12:00:00Z",
                          "urgent": true, "urgent_reason": "necesito una decisión" }],
            "delivered": [], "sent": []
        })),
        ("POST", p) if p.ends_with("/inbox/read") => (200, json!({ "read": 1 })),
        ("POST", p) if p.ends_with("/inbox/clear") => (200, json!({ "ok": true })),
        ("POST", p) if p.ends_with("/batch") => (200, json!({ "resolved": "worker" })),
        ("POST", p) if p.ends_with("/answer") => (200, json!({ "answered": "a1", "node": "worker" })),
        ("GET", p) if p.ends_with("/work-items-view") => (200, json!({
            "items": [], "counts": {}, "now": "",
            "attention": [{ "slug": "firmar-instalador", "title": "Firmar", "status": "in_progress",
                            "owner": { "node": "worker" }, "attention_sources": ["manual"],
                            "manual_attention": { "reason": "¿certificado?", "at": "2026-10-07T12:00:00Z", "by": { "node": "worker" }, "set_rev": 3 } }]
        })),
        ("POST", p) if p.ends_with("/dismiss-attention") => (200, json!({ "dismissed": "firmar-instalador", "status": "blocked", "pending_questions": 0 })),
        ("POST", p) if p.ends_with("/reply") => (200, json!({ "accepted": true, "to": "worker" })),
        ("GET", "/api/desktop/notifications") => (200, json!({
            "notices": [{ "id": "n1", "org": "spike-fixture", "kind": "question", "title": "Question from worker", "body": "¿Publico?", "agent": "worker", "source_id": "a1" }],
            "total": 2, "truncated": true, "next_offset": 1,
            "active": [{ "org": "spike-fixture", "id": "n1" }, { "org": "spike-fixture", "id": "n2" }]
        })),
        ("GET", "/api/desktop/notifications?offset=1") => (200, json!({
            "notices": [{ "id": "n2", "org": "spike-fixture", "kind": "work-attention", "title": "Firmar", "body": "¿certificado?", "item": "firmar-instalador", "rev": 3 }],
            "total": 2, "truncated": false,
            "active": [{ "org": "spike-fixture", "id": "n1" }, { "org": "spike-fixture", "id": "n2" }]
        })),
        ("GET", "/api/orgs/spike-fixture") => (200, json!({
            "slug": "spike-fixture", "name": "spike-fixture",
            "roots": [{ "id": "worker", "state": "live", "ask": { "id": 7, "node": "worker", "kind": "batch", "status": "open", "at": "2026-10-07T12:00:00Z",
                        "tabs": [{ "kind": "question", "question": "¿Publico?", "header": "Release", "options": ["Sí", { "label": "Todavía no", "description": "esperar" }] }],
                        "revs": { "ask": 1 } } }],
            // una forma rara en la cabecera no rompe el árbol
            "asks": "no es una lista"
        })),
        // docket (#29)
        ("GET", "/api/orgs/spike-fixture/work-items-view?archived=1&backlogged=1") => (200, json!({
            "counts": { "attention": 0, "active": 1, "archived": 1, "backlogged": 1 }, "now": "", "revision": "r1",
            "items": [{ "slug": "empaquetar-el-runtime", "title": "Empaquetar", "status": "in_progress", "rev": 8, "view": "list",
                        "owner": { "node": "worker" }, "done_so_far": ["armé"], "working_on_next": [], "parent": null }],
            "archived": [{ "slug": "probar-en-windows-10", "title": "Win10", "status": "dropped", "archived": true, "dropped_reason": "fuera" }],
            "backlogged": [{ "slug": "medir-la-memoria", "title": "Memoria", "status": "backlogged", "owner": null }]
        })),
        ("GET", "/api/orgs/spike-fixture/work-items/empaquetar-el-runtime") => (200, json!({ "item": {
            "slug": "empaquetar-el-runtime", "title": "Empaquetar", "status": "in_progress", "rev": 8,
            "scope": [{ "seq": 1, "at": "2026-10-07T10:00:00Z", "by": { "node": "worker" }, "kind": "decision", "text": "Runtime embebido", "supersedes": null, "superseded_by": null }],
            "evidence": [{ "at": "2026-10-07T10:00:00Z", "by": { "node": "worker" }, "kind": "commit", "ref": "6e53697", "note": "armado" }],
            "artifacts": [{ "id": "r1", "name": "medicion.txt", "bytes": 36, "sha256": "sha256:9f", "scope": "item", "visible": true },
                          { "visible": false, "scope": "named" }],
            "holders": [{ "node": "jefe", "generation": 0, "from": "2026-10-07T10:00:00Z", "derived": true }, { "node": "worker", "generation": 0 }],
            "history": [{ "at": "2026-10-07T10:00:00Z", "by": "@user", "op": "assign", "from": { "node": "jefe" }, "to": { "node": "worker" } }],
            "reply_recipients": [{ "node": "worker", "role": "owner", "state": "live" }],
            // una sección con una forma rara no rompe el ticket
            "questions": "no es una lista"
        } })),
        ("GET", p) if p.ends_with("/quick-staff") => (200, json!({
            "mode": "top_level", "configured_mode": "request", "owner": {}, "fallback": true, "disclosure": "Assignee unavailable",
            "models": [{ "tier": "haiku", "seat": 1, "efforts": ["low", "high"], "accounts": [], "default_ok": true }]
        })),
        ("POST", p) if p.ends_with("/quick-staff") => (200, json!({ "message": "Staffed medir-la-memoria at top level; ticket moved to Open.", "assigned_to": "medir-la-memoria" })),
        ("GET", p) if p.contains("/artifacts/") => (200, json!("runtime 62.8 MB")),
        // proveedores, cuentas y ajustes (#30)
        ("GET", "/api/providers") | ("PUT", "/api/providers/openai/enabled") => (200, json!({
            "providers": [
                { "id": "claude", "label": "Claude", "cli": "claude", "hire_enabled": true, "reason": null,
                  "status": { "installed": true, "connected": true, "version": "2.1.0" },
                  "tiers": [{ "tier": "haiku", "provider": "claude", "seat": 1, "model": "claude-haiku-4-5", "letter": "H" }] },
                { "id": "openai", "label": "Codex", "cli": "codex", "hire_enabled": false, "reason": "codex is not signed in",
                  "status": { "installed": true, "connected": false }, "tiers": [], "user_enabled": true },
                { "id": "google", "label": "Antigravity", "cli": "agy", "hire_enabled": false, "reason": "not installed",
                  "status": { "installed": false }, "tiers": [] },
                { "id": "openrouter", "label": "OpenRouter", "cli": "", "hire_enabled": true, "user_enabled": false,
                  "status": { "installed": true, "key_set": true }, "tiers": [] },
                // un proveedor con una forma rara no rompe la lista
                "no es un proveedor"
            ],
            "apikey_fallback": { "claude": false }
        })),
        ("GET", "/api/app-settings/runtime") | ("PUT", "/api/app-settings/runtime") => (200, json!({
            "max_concurrent_turns": 8, "turn_slots": { "limit": 8, "held": 1, "waiting": 0, "waiting_by_org": {} },
            "warming_enabled": true, "working_checkups_enabled": true, "wait_for_mcp_tools_enabled": false,
            "idle_docket_reminders_enabled": false, "blocked_docket_reminders_enabled": false, "git_periodic_fetch_enabled": false
        })),
        ("GET", "/api/accounts") => (200, json!({
            "accounts": [{ "id": "acct_1", "provider": "openai", "label": "codex-2", "name": "codex-2", "ambient": false,
                           "credential": { "kind": "managed", "path": "D:/data/profiles/openai-1" },
                           "identity": { "email": "a@example.com" }, "tint_ordinal": 1,
                           "standing": { "auth": "unauthenticated" }, "bound": [{ "org": "o", "node": "worker", "state": "live" }] }],
            "primary": { "claude": "claude/primary" }
        })),
        ("POST", "/api/accounts") => (200, json!({ "id": "acct_2", "provider": "openai", "name": "codex-3", "label": "codex-3",
                                                   "credential": { "kind": "managed", "path": "D:/data/profiles/openai-2" },
                                                   "standing": { "auth": "unobserved" } })),
        ("GET", "/api/accounts/acct_1/identity") => (200, json!({ "account": "acct_1", "identity": {}, "auth": "unauthenticated" })),
        ("DELETE", "/api/accounts/acct_1") => (200, json!({ "removed": "acct_1", "rebound": [{ "org": "o", "node": "worker" }] })),
        ("POST", "/api/orgs/spike-fixture/settings") => (200, json!({ "dirs": [], "warnings": ["compaction threshold set to 70%"] })),
        ("GET", "/api/orgs/spike-fixture/orgmd") => (200, json!({ "content": "# Charter", "chars": 9, "prompt_max": 6000, "read_truncated": false })),
        ("PUT", "/api/orgs/spike-fixture/orgmd") => (200, json!({ "path": "CLAUDE.md", "bytes": 11, "warnings": [] })),
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

#[tokio::test]
async fn la_bandeja_como_el_renderer() {
    let (client, log) = fake_engine(ok).await;
    let inbox = client.inbox("spike-fixture").await.unwrap();
    assert_eq!(inbox.pending.len(), 1);
    assert_eq!(inbox.pending[0].urgent, Some(true));
    assert_eq!(inbox.pending[0].urgent_reason.as_deref(), Some("necesito una decisión"));
    assert_eq!(last(&log).path, "/api/orgs/spike-fixture/inbox");

    // leer: `markRead(slug, [id])`
    client.mark_read("spike-fixture", &["m1".to_string()]).await.unwrap();
    let seen = last(&log);
    assert_eq!((seen.method.as_str(), seen.path.as_str()), ("POST", "/api/orgs/spike-fixture/inbox/read"));
    assert_eq!(seen.body, Some(json!({ "ids": ["m1"] })));
    // archivar todo: `clearInbox`, sin cuerpo
    client.clear_inbox("spike-fixture").await.unwrap();
    let seen = last(&log);
    assert_eq!((seen.method.as_str(), seen.path.as_str(), seen.body), ("POST", "/api/orgs/spike-fixture/inbox/clear", None));
    // responder: `replyMessage` al remitente, con la identidad del mail como `target`
    client.reply_mail("spike-fixture", "worker", "Reintentá", "m1", Some("dx-r1".into())).await.unwrap();
    let seen = last(&log);
    assert_eq!(seen.path, "/api/orgs/spike-fixture/nodes/worker/message");
    assert_eq!(seen.body, Some(json!({
        "text": "Reintentá", "client_op": "dx-r1",
        "target": { "kind": "mail", "org": "spike-fixture", "box": "user", "id": "m1" }
    })));
    assert!(log.lock().unwrap().iter().all(|s| s.token.as_deref() == Some(TOKEN)), "todos los pedidos llevan el token");
}

#[tokio::test]
async fn preguntas_y_banderas_como_el_renderer() {
    let (client, log) = fake_engine(ok).await;
    // la tarjeta compuesta llega en el árbol; una cabecera rara no lo rompe
    let tree = client.tree("spike-fixture").await.unwrap();
    let ask = tree.roots[0].ask.clone().expect("la tarjeta del agente");
    assert!(ask.is_open() && ask.id == "7" && ask.tabs.len() == 1);
    assert_eq!(ask.tabs[0].options[0].label, "Sí");
    assert_eq!(ask.tabs[0].options[1].description.as_deref(), Some("esperar"));
    assert!(tree.asks.is_empty());

    // responder: `resolveBatch` con las respuestas por posición
    let answer = BatchAnswer { revs: ask.revs.clone(), answers: Some(vec![json!("Sí")]), ..Default::default() };
    client.resolve_batch("spike-fixture", "worker", &answer).await.unwrap();
    let seen = last(&log);
    assert_eq!((seen.method.as_str(), seen.path.as_str()), ("POST", "/api/orgs/spike-fixture/nodes/worker/batch"));
    assert_eq!(seen.body, Some(json!({ "revs": { "ask": 1 }, "answers": ["Sí"] })));
    // descartar: la ✕ salta todas las pestañas (`null` explícito)
    let skip = BatchAnswer { revs: ask.revs.clone(), answers: Some(vec![Value::Null]), ..Default::default() };
    client.resolve_batch("spike-fixture", "worker", &skip).await.unwrap();
    assert_eq!(last(&log).body, Some(json!({ "revs": { "ask": 1 }, "answers": [null] })));
    // una pregunta suelta: `answerAsk(slug, aid, { dismiss: true })`
    client.answer_ask("spike-fixture", "a1", &json!({ "dismiss": true })).await.unwrap();
    let seen = last(&log);
    assert_eq!((seen.path.as_str(), seen.body), ("/api/orgs/spike-fixture/asks/a1/answer", Some(json!({ "dismiss": true }))));

    // la bandera de un ticket: leer, responder y descartar
    let work = client.work_items("spike-fixture").await.unwrap();
    let flagged = &work.attention.as_ref().unwrap()[0];
    assert_eq!(flagged.manual_attention.as_ref().unwrap().set_rev, 3);
    assert_eq!(flagged.owner_node().as_deref(), Some("worker"));
    client.reply_work_item("spike-fixture", "firmar-instalador", "Usá el de prueba").await.unwrap();
    let seen = last(&log);
    assert_eq!(seen.path, "/api/orgs/spike-fixture/work-items/firmar-instalador/reply");
    assert_eq!(seen.body, Some(json!({ "body": "Usá el de prueba" })));
    let dismissed = client.dismiss_attention("spike-fixture", "firmar-instalador", 3).await.unwrap();
    assert_eq!(dismissed.status.as_deref(), Some("blocked"));
    let seen = last(&log);
    assert_eq!(seen.path, "/api/orgs/spike-fixture/work-items/firmar-instalador/dismiss-attention");
    assert_eq!(seen.body, Some(json!({ "set_rev": 3 })));
}

#[tokio::test]
async fn las_notificaciones_se_leen_con_todas_sus_paginas() {
    let (client, log) = fake_engine(ok).await;
    let notices = client.notifications().await.unwrap();
    let ids: Vec<&str> = notices.notices.iter().map(|n| n.id.as_str()).collect();
    assert_eq!(ids, ["n1", "n2"]);
    assert_eq!(notices.notices[1].item.as_deref(), Some("firmar-instalador"));
    let active = notices.active.unwrap();
    assert!(active.contains(&NoticeIdentity { org: "spike-fixture".into(), id: "n2".into() }));
    let paths: Vec<String> = log.lock().unwrap().iter().map(|s| s.path.clone()).collect();
    assert_eq!(paths, ["/api/desktop/notifications", "/api/desktop/notifications?offset=1"]);
}

#[tokio::test]
async fn el_docket_como_el_renderer() {
    let (client, log) = fake_engine(ok).await;
    // la lista con los dos grupos pedidos aparte, y sus totales
    let work = client.work_items_view("spike-fixture", true, true).await.unwrap();
    assert_eq!(last(&log).path, "/api/orgs/spike-fixture/work-items-view?archived=1&backlogged=1");
    let counts = work.counts.clone().unwrap();
    assert_eq!((counts.active, counts.archived, counts.backlogged), (1, 1, 1));
    assert_eq!(work.items[0].view.as_deref(), Some("list"));
    assert_eq!(work.archived.as_ref().unwrap()[0].dropped_reason.as_deref(), Some("fuera"));
    assert!(work.backlogged.as_ref().unwrap()[0].owner_node().is_none());
    // sin casillas, la ruta no pide los grupos
    let _ = client.work_items_view("spike-fixture", false, false).await;
    assert_eq!(last(&log).path, "/api/orgs/spike-fixture/work-items-view");

    // el detalle: decisiones, evidencias, artefactos, holders e historial
    let item = client.work_item("spike-fixture", "empaquetar-el-runtime").await.unwrap();
    assert_eq!(last(&log).path, "/api/orgs/spike-fixture/work-items/empaquetar-el-runtime");
    assert_eq!(item.scope[0].text.as_deref(), Some("Runtime embebido"));
    assert_eq!(item.evidence[0].reference.as_deref(), Some("6e53697"));
    assert_eq!(item.artifacts.len(), 2);
    assert_eq!(item.artifacts[1].visible, Some(false));
    assert_eq!(item.holders.iter().map(|h| h.node.as_str()).collect::<Vec<_>>(), ["jefe", "worker"]);
    assert_eq!(item.history.len(), 1);
    assert!(item.questions.is_empty());
    assert_eq!(item.reply_recipients.unwrap()[0].role, "owner");

    // comentar: `replyWorkItem` con el destinatario elegido
    let reply = WorkReply { body: "¿Cómo va?".into(), to: Some("worker".into()), notice: false };
    let sent = client.reply_work_item_to("spike-fixture", "empaquetar-el-runtime", &reply).await.unwrap();
    assert_eq!(sent.to.as_deref(), Some("worker"));
    let seen = last(&log);
    assert_eq!((seen.method.as_str(), seen.path.as_str()), ("POST", "/api/orgs/spike-fixture/work-items/empaquetar-el-runtime/reply"));
    assert_eq!(seen.body, Some(json!({ "body": "¿Cómo va?", "to": "worker" })));
    let notice = WorkReply { body: "fyi".into(), to: None, notice: true };
    client.reply_work_item_to("spike-fixture", "empaquetar-el-runtime", &notice).await.unwrap();
    assert_eq!(last(&log).body, Some(json!({ "body": "fyi", "notice": true })));

    // asignar: "Staff…" en un ticket del backlog, como quickStaffEntry
    let preview = client.quick_staff_preview("spike-fixture", "medir-la-memoria").await.unwrap();
    assert_eq!(last(&log).path, "/api/orgs/spike-fixture/work-items/medir-la-memoria/quick-staff");
    assert_eq!((preview.mode.as_str(), preview.models[0].tier.as_str()), ("top_level", "haiku"));
    let selection = QuickStaffSelection {
        request_id: "7d3f8a6e-1b2c-4d5e-8f90-123456789abc".into(),
        mode: preview.mode.clone(),
        configured_mode: preview.configured_mode.clone(),
        owner: preview.owner.clone(),
        tier: Some("haiku".into()),
        ..QuickStaffSelection::default()
    };
    let staffed = client.quick_staff("spike-fixture", "medir-la-memoria", &selection).await.unwrap();
    assert_eq!(staffed.assigned_to.as_deref(), Some("medir-la-memoria"));
    let seen = last(&log);
    assert_eq!((seen.method.as_str(), seen.path.as_str()), ("POST", "/api/orgs/spike-fixture/work-items/medir-la-memoria/quick-staff"));
    assert_eq!(seen.body, Some(json!({ "request_id": "7d3f8a6e-1b2c-4d5e-8f90-123456789abc", "mode": "top_level",
                                       "configured_mode": "request", "owner": {}, "tier": "haiku" })));

    // un artefacto se descarga crudo, con el token
    let bytes = client.artifact_bytes("spike-fixture", "empaquetar-el-runtime", "r1").await.unwrap();
    assert_eq!(bytes, br#""runtime 62.8 MB""#);
    let seen = last(&log);
    assert_eq!((seen.path.as_str(), seen.token.as_deref()), ("/api/orgs/spike-fixture/work-items/empaquetar-el-runtime/artifacts/r1", Some(TOKEN)));
    // un 404 trae su detail
    let missing = client.work_item("spike-fixture", "no-existe").await.unwrap_err();
    assert!(matches!(missing, ClientError::Status { status: 404, .. }));
}

#[tokio::test]
async fn proveedores_cuentas_y_ajustes_como_el_renderer() {
    let (client, log) = fake_engine(ok).await;

    // proveedores: la lista tolera una entrada rara, y `familyOffer` decide qué se ofrece
    let providers = client.providers().await.unwrap();
    let seen = last(&log);
    assert_eq!((seen.method.as_str(), seen.path.as_str(), seen.token.as_deref()), ("GET", "/api/providers", Some(TOKEN)));
    assert_eq!(providers.providers.len(), 4, "una entrada rara se salta y no borra a las demás");
    let offer = |id: &str| providers.get(id).unwrap().offer();
    assert_eq!(offer("claude"), Offer::Offer);
    assert_eq!(offer("openai"), Offer::Disable, "instalado sin sesión: se ve deshabilitado");
    assert_eq!(offer("google"), Offer::Hide, "no instalado: no aparece");
    assert_eq!(offer("openrouter"), Offer::Hide, "apagado por el usuario: no aparece");
    assert_eq!(providers.get("claude").unwrap().tiers[0].model, "claude-haiku-4-5");
    assert_eq!(providers.apikey_fallback.as_ref().unwrap().get("claude"), Some(&false));
    client.set_provider_enabled("openai", false).await.unwrap();
    let seen = last(&log);
    assert_eq!((seen.method.as_str(), seen.path.as_str()), ("PUT", "/api/providers/openai/enabled"));
    assert_eq!(seen.body, Some(json!({ "enabled": false })));

    // ajustes de la app: una clave por pedido, como cada set… de api.ts
    let runtime = client.runtime_settings().await.unwrap();
    assert_eq!((runtime.max_concurrent_turns, runtime.turn_slots.as_ref().unwrap().held), (Some(8), 1));
    let saved = client.set_runtime("max_concurrent_turns", json!(8)).await.unwrap();
    assert_eq!(saved.working_checkups_enabled, Some(true));
    let seen = last(&log);
    assert_eq!((seen.method.as_str(), seen.path.as_str()), ("PUT", "/api/app-settings/runtime"));
    assert_eq!(seen.body, Some(json!({ "max_concurrent_turns": 8 })));
    client.set_runtime("enabled", json!(false)).await.unwrap();
    assert_eq!(last(&log).body, Some(json!({ "enabled": false })));

    // cuentas: listar, agregar, refrescar y quitar
    let registry = client.accounts().await.unwrap();
    let row = &registry.accounts[0];
    assert_eq!((row.provider.as_str(), row.credential.kind.as_str(), row.standing.auth.as_str()), ("openai", "managed", "unauthenticated"));
    assert_eq!((row.email(), row.bound[0].node.as_str()), (Some("a@example.com"), "worker"));
    let made = client.add_account(&NewAccount { provider: "openai".into(), kind: "managed".into(), path: None, key: None }).await.unwrap();
    assert_eq!(made.id, "acct_2");
    let seen = last(&log);
    assert_eq!((seen.method.as_str(), seen.path.as_str()), ("POST", "/api/accounts"));
    assert_eq!(seen.body, Some(json!({ "provider": "openai", "kind": "managed" })));
    let identity = client.account_identity("acct_1").await.unwrap();
    assert_eq!((identity.auth.as_str(), last(&log).path.as_str()), ("unauthenticated", "/api/accounts/acct_1/identity"));
    let removed = client.remove_account("acct_1").await.unwrap();
    assert_eq!((removed.removed.as_str(), removed.rebound.len()), ("acct_1", 1));
    assert_eq!(last(&log).method, "DELETE");

    // ajustes de la org: leídos del árbol (con los valores por defecto del panel) y guardados
    let tree: TreePayload = serde_json::from_value(json!({
        "slug": "spike-fixture", "name": "spike-fixture", "roots": [],
        "max_top_grant": 1000, "default_top_grant": 50, "compact_at": 0.8, "default_effort": "", "headless": false
    }))
    .unwrap();
    let mut settings = OrgSettings::from_tree(&tree);
    assert_eq!((settings.max_top_grant, settings.default_top_grant, settings.compact_at), (1000, 50, 80));
    assert!(settings.cascade_hire && settings.cascade_alloc && !settings.headless);
    settings.compact_at = 70;
    settings.default_effort = "low".into();
    let result = client.save_org_settings("spike-fixture", &settings.request()).await.unwrap();
    assert_eq!(result.warnings, ["compaction threshold set to 70%"]);
    let seen = last(&log);
    assert_eq!((seen.method.as_str(), seen.path.as_str()), ("POST", "/api/orgs/spike-fixture/settings"));
    assert_eq!(seen.body, Some(json!({ "max_top_grant": 1000, "default_top_grant": 50, "compact_at": 70, "default_effort": "low",
                                       "cascade_hire": true, "cascade_alloc": true })));
    // el charter de la org
    let md = client.org_md("spike-fixture").await.unwrap();
    assert_eq!((md.content.as_str(), md.read_truncated), ("# Charter", false));
    client.put_org_md("spike-fixture", "# Charter 2").await.unwrap();
    let seen = last(&log);
    assert_eq!((seen.method.as_str(), seen.path.as_str()), ("PUT", "/api/orgs/spike-fixture/orgmd"));
    assert_eq!(seen.body, Some(json!({ "content": "# Charter 2" })));
}
