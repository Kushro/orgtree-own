//! Pruebas del supervisor contra un motor falso y, si hay un Python con las
//! dependencias del motor, contra el `engine/launch.py` real.
//!
//! - `FAKE_ENGINE_PYTHON` (o `python3`/`python` en el PATH) corre el motor falso.
//! - La prueba con el motor real está marcada `#[ignore]`: se corre con
//!   `cargo test -- --include-ignored` y `ORGTREE_TEST_ENGINE_PYTHON`.

use orgtree_engine_host::{write_engine_paths, Engine, EngineError, EngineOptions, PostgresRuntime, QuitOutcome, StartupEvent, StopOutcome};
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
        let mut engine = Engine::start_with(&fake_options("ready"), &mut |p| phases.push(format!("{p:?}"))).expect("ready");
        assert_eq!(phases, ["Starting", "Progress(\"lifetime-owned\")"]);
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

/// La conversión (#25): las fases llegan al shell, y su plazo mide el silencio
/// entre checkpoints, no el total.
#[test]
fn la_conversion_avisa_sus_fases_y_mide_el_silencio() {
    with_mode("convert", || {
        let mut options = fake_options("convert");
        // Tres pasos a 0,6 s: el total (1,8 s) supera el plazo de conversión, ninguno solo.
        options.conversion_timeout = Duration::from_millis(1500);
        let mut phases = Vec::new();
        let mut engine = Engine::start_with(&options, &mut |p| {
            if let StartupEvent::Converting(phase) = p {
                phases.push(phase.to_string());
            }
        })
        .expect("la conversión llega a ready");
        assert_eq!(phases, ["database-convert: org 1/3", "database-convert: org 2/3", "database-convert: org 3/3"]);
        assert_eq!(engine.stop_for_quit(orgtree_engine_host::QUIT_STOP_BUDGET), QuitOutcome::Stopped);
    });
}

#[test]
fn un_paso_de_conversion_que_calla_demasiado_vence_su_plazo() {
    with_mode("convert-stall", || {
        let mut options = fake_options("convert-stall");
        options.conversion_timeout = Duration::from_millis(1500);
        assert_eq!(Engine::start(&options).unwrap_err(), EngineError::Timeout);
    });
}

#[test]
fn la_conversion_fallida_trae_su_motivo() {
    with_mode("convert-fail", || {
        match Engine::start(&fake_options("convert-fail")).unwrap_err() {
            EngineError::ConversionFailed(reason) => assert!(reason.contains("conversion"), "{reason}"),
            other => panic!("{other:?}"),
        }
    });
}

/// La salida (#25): por las buenas cuando el motor responde, forzada y probada
/// cuando no, siempre dentro del presupuesto.
#[test]
fn la_salida_se_prueba_y_respeta_el_presupuesto() {
    with_mode("ready", || {
        let mut engine = Engine::start(&fake_options("quit")).expect("ready");
        assert_eq!(engine.stop_for_quit(orgtree_engine_host::QUIT_STOP_BUDGET), QuitOutcome::Stopped);
        assert!(!engine.is_running());
        assert_eq!(engine.stop_for_quit(orgtree_engine_host::QUIT_STOP_BUDGET), QuitOutcome::Stopped, "dos veces no rompe");
    });
    with_mode("ignore-shutdown", || {
        let mut engine = Engine::start(&fake_options("quit-forced")).expect("ready");
        let began = std::time::Instant::now();
        // Con un presupuesto corto, la parte amable deja su reserva a la forzada.
        assert_eq!(engine.stop_for_quit(Duration::from_secs(6)), QuitOutcome::Forced);
        assert!(began.elapsed() < Duration::from_secs(7), "{:?}", began.elapsed());
        assert!(!engine.is_running());
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

/// Una carpeta de recursos como la de la app instalada, con archivos vacíos.
fn fake_resources(name: &str, skip: Option<&str>) -> PathBuf {
    let resources = scratch(name);
    let engine = resources.join("engine");
    let python = if cfg!(windows) { "runtime/python.exe" } else { "runtime/python" };
    let mut files = vec![python.to_string(), "launch.py".into(), "pg-custodian.exe".into(), "../tools/pypg/pgimport.py".into()];
    files.extend(PostgresRuntime::TOOLS.iter().map(|tool| format!("postgresql/bin/{tool}.exe")));
    for file in files.iter().filter(|f| Some(f.as_str()) != skip) {
        let path = engine.join(file);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, b"").unwrap();
    }
    resources
}

#[test]
fn modo_empaquetado_usa_los_recursos_y_pide_bootstrap() {
    let resources = fake_resources("packaged", None);
    let data = scratch("packaged-data");
    let options = EngineOptions::packaged(&resources, &data).expect("disposición completa");
    let engine = resources.join("engine");
    assert_eq!(options.engine_dir, engine);
    assert!(options.python.starts_with(engine.join("runtime")));
    assert!(options.bootstrap_postgres);
    let postgres = options.postgres.clone().unwrap();
    assert_eq!(postgres.custodian, engine.join("pg-custodian.exe"));
    assert_eq!(postgres.bin, engine.join("postgresql").join("bin"));

    let descriptor = resources.join("app").join("engine-paths.json");
    write_engine_paths(&descriptor, &options).expect("descriptor");
    let value: serde_json::Value = serde_json::from_slice(&std::fs::read(&descriptor).unwrap()).unwrap();
    assert_eq!(value["schema"], "orgtree.engine-paths/v1");
    assert_eq!(Path::new(value["data"].as_str().unwrap()), data);
    assert_eq!(Path::new(value["custodian"].as_str().unwrap()), postgres.custodian);
    assert_eq!(Path::new(value["pgBin"].as_str().unwrap()), postgres.bin);
    assert!(value["importer"].as_str().unwrap().ends_with("pgimport.py"));
    assert!(!resources.join("app").join("engine-paths.json.tmp").exists());
    let _ = std::fs::remove_dir_all(&resources);
}

#[test]
fn modo_empaquetado_incompleto_se_rechaza() {
    for missing in ["launch.py", "pg-custodian.exe", "postgresql/bin/initdb.exe", "../tools/pypg/pgimport.py"] {
        let resources = fake_resources("incomplete", Some(missing));
        let error = EngineOptions::packaged(&resources, scratch("incomplete-data")).unwrap_err();
        assert!(matches!(error, EngineError::Config(_)), "{missing}: {error:?}");
        let _ = std::fs::remove_dir_all(&resources);
    }
    assert!(matches!(EngineOptions::packaged(Path::new("relativa"), scratch("x")).unwrap_err(), EngineError::Config(_)));
}

#[test]
fn postgres_y_bootstrap_llegan_al_motor_y_no_se_heredan() {
    with_mode("ready", || {
        // Un bootstrap del entorno (por ejemplo, de la app instalada) nunca se hereda.
        std::env::set_var("ORGTREE_PG_BOOTSTRAP", "1");
        let engine = Engine::start(&fake_options("pg-plain")).expect("ready");
        let (_, body) = engine.get("/api/desktop/identity", Duration::from_secs(5)).unwrap();
        std::env::remove_var("ORGTREE_PG_BOOTSTRAP");
        let value: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert!(value["pg"]["ORGTREE_PG_BOOTSTRAP"].is_null(), "{body}");
        assert_eq!(engine.stop(), StopOutcome::Graceful);

        let resources = fake_resources("pg-runtime", None);
        let mut options = fake_options("pg-packaged");
        options.postgres = Some(PostgresRuntime::locate(&resources.join("engine")).unwrap());
        options.bootstrap_postgres = true;
        // libpq preferiría estas al passfile del custodio: el motor empaquetado no las hereda.
        std::env::set_var("PGPASSWORD", "root");
        std::env::set_var("PGUSER", "postgres");
        let engine = Engine::start(&options).expect("ready");
        std::env::remove_var("PGPASSWORD");
        std::env::remove_var("PGUSER");
        let (_, body) = engine.get("/api/desktop/identity", Duration::from_secs(5)).unwrap();
        let value: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(value["pg"]["ORGTREE_PG_BOOTSTRAP"], "1");
        assert!(value["pg"]["PGPASSWORD"].is_null() && value["pg"]["PGUSER"].is_null(), "{body}");
        let postgres = options.postgres.as_ref().unwrap();
        assert_eq!(Path::new(value["pg"]["ORGTREE_PG_CUSTODIAN"].as_str().unwrap()), postgres.custodian);
        assert_eq!(Path::new(value["pg"]["ORGTREE_P03_PG_BIN"].as_str().unwrap()), postgres.bin);
        assert_eq!(engine.stop(), StopOutcome::Graceful);
        let _ = std::fs::remove_dir_all(&resources);
    });
}

#[test]
fn una_raiz_prohibida_no_se_usa_ni_se_crea() {
    let installed = scratch("installed-orgtree");
    for data in [installed.join("data"), installed.clone()] {
        let mut options = fake_options("forbidden");
        options.data_root = data.clone();
        options.forbidden_roots = vec![installed.clone()];
        assert!(matches!(Engine::start(&options).unwrap_err(), EngineError::Config(_)), "{}", data.display());
        assert!(!data.exists(), "no se crea nada adentro de la raíz prohibida");
    }
    // Una raíz que contiene a la prohibida también se rechaza.
    let mut options = fake_options("forbidden-parent");
    options.forbidden_roots = vec![options.data_root.join("orgtree")];
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

    // Mientras corre, el guardián tiene la raíz (#25: la prueba de la salida).
    #[cfg(windows)]
    assert!(!orgtree_engine_host::root_released(engine.data_root()), "el guardián tiene el candado de la raíz");
    let root = engine.data_root().to_path_buf();
    assert_eq!(
        engine.stop_for_quit(orgtree_engine_host::QUIT_STOP_BUDGET),
        QuitOutcome::Stopped,
        "el motor real responde al apagado autenticado y suelta la raíz"
    );
    assert!(!process_alive(pid), "el proceso del motor terminó");
    assert!(orgtree_engine_host::root_released(&root), "el árbol soltó la raíz");
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
