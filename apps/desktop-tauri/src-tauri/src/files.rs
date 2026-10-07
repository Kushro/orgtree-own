//! Archivos (#21): `revealFile` y `openCharterFolder`, como los handlers
//! `desktop:reveal-file` y `desktop:open-charter-folder` de Electron.
//!
//! `revealFile` **selecciona** el archivo en el Explorador (`explorer
//! /select,`) y nunca lo abre ni lo ejecuta (decisión del usuario,
//! 2026-09-13): las rutas vienen de Markdown escrito por agentes, y abrirlas
//! haría de cualquier link un clic para correr un programa.

use serde_json::{json, Value};
use std::path::{Component, Path, PathBuf};

/// La ruta validada: absoluta, sin `..`, y que exista.
pub fn reveal_target(value: &str) -> Result<PathBuf, String> {
    if value.is_empty() {
        return Err("No path given".into());
    }
    let path = Path::new(value);
    // Absoluta de verdad: en Windows, `\x` o `C:x` no lo son (`Path::is_absolute`).
    if !path.is_absolute() {
        return Err(format!("Not an absolute path: {value}"));
    }
    // Sin `..` ni `.`: `path.normalize` de Electron los resuelve; acá se rechazan.
    if path.components().any(|c| matches!(c, Component::ParentDir | Component::CurDir)) {
        return Err(format!("Not a normalized path: {value}"));
    }
    // Las comillas no son válidas en rutas de Windows, y romperían el argumento de explorer.
    if value.contains('"') || value.contains('\0') {
        return Err(format!("Invalid path: {value}"));
    }
    // Un archivo que no está: avisar en lugar de no hacer nada (un link muerto).
    if !path.exists() {
        return Err(format!("No such file: {value}"));
    }
    Ok(path.to_path_buf())
}

/// `explorer /select,"<ruta>"`: abre la carpeta con el archivo seleccionado.
#[cfg(windows)]
fn explorer_select(path: &Path) -> Result<(), String> {
    use std::os::windows::process::CommandExt;
    // El argumento tiene que llegar como `/select,"C:\a b\c.txt"`: explorer no
    // entiende la forma con todo el argumento entre comillas que arma Rust.
    std::process::Command::new("explorer.exe")
        .raw_arg(format!("/select,\"{}\"", path.display()))
        .spawn()
        .map(|_| ())
        .map_err(|e| e.to_string())
}

#[cfg(not(windows))]
fn explorer_select(path: &Path) -> Result<(), String> {
    let dir = path.parent().unwrap_or(path);
    std::process::Command::new("xdg-open").arg(dir).spawn().map(|_| ()).map_err(|e| e.to_string())
}

pub fn reveal_file(value: &str) -> Value {
    match reveal_target(value).and_then(|path| explorer_select(&path)) {
        Ok(()) => json!({ "ok": true }),
        Err(error) => json!({ "ok": false, "error": error }),
    }
}

/// Abre una carpeta en el Explorador (`shell.openPath` sobre una carpeta).
#[cfg(windows)]
fn open_folder(dir: &Path) -> Result<(), String> {
    use std::os::windows::process::CommandExt;
    std::process::Command::new("explorer.exe")
        .raw_arg(format!("\"{}\"", dir.display()))
        .spawn()
        .map(|_| ())
        .map_err(|e| e.to_string())
}

#[cfg(not(windows))]
fn open_folder(dir: &Path) -> Result<(), String> {
    std::process::Command::new("xdg-open").arg(dir).spawn().map(|_| ()).map_err(|e| e.to_string())
}

/// `~/.orgtree/charters`, como `path.join(os.homedir(), '.orgtree', 'charters')`.
pub fn charter_dir() -> Option<PathBuf> {
    let home = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).filter(|v| !v.is_empty())?;
    Some(PathBuf::from(home).join(".orgtree").join("charters"))
}

/// `openCharterFolder`: crea la carpeta si falta y la abre.
pub fn open_charter_folder() -> Value {
    let Some(dir) = charter_dir() else {
        return json!({ "ok": false, "error": "Could not find the home folder" });
    };
    if dir.exists() {
        if !dir.is_dir() {
            return json!({ "ok": false, "error": format!("Charter path exists but is not a directory: {}", dir.display()) });
        }
    } else if let Err(error) = std::fs::create_dir_all(&dir) {
        return json!({ "ok": false, "error": format!("Could not create charter directory: {error}") });
    }
    if dir.to_string_lossy().contains('"') {
        return json!({ "ok": false, "error": "Could not open charter directory: invalid path" });
    }
    match open_folder(&dir) {
        Ok(()) => json!({ "ok": true, "path": dir }),
        Err(error) => json!({ "ok": false, "error": format!("Could not open charter directory: {error}") }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rechaza_relativas_inexistentes_y_con_puntos() {
        assert!(reveal_target("").is_err());
        assert!(reveal_target("relativo.txt").unwrap_err().starts_with("Not an absolute path"));
        let missing = std::env::temp_dir().join("orgtree-no-existe-21.txt");
        assert!(reveal_target(&missing.to_string_lossy()).unwrap_err().starts_with("No such file"));
        let dotted = std::env::temp_dir().join("..").join("x");
        assert!(reveal_target(&dotted.to_string_lossy()).unwrap_err().starts_with("Not a normalized path"));
        let file = std::env::temp_dir().join(format!("orgtree-reveal-{}.txt", std::process::id()));
        std::fs::write(&file, b"x").unwrap();
        assert_eq!(reveal_target(&file.to_string_lossy()).unwrap(), file);
        std::fs::remove_file(file).unwrap();
    }
}
