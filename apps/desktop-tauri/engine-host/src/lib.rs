//! Supervisión del motor Python de Orgtree desde Rust.
//!
//! Reimplementa lo esencial de `apps/desktop/main/engine.ts` (arranque
//! administrado) según `docs/engine-contract.md`, sección "Managed startup":
//!
//! - lanza `engine/launch.py` con un Python absoluto y un `ORGTREE_DATA`
//!   explícito, y le pasa un token nuevo por arranque en `ORGTREE_V2_TOKEN`;
//! - lee stdout línea por línea: los checkpoints `startup-progress` reinician
//!   el plazo de silencio, `refused` aborta, y `ready` se verifica contra el
//!   PID del hijo, la raíz de datos, el puerto y el protocolo;
//! - al salir pide `POST /api/desktop/shutdown` con el token y, si el motor no
//!   termina, mata el árbol de procesos.
//!
//! El token nunca se escribe en logs: `Token` no expone su valor en `Debug` y
//! stderr del motor se descarta, como en Electron.
//!
//! No incluye la conexión a un motor del arranque del sistema (attach) ni el
//! vigilante de cuelgues: quedan fuera del recorte del spike.

use std::fmt;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Ipv4Addr, SocketAddrV4, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdout, Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

/// Header con el que el motor autentica al escritorio (`TokenGate`).
pub const TOKEN_HEADER: &str = "X-Orgtree-Desktop-Token";

/// Silencio máximo entre checkpoints de arranque (contrato: 60 s).
pub const STARTUP_SILENCE: Duration = Duration::from_secs(60);
/// Silencio máximo durante una fase de conversión de datos (`database-convert…`).
pub const CONVERSION_SILENCE: Duration = Duration::from_secs(900);
/// Tope de stdout antes de `ready`, igual que Electron.
const READY_BUFFER_LIMIT: usize = 65536;
const CONVERSION_PHASE: &str = "database-convert";

/// Presupuesto de apagado, con los mismos números que `QUIT_DEADLINES` en Electron.
const SHUTDOWN_REQUEST: Duration = Duration::from_secs(3);
const SHUTDOWN_EXIT_WAIT: Duration = Duration::from_secs(5);
const KILL_EXIT_WAIT: Duration = Duration::from_secs(5);

/// Credencial del escritorio para un arranque. Solo vive en este proceso y en
/// el entorno del motor, que la borra antes de lanzar hijos.
#[derive(Clone, PartialEq, Eq)]
pub struct Token(String);

impl Token {
    /// 32 bytes aleatorios del sistema, en hexadecimal.
    pub fn generate() -> Result<Token, EngineError> {
        let mut bytes = [0u8; 32];
        getrandom::fill(&mut bytes).map_err(|e| EngineError::Spawn(format!("no hay aleatoriedad del sistema: {e}")))?;
        Ok(Token(bytes.iter().map(|b| format!("{b:02x}")).collect()))
    }

    /// El valor, para ponerlo en un header o una cookie. Nunca loguearlo.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Token {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Token(<oculto>)")
    }
}

#[derive(Debug, Clone)]
pub struct EngineOptions {
    /// Intérprete de Python, ruta absoluta.
    pub python: PathBuf,
    /// Directorio que contiene `launch.py` (el `engine/` del repo o del paquete).
    pub engine_dir: PathBuf,
    /// Raíz de datos explícita. Se crea si no existe.
    pub data_root: PathBuf,
    /// Build del renderer que sirve el motor (`ORGTREE_V2_UI_DIR`), si hay.
    pub ui_dir: Option<PathBuf>,
    /// Plazo de silencio entre checkpoints; `STARTUP_SILENCE` por defecto.
    pub silence_timeout: Duration,
}

impl EngineOptions {
    pub fn new(python: impl Into<PathBuf>, engine_dir: impl Into<PathBuf>, data_root: impl Into<PathBuf>) -> Self {
        EngineOptions {
            python: python.into(),
            engine_dir: engine_dir.into(),
            data_root: data_root.into(),
            ui_dir: None,
            silence_timeout: STARTUP_SILENCE,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EngineError {
    /// Configuración inválida: Python o `launch.py` ausentes, raíz inválida.
    Config(String),
    /// No se pudo crear el proceso.
    Spawn(String),
    /// Otro motor ya es dueño de la raíz (`refused` / `root-owned`).
    RootOwned(String),
    /// Falló la conversión de datos de la primera ejecución.
    ConversionFailed(String),
    /// La línea `ready` no coincide con el hijo, la raíz o el protocolo.
    InvalidReady(String),
    /// El motor terminó antes de `ready`.
    ExitedEarly,
    /// Pasó el plazo de silencio sin checkpoint ni `ready`.
    Timeout,
    /// stdout superó el tope antes de `ready`.
    Oversized,
}

impl fmt::Display for EngineError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EngineError::Config(m) => write!(f, "configuración del motor inválida: {m}"),
            EngineError::Spawn(m) => write!(f, "no se pudo lanzar el motor: {m}"),
            EngineError::RootOwned(m) => write!(f, "otro motor es dueño de la raíz de datos: {m}"),
            EngineError::ConversionFailed(m) => write!(f, "falló la conversión de datos: {m}"),
            EngineError::InvalidReady(m) => write!(f, "línea ready inválida: {m}"),
            EngineError::ExitedEarly => f.write_str("el motor terminó antes de estar listo"),
            EngineError::Timeout => f.write_str("el motor no estuvo listo a tiempo"),
            EngineError::Oversized => f.write_str("la salida de arranque del motor superó el tope"),
        }
    }
}

impl std::error::Error for EngineError {}

/// Cómo terminó `Engine::stop`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopOutcome {
    /// Respondió al pedido de apagado y salió solo.
    Graceful,
    /// Hubo que matar el árbol de procesos.
    Killed,
    /// Ni el apagado ni la terminación se pudieron confirmar.
    Unconfirmed,
}

/// Lo que dice una línea de stdout durante el arranque.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Line {
    Ready { port: u16 },
    Progress { sequence: u64, conversion: bool },
    Refused(String),
    ConversionFailed(String),
    Ignored,
}

/// Raíz canónica para comparar con `dataRootId`. En Windows quita el prefijo
/// `\\?\` de `canonicalize` y compara sin mayúsculas, como `canonicalPath` en
/// Electron.
fn canonical_text(path: &Path) -> String {
    let text = path.to_string_lossy();
    let text = text.strip_prefix(r"\\?\").unwrap_or(&text);
    let text = text.trim_end_matches(['/', '\\']);
    if cfg!(windows) {
        text.to_lowercase()
    } else {
        text.to_string()
    }
}

fn same_root(reported: &str, expected: &Path) -> bool {
    let reported = Path::new(reported);
    reported.is_absolute() && canonical_text(reported) == canonical_text(expected)
}

fn classify(line: &str, expected_root: &Path, pid: u32, previous: u64) -> Result<Line, EngineError> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
        return Ok(Line::Ignored);
    };
    let Some(object) = value.as_object() else {
        return Ok(Line::Ignored);
    };
    let text = |key: &str| object.get(key).and_then(|v| v.as_str());
    match text("type") {
        Some("refused") => {
            let reason = text("reason");
            Ok(match (text("code"), reason) {
                (Some("root-owned"), Some(r)) => Line::Refused(r.chars().take(300).collect()),
                (Some("conversion-failed"), Some(r)) => Line::ConversionFailed(r.chars().take(2000).collect()),
                _ => Line::Ignored,
            })
        }
        Some("startup-progress") => {
            let sequence = object.get("sequence").and_then(|v| v.as_u64());
            let phase = text("phase").filter(|p| !p.is_empty() && p.len() <= 100);
            let valid = object.get("protocol").and_then(|v| v.as_u64()) == Some(1)
                && object.get("pid").and_then(|v| v.as_u64()) == Some(pid as u64)
                && text("dataRootId").is_some_and(|r| same_root(r, expected_root));
            match (valid, sequence, phase) {
                (true, Some(sequence), Some(phase)) if sequence > previous => Ok(Line::Progress {
                    sequence,
                    conversion: phase.starts_with(CONVERSION_PHASE),
                }),
                _ => Ok(Line::Ignored),
            }
        }
        Some("ready") => {
            if object.get("protocol").and_then(|v| v.as_u64()) != Some(1) {
                return Err(EngineError::InvalidReady("protocolo distinto de 1".into()));
            }
            if object.get("pid").and_then(|v| v.as_u64()) != Some(pid as u64) {
                return Err(EngineError::InvalidReady("el PID no es el del proceso lanzado".into()));
            }
            let port = object
                .get("port")
                .and_then(|v| v.as_u64())
                .filter(|p| (1..=65535).contains(p))
                .ok_or_else(|| EngineError::InvalidReady("puerto inválido".into()))?;
            match text("dataRootId") {
                Some(root) if same_root(root, expected_root) => Ok(Line::Ready { port: port as u16 }),
                _ => Err(EngineError::InvalidReady("la raíz de datos no coincide".into())),
            }
        }
        _ => Ok(Line::Ignored),
    }
}

enum Event {
    Line(String),
    Oversized,
    Closed,
}

/// Lee stdout en un hilo. Antes de `ready` manda líneas acotadas al canal;
/// después sigue drenando para que el pipe no bloquee al motor.
fn read_stdout(stdout: ChildStdout, sender: mpsc::Sender<Event>) {
    let mut reader = BufReader::new(stdout);
    let mut total = 0usize;
    let mut line = Vec::new();
    loop {
        line.clear();
        // `take` acota también una sola línea sin salto.
        let budget = (READY_BUFFER_LIMIT.saturating_sub(total) + 1) as u64;
        match (&mut reader).take(budget).read_until(b'\n', &mut line) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                total += n;
                if total > READY_BUFFER_LIMIT {
                    let _ = sender.send(Event::Oversized);
                    break;
                }
                let text = String::from_utf8_lossy(&line).trim().to_string();
                if sender.send(Event::Line(text)).is_err() {
                    break; // el arranque terminó: solo queda drenar
                }
            }
        }
    }
    let _ = sender.send(Event::Closed);
    let mut sink = [0u8; 8192];
    while matches!(reader.read(&mut sink), Ok(n) if n > 0) {}
}

/// Un motor lanzado y listo. Al soltarlo sin `stop`, se mata su árbol.
pub struct Engine {
    child: Option<Child>,
    token: Token,
    port: u16,
    data_root: PathBuf,
}

impl fmt::Debug for Engine {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Engine")
            .field("pid", &self.pid())
            .field("port", &self.port)
            .field("data_root", &self.data_root)
            .field("token", &self.token)
            .finish()
    }
}

impl Engine {
    /// Lanza el motor y espera `ready`. Bloquea: llamarlo fuera del hilo de UI.
    pub fn start(options: &EngineOptions) -> Result<Engine, EngineError> {
        Self::start_with(options, &mut |_| {})
    }

    /// Como `start`, avisando cada fase (`starting`, `progress`, `converting`).
    pub fn start_with(options: &EngineOptions, notify: &mut dyn FnMut(&str)) -> Result<Engine, EngineError> {
        if !options.python.is_absolute() || !options.python.is_file() {
            return Err(EngineError::Config(format!("no hay un Python absoluto en {}", options.python.display())));
        }
        let launch = options.engine_dir.join("launch.py");
        if !launch.is_file() {
            return Err(EngineError::Config(format!("falta {}", launch.display())));
        }
        if !options.data_root.is_absolute() {
            return Err(EngineError::Config("la raíz de datos debe ser absoluta".into()));
        }
        std::fs::create_dir_all(&options.data_root)
            .map_err(|e| EngineError::Config(format!("no se pudo crear la raíz de datos: {e}")))?;
        let data_root = std::fs::canonicalize(&options.data_root)
            .map_err(|e| EngineError::Config(format!("raíz de datos ilegible: {e}")))?;
        let data_root = PathBuf::from(canonical_display(&data_root));
        let token = Token::generate()?;

        let mut command = Command::new(&options.python);
        command
            .arg(&launch)
            .current_dir(&options.engine_dir)
            // Nunca heredar un puerto v1 ni el bootstrap de PostgreSQL del instalado.
            .env_remove("ORGTREE_PORT")
            .env_remove("ORGTREE_PG_BOOTSTRAP")
            .env("ORGTREE_DATA", &data_root)
            .env("ORGTREE_V2_TOKEN", token.expose())
            .env("ORGTREE_V2_PARENT_PID", std::process::id().to_string())
            .env("PYTHONUNBUFFERED", "1")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            // El motor escribe sus diagnósticos a disco; stderr puede traer secretos.
            .stderr(Stdio::null());
        if let Some(ui) = &options.ui_dir {
            command.env("ORGTREE_V2_UI_DIR", ui);
        }
        hide_console(&mut command);
        notify("starting");
        let mut child = command.spawn().map_err(|e| EngineError::Spawn(e.to_string()))?;
        let pid = child.id();
        let stdout = child.stdout.take().expect("stdout configurado como pipe");
        let (sender, receiver) = mpsc::channel();
        thread::Builder::new()
            .name("orgtree-engine-stdout".into())
            .spawn(move || read_stdout(stdout, sender))
            .map_err(|e| EngineError::Spawn(e.to_string()))?;

        let mut engine = Engine { child: Some(child), token, port: 0, data_root };
        let mut sequence = 0u64;
        let mut window = options.silence_timeout;
        let mut deadline = Instant::now() + window;
        let result = loop {
            let wait = deadline.saturating_duration_since(Instant::now());
            match receiver.recv_timeout(wait) {
                Err(mpsc::RecvTimeoutError::Timeout) => break Err(EngineError::Timeout),
                Err(mpsc::RecvTimeoutError::Disconnected) | Ok(Event::Closed) => break Err(EngineError::ExitedEarly),
                Ok(Event::Oversized) => break Err(EngineError::Oversized),
                Ok(Event::Line(line)) => match classify(&line, &engine.data_root, pid, sequence) {
                    Err(error) => break Err(error),
                    Ok(Line::Ready { port }) => {
                        engine.port = port;
                        break Ok(());
                    }
                    Ok(Line::Refused(reason)) => break Err(EngineError::RootOwned(reason)),
                    Ok(Line::ConversionFailed(reason)) => break Err(EngineError::ConversionFailed(reason)),
                    Ok(Line::Progress { sequence: next, conversion }) => {
                        sequence = next;
                        window = if conversion { CONVERSION_SILENCE.max(options.silence_timeout) } else { options.silence_timeout };
                        notify(if conversion { "converting" } else { "progress" });
                        deadline = Instant::now() + window;
                    }
                    Ok(Line::Ignored) => {}
                },
            }
        };
        // Soltar el receptor: el hilo lector pasa a solo drenar.
        drop(receiver);
        match result {
            Ok(()) => Ok(engine),
            Err(error) => {
                engine.kill_tree_and_wait(KILL_EXIT_WAIT);
                engine.child = None;
                Err(error)
            }
        }
    }

    pub fn pid(&self) -> Option<u32> {
        self.child.as_ref().map(|c| c.id())
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    /// Origen exacto del motor, por ejemplo `http://127.0.0.1:24680`.
    pub fn origin(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    /// Raíz de datos canónica que el motor confirmó en `ready`.
    pub fn data_root(&self) -> &Path {
        &self.data_root
    }

    /// Credencial de este arranque. Solo para headers o la cookie del webview.
    pub fn token(&self) -> &Token {
        &self.token
    }

    /// ¿Sigue vivo el proceso del motor?
    pub fn is_running(&mut self) -> bool {
        match self.child.as_mut() {
            Some(child) => matches!(child.try_wait(), Ok(None)),
            None => false,
        }
    }

    /// GET autenticado contra el motor; devuelve el status HTTP y el cuerpo.
    pub fn get(&self, path: &str, timeout: Duration) -> std::io::Result<(u16, String)> {
        http_request(self.port, "GET", path, Some(&self.token), timeout)
    }

    /// Mata el árbol del motor sin pedir apagado, como una caída. Para probar la
    /// recuperación del shell; el apagado normal es `stop`.
    pub fn kill(&mut self) -> bool {
        let killed = self.kill_tree_and_wait(KILL_EXIT_WAIT);
        self.child = None;
        killed
    }

    /// Apagado limpio con el token; si el motor no sale, mata el árbol.
    pub fn stop(mut self) -> StopOutcome {
        self.stop_inner()
    }

    fn stop_inner(&mut self) -> StopOutcome {
        if self.child.is_none() {
            return StopOutcome::Graceful;
        }
        if !self.is_running() {
            self.child = None;
            return StopOutcome::Graceful;
        }
        let _ = http_request(self.port, "POST", "/api/desktop/shutdown", Some(&self.token), SHUTDOWN_REQUEST);
        if self.wait_exit(SHUTDOWN_EXIT_WAIT) {
            self.child = None;
            return StopOutcome::Graceful;
        }
        let killed = self.kill_tree_and_wait(KILL_EXIT_WAIT);
        self.child = None;
        if killed {
            StopOutcome::Killed
        } else {
            StopOutcome::Unconfirmed
        }
    }

    fn wait_exit(&mut self, budget: Duration) -> bool {
        let Some(child) = self.child.as_mut() else { return true };
        let deadline = Instant::now() + budget;
        loop {
            match child.try_wait() {
                Ok(Some(_)) => return true,
                Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(25)),
                _ => return false,
            }
        }
    }

    fn kill_tree_and_wait(&mut self, budget: Duration) -> bool {
        let Some(child) = self.child.as_mut() else { return true };
        if matches!(child.try_wait(), Ok(Some(_))) {
            return true;
        }
        kill_tree(child);
        self.wait_exit(budget)
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        if self.child.is_some() {
            self.kill_tree_and_wait(KILL_EXIT_WAIT);
        }
    }
}

/// Texto de ruta sin el prefijo `\\?\` de `canonicalize`, que el motor no usa.
fn canonical_display(path: &Path) -> String {
    let text = path.to_string_lossy();
    text.strip_prefix(r"\\?\").unwrap_or(&text).to_string()
}

#[cfg(windows)]
fn hide_console(command: &mut Command) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    command.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(windows))]
fn hide_console(_command: &mut Command) {}

/// Termina el árbol entero: el guardián del motor (Job de Windows) se lleva a
/// PostgreSQL y a los CLIs de agentes con él.
#[cfg(windows)]
fn kill_tree(child: &mut Child) {
    let mut taskkill = Command::new("taskkill");
    taskkill
        .args(["/PID", &child.id().to_string(), "/T", "/F"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    hide_console(&mut taskkill);
    if !matches!(taskkill.status(), Ok(s) if s.success()) {
        let _ = child.kill();
    }
}

#[cfg(not(windows))]
fn kill_tree(child: &mut Child) {
    let _ = child.kill();
}

/// HTTP/1.1 mínimo contra 127.0.0.1, sin dependencias: alcanza para el
/// apagado y las pruebas. El cliente completo de la UI vive en el webview.
pub fn http_request(port: u16, method: &str, path: &str, token: Option<&Token>, timeout: Duration) -> std::io::Result<(u16, String)> {
    let address = SocketAddrV4::new(Ipv4Addr::LOCALHOST, port);
    let mut stream = TcpStream::connect_timeout(&address.into(), timeout)?;
    stream.set_read_timeout(Some(timeout))?;
    stream.set_write_timeout(Some(timeout))?;
    let mut request = format!("{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\nContent-Length: 0\r\n");
    if let Some(token) = token {
        request.push_str(&format!("{TOKEN_HEADER}: {}\r\n", token.expose()));
    }
    request.push_str("\r\n");
    stream.write_all(request.as_bytes())?;
    let mut response = Vec::new();
    stream.take(1 << 20).read_to_end(&mut response)?;
    let text = String::from_utf8_lossy(&response);
    let status = text
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse::<u16>().ok())
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidData, "respuesta HTTP inválida"))?;
    let body = text.split_once("\r\n\r\n").map(|(_, b)| b.to_string()).unwrap_or_default();
    Ok((status, body))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root() -> PathBuf {
        if cfg!(windows) {
            PathBuf::from(r"C:\Datos\Orgtree")
        } else {
            PathBuf::from("/datos/orgtree")
        }
    }

    fn root_json() -> String {
        serde_json::to_string(&root().to_string_lossy()).unwrap()
    }

    #[test]
    fn ready_valido() {
        let line = format!(r#"{{"type":"ready","protocol":1,"port":24680,"pid":42,"dataRootId":{}}}"#, root_json());
        assert_eq!(classify(&line, &root(), 42, 0), Ok(Line::Ready { port: 24680 }));
    }

    #[test]
    fn ready_con_otro_pid_se_rechaza() {
        let line = format!(r#"{{"type":"ready","protocol":1,"port":24680,"pid":43,"dataRootId":{}}}"#, root_json());
        assert!(matches!(classify(&line, &root(), 42, 0), Err(EngineError::InvalidReady(_))));
    }

    #[test]
    fn ready_con_otra_raiz_o_protocolo_o_puerto_se_rechaza() {
        let other = if cfg!(windows) { r#""C:\\Otra""# } else { r#""/otra""# };
        for line in [
            format!(r#"{{"type":"ready","protocol":1,"port":24680,"pid":42,"dataRootId":{other}}}"#),
            format!(r#"{{"type":"ready","protocol":2,"port":24680,"pid":42,"dataRootId":{}}}"#, root_json()),
            format!(r#"{{"type":"ready","protocol":1,"port":0,"pid":42,"dataRootId":{}}}"#, root_json()),
            format!(r#"{{"type":"ready","protocol":1,"port":70000,"pid":42,"dataRootId":{}}}"#, root_json()),
            r#"{"type":"ready","protocol":1,"port":24680,"pid":42,"dataRootId":"relativa"}"#.to_string(),
        ] {
            assert!(matches!(classify(&line, &root(), 42, 0), Err(EngineError::InvalidReady(_))), "{line}");
        }
    }

    #[test]
    fn progreso_avanza_solo_hacia_adelante_y_detecta_conversion() {
        let line = |seq: u64, phase: &str| {
            format!(r#"{{"type":"startup-progress","protocol":1,"pid":42,"sequence":{seq},"phase":"{phase}","dataRootId":{}}}"#, root_json())
        };
        assert_eq!(classify(&line(1, "api-loaded"), &root(), 42, 0), Ok(Line::Progress { sequence: 1, conversion: false }));
        assert_eq!(classify(&line(1, "api-loaded"), &root(), 42, 1), Ok(Line::Ignored));
        assert_eq!(
            classify(&line(2, "database-convert:org-1"), &root(), 42, 1),
            Ok(Line::Progress { sequence: 2, conversion: true })
        );
        assert_eq!(classify(&line(3, "api-loaded"), &root(), 7, 0), Ok(Line::Ignored));
    }

    #[test]
    fn rechazos_estructurados() {
        assert_eq!(
            classify(r#"{"type":"refused","code":"root-owned","reason":"otro"}"#, &root(), 42, 0),
            Ok(Line::Refused("otro".into()))
        );
        assert_eq!(
            classify(r#"{"type":"refused","code":"conversion-failed","reason":"mal"}"#, &root(), 42, 0),
            Ok(Line::ConversionFailed("mal".into()))
        );
        assert_eq!(classify(r#"{"type":"refused","code":"otra"}"#, &root(), 42, 0), Ok(Line::Ignored));
    }

    #[test]
    fn texto_suelto_se_ignora() {
        assert_eq!(classify("Uvicorn running", &root(), 42, 0), Ok(Line::Ignored));
        assert_eq!(classify("[1,2]", &root(), 42, 0), Ok(Line::Ignored));
    }

    #[test]
    fn el_token_no_aparece_en_debug() {
        let token = Token::generate().unwrap();
        assert_eq!(token.expose().len(), 64);
        assert!(!format!("{token:?}").contains(token.expose()));
        assert_ne!(token, Token::generate().unwrap());
    }

    #[cfg(windows)]
    #[test]
    fn raiz_sin_prefijo_largo_y_sin_mayusculas() {
        assert!(same_root(r"C:\Datos\Orgtree", Path::new(r"\\?\c:\datos\orgtree\")));
    }
}
