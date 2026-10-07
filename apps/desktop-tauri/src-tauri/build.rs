// Solo los comandos listados existen para el ACL: cada ventana los recibe
// únicamente si una capability se los concede (ver capabilities/engine-ui.json).
// Los de ventanas por organización (#20) van al final.
const COMMANDS: &[&str] = &[
    "desktop_app_version",
    "desktop_status",
    "desktop_window_state",
    "desktop_window_controls_state",
    "desktop_preferences",
    "desktop_set_preferences",
    "desktop_show",
    "desktop_quit",
    "desktop_window_minimize",
    "desktop_window_toggle_maximize",
    "desktop_window_close",
    "desktop_harnesses",
    "desktop_notify",
    "desktop_sync_notifications",
    "desktop_pending_attention",
    "desktop_open_harness",
    "desktop_reveal_file",
    "desktop_open_charter_folder",
    "desktop_provider_login_start",
    "desktop_provider_login_status",
    "desktop_provider_login_code",
    "desktop_provider_login_cancel",
    "desktop_set_effective_theme",
    "desktop_window_identity",
    "desktop_open_homepage_window",
    "desktop_open_create_window",
    "desktop_cancel_creation",
    "desktop_request_org",
    "desktop_bind_created_org",
    "desktop_set_unsaved_creation",
    "desktop_open_orgs",
    "desktop_take_pending_events",
    // Ciclo de vida del motor (#19): el estado del mantenimiento para las
    // ventanas principales, y Reintentar / Salir para la ventana de arranque.
    "desktop_maintenance_status",
    "splash_retry",
    "splash_quit",
];

fn main() {
    tauri_build::try_build(
        tauri_build::Attributes::new().app_manifest(tauri_build::AppManifest::new().commands(COMMANDS)),
    )
    .expect("failed to run tauri-build");
}
