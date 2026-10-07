//! Enlaces externos (#30): la única vía por la que la app abre el navegador.
//!
//! Electron manda los enlaces `http(s)` del contenido de los agentes al
//! navegador del sistema (`routeExternal` y `setWindowOpenHandler` en
//! `apps/desktop/main/windows.ts`, con `externalHttpUrl` de `policy.ts`, y
//! `shell.openExternal`); el webview nunca navega. Acá es igual, pero desde
//! Rust: la página manda el `href` y esta función decide.
//!
//! - Solo `http` y `https`, con un host, sin usuario ni contraseña en la URL
//!   (como `externalHttpUrl`), y nunca el origen del motor (el token es suyo).
//! - Se abre la URL normalizada por el parser, nunca el texto crudo: una ruta
//!   relativa, `file:`, `javascript:` o un esquema raro no llegan a
//!   `ShellExecuteW` ni a `xdg-open`. Las rutas a archivos tienen su propia
//!   vía (`reveal`: revelar, nunca abrir).
//! - Dioxus manda cualquier `<a>` clicado a `webbrowser::open`; las vistas
//!   cortan el clic en la captura y llaman a `open`.

use std::sync::Mutex;

/// Lo que se abrió o se rechazó, para la prueba (`ORGTREE_DIOXUS_PROBE`).
static LOG: Mutex<Vec<serde_json::Value>> = Mutex::new(Vec::new());

pub fn log_snapshot() -> Vec<serde_json::Value> {
    LOG.lock().unwrap().clone()
}

/// `externalHttpUrl`: la URL normalizada si se puede abrir, o por qué no.
pub fn validate(href: &str) -> Result<String, String> {
    let href = href.trim();
    let url = url::Url::parse(href).map_err(|_| format!("Not a web link: {href}"))?;
    if url.scheme() != "http" && url.scheme() != "https" {
        return Err(format!("Only http and https links open in the browser: {href}"));
    }
    if url.host_str().is_none_or(str::is_empty) {
        return Err(format!("Not a web link: {href}"));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(format!("Links with a user name or password do not open: {href}"));
    }
    if is_engine(&url) {
        return Err(format!("Engine addresses do not open in the browser: {href}"));
    }
    Ok(url.to_string())
}

/// El motor escucha en `127.0.0.1:<puerto>`: una URL suya no se manda al
/// navegador (como `trustedUiUrl` en Electron).
fn is_engine(url: &url::Url) -> bool {
    let Some(origin) = crate::ENGINE.lock().ok().and_then(|e| e.as_ref().map(|e| e.origin())) else { return false };
    url::Url::parse(&origin).is_ok_and(|engine| engine.origin() == url.origin())
}

/// Valida y abre en el navegador del sistema. Devuelve la URL abierta.
pub fn open(href: &str) -> Result<String, String> {
    let result = validate(href).and_then(|url| launch(&url).map(|_| url));
    let entry = match &result {
        Ok(url) => serde_json::json!({ "href": href, "opened": url }),
        Err(why) => serde_json::json!({ "href": href, "refused": why }),
    };
    LOG.lock().unwrap().push(entry);
    result
}

#[cfg(windows)]
fn launch(url: &str) -> Result<(), String> {
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
    if result > 32 {
        Ok(())
    } else {
        Err(format!("The browser could not be opened ({result})"))
    }
}

#[cfg(not(windows))]
fn launch(url: &str) -> Result<(), String> {
    // En la prueba local (Linux, sin navegador) solo se registra.
    if std::env::var_os("ORGTREE_DIOXUS_PROBE").is_some() {
        return Ok(());
    }
    std::process::Command::new("xdg-open").arg(url).spawn().map(|_| ()).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn solo_http_y_https() {
        assert_eq!(validate("https://example.com/orgtree").as_deref(), Ok("https://example.com/orgtree"));
        assert_eq!(validate(" http://example.com ").as_deref(), Ok("http://example.com/"));
        for bad in [
            "uploads/informe.txt",
            "file:///C:/Windows/System32/calc.exe",
            "javascript:alert(1)",
            "ssh://example.com/x",
            "mailto:a@example.com",
            "C:\\Windows\\notepad.exe",
            "\\\\server\\share\\x.exe",
            "https://usuario:clave@example.com/",
            "https://usuario@example.com/",
            "",
        ] {
            assert!(validate(bad).is_err(), "{bad} no tiene que abrirse");
        }
    }
}
