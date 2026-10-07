//! Login de proveedores desde la app (#21), como `apps/desktop/main/providerlogin.ts`.
//!
//! El proceso hijo lo lanza el shell, que siempre corre en la sesión
//! interactiva del usuario (el navegador del login se abre ahí), nunca el
//! motor. Reglas:
//!
//! - Solo se ejecuta el CLI de un harness **detectado** por el shell
//!   (`harnesses::detect`), con los argumentos fijos de Electron
//!   (`claude auth login --claudeai`, `codex login`, `agy` en una terminal
//!   visible). Nada de lo que manda la página llega a la línea de comandos: el
//!   proveedor se valida contra la lista, y `profileDir` solo elige la carpeta
//!   de perfil (`CLAUDE_CONFIG_DIR` / `CODEX_HOME`).
//! - El token del escritorio no llega al hijo: el shell no lo tiene en su
//!   entorno (va al motor por `ORGTREE_V2_TOKEN` solo en el proceso del motor)
//!   y además se quitan esas variables del entorno del hijo.
//! - El código pegado va solo al stdin del hijo: nunca a la salida ni a un log.
//! - Un login que termina bien se verifica contra el motor
//!   (`/api/providers?force=true&force_provider=…` o la identidad de la cuenta),
//!   con el header del token, igual que Electron.

use crate::harnesses;
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const PASTE_PROMPT: &str = "Paste code here if prompted > ";
const TOTAL_TIMEOUT: Duration = Duration::from_secs(300);
const OUTPUT_TAIL: usize = 4000;
const VERIFY_RETRIES: usize = 5;
const VERIFY_RETRY_DELAY: Duration = Duration::from_millis(200);

/// Variables que nunca llegan a un CLI de proveedor: el token del escritorio
/// (por si algún día el shell lo tuviera en su entorno) y las del motor.
const SCRUBBED_ENV: [&str; 3] = ["ORGTREE_V2_TOKEN", "ORGTREE_DESKTOP_TOKEN", "ORGTREE_TAURI_PROBE"];

/// Una puerta de login: el id del motor (`/api/providers`), si acepta un
/// código pegado y los argumentos fijos del CLI.
struct Door {
    api_id: &'static str,
    supports_code: bool,
    args: &'static [&'static str],
}

fn door(provider: &str) -> Option<Door> {
    match provider {
        "claude" => Some(Door { api_id: "claude", supports_code: true, args: &["auth", "login", "--claudeai"] }),
        "codex" => Some(Door { api_id: "openai", supports_code: false, args: &["login"] }),
        // Antigravity no tiene una puerta programable: se abre su CLI en una
        // terminal visible y el usuario entra ahí (decisión del usuario, 2026-09-09).
        "antigravity" => Some(Door { api_id: "google", supports_code: false, args: &[] }),
        _ => None,
    }
}

/// `asLoginProvider`: solo los tres proveedores del contrato.
pub fn provider(value: &str) -> Result<&'static str, String> {
    ["claude", "codex", "antigravity"]
        .into_iter()
        .find(|p| *p == value)
        .ok_or_else(|| "Unknown login provider".to_string())
}

fn status(phase: &str, ok: Value, output: &str, age: Duration) -> Map<String, Value> {
    let Value::Object(map) = json!({
        "phase": phase, "ok": ok, "timedOut": false, "output": output, "ageMs": age.as_millis() as u64,
    }) else { unreachable!() };
    map
}

fn idle() -> Value {
    Value::Object(status("idle", Value::Null, "", Duration::ZERO))
}

fn refused(phase: &str, error: &str) -> Value {
    let mut map = status(phase, Value::Bool(false), "", Duration::ZERO);
    map.insert("started".into(), Value::Bool(false));
    map.insert("error".into(), Value::String(error.into()));
    Value::Object(map)
}

/// Cómo verificar contra el motor: su puerto y el token del header.
#[derive(Clone)]
pub struct EngineAccess {
    pub port: u16,
    pub token: String,
}

impl EngineAccess {
    /// GET mínimo a 127.0.0.1 con el header del escritorio; nunca sigue redirecciones.
    fn get(&self, path: &str) -> Option<Value> {
        let address = std::net::SocketAddr::from(([127, 0, 0, 1], self.port));
        let timeout = Duration::from_secs(10);
        let mut stream = std::net::TcpStream::connect_timeout(&address, timeout).ok()?;
        stream.set_read_timeout(Some(timeout)).ok()?;
        let request = format!(
            "GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nConnection: close\r\n{}: {}\r\n\r\n",
            self.port,
            orgtree_engine_host::TOKEN_HEADER,
            self.token
        );
        stream.write_all(request.as_bytes()).ok()?;
        let mut response = Vec::new();
        stream.take(4 << 20).read_to_end(&mut response).ok()?;
        let text = String::from_utf8_lossy(&response);
        let (head, body) = text.split_once("\r\n\r\n")?;
        if head.split_whitespace().nth(1) != Some("200") {
            return None;
        }
        serde_json::from_str(body).ok()
    }
}

#[derive(Default)]
struct SessionState {
    output: String,
    awaiting_code: bool,
    code_sent: bool,
    done: bool,
    ok: Option<bool>,
    timed_out: bool,
    exited: bool,
}

struct Session {
    supports_code: bool,
    started: Instant,
    state: Mutex<SessionState>,
    child: Mutex<Child>,
    stdin: Mutex<Option<ChildStdin>>,
    cancelled: AtomicBool,
}

impl Session {
    fn append(&self, chunk: &[u8]) {
        let mut state = self.state.lock().unwrap();
        state.output.push_str(&String::from_utf8_lossy(chunk));
        let excess = state.output.chars().count().saturating_sub(OUTPUT_TAIL);
        if excess > 0 {
            state.output = state.output.chars().skip(excess).collect();
        }
        if self.supports_code && !state.code_sent && state.output.contains(PASTE_PROMPT) {
            state.awaiting_code = true;
        }
    }

    fn snapshot(&self) -> Map<String, Value> {
        let state = self.state.lock().unwrap();
        let phase = if state.done { "done" } else if state.awaiting_code { "awaiting_code" } else { "starting" };
        let mut map = status(phase, state.ok.map(Value::Bool).unwrap_or(Value::Null), &state.output, self.started.elapsed());
        map.insert("timedOut".into(), Value::Bool(state.timed_out));
        map
    }

    fn is_done(&self) -> bool {
        self.state.lock().unwrap().done
    }

    fn settle(&self, ok: bool) {
        let mut state = self.state.lock().unwrap();
        if !state.done {
            state.done = true;
            state.ok = Some(ok);
            state.awaiting_code = false;
        }
    }

    /// Mata el árbol entero: un `.cmd` corre bajo `cmd.exe`, y matar solo ese
    /// proceso dejaría vivo al CLI (como `killTree` en Electron).
    fn kill_tree(&self) {
        if self.state.lock().unwrap().exited {
            return;
        }
        let mut child = self.child.lock().unwrap();
        #[cfg(windows)]
        {
            let mut taskkill = Command::new("taskkill");
            taskkill.args(["/PID", &child.id().to_string(), "/T", "/F"]).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
            hide_window(&mut taskkill);
            if matches!(taskkill.status(), Ok(s) if s.success()) {
                return;
            }
        }
        let _ = child.kill();
    }

    /// Cancelar es instantáneo: el slot queda libre aunque Windows tarde en
    /// terminar el proceso (A5 en Electron).
    fn cancel(&self, timed_out: bool) {
        self.cancelled.store(true, Ordering::SeqCst);
        if timed_out {
            self.state.lock().unwrap().timed_out = true;
        }
        self.kill_tree();
        self.settle(false);
    }
}

#[cfg(windows)]
fn hide_window(command: &mut Command) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    command.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(windows))]
fn hide_window(_command: &mut Command) {}

/// Los logins en curso, uno por proveedor.
#[derive(Default)]
pub struct Logins {
    sessions: Mutex<HashMap<&'static str, Arc<Session>>>,
}

/// Opciones de `startProviderLogin`: solo dos campos de texto pasan.
pub struct LoginOptions {
    pub profile_dir: Option<String>,
    pub account_id: Option<String>,
}

impl LoginOptions {
    pub fn from_value(value: Option<&Value>) -> LoginOptions {
        let text = |key: &str| value.and_then(|v| v.get(key)).and_then(Value::as_str).filter(|s| !s.is_empty()).map(str::to_string);
        LoginOptions { profile_dir: text("profileDir"), account_id: text("accountId") }
    }
}

/// `verifyProfileWritable`: escribe y borra un archivo en la carpeta de perfil.
fn profile_writable(dir: &str) -> Result<PathBuf, ()> {
    let dir = PathBuf::from(dir);
    if !dir.is_absolute() || !dir.is_dir() {
        return Err(());
    }
    let probe = dir.join(format!(".orgtree-login-write-{}-{}", std::process::id(), Instant::now().elapsed().as_nanos()));
    std::fs::OpenOptions::new().write(true).create_new(true).open(&probe).map_err(|_| ())?;
    std::fs::remove_file(&probe).map_err(|_| ())?;
    Ok(dir)
}

fn valid_account(id: &str) -> bool {
    !id.is_empty() && id.len() <= 128 && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// Abre el CLI de Antigravity en una consola nueva y visible (`start ""`).
#[cfg(windows)]
fn launch_terminal(exe: &std::path::Path) -> Result<(), String> {
    use std::os::windows::process::CommandExt;
    let mut command = Command::new("cmd");
    // `start` es un builtin de cmd; el "" es el título de la ventana. La ruta
    // viene de la detección del shell y no puede contener comillas en Windows.
    let path = exe.to_string_lossy();
    if path.contains('"') {
        return Err("ruta inválida".into());
    }
    command.raw_arg(format!("/c start \"\" \"{path}\"")).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    for key in SCRUBBED_ENV {
        command.env_remove(key);
    }
    command.spawn().map(|_| ()).map_err(|e| format!("failed to start: {e}"))
}

#[cfg(not(windows))]
fn launch_terminal(_exe: &std::path::Path) -> Result<(), String> {
    Err("opening a terminal is only supported on Windows".into())
}

impl Logins {
    /// `startProviderLogin`. Todo corre con el mapa tomado, así que dos
    /// pedidos simultáneos para el mismo proveedor no lanzan dos hijos.
    pub fn start(&self, provider: &'static str, options: LoginOptions, engine: Option<EngineAccess>) -> Value {
        let mut sessions = self.sessions.lock().unwrap();
        if let Some(existing) = sessions.get(provider) {
            if !existing.is_done() {
                let mut snap = existing.snapshot();
                snap.insert("started".into(), Value::Bool(false));
                return Value::Object(snap);
            }
        }
        let door = door(provider).expect("proveedor validado");
        let profile_dir = match options.profile_dir.as_deref().map(profile_writable) {
            Some(Err(())) => {
                return refused(
                    "error",
                    "The selected account profile is not writable. Its folder permissions must allow your Windows user before signing in.",
                )
            }
            Some(Ok(dir)) => Some(dir),
            None => None,
        };
        let account_id = options.account_id.filter(|id| valid_account(id));
        let Some(exe) = harnesses::detect().into_iter().find(|h| h.id == provider).and_then(|h| h.path) else {
            return refused("error", "not-installed");
        };
        if provider == "antigravity" {
            return match launch_terminal(&exe) {
                Ok(()) => {
                    // `ok: null`: no se verifica nada, solo se abrió la terminal.
                    let mut map = status("done", Value::Null, "", Duration::ZERO);
                    map.insert("started".into(), Value::Bool(true));
                    Value::Object(map)
                }
                Err(error) => refused("error", &error),
            };
        }
        let Some(engine) = engine else { return refused("error", "the engine is not running") };
        let mut command = Command::new(&exe);
        command.args(door.args).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
        hide_window(&mut command);
        for key in SCRUBBED_ENV {
            command.env_remove(key);
        }
        match (&profile_dir, door.api_id) {
            (Some(dir), "openai") => {
                command.env("CODEX_HOME", dir);
            }
            (Some(dir), _) => {
                command.env("CLAUDE_CONFIG_DIR", dir);
            }
            // Una cuenta sin carpeta propia es el login importado por defecto de Claude.
            (None, "claude") if account_id.is_some() => {
                command.env_remove("CLAUDE_CONFIG_DIR");
            }
            _ => {}
        }
        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(error) => return refused("error", &format!("failed to start: {error}")),
        };
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let stdin = child.stdin.take();
        let session = Arc::new(Session {
            supports_code: door.supports_code,
            started: Instant::now(),
            state: Mutex::new(SessionState::default()),
            child: Mutex::new(child),
            stdin: Mutex::new(stdin),
            cancelled: AtomicBool::new(false),
        });
        for pipe in [stdout.map(|p| Box::new(p) as Box<dyn Read + Send>), stderr.map(|p| Box::new(p) as Box<dyn Read + Send>)]
            .into_iter()
            .flatten()
        {
            let reader = session.clone();
            std::thread::spawn(move || {
                let mut pipe = pipe;
                let mut buffer = [0u8; 4096];
                while let Ok(n) = pipe.read(&mut buffer) {
                    if n == 0 {
                        break;
                    }
                    reader.append(&buffer[..n]);
                }
            });
        }
        let waiter = session.clone();
        std::thread::spawn(move || wait_and_verify(waiter, door.api_id, account_id, engine));
        sessions.insert(provider, session.clone());
        let mut snap = session.snapshot();
        snap.insert("started".into(), Value::Bool(true));
        Value::Object(snap)
    }

    pub fn status(&self, provider: &'static str) -> Value {
        match self.sessions.lock().unwrap().get(provider) {
            Some(session) => Value::Object(session.snapshot()),
            None => idle(),
        }
    }

    /// `submitProviderLoginCode`: el código va solo al stdin del hijo.
    pub fn submit_code(&self, provider: &'static str, code: &str) -> Result<Value, String> {
        if !door(provider).is_some_and(|d| d.supports_code) {
            return Err(format!("{provider} does not use a pasted code"));
        }
        let session = self.sessions.lock().unwrap().get(provider).cloned().ok_or("no login in progress")?;
        if session.is_done() {
            return Err("this login already finished — start a new one".into());
        }
        let trimmed = code.trim();
        if trimmed.is_empty() {
            return Err("code is empty".into());
        }
        if trimmed.chars().count() > 2048 {
            return Err("code is too long".into());
        }
        {
            let mut state = session.state.lock().unwrap();
            if !state.code_sent {
                state.code_sent = true;
                state.awaiting_code = false;
                drop(state);
                if let Some(stdin) = session.stdin.lock().unwrap().as_mut() {
                    let _ = stdin.write_all(format!("{trimmed}\n").as_bytes());
                    let _ = stdin.flush();
                }
            }
        }
        Ok(Value::Object(session.snapshot()))
    }

    /// `cancelProviderLogin`: mata el árbol y libera el slot en el acto.
    pub fn cancel(&self, provider: &'static str) -> Value {
        if let Some(session) = self.sessions.lock().unwrap().remove(provider) {
            if !session.is_done() {
                session.cancel(false);
            }
        }
        idle()
    }

    /// Al salir de la app, ningún login queda vivo.
    pub fn cancel_all(&self) {
        for (_, session) in self.sessions.lock().unwrap().drain() {
            if !session.is_done() {
                session.cancel(false);
            }
        }
    }
}

fn wait_and_verify(session: Arc<Session>, api_id: &'static str, account: Option<String>, engine: EngineAccess) {
    let exited_clean = loop {
        if session.cancelled.load(Ordering::SeqCst) {
            return;
        }
        if session.started.elapsed() > TOTAL_TIMEOUT {
            session.cancel(true);
            return;
        }
        let polled = session.child.lock().unwrap().try_wait();
        match polled {
            Ok(Some(code)) => break code.success(),
            Ok(None) => std::thread::sleep(Duration::from_millis(100)),
            Err(_) => break false,
        }
    };
    session.state.lock().unwrap().exited = true;
    let mut ok = false;
    if exited_clean {
        for _ in 0..VERIFY_RETRIES {
            if session.cancelled.load(Ordering::SeqCst) {
                break;
            }
            ok = match &account {
                Some(id) => engine
                    .get(&format!("/api/accounts/{id}/identity"))
                    .is_some_and(|doc| doc.get("auth").and_then(Value::as_str) == Some("authenticated")),
                None => engine
                    .get(&format!("/api/providers?force=true&force_provider={api_id}"))
                    .and_then(|doc| doc.get("providers").cloned())
                    .and_then(|rows| rows.as_array().cloned())
                    .is_some_and(|rows| {
                        rows.iter().any(|row| {
                            row.get("id").and_then(Value::as_str) == Some(api_id)
                                && row.pointer("/status/connected") == Some(&Value::Bool(true))
                        })
                    }),
            };
            if ok {
                break;
            }
            std::thread::sleep(VERIFY_RETRY_DELAY);
        }
    }
    // Un cancelar que llegó durante la verificación ya decidió el resultado.
    session.settle(ok);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn solo_proveedores_del_contrato() {
        assert_eq!(provider("codex"), Ok("codex"));
        assert!(provider("codex --help").is_err());
        assert!(provider("../claude").is_err());
    }

    #[test]
    fn cuentas_sin_rutas() {
        assert!(valid_account("acct_1-a"));
        assert!(!valid_account("../x"));
        assert!(!valid_account(""));
    }
}
