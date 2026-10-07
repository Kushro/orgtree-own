//! `startAtLogin` (#25): la entrada de la app en el inicio de Windows, como
//! `app.setLoginItemSettings({ openAtLogin, args: ['--background'] })` de
//! Electron y el `autostart.rs` del spike de Tauri (#20).
//!
//! Se usa la clave `Run` del usuario (`HKCU\Software\Microsoft\Windows\CurrentVersion\Run`),
//! nunca la de la máquina (HKLM): la app se instala por usuario y no pide
//! elevación. El valor arranca el ejecutable actual con `--background`, que
//! abre la app en la bandeja con las ventanas ocultas.
//!
//! Son tres llamadas de `advapi32` declaradas acá (`RegSetKeyValueW`,
//! `RegDeleteKeyValueW`, `RegGetValueW`): no hace falta otra dependencia.

#![cfg_attr(not(windows), allow(dead_code))]

/// El nombre del valor en `Run`: el identificador propio de la app del spike.
pub const VALUE_NAME: &str = "com.kushro.orgtree.dioxus-spike";
pub const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";

/// La línea de comandos que queda registrada.
pub fn command_line(exe: &std::path::Path) -> String {
    format!("\"{}\" --background", exe.display())
}

/// ¿La app arrancó con `--background` (desde el inicio de Windows)?
pub fn background() -> bool {
    std::env::args().skip(1).any(|a| a == "--background")
}

#[cfg(windows)]
mod ffi {
    /// `HKEY_CURRENT_USER`: `(HKEY)(ULONG_PTR)((LONG)0x80000001)`, extendido con signo.
    pub const HKEY_CURRENT_USER: isize = 0x8000_0001_u32 as i32 as isize;
    pub const REG_SZ: u32 = 1;
    pub const RRF_RT_REG_SZ: u32 = 0x0000_0002;
    pub const ERROR_SUCCESS: i32 = 0;
    pub const ERROR_FILE_NOT_FOUND: i32 = 2;

    #[link(name = "advapi32")]
    extern "system" {
        pub fn RegSetKeyValueW(hkey: isize, subkey: *const u16, value: *const u16, kind: u32, data: *const u8, len: u32) -> i32;
        pub fn RegDeleteKeyValueW(hkey: isize, subkey: *const u16, value: *const u16) -> i32;
        pub fn RegGetValueW(hkey: isize, subkey: *const u16, value: *const u16, flags: u32, kind: *mut u32, data: *mut u8, len: *mut u32) -> i32;
    }

    pub fn wide(text: &str) -> Vec<u16> {
        text.encode_utf16().chain(std::iter::once(0)).collect()
    }
}

/// Deja el registro como dice la preferencia. Devuelve el valor registrado
/// (o `None` si no hay entrada).
#[cfg(windows)]
pub fn apply(enabled: bool) -> Result<Option<String>, String> {
    use ffi::*;
    let key = wide(RUN_KEY);
    let name = wide(VALUE_NAME);
    if enabled {
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        let exe = std::path::PathBuf::from(exe.to_string_lossy().trim_start_matches(r"\\?\"));
        let line = command_line(&exe);
        let data = wide(&line);
        // REG_SZ con el terminador incluido; el largo va en bytes.
        let len = (data.len() * 2) as u32;
        // SAFETY: cadenas UTF-16 terminadas en cero que viven hasta el retorno.
        let status = unsafe { RegSetKeyValueW(HKEY_CURRENT_USER, key.as_ptr(), name.as_ptr(), REG_SZ, data.as_ptr().cast(), len) };
        if status != ERROR_SUCCESS {
            return Err(format!("Run: error {status}"));
        }
        Ok(Some(line))
    } else {
        // SAFETY: como arriba.
        let status = unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, key.as_ptr(), name.as_ptr()) };
        match status {
            ERROR_SUCCESS | ERROR_FILE_NOT_FOUND => Ok(None),
            other => Err(format!("Run: error {other}")),
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
    use ffi::*;
    let key = wide(RUN_KEY);
    let name = wide(VALUE_NAME);
    let mut buffer = vec![0u16; 2048];
    let mut len = (buffer.len() * 2) as u32;
    // SAFETY: el búfer tiene `len` bytes y RegGetValueW no escribe más.
    let status = unsafe {
        RegGetValueW(HKEY_CURRENT_USER, key.as_ptr(), name.as_ptr(), RRF_RT_REG_SZ, std::ptr::null_mut(), buffer.as_mut_ptr().cast(), &mut len)
    };
    if status != ERROR_SUCCESS {
        return None;
    }
    let chars = (len as usize / 2).min(buffer.len());
    let text = String::from_utf16_lossy(&buffer[..chars]);
    Some(text.trim_end_matches('\0').to_string())
}

#[cfg(not(windows))]
pub fn current() -> Option<String> {
    None
}

#[cfg(test)]
mod tests {
    #[test]
    fn linea_de_comandos() {
        assert_eq!(super::command_line(std::path::Path::new(r"C:\Apps\orgtree-dioxus.exe")), r#""C:\Apps\orgtree-dioxus.exe" --background"#);
    }
}
