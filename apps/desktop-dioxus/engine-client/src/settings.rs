//! Tipos de proveedores, cuentas y ajustes (#30), derivados de
//! `apps/desktop/renderer/src/types.ts` (`ProvidersPayload`, `ProviderInfo`,
//! `RuntimeSettingsPayload`, `SettingsRequest`) y de
//! `canvas/accountsregistry.tsx` (`AccountRow`).
//!
//! Como el resto del cliente, son permisivos: lo que la vista no lee queda en
//! `extra`, y un proveedor con una forma inesperada no tira abajo la lista.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::BTreeMap;

/// Una lista en la que cada elemento con una forma inesperada se salta: un
/// proveedor o una cuenta nueva y rara no borra a los demás de la vista.
fn skip_bad<'de, D, T>(deserializer: D) -> Result<Vec<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::de::DeserializeOwned,
{
    let value = Value::deserialize(deserializer)?;
    Ok(match value {
        Value::Array(items) => items.into_iter().filter_map(|item| serde_json::from_value(item).ok()).collect(),
        _ => Vec::new(),
    })
}

/// `GET /api/providers`: cada proveedor con su CLI, sus tiers y su estado en
/// esta máquina. Los dos mapas por proveedor (`apikey_fallback`,
/// `subscription_inference`) faltan en un motor viejo, que no es lo mismo
/// que apagados.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ProvidersPayload {
    #[serde(default, deserialize_with = "skip_bad")]
    pub providers: Vec<ProviderInfo>,
    #[serde(default)]
    pub apikey_fallback: Option<BTreeMap<String, bool>>,
    #[serde(default)]
    pub subscription_inference: Option<BTreeMap<String, bool>>,
}

impl ProvidersPayload {
    pub fn get(&self, id: &str) -> Option<&ProviderInfo> {
        self.providers.iter().find(|p| p.id == id)
    }
}

/// Un proveedor (`ProviderInfo`): `claude`, `openai`, `google` u `openrouter`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ProviderInfo {
    pub id: String,
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub cli: String,
    #[serde(default)]
    pub tiers: Vec<ProviderTier>,
    #[serde(default)]
    pub status: ProviderStatus,
    #[serde(default)]
    pub hire_enabled: bool,
    #[serde(default)]
    pub reason: Option<String>,
    /// El interruptor del usuario. Ausente es encendido: un motor viejo no
    /// puede leerse como todo apagado.
    #[serde(default)]
    pub user_enabled: Option<bool>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// La decisión de `familyOffer` (`canvas/shared.ts`) para una familia de
/// tiers en las superficies de contratación.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Offer {
    /// Se ofrece.
    Offer,
    /// Instalado pero no contratable (sin sesión, por ejemplo): se ve, con su motivo.
    Disable,
    /// No instalado, o apagado por el usuario: no aparece.
    Hide,
}

impl ProviderInfo {
    /// `familyOffer(hireOf(p))`: apagado por el usuario o no instalado se
    /// oculta; instalado sin poder contratar se deshabilita.
    pub fn offer(&self) -> Offer {
        if self.user_enabled == Some(false) {
            Offer::Hide
        } else if self.hire_enabled {
            Offer::Offer
        } else if self.status.installed {
            Offer::Disable
        } else {
            Offer::Hide
        }
    }
}

/// Un tier de un proveedor (`ProviderTier`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ProviderTier {
    pub tier: String,
    #[serde(default)]
    pub provider: String,
    #[serde(default)]
    pub seat: f64,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub letter: String,
    #[serde(default)]
    pub name: Option<String>,
}

/// `status` de un proveedor: si el CLI está, su versión y la sesión.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ProviderStatus {
    #[serde(default)]
    pub installed: bool,
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub source: Option<String>,
    #[serde(default)]
    pub connected: Option<bool>,
    #[serde(default)]
    pub email: Option<String>,
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// `GET/PUT /api/app-settings/runtime` (`RuntimeSettingsPayload`): el
/// comportamiento de la máquina, nunca estado de una org.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RuntimeSettings {
    #[serde(default)]
    pub max_concurrent_turns: Option<u32>,
    #[serde(default)]
    pub turn_slots: Option<TurnSlots>,
    #[serde(default)]
    pub warming_enabled: Option<bool>,
    /// Revisar a un agente que lleva 20 minutos trabajando.
    #[serde(default)]
    pub working_checkups_enabled: Option<bool>,
    #[serde(default)]
    pub wait_for_mcp_tools_enabled: Option<bool>,
    #[serde(default)]
    pub idle_docket_reminders_enabled: Option<bool>,
    #[serde(default)]
    pub blocked_docket_reminders_enabled: Option<bool>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// La cola de turnos de la máquina en este momento.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct TurnSlots {
    #[serde(default)]
    pub limit: u32,
    #[serde(default)]
    pub held: u32,
    #[serde(default)]
    pub waiting: u32,
}

/// `GET /api/accounts`: el registro de cuentas de la máquina, simétrico entre
/// orgs, con su estado de sesión y sus agentes.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AccountRegistry {
    #[serde(default, deserialize_with = "skip_bad")]
    pub accounts: Vec<AccountRow>,
    #[serde(default)]
    pub primary: Option<Value>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// Una cuenta del registro (`AccountRow` de `accountsregistry.tsx`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AccountRow {
    pub id: String,
    /// `claude`, `openai` o `google`.
    #[serde(default)]
    pub provider: String,
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub name: Option<String>,
    /// La cuenta por defecto del proveedor: no se puede quitar.
    #[serde(default)]
    pub ambient: bool,
    #[serde(default)]
    pub credential: Credential,
    /// `apikey` para una cuenta con clave de API; ausente es suscripción.
    #[serde(default)]
    pub mode: Option<String>,
    #[serde(default)]
    pub enabled: Option<bool>,
    #[serde(default)]
    pub identity: BTreeMap<String, Value>,
    #[serde(default)]
    pub tint_ordinal: u32,
    #[serde(default)]
    pub origin_org: Option<String>,
    #[serde(default)]
    pub standing: Standing,
    #[serde(default, deserialize_with = "crate::attention::lenient")]
    pub bound: Vec<Binding>,
}

impl AccountRow {
    /// El correo de la identidad, si el proveedor lo dio.
    pub fn email(&self) -> Option<&str> {
        self.identity.get("email").and_then(Value::as_str).filter(|e| !e.is_empty())
    }
}

/// `credential` de una cuenta: el tipo y la carpeta de perfil (nunca un secreto).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Credential {
    /// `managed`, `imported`, `token` o `apikey`.
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub default_config: bool,
}

/// El estado de sesión de una cuenta: `authenticated`, `unauthenticated` o `unobserved`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Standing {
    #[serde(default)]
    pub auth: String,
    #[serde(default)]
    pub state: Option<String>,
}

/// Un agente que usa la cuenta.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Binding {
    #[serde(default)]
    pub org: String,
    #[serde(default)]
    pub node: String,
    #[serde(default)]
    pub state: String,
}

/// `POST /api/accounts`: una cuenta nueva. `managed` crea un perfil propio,
/// `imported` usa una carpeta que ya existe y `apikey` guarda una clave
/// (que el motor nunca devuelve).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct NewAccount {
    pub provider: String,
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
}

/// `GET /api/accounts/{id}/identity`: quién es la cuenta, leído de su perfil.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AccountIdentity {
    #[serde(default)]
    pub account: String,
    #[serde(default)]
    pub auth: String,
    #[serde(default)]
    pub identity: Map<String, Value>,
}

/// `DELETE /api/accounts/{id}`: la cuenta quitada y los agentes que pasaron a
/// la cuenta por defecto del proveedor.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RemovedAccount {
    #[serde(default)]
    pub removed: String,
    #[serde(default, deserialize_with = "crate::attention::lenient")]
    pub rebound: Vec<Binding>,
}

/// Los ajustes de la org que muestra el panel (la pestaña Basic y las
/// políticas de `SettingsPanel` en `App.tsx`), leídos del árbol.
#[derive(Debug, Clone, PartialEq)]
pub struct OrgSettings {
    pub max_top_grant: u64,
    pub default_top_grant: u64,
    /// Porcentaje entero (50–95); el árbol lo trae como fracción.
    pub compact_at: u32,
    /// `""` es el valor por defecto del CLI.
    pub default_effort: String,
    pub cascade_hire: bool,
    pub cascade_alloc: bool,
    pub headless: bool,
}

impl OrgSettings {
    /// Con los mismos valores por defecto que el panel del renderer.
    pub fn from_tree(tree: &crate::TreePayload) -> OrgSettings {
        let get = |key: &str| tree.extra.get(key).filter(|v| !v.is_null());
        OrgSettings {
            max_top_grant: get("max_top_grant").and_then(Value::as_f64).map(|v| v as u64).unwrap_or(1000),
            default_top_grant: get("default_top_grant").and_then(Value::as_f64).map(|v| v as u64).unwrap_or(50),
            compact_at: get("compact_at").and_then(Value::as_f64).map(|v| (v * 100.0).round() as u32).unwrap_or(80),
            default_effort: get("default_effort").and_then(Value::as_str).unwrap_or_default().to_string(),
            cascade_hire: get("cascade_hire").and_then(Value::as_bool) != Some(false),
            cascade_alloc: get("cascade_alloc").and_then(Value::as_bool) != Some(false),
            headless: get("headless").and_then(Value::as_bool) == Some(true),
        }
    }

    /// El cuerpo de `POST /api/orgs/{slug}/settings` (`saveSettings`), con las
    /// mismas claves que manda el botón de guardar del renderer.
    pub fn request(&self) -> Value {
        serde_json::json!({
            "max_top_grant": self.max_top_grant,
            "default_top_grant": self.default_top_grant,
            "compact_at": self.compact_at,
            "default_effort": self.default_effort,
            "cascade_hire": self.cascade_hire,
            "cascade_alloc": self.cascade_alloc,
        })
    }
}

/// `POST /api/orgs/{slug}/settings` (`SettingsResult`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SettingsResult {
    #[serde(default)]
    pub warnings: Vec<String>,
    #[serde(default)]
    pub freezes_cleared: Vec<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// `GET /api/orgs/{slug}/orgmd` (`OrgMdPayload`): el charter de la org.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct OrgMd {
    #[serde(default)]
    pub content: String,
    #[serde(default)]
    pub chars: Option<u64>,
    #[serde(default)]
    pub prompt_max: Option<u64>,
    /// Si la lectura se cortó, el editor no se habilita: guardar lo cortado
    /// borraría el final del archivo.
    #[serde(default)]
    pub read_truncated: bool,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}
