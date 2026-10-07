//! `startAtLogin` (#20): la entrada de la app en el inicio de Windows, como
//! `app.setLoginItemSettings({ openAtLogin, args: ['--background'] })` de Electron.
//!
//! Se usa la clave `Run` del usuario (`HKCU\Software\Microsoft\Windows\CurrentVersion\Run`),
//! nunca la de la máquina (HKLM): la app se instala por usuario y no pide
//! elevación. El valor arranca el ejecutable actual con `--background`, que
//! abre las ventanas ocultas y deja la app en la bandeja.

#![cfg_attr(not(windows), allow(dead_code))]

/// El nombre del valor en `Run`: el identificador de la app, propio del spike.
pub const VALUE_NAME: &str = "com.kushro.orgtree.tauri-spike";
pub const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";

/// La línea de comandos que queda registrada.
pub fn command_line(exe: &std::path::Path) -> String {
    format!("\"{}\" --background", exe.display())
}

/// Deja el registro como dice la preferencia. Devuelve el valor registrado
/// (o `None` si no hay entrada).
#[cfg(windows)]
pub fn apply(enabled: bool) -> Result<Option<String>, String> {
    use winreg::enums::HKEY_CURRENT_USER;
    use winreg::RegKey;
    // `create_subkey` abre la clave con escritura (y la crea si falta).
    let (run, _) = RegKey::predef(HKEY_CURRENT_USER).create_subkey(RUN_KEY).map_err(|e| format!("Run: {e}"))?;
    if enabled {
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        let exe = std::path::PathBuf::from(exe.to_string_lossy().trim_start_matches(r"\\?\"));
        let line = command_line(&exe);
        run.set_value(VALUE_NAME, &line).map_err(|e| format!("Run: {e}"))?;
        Ok(Some(line))
    } else {
        match run.delete_value(VALUE_NAME) {
            Ok(()) => Ok(None),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(format!("Run: {e}")),
        }
    }
}

#[cfg(not(windows))]
pub fn apply(_enabled: bool) -> Result<Option<String>, String> {
    // Fuera de Windows el spike no registra nada (Linux no es destino).
    Ok(None)
}

/// Lo que hay registrado ahora (para la prueba).
#[cfg(windows)]
pub fn current() -> Option<String> {
    use winreg::enums::HKEY_CURRENT_USER;
    use winreg::RegKey;
    RegKey::predef(HKEY_CURRENT_USER).open_subkey(RUN_KEY).ok()?.get_value::<String, _>(VALUE_NAME).ok()
}

#[cfg(not(windows))]
pub fn current() -> Option<String> {
    None
}
