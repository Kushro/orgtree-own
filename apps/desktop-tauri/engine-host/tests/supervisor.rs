//! Pruebas del supervisor contra un motor falso y, si hay un Python con las
//! dependencias del motor, contra el `engine/launch.py` real.
//!
//! - `FAKE_ENGINE_PYTHON` (o `python3`/`python` en el PATH) corre el motor falso.
//! - La prueba con el motor real está marcada `#[ignore]`: se corre con
//!   `cargo test -- --include-ignored` y `ORGTREE_TEST_ENGINE_PYTHON`.

use orgtree_engine_host::{Engine, EngineError, EngineOptions, StopOutcome};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use std::time::Duration;

/// `FAKE_ENGINE_MODE` es global del proceso: las pruebas que lo usan van de a una.
static SERIAL: Mutex<()> = Mutex::new(());

fn which(name: &str) -> Option<PathBuf> {
    let paths = std::env::var_os("PATH")?;
    std::env::split_paths(&paths).find_map(|dir| {
        [name.to_string(), format!("{name}.exe")]
            .into_iter()
            .map(|n| dir.join(n))
            .find(|p| p.is_file())
    })
}

fn fake_python() -> PathBuf {
    if let Some(p) = std::env::var_os("FAKE_ENGINE_PYTHON") {
        return PathBuf::from(p);
    }
    which("python3").or_else(|| which("python")).expect("hace falta un Python en el PATH para el motor falso")
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("orgtree-engine-host-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

fn fake_options(name: &str) -> EngineOptions {
    let engine_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests").join("fake_engine");
    let mut options = EngineOptions::new(fake_python(), engine_dir, scratch(name));
    options.silence_timeout = Duration::from_secs(10);
    options
}

fn with_mode<T>(mode: &str, body: impl FnOnce() -> T) -> T {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    std::env::set_var("FAKE_ENGINE_MODE", mode);
    let result = body();
    std::env::remove_var("FAKE_ENGINE_MODE");
    result
}

#[test]
fn arranca_responde_con_token_y_se_apaga_limpio() {
    with_mode("ready", || {
        let mut phases = Vec::new();
        let mut engine = Engine::start_with(&fake_options("ready"), &mut |p| phases.push(p.to_string())).expect("ready");
        assert_eq!(phases, ["starting", "progress"]);
        assert!(engine.is_running());
        assert!(engine.origin().starts_with("http://127.0.0.1:"));
        let (status, _) = engine.get("/api/desktop/identity", Duration::from_secs(5)).unwrap();
        assert_eq!(status, 200);
        let (status, _) = orgtree_engine_host::http_request(engine.port(), "GET", "/api/desktop/identity", None, Duration::from_secs(5)).unwrap();
        assert_eq!(status, 401, "sin token el motor rechaza");
        assert!(!format!("{engine:?}").contains(engine.token().expose()));
        assert_eq!(engine.stop(), StopOutcome::Graceful);
    });
}

#[test]
fn si_ignora_el_apagado_mata_el_arbol() {
    with_mode("ignore-shutdown", || {
        let engine = Engine::start(&fake_options("ignore")).expect("ready");
        assert_eq!(engine.stop(), StopOutcome::Killed);
    });
}

#[test]
fn kill_simula_una_caida() {
    with_mode("ready", || {
        let mut engine = Engine::start(&fake_options("kill")).expect("ready");
        assert!(engine.kill());
        assert!(!engine.is_running());
        assert_eq!(engine.stop(), StopOutcome::Graceful, "un motor ya caído no necesita apagado");
    });
}

#[test]
fn ready_de_otro_pid_se_rechaza_y_no_deja_el_proceso() {
    with_mode("bad-pid", || {
        let error = Engine::start(&fake_options("bad-pid")).unwrap_err();
        assert!(matches!(error, EngineError::InvalidReady(_)), "{error:?}");
    });
}

#[test]
fn rechazo_por_raiz_ocupada() {
    with_mode("refused", || {
        let error = Engine::start(&fake_options("refused")).unwrap_err();
        assert!(matches!(error, EngineError::RootOwned(_)), "{error:?}");
    });
}

#[test]
fn salida_antes_de_ready() {
    with_mode("exit", || {
        assert_eq!(Engine::start(&fake_options("exit")).unwrap_err(), EngineError::ExitedEarly);
    });
}

#[test]
fn silencio_vence_el_plazo() {
    with_mode("silent", || {
        let mut options = fake_options("silent");
        options.silence_timeout = Duration::from_secs(2);
        assert_eq!(Engine::start(&options).unwrap_err(), EngineError::Timeout);
    });
}

#[test]
fn los_checkpoints_reinician_el_plazo() {
    with_mode("slow", || {
        let mut options = fake_options("slow");
        // 4 checkpoints a 0,6 s: el total (2,4 s) supera el plazo, ninguno lo hace solo.
        options.silence_timeout = Duration::from_millis(1500);
        let engine = Engine::start(&options).expect("los checkpoints mantienen vivo el arranque");
        assert_eq!(engine.stop(), StopOutcome::Graceful);
    });
}

#[test]
fn configuracion_invalida() {
    let mut options = fake_options("config");
    options.python = PathBuf::from("python-relativo");
    assert!(matches!(Engine::start(&options).unwrap_err(), EngineError::Config(_)));
    let mut options = fake_options("config2");
    options.engine_dir = std::env::temp_dir().join("orgtree-no-existe");
    assert!(matches!(Engine::start(&options).unwrap_err(), EngineError::Config(_)));
}

/// El motor real (`engine/launch.py`) en Windows, con una raíz descartable.
#[test]
#[ignore = "necesita ORGTREE_TEST_ENGINE_PYTHON con las dependencias del motor"]
fn motor_real_arranca_y_se_apaga() {
    let python = std::env::var_os("ORGTREE_TEST_ENGINE_PYTHON").expect("definir ORGTREE_TEST_ENGINE_PYTHON");
    let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let engine_dir = std::fs::canonicalize(repo.join("engine")).unwrap();
    let data = scratch("real");
    let mut options = EngineOptions::new(python, engine_dir, &data);
    options.silence_timeout = Duration::from_secs(120);
    let mut engine = Engine::start(&options).expect("el motor real llega a ready");
    let pid = engine.pid().unwrap();
    eprintln!("motor real listo: pid {pid}, {}", engine.origin());

    let (status, body) = engine.get("/api/desktop/identity", Duration::from_secs(30)).unwrap();
    assert_eq!(status, 200, "{body}");
    let identity: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(identity["pid"].as_u64(), Some(pid as u64));
    let (status, _) = orgtree_engine_host::http_request(engine.port(), "GET", "/api/desktop/identity", None, Duration::from_secs(30)).unwrap();
    assert_eq!(status, 401, "sin token el TokenGate rechaza");
    assert!(engine.is_running());

    assert_eq!(engine.stop(), StopOutcome::Graceful, "el motor real responde al apagado autenticado");
    assert!(!process_alive(pid), "el proceso del motor terminó");
    let _ = std::fs::remove_dir_all(&data);
}

#[cfg(windows)]
fn process_alive(pid: u32) -> bool {
    let output = Command::new("tasklist").args(["/FI", &format!("PID eq {pid}"), "/NH"]).output().unwrap();
    String::from_utf8_lossy(&output.stdout).contains(&pid.to_string())
}

#[cfg(not(windows))]
fn process_alive(pid: u32) -> bool {
    Command::new("kill").args(["-0", &pid.to_string()]).status().map(|s| s.success()).unwrap_or(false)
}
