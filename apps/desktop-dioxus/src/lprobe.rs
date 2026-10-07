//! Prueba del ciclo de vida del motor (#25), solo con
//! `ORGTREE_DIOXUS_PROBE=<archivo>` y `ORGTREE_DIOXUS_PROBE_MODE=lifecycle`.
//! Corre en la ventana principal desde el arranque (antes de que el motor esté
//! listo) y maneja la app compilada sobre el motor de fixture, como el
//! `lprobe` del spike de Tauri (#19). `ORGTREE_DIOXUS_LIFECYCLE_RUN` elige:
//!
//! - `full`: el CI lanza antes otro motor sobre la misma raíz, así que el
//!   arranque se rechaza (`root-owned`) con Reintentar; el CI suelta la raíz y
//!   la prueba toca el botón real. Sigue la conversión simulada
//!   (`ORGTREE_FIXTURE_CONVERT`), `ready`, una caída (el árbol del motor se
//!   mata desde afuera) con su aviso y la vuelta con otro PID, un pedido de
//!   mantenimiento `restart` (otro PID) y uno `update` (reportado
//!   `unavailable`), y sale por el camino de "Salir" de la bandeja;
//! - `close`: con `exitOnClose` prendido, cierra la última ventana con su botón;
//! - `session`: espera el fin de sesión simulado por el CI
//!   (`WM_QUERYENDSESSION` y `WM_ENDSESSION`).
//!
//! Cada etapa deja un marcador (`<archivo>.<nombre>`) para las capturas del CI.

use crate::lifecycle::{self, Splash};
use crate::probe::{marker, record, report_path, wait_for_ci};
use dioxus::prelude::*;
use serde_json::{json, Value};
use std::time::Duration;

fn active() -> bool {
    report_path().is_some() && crate::probe::mode().as_deref() == Some("lifecycle")
}

fn run() -> String {
    std::env::var("ORGTREE_DIOXUS_LIFECYCLE_RUN").unwrap_or_else(|_| "full".into())
}

/// Espera hasta que `test` dé algo, o `None` pasado el plazo.
async fn until<T>(seconds: u64, mut test: impl FnMut() -> Option<T>) -> Option<T> {
    let end = std::time::Instant::now() + Duration::from_secs(seconds);
    loop {
        if let Some(value) = test() {
            return Some(value);
        }
        if std::time::Instant::now() >= end {
            return None;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

/// Corre un script en la página de la ventana principal y devuelve lo que retorna.
async fn page(script: &str) -> Value {
    match document::eval(script).join::<Value>().await {
        Ok(value) => value,
        Err(error) => json!({ "error": error.to_string() }),
    }
}

/// Lo que la ventana principal muestra: si lista la org del fixture, el aviso
/// del motor y el del mantenimiento. Espera hasta `wait_ms` a que la org esté
/// listada y, con `clear`, a que no haya aviso.
async fn home_state(wait_ms: u64, clear: bool) -> Value {
    page(&format!(
        r#"
const end = Date.now() + {wait_ms};
const state = () => ({{
  listed: [...document.querySelectorAll('.welcome-card nav .org')].some(el => el.textContent.includes('spike-fixture')),
  notice: (document.querySelector('.dx-engine-notice') || {{}}).textContent || null,
  maintenance: (document.querySelector('.dx-maintenance-notice') || {{}}).textContent || null,
  splash: !!document.querySelector('.dx-splash'),
}});
let s = state();
while (Date.now() < end && (!s.listed || ({clear} && s.notice))) {{ await new Promise(r => setTimeout(r, 200)); s = state() }}
return s;
"#
    ))
    .await
}

/// Mata el árbol del motor desde afuera, como una caída.
fn kill_tree(pid: u32) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        std::process::Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/T", "/F"])
            .creation_flags(0x0800_0000)
            .status()
            .is_ok_and(|s| s.success())
    }
    #[cfg(not(windows))]
    {
        std::process::Command::new("kill").args(["-9", &pid.to_string()]).status().is_ok_and(|s| s.success())
    }
}

/// El director de la prueba: se monta en la ventana principal desde el arranque.
#[component]
pub fn LifecycleProbe() -> Element {
    use_future(|| async move {
        if !active() {
            return;
        }
        match run().as_str() {
            "close" => close_run().await,
            "session" => session_run().await,
            _ => full_run().await,
        }
    });
    rsx! {}
}

async fn wait_ready(seconds: u64) -> Option<u32> {
    until(seconds, || (lifecycle::is_ready() && !lifecycle::is_restarting() && lifecycle::current_client().is_some()).then(lifecycle::pid).flatten()).await
}

async fn full_run() {
    // 1. El rechazo: otro motor tiene la raíz.
    let failed = until(240, || matches!(lifecycle::splash(), Splash::Failed { .. }).then_some(())).await.is_some();
    record("refused", json!({ "failed": failed, "splash": lifecycle::splash().json(), "status": lifecycle::status().json() }));
    marker("refused");
    let released = wait_for_ci("refused").await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    let clicked = page("const b = document.querySelector('.dx-splash-retry'); if (b) b.click(); return !!b;").await;
    record("retry", json!({ "ciReleased": released, "clicked": clicked }));

    // 2. La conversión simulada.
    let converting = until(90, || matches!(lifecycle::splash(), Splash::Converting { .. }).then(lifecycle::splash)).await;
    if converting.is_some() {
        marker("converting");
    }
    record("converting", json!({ "seen": converting.as_ref().map(Splash::json) }));

    // 3. Listo.
    let Some(pid) = wait_ready(300).await else {
        record("error", json!(format!("el motor no llegó a ready: {:?}", lifecycle::status())));
        return crate::orgwindows::quit("probe");
    };
    let home = home_state(30_000, false).await;
    record("ready", json!({ "pid": pid, "status": lifecycle::status().json(), "maintenance": lifecycle::last_maintenance(), "page": home }));
    marker("ready");
    tokio::time::sleep(Duration::from_millis(1500)).await;

    // 4. Una caída: el árbol del motor muere de golpe.
    let killed = kill_tree(pid);
    let stopped = until(30, || matches!(lifecycle::status().state, "stopped" | "unavailable").then_some(())).await.is_some();
    tokio::time::sleep(Duration::from_millis(700)).await;
    let down = page("return { notice: (document.querySelector('.dx-engine-notice') || {}).textContent || null, state: (document.querySelector('.dx-engine-notice') || { dataset: {} }).dataset.state || null };").await;
    record("down", json!({ "killed": killed, "stopped": stopped, "status": lifecycle::status().json(), "page": down }));
    marker("engine-down");
    tokio::time::sleep(Duration::from_millis(1500)).await;

    // 5. La vuelta: otro motor, otro cliente, la vista montada de nuevo.
    let back = until(180, || {
        (lifecycle::is_ready() && !lifecycle::is_restarting()).then(lifecycle::pid).flatten().filter(|p| *p != pid)
    })
    .await;
    let home = home_state(30_000, true).await;
    record("recovered", json!({ "back": back.is_some(), "pid": back, "generation": lifecycle::generation(), "status": lifecycle::status().json(), "page": home }));
    marker("recovered");
    tokio::time::sleep(Duration::from_millis(1500)).await;

    // 6. Mantenimiento `restart`, pedido como lo haría un agente.
    let before = lifecycle::pid();
    let request = match lifecycle::current_client() {
        Some(client) => match client.post::<Value>("/api/fixture/maintenance", &json!({ "action": "restart" })).await {
            Ok(value) => json!({ "status": 200, "body": value }),
            Err(error) => json!({ "error": error.to_string() }),
        },
        None => json!({ "error": "sin cliente" }),
    };
    let after = until(240, || {
        (lifecycle::is_ready() && !lifecycle::is_restarting()).then(lifecycle::pid).flatten().filter(|p| Some(*p) != before)
    })
    .await;
    record("maintenanceRestart", json!({ "request": request, "pidBefore": before, "pidAfter": after, "restarted": after.is_some() }));
    let _ = home_state(30_000, true).await;

    // 7. Mantenimiento `update`: sin updater, se reporta `unavailable`.
    let request = match lifecycle::current_client() {
        Some(client) => match client.post::<Value>("/api/fixture/maintenance", &json!({ "action": "update" })).await {
            Ok(value) => json!({ "status": 200, "body": value }),
            Err(error) => json!({ "error": error.to_string() }),
        },
        None => json!({ "error": "sin cliente" }),
    };
    let reported = until(150, || (lifecycle::last_maintenance().as_deref() == Some("unavailable")).then_some(())).await.is_some();
    tokio::time::sleep(Duration::from_millis(500)).await;
    let shown = home_state(5_000, false).await;
    record("maintenanceUpdate", json!({ "request": request, "reported": reported, "state": lifecycle::last_maintenance(), "page": shown }));
    marker("maintenance");
    let _ = wait_for_ci("maintenance").await;
    record("done", json!(true));
    // Salir por el mismo camino que "Salir" de la bandeja.
    crate::native::quit();
}

async fn close_run() {
    let pid = wait_ready(300).await;
    let home = home_state(30_000, false).await;
    let exit_on_close = crate::notify::prefs().get("exitOnClose").cloned();
    record("ready", json!({ "pid": pid, "status": lifecycle::status().json(), "exitOnClose": exit_on_close, "page": home }));
    marker("close-ready");
    let _ = wait_for_ci("close-ready").await;
    // El botón real de cerrar de la única ventana: con exitOnClose, la app sale.
    let clicked = page("const b = document.querySelector('.welcome-card .window-control.close'); if (b) setTimeout(() => b.click(), 150); return !!b;").await;
    record("closeClicked", clicked);
}

async fn session_run() {
    let pid = wait_ready(300).await;
    let home = home_state(30_000, false).await;
    #[cfg(windows)]
    let hwnd = {
        use dioxus::desktop::tao::platform::windows::WindowExtWindows;
        Some(dioxus::desktop::window().window.hwnd() as isize)
    };
    #[cfg(not(windows))]
    let hwnd: Option<isize> = None;
    record("ready", json!({ "pid": pid, "status": lifecycle::status().json(), "hwnd": hwnd, "page": home }));
    // El CI anota el árbol de procesos y manda el fin de sesión a todas las ventanas.
    marker("session-ready");
}
