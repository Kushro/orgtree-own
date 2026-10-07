//! El organigrama en RSX (#26): el árbol de la org con el estado de cada
//! agente, la barra de la org (`orgbar`) con los conteos y los créditos, y el
//! menú de agente con las operaciones principales.
//!
//! Equivale a lo esencial de `canvas/OrgCanvas.tsx`, `cards.tsx`,
//! `agenttray.tsx`, `agentmenu.tsx` y el `orgbar` de `App.tsx`, sin el lienzo
//! WebGL ni el pan/zoom: un árbol indentado con las clases `.node`, `.card` y
//! `.kids` del CSS del renderer.
//!
//! - **Actualización:** solo por el WebSocket de la org. Un `changed`, un
//!   `node_event` o una reconexión vuelven a pedir el árbol (como el
//!   `refreshTree` de `App.tsx`); varios frames juntos piden una sola vez. No
//!   hay sondeo, y una operación hecha desde acá tampoco pide el árbol: el
//!   cambio llega por el mismo frame que vería otra ventana.
//! - **Operaciones:** los mismos endpoints y cuerpos que `api.ts` (`runOp`,
//!   `haltNode`, `unhaltNode`, `interruptNode`), con las mismas confirmaciones
//!   y los mismos avisos (con deshacer) que `agentmenu.tsx`.

use crate::icons::HomeIcon;
use crate::Route;
use dioxus::prelude::*;
use orgtree_engine_client::{
    AskInfo, Backoff, BatchAnswer, Client, DesktopNotice, Frame, InboxPayload, MailRow, NodeState, Offer, OpRequest, ProvidersPayload, TreeNode,
    TreePayload, WorkItem, WorkItemsPayload, WsEvent,
};
use std::collections::{BTreeMap, HashSet};
use std::time::Duration;

/// Orden de los tiers como `ALL_TIERS` de `canvas/shared.ts`.
pub(crate) const ALL_TIERS: [&str; 12] =
    ["haiku", "sonnet", "opus", "fable", "gpt-reserve", "luna", "terra", "sol", "astra", "flash", "pro", "argon"];
/// El aviso con deshacer dura 12 s, como en el renderer.
const TOAST_FOR: Duration = Duration::from_secs(12);

/// `TIER_LETTER` de `canvas/shared.ts`.
pub(crate) fn tier_letter(tier: &str) -> &'static str {
    match tier {
        "haiku" => "H",
        "sonnet" | "sol" => "S",
        "opus" => "O",
        "fable" | "flash" => "F",
        "gpt-reserve" => "R",
        "luna" => "L",
        "terra" => "T",
        "astra" | "argon" => "A",
        "pro" => "P",
        _ => "?",
    }
}

/// `providerOf` y `PROVIDER_LABEL` de `canvas/shared.ts`.
pub(crate) fn provider_of(tier: &str) -> (&'static str, &'static str) {
    match tier {
        "gpt-reserve" | "luna" | "terra" | "sol" | "astra" => ("openai", "Codex"),
        "flash" | "pro" | "argon" => ("google", "Antigravity"),
        t if t.starts_with("or-") => ("openrouter", "OpenRouter"),
        _ => ("claude", "Claude"),
    }
}

/// `fmtCredits` de `canvas/shared.ts`.
fn credits(n: f64) -> String {
    let rounded = (n * 100.0).round() / 100.0;
    if rounded.fract() == 0.0 {
        format!("{}", rounded as i64)
    } else {
        format!("{rounded}")
    }
}

/// El estado que muestra una fila, con la precedencia de `TrayStatus` y
/// `deriveTurnState` (`canvas/desk.tsx`).
#[derive(Clone, Copy, PartialEq, Debug)]
enum Status {
    Halted,
    Halting,
    Frozen,
    Compacting,
    Queued,
    Working,
    Idle,
    Retired,
}

fn status(node: &TreeNode) -> Status {
    if node.state != NodeState::Live {
        return Status::Retired;
    }
    if let Some(halt) = &node.halt {
        return if halt.phase == "halting" { Status::Halting } else { Status::Halted };
    }
    if node.frozen.as_ref().is_some_and(|f| !f.is_null()) {
        return Status::Frozen;
    }
    if node.phase.as_deref() == Some("compacting") {
        return Status::Compacting;
    }
    // en cola detrás del límite de turnos de la máquina (`waiting`, `queued_for_slot`)
    if node.waiting == Some(true) || node.queued_for_slot.as_ref().is_some_and(|q| !q.is_null()) {
        return Status::Queued;
    }
    if node.busy == Some(true) {
        return Status::Working;
    }
    Status::Idle
}

/// A quién apunta un menú: al usuario (la raíz) o a un agente.
#[derive(Clone, PartialEq, Debug)]
enum Target {
    User,
    Agent(String),
}

#[derive(Clone, PartialEq, Debug)]
struct Menu {
    target: Target,
    x: f64,
    y: f64,
}

/// Los diálogos del organigrama.
#[derive(Clone, PartialEq, Debug)]
enum Dialog {
    /// Contratar bajo `parent` (`None`: primer nivel, bajo el usuario).
    Hire { parent: Option<String> },
    Move { node: String },
    /// `retire` o `dissolve` (con subordinados vivos), como `AgentRetireConfirm`.
    Retire { node: String, dissolve: bool },
}

/// Lo que hace el botón de un aviso.
#[derive(Clone, PartialEq, Debug)]
pub(crate) enum Undo {
    Rehire(String),
    MoveBack { node: String, parent: Option<String> },
}

#[derive(Clone, PartialEq, Debug)]
struct Toast {
    id: u64,
    lines: Vec<String>,
    undo: Option<Undo>,
}

/// Cómo se mantiene al día el árbol, a la vista en la página para la prueba.
#[derive(Clone, Copy, Default, PartialEq, Debug)]
struct Sync {
    /// Pedidos del árbol: el inicial y uno por tanda de frames.
    loads: u32,
    /// Frames que pidieron un árbol nuevo.
    frames: u32,
    connected: bool,
}

/// La vista de la org: el organigrama o la cola de atención (`OrgViewToggle`).
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum View {
    Chart,
    Attention,
}

/// Todo lo que el árbol, el menú, los diálogos, la bandeja y la cola de
/// atención comparten.
#[derive(Clone, Copy)]
pub(crate) struct Ctx {
    pub(crate) slug: Signal<String>,
    client: Signal<Client>,
    menu: Signal<Option<Menu>>,
    dialog: Signal<Option<Dialog>>,
    toasts: Signal<Vec<Toast>>,
    /// El scope de la vista: las tareas de un menú o un diálogo viven acá,
    /// porque el menú o el diálogo se desmontan apenas se elige la acción
    /// (y `spawn` ata la tarea al componente que la lanza).
    pub(crate) scope: ScopeId,
    // Bandeja, preguntas y atención (#28).
    pub(crate) tree: Signal<Option<Result<TreePayload, String>>>,
    /// `GET /inbox` y `GET /work-items-view`, releídos con el árbol.
    pub(crate) inbox: Signal<Option<InboxPayload>>,
    pub(crate) work: Signal<Option<WorkItemsPayload>>,
    pub(crate) tz: Signal<Option<crate::desk::Tz>>,
    /// Mail marcado leído acá que el motor todavía lista sin leer (`mailread`).
    pub(crate) read_here: Signal<HashSet<String>>,
    /// Tarjetas de pedidos enviadas, que dejan la lista en el clic (`asksubmitted`).
    pub(crate) submitted: Signal<HashSet<String>>,
    /// Tickets cuya bandera se está descartando (`dismissing`).
    pub(crate) dismissing: Signal<HashSet<String>>,
    pub(crate) view: Signal<View>,
    pub(crate) inbox_open: Signal<bool>,
    pub(crate) inbox_sel: Signal<Option<String>>,
    pub(crate) attn_sel: Signal<Option<String>>,
    // Docket (#29).
    pub(crate) docket_open: Signal<bool>,
    pub(crate) docket_sel: Signal<Option<String>>,
    /// Las dos casillas del docket: el archivo y el backlog se piden solo con ellas.
    pub(crate) docket_archived: Signal<bool>,
    pub(crate) docket_backlog: Signal<bool>,
    /// Los ajustes de la org (#30).
    pub(crate) settings_open: Signal<bool>,
}

impl Ctx {
    pub(crate) fn spawn(self, task: impl std::future::Future<Output = ()> + 'static) {
        dioxus::core::Runtime::current().spawn(self.scope, task);
    }

    pub(crate) fn ids(self) -> (Client, String) {
        (self.client.peek().clone(), self.slug.peek().clone())
    }

    /// Mail sin leer (sin lo marcado acá), urgentes y pedidos abiertos.
    pub(crate) fn counts(self) -> (usize, usize, usize) {
        let read = self.read_here.read();
        let (unread, urgent) = match &*self.inbox.read() {
            Some(inbox) => {
                let waiting: Vec<&MailRow> = inbox.pending.iter().filter(|m| !read.contains(m.id.as_deref().unwrap_or_default())).collect();
                (waiting.len(), waiting.iter().filter(|m| m.urgent == Some(true)).count())
            }
            None => (0, 0),
        };
        let asks = match &*self.tree.read() {
            Some(Ok(tree)) => crate::inbox::open_asks(tree, &self.submitted.read()).len(),
            _ => 0,
        };
        (unread, urgent, asks)
    }

    /// `markReadNow`: leído en el clic, guardado de fondo, y de vuelta sin
    /// leer (con el error) si el motor lo rechaza.
    pub(crate) fn mark_read(mut self, id: String) {
        let pending = self.inbox.peek().as_ref().is_some_and(|i| i.pending.iter().any(|m| m.id.as_deref() == Some(id.as_str())));
        if !pending || !self.read_here.write().insert(id.clone()) {
            return;
        }
        let (client, slug) = self.ids();
        self.spawn(async move {
            if let Err(error) = client.mark_read(&slug, std::slice::from_ref(&id)).await {
                if let Ok(mut read) = self.read_here.try_write() {
                    read.remove(&id);
                }
                self.toast(vec![format!("could not mark read: {error}")], None);
            }
        });
    }

    /// "Mark all read" (`clearInbox`): archiva todo lo que queda sin leer.
    pub(crate) fn clear_inbox(self) {
        let (client, slug) = self.ids();
        self.spawn(async move {
            if let Err(error) = client.clear_inbox(&slug).await {
                self.toast(vec![format!("error: {error}")], None);
            }
        });
    }

    /// Responder un mail (`sendLinkedReply`): al remitente, y con un recibo
    /// durable el original queda leído.
    pub(crate) async fn reply_mail(self, mail: &MailRow, text: &str) -> Result<(), String> {
        let (client, slug) = self.ids();
        let id = mail.id.clone().unwrap_or_default();
        let op = format!("dx-reply-{}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0));
        let receipt = client.reply_mail(&slug, &mail.from, text, &id, Some(op)).await.map_err(|e| e.to_string())?;
        let mut lines = vec![format!("sent to {}", mail.from)];
        lines.extend(receipt.warnings.clone());
        self.toast(lines, None);
        if receipt.id.is_some() {
            self.mark_read(id);
        }
        Ok(())
    }

    /// Responder o descartar una tarjeta de pedidos (`resolveBatch`): deja la
    /// lista en el clic y vuelve, con el error, si falla.
    pub(crate) fn resolve_batch(mut self, ask: AskInfo, answer: BatchAnswer, dismissed: bool) {
        self.submitted.write().insert(ask.id.clone());
        let (client, slug) = self.ids();
        self.spawn(async move {
            match client.resolve_batch(&slug, &ask.node, &answer).await {
                Ok(_) => self.toast(
                    vec![if dismissed { format!("dismissed {}'s question", ask.node) } else { format!("resolved {}'s batch", ask.node) }],
                    None,
                ),
                Err(error) => {
                    if let Ok(mut s) = self.submitted.try_write() {
                        s.remove(&ask.id);
                    }
                    self.toast(vec![format!("error: {error}")], None);
                }
            }
        });
    }

    /// "Dismiss with no comment" (`dismissAttention`): la fila se va en el
    /// clic; el motor baja la bandera y pasa el ticket a `blocked`.
    pub(crate) fn dismiss(mut self, item: WorkItem) {
        let Some(flag) = item.manual_attention.clone() else { return };
        self.dismissing.write().insert(item.slug.clone());
        let (client, slug) = self.ids();
        self.spawn(async move {
            match client.dismiss_attention(&slug, &item.slug, flag.set_rev).await {
                Ok(result) => {
                    let status = result.status.unwrap_or_default();
                    self.toast(vec![format!("dismissed the attention flag on “{}”", item.title), format!("status: {status}")], None);
                }
                Err(error) => {
                    if let Ok(mut d) = self.dismissing.try_write() {
                        d.remove(&item.slug);
                    }
                    self.toast(vec![format!("error: {error}")], None);
                }
            }
        });
    }

    /// La respuesta a un ticket (`replyWorkItem`): mail al dueño (o al
    /// participante elegido); la bandera baja sin cambiar el estado. El aviso
    /// dice lo que hizo el motor, no lo que se pidió (`notice`, `deferred`).
    pub(crate) async fn reply_ticket_to(self, item: &str, text: &str, to: Option<String>, notice: bool) -> Result<(), String> {
        let (client, slug) = self.ids();
        let reply = orgtree_engine_client::WorkReply { body: text.to_string(), to: to.clone(), notice };
        let result = client.reply_work_item_to(&slug, item, &reply).await.map_err(|e| e.to_string())?;
        let to = result.to.or(to).unwrap_or_else(|| "the assignee".to_string());
        let mut lines = vec![if result.deferred == Some(true) {
            format!("{to} is archived — the reply waits for rehire")
        } else if result.notice == Some(true) {
            format!("sent to {to} as a notice")
        } else {
            format!("sent to {to}")
        }];
        lines.extend(result.warnings);
        self.toast(lines, None);
        Ok(())
    }

    /// `GET …/work-items/{wid}`: el ticket entero, para su panel.
    pub(crate) async fn work_item(self, item: &str) -> Result<WorkItem, String> {
        let (client, slug) = self.ids();
        client.work_item(&slug, item).await.map_err(|e| e.to_string())
    }

    /// Relee solo la lista del docket (una casilla cambió). El resto llega
    /// con el árbol, por el WebSocket.
    pub(crate) fn reload_work(mut self) {
        let (client, slug) = self.ids();
        let (archived, backlog) = (*self.docket_archived.peek(), *self.docket_backlog.peek());
        self.spawn(async move {
            if let Ok(items) = client.work_items_view(&slug, archived, backlog).await {
                // otra casilla pudo cambiar mientras tanto: esa relectura gana
                if (*self.docket_archived.peek(), *self.docket_backlog.peek()) == (archived, backlog) {
                    let _ = self.work.try_write().map(|mut w| *w = Some(items));
                }
            }
        });
    }

    /// Abre el docket, con un ticket elegido si se pide (una referencia, la
    /// cola de atención). Si el ticket está en un grupo cerrado, prende su
    /// casilla, como `goToItem`.
    pub(crate) fn open_docket(mut self, item: Option<String>) {
        if let Some(slug) = &item {
            let place = self.work.peek().as_ref().and_then(|w| {
                w.extra.get("references").and_then(|r| r.as_array()).and_then(|refs| {
                    refs.iter().find(|r| r.get("slug").and_then(|s| s.as_str()) == Some(slug.as_str())).map(|r| {
                        (r.get("archived").and_then(|a| a.as_bool()).unwrap_or(false), r.get("status").and_then(|s| s.as_str()) == Some("backlogged"))
                    })
                })
            });
            let mut reload = false;
            if place.is_some_and(|p| p.0) && !*self.docket_archived.peek() {
                self.docket_archived.set(true);
                reload = true;
            } else if place.is_some_and(|p| !p.0 && p.1) && !*self.docket_backlog.peek() {
                self.docket_backlog.set(true);
                reload = true;
            }
            if reload {
                self.reload_work();
            }
        }
        if item.is_some() || !*self.docket_open.peek() {
            self.docket_sel.set(item);
        }
        self.docket_open.set(true);
    }

    pub(crate) fn close_docket(mut self) {
        self.docket_open.set(false);
    }

    pub(crate) fn toast(mut self, lines: Vec<String>, undo: Option<Undo>) {
        if lines.is_empty() {
            return;
        }
        // la vista pudo cerrarse mientras la operación corría: sin vista, sin aviso
        let Ok(mut toasts) = self.toasts.try_write() else { return };
        let id = toasts.iter().map(|t| t.id).max().unwrap_or(0) + 1;
        toasts.push(Toast { id, lines, undo });
        drop(toasts);
        self.spawn(async move {
            tokio::time::sleep(TOAST_FOR).await;
            if let Ok(mut toasts) = self.toasts.try_write() {
                toasts.retain(|t| t.id != id);
            }
        });
    }

    /// `op` de `App.tsx`: corre la operación y muestra sus advertencias, o el
    /// error como `error: …`. El árbol llega después por el WebSocket.
    fn op(self, request: OpRequest, done: impl FnOnce(Ctx, orgtree_engine_client::OpResult) + 'static) {
        let (client, slug) = (self.client.read().clone(), self.slug.read().clone());
        self.spawn(async move {
            match client.op(&slug, &request).await {
                Ok(result) => {
                    self.toast(result.warnings.clone(), None);
                    done(self, result);
                }
                Err(error) => self.toast(vec![format!("error: {error}")], None),
            }
        });
    }

    fn undo(self, undo: Undo) {
        match undo {
            Undo::Rehire(node) => self.op(OpRequest::on_node("rehire", &node), |_, _| {}),
            Undo::MoveBack { node, parent } => self.op(OpRequest::move_to(&node, parent.as_deref()), |_, _| {}),
        }
    }
}

/// `GET /api/orgs/{slug}`, con la bandeja y los tickets (#28). Un error de
/// una relectura no borra lo que ya se ve.
async fn load_tree(client: &Client, slug: &str, ctx: Ctx, mut sync: Signal<Sync>) {
    let (mut tree, mut inbox, mut work) = (ctx.tree, ctx.inbox, ctx.work);
    let (mut read_here, mut dismissing, mut submitted) = (ctx.read_here, ctx.dismissing, ctx.submitted);
    let (archived, backlog) = (*ctx.docket_archived.peek(), *ctx.docket_backlog.peek());
    let (result, mail, items) = tokio::join!(client.tree(slug), client.inbox(slug), client.work_items_view(slug, archived, backlog));
    let result = result.map_err(|e| e.to_string());
    sync.write().loads += 1;
    if result.is_ok() || !tree.peek().as_ref().is_some_and(|t| t.is_ok()) {
        tree.set(Some(result));
    }
    if let Ok(mail) = mail {
        // lo marcado acá que el motor ya archivó deja de hacer falta (`settleReadsFromBox`)
        let pending: HashSet<String> = mail.pending.iter().filter_map(|m| m.id.clone()).collect();
        read_here.write().retain(|id| pending.contains(id));
        inbox.set(Some(mail));
    }
    if let Ok(items) = items {
        let flagged: HashSet<String> = [items.attention.as_ref(), Some(&items.items)]
            .into_iter()
            .flatten()
            .flatten()
            .filter(|i| i.manual_attention.is_some())
            .map(|i| i.slug.clone())
            .collect();
        dismissing.write().retain(|slug| flagged.contains(slug));
        work.set(Some(items));
    }
    let open: Option<HashSet<String>> = match &*tree.peek() {
        Some(Ok(t)) => Some(crate::inbox::open_asks(t, &HashSet::new()).into_iter().map(|a| a.id).collect()),
        _ => None,
    };
    if let Some(open) = open {
        submitted.write().retain(|id| open.contains(id));
    }
    // la proyección de notificaciones también cambió
    crate::notify::bump();
}

#[component]
pub fn OrgView(slug: String) -> Element {
    let client = crate::engine_client();
    let mut route = use_context::<Signal<Route>>();
    let tree = use_signal(|| None::<Result<TreePayload, String>>);
    let mut sync = use_signal(Sync::default);
    let show_archived = use_signal(|| false);
    let ctx = Ctx {
        slug: use_signal(|| slug.clone()),
        client: use_signal(|| client.clone()),
        menu: use_signal(|| None),
        dialog: use_signal(|| None),
        toasts: use_signal(Vec::new),
        scope: dioxus::core::current_scope_id(),
        tree,
        inbox: use_signal(|| None),
        work: use_signal(|| None),
        tz: use_signal(|| None),
        read_here: use_signal(HashSet::new),
        submitted: use_signal(HashSet::new),
        dismissing: use_signal(HashSet::new),
        view: use_signal(|| View::Chart),
        inbox_open: use_signal(|| false),
        inbox_sel: use_signal(|| None),
        attn_sel: use_signal(|| None),
        docket_open: use_signal(|| false),
        docket_sel: use_signal(|| None),
        docket_archived: use_signal(|| false),
        docket_backlog: use_signal(|| false),
        settings_open: use_signal(|| false),
    };
    use_context_provider(|| ctx);
    let mut tz = ctx.tz;
    use_future(move || async move {
        if let Some(found) = crate::desk::webview_tz().await {
            tz.set(Some(found));
        }
    });
    // Un clic en una notificación (#28) abre su elemento acá.
    let focus = try_use_context::<Signal<Option<DesktopNotice>>>();
    let focus_slug = slug.clone();
    use_effect(move || {
        let Some(mut focus) = focus else { return };
        let Some(notice) = focus() else { return };
        if notice.org != focus_slug {
            return;
        }
        focus.set(None);
        open_notice(ctx, &notice, route);
    });
    // Los enlaces del Markdown de la bandeja y de la cola: como en el desk, el
    // clic se corta en la captura para que Dioxus no lo mande a `webbrowser::open`.
    use_future(move || async move {
        let mut eval = document::eval(
            "if (window.__dxAttnLinks) document.removeEventListener('click', window.__dxAttnLinks, true); \
             window.__dxAttnLinks = e => { \
               const a = e.target && e.target.closest && e.target.closest('.dx-inbox a[href], .dx-attn a[href], .dx-docket a[href]'); \
               if (!a) return; \
               e.preventDefault(); \
               e.stopPropagation(); \
               const item = a.getAttribute('data-ref-item'), agent = a.getAttribute('data-ref-agent'); \
               dioxus.send(item ? 'ref-item:' + item : agent ? 'ref-agent:' + agent : a.getAttribute('data-local-path') || ('link:' + a.getAttribute('href'))); \
             }; \
             document.addEventListener('click', window.__dxAttnLinks, true); \
             await new Promise(() => {});",
        );
        while let Ok(target) = eval.recv::<String>().await {
            // una referencia del docket (#29): un ticket se abre en el docket; un agente, en su desk
            if let Some(item) = target.strip_prefix("ref-item:") {
                ctx.open_docket(Some(item.to_string()));
                continue;
            }
            if let Some(node) = target.strip_prefix("ref-agent:") {
                route.set(Route::Desk { org: ctx.slug.peek().clone(), node: node.to_string() });
                continue;
            }
            match target.strip_prefix("link:") {
                // #30: solo http(s), por la vía controlada en Rust
                Some(href) => ctx.toast(vec![link_line(href)], None),
                None => match crate::reveal::reveal(&target) {
                    Ok(shown) => ctx.toast(vec![format!("Shown in folder: {}", shown.display())], None),
                    Err(why) => ctx.toast(vec![why], None),
                },
            }
        }
    });

    // El árbol, al abrir y después solo por el WebSocket de la org.
    use_future({
        let (client, slug) = (client.clone(), slug.clone());
        move || {
            let (client, slug) = (client.clone(), slug.clone());
            async move {
                load_tree(&client, &slug, ctx, sync).await;
                let mut events = client.subscribe(&slug, Backoff::default());
                let mut connected_once = false;
                while let Some(event) = events.recv().await {
                    // varios frames juntos: una sola relectura
                    let mut pending = vec![event];
                    while let Ok(more) = events.try_recv() {
                        pending.push(more);
                    }
                    let mut refresh = false;
                    for event in pending {
                        match event {
                            WsEvent::Connected => {
                                sync.write().connected = true;
                                // una reconexión pudo perder frames (`resetSync`)
                                refresh |= connected_once;
                                connected_once = true;
                            }
                            WsEvent::Disconnected { .. } => sync.write().connected = false,
                            WsEvent::Frame(Frame::Changed { .. } | Frame::NodeEvent { .. }) => {
                                sync.write().frames += 1;
                                refresh = true;
                            }
                            // texto en vivo y animaciones de mail: no cambian el árbol
                            WsEvent::Frame(_) => {}
                        }
                    }
                    if refresh {
                        load_tree(&client, &slug, ctx, sync).await;
                    }
                }
            }
        }
    });

    let s = sync();
    let (unread, _, asks) = ctx.counts();
    let attention_count = crate::attention::entries(&ctx).len();
    let body = match &*tree.read() {
        None => rsx! { p { class: "dim pad", "cargando…" } },
        Some(Err(error)) => rsx! { p { class: "dim pad", "{error}" } },
        Some(Ok(_)) if (ctx.view)() == View::Attention => rsx! {
            crate::attention::AttentionQueue {}
        },
        Some(Ok(payload)) => rsx! {
            OrgTree { payload: payload.clone(), show_archived }
            if let Some(menu) = (ctx.menu)() {
                AgentMenu { payload: payload.clone(), menu }
            }
            if let Some(dialog) = (ctx.dialog)() {
                DialogView { payload: payload.clone(), dialog }
            }
        },
    };
    rsx! {
        div { class: "dx-org-view", "data-loads": "{s.loads}", "data-frames": "{s.frames}",
            "data-connected": "{s.connected}", "data-unread": "{unread}", "data-asks": "{asks}", "data-attention": "{attention_count}",
            header { class: "orgbar native-header dx-orgbar",
                div { class: "native-header-main",
                    button { class: "home", onclick: move |_| route.set(Route::Home), HomeIcon {} " All organizations" }
                    span { class: "orgname-wrap",
                        h2 { {tree.read().as_ref().and_then(|t| t.as_ref().ok()).map(|t| t.name.clone()).unwrap_or_else(|| slug.clone())} }
                        span { class: if s.connected || s.frames == 0 && s.loads <= 1 { "chip bad conn-chip" } else { "chip bad conn-chip show" },
                            span { class: "conn-chip-text", "reconnecting…" }
                        }
                    }
                    if let Some(Ok(payload)) = &*tree.read() {
                        OrgBar { payload: payload.clone() }
                    }
                    span { style: "flex: 1" }
                    ViewToggle { attention: attention_count }
                    crate::docket::DocketBell {}
                    crate::inbox::InboxBell {}
                    button { class: "iconbtn dx-org-settings-open", title: "Org settings", "aria-label": "Org settings",
                        onclick: move |_| { let mut open = ctx.settings_open; open.set(true) },
                        crate::icons::SettingsIcon {}
                    }
                }
                crate::native::WindowControls {}
            }
            div { class: "dx-org-body", {body} }
            if (ctx.inbox_open)() {
                crate::inbox::InboxPanel {}
            }
            if (ctx.docket_open)() {
                crate::docket::DocketPanel {}
            }
            if (ctx.settings_open)() {
                if let Some(Ok(payload)) = &*tree.read() {
                    crate::orgsettings::OrgSettingsPanel { payload: payload.clone() }
                }
            }
            Toasts {}
        }
    }
}

/// `OrgViewToggle` (attention/AttentionView.tsx): el organigrama o la cola
/// de atención, con lo que espera en la cola.
#[component]
fn ViewToggle(attention: usize) -> Element {
    let mut ctx = use_context::<Ctx>();
    let view = (ctx.view)();
    rsx! {
        div { class: "orgview-toggle", role: "group", "aria-label": "Organization view",
            button { r#type: "button", class: if view == View::Chart { "orgview-tab sel" } else { "orgview-tab" },
                "data-view": "chart", onclick: move |_| ctx.view.set(View::Chart),
                span { class: "orgview-label", "Chart" }
            }
            button { r#type: "button", class: if view == View::Attention { "orgview-tab sel" } else { "orgview-tab" },
                "data-view": "attention", onclick: move |_| ctx.view.set(View::Attention),
                span { class: "orgview-label", "Attention" }
                if attention > 0 {
                    " "
                    span { class: "tab-count", "{attention}" }
                }
            }
        }
    }
}

/// El clic en una notificación (`onNotificationFocus` y `revealOrgItem`):
/// una pregunta, un ticket con bandera o un mail urgente se abren en la cola
/// de atención; el resto del mail, en la bandeja; un agente congelado, en su desk.
fn open_notice(mut ctx: Ctx, notice: &DesktopNotice, mut route: Signal<Route>) {
    let source = notice.source_id.clone().unwrap_or_else(|| notice.id.clone());
    match notice.kind.as_str() {
        "question" | "work-attention" | "urgent-mail" => {
            let key = match notice.kind.as_str() {
                "question" => format!("question:{source}"),
                "work-attention" => format!("ticket:{}", notice.item.clone().unwrap_or_default()),
                _ => format!("mail:{source}"),
            };
            ctx.inbox_open.set(false);
            ctx.view.set(View::Attention);
            ctx.attn_sel.set(Some(key));
        }
        "routine" | "terminal-failure" => {
            ctx.inbox_sel.set(Some(format!("mail:{source}")));
            ctx.inbox_open.set(true);
        }
        "agent-frozen" => {
            if let Some(agent) = notice.agent.clone() {
                route.set(Route::Desk { org: notice.org.clone(), node: agent });
            }
        }
        _ => {}
    }
}

/// Las fichas de la barra de la org (`orgbar` de `App.tsx`): la autoauditoría
/// si encuentra algo, los agentes vivos y activos por tier (`ActiveAgentSummary`),
/// el gasto, los créditos (`circulation · seats · free`, la barra del ojo en
/// `cards.tsx`) y el killswitch trabado.
#[component]
fn OrgBar(payload: TreePayload) -> Element {
    let nodes: Vec<&TreeNode> = payload.nodes().into_iter().filter(|n| n.state == NodeState::Live).collect();
    let live = nodes.len();
    let active = nodes.iter().filter(|n| n.busy == Some(true)).count();
    let mut by_tier: BTreeMap<&str, usize> = BTreeMap::new();
    for node in &nodes {
        *by_tier.entry(node.tier.as_str()).or_default() += 1;
    }
    let mut tiers: Vec<(String, usize)> =
        ALL_TIERS.iter().filter_map(|t| by_tier.get(t).map(|n| (t.to_string(), *n))).collect();
    tiers.extend(by_tier.iter().filter(|(t, _)| t.starts_with("or-")).map(|(t, n)| (t.to_string(), *n)));
    // `orgStats` de OrgCanvas.tsx: la circulación es lo que tienen los agentes
    // de primer nivel; lo libre es la suma de `free` de los vivos.
    let circulation = payload.audit.as_ref().map(|a| a.top_level_holds).unwrap_or(0.0);
    let free: f64 = nodes.iter().filter_map(|n| n.free).filter(|f| *f > 0.0).sum();
    let seats = circulation - free;
    let problems = payload.audit.as_ref().filter(|a| !a.no_overdraft).map(|a| a.problems.join(", "));
    let cost = (payload.cost_usd_total > 0.0 || payload.cost_usd_unknown == Some(true))
        .then(|| format!("${:.2}", payload.cost_usd_total));
    let summary = format!("{live} live{}", if active > 0 { format!(" · {active} active") } else { String::new() });
    rsx! {
        div { class: "bar-detail",
            if let Some(problems) = problems {
                span { class: "chip bad", "⚠ {problems}" }
            }
            span { class: "chip agents", title: "{payload.name}: {live} live agents, {active} active now (a turn running)",
                "{summary}"
                for (tier, n) in tiers {
                    b { key: "{tier}", class: "t-{tier}", "{tier_letter(&tier)}{n}" }
                }
            }
            if let Some(cost) = cost {
                span { class: "chip", title: "total spend", "{cost}" }
            }
            span { class: "chip dx-credits", title: "credits held by your top-level agents",
                "circulation " b { class: "n-fill", "{credits(circulation)}" }
                " · seats " b { class: "n-seat", "{credits(seats)}" }
                " · free " b { class: "n-free", "{credits(free)}" }
            }
            if payload.killswitch.as_ref().is_some_and(|k| !k.is_null()) {
                span { class: "chip bad", "STOP ALL latched" }
            }
        }
    }
}

/// El árbol: la raíz (el usuario) y los agentes indentados por `parent`.
#[component]
fn OrgTree(payload: TreePayload, show_archived: Signal<bool>) -> Element {
    let mut ctx = use_context::<Ctx>();
    let archived = payload.nodes().iter().filter(|n| n.state != NodeState::Live).count();
    let show = show_archived();
    let roots: Vec<TreeNode> = payload.roots.clone();
    let any_visible = payload.nodes().iter().any(|n| show || n.state == NodeState::Live);
    rsx! {
        div { class: "dx-tree",
            div { class: "node dx-user",
                div { class: "card dx-user-card", "data-node": "@user",
                    oncontextmenu: move |e: MouseEvent| {
                        e.prevent_default();
                        let p = e.client_coordinates();
                        ctx.menu.set(Some(Menu { target: Target::User, x: p.x, y: p.y }));
                    },
                    span { class: "tier", "◉" }
                    span { class: "name", "you" }
                    span { class: "dim", "top of the org — unlimited credit" }
                    span { style: "flex: 1" }
                    if archived > 0 {
                        button { class: "tray-arch", r#type: "button",
                            onclick: move |e| { e.stop_propagation(); show_archived.toggle() },
                            if show { "▾ hide {archived} archived" } else { "▸ show {archived} archived" }
                        }
                    }
                    button { class: "dx-row-menu", r#type: "button", title: "actions", "aria-label": "actions for you",
                        onclick: move |e: MouseEvent| {
                            e.stop_propagation();
                            let p = e.client_coordinates();
                            ctx.menu.set(Some(Menu { target: Target::User, x: p.x, y: p.y }));
                        },
                        "⋯"
                    }
                }
                div { class: "kids",
                    for node in roots {
                        AgentNode { key: "{node.id}", node, show_archived: show }
                    }
                    if !any_visible {
                        div { class: "dim pad dx-empty", "no agents yet — right-click “you” to hire one" }
                    }
                }
            }
        }
    }
}

/// Un agente y sus subordinados. Clic: abre el desk. Clic derecho o `⋯`: el menú.
#[component]
fn AgentNode(node: TreeNode, show_archived: bool) -> Element {
    let mut ctx = use_context::<Ctx>();
    let mut route = use_context::<Signal<Route>>();
    let live = node.state == NodeState::Live;
    let hidden = !live && !show_archived;
    let st = status(&node);
    let (provider, provider_label) = provider_of(&node.tier);
    let id = node.id.clone();
    let children = node.children.clone();
    if hidden {
        // un retirado plegado: sus subordinados vivos (si los hubiera) siguen a la vista
        return rsx! {
            for child in children {
                AgentNode { key: "{child.id}", node: child, show_archived }
            }
        };
    }
    let open = {
        let id = id.clone();
        move |_| {
            let org = ctx.slug.read().clone();
            route.set(Route::Desk { org, node: id.clone() })
        }
    };
    let open_menu = {
        let id = id.clone();
        move |e: MouseEvent| {
            e.prevent_default();
            e.stop_propagation();
            let p = e.client_coordinates();
            ctx.menu.set(Some(Menu { target: Target::Agent(id.clone()), x: p.x, y: p.y }));
        }
    };
    let state_class = match node.state {
        NodeState::Live => "",
        NodeState::Unrecoverable => " unrecoverable",
        _ => " archived",
    };
    let model = if node.model_id.is_empty() { node.tier.clone() } else { node.model_id.clone() };
    let summary = node.last_status.as_ref().and_then(|s| s.summary.clone().map(|sum| format!("{}: {sum}", s.status)));
    rsx! {
        div { class: "node{state_class}",
            div { class: "card dx-agent prov-{provider}", "data-node": "{id}", "data-status": "{st:?}",
                role: "button", tabindex: 0, title: "open {id}'s desk",
                onclick: open,
                oncontextmenu: open_menu.clone(),
                span { class: "tier t-{node.tier}", "{tier_letter(&node.tier)}" }
                div { class: "dx-agent-main",
                    div { class: "dx-agent-line",
                        span { class: "name", "{id}" }
                        StatusBadge { st, node: node.clone() }
                    }
                    if let Some(summary) = summary {
                        div { class: "dx-agent-sum dim", title: "{summary}", "{summary}" }
                    }
                }
                div { class: "badges",
                    span { class: "badge prov-{provider}", title: "{provider_label} · {node.tier}", "{model}" }
                    if let Some(seat) = node.seat {
                        span { class: "badge", title: "seat credits", "seat {credits(seat)}" }
                    }
                    if node.grant.is_some_and(|g| g > 0.0) {
                        span { class: "badge", title: "credits granted to fund its reports",
                            "grant {credits(node.grant.unwrap_or(0.0))}"
                        }
                    }
                    if node.free.is_some_and(|f| f > 0.0) {
                        span { class: "badge free", title: "unallocated grant", "free {credits(node.free.unwrap_or(0.0))}" }
                    }
                }
                button { class: "dx-row-menu", r#type: "button", title: "actions", "aria-label": "actions for {id}",
                    onclick: open_menu,
                    "⋯"
                }
            }
            if !children.is_empty() {
                div { class: "kids",
                    for child in children {
                        AgentNode { key: "{child.id}", node: child, show_archived }
                    }
                }
            }
        }
    }
}

/// El estado de la fila, con las clases de `TrayStatus` y `HaltStatus`.
#[component]
fn StatusBadge(st: Status, node: TreeNode) -> Element {
    match st {
        Status::Halted => rsx! {
            span { class: "badge halted", role: "status", title: "No turn can run. Mail stays unread until explicit unhalt", "Halted" }
        },
        Status::Halting => rsx! {
            span { class: "badge halted", role: "status", title: "Turn admission is blocked; the active turn is still ending", "Halting…" }
        },
        Status::Frozen => rsx! { span { class: "badge frozen", title: "frozen (usage limit or network)", "Frozen" } },
        Status::Retired => {
            let state = format!("{:?}", node.state).to_lowercase();
            rsx! { span { class: "dim dx-retired", "{state}" } }
        }
        Status::Compacting => rsx! {
            span { class: "tray-status", span { class: "tray-status-label compacting", "Compacting" } }
        },
        Status::Queued => rsx! {
            span { class: "tray-status",
                span { class: "statusdot waiting", title: "queued — waiting for a free turn slot" }
                span { class: "tray-status-label waiting", title: "queued — waiting for a free turn slot", "Queued" }
            }
        },
        Status::Working => rsx! {
            span { class: "tray-status",
                span { class: "statusdot working" }
                span { class: "tray-status-label working active", "Active" }
            }
        },
        Status::Idle => {
            let recorded = node.last_status.as_ref().map(|s| s.status.clone()).filter(|s| !s.is_empty()).unwrap_or_else(|| "idle".into());
            let label = {
                let w = recorded.replace('_', " ");
                let mut c = w.chars();
                c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default()
            };
            rsx! {
                span { class: "tray-status",
                    span { class: "statusdot {recorded}" }
                    span { class: "tray-status-label {recorded}", "{label}" }
                }
            }
        }
    }
}

/// Una entrada del menú (`MenuEntry` de `contextmenu.tsx`).
#[derive(Clone, PartialEq)]
enum Entry {
    Item { label: String, title: Option<String>, danger: bool, disabled: bool, action: Action },
    Sep,
}

#[derive(Clone, PartialEq, Debug)]
enum Action {
    OpenDesk(String),
    Hire(Option<String>),
    Move(String),
    Halt(String),
    Unhalt(String),
    Interrupt(String),
    Retire { node: String, dissolve: bool },
    Rehire(String),
}

fn item(label: &str, action: Action) -> Entry {
    Entry::Item { label: label.into(), title: None, danger: false, disabled: false, action }
}

/// Las entradas del menú, en el orden de `agentMenuEntries` (`agentmenu.tsx`),
/// recortadas a lo que este recorte ofrece. La detención (`HaltControl`) y la
/// interrupción (el STOP del desk) viven en el desk del renderer; acá van al
/// menú, con las mismas condiciones.
fn entries(payload: &TreePayload, target: &Target) -> Vec<Entry> {
    let Target::Agent(id) = target else {
        // el menú del ojo (`cards.tsx`): contratar en el primer nivel
        return vec![Entry::Item {
            label: "Hire a top-level agent…".into(),
            title: Some("hire an agent that reports to you".into()),
            danger: false,
            disabled: false,
            action: Action::Hire(None),
        }];
    };
    let nodes = payload.nodes();
    let Some(node) = nodes.iter().find(|n| &n.id == id) else { return vec![] };
    let live = node.state == NodeState::Live;
    let live_kids = node.children.iter().any(|c| c.state == NodeState::Live);
    let mut out = vec![item("Open desk", Action::OpenDesk(id.clone()))];
    if live {
        out.push(Entry::Item {
            label: "Hire a subordinate…".into(),
            title: Some(format!("hire an agent that reports to {id}")),
            danger: false,
            disabled: false,
            action: Action::Hire(Some(id.clone())),
        });
        out.push(Entry::Item {
            label: "Move to…".into(),
            title: Some(format!("choose who {id} reports to")),
            danger: false,
            disabled: false,
            action: Action::Move(id.clone()),
        });
        out.push(Entry::Sep);
        let running = node.responding == Some(true) || node.busy == Some(true);
        // №3 del desk: STOP solo cuando una interrupción puede llegar
        out.push(Entry::Item {
            label: "Interrupt".into(),
            title: Some(if running { "interrupt the current response".into() } else { "no turn is running".into() }),
            danger: false,
            disabled: !running,
            action: Action::Interrupt(id.clone()),
        });
        let halt = node.halt.as_ref().map(|h| h.phase.as_str());
        out.push(match halt {
            Some("halted") => Entry::Item {
                label: "Unhalt".into(),
                title: Some("Allow pending work to resume".into()),
                danger: false,
                disabled: false,
                action: Action::Unhalt(id.clone()),
            },
            Some(_) => Entry::Item {
                label: "Finish halt".into(),
                title: Some("Check that the active turn has fully ended".into()),
                danger: true,
                disabled: false,
                action: Action::Halt(id.clone()),
            },
            None => Entry::Item {
                label: "Halt".into(),
                title: Some("Abruptly end this turn and block every wake until explicit unhalt".into()),
                danger: true,
                disabled: false,
                action: Action::Halt(id.clone()),
            },
        });
        out.push(Entry::Sep);
        out.push(Entry::Item {
            label: if live_kids { "Dissolve suborganization…".into() } else { "Retire…".into() },
            title: None,
            danger: true,
            disabled: false,
            action: Action::Retire { node: id.clone(), dissolve: live_kids },
        });
    } else {
        out.push(Entry::Sep);
        out.push(Entry::Item {
            label: "Rehire".into(),
            title: Some(format!("bring {id} back exactly as it was")),
            danger: false,
            disabled: false,
            action: Action::Rehire(id.clone()),
        });
    }
    out
}

#[component]
fn AgentMenu(payload: TreePayload, menu: Menu) -> Element {
    let mut ctx = use_context::<Ctx>();
    let mut route = use_context::<Signal<Route>>();
    let list = entries(&payload, &menu.target);
    let mut run = move |action: Action| {
        ctx.menu.set(None);
        let (client, slug) = (ctx.client.read().clone(), ctx.slug.read().clone());
        match action {
            Action::OpenDesk(node) => route.set(Route::Desk { org: slug, node }),
            Action::Hire(parent) => ctx.dialog.set(Some(Dialog::Hire { parent })),
            Action::Move(node) => ctx.dialog.set(Some(Dialog::Move { node })),
            Action::Retire { node, dissolve } => ctx.dialog.set(Some(Dialog::Retire { node, dissolve })),
            Action::Rehire(node) => ctx.op(OpRequest::on_node("rehire", &node), move |ctx, _| {
                ctx.toast(vec![format!("{node} rehired")], None)
            }),
            // HaltControl (haltcontrol.tsx): sin confirmación, el estado como aviso
            Action::Halt(node) => {
                ctx.spawn(async move {
                    match client.halt(&slug, &node).await {
                        Ok(r) => ctx.toast(vec![r.status], None),
                        Err(e) => ctx.toast(vec![format!("error: {e}")], None),
                    }
                });
            }
            Action::Unhalt(node) => {
                ctx.spawn(async move {
                    match client.unhalt(&slug, &node).await {
                        Ok(r) if r.unhalted => ctx.toast(vec![format!("{node} unhalted; pending work may resume")], None),
                        Ok(r) => ctx.toast(vec![r.status.unwrap_or_else(|| "Already unhalted".into())], None),
                        Err(e) => ctx.toast(vec![format!("error: {e}")], None),
                    }
                });
            }
            // el STOP del desk (desk.tsx): solo avisa si no llegó
            Action::Interrupt(node) => {
                ctx.spawn(async move {
                    match client.interrupt(&slug, &node).await {
                        Ok(r) if !r.interrupted => ctx.toast(vec![format!("error: {}", r.reason.unwrap_or_default())], None),
                        Ok(_) => {}
                        Err(e) => ctx.toast(vec![format!("error: {e}")], None),
                    }
                });
            }
        }
    };
    let style = format!("left: {}px; top: {}px;", menu.x, menu.y);
    rsx! {
        // un clic fuera cierra el menú
        div { class: "dx-menu-scrim", onclick: move |_| ctx.menu.set(None),
            oncontextmenu: move |e: MouseEvent| { e.prevent_default(); ctx.menu.set(None) },
        }
        div { class: "ctxmenu", role: "menu", tabindex: -1, style,
            onclick: move |e| e.stop_propagation(),
            for (i, entry) in list.into_iter().enumerate() {
                match entry {
                    Entry::Sep => rsx! { div { key: "{i}", class: "ctxmenu-sep", role: "separator" } },
                    Entry::Item { label, title, danger, disabled, action } => rsx! {
                        button { key: "{i}", r#type: "button", role: "menuitem",
                            class: if danger { "ctxmenu-item danger" } else { "ctxmenu-item" },
                            disabled, title: title.unwrap_or_default(),
                            onclick: move |_| run(action.clone()),
                            "{label}"
                        }
                    },
                }
            }
        }
    }
}

/// Los diálogos, con la caja de `ConfirmModal` (`modals.tsx`).
#[component]
fn DialogView(payload: TreePayload, dialog: Dialog) -> Element {
    let mut ctx = use_context::<Ctx>();
    let close = move |_| ctx.dialog.set(None);
    let content = match dialog {
        Dialog::Hire { parent } => rsx! { HireForm { payload, parent } },
        Dialog::Move { node } => rsx! { MoveForm { payload, node } },
        Dialog::Retire { node, dissolve } => {
            let found = payload.nodes().into_iter().find(|n| n.id == node).cloned();
            let (title, body, label) = if dissolve {
                (
                    format!("dissolve {node}?"),
                    "Its entire suborganization is retired with it. Context is kept; rehire brings nodes back.".to_string(),
                    "dissolve",
                )
            } else {
                let freed = found.as_ref().map(|n| n.seat.unwrap_or(0.0) + n.grant.unwrap_or(0.0)).unwrap_or(0.0);
                let busy = found.as_ref().is_some_and(|n| n.busy == Some(true));
                (
                    format!("retire {node}?"),
                    format!(
                        "It stops working and frees {} credit(s) back to its superior. Its context is KEPT — rehire brings it back exactly as it was.{}",
                        credits(freed),
                        if busy { " ⚠ It is mid-turn right now; that turn is cut off." } else { "" }
                    ),
                    "retire",
                )
            };
            let confirm = move |_| {
                ctx.dialog.set(None);
                let node = node.clone();
                if dissolve {
                    ctx.op(OpRequest::on_node("dissolve", &node), |_, _| {});
                } else {
                    // el aviso con deshacer de `AgentRetireConfirm`
                    ctx.op(OpRequest::on_node("retire", &node), move |ctx, _| {
                        ctx.toast(vec![format!("{node} retired")], Some(Undo::Rehire(node)))
                    });
                }
            };
            rsx! {
                div { class: "settings content-height confirm-box dx-confirm", role: "dialog", "aria-modal": "true",
                    onclick: move |e| e.stop_propagation(),
                    h3 { "{title}" }
                    div { class: "confirm-body", "{body}" }
                    div { class: "row",
                        button { class: "danger solid", onclick: confirm, "{label}" }
                        button { onclick: close, "cancel" }
                    }
                }
            }
        }
    };
    rsx! {
        div { class: "overlay", onclick: close, {content} }
    }
}

/// Contratar: tier, nombre, créditos y charter, como el borrador de
/// `cards.tsx` (`confirmDraft` de OrgCanvas.tsx manda `{op:'hire', parent,
/// tier, grant, name, charter}`).
#[component]
fn HireForm(payload: TreePayload, parent: Option<String>) -> Element {
    let mut ctx = use_context::<Ctx>();
    // #30: los tiers según los proveedores de la máquina (`/api/providers`),
    // como `familyOffer`: un proveedor no instalado o apagado no aparece; uno
    // instalado sin poder contratar aparece deshabilitado con su motivo. Hasta
    // que llega la respuesta (o si falla) se ofrece todo, como el renderer: el
    // motor rechaza igual en la puerta (`provider_hire_gate`).
    let mut providers = use_signal(|| None::<ProvidersPayload>);
    use_hook(move || {
        let (client, _) = ctx.ids();
        ctx.spawn(async move {
            if let Ok(payload) = client.providers().await {
                crate::settings::providers_known(&payload);
                let _ = providers.try_write().map(|mut p| *p = Some(payload));
            }
        });
    });
    let offer = |tier: &str| -> (Offer, Option<String>) {
        let known = providers.read();
        match known.as_ref().and_then(|p| p.get(provider_of(tier).0)) {
            Some(info) => (info.offer(), info.reason.clone()),
            None => (Offer::Offer, None),
        }
    };
    let mut all: Vec<String> = ALL_TIERS.iter().filter(|t| payload.tiers.contains_key(**t)).map(|t| t.to_string()).collect();
    all.extend(payload.tiers.keys().filter(|t| !ALL_TIERS.contains(&t.as_str())).cloned());
    let shown: Vec<(String, Offer, Option<String>)> =
        all.iter().map(|t| (t.clone(), offer(t))).filter(|(_, (o, _))| *o != Offer::Hide).map(|(t, (o, r))| (t, o, r)).collect();
    let tiers: Vec<String> = shown.iter().filter(|(_, o, _)| *o == Offer::Offer).map(|(t, ..)| t.clone()).collect();
    let default_grant = if parent.is_none() {
        payload.extra.get("default_top_grant").and_then(|v| v.as_u64()).unwrap_or(0)
    } else {
        0
    };
    let mut tier = use_signal(|| if tiers.iter().any(|t| t == "haiku") { "haiku".to_string() } else { tiers.first().cloned().unwrap_or_default() });
    // la elección tiene que seguir entre lo ofrecido cuando llegan los proveedores
    if !tiers.is_empty() && !tiers.contains(&tier.peek()) {
        tier.set(if tiers.iter().any(|t| t == "haiku") { "haiku".to_string() } else { tiers[0].clone() });
    }
    let mut name = use_signal(String::new);
    let mut grant = use_signal(move || default_grant.to_string());
    let mut charter = use_signal(String::new);
    let title = match &parent {
        Some(p) => format!("hire under {p}"),
        None => "hire a top-level agent".to_string(),
    };
    let seat = |t: &str| payload.tiers.get(t).and_then(|v| v.as_f64()).map(credits).unwrap_or_default();
    let ok = !name().trim().is_empty() && grant().trim().parse::<u64>().is_ok() && tiers.contains(&tier());
    let submit = move |e: FormEvent| {
        e.prevent_default();
        let Ok(credits) = grant().trim().parse::<u64>() else { return };
        let request = OpRequest::hire(parent.as_deref(), &tier(), name().trim(), credits, Some(&charter()));
        ctx.dialog.set(None);
        ctx.op(request, |ctx, result| {
            if let Some(born) = result.node {
                ctx.toast(vec![format!("{born} hired")], None);
            }
        });
    };
    rsx! {
        form { class: "settings content-height confirm-box dx-hire", role: "dialog", "aria-modal": "true",
            onclick: move |e| e.stop_propagation(),
            onsubmit: submit,
            h3 { "{title}" }
            label { class: "field-label", r#for: "dx-hire-tier", "model" }
            select { id: "dx-hire-tier", value: "{tier}", "data-providers": if providers.read().is_some() { "known" } else { "pending" },
                onchange: move |e| tier.set(e.value()),
                for (t, o, reason) in shown.iter() {
                    option { key: "{t}", value: "{t}", selected: *t == tier(), disabled: *o == Offer::Disable,
                        "data-provider": "{provider_of(t).0}", title: reason.clone().unwrap_or_default(),
                        {tier_option(t, &seat(t), *o == Offer::Disable)} }
                }
            }
            if shown.is_empty() && providers.read().is_some() {
                p { class: "ask-warn dx-hire-none", "No provider can hire on this machine. Install and sign in to Claude Code, Codex or Antigravity (App settings → Providers)." }
            }
            label { class: "field-label", r#for: "dx-hire-name", "name" }
            input { id: "dx-hire-name", placeholder: "name…", value: "{name}", autofocus: true,
                oninput: move |e| name.set(e.value()) }
            label { class: "field-label", r#for: "dx-hire-grant", "grant (credits for its reports)" }
            input { id: "dx-hire-grant", r#type: "number", min: "0", step: "1", value: "{grant}",
                oninput: move |e| grant.set(e.value()) }
            label { class: "field-label", r#for: "dx-hire-charter", "charter (optional)" }
            textarea { id: "dx-hire-charter", rows: 3, value: "{charter}", oninput: move |e| charter.set(e.value()) }
            div { class: "row",
                button { class: "primary", r#type: "submit", disabled: !ok, "hire" }
                button { r#type: "button", onclick: move |_| ctx.dialog.set(None), "cancel" }
            }
        }
    }
}

/// El texto de una opción de tier, con el motivo de una deshabilitada.
fn tier_option(tier: &str, seat: &str, disabled: bool) -> String {
    let base = format!("{tier} · {} · seat {seat}", provider_of(tier).1);
    if disabled {
        format!("{base} · unavailable")
    } else {
        base
    }
}

/// Un enlace del contenido de los agentes (#30): `http(s)` se abre en el
/// navegador por la vía controlada (`external`); lo demás se muestra como texto.
pub(crate) fn link_line(href: &str) -> String {
    match crate::external::open(href) {
        Ok(url) => format!("Opened in the browser: {url}"),
        Err(why) => format!("Link: {href} — {why}"),
    }
}

/// Mover: elegir el nuevo superior. El renderer lo hace arrastrando la tarjeta
/// y avisa con deshacer (`{op:'move', node, new_parent}` de vuelta).
#[component]
fn MoveForm(payload: TreePayload, node: String) -> Element {
    let mut ctx = use_context::<Ctx>();
    let nodes = payload.nodes();
    let Some(me) = nodes.iter().find(|n| n.id == node).copied() else { return rsx! {} };
    // el superior actual, y los que no pueden serlo: el propio subárbol (ciclo)
    let mut subtree = Vec::new();
    me.walk(&mut subtree);
    let parent = nodes.iter().find(|n| n.children.iter().any(|c| c.id == node)).map(|n| n.id.clone());
    let options: Vec<String> = nodes
        .iter()
        .filter(|n| n.state == NodeState::Live && !subtree.iter().any(|s| s.id == n.id))
        .map(|n| n.id.clone())
        .collect();
    let current = parent.clone().unwrap_or_default();
    let mut choice = use_signal(|| current.clone());
    let changed = choice() != current;
    let submit = move |e: FormEvent| {
        e.prevent_default();
        let to = choice();
        let target = (!to.is_empty()).then_some(to.clone());
        let (node, back) = (node.clone(), parent.clone());
        ctx.dialog.set(None);
        ctx.op(OpRequest::move_to(&node, target.as_deref()), move |ctx, _| {
            let who = if to.is_empty() { "you".to_string() } else { to };
            ctx.toast(vec![format!("{node} now reports to {who}")], Some(Undo::MoveBack { node, parent: back }))
        });
    };
    rsx! {
        form { class: "settings content-height confirm-box dx-move", role: "dialog", "aria-modal": "true",
            onclick: move |e| e.stop_propagation(),
            onsubmit: submit,
            h3 { "move {me.id}" }
            div { class: "confirm-body", "Its whole team moves with it. Its scope is clamped to the new superior's." }
            label { class: "field-label", r#for: "dx-move-to", "reports to" }
            select { id: "dx-move-to", value: "{choice}", onchange: move |e| choice.set(e.value()),
                option { value: "", selected: choice().is_empty(), "you (top level)" }
                for id in options {
                    option { key: "{id}", value: "{id}", selected: choice() == id, "{id}" }
                }
            }
            div { class: "row",
                button { class: "primary", r#type: "submit", disabled: !changed, "move" }
                button { r#type: "button", onclick: move |_| ctx.dialog.set(None), "cancel" }
            }
        }
    }
}

/// Los avisos (`.toasts` de `App.tsx`): clic para cerrar, botón de deshacer.
#[component]
fn Toasts() -> Element {
    let mut ctx = use_context::<Ctx>();
    let list = (ctx.toasts)();
    rsx! {
        div { class: "toasts",
            for toast in list {
                div { key: "{toast.id}", class: "toast",
                    onclick: move |_| ctx.toasts.write().retain(|t| t.id != toast.id),
                    for line in toast.lines.iter() {
                        div { "{line}" }
                    }
                    if let Some(undo) = toast.undo.clone() {
                        button { class: "toast-undo",
                            onclick: move |e| {
                                e.stop_propagation();
                                ctx.toasts.write().retain(|t| t.id != toast.id);
                                ctx.undo(undo.clone());
                            },
                            "undo"
                        }
                    }
                }
            }
        }
    }
}
