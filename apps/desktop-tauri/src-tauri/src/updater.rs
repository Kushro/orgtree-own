//! Actualizaciones automáticas desde las pre-releases de la rama (#23), como el
//! `UpdateController` y el camino de inactividad de `apps/desktop/main`:
//!
//! - **Feed**: `latest.json` de la pre-release móvil `tauri-preview-latest`
//!   (`releases/download/…`, sin token ni límite de la API). El CI lo reemplaza
//!   en cada pre-release `tauri-preview-N` y apunta al instalador NSIS de esa.
//! - **Firma**: `tauri-plugin-updater` descarga y verifica con la clave pública
//!   compilada (`ORGTREE_TAURI_UPDATER_PUBKEY` al compilar), y exige que la firma
//!   nombre la versión que anuncia el feed (`requireSignedVersion`). Sin clave,
//!   o fuera de una instalación NSIS, el updater no existe: `unavailable`.
//! - **Cuándo**: el sondeo de 5 s de `lifecycle` llama `tick`. Busca al arrancar
//!   y cada 6 h, con espera exponencial ante fallos (5 min, el doble, hasta 6 h),
//!   solo con `automaticUpdates` prendido. El paquete descargado espera en disco.
//! - **Instalar**: solo en un punto seguro, como Electron: `automaticUpdates`
//!   prendido, el motor quieto (`idle` de `/api/desktop/status`: ningún turno,
//!   cola ni importación), 60 s sin teclado ni mouse, ningún pedido de
//!   mantenimiento pendiente y la carpeta instalada escribible. Nunca interrumpe
//!   un turno. "Update now" del renderer instala a pedido del usuario.
//! - **Salida**: antes de lanzar el instalador (el plugin lo lanza y termina el
//!   proceso), `before_exit` guarda la sesión y apaga el motor por el mismo
//!   camino que la salida ordenada (`stop_engine_for_exit`, camino `update`),
//!   porque el instalador reemplaza el Python y PostgreSQL que usa el motor. El
//!   instalador NSIS (`/P /UPDATE /R`) vuelve a abrir la app.
//!
//! Para la prueba, `ORGTREE_TAURI_UPDATE_FEED` cambia el feed (https, o http
//! solo en loopback: la firma se verifica igual) y `ORGTREE_TAURI_UPDATE_PROBE`
//! anota cada paso en un JSONL.

use crate::{mainwin, Shell};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Manager, Url, Webview};
use tauri_plugin_updater::{Update, UpdaterExt};

/// La clave pública del updater (la de `tauri signer generate`, en base64). La
/// pone el CI desde la variable del repositorio; un build local no la tiene.
pub const PUBKEY: Option<&str> = option_env!("ORGTREE_TAURI_UPDATER_PUBKEY");
/// El feed de la rama: un asset de la pre-release móvil, que GitHub sirve sin token.
pub const DEFAULT_FEED: &str =
    "https://github.com/Kushro/orgtree-own/releases/download/tauri-preview-latest/latest.json";
const FEED_ENV: &str = "ORGTREE_TAURI_UPDATE_FEED";
const PROBE_ENV: &str = "ORGTREE_TAURI_UPDATE_PROBE";
/// `periodicMs` y `backoffBaseMs` de `UpdateController`.
const PERIODIC: Duration = Duration::from_secs(6 * 60 * 60);
const BACKOFF_BASE: Duration = Duration::from_secs(5 * 60);
/// El pedido del feed (no la descarga, que puede ser de cientos de MB).
const CHECK_TIMEOUT: Duration = Duration::from_secs(30);
/// `powerMonitor.getSystemIdleTime() >= 60` en Electron.
const USER_IDLE_TO_INSTALL: Duration = Duration::from_secs(60);
/// El instalador NSIS que deja Tauri junto al ejecutable: la marca de que esta
/// copia está instalada (y no es el `.exe` de `target/` de una prueba).
const UNINSTALLER: &str = "uninstall.exe";

// ------------------------------------------------------------- reglas puras

/// Cuándo toca buscar: `tick` de `UpdateController`. Un fallo se juzga solo
/// por `next_retry`; sin fallos, por el último chequeo que salió bien.
#[derive(Debug, Default, Clone)]
pub struct Schedule {
    last_success: Option<Instant>,
    failures: u32,
    next_retry: Option<Instant>,
}

impl Schedule {
    pub fn due(&self, now: Instant) -> bool {
        if self.failures > 0 {
            return self.next_retry.is_none_or(|at| now >= at);
        }
        self.last_success.is_none_or(|at| now.duration_since(at) >= PERIODIC)
    }

    pub fn succeeded(&mut self, now: Instant) {
        self.failures = 0;
        self.next_retry = None;
        self.last_success = Some(now);
    }

    /// 5 min, 10, 20… hasta el período de 6 h.
    pub fn failed(&mut self, now: Instant) -> Duration {
        self.failures += 1;
        let factor = 2u32.saturating_pow(self.failures - 1);
        let delay = BACKOFF_BASE.saturating_mul(factor).min(PERIODIC);
        self.next_retry = Some(now + delay);
        delay
    }
}

/// Por qué un paquete descargado todavía no se instala solo.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hold {
    AutomaticOff,
    Maintenance,
    EngineBusy,
    UserActive,
    NotWritable,
}

impl Hold {
    pub fn as_str(self) -> &'static str {
        match self {
            Hold::AutomaticOff => "automatic-off",
            Hold::Maintenance => "maintenance-pending",
            Hold::EngineBusy => "engine-busy",
            Hold::UserActive => "user-active",
            Hold::NotWritable => "not-writable",
        }
    }
}

/// El punto seguro de Electron (`automaticUpdates && downloaded && stats.idle
/// && idle >= 60 && !maintenance`), más la carpeta escribible que exige
/// `applyDownloadedUpdate` para instalar sin nadie presente.
pub fn install_gate(automatic: bool, maintenance: bool, engine_idle: bool, user_idle: Duration, writable: bool) -> Result<(), Hold> {
    if !automatic {
        return Err(Hold::AutomaticOff);
    }
    if maintenance {
        return Err(Hold::Maintenance);
    }
    if !engine_idle {
        return Err(Hold::EngineBusy);
    }
    if user_idle < USER_IDLE_TO_INSTALL {
        return Err(Hold::UserActive);
    }
    if !writable {
        return Err(Hold::NotWritable);
    }
    Ok(())
}

/// El feed: el de la rama, o el de la prueba. https siempre; http solo hacia
/// loopback (un servidor local de la prueba). La firma se verifica igual.
pub fn feed_url(overridden: Option<&str>) -> Result<Url, String> {
    let Some(text) = overridden.map(str::trim).filter(|t| !t.is_empty()) else {
        return Url::parse(DEFAULT_FEED).map_err(|e| e.to_string());
    };
    let url = Url::parse(text).map_err(|e| format!("feed inválido: {e}"))?;
    let host = url.host_str().unwrap_or_default();
    let loopback = host == "localhost"
        || host.trim_start_matches('[').trim_end_matches(']').parse::<std::net::IpAddr>().is_ok_and(|ip| ip.is_loopback());
    match url.scheme() {
        "https" => Ok(url),
        "http" if loopback => Ok(url),
        _ => Err(format!("feed rechazado (solo https, o http en loopback): {url}")),
    }
}

/// `updateOfferIsNewer`: con un paquete ya listo, solo una versión mayor lo reemplaza.
pub fn offer_is_newer(offered: &str, prepared: Option<&str>) -> bool {
    let Some(prepared) = prepared else { return true };
    match (semver::Version::parse(offered), semver::Version::parse(prepared)) {
        (Ok(offered), Ok(prepared)) => offered > prepared,
        _ => false,
    }
}

/// La verificación del plugin, repetida sobre el paquete guardado en disco
/// antes de instalarlo: la firma minisign con la clave compilada y la versión
/// firmada (comentario de confianza `version:`) igual a la anunciada.
pub fn verify_package(data: &[u8], signature: &str, pubkey: &str, version: &str) -> Result<(), String> {
    use base64::Engine as _;
    let text = |value: &str, what: &str| -> Result<String, String> {
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(value.trim())
            .map_err(|e| format!("{what}: {e}"))?;
        String::from_utf8(bytes).map_err(|_| format!("{what}: no es texto"))
    };
    let key = minisign_verify::PublicKey::decode(&text(pubkey, "clave pública")?).map_err(|e| format!("clave pública: {e}"))?;
    let signature = minisign_verify::Signature::decode(&text(signature, "firma")?).map_err(|e| format!("firma: {e}"))?;
    key.verify(data, &signature, true).map_err(|e| format!("la firma no verifica: {e}"))?;
    let signed = signature
        .trusted_comment()
        .split('\t')
        .find_map(|part| part.strip_prefix("version:"))
        .ok_or("la firma no nombra una versión")?;
    if signed != version {
        return Err(format!("la firma es de la versión {signed} y el feed anuncia {version}"));
    }
    Ok(())
}

// ------------------------------------------------------------------- estado

struct Prepared {
    update: Update,
    path: PathBuf,
    version: String,
}

#[derive(Default)]
struct Inner {
    /// El `UpdateStatus` del contrato (`state`, `version`, `percent`, `recheck`).
    status: Value,
    prepared: Option<Prepared>,
    checking: bool,
    downloading: bool,
    applying: bool,
    schedule: Schedule,
    hold: Option<Hold>,
    /// El primer sondeo con el motor listo ya se anotó (para la prueba).
    ticked: bool,
}

pub struct Updates {
    inner: Mutex<Inner>,
    /// Pubkey compilada y copia instalada por NSIS. Se fija en `start`.
    supported: Mutex<Option<bool>>,
    install_dir: Mutex<PathBuf>,
    writable: Mutex<bool>,
    probe: Option<PathBuf>,
    began: Instant,
}

impl Default for Updates {
    fn default() -> Self {
        Updates {
            inner: Mutex::new(Inner { status: json!({ "state": "idle" }), ..Default::default() }),
            supported: Mutex::new(None),
            install_dir: Mutex::new(PathBuf::new()),
            writable: Mutex::new(false),
            probe: std::env::var_os(PROBE_ENV).filter(|v| !v.is_empty()).map(PathBuf::from),
            began: Instant::now(),
        }
    }
}

fn updates(app: &AppHandle) -> &Updates {
    app.state::<Updates>().inner()
}

fn supported(app: &AppHandle) -> bool {
    updates(app).supported.lock().unwrap().unwrap_or(false)
}

/// Una línea del registro de la prueba (`ORGTREE_TAURI_UPDATE_PROBE`).
fn probe(app: &AppHandle, event: &str, mut data: Value) {
    let state = updates(app);
    let Some(path) = state.probe.as_ref() else { return };
    if !data.is_object() {
        data = json!({ "detail": data });
    }
    data["event"] = json!(event);
    data["ms"] = json!(state.began.elapsed().as_millis() as u64);
    data["pid"] = json!(std::process::id());
    data["appVersion"] = json!(app.package_info().version.to_string());
    if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        use std::io::Write;
        let _ = writeln!(file, "{data}");
    }
}

fn set_status(app: &AppHandle, inner: &mut Inner, status: Value) {
    let changed = inner.status["state"] != status["state"] || inner.status["version"] != status["version"];
    inner.status = status.clone();
    mainwin::broadcast(app, json!({ "type": "update", "data": status }));
    if changed {
        probe(app, "status", status);
    }
}

pub fn status(app: &AppHandle) -> Value {
    if !supported(app) {
        return json!({ "state": "unavailable" });
    }
    updates(app).inner.lock().unwrap().status.clone()
}

fn automatic(app: &AppHandle) -> bool {
    app.state::<Shell>().preferences.lock().unwrap().bool("automaticUpdates")
}

fn updates_dir(app: &AppHandle) -> Option<PathBuf> {
    crate::profile_dir(app).ok().map(|profile| profile.join("updates"))
}

/// La carpeta instalada se puede escribir (un archivo de prueba), como
/// `installDirectoryWritable`: sin eso, el instalador pediría permisos que
/// nadie está para dar.
fn directory_writable(dir: &Path) -> bool {
    let probe = dir.join(format!(".orgtree-update-probe-{}", std::process::id()));
    let ok = std::fs::write(&probe, b"").is_ok();
    let _ = std::fs::remove_file(&probe);
    ok
}

/// Fija si hay updater, limpia paquetes de una ejecución anterior y lo anota.
pub fn start(app: &AppHandle) {
    let exe = std::env::current_exe().ok();
    let dir = exe.as_deref().and_then(Path::parent).map(Path::to_path_buf).unwrap_or_default();
    let installed = dir.join(UNINSTALLER).is_file();
    let keyed = PUBKEY.is_some_and(|key| !key.trim().is_empty());
    let feed = feed_url(std::env::var(FEED_ENV).ok().as_deref());
    let supported = cfg!(windows) && keyed && installed && feed.is_ok();
    let writable = directory_writable(&dir);
    let state = updates(app);
    *state.supported.lock().unwrap() = Some(supported);
    *state.install_dir.lock().unwrap() = dir.clone();
    *state.writable.lock().unwrap() = writable;
    if let Some(updates) = updates_dir(app) {
        let _ = std::fs::remove_dir_all(updates);
    }
    probe(app, "start", json!({
        "supported": supported,
        "keyed": keyed,
        "installed": installed,
        "feed": feed.as_ref().map(Url::to_string).unwrap_or_else(|e| e.clone()),
        "installDirectory": dir,
        "unattendedInstall": writable,
        "automaticUpdates": automatic(app),
    }));
}

// -------------------------------------------------------------- operaciones

fn updater(app: &AppHandle) -> Result<tauri_plugin_updater::Updater, String> {
    let key = PUBKEY.filter(|k| !k.trim().is_empty()).ok_or("esta copia no tiene la clave del updater")?;
    let feed = feed_url(std::env::var(FEED_ENV).ok().as_deref())?;
    let hook = app.clone();
    app.updater_builder()
        .pubkey(key)
        .endpoints(vec![feed])
        .map_err(|e| e.to_string())?
        .timeout(CHECK_TIMEOUT)
        .on_before_exit(move || before_exit(&hook))
        .build()
        .map_err(|e| e.to_string())
}

/// Lo que responde un chequeo con un paquete ya listo: el paquete sigue y el
/// chequeo se contesta con `recheck` (como `UpdateController`).
fn answered(inner: &Inner, recheck: &str) -> Value {
    match inner.prepared.as_ref() {
        Some(prepared) => json!({ "state": "pending-idle", "version": prepared.version, "recheck": recheck }),
        None => json!({ "state": recheck }),
    }
}

fn discard_prepared(inner: &mut Inner) {
    if let Some(prepared) = inner.prepared.take() {
        let _ = std::fs::remove_file(&prepared.path);
    }
}

/// Un chequeo: el automático de `tick` o el de "Check for updates". Uno a la
/// vez; durante una descarga se contesta el estado actual.
async fn run_check(app: AppHandle, manual: bool) -> Value {
    {
        let mut inner = updates(&app).inner.lock().unwrap();
        if inner.checking || inner.downloading || inner.applying {
            return inner.status.clone();
        }
        inner.checking = true;
        set_status(&app, &mut inner, json!({ "state": "checking" }));
    }
    probe(&app, "check", json!({ "manual": manual }));
    let outcome = match updater(&app) {
        Ok(updater) => updater.check().await.map_err(|e| e.to_string()),
        Err(error) => Err(error),
    };
    let mut inner = updates(&app).inner.lock().unwrap();
    inner.checking = false;
    let now = Instant::now();
    match outcome {
        Ok(Some(update)) if offer_is_newer(&update.version, inner.prepared.as_ref().map(|p| p.version.as_str())) => {
            inner.schedule.succeeded(now);
            discard_prepared(&mut inner);
            inner.downloading = true;
            inner.hold = None;
            let version = update.version.clone();
            set_status(&app, &mut inner, json!({ "state": "downloading", "version": version }));
            let task = app.clone();
            tauri::async_runtime::spawn(async move { download(task, update).await });
        }
        Ok(_) => {
            inner.schedule.succeeded(now);
            let status = answered(&inner, "up-to-date");
            set_status(&app, &mut inner, status);
        }
        Err(error) => {
            let retry = inner.schedule.failed(now);
            probe(&app, "error", json!({ "stage": "check", "message": error, "retryInS": retry.as_secs() }));
            let status = answered(&inner, "unavailable");
            set_status(&app, &mut inner, status);
        }
    }
    inner.status.clone()
}

/// Descarga y verifica (el plugin), y guarda el paquete en `<perfil>/updates`
/// hasta el punto seguro: no queda en memoria mientras espera.
async fn download(app: AppHandle, update: Update) {
    let version = update.version.clone();
    let mut received: u64 = 0;
    let mut shown: u64 = 0;
    let progress = app.clone();
    let bytes = update
        .download(
            |chunk, total| {
                received += chunk as u64;
                let Some(total) = total.filter(|t| *t > 0) else { return };
                let percent = (received * 100 / total).min(100);
                if percent >= shown + 5 {
                    shown = percent - percent % 5;
                    let mut inner = updates(&progress).inner.lock().unwrap();
                    if inner.downloading {
                        inner.status = json!({ "state": "downloading", "version": version, "percent": shown });
                        mainwin::broadcast(&progress, json!({ "type": "update", "data": inner.status }));
                    }
                }
            },
            || {},
        )
        .await
        .map_err(|e| e.to_string());
    let saved = bytes.and_then(|bytes| {
        let dir = updates_dir(&app).ok_or("sin carpeta de la app")?;
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let path = dir.join(format!("orgtree-tauri-{}-setup.exe", update.version));
        let partial = path.with_extension("part");
        std::fs::write(&partial, &bytes).map_err(|e| e.to_string())?;
        std::fs::rename(&partial, &path).map_err(|e| e.to_string())?;
        Ok((path, bytes.len()))
    });
    let mut inner = updates(&app).inner.lock().unwrap();
    inner.downloading = false;
    match saved {
        Ok((path, size)) => {
            probe(&app, "downloaded", json!({ "version": update.version, "bytes": size }));
            let version = update.version.clone();
            inner.prepared = Some(Prepared { update, path, version: version.clone() });
            set_status(&app, &mut inner, json!({ "state": "pending-idle", "version": version }));
        }
        Err(error) => {
            let retry = inner.schedule.failed(Instant::now());
            probe(&app, "error", json!({ "stage": "download", "message": error, "retryInS": retry.as_secs() }));
            set_status(&app, &mut inner, json!({ "state": "failed" }));
        }
    }
}

/// Instala el paquete listo. En Windows, si el instalador arranca, el plugin
/// termina el proceso después de `before_exit`; volver acá es un fallo.
fn apply(app: AppHandle, automatic: bool) {
    let (update, path, version) = {
        let mut inner = updates(&app).inner.lock().unwrap();
        if inner.applying || inner.checking || inner.downloading {
            return;
        }
        let Some(prepared) = inner.prepared.as_ref() else { return };
        let taken = (prepared.update.clone(), prepared.path.clone(), prepared.version.clone());
        inner.applying = true;
        taken
    };
    probe(&app, "apply", json!({ "automatic": automatic, "version": version }));
    let result = std::fs::read(&path)
        .map_err(|e| format!("no se pudo leer el paquete: {e}"))
        .and_then(|bytes| {
            verify_package(&bytes, &update.signature, PUBKEY.unwrap_or_default(), &version)?;
            update.install(bytes).map_err(|e| e.to_string())
        });
    let mut inner = updates(&app).inner.lock().unwrap();
    inner.applying = false;
    let Err(error) = result else { return };
    probe(&app, "error", json!({ "stage": "install", "message": error }));
    discard_prepared(&mut inner);
    inner.schedule.failed(Instant::now());
    set_status(&app, &mut inner, json!({ "state": "failed" }));
    drop(inner);
    // Si el motor ya se apagó para el instalador, la app vuelve a empezar entera.
    if app.state::<Shell>().quitting.load(Ordering::SeqCst) {
        app.restart();
    }
}

/// Justo antes de lanzar el instalador: la salida ordenada de `mainwin::shutdown`
/// sin `app.exit` (el plugin termina el proceso). El instalador reemplaza el
/// runtime del motor, así que el motor tiene que haber soltado sus archivos.
fn before_exit(app: &AppHandle) {
    probe(app, "handoff", json!({}));
    let shell = app.state::<Shell>();
    crate::lifecycle::note_exit_trigger(app, "update");
    let first = !shell.quitting.swap(true, Ordering::SeqCst);
    if first {
        mainwin::save_session(app);
    }
    for (_, window) in app.webview_windows() {
        let _ = window.eval("window.dispatchEvent(new Event('orgtree:before-exit'))");
    }
    std::thread::sleep(Duration::from_millis(300));
    for (_, window) in app.webview_windows() {
        let _ = window.hide();
    }
    shell.logins.cancel_all();
    crate::lifecycle::stop_engine_for_exit(app, "update");
    probe(app, "engine-stopped", json!({}));
    app.cleanup_before_exit();
}

/// Cada sondeo de `/api/desktop/status` (5 s, con el motor listo).
pub fn tick(app: &AppHandle, stats: &Value) {
    if !supported(app) || app.state::<Shell>().quitting.load(Ordering::SeqCst) {
        return;
    }
    let automatic = automatic(app);
    let state = updates(app);
    let mut inner = state.inner.lock().unwrap();
    if !inner.ticked {
        inner.ticked = true;
        probe(app, "tick", json!({ "automaticUpdates": automatic, "engineIdle": stats["idle"], "maintenance": stats["maintenance"] }));
    }
    if inner.checking || inner.downloading || inner.applying {
        return;
    }
    if inner.prepared.is_some() {
        let gate = install_gate(
            automatic,
            !stats["maintenance"].is_null(),
            stats["idle"].as_bool() == Some(true),
            crate::lifecycle::user_idle(),
            *state.writable.lock().unwrap(),
        );
        match gate {
            Ok(()) => {
                inner.hold = None;
                drop(inner);
                let task = app.clone();
                std::thread::spawn(move || apply(task, true));
            }
            Err(hold) if inner.hold != Some(hold) => {
                inner.hold = Some(hold);
                probe(app, "hold", json!({ "reason": hold.as_str() }));
            }
            Err(_) => {}
        }
        return;
    }
    if automatic && inner.schedule.due(Instant::now()) {
        drop(inner);
        let task = app.clone();
        tauri::async_runtime::spawn(async move {
            run_check(task, false).await;
        });
    }
}

// ----------------------------------------------------------------- comandos

#[tauri::command]
pub fn desktop_update_status(webview: Webview) -> Result<Value, String> {
    crate::desktop::authorize(&webview)?;
    Ok(status(webview.app_handle()))
}

/// "Check for updates": no depende de `automaticUpdates` ni de la espera por fallos.
#[tauri::command]
pub async fn desktop_check_updates(webview: Webview) -> Result<Value, String> {
    crate::desktop::authorize(&webview)?;
    let app = webview.app_handle().clone();
    if !supported(&app) {
        return Ok(json!({ "state": "unavailable" }));
    }
    Ok(run_check(app, true).await)
}

/// "Update now": a pedido del usuario, sin esperar el punto de inactividad.
#[tauri::command]
pub fn desktop_install_update(webview: Webview) -> Result<(), String> {
    crate::desktop::authorize(&webview)?;
    let app = webview.app_handle().clone();
    {
        let inner = updates(&app).inner.lock().unwrap();
        if !supported(&app) || inner.prepared.is_none() {
            return Err("No hay una actualización descargada lista para instalar.".into());
        }
        if inner.checking || inner.downloading || inner.applying {
            return Err("Orgtree está buscando o instalando una actualización. Probá de nuevo en un momento.".into());
        }
    }
    std::thread::spawn(move || apply(app, false));
    Ok(())
}

#[tauri::command]
pub fn desktop_update_capability(webview: Webview) -> Result<Value, String> {
    crate::desktop::authorize(&webview)?;
    let state = updates(webview.app_handle());
    Ok(json!({
        "unattendedInstall": *state.writable.lock().unwrap(),
        "installDirectory": state.install_dir.lock().unwrap().clone(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    // Firmado con una clave de prueba descartable (`tauri signer generate`),
    // cuya privada no está en el repositorio: solo sirve para verificar.
    const TEST_PUBKEY: &str = "dW50cnVzdGVkIGNvbW1lbnQ6IG1pbmlzaWduIHB1YmxpYyBrZXk6IENBREQ2QkE0Nzk3MkNBQzIKUldUQ3luSjVwR3ZkeW1qZ05zV2V3cVkrTnlwM0RRYlVadmFOMHRBSndVQzMwK3NPZHNEUmVLbXUK";
    const FIXTURE: &[u8] = b"orgtree tauri update fixture\n";
    const FIXTURE_SIG: &str = "dW50cnVzdGVkIGNvbW1lbnQ6IHNpZ25hdHVyZSBmcm9tIHRhdXJpIHNlY3JldCBrZXkKUlVUQ3luSjVwR3ZkeXB0em1QMFBlVDBUcU4yYnV2RVkzbzJkVjNXL0h0WE5kdEthQlEzbFp6ZG4vRlRaM1VIN3dzcEsxWHNhMTB6ajN3cHhZUUZlSUJLOWRnTHFVRzJyS0E0PQp0cnVzdGVkIGNvbW1lbnQ6IHRpbWVzdGFtcDoxNzkxMzk4OTY0CWZpbGU6Zml4dHVyZS5iaW4JdmVyc2lvbjowLjIuMApncEI1dzBzZzZuZUpOK0Z5T1FjU0Qxa0VIemJSdjVQZW9pOFJYOXRPUkdXMW5qbkM2akF1ZXVFbFc0RWxMYVRCcVdJR1UzQklScFFSVFFSODFlL2ZDQT09Cg==";

    #[test]
    fn a_signed_package_verifies_only_as_signed() {
        assert_eq!(verify_package(FIXTURE, FIXTURE_SIG, TEST_PUBKEY, "0.2.0"), Ok(()));
        let mut tampered = FIXTURE.to_vec();
        tampered.push(b'x');
        assert!(verify_package(&tampered, FIXTURE_SIG, TEST_PUBKEY, "0.2.0").is_err());
        // El feed no puede anunciar otra versión con la firma de esta.
        assert!(verify_package(FIXTURE, FIXTURE_SIG, TEST_PUBKEY, "0.3.0").unwrap_err().contains("0.2.0"));
        assert!(verify_package(FIXTURE, "bm8=", TEST_PUBKEY, "0.2.0").is_err());
    }

    #[test]
    fn the_compiled_key_if_any_is_a_minisign_public_key() {
        if let Some(key) = PUBKEY.filter(|k| !k.trim().is_empty()) {
            use base64::Engine as _;
            let text = String::from_utf8(base64::engine::general_purpose::STANDARD.decode(key.trim()).unwrap()).unwrap();
            minisign_verify::PublicKey::decode(&text).expect("ORGTREE_TAURI_UPDATER_PUBKEY no es una clave pública de tauri signer");
        }
    }

    #[test]
    fn never_installs_while_a_turn_runs_or_the_user_is_active() {
        let quiet = Duration::from_secs(600);
        assert_eq!(install_gate(true, false, true, quiet, true), Ok(()));
        assert_eq!(install_gate(true, false, false, quiet, true), Err(Hold::EngineBusy));
        assert_eq!(install_gate(true, false, true, Duration::from_secs(59), true), Err(Hold::UserActive));
        assert_eq!(install_gate(false, false, true, quiet, true), Err(Hold::AutomaticOff));
        assert_eq!(install_gate(true, true, true, quiet, true), Err(Hold::Maintenance));
        assert_eq!(install_gate(true, false, true, quiet, false), Err(Hold::NotWritable));
    }

    #[test]
    fn checks_every_six_hours_and_backs_off_on_failure() {
        let t0 = Instant::now();
        let mut schedule = Schedule::default();
        assert!(schedule.due(t0));
        schedule.succeeded(t0);
        assert!(!schedule.due(t0 + Duration::from_secs(60)));
        assert!(schedule.due(t0 + PERIODIC));
        assert_eq!(schedule.failed(t0), BACKOFF_BASE);
        assert!(!schedule.due(t0 + BACKOFF_BASE - Duration::from_secs(1)));
        assert!(schedule.due(t0 + BACKOFF_BASE));
        assert_eq!(schedule.failed(t0), BACKOFF_BASE * 2);
        for _ in 0..20 {
            schedule.failed(t0);
        }
        assert_eq!(schedule.failed(t0), PERIODIC);
        schedule.succeeded(t0);
        assert!(!schedule.due(t0 + BACKOFF_BASE));
    }

    #[test]
    fn the_feed_is_https_or_a_local_test_server() {
        assert_eq!(feed_url(None).unwrap().as_str(), DEFAULT_FEED);
        assert_eq!(feed_url(Some("  ")).unwrap().as_str(), DEFAULT_FEED);
        assert!(feed_url(Some("http://127.0.0.1:8123/latest.json")).is_ok());
        assert!(feed_url(Some("http://localhost:8123/latest.json")).is_ok());
        assert!(feed_url(Some("http://[::1]:8123/latest.json")).is_ok());
        assert!(feed_url(Some("https://example.com/latest.json")).is_ok());
        assert!(feed_url(Some("http://example.com/latest.json")).is_err());
        assert!(feed_url(Some("http://127.0.0.1.example.com/latest.json")).is_err());
        assert!(feed_url(Some("file:///C:/latest.json")).is_err());
    }

    #[test]
    fn a_prepared_package_is_replaced_only_by_a_newer_one() {
        assert!(offer_is_newer("0.1.5", None));
        assert!(offer_is_newer("0.1.6", Some("0.1.5")));
        assert!(!offer_is_newer("0.1.5", Some("0.1.5")));
        assert!(!offer_is_newer("0.1.4", Some("0.1.5")));
        assert!(!offer_is_newer("garbage", Some("0.1.5")));
    }
}
