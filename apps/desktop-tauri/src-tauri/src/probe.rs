//! Prueba de diagnóstico del spike, solo con `ORGTREE_TAURI_PROBE=<archivo>`.
//!
//! Cuando la ventana carga el origen del motor, inyecta un script que verifica
//! la autenticación por cookie desde el webview real (WebView2 en el CI):
//!
//! - `nav`: status HTTP de la navegación a `/` (401 = la cookie no llegó);
//! - `http`: `fetch('/api/desktop/identity')` desde la página;
//! - `ws`: WebSocket a `/api/orgs/<x>/ws` (`close:4401` = rechazado);
//! - `echo`: pedidos a un servidor local en otro puerto, que registra si llegó
//!   la cookie: desde la página (control positivo, mismo sitio) y desde un
//!   iframe `sandbox="allow-scripts"` como los del HTML de agentes (no debe
//!   llevarla).
//!
//! El script devuelve el resultado en `document.title`, sin IPC, y Rust lo
//! escribe en el archivo junto con lo que vio el servidor de eco. El token no
//! se escribe nunca: solo si la cookie llegó o no.

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

const TITLE_PREFIX: &str = "orgtree-probe:";

pub struct Probe {
    out: PathBuf,
    echo_port: u16,
    hits: Arc<Mutex<Vec<serde_json::Value>>>,
}

impl Probe {
    pub fn from_env() -> Option<Probe> {
        let out = PathBuf::from(std::env::var_os("ORGTREE_TAURI_PROBE")?);
        let listener = TcpListener::bind("127.0.0.1:0").ok()?;
        let echo_port = listener.local_addr().ok()?.port();
        let hits = Arc::new(Mutex::new(Vec::new()));
        let recorded = hits.clone();
        std::thread::Builder::new()
            .name("orgtree-probe-echo".into())
            .spawn(move || {
                for stream in listener.incoming().flatten() {
                    let mut reader = BufReader::new(&stream);
                    let mut request = String::new();
                    let _ = reader.read_line(&mut request);
                    let mut cookie = false;
                    let mut line = String::new();
                    while reader.read_line(&mut line).map(|n| n > 2).unwrap_or(false) {
                        let lower = line.to_ascii_lowercase();
                        if lower.starts_with("cookie:") && lower.contains("orgtree_desktop_token=") {
                            cookie = true;
                        }
                        line.clear();
                    }
                    let path = request.split_whitespace().nth(1).unwrap_or("").to_string();
                    recorded.lock().unwrap().push(serde_json::json!({ "path": path, "cookie": cookie }));
                    let _ = (&stream).write_all(
                        b"HTTP/1.1 204 No Content\r\nAccess-Control-Allow-Origin: *\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                    );
                }
            })
            .ok()?;
        Some(Probe { out, echo_port, hits })
    }

    pub fn script(&self) -> String {
        format!(
            r#"(async () => {{
  const echo = 'http://127.0.0.1:{port}';
  const r = {{}};
  try {{ r.nav = performance.getEntriesByType('navigation')[0].responseStatus }} catch (e) {{ r.nav = String(e) }}
  try {{ r.http = (await fetch('/api/desktop/identity')).status }} catch (e) {{ r.http = String(e) }}
  r.ws = await new Promise(done => {{
    const socket = new WebSocket(`ws://${{location.host}}/api/orgs/probe/ws`);
    const timer = setTimeout(() => done('timeout'), 10000);
    socket.onopen = () => {{ clearTimeout(timer); socket.close(); done('open') }};
    socket.onclose = event => {{ clearTimeout(timer); done('close:' + event.code) }};
  }});
  try {{ await fetch(echo + '/top', {{ mode: 'no-cors', credentials: 'include' }}) }} catch (e) {{}}
  r.iframe = await new Promise(done => {{
    const frame = document.createElement('iframe');
    frame.setAttribute('sandbox', 'allow-scripts');
    frame.srcdoc = `<script>fetch('${{echo}}/iframe', {{ mode: 'no-cors', credentials: 'include' }})
      .then(() => 'sent', e => 'error:' + e)
      .then(result => parent.postMessage({{ probeIframe: result, origin: String(self.origin) }}, '*'))<\/script>`;
    addEventListener('message', event => {{ if (event.data && event.data.probeIframe) done(event.data) }});
    setTimeout(() => done('timeout'), 10000);
    document.documentElement.appendChild(frame);
  }});
  document.title = '{prefix}' + JSON.stringify(r);
}})();"#,
            port = self.echo_port,
            prefix = TITLE_PREFIX
        )
    }

    pub fn record_title(&self, title: &str) {
        let Some(json) = title.strip_prefix(TITLE_PREFIX) else { return };
        let page: serde_json::Value = serde_json::from_str(json).unwrap_or(serde_json::Value::Null);
        let report = serde_json::json!({ "page": page, "echo": *self.hits.lock().unwrap() });
        let _ = std::fs::write(&self.out, serde_json::to_vec_pretty(&report).unwrap_or_default());
    }
}
