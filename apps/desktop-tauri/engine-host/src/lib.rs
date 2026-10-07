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
//! En modo empaquetado (`packaged`), además pasa las rutas de PostgreSQL y
//! `ORGTREE_PG_BOOTSTRAP=1`, como `apps/desktop/main/postgres-runtime.ts`.
//!
//! Ciclo de vida (#19): las fases de arranque y de conversión llegan al shell
//! (`StartupEvent`), la salida tiene el presupuesto de `QUIT_DEADLINES` de
//! Electron (`stop_for_quit`) y prueba que el árbol soltó la raíz (el candado
//! del guardián), y `LivenessWatch` decide cuándo un motor colgado se reinicia,
//! con las reglas de `LIVENESS`.
//!
//! No incluye la conexión a un motor del arranque del sistema (attach): la app
//! del spike no instala esa tarea programada.

pub mod packaged;

use std::ffi::OsString;
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

/// Variables de libpq que no se heredan: el motor se conecta a su propio
/// PostgreSQL con un `passfile`, y libpq prefiere `PGPASSWORD` (y el resto)
/// a lo que no figura en la cadena de conexión. Un entorno con un PostgreSQL
/// ajeno (como los runners de GitHub, con `PGPASSWORD=root`) hacía fallar
/// la autenticación SCRAM del motor empaquetado.
pub const LIBPQ_ENV: [&str; 14] = [
    "PGPASSWORD", "PGPASSFILE", "PGUSER", "PGHOST", "PGHOSTADDR", "PGPORT", "PGDATABASE", "PGSERVICE",
    "PGSERVICEFILE", "PGOPTIONS", "PGSSLMODE", "PGREQUIREAUTH", "PGCHANNELBINDING", "PGTARGETSESSIONATTRS",
];

/// Presupuesto de apagado, con los mismos números que `QUIT_DEADLINES` en Electron.
const SHUTDOWN_REQUEST: Duration = Duration::from_secs(3);
const SHUTDOWN_EXIT_WAIT: Duration = Duration::from_secs(5);
const KILL_EXIT_WAIT: Duration = Duration::from_secs(5);

/// `QUIT_DEADLINES` de `apps/desktop/main/engine.ts`: cada fase que puede
/// gastar una salida. `stop_for_quit` reparte un solo presupuesto entre ellas.
pub mod quit_deadlines {
    use std::time::Duration;
    /// `stop()` de un hijo administrado: 3 s de pedido de apagado y 5 s de espera.
    pub const MANAGED_STOP: Duration = Duration::from_secs(8);
    /// El pedido autenticado a un motor enganchado (attach; no aplica acá).
    pub const REQUEST: Duration = Duration::from_secs(4);
    /// Esperar la prueba de salida después de pedir por las buenas.
    pub const RELEASE: Duration = Duration::from_secs(8);
    /// `taskkill /T /F` sobre el árbol: el techo, no lo esperable.
    pub const KILL: Duration = Duration::from_secs(5);
    /// La prueba después de matar: la salida y el candado del guardián.
    pub const PROVEN: Duration = Duration::from_secs(5);
}

/// `QUIT_STOP_BUDGET_MS`: el peor caso de una salida (26 s).
pub const QUIT_STOP_BUDGET: Duration = Duration::from_secs(
    if quit_deadlines::MANAGED_STOP.as_secs() > quit_deadlines::REQUEST.as_secs() {
        quit_deadlines::MANAGED_STOP.as_secs()
    } else {
        quit_deadlines::REQUEST.as_secs()
    } + quit_deadlines::RELEASE.as_secs()
        + quit_deadlines::KILL.as_secs()
        + quit_deadlines::PROVEN.as_secs(),
);

/// `INSTALLER_UPGRADE_STOP_BUDGET_MS` de Electron. En Tauri el instalador NSIS
/// cierra la app con el Restart Manager (`WM_ENDSESSION`), que pasa por la
/// misma salida que el fin de sesión de Windows; queda como referencia.
pub const INSTALLER_UPGRADE_STOP_BUDGET: Duration = Duration::from_secs(45);

/// Archivo del candado del guardián (`engine/process_lifetime.py`, `RootLock`).
pub const ROOT_LOCK_FILE: &str = ".desktop-engine.lock";

/// Reglas del vigilante de cuelgues, número por número las de `LIVENESS` en
/// Electron (y las del host de arranque, `engine/service_host.py`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LivenessRules {
    pub interval: Duration,
    pub probe_timeout: Duration,
    pub deadline: Duration,
    pub min_failures: u32,
    pub restart_limit: usize,
    pub restart_window: Duration,
}

pub const LIVENESS: LivenessRules = LivenessRules {
    interval: Duration::from_secs(30),
    probe_timeout: Duration::from_secs(60),
    deadline: Duration::from_secs(300),
    min_failures: 3,
    restart_limit: 3,
    restart_window: Duration::from_secs(3600),
};

/// Colgado exige las dos cosas: un silencio largo desde la última respuesta
/// buena y varias sondas fallidas. Una respuesta lenta es contención, no cuelgue.
#[derive(Debug, Clone)]
pub struct LivenessWatch {
    rules: LivenessRules,
    last_ok: Instant,
    failures: u32,
    last_error: Option<String>,
}

impl LivenessWatch {
    pub fn new(rules: LivenessRules, now: Instant) -> Self {
        LivenessWatch { rules, last_ok: now, failures: 0, last_error: None }
    }

    /// Anota una sonda: `None` si respondió como sí mismo, si no el motivo.
    pub fn record(&mut self, error: Option<String>, now: Instant) {
        match error {
            None => {
                self.last_ok = now;
                self.failures = 0;
                self.last_error = None;
            }
            Some(error) => {
                self.failures += 1;
                self.last_error = Some(error);
            }
        }
    }

    pub fn silent(&self, now: Instant) -> Duration {
        now.saturating_duration_since(self.last_ok)
    }

    pub fn failures(&self) -> u32 {
        self.failures
    }

    pub fn last_error(&self) -> Option<&str> {
        self.last_error.as_deref()
    }

    pub fn hung(&self, now: Instant) -> bool {
        self.failures >= self.rules.min_failures && self.silent(now) >= self.rules.deadline
    }
}

/// Cuántos reinicios automáticos van en la ventana: los cuelgues (y las caídas
/// en el shell) se reinician a lo sumo `limit` veces por `window`.
#[derive(Debug, Clone)]
pub struct RestartBudget {
    limit: usize,
    window: Duration,
    at: Vec<Instant>,
}

impl RestartBudget {
    pub fn new(limit: usize, window: Duration) -> Self {
        RestartBudget { limit, window, at: Vec::new() }
    }

    /// Anota un reinicio en `now` y dice si todavía entra en el presupuesto.
    pub fn consider(&mut self, now: Instant) -> bool {
        let window = self.window;
        self.at.retain(|t| now.saturating_duration_since(*t) < window);
        self.at.push(now);
        self.at.len() <= self.limit
    }

    pub fn recent(&self) -> usize {
        self.at.len()
    }

    /// Un reinicio pedido por la persona (la bandeja) vuelve a empezar la cuenta.
    pub fn reset(&mut self) {
        self.at.clear();
    }
}

/// Lo que el arranque le cuenta al shell, en orden.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StartupEvent<'a> {
    /// Se lanzó el proceso.
    Starting,
    /// Un checkpoint de arranque (`startup-progress`), con su fase.
    Progress(&'a str),
    /// Un checkpoint de la conversión de datos (`database-convert…`).
    Converting(&'a str),
}

/// Cómo terminó `Engine::stop_for_quit`, como `stopForQuit` de Electron.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuitOutcome {
    /// Salió por las buenas y el árbol soltó la raíz.
    Stopped,
    /// Hubo que matar el árbol, y después soltó la raíz.
    Forced,
    /// No se pudo probar que el árbol terminó dentro del presupuesto.
    Unverified,
}

impl QuitOutcome {
    pub fn as_str(self) -> &'static str {
        match self {
            QuitOutcome::Stopped => "stopped",
            QuitOutcome::Forced => "forced",
            QuitOutcome::Unverified => "unverified",
        }
    }
}

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
    /// Plazo de silencio mientras la fase es de conversión (`database-convert…`);
    /// `CONVERSION_SILENCE` por defecto. Mide el tiempo entre checkpoints, no el total.
    pub conversion_timeout: Duration,
    /// `ORGTREE_PG_BOOTSTRAP=1`: una raíz nueva nace en PostgreSQL. Solo la app
    /// empaquetada lo pide; nunca se hereda del entorno.
    pub bootstrap_postgres: bool,
    /// Variables extra para el motor (rutas de PostgreSQL del paquete).
    pub env: Vec<(String, OsString)>,
    /// Raíces que el motor nunca debe servir (la de Orgtree instalado): la
    /// raíz de datos no puede estar dentro de ninguna ni contener a ninguna.
    pub forbidden_roots: Vec<PathBuf>,
}

impl EngineOptions {
    pub fn new(python: impl Into<PathBuf>, engine_dir: impl Into<PathBuf>, data_root: impl Into<PathBuf>) -> Self {
        EngineOptions {
            python: python.into(),
            engine_dir: engine_dir.into(),
            data_root: data_root.into(),
            ui_dir: None,
            silence_timeout: STARTUP_SILENCE,
            conversion_timeout: CONVERSION_SILENCE,
            bootstrap_postgres: false,
            env: Vec::new(),
            forbidden_roots: Vec::new(),
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
    Progress { sequence: u64, phase: String },
    Refused(String),
    ConversionFailed(String),
    Ignored,
}

/// ¿Es una fase de la conversión de datos de la primera ejecución?
pub fn is_conversion_phase(phase: &str) -> bool {
    phase.starts_with(CONVERSION_PHASE)
}

/// El texto que acompaña a una fase de conversión, sin el prefijo
/// (`database-convert: org 1/4` → `org 1/4`), como `conversionMessage`.
pub fn conversion_detail(phase: &str) -> &str {
    phase.strip_prefix(CONVERSION_PHASE).unwrap_or(phase).trim_start_matches([':', ' ', '-'])
}

/// ¿El árbol del motor soltó la raíz? El guardián tiene un candado exclusivo
/// sobre el byte 0 de `.desktop-engine.lock` hasta que termina el árbol entero
/// (PostgreSQL y los CLIs incluidos), así que poder escribir ese byte (el mismo
/// que ya tiene) lo prueba. Sin archivo, ningún árbol tomó la raíz. Como
/// `guardianReleased` y `rootReleased` en Electron.
pub fn root_released(root: &Path) -> bool {
    let lock = root.join(ROOT_LOCK_FILE);
    if !lock.exists() {
        return true;
    }
    match std::fs::OpenOptions::new().write(true).open(&lock) {
        Ok(mut file) => file.write_all(b"0").is_ok() && file.flush().is_ok(),
        Err(_) => false,
    }
}

/// Espera a que el árbol suelte la raíz, hasta `budget`.
pub fn wait_root_released(root: &Path, budget: Duration) -> bool {
    let deadline = Instant::now() + budget;
    loop {
        if root_released(root) {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        thread::sleep(Duration::from_millis(100));
    }
}

/// ¿`a` es `b` o está dentro de `b`? Por componentes, sin mayúsculas en Windows.
fn within(a: &Path, b: &Path) -> bool {
    let a = canonical_text(a);
    let b = canonical_text(b);
    let sep = if cfg!(windows) { '\\' } else { '/' };
    let a = if cfg!(windows) { a.replace('/', "\\") } else { a };
    let b = if cfg!(windows) { b.replace('/', "\\") } else { b };
    !b.is_empty() && (a == b || a.starts_with(&format!("{b}{sep}")))
}

/// La ruta canónica si existe; si no, la de su ancestro existente más cercano
/// con el resto agregado (una raíz nueva todavía no existe).
fn resolve_lexically(path: &Path) -> PathBuf {
    let mut rest = Vec::new();
    let mut current = path.to_path_buf();
    loop {
        if let Ok(real) = std::fs::canonicalize(&current) {
            let mut out = PathBuf::from(canonical_display(&real));
            for part in rest.iter().rev() {
                out.push(part);
            }
            return out;
        }
        match (current.file_name().map(|n| n.to_os_string()), current.parent()) {
            (Some(name), Some(parent)) => {
                rest.push(name);
                current = parent.to_path_buf();
            }
            _ => return path.to_path_buf(),
        }
    }
}

/// Rechaza una raíz de datos que se superpone con una raíz prohibida.
pub fn check_forbidden(root: &Path, forbidden: &[PathBuf]) -> Result<(), EngineError> {
    let root = resolve_lexically(root);
    for other in forbidden.iter().filter(|p| p.is_absolute()) {
        let other = resolve_lexically(other);
        if within(&root, &other) || within(&other, &root) {
            return Err(EngineError::Config(format!(
                "la raíz de datos {} se superpone con {}, que no es de esta app",
                root.display(),
                other.display()
            )));
        }
    }
    Ok(())
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
                    phase: phase.to_string(),
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

    /// Como `start`, avisando cada fase (`StartupEvent`): el shell las muestra
    /// en la ventana de arranque, como la ventana de conversión de Electron.
    pub fn start_with(options: &EngineOptions, notify: &mut dyn FnMut(StartupEvent<'_>)) -> Result<Engine, EngineError> {
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
        check_forbidden(&options.data_root, &options.forbidden_roots)?;
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
            .env_remove("ORGTREE_PG_CUSTODIAN")
            .env_remove("ORGTREE_P03_PG_BIN")
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
        for (key, value) in &options.env {
            command.env(key, value);
        }
        if options.bootstrap_postgres {
            command.env("ORGTREE_PG_BOOTSTRAP", "1");
        }
        for key in LIBPQ_ENV {
            command.env_remove(key);
        }
        hide_console(&mut command);
        notify(StartupEvent::Starting);
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
                    Ok(Line::Progress { sequence: next, phase }) => {
                        sequence = next;
                        // Un paso de la conversión (copiar o releer una org grande) puede
                        // durar más que el plazo común: mientras dura, rige el suyo.
                        let conversion = is_conversion_phase(&phase);
                        window = if conversion { options.conversion_timeout } else { options.silence_timeout };
                        notify(if conversion { StartupEvent::Converting(&phase) } else { StartupEvent::Progress(&phase) });
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

    /// LA SALIDA (`stopForQuit` de Electron): por las buenas, después probado y
    /// recién después forzado, con UN presupuesto repartido entre las fases de
    /// `quit_deadlines`. La parte amable siempre le deja a la forzada su parte
    /// (`KILL + PROVEN`, a lo sumo la mitad), para que un motor lento no se coma
    /// el presupuesto pidiendo y la terminación nunca se intente.
    ///
    /// Probado quiere decir el proceso terminado **y** la raíz liberada por el
    /// guardián, que la tiene hasta que muere todo el árbol (PostgreSQL y los
    /// CLIs de agentes). Así una salida nunca deja huérfanos sin decirlo.
    pub fn stop_for_quit(&mut self, budget: Duration) -> QuitOutcome {
        let until = Instant::now() + budget;
        let left = || until.saturating_duration_since(Instant::now());
        let reserve = (quit_deadlines::KILL + quit_deadlines::PROVEN).min(budget / 2);
        let graceful = |want: Duration| want.min(left().saturating_sub(reserve));
        let root = self.data_root.clone();
        if self.child.is_none() || !self.is_running() {
            self.child = None;
            return if wait_root_released(&root, quit_deadlines::PROVEN.min(left())) {
                QuitOutcome::Stopped
            } else {
                QuitOutcome::Unverified
            };
        }
        let request = graceful(SHUTDOWN_REQUEST);
        if !request.is_zero() {
            let _ = http_request(self.port, "POST", "/api/desktop/shutdown", Some(&self.token), request);
        }
        if self.wait_exit(graceful(SHUTDOWN_EXIT_WAIT + quit_deadlines::RELEASE)) {
            self.child = None;
            return if wait_root_released(&root, quit_deadlines::PROVEN.min(left())) {
                QuitOutcome::Stopped
            } else {
                QuitOutcome::Unverified
            };
        }
        if let Some(child) = self.child.as_mut() {
            kill_tree(child);
        }
        let exited = self.wait_exit(quit_deadlines::KILL.min(left()));
        if exited {
            self.child = None;
        }
        if exited && wait_root_released(&root, quit_deadlines::PROVEN.min(left())) {
            QuitOutcome::Forced
        } else {
            QuitOutcome::Unverified
        }
    }

    /// La sonda del vigilante de cuelgues (`probeAlive`): `None` si el motor
    /// respondió `/api/desktop/alive` como sí mismo (su PID y su raíz).
    pub fn probe_alive(&self, timeout: Duration) -> Option<String> {
        let pid = self.pid()?;
        self.handle(pid).probe_alive(timeout)
    }

    /// Lo necesario para hablar con el motor sin tener su `Engine` (que el shell
    /// guarda tras un mutex): una sonda lenta no bloquea la salida ni el estado.
    pub fn handle(&self, pid: u32) -> EngineHandle {
        EngineHandle { port: self.port, token: self.token.clone(), pid, data_root: self.data_root.clone() }
    }
}

/// Un motor listo, visto desde afuera de su `Engine`.
#[derive(Debug, Clone)]
pub struct EngineHandle {
    pub port: u16,
    pub token: Token,
    pub pid: u32,
    pub data_root: PathBuf,
}

impl EngineHandle {
    /// `probeAlive` de Electron: `None` si respondió como sí mismo.
    pub fn probe_alive(&self, timeout: Duration) -> Option<String> {
        let pid = self.pid;
        match http_request(self.port, "GET", "/api/desktop/alive", Some(&self.token), timeout) {
            Ok((200, body)) => {
                let value: serde_json::Value = serde_json::from_str(&body).unwrap_or_default();
                let same = value["pid"].as_u64() == Some(pid as u64)
                    && value["dataRootId"].as_str().is_some_and(|r| same_root(r, &self.data_root));
                if same {
                    None
                } else {
                    Some("el motor respondió con otra identidad".into())
                }
            }
            Ok((status, _)) => Some(format!("status {status}")),
            Err(error) => Some(error.to_string().chars().take(300).collect()),
        }
    }

    /// Pedido autenticado con cuerpo JSON opcional.
    pub fn request(&self, method: &str, path: &str, json: Option<&str>, timeout: Duration) -> std::io::Result<(u16, String)> {
        http_request_body(self.port, method, path, Some(&self.token), json, timeout)
    }
}

impl Engine {

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
pub(crate) fn canonical_display(path: &Path) -> String {
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
    http_request_body(port, method, path, token, None, timeout)
}

/// Como `http_request`, con un cuerpo JSON opcional (el acuse de mantenimiento).
pub fn http_request_body(
    port: u16,
    method: &str,
    path: &str,
    token: Option<&Token>,
    json: Option<&str>,
    timeout: Duration,
) -> std::io::Result<(u16, String)> {
    let address = SocketAddrV4::new(Ipv4Addr::LOCALHOST, port);
    let mut stream = TcpStream::connect_timeout(&address.into(), timeout)?;
    stream.set_read_timeout(Some(timeout))?;
    stream.set_write_timeout(Some(timeout))?;
    let body = json.unwrap_or("");
    let mut request = format!(
        "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\nContent-Length: {}\r\n",
        body.len()
    );
    if json.is_some() {
        request.push_str("Content-Type: application/json\r\n");
    }
    if let Some(token) = token {
        request.push_str(&format!("{TOKEN_HEADER}: {}\r\n", token.expose()));
    }
    request.push_str("\r\n");
    request.push_str(body);
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
        assert_eq!(classify(&line(1, "api-loaded"), &root(), 42, 0), Ok(Line::Progress { sequence: 1, phase: "api-loaded".into() }));
        assert_eq!(classify(&line(1, "api-loaded"), &root(), 42, 1), Ok(Line::Ignored));
        assert_eq!(
            classify(&line(2, "database-convert:org-1"), &root(), 42, 1),
            Ok(Line::Progress { sequence: 2, phase: "database-convert:org-1".into() })
        );
        assert!(is_conversion_phase("database-convert:org-1") && !is_conversion_phase("api-loaded"));
        assert_eq!(conversion_detail("database-convert: org 1/4"), "org 1/4");
        assert_eq!(conversion_detail("database-convert"), "");
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
    fn colgado_exige_silencio_y_varias_sondas() {
        let t0 = Instant::now();
        let mut watch = LivenessWatch::new(LIVENESS, t0);
        let later = |s: u64| t0 + Duration::from_secs(s);
        watch.record(Some("timeout".into()), later(30));
        watch.record(Some("timeout".into()), later(60));
        assert!(!watch.hung(later(400)), "dos sondas no alcanzan");
        watch.record(Some("timeout".into()), later(90));
        assert!(!watch.hung(later(200)), "tres sondas sin el silencio no alcanzan");
        assert!(watch.hung(later(300)));
        watch.record(None, later(301));
        assert!(!watch.hung(later(400)) && watch.failures() == 0, "una respuesta buena lo reinicia");
        assert_eq!(LIVENESS.restart_limit, 3);
    }

    #[test]
    fn el_presupuesto_de_reinicios_es_por_ventana() {
        let t0 = Instant::now();
        let mut budget = RestartBudget::new(3, Duration::from_secs(3600));
        let at = |s: u64| t0 + Duration::from_secs(s);
        assert!(budget.consider(at(0)) && budget.consider(at(10)) && budget.consider(at(20)));
        assert!(!budget.consider(at(30)), "el cuarto en una hora no");
        assert!(budget.consider(at(3700)), "pasada la ventana, sí");
        budget.reset();
        assert_eq!(budget.recent(), 0);
    }

    #[test]
    fn el_presupuesto_de_salida_es_el_de_electron() {
        // max(8, 4) + 8 + 5 + 5 = 26 s, `QUIT_STOP_BUDGET_MS`.
        assert_eq!(QUIT_STOP_BUDGET, Duration::from_secs(26));
    }

    #[test]
    fn la_raiz_sin_candado_esta_libre() {
        let dir = std::env::temp_dir().join(format!("orgtree-lock-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert!(root_released(&dir));
        std::fs::write(dir.join(ROOT_LOCK_FILE), b"0").unwrap();
        assert!(root_released(&dir), "un candado que nadie tiene se puede escribir");
        let _ = std::fs::remove_dir_all(&dir);
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

    #[test]
    fn raices_prohibidas_en_ambos_sentidos() {
        let base = std::env::temp_dir().join(format!("orgtree-forbidden-{}", std::process::id()));
        let real = base.join("Orgtree v2").join("data");
        let forbidden = vec![base.join("Orgtree v2")];
        assert!(check_forbidden(&real, &forbidden).is_err(), "dentro de la raíz real");
        assert!(check_forbidden(&base, &forbidden).is_err(), "contiene la raíz real");
        assert!(check_forbidden(&base.join("Orgtree v2 spike"), &forbidden).is_ok(), "solo comparte el prefijo");
        assert!(check_forbidden(&base.join("spike").join("data"), &forbidden).is_ok());
        assert!(check_forbidden(&real, &[PathBuf::from("relativa")]).is_ok(), "una ruta relativa no cuenta");
    }

    #[cfg(windows)]
    #[test]
    fn raiz_prohibida_sin_mayusculas() {
        assert!(within(Path::new(r"C:\Users\X\AppData\Roaming\orgtree V2\data"), Path::new(r"c:\users\x\appdata\roaming\Orgtree v2")));
    }

    #[cfg(windows)]
    #[test]
    fn raiz_sin_prefijo_largo_y_sin_mayusculas() {
        assert!(same_root(r"C:\Datos\Orgtree", Path::new(r"\\?\c:\datos\orgtree\")));
    }
}
