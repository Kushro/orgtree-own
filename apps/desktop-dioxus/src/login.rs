//! Login de proveedores desde la app (#30), como
//! `apps/desktop/main/providerlogin.ts`. Es el código del spike de Tauri
//! (#21), con el estado tipado para la UI en RSX.
//!
//! El proceso hijo lo lanza el shell, que corre en la sesión interactiva del
//! usuario (el navegador del login se abre ahí), nunca el motor. Reglas:
//!
//! - Solo se ejecuta el CLI de un harness **detectado** por el shell
//!   (`harnesses::detect`), con los argumentos fijos de Electron
//!   (`claude auth login --claudeai`, `codex login`, `agy` en una terminal
//!   visible). Nada de lo que escribe la página llega a la línea de comandos:
//!   el proveedor se valida contra la lista, y la carpeta de perfil solo
//!   elige `CLAUDE_CONFIG_DIR` o `CODEX_HOME`.
//! - El token del escritorio no llega al hijo: el shell no lo tiene en su
//!   entorno (va al motor solo en el proceso del motor) y además se quitan
//!   esas variables del entorno del hijo.
//! - El código pegado va solo al stdin del hijo: nunca a la salida ni a un log.
//! - Cancelar, o salir de la app, mata el árbol entero del hijo.
//! - Un login que termina bien se verifica contra el motor
//!   (`/api/providers?force=true&force_provider=…` o la identidad de la
//!   cuenta), con el header del token, igual que Electron.

use crate::harnesses;
use serde::Serialize;
use serde_json::Value;
use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

const PASTE_PROMPT: &str = "Paste code here if prompted > ";
const TOTAL_TIMEOUT: Duration = Duration::from_secs(300);
const OUTPUT_TAIL: usize = 4000;
const VERIFY_RETRIES: usize = 5;
const VERIFY_RETRY_DELAY: Duration = Duration::from_millis(200);

/// Variables que nunca llegan a un CLI de proveedor: el token del escritorio
/// (por si algún día el shell lo tuviera en su entorno) y las de la prueba.
const SCRUBBED_ENV: [&str; 3] = ["ORGTREE_V2_TOKEN", "ORGTREE_DESKTOP_TOKEN", "ORGTREE_DIOXUS_PROBE"];

/// Los tres proveedores del login (`LoginProvider` del contrato).
pub const PROVIDERS: [&str; 3] = ["claude", "codex", "antigravity"];

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

/// `SUPPORTS_CODE` de `accounts.tsx`.
pub fn supports_code(provider: &str) -> bool {
    door(provider).is_some_and(|d| d.supports_code)
}

/// `asLoginProvider`: solo los tres proveedores del contrato.
pub fn provider(value: &str) -> Result<&'static str, String> {
    PROVIDERS.into_iter().find(|p| *p == value).ok_or_else(|| "Unknown login provider".to_string())
}

/// `ProviderLoginStatus` del contrato.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LoginStatus {
    /// `idle`, `starting`, `awaiting_code`, `done` o `error`.
    pub phase: &'static str,
    /// `None` mientras corre; en `done` sin verificar (Antigravity) también.
    pub ok: Option<bool>,
    pub timed_out: bool,
    /// La cola de la salida del CLI.
    pub output: String,
    pub age_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl LoginStatus {
    fn idle() -> LoginStatus {
        LoginStatus { phase: "idle", ..LoginStatus::default() }
    }

    fn refused(error: &str) -> LoginStatus {
        LoginStatus { phase: "error", ok: Some(false), started: Some(false), error: Some(error.into()), ..LoginStatus::default() }
    }

    pub fn active(&self) -> bool {
        matches!(self.phase, "starting" | "awaiting_code")
    }
}

/// Cómo verificar contra el motor: su puerto y el token del header.
#[derive(Clone)]
pub struct EngineAccess {
    pub port: u16,
    pub token: String,
}

impl EngineAccess {
    /// El motor que supervisa la app, si está corriendo.
    pub fn current() -> Option<EngineAccess> {
        let engine = crate::ENGINE.lock().unwrap();
        let engine = engine.as_ref()?;
        let port = engine.origin().rsplit(':').next()?.trim_end_matches('/').parse().ok()?;
        Some(EngineAccess { port, token: engine.token().expose().to_string() })
    }

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

    fn snapshot(&self) -> LoginStatus {
        let state = self.state.lock().unwrap();
        let phase = if state.done { "done" } else if state.awaiting_code { "awaiting_code" } else { "starting" };
        LoginStatus {
            phase,
            ok: state.ok,
            timed_out: state.timed_out,
            output: state.output.clone(),
            age_ms: self.started.elapsed().as_millis() as u64,
            ..LoginStatus::default()
        }
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
        #[cfg(unix)]
        {
            // El hijo es líder de su propio grupo (`process_group(0)`): se mata el grupo entero.
            // por ruta absoluta: el `PATH` del proceso no decide qué programa mata
            let kill = if std::path::Path::new("/bin/kill").exists() { "/bin/kill" } else { "/usr/bin/kill" };
            let _ = Command::new(kill).args(["-KILL", "--", &format!("-{}", child.id())]).status();
        }
        let _ = child.kill();
    }

    /// Cancelar es instantáneo: el lugar queda libre aunque Windows tarde en
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
fn hide_window(command: &mut Command) {
    use std::os::unix::process::CommandExt;
    command.process_group(0);
}

/// Los logins en curso, uno por proveedor, compartidos por todas las ventanas.
#[derive(Default)]
pub struct Logins {
    sessions: Mutex<HashMap<&'static str, Arc<Session>>>,
}

pub fn logins() -> &'static Logins {
    static LOGINS: OnceLock<Logins> = OnceLock::new();
    LOGINS.get_or_init(Logins::default)
}

/// Opciones de `startProviderLogin`: solo dos campos de texto pasan.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LoginOptions {
    pub profile_dir: Option<String>,
    pub account_id: Option<String>,
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
    pub fn start(&self, provider: &'static str, options: LoginOptions, engine: Option<EngineAccess>) -> LoginStatus {
        let mut sessions = self.sessions.lock().unwrap();
        if let Some(existing) = sessions.get(provider) {
            if !existing.is_done() {
                return LoginStatus { started: Some(false), ..existing.snapshot() };
            }
        }
        let Some(door) = door(provider) else { return LoginStatus::refused("Unknown login provider") };
        let profile_dir = match options.profile_dir.as_deref().map(profile_writable) {
            Some(Err(())) => {
                return LoginStatus::refused(
                    "The selected account profile is not writable. Its folder permissions must allow your Windows user before signing in.",
                )
            }
            Some(Ok(dir)) => Some(dir),
            None => None,
        };
        let account_id = options.account_id.filter(|id| valid_account(id));
        let Some(exe) = harnesses::detect().into_iter().find(|h| h.id == provider).and_then(|h| h.path) else {
            return LoginStatus::refused("not-installed");
        };
        if provider == "antigravity" {
            return match launch_terminal(&exe) {
                // `ok: None`: no se verifica nada, solo se abrió la terminal.
                Ok(()) => LoginStatus { phase: "done", started: Some(true), ..LoginStatus::default() },
                Err(error) => LoginStatus::refused(&error),
            };
        }
        let Some(engine) = engine else { return LoginStatus::refused("the engine is not running") };
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
            Err(error) => return LoginStatus::refused(&format!("failed to start: {error}")),
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
        LoginStatus { started: Some(true), ..session.snapshot() }
    }

    pub fn status(&self, provider: &'static str) -> LoginStatus {
        match self.sessions.lock().unwrap().get(provider) {
            Some(session) => session.snapshot(),
            None => LoginStatus::idle(),
        }
    }

    /// `submitProviderLoginCode`: el código va solo al stdin del hijo.
    pub fn submit_code(&self, provider: &'static str, code: &str) -> Result<LoginStatus, String> {
        if !supports_code(provider) {
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
        Ok(session.snapshot())
    }

    /// `cancelProviderLogin`: mata el árbol y libera el lugar en el acto.
    pub fn cancel(&self, provider: &'static str) -> LoginStatus {
        if let Some(session) = self.sessions.lock().unwrap().remove(provider) {
            if !session.is_done() {
                session.cancel(false);
            }
        }
        LoginStatus::idle()
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
        assert!(supports_code("claude") && !supports_code("codex") && !supports_code("antigravity"));
    }

    #[test]
    fn cuentas_sin_rutas() {
        assert!(valid_account("acct_1-a"));
        assert!(!valid_account("../x"));
        assert!(!valid_account(""));
    }

    /// Un CLI falso en el `PATH` (Linux): corre con los argumentos fijos, no
    /// recibe las variables quitadas y cancelar mata el árbol (el nieto
    /// `sleep` no llega a escribir su marca).
    #[cfg(unix)]
    #[test]
    fn el_login_corre_el_cli_detectado_y_cancelar_mata_el_arbol() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("orgtree-dioxus-login-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let fake = dir.join("codex");
        let done = dir.join("finished.txt");
        std::fs::write(
            &fake,
            format!(
                "#!/bin/sh\necho fake-codex \"$@\"\n[ -n \"$ORGTREE_V2_TOKEN\" ] && echo token-leak\n(sleep 3; echo done > '{}') &\nwait\n",
                done.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        // SAFETY: el test es el único que toca el entorno en este proceso de pruebas.
        unsafe {
            let mut path = std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()).collect::<Vec<_>>();
            path.insert(0, dir.clone());
            std::env::set_var("PATH", std::env::join_paths(path).unwrap());
            std::env::set_var("ORGTREE_V2_TOKEN", "secreto");
        }
        let logins = Logins::default();
        let engine = EngineAccess { port: 1, token: "x".into() };
        // un harness no detectado se niega
        if let Some(missing) = harnesses::detect().into_iter().find(|h| !h.detected()) {
            assert_eq!(logins.start(missing.id, LoginOptions::default(), Some(engine.clone())).error.as_deref(), Some("not-installed"));
        }
        let started = logins.start("codex", LoginOptions::default(), Some(engine.clone()));
        assert_eq!((started.phase, started.started), ("starting", Some(true)));
        assert_eq!(logins.start("codex", LoginOptions::default(), Some(engine)).started, Some(false));
        let deadline = Instant::now() + Duration::from_secs(5);
        while !logins.status("codex").output.contains("fake-codex login") && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(50));
        }
        let running = logins.status("codex");
        assert!(running.output.contains("fake-codex login"), "{running:?}");
        assert!(!running.output.contains("token-leak"));
        assert!(logins.submit_code("codex", "123").is_err());
        assert_eq!(logins.cancel("codex").phase, "idle");
        std::thread::sleep(Duration::from_secs(4));
        assert!(!done.exists(), "cancelar no mató al nieto");
        std::fs::remove_dir_all(dir).unwrap();
    }
}
