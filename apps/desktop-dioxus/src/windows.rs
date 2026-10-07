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
//! **Cerrar la principal.** Con desks abiertos en otras ventanas, cerrar la
//! principal la oculta (como Electron: "Main close preserves all popouts") y
//! los desks siguen vivos con su borrador. Al cerrar el último desk con la
//! principal oculta, la app termina.

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
}

/// La ventana principal se registra al montarse.
pub fn register_main(window: DesktopContext) {
    MAIN.with(|main| *main.borrow_mut() = Some(window));
    sync_main_close();
}

pub fn popout_count() -> usize {
    POPOUTS.with(Cell::get)
}

/// Con desks abiertos, cerrar la principal la oculta; sin ellos, la cierra.
fn sync_main_close() {
    let behaviour = if popout_count() > 0 { WindowCloseBehaviour::WindowHides } else { WindowCloseBehaviour::WindowCloses };
    MAIN.with(|main| {
        if let Some(main) = main.borrow().as_ref() {
            main.set_close_behavior(behaviour);
        }
    });
}

/// Abre el desk de un agente en una ventana nativa nueva.
pub fn open_desk_window(client: Client, org: String, node: String) {
    let title = format!("{node} — Orgtree");
    let dom = VirtualDom::new_with_props(DeskWindow, DeskWindowProps { client, org, node });
    let window = WindowBuilder::new()
        .with_title(title)
        .with_inner_size(LogicalSize::new(760.0, 680.0))
        .with_min_inner_size(LogicalSize::new(420.0, 360.0));
    POPOUTS.with(|count| count.set(count.get() + 1));
    sync_main_close();
    dioxus::desktop::window().new_window(dom, Config::new().with_window(window).with_menu(None));
}

/// Un desk cerró su ventana. Corre al soltar su VirtualDom, fuera de un render.
fn popout_closed() {
    POPOUTS.with(|count| count.set(count.get().saturating_sub(1)));
    sync_main_close();
    if popout_count() == 0 {
        MAIN.with(|main| {
            if let Some(main) = main.borrow().as_ref() {
                if !main.window.is_visible() {
                    // La principal estaba oculta y no queda ninguna ventana: salir.
                    main.close();
                }
            }
        });
    }
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
    use_drop(popout_closed);
    rsx! {
        style { {crate::RENDERER_CSS} }
        style { {crate::SHELL_CSS} }
        crate::desk::DeskView { org: props.org.clone(), node: props.node.clone(), popout: true }
        crate::probe::PopoutProbe {}
    }
}
