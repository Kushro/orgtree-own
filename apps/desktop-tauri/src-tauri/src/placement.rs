//! Dónde estaba cada ventana principal y cuáles se reabren al iniciar (#20),
//! como `apps/desktop/main/org-placement.ts` y `window-placement.ts`.
//!
//! Son dos datos distintos y se guardan aparte:
//!
//! - `geometry`: la última posición de cada ventana (`homepage` u `org:<slug>`),
//!   recordada siempre, para que abrir una org a mano la ponga donde estaba.
//! - `session`: las ventanas abiertas al salir, que el próximo arranque reabre.
//!
//! Cerrar una ventana a mano la saca de `session` pero conserva su posición.
//! Una salida guarda el conjunto abierto **antes** de cerrar nada
//! (`begin_shutdown`), y los cierres que siguen ya no cuentan.
//!
//! Las coordenadas son píxeles físicos (la posición exterior y el tamaño del
//! área cliente), los mismos que dan los monitores de Tauri. Al restaurar,
//! `fit_window` (el `fitWindow` de Electron) deja la ventana entera dentro del
//! área de trabajo del monitor con el que más se superpone, así que una
//! ventana guardada en un monitor que ya no existe vuelve a uno que sí.
//! El archivo se escribe de forma atómica (temporal y `rename`).

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const HOMEPAGE_KEY: &str = "homepage";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Bounds {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Placement {
    pub bounds: Bounds,
    pub maximized: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Saved {
    key: String,
    placement: Placement,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct File {
    #[serde(default)]
    version: u32,
    #[serde(default)]
    geometry: Vec<Saved>,
    #[serde(default)]
    session: Vec<String>,
}

/// La clave de una ventana: `homepage` u `org:<slug>`. Una ventana de
/// creación no se guarda, porque su borrador no se persiste.
pub fn key_of(kind: crate::orgwindows::Kind, org: Option<&str>) -> Option<String> {
    use crate::orgwindows::Kind;
    match (kind, org) {
        (Kind::Org, Some(org)) => Some(format!("org:{org}")),
        (Kind::Homepage, _) => Some(HOMEPAGE_KEY.into()),
        _ => None,
    }
}

pub fn org_of_key(key: &str) -> Option<&str> {
    key.strip_prefix("org:")
}

/// `fitWindow`: conserva el rectángulo exacto en su monitor; si el monitor ya
/// no está o cambió su área, lo lleva al área de trabajo con más superposición.
pub fn fit_window(bounds: Bounds, areas: &[Bounds], min: (u32, u32)) -> Bounds {
    let overlap = |a: &Bounds| {
        let w = (bounds.x as i64 + bounds.width as i64).min(a.x as i64 + a.width as i64) - (bounds.x as i64).max(a.x as i64);
        let h = (bounds.y as i64 + bounds.height as i64).min(a.y as i64 + a.height as i64) - (bounds.y as i64).max(a.y as i64);
        w.max(0) * h.max(0)
    };
    let Some(area) = areas.iter().copied().reduce(|best, a| if overlap(&a) > overlap(&best) { a } else { best }) else {
        return bounds;
    };
    let width = bounds.width.max(min.0).min(area.width);
    let height = bounds.height.max(min.1).min(area.height);
    let x = (bounds.x as i64).min(area.x as i64 + area.width as i64 - width as i64).max(area.x as i64) as i32;
    let y = (bounds.y as i64).min(area.y as i64 + area.height as i64 - height as i64).max(area.y as i64) as i32;
    Bounds { x, y, width, height }
}

pub struct Store {
    path: PathBuf,
    file: File,
    shutting_down: bool,
}

impl Store {
    /// Lee el archivo; uno dañado o ausente da un estado vacío, nunca un error.
    pub fn load(path: &Path) -> Store {
        let mut file: File = std::fs::read(path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        file.version = 2;
        file.geometry.retain(|g| !g.key.is_empty() && g.placement.bounds.width > 0 && g.placement.bounds.height > 0);
        let known: Vec<String> = file.geometry.iter().map(|g| g.key.clone()).collect();
        let mut seen = std::collections::HashSet::new();
        // La membresía nombra ventanas; una clave sin posición no se puede ubicar.
        file.session.retain(|k| known.contains(k) && seen.insert(k.clone()));
        Store { path: path.to_path_buf(), file, shutting_down: false }
    }

    /// Lo que el arranque reabre, en orden.
    pub fn session(&self) -> Vec<String> {
        self.file.session.clone()
    }

    pub fn restore(&self, key: &str, areas: &[Bounds], min: (u32, u32)) -> Option<Placement> {
        let saved = self.file.geometry.iter().find(|g| g.key == key)?.placement;
        Some(Placement { bounds: fit_window(saved.bounds, areas, min), maximized: saved.maximized })
    }

    /// Recuerda dónde está la ventana. Solo geometría: no dice si se reabre.
    pub fn capture(&mut self, key: &str, placement: Placement) {
        match self.file.geometry.iter_mut().find(|g| g.key == key) {
            Some(saved) if saved.placement == placement => return,
            Some(saved) => saved.placement = placement,
            None => self.file.geometry.push(Saved { key: key.into(), placement }),
        }
        self.write();
    }

    /// Solo cambia `maximized`, conservando el rectángulo normal (Tauri no da
    /// los límites normales de una ventana maximizada).
    pub fn capture_maximized(&mut self, key: &str, maximized: bool) {
        if let Some(saved) = self.file.geometry.iter_mut().find(|g| g.key == key) {
            if saved.placement.maximized != maximized {
                saved.placement.maximized = maximized;
                self.write();
            }
        }
    }

    pub fn opened(&mut self, key: &str) {
        if !self.file.session.iter().any(|k| k == key) {
            self.file.session.push(key.into());
            self.write();
        }
    }

    /// La persona cerró esta ventana: no se reabre, pero su posición queda.
    /// No hace nada una vez empezada la salida.
    pub fn closed(&mut self, key: &str) {
        if self.shutting_down {
            return;
        }
        let before = self.file.session.len();
        self.file.session.retain(|k| k != key);
        if self.file.session.len() != before {
            self.write();
        }
    }

    /// La app sale: guarda las ventanas abiertas ahora, antes de cerrar nada,
    /// y desde acá los cierres ya no cambian la sesión.
    pub fn begin_shutdown(&mut self, open: &[String]) {
        if self.shutting_down {
            return;
        }
        self.shutting_down = true;
        let mut seen = std::collections::HashSet::new();
        let next: Vec<String> = open
            .iter()
            .filter(|k| !k.is_empty() && self.file.geometry.iter().any(|g| &g.key == *k) && seen.insert((*k).clone()))
            .cloned()
            .collect();
        if next != self.file.session {
            self.file.session = next;
            self.write();
        }
    }

    fn write(&self) {
        let Ok(bytes) = serde_json::to_vec_pretty(&self.file) else { return };
        let _ = crate::preferences::write_atomic(&self.path, &bytes);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCREEN: Bounds = Bounds { x: 0, y: 0, width: 1920, height: 1040 };

    #[test]
    fn a_window_on_a_missing_monitor_comes_back_on_screen() {
        let lost = Bounds { x: 30000, y: -5000, width: 900, height: 600 };
        assert_eq!(fit_window(lost, &[SCREEN], (640, 480)), Bounds { x: 1020, y: 0, width: 900, height: 600 });
        let kept = Bounds { x: 100, y: 120, width: 900, height: 650 };
        assert_eq!(fit_window(kept, &[SCREEN], (640, 480)), kept);
        let second = Bounds { x: 1920, y: 0, width: 1280, height: 1000 };
        let on_second = Bounds { x: 2000, y: 50, width: 800, height: 600 };
        assert_eq!(fit_window(on_second, &[SCREEN, second], (640, 480)), on_second);
        let huge = Bounds { x: 0, y: 0, width: 5000, height: 100 };
        assert_eq!(fit_window(huge, &[SCREEN], (640, 480)), Bounds { x: 0, y: 0, width: 1920, height: 480 });
    }

    #[test]
    fn geometry_is_not_membership() {
        let dir = std::env::temp_dir().join(format!("orgtree-placement-{}", std::process::id()));
        let path = dir.join("window-placement.json");
        let _ = std::fs::remove_file(&path);
        let mut store = Store::load(&path);
        let place = Placement { bounds: Bounds { x: 10, y: 20, width: 800, height: 600 }, maximized: false };
        store.capture("org:a", place);
        store.capture("org:b", place);
        store.opened("org:a");
        store.opened("org:b");
        store.closed("org:b");
        assert_eq!(store.session(), vec!["org:a".to_string()]);
        store.begin_shutdown(&["org:a".into(), "org:a".into(), "org:x".into()]);
        store.closed("org:a");
        let again = Store::load(&path);
        assert_eq!(again.session(), vec!["org:a".to_string()]);
        assert_eq!(again.restore("org:b", &[SCREEN], (640, 480)).unwrap().bounds, place.bounds);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
