//! El docket compartido de tickets en RSX (#29): `canvas/docket.tsx`
//! (`DocketModal`, `DocketRow`, `DocketPane`), `docketdesc.tsx`, las
//! referencias de `workrefs.tsx`, `refmd.tsx` y `reflinks.tsx`, "Staff…" de
//! `quickstaff.ts` y los endpoints de `api.ts`, con las clases y el CSS del
//! renderer.
//!
//! - **Lista.** Las filas livianas de `GET /work-items-view`, ordenadas por la
//!   última actualización del docket (el orden del motor), con los filtros por
//!   estado y por dueño, la búsqueda y los tres arreglos del renderer (sin
//!   agrupar, por estado y por agente). El backlog y el archivo se piden solo
//!   con su casilla (`include_archived`, regla del usuario en `AGENTS.md`), y
//!   siempre van al final. Los totales no cuentan lo archivado.
//! - **Detalle.** La fila liviana se ve en el acto y el ticket entero se pide
//!   al abrirlo (`GET /work-items/{wid}`) y cada vez que su `rev` cambia:
//!   descripción en Markdown seguro, decisiones (solo se agregan), evidencias,
//!   artefactos y adjuntos con los topes del motor, holders anteriores e
//!   historial. Un artefacto se guarda en Descargas y se **revela** en el
//!   Explorador, nunca se abre.
//! - **Acciones del usuario**, las mismas que el docket del renderer, con los
//!   mismos cuerpos: comentar (`reply`, al dueño o a un participante, como
//!   aviso pasivo si se pide), descartar la bandera ("Dismiss with no
//!   comment", que pasa el ticket a `blocked` con su motivo), "Staff…" en un
//!   ticket del backlog (asignarlo) y responder la pregunta adjunta. Cambiar el
//!   estado, asignar a mano, levantar la bandera y adjuntar una pregunta son
//!   actos de los agentes con la herramienta del docket: el renderer no tiene
//!   esos controles y el motor no tiene esas rutas para el usuario.
//! - **Actualización** solo por el WebSocket de la org, con el árbol (sin
//!   sondeo).

use crate::desk::{attr, fmt_local, markdown_with, parse_iso};
use crate::org::Ctx;
use crate::Route;
use dioxus::prelude::*;
use orgtree_engine_client::{actor_name, Artifact, QuickStaffPreview, QuickStaffSelection, WorkItem, WorkItemsPayload};
use serde_json::Value;
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

/// Topes del motor por ticket (`ledger.py`).
pub(crate) const EVIDENCE_MAX: usize = 50;
pub(crate) const ARTIFACT_MAX: usize = 40;
pub(crate) const HISTORY_MAX: usize = 100;

// ─── Estados ──────────────────────────────────────────────────────────────────

/// `STATUS_LABEL` de `docket.tsx`.
pub(crate) fn status_label(status: &str) -> String {
    match status {
        "backlogged" => "Backlogged",
        "open" => "Open",
        "in_progress" => "In progress",
        "blocked" => "Blocked",
        "review" => "Agent review",
        "approved" => "Approved — not landed",
        "deploy_ready" => "Deploy Ready",
        "done" => "Done",
        "superseded" => "Superseded",
        "dropped" => "Dropped",
        other => other,
    }
    .to_string()
}

/// `statusHelp`: la ayuda solo donde la palabra se puede leer de dos maneras.
fn status_help(status: &str) -> Option<&'static str> {
    match status {
        "review" => Some("Review by agents — a request for you rides the attention flag or a question"),
        "blocked" => Some("Cannot move until something outside the item happens — an answer, an event, another agent's work. It says what and how the agent will hear of it; it stays active and is never nudged by the idle reminder"),
        "dropped" => Some("Ended WITHOUT being completed — cancelled, or failed in a way it cannot be recovered from. Closed and archived at once (no one-hour wait), but never Done"),
        "approved" => Some("A reviewer approved one exact commit and it is NOT on main yet — the item is still with its owner and the outstanding action is the landing. Completion happens after the push is recorded, never before it"),
        "deploy_ready" => Some("Implementation is complete and awaiting deployment or publication. Active and actionable — not stuck on anything, and not yet live — so it stays on the desk and is nudged like any other in-flight status"),
        _ => None,
    }
}

/// `STATUS_GROUPS`: el orden de "Group by status" y del filtro por estado.
const STATUS_GROUPS: [(&str, &str); 10] = [
    ("attention", "Needs attention"),
    ("blocked", "Blocked"),
    ("in_progress", "In progress"),
    ("review", "Agent review"),
    ("approved", "Approved — not landed"),
    ("deploy_ready", "Deploy Ready"),
    ("open", "Open"),
    ("backlogged", "Backlogged"),
    ("done", "Done"),
    ("other", "Other closed"),
];

pub(crate) const UNASSIGNED: &str = "Unassigned";

/// `flaggedForUser`: solo una bandera manual hace de un ticket una fila de
/// atención; una pregunta adjunta dice `question waiting` y se responde en la
/// bandeja. Una bandera que se está descartando ya no cuenta.
pub(crate) fn flagged(item: &WorkItem, dismissing: &HashSet<String>) -> bool {
    item.manual_attention.is_some() && !dismissing.contains(&item.slug)
}

/// `questionWaiting`: una pregunta adjunta que el usuario no acaba de responder.
fn question_waiting(item: &WorkItem, submitted: &HashSet<String>) -> bool {
    item.questions.iter().any(|q| !submitted.contains(&q.ask_id))
}

/// `ago` de `canvas/shared.ts`.
pub(crate) fn ago(at: &str) -> String {
    let Some(then) = parse_iso(at) else { return String::new() };
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(then);
    let s = (now - then).max(0);
    if s < 90 {
        format!("{s}s")
    } else if s < 5400 {
        format!("{}m", (s as f64 / 60.0).round() as i64)
    } else {
        format!("{}h", (s as f64 / 3600.0).round() as i64)
    }
}

/// El reloj de la fila con el orden "Last updated": `docket_at`.
fn stamp(item: &WorkItem) -> String {
    item.docket_at.clone().or(item.updated_at.clone()).or(item.at.clone()).unwrap_or_default()
}

fn owner_name(item: &WorkItem) -> String {
    item.owner_node().unwrap_or_else(|| UNASSIGNED.to_string())
}

// ─── Secciones y filas ────────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum Group {
    None,
    Status,
    Agent,
}

#[derive(Clone, PartialEq, Debug)]
pub(crate) struct Section {
    pub key: String,
    pub heading: Option<String>,
    pub tone: Option<&'static str>,
    pub items: Vec<WorkItem>,
}

/// `buildSections`: el backlog y el archivo van siempre al final, en ese
/// orden, en cualquier arreglo, así que tildar una casilla solo agrega abajo.
pub(crate) fn build_sections(mode: Group, active: Vec<WorkItem>, backlog: Vec<WorkItem>, archived: Vec<WorkItem>, dismissing: &HashSet<String>) -> Vec<Section> {
    let mut out = Vec::new();
    match mode {
        Group::Status => {
            let bucket = |it: &WorkItem| -> &str {
                if flagged(it, dismissing) {
                    "attention"
                } else if STATUS_GROUPS.iter().any(|(k, _)| *k == it.status) {
                    STATUS_GROUPS.iter().find(|(k, _)| *k == it.status).map(|(k, _)| *k).unwrap_or("other")
                } else {
                    "other"
                }
            };
            for (key, heading) in STATUS_GROUPS {
                let items: Vec<WorkItem> = active.iter().filter(|it| bucket(it) == key).cloned().collect();
                if !items.is_empty() {
                    out.push(Section { key: format!("st:{key}"), heading: Some(heading.to_string()), tone: None, items });
                }
            }
        }
        Group::Agent => {
            let mut order: Vec<String> = Vec::new();
            let mut groups: HashMap<String, Vec<WorkItem>> = HashMap::new();
            for it in &active {
                let who = owner_name(it);
                if !groups.contains_key(&who) {
                    order.push(who.clone());
                }
                groups.entry(who).or_default().push(it.clone());
            }
            for who in order.iter().filter(|w| w.as_str() != UNASSIGNED) {
                out.push(Section { key: format!("ag:{who}"), heading: Some(who.clone()), tone: None, items: groups.remove(who).unwrap_or_default() });
            }
            if let Some(items) = groups.remove(UNASSIGNED) {
                out.push(Section { key: "ag:unassigned".into(), heading: Some(UNASSIGNED.into()), tone: None, items });
            }
        }
        Group::None => {
            if !active.is_empty() {
                out.push(Section { key: "all".into(), heading: None, tone: None, items: active });
            }
        }
    }
    if !backlog.is_empty() {
        out.push(Section { key: "backlog".into(), heading: Some("Backlogged — not yet approached".into()), tone: Some("backlog"), items: backlog });
    }
    if !archived.is_empty() {
        out.push(Section { key: "archive".into(), heading: Some("Archived".into()), tone: Some("archive"), items: archived });
    }
    out
}

/// `nestRows`: el orden del motor, con cada sub-ítem bajo su padre si el padre
/// está en la misma sección. Un ciclo no cuelga la lista: lo que queda suelto
/// se agrega en el primer nivel.
pub(crate) fn nest_rows(items: &[WorkItem]) -> Vec<(WorkItem, usize, usize)> {
    let here: HashSet<&str> = items.iter().map(|i| i.slug.as_str()).collect();
    let mut kids: HashMap<&str, Vec<&WorkItem>> = HashMap::new();
    let mut roots = Vec::new();
    for it in items {
        match it.parent.as_deref().filter(|p| here.contains(p) && *p != it.slug) {
            Some(p) => kids.entry(p).or_default().push(it),
            None => roots.push(it),
        }
    }
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    fn walk<'a>(it: &'a WorkItem, depth: usize, kids: &HashMap<&str, Vec<&'a WorkItem>>, seen: &mut HashSet<String>, out: &mut Vec<(WorkItem, usize, usize)>) {
        if !seen.insert(it.slug.clone()) {
            return;
        }
        let mine = kids.get(it.slug.as_str()).cloned().unwrap_or_default();
        out.push((it.clone(), depth, mine.len()));
        for k in mine {
            walk(k, depth + 1, kids, seen, out);
        }
    }
    for r in roots {
        walk(r, 0, &kids, &mut seen, &mut out);
    }
    for it in items {
        walk(it, 0, &kids, &mut seen, &mut out);
    }
    out
}

/// `searchText` + `matchesTerms`: cada término (sin mayúsculas) tiene que
/// aparecer en el nombre, el título, la descripción o el último avance.
pub(crate) fn matches(item: &WorkItem, query: &str) -> bool {
    let terms: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
    if terms.is_empty() {
        return true;
    }
    let hay = [item.slug.as_str(), item.title.as_str(), item.objective.as_deref().unwrap_or_default()]
        .into_iter()
        .chain(item.done_so_far.iter().map(String::as_str))
        .chain(item.working_on_next.iter().map(String::as_str))
        .collect::<Vec<_>>()
        .join("\n")
        .to_lowercase();
    terms.iter().all(|t| hay.contains(t.as_str()))
}

// ─── Referencias en la prosa (workrefs.tsx, reflinks.tsx) ─────────────────────

/// Lo que la vista sabe de los nombres de la org: los tickets cargados y los
/// agentes del árbol. `loaded` en falso es `pending` (todavía no se sabe).
#[derive(Clone, Default, PartialEq, Debug)]
pub(crate) struct RefWorld {
    pub org: String,
    pub items: HashSet<String>,
    pub agents: HashSet<String>,
    pub loaded: bool,
}

#[derive(Clone, PartialEq, Debug)]
pub(crate) enum Part {
    Text(String),
    Item(String, String),
    Agent(String, String),
    /// Un token canónico que no se puede abrir acá: el texto, la clase y por qué.
    Inert(String, &'static str, &'static str),
}

fn blocks_before(c: char) -> bool {
    c.is_ascii_alphanumeric() || "_-/\\:.@#&?=+~".contains(c)
}

fn blocks_after(c: char) -> bool {
    c.is_ascii_alphanumeric() || "_-/\\:@#&?=+~".contains(c)
}

/// `linkable`: un nombre solo es una mención si lo que lo toca no lo hace
/// parte de algo más largo (una ruta, una URL, un nombre con punto).
fn linkable(text: &str, start: usize, end: usize) -> bool {
    if let Some(before) = text[..start].chars().next_back() {
        if blocks_before(before) {
            return false;
        }
    }
    let mut after = text[end..].chars();
    match after.next() {
        Some(c) if blocks_after(c) => false,
        Some('.') => !after.next().is_some_and(|n| n.is_ascii_alphanumeric()),
        _ => true,
    }
}

fn seg_char(c: char) -> bool {
    c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'
}

/// Un token canónico al principio de `text` (`@item:org/slug`,
/// `@agent:org/node[@gen]`, `@doc:…`, `@mail:…`): su tipo, la org, el id y
/// el largo. `None` si no es uno (un token mal formado no se trunca).
fn canonical(text: &str) -> Option<(&'static str, String, String, usize)> {
    let (kind, rest) = ["item", "doc", "agent", "mail"].into_iter().find_map(|k| text.strip_prefix('@')?.strip_prefix(k)?.strip_prefix(':').map(|r| (k, r)))?;
    let head = 1 + kind.len() + 1;
    let org_len = rest.find(|c: char| !seg_char(c))?;
    if org_len == 0 || !rest[org_len..].starts_with('/') {
        return None;
    }
    let org = rest[..org_len].to_string();
    let tail = &rest[org_len + 1..];
    let mut id_len = tail.find(|c: char| !seg_char(c)).unwrap_or(tail.len());
    if kind == "agent" && tail[id_len..].starts_with('@') {
        let gen = tail[id_len + 1..].find(|c: char| !c.is_ascii_digit()).unwrap_or(tail.len() - id_len - 1);
        if gen > 0 {
            id_len += 1 + gen;
        }
    }
    if kind == "mail" {
        // la forma completa de mail no se abre desde el docket: hasta el final del token
        id_len = tail.find(|c: char| !(seg_char(c) || c == '/' || c == '@' || c.is_ascii_digit())).unwrap_or(tail.len());
    }
    if id_len == 0 {
        return None;
    }
    let end = head + org_len + 1 + id_len;
    // END: lo que sigue no puede ser parte del id, ni un `@` suelto
    let next = &text[end..];
    if let Some(c) = next.chars().next() {
        if c.is_ascii_alphanumeric() || c == '_' || c == '/' || c == '-' {
            return None;
        }
        if c == '@' && !["@item:", "@doc:", "@agent:", "@mail:"].iter().any(|p| next.starts_with(p)) {
            return None;
        }
    }
    Some((kind, org, tail[..id_len].to_string(), end))
}

/// `splitRefs` + `scanRefs` + `refOutcome`: la prosa en tramos, con los
/// nombres de tickets y agentes de esta org como enlaces (un ticket gana a un
/// agente del mismo nombre) y los tokens canónicos resueltos: de otra org,
/// inexistente o de un tipo que el docket no abre, quedan como texto que dice
/// por qué.
pub(crate) fn split_refs(text: &str, world: &RefWorld) -> Vec<Part> {
    let mut names: Vec<(&str, bool)> = world.items.iter().map(|s| (s.as_str(), true)).collect();
    names.extend(world.agents.iter().filter(|a| !world.items.contains(*a)).map(|a| (a.as_str(), false)));
    // el más largo primero: `a-b` no le gana a `a-b-c`
    names.sort_by(|a, b| b.0.len().cmp(&a.0.len()).then(a.0.cmp(b.0)));
    let mut out: Vec<Part> = Vec::new();
    let mut plain = String::new();
    let mut last_token_end: Option<usize> = None;
    let mut i = 0;
    let flush = |plain: &mut String, out: &mut Vec<Part>| {
        if !plain.is_empty() {
            out.push(Part::Text(std::mem::take(plain)));
        }
    };
    while i < text.len() {
        let rest = &text[i..];
        let c = rest.chars().next().unwrap();
        if c == '@' {
            let lead_ok = i == 0 || last_token_end == Some(i) || !text[..i].chars().next_back().is_some_and(|b| b.is_ascii_alphanumeric() || b == '_' || b == '@' || b == '-');
            if let Some((kind, org, id, len)) = canonical(rest).filter(|_| lead_ok) {
                flush(&mut plain, &mut out);
                let token = rest[..len].to_string();
                out.push(if org != world.org {
                    Part::Inert(token, "foreign", "from another organization — not resolved here")
                } else if kind == "item" && world.items.contains(&id) {
                    Part::Item(id, token)
                } else if kind == "agent" && world.agents.contains(id.split('@').next().unwrap_or_default()) {
                    Part::Agent(id, token)
                } else if (kind == "item" || kind == "agent") && !world.loaded {
                    Part::Inert(token, "pending", "still loading")
                } else if kind == "item" || kind == "agent" {
                    Part::Inert(token, "absent", "not in this organization")
                } else {
                    Part::Inert(token, "elsewhere", "opens from its own panel, not the docket")
                });
                i += len;
                last_token_end = Some(i);
                continue;
            }
        }
        if seg_char(c) {
            if let Some((name, is_item)) = names.iter().find(|(n, _)| rest.starts_with(*n) && linkable(text, i, i + n.len())) {
                flush(&mut plain, &mut out);
                out.push(if *is_item { Part::Item(name.to_string(), name.to_string()) } else { Part::Agent(name.to_string(), name.to_string()) });
                i += name.len();
                continue;
            }
        }
        plain.push(c);
        i += c.len_utf8();
    }
    flush(&mut plain, &mut out);
    out
}

/// Las referencias como HTML, para el Markdown de la descripción (`linkifyRefs`).
pub(crate) fn refs_html(text: &str, world: &RefWorld) -> Option<String> {
    let parts = split_refs(text, world);
    if parts.iter().all(|p| matches!(p, Part::Text(_))) {
        return None;
    }
    let esc = |t: &str| attr(t);
    Some(
        parts
            .iter()
            .map(|p| match p {
                Part::Text(t) => esc(t),
                Part::Item(slug, label) => format!("<a href=\"#\" class=\"docket-ref\" data-ref-item=\"{}\" title=\"open {}\">{}</a>", esc(slug), esc(slug), esc(label)),
                Part::Agent(id, label) => format!("<a href=\"#\" class=\"docket-ref docket-ref-agent\" data-ref-agent=\"{}\" title=\"open {}'s desk\">{}</a>", esc(id), esc(id), esc(label)),
                Part::Inert(token, class, why) => format!("<span class=\"docket-ref dim {class}\" title=\"{}\">{}</span>", esc(why), esc(token)),
            })
            .collect(),
    )
}

/// Lo que esta vista sabe de los nombres: los tickets que trajo el motor
/// (también las referencias de los grupos cerrados) y los agentes del árbol.
pub(crate) fn ref_world(ctx: &Ctx) -> RefWorld {
    let mut world = RefWorld { org: ctx.slug.peek().clone(), ..RefWorld::default() };
    if let Some(work) = &*ctx.work.read() {
        world.loaded = true;
        for it in all_rows(work) {
            world.items.insert(it.slug.clone());
        }
        for r in references(work) {
            world.items.insert(r.0);
        }
    }
    if let Some(Ok(tree)) = &*ctx.tree.read() {
        for node in tree.nodes() {
            world.agents.insert(node.id.clone());
        }
    }
    world
}

/// Todas las filas de la lista, sin repetir: activas, banderas, archivo y backlog.
pub(crate) fn all_rows(work: &WorkItemsPayload) -> Vec<&WorkItem> {
    let mut seen = HashSet::new();
    [Some(&work.items), work.attention.as_ref(), work.backlogged.as_ref(), work.archived.as_ref()]
        .into_iter()
        .flatten()
        .flatten()
        .filter(|i| seen.insert(i.slug.clone()))
        .collect()
}

/// `references` de la lista: cada ticket de la org (también los de los
/// grupos cerrados) con si está archivado y su estado.
fn references(work: &WorkItemsPayload) -> Vec<(String, bool, String)> {
    work.extra
        .get("references")
        .and_then(Value::as_array)
        .map(|refs| {
            refs.iter()
                .filter_map(|r| {
                    let slug = r.get("slug")?.as_str()?.to_string();
                    let archived = r.get("archived").and_then(Value::as_bool).unwrap_or(false);
                    let status = r.get("status").and_then(Value::as_str).unwrap_or_default().to_string();
                    Some((slug, archived, status))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// La prosa corta del docket (listas, la bandera, decisiones) con sus
/// referencias como botones (`RefProse`).
#[component]
fn Prose(text: String) -> Element {
    let ctx = use_context::<Ctx>();
    let world = ref_world(&ctx);
    let parts = split_refs(&text, &world);
    rsx! {
        for part in parts {
            match part {
                Part::Text(t) => rsx! { "{t}" },
                Part::Item(slug, label) => rsx! {
                    button { r#type: "button", class: "docket-ref", "data-ref-item": "{slug}", title: "open {slug}",
                        onclick: move |_| ctx.open_docket(Some(slug.clone())),
                        "{label}"
                    }
                },
                Part::Agent(id, label) => rsx! { AgentRef { id, label } },
                Part::Inert(token, class, why) => rsx! { span { class: "docket-ref dim {class}", title: "{why}", "{token}" } },
            }
        }
    }
}

#[component]
fn AgentRef(id: String, label: String) -> Element {
    let ctx = use_context::<Ctx>();
    let mut route = use_context::<Signal<Route>>();
    rsx! {
        button { r#type: "button", class: "docket-ref docket-ref-agent cc-name cc-name-jump", "data-ref-agent": "{id}", title: "open {id}'s desk",
            onclick: move |_| route.set(Route::Desk { org: ctx.slug.peek().clone(), node: id.clone() }),
            "{label}"
        }
    }
}

// ─── Descargas: guardar y revelar, nunca abrir ────────────────────────────────

/// La carpeta de Descargas del usuario, como la descarga de un navegador.
fn downloads_dir() -> Option<std::path::PathBuf> {
    let home = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME"))?;
    Some(std::path::PathBuf::from(home).join("Downloads"))
}

/// Un nombre de archivo seguro: solo el último componente, sin caracteres que
/// Windows rechaza ni puntos al principio.
pub(crate) fn safe_name(name: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or_default();
    let clean: String = base.chars().map(|c| if c.is_control() || "<>:\"|?*".contains(c) { '_' } else { c }).collect();
    let clean = clean.trim().trim_start_matches('.').to_string();
    if clean.is_empty() {
        "archivo".to_string()
    } else {
        clean
    }
}

/// Guarda los bytes en `Descargas/Orgtree/<org>/<ticket>/<nombre>` sin pisar
/// un archivo distinto (`nombre (2).ext`) y lo revela en el Explorador.
pub(crate) fn save_and_reveal(org: &str, item: &str, name: &str, bytes: &[u8]) -> Result<std::path::PathBuf, String> {
    let dir = downloads_dir().ok_or("no Downloads folder")?.join("Orgtree").join(safe_name(org)).join(safe_name(item));
    std::fs::create_dir_all(&dir).map_err(|e| format!("could not create {}: {e}", dir.display()))?;
    let name = safe_name(name);
    let (stem, ext) = match name.rsplit_once('.') {
        Some((s, e)) if !s.is_empty() => (s.to_string(), format!(".{e}")),
        _ => (name.clone(), String::new()),
    };
    let mut path = dir.join(&name);
    let mut n = 2;
    while path.exists() && std::fs::read(&path).map(|b| b != bytes).unwrap_or(true) {
        path = dir.join(format!("{stem} ({n}){ext}"));
        n += 1;
    }
    std::fs::write(&path, bytes).map_err(|e| format!("could not save {}: {e}", path.display()))?;
    crate::reveal::reveal(&path.to_string_lossy())
}

/// Un UUID v4 para `request_id` de "Staff…" (`crypto.randomUUID`).
fn new_uuid() -> String {
    use std::hash::{BuildHasher, Hasher};
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    let state = std::collections::hash_map::RandomState::new();
    let mut a = state.build_hasher();
    a.write_u128(nanos);
    a.write_u64(COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed));
    let hi = a.finish();
    let mut b = state.build_hasher();
    b.write_u64(hi.rotate_left(17) ^ 0x9e37_79b9_7f4a_7c15);
    let lo = b.finish();
    let hi = (hi & 0xffff_ffff_ffff_0fff) | 0x0000_0000_0000_4000;
    let lo = (lo & 0x3fff_ffff_ffff_ffff) | 0x8000_0000_0000_0000;
    format!("{:08x}-{:04x}-{:04x}-{:04x}-{:012x}", hi >> 32, (hi >> 16) & 0xffff, hi & 0xffff, lo >> 48, lo & 0xffff_ffff_ffff)
}

// ─── El botón de la barra de la org ───────────────────────────────────────────

/// `DocketToolbarButton`: brilla con una bandera manual sin descartar y cuenta
/// las banderas, o si no hay ninguna, los tickets activos (nunca los archivados).
#[component]
pub(crate) fn DocketBell() -> Element {
    let ctx = use_context::<Ctx>();
    let summary = match &*ctx.tree.read() {
        Some(Ok(tree)) => tree.work_items_summary.clone().unwrap_or_default(),
        _ => Default::default(),
    };
    let dismissing = ctx.dismissing.read();
    let attention = if summary.raises.is_empty() { summary.attention } else { summary.raises.iter().filter(|(slug, _)| !dismissing.contains(slug)).count() as u64 };
    let glow = attention > 0;
    let count = if glow { attention } else { summary.active };
    let title = if glow { format!("work docket — {attention} item(s) need attention") } else { "work docket".to_string() };
    rsx! {
        button { class: if glow { "iconbtn docket-bell glow dx-docket-bell" } else { "iconbtn docket-bell dx-docket-bell" },
            "aria-label": "Work", title: "{title}", "data-attention": "{attention}", "data-active": "{summary.active}",
            onclick: move |_| {
                if *ctx.docket_open.peek() { ctx.close_docket() } else { ctx.open_docket(None) }
            },
            crate::icons::DocketIcon {}
            if count > 0 {
                b { class: if glow { "eye-count docket-attn asks" } else { "eye-count" }, "{count}" }
            }
        }
    }
}

// ─── El panel ─────────────────────────────────────────────────────────────────

/// `DocketModal`: la cabecera con la búsqueda, las casillas, los filtros y el
/// arreglo; la lista; y el panel del ticket elegido.
#[component]
pub(crate) fn DocketPanel() -> Element {
    let mut ctx = use_context::<Ctx>();
    let mut group = use_signal(|| Group::None);
    let mut status_filter = use_signal(String::new);
    let mut owner_filter = use_signal(String::new);
    let mut query = use_signal(String::new);
    let work = ctx.work.read().clone();
    let (show_archived, show_backlog) = ((ctx.docket_archived)(), (ctx.docket_backlog)());
    let dismissing = ctx.dismissing.read().clone();
    let submitted = ctx.submitted.read().clone();
    let selected = (ctx.docket_sel)();

    let counts = work.as_ref().and_then(|w| w.counts.clone()).unwrap_or_default();
    let active: Vec<WorkItem> = work.as_ref().map(|w| w.items.clone()).unwrap_or_default();
    let backlog: Vec<WorkItem> = if show_backlog { work.as_ref().and_then(|w| w.backlogged.clone()).unwrap_or_default() } else { Vec::new() };
    let archived: Vec<WorkItem> = if show_archived { work.as_ref().and_then(|w| w.archived.clone()).unwrap_or_default() } else { Vec::new() };
    // los filtros eligen entre lo que está a la vista (nunca más allá de una casilla)
    let visible: Vec<&WorkItem> = active.iter().chain(backlog.iter()).chain(archived.iter()).collect();
    let mut statuses: Vec<String> = Vec::new();
    for (key, _) in STATUS_GROUPS.iter().filter(|(k, _)| *k != "attention" && *k != "other") {
        if visible.iter().any(|i| i.status == *key) {
            statuses.push(key.to_string());
        }
    }
    for it in &visible {
        if !statuses.contains(&it.status) {
            statuses.push(it.status.clone());
        }
    }
    let mut owners: Vec<String> = Vec::new();
    for it in &visible {
        let who = owner_name(it);
        if !owners.contains(&who) {
            owners.push(who);
        }
    }
    owners.sort_by(|a, b| (a == UNASSIGNED).cmp(&(b == UNASSIGNED)).then(a.cmp(b)));
    let (sf, of, q) = (status_filter(), owner_filter(), query());
    let keep = |it: &WorkItem| (sf.is_empty() || it.status == sf) && (of.is_empty() || owner_name(it) == of) && matches(it, &q);
    let shown = |list: &[WorkItem]| list.iter().filter(|i| keep(i)).cloned().collect::<Vec<_>>();
    let sections = build_sections(group(), shown(&active), shown(&backlog), shown(&archived), &dismissing);
    let shown_rows: usize = sections.iter().filter(|s| s.key != "archive").map(|s| s.items.len()).sum();
    let filtering = !sf.is_empty() || !of.is_empty() || !q.trim().is_empty();
    let attention_now = active.iter().filter(|i| flagged(i, &dismissing)).count();
    // lo elegido: de la lista, o (desde una referencia o la cola) solo el nombre
    let current = selected.as_ref().map(|slug| {
        work.as_ref()
            .and_then(|w| all_rows(w).into_iter().find(|i| &i.slug == slug).cloned())
            .unwrap_or_else(|| WorkItem { slug: slug.clone(), view: Some("list".into()), ..WorkItem::default() })
    });
    let close = move |_| ctx.close_docket();
    let group_value = match group() {
        Group::None => "none",
        Group::Status => "status",
        Group::Agent => "agent",
    };
    rsx! {
        div { class: "overlay dx-docket-overlay", onclick: close,
            div { class: "settings wide docket-modal dx-docket", role: "dialog", "aria-modal": "true", "aria-label": "Work docket",
                "data-rows": "{shown_rows}", onclick: move |e| e.stop_propagation(),
                div { class: "gallery-head docket-head",
                    h3 { crate::icons::DocketIcon {} " Work docket" }
                    div { class: "docket-search", role: "search",
                        input { r#type: "search", class: "docket-search-input", placeholder: "Search tickets…",
                            "aria-label": "Search work items by title, name, description or latest progress",
                            value: "{q}", oninput: move |e| query.set(e.value()) }
                        if !q.is_empty() {
                            button { r#type: "button", class: "docket-search-clear", "aria-label": "Clear search", title: "Clear search", onclick: move |_| query.set(String::new()), "✕" }
                        }
                    }
                    div { class: "docket-options", "data-open": "true", role: "group", "aria-label": "Docket view options",
                        div { class: "docket-filterbar",
                            label { class: "checkline docket-showarchived", title: "include archived work items — done items an hour after their last docket update, dropped items at once",
                                input { r#type: "checkbox", checked: show_archived,
                                    onchange: move |e| { ctx.docket_archived.set(e.checked()); ctx.reload_work() } }
                                "Show archived"
                                if counts.archived > 0 {
                                    span { class: "dim", " · {counts.archived}" }
                                }
                            }
                            label { class: "checkline docket-showbacklog", title: "include work that has not been approached or approved yet",
                                input { r#type: "checkbox", checked: show_backlog,
                                    onchange: move |e| { ctx.docket_backlog.set(e.checked()); ctx.reload_work() } }
                                "Show backlogged"
                                if counts.backlogged > 0 {
                                    span { class: "dim", " · {counts.backlogged}" }
                                }
                            }
                        }
                        div { class: "docket-sortbar",
                            label { class: "dim", r#for: "dx-docket-group", "Arrange" }
                            select { id: "dx-docket-group", class: "docket-group-select", value: "{group_value}",
                                onchange: move |e| group.set(match e.value().as_str() { "status" => Group::Status, "agent" => Group::Agent, _ => Group::None }),
                                option { value: "none", selected: group_value == "none", "No group" }
                                option { value: "status", selected: group_value == "status", "Group by status" }
                                option { value: "agent", selected: group_value == "agent", "Group by agent" }
                            }
                            label { class: "dim", r#for: "dx-docket-status", "Status" }
                            select { id: "dx-docket-status", class: "docket-group-select", value: "{sf}",
                                onchange: move |e| status_filter.set(e.value()),
                                option { value: "", selected: sf.is_empty(), "All statuses" }
                                for s in statuses.iter().cloned() {
                                    option { key: "{s}", value: "{s}", selected: sf == s, {status_label(&s)} }
                                }
                            }
                            label { class: "dim", r#for: "dx-docket-owner", "Owner" }
                            select { id: "dx-docket-owner", class: "docket-group-select", value: "{of}",
                                onchange: move |e| owner_filter.set(e.value()),
                                option { value: "", selected: of.is_empty(), "All owners" }
                                for o in owners.iter().cloned() {
                                    option { key: "{o}", value: "{o}", selected: of == o, "{o}" }
                                }
                            }
                            span { class: "dim docket-sort-why",
                                "most recently updated first"
                                if group() != Group::None {
                                    ", inside each group"
                                }
                            }
                        }
                    }
                }
                // los totales: nunca cuentan lo archivado
                div { class: "dim dx-docket-totals", "data-active": "{counts.active}", "data-attention": "{attention_now}",
                    "data-shown": "{shown_rows}", "data-archived": "{counts.archived}",
                    "{counts.active} active"
                    if attention_now > 0 {
                        " · {attention_now} need attention"
                    }
                    if show_backlog {
                        " · {counts.backlogged} backlogged"
                    }
                    if filtering {
                        " · {shown_rows} shown"
                    }
                    " · archived items are not counted"
                }
                div { class: "mailpane",
                    if work.is_none() {
                        div { class: "dim pad", "loading…" }
                    } else {
                        div { class: "mailer",
                            div { class: "mailer-list dx-docket-list",
                                if sections.is_empty() {
                                    if filtering {
                                        div { class: "dim pad docket-nomatch", "no items match the filters — backlogged and archived work is listed only when its box is ticked" }
                                    } else {
                                        div { class: "dim pad", "no work items yet" }
                                    }
                                }
                                for section in sections.iter().cloned() {
                                    div { key: "{section.key}", class: if let Some(tone) = section.tone { "docket-section tone-{tone}" } else { "docket-section" },
                                        "data-section": "{section.key}",
                                        if let Some(heading) = section.heading.clone() {
                                            div { class: "docket-group-head",
                                                span { "{heading}" }
                                                span { class: "dim docket-group-n", "{section.items.len()}" }
                                            }
                                        }
                                        div { class: "docket-category-rows",
                                            for (item, depth, kids) in nest_rows(&section.items) {
                                                DocketRowView { key: "{item.slug}", selected: selected.as_deref() == Some(item.slug.as_str()),
                                                    attention: flagged(&item, &dismissing), asking: question_waiting(&item, &submitted),
                                                    depth, kids, item }
                                            }
                                        }
                                    }
                                }
                            }
                            div { class: "mailer-read dx-docket-read",
                                match current {
                                    Some(row) => rsx! { DocketPane { key: "{row.slug}", row, in_docket: true } },
                                    None => rsx! { div { class: "dim pad mailer-none", "select an item to view it" } },
                                }
                            }
                        }
                    }
                }
                div { class: "docket-foot dim", "Done items archive after 1 hour without an update." }
            }
        }
    }
}

/// `DocketRow`: el nombre es el slug (el título va en el tooltip y en el
/// panel), la edad de la última actualización, el estado y el dueño.
#[component]
fn DocketRowView(item: WorkItem, selected: bool, attention: bool, asking: bool, depth: usize, kids: usize) -> Element {
    let mut ctx = use_context::<Ctx>();
    let state = if item.archived == Some(true) {
        "archived"
    } else if attention {
        "attention"
    } else if item.status == "backlogged" {
        "backlog"
    } else {
        "active"
    };
    let class = format!(
        "mailrow docket-row {state} status-{}{}{}{}",
        item.status,
        if selected { " on" } else { "" },
        if depth > 0 { " docket-child" } else { "" },
        if kids > 0 { " docket-parent" } else { "" }
    );
    let label = if attention { "Needs attention".to_string() } else { status_label(&item.status) };
    let help = if attention { None } else { status_help(&item.status) };
    let at = stamp(&item);
    let full = fmt_local(&at, (ctx.tz)().as_ref());
    let slug = item.slug.clone();
    let style = if depth > 0 { format!("--docket-depth: {depth}") } else { String::new() };
    rsx! {
        div { class: "{class}", title: "{item.title}", "data-ticket": "{item.slug}", "data-status": "{item.status}",
            "data-owner": "{owner_name(&item)}", "data-depth": "{depth}", style: "{style}",
            onclick: move |_| {
                let again = ctx.docket_sel.peek().as_deref() == Some(slug.as_str());
                ctx.docket_sel.set(if again { None } else { Some(slug.clone()) });
            },
            div { class: "l1",
                span { class: "mfrom docket-rowname", "{item.slug}" }
                if kids > 0 {
                    span { class: "dim docket-subcount", title: "Direct sub-items in this view", {plural(kids, "sub-item")} }
                }
                span { class: "mtime", title: "updated {full}", "aria-label": "updated {full}", {ago(&at)} }
            }
            div { class: "l2",
                span { class: if attention { "docket-status status-{item.status} attention" } else { "docket-status status-{item.status}" },
                    title: help.unwrap_or_default(), "{label}" }
                if asking {
                    span { class: "docket-qwait", title: "An open question on this ticket is waiting in your Inbox", "question waiting" }
                }
                span { class: "docket-updater",
                    match item.owner_node() {
                        Some(owner) => rsx! { span { class: "docket-actor", span { class: "docket-actor-name", "{owner}" } } },
                        None => rsx! { span { class: "dim", "{UNASSIGNED}" } },
                    }
                }
                if item.status == "review" {
                    if let Some(reviewer) = item.reviewer.as_ref().and_then(|r| r.get("node")).and_then(Value::as_str) {
                        span { class: "docket-reviewer", span { class: "dim", "Reviewer: " } "{reviewer}" }
                    }
                }
            }
        }
    }
}

// ─── El panel del ticket ──────────────────────────────────────────────────────

/// `DocketSection`: cada sección del detalle con su flecha para plegarla. El
/// historial empieza plegado (es contexto de auditoría, como la verificación
/// en el renderer).
#[component]
fn DocketSection(title: String, #[props(default)] summary: Option<String>, #[props(default)] desc: bool, children: Element) -> Element {
    let mut collapsed = use_signal(|| title == "HISTORY");
    let heading_class = if desc || title == "ATTACHMENTS" { "docket-detail-section-title dim docket-list-heading" } else { "docket-detail-section-title dim" };
    let key = title.to_lowercase().replace([' ', '/'], "-");
    rsx! {
        section { class: if desc { "docket-desc" } else { "docket-detail-section" }, "data-section": "{key}",
            div { class: "docket-detail-section-head",
                h4 { class: "{heading_class}", "{title}" }
                if let Some(summary) = summary {
                    span { class: "docket-detail-section-summary dim", "{summary}" }
                }
                button { r#type: "button", class: "docket-detail-toggle", "aria-expanded": "{!collapsed()}",
                    "aria-label": if collapsed() { "Expand {title}" } else { "Collapse {title}" },
                    onclick: move |_| collapsed.toggle(),
                    span { "aria-hidden": "true", if collapsed() { "▶" } else { "▼" } }
                }
            }
            div { class: "docket-detail-section-body", hidden: collapsed(), {children} }
        }
    }
}

/// Un actor del docket por su nombre: el usuario es "you".
fn who(actor: Option<&Value>) -> String {
    actor_name(actor)
}

/// Una fila del historial en palabras: la operación y sus datos (`from`/`to`
/// de una asignación, el estado, el motivo…). Una fila plegada dice cuántas
/// filas viejas resume.
pub(crate) fn history_text(row: &Value) -> String {
    if row.get("kind").and_then(Value::as_str) == Some("folded") {
        let n = row.get("count").and_then(Value::as_u64).unwrap_or(0);
        return format!("{n} older rows summarised");
    }
    let op = row.get("op").and_then(Value::as_str).unwrap_or("update");
    let mut details = Vec::new();
    if let Some(map) = row.as_object() {
        for (key, value) in map {
            if ["at", "by", "op", "why"].contains(&key.as_str()) {
                continue;
            }
            let text = match value {
                Value::Null => continue,
                Value::String(s) => s.clone(),
                Value::Number(n) => n.to_string(),
                Value::Bool(b) => b.to_string(),
                Value::Object(o) if o.contains_key("node") => who(Some(value)),
                Value::Array(a) => a.iter().map(|v| v.as_str().map(str::to_string).unwrap_or_else(|| who(Some(v)))).collect::<Vec<_>>().join(", "),
                other => other.to_string(),
            };
            let text: String = text.chars().take(160).collect();
            details.push(format!("{key}: {text}"));
        }
    }
    if details.is_empty() {
        op.to_string()
    } else {
        format!("{op} — {}", details.join(" · "))
    }
}

/// El estado y su información, elegida por el estado actual (`stateInfo`):
/// `blocked` con su motivo, `dropped` con por qué terminó sin completarse.
fn state_info(item: &WorkItem) -> Option<(String, Option<String>)> {
    match item.status.as_str() {
        "blocked" => Some((
            if item.legacy_status.as_deref() == Some("waiting") { "BLOCKED BECAUSE (recorded as waiting before 2026-09-07)".into() } else { "BLOCKED BECAUSE".into() },
            item.blocked_reason.clone(),
        )),
        "waiting" => Some(("WAITING FOR".into(), item.waiting_reason.clone())),
        "dropped" => Some(("ENDED WITHOUT COMPLETING — WHY".into(), item.dropped_reason.clone())),
        _ => None,
    }
    .map(|(h, t)| (h, t.filter(|t| !t.trim().is_empty())))
}

/// `DocketPane`: lo que la fila ya trae se ve en el acto, y el ticket entero
/// (decisiones, evidencias, artefactos, adjuntos, holders e historial) llega
/// con `GET /work-items/{wid}`, otra vez cada vez que su `rev` cambia. En la
/// cola de atención (#28) es el mismo panel, con "Open in docket".
#[component]
pub(crate) fn DocketPane(row: WorkItem, #[props(default)] in_docket: bool) -> Element {
    let ctx = use_context::<Ctx>();
    // en el scope de la vista: el pedido y la respuesta terminan aunque el panel se vaya
    let mut full = use_hook(|| Signal::new_in_scope(None::<Result<WorkItem, String>>, ctx.scope));
    let fetched = use_hook(|| Rc::new(RefCell::new(String::new())));
    let key = format!("{}#{}#{}", row.slug, row.rev.unwrap_or(0), row.view_revision.clone().unwrap_or_default());
    if *fetched.borrow() != key {
        *fetched.borrow_mut() = key.clone();
        let (slug, fetched) = (row.slug.clone(), fetched.clone());
        ctx.spawn(async move {
            let result = ctx.work_item(&slug).await;
            if *fetched.borrow() == key {
                let _ = full.try_write().map(|mut f| *f = Some(result));
            }
        });
    }
    let retry = {
        let fetched = fetched.clone();
        move |_| {
            fetched.borrow_mut().clear();
            full.set(None);
        }
    };
    let loaded = full.read().clone();
    let error = match &loaded {
        Some(Err(e)) => Some(e.clone()),
        _ => None,
    };
    let detail = match loaded {
        Some(Ok(f)) if f.slug == row.slug && f.rev >= row.rev => Some(f),
        _ => None,
    };
    let mut item = detail.clone().unwrap_or_else(|| row.clone());
    let dismissing = ctx.dismissing.read().clone();
    if dismissing.contains(&item.slug) {
        item.manual_attention = None;
    }
    if item.title.is_empty() && detail.is_none() {
        // solo el nombre (una referencia o la cola): esperar el ticket entero
        return rsx! {
            if let Some(error) = error {
                div { role: "alert", "Could not load this item: {error} "
                    button { onclick: retry, "Retry" }
                }
            } else {
                div { class: "dim pad", role: "status", "Loading item details…" }
            }
        };
    }
    let attention = flagged(&item, &dismissing);
    let label = if attention { "Needs attention".to_string() } else { status_label(&item.status) };
    let owner = item.owner_node();
    let recipients = item.reply_recipients.clone().unwrap_or_else(|| {
        let mut list = Vec::new();
        if let Some(owner) = owner.clone() {
            let state = match item.owner_state.as_deref() {
                Some("missing") => "missing",
                Some("retired") => "retired",
                _ => "live",
            };
            list.push(orgtree_engine_client::Recipient { node: owner, role: "owner".into(), state: state.into() });
        }
        for p in item.participants.iter().filter(|p| Some(*p) != owner.as_ref()) {
            list.push(orgtree_engine_client::Recipient { node: p.clone(), role: "participant".into(), state: "missing".into() });
        }
        list
    });
    let participants: Vec<_> = recipients.iter().filter(|r| r.role == "participant").cloned().collect();
    let world = ref_world(&ctx);
    let description = item.objective.clone().filter(|o| !o.trim().is_empty()).map(|o| markdown_with(&o, Some(&|t: &str| refs_html(t, &world))));
    let notice = item.objective_notice.clone().filter(|n| !n.is_null());
    let decisions: Vec<_> = item.scope.iter().filter(|s| s.kind == "decision").cloned().collect();
    let artifacts = item.artifacts.clone();
    let holders = item.holders.clone();
    let earlier: Vec<_> = match holders.split_last() {
        Some((last, rest)) if Some(&last.node) == owner.as_ref() => rest.to_vec(),
        _ => holders.clone(),
    };
    let asks: HashMap<String, orgtree_engine_client::AskInfo> = match &*ctx.tree.read() {
        Some(Ok(tree)) => crate::inbox::open_asks(tree, &ctx.submitted.read()).into_iter().map(|a| (a.id.clone(), a)).collect(),
        _ => HashMap::new(),
    };
    let staffable = item.status == "backlogged" && item.archived != Some(true);
    let tz = (ctx.tz)();
    let updated = item.docket_at.clone().or(item.at.clone()).unwrap_or_default();
    let slug = item.slug.clone();
    let open_slug = slug.clone();
    let dismiss_item = item.clone();
    let can_dismiss = item.attention_sources.iter().any(|s| s == "manual") || item.manual_attention.is_some();
    rsx! {
        if let Some(error) = error {
            div { role: "alert", class: "dx-docket-error", "Could not refresh this item: {error} "
                button { onclick: retry, "Retry" }
            }
        }
        div { class: "mailer-head docket-pane-head", "data-ticket": "{item.slug}", "data-full": "{detail.is_some()}", "data-rev": "{item.rev.unwrap_or(0)}",
            b { if item.title.is_empty() { "(untitled)" } else { "{item.title}" } }
            span { class: "spacer" }
            if !in_docket {
                button { r#type: "button", class: "badge dx-open-docket", title: "open this ticket in the work docket",
                    onclick: move |_| ctx.open_docket(Some(open_slug.clone())), "Open in docket" }
            }
        }
        div { class: if attention { "dim docket-pane-sub docket-pane-sub-attn" } else { "dim docket-pane-sub" },
            span { class: if attention { "docket-status status-{item.status} attention" } else { "docket-status status-{item.status}" },
                title: if attention { "" } else { status_help(&item.status).unwrap_or_default() }, "data-status": "{item.status}", "{label}" }
            " "
            span { class: "docket-slug-text", "{item.slug}" }
            " · Updated {ago(&updated)}"
            if let Some(owner) = owner.clone() {
                " · Assigned to "
                span { class: "docket-actor", AgentRef { id: owner.clone(), label: owner.clone() } }
            } else {
                " · Unassigned"
            }
            if item.status == "review" {
                if let Some(reviewer) = item.reviewer.as_ref().and_then(|r| r.get("node")).and_then(Value::as_str) {
                    " · Reviewer {reviewer}"
                }
            }
            if let Some(parent) = item.parent.clone() {
                " · Sub-item of "
                Prose { text: parent }
            }
        }
        if !participants.is_empty() {
            div { class: "docket-participants",
                span { class: "dim", "Participants" }
                for r in participants {
                    span { key: "{r.node}", class: "docket-participant", "{r.node}"
                        if r.state != "live" {
                            span { class: "dim", if r.state == "retired" { " (retired)" } else { " (unavailable)" } }
                        }
                    }
                }
            }
        }
        DocketSection { title: "DESCRIPTION", desc: true,
            if let Some(notice) = notice {
                div { class: format!("docket-desc-notice {}", text_of(&notice, "kind")),
                    div { class: "docket-desc-notice-head", {text_of(&notice, "headline")} }
                    div { class: "docket-desc-notice-detail", {text_of(&notice, "detail")} }
                }
            }
            match description {
                Some(html) => rsx! { div { class: "docket-desc-body md dx-docket-desc", dangerous_inner_html: html } },
                None => rsx! { div { class: "dim docket-list-empty", "no description — this item predates the rule that every item states its problem and proposed solution" } },
            }
        }
        if let Some((heading, text)) = state_info(&item) {
            DocketSection { title: heading, desc: true,
                match text {
                    Some(text) => rsx! { div { class: "docket-desc-body dx-state-reason", Prose { text } } },
                    None => rsx! { div { class: "dim docket-list-empty", "not recorded — this item entered {item.status} before the rule that the state says why" } },
                }
            }
        }
        ProgressList { heading: "DONE SO FAR", items: item.done_so_far.clone(), mark: "done" }
        ProgressList { heading: "WORKING ON / NEXT", items: item.working_on_next.clone(), mark: "next" }
        if detail.is_some() {
            DocketSection { title: "DECISIONS", summary: format!("{} · append-only", decisions.len()),
                if decisions.is_empty() {
                    div { class: "dim docket-list-empty", "None" }
                } else {
                    ol { class: "docket-list-items dx-decisions",
                        for d in decisions {
                            li { key: "{d.seq}", class: if d.superseded_by.is_some() { "dx-decision superseded" } else { "dx-decision" }, "data-seq": "{d.seq}",
                                Prose { text: d.text.clone().unwrap_or_default() }
                                span { class: "dim", " — {who(d.by.as_ref())}, {fmt_local(&d.at, tz.as_ref())}" }
                                if let Some(old) = d.supersedes {
                                    span { class: "dim", " · replaces #{old}" }
                                }
                                if let Some(new) = d.superseded_by {
                                    span { class: "dim", " · superseded by #{new}" }
                                }
                            }
                        }
                    }
                }
            }
            DocketSection { title: "EVIDENCE", summary: format!("{} of {EVIDENCE_MAX}", item.evidence.len()),
                if item.evidence.is_empty() {
                    div { class: "dim docket-list-empty", "None" }
                } else {
                    ul { class: "docket-receipt-list dx-evidence",
                        for (i, e) in item.evidence.iter().cloned().enumerate() {
                            li { key: "{i}", class: "docket-receipt",
                                span { class: "badge", "{e.kind}" }
                                if let Some(r) = e.reference.clone() {
                                    span { class: "docket-receipt-ref", " " code { "{r}" } }
                                }
                                if let Some(note) = e.note.clone() {
                                    " — "
                                    Prose { text: note }
                                }
                                span { class: "dim", " · {who(e.by.as_ref())}, {fmt_local(&e.at, tz.as_ref())}" }
                            }
                        }
                    }
                }
            }
            DocketSection { title: "ARTIFACTS", summary: format!("{} of {ARTIFACT_MAX}", artifacts.len()),
                if artifacts.is_empty() {
                    div { class: "dim docket-list-empty", "None" }
                } else {
                    div { class: "attach-row dx-artifacts",
                        for (n, a) in artifacts.into_iter().enumerate() {
                            FileChip { key: "{n}", slug: slug.clone(), artifact: a }
                        }
                    }
                }
            }
            if !item.attachments.is_empty() {
                DocketSection { title: "ATTACHMENTS",
                    div { class: "attach-row dx-attachments",
                        for a in item.attachments.clone() {
                            FileChip { key: "{a.id}", slug: slug.clone(),
                                artifact: Artifact { id: a.id.clone(), name: a.name.clone(), bytes: a.bytes, visible: Some(true), scope: "attachment".into(), ..Artifact::default() } }
                        }
                    }
                }
            }
        }
        if let Some(flag) = item.manual_attention.clone() {
            DocketSection { title: "MANUAL ATTENTION",
                div { class: "docket-attention-box",
                    div { class: "docket-question-head docket-attention-head",
                        span { "Manual attention from {who(flag.by.as_ref())}" }
                        if can_dismiss {
                            button { r#type: "button", class: "badge docket-dismiss", title: "clear this manually-raised flag",
                                onclick: move |_| ctx.dismiss(dismiss_item.clone()),
                                "Dismiss with no comment"
                            }
                        }
                    }
                    div { class: "docket-attention-body", Prose { text: flag.reason.clone() } }
                }
            }
        }
        for q in item.questions.clone() {
            DocketSection { key: "{q.ask_id}", title: format!("QUESTION FROM {}", q.node),
                div { class: "docket-question-box", "data-ask": "{q.ask_id}",
                    div { class: "docket-question-head", "Question from " AgentRef { id: q.node.clone(), label: q.node.clone() } }
                    match asks.get(&q.ask_id).cloned() {
                        Some(ask) => rsx! {
                            if ask.tabs.len() > q.tabs.len() {
                                div { class: "dim docket-question-note", "this batch also covers other items — answering it resolves every tab at once" }
                            }
                            crate::inbox::AskCard { ask }
                        },
                        None => rsx! { div { class: "dim", "this question is no longer open" } },
                    }
                }
            }
        }
        if staffable {
            StaffBox { key: "{slug}", slug: slug.clone() }
        }
        if detail.is_some() {
            if !earlier.is_empty() {
                DocketSection { title: "EARLIER HOLDERS", summary: format!("{}", earlier.len()),
                    ul { class: "docket-list-items dx-holders",
                        for (i, h) in earlier.into_iter().enumerate() {
                            li { key: "{i}", "data-holder": "{h.node}",
                                AgentRef { id: h.node.clone(), label: h.node.clone() }
                                span { class: "dim", " · from {fmt_local(h.from.as_deref().unwrap_or_default(), tz.as_ref())}" }
                                if h.derived == Some(true) {
                                    span { class: "dim", title: "recovered from retained history, not recorded at the assignment", " (derived)" }
                                }
                            }
                        }
                    }
                }
            }
            DocketSection { title: "HISTORY", summary: format!("{} of {HISTORY_MAX}", item.history.len()),
                ol { class: "docket-list-items dx-history",
                    for (i, h) in item.history.iter().cloned().enumerate().rev() {
                        li { key: "{i}", "data-op": text_of(&h, "op"),
                            span { class: "dim", {format!("{} · {} · ", fmt_local(&text_of(&h, "at"), tz.as_ref()), who(h.get("by")))} }
                            "{history_text(&h)}"
                        }
                    }
                }
            }
        }
        ReplyBox { item: item.clone(), recipients }
    }
}

#[component]
fn ProgressList(heading: String, items: Vec<String>, mark: String) -> Element {
    rsx! {
        DocketSection { title: heading.clone(),
            div { class: "docket-list",
                div { class: "docket-list-heading dim", "{heading}" }
                if items.is_empty() {
                    div { class: "dim docket-list-empty", "None" }
                } else {
                    ul { class: "docket-list-items mark-{mark}",
                        for (i, t) in items.into_iter().enumerate() {
                            li { key: "{i}", Prose { text: t } }
                        }
                    }
                }
            }
        }
    }
}

/// Un artefacto o un adjunto: se guarda en Descargas y se revela en el
/// Explorador (`download` en el renderer); nunca se abre ni se ejecuta.
#[component]
fn FileChip(slug: String, artifact: Artifact) -> Element {
    let ctx = use_context::<Ctx>();
    // en el scope de la vista, como las demás: la descarga termina en ese scope
    let mut busy = use_hook(|| Signal::new_in_scope(false, ctx.scope));
    if artifact.visible == Some(false) {
        return rsx! { span { class: "attach-chip dim", title: "a named artifact you hold no grant on", "{artifact.scope} artifact — not shared with you" } };
    }
    let is_attachment = artifact.scope == "attachment";
    let size = fmt_bytes(artifact.bytes);
    let title = if is_attachment { "save to Downloads and show in folder — never opened".to_string() } else { format!("{}\nimmutable — save to Downloads and show in folder; never opened", artifact.sha256) };
    let a = artifact.clone();
    rsx! {
        button { r#type: "button", class: "attach-chip dx-file-chip", title: "{title}", disabled: busy(), "data-file": "{artifact.name}",
            onclick: move |_| {
                busy.set(true);
                let (a, slug) = (a.clone(), slug.clone());
                ctx.spawn(async move {
                    let (client, org) = ctx.ids();
                    let bytes = if is_attachment { client.attachment_bytes(&org, &slug, &a.id).await } else { client.artifact_bytes(&org, &slug, &a.id).await };
                    let line = match bytes.map_err(|e| e.to_string()).and_then(|b| save_and_reveal(&org, &slug, &a.name, &b)) {
                        Ok(path) => format!("Shown in folder: {}", path.display()),
                        Err(why) => format!("could not save {}: {why}", a.name),
                    };
                    ctx.toast(vec![line], None);
                    let _ = busy.try_write().map(|mut b| *b = false);
                });
            },
            "⤓ {artifact.name}"
            span { class: "dim", " {size}" }
            if artifact.scope == "named" {
                span { class: "badge dim", {if artifact.grants_live.is_empty() { "not shared".to_string() } else { format!("shared with {}", artifact.grants_live.join(", ")) }} }
            }
        }
    }
}

/// Un campo de texto de un objeto JSON, o vacío.
fn text_of(value: &Value, key: &str) -> String {
    value.get(key).and_then(Value::as_str).unwrap_or_default().to_string()
}

/// ` 2 sub-items`, ` 1 sub-item`.
fn plural(n: usize, word: &str) -> String {
    format!(" {n} {word}{}", if n == 1 { "" } else { "s" })
}

/// Una opción del selector de destinatario, con su papel y su estado.
fn recipient_label(r: &orgtree_engine_client::Recipient) -> String {
    let role = if r.role == "owner" { " (assignee)" } else { "" };
    let state = match r.state.as_str() {
        "retired" => " — retired",
        "missing" => " — unavailable",
        _ => "",
    };
    format!("{}{role}{state}", r.node)
}

/// `fmtBytes` de `canvas/shared.ts`.
pub(crate) fn fmt_bytes(n: u64) -> String {
    if n < 1024 {
        format!("{n} B")
    } else if n < 1024 * 1024 {
        format!("{:.1} KB", n as f64 / 1024.0)
    } else {
        format!("{:.1} MB", n as f64 / 1_048_576.0)
    }
}

/// "Staff…" (`quickStaffEntry`) en el panel: el texto del motor, el modelo y
/// el esfuerzo. En modo `request` se le pide al asignado sin elegir modelo; en
/// los otros dos se contrata y el ticket queda asignado. Un reintento de la
/// misma elección repite su `request_id` (el motor no duplica la contratación).
#[component]
fn StaffBox(slug: String) -> Element {
    let ctx = use_context::<Ctx>();
    let mut preview = use_hook(|| Signal::new_in_scope(None::<Result<QuickStaffPreview, String>>, ctx.scope));
    let mut tier = use_hook(|| Signal::new_in_scope(String::new(), ctx.scope));
    let mut effort = use_hook(|| Signal::new_in_scope(String::new(), ctx.scope));
    let mut busy = use_hook(|| Signal::new_in_scope(false, ctx.scope));
    let mut feedback = use_hook(|| Signal::new_in_scope(None::<String>, ctx.scope));
    let ops = use_hook(|| Rc::new(RefCell::new(HashMap::<String, String>::new())));
    let load_slug = slug.clone();
    use_hook(move || {
        ctx.spawn(async move {
            let (client, org) = ctx.ids();
            let result = client.quick_staff_preview(&org, &load_slug).await.map_err(|e| e.to_string());
            let _ = preview.try_write().map(|mut p| *p = Some(result));
        })
    });
    let current = preview.read().clone();
    let body = match current {
        None => rsx! { div { class: "dim", "Loading current staffing choices…" } },
        Some(Err(e)) => rsx! { div { class: "dim", "Staffing options could not be loaded: {e}" } },
        Some(Ok(p)) => {
            let request = p.mode == "request";
            let models = p.models.clone();
            let chosen = models.iter().find(|m| m.tier == tier()).cloned();
            let efforts = chosen.as_ref().map(|m| m.efforts.clone()).unwrap_or_default();
            let ready = (request || chosen.is_some()) && !busy();
            let errors: Vec<String> = p.availability.as_ref().and_then(|a| a.get("errors")).and_then(Value::as_array).map(|e| e.iter().filter_map(|x| x.as_str().map(str::to_string)).collect()).unwrap_or_default();
            let go = {
                let (p, slug, ops) = (p.clone(), slug.clone(), ops.clone());
                move |_| {
                    let t = tier.peek().clone();
                    let e = effort.peek().clone();
                    let mut selection = QuickStaffSelection {
                        request_id: String::new(),
                        mode: p.mode.clone(),
                        configured_mode: p.configured_mode.clone(),
                        owner: p.owner.clone(),
                        tier: (!t.is_empty()).then_some(t),
                        effort: (!e.is_empty()).then_some(e),
                        account: None,
                    };
                    let op_key = serde_json::to_string(&selection).unwrap_or_default();
                    selection.request_id = ops.borrow_mut().entry(op_key).or_insert_with(new_uuid).clone();
                    busy.set(true);
                    let slug = slug.clone();
                    ctx.spawn(async move {
                        let (client, org) = ctx.ids();
                        let message = match client.quick_staff(&org, &slug, &selection).await {
                            Ok(result) => result.message,
                            Err(error) => format!("error: {error}"),
                        };
                        let _ = feedback.try_write().map(|mut f| *f = Some(message.clone()));
                        ctx.toast(vec![message], None);
                        let _ = busy.try_write().map(|mut b| *b = false);
                    });
                }
            };
            rsx! {
                div { class: "dim dx-staff-disclosure", "{p.disclosure}" }
                if models.is_empty() {
                    div { class: "dim", title: errors.join("; "), if errors.is_empty() { "No models available" } else { "Staffing options could not be loaded" } }
                }
                div { class: "row dx-staff-row",
                    if !models.is_empty() {
                        select { class: "docket-group-select dx-staff-tier", value: "{tier}", "aria-label": "Model",
                            onchange: move |e| { tier.set(e.value()); effort.set(String::new()) },
                            option { value: "", selected: tier().is_empty(), if request { "any model (the assignee decides)" } else { "choose a model" } }
                            for m in models.iter().cloned() {
                                option { key: "{m.tier}", value: "{m.tier}", selected: tier() == m.tier, title: "{m.seat} credits for the seat", "{m.tier} · {m.seat} cr" }
                            }
                        }
                    }
                    if !efforts.is_empty() {
                        select { class: "docket-group-select dx-staff-effort", value: "{effort}", "aria-label": "Effort",
                            onchange: move |e| effort.set(e.value()),
                            option { value: "", selected: effort().is_empty(), "default effort" }
                            for e in efforts.iter().cloned() {
                                option { key: "{e}", value: "{e}", selected: effort() == e, "{e}" }
                            }
                        }
                    }
                    button { r#type: "button", class: "badge dx-staff-go", disabled: !ready, onclick: go,
                        if busy() { "staffing…" } else if request { "Request staffing" } else { "Staff" }
                    }
                }
                if let Some(text) = feedback() {
                    div { class: "docket-staff-feedback", role: "status", "{text}" }
                }
            }
        }
    };
    rsx! {
        DocketSection { title: "STAFF", summary: "backlogged — not yet approached".to_string(),
            div { class: "dx-staff", {body} }
        }
    }
}

/// La caja de respuesta del ticket (`REPLY`): al dueño por defecto, o a un
/// participante elegido; "as a notice" la entrega sin despertar al agente.
/// Enter envía y Shift+Enter es un salto de línea; un envío que falla
/// devuelve el texto a la caja.
#[component]
fn ReplyBox(item: WorkItem, recipients: Vec<orgtree_engine_client::Recipient>) -> Element {
    let ctx = use_context::<Ctx>();
    let owner = item.owner_node();
    let mut draft = use_hook(|| Signal::new_in_scope(String::new(), ctx.scope));
    let mut busy = use_hook(|| Signal::new_in_scope(false, ctx.scope));
    let mut notice = use_hook(|| Signal::new_in_scope(false, ctx.scope));
    let initial = owner.clone().unwrap_or_default();
    let mut to = use_hook(|| Signal::new_in_scope(initial, ctx.scope));
    if recipients.is_empty() && owner.is_none() {
        return rsx! {
            DocketSection { title: "REPLY",
                div { class: "dim docket-reply-label", "nobody is assigned to this item — there is nobody to reply to" }
            }
        };
    }
    let target = to();
    let recipient = recipients.iter().find(|r| r.node == target).cloned();
    let unavailable = recipient.as_ref().is_none_or(|r| r.state == "missing");
    let picker = recipients.iter().any(|r| r.role == "participant") || target.is_empty() || Some(&target) != owner.as_ref();
    let slug = item.slug.clone();
    let send = move || {
        let text = draft.peek().trim().to_string();
        if text.is_empty() || *busy.peek() || unavailable {
            return;
        }
        busy.set(true);
        draft.set(String::new());
        let (slug, to_node, as_notice) = (slug.clone(), to.peek().clone(), *notice.peek());
        ctx.spawn(async move {
            if let Err(error) = ctx.reply_ticket_to(&slug, &text, Some(to_node).filter(|t| !t.is_empty()), as_notice).await {
                let _ = draft.try_write().map(|mut d| *d = text);
                ctx.toast(vec![format!("error: {error}")], None);
            }
            let _ = busy.try_write().map(|mut b| *b = false);
        });
    };
    let mut send_key = send.clone();
    let mut send_click = send.clone();
    rsx! {
        DocketSection { title: "REPLY",
            div { class: "dim docket-reply-label",
                if picker {
                    div { class: "docket-reply-picker",
                        "Reply to "
                        select { class: "docket-reply-select dx-reply-to", value: "{target}", "aria-label": "Reply to", disabled: busy(),
                            onchange: move |e| to.set(e.value()),
                            if target.is_empty() {
                                option { value: "", disabled: true, selected: true, "Choose a recipient" }
                            }
                            for r in recipients.iter().cloned() {
                                option { key: "{r.node}", value: "{r.node}", disabled: r.state == "missing", selected: r.node == target,
                                    {recipient_label(&r)}
                                }
                            }
                        }
                    }
                } else {
                    "Reply to {target} · assigned to this item"
                }
            }
            if unavailable {
                div { class: "dim docket-reply-note", role: "status",
                    if target.is_empty() { "Choose a recipient to send this draft." } else { "{target} is unavailable. Choose a recipient to send this draft." }
                }
            }
            if recipient.as_ref().is_some_and(|r| r.state == "retired") {
                div { class: "dim docket-reply-note", "{target} is retired — the reply waits for rehire." }
            }
            div { class: "mail-reply",
                textarea { rows: 2, value: "{draft}", placeholder: "reply to {target}…",
                    oninput: move |e| draft.set(e.value()),
                    onkeydown: move |e: KeyboardEvent| {
                        if e.key() == Key::Enter && !e.modifiers().shift() {
                            e.prevent_default();
                            send_key();
                        }
                    },
                }
                button { class: "mail-reply-send", disabled: draft().trim().is_empty() || busy() || unavailable, onclick: move |_| send_click(), "reply" }
            }
            label { class: "checkline dx-reply-notice", title: "deliver as a passive notice: the agent reads it without a turn being started",
                input { r#type: "checkbox", checked: notice(), onchange: move |e| notice.set(e.checked()) }
                "as a notice"
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(slug: &str, status: &str, owner: Option<&str>, parent: Option<&str>) -> WorkItem {
        WorkItem {
            slug: slug.into(),
            title: slug.into(),
            status: status.into(),
            owner: owner.map(|o| serde_json::json!({ "node": o })),
            parent: parent.map(str::to_string),
            ..WorkItem::default()
        }
    }

    fn world() -> RefWorld {
        RefWorld {
            org: "spike-fixture".into(),
            items: ["empaquetar-el-runtime", "comprimir-con-lzma", "a-b", "a-b-c"].into_iter().map(String::from).collect(),
            agents: ["worker", "jefe"].into_iter().map(String::from).collect(),
            loaded: true,
        }
    }

    #[test]
    fn el_backlog_y_el_archivo_van_al_final() {
        let none = HashSet::new();
        let active = vec![item("a", "blocked", Some("worker"), None), item("b", "open", None, None), item("c", "in_progress", Some("jefe"), None)];
        let backlog = vec![item("d", "backlogged", None, None)];
        let archived = vec![item("e", "dropped", Some("worker"), None)];
        let keys = |s: Vec<Section>| s.into_iter().map(|s| s.key).collect::<Vec<_>>();
        assert_eq!(keys(build_sections(Group::None, active.clone(), backlog.clone(), archived.clone(), &none)), ["all", "backlog", "archive"]);
        assert_eq!(keys(build_sections(Group::Status, active.clone(), backlog.clone(), archived.clone(), &none)), ["st:blocked", "st:in_progress", "st:open", "backlog", "archive"]);
        assert_eq!(keys(build_sections(Group::Agent, active, vec![], vec![], &none)), ["ag:worker", "ag:jefe", "ag:unassigned"]);
    }

    #[test]
    fn una_bandera_agrupa_en_atencion_salvo_que_se_este_descartando() {
        let mut flagged_item = item("a", "open", Some("worker"), None);
        flagged_item.manual_attention = Some(Default::default());
        let none = HashSet::new();
        assert_eq!(build_sections(Group::Status, vec![flagged_item.clone()], vec![], vec![], &none)[0].key, "st:attention");
        let dismissing: HashSet<String> = ["a".to_string()].into();
        assert_eq!(build_sections(Group::Status, vec![flagged_item], vec![], vec![], &dismissing)[0].key, "st:open");
    }

    #[test]
    fn los_subitems_van_bajo_su_padre() {
        let rows = nest_rows(&[item("hijo", "open", None, Some("padre")), item("otro", "open", None, None), item("padre", "open", None, None), item("ciclo", "open", None, Some("ciclo"))]);
        let order: Vec<(String, usize, usize)> = rows.into_iter().map(|(i, d, k)| (i.slug, d, k)).collect();
        assert_eq!(order, [("otro".into(), 0, 0), ("padre".into(), 0, 1), ("hijo".into(), 1, 0), ("ciclo".into(), 0, 0)]);
    }

    #[test]
    fn la_busqueda_mira_nombre_titulo_descripcion_y_avance() {
        let mut it = item("medir-la-memoria", "open", None, None);
        it.objective = Some("El **consumo** en reposo".into());
        it.done_so_far = vec!["armé el perfil".into()];
        assert!(matches(&it, "MEMORIA consumo"));
        assert!(matches(&it, "perfil"));
        assert!(!matches(&it, "memoria cpu"));
    }

    #[test]
    fn las_menciones_respetan_los_bordes() {
        let w = world();
        let parts = split_refs("ver `comprimir-con-lzma`, y a-b-c. Lo pidió jefe; no a-b.json ni x/a-b", &w);
        let links: Vec<&Part> = parts.iter().filter(|p| !matches!(p, Part::Text(_))).collect();
        assert_eq!(links, [&Part::Item("comprimir-con-lzma".into(), "comprimir-con-lzma".into()), &Part::Item("a-b-c".into(), "a-b-c".into()), &Part::Agent("jefe".into(), "jefe".into())]);
        // concatenar los tramos devuelve el texto
        let text: String = split_refs("hola a-b, chau", &w).iter().map(|p| match p { Part::Text(t) => t.clone(), Part::Item(_, l) | Part::Agent(_, l) => l.clone(), Part::Inert(t, _, _) => t.clone() }).collect();
        assert_eq!(text, "hola a-b, chau");
    }

    #[test]
    fn los_tokens_canonicos_se_resuelven_o_dicen_por_que_no() {
        let w = world();
        let parts = split_refs("@item:spike-fixture/empaquetar-el-runtime @item:otra-org/x @item:spike-fixture/no-existe @agent:spike-fixture/worker@2 @doc:spike-fixture/d1 @item:spike-fixture/a-b/extra", &w);
        let kinds: Vec<String> = parts
            .iter()
            .filter_map(|p| match p {
                Part::Item(s, _) => Some(format!("item {s}")),
                Part::Agent(s, _) => Some(format!("agent {s}")),
                Part::Inert(t, c, _) => Some(format!("{c} {t}")),
                Part::Text(_) => None,
            })
            .collect();
        assert_eq!(
            kinds,
            [
                "item empaquetar-el-runtime",
                "foreign @item:otra-org/x",
                "absent @item:spike-fixture/no-existe",
                "agent worker@2",
                "elsewhere @doc:spike-fixture/d1",
            ]
        );
        // un token pegado a una palabra no es una referencia
        assert!(split_refs("x@item:spike-fixture/a-b", &w).iter().all(|p| matches!(p, Part::Text(_))));
        // mientras la lista no llegó, un ticket desconocido está pendiente, no ausente
        let pending = RefWorld { loaded: false, ..w };
        assert!(matches!(&split_refs("@item:spike-fixture/zzz", &pending)[0], Part::Inert(_, "pending", _)));
    }

    #[test]
    fn el_markdown_enlaza_referencias_fuera_de_enlaces_y_bloques() {
        let w = world();
        let html = markdown_with("Ver `empaquetar-el-runtime` y [jefe](https://example.com).\n\n```\njefe\n```", Some(&|t: &str| refs_html(t, &w)));
        assert!(html.contains("<code><a href=\"#\" class=\"docket-ref\" data-ref-item=\"empaquetar-el-runtime\""), "{html}");
        assert!(!html.contains("data-ref-agent=\"jefe\""), "{html}");
        // el HTML inyectado en la prosa sigue saneado
        let html = markdown_with("jefe <img src=x onerror=alert(1)>", Some(&|t: &str| refs_html(t, &w)));
        assert!(html.contains("data-ref-agent=\"jefe\"") && !html.contains("onerror"), "{html}");
    }

    #[test]
    fn el_historial_en_palabras() {
        let row = serde_json::json!({ "at": "x", "by": "@user", "op": "assign", "from": { "node": "jefe" }, "to": { "node": "worker" }, "why": "assign" });
        assert_eq!(history_text(&row), "assign — from: jefe · to: worker");
        assert_eq!(history_text(&serde_json::json!({ "kind": "folded", "count": 7 })), "7 older rows summarised");
    }

    #[test]
    fn el_estado_trae_su_motivo() {
        let mut it = item("a", "blocked", None, None);
        it.blocked_reason = Some("Falta el certificado".into());
        assert_eq!(state_info(&it), Some(("BLOCKED BECAUSE".into(), Some("Falta el certificado".into()))));
        let mut dropped = item("b", "dropped", None, None);
        dropped.dropped_reason = Some("  ".into());
        assert_eq!(state_info(&dropped), Some(("ENDED WITHOUT COMPLETING — WHY".into(), None)));
        assert_eq!(state_info(&item("c", "open", None, None)), None);
    }

    #[test]
    fn un_nombre_de_archivo_no_sale_de_su_carpeta() {
        assert_eq!(safe_name("..\\..\\Windows\\system32\\evil.exe"), "evil.exe");
        assert_eq!(safe_name("../.bashrc"), "bashrc");
        assert_eq!(safe_name("a:b?.txt"), "a_b_.txt");
        assert_eq!(safe_name(""), "archivo");
    }

    #[test]
    fn el_request_id_es_un_uuid_v4() {
        let (a, b) = (new_uuid(), new_uuid());
        assert_ne!(a, b);
        assert_eq!(a.len(), 36);
        assert_eq!(&a[14..15], "4");
        assert!("89ab".contains(&a[19..20]));
    }
}
