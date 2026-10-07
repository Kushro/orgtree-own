//! Preferencias persistentes del shell (#20), como `apps/desktop/main/preferences.ts`
//! con `preferencesPatch` de `policy.ts`.
//!
//! - Viven en `preferences.json`, en la carpeta de la app (la del perfil, no la
//!   raíz de datos del motor), y se escriben de forma atómica: un temporal y
//!   `rename`, así que un corte a mitad de escritura deja el archivo anterior.
//! - Solo claves conocidas y con su tipo. Un parche con una clave desconocida o
//!   un valor inválido se rechaza entero, como en Electron; lo que hay en el
//!   archivo se filtra con las mismas reglas y lo inválido se ignora.
//! - `orgView` no se guarda: mueve una org entre `attentionOrgs` y
//!   `canvasOrgs` sobre la copia viva, para que una ventana con una copia vieja
//!   no borre lo que registró otra.
//! - Un `visualTheme` sin `visualThemeExplicit` cuenta como elección explícita.

use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};

const VISUAL_THEMES: [&str; 5] = ["orgtree", "claude", "codex", "antigravity", "openrouter"];
const CONTRAST_THEMES: [&str; 4] = ["charcoal", "light", "solarized-light", "obsidian-black"];
const BOOLEANS: [&str; 6] = ["exitOnClose", "startAtLogin", "automaticUpdates", "routineNotifications", "onboarded", "notificationsEnabled"];
const VIEW_LIMIT: usize = 200;

/// `DEFAULT_PREFERENCES` de `policy.ts`, salvo dos valores propios del spike:
/// `startAtLogin` y `automaticUpdates` empiezan apagados (una vista previa no
/// se agrega sola al inicio de Windows, y no hay updater).
pub fn defaults() -> Value {
    let mut preferences = json!({
        "notificationsEnabled": true,
        "visualTheme": "orgtree",
        "contrastTheme": "charcoal",
        "agentColorSource": "provider",
        "visualThemeExplicit": false,
        "exitOnClose": false,
        "startAtLogin": false,
        "automaticUpdates": false,
        "routineNotifications": false,
        "onboarded": false,
        "startupMode": "restore"
    });
    for (key, default) in crate::notifications::OPTIONS {
        preferences[key] = Value::Bool(default);
    }
    preferences
}

/// `isVisualTheme`: un tema de la lista o `custom:#rrggbb`.
pub fn is_visual_theme(value: &Value) -> bool {
    let Some(text) = value.as_str() else { return false };
    VISUAL_THEMES.contains(&text)
        || text.strip_prefix("custom:#").is_some_and(|hex| hex.len() == 6 && hex.bytes().all(|b| b.is_ascii_hexdigit()))
}

fn short_text(value: &Value) -> bool {
    value.as_str().is_some_and(|s| !s.is_empty() && s.chars().count() <= 256)
}

/// `preferencesPatch`: valida un parche entero o lo rechaza.
pub fn patch(value: &Value) -> Result<Map<String, Value>, String> {
    let Value::Object(input) = value else { return Err("Invalid preferences".into()) };
    let mut result = Map::new();
    for (key, val) in input {
        let ok = match key.as_str() {
            "visualTheme" => is_visual_theme(val),
            "contrastTheme" => val.as_str().is_some_and(|s| CONTRAST_THEMES.contains(&s)),
            "agentColorSource" => matches!(val.as_str(), Some("provider" | "organization")),
            "startupMode" => matches!(val.as_str(), Some("restore" | "homepage")),
            "orgView" => {
                short_text(&val["slug"]) && matches!(val["view"].as_str(), Some("attention" | "canvas"))
            }
            "attentionOrgs" | "canvasOrgs" => val.as_array().is_some_and(|l| l.len() <= VIEW_LIMIT && l.iter().all(short_text)),
            "visualThemeExplicit" => val.is_boolean(),
            other => {
                (BOOLEANS.contains(&other) || crate::notifications::OPTIONS.iter().any(|(k, _)| *k == other)) && val.is_boolean()
            }
        };
        if !ok {
            return Err(format!("Invalid preference: {key}"));
        }
        let val = match key.as_str() {
            "orgView" => json!({ "slug": val["slug"], "view": val["view"] }),
            "attentionOrgs" | "canvasOrgs" => {
                let mut seen = std::collections::HashSet::new();
                Value::Array(val.as_array().unwrap().iter().filter(|v| seen.insert(v.as_str().unwrap().to_string())).cloned().collect())
            }
            _ => val.clone(),
        };
        result.insert(key.clone(), val);
    }
    Ok(result)
}

pub struct Preferences {
    path: PathBuf,
    value: Value,
}

impl Preferences {
    /// Lee el archivo; uno ausente o dañado da los valores por defecto. Cada
    /// clave guardada pasa por las mismas reglas que un parche.
    pub fn load(path: &Path) -> Preferences {
        let mut value = defaults();
        let stored: Option<Value> = std::fs::read(path).ok().and_then(|b| serde_json::from_slice(&b).ok());
        if let Some(Value::Object(stored)) = stored {
            let migrate = stored.contains_key("visualTheme") && !stored.contains_key("visualThemeExplicit");
            for (key, val) in stored {
                let mut single = Map::new();
                single.insert(key.clone(), val);
                if let Ok(mut one) = patch(&Value::Object(single)) {
                    if key == "orgView" {
                        continue;
                    }
                    if let Some(v) = one.remove(&key) {
                        value[key.as_str()] = v;
                    }
                }
            }
            // Un tema guardado sin la marca es anterior a ella: fue elegido a mano.
            if migrate {
                value["visualThemeExplicit"] = Value::Bool(true);
            }
        }
        Preferences { path: path.to_path_buf(), value }
    }

    pub fn get(&self) -> Value {
        self.value.clone()
    }

    pub fn bool(&self, key: &str) -> bool {
        self.value.get(key) == Some(&Value::Bool(true))
    }

    /// Aplica un parche válido, lo escribe y devuelve las preferencias nuevas.
    pub fn set(&mut self, input: &Value) -> Result<Value, String> {
        let mut normalized = patch(input)?;
        let view = normalized.remove("orgView");
        normalized.remove("attentionOrgs");
        normalized.remove("canvasOrgs");
        let mut next = self.value.clone();
        if let Some(view) = view {
            let slug = view["slug"].as_str().unwrap_or_default().to_string();
            for (list, wanted) in [("attentionOrgs", "attention"), ("canvasOrgs", "canvas")] {
                let mut rows: Vec<Value> = next[list].as_array().cloned().unwrap_or_default();
                rows.retain(|v| v.as_str() != Some(slug.as_str()));
                if view["view"] == wanted {
                    rows.push(Value::String(slug.clone()));
                    if rows.len() > VIEW_LIMIT {
                        rows.drain(..rows.len() - VIEW_LIMIT);
                    }
                }
                next[list] = Value::Array(rows);
            }
        }
        if normalized.contains_key("visualTheme") && !normalized.contains_key("visualThemeExplicit") {
            normalized.insert("visualThemeExplicit".into(), Value::Bool(true));
        }
        for (key, val) in normalized {
            next[key.as_str()] = val;
        }
        let bytes = serde_json::to_vec_pretty(&next).map_err(|e| e.to_string())?;
        write_atomic(&self.path, &bytes).map_err(|e| format!("no se pudieron guardar las preferencias: {e}"))?;
        self.value = next;
        Ok(self.get())
    }
}

/// Escritura atómica: un temporal en la misma carpeta y `rename`.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut temp = path.as_os_str().to_owned();
    temp.push(".tmp");
    std::fs::write(&temp, bytes)?;
    std::fs::rename(&temp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_known_keys_with_their_types() {
        assert!(patch(&json!({})).unwrap().is_empty());
        assert!(patch(&json!({ "exitOnClose": true, "notifyAllMail": false, "visualTheme": "custom:#a0B1c2" })).is_ok());
        assert!(patch(&json!({ "exitOnClose": "yes" })).is_err());
        assert!(patch(&json!({ "nope": true })).is_err());
        assert!(patch(&json!({ "visualTheme": "custom:#12345" })).is_err());
        assert!(patch(&json!({ "startupMode": "restore", "contrastTheme": "light" })).is_ok());
        assert!(patch(&json!({ "orgView": { "slug": "a", "view": "other" } })).is_err());
        assert!(patch(&json!([1])).is_err());
    }

    #[test]
    fn persists_between_two_loads() {
        let dir = std::env::temp_dir().join(format!("orgtree-prefs-{}", std::process::id()));
        let path = dir.join("preferences.json");
        let _ = std::fs::remove_file(&path);
        let mut prefs = Preferences::load(&path);
        assert!(!prefs.bool("startAtLogin"));
        prefs.set(&json!({ "startAtLogin": true, "visualTheme": "codex", "orgView": { "slug": "a", "view": "attention" } })).unwrap();
        prefs.set(&json!({ "orgView": { "slug": "b", "view": "attention" } })).unwrap();
        prefs.set(&json!({ "orgView": { "slug": "a", "view": "canvas" } })).unwrap();
        assert!(prefs.set(&json!({ "bogus": 1 })).is_err());
        let again = Preferences::load(&path);
        assert!(again.bool("startAtLogin"));
        assert!(again.bool("visualThemeExplicit"));
        assert_eq!(again.get()["attentionOrgs"], json!(["b"]));
        assert_eq!(again.get()["canvasOrgs"], json!(["a"]));
        // Un archivo dañado da los valores por defecto.
        std::fs::write(&path, b"{ no es json").unwrap();
        assert_eq!(Preferences::load(&path).get(), defaults());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
