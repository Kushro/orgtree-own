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
use orgtree_engine_client::{Backoff, Client, Frame, NodeState, OpRequest, TreeNode, TreePayload, WsEvent};
use std::collections::BTreeMap;
use std::time::Duration;

/// Orden de los tiers como `ALL_TIERS` de `canvas/shared.ts`.
const ALL_TIERS: [&str; 12] =
    ["haiku", "sonnet", "opus", "fable", "gpt-reserve", "luna", "terra", "sol", "astra", "flash", "pro", "argon"];
/// El aviso con deshacer dura 12 s, como en el renderer.
const TOAST_FOR: Duration = Duration::from_secs(12);

/// `TIER_LETTER` de `canvas/shared.ts`.
fn tier_letter(tier: &str) -> &'static str {
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
fn provider_of(tier: &str) -> (&'static str, &'static str) {
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
enum Undo {
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

/// Todo lo que el árbol, el menú y los diálogos comparten.
#[derive(Clone, Copy)]
struct Ctx {
    slug: Signal<String>,
    client: Signal<Client>,
    menu: Signal<Option<Menu>>,
    dialog: Signal<Option<Dialog>>,
    toasts: Signal<Vec<Toast>>,
    /// El scope de la vista: las tareas de un menú o un diálogo viven acá,
    /// porque el menú o el diálogo se desmontan apenas se elige la acción
    /// (y `spawn` ata la tarea al componente que la lanza).
    scope: ScopeId,
}

impl Ctx {
    fn spawn(self, task: impl std::future::Future<Output = ()> + 'static) {
        dioxus::core::Runtime::current().spawn(self.scope, task);
    }

    fn toast(mut self, lines: Vec<String>, undo: Option<Undo>) {
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

/// `GET /api/orgs/{slug}`. Un error de una relectura no borra el árbol que ya se ve.
async fn load_tree(client: &Client, slug: &str, mut tree: Signal<Option<Result<TreePayload, String>>>, mut sync: Signal<Sync>) {
    let result = client.tree(slug).await.map_err(|e| e.to_string());
    sync.write().loads += 1;
    if result.is_ok() || !tree.peek().as_ref().is_some_and(|t| t.is_ok()) {
        tree.set(Some(result));
    }
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
    };
    use_context_provider(|| ctx);

    // El árbol, al abrir y después solo por el WebSocket de la org.
    use_future({
        let (client, slug) = (client.clone(), slug.clone());
        move || {
            let (client, slug) = (client.clone(), slug.clone());
            async move {
                load_tree(&client, &slug, tree, sync).await;
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
                        load_tree(&client, &slug, tree, sync).await;
                    }
                }
            }
        }
    });

    let s = sync();
    let body = match &*tree.read() {
        None => rsx! { p { class: "dim pad", "cargando…" } },
        Some(Err(error)) => rsx! { p { class: "dim pad", "{error}" } },
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
            "data-connected": "{s.connected}",
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
                }
                crate::native::WindowControls {}
            }
            div { class: "dx-org-body", {body} }
            Toasts {}
        }
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
    let mut tiers: Vec<String> = ALL_TIERS.iter().filter(|t| payload.tiers.contains_key(**t)).map(|t| t.to_string()).collect();
    tiers.extend(payload.tiers.keys().filter(|t| !ALL_TIERS.contains(&t.as_str())).cloned());
    let default_grant = if parent.is_none() {
        payload.extra.get("default_top_grant").and_then(|v| v.as_u64()).unwrap_or(0)
    } else {
        0
    };
    let mut tier = use_signal(|| if tiers.iter().any(|t| t == "haiku") { "haiku".to_string() } else { tiers.first().cloned().unwrap_or_default() });
    let mut name = use_signal(String::new);
    let mut grant = use_signal(move || default_grant.to_string());
    let mut charter = use_signal(String::new);
    let title = match &parent {
        Some(p) => format!("hire under {p}"),
        None => "hire a top-level agent".to_string(),
    };
    let seat = |t: &str| payload.tiers.get(t).and_then(|v| v.as_f64()).map(credits).unwrap_or_default();
    let ok = !name().trim().is_empty() && grant().trim().parse::<u64>().is_ok();
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
            select { id: "dx-hire-tier", value: "{tier}", onchange: move |e| tier.set(e.value()),
                for t in tiers.iter() {
                    option { key: "{t}", value: "{t}", selected: *t == tier(), "{t} · {provider_of(t).1} · seat {seat(t)}" }
                }
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
