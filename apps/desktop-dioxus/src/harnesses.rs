//! Harnesses de agentes (#30), como `apps/desktop/main/harnesses.ts`. Es el
//! mismo código que el spike de Tauri (#21): no depende del framework.
//!
//! Solo presencia: nunca lanza, instala ni contacta a un proveedor. Busca el
//! CLI en el `PATH` del proceso y en las ubicaciones conocidas
//! (`ORGTREE_CODEX`, `ORGTREE_ANTIGRAVITY` o el instalador de Antigravity).
//!
//! Qué proveedores se pueden contratar lo decide `/api/providers` del motor,
//! que hereda el mismo `PATH`. Esta lista la usa el shell: el login de
//! proveedores (solo corre un CLI detectado acá) y el primer uso, que explica
//! qué instalar con los enlaces oficiales.

use std::path::{Path, PathBuf};

/// Los harnesses del contrato, con su CLI, su nombre y el enlace oficial de
/// instalación (`HARNESS_LINKS` de `apps/desktop/main/policy.ts`). La URL es
/// fija: nunca viene de la página.
pub const HARNESSES: [(&str, &str, &str, &str); 3] = [
    ("claude", "claude", "Claude Code", "https://code.claude.com/docs/en/setup"),
    ("codex", "codex", "Codex", "https://developers.openai.com/codex/cli"),
    ("antigravity", "agy", "Antigravity", "https://antigravity.google/download"),
];

#[derive(Clone, Debug, PartialEq)]
pub struct Harness {
    pub id: &'static str,
    pub exe: &'static str,
    pub label: &'static str,
    pub url: &'static str,
    /// La ruta absoluta del CLI encontrado, si hay uno. No sale del shell.
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
        .map(|&(id, exe, label, url)| Harness {
            id,
            exe,
            label,
            url,
            path: known_location(id).filter(|p| is_file(p)).or_else(|| on_path(&search_path, exe)),
        })
        .collect()
}

/// El enlace oficial de un harness del contrato; cualquier otro id se rechaza
/// (`desktop:open-harness`).
pub fn link(id: &str) -> Option<&'static str> {
    HARNESSES.iter().find(|(known, ..)| *known == id).map(|&(.., url)| url)
}

/// El harness de un proveedor del motor (`claude`, `openai`, `google`).
pub fn of_provider(provider: &str) -> Option<&'static str> {
    match provider {
        "claude" => Some("claude"),
        "openai" => Some("codex"),
        "google" => Some("antigravity"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encuentra_un_cli_en_el_path_y_no_uno_ausente() {
        let dir = std::env::temp_dir().join(format!("orgtree-dioxus-harness-test-{}", std::process::id()));
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
        assert_eq!(of_provider("openai"), Some("codex"));
        assert_eq!(of_provider("openrouter"), None);
    }
}
