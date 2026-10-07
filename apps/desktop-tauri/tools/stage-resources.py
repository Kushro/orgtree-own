"""Arma los recursos del instalador Tauri en apps/desktop-tauri/bundle-resources.

Copia exactamente lo que empaqueta Electron (`build.extraResources` en
package.json), porque `bundle.resources` de Tauri no admite exclusiones:

- engine/            sin __pycache__, sin .git (el gitlink del submódulo
                     mailhub) y sin native/**/target; incluye engine/runtime
                     (tools/provision-runtime.py) y PostgreSQL con
                     pg-custodian.exe (tools/provision-postgres.py)
- tools/pypg/        solo pgimport.py y cutover_verify.py
- ui/                el renderer construido (dist/renderer)
- build-info.json    identidad del paquete (build_identity.py), canal "dev"

Uso: python apps/desktop-tauri/tools/stage-resources.py [--allow-missing-runtime]
"""
from __future__ import annotations

import argparse
import datetime
import json
from pathlib import Path
import shutil
import subprocess

ROOT = Path(__file__).resolve().parents[3]
OUT = ROOT / "apps" / "desktop-tauri" / "bundle-resources"
PG_TOOLS = ("postgres.exe", "pg_ctl.exe", "initdb.exe", "psql.exe", "pg_controldata.exe")


def ignore_engine(directory: str, names: list[str]) -> set[str]:
    here = Path(directory).resolve()
    skipped = {name for name in names if name in ("__pycache__", ".git", ".pytest_cache") or name.endswith(".pyc")}
    # native/<crate>/target (y cualquier target más abajo en native/), como `!native/**/target/**`.
    native = (ROOT / "engine" / "native").resolve()
    if "target" in names and (here == native or native in here.parents):
        skipped.add("target")
    return skipped


def no_links(path: Path) -> None:
    for entry in [path, *path.rglob("*")]:
        if entry.is_symlink():
            raise SystemExit(f"enlace en los recursos: {entry}")


def git(*args: str) -> str:
    return subprocess.run(["git", *args], cwd=ROOT, check=True, capture_output=True, text=True).stdout.strip()


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--allow-missing-runtime", action="store_true",
                        help="para probar el armado fuera de Windows: no exige runtime ni PostgreSQL")
    args = parser.parse_args()
    required = [ROOT / "engine/launch.py", ROOT / "dist/renderer/index.html",
                ROOT / "tools/pypg/pgimport.py", ROOT / "tools/pypg/cutover_verify.py",
                ROOT / "engine/mailhub/mailhub"]
    if not args.allow_missing_runtime:
        required += [ROOT / "engine/runtime/python.exe", ROOT / "engine/pg-custodian.exe",
                     ROOT / "engine/postgres-runtime-manifest.json",
                     *(ROOT / "engine/postgresql/bin" / tool for tool in PG_TOOLS)]
    missing = [str(path) for path in required if not path.exists()]
    if missing:
        raise SystemExit("faltan entradas del paquete:\n  " + "\n  ".join(missing))

    if OUT.exists():
        shutil.rmtree(OUT)
    OUT.mkdir(parents=True)
    no_links(ROOT / "engine")
    shutil.copytree(ROOT / "engine", OUT / "engine", ignore=ignore_engine)
    (OUT / "tools" / "pypg").mkdir(parents=True)
    for name in ("pgimport.py", "cutover_verify.py"):
        shutil.copy2(ROOT / "tools" / "pypg" / name, OUT / "tools" / "pypg" / name)
    shutil.copytree(ROOT / "dist" / "renderer", OUT / "ui")

    commit = git("rev-parse", "HEAD")
    info = {
        "version": "0.0.1-tauri." + commit[:7],
        "channel": "dev",
        "commit": commit,
        "dirty": bool(git("status", "--porcelain", "--untracked-files=no")),
        "mailhubCommit": git("-C", "engine/mailhub", "rev-parse", "HEAD"),
        "builtAt": datetime.datetime.now(datetime.timezone.utc).isoformat(timespec="seconds"),
        "shell": "tauri",
    }
    (OUT / "build-info.json").write_text(json.dumps(info, indent=2) + "\n", encoding="utf-8")

    leftovers = [str(p) for p in OUT.rglob("*") if p.name in ("__pycache__", ".git") or p.suffix == ".pyc"
                 or (p.name == "target" and "native" in p.parts)]
    if leftovers:
        raise SystemExit("quedaron archivos de desarrollo:\n  " + "\n  ".join(leftovers[:20]))
    files = [p for p in OUT.rglob("*") if p.is_file()]
    total = sum(p.stat().st_size for p in files)
    summary = {"out": str(OUT), "files": len(files), "megabytes": round(total / 2**20, 1)}
    for part in ("engine/runtime", "engine/postgresql", "engine/backend", "engine/mailhub", "ui"):
        sub = [p for p in (OUT / part).rglob("*") if p.is_file()] if (OUT / part).exists() else []
        summary[part] = {"files": len(sub), "megabytes": round(sum(p.stat().st_size for p in sub) / 2**20, 1)}
    print(json.dumps(summary, indent=2))


if __name__ == "__main__":
    main()
