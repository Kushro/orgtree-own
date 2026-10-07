//! Motor empaquetado: el runtime de Python, el motor, PostgreSQL y el
//! renderer que trae el instalador, con la misma disposición que los
//! `extraResources` de Electron (`package.json`):
//!
//! ```text
//! <recursos>/engine/launch.py
//! <recursos>/engine/runtime/python.exe          tools/provision-runtime.py
//! <recursos>/engine/pg-custodian.exe            tools/provision-postgres.py
//! <recursos>/engine/postgresql/bin/*.exe
//! <recursos>/tools/pypg/pgimport.py             conversión de una raíz 2.x
//! <recursos>/ui/index.html                      renderer construido
//! ```
//!
//! Replica `apps/desktop/main/postgres-runtime.ts`: las rutas de PostgreSQL
//! (`ORGTREE_PG_CUSTODIAN`, `ORGTREE_P03_PG_BIN`), `ORGTREE_PG_BOOTSTRAP=1` y
//! el descriptor `engine-paths.json` (`writeEnginePaths`).

use crate::{EngineError, EngineOptions};
use std::path::{Path, PathBuf};

/// Las herramientas de PostgreSQL que exige el paquete (las mismas que
/// `postgresRuntimeEnvironment` y `PgBin::locate` en pg-custodian).
pub const PG_TOOLS: [&str; 5] = ["postgres.exe", "pg_ctl.exe", "initdb.exe", "psql.exe", "pg_controldata.exe"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackagedRuntime {
    pub engine_dir: PathBuf,
    pub python: PathBuf,
    pub custodian: PathBuf,
    pub pg_bin: PathBuf,
    pub importer: PathBuf,
    pub ui_dir: PathBuf,
}

impl PackagedRuntime {
    /// `None` si `resources` no trae el runtime embebido (no es una app
    /// empaquetada). Si lo trae pero falta otra pieza, es un error: un
    /// paquete roto no debe caer en el modo de desarrollo.
    pub fn locate(resources: &Path) -> Option<Result<PackagedRuntime, EngineError>> {
        // Tauri da la carpeta de recursos con el prefijo `\\?\` en Windows;
        // Electron pasa rutas comunes, y el motor y PostgreSQL las esperan así.
        let resources = &PathBuf::from(crate::canonical_display(resources));
        let engine_dir = resources.join("engine");
        let python = engine_dir.join("runtime").join("python.exe");
        if !python.is_file() {
            return None;
        }
        Some(Self::complete(resources, engine_dir, python))
    }

    fn complete(resources: &Path, engine_dir: PathBuf, python: PathBuf) -> Result<PackagedRuntime, EngineError> {
        if !resources.is_absolute() {
            return Err(EngineError::Config("la carpeta de recursos debe ser absoluta".into()));
        }
        let runtime = PackagedRuntime {
            custodian: engine_dir.join("pg-custodian.exe"),
            pg_bin: engine_dir.join("postgresql").join("bin"),
            importer: resources.join("tools").join("pypg").join("pgimport.py"),
            ui_dir: resources.join("ui"),
            engine_dir,
            python,
        };
        let mut required = vec![runtime.engine_dir.join("launch.py"), runtime.custodian.clone(), runtime.importer.clone()];
        required.extend(PG_TOOLS.iter().map(|tool| runtime.pg_bin.join(tool)));
        required.push(runtime.ui_dir.join("index.html"));
        if let Some(missing) = required.iter().find(|file| !file.is_file()) {
            return Err(EngineError::Config(format!("falta un archivo del paquete: {}", missing.display())));
        }
        Ok(runtime)
    }

    /// Opciones de arranque como las de Electron empaquetado: el Python
    /// embebido, el renderer del paquete, las rutas de PostgreSQL y el
    /// bootstrap de una raíz nueva sobre PostgreSQL.
    pub fn options(&self, data_root: impl Into<PathBuf>) -> EngineOptions {
        let mut options = EngineOptions::new(&self.python, &self.engine_dir, data_root);
        options.ui_dir = Some(self.ui_dir.clone());
        options.bootstrap_postgres = true;
        options.env = vec![
            ("ORGTREE_PG_CUSTODIAN".into(), self.custodian.clone().into_os_string()),
            ("ORGTREE_P03_PG_BIN".into(), self.pg_bin.clone().into_os_string()),
        ];
        options
    }

    /// El descriptor `engine-paths.json` (`orgtree.engine-paths/v1`), escrito
    /// de forma atómica como `writeEnginePaths`.
    pub fn write_engine_paths(&self, file: &Path, data_root: &Path) -> Result<(), EngineError> {
        if !data_root.is_absolute() {
            return Err(EngineError::Config("la raíz de datos debe ser absoluta".into()));
        }
        let descriptor = serde_json::json!({
            "schema": "orgtree.engine-paths/v1",
            "engine": self.engine_dir,
            "python": self.python,
            "data": data_root,
            "custodian": self.custodian,
            "pgBin": self.pg_bin,
            "importer": self.importer,
        });
        let text = serde_json::to_string_pretty(&descriptor).map_err(|e| EngineError::Config(e.to_string()))? + "\n";
        let write = || -> std::io::Result<()> {
            if let Some(parent) = file.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let mut tmp = file.as_os_str().to_owned();
            tmp.push(".tmp");
            std::fs::write(&tmp, text)?;
            std::fs::rename(&tmp, file)
        };
        write().map_err(|e| EngineError::Config(format!("no se pudo escribir {}: {e}", file.display())))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("orgtree-packaged-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn touch(path: PathBuf) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, b"").unwrap();
    }

    fn full_package(root: &Path) {
        touch(root.join("engine/runtime/python.exe"));
        touch(root.join("engine/launch.py"));
        touch(root.join("engine/pg-custodian.exe"));
        for tool in PG_TOOLS {
            touch(root.join("engine/postgresql/bin").join(tool));
        }
        touch(root.join("tools/pypg/pgimport.py"));
        touch(root.join("ui/index.html"));
    }

    #[cfg(windows)]
    #[test]
    fn sin_prefijo_de_ruta_larga() {
        let root = scratch("prefijo");
        full_package(&root);
        let long = PathBuf::from(format!(r"\\?\{}", root.display()));
        let runtime = PackagedRuntime::locate(&long).unwrap().unwrap();
        assert!(!runtime.python.to_string_lossy().starts_with(r"\\?\"), "{}", runtime.python.display());
    }

    #[test]
    fn sin_runtime_no_es_un_paquete() {
        let root = scratch("vacio");
        touch(root.join("engine/launch.py"));
        assert!(PackagedRuntime::locate(&root).is_none());
    }

    #[test]
    fn paquete_incompleto_es_un_error() {
        let root = scratch("incompleto");
        full_package(&root);
        std::fs::remove_file(root.join("engine/postgresql/bin/initdb.exe")).unwrap();
        match PackagedRuntime::locate(&root) {
            Some(Err(EngineError::Config(message))) => assert!(message.contains("initdb.exe"), "{message}"),
            other => panic!("se esperaba un error de configuración: {other:?}"),
        }
    }

    #[test]
    fn paquete_completo_da_las_opciones_de_electron() {
        let root = scratch("completo");
        full_package(&root);
        let runtime = PackagedRuntime::locate(&root).unwrap().unwrap();
        let data = root.join("datos");
        let options = runtime.options(&data);
        assert_eq!(options.python, root.join("engine/runtime/python.exe"));
        assert_eq!(options.engine_dir, root.join("engine"));
        assert_eq!(options.ui_dir, Some(root.join("ui")));
        assert!(options.bootstrap_postgres);
        let env: std::collections::HashMap<_, _> = options.env.iter().cloned().collect();
        // Path compara por componentes: en Windows `/` y `\` son el mismo separador.
        assert_eq!(Path::new(&env["ORGTREE_PG_CUSTODIAN"]), root.join("engine/pg-custodian.exe"));
        assert_eq!(Path::new(&env["ORGTREE_P03_PG_BIN"]), root.join("engine/postgresql/bin"));
    }

    #[test]
    fn descriptor_engine_paths() {
        let root = scratch("descriptor");
        full_package(&root);
        let runtime = PackagedRuntime::locate(&root).unwrap().unwrap();
        let file = root.join("perfil/engine-paths.json");
        runtime.write_engine_paths(&file, &root.join("datos")).unwrap();
        let value: serde_json::Value = serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
        assert_eq!(value["schema"], "orgtree.engine-paths/v1");
        let path = |key: &str| PathBuf::from(value[key].as_str().unwrap());
        assert_eq!(path("data"), root.join("datos"));
        assert_eq!(path("pgBin"), root.join("engine/postgresql/bin"));
        assert_eq!(path("importer"), root.join("tools/pypg/pgimport.py"));
        assert_eq!(path("python"), root.join("engine/runtime/python.exe"));
        assert!(runtime.write_engine_paths(&file, Path::new("relativa")).is_err());
    }
}
