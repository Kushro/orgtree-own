//! Integración nativa básica (#14): ventana sin marco con botones propios,
//! ícono en la bandeja, notificación nativa e instancia única.
//!
//! Dioxus no trae plugins para la instancia única ni para las notificaciones
//! (Tauri sí): acá son unas decenas de líneas propias y `notify-rust`.

use dioxus::desktop::tao::event::{Event, WindowEvent};
use dioxus::desktop::trayicon::menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem};
use dioxus::desktop::{use_tray_menu_event_handler, use_wry_event_handler};
use dioxus::prelude::*;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::cell::RefCell;
use std::sync::Mutex;

/// Los botones de la ventana sin marco, con las clases de `WindowControls`
/// del renderer (`renderer/src/window-controls.tsx`). Actúan sobre la ventana
/// en la que están, así que sirven igual en la principal y en un desk aparte.
#[component]
pub fn WindowControls() -> Element {
    let mut maximized = use_signal(|| dioxus::desktop::window().is_maximized());
    // Sigue el estado real: maximizar con doble clic en la zona de arrastre,
    // Win+flechas, etc.
    use_wry_event_handler(move |event, _| {
        if let Event::WindowEvent { event: WindowEvent::Resized(_), .. } = event {
            let now = dioxus::desktop::window().is_maximized();
            if *maximized.peek() != now {
                maximized.set(now);
            }
        }
    });
    let label = if maximized() { "Restore window" } else { "Maximize window" };
    rsx! {
        div { class: "window-controls", role: "group", aria_label: "Window controls",
            button { r#type: "button", class: "window-control", aria_label: "Minimize window", title: "Minimize window",
                onclick: move |_| dioxus::desktop::window().set_minimized(true),
                svg { width: "1em", height: "1em", view_box: "0 0 24 24", fill: "currentColor", path { d: "M6 19h12v2H6z" } }
            }
            button { r#type: "button", class: "window-control", aria_label: "{label}", title: "{label}",
                onclick: move |_| {
                    let window = dioxus::desktop::window();
                    window.set_maximized(!window.is_maximized());
                },
                if maximized() {
                    svg { width: "1em", height: "1em", view_box: "0 0 24 24", fill: "currentColor",
                        path { d: "M4 8h12v12H4zm2 2v8h8v-8zM8 4h12v12h-2V6H8z" } }
                } else {
                    svg { width: "1em", height: "1em", view_box: "0 0 24 24", fill: "currentColor",
                        path { d: "M4 4h16v16H4zm2 2v12h12V6z" } }
                }
            }
            button { r#type: "button", class: "window-control close", aria_label: "Close window", title: "Close window",
                // #25: el cierre decide (`performClose`): una de varias se cierra, la
                // última se oculta en la bandeja o sale con `exitOnClose`.
                onclick: move |_| crate::orgwindows::request_close(&dioxus::desktop::window()),
                svg { width: "1em", height: "1em", view_box: "0 0 24 24", fill: "currentColor",
                    path { d: "M19 6.41 17.59 5 12 10.59 6.41 5 5 6.41 10.59 12 5 17.59 6.41 19 12 13.41 17.59 19 19 17.59 13.41 12z" } }
            }
        }
    }
}

/// Ícono en la bandeja. Un clic en el ícono muestra las ventanas
/// (comportamiento por defecto de Dioxus). El menú, como el de Electron con lo
/// que el spike tiene: el estado del motor (#25), abrir, una ventana nueva,
/// reiniciar el motor (siempre visible y sin confirmación, como decidió el
/// usuario para Electron), arrancar con Windows, salir al cerrar la última
/// ventana y salir.
static TRAY_READY: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn tray_ready() -> bool {
    TRAY_READY.load(std::sync::atomic::Ordering::SeqCst)
}

/// Los ítems que cambian (los de muda no son `Send`: viven en el hilo del loop).
struct TrayItems {
    tray: dioxus::desktop::trayicon::DioxusTray,
    status: MenuItem,
    start_at_login: CheckMenuItem,
    exit_on_close: CheckMenuItem,
}

thread_local! {
    static TRAY_ITEMS: RefCell<Option<TrayItems>> = const { RefCell::new(None) };
}

pub fn use_tray() {
    use_hook(|| {
        let menu = Menu::new();
        let status = MenuItem::with_id("engine-status", crate::lifecycle::tray_line(), false, None);
        let open = MenuItem::with_id("open", "Abrir Orgtree", true, None);
        let new_window = MenuItem::with_id("new-window", "Nueva ventana", true, None);
        let restart = MenuItem::with_id("restart-engine", "Reiniciar motor", true, None);
        let prefs = crate::notify::prefs();
        let flag = |key: &str| prefs.get(key).and_then(|v| v.as_bool()).unwrap_or(false);
        let start_at_login = CheckMenuItem::with_id("start-at-login", "Iniciar con Windows", true, flag("startAtLogin"), None);
        let exit_on_close = CheckMenuItem::with_id("exit-on-close", "Salir al cerrar la última ventana", true, flag("exitOnClose"), None);
        let quit = MenuItem::with_id("quit", "Salir", true, None);
        let (sep1, sep2, sep3) = (PredefinedMenuItem::separator(), PredefinedMenuItem::separator(), PredefinedMenuItem::separator());
        let _ = menu.append_items(&[
            &status,
            &sep1,
            &open,
            &new_window,
            &restart,
            &sep2,
            &start_at_login,
            &exit_on_close,
            &sep3,
            &quit,
        ]);
        let tray = dioxus::desktop::trayicon::init_tray_icon(menu, None);
        TRAY_ITEMS.with(|items| *items.borrow_mut() = Some(TrayItems { tray, status, start_at_login, exit_on_close }));
        TRAY_READY.store(true, std::sync::atomic::Ordering::SeqCst);
    });
    use_tray_menu_event_handler(|event| match event.id().as_ref() {
        "open" => show_main(),
        "new-window" => {
            crate::orgwindows::open_homepage_window();
        }
        "restart-engine" => crate::lifecycle::restart_from_tray(),
        "start-at-login" | "exit-on-close" => {
            let key = if event.id().as_ref() == "start-at-login" { "startAtLogin" } else { "exitOnClose" };
            let on = !crate::notify::prefs().get(key).and_then(|v| v.as_bool()).unwrap_or(false);
            let mut patch = serde_json::Map::new();
            patch.insert(key.into(), serde_json::Value::Bool(on));
            crate::notify::set_prefs(&patch);
        }
        "quit" => quit(),
        _ => {}
    });
    // La línea de estado del motor sigue al canal del ciclo de vida.
    use_future(|| async move {
        let mut changes = crate::lifecycle::subscribe_status();
        loop {
            refresh_tray();
            if changes.changed().await.is_err() {
                break;
            }
        }
    });
}

/// Pone la bandeja al día: la línea y el tooltip del motor, y las preferencias.
/// Solo en el hilo del loop (los ítems viven ahí).
pub fn refresh_tray() {
    let line = crate::lifecycle::tray_line();
    let prefs = crate::notify::prefs();
    let flag = |key: &str| prefs.get(key).and_then(|v| v.as_bool()).unwrap_or(false);
    let _ = TRAY_ITEMS.try_with(|items| {
        if let Some(items) = items.borrow().as_ref() {
            items.status.set_text(&line);
            items.start_at_login.set_checked(flag("startAtLogin"));
            items.exit_on_close.set_checked(flag("exitOnClose"));
            let _ = items.tray.set_tooltip(Some(format!("Orgtree — {line}")));
        }
    });
}

/// "Abrir Orgtree" de la bandeja y una segunda ejecución.
pub fn show_main() {
    crate::orgwindows::show_all();
}

/// Salir de la app (menú de la bandeja). El motor se apaga en `LoopDestroyed`.
pub fn quit() {
    crate::orgwindows::quit("tray");
}

/// Notificación nativa. En Windows, `notify-rust` usa el AppUserModelID de
/// PowerShell si la app no está instalada con el suyo (el instalador lo
/// registraría con el acceso directo).
pub fn notify(title: &str, body: &str) -> Result<(), String> {
    notify_rust::Notification::new().summary(title).body(body).show().map(|_| ()).map_err(|e| e.to_string())
}

/// Instancia única por raíz de datos (como `requestSingleInstanceLock` de
/// Electron por `userData`): la primera instancia toma un candado de archivo
/// y escucha en un puerto local; una segunda ejecución no consigue el
/// candado, le avisa por ese puerto y termina. Al recibir el aviso, la
/// primera enfoca su ventana.
pub enum Instance {
    First,
    Second,
}

static LOCK: Mutex<Option<std::fs::File>> = Mutex::new(None);
static MESSAGES: Mutex<Option<futures_channel::mpsc::UnboundedReceiver<String>>> = Mutex::new(None);

fn sibling(data_root: &Path, suffix: &str) -> PathBuf {
    let mut path = data_root.as_os_str().to_owned();
    path.push(suffix);
    PathBuf::from(path)
}

pub fn single_instance(data_root: &Path) -> Instance {
    if let Some(parent) = data_root.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let port_file = sibling(data_root, ".dioxus-instance-port");
    let lock = std::fs::OpenOptions::new().create(true).truncate(false).write(true).open(sibling(data_root, ".dioxus-instance.lock"));
    let Ok(lock) = lock else { return Instance::First };
    if lock.try_lock().is_err() {
        // Otra instancia tiene el candado: avisarle y terminar.
        let args: Vec<String> = std::env::args().skip(1).collect();
        if let Some(port) = std::fs::read_to_string(&port_file).ok().and_then(|p| p.trim().parse::<u16>().ok()) {
            if let Ok(mut stream) = TcpStream::connect(("127.0.0.1", port)) {
                let _ = writeln!(stream, "{}", if args.is_empty() { "focus".to_string() } else { args.join(" ") });
            }
        }
        return Instance::Second;
    }
    *LOCK.lock().unwrap() = Some(lock);
    let (sender, receiver) = futures_channel::mpsc::unbounded();
    *MESSAGES.lock().unwrap() = Some(receiver);
    if let Ok(listener) = TcpListener::bind("127.0.0.1:0") {
        if let Ok(address) = listener.local_addr() {
            let _ = std::fs::write(&port_file, address.port().to_string());
        }
        let _ = std::thread::Builder::new().name("orgtree-instance".into()).spawn(move || {
            for stream in listener.incoming().flatten() {
                let mut line = String::new();
                if BufReader::new(stream).read_line(&mut line).is_ok() {
                    let _ = sender.unbounded_send(line.trim().to_string());
                }
            }
        });
    }
    Instance::First
}

/// En la ventana principal: atiende los avisos de una segunda ejecución.
pub fn use_second_instance() {
    use_hook(|| {
        let Some(mut messages) = MESSAGES.lock().unwrap().take() else { return };
        spawn(async move {
            use futures_util::StreamExt;
            while let Some(message) = messages.next().await {
                show_main();
                crate::probe::second_instance(&message);
            }
        });
    });
}
