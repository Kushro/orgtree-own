//! Harnesses de agentes (#21), como `apps/desktop/main/harnesses.ts`.
//!
//! Solo presencia: nunca lanza, instala ni contacta a un proveedor. Busca el
//! CLI en el `PATH` del proceso y en las ubicaciones conocidas
//! (`ORGTREE_CODEX`, `ORGTREE_ANTIGRAVITY` o el instalador de Antigravity).
//!
//! El renderer decide qué proveedores se pueden contratar con
//! `/api/providers` del motor, que hereda el mismo `PATH`; esta lista la usa
//! el shell (menú de la bandeja, login de proveedores) y el puente
//! (`getHarnesses`).

use serde_json::{json, Value};
use std::path::{Path, PathBuf};

/// Los harnesses del contrato, con su CLI y el enlace oficial de instalación
/// (`HARNESS_LINKS` de `apps/desktop/main/policy.ts`). La URL es fija: nunca
/// viene de la página.
pub const HARNESSES: [(&str, &str, &str); 3] = [
    ("claude", "claude", "https://code.claude.com/docs/en/setup"),
    ("codex", "codex", "https://developers.openai.com/codex/cli"),
    ("antigravity", "agy", "https://antigravity.google/download"),
];

pub struct Harness {
    pub id: &'static str,
    pub url: &'static str,
    /// La ruta absoluta del CLI encontrado, si hay uno.
    pub path: Option<PathBuf>,
}

impl Harness {
    pub fn detected(&self) -> bool {
        self.path.is_some()
    }
}

fn is_file(path: &Path) -> bool {
    path.is_absolute() && std::fs::metadata(path).map(|m| m.is_file()).unwrap_or(false)
}

fn env_path(name: &str) -> Option<PathBuf> {
    std::env::var_os(name).filter(|v| !v.is_empty()).map(PathBuf::from)
}

/// La ubicación conocida de cada CLI fuera del `PATH`, como `knownLocations`.
fn known_location(id: &str) -> Option<PathBuf> {
    match id {
        "codex" => env_path("ORGTREE_CODEX"),
        "antigravity" => env_path("ORGTREE_ANTIGRAVITY").or_else(|| {
            if cfg!(windows) {
                let local = env_path("LOCALAPPDATA")
                    .or_else(|| env_path("USERPROFILE").map(|home| home.join("AppData").join("Local")))?;
                Some(local.join("agy").join("bin").join("agy.exe"))
            } else {
                env_path("HOME").map(|home| home.join(".local").join("bin").join("agy"))
            }
        }),
        _ => None,
    }
}

/// Busca `exe` en las carpetas de `search_path`, con las mismas extensiones
/// que Electron (`.exe`, `.cmd`, `.bat` y sin extensión en Windows).
fn on_path(search_path: &std::ffi::OsStr, exe: &str) -> Option<PathBuf> {
    let extensions: &[&str] = if cfg!(windows) { &[".exe", ".cmd", ".bat", ""] } else { &[""] };
    std::env::split_paths(search_path)
        .filter(|dir| !dir.as_os_str().is_empty())
        .map(|dir| PathBuf::from(dir.to_string_lossy().trim_matches('"')))
        .find_map(|dir| extensions.iter().map(|ext| dir.join(format!("{exe}{ext}"))).find(|file| is_file(file)))
}

/// `detectHarnesses()` sobre el `PATH` del proceso.
pub fn detect() -> Vec<Harness> {
    let search_path = std::env::var_os("PATH").unwrap_or_default();
    HARNESSES
        .iter()
        .map(|&(id, exe, url)| Harness {
            id,
            url,
            path: known_location(id).filter(|p| is_file(p)).or_else(|| on_path(&search_path, exe)),
        })
        .collect()
}

/// La forma del contrato: `{ id, detected, url }[]`. La ruta no sale del shell.
pub fn as_json(harnesses: &[Harness]) -> Value {
    harnesses.iter().map(|h| json!({ "id": h.id, "detected": h.detected(), "url": h.url })).collect()
}

/// El enlace oficial de un harness del contrato; cualquier otro id se rechaza.
pub fn link(id: &str) -> Option<&'static str> {
    HARNESSES.iter().find(|(known, _, _)| *known == id).map(|&(_, _, url)| url)
}

/// Abre una URL fija en el navegador del sistema (`shell.openExternal`).
/// Solo se llama con los enlaces de `HARNESSES`.
#[cfg(windows)]
pub fn open_external(url: &'static str) -> Result<(), String> {
    use std::ffi::c_void;
    #[link(name = "shell32")]
    extern "system" {
        fn ShellExecuteW(hwnd: *mut c_void, op: *const u16, file: *const u16, params: *const u16, dir: *const u16, show: i32) -> isize;
    }
    let wide = |s: &str| s.encode_utf16().chain([0]).collect::<Vec<u16>>();
    let (op, file) = (wide("open"), wide(url));
    // SAFETY: cadenas UTF-16 terminadas en NUL que viven durante la llamada.
    let result = unsafe { ShellExecuteW(std::ptr::null_mut(), op.as_ptr(), file.as_ptr(), std::ptr::null(), std::ptr::null(), 1) };
    // ShellExecuteW devuelve un valor mayor que 32 si tuvo éxito.
    if result > 32 { Ok(()) } else { Err(format!("no se pudo abrir el navegador ({result})")) }
}

#[cfg(not(windows))]
pub fn open_external(url: &'static str) -> Result<(), String> {
    std::process::Command::new("xdg-open").arg(url).spawn().map(|_| ()).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encuentra_un_cli_en_el_path_y_no_uno_ausente() {
        let dir = std::env::temp_dir().join(format!("orgtree-harness-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let name = if cfg!(windows) { "codex.cmd" } else { "codex" };
        std::fs::write(dir.join(name), b"").unwrap();
        let path = std::env::join_paths([dir.clone()]).unwrap();
        assert_eq!(on_path(&path, "codex"), Some(dir.join(name)));
        assert_eq!(on_path(&path, "claude"), None);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn solo_enlaces_fijos() {
        assert_eq!(link("codex"), Some("https://developers.openai.com/codex/cli"));
        assert_eq!(link("https://example.com"), None);
    }
}
