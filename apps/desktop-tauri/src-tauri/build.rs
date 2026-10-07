// Solo los comandos listados existen para el ACL: cada ventana los recibe
// únicamente si una capability se los concede (ver capabilities/engine-ui.json).
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
];

fn main() {
    tauri_build::try_build(
        tauri_build::Attributes::new().app_manifest(tauri_build::AppManifest::new().commands(COMMANDS)),
    )
    .expect("failed to run tauri-build");
}
