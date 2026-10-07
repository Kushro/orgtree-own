//! Ajustes de la app, proveedores y cuentas en RSX (#30).
//!
//! Sigue `canvas/accounts.tsx` (`AccountsPanel`, `ProviderSignIn`,
//! `TurnLimitSetting`), `canvas/accountsregistry.tsx`
//! (`AccountRegistrySection`, `AddAccountDialog`), `desktopsettings.tsx`,
//! `themes.tsx` y `settingskit.tsx`, con las mismas clases del renderer y su
//! CSS sin cambios. Los endpoints son los de `api.ts`.
//!
//! - **Proveedores:** lo que dice `/api/providers` del motor (instalado,
//!   sesión, tiers, el interruptor del usuario) y, al lado, lo que detecta el
//!   shell (`harnesses`), que es lo que el login puede correr.
//! - **Cuentas:** el registro (`/api/accounts`) de cada proveedor, con
//!   agregar, iniciar sesión, refrescar y quitar.
//! - **Runtime:** el límite de turnos y los tiempos de turno del motor, y
//!   las notificaciones del escritorio.
//! - **Display:** el tema, que se guarda en las preferencias del escritorio.
//!
//! Las tareas se lanzan en el scope del panel (la trampa de `spawn` de #26):
//! un diálogo que se cierra al enviar no cancela su pedido.

use crate::harnesses;
use crate::icons::SettingsIcon;
use crate::login::{self, LoginOptions, LoginStatus};
use dioxus::prelude::*;
use orgtree_engine_client::{AccountRegistry, AccountRow, Client, NewAccount, ProviderInfo, ProvidersPayload, RuntimeSettings};
use serde_json::Value;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

// ─── Tema ─────────────────────────────────────────────────────────────────────

/// `THEMES` de `themes.tsx`: id, nombre, acento, hover y suave.
pub const THEMES: [(&str, &str, &str, &str, &str); 5] = [
    ("orgtree", "Orgtree Grey", "#b6bdc8", "#d0d5dd", "rgba(182,189,200,.16)"),
    ("claude", "Claude Terracotta", "#d97757", "#e99b81", "rgba(217,119,87,.16)"),
    ("codex", "Codex Teal", "#22c4bd", "#64ddd7", "rgba(34,196,189,.16)"),
    ("antigravity", "Antigravity Blue", "#75a5ff", "#a3c3ff", "rgba(117,165,255,.16)"),
    ("openrouter", "OpenRouter Lavender", "#b69afa", "#d0baff", "rgba(182,154,250,.16)"),
];
/// Sin una elección explícita ni proveedores conocidos, Claude (`DEFAULT_THEME`).
const DEFAULT_THEME: &str = "claude";

/// El color de un tema `custom:#rrggbb`.
fn custom_color(theme: &str) -> Option<&str> {
    let color = theme.strip_prefix("custom:")?;
    (color.len() == 7 && color.starts_with('#') && color[1..].chars().all(|c| c.is_ascii_hexdigit())).then_some(color)
}

/// `isVisualTheme`: un tema de la lista o un color propio.
pub fn is_visual_theme(theme: &str) -> bool {
    THEMES.iter().any(|t| t.0 == theme) || custom_color(theme).is_some()
}

/// Las variables de `applyTheme`: acento, hover, suave y tinta.
fn theme_vars(theme: &str) -> (String, String, String, String) {
    if let Some(color) = custom_color(theme) {
        // `customTheme`: hover 25 % hacia el blanco y tinta según la luminancia
        let rgb: Vec<u32> = [1, 3, 5].iter().map(|i| u32::from_str_radix(&color[*i..i + 2], 16).unwrap_or(0)).collect();
        let hover: String = rgb.iter().map(|v| format!("{:02x}", (*v as f64 + (255.0 - *v as f64) * 0.25).round() as u32)).collect();
        let luminance = rgb[0] as f64 * 0.299 + rgb[1] as f64 * 0.587 + rgb[2] as f64 * 0.114;
        let ink = if luminance > 140.0 { "#17191d" } else { "#ffffff" };
        return (color.to_string(), format!("#{hover}"), format!("rgba({},{},{},.16)", rgb[0], rgb[1], rgb[2]), ink.to_string());
    }
    let t = THEMES.iter().find(|t| t.0 == theme).unwrap_or(&THEMES[1]);
    (t.2.to_string(), t.3.to_string(), t.4.to_string(), "#17191d".to_string())
}

/// La hoja que aplica un tema: las mismas variables que `applyTheme` pone
/// en el `<html>` del renderer.
pub fn theme_css(theme: &str) -> String {
    let (accent, hover, soft, ink) = theme_vars(theme);
    format!(
        ":root {{ --accent: {accent}; --accent-hover: {hover}; --accent-soft: {soft}; --accent-ink: {ink}; \
         --org-accent: {accent}; --org-accent-hover: {hover}; --org-accent-soft: {soft}; }}"
    )
}

/// El tema que corresponde a los proveedores instalados (`defaultThemeForProviders`).
static PROVIDER_DEFAULT: Mutex<Option<&'static str>> = Mutex::new(None);

fn theme_channel() -> &'static tokio::sync::watch::Sender<String> {
    static CHANNEL: OnceLock<tokio::sync::watch::Sender<String>> = OnceLock::new();
    CHANNEL.get_or_init(|| tokio::sync::watch::Sender::new(effective_theme(&crate::notify::prefs())))
}

/// Una elección explícita gana; si no, el primer proveedor instalado, como
/// `startThemeSync`.
fn effective_theme(prefs: &Value) -> String {
    let explicit = prefs.get("visualThemeExplicit").and_then(Value::as_bool) == Some(true);
    match prefs.get("visualTheme").and_then(Value::as_str) {
        Some(theme) if explicit && is_visual_theme(theme) => theme.to_string(),
        _ => PROVIDER_DEFAULT.lock().unwrap().unwrap_or(DEFAULT_THEME).to_string(),
    }
}

/// Las preferencias cambiaron: todas las ventanas toman el tema nuevo.
pub fn theme_changed(prefs: &Value) {
    theme_channel().send_replace(effective_theme(prefs));
}

/// `defaultThemeForProviders`: el tema inicial según el primer CLI instalado.
pub fn providers_known(payload: &ProvidersPayload) {
    let installed = |id: &str| payload.get(id).is_some_and(|p| p.status.installed);
    let theme = if installed("claude") {
        "claude"
    } else if installed("openai") {
        "codex"
    } else if installed("google") {
        "antigravity"
    } else {
        DEFAULT_THEME
    };
    *PROVIDER_DEFAULT.lock().unwrap() = Some(theme);
    theme_changed(&crate::notify::prefs());
}

/// El tema en uso, en una hoja propia de cada ventana (cada una tiene su
/// VirtualDom y recibe el cambio por el canal).
#[component]
pub fn ThemeStyle() -> Element {
    let mut theme = use_signal(|| theme_channel().borrow().clone());
    use_future(move || async move {
        let mut changes = theme_channel().subscribe();
        while changes.changed().await.is_ok() {
            let next = changes.borrow_and_update().clone();
            theme.set(next);
        }
    });
    let current = theme();
    rsx! {
        style { id: "dx-theme", "data-theme": "{current}", {theme_css(&current)} }
    }
}

// ─── Piezas de `settingskit.tsx` ──────────────────────────────────────────────

/// `SetToggle`: una fila booleana; la fila entera es el `<label>`.
#[component]
pub fn SetToggle(label: String, hint: Option<String>, checked: bool, disabled: bool, key_name: String, onchange: EventHandler<bool>) -> Element {
    rsx! {
        label { class: "set-row", "data-setting": "{key_name}",
            span { class: "set-lead",
                input { r#type: "checkbox", role: "switch", "aria-label": "{label}", checked, disabled,
                    onchange: move |e| onchange.call(e.checked()) }
            }
            span { class: "set-label", "{label}" }
            span { class: "set-control", span { class: if checked { "set-state on" } else { "set-state" }, if checked { "on" } else { "off" } } }
            if let Some(hint) = hint {
                span { class: "set-hint", "{hint}" }
            }
        }
    }
}

/// El grupo "Notifications" de `DesktopSettings` (#28): el interruptor
/// general y uno por tipo de `NOTIFICATION_OPTIONS`.
#[component]
pub fn NotificationsGroup() -> Element {
    let mut prefs = use_signal(crate::notify::prefs);
    let current = prefs();
    let master = current.get("notificationsEnabled").and_then(|v| v.as_bool()).unwrap_or(true);
    let mut put = move |key: &'static str, value: bool| {
        let mut patch = serde_json::Map::new();
        patch.insert(key.into(), Value::Bool(value));
        prefs.set(crate::notify::set_prefs(&patch));
    };
    let mut rows: Vec<(&'static str, &'static str, bool, Option<&'static str>)> =
        vec![("notificationsEnabled", "Notifications", master, Some("When off, all desktop notifications are suspended."))];
    for (key, label, _) in crate::notify::OPTIONS {
        let hint = (key == "notifyWhileFocused").then_some("When off, notifications pause while any Orgtree window has focus.");
        rows.push((key, label, current.get(key).and_then(|v| v.as_bool()).unwrap_or(false), hint));
    }
    rsx! {
        div { class: "set-group",
            div { class: "set-group-head", "Notifications" }
            for (key, label, on, hint) in rows {
                label { key: "{key}", class: "set-row", "data-pref": "{key}",
                    span { class: "set-lead",
                        input { r#type: "checkbox", role: "switch", "aria-label": "{label}", checked: on,
                            disabled: key != "notificationsEnabled" && !master,
                            onchange: move |e| put(key, e.checked()) }
                    }
                    span { class: "set-label", "{label}" }
                    span { class: "set-control", span { class: if on { "set-state on" } else { "set-state" }, if on { "on" } else { "off" } } }
                    if let Some(hint) = hint {
                        span { class: "set-hint", "{hint}" }
                    }
                }
            }
        }
    }
}

// ─── El panel ─────────────────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Tab {
    Providers,
    Runtime,
    Display,
}

const TABS: [(Tab, &str, &str); 3] = [(Tab::Providers, "providers", "Providers"), (Tab::Runtime, "runtime", "Runtime"), (Tab::Display, "display", "Display")];

/// Lo que comparten las piezas del panel. Las señales nacen en el panel, y
/// las tareas se lanzan en su scope.
#[derive(Clone, Copy)]
struct Panel {
    scope: ScopeId,
    client: Signal<Client>,
    providers: Signal<Option<Result<ProvidersPayload, String>>>,
    registry: Signal<Option<Result<AccountRegistry, String>>>,
    harnesses: Signal<Vec<harnesses::Harness>>,
    runtime: Signal<Option<RuntimeSettings>>,
    busy: Signal<bool>,
    notes: Signal<Vec<String>>,
    add_account: Signal<Option<String>>,
}

impl Panel {
    fn spawn(self, task: impl std::future::Future<Output = ()> + 'static) {
        dioxus::core::Runtime::current().spawn(self.scope, task);
    }

    /// Un aviso del panel (el `toast` de `AccountsPanel`).
    fn note(mut self, line: impl Into<String>) {
        if let Ok(mut notes) = self.notes.try_write() {
            notes.push(line.into());
            let excess = notes.len().saturating_sub(4);
            notes.drain(..excess);
        }
    }

    fn load_providers(mut self) {
        let client = self.client.peek().clone();
        self.harnesses.set(harnesses::detect());
        self.spawn(async move {
            let result = client.providers().await.map_err(|e| e.to_string());
            if let Ok(payload) = &result {
                providers_known(payload);
            }
            let _ = self.providers.try_write().map(|mut p| *p = Some(result));
        });
    }

    fn load_registry(self) {
        let client = self.client.peek().clone();
        let mut registry = self.registry;
        self.spawn(async move {
            let result = client.accounts().await.map_err(|e| e.to_string());
            let _ = registry.try_write().map(|mut r| *r = Some(result));
        });
    }

    fn load_runtime(self) {
        let client = self.client.peek().clone();
        let mut runtime = self.runtime;
        self.spawn(async move {
            match client.runtime_settings().await {
                Ok(value) => {
                    let _ = runtime.try_write().map(|mut r| *r = Some(value));
                }
                Err(e) => self.note(format!("error: {e}")),
            }
        });
    }

    /// Un ajuste del runtime: una clave por pedido, como cada `set…` de api.ts.
    fn put_runtime(mut self, key: &'static str, value: Value) {
        let client = self.client.peek().clone();
        let mut runtime = self.runtime;
        self.busy.set(true);
        self.spawn(async move {
            match client.set_runtime(key, value).await {
                Ok(saved) => {
                    let _ = runtime.try_write().map(|mut r| *r = Some(saved));
                }
                Err(e) => self.note(format!("error: {e}")),
            }
            let _ = self.busy.try_write().map(|mut b| *b = false);
        });
    }
}

/// `AccountsPanel` (App settings): Providers, Runtime y Display.
#[component]
pub fn AppSettings(onclose: EventHandler<()>, initial: Option<Tab>) -> Element {
    let client = crate::engine_client();
    let mut tab = use_signal(|| initial.unwrap_or(Tab::Providers));
    let panel = Panel {
        scope: dioxus::core::current_scope_id(),
        client: use_signal(|| client.clone()),
        providers: use_signal(|| None),
        registry: use_signal(|| None),
        harnesses: use_signal(Vec::new),
        runtime: use_signal(|| None),
        busy: use_signal(|| false),
        notes: use_signal(Vec::new),
        add_account: use_signal(|| None),
    };
    use_context_provider(|| panel);
    use_hook(move || {
        panel.load_providers();
        panel.load_registry();
        panel.load_runtime();
    });
    let current = tab();
    rsx! {
        div { class: "overlay dx-app-settings-overlay", onclick: move |_| onclose.call(()),
            div { class: "settings acct-panel dx-app-settings", role: "dialog", "aria-modal": "true", "aria-label": "App settings",
                onclick: move |e| e.stop_propagation(),
                h3 { SettingsIcon {} " App settings" }
                div { class: "app-settings-tabs", role: "tablist", "aria-label": "Application settings sections",
                    for (id, slug, label) in TABS {
                        button { key: "{slug}", r#type: "button", role: "tab", "data-tab": "{slug}",
                            "aria-selected": "{current == id}",
                            class: if current == id { "app-settings-tab on" } else { "app-settings-tab" },
                            onclick: move |_| tab.set(id),
                            "{label}"
                        }
                    }
                }
                for (i, line) in (panel.notes)().into_iter().enumerate() {
                    p { key: "{i}", class: if line.starts_with("error") { "ask-warn dx-settings-note" } else { "dim dx-settings-note" }, role: "status", "{line}" }
                }
                div { class: "app-settings-panel", role: "tabpanel",
                    match current {
                        Tab::Providers => rsx! { ProvidersTab {} },
                        Tab::Runtime => rsx! { RuntimeTab {} },
                        Tab::Display => rsx! { DisplayTab {} },
                    }
                }
                div { class: "row", button { class: "dx-settings-close", onclick: move |_| onclose.call(()), "close" } }
            }
            if let Some(provider) = (panel.add_account)() {
                AddAccountDialog { provider }
            }
        }
    }
}

// ─── Providers ────────────────────────────────────────────────────────────────

/// El nombre del proveedor para las cuentas (`LABELS` de accountsregistry.tsx).
fn provider_label(id: &str) -> &'static str {
    match id {
        "claude" => "Claude",
        "openai" => "Codex",
        "google" => "Antigravity",
        _ => "OpenRouter",
    }
}

/// El estado del proveedor como lo escribe el panel (`acct-provider-state`).
fn provider_state(p: &ProviderInfo) -> (&'static str, &'static str) {
    match (p.status.installed, p.status.connected) {
        (false, _) => ("missing", "Not installed"),
        (true, Some(true)) => ("connected", "Installed · connected"),
        (true, Some(false)) => ("requires-signin", "Installed · sign-in required"),
        (true, None) => ("unknown", "Installed · connection unknown"),
    }
}

#[component]
fn ProvidersTab() -> Element {
    let panel = use_context::<Panel>();
    let providers = (panel.providers)();
    let found = (panel.harnesses)();
    let list: Vec<ProviderInfo> = match &providers {
        Some(Ok(p)) => p.providers.iter().filter(|p| p.id != "openrouter").cloned().collect(),
        _ => Vec::new(),
    };
    let none_installed = matches!(&providers, Some(Ok(_))) && !list.iter().any(|p| p.status.installed);
    rsx! {
        match &providers {
            None => rsx! { p { class: "dim", "Detecting harnesses…" } },
            Some(Err(e)) => rsx! { p { class: "ask-warn", role: "alert", "{e}" } },
            Some(Ok(_)) => rsx! {
                div { class: "acct-providers-bar", role: "status",
                    span { class: "acct-providers-msg dim", "Installed harnesses and accounts" }
                    button { r#type: "button", class: "acct-secondary-btn acct-refresh-btn", "aria-label": "refresh provider status",
                        onclick: move |_| {
                            panel.load_providers();
                            panel.load_registry();
                        },
                        span { "refresh" }
                    }
                }
            },
        }
        if let Some(Err(e)) = (panel.registry)() {
            p { class: "ask-warn", role: "alert", "Could not load the account list: {e}" }
        }
        if none_installed {
            p { class: "ask-warn dx-no-harness", "No supported harness was found. Install and sign in to Claude Code, Codex or Antigravity to run agents." }
        }
        for provider in list {
            ProviderGroup { key: "{provider.id}", harness: found.iter().find(|h| Some(h.id) == harnesses::of_provider(&provider.id)).cloned(), provider }
        }
    }
}

#[component]
fn ProviderGroup(provider: ProviderInfo, harness: Option<harnesses::Harness>) -> Element {
    let mut panel = use_context::<Panel>();
    let p = provider.clone();
    let (state_class, state_text) = provider_state(&p);
    let enabled = p.user_enabled != Some(false);
    let id = p.id.clone();
    let switch_id = p.id.clone();
    let add_id = p.id.clone();
    let rows: Vec<AccountRow> = match (panel.registry)() {
        Some(Ok(registry)) => registry.accounts.into_iter().filter(|row| row.provider == p.id).collect(),
        _ => Vec::new(),
    };
    let link = harness.as_ref().map(|h| h.url);
    let open_link = move |_| {
        let Some(url) = link else { return };
        match crate::external::open(url) {
            Ok(url) => panel.note(format!("Opened in the browser: {url}")),
            Err(why) => panel.note(format!("error: {why}")),
        }
    };
    rsx! {
        div { class: "set-group acct-provider-group", "data-provider": "{id}",
            div { class: "set-group-head acct-provider-head prov-{id}",
                span { "{p.label}" span { class: "dim", " · {p.cli}" } }
                span { class: "set-head-right",
                    button { class: "acct-secondary-btn dx-add-account", onclick: move |_| panel.add_account.set(Some(add_id.clone())), "Add secondary account" }
                    if p.status.installed && enabled && !p.hire_enabled {
                        span { class: "acct-preview-tag", "preview" }
                    }
                    // `ProviderSwitch`: solo si está instalado, o si el usuario lo apagó
                    if p.status.installed || !enabled {
                        label { class: "provider-switch",
                            input { r#type: "checkbox", role: "switch", checked: enabled, disabled: (panel.busy)(),
                                "aria-label": "{p.label} enabled for new agents",
                                onchange: move |e| {
                                    let client = panel.client.peek().clone();
                                    let (provider, on) = (switch_id.clone(), e.checked());
                                    panel.busy.set(true);
                                    panel.spawn(async move {
                                        match client.set_provider_enabled(&provider, on).await {
                                            Ok(payload) => {
                                                let _ = panel.providers.try_write().map(|mut p| *p = Some(Ok(payload)));
                                            }
                                            Err(e) => panel.note(format!("error: {e}")),
                                        }
                                        let _ = panel.busy.try_write().map(|mut b| *b = false);
                                    });
                                }
                            }
                            span { if enabled { "on" } else { "off" } }
                        }
                    }
                }
            }
            div { class: "acct-provider-status",
                span { class: "acct-provider-state {state_class}", "{state_text}" }
                if let Some(v) = &p.status.version {
                    span { class: "acct-provider-meta", "version {v}" }
                }
                if let Some(email) = &p.status.email {
                    span { class: "acct-provider-meta", "account {email}" }
                }
                if let Some(kind) = &p.status.kind {
                    span { class: "acct-provider-meta", "sign-in {kind}" }
                }
            }
            if let Some(path) = &p.status.path {
                p { class: "dim mono acct-provider-path", "{path}" }
            }
            // Lo que detecta el shell: lo único que el login puede correr.
            if let Some(h) = &harness {
                p { class: "dim acct-provider-note dx-harness", "data-harness": "{h.id}", "data-detected": "{h.detected()}",
                    if h.detected() { "{h.label} CLI ({h.exe}) found on this PC — sign-in runs it here." } else { "{h.label} CLI ({h.exe}) not found on this PC." }
                }
                if !p.status.installed || !h.detected() {
                    button { class: "acct-provider-download dx-harness-link", "data-harness": "{h.id}", onclick: open_link, "Download {p.label} (official setup)" }
                }
            }
            if let Some(reason) = p.reason.as_ref().filter(|_| enabled) {
                p { class: "dim acct-provider-note", "{reason}" }
            }
            if p.tiers.is_empty() {
                p { class: "dim acct-provider-empty", "No model tiers reported" }
            } else {
                div { class: "acct-provider-tiers", "aria-label": "{p.label} model tiers",
                    div { class: "acct-provider-tier-title", "Model tiers" }
                    for t in p.tiers.iter() {
                        div { key: "{t.tier}", class: "acct-provider-tier",
                            span { class: "tier t-{t.tier}", "{t.letter}" }
                            span { class: "acct-provider-tier-name", {t.name.clone().unwrap_or_else(|| t.tier.clone())} }
                            span { class: "acct-provider-tier-model", "{t.model}" }
                            span { class: "acct-provider-tier-seat", "seat {t.seat}" }
                        }
                    }
                }
            }
            div { class: "provider-accounts", "aria-label": "{provider_label(&id)} accounts",
                for row in rows {
                    AccountRowView { key: "{row.id}", row }
                }
            }
        }
    }
}

/// `accountIdentity(accountDisplayId(row), email)`.
fn account_name(row: &AccountRow) -> String {
    let name = row.name.clone().filter(|n| !n.is_empty()).unwrap_or_else(|| if row.label.is_empty() { row.id.clone() } else { row.label.clone() });
    match row.email() {
        Some(email) => format!("{name} · {email}"),
        None => name,
    }
}

fn auth_text(auth: &str) -> &'static str {
    match auth {
        "authenticated" => "Signed in",
        "unauthenticated" => "Sign-in required",
        _ => "Sign-in not verified",
    }
}

/// Una fila de `AccountRegistrySection`.
#[component]
fn AccountRowView(row: AccountRow) -> Element {
    let panel = use_context::<Panel>();
    let mut busy = use_signal(|| false);
    // la puerta de login de la fila: Claude o Codex, nunca una clave
    let login: Option<&'static str> = match (row.credential.kind.as_str(), row.provider.as_str()) {
        ("token" | "apikey", _) => None,
        (_, "claude") => Some("claude"),
        (_, "openai") => Some("codex"),
        _ => None,
    };
    let name = account_name(&row);
    let mut refresh = {
        let (id, name) = (row.id.clone(), name.clone());
        move || {
            let client = panel.client.peek().clone();
            let (id, name) = (id.clone(), name.clone());
            busy.set(true);
            panel.spawn(async move {
                match client.account_identity(&id).await {
                    Ok(found) => panel.note(format!("{name}: {}", found.auth)),
                    Err(e) => panel.note(format!("error: {e}")),
                }
                panel.load_registry();
                let _ = busy.try_write().map(|mut b| *b = false);
            });
        }
    };
    let remove = {
        let (id, name, provider) = (row.id.clone(), name.clone(), row.provider.clone());
        move |_| {
            let client = panel.client.peek().clone();
            let (id, name, provider) = (id.clone(), name.clone(), provider.clone());
            busy.set(true);
            panel.spawn(async move {
                match client.remove_account(&id).await {
                    Ok(out) if !out.rebound.is_empty() => {
                        panel.note(format!("{name} removed — {} agent(s) moved to {} default", out.rebound.len(), provider_label(&provider)))
                    }
                    Ok(_) => panel.note(format!("{name} removed")),
                    Err(e) => panel.note(format!("error: {e}")),
                }
                panel.load_registry();
                let _ = busy.try_write().map(|mut b| *b = false);
            });
        }
    };
    let mut refresh_click = refresh.clone();
    let swatch = match row.provider.as_str() {
        "openai" => THEMES[2].2,
        "google" => THEMES[3].2,
        _ => THEMES[1].2,
    };
    let bound = row.bound.iter().map(|b| format!("{}/{}", b.org, b.node)).collect::<Vec<_>>().join(", ");
    let profile = if row.credential.default_config { None } else { row.credential.path.clone() };
    rsx! {
        div { class: "account-row", "data-account": "{row.id}", "data-auth": "{row.standing.auth}",
            div { class: "account-identity",
                span { class: "account-swatch", "aria-hidden": "true", style: "background: {swatch}" }
                strong { "{name}" }
                span { class: "dim", "{auth_text(&row.standing.auth)}" }
            }
            div { class: "account-management",
                if !row.bound.is_empty() {
                    span { class: "dim", title: "{bound}", "{row.bound.len()} agent(s)" }
                }
                if let Some(provider) = login {
                    ProviderSignIn { provider, connected: row.standing.auth == "authenticated", profile_dir: profile, account_id: Some(row.id.clone()),
                        onrefresh: move |_| refresh() }
                }
                button { class: "dx-account-refresh", disabled: busy(), onclick: move |_| refresh_click(), "refresh" }
                button { class: "dx-account-remove", disabled: busy() || row.ambient,
                    title: if row.ambient { "The default account cannot be removed" } else { "Remove this account" },
                    onclick: remove, "remove" }
            }
            if let Some(org) = &row.origin_org {
                div { class: "dim", "Available only to {org}" }
            }
        }
    }
}

/// `ProviderSignIn`: empezar, esperar el código (solo Claude), cancelar. El
/// hijo lo lanza el shell (`login.rs`), nunca el motor.
#[component]
pub fn ProviderSignIn(provider: &'static str, connected: bool, profile_dir: Option<String>, account_id: Option<String>, onrefresh: EventHandler<()>) -> Element {
    let mut status = use_signal(|| login::logins().status(provider));
    let mut code = use_signal(String::new);
    let mut warning = use_signal(|| None::<String>);
    let label = match provider {
        "claude" => "Claude",
        "codex" => "Codex",
        _ => "Antigravity",
    };
    // mientras corre, el estado se relee cada 800 ms; al terminar bien, se refresca la cuenta
    use_future(move || async move {
        let mut was_active = false;
        loop {
            tokio::time::sleep(Duration::from_millis(800)).await;
            let active = status.peek().active();
            if active || was_active {
                let next = login::logins().status(provider);
                let finished_ok = next.phase == "done" && next.ok == Some(true);
                if *status.peek() != next {
                    status.set(next);
                }
                if was_active && finished_ok {
                    onrefresh.call(());
                }
            }
            was_active = active;
        }
    });
    let begin = move |_| {
        let options = LoginOptions { profile_dir: profile_dir.clone(), account_id: account_id.clone() };
        let started = login::logins().start(provider, options, login::EngineAccess::current());
        if started.phase == "error" {
            let text = match started.error.as_deref() {
                Some("not-installed") => format!("{label} is not installed"),
                Some(e) => e.to_string(),
                None => "could not start sign-in".to_string(),
            };
            warning.set(Some(text));
        } else {
            warning.set(None);
        }
        status.set(started);
    };
    let cancel = move |_| {
        status.set(login::logins().cancel(provider));
    };
    let mut submit = move || {
        let text = code().trim().to_string();
        if text.is_empty() {
            return;
        }
        match login::logins().submit_code(provider, &text) {
            Ok(next) => status.set(next),
            Err(e) => warning.set(Some(e)),
        }
        code.set(String::new());
    };
    let s: LoginStatus = status();
    let failed = s.phase == "error" || (s.phase == "done" && s.ok == Some(false));
    rsx! {
        div { class: "acct-claude-login dx-signin", "data-provider": "{provider}", "data-phase": "{s.phase}",
            if s.phase == "awaiting_code" && login::supports_code(provider) {
                input { class: "acct-claude-code", placeholder: "verification code", "aria-label": "{label} verification code", value: "{code}",
                    oninput: move |e| code.set(e.value()),
                    onkeydown: move |e| if e.key() == Key::Enter { submit() } }
                button { disabled: code().trim().is_empty(), onclick: move |_| submit(), "Submit code" }
                button { class: "dx-signin-cancel", onclick: cancel, "Cancel" }
            } else if s.active() {
                span { class: "dim",
                    if login::supports_code(provider) { "Starting sign-in…" } else { "Waiting for the browser sign-in to {label}…" }
                }
                button { class: "dx-signin-cancel", onclick: cancel, "Cancel" }
            } else if s.phase == "done" && s.ok.is_none() {
                span { class: "dim", "Terminal opened — sign in there, then refresh." }
                button { onclick: move |_| onrefresh.call(()), "Refresh" }
            } else {
                button { class: "dx-signin-start", onclick: begin, if connected { "Sign in again" } else { "Sign in" } }
                if failed {
                    span { class: "ask-warn",
                        {warning().unwrap_or_else(|| if s.timed_out { "Sign-in timed out.".to_string() } else { "Sign-in did not complete.".to_string() })}
                    }
                }
            }
        }
    }
}

/// `AddAccountDialog`: una cuenta administrada, una carpeta importada o una
/// clave de API. La clave sale del campo al enviarla y nunca se vuelve a leer.
#[component]
fn AddAccountDialog(provider: String) -> Element {
    let mut panel = use_context::<Panel>();
    let mut path = use_signal(String::new);
    let mut key = use_signal(String::new);
    let mut error = use_signal(|| None::<String>);
    let mut busy = use_signal(|| false);
    let label = provider_label(&provider);
    let google = provider == "google";
    let mut create = {
        let provider = provider.clone();
        move |kind: &'static str| {
            let (p, k) = (path().trim().to_string(), key().trim().to_string());
            if (kind == "imported" && p.is_empty()) || (kind == "apikey" && k.is_empty()) {
                return;
            }
            let account = NewAccount {
                provider: provider.clone(),
                kind: kind.into(),
                path: (kind == "imported").then_some(p),
                key: (kind == "apikey").then_some(k),
            };
            // la clave sale de la vista en el acto
            key.set(String::new());
            busy.set(true);
            let client = panel.client.peek().clone();
            panel.spawn(async move {
                match client.add_account(&account).await {
                    Ok(row) => {
                        panel.note(format!("{} added", row.name.clone().unwrap_or(row.id)));
                        panel.load_registry();
                        let _ = panel.add_account.try_write().map(|mut a| *a = None);
                    }
                    Err(e) => {
                        let _ = error.try_write().map(|mut x| *x = Some(e.to_string()));
                        let _ = busy.try_write().map(|mut b| *b = false);
                    }
                }
            });
        }
    };
    let mut create2 = create.clone();
    let mut create3 = create.clone();
    rsx! {
        div { class: "overlay dx-add-account-overlay", onclick: move |e| { e.stop_propagation(); panel.add_account.set(None) },
            div { class: "settings add-account-dialog dx-add-account-dialog", role: "dialog", "aria-label": "Add secondary {label} account",
                onclick: move |e| e.stop_propagation(),
                h3 { "Add secondary {label} account" }
                if google {
                    section { class: "account-add-option account-upstream-note",
                        h4 { "Secondary subscription accounts unavailable" }
                        p { class: "dim", "Native secondary subscription-account setup is waiting on upstream Antigravity CLI support." }
                    }
                } else {
                    section { class: "account-add-option",
                        h4 { "Create a managed account" }
                        p { class: "dim", "Create a separate profile for this account, then sign in." }
                        button { class: "dx-create-managed", disabled: busy(), onclick: move |_| create("managed"), "Create managed account" }
                    }
                    section { class: "account-add-option",
                        h4 { "Import a folder" }
                        p { class: "dim", "Use an existing {label} profile folder." }
                        label { "Profile folder"
                            input { "aria-label": "Profile folder", value: "{path}", disabled: busy(), oninput: move |e| path.set(e.value()) }
                        }
                        div { class: "account-management",
                            button { disabled: busy() || path().trim().is_empty(), onclick: move |_| create2("imported"), "Import folder" }
                        }
                    }
                }
                section { class: "account-add-option",
                    h4 { "Use an API key" }
                    p { class: "dim", "Bill this account directly to API credit. It shows total spend instead of subscription limits, and is never used for fallback unless you turn that on." }
                    label { "API key"
                        input { "aria-label": "API key", r#type: "password", value: "{key}", disabled: busy(), oninput: move |e| key.set(e.value()) }
                    }
                    p { class: "dim", "Stored on this machine and never shown again." }
                    button { disabled: busy() || key().trim().is_empty(), onclick: move |_| create3("apikey"), "Add API-key account" }
                }
                if let Some(e) = error() {
                    p { class: "ask-warn", role: "alert", "{e}" }
                }
                button { disabled: busy(), onclick: move |_| panel.add_account.set(None), "Cancel" }
            }
        }
    }
}

// ─── Runtime ──────────────────────────────────────────────────────────────────

/// Los toggles de `Turns` y `Agent processes` en `AccountsPanel`: clave del
/// motor, texto, pista y si el motor los trae prendidos cuando faltan.
const RUNTIME_TOGGLES: [(&str, &str, &str, Option<&str>, bool); 5] = [
    ("enabled", "warming_enabled", "keep agent processes warm", Some("Keep supported harness processes ready between turns."), true),
    ("working_checkups_enabled", "working_checkups_enabled", "check on working agents after 20 minutes", None, true),
    ("wait_for_mcp_tools_enabled", "wait_for_mcp_tools_enabled", "wait until the MCP tool surface is ready", None, false),
    ("idle_docket_reminders_enabled", "idle_docket_reminders_enabled", "remind idle agents about unfinished docket items", None, false),
    ("blocked_docket_reminders_enabled", "blocked_docket_reminders_enabled", "also remind about blocked items when every ticket is blocked", None, false),
];

fn runtime_flag(runtime: &RuntimeSettings, field: &str, default: bool) -> bool {
    let value = match field {
        "warming_enabled" => runtime.warming_enabled,
        "working_checkups_enabled" => runtime.working_checkups_enabled,
        "wait_for_mcp_tools_enabled" => runtime.wait_for_mcp_tools_enabled,
        "idle_docket_reminders_enabled" => runtime.idle_docket_reminders_enabled,
        "blocked_docket_reminders_enabled" => runtime.blocked_docket_reminders_enabled,
        _ => None,
    };
    value.unwrap_or(default)
}

#[component]
fn RuntimeTab() -> Element {
    let panel = use_context::<Panel>();
    let runtime = (panel.runtime)();
    let busy = (panel.busy)();
    rsx! {
        DesktopGroup {}
        NotificationsGroup {}
        match runtime {
            None => rsx! { p { class: "dim", "loading…" } },
            Some(runtime) => rsx! {
                div { class: "set-group",
                    div { class: "set-group-head", "Agent processes" }
                    for (key, field, label, hint, default) in RUNTIME_TOGGLES.into_iter().take(1) {
                        SetToggle { key: "{key}", key_name: key, label, hint: hint.map(str::to_string), checked: runtime_flag(&runtime, field, default), disabled: busy,
                            onchange: move |on: bool| panel.put_runtime(key, Value::Bool(on)) }
                    }
                }
                div { class: "set-group",
                    div { class: "set-group-head", "Turns" }
                    TurnLimit { runtime: runtime.clone() }
                    for (key, field, label, hint, default) in RUNTIME_TOGGLES.into_iter().skip(1) {
                        SetToggle { key: "{key}", key_name: key, label, hint: hint.map(str::to_string), checked: runtime_flag(&runtime, field, default), disabled: busy,
                            onchange: move |on: bool| panel.put_runtime(key, Value::Bool(on)) }
                    }
                }
            },
        }
    }
}

/// El grupo "Desktop" de `DesktopSettings` (#25): arrancar con Windows (el
/// valor de `Run` de HKCU, con `--background`) y salir al cerrar la última
/// ventana. Se guardan en `preferences.json`, como el resto.
#[component]
fn DesktopGroup() -> Element {
    let mut prefs = use_signal(crate::notify::prefs);
    let current = prefs();
    let flag = |key: &str| current.get(key).and_then(|v| v.as_bool()).unwrap_or(false);
    let mut put = move |key: &'static str, value: bool| {
        let mut patch = serde_json::Map::new();
        patch.insert(key.into(), Value::Bool(value));
        prefs.set(crate::notify::set_prefs(&patch));
    };
    rsx! {
        div { class: "set-group dx-desktop-group",
            div { class: "set-group-head", "Desktop" }
            SetToggle { key_name: "startAtLogin", label: "start at login", hint: Some("Start quietly in the system tray.".to_string()),
                checked: flag("startAtLogin"), disabled: false, onchange: move |on: bool| put("startAtLogin", on) }
            SetToggle { key_name: "exitOnClose", label: "exit when the last window closes",
                hint: Some("Closing the main window keeps your other windows open.".to_string()),
                checked: flag("exitOnClose"), disabled: false, onchange: move |on: bool| put("exitOnClose", on) }
        }
    }
}

/// `TurnLimitSetting`: el límite de la máquina (1–512, por defecto 16), que
/// se guarda al salir del campo o con Enter.
#[component]
fn TurnLimit(runtime: RuntimeSettings) -> Element {
    let panel = use_context::<Panel>();
    let live = runtime.max_concurrent_turns.unwrap_or(16);
    let mut draft = use_signal(|| live.to_string());
    let mut seen = use_signal(|| live);
    if *seen.peek() != live {
        seen.set(live);
        draft.set(live.to_string());
    }
    let parsed = draft().trim().parse::<u32>().ok().filter(|n| (1..=512).contains(n));
    let commit = move || {
        if let Some(n) = draft().trim().parse::<u32>().ok().filter(|n| (1..=512).contains(n)) {
            if n != *seen.peek() {
                panel.put_runtime("max_concurrent_turns", Value::from(n));
            }
        }
    };
    let slots = runtime.turn_slots.as_ref().map(|s| format!(" Now: {} running, {} waiting.", s.held, s.waiting)).unwrap_or_default();
    rsx! {
        div { class: "set-row", "data-setting": "max_concurrent_turns",
            span { class: "set-label", "most agent turns running at once" }
            span { class: "set-control",
                input { id: "app-settings-max-concurrent-turns", r#type: "number", min: "1", max: "512",
                    "aria-label": "most agent turns running at once", value: "{draft}", "aria-invalid": "{parsed.is_none()}",
                    disabled: (panel.busy)(),
                    oninput: move |e| draft.set(e.value()),
                    onblur: move |_| commit(),
                    onkeydown: move |e| if e.key() == Key::Enter { commit() } }
            }
            span { class: "set-hint",
                "Default 16, shared by every organization on this machine. Extra turns wait their turn: first come, first served within an organization, and taking turns across organizations. A change applies at once; lowering it lets running turns finish.{slots}"
            }
        }
    }
}

// ─── Display ──────────────────────────────────────────────────────────────────

/// `ThemeSetting`: el tema guardado en las preferencias del escritorio, con
/// un color propio.
#[component]
fn DisplayTab() -> Element {
    let mut prefs = use_signal(crate::notify::prefs);
    let current = effective_theme(&prefs());
    let custom = custom_color(&current).map(str::to_string);
    let mut color = use_signal(|| custom.clone().unwrap_or_else(|| "#b6bdc8".into()));
    let mut put = move |theme: String| {
        if !is_visual_theme(&theme) {
            return;
        }
        let mut patch = serde_json::Map::new();
        patch.insert("visualTheme".into(), Value::String(theme));
        patch.insert("visualThemeExplicit".into(), Value::Bool(true));
        prefs.set(crate::notify::set_prefs(&patch));
    };
    let selected = if custom.is_some() { "custom".to_string() } else { current.clone() };
    rsx! {
        div { class: "set-group",
            div { class: "set-group-head", "Appearance" }
            div { class: "set-row", "data-setting": "visualTheme",
                span { class: "set-label", "visual theme" }
                span { class: "set-control",
                    select { "aria-label": "Visual theme", class: "dx-theme-select", value: "{selected}",
                        onchange: move |e| {
                            let v = e.value();
                            put(if v == "custom" { format!("custom:{}", color()) } else { v });
                        },
                        for (id, label, ..) in THEMES {
                            option { key: "{id}", value: "{id}", selected: selected == id, "{label}" }
                        }
                        option { value: "custom", selected: selected == "custom", "Custom" }
                    }
                }
                span { class: "set-hint", "Choose an accent for the desk. Provider badges and work status keep their own colors." }
            }
            if custom.is_some() {
                div { class: "set-row",
                    span { class: "set-label", "custom color" }
                    span { class: "set-control",
                        input { r#type: "color", "aria-label": "Custom theme color", value: "{color}",
                            onchange: move |e| {
                                color.set(e.value());
                                put(format!("custom:{}", e.value()));
                            } }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn temas_como_el_renderer() {
        assert!(is_visual_theme("codex") && is_visual_theme("custom:#22C4bd"));
        assert!(!is_visual_theme("custom:#12345") && !is_visual_theme("rojo") && !is_visual_theme("custom:red;}"));
        assert!(theme_css("codex").contains("--accent: #22c4bd"));
        // customTheme: hover 25 % hacia el blanco, tinta oscura sobre un color claro
        let (accent, hover, soft, ink) = theme_vars("custom:#ffffff");
        assert_eq!((accent.as_str(), hover.as_str(), soft.as_str(), ink.as_str()), ("#ffffff", "#ffffff", "rgba(255,255,255,.16)", "#17191d"));
        assert_eq!(theme_vars("custom:#000000").3, "#ffffff");
    }
}
