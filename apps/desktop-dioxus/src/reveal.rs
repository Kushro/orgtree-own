//! Enlaces a archivos locales (#27): un clic **revela** el archivo en el
//! administrador de archivos y nunca lo abre ni lo ejecuta.
//!
//! Es la regla del producto (user ruling 2026-09-13, `revealFileFromEvent` en
//! `canvas/shared.ts` y `desktop:reveal-file` en `main/index.ts`): las rutas
//! vienen de texto que escribió un agente, así que abrirlas como un doble clic
//! convertiría cualquier enlace del chat en una forma de correr un programa.
//! Revelar no puede: la persona ve el archivo en su carpeta y decide.
//!
//! Como en Electron, la ruta tiene que ser absoluta (una relativa no tiene una
//! base con sentido) y existir (en Windows, revelar algo que no está no hace
//! nada y parece un enlace roto). Si no se puede, el desk muestra la ruta como
//! texto.

use std::path::{Path, PathBuf};

/// `winFileHref` + `winFilePath` de `canvas/shared.ts`: el destino de un
/// enlace Markdown que nombra un archivo de Windows (`C:\…`, `C:/…` o
/// `/C:/…`), como ruta nativa. Fuera de Windows también acepta una ruta POSIX
/// absoluta, para desarrollar el shell en Linux.
pub fn local_path(dest: &str) -> Option<String> {
    let trimmed = dest.strip_prefix('/').unwrap_or(dest);
    let bytes = trimmed.as_bytes();
    if bytes.len() >= 3 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' && (bytes[2] == b'\\' || bytes[2] == b'/') {
        return Some(format!("{}:\\{}", &trimmed[..1], trimmed[3..].replace('/', "\\")));
    }
    if cfg!(not(windows)) && dest.starts_with('/') && !dest.starts_with("//") {
        return Some(dest.to_string());
    }
    None
}

/// `escapeProse` de `canvas/shared.ts`: antes de convertir el Markdown, el
/// destino de un enlace a un archivo de Windows pasa a barras normales
/// (`](<C:\a b\c.exe>)` → `](</C:/a b/c.exe>)`), porque CommonMark toma
/// `\_`, `\[` y compañía como escapes y la ruta llegaría cambiada. Las
/// líneas dentro de un bloque de código quedan como están.
pub fn normalize_links(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut fence = false;
    for line in text.split_inclusive('\n') {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            fence = !fence;
            out.push_str(line);
            continue;
        }
        if fence || !line.contains("](") {
            out.push_str(line);
            continue;
        }
        let mut rest = line;
        while let Some(at) = rest.find("](") {
            out.push_str(&rest[..at + 2]);
            rest = &rest[at + 2..];
            let (dest, tail) = match rest.strip_prefix('<') {
                Some(inner) => match inner.find(['>', '\n']) {
                    Some(end) if inner[end..].starts_with('>') => {
                        out.push('<');
                        (&inner[..end], &inner[end..])
                    }
                    _ => ("", rest),
                },
                None => {
                    let end = rest.find(|c: char| c == ')' || c.is_whitespace() || c == '<' || c == '(').unwrap_or(rest.len());
                    (&rest[..end], &rest[end..])
                }
            };
            match local_path(dest).filter(|_| dest.contains('\\')) {
                Some(_) => out.push_str(&format!("/{}", dest.trim_start_matches('/').replace('\\', "/"))),
                None => out.push_str(dest),
            }
            rest = tail;
        }
        out.push_str(rest);
    }
    out
}

/// Valida una ruta para revelarla: absoluta y existente. Los textos son los de
/// `desktop:reveal-file` en Electron.
pub fn check(raw: &str) -> Result<PathBuf, String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err("No path given".into());
    }
    let path = Path::new(raw);
    if !path.is_absolute() {
        return Err(format!("Not an absolute path: {raw}"));
    }
    // Windows no admite comillas en un nombre: así la ruta no puede cerrar el
    // argumento de `explorer` y agregar otro.
    if raw.contains('"') || !path.exists() {
        return Err(format!("No such file: {}", path.display()));
    }
    Ok(path.to_path_buf())
}

/// Revela el archivo: valida la ruta y abre su carpeta con el archivo
/// seleccionado (`explorer /select,`). Nunca lo abre.
pub fn reveal(raw: &str) -> Result<PathBuf, String> {
    let path = check(raw)?;
    show_in_folder(&path)?;
    Ok(path)
}

#[cfg(windows)]
fn show_in_folder(path: &Path) -> Result<(), String> {
    use std::os::windows::process::CommandExt;
    let native = path.display().to_string().replace('/', "\\");
    std::process::Command::new("explorer.exe")
        .raw_arg(format!("/select,\"{native}\""))
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("Could not show {native}: {e}"))
}

/// Fuera de Windows (solo desarrollo del shell) no hay un "seleccionar en la
/// carpeta" común a todos los escritorios: la validación es la misma y no se
/// lanza nada.
#[cfg(not(windows))]
fn show_in_folder(_path: &Path) -> Result<(), String> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rutas_de_windows_en_enlaces() {
        assert_eq!(local_path(r"C:\dir\f.exe").as_deref(), Some(r"C:\dir\f.exe"));
        assert_eq!(local_path("C:/a b/c.exe").as_deref(), Some(r"C:\a b\c.exe"));
        assert_eq!(local_path("/d:/x/y.txt").as_deref(), Some(r"d:\x\y.txt"));
        assert_eq!(local_path("https://example.com/x"), None);
        assert_eq!(local_path("uploads/informe.txt"), None);
        assert_eq!(local_path("//servidor/x"), None);
    }

    #[test]
    fn los_escapes_de_commonmark_no_cambian_la_ruta() {
        assert_eq!(normalize_links(r"ver [log](<D:\a\_temp\x y\f.txt>) ya"), "ver [log](</D:/a/_temp/x y/f.txt>) ya");
        assert_eq!(normalize_links(r"[f](C:\dir\a_b.exe) y [w](https://x.y/a\b)"), r"[f](/C:/dir/a_b.exe) y [w](https://x.y/a\b)");
        assert_eq!(normalize_links("```\n[f](C:\\x)\n```\n"), "```\n[f](C:\\x)\n```\n");
        assert_eq!(local_path("/D:/a/_temp/x y/f.txt").as_deref(), Some(r"D:\a\_temp\x y\f.txt"));
    }

    #[test]
    fn rechaza_relativas_e_inexistentes() {
        assert_eq!(check("uploads/informe.txt").unwrap_err(), "Not an absolute path: uploads/informe.txt");
        assert_eq!(check("  ").unwrap_err(), "No path given");
        let missing = std::env::temp_dir().join("orgtree-dioxus-no-existe").join("falta.log");
        assert!(check(&missing.display().to_string()).unwrap_err().starts_with("No such file"));
        let quoted = format!("{}\" /x", std::env::temp_dir().display());
        assert!(check(&quoted).is_err());
        let here = std::env::current_exe().unwrap();
        assert_eq!(check(&here.display().to_string()).unwrap(), here);
    }
}
