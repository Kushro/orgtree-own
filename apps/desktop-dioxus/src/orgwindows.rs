//! Una ventana principal por organización (#25), como `org-windows.ts`,
//! `window-close.ts`, `org-placement.ts` e `index.ts` de Electron, y el
//! `orgwindows.rs` del spike de Tauri (#20).
//!
//! **Registro.** `Registry` es puro (se prueba sin ventanas) y responde quién
//! es cada ventana principal: la que abre Dioxus al arrancar (`MAIN`, con la
//! pantalla de arranque, la bandeja, las notificaciones y la instancia única)
//! y una por cada org que se abre después. Una ventana sin org es una ventana
//! de inicio (Homepage). Las reglas de `requestOrg`:
//!
//! - si una ventana ya muestra esa org, se enfoca (no se abre otra);
//! - si la pide una ventana de inicio, ella misma se liga a la org;
//! - si no, se abre una ventana nueva, registrada **antes** de construirla
//!   (todo corre en el hilo del event loop), así que un segundo pedido de la
//!   misma org la encuentra.
//!
//! **Estado entre ventanas.** Las señales de Dioxus son de cada VirtualDom: el
//! registro vive en el hilo del event loop (`thread_local!`) y cada ventana
//! recibe sus órdenes (ir a una ruta, abrir el elemento de una notificación)
//! por un canal propio. La ventana anota su org en el registro cuando cambia
//! su ruta.
//!
//! **Cierre** (`performClose`): cerrar una de varias ventanas la cierra (y sus
//! desks aparte); la última se oculta en la bandeja, o sale si `exitOnClose`
//! está prendido y no queda otra vista. La principal nunca se destruye (tiene
//! la bandeja): cerrarla entre otras la oculta, la vuelve al inicio y la saca
//! de la sesión. Un `close()` programático no pasa por `CloseRequested`, así
//! que el botón de la ventana llama a `request_close`; Alt+F4 llega por el
//! manejador de eventos de `main.rs` (`native_close_requested`), que fija el
//! comportamiento de cierre antes de que Dioxus lo aplique.
//!
//! **Posición y restauración** (`placement.rs`): cada ventana guarda su lugar;
//! la salida guarda la sesión antes de cerrar nada, y el arranque reabre las
//! ventanas en su lugar, ajustadas al área de trabajo de los monitores de
//! ahora. Una ventana guardada cuya org ya no existe abre igual y muestra el
//! error del motor (la ventana de error de Electron).

use crate::placement::{self, Bounds, Placement, Store};
use crate::Route;
use dioxus::desktop::tao::dpi::{PhysicalPosition, PhysicalSize};
use dioxus::desktop::tao::event::{Event, WindowEvent};
use dioxus::desktop::tao::window::WindowId;
use dioxus::desktop::{use_wry_event_handler, Config, DesktopContext, LogicalSize, WindowBuilder, WindowCloseBehaviour};
use dioxus::prelude::*;
use orgtree_engine_client::DesktopNotice;
use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

/// Identidad de una ventana principal (propia: el `WindowId` de tao recién
/// existe después de construirla).
pub type Key = u64;
/// La ventana que abre Dioxus al arrancar.
pub const MAIN: Key = 1;
/// El tamaño mínimo de una ventana principal (lógico), como el de la principal.
pub const MIN_SIZE: (f64, f64) = (640.0, 480.0);

// ------------------------------------------------------------------ registro

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub key: Key,
    pub org: Option<String>,
    pub activated: u64,
    /// Solo la principal: se cerró entre otras (está oculta y fuera de la sesión).
    pub closed: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Decision {
    /// Ya había una ventana con esa org: se enfoca.
    Focused(Key),
    /// La ventana que pidió (una de inicio) se liga a la org.
    Bound(Key),
    /// Una ventana nueva, ya registrada con esa clave.
    Open(Key),
}

impl Decision {
    pub fn action(&self) -> &'static str {
        match self {
            Decision::Focused(_) => "focused",
            Decision::Bound(_) => "bound",
            Decision::Open(_) => "opened",
        }
    }

    pub fn key(&self) -> Key {
        match self {
            Decision::Focused(k) | Decision::Bound(k) | Decision::Open(k) => *k,
        }
    }
}

/// Las ventanas principales, en orden de registro.
#[derive(Clone, Debug)]
pub struct Registry {
    entries: Vec<Entry>,
    next: Key,
    activations: u64,
}

impl Default for Registry {
    fn default() -> Self {
        Registry { entries: vec![Entry { key: MAIN, org: None, activated: 0, closed: false }], next: MAIN + 1, activations: 0 }
    }
}

impl Registry {
    pub fn entry(&self, key: Key) -> Option<&Entry> {
        self.entries.iter().find(|e| e.key == key)
    }

    fn entry_mut(&mut self, key: Key) -> Option<&mut Entry> {
        self.entries.iter_mut().find(|e| e.key == key)
    }

    /// La ventana (abierta) que muestra esa org.
    pub fn holder(&self, org: &str) -> Option<Key> {
        self.entries.iter().find(|e| !e.closed && e.org.as_deref() == Some(org)).map(|e| e.key)
    }

    /// `requestOrg`: decide y registra en un solo paso.
    pub fn request_org(&mut self, from: Option<Key>, org: &str) -> Decision {
        if let Some(key) = self.holder(org) {
            return Decision::Focused(key);
        }
        if let Some(entry) = from.and_then(|from| self.entry_mut(from)) {
            if entry.org.is_none() {
                entry.org = Some(org.to_string());
                entry.closed = false;
                return Decision::Bound(entry.key);
            }
        }
        // La principal cerrada entre otras se reutiliza antes de abrir otra ventana.
        if let Some(main) = self.entry_mut(MAIN).filter(|m| m.closed) {
            main.org = Some(org.to_string());
            main.closed = false;
            return Decision::Bound(MAIN);
        }
        Decision::Open(self.reserve(Some(org.to_string())))
    }

    /// Registra una ventana nueva (antes de construirla).
    pub fn reserve(&mut self, org: Option<String>) -> Key {
        let key = self.next;
        self.next += 1;
        self.entries.push(Entry { key, org, activated: 0, closed: false });
        key
    }

    /// La ventana cambió de ruta. Devuelve la org anterior si cambió.
    pub fn set_org(&mut self, key: Key, org: Option<String>) -> Option<Option<String>> {
        let entry = self.entry_mut(key)?;
        if entry.org == org {
            return None;
        }
        Some(std::mem::replace(&mut entry.org, org))
    }

    pub fn remove(&mut self, key: Key) {
        if key != MAIN {
            self.entries.retain(|e| e.key != key);
        }
    }

    /// La principal se cerró entre otras: queda oculta, en el inicio y fuera de la sesión.
    pub fn close_main(&mut self) {
        if let Some(main) = self.entry_mut(MAIN) {
            main.closed = true;
            main.org = None;
        }
    }

    pub fn activate(&mut self, key: Key) {
        self.activations += 1;
        let at = self.activations;
        if let Some(entry) = self.entry_mut(key) {
            entry.activated = at;
            entry.closed = false;
        }
    }

    /// Las ventanas abiertas (sin la principal cerrada), en orden de registro.
    pub fn open(&self) -> Vec<&Entry> {
        self.entries.iter().filter(|e| !e.closed).collect()
    }

    /// La sesión: la clave de lugar de cada ventana abierta, en orden.
    pub fn session(&self) -> Vec<String> {
        self.open().iter().map(|e| placement::key_of(e.org.as_deref())).collect()
    }

    pub fn open_orgs(&self) -> Vec<String> {
        self.open().iter().filter_map(|e| e.org.clone()).collect()
    }

    /// La última ventana usada.
    pub fn last_used(&self) -> Option<Key> {
        self.open().iter().max_by_key(|e| e.activated).map(|e| e.key)
    }

    /// La dueña de las tareas de toda la app (notificaciones, bandeja): la
    /// primera registrada, que es siempre la principal.
    pub fn notification_owner(&self) -> Key {
        MAIN
    }
}

// --------------------------------------------------------------------- host

/// Lo que una ventana recibe de las demás.
#[derive(Clone, Debug)]
pub enum Command {
    Route(Route),
    Notice(DesktopNotice),
}

#[derive(Default)]
struct Host {
    registry: Registry,
    contexts: HashMap<Key, DesktopContext>,
    commands: HashMap<Key, futures_channel::mpsc::UnboundedSender<Command>>,
    /// Órdenes para ventanas que todavía no montaron.
    held: HashMap<Key, Vec<Command>>,
    /// El lugar pedido al construir cada ventana, para corregirlo al montarla.
    requested: HashMap<Key, Placement>,
    /// Las ventanas que ya se ubicaron (antes no se captura su lugar).
    settled: std::collections::HashSet<Key>,
}

thread_local! {
    static HOST: RefCell<Host> = RefCell::new(Host::default());
}

static STORE: Mutex<Option<Store>> = Mutex::new(None);

fn with_store<T>(f: impl FnOnce(&mut Store) -> T) -> T {
    let mut store = STORE.lock().unwrap();
    let store = store.get_or_insert_with(|| Store::load(crate::app_dir().map(|d| d.join("window-placement.json")).as_deref()));
    f(store)
}

fn with_host<T>(f: impl FnOnce(&mut Host) -> T) -> T {
    HOST.with(|host| f(&mut host.borrow_mut()))
}

/// Una copia del registro (para la prueba y las decisiones de la UI).
pub fn registry() -> Registry {
    with_host(|h| h.registry.clone())
}

pub fn context(key: Key) -> Option<DesktopContext> {
    with_host(|h| h.contexts.get(&key).cloned())
}

/// La clave de la ventana con ese `WindowId`, si es principal.
pub fn key_of_window(id: WindowId) -> Option<Key> {
    with_host(|h| h.contexts.iter().find(|(_, c)| c.window.id() == id).map(|(k, _)| *k))
}

/// La sesión guardada (para la prueba).
pub fn saved_session() -> Vec<String> {
    with_store(|s| s.session())
}

fn org_of(route: &Route) -> Option<String> {
    match route {
        Route::Home => None,
        Route::Org(org) => Some(org.clone()),
        Route::Desk { org, .. } => Some(org.clone()),
    }
}

/// Las orgs abiertas en alguna ventana, para que el inicio diga "abierta".
static OPEN_ORGS: Mutex<Option<tokio::sync::watch::Sender<Vec<String>>>> = Mutex::new(None);

fn open_orgs_channel<T>(f: impl FnOnce(&tokio::sync::watch::Sender<Vec<String>>) -> T) -> T {
    let mut channel = OPEN_ORGS.lock().unwrap();
    f(channel.get_or_insert_with(|| tokio::sync::watch::Sender::new(Vec::new())))
}

fn publish_open_orgs() {
    let orgs = with_host(|h| h.registry.open_orgs());
    open_orgs_channel(|c| c.send_replace(orgs));
}

/// Las orgs abiertas, como señal de esta ventana.
pub fn use_open_orgs() -> Signal<Vec<String>> {
    let mut orgs = use_signal(|| open_orgs_channel(|c| c.borrow().clone()));
    use_future(move || async move {
        let mut changes = open_orgs_channel(|c| c.subscribe());
        loop {
            let next = changes.borrow_and_update().clone();
            if *orgs.peek() != next {
                orgs.set(next);
            }
            if changes.changed().await.is_err() {
                break;
            }
        }
    });
    orgs
}

// ------------------------------------------------------------- el arranque

/// Lo que el arranque abre en la ventana principal: la primera ventana de la
/// sesión guardada (o el inicio) y su lugar, sin ajustar todavía.
pub fn main_plan() -> (Route, Option<Placement>) {
    let session = with_store(|s| s.session());
    let first = session.first().cloned().unwrap_or_else(|| placement::HOMEPAGE_KEY.to_string());
    let route = match placement::org_of_key(&first) {
        Some(org) => Route::Org(org.to_string()),
        None => Route::Home,
    };
    let saved = with_store(|s| s.saved(&first));
    with_host(|h| {
        h.registry.set_org(MAIN, org_of(&route));
        if let Some(saved) = saved {
            h.requested.insert(MAIN, saved);
        }
    });
    with_store(|s| s.opened(&first));
    (route, saved)
}

/// Aplica un lugar guardado al constructor de una ventana (píxeles físicos).
pub fn place_builder(builder: WindowBuilder, placement: Option<Placement>) -> WindowBuilder {
    match placement {
        Some(p) => builder
            .with_position(PhysicalPosition::new(p.bounds.x, p.bounds.y))
            .with_inner_size(PhysicalSize::new(p.bounds.width, p.bounds.height))
            .with_maximized(p.maximized),
        None => builder.with_inner_size(LogicalSize::new(1200.0, 800.0)),
    }
}

/// La principal está lista (el motor arrancó): se reabren las demás ventanas
/// de la sesión guardada, una sola vez.
fn restore_rest(main: &DesktopContext) {
    static RESTORED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if RESTORED.swap(true, std::sync::atomic::Ordering::SeqCst) {
        return;
    }
    let session = with_store(|s| s.session());
    let areas = work_areas(main);
    let min = min_physical(main);
    let visible = !crate::autostart::background();
    for key in session.iter().skip(1) {
        let org = placement::org_of_key(key).map(str::to_string);
        // Una org repetida en la sesión (dañada a mano) no abre dos ventanas.
        if let Some(org) = &org {
            if with_host(|h| h.registry.holder(org).is_some()) {
                continue;
            }
        }
        let placement = with_store(|s| s.restore(key, &areas, min));
        let win = with_host(|h| h.registry.reserve(org.clone()));
        let route = org.map(Route::Org).unwrap_or(Route::Home);
        build_window(main, win, route, placement, visible);
    }
    publish_open_orgs();
}

fn min_physical(ctx: &DesktopContext) -> (u32, u32) {
    let scale = ctx.window.scale_factor();
    ((MIN_SIZE.0 * scale).round() as u32, (MIN_SIZE.1 * scale).round() as u32)
}

/// Construye una ventana principal más (la de una org o una de inicio).
fn build_window(from: &DesktopContext, win: Key, route: Route, placement: Option<Placement>, visible: bool) {
    let title = match org_of(&route) {
        Some(org) => format!("{org} — Orgtree"),
        None => "Orgtree".to_string(),
    };
    if let Some(p) = placement {
        with_host(|h| h.requested.insert(win, p));
    }
    with_store(|s| s.opened(&placement::key_of(org_of(&route).as_deref())));
    let dom = VirtualDom::new_with_props(OrgWindow, OrgWindowProps { win, route });
    let builder = WindowBuilder::new()
        .with_title(title)
        // Sin marco, como la principal (#14): botones propios y arrastre por CSS.
        .with_decorations(false)
        .with_visible(visible)
        .with_min_inner_size(LogicalSize::new(MIN_SIZE.0, MIN_SIZE.1));
    let builder = place_builder(builder, placement);
    from.new_window(dom, Config::new().with_window(builder).with_menu(None));
}

/// La principal se registra al montarse, antes de que el motor esté listo (la
/// bandeja y una segunda ejecución la muestran desde la pantalla de arranque).
pub fn register_main(ctx: DesktopContext) {
    with_host(|h| {
        h.contexts.insert(MAIN, ctx);
    });
}

/// Cualquier ventana viva, para crear otra (`new_window` es del contexto).
fn any_context() -> Option<DesktopContext> {
    with_host(|h| h.contexts.get(&MAIN).cloned().or_else(|| h.contexts.values().next().cloned()))
}

// ------------------------------------------------------------- las órdenes

fn send(win: Key, command: Command) {
    with_host(|h| match h.commands.get(&win) {
        Some(sender) if sender.unbounded_send(command.clone()).is_ok() => {}
        _ => h.held.entry(win).or_default().push(command),
    });
}

/// Muestra, saca del minimizado y enfoca una ventana.
pub fn reveal(win: Key) {
    if let Some(ctx) = context(win) {
        ctx.window.set_visible(true);
        ctx.window.set_minimized(false);
        ctx.window.set_focus();
    }
}

/// `requestOrg` desde una ventana (`from`): enfoca, liga o abre.
pub fn open_org(from: Option<Key>, org: &str) -> Decision {
    let decision = with_host(|h| h.registry.request_org(from, org));
    match &decision {
        Decision::Focused(win) => reveal(*win),
        Decision::Bound(win) => {
            // La ventana de inicio pasa a ser la de la org: su lugar en la sesión también.
            with_store(|s| s.renamed(placement::HOMEPAGE_KEY, &placement::key_of(Some(org))));
            send(*win, Command::Route(Route::Org(org.to_string())));
            reveal(*win);
        }
        Decision::Open(win) => {
            let placement = any_context().and_then(|ctx| {
                let (areas, min) = (work_areas(&ctx), min_physical(&ctx));
                with_store(|s| s.restore(&placement::key_of(Some(org)), &areas, min))
            });
            match any_context() {
                Some(ctx) => build_window(&ctx, *win, Route::Org(org.to_string()), placement, true),
                None => with_host(|h| h.registry.remove(*win)),
            }
        }
    }
    crate::probe::log("request-org", serde_json::json!({ "from": from, "org": org, "action": decision.action(), "window": decision.key() }));
    publish_open_orgs();
    decision
}

/// "Nueva ventana" (`openHomepageWindow`): una ventana de inicio aparte.
pub fn open_homepage_window() -> Option<Key> {
    let ctx = any_context()?;
    let win = with_host(|h| h.registry.reserve(None));
    build_window(&ctx, win, Route::Home, None, true);
    Some(win)
}

/// El clic en una notificación: la ventana de su org (enfocada, ligada o
/// nueva) abre el elemento. Si la principal está en el inicio, se liga ella.
pub fn deliver_notice(notice: DesktopNotice) -> Decision {
    let decision = open_org(Some(MAIN), &notice.org.clone());
    send(decision.key(), Command::Notice(notice));
    reveal(decision.key());
    decision
}

/// "Abrir Orgtree" de la bandeja y una segunda ejecución: muestra las
/// ventanas abiertas y enfoca la última usada (o la principal).
pub fn show_all() {
    let (open, last) = with_host(|h| (h.registry.open().iter().map(|e| e.key).collect::<Vec<_>>(), h.registry.last_used()));
    for win in &open {
        if let Some(ctx) = context(*win) {
            ctx.window.set_visible(true);
            ctx.window.set_minimized(false);
        }
    }
    if open.is_empty() {
        with_host(|h| h.registry.activate(MAIN));
    }
    reveal(last.unwrap_or(MAIN));
}

/// ¿Alguna ventana principal tiene el foco (y no está minimizada)?
pub fn any_focused() -> bool {
    with_host(|h| h.contexts.values().any(|c| c.window.is_focused() && !c.window.is_minimized()))
}

/// La ventana que parpadea en la barra de tareas: la principal, o la primera
/// ventana visible si la principal está oculta.
pub fn with_attention_window(f: impl FnOnce(&DesktopContext)) {
    let target = with_host(|h| {
        let main = h.contexts.get(&MAIN).cloned();
        match main {
            Some(main) if main.window.is_visible() => Some(main),
            main => h
                .registry
                .open()
                .iter()
                .filter_map(|e| h.contexts.get(&e.key))
                .find(|c| c.window.is_visible())
                .cloned()
                .or(main),
        }
    });
    if let Some(target) = target {
        f(&target);
    }
}

// ------------------------------------------------------------------ cierre

/// Qué hacer con un pedido de cierre (`performClose`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CloseOutcome {
    /// La última ventana, con `exitOnClose` apagado: se oculta en la bandeja.
    Hide,
    /// La última ventana, con `exitOnClose` prendido y sin otras vistas: sale.
    Quit,
    /// Se cierra (o la principal, que se oculta y vuelve al inicio).
    Proceed,
}

/// `closeAction` + `performClose`, puro: `others` son las demás ventanas
/// principales visibles y `views` las vistas visibles que no son principales
/// (los desks aparte).
pub fn close_outcome(quitting: bool, others: usize, views: usize, exit_on_close: bool) -> CloseOutcome {
    if quitting || others > 0 {
        CloseOutcome::Proceed
    } else if exit_on_close && views == 0 {
        CloseOutcome::Quit
    } else {
        CloseOutcome::Hide
    }
}

fn decide(win: Key) -> CloseOutcome {
    let others = with_host(|h| {
        h.registry
            .open()
            .iter()
            .filter(|e| e.key != win)
            .filter(|e| h.contexts.get(&e.key).is_some_and(|c| c.window.is_visible()))
            .count()
    });
    let views = crate::windows::visible_popouts();
    let exit_on_close = crate::notify::prefs().get("exitOnClose").and_then(|v| v.as_bool()).unwrap_or(false);
    close_outcome(crate::lifecycle::quitting(), others, views, exit_on_close)
}

/// Lo que se desarma al cerrar una ventana: sus desks aparte, su lugar en la
/// sesión y su registro. La principal no se destruye: se oculta y vuelve al inicio.
fn teardown(win: Key) {
    let org = with_host(|h| h.registry.entry(win).and_then(|e| e.org.clone()));
    crate::windows::close_popouts_of(win);
    with_store(|s| s.closed(&placement::key_of(org.as_deref())));
    if win == MAIN {
        with_host(|h| h.registry.close_main());
        send(MAIN, Command::Route(Route::Home));
    } else {
        with_host(|h| h.registry.remove(win));
    }
    publish_open_orgs();
}

/// El botón de cerrar de una ventana (los de `WindowControls`).
pub fn request_close(window: &DesktopContext) {
    let Some(win) = key_of_window(window.window.id()) else {
        // Un desk aparte: se cierra.
        window.close();
        return;
    };
    let outcome = decide(win);
    crate::probe::log("close", serde_json::json!({ "window": win, "outcome": format!("{outcome:?}") }));
    match outcome {
        CloseOutcome::Hide => window.window.set_visible(false),
        CloseOutcome::Quit => quit("last-window"),
        CloseOutcome::Proceed => {
            teardown(win);
            if win == MAIN {
                window.window.set_visible(false);
            } else {
                window.set_close_behavior(WindowCloseBehaviour::WindowCloses);
                window.close();
            }
        }
    }
}

/// `CloseRequested` de tao (Alt+F4, el menú del sistema): corre en el manejador
/// de eventos de `main.rs`, antes de que Dioxus aplique el comportamiento de
/// cierre de la ventana, que se fija acá.
pub fn native_close_requested(id: WindowId) {
    let Some(win) = key_of_window(id) else { return };
    let Some(ctx) = context(win) else { return };
    let outcome = decide(win);
    crate::probe::log("close", serde_json::json!({ "window": win, "outcome": format!("{outcome:?}"), "native": true }));
    match outcome {
        CloseOutcome::Hide => ctx.set_close_behavior(WindowCloseBehaviour::WindowHides),
        CloseOutcome::Quit => {
            ctx.set_close_behavior(WindowCloseBehaviour::WindowHides);
            quit("last-window");
        }
        CloseOutcome::Proceed => {
            teardown(win);
            let behaviour = if win == MAIN { WindowCloseBehaviour::WindowHides } else { WindowCloseBehaviour::WindowCloses };
            ctx.set_close_behavior(behaviour);
        }
    }
}

// ------------------------------------------------------------------- salida

/// Salir de la app (bandeja, última ventana, pantalla de arranque, prueba):
/// guarda la sesión **antes** de cerrar nada y cierra todas las ventanas. Sin
/// ventanas, Dioxus termina el loop y `LoopDestroyed` apaga el motor.
pub fn quit(trigger: &'static str) {
    if !crate::lifecycle::begin_quit() {
        return;
    }
    crate::lifecycle::note_exit_trigger(trigger);
    save_session();
    let contexts: Vec<DesktopContext> = with_host(|h| h.contexts.values().cloned().collect());
    crate::windows::close_all_popouts();
    for ctx in contexts {
        ctx.set_close_behavior(WindowCloseBehaviour::WindowCloses);
        ctx.close();
    }
}

/// Guarda el lugar de cada ventana y la sesión (una sola vez por ejecución).
fn save_session() {
    for win in with_host(|h| h.contexts.keys().copied().collect::<Vec<_>>()) {
        capture(win);
    }
    let session = with_host(|h| h.registry.session());
    with_store(|s| s.begin_shutdown(&session));
}

/// El loop terminó (`LoopDestroyed`): por la salida, o por el fin de la sesión
/// de Windows (tao termina el loop con `WM_ENDSESSION`), cuando todavía nadie
/// guardó la sesión. Devuelve el camino de la salida.
pub fn loop_destroyed() -> &'static str {
    let path = if crate::lifecycle::quitting() { "quit" } else { "session-end" };
    crate::lifecycle::begin_quit();
    if !with_store(|s| s.shutting_down()) {
        save_session();
    }
    with_store(|s| s.flush());
    path
}

// --------------------------------------------------------------- posiciones

/// El área de trabajo de cada monitor, en píxeles físicos. tao no la da: en
/// Windows se pide con `GetMonitorInfoW` (sin la barra de tareas).
fn work_areas(ctx: &DesktopContext) -> Vec<Bounds> {
    ctx.window.available_monitors().map(|m| work_area(&m)).collect()
}

#[cfg(windows)]
fn work_area(monitor: &dioxus::desktop::tao::monitor::MonitorHandle) -> Bounds {
    use dioxus::desktop::tao::platform::windows::MonitorHandleExtWindows;
    #[repr(C)]
    struct MonitorInfo {
        cb_size: u32,
        rc_monitor: [i32; 4],
        rc_work: [i32; 4],
        flags: u32,
    }
    #[link(name = "user32")]
    extern "system" {
        fn GetMonitorInfoW(monitor: isize, info: *mut MonitorInfo) -> i32;
    }
    let mut info = MonitorInfo { cb_size: std::mem::size_of::<MonitorInfo>() as u32, rc_monitor: [0; 4], rc_work: [0; 4], flags: 0 };
    // SAFETY: HMONITOR válido de tao y una estructura MONITORINFO con cbSize.
    if unsafe { GetMonitorInfoW(monitor.hmonitor(), &mut info) } != 0 {
        let [left, top, right, bottom] = info.rc_work;
        if right > left && bottom > top {
            return Bounds { x: left, y: top, width: (right - left) as u32, height: (bottom - top) as u32 };
        }
    }
    monitor_bounds(monitor)
}

#[cfg(not(windows))]
fn work_area(monitor: &dioxus::desktop::tao::monitor::MonitorHandle) -> Bounds {
    monitor_bounds(monitor)
}

fn monitor_bounds(monitor: &dioxus::desktop::tao::monitor::MonitorHandle) -> Bounds {
    let (position, size) = (monitor.position(), monitor.size());
    Bounds { x: position.x, y: position.y, width: size.width, height: size.height }
}

/// Dónde está una ventana ahora (posición exterior y área cliente, físicos).
pub fn bounds_of(ctx: &DesktopContext) -> Option<Bounds> {
    let position = ctx.window.outer_position().ok()?;
    let size = ctx.window.inner_size();
    Some(Bounds { x: position.x, y: position.y, width: size.width, height: size.height })
}

/// Las áreas de trabajo de ahora (para la prueba).
pub fn current_work_areas() -> Vec<Bounds> {
    any_context().map(|ctx| work_areas(&ctx)).unwrap_or_default()
}

/// Anota el lugar de una ventana (solo en memoria; `flush` lo escribe).
fn capture(win: Key) {
    let Some(ctx) = context(win) else { return };
    let ready = with_host(|h| h.settled.contains(&win));
    // Una ventana minimizada no está donde la persona la quiere ver la próxima vez.
    if !ready || ctx.window.is_minimized() {
        return;
    }
    let org = with_host(|h| h.registry.entry(win).and_then(|e| e.org.clone()));
    let key = placement::key_of(org.as_deref());
    if ctx.window.is_maximized() {
        with_store(|s| s.capture_maximized(&key, true));
        return;
    }
    if let Some(bounds) = bounds_of(&ctx) {
        with_store(|s| s.capture(&key, Placement { bounds, maximized: false }));
    }
}

/// Pide un lugar exacto (la prueba lo usa para ubicar las ventanas).
pub fn set_bounds(win: Key, bounds: Bounds) {
    if let Some(ctx) = context(win) {
        ctx.window.set_maximized(false);
        ctx.window.set_outer_position(PhysicalPosition::new(bounds.x, bounds.y));
        ctx.window.set_inner_size(PhysicalSize::new(bounds.width, bounds.height));
        with_host(|h| h.requested.insert(win, Placement { bounds, maximized: false }));
    }
}

/// Al montar: deja la ventana entera dentro del área de trabajo (`fitWindow`)
/// y corrige el tamaño exacto. tao deja `WS_CAPTION` en las ventanas sin marco,
/// y el área cliente puede quedar distinta de la pedida (en Tauri, 30 px más
/// alta): se mide y se corrige la diferencia sobre lo pedido, como
/// `setExactPopoutBounds` en Electron.
async fn settle(win: Key) {
    let want = with_host(|h| h.requested.get(&win).copied());
    if let Some(want) = want.filter(|p| !p.maximized) {
        let mut ask = (want.bounds.width, want.bounds.height);
        for delay in [50u64, 400, 1000] {
            tokio::time::sleep(Duration::from_millis(delay)).await;
            let Some(ctx) = context(win) else { return };
            let target = fit_target(&ctx, want.bounds);
            if let Ok(position) = ctx.window.outer_position() {
                if (position.x, position.y) != (target.x, target.y) {
                    ctx.window.set_outer_position(PhysicalPosition::new(target.x, target.y));
                }
            }
            let size = ctx.window.inner_size();
            let (dw, dh) = (size.width as i64 - target.width as i64, size.height as i64 - target.height as i64);
            if (dw, dh) == (0, 0) {
                continue;
            }
            // Solo una diferencia chica es el marco invisible; una grande es otra cosa.
            if dw.abs() > 64 || dh.abs() > 64 {
                break;
            }
            ask = ((ask.0 as i64 - dw).max(1) as u32, (ask.1 as i64 - dh).max(1) as u32);
            ctx.window.set_inner_size(PhysicalSize::new(ask.0, ask.1));
            crate::probe::log("exact-size", serde_json::json!({ "window": win, "want": [target.width, target.height], "had": [size.width, size.height], "ask": [ask.0, ask.1] }));
        }
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    with_host(|h| {
        h.settled.insert(win);
        h.requested.remove(&win);
    });
    capture(win);
    with_store(|s| s.flush());
}

fn fit_target(ctx: &DesktopContext, bounds: Bounds) -> Bounds {
    let areas = work_areas(ctx);
    placement::fit_window(bounds, &areas, min_physical(ctx))
}

// ------------------------------------------------------------------ la vista

/// Qué ventana es esta (por contexto, para el inicio y el header de la org).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct WindowKey(pub Key);

pub fn this_window() -> Option<Key> {
    try_use_context::<WindowKey>().map(|k| k.0)
}

#[derive(Props, Clone, PartialEq)]
pub struct OrgWindowProps {
    win: Key,
    route: Route,
}

/// La raíz de una ventana principal más: su propio VirtualDom con la misma vista.
#[allow(non_snake_case)]
fn OrgWindow(props: OrgWindowProps) -> Element {
    rsx! { Shell { win: props.win, initial: props.route.clone() } }
}

/// Lo común a toda ventana principal: su ruta, el cliente del motor (que se
/// renueva con cada arranque), el registro y su lugar, el aviso del motor y la
/// vista. La vista se vuelve a montar con cada cliente nuevo.
#[component]
pub fn Shell(win: Key, initial: Route) -> Element {
    let mut route = use_signal(|| initial.clone());
    use_context_provider(|| route);
    use_context_provider(|| WindowKey(win));
    let mut focus = use_signal(|| None::<DesktopNotice>);
    use_context_provider(|| focus);
    let (client, generation) = crate::lifecycle::use_engine_client(None);
    use_context_provider(|| client);
    use_hook(move || {
        let ctx = dioxus::desktop::window();
        let (sender, mut receiver) = futures_channel::mpsc::unbounded::<Command>();
        let held = with_host(|h| {
            h.contexts.insert(win, ctx.clone());
            h.commands.insert(win, sender.clone());
            h.held.remove(&win).unwrap_or_default()
        });
        for command in held {
            let _ = sender.unbounded_send(command);
        }
        if win == MAIN {
            restore_rest(&ctx);
        }
        spawn(async move {
            use futures_util::StreamExt;
            while let Some(command) = receiver.next().await {
                match command {
                    Command::Route(next) => {
                        if *route.peek() != next {
                            route.set(next);
                        }
                    }
                    Command::Notice(notice) => {
                        let org = Route::Org(notice.org.clone());
                        if *route.peek() != org {
                            route.set(org);
                        }
                        focus.set(Some(notice));
                    }
                }
            }
        });
        spawn(settle(win));
        publish_open_orgs();
    });
    // La ruta de la ventana es su identidad: el registro y la sesión la siguen.
    use_effect(move || {
        let org = org_of(&route());
        let previous = with_host(|h| h.registry.set_org(win, org.clone()));
        if let Some(previous) = previous {
            let (from, to) = (placement::key_of(previous.as_deref()), placement::key_of(org.as_deref()));
            with_store(|s| s.renamed(&from, &to));
            publish_open_orgs();
        }
        if let Some(ctx) = context(win) {
            ctx.window.set_title(&match &org {
                Some(org) => format!("{org} — Orgtree"),
                None => "Orgtree".to_string(),
            });
        }
    });
    use_wry_event_handler(move |event, _| {
        if let Event::WindowEvent { event, .. } = event {
            match event {
                WindowEvent::Moved(_) | WindowEvent::Resized(_) => capture(win),
                WindowEvent::Focused(true) => with_host(|h| h.registry.activate(win)),
                _ => {}
            }
        }
    });
    // Se escribe el lugar cada tanto (arrastrar manda muchos `Moved`).
    use_future(move || async move {
        loop {
            tokio::time::sleep(Duration::from_secs(2)).await;
            with_store(|s| s.flush());
        }
    });
    use_drop(move || {
        with_host(|h| {
            h.contexts.remove(&win);
            h.commands.remove(&win);
            h.settled.remove(&win);
        });
    });
    let generation = generation();
    let current = route();
    rsx! {
        style { {crate::RENDERER_CSS} }
        style { {crate::SHELL_CSS} }
        style { {crate::ATTENTION_CSS} }
        crate::settings::ThemeStyle {}
        crate::lifecycle::EngineNotice {}
        for g in std::iter::once(generation) {
            Body { key: "{g}", route: current.clone() }
        }
        crate::wprobe::WindowAgent { win }
        if win == MAIN && client().is_some() {
            crate::probe::Probe {}
            crate::wprobe::WindowsProbe {}
        }
    }
}

#[component]
fn Body(route: Route) -> Element {
    match route {
        Route::Home => rsx! { crate::home::Home {} },
        Route::Org(slug) => rsx! { crate::org::OrgView { slug } },
        Route::Desk { org, node } => rsx! { crate::desk::DeskView { org, node } },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn una_ventana_por_org() {
        let mut r = Registry::default();
        // la principal es una ventana de inicio: pedir una org la liga
        assert_eq!(r.request_org(Some(MAIN), "a"), Decision::Bound(MAIN));
        // pedirla de nuevo, desde donde sea, la enfoca
        assert_eq!(r.request_org(Some(MAIN), "a"), Decision::Focused(MAIN));
        assert_eq!(r.request_org(None, "a"), Decision::Focused(MAIN));
        // desde una ventana con org, otra org abre una ventana nueva, ya registrada
        let b = r.request_org(Some(MAIN), "b");
        assert!(matches!(b, Decision::Open(k) if k != MAIN));
        assert_eq!(r.request_org(Some(MAIN), "b"), Decision::Focused(b.key()), "un segundo pedido la encuentra");
        // una ventana de inicio aparte se liga; una org abierta la enfoca sin abrir otra
        let home = r.reserve(None);
        assert_eq!(r.request_org(Some(home), "a"), Decision::Focused(MAIN));
        assert_eq!(r.entry(home).unwrap().org, None, "la de inicio sigue en el inicio");
        assert_eq!(r.request_org(Some(home), "c"), Decision::Bound(home));
        assert_eq!(r.open_orgs(), vec!["a", "b", "c"]);
        assert_eq!(r.session(), vec!["org:a", "org:b", "org:c"]);
    }

    #[test]
    fn la_principal_cerrada_se_reutiliza() {
        let mut r = Registry::default();
        r.request_org(Some(MAIN), "a");
        let b = r.request_org(Some(MAIN), "b").key();
        r.close_main();
        assert_eq!(r.session(), vec!["org:b"], "la principal cerrada sale de la sesión");
        assert_eq!(r.holder("a"), None);
        assert_eq!(r.request_org(Some(b), "a"), Decision::Bound(MAIN));
        assert_eq!(r.session(), vec!["org:a", "org:b"]);
        r.remove(b);
        r.remove(MAIN);
        assert_eq!(r.open().len(), 1, "la principal nunca se quita");
    }

    #[test]
    fn la_ultima_usada_y_el_cambio_de_ruta() {
        let mut r = Registry::default();
        let other = r.reserve(Some("x".into()));
        r.activate(other);
        assert_eq!(r.last_used(), Some(other));
        r.activate(MAIN);
        assert_eq!(r.last_used(), Some(MAIN));
        assert_eq!(r.set_org(other, Some("y".into())), Some(Some("x".into())));
        assert_eq!(r.set_org(other, Some("y".into())), None);
        assert_eq!(r.notification_owner(), MAIN);
    }

    #[test]
    fn cerrar_como_perform_close() {
        // una de varias: se cierra
        assert_eq!(close_outcome(false, 1, 0, true), CloseOutcome::Proceed);
        // la última: se oculta, o sale con exitOnClose y sin otras vistas
        assert_eq!(close_outcome(false, 0, 0, false), CloseOutcome::Hide);
        assert_eq!(close_outcome(false, 0, 0, true), CloseOutcome::Quit);
        assert_eq!(close_outcome(false, 0, 1, true), CloseOutcome::Hide, "un desk aparte visible la retiene");
        // saliendo, todo se cierra
        assert_eq!(close_outcome(true, 0, 0, false), CloseOutcome::Proceed);
    }
}
