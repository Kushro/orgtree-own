"""Arma los recursos del instalador Dioxus (#24) en ``target/bundle-resources``.

Copia lo mismo que ``build.extraResources`` de ``package.json`` (el instalador
de Electron), menos el renderer React, que la UI en RSX no usa:

- ``engine/`` sin ``__pycache__``, ``*.pyc`` ni ``native/**/target``. Además
  se dejan afuera los archivos de desarrollo: las fuentes de ``engine/native``
  (solo queda ``prototype-guard/live-locations.json``, que ``pg_process.py``
  lee en tiempo de ejecución), ``engine/docs`` y los ``.git`` del submódulo;
- ``tools/pypg/pgimport.py`` y ``cutover_verify.py`` (la conversión de la
  primera ejecución busca el importador junto a ``engine/``);
- ``build-info.json`` con el commit, para que el motor sepa qué artefacto corre.

Antes verifica que el runtime (``tools/provision-runtime.py``) y PostgreSQL
(``tools/provision-postgres.py``) estén aprovisionados, con la misma lista de
archivos que ``postgres-runtime.ts``. No toca ``engine/`` del checkout.
"""

from __future__ import annotations

import argparse
import json
import shutil
import subprocess
import sys
from pathlib import Path

APP = Path(__file__).resolve().parents[1]
ROOT = APP.parents[1]
OUT = APP / "target" / "bundle-resources"
PG_TOOLS = ("postgres.exe", "pg_ctl.exe", "initdb.exe", "psql.exe", "pg_controldata.exe")
# Lo único de engine/native que el motor lee al correr.
NATIVE_KEEP = {("native", "prototype-guard", "live-locations.json")}
VERSION = "0.0.1-dioxus"


def skip_engine(relative: Path) -> bool:
    parts = relative.parts
    if "__pycache__" in parts or relative.suffix == ".pyc" or ".git" in parts:
        return True
    if parts[:1] == ("docs",) or relative == Path("README.md"):
        return True
    if parts[:1] == ("native",):
        return tuple(parts) not in NATIVE_KEEP
    return False


def require(files: list[Path], what: str) -> None:
    missing = [str(f) for f in files if not f.is_file()]
    if missing:
        raise SystemExit(f"Falta {what}: {', '.join(missing)}")


def copy_tree(source: Path, target: Path) -> tuple[int, int]:
    count = size = 0
    for path in sorted(source.rglob("*")):
        relative = path.relative_to(source)
        if path.is_symlink():
            raise SystemExit(f"Enlace en el motor, no se empaqueta: {path}")
        if not path.is_file() or skip_engine(relative):
            continue
        destination = target / relative
        destination.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(path, destination)
        count += 1
        size += path.stat().st_size
    return count, size


def git(*args: str) -> str:
    return subprocess.run(["git", *args], cwd=ROOT, check=True, capture_output=True, text=True).stdout.strip()


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--allow-missing-runtime", action="store_true",
                        help="Solo para probar fuera de Windows: no exige runtime ni PostgreSQL")
    args = parser.parse_args()
    engine = ROOT / "engine"
    if not args.allow_missing_runtime:
        require([engine / "runtime" / "python.exe", engine / "runtime" / "python313._pth"], "el runtime embebido (python tools/provision-runtime.py)")
        require([engine / "pg-custodian.exe", *(engine / "postgresql" / "bin" / t for t in PG_TOOLS)],
                "PostgreSQL empaquetado (python tools/provision-postgres.py)")
        require([engine / "mailhub" / "__init__.py"], "el submódulo engine/mailhub")
    if OUT.exists():
        shutil.rmtree(OUT)
    OUT.mkdir(parents=True)
    files, size = copy_tree(engine, OUT / "engine")
    pypg = OUT / "tools" / "pypg"
    pypg.mkdir(parents=True)
    for name in ("pgimport.py", "cutover_verify.py"):
        shutil.copy2(ROOT / "tools" / "pypg" / name, pypg / name)
    commit = git("rev-parse", "HEAD")
    dirty = bool(git("status", "--porcelain", "--untracked-files=no"))
    info = {"version": VERSION, "channel": "dev", "commit": commit, "dirty": dirty,
            "mailhubCommit": git("-C", "engine/mailhub", "rev-parse", "HEAD") if (engine / "mailhub" / ".git").exists() else None,
            "app": "orgtree-dioxus"}
    (OUT / "build-info.json").write_text(json.dumps(info, indent=2) + "\n", encoding="utf-8")
    print(json.dumps({"out": str(OUT), "engineFiles": files, "engineBytes": size, "commit": commit}, indent=2))


if __name__ == "__main__":
    sys.exit(main())
