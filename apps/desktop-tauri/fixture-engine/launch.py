"""Motor de fixture del spike: el motor real con datos sembrados.

Se lanza igual que ``engine/launch.py`` (``ORGTREE_DATA`` descartable y
``ORGTREE_V2_TOKEN``), pero antes de servir siembra una org con un agente que
tiene historial (texto y una herramienta) y apaga los proveedores, que en el CI
no existen. Sigue la receta de ``tests/test_engine_http.py``.

- En Windows corre ``launch.main()``: el arranque real, con guardián y hub.
- En otros sistemas el motor no maneja procesos (no hay guardián), así que
  sirve la misma app con uvicorn y la misma línea ``ready``. Es solo para
  desarrollar el shell en Linux.

``ORGTREE_FIXTURE_LIVE=1`` emite frames ``node_stream`` del agente cada 1,5 s
para probar el desk en vivo sin un proveedor real, y
``ORGTREE_FIXTURE_MESSAGES=N`` agrega N mensajes al historial para medir el
scroll de una conversación larga.

``PUT /api/fixture/notices`` (solo en este fixture) reemplaza una lista de
avisos sintéticos que ``/api/desktop/notifications`` suma a los reales: la
prueba de integraciones (#21) los usa para que el renderer real pida
notificaciones nativas y el parpadeo de la barra de tareas, como haría con una
pregunta de un agente.
"""

from __future__ import annotations

import json
import os
import sys
import threading
import time
import uuid
from pathlib import Path

CHECKOUT = Path(os.environ.get("ORGTREE_FIXTURE_CHECKOUT") or Path(__file__).resolve().parents[3]).resolve()

# El perfil (~/.claude, donde la reconciliación busca transcripts) vive junto a
# la raíz descartable, nunca adentro (el motor rechaza ~/orgtree dentro de la
# raíz): el fixture nunca toca el perfil real de nadie.
_DATA = Path(os.environ["ORGTREE_DATA"]).resolve()
_HOME = _DATA.parent / f"{_DATA.name}-fixture-home"
_HOME.mkdir(parents=True, exist_ok=True)
os.environ["HOME"] = os.environ["USERPROFILE"] = str(_HOME)
sys.path[:0] = [str(CHECKOUT / "engine" / "backend"), str(CHECKOUT / "engine"), str(CHECKOUT)]

import launch  # noqa: E402  (engine/launch.py del checkout)

ORG = "spike-fixture"
AGENT = "worker"


def _records(sid: str) -> list[dict]:
    def rec(kind: str, parent: str | None, at: str, message: dict) -> dict:
        return {"type": kind, "uuid": str(uuid.uuid4()), "parentUuid": parent, "sessionId": sid,
                "timestamp": at, "message": message}
    ask = rec("user", None, "2026-10-07T10:00:00Z",
              {"role": "user", "content": "Revisá el README y resumilo en una línea."})
    tool = rec("assistant", ask["uuid"], "2026-10-07T10:00:03Z",
               {"id": "msg_fixture_1", "role": "assistant", "content": [
                   {"type": "text", "text": "Voy a leer el README."},
                   {"type": "tool_use", "id": "toolu_fixture_1", "name": "Read",
                    "input": {"file_path": "README.md"}}]})
    result = rec("user", tool["uuid"], "2026-10-07T10:00:04Z",
                 {"role": "user", "content": [
                     {"type": "tool_result", "tool_use_id": "toolu_fixture_1",
                      "content": "# Orgtree\nUna app de escritorio para equipos de agentes."}]})
    answer = rec("assistant", result["uuid"], "2026-10-07T10:00:06Z",
                 {"id": "msg_fixture_2", "role": "assistant", "content": [
                     {"type": "text", "text": "Orgtree organiza agentes de código en un organigrama."}]})
    records = [ask, tool, result, answer]
    # Conversación larga para medir el scroll del desk (#5): N pares extra.
    extra = int(os.environ.get("ORGTREE_FIXTURE_MESSAGES", "0") or 0)
    parent = answer["uuid"]
    for i in range(extra // 2):
        q = rec("user", parent, f"2026-10-07T11:{(i // 60) % 60:02d}:{i % 60:02d}Z",
                {"role": "user", "content": f"Mensaje {2 * i + 1}: ¿cómo va la tarea {i}?"})
        a = rec("assistant", q["uuid"], f"2026-10-07T11:{(i // 60) % 60:02d}:{i % 60:02d}Z",
                {"id": f"msg_fixture_long_{i}", "role": "assistant", "content": [
                    {"type": "text", "text": f"Respuesta {2 * i + 2}: la tarea {i} avanza. " + "Detalle. " * 12}]})
        records += [q, a]
        parent = a["uuid"]
    return records


def _stub_providers() -> None:
    from orgtree import providers, supervisor, warmpool
    providers.antigravity_status = lambda **kw: {"available": False, "installed": False}
    providers.codex_status = lambda **kw: {"available": False, "installed": False}
    supervisor.start_usage_warm_loop = lambda: None
    supervisor.start_cred_watcher = lambda: None
    warmpool.start_warm_pool = lambda: None
    supervisor.send_message = lambda *a, **k: None


def _seed() -> None:
    from orgtree import store
    from orgtree.ledger import USER
    try:
        store.load_org(ORG)
        return  # raíz reutilizada: ya sembrada
    except Exception:
        pass
    org = store.create_org(ORG)
    org.hire(USER, None, "haiku", 0, AGENT)
    node = org.node(AGENT)
    sid = str(uuid.uuid4())
    node["session_id"] = sid
    node.pop("session_unrun", None)
    node["cost_usd"] = 1
    path = _HOME / ".claude" / "projects" / ORG / f"{sid}.jsonl"
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text("".join(json.dumps(r) + "\n" for r in _records(sid)), encoding="utf-8")
    store.save_org(org)


def _live_frames() -> None:
    from orgtree import supervisor
    time.sleep(8)  # supervisor.stream se conecta al hub del WebSocket al arrancar
    n = 0
    while True:
        n += 1
        supervisor.stream(ORG, AGENT, {"kind": "delta", "text": f"latido {n} "})
        time.sleep(1.5)


_FIXTURE_NOTICES: list[dict] = []
_NOTICE_KINDS = {"question", "terminal-failure", "urgent-mail", "work-attention", "routine", "document", "agent-frozen"}


def _install_fixture_notices() -> None:
    from fastapi import HTTPException
    from orgtree import api, desktop_notifications
    original = desktop_notifications._all_rows
    desktop_notifications._all_rows = lambda: original() + [dict(row) for row in _FIXTURE_NOTICES]

    def put_notices(body: dict) -> dict:
        rows = body.get("notices")
        if not isinstance(rows, list) or len(rows) > 50:
            raise HTTPException(422, "notices must be a short list")
        clean = []
        for row in rows:
            if (not isinstance(row, dict) or row.get("org") != ORG or row.get("kind") not in _NOTICE_KINDS
                    or not all(isinstance(row.get(k), str) and row.get(k) for k in ("id", "title", "body"))):
                raise HTTPException(422, "invalid notice")
            clean.append({k: row[k] for k in ("id", "org", "kind", "title", "body", "agent", "source_id") if k in row})
        _FIXTURE_NOTICES[:] = clean
        return {"count": len(clean)}

    api.app.add_api_route("/api/fixture/notices", put_notices, methods=["PUT"])
    # Antes que cualquier ruta comodín de la app.
    api.app.router.routes.insert(0, api.app.router.routes.pop())


original_load_app = launch.load_app


def seeded_load_app():
    result = original_load_app()
    _stub_providers()
    _install_fixture_notices()
    _seed()
    if os.environ.get("ORGTREE_FIXTURE_LIVE") == "1":
        threading.Thread(target=_live_frames, name="fixture-live", daemon=True).start()
    return result


launch.load_app = seeded_load_app


def serve_without_lifetime() -> None:
    """Solo fuera de Windows: la app real, servida con uvicorn, sin guardián."""
    import asyncio
    import uvicorn
    app, _token, data, port, stopping = launch.load_app()
    from orgtree.api import LOCAL_UVICORN_OPTIONS
    server = uvicorn.Server(uvicorn.Config(app, host="127.0.0.1", port=port, access_log=False,
                                           **LOCAL_UVICORN_OPTIONS))

    async def main() -> None:
        task = asyncio.create_task(server.serve())
        while not server.started and not task.done():
            await asyncio.sleep(0.01)
        if task.done():
            await task
            raise RuntimeError("engine exited before readiness")
        print(json.dumps({"type": "ready", "protocol": 1, "port": port, "pid": os.getpid(),
                          "dataRootId": launch.data_root_id(data)}, separators=(",", ":")), flush=True)
        while not task.done():
            if stopping["value"]:
                server.should_exit = True
            await asyncio.sleep(0.1)

    asyncio.run(main())


if __name__ == "__main__":
    if os.name == "nt":
        launch.main()
    else:
        serve_without_lifetime()
