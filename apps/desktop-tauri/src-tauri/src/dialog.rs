//! La confirmación de descarte de una organización sin crear (#20), la misma
//! pregunta para cerrar la ventana, cancelar la creación y salir de la app
//! (`CREATION_DISCARD_DIALOG` de Electron, decisión del usuario 2026-09-21).
//!
//! En Windows es un `MessageBoxW` modal de la ventana cuyo formulario está en
//! juego, con Aceptar (descartar) y Cancelar (seguir editando). Cancelar es el
//! botón por defecto, y Escape o la X también conservan el borrador: la
//! respuesta peligrosa nunca es la que da cerrar la pregunta. `MessageBoxW` no
//! deja poner textos propios en los botones, así que el texto lo explica.
//!
//! Bloquea hasta la respuesta: se llama desde un hilo propio, nunca desde el
//! hilo de eventos.

#![cfg_attr(not(windows), allow(dead_code))]

pub const TITLE: &str = "Orgtree";
pub const MESSAGE: &str = "Discard this new organization?\n\nThe details you have entered have not been saved and will be lost.\n\nOK discards them; Cancel keeps editing.";

/// Devuelve `true` solo si la persona eligió descartar.
#[cfg(windows)]
pub fn confirm_discard(owner: Option<isize>) -> bool {
    use std::ffi::c_void;
    #[link(name = "user32")]
    extern "system" {
        fn MessageBoxW(hwnd: *mut c_void, text: *const u16, caption: *const u16, kind: u32) -> i32;
    }
    const MB_OKCANCEL: u32 = 0x1;
    const MB_ICONWARNING: u32 = 0x30;
    const MB_DEFBUTTON2: u32 = 0x100;
    const IDOK: i32 = 1;
    let wide = |s: &str| s.encode_utf16().chain([0]).collect::<Vec<u16>>();
    let (text, caption) = (wide(MESSAGE), wide(TITLE));
    let hwnd = owner.map(|h| h as *mut c_void).unwrap_or(std::ptr::null_mut());
    // SAFETY: cadenas terminadas en NUL que viven durante la llamada; hwnd es
    // una ventana de la app o nulo.
    unsafe { MessageBoxW(hwnd, text.as_ptr(), caption.as_ptr(), MB_OKCANCEL | MB_ICONWARNING | MB_DEFBUTTON2) == IDOK }
}

#[cfg(not(windows))]
pub fn confirm_discard(_owner: Option<isize>) -> bool {
    // Fuera de Windows no hay diálogo en el spike: se conserva el borrador.
    false
}
