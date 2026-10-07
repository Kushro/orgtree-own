//! Varias ventanas nativas (#13): el desk en una ventana separada, con su
//! propio VirtualDom (`new_window`), y el estado que comparten.
//!
//! **Estado compartido.** Las señales de Dioxus pertenecen al runtime de su
//! VirtualDom: una ventana no puede leer ni escribir las de otra. Lo que se
//! comparte vive fuera de los VirtualDom, en el proceso: un mapa con los
//! borradores y un canal `broadcast`. Cada ventana copia el valor a una señal
//! propia (`use_shared_draft`) y publica sus cambios en el canal; las demás
//! los reciben y re-renderizan. Es el patrón de "store" externo con
//! suscripción (como un `BroadcastChannel` entre pestañas).
//!
//! Límites: cada ventana tiene su copia (un cambio llega en el siguiente
//! tick del executor, no en el mismo render); el estado compartido tiene que
//! ser `Send + Clone` y se copia en cada cambio; nada de esto sobrevive al
//! proceso. Las ventanas corren en el mismo hilo, así que el registro de
//! ventanas usa `thread_local!`.
//!
//! **Cerrar la principal.** Cerrar la principal la oculta y la app sigue en
//! la bandeja (#14), como Electron con `exitOnClose` apagado, que es su valor
//! por defecto. Los desks abiertos en otras ventanas siguen vivos con su
//! borrador ("Main close preserves all popouts"). Salir (menú de la bandeja)
//! cierra todas las ventanas y la app termina.

use dioxus::desktop::tao::window::WindowId;
use dioxus::desktop::{Config, DesktopContext, LogicalSize, WindowBuilder, WindowCloseBehaviour};
use dioxus::prelude::*;
use orgtree_engine_client::Client;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use tokio::sync::broadcast;

static DRAFTS: Mutex<Option<HashMap<String, String>>> = Mutex::new(None);

fn channel() -> &'static broadcast::Sender<(String, String)> {
    static CHANNEL: OnceLock<broadcast::Sender<(String, String)>> = OnceLock::new();
    CHANNEL.get_or_init(|| broadcast::channel(64).0)
}

pub fn draft(key: &str) -> String {
    DRAFTS.lock().unwrap().as_ref().and_then(|d| d.get(key).cloned()).unwrap_or_default()
}

pub fn set_draft(key: &str, text: String) {
    DRAFTS.lock().unwrap().get_or_insert_with(HashMap::new).insert(key.to_string(), text.clone());
    let _ = channel().send((key.to_string(), text));
}

/// La copia local (de esta ventana) de un borrador compartido.
pub fn use_shared_draft(key: String) -> Signal<String> {
    let mut local = use_signal({
        let key = key.clone();
        move || draft(&key)
    });
    use_future(move || {
        let key = key.clone();
        async move {
            let mut changes = channel().subscribe();
            loop {
                match changes.recv().await {
                    Ok((changed, text)) if changed == key => {
                        if *local.peek() != text {
                            local.set(text);
                        }
                    }
                    Ok(_) => {}
                    // Se perdieron cambios: leer el valor actual.
                    Err(broadcast::error::RecvError::Lagged(_)) => local.set(draft(&key)),
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        }
    });
    local
}

thread_local! {
    static MAIN: RefCell<Option<DesktopContext>> = const { RefCell::new(None) };
    static POPOUTS: Cell<usize> = const { Cell::new(0) };
    static POPOUT_IDS: RefCell<Vec<WindowId>> = const { RefCell::new(Vec::new()) };
    /// Las ventanas de desk abiertas, para saber si alguna tiene el foco (#28).
    static POPOUT_WINDOWS: RefCell<Vec<DesktopContext>> = const { RefCell::new(Vec::new()) };
}

/// ¿Alguna ventana de desk tiene el foco? (las notificaciones se pausan con
/// Orgtree enfocado, salvo `notifyWhileFocused`).
pub fn popout_focused() -> bool {
    POPOUT_WINDOWS.with(|w| w.borrow().iter().any(|d| d.window.is_focused() && !d.window.is_minimized()))
}

/// La ventana principal se registra al montarse. Cerrarla la oculta.
pub fn register_main(window: DesktopContext) {
    window.set_close_behavior(WindowCloseBehaviour::WindowHides);
    MAIN.with(|main| *main.borrow_mut() = Some(window));
}

pub fn with_main(f: impl FnOnce(&DesktopContext)) {
    MAIN.with(|main| {
        if let Some(main) = main.borrow().as_ref() {
            f(main);
        }
    });
}

pub fn popout_count() -> usize {
    POPOUTS.with(Cell::get)
}

/// Salir: cerrar los desks y la principal (en modo cierre, no oculta). Sin
/// ventanas, Dioxus termina el loop y `LoopDestroyed` apaga el motor.
pub fn quit_all() {
    with_main(|main| {
        main.set_close_behavior(WindowCloseBehaviour::WindowCloses);
        for id in POPOUT_IDS.with(|ids| ids.borrow().clone()) {
            main.close_window(id);
        }
        main.close();
    });
}

/// Abre el desk de un agente en una ventana nativa nueva.
pub fn open_desk_window(client: Client, org: String, node: String) {
    let title = format!("{node} — Orgtree");
    let dom = VirtualDom::new_with_props(DeskWindow, DeskWindowProps { client, org, node });
    let window = WindowBuilder::new()
        .with_title(title)
        .with_inner_size(LogicalSize::new(760.0, 680.0))
        // Sin marco, como los popouts de Electron: botones propios en el header.
        .with_decorations(false)
        .with_min_inner_size(LogicalSize::new(420.0, 360.0));
    POPOUTS.with(|count| count.set(count.get() + 1));
    dioxus::desktop::window().new_window(dom, Config::new().with_window(window).with_menu(None));
}

/// Un desk cerró su ventana. Corre al soltar su VirtualDom, fuera de un render.
fn popout_closed(id: WindowId) {
    POPOUTS.with(|count| count.set(count.get().saturating_sub(1)));
    POPOUT_IDS.with(|ids| ids.borrow_mut().retain(|other| *other != id));
    POPOUT_WINDOWS.with(|w| w.borrow_mut().retain(|d| d.window.id() != id));
}

#[derive(Props, Clone)]
pub struct DeskWindowProps {
    client: Client,
    org: String,
    node: String,
}

impl PartialEq for DeskWindowProps {
    fn eq(&self, other: &Self) -> bool {
        self.org == other.org && self.node == other.node
    }
}

/// Raíz de la ventana de un desk: su propio VirtualDom con el mismo CSS.
#[allow(non_snake_case)]
fn DeskWindow(props: DeskWindowProps) -> Element {
    let client = props.client.clone();
    use_context_provider(|| Signal::new(Some(client)));
    use_context_provider(|| Signal::new(crate::Route::Desk { org: props.org.clone(), node: props.node.clone() }));
    let id = use_hook(|| {
        let id = dioxus::desktop::window().id();
        POPOUT_IDS.with(|ids| ids.borrow_mut().push(id));
        POPOUT_WINDOWS.with(|w| w.borrow_mut().push(dioxus::desktop::window()));
        id
    });
    use_drop(move || popout_closed(id));
    rsx! {
        style { {crate::RENDERER_CSS} }
        style { {crate::SHELL_CSS} }
        crate::desk::DeskView { org: props.org.clone(), node: props.node.clone(), popout: true }
        crate::probe::PopoutProbe {}
    }
}
