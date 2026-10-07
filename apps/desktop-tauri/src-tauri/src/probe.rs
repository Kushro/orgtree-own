//! Prueba de diagnóstico del spike, solo con `ORGTREE_TAURI_PROBE=<archivo>`.
//!
//! Cuando la ventana carga el origen del motor, inyecta `probe.js`, que
//! verifica desde el webview real (WebView2 en el CI):
//!
//! - #3, cookie: `nav` (status de la navegación, 401 = la cookie no llegó),
//!   `http` (`fetch` a `/api/desktop/identity`), `ws` (WebSocket a una org),
//!   y `echo`, un servidor local en otro puerto que registra si llegó la
//!   cookie desde la página (control positivo) y desde un iframe
//!   `sandbox="allow-scripts"` como los del HTML de agentes (no debe llevarla);
//! - #4, ventana de inicio: el shim de `window.orgtreeDesktop` existe en la
//!   página y responde por `invoke`, y no existe en iframes ni popouts; si el
//!   motor sirve el renderer, la lista de orgs muestra la org del fixture y
//!   abrirla lleva a su organigrama;
//! - #6, popouts: la secuencia de `renderer/src/popout.tsx` (`window.open`,
//!   shell escrito en el hijo, estilos clonados, DOM del dueño movido al hijo,
//!   borrador compartido).
//!
//! - #5, desk en vivo: abre el desk del agente del fixture, espera frames
//!   `node_stream` (texto que crece), carga la conversación larga y mide los
//!   cuadros de un scroll de punta a punta; después abre el desk en una ventana
//!   aparte con su botón real y verifica tipografía y borrador.
//!
//! El script devuelve el resultado en `document.title`, sin IPC. Después Rust
//! pide cerrar la ventana dueña y anota qué pasó con el popout (etapa
//! `owner_close`), y luego mata el motor como si se cayera: el shell lo
//! reinicia y la página recargada corre `probe_reconnect.js` (etapa
//! `reconnect`). El reporte se escribe en cada etapa; el token no se escribe
//! nunca, solo si la cookie llegó o no.

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

pub const PAGE_PREFIX: &str = "orgtree-probe:";
pub const CLOSE_PREFIX: &str = "orgtree-probe-closed:";
pub const RECONNECT_PREFIX: &str = "orgtree-probe-reconnected:";
pub const PAUSE_PREFIX: &str = "orgtree-probe-pause:";

pub struct Probe {
    out: PathBuf,
    echo_port: u16,
    hits: Arc<Mutex<Vec<serde_json::Value>>>,
    page: Mutex<Option<serde_json::Value>>,
    close: Mutex<Option<serde_json::Value>>,
    /// Después de matar el motor, la próxima carga corre el script de reconexión.
    reconnecting: std::sync::atomic::AtomicBool,
    crashed_pid: Mutex<Option<u32>>,
}

impl Probe {
    pub fn from_env() -> Option<Probe> {
        let out = PathBuf::from(std::env::var_os("ORGTREE_TAURI_PROBE").filter(|v| !v.is_empty())?);
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
        Some(Probe {
            out,
            echo_port,
            hits,
            page: Mutex::new(None),
            close: Mutex::new(None),
            reconnecting: std::sync::atomic::AtomicBool::new(false),
            crashed_pid: Mutex::new(None),
        })
    }

    /// El script que corresponde a esta carga de la página: la prueba completa
    /// la primera vez, la de reconexión después de la caída del motor.
    pub fn script_for_load(&self) -> String {
        if self.reconnecting.load(std::sync::atomic::Ordering::SeqCst) {
            include_str!("probe_reconnect.js").replace("__PREFIX__", RECONNECT_PREFIX)
        } else {
            self.script()
        }
    }

    pub fn begin_reconnect(&self) {
        self.reconnecting.store(true, std::sync::atomic::Ordering::SeqCst);
    }

    pub fn note_crashed_pid(&self, pid: Option<u32>) {
        *self.crashed_pid.lock().unwrap() = pid;
    }

    /// Script de la página del motor.
    pub fn script(&self) -> String {
        include_str!("probe.js")
            .replace("__ECHO__", &self.echo_port.to_string())
            .replace("__PREFIX__", PAGE_PREFIX)
    }

    /// Script que reporta, ya cerrada la ventana dueña, si el popout sigue vivo
    /// y su borrador sigue en el mismo contexto de JavaScript.
    pub fn close_script() -> String {
        format!(
            "(() => {{ const w = window.__orgtreeProbePopout; let r = {{}};\
             try {{ r.childOpen = !!w && !w.closed; r.draft = w.document.querySelector('textarea').value }} catch (e) {{ r.error = String(e) }}\
             document.title = '{CLOSE_PREFIX}' + JSON.stringify(r) }})();"
        )
    }

    /// La página pide una captura (`orgtree-probe-pause:<nombre>`): deja el
    /// archivo `<salida>.<nombre>` para que el CI saque la captura en ese momento.
    pub fn record_pause(&self, title: &str) {
        let Some(name) = title.strip_prefix(PAUSE_PREFIX) else { return };
        let mut marker = self.out.clone().into_os_string();
        marker.push(format!(".{name}"));
        let _ = std::fs::write(marker, b"");
    }

    /// Guarda el resultado de la página. Devuelve `true` la primera vez, para
    /// que el shell siga con la etapa de cierre de la ventana dueña.
    pub fn record_page(&self, title: &str, shell: serde_json::Value) -> bool {
        let Some(json) = title.strip_prefix(PAGE_PREFIX) else { return false };
        let mut page = self.page.lock().unwrap();
        if page.is_some() {
            return false;
        }
        let mut value: serde_json::Value = serde_json::from_str(json).unwrap_or(serde_json::Value::Null);
        if let Some(object) = value.as_object_mut() {
            // Estado del shell al terminar la página: cuántas ventanas de popout siguen abiertas.
            object.insert("shell_after".into(), shell);
        }
        *page = Some(value);
        // Reporte parcial: si la etapa de cierre no llega, queda esta evidencia.
        let report = serde_json::json!({ "page": page.clone(), "echo": *self.hits.lock().unwrap() });
        let _ = std::fs::write(&self.out, serde_json::to_vec_pretty(&report).unwrap_or_default());
        true
    }

    fn write(&self, reconnect: Option<serde_json::Value>) {
        let mut report = serde_json::json!({
            "page": self.page.lock().unwrap().clone(),
            "echo": *self.hits.lock().unwrap(),
            "owner_close": self.close.lock().unwrap().clone(),
        });
        if let Some(reconnect) = reconnect {
            report["reconnect"] = reconnect;
        }
        let _ = std::fs::write(&self.out, serde_json::to_vec_pretty(&report).unwrap_or_default());
    }

    /// Guarda la etapa de cierre (reporte parcial). Devuelve `true` la primera
    /// vez, para que el shell siga con la etapa de caída del motor.
    pub fn record_close(&self, title: &str, shell: serde_json::Value) -> bool {
        let Some(json) = title.strip_prefix(CLOSE_PREFIX) else { return false };
        let page_side: serde_json::Value = serde_json::from_str(json).unwrap_or(serde_json::Value::Null);
        {
            let mut close = self.close.lock().unwrap();
            if close.is_some() {
                return false;
            }
            *close = Some(serde_json::json!({ "shell": shell, "page": page_side }));
        }
        self.write(None);
        true
    }

    /// Cierra el reporte con la etapa de reconexión y lo escribe completo.
    pub fn record_reconnect(&self, title: &str, shell: serde_json::Value) {
        let Some(json) = title.strip_prefix(RECONNECT_PREFIX) else { return };
        let page_side: serde_json::Value = serde_json::from_str(json).unwrap_or(serde_json::Value::Null);
        let crashed = *self.crashed_pid.lock().unwrap();
        self.write(Some(serde_json::json!({ "shell": shell, "crashed_pid": crashed, "page": page_side })));
    }
}
